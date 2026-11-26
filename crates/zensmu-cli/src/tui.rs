//! `btop`/`htop`-inspired live terminal UI (`zensmu top`).
//!
//! Works unprivileged out of the box by polling the read-only `pm_table` sysfs
//! endpoint (`0444`). When elevated privileges are available, also reads back
//! Curve Optimizer margins, Zen 5 Curve Shaper matrix, and PBO status/fused
//! limits via the SMU mailbox.

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, LineGauge, Padding, Paragraph, Row, Sparkline, Table,
    Wrap,
};
use ratatui::{Frame, Terminal};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{self, Stdout, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};
use zensmu_core::config::profile::Profile;
use zensmu_core::error::Error;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::ops::apu::{self, ApuSetRequest};
use zensmu_core::ops::co::{self, CoSnapshot};
use zensmu_core::ops::pbo::{self, FusedLimits, PboSetRequest, PboStatus};
use zensmu_core::ops::pm_table::{self, TelemetrySnapshot};
use zensmu_core::ops::shaper;
use zensmu_core::recorder::{self, RecordConfig, RecordFormat, TelemetryRecorder};
use zensmu_core::report::SystemBoardInfo;
use zensmu_core::safety::guard::BootLoopGuard;
use zensmu_core::safety::persist::{self, PersistPaths, PersistStatus};
use zensmu_core::safety::snapshot::{SmuStateSnapshot, SnapshotStore};
use zensmu_core::session;
use zensmu_core::transport::SmuTransport;
use zensmu_core::types::{
    ApuPowerMode, CurveShaperState, FreqTier, PlannedOp, ShaperTierMargins, TempBand,
};

/// Minimum allowed telemetry polling interval (ms).
pub const MIN_INTERVAL_MS: u64 = 100;
/// Maximum allowed telemetry polling interval (ms).
pub const MAX_INTERVAL_MS: u64 = 10_000;
/// Number of historical telemetry samples retained per metric.
pub const HISTORY_CAPACITY: usize = 120;

const UNPRIV_MAILBOX_NOTE: &str =
    "Run with sudo for Curve Optimizer / Shaper / PBO mailbox readback";

/// Sleek dark high-contrast palette inspired by `btop`.
mod palette {
    use ratatui::style::Color;

    pub const CYAN: Color = Color::Rgb(56, 189, 248);
    pub const TEAL: Color = Color::Rgb(45, 212, 191);
    pub const GREEN: Color = Color::Rgb(74, 222, 128);
    pub const YELLOW: Color = Color::Rgb(250, 204, 21);
    pub const ORANGE: Color = Color::Rgb(251, 146, 60);
    pub const RED: Color = Color::Rgb(248, 113, 113);
    pub const PURPLE: Color = Color::Rgb(192, 132, 252);
    pub const FG: Color = Color::Rgb(226, 232, 240);
    pub const MUTED: Color = Color::Rgb(148, 163, 184);
    pub const DIM: Color = Color::Rgb(100, 116, 139);
    pub const BORDER: Color = Color::Rgb(71, 85, 105);
    pub const BAR_BG: Color = Color::Rgb(15, 23, 42);
    pub const ROW_HL: Color = Color::Rgb(30, 41, 59);
    pub const GAUGE_TRACK: Color = Color::Rgb(51, 65, 85);
    pub const BADGE_FG: Color = Color::Rgb(15, 23, 42);
    pub const POPUP_BG: Color = Color::Rgb(15, 23, 42);
}

// ---------------------------------------------------------------------------
// Terminal safety: RAII TerminalGuard + one-time panic hook
// ---------------------------------------------------------------------------

static PANIC_HOOK_INSTALLED: Once = Once::new();
static TERMINAL_ACTIVE: AtomicBool = AtomicBool::new(false);

fn install_panic_hook_once() {
    PANIC_HOOK_INSTALLED.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            restore_terminal_best_effort();
            prev(info);
        }));
    });
}

fn restore_terminal_best_effort() {
    if TERMINAL_ACTIVE.swap(false, Ordering::SeqCst) {
        let _ = disable_raw_mode();
        let mut out = io::stdout();
        let _ = execute!(out, LeaveAlternateScreen, Show);
        let _ = out.flush();
    }
}

fn tty_io_err(source: io::Error) -> Error {
    Error::Io {
        path: "tty".to_string(),
        source,
    }
}

/// RAII guard that enters raw mode and alternate screen on creation and
/// unconditionally restores terminal state on drop, error, or panic.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    fn new() -> zensmu_core::Result<Self> {
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

// ---------------------------------------------------------------------------
// Rolling history ring buffer & statistics
// ---------------------------------------------------------------------------

/// Fixed-capacity rolling history buffer for live telemetry series.
#[derive(Debug, Clone)]
pub(crate) struct HistoryBuffer {
    capacity: usize,
    samples: VecDeque<f64>,
}

impl HistoryBuffer {
    pub(crate) fn new(capacity: usize) -> Self {
        let cap = capacity.max(1);
        Self {
            capacity: cap,
            samples: VecDeque::with_capacity(cap),
        }
    }

    pub(crate) fn push(&mut self, value: f64) {
        if !value.is_finite() {
            return;
        }
        if self.samples.len() == self.capacity {
            self.samples.pop_front();
        }
        self.samples.push_back(value);
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.samples.len()
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn latest(&self) -> Option<f64> {
        self.samples.back().copied()
    }

    pub(crate) fn min(&self) -> Option<f64> {
        self.samples.iter().copied().reduce(f64::min)
    }

    pub(crate) fn max(&self) -> Option<f64> {
        self.samples.iter().copied().reduce(f64::max)
    }

    pub(crate) fn avg(&self) -> Option<f64> {
        if self.samples.is_empty() {
            return None;
        }
        let sum: f64 = self.samples.iter().sum();
        Some(sum / self.samples.len() as f64)
    }

    /// Returns `(min, avg, max)` across all buffered samples.
    pub(crate) fn stats(&self) -> Option<(f64, f64, f64)> {
        Some((self.min()?, self.avg()?, self.max()?))
    }

    /// Convert the trailing `max_points` samples into non-negative `u64` values
    /// scaled by `scale` for `ratatui::widgets::Sparkline`.
    pub(crate) fn sparkline_window(&self, max_points: usize, scale: f64) -> Vec<u64> {
        if max_points == 0 || self.samples.is_empty() {
            return Vec::new();
        }
        let skip = self.samples.len().saturating_sub(max_points);
        self.samples
            .iter()
            .skip(skip)
            .map(|&v| {
                let scaled = (v * scale).max(0.0).round();
                if scaled.is_finite() {
                    scaled as u64
                } else {
                    0
                }
            })
            .collect()
    }
}

/// Rolling histories for all tracked PM table telemetry metrics.
#[derive(Debug, Clone)]
struct TelemetryHistory {
    ppt_w: HistoryBuffer,
    tctl_c: HistoryBuffer,
    tdc_a: HistoryBuffer,
    edc_a: HistoryBuffer,
    fclk_mhz: HistoryBuffer,
    uclk_mhz: HistoryBuffer,
    mclk_mhz: HistoryBuffer,
    vddcr_soc_v: HistoryBuffer,
}

impl TelemetryHistory {
    fn new(capacity: usize) -> Self {
        Self {
            ppt_w: HistoryBuffer::new(capacity),
            tctl_c: HistoryBuffer::new(capacity),
            tdc_a: HistoryBuffer::new(capacity),
            edc_a: HistoryBuffer::new(capacity),
            fclk_mhz: HistoryBuffer::new(capacity),
            uclk_mhz: HistoryBuffer::new(capacity),
            mclk_mhz: HistoryBuffer::new(capacity),
            vddcr_soc_v: HistoryBuffer::new(capacity),
        }
    }

    fn record(&mut self, snap: &TelemetrySnapshot) {
        if let Some(v) = snap.ppt_value_w {
            self.ppt_w.push(f64::from(v));
        }
        if let Some(v) = snap.tctl_value_c {
            self.tctl_c.push(f64::from(v));
        }
        if let Some(v) = snap.tdc_value_a {
            self.tdc_a.push(f64::from(v));
        }
        if let Some(v) = snap.edc_value_a {
            self.edc_a.push(f64::from(v));
        }
        if let Some(v) = snap.fclk_mhz {
            self.fclk_mhz.push(f64::from(v));
        }
        if let Some(v) = snap.uclk_mhz {
            self.uclk_mhz.push(f64::from(v));
        }
        if let Some(v) = snap.mclk_mhz {
            self.mclk_mhz.push(f64::from(v));
        }
        if let Some(v) = snap.vddcr_soc_v {
            self.vddcr_soc_v.push(f64::from(v));
        }
    }
}

// ---------------------------------------------------------------------------
// View state, interactive tuning model & mailbox load tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewTab {
    Overview,
    ClocksVolts,
    CoShaper,
    Tune,
}

