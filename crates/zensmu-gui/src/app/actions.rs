//! CLI dispatch, profile validation/diffing/persistence, reports, and elevated result handling.

use super::state::{DryRunModal, StatusToast, ToastLevel};
use super::tuning::finite_positive_u32;
use super::ZensmuApp;
use crate::elevate::{CliAction, ElevatedResult, ReportPayload, StatusPayload};
use eframe::egui;
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
use zensmu_core::config::profile::Profile;
use zensmu_core::ops::co::CoSnapshot;
use zensmu_core::ops::pbo::PboStatus;
use zensmu_core::report::{collect_debug_report, ReportOptions};
use zensmu_core::safety::snapshot::SmuStateSnapshot;
use zensmu_core::transport::sysfs::SysfsTransport;

impl ZensmuApp {
    pub fn set_toast(&mut self, level: ToastLevel, message: impl Into<String>) {
        self.toast = Some(StatusToast {
            level,
            message: message.into(),
            created_at: Instant::now(),
        });
    }

    /// Dispatch a `zensmu` CLI action on the background thread.
    pub fn dispatch_cli(&mut self, action: CliAction, ctx: &egui::Context) {
        if self.runner.is_busy() {
            self.set_toast(
                ToastLevel::Warning,
                "Another SMU command is already running — please wait.",
            );
            return;
        }
        self.runner.dispatch(action, ctx.clone());
    }

    /// Validate the TOML buffer in-process against the detected hardware without root.
    pub fn validate_profile_buffer(&mut self) {
        let Some(hw) = &self.hw else {
            self.profile_editor.validation = Some(Err(
                "Cannot validate profile: no Ryzen SMU hardware detected.".into(),
            ));
            return;
        };
        match Profile::from_toml_str(&self.profile_editor.toml_text) {
            Ok(profile) => match profile.validate_for(
                hw.codename,
                hw.topology.physical_cores,
                self.profile_editor.allow_extreme,
            ) {
                Ok(()) => {
                    let msg = format!(
                        "Valid profile for {} ({} cores).",
                        hw.codename.display_name(),
                        hw.topology.physical_cores
                    );
                    self.profile_editor.validation = Some(Ok(msg.clone()));
                    self.set_toast(ToastLevel::Success, msg);
                }
                Err(e) => {
                    let msg = e.to_string();
                    self.profile_editor.validation = Some(Err(msg.clone()));
                    self.set_toast(ToastLevel::Error, msg);
                }
            },
            Err(e) => {
                let msg = format!("TOML syntax/schema error: {e}");
                self.profile_editor.validation = Some(Err(msg.clone()));
                self.set_toast(ToastLevel::Error, msg);
            }
        }
    }

    /// Compute a diff between the TOML buffer and the latest known hardware state.
    pub fn compute_profile_diff(&mut self) {
        let Some(hw) = &self.hw else {
            self.set_toast(ToastLevel::Warning, "No hardware detected to diff against.");
            return;
        };
        let profile = match Profile::from_toml_str(&self.profile_editor.toml_text) {
            Ok(p) => p,
            Err(e) => {
                self.set_toast(ToastLevel::Error, format!("Invalid TOML: {e}"));
                return;
            }
        };

        let mut snap = SmuStateSnapshot::new(hw.codename, hw.topology.physical_cores);
        if let Some(co) = &self.co_snapshot {
            snap.co_per_core = Some(co.cores.iter().map(|c| (c.core_index, c.margin)).collect());
            snap.igpu_co = co.igpu;
        }
        snap.curve_shaper = self.shaper_state;
        if let Some(pbo) = &self.pbo_status {
            snap.pbo_scalar = pbo.scalar;
            snap.fmax_mhz = pbo.fmax_mhz;
        }
        if let Some(telem) = &self.latest_telemetry {
            snap.ppt_w = finite_positive_u32(telem.ppt_limit_w);
            snap.tdc_a = finite_positive_u32(telem.tdc_limit_a);
            snap.edc_a = finite_positive_u32(telem.edc_limit_a);
            snap.tctl_c = finite_positive_u32(telem.tctl_limit_c);
            snap.stapm_w = finite_positive_u32(telem.stapm_limit_w);
            snap.slow_w = finite_positive_u32(telem.slow_limit_w);
            snap.apu_slow_w = finite_positive_u32(telem.apu_slow_limit_w);
            snap.tdc_soc_a = finite_positive_u32(telem.tdc_soc_limit_a);
            snap.edc_soc_a = finite_positive_u32(telem.edc_soc_limit_a);
            snap.apu_skin_temp_c = finite_positive_u32(telem.apu_skin_temp_limit_c);
            snap.dgpu_skin_temp_c = finite_positive_u32(telem.dgpu_skin_temp_limit_c);
        }

        let diffs = profile.diff_against(&snap);
        let count = diffs.len();
        self.profile_editor.diff_entries = diffs;
        self.set_toast(
            ToastLevel::Info,
            if count == 0 {
                "Profile matches current known SMU state (0 differences).".into()
            } else {
                format!("Computed {count} difference(s) against current state.")
            },
        );
    }

