//! Declarative TOML tuning profiles with target validation and state diffing.

use crate::error::{Error, Result};
use crate::hw::profiles::{capabilities_for, CpuCapabilities};
use crate::safety::bounds::{
    validate_apu_fast_watts, validate_apu_soc_edc_amps, validate_apu_soc_tdc_amps,
    validate_apu_time_secs, validate_apu_watts, validate_co_margin, validate_edc_amps,
    validate_fmax_mhz, validate_igpu_co_margin, validate_pbo_scalar_checked, validate_ppt_watts,
    validate_shaper_margin, validate_skin_temp_celsius, validate_tctl_celsius, validate_tdc_amps,
    PBO_SCALAR_MAX, PBO_SCALAR_MIN,
};
use crate::safety::snapshot::SmuStateSnapshot;
use crate::safety::{map_io_err, write_atomic};
use crate::types::{ApuPowerMode, Codename, FreqTier};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

/// Current profile TOML schema version.
pub const PROFILE_SCHEMA_VERSION: u32 = 1;

/// Declarative Ryzen SMU tuning profile (serialized as TOML).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Profile {
    pub schema: u32,
    pub target: ProfileTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve_optimizer: Option<CoProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curve_shaper: Option<BTreeMap<FreqTier, ShaperTierProfile>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pbo: Option<PboProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apu: Option<ApuProfile>,
}

/// CPU identity target pinned in a profile to prevent cross-CPU misapplication.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProfileTarget {
    pub codename: Codename,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub physical_cores: Option<usize>,
}

/// Curve Optimizer tuning section.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct CoProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub all: Option<i32>,
    #[serde(
        default,
        skip_serializing_if = "BTreeMap::is_empty",
        with = "u32_map_serde"
    )]
    pub per_core: BTreeMap<u32, i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub igpu: Option<i32>,
}

/// Curve Shaper temperature-band margins for a single frequency tier.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ShaperTierProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub low: Option<i8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub med: Option<i8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub high: Option<i8>,
}

/// Precision Boost Overdrive (PBO) limits and boost ceiling section.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct PboProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ppt_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tdc_a: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edc_a: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tctl_c: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scalar: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fmax_mhz: Option<u32>,
}

/// APU-specific DPTC power, current, time-constant, and skin-temperature section.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ApuProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stapm_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stapm_time_s: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slow_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slow_time_s: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apu_slow_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tdc_soc_a: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edc_soc_a: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apu_skin_temp_c: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dgpu_skin_temp_c: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power_mode: Option<ApuPowerMode>,
}

/// Single field difference between current hardware state and a desired profile.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ProfileDiffEntry {
    pub field: String,
    pub current: String,
    pub desired: String,
}

impl Profile {
    /// Parse a profile from a TOML string.
    pub fn from_toml_str(s: &str) -> Result<Self> {
        toml::from_str(s).map_err(|e| Error::Serde(e.to_string()))
    }

