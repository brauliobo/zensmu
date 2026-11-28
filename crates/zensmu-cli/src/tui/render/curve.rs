//! Tab 3: Curve Optimizer, Curve Shaper matrix, and fused PBO table.

use super::super::app::App;
use super::super::format::{
    badge_span, format_co_bar, format_linux_cpus, format_opt_f32, format_opt_u32,
    format_signed_margin, mailbox_status_label, margin_color, render_mailbox_placeholder,
    styled_block,
};
use super::super::palette;
use super::super::types::MailboxState;
use super::kv_row;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Padding, Paragraph, Row, Table, Wrap};
use ratatui::Frame;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::types::FreqTier;

// ---------------------------------------------------------------------------
// Tab 3: Curve Optimizer & Curve Shaper
// ---------------------------------------------------------------------------

pub(super) fn draw_co_shaper_tab(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
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

pub(super) fn draw_unprivileged_tab3(
    frame: &mut Frame,
    area: Rect,
    note: &str,
    hw: &HardwareContext,
) {
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

pub(super) fn draw_co_table(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
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
                    let rank = perf_scores
                        .iter()
                        .find(|(idx, _)| *idx == c.core_index)
                        .map(|(_, rank)| format!("#{rank}"))
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
                        Cell::from(rank).style(Style::default().fg(palette::TEAL)),
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
                "Rank",
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

pub(super) fn draw_shaper_matrix(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
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

pub(super) fn draw_fused_pbo_table(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
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

pub(super) fn fmt_bool_opt(b: Option<bool>) -> &'static str {
    match b {
        Some(true) => "ON",
        Some(false) => "OFF",
        None => "—",
    }
}
