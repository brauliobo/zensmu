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
use std::collections::VecDeque;
use std::io::{self, Stdout, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};
use zensmu_core::error::Error;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::ops::co::{self, CoSnapshot};
use zensmu_core::ops::pbo::{self, PboStatus};
use zensmu_core::ops::pm_table::{self, TelemetrySnapshot};
use zensmu_core::ops::shaper;
use zensmu_core::transport::SmuTransport;
use zensmu_core::types::{CurveShaperState, FreqTier};

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
// View state & mailbox load tracking
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewTab {
    Overview,
    ClocksVolts,
    CoShaper,
}

impl ViewTab {
    fn next(self) -> Self {
        match self {
            Self::Overview => Self::ClocksVolts,
            Self::ClocksVolts => Self::CoShaper,
            Self::CoShaper => Self::Overview,
        }
    }

    fn prev(self) -> Self {
        match self {
            Self::Overview => Self::CoShaper,
            Self::ClocksVolts => Self::Overview,
            Self::CoShaper => Self::ClocksVolts,
        }
    }
}

#[derive(Debug, Clone)]
enum MailboxState<T> {
    Loaded(T),
    PermissionDenied,
    Unsupported,
    Error(String),
}

impl<T> MailboxState<T> {
    fn from_result(res: zensmu_core::Result<T>) -> Self {
        match res {
            Ok(v) => Self::Loaded(v),
            Err(Error::PermissionDenied { .. }) => Self::PermissionDenied,
            Err(Error::UnsupportedCapability { .. }) => Self::Unsupported,
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
    telemetry: Option<TelemetrySnapshot>,
    telemetry_error: Option<String>,
    history: TelemetryHistory,
    co_state: MailboxState<CoSnapshot>,
    shaper_state: MailboxState<CurveShaperState>,
    pbo_state: MailboxState<PboStatus>,
    mailbox_note: Option<String>,
    status_flash: Option<(String, Instant)>,
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
            telemetry: None,
            telemetry_error: None,
            history: TelemetryHistory::new(HISTORY_CAPACITY),
            co_state: MailboxState::Unsupported,
            shaper_state: MailboxState::Unsupported,
            pbo_state: MailboxState::Unsupported,
            mailbox_note: None,
            status_flash: None,
        }
    }

    fn set_flash(&mut self, msg: impl Into<String>) {
        self.status_flash = Some((msg.into(), Instant::now()));
    }

    fn active_flash(&self) -> Option<&str> {
        let (msg, ts) = self.status_flash.as_ref()?;
        if ts.elapsed() < Duration::from_secs(3) {
            Some(msg.as_str())
        } else {
            None
        }
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
    }

    fn has_any_mailbox_data(&self) -> bool {
        self.co_state.as_loaded().is_some()
            || self.shaper_state.as_loaded().is_some()
            || self.pbo_state.as_loaded().is_some()
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

        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => return true,
            KeyCode::Char('?') => {
                self.show_help = !self.show_help;
            }
            KeyCode::Esc => {
                if self.show_help {
                    self.show_help = false;
                }
            }
            KeyCode::Tab | KeyCode::Right | KeyCode::Char('l') => {
                self.show_help = false;
                self.active_tab = self.active_tab.next();
            }
            KeyCode::BackTab | KeyCode::Left | KeyCode::Char('h') => {
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
            KeyCode::Char('p') | KeyCode::Char('P') | KeyCode::Char(' ') => {
                self.paused = !self.paused;
                self.set_flash(if self.paused {
                    "Telemetry paused"
                } else {
                    "Telemetry resumed"
                });
            }
            KeyCode::Char('r') | KeyCode::Char('R') => {
                self.poll_telemetry(transport, hw);
                self.refresh_mailbox(transport, hw);
                if self.mailbox_note.is_some() {
                    self.set_flash("Telemetry refreshed (SMU mailbox requires sudo)");
                } else {
                    self.set_flash("SMU mailbox & telemetry refreshed");
                }
            }
            KeyCode::Char('+') | KeyCode::Char('=') => {
                self.interval_ms = adjust_interval_ms(self.interval_ms, true);
                self.set_flash(format!("Interval: {}ms", self.interval_ms));
            }
            KeyCode::Char('-') | KeyCode::Char('_') => {
                self.interval_ms = adjust_interval_ms(self.interval_ms, false);
                self.set_flash(format!("Interval: {}ms", self.interval_ms));
            }
            _ => {}
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
    }

    draw_footer(frame, chunks[2], app);

    if app.show_help {
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

    let row1_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(20), Constraint::Length(56)])
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

