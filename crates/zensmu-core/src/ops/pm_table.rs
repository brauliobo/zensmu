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

/// Verified byte offsets for a specific `(Codename, pm_table_version)` pair
/// (cross-referenced against `ZenStates-Core/Hardware/PowerTable.cs`).
#[derive(Debug, Clone, Copy, Default)]
struct PmTableLayout {
    min_size: usize,
    ppt_limit_w: Option<usize>,
    ppt_value_w: Option<usize>,
    tdc_limit_a: Option<usize>,
    tdc_value_a: Option<usize>,
    tctl_limit_c: Option<usize>,
    tctl_value_c: Option<usize>,
    edc_limit_a: Option<usize>,
    edc_value_a: Option<usize>,
    stapm_limit_w: Option<usize>,
    stapm_value_w: Option<usize>,
    slow_limit_w: Option<usize>,
    slow_value_w: Option<usize>,
    apu_slow_limit_w: Option<usize>,
    apu_slow_value_w: Option<usize>,
    tdc_soc_limit_a: Option<usize>,
    tdc_soc_value_a: Option<usize>,
    edc_soc_limit_a: Option<usize>,
    edc_soc_value_a: Option<usize>,
    apu_skin_temp_limit_c: Option<usize>,
    apu_skin_temp_value_c: Option<usize>,
    dgpu_skin_temp_limit_c: Option<usize>,
    dgpu_skin_temp_value_c: Option<usize>,
    vddcr_soc_v: Option<usize>,
    vddcr_misc_v: Option<usize>,
    fclk_mhz: Option<usize>,
    uclk_mhz: Option<usize>,
    mclk_mhz: Option<usize>,
    cldo_vddp_v: Option<usize>,
    cldo_vddg_iod_v: Option<usize>,
    cldo_vddg_ccd_v: Option<usize>,
}

/// Standard mobile APU PM table header offsets (`0x000..=0x054`), shared by
/// Renoir/Lucienne, Cezanne, Rembrandt, and Phoenix/HawkPoint (`0x004C_0006+`).
fn apu_std_header(min_size: usize) -> PmTableLayout {
    PmTableLayout {
        min_size,
        stapm_limit_w: Some(0x000),
        stapm_value_w: Some(0x004),
        ppt_limit_w: Some(0x008),
        ppt_value_w: Some(0x00C),
        slow_limit_w: Some(0x010),
        slow_value_w: Some(0x014),
        apu_slow_limit_w: Some(0x018),
        apu_slow_value_w: Some(0x01C),
        tdc_limit_a: Some(0x020),
        tdc_value_a: Some(0x024),
        tdc_soc_limit_a: Some(0x028),
        tdc_soc_value_a: Some(0x02C),
        edc_limit_a: Some(0x030),
        edc_value_a: Some(0x034),
        edc_soc_limit_a: Some(0x038),
        edc_soc_value_a: Some(0x03C),
        tctl_limit_c: Some(0x040),
        tctl_value_c: Some(0x044),
        apu_skin_temp_limit_c: Some(0x048),
        apu_skin_temp_value_c: Some(0x04C),
        dgpu_skin_temp_limit_c: Some(0x050),
        dgpu_skin_temp_value_c: Some(0x054),
        ..PmTableLayout::default()
    }
}

