//! Human-readable terminal rendering and structured JSON serialization for all CLI subcommands.

use crate::doctor::{CheckStatus, DoctorReport};
use serde::Serialize;
use zensmu_core::config::profile::ProfileDiffEntry;
use zensmu_core::error::Error;
use zensmu_core::hw::identity::{CpuidInfo, HardwareContext, driver_has_false_ok_bug};
use zensmu_core::hw::profiles::CoBounds;
use zensmu_core::hw::topology::CpuTopology;
use zensmu_core::ops::co::{CoSnapshot, CoreCoMargin};
use zensmu_core::ops::pbo::PboStatus;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::safety::persist::{DEFAULT_MODULES_LOAD_PATH, DEFAULT_RESUME_HOOK_PATH, PersistStatus};
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
pub struct ErrorOutput {
    pub ok: bool,
    pub kind: &'static str,
    pub error: String,
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
        };
        if let Ok(s) = serde_json::to_string(&payload) {
            eprintln!("{s}");
        } else {
            eprintln!("error: {err}");
        }
    } else {
        eprintln!("error: {err}");
    }
}

// ============================================================================
// Human-Readable Formatters
// ============================================================================

pub fn format_signed_i16(v: i16) -> String {
    if v > 0 {
        format!("+{v}")
    } else {
        v.to_string()
    }
}

