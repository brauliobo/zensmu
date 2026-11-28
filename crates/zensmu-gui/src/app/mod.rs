//! Application state, telemetry polling thread, and dark cockpit theme for `zensmu-gui`.

mod actions;
mod helpers;
mod polling;
mod state;
mod tuning;

#[cfg(test)]
mod tests;

pub use helpers::{
    clamp_ui_zoom, configure_dark_cockpit_theme, error_message_suggests_report,
    format_board_summary, open_url_in_browser,
};
pub use state::{
    BenchUiState, DryRunModal, PersistConfirmModal, PersistConfirmTarget, ProfileEditorState,
    RecorderUiState, ReportModalState, StatusToast, Tab, TelemetrySample, ToastLevel,
    POLL_INTERVAL_OPTIONS_MS, TELEMETRY_RING_CAPACITY, UI_ZOOM_DEFAULT, UI_ZOOM_MAX, UI_ZOOM_MIN,
    UI_ZOOM_STEP,
};
pub use tuning::{ApuEditorState, CoEditorState, PboEditorState, ShaperEditorState};

use crate::elevate::{is_running_as_root, CommandRunner};
use crate::views;
use eframe::egui;
use helpers::default_profile_template;
use state::TelemetryMsg;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zensmu_core::driver::{inspect_driver_status, DriverStatus};
use zensmu_core::hw::identity::{detect_hardware, HardwareContext};
use zensmu_core::metrics::{read_exporter_service_status, ExporterServiceStatus};
use zensmu_core::ops::co::CoSnapshot;
use zensmu_core::ops::pbo::PboStatus;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::report::SystemBoardInfo;
use zensmu_core::safety::guard::BootLoopGuard;
use zensmu_core::safety::persist::{read_persist_status, PersistPaths, PersistStatus};
use zensmu_core::safety::snapshot::SnapshotStore;
use zensmu_core::transport::sysfs::SysfsTransport;
use zensmu_core::types::CurveShaperState;

/// Top-level `eframe` application state.
pub struct ZensmuApp {
    pub is_root: bool,
    pub hw: Option<HardwareContext>,
    pub hw_error: Option<String>,
    pub hw_error_suggests_report: bool,
    pub hw_error_hint: Option<&'static str>,
    pub driver_status: DriverStatus,
    pub board_info: SystemBoardInfo,
    pub board_caps: BTreeMap<String, u32>,
    pub clamp_warning: Option<String>,
    suppress_next_status_toast: bool,
    pub active_tab: Tab,
    pub ui_zoom: f32,

    // Live unprivileged telemetry
    pub samples: VecDeque<TelemetrySample>,
    pub latest_telemetry: Option<TelemetrySnapshot>,
    pub telemetry_error: Option<String>,
    pub telemetry_error_suggests_report: bool,
    pub telemetry_error_hint: Option<&'static str>,
    pub poll_interval_ms: u64,
    pub telemetry_paused: bool,
    telemetry_rx: Option<Receiver<TelemetryMsg>>,
    telemetry_stop: Arc<AtomicBool>,
    telemetry_pause_flag: Arc<AtomicBool>,
    telemetry_interval_flag: Arc<AtomicU64>,
    start_time: Instant,

    // Privileged SMU state read via pkexec
    pub co_snapshot: Option<CoSnapshot>,
    pub shaper_state: Option<CurveShaperState>,
    pub pbo_status: Option<PboStatus>,

    // Per-tab editor buffers
    pub co_editor: CoEditorState,
    pub shaper_editor: ShaperEditorState,
    pub pbo_editor: PboEditorState,
    pub apu_editor: ApuEditorState,
    pub profile_editor: ProfileEditorState,

    // Unprivileged telemetry recording, Prometheus exporter & Core-Cycler tester
    pub recorder: RecorderUiState,
    pub exporter_status: ExporterServiceStatus,
    pub bench: BenchUiState,

    // Unprivileged boot/resume persistence status & confirmation modal
    pub persist_status: PersistStatus,
    pub persist_confirm: Option<PersistConfirmModal>,
    staging_persist_path: Option<PathBuf>,
    staging_profile_path: Option<PathBuf>,

    // Dry-run confirmation modal, report modal & async pkexec runner
    pub pending_modal: Option<DryRunModal>,
    pub report_modal: ReportModalState,
    pub runner: CommandRunner,
    pub toast: Option<StatusToast>,
}

