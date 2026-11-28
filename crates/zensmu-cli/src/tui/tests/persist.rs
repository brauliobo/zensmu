use super::super::app::{default_persist_status, App};
use super::super::render::{draw_ui, param_bounds_summary};
use super::super::types::{ConfirmAction, FlashKind, TuneParam, TuneValue, ViewTab};
use super::{buffer_text, mock_9700x};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use zensmu_core::report::SystemBoardInfo;
use zensmu_core::safety::guard::BootLoopGuard;
use zensmu_core::safety::persist::PersistPaths;
use zensmu_core::safety::snapshot::SnapshotStore;
use zensmu_core::types::Codename;

#[test]
fn tune_persist_and_service_modal_transitions_and_rendering() {
    let (transport, hw) = mock_9700x();
    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(500);
    app.persist_paths = PersistPaths {
        active_profile: tmp.path().join("etc/zensmu/active.toml"),
        modules_load: tmp.path().join("etc/modules-load.d/ryzen_smu.conf"),
        guard_path: tmp.path().join("var/lib/zensmu/applying.lock"),
        resume_hook: tmp
            .path()
            .join("usr/lib/systemd/system-sleep/zensmu-resume"),
        service_name: "zensmu-apply.service".to_string(),
    };
    app.persist_status = default_persist_status(&app.persist_paths);
    app.poll_telemetry(&transport, &hw);
    app.refresh_mailbox(&transport, &hw);

    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(press(KeyCode::Char('4')), &transport, &hw);
    assert_eq!(app.active_tab, ViewTab::Tune);

    // 1. Pressing 'P' while staged changes exist must refuse with warning flash and NOT open modal
    app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
    assert_eq!(app.staged_count(), 1);
    app.handle_key(press(KeyCode::Char('P')), &transport, &hw);
    assert!(app.confirm_modal.is_none());
    let (msg, kind) = app.active_flash().expect("warning flash");
    assert_eq!(kind, FlashKind::Warn);
    assert!(msg.contains("Apply (a) or discard (u) staged changes"));

    // Discard staged changes via 'u'
    app.handle_key(press(KeyCode::Char('u')), &transport, &hw);
    assert!(app.staged.is_empty());

    // 2. Pressing 'P' with clean staged state opens PersistLiveAndEnable confirmation modal
    app.handle_key(press(KeyCode::Char('P')), &transport, &hw);
    {
        let modal = app
            .confirm_modal
            .as_ref()
            .expect("persist modal should open");
        assert!(matches!(modal.action, ConfirmAction::PersistLiveAndEnable));
        assert_eq!(modal.steps.len(), 3);
        assert!(modal.steps[0].1.contains("active.toml"));
        assert!(modal.steps[1].1.contains("ryzen_smu"));
        assert!(modal.steps[2].1.contains("zensmu-apply.service"));
    }

    // Render modal at 120x36 and 80x24
    for (w, h) in [(120u16, 36u16), (80, 24)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("Persist Live State"),
            "expected persist modal title at {w}x{h}"
        );
        assert!(
            text.contains("ryzen_smu"),
            "expected modules-load step at {w}x{h}"
        );
    }

    // Confirming 'P' while unprivileged refuses with helpful sudo error
    app.is_root = false;
    app.test_allow_writes = false;
    app.handle_key(press(KeyCode::Char('y')), &transport, &hw);
    assert!(app.confirm_modal.is_none());
    let (msg, kind) = app.active_flash().expect("unprivileged error flash");
    assert_eq!(kind, FlashKind::Error);
    assert!(msg.contains("root required"));

    // 3. Pressing 'E' when disabled and active.toml missing states it will save live state first + enable
    app.persist_status.service_enabled = false;
    app.persist_status.active_profile_exists = false;
    app.handle_key(press(KeyCode::Char('E')), &transport, &hw);
    {
        let modal = app
            .confirm_modal
            .as_ref()
            .expect("enable modal should open");
        assert!(matches!(
            modal.action,
            ConfirmAction::ToggleBootService { enable: true }
        ));
        assert!(modal.title.contains("Save Live State"));
        assert!(modal.subtitle.contains("save live state first"));
    }
    app.handle_key(press(KeyCode::Esc), &transport, &hw);
    assert!(app.confirm_modal.is_none());

    // 4. Pressing 'E' when disabled and active.toml valid opens Enable modal using existing profile
    app.persist_status.active_profile_exists = true;
    app.persist_status.active_profile_valid = Some(true);
    app.persist_status.active_profile_codename = Some(Codename::GraniteRidge);
    app.handle_key(press(KeyCode::Char('E')), &transport, &hw);
    {
        let modal = app
            .confirm_modal
            .as_ref()
            .expect("enable modal should open");
        assert!(matches!(
            modal.action,
            ConfirmAction::ToggleBootService { enable: true }
        ));
        assert!(modal.subtitle.contains("using existing"));
    }
    app.handle_key(press(KeyCode::Char('n')), &transport, &hw);
    assert!(app.confirm_modal.is_none());

    // 5. Pressing 'E' when enabled opens Disable modal
    app.persist_status.service_enabled = true;
    app.handle_key(press(KeyCode::Char('E')), &transport, &hw);
    {
        let modal = app
            .confirm_modal
            .as_ref()
            .expect("disable modal should open");
        assert!(matches!(
            modal.action,
            ConfirmAction::ToggleBootService { enable: false }
        ));
        assert!(modal.title.contains("Disable"));
    }
    app.handle_key(press(KeyCode::Esc), &transport, &hw);
    assert!(app.confirm_modal.is_none());

    // 6. Test Boot Persistence badges & inspector block rendering on Tab 4 across all 3 states
    for (enabled, exists, expected_badge) in [
        (true, true, "[BOOT:ON]"),
        (false, true, "[BOOT:SAVED]"),
        (false, false, "[BOOT:OFF]"),
    ] {
        app.persist_status.service_enabled = enabled;
        app.persist_status.active_profile_exists = exists;
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains(expected_badge),
            "expected badge {expected_badge} on Tab 4, got:\n{text}"
        );
        assert!(
            text.contains("Boot Persistence"),
            "expected Boot Persistence inspector block on Tab 4"
        );
        assert!(
            text.contains("Persist+Enable"),
            "expected Persist+Enable in Tab 4 footer"
        );
        assert!(
            text.contains("Toggle Service"),
            "expected Toggle Service in Tab 4 footer"
        );
    }

    // 7. Test 'C' clears tripped boot guard after confirmation
    let guard = BootLoopGuard::new(app.persist_paths.guard_path.clone());
    let _armed = guard.check_and_arm("active.toml").unwrap();
    assert!(guard.is_tripped());
    app.persist_status.boot_guard_tripped = true;
    app.test_allow_writes = true;

    app.handle_key(press(KeyCode::Char('C')), &transport, &hw);
    assert!(matches!(
        app.confirm_modal.as_ref().map(|m| &m.action),
        Some(ConfirmAction::ClearBootGuard)
    ));
    app.handle_key(press(KeyCode::Enter), &transport, &hw);
    assert!(app.confirm_modal.is_none());
    assert!(!guard.is_tripped());
    assert!(!app.persist_status.boot_guard_tripped);
    let (msg, kind) = app.active_flash().expect("clear guard success flash");
    assert_eq!(kind, FlashKind::Success);
    assert!(msg.contains("Cleared boot-loop"));
}

