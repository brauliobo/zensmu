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
use zensmu_core::hw::profiles::CpuCapabilities;
use zensmu_core::ops::apu::{
    ApuSetRequest, APU_FAST_POWER_SOFT_MAX_W, APU_POWER_MAX_W, APU_POWER_MIN_W,
    APU_POWER_SOFT_MAX_W, APU_SKIN_TEMP_MAX_C, APU_SKIN_TEMP_MIN_C, APU_SKIN_TEMP_SOFT_MAX_C,
    APU_SOC_EDC_MAX_A, APU_SOC_EDC_MIN_A, APU_SOC_EDC_SOFT_MAX_A, APU_SOC_TDC_MAX_A,
    APU_SOC_TDC_MIN_A, APU_SOC_TDC_SOFT_MAX_A,
};
use zensmu_core::ops::co::CoSnapshot;
use zensmu_core::ops::pbo::{PboSetRequest, PboStatus, TCTL_MAX_C};
use zensmu_core::ops::pm_table::{self, TelemetrySnapshot};
use zensmu_core::safety::guard::{BootLoopGuard, SentinelInfo};
use zensmu_core::safety::persist::{read_persist_status, PersistPaths, PersistStatus};
use zensmu_core::safety::snapshot::{SmuStateSnapshot, SnapshotStore};
use zensmu_core::transport::sysfs::SysfsTransport;
use zensmu_core::types::{ApuPowerMode, CurveShaperState, PlannedOp};

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

/// Action executed when the user confirms a boot persistence modal.
#[derive(Debug, Clone)]
pub enum PersistConfirmTarget {
    Cli(CliAction),
    EditorToml { enable: bool },
}

/// Confirmation modal explaining exact system changes before invoking `pkexec` for persistence.
#[derive(Debug, Clone)]
pub struct PersistConfirmModal {
    pub title: String,
    pub summary: String,
    pub changes: Vec<String>,
    pub confirm_label: String,
    pub is_destructive: bool,
    pub target: PersistConfirmTarget,
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
    pub fn build_request_for(&self, caps: &CpuCapabilities) -> PboSetRequest {
        PboSetRequest {
            ppt_w: (self.ppt_enabled && caps.set_ppt.is_some()).then_some(self.ppt_w),
            tdc_a: (self.tdc_enabled && caps.set_tdc.is_some()).then_some(self.tdc_a),
            edc_a: (self.edc_enabled && caps.set_edc.is_some()).then_some(self.edc_a),
            tctl_c: (self.tctl_enabled && caps.set_tctl.is_some())
                .then_some(self.tctl_c.min(TCTL_MAX_C)),
            scalar: (self.scalar_enabled && caps.set_pbo_scalar.is_some()).then_some(self.scalar),
            fmax_mhz: (self.fmax_enabled && caps.set_fmax.is_some()).then_some(self.fmax_mhz),
        }
    }

    pub fn has_any_enabled_for(&self, caps: &CpuCapabilities) -> bool {
        (self.ppt_enabled && caps.set_ppt.is_some())
            || (self.tdc_enabled && caps.set_tdc.is_some())
            || (self.edc_enabled && caps.set_edc.is_some())
            || (self.tctl_enabled && caps.set_tctl.is_some())
            || (self.scalar_enabled && caps.set_pbo_scalar.is_some())
            || (self.fmax_enabled && caps.set_fmax.is_some())
    }
}

/// Editable mobile APU Dynamic Power and Thermal Configuration (DPTC) UI state.
#[derive(Debug, Clone)]
pub struct ApuEditorState {
    pub stapm_enabled: bool,
    pub stapm_w: u32,
    pub stapm_dirty: bool,
    pub live_stapm_w: Option<f32>,

    pub stapm_time_enabled: bool,
    pub stapm_time_s: u32,

    pub fast_enabled: bool,
    pub fast_w: u32,
    pub fast_dirty: bool,
    pub live_fast_w: Option<f32>,

