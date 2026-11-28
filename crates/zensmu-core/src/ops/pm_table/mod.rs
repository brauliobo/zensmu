//! Strictly version-gated PM table decoders for unprivileged SMU telemetry.
//!
//! Unknown PM table versions and tables exceeding the safe sysfs `PAGE_SIZE`
//! limit (4096 bytes) are explicitly refused rather than guessed.

use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
use crate::safety::map_io_err;
use crate::transport::SmuTransport;
use crate::types::Codename;
use serde::{Deserialize, Serialize};
use std::path::Path;

mod layouts;
#[cfg(test)]
mod tests;

use layouts::layout_for;

/// Safe upper bound on PM table sysfs reads (`PAGE_SIZE` on x86_64 Linux).
pub const MAX_SAFE_PM_TABLE_SIZE: usize = 4096;

/// Decoded telemetry snapshot from a verified PM table layout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TelemetrySnapshot {
    pub codename: Codename,
    pub table_version: u32,
    pub table_size: usize,
    pub ppt_limit_w: Option<f32>,
    pub ppt_value_w: Option<f32>,
    pub tdc_limit_a: Option<f32>,
    pub tdc_value_a: Option<f32>,
    pub edc_limit_a: Option<f32>,
    pub edc_value_a: Option<f32>,
    pub tctl_limit_c: Option<f32>,
    pub tctl_value_c: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stapm_limit_w: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stapm_value_w: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slow_limit_w: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slow_value_w: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apu_slow_limit_w: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apu_slow_value_w: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tdc_soc_limit_a: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tdc_soc_value_a: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edc_soc_limit_a: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edc_soc_value_a: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apu_skin_temp_limit_c: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apu_skin_temp_value_c: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dgpu_skin_temp_limit_c: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dgpu_skin_temp_value_c: Option<f32>,
    pub fclk_mhz: Option<f32>,
    pub uclk_mhz: Option<f32>,
    pub mclk_mhz: Option<f32>,
    pub vddcr_soc_v: Option<f32>,
    pub vddcr_misc_v: Option<f32>,
    pub cldo_vddp_v: Option<f32>,
    pub cldo_vddg_iod_v: Option<f32>,
    pub cldo_vddg_ccd_v: Option<f32>,
}

fn read_f32_sane(raw: &[u8], offset: Option<usize>, min: f32, max: f32) -> Option<f32> {
    let off = offset?;
    let end = off.checked_add(4)?;
    let bytes: [u8; 4] = raw.get(off..end)?.try_into().ok()?;
    let val = f32::from_le_bytes(bytes);
    if val.is_finite() && val >= min && val <= max {
        Some(val)
    } else {
        None
    }
}

