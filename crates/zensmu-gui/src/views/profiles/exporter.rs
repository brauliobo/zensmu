//! Unprivileged Prometheus / OpenMetrics user exporter service card.

use crate::app::{ToastLevel, ZensmuApp};
use crate::views::theme::*;
use crate::views::widgets::{card_frame, cta_button, muted_pill, split_or_wrap_row, tinted_pill};
use eframe::egui::{self, RichText, Ui};
use zensmu_core::metrics::{DEFAULT_METRICS_LISTEN, USER_EXPORTER_SERVICE_NAME};

pub(super) fn render_exporter_card(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    let exporter = app.exporter_status.clone();
    let service_name = if exporter.service_name.is_empty() {
        USER_EXPORTER_SERVICE_NAME
    } else {
        exporter.service_name.as_str()
    };
    let endpoint = if exporter.endpoint.is_empty() {
        format!("http://{DEFAULT_METRICS_LISTEN}/metrics")
    } else {
        exporter.endpoint.clone()
    };

    let mut refresh_exporter = false;
    let mut toggle_exporter = false;
    let mut copy_url = false;

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "exporter_card_header",
            |ui| {
                ui.label(
                    RichText::new(format!("Prometheus / OpenMetrics Exporter ({service_name})"))
                        .strong(),
                );
                if exporter.active {
                    tinted_pill(ui, "Exporter: ACTIVE", ACCENT_EMERALD, PILL_ALPHA_EMERALD);
                } else if exporter.enabled {
                    tinted_pill(ui, "Exporter: ENABLED", ACCENT_CYAN, PILL_ALPHA_CYAN);
                } else if !exporter.installed {
                    tinted_pill(
                        ui,
                        "Exporter: NOT INSTALLED",
                        ACCENT_AMBER,
                        PILL_ALPHA_AMBER,
                    );
                } else {
                    muted_pill(ui, "Exporter: OFF");
                }
            },
            |ui| {
                if ui
                    .button("Refresh Status")
                    .on_hover_text("Re-check user systemd exporter status (unprivileged)")
                    .clicked()
                {
                    refresh_exporter = true;
                }

                let hover = if !exporter.installed {
                    "zensmu-exporter.service is not installed in /usr/lib/systemd/user/ or ~/.config/systemd/user/"
                } else if exporter.enabled {
                    "Run `systemctl --user disable --now zensmu-exporter.service` (unprivileged)"
                } else {
                    "Run `systemctl --user enable --now zensmu-exporter.service` (unprivileged)"
                };
                if exporter.enabled {
                    if ui
                        .add_enabled(
                            exporter.installed,
                            egui::Button::new("Disable Exporter"),
                        )
                        .on_hover_text(hover)
                        .clicked()
                    {
                        toggle_exporter = true;
                    }
                } else if ui
                    .add_enabled(
                        exporter.installed,
                        cta_button("Enable Exporter", CTA_EMERALD),
                    )
                    .on_hover_text(hover)
                    .clicked()
                {
                    toggle_exporter = true;
                }
            },
        );

        ui.add_space(6.0);
        egui::Grid::new("exporter_service_grid")
            .num_columns(2)
            .striped(true)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.label(RichText::new("User service state").small().color(TEXT_MUTED));
                let state_color = if exporter.active {
                    ACCENT_EMERALD
                } else if exporter.enabled {
                    ACCENT_CYAN
                } else if !exporter.installed || exporter.state == "failed" {
                    ACCENT_AMBER
                } else {
                    TEXT_MUTED
                };
                ui.label(
                    RichText::new(&exporter.state)
                        .small()
                        .monospace()
                        .strong()
                        .color(state_color),
                );
                ui.end_row();

                ui.label(RichText::new("Metrics endpoint").small().color(TEXT_MUTED));
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(&endpoint)
                            .small()
                            .monospace()
                            .color(TEXT_PRIMARY),
                    );
                    if ui
                        .add(egui::Button::new("Copy URL").small())
                        .on_hover_text("Copy Prometheus / OpenMetrics scrape endpoint URL")
                        .clicked()
                    {
                        copy_url = true;
                    }
                });
                ui.end_row();
            });
    });

    if refresh_exporter {
        app.refresh_exporter_daemon_status();
        app.set_toast(ToastLevel::Info, "Refreshed Prometheus exporter status.");
    }
    if toggle_exporter {
        app.toggle_exporter_daemon();
    }
    if copy_url {
        ctx.copy_text(endpoint);
        app.set_toast(
            ToastLevel::Info,
            "Copied Prometheus metrics URL to clipboard.",
        );
    }
}
