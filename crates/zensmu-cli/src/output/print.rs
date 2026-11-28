//! Terminal print routines for CLI subcommands.

use super::format::{
    format_bench_report, format_cpu_list, format_driver_status, format_pbo_status,
    format_signed_i16, format_signed_i8, format_telemetry,
};
use super::types::{
    ActivePboLimits, CoGetOutput, ExporterServiceActionOutput, InfoOutput, MutationOutput,
    PboGetOutput, PersistActionOutput, ProfileDiffOutput, ProfileListOutput, RecordExportPtsOutput,
    RecordServiceActionOutput, ReportOutput, StatusOutput,
};
use crate::doctor::{CheckStatus, DoctorReport};
use zensmu_core::bench::BenchReport;
use zensmu_core::driver::{DriverActionOutput, DriverStatus};
use zensmu_core::metrics::ExporterServiceStatus;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::recorder::{RecordServiceStatus, RecordStats};
use zensmu_core::report::GITHUB_REPO;
use zensmu_core::safety::persist::{
    PersistStatus, DEFAULT_MODULES_LOAD_PATH, DEFAULT_RESUME_HOOK_PATH,
};
use zensmu_core::types::{Codename, CurveShaperState, FreqTier, PlannedOp};

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

pub fn print_driver_status(status: &DriverStatus) {
    print!("{}", format_driver_status(status));
}

pub fn print_driver_action(out: &DriverActionOutput) {
    let tag = if out.dry_run { "[DRY-RUN]" } else { "[OK]" };
    println!("{tag} driver {} (method: {})", out.action, out.method);
    if !out.steps.is_empty() {
        println!("Steps:");
        for step in &out.steps {
            println!("  - {step}");
        }
    }
    for note in &out.notes {
        println!("Note : {note}");
    }
    println!();
    print_driver_status(&out.status);
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

pub fn print_bench_report(report: &BenchReport) {
    print!("{}", format_bench_report(report));
}

pub fn print_exporter_service_status(status: &ExporterServiceStatus) {
    println!("Prometheus Metrics Exporter Service (user)");
    println!("==========================================");
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
    println!("Endpoint             : {}", status.endpoint);
}

pub fn print_exporter_service_action(out: &ExporterServiceActionOutput) {
    let tag = if out.dry_run { "[DRY-RUN]" } else { "[OK]" };
    println!("{tag} {}", out.action);
    if let Some(ref note) = out.note {
        println!("Note                 : {note}");
    }
    println!();
    print_exporter_service_status(&out.status);
}