    pub slow_enabled: bool,
    pub slow_w: u32,
    pub slow_dirty: bool,
    pub live_slow_w: Option<f32>,

    pub slow_time_enabled: bool,
    pub slow_time_s: u32,

    pub apu_slow_enabled: bool,
    pub apu_slow_w: u32,
    pub apu_slow_dirty: bool,
    pub live_apu_slow_w: Option<f32>,

    pub tdc_soc_enabled: bool,
    pub tdc_soc_a: u32,
    pub tdc_soc_dirty: bool,
    pub live_tdc_soc_a: Option<f32>,

    pub edc_soc_enabled: bool,
    pub edc_soc_a: u32,
    pub edc_soc_dirty: bool,
    pub live_edc_soc_a: Option<f32>,

    pub apu_skin_temp_enabled: bool,
    pub apu_skin_temp_c: u32,
    pub apu_skin_temp_dirty: bool,
    pub live_apu_skin_temp_c: Option<f32>,

    pub dgpu_skin_temp_enabled: bool,
    pub dgpu_skin_temp_c: u32,
    pub dgpu_skin_temp_dirty: bool,
    pub live_dgpu_skin_temp_c: Option<f32>,

    pub power_mode: Option<ApuPowerMode>,
    pub allow_extreme: bool,
}

impl Default for ApuEditorState {
    fn default() -> Self {
        Self {
            stapm_enabled: false,
            stapm_w: 28,
            stapm_dirty: false,
            live_stapm_w: None,

            stapm_time_enabled: false,
            stapm_time_s: 120,

            fast_enabled: false,
            fast_w: 45,
            fast_dirty: false,
            live_fast_w: None,

            slow_enabled: false,
            slow_w: 35,
            slow_dirty: false,
            live_slow_w: None,

            slow_time_enabled: false,
            slow_time_s: 10,

            apu_slow_enabled: false,
            apu_slow_w: 30,
            apu_slow_dirty: false,
            live_apu_slow_w: None,

            tdc_soc_enabled: false,
            tdc_soc_a: 30,
            tdc_soc_dirty: false,
            live_tdc_soc_a: None,

            edc_soc_enabled: false,
            edc_soc_a: 50,
            edc_soc_dirty: false,
            live_edc_soc_a: None,

            apu_skin_temp_enabled: false,
            apu_skin_temp_c: 55,
            apu_skin_temp_dirty: false,
            live_apu_skin_temp_c: None,

            dgpu_skin_temp_enabled: false,
            dgpu_skin_temp_c: 60,
            dgpu_skin_temp_dirty: false,
            live_dgpu_skin_temp_c: None,

            power_mode: None,
            allow_extreme: false,
        }
    }
}

