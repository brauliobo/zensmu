//! Tab 2: Clocks, voltages, voltage rails table, and PM metadata card.

use super::super::app::App;
use super::super::format::{
    format_opt_f32, gauge_ratio, ratio_color, soc_voltage_color, styled_block, thermal_color,
};
use super::super::history::HistoryBuffer;
use super::super::palette;
use super::{draw_metric_sparkline, kv_row};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Cell, Row, Table};
use ratatui::Frame;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::ops::pm_table::TelemetrySnapshot;

// ---------------------------------------------------------------------------
// Tab 2: Clocks & Voltages Detail
// ---------------------------------------------------------------------------

pub(super) fn draw_clocks_volts_tab(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    hw: &HardwareContext,
) {
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

pub(super) fn format_history_stats(history: &HistoryBuffer, decimals: usize, unit: &str) -> String {
    history
        .stats()
        .map(|(mn, av, mx)| format!("{mn:.decimals$} / {av:.decimals$} / {mx:.decimals$} {unit}"))
        .unwrap_or_else(|| "—".to_string())
}

pub(super) fn push_apu_rail_rows<'a>(rows: &mut Vec<Row<'a>>, snap: &TelemetrySnapshot) {
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

pub(super) fn draw_voltage_rails_table(frame: &mut Frame, area: Rect, app: &App) {
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

pub(super) fn draw_pm_metadata_card(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    hw: &HardwareContext,
) {
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
