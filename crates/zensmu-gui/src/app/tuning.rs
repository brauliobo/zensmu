//! Editable tuning state for Curve Optimizer, Curve Shaper, PBO, and APU DPTC.

use crate::elevate::CliAction;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::hw::profiles::CpuCapabilities;
use zensmu_core::ops::apu::{
    ApuSetRequest, APU_FAST_POWER_SOFT_MAX_W, APU_POWER_MAX_W, APU_POWER_MIN_W,
    APU_POWER_SOFT_MAX_W, APU_SKIN_TEMP_MAX_C, APU_SKIN_TEMP_MIN_C, APU_SKIN_TEMP_SOFT_MAX_C,
    APU_SOC_EDC_MAX_A, APU_SOC_EDC_MIN_A, APU_SOC_EDC_SOFT_MAX_A, APU_SOC_TDC_MAX_A,
    APU_SOC_TDC_MIN_A, APU_SOC_TDC_SOFT_MAX_A,
};
use zensmu_core::ops::pbo::{
    PboSetRequest, PboStatus, EDC_MAX_A, EDC_MIN_A, EDC_SOFT_MAX_A, FMAX_MAX_MHZ, FMAX_MIN_MHZ,
    PPT_MAX_W, PPT_MIN_W, PPT_SOFT_MAX_W, SCALAR_MAX, SCALAR_MIN, SCALAR_SOFT_MAX, TCTL_MAX_C,
    TCTL_MIN_C, TDC_MAX_A, TDC_MIN_A, TDC_SOFT_MAX_A,
};
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::types::{ApuPowerMode, CurveShaperState};

/// Editable Curve Optimizer UI state.
#[derive(Debug, Clone, Default)]
pub struct CoEditorState {
    pub all_core: i32,
    pub per_core: Vec<i32>,
    pub igpu: i32,
    pub allow_extreme: bool,
}

/// Editable Curve Shaper (Zen 5) UI state.
#[derive(Debug, Clone, Default)]
pub struct ShaperEditorState {
    pub draft: CurveShaperState,
    pub allow_extreme: bool,
}

/// Sync a positive finite live telemetry limit into a non-dirty `u32` slider field.
fn sync_u32_limit(target: &mut u32, dirty: bool, live: Option<f32>, min: u32, max: u32) {
    if !dirty {
        if let Some(v) = live.filter(|v| v.is_finite() && *v > 0.0) {
            *target = v.round().clamp(min as f32, max as f32) as u32;
        }
    }
}

/// Convert a finite, strictly positive telemetry float to `Some(u32)` for diff snapshots.
pub(super) fn finite_positive_u32(live: Option<f32>) -> Option<u32> {
    live.filter(|v| v.is_finite() && *v > 0.0)
        .map(|v| v.round() as u32)
}

/// Editable PBO Limits UI state.
#[derive(Debug, Clone)]
pub struct PboEditorState {
    pub ppt_enabled: bool,
    pub ppt_w: u32,
    pub ppt_dirty: bool,
    pub tdc_enabled: bool,
    pub tdc_a: u32,
    pub tdc_dirty: bool,
    pub edc_enabled: bool,
    pub edc_a: u32,
    pub edc_dirty: bool,
    pub tctl_enabled: bool,
    pub tctl_c: u32,
    pub tctl_dirty: bool,
    pub scalar_enabled: bool,
    pub scalar: u32,
    pub scalar_dirty: bool,
    pub fmax_enabled: bool,
    pub fmax_mhz: u32,
    pub fmax_dirty: bool,
    pub allow_extreme: bool,
}

impl Default for PboEditorState {
    fn default() -> Self {
        Self {
            ppt_enabled: true,
            ppt_w: 88,
            ppt_dirty: false,
            tdc_enabled: true,
            tdc_a: 75,
            tdc_dirty: false,
            edc_enabled: true,
            edc_a: 150,
            edc_dirty: false,
            tctl_enabled: true,
            tctl_c: 85,
            tctl_dirty: false,
            scalar_enabled: false,
            scalar: 1,
            scalar_dirty: false,
            fmax_enabled: false,
            fmax_mhz: 5500,
            fmax_dirty: false,
            allow_extreme: false,
        }
    }
}

