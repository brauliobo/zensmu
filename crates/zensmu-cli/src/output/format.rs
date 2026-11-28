//! Pure string formatters for human-readable CLI output.

use super::types::PboGetOutput;
use std::sync::OnceLock;
use zensmu_core::bench::{BenchReport, CoreBenchStatus};
use zensmu_core::driver::DriverStatus;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::report::SystemBoardInfo;

pub(super) fn format_signed(v: i32) -> String {
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

pub(super) fn format_pbo_status_with_board(out: &PboGetOutput, board: &SystemBoardInfo) -> String {
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
        ranked.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));
        let _ = writeln!(s, "  {:<6} {:<8}", "RANK", "CORE");
        for (core_idx, rank) in &ranked {
            let _ = writeln!(s, "  {:<6} {:<8}", rank, core_idx);
        }
    }
    s
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

pub fn format_driver_status(status: &DriverStatus) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "ryzen_smu Kernel Driver");
    let _ = writeln!(out, "=======================");
    let _ = writeln!(out, "Summary                : {}", status.summary);
    let _ = writeln!(
        out,
        "Loaded                 : {}",
        if status.loaded { "yes" } else { "no" }
    );
    if let Some(ref v) = status.drv_version {
        let _ = writeln!(out, "Driver Version         : {v}");
    }
    let _ = writeln!(out, "Sysfs Path             : {}", status.sysfs_path);
    let _ = writeln!(
        out,
        "Module Installed       : {}{}",
        if status.module_installed { "yes" } else { "no" },
        status
            .module_path
            .as_deref()
            .map(|p| format!(" ({p})"))
            .unwrap_or_default()
    );
    let _ = writeln!(
        out,
        "DKMS Registered        : {}{}",
        if status.dkms_installed { "yes" } else { "no" },
        status
            .dkms_status
            .as_deref()
            .map(|s| format!(" ({})", s.replace('\n', "; ")))
            .unwrap_or_default()
    );
    let _ = writeln!(
        out,
        "Kernel Headers         : {}",
        if status.kernel_headers_installed {
            "installed"
        } else {
            "missing (run `sudo zensmu driver install` to install headers & build DKMS)"
        }
    );
    let _ = writeln!(
        out,
        "Bundled DKMS Source    : {}",
        if status.bundled_dkms_available {
            "available (/usr/share/zensmu/ryzen_smu-dkms)"
        } else {
            "not bundled (will clone upstream amkillam/ryzen_smu)"
        }
    );
    let _ = writeln!(
        out,
        "Boot Autoload          : {}",
        if status.autoload_configured {
            "configured (/etc/modules-load.d/ryzen_smu.conf)"
        } else {
            "not configured"
        }
    );
    let sb = &status.secure_boot;
    let sb_state = match sb.secure_boot_enabled {
        Some(true) => "enabled",
        Some(false) => "disabled",
        None => "unknown",
    };
    let lockdown = sb.lockdown_mode.as_deref().unwrap_or("none");
    let sb_note = if sb.blocks_unsigned_modules {
        if status.loaded {
            " (blocks unsigned modules; ryzen_smu loaded)"
        } else {
            " (blocks unsigned modules)"
        }
    } else {
        ""
    };
    let _ = writeln!(
        out,
        "Secure Boot / Lockdown : Secure Boot {sb_state}, lockdown={lockdown}{sb_note}"
    );
    if sb.blocks_unsigned_modules && !status.loaded {
        if let Some(ref hint) = sb.hint {
            let _ = writeln!(out, "Secure Boot Hint       : {hint}");
        }
    }
    let _ = writeln!(out, "Kernel                 : {}", status.kernel_release);
    let _ = writeln!(
        out,
        "Distribution           : {} ({})",
        status.distro_name, status.distro_family
    );
    if !status.loaded {
        let _ = writeln!(out);
        let _ = writeln!(out, "Recommended next step:");
        let _ = writeln!(out, "  {}", status.recommended_command);
        if !status.manual_commands.is_empty() {
            let _ = writeln!(out);
            let _ = writeln!(out, "Manual install options:");
            for cmd in &status.manual_commands {
                let _ = writeln!(out, "  {cmd}");
            }
        }
    }
    out
}

