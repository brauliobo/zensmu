//! Application state, telemetry polling thread, and dark cockpit theme for `zensmu-gui`.

use crate::elevate::{is_running_as_root, CliAction, CommandRunner, ElevatedResult, StatusPayload};
use crate::views;
use eframe::egui;
use std::collections::VecDeque;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use zensmu_core::config::profile::{Profile, ProfileDiffEntry, PROFILE_SCHEMA_VERSION};
use zensmu_core::hw::identity::{detect_hardware, HardwareContext};
use zensmu_core::ops::co::CoSnapshot;
use zensmu_core::ops::pbo::{PboSetRequest, PboStatus, TCTL_MAX_C};
use zensmu_core::ops::pm_table::{self, TelemetrySnapshot};
use zensmu_core::safety::guard::{BootLoopGuard, SentinelInfo};
use zensmu_core::safety::snapshot::{SmuStateSnapshot, SnapshotStore};
use zensmu_core::transport::sysfs::SysfsTransport;
use zensmu_core::types::{CurveShaperState, PlannedOp};

/// Ring buffer depth for live Dashboard plots (180 samples = 90s at 500ms).
pub const TELEMETRY_RING_CAPACITY: usize = 180;

/// Available telemetry polling intervals in milliseconds.
pub const POLL_INTERVAL_OPTIONS_MS: [u64; 4] = [250, 500, 1000, 2000];

/// Interactive UI zoom / font scale bounds and step.
pub const UI_ZOOM_DEFAULT: f32 = 1.0;
pub const UI_ZOOM_MIN: f32 = 0.85;
pub const UI_ZOOM_MAX: f32 = 1.60;
pub const UI_ZOOM_STEP: f32 = 0.10;

/// Main navigation tabs in the left sidebar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tab {
    Dashboard,
    CurveOptimizer,
    CurveShaper,
    PboLimits,
    ProfilesSafety,
}

/// Timestamped telemetry sample stored in the Dashboard ring buffer.
#[derive(Debug, Clone)]
pub struct TelemetrySample {
    pub elapsed_s: f64,
    pub snapshot: TelemetrySnapshot,
}

/// Message sent from the unprivileged telemetry poller thread to the UI.
enum TelemetryMsg {
    Sample(TelemetrySample),
    Error(String),
}

/// Toast severity for the bottom status bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct StatusToast {
    pub level: ToastLevel,
    pub message: String,
    pub created_at: Instant,
}

/// Dry-run confirmation modal displaying every planned SMU operation before apply.
#[derive(Debug, Clone)]
pub struct DryRunModal {
    pub title: String,
    pub ops: Vec<PlannedOp>,
    pub live_action: CliAction,
}

/// Editable Curve Optimizer UI state.
#[derive(Debug, Clone)]
pub struct CoEditorState {
    pub all_core: i32,
    pub per_core: Vec<i32>,
    pub igpu: i32,
    pub allow_extreme: bool,
}

impl Default for CoEditorState {
    fn default() -> Self {
        Self {
            all_core: 0,
            per_core: Vec::new(),
            igpu: 0,
            allow_extreme: false,
        }
    }
}

/// Editable Curve Shaper (Zen 5) UI state.
#[derive(Debug, Clone, Default)]
pub struct ShaperEditorState {
    pub draft: CurveShaperState,
    pub allow_extreme: bool,
}

/// Editable PBO Limits UI state.
#[derive(Debug, Clone)]
pub struct PboEditorState {
    pub ppt_enabled: bool,
    pub ppt_w: u32,
    pub tdc_enabled: bool,
    pub tdc_a: u32,
    pub edc_enabled: bool,
    pub edc_a: u32,
    pub tctl_enabled: bool,
    pub tctl_c: u32,
    pub scalar_enabled: bool,
    pub scalar: u32,
    pub fmax_enabled: bool,
    pub fmax_mhz: u32,
    pub allow_extreme: bool,
    pub seeded_from_telemetry: bool,
}

impl Default for PboEditorState {
    fn default() -> Self {
        Self {
            ppt_enabled: true,
            ppt_w: 88,
            tdc_enabled: true,
            tdc_a: 75,
            edc_enabled: true,
            edc_a: 150,
            tctl_enabled: true,
            tctl_c: 85,
            scalar_enabled: false,
            scalar: 1,
            fmax_enabled: false,
            fmax_mhz: 5500,
            allow_extreme: false,
            seeded_from_telemetry: false,
        }
    }
}

