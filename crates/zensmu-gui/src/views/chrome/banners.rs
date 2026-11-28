//! Actionable warning, driver, persistence, and firmware clamp banners.

use crate::app::{ToastLevel, ZensmuApp};
use crate::elevate::CliAction;
use crate::views::theme::*;
use crate::views::widgets::{cta_button, split_or_wrap_row};
use eframe::egui::{self, RichText, Stroke, Ui};
use zensmu_core::driver::DriverInstallMethod;

/// Warning / actionable banners shown above the main content area.
pub fn render_banners(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    if app.is_root {
        egui::Frame::NONE
            .fill(tinted_bg(ACCENT_CRIMSON, BANNER_ALPHA_CRIMSON))
            .stroke(Stroke::new(1.0_f32, ACCENT_CRIMSON))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("WARNING: Running zensmu-gui as root (uid 0).")
                            .strong()
                            .color(ACCENT_CRIMSON),
                    );
                    ui.label(
                        RichText::new(
                            "Please launch zensmu-gui as your normal user account. Telemetry reads sysfs without root, and PolicyKit (pkexec) elevates only individual zensmu CLI commands.",
                        )
                        .color(TEXT_PRIMARY),
                    );
                });
            });
        ui.add_space(10.0);
    }

    if let Some(err) = app.hw_error.clone() {
        let lower = err.to_ascii_lowercase();
        let is_unsupported_or_mismatch = app.hw_error_suggests_report
            || lower.contains("unsupported")
            || lower.contains("mismatch");
        let banner_title = if is_unsupported_or_mismatch {
            "Unsupported CPU or ryzen_smu identity mismatch"
        } else {
            "ryzen_smu kernel module is not available"
        };
        let hw_hint = app.hw_error_hint;
        egui::Frame::NONE
            .fill(tinted_bg(ACCENT_AMBER, BANNER_ALPHA_AMBER))
            .stroke(Stroke::new(1.0_f32, ACCENT_AMBER))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new(banner_title).strong().color(ACCENT_AMBER));
                ui.label(RichText::new(&err).small().color(TEXT_PRIMARY));
                if let Some(hint) = hw_hint {
                    ui.label(RichText::new(hint).small().color(TEXT_MUTED));
                }
                if !is_unsupported_or_mismatch
                    && app.driver_status.secure_boot.blocks_unsigned_modules
                {
                    ui.add_space(4.0);
                    let sb_warning = match app.driver_status.secure_boot.hint.as_deref() {
                        Some(sb_hint) if !sb_hint.trim().is_empty() => {
                            format!("Secure Boot / MOK warning: {sb_hint}")
                        }
                        _ => "Secure Boot / kernel lockdown is active — unsigned `ryzen_smu` DKMS modules require MOK signing (`sudo mokutil --import /var/lib/dkms/mok.pub`) or disabling Secure Boot.".to_string(),
                    };
                    ui.label(
                        RichText::new(sb_warning)
                            .small()
                            .strong()
                            .color(ACCENT_AMBER),
                    );
                }
                ui.add_space(6.0);
                let driver_cmd = app.driver_status.recommended_command.clone();
                let driver_installed = app.driver_status.module_installed;
                let driver_distro = app.driver_status.distro_name.clone();
                let bundled_dkms = app.driver_status.bundled_dkms_available;
                let runner_busy = app.runner.is_busy();
                let mut requested_driver_action: Option<CliAction> = None;
                let mut copied_driver_cmd = false;
                let mut retry_probe = false;
                let mut open_report = false;

                split_or_wrap_row(
                    ui,
                    "hw_error_banner_actions",
                    |ui| {
                        if !is_unsupported_or_mismatch && driver_installed {
                            ui.label(
                                RichText::new("Module installed; load with:").color(TEXT_PRIMARY),
                            );
                            ui.code(&driver_cmd);
                            if ui
                                .add_enabled(
                                    !runner_busy,
                                    cta_button("Load ryzen_smu (pkexec)", CTA_EMERALD),
                                )
                                .on_hover_text(
                                    "Elevate via PolicyKit to run `zensmu driver load --json`",
                                )
                                .clicked()
                            {
                                requested_driver_action = Some(CliAction::DriverLoad);
                            }
                        } else if !is_unsupported_or_mismatch {
                            ui.label(
                                RichText::new(format!("Build DKMS driver ({driver_distro}):"))
                                    .color(TEXT_PRIMARY),
                            );
                            ui.code(&driver_cmd);
                            let install_hover = if bundled_dkms {
                                "Build & install ryzen_smu via DKMS from bundled sources (auto-installs kernel headers/dkms if needed)"
                            } else {
                                "Clone upstream amkillam/ryzen_smu, build & install via DKMS (auto-installs kernel headers/dkms if needed)"
                            };
                            if ui
                                .add_enabled(
                                    !runner_busy,
                                    cta_button("Build & Install ryzen_smu (DKMS)", CTA_EMERALD),
                                )
                                .on_hover_text(install_hover)
                                .clicked()
                            {
                                requested_driver_action = Some(CliAction::DriverInstall {
                                    method: DriverInstallMethod::Dkms,
                                });
                            }
                            if ui.button("Copy Command").clicked() {
                                ctx.copy_text(driver_cmd.clone());
                                copied_driver_cmd = true;
                            }
                        } else {
                            ui.label(RichText::new("Driver command:").color(TEXT_PRIMARY));
                            ui.code("sudo modprobe ryzen_smu");
                        }
                    },
                    |ui| {
                        if ui.button("Retry Hardware Probe").clicked() {
                            retry_probe = true;
                        }
                        let cta_label = if is_unsupported_or_mismatch {
                            "Report on GitHub"
                        } else {
                            "Report Unsupported CPU on GitHub"
                        };
                        if ui
                            .add(cta_button(cta_label, CTA_EMERALD))
                            .on_hover_text(
                                "Collect privacy-safe CPU/PM-table diagnostics and pre-fill a GitHub issue",
                            )
                            .clicked()
                        {
                            open_report = true;
                        }
                    },
                );

                if let Some(action) = requested_driver_action {
                    app.dispatch_cli(action, ctx);
                }
                if copied_driver_cmd {
                    app.set_toast(
                        ToastLevel::Info,
                        "Copied ryzen_smu install command to clipboard.",
                    );
                }
                if retry_probe {
                    app.probe_hardware(ctx);
                }
                if open_report {
                    app.open_report_modal_for_unsupported();
                }
            });
        ui.add_space(10.0);
    }

    if let Some(clamp_msg) = app.clamp_warning.clone() {
        egui::Frame::NONE
            .fill(tinted_bg(ACCENT_AMBER, BANNER_ALPHA_AMBER))
            .stroke(Stroke::new(1.0_f32, ACCENT_AMBER))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                split_or_wrap_row(
                    ui,
                    "clamp_warning_banner_header",
                    |ui| {
                        ui.label(
                            RichText::new(
                                "Board/BIOS firmware clamped limit — check BIOS PBO Limits",
                            )
                            .strong()
                            .color(ACCENT_AMBER),
                        );
                    },
                    |ui| {
                        if ui.button("Dismiss").clicked() {
                            app.clamp_warning = None;
                        }
                    },
                );
                ui.label(RichText::new(&clamp_msg).small().color(TEXT_PRIMARY));
            });
        ui.add_space(10.0);
    }
}