impl PboEditorState {
    /// Sync live hardware PBO limits from a PM table telemetry snapshot into non-dirty slider fields.
    pub fn sync_from_telemetry(&mut self, snap: &TelemetrySnapshot) {
        sync_u32_limit(
            &mut self.ppt_w,
            self.ppt_dirty,
            snap.ppt_limit_w,
            PPT_MIN_W,
            PPT_MAX_W,
        );
        sync_u32_limit(
            &mut self.tdc_a,
            self.tdc_dirty,
            snap.tdc_limit_a,
            TDC_MIN_A,
            TDC_MAX_A,
        );
        sync_u32_limit(
            &mut self.edc_a,
            self.edc_dirty,
            snap.edc_limit_a,
            EDC_MIN_A,
            EDC_MAX_A,
        );
        sync_u32_limit(
            &mut self.tctl_c,
            self.tctl_dirty,
            snap.tctl_limit_c,
            TCTL_MIN_C,
            TCTL_MAX_C,
        );
    }

    /// Sync PBO scalar and Fmax from a `PboStatus` mailbox snapshot.
    ///
    /// When `force` is true (explicit refresh or post-apply resync), dirty flags for
    /// scalar and Fmax are cleared and overwritten from live hardware state.
    pub fn sync_from_status(&mut self, status: &PboStatus, force: bool) {
        if force {
            self.scalar_dirty = false;
            self.fmax_dirty = false;
        }
        sync_u32_limit(
            &mut self.scalar,
            self.scalar_dirty,
            status.scalar,
            SCALAR_MIN,
            SCALAR_MAX,
        );
        if !self.fmax_dirty {
            if let Some(fmax) = status.fmax_mhz {
                self.fmax_mhz = fmax.clamp(FMAX_MIN_MHZ, FMAX_MAX_MHZ);
            }
        }
    }

    /// Clear all dirty flags so subsequent telemetry/status snapshots resync slider values.
    pub fn clear_dirty(&mut self) {
        self.ppt_dirty = false;
        self.tdc_dirty = false;
        self.edc_dirty = false;
        self.tctl_dirty = false;
        self.scalar_dirty = false;
        self.fmax_dirty = false;
    }

    pub fn build_request_for(&self, caps: &CpuCapabilities) -> PboSetRequest {
        PboSetRequest {
            ppt_w: (self.ppt_enabled && caps.set_ppt.is_some()).then_some(self.ppt_w),
            tdc_a: (self.tdc_enabled && caps.set_tdc.is_some()).then_some(self.tdc_a),
            edc_a: (self.edc_enabled && caps.set_edc.is_some()).then_some(self.edc_a),
            tctl_c: (self.tctl_enabled && caps.set_tctl.is_some())
                .then_some(self.tctl_c.min(TCTL_MAX_C)),
            scalar: (self.scalar_enabled && caps.set_pbo_scalar.is_some()).then_some(self.scalar),
            fmax_mhz: (self.fmax_enabled && caps.set_fmax.is_some()).then_some(self.fmax_mhz),
        }
    }

    pub fn has_any_enabled_for(&self, caps: &CpuCapabilities) -> bool {
        (self.ppt_enabled && caps.set_ppt.is_some())
            || (self.tdc_enabled && caps.set_tdc.is_some())
            || (self.edc_enabled && caps.set_edc.is_some())
            || (self.tctl_enabled && caps.set_tctl.is_some())
            || (self.scalar_enabled && caps.set_pbo_scalar.is_some())
            || (self.fmax_enabled && caps.set_fmax.is_some())
    }

    /// Returns true if any enabled PBO field exceeds its conservative soft ceiling.
    pub fn has_any_extreme_for(&self, caps: &CpuCapabilities) -> bool {
        let req = self.build_request_for(caps);
        req.ppt_w.is_some_and(|v| v > PPT_SOFT_MAX_W)
            || req.tdc_a.is_some_and(|v| v > TDC_SOFT_MAX_A)
            || req.edc_a.is_some_and(|v| v > EDC_SOFT_MAX_A)
            || req.scalar.is_some_and(|v| v > SCALAR_SOFT_MAX)
    }
}

/// Editable mobile APU Dynamic Power and Thermal Configuration (DPTC) UI state.
#[derive(Debug, Clone)]
pub struct ApuEditorState {
    pub stapm_enabled: bool,
    pub stapm_w: u32,
    pub stapm_dirty: bool,
    pub live_stapm_w: Option<f32>,

    pub stapm_time_enabled: bool,
    pub stapm_time_s: u32,

    pub fast_enabled: bool,
    pub fast_w: u32,
    pub fast_dirty: bool,
    pub live_fast_w: Option<f32>,

    pub slow_enabled: bool,
    pub slow_w: u32,
    pub slow_dirty: bool,
    pub live_slow_w: Option<f32>,

    pub slow_time_enabled: bool,
    pub slow_time_s: u32,

