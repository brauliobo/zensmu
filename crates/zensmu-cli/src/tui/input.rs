//! Keyboard input handling for tabs, tuning controls, and confirmation modals.

use super::app::App;
use super::format::adjust_interval_ms;
use super::types::{FlashKind, ViewTab};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::transport::SmuTransport;

impl App {
    pub(super) fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.set_flash(if self.paused {
            "Telemetry paused"
        } else {
            "Telemetry resumed"
        });
    }

    pub(super) fn refresh_all(&mut self, transport: &dyn SmuTransport, hw: &HardwareContext) {
        self.poll_telemetry(transport, hw);
        self.refresh_mailbox(transport, hw);
        self.refresh_persist_status(hw);
        if self.mailbox_note.is_some() {
            self.set_flash("Telemetry refreshed (SMU mailbox requires sudo)");
        } else {
            self.set_flash("SMU mailbox, telemetry & persistence refreshed");
        }
    }

    pub(super) fn handle_modal_key(
        &mut self,
        code: KeyCode,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) {
        match code {
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                self.execute_confirmed_modal(transport, hw);
            }
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                let was_persist = self
                    .confirm_modal
                    .as_ref()
                    .is_some_and(|m| m.action.is_persist_action());
                self.confirm_modal = None;
                if was_persist {
                    self.set_flash("Persistence action cancelled");
                } else {
                    self.set_flash("Dry-run cancelled (no SMU writes sent)");
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(m) = self.confirm_modal.as_mut() {
                    m.scroll = m.scroll.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(m) = self.confirm_modal.as_mut() {
                    let max_scroll = m.item_count().saturating_sub(1);
                    m.scroll = (m.scroll + 1).min(max_scroll);
                }
            }
            _ => {}
        }
    }

    /// Returns `Some(should_quit)` if the key was handled as a global command.
    pub(super) fn handle_global_key(
        &mut self,
        code: KeyCode,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) -> Option<bool> {
        match code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return Some(true),
            KeyCode::Char('?') => self.show_help = !self.show_help,
            KeyCode::Esc => self.show_help = false,
            KeyCode::Tab => {
                self.show_help = false;
                self.active_tab = self.active_tab.next();
            }
            KeyCode::BackTab => {
                self.show_help = false;
                self.active_tab = self.active_tab.prev();
            }
            KeyCode::Char('1') => {
                self.show_help = false;
                self.active_tab = ViewTab::Overview;
            }
            KeyCode::Char('2') => {
                self.show_help = false;
                self.active_tab = ViewTab::ClocksVolts;
            }
            KeyCode::Char('3') => {
                self.show_help = false;
                self.active_tab = ViewTab::CoShaper;
            }
            KeyCode::Char('4') | KeyCode::Char('t') => {
                self.show_help = false;
                self.active_tab = ViewTab::Tune;
                self.clamp_tune_selection(hw);
            }
            KeyCode::Char('p') | KeyCode::Char(' ') => self.toggle_pause(),
            KeyCode::Char('w') => self.toggle_recording(hw),
            KeyCode::Char('W') => self.cycle_record_format(),
            KeyCode::Char('r') => self.refresh_all(transport, hw),
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.interval_ms = adjust_interval_ms(self.interval_ms, true);
                self.set_flash(format!("Interval: {}ms", self.interval_ms));
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.interval_ms = adjust_interval_ms(self.interval_ms, false);
                self.set_flash(format!("Interval: {}ms", self.interval_ms));
            }
            _ => return None,
        }
        Some(false)
    }

    pub(super) fn handle_tune_key(
        &mut self,
        key: KeyEvent,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) {
        let shifted = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.tune_selected = self.tune_selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') => {
                let count = self.tune_params(hw).len();
                if count > 0 {
                    self.tune_selected = (self.tune_selected + 1).min(count - 1);
                }
            }
            KeyCode::Char('[') => self.jump_tune_section(hw, false),
            KeyCode::Char(']') => self.jump_tune_section(hw, true),
            KeyCode::Char('H') | KeyCode::PageDown => self.adjust_selected_param(hw, -1, true),
            KeyCode::Char('L') | KeyCode::PageUp => self.adjust_selected_param(hw, 1, true),
            KeyCode::Left => self.adjust_selected_param(hw, -1, shifted),
            KeyCode::Right => self.adjust_selected_param(hw, 1, shifted),
            KeyCode::Char('h') => self.adjust_selected_param(hw, -1, false),
            KeyCode::Char('l') => self.adjust_selected_param(hw, 1, false),
            KeyCode::Backspace => self.reset_selected_param(hw, false),
            KeyCode::Char('0') => self.reset_selected_param(hw, true),
            KeyCode::Char('u') => {
                let n = self.staged.len();
                self.staged.clear();
                if n > 0 {
                    self.set_flash(format!("Discarded {n} staged change(s)"));
                } else {
                    self.set_flash("No staged changes to discard");
                }
            }
            KeyCode::Char('x') => {
                self.allow_extreme = !self.allow_extreme;
                if self.allow_extreme {
                    self.set_flash_kind(
                        "Allow Extreme ON (hard bounds & 95°C Tctl still enforced)",
                        FlashKind::Warn,
                    );
                } else {
                    self.set_flash("Allow Extreme OFF (soft safety thresholds active)");
                }
            }
            KeyCode::Char('a') | KeyCode::Enter => self.open_staged_dry_run_modal(transport, hw),
            KeyCode::Char('R') => self.open_snapshot_restore_modal(transport, hw, false),
            KeyCode::Char('B') => self.open_snapshot_restore_modal(transport, hw, true),
            KeyCode::Char('Z') => self.open_reset_co_zero_modal(transport, hw),
            KeyCode::Char('s') => self.save_live_profile(transport, hw),
            KeyCode::Char('P') => self.open_persist_live_modal(),
            KeyCode::Char('E') => self.open_toggle_service_modal(hw),
            KeyCode::Char('C') => self.open_clear_boot_guard_modal(),
            _ => {}
        }
    }

    pub(super) fn handle_non_tune_key(
        &mut self,
        code: KeyCode,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) {
        match code {
            KeyCode::Right | KeyCode::Char('l') => self.active_tab = self.active_tab.next(),
            KeyCode::Left | KeyCode::Char('h') => self.active_tab = self.active_tab.prev(),
            KeyCode::Char('P') => self.toggle_pause(),
            KeyCode::Char('R') => self.refresh_all(transport, hw),
            _ => {}
        }
    }

    /// Handle a key press. Returns `true` when the application should exit.
    pub(super) fn handle_key(
        &mut self,
        key: KeyEvent,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) -> bool {
        if key.kind != KeyEventKind::Press {
            return false;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c')) {
            return true;
        }

        // Modal key handling takes highest precedence (live telemetry keeps ticking in run_tui!)
        if self.confirm_modal.is_some() {
            self.handle_modal_key(key.code, transport, hw);
            return false;
        }

        if let Some(should_quit) = self.handle_global_key(key.code, transport, hw) {
            return should_quit;
        }

        if self.show_help {
            return false;
        }

        if self.active_tab == ViewTab::Tune {
            self.handle_tune_key(key, transport, hw);
        } else {
            self.handle_non_tune_key(key.code, transport, hw);
        }

        false
    }
}