/// Decode a raw PM table buffer using the strict `(codename, version)` allowlist.
///
/// Refuses unknown versions with [`Error::UnsupportedPmTable`] and buffers
/// larger than 4096 bytes with [`Error::PmTableTooLarge`]. Non-finite or
/// physically absurd floats are filtered to `None`.
pub fn decode_pm_table(codename: Codename, version: u32, raw: &[u8]) -> Result<TelemetrySnapshot> {
    if raw.len() > MAX_SAFE_PM_TABLE_SIZE {
        return Err(Error::PmTableTooLarge { size: raw.len() });
    }

    let layout = layout_for(codename, version).ok_or_else(|| Error::UnsupportedPmTable {
        codename: codename.display_name().to_string(),
        version,
        size: raw.len(),
    })?;

    if raw.len() < layout.min_size {
        return Err(Error::UnsupportedPmTable {
            codename: codename.display_name().to_string(),
            version,
            size: raw.len(),
        });
    }

    Ok(TelemetrySnapshot {
        codename,
        table_version: version,
        table_size: raw.len(),
        ppt_limit_w: read_f32_sane(raw, layout.ppt_limit_w, 0.0, 2000.0),
        ppt_value_w: read_f32_sane(raw, layout.ppt_value_w, 0.0, 2000.0),
        tdc_limit_a: read_f32_sane(raw, layout.tdc_limit_a, 0.0, 2000.0),
        tdc_value_a: read_f32_sane(raw, layout.tdc_value_a, 0.0, 2000.0),
        edc_limit_a: read_f32_sane(raw, layout.edc_limit_a, 0.0, 2000.0),
        edc_value_a: read_f32_sane(raw, layout.edc_value_a, 0.0, 2000.0),
        tctl_limit_c: read_f32_sane(raw, layout.tctl_limit_c, -40.0, 200.0),
        tctl_value_c: read_f32_sane(raw, layout.tctl_value_c, -40.0, 200.0),
        stapm_limit_w: read_f32_sane(raw, layout.stapm_limit_w, 0.0, 2000.0),
        stapm_value_w: read_f32_sane(raw, layout.stapm_value_w, 0.0, 2000.0),
        slow_limit_w: read_f32_sane(raw, layout.slow_limit_w, 0.0, 2000.0),
        slow_value_w: read_f32_sane(raw, layout.slow_value_w, 0.0, 2000.0),
        apu_slow_limit_w: read_f32_sane(raw, layout.apu_slow_limit_w, 0.0, 2000.0),
        apu_slow_value_w: read_f32_sane(raw, layout.apu_slow_value_w, 0.0, 2000.0),
        tdc_soc_limit_a: read_f32_sane(raw, layout.tdc_soc_limit_a, 0.0, 2000.0),
        tdc_soc_value_a: read_f32_sane(raw, layout.tdc_soc_value_a, 0.0, 2000.0),
        edc_soc_limit_a: read_f32_sane(raw, layout.edc_soc_limit_a, 0.0, 2000.0),
        edc_soc_value_a: read_f32_sane(raw, layout.edc_soc_value_a, 0.0, 2000.0),
        apu_skin_temp_limit_c: read_f32_sane(raw, layout.apu_skin_temp_limit_c, -40.0, 200.0),
        apu_skin_temp_value_c: read_f32_sane(raw, layout.apu_skin_temp_value_c, -40.0, 200.0),
        dgpu_skin_temp_limit_c: read_f32_sane(raw, layout.dgpu_skin_temp_limit_c, -40.0, 200.0),
        dgpu_skin_temp_value_c: read_f32_sane(raw, layout.dgpu_skin_temp_value_c, -40.0, 200.0),
        fclk_mhz: read_f32_sane(raw, layout.fclk_mhz, 100.0, 10_000.0),
        uclk_mhz: read_f32_sane(raw, layout.uclk_mhz, 100.0, 10_000.0),
        mclk_mhz: read_f32_sane(raw, layout.mclk_mhz, 100.0, 10_000.0),
        vddcr_soc_v: read_f32_sane(raw, layout.vddcr_soc_v, 0.05, 3.0),
        vddcr_misc_v: read_f32_sane(raw, layout.vddcr_misc_v, 0.05, 3.0),
        cldo_vddp_v: read_f32_sane(raw, layout.cldo_vddp_v, 0.05, 3.0),
        cldo_vddg_iod_v: read_f32_sane(raw, layout.cldo_vddg_iod_v, 0.05, 3.0),
        cldo_vddg_ccd_v: read_f32_sane(raw, layout.cldo_vddg_ccd_v, 0.05, 3.0),
    })
}

/// Read and decode the unprivileged PM table from `transport`.
pub fn read_telemetry(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
) -> Result<TelemetrySnapshot> {
    if !hw.driver.has_pm_table {
        return Err(Error::UnsupportedCapability {
            capability: "pm_table",
            codename: hw.codename.display_name().to_string(),
        });
    }
    let version = hw
        .driver
        .pm_table_version
        .ok_or_else(|| Error::UnsupportedCapability {
            capability: "pm_table_version",
            codename: hw.codename.display_name().to_string(),
        })?;

    if let Some(sz) = hw.driver.pm_table_size {
        if sz > MAX_SAFE_PM_TABLE_SIZE {
            return Err(Error::PmTableTooLarge { size: sz });
        }
    }

    let raw = transport.read_pm_table()?;
    decode_pm_table(hw.codename, version, &raw)
}

/// Dump raw PM table bytes to `out_path` (for offline reverse-engineering / bug reports).
///
/// Refuses tables exceeding [`MAX_SAFE_PM_TABLE_SIZE`] (4096 bytes) before writing.
pub fn dump_raw_pm_table(transport: &dyn SmuTransport, out_path: &Path) -> Result<usize> {
    let info = transport.read_info()?;
    if let Some(sz) = info.pm_table_size {
        if sz > MAX_SAFE_PM_TABLE_SIZE {
            return Err(Error::PmTableTooLarge { size: sz });
        }
    }
    let raw = transport.read_pm_table()?;
    if raw.len() > MAX_SAFE_PM_TABLE_SIZE {
        return Err(Error::PmTableTooLarge { size: raw.len() });
    }
    std::fs::write(out_path, &raw).map_err(|source| map_io_err(out_path, source))?;
    Ok(raw.len())
}
