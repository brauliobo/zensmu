//! Mobile / APU DPTC power, current, thermal, and power-mode controls card.

use crate::app::ZensmuApp;
use crate::views::theme::*;
use crate::views::widgets::{
    capability_disabled_hint, card_frame, cta_button, fused_soft_note, grid_header_row, opt_f32,
    render_extreme_warning, render_limit_slider_row, split_or_wrap_row, tinted_pill,
    LimitSliderRow,
};
use eframe::egui::{self, RichText, Ui};
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::ops::apu::{
    APU_FAST_POWER_SOFT_MAX_W, APU_POWER_MAX_W, APU_POWER_MIN_W, APU_POWER_SOFT_MAX_W,
    APU_SKIN_TEMP_MAX_C, APU_SKIN_TEMP_MIN_C, APU_SKIN_TEMP_SOFT_MAX_C, APU_SOC_EDC_MAX_A,
    APU_SOC_EDC_MIN_A, APU_SOC_EDC_SOFT_MAX_A, APU_SOC_TDC_MAX_A, APU_SOC_TDC_MIN_A,
    APU_SOC_TDC_SOFT_MAX_A, APU_TIME_MAX_S, APU_TIME_MIN_S,
};
use zensmu_core::ops::pbo::FusedLimits;
use zensmu_core::types::ApuPowerMode;

