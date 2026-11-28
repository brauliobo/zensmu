use super::super::app::App;
use super::super::render::draw_ui;
use super::super::types::{
    ConfirmAction, FlashKind, MailboxState, TuneParam, TuneSection, TuneValue, ViewTab,
};
use super::{buffer_text, mock_9700x};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::hw::profiles::capabilities_for;
use zensmu_core::hw::topology::CpuTopology;
use zensmu_core::ops::apu::ApuSetRequest;
use zensmu_core::ops::co;
use zensmu_core::ops::pm_table::{self, TelemetrySnapshot};
use zensmu_core::safety::snapshot::SnapshotStore;
use zensmu_core::transport::mock::MockTransport;
use zensmu_core::transport::SmuTransport;
use zensmu_core::types::{ApuPowerMode, Codename};

#[test]
fn tune_stage_dry_run_apply_and_rollback_co_and_pbo() {
    let (transport, hw) = mock_9700x();
    let tmp = tempfile::tempdir().unwrap();

    let mut app = App::new(500);
    app.test_allow_writes = true;
    app.snapshot_store = SnapshotStore::new(tmp.path().join("state"));
    app.poll_telemetry(&transport, &hw);
    app.refresh_mailbox(&transport, &hw);

    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);

    // Jump straight to Tune tab via 't'
    assert!(!app.handle_key(press(KeyCode::Char('t')), &transport, &hw));
    assert_eq!(app.active_tab, ViewTab::Tune);
    assert_eq!(app.selected_tune_param(&hw), Some(TuneParam::CoAll));

    // Stage CO All-Core = -20 via 4 coarse decrements ('H' = -5 each)
    for _ in 0..4 {
        app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
    }
    assert_eq!(
        app.staged.get(&TuneParam::CoAll),
        Some(&TuneValue::Int(-20))
    );

    // Jump sections twice: CurveOptimizer -> CurveShaper -> PBO
    app.handle_key(press(KeyCode::Char(']')), &transport, &hw);
    assert_eq!(
        app.selected_tune_param(&hw).map(TuneParam::section),
        Some(TuneSection::CurveShaper)
    );
    app.handle_key(press(KeyCode::Char(']')), &transport, &hw);
    assert_eq!(app.selected_tune_param(&hw), Some(TuneParam::PboPpt));

    // Live PPT on mock 9700X is 88 W; step +25 W twice -> 138 W
    app.handle_key(press(KeyCode::Char('L')), &transport, &hw);
    app.handle_key(press(KeyCode::Char('L')), &transport, &hw);
    assert_eq!(
        app.staged.get(&TuneParam::PboPpt),
        Some(&TuneValue::Int(138))
    );
    assert_eq!(app.staged_count(), 2);

    // Press 'a' to open Dry-Run Confirmation Modal (must NOT write to hardware yet)
    app.handle_key(press(KeyCode::Char('a')), &transport, &hw);
    let modal = app
        .confirm_modal
        .as_ref()
        .expect("confirm modal should open");
    assert_eq!(modal.ops.len(), 2);
    assert_eq!(modal.ops[0].target, "-20");
    assert_eq!(modal.ops[0].current.as_deref(), Some("0"));
    assert_eq!(modal.ops[1].target, "138 W");
    assert_eq!(modal.ops[1].current.as_deref(), Some("88 W"));

    // Verify hardware is still untouched before confirmation
    assert_eq!(co::get_co(&transport, &hw).unwrap().cores[0].margin, 0);
    assert_eq!(
        pm_table::read_telemetry(&transport, &hw)
            .unwrap()
            .ppt_limit_w,
        Some(88.0)
    );

    // Press 'y' to Confirm & Apply
    app.handle_key(press(KeyCode::Char('y')), &transport, &hw);
    assert!(app.confirm_modal.is_none());
    assert!(app.staged.is_empty());
    let (flash_msg, flash_kind) = app.active_flash().expect("success flash");
    assert_eq!(flash_kind, FlashKind::Success);
    assert!(flash_msg.contains("Applied 2 SMU operation(s) & verified"));

    // Verify real hardware state in MockTransport updated and verified!
    let co_after = co::get_co(&transport, &hw).unwrap();
    assert!(co_after.cores.iter().all(|c| c.margin == -20));
    let telem_after = pm_table::read_telemetry(&transport, &hw).unwrap();
    assert_eq!(telem_after.ppt_limit_w, Some(138.0));

    // Snapshots were saved before mutation
    assert!(app.snapshot_store.load_boot_baseline().unwrap().is_some());
    assert!(app.snapshot_store.load_last_state().unwrap().is_some());

    // Press 'R' (uppercase) to preview Rollback, then confirm with Enter
    app.handle_key(press(KeyCode::Char('R')), &transport, &hw);
    assert!(app.confirm_modal.is_some());
    app.handle_key(press(KeyCode::Enter), &transport, &hw);
    assert!(app.confirm_modal.is_none());

    // Hardware rolled back cleanly to 0 CO and 88 W PPT
    assert!(co::get_co(&transport, &hw)
        .unwrap()
        .cores
        .iter()
        .all(|c| c.margin == 0));
    assert_eq!(
        pm_table::read_telemetry(&transport, &hw)
            .unwrap()
            .ppt_limit_w,
        Some(88.0)
    );
}