pub fn format_signed_i8(v: i8) -> String {
    if v > 0 {
        format!("+{v}")
    } else {
        v.to_string()
    }
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
        println!("[{:4}] {}: {}", check.status.tag(), check.name, check.message);
        if let Some(ref rem) = check.remediation {
            println!("       -> {rem}");
        }
    }

    let pass = report
        .checks
        .iter()
        .filter(|c| c.status == CheckStatus::Pass)
        .count();
    let warn = report
        .checks
        .iter()
        .filter(|c| c.status == CheckStatus::Warn)
        .count();
    let fail = report
        .checks
        .iter()
        .filter(|c| c.status == CheckStatus::Fail)
        .count();
    let skip = report
        .checks
        .iter()
        .filter(|c| c.status == CheckStatus::Skip)
        .count();

    println!();
    println!(
        "Summary: {} ({pass} passed, {warn} warnings, {fail} failed, {skip} skipped)",
        if report.healthy { "HEALTHY" } else { "ISSUES DETECTED" }
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

pub fn print_pbo_status(out: &PboGetOutput) {
    println!("Precision Boost Overdrive ({})", out.codename.display_name());
    println!("==========================================");

    println!("Active Limits (PM Table):");
    println!(
        "  PPT   : {}",
        fmt_opt_f32(out.active_limits.ppt_w, 1, "W")
    );
    println!(
        "  TDC   : {}",
        fmt_opt_f32(out.active_limits.tdc_a, 1, "A")
    );
    println!(
        "  EDC   : {}",
        fmt_opt_f32(out.active_limits.edc_a, 1, "A")
    );
    println!(
        "  Tctl  : {}",
        fmt_opt_f32(out.active_limits.tctl_c, 1, "°C")
    );

    println!();
    println!("Boost & Scalar:");
    println!(
        "  PBO Scalar : {}",
        out.status
            .scalar
            .map(|s| format!("{s:.2}x"))
            .unwrap_or_else(|| "n/a".into())
    );
    println!(
        "  Fmax Limit : {}",
        out.status
            .fmax_mhz
            .map(|m| format!("{m} MHz"))
            .unwrap_or_else(|| "n/a".into())
    );

    println!();
    println!("Fused Factory Limits:");
    let f = &out.status.fused;
    println!("  Fused PPT      : {}", fmt_opt_u32(f.ppt_w, "W"));
    println!("  Fused VDD TDC  : {}", fmt_opt_u32(f.vdd_tdc_a, "A"));
    println!("  Fused EDC      : {}", fmt_opt_u32(f.edc_a, "A"));
    if f.soc_tdc_a.is_some() {
        println!("  Fused SoC TDC  : {}", fmt_opt_u32(f.soc_tdc_a, "A"));
    }
    if f.slow_w.is_some() || f.fast_w.is_some() || f.apu_slow_w.is_some() {
        println!("  Fused Slow     : {}", fmt_opt_u32(f.slow_w, "W"));
        println!("  Fused Fast     : {}", fmt_opt_u32(f.fast_w, "W"));
        println!("  Fused APU Slow : {}", fmt_opt_u32(f.apu_slow_w, "W"));
    }

    println!();
    println!("Platform Flags:");
    println!(
        "  Overclockable  : {}",
        fmt_opt_bool(out.status.flags.overclockable)
    );
    println!(
        "  EXPO Active    : {}",
        fmt_opt_bool(out.status.flags.expo_active)
    );
    println!(
        "  LN2 Mode       : {}",
        fmt_opt_bool(out.status.flags.ln2_mode)
    );

    if !out.status.core_perf_scores.is_empty() {
        println!();
        println!("CPPC Core Performance Ranking:");
        let mut ranked = out.status.core_perf_scores.clone();
        ranked.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        println!("  {:<6} {:<8} {:>8}", "RANK", "CORE", "SCORE");
        for (rank, (core_idx, score)) in ranked.iter().enumerate() {
            println!("  {:<6} {:<8} {:>8}", rank + 1, core_idx, score);
        }
    }
}

pub fn print_telemetry(t: &TelemetrySnapshot) {
    println!(
        "Telemetry — {} (PM Table 0x{:08X}, {} bytes)",
        t.codename.display_name(),
        t.table_version,
        t.table_size
    );
    println!("============================================================");
    println!("Power & Thermals:");
    println!(
        "  PPT   : {}",
        fmt_ratio(t.ppt_value_w, t.ppt_limit_w, "W")
    );
    println!(
        "  TDC   : {}",
        fmt_ratio(t.tdc_value_a, t.tdc_limit_a, "A")
    );
    println!(
        "  EDC   : {}",
        fmt_ratio(t.edc_value_a, t.edc_limit_a, "A")
    );
    println!(
        "  Tctl  : {}",
        fmt_ratio(t.tctl_value_c, t.tctl_limit_c, "°C")
    );

    println!();
    println!("Fabric & Memory Clocks:");
    println!("  FCLK  : {}", fmt_opt_f32(t.fclk_mhz, 1, "MHz"));
    println!("  UCLK  : {}", fmt_opt_f32(t.uclk_mhz, 1, "MHz"));
    println!("  MCLK  : {}", fmt_opt_f32(t.mclk_mhz, 1, "MHz"));

    println!();
    println!("Voltages:");
    println!("  VDDCR_SOC     : {}", fmt_opt_f32(t.vddcr_soc_v, 4, "V"));
    println!("  VDDCR_MISC    : {}", fmt_opt_f32(t.vddcr_misc_v, 4, "V"));
    println!("  CLDO_VDDP     : {}", fmt_opt_f32(t.cldo_vddp_v, 4, "V"));
    println!(
        "  CLDO_VDDG_IOD : {}",
        fmt_opt_f32(t.cldo_vddg_iod_v, 4, "V")
    );
    println!(
        "  CLDO_VDDG_CCD : {}",
        fmt_opt_f32(t.cldo_vddg_ccd_v, 4, "V")
    );
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
            println!(
                "Curve Shaper    : not exposed by BIOS / SMU firmware ({code})"
            );
        } else {
            println!("Curve Shaper    : unavailable ({err})");
        }
    }

    if let Some(ref pbo) = status.pbo {
        println!();
        let active_limits = status
            .telemetry
            .as_ref()
            .map(|t| ActivePboLimits {
                ppt_w: t.ppt_limit_w,
                tdc_a: t.tdc_limit_a,
                edc_a: t.edc_limit_a,
                tctl_c: t.tctl_limit_c,
            })
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
        format!("[DRY-RUN] {} ({} planned operation(s)):", out.action, out.operations.len())
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
