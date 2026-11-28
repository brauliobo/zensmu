use super::hwmon::{scan_hwmon_dir, HwmonSensorReading};
use super::system::SystemBoardInfo;
use super::{DebugReport, PmTableFloatEntry, ReportOptions};
use crate::driver::{detect_secure_boot, SecureBootStatus};
use crate::error::Error;
use crate::hw::identity::{
    read_host_cpuid, verify_codename_against_cpuid, CpuidInfo, HardwareContext,
};
use crate::hw::profiles::capabilities_for;
use crate::hw::topology::CpuTopology;
use crate::ops::co::{self, HEALTH_CHECK_ARG, HEALTH_CHECK_EXPECTED};
use crate::ops::pbo;
use crate::ops::pm_table::{self, MAX_SAFE_PM_TABLE_SIZE};
use crate::ops::shaper;
use crate::transport::SmuTransport;
use crate::types::Codename;
use std::path::Path;

const MAX_RAW_PM_FLOATS: usize = 160;

/// Collect a complete, strictly read-only, privacy-safe diagnostic report.
///
/// Never fails or panics even if `ryzen_smu` is not loaded, the CPU codename or
/// PM table version is unsupported, CPUID is unavailable, or mailbox access is
/// denied. Every probe captures either `Some(value)` or its corresponding
/// `*_error` field.
pub fn collect_debug_report(transport: &dyn SmuTransport, opts: &ReportOptions) -> DebugReport {
    let system = SystemBoardInfo::detect_linux();
    let cpuid = read_host_cpuid();
    let topology = CpuTopology::detect_linux().ok();
    let hwmon_reference = scan_hwmon_dir(Path::new("/sys/class/hwmon"));
    let secure_boot = Some(detect_secure_boot());

    collect_debug_report_inner(
        transport,
        opts,
        system,
        cpuid,
        topology,
        hwmon_reference,
        secure_boot,
    )
}