#[test]
fn tune_refuses_unprivileged_write_and_guards_extreme_values() {
    let (transport, hw) = mock_9700x();
    let mut app = App::new(500);
    app.is_root = false;
    app.test_allow_writes = false;
    app.poll_telemetry(&transport, &hw);
    app.refresh_mailbox(&transport, &hw);

    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(press(KeyCode::Char('t')), &transport, &hw);

    // Stage CO = -10, open modal, attempt confirm while unprivileged -> must refuse
    app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
    app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
    app.handle_key(press(KeyCode::Enter), &transport, &hw);
    assert!(app.confirm_modal.is_some());
    app.handle_key(press(KeyCode::Char('y')), &transport, &hw);
    assert!(app.confirm_modal.is_none());
    let (msg, kind) = app.active_flash().unwrap();
    assert_eq!(kind, FlashKind::Error);
    assert!(msg.contains("root required"));
    assert_eq!(co::get_co(&transport, &hw).unwrap().cores[0].margin, 0);

    // Now test extreme guard: CO = -40 (below soft_min -35) blocked when allow_extreme = false
    app.test_allow_writes = true;
    for _ in 0..6 {
        app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
    }
    assert_eq!(
        app.staged.get(&TuneParam::CoAll),
        Some(&TuneValue::Int(-40))
    );
    app.handle_key(press(KeyCode::Char('a')), &transport, &hw);
    assert!(app.confirm_modal.is_none());
    let (msg, kind) = app.active_flash().unwrap();
    assert_eq!(kind, FlashKind::Error);
    assert!(msg.contains("soft limit"));

    // Toggle Allow Extreme ('x'), now dry-run preview succeeds
    app.handle_key(press(KeyCode::Char('x')), &transport, &hw);
    assert!(app.allow_extreme);
    app.handle_key(press(KeyCode::Char('a')), &transport, &hw);
    assert!(app.confirm_modal.is_some());

    // Cancel with Esc, then 'u' discards all staged changes
    app.handle_key(press(KeyCode::Esc), &transport, &hw);
    assert!(app.confirm_modal.is_none());
    app.handle_key(press(KeyCode::Char('u')), &transport, &hw);
    assert!(app.staged.is_empty());
}

#[test]
fn renders_tune_tab_and_confirm_modal_without_tty() {
    let (transport, hw) = mock_9700x();
    let mut app = App::new(500);
    app.test_allow_writes = true;
    app.poll_telemetry(&transport, &hw);
    app.refresh_mailbox(&transport, &hw);

    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(press(KeyCode::Char('4')), &transport, &hw);
    app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
    app.handle_key(press(KeyCode::Char(']')), &transport, &hw);
    app.handle_key(press(KeyCode::Char(']')), &transport, &hw);
    app.handle_key(press(KeyCode::Char('L')), &transport, &hw);

    for (w, h) in [(120u16, 36u16), (80, 24)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();

        // Render Tab 4 (Tune) with staged changes
        app.confirm_modal = None;
        terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
        let text = buffer_text(&terminal);
        assert!(text.contains("4:Tune"), "expected 4:Tune tab at {w}x{h}");
        assert!(text.contains("*2"), "expected *2 staged badge at {w}x{h}");
        assert!(
            text.contains("Live Telemetry"),
            "expected Live Telemetry strip at {w}x{h}"
        );
        assert!(text.contains("PPT"), "expected PPT telemetry at {w}x{h}");

        // Open Dry-Run Confirmation Modal and render cleanly
        app.handle_key(press(KeyCode::Char('a')), &transport, &hw);
        assert!(app.confirm_modal.is_some());
        terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
        let modal_text = buffer_text(&terminal);
        assert!(
            modal_text.contains("Dry-Run Preview"),
            "expected modal title at {w}x{h}"
        );
        assert!(
            modal_text.contains("Confirm & Apply"),
            "expected confirm button at {w}x{h}"
        );
        assert!(modal_text.contains("0x"), "expected hex opcode at {w}x{h}");
        assert!(
            modal_text.contains("→"),
            "expected current→target arrow at {w}x{h}"
        );
    }
}