impl PboEditorState {
    pub fn build_request(&self) -> PboSetRequest {
        PboSetRequest {
            ppt_w: self.ppt_enabled.then_some(self.ppt_w),
            tdc_a: self.tdc_enabled.then_some(self.tdc_a),
            edc_a: self.edc_enabled.then_some(self.edc_a),
            tctl_c: self.tctl_enabled.then_some(self.tctl_c.min(TCTL_MAX_C)),
            scalar: self.scalar_enabled.then_some(self.scalar),
            fmax_mhz: self.fmax_enabled.then_some(self.fmax_mhz),
        }
    }

    pub fn has_any_enabled(&self) -> bool {
        self.ppt_enabled
            || self.tdc_enabled
            || self.edc_enabled
            || self.tctl_enabled
            || self.scalar_enabled
            || self.fmax_enabled
    }
}

/// Editable Profile & Safety UI state.
#[derive(Debug, Clone)]
pub struct ProfileEditorState {
    pub path: String,
    pub toml_text: String,
    pub validation: Option<Result<String, String>>,
    pub diff_entries: Vec<ProfileDiffEntry>,
    pub allow_extreme: bool,
    pub boot_guard_tripped: bool,
    pub boot_guard_sentinel: Option<SentinelInfo>,
    pub has_boot_baseline: bool,
    pub has_last_state: bool,
}

impl Default for ProfileEditorState {
    fn default() -> Self {
        Self {
            path: "/etc/zensmu/active.toml".into(),
            toml_text: String::new(),
            validation: None,
            diff_entries: Vec::new(),
            allow_extreme: false,
            boot_guard_tripped: false,
            boot_guard_sentinel: None,
            has_boot_baseline: false,
            has_last_state: false,
        }
    }
}

/// Top-level `eframe` application state.
pub struct ZensmuApp {
    pub is_root: bool,
    pub hw: Option<HardwareContext>,
    pub hw_error: Option<String>,
    pub active_tab: Tab,
    pub ui_zoom: f32,

    // Live unprivileged telemetry
    pub samples: VecDeque<TelemetrySample>,
    pub latest_telemetry: Option<TelemetrySnapshot>,
    pub telemetry_error: Option<String>,
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
    pub profile_editor: ProfileEditorState,

