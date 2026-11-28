//! Tune parameter catalog, bounds, live values, and stepping on `App`.

use super::app::App;
use super::palette;
use super::types::{MailboxState, TuneParam, TuneRow, TuneSection, TuneValue};
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::ops::pbo::{self, FusedLimits};
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::ops::{apu, co, shaper};
use zensmu_core::types::{ApuPowerMode, FreqTier, TempBand};

impl App {
    // -----------------------------------------------------------------------
    // Tune parameter catalog, bounds, live values & stepping
    // -----------------------------------------------------------------------

    pub(super) fn build_tune_rows(&self, hw: &HardwareContext) -> Vec<TuneRow> {
        let mut rows = Vec::new();
        self.push_co_tune_rows(hw, &mut rows);
        self.push_shaper_tune_rows(hw, &mut rows);
        self.push_pbo_tune_rows(hw, &mut rows);
        self.push_apu_tune_rows(hw, &mut rows);
        rows
    }

    pub(super) fn push_co_tune_rows(&self, hw: &HardwareContext, rows: &mut Vec<TuneRow>) {
        let caps = &hw.capabilities;
        if caps.set_co_per_core.is_none() && caps.set_co_all_cores.is_none() {
            return;
        }
        let bounds_hint = caps
            .co_bounds
            .map(|b| format!("{}..=+{} (soft >= {})", b.min, b.max, b.soft_min))
            .unwrap_or_default();
        rows.push(TuneRow::SectionHeader(
            TuneSection::CurveOptimizer.title().to_string(),
            bounds_hint,
        ));
        if caps.set_co_all_cores.is_some() {
            rows.push(TuneRow::Param(TuneParam::CoAll));
        }
        if caps.set_co_per_core.is_some() {
            for core in &hw.topology.cores {
                rows.push(TuneRow::Param(TuneParam::CoCore(core.index)));
            }
        }
        if caps.set_igpu_co.is_some() {
            rows.push(TuneRow::Param(TuneParam::CoIgpu));
        }
    }

    pub(super) fn push_shaper_tune_rows(&self, hw: &HardwareContext, rows: &mut Vec<TuneRow>) {
        if hw.capabilities.set_curve_shaper.is_none() {
            return;
        }
        rows.push(TuneRow::SectionHeader(
            TuneSection::CurveShaper.title().to_string(),
            format!(
                "{}..=+{} (soft >= {})",
                shaper::SHAPER_MIN,
                shaper::SHAPER_MAX,
                shaper::SHAPER_SOFT_MIN
            ),
        ));
        if let MailboxState::FirmwareUnsupported(code) = &self.shaper_state {
            rows.push(TuneRow::InfoBanner(
                format!("Not exposed by SMU {} ({code})", hw.driver.smu_version),
                palette::YELLOW,
            ));
        } else {
            for tier in FreqTier::ALL {
                for band in TempBand::ALL {
                    rows.push(TuneRow::Param(TuneParam::Shaper { tier, band }));
                }
            }
        }
    }

    pub(super) fn push_pbo_tune_rows(&self, hw: &HardwareContext, rows: &mut Vec<TuneRow>) {
        let caps = &hw.capabilities;
        let pbo_params = [
            (caps.set_ppt.is_some(), TuneParam::PboPpt),
            (caps.set_tdc.is_some(), TuneParam::PboTdc),
            (caps.set_edc.is_some(), TuneParam::PboEdc),
            (caps.set_tctl.is_some(), TuneParam::PboTctl),
            (caps.set_pbo_scalar.is_some(), TuneParam::PboScalar),
            (caps.set_fmax.is_some(), TuneParam::PboFmax),
        ];
        if !pbo_params.iter().any(|(supported, _)| *supported) {
            return;
        }
        let pbo_hint = match self.pbo_state.as_loaded() {
            Some(pbo) => {
                let mut parts = Vec::new();
                if let Some(tdp) = pbo.fused.tdp_w {
                    parts.push(format!("TDP {tdp}W"));
                }
                if let Some(tjmax) = pbo.fused.tjmax_c {
                    parts.push(format!("TjMax {tjmax}°C"));
                }
                if parts.is_empty() {
                    format!("Tctl max {}°C", pbo::TCTL_MAX_C)
                } else {
                    parts.join(" · ")
                }
            }
            None => format!("Tctl max {}°C", pbo::TCTL_MAX_C),
        };
        rows.push(TuneRow::SectionHeader(
            TuneSection::Pbo.title().to_string(),
            pbo_hint,
        ));
        for (supported, param) in pbo_params {
            if supported {
                rows.push(TuneRow::Param(param));
            }
        }
    }

