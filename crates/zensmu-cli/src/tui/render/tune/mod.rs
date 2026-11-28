//! Tab 4: Interactive Tune layout, telemetry strip, parameter list, and compact bar.

mod format;
mod inspector;

#[cfg(test)]
pub(in crate::tui) use format::param_bounds_summary;

use super::super::app::App;
use super::super::format::{
    badge_span, format_opt_f32, gauge_ratio, ratio_color, soc_voltage_color, styled_block,
    thermal_color,
};
use super::super::palette;
use super::super::types::{FlashKind, TuneRow};
use format::{
    estimate_wrapped_lines, format_live_param_display, format_staged_cell, format_tune_param_label,
    locate_selected_row_and_scroll, param_bounds_summary as format_param_bounds_summary,
    tune_param_live_style,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Padding, Paragraph, Row, Table, Wrap};
use ratatui::Frame;
use zensmu_core::hw::identity::HardwareContext;

pub(super) fn draw_tune_tab(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
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
        inspector::draw_tune_inspector_pane(frame, cols[1], app, hw);
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

pub(super) fn build_tune_strip_title(app: &App) -> Line<'static> {
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

pub(super) fn push_telemetry_pair_spans(
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

pub(super) fn draw_tune_telemetry_strip(frame: &mut Frame, area: Rect, app: &App) {
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

pub(super) fn draw_tune_param_list(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
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

pub(super) fn draw_tune_compact_bar(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    hw: &HardwareContext,
) {
    let block = styled_block(" Inspector & Quick Actions ", palette::PURPLE)
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let sel_info = if let Some(p) = app.selected_tune_param(hw) {
        format!(
            "{} · {}",
            format_tune_param_label(p, hw),
            format_param_bounds_summary(app, p, hw)
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