impl ViewTab {
    fn next(self) -> Self {
        match self {
            Self::Overview => Self::ClocksVolts,
            Self::ClocksVolts => Self::CoShaper,
            Self::CoShaper => Self::Tune,
            Self::Tune => Self::Overview,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Overview => Self::Tune,
            Self::ClocksVolts => Self::Overview,
            Self::CoShaper => Self::ClocksVolts,
            Self::Tune => Self::CoShaper,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FlashKind {
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuneSection {
    CurveOptimizer,
    CurveShaper,
    Pbo,
    Apu,
}

impl TuneSection {
    fn title(self) -> &'static str {
        match self {
            Self::CurveOptimizer => "Curve Optimizer",
            Self::CurveShaper => "Curve Shaper (Zen 5)",
            Self::Pbo => "PBO Limits & Boost",
            Self::Apu => "APU DPTC Controls",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum TuneParam {
    CoAll,
    CoCore(u32),
    CoIgpu,
    Shaper { tier: FreqTier, band: TempBand },
    PboPpt,
    PboTdc,
    PboEdc,
    PboTctl,
    PboScalar,
    PboFmax,
    ApuStapm,
    ApuStapmTime,
    ApuFast,
    ApuSlow,
    ApuSlowTime,
    ApuApuSlow,
    ApuTdcSoc,
    ApuEdcSoc,
    ApuSkinTemp,
    ApuDgpuSkinTemp,
    ApuPowerMode,
}

impl TuneParam {
    fn section(self) -> TuneSection {
        match self {
            Self::CoAll | Self::CoCore(_) | Self::CoIgpu => TuneSection::CurveOptimizer,
            Self::Shaper { .. } => TuneSection::CurveShaper,
            Self::PboPpt
            | Self::PboTdc
            | Self::PboEdc
            | Self::PboTctl
            | Self::PboScalar
            | Self::PboFmax => TuneSection::Pbo,
            Self::ApuStapm
            | Self::ApuStapmTime
            | Self::ApuFast
            | Self::ApuSlow
            | Self::ApuSlowTime
            | Self::ApuApuSlow
            | Self::ApuTdcSoc
            | Self::ApuEdcSoc
            | Self::ApuSkinTemp
            | Self::ApuDgpuSkinTemp
            | Self::ApuPowerMode => TuneSection::Apu,
        }
    }

    fn is_co_or_shaper(self) -> bool {
        matches!(
            self,
            Self::CoAll | Self::CoCore(_) | Self::CoIgpu | Self::Shaper { .. }
        )
    }

    fn sort_key(self) -> (u8, u32, u8) {
        match self {
            Self::CoAll => (0, 0, 0),
            Self::CoCore(idx) => (1, idx, 0),
            Self::CoIgpu => (2, 0, 0),
            Self::Shaper { tier, band } => {
                let b = match band {
                    TempBand::Low => 0,
                    TempBand::Med => 1,
                    TempBand::High => 2,
                };
                (3, tier.as_u32(), b)
            }
            Self::PboPpt => (4, 0, 0),
            Self::PboTdc => (5, 0, 0),
            Self::PboEdc => (6, 0, 0),
            Self::PboTctl => (7, 0, 0),
            Self::PboScalar => (8, 0, 0),
            Self::PboFmax => (9, 0, 0),
            Self::ApuStapm => (10, 0, 0),
            Self::ApuStapmTime => (11, 0, 0),
            Self::ApuFast => (12, 0, 0),
            Self::ApuSlow => (13, 0, 0),
            Self::ApuSlowTime => (14, 0, 0),
            Self::ApuApuSlow => (15, 0, 0),
            Self::ApuTdcSoc => (16, 0, 0),
            Self::ApuEdcSoc => (17, 0, 0),
            Self::ApuSkinTemp => (18, 0, 0),
            Self::ApuDgpuSkinTemp => (19, 0, 0),
            Self::ApuPowerMode => (20, 0, 0),
        }
    }
}

impl PartialOrd for TuneParam {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TuneParam {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

fn tune_param_from_clamped_field(field: &str) -> Option<TuneParam> {
    match field {
        "pbo.ppt_w" => Some(TuneParam::PboPpt),
        "pbo.tdc_a" => Some(TuneParam::PboTdc),
        "pbo.edc_a" => Some(TuneParam::PboEdc),
        "pbo.tctl_c" => Some(TuneParam::PboTctl),
        "apu.stapm_w" => Some(TuneParam::ApuStapm),
        "apu.fast_w" => Some(TuneParam::ApuFast),
        "apu.slow_w" => Some(TuneParam::ApuSlow),
        "apu.apu_slow_w" => Some(TuneParam::ApuApuSlow),
        "apu.tdc_soc_a" => Some(TuneParam::ApuTdcSoc),
        "apu.edc_soc_a" => Some(TuneParam::ApuEdcSoc),
        "apu.apu_skin_temp_c" => Some(TuneParam::ApuSkinTemp),
        "apu.dgpu_skin_temp_c" => Some(TuneParam::ApuDgpuSkinTemp),
        _ => None,
    }
}

fn param_unit(param: TuneParam) -> &'static str {
    match param {
        TuneParam::PboPpt
        | TuneParam::ApuStapm
        | TuneParam::ApuFast
        | TuneParam::ApuSlow
        | TuneParam::ApuApuSlow => "W",
        TuneParam::PboTdc | TuneParam::PboEdc | TuneParam::ApuTdcSoc | TuneParam::ApuEdcSoc => "A",
        TuneParam::PboTctl | TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp => "°C",
        TuneParam::ApuStapmTime | TuneParam::ApuSlowTime => "s",
        TuneParam::PboScalar => "x",
        TuneParam::PboFmax => "MHz",
        TuneParam::CoAll
        | TuneParam::CoCore(_)
        | TuneParam::CoIgpu
        | TuneParam::Shaper { .. }
        | TuneParam::ApuPowerMode => "",
    }
}

fn format_board_cap(param: TuneParam, cap: u32) -> String {
    let unit = param_unit(param);
    if unit.is_empty() {
        format!("board cap ≤{cap}")
    } else {
        format!("board cap ≤{cap} {unit}")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuneValue {
    Int(i32),
    PowerMode(Option<ApuPowerMode>),
}

impl TuneValue {
    fn as_int(self) -> Option<i32> {
        match self {
            Self::Int(v) => Some(v),
            Self::PowerMode(_) => None,
        }
    }

    fn as_power_mode(self) -> Option<ApuPowerMode> {
        match self {
            Self::PowerMode(m) => m,
            Self::Int(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
enum TuneRow {
    SectionHeader(String, String),
    InfoBanner(String, Color),
    Param(TuneParam),
}

#[derive(Debug, Clone)]
enum ConfirmAction {
    ApplyStaged {
        co_all: Option<i32>,
        co_cores: Vec<(u32, i32)>,
        co_igpu: Option<i32>,
        shaper_tiers: Vec<(FreqTier, ShaperTierMargins)>,
        pbo: Option<PboSetRequest>,
        apu: Option<ApuSetRequest>,
    },
    Rollback(SmuStateSnapshot),
    ResetBootBaseline(SmuStateSnapshot),
    ResetCoZero,
    PersistLiveAndEnable,
    ToggleBootService {
        enable: bool,
    },
    ClearBootGuard,
}

impl ConfirmAction {
    fn is_persist_action(&self) -> bool {
        matches!(
            self,
            Self::PersistLiveAndEnable | Self::ToggleBootService { .. } | Self::ClearBootGuard
        )
    }
}

#[derive(Debug, Clone)]
struct ConfirmModal {
    title: String,
    subtitle: String,
    action: ConfirmAction,
    ops: Vec<PlannedOp>,
    steps: Vec<(String, String)>,
    scroll: usize,
    allow_extreme: bool,
}

impl ConfirmModal {
    fn ops_modal(
        title: impl Into<String>,
        subtitle: impl Into<String>,
        action: ConfirmAction,
        ops: Vec<PlannedOp>,
        allow_extreme: bool,
    ) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            action,
            ops,
            steps: Vec::new(),
            scroll: 0,
            allow_extreme,
        }
    }

    fn steps_modal(
        title: impl Into<String>,
        subtitle: impl Into<String>,
        action: ConfirmAction,
        steps: Vec<(String, String)>,
        allow_extreme: bool,
    ) -> Self {
        Self {
            title: title.into(),
            subtitle: subtitle.into(),
            action,
            ops: Vec::new(),
            steps,
            scroll: 0,
            allow_extreme,
        }
    }

    fn item_count(&self) -> usize {
        if self.steps.is_empty() {
            self.ops.len()
        } else {
            self.steps.len()
        }
    }
}

#[derive(Debug, Clone)]
enum MailboxState<T> {
    Loaded(T),
    PermissionDenied,
    Unsupported,
    FirmwareUnsupported(String),
    Error(String),
}

impl<T> MailboxState<T> {
    fn from_result(res: zensmu_core::Result<T>) -> Self {
        match res {
            Ok(v) => Self::Loaded(v),
            Err(Error::PermissionDenied { .. }) => Self::PermissionDenied,
            Err(Error::UnsupportedCapability { .. }) => Self::Unsupported,
            Err(Error::SmuRejected { status, .. })
                if status.contains("0xFE") || status.contains("0xFD") =>
            {
                let code = if status.contains("0xFE") {
                    "0xFE"
                } else {
                    "0xFD"
                };
                Self::FirmwareUnsupported(code.to_string())
            }
            Err(err) => Self::Error(err.to_string()),
        }
    }

    fn as_loaded(&self) -> Option<&T> {
        match self {
            Self::Loaded(v) => Some(v),
            _ => None,
        }
    }

    fn is_permission_denied(&self) -> bool {
        matches!(self, Self::PermissionDenied)
    }
}

struct App {
    active_tab: ViewTab,
    show_help: bool,
    paused: bool,
    interval_ms: u64,
    sample_count: u64,
    is_root: bool,
    #[cfg(test)]
    test_allow_writes: bool,
    telemetry: Option<TelemetrySnapshot>,
    telemetry_error: Option<String>,
    history: TelemetryHistory,
    co_state: MailboxState<CoSnapshot>,
    shaper_state: MailboxState<CurveShaperState>,
    pbo_state: MailboxState<PboStatus>,
    mailbox_note: Option<String>,
    status_flash: Option<(String, FlashKind, Instant)>,
    last_alert: Option<(String, FlashKind)>,
    board_info: SystemBoardInfo,
    // Interactive Tune state (Tab 4)
    tune_selected: usize,
    staged: HashMap<TuneParam, TuneValue>,
    board_caps: BTreeMap<TuneParam, u32>,
    allow_extreme: bool,
    confirm_modal: Option<ConfirmModal>,
    snapshot_store: SnapshotStore,
    persist_paths: PersistPaths,
    persist_status: PersistStatus,
    // Live telemetry recording (unprivileged)
    recorder: Option<TelemetryRecorder>,
    record_started: Option<Instant>,
    record_format: RecordFormat,
    #[cfg(test)]
    record_dir_override: Option<PathBuf>,
}

fn default_persist_status(paths: &PersistPaths) -> PersistStatus {
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
    fn new(interval_ms: u64) -> Self {
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

    fn refresh_persist_status(&mut self, hw: &HardwareContext) {
        self.persist_status = persist::read_persist_status(
            &self.persist_paths,
            Some(hw.codename),
            Some(hw.topology.physical_cores),
        );
    }

    fn can_write(&self) -> bool {
        #[cfg(test)]
        {
            self.is_root || self.test_allow_writes
        }
        #[cfg(not(test))]
        {
            self.is_root
        }
    }

    fn set_flash(&mut self, msg: impl Into<String>) {
        self.set_flash_kind(msg, FlashKind::Info);
    }

    fn set_flash_kind(&mut self, msg: impl Into<String>, kind: FlashKind) {
        self.status_flash = Some((msg.into(), kind, Instant::now()));
    }

    fn active_flash(&self) -> Option<(&str, FlashKind)> {
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

    fn staged_count(&self) -> usize {
        self.staged.len()
    }

    fn poll_telemetry(&mut self, transport: &dyn SmuTransport, hw: &HardwareContext) {
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

    fn toggle_recording(&mut self, hw: &HardwareContext) {
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

    fn cycle_record_format(&mut self) {
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

    fn refresh_mailbox(&mut self, transport: &dyn SmuTransport, hw: &HardwareContext) {
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

    fn has_any_mailbox_data(&self) -> bool {
        self.co_state.as_loaded().is_some()
            || self.shaper_state.as_loaded().is_some()
            || self.pbo_state.as_loaded().is_some()
    }

    // -----------------------------------------------------------------------
    // Tune parameter catalog, bounds, live values & stepping
    // -----------------------------------------------------------------------

    fn build_tune_rows(&self, hw: &HardwareContext) -> Vec<TuneRow> {
        let mut rows = Vec::new();
        self.push_co_tune_rows(hw, &mut rows);
        self.push_shaper_tune_rows(hw, &mut rows);
        self.push_pbo_tune_rows(hw, &mut rows);
        self.push_apu_tune_rows(hw, &mut rows);
        rows
    }

    fn push_co_tune_rows(&self, hw: &HardwareContext, rows: &mut Vec<TuneRow>) {
        let caps = &hw.capabilities;
        if caps.set_co_per_core.is_none() && caps.set_co_all_cores.is_none() {
            return;
        }
        let bounds_hint = caps
            .co_bounds
            .map(|b| format!("{}..=+{} (soft >= {})", b.min, b.max, b.soft_min))
            .unwrap_or_default();
        rows.push(TuneRow::SectionHeader(
            TuneSection::CurveOptimizer.title().to_string(),
            bounds_hint,
        ));
        if caps.set_co_all_cores.is_some() {
            rows.push(TuneRow::Param(TuneParam::CoAll));
        }
        if caps.set_co_per_core.is_some() {
            for core in &hw.topology.cores {
                rows.push(TuneRow::Param(TuneParam::CoCore(core.index)));
            }
        }
        if caps.set_igpu_co.is_some() {
            rows.push(TuneRow::Param(TuneParam::CoIgpu));
        }
    }

    fn push_shaper_tune_rows(&self, hw: &HardwareContext, rows: &mut Vec<TuneRow>) {
        if hw.capabilities.set_curve_shaper.is_none() {
            return;
        }
        rows.push(TuneRow::SectionHeader(
            TuneSection::CurveShaper.title().to_string(),
            format!(
                "{}..=+{} (soft >= {})",
                shaper::SHAPER_MIN,
                shaper::SHAPER_MAX,
                shaper::SHAPER_SOFT_MIN
            ),
        ));
        if let MailboxState::FirmwareUnsupported(code) = &self.shaper_state {
            rows.push(TuneRow::InfoBanner(
                format!("Not exposed by SMU {} ({code})", hw.driver.smu_version),
                palette::YELLOW,
            ));
        } else {
            for tier in FreqTier::ALL {
                for band in TempBand::ALL {
                    rows.push(TuneRow::Param(TuneParam::Shaper { tier, band }));
                }
            }
        }
    }

    fn push_pbo_tune_rows(&self, hw: &HardwareContext, rows: &mut Vec<TuneRow>) {
        let caps = &hw.capabilities;
        let pbo_params = [
            (caps.set_ppt.is_some(), TuneParam::PboPpt),
            (caps.set_tdc.is_some(), TuneParam::PboTdc),
            (caps.set_edc.is_some(), TuneParam::PboEdc),
            (caps.set_tctl.is_some(), TuneParam::PboTctl),
            (caps.set_pbo_scalar.is_some(), TuneParam::PboScalar),
            (caps.set_fmax.is_some(), TuneParam::PboFmax),
        ];
        if !pbo_params.iter().any(|(supported, _)| *supported) {
            return;
        }
        let pbo_hint = match self.pbo_state.as_loaded() {
            Some(pbo) => {
                let mut parts = Vec::new();
                if let Some(tdp) = pbo.fused.tdp_w {
                    parts.push(format!("TDP {tdp}W"));
                }
                if let Some(tjmax) = pbo.fused.tjmax_c {
                    parts.push(format!("TjMax {tjmax}°C"));
                }
                if parts.is_empty() {
                    format!("Tctl max {}°C", pbo::TCTL_MAX_C)
                } else {
                    parts.join(" · ")
                }
            }
            None => format!("Tctl max {}°C", pbo::TCTL_MAX_C),
        };
        rows.push(TuneRow::SectionHeader(
            TuneSection::Pbo.title().to_string(),
            pbo_hint,
        ));
        for (supported, param) in pbo_params {
            if supported {
                rows.push(TuneRow::Param(param));
            }
        }
    }

    fn push_apu_tune_rows(&self, hw: &HardwareContext, rows: &mut Vec<TuneRow>) {
        let caps = &hw.capabilities;
        if caps.set_stapm_limit.is_none() {
            return;
        }
        rows.push(TuneRow::SectionHeader(
            TuneSection::Apu.title().to_string(),
            "Mobile Power & Thermal".to_string(),
        ));
        let apu_params = [
            (true, TuneParam::ApuStapm),
            (caps.set_stapm_time.is_some(), TuneParam::ApuStapmTime),
            (
                hw.codename.is_apu() && caps.set_ppt.is_some(),
                TuneParam::ApuFast,
            ),
            (caps.set_slow_limit.is_some(), TuneParam::ApuSlow),
            (caps.set_slow_time.is_some(), TuneParam::ApuSlowTime),
            (caps.set_apu_slow_limit.is_some(), TuneParam::ApuApuSlow),
            (caps.set_tdc_soc.is_some(), TuneParam::ApuTdcSoc),
            (caps.set_edc_soc.is_some(), TuneParam::ApuEdcSoc),
            (caps.set_apu_skin_temp.is_some(), TuneParam::ApuSkinTemp),
            (
                caps.set_dgpu_skin_temp.is_some(),
                TuneParam::ApuDgpuSkinTemp,
            ),
            (
                caps.set_max_performance.is_some() || caps.set_power_saving.is_some(),
                TuneParam::ApuPowerMode,
            ),
        ];
        for (supported, param) in apu_params {
            if supported {
                rows.push(TuneRow::Param(param));
            }
        }
    }

    fn tune_params(&self, hw: &HardwareContext) -> Vec<TuneParam> {
        self.build_tune_rows(hw)
            .into_iter()
            .filter_map(|r| match r {
                TuneRow::Param(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    fn clamp_tune_selection(&mut self, hw: &HardwareContext) {
        let count = self.tune_params(hw).len();
        if count == 0 {
            self.tune_selected = 0;
        } else if self.tune_selected >= count {
            self.tune_selected = count - 1;
        }
    }

    fn selected_tune_param(&self, hw: &HardwareContext) -> Option<TuneParam> {
        let params = self.tune_params(hw);
        params.get(self.tune_selected).copied()
    }

    fn live_telemetry_int(
        &self,
        getter: impl FnOnce(&TelemetrySnapshot) -> Option<f32>,
    ) -> Option<TuneValue> {
        self.telemetry
            .as_ref()
            .and_then(getter)
            .filter(|v| v.is_finite())
            .map(|v| TuneValue::Int(v.round() as i32))
    }

    fn live_telemetry_or_fused(
        &self,
        telem_getter: impl FnOnce(&TelemetrySnapshot) -> Option<f32>,
        fused_getter: impl FnOnce(&FusedLimits) -> Option<u32>,
    ) -> Option<TuneValue> {
        self.live_telemetry_int(telem_getter).or_else(|| {
            self.pbo_state
                .as_loaded()
                .and_then(|p| fused_getter(&p.fused))
                .map(|v| TuneValue::Int(v as i32))
        })
    }

    /// Returns the known live value of `param` from hardware telemetry / mailbox readback.
    fn live_param_value(&self, param: TuneParam) -> Option<TuneValue> {
        match param {
            TuneParam::CoAll => {
                let snap = self.co_state.as_loaded()?;
                let first = snap.cores.first()?.margin;
                snap.cores
                    .iter()
                    .all(|c| c.margin == first)
                    .then_some(TuneValue::Int(i32::from(first)))
            }
            TuneParam::CoCore(idx) => self
                .co_state
                .as_loaded()
                .and_then(|s| s.cores.iter().find(|c| c.core_index == idx))
                .map(|c| TuneValue::Int(i32::from(c.margin))),
            TuneParam::CoIgpu => self
                .co_state
                .as_loaded()
                .and_then(|s| s.igpu)
                .map(|v| TuneValue::Int(i32::from(v))),
            TuneParam::Shaper { tier, band } => {
                let cs = self.shaper_state.as_loaded()?;
                let t = cs.tier(tier);
                let v = match band {
                    TempBand::Low => t.low,
                    TempBand::Med => t.med,
                    TempBand::High => t.high,
                };
                Some(TuneValue::Int(i32::from(v)))
            }
            TuneParam::PboPpt => self.live_telemetry_or_fused(|t| t.ppt_limit_w, |f| f.ppt_w),
            TuneParam::PboTdc => self.live_telemetry_or_fused(|t| t.tdc_limit_a, |f| f.vdd_tdc_a),
            TuneParam::PboEdc => self.live_telemetry_or_fused(|t| t.edc_limit_a, |f| f.edc_a),
            TuneParam::PboTctl => self.live_telemetry_int(|t| t.tctl_limit_c),
            TuneParam::PboScalar => self
                .pbo_state
                .as_loaded()
                .and_then(|p| p.scalar)
                .filter(|v| v.is_finite() && *v >= 1.0)
                .map(|v| TuneValue::Int(v.round().clamp(1.0, 10.0) as i32)),
            TuneParam::PboFmax => self
                .pbo_state
                .as_loaded()
                .and_then(|p| p.fmax_mhz)
                .map(|v| TuneValue::Int(v as i32)),
            TuneParam::ApuStapm => self.live_telemetry_or_fused(|t| t.stapm_limit_w, |f| f.slow_w),
            TuneParam::ApuStapmTime | TuneParam::ApuSlowTime => None,
            TuneParam::ApuFast => self.live_telemetry_or_fused(|t| t.ppt_limit_w, |f| f.fast_w),
            TuneParam::ApuSlow => self.live_telemetry_or_fused(|t| t.slow_limit_w, |f| f.slow_w),
            TuneParam::ApuApuSlow => {
                self.live_telemetry_or_fused(|t| t.apu_slow_limit_w, |f| f.apu_slow_w)
            }
            TuneParam::ApuTdcSoc => {
                self.live_telemetry_or_fused(|t| t.tdc_soc_limit_a, |f| f.soc_tdc_a)
            }
            TuneParam::ApuEdcSoc => self.live_telemetry_int(|t| t.edc_soc_limit_a),
            TuneParam::ApuSkinTemp => self.live_telemetry_int(|t| t.apu_skin_temp_limit_c),
            TuneParam::ApuDgpuSkinTemp => self.live_telemetry_int(|t| t.dgpu_skin_temp_limit_c),
            TuneParam::ApuPowerMode => Some(TuneValue::PowerMode(None)),
        }
    }

    /// Fallback starting value when stepping a parameter whose live value is unknown.
    fn default_step_base(&self, param: TuneParam) -> TuneValue {
        match param {
            TuneParam::CoAll
            | TuneParam::CoCore(_)
            | TuneParam::CoIgpu
            | TuneParam::Shaper { .. } => TuneValue::Int(0),
            TuneParam::PboPpt => TuneValue::Int(88),
            TuneParam::PboTdc => TuneValue::Int(75),
            TuneParam::PboEdc => TuneValue::Int(150),
            TuneParam::PboTctl => TuneValue::Int(85),
            TuneParam::PboScalar => TuneValue::Int(1),
            TuneParam::PboFmax => TuneValue::Int(5000),
            TuneParam::ApuStapm => TuneValue::Int(28),
            TuneParam::ApuStapmTime => TuneValue::Int(200),
            TuneParam::ApuFast => TuneValue::Int(45),
            TuneParam::ApuSlow => TuneValue::Int(35),
            TuneParam::ApuSlowTime => TuneValue::Int(5),
            TuneParam::ApuApuSlow => TuneValue::Int(30),
            TuneParam::ApuTdcSoc => TuneValue::Int(20),
            TuneParam::ApuEdcSoc => TuneValue::Int(35),
            TuneParam::ApuSkinTemp => TuneValue::Int(45),
            TuneParam::ApuDgpuSkinTemp => TuneValue::Int(55),
            TuneParam::ApuPowerMode => TuneValue::PowerMode(None),
        }
    }

    fn param_int_bounds(&self, param: TuneParam, hw: &HardwareContext) -> (i32, i32) {
        match param {
            TuneParam::CoAll | TuneParam::CoCore(_) => hw
                .capabilities
                .co_bounds
                .map(|b| (b.min, b.max))
                .unwrap_or((-30, 30)),
            TuneParam::CoIgpu => (co::IGPU_CO_MIN, co::IGPU_CO_MAX),
            TuneParam::Shaper { .. } => {
                (i32::from(shaper::SHAPER_MIN), i32::from(shaper::SHAPER_MAX))
            }
            TuneParam::PboPpt => (pbo::PPT_MIN_W as i32, pbo::PPT_MAX_W as i32),
            TuneParam::PboTdc => (pbo::TDC_MIN_A as i32, pbo::TDC_MAX_A as i32),
            TuneParam::PboEdc => (pbo::EDC_MIN_A as i32, pbo::EDC_MAX_A as i32),
            TuneParam::PboTctl => (pbo::TCTL_MIN_C as i32, pbo::TCTL_MAX_C as i32),
            TuneParam::PboScalar => (pbo::SCALAR_MIN as i32, pbo::SCALAR_MAX as i32),
            TuneParam::PboFmax => (pbo::FMAX_MIN_MHZ as i32, pbo::FMAX_MAX_MHZ as i32),
            TuneParam::ApuStapm
            | TuneParam::ApuFast
            | TuneParam::ApuSlow
            | TuneParam::ApuApuSlow => (apu::APU_POWER_MIN_W as i32, apu::APU_POWER_MAX_W as i32),
            TuneParam::ApuStapmTime | TuneParam::ApuSlowTime => {
                (apu::APU_TIME_MIN_S as i32, apu::APU_TIME_MAX_S as i32)
            }
            TuneParam::ApuTdcSoc => (apu::APU_SOC_TDC_MIN_A as i32, apu::APU_SOC_TDC_MAX_A as i32),
            TuneParam::ApuEdcSoc => (apu::APU_SOC_EDC_MIN_A as i32, apu::APU_SOC_EDC_MAX_A as i32),
            TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp => (
                apu::APU_SKIN_TEMP_MIN_C as i32,
                apu::APU_SKIN_TEMP_MAX_C as i32,
            ),
            TuneParam::ApuPowerMode => (0, 2),
        }
    }

    fn param_steps(param: TuneParam) -> (i32, i32) {
        match param {
            TuneParam::CoAll
            | TuneParam::CoCore(_)
            | TuneParam::CoIgpu
            | TuneParam::Shaper { .. } => (1, 5),
            TuneParam::PboPpt => (5, 25),
            TuneParam::PboTdc => (5, 20),
            TuneParam::PboEdc => (5, 25),
            TuneParam::PboTctl => (1, 5),
            TuneParam::PboScalar => (1, 2),
            TuneParam::PboFmax => (25, 100),
            TuneParam::ApuStapm
            | TuneParam::ApuFast
            | TuneParam::ApuSlow
            | TuneParam::ApuApuSlow => (1, 5),
            TuneParam::ApuStapmTime => (5, 30),
            TuneParam::ApuSlowTime => (1, 10),
            TuneParam::ApuTdcSoc | TuneParam::ApuEdcSoc => (1, 5),
            TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp => (1, 5),
            TuneParam::ApuPowerMode => (1, 1),
        }
    }

    fn param_exceeds_soft_limit(
        &self,
        param: TuneParam,
        val: TuneValue,
        hw: &HardwareContext,
    ) -> bool {
        let Some(v) = val.as_int() else {
            return false;
        };
        match param {
            TuneParam::CoAll | TuneParam::CoCore(_) => {
                hw.capabilities.co_bounds.is_some_and(|b| v < b.soft_min)
            }
            TuneParam::CoIgpu => v < co::IGPU_CO_SOFT_MIN,
            TuneParam::Shaper { .. } => v < i32::from(shaper::SHAPER_SOFT_MIN),
            TuneParam::PboPpt => v > pbo::PPT_SOFT_MAX_W as i32,
            TuneParam::PboTdc => v > pbo::TDC_SOFT_MAX_A as i32,
            TuneParam::PboEdc => v > pbo::EDC_SOFT_MAX_A as i32,
            TuneParam::PboTctl => false, // hard max 95 C always enforced
            TuneParam::PboScalar => v > pbo::SCALAR_SOFT_MAX as i32,
            TuneParam::PboFmax => self
                .pbo_state
                .as_loaded()
                .and_then(|p| p.fmax_mhz)
                .is_some_and(|cur| v > (cur.saturating_add(pbo::FMAX_SOFT_DELTA_MHZ)) as i32),
            TuneParam::ApuStapm | TuneParam::ApuSlow | TuneParam::ApuApuSlow => {
                v > apu::APU_POWER_SOFT_MAX_W as i32
            }
            TuneParam::ApuFast => v > apu::APU_FAST_POWER_SOFT_MAX_W as i32,
            TuneParam::ApuStapmTime | TuneParam::ApuSlowTime => false,
            TuneParam::ApuTdcSoc => v > apu::APU_SOC_TDC_SOFT_MAX_A as i32,
            TuneParam::ApuEdcSoc => v > apu::APU_SOC_EDC_SOFT_MAX_A as i32,
            TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp => {
                v > apu::APU_SKIN_TEMP_SOFT_MAX_C as i32
            }
            TuneParam::ApuPowerMode => false,
        }
    }

    fn adjust_selected_param(&mut self, hw: &HardwareContext, delta_sign: i32, coarse: bool) {
        let Some(param) = self.selected_tune_param(hw) else {
            return;
        };

        let live = self.live_param_value(param);
        let current = self
            .staged
            .get(&param)
            .copied()
            .or(live)
            .unwrap_or_else(|| self.default_step_base(param));

        let next = match (param, current) {
            (TuneParam::ApuPowerMode, TuneValue::PowerMode(cur_mode)) => {
                let idx = match cur_mode {
                    None => 0i32,
                    Some(ApuPowerMode::MaxPerformance) => 1,
                    Some(ApuPowerMode::PowerSaving) => 2,
                };
                let next_idx = (idx + delta_sign).clamp(0, 2);
                let next_mode = match next_idx {
                    1 => Some(ApuPowerMode::MaxPerformance),
                    2 => Some(ApuPowerMode::PowerSaving),
                    _ => None,
                };
                TuneValue::PowerMode(next_mode)
            }
            (_, TuneValue::Int(cur_int)) => {
                let (fine, big) = Self::param_steps(param);
                let step = if coarse { big } else { fine };
                let (min_v, max_v) = self.param_int_bounds(param, hw);
                let next_v = cur_int
                    .saturating_add(delta_sign * step)
                    .clamp(min_v, max_v);
                TuneValue::Int(next_v)
            }
            _ => return,
        };

        if live == Some(next)
            || (param == TuneParam::ApuPowerMode && next == TuneValue::PowerMode(None))
        {
            self.staged.remove(&param);
        } else {
            self.staged.insert(param, next);
        }
    }

    fn reset_selected_param(&mut self, hw: &HardwareContext, to_zero_if_co: bool) {
        let Some(param) = self.selected_tune_param(hw) else {
            return;
        };
        let live = self.live_param_value(param);
        if to_zero_if_co && param.is_co_or_shaper() {
            if live == Some(TuneValue::Int(0)) {
                self.staged.remove(&param);
            } else {
                self.staged.insert(param, TuneValue::Int(0));
            }
        } else {
            self.staged.remove(&param);
        }
    }

    fn jump_tune_section(&mut self, hw: &HardwareContext, forward: bool) {
        let params = self.tune_params(hw);
        let Some(cur_param) = params
            .get(self.tune_selected.min(params.len().saturating_sub(1)))
            .copied()
        else {
            return;
        };
        let cur_sec = cur_param.section();

        // Collect first index of each distinct section in order
        let mut starts: Vec<(TuneSection, usize)> = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let s = p.section();
            if starts.last().map(|(ls, _)| *ls) != Some(s) {
                starts.push((s, i));
            }
        }
        if starts.len() <= 1 {
            return;
        }
        let sec_pos = starts.iter().position(|(s, _)| *s == cur_sec).unwrap_or(0);
        let next_pos = if forward {
            (sec_pos + 1) % starts.len()
        } else if sec_pos == 0 {
            starts.len() - 1
        } else {
            sec_pos - 1
        };
        if let Some(&(_, target_idx)) = starts.get(next_pos) {
            self.tune_selected = target_idx;
        }
    }

    // -----------------------------------------------------------------------
    // Dry-run preview & safe execution (flocked transport + snapshot + verify)
    // -----------------------------------------------------------------------

    fn current_co_all_summary(&self) -> Option<String> {
        let snap = self.co_state.as_loaded()?;
        let first = snap.cores.first()?;
        if snap.cores.iter().all(|c| c.margin == first.margin) {
            Some(format_signed_margin(first.margin))
        } else {
            Some("mixed".to_string())
        }
    }

    fn staged_int(&self, param: TuneParam) -> Option<i32> {
        self.staged.get(&param).and_then(|v| v.as_int())
    }

    fn staged_u32(&self, param: TuneParam) -> Option<u32> {
        self.staged_int(param).and_then(|v| u32::try_from(v).ok())
    }

    fn enrich_op_current(&self, op: &mut PlannedOp, rules: &[(&str, TuneParam, &str)]) {
        if op.current.is_some() {
            return;
        }
        for &(prefix, param, suffix) in rules {
            if op.description.starts_with(prefix) {
                op.current = self
                    .live_param_value(param)
                    .and_then(TuneValue::as_int)
                    .map(|v| format!("{v}{suffix}"));
                return;
            }
        }
    }

    fn collect_staged_shaper_tiers(
        &self,
        hw: &HardwareContext,
    ) -> Vec<(FreqTier, ShaperTierMargins)> {
        if hw.capabilities.set_curve_shaper.is_none()
            || matches!(self.shaper_state, MailboxState::FirmwareUnsupported(_))
        {
            return Vec::new();
        }
        let live_cs = self.shaper_state.as_loaded().copied().unwrap_or_default();
        let mut tiers = Vec::new();
        for tier in FreqTier::ALL {
            let s_low = self.staged_int(TuneParam::Shaper {
                tier,
                band: TempBand::Low,
            });
            let s_med = self.staged_int(TuneParam::Shaper {
                tier,
                band: TempBand::Med,
            });
            let s_high = self.staged_int(TuneParam::Shaper {
                tier,
                band: TempBand::High,
            });
            if s_low.is_some() || s_med.is_some() || s_high.is_some() {
                let base = live_cs.tier(tier);
                tiers.push((
                    tier,
                    ShaperTierMargins {
                        low: s_low.map(|v| v as i8).unwrap_or(base.low),
                        med: s_med.map(|v| v as i8).unwrap_or(base.med),
                        high: s_high.map(|v| v as i8).unwrap_or(base.high),
                    },
                ));
            }
        }
        tiers
    }

    fn collect_staged_pbo_request(&self) -> Option<PboSetRequest> {
        let req = PboSetRequest {
            ppt_w: self.staged_u32(TuneParam::PboPpt),
            tdc_a: self.staged_u32(TuneParam::PboTdc),
            edc_a: self.staged_u32(TuneParam::PboEdc),
            tctl_c: self.staged_u32(TuneParam::PboTctl),
            scalar: self.staged_u32(TuneParam::PboScalar),
            fmax_mhz: self.staged_u32(TuneParam::PboFmax),
        };
        (req != PboSetRequest::default()).then_some(req)
    }

    fn collect_staged_apu_request(&self) -> Option<ApuSetRequest> {
        let req = ApuSetRequest {
            stapm_w: self.staged_u32(TuneParam::ApuStapm),
            stapm_time_s: self.staged_u32(TuneParam::ApuStapmTime),
            fast_w: self.staged_u32(TuneParam::ApuFast),
            slow_w: self.staged_u32(TuneParam::ApuSlow),
            slow_time_s: self.staged_u32(TuneParam::ApuSlowTime),
            apu_slow_w: self.staged_u32(TuneParam::ApuApuSlow),
            tdc_soc_a: self.staged_u32(TuneParam::ApuTdcSoc),
            edc_soc_a: self.staged_u32(TuneParam::ApuEdcSoc),
            apu_skin_temp_c: self.staged_u32(TuneParam::ApuSkinTemp),
            dgpu_skin_temp_c: self.staged_u32(TuneParam::ApuDgpuSkinTemp),
            power_mode: self
                .staged
                .get(&TuneParam::ApuPowerMode)
                .and_then(|v| v.as_power_mode()),
        };
        (!req.is_empty()).then_some(req)
    }

    #[allow(clippy::too_many_arguments)]
    fn plan_staged_ops(
        &self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
        co_all: Option<i32>,
        co_cores: &[(u32, i32)],
        co_igpu: Option<i32>,
        shaper_tiers: &[(FreqTier, ShaperTierMargins)],
        pbo_req: Option<&PboSetRequest>,
        apu_req: Option<&ApuSetRequest>,
        allow_extreme: bool,
    ) -> zensmu_core::Result<Vec<PlannedOp>> {
        const PBO_ENRICH_RULES: &[(&str, TuneParam, &str)] = &[
            ("Set PPT", TuneParam::PboPpt, " W"),
            ("Set TDC", TuneParam::PboTdc, " A"),
            ("Set EDC", TuneParam::PboEdc, " A"),
            ("Set Tctl", TuneParam::PboTctl, " C"),
            ("Set PBO scalar", TuneParam::PboScalar, "x"),
            ("Set Fmax", TuneParam::PboFmax, " MHz"),
        ];
        const APU_ENRICH_RULES: &[(&str, TuneParam, &str)] = &[
            ("Set APU STAPM limit", TuneParam::ApuStapm, " W"),
            ("Set APU Fast PPT", TuneParam::ApuFast, " W"),
            ("Set APU Slow PPT limit", TuneParam::ApuSlow, " W"),
            ("Set APU-only Slow PPT", TuneParam::ApuApuSlow, " W"),
            ("Set APU SoC TDC", TuneParam::ApuTdcSoc, " A"),
            ("Set APU SoC EDC", TuneParam::ApuEdcSoc, " A"),
            ("Set APU skin", TuneParam::ApuSkinTemp, " C"),
            ("Set dGPU skin", TuneParam::ApuDgpuSkinTemp, " C"),
        ];

        let mut ops = Vec::new();
        if let Some(m) = co_all {
            let mut v = co::set_co_all(transport, hw, m, allow_extreme, true)?;
            for op in &mut v {
                if op.current.is_none() {
                    op.current = self.current_co_all_summary();
                }
            }
            ops.extend(v);
        }
        if !co_cores.is_empty() {
            let mut v = co::set_co_cores(transport, hw, co_cores, allow_extreme, true)?;
            for (op, &(idx, _)) in v.iter_mut().zip(co_cores.iter()) {
                if op.current.is_none() {
                    op.current = self
                        .live_param_value(TuneParam::CoCore(idx))
                        .and_then(TuneValue::as_int)
                        .map(|m| format_signed_margin(m as i16));
                }
            }
            ops.extend(v);
        }
        if let Some(m) = co_igpu {
            let mut op = co::set_igpu_co(transport, hw, m, allow_extreme, true)?;
            if op.current.is_none() {
                op.current = self
                    .live_param_value(TuneParam::CoIgpu)
                    .and_then(TuneValue::as_int)
                    .map(|v| format_signed_margin(v as i16));
            }
            ops.push(op);
        }
        for &(tier, margins) in shaper_tiers {
            let mut op =
                shaper::set_curve_shaper_tier(transport, hw, tier, margins, allow_extreme, true)?;
            if op.current.is_none() {
                op.current = self.shaper_state.as_loaded().map(|cs| {
                    let cur = cs.tier(tier);
                    format!("low={},med={},high={}", cur.low, cur.med, cur.high)
                });
            }
            ops.push(op);
        }
        if let Some(req) = pbo_req {
            let mut v = pbo::set_pbo(transport, hw, req, allow_extreme, true)?;
            for op in &mut v {
                self.enrich_op_current(op, PBO_ENRICH_RULES);
            }
            ops.extend(v);
        }
        if let Some(req) = apu_req {
            let mut v = apu::set_apu(transport, hw, req, allow_extreme, true)?;
            for op in &mut v {
                self.enrich_op_current(op, APU_ENRICH_RULES);
            }
            ops.extend(v);
        }
        Ok(ops)
    }

    fn open_staged_dry_run_modal(&mut self, transport: &dyn SmuTransport, hw: &HardwareContext) {
        if self.staged.is_empty() {
            self.set_flash_kind("No staged changes to preview", FlashKind::Warn);
            return;
        }

        let co_all = self.staged_int(TuneParam::CoAll);
        let co_cores: Vec<(u32, i32)> = hw
            .topology
            .cores
            .iter()
            .filter_map(|core| {
                self.staged_int(TuneParam::CoCore(core.index))
                    .map(|val| (core.index, val))
            })
            .collect();
        let co_igpu = self.staged_int(TuneParam::CoIgpu);
        let shaper_tiers = self.collect_staged_shaper_tiers(hw);
        let pbo_req = self.collect_staged_pbo_request();
        let apu_req = self.collect_staged_apu_request();

        // Check Fmax soft delta in dry-run (since pbo::set_pbo skips live get_fmax in dry_run)
        if let Some(fmax) = pbo_req.as_ref().and_then(|r| r.fmax_mhz) {
            if !self.allow_extreme {
                if let Some(cur) = self.pbo_state.as_loaded().and_then(|p| p.fmax_mhz) {
                    let ceiling = cur.saturating_add(pbo::FMAX_SOFT_DELTA_MHZ);
                    if fmax > ceiling {
                        self.set_flash_kind(
                            format!(
                                "Extreme blocked: fmax {fmax} > {ceiling} MHz (press 'x' for Allow Extreme)"
                            ),
                            FlashKind::Error,
                        );
                        return;
                    }
                }
            }
        }

        let allow_extreme = self.allow_extreme;
        match self.plan_staged_ops(
            transport,
            hw,
            co_all,
            &co_cores,
            co_igpu,
            &shaper_tiers,
            pbo_req.as_ref(),
            apu_req.as_ref(),
            allow_extreme,
        ) {
            Ok(ops) if ops.is_empty() => {
                self.set_flash_kind("No SMU operations to apply", FlashKind::Warn);
            }
            Ok(ops) => {
                let subtitle = format!(
                    "{} SMU operation(s) validated · dry-run passed · zero writes sent yet",
                    ops.len()
                );
                self.confirm_modal = Some(ConfirmModal::ops_modal(
                    " Dry-Run Preview — Apply Staged Tuning ",
                    subtitle,
                    ConfirmAction::ApplyStaged {
                        co_all,
                        co_cores,
                        co_igpu,
                        shaper_tiers,
                        pbo: pbo_req,
                        apu: apu_req,
                    },
                    ops,
                    allow_extreme,
                ));
            }
            Err(Error::ExtremeValueBlocked {
                field,
                value,
                threshold,
            }) => {
                self.set_flash_kind(
                    format!(
                        "Blocked: {field}={value} violates soft limit {threshold} (press 'x' for Extreme)"
                    ),
                    FlashKind::Error,
                );
            }
            Err(err) => {
                self.set_flash_kind(format!("Dry-run failed: {err}"), FlashKind::Error);
            }
        }
    }

    fn open_snapshot_restore_modal(
        &mut self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
        boot_baseline: bool,
    ) {
        let (label, loaded) = if boot_baseline {
            (
                "boot-baseline.json",
                self.snapshot_store.load_boot_baseline(),
            )
        } else {
            ("last-state.json", self.snapshot_store.load_last_state())
        };

        let snap = match loaded {
            Ok(Some(s)) => s,
            Ok(None) => {
                self.set_flash_kind(format!("No {label} snapshot found"), FlashKind::Warn);
                return;
            }
            Err(err) => {
                self.set_flash_kind(format!("Failed to load {label}: {err}"), FlashKind::Error);
                return;
            }
        };

        match session::apply_snapshot(transport, hw, &snap, true, true) {
            Ok(ops) if ops.is_empty() => {
                self.set_flash_kind(format!("Hardware already matches {label}"), FlashKind::Info);
            }
            Ok(ops) => {
                let (title, action) = if boot_baseline {
                    (
                        " Dry-Run Preview — Reset to Boot Baseline ",
                        ConfirmAction::ResetBootBaseline(snap),
                    )
                } else {
                    (
                        " Dry-Run Preview — Rollback to Last State ",
                        ConfirmAction::Rollback(snap),
                    )
                };
                let subtitle =
                    format!("Restoring {label} · {} SMU operation(s) planned", ops.len());
                self.confirm_modal =
                    Some(ConfirmModal::ops_modal(title, subtitle, action, ops, true));
            }
            Err(err) => {
                self.set_flash_kind(format!("Snapshot dry-run failed: {err}"), FlashKind::Error);
            }
        }
    }

    fn plan_reset_co_zero_ops(
        &self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) -> zensmu_core::Result<Vec<PlannedOp>> {
        let mut ops = Vec::new();
        if hw.capabilities.set_co_all_cores.is_some() {
            let mut v = co::set_co_all(transport, hw, 0, false, true)?;
            for op in &mut v {
                if op.current.is_none() {
                    op.current = self.current_co_all_summary();
                }
            }
            ops.extend(v);
        } else if hw.capabilities.set_co_per_core.is_some() {
            let pairs: Vec<(u32, i32)> = hw.topology.cores.iter().map(|c| (c.index, 0)).collect();
            let mut v = co::set_co_cores(transport, hw, &pairs, false, true)?;
            for (op, &(idx, _)) in v.iter_mut().zip(pairs.iter()) {
                if op.current.is_none() {
                    op.current = self
                        .live_param_value(TuneParam::CoCore(idx))
                        .and_then(TuneValue::as_int)
                        .map(|m| format_signed_margin(m as i16));
                }
            }
            ops.extend(v);
        } else {
            return Err(Error::UnsupportedCapability {
                capability: "curve_optimizer",
                codename: hw.codename.display_name().to_string(),
            });
        }

        if hw.capabilities.set_igpu_co.is_some() {
            let mut op = co::set_igpu_co(transport, hw, 0, false, true)?;
            if op.current.is_none() {
                op.current = self
                    .live_param_value(TuneParam::CoIgpu)
                    .and_then(TuneValue::as_int)
                    .map(|v| format_signed_margin(v as i16));
            }
            ops.push(op);
        }
        Ok(ops)
    }

    fn open_reset_co_zero_modal(&mut self, transport: &dyn SmuTransport, hw: &HardwareContext) {
        match self.plan_reset_co_zero_ops(transport, hw) {
            Ok(ops) => {
                let subtitle = format!("{} SMU operation(s) to reset CO margins to 0", ops.len());
                self.confirm_modal = Some(ConfirmModal::ops_modal(
                    " Dry-Run Preview — Reset Curve Optimizer to 0 ",
                    subtitle,
                    ConfirmAction::ResetCoZero,
                    ops,
                    false,
                ));
            }
            Err(err) => {
                self.set_flash_kind(format!("CO reset unavailable: {err}"), FlashKind::Error);
            }
        }
    }

    fn persist_enable_steps(&self, profile_step: String) -> Vec<(String, String)> {
        vec![
            ("1. Profile".to_string(), profile_step),
            (
                "2. Module".to_string(),
                format!(
                    "Ensure {} contains ryzen_smu",
                    self.persist_paths.modules_load.display()
                ),
            ),
            (
                "3. Service".to_string(),
                format!(
                    "Enable {} (boot + suspend resume with boot-loop guard)",
                    self.persist_paths.service_name
                ),
            ),
        ]
    }

    fn open_persist_live_modal(&mut self) {
        if !self.staged.is_empty() {
            self.set_flash_kind(
                "Apply (a) or discard (u) staged changes before persisting live state",
                FlashKind::Warn,
            );
            return;
        }

        let steps = self.persist_enable_steps(format!(
            "Capture live SMU state → {}",
            self.persist_paths.active_profile.display()
        ));
        self.confirm_modal = Some(ConfirmModal::steps_modal(
            " Confirm — Persist Live State & Enable Boot Service ",
            "Save tested live SMU tuning for boot & suspend resume",
            ConfirmAction::PersistLiveAndEnable,
            steps,
            self.allow_extreme,
        ));
    }

    fn open_toggle_service_modal(&mut self, hw: &HardwareContext) {
        if self.persist_status.service_enabled {
            let steps = vec![
                (
                    "1. Service".to_string(),
                    format!("Disable {} via systemctl", self.persist_paths.service_name),
                ),
                (
                    "2. Profile".to_string(),
                    format!(
                        "Keep {} untouched on disk (live SMU unchanged)",
                        self.persist_paths.active_profile.display()
                    ),
                ),
            ];
            self.confirm_modal = Some(ConfirmModal::steps_modal(
                " Confirm — Disable Boot Persistence Service ",
                format!(
                    "Disable {} on boot and suspend resume",
                    self.persist_paths.service_name
                ),
                ConfirmAction::ToggleBootService { enable: false },
                steps,
                false,
            ));
            return;
        }

        let needs_save = !self.persist_status.active_profile_exists
            || self.persist_status.active_profile_valid == Some(false);

        if needs_save {
            let steps = self.persist_enable_steps(format!(
                "Capture & save live SMU state → {}",
                self.persist_paths.active_profile.display()
            ));
            self.confirm_modal = Some(ConfirmModal::steps_modal(
                " Confirm — Save Live State & Enable Boot Service ",
                format!(
                    "{} not ready yet — will save live state first, then enable {}",
                    self.persist_paths.active_profile.display(),
                    self.persist_paths.service_name
                ),
                ConfirmAction::ToggleBootService { enable: true },
                steps,
                self.allow_extreme,
            ));
        } else {
            let steps = self.persist_enable_steps(format!(
                "Validate {} for {}",
                self.persist_paths.active_profile.display(),
                hw.codename.display_name()
            ));
            self.confirm_modal = Some(ConfirmModal::steps_modal(
                " Confirm — Enable Boot Persistence Service ",
                format!(
                    "Enable {} using existing {}",
                    self.persist_paths.service_name,
                    self.persist_paths.active_profile.display()
                ),
                ConfirmAction::ToggleBootService { enable: true },
                steps,
                self.allow_extreme,
            ));
        }
    }

    fn open_clear_boot_guard_modal(&mut self) {
        if !self.persist_status.boot_guard_tripped {
            self.set_flash("Boot-loop guard is already clear");
            return;
        }
        let steps = vec![
            (
                "1. Guard".to_string(),
                format!(
                    "Remove tripped sentinel {}",
                    self.persist_paths.guard_path.display()
                ),
            ),
            (
                "2. Boot".to_string(),
                format!(
                    "Allow {} to auto-apply on next boot",
                    self.persist_paths.service_name
                ),
            ),
        ];
        self.confirm_modal = Some(ConfirmModal::steps_modal(
            " Confirm — Clear Boot-Loop Crash Guard ",
            "Clear tripped sentinel after confirming system stability",
            ConfirmAction::ClearBootGuard,
            steps,
            false,
        ));
    }

    fn persist_live_profile_now(
        &self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) -> zensmu_core::Result<()> {
        let snap = session::capture_snapshot(transport, hw)?;
        let profile = Profile::from_snapshot(&snap);
        // Live hardware state may include BIOS Motherboard-PBO values above
        // interactive soft limits; hard hardware bounds remain strictly enforced.
        persist::persist_profile(
            &self.persist_paths,
            &profile,
            hw.codename,
            hw.topology.physical_cores,
            true,
        )
    }

    fn execute_persist_modal_action(
        &mut self,
        action: &ConfirmAction,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) {
        match action {
            ConfirmAction::PersistLiveAndEnable => {
                let res = self.persist_live_profile_now(transport, hw).and_then(|()| {
                    persist::set_service_enabled(
                        &self.persist_paths,
                        true,
                        hw.codename,
                        hw.topology.physical_cores,
                        true,
                    )
                });
                self.refresh_persist_status(hw);
                match res {
                    Ok(_) => self.set_flash_kind(
                        format!(
                            "Persisted live state & enabled {}",
                            self.persist_paths.service_name
                        ),
                        FlashKind::Success,
                    ),
                    Err(err) => {
                        self.set_flash_kind(format!("Persist failed: {err}"), FlashKind::Error);
                    }
                }
            }
            ConfirmAction::ToggleBootService { enable } => {
                let enable = *enable;
                let needs_save = enable
                    && (!self.persist_paths.active_profile.exists()
                        || self.persist_status.active_profile_valid == Some(false));
                let res = (if needs_save {
                    self.persist_live_profile_now(transport, hw)
                } else {
                    Ok(())
                })
                .and_then(|()| {
                    persist::set_service_enabled(
                        &self.persist_paths,
                        enable,
                        hw.codename,
                        hw.topology.physical_cores,
                        true,
                    )
                });
                self.refresh_persist_status(hw);
                match res {
                    Ok(_) => self.set_flash_kind(
                        if enable {
                            format!("Enabled {}", self.persist_paths.service_name)
                        } else {
                            format!("Disabled {}", self.persist_paths.service_name)
                        },
                        FlashKind::Success,
                    ),
                    Err(err) => {
                        self.set_flash_kind(
                            format!("Service toggle failed: {err}"),
                            FlashKind::Error,
                        );
                    }
                }
            }
            ConfirmAction::ClearBootGuard => {
                let res = BootLoopGuard::new(self.persist_paths.guard_path.clone()).clear();
                self.refresh_persist_status(hw);
                match res {
                    Ok(()) => self.set_flash_kind(
                        "Cleared boot-loop crash guard sentinel",
                        FlashKind::Success,
                    ),
                    Err(err) => {
                        self.set_flash_kind(format!("Clear guard failed: {err}"), FlashKind::Error);
                    }
                }
            }
            ConfirmAction::ApplyStaged { .. }
            | ConfirmAction::Rollback(_)
            | ConfirmAction::ResetBootBaseline(_)
            | ConfirmAction::ResetCoZero => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn apply_staged_tuning(
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
        co_all: Option<i32>,
        co_cores: &[(u32, i32)],
        co_igpu: Option<i32>,
        shaper_tiers: &[(FreqTier, ShaperTierMargins)],
        pbo: Option<&PboSetRequest>,
        apu: Option<&ApuSetRequest>,
        allow_extreme: bool,
    ) -> zensmu_core::Result<usize> {
        let mut applied = 0usize;
        if let Some(m) = co_all {
            applied += co::set_co_all(transport, hw, m, allow_extreme, false)?.len();
        }
        if !co_cores.is_empty() {
            applied += co::set_co_cores(transport, hw, co_cores, allow_extreme, false)?.len();
        }
        if let Some(m) = co_igpu {
            co::set_igpu_co(transport, hw, m, allow_extreme, false)?;
            applied += 1;
        }
        for &(tier, margins) in shaper_tiers {
            shaper::set_curve_shaper_tier(transport, hw, tier, margins, allow_extreme, false)?;
            applied += 1;
        }
        if let Some(req) = pbo {
            applied += pbo::set_pbo(transport, hw, req, allow_extreme, false)?.len();
        }
        if let Some(req) = apu {
            applied += apu::set_apu(transport, hw, req, allow_extreme, false)?.len();
        }
        Ok(applied)
    }

    fn apply_reset_co_zero(
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) -> zensmu_core::Result<usize> {
        let mut applied = 0usize;
        if hw.capabilities.set_co_all_cores.is_some() {
            applied += co::set_co_all(transport, hw, 0, false, false)?.len();
        } else if hw.capabilities.set_co_per_core.is_some() {
            let pairs: Vec<(u32, i32)> = hw.topology.cores.iter().map(|c| (c.index, 0)).collect();
            applied += co::set_co_cores(transport, hw, &pairs, false, false)?.len();
        }
        if hw.capabilities.set_igpu_co.is_some() {
            match co::set_igpu_co(transport, hw, 0, false, false) {
                Ok(_) => applied += 1,
                Err(Error::SmuRejected { .. }) => {
                    // iGPU may be disabled in BIOS; match CLI `co reset` tolerance
                }
                Err(err) => return Err(err),
            }
        }
        Ok(applied)
    }

    fn execute_confirmed_modal(&mut self, transport: &dyn SmuTransport, hw: &HardwareContext) {
        let Some(modal) = self.confirm_modal.take() else {
            return;
        };

        if !self.can_write() {
            let msg = if modal.action.is_persist_action() {
                "Refused: root required to persist boot config & control service (relaunch with sudo zensmu top)"
            } else {
                "Refused: root required to write SMU (relaunch with sudo zensmu top)"
            };
            self.set_flash_kind(msg, FlashKind::Error);
            return;
        }

        if modal.action.is_persist_action() {
            self.execute_persist_modal_action(&modal.action, transport, hw);
            return;
        }

        // Capture pre-change snapshot and persist boot-baseline + last-state best-effort
        if let Ok(pre) = session::capture_snapshot(transport, hw) {
            let _ = self.snapshot_store.ensure_boot_baseline(&pre);
            let _ = self.snapshot_store.save_last_state(&pre);
        }

        let exec_res: zensmu_core::Result<usize> = match &modal.action {
            ConfirmAction::ApplyStaged {
                co_all,
                co_cores,
                co_igpu,
                shaper_tiers,
                pbo,
                apu,
            } => Self::apply_staged_tuning(
                transport,
                hw,
                *co_all,
                co_cores,
                *co_igpu,
                shaper_tiers,
                pbo.as_ref(),
                apu.as_ref(),
                modal.allow_extreme,
            ),
            ConfirmAction::Rollback(snap) | ConfirmAction::ResetBootBaseline(snap) => {
                session::apply_snapshot(transport, hw, snap, true, false).map(|ops| ops.len())
            }
            ConfirmAction::ResetCoZero => Self::apply_reset_co_zero(transport, hw),
            ConfirmAction::PersistLiveAndEnable
            | ConfirmAction::ToggleBootService { .. }
            | ConfirmAction::ClearBootGuard => Ok(0),
        };

        // Always refresh live mailbox + telemetry right after write attempts
        self.refresh_mailbox(transport, hw);
        self.poll_telemetry(transport, hw);

        match exec_res {
            Ok(n) => {
                match modal.action {
                    ConfirmAction::ResetCoZero => {
                        self.staged.retain(|p, _| {
                            !matches!(
                                p,
                                TuneParam::CoAll | TuneParam::CoCore(_) | TuneParam::CoIgpu
                            )
                        });
                    }
                    _ => {
                        self.staged.clear();
                    }
                }
                self.last_alert = None;
                self.set_flash_kind(
                    format!("Applied {n} SMU operation(s) & verified"),
                    FlashKind::Success,
                );
            }
            Err(Error::HardwareClamped { summary, clamped }) => {
                for item in &clamped {
                    if let Some(param) = tune_param_from_clamped_field(&item.field) {
                        self.board_caps.insert(param, item.actual_value);
                    }
                }
                self.refresh_all(transport, hw);
                self.staged.clear();
                let msg = format!("Board/BIOS cap hit: {summary} (check BIOS PBO Limits)");
                self.last_alert = Some((msg.clone(), FlashKind::Warn));
                self.set_flash_kind(msg, FlashKind::Warn);
            }
            Err(err) => {
                let msg = if err.suggests_report() {
                    format!("SMU apply failed: {err} · run 'zensmu report --open'")
                } else {
                    format!("SMU apply failed: {err}")
                };
                self.last_alert = Some((msg.clone(), FlashKind::Error));
                self.set_flash_kind(msg, FlashKind::Error);
            }
        }
    }

    fn save_live_profile(&mut self, transport: &dyn SmuTransport, hw: &HardwareContext) {
        let path = if self.is_root {
            PathBuf::from("/etc/zensmu/top-saved.toml")
        } else {
            PathBuf::from("/tmp/zensmu-top-profile.toml")
        };

        match session::capture_snapshot(transport, hw)
            .and_then(|snap| Profile::from_snapshot(&snap).save(&path))
        {
            Ok(()) => {
                self.set_flash_kind(
                    format!("Saved profile: {}", path.display()),
                    FlashKind::Success,
                );
            }
            Err(err) => {
                self.set_flash_kind(format!("Save failed: {err}"), FlashKind::Error);
            }
        }
    }

    fn toggle_pause(&mut self) {
        self.paused = !self.paused;
        self.set_flash(if self.paused {
            "Telemetry paused"
        } else {
            "Telemetry resumed"
        });
    }

    fn refresh_all(&mut self, transport: &dyn SmuTransport, hw: &HardwareContext) {
        self.poll_telemetry(transport, hw);
        self.refresh_mailbox(transport, hw);
        self.refresh_persist_status(hw);
        if self.mailbox_note.is_some() {
            self.set_flash("Telemetry refreshed (SMU mailbox requires sudo)");
        } else {
            self.set_flash("SMU mailbox, telemetry & persistence refreshed");
        }
    }

    fn handle_modal_key(
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
    fn handle_global_key(
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

    fn handle_tune_key(
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

    fn handle_non_tune_key(
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
    fn handle_key(
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

// ---------------------------------------------------------------------------
// Pure helper functions (color, ratio, formatting, interval adjustment)
// ---------------------------------------------------------------------------

pub(crate) fn clamp_interval_ms(interval_ms: u64) -> u64 {
    interval_ms.clamp(MIN_INTERVAL_MS, MAX_INTERVAL_MS)
}

pub(crate) fn adjust_interval_ms(current_ms: u64, increase: bool) -> u64 {
    let cur = clamp_interval_ms(current_ms);
    let next = if increase {
        if cur < 1000 {
            cur.saturating_add(100)
        } else if cur < 2000 {
            cur.saturating_add(250)
        } else {
            cur.saturating_add(500)
        }
    } else if cur <= 1000 {
        cur.saturating_sub(100)
    } else if cur <= 2000 {
        cur.saturating_sub(250)
    } else {
        cur.saturating_sub(500)
    };
    clamp_interval_ms(next)
}

/// Compute a safe normalized gauge ratio in `0.0..=1.0`.
pub(crate) fn gauge_ratio(value: Option<f32>, limit: Option<f32>) -> f64 {
    match (value, limit) {
        (Some(v), Some(lim)) if v.is_finite() && lim.is_finite() && lim > 0.0 => {
            f64::from((v / lim).clamp(0.0, 1.0))
        }
        _ => 0.0,
    }
}

/// Compute gauge percentage (`0..=100+` rounded, or `None` if unavailable).
pub(crate) fn gauge_percent(value: Option<f32>, limit: Option<f32>) -> Option<u16> {
    match (value, limit) {
        (Some(v), Some(lim)) if v.is_finite() && lim.is_finite() && lim > 0.0 => {
            Some(((v / lim) * 100.0).max(0.0).round().min(999.0) as u16)
        }
        _ => None,
    }
}

/// Color-code a normalized utilization ratio: green (<65%), yellow (65..85%), red (>=85%).
pub(crate) fn ratio_color(ratio: f64) -> Color {
    if !ratio.is_finite() || ratio < 0.65 {
        palette::GREEN
    } else if ratio < 0.85 {
        palette::YELLOW
    } else {
        palette::RED
    }
}

/// Color-code temperature considering both absolute °C and ratio to `Tctl` limit.
pub(crate) fn thermal_color(temp_c: Option<f32>, limit_c: Option<f32>) -> Color {
    let ratio = gauge_ratio(temp_c, limit_c);
    if let Some(t) = temp_c {
        if t >= 85.0 || ratio >= 0.88 {
            return palette::RED;
        }
        if t >= 75.0 || ratio >= 0.72 {
            return palette::ORANGE;
        }
        if t >= 65.0 || ratio >= 0.60 {
            return palette::YELLOW;
        }
    }
    ratio_color(ratio)
}

/// Color-code a signed Curve Optimizer or Curve Shaper margin.
pub(crate) fn margin_color(margin: i16) -> Color {
    if margin <= -35 {
        palette::CYAN
    } else if margin < 0 {
        palette::GREEN
    } else if margin == 0 {
        palette::DIM
    } else if margin <= 10 {
        palette::YELLOW
    } else {
        palette::RED
    }
}

pub(crate) fn format_signed_margin(margin: i16) -> String {
    if margin > 0 {
        format!("+{margin}")
    } else {
        format!("{margin}")
    }
}

pub(crate) fn format_opt_f32(val: Option<f32>, decimals: usize, unit: &str) -> String {
    match val {
        Some(v) if v.is_finite() => {
            if unit.is_empty() {
                format!("{v:.decimals$}")
            } else {
                format!("{v:.decimals$} {unit}")
            }
        }
        _ => "—".to_string(),
    }
}

pub(crate) fn format_opt_u32(val: Option<u32>, unit: &str) -> String {
    match val {
        Some(v) => {
            if unit.is_empty() {
                format!("{v}")
            } else {
                format!("{v} {unit}")
            }
        }
        None => "—".to_string(),
    }
}

pub(crate) fn format_gauge_readout(value: Option<f32>, limit: Option<f32>, unit: &str) -> String {
    match (value, limit, gauge_percent(value, limit)) {
        (Some(v), Some(lim), Some(pct)) => format!("{v:5.1} / {lim:5.1} {unit} ({pct:3}%)"),
        (Some(v), _, _) if v.is_finite() => format!("{v:5.1} /   —   {unit} (  —)"),
        _ => format!("  —   /   —   {unit} (  —)"),
    }
}

pub(crate) fn format_clock_ratio(uclk: Option<f32>, mclk: Option<f32>) -> Option<&'static str> {
    let (u, m) = (uclk?, mclk?);
    if !u.is_finite() || !m.is_finite() || u <= 0.0 || m <= 0.0 {
        return None;
    }
    let r = m / u;
    if (r - 1.0).abs() < 0.05 {
        Some("1:1 sync")
    } else if (r - 2.0).abs() < 0.08 {
        Some("1:2 div")
    } else {
        None
    }
}

pub(crate) fn format_linux_cpus(cpus: &[u32]) -> String {
    if cpus.is_empty() {
        return "—".to_string();
    }
    cpus.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

/// Render a centered bidirectional ASCII/Unicode bar for Curve Optimizer margin.
pub(crate) fn format_co_bar(margin: i16, max_abs: i32, half_width: usize) -> String {
    let half = half_width.max(1);
    let bound = max_abs.max(1) as f64;
    let filled = ((f64::from(margin.abs()) / bound) * half as f64)
        .round()
        .clamp(0.0, half as f64) as usize;

    let mut left = String::with_capacity(half * 3);
    let mut right = String::with_capacity(half * 3);

    if margin < 0 {
        let empty = half.saturating_sub(filled);
        for _ in 0..empty {
            left.push(' ');
        }
        for _ in 0..filled {
            left.push('█');
        }
        for _ in 0..half {
            right.push(' ');
        }
    } else if margin > 0 {
        for _ in 0..half {
            left.push(' ');
        }
        for _ in 0..filled {
            right.push('█');
        }
        let empty = half.saturating_sub(filled);
        for _ in 0..empty {
            right.push(' ');
        }
    } else {
        for _ in 0..half {
            left.push(' ');
            right.push(' ');
        }
    }

    format!("[{left}│{right}]")
}

fn cpu_display_title(hw: &HardwareContext) -> String {
    if let Some(ref cpuid) = hw.cpuid {
        let brand = cpuid.brand.trim();
        if !brand.is_empty() {
            return format!("{brand} · {}", hw.codename.display_name());
        }
    }
    hw.codename.display_name().to_string()
}

fn styled_block<'a>(title: impl Into<Line<'a>>, accent: Color) -> Block<'a> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette::BORDER))
        .title_style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
}

fn modal_frame_block<'a>(title: &'a str, accent: Color) -> Block<'a> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title_style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
        .style(Style::default().bg(palette::POPUP_BG))
        .padding(Padding::horizontal(1))
}

fn badge_span<'a>(label: impl Into<Cow<'a, str>>, bg: Color) -> Span<'a> {
    Span::styled(
        label,
        Style::default()
            .fg(palette::BADGE_FG)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    )
}

fn render_mailbox_placeholder(
    frame: &mut Frame,
    area: Rect,
    block: Block<'_>,
    text: String,
    color: Color,
    wrap: bool,
) {
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let mut para = Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color))));
    if wrap {
        para = para.wrap(Wrap { trim: true });
    }
    frame.render_widget(para, inner);
}

// ---------------------------------------------------------------------------
// Top-level UI rendering
// ---------------------------------------------------------------------------

fn draw_ui(frame: &mut Frame, app: &App, hw: &HardwareContext) {
    let root = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(root);

    draw_header(frame, chunks[0], app, hw);

    match app.active_tab {
        ViewTab::Overview => draw_overview_tab(frame, chunks[1], app, hw),
        ViewTab::ClocksVolts => draw_clocks_volts_tab(frame, chunks[1], app, hw),
        ViewTab::CoShaper => draw_co_shaper_tab(frame, chunks[1], app, hw),
        ViewTab::Tune => draw_tune_tab(frame, chunks[1], app, hw),
    }

    draw_footer(frame, chunks[2], app);

    if let Some(ref modal) = app.confirm_modal {
        draw_confirm_modal(frame, root, app, modal);
    } else if app.show_help {
        draw_help_popup(frame, root);
    }
}

// ---------------------------------------------------------------------------
// Header & Footer
// ---------------------------------------------------------------------------

fn draw_header(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let header_block = Block::default()
        .borders(Borders::BOTTOM)
        .border_style(Style::default().fg(palette::BORDER))
        .style(Style::default().bg(palette::BAR_BG));
    let inner = header_block.inner(area);
    frame.render_widget(header_block, area);

    if inner.height < 2 {
        return;
    }

    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    draw_header_top_row(frame, rows[0], app, hw);
    draw_header_tabs_row(frame, rows[1], app);
}

fn draw_header_top_row(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let cpu_title = cpu_display_title(hw);
    let left_spans = vec![
        badge_span(" zensmu top ", palette::CYAN),
        Span::raw(" "),
        Span::styled(
            cpu_title,
            Style::default()
                .fg(palette::FG)
                .add_modifier(Modifier::BOLD),
        ),
    ];

    let mut right_spans = vec![
        Span::styled("SMU ", Style::default().fg(palette::MUTED)),
        Span::styled(
            hw.driver.smu_version.as_str(),
            Style::default().fg(palette::TEAL),
        ),
        Span::styled(" · drv ", Style::default().fg(palette::MUTED)),
        Span::styled(
            hw.driver.drv_version.as_str(),
            Style::default().fg(palette::FG),
        ),
        Span::styled(" · ", Style::default().fg(palette::DIM)),
        Span::styled(
            format!("{}ms", app.interval_ms),
            Style::default().fg(palette::CYAN),
        ),
        Span::raw(" "),
    ];

    if let Some(ref rec) = app.recorder {
        let elapsed_secs = app
            .record_started
            .map(|t| t.elapsed().as_secs())
            .unwrap_or(0);
        let mins = elapsed_secs / 60;
        let secs = elapsed_secs % 60;
        let samples = rec.stats().samples_written;
        right_spans.push(badge_span(
            format!(" ● REC {mins:02}:{secs:02} ({samples}) "),
            palette::RED,
        ));
        right_spans.push(Span::raw(" "));
    }

    if app.allow_extreme {
        right_spans.push(badge_span(" EXTREME ", palette::ORANGE));
        right_spans.push(Span::raw(" "));
    }

    if app.paused {
        right_spans.push(badge_span(" PAUSED ", palette::YELLOW));
        right_spans.push(Span::raw(" "));
    }

    let (priv_label, priv_bg) = if app.is_root {
        (" ROOT ", palette::GREEN)
    } else if app.has_any_mailbox_data() && app.mailbox_note.is_none() {
        (" SMU:OK ", palette::TEAL)
    } else {
        (" UNPRIV ", palette::YELLOW)
    };
    right_spans.push(badge_span(priv_label, priv_bg));

    let base_right_w: u16 = if area.width < 92 { 44 } else { 62 };
    let needed_right_w: u16 = right_spans
        .iter()
        .map(|s| s.width() as u16)
        .fold(0u16, u16::saturating_add);
    let max_right_w = area.width.saturating_sub(18);
    let right_w = needed_right_w.max(base_right_w).min(max_right_w);
    let row1_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(18), Constraint::Length(right_w)])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(left_spans)).style(Style::default().bg(palette::BAR_BG)),
        row1_split[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(right_spans))
            .alignment(Alignment::Right)
            .style(Style::default().bg(palette::BAR_BG)),
        row1_split[1],
    );
}

fn draw_header_tabs_row(frame: &mut Frame, area: Rect, app: &App) {
    let tab_span = |label: &str, active: bool| -> Span<'_> {
        if active {
            badge_span(format!(" {label} "), palette::TEAL)
        } else {
            Span::styled(format!(" {label} "), Style::default().fg(palette::MUTED))
        }
    };

    let compact = area.width < 100;
    let staged_n = app.staged_count();
    let mut tabs_spans = vec![
        tab_span(
            if compact { "[1:Over]" } else { "[1:Overview]" },
            app.active_tab == ViewTab::Overview,
        ),
        Span::raw(" "),
        tab_span(
            if compact {
                "[2:Clocks]"
            } else {
                "[2:Clocks & Volts]"
            },
            app.active_tab == ViewTab::ClocksVolts,
        ),
        Span::raw(" "),
        tab_span(
            if compact {
                "[3:CO/Shp]"
            } else {
                "[3:CO & Shaper]"
            },
            app.active_tab == ViewTab::CoShaper,
        ),
        Span::raw(" "),
        tab_span("[4:Tune]", app.active_tab == ViewTab::Tune),
    ];

    if staged_n > 0 {
        tabs_spans.push(badge_span(format!(" *{staged_n} "), palette::ORANGE));
    }

    tabs_spans.push(Span::raw(" "));
    tabs_spans.push(tab_span("[?:Help]", app.show_help));

    let row2_right = if let Some(ref err) = app.telemetry_error {
        Line::from(vec![Span::styled(
            format!("PM err: {err}"),
            Style::default().fg(palette::RED),
        )])
    } else if app.mailbox_note.is_some() {
        Line::from(vec![Span::styled(
            if compact {
                "sudo for SMU"
            } else {
                "PM telemetry live · sudo for mailbox"
            },
            Style::default().fg(palette::YELLOW),
        )])
    } else {
        Line::from(vec![
            Span::styled("samples ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format!("#{}", app.sample_count),
                Style::default().fg(palette::CYAN),
            ),
        ])
    };

    let row2_right_w = if compact { 16 } else { 36 };
    let row2_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(46), Constraint::Length(row2_right_w)])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(tabs_spans)).style(Style::default().bg(palette::BAR_BG)),
        row2_split[0],
    );
    frame.render_widget(
        Paragraph::new(row2_right)
            .alignment(Alignment::Right)
            .style(Style::default().bg(palette::BAR_BG)),
        row2_split[1],
    );
}

fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
    let keycap = |k: &'static str, desc: &'static str| -> Vec<Span<'static>> {
        vec![
            Span::styled(
                k,
                Style::default()
                    .fg(palette::CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(":{desc} "), Style::default().fg(palette::MUTED)),
        ]
    };

    let mut left = vec![Span::raw(" ")];
    if app.confirm_modal.is_some() {
        left.extend(keycap("y/Enter", "Confirm & Apply"));
        left.extend(keycap("n/Esc", "Cancel"));
        left.extend(keycap("↑↓", "Scroll"));
    } else if app.active_tab == ViewTab::Tune {
        left.extend(keycap("↑↓", "Sel"));
        left.extend(keycap("←→", "Step"));
        left.extend(keycap("a/Ent", "Apply"));
        left.extend(keycap("u", "Undo"));
        left.extend(keycap("x", "Ext"));
        left.extend(keycap("R/B/Z", "Reset"));
        left.extend(keycap("P", "Persist+Enable"));
        left.extend(keycap("E", "Toggle Service"));
        left.extend(keycap("w", "Rec"));
    } else {
        left.extend(keycap("q", "Quit"));
        left.extend(keycap("Tab/1-4", "Views"));
        left.extend(keycap("t", "Tune"));
        left.extend(keycap("p", "Pause"));
        left.extend(keycap("w", "Rec"));
        left.extend(keycap("r", "Refresh"));
        left.extend(keycap("+/-", "Int"));
        left.extend(keycap("?", "Help"));
    }

    let right_text = if let Some((flash, kind)) = app.active_flash() {
        let color = match kind {
            FlashKind::Info => palette::TEAL,
            FlashKind::Success => palette::GREEN,
            FlashKind::Warn => palette::YELLOW,
            FlashKind::Error => palette::RED,
        };
        Line::from(vec![
            Span::styled(
                flash,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ])
    } else if app.active_tab == ViewTab::Tune {
        let staged_n = app.staged_count();
        if staged_n > 0 {
            Line::from(vec![Span::styled(
                format!("{staged_n} staged · Enter to preview "),
                Style::default()
                    .fg(palette::ORANGE)
                    .add_modifier(Modifier::BOLD),
            )])
        } else {
            Line::from(vec![Span::styled(
                format!("hist {}s ", app.history.ppt_w.samples.len()),
                Style::default().fg(palette::DIM),
            )])
        }
    } else {
        Line::from(vec![Span::styled(
            format!("hist {}s ", app.history.ppt_w.samples.len()),
            Style::default().fg(palette::DIM),
        )])
    };

    let (left_min, right_w) = if let Some((flash, kind)) = app.active_flash() {
        let needed = (flash.chars().count() as u16).saturating_add(2);
        match kind {
            FlashKind::Warn | FlashKind::Error => {
                let max_w = area.width.saturating_sub(12).max(16);
                (12u16.min(area.width), needed.clamp(16, max_w))
            }
            FlashKind::Info | FlashKind::Success => {
                (30u16, needed.clamp(16, (area.width / 2).clamp(24, 58)))
            }
        }
    } else if app.active_tab == ViewTab::Tune && app.staged_count() > 0 {
        (30u16, 28)
    } else {
        (30u16, 16)
    };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(left_min), Constraint::Length(right_w)])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(left)).style(Style::default().bg(palette::BAR_BG)),
        cols[0],
    );
    frame.render_widget(
        Paragraph::new(right_text)
            .alignment(Alignment::Right)
            .style(Style::default().bg(palette::BAR_BG)),
        cols[1],
    );
}

// ---------------------------------------------------------------------------
// Tab 1: Overview (btop style)
// ---------------------------------------------------------------------------

fn has_apu_telemetry(snap: Option<&TelemetrySnapshot>) -> bool {
    snap.is_some_and(crate::output::has_apu_telemetry)
}

fn draw_overview_tab(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let snap = app.telemetry.as_ref();
    let gauge_height = if has_apu_telemetry(snap) {
        if area.height >= 28 {
            9
        } else {
            7
        }
    } else {
        6
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(gauge_height),
            Constraint::Percentage(40),
            Constraint::Min(8),
        ])
        .split(area);

    draw_power_thermal_gauges(frame, rows[0], app);

    let spark_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);

    let ppt_cur = snap.and_then(|s| s.ppt_value_w);
    let ppt_lim = snap.and_then(|s| s.ppt_limit_w);
    let tctl_cur = snap.and_then(|s| s.tctl_value_c);
    let tctl_lim = snap.and_then(|s| s.tctl_limit_c);

    draw_metric_sparkline(
        frame,
        spark_cols[0],
        "PPT Power",
        "W",
        1,
        ppt_cur,
        ppt_lim,
        &app.history.ppt_w,
        10.0,
        ratio_color(gauge_ratio(ppt_cur, ppt_lim)),
    );

    draw_metric_sparkline(
        frame,
        spark_cols[1],
        "Tctl Temp",
        "°C",
        1,
        tctl_cur,
        tctl_lim,
        &app.history.tctl_c,
        10.0,
        thermal_color(tctl_cur, tctl_lim),
    );

    let bottom_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(rows[2]);

    draw_clocks_voltages_summary(frame, bottom_cols[0], app);
    draw_co_pbo_summary(frame, bottom_cols[1], app, hw);
}

struct GaugeSpec<'a> {
    label: &'a str,
    value: Option<f32>,
    limit: Option<f32>,
    unit: &'a str,
    is_thermal: bool,
}

fn render_gauge_line(frame: &mut Frame, area: Rect, label_w: u16, spec: &GaugeSpec<'_>) {
    let ratio = gauge_ratio(spec.value, spec.limit);
    let color = if spec.is_thermal {
        thermal_color(spec.value, spec.limit)
    } else {
        ratio_color(ratio)
    };
    let compact = area.width < 48;
    let readout = if compact {
        match (spec.value, spec.limit) {
            (Some(v), Some(l)) if v.is_finite() && l.is_finite() => {
                format!("{v:4.1}/{l:4.0}{}", spec.unit)
            }
            (Some(v), _) if v.is_finite() => format!("{v:4.1}/—{}", spec.unit),
            (_, Some(l)) if l.is_finite() => format!("—/{l:4.0}{}", spec.unit),
            _ => format!("—/—{}", spec.unit),
        }
    } else {
        format_gauge_readout(spec.value, spec.limit, spec.unit)
    };
    let readout_w = if compact { 14 } else { 25 };

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(label_w),
            Constraint::Min(6),
            Constraint::Length(readout_w),
        ])
        .split(area);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {}", spec.label),
            Style::default()
                .fg(palette::MUTED)
                .add_modifier(Modifier::BOLD),
        ))),
        cols[0],
    );

    let gauge = LineGauge::default()
        .filled_style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .unfilled_style(Style::default().fg(palette::GAUGE_TRACK))
        .line_set(symbols::line::THICK)
        .ratio(ratio)
        .label("");
    frame.render_widget(gauge, cols[1]);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            readout,
            Style::default().fg(color),
        )))
        .alignment(Alignment::Right),
        cols[2],
    );
}