    pub apu_slow_enabled: bool,
    pub apu_slow_w: u32,
    pub apu_slow_dirty: bool,
    pub live_apu_slow_w: Option<f32>,

    pub tdc_soc_enabled: bool,
    pub tdc_soc_a: u32,
    pub tdc_soc_dirty: bool,
    pub live_tdc_soc_a: Option<f32>,

    pub edc_soc_enabled: bool,
    pub edc_soc_a: u32,
    pub edc_soc_dirty: bool,
    pub live_edc_soc_a: Option<f32>,

    pub apu_skin_temp_enabled: bool,
    pub apu_skin_temp_c: u32,
    pub apu_skin_temp_dirty: bool,
    pub live_apu_skin_temp_c: Option<f32>,

    pub dgpu_skin_temp_enabled: bool,
    pub dgpu_skin_temp_c: u32,
    pub dgpu_skin_temp_dirty: bool,
    pub live_dgpu_skin_temp_c: Option<f32>,

    pub power_mode: Option<ApuPowerMode>,
    pub allow_extreme: bool,
}

impl Default for ApuEditorState {
    fn default() -> Self {
        Self {
            stapm_enabled: false,
            stapm_w: 28,
            stapm_dirty: false,
            live_stapm_w: None,

            stapm_time_enabled: false,
            stapm_time_s: 120,

            fast_enabled: false,
            fast_w: 45,
            fast_dirty: false,
            live_fast_w: None,

            slow_enabled: false,
            slow_w: 35,
            slow_dirty: false,
            live_slow_w: None,

            slow_time_enabled: false,
            slow_time_s: 10,

            apu_slow_enabled: false,
            apu_slow_w: 30,
            apu_slow_dirty: false,
            live_apu_slow_w: None,

            tdc_soc_enabled: false,
            tdc_soc_a: 30,
            tdc_soc_dirty: false,
            live_tdc_soc_a: None,

            edc_soc_enabled: false,
            edc_soc_a: 50,
            edc_soc_dirty: false,
            live_edc_soc_a: None,

            apu_skin_temp_enabled: false,
            apu_skin_temp_c: 55,
            apu_skin_temp_dirty: false,
            live_apu_skin_temp_c: None,

            dgpu_skin_temp_enabled: false,
            dgpu_skin_temp_c: 60,
            dgpu_skin_temp_dirty: false,
            live_dgpu_skin_temp_c: None,

            power_mode: None,
            allow_extreme: false,
        }
    }
}

impl ApuEditorState {
    /// Sync live hardware limits from a PM table telemetry snapshot into non-dirty slider fields.
    pub fn sync_from_telemetry(&mut self, snap: &TelemetrySnapshot) {
        self.live_stapm_w = snap.stapm_limit_w;
        self.live_fast_w = snap.ppt_limit_w;
        self.live_slow_w = snap.slow_limit_w;
        self.live_apu_slow_w = snap.apu_slow_limit_w;
        self.live_tdc_soc_a = snap.tdc_soc_limit_a;
        self.live_edc_soc_a = snap.edc_soc_limit_a;
        self.live_apu_skin_temp_c = snap.apu_skin_temp_limit_c;
        self.live_dgpu_skin_temp_c = snap.dgpu_skin_temp_limit_c;

        sync_u32_limit(
            &mut self.stapm_w,
            self.stapm_dirty,
            snap.stapm_limit_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
        );
        sync_u32_limit(
            &mut self.fast_w,
            self.fast_dirty,
            snap.ppt_limit_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
        );
        sync_u32_limit(
            &mut self.slow_w,
            self.slow_dirty,
            snap.slow_limit_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
        );
        sync_u32_limit(
            &mut self.apu_slow_w,
            self.apu_slow_dirty,
            snap.apu_slow_limit_w,
            APU_POWER_MIN_W,
            APU_POWER_MAX_W,
        );
        sync_u32_limit(
            &mut self.tdc_soc_a,
            self.tdc_soc_dirty,
            snap.tdc_soc_limit_a,
            APU_SOC_TDC_MIN_A,
            APU_SOC_TDC_MAX_A,
        );
        sync_u32_limit(
            &mut self.edc_soc_a,
            self.edc_soc_dirty,
            snap.edc_soc_limit_a,
            APU_SOC_EDC_MIN_A,
            APU_SOC_EDC_MAX_A,
        );
        sync_u32_limit(
            &mut self.apu_skin_temp_c,
            self.apu_skin_temp_dirty,
            snap.apu_skin_temp_limit_c,
            APU_SKIN_TEMP_MIN_C,
            APU_SKIN_TEMP_MAX_C,
        );
        sync_u32_limit(
            &mut self.dgpu_skin_temp_c,
            self.dgpu_skin_temp_dirty,
            snap.dgpu_skin_temp_limit_c,
            APU_SKIN_TEMP_MIN_C,
            APU_SKIN_TEMP_MAX_C,
        );
    }

