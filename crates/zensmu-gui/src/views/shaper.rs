//! Zen 5 Curve Shaper 5x3 frequency x temperature matrix tab.

use super::theme::*;
use super::widgets::{
    card_frame, cta_button, render_no_hardware_placeholder, render_tab_header,
    render_unsupported_feature_card, split_or_wrap_row, tinted_pill,
};
use crate::app::ZensmuApp;
use crate::elevate::CliAction;
use eframe::egui::{self, RichText, Ui};
use zensmu_core::ops::shaper::{SHAPER_MAX, SHAPER_MIN, SHAPER_SOFT_MIN};
use zensmu_core::types::FreqTier;

/// Tab 3: Zen 5 Curve Shaper (5 frequency tiers × 3 temperature bands).
pub fn render_curve_shaper(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    render_tab_header(
        ui,
        "Curve Shaper (Zen 5)",
        "15-point frequency × temperature voltage curve shaping (-50..=+30, soft floor -35).",
    );

    let Some(hw) = app.hw.clone() else {
        render_no_hardware_placeholder(app, ui);
        return;
    };

    if hw.capabilities.set_curve_shaper.is_none() {
        render_unsupported_feature_card(
            ui,
            "Curve Shaper",
            hw.codename.display_name(),
            "Curve Shaper (5 frequency tiers × 3 temperature bands) requires a Zen 5 processor such as Granite Ridge (Ryzen 9000 series). zensmu disables these controls on non-Zen-5 CPUs so invalid SMU opcodes are never emitted.",
        );
        return;
    }

    let busy = app.runner.is_busy();

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "shaper_matrix_header",
            |ui| {
                ui.label(RichText::new("5×3 Frequency / Temperature Matrix").strong());
                tinted_pill(
                    ui,
                    &format!("Range {SHAPER_MIN}..=+{SHAPER_MAX} · Soft floor {SHAPER_SOFT_MIN}"),
                    ACCENT_CYAN,
                    PILL_ALPHA_CYAN,
                );
            },
            |ui| {
                if hw.capabilities.get_curve_shaper.is_some()
                    && ui
                        .add_enabled(!busy, egui::Button::new("Read Current Matrix (pkexec)"))
                        .clicked()
                {
                    app.dispatch_cli(CliAction::ShaperGet, ctx);
                }
            },
        );

        ui.add_space(10.0);

        let mut single_tier_preview: Option<FreqTier> = None;

        egui::Grid::new("shaper_matrix_grid")
            .num_columns(5)
            .striped(true)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Freq Tier").strong().color(TEXT_MUTED));
                ui.label(RichText::new("Low Temp").strong().color(ACCENT_CYAN));
                ui.label(RichText::new("Med Temp").strong().color(ACCENT_EMERALD));
                ui.label(RichText::new("High Temp").strong().color(ACCENT_AMBER));
                ui.label(RichText::new("Row Action").strong().color(TEXT_MUTED));
                ui.end_row();

                for tier in FreqTier::ALL {
                    let cur = app.shaper_state.map(|s| s.tier(tier));
                    let margins = app.shaper_editor.draft.tier_mut(tier);

                    ui.label(
                        RichText::new(tier.label().to_uppercase())
                            .strong()
                            .monospace(),
                    );

                    shaper_cell(ui, &mut margins.low, cur.map(|c| c.low));
                    shaper_cell(ui, &mut margins.med, cur.map(|c| c.med));
                    shaper_cell(ui, &mut margins.high, cur.map(|c| c.high));

                    let row_extreme = margins.low < SHAPER_SOFT_MIN
                        || margins.med < SHAPER_SOFT_MIN
                        || margins.high < SHAPER_SOFT_MIN;
                    if ui
                        .add_enabled(
                            !busy && (!row_extreme || app.shaper_editor.allow_extreme),
                            egui::Button::new("Preview Row"),
                        )
                        .clicked()
                    {
                        single_tier_preview = Some(tier);
                    }
                    ui.end_row();
                }
            });

        if let Some(tier) = single_tier_preview {
            let margins = app.shaper_editor.draft.tier(tier);
            app.dispatch_cli(
                CliAction::ShaperSetTier {
                    tier,
                    margins,
                    allow_extreme: app.shaper_editor.allow_extreme,
                    dry_run: true,
                },
                ctx,
            );
        }

        ui.add_space(10.0);
        ui.checkbox(
            &mut app.shaper_editor.allow_extreme,
            format!("Allow Extreme (< {SHAPER_SOFT_MIN} soft floor)"),
        );

        let any_extreme = FreqTier::ALL.iter().any(|&t| {
            let m = app.shaper_editor.draft.tier(t);
            m.low < SHAPER_SOFT_MIN || m.med < SHAPER_SOFT_MIN || m.high < SHAPER_SOFT_MIN
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !busy && (!any_extreme || app.shaper_editor.allow_extreme),
                    cta_button("Preview (Dry-Run) & Apply Matrix", CTA_CRIMSON),
                )
                .clicked()
            {
                app.dispatch_cli(
                    CliAction::ShaperSetAll {
                        state: app.shaper_editor.draft,
                        allow_extreme: app.shaper_editor.allow_extreme,
                        dry_run: true,
                    },
                    ctx,
                );
            }

            if ui
                .add_enabled(!busy, egui::Button::new("Reset Shaper to 0"))
                .clicked()
            {
                app.dispatch_cli(CliAction::ShaperReset { dry_run: true }, ctx);
            }
        });
    });
}

fn shaper_cell(ui: &mut Ui, val: &mut i8, current: Option<i8>) {
    let tint = if *val < SHAPER_SOFT_MIN {
        ACCENT_AMBER
    } else if *val < 0 {
        ACCENT_CYAN
    } else if *val > 0 {
        ACCENT_CRIMSON
    } else {
        TEXT_PRIMARY
    };

    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(val)
                .range(SHAPER_MIN..=SHAPER_MAX)
                .speed(0.5),
        );
        ui.label(
            RichText::new(format!("{:+}", *val))
                .small()
                .monospace()
                .color(tint),
        );
        if let Some(cur) = current {
            if cur != *val {
                ui.label(
                    RichText::new(format!("(hw {cur:+})"))
                        .small()
                        .color(TEXT_MUTED),
                );
            }
        }
    });
}