impl ZensmuApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        configure_dark_cockpit_theme(&cc.egui_ctx);

        let is_root = is_running_as_root();
        let runner = CommandRunner::new(is_root);

        let mut app = Self {
            is_root,
            hw: None,
            hw_error: None,
            hw_error_suggests_report: false,
            hw_error_hint: None,
            driver_status: inspect_driver_status(Path::new(
                zensmu_core::transport::sysfs::DEFAULT_SYSFS_PATH,
            )),
            board_info: SystemBoardInfo::detect_linux(),
            board_caps: BTreeMap::new(),
            clamp_warning: None,
            suppress_next_status_toast: false,
            active_tab: Tab::Dashboard,
            ui_zoom: UI_ZOOM_DEFAULT,
            samples: VecDeque::with_capacity(TELEMETRY_RING_CAPACITY),
            latest_telemetry: None,
            telemetry_error: None,
            telemetry_error_suggests_report: false,
            telemetry_error_hint: None,
            poll_interval_ms: 500,
            telemetry_paused: false,
            telemetry_rx: None,
            telemetry_stop: Arc::new(AtomicBool::new(false)),
            telemetry_pause_flag: Arc::new(AtomicBool::new(false)),
            telemetry_interval_flag: Arc::new(AtomicU64::new(500)),
            start_time: Instant::now(),
            co_snapshot: None,
            shaper_state: None,
            pbo_status: None,
            co_editor: CoEditorState::default(),
            shaper_editor: ShaperEditorState::default(),
            pbo_editor: PboEditorState::default(),
            apu_editor: ApuEditorState::default(),
            profile_editor: ProfileEditorState::default(),
            recorder: RecorderUiState::with_detected_daemon_status(),
            exporter_status: read_exporter_service_status(),
            bench: BenchUiState::default(),
            persist_status: read_persist_status(&PersistPaths::default(), None, None),
            persist_confirm: None,
            staging_persist_path: None,
            staging_profile_path: None,
            pending_modal: None,
            report_modal: ReportModalState::default(),
            runner,
            toast: None,
        };

        app.set_ui_zoom(app.ui_zoom, &cc.egui_ctx);
        app.probe_hardware(&cc.egui_ctx);
        app.refresh_safety_files();
        app.refresh_persist_status();
        app
    }

    /// Set the interactive UI zoom factor (clamped to `0.85..=1.60`) and apply it to `egui`.
    pub fn set_ui_zoom(&mut self, zoom: f32, ctx: &egui::Context) {
        let clamped = clamp_ui_zoom(zoom);
        self.ui_zoom = clamped;
        ctx.set_zoom_factor(clamped);
    }

    /// Adjust the interactive UI zoom factor by `delta` (e.g. `±UI_ZOOM_STEP`).
    pub fn adjust_ui_zoom(&mut self, delta: f32, ctx: &egui::Context) {
        self.set_ui_zoom(self.ui_zoom + delta, ctx);
    }

    /// Handle global keyboard zoom shortcuts (`Ctrl +`/`=`, `Ctrl -`, `Ctrl 0`).
    fn handle_zoom_shortcuts(&mut self, ctx: &egui::Context) {
        let mut delta = 0.0_f32;
        let mut reset = false;

        ctx.input_mut(|i| {
            let cmd = egui::Modifiers::COMMAND;
            let cmd_shift = egui::Modifiers::COMMAND | egui::Modifiers::SHIFT;

            if i.consume_key(cmd, egui::Key::Plus)
                || i.consume_key(cmd_shift, egui::Key::Plus)
                || i.consume_key(cmd, egui::Key::Equals)
                || i.consume_key(cmd_shift, egui::Key::Equals)
            {
                delta += UI_ZOOM_STEP;
            }
            if i.consume_key(cmd, egui::Key::Minus) {
                delta -= UI_ZOOM_STEP;
            }
            if i.consume_key(cmd, egui::Key::Num0) {
                reset = true;
            }
        });

        if reset {
            self.set_ui_zoom(UI_ZOOM_DEFAULT, ctx);
        } else if delta != 0.0 {
            self.adjust_ui_zoom(delta, ctx);
        }
    }

    /// Probe `/sys/kernel/ryzen_smu_drv/` read-only attributes without root.
    pub fn probe_hardware(&mut self, ctx: &egui::Context) {
        self.stop_telemetry_thread();
        self.board_caps.clear();
        self.clamp_warning = None;

        let transport = SysfsTransport::default();
        self.driver_status = inspect_driver_status(transport.sysfs_dir());
        match detect_hardware(&transport, true) {
            Ok(hw) => {
                let core_count = hw.topology.physical_cores;
                self.co_editor.per_core = vec![0; core_count];
                self.pbo_editor.clear_dirty();
                self.apu_editor.clear_dirty();
                if self.profile_editor.toml_text.is_empty() {
                    self.profile_editor.toml_text = default_profile_template(&hw);
                }
                self.hw_error = None;
                self.hw_error_suggests_report = false;
                self.hw_error_hint = None;
                self.telemetry_error = None;
                self.telemetry_error_suggests_report = false;
                self.telemetry_error_hint = None;
                self.start_telemetry_thread(hw.clone(), ctx.clone());
                self.hw = Some(hw);
                self.set_toast(
                    ToastLevel::Success,
                    "Detected Ryzen SMU hardware (unprivileged telemetry active).",
                );
            }
            Err(err) => {
                self.hw = None;
                self.hw_error_suggests_report = err.suggests_report();
                self.hw_error_hint = err.report_hint();
                self.hw_error = Some(err.to_string());
                self.set_toast(
                    ToastLevel::Warning,
                    "ryzen_smu hardware probe unavailable — see banner above.",
                );
            }
        }
        self.refresh_persist_status();
    }

    /// Refresh read-only boot/resume persistence and systemd service status (unprivileged).
    pub fn refresh_persist_status(&mut self) {
        let host_codename = self.hw.as_ref().map(|h| h.codename);
        let host_cores = self.hw.as_ref().map(|h| h.topology.physical_cores);
        self.persist_status =
            read_persist_status(&PersistPaths::default(), host_codename, host_cores);
        self.profile_editor.boot_guard_tripped = self.persist_status.boot_guard_tripped;
    }

    /// Refresh on-disk snapshot and boot-loop guard indicators (read-only).
    pub fn refresh_safety_files(&mut self) {
        let guard = BootLoopGuard::default();
        self.profile_editor.boot_guard_tripped = guard.is_tripped();
        self.profile_editor.boot_guard_sentinel = guard.read_sentinel().ok().flatten();

        let store = SnapshotStore::default();
        self.profile_editor.has_boot_baseline = store.boot_baseline_path().exists();
        self.profile_editor.has_last_state = store.last_state_path().exists();
    }
}