pub fn format_bench_report(report: &BenchReport) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    let _ = writeln!(out, "Per-Core CO Stability Test — {}", report.codename);
    let _ = writeln!(
        out,
        "================================================================================"
    );
    let _ = writeln!(out, "Workload         : {}", report.workload);
    let _ = writeln!(out, "Passes Completed : {}", report.passes_completed);
    let _ = writeln!(out, "Total Duration   : {:.1}s", report.total_duration_secs);
    let _ = writeln!(
        out,
        "Kernel MCE Log   : {}",
        if report.dmesg_readable {
            "readable"
        } else {
            "unreadable (run with sudo or grant dmesg/journalctl access for full MCE log detection)"
        }
    );

    let verdict = if report.passed {
        "PASSED".to_string()
    } else if !report.failed_cores.is_empty() {
        let cores = format_cpu_list(&report.failed_cores);
        if report.cancelled {
            format!("FAILED & CANCELLED (failed core(s): {cores})")
        } else {
            format!("FAILED (failed core(s): {cores})")
        }
    } else if report.cancelled {
        "CANCELLED".to_string()
    } else {
        "INCOMPLETE".to_string()
    };
    let _ = writeln!(out, "Result           : {verdict}");

    if !report.results.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "{:<4} {:<4} {:<4} {:<14} {:>7} {:>8} {:>10} {:>9} {:>9} {:>15} {:>9}",
            "PASS",
            "CORE",
            "CPU",
            "STATUS",
            "TIME",
            "ITERS",
            "PEAK FREQ",
            "PEAK TEMP",
            "PEAK PWR",
            "VCORE MIN/MAX",
            "MCE CE/UE"
        );
        let _ = writeln!(
            out,
            "{:-<4} {:-<4} {:-<4} {:-<14} {:->7} {:->8} {:->10} {:->9} {:->9} {:->15} {:->9}",
            "", "", "", "", "", "", "", "", "", "", ""
        );
        for r in &report.results {
            let freq = r
                .peak_freq_mhz
                .map(|m| format!("{m:.0} MHz"))
                .unwrap_or_else(|| "n/a".into());
            let temp = r
                .peak_temp_c
                .map(|t| format!("{t:.1} °C"))
                .unwrap_or_else(|| "n/a".into());
            let pwr = r
                .peak_power_w
                .map(|w| format!("{w:.1} W"))
                .unwrap_or_else(|| "n/a".into());
            let vcore = match (r.min_voltage_v, r.max_voltage_v) {
                (Some(lo), Some(hi)) => format!("{lo:.3}/{hi:.3}V"),
                (Some(v), None) | (None, Some(v)) => format!("{v:.3} V"),
                (None, None) => "n/a".into(),
            };
            let mce = format!("{}/{}", r.mce_delta_corrected, r.mce_delta_uncorrected);
            let _ = writeln!(
                out,
                "{:<4} {:<4} {:<4} {:<14} {:>6.1}s {:>8} {:>10} {:>9} {:>9} {:>15} {:>9}",
                r.pass,
                r.core_index,
                r.logical_cpu,
                r.status.as_str(),
                r.duration_secs,
                r.iterations,
                freq,
                temp,
                pwr,
                vcore,
                mce
            );
        }
    }

    let failures: Vec<_> = report
        .results
        .iter()
        .filter(|r| {
            matches!(
                r.status,
                CoreBenchStatus::FailedMce | CoreBenchStatus::FailedCompute
            ) || r.error.is_some()
                || !r.new_mce_lines.is_empty()
        })
        .collect();

    if !failures.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(out, "Failure Details:");
        for r in failures {
            let msg = r.error.as_deref().unwrap_or(r.status.as_str());
            let _ = writeln!(out, "  - Pass {} Core {}: {msg}", r.pass, r.core_index);
            for line in &r.new_mce_lines {
                let _ = writeln!(out, "      MCE: {line}");
            }
        }
    }

    if !report.failed_cores.is_empty() {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Tip: Reduce negative Curve Optimizer offset (e.g. +5 toward 0) on failed core(s): {}",
            format_cpu_list(&report.failed_cores)
        );
    }

    out
}

pub(super) fn fmt_opt_f32(v: Option<f32>, prec: usize, unit: &str) -> String {
    match v {
        Some(x) => format!("{x:.prec$} {unit}"),
        None => "n/a".into(),
    }
}

pub(super) fn fmt_opt_u32(v: Option<u32>, unit: &str) -> String {
    match v {
        Some(x) => format!("{x} {unit}"),
        None => "n/a".into(),
    }
}

pub(super) fn fmt_opt_bool(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "yes",
        Some(false) => "no",
        None => "n/a",
    }
}

pub(super) fn fmt_ratio(val: Option<f32>, limit: Option<f32>, unit: &str) -> String {
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