#[test]
fn tune_hardware_clamped_records_board_caps_and_renders_untruncated_warning() {
    let (transport, hw) = mock_9700x();
    transport.set_clamp_tdc_a(Some(100));
    transport.set_clamp_edc_a(Some(120));
    transport.set_sustained_power_thm_raw(0x0069_005A); // 105 W TDP, 90 °C TjMax

    let tmp = tempfile::tempdir().unwrap();
    let mut app = App::new(500);
    app.test_allow_writes = true;
    app.snapshot_store = SnapshotStore::new(tmp.path().join("state"));
    app.board_info = SystemBoardInfo {
        board_vendor: Some("Gigabyte Technology Co., Ltd.".into()),
        board_name: Some("B450M DS3H V2".into()),
        bios_version: Some("F66".into()),
        ..Default::default()
    };
    app.poll_telemetry(&transport, &hw);
    app.refresh_mailbox(&transport, &hw);

    // Verify proactive board/BIOS VRM note is shown on PBO param before any clamp occurs
    app.active_tab = ViewTab::Tune;
    let params = app.tune_params(&hw);
    app.tune_selected = params.iter().position(|p| *p == TuneParam::PboPpt).unwrap();
    {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
        let pre_text = buffer_text(&terminal);
        assert!(
            pre_text.contains("B450M")
                && pre_text.contains("DS3H V2")
                && pre_text.contains("BIOS F66")
                && pre_text.contains("BIOS PBO Limits")
                && pre_text.contains("PPT/TDC/EDC"),
            "expected proactive board/BIOS VRM note before clamp:\n{pre_text}"
        );
        assert!(
            pre_text.contains("TDP 105W") && pre_text.contains("TjMax 90°C"),
            "expected fused TDP & TjMax in Tune PBO section header:\n{pre_text}"
        );
    }

    // Verify Overview and CO/Shaper tabs show TDP & TjMax
    {
        let backend = TestBackend::new(120, 36);
        let mut terminal = Terminal::new(backend).unwrap();
        app.active_tab = ViewTab::Overview;
        terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
        let over_text = buffer_text(&terminal);
        assert!(
            over_text.contains("105W") && over_text.contains("90°C"),
            "expected TDP/TjMax in Overview PBO summary:\n{over_text}"
        );

        app.active_tab = ViewTab::CoShaper;
        terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
        let co_text = buffer_text(&terminal);
        assert!(
            co_text.contains("Fused TDP / TjMax") && co_text.contains("105 W / 90 °C"),
            "expected Fused TDP / TjMax in CO/Shaper fused PBO table:\n{co_text}"
        );
    }

    // Stage PBO PPT=142W (not clamped), TDC=140A (clamped to 100A), EDC=220A (clamped to 120A)
    app.active_tab = ViewTab::Tune;
    app.allow_extreme = true;
    app.staged.insert(TuneParam::PboPpt, TuneValue::Int(142));
    app.staged.insert(TuneParam::PboTdc, TuneValue::Int(140));
    app.staged.insert(TuneParam::PboEdc, TuneValue::Int(220));

    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
    app.handle_key(press(KeyCode::Char('a')), &transport, &hw);
    assert!(app.confirm_modal.is_some());
    app.handle_key(press(KeyCode::Char('y')), &transport, &hw);

    // Modal closed, staged cleared, board_caps recorded for both clamped params
    assert!(app.confirm_modal.is_none());
    assert!(app.staged.is_empty());
    assert_eq!(app.board_caps.get(&TuneParam::PboPpt), None);
    assert_eq!(app.board_caps.get(&TuneParam::PboTdc), Some(&100));
    assert_eq!(app.board_caps.get(&TuneParam::PboEdc), Some(&120));

    let expected_msg = "Board/BIOS cap hit: pbo.tdc_a (req 140 A -> cap 100 A), pbo.edc_a (req 220 A -> cap 120 A) (check BIOS PBO Limits)";
    let (flash_msg, flash_kind) = app.active_flash().expect("board-cap warning flash");
    assert_eq!(flash_kind, FlashKind::Warn);
    assert_eq!(flash_msg, expected_msg);
    assert_eq!(
        app.last_alert.as_ref(),
        Some(&(expected_msg.to_string(), FlashKind::Warn))
    );

    // param_bounds_summary includes learned board cap
    let edc_bounds = param_bounds_summary(&app, TuneParam::PboEdc, &hw);
    assert!(
        edc_bounds.contains("board cap ≤120 A"),
        "expected board cap in bounds summary, got: {edc_bounds}"
    );

    // Select PboEdc and render at both 120x36 and 80x24: verify learned cap + full untruncated warning
    app.tune_selected = params.iter().position(|p| *p == TuneParam::PboEdc).unwrap();
    for (w, h) in [(120u16, 36u16), (80, 24)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
        let text = buffer_text(&terminal);
        assert!(
            text.contains("board cap ≤120 A"),
            "expected learned board cap hint at {w}x{h}:\n{text}"
        );
        assert!(
            text.contains("Board/BIOS cap hit:")
                && text.contains("pbo.tdc_a")
                && text.contains("100 A")
                && text.contains("pbo.edc_a")
                && text.contains("120 A")
                && text.contains("BIOS PBO Limits"),
            "expected full board-cap warning without truncation at {w}x{h}:\n{text}"
        );
    }
}
