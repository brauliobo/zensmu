//! Bottom status bar showing toasts, spinner, and telemetry polling cadence.

use crate::app::{error_message_suggests_report, ToastLevel, ZensmuApp, TELEMETRY_RING_CAPACITY};
use crate::views::theme::*;
use crate::views::widgets::{format_interval_label, panel_frame, split_or_wrap_row};
use eframe::egui::{self, RichText, Stroke};

/// Bottom status bar showing last command toast / spinner and telemetry sampling status.
pub fn render_bottom_bar(app: &mut ZensmuApp, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("bottom_bar")
        .frame(panel_frame(egui::Margin::symmetric(14, 6)))
        .show(ctx, |ui| {
            let rate_text = if app.hw.is_none() {
                "Telemetry offline".into()
            } else if app.telemetry_paused {
                format!(
                    "Paused · {}/{} samples",
                    app.samples.len(),
                    TELEMETRY_RING_CAPACITY
                )
            } else {
                let hz = 1000.0 / (app.poll_interval_ms.max(1) as f64);
                format!(
                    "{hz:.1} Hz ({}) · {}/{} samples",
                    format_interval_label(app.poll_interval_ms),
                    app.samples.len(),
                    TELEMETRY_RING_CAPACITY
                )
            };

            let mut open_report_from_toast: Option<bool> = None;
            let mut dismiss_toast = false;

            split_or_wrap_row(
                ui,
                "bottom_bar_row",
                |ui| {
                    if let Some(label) = app.runner.in_flight_label() {
                        ui.spinner();
                        ui.label(RichText::new(format!("Running: {label}…")).color(ACCENT_CYAN));
                    } else if let Some(toast) = &app.toast {
                        let color = match toast.level {
                            ToastLevel::Info => ACCENT_CYAN,
                            ToastLevel::Success => ACCENT_EMERALD,
                            ToastLevel::Warning => ACCENT_AMBER,
                            ToastLevel::Error => ACCENT_CRIMSON,
                        };
                        let age = toast.created_at.elapsed().as_secs();
                        ui.label(RichText::new(&toast.message).color(color));
                        if age > 2 {
                            ui.label(
                                RichText::new(format!("({age}s ago)"))
                                    .small()
                                    .color(TEXT_MUTED),
                            );
                        }
                        if toast.level == ToastLevel::Error
                            && error_message_suggests_report(&toast.message)
                        {
                            let raw_pm = toast
                                .message
                                .to_ascii_lowercase()
                                .contains("pm table");
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("Report on GitHub")
                                            .small()
                                            .strong()
                                            .color(ACCENT_CYAN),
                                    )
                                    .fill(tinted_bg(ACCENT_CYAN, PILL_ALPHA_CYAN))
                                    .stroke(Stroke::new(1.0_f32, ACCENT_CYAN.linear_multiply(0.55)))
                                    .corner_radius(egui::CornerRadius::same(10)),
                                )
                                .on_hover_text(
                                    "Collect privacy-safe hardware diagnostics & open a GitHub issue",
                                )
                                .clicked()
                            {
                                open_report_from_toast = Some(raw_pm);
                            }
                        }
                        if ui
                            .add(egui::Button::new(RichText::new("×").small()).small())
                            .on_hover_text("Dismiss status message")
                            .clicked()
                        {
                            dismiss_toast = true;
                        }
                    } else {
                        ui.label(
                            RichText::new("Ready — telemetry reads sysfs without root.")
                                .color(TEXT_MUTED),
                        );
                    }
                },
                |ui| {
                    ui.label(
                        RichText::new(rate_text)
                            .small()
                            .monospace()
                            .color(TEXT_MUTED),
                    );
                },
            );

            if let Some(raw_pm) = open_report_from_toast {
                if raw_pm {
                    app.open_report_modal_for_unsupported();
                } else {
                    app.open_report_modal();
                }
            }
            if dismiss_toast {
                app.toast = None;
            }
        });
}
