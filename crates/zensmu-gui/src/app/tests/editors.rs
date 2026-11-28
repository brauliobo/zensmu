use super::super::*;
use crate::elevate::CliAction;
use eframe::egui;
use std::fs;
use zensmu_core::bench::BenchWorkload;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::ops::apu::ApuSetRequest;
use zensmu_core::ops::pbo::PboStatus;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::recorder::RecordFormat;
use zensmu_core::report::SystemBoardInfo;
use zensmu_core::types::{ApuPowerMode, Codename};

#[test]
fn ui_zoom_clamps_and_rounds_cleanly() {
    assert_eq!(clamp_ui_zoom(0.50), UI_ZOOM_MIN);
    assert_eq!(clamp_ui_zoom(UI_ZOOM_MIN), 0.85);
    assert_eq!(clamp_ui_zoom(1.0 + 0.1 + 0.1), 1.2);
    assert_eq!(clamp_ui_zoom(2.25), UI_ZOOM_MAX);
}

#[test]
fn dark_cockpit_theme_sets_legible_typography_and_spacing() {
    let ctx = egui::Context::default();
    configure_dark_cockpit_theme(&ctx);

    let style = ctx.style();
    assert_eq!(
        style.text_styles.get(&egui::TextStyle::Heading),
        Some(&egui::FontId::new(24.0, egui::FontFamily::Proportional))
    );
    assert_eq!(
        style.text_styles.get(&egui::TextStyle::Body),
        Some(&egui::FontId::new(16.0, egui::FontFamily::Proportional))
    );
    assert_eq!(
        style.text_styles.get(&egui::TextStyle::Button),
        Some(&egui::FontId::new(15.5, egui::FontFamily::Proportional))
    );
    assert_eq!(
        style.text_styles.get(&egui::TextStyle::Monospace),
        Some(&egui::FontId::new(15.0, egui::FontFamily::Monospace))
    );
    assert_eq!(
        style.text_styles.get(&egui::TextStyle::Small),
        Some(&egui::FontId::new(13.5, egui::FontFamily::Proportional))
    );
    assert_eq!(style.spacing.item_spacing, egui::vec2(10.0, 8.0));
    assert_eq!(style.spacing.button_padding, egui::vec2(12.0, 6.0));
    assert_eq!(style.spacing.interact_size.y, 26.0);
    assert!(!ctx.options(|o| o.zoom_with_keyboard));
}

