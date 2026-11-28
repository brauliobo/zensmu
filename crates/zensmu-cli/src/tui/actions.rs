//! Safe execution of confirmed tuning, persistence, and profile-save actions on `App`.

use super::app::App;
use super::types::{tune_param_from_clamped_field, ConfirmAction, FlashKind, TuneParam};
use std::path::PathBuf;
use zensmu_core::config::profile::Profile;
use zensmu_core::error::Error;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::ops::apu::{self, ApuSetRequest};
use zensmu_core::ops::co;
use zensmu_core::ops::pbo::{self, PboSetRequest};
use zensmu_core::ops::shaper;
use zensmu_core::safety::guard::BootLoopGuard;
use zensmu_core::safety::persist;
use zensmu_core::session;
use zensmu_core::transport::SmuTransport;
use zensmu_core::types::{FreqTier, ShaperTierMargins};

impl App {
    pub(super) fn persist_live_profile_now(
        &self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) -> zensmu_core::Result<()> {
        let snap = session::capture_snapshot(transport, hw)?;
        let profile = Profile::from_snapshot(&snap);
        // Live hardware state may include BIOS Motherboard-PBO values above
        // interactive soft limits; hard hardware bounds remain strictly enforced.
        persist::persist_profile(
            &self.persist_paths,
            &profile,
            hw.codename,
            hw.topology.physical_cores,
            true,
        )
    }

