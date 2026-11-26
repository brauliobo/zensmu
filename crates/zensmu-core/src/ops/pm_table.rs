//! Strictly version-gated PM table decoders for unprivileged SMU telemetry.
//!
//! Unknown PM table versions and tables exceeding the safe sysfs `PAGE_SIZE`
//! limit (4096 bytes) are explicitly refused rather than guessed.

use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
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
#[derive(Debug, Clone, Copy)]
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
    vddcr_soc_v: Option<usize>,
    vddcr_misc_v: Option<usize>,
    fclk_mhz: Option<usize>,
    uclk_mhz: Option<usize>,
    mclk_mhz: Option<usize>,
    cldo_vddp_v: Option<usize>,
    cldo_vddg_iod_v: Option<usize>,
    cldo_vddg_ccd_v: Option<usize>,
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
            })
        }
        Codename::Raphael => {
            let supported = matches!(
                version,
                0x0054_0000..=0x0054_0005 | 0x0054_0100..=0x0054_0108 | 0x0054_0208
            );
            if !supported {
                return None;
            }
            Some(PmTableLayout {
                min_size: 0x434,
                ppt_limit_w: Some(0x008),
                ppt_value_w: Some(0x00C),
                tdc_limit_a: Some(0x020),
                tdc_value_a: Some(0x024),
                tctl_limit_c: Some(0x028),
                tctl_value_c: Some(0x02C),
                edc_limit_a: Some(0x0FC),
                edc_value_a: Some(0x100),
                vddcr_soc_v: Some(0x0D0),
                vddcr_misc_v: Some(0x0E4),
                fclk_mhz: Some(0x118),
                uclk_mhz: Some(0x128),
                mclk_mhz: Some(0x138),
                cldo_vddp_v: Some(0x430),
                cldo_vddg_iod_v: None,
                cldo_vddg_ccd_v: None,
            })
        }
        Codename::Vermeer => {
            let supported = matches!(
                version,
                0x0038_0505
                    | 0x0038_0605
                    | 0x0038_0705
                    | 0x0038_0804
                    | 0x0038_0805
                    | 0x0038_0904
                    | 0x0038_0905
            );
            if !supported {
                return None;
            }
            Some(PmTableLayout {
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
                vddcr_misc_v: None,
                fclk_mhz: Some(0x0C0),
                uclk_mhz: Some(0x0C8),
                mclk_mhz: Some(0x0CC),
                cldo_vddp_v: Some(0x224),
                cldo_vddg_iod_v: Some(0x228),
                cldo_vddg_ccd_v: Some(0x22C),
            })
        }
        Codename::Matisse => {
            let supported = matches!(
                version,
                0x0024_0802 | 0x0024_0803 | 0x0024_0902 | 0x0024_0903
            );
            if !supported {
                return None;
            }
            Some(PmTableLayout {
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
                vddcr_misc_v: None,
                fclk_mhz: Some(0x0BC),
                uclk_mhz: Some(0x0C4),
                mclk_mhz: Some(0x0C8),
                cldo_vddp_v: Some(0x220),
                cldo_vddg_iod_v: Some(0x224),
                cldo_vddg_ccd_v: None,
            })
        }
        Codename::Phoenix | Codename::HawkPoint => {
            let supported = matches!(version, 0x004C_0003..=0x004C_0009);
            if !supported {
                return None;
            }
            Some(PmTableLayout {
                min_size: 0x778,
                ppt_limit_w: None,
                ppt_value_w: None,
                tdc_limit_a: None,
                tdc_value_a: None,
                tctl_limit_c: None,
                tctl_value_c: None,
                edc_limit_a: None,
                edc_value_a: None,
                fclk_mhz: Some(0x164),
                uclk_mhz: Some(0x174),
                mclk_mhz: Some(0x184),
                vddcr_soc_v: Some(0x1BC),
                vddcr_misc_v: None,
                cldo_vddp_v: Some(0x774),
                cldo_vddg_iod_v: None,
                cldo_vddg_ccd_v: None,
            })
        }
        _ => None,
    }
}

fn read_f32_sane(raw: &[u8], offset: Option<usize>, min: f32, max: f32) -> Option<f32> {
    let off = offset?;
    let bytes: [u8; 4] = raw.get(off..off + 4)?.try_into().ok()?;
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
pub fn decode_pm_table(
    codename: Codename,
    version: u32,
    raw: &[u8],
) -> Result<TelemetrySnapshot> {
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
    std::fs::write(out_path, &raw).map_err(|source| Error::Io {
        path: out_path.display().to_string(),
        source,
    })?;
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
        write_f32(&mut phoenix, 0x164, 2000.0);
        write_f32(&mut phoenix, 0x1BC, 1.15);
        write_f32(&mut phoenix, 0x774, 0.95);
        let psnap = decode_pm_table(Codename::Phoenix, 0x004C_0008, &phoenix).unwrap();
        assert_eq!(psnap.fclk_mhz, Some(2000.0));
        assert_eq!(psnap.vddcr_soc_v, Some(1.15));
        assert_eq!(psnap.cldo_vddp_v, Some(0.95));
        assert_eq!(psnap.ppt_limit_w, None);
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