pub(super) fn collect_debug_report_inner(
    transport: &dyn SmuTransport,
    opts: &ReportOptions,
    system: SystemBoardInfo,
    cpuid: Option<CpuidInfo>,
    topology: Option<CpuTopology>,
    hwmon_reference: Vec<HwmonSensorReading>,
    secure_boot: Option<SecureBootStatus>,
) -> DebugReport {
    let zensmu_version = env!("CARGO_PKG_VERSION").to_string();
    let user_note = opts
        .user_note
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut driver_error = None;
    let driver = match transport.read_info() {
        Ok(info) => Some(info),
        Err(err) => {
            driver_error = Some(err.to_string());
            None
        }
    };

    let codename = driver
        .as_ref()
        .and_then(|d| Codename::from_driver_id(d.codename_id));
    let capabilities = codename.and_then(capabilities_for);
    let capabilities_supported = capabilities.is_some();

    let mut telemetry = None;
    let mut telemetry_error = None;
    let mut raw_pm_table_floats = Vec::new();

    if let Some(ref drv) = driver {
        if !drv.has_pm_table {
            telemetry_error = Some(
                Error::UnsupportedCapability {
                    capability: "pm_table",
                    codename: codename
                        .map(|c| c.display_name().to_string())
                        .unwrap_or_else(|| format!("id={}", drv.codename_id)),
                }
                .to_string(),
            );
        } else if let Some(sz) = drv.pm_table_size.filter(|&s| s > MAX_SAFE_PM_TABLE_SIZE) {
            telemetry_error = Some(Error::PmTableTooLarge { size: sz }.to_string());
        } else {
            match transport.read_pm_table() {
                Ok(raw) => {
                    if raw.len() > MAX_SAFE_PM_TABLE_SIZE {
                        telemetry_error =
                            Some(Error::PmTableTooLarge { size: raw.len() }.to_string());
                    } else {
                        match (codename, drv.pm_table_version) {
                            (Some(cn), Some(ver)) => {
                                match pm_table::decode_pm_table(cn, ver, &raw) {
                                    Ok(snap) => telemetry = Some(snap),
                                    Err(err) => telemetry_error = Some(err.to_string()),
                                }
                            }
                            (None, Some(ver)) => {
                                telemetry_error = Some(
                                    Error::UnsupportedPmTable {
                                        codename: format!("id={}", drv.codename_id),
                                        version: ver,
                                        size: raw.len(),
                                    }
                                    .to_string(),
                                );
                            }
                            (_, None) => {
                                telemetry_error = Some(
                                    Error::UnsupportedCapability {
                                        capability: "pm_table_version",
                                        codename: codename
                                            .map(|c| c.display_name().to_string())
                                            .unwrap_or_else(|| format!("id={}", drv.codename_id)),
                                    }
                                    .to_string(),
                                );
                            }
                        }

                        if opts.include_raw_pm_table || telemetry.is_none() {
                            raw_pm_table_floats = extract_raw_pm_table_floats(&raw);
                        }
                    }
                }
                Err(err) => {
                    telemetry_error = Some(err.to_string());
                }
            }
        }
    }

    // Build HardwareContext only when driver, codename, and verified capabilities exist
    // and (unless `no_cpuid_check` is set) CPUID matches the reported codename.
    let mut cpuid_check_ok = true;
    if !opts.no_cpuid_check {
        if let (Some(cn), Some(ref c)) = (codename, &cpuid) {
            if let Err(err) = verify_codename_against_cpuid(cn, c) {
                cpuid_check_ok = false;
                if driver_error.is_none() {
                    driver_error = Some(err.to_string());
                }
            }
        }
    }

    let hw = if cpuid_check_ok {
        match (&driver, codename, capabilities) {
            (Some(drv), Some(cn), Some(caps)) => Some(HardwareContext {
                driver: drv.clone(),
                codename: cn,
                cpuid: cpuid.clone(),
                topology: topology.clone().unwrap_or_else(|| CpuTopology::fallback(1)),
                capabilities: caps,
            }),
            _ => None,
        }
    } else {
        None
    };

    let mut rsmu_ping = None;
    let mut mp1_ping = None;
    let mut pbo_status = None;
    let mut pbo_error = None;
    let mut co_snapshot = None;
    let mut co_error = None;
    let mut shaper_state = None;
    let mut shaper_error = None;

    if opts.include_mailbox_probes {
        if let Some(ref hw) = hw {
            if hw.driver.has_rsmu {
                if let Some(op) = hw.capabilities.test_message_rsmu {
                    rsmu_ping = Some(run_ping_probe(transport, hw, op));
                }
            } else {
                rsmu_ping = Some("unavailable (driver reports has_rsmu=false)".to_string());
            }

            if let Some(op) = hw.capabilities.test_message_mp1 {
                mp1_ping = Some(run_ping_probe(transport, hw, op));
            }

            match pbo::get_pbo_status(transport, hw) {
                Ok(st) => pbo_status = Some(st),
                Err(err) => pbo_error = Some(err.to_string()),
            }

            if hw.capabilities.get_co_per_core.is_some() {
                match co::get_co(transport, hw) {
                    Ok(snap) => co_snapshot = Some(snap),
                    Err(err) => co_error = Some(err.to_string()),
                }
            }

            if hw.capabilities.get_curve_shaper.is_some() {
                match shaper::get_curve_shaper(transport, hw) {
                    Ok(state) => shaper_state = Some(state),
                    Err(err) => shaper_error = Some(err.to_string()),
                }
            }
        }
    }

    DebugReport {
        zensmu_version,
        system,
        cpuid,
        topology,
        driver,
        driver_error,
        codename,
        capabilities_supported,
        telemetry,
        telemetry_error,
        raw_pm_table_floats,
        hwmon_reference,
        rsmu_ping,
        mp1_ping,
        pbo_status,
        pbo_error,
        co_snapshot,
        co_error,
        shaper_state,
        shaper_error,
        user_note,
        secure_boot,
    }
}

fn run_ping_probe(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    op: crate::hw::profiles::OpcodeSpec,
) -> String {
    match co::send_verified(transport, hw, op, [HEALTH_CHECK_ARG, 0, 0, 0, 0, 0]) {
        Ok(out) if out[0] == HEALTH_CHECK_EXPECTED => format!("OK (0x{:04X})", out[0]),
        Ok(out) => format!(
            "unexpected response 0x{:X} (expected 0x{:X})",
            out[0], HEALTH_CHECK_EXPECTED
        ),
        Err(err) => format!("error: {err}"),
    }
}

/// Extract up to 160 non-zero finite floats in `(-100_000.0..100_000.0)` with
/// `abs() >= 0.001` from raw little-endian PM table bytes starting at `0x000`.
pub fn extract_raw_pm_table_floats(raw: &[u8]) -> Vec<PmTableFloatEntry> {
    let mut entries = Vec::new();
    for (idx, &bytes) in raw.as_chunks::<4>().0.iter().enumerate() {
        let value = f32::from_le_bytes(bytes);
        if value.is_finite() && value > -100_000.0 && value < 100_000.0 && value.abs() >= 0.001 {
            entries.push(PmTableFloatEntry {
                offset_hex: format!("0x{:03X}", idx * 4),
                value,
            });
            if entries.len() >= MAX_RAW_PM_FLOATS {
                break;
            }
        }
    }
    entries
}