impl Drop for ZensmuApp {
    fn drop(&mut self) {
        let _ = self.recorder.stop();
        self.telemetry_stop.store(true, Ordering::Relaxed);
        self.bench.stop_flag.store(true, Ordering::Relaxed);
        self.cleanup_staging_persist();
        self.cleanup_staging_profile();
    }
}

impl eframe::App for ZensmuApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_zoom_shortcuts(ctx);
        self.drain_telemetry();
        self.drain_bench();
        self.drain_cli_results(ctx);

        if self.recorder.is_recording() {
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if self.bench.running {
            ctx.request_repaint_after(Duration::from_millis(250));
        }

        views::render_top_bar(self, ctx);
        views::render_bottom_bar(self, ctx);
        views::render_sidebar(self, ctx);

        egui::CentralPanel::default()
            .frame(
                egui::Frame::central_panel(&ctx.style())
                    .fill(views::BG_CANVAS)
                    .inner_margin(egui::Margin::same(16)),
            )
            .show(ctx, |ui| {
                views::render_banners(self, ui, ctx);

                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.active_tab {
                        Tab::Dashboard => views::render_dashboard(self, ui),
                        Tab::CurveOptimizer => views::render_curve_optimizer(self, ui, ctx),
                        Tab::CurveShaper => views::render_curve_shaper(self, ui, ctx),
                        Tab::PboLimits => views::render_pbo_limits(self, ui, ctx),
                        Tab::ProfilesSafety => views::render_profiles_and_safety(self, ui, ctx),
                    });
            });

        views::render_dry_run_modal(self, ctx);
        views::render_persist_confirm_modal(self, ctx);
        views::render_report_modal(self, ctx);
    }
}
