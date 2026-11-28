use super::super::app::App;
use super::super::render::draw_ui;
use super::super::types::{FlashKind, MailboxState, ViewTab};
use super::super::UNPRIV_MAILBOX_NOTE;
use super::{buffer_text, mock_9700x};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use zensmu_core::recorder::{self, RecordFormat};

#[test]
fn renders_all_tabs_and_help_without_tty() {
    let (transport, hw) = mock_9700x();
    let mut app = App::new(500);
    app.poll_telemetry(&transport, &hw);
    app.refresh_mailbox(&transport, &hw);

    assert!(app.telemetry.is_some());
    assert!(app.co_state.as_loaded().is_some());
    assert!(app.shaper_state.as_loaded().is_some());
    assert!(app.pbo_state.as_loaded().is_some());

    for (w, h) in [(120u16, 36u16), (80, 24)] {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).unwrap();

        for tab in [
            ViewTab::Overview,
            ViewTab::ClocksVolts,
            ViewTab::CoShaper,
            ViewTab::Tune,
        ] {
            app.active_tab = tab;
            app.show_help = false;
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
        }

        // Render Help overlay
        app.show_help = true;
        terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
    }
}

#[test]
fn handles_unprivileged_mailbox_gracefully() {
    let (transport, hw) = mock_9700x();
    let mut app = App::new(1000);
    app.poll_telemetry(&transport, &hw);

    // Simulate unprivileged PermissionDenied state on all mailbox queries
    app.co_state = MailboxState::PermissionDenied;
    app.shaper_state = MailboxState::PermissionDenied;
    app.pbo_state = MailboxState::PermissionDenied;
    app.mailbox_note = Some(UNPRIV_MAILBOX_NOTE.to_string());

    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    for tab in [
        ViewTab::Overview,
        ViewTab::ClocksVolts,
        ViewTab::CoShaper,
        ViewTab::Tune,
    ] {
        app.active_tab = tab;
        terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
    }
}

#[test]
fn keybindings_navigate_pause_interval_help_and_quit() {
    let (transport, hw) = mock_9700x();
    let mut app = App::new(500);
    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);

    assert_eq!(app.active_tab, ViewTab::Overview);
    assert!(!app.handle_key(press(KeyCode::Tab), &transport, &hw));
    assert_eq!(app.active_tab, ViewTab::ClocksVolts);
    assert!(!app.handle_key(press(KeyCode::Char('3')), &transport, &hw));
    assert_eq!(app.active_tab, ViewTab::CoShaper);
    assert!(!app.handle_key(press(KeyCode::BackTab), &transport, &hw));
    assert_eq!(app.active_tab, ViewTab::ClocksVolts);
    assert!(!app.handle_key(press(KeyCode::Char('1')), &transport, &hw));
    assert_eq!(app.active_tab, ViewTab::Overview);
    assert!(!app.handle_key(press(KeyCode::Char('t')), &transport, &hw));
    assert_eq!(app.active_tab, ViewTab::Tune);
    assert!(!app.handle_key(press(KeyCode::Char('1')), &transport, &hw));
    assert_eq!(app.active_tab, ViewTab::Overview);

    assert!(!app.paused);
    assert!(!app.handle_key(press(KeyCode::Char('p')), &transport, &hw));
    assert!(app.paused);

    assert!(!app.handle_key(press(KeyCode::Char('+')), &transport, &hw));
    assert_eq!(app.interval_ms, 600);
    assert!(!app.handle_key(press(KeyCode::Char('-')), &transport, &hw));
    assert_eq!(app.interval_ms, 500);

    assert!(!app.handle_key(press(KeyCode::Char('?')), &transport, &hw));
    assert!(app.show_help);
    assert!(!app.handle_key(press(KeyCode::Esc), &transport, &hw));
    assert!(!app.show_help);

    assert!(!app.handle_key(press(KeyCode::Char('r')), &transport, &hw));
    assert!(app.telemetry.is_some());
    assert!(app.co_state.as_loaded().is_some());

    assert!(app.handle_key(press(KeyCode::Char('q')), &transport, &hw));
    assert!(app.handle_key(
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
        &transport,
        &hw
    ));
}