fn render_gauge_column(frame: &mut Frame, area: Rect, label_w: u16, specs: &[GaugeSpec<'_>]) {
    let row_count = specs.len().min(area.height as usize);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(1); row_count])
        .split(area);
    for (spec, &row_area) in specs.iter().zip(rows.iter()) {
        render_gauge_line(frame, row_area, label_w, spec);
    }
}

fn push_optional_gauge<'a>(
    out: &mut Vec<GaugeSpec<'a>>,
    label: &'a str,
    value: Option<f32>,
    limit: Option<f32>,
    unit: &'a str,
    is_thermal: bool,
) {
    if value.is_some() || limit.is_some() {
        out.push(GaugeSpec {
            label,
            value,
            limit,
            unit,
            is_thermal,
        });
    }
}

fn draw_power_thermal_gauges(frame: &mut Frame, area: Rect, app: &App) {
    let snap = app.telemetry.as_ref();
    let is_apu_telem = has_apu_telemetry(snap);
    let title = if is_apu_telem {
        " Power, Current & Thermal Limits (APU DPTC) "
    } else {
        " Power & Thermal Limits "
    };
    let block = styled_block(title, palette::CYAN);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    if !is_apu_telem {
        let specs = [
            GaugeSpec {
                label: "PPT ",
                value: snap.and_then(|s| s.ppt_value_w),
                limit: snap.and_then(|s| s.ppt_limit_w),
                unit: "W",
                is_thermal: false,
            },
            GaugeSpec {
                label: "TDC ",
                value: snap.and_then(|s| s.tdc_value_a),
                limit: snap.and_then(|s| s.tdc_limit_a),
                unit: "A",
                is_thermal: false,
            },
            GaugeSpec {
                label: "EDC ",
                value: snap.and_then(|s| s.edc_value_a),
                limit: snap.and_then(|s| s.edc_limit_a),
                unit: "A",
                is_thermal: false,
            },
            GaugeSpec {
                label: "Tctl",
                value: snap.and_then(|s| s.tctl_value_c),
                limit: snap.and_then(|s| s.tctl_limit_c),
                unit: "°C",
                is_thermal: true,
            },
        ];
        render_gauge_column(frame, inner, 6, &specs);
        return;
    }

    // APU 2-column compact layout: Left = Power & Skin Thermal, Right = Currents & Tctl
    let mut left_specs: Vec<GaugeSpec<'_>> = Vec::new();
    push_optional_gauge(
        &mut left_specs,
        "STAPM",
        snap.and_then(|s| s.stapm_value_w),
        snap.and_then(|s| s.stapm_limit_w),
        "W",
        false,
    );
    left_specs.push(GaugeSpec {
        label: "Fast ",
        value: snap.and_then(|s| s.ppt_value_w),
        limit: snap.and_then(|s| s.ppt_limit_w),
        unit: "W",
        is_thermal: false,
    });
    push_optional_gauge(
        &mut left_specs,
        "Slow ",
        snap.and_then(|s| s.slow_value_w),
        snap.and_then(|s| s.slow_limit_w),
        "W",
        false,
    );
    push_optional_gauge(
        &mut left_specs,
        "APUSl",
        snap.and_then(|s| s.apu_slow_value_w),
        snap.and_then(|s| s.apu_slow_limit_w),
        "W",
        false,
    );
    push_optional_gauge(
        &mut left_specs,
        "ASkin",
        snap.and_then(|s| s.apu_skin_temp_value_c),
        snap.and_then(|s| s.apu_skin_temp_limit_c),
        "°C",
        true,
    );
    push_optional_gauge(
        &mut left_specs,
        "dSkin",
        snap.and_then(|s| s.dgpu_skin_temp_value_c),
        snap.and_then(|s| s.dgpu_skin_temp_limit_c),
        "°C",
        true,
    );

    let mut right_specs: Vec<GaugeSpec<'_>> = vec![GaugeSpec {
        label: "TDC  ",
        value: snap.and_then(|s| s.tdc_value_a),
        limit: snap.and_then(|s| s.tdc_limit_a),
        unit: "A",
        is_thermal: false,
    }];
    push_optional_gauge(
        &mut right_specs,
        "SoCTD",
        snap.and_then(|s| s.tdc_soc_value_a),
        snap.and_then(|s| s.tdc_soc_limit_a),
        "A",
        false,
    );
    right_specs.push(GaugeSpec {
        label: "EDC  ",
        value: snap.and_then(|s| s.edc_value_a),
        limit: snap.and_then(|s| s.edc_limit_a),
        unit: "A",
        is_thermal: false,
    });
    push_optional_gauge(
        &mut right_specs,
        "SoCED",
        snap.and_then(|s| s.edc_soc_value_a),
        snap.and_then(|s| s.edc_soc_limit_a),
        "A",
        false,
    );
    right_specs.push(GaugeSpec {
        label: "Tctl ",
        value: snap.and_then(|s| s.tctl_value_c),
        limit: snap.and_then(|s| s.tctl_limit_c),
        unit: "°C",
        is_thermal: true,
    });

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    render_gauge_column(frame, cols[0], 7, &left_specs);
    render_gauge_column(frame, cols[1], 7, &right_specs);
}