    // Dry-run confirmation modal & async pkexec runner
    pub pending_modal: Option<DryRunModal>,
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
            active_tab: Tab::Dashboard,
            ui_zoom: UI_ZOOM_DEFAULT,
            samples: VecDeque::with_capacity(TELEMETRY_RING_CAPACITY),
            latest_telemetry: None,
            telemetry_error: None,
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
            profile_editor: ProfileEditorState::default(),
            pending_modal: None,
            runner,
            toast: None,
        };

        app.set_ui_zoom(app.ui_zoom, &cc.egui_ctx);
        app.probe_hardware(&cc.egui_ctx);
        app.refresh_safety_files();
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

        let transport = SysfsTransport::default();
        match detect_hardware(&transport, true) {
            Ok(hw) => {
                let core_count = hw.topology.physical_cores;
                self.co_editor.per_core = vec![0; core_count];
                if self.profile_editor.toml_text.is_empty() {
                    self.profile_editor.toml_text = default_profile_template(&hw);
                }
                self.hw_error = None;
                self.telemetry_error = None;
                self.start_telemetry_thread(hw.clone(), ctx.clone());
                self.hw = Some(hw);
                self.set_toast(
                    ToastLevel::Success,
                    "Detected Ryzen SMU hardware (unprivileged telemetry active).",
                );
            }
            Err(err) => {
                self.hw = None;
                self.hw_error = Some(err.to_string());
                self.set_toast(
                    ToastLevel::Warning,
                    "ryzen_smu hardware probe unavailable — see banner above.",
                );
            }
        }
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

    fn stop_telemetry_thread(&mut self) {
        self.telemetry_stop.store(true, Ordering::Relaxed);
        self.telemetry_rx = None;
    }

    fn start_telemetry_thread(&mut self, hw: HardwareContext, ctx: egui::Context) {
        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(self.telemetry_paused));
        let interval_ms = Arc::new(AtomicU64::new(self.poll_interval_ms));
        let (tx, rx) = mpsc::channel();
        let start_time = self.start_time;

        self.telemetry_stop = Arc::clone(&stop);
        self.telemetry_pause_flag = Arc::clone(&pause);
        self.telemetry_interval_flag = Arc::clone(&interval_ms);
        self.telemetry_rx = Some(rx);

        thread::spawn(move || {
            let transport = SysfsTransport::default();
            while !stop.load(Ordering::Relaxed) {
                if !pause.load(Ordering::Relaxed) {
                    let msg = match pm_table::read_telemetry(&transport, &hw) {
                        Ok(snapshot) => TelemetryMsg::Sample(TelemetrySample {
                            elapsed_s: start_time.elapsed().as_secs_f64(),
                            snapshot,
                        }),
                        Err(e) => TelemetryMsg::Error(e.to_string()),
                    };
                    if tx.send(msg).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }

                let sleep_ms = interval_ms.load(Ordering::Relaxed).clamp(100, 10_000);
                let mut slept = 0u64;
                while slept < sleep_ms && !stop.load(Ordering::Relaxed) {
                    let step = (sleep_ms - slept).min(50);
                    thread::sleep(Duration::from_millis(step));
                    slept += step;
                }
            }
        });
    }

    pub fn set_telemetry_paused(&mut self, paused: bool) {
        self.telemetry_paused = paused;
        self.telemetry_pause_flag.store(paused, Ordering::Relaxed);
    }

    pub fn set_poll_interval_ms(&mut self, ms: u64) {
        self.poll_interval_ms = ms;
        self.telemetry_interval_flag.store(ms, Ordering::Relaxed);
    }

    pub fn set_toast(&mut self, level: ToastLevel, message: impl Into<String>) {
        self.toast = Some(StatusToast {
            level,
            message: message.into(),
            created_at: Instant::now(),
        });
    }

    /// Dispatch a `zensmu` CLI action on the background thread.
    pub fn dispatch_cli(&mut self, action: CliAction, ctx: &egui::Context) {
        if self.runner.is_busy() {
            self.set_toast(
                ToastLevel::Warning,
                "Another SMU command is already running — please wait.",
            );
            return;
        }
        self.runner.dispatch(action, ctx.clone());
    }

    /// Validate the TOML buffer in-process against the detected hardware without root.
    pub fn validate_profile_buffer(&mut self) {
        let Some(hw) = &self.hw else {
            self.profile_editor.validation = Some(Err(
                "Cannot validate profile: no Ryzen SMU hardware detected.".into(),
            ));
            return;
        };
        match Profile::from_toml_str(&self.profile_editor.toml_text) {
            Ok(profile) => match profile.validate_for(
                hw.codename,
                hw.topology.physical_cores,
                self.profile_editor.allow_extreme,
            ) {
                Ok(()) => {
                    let msg = format!(
                        "Valid profile for {} ({} cores).",
                        hw.codename.display_name(),
                        hw.topology.physical_cores
                    );
                    self.profile_editor.validation = Some(Ok(msg.clone()));
                    self.set_toast(ToastLevel::Success, msg);
                }
                Err(e) => {
                    let msg = e.to_string();
                    self.profile_editor.validation = Some(Err(msg.clone()));
                    self.set_toast(ToastLevel::Error, msg);
                }
            },
            Err(e) => {
                let msg = format!("TOML syntax/schema error: {e}");
                self.profile_editor.validation = Some(Err(msg.clone()));
                self.set_toast(ToastLevel::Error, msg);
            }
        }
    }

    /// Compute a diff between the TOML buffer and the latest known hardware state.
    pub fn compute_profile_diff(&mut self) {
        let Some(hw) = &self.hw else {
            self.set_toast(ToastLevel::Warning, "No hardware detected to diff against.");
            return;
        };
        let profile = match Profile::from_toml_str(&self.profile_editor.toml_text) {
            Ok(p) => p,
            Err(e) => {
                self.set_toast(ToastLevel::Error, format!("Invalid TOML: {e}"));
                return;
            }
        };

        let mut snap = SmuStateSnapshot::new(hw.codename, hw.topology.physical_cores);
        if let Some(co) = &self.co_snapshot {
            snap.co_per_core = Some(co.cores.iter().map(|c| (c.core_index, c.margin)).collect());
            snap.igpu_co = co.igpu;
        }
        snap.curve_shaper = self.shaper_state;
        if let Some(pbo) = &self.pbo_status {
            snap.pbo_scalar = pbo.scalar;
            snap.fmax_mhz = pbo.fmax_mhz;
        }
        if let Some(telem) = &self.latest_telemetry {
            snap.ppt_w = telem.ppt_limit_w.map(|v| v.round() as u32);
            snap.tdc_a = telem.tdc_limit_a.map(|v| v.round() as u32);
            snap.edc_a = telem.edc_limit_a.map(|v| v.round() as u32);
            snap.tctl_c = telem.tctl_limit_c.map(|v| v.round() as u32);
        }

        let diffs = profile.diff_against(&snap);
        let count = diffs.len();
        self.profile_editor.diff_entries = diffs;
        self.set_toast(
            ToastLevel::Info,
            if count == 0 {
                "Profile matches current known SMU state (0 differences).".into()
            } else {
                format!("Computed {count} difference(s) against current state.")
            },
        );
    }

    /// Load a TOML file from `profile_editor.path` into the editor.
    pub fn load_profile_from_disk(&mut self) {
        let path = PathBuf::from(self.profile_editor.path.trim());
        match fs::read_to_string(&path) {
            Ok(content) => {
                self.profile_editor.toml_text = content;
                self.profile_editor.validation = None;
                self.profile_editor.diff_entries.clear();
                self.set_toast(
                    ToastLevel::Success,
                    format!("Loaded profile from '{}'.", path.display()),
                );
            }
            Err(e) => {
                self.set_toast(
                    ToastLevel::Error,
                    format!("Failed to read '{}': {e}", path.display()),
                );
            }
        }
    }

    /// Write the editor TOML buffer to a temporary file and run `profile apply --dry-run --json`.
    pub fn preview_apply_profile(&mut self, ctx: &egui::Context) {
        // Validate locally first so malformed TOML never spawns pkexec.
        if let Some(hw) = &self.hw {
            match Profile::from_toml_str(&self.profile_editor.toml_text) {
                Ok(p) => {
                    if let Err(e) = p.validate_for(
                        hw.codename,
                        hw.topology.physical_cores,
                        self.profile_editor.allow_extreme,
                    ) {
                        self.profile_editor.validation = Some(Err(e.to_string()));
                        self.set_toast(ToastLevel::Error, e.to_string());
                        return;
                    }
                }
                Err(e) => {
                    let msg = format!("Invalid TOML: {e}");
                    self.profile_editor.validation = Some(Err(msg.clone()));
                    self.set_toast(ToastLevel::Error, msg);
                    return;
                }
            }
        }

        let tmp_path = std::env::temp_dir().join(format!(
            "zensmu-gui-profile-{}.toml",
            std::process::id()
        ));
        if let Err(e) = fs::write(&tmp_path, &self.profile_editor.toml_text) {
            self.set_toast(
                ToastLevel::Error,
                format!("Failed to stage temporary profile '{}': {e}", tmp_path.display()),
            );
            return;
        }

        self.dispatch_cli(
            CliAction::ProfileApply {
                path: tmp_path,
                allow_extreme: self.profile_editor.allow_extreme,
                dry_run: true,
            },
            ctx,
        );
    }

    /// Export current live hardware state via `zensmu profile export` to a temp file and display it.
    pub fn export_current_profile(&mut self, ctx: &egui::Context) {
        let tmp_path = std::env::temp_dir().join(format!(
            "zensmu-gui-export-{}.toml",
            std::process::id()
        ));
        self.dispatch_cli(CliAction::ProfileExport { path: tmp_path }, ctx);
    }

    fn drain_telemetry(&mut self) {
        let mut drained = Vec::new();
        if let Some(rx) = &self.telemetry_rx {
            while let Ok(msg) = rx.try_recv() {
                drained.push(msg);
            }
        }

        for msg in drained {
            match msg {
                TelemetryMsg::Sample(sample) => {
                    if !self.pbo_editor.seeded_from_telemetry {
                        if let Some(v) = sample.snapshot.ppt_limit_w {
                            self.pbo_editor.ppt_w = v.round().clamp(15.0, 300.0) as u32;
                        }
                        if let Some(v) = sample.snapshot.tdc_limit_a {
                            self.pbo_editor.tdc_a = v.round().clamp(15.0, 250.0) as u32;
                        }
                        if let Some(v) = sample.snapshot.edc_limit_a {
                            self.pbo_editor.edc_a = v.round().clamp(20.0, 300.0) as u32;
                        }
                        if let Some(v) = sample.snapshot.tctl_limit_c {
                            self.pbo_editor.tctl_c = v.round().clamp(50.0, 95.0) as u32;
                        }
                        self.pbo_editor.seeded_from_telemetry = true;
                    }
                    self.latest_telemetry = Some(sample.snapshot.clone());
                    self.telemetry_error = None;
                    if self.samples.len() >= TELEMETRY_RING_CAPACITY {
                        self.samples.pop_front();
                    }
                    self.samples.push_back(sample);
                }
                TelemetryMsg::Error(err) => {
                    self.telemetry_error = Some(err);
                }
            }
        }
    }

    fn drain_cli_results(&mut self, ctx: &egui::Context) {
        for result in self.runner.poll_results() {
            match result {
                ElevatedResult::Status(payload) => {
                    self.apply_status_payload(payload);
                    self.refresh_safety_files();
                    self.set_toast(ToastLevel::Success, "Refreshed live SMU state.");
                }
                ElevatedResult::Co(snap) => {
                    self.sync_co_from_snapshot(snap);
                    self.set_toast(ToastLevel::Success, "Read current Curve Optimizer margins.");
                }
                ElevatedResult::Shaper(state) => {
                    self.shaper_state = Some(state);
                    self.shaper_editor.draft = state;
                    self.set_toast(ToastLevel::Success, "Read current Curve Shaper matrix.");
                }
                ElevatedResult::Pbo(status) => {
                    self.sync_pbo_from_status(status);
                    self.set_toast(ToastLevel::Success, "Read current PBO status & fused limits.");
                }
                ElevatedResult::DryRunPlan {
                    title,
                    live_action,
                    ops,
                } => {
                    self.pending_modal = Some(DryRunModal {
                        title,
                        ops,
                        live_action,
                    });
                }
                ElevatedResult::Applied { title, ops } => {
                    self.refresh_safety_files();
                    self.set_toast(
                        ToastLevel::Success,
                        format!("{title}: applied {} verified SMU operation(s).", ops.len()),
                    );
                    // Refresh live SMU state after a successful mutation so sliders stay in sync.
                    self.dispatch_cli(CliAction::Status, ctx);
                }
                ElevatedResult::ProfileExported { path, toml } => {
                    self.profile_editor.toml_text = toml;
                    self.profile_editor.validation = None;
                    let _ = fs::remove_file(path);
                    self.set_toast(
                        ToastLevel::Success,
                        "Exported current SMU state into the TOML editor.",
                    );
                }
                ElevatedResult::ProfileDiff { entries } => {
                    self.profile_editor.diff_entries = entries;
                    self.set_toast(ToastLevel::Info, "Updated profile diff.");
                }
                ElevatedResult::Error { title, message } => {
                    self.set_toast(ToastLevel::Error, format!("{title}: {message}"));
                }
            }
        }
    }

    fn apply_status_payload(&mut self, payload: StatusPayload) {
        if let Some(co) = payload.co {
            self.sync_co_from_snapshot(co);
        }
        if let Some(cs) = payload.shaper {
            self.shaper_state = Some(cs);
            self.shaper_editor.draft = cs;
        }
        if let Some(pbo) = payload.pbo {
            self.sync_pbo_from_status(pbo);
        }
        if let Some(telem) = payload.telemetry {
            self.latest_telemetry = Some(telem);
        }
    }

    fn sync_co_from_snapshot(&mut self, snap: CoSnapshot) {
        let count = self
            .hw
            .as_ref()
            .map(|h| h.topology.physical_cores)
            .unwrap_or(snap.cores.len())
            .max(snap.cores.len());
        if self.co_editor.per_core.len() < count {
            self.co_editor.per_core.resize(count, 0);
        }
        for c in &snap.cores {
            if let Some(slot) = self.co_editor.per_core.get_mut(c.core_index as usize) {
                *slot = i32::from(c.margin);
            }
        }
        if let Some(first) = snap.cores.first() {
            if snap.cores.iter().all(|c| c.margin == first.margin) {
                self.co_editor.all_core = i32::from(first.margin);
            }
        }
        if let Some(igpu) = snap.igpu {
            self.co_editor.igpu = i32::from(igpu);
        }
        self.co_snapshot = Some(snap);
    }

    fn sync_pbo_from_status(&mut self, status: PboStatus) {
        if let Some(scalar) = status.scalar {
            self.pbo_editor.scalar = scalar.round().clamp(1.0, 10.0) as u32;
        }
        if let Some(fmax) = status.fmax_mhz {
            self.pbo_editor.fmax_mhz = fmax.clamp(2000, 6500);
        }
        self.pbo_status = Some(status);
    }
}