    pub(super) fn execute_persist_modal_action(
        &mut self,
        action: &ConfirmAction,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) {
        match action {
            ConfirmAction::PersistLiveAndEnable => {
                let res = self.persist_live_profile_now(transport, hw).and_then(|()| {
                    persist::set_service_enabled(
                        &self.persist_paths,
                        true,
                        hw.codename,
                        hw.topology.physical_cores,
                        true,
                    )
                });
                self.refresh_persist_status(hw);
                match res {
                    Ok(_) => self.set_flash_kind(
                        format!(
                            "Persisted live state & enabled {}",
                            self.persist_paths.service_name
                        ),
                        FlashKind::Success,
                    ),
                    Err(err) => {
                        self.set_flash_kind(format!("Persist failed: {err}"), FlashKind::Error);
                    }
                }
            }
            ConfirmAction::ToggleBootService { enable } => {
                let enable = *enable;
                let needs_save = enable
                    && (!self.persist_paths.active_profile.exists()
                        || self.persist_status.active_profile_valid == Some(false));
                let res = (if needs_save {
                    self.persist_live_profile_now(transport, hw)
                } else {
                    Ok(())
                })
                .and_then(|()| {
                    persist::set_service_enabled(
                        &self.persist_paths,
                        enable,
                        hw.codename,
                        hw.topology.physical_cores,
                        true,
                    )
                });
                self.refresh_persist_status(hw);
                match res {
                    Ok(_) => self.set_flash_kind(
                        if enable {
                            format!("Enabled {}", self.persist_paths.service_name)
                        } else {
                            format!("Disabled {}", self.persist_paths.service_name)
                        },
                        FlashKind::Success,
                    ),
                    Err(err) => {
                        self.set_flash_kind(
                            format!("Service toggle failed: {err}"),
                            FlashKind::Error,
                        );
                    }
                }
            }
            ConfirmAction::ClearBootGuard => {
                let res = BootLoopGuard::new(self.persist_paths.guard_path.clone()).clear();
                self.refresh_persist_status(hw);
                match res {
                    Ok(()) => self.set_flash_kind(
                        "Cleared boot-loop crash guard sentinel",
                        FlashKind::Success,
                    ),
                    Err(err) => {
                        self.set_flash_kind(format!("Clear guard failed: {err}"), FlashKind::Error);
                    }
                }
            }
            ConfirmAction::ApplyStaged { .. }
            | ConfirmAction::Rollback(_)
            | ConfirmAction::ResetBootBaseline(_)
            | ConfirmAction::ResetCoZero => {}
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn apply_staged_tuning(
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
        co_all: Option<i32>,
        co_cores: &[(u32, i32)],
        co_igpu: Option<i32>,
        shaper_tiers: &[(FreqTier, ShaperTierMargins)],
        pbo: Option<&PboSetRequest>,
        apu: Option<&ApuSetRequest>,
        allow_extreme: bool,
    ) -> zensmu_core::Result<usize> {
        let mut applied = 0usize;
        if let Some(m) = co_all {
            applied += co::set_co_all(transport, hw, m, allow_extreme, false)?.len();
        }
        if !co_cores.is_empty() {
            applied += co::set_co_cores(transport, hw, co_cores, allow_extreme, false)?.len();
        }
        if let Some(m) = co_igpu {
            co::set_igpu_co(transport, hw, m, allow_extreme, false)?;
            applied += 1;
        }
        for &(tier, margins) in shaper_tiers {
            shaper::set_curve_shaper_tier(transport, hw, tier, margins, allow_extreme, false)?;
            applied += 1;
        }
        if let Some(req) = pbo {
            applied += pbo::set_pbo(transport, hw, req, allow_extreme, false)?.len();
        }
        if let Some(req) = apu {
            applied += apu::set_apu(transport, hw, req, allow_extreme, false)?.len();
        }
        Ok(applied)
    }

    pub(super) fn apply_reset_co_zero(
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) -> zensmu_core::Result<usize> {
        let mut applied = 0usize;
        if hw.capabilities.set_co_all_cores.is_some() {
            applied += co::set_co_all(transport, hw, 0, false, false)?.len();
        } else if hw.capabilities.set_co_per_core.is_some() {
            let pairs: Vec<(u32, i32)> = hw.topology.cores.iter().map(|c| (c.index, 0)).collect();
            applied += co::set_co_cores(transport, hw, &pairs, false, false)?.len();
        }
        if hw.capabilities.set_igpu_co.is_some() {
            match co::set_igpu_co(transport, hw, 0, false, false) {
                Ok(_) => applied += 1,
                Err(Error::SmuRejected { .. }) => {
                    // iGPU may be disabled in BIOS; match CLI `co reset` tolerance
                }
                Err(err) => return Err(err),
            }
        }
        Ok(applied)
    }

    pub(super) fn execute_confirmed_modal(
        &mut self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) {
        let Some(modal) = self.confirm_modal.take() else {
            return;
        };

        if !self.can_write() {
            let msg = if modal.action.is_persist_action() {
                "Refused: root required to persist boot config & control service (relaunch with sudo zensmu top)"
            } else {
                "Refused: root required to write SMU (relaunch with sudo zensmu top)"
            };
            self.set_flash_kind(msg, FlashKind::Error);
            return;
        }

        if modal.action.is_persist_action() {
            self.execute_persist_modal_action(&modal.action, transport, hw);
            return;
        }

        // Capture pre-change snapshot and persist boot-baseline + last-state best-effort
        if let Ok(pre) = session::capture_snapshot(transport, hw) {
            let _ = self.snapshot_store.ensure_boot_baseline(&pre);
            let _ = self.snapshot_store.save_last_state(&pre);
        }

        let exec_res: zensmu_core::Result<usize> = match &modal.action {
            ConfirmAction::ApplyStaged {
                co_all,
                co_cores,
                co_igpu,
                shaper_tiers,
                pbo,
                apu,
            } => Self::apply_staged_tuning(
                transport,
                hw,
                *co_all,
                co_cores,
                *co_igpu,
                shaper_tiers,
                pbo.as_ref(),
                apu.as_ref(),
                modal.allow_extreme,
            ),
            ConfirmAction::Rollback(snap) | ConfirmAction::ResetBootBaseline(snap) => {
                session::apply_snapshot(transport, hw, snap, true, false).map(|ops| ops.len())
            }
            ConfirmAction::ResetCoZero => Self::apply_reset_co_zero(transport, hw),
            ConfirmAction::PersistLiveAndEnable
            | ConfirmAction::ToggleBootService { .. }
            | ConfirmAction::ClearBootGuard => Ok(0),
        };

        // Always refresh live mailbox + telemetry right after write attempts
        self.refresh_mailbox(transport, hw);
        self.poll_telemetry(transport, hw);

        match exec_res {
            Ok(n) => {
                match modal.action {
                    ConfirmAction::ResetCoZero => {
                        self.staged.retain(|p, _| {
                            !matches!(
                                p,
                                TuneParam::CoAll | TuneParam::CoCore(_) | TuneParam::CoIgpu
                            )
                        });
                    }
                    _ => {
                        self.staged.clear();
                    }
                }
                self.last_alert = None;
                self.set_flash_kind(
                    format!("Applied {n} SMU operation(s) & verified"),
                    FlashKind::Success,
                );
            }
            Err(Error::HardwareClamped { summary, clamped }) => {
                for item in &clamped {
                    if let Some(param) = tune_param_from_clamped_field(&item.field) {
                        self.board_caps.insert(param, item.actual_value);
                    }
                }
                self.refresh_all(transport, hw);
                self.staged.clear();
                let msg = format!("Board/BIOS cap hit: {summary} (check BIOS PBO Limits)");
                self.last_alert = Some((msg.clone(), FlashKind::Warn));
                self.set_flash_kind(msg, FlashKind::Warn);
            }
            Err(err) => {
                let msg = if err.suggests_report() {
                    format!("SMU apply failed: {err} · run 'zensmu report --open'")
                } else {
                    format!("SMU apply failed: {err}")
                };
                self.last_alert = Some((msg.clone(), FlashKind::Error));
                self.set_flash_kind(msg, FlashKind::Error);
            }
        }
    }

    pub(super) fn save_live_profile(&mut self, transport: &dyn SmuTransport, hw: &HardwareContext) {
        let path = if self.is_root {
            PathBuf::from("/etc/zensmu/top-saved.toml")
        } else {
            PathBuf::from("/tmp/zensmu-top-profile.toml")
        };

        match session::capture_snapshot(transport, hw)
            .and_then(|snap| Profile::from_snapshot(&snap).save(&path))
        {
            Ok(()) => {
                self.set_flash_kind(
                    format!("Saved profile: {}", path.display()),
                    FlashKind::Success,
                );
            }
            Err(err) => {
                self.set_flash_kind(format!("Save failed: {err}"), FlashKind::Error);
            }
        }
    }
}
