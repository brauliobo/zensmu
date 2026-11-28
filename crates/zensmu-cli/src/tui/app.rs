//! Core TUI `App` state, polling, mailbox refresh, and live recording.

use super::format::clamp_interval_ms;
use super::history::TelemetryHistory;
use super::types::{ConfirmModal, FlashKind, MailboxState, TuneParam, TuneValue, ViewTab};
use super::{HISTORY_CAPACITY, UNPRIV_MAILBOX_NOTE};
use std::collections::{BTreeMap, HashMap};
#[cfg(test)]
use std::path::PathBuf;
use std::time::{Duration, Instant};
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::ops::co::{self, CoSnapshot};
use zensmu_core::ops::pbo::{self, PboStatus};
use zensmu_core::ops::pm_table::{self, TelemetrySnapshot};
use zensmu_core::ops::shaper;
use zensmu_core::recorder::{self, RecordConfig, RecordFormat, TelemetryRecorder};
use zensmu_core::report::SystemBoardInfo;
use zensmu_core::safety::persist::{self, PersistPaths, PersistStatus};
use zensmu_core::safety::snapshot::SnapshotStore;
use zensmu_core::transport::SmuTransport;
use zensmu_core::types::CurveShaperState;

pub(super) struct App {
    pub(super) active_tab: ViewTab,
    pub(super) show_help: bool,
    pub(super) paused: bool,
    pub(super) interval_ms: u64,
    pub(super) sample_count: u64,
    pub(super) is_root: bool,
    #[cfg(test)]
    pub(super) test_allow_writes: bool,
    pub(super) telemetry: Option<TelemetrySnapshot>,
    pub(super) telemetry_error: Option<String>,
    pub(super) history: TelemetryHistory,
    pub(super) co_state: MailboxState<CoSnapshot>,
    pub(super) shaper_state: MailboxState<CurveShaperState>,
    pub(super) pbo_state: MailboxState<PboStatus>,
    pub(super) mailbox_note: Option<String>,
    pub(super) status_flash: Option<(String, FlashKind, Instant)>,
    pub(super) last_alert: Option<(String, FlashKind)>,
    pub(super) board_info: SystemBoardInfo,
    // Interactive Tune state (Tab 4)
    pub(super) tune_selected: usize,
    pub(super) staged: HashMap<TuneParam, TuneValue>,
    pub(super) board_caps: BTreeMap<TuneParam, u32>,
    pub(super) allow_extreme: bool,
    pub(super) confirm_modal: Option<ConfirmModal>,
    pub(super) snapshot_store: SnapshotStore,
    pub(super) persist_paths: PersistPaths,
    pub(super) persist_status: PersistStatus,
    // Live telemetry recording (unprivileged)
    pub(super) recorder: Option<TelemetryRecorder>,
    pub(super) record_started: Option<Instant>,
    pub(super) record_format: RecordFormat,
    #[cfg(test)]
    pub(super) record_dir_override: Option<PathBuf>,
}

pub(super) fn default_persist_status(paths: &PersistPaths) -> PersistStatus {
    PersistStatus {
        active_profile_path: paths.active_profile.display().to_string(),
        active_profile_exists: false,
        active_profile_valid: None,
        active_profile_error: None,
        active_profile_codename: None,
        modules_load_configured: false,
        service_name: paths.service_name.clone(),
        service_installed: false,
        service_enabled: false,
        service_state: "unknown".to_string(),
        resume_hook_installed: false,
        boot_guard_tripped: false,
    }
}

impl App {
    pub(super) fn new(interval_ms: u64) -> Self {
        let clamped = clamp_interval_ms(interval_ms);
        let is_root = crate::doctor::is_effective_root();
        let persist_paths = PersistPaths::default();
        let persist_status = default_persist_status(&persist_paths);
        Self {
            active_tab: ViewTab::Overview,
            show_help: false,
            paused: false,
            interval_ms: clamped,
            sample_count: 0,
            is_root,
            #[cfg(test)]
            test_allow_writes: false,
            telemetry: None,
            telemetry_error: None,
            history: TelemetryHistory::new(HISTORY_CAPACITY),
            co_state: MailboxState::Unsupported,
            shaper_state: MailboxState::Unsupported,
            pbo_state: MailboxState::Unsupported,
            mailbox_note: None,
            status_flash: None,
            last_alert: None,
            board_info: crate::output::detected_board_info().clone(),
            tune_selected: 0,
            staged: HashMap::new(),
            board_caps: BTreeMap::new(),
            allow_extreme: false,
            confirm_modal: None,
            snapshot_store: SnapshotStore::default(),
            persist_paths,
            persist_status,
            recorder: None,
            record_started: None,
            record_format: RecordFormat::Csv,
            #[cfg(test)]
            record_dir_override: None,
        }
    }

    pub(super) fn refresh_persist_status(&mut self, hw: &HardwareContext) {
        self.persist_status = persist::read_persist_status(
            &self.persist_paths,
            Some(hw.codename),
            Some(hw.topology.physical_cores),
        );
    }

    pub(super) fn can_write(&self) -> bool {
        #[cfg(test)]
        {
            self.is_root || self.test_allow_writes
        }
        #[cfg(not(test))]
        {
            self.is_root
        }
    }