#[allow(clippy::too_many_arguments)]
fn draw_metric_sparkline(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    unit: &str,
    decimals: usize,
    current: Option<f32>,
    limit: Option<f32>,
    history: &HistoryBuffer,
    scale: f64,
    color: Color,
) {
    let live_str = format_opt_f32(current, decimals, unit);
    let title_line = Line::from(vec![
        Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("· ", Style::default().fg(palette::DIM)),
        Span::styled(
            format!("{live_str} "),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]);

    let block = styled_block(title_line, palette::CYAN).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height < 2 || inner.width == 0 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);

    let stats_line = if let Some((min, avg, max)) = history.stats() {
        let mut spans = vec![
            Span::styled("min ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format!("{min:.decimals$}"),
                Style::default().fg(palette::FG),
            ),
            Span::styled("  avg ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format!("{avg:.decimals$}"),
                Style::default().fg(palette::TEAL),
            ),
            Span::styled("  max ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format!("{max:.decimals$}"),
                Style::default().fg(palette::YELLOW),
            ),
        ];
        if let Some(lim) = limit {
            spans.push(Span::styled("  lim ", Style::default().fg(palette::MUTED)));
            spans.push(Span::styled(
                format!("{lim:.decimals$}{unit}"),
                Style::default().fg(palette::DIM),
            ));
        }
        Line::from(spans)
    } else {
        Line::from(Span::styled(
            "waiting for telemetry samples...",
            Style::default().fg(palette::DIM),
        ))
    };
    frame.render_widget(Paragraph::new(stats_line), chunks[0]);

    let data = history.sparkline_window(chunks[1].width as usize, scale);
    let observed_max = data.iter().copied().max().unwrap_or(0);
    let lim_scaled = limit
        .filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| (f64::from(v) * scale).round() as u64)
        .unwrap_or(0);
    let spark_max = observed_max.max(lim_scaled).max(1);

    let sparkline = Sparkline::default()
        .data(&data)
        .max(spark_max)
        .bar_set(symbols::bar::NINE_LEVELS)
        .style(Style::default().fg(color));
    frame.render_widget(sparkline, chunks[1]);
}

fn draw_clocks_voltages_summary(frame: &mut Frame, area: Rect, app: &App) {
    let block = styled_block(" Clocks & Voltages ", palette::TEAL).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let snap = app.telemetry.as_ref();
    let fclk = snap.and_then(|s| s.fclk_mhz);
    let uclk = snap.and_then(|s| s.uclk_mhz);
    let mclk = snap.and_then(|s| s.mclk_mhz);

    let ratio_badge = format_clock_ratio(uclk, mclk).unwrap_or("");
    let rows = vec![
        Row::new(vec![
            Cell::from("FCLK").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(fclk, 0, "MHz")).style(
                Style::default()
                    .fg(palette::TEAL)
                    .add_modifier(Modifier::BOLD),
            ),
            Cell::from("VDDCR_SOC").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.vddcr_soc_v), 3, "V")).style(
                Style::default()
                    .fg(palette::PURPLE)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Row::new(vec![
            Cell::from("UCLK").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(uclk, 0, "MHz")).style(
                Style::default()
                    .fg(palette::CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
            Cell::from("VDDCR_MISC").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.vddcr_misc_v), 3, "V"))
                .style(Style::default().fg(palette::FG)),
        ]),
        Row::new(vec![
            Cell::from("MCLK").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(mclk, 0, "MHz")).style(
                Style::default()
                    .fg(palette::PURPLE)
                    .add_modifier(Modifier::BOLD),
            ),
            Cell::from("CLDO_VDDP").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.cldo_vddp_v), 3, "V"))
                .style(Style::default().fg(palette::FG)),
        ]),
        Row::new(vec![
            Cell::from("Ratio").style(Style::default().fg(palette::MUTED)),
            Cell::from(if ratio_badge.is_empty() {
                "—"
            } else {
                ratio_badge
            })
            .style(Style::default().fg(palette::GREEN)),
            Cell::from("VDDG IOD").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.cldo_vddg_iod_v), 3, "V"))
                .style(Style::default().fg(palette::FG)),
        ]),
        Row::new(vec![
            Cell::from("PM Ver").style(Style::default().fg(palette::MUTED)),
            Cell::from(
                snap.map(|s| format!("0x{:08X}", s.table_version))
                    .unwrap_or_else(|| "—".to_string()),
            )
            .style(Style::default().fg(palette::DIM)),
            Cell::from("VDDG CCD").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.cldo_vddg_ccd_v), 3, "V"))
                .style(Style::default().fg(palette::FG)),
        ]),
    ];

    let widths = [
        Constraint::Length(7),
        Constraint::Percentage(42),
        Constraint::Length(11),
        Constraint::Percentage(40),
    ];

    let table = Table::new(rows, widths).column_spacing(1);
    frame.render_widget(table, inner);
}