    /// Load a TOML file from `profile_editor.path` into the editor.
    pub fn load_profile_from_disk(&mut self) {
        let path = PathBuf::from(self.profile_editor.path.trim());
        match fs::read_to_string(&path) {
            Ok(content) => {
                self.profile_editor.toml_text = content;
                self.profile_editor.validation = None;
                self.profile_editor.diff_entries.clear();
                self.set_toast(
                    ToastLevel::Success,
                    format!("Loaded profile from '{}'.", path.display()),
                );
            }
            Err(e) => {
                self.set_toast(
                    ToastLevel::Error,
                    format!("Failed to read '{}': {e}", path.display()),
                );
            }
        }
    }

    /// Write the editor TOML buffer to a temporary file and run `profile apply --dry-run --json`.
    pub fn preview_apply_profile(&mut self, ctx: &egui::Context) {
        // Validate locally first so malformed TOML never spawns pkexec.
        if !self.validate_editor_toml_for_persist() {
            return;
        }

        let tmp_path =
            std::env::temp_dir().join(format!("zensmu-gui-profile-{}.toml", std::process::id()));
        if let Err(e) = fs::write(&tmp_path, &self.profile_editor.toml_text) {
            self.set_toast(
                ToastLevel::Error,
                format!(
                    "Failed to stage temporary profile '{}': {e}",
                    tmp_path.display()
                ),
            );
            return;
        }

        self.staging_profile_path = Some(tmp_path.clone());
        self.dispatch_cli(
            CliAction::ProfileApply {
                path: tmp_path,
                allow_extreme: self.profile_editor.allow_extreme,
                dry_run: true,
            },
            ctx,
        );
    }

    /// Close the dry-run confirmation modal and clean up any staged profile file.
    pub fn dismiss_dry_run_modal(&mut self) {
        self.pending_modal = None;
        self.cleanup_staging_profile();
    }

    pub(super) fn cleanup_staging_profile(&mut self) {
        if let Some(tmp) = self.staging_profile_path.take() {
            let _ = fs::remove_file(tmp);
        }
    }

    pub(super) fn cleanup_staging_persist(&mut self) {
        if let Some(tmp) = self.staging_persist_path.take() {
            let _ = fs::remove_file(tmp);
        }
    }

    /// Validate the TOML editor buffer against detected hardware; updates inline validation & toast on failure.
    pub fn validate_editor_toml_for_persist(&mut self) -> bool {
        let profile = match Profile::from_toml_str(&self.profile_editor.toml_text) {
            Ok(p) => p,
            Err(e) => {
                let msg = format!("Invalid TOML: {e}");
                self.profile_editor.validation = Some(Err(msg.clone()));
                self.set_toast(ToastLevel::Error, msg);
                return false;
            }
        };

        if let Some(hw) = &self.hw {
            if let Err(e) = profile.validate_for(
                hw.codename,
                hw.topology.physical_cores,
                self.profile_editor.allow_extreme,
            ) {
                let msg = e.to_string();
                self.profile_editor.validation = Some(Err(msg.clone()));
                self.set_toast(ToastLevel::Error, msg);
                return false;
            }
        }
        true
    }

