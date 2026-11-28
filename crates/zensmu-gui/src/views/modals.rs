//! Confirmation and diagnostic modals (persistence, dry-run preview, GitHub report).

use super::theme::*;
use super::widgets::{cta_button, split_or_wrap_row};
use crate::app::{open_url_in_browser, PersistConfirmTarget, ZensmuApp};
use crate::elevate::CliAction;
use eframe::egui::{self, RichText, Stroke};
use zensmu_core::report::GITHUB_REPO;

/// Modal confirming boot persistence & systemd changes before invoking `pkexec`.
pub fn render_persist_confirm_modal(app: &mut ZensmuApp, ctx: &egui::Context) {
    let Some(modal) = app.persist_confirm.clone() else {
        return;
    };

    let mut close_modal = false;
    let mut confirmed_target: Option<PersistConfirmTarget> = None;

    egui::Window::new(format!("Confirm System Change — {}", modal.title))
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(&modal.summary);
            ui.add_space(8.0);

            egui::Frame::NONE
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("EXACT SYSTEM CHANGES")
                            .small()
                            .strong()
                            .color(TEXT_MUTED),
                    );
                    ui.add_space(4.0);
                    for change in &modal.changes {
                        ui.label(
                            RichText::new(format!("• {change}"))
                                .small()
                                .monospace()
                                .color(TEXT_PRIMARY),
                        );
                    }
                });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);

            let mut cancel_clicked = false;
            let mut confirm_clicked = false;
            split_or_wrap_row(
                ui,
                "persist_modal_actions",
                |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel_clicked = true;
                    }
                },
                |ui| {
                    let btn_label = if app.is_root {
                        modal.confirm_label.clone()
                    } else {
                        format!("{} (pkexec)", modal.confirm_label)
                    };
                    let fill = if modal.is_destructive {
                        CTA_CRIMSON
                    } else {
                        CTA_EMERALD
                    };
                    if ui
                        .add_enabled(!app.runner.is_busy(), cta_button(btn_label, fill))
                        .clicked()
                    {
                        confirmed_target = Some(modal.target.clone());
                        confirm_clicked = true;
                    }
                },
            );
            close_modal = cancel_clicked || confirm_clicked;
        });

    if close_modal {
        app.persist_confirm = None;
    }
    if let Some(target) = confirmed_target {
        match target {
            PersistConfirmTarget::Cli(action) => app.dispatch_cli(action, ctx),
            PersistConfirmTarget::EditorToml { enable } => app.persist_editor_toml(enable, ctx),
        }
    }
}

