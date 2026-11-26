//! Application state, telemetry polling thread, and dark cockpit theme for `zensmu-gui`.

use crate::elevate::{
    is_running_as_root, CliAction, CommandRunner, ElevatedResult, ReportPayload, StatusPayload,
};
use crate::views;
use eframe::egui;
use std::collections::{BTreeMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
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
use zensmu_core::ops::pbo::{
    PboSetRequest, PboStatus, EDC_MAX_A, EDC_MIN_A, EDC_SOFT_MAX_A, FMAX_MAX_MHZ, FMAX_MIN_MHZ,
    PPT_MAX_W, PPT_MIN_W, PPT_SOFT_MAX_W, SCALAR_MAX, SCALAR_MIN, SCALAR_SOFT_MAX, TCTL_MAX_C,
    TCTL_MIN_C, TDC_MAX_A, TDC_MIN_A, TDC_SOFT_MAX_A,
};
use zensmu_core::ops::pm_table::{self, TelemetrySnapshot};
use zensmu_core::recorder::{
    default_daemon_recording_path, default_recording_path, default_recordings_dir, pts_result_path,
    read_record_service_status, set_record_service_enabled, RecordConfig, RecordFormat,
    RecordServiceStatus, TelemetryRecorder, USER_RECORD_SERVICE_NAME,
};
use zensmu_core::report::{collect_debug_report, ReportOptions, SystemBoardInfo};
use zensmu_core::safety::guard::{BootLoopGuard, SentinelInfo};
use zensmu_core::safety::persist::{read_persist_status, PersistPaths, PersistStatus};
use zensmu_core::safety::snapshot::{SmuStateSnapshot, SnapshotStore};
use zensmu_core::transport::sysfs::SysfsTransport;
use zensmu_core::types::{ApuPowerMode, Codename, CurveShaperState, PlannedOp};

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
    Sample(Box<TelemetrySample>),
    Error {
        message: String,
        suggests_report: bool,
        hint: Option<&'static str>,
    },
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
#[derive(Debug, Clone, Default)]
pub struct CoEditorState {
    pub all_core: i32,
    pub per_core: Vec<i32>,
    pub igpu: i32,
    pub allow_extreme: bool,
}

/// Editable Curve Shaper (Zen 5) UI state.
#[derive(Debug, Clone, Default)]
pub struct ShaperEditorState {
    pub draft: CurveShaperState,
    pub allow_extreme: bool,
}

/// Sync a positive finite live telemetry limit into a non-dirty `u32` slider field.
fn sync_u32_limit(target: &mut u32, dirty: bool, live: Option<f32>, min: u32, max: u32) {
    if !dirty {
        if let Some(v) = live.filter(|v| v.is_finite() && *v > 0.0) {
            *target = v.round().clamp(min as f32, max as f32) as u32;
        }
    }
}

/// Convert a finite, strictly positive telemetry float to `Some(u32)` for diff snapshots.
fn finite_positive_u32(live: Option<f32>) -> Option<u32> {
    live.filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v.round() as u32)
}

/// Editable PBO Limits UI state.
#[derive(Debug, Clone)]
pub struct PboEditorState {
    pub ppt_enabled: bool,
    pub ppt_w: u32,
    pub ppt_dirty: bool,
    pub tdc_enabled: bool,
    pub tdc_a: u32,
    pub tdc_dirty: bool,
    pub edc_enabled: bool,
    pub edc_a: u32,
    pub edc_dirty: bool,
    pub tctl_enabled: bool,
    pub tctl_c: u32,
    pub tctl_dirty: bool,
    pub scalar_enabled: bool,
    pub scalar: u32,
    pub scalar_dirty: bool,
    pub fmax_enabled: bool,
    pub fmax_mhz: u32,
    pub fmax_dirty: bool,
    pub allow_extreme: bool,
}

impl Default for PboEditorState {
    fn default() -> Self {
        Self {
            ppt_enabled: true,
            ppt_w: 88,
            ppt_dirty: false,
            tdc_enabled: true,
            tdc_a: 75,
            tdc_dirty: false,
            edc_enabled: true,
            edc_a: 150,
            edc_dirty: false,
            tctl_enabled: true,
            tctl_c: 85,
            tctl_dirty: false,
            scalar_enabled: false,
            scalar: 1,
            scalar_dirty: false,
            fmax_enabled: false,
            fmax_mhz: 5500,
            fmax_dirty: false,
            allow_extreme: false,
        }
    }
}

impl PboEditorState {
    /// Sync live hardware PBO limits from a PM table telemetry snapshot into non-dirty slider fields.
    pub fn sync_from_telemetry(&mut self, snap: &TelemetrySnapshot) {
        sync_u32_limit(
            &mut self.ppt_w,
            self.ppt_dirty,
            snap.ppt_limit_w,
            PPT_MIN_W,
            PPT_MAX_W,
        );
        sync_u32_limit(
            &mut self.tdc_a,
            self.tdc_dirty,
            snap.tdc_limit_a,
            TDC_MIN_A,
            TDC_MAX_A,
        );
        sync_u32_limit(
            &mut self.edc_a,
            self.edc_dirty,
            snap.edc_limit_a,
            EDC_MIN_A,
            EDC_MAX_A,
        );
        sync_u32_limit(
            &mut self.tctl_c,
            self.tctl_dirty,
            snap.tctl_limit_c,
            TCTL_MIN_C,
            TCTL_MAX_C,
        );
    }

    /// Sync PBO scalar and Fmax from a `PboStatus` mailbox snapshot.
    ///
    /// When `force` is true (explicit refresh or post-apply resync), dirty flags for
    /// scalar and Fmax are cleared and overwritten from live hardware state.
    pub fn sync_from_status(&mut self, status: &PboStatus, force: bool) {
        if force {
            self.scalar_dirty = false;
            self.fmax_dirty = false;
        }
        sync_u32_limit(
            &mut self.scalar,
            self.scalar_dirty,
            status.scalar,
            SCALAR_MIN,
            SCALAR_MAX,
        );
        if !self.fmax_dirty {
            if let Some(fmax) = status.fmax_mhz {
                self.fmax_mhz = fmax.clamp(FMAX_MIN_MHZ, FMAX_MAX_MHZ);
            }
        }
    }

    /// Clear all dirty flags so subsequent telemetry/status snapshots resync slider values.
    pub fn clear_dirty(&mut self) {
        self.ppt_dirty = false;
        self.tdc_dirty = false;
        self.edc_dirty = false;
        self.tctl_dirty = false;
        self.scalar_dirty = false;
        self.fmax_dirty = false;
    }

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

