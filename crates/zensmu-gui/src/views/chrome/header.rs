//! Top command bar with CPU identity, global actions, and status strip.

use crate::app::{
    Tab, ZensmuApp, POLL_INTERVAL_OPTIONS_MS, UI_ZOOM_DEFAULT, UI_ZOOM_MAX, UI_ZOOM_MIN,
    UI_ZOOM_STEP,
};
use crate::elevate::CliAction;
use crate::views::theme::*;
use crate::views::widgets::{
    compact_cpu_label, cta_button, format_duration_mmss, format_interval_label,
    format_record_format_short, muted_pill, panel_frame, split_or_wrap_row, tinted_pill,
};
use eframe::egui::{self, RichText, Stroke};

/// Top command bar with brand, compact CPU identity, global actions, and a clean non-overlapping status strip.
pub fn render_top_bar(app: &mut ZensmuApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("top_bar")
        .frame(panel_frame(egui::Margin::symmetric(14, 8)))
        .show(ctx, |ui| {
            let busy = app.runner.is_busy();
            let has_hw = app.hw.is_some();
            let cpu_badge = app.hw.as_ref().map(|hw| {
                let full_brand = hw
                    .cpuid
                    .as_ref()
                    .map(|c| c.brand.as_str())
                    .filter(|b| !b.is_empty())
                    .unwrap_or_else(|| hw.codename.display_name());
                let short_cpu = compact_cpu_label(full_brand);
                let badge = format!("{short_cpu} · SMU {}", hw.driver.smu_version);
                let hover = format!(
                    "{full_brand}\nCodename: {} · {}C/{}T ({} CCD) · SMU {}",
                    hw.codename.slug(),
                    hw.topology.physical_cores,
                    hw.topology.logical_cpus,
                    hw.topology.ccd_count,
                    hw.driver.smu_version
                );
                (badge, hover)
            });

            // Row 1: Brand + compact CPU pill (left) and global telemetry/SMU/zoom controls (right).
            split_or_wrap_row(
                ui,
                "top_bar_primary_row",
                |ui| {
                    ui.label(
                        RichText::new("zensmu")
                            .strong()
                            .size(21.0)
                            .color(ACCENT_CRIMSON),
                    );
                    ui.separator();

                    if let Some((badge, hover)) = &cpu_badge {
                        egui::Frame::NONE
                            .fill(BG_ELEVATED)
                            .stroke(Stroke::new(1.0_f32, BORDER_CONTROL))
                            .corner_radius(egui::CornerRadius::same(12))
                            .inner_margin(egui::Margin::symmetric(9, 3))
                            .show(ui, |ui| {
                                ui.label(RichText::new(badge).small().strong().color(TEXT_PRIMARY))
                                    .on_hover_text(hover);
                            });
                    } else {
                        tinted_pill(ui, "ryzen_smu not detected", ACCENT_AMBER, PILL_ALPHA_AMBER);
                    }
                },
                |ui| {
                    let mut selected_ms = app.poll_interval_ms;
                    egui::ComboBox::from_id_salt("poll_rate_combo")
                        .width(76.0)
                        .selected_text(format_interval_label(selected_ms))
                        .show_ui(ui, |ui| {
                            for ms in POLL_INTERVAL_OPTIONS_MS {
                                ui.selectable_value(
                                    &mut selected_ms,
                                    ms,
                                    format_interval_label(ms),
                                );
                            }
                        });
                    if selected_ms != app.poll_interval_ms {
                        app.set_poll_interval_ms(selected_ms);
                    }

                    let pause_label = if app.telemetry_paused {
                        "Resume"
                    } else {
                        "Pause"
                    };
                    if ui
                        .add_enabled(has_hw, egui::Button::new(pause_label))
                        .on_hover_text("Pause or resume live PM table telemetry polling")
                        .clicked()
                    {
                        app.set_telemetry_paused(!app.telemetry_paused);
                    }

                    ui.separator();

                    if ui
                        .add_enabled(has_hw && !busy, egui::Button::new("Refresh"))
                        .on_hover_text("Read current CO, Curve Shaper, and PBO state via pkexec")
                        .clicked()
                    {
                        app.dispatch_cli(CliAction::Status, ctx);
                    }

                    if ui
                        .add_enabled(has_hw && !busy, egui::Button::new("Rollback"))
                        .on_hover_text("Preview rollback to last pre-change snapshot (dry-run first)")
                        .clicked()
                    {
                        app.dispatch_cli(CliAction::Rollback { dry_run: true }, ctx);
                    }

                    if ui
                        .add_enabled(has_hw && !busy, egui::Button::new("Reset"))
                        .on_hover_text("Preview reset to boot baseline (dry-run first)")
                        .clicked()
                    {
                        app.dispatch_cli(CliAction::Reset { dry_run: true }, ctx);
                    }

                    ui.separator();

                    if ui
                        .add_enabled(
                            app.ui_zoom > UI_ZOOM_MIN + 0.001,
                            egui::Button::new("A-").small(),
                        )
                        .on_hover_text("Decrease UI scale (Ctrl -)")
                        .clicked()
                    {
                        app.adjust_ui_zoom(-UI_ZOOM_STEP, ctx);
                    }

                    let zoom_pct = format!("{:.0}%", app.ui_zoom * 100.0);
                    if ui
                        .add(egui::Button::new(RichText::new(zoom_pct).monospace()).small())
                        .on_hover_text("Reset UI scale to 100% (Ctrl 0)")
                        .clicked()
                    {
                        app.set_ui_zoom(UI_ZOOM_DEFAULT, ctx);
                    }

                    if ui
                        .add_enabled(
                            app.ui_zoom < UI_ZOOM_MAX - 0.001,
                            egui::Button::new("A+").small(),
                        )
                        .on_hover_text("Increase UI scale (Ctrl +)")
                        .clicked()
                    {
                        app.adjust_ui_zoom(UI_ZOOM_STEP, ctx);
                    }
                },
            );

            ui.add_space(4.0);

            // Row 2: Compact wrapped status strip (privilege, boot persistence, crash guard, topology).
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);

                if app.is_root {
                    tinted_pill(
                        ui,
                        "ROOT (uid 0) · run unprivileged",
                        ACCENT_CRIMSON,
                        PILL_ALPHA_CRIMSON,
                    );
                } else {
                    tinted_pill(
                        ui,
                        "Unprivileged · pkexec on apply",
                        ACCENT_EMERALD,
                        PILL_ALPHA_EMERALD,
                    );
                }

                let boot_on = app.persist_status.service_enabled
                    && app.persist_status.active_profile_exists;
                let (boot_text, boot_fg, boot_bg, boot_stroke, boot_hover) = if boot_on {
                    (
                        "Boot: ON",
                        ACCENT_EMERALD,
                        tinted_bg(ACCENT_EMERALD, PILL_ALPHA_EMERALD),
                        ACCENT_EMERALD.linear_multiply(0.55),
                        "Boot persistence is active (zensmu-apply.service enabled with active profile). Click to manage in Profiles & Safety.",
                    )
                } else {
                    (
                        "Boot: OFF",
                        TEXT_MUTED,
                        BG_ELEVATED,
                        BORDER_CONTROL,
                        "Boot persistence is inactive. Click to configure in Profiles & Safety.",
                    )
                };
                if ui
                    .add(
                        egui::Button::new(RichText::new(boot_text).small().strong().color(boot_fg))
                            .fill(boot_bg)
                            .stroke(Stroke::new(1.0_f32, boot_stroke))
                            .corner_radius(egui::CornerRadius::same(12)),
                    )
                    .on_hover_text(boot_hover)
                    .clicked()
                {
                    app.active_tab = Tab::ProfilesSafety;
                    app.refresh_safety_files();
                    app.refresh_persist_status();
                    app.refresh_record_daemon_status();
                    app.refresh_exporter_daemon_status();
                }

                let (guard_text, guard_fg, guard_bg, guard_stroke, guard_hover) =
                    if app.profile_editor.boot_guard_tripped {
                        (
                            "Guard: TRIPPED",
                            ACCENT_CRIMSON,
                            tinted_bg(ACCENT_CRIMSON, PILL_ALPHA_CRIMSON),
                            ACCENT_CRIMSON.linear_multiply(0.60),
                            "Boot-loop crash guard tripped — auto-apply halted. Click to inspect or clear in Profiles & Safety.",
                        )
                    } else {
                        (
                            "Guard: Clean",
                            ACCENT_EMERALD,
                            tinted_bg(ACCENT_EMERALD, PILL_ALPHA_EMERALD),
                            ACCENT_EMERALD.linear_multiply(0.55),
                            "Boot-loop crash guard is armed and clean. Click to view safety status.",
                        )
                    };
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(guard_text).small().strong().color(guard_fg),
                        )
                        .fill(guard_bg)
                        .stroke(Stroke::new(1.0_f32, guard_stroke))
                        .corner_radius(egui::CornerRadius::same(12)),
                    )
                    .on_hover_text(guard_hover)
                    .clicked()
                {
                    app.active_tab = Tab::ProfilesSafety;
                    app.refresh_safety_files();
                    app.refresh_persist_status();
                    app.refresh_record_daemon_status();
                    app.refresh_exporter_daemon_status();
                }

                if let Some(hw) = &app.hw {
                    muted_pill(
                        ui,
                        &format!(
                            "{} · {}C/{}T",
                            hw.codename.slug(),
                            hw.topology.physical_cores,
                            hw.topology.logical_cpus
                        ),
                    );
                }

                if app.recorder.is_recording() {
                    let elapsed = format_duration_mmss(app.recorder.elapsed_secs());
                    let sample_count = app.recorder.live_samples();
                    let label = format!("■ Stop REC {elapsed} ({sample_count})");
                    let hover = app
                        .recorder
                        .active_path()
                        .map(|p| {
                            format!(
                                "Recording {} telemetry ({sample_count} samples) to {}\nClick to flush and stop.",
                                format_record_format_short(app.recorder.format),
                                p.display()
                            )
                        })
                        .unwrap_or_else(|| "Stop active telemetry recording and flush to disk.".into());
                    if ui
                        .add(
                            cta_button(label, CTA_CRIMSON)
                                .small()
                                .corner_radius(egui::CornerRadius::same(12)),
                        )
                        .on_hover_text(hover)
                        .clicked()
                    {
                        app.toggle_recording();
                    }
                } else {
                    let hover = if has_hw {
                        format!(
                            "Start unprivileged {} telemetry recording (configure format, tag & daemon in Profiles & Safety)",
                            format_record_format_short(app.recorder.format)
                        )
                    } else {
                        "Detect ryzen_smu hardware to start live telemetry recording".into()
                    };
                    if ui
                        .add_enabled(
                            has_hw,
                            egui::Button::new(
                                RichText::new("● Record")
                                    .small()
                                    .strong()
                                    .color(ACCENT_CRIMSON),
                            )
                            .fill(tinted_bg(ACCENT_CRIMSON, PILL_ALPHA_CRIMSON))
                            .stroke(Stroke::new(1.0_f32, ACCENT_CRIMSON.linear_multiply(0.55)))
                            .corner_radius(egui::CornerRadius::same(12)),
                        )
                        .on_hover_text(hover)
                        .clicked()
                    {
                        app.toggle_recording();
                    }
                }

                if ui
                    .add(
                        egui::Button::new(
                            RichText::new("Report CPU / Issue")
                                .small()
                                .strong()
                                .color(ACCENT_CYAN),
                        )
                        .fill(tinted_bg(ACCENT_CYAN, PILL_ALPHA_CYAN))
                        .stroke(Stroke::new(1.0_f32, ACCENT_CYAN.linear_multiply(0.55)))
                        .corner_radius(egui::CornerRadius::same(12)),
                    )
                    .on_hover_text(
                        "Collect privacy-safe hardware diagnostics & open a GitHub issue",
                    )
                    .clicked()
                {
                    app.open_report_modal();
                }
            });
        });
}