#[test]
fn tui_recording_toggle_writes_and_saves_valid_csv() {
    let dir = tempfile::tempdir().unwrap();
    let (transport, hw) = mock_9700x();
    let mut app = App::new(500);
    // Explicitly unprivileged: telemetry recording must work without root!
    app.is_root = false;
    app.test_allow_writes = false;
    app.record_dir_override = Some(dir.path().to_path_buf());

    let press = |code| KeyEvent::new(code, KeyModifiers::NONE);

    // Press 'w' to start recording in default CSV format
    assert!(!app.handle_key(press(KeyCode::Char('w')), &transport, &hw));
    assert!(app.recorder.is_some());
    assert!(app.record_started.is_some());
    let rec_path = app.recorder.as_ref().unwrap().path().to_path_buf();
    let (start_flash, start_kind) = app.active_flash().expect("recording start flash");
    assert_eq!(start_kind, FlashKind::Success);
    assert!(
        start_flash.contains("Recording telemetry (CSV) ->"),
        "unexpected start flash: {start_flash}"
    );

    // Pressing Shift+W while recording must not change format
    assert!(!app.handle_key(press(KeyCode::Char('W')), &transport, &hw));
    assert_eq!(app.record_format, RecordFormat::Csv);

    // Poll 3 telemetry samples while recording
    app.poll_telemetry(&transport, &hw);
    app.poll_telemetry(&transport, &hw);
    app.poll_telemetry(&transport, &hw);
    assert_eq!(app.recorder.as_ref().unwrap().stats().samples_written, 3);

    // Render UI and verify red REC badge and w:Rec keycap appear
    let backend = TestBackend::new(120, 36);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
    let rendered = buffer_text(&terminal);
    assert!(
        rendered.contains("● REC") && rendered.contains("(3)"),
        "expected REC badge with sample count in top bar:\n{rendered}"
    );
    assert!(
        rendered.contains("w:Rec"),
        "expected w:Rec keycap in footer:\n{rendered}"
    );

    // Press 'w' again (even from another tab!) to stop, flush, and save
    app.active_tab = ViewTab::Tune;
    assert!(!app.handle_key(press(KeyCode::Char('w')), &transport, &hw));
    assert!(app.recorder.is_none());
    assert!(app.record_started.is_none());
    let (stop_flash, stop_kind) = app.active_flash().expect("recording stop flash");
    assert_eq!(stop_kind, FlashKind::Success);
    assert!(
        stop_flash.contains("Saved 3 samples ->"),
        "unexpected stop flash: {stop_flash}"
    );

    // Verify saved CSV file contains header + 3 valid rows
    let content = std::fs::read_to_string(&rec_path).unwrap();
    let lines: Vec<&str> = content.lines().collect();
    assert_eq!(lines.len(), 4, "expected CSV header + 3 rows:\n{content}");
    assert_eq!(lines[0], recorder::CSV_HEADER);
    let expected_cols = recorder::CSV_HEADER.split(',').count();
    for row in &lines[1..] {
        assert_eq!(row.split(',').count(), expected_cols);
        assert!(row.contains("granite-ridge"), "row missing codename: {row}");
    }

    // When idle, Shift+W cycles CSV -> JSONL -> PTS -> CSV
    assert!(!app.handle_key(press(KeyCode::Char('W')), &transport, &hw));
    assert_eq!(app.record_format, RecordFormat::Jsonl);
    assert!(!app.handle_key(press(KeyCode::Char('W')), &transport, &hw));
    assert_eq!(app.record_format, RecordFormat::Pts);
    assert!(!app.handle_key(press(KeyCode::Char('W')), &transport, &hw));
    assert_eq!(app.record_format, RecordFormat::Csv);
}
