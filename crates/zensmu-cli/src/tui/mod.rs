//! `btop`/`htop`-inspired live terminal UI (`zensmu top`).
//!
//! Works unprivileged out of the box by polling the read-only `pm_table` sysfs
//! endpoint (`0444`). When elevated privileges are available, also reads back
//! Curve Optimizer margins, Zen 5 Curve Shaper matrix, and PBO status/fused
//! limits via the SMU mailbox.

mod actions;
mod app;
mod format;
mod history;
mod input;
mod palette;
mod plan;
mod render;
mod terminal;
mod tuning;
mod types;

#[cfg(test)]
mod tests;

use app::App;
use crossterm::event::{self, Event};
use render::draw_ui;
use std::time::{Duration, Instant};
use terminal::{tty_io_err, TerminalGuard};

/// Minimum allowed telemetry polling interval (ms).
pub const MIN_INTERVAL_MS: u64 = 100;
/// Maximum allowed telemetry polling interval (ms).
pub const MAX_INTERVAL_MS: u64 = 10_000;
/// Number of historical telemetry samples retained per metric.
pub const HISTORY_CAPACITY: usize = 120;

pub(super) const UNPRIV_MAILBOX_NOTE: &str =
    "Run with sudo for Curve Optimizer / Shaper / PBO mailbox readback";

// ---------------------------------------------------------------------------
// Public entrypoint (FROZEN SIGNATURE)
// ---------------------------------------------------------------------------

/// Launch the interactive `btop`/`htop`-inspired `zensmu top` live TUI.
pub fn run_tui(
    transport: &dyn zensmu_core::transport::SmuTransport,
    hw: &zensmu_core::hw::identity::HardwareContext,
    interval_ms: u64,
) -> zensmu_core::Result<()> {
    let mut guard = TerminalGuard::new()?;
    let mut app = App::new(interval_ms);

    // Initial unprivileged PM table read + best-effort mailbox & persistence readback.
    app.poll_telemetry(transport, hw);
    app.refresh_mailbox(transport, hw);
    app.refresh_persist_status(hw);

    let mut last_tick = Instant::now();

    loop {
        guard
            .terminal
            .draw(|frame| draw_ui(frame, &app, hw))
            .map_err(tty_io_err)?;

        let tick_duration = Duration::from_millis(app.interval_ms);
        let timeout = tick_duration
            .checked_sub(last_tick.elapsed())
            .unwrap_or(Duration::ZERO)
            .min(Duration::from_millis(100));

        if event::poll(timeout).map_err(tty_io_err)? {
            if let Event::Key(key) = event::read().map_err(tty_io_err)? {
                if app.handle_key(key, transport, hw) {
                    break;
                }
            }
        }

        if !app.paused && last_tick.elapsed() >= tick_duration {
            app.poll_telemetry(transport, hw);
            last_tick = Instant::now();
        }
    }

    if let Some(mut rec) = app.recorder.take() {
        let _ = rec.flush();
    }

    Ok(())
}
