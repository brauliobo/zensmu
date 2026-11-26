//! Human-readable terminal rendering and structured JSON serialization for all CLI subcommands.

use crate::doctor::{CheckStatus, DoctorReport};
use serde::Serialize;
use std::sync::OnceLock;
use zensmu_core::config::profile::ProfileDiffEntry;
use zensmu_core::error::Error;
use zensmu_core::hw::identity::{driver_has_false_ok_bug, CpuidInfo, HardwareContext};
use zensmu_core::hw::profiles::CoBounds;
use zensmu_core::hw::topology::CpuTopology;
use zensmu_core::ops::co::{CoSnapshot, CoreCoMargin};
use zensmu_core::ops::pbo::PboStatus;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::recorder::{RecordServiceStatus, RecordStats};
use zensmu_core::report::{DebugReport, SystemBoardInfo, GITHUB_REPO};
use zensmu_core::safety::persist::{
    PersistStatus, DEFAULT_MODULES_LOAD_PATH, DEFAULT_RESUME_HOOK_PATH,
};
use zensmu_core::transport::DriverSysfsInfo;
use zensmu_core::types::{Codename, CurveShaperState, FreqTier, PlannedOp};

// ============================================================================
// Structured JSON Output Schemas
// ============================================================================

#[derive(Debug, Serialize)]
pub struct CapabilityItem {
    pub name: String,
    pub supported: bool,
}

#[derive(Debug, Serialize)]
pub struct InfoOutput {
    pub codename: Codename,
    pub codename_name: String,
    pub cpuid: Option<CpuidInfo>,
    pub driver: DriverSysfsInfo,
    pub topology: CpuTopology,
    pub capabilities: Vec<CapabilityItem>,
    pub co_bounds: Option<CoBounds>,
    pub false_ok_bug_affected: bool,
}

