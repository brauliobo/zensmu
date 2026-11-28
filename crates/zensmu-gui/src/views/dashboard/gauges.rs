//! Rolling telemetry plots, utilization gauges, and APU limit bars.

use crate::views::theme::*;
use crate::views::widgets::{card_frame, split_or_wrap_row, utilization_color};
use eframe::egui::{self, Color32, RichText, Ui};
use egui_plot::{HLine, Line, Plot, PlotPoints};

pub(super) struct RollingPlotSpec<'a> {
    pub(super) plot_id: &'a str,
    pub(super) title: &'a str,
    pub(super) series_name: &'a str,
    pub(super) limit_name: &'a str,
    pub(super) unit: &'a str,
    pub(super) accent: Color32,
    pub(super) current: Option<f32>,
    pub(super) limit: Option<f64>,
    pub(super) include_y: &'a [f64],
    pub(super) points: PlotPoints<'a>,
}

pub(super) fn render_rolling_plot_card(ui: &mut Ui, spec: RollingPlotSpec<'_>) {
    card_frame().show(ui, |ui| {
        split_or_wrap_row(
            ui,
            ("plot_header", spec.plot_id),
            |ui| {
                ui.label(RichText::new(spec.title).strong());
            },
            |ui| {
                if let Some(v) = spec.current {
                    ui.label(
                        RichText::new(format!("{v:.1} {}", spec.unit))
                            .monospace()
                            .strong()
                            .color(spec.accent),
                    );
                }
            },
        );
        ui.add_space(4.0);
        let mut plot = Plot::new(spec.plot_id)
            .height(185.0)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .y_axis_label(spec.unit)
            .x_axis_label("seconds ago");
        for &y in spec.include_y {
            plot = plot.include_y(y);
        }
        plot.show(ui, |plot_ui| {
            plot_ui.line(
                Line::new(spec.points)
                    .name(spec.series_name)
                    .color(spec.accent)
                    .width(2.0_f32),
            );
            if let Some(lim) = spec.limit {
                plot_ui.hline(
                    HLine::new(lim)
                        .name(spec.limit_name)
                        .color(ACCENT_CRIMSON)
                        .style(egui_plot::LineStyle::Dashed { length: 6.0 }),
                );
            }
        });
    });
}

pub(super) fn render_gauge_card(
    ui: &mut Ui,
    title: &str,
    value: Option<f32>,
    limit: Option<f32>,
    unit: &str,
    accent: Color32,
) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).small().strong().color(TEXT_MUTED));
        ui.add_space(4.0);
        match (value, limit) {
            (Some(val), Some(lim)) if lim > 0.0 => {
                let frac = (val / lim).clamp(0.0, 1.0);
                let fill = utilization_color(frac, accent);
                ui.label(
                    RichText::new(format!("{val:.1} {unit}"))
                        .size(24.0)
                        .strong()
                        .monospace()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                split_or_wrap_row(
                    ui,
                    ("gauge_caption", title),
                    |ui| {
                        ui.label(
                            RichText::new(format!("{:.0}%", frac * 100.0))
                                .small()
                                .strong()
                                .monospace()
                                .color(fill),
                        );
                    },
                    |ui| {
                        ui.label(
                            RichText::new(format!("of {lim:.0} {unit}"))
                                .small()
                                .monospace()
                                .color(TEXT_MUTED),
                        );
                    },
                );
                ui.add_space(3.0);
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4))
                        .fill(fill),
                );
            }
            (Some(val), _) => {
                ui.label(
                    RichText::new(format!("{val:.1} {unit}"))
                        .size(24.0)
                        .strong()
                        .monospace()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                ui.label(RichText::new("Limit N/A").small().color(TEXT_MUTED));
                ui.add_space(3.0);
                ui.add(
                    egui::ProgressBar::new(0.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4)),
                );
            }
            _ => {
                ui.label(
                    RichText::new("—")
                        .size(24.0)
                        .strong()
                        .monospace()
                        .color(TEXT_MUTED),
                );
                ui.add_space(4.0);
                ui.label(RichText::new("No data").small().color(TEXT_MUTED));
                ui.add_space(3.0);
                ui.add(
                    egui::ProgressBar::new(0.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4)),
                );
            }
        }
    });
}

pub(super) fn render_apu_limit_row(
    ui: &mut Ui,
    label: &str,
    value: Option<f32>,
    limit: Option<f32>,
    unit: &str,
    accent: Color32,
) {
    ui.label(RichText::new(label).strong().color(TEXT_MUTED));
    match (value, limit) {
        (Some(val), Some(lim)) if lim > 0.0 => {
            let frac = (val / lim).clamp(0.0, 1.0);
            let fill = utilization_color(frac, accent);
            ui.label(
                RichText::new(format!("{val:.1} / {lim:.0} {unit}"))
                    .monospace()
                    .strong()
                    .color(TEXT_PRIMARY),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(165.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4))
                        .fill(fill),
                );
                ui.label(
                    RichText::new(format!("{:.0}%", frac * 100.0))
                        .small()
                        .strong()
                        .monospace()
                        .color(fill),
                );
            });
        }
        (Some(val), Some(_)) => {
            ui.label(
                RichText::new(format!("{val:.1} {unit}"))
                    .monospace()
                    .color(TEXT_MUTED),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(0.0)
                        .desired_width(165.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4)),
                );
                ui.label(
                    RichText::new("Inactive (0 limit)")
                        .small()
                        .color(TEXT_MUTED),
                );
            });
        }
        (Some(val), None) => {
            ui.label(
                RichText::new(format!("{val:.1} {unit}"))
                    .monospace()
                    .color(TEXT_PRIMARY),
            );
            ui.label(RichText::new("Limit N/A").small().color(TEXT_MUTED));
        }
        (None, Some(lim)) if lim > 0.0 => {
            ui.label(
                RichText::new(format!("— / {lim:.0} {unit}"))
                    .monospace()
                    .color(TEXT_MUTED),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(0.0)
                        .desired_width(165.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4)),
                );
                ui.label(
                    RichText::new(format!("Limit {lim:.0} {unit}"))
                        .small()
                        .monospace()
                        .color(TEXT_MUTED),
                );
            });
        }
        _ => {
            ui.label(RichText::new("—").monospace().color(TEXT_MUTED));
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(0.0)
                        .desired_width(165.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4)),
                );
                ui.label(RichText::new("No data").small().color(TEXT_MUTED));
            });
        }
    }
    ui.end_row();
}

pub(super) fn metric_row(
    ui: &mut Ui,
    label: &str,
    value: Option<f32>,
    unit: &str,
    decimals: usize,
) {
    ui.label(RichText::new(label).color(TEXT_MUTED));
    let formatted = match value {
        Some(v) => format!("{v:.decimals$} {unit}"),
        None => "—".into(),
    };
    ui.label(RichText::new(formatted).monospace().strong());
    ui.end_row();
}
