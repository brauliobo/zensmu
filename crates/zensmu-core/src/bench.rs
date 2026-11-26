//! Zero-dependency Per-Core Curve Optimizer Stability Tester / Core-Cycler.
//!
//! Pins a deterministic, self-verifying stress worker thread to each selected
//! physical core (via `libc::sched_setaffinity`) while sampling live telemetry
//! (`pm_table::read_telemetry`, Linux `cpufreq`, optional `hwmon` Vcore) and
//! checking Linux MCE / EDAC / kernel log deltas before and after each core
//! run to catch silent computational corruption and hardware Machine Check
//! Exceptions caused by unstable Curve Optimizer undervolts.

use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
use crate::ops::pm_table;
use crate::transport::SmuTransport;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Minimum allowed stress duration per core (seconds).
pub const MIN_DURATION_PER_CORE_SECS: u64 = 1;
/// Maximum allowed stress duration per core (seconds).
pub const MAX_DURATION_PER_CORE_SECS: u64 = 3600;
/// Minimum allowed number of full core-cycling passes.
pub const MIN_PASSES: u32 = 1;
/// Maximum allowed number of full core-cycling passes.
pub const MAX_PASSES: u32 = 100;
/// Minimum allowed telemetry/progress sampling interval (milliseconds).
pub const MIN_SAMPLE_INTERVAL_MS: u64 = 100;

/// 64-byte cache line = 8 x `u64`. 4096 cache lines = 256 KiB (exceeds Zen L1D
/// while heavily exercising L1D eviction/refill and L2 cache lines).
const CACHE_LINES: usize = 4096;
const WORDS_PER_LINE: usize = 8;
const CACHE_BUFFER_WORDS: usize = CACHE_LINES * WORDS_PER_LINE;

/// Responsive sleep slice for coordinator polling (milliseconds).
const POLL_SLICE_MS: u64 = 25;

/// Deterministic CPU stress workload profile for the core-cycler.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BenchWorkload {
    /// Interleaved ALU/integer, FP64 FMA, and L1/L2 cache stress (default).
    #[default]
    Mixed,
    /// Multi-lane 64-bit integer ALU/IMUL/bit-mix Feistel stress.
    Integer,
    /// Multi-lane IEEE-754 FP64 FMA multiply-add stress.
    FloatingPoint,
    /// Prime-strided 64-byte cache-line L1/L2 read-modify-write stress.
    Cache,
}

impl BenchWorkload {
    /// Canonical lowercase kebab-case identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Mixed => "mixed",
            Self::Integer => "integer",
            Self::FloatingPoint => "floating-point",
            Self::Cache => "cache",
        }
    }

    /// Parse a workload name case-insensitively (`mixed`, `integer`/`int`,
    /// `floating-point`/`fp`/`float`, `cache`).
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "mixed" => Some(Self::Mixed),
            "integer" | "int" => Some(Self::Integer),
            "floating-point" | "floating_point" | "fp" | "float" => Some(Self::FloatingPoint),
            "cache" => Some(Self::Cache),
            _ => None,
        }
    }
}

impl fmt::Display for BenchWorkload {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Configuration for a per-core Curve Optimizer stability test run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BenchConfig {
    /// Physical core indices to test. Empty means all physical cores from `hw.topology`.
    pub cores: Vec<u32>,
    /// Duration to stress each core per pass, in seconds (`1..=3600`, default `30`).
    pub duration_per_core_secs: u64,
    /// Number of full passes across the selected cores (`1..=100`, default `1`).
    pub passes: u32,
    /// Deterministic stress kernel workload (default [`BenchWorkload::Mixed`]).
    pub workload: BenchWorkload,
    /// Telemetry and progress callback interval in milliseconds (`>= 100`, default `500`).
    pub sample_interval_ms: u64,
    /// Stop remaining cores immediately when any core fails MCE or compute self-check.
    pub stop_on_error: bool,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            cores: Vec::new(),
            duration_per_core_secs: 30,
            passes: 1,
            workload: BenchWorkload::Mixed,
            sample_interval_ms: 500,
            stop_on_error: true,
        }
    }
}

/// Snapshot of Linux hardware Machine Check / EDAC counters and kernel error lines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MceCounters {
    /// Sum of corrected error counts from `machinecheck` threshold blocks and EDAC `ce_count`.
    pub corrected_errors: u64,
    /// Sum of uncorrected error counts from EDAC `ue_count`.
    pub uncorrected_errors: u64,
    /// Matching MCE / WHEA / Hardware Error lines from `dmesg` or `journalctl -k`.
    pub dmesg_mce_lines: Vec<String>,
    /// Whether `dmesg` or `journalctl -k` was readable in this execution context.
    pub dmesg_readable: bool,
}

/// Outcome of a single physical core test run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CoreBenchStatus {
    /// Core completed its target duration with zero compute mismatches and zero new MCEs.
    Passed,
    /// One or more hardware MCE / EDAC / kernel log errors were detected during the run.
    FailedMce,
    /// Deterministic workload self-check detected silent computational corruption.
    FailedCompute,
    /// Cancelled cleanly via `stop_flag` before completion.
    Cancelled,
}

impl CoreBenchStatus {
    /// Canonical lowercase kebab-case status label.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Passed => "passed",
            Self::FailedMce => "failed-mce",
            Self::FailedCompute => "failed-compute",
            Self::Cancelled => "cancelled",
        }
    }
}

impl fmt::Display for CoreBenchStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Per-core benchmark result for one pass.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoreBenchResult {
    pub pass: u32,
    pub core_index: u32,
    pub logical_cpu: u32,
    pub status: CoreBenchStatus,
    pub duration_secs: f64,
    pub iterations: u64,
    pub peak_freq_mhz: Option<f32>,
    pub peak_temp_c: Option<f32>,
    pub peak_power_w: Option<f32>,
    pub min_voltage_v: Option<f32>,
    pub max_voltage_v: Option<f32>,
    pub mce_delta_corrected: u64,
    pub mce_delta_uncorrected: u64,
    pub new_mce_lines: Vec<String>,
    pub error: Option<String>,
}