    /// Clear all dirty flags so subsequent telemetry samples resync slider values.
    pub fn clear_dirty(&mut self) {
        self.stapm_dirty = false;
        self.fast_dirty = false;
        self.slow_dirty = false;
        self.apu_slow_dirty = false;
        self.tdc_soc_dirty = false;
        self.edc_soc_dirty = false;
        self.apu_skin_temp_dirty = false;
        self.dgpu_skin_temp_dirty = false;
    }

    /// Build an `ApuSetRequest` filtered to enabled fields supported by `hw`.
    pub fn build_request_for(&self, hw: &HardwareContext) -> ApuSetRequest {
        let caps = &hw.capabilities;
        let power_mode = self.power_mode.filter(|mode| match mode {
            ApuPowerMode::MaxPerformance => caps.set_max_performance.is_some(),
            ApuPowerMode::PowerSaving => caps.set_power_saving.is_some(),
        });
        ApuSetRequest {
            stapm_w: (self.stapm_enabled && caps.set_stapm_limit.is_some()).then_some(self.stapm_w),
            stapm_time_s: (self.stapm_time_enabled && caps.set_stapm_time.is_some())
                .then_some(self.stapm_time_s),
            fast_w: (self.fast_enabled && hw.codename.is_apu() && caps.set_ppt.is_some())
                .then_some(self.fast_w),
            slow_w: (self.slow_enabled && caps.set_slow_limit.is_some()).then_some(self.slow_w),
            slow_time_s: (self.slow_time_enabled && caps.set_slow_time.is_some())
                .then_some(self.slow_time_s),
            apu_slow_w: (self.apu_slow_enabled && caps.set_apu_slow_limit.is_some())
                .then_some(self.apu_slow_w),
            tdc_soc_a: (self.tdc_soc_enabled && caps.set_tdc_soc.is_some())
                .then_some(self.tdc_soc_a),
            edc_soc_a: (self.edc_soc_enabled && caps.set_edc_soc.is_some())
                .then_some(self.edc_soc_a),
            apu_skin_temp_c: (self.apu_skin_temp_enabled && caps.set_apu_skin_temp.is_some())
                .then_some(self.apu_skin_temp_c),
            dgpu_skin_temp_c: (self.dgpu_skin_temp_enabled && caps.set_dgpu_skin_temp.is_some())
                .then_some(self.dgpu_skin_temp_c),
            power_mode,
        }
    }

    pub fn has_any_enabled_for(&self, hw: &HardwareContext) -> bool {
        !self.build_request_for(hw).is_empty()
    }

    /// Returns true if any enabled APU field exceeds its conservative soft ceiling.
    pub fn has_any_extreme_for(&self, hw: &HardwareContext) -> bool {
        let req = self.build_request_for(hw);
        req.stapm_w.is_some_and(|v| v > APU_POWER_SOFT_MAX_W)
            || req.fast_w.is_some_and(|v| v > APU_FAST_POWER_SOFT_MAX_W)
            || req.slow_w.is_some_and(|v| v > APU_POWER_SOFT_MAX_W)
            || req.apu_slow_w.is_some_and(|v| v > APU_POWER_SOFT_MAX_W)
            || req.tdc_soc_a.is_some_and(|v| v > APU_SOC_TDC_SOFT_MAX_A)
            || req.edc_soc_a.is_some_and(|v| v > APU_SOC_EDC_SOFT_MAX_A)
            || req
                .apu_skin_temp_c
                .is_some_and(|v| v > APU_SKIN_TEMP_SOFT_MAX_C)
            || req
                .dgpu_skin_temp_c
                .is_some_and(|v| v > APU_SKIN_TEMP_SOFT_MAX_C)
    }

    /// Build a `CliAction::ApuSet` containing only enabled, hardware-supported fields.
    pub fn build_cli_action_for(&self, hw: &HardwareContext, dry_run: bool) -> Option<CliAction> {
        let req = self.build_request_for(hw);
        if req.is_empty() {
            return None;
        }
        Some(CliAction::ApuSet {
            req,
            allow_extreme: self.allow_extreme,
            dry_run,
        })
    }
}