    /// Validate the editor TOML buffer, stage it to `/tmp/zensmu-gui-persist-<pid>.toml`,
    /// and dispatch `zensmu persist save --from <tmp>` via `pkexec`.
    pub fn persist_editor_toml(&mut self, enable: bool, ctx: &egui::Context) {
        if !self.validate_editor_toml_for_persist() {
            return;
        }

        let tmp_path =
            std::env::temp_dir().join(format!("zensmu-gui-persist-{}.toml", std::process::id()));
        if let Err(e) = fs::write(&tmp_path, &self.profile_editor.toml_text) {
            self.set_toast(
                ToastLevel::Error,
                format!(
                    "Failed to stage temporary profile '{}': {e}",
                    tmp_path.display()
                ),
            );
            return;
        }

        self.staging_persist_path = Some(tmp_path.clone());
        self.dispatch_cli(
            CliAction::PersistSave {
                from_path: Some(tmp_path.display().to_string()),
                enable,
                allow_extreme: self.profile_editor.allow_extreme,
            },
            ctx,
        );
    }

    /// Preview enabled APU DPTC limit changes via `zensmu apu set --dry-run --json`.
    pub fn preview_apply_apu(&mut self, ctx: &egui::Context) {
        let Some(hw) = self.hw.as_ref() else {
            self.set_toast(ToastLevel::Warning, "No Ryzen SMU hardware detected.");
            return;
        };
        let Some(action) = self.apu_editor.build_cli_action_for(hw, true) else {
            self.set_toast(
                ToastLevel::Info,
                "Enable at least one APU DPTC parameter or power mode to preview.",
            );
            return;
        };
        self.dispatch_cli(action, ctx);
    }

    /// Open the GitHub CPU Support / Bug Report modal and immediately pre-generate
    /// an unprivileged diagnostic report so the Markdown preview is never empty.
    pub fn open_report_modal(&mut self) {
        self.report_modal.open = true;
        self.report_modal.status_note = None;
        self.generate_unprivileged_report();
    }

    /// Open the report modal pre-configured with raw PM table float dumps enabled
    /// (ideal for unsupported CPUs or unrecognized PM table versions).
    pub fn open_report_modal_for_unsupported(&mut self) {
        self.report_modal.include_raw_pm_table = true;
        self.open_report_modal();
    }

    /// Synchronously collect an unprivileged, strictly read-only diagnostic report.
    pub fn generate_unprivileged_report(&mut self) {
        let note = self.report_modal.note.trim();
        let opts = ReportOptions {
            include_raw_pm_table: self.report_modal.include_raw_pm_table,
            include_mailbox_probes: false,
            user_note: (!note.is_empty()).then(|| note.to_string()),
            no_cpuid_check: false,
        };
        let report = collect_debug_report(&SysfsTransport::default(), &opts);
        self.report_modal.payload = Some(ReportPayload::from_report(report));
    }

    /// Refresh diagnostics in the report modal — either synchronously (unprivileged)
    /// or asynchronously via `pkexec zensmu report --json` when privileged probes are enabled.
    pub fn refresh_report(&mut self, ctx: &egui::Context) {
        if self.report_modal.include_privileged_probes {
            if self.runner.is_busy() {
                self.report_modal.status_note = Some((
                    "Another SMU command is already running — please wait.".into(),
                    false,
                ));
                return;
            }
            let note = self.report_modal.note.trim();
            let action = CliAction::Report {
                raw_pm_table: self.report_modal.include_raw_pm_table,
                note: (!note.is_empty()).then(|| note.to_string()),
            };
            self.report_modal.status_note = Some((
                if self.is_root {
                    "Collecting privileged SMU diagnostics…".into()
                } else {
                    "Collecting privileged SMU diagnostics via pkexec…".into()
                },
                true,
            ));
            self.dispatch_cli(action, ctx);
        } else {
            self.generate_unprivileged_report();
            self.report_modal.status_note =
                Some(("Refreshed unprivileged hardware diagnostics.".into(), true));
        }
    }