    let tabs_line = Line::from(vec![
        tab_span("[1:Overview]", app.active_tab == ViewTab::Overview),
        Span::raw(" "),
        tab_span("[2:Clocks & Volts]", app.active_tab == ViewTab::ClocksVolts),
        Span::raw(" "),
        tab_span("[3:CO & Shaper]", app.active_tab == ViewTab::CoShaper),
        Span::raw(" "),
        tab_span("[?:Help]", app.show_help),
    ]);

    let row2_right = if let Some(ref err) = app.telemetry_error {
        Line::from(vec![Span::styled(
            format!("PM err: {err}"),
            Style::default().fg(palette::RED),
        )])
    } else if app.mailbox_note.is_some() {
        Line::from(vec![Span::styled(
            "PM telemetry live · sudo for mailbox",
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

    let row2_split = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(40), Constraint::Length(38)])
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
            Span::styled(format!(":{desc}  "), Style::default().fg(palette::MUTED)),
        ]
    };

    let mut left = vec![Span::raw(" ")];
    left.extend(keycap("q", "Quit"));
    left.extend(keycap("Tab/1-3", "Views"));
    left.extend(keycap("p", "Pause"));
    left.extend(keycap("r", "Refresh SMU"));
    left.extend(keycap("+/-", "Interval"));
    left.extend(keycap("?", "Help"));

