//! Unprivileged telemetry recorder and background user daemon card.

use crate::app::{ToastLevel, ZensmuApp};
use crate::views::theme::*;
use crate::views::widgets::{
    card_frame, cta_button, format_bytes_compact, format_duration_mmss, format_record_format_label,
    format_record_format_short, handle_open_recordings_folder, muted_pill, split_or_wrap_row,
    tinted_pill,
};
use eframe::egui::{self, RichText, Ui};
use zensmu_core::recorder::{
    default_daemon_recording_path, pts_result_path, RecordFormat, USER_RECORD_SERVICE_NAME,
};

pub(super) fn render_recorder_card(
    app: &mut ZensmuApp,
    ui: &mut Ui,
    ctx: &egui::Context,
    has_hw: bool,
) {
    let is_recording = app.recorder.is_recording();
    let current_format = app.recorder.format;
    let elapsed = format_duration_mmss(app.recorder.elapsed_secs());
    let live_samples = app.recorder.live_samples();
    let live_bytes = app.recorder.live_bytes();
    let active_path = app.recorder.active_path().map(|p| p.display().to_string());
    let last_saved_path = app
        .recorder
        .last_saved_path
        .as_ref()
        .map(|p| p.display().to_string());
    let last_saved_samples = app.recorder.last_saved_samples;
    let daemon = app.recorder.daemon_status.clone();
    let daemon_service = if daemon.service_name.is_empty() {
        USER_RECORD_SERVICE_NAME
    } else {
        daemon.service_name.as_str()
    };
    let daemon_output_path = if daemon.default_output_path.is_empty() {
        default_daemon_recording_path().display().to_string()
    } else {
        daemon.default_output_path.clone()
    };

    let mut refresh_daemon = false;
    let mut toggle_recording = false;
    let mut open_folder = false;
    let mut toggle_daemon = false;
    let mut copy_path: Option<(String, &'static str)> = None;

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "recorder_card_header",
            |ui| {
                ui.label(RichText::new("Telemetry Recording & Background Daemon").strong());
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
                } else {
                    tinted_pill(
                        ui,
                        "Unprivileged · no pkexec",
                        ACCENT_EMERALD,
                        PILL_ALPHA_EMERALD,
                    );
                }
            },
            |ui| {
                if ui
                    .button("Refresh Status")
                    .on_hover_text("Re-check user systemd daemon status (unprivileged)")
                    .clicked()
                {
                    refresh_daemon = true;
                }
            },
        );

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Format:").small().color(TEXT_MUTED));
            ui.add_enabled_ui(!is_recording, |ui| {
                egui::ComboBox::from_id_salt("recorder_card_format_combo")
                    .width(150.0)
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

            ui.label(RichText::new("Workload Tag:").small().color(TEXT_MUTED));
            ui.add_enabled(
                !is_recording,
                egui::TextEdit::singleline(&mut app.recorder.tag)
                    .desired_width(170.0)
                    .hint_text("e.g. prime95, co-20"),
            );
        });

        if app.recorder.format == RecordFormat::Pts {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("PTS Result Name:").small().color(TEXT_MUTED));
                ui.add_enabled(
                    !is_recording,
                    egui::TextEdit::singleline(&mut app.recorder.pts_result_name)
                        .desired_width(210.0)
                        .hint_text("optional (e.g. co-20-run1)"),
                );
                let preview = {
                    let trimmed = app.recorder.pts_result_name.trim();
                    if trimmed.is_empty() {
                        "~/.phoronix-test-suite/test-results/<name>/composite.xml".to_string()
                    } else {
                        pts_result_path(trimmed).display().to_string()
                    }
                };
                ui.label(
                    RichText::new(preview)
                        .small()
                        .monospace()
                        .color(TEXT_MUTED),
                );
            });
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if is_recording {
                if ui
                    .add(cta_button(
                        format!("■ Stop Recording ({elapsed} · {live_samples} samples)"),
                        CTA_CRIMSON,
                    ))
                    .on_hover_text("Flush active recording to disk and stop")
                    .clicked()
                {
                    toggle_recording = true;
                }
            } else {
                let hover = if has_hw {
                    "Start live telemetry recording to disk (unprivileged)"
                } else {
                    "Detect ryzen_smu hardware to start live telemetry recording"
                };
                if ui
                    .add_enabled(has_hw, cta_button("● Start Recording", CTA_EMERALD))
                    .on_hover_text(hover)
                    .clicked()
                {
                    toggle_recording = true;
                }
            }

            if ui
                .button("Open Folder")
                .on_hover_text("Ensure default recordings directory exists and open via xdg-open")
                .clicked()
            {
                open_folder = true;
            }
        });

        if let Some(path_str) = active_path {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Active ({live_samples} samples · {}):",
                        format_bytes_compact(live_bytes)
                    ))
                    .small()
                    .strong()
                    .color(ACCENT_CRIMSON),
                );
                ui.label(
                    RichText::new(&path_str)
                        .small()
                        .monospace()
                        .color(TEXT_PRIMARY),
                );
                if ui.add(egui::Button::new("Copy Path").small()).clicked() {
                    copy_path = Some((path_str, "Copied active recording path to clipboard."));
                }
            });
        } else if let Some(path_str) = last_saved_path {
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!("Last saved ({last_saved_samples} samples):"))
                        .small()
                        .strong()
                        .color(ACCENT_EMERALD),
                );
                ui.label(
                    RichText::new(&path_str)
                        .small()
                        .monospace()
                        .color(TEXT_PRIMARY),
                );
                if ui.add(egui::Button::new("Copy Path").small()).clicked() {
                    copy_path = Some((path_str, "Copied saved recording path to clipboard."));
                }
            });
        }

        ui.add_space(10.0);
        ui.separator();
        ui.add_space(8.0);

        split_or_wrap_row(
            ui,
            "recorder_daemon_header",
            |ui| {
                ui.label(
                    RichText::new(format!("Background Daemon ({daemon_service})")).strong(),
                );
                if daemon.active {
                    tinted_pill(ui, "Daemon: ACTIVE", ACCENT_EMERALD, PILL_ALPHA_EMERALD);
                } else if daemon.enabled {
                    tinted_pill(ui, "Daemon: ENABLED", ACCENT_CYAN, PILL_ALPHA_CYAN);
                } else if !daemon.installed {
                    tinted_pill(ui, "Daemon: NOT INSTALLED", ACCENT_AMBER, PILL_ALPHA_AMBER);
                } else {
                    muted_pill(ui, "Daemon: OFF");
                }
            },
            |ui| {
                let hover = if !daemon.installed {
                    "zensmu-record.service is not installed in /usr/lib/systemd/user/ or ~/.config/systemd/user/"
                } else if daemon.enabled {
                    "Run `systemctl --user disable --now zensmu-record.service` (unprivileged)"
                } else {
                    "Run `systemctl --user enable --now zensmu-record.service` (unprivileged)"
                };
                if daemon.enabled {
                    if ui
                        .add_enabled(
                            daemon.installed,
                            egui::Button::new("Disable Background Recorder"),
                        )
                        .on_hover_text(hover)
                        .clicked()
                    {
                        toggle_daemon = true;
                    }
                } else if ui
                    .add_enabled(
                        daemon.installed,
                        cta_button("Enable Background Recorder", CTA_EMERALD),
                    )
                    .on_hover_text(hover)
                    .clicked()
                {
                    toggle_daemon = true;
                }
            },
        );

        ui.add_space(6.0);
        egui::Grid::new("recorder_daemon_grid")
            .num_columns(2)
            .striped(true)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                ui.label(RichText::new("User service state").small().color(TEXT_MUTED));
                let state_color = if daemon.active {
                    ACCENT_EMERALD
                } else if daemon.enabled {
                    ACCENT_CYAN
                } else if !daemon.installed || daemon.state == "failed" {
                    ACCENT_AMBER
                } else {
                    TEXT_MUTED
                };
                ui.label(
                    RichText::new(&daemon.state)
                        .small()
                        .monospace()
                        .strong()
                        .color(state_color),
                );
                ui.end_row();

                ui.label(RichText::new("Default daemon log").small().color(TEXT_MUTED));
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new(&daemon_output_path)
                            .small()
                            .monospace()
                            .color(TEXT_PRIMARY),
                    );
                    if ui.add(egui::Button::new("Copy Path").small()).clicked() {
                        copy_path = Some((
                            daemon_output_path.clone(),
                            "Copied daemon recording path to clipboard.",
                        ));
                    }
                });
                ui.end_row();
            });
    });

    if refresh_daemon {
        app.refresh_record_daemon_status();
        app.set_toast(ToastLevel::Info, "Refreshed background recorder status.");
    }
    if toggle_recording {
        app.toggle_recording();
    }
    if open_folder {
        handle_open_recordings_folder(app);
    }
    if toggle_daemon {
        app.toggle_record_daemon();
    }
    if let Some((path_str, toast_msg)) = copy_path {
        ctx.copy_text(path_str);
        app.set_toast(ToastLevel::Info, toast_msg);
    }
}
