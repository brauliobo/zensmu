//! PBO power/current/thermal limits, scalar, Fmax, and APU DPTC tab.

mod apu;

use crate::app::{format_board_summary, ZensmuApp};
use crate::elevate::CliAction;
use crate::views::theme::*;
use crate::views::widgets::{
    capability_disabled_hint, card_frame, cta_button, flag_pill, format_board_caps_summary,
    fused_soft_note, grid_header_row, opt_f32, optional_fused_pill, render_extreme_warning,
    render_limit_slider_row, render_no_hardware_placeholder, render_tab_header, split_or_wrap_row,
    LimitSliderRow,
};
use apu::render_apu_dptc_card;
use eframe::egui::{self, RichText, Ui};
use zensmu_core::ops::pbo::{
    EDC_MAX_A, EDC_MIN_A, EDC_SOFT_MAX_A, FMAX_MAX_MHZ, FMAX_MIN_MHZ, PPT_MAX_W, PPT_MIN_W,
    PPT_SOFT_MAX_W, SCALAR_MAX, SCALAR_MIN, SCALAR_SOFT_MAX, TCTL_MAX_C, TCTL_MIN_C, TDC_MAX_A,
    TDC_MIN_A, TDC_SOFT_MAX_A,
};

/// Tab 4: PBO Power/Thermal Limits, Scalar, Fmax Boost Limit, and Fused Factory Limits.
pub fn render_pbo_limits(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    render_tab_header(
        ui,
        "Precision Boost Overdrive (PBO) Limits",
        "Configure package power, VRM current, thermal ceiling (hard max 95°C), scalar, and Fmax boost limit.",
    );

    let Some(hw) = app.hw.clone() else {
        render_no_hardware_placeholder(app, ui);
        return;
    };

    let busy = app.runner.is_busy();
    let fused = app.pbo_status.as_ref().map(|s| s.fused.clone());
    let telem = app.latest_telemetry.clone();
    let caps = &hw.capabilities;
    let board_summary = format_board_summary(&app.board_info);
    let ppt_cap = app.board_caps.get("pbo.ppt_w").copied();
    let tdc_cap = app.board_caps.get("pbo.tdc_a").copied();
    let edc_cap = app.board_caps.get("pbo.edc_a").copied();
    let tctl_cap = app.board_caps.get("pbo.tctl_c").copied();
    let learned_caps_summary = if app.board_caps.is_empty() {
        None
    } else {
        Some(format_board_caps_summary(&app.board_caps))
    };

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "pbo_controls_header",
            |ui| {
                ui.label(RichText::new("PBO Controls").strong());
            },
            |ui| {
                if ui
                    .button("Sync from Live")
                    .on_hover_text("Reset edited PBO sliders to live telemetry and mailbox values")
                    .clicked()
                {
                    app.pbo_editor.clear_dirty();
                    if let Some(snap) = app.latest_telemetry.clone() {
                        app.pbo_editor.sync_from_telemetry(&snap);
                    }
                    if let Some(status) = app.pbo_status.clone() {
                        app.pbo_editor.sync_from_status(&status, true);
                    }
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Read PBO Status & Fused Limits (pkexec)"))
                    .clicked()
                {
                    app.dispatch_cli(CliAction::PboGet, ctx);
                }
            },
        );

        ui.add_space(6.0);
        let board_notice = match &board_summary {
            Some(summary) => format!(
                "Board: {summary} — Motherboard BIOS/VRM firmware may cap PPT/TDC/EDC unless BIOS PBO Limits are set to Manual/Motherboard."
            ),
            None => "Motherboard BIOS/VRM firmware may cap PPT/TDC/EDC unless BIOS PBO Limits are set to Manual/Motherboard.".into(),
        };
        ui.label(RichText::new(board_notice).small().color(TEXT_MUTED));
        if let Some(ref learned) = learned_caps_summary {
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "Detected board/BIOS ceiling(s): {learned} — set BIOS PBO Limits to Manual/Motherboard to raise VRM caps."
                ))
                .small()
                .strong()
                .color(ACCENT_AMBER),
            );
        }

        ui.add_space(8.0);

        egui::Grid::new("pbo_controls_grid")
            .num_columns(4)
            .striped(true)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                grid_header_row(
                    ui,
                    &[
                        "Parameter",
                        "Target Value",
                        "Current Live",
                        "Fused Factory / Guard",
                    ],
                );

                // PPT
                let ppt_supported = caps.set_ppt.is_some();
                let ppt_live = telem.as_ref().and_then(|t| t.ppt_limit_w);
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "PPT Limit (W)",
                        supported: ppt_supported,
                        enabled: &mut app.pbo_editor.ppt_enabled,
                        value: &mut app.pbo_editor.ppt_w,
                        dirty: Some(&mut app.pbo_editor.ppt_dirty),
                        range: PPT_MIN_W..=PPT_MAX_W,
                        step_by: None,
                        suffix: " W",
                        live_display: opt_f32(ppt_live, "W"),
                        live_is_muted: false,
                        note: if ppt_supported {
                            fused_soft_note(
                                fused.as_ref().and_then(|f| f.ppt_w),
                                "Fused",
                                PPT_SOFT_MAX_W,
                                "W",
                            )
                        } else {
                            capability_disabled_hint(ppt_live.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: ppt_cap,
                    },
                );

                // TDC
                let tdc_supported = caps.set_tdc.is_some();
                let tdc_live = telem.as_ref().and_then(|t| t.tdc_limit_a);
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "TDC Limit (A)",
                        supported: tdc_supported,
                        enabled: &mut app.pbo_editor.tdc_enabled,
                        value: &mut app.pbo_editor.tdc_a,
                        dirty: Some(&mut app.pbo_editor.tdc_dirty),
                        range: TDC_MIN_A..=TDC_MAX_A,
                        step_by: None,
                        suffix: " A",
                        live_display: opt_f32(tdc_live, "A"),
                        live_is_muted: false,
                        note: if tdc_supported {
                            fused_soft_note(
                                fused.as_ref().and_then(|f| f.vdd_tdc_a),
                                "Fused VDD",
                                TDC_SOFT_MAX_A,
                                "A",
                            )
                        } else {
                            capability_disabled_hint(tdc_live.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: tdc_cap,
                    },
                );

                // EDC
                let edc_supported = caps.set_edc.is_some();
                let edc_live = telem.as_ref().and_then(|t| t.edc_limit_a);
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "EDC Limit (A)",
                        supported: edc_supported,
                        enabled: &mut app.pbo_editor.edc_enabled,
                        value: &mut app.pbo_editor.edc_a,
                        dirty: Some(&mut app.pbo_editor.edc_dirty),
                        range: EDC_MIN_A..=EDC_MAX_A,
                        step_by: None,
                        suffix: " A",
                        live_display: opt_f32(edc_live, "A"),
                        live_is_muted: false,
                        note: if edc_supported {
                            fused_soft_note(
                                fused.as_ref().and_then(|f| f.edc_a),
                                "Fused EDC",
                                EDC_SOFT_MAX_A,
                                "A",
                            )
                        } else {
                            capability_disabled_hint(edc_live.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: edc_cap,
                    },
                );

                // Tctl Max (Hard ceiling 95C)
                let tctl_supported = caps.set_tctl.is_some();
                let tctl_live = telem.as_ref().and_then(|t| t.tctl_limit_c);
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "Tctl Thermal Max (°C)",
                        supported: tctl_supported,
                        enabled: &mut app.pbo_editor.tctl_enabled,
                        value: &mut app.pbo_editor.tctl_c,
                        dirty: Some(&mut app.pbo_editor.tctl_dirty),
                        range: TCTL_MIN_C..=TCTL_MAX_C,
                        step_by: None,
                        suffix: " °C",
                        live_display: opt_f32(tctl_live, "°C"),
                        live_is_muted: false,
                        note: if tctl_supported {
                            "Hard safety ceiling: 95 °C (non-overridable)".into()
                        } else {
                            capability_disabled_hint(tctl_live.is_some()).into()
                        },
                        note_color: if tctl_supported {
                            ACCENT_AMBER
                        } else {
                            TEXT_MUTED
                        },
                        board_cap: tctl_cap,
                    },
                );

                // PBO Scalar
                let scalar_supported = caps.set_pbo_scalar.is_some();
                let scalar_live = app
                    .pbo_status
                    .as_ref()
                    .and_then(|s| s.scalar)
                    .map(|s| format!("{s:.1}x"))
                    .unwrap_or_else(|| "—".into());
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "PBO Scalar (1–10x)",
                        supported: scalar_supported,
                        enabled: &mut app.pbo_editor.scalar_enabled,
                        value: &mut app.pbo_editor.scalar,
                        dirty: Some(&mut app.pbo_editor.scalar_dirty),
                        range: SCALAR_MIN..=SCALAR_MAX,
                        step_by: None,
                        suffix: "x",
                        live_display: scalar_live,
                        live_is_muted: false,
                        note: if scalar_supported {
                            format!("Soft max ≤ {SCALAR_SOFT_MAX}x")
                        } else {
                            capability_disabled_hint(caps.get_pbo_scalar.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: None,
                    },
                );

                // Fmax Boost Limit
                let fmax_supported = caps.set_fmax.is_some();
                let fmax_live = app
                    .pbo_status
                    .as_ref()
                    .and_then(|s| s.fmax_mhz)
                    .map(|m| format!("{m} MHz"))
                    .unwrap_or_else(|| "—".into());
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "Fmax Boost Limit (MHz)",
                        supported: fmax_supported,
                        enabled: &mut app.pbo_editor.fmax_enabled,
                        value: &mut app.pbo_editor.fmax_mhz,
                        dirty: Some(&mut app.pbo_editor.fmax_dirty),
                        range: FMAX_MIN_MHZ..=FMAX_MAX_MHZ,
                        step_by: Some(25.0),
                        suffix: " MHz",
                        live_display: fmax_live,
                        live_is_muted: false,
                        note: if fmax_supported {
                            "Soft delta ≤ +200 MHz above current".into()
                        } else {
                            capability_disabled_hint(caps.get_fmax.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: None,
                    },
                );
            });

        ui.add_space(10.0);
        ui.checkbox(
            &mut app.pbo_editor.allow_extreme,
            "Allow Extreme PBO values (above conservative soft limits; 95°C Tctl ceiling still enforced)",
        );

        if app.pbo_editor.has_any_extreme_for(caps) && !app.pbo_editor.allow_extreme {
            render_extreme_warning(
                ui,
                "One or more enabled PBO values exceed conservative soft limits; enable 'Allow Extreme' to override.",
            );
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let can_apply = !busy && app.pbo_editor.has_any_enabled_for(caps);
            if ui
                .add_enabled(
                    can_apply,
                    cta_button("Preview (Dry-Run) & Apply PBO", CTA_CRIMSON),
                )
                .clicked()
            {
                app.dispatch_cli(
                    CliAction::PboSet {
                        req: app.pbo_editor.build_request_for(caps),
                        allow_extreme: app.pbo_editor.allow_extreme,
                        dry_run: true,
                    },
                    ctx,
                );
            }

            if ui
                .add_enabled(!busy, egui::Button::new("Reset PBO"))
                .on_hover_text("Preview restoring fused factory / boot baseline PBO limits")
                .clicked()
            {
                app.dispatch_cli(CliAction::PboReset { dry_run: true }, ctx);
            }
        });
    });

    if hw.codename.is_apu() || caps.set_stapm_limit.is_some() {
        ui.add_space(12.0);
        render_apu_dptc_card(app, ui, ctx, &hw, fused.as_ref(), busy);
    }

    if let Some(status) = &app.pbo_status {
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Platform Flags & Fused Factory Telemetry").strong());
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                flag_pill(ui, "Overclockable", status.flags.overclockable);
                flag_pill(ui, "EXPO Active", status.flags.expo_active);
                flag_pill(ui, "LN2 Mode", status.flags.ln2_mode);
                optional_fused_pill(ui, "Fused TDP", status.fused.tdp_w, "W");
                optional_fused_pill(ui, "Fused TjMax", status.fused.tjmax_c, "°C");
                optional_fused_pill(ui, "Fused PPT", status.fused.ppt_w, "W");
                optional_fused_pill(ui, "Fused Fast", status.fused.fast_w, "W");
                optional_fused_pill(ui, "Fused Slow", status.fused.slow_w, "W");
                optional_fused_pill(ui, "Fused SoC TDC", status.fused.soc_tdc_a, "A");
                optional_fused_pill(ui, "Fused APU Slow", status.fused.apu_slow_w, "W");
            });
        });
    }
}
