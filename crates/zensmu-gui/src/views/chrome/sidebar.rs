//! Left navigation sidebar and compact hardware summary card.

use crate::app::{Tab, ZensmuApp};
use crate::views::theme::*;
use crate::views::widgets::{card_frame, panel_frame, split_or_wrap_row};
use eframe::egui::{self, Color32, RichText, Stroke, Ui};

/// Left sidebar navigation and compact hardware summary card.
pub fn render_sidebar(app: &mut ZensmuApp, ctx: &egui::Context) {
    egui::SidePanel::left("sidebar")
        .resizable(false)
        .exact_width(245.0)
        .frame(panel_frame(egui::Margin::same(12)))
        .show(ctx, |ui| {
            ui.label(
                RichText::new("NAVIGATION")
                    .small()
                    .strong()
                    .color(TEXT_MUTED),
            );
            ui.add_space(6.0);

            let pbo_subtitle =
                if app.hw.as_ref().is_some_and(|h| {
                    h.codename.is_apu() || h.capabilities.set_stapm_limit.is_some()
                }) {
                    "PPT / TDC / EDC / APU DPTC"
                } else {
                    "PPT / TDC / EDC / Fmax"
                };
            let tabs = [
                (Tab::Dashboard, "Dashboard", "Live telemetry"),
                (
                    Tab::CurveOptimizer,
                    "Curve Optimizer",
                    "All-core & per-core",
                ),
                (Tab::CurveShaper, "Curve Shaper", "Zen 5 5×3 matrix"),
                (Tab::PboLimits, "PBO Limits", pbo_subtitle),
                (
                    Tab::ProfilesSafety,
                    "Profiles & Safety",
                    "Boot persist, TOML, guards",
                ),
            ];

            for (tab, title, subtitle) in tabs {
                let selected = app.active_tab == tab;
                let fill = if selected {
                    tinted_bg(ACCENT_CRIMSON, SIDEBAR_SELECTED_ALPHA)
                } else {
                    Color32::TRANSPARENT
                };
                let stroke = if selected {
                    Stroke::new(1.0_f32, ACCENT_CRIMSON)
                } else {
                    Stroke::NONE
                };

                let resp = egui::Frame::NONE
                    .fill(fill)
                    .stroke(stroke)
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.vertical(|ui| {
                            ui.label(RichText::new(title).strong().color(if selected {
                                TEXT_BRIGHT
                            } else {
                                TEXT_PRIMARY
                            }));
                            ui.label(RichText::new(subtitle).small().color(TEXT_MUTED));
                        });
                    })
                    .response
                    .interact(egui::Sense::click());

                if resp.clicked() {
                    app.active_tab = tab;
                    if tab == Tab::ProfilesSafety {
                        app.refresh_safety_files();
                        app.refresh_persist_status();
                        app.refresh_record_daemon_status();
                        app.refresh_exporter_daemon_status();
                    }
                }
                ui.add_space(4.0);
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                card_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("SYSTEM").small().strong().color(TEXT_MUTED));
                    ui.add_space(4.0);
                    if let Some(hw) = &app.hw {
                        kv_small(ui, "Codename", hw.codename.slug());
                        kv_small(
                            ui,
                            "Topology",
                            &format!(
                                "{}C / {}T ({} CCD)",
                                hw.topology.physical_cores,
                                hw.topology.logical_cpus,
                                hw.topology.ccd_count
                            ),
                        );
                        kv_small(ui, "ryzen_smu", &hw.driver.drv_version);
                        if let Some(ver) = hw.driver.pm_table_version {
                            kv_small(ui, "PM Table", &format!("0x{ver:08X}"));
                        }
                    } else {
                        ui.label(RichText::new("No SMU hardware").small().color(ACCENT_AMBER));
                    }
                    kv_small(
                        ui,
                        "CLI",
                        app.runner
                            .zensmu_bin()
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("zensmu"),
                    );
                });
            });
        });
}

fn kv_small(ui: &mut Ui, key: &str, val: &str) {
    split_or_wrap_row(
        ui,
        ("sidebar_kv", key),
        |ui| {
            ui.label(RichText::new(key).small().color(TEXT_MUTED));
        },
        |ui| {
            ui.label(RichText::new(val).small().monospace().color(TEXT_PRIMARY));
        },
    );
}
