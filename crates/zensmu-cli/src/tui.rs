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
use std::collections::{HashMap, VecDeque};
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
use zensmu_core::ops::pbo::{self, PboSetRequest, PboStatus};
use zensmu_core::ops::pm_table::{self, TelemetrySnapshot};
use zensmu_core::ops::shaper;
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
        self.samples
            .iter()
            .copied()
            .reduce(f64::min)
    }

    pub(crate) fn max(&self) -> Option<f64> {
        self.samples
            .iter()
            .copied()
            .reduce(f64::max)
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
    ApuFast,
    ApuSlow,
    ApuApuSlow,
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
            | Self::ApuFast
            | Self::ApuSlow
            | Self::ApuApuSlow
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
}

#[derive(Debug, Clone)]
struct ConfirmModal {
    title: String,
    subtitle: String,
    action: ConfirmAction,
    ops: Vec<PlannedOp>,
    scroll: usize,
    allow_extreme: bool,
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
    // Interactive Tune state (Tab 4)
    tune_selected: usize,
    staged: HashMap<TuneParam, TuneValue>,
    allow_extreme: bool,
    confirm_modal: Option<ConfirmModal>,
    snapshot_store: SnapshotStore,
}

impl App {
    fn new(interval_ms: u64) -> Self {
        let clamped = clamp_interval_ms(interval_ms);
        let is_root = unsafe { libc::geteuid() == 0 };
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
            tune_selected: 0,
            staged: HashMap::new(),
            allow_extreme: false,
            confirm_modal: None,
            snapshot_store: SnapshotStore::default(),
        }
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
                self.telemetry = Some(snap);
                self.telemetry_error = None;
                self.sample_count = self.sample_count.saturating_add(1);
            }
            Err(err) => {
                self.telemetry_error = Some(err.to_string());
            }
        }
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
        let caps = &hw.capabilities;
        let mut rows = Vec::new();

        // 1. Curve Optimizer
        if caps.set_co_per_core.is_some() || caps.set_co_all_cores.is_some() {
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

        // 2. Curve Shaper (Zen 5)
        if caps.set_curve_shaper.is_some() {
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
                    format!(
                        "Not exposed by SMU {} ({code})",
                        hw.driver.smu_version
                    ),
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

        // 3. PBO Limits & Boost
        let has_pbo = caps.set_ppt.is_some()
            || caps.set_tdc.is_some()
            || caps.set_edc.is_some()
            || caps.set_tctl.is_some()
            || caps.set_pbo_scalar.is_some()
            || caps.set_fmax.is_some();
        if has_pbo {
            rows.push(TuneRow::SectionHeader(
                TuneSection::Pbo.title().to_string(),
                format!("Tctl max {}°C", pbo::TCTL_MAX_C),
            ));
            if caps.set_ppt.is_some() {
                rows.push(TuneRow::Param(TuneParam::PboPpt));
            }
            if caps.set_tdc.is_some() {
                rows.push(TuneRow::Param(TuneParam::PboTdc));
            }
            if caps.set_edc.is_some() {
                rows.push(TuneRow::Param(TuneParam::PboEdc));
            }
            if caps.set_tctl.is_some() {
                rows.push(TuneRow::Param(TuneParam::PboTctl));
            }
            if caps.set_pbo_scalar.is_some() {
                rows.push(TuneRow::Param(TuneParam::PboScalar));
            }
            if caps.set_fmax.is_some() {
                rows.push(TuneRow::Param(TuneParam::PboFmax));
            }
        }

        // 4. APU DPTC (automatically shown when set_stapm_limit is Some)
        if caps.set_stapm_limit.is_some() {
            rows.push(TuneRow::SectionHeader(
                TuneSection::Apu.title().to_string(),
                "Mobile Power & Thermal".to_string(),
            ));
            rows.push(TuneRow::Param(TuneParam::ApuStapm));
            if hw.codename.is_apu() && caps.set_ppt.is_some() {
                rows.push(TuneRow::Param(TuneParam::ApuFast));
            }
            if caps.set_slow_limit.is_some() {
                rows.push(TuneRow::Param(TuneParam::ApuSlow));
            }
            if caps.set_apu_slow_limit.is_some() {
                rows.push(TuneRow::Param(TuneParam::ApuApuSlow));
            }
            if caps.set_apu_skin_temp.is_some() {
                rows.push(TuneRow::Param(TuneParam::ApuSkinTemp));
            }
            if caps.set_dgpu_skin_temp.is_some() {
                rows.push(TuneRow::Param(TuneParam::ApuDgpuSkinTemp));
            }
            if caps.set_max_performance.is_some() || caps.set_power_saving.is_some() {
                rows.push(TuneRow::Param(TuneParam::ApuPowerMode));
            }
        }

        rows
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

    /// Returns the known live value of `param` from hardware telemetry / mailbox readback.
    fn live_param_value(&self, param: TuneParam) -> Option<TuneValue> {
        match param {
            TuneParam::CoAll => {
                let snap = self.co_state.as_loaded()?;
                let first = snap.cores.first()?.margin;
                if snap.cores.iter().all(|c| c.margin == first) {
                    Some(TuneValue::Int(i32::from(first)))
                } else {
                    None
                }
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
            TuneParam::PboPpt => self
                .telemetry
                .as_ref()
                .and_then(|t| t.ppt_limit_w)
                .filter(|v| v.is_finite())
                .map(|v| TuneValue::Int(v.round() as i32))
                .or_else(|| {
                    self.pbo_state
                        .as_loaded()
                        .and_then(|p| p.fused.ppt_w)
                        .map(|v| TuneValue::Int(v as i32))
                }),
            TuneParam::PboTdc => self
                .telemetry
                .as_ref()
                .and_then(|t| t.tdc_limit_a)
                .filter(|v| v.is_finite())
                .map(|v| TuneValue::Int(v.round() as i32))
                .or_else(|| {
                    self.pbo_state
                        .as_loaded()
                        .and_then(|p| p.fused.vdd_tdc_a)
                        .map(|v| TuneValue::Int(v as i32))
                }),
            TuneParam::PboEdc => self
                .telemetry
                .as_ref()
                .and_then(|t| t.edc_limit_a)
                .filter(|v| v.is_finite())
                .map(|v| TuneValue::Int(v.round() as i32))
                .or_else(|| {
                    self.pbo_state
                        .as_loaded()
                        .and_then(|p| p.fused.edc_a)
                        .map(|v| TuneValue::Int(v as i32))
                }),
            TuneParam::PboTctl => self
                .telemetry
                .as_ref()
                .and_then(|t| t.tctl_limit_c)
                .filter(|v| v.is_finite())
                .map(|v| TuneValue::Int(v.round() as i32)),
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
            TuneParam::ApuStapm => self
                .pbo_state
                .as_loaded()
                .and_then(|p| p.fused.slow_w)
                .map(|v| TuneValue::Int(v as i32)),
            TuneParam::ApuFast => self
                .pbo_state
                .as_loaded()
                .and_then(|p| p.fused.fast_w)
                .map(|v| TuneValue::Int(v as i32))
                .or_else(|| {
                    self.telemetry
                        .as_ref()
                        .and_then(|t| t.ppt_limit_w)
                        .filter(|v| v.is_finite())
                        .map(|v| TuneValue::Int(v.round() as i32))
                }),
            TuneParam::ApuSlow => self
                .pbo_state
                .as_loaded()
                .and_then(|p| p.fused.slow_w)
                .map(|v| TuneValue::Int(v as i32)),
            TuneParam::ApuApuSlow => self
                .pbo_state
                .as_loaded()
                .and_then(|p| p.fused.apu_slow_w)
                .map(|v| TuneValue::Int(v as i32)),
            TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp => None,
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
            TuneParam::ApuFast => TuneValue::Int(45),
            TuneParam::ApuSlow => TuneValue::Int(35),
            TuneParam::ApuApuSlow => TuneValue::Int(30),
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
            TuneParam::Shaper { .. } => (i32::from(shaper::SHAPER_MIN), i32::from(shaper::SHAPER_MAX)),
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
            TuneParam::CoAll | TuneParam::CoCore(_) => hw
                .capabilities
                .co_bounds
                .is_some_and(|b| v < b.soft_min),
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
                let next_v = cur_int.saturating_add(delta_sign * step).clamp(min_v, max_v);
                TuneValue::Int(next_v)
            }
            _ => return,
        };

        if live == Some(next) || (param == TuneParam::ApuPowerMode && next == TuneValue::PowerMode(None)) {
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
        if params.is_empty() {
            return;
        }
        let cur_idx = self.tune_selected.min(params.len() - 1);
        let cur_sec = params[cur_idx].section();

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
        let sec_pos = starts
            .iter()
            .position(|(s, _)| *s == cur_sec)
            .unwrap_or(0);
        let next_pos = if forward {
            (sec_pos + 1) % starts.len()
        } else if sec_pos == 0 {
            starts.len() - 1
        } else {
            sec_pos - 1
        };
        self.tune_selected = starts[next_pos].1;
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

    fn enrich_pbo_op_current(&self, op: &mut PlannedOp) {
        if op.current.is_some() {
            return;
        }
        if op.description.starts_with("Set PPT") {
            op.current = self
                .live_param_value(TuneParam::PboPpt)
                .and_then(TuneValue::as_int)
                .map(|v| format!("{v} W"));
        } else if op.description.starts_with("Set TDC") {
            op.current = self
                .live_param_value(TuneParam::PboTdc)
                .and_then(TuneValue::as_int)
                .map(|v| format!("{v} A"));
        } else if op.description.starts_with("Set EDC") {
            op.current = self
                .live_param_value(TuneParam::PboEdc)
                .and_then(TuneValue::as_int)
                .map(|v| format!("{v} A"));
        } else if op.description.starts_with("Set Tctl") {
            op.current = self
                .live_param_value(TuneParam::PboTctl)
                .and_then(TuneValue::as_int)
                .map(|v| format!("{v} C"));
        } else if op.description.starts_with("Set PBO scalar") {
            op.current = self
                .live_param_value(TuneParam::PboScalar)
                .and_then(TuneValue::as_int)
                .map(|v| format!("{v}x"));
        } else if op.description.starts_with("Set Fmax") {
            op.current = self
                .live_param_value(TuneParam::PboFmax)
                .and_then(TuneValue::as_int)
                .map(|v| format!("{v} MHz"));
        }
    }

    fn open_staged_dry_run_modal(
        &mut self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) {
        if self.staged.is_empty() {
            self.set_flash_kind("No staged changes to preview", FlashKind::Warn);
            return;
        }

        let co_all = self.staged.get(&TuneParam::CoAll).and_then(|v| v.as_int());
        let mut co_cores: Vec<(u32, i32)> = Vec::new();
        for core in &hw.topology.cores {
            if let Some(val) = self
                .staged
                .get(&TuneParam::CoCore(core.index))
                .and_then(|v| v.as_int())
            {
                co_cores.push((core.index, val));
            }
        }
        let co_igpu = self.staged.get(&TuneParam::CoIgpu).and_then(|v| v.as_int());

        let mut shaper_tiers: Vec<(FreqTier, ShaperTierMargins)> = Vec::new();
        if hw.capabilities.set_curve_shaper.is_some()
            && !matches!(self.shaper_state, MailboxState::FirmwareUnsupported(_))
        {
            let live_cs = self.shaper_state.as_loaded().copied().unwrap_or_default();
            for tier in FreqTier::ALL {
                let s_low = self
                    .staged
                    .get(&TuneParam::Shaper {
                        tier,
                        band: TempBand::Low,
                    })
                    .and_then(|v| v.as_int());
                let s_med = self
                    .staged
                    .get(&TuneParam::Shaper {
                        tier,
                        band: TempBand::Med,
                    })
                    .and_then(|v| v.as_int());
                let s_high = self
                    .staged
                    .get(&TuneParam::Shaper {
                        tier,
                        band: TempBand::High,
                    })
                    .and_then(|v| v.as_int());
                if s_low.is_some() || s_med.is_some() || s_high.is_some() {
                    let base = live_cs.tier(tier);
                    let margins = ShaperTierMargins {
                        low: s_low.map(|v| v as i8).unwrap_or(base.low),
                        med: s_med.map(|v| v as i8).unwrap_or(base.med),
                        high: s_high.map(|v| v as i8).unwrap_or(base.high),
                    };
                    shaper_tiers.push((tier, margins));
                }
            }
        }

        let pbo_req = PboSetRequest {
            ppt_w: self
                .staged
                .get(&TuneParam::PboPpt)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
            tdc_a: self
                .staged
                .get(&TuneParam::PboTdc)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
            edc_a: self
                .staged
                .get(&TuneParam::PboEdc)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
            tctl_c: self
                .staged
                .get(&TuneParam::PboTctl)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
            scalar: self
                .staged
                .get(&TuneParam::PboScalar)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
            fmax_mhz: self
                .staged
                .get(&TuneParam::PboFmax)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
        };
        let has_pbo = pbo_req.ppt_w.is_some()
            || pbo_req.tdc_a.is_some()
            || pbo_req.edc_a.is_some()
            || pbo_req.tctl_c.is_some()
            || pbo_req.scalar.is_some()
            || pbo_req.fmax_mhz.is_some();

        let apu_req = ApuSetRequest {
            stapm_w: self
                .staged
                .get(&TuneParam::ApuStapm)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
            fast_w: self
                .staged
                .get(&TuneParam::ApuFast)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
            slow_w: self
                .staged
                .get(&TuneParam::ApuSlow)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
            apu_slow_w: self
                .staged
                .get(&TuneParam::ApuApuSlow)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
            apu_skin_temp_c: self
                .staged
                .get(&TuneParam::ApuSkinTemp)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
            dgpu_skin_temp_c: self
                .staged
                .get(&TuneParam::ApuDgpuSkinTemp)
                .and_then(|v| v.as_int())
                .map(|v| v as u32),
            power_mode: self
                .staged
                .get(&TuneParam::ApuPowerMode)
                .and_then(|v| v.as_power_mode()),
            ..Default::default()
        };
        let has_apu = apu_req.stapm_w.is_some()
            || apu_req.fast_w.is_some()
            || apu_req.slow_w.is_some()
            || apu_req.apu_slow_w.is_some()
            || apu_req.apu_skin_temp_c.is_some()
            || apu_req.dgpu_skin_temp_c.is_some()
            || apu_req.power_mode.is_some();

        // Check Fmax soft delta in dry-run (since pbo::set_pbo skips live get_fmax in dry_run)
        if let Some(fmax) = pbo_req.fmax_mhz {
            if !self.allow_extreme {
                if let Some(cur) = self.pbo_state.as_loaded().and_then(|p| p.fmax_mhz) {
                    if fmax > cur.saturating_add(pbo::FMAX_SOFT_DELTA_MHZ) {
                        self.set_flash_kind(
                            format!(
                                "Extreme blocked: fmax {fmax} > {} MHz (press 'x' for Allow Extreme)",
                                cur.saturating_add(pbo::FMAX_SOFT_DELTA_MHZ)
                            ),
                            FlashKind::Error,
                        );
                        return;
                    }
                }
            }
        }

        let allow_extreme = self.allow_extreme;
        let plan_res: zensmu_core::Result<Vec<PlannedOp>> = (|| {
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
                let mut v = co::set_co_cores(transport, hw, &co_cores, allow_extreme, true)?;
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
            for &(tier, margins) in &shaper_tiers {
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
            if has_pbo {
                let mut v = pbo::set_pbo(transport, hw, &pbo_req, allow_extreme, true)?;
                for op in &mut v {
                    self.enrich_pbo_op_current(op);
                }
                ops.extend(v);
            }
            if has_apu {
                let v = apu::set_apu(transport, hw, &apu_req, allow_extreme, true)?;
                ops.extend(v);
            }
            Ok(ops)
        })();

        match plan_res {
            Ok(ops) if ops.is_empty() => {
                self.set_flash_kind("No SMU operations to apply", FlashKind::Warn);
            }
            Ok(ops) => {
                self.confirm_modal = Some(ConfirmModal {
                    title: " Dry-Run Preview — Apply Staged Tuning ".to_string(),
                    subtitle: format!(
                        "{} SMU operation(s) validated · dry-run passed · zero writes sent yet",
                        ops.len()
                    ),
                    action: ConfirmAction::ApplyStaged {
                        co_all,
                        co_cores,
                        co_igpu,
                        shaper_tiers,
                        pbo: has_pbo.then_some(pbo_req),
                        apu: has_apu.then_some(apu_req),
                    },
                    ops,
                    scroll: 0,
                    allow_extreme,
                });
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
                self.set_flash_kind(
                    format!("Hardware already matches {label}"),
                    FlashKind::Info,
                );
            }
            Ok(ops) => {
                let (title, action) = if boot_baseline {
                    (
                        " Dry-Run Preview — Reset to Boot Baseline ".to_string(),
                        ConfirmAction::ResetBootBaseline(snap),
                    )
                } else {
                    (
                        " Dry-Run Preview — Rollback to Last State ".to_string(),
                        ConfirmAction::Rollback(snap),
                    )
                };
                self.confirm_modal = Some(ConfirmModal {
                    title,
                    subtitle: format!(
                        "Restoring {label} · {} SMU operation(s) planned",
                        ops.len()
                    ),
                    action,
                    ops,
                    scroll: 0,
                    allow_extreme: true,
                });
            }
            Err(err) => {
                self.set_flash_kind(
                    format!("Snapshot dry-run failed: {err}"),
                    FlashKind::Error,
                );
            }
        }
    }

    fn open_reset_co_zero_modal(
        &mut self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) {
        let plan_res: zensmu_core::Result<Vec<PlannedOp>> = (|| {
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
        })();

        match plan_res {
            Ok(ops) => {
                self.confirm_modal = Some(ConfirmModal {
                    title: " Dry-Run Preview — Reset Curve Optimizer to 0 ".to_string(),
                    subtitle: format!(
                        "{} SMU operation(s) to reset CO margins to 0",
                        ops.len()
                    ),
                    action: ConfirmAction::ResetCoZero,
                    ops,
                    scroll: 0,
                    allow_extreme: false,
                });
            }
            Err(err) => {
                self.set_flash_kind(format!("CO reset unavailable: {err}"), FlashKind::Error);
            }
        }
    }

    fn execute_confirmed_modal(
        &mut self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) {
        let Some(modal) = self.confirm_modal.take() else {
            return;
        };

        if !self.can_write() {
            self.set_flash_kind(
                "Refused: root required to write SMU (relaunch with sudo zensmu top)",
                FlashKind::Error,
            );
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
            } => (|| {
                let mut applied = 0usize;
                if let Some(m) = *co_all {
                    applied += co::set_co_all(transport, hw, m, modal.allow_extreme, false)?.len();
                }
                if !co_cores.is_empty() {
                    applied +=
                        co::set_co_cores(transport, hw, co_cores, modal.allow_extreme, false)?
                            .len();
                }
                if let Some(m) = *co_igpu {
                    co::set_igpu_co(transport, hw, m, modal.allow_extreme, false)?;
                    applied += 1;
                }
                for &(tier, margins) in shaper_tiers {
                    shaper::set_curve_shaper_tier(
                        transport,
                        hw,
                        tier,
                        margins,
                        modal.allow_extreme,
                        false,
                    )?;
                    applied += 1;
                }
                if let Some(req) = pbo {
                    applied += pbo::set_pbo(transport, hw, req, modal.allow_extreme, false)?.len();
                }
                if let Some(req) = apu {
                    applied += apu::set_apu(transport, hw, req, modal.allow_extreme, false)?.len();
                }
                Ok(applied)
            })(),
            ConfirmAction::Rollback(snap) | ConfirmAction::ResetBootBaseline(snap) => {
                session::apply_snapshot(transport, hw, snap, true, false).map(|ops| ops.len())
            }
            ConfirmAction::ResetCoZero => (|| {
                let mut applied = 0usize;
                if hw.capabilities.set_co_all_cores.is_some() {
                    applied += co::set_co_all(transport, hw, 0, false, false)?.len();
                } else if hw.capabilities.set_co_per_core.is_some() {
                    let pairs: Vec<(u32, i32)> =
                        hw.topology.cores.iter().map(|c| (c.index, 0)).collect();
                    applied += co::set_co_cores(transport, hw, &pairs, false, false)?.len();
                }
                if hw.capabilities.set_igpu_co.is_some() {
                    co::set_igpu_co(transport, hw, 0, false, false)?;
                    applied += 1;
                }
                Ok(applied)
            })(),
        };

        // Always refresh live mailbox + telemetry right after write attempts
        self.refresh_mailbox(transport, hw);
        self.poll_telemetry(transport, hw);

        match exec_res {
            Ok(n) => {
                match modal.action {
                    ConfirmAction::ResetCoZero => {
                        self.staged.retain(|p, _| {
                            !matches!(p, TuneParam::CoAll | TuneParam::CoCore(_) | TuneParam::CoIgpu)
                        });
                    }
                    _ => {
                        self.staged.clear();
                    }
                }
                self.set_flash_kind(
                    format!("Applied {n} SMU operation(s) & verified"),
                    FlashKind::Success,
                );
            }
            Err(err) => {
                self.set_flash_kind(format!("SMU apply failed: {err}"), FlashKind::Error);
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
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    self.execute_confirmed_modal(transport, hw);
                }
                KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                    self.confirm_modal = None;
                    self.set_flash("Dry-run cancelled (no SMU writes sent)");
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if let Some(m) = self.confirm_modal.as_mut() {
                        m.scroll = m.scroll.saturating_sub(1);
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if let Some(m) = self.confirm_modal.as_mut() {
                        let max_scroll = m.ops.len().saturating_sub(1);
                        m.scroll = (m.scroll + 1).min(max_scroll);
                    }
                }
                _ => {}
            }
            return false;
        }

        // Global keys
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return true,
            KeyCode::Char('?') => {
                self.show_help = !self.show_help;
                return false;
            }
            KeyCode::Esc => {
                if self.show_help {
                    self.show_help = false;
                }
                return false;
            }
            KeyCode::Tab => {
                self.show_help = false;
                self.active_tab = self.active_tab.next();
                return false;
            }
            KeyCode::BackTab => {
                self.show_help = false;
                self.active_tab = self.active_tab.prev();
                return false;
            }
            KeyCode::Char('1') => {
                self.show_help = false;
                self.active_tab = ViewTab::Overview;
                return false;
            }
            KeyCode::Char('2') => {
                self.show_help = false;
                self.active_tab = ViewTab::ClocksVolts;
                return false;
            }
            KeyCode::Char('3') => {
                self.show_help = false;
                self.active_tab = ViewTab::CoShaper;
                return false;
            }
            KeyCode::Char('4') | KeyCode::Char('t') => {
                self.show_help = false;
                self.active_tab = ViewTab::Tune;
                self.clamp_tune_selection(hw);
                return false;
            }
            KeyCode::Char('p') | KeyCode::Char('P') | KeyCode::Char(' ') => {
                self.paused = !self.paused;
                self.set_flash(if self.paused {
                    "Telemetry paused"
                } else {
                    "Telemetry resumed"
                });
                return false;
            }
            KeyCode::Char('r') => {
                self.poll_telemetry(transport, hw);
                self.refresh_mailbox(transport, hw);
                if self.mailbox_note.is_some() {
                    self.set_flash("Telemetry refreshed (SMU mailbox requires sudo)");
                } else {
                    self.set_flash("SMU mailbox & telemetry refreshed");
                }
                return false;
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.interval_ms = adjust_interval_ms(self.interval_ms, true);
                self.set_flash(format!("Interval: {}ms", self.interval_ms));
                return false;
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.interval_ms = adjust_interval_ms(self.interval_ms, false);
                self.set_flash(format!("Interval: {}ms", self.interval_ms));
                return false;
            }
            _ => {}
        }

        if self.show_help {
            return false;
        }

        // Tab-specific controls
        if self.active_tab == ViewTab::Tune {
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
                KeyCode::Char('[') => {
                    self.jump_tune_section(hw, false);
                }
                KeyCode::Char(']') => {
                    self.jump_tune_section(hw, true);
                }
                KeyCode::Char('H') | KeyCode::PageDown => {
                    self.adjust_selected_param(hw, -1, true);
                }
                KeyCode::Char('L') | KeyCode::PageUp => {
                    self.adjust_selected_param(hw, 1, true);
                }
                KeyCode::Left => {
                    self.adjust_selected_param(hw, -1, shifted);
                }
                KeyCode::Right => {
                    self.adjust_selected_param(hw, 1, shifted);
                }
                KeyCode::Char('h') => {
                    self.adjust_selected_param(hw, -1, false);
                }
                KeyCode::Char('l') => {
                    self.adjust_selected_param(hw, 1, false);
                }
                KeyCode::Backspace => {
                    self.reset_selected_param(hw, false);
                }
                KeyCode::Char('0') => {
                    self.reset_selected_param(hw, true);
                }
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
                KeyCode::Char('a') | KeyCode::Enter => {
                    self.open_staged_dry_run_modal(transport, hw);
                }
                KeyCode::Char('R') => {
                    self.open_snapshot_restore_modal(transport, hw, false);
                }
                KeyCode::Char('B') => {
                    self.open_snapshot_restore_modal(transport, hw, true);
                }
                KeyCode::Char('Z') => {
                    self.open_reset_co_zero_modal(transport, hw);
                }
                KeyCode::Char('s') => {
                    self.save_live_profile(transport, hw);
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Right | KeyCode::Char('l') => {
                    self.active_tab = self.active_tab.next();
                }
                KeyCode::Left | KeyCode::Char('h') => {
                    self.active_tab = self.active_tab.prev();
                }
                KeyCode::Char('R') => {
                    self.poll_telemetry(transport, hw);
                    self.refresh_mailbox(transport, hw);
                    if self.mailbox_note.is_some() {
                        self.set_flash("Telemetry refreshed (SMU mailbox requires sudo)");
                    } else {
                        self.set_flash("SMU mailbox & telemetry refreshed");
                    }
                }
                _ => {}
            }
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

    // Initial unprivileged PM table read + best-effort mailbox readback.
    app.poll_telemetry(transport, hw);
    app.refresh_mailbox(transport, hw);

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

    // Row 1: Badge + CPU identity on left, SMU fw / driver / interval / status on right
    let cpu_title = cpu_display_title(hw);
    let mut left_spans = vec![
        Span::styled(
            " zensmu top ",
            Style::default()
                .fg(palette::BADGE_FG)
                .bg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(
            cpu_title,
            Style::default().fg(palette::FG).add_modifier(Modifier::BOLD),
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

    if app.allow_extreme {
        right_spans.push(Span::styled(
            " EXTREME ",
            Style::default()
                .fg(palette::BADGE_FG)
                .bg(palette::ORANGE)
                .add_modifier(Modifier::BOLD),
        ));
        right_spans.push(Span::raw(" "));
    }

    if app.paused {
        right_spans.push(Span::styled(
            " PAUSED ",
            Style::default()
                .fg(palette::BADGE_FG)
                .bg(palette::YELLOW)
                .add_modifier(Modifier::BOLD),
        ));
        right_spans.push(Span::raw(" "));
    }

    let (priv_label, priv_bg) = if app.is_root {
        (" ROOT ", palette::GREEN)
    } else if app.has_any_mailbox_data() && app.mailbox_note.is_none() {
        (" SMU:OK ", palette::TEAL)
    } else {
        (" UNPRIV ", palette::YELLOW)
    };
    right_spans.push(Span::styled(
        priv_label,
        Style::default()
            .fg(palette::BADGE_FG)
            .bg(priv_bg)
            .add_modifier(Modifier::BOLD),
    ));

    let right_w = if inner.width < 92 { 44 } else { 62 };
    let row1_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(18), Constraint::Length(right_w)])
        .split(rows[0]);

    // Trim left spans gracefully via Paragraph truncation
    frame.render_widget(
        Paragraph::new(Line::from(std::mem::take(&mut left_spans)))
            .style(Style::default().bg(palette::BAR_BG)),
        row1_split[0],
    );
    frame.render_widget(
        Paragraph::new(Line::from(right_spans))
            .alignment(Alignment::Right)
            .style(Style::default().bg(palette::BAR_BG)),
        row1_split[1],
    );

    // Row 2: View tabs on left, compact note or sample counter on right
    let tab_span = |label: &str, active: bool| -> Span<'_> {
        if active {
            Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(palette::BADGE_FG)
                    .bg(palette::TEAL)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::styled(format!(" {label} "), Style::default().fg(palette::MUTED))
        }
    };

    let compact = inner.width < 100;
    let staged_n = app.staged_count();
    let tune_active = app.active_tab == ViewTab::Tune;
    let mut tabs_spans = vec![
        tab_span(
            if compact { "[1:Over]" } else { "[1:Overview]" },
            app.active_tab == ViewTab::Overview,
        ),
        Span::raw(" "),
        tab_span(
            if compact { "[2:Clocks]" } else { "[2:Clocks & Volts]" },
            app.active_tab == ViewTab::ClocksVolts,
        ),
        Span::raw(" "),
        tab_span(
            if compact { "[3:CO/Shp]" } else { "[3:CO & Shaper]" },
            app.active_tab == ViewTab::CoShaper,
        ),
        Span::raw(" "),
    ];

    if tune_active {
        tabs_spans.push(Span::styled(
            " [4:Tune] ",
            Style::default()
                .fg(palette::BADGE_FG)
                .bg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        ));
    } else {
        tabs_spans.push(Span::styled(
            " [4:Tune] ",
            Style::default().fg(palette::MUTED),
        ));
    }
    if staged_n > 0 {
        tabs_spans.push(Span::styled(
            format!(" *{staged_n} "),
            Style::default()
                .fg(palette::BADGE_FG)
                .bg(palette::ORANGE)
                .add_modifier(Modifier::BOLD),
        ));
    }

    tabs_spans.push(Span::raw(" "));
    tabs_spans.push(tab_span("[?:Help]", app.show_help));

    let tabs_line = Line::from(tabs_spans);

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
        .split(rows[1]);

    frame.render_widget(
        Paragraph::new(tabs_line).style(Style::default().bg(palette::BAR_BG)),
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
        left.extend(keycap("←→/hl", "Step"));
        left.extend(keycap("H/L", "Coarse"));
        left.extend(keycap("a/Enter", "DryRun"));
        left.extend(keycap("u", "Undo"));
        left.extend(keycap("x", "Ext"));
        left.extend(keycap("R/B/Z", "Reset"));
        left.extend(keycap("s", "Save"));
    } else {
        left.extend(keycap("q", "Quit"));
        left.extend(keycap("Tab/1-4", "Views"));
        left.extend(keycap("t", "Tune"));
        left.extend(keycap("p", "Pause"));
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

    let right_w = if app.active_flash().is_some() {
        (area.width / 2).clamp(24, 58)
    } else {
        28
    };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(30), Constraint::Length(right_w)])
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

fn draw_overview_tab(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(6),
            Constraint::Percentage(42),
            Constraint::Min(8),
        ])
        .split(area);

    draw_power_thermal_gauges(frame, rows[0], app);

    let spark_cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(rows[1]);

    let snap = app.telemetry.as_ref();
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

fn draw_power_thermal_gauges(frame: &mut Frame, area: Rect, app: &App) {
    let block = styled_block(" Power & Thermal Limits ", palette::CYAN);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let snap = app.telemetry.as_ref();
    let gauge_rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(inner);

    let specs = [
        (
            "PPT ",
            snap.and_then(|s| s.ppt_value_w),
            snap.and_then(|s| s.ppt_limit_w),
            "W",
            false,
        ),
        (
            "TDC ",
            snap.and_then(|s| s.tdc_value_a),
            snap.and_then(|s| s.tdc_limit_a),
            "A",
            false,
        ),
        (
            "EDC ",
            snap.and_then(|s| s.edc_value_a),
            snap.and_then(|s| s.edc_limit_a),
            "A",
            false,
        ),
        (
            "Tctl",
            snap.and_then(|s| s.tctl_value_c),
            snap.and_then(|s| s.tctl_limit_c),
            "°C",
            true,
        ),
    ];

    for (idx, (label, val, lim, unit, is_thermal)) in specs.into_iter().enumerate() {
        if idx >= gauge_rows.len() {
            break;
        }
        let ratio = gauge_ratio(val, lim);
        let color = if is_thermal {
            thermal_color(val, lim)
        } else {
            ratio_color(ratio)
        };
        let readout = format_gauge_readout(val, lim, unit);

        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(6),
                Constraint::Min(10),
                Constraint::Length(25),
            ])
            .split(gauge_rows[idx]);

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {label}"),
                Style::default().fg(palette::MUTED).add_modifier(Modifier::BOLD),
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
            Style::default().fg(palette::CYAN).add_modifier(Modifier::BOLD),
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
            Cell::from(format_opt_f32(fclk, 0, "MHz"))
                .style(Style::default().fg(palette::TEAL).add_modifier(Modifier::BOLD)),
            Cell::from("VDDCR_SOC").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.vddcr_soc_v), 3, "V"))
                .style(Style::default().fg(palette::PURPLE).add_modifier(Modifier::BOLD)),
        ]),
        Row::new(vec![
            Cell::from("UCLK").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(uclk, 0, "MHz"))
                .style(Style::default().fg(palette::CYAN).add_modifier(Modifier::BOLD)),
            Cell::from("VDDCR_MISC").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.vddcr_misc_v), 3, "V"))
                .style(Style::default().fg(palette::FG)),
        ]),
        Row::new(vec![
            Cell::from("MCLK").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(mclk, 0, "MHz"))
                .style(Style::default().fg(palette::PURPLE).add_modifier(Modifier::BOLD)),
            Cell::from("CLDO_VDDP").style(Style::default().fg(palette::MUTED)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.cldo_vddp_v), 3, "V"))
                .style(Style::default().fg(palette::FG)),
        ]),
        Row::new(vec![
            Cell::from("Ratio").style(Style::default().fg(palette::MUTED)),
            Cell::from(if ratio_badge.is_empty() { "—" } else { ratio_badge })
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
    let block =
        styled_block(" Active Curve Optimizer / PBO ", palette::CYAN).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(ref note) = app.mailbox_note {
        if !app.has_any_mailbox_data() {
            let lines = vec![
                Line::from(""),
                Line::from(vec![
                    Span::styled(
                        " [LOCKED] ",
                        Style::default()
                            .fg(palette::BADGE_FG)
                            .bg(palette::YELLOW)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(note.as_str(), Style::default().fg(palette::YELLOW)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("Tip: ", Style::default().fg(palette::MUTED)),
                    Span::styled("sudo zensmu top", Style::default().fg(palette::CYAN)),
                    Span::styled("  ·  or press ", Style::default().fg(palette::MUTED)),
                    Span::styled("r", Style::default().fg(palette::TEAL).add_modifier(Modifier::BOLD)),
                    Span::styled(" to retry SMU mailbox", Style::default().fg(palette::MUTED)),
                ]),
            ];
            frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
            return;
        }
    }

    let mut lines: Vec<Line<'_>> = Vec::new();

    // PBO status & flags line
    if let Some(pbo) = app.pbo_state.as_loaded() {
        let mut spans = vec![
            Span::styled("Scalar ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format_opt_f32(pbo.scalar, 2, "x"),
                Style::default().fg(palette::CYAN).add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Fmax ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format_opt_u32(pbo.fmax_mhz, "MHz"),
                Style::default().fg(palette::TEAL).add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
        ];
        spans.push(flag_pill("OC", pbo.flags.overclockable));
        spans.push(Span::raw(" "));
        spans.push(flag_pill("EXPO", pbo.flags.expo_active));
        spans.push(Span::raw(" "));
        spans.push(flag_pill("LN2", pbo.flags.ln2_mode));
        lines.push(Line::from(spans));
    } else {
        lines.push(Line::from(vec![
            Span::styled("PBO: ", Style::default().fg(palette::MUTED)),
            Span::styled(
                mailbox_status_label(&app.pbo_state, &hw.driver.smu_version),
                Style::default().fg(palette::DIM),
            ),
        ]));
    }

    // Per-core CO compact pills
    match &app.co_state {
        MailboxState::Loaded(co_snap) => {
            if co_snap.cores.len() > 8 {
                // On high-core-count dual-CCD CPUs (e.g. 16-core 5900XT/9950X),
                // show a concise all-equal summary or CCD-grouped compact lines so nothing clips.
                let first = co_snap.cores[0].margin;
                if co_snap.cores.iter().all(|c| c.margin == first) {
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
                    for chunk in co_snap.cores.chunks(8) {
                        let mut pill_spans: Vec<Span<'_>> = Vec::new();
                        let label = if chunk[0].core_index == 0 {
                            "CO: "
                        } else {
                            "    "
                        };
                        pill_spans.push(Span::styled(label, Style::default().fg(palette::MUTED)));
                        for core in chunk {
                            let col = margin_color(core.margin);
                            pill_spans.push(Span::styled(
                                format!("C{}:{}", core.core_index, format_signed_margin(core.margin)),
                                Style::default().fg(col).add_modifier(Modifier::BOLD),
                            ));
                            pill_spans.push(Span::raw(" "));
                        }
                        lines.push(Line::from(pill_spans));
                    }
                }
            } else {
                let mut pill_spans: Vec<Span<'_>> = Vec::new();
                pill_spans.push(Span::styled("CO: ", Style::default().fg(palette::MUTED)));
                for core in &co_snap.cores {
                    let col = margin_color(core.margin);
                    pill_spans.push(Span::styled(
                        format!("C{}:{}", core.core_index, format_signed_margin(core.margin)),
                        Style::default().fg(col).add_modifier(Modifier::BOLD),
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

    // Curve Shaper quick status
    match &app.shaper_state {
        MailboxState::Loaded(shaper) => {
            let all_vals = shaper_all_values(shaper);
            let min_v = all_vals.iter().copied().min().unwrap_or(0);
            let max_v = all_vals.iter().copied().max().unwrap_or(0);
            let active_count = all_vals.iter().filter(|&&v| v != 0).count();
            lines.push(Line::from(vec![
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
            ]));
        }
        MailboxState::Unsupported => {
            lines.push(Line::from(vec![
                Span::styled("Shaper: ", Style::default().fg(palette::MUTED)),
                Span::styled(
                    format!("N/A on {}", hw.codename.display_name()),
                    Style::default().fg(palette::DIM),
                ),
            ]));
        }
        other => {
            lines.push(Line::from(vec![
                Span::styled("Shaper: ", Style::default().fg(palette::MUTED)),
                Span::styled(
                    mailbox_status_label(other, &hw.driver.smu_version),
                    Style::default().fg(palette::DIM),
                ),
            ]));
        }
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
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
        s.min.low, s.min.med, s.min.high,
        s.low.low, s.low.med, s.low.high,
        s.med.low, s.med.med, s.med.high,
        s.high.low, s.high.med, s.high.high,
        s.max.low, s.max.med, s.max.high,
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

fn draw_voltage_rails_table(frame: &mut Frame, area: Rect, app: &App) {
    let block = styled_block(" Voltage Rails & Current Telemetry ", palette::PURPLE);
    let snap = app.telemetry.as_ref();

    let soc_stats = app
        .history
        .vddcr_soc_v
        .stats()
        .map(|(mn, av, mx)| format!("{mn:.3} / {av:.3} / {mx:.3} V"))
        .unwrap_or_else(|| "—".to_string());
    let tdc_stats = app
        .history
        .tdc_a
        .stats()
        .map(|(mn, av, mx)| format!("{mn:.1} / {av:.1} / {mx:.1} A"))
        .unwrap_or_else(|| "—".to_string());
    let edc_stats = app
        .history
        .edc_a
        .stats()
        .map(|(mn, av, mx)| format!("{mn:.1} / {av:.1} / {mx:.1} A"))
        .unwrap_or_else(|| "—".to_string());

    let soc_val = snap.and_then(|s| s.vddcr_soc_v);
    let rows = vec![
        Row::new(vec![
            Cell::from("VDDCR_SOC").style(Style::default().fg(palette::FG)),
            Cell::from(format_opt_f32(soc_val, 3, "V"))
                .style(Style::default().fg(soc_voltage_color(soc_val)).add_modifier(Modifier::BOLD)),
            Cell::from(soc_stats).style(Style::default().fg(palette::MUTED)),
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
            Cell::from(format_opt_f32(snap.and_then(|s| s.tdc_value_a), 1, "A"))
                .style(Style::default().fg(ratio_color(gauge_ratio(
                    snap.and_then(|s| s.tdc_value_a),
                    snap.and_then(|s| s.tdc_limit_a),
                )))),
            Cell::from(tdc_stats).style(Style::default().fg(palette::MUTED)),
        ]),
        Row::new(vec![
            Cell::from("EDC Current").style(Style::default().fg(palette::FG)),
            Cell::from(format_opt_f32(snap.and_then(|s| s.edc_value_a), 1, "A"))
                .style(Style::default().fg(ratio_color(gauge_ratio(
                    snap.and_then(|s| s.edc_value_a),
                    snap.and_then(|s| s.edc_limit_a),
                )))),
            Cell::from(edc_stats).style(Style::default().fg(palette::MUTED)),
        ]),
    ];

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
        .or_else(|| hw.driver.pm_table_size.map(|sz| format!("0x{sz:X} ({sz} B)")))
        .unwrap_or_else(|| "—".to_string());
    let topo = format!(
        "{} CCD · {}C / {}T",
        hw.topology.ccd_count, hw.topology.physical_cores, hw.topology.logical_cpus
    );

    let rows = vec![
        kv_row("Codename", hw.codename.display_name().to_string(), palette::CYAN),
        kv_row("PM Version", pm_ver, palette::TEAL),
        kv_row("PM Size", pm_size, palette::FG),
        kv_row("SMU Firmware", hw.driver.smu_version.clone(), palette::FG),
        kv_row("Driver Version", hw.driver.drv_version.clone(), palette::FG),
        kv_row("MP1 IF Ver", format!("0x{:X}", hw.driver.mp1_if_version), palette::MUTED),
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
    let block = styled_block(" Curve Optimizer, Curve Shaper & Fused PBO ", palette::YELLOW)
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
        Line::from(vec![
            Span::styled(
                " PRIVILEGED MAILBOX REQUIRED ",
                Style::default()
                    .fg(palette::BADGE_FG)
                    .bg(palette::YELLOW)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            note,
            Style::default().fg(palette::FG).add_modifier(Modifier::BOLD),
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
                        Cell::from(format!("C{}", c.core_index))
                            .style(Style::default().fg(palette::FG).add_modifier(Modifier::BOLD)),
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
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {}", mailbox_status_label(other, &hw.driver.smu_version)),
                    Style::default().fg(palette::MUTED),
                ))),
                inner,
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
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(
                        " Not supported on {} (Zen 5 only)",
                        hw.codename.display_name()
                    ),
                    Style::default().fg(palette::DIM),
                )))
                .wrap(Wrap { trim: true }),
                inner,
            );
        }
        other => {
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {}", mailbox_status_label(other, &hw.driver.smu_version)),
                    Style::default().fg(palette::MUTED),
                ))),
                inner,
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
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {}", mailbox_status_label(other, &hw.driver.smu_version)),
                    Style::default().fg(palette::MUTED),
                ))),
                inner,
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
        TuneParam::ApuFast => "Fast Limit (W)".to_string(),
        TuneParam::ApuSlow => "Slow Limit (W)".to_string(),
        TuneParam::ApuApuSlow => "APU Slow (W)".to_string(),
        TuneParam::ApuSkinTemp => "APU Skin Temp (°C)".to_string(),
        TuneParam::ApuDgpuSkinTemp => "dGPU Skin Temp (°C)".to_string(),
        TuneParam::ApuPowerMode => "Power Mode".to_string(),
    }
}

fn format_tune_value(param: TuneParam, val: TuneValue) -> String {
    match (param, val) {
        (
            TuneParam::CoAll
            | TuneParam::CoCore(_)
            | TuneParam::CoIgpu
            | TuneParam::Shaper { .. },
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
        (TuneParam::PboTdc | TuneParam::PboEdc, TuneValue::Int(v)) => format!("{v} A"),
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

fn param_bounds_summary(app: &App, param: TuneParam, hw: &HardwareContext) -> String {
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
        TuneParam::ApuFast => format!(
            "{}..={} W (soft <= {} W)",
            apu::APU_POWER_MIN_W,
            apu::APU_POWER_MAX_W,
            apu::APU_FAST_POWER_SOFT_MAX_W
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
        let split = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(6), Constraint::Length(4)])
            .split(rows[1]);
        draw_tune_param_list(frame, split[0], app, hw);
        draw_tune_compact_bar(frame, split[1], app, hw);
    }
}

fn draw_tune_telemetry_strip(frame: &mut Frame, area: Rect, app: &App) {
    let mut title_spans = vec![Span::styled(
        " Live Telemetry ",
        Style::default()
            .fg(palette::CYAN)
            .add_modifier(Modifier::BOLD),
    )];
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
        title_spans.push(Span::styled(
            format!("[*{staged_n} STAGED] "),
            Style::default()
                .fg(palette::BADGE_FG)
                .bg(palette::ORANGE)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !app.can_write() {
        title_spans.push(Span::styled(
            "[READ-ONLY · sudo to write] ",
            Style::default().fg(palette::YELLOW),
        ));
    }

    let block = styled_block(Line::from(title_spans), palette::CYAN).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let snap = app.telemetry.as_ref();
    let ppt_v = snap.and_then(|s| s.ppt_value_w);
    let ppt_l = snap.and_then(|s| s.ppt_limit_w);
    let tdc_v = snap.and_then(|s| s.tdc_value_a);
    let tdc_l = snap.and_then(|s| s.tdc_limit_a);
    let edc_v = snap.and_then(|s| s.edc_value_a);
    let edc_l = snap.and_then(|s| s.edc_limit_a);
    let tctl_v = snap.and_then(|s| s.tctl_value_c);
    let tctl_l = snap.and_then(|s| s.tctl_limit_c);
    let fclk = snap.and_then(|s| s.fclk_mhz);
    let uclk = snap.and_then(|s| s.uclk_mhz);
    let mclk = snap.and_then(|s| s.mclk_mhz);
    let soc = snap.and_then(|s| s.vddcr_soc_v);

    let fmt_pair = |val: Option<f32>, lim: Option<f32>, unit: &str| -> String {
        match (val, lim) {
            (Some(v), Some(l)) if v.is_finite() && l.is_finite() => {
                format!("{v:.1}/{l:.0}{unit}")
            }
            (Some(v), _) if v.is_finite() => format!("{v:.1}/—{unit}"),
            _ => format!("—/—{unit}"),
        }
    };

    let line1 = Line::from(vec![
        Span::styled("PPT ", Style::default().fg(palette::MUTED)),
        Span::styled(
            fmt_pair(ppt_v, ppt_l, "W"),
            Style::default()
                .fg(ratio_color(gauge_ratio(ppt_v, ppt_l)))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · TDC ", Style::default().fg(palette::MUTED)),
        Span::styled(
            fmt_pair(tdc_v, tdc_l, "A"),
            Style::default()
                .fg(ratio_color(gauge_ratio(tdc_v, tdc_l)))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · EDC ", Style::default().fg(palette::MUTED)),
        Span::styled(
            fmt_pair(edc_v, edc_l, "A"),
            Style::default()
                .fg(ratio_color(gauge_ratio(edc_v, edc_l)))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · Tctl ", Style::default().fg(palette::MUTED)),
        Span::styled(
            fmt_pair(tctl_v, tctl_l, "°C"),
            Style::default()
                .fg(thermal_color(tctl_v, tctl_l))
                .add_modifier(Modifier::BOLD),
        ),
    ]);

    let line2 = Line::from(vec![
        Span::styled("FCLK ", Style::default().fg(palette::MUTED)),
        Span::styled(
            format_opt_f32(fclk, 0, ""),
            Style::default().fg(palette::TEAL).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" · UCLK ", Style::default().fg(palette::MUTED)),
        Span::styled(
            format_opt_f32(uclk, 0, ""),
            Style::default().fg(palette::CYAN).add_modifier(Modifier::BOLD),
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

fn draw_tune_param_list(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
    let tune_rows = app.build_tune_rows(hw);
    let param_count = tune_rows
        .iter()
        .filter(|r| matches!(r, TuneRow::Param(_)))
        .count();

    let title = format!(
        " Interactive SMU Controls ({} params · [/] Section · ←→/h/l Step) ",
        param_count
    );
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

    // Locate which row index corresponds to app.tune_selected
    let clamped_sel = if param_count == 0 {
        0
    } else {
        app.tune_selected.min(param_count - 1)
    };
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

    // Compute scroll window keeping the selected row (and its immediate section header) visible
    let scroll_top = if tune_rows.len() <= visible_rows {
        0
    } else {
        let half = visible_rows / 2;
        let desired = selected_row_idx.saturating_sub(half);
        desired.min(tune_rows.len().saturating_sub(visible_rows))
    };

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

                let live_style = match (*param, app.live_param_value(*param)) {
                    (
                        TuneParam::CoAll
                        | TuneParam::CoCore(_)
                        | TuneParam::CoIgpu
                        | TuneParam::Shaper { .. },
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
                    (TuneParam::PboTdc | TuneParam::PboEdc, Some(_)) => Style::default()
                        .fg(palette::TEAL)
                        .add_modifier(Modifier::BOLD),
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
                    (TuneParam::PboScalar, Some(_)) => Style::default()
                        .fg(palette::PURPLE)
                        .add_modifier(Modifier::BOLD),
                    (TuneParam::PboFmax, Some(_)) => Style::default()
                        .fg(palette::GREEN)
                        .add_modifier(Modifier::BOLD),
                    _ => Style::default().fg(palette::MUTED),
                };

                let staged_cell = if let Some(sv) = staged_val {
                    let exceeds = app.param_exceeds_soft_limit(*param, sv, hw);
                    let suffix = if exceeds && !app.allow_extreme {
                        " [!soft]"
                    } else if exceeds {
                        " [EXT]"
                    } else {
                        " *"
                    };
                    let col = if exceeds && !app.allow_extreme {
                        palette::RED
                    } else {
                        palette::ORANGE
                    };
                    Cell::from(format!("→ {}{suffix}", format_tune_value(*param, sv)))
                        .style(Style::default().fg(col).add_modifier(Modifier::BOLD))
                } else if is_selected {
                    Cell::from("←/→ adjust").style(Style::default().fg(palette::CYAN))
                } else {
                    Cell::from("·").style(Style::default().fg(palette::DIM))
                };

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
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(46), Constraint::Percentage(54)])
        .split(area);

    // Top: Selected parameter inspector
    let insp_block =
        styled_block(" Selected Parameter ", palette::CYAN).padding(Padding::horizontal(1));
    let insp_inner = insp_block.inner(chunks[0]);
    frame.render_widget(insp_block, chunks[0]);

    if let Some(param) = app.selected_tune_param(hw) {
        let label = format_tune_param_label(param, hw);
        let live = format_live_param_display(app, param);
        let staged = app
            .staged
            .get(&param)
            .map(|v| format_tune_value(param, *v))
            .unwrap_or_else(|| "(unchanged)".to_string());
        let (fine, coarse) = App::param_steps(param);
        let bounds = param_bounds_summary(app, param, hw);

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
                Span::styled(live, Style::default().fg(palette::FG).add_modifier(Modifier::BOLD)),
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
            Line::from(vec![
                Span::styled("Bounds:  ", Style::default().fg(palette::MUTED)),
                Span::styled(bounds, Style::default().fg(palette::FG)),
            ]),
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

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), insp_inner);
    }

    // Bottom: Safety & Quick Actions
    let act_block =
        styled_block(" Safety & Quick Actions ", palette::PURPLE).padding(Padding::horizontal(1));
    let act_inner = act_block.inner(chunks[1]);
    frame.render_widget(act_block, chunks[1]);

    let extreme_span = if app.allow_extreme {
        Span::styled(
            " ON (hard bounds & 95°C Tctl active) ",
            Style::default()
                .fg(palette::BADGE_FG)
                .bg(palette::ORANGE)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(" OFF (conservative soft bounds)", Style::default().fg(palette::GREEN))
    };

    let write_span = if app.can_write() {
        Span::styled("READY (root)", Style::default().fg(palette::GREEN).add_modifier(Modifier::BOLD))
    } else {
        Span::styled("LOCKED (run with sudo)", Style::default().fg(palette::YELLOW))
    };

    let action_lines = vec![
        Line::from(vec![
            Span::styled("Writes:  ", Style::default().fg(palette::MUTED)),
            write_span,
        ]),
        Line::from(vec![
            Span::styled("Extreme: ", Style::default().fg(palette::MUTED)),
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
        Line::from(""),
        tune_key_line("a / Enter", "Dry-run preview & confirm apply"),
        tune_key_line("0 / Bksp", "Reset param to 0 (CO/Shp) / live"),
        tune_key_line("u", "Undo / discard all staged changes"),
        tune_key_line("x", "Toggle Allow Extreme"),
        tune_key_line("R", "Preview Rollback (last-state.json)"),
        tune_key_line("B", "Preview Boot Baseline reset"),
        tune_key_line("Z", "Preview Reset CO to 0"),
        tune_key_line("s", "Save live state to TOML profile"),
    ];

    frame.render_widget(Paragraph::new(action_lines).wrap(Wrap { trim: true }), act_inner);
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

    let lines = vec![
        Line::from(Span::styled(sel_info, Style::default().fg(palette::CYAN))),
        Line::from(vec![
            Span::styled("a/Ent", Style::default().fg(palette::GREEN).add_modifier(Modifier::BOLD)),
            Span::styled(":DryRun ", Style::default().fg(palette::MUTED)),
            Span::styled("u", Style::default().fg(palette::CYAN).add_modifier(Modifier::BOLD)),
            Span::styled(":Undo ", Style::default().fg(palette::MUTED)),
            Span::styled("x", Style::default().fg(palette::ORANGE).add_modifier(Modifier::BOLD)),
            Span::styled(
                if app.allow_extreme { ":Ext[ON] " } else { ":Ext " },
                Style::default().fg(if app.allow_extreme { palette::ORANGE } else { palette::MUTED }),
            ),
            Span::styled("R/B/Z", Style::default().fg(palette::YELLOW).add_modifier(Modifier::BOLD)),
            Span::styled(":Roll/Boot/CO=0 ", Style::default().fg(palette::MUTED)),
            Span::styled("s", Style::default().fg(palette::TEAL).add_modifier(Modifier::BOLD)),
            Span::styled(":Save", Style::default().fg(palette::MUTED)),
        ]),
    ];

    frame.render_widget(Paragraph::new(lines), inner);
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

    let block = Block::default()
        .title(modal.title.as_str())
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(border_color))
        .title_style(
            Style::default()
                .fg(border_color)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(palette::POPUP_BG))
        .padding(Padding::horizontal(1));
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

    // Subtitle + warning badges
    let mut sub_spans = vec![Span::styled(
        modal.subtitle.as_str(),
        Style::default().fg(palette::FG),
    )];
    if modal.allow_extreme {
        sub_spans.push(Span::raw(" "));
        sub_spans.push(Span::styled(
            " EXTREME ",
            Style::default()
                .fg(palette::BADGE_FG)
                .bg(palette::ORANGE)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if !app.can_write() {
        sub_spans.push(Span::raw(" "));
        sub_spans.push(Span::styled(
            " UNPRIVILEGED ",
            Style::default()
                .fg(palette::BADGE_FG)
                .bg(palette::YELLOW)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let sub_lines = vec![
        Line::from(sub_spans),
        Line::from(Span::styled(
            "Pre-change snapshot saved before writes · flock + SMN RSP + readback verification",
            Style::default().fg(palette::DIM),
        )),
    ];
    frame.render_widget(Paragraph::new(sub_lines), chunks[0]);

    // PlannedOps table (scrollable)
    let visible_ops = chunks[1].height.saturating_sub(1).max(1) as usize;
    let start = modal
        .scroll
        .min(modal.ops.len().saturating_sub(1));

    let op_rows: Vec<Row<'_>> = modal
        .ops
        .iter()
        .enumerate()
        .skip(start)
        .take(visible_ops)
        .map(|(idx, op)| {
            let cur = op.current.as_deref().unwrap_or("—");
            let change = format!("{cur} → {}", op.target);
            Row::new(vec![
                Cell::from(format!("{}", idx + 1)).style(Style::default().fg(palette::DIM)),
                Cell::from(op.mailbox.as_str()).style(Style::default().fg(palette::TEAL)),
                Cell::from(format!("0x{:02X}", op.opcode))
                    .style(Style::default().fg(palette::CYAN).add_modifier(Modifier::BOLD)),
                Cell::from(format!("0x{:08X}", op.args[0])).style(Style::default().fg(palette::MUTED)),
                Cell::from(change)
                    .style(Style::default().fg(palette::ORANGE).add_modifier(Modifier::BOLD)),
                Cell::from(op.description.as_str()).style(Style::default().fg(palette::FG)),
            ])
        })
        .collect();

    let header = Row::new(vec!["#", "MB", "Opcode", "Arg0", "Current → Target", "Description"])
        .style(
            Style::default()
                .fg(palette::MUTED)
                .add_modifier(Modifier::BOLD),
        );

    let widths = if chunks[1].width < 72 {
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
    frame.render_widget(table, chunks[1]);

    // Footer action prompts
    let scroll_hint = if modal.ops.len() > visible_ops {
        format!(
            "  [{}-{}/{} · ↑/↓ Scroll]",
            start + 1,
            (start + visible_ops).min(modal.ops.len()),
            modal.ops.len()
        )
    } else {
        String::new()
    };

    let actions = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                " [y / Enter] Confirm & Apply ",
                Style::default()
                    .fg(palette::BADGE_FG)
                    .bg(if app.can_write() {
                        palette::GREEN
                    } else {
                        palette::YELLOW
                    })
                    .add_modifier(Modifier::BOLD),
            ),
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
    frame.render_widget(
        Paragraph::new(actions).alignment(Alignment::Center),
        chunks[2],
    );
}

// ---------------------------------------------------------------------------
// Help Modal Overlay
// ---------------------------------------------------------------------------

fn draw_help_popup(frame: &mut Frame, root: Rect) {
    let pct_x = if root.width < 92 { 92 } else { 74 };
    let pct_y = if root.height < 28 { 90 } else { 82 };
    let popup = centered_rect(pct_x, pct_y, root);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(" zensmu top — Help & Keybindings ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette::CYAN))
        .title_style(
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        )
        .style(Style::default().bg(palette::POPUP_BG))
        .padding(Padding::horizontal(1));

    let help_lines = vec![
        Line::from(Span::styled(
            "Navigation & Telemetry",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        )),
        help_entry("1 / 2 / 3 / 4", "Switch to Overview, Clocks & Volts, CO & Shaper, or Tune"),
        help_entry("t", "Jump straight to [4:Tune] from any tab"),
        help_entry("Tab / Shift+Tab", "Cycle forward / backward through views"),
        help_entry("p / Space", "Pause or resume live PM table polling"),
        help_entry("r", "Force refresh of PM telemetry and SMU mailbox readback"),
        help_entry("+ / -", "Increase or decrease polling interval (100ms..10000ms)"),
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
        help_entry("H / L / Shift+←→", "Coarse step decrement / increment (or PgDn / PgUp)"),
        help_entry("0 / Backspace", "Reset selected param to 0 (CO/Shaper) or live value"),
        help_entry("u", "Undo / discard all staged changes"),
        help_entry("x", "Toggle Allow Extreme (hard bounds & 95°C Tctl always enforced)"),
        help_entry("a / Enter", "Run dry-run validation & open Confirmation Modal"),
        help_entry("R / B / Z", "Preview Rollback (last-state), Boot Baseline reset, or CO=0"),
        help_entry("s", "Save live SMU state to TOML profile"),
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
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
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
        assert_eq!(format_clock_ratio(Some(3000.0), Some(3000.0)), Some("1:1 sync"));
        assert_eq!(format_clock_ratio(Some(1500.0), Some(3000.0)), Some("1:2 div"));
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
        assert_eq!(app.staged.get(&TuneParam::CoAll), Some(&TuneValue::Int(-20)));

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
        assert_eq!(app.staged.get(&TuneParam::PboPpt), Some(&TuneValue::Int(138)));
        assert_eq!(app.staged_count(), 2);

        // Press 'a' to open Dry-Run Confirmation Modal (must NOT write to hardware yet)
        app.handle_key(press(KeyCode::Char('a')), &transport, &hw);
        let modal = app.confirm_modal.as_ref().expect("confirm modal should open");
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
        assert_eq!(app.staged.get(&TuneParam::CoAll), Some(&TuneValue::Int(-40)));
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
            assert!(modal_text.contains("→"), "expected current→target arrow at {w}x{h}");
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
            !params
                .iter()
                .any(|p| matches!(p, TuneParam::Shaper { .. })),
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
        let mut driver = transport.read_info().unwrap();
        driver.codename_id = Codename::Phoenix as u32;
        let hw = HardwareContext {
            driver,
            codename: Codename::Phoenix,
            cpuid: None,
            topology: CpuTopology::synthetic(1, 8, 2),
            capabilities: capabilities_for(Codename::Phoenix).unwrap(),
        };

        let mut app = App::new(500);
        app.active_tab = ViewTab::Tune;
        let params = app.tune_params(&hw);
        for expected in [
            TuneParam::ApuStapm,
            TuneParam::ApuFast,
            TuneParam::ApuSlow,
            TuneParam::ApuApuSlow,
            TuneParam::ApuSkinTemp,
            TuneParam::ApuDgpuSkinTemp,
            TuneParam::ApuPowerMode,
        ] {
            assert!(params.contains(&expected), "missing APU control {expected:?}");
        }

        // Jump to APU section and render at both sizes
        app.tune_selected = params
            .iter()
            .position(|p| *p == TuneParam::ApuStapm)
            .unwrap();
        for (w, h) in [(120u16, 36u16), (80, 24)] {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal.draw(|f| draw_ui(f, &app, &hw)).unwrap();
        }
    }
}