    let right_text = if let Some(flash) = app.active_flash() {
        Line::from(vec![
            Span::styled(
                flash,
                Style::default()
                    .fg(palette::TEAL)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                format!("hist {}s ", app.history.ppt_w.samples.len()),
                Style::default().fg(palette::DIM),
            ),
        ])
    };

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(40), Constraint::Length(38)])
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
                mailbox_status_label(&app.pbo_state),
                Style::default().fg(palette::DIM),
            ),
        ]));
    }

    // Per-core CO compact pills
    match &app.co_state {
        MailboxState::Loaded(co_snap) => {
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
        other => {
            lines.push(Line::from(vec![
                Span::styled("CO: ", Style::default().fg(palette::MUTED)),
                Span::styled(
                    mailbox_status_label(other),
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
                    mailbox_status_label(other),
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

fn mailbox_status_label<T>(state: &MailboxState<T>) -> String {
    match state {
        MailboxState::Loaded(_) => "loaded".to_string(),
        MailboxState::PermissionDenied => "requires sudo".to_string(),
        MailboxState::Unsupported => "unsupported on this CPU".to_string(),
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
    draw_fused_pbo_table(frame, right[1], app);
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
                    format!(" {}", mailbox_status_label(other)),
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
                    format!(" {}", mailbox_status_label(other)),
                    Style::default().fg(palette::MUTED),
                ))),
                inner,
            );
        }
    }
}

fn draw_fused_pbo_table(frame: &mut Frame, area: Rect, app: &App) {
    let block = styled_block(" Fused Factory PBO Limits & Status ", palette::PURPLE);

    match &app.pbo_state {
        MailboxState::Loaded(pbo) => {
            let flags_str = format!(
                "OC:{}  EXPO:{}  LN2:{}",
                fmt_bool_opt(pbo.flags.overclockable),
                fmt_bool_opt(pbo.flags.expo_active),
                fmt_bool_opt(pbo.flags.ln2_mode),
            );
            let rows = vec![
                kv_row(
                    "Fused PPT",
                    format_opt_u32(pbo.fused.ppt_w, "W"),
                    palette::CYAN,
                ),
                kv_row(
                    "Fused Slow / Fast",
                    format!(
                        "{} / {}",
                        format_opt_u32(pbo.fused.slow_w, "W"),
                        format_opt_u32(pbo.fused.fast_w, "W"),
                    ),
                    palette::FG,
                ),
                kv_row(
                    "Fused APU Slow",
                    format_opt_u32(pbo.fused.apu_slow_w, "W"),
                    palette::FG,
                ),
                kv_row(
                    "Fused VDD / SoC TDC",
                    format!(
                        "{} / {}",
                        format_opt_u32(pbo.fused.vdd_tdc_a, "A"),
                        format_opt_u32(pbo.fused.soc_tdc_a, "A"),
                    ),
                    palette::TEAL,
                ),
                kv_row(
                    "PBO Scalar / Fmax",
                    format!(
                        "{}  ·  {}",
                        format_opt_f32(pbo.scalar, 2, "x"),
                        format_opt_u32(pbo.fmax_mhz, "MHz"),
                    ),
                    palette::YELLOW,
                ),
                kv_row("Platform Flags", flags_str, palette::GREEN),
            ];

            let widths = [Constraint::Length(20), Constraint::Min(16)];
            let table = Table::new(rows, widths).block(block).column_spacing(1);
            frame.render_widget(table, area);
        }
        other => {
            let inner = block.inner(area);
            frame.render_widget(block, area);
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(" {}", mailbox_status_label(other)),
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
// Help Modal Overlay
// ---------------------------------------------------------------------------

fn draw_help_popup(frame: &mut Frame, root: Rect) {
    let popup = centered_rect(68, 72, root);
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
        .padding(Padding::uniform(1));

    let help_lines = vec![
        Line::from(Span::styled(
            "Navigation & Controls",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        )),
        help_entry("1 / 2 / 3", "Switch directly to Overview, Clocks & Volts, or CO & Shaper"),
        help_entry("Tab / Shift+Tab", "Cycle forward / backward through views"),
        help_entry("p / Space", "Pause or resume live PM table polling"),
        help_entry("r", "Force refresh of PM telemetry and SMU mailbox readback"),
        help_entry("+ / -", "Increase or decrease polling interval (100ms..10000ms)"),
        help_entry("? / Esc", "Toggle or close this help overlay"),
        help_entry("q / Ctrl+C", "Quit and cleanly restore terminal state"),
        Line::from(""),
        Line::from(Span::styled(
            "Color Legend",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("  Gauges:  ", Style::default().fg(palette::MUTED)),
            Span::styled("Green <65%", Style::default().fg(palette::GREEN)),
            Span::raw("  ·  "),
            Span::styled("Yellow 65-85%", Style::default().fg(palette::YELLOW)),
            Span::raw("  ·  "),
            Span::styled("Red >=85%", Style::default().fg(palette::RED)),
        ]),
        Line::from(vec![
            Span::styled("  CO/Shp:  ", Style::default().fg(palette::MUTED)),
            Span::styled("Cyan <=-35", Style::default().fg(palette::CYAN)),
            Span::raw("  ·  "),
            Span::styled("Green <0", Style::default().fg(palette::GREEN)),
            Span::raw("  ·  "),
            Span::styled("Dim 0", Style::default().fg(palette::DIM)),
            Span::raw("  ·  "),
            Span::styled("Yellow/Red >0", Style::default().fg(palette::YELLOW)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "Privilege Model",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  PM table power, thermal, clock, and voltage telemetry works unprivileged.",
            Style::default().fg(palette::FG),
        )),
        Line::from(Span::styled(
            "  Run with sudo for Curve Optimizer, Curve Shaper, and PBO mailbox readback.",
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
            format!("  {key:<16}"),
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

            for tab in [ViewTab::Overview, ViewTab::ClocksVolts, ViewTab::CoShaper] {
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
        for tab in [ViewTab::Overview, ViewTab::ClocksVolts, ViewTab::CoShaper] {
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
}
