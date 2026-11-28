//! Dry-run planning and confirmation modal builders on `App`.

use super::app::App;
use super::format::format_signed_margin;
use super::types::{ConfirmAction, ConfirmModal, FlashKind, MailboxState, TuneParam, TuneValue};
use zensmu_core::error::Error;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::ops::apu::{self, ApuSetRequest};
use zensmu_core::ops::co;
use zensmu_core::ops::pbo::{self, PboSetRequest};
use zensmu_core::ops::shaper;
use zensmu_core::session;
use zensmu_core::transport::SmuTransport;
use zensmu_core::types::{FreqTier, PlannedOp, ShaperTierMargins, TempBand};

impl App {
    // -----------------------------------------------------------------------
    // Dry-run preview & safe execution (flocked transport + snapshot + verify)
    // -----------------------------------------------------------------------

    pub(super) fn current_co_all_summary(&self) -> Option<String> {
        let snap = self.co_state.as_loaded()?;
        let first = snap.cores.first()?;
        if snap.cores.iter().all(|c| c.margin == first.margin) {
            Some(format_signed_margin(first.margin))
        } else {
            Some("mixed".to_string())
        }
    }

    pub(super) fn staged_int(&self, param: TuneParam) -> Option<i32> {
        self.staged.get(&param).and_then(|v| v.as_int())
    }

    pub(super) fn staged_u32(&self, param: TuneParam) -> Option<u32> {
        self.staged_int(param).and_then(|v| u32::try_from(v).ok())
    }

    pub(super) fn enrich_op_current(&self, op: &mut PlannedOp, rules: &[(&str, TuneParam, &str)]) {
        if op.current.is_some() {
            return;
        }
        for &(prefix, param, suffix) in rules {
            if op.description.starts_with(prefix) {
                op.current = self
                    .live_param_value(param)
                    .and_then(TuneValue::as_int)
                    .map(|v| format!("{v}{suffix}"));
                return;
            }
        }
    }

    pub(super) fn collect_staged_shaper_tiers(
        &self,
        hw: &HardwareContext,
    ) -> Vec<(FreqTier, ShaperTierMargins)> {
        if hw.capabilities.set_curve_shaper.is_none()
            || matches!(self.shaper_state, MailboxState::FirmwareUnsupported(_))
        {
            return Vec::new();
        }
        let live_cs = self.shaper_state.as_loaded().copied().unwrap_or_default();
        let mut tiers = Vec::new();
        for tier in FreqTier::ALL {
            let s_low = self.staged_int(TuneParam::Shaper {
                tier,
                band: TempBand::Low,
            });
            let s_med = self.staged_int(TuneParam::Shaper {
                tier,
                band: TempBand::Med,
            });
            let s_high = self.staged_int(TuneParam::Shaper {
                tier,
                band: TempBand::High,
            });
            if s_low.is_some() || s_med.is_some() || s_high.is_some() {
                let base = live_cs.tier(tier);
                tiers.push((
                    tier,
                    ShaperTierMargins {
                        low: s_low.map(|v| v as i8).unwrap_or(base.low),
                        med: s_med.map(|v| v as i8).unwrap_or(base.med),
                        high: s_high.map(|v| v as i8).unwrap_or(base.high),
                    },
                ));
            }
        }
        tiers
    }

    pub(super) fn collect_staged_pbo_request(&self) -> Option<PboSetRequest> {
        let req = PboSetRequest {
            ppt_w: self.staged_u32(TuneParam::PboPpt),
            tdc_a: self.staged_u32(TuneParam::PboTdc),
            edc_a: self.staged_u32(TuneParam::PboEdc),
            tctl_c: self.staged_u32(TuneParam::PboTctl),
            scalar: self.staged_u32(TuneParam::PboScalar),
            fmax_mhz: self.staged_u32(TuneParam::PboFmax),
        };
        (req != PboSetRequest::default()).then_some(req)
    }

