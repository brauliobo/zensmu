//! Terminal safety: RAII `TerminalGuard` + one-time panic hook.

use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use zensmu_core::error::Error;

// ---------------------------------------------------------------------------
// Terminal safety: RAII TerminalGuard + one-time panic hook
// ---------------------------------------------------------------------------

pub(super) static PANIC_HOOK_INSTALLED: Once = Once::new();
pub(super) static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

pub(super) fn install_panic_hook_once() {
    PANIC_HOOK_INSTALLED.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal_best_effort();
            prev(info);
        }));
    });
}

pub(super) fn restore_terminal_best_effort() {
    if TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = execute!(out, LeaveAlternateScreen, Show);
        let _ = out.flush();
    }
}

pub(super) fn tty_io_err(source: io::Error) -> Error {
    Error::Io {
        path: "tty".to_string(),
        source,
    }
}

/// RAII guard that enters raw mode and alternate screen on creation and
/// unconditionally restores terminal state on drop, error, or panic.
pub(super) struct TerminalGuard {
    pub(super) terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    pub(super) fn new() -> zensmu_core::Result<Self> {
        install_panic_hook_once();
        enable_raw_mode().map_err(tty_io_err)?;

        let mut stdout = io::stdout();
        if let Err(err) = execute!(stdout, EnterAlternateScreen, Hide) {
            let _ = disable_raw_mode();
            return Err(tty_io_err(err));
        }

        TERMINAL_ACTIVE.store(true, Ordering::SeqCst);

        let backend = CrosstermBackend::new(stdout);
        let mut terminal = match Terminal::new(backend) {
            Ok(t) => t,
            Err(err) => {
                restore_terminal_best_effort();
                return Err(tty_io_err(err));
            }
        };
        if let Err(err) = terminal.clear() {
            restore_terminal_best_effort();
            return Err(tty_io_err(err));
        }

        Ok(Self { terminal })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal_best_effort();
    }
}