#[test]
fn tune_hides_curve_shaper_when_firmware_unsupported() {
    let (transport, hw) = mock_9700x();
    let mut app = App::new(500);
    app.poll_telemetry(&transport, &hw);
    app.refresh_mailbox(&transport, &hw);
    app.shaper_state = MailboxState::FirmwareUnsupported("0xFE".to_string());
    app.active_tab = ViewTab::Tune;

    // No Shaper params should be exposed as interactive controls
    let params = app.tune_params(&hw);
    assert!(
        !params.iter().any(|p| matches!(p, TuneParam::Shaper { .. })),
        "Curve Shaper controls must be hidden when FirmwareUnsupported"
    );

    // Navigate down near the Curve Shaper banner so it's scrolled into view, and render
    app.tune_selected = params
        .iter()
        .position(|p| matches!(p, TuneParam::PboPpt))
        .unwrap_or(0);

    for (w, h) in [(120u16, 36u16), (80, 24)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("Not exposed by SMU") && text.contains("0xFE"),
            "expected FirmwareUnsupported banner at {w}x{h}, got:\n{text}"
        );
    }
}

#[test]
fn tune_exposes_apu_dptc_controls_on_apu() {
    let transport = MockTransport::granite_ridge_9700x();
    for codename in [Codename::Lucienne, Codename::Phoenix] {
        let mut driver = transport.read_info().unwrap();
        driver.codename_id = codename as u32;
        let hw = HardwareContext {
            driver,
            codename,
            cpuid: None,
            topology: CpuTopology::synthetic(1, 8, 2),
            capabilities: capabilities_for(codename).unwrap(),
        };

        let mut app = App::new(500);
        app.active_tab = ViewTab::Tune;
        let params = app.tune_params(&hw);
        for expected in [
            TuneParam::ApuStapm,
            TuneParam::ApuStapmTime,
            TuneParam::ApuFast,
            TuneParam::ApuSlow,
            TuneParam::ApuSlowTime,
            TuneParam::ApuApuSlow,
            TuneParam::ApuTdcSoc,
            TuneParam::ApuEdcSoc,
            TuneParam::ApuSkinTemp,
            TuneParam::ApuDgpuSkinTemp,
            TuneParam::ApuPowerMode,
        ] {
            assert!(
                params.contains(&expected),
                "missing APU control {expected:?} on {codename:?}"
            );
        }

        // Populate live APU telemetry and verify live_param_value reads all APU limits
        app.telemetry = Some(TelemetrySnapshot {
            codename,
            table_version: if codename == Codename::Lucienne {
                0x0037_0005
            } else {
                0x004C_0006
            },
            table_size: 0x8C8,
            ppt_limit_w: Some(30.0),
            ppt_value_w: Some(18.0),
            tdc_limit_a: Some(35.0),
            tdc_value_a: Some(12.0),
            edc_limit_a: Some(50.0),
            edc_value_a: Some(20.0),
            tctl_limit_c: Some(95.0),
            tctl_value_c: Some(61.0),
            stapm_limit_w: Some(25.0),
            stapm_value_w: Some(15.0),
            slow_limit_w: Some(28.0),
            slow_value_w: Some(16.0),
            apu_slow_limit_w: Some(22.0),
            apu_slow_value_w: Some(14.0),
            tdc_soc_limit_a: Some(20.0),
            tdc_soc_value_a: Some(8.5),
            edc_soc_limit_a: Some(35.0),
            edc_soc_value_a: Some(14.0),
            apu_skin_temp_limit_c: Some(45.0),
            apu_skin_temp_value_c: Some(39.0),
            dgpu_skin_temp_limit_c: Some(55.0),
            dgpu_skin_temp_value_c: Some(42.0),
            fclk_mhz: Some(1600.0),
            uclk_mhz: Some(1600.0),
            mclk_mhz: Some(1600.0),
            vddcr_soc_v: Some(0.95),
            vddcr_misc_v: None,
            cldo_vddp_v: None,
            cldo_vddg_iod_v: None,
            cldo_vddg_ccd_v: None,
        });

        assert_eq!(
            app.live_param_value(TuneParam::ApuStapm),
            Some(TuneValue::Int(25))
        );
        assert_eq!(app.live_param_value(TuneParam::ApuStapmTime), None);
        assert_eq!(
            app.live_param_value(TuneParam::ApuFast),
            Some(TuneValue::Int(30))
        );
        assert_eq!(
            app.live_param_value(TuneParam::ApuSlow),
            Some(TuneValue::Int(28))
        );
        assert_eq!(app.live_param_value(TuneParam::ApuSlowTime), None);
        assert_eq!(
            app.live_param_value(TuneParam::ApuApuSlow),
            Some(TuneValue::Int(22))
        );
        assert_eq!(
            app.live_param_value(TuneParam::ApuTdcSoc),
            Some(TuneValue::Int(20))
        );
        assert_eq!(
            app.live_param_value(TuneParam::ApuEdcSoc),
            Some(TuneValue::Int(35))
        );
        assert_eq!(
            app.live_param_value(TuneParam::ApuSkinTemp),
            Some(TuneValue::Int(45))
        );
        assert_eq!(
            app.live_param_value(TuneParam::ApuDgpuSkinTemp),
            Some(TuneValue::Int(55))
        );
        assert_eq!(
            app.live_param_value(TuneParam::ApuPowerMode),
            Some(TuneValue::PowerMode(None))
        );

        // Stage EVERY APU parameter and open dry-run preview modal
        app.staged.insert(TuneParam::ApuStapm, TuneValue::Int(35));
        app.staged
            .insert(TuneParam::ApuStapmTime, TuneValue::Int(120));
        app.staged.insert(TuneParam::ApuFast, TuneValue::Int(45));
        app.staged.insert(TuneParam::ApuSlow, TuneValue::Int(38));
        app.staged
            .insert(TuneParam::ApuSlowTime, TuneValue::Int(10));
        app.staged.insert(TuneParam::ApuApuSlow, TuneValue::Int(30));
        app.staged.insert(TuneParam::ApuTdcSoc, TuneValue::Int(25));
        app.staged.insert(TuneParam::ApuEdcSoc, TuneValue::Int(45));
        app.staged
            .insert(TuneParam::ApuSkinTemp, TuneValue::Int(50));
        app.staged
            .insert(TuneParam::ApuDgpuSkinTemp, TuneValue::Int(60));
        app.staged.insert(
            TuneParam::ApuPowerMode,
            TuneValue::PowerMode(Some(ApuPowerMode::MaxPerformance)),
        );

        app.open_staged_dry_run_modal(&transport, &hw);
        let modal = app
            .confirm_modal
            .as_ref()
            .expect("dry-run modal should open for all 11 APU params");
        assert_eq!(
            modal.ops.len(),
            11,
            "expected 11 planned APU ops on {codename:?}"
        );
        match &modal.action {
            ConfirmAction::ApplyStaged { apu: Some(req), .. } => {
                assert_eq!(
                    req,
                    &ApuSetRequest {
                        stapm_w: Some(35),
                        stapm_time_s: Some(120),
                        fast_w: Some(45),
                        slow_w: Some(38),
                        slow_time_s: Some(10),
                        apu_slow_w: Some(30),
                        tdc_soc_a: Some(25),
                        edc_soc_a: Some(45),
                        apu_skin_temp_c: Some(50),
                        dgpu_skin_temp_c: Some(60),
                        power_mode: Some(ApuPowerMode::MaxPerformance),
                    }
                );
            }
            other => panic!("expected ApplyStaged with ApuSetRequest, got {other:?}"),
        }
        // Verify live current enrichment on dry-run ops
        assert_eq!(modal.ops[0].current.as_deref(), Some("25 W"));
        assert_eq!(modal.ops[6].current.as_deref(), Some("20 A"));
        assert_eq!(modal.ops[7].current.as_deref(), Some("35 A"));
        assert_eq!(modal.ops[8].current.as_deref(), Some("45 C"));
        assert_eq!(modal.ops[9].current.as_deref(), Some("55 C"));

        // Render Overview, ClocksVolts, and Tune tabs with APU telemetry at both sizes
        app.confirm_modal = None;
        app.tune_selected = params
            .iter()
            .position(|p| *p == TuneParam::ApuStapm)
            .unwrap();
        for (w, h) in [(120u16, 36u16), (80, 24)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();

            app.active_tab = ViewTab::Overview;
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
            let overview_text = buffer_text(&terminal);
            assert!(
                overview_text.contains("STAPM"),
                "Overview should show STAPM on {codename:?} at {w}x{h}"
            );
            assert!(
                overview_text.contains("SoCTD"),
                "Overview should show SoC TDC on {codename:?} at {w}x{h}"
            );
            assert!(
                overview_text.contains("ASkin"),
                "Overview should show APU Skin on {codename:?} at {w}x{h}"
            );

            app.active_tab = ViewTab::ClocksVolts;
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();

            app.active_tab = ViewTab::Tune;
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
            let tune_text = buffer_text(&terminal);
            assert!(
                tune_text.contains("STAPM"),
                "Tune telemetry strip should show STAPM on {codename:?} at {w}x{h}"
            );
        }
    }
}