/// Modal displaying every planned SMU command from `--dry-run --json` before applying.
pub fn render_dry_run_modal(app: &mut ZensmuApp, ctx: &egui::Context) {
    let Some(modal) = app.pending_modal.clone() else {
        return;
    };

    let mut close_modal = false;
    let mut confirm_action: Option<CliAction> = None;

    egui::Window::new(format!("Dry-Run Confirmation — {}", modal.title))
        .collapsible(false)
        .resizable(true)
        .default_width(860.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                RichText::new(
                    "Review every planned SMU mailbox command below before elevating privileges to write to hardware.",
                )
                .color(TEXT_MUTED),
            );
            ui.add_space(8.0);

            if modal.ops.is_empty() {
                ui.label(
                    RichText::new("No SMU write operations are required for this request.")
                        .color(ACCENT_EMERALD),
                );
            } else {
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        egui::Grid::new("dry_run_ops_table")
                            .num_columns(6)
                            .striped(true)
                            .spacing([10.0, 6.0])
                            .show(ui, |ui| {
                                ui.label(RichText::new("#").small().strong().color(TEXT_MUTED));
                                ui.label(
                                    RichText::new("Operation").small().strong().color(TEXT_MUTED),
                                );
                                ui.label(
                                    RichText::new("Mailbox").small().strong().color(TEXT_MUTED),
                                );
                                ui.label(
                                    RichText::new("Opcode").small().strong().color(TEXT_MUTED),
                                );
                                ui.label(
                                    RichText::new("Encoded Args").small().strong().color(TEXT_MUTED),
                                );
                                ui.label(
                                    RichText::new("Current → Target")
                                        .small()
                                        .strong()
                                        .color(TEXT_MUTED),
                                );
                                ui.end_row();

                                for (i, op) in modal.ops.iter().enumerate() {
                                    ui.label(RichText::new(format!("{}", i + 1)).monospace());
                                    ui.label(&op.description);
                                    ui.label(
                                        RichText::new(op.mailbox.as_str())
                                            .monospace()
                                            .color(ACCENT_CYAN),
                                    );
                                    ui.label(
                                        RichText::new(format!("0x{:02X}", op.opcode))
                                            .monospace()
                                            .color(ACCENT_AMBER),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "[0x{:08X}, 0x{:X}, 0x{:X}, 0x{:X}, 0x{:X}, 0x{:X}]",
                                            op.args[0],
                                            op.args[1],
                                            op.args[2],
                                            op.args[3],
                                            op.args[4],
                                            op.args[5],
                                        ))
                                        .small()
                                        .monospace(),
                                    );
                                    let transition = format!(
                                        "{} → {}",
                                        op.current.as_deref().unwrap_or("—"),
                                        op.target
                                    );
                                    ui.label(
                                        RichText::new(transition)
                                            .monospace()
                                            .color(ACCENT_EMERALD),
                                    );
                                    ui.end_row();
                                }
                            });
                    });
            }

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);

            let mut cancel_clicked = false;
            let mut confirm_clicked = false;
            split_or_wrap_row(
                ui,
                "dry_run_modal_actions",
                |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel_clicked = true;
                    }
                },
                |ui| {
                    let confirm_label = if app.is_root {
                        "Confirm & Apply"
                    } else {
                        "Confirm & Apply (pkexec)"
                    };
                    if ui
                        .add_enabled(
                            !modal.ops.is_empty() && !app.runner.is_busy(),
                            cta_button(confirm_label, CTA_CRIMSON),
                        )
                        .clicked()
                    {
                        confirm_action = Some(modal.live_action.clone());
                        confirm_clicked = true;
                    }
                },
            );
            close_modal = cancel_clicked || confirm_clicked;
        });

    if let Some(action) = confirm_action {
        app.pending_modal = None;
        app.dispatch_cli(action, ctx);
    } else if close_modal {
        app.dismiss_dry_run_modal();
    }
}

