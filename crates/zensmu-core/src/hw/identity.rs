use crate::error::{Error, Result};
use crate::hw::profiles::{capabilities_for, CpuCapabilities};
use crate::hw::topology::CpuTopology;
use crate::transport::{DriverSysfsInfo, SmuTransport};
use crate::types::Codename;
use serde::{Deserialize, Serialize};

/// x86 CPUID info cross-checked against `ryzen_smu`'s reported codename.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CpuidInfo {
    pub vendor: String,
    pub brand: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
}

/// Complete verified hardware identity & capabilities for the running system.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HardwareContext {
    pub driver: DriverSysfsInfo,
    pub codename: Codename,
    pub cpuid: Option<CpuidInfo>,
    pub topology: CpuTopology,
    pub capabilities: CpuCapabilities,
}

/// Read x86_64 CPUID family/model/stepping and brand string.
pub fn read_host_cpuid() -> Option<CpuidInfo> {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::__cpuid;
        let leaf0 = __cpuid(0);
        let mut vendor_bytes = [0u8; 12];
        vendor_bytes[0..4].copy_from_slice(&leaf0.ebx.to_le_bytes());
        vendor_bytes[4..8].copy_from_slice(&leaf0.edx.to_le_bytes());
        vendor_bytes[8..12].copy_from_slice(&leaf0.ecx.to_le_bytes());
        let vendor = String::from_utf8_lossy(&vendor_bytes).trim().to_string();

        let leaf1 = __cpuid(1);
        let eax = leaf1.eax;
        let base_family = (eax >> 8) & 0xF;
        let ext_family = (eax >> 20) & 0xFF;
        let family = if base_family == 0xF {
            base_family + ext_family
        } else {
            base_family
        };
        let base_model = (eax >> 4) & 0xF;
        let ext_model = (eax >> 16) & 0xF;
        let model = if base_family == 0x6 || base_family == 0xF {
            (ext_model << 4) | base_model
        } else {
            base_model
        };
        let stepping = eax & 0xF;

        let mut brand_bytes = Vec::with_capacity(48);
        let max_ext = __cpuid(0x8000_0000).eax;
        if max_ext >= 0x8000_0004 {
            for leaf in 0x8000_0002..=0x8000_0004 {
                let r = __cpuid(leaf);
                brand_bytes.extend_from_slice(&r.eax.to_le_bytes());
                brand_bytes.extend_from_slice(&r.ebx.to_le_bytes());
                brand_bytes.extend_from_slice(&r.ecx.to_le_bytes());
                brand_bytes.extend_from_slice(&r.edx.to_le_bytes());
            }
        }
        let brand = String::from_utf8_lossy(&brand_bytes)
            .trim_matches(char::from(0))
            .trim()
            .to_string();

        Some(CpuidInfo {
            vendor,
            brand,
            family,
            model,
            stepping,
        })
    }
    #[cfg(not(target_arch = "x86_64"))]
    {
        None
    }
}

/// Verify that `codename` is consistent with CPUID `family` (and verified `model`s
/// for finalized silicon families), guarding against spoofed/mismatched sysfs.
pub fn verify_codename_against_cpuid(codename: Codename, cpuid: &CpuidInfo) -> Result<()> {
    let expected: Option<(u32, &'static [u32])> = match codename {
        Codename::SummitRidge | Codename::ThreadRipper => Some((0x17, &[0x01])),
        Codename::PinnacleRidge | Codename::Colfax => Some((0x17, &[0x08])),
        Codename::RavenRidge => Some((0x17, &[0x11])),
        Codename::Picasso => Some((0x17, &[0x18])),
        Codename::RavenRidge2 | Codename::Dali => Some((0x17, &[0x20])),
        Codename::VanGogh => Some((0x17, &[0x90, 0x91])),
        Codename::Matisse
        | Codename::CastlePeak
        | Codename::Renoir
        | Codename::Lucienne
        | Codename::Naples => Some((0x17, &[])),
        Codename::Vermeer
        | Codename::Chagall
        | Codename::Milan
        | Codename::Cezanne
        | Codename::Rembrandt
        | Codename::Raphael
        | Codename::Phoenix
        | Codename::HawkPoint
        | Codename::StormPeak => Some((0x19, &[])),
        Codename::GraniteRidge | Codename::StrixPoint | Codename::StrixHalo => Some((0x1A, &[])),
        Codename::Undefined => None,
    };

    if let Some((fam, models)) = expected {
        let model_ok = models.is_empty() || models.contains(&cpuid.model);
        if cpuid.family != fam || !model_ok {
            return Err(Error::IdentityMismatch {
                driver_codename: codename.display_name().to_string(),
                family: cpuid.family,
                model: cpuid.model,
            });
        }
    }
    Ok(())
}