#[test]
fn apu_editor_syncs_non_dirty_fields_and_builds_filtered_cli_action() {
    use zensmu_core::hw::profiles::capabilities_for;
    use zensmu_core::hw::topology::CpuTopology;
    use zensmu_core::transport::DriverSysfsInfo;
    use zensmu_core::types::Codename;

    let mut editor = ApuEditorState {
        slow_dirty: true,
        slow_w: 42,
        ..Default::default()
    };

    let snap = TelemetrySnapshot {
        codename: Codename::Phoenix,
        table_version: 0x004C_0008,
        table_size: 0x800,
        ppt_limit_w: Some(54.0),
        ppt_value_w: Some(20.0),
        tdc_limit_a: Some(45.0),
        tdc_value_a: Some(15.0),
        edc_limit_a: Some(75.0),
        edc_value_a: Some(22.0),
        tctl_limit_c: Some(95.0),
        tctl_value_c: Some(55.0),
        stapm_limit_w: Some(28.0),
        stapm_value_w: Some(18.0),
        slow_limit_w: Some(35.0),
        slow_value_w: Some(19.0),
        apu_slow_limit_w: Some(25.0),
        apu_slow_value_w: Some(14.0),
        tdc_soc_limit_a: Some(22.0),
        tdc_soc_value_a: Some(8.0),
        edc_soc_limit_a: Some(40.0),
        edc_soc_value_a: Some(12.0),
        apu_skin_temp_limit_c: Some(48.0),
        apu_skin_temp_value_c: Some(37.0),
        dgpu_skin_temp_limit_c: Some(0.0),
        dgpu_skin_temp_value_c: Some(0.0),
        fclk_mhz: Some(2000.0),
        uclk_mhz: Some(2000.0),
        mclk_mhz: Some(2000.0),
        vddcr_soc_v: Some(1.1),
        vddcr_misc_v: None,
        cldo_vddp_v: None,
        cldo_vddg_iod_v: None,
        cldo_vddg_ccd_v: None,
    };

    editor.sync_from_telemetry(&snap);
    assert_eq!(editor.stapm_w, 28);
    assert_eq!(editor.fast_w, 54);
    // Dirty field stays at user value while live value still updates.
    assert_eq!(editor.slow_w, 42);
    assert_eq!(editor.live_slow_w, Some(35.0));
    assert_eq!(editor.apu_slow_w, 25);
    assert_eq!(editor.tdc_soc_a, 22);
    assert_eq!(editor.edc_soc_a, 40);
    assert_eq!(editor.apu_skin_temp_c, 48);
    // Zero limit leaves default slider untouched.
    assert_eq!(editor.dgpu_skin_temp_c, 60);
    assert_eq!(editor.live_dgpu_skin_temp_c, Some(0.0));

    let phoenix_hw = HardwareContext {
        driver: DriverSysfsInfo {
            drv_version: "0.1.7".into(),
            smu_version: "76.70.0".into(),
            codename_id: Codename::Phoenix as u32,
            mp1_if_version: 13,
            has_rsmu: true,
            has_pm_table: true,
            pm_table_version: Some(0x004C_0008),
            pm_table_size: Some(0x800),
        },
        codename: Codename::Phoenix,
        cpuid: None,
        topology: CpuTopology::synthetic(1, 8, 2),
        capabilities: capabilities_for(Codename::Phoenix).unwrap(),
    };

    assert!(!editor.has_any_enabled_for(&phoenix_hw));
    assert!(editor.build_cli_action_for(&phoenix_hw, true).is_none());

    editor.stapm_enabled = true;
    editor.stapm_w = 110; // above soft max 100
    editor.power_mode = Some(ApuPowerMode::MaxPerformance);
    assert!(editor.has_any_enabled_for(&phoenix_hw));
    assert!(editor.has_any_extreme_for(&phoenix_hw));

    let action = editor.build_cli_action_for(&phoenix_hw, true).unwrap();
    assert_eq!(
        action,
        CliAction::ApuSet {
            req: ApuSetRequest {
                stapm_w: Some(110),
                power_mode: Some(ApuPowerMode::MaxPerformance),
                ..Default::default()
            },
            allow_extreme: false,
            dry_run: true,
        }
    );

    // Capability filtering on PboEditorState (Lucienne has no set_fmax)
    let lucienne_caps = capabilities_for(Codename::Lucienne).unwrap();
    let pbo = PboEditorState {
        fmax_enabled: true,
        fmax_mhz: 4500,
        ..Default::default()
    };
    let pbo_req = pbo.build_request_for(&lucienne_caps);
    assert_eq!(pbo_req.fmax_mhz, None);
}

#[test]
fn pbo_editor_respects_dirty_flags_clamps_full_bounds_and_resyncs() {
    use zensmu_core::hw::profiles::capabilities_for;
    use zensmu_core::types::Codename;

    let mut pbo = PboEditorState {
        ppt_dirty: true,
        ppt_w: 120,
        scalar_dirty: true,
        scalar: 4,
        ..Default::default()
    };

    let snap = TelemetrySnapshot {
        codename: Codename::GraniteRidge,
        table_version: 0x0062_0205,
        table_size: 0x800,
        ppt_limit_w: Some(1000.0), // BIOS Motherboard/Unlimited profile
        ppt_value_w: Some(85.0),
        tdc_limit_a: Some(480.0),
        tdc_value_a: Some(45.0),
        edc_limit_a: Some(f32::NAN), // non-finite ignored
        edc_value_a: Some(60.0),
        tctl_limit_c: Some(95.0),
        tctl_value_c: Some(65.0),
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
        vddcr_soc_v: Some(1.2),
        vddcr_misc_v: None,
        cldo_vddp_v: None,
        cldo_vddg_iod_v: None,
        cldo_vddg_ccd_v: None,
    };

    pbo.sync_from_telemetry(&snap);
    // Dirty PPT stays at user value; non-dirty TDC/Tctl sync to full bounds; NaN EDC untouched.
    assert_eq!(pbo.ppt_w, 120);
    assert_eq!(pbo.tdc_a, 480);
    assert_eq!(pbo.edc_a, 150);
    assert_eq!(pbo.tctl_c, 95);

    let caps = capabilities_for(Codename::GraniteRidge).unwrap();
    assert!(pbo.has_any_extreme_for(&caps)); // TDC 480 > 180 soft max

    // Status sync respects dirty when force=false, overwrites when force=true.
    let status = PboStatus {
        scalar: Some(2.0),
        fmax_mhz: Some(5650),
        ..PboStatus::default()
    };
    pbo.sync_from_status(&status, false);
    assert_eq!(pbo.scalar, 4);
    assert_eq!(pbo.fmax_mhz, 5650);

    pbo.sync_from_status(&status, true);
    assert_eq!(pbo.scalar, 2);
    assert!(!pbo.scalar_dirty);

    // Clearing dirty allows PPT to resync to live 1000W limit.
    pbo.clear_dirty();
    pbo.sync_from_telemetry(&snap);
    assert_eq!(pbo.ppt_w, 1000);
}

