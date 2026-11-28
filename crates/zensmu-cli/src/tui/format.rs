//! Pure TUI helper functions for colors, ratios, formatting, and widget frames.

use super::palette;
use super::types::MailboxState;
use super::{MAX_INTERVAL_MS, MIN_INTERVAL_MS};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Padding, Paragraph, Wrap};
use ratatui::Frame;
use std::borrow::Cow;
use zensmu_core::hw::identity::HardwareContext;

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

pub(super) fn cpu_display_title(hw: &HardwareContext) -> String {
    if let Some(ref cpuid) = hw.cpuid {
        let brand = cpuid.brand.trim();
        if !brand.is_empty() {
            return format!("{brand} · {}", hw.codename.display_name());
        }
    }
    hw.codename.display_name().to_string()
}

pub(super) fn styled_block<'a>(title: impl Into<Line<'a>>, accent: Color) -> Block<'a> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(palette::BORDER))
        .title_style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
}

pub(super) fn modal_frame_block<'a>(title: &'a str, accent: Color) -> Block<'a> {
    Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .title_style(Style::default().fg(accent).add_modifier(Modifier::BOLD))
        .style(Style::default().bg(palette::POPUP_BG))
        .padding(Padding::horizontal(1))
}

pub(super) fn badge_span<'a>(label: impl Into<Cow<'a, str>>, bg: Color) -> Span<'a> {
    Span::styled(
        label,
        Style::default()
            .fg(palette::BADGE_FG)
            .bg(bg)
            .add_modifier(Modifier::BOLD),
    )
}

pub(super) fn render_mailbox_placeholder(
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

pub(super) fn mailbox_status_label<T>(state: &MailboxState<T>, smu_ver: &str) -> String {
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

pub(super) fn soc_voltage_color(v: Option<f32>) -> Color {
    match v {
        Some(val) if val >= 1.30 => palette::RED,
        Some(val) if val >= 1.25 => palette::YELLOW,
        Some(_) => palette::GREEN,
        None => palette::DIM,
    }
}
