//! Dashboard quick telemetry recording control strip.

use crate::app::{Tab, ToastLevel, ZensmuApp};
use crate::views::theme::*;
use crate::views::widgets::{
    card_frame, cta_button, format_duration_mmss, format_record_format_label,
    format_record_format_short, handle_open_recordings_folder, muted_pill, split_or_wrap_row,
    tinted_pill,
};
use eframe::egui::{self, RichText, Ui};
use zensmu_core::recorder::RecordFormat;

pub(super) fn render_dashboard_recorder_strip(app: &mut ZensmuApp, ui: &mut Ui) {
    let has_hw = app.hw.is_some();
    let is_recording = app.recorder.is_recording();
    let current_format = app.recorder.format;
    let daemon_active = app.recorder.daemon_status.active;
    let daemon_enabled = app.recorder.daemon_status.enabled;
    let elapsed = format_duration_mmss(app.recorder.elapsed_secs());
    let live_samples = app.recorder.live_samples();
    let active_path = app.recorder.active_path().map(|p| p.display().to_string());
    let last_saved_path = app
        .recorder
        .last_saved_path
        .as_ref()
        .map(|p| p.display().to_string());
    let last_saved_samples = app.recorder.last_saved_samples;

    let mut toggle_clicked = false;
    let mut open_folder_clicked = false;
    let mut configure_clicked = false;
    let mut copy_path: Option<String> = None;

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "dashboard_recorder_strip",
            |ui| {
                ui.label(RichText::new("Telemetry Recording").strong());
                if is_recording {
                    tinted_pill(
                        ui,
                        &format!(
                            "REC · {elapsed} · {live_samples} samples · {}",
                            format_record_format_short(current_format)
                        ),
                        ACCENT_CRIMSON,
                        PILL_ALPHA_CRIMSON,
                    );
                } else if daemon_active {
                    tinted_pill(ui, "Daemon: ACTIVE", ACCENT_EMERALD, PILL_ALPHA_EMERALD);
                } else if daemon_enabled {
                    tinted_pill(ui, "Daemon: ENABLED", ACCENT_CYAN, PILL_ALPHA_CYAN);
                } else if last_saved_path.is_some() {
                    tinted_pill(
                        ui,
                        &format!("Last: {last_saved_samples} samples"),
                        ACCENT_CYAN,
                        PILL_ALPHA_CYAN,
                    );
                } else {
                    muted_pill(ui, "Idle · unprivileged");
                }
            },
            |ui| {
                ui.add_enabled_ui(!is_recording, |ui| {
                    egui::ComboBox::from_id_salt("dashboard_recorder_format_combo")
                        .width(126.0)
                        .selected_text(format_record_format_label(app.recorder.format))
                        .show_ui(ui, |ui| {
                            for fmt in [RecordFormat::Csv, RecordFormat::Jsonl, RecordFormat::Pts] {
                                ui.selectable_value(
                                    &mut app.recorder.format,
                                    fmt,
                                    format_record_format_label(fmt),
                                );
                            }
                        });
                });

                if is_recording {
                    if ui
                        .add(cta_button("■ Stop Recording", CTA_CRIMSON))
                        .on_hover_text("Flush and stop active telemetry recording")
                        .clicked()
                    {
                        toggle_clicked = true;
                    }
                } else if ui
                    .add_enabled(has_hw, cta_button("● Start Recording", CTA_EMERALD))
                    .on_hover_text("Start recording live PM table telemetry without root")
                    .clicked()
                {
                    toggle_clicked = true;
                }

                if ui
                    .button("Configure…")
                    .on_hover_text(
                        "Set workload tag, PTS result name, or toggle the background daemon",
                    )
                    .clicked()
                {
                    configure_clicked = true;
                }
            },
        );

        let display_path = active_path.or(last_saved_path);
        if let Some(path_str) = display_path {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                let prefix = if is_recording {
                    "Writing:"
                } else {
                    "Last saved:"
                };
                ui.label(RichText::new(prefix).small().color(TEXT_MUTED));
                ui.label(
                    RichText::new(&path_str)
                        .small()
                        .monospace()
                        .color(TEXT_PRIMARY),
                );
                if ui.add(egui::Button::new("Copy Path").small()).clicked() {
                    copy_path = Some(path_str);
                }
                if ui.add(egui::Button::new("Open Folder").small()).clicked() {
                    open_folder_clicked = true;
                }
            });
        }
    });

    if toggle_clicked {
        app.toggle_recording();
    }
    if let Some(path_str) = copy_path {
        ui.ctx().copy_text(path_str);
        app.set_toast(ToastLevel::Info, "Copied recording path to clipboard.");
    }
    if open_folder_clicked {
        handle_open_recordings_folder(app);
    }
    if configure_clicked {
        app.active_tab = Tab::ProfilesSafety;
        app.refresh_safety_files();
        app.refresh_persist_status();
        app.refresh_record_daemon_status();
        app.refresh_exporter_daemon_status();
    }
}