#[test]
fn format_board_summary_formats_vendor_model_and_bios_cleanly() {
    let full = SystemBoardInfo {
        board_vendor: Some("Gigabyte Technology Co., Ltd.".into()),
        board_name: Some("B450M DS3H V2".into()),
        bios_version: Some("F65b".into()),
        ..SystemBoardInfo::default()
    };
    assert_eq!(
        format_board_summary(&full).as_deref(),
        Some("Gigabyte Technology Co., Ltd. B450M DS3H V2 · BIOS F65b")
    );

    let no_vendor = SystemBoardInfo {
        board_name: Some("ROG STRIX X670E-E".into()),
        bios_version: Some("2401".into()),
        ..SystemBoardInfo::default()
    };
    assert_eq!(
        format_board_summary(&no_vendor).as_deref(),
        Some("ROG STRIX X670E-E · BIOS 2401")
    );

    assert_eq!(format_board_summary(&SystemBoardInfo::default()), None);
}

#[test]
fn open_url_in_browser_rejects_non_http_schemes() {
    let err = open_url_in_browser("file:///etc/passwd").unwrap_err();
    assert!(err.contains("http://"));
    assert!(open_url_in_browser("javascript:alert(1)").is_err());
}

#[test]
fn error_message_suggests_report_matches_smu_and_unsupported_errors() {
    assert!(error_message_suggests_report("SMU rejected command 0x07"));
    assert!(error_message_suggests_report(
        "read-after-write verification failed for pbo.ppt_w"
    ));
    assert!(error_message_suggests_report(
        "unsupported PM table version 0xDEADBEEF"
    ));
    assert!(error_message_suggests_report(
        "CPU identity mismatch: ryzen_smu reports Vermeer"
    ));
    assert!(!error_message_suggests_report(
        "PolicyKit authentication was dismissed or cancelled."
    ));
}

#[test]
fn report_modal_state_defaults_match_expected_path_and_flags() {
    let state = ReportModalState::default();
    assert!(!state.open);
    assert!(!state.include_raw_pm_table);
    assert!(!state.include_privileged_probes);
    assert_eq!(state.save_path, "/tmp/zensmu-report.md");
    assert!(state.payload.is_none());
    assert!(state.status_note.is_none());
}