    /// Returns true if any enabled PBO field exceeds its conservative soft ceiling.
    pub fn has_any_extreme_for(&self, caps: &CpuCapabilities) -> bool {
        let req = self.build_request_for(caps);
        req.ppt_w.is_some_and(|v| v > PPT_SOFT_MAX_W)
            || req.tdc_a.is_some_and(|v| v > TDC_SOFT_MAX_A)
            || req.edc_a.is_some_and(|v| v > EDC_SOFT_MAX_A)
            || req.scalar.is_some_and(|v| v > SCALAR_SOFT_MAX)
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

        sync_u32_limit(
            &mut self.stapm_w,
            self.stapm_dirty,
            snap.stapm_limit_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
        );
        sync_u32_limit(
            &mut self.fast_w,
            self.fast_dirty,
            snap.ppt_limit_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
        );
        sync_u32_limit(
            &mut self.slow_w,
            self.slow_dirty,
            snap.slow_limit_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
        );
        sync_u32_limit(
            &mut self.apu_slow_w,
            self.apu_slow_dirty,
            snap.apu_slow_limit_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
        );
        sync_u32_limit(
            &mut self.tdc_soc_a,
            self.tdc_soc_dirty,
            snap.tdc_soc_limit_a,
            APU_SOC_TDC_MIN_A,
            APU_SOC_TDC_MAX_A,
        );
        sync_u32_limit(
            &mut self.edc_soc_a,
            self.edc_soc_dirty,
            snap.edc_soc_limit_a,
            APU_SOC_EDC_MIN_A,
            APU_SOC_EDC_MAX_A,
        );
        sync_u32_limit(
            &mut self.apu_skin_temp_c,
            self.apu_skin_temp_dirty,
            snap.apu_skin_temp_limit_c,
            APU_SKIN_TEMP_MIN_C,
            APU_SKIN_TEMP_MAX_C,
        );
        sync_u32_limit(
            &mut self.dgpu_skin_temp_c,
            self.dgpu_skin_temp_dirty,
            snap.dgpu_skin_temp_limit_c,
            APU_SKIN_TEMP_MIN_C,
            APU_SKIN_TEMP_MAX_C,
        );
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

/// State for the privacy-safe GitHub CPU Support / Bug Report modal.
#[derive(Debug, Clone)]
pub struct ReportModalState {
    pub open: bool,
    pub note: String,
    pub include_raw_pm_table: bool,
    pub include_privileged_probes: bool,
    pub save_path: String,
    pub payload: Option<ReportPayload>,
    pub status_note: Option<(String, bool)>,
}

impl Default for ReportModalState {
    fn default() -> Self {
        Self {
            open: false,
            note: String::new(),
            include_raw_pm_table: false,
            include_privileged_probes: false,
            save_path: "/tmp/zensmu-report.md".into(),
            payload: None,
            status_note: None,
        }
    }
}

/// Unprivileged telemetry recording & user-daemon UI state.
pub struct RecorderUiState {
    pub active: Option<TelemetryRecorder>,
    pub started_at: Option<Instant>,
    pub format: RecordFormat,
    pub tag: String,
    pub pts_result_name: String,
    pub last_saved_path: Option<PathBuf>,
    pub last_saved_samples: u64,
    pub daemon_status: RecordServiceStatus,
}

impl Default for RecorderUiState {
    fn default() -> Self {
        Self {
            active: None,
            started_at: None,
            format: RecordFormat::Csv,
            tag: String::new(),
            pts_result_name: String::new(),
            last_saved_path: None,
            last_saved_samples: 0,
            daemon_status: RecordServiceStatus {
                service_name: USER_RECORD_SERVICE_NAME.to_string(),
                installed: false,
                enabled: false,
                active: false,
                state: "unknown".to_string(),
                default_output_path: default_daemon_recording_path().display().to_string(),
            },
        }
    }
}

impl RecorderUiState {
    /// Create initial recorder UI state with live `systemctl --user` daemon status.
    pub fn with_detected_daemon_status() -> Self {
        Self {
            daemon_status: read_record_service_status(),
            ..Self::default()
        }
    }

    pub fn is_recording(&self) -> bool {
        self.active.is_some()
    }

    pub fn live_samples(&self) -> u64 {
        self.active
            .as_ref()
            .map(|r| r.stats().samples_written)
            .unwrap_or(0)
    }

    pub fn live_bytes(&self) -> u64 {
        self.active
            .as_ref()
            .map(|r| r.stats().bytes_written)
            .unwrap_or(0)
    }

    pub fn elapsed_secs(&self) -> u64 {
        self.started_at
            .map(|started| started.elapsed().as_secs())
            .unwrap_or(0)
    }

    pub fn active_path(&self) -> Option<&Path> {
        self.active.as_ref().map(|r| r.path())
    }

    /// Resolve the output destination path based on current format, optional PTS result name, and codename.
    pub fn resolve_output_path(&self, codename: Codename) -> PathBuf {
        let pts_name = self.pts_result_name.trim();
        if self.format == RecordFormat::Pts && !pts_name.is_empty() {
            pts_result_path(pts_name)
        } else {
            default_recording_path(codename, self.format)
        }
    }

    /// Open a new `TelemetryRecorder` writing to `path` using the configured format and tag.
    pub fn start_with_path(&mut self, path: PathBuf) -> zensmu_core::error::Result<PathBuf> {
        let trimmed_tag = self.tag.trim();
        let tag = (!trimmed_tag.is_empty()).then(|| trimmed_tag.to_string());
        let config = RecordConfig {
            path,
            format: self.format,
            append: false,
            max_file_bytes: None,
            max_rotated_files: 0,
            flush_every: 1,
            tag,
        };
        let recorder = TelemetryRecorder::open(config)?;
        let out_path = recorder.path().to_path_buf();
        self.active = Some(recorder);
        self.started_at = Some(Instant::now());
        Ok(out_path)
    }

    /// Write a live telemetry snapshot to the active recorder, if one is open.
    pub fn write_snapshot(&mut self, snap: &TelemetrySnapshot) -> zensmu_core::error::Result<()> {
        if let Some(ref mut rec) = self.active {
            rec.write_snapshot(snap)?;
        }
        Ok(())
    }

    /// Flush, persist last-saved metadata, and drop the active recorder.
    pub fn stop(&mut self) -> zensmu_core::error::Result<Option<(PathBuf, u64)>> {
        self.started_at = None;
        let Some(mut rec) = self.active.take() else {
            return Ok(None);
        };
        let flush_res = rec.flush();
        let stats = rec.stats();
        self.last_saved_path = Some(stats.path.clone());
        self.last_saved_samples = stats.samples_written;
        drop(rec);
        flush_res?;
        Ok(Some((stats.path, stats.samples_written)))
    }
}

/// Top-level `eframe` application state.
pub struct ZensmuApp {
    pub is_root: bool,
    pub hw: Option<HardwareContext>,
    pub hw_error: Option<String>,
    pub hw_error_suggests_report: bool,
    pub hw_error_hint: Option<&'static str>,
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

    // Unprivileged telemetry recording & user-level background daemon
    pub recorder: RecorderUiState,

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

    /// Start unprivileged live telemetry recording to the configured format and path.
    pub fn start_recording(&mut self) {
        if self.recorder.is_recording() {
            return;
        }
        let codename = self
            .hw
            .as_ref()
            .map(|h| h.codename)
            .unwrap_or(Codename::Undefined);
        let path = self.recorder.resolve_output_path(codename);
        let fmt_label = self.recorder.format.as_str().to_ascii_uppercase();
        match self.recorder.start_with_path(path) {
            Ok(out_path) => {
                self.set_toast(
                    ToastLevel::Success,
                    format!("Recording {fmt_label} telemetry to {}", out_path.display()),
                );
            }
            Err(err) => {
                self.set_toast(
                    ToastLevel::Error,
                    format!("Failed to start telemetry recording: {err}"),
                );
            }
        }
    }

    /// Flush and stop the active telemetry recording, saving last-file summary metadata.
    pub fn stop_recording(&mut self) {
        match self.recorder.stop() {
            Ok(Some((path, samples))) => {
                self.set_toast(
                    ToastLevel::Success,
                    format!("Saved {samples} samples to {}", path.display()),
                );
            }
            Ok(None) => {}
            Err(err) => {
                self.set_toast(
                    ToastLevel::Error,
                    format!("Recording stopped with flush error: {err}"),
                );
            }
        }
    }

    /// Toggle live telemetry recording on or off.
    pub fn toggle_recording(&mut self) {
        if self.recorder.is_recording() {
            self.stop_recording();
        } else {
            self.start_recording();
        }
    }

    /// Enable or disable the unprivileged user-level `zensmu-record.service` daemon (no `pkexec`).
    pub fn toggle_record_daemon(&mut self) {
        let target_enable = !self.recorder.daemon_status.enabled;
        match set_record_service_enabled(target_enable) {
            Ok(status) => {
                self.recorder.daemon_status = status;
                let msg = if target_enable {
                    format!("Enabled background telemetry daemon ({USER_RECORD_SERVICE_NAME}).")
                } else {
                    format!("Disabled background telemetry daemon ({USER_RECORD_SERVICE_NAME}).")
                };
                self.set_toast(ToastLevel::Info, msg);
            }
            Err(err) => {
                self.recorder.daemon_status = read_record_service_status();
                self.set_toast(
                    ToastLevel::Error,
                    format!("Background recorder toggle failed: {err}"),
                );
            }
        }
    }

    /// Refresh unprivileged `zensmu-record.service` status via `systemctl --user`.
    pub fn refresh_record_daemon_status(&mut self) {
        self.recorder.daemon_status = read_record_service_status();
    }

    /// Ensure the default telemetry recordings folder exists and open it via `xdg-open` (no shell).
    pub fn open_recordings_folder(&self) -> Result<PathBuf, String> {
        let dir = default_recordings_dir();
        fs::create_dir_all(&dir).map_err(|e| {
            format!(
                "Failed to create recordings directory '{}': {e}",
                dir.display()
            )
        })?;
        Command::new("xdg-open")
            .arg(&dir)
            .spawn()
            .map(|_| dir.clone())
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    format!(
                        "Could not find `xdg-open`. Recordings are stored in '{}'.",
                        dir.display()
                    )
                } else {
                    format!("Failed to open '{}': {e}", dir.display())
                }
            })
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
                        Ok(snapshot) => TelemetryMsg::Sample(Box::new(TelemetrySample {
                            elapsed_s: start_time.elapsed().as_secs_f64(),
                            snapshot,
                        })),
                        Err(e) => TelemetryMsg::Error {
                            suggests_report: e.suggests_report(),
                            hint: e.report_hint(),
                            message: e.to_string(),
                        },
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
            snap.ppt_w = finite_positive_u32(telem.ppt_limit_w);
            snap.tdc_a = finite_positive_u32(telem.tdc_limit_a);
            snap.edc_a = finite_positive_u32(telem.edc_limit_a);
            snap.tctl_c = finite_positive_u32(telem.tctl_limit_c);
            snap.stapm_w = finite_positive_u32(telem.stapm_limit_w);
            snap.slow_w = finite_positive_u32(telem.slow_limit_w);
            snap.apu_slow_w = finite_positive_u32(telem.apu_slow_limit_w);
            snap.tdc_soc_a = finite_positive_u32(telem.tdc_soc_limit_a);
            snap.edc_soc_a = finite_positive_u32(telem.edc_soc_limit_a);
            snap.apu_skin_temp_c = finite_positive_u32(telem.apu_skin_temp_limit_c);
            snap.dgpu_skin_temp_c = finite_positive_u32(telem.dgpu_skin_temp_limit_c);
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
        if !self.validate_editor_toml_for_persist() {
            return;
        }