    /// Save the currently generated Markdown report to `report_modal.save_path`.
    pub fn save_report_to_disk(&mut self) {
        let Some(markdown) = self
            .report_modal
            .payload
            .as_ref()
            .map(|p| p.markdown.clone())
        else {
            self.report_modal.status_note = Some((
                "No report generated yet — click Refresh Diagnostics.".into(),
                false,
            ));
            return;
        };
        let path = self.report_modal.save_path.trim().to_string();
        if path.is_empty() {
            self.report_modal.status_note =
                Some(("Please enter a destination file path.".into(), false));
            return;
        }
        match fs::write(&path, markdown) {
            Ok(()) => {
                self.report_modal.status_note = Some((format!("Saved to {path}"), true));
            }
            Err(e) => {
                self.report_modal.status_note =
                    Some((format!("Failed to save '{path}': {e}"), false));
            }
        }
    }

    /// Export current live hardware state via `zensmu profile export` to a temp file and display it.
    pub fn export_current_profile(&mut self, ctx: &egui::Context) {
        let tmp_path =
            std::env::temp_dir().join(format!("zensmu-gui-export-{}.toml", std::process::id()));
        self.dispatch_cli(CliAction::ProfileExport { path: tmp_path }, ctx);
    }

    pub(super) fn drain_cli_results(&mut self, ctx: &egui::Context) {
        for result in self.runner.poll_results() {
            match result {
                ElevatedResult::Status(payload) => {
                    self.apply_status_payload(*payload);
                    self.refresh_safety_files();
                    self.refresh_persist_status();
                    if self.suppress_next_status_toast {
                        self.suppress_next_status_toast = false;
                    } else {
                        self.set_toast(ToastLevel::Success, "Refreshed live SMU state.");
                    }
                }
                ElevatedResult::Co(snap) => {
                    self.sync_co_from_snapshot(snap);
                    self.set_toast(ToastLevel::Success, "Read current Curve Optimizer margins.");
                }
                ElevatedResult::Shaper(state) => {
                    self.shaper_state = Some(state);
                    self.shaper_editor.draft = state;
                    self.set_toast(ToastLevel::Success, "Read current Curve Shaper matrix.");
                }
                ElevatedResult::Pbo(status) => {
                    self.sync_pbo_from_status(status, true);
                    self.set_toast(
                        ToastLevel::Success,
                        "Read current PBO status & fused limits.",
                    );
                }
                ElevatedResult::Report(payload) => {
                    self.report_modal.payload = Some(*payload);
                    self.report_modal.status_note = Some((
                        "Refreshed diagnostics with privileged SMU probes.".into(),
                        true,
                    ));
                    self.set_toast(
                        ToastLevel::Success,
                        "Collected privileged hardware diagnostic report.",
                    );
                }
                ElevatedResult::DryRunPlan {
                    title,
                    live_action,
                    ops,
                } => {
                    self.pending_modal = Some(DryRunModal {
                        title,
                        ops,
                        live_action,
                    });
                }
                ElevatedResult::Applied { title, ops } => {
                    self.cleanup_staging_profile();
                    self.clamp_warning = None;
                    self.pbo_editor.clear_dirty();
                    self.apu_editor.clear_dirty();
                    self.refresh_safety_files();
                    self.refresh_persist_status();
                    self.set_toast(
                        ToastLevel::Success,
                        format!("{title}: applied {} verified SMU operation(s).", ops.len()),
                    );
                    // Refresh live SMU state after a successful mutation so sliders stay in sync.
                    self.dispatch_cli(CliAction::Status, ctx);
                }
                ElevatedResult::ProfileExported { path, toml } => {
                    self.profile_editor.toml_text = toml;
                    self.profile_editor.validation = None;
                    let _ = fs::remove_file(path);
                    self.set_toast(
                        ToastLevel::Success,
                        "Exported current SMU state into the TOML editor.",
                    );
                }
                ElevatedResult::PersistUpdated {
                    action,
                    status,
                    note,
                } => {
                    self.cleanup_staging_persist();
                    self.persist_status = status;
                    self.refresh_safety_files();
                    self.refresh_persist_status();
                    let msg = match note {
                        Some(n) if !n.trim().is_empty() => format!("{action}: {n}"),
                        _ => format!(
                            "{action}: boot service is now {}.",
                            if self.persist_status.service_enabled {
                                "enabled"
                            } else {
                                "disabled"
                            }
                        ),
                    };
                    self.set_toast(ToastLevel::Success, msg);
                }
                ElevatedResult::GuardCleared { cleared } => {
                    self.refresh_safety_files();
                    self.refresh_persist_status();
                    if cleared {
                        self.set_toast(
                            ToastLevel::Success,
                            "Boot-loop crash guard cleared — auto-apply is re-armed.",
                        );
                    } else {
                        self.set_toast(
                            ToastLevel::Info,
                            "Boot-loop crash guard was already clean.",
                        );
                    }
                }
                ElevatedResult::DriverLoaded(out) => {
                    let action_verb = if out.action == "install" {
                        "Installed and loaded"
                    } else {
                        "Loaded"
                    };
                    self.driver_status = out.status;
                    self.set_toast(
                        ToastLevel::Success,
                        format!(
                            "{action_verb} ryzen_smu kernel driver — probing Ryzen SMU hardware."
                        ),
                    );
                    self.probe_hardware(ctx);
                }
                ElevatedResult::HardwareClamped { message, caps } => {
                    self.cleanup_staging_persist();
                    self.cleanup_staging_profile();
                    self.pbo_editor.clear_dirty();
                    self.apu_editor.clear_dirty();
                    for (field, cap) in caps {
                        self.board_caps.insert(field, cap);
                    }
                    self.clamp_warning = Some(message.clone());
                    self.set_toast(
                        ToastLevel::Warning,
                        format!("Board/BIOS clamped limit — check BIOS PBO Limits: {message}"),
                    );
                    // Immediately resync sliders to the actual clamped hardware values.
                    self.suppress_next_status_toast = true;
                    self.dispatch_cli(CliAction::Status, ctx);
                }
                ElevatedResult::Error { title, message } => {
                    self.cleanup_staging_persist();
                    self.cleanup_staging_profile();
                    if self.report_modal.open && title == "Collect Hardware Report" {
                        self.report_modal.status_note =
                            Some((format!("Privileged report failed: {message}"), false));
                    }
                    self.set_toast(ToastLevel::Error, format!("{title}: {message}"));
                }
            }
        }
    }