fn draw_co_pbo_summary(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let block = styled_block(" Active Curve Optimizer / PBO ", palette::CYAN)
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(ref note) = app.mailbox_note {
        if !app.has_any_mailbox_data() {
            draw_locked_mailbox_banner(frame, inner, note);
            return;
        }
    }

    let mut lines: Vec<Line<'_>> = Vec::new();
    lines.push(build_pbo_summary_line(app, hw));
    build_co_summary_lines(app, hw, &mut lines);
    lines.push(build_shaper_summary_line(app, hw));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_locked_mailbox_banner(frame: &mut Frame, area: Rect, note: &str) {
    let lines = vec![
        Line::from(""),
        Line::from(vec![
            badge_span(" [LOCKED] ", palette::YELLOW),
            Span::raw(" "),
            Span::styled(note, Style::default().fg(palette::YELLOW)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Tip: ", Style::default().fg(palette::MUTED)),
            Span::styled("sudo zensmu top", Style::default().fg(palette::CYAN)),
            Span::styled("  ·  or press ", Style::default().fg(palette::MUTED)),
            Span::styled(
                "r",
                Style::default()
                    .fg(palette::TEAL)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to retry SMU mailbox", Style::default().fg(palette::MUTED)),
        ]),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), area);
}

fn build_pbo_summary_line<'a>(app: &'a App, hw: &'a HardwareContext) -> Line<'a> {
    if let Some(pbo) = app.pbo_state.as_loaded() {
        let mut spans = vec![
            Span::styled("Scalar ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format_opt_f32(pbo.scalar, 2, "x"),
                Style::default()
                    .fg(palette::CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Fmax ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format_opt_u32(pbo.fmax_mhz, "MHz"),
                Style::default()
                    .fg(palette::TEAL)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if let Some(tdp) = pbo.fused.tdp_w {
            spans.push(Span::styled("  TDP ", Style::default().fg(palette::MUTED)));
            spans.push(Span::styled(
                format!("{tdp}W"),
                Style::default()
                    .fg(palette::CYAN)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(tjmax) = pbo.fused.tjmax_c {
            spans.push(Span::styled(
                "  TjMax ",
                Style::default().fg(palette::MUTED),
            ));
            spans.push(Span::styled(
                format!("{tjmax}°C"),
                Style::default()
                    .fg(palette::ORANGE)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.extend([
            Span::raw("  "),
            flag_pill("OC", pbo.flags.overclockable),
            Span::raw(" "),
            flag_pill("EXPO", pbo.flags.expo_active),
            Span::raw(" "),
            flag_pill("LN2", pbo.flags.ln2_mode),
        ]);
        Line::from(spans)
    } else {
        Line::from(vec![
            Span::styled("PBO: ", Style::default().fg(palette::MUTED)),
            Span::styled(
                mailbox_status_label(&app.pbo_state, &hw.driver.smu_version),
                Style::default().fg(palette::DIM),
            ),
        ])
    }
}

fn build_co_summary_lines<'a>(app: &'a App, hw: &'a HardwareContext, lines: &mut Vec<Line<'a>>) {
    match &app.co_state {
        MailboxState::Loaded(co_snap) => {
            let uniform_margin = co_snap
                .cores
                .first()
                .map(|c| c.margin)
                .filter(|&first| co_snap.cores.iter().all(|c| c.margin == first));

            if co_snap.cores.len() > 8 {
                // On high-core-count dual-CCD CPUs (e.g. 16-core 5900XT/9950X),
                // show a concise all-equal summary or CCD-grouped compact lines so nothing clips.
                if let Some(first) = uniform_margin {
                    let mut spans = vec![
                        Span::styled("CO: ", Style::default().fg(palette::MUTED)),
                        Span::styled(
                            format!(
                                "all {} ({} cores across {} CCDs)",
                                format_signed_margin(first),
                                co_snap.cores.len(),
                                hw.topology.ccd_count
                            ),
                            Style::default()
                                .fg(margin_color(first))
                                .add_modifier(Modifier::BOLD),
                        ),
                    ];
                    if let Some(igpu) = co_snap.igpu {
                        spans.push(Span::raw("  "));
                        spans.push(Span::styled(
                            format!("iGPU:{}", format_signed_margin(igpu)),
                            Style::default()
                                .fg(margin_color(igpu))
                                .add_modifier(Modifier::BOLD),
                        ));
                    }
                    lines.push(Line::from(spans));
                } else {
                    for (chunk_idx, chunk) in co_snap.cores.chunks(8).enumerate() {
                        let label = if chunk_idx == 0 { "CO: " } else { "    " };
                        let mut pill_spans =
                            vec![Span::styled(label, Style::default().fg(palette::MUTED))];
                        for core in chunk {
                            pill_spans.push(Span::styled(
                                format!(
                                    "C{}:{}",
                                    core.core_index,
                                    format_signed_margin(core.margin)
                                ),
                                Style::default()
                                    .fg(margin_color(core.margin))
                                    .add_modifier(Modifier::BOLD),
                            ));
                            pill_spans.push(Span::raw(" "));
                        }
                        lines.push(Line::from(pill_spans));
                    }
                }
            } else {
                let mut pill_spans =
                    vec![Span::styled("CO: ", Style::default().fg(palette::MUTED))];
                for core in &co_snap.cores {
                    pill_spans.push(Span::styled(
                        format!("C{}:{}", core.core_index, format_signed_margin(core.margin)),
                        Style::default()
                            .fg(margin_color(core.margin))
                            .add_modifier(Modifier::BOLD),
                    ));
                    pill_spans.push(Span::raw(" "));
                }
                if let Some(igpu) = co_snap.igpu {
                    pill_spans.push(Span::styled(
                        format!("iGPU:{}", format_signed_margin(igpu)),
                        Style::default()
                            .fg(margin_color(igpu))
                            .add_modifier(Modifier::BOLD),
                    ));
                }
                lines.push(Line::from(pill_spans));
            }
        }
        MailboxState::Unsupported if hw.capabilities.set_co_per_core.is_some() => {
            lines.push(Line::from(vec![
                Span::styled("CO: ", Style::default().fg(palette::MUTED)),
                Span::styled(
                    "write-only on this CPU (no read opcode)",
                    Style::default().fg(palette::DIM),
                ),
            ]));
        }
        other => {
            lines.push(Line::from(vec![
                Span::styled("CO: ", Style::default().fg(palette::MUTED)),
                Span::styled(
                    mailbox_status_label(other, &hw.driver.smu_version),
                    Style::default().fg(palette::DIM),
                ),
            ]));
        }
    }
}

fn build_shaper_summary_line<'a>(app: &'a App, hw: &'a HardwareContext) -> Line<'a> {
    match &app.shaper_state {
        MailboxState::Loaded(shaper) => {
            let all_vals = shaper_all_values(shaper);
            let min_v = all_vals.iter().copied().min().unwrap_or(0);
            let max_v = all_vals.iter().copied().max().unwrap_or(0);
            let active_count = all_vals.iter().filter(|&&v| v != 0).count();
            Line::from(vec![
                Span::styled("Shaper: ", Style::default().fg(palette::MUTED)),
                if active_count == 0 {
                    Span::styled("all 0 (stock)", Style::default().fg(palette::DIM))
                } else {
                    Span::styled(
                        format!(
                            "{active_count}/15 tuned (range {} .. {})",
                            format_signed_margin(i16::from(min_v)),
                            format_signed_margin(i16::from(max_v))
                        ),
                        Style::default().fg(palette::GREEN),
                    )
                },
            ])
        }
        MailboxState::Unsupported => Line::from(vec![
            Span::styled("Shaper: ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format!("N/A on {}", hw.codename.display_name()),
                Style::default().fg(palette::DIM),
            ),
        ]),
        other => Line::from(vec![
            Span::styled("Shaper: ", Style::default().fg(palette::MUTED)),
            Span::styled(
                mailbox_status_label(other, &hw.driver.smu_version),
                Style::default().fg(palette::DIM),
            ),
        ]),
    }
}

fn flag_pill(name: &str, state: Option<bool>) -> Span<'static> {
    match state {
        Some(true) => Span::styled(
            format!("[{name}:ON]"),
            Style::default()
                .fg(palette::GREEN)
                .add_modifier(Modifier::BOLD),
        ),
        Some(false) => Span::styled(format!("[{name}:OFF]"), Style::default().fg(palette::DIM)),
        None => Span::styled(format!("[{name}:—]"), Style::default().fg(palette::DIM)),
    }
}

fn mailbox_status_label<T>(state: &MailboxState<T>, smu_ver: &str) -> String {
    match state {
        MailboxState::Loaded(_) => "loaded".to_string(),
        MailboxState::PermissionDenied => "requires sudo".to_string(),
        MailboxState::Unsupported => "unsupported on this CPU".to_string(),
        MailboxState::FirmwareUnsupported(code) => {
            format!("not exposed by SMU {smu_ver} ({code})")
        }
        MailboxState::Error(e) => format!("error: {e}"),
    }
}

fn shaper_all_values(s: &CurveShaperState) -> [i8; 15] {
    [
        s.min.low,
        s.min.med,
        s.min.high,
        s.low.low,
        s.low.med,
        s.low.high,
        s.med.low,
        s.med.med,
        s.med.high,
        s.high.low,
        s.high.med,
        s.high.high,
        s.max.low,
        s.max.med,
        s.max.high,
    ]
}

// ---------------------------------------------------------------------------
// Tab 2: Clocks & Voltages Detail
// ---------------------------------------------------------------------------

fn draw_clocks_volts_tab(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(area);

    // 2x2 grid of sparklines: FCLK, UCLK, MCLK, VDDCR_SOC
    let top_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[0]);
    let r0 = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(top_rows[0]);
    let r1 = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(top_rows[1]);

    let snap = app.telemetry.as_ref();

    draw_metric_sparkline(
        frame,
        r0[0],
        "FCLK (Fabric)",
        "MHz",
        0,
        snap.and_then(|s| s.fclk_mhz),
        None,
        &app.history.fclk_mhz,
        1.0,
        palette::TEAL,
    );
    draw_metric_sparkline(
        frame,
        r0[1],
        "UCLK (Mem Ctrl)",
        "MHz",
        0,
        snap.and_then(|s| s.uclk_mhz),
        None,
        &app.history.uclk_mhz,
        1.0,
        palette::CYAN,
    );
    draw_metric_sparkline(
        frame,
        r1[0],
        "MCLK (Memory)",
        "MHz",
        0,
        snap.and_then(|s| s.mclk_mhz),
        None,
        &app.history.mclk_mhz,
        1.0,
        palette::PURPLE,
    );
    draw_metric_sparkline(
        frame,
        r1[1],
        "VDDCR_SOC",
        "V",
        3,
        snap.and_then(|s| s.vddcr_soc_v),
        Some(1.30),
        &app.history.vddcr_soc_v,
        1000.0,
        soc_voltage_color(snap.and_then(|s| s.vddcr_soc_v)),
    );

    // Bottom section: Voltage rails table + PM table metadata
    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(rows[1]);

    draw_voltage_rails_table(frame, bottom[0], app);
    draw_pm_metadata_card(frame, bottom[1], app, hw);
}

fn soc_voltage_color(v: Option<f32>) -> Color {
    match v {
        Some(val) if val >= 1.30 => palette::RED,
        Some(val) if val >= 1.25 => palette::YELLOW,
        Some(_) => palette::GREEN,
        None => palette::DIM,
    }
}

fn format_history_stats(history: &HistoryBuffer, decimals: usize, unit: &str) -> String {
    history
        .stats()
        .map(|(mn, av, mx)| format!("{mn:.decimals$} / {av:.decimals$} / {mx:.decimals$} {unit}"))
        .unwrap_or_else(|| "—".to_string())
}

fn push_apu_rail_rows<'a>(rows: &mut Vec<Row<'a>>, snap: &TelemetrySnapshot) {
    let extras = [
        (
            "SoC TDC",
            snap.tdc_soc_value_a,
            snap.tdc_soc_limit_a,
            "A",
            false,
        ),
        (
            "SoC EDC",
            snap.edc_soc_value_a,
            snap.edc_soc_limit_a,
            "A",
            false,
        ),
        (
            "APU Skin",
            snap.apu_skin_temp_value_c,
            snap.apu_skin_temp_limit_c,
            "°C",
            true,
        ),
        (
            "dGPU Skin",
            snap.dgpu_skin_temp_value_c,
            snap.dgpu_skin_temp_limit_c,
            "°C",
            true,
        ),
    ];
    for (label, val, lim, unit, is_thermal) in extras {
        if val.is_some() || lim.is_some() {
            let color = if is_thermal {
                thermal_color(val, lim)
            } else {
                ratio_color(gauge_ratio(val, lim))
            };
            rows.push(Row::new(vec![
                Cell::from(label).style(Style::default().fg(palette::FG)),
                Cell::from(format_opt_f32(val, 1, unit)).style(Style::default().fg(color)),
                Cell::from(format!("lim {}", format_opt_f32(lim, 1, unit)))
                    .style(Style::default().fg(palette::MUTED)),
            ]));
        }
    }
}

fn draw_voltage_rails_table(frame: &mut Frame, area: Rect, app: &App) {
    let block = styled_block(" Voltage Rails & Current Telemetry ", palette::PURPLE);
    let snap = app.telemetry.as_ref();

    let soc_val = snap.and_then(|s| s.vddcr_soc_v);
    let mut rows = vec![
        Row::new(vec![
            Cell::from("VDDCR_SOC").style(Style::default().fg(palette::FG)),
            Cell::from(format_opt_f32(soc_val, 3, "V")).style(
                Style::default()
                    .fg(soc_voltage_color(soc_val))
                    .add_modifier(Modifier::BOLD),
            ),
            Cell::from(format_history_stats(&app.history.vddcr_soc_v, 3, "V"))
                .style(Style::default().fg(palette::MUTED)),
        ]),
        Row::new(vec![
            Cell::from("VDDCR_MISC").style(Style::default().fg(palette::FG)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.vddcr_misc_v), 3, "V"))
                .style(Style::default().fg(palette::CYAN)),
            Cell::from("SoC misc rail").style(Style::default().fg(palette::DIM)),
        ]),
        Row::new(vec![
            Cell::from("CLDO_VDDP").style(Style::default().fg(palette::FG)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.cldo_vddp_v), 3, "V"))
                .style(Style::default().fg(palette::CYAN)),
            Cell::from("DDR PHY rail").style(Style::default().fg(palette::DIM)),
        ]),
        Row::new(vec![
            Cell::from("CLDO_VDDG_IOD").style(Style::default().fg(palette::FG)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.cldo_vddg_iod_v), 3, "V"))
                .style(Style::default().fg(palette::CYAN)),
            Cell::from("Infinity Fabric IOD").style(Style::default().fg(palette::DIM)),
        ]),
        Row::new(vec![
            Cell::from("CLDO_VDDG_CCD").style(Style::default().fg(palette::FG)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.cldo_vddg_ccd_v), 3, "V"))
                .style(Style::default().fg(palette::CYAN)),
            Cell::from("Infinity Fabric CCD").style(Style::default().fg(palette::DIM)),
        ]),
        Row::new(vec![
            Cell::from("TDC Current").style(Style::default().fg(palette::FG)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.tdc_value_a), 1, "A")).style(
                Style::default().fg(ratio_color(gauge_ratio(
                    snap.and_then(|s| s.tdc_value_a),
                    snap.and_then(|s| s.tdc_limit_a),
                ))),
            ),
            Cell::from(format_history_stats(&app.history.tdc_a, 1, "A"))
                .style(Style::default().fg(palette::MUTED)),
        ]),
        Row::new(vec![
            Cell::from("EDC Current").style(Style::default().fg(palette::FG)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.edc_value_a), 1, "A")).style(
                Style::default().fg(ratio_color(gauge_ratio(
                    snap.and_then(|s| s.edc_value_a),
                    snap.and_then(|s| s.edc_limit_a),
                ))),
            ),
            Cell::from(format_history_stats(&app.history.edc_a, 1, "A"))
                .style(Style::default().fg(palette::MUTED)),
        ]),
    ];

    if let Some(s) = snap {
        push_apu_rail_rows(&mut rows, s);
    }

    let header = Row::new(vec!["Rail / Metric", "Current", "Min / Avg / Max or Note"]).style(
        Style::default()
            .fg(palette::MUTED)
            .add_modifier(Modifier::BOLD),
    );

    let widths = [
        Constraint::Length(15),
        Constraint::Length(11),
        Constraint::Min(18),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(1);
    frame.render_widget(table, area);
}

fn draw_pm_metadata_card(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let block = styled_block(" PM Table & Platform Metadata ", palette::TEAL);
    let snap = app.telemetry.as_ref();

    let pm_ver = snap
        .map(|s| format!("0x{:08X}", s.table_version))
        .or_else(|| hw.driver.pm_table_version.map(|v| format!("0x{v:08X}")))
        .unwrap_or_else(|| "—".to_string());
    let pm_size = snap
        .map(|s| format!("0x{:X} ({} B)", s.table_size, s.table_size))
        .or_else(|| {
            hw.driver
                .pm_table_size
                .map(|sz| format!("0x{sz:X} ({sz} B)"))
        })
        .unwrap_or_else(|| "—".to_string());
    let topo = format!(
        "{} CCD · {}C / {}T",
        hw.topology.ccd_count, hw.topology.physical_cores, hw.topology.logical_cpus
    );

    let rows = vec![
        kv_row(
            "Codename",
            hw.codename.display_name().to_string(),
            palette::CYAN,
        ),
        kv_row("PM Version", pm_ver, palette::TEAL),
        kv_row("PM Size", pm_size, palette::FG),
        kv_row("SMU Firmware", hw.driver.smu_version.clone(), palette::FG),
        kv_row("Driver Version", hw.driver.drv_version.clone(), palette::FG),
        kv_row(
            "MP1 IF Ver",
            format!("0x{:X}", hw.driver.mp1_if_version),
            palette::MUTED,
        ),
        kv_row("Topology", topo, palette::GREEN),
    ];

    let widths = [Constraint::Length(15), Constraint::Min(16)];
    let table = Table::new(rows, widths).block(block).column_spacing(1);
    frame.render_widget(table, area);
}

fn kv_row<'a>(k: &'a str, v: String, val_color: Color) -> Row<'a> {
    Row::new(vec![
        Cell::from(k).style(Style::default().fg(palette::MUTED)),
        Cell::from(v).style(Style::default().fg(val_color)),
    ])
}

// ---------------------------------------------------------------------------
// Tab 3: Curve Optimizer & Curve Shaper
// ---------------------------------------------------------------------------

fn draw_co_shaper_tab(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    if let Some(ref note) = app.mailbox_note {
        if !app.has_any_mailbox_data() {
            draw_unprivileged_tab3(frame, area, note, hw);
            return;
        }
    }

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(54), Constraint::Percentage(46)])
        .split(area);

    draw_co_table(frame, cols[0], app, hw);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(52), Constraint::Percentage(48)])
        .split(cols[1]);

    draw_shaper_matrix(frame, right[0], app, hw);
    draw_fused_pbo_table(frame, right[1], app, hw);
}

fn draw_unprivileged_tab3(frame: &mut Frame, area: Rect, note: &str, hw: &HardwareContext) {
    let block = styled_block(
        " Curve Optimizer, Curve Shaper & Fused PBO ",
        palette::YELLOW,
    )
    .padding(Padding::uniform(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let bounds_str = hw
        .capabilities
        .co_bounds
        .map(|b| format!("{}..={} (soft >= {})", b.min, b.max, b.soft_min))
        .unwrap_or_else(|| "unsupported".to_string());
    let shaper_cap = if hw.capabilities.get_curve_shaper.is_some() {
        "supported (Zen 5 5x3 matrix)"
    } else {
        "unsupported (Zen 5 only)"
    };

    let lines = vec![
        Line::from(vec![badge_span(
            " PRIVILEGED MAILBOX REQUIRED ",
            palette::YELLOW,
        )]),
        Line::from(""),
        Line::from(Span::styled(
            note,
            Style::default()
                .fg(palette::FG)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Launch with: ", Style::default().fg(palette::MUTED)),
            Span::styled(
                "sudo zensmu top",
                Style::default()
                    .fg(palette::CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("   (or press ", Style::default().fg(palette::MUTED)),
            Span::styled(
                "r",
                Style::default()
                    .fg(palette::TEAL)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" to retry)", Style::default().fg(palette::MUTED)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Detected Platform Capabilities:",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("  CPU Codename:   ", Style::default().fg(palette::MUTED)),
            Span::styled(hw.codename.display_name(), Style::default().fg(palette::FG)),
        ]),
        Line::from(vec![
            Span::styled("  Topology:       ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format!(
                    "{} CCD, {} physical cores, {} logical CPUs",
                    hw.topology.ccd_count, hw.topology.physical_cores, hw.topology.logical_cpus
                ),
                Style::default().fg(palette::FG),
            ),
        ]),
        Line::from(vec![
            Span::styled("  CO Bounds:      ", Style::default().fg(palette::MUTED)),
            Span::styled(bounds_str, Style::default().fg(palette::CYAN)),
        ]),
        Line::from(vec![
            Span::styled("  Curve Shaper:   ", Style::default().fg(palette::MUTED)),
            Span::styled(shaper_cap, Style::default().fg(palette::CYAN)),
        ]),
    ];

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
}

fn draw_co_table(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let max_abs = hw
        .capabilities
        .co_bounds
        .map(|b| b.min.abs().max(b.max.abs()))
        .unwrap_or(50);

    let igpu_suffix = match &app.co_state {
        MailboxState::Loaded(snap) => match snap.igpu {
            Some(v) => format!(" · iGPU {} ", format_signed_margin(v)),
            None => " · iGPU — ".to_string(),
        },
        _ => " ".to_string(),
    };

    let title = Line::from(vec![
        Span::styled(
            " Per-Core Curve Optimizer",
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(igpu_suffix, Style::default().fg(palette::TEAL)),
    ]);

    let block = styled_block(title, palette::CYAN);

    match &app.co_state {
        MailboxState::Loaded(co_snap) => {
            let perf_scores = app
                .pbo_state
                .as_loaded()
                .map(|p| p.core_perf_scores.as_slice())
                .unwrap_or(&[]);

            let rows: Vec<Row<'_>> = co_snap
                .cores
                .iter()
                .map(|c| {
                    let color = margin_color(c.margin);
                    let bar = format_co_bar(c.margin, max_abs, 6);
                    let perf = perf_scores
                        .iter()
                        .find(|(idx, _)| *idx == c.core_index)
                        .map(|(_, score)| score.to_string())
                        .unwrap_or_else(|| "—".to_string());

                    Row::new(vec![
                        Cell::from(format!("C{}", c.core_index)).style(
                            Style::default()
                                .fg(palette::FG)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Cell::from(c.ccd.to_string()).style(Style::default().fg(palette::MUTED)),
                        Cell::from(c.core_in_ccd.to_string())
                            .style(Style::default().fg(palette::MUTED)),
                        Cell::from(format_linux_cpus(&c.linux_cpus))
                            .style(Style::default().fg(palette::DIM)),
                        Cell::from(format!("{:>4}", format_signed_margin(c.margin)))
                            .style(Style::default().fg(color).add_modifier(Modifier::BOLD)),
                        Cell::from(bar).style(Style::default().fg(color)),
                        Cell::from(perf).style(Style::default().fg(palette::TEAL)),
                    ])
                })
                .collect();

            let header = Row::new(vec![
                "Core",
                "CCD",
                "InCCD",
                "CPUs",
                "CO",
                "Offset Bar",
                "Perf",
            ])
            .style(
                Style::default()
                    .fg(palette::MUTED)
                    .add_modifier(Modifier::BOLD),
            );

            let widths = [
                Constraint::Length(5),
                Constraint::Length(4),
                Constraint::Length(5),
                Constraint::Length(8),
                Constraint::Length(5),
                Constraint::Length(16),
                Constraint::Min(4),
            ];

            let table = Table::new(rows, widths)
                .header(header)
                .block(block)
                .column_spacing(1);
            frame.render_widget(table, area);
        }
        other => {
            render_mailbox_placeholder(
                frame,
                area,
                block,
                format!(" {}", mailbox_status_label(other, &hw.driver.smu_version)),
                palette::MUTED,
                false,
            );
        }
    }
}

fn draw_shaper_matrix(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let block = styled_block(" Zen 5 Curve Shaper (5x3 Matrix) ", palette::TEAL);

    match &app.shaper_state {
        MailboxState::Loaded(shaper) => {
            let rows: Vec<Row<'_>> = FreqTier::ALL
                .iter()
                .map(|&tier| {
                    let m = shaper.tier(tier);
                    let cell = |v: i8| {
                        let col = margin_color(i16::from(v));
                        Cell::from(format!("{:>5}", format_signed_margin(i16::from(v))))
                            .style(Style::default().fg(col).add_modifier(Modifier::BOLD))
                    };
                    Row::new(vec![
                        Cell::from(format!("{:<5}", tier.label().to_ascii_uppercase())).style(
                            Style::default()
                                .fg(palette::FG)
                                .add_modifier(Modifier::BOLD),
                        ),
                        cell(m.low),
                        cell(m.med),
                        cell(m.high),
                    ])
                })
                .collect();

            let header = Row::new(vec!["Freq", "  Low°C", "  Med°C", " High°C"]).style(
                Style::default()
                    .fg(palette::MUTED)
                    .add_modifier(Modifier::BOLD),
            );

            let widths = [
                Constraint::Length(7),
                Constraint::Percentage(31),
                Constraint::Percentage(31),
                Constraint::Percentage(31),
            ];

            let table = Table::new(rows, widths)
                .header(header)
                .block(block)
                .column_spacing(1);
            frame.render_widget(table, area);
        }
        MailboxState::Unsupported => {
            render_mailbox_placeholder(
                frame,
                area,
                block,
                format!(
                    " Not supported on {} (Zen 5 only)",
                    hw.codename.display_name()
                ),
                palette::DIM,
                true,
            );
        }
        other => {
            render_mailbox_placeholder(
                frame,
                area,
                block,
                format!(" {}", mailbox_status_label(other, &hw.driver.smu_version)),
                palette::MUTED,
                false,
            );
        }
    }
}

fn draw_fused_pbo_table(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let block = styled_block(" Fused Factory PBO Limits & Status ", palette::PURPLE);

    match &app.pbo_state {
        MailboxState::Loaded(pbo) => {
            let flags_str = format!(
                "OC:{}  EXPO:{}  LN2:{}",
                fmt_bool_opt(pbo.flags.overclockable),
                fmt_bool_opt(pbo.flags.expo_active),
                fmt_bool_opt(pbo.flags.ln2_mode),
            );
            let mut rows = vec![
                kv_row(
                    "Fused PPT",
                    format_opt_u32(pbo.fused.ppt_w, "W"),
                    palette::CYAN,
                ),
                kv_row(
                    "Fused VDD TDC / EDC",
                    format!(
                        "{} / {}",
                        format_opt_u32(pbo.fused.vdd_tdc_a, "A"),
                        format_opt_u32(pbo.fused.edc_a, "A"),
                    ),
                    palette::TEAL,
                ),
            ];
            if pbo.fused.tdp_w.is_some() || pbo.fused.tjmax_c.is_some() {
                rows.push(kv_row(
                    "Fused TDP / TjMax",
                    format!(
                        "{} / {}",
                        format_opt_u32(pbo.fused.tdp_w, "W"),
                        format_opt_u32(pbo.fused.tjmax_c, "°C"),
                    ),
                    palette::ORANGE,
                ));
            }
            if pbo.fused.soc_tdc_a.is_some() {
                rows.push(kv_row(
                    "Fused SoC TDC",
                    format_opt_u32(pbo.fused.soc_tdc_a, "A"),
                    palette::FG,
                ));
            }
            if pbo.fused.slow_w.is_some() || pbo.fused.fast_w.is_some() {
                rows.push(kv_row(
                    "Fused Slow / Fast",
                    format!(
                        "{} / {}",
                        format_opt_u32(pbo.fused.slow_w, "W"),
                        format_opt_u32(pbo.fused.fast_w, "W"),
                    ),
                    palette::FG,
                ));
            }
            if pbo.fused.apu_slow_w.is_some() {
                rows.push(kv_row(
                    "Fused APU Slow",
                    format_opt_u32(pbo.fused.apu_slow_w, "W"),
                    palette::FG,
                ));
            }
            rows.push(kv_row(
                "PBO Scalar / Fmax",
                format!(
                    "{}  ·  {}",
                    format_opt_f32(pbo.scalar, 2, "x"),
                    format_opt_u32(pbo.fmax_mhz, "MHz"),
                ),
                palette::YELLOW,
            ));
            rows.push(kv_row("Platform Flags", flags_str, palette::GREEN));

            let widths = [Constraint::Length(20), Constraint::Min(16)];
            let table = Table::new(rows, widths).block(block).column_spacing(1);
            frame.render_widget(table, area);
        }
        other => {
            render_mailbox_placeholder(
                frame,
                area,
                block,
                format!(" {}", mailbox_status_label(other, &hw.driver.smu_version)),
                palette::MUTED,
                false,
            );
        }
    }
}

fn fmt_bool_opt(b: Option<bool>) -> &'static str {
    match b {
        Some(true) => "ON",
        Some(false) => "OFF",
        None => "—",
    }
}

// ---------------------------------------------------------------------------
// Tab 4: Interactive Tune (with always-live telemetry strip)
// ---------------------------------------------------------------------------

fn format_tune_param_label(param: TuneParam, hw: &HardwareContext) -> String {
    match param {
        TuneParam::CoAll => "CO All-Core".to_string(),
        TuneParam::CoCore(idx) => {
            if let Some(c) = hw.topology.core(idx) {
                format!("CO Core {idx} (CCD{}·L{})", c.ccd, c.core_in_ccd)
            } else {
                format!("CO Core {idx}")
            }
        }
        TuneParam::CoIgpu => "CO iGPU".to_string(),
        TuneParam::Shaper { tier, band } => {
            format!(
                "Shaper {:<4} / {:<4} °C",
                tier.label().to_ascii_uppercase(),
                band.label()
            )
        }
        TuneParam::PboPpt => "PPT Limit (W)".to_string(),
        TuneParam::PboTdc => "TDC Limit (A)".to_string(),
        TuneParam::PboEdc => "EDC Limit (A)".to_string(),
        TuneParam::PboTctl => "Tctl Max (°C)".to_string(),
        TuneParam::PboScalar => "PBO Scalar (x)".to_string(),
        TuneParam::PboFmax => "Fmax Boost (MHz)".to_string(),
        TuneParam::ApuStapm => "STAPM Limit (W)".to_string(),
        TuneParam::ApuStapmTime => "STAPM Time (s)".to_string(),
        TuneParam::ApuFast => "Fast PPT Limit (W)".to_string(),
        TuneParam::ApuSlow => "Slow PPT Limit (W)".to_string(),
        TuneParam::ApuSlowTime => "Slow Time (s)".to_string(),
        TuneParam::ApuApuSlow => "APU Slow Limit (W)".to_string(),
        TuneParam::ApuTdcSoc => "SoC TDC Limit (A)".to_string(),
        TuneParam::ApuEdcSoc => "SoC EDC Limit (A)".to_string(),
        TuneParam::ApuSkinTemp => "APU Skin Temp (°C)".to_string(),
        TuneParam::ApuDgpuSkinTemp => "dGPU Skin Temp (°C)".to_string(),
        TuneParam::ApuPowerMode => "Power Mode".to_string(),
    }
}

fn format_tune_value(param: TuneParam, val: TuneValue) -> String {
    match (param, val) {
        (
            TuneParam::CoAll | TuneParam::CoCore(_) | TuneParam::CoIgpu | TuneParam::Shaper { .. },
            TuneValue::Int(v),
        ) => format_signed_margin(v as i16),
        (
            TuneParam::PboPpt
            | TuneParam::ApuStapm
            | TuneParam::ApuFast
            | TuneParam::ApuSlow
            | TuneParam::ApuApuSlow,
            TuneValue::Int(v),
        ) => format!("{v} W"),
        (TuneParam::ApuStapmTime | TuneParam::ApuSlowTime, TuneValue::Int(v)) => format!("{v} s"),
        (
            TuneParam::PboTdc | TuneParam::PboEdc | TuneParam::ApuTdcSoc | TuneParam::ApuEdcSoc,
            TuneValue::Int(v),
        ) => format!("{v} A"),
        (
            TuneParam::PboTctl | TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp,
            TuneValue::Int(v),
        ) => format!("{v} °C"),
        (TuneParam::PboScalar, TuneValue::Int(v)) => format!("{v}x"),
        (TuneParam::PboFmax, TuneValue::Int(v)) => format!("{v} MHz"),
        (TuneParam::ApuPowerMode, TuneValue::PowerMode(mode)) => match mode {
            None => "auto".to_string(),
            Some(ApuPowerMode::MaxPerformance) => "max-performance".to_string(),
            Some(ApuPowerMode::PowerSaving) => "power-saving".to_string(),
        },
        (_, TuneValue::Int(v)) => v.to_string(),
        (_, TuneValue::PowerMode(_)) => "—".to_string(),
    }
}

fn format_live_param_display(app: &App, param: TuneParam) -> String {
    if param == TuneParam::CoAll {
        if let Some(snap) = app.co_state.as_loaded() {
            if let Some(first) = snap.cores.first() {
                if snap.cores.iter().all(|c| c.margin == first.margin) {
                    return format_signed_margin(first.margin);
                }
                return "mixed".to_string();
            }
        }
        return "—".to_string();
    }
    app.live_param_value(param)
        .map(|v| format_tune_value(param, v))
        .unwrap_or_else(|| "—".to_string())
}

fn param_base_bounds_summary(app: &App, param: TuneParam, hw: &HardwareContext) -> String {
    match param {
        TuneParam::CoAll | TuneParam::CoCore(_) => hw
            .capabilities
            .co_bounds
            .map(|b| format!("{}..={} (soft >= {})", b.min, b.max, b.soft_min))
            .unwrap_or_else(|| "—".to_string()),
        TuneParam::CoIgpu => format!(
            "{}..={} (soft >= {})",
            co::IGPU_CO_MIN,
            co::IGPU_CO_MAX,
            co::IGPU_CO_SOFT_MIN
        ),
        TuneParam::Shaper { .. } => format!(
            "{}..=+{} (soft >= {})",
            shaper::SHAPER_MIN,
            shaper::SHAPER_MAX,
            shaper::SHAPER_SOFT_MIN
        ),
        TuneParam::PboPpt => format!(
            "{}..={} W (soft <= {} W)",
            pbo::PPT_MIN_W,
            pbo::PPT_MAX_W,
            pbo::PPT_SOFT_MAX_W
        ),
        TuneParam::PboTdc => format!(
            "{}..={} A (soft <= {} A)",
            pbo::TDC_MIN_A,
            pbo::TDC_MAX_A,
            pbo::TDC_SOFT_MAX_A
        ),
        TuneParam::PboEdc => format!(
            "{}..={} A (soft <= {} A)",
            pbo::EDC_MIN_A,
            pbo::EDC_MAX_A,
            pbo::EDC_SOFT_MAX_A
        ),
        TuneParam::PboTctl => format!(
            "{}..={} °C (hard ceiling)",
            pbo::TCTL_MIN_C,
            pbo::TCTL_MAX_C
        ),
        TuneParam::PboScalar => format!(
            "{}..={}x (soft <= {}x)",
            pbo::SCALAR_MIN,
            pbo::SCALAR_MAX,
            pbo::SCALAR_SOFT_MAX
        ),
        TuneParam::PboFmax => {
            if let Some(cur) = app.pbo_state.as_loaded().and_then(|p| p.fmax_mhz) {
                format!(
                    "{}..={} MHz (soft <= {})",
                    pbo::FMAX_MIN_MHZ,
                    pbo::FMAX_MAX_MHZ,
                    cur.saturating_add(pbo::FMAX_SOFT_DELTA_MHZ)
                )
            } else {
                format!("{}..={} MHz", pbo::FMAX_MIN_MHZ, pbo::FMAX_MAX_MHZ)
            }
        }
        TuneParam::ApuStapm | TuneParam::ApuSlow | TuneParam::ApuApuSlow => format!(
            "{}..={} W (soft <= {} W)",
            apu::APU_POWER_MIN_W,
            apu::APU_POWER_MAX_W,
            apu::APU_POWER_SOFT_MAX_W
        ),
        TuneParam::ApuStapmTime | TuneParam::ApuSlowTime => {
            format!("{}..={} s", apu::APU_TIME_MIN_S, apu::APU_TIME_MAX_S)
        }
        TuneParam::ApuFast => format!(
            "{}..={} W (soft <= {} W)",
            apu::APU_POWER_MIN_W,
            apu::APU_POWER_MAX_W,
            apu::APU_FAST_POWER_SOFT_MAX_W
        ),
        TuneParam::ApuTdcSoc => format!(
            "{}..={} A (soft <= {} A)",
            apu::APU_SOC_TDC_MIN_A,
            apu::APU_SOC_TDC_MAX_A,
            apu::APU_SOC_TDC_SOFT_MAX_A
        ),
        TuneParam::ApuEdcSoc => format!(
            "{}..={} A (soft <= {} A)",
            apu::APU_SOC_EDC_MIN_A,
            apu::APU_SOC_EDC_MAX_A,
            apu::APU_SOC_EDC_SOFT_MAX_A
        ),
        TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp => format!(
            "{}..={} °C (soft <= {} °C)",
            apu::APU_SKIN_TEMP_MIN_C,
            apu::APU_SKIN_TEMP_MAX_C,
            apu::APU_SKIN_TEMP_SOFT_MAX_C
        ),
        TuneParam::ApuPowerMode => "auto / max-performance / power-saving".to_string(),
    }
}

fn param_bounds_summary(app: &App, param: TuneParam, hw: &HardwareContext) -> String {
    let mut base = param_base_bounds_summary(app, param, hw);
    if let Some(&cap) = app.board_caps.get(&param) {
        base.push_str(&format!(" · {}", format_board_cap(param, cap)));
    }
    base
}

fn estimate_wrapped_lines(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let mut lines = 1usize;
    let mut col = 0usize;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if col == 0 {
            if wlen > width {
                lines += wlen.saturating_sub(1) / width;
                col = ((wlen - 1) % width) + 1;
            } else {
                col = wlen;
            }
        } else if col + 1 + wlen <= width {
            col += 1 + wlen;
        } else {
            lines += 1;
            if wlen > width {
                lines += wlen.saturating_sub(1) / width;
                col = ((wlen - 1) % width) + 1;
            } else {
                col = wlen;
            }
        }
    }
    lines
}

fn draw_tune_tab(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(8)])
        .split(area);

    draw_tune_telemetry_strip(frame, rows[0], app);

    if rows[1].width >= 94 {
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(62), Constraint::Percentage(38)])
            .split(rows[1]);
        draw_tune_param_list(frame, cols[0], app, hw);
        draw_tune_inspector_pane(frame, cols[1], app, hw);
    } else {
        let compact_h = if let Some((ref msg, _)) = app.last_alert {
            let inner_w = rows[1].width.saturating_sub(4).max(12) as usize;
            let alert_lines =
                estimate_wrapped_lines(&format!("Last Event: {msg}"), inner_w).clamp(1, 4) as u16;
            (4 + alert_lines).min(rows[1].height.saturating_sub(5).max(4))
        } else {
            4
        };
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(5), Constraint::Length(compact_h)])
            .split(rows[1]);
        draw_tune_param_list(frame, split[0], app, hw);
        draw_tune_compact_bar(frame, split[1], app, hw);
    }
}