    /// Load a TOML profile from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let content = fs::read_to_string(path).map_err(|e| map_io_err(path, e))?;
        Self::from_toml_str(&content)
    }

    /// Serialize this profile to a pretty TOML string.
    pub fn to_toml_string(&self) -> Result<String> {
        toml::to_string_pretty(self).map_err(|e| Error::Serde(e.to_string()))
    }

    /// Atomically save this profile to disk as TOML (write temp + sync + rename + dir sync).
    pub fn save(&self, path: &Path) -> Result<()> {
        let content = self.to_toml_string()?;
        write_atomic(path, content.as_bytes())
    }

    /// Validate schema, target hardware match, capability support, core indices, and safety bounds.
    pub fn validate_for(
        &self,
        codename: Codename,
        physical_cores: usize,
        allow_extreme: bool,
    ) -> Result<()> {
        if self.schema != PROFILE_SCHEMA_VERSION {
            return Err(Error::InvalidInput(format!(
                "unsupported profile schema version {} (expected {})",
                self.schema, PROFILE_SCHEMA_VERSION
            )));
        }

        if self.target.codename != codename {
            return Err(Error::ProfileTargetMismatch {
                expected: self.target.codename.to_string(),
                actual: codename.to_string(),
            });
        }

        if let Some(expected_cores) = self.target.physical_cores {
            if expected_cores != physical_cores {
                return Err(Error::ProfileTargetMismatch {
                    expected: format!("{} ({} cores)", self.target.codename, expected_cores),
                    actual: format!("{} ({} cores)", codename, physical_cores),
                });
            }
        }

        let caps = capabilities_for(codename).ok_or_else(|| Error::UnsupportedCpu {
            codename: codename.to_string(),
            detail: "no verified capability profile for this CPU".into(),
        })?;

        if let Some(co) = &self.curve_optimizer {
            let has_any_co = co.all.is_some() || !co.per_core.is_empty() || co.igpu.is_some();
            if has_any_co {
                let bounds = caps.co_bounds.ok_or_else(|| Error::UnsupportedCapability {
                    capability: "curve_optimizer",
                    codename: codename.to_string(),
                })?;

                if let Some(all_margin) = co.all {
                    require_cap(
                        caps.set_co_all_cores.is_some() || caps.set_co_per_core.is_some(),
                        "curve_optimizer",
                        codename,
                    )?;
                    validate_co_margin("curve_optimizer.all", all_margin, bounds, allow_extreme)?;
                }

                if !co.per_core.is_empty() {
                    require_cap(
                        caps.set_co_per_core.is_some(),
                        "curve_optimizer_per_core",
                        codename,
                    )?;
                }

                for (&core_idx, &margin) in &co.per_core {
                    if (core_idx as usize) >= physical_cores {
                        return Err(Error::OutOfBounds {
                            field: "curve_optimizer.per_core".into(),
                            value: format!("core {core_idx}"),
                            bounds: format!("0..{physical_cores}"),
                        });
                    }
                    validate_co_margin(
                        &format!("curve_optimizer.per_core.{core_idx}"),
                        margin,
                        bounds,
                        allow_extreme,
                    )?;
                }

                if let Some(igpu_margin) = co.igpu {
                    require_cap(caps.set_igpu_co.is_some(), "igpu_co", codename)?;
                    validate_igpu_co_margin("curve_optimizer.igpu", igpu_margin, allow_extreme)?;
                }
            }
        }

        if let Some(shaper) = &self.curve_shaper {
            let has_any_shaper = shaper
                .values()
                .any(|t| t.low.is_some() || t.med.is_some() || t.high.is_some());
            if has_any_shaper {
                require_cap(caps.set_curve_shaper.is_some(), "curve_shaper", codename)?;
                for (&tier, margins) in shaper {
                    for (band, val) in [
                        ("low", margins.low),
                        ("med", margins.med),
                        ("high", margins.high),
                    ] {
                        if let Some(v) = val {
                            validate_shaper_margin(
                                &format!("curve_shaper.{}.{band}", tier.label()),
                                i32::from(v),
                                allow_extreme,
                            )?;
                        }
                    }
                }
            }
        }

        if let Some(pbo) = &self.pbo {
            if let Some(ppt) = pbo.ppt_w {
                require_cap(caps.set_ppt.is_some(), "pbo_ppt", codename)?;
                validate_ppt_watts(ppt, allow_extreme)?;
            }
            if let Some(tdc) = pbo.tdc_a {
                require_cap(caps.set_tdc.is_some(), "pbo_tdc", codename)?;
                validate_tdc_amps(tdc, allow_extreme)?;
            }
            if let Some(edc) = pbo.edc_a {
                require_cap(caps.set_edc.is_some(), "pbo_edc", codename)?;
                validate_edc_amps(edc, allow_extreme)?;
            }
            if let Some(tctl) = pbo.tctl_c {
                require_cap(caps.set_tctl.is_some(), "pbo_tctl", codename)?;
                validate_tctl_celsius(tctl, allow_extreme)?;
            }
            if let Some(scalar) = pbo.scalar {
                require_cap(caps.set_pbo_scalar.is_some(), "pbo_scalar", codename)?;
                validate_pbo_scalar_checked(scalar, allow_extreme)?;
            }
            if let Some(fmax) = pbo.fmax_mhz {
                require_cap(caps.set_fmax.is_some(), "fmax_boost_limit", codename)?;
                validate_fmax_mhz(fmax, allow_extreme)?;
            }
        }

        if let Some(apu) = &self.apu {
            if let Some(w) = apu.stapm_w {
                require_cap(caps.set_stapm_limit.is_some(), "apu_stapm", codename)?;
                validate_apu_watts("apu.stapm_w", w, allow_extreme)?;
            }
            if let Some(w) = apu.fast_w {
                require_cap(
                    codename.is_apu() && caps.set_ppt.is_some(),
                    "apu_fast_limit",
                    codename,
                )?;
                validate_apu_fast_watts("apu.fast_w", w, allow_extreme)?;
            }
            if let Some(w) = apu.slow_w {
                require_cap(
                    codename.is_apu() && caps.set_slow_limit.is_some(),
                    "apu_slow_limit",
                    codename,
                )?;
                validate_apu_watts("apu.slow_w", w, allow_extreme)?;
            }
            if let Some(w) = apu.apu_slow_w {
                require_cap(
                    caps.set_apu_slow_limit.is_some(),
                    "apu_slow_limit",
                    codename,
                )?;
                validate_apu_watts("apu.apu_slow_w", w, allow_extreme)?;
            }
            if let Some(s) = apu.stapm_time_s {
                require_cap(caps.set_stapm_time.is_some(), "apu_stapm_time", codename)?;
                validate_apu_time_secs("apu.stapm_time_s", s)?;
            }
            if let Some(s) = apu.slow_time_s {
                require_cap(caps.set_slow_time.is_some(), "apu_slow_time", codename)?;
                validate_apu_time_secs("apu.slow_time_s", s)?;
            }
            if let Some(a) = apu.tdc_soc_a {
                require_cap(caps.set_tdc_soc.is_some(), "apu_tdc_soc", codename)?;
                validate_apu_soc_tdc_amps("apu.tdc_soc_a", a, allow_extreme)?;
            }
            if let Some(a) = apu.edc_soc_a {
                require_cap(caps.set_edc_soc.is_some(), "apu_edc_soc", codename)?;
                validate_apu_soc_edc_amps("apu.edc_soc_a", a, allow_extreme)?;
            }
            if let Some(c) = apu.apu_skin_temp_c {
                require_cap(caps.set_apu_skin_temp.is_some(), "apu_skin_temp", codename)?;
                validate_skin_temp_celsius("apu.apu_skin_temp_c", c, allow_extreme)?;
            }
            if let Some(c) = apu.dgpu_skin_temp_c {
                require_cap(
                    caps.set_dgpu_skin_temp.is_some(),
                    "dgpu_skin_temp",
                    codename,
                )?;
                validate_skin_temp_celsius("apu.dgpu_skin_temp_c", c, allow_extreme)?;
            }
            if let Some(mode) = apu.power_mode {
                let supported = match mode {
                    ApuPowerMode::MaxPerformance => caps.set_max_performance.is_some(),
                    ApuPowerMode::PowerSaving => caps.set_power_saving.is_some(),
                };
                require_cap(supported, "apu_power_mode", codename)?;
            }
        }

        Ok(())
    }

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

    /// Compute human-readable differences between this desired profile and a current snapshot.
    pub fn diff_against(&self, current: &SmuStateSnapshot) -> Vec<ProfileDiffEntry> {
        let mut diffs = Vec::new();

        if let Some(co) = &self.curve_optimizer {
            let current_cores: BTreeMap<u32, i32> = current
                .co_per_core
                .as_deref()
                .unwrap_or(&[])
                .iter()
                .map(|&(idx, val)| (idx, i32::from(val)))
                .collect();

            if let Some(all_val) = co.all {
                let core_count = current.physical_cores as u32;
                let mut core_indices: Vec<u32> = (0..core_count).collect();
                for &idx in current_cores.keys().chain(co.per_core.keys()) {
                    if !core_indices.contains(&idx) {
                        core_indices.push(idx);
                    }
                }
                core_indices.sort_unstable();

                if core_indices.is_empty() {
                    diffs.push(ProfileDiffEntry {
                        field: "curve_optimizer.all".into(),
                        current: "unknown".into(),
                        desired: format_signed(all_val),
                    });
                } else {
                    for core_idx in core_indices {
                        let desired = co.per_core.get(&core_idx).copied().unwrap_or(all_val);
                        push_diff_signed(
                            &mut diffs,
                            format!("curve_optimizer.core.{core_idx}"),
                            current_cores.get(&core_idx).copied(),
                            desired,
                        );
                    }
                }
            } else {
                for (&core_idx, &desired) in &co.per_core {
                    push_diff_signed(
                        &mut diffs,
                        format!("curve_optimizer.core.{core_idx}"),
                        current_cores.get(&core_idx).copied(),
                        desired,
                    );
                }
            }

            if let Some(desired_igpu) = co.igpu {
                push_diff_signed(
                    &mut diffs,
                    "curve_optimizer.igpu".into(),
                    current.igpu_co.map(i32::from),
                    desired_igpu,
                );
            }
        }

        if let Some(shaper) = &self.curve_shaper {
            for (&tier, desired_tier) in shaper {
                let cur_tier = current.curve_shaper.map(|cs| cs.tier(tier));
                for (band, cur_val, des_val) in [
                    ("low", cur_tier.map(|m| i32::from(m.low)), desired_tier.low),
                    ("med", cur_tier.map(|m| i32::from(m.med)), desired_tier.med),
                    (
                        "high",
                        cur_tier.map(|m| i32::from(m.high)),
                        desired_tier.high,
                    ),
                ] {
                    if let Some(d) = des_val {
                        push_diff_signed(
                            &mut diffs,
                            format!("curve_shaper.{}.{band}", tier.label()),
                            cur_val,
                            i32::from(d),
                        );
                    }
                }
            }
        }

        if let Some(pbo) = &self.pbo {
            push_diff_u32(&mut diffs, "pbo.ppt_w", current.ppt_w, pbo.ppt_w, "W");
            push_diff_u32(&mut diffs, "pbo.tdc_a", current.tdc_a, pbo.tdc_a, "A");
            push_diff_u32(&mut diffs, "pbo.edc_a", current.edc_a, pbo.edc_a, "A");
            push_diff_u32(&mut diffs, "pbo.tctl_c", current.tctl_c, pbo.tctl_c, "°C");
            let cur_scalar = current
                .pbo_scalar
                .filter(|s| s.is_finite() && *s >= 0.0)
                .map(|s| s.round() as u32);
            push_diff_u32(&mut diffs, "pbo.scalar", cur_scalar, pbo.scalar, "x");
            push_diff_u32(
                &mut diffs,
                "pbo.fmax_mhz",
                current.fmax_mhz,
                pbo.fmax_mhz,
                "MHz",
            );
        }

        if let Some(apu) = &self.apu {
            push_diff_u32(&mut diffs, "apu.stapm_w", current.stapm_w, apu.stapm_w, "W");
            push_diff_u32(&mut diffs, "apu.stapm_time_s", None, apu.stapm_time_s, "s");
            push_diff_u32(&mut diffs, "apu.fast_w", current.ppt_w, apu.fast_w, "W");
            push_diff_u32(&mut diffs, "apu.slow_w", current.slow_w, apu.slow_w, "W");
            push_diff_u32(&mut diffs, "apu.slow_time_s", None, apu.slow_time_s, "s");
            push_diff_u32(
                &mut diffs,
                "apu.apu_slow_w",
                current.apu_slow_w,
                apu.apu_slow_w,
                "W",
            );
            push_diff_u32(
                &mut diffs,
                "apu.tdc_soc_a",
                current.tdc_soc_a,
                apu.tdc_soc_a,
                "A",
            );
            push_diff_u32(
                &mut diffs,
                "apu.edc_soc_a",
                current.edc_soc_a,
                apu.edc_soc_a,
                "A",
            );
            push_diff_u32(
                &mut diffs,
                "apu.apu_skin_temp_c",
                current.apu_skin_temp_c,
                apu.apu_skin_temp_c,
                "°C",
            );
            push_diff_u32(
                &mut diffs,
                "apu.dgpu_skin_temp_c",
                current.dgpu_skin_temp_c,
                apu.dgpu_skin_temp_c,
                "°C",
            );
            if let Some(mode) = apu.power_mode {
                let label = match mode {
                    ApuPowerMode::MaxPerformance => "max-performance",
                    ApuPowerMode::PowerSaving => "power-saving",
                };
                diffs.push(ProfileDiffEntry {
                    field: "apu.power_mode".into(),
                    current: "unknown".into(),
                    desired: label.into(),
                });
            }
        }

        diffs
    }
}