/// Live progress update emitted to `on_progress` during core-cycling.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchProgress {
    pub pass: u32,
    pub total_passes: u32,
    pub core_index: u32,
    pub core_pos: usize,
    pub total_cores: usize,
    pub elapsed_secs: f64,
    pub target_secs: u64,
    pub latest_freq_mhz: Option<f32>,
    pub latest_temp_c: Option<f32>,
}

/// Aggregated report returned by [`run_core_cycler`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchReport {
    pub codename: String,
    pub workload: BenchWorkload,
    pub passes_completed: u32,
    pub total_duration_secs: f64,
    pub passed: bool,
    pub cancelled: bool,
    pub failed_cores: Vec<u32>,
    pub dmesg_readable: bool,
    pub results: Vec<CoreBenchResult>,
}

/// Validate `config` against `hw.topology` and return ordered
/// `(physical_core_index, logical_cpu_id)` pairs to execute.
pub fn validate_bench_config(
    config: &BenchConfig,
    hw: &HardwareContext,
) -> Result<Vec<(u32, u32)>> {
    if !(MIN_DURATION_PER_CORE_SECS..=MAX_DURATION_PER_CORE_SECS)
        .contains(&config.duration_per_core_secs)
    {
        return Err(Error::OutOfBounds {
            field: "duration_per_core_secs".into(),
            value: config.duration_per_core_secs.to_string(),
            bounds: format!("{MIN_DURATION_PER_CORE_SECS}..={MAX_DURATION_PER_CORE_SECS} s"),
        });
    }

    if !(MIN_PASSES..=MAX_PASSES).contains(&config.passes) {
        return Err(Error::OutOfBounds {
            field: "passes".into(),
            value: config.passes.to_string(),
            bounds: format!("{MIN_PASSES}..={MAX_PASSES}"),
        });
    }

    if config.sample_interval_ms < MIN_SAMPLE_INTERVAL_MS {
        return Err(Error::OutOfBounds {
            field: "sample_interval_ms".into(),
            value: config.sample_interval_ms.to_string(),
            bounds: format!(">={MIN_SAMPLE_INTERVAL_MS} ms"),
        });
    }

    if hw.topology.cores.is_empty() {
        return Err(Error::InvalidInput(
            "no physical cores detected in CPU topology".into(),
        ));
    }

    let mut targets = Vec::new();
    if config.cores.is_empty() {
        for core in &hw.topology.cores {
            let logical_cpu = core.linux_cpus.first().copied().ok_or_else(|| {
                Error::InvalidInput(format!(
                    "physical core {} has no mapped Linux logical CPU",
                    core.index
                ))
            })?;
            targets.push((core.index, logical_cpu));
        }
    } else {
        let mut seen = HashSet::with_capacity(config.cores.len());
        for &core_idx in &config.cores {
            if !seen.insert(core_idx) {
                continue;
            }
            let core = hw.topology.core(core_idx).ok_or_else(|| {
                Error::InvalidInput(format!(
                    "core {core_idx} not found in CPU topology (detected {} physical cores)",
                    hw.topology.physical_cores
                ))
            })?;
            let logical_cpu = core.linux_cpus.first().copied().ok_or_else(|| {
                Error::InvalidInput(format!(
                    "physical core {core_idx} has no mapped Linux logical CPU"
                ))
            })?;
            targets.push((core.index, logical_cpu));
        }
    }

    if targets.is_empty() {
        return Err(Error::InvalidInput(
            "no target cores resolved for benchmark".into(),
        ));
    }

    Ok(targets)
}

/// Extract MCE / WHEA / Hardware Error lines from kernel log text.
pub fn extract_mce_lines(log: &str) -> Vec<String> {
    log.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && is_mce_log_line(line))
        .map(str::to_string)
        .collect()
}

fn is_mce_log_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("mce:")
        || lower.contains("hardware error")
        || lower.contains("whea")
        || lower.contains("machine check")
}

/// Compute newly appended MCE log lines between a `before` and `after` snapshot.
///
/// Uses longest suffix-prefix overlap so both normal appends, repeated identical
/// MCE lines, and sliding kernel ring-buffer windows (e.g. `journalctl -n 200`)
/// are handled accurately.
pub fn diff_mce_lines(before: &[String], after: &[String]) -> Vec<String> {
    if after.is_empty() {
        return Vec::new();
    }
    if before.is_empty() {
        return after.to_vec();
    }
    let max_overlap = before.len().min(after.len());
    for overlap in (1..=max_overlap).rev() {
        let before_start = before.len() - overlap;
        if let (Some(b_tail), Some(a_head)) = (before.get(before_start..), after.get(..overlap)) {
            if b_tail == a_head {
                return after
                    .get(overlap..)
                    .map(<[String]>::to_vec)
                    .unwrap_or_default();
            }
        }
    }
    after.to_vec()
}

/// Snapshot Linux MCE / EDAC sysfs counters and kernel log MCE lines (best-effort).
pub fn read_mce_counters() -> MceCounters {
    let (corrected_errors, uncorrected_errors) = read_mce_sysfs_counters(
        Path::new("/sys/devices/system/machinecheck"),
        Path::new("/sys/devices/system/edac/mc"),
    );
    let (dmesg_readable, dmesg_mce_lines) = read_kernel_mce_log();

    MceCounters {
        corrected_errors,
        uncorrected_errors,
        dmesg_mce_lines,
        dmesg_readable,
    }
}

fn read_mce_sysfs_counters(machinecheck_root: &Path, edac_mc_root: &Path) -> (u64, u64) {
    let mut corrected = 0u64;
    let mut uncorrected = 0u64;

    if let Ok(entries) = fs::read_dir(machinecheck_root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with("machinecheck") {
                continue;
            }
            if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                collect_machinecheck_error_counts(&entry.path(), 0, &mut corrected);
            }
        }
    }

    if let Ok(entries) = fs::read_dir(edac_mc_root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            let Some(rest) = s.strip_prefix("mc") else {
                continue;
            };
            if rest.parse::<u32>().is_err() {
                continue;
            }
            let path = entry.path();
            if let Some(ce) = read_u64_file(&path.join("ce_count")) {
                corrected = corrected.saturating_add(ce);
            }
            if let Some(ue) = read_u64_file(&path.join("ue_count")) {
                uncorrected = uncorrected.saturating_add(ue);
            }
        }
    }

    (corrected, uncorrected)
}

