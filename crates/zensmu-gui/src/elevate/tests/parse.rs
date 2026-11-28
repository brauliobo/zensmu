use super::super::*;
use zensmu_core::ops::co::{CoSnapshot, CoreCoMargin};
use zensmu_core::ops::pbo::PboStatus;
use zensmu_core::report::DebugReport;
use zensmu_core::safety::persist::PersistStatus;
use zensmu_core::safety::snapshot::SmuStateSnapshot;
use zensmu_core::types::{Codename, CurveShaperState, Mailbox, PlannedOp, ShaperTierMargins};

#[test]
fn parse_planned_ops_supports_array_single_wrapped_and_error() {
    let op = PlannedOp {
        description: "Set all-core CO to -20".into(),
        mailbox: Mailbox::Rsmu,
        opcode: 0x07,
        args: [0xFFEC, 0, 0, 0, 0, 0],
        current: Some("0".into()),
        target: "-20".into(),
    };

    let arr_json = serde_json::to_string(&vec![op.clone()]).unwrap();
    assert_eq!(parse_planned_ops(&arr_json).unwrap(), vec![op.clone()]);

    let single_json = serde_json::to_string(&op).unwrap();
    assert_eq!(parse_planned_ops(&single_json).unwrap(), vec![op.clone()]);

    let wrapped = serde_json::json!({
        "dry_run": true,
        "ops": [op.clone()]
    })
    .to_string();
    assert_eq!(parse_planned_ops(&wrapped).unwrap(), vec![op.clone()]);

    let wrapped2 = serde_json::json!({
        "action": "co set",
        "dry_run": true,
        "applied": false,
        "rolled_back": false,
        "operations": [op.clone()]
    })
    .to_string();
    assert_eq!(parse_planned_ops(&wrapped2).unwrap(), vec![op]);

    let err_json = r#"{"ok": false, "error": "value exceeds soft limit"}"#;
    assert!(parse_planned_ops(err_json)
        .unwrap_err()
        .contains("soft limit"));
}

#[test]
fn parse_state_payloads_decode_co_shaper_pbo_and_status() {
    let co = CoSnapshot {
        cores: vec![CoreCoMargin {
            core_index: 0,
            ccd: 0,
            core_in_ccd: 0,
            linux_cpus: vec![0, 8],
            margin: -20,
        }],
        igpu: Some(-10),
    };
    let co_json = serde_json::to_string(&co).unwrap();
    assert_eq!(parse_co_snapshot(&co_json).unwrap(), co);

    let shaper = CurveShaperState {
        high: ShaperTierMargins {
            low: -10,
            med: -15,
            high: -20,
        },
        ..CurveShaperState::default()
    };
    let shaper_wrapped = serde_json::json!({ "curve_shaper": shaper }).to_string();
    assert_eq!(parse_shaper_state(&shaper_wrapped).unwrap(), shaper);

    let pbo = PboStatus {
        scalar: Some(2.0),
        fmax_mhz: Some(5550),
        ..PboStatus::default()
    };
    let pbo_json = serde_json::to_string(&pbo).unwrap();
    assert_eq!(parse_pbo_status(&pbo_json).unwrap(), pbo);

    // Wrapped PboGetOutput format emitted by `zensmu pbo get --json`
    let pbo_wrapped = serde_json::json!({
        "codename": "granite-ridge",
        "active_limits": { "ppt_w": 88.0 },
        "status": pbo
    })
    .to_string();
    assert_eq!(parse_pbo_status(&pbo_wrapped).unwrap(), pbo);

    let status_json = serde_json::json!({
        "co": co,
        "curve_shaper": shaper,
        "pbo": pbo,
    })
    .to_string();
    let parsed = parse_status_payload(&status_json).unwrap();
    assert_eq!(parsed.co.unwrap().igpu, Some(-10));
    assert_eq!(parsed.shaper.unwrap().high.med, -15);
    assert_eq!(parsed.pbo.unwrap().fmax_mhz, Some(5550));

    // Raw SmuStateSnapshot fallback
    let mut snap = SmuStateSnapshot::new(Codename::GraniteRidge, 8);
    snap.co_per_core = Some(vec![(0, -15), (1, -20)]);
    snap.fmax_mhz = Some(5600);
    let snap_json = serde_json::to_string(&snap).unwrap();
    let parsed_snap = parse_status_payload(&snap_json).unwrap();
    assert_eq!(parsed_snap.co.unwrap().cores[1].margin, -20);
    assert_eq!(parsed_snap.pbo.unwrap().fmax_mhz, Some(5600));
}

