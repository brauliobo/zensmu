//! Tab 1: Overview (`btop`-style gauges, sparklines, and tuning summary).

use super::super::app::App;
use super::super::format::{
    badge_span, format_clock_ratio, format_gauge_readout, format_opt_f32, format_opt_u32,
    format_signed_margin, gauge_ratio, mailbox_status_label, margin_color, ratio_color,
    styled_block, thermal_color,
};
use super::super::palette;
use super::super::types::MailboxState;
use super::draw_metric_sparkline;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, LineGauge, Padding, Paragraph, Row, Table, Wrap};
use ratatui::Frame;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::types::CurveShaperState;

// ---------------------------------------------------------------------------
// Tab 1: Overview (btop style)
// ---------------------------------------------------------------------------

pub(super) fn has_apu_telemetry(snap: Option<&TelemetrySnapshot>) -> bool {
    snap.is_some_and(crate::output::has_apu_telemetry)
}

pub(super) fn draw_overview_tab(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
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

pub(super) struct GaugeSpec<'a> {
    label: &'a str,
    value: Option<f32>,
    limit: Option<f32>,
    unit: &'a str,
    is_thermal: bool,
}

pub(super) fn render_gauge_line(frame: &mut Frame, area: Rect, label_w: u16, spec: &GaugeSpec<'_>) {
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

pub(super) fn render_gauge_column(
    frame: &mut Frame,
    area: Rect,
    label_w: u16,
    specs: &[GaugeSpec<'_>],
) {
    let row_count = specs.len().min(area.height as usize);
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(vec![Constraint::Length(1); row_count])
        .split(area);
    for (spec, &row_area) in specs.iter().zip(rows.iter()) {
        render_gauge_line(frame, row_area, label_w, spec);
    }
}

pub(super) fn push_optional_gauge<'a>(
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

pub(super) fn draw_power_thermal_gauges(frame: &mut Frame, area: Rect, app: &App) {
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

pub(super) fn draw_clocks_voltages_summary(frame: &mut Frame, area: Rect, app: &App) {
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

pub(super) fn draw_co_pbo_summary(frame: &mut Frame, area: Rect, app: &App, hw: &HardwareContext) {
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

pub(super) fn draw_locked_mailbox_banner(frame: &mut Frame, area: Rect, note: &str) {
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

pub(super) fn build_pbo_summary_line<'a>(app: &'a App, hw: &'a HardwareContext) -> Line<'a> {
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

pub(super) fn build_co_summary_lines<'a>(
    app: &'a App,
    hw: &'a HardwareContext,
    lines: &mut Vec<Line<'a>>,
) {
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

pub(super) fn build_shaper_summary_line<'a>(app: &'a App, hw: &'a HardwareContext) -> Line<'a> {
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

pub(super) fn flag_pill(name: &str, state: Option<bool>) -> Span<'static> {
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

pub(super) fn shaper_all_values(s: &CurveShaperState) -> [i8; 15] {
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