/// Recursively scan real subdirectories (never following symlinks) under a
/// `machinecheck<N>` directory up to depth 3 for AMD threshold `error_count` files.
fn collect_machinecheck_error_counts(dir: &Path, depth: usize, total: &mut u64) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if ft.is_dir() {
            if name_str == "power" {
                continue;
            }
            collect_machinecheck_error_counts(&entry.path(), depth + 1, total);
        } else if ft.is_file() && (name_str == "error_count" || name_str == "ce_count") {
            if let Some(val) = read_u64_file(&entry.path()) {
                *total = total.saturating_add(val);
            }
        }
    }
}

fn read_u64_file(path: &Path) -> Option<u64> {
    let raw = fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        trimmed.parse::<u64>().ok()
    }
}

fn read_kernel_mce_log() -> (bool, Vec<String>) {
    if let Ok(output) = Command::new("dmesg")
        .args(["-T", "--level=err,crit,alert,emerg"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            return (true, extract_mce_lines(&text));
        }
    }

    if let Ok(output) = Command::new("journalctl")
        .args(["-k", "-b", "-p", "err", "--no-pager", "-n", "200"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            return (true, extract_mce_lines(&text));
        }
    }

    (false, Vec::new())
}

/// Pin the calling OS thread to a single Linux logical CPU ID via `sched_setaffinity`.
#[cfg(target_os = "linux")]
fn pin_current_thread_to_cpu(logical_cpu: u32) -> Result<()> {
    let cpu_idx = usize::try_from(logical_cpu).map_err(|_| Error::Io {
        path: format!("sched_setaffinity(cpu={logical_cpu})"),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("logical CPU ID {logical_cpu} out of range"),
        ),
    })?;
    let max_cpus = usize::try_from(libc::CPU_SETSIZE).unwrap_or(1024);
    if cpu_idx >= max_cpus {
        return Err(Error::Io {
            path: format!("sched_setaffinity(cpu={logical_cpu})"),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("logical CPU ID {logical_cpu} exceeds CPU_SETSIZE ({max_cpus})"),
            ),
        });
    }

    // SAFETY: `cpu_set_t` is a POD bitset zero-initialized before setting a single
    // bounds-checked CPU index (`cpu_idx < CPU_SETSIZE`), then passed to
    // `sched_setaffinity(0, ...)` to pin the calling thread.
    let rc = unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu_idx, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set)
    };
    if rc != 0 {
        let source = std::io::Error::last_os_error();
        return Err(Error::Io {
            path: format!("sched_setaffinity(cpu={logical_cpu})"),
            source,
        });
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn pin_current_thread_to_cpu(logical_cpu: u32) -> Result<()> {
    Err(Error::Io {
        path: format!("sched_setaffinity(cpu={logical_cpu})"),
        source: std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "CPU affinity pinning is only supported on Linux",
        ),
    })
}

/// Read live per-logical-CPU clock frequency in MHz from Linux `cpufreq` sysfs.
fn read_cpu_freq_mhz(logical_cpu: u32) -> Option<f32> {
    let base = PathBuf::from(format!("/sys/devices/system/cpu/cpu{logical_cpu}/cpufreq"));
    for file in ["scaling_cur_freq", "cpuinfo_cur_freq"] {
        if let Ok(raw) = fs::read_to_string(base.join(file)) {
            if let Ok(khz) = raw.trim().parse::<u64>() {
                let mhz = (khz as f64 / 1000.0) as f32;
                if mhz.is_finite() && (100.0..=10_000.0).contains(&mhz) {
                    return Some(mhz);
                }
            }
        }
    }
    None
}

/// Locate an AMD SVI2 / Vcore `hwmon` input path once per run (best-effort).
fn find_hwmon_vcore_path() -> Option<PathBuf> {
    let hwmon_root = Path::new("/sys/class/hwmon");
    let entries = fs::read_dir(hwmon_root).ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(chip) = fs::read_to_string(dir.join("name"))
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
        else {
            continue;
        };
        let is_amd_cpu_sensor = matches!(
            chip.as_str(),
            "k10temp" | "zenpower" | "zenpower3" | "zenpower5"
        );
        if !is_amd_cpu_sensor {
            continue;
        }
        let Ok(files) = fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let fname = f.file_name().to_string_lossy().into_owned();
            let Some(prefix) = fname.strip_suffix("_label") else {
                continue;
            };
            if !prefix.starts_with("in") {
                continue;
            }
            let Ok(label) = fs::read_to_string(f.path()) else {
                continue;
            };
            let norm = label.trim().to_ascii_lowercase();
            if norm.contains("svi2_core") || norm == "vcore" || norm.contains("cpu core") {
                let input_path = dir.join(format!("{prefix}_input"));
                if input_path.is_file() {
                    return Some(input_path);
                }
            }
        }
    }
    None
}

/// Read an `in*_input` hwmon millivolt sensor and convert to Volts.
fn read_hwmon_voltage_v(path: &Path) -> Option<f32> {
    let raw = fs::read_to_string(path).ok()?;
    let mv = raw.trim().parse::<i64>().ok()?;
    let volts = (mv as f64 / 1000.0) as f32;
    if volts.is_finite() && (0.2..=2.2).contains(&volts) {
        Some(volts)
    } else {
        None
    }
}

fn update_max(slot: &mut Option<f32>, val: f32) {
    if !val.is_finite() {
        return;
    }
    *slot = Some(match *slot {
        Some(prev) if prev >= val => prev,
        _ => val,
    });
}

fn update_min(slot: &mut Option<f32>, val: f32) {
    if !val.is_finite() {
        return;
    }
    *slot = Some(match *slot {
        Some(prev) if prev <= val => prev,
        _ => val,
    });
}

// ---------------------------------------------------------------------------
// Deterministic Stress Kernels + Self-Checking Verification
// ---------------------------------------------------------------------------

