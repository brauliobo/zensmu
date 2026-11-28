//! Declarative TOML profiles, rollback/reset, boot-loop guard, and daemon cards.

mod exporter;
mod persistence;
mod recorder;

use crate::app::ZensmuApp;
use crate::elevate::CliAction;
use crate::views::theme::*;
use crate::views::widgets::{card_frame, cta_button, muted_pill, render_tab_header, tinted_pill};
use eframe::egui::{self, RichText, Ui};
use exporter::render_exporter_card;
use persistence::render_persistence_card;
use recorder::render_recorder_card;
use zensmu_core::hw::identity::driver_has_false_ok_bug;
use zensmu_core::transport::sysfs::{DEFAULT_LOCK_PATH, DEFAULT_SYSFS_PATH};

/// Tab 5: Declarative TOML Profiles, Rollback/Reset, Boot-Loop Guard, and Driver Safety Status.
pub fn render_profiles_and_safety(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    render_tab_header(
        ui,
        "Profiles & Safety",
        "Load, validate, diff, export, and apply declarative TOML profiles, or manage rollback and driver safety guards.",
    );

    let busy = app.runner.is_busy();
    let has_hw = app.hw.is_some();

    // Boot & Resume Persistence (systemd) Card
    render_persistence_card(app, ui, ctx, has_hw, busy);
    ui.add_space(12.0);

    // Telemetry Recording & Background Daemon (unprivileged) Card
    render_recorder_card(app, ui, ctx, has_hw);
    ui.add_space(12.0);

    // Prometheus / OpenMetrics Exporter (unprivileged) Card
    render_exporter_card(app, ui, ctx);
    ui.add_space(12.0);

    // TOML Profile Card
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Declarative TOML Profile").strong());
        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            ui.label("Path:");
            ui.add(
                egui::TextEdit::singleline(&mut app.profile_editor.path)
                    .desired_width(400.0)
                    .hint_text("/etc/zensmu/active.toml"),
            );
            if ui.button("Load TOML").clicked() {
                app.load_profile_from_disk();
            }
            if ui
                .add_enabled(has_hw && !busy, egui::Button::new("Export Current State"))
                .clicked()
            {
                app.export_current_profile(ctx);
            }
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(has_hw, egui::Button::new("Validate"))
                .clicked()
            {
                app.validate_profile_buffer();
            }
            if ui.add_enabled(has_hw, egui::Button::new("Diff")).clicked() {
                app.compute_profile_diff();
            }
            ui.checkbox(&mut app.profile_editor.allow_extreme, "Allow Extreme");
            if ui
                .add_enabled(
                    has_hw && !busy,
                    cta_button("Preview (Dry-Run) & Apply", CTA_CRIMSON),
                )
                .clicked()
            {
                app.preview_apply_profile(ctx);
            }
        });

        if let Some(res) = &app.profile_editor.validation {
            ui.add_space(4.0);
            match res {
                Ok(ok) => ui.label(RichText::new(format!("✓ {ok}")).color(ACCENT_EMERALD)),
                Err(err) => ui.label(RichText::new(format!("✗ {err}")).color(ACCENT_CRIMSON)),
            };
        }

        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .id_salt("toml_editor_scroll")
            .max_height(260.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut app.profile_editor.toml_text)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(12),
                );
            });

        if !app.profile_editor.diff_entries.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Profile Diff vs Current State").strong());
            egui::Grid::new("profile_diff_grid")
                .num_columns(3)
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Field").small().strong().color(TEXT_MUTED));
                    ui.label(RichText::new("Current").small().strong().color(TEXT_MUTED));
                    ui.label(RichText::new("Desired").small().strong().color(TEXT_MUTED));
                    ui.end_row();
                    for entry in &app.profile_editor.diff_entries {
                        ui.label(RichText::new(&entry.field).monospace());
                        ui.label(RichText::new(&entry.current).monospace().color(TEXT_MUTED));
                        ui.label(RichText::new(&entry.desired).monospace().color(ACCENT_CYAN));
                        ui.end_row();
                    }
                });
        }
    });

    ui.add_space(12.0);

    // Rollback, Reset & Driver Safety Card
    ui.columns(2, |cols| {
        card_frame().show(&mut cols[0], |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Rollback & Boot Baseline").strong());
            ui.add_space(6.0);

            ui.horizontal_wrapped(|ui| {
                ui.label("Last-change snapshot:");
                if app.profile_editor.has_last_state {
                    tinted_pill(
                        ui,
                        "Present (/run/zensmu/last-state.json)",
                        ACCENT_EMERALD,
                        PILL_ALPHA_EMERALD,
                    );
                } else {
                    muted_pill(ui, "None yet this boot");
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label("Boot baseline snapshot:");
                if app.profile_editor.has_boot_baseline {
                    tinted_pill(
                        ui,
                        "Captured (/run/zensmu/boot-baseline.json)",
                        ACCENT_EMERALD,
                        PILL_ALPHA_EMERALD,
                    );
                } else {
                    muted_pill(ui, "Captured on first write");
                }
            });

            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        has_hw && !busy,
                        egui::Button::new("Rollback Last Change"),
                    )
                    .clicked()
                {
                    app.dispatch_cli(CliAction::Rollback { dry_run: true }, ctx);
                }
                if ui
                    .add_enabled(
                        has_hw && !busy,
                        egui::Button::new("Reset to Boot Baseline"),
                    )
                    .clicked()
                {
                    app.dispatch_cli(CliAction::Reset { dry_run: true }, ctx);
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("Boot-Loop Crash Guard:");
                if app.profile_editor.boot_guard_tripped {
                    tinted_pill(
                        ui,
                        "TRIPPED — auto-apply halted",
                        ACCENT_CRIMSON,
                        PILL_ALPHA_CRIMSON,
                    );
                } else {
                    tinted_pill(ui, "Armed & Clear", ACCENT_EMERALD, PILL_ALPHA_EMERALD);
                }
            });
            if let Some(info) = &app.profile_editor.boot_guard_sentinel {
                ui.label(
                    RichText::new(format!(
                        "Sentinel armed by profile '{}' (PID {}). Run `sudo zensmu profile clear-guard` after relaxing unstable settings.",
                        info.profile, info.pid
                    ))
                    .small()
                    .color(ACCENT_AMBER),
                );
            }
        });

        card_frame().show(&mut cols[1], |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Driver Safety & Mitigation Status").strong());
            ui.add_space(6.0);

            if let Some(hw) = &app.hw {
                let affected = driver_has_false_ok_bug(&hw.driver.drv_version);
                let rsmu_rsp = hw.capabilities.rsp_addrs.rsmu_rsp;
                let mp1_rsp = hw.capabilities.rsp_addrs.mp1_rsp;

                egui::Grid::new("driver_safety_grid")
                    .num_columns(2)
                    .striped(true)
                    .spacing([12.0, 5.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Sysfs path").small().color(TEXT_MUTED));
                        ui.label(RichText::new(DEFAULT_SYSFS_PATH).small().monospace());
                        ui.end_row();

                        ui.label(RichText::new("Driver version").small().color(TEXT_MUTED));
                        ui.label(RichText::new(&hw.driver.drv_version).small().monospace());
                        ui.end_row();

                        ui.label(RichText::new("smu.c:213 false-OK").small().color(TEXT_MUTED));
                        if affected {
                            ui.label(
                                RichText::new("Affected version — MITIGATED via SMN RSP + readback")
                                    .small()
                                    .color(ACCENT_EMERALD),
                            );
                        } else {
                            ui.label(
                                RichText::new("SMN RSP verification + readback active")
                                    .small()
                                    .color(ACCENT_EMERALD),
                            );
                        }
                        ui.end_row();

                        ui.label(RichText::new("SMN RSP addrs").small().color(TEXT_MUTED));
                        ui.label(
                            RichText::new(format!(
                                "RSMU={} · MP1={}",
                                rsmu_rsp
                                    .map(|a| format!("0x{a:08X}"))
                                    .unwrap_or_else(|| "none".into()),
                                mp1_rsp
                                    .map(|a| format!("0x{a:08X}"))
                                    .unwrap_or_else(|| "none".into()),
                            ))
                            .small()
                            .monospace(),
                        );
                        ui.end_row();

                        ui.label(RichText::new("Transaction lock").small().color(TEXT_MUTED));
                        ui.label(
                            RichText::new(format!("flock ({DEFAULT_LOCK_PATH})"))
                                .small()
                                .monospace(),
                        );
                        ui.end_row();

                        ui.label(RichText::new("CPUID cross-check").small().color(TEXT_MUTED));
                        if let Some(cpuid) = &hw.cpuid {
                            ui.label(
                                RichText::new(format!(
                                    "Verified (Family 0x{:X} Model 0x{:X})",
                                    cpuid.family, cpuid.model
                                ))
                                .small()
                                .color(ACCENT_EMERALD),
                            );
                        } else {
                            ui.label(RichText::new("Unavailable").small().color(TEXT_MUTED));
                        }
                        ui.end_row();
                    });
            } else {
                ui.label(
                    RichText::new("Driver safety details will appear once ryzen_smu is loaded.")
                        .color(TEXT_MUTED),
                );
            }
        });
    });
}