#[test]
fn recorder_ui_state_records_and_flushes_csv_jsonl_and_pts_cleanly() {
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
    use zensmu_core::recorder::CSV_HEADER;

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);
    let seq = TEST_SEQ.fetch_add(1, AtomicOrdering::Relaxed);
    let test_dir = std::env::temp_dir().join(format!(
        "zensmu-gui-recorder-test-{}-{seq}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&test_dir);
    fs::create_dir_all(&test_dir).unwrap();

    struct CleanupDir(PathBuf);
    impl Drop for CleanupDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = CleanupDir(test_dir.clone());

    let sample_a = TelemetrySnapshot {
        codename: Codename::GraniteRidge,
        table_version: 0x0062_0205,
        table_size: 0x800,
        ppt_limit_w: Some(142.0),
        ppt_value_w: Some(88.5),
        tdc_limit_a: Some(110.0),
        tdc_value_a: Some(62.0),
        edc_limit_a: Some(170.0),
        edc_value_a: Some(95.0),
        tctl_limit_c: Some(95.0),
        tctl_value_c: Some(71.25),
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
        cldo_vddp_v: Some(0.95),
        cldo_vddg_iod_v: Some(0.95),
        cldo_vddg_ccd_v: Some(0.95),
    };
    let mut sample_b = sample_a.clone();
    sample_b.ppt_value_w = Some(102.0);
    sample_b.tctl_value_c = Some(76.5);

    let mut ui_state = RecorderUiState::default();
    assert!(!ui_state.is_recording());
    assert_eq!(ui_state.format, RecordFormat::Csv);

    // 1. CSV recording
    ui_state.format = RecordFormat::Csv;
    ui_state.tag = "co-20".into();
    let csv_path = test_dir.join("run.csv");
    let opened = ui_state.start_with_path(csv_path.clone()).unwrap();
    assert_eq!(opened, csv_path);
    assert!(ui_state.is_recording());
    assert!(ui_state.started_at.is_some());
    ui_state.write_snapshot(&sample_a).unwrap();
    ui_state.write_snapshot(&sample_b).unwrap();
    assert_eq!(ui_state.live_samples(), 2);

    let stopped = ui_state.stop().unwrap().unwrap();
    assert_eq!(stopped, (csv_path.clone(), 2));
    assert!(!ui_state.is_recording());
    assert!(ui_state.started_at.is_none());
    assert_eq!(ui_state.last_saved_path.as_ref(), Some(&csv_path));
    assert_eq!(ui_state.last_saved_samples, 2);

    let csv_text = fs::read_to_string(&csv_path).unwrap();
    let csv_lines: Vec<&str> = csv_text.lines().collect();
    assert_eq!(csv_lines.len(), 3, "expected header + 2 rows");
    assert_eq!(csv_lines[0], CSV_HEADER);
    assert!(csv_lines[1].contains("granite-ridge,co-20,88.50,142.00"));
    assert!(csv_lines[2].contains("granite-ridge,co-20,102.00,142.00"));

    // 2. JSONL recording
    ui_state.format = RecordFormat::Jsonl;
    ui_state.tag = "gaming".into();
    let jsonl_path = test_dir.join("run.jsonl");
    ui_state.start_with_path(jsonl_path.clone()).unwrap();
    ui_state.write_snapshot(&sample_a).unwrap();
    ui_state.write_snapshot(&sample_b).unwrap();
    let stopped = ui_state.stop().unwrap().unwrap();
    assert_eq!(stopped, (jsonl_path.clone(), 2));
    assert_eq!(ui_state.last_saved_path.as_ref(), Some(&jsonl_path));
    assert_eq!(ui_state.last_saved_samples, 2);

    let jsonl_text = fs::read_to_string(&jsonl_path).unwrap();
    let jsonl_lines: Vec<&str> = jsonl_text.lines().collect();
    assert_eq!(jsonl_lines.len(), 2);
    let parsed0: serde_json::Value = serde_json::from_str(jsonl_lines[0]).unwrap();
    let parsed1: serde_json::Value = serde_json::from_str(jsonl_lines[1]).unwrap();
    assert_eq!(parsed0["tag"], "gaming");
    assert_eq!(parsed0["ppt_value_w"], 88.5);
    assert_eq!(parsed1["ppt_value_w"], 102.0);

    // 3. PTS composite.xml recording + PTS result path resolution
    ui_state.format = RecordFormat::Pts;
    ui_state.tag = "prime95".into();
    ui_state.pts_result_name = "my-bench-run".into();
    let resolved_pts = ui_state.resolve_output_path(Codename::GraniteRidge);
    assert!(
        resolved_pts.ends_with("test-results/my-bench-run/composite.xml"),
        "unexpected resolved PTS path: {}",
        resolved_pts.display()
    );

    let pts_path = test_dir.join("composite.xml");
    ui_state.start_with_path(pts_path.clone()).unwrap();
    ui_state.write_snapshot(&sample_a).unwrap();
    ui_state.write_snapshot(&sample_b).unwrap();
    let stopped = ui_state.stop().unwrap().unwrap();
    assert_eq!(stopped, (pts_path.clone(), 2));
    assert_eq!(ui_state.last_saved_path.as_ref(), Some(&pts_path));
    assert_eq!(ui_state.last_saved_samples, 2);

    let xml = fs::read_to_string(&pts_path).unwrap();
    assert!(xml.contains("<PhoronixTestSuite>"));
    assert!(xml.contains("<Identifier>zensmu/ppt</Identifier>"));
    assert!(xml.contains("<Identifier>zensmu/tctl</Identifier>"));
    assert!(xml.contains("88.50:102.00"));
    assert!(xml.contains("</PhoronixTestSuite>"));
}

#[test]
fn bench_ui_state_defaults_match_spec() {
    let state = BenchUiState::default();
    assert!(!state.running);
    assert_eq!(state.duration_per_core_secs, 15);
    assert_eq!(state.passes, 1);
    assert_eq!(state.workload, BenchWorkload::Mixed);
    assert!(state.stop_on_error);
    assert!(state.progress.is_none());
    assert!(state.last_report.is_none());
    assert!(state.error.is_none());
    assert!(!state.stop_flag.load(Ordering::Relaxed));
    assert!(state.rx.is_none());
}