#[inline]
fn feistel_round(x: u64, key: u64) -> u64 {
    let mut v = x.wrapping_add(key);
    v = (v ^ (v >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    v = (v ^ (v >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    v ^ (v >> 31) ^ v.rotate_left(13)
}

/// Multi-lane 64-bit integer ALU/IMUL/bit-mix stress using a bijective Feistel
/// network: runs 1024 forward rounds across two independent 128-bit pairs and
/// unwinds 1024 inverse rounds, verifying bit-exact recovery of the initial state.
fn run_integer_batch(batch_idx: u64) -> std::result::Result<u64, String> {
    const ROUNDS: u64 = 1024;
    let seed = batch_idx.wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ 0xA5A5_5A5A_F00F_0FF0;

    let init_l0 = black_box(seed ^ 0x0123_4567_89AB_CDEF);
    let init_r0 = black_box(seed.rotate_left(29) ^ 0xFEDC_BA98_7654_3210);
    let init_l1 = black_box(seed.wrapping_add(0x517C_C1B7_2722_0A95));
    let init_r1 = black_box(seed.rotate_right(19) ^ 0x6C62_272E_07BB_0142);

    let (mut l0, mut r0) = (init_l0, init_r0);
    let (mut l1, mut r1) = (init_l1, init_r1);

    for round in 0..ROUNDS {
        let k0 = seed.wrapping_add(round.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let k1 = k0 ^ 0xD1B5_4A32_D192_ED03;

        let next_r0 = l0 ^ feistel_round(r0, k0);
        l0 = r0;
        r0 = next_r0;

        let next_r1 = l1 ^ feistel_round(r1, k1);
        l1 = r1;
        r1 = next_r1;
    }

    // Optimization barrier preventing the compiler from cancelling forward+inverse rounds.
    l0 = black_box(l0);
    r0 = black_box(r0);
    l1 = black_box(l1);
    r1 = black_box(r1);
    let digest = l0 ^ r0.rotate_left(17) ^ l1.rotate_left(31) ^ r1.rotate_left(47);

    for round in (0..ROUNDS).rev() {
        let k0 = seed.wrapping_add(round.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let k1 = k0 ^ 0xD1B5_4A32_D192_ED03;

        let prev_l0 = r0 ^ feistel_round(l0, k0);
        r0 = l0;
        l0 = prev_l0;

        let prev_l1 = r1 ^ feistel_round(l1, k1);
        r1 = l1;
        l1 = prev_l1;
    }

    if l0 != init_l0 || r0 != init_r0 || l1 != init_l1 || r1 != init_r1 {
        return Err(format!(
            "integer ALU self-check mismatch on batch {batch_idx}: \
             expected ({init_l0:#018x}, {init_r0:#018x}, {init_l1:#018x}, {init_r1:#018x}), \
             got ({l0:#018x}, {r0:#018x}, {l1:#018x}, {r1:#018x})"
        ));
    }

    Ok(black_box(digest))
}

fn fp_fma_pass(seed_idx: u64) -> [f64; 8] {
    const STEPS: usize = 512;
    let base = ((seed_idx & 0xFF) as f64 + 1.0) * 0.003_906_25;
    let mut acc: [f64; 8] = [
        black_box(1.000_000_1 + base),
        black_box(0.999_999_7 - base * 0.5),
        black_box(1.141_592_6 + base * 0.25),
        black_box(0.718_281_8 + base * 0.125),
        black_box(std::f64::consts::SQRT_2 - base * 0.0625),
        black_box(0.618_033_9 + base * 0.375),
        black_box(1.732_050_8 - base * 0.1875),
        black_box(0.866_025_4 + base * 0.3125),
    ];
    let mul: [f64; 8] = [
        0.999_877_929_687_5,
        0.999_755_859_375,
        0.999_633_789_062_5,
        0.999_511_718_75,
        0.999_389_648_437_5,
        0.999_267_578_125,
        0.999_145_507_812_5,
        0.999_023_437_5,
    ];
    let add: [f64; 8] = [
        0.000_317_382_812_5,
        0.000_421_142_578_125,
        0.000_518_798_828_125,
        0.000_610_351_562_5,
        0.000_701_904_296_875,
        0.000_793_457_031_25,
        0.000_885_009_765_625,
        0.000_976_562_5,
    ];

    for step in 0..STEPS {
        let bias = ((step & 7) as f64) * 0.000_122_070_312_5;
        for i in 0..8 {
            let next = acc[i].mul_add(mul[i], add[i] + bias);
            let neighbor = acc[(i + 1) & 7];
            acc[i] = next.mul_add(0.999_5, neighbor * 0.000_5);
        }
    }

    black_box(acc)
}

/// Multi-lane IEEE-754 FP64 FMA stress with dual-execution bit-exact verification
/// plus an exact closed-form dyadic FMA series check.
fn run_fp_batch(batch_idx: u64) -> std::result::Result<u64, String> {
    let pass_a = fp_fma_pass(black_box(batch_idx));
    let pass_b = fp_fma_pass(black_box(batch_idx));

    let mut digest = 0u64;
    for (lane, (&a, &b)) in pass_a.iter().zip(pass_b.iter()).enumerate() {
        if !a.is_finite() || !b.is_finite() || a.to_bits() != b.to_bits() {
            return Err(format!(
                "FP64/FMA self-check mismatch on batch {batch_idx} lane {lane}: \
                 pass_a={a:?} ({:#018x}) vs pass_b={b:?} ({:#018x})",
                a.to_bits(),
                b.to_bits()
            ));
        }
        digest ^= a.to_bits().rotate_left((lane as u32) * 7);
    }

    // Exact dyadic FMA series: sum_{k=0..1024} (k * slope + 0.5)
    // = slope * (1023 * 1024 / 2) + 512.0 = slope * 523_776.0 + 512.0.
    // Every intermediate term is an exact half-integer well below 2^52.
    const EXACT_N: usize = 1024;
    let slope = black_box(((batch_idx & 7) + 1) as f64);
    let mut exact_sum = black_box(0.0_f64);
    for k in 0..EXACT_N {
        exact_sum = (k as f64).mul_add(slope, exact_sum + 0.5);
    }
    let expected_exact = slope * 523_776.0 + 512.0;
    if exact_sum.to_bits() != expected_exact.to_bits() {
        return Err(format!(
            "FP64 exact dyadic FMA self-check mismatch on batch {batch_idx}: \
             expected {expected_exact} ({:#018x}), got {exact_sum} ({:#018x})",
            expected_exact.to_bits(),
            exact_sum.to_bits()
        ));
    }

    Ok(black_box(digest ^ exact_sum.to_bits()))
}

#[inline]
fn cache_word_pattern(idx: usize, seed: u64) -> u64 {
    let x = (idx as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(seed);
    let y = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    y ^ (y >> 27)
}

/// Prime-strided 64-byte cache-line L1/L2 write -> read-modify-write -> verify pass.
fn run_cache_batch(buf: &mut [u64], batch_idx: u64) -> std::result::Result<u64, String> {
    if buf.len() != CACHE_BUFFER_WORDS {
        return Err(format!(
            "internal cache buffer size mismatch: expected {CACHE_BUFFER_WORDS}, got {}",
            buf.len()
        ));
    }

    const WRITE_STRIDE: usize = 131;
    const RMW_STRIDE: usize = 257;
    const VERIFY_STRIDE: usize = 521;
    const LINE_MASK: usize = CACHE_LINES - 1;

    let seed = batch_idx
        .wrapping_mul(0xD1B5_4A32_D192_ED03)
        .wrapping_add(0x6A09_E667_F3BC_C908);
    let xor_mask = seed.rotate_left(23) ^ 0xA55A_5AA5_C33C_3CC3;

    // Pass 1: prime-strided cache-line writes
    for step in 0..CACHE_LINES {
        let line = step.wrapping_mul(WRITE_STRIDE) & LINE_MASK;
        let base = line * WORDS_PER_LINE;
        for w in 0..WORDS_PER_LINE {
            let idx = base + w;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = cache_word_pattern(idx, seed);
            }
        }
    }
    black_box(buf.as_mut_ptr());

    // Pass 2: prime-strided read-modify-write in a different permutation
    for step in 0..CACHE_LINES {
        let line = step.wrapping_mul(RMW_STRIDE) & LINE_MASK;
        let base = line * WORDS_PER_LINE;
        for w in 0..WORDS_PER_LINE {
            let idx = base + w;
            if let Some(slot) = buf.get_mut(idx) {
                *slot = slot.rotate_left(19) ^ xor_mask;
            }
        }
    }
    black_box(buf.as_ptr());

    // Pass 3: prime-strided full readback and bit-exact verification
    let mut digest = 0u64;
    for step in 0..CACHE_LINES {
        let line = step.wrapping_mul(VERIFY_STRIDE) & LINE_MASK;
        let base = line * WORDS_PER_LINE;
        for w in 0..WORDS_PER_LINE {
            let idx = base + w;
            let expected = cache_word_pattern(idx, seed).rotate_left(19) ^ xor_mask;
            let actual = buf.get(idx).copied().unwrap_or(0);
            if actual != expected {
                return Err(format!(
                    "L1/L2 cache self-check mismatch on batch {batch_idx} at word {idx}: \
                     expected {expected:#018x}, got {actual:#018x}"
                ));
            }
            digest ^= actual.rotate_left((idx & 63) as u32);
        }
    }

    Ok(black_box(digest))
}

fn run_workload_batch(
    workload: BenchWorkload,
    cache_buf: &mut [u64],
    batch_idx: u64,
) -> std::result::Result<u64, String> {
    match workload {
        BenchWorkload::Integer => run_integer_batch(batch_idx),
        BenchWorkload::FloatingPoint => run_fp_batch(batch_idx),
        BenchWorkload::Cache => run_cache_batch(cache_buf, batch_idx),
        BenchWorkload::Mixed => {
            let d_int = run_integer_batch(batch_idx)?;
            let d_fp = run_fp_batch(batch_idx)?;
            let d_cache = run_cache_batch(cache_buf, batch_idx)?;
            Ok(black_box(
                d_int ^ d_fp.rotate_left(11) ^ d_cache.rotate_left(29),
            ))
        }
    }
}

enum WorkerOutcome {
    Completed,
    PinError(Error),
    ComputeError(String),
}

/// Execute the Per-Core Curve Optimizer stability tester / core-cycler.
///
/// Validates `config`, then iterates over `1..=config.passes` and each target
/// core. For each core, pins a dedicated stress worker thread to that core's
/// first Linux logical CPU via `sched_setaffinity`, samples telemetry at
/// `config.sample_interval_ms`, invokes `on_progress`, detects MCE/EDAC/dmesg
/// deltas before and after the core run, and stops promptly when `stop_flag` is
/// set or when `config.stop_on_error` triggers on a failure.
pub fn run_core_cycler<F>(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    config: &BenchConfig,
    stop_flag: &Arc<AtomicBool>,
    mut on_progress: F,
) -> Result<BenchReport>
where
    F: FnMut(&BenchProgress),
{
    let targets = validate_bench_config(config, hw)?;
    let suite_start = Instant::now();
    let vcore_path = find_hwmon_vcore_path();

    let total_expected = targets.len().saturating_mul(config.passes as usize);
    let mut results = Vec::with_capacity(total_expected);
    let mut failed_cores: Vec<u32> = Vec::new();
    let mut passes_completed = 0u32;
    let mut cancelled = false;
    let mut aborted_on_error = false;
    let mut any_dmesg_readable = false;

    let target_duration = Duration::from_secs(config.duration_per_core_secs);
    let sample_interval = Duration::from_millis(config.sample_interval_ms);
    let poll_slice = Duration::from_millis(POLL_SLICE_MS);
    let total_cores = targets.len();

    for pass in 1..=config.passes {
        if stop_flag.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }

        let mut pass_finished_all_cores = true;

        for (core_pos, &(core_index, logical_cpu)) in targets.iter().enumerate() {
            if stop_flag.load(Ordering::Relaxed) {
                cancelled = true;
                pass_finished_all_cores = false;
                break;
            }

            let mce_before = read_mce_counters();
            any_dmesg_readable |= mce_before.dmesg_readable;

            let core_stop = Arc::new(AtomicBool::new(false));
            let iterations = Arc::new(AtomicU64::new(0));
            let worker_done = Arc::new(AtomicBool::new(false));

            let w_core_stop = Arc::clone(&core_stop);
            let w_global_stop = Arc::clone(stop_flag);
            let w_iterations = Arc::clone(&iterations);
            let w_done = Arc::clone(&worker_done);
            let workload = config.workload;

            let worker_handle = thread::Builder::new()
                .name(format!("zensmu-bench-c{core_index}"))
                .spawn(move || {
                    if let Err(err) = pin_current_thread_to_cpu(logical_cpu) {
                        w_done.store(true, Ordering::Release);
                        return WorkerOutcome::PinError(err);
                    }

                    let mut cache_buf = match workload {
                        BenchWorkload::Cache | BenchWorkload::Mixed => {
                            vec![0u64; CACHE_BUFFER_WORDS]
                        }
                        BenchWorkload::Integer | BenchWorkload::FloatingPoint => Vec::new(),
                    };

                    let mut batch_idx = 0u64;
                    while !w_core_stop.load(Ordering::Relaxed)
                        && !w_global_stop.load(Ordering::Relaxed)
                    {
                        match run_workload_batch(workload, &mut cache_buf, batch_idx) {
                            Ok(digest) => {
                                black_box(digest);
                                batch_idx = batch_idx.wrapping_add(1);
                                w_iterations.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(msg) => {
                                w_done.store(true, Ordering::Release);
                                return WorkerOutcome::ComputeError(msg);
                            }
                        }
                    }

                    w_done.store(true, Ordering::Release);
                    WorkerOutcome::Completed
                })
                .map_err(|source| Error::Io {
                    path: format!("spawn worker thread for core {core_index}"),
                    source,
                })?;

            let core_start = Instant::now();
            let mut peak_freq_mhz: Option<f32> = None;
            let mut peak_temp_c: Option<f32> = None;
            let mut peak_power_w: Option<f32> = None;
            let mut min_voltage_v: Option<f32> = None;
            let mut max_voltage_v: Option<f32> = None;

            loop {
                let latest_freq_mhz = read_cpu_freq_mhz(logical_cpu);
                if let Some(mhz) = latest_freq_mhz {
                    update_max(&mut peak_freq_mhz, mhz);
                }

                let mut latest_temp_c = None;
                if let Ok(snap) = pm_table::read_telemetry(transport, hw) {
                    if let Some(tctl) = snap.tctl_value_c {
                        latest_temp_c = Some(tctl);
                        update_max(&mut peak_temp_c, tctl);
                    }
                    if let Some(pwr) = snap.ppt_value_w.or(snap.stapm_value_w) {
                        update_max(&mut peak_power_w, pwr);
                    }
                }

                if let Some(volts) = vcore_path.as_deref().and_then(read_hwmon_voltage_v) {
                    update_min(&mut min_voltage_v, volts);
                    update_max(&mut max_voltage_v, volts);
                }

                let elapsed = core_start.elapsed();
                on_progress(&BenchProgress {
                    pass,
                    total_passes: config.passes,
                    core_index,
                    core_pos,
                    total_cores,
                    elapsed_secs: elapsed.as_secs_f64(),
                    target_secs: config.duration_per_core_secs,
                    latest_freq_mhz,
                    latest_temp_c,
                });

                if stop_flag.load(Ordering::Relaxed) {
                    cancelled = true;
                    break;
                }
                if worker_done.load(Ordering::Acquire) {
                    break;
                }
                if elapsed >= target_duration {
                    break;
                }

                let remaining_target = target_duration.saturating_sub(elapsed);
                let sleep_budget = remaining_target.min(sample_interval);
                let sleep_start = Instant::now();

                while sleep_start.elapsed() < sleep_budget {
                    if stop_flag.load(Ordering::Relaxed) || worker_done.load(Ordering::Acquire) {
                        break;
                    }
                    let left = sleep_budget.saturating_sub(sleep_start.elapsed());
                    if left.is_zero() {
                        break;
                    }
                    thread::sleep(left.min(poll_slice));
                }
            }

            core_stop.store(true, Ordering::Release);

            let worker_outcome = match worker_handle.join() {
                Ok(outcome) => outcome,
                Err(_) => {
                    WorkerOutcome::ComputeError("benchmark worker thread exited abnormally".into())
                }
            };

            if let WorkerOutcome::PinError(err) = worker_outcome {
                return Err(err);
            }

            let duration_secs = core_start.elapsed().as_secs_f64();
            let completed_iters = iterations.load(Ordering::Relaxed);

            let mce_after = read_mce_counters();
            any_dmesg_readable |= mce_after.dmesg_readable;

            let mce_delta_corrected = mce_after
                .corrected_errors
                .saturating_sub(mce_before.corrected_errors);
            let mce_delta_uncorrected = mce_after
                .uncorrected_errors
                .saturating_sub(mce_before.uncorrected_errors);
            let new_mce_lines =
                diff_mce_lines(&mce_before.dmesg_mce_lines, &mce_after.dmesg_mce_lines);
            let mce_failed =
                mce_delta_uncorrected > 0 || mce_delta_corrected > 0 || !new_mce_lines.is_empty();

            let (status, error) = if mce_failed {
                let mut detail = format!(
                    "hardware MCE detected on core {core_index} (CE +{mce_delta_corrected}, UE +{mce_delta_uncorrected}, {} new kernel log line(s))",
                    new_mce_lines.len()
                );
                if let WorkerOutcome::ComputeError(ref compute_msg) = worker_outcome {
                    detail.push_str("; ");
                    detail.push_str(compute_msg);
                }
                (CoreBenchStatus::FailedMce, Some(detail))
            } else if let WorkerOutcome::ComputeError(compute_msg) = worker_outcome {
                (CoreBenchStatus::FailedCompute, Some(compute_msg))
            } else if cancelled || stop_flag.load(Ordering::Relaxed) {
                cancelled = true;
                (CoreBenchStatus::Cancelled, None)
            } else {
                (CoreBenchStatus::Passed, None)
            };

            results.push(CoreBenchResult {
                pass,
                core_index,
                logical_cpu,
                status,
                duration_secs,
                iterations: completed_iters,
                peak_freq_mhz,
                peak_temp_c,
                peak_power_w,
                min_voltage_v,
                max_voltage_v,
                mce_delta_corrected,
                mce_delta_uncorrected,
                new_mce_lines,
                error,
            });

            if matches!(
                status,
                CoreBenchStatus::FailedMce | CoreBenchStatus::FailedCompute
            ) {
                if !failed_cores.contains(&core_index) {
                    failed_cores.push(core_index);
                }
                if config.stop_on_error {
                    aborted_on_error = true;
                    pass_finished_all_cores = false;
                    break;
                }
            }

            if cancelled {
                pass_finished_all_cores = false;
                break;
            }
        }

        if pass_finished_all_cores {
            passes_completed = pass;
        }
        if cancelled || aborted_on_error {
            break;
        }
    }

    let passed = !cancelled && failed_cores.is_empty() && !results.is_empty();

    Ok(BenchReport {
        codename: hw.codename.display_name().to_string(),
        workload: config.workload,
        passes_completed,
        total_duration_secs: suite_start.elapsed().as_secs_f64(),
        passed,
        cancelled,
        failed_cores,
        dmesg_readable: any_dmesg_readable,
        results,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::profiles::capabilities_for;
    use crate::hw::topology::CpuTopology;
    use crate::transport::mock::MockTransport;
    use crate::types::{Codename, CoreInfo};

    /// Return the first Linux logical CPU allowed for the current process so
    /// affinity tests succeed even inside constrained cgroups/taskset runners.
    #[cfg(target_os = "linux")]
    fn first_allowed_logical_cpu() -> u32 {
        // SAFETY: `cpu_set_t` is zero-initialized and populated by `sched_getaffinity`
        // for the calling thread (`pid = 0`), then queried for indices `< CPU_SETSIZE`.
        unsafe {
            let mut set: libc::cpu_set_t = std::mem::zeroed();
            if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) == 0 {
                let max_cpus = usize::try_from(libc::CPU_SETSIZE).unwrap_or(1024);
                for cpu in 0..max_cpus {
                    if libc::CPU_ISSET(cpu, &set) {
                        return cpu as u32;
                    }
                }
            }
        }
        0
    }

    #[cfg(not(target_os = "linux"))]
    fn first_allowed_logical_cpu() -> u32 {
        0
    }

    fn mock_hw_9700x(transport: &MockTransport, first_cpu: u32) -> HardwareContext {
        let driver = transport.read_info().unwrap();
        let mut topology = CpuTopology::synthetic(1, 8, 2);
        if let Some(c0) = topology.cores.first_mut() {
            c0.linux_cpus = vec![first_cpu];
        }
        HardwareContext {
            driver,
            codename: Codename::GraniteRidge,
            cpuid: None,
            topology,
            capabilities: capabilities_for(Codename::GraniteRidge).unwrap(),
        }
    }

    #[test]
    fn extract_mce_lines_filters_relevant_kernel_messages() {
        let log = "\
[Thu Nov 26 20:10:01 2026] usb 1-2: device descriptor read/64, error -71
[Thu Nov 26 20:10:05 2026] mce: [Hardware Error]: CPU 3: Machine Check: 0 Bank 5: bea0000000000108
[Thu Nov 26 20:10:05 2026] [Hardware Error]: TSC 0 ADDR 1ffff8123456 MISC d012000100000000
[Thu Nov 26 20:10:06 2026] WHEA: Corrected hardware error on processor core 3
[Thu Nov 26 20:10:07 2026] ext4-fs error (device nvme0n1p2): benign test line
[Thu Nov 26 20:10:08 2026] Machine check events logged
";
        let lines = extract_mce_lines(log);
        assert_eq!(lines.len(), 4);
        assert!(lines[0].contains("mce:"));
        assert!(lines[1].contains("[Hardware Error]"));
        assert!(lines[2].contains("WHEA:"));
        assert!(lines[3].contains("Machine check events logged"));
    }

    #[test]
    fn diff_mce_lines_handles_appends_duplicates_and_sliding_windows() {
        let empty: Vec<String> = Vec::new();
        let a = vec!["mce: err 1".to_string(), "mce: err 2".to_string()];
        assert_eq!(diff_mce_lines(&empty, &a), a);
        assert!(diff_mce_lines(&a, &empty).is_empty());
        assert!(diff_mce_lines(&a, &a).is_empty());

        // Normal append including a duplicate of the previous line
        let b = vec![
            "mce: err 1".to_string(),
            "mce: err 2".to_string(),
            "mce: err 2".to_string(),
            "mce: err 3".to_string(),
        ];
        assert_eq!(
            diff_mce_lines(&a, &b),
            vec!["mce: err 2".to_string(), "mce: err 3".to_string()]
        );

        // Sliding ring-buffer window (oldest line rotated out, 1 new line added)
        let rotated = vec![
            "mce: err 2".to_string(),
            "mce: err 2".to_string(),
            "mce: err 3".to_string(),
            "mce: err 4".to_string(),
        ];
        assert_eq!(diff_mce_lines(&b, &rotated), vec!["mce: err 4".to_string()]);

        // Completely disjoint
        let disjoint = vec!["mce: brand new".to_string()];
        assert_eq!(diff_mce_lines(&a, &disjoint), disjoint);
    }

    #[test]
    fn mce_sysfs_reader_sums_machinecheck_error_counts_and_edac() {
        let dir = tempfile::tempdir().unwrap();
        let mc_root = dir.path().join("machinecheck");
        let edac_root = dir.path().join("edac_mc");

        // AMD Zen machinecheck hierarchy: bank0 is a config mask and must be ignored
        let mc0 = mc_root.join("machinecheck0");
        fs::create_dir_all(mc0.join("execution_unit/execution_unit")).unwrap();
        fs::create_dir_all(mc0.join("l2_cache/l2_cache")).unwrap();
        fs::create_dir_all(mc0.join("power")).unwrap();
        fs::write(mc0.join("bank0"), "0xffffffffffffffff\n").unwrap();
        fs::write(mc0.join("execution_unit/execution_unit/error_count"), "2\n").unwrap();
        fs::write(mc0.join("l2_cache/l2_cache/error_count"), "0x3\n").unwrap();

        let edac_mc0 = edac_root.join("mc0");
        fs::create_dir_all(&edac_mc0).unwrap();
        fs::write(edac_mc0.join("ce_count"), "4\n").unwrap();
        fs::write(edac_mc0.join("ue_count"), "1\n").unwrap();

        let (ce, ue) = read_mce_sysfs_counters(&mc_root, &edac_root);
        assert_eq!(ce, 2 + 3 + 4);
        assert_eq!(ue, 1);
    }

    #[test]
    fn all_four_workloads_pass_deterministic_self_check() {
        let mut cache_buf = vec![0u64; CACHE_BUFFER_WORDS];
        for workload in [
            BenchWorkload::Mixed,
            BenchWorkload::Integer,
            BenchWorkload::FloatingPoint,
            BenchWorkload::Cache,
        ] {
            for batch in 0..3u64 {
                let res = run_workload_batch(workload, &mut cache_buf, batch);
                assert!(
                    res.is_ok(),
                    "workload {workload} failed deterministic self-check on batch {batch}: {res:?}"
                );
            }
        }
    }

    #[test]
    fn validate_bench_config_checks_bounds_and_core_resolution() {
        let transport = MockTransport::granite_ridge_9700x();
        let hw = mock_hw_9700x(&transport, 0);

        // Default config expands empty `cores` to all 8 physical cores
        let all = validate_bench_config(&BenchConfig::default(), &hw).unwrap();
        assert_eq!(all.len(), 8);
        assert_eq!(all[0], (0, 0));
        assert_eq!(all[7], (7, 7));

        // Explicit subset deduplicates while preserving order
        let subset_cfg = BenchConfig {
            cores: vec![3, 1, 3, 5],
            ..BenchConfig::default()
        };
        let subset = validate_bench_config(&subset_cfg, &hw).unwrap();
        assert_eq!(subset, vec![(3, 3), (1, 1), (5, 5)]);

        // Invalid core index
        let bad_core = BenchConfig {
            cores: vec![99],
            ..BenchConfig::default()
        };
        assert!(matches!(
            validate_bench_config(&bad_core, &hw),
            Err(Error::InvalidInput(_))
        ));

        // Out-of-bounds duration, passes, and sample interval
        for bad_cfg in [
            BenchConfig {
                duration_per_core_secs: 0,
                ..BenchConfig::default()
            },
            BenchConfig {
                duration_per_core_secs: 3601,
                ..BenchConfig::default()
            },
            BenchConfig {
                passes: 0,
                ..BenchConfig::default()
            },
            BenchConfig {
                passes: 101,
                ..BenchConfig::default()
            },
            BenchConfig {
                sample_interval_ms: 50,
                ..BenchConfig::default()
            },
        ] {
            assert!(matches!(
                validate_bench_config(&bad_cfg, &hw),
                Err(Error::OutOfBounds { .. })
            ));
        }

        // Core with no mapped logical CPU is rejected
        let mut broken_hw = hw.clone();
        broken_hw.topology.cores[0] = CoreInfo {
            index: 0,
            ccd: 0,
            ccx: 0,
            core_in_ccd: 0,
            linux_cpus: Vec::new(),
        };
        assert!(matches!(
            validate_bench_config(&BenchConfig::default(), &broken_hw),
            Err(Error::InvalidInput(_))
        ));
    }

    #[test]
    fn workload_and_status_display_parse_and_serde_roundtrip() {
        assert_eq!(BenchWorkload::default(), BenchWorkload::Mixed);
        assert_eq!(BenchWorkload::Mixed.to_string(), "mixed");
        assert_eq!(BenchWorkload::Integer.to_string(), "integer");
        assert_eq!(BenchWorkload::FloatingPoint.to_string(), "floating-point");
        assert_eq!(BenchWorkload::Cache.to_string(), "cache");

        assert_eq!(
            BenchWorkload::parse("FP"),
            Some(BenchWorkload::FloatingPoint)
        );
        assert_eq!(BenchWorkload::parse("int"), Some(BenchWorkload::Integer));
        assert_eq!(BenchWorkload::parse("cache"), Some(BenchWorkload::Cache));
        assert_eq!(BenchWorkload::parse("unknown"), None);

        let cfg = BenchConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        let decoded: BenchConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, cfg);
    }

    #[test]
    fn run_core_cycler_one_second_mock_transport() {
        let transport = MockTransport::granite_ridge_9700x();
        let cpu = first_allowed_logical_cpu();
        let hw = mock_hw_9700x(&transport, cpu);

        let config = BenchConfig {
            cores: vec![0],
            duration_per_core_secs: 1,
            passes: 1,
            workload: BenchWorkload::Mixed,
            sample_interval_ms: 200,
            stop_on_error: true,
        };
        let stop_flag = Arc::new(AtomicBool::new(false));
        let mut progress_events = Vec::new();

        let report = run_core_cycler(&transport, &hw, &config, &stop_flag, |p| {
            progress_events.push(p.clone());
        })
        .unwrap();

        assert_eq!(report.codename, Codename::GraniteRidge.display_name());
        assert_eq!(report.workload, BenchWorkload::Mixed);
        assert_eq!(report.passes_completed, 1);
        assert!(!report.cancelled);
        assert_eq!(report.results.len(), 1);
        assert!(!progress_events.is_empty());

        let r = &report.results[0];
        assert_eq!(r.pass, 1);
        assert_eq!(r.core_index, 0);
        assert_eq!(r.logical_cpu, cpu);
        assert!(
            r.iterations > 0,
            "expected worker to complete stress batches"
        );
        assert!(r.duration_secs >= 0.9);
        assert_eq!(r.peak_temp_c, Some(54.2));
        assert_eq!(r.peak_power_w, Some(42.5));

        if r.status == CoreBenchStatus::Passed {
            assert!(report.passed);
            assert!(report.failed_cores.is_empty());
        }
    }

    #[test]
    fn run_core_cycler_cancels_cleanly_via_stop_flag() {
        let transport = MockTransport::granite_ridge_9700x();
        let cpu = first_allowed_logical_cpu();
        let hw = mock_hw_9700x(&transport, cpu);

        let config = BenchConfig {
            cores: vec![0],
            duration_per_core_secs: 10,
            passes: 1,
            workload: BenchWorkload::Integer,
            sample_interval_ms: 100,
            stop_on_error: true,
        };
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_from_cb = Arc::clone(&stop_flag);

        let report = run_core_cycler(&transport, &hw, &config, &stop_flag, |_| {
            stop_from_cb.store(true, Ordering::Relaxed);
        })
        .unwrap();

        assert!(report.cancelled);
        assert!(!report.passed);
        assert_eq!(report.passes_completed, 0);
        assert_eq!(report.results.len(), 1);
        assert_eq!(report.results[0].status, CoreBenchStatus::Cancelled);
        assert!(report.total_duration_secs < 2.0);
    }
}