        let tmp_path =
            std::env::temp_dir().join(format!("zensmu-gui-profile-{}.toml", std::process::id()));
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

        self.staging_profile_path = Some(tmp_path.clone());
        self.dispatch_cli(
            CliAction::ProfileApply {
                path: tmp_path,
                allow_extreme: self.profile_editor.allow_extreme,
                dry_run: true,
            },
            ctx,
        );
    }

    /// Close the dry-run confirmation modal and clean up any staged profile file.
    pub fn dismiss_dry_run_modal(&mut self) {
        self.pending_modal = None;
        self.cleanup_staging_profile();
    }

    fn cleanup_staging_profile(&mut self) {
        if let Some(tmp) = self.staging_profile_path.take() {
            let _ = fs::remove_file(tmp);
        }
    }

    fn cleanup_staging_persist(&mut self) {
        if let Some(tmp) = self.staging_persist_path.take() {
            let _ = fs::remove_file(tmp);
        }
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

        let tmp_path =
            std::env::temp_dir().join(format!("zensmu-gui-persist-{}.toml", std::process::id()));
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

    /// Open the GitHub CPU Support / Bug Report modal and immediately pre-generate
    /// an unprivileged diagnostic report so the Markdown preview is never empty.
    pub fn open_report_modal(&mut self) {
        self.report_modal.open = true;
        self.report_modal.status_note = None;
        self.generate_unprivileged_report();
    }

    /// Open the report modal pre-configured with raw PM table float dumps enabled
    /// (ideal for unsupported CPUs or unrecognized PM table versions).
    pub fn open_report_modal_for_unsupported(&mut self) {
        self.report_modal.include_raw_pm_table = true;
        self.open_report_modal();
    }

    /// Synchronously collect an unprivileged, strictly read-only diagnostic report.
    pub fn generate_unprivileged_report(&mut self) {
        let note = self.report_modal.note.trim();
        let opts = ReportOptions {
            include_raw_pm_table: self.report_modal.include_raw_pm_table,
            include_mailbox_probes: false,
            user_note: (!note.is_empty()).then(|| note.to_string()),
            no_cpuid_check: false,
        };
        let report = collect_debug_report(&SysfsTransport::default(), &opts);
        self.report_modal.payload = Some(ReportPayload::from_report(report));
    }

    /// Refresh diagnostics in the report modal — either synchronously (unprivileged)
    /// or asynchronously via `pkexec zensmu report --json` when privileged probes are enabled.
    pub fn refresh_report(&mut self, ctx: &egui::Context) {
        if self.report_modal.include_privileged_probes {
            if self.runner.is_busy() {
                self.report_modal.status_note = Some((
                    "Another SMU command is already running — please wait.".into(),
                    false,
                ));
                return;
            }
            let note = self.report_modal.note.trim();
            let action = CliAction::Report {
                raw_pm_table: self.report_modal.include_raw_pm_table,
                note: (!note.is_empty()).then(|| note.to_string()),
            };
            self.report_modal.status_note = Some((
                if self.is_root {
                    "Collecting privileged SMU diagnostics…".into()
                } else {
                    "Collecting privileged SMU diagnostics via pkexec…".into()
                },
                true,
            ));
            self.dispatch_cli(action, ctx);
        } else {
            self.generate_unprivileged_report();
            self.report_modal.status_note =
                Some(("Refreshed unprivileged hardware diagnostics.".into(), true));
        }
    }

    /// Save the currently generated Markdown report to `report_modal.save_path`.
    pub fn save_report_to_disk(&mut self) {
        let Some(markdown) = self
            .report_modal
            .payload
            .as_ref()
            .map(|p| p.markdown.clone())
        else {
            self.report_modal.status_note = Some((
                "No report generated yet — click Refresh Diagnostics.".into(),
                false,
            ));
            return;
        };
        let path = self.report_modal.save_path.trim().to_string();
        if path.is_empty() {
            self.report_modal.status_note =
                Some(("Please enter a destination file path.".into(), false));
            return;
        }
        match fs::write(&path, markdown) {
            Ok(()) => {
                self.report_modal.status_note = Some((format!("Saved to {path}"), true));
            }
            Err(e) => {
                self.report_modal.status_note =
                    Some((format!("Failed to save '{path}': {e}"), false));
            }
        }
    }

    /// Export current live hardware state via `zensmu profile export` to a temp file and display it.
    pub fn export_current_profile(&mut self, ctx: &egui::Context) {
        let tmp_path =
            std::env::temp_dir().join(format!("zensmu-gui-export-{}.toml", std::process::id()));
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
                    let sample = *sample;
                    if let Err(err) = self.recorder.write_snapshot(&sample.snapshot) {
                        let _ = self.recorder.stop();
                        self.set_toast(
                            ToastLevel::Error,
                            format!("Telemetry recording stopped due to write error: {err}"),
                        );
                    }
                    self.pbo_editor.sync_from_telemetry(&sample.snapshot);
                    self.apu_editor.sync_from_telemetry(&sample.snapshot);
                    self.latest_telemetry = Some(sample.snapshot.clone());
                    self.telemetry_error = None;
                    self.telemetry_error_suggests_report = false;
                    self.telemetry_error_hint = None;
                    if self.samples.len() >= TELEMETRY_RING_CAPACITY {
                        self.samples.pop_front();
                    }
                    self.samples.push_back(sample);
                }
                TelemetryMsg::Error {
                    message,
                    suggests_report,
                    hint,
                } => {
                    self.telemetry_error = Some(message);
                    self.telemetry_error_suggests_report = suggests_report;
                    self.telemetry_error_hint = hint;
                }
            }
        }
    }

    fn drain_cli_results(&mut self, ctx: &egui::Context) {
        for result in self.runner.poll_results() {
            match result {
                ElevatedResult::Status(payload) => {
                    self.apply_status_payload(*payload);
                    self.refresh_safety_files();
                    self.refresh_persist_status();
                    if self.suppress_next_status_toast {
                        self.suppress_next_status_toast = false;
                    } else {
                        self.set_toast(ToastLevel::Success, "Refreshed live SMU state.");
                    }
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
                    self.sync_pbo_from_status(status, true);
                    self.set_toast(
                        ToastLevel::Success,
                        "Read current PBO status & fused limits.",
                    );
                }
                ElevatedResult::Report(payload) => {
                    self.report_modal.payload = Some(*payload);
                    self.report_modal.status_note = Some((
                        "Refreshed diagnostics with privileged SMU probes.".into(),
                        true,
                    ));
                    self.set_toast(
                        ToastLevel::Success,
                        "Collected privileged hardware diagnostic report.",
                    );
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
                    self.cleanup_staging_profile();
                    self.clamp_warning = None;
                    self.pbo_editor.clear_dirty();
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
                ElevatedResult::PersistUpdated {
                    action,
                    status,
                    note,
                } => {
                    self.cleanup_staging_persist();
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
                ElevatedResult::HardwareClamped { message, caps } => {
                    self.cleanup_staging_persist();
                    self.cleanup_staging_profile();
                    self.pbo_editor.clear_dirty();
                    self.apu_editor.clear_dirty();
                    for (field, cap) in caps {
                        self.board_caps.insert(field, cap);
                    }
                    self.clamp_warning = Some(message.clone());
                    self.set_toast(
                        ToastLevel::Warning,
                        format!("Board/BIOS clamped limit — check BIOS PBO Limits: {message}"),
                    );
                    // Immediately resync sliders to the actual clamped hardware values.
                    self.suppress_next_status_toast = true;
                    self.dispatch_cli(CliAction::Status, ctx);
                }
                ElevatedResult::Error { title, message } => {
                    self.cleanup_staging_persist();
                    self.cleanup_staging_profile();
                    if self.report_modal.open && title == "Collect Hardware Report" {
                        self.report_modal.status_note =
                            Some((format!("Privileged report failed: {message}"), false));
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
            self.sync_pbo_from_status(pbo, true);
        }
        if let Some(telem) = payload.telemetry {
            self.pbo_editor.sync_from_telemetry(&telem);
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
        self.co_editor.per_core.resize(count, 0);
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

    fn sync_pbo_from_status(&mut self, status: PboStatus, force: bool) {
        self.pbo_editor.sync_from_status(&status, force);
        self.pbo_status = Some(status);
    }
}

impl Drop for ZensmuApp {
    fn drop(&mut self) {
        let _ = self.recorder.stop();
        self.telemetry_stop.store(true, Ordering::Relaxed);
        self.cleanup_staging_persist();
        self.cleanup_staging_profile();
    }
}

impl eframe::App for ZensmuApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_zoom_shortcuts(ctx);
        self.drain_telemetry();
        self.drain_cli_results(ctx);

        if self.recorder.is_recording() {
            ctx.request_repaint_after(Duration::from_millis(500));
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

/// Returns true when a CLI/telemetry error string indicates an unexpected SMU rejection,
/// verification failure, unsupported CPU/PM table, or identity mismatch where filing a
/// diagnostic GitHub report is helpful.
pub fn error_message_suggests_report(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("smu rejected")
        || lower.contains("verification failed")
        || lower.contains("unsupported")
        || lower.contains("mismatch")
        || lower.contains("mailbox health check failed")
}

/// Launch an `http://` or `https://` URL in the user's desktop browser via `xdg-open`
/// without any shell interpolation.
pub fn open_url_in_browser(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err("Refused to open URL: only http:// and https:// URLs are supported.".into());
    }
    Command::new("xdg-open")
        .arg(trimmed)
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "Could not find `xdg-open`. Copy the Markdown report and open GitHub manually."
                    .into()
            } else {
                format!("Failed to launch browser via `xdg-open`: {e}")
            }
        })
}

/// Format a compact motherboard & BIOS summary string from non-identifying DMI fields.
pub fn format_board_summary(info: &SystemBoardInfo) -> Option<String> {
    let vendor = info
        .board_vendor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let name = info
        .board_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let bios = info
        .bios_version
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let board_part = match (vendor, name) {
        (Some(v), Some(n)) => {
            if n.to_ascii_lowercase().starts_with(&v.to_ascii_lowercase()) {
                Some(n.to_string())
            } else {
                Some(format!("{v} {n}"))
            }
        }
        (Some(v), None) => Some(v.to_string()),
        (None, Some(n)) => Some(n.to_string()),
        (None, None) => None,
    };

    match (board_part, bios) {
        (Some(b), Some(v)) => Some(format!("{b} · BIOS {v}")),
        (Some(b), None) => Some(b),
        (None, Some(v)) => Some(format!("BIOS {v}")),
        (None, None) => None,
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
    visuals.faint_bg_color = views::BG_STRIPE;
    visuals.extreme_bg_color = views::BG_EXTREME;
    visuals.code_bg_color = views::BG_ELEVATED;
    visuals.window_stroke = egui::Stroke::new(1.0_f32, views::BORDER_SUBTLE);

    visuals.hyperlink_color = views::ACCENT_CYAN;
    visuals.warn_fg_color = views::ACCENT_AMBER;
    visuals.error_fg_color = views::ACCENT_CRIMSON;

    // Non-interactive body text, headings, dividers
    visuals.widgets.noninteractive.weak_bg_fill = views::BG_CARD;
    visuals.widgets.noninteractive.bg_fill = views::BG_CARD;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, views::BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, views::TEXT_PRIMARY);

    // Inactive buttons (`weak_bg_fill`), checkboxes, sliders, text inputs (`bg_fill`)
    visuals.widgets.inactive.weak_bg_fill = views::BG_ELEVATED;
    visuals.widgets.inactive.bg_fill = views::BG_ELEVATED;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, views::BORDER_CONTROL);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, views::TEXT_PRIMARY);

    // Hovered controls
    visuals.widgets.hovered.weak_bg_fill = views::BG_HOVERED;
    visuals.widgets.hovered.bg_fill = views::BG_HOVERED;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, views::BORDER_HOVER);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5_f32, views::TEXT_BRIGHT);

    // Active / pressed controls & dragged slider handle
    visuals.widgets.active.weak_bg_fill = views::CTA_CRIMSON;
    visuals.widgets.active.bg_fill = views::CTA_CRIMSON;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, views::TEXT_ON_CTA);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(2.0_f32, views::TEXT_ON_CTA);

    // Open ComboBox / dropdown headers
    visuals.widgets.open.weak_bg_fill = views::BG_OPEN;
    visuals.widgets.open.bg_fill = views::BG_OPEN;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, views::BORDER_HOVER);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, views::TEXT_BRIGHT);

    // Selection highlight (ComboBox selected row, TextEdit selection, ProgressBar default text)
    visuals.selection.bg_fill = views::SELECTION_BG;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, views::TEXT_BRIGHT);

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

    #[test]
    fn pbo_editor_respects_dirty_flags_clamps_full_bounds_and_resyncs() {
        use zensmu_core::hw::profiles::capabilities_for;
        use zensmu_core::types::Codename;

        let mut pbo = PboEditorState {
            ppt_dirty: true,
            ppt_w: 120,
            scalar_dirty: true,
            scalar: 4,
            ..Default::default()
        };

        let snap = TelemetrySnapshot {
            codename: Codename::GraniteRidge,
            table_version: 0x0062_0205,
            table_size: 0x800,
            ppt_limit_w: Some(1000.0), // BIOS Motherboard/Unlimited profile
            ppt_value_w: Some(85.0),
            tdc_limit_a: Some(480.0),
            tdc_value_a: Some(45.0),
            edc_limit_a: Some(f32::NAN), // non-finite ignored
            edc_value_a: Some(60.0),
            tctl_limit_c: Some(95.0),
            tctl_value_c: Some(65.0),
            stapm_limit_w: None,
            stapm_value_w: None,
            slow_limit_w: None,
            slow_value_w: None,
            apu_slow_limit_w: None,
            apu_slow_value_w: None,
            tdc_soc_limit_a: None,
            tdc_soc_value_a: None,
            edc_soc_limit_a: None,
            edc_soc_value_a: None,
            apu_skin_temp_limit_c: None,
            apu_skin_temp_value_c: None,
            dgpu_skin_temp_limit_c: None,
            dgpu_skin_temp_value_c: None,
            fclk_mhz: Some(2000.0),
            uclk_mhz: Some(3000.0),
            mclk_mhz: Some(3000.0),
            vddcr_soc_v: Some(1.2),
            vddcr_misc_v: None,
            cldo_vddp_v: None,
            cldo_vddg_iod_v: None,
            cldo_vddg_ccd_v: None,
        };

        pbo.sync_from_telemetry(&snap);
        // Dirty PPT stays at user value; non-dirty TDC/Tctl sync to full bounds; NaN EDC untouched.
        assert_eq!(pbo.ppt_w, 120);
        assert_eq!(pbo.tdc_a, 480);
        assert_eq!(pbo.edc_a, 150);
        assert_eq!(pbo.tctl_c, 95);

        let caps = capabilities_for(Codename::GraniteRidge).unwrap();
        assert!(pbo.has_any_extreme_for(&caps)); // TDC 480 > 180 soft max

        // Status sync respects dirty when force=false, overwrites when force=true.
        let status = PboStatus {
            scalar: Some(2.0),
            fmax_mhz: Some(5650),
            ..PboStatus::default()
        };
        pbo.sync_from_status(&status, false);
        assert_eq!(pbo.scalar, 4);
        assert_eq!(pbo.fmax_mhz, 5650);

        pbo.sync_from_status(&status, true);
        assert_eq!(pbo.scalar, 2);
        assert!(!pbo.scalar_dirty);

        // Clearing dirty allows PPT to resync to live 1000W limit.
        pbo.clear_dirty();
        pbo.sync_from_telemetry(&snap);
        assert_eq!(pbo.ppt_w, 1000);
    }

    /// Convert an 8-bit sRGB channel to linear light per WCAG 2.1 §1.4.3.
    fn srgb_channel_to_linear(channel: u8) -> f64 {
        let s = f64::from(channel) / 255.0;
        if s <= 0.04045 {
            s / 12.92
        } else {
            ((s + 0.055) / 1.055).powf(2.4)
        }
    }

    /// WCAG 2.1 relative luminance for an opaque sRGB `Color32`.
    fn relative_luminance(color: egui::Color32) -> f64 {
        let [r, g, b, _] = color.to_srgba_unmultiplied();
        0.2126 * srgb_channel_to_linear(r)
            + 0.7152 * srgb_channel_to_linear(g)
            + 0.0722 * srgb_channel_to_linear(b)
    }

    /// Alpha-composite a (possibly translucent) `egui::Color32` foreground over an opaque background.
    fn composite_over(fg: egui::Color32, bg: egui::Color32) -> egui::Color32 {
        let [fr, fg_c, fb, fa] = fg.to_srgba_unmultiplied();
        let [br, bg_c, bb, _] = bg.to_srgba_unmultiplied();
        let a = f64::from(fa) / 255.0;
        let blend = |f: u8, b: u8| -> u8 {
            (f64::from(f) * a + f64::from(b) * (1.0 - a))
                .round()
                .clamp(0.0, 255.0) as u8
        };
        egui::Color32::from_rgb(blend(fr, br), blend(fg_c, bg_c), blend(fb, bb))
    }

    /// Exact WCAG 2.1 contrast ratio `(L1 + 0.05) / (L2 + 0.05)` in `[1.0, 21.0]`.
    fn contrast_ratio(fg: egui::Color32, bg: egui::Color32) -> f64 {
        let fg_opaque = if fg.a() < 255 {
            composite_over(fg, bg)
        } else {
            fg
        };
        let l1 = relative_luminance(fg_opaque);
        let l2 = relative_luminance(bg);
        let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
        (hi + 0.05) / (lo + 0.05)
    }

    fn assert_wcag(label: &str, fg: egui::Color32, bg: egui::Color32, min_ratio: f64) {
        let ratio = contrast_ratio(fg, bg);
        assert!(
            ratio >= min_ratio,
            "WCAG contrast failure for {label}: fg={fg:?} on bg={bg:?} -> {ratio:.2}:1 (required >= {min_ratio:.1}:1)"
        );
    }

    #[test]
    fn wcag_contrast_math_matches_reference_values() {
        let bw = contrast_ratio(egui::Color32::BLACK, egui::Color32::WHITE);
        assert!(
            (bw - 21.0).abs() < 1e-6,
            "black/white expected 21.0, got {bw}"
        );

        let same = contrast_ratio(views::BG_CARD, views::BG_CARD);
        assert!(
            (same - 1.0).abs() < 1e-6,
            "identical colors expected 1.0, got {same}"
        );

        // #767676 on #FFFFFF is the canonical WCAG 2.1 AA boundary (~4.54:1).
        let aa_ref = contrast_ratio(
            egui::Color32::from_rgb(0x76, 0x76, 0x76),
            egui::Color32::WHITE,
        );
        assert!(
            (4.53..=4.56).contains(&aa_ref),
            "#767676 on white expected ~4.54:1, got {aa_ref:.3}"
        );
    }

    #[test]
    fn dark_cockpit_palette_and_widgets_pass_wcag_aa() {
        const AA_TEXT: f64 = 4.5;
        const AAA_TEXT: f64 = 7.0;
        const AA_GRAPHICAL: f64 = 3.0;

        let ctx = egui::Context::default();
        configure_dark_cockpit_theme(&ctx);
        let visuals = ctx.style().visuals.clone();

        // 1. Every semantic foreground/text color against every dark surface
        let dark_surfaces = [
            ("BG_CANVAS", views::BG_CANVAS),
            ("BG_EXTREME", views::BG_EXTREME),
            ("BG_PANEL", views::BG_PANEL),
            ("BG_CARD", views::BG_CARD),
            ("BG_STRIPE", views::BG_STRIPE),
            ("BG_ELEVATED", views::BG_ELEVATED),
            ("BG_HOVERED", views::BG_HOVERED),
            ("BG_OPEN", views::BG_OPEN),
        ];
        let text_colors = [
            ("TEXT_BRIGHT", views::TEXT_BRIGHT),
            ("TEXT_PRIMARY", views::TEXT_PRIMARY),
            ("TEXT_MUTED", views::TEXT_MUTED),
            ("ACCENT_CRIMSON", views::ACCENT_CRIMSON),
            ("ACCENT_CYAN", views::ACCENT_CYAN),
            ("ACCENT_AMBER", views::ACCENT_AMBER),
            ("ACCENT_EMERALD", views::ACCENT_EMERALD),
            ("ACCENT_VIOLET", views::ACCENT_VIOLET),
        ];
        for (fg_name, fg) in text_colors {
            for (bg_name, bg) in dark_surfaces {
                assert_wcag(&format!("{fg_name} on {bg_name}"), fg, bg, AA_TEXT);
            }
        }

        // Primary body text should hit AAA (>= 7:1) across all main surfaces.
        for (bg_name, bg) in dark_surfaces {
            assert_wcag(
                &format!("TEXT_PRIMARY AAA on {bg_name}"),
                views::TEXT_PRIMARY,
                bg,
                AAA_TEXT,
            );
            assert_wcag(
                &format!("TEXT_BRIGHT AAA on {bg_name}"),
                views::TEXT_BRIGHT,
                bg,
                AAA_TEXT,
            );
        }

        // 2. Configured egui::Visuals widget states (buttons, inputs, sliders, checkboxes, selection)
        assert_wcag(
            "visuals.noninteractive text on BG_CARD",
            visuals.widgets.noninteractive.fg_stroke.color,
            views::BG_CARD,
            AAA_TEXT,
        );
        assert_wcag(
            "visuals.inactive button text on weak_bg_fill",
            visuals.widgets.inactive.fg_stroke.color,
            visuals.widgets.inactive.weak_bg_fill,
            AAA_TEXT,
        );
        assert_wcag(
            "visuals.inactive control fg on bg_fill",
            visuals.widgets.inactive.fg_stroke.color,
            visuals.widgets.inactive.bg_fill,
            AAA_TEXT,
        );
        assert_wcag(
            "visuals.hovered button text on weak_bg_fill",
            visuals.widgets.hovered.fg_stroke.color,
            visuals.widgets.hovered.weak_bg_fill,
            AAA_TEXT,
        );
        assert_wcag(
            "visuals.active button text on weak_bg_fill",
            visuals.widgets.active.fg_stroke.color,
            visuals.widgets.active.weak_bg_fill,
            AA_TEXT,
        );
        assert_wcag(
            "visuals.active control fg on bg_fill",
            visuals.widgets.active.fg_stroke.color,
            visuals.widgets.active.bg_fill,
            AA_TEXT,
        );
        assert_wcag(
            "visuals.open text on weak_bg_fill",
            visuals.widgets.open.fg_stroke.color,
            visuals.widgets.open.weak_bg_fill,
            AAA_TEXT,
        );
        for (bg_name, bg) in [
            ("BG_PANEL", views::BG_PANEL),
            ("BG_CARD", views::BG_CARD),
            ("BG_EXTREME", views::BG_EXTREME),
        ] {
            let sel_bg = composite_over(visuals.selection.bg_fill, bg);
            assert_wcag(
                &format!("visuals.selection stroke on selection bg over {bg_name}"),
                visuals.selection.stroke.color,
                sel_bg,
                AAA_TEXT,
            );
            assert_wcag(
                &format!("TEXT_PRIMARY on selection bg over {bg_name}"),
                views::TEXT_PRIMARY,
                sel_bg,
                AAA_TEXT,
            );
        }
        assert_wcag(
            "ui.code text on code_bg_color",
            visuals.widgets.noninteractive.fg_stroke.color,
            visuals.code_bg_color,
            AAA_TEXT,
        );
        assert_wcag(
            "TextEdit text on extreme_bg_color",
            visuals.widgets.inactive.fg_stroke.color,
            visuals.extreme_bg_color,
            AAA_TEXT,
        );
        assert_wcag(
            "TextEdit hint (weak_text_color) on extreme_bg_color",
            visuals.weak_text_color(),
            visuals.extreme_bg_color,
            AA_TEXT,
        );
        for (bg_name, bg) in [
            ("BG_CANVAS", views::BG_CANVAS),
            ("BG_PANEL", views::BG_PANEL),
            ("BG_CARD", views::BG_CARD),
            ("BG_STRIPE", views::BG_STRIPE),
        ] {
            assert_wcag(
                &format!("hyperlink_color on {bg_name}"),
                visuals.hyperlink_color,
                bg,
                AA_TEXT,
            );
            assert_wcag(
                &format!("warn_fg_color on {bg_name}"),
                visuals.warn_fg_color,
                bg,
                AA_TEXT,
            );
            assert_wcag(
                &format!("error_fg_color on {bg_name}"),
                visuals.error_fg_color,
                bg,
                AA_TEXT,
            );
        }

        // 3. Solid CTA buttons
        assert_wcag(
            "CTA_CRIMSON button label",
            views::TEXT_ON_CTA,
            views::CTA_CRIMSON,
            AA_TEXT,
        );
        assert_wcag(
            "CTA_EMERALD button label",
            views::TEXT_ON_CTA,
            views::CTA_EMERALD,
            AA_TEXT,
        );

        // 4. Tinted pills composited over panel, card, and striped table rows
        let pill_specs = [
            (
                "crimson pill",
                views::ACCENT_CRIMSON,
                views::PILL_ALPHA_CRIMSON,
            ),
            ("amber pill", views::ACCENT_AMBER, views::PILL_ALPHA_AMBER),
            (
                "emerald pill",
                views::ACCENT_EMERALD,
                views::PILL_ALPHA_EMERALD,
            ),
            ("cyan pill", views::ACCENT_CYAN, views::PILL_ALPHA_CYAN),
            (
                "violet pill",
                views::ACCENT_VIOLET,
                views::PILL_ALPHA_VIOLET,
            ),
        ];
        for (name, fg, alpha) in pill_specs {
            let tint = views::tinted_bg(fg, alpha);
            for (bg_name, under) in [
                ("BG_PANEL", views::BG_PANEL),
                ("BG_CARD", views::BG_CARD),
                ("BG_STRIPE", views::BG_STRIPE),
            ] {
                let effective_bg = composite_over(tint, under);
                assert_wcag(&format!("{name} on {bg_name}"), fg, effective_bg, AA_TEXT);
            }
        }
        assert_wcag("muted_pill", views::TEXT_MUTED, views::BG_ELEVATED, AA_TEXT);
        assert_wcag(
            "identity_pill",
            views::TEXT_PRIMARY,
            views::BG_ELEVATED,
            AAA_TEXT,
        );

        // 5. Alert banners (root warning & missing driver) on BG_CANVAS
        let root_banner_bg = composite_over(
            views::tinted_bg(views::ACCENT_CRIMSON, views::BANNER_ALPHA_CRIMSON),
            views::BG_CANVAS,
        );
        assert_wcag(
            "root banner title",
            views::ACCENT_CRIMSON,
            root_banner_bg,
            AA_TEXT,
        );
        assert_wcag(
            "root banner body",
            views::TEXT_PRIMARY,
            root_banner_bg,
            AAA_TEXT,
        );

        let hw_banner_bg = composite_over(
            views::tinted_bg(views::ACCENT_AMBER, views::BANNER_ALPHA_AMBER),
            views::BG_CANVAS,
        );
        assert_wcag(
            "hw_error banner title",
            views::ACCENT_AMBER,
            hw_banner_bg,
            AA_TEXT,
        );
        assert_wcag(
            "hw_error banner body",
            views::TEXT_PRIMARY,
            hw_banner_bg,
            AAA_TEXT,
        );

        // 6. Selected sidebar navigation tab on BG_PANEL
        let sidebar_sel_bg = composite_over(
            views::tinted_bg(views::ACCENT_CRIMSON, views::SIDEBAR_SELECTED_ALPHA),
            views::BG_PANEL,
        );
        assert_wcag(
            "sidebar selected title",
            views::TEXT_BRIGHT,
            sidebar_sel_bg,
            AAA_TEXT,
        );
        assert_wcag(
            "sidebar selected subtitle",
            views::TEXT_MUTED,
            sidebar_sel_bg,
            AA_TEXT,
        );

        // 7. Graphical UI components (>= 3.0:1): progress bar fills on dark track, plot lines, interactive control borders
        for (name, bar_color) in [
            ("ACCENT_CYAN", views::ACCENT_CYAN),
            ("ACCENT_AMBER", views::ACCENT_AMBER),
            ("ACCENT_EMERALD", views::ACCENT_EMERALD),
            ("ACCENT_VIOLET", views::ACCENT_VIOLET),
            ("ACCENT_CRIMSON", views::ACCENT_CRIMSON),
        ] {
            assert_wcag(
                &format!("progress bar / plot {name} on BG_EXTREME"),
                bar_color,
                views::BG_EXTREME,
                AA_GRAPHICAL,
            );
            assert_wcag(
                &format!("plot {name} on BG_CARD"),
                bar_color,
                views::BG_CARD,
                AA_GRAPHICAL,
            );
        }
        for (bg_name, bg) in [
            ("BG_EXTREME", views::BG_EXTREME),
            ("BG_PANEL", views::BG_PANEL),
            ("BG_CARD", views::BG_CARD),
            ("BG_STRIPE", views::BG_STRIPE),
        ] {
            assert_wcag(
                &format!("BORDER_CONTROL on {bg_name}"),
                views::BORDER_CONTROL,
                bg,
                AA_GRAPHICAL,
            );
            assert_wcag(
                &format!("BORDER_HOVER on {bg_name}"),
                views::BORDER_HOVER,
                bg,
                AA_GRAPHICAL,
            );
        }
    }

    #[test]
    fn compact_cpu_label_shortens_verbose_ryzen_brands() {
        assert_eq!(
            views::compact_cpu_label("AMD Ryzen 7 9700X 8-Core Processor"),
            "Ryzen 7 9700X"
        );
        assert_eq!(
            views::compact_cpu_label("AMD Ryzen 9 9950X3D 16-Core Processor"),
            "Ryzen 9 9950X3D"
        );
        assert_eq!(
            views::compact_cpu_label("AMD Ryzen 7 7840U w/ Radeon 780M Graphics"),
            "Ryzen 7 7840U"
        );
        assert_eq!(
            views::compact_cpu_label("AMD Ryzen 7 5700G with Radeon Graphics"),
            "Ryzen 7 5700G"
        );
    }

    #[test]
    fn format_board_summary_formats_vendor_model_and_bios_cleanly() {
        let full = SystemBoardInfo {
            board_vendor: Some("Gigabyte Technology Co., Ltd.".into()),
            board_name: Some("B450M DS3H V2".into()),
            bios_version: Some("F65b".into()),
            ..SystemBoardInfo::default()
        };
        assert_eq!(
            format_board_summary(&full).as_deref(),
            Some("Gigabyte Technology Co., Ltd. B450M DS3H V2 · BIOS F65b")
        );

        let no_vendor = SystemBoardInfo {
            board_name: Some("ROG STRIX X670E-E".into()),
            bios_version: Some("2401".into()),
            ..SystemBoardInfo::default()
        };
        assert_eq!(
            format_board_summary(&no_vendor).as_deref(),
            Some("ROG STRIX X670E-E · BIOS 2401")
        );

        assert_eq!(format_board_summary(&SystemBoardInfo::default()), None);
    }

    #[test]
    fn open_url_in_browser_rejects_non_http_schemes() {
        let err = open_url_in_browser("file:///etc/passwd").unwrap_err();
        assert!(err.contains("http://"));
        assert!(open_url_in_browser("javascript:alert(1)").is_err());
    }

    #[test]
    fn error_message_suggests_report_matches_smu_and_unsupported_errors() {
        assert!(error_message_suggests_report("SMU rejected command 0x07"));
        assert!(error_message_suggests_report(
            "read-after-write verification failed for pbo.ppt_w"
        ));
        assert!(error_message_suggests_report(
            "unsupported PM table version 0xDEADBEEF"
        ));
        assert!(error_message_suggests_report(
            "CPU identity mismatch: ryzen_smu reports Vermeer"
        ));
        assert!(!error_message_suggests_report(
            "PolicyKit authentication was dismissed or cancelled."
        ));
    }

    #[test]
    fn report_modal_state_defaults_match_expected_path_and_flags() {
        let state = ReportModalState::default();
        assert!(!state.open);
        assert!(!state.include_raw_pm_table);
        assert!(!state.include_privileged_probes);
        assert_eq!(state.save_path, "/tmp/zensmu-report.md");
        assert!(state.payload.is_none());
        assert!(state.status_note.is_none());
    }

    #[test]
    fn recorder_ui_state_records_and_flushes_csv_jsonl_and_pts_cleanly() {
        use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};
        use zensmu_core::recorder::CSV_HEADER;

        static TEST_SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = TEST_SEQ.fetch_add(1, AtomicOrdering::Relaxed);
        let test_dir = std::env::temp_dir().join(format!(
            "zensmu-gui-recorder-test-{}-{seq}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&test_dir);
        fs::create_dir_all(&test_dir).unwrap();

        struct CleanupDir(PathBuf);
        impl Drop for CleanupDir {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = CleanupDir(test_dir.clone());

        let sample_a = TelemetrySnapshot {
            codename: Codename::GraniteRidge,
            table_version: 0x0062_0205,
            table_size: 0x800,
            ppt_limit_w: Some(142.0),
            ppt_value_w: Some(88.5),
            tdc_limit_a: Some(110.0),
            tdc_value_a: Some(62.0),
            edc_limit_a: Some(170.0),
            edc_value_a: Some(95.0),
            tctl_limit_c: Some(95.0),
            tctl_value_c: Some(71.25),
            stapm_limit_w: None,
            stapm_value_w: None,
            slow_limit_w: None,
            slow_value_w: None,
            apu_slow_limit_w: None,
            apu_slow_value_w: None,
            tdc_soc_limit_a: None,
            tdc_soc_value_a: None,
            edc_soc_limit_a: None,
            edc_soc_value_a: None,
            apu_skin_temp_limit_c: None,
            apu_skin_temp_value_c: None,
            dgpu_skin_temp_limit_c: None,
            dgpu_skin_temp_value_c: None,
            fclk_mhz: Some(2000.0),
            uclk_mhz: Some(3000.0),
            mclk_mhz: Some(3000.0),
            vddcr_soc_v: Some(1.24),
            vddcr_misc_v: Some(1.10),
            cldo_vddp_v: Some(0.95),
            cldo_vddg_iod_v: Some(0.95),
            cldo_vddg_ccd_v: Some(0.95),
        };
        let mut sample_b = sample_a.clone();
        sample_b.ppt_value_w = Some(102.0);
        sample_b.tctl_value_c = Some(76.5);

        let mut ui_state = RecorderUiState::default();
        assert!(!ui_state.is_recording());
        assert_eq!(ui_state.format, RecordFormat::Csv);

        // 1. CSV recording
        ui_state.format = RecordFormat::Csv;
        ui_state.tag = "co-20".into();
        let csv_path = test_dir.join("run.csv");
        let opened = ui_state.start_with_path(csv_path.clone()).unwrap();
        assert_eq!(opened, csv_path);
        assert!(ui_state.is_recording());
        assert!(ui_state.started_at.is_some());
        ui_state.write_snapshot(&sample_a).unwrap();
        ui_state.write_snapshot(&sample_b).unwrap();
        assert_eq!(ui_state.live_samples(), 2);

        let stopped = ui_state.stop().unwrap().unwrap();
        assert_eq!(stopped, (csv_path.clone(), 2));
        assert!(!ui_state.is_recording());
        assert!(ui_state.started_at.is_none());
        assert_eq!(ui_state.last_saved_path.as_ref(), Some(&csv_path));
        assert_eq!(ui_state.last_saved_samples, 2);

        let csv_text = fs::read_to_string(&csv_path).unwrap();
        let csv_lines: Vec<&str> = csv_text.lines().collect();
        assert_eq!(csv_lines.len(), 3, "expected header + 2 rows");
        assert_eq!(csv_lines[0], CSV_HEADER);
        assert!(csv_lines[1].contains("granite-ridge,co-20,88.50,142.00"));
        assert!(csv_lines[2].contains("granite-ridge,co-20,102.00,142.00"));

        // 2. JSONL recording
        ui_state.format = RecordFormat::Jsonl;
        ui_state.tag = "gaming".into();
        let jsonl_path = test_dir.join("run.jsonl");
        ui_state.start_with_path(jsonl_path.clone()).unwrap();
        ui_state.write_snapshot(&sample_a).unwrap();
        ui_state.write_snapshot(&sample_b).unwrap();
        let stopped = ui_state.stop().unwrap().unwrap();
        assert_eq!(stopped, (jsonl_path.clone(), 2));
        assert_eq!(ui_state.last_saved_path.as_ref(), Some(&jsonl_path));
        assert_eq!(ui_state.last_saved_samples, 2);

        let jsonl_text = fs::read_to_string(&jsonl_path).unwrap();
        let jsonl_lines: Vec<&str> = jsonl_text.lines().collect();
        assert_eq!(jsonl_lines.len(), 2);
        let parsed0: serde_json::Value = serde_json::from_str(jsonl_lines[0]).unwrap();
        let parsed1: serde_json::Value = serde_json::from_str(jsonl_lines[1]).unwrap();
        assert_eq!(parsed0["tag"], "gaming");
        assert_eq!(parsed0["ppt_value_w"], 88.5);
        assert_eq!(parsed1["ppt_value_w"], 102.0);

        // 3. PTS composite.xml recording + PTS result path resolution
        ui_state.format = RecordFormat::Pts;
        ui_state.tag = "prime95".into();
        ui_state.pts_result_name = "my-bench-run".into();
        let resolved_pts = ui_state.resolve_output_path(Codename::GraniteRidge);
        assert!(
            resolved_pts.ends_with("test-results/my-bench-run/composite.xml"),
            "unexpected resolved PTS path: {}",
            resolved_pts.display()
        );

        let pts_path = test_dir.join("composite.xml");
        ui_state.start_with_path(pts_path.clone()).unwrap();
        ui_state.write_snapshot(&sample_a).unwrap();
        ui_state.write_snapshot(&sample_b).unwrap();
        let stopped = ui_state.stop().unwrap().unwrap();
        assert_eq!(stopped, (pts_path.clone(), 2));
        assert_eq!(ui_state.last_saved_path.as_ref(), Some(&pts_path));
        assert_eq!(ui_state.last_saved_samples, 2);

        let xml = fs::read_to_string(&pts_path).unwrap();
        assert!(xml.contains("<PhoronixTestSuite>"));
        assert!(xml.contains("<Identifier>zensmu/ppt</Identifier>"));
        assert!(xml.contains("<Identifier>zensmu/tctl</Identifier>"));
        assert!(xml.contains("88.50:102.00"));
        assert!(xml.contains("</PhoronixTestSuite>"));
    }
}