    pub(super) fn collect_staged_apu_request(&self) -> Option<ApuSetRequest> {
        let req = ApuSetRequest {
            stapm_w: self.staged_u32(TuneParam::ApuStapm),
            stapm_time_s: self.staged_u32(TuneParam::ApuStapmTime),
            fast_w: self.staged_u32(TuneParam::ApuFast),
            slow_w: self.staged_u32(TuneParam::ApuSlow),
            slow_time_s: self.staged_u32(TuneParam::ApuSlowTime),
            apu_slow_w: self.staged_u32(TuneParam::ApuApuSlow),
            tdc_soc_a: self.staged_u32(TuneParam::ApuTdcSoc),
            edc_soc_a: self.staged_u32(TuneParam::ApuEdcSoc),
            apu_skin_temp_c: self.staged_u32(TuneParam::ApuSkinTemp),
            dgpu_skin_temp_c: self.staged_u32(TuneParam::ApuDgpuSkinTemp),
            power_mode: self
                .staged
                .get(&TuneParam::ApuPowerMode)
                .and_then(|v| v.as_power_mode()),
        };
        (!req.is_empty()).then_some(req)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn plan_staged_ops(
        &self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
        co_all: Option<i32>,
        co_cores: &[(u32, i32)],
        co_igpu: Option<i32>,
        shaper_tiers: &[(FreqTier, ShaperTierMargins)],
        pbo_req: Option<&PboSetRequest>,
        apu_req: Option<&ApuSetRequest>,
        allow_extreme: bool,
    ) -> zensmu_core::Result<Vec<PlannedOp>> {
        const PBO_ENRICH_RULES: &[(&str, TuneParam, &str)] = &[
            ("Set PPT", TuneParam::PboPpt, " W"),
            ("Set TDC", TuneParam::PboTdc, " A"),
            ("Set EDC", TuneParam::PboEdc, " A"),
            ("Set Tctl", TuneParam::PboTctl, " C"),
            ("Set PBO scalar", TuneParam::PboScalar, "x"),
            ("Set Fmax", TuneParam::PboFmax, " MHz"),
        ];
        const APU_ENRICH_RULES: &[(&str, TuneParam, &str)] = &[
            ("Set APU STAPM limit", TuneParam::ApuStapm, " W"),
            ("Set APU Fast PPT", TuneParam::ApuFast, " W"),
            ("Set APU Slow PPT limit", TuneParam::ApuSlow, " W"),
            ("Set APU-only Slow PPT", TuneParam::ApuApuSlow, " W"),
            ("Set APU SoC TDC", TuneParam::ApuTdcSoc, " A"),
            ("Set APU SoC EDC", TuneParam::ApuEdcSoc, " A"),
            ("Set APU skin", TuneParam::ApuSkinTemp, " C"),
            ("Set dGPU skin", TuneParam::ApuDgpuSkinTemp, " C"),
        ];

        let mut ops = Vec::new();
        if let Some(m) = co_all {
            let mut v = co::set_co_all(transport, hw, m, allow_extreme, true)?;
            for op in &mut v {
                if op.current.is_none() {
                    op.current = self.current_co_all_summary();
                }
            }
            ops.extend(v);
        }
        if !co_cores.is_empty() {
            let mut v = co::set_co_cores(transport, hw, co_cores, allow_extreme, true)?;
            for (op, &(idx, _)) in v.iter_mut().zip(co_cores.iter()) {
                if op.current.is_none() {
                    op.current = self
                        .live_param_value(TuneParam::CoCore(idx))
                        .and_then(TuneValue::as_int)
                        .map(|m| format_signed_margin(m as i16));
                }
            }
            ops.extend(v);
        }
        if let Some(m) = co_igpu {
            let mut op = co::set_igpu_co(transport, hw, m, allow_extreme, true)?;
            if op.current.is_none() {
                op.current = self
                    .live_param_value(TuneParam::CoIgpu)
                    .and_then(TuneValue::as_int)
                    .map(|v| format_signed_margin(v as i16));
            }
            ops.push(op);
        }
        for &(tier, margins) in shaper_tiers {
            let mut op =
                shaper::set_curve_shaper_tier(transport, hw, tier, margins, allow_extreme, true)?;
            if op.current.is_none() {
                op.current = self.shaper_state.as_loaded().map(|cs| {
                    let cur = cs.tier(tier);
                    format!("low={},med={},high={}", cur.low, cur.med, cur.high)
                });
            }
            ops.push(op);
        }
        if let Some(req) = pbo_req {
            let mut v = pbo::set_pbo(transport, hw, req, allow_extreme, true)?;
            for op in &mut v {
                self.enrich_op_current(op, PBO_ENRICH_RULES);
            }
            ops.extend(v);
        }
        if let Some(req) = apu_req {
            let mut v = apu::set_apu(transport, hw, req, allow_extreme, true)?;
            for op in &mut v {
                self.enrich_op_current(op, APU_ENRICH_RULES);
            }
            ops.extend(v);
        }
        Ok(ops)
    }

    pub(super) fn open_staged_dry_run_modal(
        &mut self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) {
        if self.staged.is_empty() {
            self.set_flash_kind("No staged changes to preview", FlashKind::Warn);
            return;
        }

        let co_all = self.staged_int(TuneParam::CoAll);
        let co_cores: Vec<(u32, i32)> = hw
            .topology
            .cores
            .iter()
            .filter_map(|core| {
                self.staged_int(TuneParam::CoCore(core.index))
                    .map(|val| (core.index, val))
            })
            .collect();
        let co_igpu = self.staged_int(TuneParam::CoIgpu);
        let shaper_tiers = self.collect_staged_shaper_tiers(hw);
        let pbo_req = self.collect_staged_pbo_request();
        let apu_req = self.collect_staged_apu_request();

        // Check Fmax soft delta in dry-run (since pbo::set_pbo skips live get_fmax in dry_run)
        if let Some(fmax) = pbo_req.as_ref().and_then(|r| r.fmax_mhz) {
            if !self.allow_extreme {
                if let Some(cur) = self.pbo_state.as_loaded().and_then(|p| p.fmax_mhz) {
                    let ceiling = cur.saturating_add(pbo::FMAX_SOFT_DELTA_MHZ);
                    if fmax > ceiling {
                        self.set_flash_kind(
                            format!(
                                "Extreme blocked: fmax {fmax} > {ceiling} MHz (press 'x' for Allow Extreme)"
                            ),
                            FlashKind::Error,
                        );
                        return;
                    }
                }
            }
        }

        let allow_extreme = self.allow_extreme;
        match self.plan_staged_ops(
            transport,
            hw,
            co_all,
            &co_cores,
            co_igpu,
            &shaper_tiers,
            pbo_req.as_ref(),
            apu_req.as_ref(),
            allow_extreme,
        ) {
            Ok(ops) if ops.is_empty() => {
                self.set_flash_kind("No SMU operations to apply", FlashKind::Warn);
            }
            Ok(ops) => {
                let subtitle = format!(
                    "{} SMU operation(s) validated · dry-run passed · zero writes sent yet",
                    ops.len()
                );
                self.confirm_modal = Some(ConfirmModal::ops_modal(
                    " Dry-Run Preview — Apply Staged Tuning ",
                    subtitle,
                    ConfirmAction::ApplyStaged {
                        co_all,
                        co_cores,
                        co_igpu,
                        shaper_tiers,
                        pbo: pbo_req,
                        apu: apu_req,
                    },
                    ops,
                    allow_extreme,
                ));
            }
            Err(Error::ExtremeValueBlocked {
                field,
                value,
                threshold,
            }) => {
                self.set_flash_kind(
                    format!(
                        "Blocked: {field}={value} violates soft limit {threshold} (press 'x' for Extreme)"
                    ),
                    FlashKind::Error,
                );
            }
            Err(err) => {
                self.set_flash_kind(format!("Dry-run failed: {err}"), FlashKind::Error);
            }
        }
    }

    pub(super) fn open_snapshot_restore_modal(
        &mut self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
        boot_baseline: bool,
    ) {
        let (label, loaded) = if boot_baseline {
            (
                "boot-baseline.json",
                self.snapshot_store.load_boot_baseline(),
            )
        } else {
            ("last-state.json", self.snapshot_store.load_last_state())
        };

        let snap = match loaded {
            Ok(Some(s)) => s,
            Ok(None) => {
                self.set_flash_kind(format!("No {label} snapshot found"), FlashKind::Warn);
                return;
            }
            Err(err) => {
                self.set_flash_kind(format!("Failed to load {label}: {err}"), FlashKind::Error);
                return;
            }
        };

        match session::apply_snapshot(transport, hw, &snap, true, true) {
            Ok(ops) if ops.is_empty() => {
                self.set_flash_kind(format!("Hardware already matches {label}"), FlashKind::Info);
            }
            Ok(ops) => {
                let (title, action) = if boot_baseline {
                    (
                        " Dry-Run Preview — Reset to Boot Baseline ",
                        ConfirmAction::ResetBootBaseline(snap),
                    )
                } else {
                    (
                        " Dry-Run Preview — Rollback to Last State ",
                        ConfirmAction::Rollback(snap),
                    )
                };
                let subtitle =
                    format!("Restoring {label} · {} SMU operation(s) planned", ops.len());
                self.confirm_modal =
                    Some(ConfirmModal::ops_modal(title, subtitle, action, ops, true));
            }
            Err(err) => {
                self.set_flash_kind(format!("Snapshot dry-run failed: {err}"), FlashKind::Error);
            }
        }
    }

    pub(super) fn plan_reset_co_zero_ops(
        &self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) -> zensmu_core::Result<Vec<PlannedOp>> {
        let mut ops = Vec::new();
        if hw.capabilities.set_co_all_cores.is_some() {
            let mut v = co::set_co_all(transport, hw, 0, false, true)?;
            for op in &mut v {
                if op.current.is_none() {
                    op.current = self.current_co_all_summary();
                }
            }
            ops.extend(v);
        } else if hw.capabilities.set_co_per_core.is_some() {
            let pairs: Vec<(u32, i32)> = hw.topology.cores.iter().map(|c| (c.index, 0)).collect();
            let mut v = co::set_co_cores(transport, hw, &pairs, false, true)?;
            for (op, &(idx, _)) in v.iter_mut().zip(pairs.iter()) {
                if op.current.is_none() {
                    op.current = self
                        .live_param_value(TuneParam::CoCore(idx))
                        .and_then(TuneValue::as_int)
                        .map(|m| format_signed_margin(m as i16));
                }
            }
            ops.extend(v);
        } else {
            return Err(Error::UnsupportedCapability {
                capability: "curve_optimizer",
                codename: hw.codename.display_name().to_string(),
            });
        }

        if hw.capabilities.set_igpu_co.is_some() {
            let mut op = co::set_igpu_co(transport, hw, 0, false, true)?;
            if op.current.is_none() {
                op.current = self
                    .live_param_value(TuneParam::CoIgpu)
                    .and_then(TuneValue::as_int)
                    .map(|v| format_signed_margin(v as i16));
            }
            ops.push(op);
        }
        Ok(ops)
    }

    pub(super) fn open_reset_co_zero_modal(
        &mut self,
        transport: &dyn SmuTransport,
        hw: &HardwareContext,
    ) {
        match self.plan_reset_co_zero_ops(transport, hw) {
            Ok(ops) => {
                let subtitle = format!("{} SMU operation(s) to reset CO margins to 0", ops.len());
                self.confirm_modal = Some(ConfirmModal::ops_modal(
                    " Dry-Run Preview — Reset Curve Optimizer to 0 ",
                    subtitle,
                    ConfirmAction::ResetCoZero,
                    ops,
                    false,
                ));
            }
            Err(err) => {
                self.set_flash_kind(format!("CO reset unavailable: {err}"), FlashKind::Error);
            }
        }
    }

    pub(super) fn persist_enable_steps(&self, profile_step: String) -> Vec<(String, String)> {
        vec![
            ("1. Profile".to_string(), profile_step),
            (
                "2. Module".to_string(),
                format!(
                    "Ensure {} contains ryzen_smu",
                    self.persist_paths.modules_load.display()
                ),
            ),
            (
                "3. Service".to_string(),
                format!(
                    "Enable {} (boot + suspend resume with boot-loop guard)",
                    self.persist_paths.service_name
                ),
            ),
        ]
    }

    pub(super) fn open_persist_live_modal(&mut self) {
        if !self.staged.is_empty() {
            self.set_flash_kind(
                "Apply (a) or discard (u) staged changes before persisting live state",
                FlashKind::Warn,
            );
            return;
        }

        let steps = self.persist_enable_steps(format!(
            "Capture live SMU state → {}",
            self.persist_paths.active_profile.display()
        ));
        self.confirm_modal = Some(ConfirmModal::steps_modal(
            " Confirm — Persist Live State & Enable Boot Service ",
            "Save tested live SMU tuning for boot & suspend resume",
            ConfirmAction::PersistLiveAndEnable,
            steps,
            self.allow_extreme,
        ));
    }

    pub(super) fn open_toggle_service_modal(&mut self, hw: &HardwareContext) {
        if self.persist_status.service_enabled {
            let steps = vec![
                (
                    "1. Service".to_string(),
                    format!("Disable {} via systemctl", self.persist_paths.service_name),
                ),
                (
                    "2. Profile".to_string(),
                    format!(
                        "Keep {} untouched on disk (live SMU unchanged)",
                        self.persist_paths.active_profile.display()
                    ),
                ),
            ];
            self.confirm_modal = Some(ConfirmModal::steps_modal(
                " Confirm — Disable Boot Persistence Service ",
                format!(
                    "Disable {} on boot and suspend resume",
                    self.persist_paths.service_name
                ),
                ConfirmAction::ToggleBootService { enable: false },
                steps,
                false,
            ));
            return;
        }

        let needs_save = !self.persist_status.active_profile_exists
            || self.persist_status.active_profile_valid == Some(false);

        if needs_save {
            let steps = self.persist_enable_steps(format!(
                "Capture & save live SMU state → {}",
                self.persist_paths.active_profile.display()
            ));
            self.confirm_modal = Some(ConfirmModal::steps_modal(
                " Confirm — Save Live State & Enable Boot Service ",
                format!(
                    "{} not ready yet — will save live state first, then enable {}",
                    self.persist_paths.active_profile.display(),
                    self.persist_paths.service_name
                ),
                ConfirmAction::ToggleBootService { enable: true },
                steps,
                self.allow_extreme,
            ));
        } else {
            let steps = self.persist_enable_steps(format!(
                "Validate {} for {}",
                self.persist_paths.active_profile.display(),
                hw.codename.display_name()
            ));
            self.confirm_modal = Some(ConfirmModal::steps_modal(
                " Confirm — Enable Boot Persistence Service ",
                format!(
                    "Enable {} using existing {}",
                    self.persist_paths.service_name,
                    self.persist_paths.active_profile.display()
                ),
                ConfirmAction::ToggleBootService { enable: true },
                steps,
                self.allow_extreme,
            ));
        }
    }

    pub(super) fn open_clear_boot_guard_modal(&mut self) {
        if !self.persist_status.boot_guard_tripped {
            self.set_flash("Boot-loop guard is already clear");
            return;
        }
        let steps = vec![
            (
                "1. Guard".to_string(),
                format!(
                    "Remove tripped sentinel {}",
                    self.persist_paths.guard_path.display()
                ),
            ),
            (
                "2. Boot".to_string(),
                format!(
                    "Allow {} to auto-apply on next boot",
                    self.persist_paths.service_name
                ),
            ),
        ];
        self.confirm_modal = Some(ConfirmModal::steps_modal(
            " Confirm — Clear Boot-Loop Crash Guard ",
            "Clear tripped sentinel after confirming system stability",
            ConfirmAction::ClearBootGuard,
            steps,
            false,
        ));
    }
}