impl Drop for ZensmuApp {
    fn drop(&mut self) {
        self.telemetry_stop.store(true, Ordering::Relaxed);
    }
}

impl eframe::App for ZensmuApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_zoom_shortcuts(ctx);
        self.drain_telemetry();
        self.drain_cli_results(ctx);

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
    }
}

/// Clamp and round a UI zoom value to two decimal places within `UI_ZOOM_MIN..=UI_ZOOM_MAX`.
pub fn clamp_ui_zoom(zoom: f32) -> f32 {
    (zoom.clamp(UI_ZOOM_MIN, UI_ZOOM_MAX) * 100.0).round() / 100.0
}

/// Apply the dark cockpit visual theme and legible desktop typography to `egui`.
pub fn configure_dark_cockpit_theme(ctx: &egui::Context) {
    // Manage Ctrl+/Ctrl-/Ctrl+0 ourselves so `ZensmuApp::ui_zoom` stays in sync and clamped.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = views::BG_PANEL;
    visuals.window_fill = views::BG_CARD;
    visuals.faint_bg_color = egui::Color32::from_rgb(24, 28, 38);
    visuals.extreme_bg_color = egui::Color32::from_rgb(11, 13, 18);
    visuals.window_stroke = egui::Stroke::new(1.0_f32, views::BORDER_SUBTLE);
    visuals.widgets.noninteractive.bg_fill = views::BG_CARD;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, views::BORDER_SUBTLE);
    visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(31, 36, 48);
    visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(42, 49, 65);
    visuals.widgets.active.bg_fill = views::ACCENT_CRIMSON;
    visuals.selection.bg_fill = egui::Color32::from_rgba_unmultiplied(224, 90, 71, 65);
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, views::ACCENT_CRIMSON);
    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (
            egui::TextStyle::Heading,
            egui::FontId::new(24.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Body,
            egui::FontId::new(16.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Button,
            egui::FontId::new(15.5, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Monospace,
            egui::FontId::new(15.0, egui::FontFamily::Monospace),
        ),
        (
            egui::TextStyle::Small,
            egui::FontId::new(13.5, egui::FontFamily::Proportional),
        ),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.slider_width = 150.0;
    ctx.set_style(style);
}

fn default_profile_template(hw: &HardwareContext) -> String {
    let mut out = format!(
        "schema = {PROFILE_SCHEMA_VERSION}\n\n[target]\ncodename = \"{}\"\nphysical_cores = {}\n",
        hw.codename.slug(),
        hw.topology.physical_cores
    );

    if hw.capabilities.co_bounds.is_some() {
        out.push_str("\n[curve_optimizer]\nall = -15\n# per_core = { \"0\" = -10, \"1\" = -20 }\n");
        if hw.capabilities.set_igpu_co.is_some() {
            out.push_str("# igpu = -10\n");
        }
    }

    if hw.capabilities.set_curve_shaper.is_some() {
        out.push_str(
            "\n[curve_shaper.high]\nlow = -10\nmed = -15\nhigh = -10\n\n[curve_shaper.max]\nlow = -5\nmed = -10\nhigh = -5\n",
        );
    }

    if hw.capabilities.set_ppt.is_some() {
        out.push_str("\n[pbo]\nppt_w = 105\ntdc_a = 80\nedc_a = 150\ntctl_c = 85\n# scalar = 1\n");
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