/// Modal for collecting privacy-safe hardware diagnostics and opening a pre-filled GitHub issue.
pub fn render_report_modal(app: &mut ZensmuApp, ctx: &egui::Context) {
    if !app.report_modal.open {
        return;
    }

    let mut close_clicked = false;
    let mut refresh_clicked = false;
    let mut copy_clicked = false;
    let mut open_github_clicked = false;
    let mut save_clicked = false;
    let busy = app.runner.is_busy();

    egui::Window::new("CPU Support / GitHub Issue Report")
        .collapsible(false)
        .resizable(true)
        .default_width(760.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .frame(
            egui::Frame::window(&ctx.style())
                .fill(BG_CARD)
                .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE)),
        )
        .show(ctx, |ui| {
            // Privacy notice
            egui::Frame::NONE
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new(
                            "No hostnames, usernames, IPs, or serial numbers are collected.",
                        )
                        .strong()
                        .color(ACCENT_EMERALD),
                    );
                    ui.label(
                        RichText::new(format!(
                            "All diagnostic probes are strictly read-only and formatted for {GITHUB_REPO}."
                        ))
                        .small()
                        .color(TEXT_MUTED),
                    );
                });

            ui.add_space(8.0);

            // Optional user note
            ui.label(
                RichText::new("Optional note (included at top of Markdown report):")
                    .small()
                    .color(TEXT_MUTED),
            );
            ui.add(
                egui::TextEdit::singleline(&mut app.report_modal.note)
                    .desired_width(f32::INFINITY)
                    .hint_text("Describe your CPU, motherboard, or issue (optional)…"),
            );

            ui.add_space(6.0);

            // Options + Refresh
            let can_refresh = !app.report_modal.include_privileged_probes || !busy;
            split_or_wrap_row(
                ui,
                "report_modal_options",
                |ui| {
                    ui.checkbox(
                        &mut app.report_modal.include_raw_pm_table,
                        "Include non-zero PM table floats",
                    );
                    ui.checkbox(
                        &mut app.report_modal.include_privileged_probes,
                        "Include privileged SMU probes via pkexec",
                    );
                },
                |ui| {
                    if ui
                        .add_enabled(can_refresh, egui::Button::new("Refresh Diagnostics"))
                        .on_hover_text(
                            "Re-collect diagnostics with the selected options and note",
                        )
                        .clicked()
                    {
                        refresh_clicked = true;
                    }
                },
            );

            ui.add_space(8.0);

            // Suggested issue title & scrollable Markdown preview
            if let Some(payload) = &app.report_modal.payload {
                egui::Frame::NONE
                    .fill(BG_PANEL)
                    .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(10, 6))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.horizontal_wrapped(|ui| {
                            ui.label(
                                RichText::new("Suggested Title:")
                                    .small()
                                    .strong()
                                    .color(TEXT_MUTED),
                            );
                            ui.label(
                                RichText::new(&payload.issue_title)
                                    .small()
                                    .monospace()
                                    .color(TEXT_PRIMARY),
                            );
                        });
                    });

                ui.add_space(6.0);

                egui::Frame::NONE
                    .fill(BG_EXTREME)
                    .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        egui::ScrollArea::vertical()
                            .id_salt("report_modal_markdown_scroll")
                            .max_height(260.0)
                            .show(ui, |ui| {
                                let mut preview = payload.markdown.as_str();
                                ui.add(
                                    egui::TextEdit::multiline(&mut preview)
                                        .code_editor()
                                        .desired_width(f32::INFINITY)
                                        .desired_rows(14),
                                );
                            });
                    });
            } else {
                ui.label(
                    RichText::new("Click 'Refresh Diagnostics' to generate a hardware report.")
                        .color(TEXT_MUTED),
                );
            }

            ui.add_space(8.0);

            // Save path row
            ui.horizontal(|ui| {
                ui.label(RichText::new("Save path:").small().color(TEXT_MUTED));
                ui.add(
                    egui::TextEdit::singleline(&mut app.report_modal.save_path)
                        .desired_width(ui.available_width().max(200.0))
                        .hint_text("/tmp/zensmu-report.md"),
                );
            });

            // Status note
            if let Some((msg, is_ok)) = &app.report_modal.status_note {
                ui.add_space(4.0);
                let color = if *is_ok { ACCENT_EMERALD } else { ACCENT_AMBER };
                ui.label(RichText::new(msg).small().color(color));
            }

            ui.add_space(10.0);
            ui.separator();
            ui.add_space(6.0);

            let has_payload = app.report_modal.payload.is_some();
            split_or_wrap_row(
                ui,
                "report_modal_footer",
                |ui| {
                    if ui.button("Close").clicked() {
                        close_clicked = true;
                    }
                },
                |ui| {
                    if ui
                        .add_enabled(has_payload, egui::Button::new("Copy Markdown"))
                        .clicked()
                    {
                        copy_clicked = true;
                    }
                    if ui
                        .add_enabled(has_payload, egui::Button::new("Save .md"))
                        .clicked()
                    {
                        save_clicked = true;
                    }
                    if ui
                        .add_enabled(
                            has_payload,
                            cta_button("Open GitHub Issue", CTA_EMERALD),
                        )
                        .on_hover_text(
                            "Open a pre-filled GitHub issue in your browser via xdg-open",
                        )
                        .clicked()
                    {
                        open_github_clicked = true;
                    }
                },
            );
        });

    if refresh_clicked {
        app.refresh_report(ctx);
    }
    if copy_clicked {
        if let Some(payload) = &app.report_modal.payload {
            ctx.copy_text(payload.markdown.clone());
            app.report_modal.status_note = Some(("Copied Markdown to clipboard.".into(), true));
        }
    }
    if save_clicked {
        app.save_report_to_disk();
    }
    if open_github_clicked {
        if let Some(url) = app
            .report_modal
            .payload
            .as_ref()
            .map(|p| p.issue_url.clone())
        {
            match open_url_in_browser(&url) {
                Ok(()) => {
                    app.report_modal.status_note = Some(("Opened GitHub in browser.".into(), true));
                }
                Err(err) => {
                    app.report_modal.status_note = Some((err, false));
                }
            }
        }
    }
    if close_clicked {
        app.report_modal.open = false;
    }
}