fn require_cap(supported: bool, capability: &'static str, codename: Codename) -> Result<()> {
    if !supported {
        return Err(Error::UnsupportedCapability {
            capability,
            codename: codename.to_string(),
        });
    }
    Ok(())
}

fn push_diff_signed(
    diffs: &mut Vec<ProfileDiffEntry>,
    field: String,
    current: Option<i32>,
    desired: i32,
) {
    if current != Some(desired) {
        diffs.push(ProfileDiffEntry {
            field,
            current: current
                .map(format_signed)
                .unwrap_or_else(|| "unknown".into()),
            desired: format_signed(desired),
        });
    }
}

fn push_diff_u32(
    diffs: &mut Vec<ProfileDiffEntry>,
    field: &str,
    current: Option<u32>,
    desired: Option<u32>,
    unit: &str,
) {
    if let Some(d) = desired {
        if current != Some(d) {
            diffs.push(ProfileDiffEntry {
                field: field.into(),
                current: fmt_opt_unit(current, unit),
                desired: format!("{d}{unit}"),
            });
        }
    }
}

fn format_signed(v: i32) -> String {
    if v > 0 {
        format!("+{v}")
    } else {
        v.to_string()
    }
}

fn fmt_opt_unit(val: Option<u32>, unit: &str) -> String {
    match val {
        Some(v) => format!("{v}{unit}"),
        None => "unknown".into(),
    }
}