impl ApuEditorState {
    /// Sync live hardware limits from a PM table telemetry snapshot into non-dirty slider fields.
    pub fn sync_from_telemetry(&mut self, snap: &TelemetrySnapshot) {
        self.live_stapm_w = snap.stapm_limit_w;
        self.live_fast_w = snap.ppt_limit_w;
        self.live_slow_w = snap.slow_limit_w;
        self.live_apu_slow_w = snap.apu_slow_limit_w;
        self.live_tdc_soc_a = snap.tdc_soc_limit_a;
        self.live_edc_soc_a = snap.edc_soc_limit_a;
        self.live_apu_skin_temp_c = snap.apu_skin_temp_limit_c;
        self.live_dgpu_skin_temp_c = snap.dgpu_skin_temp_limit_c;

        if !self.stapm_dirty {
            if let Some(v) = snap.stapm_limit_w.filter(|&v| v > 0.0) {
                self.stapm_w = v
                    .round()
                    .clamp(APU_POWER_MIN_W as f32, APU_POWER_MAX_W as f32)
                    as u32;
            }
        }
        if !self.fast_dirty {
            if let Some(v) = snap.ppt_limit_w.filter(|&v| v > 0.0) {
                self.fast_w = v
                    .round()
                    .clamp(APU_POWER_MIN_W as f32, APU_POWER_MAX_W as f32)
                    as u32;
            }
        }
        if !self.slow_dirty {
            if let Some(v) = snap.slow_limit_w.filter(|&v| v > 0.0) {
                self.slow_w = v
                    .round()
                    .clamp(APU_POWER_MIN_W as f32, APU_POWER_MAX_W as f32)
                    as u32;
            }
        }
        if !self.apu_slow_dirty {
            if let Some(v) = snap.apu_slow_limit_w.filter(|&v| v > 0.0) {
                self.apu_slow_w = v
                    .round()
                    .clamp(APU_POWER_MIN_W as f32, APU_POWER_MAX_W as f32)
                    as u32;
            }
        }
        if !self.tdc_soc_dirty {
            if let Some(v) = snap.tdc_soc_limit_a.filter(|&v| v > 0.0) {
                self.tdc_soc_a = v
                    .round()
                    .clamp(APU_SOC_TDC_MIN_A as f32, APU_SOC_TDC_MAX_A as f32)
                    as u32;
            }
        }
        if !self.edc_soc_dirty {
            if let Some(v) = snap.edc_soc_limit_a.filter(|&v| v > 0.0) {
                self.edc_soc_a = v
                    .round()
                    .clamp(APU_SOC_EDC_MIN_A as f32, APU_SOC_EDC_MAX_A as f32)
                    as u32;
            }
        }
        if !self.apu_skin_temp_dirty {
            if let Some(v) = snap.apu_skin_temp_limit_c.filter(|&v| v > 0.0) {
                self.apu_skin_temp_c = v
                    .round()
                    .clamp(APU_SKIN_TEMP_MIN_C as f32, APU_SKIN_TEMP_MAX_C as f32)
                    as u32;
            }
        }
        if !self.dgpu_skin_temp_dirty {
            if let Some(v) = snap.dgpu_skin_temp_limit_c.filter(|&v| v > 0.0) {
                self.dgpu_skin_temp_c = v
                    .round()
                    .clamp(APU_SKIN_TEMP_MIN_C as f32, APU_SKIN_TEMP_MAX_C as f32)
                    as u32;
            }
        }
    }

    /// Clear all dirty flags so subsequent telemetry samples resync slider values.
    pub fn clear_dirty(&mut self) {
        self.stapm_dirty = false;
        self.fast_dirty = false;
        self.slow_dirty = false;
        self.apu_slow_dirty = false;
        self.tdc_soc_dirty = false;
        self.edc_soc_dirty = false;
        self.apu_skin_temp_dirty = false;
        self.dgpu_skin_temp_dirty = false;
    }

    /// Build an `ApuSetRequest` filtered to enabled fields supported by `hw`.
    pub fn build_request_for(&self, hw: &HardwareContext) -> ApuSetRequest {
        let caps = &hw.capabilities;
        let power_mode = self.power_mode.filter(|mode| match mode {
            ApuPowerMode::MaxPerformance => caps.set_max_performance.is_some(),
            ApuPowerMode::PowerSaving => caps.set_power_saving.is_some(),
        });
        ApuSetRequest {
            stapm_w: (self.stapm_enabled && caps.set_stapm_limit.is_some()).then_some(self.stapm_w),
            stapm_time_s: (self.stapm_time_enabled && caps.set_stapm_time.is_some())
                .then_some(self.stapm_time_s),
            fast_w: (self.fast_enabled && hw.codename.is_apu() && caps.set_ppt.is_some())
                .then_some(self.fast_w),
            slow_w: (self.slow_enabled && caps.set_slow_limit.is_some()).then_some(self.slow_w),
            slow_time_s: (self.slow_time_enabled && caps.set_slow_time.is_some())
                .then_some(self.slow_time_s),
            apu_slow_w: (self.apu_slow_enabled && caps.set_apu_slow_limit.is_some())
                .then_some(self.apu_slow_w),
            tdc_soc_a: (self.tdc_soc_enabled && caps.set_tdc_soc.is_some())
                .then_some(self.tdc_soc_a),
            edc_soc_a: (self.edc_soc_enabled && caps.set_edc_soc.is_some())
                .then_some(self.edc_soc_a),
            apu_skin_temp_c: (self.apu_skin_temp_enabled && caps.set_apu_skin_temp.is_some())
                .then_some(self.apu_skin_temp_c),
            dgpu_skin_temp_c: (self.dgpu_skin_temp_enabled && caps.set_dgpu_skin_temp.is_some())
                .then_some(self.dgpu_skin_temp_c),
            power_mode,
        }
    }