pub(super) fn render_apu_dptc_card(
    app: &mut ZensmuApp,
    ui: &mut Ui,
    ctx: &egui::Context,
    hw: &HardwareContext,
    fused: Option<&FusedLimits>,
    busy: bool,
) {
    let caps = &hw.capabilities;
    let stapm_cap = app.board_caps.get("apu.stapm_w").copied();
    let fast_cap = app.board_caps.get("apu.fast_w").copied();
    let slow_cap = app.board_caps.get("apu.slow_w").copied();
    let apu_slow_cap = app.board_caps.get("apu.apu_slow_w").copied();
    let tdc_soc_cap = app.board_caps.get("apu.tdc_soc_a").copied();
    let edc_soc_cap = app.board_caps.get("apu.edc_soc_a").copied();
    let apu_skin_cap = app.board_caps.get("apu.apu_skin_temp_c").copied();
    let dgpu_skin_cap = app.board_caps.get("apu.dgpu_skin_temp_c").copied();

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "apu_dptc_header",
            |ui| {
                ui.label(RichText::new("APU DPTC Limits").strong());
                tinted_pill(
                    ui,
                    &format!("{} · Mobile Power & Thermal", hw.codename.display_name()),
                    ACCENT_CYAN,
                    PILL_ALPHA_CYAN,
                );
            },
            |ui| {
                if ui
                    .button("Sync from Live")
                    .on_hover_text("Reset edited APU sliders to live PM table hardware limits")
                    .clicked()
                {
                    app.apu_editor.clear_dirty();
                    if let Some(snap) = app.latest_telemetry.clone() {
                        app.apu_editor.sync_from_telemetry(&snap);
                    }
                }
            },
        );

        ui.add_space(10.0);

        egui::Grid::new("apu_dptc_controls_grid")
            .num_columns(4)
            .striped(true)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                grid_header_row(
                    ui,
                    &["Parameter", "Target Value", "Current Live", "Fused / Guard"],
                );

                // STAPM Limit (W)
                let stapm_supported = caps.set_stapm_limit.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "STAPM Limit (W)",
                        supported: stapm_supported,
                        enabled: &mut app.apu_editor.stapm_enabled,
                        value: &mut app.apu_editor.stapm_w,
                        dirty: Some(&mut app.apu_editor.stapm_dirty),
                        range: APU_POWER_MIN_W..=APU_POWER_MAX_W,
                        step_by: None,
                        suffix: " W",
                        live_display: opt_f32(app.apu_editor.live_stapm_w, "W"),
                        live_is_muted: false,
                        note: if stapm_supported {
                            format!("Soft max ≤ {APU_POWER_SOFT_MAX_W} W")
                        } else {
                            capability_disabled_hint(app.apu_editor.live_stapm_w.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: stapm_cap,
                    },
                );

                // Fast PPT (W)
                let fast_supported = hw.codename.is_apu() && caps.set_ppt.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "Fast PPT (W)",
                        supported: fast_supported,
                        enabled: &mut app.apu_editor.fast_enabled,
                        value: &mut app.apu_editor.fast_w,
                        dirty: Some(&mut app.apu_editor.fast_dirty),
                        range: APU_POWER_MIN_W..=APU_POWER_MAX_W,
                        step_by: None,
                        suffix: " W",
                        live_display: opt_f32(app.apu_editor.live_fast_w, "W"),
                        live_is_muted: false,
                        note: if fast_supported {
                            fused_soft_note(
                                fused.and_then(|f| f.fast_w),
                                "Fused",
                                APU_FAST_POWER_SOFT_MAX_W,
                                "W",
                            )
                        } else {
                            capability_disabled_hint(app.apu_editor.live_fast_w.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: fast_cap,
                    },
                );

                // Slow PPT (W)
                let slow_supported = caps.set_slow_limit.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "Slow PPT (W)",
                        supported: slow_supported,
                        enabled: &mut app.apu_editor.slow_enabled,
                        value: &mut app.apu_editor.slow_w,
                        dirty: Some(&mut app.apu_editor.slow_dirty),
                        range: APU_POWER_MIN_W..=APU_POWER_MAX_W,
                        step_by: None,
                        suffix: " W",
                        live_display: opt_f32(app.apu_editor.live_slow_w, "W"),
                        live_is_muted: false,
                        note: if slow_supported {
                            fused_soft_note(
                                fused.and_then(|f| f.slow_w),
                                "Fused",
                                APU_POWER_SOFT_MAX_W,
                                "W",
                            )
                        } else {
                            capability_disabled_hint(app.apu_editor.live_slow_w.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: slow_cap,
                    },
                );

                // APU Slow (W)
                let apu_slow_supported = caps.set_apu_slow_limit.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "APU Slow (W)",
                        supported: apu_slow_supported,
                        enabled: &mut app.apu_editor.apu_slow_enabled,
                        value: &mut app.apu_editor.apu_slow_w,
                        dirty: Some(&mut app.apu_editor.apu_slow_dirty),
                        range: APU_POWER_MIN_W..=APU_POWER_MAX_W,
                        step_by: None,
                        suffix: " W",
                        live_display: opt_f32(app.apu_editor.live_apu_slow_w, "W"),
                        live_is_muted: false,
                        note: if apu_slow_supported {
                            fused_soft_note(
                                fused.and_then(|f| f.apu_slow_w),
                                "Fused",
                                APU_POWER_SOFT_MAX_W,
                                "W",
                            )
                        } else {
                            capability_disabled_hint(app.apu_editor.live_apu_slow_w.is_some())
                                .into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: apu_slow_cap,
                    },
                );

                // STAPM Time Constant (s)
                let stapm_time_supported = caps.set_stapm_time.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "STAPM Time Constant (s)",
                        supported: stapm_time_supported,
                        enabled: &mut app.apu_editor.stapm_time_enabled,
                        value: &mut app.apu_editor.stapm_time_s,
                        dirty: None,
                        range: APU_TIME_MIN_S..=APU_TIME_MAX_S,
                        step_by: None,
                        suffix: " s",
                        live_display: "—".into(),
                        live_is_muted: true,
                        note: if stapm_time_supported {
                            format!("Range {APU_TIME_MIN_S}–{APU_TIME_MAX_S} s")
                        } else {
                            capability_disabled_hint(false).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: None,
                    },
                );

                // Slow Time Constant (s)
                let slow_time_supported = caps.set_slow_time.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "Slow Time Constant (s)",
                        supported: slow_time_supported,
                        enabled: &mut app.apu_editor.slow_time_enabled,
                        value: &mut app.apu_editor.slow_time_s,
                        dirty: None,
                        range: APU_TIME_MIN_S..=APU_TIME_MAX_S,
                        step_by: None,
                        suffix: " s",
                        live_display: "—".into(),
                        live_is_muted: true,
                        note: if slow_time_supported {
                            format!("Range {APU_TIME_MIN_S}–{APU_TIME_MAX_S} s")
                        } else {
                            capability_disabled_hint(false).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: None,
                    },
                );

                // SoC TDC Limit (A)
                let tdc_soc_supported = caps.set_tdc_soc.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "SoC TDC Limit (A)",
                        supported: tdc_soc_supported,
                        enabled: &mut app.apu_editor.tdc_soc_enabled,
                        value: &mut app.apu_editor.tdc_soc_a,
                        dirty: Some(&mut app.apu_editor.tdc_soc_dirty),
                        range: APU_SOC_TDC_MIN_A..=APU_SOC_TDC_MAX_A,
                        step_by: None,
                        suffix: " A",
                        live_display: opt_f32(app.apu_editor.live_tdc_soc_a, "A"),
                        live_is_muted: false,
                        note: if tdc_soc_supported {
                            fused_soft_note(
                                fused.and_then(|f| f.soc_tdc_a),
                                "Fused SoC",
                                APU_SOC_TDC_SOFT_MAX_A,
                                "A",
                            )
                        } else {
                            capability_disabled_hint(app.apu_editor.live_tdc_soc_a.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: tdc_soc_cap,
                    },
                );

                // SoC EDC Limit (A)
                let edc_soc_supported = caps.set_edc_soc.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "SoC EDC Limit (A)",
                        supported: edc_soc_supported,
                        enabled: &mut app.apu_editor.edc_soc_enabled,
                        value: &mut app.apu_editor.edc_soc_a,
                        dirty: Some(&mut app.apu_editor.edc_soc_dirty),
                        range: APU_SOC_EDC_MIN_A..=APU_SOC_EDC_MAX_A,
                        step_by: None,
                        suffix: " A",
                        live_display: opt_f32(app.apu_editor.live_edc_soc_a, "A"),
                        live_is_muted: false,
                        note: if edc_soc_supported {
                            format!("Soft max ≤ {APU_SOC_EDC_SOFT_MAX_A} A")
                        } else {
                            capability_disabled_hint(app.apu_editor.live_edc_soc_a.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: edc_soc_cap,
                    },
                );

                // APU Skin Temp (°C)
                let apu_skin_supported = caps.set_apu_skin_temp.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "APU Skin Temp (°C)",
                        supported: apu_skin_supported,
                        enabled: &mut app.apu_editor.apu_skin_temp_enabled,
                        value: &mut app.apu_editor.apu_skin_temp_c,
                        dirty: Some(&mut app.apu_editor.apu_skin_temp_dirty),
                        range: APU_SKIN_TEMP_MIN_C..=APU_SKIN_TEMP_MAX_C,
                        step_by: None,
                        suffix: " °C",
                        live_display: opt_f32(app.apu_editor.live_apu_skin_temp_c, "°C"),
                        live_is_muted: false,
                        note: if apu_skin_supported {
                            format!("Soft max ≤ {APU_SKIN_TEMP_SOFT_MAX_C} °C")
                        } else {
                            capability_disabled_hint(app.apu_editor.live_apu_skin_temp_c.is_some())
                                .into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: apu_skin_cap,
                    },
                );

                // dGPU Skin Temp (°C)
                let dgpu_skin_supported = caps.set_dgpu_skin_temp.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "dGPU Skin Temp (°C)",
                        supported: dgpu_skin_supported,
                        enabled: &mut app.apu_editor.dgpu_skin_temp_enabled,
                        value: &mut app.apu_editor.dgpu_skin_temp_c,
                        dirty: Some(&mut app.apu_editor.dgpu_skin_temp_dirty),
                        range: APU_SKIN_TEMP_MIN_C..=APU_SKIN_TEMP_MAX_C,
                        step_by: None,
                        suffix: " °C",
                        live_display: opt_f32(app.apu_editor.live_dgpu_skin_temp_c, "°C"),
                        live_is_muted: false,
                        note: if dgpu_skin_supported {
                            format!("Soft max ≤ {APU_SKIN_TEMP_SOFT_MAX_C} °C")
                        } else {
                            capability_disabled_hint(
                                app.apu_editor.live_dgpu_skin_temp_c.is_some(),
                            )
                            .into()
                        },
                        note_color: TEXT_MUTED,
                        board_cap: dgpu_skin_cap,
                    },
                );

                // Power Mode selector
                {
                    let has_perf = caps.set_max_performance.is_some();
                    let has_save = caps.set_power_saving.is_some();
                    let supported = has_perf || has_save;
                    if !supported {
                        app.apu_editor.power_mode = None;
                    }
                    ui.label(if supported {
                        RichText::new("Power Mode")
                    } else {
                        RichText::new("Power Mode").color(TEXT_MUTED)
                    });
                    ui.add_enabled_ui(supported, |ui| {
                        let selected_label = match app.apu_editor.power_mode {
                            None => "Keep Current",
                            Some(ApuPowerMode::MaxPerformance) => "Max Performance",
                            Some(ApuPowerMode::PowerSaving) => "Power Saving",
                        };
                        egui::ComboBox::from_id_salt("apu_power_mode_combo")
                            .width(170.0)
                            .selected_text(selected_label)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut app.apu_editor.power_mode,
                                    None,
                                    "Keep Current",
                                );
                                if has_perf {
                                    ui.selectable_value(
                                        &mut app.apu_editor.power_mode,
                                        Some(ApuPowerMode::MaxPerformance),
                                        "Max Performance",
                                    );
                                }
                                if has_save {
                                    ui.selectable_value(
                                        &mut app.apu_editor.power_mode,
                                        Some(ApuPowerMode::PowerSaving),
                                        "Power Saving",
                                    );
                                }
                            });
                    });
                    ui.label(RichText::new("—").monospace().color(TEXT_MUTED));
                    let note = if supported {
                        "SMU performance / efficiency bias"
                    } else {
                        capability_disabled_hint(false)
                    };
                    ui.label(RichText::new(note).small().color(TEXT_MUTED));
                    ui.end_row();
                }
            });

        ui.add_space(10.0);
        ui.checkbox(
            &mut app.apu_editor.allow_extreme,
            "Allow Extreme APU limits (above conservative soft power, current, and skin temp limits)",
        );

        let any_extreme = app.apu_editor.has_any_extreme_for(hw);
        if any_extreme && !app.apu_editor.allow_extreme {
            render_extreme_warning(
                ui,
                "One or more enabled APU values exceed conservative soft limits; enable 'Allow Extreme' to proceed.",
            );
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let can_apply = !busy
                && app.apu_editor.has_any_enabled_for(hw)
                && (!any_extreme || app.apu_editor.allow_extreme);
            if ui
                .add_enabled(
                    can_apply,
                    cta_button("Preview (Dry-Run) & Apply APU Limits", CTA_CRIMSON),
                )
                .clicked()
            {
                app.preview_apply_apu(ctx);
            }
        });
    });
}