#[test]
fn parse_cli_error_handles_pkexec_cancel_and_json_errors() {
    assert!(parse_cli_error("", "", Some(126), true)
        .into_message()
        .contains("cancelled"));
    assert!(parse_cli_error("", "", Some(127), true)
        .into_message()
        .contains("PolicyKit"));
    assert_eq!(
        parse_cli_error(r#"{"error":"SMU rejected 0x07"}"#, "", Some(1), false).into_message(),
        "SMU rejected 0x07"
    );
    assert_eq!(
        parse_cli_error("", "plain stderr error", Some(1), false).into_message(),
        "plain stderr error"
    );
}

#[test]
fn parse_persist_action_and_clear_guard_decode_json_responses() {
    let status = PersistStatus {
        active_profile_path: "/etc/zensmu/active.toml".into(),
        active_profile_exists: true,
        active_profile_valid: Some(true),
        active_profile_error: None,
        active_profile_codename: Some(Codename::GraniteRidge),
        modules_load_configured: true,
        service_name: "zensmu-apply.service".into(),
        service_installed: true,
        service_enabled: true,
        service_state: "enabled".into(),
        resume_hook_installed: true,
        boot_guard_tripped: false,
    };

    let payload = serde_json::json!({
        "action": "persist save",
        "dry_run": false,
        "active_profile_path": "/etc/zensmu/active.toml",
        "service_enabled": true,
        "status": status,
        "note": "Enabled zensmu-apply.service"
    })
    .to_string();

    let decoded = parse_persist_action_output(&payload).unwrap();
    assert_eq!(decoded.action, "persist save");
    assert!(!decoded.dry_run);
    assert!(decoded.service_enabled);
    assert_eq!(
        decoded.status.active_profile_codename,
        Some(Codename::GraniteRidge)
    );
    assert_eq!(
        decoded.note.as_deref(),
        Some("Enabled zensmu-apply.service")
    );

    let err_payload = r#"{"ok": false, "error": "cannot enable service without active profile"}"#;
    assert!(parse_persist_action_output(err_payload)
        .unwrap_err()
        .contains("without active profile"));

    let guard_json =
        r#"{"guard_path":"/var/lib/zensmu/applying.lock","was_tripped":true,"cleared":true}"#;
    assert!(parse_clear_guard_output(guard_json).unwrap());

    let guard_noop =
        r#"{"guard_path":"/var/lib/zensmu/applying.lock","was_tripped":false,"cleared":false}"#;
    assert!(!parse_clear_guard_output(guard_noop).unwrap());

    let guard_err = r#"{"ok":false,"error":"permission denied"}"#;
    assert!(parse_clear_guard_output(guard_err)
        .unwrap_err()
        .contains("permission denied"));
}

#[test]
fn parse_report_payload_decodes_cli_wrapper_and_bare_report() {
    use zensmu_core::report::{SystemBoardInfo, GITHUB_ISSUES_NEW_URL};

    let report = DebugReport {
        zensmu_version: "0.1.1".into(),
        system: SystemBoardInfo {
            kernel_release: Some("6.12.0".into()),
            os_pretty_name: Some("Arch Linux".into()),
            board_vendor: Some("Gigabyte Technology Co., Ltd.".into()),
            board_name: Some("B450M DS3H V2".into()),
            bios_version: Some("F65b".into()),
            bios_date: Some("03/22/2024".into()),
        },
        cpuid: None,
        topology: None,
        driver: None,
        driver_error: Some("ryzen_smu not loaded".into()),
        codename: Some(Codename::Vermeer),
        capabilities_supported: true,
        telemetry: None,
        telemetry_error: None,
        raw_pm_table_floats: Vec::new(),
        hwmon_reference: Vec::new(),
        rsmu_ping: None,
        mp1_ping: None,
        pbo_status: None,
        pbo_error: None,
        co_snapshot: None,
        co_error: None,
        shaper_state: None,
        shaper_error: None,
        user_note: Some("Checking VRM caps".into()),
        secure_boot: None,
    };

    let expected = ReportPayload::from_report(report.clone());
    let cli_json = serde_json::json!({
        "report": report,
        "issue_title": expected.issue_title,
        "issue_labels": ["bug"],
        "issue_url": expected.issue_url,
        "markdown": expected.markdown,
    })
    .to_string();

    let decoded = parse_report_payload(&cli_json).unwrap();
    assert_eq!(decoded.issue_title, expected.issue_title);
    assert!(decoded.issue_url.starts_with(GITHUB_ISSUES_NEW_URL));
    assert!(decoded.markdown.contains("B450M DS3H V2"));
    assert_eq!(decoded.report.codename, Some(Codename::Vermeer));

    let bare_json = serde_json::to_string(&decoded.report).unwrap();
    let decoded_bare = parse_report_payload(&bare_json).unwrap();
    assert_eq!(decoded_bare.issue_title, expected.issue_title);
    assert!(decoded_bare.markdown.contains("Checking VRM caps"));

    let err_json = r#"{"ok":false,"error":"permission denied"}"#;
    assert!(parse_report_payload(err_json)
        .unwrap_err()
        .contains("permission denied"));
}

#[test]
fn parse_cli_error_detects_hardware_clamped_and_extracts_caps() {
    let clamped_json = serde_json::json!({
        "error": "motherboard/BIOS firmware clamped pbo.ppt_w (req 200 W -> cap 142 W), pbo.edc_a (req 220 A -> cap 120 A) (SMU accepted the command but capped the limit at the board VRM/BIOS ceiling; check BIOS PBO Limits)",
        "kind": "hardware_clamped"
    })
    .to_string();

    let parsed = parse_cli_error(&clamped_json, "", Some(1), true);
    match parsed {
        CliError::HardwareClamped { message, caps } => {
            assert!(message.contains("pbo.edc_a (req 220 A -> cap 120 A)"));
            assert_eq!(
                caps,
                vec![
                    ("pbo.ppt_w".to_string(), 142),
                    ("pbo.edc_a".to_string(), 120)
                ]
            );
        }
        other => panic!("expected HardwareClamped, got {other:?}"),
    }

    // Plain stderr fallback without JSON `kind` field
    let plain = "Error: motherboard/BIOS firmware clamped apu.stapm_w (req 65 W -> cap 45 W), apu.apu_skin_temp_c (req 60 C -> cap 52 C)";
    let parsed_plain = parse_cli_error("", plain, Some(1), false);
    match parsed_plain {
        CliError::HardwareClamped { caps, .. } => {
            assert_eq!(
                caps,
                vec![
                    ("apu.stapm_w".to_string(), 45),
                    ("apu.apu_skin_temp_c".to_string(), 52),
                ]
            );
        }
        other => panic!("expected HardwareClamped from plain text, got {other:?}"),
    }

    // Structured `clamped` array in JSON takes precedence when present
    let with_array = serde_json::json!({
        "error": "motherboard/BIOS firmware clamped limits",
        "kind": "hardware_clamped",
        "clamped": [
            { "field": "pbo.tdc_a", "actual_value": 95 }
        ]
    })
    .to_string();
    match parse_cli_error(&with_array, "", Some(1), false) {
        CliError::HardwareClamped { caps, .. } => {
            assert_eq!(caps, vec![("pbo.tdc_a".to_string(), 95)]);
        }
        other => panic!("expected HardwareClamped with array caps, got {other:?}"),
    }

    // Generic error remains Generic
    assert!(matches!(
        parse_cli_error(r#"{"error":"SMU rejected 0x07"}"#, "", Some(1), false),
        CliError::Generic(msg) if msg == "SMU rejected 0x07"
    ));
}