    pub fn has_any_enabled_for(&self, hw: &HardwareContext) -> bool {
        !self.build_request_for(hw).is_empty()
    }

    /// Returns true if any enabled APU field exceeds its conservative soft ceiling.
    pub fn has_any_extreme_for(&self, hw: &HardwareContext) -> bool {
        let req = self.build_request_for(hw);
        req.stapm_w.is_some_and(|v| v > APU_POWER_SOFT_MAX_W)
            || req.fast_w.is_some_and(|v| v > APU_FAST_POWER_SOFT_MAX_W)
            || req.slow_w.is_some_and(|v| v > APU_POWER_SOFT_MAX_W)
            || req.apu_slow_w.is_some_and(|v| v > APU_POWER_SOFT_MAX_W)
            || req.tdc_soc_a.is_some_and(|v| v > APU_SOC_TDC_SOFT_MAX_A)
            || req.edc_soc_a.is_some_and(|v| v > APU_SOC_EDC_SOFT_MAX_A)
            || req
                .apu_skin_temp_c
                .is_some_and(|v| v > APU_SKIN_TEMP_SOFT_MAX_C)
            || req
                .dgpu_skin_temp_c
                .is_some_and(|v| v > APU_SKIN_TEMP_SOFT_MAX_C)
    }

    /// Build a `CliAction::ApuSet` containing only enabled, hardware-supported fields.
    pub fn build_cli_action_for(&self, hw: &HardwareContext, dry_run: bool) -> Option<CliAction> {
        let req = self.build_request_for(hw);
        if req.is_empty() {
            return None;
        }
        Some(CliAction::ApuSet {
            req,
            allow_extreme: self.allow_extreme,
            dry_run,
        })
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
    pub apu_editor: ApuEditorState,
    pub profile_editor: ProfileEditorState,

    // Unprivileged boot/resume persistence status & confirmation modal
    pub persist_status: PersistStatus,
    pub persist_confirm: Option<PersistConfirmModal>,
    staging_persist_path: Option<PathBuf>,

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
            apu_editor: ApuEditorState::default(),
            profile_editor: ProfileEditorState::default(),
            persist_status: read_persist_status(&PersistPaths::default(), None, None),
            persist_confirm: None,
            staging_persist_path: None,
            pending_modal: None,
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
            snap.stapm_w = telem.stapm_limit_w.map(|v| v.round() as u32);
            snap.slow_w = telem.slow_limit_w.map(|v| v.round() as u32);
            snap.apu_slow_w = telem.apu_slow_limit_w.map(|v| v.round() as u32);
            snap.tdc_soc_a = telem.tdc_soc_limit_a.map(|v| v.round() as u32);
            snap.edc_soc_a = telem.edc_soc_limit_a.map(|v| v.round() as u32);
            snap.apu_skin_temp_c = telem.apu_skin_temp_limit_c.map(|v| v.round() as u32);
            snap.dgpu_skin_temp_c = telem.dgpu_skin_temp_limit_c.map(|v| v.round() as u32);
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

    /// Validate the TOML editor buffer against detected hardware; updates inline validation & toast on failure.
    pub fn validate_editor_toml_for_persist(&mut self) -> bool {
        let profile = match Profile::from_toml_str(&self.profile_editor.toml_text) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("Invalid TOML: {e}");
                self.profile_editor.validation = Some(Err(msg.clone()));
                self.set_toast(ToastLevel::Error, msg);
                return false;
            }
        };

