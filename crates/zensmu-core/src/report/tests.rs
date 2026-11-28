use super::collect::collect_debug_report_inner;
use super::hwmon::scan_hwmon_dir;
use super::system::parse_os_release_pretty_name;
use super::url::MAX_ISSUE_URL_BYTES;
use super::*;
use crate::error::{Error, Result};
use crate::transport::mock::MockTransport;
use crate::transport::{DriverSysfsInfo, MailboxResponse, SmuTransport};
use crate::types::{Mailbox, SmuArgs};
use std::fs;

struct FailingTransport;

impl SmuTransport for FailingTransport {
    fn read_info(&self) -> Result<DriverSysfsInfo> {
        Err(Error::DriverNotLoaded {
            path: "/sys/kernel/ryzen_smu_drv".into(),
        })
    }

    fn read_pm_table(&self) -> Result<Vec<u8>> {
        Err(Error::DriverNotLoaded {
            path: "/sys/kernel/ryzen_smu_drv/pm_table".into(),
        })
    }

    fn send_command(
        &self,
        _mailbox: Mailbox,
        _opcode: u32,
        _args: SmuArgs,
        _verify_rsp_smn_addr: Option<u32>,
    ) -> Result<MailboxResponse> {
        Err(Error::DriverNotLoaded {
            path: "/sys/kernel/ryzen_smu_drv".into(),
        })
    }

    fn read_smn(&self, _addr: u32) -> Result<u32> {
        Err(Error::DriverNotLoaded {
            path: "/sys/kernel/ryzen_smu_drv/smn".into(),
        })
    }
}

fn mock_cpuid_9700x() -> CpuidInfo {
    CpuidInfo {
        vendor: "AuthenticAMD".into(),
        brand: "AMD Ryzen 7 9700X 8-Core Processor".into(),
        family: 0x1A,
        model: 0x44,
        stepping: 0,
    }
}

fn mock_system_info() -> SystemBoardInfo {
    SystemBoardInfo {
        kernel_release: Some("6.12.10-arch1-1".into()),
        os_pretty_name: Some("Arch Linux".into()),
        board_vendor: Some("ASRock".into()),
        board_name: Some("B650M-HDV/M.2".into()),
        bios_version: Some("3.10".into()),
        bios_date: Some("10/24/2024".into()),
    }
}

#[test]
fn collect_report_with_mock_granite_ridge_captures_all_probes_and_markdown() {
    let transport = MockTransport::granite_ridge_9700x();
    let opts = ReportOptions {
        include_raw_pm_table: true,
        include_mailbox_probes: true,
        user_note: Some("Verifying full Granite Ridge diagnostic output.".into()),
        no_cpuid_check: true,
    };

    // Deterministic full-probe verification independent of CI host core count.
    let report = collect_debug_report_inner(
        &transport,
        &opts,
        mock_system_info(),
        Some(mock_cpuid_9700x()),
        Some(CpuTopology::synthetic(1, 8, 2)),
        vec![HwmonSensorReading {
            chip: "k10temp".into(),
            label: "Tctl".into(),
            value: "54.2 °C".into(),
        }],
        Some(crate::driver::secure_boot_status_from(
            Some(false),
            Some("none".into()),
        )),
    );

    assert_eq!(report.zensmu_version, env!("CARGO_PKG_VERSION"));
    assert_eq!(report.codename, Some(Codename::GraniteRidge));
    assert!(report.capabilities_supported);
    assert!(report.driver.is_some());
    assert!(report.driver_error.is_none());
    assert!(report.telemetry.is_some());
    assert!(report.telemetry_error.is_none());
    assert!(!report.raw_pm_table_floats.is_empty());

    assert_eq!(report.rsmu_ping.as_deref(), Some("OK (0x1338)"));
    assert_eq!(report.mp1_ping.as_deref(), Some("OK (0x1338)"));
    assert!(report.pbo_status.is_some());
    assert!(report.pbo_error.is_none());
    let pbo = report.pbo_status.as_ref().unwrap();
    assert_eq!(pbo.fmax_mhz, Some(5550));
    assert_eq!(pbo.scalar, Some(1.0));
    assert!(report.co_snapshot.is_some());
    assert_eq!(report.co_snapshot.as_ref().unwrap().cores.len(), 8);
    assert!(report.co_error.is_none());
    assert!(report.shaper_state.is_some());
    assert!(report.shaper_error.is_none());

    assert_eq!(
        report.suggested_issue_title(),
        "Hardware report / issue: AMD Ryzen 7 9700X 8-Core Processor (Granite Ridge (Zen 5 Desktop))"
    );
    assert_eq!(report.suggested_labels(), vec!["bug"]);

    let md = report.to_markdown();
    assert!(md.contains("Verifying full Granite Ridge diagnostic output."));
    assert!(md.contains("ASRock B650M-HDV/M.2 (BIOS 3.10, 10/24/2024)"));
    assert!(md.contains("Granite Ridge (Zen 5 Desktop)"));
    assert!(md.contains("0x00620105"));
    assert!(md.contains("OK (0x1338)"));
    assert!(md.contains("hwmon reference sensors (k10temp/zenpower)"));
    assert!(md.contains("Raw PM table non-zero floats"));
    assert!(md.contains("<summary>Topology</summary>"));
    assert!(md.contains("No hostnames, usernames"));

    // Verify JSON serialization roundtrip.
    let json = serde_json::to_string(&report).unwrap();
    let parsed: DebugReport = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.codename, Some(Codename::GraniteRidge));
    assert_eq!(parsed.rsmu_ping.as_deref(), Some("OK (0x1338)"));

    // Also exercise the public entry point directly to confirm it never panics.
    let live_env_report = collect_debug_report(&transport, &opts);
    assert_eq!(live_env_report.codename, Some(Codename::GraniteRidge));
    assert!(live_env_report.telemetry.is_some());
    assert_eq!(live_env_report.rsmu_ping.as_deref(), Some("OK (0x1338)"));
}

