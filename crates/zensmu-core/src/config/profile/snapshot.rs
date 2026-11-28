use super::{
    ApuProfile, CoProfile, PboProfile, Profile, ProfileTarget, ShaperTierProfile,
    PROFILE_SCHEMA_VERSION,
};
use crate::hw::profiles::{capabilities_for, CpuCapabilities};
use crate::safety::bounds::{PBO_SCALAR_MAX, PBO_SCALAR_MIN};
use crate::safety::snapshot::SmuStateSnapshot;
use crate::types::FreqTier;
use std::collections::BTreeMap;

impl Profile {
    /// Construct a declarative profile from a captured hardware [`SmuStateSnapshot`].
    ///
    /// Only fields that are actually writable on `snap.codename` (according to
    /// [`capabilities_for`]) and contain valid non-zero values are included, so
    /// read-only telemetry/queries such as `fmax_mhz` on Zen 2/3 or Lucienne
    /// never fail validation or apply.
    pub fn from_snapshot(snap: &SmuStateSnapshot) -> Self {
        let caps = capabilities_for(snap.codename);
        let can = |f: fn(&CpuCapabilities) -> bool| caps.as_ref().is_some_and(f);
        let cap_pos = |val: Option<u32>, f: fn(&CpuCapabilities) -> bool| {
            if can(f) {
                val.filter(|&v| v > 0)
            } else {
                None
            }
        };

        let co_cores = snap
            .co_per_core
            .as_deref()
            .filter(|cores| can(|c| c.set_co_per_core.is_some()) && !cores.is_empty());
        let igpu = if can(|c| c.set_igpu_co.is_some()) {
            snap.igpu_co.map(i32::from)
        } else {
            None
        };

        let curve_optimizer = if co_cores.is_some() || igpu.is_some() {
            let mut per_core = BTreeMap::new();
            if let Some(cores) = co_cores {
                for &(core, margin) in cores {
                    per_core.insert(core, i32::from(margin));
                }
            }
            Some(CoProfile {
                all: None,
                per_core,
                igpu,
            })
        } else {
            None
        };

        let curve_shaper = if can(|c| c.set_curve_shaper.is_some()) {
            snap.curve_shaper.map(|cs| {
                let mut map = BTreeMap::new();
                for tier in FreqTier::ALL {
                    let m = cs.tier(tier);
                    map.insert(
                        tier,
                        ShaperTierProfile {
                            low: Some(m.low),
                            med: Some(m.med),
                            high: Some(m.high),
                        },
                    );
                }
                map
            })
        } else {
            None
        };

        let ppt_w = cap_pos(snap.ppt_w, |c| c.set_ppt.is_some());
        let tdc_a = cap_pos(snap.tdc_a, |c| c.set_tdc.is_some());
        let edc_a = cap_pos(snap.edc_a, |c| c.set_edc.is_some());
        let tctl_c = cap_pos(snap.tctl_c, |c| c.set_tctl.is_some());
        let scalar = if can(|c| c.set_pbo_scalar.is_some()) {
            snap.pbo_scalar
                .filter(|s| s.is_finite() && *s >= PBO_SCALAR_MIN as f32)
                .map(|s| s.round() as u32)
                .filter(|s| (PBO_SCALAR_MIN..=PBO_SCALAR_MAX).contains(s))
        } else {
            None
        };
        let fmax_mhz = cap_pos(snap.fmax_mhz, |c| c.set_fmax.is_some());

        let pbo = if ppt_w.is_some()
            || tdc_a.is_some()
            || edc_a.is_some()
            || tctl_c.is_some()
            || scalar.is_some()
            || fmax_mhz.is_some()
        {
            Some(PboProfile {
                ppt_w,
                tdc_a,
                edc_a,
                tctl_c,
                scalar,
                fmax_mhz,
            })
        } else {
            None
        };

        let apu = if snap.codename.is_apu() {
            let stapm_w = cap_pos(snap.stapm_w, |c| c.set_stapm_limit.is_some());
            let slow_w = cap_pos(snap.slow_w, |c| c.set_slow_limit.is_some());
            let apu_slow_w = cap_pos(snap.apu_slow_w, |c| c.set_apu_slow_limit.is_some());
            let tdc_soc_a = cap_pos(snap.tdc_soc_a, |c| c.set_tdc_soc.is_some());
            let edc_soc_a = cap_pos(snap.edc_soc_a, |c| c.set_edc_soc.is_some());
            let apu_skin_temp_c = cap_pos(snap.apu_skin_temp_c, |c| c.set_apu_skin_temp.is_some());
            let dgpu_skin_temp_c =
                cap_pos(snap.dgpu_skin_temp_c, |c| c.set_dgpu_skin_temp.is_some());

            if stapm_w.is_some()
                || slow_w.is_some()
                || apu_slow_w.is_some()
                || tdc_soc_a.is_some()
                || edc_soc_a.is_some()
                || apu_skin_temp_c.is_some()
                || dgpu_skin_temp_c.is_some()
            {
                Some(ApuProfile {
                    stapm_w,
                    stapm_time_s: None,
                    fast_w: None, // already represented by pbo.ppt_w
                    slow_w,
                    slow_time_s: None,
                    apu_slow_w,
                    tdc_soc_a,
                    edc_soc_a,
                    apu_skin_temp_c,
                    dgpu_skin_temp_c,
                    power_mode: None,
                })
            } else {
                None
            }
        } else {
            None
        };

        Self {
            schema: PROFILE_SCHEMA_VERSION,
            target: ProfileTarget {
                codename: snap.codename,
                physical_cores: Some(snap.physical_cores),
            },
            curve_optimizer,
            curve_shaper,
            pbo,
            apu,
        }
    }
}