        if let Some(hw) = &self.hw {
            if let Err(e) = profile.validate_for(
                hw.codename,
                hw.topology.physical_cores,
                self.profile_editor.allow_extreme,
            ) {
                let msg = e.to_string();
                self.profile_editor.validation = Some(Err(msg.clone()));
                self.set_toast(ToastLevel::Error, msg);
                return false;
            }
        }
        true
    }

    /// Validate the editor TOML buffer, stage it to `/tmp/zensmu-gui-persist-<pid>.toml`,
    /// and dispatch `zensmu persist save --from <tmp>` via `pkexec`.
    pub fn persist_editor_toml(&mut self, enable: bool, ctx: &egui::Context) {
        if !self.validate_editor_toml_for_persist() {
            return;
        }

        let tmp_path = std::env::temp_dir().join(format!(
            "zensmu-gui-persist-{}.toml",
            std::process::id()
        ));
        if let Err(e) = fs::write(&tmp_path, &self.profile_editor.toml_text) {
            self.set_toast(
                ToastLevel::Error,
                format!(
                    "Failed to stage temporary profile '{}': {e}",
                    tmp_path.display()
                ),
            );
            return;
        }

        self.staging_persist_path = Some(tmp_path.clone());
        self.dispatch_cli(
            CliAction::PersistSave {
                from_path: Some(tmp_path.display().to_string()),
                enable,
                allow_extreme: self.profile_editor.allow_extreme,
            },
            ctx,
        );
    }

    /// Preview enabled APU DPTC limit changes via `zensmu apu set --dry-run --json`.
    pub fn preview_apply_apu(&mut self, ctx: &egui::Context) {
        let Some(hw) = self.hw.as_ref() else {
            self.set_toast(ToastLevel::Warning, "No Ryzen SMU hardware detected.");
            return;
        };
        let Some(action) = self.apu_editor.build_cli_action_for(hw, true) else {
            self.set_toast(
                ToastLevel::Info,
                "Enable at least one APU DPTC parameter or power mode to preview.",
            );
            return;
        };
        self.dispatch_cli(action, ctx);
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
                    self.apu_editor.sync_from_telemetry(&sample.snapshot);
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
                    self.refresh_persist_status();
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
                    self.apu_editor.clear_dirty();
                    self.refresh_safety_files();
                    self.refresh_persist_status();
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
                ElevatedResult::PersistUpdated {
                    action,
                    status,
                    note,
                } => {
                    if let Some(tmp) = self.staging_persist_path.take() {
                        let _ = fs::remove_file(tmp);
                    }
                    self.persist_status = status;
                    self.refresh_safety_files();
                    self.refresh_persist_status();
                    let msg = match note {
                        Some(n) if !n.trim().is_empty() => format!("{action}: {n}"),
                        _ => format!(
                            "{action}: boot service is now {}.",
                            if self.persist_status.service_enabled {
                                "enabled"
                            } else {
                                "disabled"
                            }
                        ),
                    };
                    self.set_toast(ToastLevel::Success, msg);
                }
                ElevatedResult::GuardCleared { cleared } => {
                    self.refresh_safety_files();
                    self.refresh_persist_status();
                    if cleared {
                        self.set_toast(
                            ToastLevel::Success,
                            "Boot-loop crash guard cleared — auto-apply is re-armed.",
                        );
                    } else {
                        self.set_toast(
                            ToastLevel::Info,
                            "Boot-loop crash guard was already clean.",
                        );
                    }
                }
                ElevatedResult::Error { title, message } => {
                    if let Some(tmp) = self.staging_persist_path.take() {
                        let _ = fs::remove_file(tmp);
                    }
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
            self.apu_editor.sync_from_telemetry(&telem);
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
        views::render_persist_confirm_modal(self, ctx);
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
}
