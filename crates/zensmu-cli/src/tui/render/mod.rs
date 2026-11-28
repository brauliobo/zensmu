//! Top-level TUI rendering, header/footer bars, and shared widget helpers.

mod clocks;
mod curve;
mod modal;
mod overview;
mod tune;

#[cfg(test)]
pub(super) use tune::param_bounds_summary;

use super::app::App;
use super::format::{badge_span, cpu_display_title, format_opt_f32, styled_block};
use super::history::HistoryBuffer;
use super::palette;
use super::types::{FlashKind, ViewTab};
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Padding, Paragraph, Row, Sparkline};
use ratatui::Frame;
use zensmu_core::hw::identity::HardwareContext;

// ---------------------------------------------------------------------------
// Top-level UI rendering
// ---------------------------------------------------------------------------

pub(super) fn draw_ui(frame: &mut Frame, app: &App, hw: &HardwareContext) {
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
        ViewTab::Overview => overview::draw_overview_tab(frame, chunks[1], app, hw),
        ViewTab::ClocksVolts => clocks::draw_clocks_volts_tab(frame, chunks[1], app, hw),
        ViewTab::CoShaper => curve::draw_co_shaper_tab(frame, chunks[1], app, hw),
        ViewTab::Tune => tune::draw_tune_tab(frame, chunks[1], app, hw),
    }

    draw_footer(frame, chunks[2], app);

    if let Some(ref modal) = app.confirm_modal {
        modal::draw_confirm_modal(frame, root, app, modal);
    } else if app.show_help {
        modal::draw_help_popup(frame, root);
    }
}

// ---------------------------------------------------------------------------
// Header & Footer
// ---------------------------------------------------------------------------

pub(super) fn draw_header(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
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

pub(super) fn draw_header_top_row(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
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

pub(super) fn draw_header_tabs_row(frame: &mut Frame, area: Rect, app: &App) {
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

pub(super) fn draw_footer(frame: &mut Frame, area: Rect, app: &App) {
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

pub(super) fn kv_row<'a>(k: &'a str, v: String, val_color: Color) -> Row<'a> {
    Row::new(vec![
        Cell::from(k).style(Style::default().fg(palette::MUTED)),
        Cell::from(v).style(Style::default().fg(val_color)),
    ])
}