#[test]
fn collect_report_handles_unsupported_cpu_and_dumps_raw_pm_floats() {
    // Case 1: Supported CPU codename with unrecognized PM table version ->
    // telemetry_error is Some(...) and raw_pm_table_floats is auto-populated even with include_raw_pm_table=false.
    let mut unknown_pm_transport = MockTransport::granite_ridge_9700x();
    unknown_pm_transport.info.pm_table_version = Some(0xDEAD_BEEF);
    let opts = ReportOptions {
        include_raw_pm_table: false,
        include_mailbox_probes: false,
        user_note: None,
        no_cpuid_check: true,
    };

    let pm_report = collect_debug_report_inner(
        &unknown_pm_transport,
        &opts,
        mock_system_info(),
        Some(mock_cpuid_9700x()),
        Some(CpuTopology::synthetic(1, 8, 2)),
        Vec::new(),
        None,
    );

    assert!(pm_report.capabilities_supported);
    assert!(pm_report.telemetry.is_none());
    assert!(pm_report.telemetry_error.is_some());
    assert!(pm_report
        .telemetry_error
        .as_deref()
        .unwrap()
        .contains("0xDEADBEEF"));
    assert!(
        !pm_report.raw_pm_table_floats.is_empty(),
        "raw PM table floats must auto-populate when telemetry decoding fails"
    );
    assert!(pm_report
        .raw_pm_table_floats
        .iter()
        .any(|e| e.offset_hex == "0x008" && (e.value - 88.0).abs() < 0.01));
    assert!(pm_report
        .raw_pm_table_floats
        .iter()
        .any(|e| e.offset_hex == "0x11C" && (e.value - 2000.0).abs() < 0.01));
    assert!(pm_report
        .suggested_issue_title()
        .starts_with("PM table support:"));
    assert!(pm_report.suggested_issue_title().contains("0xDEADBEEF"));
    assert_eq!(pm_report.suggested_labels(), vec!["hardware-support"]);

    // Case 2: Completely unrecognized CPU codename + unrecognized PM table version ->
    // must not panic, must NOT send any mailbox commands, and must still dump raw PM floats.
    let mut unsupported_cpu_transport = MockTransport::granite_ridge_9700x();
    unsupported_cpu_transport.info.codename_id = 999;
    unsupported_cpu_transport.info.pm_table_version = Some(0xCAFE_0001);
    let probe_opts = ReportOptions {
        include_raw_pm_table: false,
        include_mailbox_probes: true,
        user_note: None,
        no_cpuid_check: true,
    };

    let cpu_report = collect_debug_report_inner(
        &unsupported_cpu_transport,
        &probe_opts,
        mock_system_info(),
        Some(CpuidInfo {
            vendor: "AuthenticAMD".into(),
            brand: "AMD Future Zen 6 Engineering Sample".into(),
            family: 0x1B,
            model: 0x01,
            stepping: 0,
        }),
        Some(CpuTopology::synthetic(1, 8, 2)),
        Vec::new(),
        None,
    );

    assert!(cpu_report.codename.is_none());
    assert!(!cpu_report.capabilities_supported);
    assert!(cpu_report.telemetry.is_none());
    assert!(cpu_report.telemetry_error.is_some());
    assert!(!cpu_report.raw_pm_table_floats.is_empty());
    assert!(
        unsupported_cpu_transport.history().is_empty(),
        "must never send mailbox commands to an unsupported CPU"
    );
    assert_eq!(
        cpu_report.suggested_issue_title(),
        "CPU support request: AMD Future Zen 6 Engineering Sample (family 0x1B, model 0x1)"
    );
    assert_eq!(cpu_report.suggested_labels(), vec!["hardware-support"]);

    let md = cpu_report.to_markdown();
    assert!(md.contains("Raw PM table non-zero floats"));
    assert!(md.contains("0x008 = 88.000"));

    // Case 3: Missing driver transport must never panic and must record driver_error.
    let missing_driver = collect_debug_report(&FailingTransport, &probe_opts);
    assert!(missing_driver.driver.is_none());
    assert!(missing_driver.driver_error.is_some());
    assert!(!missing_driver.capabilities_supported);
    assert!(missing_driver.raw_pm_table_floats.is_empty());
}