fn build_tune_strip_title(app: &App) -> Line<'static> {
    let mut title_spans = vec![Span::styled(
        " Live Telemetry ",
        Style::default()
            .fg(palette::CYAN)
            .add_modifier(Modifier::BOLD),
    )];
    let (boot_badge, boot_style) = if app.persist_status.service_enabled {
        (
            "[BOOT:ON] ",
            Style::default()
                .fg(palette::GREEN)
                .add_modifier(Modifier::BOLD),
        )
    } else if app.persist_status.active_profile_exists {
        (
            "[BOOT:SAVED] ",
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        ("[BOOT:OFF] ", Style::default().fg(palette::MUTED))
    };
    title_spans.push(Span::styled(boot_badge, boot_style));
    if app.persist_status.boot_guard_tripped {
        title_spans.push(Span::styled(
            "[GUARD:TRIPPED] ",
            Style::default()
                .fg(palette::RED)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if app.paused {
        title_spans.push(Span::styled(
            "[PAUSED] ",
            Style::default()
                .fg(palette::YELLOW)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if app.allow_extreme {
        title_spans.push(Span::styled(
            "[EXTREME ON] ",
            Style::default()
                .fg(palette::ORANGE)
                .add_modifier(Modifier::BOLD),
        ));
    }
    let staged_n = app.staged_count();
    if staged_n > 0 {
        title_spans.push(badge_span(
            format!("[*{staged_n} STAGED] "),
            palette::ORANGE,
        ));
    }
    if !app.can_write() {
        title_spans.push(Span::styled(
            "[READ-ONLY · sudo to write] ",
            Style::default().fg(palette::YELLOW),
        ));
    }
    Line::from(title_spans)
}

fn push_telemetry_pair_spans(
    spans: &mut Vec<Span<'static>>,
    label: &'static str,
    val: Option<f32>,
    lim: Option<f32>,
    unit: &'static str,
    is_thermal: bool,
) {
    if !spans.is_empty() {
        spans.push(Span::styled(" · ", Style::default().fg(palette::MUTED)));
    }
    let formatted = match (val, lim) {
        (Some(v), Some(l)) if v.is_finite() && l.is_finite() => format!("{v:.1}/{l:.0}{unit}"),
        (Some(v), _) if v.is_finite() => format!("{v:.1}/—{unit}"),
        _ => format!("—/—{unit}"),
    };
    let color = if is_thermal {
        thermal_color(val, lim)
    } else {
        ratio_color(gauge_ratio(val, lim))
    };
    spans.push(Span::styled(
        format!("{label} "),
        Style::default().fg(palette::MUTED),
    ));
    spans.push(Span::styled(
        formatted,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    ));
}

fn draw_tune_telemetry_strip(frame: &mut Frame, area: Rect, app: &App) {
    let block =
        styled_block(build_tune_strip_title(app), palette::CYAN).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let snap = app.telemetry.as_ref();
    let mut line1_spans = Vec::new();
    if let Some(s) = snap {
        if s.stapm_limit_w.is_some() || s.stapm_value_w.is_some() {
            push_telemetry_pair_spans(
                &mut line1_spans,
                "STAPM",
                s.stapm_value_w,
                s.stapm_limit_w,
                "W",
                false,
            );
        }
    }
    push_telemetry_pair_spans(
        &mut line1_spans,
        "PPT",
        snap.and_then(|s| s.ppt_value_w),
        snap.and_then(|s| s.ppt_limit_w),
        "W",
        false,
    );
    push_telemetry_pair_spans(
        &mut line1_spans,
        "TDC",
        snap.and_then(|s| s.tdc_value_a),
        snap.and_then(|s| s.tdc_limit_a),
        "A",
        false,
    );
    push_telemetry_pair_spans(
        &mut line1_spans,
        "EDC",
        snap.and_then(|s| s.edc_value_a),
        snap.and_then(|s| s.edc_limit_a),
        "A",
        false,
    );
    push_telemetry_pair_spans(
        &mut line1_spans,
        "Tctl",
        snap.and_then(|s| s.tctl_value_c),
        snap.and_then(|s| s.tctl_limit_c),
        "°C",
        true,
    );
    let line1 = Line::from(line1_spans);

    let fclk = snap.and_then(|s| s.fclk_mhz);
    let uclk = snap.and_then(|s| s.uclk_mhz);
    let mclk = snap.and_then(|s| s.mclk_mhz);
    let soc = snap.and_then(|s| s.vddcr_soc_v);
    let line2 = Line::from(vec![
        Span::styled("FCLK ", Style::default().fg(palette::MUTED)),
        Span::styled(
            format_opt_f32(fclk, 0, ""),
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · UCLK ", Style::default().fg(palette::MUTED)),
        Span::styled(
            format_opt_f32(uclk, 0, ""),
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · MCLK ", Style::default().fg(palette::MUTED)),
        Span::styled(
            format_opt_f32(mclk, 0, "MHz"),
            Style::default()
                .fg(palette::PURPLE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · SOC ", Style::default().fg(palette::MUTED)),
        Span::styled(
            format_opt_f32(soc, 3, "V"),
            Style::default()
                .fg(soc_voltage_color(soc))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" · #{}", app.sample_count),
            Style::default().fg(palette::DIM),
        ),
    ]);

    let lines = if inner.height >= 2 {
        vec![line1, line2]
    } else {
        vec![line1]
    };
    frame.render_widget(Paragraph::new(lines), inner);
}

fn tune_param_live_style(param: TuneParam, live: Option<TuneValue>) -> Style {
    match (param, live) {
        (
            TuneParam::CoAll | TuneParam::CoCore(_) | TuneParam::CoIgpu | TuneParam::Shaper { .. },
            Some(TuneValue::Int(v)),
        ) => Style::default()
            .fg(margin_color(v as i16))
            .add_modifier(Modifier::BOLD),
        (
            TuneParam::PboPpt
            | TuneParam::ApuStapm
            | TuneParam::ApuFast
            | TuneParam::ApuSlow
            | TuneParam::ApuApuSlow
            | TuneParam::ApuPowerMode,
            Some(_),
        ) => Style::default()
            .fg(palette::CYAN)
            .add_modifier(Modifier::BOLD),
        (
            TuneParam::PboTdc | TuneParam::PboEdc | TuneParam::ApuTdcSoc | TuneParam::ApuEdcSoc,
            Some(_),
        ) => Style::default()
            .fg(palette::TEAL)
            .add_modifier(Modifier::BOLD),
        (TuneParam::ApuStapmTime | TuneParam::ApuSlowTime | TuneParam::PboScalar, Some(_)) => {
            Style::default()
                .fg(palette::PURPLE)
                .add_modifier(Modifier::BOLD)
        }
        (
            TuneParam::PboTctl | TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp,
            Some(TuneValue::Int(c)),
        ) => {
            let col = if c >= 90 {
                palette::ORANGE
            } else {
                palette::YELLOW
            };
            Style::default().fg(col).add_modifier(Modifier::BOLD)
        }
        (TuneParam::PboFmax, Some(_)) => Style::default()
            .fg(palette::GREEN)
            .add_modifier(Modifier::BOLD),
        _ => Style::default().fg(palette::MUTED),
    }
}

fn format_staged_cell(
    app: &App,
    hw: &HardwareContext,
    param: TuneParam,
    staged_val: Option<TuneValue>,
    is_selected: bool,
) -> Cell<'static> {
    if let Some(sv) = staged_val {
        let exceeds = app.param_exceeds_soft_limit(param, sv, hw);
        let exceeds_cap = app
            .board_caps
            .get(&param)
            .is_some_and(|&cap| sv.as_int().is_some_and(|v| v > cap as i32));
        let suffix = if exceeds && !app.allow_extreme {
            " [!soft]"
        } else if exceeds_cap {
            " [!cap]"
        } else if exceeds {
            " [EXT]"
        } else {
            " *"
        };
        let col = if exceeds && !app.allow_extreme {
            palette::RED
        } else if exceeds_cap {
            palette::YELLOW
        } else {
            palette::ORANGE
        };
        Cell::from(format!("→ {}{suffix}", format_tune_value(param, sv)))
            .style(Style::default().fg(col).add_modifier(Modifier::BOLD))
    } else if let Some(&cap) = app.board_caps.get(&param) {
        Cell::from(format!("· {}", format_board_cap(param, cap))).style(
            Style::default()
                .fg(palette::ORANGE)
                .add_modifier(Modifier::BOLD),
        )
    } else if is_selected {
        Cell::from("←/→ adjust").style(Style::default().fg(palette::CYAN))
    } else {
        Cell::from("·").style(Style::default().fg(palette::DIM))
    }
}

fn locate_selected_row_and_scroll(
    tune_rows: &[TuneRow],
    clamped_sel: usize,
    visible_rows: usize,
) -> usize {
    let mut selected_row_idx = 0usize;
    let mut seen_params = 0usize;
    for (row_idx, row) in tune_rows.iter().enumerate() {
        if matches!(row, TuneRow::Param(_)) {
            if seen_params == clamped_sel {
                selected_row_idx = row_idx;
                break;
            }
            seen_params += 1;
        }
    }

    if tune_rows.len() <= visible_rows {
        0
    } else {
        let desired = selected_row_idx.saturating_sub(visible_rows / 2);
        desired.min(tune_rows.len().saturating_sub(visible_rows))
    }
}

fn draw_tune_param_list(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let tune_rows = app.build_tune_rows(hw);
    let param_count = tune_rows
        .iter()
        .filter(|r| matches!(r, TuneRow::Param(_)))
        .count();

    let title =
        format!(" Interactive SMU Controls ({param_count} params · [/] Section · ←→/h/l Step) ");
    let block = styled_block(title, palette::TEAL);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    if tune_rows.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " No writable SMU capabilities detected on this CPU.",
                Style::default().fg(palette::MUTED),
            )),
            inner,
        );
        return;
    }

    // Reserve 1 line for table header
    let visible_rows = inner.height.saturating_sub(1).max(1) as usize;
    let clamped_sel = app.tune_selected.min(param_count.saturating_sub(1));
    let scroll_top = locate_selected_row_and_scroll(&tune_rows, clamped_sel, visible_rows);

    let mut table_rows: Vec<Row<'_>> = Vec::with_capacity(visible_rows);
    let mut param_ordinal = 0usize;
    for (row_idx, row) in tune_rows.iter().enumerate() {
        let is_selected = match row {
            TuneRow::Param(_) => {
                let matched = param_ordinal == clamped_sel;
                param_ordinal += 1;
                matched
            }
            _ => false,
        };

        if row_idx < scroll_top || table_rows.len() >= visible_rows {
            continue;
        }

        match row {
            TuneRow::SectionHeader(header_text, hint) => {
                table_rows.push(
                    Row::new(vec![
                        Cell::from(format!("── {header_text}")).style(
                            Style::default()
                                .fg(palette::TEAL)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Cell::from(""),
                        Cell::from(hint.as_str()).style(Style::default().fg(palette::MUTED)),
                    ])
                    .style(Style::default().bg(palette::BAR_BG)),
                );
            }
            TuneRow::InfoBanner(msg, color) => {
                table_rows.push(Row::new(vec![
                    Cell::from(format!("  ! {msg}")).style(Style::default().fg(*color)),
                    Cell::from("n/a").style(Style::default().fg(palette::DIM)),
                    Cell::from("BIOS / newer AGESA").style(Style::default().fg(palette::MUTED)),
                ]));
            }
            TuneRow::Param(param) => {
                let label = format_tune_param_label(*param, hw);
                let live_str = format_live_param_display(app, *param);
                let staged_val = app.staged.get(param).copied();

                let prefix = if is_selected { "▸ " } else { "  " };
                let label_style = if is_selected {
                    Style::default()
                        .fg(palette::CYAN)
                        .add_modifier(Modifier::BOLD)
                } else if staged_val.is_some() {
                    Style::default()
                        .fg(palette::FG)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(palette::FG)
                };

                let live_style = tune_param_live_style(*param, app.live_param_value(*param));
                let staged_cell = format_staged_cell(app, hw, *param, staged_val, is_selected);

                let mut r = Row::new(vec![
                    Cell::from(format!("{prefix}{label}")).style(label_style),
                    Cell::from(format!("{live_str:>10}")).style(live_style),
                    staged_cell,
                ]);
                if is_selected {
                    r = r.style(Style::default().bg(palette::ROW_HL));
                }
                table_rows.push(r);
            }
        }
    }

    let header = Row::new(vec!["  Parameter", "      Live", "Staged Target"]).style(
        Style::default()
            .fg(palette::MUTED)
            .add_modifier(Modifier::BOLD),
    );

    let widths = [
        Constraint::Min(24),
        Constraint::Length(11),
        Constraint::Length(22),
    ];

    let table = Table::new(table_rows, widths)
        .header(header)
        .column_spacing(1);
    frame.render_widget(table, inner);
}

fn draw_tune_inspector_pane(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    if let Some((ref msg, kind)) = app.last_alert {
        let inner_w = area.width.saturating_sub(4).max(12) as usize;
        let alert_h = (estimate_wrapped_lines(msg, inner_w).clamp(1, 5) as u16) + 2;
        let constraints = if area.height >= 26 {
            [
                Constraint::Length(alert_h),
                Constraint::Length(10),
                Constraint::Length(7),
                Constraint::Min(5),
            ]
        } else {
            [
                Constraint::Length(alert_h.min(area.height / 2).max(3)),
                Constraint::Percentage(36),
                Constraint::Percentage(32),
                Constraint::Percentage(32),
            ]
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        draw_last_event_card(frame, chunks[0], msg, kind);
        draw_selected_param_card(frame, chunks[1], app, hw);
        draw_boot_persistence_card(frame, chunks[2], app);
        draw_safety_actions_card(frame, chunks[3], app);
        return;
    }

    let persist_h = if app.persist_status.boot_guard_tripped {
        9
    } else {
        8
    };
    let constraints = if area.height >= 26 {
        [
            Constraint::Length(11),
            Constraint::Length(persist_h),
            Constraint::Min(8),
        ]
    } else {
        [
            Constraint::Percentage(36),
            Constraint::Percentage(32),
            Constraint::Percentage(32),
        ]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    draw_selected_param_card(frame, chunks[0], app, hw);
    draw_boot_persistence_card(frame, chunks[1], app);
    draw_safety_actions_card(frame, chunks[2], app);
}

fn draw_last_event_card(frame: &mut Frame, area: Rect, msg: &str, kind: FlashKind) {
    let accent = match kind {
        FlashKind::Warn => palette::YELLOW,
        FlashKind::Error => palette::RED,
        FlashKind::Success => palette::GREEN,
        FlashKind::Info => palette::TEAL,
    };
    let block = styled_block(" Last Event ", accent)
        .border_style(Style::default().fg(accent))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let para = Paragraph::new(Line::from(Span::styled(
        msg,
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    )))
    .wrap(Wrap { trim: true });
    frame.render_widget(para, inner);
}

fn draw_selected_param_card(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let block = styled_block(" Selected Parameter ", palette::CYAN).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(param) = app.selected_tune_param(hw) else {
        return;
    };
    let label = format_tune_param_label(param, hw);
    let live = format_live_param_display(app, param);
    let staged = app
        .staged
        .get(&param)
        .map(|v| format_tune_value(param, *v))
        .unwrap_or_else(|| "(unchanged)".to_string());
    let (fine, coarse) = App::param_steps(param);
    let base_bounds = param_base_bounds_summary(app, param, hw);
    let board_cap = app.board_caps.get(&param).copied();

    let mut bounds_spans = vec![
        Span::styled("Bounds:  ", Style::default().fg(palette::MUTED)),
        Span::styled(base_bounds, Style::default().fg(palette::FG)),
    ];
    if let Some(cap) = board_cap {
        bounds_spans.push(Span::styled(
            format!(" · {}", format_board_cap(param, cap)),
            Style::default()
                .fg(palette::ORANGE)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let mut lines = vec![
        Line::from(Span::styled(
            label,
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("Section: ", Style::default().fg(palette::MUTED)),
            Span::styled(param.section().title(), Style::default().fg(palette::TEAL)),
        ]),
        Line::from(vec![
            Span::styled("Live:    ", Style::default().fg(palette::MUTED)),
            Span::styled(
                live,
                Style::default()
                    .fg(palette::FG)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Staged:  ", Style::default().fg(palette::MUTED)),
            Span::styled(
                staged,
                Style::default().fg(if app.staged.contains_key(&param) {
                    palette::ORANGE
                } else {
                    palette::DIM
                }),
            ),
        ]),
        Line::from(bounds_spans),
        Line::from(vec![
            Span::styled("Step:    ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format!("±{fine} (Shift/H/L/PgUp/Dn: ±{coarse})"),
                Style::default().fg(palette::MUTED),
            ),
        ]),
    ];

    if let Some(sv) = app.staged.get(&param).copied() {
        if app.param_exceeds_soft_limit(param, sv, hw) && !app.allow_extreme {
            lines.push(Line::from(Span::styled(
                "! Exceeds soft limit — press 'x' for Allow Extreme",
                Style::default()
                    .fg(palette::YELLOW)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    }

    if let Some(cap) = board_cap {
        let unit = param_unit(param);
        let cap_label = if unit.is_empty() {
            format!("Board cap ≤{cap}")
        } else {
            format!("Board cap ≤{cap} {unit}")
        };
        lines.push(Line::from(Span::styled(
            format!("! {cap_label} (check BIOS PBO Limits)"),
            Style::default()
                .fg(palette::YELLOW)
                .add_modifier(Modifier::BOLD),
        )));
    } else if matches!(param.section(), TuneSection::Pbo | TuneSection::Apu) {
        lines.push(Line::from(Span::styled(
            crate::output::format_board_vrm_note(&app.board_info),
            Style::default().fg(palette::MUTED),
        )));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn draw_boot_persistence_card(frame: &mut Frame, area: Rect, app: &App) {
    let ps = &app.persist_status;
    let persist_accent = if ps.boot_guard_tripped {
        palette::RED
    } else if ps.service_enabled {
        palette::GREEN
    } else {
        palette::TEAL
    };
    let block = styled_block(" Boot Persistence ", persist_accent).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (profile_text, profile_color) = if !ps.active_profile_exists {
        (
            format!("missing ({})", ps.active_profile_path),
            palette::MUTED,
        )
    } else {
        match ps.active_profile_valid {
            Some(true) => {
                let cn = ps
                    .active_profile_codename
                    .map(|c| c.display_name())
                    .unwrap_or("valid");
                (format!("valid ({cn})"), palette::GREEN)
            }
            Some(false) => ("invalid (re-save with P)".to_string(), palette::RED),
            None => ("present".to_string(), palette::CYAN),
        }
    };

    let (svc_text, svc_color) = if ps.service_enabled {
        (format!("enabled ({})", ps.service_name), palette::GREEN)
    } else if !ps.service_installed && ps.service_state == "not-found" {
        (
            format!("not installed ({})", ps.service_name),
            palette::YELLOW,
        )
    } else {
        (format!("disabled ({})", ps.service_name), palette::MUTED)
    };

    let resume_span = if ps.resume_hook_installed {
        Span::styled("installed", Style::default().fg(palette::GREEN))
    } else {
        Span::styled("none", Style::default().fg(palette::MUTED))
    };
    let guard_span = if ps.boot_guard_tripped {
        Span::styled(
            "TRIPPED!",
            Style::default()
                .fg(palette::RED)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("clean", Style::default().fg(palette::GREEN))
    };

    let toggle_desc = if ps.service_enabled {
        "Disable boot service"
    } else {
        "Enable boot service"
    };

    let mut persist_lines = vec![
        Line::from(vec![
            Span::styled("Profile: ", Style::default().fg(palette::MUTED)),
            Span::styled(
                profile_text,
                Style::default()
                    .fg(profile_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Service: ", Style::default().fg(palette::MUTED)),
            Span::styled(
                svc_text,
                Style::default().fg(svc_color).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Resume:  ", Style::default().fg(palette::MUTED)),
            resume_span,
            Span::styled("  ·  Guard: ", Style::default().fg(palette::MUTED)),
            guard_span,
        ]),
    ];
    if inner.height > 5 {
        persist_lines.push(Line::from(""));
    }
    persist_lines.push(tune_key_line("P", "Persist live & enable service"));
    persist_lines.push(tune_key_line("E", toggle_desc));
    if ps.boot_guard_tripped {
        persist_lines.push(tune_key_line("C", "Clear boot guard"));
    }

    frame.render_widget(
        Paragraph::new(persist_lines).wrap(Wrap { trim: true }),
        inner,
    );
}

fn draw_safety_actions_card(frame: &mut Frame, area: Rect, app: &App) {
    let block =
        styled_block(" Safety & Quick Actions ", palette::PURPLE).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let extreme_span = if app.allow_extreme {
        badge_span(" ON (hard bounds) ", palette::ORANGE)
    } else {
        Span::styled(" OFF (soft bounds)", Style::default().fg(palette::GREEN))
    };

    let write_span = if app.can_write() {
        Span::styled(
            "READY (root)",
            Style::default()
                .fg(palette::GREEN)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("LOCKED (sudo)", Style::default().fg(palette::YELLOW))
    };

    let mut action_lines = vec![
        Line::from(vec![
            Span::styled("Writes:  ", Style::default().fg(palette::MUTED)),
            write_span,
            Span::styled(" · Ext:", Style::default().fg(palette::MUTED)),
            extreme_span,
        ]),
        Line::from(vec![
            Span::styled("Staged:  ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format!("{} pending change(s)", app.staged_count()),
                Style::default()
                    .fg(if app.staged_count() > 0 {
                        palette::ORANGE
                    } else {
                        palette::DIM
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        tune_key_line("a / Enter", "Dry-run preview & apply"),
        tune_key_line("0 / Bksp", "Reset param to 0 / live"),
        tune_key_line("u / x", "Undo all · Toggle Extreme"),
        tune_key_line("R / B / Z", "Rollback · Baseline · CO=0"),
        tune_key_line("s", "Save live TOML profile"),
    ];
    if inner.height >= 8 {
        action_lines.push(Line::from(Span::styled(
            crate::output::format_board_vrm_note(&app.board_info),
            Style::default().fg(palette::DIM),
        )));
    }

    frame.render_widget(
        Paragraph::new(action_lines).wrap(Wrap { trim: true }),
        inner,
    );
}

fn draw_tune_compact_bar(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let block = styled_block(" Inspector & Quick Actions ", palette::PURPLE)
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sel_info = if let Some(p) = app.selected_tune_param(hw) {
        format!(
            "{} · {}",
            format_tune_param_label(p, hw),
            param_bounds_summary(app, p, hw)
        )
    } else {
        "No parameter selected".to_string()
    };

    let mut key_spans = vec![
        Span::styled(
            "a/Ent",
            Style::default()
                .fg(palette::GREEN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(":DryRun ", Style::default().fg(palette::MUTED)),
        Span::styled(
            "u",
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(":Undo ", Style::default().fg(palette::MUTED)),
        Span::styled(
            "x",
            Style::default()
                .fg(palette::ORANGE)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if app.allow_extreme {
                ":Ext[ON] "
            } else {
                ":Ext "
            },
            Style::default().fg(if app.allow_extreme {
                palette::ORANGE
            } else {
                palette::MUTED
            }),
        ),
        Span::styled(
            "R/B/Z",
            Style::default()
                .fg(palette::YELLOW)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(":Rst ", Style::default().fg(palette::MUTED)),
        Span::styled(
            "P",
            Style::default()
                .fg(palette::GREEN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(":Persist ", Style::default().fg(palette::MUTED)),
        Span::styled(
            "E",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(":Svc", Style::default().fg(palette::MUTED)),
    ];
    if app.persist_status.boot_guard_tripped {
        key_spans.push(Span::raw(" "));
        key_spans.push(Span::styled(
            "C",
            Style::default()
                .fg(palette::RED)
                .add_modifier(Modifier::BOLD),
        ));
        key_spans.push(Span::styled(":ClrGuard", Style::default().fg(palette::RED)));
    }

    let mut lines = vec![Line::from(Span::styled(
        sel_info,
        Style::default().fg(palette::CYAN),
    ))];
    if let Some((ref msg, kind)) = app.last_alert {
        let col = match kind {
            FlashKind::Warn => palette::YELLOW,
            FlashKind::Error => palette::RED,
            FlashKind::Success => palette::GREEN,
            FlashKind::Info => palette::TEAL,
        };
        lines.push(Line::from(Span::styled(
            format!("Last Event: {msg}"),
            Style::default().fg(col).add_modifier(Modifier::BOLD),
        )));
    }
    lines.push(Line::from(key_spans));

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

fn tune_key_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{key:<10}"),
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(palette::FG)),
    ])
}

// ---------------------------------------------------------------------------
// Dry-Run Confirmation Modal Overlay
// ---------------------------------------------------------------------------

fn draw_confirm_modal(frame: &mut Frame, root: Rect, app: &App, modal: &ConfirmModal) {
    let pct_x = if root.width < 92 { 94 } else { 84 };
    let pct_y = if root.height < 28 { 86 } else { 76 };
    let popup = centered_rect(pct_x, pct_y, root);
    frame.render_widget(Clear, popup);

    let border_color = if !app.can_write() {
        palette::YELLOW
    } else if modal.allow_extreme {
        palette::ORANGE
    } else {
        palette::CYAN
    };

    let block = modal_frame_block(modal.title.as_str(), border_color);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 5 || inner.width < 20 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

    draw_confirm_modal_header(frame, chunks[0], app, modal);

    let visible_items = chunks[1].height.saturating_sub(1).max(1) as usize;
    let total_items = modal.item_count();
    let start = modal.scroll.min(total_items.saturating_sub(1));

    if !modal.steps.is_empty() {
        draw_confirm_modal_steps(frame, chunks[1], modal, start, visible_items);
    } else {
        draw_confirm_modal_ops_table(frame, chunks[1], modal, start, visible_items);
    }

    draw_confirm_modal_footer(frame, chunks[2], app, start, visible_items, total_items);
}

fn draw_confirm_modal_header(frame: &mut Frame, area: Rect, app: &App, modal: &ConfirmModal) {
    let mut sub_spans = vec![Span::styled(
        modal.subtitle.as_str(),
        Style::default().fg(palette::FG),
    )];
    if modal.allow_extreme {
        sub_spans.push(Span::raw(" "));
        sub_spans.push(badge_span(" EXTREME ", palette::ORANGE));
    }
    if !app.can_write() {
        sub_spans.push(Span::raw(" "));
        sub_spans.push(badge_span(" UNPRIVILEGED ", palette::YELLOW));
    }

    let safety_note = if modal.action.is_persist_action() {
        "Boot persistence & systemd control · validated before writing · no SMU registers modified"
    } else {
        "Pre-change snapshot saved before writes · flock + SMN RSP + readback verification"
    };
    let sub_lines = vec![
        Line::from(sub_spans),
        Line::from(Span::styled(safety_note, Style::default().fg(palette::DIM))),
    ];
    frame.render_widget(Paragraph::new(sub_lines), area);
}

fn draw_confirm_modal_steps(
    frame: &mut Frame,
    area: Rect,
    modal: &ConfirmModal,
    start: usize,
    visible_items: usize,
) {
    let mut step_lines: Vec<Line<'_>> = vec![Line::from(Span::styled(
        "Planned System Actions:",
        Style::default()
            .fg(palette::MUTED)
            .add_modifier(Modifier::BOLD),
    ))];
    for (step_label, detail) in modal.steps.iter().skip(start).take(visible_items) {
        step_lines.push(Line::from(vec![
            Span::styled(
                format!("  {step_label:<11} "),
                Style::default()
                    .fg(palette::CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(detail.as_str(), Style::default().fg(palette::FG)),
        ]));
    }
    frame.render_widget(Paragraph::new(step_lines).wrap(Wrap { trim: false }), area);
}

fn draw_confirm_modal_ops_table(
    frame: &mut Frame,
    area: Rect,
    modal: &ConfirmModal,
    start: usize,
    visible_items: usize,
) {
    let op_rows: Vec<Row<'_>> = modal
        .ops
        .iter()
        .enumerate()
        .skip(start)
        .take(visible_items)
        .map(|(idx, op)| {
            let cur = op.current.as_deref().unwrap_or("—");
            let change = format!("{cur} → {}", op.target);
            Row::new(vec![
                Cell::from(format!("{}", idx + 1)).style(Style::default().fg(palette::DIM)),
                Cell::from(op.mailbox.as_str()).style(Style::default().fg(palette::TEAL)),
                Cell::from(format!("0x{:02X}", op.opcode)).style(
                    Style::default()
                        .fg(palette::CYAN)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(format!("0x{:08X}", op.args[0]))
                    .style(Style::default().fg(palette::MUTED)),
                Cell::from(change).style(
                    Style::default()
                        .fg(palette::ORANGE)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(op.description.as_str()).style(Style::default().fg(palette::FG)),
            ])
        })
        .collect();

    let header = Row::new(vec![
        "#",
        "MB",
        "Opcode",
        "Arg0",
        "Current → Target",
        "Description",
    ])
    .style(
        Style::default()
            .fg(palette::MUTED)
            .add_modifier(Modifier::BOLD),
    );

    let widths = if area.width < 72 {
        [
            Constraint::Length(2),
            Constraint::Length(4),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(15),
            Constraint::Min(12),
        ]
    } else {
        [
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Length(7),
            Constraint::Length(11),
            Constraint::Length(22),
            Constraint::Min(20),
        ]
    };

    let table = Table::new(op_rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, area);
}

fn draw_confirm_modal_footer(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    start: usize,
    visible_items: usize,
    total_items: usize,
) {
    let scroll_hint = if total_items > visible_items {
        format!(
            "  [{}-{}/{} · ↑/↓ Scroll]",
            start + 1,
            (start + visible_items).min(total_items),
            total_items
        )
    } else {
        String::new()
    };

    let confirm_bg = if app.can_write() {
        palette::GREEN
    } else {
        palette::YELLOW
    };
    let actions = vec![
        Line::from(""),
        Line::from(vec![
            badge_span(" [y / Enter] Confirm & Apply ", confirm_bg),
            Span::raw("   "),
            Span::styled(
                " [n / Esc] Cancel ",
                Style::default()
                    .fg(palette::FG)
                    .bg(palette::ROW_HL)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(scroll_hint, Style::default().fg(palette::DIM)),
        ]),
    ];
    frame.render_widget(Paragraph::new(actions).alignment(Alignment::Center), area);
}

// ---------------------------------------------------------------------------
// Help Modal Overlay
// ---------------------------------------------------------------------------

fn draw_help_popup(frame: &mut Frame, root: Rect) {
    let pct_x = if root.width < 92 { 92 } else { 74 };
    let pct_y = if root.height < 28 { 90 } else { 82 };
    let popup = centered_rect(pct_x, pct_y, root);
    frame.render_widget(Clear, popup);

    let block = modal_frame_block(" zensmu top — Help & Keybindings ", palette::CYAN);

    let help_lines = vec![
        Line::from(Span::styled(
            "Navigation & Telemetry",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        )),
        help_entry(
            "1 / 2 / 3 / 4",
            "Switch to Overview, Clocks & Volts, CO & Shaper, or Tune",
        ),
        help_entry("t", "Jump straight to [4:Tune] from any tab"),
        help_entry("Tab / Shift+Tab", "Cycle forward / backward through views"),
        help_entry("p / Space", "Pause or resume live PM table polling"),
        help_entry("w", "Start or stop live telemetry recording (unprivileged)"),
        help_entry("W", "Cycle recording format (CSV / JSONL / PTS) when idle"),
        help_entry(
            "r",
            "Force refresh of PM telemetry and SMU mailbox readback",
        ),
        help_entry(
            "+ / -",
            "Increase or decrease polling interval (100ms..10000ms)",
        ),
        help_entry("? / Esc", "Toggle or close this help overlay"),
        help_entry("q / Ctrl+C", "Quit and cleanly restore terminal state"),
        Line::from(""),
        Line::from(Span::styled(
            "Interactive Tuning ([4:Tune] · Live Telemetry Always Active)",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        )),
        help_entry("↑↓ / j / k", "Move parameter selection up / down"),
        help_entry("[ / ]", "Jump to previous / next capability section"),
        help_entry("←→ / h / l", "Decrement / increment selected parameter"),
        help_entry(
            "H / L / Shift+←→",
            "Coarse step decrement / increment (or PgDn / PgUp)",
        ),
        help_entry(
            "0 / Backspace",
            "Reset selected param to 0 (CO/Shaper) or live value",
        ),
        help_entry("u", "Undo / discard all staged changes"),
        help_entry(
            "x",
            "Toggle Allow Extreme (hard bounds & 95°C Tctl always enforced)",
        ),
        help_entry(
            "a / Enter",
            "Run dry-run validation & open Confirmation Modal",
        ),
        help_entry(
            "R / B / Z",
            "Preview Rollback (last-state), Boot Baseline reset, or CO=0",
        ),
        help_entry("s", "Save live SMU state to TOML profile"),
        help_entry(
            "P",
            "Persist+Enable: save live state to /etc/zensmu/active.toml & enable service",
        ),
        help_entry(
            "E",
            "Toggle Service: enable or disable zensmu-apply.service",
        ),
        help_entry("C", "Clear tripped boot-loop crash guard sentinel"),
        Line::from(""),
        Line::from(Span::styled(
            "Safety & Privileges",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  PM table telemetry works unprivileged. Run with sudo to read/write SMU mailboxes.",
            Style::default().fg(palette::MUTED),
        )),
        Line::from(Span::styled(
            "  Every write previews exact opcodes first, snapshots state, and verifies readback.",
            Style::default().fg(palette::MUTED),
        )),
    ];

    frame.render_widget(
        Paragraph::new(help_lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}

fn help_entry<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {key:<18}"),
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(palette::FG)),
    ])
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pad_y = 100u16.saturating_sub(percent_y) / 2;
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(pad_y),
            Constraint::Percentage(percent_y.min(100)),
            Constraint::Percentage(pad_y),
        ])
        .split(area);

    let pad_x = 100u16.saturating_sub(percent_x) / 2;
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(pad_x),
            Constraint::Percentage(percent_x.min(100)),
            Constraint::Percentage(pad_x),
        ])
        .split(vertical[1])[1]
}

// ---------------------------------------------------------------------------
// Unit tests (no TTY required)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use zensmu_core::hw::profiles::capabilities_for;
    use zensmu_core::hw::topology::CpuTopology;
    use zensmu_core::transport::mock::MockTransport;
    use zensmu_core::types::Codename;

    fn mock_9700x() -> (MockTransport, HardwareContext) {
        let transport = MockTransport::granite_ridge_9700x();
        let driver = transport.read_info().unwrap();
        let hw = HardwareContext {
            driver,
            codename: Codename::GraniteRidge,
            cpuid: None,
            topology: CpuTopology::synthetic(1, 8, 2),
            capabilities: capabilities_for(Codename::GraniteRidge).unwrap(),
        };
        (transport, hw)
    }

    fn buffer_text(terminal: &Terminal<TestBackend>) -> String {
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    #[test]
    fn history_buffer_ring_eviction_and_stats() {
        let mut buf = HistoryBuffer::new(4);
        assert!(buf.is_empty());
        assert_eq!(buf.stats(), None);

        buf.push(10.0);
        buf.push(20.0);
        buf.push(f64::NAN); // ignored
        buf.push(f64::INFINITY); // ignored
        buf.push(30.0);
        buf.push(40.0);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.latest(), Some(40.0));
        assert_eq!(buf.min(), Some(10.0));
        assert_eq!(buf.max(), Some(40.0));
        assert_eq!(buf.avg(), Some(25.0));

        // Pushing a 5th value evicts 10.0
        buf.push(50.0);
        assert_eq!(buf.len(), 4);
        assert_eq!(buf.min(), Some(20.0));
        assert_eq!(buf.max(), Some(50.0));
        assert_eq!(buf.avg(), Some(35.0));

        // Window slicing returns the MOST RECENT samples
        assert_eq!(buf.sparkline_window(2, 10.0), vec![400, 500]);
        assert_eq!(buf.sparkline_window(10, 1.0), vec![20, 30, 40, 50]);
    }

    #[test]
    fn gauge_ratio_percent_and_color_thresholds() {
        assert!((gauge_ratio(Some(44.0), Some(88.0)) - 0.5).abs() < 1e-6);
        assert_eq!(gauge_percent(Some(44.0), Some(88.0)), Some(50));
        assert_eq!(gauge_ratio(Some(120.0), Some(88.0)), 1.0);
        assert_eq!(gauge_ratio(None, Some(88.0)), 0.0);
        assert_eq!(gauge_ratio(Some(44.0), Some(0.0)), 0.0);
        assert_eq!(gauge_percent(Some(44.0), None), None);

        assert_eq!(ratio_color(0.40), palette::GREEN);
        assert_eq!(ratio_color(0.75), palette::YELLOW);
        assert_eq!(ratio_color(0.92), palette::RED);

        assert_eq!(thermal_color(Some(50.0), Some(95.0)), palette::GREEN);
        assert_eq!(thermal_color(Some(68.0), Some(95.0)), palette::YELLOW);
        assert_eq!(thermal_color(Some(78.0), Some(95.0)), palette::ORANGE);
        assert_eq!(thermal_color(Some(88.0), Some(95.0)), palette::RED);
    }

    #[test]
    fn formatting_and_interval_helpers() {
        assert_eq!(clamp_interval_ms(10), MIN_INTERVAL_MS);
        assert_eq!(clamp_interval_ms(50_000), MAX_INTERVAL_MS);
        assert_eq!(adjust_interval_ms(500, true), 600);
        assert_eq!(adjust_interval_ms(500, false), 400);
        assert_eq!(adjust_interval_ms(100, false), 100);
        assert_eq!(adjust_interval_ms(1000, true), 1250);
        assert_eq!(adjust_interval_ms(2000, true), 2500);

        assert_eq!(format_signed_margin(-20), "-20");
        assert_eq!(format_signed_margin(0), "0");
        assert_eq!(format_signed_margin(5), "+5");

        assert_eq!(format_opt_f32(Some(1.24), 3, "V"), "1.240 V");
        assert_eq!(format_opt_f32(None, 3, "V"), "—");
        assert_eq!(format_opt_u32(Some(5550), "MHz"), "5550 MHz");
        assert_eq!(format_opt_u32(None, "MHz"), "—");

        assert_eq!(
            format_gauge_readout(Some(42.5), Some(88.0), "W"),
            " 42.5 /  88.0 W ( 48%)"
        );
        assert_eq!(
            format_clock_ratio(Some(3000.0), Some(3000.0)),
            Some("1:1 sync")
        );
        assert_eq!(
            format_clock_ratio(Some(1500.0), Some(3000.0)),
            Some("1:2 div")
        );
        assert_eq!(format_linux_cpus(&[0, 8]), "0,8");
        assert_eq!(format_linux_cpus(&[]), "—");

        let bar_neg = format_co_bar(-25, 50, 4);
        assert_eq!(bar_neg, "[  ██│    ]");
        let bar_pos = format_co_bar(50, 50, 4);
        assert_eq!(bar_pos, "[    │████]");
        let bar_zero = format_co_bar(0, 50, 4);
        assert_eq!(bar_zero, "[    │    ]");
    }

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
    fn tune_stage_dry_run_apply_and_rollback_co_and_pbo() {
        let (transport, hw) = mock_9700x();
        let tmp = tempfile::tempdir().unwrap();

        let mut app = App::new(500);
        app.test_allow_writes = true;
        app.snapshot_store = SnapshotStore::new(tmp.path().join("state"));
        app.poll_telemetry(&transport, &hw);
        app.refresh_mailbox(&transport, &hw);

        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);

        // Jump straight to Tune tab via 't'
        assert!(!app.handle_key(press(KeyCode::Char('t')), &transport, &hw));
        assert_eq!(app.active_tab, ViewTab::Tune);
        assert_eq!(app.selected_tune_param(&hw), Some(TuneParam::CoAll));

        // Stage CO All-Core = -20 via 4 coarse decrements ('H' = -5 each)
        for _ in 0..4 {
            app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
        }
        assert_eq!(
            app.staged.get(&TuneParam::CoAll),
            Some(&TuneValue::Int(-20))
        );

        // Jump sections twice: CurveOptimizer -> CurveShaper -> PBO
        app.handle_key(press(KeyCode::Char(']')), &transport, &hw);
        assert_eq!(
            app.selected_tune_param(&hw).map(TuneParam::section),
            Some(TuneSection::CurveShaper)
        );
        app.handle_key(press(KeyCode::Char(']')), &transport, &hw);
        assert_eq!(app.selected_tune_param(&hw), Some(TuneParam::PboPpt));

        // Live PPT on mock 9700X is 88 W; step +25 W twice -> 138 W
        app.handle_key(press(KeyCode::Char('L')), &transport, &hw);
        app.handle_key(press(KeyCode::Char('L')), &transport, &hw);
        assert_eq!(
            app.staged.get(&TuneParam::PboPpt),
            Some(&TuneValue::Int(138))
        );
        assert_eq!(app.staged_count(), 2);

        // Press 'a' to open Dry-Run Confirmation Modal (must NOT write to hardware yet)
        app.handle_key(press(KeyCode::Char('a')), &transport, &hw);
        let modal = app
            .confirm_modal
            .as_ref()
            .expect("confirm modal should open");
        assert_eq!(modal.ops.len(), 2);
        assert_eq!(modal.ops[0].target, "-20");
        assert_eq!(modal.ops[0].current.as_deref(), Some("0"));
        assert_eq!(modal.ops[1].target, "138 W");
        assert_eq!(modal.ops[1].current.as_deref(), Some("88 W"));

        // Verify hardware is still untouched before confirmation
        assert_eq!(co::get_co(&transport, &hw).unwrap().cores[0].margin, 0);
        assert_eq!(
            pm_table::read_telemetry(&transport, &hw)
                .unwrap()
                .ppt_limit_w,
            Some(88.0)
        );

        // Press 'y' to Confirm & Apply
        app.handle_key(press(KeyCode::Char('y')), &transport, &hw);
        assert!(app.confirm_modal.is_none());
        assert!(app.staged.is_empty());
        let (flash_msg, flash_kind) = app.active_flash().expect("success flash");
        assert_eq!(flash_kind, FlashKind::Success);
        assert!(flash_msg.contains("Applied 2 SMU operation(s) & verified"));

        // Verify real hardware state in MockTransport updated and verified!
        let co_after = co::get_co(&transport, &hw).unwrap();
        assert!(co_after.cores.iter().all(|c| c.margin == -20));
        let telem_after = pm_table::read_telemetry(&transport, &hw).unwrap();
        assert_eq!(telem_after.ppt_limit_w, Some(138.0));

        // Snapshots were saved before mutation
        assert!(app.snapshot_store.load_boot_baseline().unwrap().is_some());
        assert!(app.snapshot_store.load_last_state().unwrap().is_some());

        // Press 'R' (uppercase) to preview Rollback, then confirm with Enter
        app.handle_key(press(KeyCode::Char('R')), &transport, &hw);
        assert!(app.confirm_modal.is_some());
        app.handle_key(press(KeyCode::Enter), &transport, &hw);
        assert!(app.confirm_modal.is_none());

        // Hardware rolled back cleanly to 0 CO and 88 W PPT
        assert!(co::get_co(&transport, &hw)
            .unwrap()
            .cores
            .iter()
            .all(|c| c.margin == 0));
        assert_eq!(
            pm_table::read_telemetry(&transport, &hw)
                .unwrap()
                .ppt_limit_w,
            Some(88.0)
        );
    }

    #[test]
    fn tune_refuses_unprivileged_write_and_guards_extreme_values() {
        let (transport, hw) = mock_9700x();
        let mut app = App::new(500);
        app.is_root = false;
        app.test_allow_writes = false;
        app.poll_telemetry(&transport, &hw);
        app.refresh_mailbox(&transport, &hw);

        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.handle_key(press(KeyCode::Char('t')), &transport, &hw);

        // Stage CO = -10, open modal, attempt confirm while unprivileged -> must refuse
        app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
        app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
        app.handle_key(press(KeyCode::Enter), &transport, &hw);
        assert!(app.confirm_modal.is_some());
        app.handle_key(press(KeyCode::Char('y')), &transport, &hw);
        assert!(app.confirm_modal.is_none());
        let (msg, kind) = app.active_flash().unwrap();
        assert_eq!(kind, FlashKind::Error);
        assert!(msg.contains("root required"));
        assert_eq!(co::get_co(&transport, &hw).unwrap().cores[0].margin, 0);

        // Now test extreme guard: CO = -40 (below soft_min -35) blocked when allow_extreme = false
        app.test_allow_writes = true;
        for _ in 0..6 {
            app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
        }
        assert_eq!(
            app.staged.get(&TuneParam::CoAll),
            Some(&TuneValue::Int(-40))
        );
        app.handle_key(press(KeyCode::Char('a')), &transport, &hw);
        assert!(app.confirm_modal.is_none());
        let (msg, kind) = app.active_flash().unwrap();
        assert_eq!(kind, FlashKind::Error);
        assert!(msg.contains("soft limit"));

        // Toggle Allow Extreme ('x'), now dry-run preview succeeds
        app.handle_key(press(KeyCode::Char('x')), &transport, &hw);
        assert!(app.allow_extreme);
        app.handle_key(press(KeyCode::Char('a')), &transport, &hw);
        assert!(app.confirm_modal.is_some());

        // Cancel with Esc, then 'u' discards all staged changes
        app.handle_key(press(KeyCode::Esc), &transport, &hw);
        assert!(app.confirm_modal.is_none());
        app.handle_key(press(KeyCode::Char('u')), &transport, &hw);
        assert!(app.staged.is_empty());
    }

    #[test]
    fn renders_tune_tab_and_confirm_modal_without_tty() {
        let (transport, hw) = mock_9700x();
        let mut app = App::new(500);
        app.test_allow_writes = true;
        app.poll_telemetry(&transport, &hw);
        app.refresh_mailbox(&transport, &hw);

        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.handle_key(press(KeyCode::Char('4')), &transport, &hw);
        app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
        app.handle_key(press(KeyCode::Char(']')), &transport, &hw);
        app.handle_key(press(KeyCode::Char(']')), &transport, &hw);
        app.handle_key(press(KeyCode::Char('L')), &transport, &hw);

        for (w, h) in [(120u16, 36u16), (80, 24)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();

            // Render Tab 4 (Tune) with staged changes
            app.confirm_modal = None;
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
            let text = buffer_text(&terminal);
            assert!(text.contains("4:Tune"), "expected 4:Tune tab at {w}x{h}");
            assert!(text.contains("*2"), "expected *2 staged badge at {w}x{h}");
            assert!(
                text.contains("Live Telemetry"),
                "expected Live Telemetry strip at {w}x{h}"
            );
            assert!(text.contains("PPT"), "expected PPT telemetry at {w}x{h}");

            // Open Dry-Run Confirmation Modal and render cleanly
            app.handle_key(press(KeyCode::Char('a')), &transport, &hw);
            assert!(app.confirm_modal.is_some());
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
            let modal_text = buffer_text(&terminal);
            assert!(
                modal_text.contains("Dry-Run Preview"),
                "expected modal title at {w}x{h}"
            );
            assert!(
                modal_text.contains("Confirm & Apply"),
                "expected confirm button at {w}x{h}"
            );
            assert!(modal_text.contains("0x"), "expected hex opcode at {w}x{h}");
            assert!(
                modal_text.contains("→"),
                "expected current→target arrow at {w}x{h}"
            );
        }
    }

    #[test]
    fn tune_hides_curve_shaper_when_firmware_unsupported() {
        let (transport, hw) = mock_9700x();
        let mut app = App::new(500);
        app.poll_telemetry(&transport, &hw);
        app.refresh_mailbox(&transport, &hw);
        app.shaper_state = MailboxState::FirmwareUnsupported("0xFE".to_string());
        app.active_tab = ViewTab::Tune;

        // No Shaper params should be exposed as interactive controls
        let params = app.tune_params(&hw);
        assert!(
            !params.iter().any(|p| matches!(p, TuneParam::Shaper { .. })),
            "Curve Shaper controls must be hidden when FirmwareUnsupported"
        );

        // Navigate down near the Curve Shaper banner so it's scrolled into view, and render
        app.tune_selected = params
            .iter()
            .position(|p| matches!(p, TuneParam::PboPpt))
            .unwrap_or(0);

        for (w, h) in [(120u16, 36u16), (80, 24)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
            let text = buffer_text(&terminal);
            assert!(
                text.contains("Not exposed by SMU") && text.contains("0xFE"),
                "expected FirmwareUnsupported banner at {w}x{h}, got:\n{text}"
            );
        }
    }

    #[test]
    fn tune_exposes_apu_dptc_controls_on_apu() {
        let transport = MockTransport::granite_ridge_9700x();
        for codename in [Codename::Lucienne, Codename::Phoenix] {
            let mut driver = transport.read_info().unwrap();
            driver.codename_id = codename as u32;
            let hw = HardwareContext {
                driver,
                codename,
                cpuid: None,
                topology: CpuTopology::synthetic(1, 8, 2),
                capabilities: capabilities_for(codename).unwrap(),
            };

            let mut app = App::new(500);
            app.active_tab = ViewTab::Tune;
            let params = app.tune_params(&hw);
            for expected in [
                TuneParam::ApuStapm,
                TuneParam::ApuStapmTime,
                TuneParam::ApuFast,
                TuneParam::ApuSlow,
                TuneParam::ApuSlowTime,
                TuneParam::ApuApuSlow,
                TuneParam::ApuTdcSoc,
                TuneParam::ApuEdcSoc,
                TuneParam::ApuSkinTemp,
                TuneParam::ApuDgpuSkinTemp,
                TuneParam::ApuPowerMode,
            ] {
                assert!(
                    params.contains(&expected),
                    "missing APU control {expected:?} on {codename:?}"
                );
            }

            // Populate live APU telemetry and verify live_param_value reads all APU limits
            app.telemetry = Some(TelemetrySnapshot {
                codename,
                table_version: if codename == Codename::Lucienne {
                    0x0037_0005
                } else {
                    0x004C_0006
                },
                table_size: 0x8C8,
                ppt_limit_w: Some(30.0),
                ppt_value_w: Some(18.0),
                tdc_limit_a: Some(35.0),
                tdc_value_a: Some(12.0),
                edc_limit_a: Some(50.0),
                edc_value_a: Some(20.0),
                tctl_limit_c: Some(95.0),
                tctl_value_c: Some(61.0),
                stapm_limit_w: Some(25.0),
                stapm_value_w: Some(15.0),
                slow_limit_w: Some(28.0),
                slow_value_w: Some(16.0),
                apu_slow_limit_w: Some(22.0),
                apu_slow_value_w: Some(14.0),
                tdc_soc_limit_a: Some(20.0),
                tdc_soc_value_a: Some(8.5),
                edc_soc_limit_a: Some(35.0),
                edc_soc_value_a: Some(14.0),
                apu_skin_temp_limit_c: Some(45.0),
                apu_skin_temp_value_c: Some(39.0),
                dgpu_skin_temp_limit_c: Some(55.0),
                dgpu_skin_temp_value_c: Some(42.0),
                fclk_mhz: Some(1600.0),
                uclk_mhz: Some(1600.0),
                mclk_mhz: Some(1600.0),
                vddcr_soc_v: Some(0.95),
                vddcr_misc_v: None,
                cldo_vddp_v: None,
                cldo_vddg_iod_v: None,
                cldo_vddg_ccd_v: None,
            });

            assert_eq!(
                app.live_param_value(TuneParam::ApuStapm),
                Some(TuneValue::Int(25))
            );
            assert_eq!(app.live_param_value(TuneParam::ApuStapmTime), None);
            assert_eq!(
                app.live_param_value(TuneParam::ApuFast),
                Some(TuneValue::Int(30))
            );
            assert_eq!(
                app.live_param_value(TuneParam::ApuSlow),
                Some(TuneValue::Int(28))
            );
            assert_eq!(app.live_param_value(TuneParam::ApuSlowTime), None);
            assert_eq!(
                app.live_param_value(TuneParam::ApuApuSlow),
                Some(TuneValue::Int(22))
            );
            assert_eq!(
                app.live_param_value(TuneParam::ApuTdcSoc),
                Some(TuneValue::Int(20))
            );
            assert_eq!(
                app.live_param_value(TuneParam::ApuEdcSoc),
                Some(TuneValue::Int(35))
            );
            assert_eq!(
                app.live_param_value(TuneParam::ApuSkinTemp),
                Some(TuneValue::Int(45))
            );
            assert_eq!(
                app.live_param_value(TuneParam::ApuDgpuSkinTemp),
                Some(TuneValue::Int(55))
            );
            assert_eq!(
                app.live_param_value(TuneParam::ApuPowerMode),
                Some(TuneValue::PowerMode(None))
            );

            // Stage EVERY APU parameter and open dry-run preview modal
            app.staged.insert(TuneParam::ApuStapm, TuneValue::Int(35));
            app.staged
                .insert(TuneParam::ApuStapmTime, TuneValue::Int(120));
            app.staged.insert(TuneParam::ApuFast, TuneValue::Int(45));
            app.staged.insert(TuneParam::ApuSlow, TuneValue::Int(38));
            app.staged
                .insert(TuneParam::ApuSlowTime, TuneValue::Int(10));
            app.staged.insert(TuneParam::ApuApuSlow, TuneValue::Int(30));
            app.staged.insert(TuneParam::ApuTdcSoc, TuneValue::Int(25));
            app.staged.insert(TuneParam::ApuEdcSoc, TuneValue::Int(45));
            app.staged
                .insert(TuneParam::ApuSkinTemp, TuneValue::Int(50));
            app.staged
                .insert(TuneParam::ApuDgpuSkinTemp, TuneValue::Int(60));
            app.staged.insert(
                TuneParam::ApuPowerMode,
                TuneValue::PowerMode(Some(ApuPowerMode::MaxPerformance)),
            );

            app.open_staged_dry_run_modal(&transport, &hw);
            let modal = app
                .confirm_modal
                .as_ref()
                .expect("dry-run modal should open for all 11 APU params");
            assert_eq!(
                modal.ops.len(),
                11,
                "expected 11 planned APU ops on {codename:?}"
            );
            match &modal.action {
                ConfirmAction::ApplyStaged { apu: Some(req), .. } => {
                    assert_eq!(
                        req,
                        &ApuSetRequest {
                            stapm_w: Some(35),
                            stapm_time_s: Some(120),
                            fast_w: Some(45),
                            slow_w: Some(38),
                            slow_time_s: Some(10),
                            apu_slow_w: Some(30),
                            tdc_soc_a: Some(25),
                            edc_soc_a: Some(45),
                            apu_skin_temp_c: Some(50),
                            dgpu_skin_temp_c: Some(60),
                            power_mode: Some(ApuPowerMode::MaxPerformance),
                        }
                    );
                }
                other => panic!("expected ApplyStaged with ApuSetRequest, got {other:?}"),
            }
            // Verify live current enrichment on dry-run ops
            assert_eq!(modal.ops[0].current.as_deref(), Some("25 W"));
            assert_eq!(modal.ops[6].current.as_deref(), Some("20 A"));
            assert_eq!(modal.ops[7].current.as_deref(), Some("35 A"));
            assert_eq!(modal.ops[8].current.as_deref(), Some("45 C"));
            assert_eq!(modal.ops[9].current.as_deref(), Some("55 C"));

            // Render Overview, ClocksVolts, and Tune tabs with APU telemetry at both sizes
            app.confirm_modal = None;
            app.tune_selected = params
                .iter()
                .position(|p| *p == TuneParam::ApuStapm)
                .unwrap();
            for (w, h) in [(120u16, 36u16), (80, 24)] {
                let backend = TestBackend::new(w, h);
                let mut terminal = Terminal::new(backend).unwrap();

                app.active_tab = ViewTab::Overview;
                terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
                let overview_text = buffer_text(&terminal);
                assert!(
                    overview_text.contains("STAPM"),
                    "Overview should show STAPM on {codename:?} at {w}x{h}"
                );
                assert!(
                    overview_text.contains("SoCTD"),
                    "Overview should show SoC TDC on {codename:?} at {w}x{h}"
                );
                assert!(
                    overview_text.contains("ASkin"),
                    "Overview should show APU Skin on {codename:?} at {w}x{h}"
                );

                app.active_tab = ViewTab::ClocksVolts;
                terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();

                app.active_tab = ViewTab::Tune;
                terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
                let tune_text = buffer_text(&terminal);
                assert!(
                    tune_text.contains("STAPM"),
                    "Tune telemetry strip should show STAPM on {codename:?} at {w}x{h}"
                );
            }
        }
    }

    #[test]
    fn tune_persist_and_service_modal_transitions_and_rendering() {
        let (transport, hw) = mock_9700x();
        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(500);
        app.persist_paths = PersistPaths {
            active_profile: tmp.path().join("etc/zensmu/active.toml"),
            modules_load: tmp.path().join("etc/modules-load.d/ryzen_smu.conf"),
            guard_path: tmp.path().join("var/lib/zensmu/applying.lock"),
            resume_hook: tmp
                .path()
                .join("usr/lib/systemd/system-sleep/zensmu-resume"),
            service_name: "zensmu-apply.service".to_string(),
        };
        app.persist_status = default_persist_status(&app.persist_paths);
        app.poll_telemetry(&transport, &hw);
        app.refresh_mailbox(&transport, &hw);

        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.handle_key(press(KeyCode::Char('4')), &transport, &hw);
        assert_eq!(app.active_tab, ViewTab::Tune);

        // 1. Pressing 'P' while staged changes exist must refuse with warning flash and NOT open modal
        app.handle_key(press(KeyCode::Char('H')), &transport, &hw);
        assert_eq!(app.staged_count(), 1);
        app.handle_key(press(KeyCode::Char('P')), &transport, &hw);
        assert!(app.confirm_modal.is_none());
        let (msg, kind) = app.active_flash().expect("warning flash");
        assert_eq!(kind, FlashKind::Warn);
        assert!(msg.contains("Apply (a) or discard (u) staged changes"));

        // Discard staged changes via 'u'
        app.handle_key(press(KeyCode::Char('u')), &transport, &hw);
        assert!(app.staged.is_empty());

        // 2. Pressing 'P' with clean staged state opens PersistLiveAndEnable confirmation modal
        app.handle_key(press(KeyCode::Char('P')), &transport, &hw);
        {
            let modal = app
                .confirm_modal
                .as_ref()
                .expect("persist modal should open");
            assert!(matches!(modal.action, ConfirmAction::PersistLiveAndEnable));
            assert_eq!(modal.steps.len(), 3);
            assert!(modal.steps[0].1.contains("active.toml"));
            assert!(modal.steps[1].1.contains("ryzen_smu"));
            assert!(modal.steps[2].1.contains("zensmu-apply.service"));
        }

        // Render modal at 120x36 and 80x24
        for (w, h) in [(120u16, 36u16), (80, 24)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
            let text = buffer_text(&terminal);
            assert!(
                text.contains("Persist Live State"),
                "expected persist modal title at {w}x{h}"
            );
            assert!(
                text.contains("ryzen_smu"),
                "expected modules-load step at {w}x{h}"
            );
        }

        // Confirming 'P' while unprivileged refuses with helpful sudo error
        app.is_root = false;
        app.test_allow_writes = false;
        app.handle_key(press(KeyCode::Char('y')), &transport, &hw);
        assert!(app.confirm_modal.is_none());
        let (msg, kind) = app.active_flash().expect("unprivileged error flash");
        assert_eq!(kind, FlashKind::Error);
        assert!(msg.contains("root required"));

        // 3. Pressing 'E' when disabled and active.toml missing states it will save live state first + enable
        app.persist_status.service_enabled = false;
        app.persist_status.active_profile_exists = false;
        app.handle_key(press(KeyCode::Char('E')), &transport, &hw);
        {
            let modal = app
                .confirm_modal
                .as_ref()
                .expect("enable modal should open");
            assert!(matches!(
                modal.action,
                ConfirmAction::ToggleBootService { enable: true }
            ));
            assert!(modal.title.contains("Save Live State"));
            assert!(modal.subtitle.contains("save live state first"));
        }
        app.handle_key(press(KeyCode::Esc), &transport, &hw);
        assert!(app.confirm_modal.is_none());

        // 4. Pressing 'E' when disabled and active.toml valid opens Enable modal using existing profile
        app.persist_status.active_profile_exists = true;
        app.persist_status.active_profile_valid = Some(true);
        app.persist_status.active_profile_codename = Some(Codename::GraniteRidge);
        app.handle_key(press(KeyCode::Char('E')), &transport, &hw);
        {
            let modal = app
                .confirm_modal
                .as_ref()
                .expect("enable modal should open");
            assert!(matches!(
                modal.action,
                ConfirmAction::ToggleBootService { enable: true }
            ));
            assert!(modal.subtitle.contains("using existing"));
        }
        app.handle_key(press(KeyCode::Char('n')), &transport, &hw);
        assert!(app.confirm_modal.is_none());

        // 5. Pressing 'E' when enabled opens Disable modal
        app.persist_status.service_enabled = true;
        app.handle_key(press(KeyCode::Char('E')), &transport, &hw);
        {
            let modal = app
                .confirm_modal
                .as_ref()
                .expect("disable modal should open");
            assert!(matches!(
                modal.action,
                ConfirmAction::ToggleBootService { enable: false }
            ));
            assert!(modal.title.contains("Disable"));
        }
        app.handle_key(press(KeyCode::Esc), &transport, &hw);
        assert!(app.confirm_modal.is_none());

        // 6. Test Boot Persistence badges & inspector block rendering on Tab 4 across all 3 states
        for (enabled, exists, expected_badge) in [
            (true, true, "[BOOT:ON]"),
            (false, true, "[BOOT:SAVED]"),
            (false, false, "[BOOT:OFF]"),
        ] {
            app.persist_status.service_enabled = enabled;
            app.persist_status.active_profile_exists = exists;
            let backend = TestBackend::new(120, 36);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
            let text = buffer_text(&terminal);
            assert!(
                text.contains(expected_badge),
                "expected badge {expected_badge} on Tab 4, got:\n{text}"
            );
            assert!(
                text.contains("Boot Persistence"),
                "expected Boot Persistence inspector block on Tab 4"
            );
            assert!(
                text.contains("Persist+Enable"),
                "expected Persist+Enable in Tab 4 footer"
            );
            assert!(
                text.contains("Toggle Service"),
                "expected Toggle Service in Tab 4 footer"
            );
        }

        // 7. Test 'C' clears tripped boot guard after confirmation
        let guard = BootLoopGuard::new(app.persist_paths.guard_path.clone());
        let _armed = guard.check_and_arm("active.toml").unwrap();
        assert!(guard.is_tripped());
        app.persist_status.boot_guard_tripped = true;
        app.test_allow_writes = true;

        app.handle_key(press(KeyCode::Char('C')), &transport, &hw);
        assert!(matches!(
            app.confirm_modal.as_ref().map(|m| &m.action),
            Some(ConfirmAction::ClearBootGuard)
        ));
        app.handle_key(press(KeyCode::Enter), &transport, &hw);
        assert!(app.confirm_modal.is_none());
        assert!(!guard.is_tripped());
        assert!(!app.persist_status.boot_guard_tripped);
        let (msg, kind) = app.active_flash().expect("clear guard success flash");
        assert_eq!(kind, FlashKind::Success);
        assert!(msg.contains("Cleared boot-loop"));
    }

    #[test]
    fn tune_hardware_clamped_records_board_caps_and_renders_untruncated_warning() {
        let (transport, hw) = mock_9700x();
        transport.set_clamp_tdc_a(Some(100));
        transport.set_clamp_edc_a(Some(120));
        transport.set_sustained_power_thm_raw(0x0069_005A); // 105 W TDP, 90 °C TjMax

        let tmp = tempfile::tempdir().unwrap();
        let mut app = App::new(500);
        app.test_allow_writes = true;
        app.snapshot_store = SnapshotStore::new(tmp.path().join("state"));
        app.board_info = SystemBoardInfo {
            board_vendor: Some("Gigabyte Technology Co., Ltd.".into()),
            board_name: Some("B450M DS3H V2".into()),
            bios_version: Some("F66".into()),
            ..Default::default()
        };
        app.poll_telemetry(&transport, &hw);
        app.refresh_mailbox(&transport, &hw);

        // Verify proactive board/BIOS VRM note is shown on PBO param before any clamp occurs
        app.active_tab = ViewTab::Tune;
        let params = app.tune_params(&hw);
        app.tune_selected = params.iter().position(|p| *p == TuneParam::PboPpt).unwrap();
        {
            let backend = TestBackend::new(120, 36);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
            let pre_text = buffer_text(&terminal);
            assert!(
                pre_text.contains("B450M")
                    && pre_text.contains("DS3H V2")
                    && pre_text.contains("BIOS F66")
                    && pre_text.contains("BIOS PBO Limits")
                    && pre_text.contains("PPT/TDC/EDC"),
                "expected proactive board/BIOS VRM note before clamp:\n{pre_text}"
            );
            assert!(
                pre_text.contains("TDP 105W") && pre_text.contains("TjMax 90°C"),
                "expected fused TDP & TjMax in Tune PBO section header:\n{pre_text}"
            );
        }

        // Verify Overview and CO/Shaper tabs show TDP & TjMax
        {
            let backend = TestBackend::new(120, 36);
            let mut terminal = Terminal::new(backend).unwrap();
            app.active_tab = ViewTab::Overview;
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
            let over_text = buffer_text(&terminal);
            assert!(
                over_text.contains("105W") && over_text.contains("90°C"),
                "expected TDP/TjMax in Overview PBO summary:\n{over_text}"
            );

            app.active_tab = ViewTab::CoShaper;
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
            let co_text = buffer_text(&terminal);
            assert!(
                co_text.contains("Fused TDP / TjMax") && co_text.contains("105 W / 90 °C"),
                "expected Fused TDP / TjMax in CO/Shaper fused PBO table:\n{co_text}"
            );
        }

        // Stage PBO PPT=142W (not clamped), TDC=140A (clamped to 100A), EDC=220A (clamped to 120A)
        app.active_tab = ViewTab::Tune;
        app.allow_extreme = true;
        app.staged.insert(TuneParam::PboPpt, TuneValue::Int(142));
        app.staged.insert(TuneParam::PboTdc, TuneValue::Int(140));
        app.staged.insert(TuneParam::PboEdc, TuneValue::Int(220));

        let press = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.handle_key(press(KeyCode::Char('a')), &transport, &hw);
        assert!(app.confirm_modal.is_some());
        app.handle_key(press(KeyCode::Char('y')), &transport, &hw);

        // Modal closed, staged cleared, board_caps recorded for both clamped params
        assert!(app.confirm_modal.is_none());
        assert!(app.staged.is_empty());
        assert_eq!(app.board_caps.get(&TuneParam::PboPpt), None);
        assert_eq!(app.board_caps.get(&TuneParam::PboTdc), Some(&100));
        assert_eq!(app.board_caps.get(&TuneParam::PboEdc), Some(&120));

        let expected_msg = "Board/BIOS cap hit: pbo.tdc_a (req 140 A -> cap 100 A), pbo.edc_a (req 220 A -> cap 120 A) (check BIOS PBO Limits)";
        let (flash_msg, flash_kind) = app.active_flash().expect("board-cap warning flash");
        assert_eq!(flash_kind, FlashKind::Warn);
        assert_eq!(flash_msg, expected_msg);
        assert_eq!(
            app.last_alert.as_ref(),
            Some(&(expected_msg.to_string(), FlashKind::Warn))
        );

        // param_bounds_summary includes learned board cap
        let edc_bounds = param_bounds_summary(&app, TuneParam::PboEdc, &hw);
        assert!(
            edc_bounds.contains("board cap ≤120 A"),
            "expected board cap in bounds summary, got: {edc_bounds}"
        );

        // Select PboEdc and render at both 120x36 and 80x24: verify learned cap + full untruncated warning
        app.tune_selected = params.iter().position(|p| *p == TuneParam::PboEdc).unwrap();
        for (w, h) in [(120u16, 36u16), (80, 24)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
            let text = buffer_text(&terminal);
            assert!(
                text.contains("board cap ≤120 A"),
                "expected learned board cap hint at {w}x{h}:\n{text}"
            );
            assert!(
                text.contains("Board/BIOS cap hit:")
                    && text.contains("pbo.tdc_a")
                    && text.contains("100 A")
                    && text.contains("pbo.edc_a")
                    && text.contains("120 A")
                    && text.contains("BIOS PBO Limits"),
                "expected full board-cap warning without truncation at {w}x{h}:\n{text}"
            );
        }
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
}