    pub(super) fn push_apu_tune_rows(&self, hw: &HardwareContext, rows: &mut Vec<TuneRow>) {
        let caps = &hw.capabilities;
        if caps.set_stapm_limit.is_none() {
            return;
        }
        rows.push(TuneRow::SectionHeader(
            TuneSection::Apu.title().to_string(),
            "Mobile Power & Thermal".to_string(),
        ));
        let apu_params = [
            (true, TuneParam::ApuStapm),
            (caps.set_stapm_time.is_some(), TuneParam::ApuStapmTime),
            (
                hw.codename.is_apu() && caps.set_ppt.is_some(),
                TuneParam::ApuFast,
            ),
            (caps.set_slow_limit.is_some(), TuneParam::ApuSlow),
            (caps.set_slow_time.is_some(), TuneParam::ApuSlowTime),
            (caps.set_apu_slow_limit.is_some(), TuneParam::ApuApuSlow),
            (caps.set_tdc_soc.is_some(), TuneParam::ApuTdcSoc),
            (caps.set_edc_soc.is_some(), TuneParam::ApuEdcSoc),
            (caps.set_apu_skin_temp.is_some(), TuneParam::ApuSkinTemp),
            (
                caps.set_dgpu_skin_temp.is_some(),
                TuneParam::ApuDgpuSkinTemp,
            ),
            (
                caps.set_max_performance.is_some() || caps.set_power_saving.is_some(),
                TuneParam::ApuPowerMode,
            ),
        ];
        for (supported, param) in apu_params {
            if supported {
                rows.push(TuneRow::Param(param));
            }
        }
    }

