//! Boot & resume systemd persistence configuration card.

use crate::app::{PersistConfirmModal, PersistConfirmTarget, ToastLevel, ZensmuApp};
use crate::elevate::CliAction;
use crate::views::theme::*;
use crate::views::widgets::{card_frame, cta_button, muted_pill, split_or_wrap_row, tinted_pill};
use eframe::egui::{self, RichText, Ui};
use zensmu_core::safety::persist::{
    DEFAULT_ACTIVE_PROFILE_PATH, DEFAULT_MODULES_LOAD_PATH, DEFAULT_RESUME_HOOK_PATH,
    DEFAULT_SERVICE_NAME,
};

pub(super) fn render_persistence_card(
    app: &mut ZensmuApp,
    ui: &mut Ui,
    ctx: &egui::Context,
    has_hw: bool,
    busy: bool,
) {
    let status = app.persist_status.clone();
    let service_name = if status.service_name.is_empty() {
        DEFAULT_SERVICE_NAME
    } else {
        status.service_name.as_str()
    };
    let active_path = if status.active_profile_path.is_empty() {
        DEFAULT_ACTIVE_PROFILE_PATH
    } else {
        status.active_profile_path.as_str()
    };

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "persist_card_header",
            |ui| {
                ui.label(RichText::new("Boot & Resume Persistence (systemd)").strong());
                if status.boot_guard_tripped {
                    tinted_pill(ui, "GUARD TRIPPED", ACCENT_CRIMSON, PILL_ALPHA_CRIMSON);
                } else if status.service_enabled
                    && status.active_profile_exists
                    && status.active_profile_valid != Some(false)
                {
                    tinted_pill(ui, "ACTIVE", ACCENT_EMERALD, PILL_ALPHA_EMERALD);
                } else {
                    muted_pill(ui, "INACTIVE");
                }
            },
            |ui| {
                if ui
                    .button("Refresh Status")
                    .on_hover_text("Re-check boot persistence & systemd status (unprivileged)")
                    .clicked()
                {
                    app.refresh_persist_status();
                    app.refresh_safety_files();
                    app.set_toast(ToastLevel::Info, "Refreshed boot persistence status.");
                }
            },
        );

        ui.add_space(6.0);
        egui::Grid::new("persist_status_grid")
            .num_columns(2)
            .striped(true)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                // 1. Active Profile
                ui.label(
                    RichText::new(format!("Active Profile ({active_path})"))
                        .small()
                        .color(TEXT_MUTED),
                );
                if status.active_profile_exists {
                    match status.active_profile_valid {
                        Some(true) => {
                            let label = match status.active_profile_codename {
                                Some(cn) => format!("Valid ({})", cn.display_name()),
                                None => "Valid".to_string(),
                            };
                            ui.label(RichText::new(label).small().strong().color(ACCENT_EMERALD));
                        }
                        Some(false) => {
                            let err = status
                                .active_profile_error
                                .as_deref()
                                .unwrap_or("validation error");
                            ui.label(
                                RichText::new(format!("Invalid ({err})"))
                                    .small()
                                    .strong()
                                    .color(ACCENT_CRIMSON),
                            );
                        }
                        None => {
                            ui.label(RichText::new("Present").small().color(ACCENT_CYAN));
                        }
                    }
                } else {
                    ui.label(RichText::new("Not saved").small().color(TEXT_MUTED));
                }
                ui.end_row();

                // 2. Kernel Module Autoload
                ui.label(
                    RichText::new(format!(
                        "Kernel Module Autoload ({DEFAULT_MODULES_LOAD_PATH})"
                    ))
                    .small()
                    .color(TEXT_MUTED),
                );
                if status.modules_load_configured {
                    ui.label(
                        RichText::new("Configured")
                            .small()
                            .strong()
                            .color(ACCENT_EMERALD),
                    );
                } else {
                    ui.label(RichText::new("Not configured").small().color(TEXT_MUTED));
                }
                ui.end_row();

                // 3. Boot Service
                ui.label(
                    RichText::new(format!("Boot Service ({service_name})"))
                        .small()
                        .color(TEXT_MUTED),
                );
                if status.service_enabled {
                    ui.label(RichText::new("Enabled").small().strong().color(ACCENT_EMERALD));
                } else if !status.service_installed {
                    ui.label(
                        RichText::new(format!("Not installed ({})", status.service_state))
                            .small()
                            .color(ACCENT_AMBER),
                    );
                } else {
                    ui.label(RichText::new("Disabled").small().color(TEXT_MUTED));
                }
                ui.end_row();

                // 4. Suspend/Resume Hook
                ui.label(
                    RichText::new("Suspend/Resume Hook (zensmu-resume)")
                        .small()
                        .color(TEXT_MUTED),
                );
                if status.resume_hook_installed {
                    ui.label(
                        RichText::new("Installed")
                            .small()
                            .strong()
                            .color(ACCENT_EMERALD),
                    );
                } else {
                    ui.label(
                        RichText::new(format!("Missing ({DEFAULT_RESUME_HOOK_PATH})"))
                            .small()
                            .color(ACCENT_AMBER),
                    );
                }
                ui.end_row();

                // 5. Boot-Loop Crash Guard
                ui.label(
                    RichText::new("Boot-Loop Crash Guard")
                        .small()
                        .color(TEXT_MUTED),
                );
                if status.boot_guard_tripped {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("TRIPPED — auto-apply halted")
                                .small()
                                .strong()
                                .color(ACCENT_CRIMSON),
                        );
                        let clear_label = if app.is_root {
                            "Clear Boot Guard"
                        } else {
                            "Clear Boot Guard (pkexec)"
                        };
                        if ui
                            .add_enabled(!busy, cta_button(clear_label, CTA_CRIMSON).small())
                            .clicked()
                        {
                            app.dispatch_cli(CliAction::ClearBootGuard, ctx);
                        }
                    });
                } else {
                    ui.label(RichText::new("Clean").small().strong().color(ACCENT_EMERALD));
                }
                ui.end_row();
            });

        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    has_hw && !busy,
                    cta_button("Persist Live SMU & Enable Service", CTA_EMERALD),
                )
                .on_hover_text(
                    "Snapshot live SMU settings to /etc/zensmu/active.toml, configure ryzen_smu autoload, and enable zensmu-apply.service",
                )
                .clicked()
            {
                app.persist_confirm = Some(PersistConfirmModal {
                    title: "Persist Live SMU & Enable Boot Service".into(),
                    summary: "This will save your current live Ryzen SMU tuning for automatic re-application on boot and resume.".into(),
                    changes: vec![
                        format!("Export live SMU state and write {active_path}"),
                        format!("Ensure `ryzen_smu` is configured in {DEFAULT_MODULES_LOAD_PATH}"),
                        format!("Run `systemctl enable {service_name}`"),
                    ],
                    confirm_label: "Persist & Enable".into(),
                    is_destructive: false,
                    target: PersistConfirmTarget::Cli(CliAction::PersistSave {
                        from_path: None,
                        enable: true,
                        allow_extreme: app.profile_editor.allow_extreme,
                    }),
                });
            }

            let editor_non_empty = !app.profile_editor.toml_text.trim().is_empty();
            if ui
                .add_enabled(
                    has_hw && !busy && editor_non_empty,
                    egui::Button::new("Persist Editor TOML & Enable Service"),
                )
                .on_hover_text(
                    "Validate the TOML editor below, install it to /etc/zensmu/active.toml, and enable zensmu-apply.service",
                )
                .clicked()
                && app.validate_editor_toml_for_persist()
            {
                app.persist_confirm = Some(PersistConfirmModal {
                    title: "Persist Editor TOML & Enable Boot Service".into(),
                    summary: "This will install the validated TOML from the editor below as the active boot profile.".into(),
                    changes: vec![
                        format!("Install validated editor TOML to {active_path}"),
                        format!("Ensure `ryzen_smu` is configured in {DEFAULT_MODULES_LOAD_PATH}"),
                        format!("Run `systemctl enable {service_name}`"),
                    ],
                    confirm_label: "Persist Editor & Enable".into(),
                    is_destructive: false,
                    target: PersistConfirmTarget::EditorToml { enable: true },
                });
            }

            if status.service_enabled {
                if ui
                    .add_enabled(!busy, egui::Button::new("Disable Boot Service"))
                    .on_hover_text(
                        "Disable zensmu-apply.service while keeping /etc/zensmu/active.toml on disk",
                    )
                    .clicked()
                {
                    app.persist_confirm = Some(PersistConfirmModal {
                        title: "Disable Boot Service".into(),
                        summary: "Automatic tuning on boot will be disabled. Your saved profile file will remain on disk.".into(),
                        changes: vec![
                            format!("Run `systemctl disable {service_name}`"),
                            format!("Keep {active_path} intact for future use"),
                        ],
                        confirm_label: "Disable Service".into(),
                        is_destructive: false,
                        target: PersistConfirmTarget::Cli(CliAction::PersistSetService {
                            enable: false,
                            remove_profile: false,
                            allow_extreme: false,
                        }),
                    });
                }
            } else {
                let can_enable = has_hw
                    && !busy
                    && status.active_profile_exists
                    && status.active_profile_valid != Some(false);
                let hover = if !status.active_profile_exists {
                    "Save an active profile first before enabling zensmu-apply.service"
                } else if status.active_profile_valid == Some(false) {
                    "Active profile is invalid — save a valid profile first"
                } else {
                    "Enable zensmu-apply.service using the existing /etc/zensmu/active.toml"
                };
                if ui
                    .add_enabled(can_enable, egui::Button::new("Enable Boot Service"))
                    .on_hover_text(hover)
                    .clicked()
                {
                    app.persist_confirm = Some(PersistConfirmModal {
                        title: "Enable Boot Service".into(),
                        summary: "This will enable automatic application of the existing active profile on boot and resume.".into(),
                        changes: vec![
                            format!("Validate existing {active_path}"),
                            format!("Ensure `ryzen_smu` is configured in {DEFAULT_MODULES_LOAD_PATH}"),
                            format!("Run `systemctl enable {service_name}`"),
                        ],
                        confirm_label: "Enable Service".into(),
                        is_destructive: false,
                        target: PersistConfirmTarget::Cli(CliAction::PersistSetService {
                            enable: true,
                            remove_profile: false,
                            allow_extreme: app.profile_editor.allow_extreme,
                        }),
                    });
                }
            }

            if status.active_profile_exists
                && ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(
                            RichText::new("Disable & Remove Active Profile").color(ACCENT_CRIMSON),
                        ),
                    )
                    .on_hover_text(
                        "Disable zensmu-apply.service and delete /etc/zensmu/active.toml",
                    )
                    .clicked()
            {
                app.persist_confirm = Some(PersistConfirmModal {
                    title: "Disable & Remove Active Profile".into(),
                    summary: "This will disable boot persistence and delete the active boot profile file.".into(),
                    changes: vec![
                        format!("Run `systemctl disable {service_name}`"),
                        format!("Delete {active_path}"),
                    ],
                    confirm_label: "Disable & Remove".into(),
                    is_destructive: true,
                    target: PersistConfirmTarget::Cli(CliAction::PersistSetService {
                        enable: false,
                        remove_profile: true,
                        allow_extreme: false,
                    }),
                });
            }
        });
    });
}