    pub(super) fn set_flash(&mut self, msg: impl Into<String>) {
        self.set_flash_kind(msg, FlashKind::Info);
    }

    pub(super) fn set_flash_kind(&mut self, msg: impl Into<String>, kind: FlashKind) {
        self.status_flash = Some((msg.into(), kind, Instant::now()));
    }

    pub(super) fn active_flash(&self) -> Option<(&str, FlashKind)> {
        let (msg, kind, ts) = self.status_flash.as_ref()?;
        let ttl = match kind {
            FlashKind::Info => Duration::from_secs(3),
            FlashKind::Success | FlashKind::Warn | FlashKind::Error => Duration::from_secs(6),
        };
        if ts.elapsed() < ttl {
            Some((msg.as_str(), *kind))
        } else {
            None
        }
    }

    pub(super) fn staged_count(&self) -> usize {
        self.staged.len()
    }

    pub(super) fn poll_telemetry(&mut self, transport: &dyn SmuTransport, hw: &HardwareContext) {
        match pm_table::read_telemetry(transport, hw) {
            Ok(snap) => {
                self.history.record(&snap);
                if let Some(ref mut rec) = self.recorder {
                    if let Err(err) = rec.write_snapshot(&snap) {
                        self.recorder = None;
                        self.record_started = None;
                        self.set_flash_kind(
                            format!("Recording stopped (write error: {err})"),
                            FlashKind::Error,
                        );
                    }
                }
                self.telemetry = Some(snap);
                self.telemetry_error = None;
                self.sample_count = self.sample_count.saturating_add(1);
            }
            Err(err) => {
                self.telemetry_error = Some(err.to_string());
            }
        }
    }

    pub(super) fn toggle_recording(&mut self, hw: &HardwareContext) {
        if let Some(mut rec) = self.recorder.take() {
            self.record_started = None;
            let flush_res = rec.flush();
            let stats = rec.stats();
            match flush_res {
                Ok(()) => {
                    self.set_flash_kind(
                        format!(
                            "Saved {} samples -> {}",
                            stats.samples_written,
                            stats.path.display()
                        ),
                        FlashKind::Success,
                    );
                }
                Err(err) => {
                    self.set_flash_kind(format!("Recording flush error: {err}"), FlashKind::Error);
                }
            }
            return;
        }

        #[cfg(test)]
        let path = if let Some(ref dir) = self.record_dir_override {
            dir.join(format!(
                "zensmu-{}.{}",
                hw.codename.slug(),
                self.record_format.extension()
            ))
        } else {
            recorder::default_recording_path(hw.codename, self.record_format)
        };
        #[cfg(not(test))]
        let path = recorder::default_recording_path(hw.codename, self.record_format);

        let config = RecordConfig {
            path,
            format: self.record_format,
            append: false,
            max_file_bytes: None,
            max_rotated_files: 0,
            flush_every: 1,
            tag: None,
        };

        match TelemetryRecorder::open(config) {
            Ok(rec) => {
                let path_str = rec.path().display().to_string();
                let fmt_label = self.record_format.as_str().to_ascii_uppercase();
                self.recorder = Some(rec);
                self.record_started = Some(Instant::now());
                self.set_flash_kind(
                    format!("Recording telemetry ({fmt_label}) -> {path_str}"),
                    FlashKind::Success,
                );
            }
            Err(err) => {
                self.set_flash_kind(
                    format!("Failed to start recording: {err}"),
                    FlashKind::Error,
                );
            }
        }
    }

    pub(super) fn cycle_record_format(&mut self) {
        if self.recorder.is_some() {
            self.set_flash_kind("Stop recording (w) before changing format", FlashKind::Warn);
            return;
        }
        self.record_format = match self.record_format {
            RecordFormat::Csv => RecordFormat::Jsonl,
            RecordFormat::Jsonl => RecordFormat::Pts,
            RecordFormat::Pts => RecordFormat::Csv,
        };
        self.set_flash(format!(
            "Record format: {}",
            self.record_format.as_str().to_ascii_uppercase()
        ));
    }

    pub(super) fn refresh_mailbox(&mut self, transport: &dyn SmuTransport, hw: &HardwareContext) {
        self.co_state = MailboxState::from_result(co::get_co(transport, hw));
        self.shaper_state = MailboxState::from_result(shaper::get_curve_shaper(transport, hw));
        self.pbo_state = MailboxState::from_result(pbo::get_pbo_status(transport, hw));

        let any_denied = self.co_state.is_permission_denied()
            || self.shaper_state.is_permission_denied()
            || self.pbo_state.is_permission_denied();

        if any_denied {
            self.mailbox_note = Some(UNPRIV_MAILBOX_NOTE.to_string());
        } else {
            self.mailbox_note = None;
        }

        // Remove any staged Curve Shaper points if firmware doesn't expose it
        if matches!(self.shaper_state, MailboxState::FirmwareUnsupported(_)) {
            self.staged
                .retain(|p, _| !matches!(p, TuneParam::Shaper { .. }));
        }

        self.clamp_tune_selection(hw);
    }

    pub(super) fn has_any_mailbox_data(&self) -> bool {
        self.co_state.as_loaded().is_some()
            || self.shaper_state.as_loaded().is_some()
            || self.pbo_state.as_loaded().is_some()
    }
}