/// Probe hardware context from a transport, optionally cross-checking host CPUID
/// (set `verify_host_cpuid = false` when testing against a mock sysfs on a different host).
pub fn detect_hardware(
    transport: &dyn SmuTransport,
    verify_host_cpuid: bool,
) -> Result<HardwareContext> {
    let driver = transport.read_info()?;
    let codename =
        Codename::from_driver_id(driver.codename_id).ok_or_else(|| Error::UnsupportedCpu {
            codename: format!("id={}", driver.codename_id),
            detail: "codename ID not recognized by zensmu".into(),
        })?;

    let cpuid = if verify_host_cpuid {
        let info = read_host_cpuid();
        if let Some(ref c) = info {
            verify_codename_against_cpuid(codename, c)?;
        }
        info
    } else {
        None
    };

    let capabilities = capabilities_for(codename).ok_or_else(|| Error::UnsupportedCpu {
        codename: codename.display_name().to_string(),
        detail: "no verified safe SMU capability table for this CPU generation".into(),
    })?;

    let topology = CpuTopology::detect_linux().unwrap_or_else(|_| CpuTopology::fallback(1));

    Ok(HardwareContext {
        driver,
        codename,
        cpuid,
        topology,
        capabilities,
    })
}

/// Detect whether the installed `ryzen_smu` version is known to suffer from `smu.c:213` false-OK bug.
pub fn driver_has_false_ok_bug(drv_version: &str) -> bool {
    let trimmed = drv_version.trim();
    matches!(trimmed, "0.1.5" | "0.1.6" | "0.1.7")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpuid_verification_accepts_granite_ridge() {
        let cpuid = CpuidInfo {
            vendor: "AuthenticAMD".into(),
            brand: "AMD Ryzen 7 9700X 8-Core Processor".into(),
            family: 0x1A,
            model: 0x44,
            stepping: 0,
        };
        assert!(verify_codename_against_cpuid(Codename::GraniteRidge, &cpuid).is_ok());
        assert!(verify_codename_against_cpuid(Codename::Vermeer, &cpuid).is_err());
    }

    #[test]
    fn cpuid_verification_checks_zen1_zenplus_and_apu0_models() {
        let mk = |family, model| CpuidInfo {
            vendor: "AuthenticAMD".into(),
            brand: "AMD Test".into(),
            family,
            model,
            stepping: 0,
        };
        assert!(verify_codename_against_cpuid(Codename::SummitRidge, &mk(0x17, 0x01)).is_ok());
        assert!(verify_codename_against_cpuid(Codename::ThreadRipper, &mk(0x17, 0x01)).is_ok());
        assert!(verify_codename_against_cpuid(Codename::PinnacleRidge, &mk(0x17, 0x08)).is_ok());
        assert!(verify_codename_against_cpuid(Codename::Colfax, &mk(0x17, 0x08)).is_ok());
        assert!(verify_codename_against_cpuid(Codename::RavenRidge, &mk(0x17, 0x11)).is_ok());
        assert!(verify_codename_against_cpuid(Codename::Picasso, &mk(0x17, 0x18)).is_ok());
        assert!(verify_codename_against_cpuid(Codename::RavenRidge2, &mk(0x17, 0x20)).is_ok());
        assert!(verify_codename_against_cpuid(Codename::Dali, &mk(0x17, 0x20)).is_ok());
        assert!(verify_codename_against_cpuid(Codename::VanGogh, &mk(0x17, 0x90)).is_ok());
        assert!(verify_codename_against_cpuid(Codename::VanGogh, &mk(0x17, 0x91)).is_ok());

        // Cross-model mismatch within family 0x17 is rejected
        assert!(verify_codename_against_cpuid(Codename::Picasso, &mk(0x17, 0x08)).is_err());
        assert!(verify_codename_against_cpuid(Codename::VanGogh, &mk(0x17, 0x11)).is_err());
    }
}