/// Custom serde adapter for `BTreeMap<u32, i32>` so TOML table keys serialize as strings (`"0"`).
mod u32_map_serde {
    use serde::de::{self, MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};
    use std::collections::BTreeMap;
    use std::fmt;

    pub fn serialize<S>(
        map: &BTreeMap<u32, i32>,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut s = serializer.serialize_map(Some(map.len()))?;
        for (k, v) in map {
            s.serialize_entry(&k.to_string(), v)?;
        }
        s.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> std::result::Result<BTreeMap<u32, i32>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct U32MapVisitor;

        impl<'de> Visitor<'de> for U32MapVisitor {
            type Value = BTreeMap<u32, i32>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of non-negative integer core indices to CO margins")
            }

            fn visit_map<M>(self, mut access: M) -> std::result::Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut map = BTreeMap::new();
                while let Some((key, val)) = access.next_entry::<String, i32>()? {
                    let core_idx: u32 = key.parse().map_err(|_| {
                        de::Error::custom(format!(
                            "invalid core index key '{key}': expected non-negative integer"
                        ))
                    })?;
                    map.insert(core_idx, val);
                }
                Ok(map)
            }
        }

        deserializer.deserialize_map(U32MapVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CurveShaperState, ShaperTierMargins};

    fn sample_zen5_profile() -> Profile {
        let mut per_core = BTreeMap::new();
        per_core.insert(0, -25);
        per_core.insert(1, -20);

        let mut shaper = BTreeMap::new();
        shaper.insert(
            FreqTier::High,
            ShaperTierProfile {
                low: Some(-10),
                med: Some(-15),
                high: Some(-5),
            },
        );
        shaper.insert(
            FreqTier::Max,
            ShaperTierProfile {
                low: Some(-5),
                med: Some(-10),
                high: Some(0),
            },
        );

        Profile {
            schema: PROFILE_SCHEMA_VERSION,
            target: ProfileTarget {
                codename: Codename::GraniteRidge,
                physical_cores: Some(8),
            },
            curve_optimizer: Some(CoProfile {
                all: Some(-15),
                per_core,
                igpu: Some(-10),
            }),
            curve_shaper: Some(shaper),
            pbo: Some(PboProfile {
                ppt_w: Some(88),
                tdc_a: Some(75),
                edc_a: Some(150),
                tctl_c: Some(85),
                scalar: Some(1),
                fmax_mhz: Some(5550),
            }),
            apu: None,
        }
    }

    #[test]
    fn toml_roundtrip_and_disk_save_load() {
        let profile = sample_zen5_profile();
        let toml_str = profile.to_toml_string().unwrap();
        let parsed = Profile::from_toml_str(&toml_str).unwrap();
        assert_eq!(parsed, profile);

        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("profiles/9700x.toml");
        profile.save(&path).unwrap();
        let loaded = Profile::load(&path).unwrap();
        assert_eq!(loaded, profile);
    }

    #[test]
    fn parses_handwritten_toml_profile() {
        let raw = r#"
schema = 1

[target]
codename = "granite-ridge"
physical_cores = 8

[curve_optimizer]
all = -20
igpu = -10

[curve_optimizer.per_core]
0 = -25
1 = -15

[curve_shaper.high]
low = -10
med = -15
high = -5

[pbo]
ppt_w = 88
tdc_a = 75
edc_a = 150
tctl_c = 85
scalar = 2
fmax_mhz = 5550
"#;
        let profile = Profile::from_toml_str(raw).unwrap();
        assert_eq!(profile.target.codename, Codename::GraniteRidge);
        assert_eq!(
            profile.curve_optimizer.as_ref().unwrap().per_core.get(&0),
            Some(&-25)
        );
        assert!(profile
            .validate_for(Codename::GraniteRidge, 8, false)
            .is_ok());
    }

    #[test]
    fn validation_catches_target_capability_core_and_bounds_errors() {
        let mut p = sample_zen5_profile();
        assert!(p.validate_for(Codename::GraniteRidge, 8, false).is_ok());

        // Codename mismatch
        assert!(matches!(
            p.validate_for(Codename::Raphael, 8, false),
            Err(Error::ProfileTargetMismatch { .. })
        ));

        // Core count mismatch
        assert!(matches!(
            p.validate_for(Codename::GraniteRidge, 12, false),
            Err(Error::ProfileTargetMismatch { .. })
        ));

        // Schema mismatch
        p.schema = 2;
        assert!(matches!(
            p.validate_for(Codename::GraniteRidge, 8, false),
            Err(Error::InvalidInput(_))
        ));
        p.schema = 1;

        // Curve shaper unsupported on Raphael (Zen 4)
        let mut raphael = sample_zen5_profile();
        raphael.target.codename = Codename::Raphael;
        assert!(matches!(
            raphael.validate_for(Codename::Raphael, 8, false),
            Err(Error::UnsupportedCapability {
                capability: "curve_shaper",
                ..
            })
        ));

        // Out-of-bounds core index
        let mut bad_core = sample_zen5_profile();
        bad_core
            .curve_optimizer
            .as_mut()
            .unwrap()
            .per_core
            .insert(8, -10);
        assert!(matches!(
            bad_core.validate_for(Codename::GraniteRidge, 8, false),
            Err(Error::OutOfBounds { .. })
        ));

        // Extreme CO margin requires allow_extreme
        let mut extreme = sample_zen5_profile();
        extreme.curve_optimizer.as_mut().unwrap().all = Some(-45);
        assert!(matches!(
            extreme.validate_for(Codename::GraniteRidge, 8, false),
            Err(Error::ExtremeValueBlocked { .. })
        ));
        assert!(extreme
            .validate_for(Codename::GraniteRidge, 8, true)
            .is_ok());
    }

    #[test]
    fn from_snapshot_roundtrip_has_empty_diff() {
        let mut snap = SmuStateSnapshot::new(Codename::GraniteRidge, 4);
        snap.co_per_core = Some(vec![(0, -25), (1, -20), (2, -20), (3, -15)]);
        snap.igpu_co = Some(-10);
        snap.curve_shaper = Some(CurveShaperState {
            high: ShaperTierMargins {
                low: -10,
                med: -15,
                high: -5,
            },
            ..CurveShaperState::default()
        });
        snap.pbo_scalar = Some(1.0);
        snap.fmax_mhz = Some(5550);
        snap.ppt_w = Some(88);
        snap.tdc_a = Some(75);
        snap.edc_a = Some(150);
        snap.tctl_c = Some(85);

        let profile = Profile::from_snapshot(&snap);
        assert!(profile
            .validate_for(Codename::GraniteRidge, 4, false)
            .is_ok());
        assert!(profile.diff_against(&snap).is_empty());
    }

    #[test]
    fn diff_against_detects_changed_fields() {
        let mut snap = SmuStateSnapshot::new(Codename::GraniteRidge, 4);
        snap.co_per_core = Some(vec![(0, -15), (1, -15), (2, -15), (3, -15)]);
        snap.igpu_co = Some(0);
        snap.ppt_w = Some(88);
        snap.tctl_c = Some(95);

        let mut per_core = BTreeMap::new();
        per_core.insert(0, -25);
        let profile = Profile {
            schema: PROFILE_SCHEMA_VERSION,
            target: ProfileTarget {
                codename: Codename::GraniteRidge,
                physical_cores: Some(4),
            },
            curve_optimizer: Some(CoProfile {
                all: Some(-15),
                per_core,
                igpu: Some(-10),
            }),
            curve_shaper: None,
            pbo: Some(PboProfile {
                ppt_w: Some(88),
                tctl_c: Some(85),
                ..PboProfile::default()
            }),
            apu: None,
        };

        let diffs = profile.diff_against(&snap);
        assert_eq!(
            diffs,
            vec![
                ProfileDiffEntry {
                    field: "curve_optimizer.core.0".into(),
                    current: "-15".into(),
                    desired: "-25".into(),
                },
                ProfileDiffEntry {
                    field: "curve_optimizer.igpu".into(),
                    current: "0".into(),
                    desired: "-10".into(),
                },
                ProfileDiffEntry {
                    field: "pbo.tctl_c".into(),
                    current: "95°C".into(),
                    desired: "85°C".into(),
                },
            ]
        );
    }

    #[test]
    fn apu_profile_validates_on_phoenix_and_rejects_on_desktop() {
        let apu_profile = Profile {
            schema: PROFILE_SCHEMA_VERSION,
            target: ProfileTarget {
                codename: Codename::Phoenix,
                physical_cores: Some(8),
            },
            curve_optimizer: None,
            curve_shaper: None,
            pbo: None,
            apu: Some(ApuProfile {
                stapm_w: Some(35),
                stapm_time_s: Some(120),
                fast_w: Some(54),
                slow_w: Some(45),
                slow_time_s: Some(10),
                apu_slow_w: Some(35),
                tdc_soc_a: Some(20),
                edc_soc_a: Some(35),
                apu_skin_temp_c: Some(80),
                dgpu_skin_temp_c: Some(75),
                power_mode: Some(ApuPowerMode::MaxPerformance),
            }),
        };

        assert!(apu_profile
            .validate_for(Codename::Phoenix, 8, false)
            .is_ok());

        let mut desktop_apu = apu_profile.clone();
        desktop_apu.target.codename = Codename::GraniteRidge;
        assert!(matches!(
            desktop_apu.validate_for(Codename::GraniteRidge, 8, false),
            Err(Error::UnsupportedCapability { .. })
        ));
    }

    #[test]
    fn from_snapshot_on_lucienne_filters_unwritable_fmax_and_captures_all_apu_limits() {
        let mut snap = SmuStateSnapshot::new(Codename::Lucienne, 8);
        snap.pbo_scalar = Some(1.0);
        snap.fmax_mhz = Some(4350); // readable via get_fmax on Lucienne, NOT writable!
        snap.ppt_w = Some(30);
        snap.tdc_a = Some(33);
        snap.edc_a = Some(70);
        snap.tctl_c = Some(80);
        snap.stapm_w = Some(20);
        snap.slow_w = Some(25);
        snap.apu_slow_w = Some(25);
        snap.tdc_soc_a = Some(13);
        snap.edc_soc_a = Some(17);
        snap.apu_skin_temp_c = Some(80);
        snap.dgpu_skin_temp_c = Some(80);

        let profile = Profile::from_snapshot(&snap);
        let pbo = profile.pbo.as_ref().expect("pbo section populated");
        assert_eq!(pbo.fmax_mhz, None, "read-only fmax must not be emitted");
        assert_eq!(pbo.ppt_w, Some(30));
        assert_eq!(pbo.tdc_a, Some(33));
        assert_eq!(pbo.edc_a, Some(70));
        assert_eq!(pbo.tctl_c, Some(80));
        assert_eq!(pbo.scalar, Some(1));

        let apu = profile.apu.as_ref().expect("apu section populated");
        assert_eq!(apu.stapm_w, Some(20));
        assert_eq!(apu.slow_w, Some(25));
        assert_eq!(apu.apu_slow_w, Some(25));
        assert_eq!(apu.tdc_soc_a, Some(13));
        assert_eq!(apu.edc_soc_a, Some(17));
        assert_eq!(apu.apu_skin_temp_c, Some(80));
        assert_eq!(apu.dgpu_skin_temp_c, Some(80));

        // Validates cleanly with allow_extreme=true (as persist/boot service does) and diffs empty
        assert!(profile.validate_for(Codename::Lucienne, 8, true).is_ok());
        assert!(profile.diff_against(&snap).is_empty());
    }

    #[test]
    fn from_snapshot_on_vermeer_filters_unwritable_fmax() {
        let mut snap = SmuStateSnapshot::new(Codename::Vermeer, 16);
        snap.co_per_core = Some((0..16).map(|i| (i, -10)).collect());
        snap.pbo_scalar = Some(1.0);
        snap.fmax_mhz = Some(5150); // read-only on Vermeer
        snap.ppt_w = Some(1000);
        snap.tdc_a = Some(140);
        snap.edc_a = Some(220);
        snap.tctl_c = Some(75);

        let profile = Profile::from_snapshot(&snap);
        assert_eq!(profile.pbo.as_ref().unwrap().fmax_mhz, None);
        assert!(profile.apu.is_none());
        assert!(profile.validate_for(Codename::Vermeer, 16, true).is_ok());
        assert!(profile.diff_against(&snap).is_empty());
    }

    #[test]
    fn from_snapshot_filters_zero_nan_empty_and_unsupported_fields() {
        let mut snap = SmuStateSnapshot::new(Codename::GraniteRidge, 8);
        snap.co_per_core = Some(vec![]);
        snap.pbo_scalar = Some(f32::NAN);
        snap.ppt_w = Some(0);
        snap.fmax_mhz = Some(0);

        let profile = Profile::from_snapshot(&snap);
        assert!(profile.curve_optimizer.is_none());
        assert!(profile.pbo.is_none());
        assert!(profile
            .validate_for(Codename::GraniteRidge, 8, false)
            .is_ok());

        // Non-finite or out-of-range scalar values are dropped cleanly
        snap.pbo_scalar = Some(99.0);
        let p2 = Profile::from_snapshot(&snap);
        assert!(p2.pbo.is_none());

        // Unsupported codename emits no writable sections
        let mut unknown = SmuStateSnapshot::new(Codename::Undefined, 4);
        unknown.ppt_w = Some(88);
        let p3 = Profile::from_snapshot(&unknown);
        assert!(p3.pbo.is_none());
        assert!(p3.apu.is_none());
    }

    #[test]
    fn validate_for_matches_live_op_bounds_on_edge_cases() {
        // Hard Tctl ceiling 95°C cannot be bypassed with allow_extreme
        let mut p = sample_zen5_profile();
        p.pbo.as_mut().unwrap().tctl_c = Some(96);
        assert!(matches!(
            p.validate_for(Codename::GraniteRidge, 8, true),
            Err(Error::OutOfBounds { .. })
        ));

        // iGPU CO enforces -30..=30 (soft -25), even on Zen 5 where CPU CO is -50..=50
        let mut p = sample_zen5_profile();
        p.curve_optimizer.as_mut().unwrap().igpu = Some(-28);
        assert!(matches!(
            p.validate_for(Codename::GraniteRidge, 8, false),
            Err(Error::ExtremeValueBlocked { .. })
        ));
        p.curve_optimizer.as_mut().unwrap().igpu = Some(-35);
        assert!(matches!(
            p.validate_for(Codename::GraniteRidge, 8, true),
            Err(Error::OutOfBounds { .. })
        ));

        // PBO scalar > 4x requires allow_extreme
        let mut p = sample_zen5_profile();
        p.pbo.as_mut().unwrap().scalar = Some(6);
        assert!(matches!(
            p.validate_for(Codename::GraniteRidge, 8, false),
            Err(Error::ExtremeValueBlocked { .. })
        ));
        assert!(p.validate_for(Codename::GraniteRidge, 8, true).is_ok());

        // Curve Shaper hard max is +30 (not +50)
        let mut p = sample_zen5_profile();
        p.curve_shaper
            .as_mut()
            .unwrap()
            .get_mut(&FreqTier::High)
            .unwrap()
            .high = Some(31);
        assert!(matches!(
            p.validate_for(Codename::GraniteRidge, 8, true),
            Err(Error::OutOfBounds { .. })
        ));
    }
}