    fn apply_status_payload(&mut self, payload: StatusPayload) {
        if let Some(co) = payload.co {
            self.sync_co_from_snapshot(co);
        }
        if let Some(cs) = payload.shaper {
            self.shaper_state = Some(cs);
            self.shaper_editor.draft = cs;
        }
        if let Some(pbo) = payload.pbo {
            self.sync_pbo_from_status(pbo, true);
        }
        if let Some(telem) = payload.telemetry {
            self.pbo_editor.sync_from_telemetry(&telem);
            self.apu_editor.sync_from_telemetry(&telem);
            self.latest_telemetry = Some(telem);
        }
    }

    fn sync_co_from_snapshot(&mut self, snap: CoSnapshot) {
        let count = self
            .hw
            .as_ref()
            .map(|h| h.topology.physical_cores)
            .unwrap_or(snap.cores.len())
            .max(snap.cores.len());
        self.co_editor.per_core.resize(count, 0);
        for c in &snap.cores {
            if let Some(slot) = self.co_editor.per_core.get_mut(c.core_index as usize) {
                *slot = i32::from(c.margin);
            }
        }
        if let Some(first) = snap.cores.first() {
            if snap.cores.iter().all(|c| c.margin == first.margin) {
                self.co_editor.all_core = i32::from(first.margin);
            }
        }
        if let Some(igpu) = snap.igpu {
            self.co_editor.igpu = i32::from(igpu);
        }
        self.co_snapshot = Some(snap);
    }

    fn sync_pbo_from_status(&mut self, status: PboStatus, force: bool) {
        self.pbo_editor.sync_from_status(&status, force);
        self.pbo_status = Some(status);
    }
}
