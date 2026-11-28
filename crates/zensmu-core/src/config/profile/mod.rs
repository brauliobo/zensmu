//! Declarative TOML tuning profiles with target validation and state diffing.

mod codec;
mod diff;
mod snapshot;
#[cfg(test)]
mod tests;
mod validate;

use crate::error::{Error, Result};
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty", with = "codec")]
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
}