fn layout_for(codename: Codename, version: u32) -> Option<PmTableLayout> {
    match codename {
        Codename::GraniteRidge => {
            let min_size = match version {
                0x0062_0105 | 0x0062_1101 | 0x0062_1102 => 0x724,
                0x0062_0205 | 0x0062_1201 | 0x0062_1202 => 0x994,
                _ => return None,
            };
            Some(PmTableLayout {
                min_size,
                // Verified live on Ryzen 7 9700X (Granite Ridge, PM table 0x620105, 0x724 bytes):
                // 0x000..0x004 STAPM (0 on desktop), 0x008/0x00C PPT, 0x020/0x024 TDC VDD,
                // 0x028/0x02C THM/Tctl, 0x0FC/0x100 EDC VDD.
                ppt_limit_w: Some(0x008),
                ppt_value_w: Some(0x00C),
                tdc_limit_a: Some(0x020),
                tdc_value_a: Some(0x024),
                tctl_limit_c: Some(0x028),
                tctl_value_c: Some(0x02C),
                edc_limit_a: Some(0x0FC),
                edc_value_a: Some(0x100),
                vddcr_soc_v: Some(0x0D8),
                vddcr_misc_v: Some(0x0EC),
                fclk_mhz: Some(0x11C),
                uclk_mhz: Some(0x12C),
                mclk_mhz: Some(0x13C),
                cldo_vddg_iod_v: Some(0x40C),
                cldo_vddg_ccd_v: Some(0x414),
                cldo_vddp_v: Some(0x434),
                ..PmTableLayout::default()
            })
        }
        Codename::Raphael => {
            let (min_size, vsoc, misc, fclk, uclk, mclk, vddp) = match version {
                0x0054_0000..=0x0054_0005 | 0x0054_0100..=0x0054_0108 => {
                    (0x434, 0x0D0, 0x0E4, 0x118, 0x128, 0x138, 0x430)
                }
                0x0054_0208 => (0x438, 0x0D4, 0x0E8, 0x11C, 0x12C, 0x13C, 0x434),
                _ => return None,
            };
            Some(PmTableLayout {
                min_size,
                ppt_limit_w: Some(0x008),
                ppt_value_w: Some(0x00C),
                tdc_limit_a: Some(0x020),
                tdc_value_a: Some(0x024),
                tctl_limit_c: Some(0x028),
                tctl_value_c: Some(0x02C),
                edc_limit_a: Some(0x0FC),
                edc_value_a: Some(0x100),
                vddcr_soc_v: Some(vsoc),
                vddcr_misc_v: Some(misc),
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                cldo_vddp_v: Some(vddp),
                ..PmTableLayout::default()
            })
        }
        Codename::StormPeak => {
            let (min_size, fclk, uclk, mclk, vsoc, misc) = match version {
                0x005C_0302 => (0xD9C, 0x194, 0x1A8, 0x1BC, 0x120, 0x134),
                0x005C_0303 | 0x005C_0402 | 0x005C_0403 => {
                    (0x974, 0x19C, 0x1B0, 0x1C4, 0x128, 0x13C)
                }
                _ => return None,
            };
            Some(PmTableLayout {
                min_size,
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(vsoc),
                vddcr_misc_v: Some(misc),
                ..PmTableLayout::default()
            })
        }
        Codename::Vermeer | Codename::Chagall => match version {
            0x002D_0803 | 0x002D_0903 => Some(PmTableLayout {
                min_size: 0x228,
                ppt_limit_w: Some(0x000),
                ppt_value_w: Some(0x004),
                tdc_limit_a: Some(0x008),
                tdc_value_a: Some(0x00C),
                tctl_limit_c: Some(0x010),
                tctl_value_c: Some(0x014),
                edc_limit_a: Some(0x020),
                edc_value_a: Some(0x024),
                vddcr_soc_v: Some(0x0B0),
                fclk_mhz: Some(0x0BC),
                uclk_mhz: Some(0x0C4),
                mclk_mhz: Some(0x0C8),
                cldo_vddp_v: Some(0x220),
                cldo_vddg_iod_v: Some(0x224),
                ..PmTableLayout::default()
            }),
            0x0038_0505 | 0x0038_0605 | 0x0038_0705 | 0x0038_0804 | 0x0038_0805 | 0x0038_0904
            | 0x0038_0905 => Some(PmTableLayout {
                min_size: 0x230,
                ppt_limit_w: Some(0x000),
                ppt_value_w: Some(0x004),
                tdc_limit_a: Some(0x008),
                tdc_value_a: Some(0x00C),
                tctl_limit_c: Some(0x010),
                tctl_value_c: Some(0x014),
                edc_limit_a: Some(0x020),
                edc_value_a: Some(0x024),
                vddcr_soc_v: Some(0x0B4),
                fclk_mhz: Some(0x0C0),
                uclk_mhz: Some(0x0C8),
                mclk_mhz: Some(0x0CC),
                cldo_vddp_v: Some(0x224),
                cldo_vddg_iod_v: Some(0x228),
                cldo_vddg_ccd_v: Some(0x22C),
                ..PmTableLayout::default()
            }),
            _ => None,
        },
        Codename::Matisse | Codename::CastlePeak => {
            let (min_size, vsoc, fclk, uclk, mclk, vddp, vddg_iod) = match version {
                0x0024_0802 | 0x0024_0902 => (0x1F8, 0x0B0, 0x0BC, 0x0C4, 0x0C8, 0x1F0, 0x1F4),
                0x0024_0503 | 0x0024_0603 | 0x0024_0703 | 0x0024_0803 | 0x0024_0903 => {
                    (0x1FC, 0x0B4, 0x0C0, 0x0C8, 0x0CC, 0x1F4, 0x1F8)
                }
                _ => return None,
            };
            Some(PmTableLayout {
                min_size,
                ppt_limit_w: Some(0x000),
                ppt_value_w: Some(0x004),
                tdc_limit_a: Some(0x008),
                tdc_value_a: Some(0x00C),
                tctl_limit_c: Some(0x010),
                tctl_value_c: Some(0x014),
                edc_limit_a: Some(0x020),
                edc_value_a: Some(0x024),
                vddcr_soc_v: Some(vsoc),
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                cldo_vddp_v: Some(vddp),
                cldo_vddg_iod_v: Some(vddg_iod),
                ..PmTableLayout::default()
            })
        }
        Codename::Renoir | Codename::Lucienne => {
            // Verified live on Ryzen 7 5700U (Lucienne, PM table 0x00370005, 0x8C8 bytes)
            // and cross-checked with RyzenAdj + ZenStates-Core PowerTable.cs.
            let (min_size, fclk, uclk, mclk, vsoc) = match version {
                0x0037_0000 => (0x794, 0x4B4, 0x4B8, 0x4BC, 0x190),
                0x0037_0001 => (0x884, 0x5A4, 0x5A8, 0x5AC, 0x190),
                0x0037_0002 => (0x88C, 0x5AC, 0x5B0, 0x5B4, 0x198),
                0x0037_0003 | 0x0037_0004 => (0x8AC, 0x5CC, 0x5D0, 0x5D4, 0x198),
                0x0037_0005 => (0x8C8, 0x5E8, 0x5EC, 0x5F0, 0x198),
                _ => return None,
            };
            Some(PmTableLayout {
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(vsoc),
                ..apu_std_header(min_size)
            })
        }
        Codename::Cezanne => {
            let (min_size, fclk, uclk, mclk) = match version {
                0x0040_0001 => (0x8D0, 0x624, 0x628, 0x62C),
                0x0040_0002 => (0x928, 0x63C, 0x640, 0x644),
                0x0040_0003 => (0x94C, 0x660, 0x664, 0x668),
                0x0040_0004 | 0x0040_0005 => (0x944, 0x664, 0x668, 0x66C),
                _ => return None,
            };
            Some(PmTableLayout {
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(0x19C),
                ..apu_std_header(min_size)
            })
        }
        Codename::Rembrandt => {
            let (min_size, fclk, uclk, mclk, vsoc) = match version {
                0x0045_0004 => (0xAA4, 0x664, 0x668, 0x66C, 0x19C),
                0x0045_0005 => (0xAB0, 0x6B0, 0x6B4, 0x6B8, 0x1C8),
                _ => return None,
            };
            Some(PmTableLayout {
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(vsoc),
                ..apu_std_header(min_size)
            })
        }
        Codename::Phoenix | Codename::HawkPoint => {
            let (min_size, fclk, uclk, mclk, vsoc, vddp) = match version {
                0x004C_0001..=0x004C_0007 => (0x76C, 0x174, 0x184, 0x194, 0x1C8, 0x768),
                0x004C_0008 => (0x76C, 0x164, 0x174, 0x184, 0x1B8, 0x768),
                0x004C_0009 => (0x778, 0x164, 0x174, 0x184, 0x1BC, 0x774),
                _ => return None,
            };
            let base = if matches!(version, 0x004C_0006..=0x004C_0009) {
                apu_std_header(min_size)
            } else {
                PmTableLayout {
                    min_size,
                    ..PmTableLayout::default()
                }
            };
            Some(PmTableLayout {
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(vsoc),
                cldo_vddp_v: Some(vddp),
                ..base
            })
        }
        Codename::StrixPoint => match version {
            0x005D_0008..=0x005D_000B => Some(PmTableLayout {
                fclk_mhz: Some(0x4E0),
                uclk_mhz: Some(0x4E4),
                mclk_mhz: Some(0x4EC),
                ..apu_std_header(0xD54)
            }),
            _ => None,
        },
        Codename::StrixHalo => match version {
            0x0064_0107..=0x0064_010C => Some(apu_std_header(0xDC0)),
            _ => None,
        },
        Codename::VanGogh => match version {
            0x003F_0000 => Some(PmTableLayout {
                fclk_mhz: Some(0x3BC),
                uclk_mhz: Some(0x3C0),
                mclk_mhz: Some(0x3C4),
                vddcr_soc_v: Some(0x1A0),
                ..apu_std_header(0x7AC)
            }),
            _ => None,
        },
        Codename::RavenRidge | Codename::RavenRidge2 | Codename::Picasso | Codename::Dali => {
            let (min_size, fclk, uclk, mclk, vsoc, vddp) = match version {
                0x001E_0001 => (0x570, 0x460, 0x464, 0x468, 0x10C, 0x0F8),
                0x001E_0002 => (0x580, 0x474, 0x478, 0x47C, 0x10C, 0x0F8),
                0x001E_0003..=0x001E_0005 | 0x001E_000A | 0x001E_0101 => {
                    (0x608, 0x298, 0x29C, 0x2A0, 0x104, 0x0F0)
                }
                _ => return None,
            };
            Some(PmTableLayout {
                fclk_mhz: Some(fclk),
                uclk_mhz: Some(uclk),
                mclk_mhz: Some(mclk),
                vddcr_soc_v: Some(vsoc),
                cldo_vddp_v: Some(vddp),
                ..apu_std_header(min_size)
            })
        }
        _ => None,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::profiles::capabilities_for;
    use crate::hw::topology::CpuTopology;
    use crate::transport::mock::MockTransport;

    fn write_f32(buf: &mut [u8], offset: usize, val: f32) {
        buf[offset..offset + 4].copy_from_slice(&val.to_le_bytes());
    }

    #[test]
    fn decodes_granite_ridge_9700x_pm_table() {
        let transport = MockTransport::granite_ridge_9700x();
        let driver = transport.read_info().unwrap();
        let hw = HardwareContext {
            driver,
            codename: Codename::GraniteRidge,
            cpuid: None,
            topology: CpuTopology::synthetic(1, 8, 2),
            capabilities: capabilities_for(Codename::GraniteRidge).unwrap(),
        };

        let snap = read_telemetry(&transport, &hw).unwrap();
        assert_eq!(snap.codename, Codename::GraniteRidge);
        assert_eq!(snap.table_version, 0x0062_0105);
        assert_eq!(snap.table_size, 0x724);
        assert_eq!(snap.ppt_limit_w, Some(88.0));
        assert_eq!(snap.ppt_value_w, Some(42.5));
        assert_eq!(snap.tdc_limit_a, Some(75.0));
        assert_eq!(snap.tdc_value_a, Some(21.0));
        assert_eq!(snap.edc_limit_a, Some(150.0));
        assert_eq!(snap.edc_value_a, Some(38.0));
        assert_eq!(snap.tctl_limit_c, Some(95.0));
        assert_eq!(snap.tctl_value_c, Some(54.2));
        assert_eq!(snap.fclk_mhz, Some(2000.0));
        assert_eq!(snap.uclk_mhz, Some(3000.0));
        assert_eq!(snap.mclk_mhz, Some(3000.0));
        assert_eq!(snap.vddcr_soc_v, Some(1.24));
        assert_eq!(snap.vddcr_misc_v, Some(1.10));
        assert_eq!(snap.cldo_vddp_v, Some(1.05));
        assert_eq!(snap.cldo_vddg_iod_v, Some(0.95));
        assert_eq!(snap.cldo_vddg_ccd_v, Some(0.95));
    }

    #[test]
    fn refuses_unknown_version_and_oversized_table() {
        let buf = vec![0u8; 0x724];
        assert!(matches!(
            decode_pm_table(Codename::GraniteRidge, 0xDEAD_BEEF, &buf),
            Err(Error::UnsupportedPmTable {
                version: 0xDEAD_BEEF,
                ..
            })
        ));

        // Valid version for GraniteRidge rejected when paired with Raphael
        assert!(matches!(
            decode_pm_table(Codename::Raphael, 0x0062_0105, &buf),
            Err(Error::UnsupportedPmTable { .. })
        ));

        // Oversized (> 4096 bytes) refused
        let huge = vec![0u8; 4097];
        assert!(matches!(
            decode_pm_table(Codename::GraniteRidge, 0x0062_0105, &huge),
            Err(Error::PmTableTooLarge { size: 4097 })
        ));

        // Truncated buffer refused
        let tiny = vec![0u8; 0x100];
        assert!(matches!(
            decode_pm_table(Codename::GraniteRidge, 0x0062_0105, &tiny),
            Err(Error::UnsupportedPmTable { .. })
        ));
    }

    #[test]
    fn filters_nan_inf_and_absurd_values() {
        let mut raw = vec![0u8; 0x724];
        write_f32(&mut raw, 0x008, f32::NAN);
        write_f32(&mut raw, 0x00C, f32::INFINITY);
        write_f32(&mut raw, 0x0D8, 99.0); // absurd voltage
        write_f32(&mut raw, 0x11C, 2000.0); // valid FCLK
        write_f32(&mut raw, 0x12C, -50.0); // negative clock

        let snap = decode_pm_table(Codename::GraniteRidge, 0x0062_0105, &raw).unwrap();
        assert_eq!(snap.ppt_limit_w, None);
        assert_eq!(snap.ppt_value_w, None);
        assert_eq!(snap.vddcr_soc_v, None);
        assert_eq!(snap.fclk_mhz, Some(2000.0));
        assert_eq!(snap.uclk_mhz, None);
    }

    #[test]
    fn decodes_vermeer_and_phoenix_layouts() {
        let mut vermeer = vec![0u8; 0x300];
        write_f32(&mut vermeer, 0x000, 142.0);
        write_f32(&mut vermeer, 0x0B4, 1.12);
        write_f32(&mut vermeer, 0x0C0, 1800.0);
        write_f32(&mut vermeer, 0x224, 0.90);
        let vsnap = decode_pm_table(Codename::Vermeer, 0x0038_0805, &vermeer).unwrap();
        assert_eq!(vsnap.ppt_limit_w, Some(142.0));
        assert_eq!(vsnap.vddcr_soc_v, Some(1.12));
        assert_eq!(vsnap.fclk_mhz, Some(1800.0));
        assert_eq!(vsnap.cldo_vddp_v, Some(0.90));

        let mut phoenix = vec![0u8; 0x800];
        write_f32(&mut phoenix, 0x008, 45.0);
        write_f32(&mut phoenix, 0x164, 2000.0);
        write_f32(&mut phoenix, 0x1B8, 1.15);
        write_f32(&mut phoenix, 0x768, 0.95);
        let psnap = decode_pm_table(Codename::Phoenix, 0x004C_0008, &phoenix).unwrap();
        assert_eq!(psnap.ppt_limit_w, Some(45.0));
        assert_eq!(psnap.fclk_mhz, Some(2000.0));
        assert_eq!(psnap.vddcr_soc_v, Some(1.15));
        assert_eq!(psnap.cldo_vddp_v, Some(0.95));

        let mut strix = vec![0u8; 0xD54];
        write_f32(&mut strix, 0x000, 28.0);
        write_f32(&mut strix, 0x4E0, 2000.0);
        let ssnap = decode_pm_table(Codename::StrixPoint, 0x005D_0008, &strix).unwrap();
        assert_eq!(ssnap.stapm_limit_w, Some(28.0));
        assert_eq!(ssnap.fclk_mhz, Some(2000.0));

        let mut picasso = vec![0u8; 0x608];
        write_f32(&mut picasso, 0x000, 15.0);
        write_f32(&mut picasso, 0x298, 1200.0);
        write_f32(&mut picasso, 0x104, 0.92);
        let picsnap = decode_pm_table(Codename::Picasso, 0x001E_0004, &picasso).unwrap();
        assert_eq!(picsnap.stapm_limit_w, Some(15.0));
        assert_eq!(picsnap.fclk_mhz, Some(1200.0));
        assert_eq!(picsnap.vddcr_soc_v, Some(0.92));

        let mut lucienne = vec![0u8; 0x8C8];
        write_f32(&mut lucienne, 0x008, 30.0);
        write_f32(&mut lucienne, 0x00C, 4.6);
        write_f32(&mut lucienne, 0x020, 33.0);
        write_f32(&mut lucienne, 0x030, 70.0);
        write_f32(&mut lucienne, 0x040, 85.0);
        write_f32(&mut lucienne, 0x044, 44.4);
        write_f32(&mut lucienne, 0x198, 0.725);
        write_f32(&mut lucienne, 0x5E8, 1200.0);
        let lsnap = decode_pm_table(Codename::Lucienne, 0x0037_0005, &lucienne).unwrap();
        assert_eq!(lsnap.ppt_limit_w, Some(30.0));
        assert_eq!(lsnap.tdc_limit_a, Some(33.0));
        assert_eq!(lsnap.edc_limit_a, Some(70.0));
        assert_eq!(lsnap.tctl_limit_c, Some(85.0));
        assert_eq!(lsnap.tctl_value_c, Some(44.4));
        assert_eq!(lsnap.vddcr_soc_v, Some(0.725));
        assert_eq!(lsnap.fclk_mhz, Some(1200.0));
    }

    #[test]
    fn dump_raw_pm_table_writes_file() {
        let transport = MockTransport::granite_ridge_9700x();
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("pm_table.bin");
        let n = dump_raw_pm_table(&transport, &out).unwrap();
        assert_eq!(n, 0x724);
        assert_eq!(std::fs::read(&out).unwrap().len(), 0x724);
    }
}
