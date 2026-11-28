//! Zero-dependency Per-Core Curve Optimizer Stability Tester / Core-Cycler.
//!
//! Pins a deterministic, self-verifying stress worker thread to each selected
//! physical core (via `libc::sched_setaffinity`) while sampling live telemetry
//! (`pm_table::read_telemetry`, Linux `cpufreq`, optional `hwmon` Vcore) and
//! checking Linux MCE / EDAC / kernel log deltas before and after each core
//! run to catch silent computational corruption and hardware Machine Check
//! Exceptions caused by unstable Curve Optimizer undervolts.

mod mce;
mod runner;
mod system;
#[cfg(test)]
mod tests;
mod workload;

use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt;

pub use mce::{diff_mce_lines, extract_mce_lines, read_mce_counters, MceCounters};
pub use runner::run_core_cycler;

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
