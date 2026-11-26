//! Declarative TOML tuning profiles with target validation and state diffing.

use crate::error::{Error, Result};
use crate::hw::profiles::capabilities_for;
use crate::safety::bounds::{
    validate_apu_watts, validate_co_margin, validate_edc_amps, validate_fmax_mhz,
    validate_pbo_scalar, validate_ppt_watts, validate_shaper_margin, validate_skin_temp_celsius,
    validate_tctl_celsius, validate_tdc_amps,
};
use crate::safety::snapshot::SmuStateSnapshot;
use crate::types::{ApuPowerMode, Codename, FreqTier};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

/// Current profile TOML schema version.
pub const PROFILE_SCHEMA_VERSION: u32 = 1;

static PROFILE_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

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

/// APU-specific DPTC power and skin-temperature section.
#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq)]
pub struct ApuProfile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stapm_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slow_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apu_slow_w: Option<u32>,
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

    /// Atomically save this profile to disk as TOML (write temp + sync + rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        let content = self.to_toml_string()?;
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|e| map_io_err(parent, e))?;

        let seq = PROFILE_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("profile.toml");
        let tmp_path = parent.join(format!(".{file_name}.{pid}.{seq}.tmp"));

        let mut file = File::create(&tmp_path).map_err(|e| map_io_err(&tmp_path, e))?;
        if let Err(e) = file
            .write_all(content.as_bytes())
            .and_then(|()| file.sync_all())
        {
            let _ = fs::remove_file(&tmp_path);
            return Err(map_io_err(&tmp_path, e));
        }
        drop(file);

        fs::rename(&tmp_path, path).map_err(|e| {
            let _ = fs::remove_file(&tmp_path);
            map_io_err(path, e)
        })?;
        Ok(())
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
                    if caps.set_co_all_cores.is_none() && caps.set_co_per_core.is_none() {
                        return Err(Error::UnsupportedCapability {
                            capability: "curve_optimizer",
                            codename: codename.to_string(),
                        });
                    }
                    validate_co_margin("curve_optimizer.all", all_margin, bounds, allow_extreme)?;
                }

                if !co.per_core.is_empty() && caps.set_co_per_core.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "curve_optimizer_per_core",
                        codename: codename.to_string(),
                    });
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
                    if caps.set_igpu_co.is_none() {
                        return Err(Error::UnsupportedCapability {
                            capability: "igpu_co",
                            codename: codename.to_string(),
                        });
                    }
                    validate_co_margin("curve_optimizer.igpu", igpu_margin, bounds, allow_extreme)?;
                }
            }
        }

        if let Some(shaper) = &self.curve_shaper {
            let has_any_shaper = shaper
                .values()
                .any(|t| t.low.is_some() || t.med.is_some() || t.high.is_some());
            if has_any_shaper {
                if caps.set_curve_shaper.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "curve_shaper",
                        codename: codename.to_string(),
                    });
                }
                for (&tier, margins) in shaper {
                    if let Some(v) = margins.low {
                        validate_shaper_margin(
                            &format!("curve_shaper.{}.low", tier.label()),
                            i32::from(v),
                            allow_extreme,
                        )?;
                    }
                    if let Some(v) = margins.med {
                        validate_shaper_margin(
                            &format!("curve_shaper.{}.med", tier.label()),
                            i32::from(v),
                            allow_extreme,
                        )?;
                    }
                    if let Some(v) = margins.high {
                        validate_shaper_margin(
                            &format!("curve_shaper.{}.high", tier.label()),
                            i32::from(v),
                            allow_extreme,
                        )?;
                    }
                }
            }
        }

        if let Some(pbo) = &self.pbo {
            if let Some(ppt) = pbo.ppt_w {
                if caps.set_ppt.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "pbo_ppt",
                        codename: codename.to_string(),
                    });
                }
                validate_ppt_watts(ppt, allow_extreme)?;
            }
            if let Some(tdc) = pbo.tdc_a {
                if caps.set_tdc.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "pbo_tdc",
                        codename: codename.to_string(),
                    });
                }
                validate_tdc_amps(tdc, allow_extreme)?;
            }
            if let Some(edc) = pbo.edc_a {
                if caps.set_edc.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "pbo_edc",
                        codename: codename.to_string(),
                    });
                }
                validate_edc_amps(edc, allow_extreme)?;
            }
            if let Some(tctl) = pbo.tctl_c {
                if caps.set_tctl.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "pbo_tctl",
                        codename: codename.to_string(),
                    });
                }
                validate_tctl_celsius(tctl, allow_extreme)?;
            }
            if let Some(scalar) = pbo.scalar {
                if caps.set_pbo_scalar.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "pbo_scalar",
                        codename: codename.to_string(),
                    });
                }
                validate_pbo_scalar(scalar)?;
            }
            if let Some(fmax) = pbo.fmax_mhz {
                if caps.set_fmax.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "fmax_boost_limit",
                        codename: codename.to_string(),
                    });
                }
                validate_fmax_mhz(fmax, allow_extreme)?;
            }
        }

        if let Some(apu) = &self.apu {
            if let Some(w) = apu.stapm_w {
                if caps.set_stapm_limit.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "apu_stapm",
                        codename: codename.to_string(),
                    });
                }
                validate_apu_watts("apu.stapm_w", w, allow_extreme)?;
            }
            if let Some(w) = apu.fast_w {
                if !codename.is_apu() || caps.set_ppt.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "apu_fast_limit",
                        codename: codename.to_string(),
                    });
                }
                validate_apu_watts("apu.fast_w", w, allow_extreme)?;
            }
            if let Some(w) = apu.slow_w {
                if !codename.is_apu() || caps.set_slow_limit.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "apu_slow_limit",
                        codename: codename.to_string(),
                    });
                }
                validate_apu_watts("apu.slow_w", w, allow_extreme)?;
            }
            if let Some(w) = apu.apu_slow_w {
                if caps.set_apu_slow_limit.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "apu_slow_limit",
                        codename: codename.to_string(),
                    });
                }
                validate_apu_watts("apu.apu_slow_w", w, allow_extreme)?;
            }
            if let Some(c) = apu.apu_skin_temp_c {
                if caps.set_apu_skin_temp.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "apu_skin_temp",
                        codename: codename.to_string(),
                    });
                }
                validate_skin_temp_celsius("apu.apu_skin_temp_c", c, allow_extreme)?;
            }
            if let Some(c) = apu.dgpu_skin_temp_c {
                if caps.set_dgpu_skin_temp.is_none() {
                    return Err(Error::UnsupportedCapability {
                        capability: "dgpu_skin_temp",
                        codename: codename.to_string(),
                    });
                }
                validate_skin_temp_celsius("apu.dgpu_skin_temp_c", c, allow_extreme)?;
            }
            if apu.power_mode.is_some()
                && (caps.set_max_performance.is_none() || caps.set_power_saving.is_none())
            {
                return Err(Error::UnsupportedCapability {
                    capability: "apu_power_mode",
                    codename: codename.to_string(),
                });
            }
        }

        Ok(())
    }

    /// Construct a declarative profile from a captured hardware [`SmuStateSnapshot`].
    pub fn from_snapshot(snap: &SmuStateSnapshot) -> Self {
        let curve_optimizer = if snap.co_per_core.is_some() || snap.igpu_co.is_some() {
            let mut per_core = BTreeMap::new();
            if let Some(cores) = &snap.co_per_core {
                for &(core, margin) in cores {
                    per_core.insert(core, i32::from(margin));
                }
            }
            Some(CoProfile {
                all: None,
                per_core,
                igpu: snap.igpu_co.map(i32::from),
            })
        } else {
            None
        };

        let curve_shaper = snap.curve_shaper.map(|cs| {
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
        });

        let scalar = snap.pbo_scalar.and_then(|s| {
            if s.is_finite() && s >= 1.0 {
                Some(s.round() as u32)
            } else {
                None
            }
        });
        let pbo = if snap.ppt_w.is_some()
            || snap.tdc_a.is_some()
            || snap.edc_a.is_some()
            || snap.tctl_c.is_some()
            || scalar.is_some()
            || snap.fmax_mhz.is_some()
        {
            Some(PboProfile {
                ppt_w: snap.ppt_w,
                tdc_a: snap.tdc_a,
                edc_a: snap.edc_a,
                tctl_c: snap.tctl_c,
                scalar,
                fmax_mhz: snap.fmax_mhz,
            })
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
            apu: None,
        }
    }

    /// Compute human-readable differences between this desired profile and a current snapshot.
    pub fn diff_against(&self, current: &SmuStateSnapshot) -> Vec<ProfileDiffEntry> {
        let mut diffs = Vec::new();

        if let Some(co) = &self.curve_optimizer {
            let mut current_cores: BTreeMap<u32, i32> = BTreeMap::new();
            if let Some(cores) = &current.co_per_core {
                for &(idx, val) in cores {
                    current_cores.insert(idx, i32::from(val));
                }
            }

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
                        let cur = current_cores.get(&core_idx).copied();
                        if cur != Some(desired) {
                            diffs.push(ProfileDiffEntry {
                                field: format!("curve_optimizer.core.{core_idx}"),
                                current: cur.map(format_signed).unwrap_or_else(|| "unknown".into()),
                                desired: format_signed(desired),
                            });
                        }
                    }
                }
            } else {
                for (&core_idx, &desired) in &co.per_core {
                    let cur = current_cores.get(&core_idx).copied();
                    if cur != Some(desired) {
                        diffs.push(ProfileDiffEntry {
                            field: format!("curve_optimizer.core.{core_idx}"),
                            current: cur.map(format_signed).unwrap_or_else(|| "unknown".into()),
                            desired: format_signed(desired),
                        });
                    }
                }
            }

            if let Some(desired_igpu) = co.igpu {
                let cur_igpu = current.igpu_co.map(i32::from);
                if cur_igpu != Some(desired_igpu) {
                    diffs.push(ProfileDiffEntry {
                        field: "curve_optimizer.igpu".into(),
                        current: cur_igpu
                            .map(format_signed)
                            .unwrap_or_else(|| "unknown".into()),
                        desired: format_signed(desired_igpu),
                    });
                }
            }
        }

        if let Some(shaper) = &self.curve_shaper {
            for (&tier, desired_tier) in shaper {
                let cur_tier = current.curve_shaper.map(|cs| cs.tier(tier));
                if let Some(d) = desired_tier.low {
                    let cur = cur_tier.map(|m| m.low);
                    if cur != Some(d) {
                        diffs.push(ProfileDiffEntry {
                            field: format!("curve_shaper.{}.low", tier.label()),
                            current: cur
                                .map(|v| format_signed(i32::from(v)))
                                .unwrap_or_else(|| "unknown".into()),
                            desired: format_signed(i32::from(d)),
                        });
                    }
                }
                if let Some(d) = desired_tier.med {
                    let cur = cur_tier.map(|m| m.med);
                    if cur != Some(d) {
                        diffs.push(ProfileDiffEntry {
                            field: format!("curve_shaper.{}.med", tier.label()),
                            current: cur
                                .map(|v| format_signed(i32::from(v)))
                                .unwrap_or_else(|| "unknown".into()),
                            desired: format_signed(i32::from(d)),
                        });
                    }
                }
                if let Some(d) = desired_tier.high {
                    let cur = cur_tier.map(|m| m.high);
                    if cur != Some(d) {
                        diffs.push(ProfileDiffEntry {
                            field: format!("curve_shaper.{}.high", tier.label()),
                            current: cur
                                .map(|v| format_signed(i32::from(v)))
                                .unwrap_or_else(|| "unknown".into()),
                            desired: format_signed(i32::from(d)),
                        });
                    }
                }
            }
        }

        if let Some(pbo) = &self.pbo {
            if let Some(d) = pbo.ppt_w {
                if current.ppt_w != Some(d) {
                    diffs.push(ProfileDiffEntry {
                        field: "pbo.ppt_w".into(),
                        current: fmt_opt_unit(current.ppt_w, "W"),
                        desired: format!("{d}W"),
                    });
                }
            }
            if let Some(d) = pbo.tdc_a {
                if current.tdc_a != Some(d) {
                    diffs.push(ProfileDiffEntry {
                        field: "pbo.tdc_a".into(),
                        current: fmt_opt_unit(current.tdc_a, "A"),
                        desired: format!("{d}A"),
                    });
                }
            }
            if let Some(d) = pbo.edc_a {
                if current.edc_a != Some(d) {
                    diffs.push(ProfileDiffEntry {
                        field: "pbo.edc_a".into(),
                        current: fmt_opt_unit(current.edc_a, "A"),
                        desired: format!("{d}A"),
                    });
                }
            }
            if let Some(d) = pbo.tctl_c {
                if current.tctl_c != Some(d) {
                    diffs.push(ProfileDiffEntry {
                        field: "pbo.tctl_c".into(),
                        current: fmt_opt_unit(current.tctl_c, "°C"),
                        desired: format!("{d}°C"),
                    });
                }
            }
            if let Some(d) = pbo.scalar {
                let cur = current
                    .pbo_scalar
                    .filter(|s| s.is_finite() && *s >= 0.0)
                    .map(|s| s.round() as u32);
                if cur != Some(d) {
                    diffs.push(ProfileDiffEntry {
                        field: "pbo.scalar".into(),
                        current: fmt_opt_unit(cur, "x"),
                        desired: format!("{d}x"),
                    });
                }
            }
            if let Some(d) = pbo.fmax_mhz {
                if current.fmax_mhz != Some(d) {
                    diffs.push(ProfileDiffEntry {
                        field: "pbo.fmax_mhz".into(),
                        current: fmt_opt_unit(current.fmax_mhz, "MHz"),
                        desired: format!("{d}MHz"),
                    });
                }
            }
        }

        if let Some(apu) = &self.apu {
            if let Some(w) = apu.stapm_w {
                diffs.push(ProfileDiffEntry {
                    field: "apu.stapm_w".into(),
                    current: "unknown".into(),
                    desired: format!("{w}W"),
                });
            }
            if let Some(w) = apu.fast_w {
                diffs.push(ProfileDiffEntry {
                    field: "apu.fast_w".into(),
                    current: "unknown".into(),
                    desired: format!("{w}W"),
                });
            }
            if let Some(w) = apu.slow_w {
                diffs.push(ProfileDiffEntry {
                    field: "apu.slow_w".into(),
                    current: "unknown".into(),
                    desired: format!("{w}W"),
                });
            }
            if let Some(w) = apu.apu_slow_w {
                diffs.push(ProfileDiffEntry {
                    field: "apu.apu_slow_w".into(),
                    current: "unknown".into(),
                    desired: format!("{w}W"),
                });
            }
            if let Some(c) = apu.apu_skin_temp_c {
                diffs.push(ProfileDiffEntry {
                    field: "apu.apu_skin_temp_c".into(),
                    current: "unknown".into(),
                    desired: format!("{c}°C"),
                });
            }
            if let Some(c) = apu.dgpu_skin_temp_c {
                diffs.push(ProfileDiffEntry {
                    field: "apu.dgpu_skin_temp_c".into(),
                    current: "unknown".into(),
                    desired: format!("{c}°C"),
                });
            }
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

fn map_io_err(path: &Path, source: std::io::Error) -> Error {
    if source.kind() == ErrorKind::PermissionDenied {
        Error::PermissionDenied {
            path: path.display().to_string(),
        }
    } else {
        Error::Io {
            path: path.display().to_string(),
            source,
        }
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

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> std::result::Result<BTreeMap<u32, i32>, D::Error>
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
        assert!(profile.validate_for(Codename::GraniteRidge, 8, false).is_ok());
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
        bad_core.curve_optimizer.as_mut().unwrap().per_core.insert(8, -10);
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
        assert!(extreme.validate_for(Codename::GraniteRidge, 8, true).is_ok());
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
        assert!(profile.validate_for(Codename::GraniteRidge, 4, false).is_ok());
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
                fast_w: Some(54),
                slow_w: Some(45),
                apu_slow_w: Some(35),
                apu_skin_temp_c: Some(80),
                dgpu_skin_temp_c: Some(75),
                power_mode: Some(ApuPowerMode::MaxPerformance),
            }),
        };

        assert!(apu_profile.validate_for(Codename::Phoenix, 8, false).is_ok());

        let mut desktop_apu = apu_profile.clone();
        desktop_apu.target.codename = Codename::GraniteRidge;
        assert!(matches!(
            desktop_apu.validate_for(Codename::GraniteRidge, 8, false),
            Err(Error::UnsupportedCapability { .. })
        ));
    }
}