#[test]
fn github_issue_url_encodes_properly_and_stays_under_length_limit() {
    // RFC 3986 percent-encoding sanity check
    assert_eq!(percent_encode("AZaz09-._~"), "AZaz09-._~");
    assert_eq!(
        percent_encode("a b&c=d?e#f%g\nh"),
        "a%20b%26c%3Dd%3Fe%23f%25g%0Ah"
    );
    assert_eq!(percent_encode("54.2 °C"), "54.2%20%C2%B0C");

    // Normal compact report URL
    let transport = MockTransport::granite_ridge_9700x();
    let compact = collect_debug_report_inner(
        &transport,
        &ReportOptions {
            include_raw_pm_table: false,
            include_mailbox_probes: true,
            user_note: Some("Quick test with spaces & symbols = 100%".into()),
            no_cpuid_check: true,
        },
        mock_system_info(),
        Some(mock_cpuid_9700x()),
        Some(CpuTopology::synthetic(1, 8, 2)),
        Vec::new(),
        None,
    );

    let url = compact.github_issue_url();
    assert!(url.starts_with(GITHUB_ISSUES_NEW_URL));
    assert!(url.contains("?title="));
    assert!(url.contains("&labels=bug&body="));
    assert!(!url.contains(' '));
    assert!(!url.contains('\n'));
    assert!(url.len() <= MAX_ISSUE_URL_BYTES);

    // Oversized report with 160 raw PM table floats + large user note must cleanly truncate <= 6500 bytes
    let mut oversized = compact;
    oversized.raw_pm_table_floats = (0..160usize)
        .map(|i| PmTableFloatEntry {
            offset_hex: format!("0x{:03X}", i * 4),
            value: 100.0 + i as f32 * 0.25,
        })
        .collect();
    oversized.user_note = Some("Detailed reproduction steps: ".repeat(120));

    let big_md = oversized.to_markdown();
    assert!(
        percent_encode(&big_md).len() > MAX_ISSUE_URL_BYTES,
        "test fixture must actually exceed URL limit before truncation"
    );

    let truncated_url = oversized.github_issue_url();
    assert!(
        truncated_url.len() <= MAX_ISSUE_URL_BYTES,
        "URL length {} exceeded limit {}",
        truncated_url.len(),
        MAX_ISSUE_URL_BYTES
    );
    assert!(!truncated_url.contains(' '));
    assert!(!truncated_url.contains('\n'));
    assert!(truncated_url.contains("Truncated%20for%20URL%20length"));
    assert!(truncated_url.contains("zensmu%20report"));
}

