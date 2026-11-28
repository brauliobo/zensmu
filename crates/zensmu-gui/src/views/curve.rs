//! Curve Optimizer tab (all-core, per-core grouped by CCD, and iGPU CO).

use super::bench::render_core_cycler_card;
use super::theme::*;
use super::widgets::{
    card_frame, cta_button, render_extreme_warning, render_no_hardware_placeholder,
    render_tab_header, render_unsupported_feature_card, split_or_wrap_row, tinted_pill,
};
use crate::app::{ToastLevel, ZensmuApp};
use crate::elevate::CliAction;
use eframe::egui::{self, RichText, Ui};
use std::collections::BTreeMap;
use zensmu_core::ops::co::{IGPU_CO_MAX, IGPU_CO_MIN, IGPU_CO_SOFT_MIN};
use zensmu_core::types::CoreInfo;

/// Tab 2: Curve Optimizer (All-Core, Per-Core grouped by CCD, and iGPU CO).
pub fn render_curve_optimizer(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    render_tab_header(
        ui,
        "Curve Optimizer",
        "Adjust voltage-frequency curve margins safely. Every write runs --dry-run first and verifies readback.",
    );

    let Some(hw) = app.hw.clone() else {
        render_no_hardware_placeholder(app, ui);
        return;
    };

    let Some(bounds) = hw.capabilities.co_bounds else {
        render_unsupported_feature_card(
            ui,
            "Curve Optimizer",
            hw.codename.display_name(),
            "Zen 2 and unsupported platforms do not expose Curve Optimizer SMU opcodes.",
        );
        return;
    };

    let busy = app.runner.is_busy();

    // Safety bar + All-core card
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "co_all_core_header",
            |ui| {
                ui.label(RichText::new("All-Core Curve Optimizer").strong());
                tinted_pill(
                    ui,
                    &format!(
                        "Bounds {}..=+{} · Soft floor {}",
                        bounds.min, bounds.max, bounds.soft_min
                    ),
                    ACCENT_CYAN,
                    PILL_ALPHA_CYAN,
                );
            },
            |ui| {
                if hw.capabilities.get_co_per_core.is_some()
                    && ui
                        .add_enabled(!busy, egui::Button::new("Read Current CO (pkexec)"))
                        .clicked()
                {
                    app.dispatch_cli(CliAction::CoGet, ctx);
                }
            },
        );

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label("All-Core Margin:");
            ui.add(
                egui::Slider::new(&mut app.co_editor.all_core, bounds.min..=bounds.max)
                    .show_value(true),
            );

            if ui.button("Copy to All Per-Core Sliders").clicked() {
                app.co_editor.per_core.fill(app.co_editor.all_core);
            }
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(
                &mut app.co_editor.allow_extreme,
                format!(
                    "Allow Extreme (< {} soft floor — requires stress testing)",
                    bounds.soft_min
                ),
            );
        });

        if app.co_editor.all_core < bounds.soft_min && !app.co_editor.allow_extreme {
            render_extreme_warning(
                ui,
                &format!(
                    "Value {} is below conservative soft floor ({}); enable 'Allow Extreme' to proceed.",
                    app.co_editor.all_core, bounds.soft_min
                ),
            );
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let can_all = hw.capabilities.set_co_all_cores.is_some()
                && !busy
                && (app.co_editor.all_core >= bounds.soft_min || app.co_editor.allow_extreme);
            if ui
                .add_enabled(
                    can_all,
                    cta_button("Preview (Dry-Run) & Apply All-Core", CTA_CRIMSON),
                )
                .clicked()
            {
                app.dispatch_cli(
                    CliAction::CoSetAll {
                        margin: app.co_editor.all_core,
                        allow_extreme: app.co_editor.allow_extreme,
                        dry_run: true,
                    },
                    ctx,
                );
            }

            if ui
                .add_enabled(
                    (hw.capabilities.set_co_all_cores.is_some()
                        || hw.capabilities.set_co_per_core.is_some())
                        && !busy,
                    egui::Button::new("Reset CO to 0"),
                )
                .clicked()
            {
                app.dispatch_cli(CliAction::CoReset { dry_run: true }, ctx);
            }
        });
    });

    ui.add_space(12.0);

    // Per-Core sliders grouped by CCD
    if hw.capabilities.set_co_per_core.is_some() {
        let mut by_ccd: BTreeMap<u32, Vec<&CoreInfo>> = BTreeMap::new();
        for core in &hw.topology.cores {
            by_ccd.entry(core.ccd).or_default().push(core);
        }

        if app.co_editor.per_core.len() < hw.topology.physical_cores {
            app.co_editor.per_core.resize(hw.topology.physical_cores, 0);
        }

        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            split_or_wrap_row(
                ui,
                "co_per_core_header",
                |ui| {
                    ui.label(RichText::new("Per-Core Curve Optimizer (by CCD)").strong());
                },
                |ui| {
                    let any_below_soft =
                        app.co_editor.per_core.iter().any(|&v| v < bounds.soft_min);
                    let can_apply = !busy && (!any_below_soft || app.co_editor.allow_extreme);
                    if ui
                        .add_enabled(
                            can_apply,
                            cta_button("Preview (Dry-Run) & Apply Per-Core", CTA_CRIMSON),
                        )
                        .clicked()
                    {
                        let updates: Vec<(u32, i32)> = app
                            .co_editor
                            .per_core
                            .iter()
                            .enumerate()
                            .filter(|(idx, &val)| {
                                // Send only modified cores when we have a live snapshot; otherwise all cores.
                                if let Some(snap) = &app.co_snapshot {
                                    snap.cores
                                        .iter()
                                        .find(|c| c.core_index == *idx as u32)
                                        .map(|c| i32::from(c.margin) != val)
                                        .unwrap_or(true)
                                } else {
                                    true
                                }
                            })
                            .map(|(idx, &val)| (idx as u32, val))
                            .collect();

                        if updates.is_empty() {
                            app.set_toast(
                                ToastLevel::Info,
                                "All per-core margins already match hardware state.",
                            );
                        } else {
                            app.dispatch_cli(
                                CliAction::CoSetCores {
                                    updates,
                                    allow_extreme: app.co_editor.allow_extreme,
                                    dry_run: true,
                                },
                                ctx,
                            );
                        }
                    }
                },
            );

            ui.add_space(8.0);
            for (ccd, cores) in by_ccd {
                ui.label(
                    RichText::new(format!("CCD {ccd} ({} physical cores)", cores.len()))
                        .small()
                        .strong()
                        .color(ACCENT_CYAN),
                );
                ui.add_space(4.0);

                egui::Grid::new(format!("co_ccd_{ccd}_grid"))
                    .num_columns(4)
                    .striped(true)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Physical Core").small().color(TEXT_MUTED));
                        ui.label(RichText::new("Linux CPUs").small().color(TEXT_MUTED));
                        ui.label(RichText::new("Current").small().color(TEXT_MUTED));
                        ui.label(RichText::new("Target Margin").small().color(TEXT_MUTED));
                        ui.end_row();

                        for core in cores {
                            let idx = core.index as usize;
                            let cpus = core
                                .linux_cpus
                                .iter()
                                .map(|c| format!("CPU {c}"))
                                .collect::<Vec<_>>()
                                .join(", ");

                            let current_str = app
                                .co_snapshot
                                .as_ref()
                                .and_then(|s| s.cores.iter().find(|c| c.core_index == core.index))
                                .map(|c| format!("{:+}", c.margin))
                                .unwrap_or_else(|| "—".into());

                            ui.label(
                                RichText::new(format!(
                                    "Core {} (local {})",
                                    core.index, core.core_in_ccd
                                ))
                                .monospace(),
                            );
                            ui.label(RichText::new(cpus).small().color(TEXT_MUTED));
                            ui.label(RichText::new(current_str).monospace().color(ACCENT_EMERALD));

                            if let Some(val) = app.co_editor.per_core.get_mut(idx) {
                                ui.horizontal(|ui| {
                                    ui.add(egui::Slider::new(val, bounds.min..=bounds.max));
                                    if *val < bounds.soft_min {
                                        ui.label(
                                            RichText::new("extreme").small().color(ACCENT_AMBER),
                                        );
                                    }
                                });
                            }
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
            }
        });
    }

    // iGPU CO Card
    if hw.capabilities.set_igpu_co.is_some() {
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("iGPU Curve Optimizer").strong());
                if let Some(cur) = app.co_snapshot.as_ref().and_then(|s| s.igpu) {
                    tinted_pill(
                        ui,
                        &format!("Current: {cur:+}"),
                        ACCENT_EMERALD,
                        PILL_ALPHA_EMERALD,
                    );
                }
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("iGPU Margin:");
                ui.add(egui::Slider::new(
                    &mut app.co_editor.igpu,
                    IGPU_CO_MIN..=IGPU_CO_MAX,
                ));
                let can_igpu = !busy
                    && (app.co_editor.igpu >= IGPU_CO_SOFT_MIN || app.co_editor.allow_extreme);
                if ui
                    .add_enabled(can_igpu, egui::Button::new("Preview & Apply iGPU CO"))
                    .clicked()
                {
                    app.dispatch_cli(
                        CliAction::CoSetIgpu {
                            margin: app.co_editor.igpu,
                            allow_extreme: app.co_editor.allow_extreme,
                            dry_run: true,
                        },
                        ctx,
                    );
                }
            });
        });
    }

    // Core-Cycler Stability Tester Card (unprivileged)
    ui.add_space(12.0);
    render_core_cycler_card(app, ui, ctx);
}
