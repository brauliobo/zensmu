//! Live SMU telemetry dashboard tab.

mod gauges;
mod recorder;

use crate::app::ZensmuApp;
use crate::views::theme::*;
use crate::views::widgets::{
    card_frame, cta_button, render_tab_header, split_or_wrap_row, tinted_pill,
};
use eframe::egui::{self, Color32, RichText, Ui};
use egui_plot::PlotPoints;
use gauges::{
    metric_row, render_apu_limit_row, render_gauge_card, render_rolling_plot_card, RollingPlotSpec,
};
use recorder::render_dashboard_recorder_strip;

/// Tab 1: Live Dashboard with rolling plots, limit progress bars, clocks, and voltage rails.
pub fn render_dashboard(app: &mut ZensmuApp, ui: &mut Ui) {
    render_tab_header(
        ui,
        "Live SMU Telemetry Dashboard",
        "Unprivileged 0444 PM table polling from /sys/kernel/ryzen_smu_drv/pm_table",
    );

    if let Some(err) = app.telemetry_error.clone() {
        let lower = err.to_ascii_lowercase();
        let is_unsupported_pm = app.telemetry_error_suggests_report
            || lower.contains("unsupported pm table")
            || lower.contains("unsupported");
        let hint = app.telemetry_error_hint;
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("PM Table Telemetry Unavailable")
                    .strong()
                    .color(ACCENT_AMBER),
            );
            ui.label(&err);
            ui.label(
                RichText::new(
                    hint.unwrap_or(
                        "zensmu strictly refuses to guess unverified PM table layouts or read tables exceeding 4096 bytes.",
                    ),
                )
                .small()
                .color(TEXT_MUTED),
            );
            ui.add_space(6.0);
            if ui
                .add(cta_button("Report on GitHub", CTA_EMERALD))
                .on_hover_text(
                    "Collect privacy-safe CPU & non-zero PM table diagnostics to add support",
                )
                .clicked()
            {
                if is_unsupported_pm {
                    app.open_report_modal_for_unsupported();
                } else {
                    app.open_report_modal();
                }
            }
        });
        ui.add_space(12.0);
    }

    let snap = app.latest_telemetry.as_ref();

    // Row 1: Power / Thermal / Current Limit Gauges (4 columns wide, or 2×2 when narrow/zoomed)
    let gauge_specs = [
        (
            "PPT Power",
            snap.and_then(|s| s.ppt_value_w),
            snap.and_then(|s| s.ppt_limit_w),
            "W",
            ACCENT_CYAN,
        ),
        (
            "Tctl Temp",
            snap.and_then(|s| s.tctl_value_c),
            snap.and_then(|s| s.tctl_limit_c),
            "°C",
            ACCENT_AMBER,
        ),
        (
            "TDC Current",
            snap.and_then(|s| s.tdc_value_a),
            snap.and_then(|s| s.tdc_limit_a),
            "A",
            ACCENT_EMERALD,
        ),
        (
            "EDC Current",
            snap.and_then(|s| s.edc_value_a),
            snap.and_then(|s| s.edc_limit_a),
            "A",
            ACCENT_VIOLET,
        ),
    ];
    if ui.available_width() >= 680.0 {
        ui.columns(4, |cols| {
            for (idx, (title, value, limit, unit, accent)) in gauge_specs.iter().enumerate() {
                render_gauge_card(&mut cols[idx], title, *value, *limit, unit, *accent);
            }
        });
    } else {
        for pair in gauge_specs.chunks(2) {
            ui.columns(2, |cols| {
                for (idx, (title, value, limit, unit, accent)) in pair.iter().enumerate() {
                    render_gauge_card(&mut cols[idx], title, *value, *limit, unit, *accent);
                }
            });
            ui.add_space(8.0);
        }
    }

    // Optional Row 1b: APU DPTC Limits card (only shown when APU telemetry fields are present)
    if let Some(s) = snap {
        struct ApuMetricRow<'a> {
            label: &'a str,
            value: Option<f32>,
            limit: Option<f32>,
            unit: &'a str,
            accent: Color32,
        }

        let apu_metrics = [
            ApuMetricRow {
                label: "STAPM Power",
                value: s.stapm_value_w,
                limit: s.stapm_limit_w,
                unit: "W",
                accent: ACCENT_CYAN,
            },
            ApuMetricRow {
                label: "Slow PPT",
                value: s.slow_value_w,
                limit: s.slow_limit_w,
                unit: "W",
                accent: ACCENT_CYAN,
            },
            ApuMetricRow {
                label: "APU Slow",
                value: s.apu_slow_value_w,
                limit: s.apu_slow_limit_w,
                unit: "W",
                accent: ACCENT_CYAN,
            },
            ApuMetricRow {
                label: "SoC TDC",
                value: s.tdc_soc_value_a,
                limit: s.tdc_soc_limit_a,
                unit: "A",
                accent: ACCENT_EMERALD,
            },
            ApuMetricRow {
                label: "SoC EDC",
                value: s.edc_soc_value_a,
                limit: s.edc_soc_limit_a,
                unit: "A",
                accent: ACCENT_VIOLET,
            },
            ApuMetricRow {
                label: "APU Skin Temp",
                value: s.apu_skin_temp_value_c,
                limit: s.apu_skin_temp_limit_c,
                unit: "°C",
                accent: ACCENT_AMBER,
            },
            ApuMetricRow {
                label: "dGPU Skin Temp",
                value: s.dgpu_skin_temp_value_c,
                limit: s.dgpu_skin_temp_limit_c,
                unit: "°C",
                accent: ACCENT_AMBER,
            },
        ];

        let present_apu: Vec<_> = apu_metrics
            .into_iter()
            .filter(|m| m.limit.is_some() || m.value.is_some())
            .collect();

        if !present_apu.is_empty() {
            ui.add_space(12.0);
            card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("APU DPTC Limits").strong());
                    tinted_pill(
                        ui,
                        "Mobile Power & Thermal Telemetry",
                        ACCENT_CYAN,
                        PILL_ALPHA_CYAN,
                    );
                });
                ui.add_space(8.0);

                egui::Grid::new("dashboard_apu_dptc_grid")
                    .num_columns(3)
                    .striped(true)
                    .spacing([14.0, 8.0])
                    .show(ui, |ui| {
                        for m in present_apu {
                            render_apu_limit_row(ui, m.label, m.value, m.limit, m.unit, m.accent);
                        }
                    });
            });
        }
    }

    ui.add_space(12.0);

    // Row 2: Live rolling time-series plots (PPT W and Tctl C)
    let latest_t = app.samples.back().map(|s| s.elapsed_s).unwrap_or(0.0);

    let ppt_points: PlotPoints<'_> = app
        .samples
        .iter()
        .filter_map(|s| {
            s.snapshot
                .ppt_value_w
                .map(|v| [s.elapsed_s - latest_t, v as f64])
        })
        .collect();
    let tctl_points: PlotPoints<'_> = app
        .samples
        .iter()
        .filter_map(|s| {
            s.snapshot
                .tctl_value_c
                .map(|v| [s.elapsed_s - latest_t, v as f64])
        })
        .collect();

    let ppt_limit = snap.and_then(|s| s.ppt_limit_w).map(f64::from);
    let tctl_limit = snap.and_then(|s| s.tctl_limit_c).map(f64::from);

    ui.columns(2, |cols| {
        render_rolling_plot_card(
            &mut cols[0],
            RollingPlotSpec {
                plot_id: "ppt_plot",
                title: "Package Power (PPT)",
                series_name: "PPT (W)",
                limit_name: "PPT Limit",
                unit: "W",
                accent: ACCENT_CYAN,
                current: snap.and_then(|s| s.ppt_value_w),
                limit: ppt_limit,
                include_y: &[0.0],
                points: ppt_points,
            },
        );

        render_rolling_plot_card(
            &mut cols[1],
            RollingPlotSpec {
                plot_id: "tctl_plot",
                title: "CPU Temperature (Tctl)",
                series_name: "Tctl (°C)",
                limit_name: "Tctl Limit",
                unit: "°C",
                accent: ACCENT_AMBER,
                current: snap.and_then(|s| s.tctl_value_c),
                limit: tctl_limit,
                include_y: &[20.0, 100.0],
                points: tctl_points,
            },
        );
    });

    ui.add_space(12.0);

    // Row 3: Clocks & Voltage Rails cards
    ui.columns(2, |cols| {
        card_frame().show(&mut cols[0], |ui| {
            ui.set_width(ui.available_width());
            split_or_wrap_row(
                ui,
                "clocks_card_header",
                |ui| {
                    ui.label(RichText::new("Fabric & Memory Clocks").strong());
                },
                |ui| {
                    if let (Some(uclk), Some(mclk)) =
                        (snap.and_then(|s| s.uclk_mhz), snap.and_then(|s| s.mclk_mhz))
                    {
                        let ratio = if (uclk - mclk).abs() < 25.0 {
                            "UCLK:MCLK 1:1"
                        } else if (uclk * 2.0 - mclk).abs() < 50.0 {
                            "UCLK:MCLK 1:2"
                        } else {
                            "Async"
                        };
                        tinted_pill(ui, ratio, ACCENT_CYAN, PILL_ALPHA_CYAN);
                    }
                },
            );
            ui.add_space(8.0);
            egui::Grid::new("clocks_grid")
                .num_columns(2)
                .striped(true)
                .min_col_width(180.0)
                .show(ui, |ui| {
                    metric_row(
                        ui,
                        "FCLK (Infinity Fabric)",
                        snap.and_then(|s| s.fclk_mhz),
                        "MHz",
                        0,
                    );
                    metric_row(
                        ui,
                        "UCLK (Memory Controller)",
                        snap.and_then(|s| s.uclk_mhz),
                        "MHz",
                        0,
                    );
                    metric_row(
                        ui,
                        "MCLK (Memory Clock)",
                        snap.and_then(|s| s.mclk_mhz),
                        "MHz",
                        0,
                    );
                });
        });

        card_frame().show(&mut cols[1], |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Voltage Rails (Read-Only)").strong());
            ui.add_space(8.0);
            egui::Grid::new("voltages_grid")
                .num_columns(2)
                .striped(true)
                .min_col_width(180.0)
                .show(ui, |ui| {
                    metric_row(ui, "VDDCR_SOC", snap.and_then(|s| s.vddcr_soc_v), "V", 3);
                    metric_row(ui, "VDDCR_MISC", snap.and_then(|s| s.vddcr_misc_v), "V", 3);
                    metric_row(ui, "CLDO_VDDP", snap.and_then(|s| s.cldo_vddp_v), "V", 3);
                    metric_row(ui, "VDDG IOD", snap.and_then(|s| s.cldo_vddg_iod_v), "V", 3);
                    metric_row(ui, "VDDG CCD", snap.and_then(|s| s.cldo_vddg_ccd_v), "V", 3);
                });
        });
    });

    ui.add_space(12.0);
    render_dashboard_recorder_strip(app, ui);
}