impl InfoOutput {
    pub fn from_hw(hw: &HardwareContext) -> Self {
        let capabilities = hw
            .capabilities
            .feature_summary()
            .into_iter()
            .map(|(name, supported)| CapabilityItem {
                name: name.to_string(),
                supported,
            })
            .collect();
        Self {
            codename: hw.codename,
            codename_name: hw.codename.display_name().to_string(),
            cpuid: hw.cpuid.clone(),
            driver: hw.driver.clone(),
            topology: hw.topology.clone(),
            capabilities,
            co_bounds: hw.capabilities.co_bounds,
            false_ok_bug_affected: driver_has_false_ok_bug(&hw.driver.drv_version),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StatusOutput {
    pub codename: Codename,
    pub codename_name: String,
    pub is_root: bool,
    pub telemetry: Option<TelemetrySnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry_error: Option<String>,
    pub co: Option<CoSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub co_error: Option<String>,
    pub curve_shaper: Option<CurveShaperState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curve_shaper_error: Option<String>,
    pub pbo: Option<PboStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pbo_error: Option<String>,
    pub guard_tripped: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CoGetOutput {
    pub codename: Codename,
    pub cores: Vec<CoreCoMargin>,
    pub igpu: Option<i16>,
}

#[derive(Debug, Serialize)]
pub struct ShaperGetOutput {
    pub codename: Codename,
    pub shaper: CurveShaperState,
}

#[derive(Debug, Default, Serialize)]
pub struct ActivePboLimits {
    pub ppt_w: Option<f32>,
    pub tdc_a: Option<f32>,
    pub edc_a: Option<f32>,
    pub tctl_c: Option<f32>,
}

impl ActivePboLimits {
    pub fn from_telemetry(t: &TelemetrySnapshot) -> Self {
        Self {
            ppt_w: t.ppt_limit_w,
            tdc_a: t.tdc_limit_a,
            edc_a: t.edc_limit_a,
            tctl_c: t.tctl_limit_c,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PboGetOutput {
    pub codename: Codename,
    pub active_limits: ActivePboLimits,
    pub status: PboStatus,
}

#[derive(Debug, Serialize)]
pub struct TelemetryDumpOutput {
    pub path: String,
    pub bytes: usize,
    pub table_version: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct RecordServiceActionOutput {
    pub action: String,
    pub dry_run: bool,
    pub status: RecordServiceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RecordExportPtsOutput {
    pub input: String,
    pub output: String,
    pub samples: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pts_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MutationOutput {
    pub action: String,
    pub dry_run: bool,
    pub applied: bool,
    pub rolled_back: bool,
    pub confirmed: Option<bool>,
    pub operations: Vec<PlannedOp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProfileSaveOutput {
    pub path: String,
    pub codename: Codename,
    pub physical_cores: usize,
}

#[derive(Debug, Serialize)]
pub struct ProfileValidateOutput {
    pub path: String,
    pub valid: bool,
    pub target_codename: Codename,
    pub target_physical_cores: Option<usize>,
    pub host_codename: Option<Codename>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProfileDiffOutput {
    pub path: String,
    pub identical: bool,
    pub diffs: Vec<ProfileDiffEntry>,
}

#[derive(Debug, Serialize)]
pub struct ProfileListItem {
    pub name: String,
    pub path: String,
    pub codename: Option<Codename>,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProfileListOutput {
    pub dir: String,
    pub profiles: Vec<ProfileListItem>,
}

#[derive(Debug, Serialize)]
pub struct ClearGuardOutput {
    pub guard_path: String,
    pub was_tripped: bool,
    pub cleared: bool,
}

#[derive(Debug, Serialize)]
pub struct PersistActionOutput {
    pub action: String,
    pub dry_run: bool,
    pub active_profile_path: String,
    pub service_enabled: bool,
    pub status: PersistStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReportOutput {
    pub report: DebugReport,
    pub issue_title: String,
    pub issue_labels: Vec<String>,
    pub issue_url: String,
    pub markdown: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorOutput {
    pub ok: bool,
    pub kind: &'static str,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

// ============================================================================
// JSON Helpers
// ============================================================================

pub fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("failed to serialize JSON output: {e}"),
    }
}

pub fn print_json_compact<T: Serialize>(value: &T) {
    match serde_json::to_string(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("failed to serialize JSON output: {e}"),
    }
}

pub fn error_kind(err: &Error) -> &'static str {
    match err {
        Error::DriverNotLoaded { .. } => "driver_not_loaded",
        Error::PermissionDenied { .. } => "permission_denied",
        Error::LockFailed { .. } => "lock_failed",
        Error::UnsupportedCpu { .. } => "unsupported_cpu",
        Error::IdentityMismatch { .. } => "identity_mismatch",
        Error::UnsupportedCapability { .. } => "unsupported_capability",
        Error::SmuRejected { .. } => "smu_rejected",
        Error::MailboxUnhealthy { .. } => "mailbox_unhealthy",
        Error::VerificationFailed { .. } => "verification_failed",
        Error::HardwareClamped { .. } => "hardware_clamped",
        Error::OutOfBounds { .. } => "out_of_bounds",
        Error::ExtremeValueBlocked { .. } => "extreme_value_blocked",
        Error::UnsupportedPmTable { .. } => "unsupported_pm_table",
        Error::PmTableTooLarge { .. } => "pm_table_too_large",
        Error::InvalidInput(_) => "invalid_input",
        Error::BootGuardTripped { .. } => "boot_guard_tripped",
        Error::ProfileTargetMismatch { .. } => "profile_target_mismatch",
        Error::Io { .. } => "io",
        Error::Serde(_) => "serde",
    }
}

pub fn print_error(err: &Error, json: bool) {
    if json {
        let payload = ErrorOutput {
            ok: false,
            kind: error_kind(err),
            error: err.to_string(),
            hint: err.report_hint().map(str::to_string),
        };
        if let Ok(s) = serde_json::to_string(&payload) {
            eprintln!("{s}");
        } else {
            eprintln!("error: {err}");
        }
    } else {
        eprintln!("error: {err}");
        if let Some(hint) = err.report_hint() {
            eprintln!("hint: {hint}");
        }
    }
}

// ============================================================================
// Human-Readable Formatters
// ============================================================================

fn format_signed(v: i32) -> String {
    if v > 0 {
        format!("+{v}")
    } else {
        v.to_string()
    }
}

pub fn format_signed_i16(v: i16) -> String {
    format_signed(i32::from(v))
}

pub fn format_signed_i8(v: i8) -> String {
    format_signed(i32::from(v))
}

pub fn format_cpu_list(cpus: &[u32]) -> String {
    cpus.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

pub fn print_info(info: &InfoOutput) {
    println!("CPU & Driver Information");
    println!("========================");
    if let Some(ref c) = info.cpuid {
        println!("CPU Brand        : {}", c.brand);
        println!(
            "CPUID            : {} (Family 0x{:X}, Model 0x{:X}, Stepping {})",
            c.vendor, c.family, c.model, c.stepping
        );
    } else {
        println!("CPUID            : (check skipped or unavailable)");
    }
    println!(
        "Codename         : {} [slug: {}, id: {}]",
        info.codename_name,
        info.codename.slug(),
        info.driver.codename_id
    );
    let bug_note = if info.false_ok_bug_affected {
        " (smu.c:213 false-OK bug mitigated by zensmu)"
    } else {
        ""
    };
    println!("Driver Version   : {}{}", info.driver.drv_version, bug_note);
    println!("SMU Version      : {}", info.driver.smu_version);
    println!("MP1 IF Version   : {}", info.driver.mp1_if_version);
    println!(
        "RSMU Mailbox     : {}",
        if info.driver.has_rsmu {
            "available"
        } else {
            "not exposed"
        }
    );
    if info.driver.has_pm_table {
        let ver = info
            .driver
            .pm_table_version
            .map(|v| format!("0x{v:08X}"))
            .unwrap_or_else(|| "unknown".into());
        let sz = info
            .driver
            .pm_table_size
            .map(|s| format!("{s} bytes (0x{s:X})"))
            .unwrap_or_else(|| "unknown".into());
        println!("PM Table         : version {ver}, size {sz}");
    } else {
        println!("PM Table         : not exposed");
    }
    println!(
        "Topology         : {} CCD(s), {} physical core(s), {} logical CPU(s)",
        info.topology.ccd_count, info.topology.physical_cores, info.topology.logical_cpus
    );
    if let Some(bounds) = info.co_bounds {
        println!(
            "CO Bounds        : {}..=+{} (conservative soft floor: {})",
            bounds.min, bounds.max, bounds.soft_min
        );
    }

    println!();
    println!("Capabilities");
    println!("------------");
    for cap in &info.capabilities {
        let mark = if cap.supported { "yes" } else { "no" };
        println!("  {:<24} {}", cap.name, mark);
    }
}

pub fn print_doctor(report: &DoctorReport) {
    println!("zensmu Doctor");
    println!("=============");
    println!("Sysfs Path : {}", report.sysfs_path);
    println!("Guard Path : {}", report.guard_path);
    println!(
        "Privileges : {}",
        if report.is_root {
            "root (full mailbox + telemetry access)"
        } else {
            "unprivileged (telemetry/info only)"
        }
    );
    println!();

    for check in &report.checks {
        println!(
            "[{:4}] {}: {}",
            check.status.tag(),
            check.name,
            check.message
        );
        if let Some(ref rem) = check.remediation {
            println!("       -> {rem}");
        }
    }

    let (mut pass, mut warn, mut fail, mut skip) = (0usize, 0usize, 0usize, 0usize);
    for check in &report.checks {
        match check.status {
            CheckStatus::Pass => pass += 1,
            CheckStatus::Warn => warn += 1,
            CheckStatus::Fail => fail += 1,
            CheckStatus::Skip => skip += 1,
        }
    }

    println!();
    println!(
        "Summary: {} ({pass} passed, {warn} warnings, {fail} failed, {skip} skipped)",
        if report.healthy {
            "HEALTHY"
        } else {
            "ISSUES DETECTED"
        }
    );
}

pub fn print_co_table(co: &CoGetOutput) {
    println!("Curve Optimizer ({})", co.codename.display_name());
    println!("----------------------------------------------");
    println!(
        "{:<6} {:<5} {:<7} {:<14} {:>8}",
        "CORE", "CCD", "LOCAL", "LINUX CPUS", "MARGIN"
    );
    for c in &co.cores {
        println!(
            "{:<6} {:<5} {:<7} {:<14} {:>8}",
            c.core_index,
            c.ccd,
            c.core_in_ccd,
            format_cpu_list(&c.linux_cpus),
            format_signed_i16(c.margin)
        );
    }
    if let Some(igpu) = co.igpu {
        println!("----------------------------------------------");
        println!("iGPU CO Margin: {}", format_signed_i16(igpu));
    }
}

pub fn print_shaper_matrix(shaper: &CurveShaperState, codename: Codename) {
    println!("Curve Shaper 5x3 Matrix ({})", codename.display_name());
    println!("-------------------------------------------");
    println!(
        "{:<10} {:>10} {:>10} {:>10}",
        "FREQ TIER", "LOW TEMP", "MED TEMP", "HIGH TEMP"
    );
    for tier in FreqTier::ALL {
        let m = shaper.tier(tier);
        println!(
            "{:<10} {:>10} {:>10} {:>10}",
            tier.label(),
            format_signed_i8(m.low),
            format_signed_i8(m.med),
            format_signed_i8(m.high),
        );
    }
}

pub(crate) fn detected_board_info() -> &'static SystemBoardInfo {
    static BOARD_INFO: OnceLock<SystemBoardInfo> = OnceLock::new();
    BOARD_INFO.get_or_init(SystemBoardInfo::detect_linux)
}

pub(crate) fn format_board_vrm_note(info: &SystemBoardInfo) -> String {
    let mut name_parts = Vec::new();
    if let Some(vendor) = info
        .board_vendor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        name_parts.push(vendor);
    }
    if let Some(board) = info
        .board_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        let dup_vendor = name_parts.first().is_some_and(|v| {
            board
                .to_ascii_lowercase()
                .starts_with(&v.to_ascii_lowercase())
        });
        if dup_vendor {
            name_parts.clear();
        }
        name_parts.push(board);
    }
    let bios = info
        .bios_version
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let mut label = name_parts.join(" ");
    if let Some(ver) = bios {
        if label.is_empty() {
            label = format!("BIOS {ver}");
        } else {
            label.push_str(&format!(" (BIOS {ver})"));
        }
    }

    if label.is_empty() {
        "Board: BIOS PBO Limits / VRM firmware may cap PPT/TDC/EDC.".to_string()
    } else {
        format!("Board: {label} — BIOS PBO Limits / VRM firmware may cap PPT/TDC/EDC.")
    }
}

pub fn format_pbo_status(out: &PboGetOutput) -> String {
    format_pbo_status_with_board(out, detected_board_info())
}

fn format_pbo_status_with_board(out: &PboGetOutput, board: &SystemBoardInfo) -> String {
    use std::fmt::Write as _;

    let mut s = String::new();
    let _ = writeln!(
        s,
        "Precision Boost Overdrive ({})",
        out.codename.display_name()
    );
    let _ = writeln!(s, "==========================================");

    let _ = writeln!(s, "Active Limits (PM Table):");
    let _ = writeln!(
        s,
        "  PPT   : {}",
        fmt_opt_f32(out.active_limits.ppt_w, 1, "W")
    );
    let _ = writeln!(
        s,
        "  TDC   : {}",
        fmt_opt_f32(out.active_limits.tdc_a, 1, "A")
    );
    let _ = writeln!(
        s,
        "  EDC   : {}",
        fmt_opt_f32(out.active_limits.edc_a, 1, "A")
    );
    let _ = writeln!(
        s,
        "  Tctl  : {}",
        fmt_opt_f32(out.active_limits.tctl_c, 1, "°C")
    );

    let _ = writeln!(s);
    let _ = writeln!(s, "Boost & Scalar:");
    let _ = writeln!(
        s,
        "  PBO Scalar : {}",
        out.status
            .scalar
            .map(|sc| format!("{sc:.2}x"))
            .unwrap_or_else(|| "n/a".into())
    );
    let _ = writeln!(
        s,
        "  Fmax Limit : {}",
        out.status
            .fmax_mhz
            .map(|m| format!("{m} MHz"))
            .unwrap_or_else(|| "n/a".into())
    );

    let _ = writeln!(s);
    let _ = writeln!(s, "Fused Factory Limits:");
    let f = &out.status.fused;
    let _ = writeln!(s, "  Fused PPT      : {}", fmt_opt_u32(f.ppt_w, "W"));
    let _ = writeln!(s, "  Fused VDD TDC  : {}", fmt_opt_u32(f.vdd_tdc_a, "A"));
    let _ = writeln!(s, "  Fused EDC      : {}", fmt_opt_u32(f.edc_a, "A"));
    if f.tdp_w.is_some() {
        let _ = writeln!(s, "  Fused TDP      : {}", fmt_opt_u32(f.tdp_w, "W"));
    }
    if f.tjmax_c.is_some() {
        let _ = writeln!(s, "  Fused TjMax    : {}", fmt_opt_u32(f.tjmax_c, "°C"));
    }
    if f.soc_tdc_a.is_some() {
        let _ = writeln!(s, "  Fused SoC TDC  : {}", fmt_opt_u32(f.soc_tdc_a, "A"));
    }
    if f.slow_w.is_some() || f.fast_w.is_some() || f.apu_slow_w.is_some() {
        let _ = writeln!(s, "  Fused Slow     : {}", fmt_opt_u32(f.slow_w, "W"));
        let _ = writeln!(s, "  Fused Fast     : {}", fmt_opt_u32(f.fast_w, "W"));
        let _ = writeln!(s, "  Fused APU Slow : {}", fmt_opt_u32(f.apu_slow_w, "W"));
    }
    let _ = writeln!(s, "  {}", format_board_vrm_note(board));

    let _ = writeln!(s);
    let _ = writeln!(s, "Platform Flags:");
    let _ = writeln!(
        s,
        "  Overclockable  : {}",
        fmt_opt_bool(out.status.flags.overclockable)
    );
    let _ = writeln!(
        s,
        "  EXPO Active    : {}",
        fmt_opt_bool(out.status.flags.expo_active)
    );
    let _ = writeln!(
        s,
        "  LN2 Mode       : {}",
        fmt_opt_bool(out.status.flags.ln2_mode)
    );

    if !out.status.core_perf_scores.is_empty() {
        let _ = writeln!(s);
        let _ = writeln!(s, "CPPC Core Performance Ranking:");
        let mut ranked = out.status.core_perf_scores.clone();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let _ = writeln!(s, "  {:<6} {:<8} {:>8}", "RANK", "CORE", "SCORE");
        for (rank, (core_idx, score)) in ranked.iter().enumerate() {
            let _ = writeln!(s, "  {:<6} {:<8} {:>8}", rank + 1, core_idx, score);
        }
    }
    s
}

pub fn print_pbo_status(out: &PboGetOutput) {
    print!("{}", format_pbo_status(out));
}

pub fn print_report_footer(out: &ReportOutput, saved_path: Option<&std::path::Path>) {
    println!();
    println!("GitHub Issue Submission ({GITHUB_REPO})");
    println!("========================================");
    if let Some(path) = saved_path {
        println!("Saved Report     : {}", path.display());
    }
    println!("Suggested Title  : {}", out.issue_title);
    if !out.issue_labels.is_empty() {
        println!("Suggested Labels : {}", out.issue_labels.join(", "));
    }
    println!("Prefilled URL    : {}", out.issue_url);
    println!();
    println!("Next steps:");
    println!("  - Open in browser    : zensmu report --open");
    println!("  - File via GitHub CLI: zensmu report --create");
}

/// Returns `true` when any mobile APU DPTC telemetry field is populated.
pub(crate) fn has_apu_telemetry(t: &TelemetrySnapshot) -> bool {
    t.stapm_value_w.is_some()
        || t.stapm_limit_w.is_some()
        || t.slow_value_w.is_some()
        || t.slow_limit_w.is_some()
        || t.apu_slow_value_w.is_some()
        || t.apu_slow_limit_w.is_some()
        || t.tdc_soc_value_a.is_some()
        || t.tdc_soc_limit_a.is_some()
        || t.edc_soc_value_a.is_some()
        || t.edc_soc_limit_a.is_some()
        || t.apu_skin_temp_value_c.is_some()
        || t.apu_skin_temp_limit_c.is_some()
        || t.dgpu_skin_temp_value_c.is_some()
        || t.dgpu_skin_temp_limit_c.is_some()
}

pub fn format_telemetry(t: &TelemetrySnapshot) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(
        out,
        "Telemetry — {} (PM Table 0x{:08X}, {} bytes)",
        t.codename.display_name(),
        t.table_version,
        t.table_size
    );
    let _ = writeln!(
        out,
        "============================================================"
    );
    let _ = writeln!(out, "Power & Thermals:");

    let w = if has_apu_telemetry(t) { 9 } else { 5 };
    let power_thermal_rows = [
        ("STAPM", t.stapm_value_w, t.stapm_limit_w, "W", false),
        ("PPT", t.ppt_value_w, t.ppt_limit_w, "W", true),
        ("Slow PPT", t.slow_value_w, t.slow_limit_w, "W", false),
        (
            "APU Slow",
            t.apu_slow_value_w,
            t.apu_slow_limit_w,
            "W",
            false,
        ),
        ("TDC", t.tdc_value_a, t.tdc_limit_a, "A", true),
        ("SoC TDC", t.tdc_soc_value_a, t.tdc_soc_limit_a, "A", false),
        ("EDC", t.edc_value_a, t.edc_limit_a, "A", true),
        ("SoC EDC", t.edc_soc_value_a, t.edc_soc_limit_a, "A", false),
        ("Tctl", t.tctl_value_c, t.tctl_limit_c, "°C", true),
        (
            "APU Skin",
            t.apu_skin_temp_value_c,
            t.apu_skin_temp_limit_c,
            "°C",
            false,
        ),
        (
            "dGPU Skin",
            t.dgpu_skin_temp_value_c,
            t.dgpu_skin_temp_limit_c,
            "°C",
            false,
        ),
    ];
    for (label, val, lim, unit, always_show) in power_thermal_rows {
        if always_show || val.is_some() || lim.is_some() {
            let _ = writeln!(out, "  {label:<w$} : {}", fmt_ratio(val, lim, unit));
        }
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "Fabric & Memory Clocks:");
    for (label, val) in [
        ("FCLK", t.fclk_mhz),
        ("UCLK", t.uclk_mhz),
        ("MCLK", t.mclk_mhz),
    ] {
        let _ = writeln!(out, "  {label:<5} : {}", fmt_opt_f32(val, 1, "MHz"));
    }

    let _ = writeln!(out);
    let _ = writeln!(out, "Voltages:");
    for (label, val) in [
        ("VDDCR_SOC", t.vddcr_soc_v),
        ("VDDCR_MISC", t.vddcr_misc_v),
        ("CLDO_VDDP", t.cldo_vddp_v),
        ("CLDO_VDDG_IOD", t.cldo_vddg_iod_v),
        ("CLDO_VDDG_CCD", t.cldo_vddg_ccd_v),
    ] {
        let _ = writeln!(out, "  {label:<13} : {}", fmt_opt_f32(val, 4, "V"));
    }
    out
}

pub fn print_telemetry(t: &TelemetrySnapshot) {
    print!("{}", format_telemetry(t));
}

pub fn print_status(status: &StatusOutput) {
    println!("zensmu Status — {}", status.codename_name);
    println!("============================================================");
    println!(
        "Boot Guard : {}",
        if status.guard_tripped {
            "TRIPPED (auto-apply blocked; run `sudo zensmu profile clear-guard`)"
        } else {
            "clean"
        }
    );

    if !status.notes.is_empty() {
        for note in &status.notes {
            println!("Note       : {note}");
        }
    }

    println!();
    if let Some(ref telem) = status.telemetry {
        print_telemetry(telem);
    } else if let Some(ref err) = status.telemetry_error {
        println!("Telemetry  : unavailable ({err})");
    }

    println!();
    if let Some(ref co) = status.co {
        print_co_table(&CoGetOutput {
            codename: status.codename,
            cores: co.cores.clone(),
            igpu: co.igpu,
        });
    } else if let Some(ref err) = status.co_error {
        println!("Curve Optimizer : unavailable ({err})");
    }

    if let Some(ref cs) = status.curve_shaper {
        println!();
        print_shaper_matrix(cs, status.codename);
    } else if let Some(ref err) = status.curve_shaper_error {
        println!();
        if err.contains("UNKNOWN_CMD (0xFE)") || err.contains("CMD_REJECTED_PREREQ (0xFD)") {
            let code = if err.contains("0xFE") { "0xFE" } else { "0xFD" };
            println!("Curve Shaper    : not exposed by BIOS / SMU firmware ({code})");
        } else {
            println!("Curve Shaper    : unavailable ({err})");
        }
    }

    if let Some(ref pbo) = status.pbo {
        println!();
        let active_limits = status
            .telemetry
            .as_ref()
            .map(ActivePboLimits::from_telemetry)
            .unwrap_or_default();
        print_pbo_status(&PboGetOutput {
            codename: status.codename,
            active_limits,
            status: pbo.clone(),
        });
    } else if let Some(ref err) = status.pbo_error {
        println!();
        println!("PBO Status      : unavailable ({err})");
    }
}

pub fn print_planned_ops(ops: &[PlannedOp], header: &str) {
    println!("{header}");
    if ops.is_empty() {
        println!("  (no SMU operations)");
        return;
    }
    for (i, op) in ops.iter().enumerate() {
        let transition = match &op.current {
            Some(cur) => format!(" [{cur} -> {}]", op.target),
            None => format!(" [-> {}]", op.target),
        };
        println!(
            "  {:>2}. {} ({} 0x{:02X}, arg0=0x{:08X}){}",
            i + 1,
            op.description,
            op.mailbox.as_str(),
            op.opcode,
            op.args[0],
            transition
        );
    }
}

pub fn print_mutation(out: &MutationOutput) {
    let header = if out.dry_run {
        format!(
            "[DRY-RUN] {} ({} planned operation(s)):",
            out.action,
            out.operations.len()
        )
    } else if out.rolled_back {
        format!(
            "[ROLLED BACK] {} ({} operation(s) reverted):",
            out.action,
            out.operations.len()
        )
    } else {
        format!(
            "[APPLIED] {} ({} operation(s)):",
            out.action,
            out.operations.len()
        )
    };
    print_planned_ops(&out.operations, &header);
    if let Some(ref note) = out.note {
        println!("Note: {note}");
    }
}

pub fn print_profile_diff(out: &ProfileDiffOutput) {
    println!("Profile Diff: {}", out.path);
    println!("------------------------------------------------------------");
    if out.diffs.is_empty() {
        println!("No differences (current hardware state matches profile).");
        return;
    }
    println!("{:<32} {:>12} {:>12}", "FIELD", "CURRENT", "DESIRED");
    for d in &out.diffs {
        println!("{:<32} {:>12} {:>12}", d.field, d.current, d.desired);
    }
}

pub fn print_profile_list(out: &ProfileListOutput) {
    println!("Profiles in {}:", out.dir);
    if out.profiles.is_empty() {
        println!("  (no .toml profiles found)");
        return;
    }
    println!("{:<28} {:<24} {:<10}", "NAME", "TARGET", "STATUS");
    for p in &out.profiles {
        let target = p
            .codename
            .map(|c| c.slug().to_string())
            .unwrap_or_else(|| "unknown".into());
        let status = if p.valid {
            "ok".to_string()
        } else {
            format!("invalid ({})", p.error.as_deref().unwrap_or("parse error"))
        };
        println!("{:<28} {:<24} {:<10}", p.name, target, status);
    }
}

pub fn print_persist_status(status: &PersistStatus) {
    println!("Boot & Suspend Persistence");
    println!("==========================");

    let profile_summary = if !status.active_profile_exists {
        format!("missing ({})", status.active_profile_path)
    } else {
        let cn = status
            .active_profile_codename
            .map(|c| c.display_name().to_string())
            .unwrap_or_else(|| "unknown".into());
        match status.active_profile_valid {
            Some(true) => format!("valid ({}, target: {cn})", status.active_profile_path),
            Some(false) => format!(
                "INVALID ({}, {})",
                status.active_profile_path,
                status
                    .active_profile_error
                    .as_deref()
                    .unwrap_or("validation error")
            ),
            None => format!("present ({}, target: {cn})", status.active_profile_path),
        }
    };
    println!("Active Profile       : {profile_summary}");
    println!(
        "Module Autoload      : {} ({})",
        if status.modules_load_configured {
            "configured (ryzen_smu)"
        } else {
            "not configured"
        },
        DEFAULT_MODULES_LOAD_PATH
    );
    let svc_summary = if !status.service_installed {
        format!("not installed ({})", status.service_name)
    } else if status.service_enabled {
        format!("enabled ({})", status.service_name)
    } else {
        format!("{} ({})", status.service_state, status.service_name)
    };
    println!("Boot Service         : {svc_summary}");
    println!(
        "Suspend Resume Hook  : {} ({})",
        if status.resume_hook_installed {
            "installed"
        } else {
            "not installed"
        },
        DEFAULT_RESUME_HOOK_PATH
    );
    println!(
        "Boot-Loop Guard      : {}",
        if status.boot_guard_tripped {
            "TRIPPED (auto-apply blocked; run `sudo zensmu profile clear-guard`)"
        } else {
            "clean"
        }
    );
}

pub fn print_persist_action(out: &PersistActionOutput) {
    let tag = if out.dry_run { "[DRY-RUN]" } else { "[OK]" };
    println!("{tag} {}", out.action);
    if let Some(ref note) = out.note {
        println!("Note                 : {note}");
    }
    println!();
    print_persist_status(&out.status);
}

pub fn print_record_service_status(status: &RecordServiceStatus) {
    println!("Telemetry Recording Service (user)");
    println!("==================================");
    println!("Service              : {}", status.service_name);
    println!(
        "Installed            : {}",
        if status.installed { "yes" } else { "no" }
    );
    println!(
        "Enabled              : {}",
        if status.enabled { "yes" } else { "no" }
    );
    println!(
        "Active               : {}",
        if status.active { "yes" } else { "no" }
    );
    println!("State                : {}", status.state);
    println!("Default Output       : {}", status.default_output_path);
}

pub fn print_record_service_action(out: &RecordServiceActionOutput) {
    let tag = if out.dry_run { "[DRY-RUN]" } else { "[OK]" };
    println!("{tag} {}", out.action);
    if let Some(ref note) = out.note {
        println!("Note                 : {note}");
    }
    println!();
    print_record_service_status(&out.status);
}

pub fn print_record_export_pts(out: &RecordExportPtsOutput) {
    println!(
        "Exported {} telemetry sample(s) to Phoronix Test Suite XML:",
        out.samples
    );
    println!("  Input  : {}", out.input);
    println!("  Output : {}", out.output);
    if let Some(ref pts_name) = out.pts_name {
        println!("  View   : phoronix-test-suite show-result {pts_name}");
    }
}

pub fn print_record_stats(stats: &RecordStats, pts_name: Option<&str>) {
    println!(
        "Recorded {} sample(s) ({} bytes, {} rotation(s), format: {}) -> {}",
        stats.samples_written,
        stats.bytes_written,
        stats.rotations,
        stats.format.as_str(),
        stats.path.display()
    );
    if let Some(name) = pts_name {
        println!("View in Phoronix Test Suite: phoronix-test-suite show-result {name}");
    }
}

fn fmt_opt_f32(v: Option<f32>, prec: usize, unit: &str) -> String {
    match v {
        Some(x) => format!("{x:.prec$} {unit}"),
        None => "n/a".into(),
    }
}

fn fmt_opt_u32(v: Option<u32>, unit: &str) -> String {
    match v {
        Some(x) => format!("{x} {unit}"),
        None => "n/a".into(),
    }
}

fn fmt_opt_bool(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "yes",
        Some(false) => "no",
        None => "n/a",
    }
}

fn fmt_ratio(val: Option<f32>, limit: Option<f32>, unit: &str) -> String {
    match (val, limit) {
        (Some(v), Some(l)) if l > 0.0 => {
            let pct = (v / l) * 100.0;
            format!("{v:>6.1} / {l:<6.1} {unit}  ({pct:5.1}%)")
        }
        (Some(v), Some(l)) => format!("{v:>6.1} / {l:<6.1} {unit}"),
        (Some(v), None) => format!("{v:>6.1} {unit}"),
        (None, Some(l)) => format!("   n/a / {l:<6.1} {unit}"),
        (None, None) => "n/a".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desktop_snapshot() -> TelemetrySnapshot {
        TelemetrySnapshot {
            codename: Codename::GraniteRidge,
            table_version: 0x0062_0105,
            table_size: 0x724,
            ppt_limit_w: Some(88.0),
            ppt_value_w: Some(42.5),
            tdc_limit_a: Some(75.0),
            tdc_value_a: Some(21.0),
            edc_limit_a: Some(150.0),
            edc_value_a: Some(38.0),
            tctl_limit_c: Some(95.0),
            tctl_value_c: Some(54.2),
            stapm_limit_w: None,
            stapm_value_w: None,
            slow_limit_w: None,
            slow_value_w: None,
            apu_slow_limit_w: None,
            apu_slow_value_w: None,
            tdc_soc_limit_a: None,
            tdc_soc_value_a: None,
            edc_soc_limit_a: None,
            edc_soc_value_a: None,
            apu_skin_temp_limit_c: None,
            apu_skin_temp_value_c: None,
            dgpu_skin_temp_limit_c: None,
            dgpu_skin_temp_value_c: None,
            fclk_mhz: Some(2000.0),
            uclk_mhz: Some(3000.0),
            mclk_mhz: Some(3000.0),
            vddcr_soc_v: Some(1.24),
            vddcr_misc_v: Some(1.10),
            cldo_vddp_v: Some(1.05),
            cldo_vddg_iod_v: Some(0.95),
            cldo_vddg_ccd_v: Some(0.95),
        }
    }

    #[test]
    fn telemetry_format_keeps_desktop_uncluttered() {
        let out = format_telemetry(&desktop_snapshot());
        assert!(out.contains("  PPT   :"));
        assert!(out.contains("  TDC   :"));
        assert!(out.contains("  EDC   :"));
        assert!(out.contains("  Tctl  :"));
        for apu_label in [
            "STAPM",
            "Slow PPT",
            "APU Slow",
            "SoC TDC",
            "SoC EDC",
            "APU Skin",
            "dGPU Skin",
        ] {
            assert!(
                !out.contains(apu_label),
                "desktop output should not contain {apu_label}:\n{out}"
            );
        }
    }

    #[test]
    fn telemetry_format_renders_all_apu_fields_when_present() {
        let mut snap = desktop_snapshot();
        snap.codename = Codename::Lucienne;
        snap.table_version = 0x0037_0005;
        snap.table_size = 0x8C8;
        snap.stapm_limit_w = Some(25.0);
        snap.stapm_value_w = Some(15.0);
        snap.ppt_limit_w = Some(30.0);
        snap.ppt_value_w = Some(18.0);
        snap.slow_limit_w = Some(25.0);
        snap.slow_value_w = Some(16.0);
        snap.apu_slow_limit_w = Some(20.0);
        snap.apu_slow_value_w = Some(12.0);
        snap.tdc_soc_limit_a = Some(20.0);
        snap.tdc_soc_value_a = Some(8.5);
        snap.edc_soc_limit_a = Some(35.0);
        snap.edc_soc_value_a = Some(14.0);
        snap.apu_skin_temp_limit_c = Some(45.0);
        snap.apu_skin_temp_value_c = Some(39.0);
        snap.dgpu_skin_temp_limit_c = Some(55.0);
        snap.dgpu_skin_temp_value_c = Some(42.0);

        let out = format_telemetry(&snap);
        for expected in [
            "STAPM",
            "PPT",
            "Slow PPT",
            "APU Slow",
            "TDC",
            "SoC TDC",
            "EDC",
            "SoC EDC",
            "Tctl",
            "APU Skin",
            "dGPU Skin",
            "15.0 / 25.0   W",
            "16.0 / 25.0   W",
            "12.0 / 20.0   W",
            "8.5 / 20.0   A",
            "14.0 / 35.0   A",
            "39.0 / 45.0   °C",
            "42.0 / 55.0   °C",
        ] {
            assert!(
                out.contains(expected),
                "APU output missing {expected:?}:\n{out}"
            );
        }
    }

    #[test]
    fn error_kind_maps_hardware_clamped() {
        let err = Error::HardwareClamped {
            summary: "pbo.edc_a (req 220 A -> cap 120 A)".into(),
            clamped: vec![],
        };
        assert_eq!(error_kind(&err), "hardware_clamped");
        let payload = ErrorOutput {
            ok: false,
            kind: error_kind(&err),
            error: err.to_string(),
            hint: err.report_hint().map(str::to_string),
        };
        let v = serde_json::to_value(&payload).unwrap();
        assert!(v.get("hint").is_none());

        let pm_err = Error::UnsupportedPmTable {
            codename: "Granite Ridge".into(),
            version: 0x0062_9999,
            size: 0x724,
        };
        let pm_payload = ErrorOutput {
            ok: false,
            kind: error_kind(&pm_err),
            error: pm_err.to_string(),
            hint: pm_err.report_hint().map(str::to_string),
        };
        let pm_v = serde_json::to_value(&pm_payload).unwrap();
        assert!(pm_v
            .get("hint")
            .and_then(|h| h.as_str())
            .is_some_and(|h| h.contains("zensmu report --raw-pm-table --open")));
    }

    #[test]
    fn pbo_format_renders_fused_tdp_and_tjmax_when_present() {
        let mut status = PboStatus::default();
        status.fused.ppt_w = Some(142);
        status.fused.vdd_tdc_a = Some(95);
        status.fused.edc_a = Some(140);
        status.fused.tdp_w = Some(105);
        status.fused.tjmax_c = Some(90);

        let board = SystemBoardInfo {
            board_vendor: Some("Gigabyte Technology Co., Ltd.".into()),
            board_name: Some("B450M DS3H V2".into()),
            bios_version: Some("F66".into()),
            ..Default::default()
        };
        let out = format_pbo_status_with_board(
            &PboGetOutput {
                codename: Codename::Vermeer,
                active_limits: ActivePboLimits::default(),
                status,
            },
            &board,
        );
        assert!(out.contains("  Fused TDP      : 105 W"), "{out}");
        assert!(out.contains("  Fused TjMax    : 90 °C"), "{out}");
        assert!(
            out.contains(
                "Board: Gigabyte Technology Co., Ltd. B450M DS3H V2 (BIOS F66) — BIOS PBO Limits / VRM firmware may cap PPT/TDC/EDC."
            ),
            "{out}"
        );

        // When None, omit Fused TDP and Fused TjMax lines cleanly
        let out_none = format_pbo_status_with_board(
            &PboGetOutput {
                codename: Codename::GraniteRidge,
                active_limits: ActivePboLimits::default(),
                status: PboStatus::default(),
            },
            &SystemBoardInfo::default(),
        );
        assert!(!out_none.contains("Fused TDP"), "{out_none}");
        assert!(!out_none.contains("Fused TjMax"), "{out_none}");
        assert!(
            out_none.contains("Board: BIOS PBO Limits / VRM firmware may cap PPT/TDC/EDC."),
            "{out_none}"
        );
    }

    #[test]
    fn report_output_serializes_expected_json_contract() {
        use zensmu_core::report::{collect_debug_report, ReportOptions};
        use zensmu_core::transport::mock::MockTransport;

        let transport = MockTransport::granite_ridge_9700x();
        let report = collect_debug_report(
            &transport,
            &ReportOptions {
                include_raw_pm_table: false,
                include_mailbox_probes: false,
                user_note: Some("test note".into()),
                no_cpuid_check: true,
            },
        );
        let payload = ReportOutput {
            issue_title: report.suggested_issue_title(),
            issue_labels: report
                .suggested_labels()
                .into_iter()
                .map(str::to_string)
                .collect(),
            issue_url: report.github_issue_url(),
            markdown: report.to_markdown(),
            report,
        };
        let val = serde_json::to_value(&payload).unwrap();
        assert!(val.get("report").is_some());
        assert!(val.get("issue_title").and_then(|v| v.as_str()).is_some());
        assert!(val.get("issue_labels").and_then(|v| v.as_array()).is_some());
        assert!(val
            .get("issue_url")
            .and_then(|v| v.as_str())
            .is_some_and(|u| u.starts_with("https://github.com/brauliobo/zensmu/issues/new")));
        assert!(val
            .get("markdown")
            .and_then(|v| v.as_str())
            .is_some_and(|m| m.contains("test note")));
    }
}