#[test]
fn hwmon_scanner_filters_allowlisted_chips_and_scales_units() {
    let tmp = tempfile::tempdir().unwrap();

    let k10 = tmp.path().join("hwmon0");
    fs::create_dir(&k10).unwrap();
    fs::write(k10.join("name"), "k10temp\n").unwrap();
    fs::write(k10.join("temp1_label"), "Tctl\n").unwrap();
    fs::write(k10.join("temp1_input"), "54250\n").unwrap();

    let zen = tmp.path().join("hwmon1");
    fs::create_dir(&zen).unwrap();
    fs::write(zen.join("name"), "zenpower\n").unwrap();
    fs::write(zen.join("in1_label"), "SVI2_Core\n").unwrap();
    fs::write(zen.join("in1_input"), "1185\n").unwrap();
    fs::write(zen.join("curr1_label"), "SVI2_C_Core\n").unwrap();
    fs::write(zen.join("curr1_input"), "24500\n").unwrap();
    fs::write(zen.join("power1_label"), "SVI2_P_Core\n").unwrap();
    fs::write(zen.join("power1_input"), "29000000\n").unwrap();
    fs::write(zen.join("power1_average"), "28500000\n").unwrap();

    let gpu = tmp.path().join("hwmon2");
    fs::create_dir(&gpu).unwrap();
    fs::write(gpu.join("name"), "amdgpu\n").unwrap();
    fs::write(gpu.join("power1_average"), "12400000\n").unwrap();

    // Non-allowlisted chip must be ignored
    let other = tmp.path().join("hwmon3");
    fs::create_dir(&other).unwrap();
    fs::write(other.join("name"), "nct6775\n").unwrap();
    fs::write(other.join("temp1_input"), "40000\n").unwrap();

    let readings = scan_hwmon_dir(tmp.path());
    assert_eq!(readings.len(), 5);
    assert_eq!(
        readings[0],
        HwmonSensorReading {
            chip: "k10temp".into(),
            label: "Tctl".into(),
            value: "54.2 °C".into(),
        }
    );
    assert_eq!(
        readings[1],
        HwmonSensorReading {
            chip: "zenpower".into(),
            label: "SVI2_P_Core".into(),
            value: "29.0 W".into(),
        }
    );
    assert_eq!(
        readings[2],
        HwmonSensorReading {
            chip: "zenpower".into(),
            label: "SVI2_C_Core".into(),
            value: "24.5 A".into(),
        }
    );
    assert_eq!(
        readings[3],
        HwmonSensorReading {
            chip: "zenpower".into(),
            label: "SVI2_Core".into(),
            value: "1.185 V".into(),
        }
    );
    assert_eq!(
        readings[4],
        HwmonSensorReading {
            chip: "amdgpu".into(),
            label: "power1".into(),
            value: "12.4 W".into(),
        }
    );

    assert_eq!(
        parse_os_release_pretty_name("NAME=Arch\nPRETTY_NAME=\"Arch Linux\"\n"),
        Some("Arch Linux".into())
    );
    assert_eq!(
        parse_os_release_pretty_name("PRETTY_NAME='Ubuntu 24.04 LTS'\n"),
        Some("Ubuntu 24.04 LTS".into())
    );
}

#[test]
fn report_renders_secure_boot_and_deserializes_legacy_json() {
    let sb_blocking = crate::driver::secure_boot_status_from(Some(true), Some("integrity".into()));
    let report = collect_debug_report_inner(
        &FailingTransport,
        &ReportOptions::default(),
        mock_system_info(),
        Some(mock_cpuid_9700x()),
        Some(CpuTopology::synthetic(1, 8, 2)),
        Vec::new(),
        Some(sb_blocking),
    );
    let md = report.to_markdown();
    assert!(md.contains(
        "| Secure Boot / Lockdown | enabled, lockdown: `integrity` (blocks unsigned modules) |"
    ));
    assert!(md.contains("**Secure Boot / lockdown note:**"));
    assert!(md.contains("mokutil --import"));

    // Verify legacy JSON without `secure_boot` still deserializes cleanly
    let mut val = serde_json::to_value(&report).unwrap();
    val.as_object_mut().unwrap().remove("secure_boot");
    let parsed: DebugReport = serde_json::from_value(val).unwrap();
    assert!(parsed.secure_boot.is_none());
}
