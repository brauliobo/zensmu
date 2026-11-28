//! Core UI state structs, modal payloads, recorder state, and bench state.

use crate::elevate::{CliAction, ReportPayload};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Instant;
use zensmu_core::bench::{BenchProgress, BenchReport, BenchWorkload};
use zensmu_core::config::profile::ProfileDiffEntry;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::recorder::{
    default_daemon_recording_path, default_recording_path, pts_result_path,
    read_record_service_status, RecordConfig, RecordFormat, RecordServiceStatus, TelemetryRecorder,
    USER_RECORD_SERVICE_NAME,
};
use zensmu_core::safety::guard::SentinelInfo;
use zensmu_core::types::{Codename, PlannedOp};

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
pub(super) enum TelemetryMsg {
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

/// Message sent from the unprivileged Core-Cycler background thread to the UI.
pub enum BenchUiMsg {
    Progress(BenchProgress),
    Finished(Result<BenchReport, String>),
}

/// Unprivileged per-core Curve Optimizer stability tester / Core-Cycler UI state.
pub struct BenchUiState {
    pub running: bool,
    pub duration_per_core_secs: u64,
    pub passes: u32,
    pub workload: BenchWorkload,
    pub stop_on_error: bool,
    pub progress: Option<BenchProgress>,
    pub last_report: Option<BenchReport>,
    pub error: Option<String>,
    pub stop_flag: Arc<AtomicBool>,
    pub rx: Option<Receiver<BenchUiMsg>>,
}

impl Default for BenchUiState {
    fn default() -> Self {
        Self {
            running: false,
            duration_per_core_secs: 15,
            passes: 1,
            workload: BenchWorkload::Mixed,
            stop_on_error: true,
            progress: None,
            last_report: None,
            error: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
            rx: None,
        }
    }
}