    pub(super) fn tune_params(&self, hw: &HardwareContext) -> Vec<TuneParam> {
        self.build_tune_rows(hw)
            .into_iter()
            .filter_map(|r| match r {
                TuneRow::Param(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    pub(super) fn clamp_tune_selection(&mut self, hw: &HardwareContext) {
        let count = self.tune_params(hw).len();
        if count == 0 {
            self.tune_selected = 0;
        } else if self.tune_selected >= count {
            self.tune_selected = count - 1;
        }
    }

    pub(super) fn selected_tune_param(&self, hw: &HardwareContext) -> Option<TuneParam> {
        let params = self.tune_params(hw);
        params.get(self.tune_selected).copied()
    }

    pub(super) fn live_telemetry_int(
        &self,
        getter: impl FnOnce(&TelemetrySnapshot) -> Option<f32>,
    ) -> Option<TuneValue> {
        self.telemetry
            .as_ref()
            .and_then(getter)
            .filter(|v| v.is_finite())
            .map(|v| TuneValue::Int(v.round() as i32))
    }

    pub(super) fn live_telemetry_or_fused(
        &self,
        telem_getter: impl FnOnce(&TelemetrySnapshot) -> Option<f32>,
        fused_getter: impl FnOnce(&FusedLimits) -> Option<u32>,
    ) -> Option<TuneValue> {
        self.live_telemetry_int(telem_getter).or_else(|| {
            self.pbo_state
                .as_loaded()
                .and_then(|p| fused_getter(&p.fused))
                .map(|v| TuneValue::Int(v as i32))
        })
    }

    /// Returns the known live value of `param` from hardware telemetry / mailbox readback.
    pub(super) fn live_param_value(&self, param: TuneParam) -> Option<TuneValue> {
        match param {
            TuneParam::CoAll => {
                let snap = self.co_state.as_loaded()?;
                let first = snap.cores.first()?.margin;
                snap.cores
                    .iter()
                    .all(|c| c.margin == first)
                    .then_some(TuneValue::Int(i32::from(first)))
            }
            TuneParam::CoCore(idx) => self
                .co_state
                .as_loaded()
                .and_then(|s| s.cores.iter().find(|c| c.core_index == idx))
                .map(|c| TuneValue::Int(i32::from(c.margin))),
            TuneParam::CoIgpu => self
                .co_state
                .as_loaded()
                .and_then(|s| s.igpu)
                .map(|v| TuneValue::Int(i32::from(v))),
            TuneParam::Shaper { tier, band } => {
                let cs = self.shaper_state.as_loaded()?;
                let t = cs.tier(tier);
                let v = match band {
                    TempBand::Low => t.low,
                    TempBand::Med => t.med,
                    TempBand::High => t.high,
                };
                Some(TuneValue::Int(i32::from(v)))
            }
            TuneParam::PboPpt => self.live_telemetry_or_fused(|t| t.ppt_limit_w, |f| f.ppt_w),
            TuneParam::PboTdc => self.live_telemetry_or_fused(|t| t.tdc_limit_a, |f| f.vdd_tdc_a),
            TuneParam::PboEdc => self.live_telemetry_or_fused(|t| t.edc_limit_a, |f| f.edc_a),
            TuneParam::PboTctl => self.live_telemetry_int(|t| t.tctl_limit_c),
            TuneParam::PboScalar => self
                .pbo_state
                .as_loaded()
                .and_then(|p| p.scalar)
                .filter(|v| v.is_finite() && *v >= 1.0)
                .map(|v| TuneValue::Int(v.round().clamp(1.0, 10.0) as i32)),
            TuneParam::PboFmax => self
                .pbo_state
                .as_loaded()
                .and_then(|p| p.fmax_mhz)
                .map(|v| TuneValue::Int(v as i32)),
            TuneParam::ApuStapm => self.live_telemetry_or_fused(|t| t.stapm_limit_w, |f| f.slow_w),
            TuneParam::ApuStapmTime | TuneParam::ApuSlowTime => None,
            TuneParam::ApuFast => self.live_telemetry_or_fused(|t| t.ppt_limit_w, |f| f.fast_w),
            TuneParam::ApuSlow => self.live_telemetry_or_fused(|t| t.slow_limit_w, |f| f.slow_w),
            TuneParam::ApuApuSlow => {
                self.live_telemetry_or_fused(|t| t.apu_slow_limit_w, |f| f.apu_slow_w)
            }
            TuneParam::ApuTdcSoc => {
                self.live_telemetry_or_fused(|t| t.tdc_soc_limit_a, |f| f.soc_tdc_a)
            }
            TuneParam::ApuEdcSoc => self.live_telemetry_int(|t| t.edc_soc_limit_a),
            TuneParam::ApuSkinTemp => self.live_telemetry_int(|t| t.apu_skin_temp_limit_c),
            TuneParam::ApuDgpuSkinTemp => self.live_telemetry_int(|t| t.dgpu_skin_temp_limit_c),
            TuneParam::ApuPowerMode => Some(TuneValue::PowerMode(None)),
        }
    }

    /// Fallback starting value when stepping a parameter whose live value is unknown.
    pub(super) fn default_step_base(&self, param: TuneParam) -> TuneValue {
        match param {
            TuneParam::CoAll
            | TuneParam::CoCore(_)
            | TuneParam::CoIgpu
            | TuneParam::Shaper { .. } => TuneValue::Int(0),
            TuneParam::PboPpt => TuneValue::Int(88),
            TuneParam::PboTdc => TuneValue::Int(75),
            TuneParam::PboEdc => TuneValue::Int(150),
            TuneParam::PboTctl => TuneValue::Int(85),
            TuneParam::PboScalar => TuneValue::Int(1),
            TuneParam::PboFmax => TuneValue::Int(5000),
            TuneParam::ApuStapm => TuneValue::Int(28),
            TuneParam::ApuStapmTime => TuneValue::Int(200),
            TuneParam::ApuFast => TuneValue::Int(45),
            TuneParam::ApuSlow => TuneValue::Int(35),
            TuneParam::ApuSlowTime => TuneValue::Int(5),
            TuneParam::ApuApuSlow => TuneValue::Int(30),
            TuneParam::ApuTdcSoc => TuneValue::Int(20),
            TuneParam::ApuEdcSoc => TuneValue::Int(35),
            TuneParam::ApuSkinTemp => TuneValue::Int(45),
            TuneParam::ApuDgpuSkinTemp => TuneValue::Int(55),
            TuneParam::ApuPowerMode => TuneValue::PowerMode(None),
        }
    }

    pub(super) fn param_int_bounds(&self, param: TuneParam, hw: &HardwareContext) -> (i32, i32) {
        match param {
            TuneParam::CoAll | TuneParam::CoCore(_) => hw
                .capabilities
                .co_bounds
                .map(|b| (b.min, b.max))
                .unwrap_or((-30, 30)),
            TuneParam::CoIgpu => (co::IGPU_CO_MIN, co::IGPU_CO_MAX),
            TuneParam::Shaper { .. } => {
                (i32::from(shaper::SHAPER_MIN), i32::from(shaper::SHAPER_MAX))
            }
            TuneParam::PboPpt => (pbo::PPT_MIN_W as i32, pbo::PPT_MAX_W as i32),
            TuneParam::PboTdc => (pbo::TDC_MIN_A as i32, pbo::TDC_MAX_A as i32),
            TuneParam::PboEdc => (pbo::EDC_MIN_A as i32, pbo::EDC_MAX_A as i32),
            TuneParam::PboTctl => (pbo::TCTL_MIN_C as i32, pbo::TCTL_MAX_C as i32),
            TuneParam::PboScalar => (pbo::SCALAR_MIN as i32, pbo::SCALAR_MAX as i32),
            TuneParam::PboFmax => (pbo::FMAX_MIN_MHZ as i32, pbo::FMAX_MAX_MHZ as i32),
            TuneParam::ApuStapm
            | TuneParam::ApuFast
            | TuneParam::ApuSlow
            | TuneParam::ApuApuSlow => (apu::APU_POWER_MIN_W as i32, apu::APU_POWER_MAX_W as i32),
            TuneParam::ApuStapmTime | TuneParam::ApuSlowTime => {
                (apu::APU_TIME_MIN_S as i32, apu::APU_TIME_MAX_S as i32)
            }
            TuneParam::ApuTdcSoc => (apu::APU_SOC_TDC_MIN_A as i32, apu::APU_SOC_TDC_MAX_A as i32),
            TuneParam::ApuEdcSoc => (apu::APU_SOC_EDC_MIN_A as i32, apu::APU_SOC_EDC_MAX_A as i32),
            TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp => (
                apu::APU_SKIN_TEMP_MIN_C as i32,
                apu::APU_SKIN_TEMP_MAX_C as i32,
            ),
            TuneParam::ApuPowerMode => (0, 2),
        }
    }

    pub(super) fn param_steps(param: TuneParam) -> (i32, i32) {
        match param {
            TuneParam::CoAll
            | TuneParam::CoCore(_)
            | TuneParam::CoIgpu
            | TuneParam::Shaper { .. } => (1, 5),
            TuneParam::PboPpt => (5, 25),
            TuneParam::PboTdc => (5, 20),
            TuneParam::PboEdc => (5, 25),
            TuneParam::PboTctl => (1, 5),
            TuneParam::PboScalar => (1, 2),
            TuneParam::PboFmax => (25, 100),
            TuneParam::ApuStapm
            | TuneParam::ApuFast
            | TuneParam::ApuSlow
            | TuneParam::ApuApuSlow => (1, 5),
            TuneParam::ApuStapmTime => (5, 30),
            TuneParam::ApuSlowTime => (1, 10),
            TuneParam::ApuTdcSoc | TuneParam::ApuEdcSoc => (1, 5),
            TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp => (1, 5),
            TuneParam::ApuPowerMode => (1, 1),
        }
    }

    pub(super) fn param_exceeds_soft_limit(
        &self,
        param: TuneParam,
        val: TuneValue,
        hw: &HardwareContext,
    ) -> bool {
        let Some(v) = val.as_int() else {
            return false;
        };
        match param {
            TuneParam::CoAll | TuneParam::CoCore(_) => {
                hw.capabilities.co_bounds.is_some_and(|b| v < b.soft_min)
            }
            TuneParam::CoIgpu => v < co::IGPU_CO_SOFT_MIN,
            TuneParam::Shaper { .. } => v < i32::from(shaper::SHAPER_SOFT_MIN),
            TuneParam::PboPpt => v > pbo::PPT_SOFT_MAX_W as i32,
            TuneParam::PboTdc => v > pbo::TDC_SOFT_MAX_A as i32,
            TuneParam::PboEdc => v > pbo::EDC_SOFT_MAX_A as i32,
            TuneParam::PboTctl => false, // hard max 95 C always enforced
            TuneParam::PboScalar => v > pbo::SCALAR_SOFT_MAX as i32,
            TuneParam::PboFmax => self
                .pbo_state
                .as_loaded()
                .and_then(|p| p.fmax_mhz)
                .is_some_and(|cur| v > (cur.saturating_add(pbo::FMAX_SOFT_DELTA_MHZ)) as i32),
            TuneParam::ApuStapm | TuneParam::ApuSlow | TuneParam::ApuApuSlow => {
                v > apu::APU_POWER_SOFT_MAX_W as i32
            }
            TuneParam::ApuFast => v > apu::APU_FAST_POWER_SOFT_MAX_W as i32,
            TuneParam::ApuStapmTime | TuneParam::ApuSlowTime => false,
            TuneParam::ApuTdcSoc => v > apu::APU_SOC_TDC_SOFT_MAX_A as i32,
            TuneParam::ApuEdcSoc => v > apu::APU_SOC_EDC_SOFT_MAX_A as i32,
            TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp => {
                v > apu::APU_SKIN_TEMP_SOFT_MAX_C as i32
            }
            TuneParam::ApuPowerMode => false,
        }
    }

    pub(super) fn adjust_selected_param(
        &mut self,
        hw: &HardwareContext,
        delta_sign: i32,
        coarse: bool,
    ) {
        let Some(param) = self.selected_tune_param(hw) else {
            return;
        };

        let live = self.live_param_value(param);
        let current = self
            .staged
            .get(&param)
            .copied()
            .or(live)
            .unwrap_or_else(|| self.default_step_base(param));

        let next = match (param, current) {
            (TuneParam::ApuPowerMode, TuneValue::PowerMode(cur_mode)) => {
                let idx = match cur_mode {
                    None => 0i32,
                    Some(ApuPowerMode::MaxPerformance) => 1,
                    Some(ApuPowerMode::PowerSaving) => 2,
                };
                let next_idx = (idx + delta_sign).clamp(0, 2);
                let next_mode = match next_idx {
                    1 => Some(ApuPowerMode::MaxPerformance),
                    2 => Some(ApuPowerMode::PowerSaving),
                    _ => None,
                };
                TuneValue::PowerMode(next_mode)
            }
            (_, TuneValue::Int(cur_int)) => {
                let (fine, big) = Self::param_steps(param);
                let step = if coarse { big } else { fine };
                let (min_v, max_v) = self.param_int_bounds(param, hw);
                let next_v = cur_int
                    .saturating_add(delta_sign * step)
                    .clamp(min_v, max_v);
                TuneValue::Int(next_v)
            }
            _ => return,
        };

        if live == Some(next)
            || (param == TuneParam::ApuPowerMode && next == TuneValue::PowerMode(None))
        {
            self.staged.remove(&param);
        } else {
            self.staged.insert(param, next);
        }
    }

    pub(super) fn reset_selected_param(&mut self, hw: &HardwareContext, to_zero_if_co: bool) {
        let Some(param) = self.selected_tune_param(hw) else {
            return;
        };
        let live = self.live_param_value(param);
        if to_zero_if_co && param.is_co_or_shaper() {
            if live == Some(TuneValue::Int(0)) {
                self.staged.remove(&param);
            } else {
                self.staged.insert(param, TuneValue::Int(0));
            }
        } else {
            self.staged.remove(&param);
        }
    }

    pub(super) fn jump_tune_section(&mut self, hw: &HardwareContext, forward: bool) {
        let params = self.tune_params(hw);
        let Some(cur_param) = params
            .get(self.tune_selected.min(params.len().saturating_sub(1)))
            .copied()
        else {
            return;
        };
        let cur_sec = cur_param.section();

        // Collect first index of each distinct section in order
        let mut starts: Vec<(TuneSection, usize)> = Vec::new();
        for (i, p) in params.iter().enumerate() {
            let s = p.section();
            if starts.last().map(|(ls, _)| *ls) != Some(s) {
                starts.push((s, i));
            }
        }
        if starts.len() <= 1 {
            return;
        }
        let sec_pos = starts.iter().position(|(s, _)| *s == cur_sec).unwrap_or(0);
        let next_pos = if forward {
            (sec_pos + 1) % starts.len()
        } else if sec_pos == 0 {
            starts.len() - 1
        } else {
            sec_pos - 1
        };
        if let Some(&(_, target_idx)) = starts.get(next_pos) {
            self.tune_selected = target_idx;
        }
    }
}
