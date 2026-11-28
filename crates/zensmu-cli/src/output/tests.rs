use super::format::{
    format_bench_report, format_driver_status, format_pbo_status_with_board, format_telemetry,
};
use super::json::error_kind;
use super::types::ErrorOutput;
use super::*;
use zensmu_core::bench::{BenchReport, CoreBenchStatus};
use zensmu_core::driver::DriverStatus;
use zensmu_core::error::Error;
use zensmu_core::ops::pbo::PboStatus;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::report::SystemBoardInfo;
use zensmu_core::types::Codename;

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

#[test]
fn driver_status_format_displays_secure_boot_row_and_hint_when_unloaded() {
    use zensmu_core::driver::{secure_boot_status_from, DistroFamily};

    let status = DriverStatus {
        sysfs_path: "/sys/kernel/ryzen_smu_drv".into(),
        loaded: false,
        drv_version: None,
        module_installed: true,
        module_path: Some("/lib/modules/6.12/ryzen_smu.ko".into()),
        dkms_installed: true,
        dkms_status: Some("ryzen_smu/0.1.7: installed".into()),
        bundled_dkms_available: true,
        zensmu_managed_dkms: true,
        kernel_headers_installed: true,
        autoload_configured: true,
        kernel_release: "6.12.0".into(),
        distro_family: DistroFamily::Fedora,
        distro_name: "Fedora Linux".into(),
        summary: "ryzen_smu installed but not loaded".into(),
        recommended_command: "sudo zensmu driver load".into(),
        manual_commands: vec![],
        secure_boot: secure_boot_status_from(Some(true), Some("integrity".into())),
    };
    let rendered = format_driver_status(&status);
    assert!(rendered.contains("Secure Boot / Lockdown :"), "{rendered}");
    assert!(rendered.contains("Secure Boot enabled"), "{rendered}");
    assert!(rendered.contains("lockdown=integrity"), "{rendered}");
    assert!(rendered.contains("Secure Boot Hint"), "{rendered}");
    assert!(rendered.contains("mokutil"), "{rendered}");
}

#[test]
fn bench_report_format_renders_passed_and_failed_runs() {
    use zensmu_core::bench::{BenchWorkload, CoreBenchResult};

    let failed_report = BenchReport {
        codename: "Granite Ridge (Zen 5)".into(),
        workload: BenchWorkload::Mixed,
        passes_completed: 0,
        total_duration_secs: 12.4,
        passed: false,
        cancelled: false,
        failed_cores: vec![3],
        dmesg_readable: true,
        results: vec![CoreBenchResult {
            pass: 1,
            core_index: 3,
            logical_cpu: 6,
            status: CoreBenchStatus::FailedMce,
            duration_secs: 12.4,
            iterations: 512,
            peak_freq_mhz: Some(5525.0),
            peak_temp_c: Some(68.5),
            peak_power_w: Some(46.2),
            min_voltage_v: Some(1.125),
            max_voltage_v: Some(1.255),
            mce_delta_corrected: 1,
            mce_delta_uncorrected: 0,
            new_mce_lines: vec!["mce: [Hardware Error]: CPU 6".into()],
            error: Some("hardware MCE detected on core 3".into()),
        }],
    };

    let rendered = format_bench_report(&failed_report);
    assert!(
        rendered.contains("FAILED (failed core(s): 3)"),
        "{rendered}"
    );
    assert!(rendered.contains("failed-mce"), "{rendered}");
    assert!(rendered.contains("5525 MHz"), "{rendered}");
    assert!(rendered.contains("1.125/1.255V"), "{rendered}");
    assert!(rendered.contains("Failure Details:"), "{rendered}");
    assert!(
        rendered.contains("MCE: mce: [Hardware Error]: CPU 6"),
        "{rendered}"
    );
    assert!(
        rendered.contains("Reduce negative Curve Optimizer offset"),
        "{rendered}"
    );
}
