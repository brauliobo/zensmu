//! Privacy-safe, strictly read-only hardware and SMU diagnostic report generator.
//!
//! Designed so users on untested or unsupported AMD Ryzen CPUs/motherboards can
//! produce a compact Markdown report and pre-filled GitHub issue URL with all
//! diagnostic data needed to add support or debug issues — without ever sending
//! mutating SMU commands or collecting hostnames, usernames, IP/MAC addresses,
//! or hardware serial numbers.

use crate::driver::{detect_secure_boot, SecureBootStatus};
use crate::error::Error;
use crate::hw::identity::{
    read_host_cpuid, verify_codename_against_cpuid, CpuidInfo, HardwareContext,
};
use crate::hw::profiles::capabilities_for;
use crate::hw::topology::CpuTopology;
use crate::ops::co::{self, CoSnapshot, HEALTH_CHECK_ARG, HEALTH_CHECK_EXPECTED};
use crate::ops::pbo::{self, PboStatus};
use crate::ops::pm_table::{self, TelemetrySnapshot, MAX_SAFE_PM_TABLE_SIZE};
use crate::ops::shaper;
use crate::transport::{DriverSysfsInfo, SmuTransport};
use crate::types::{Codename, CurveShaperState, FreqTier};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

/// Upstream GitHub repository for issue filing.
pub const GITHUB_REPO: &str = "brauliobo/zensmu";

/// GitHub "New Issue" URL for `zensmu`.
pub const GITHUB_ISSUES_NEW_URL: &str = "https://github.com/brauliobo/zensmu/issues/new";

const ALLOWED_HWMON_CHIPS: &[&str] = &["k10temp", "zenpower", "amdgpu"];
const MAX_HWMON_READINGS: usize = 32;
const MAX_RAW_PM_FLOATS: usize = 160;
const MAX_ISSUE_URL_BYTES: usize = 6500;
const TRUNCATION_NOTE: &str =
    "\n\n(Truncated for URL length — paste full output from 'zensmu report' here)";

/// Privacy-safe OS, kernel, motherboard, and BIOS metadata.
///
/// Only reads non-identifying fields; never reads serial numbers, UUIDs,
/// hostnames, usernames, or network addresses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemBoardInfo {
    pub kernel_release: Option<String>,
    pub os_pretty_name: Option<String>,
    pub board_vendor: Option<String>,
    pub board_name: Option<String>,
    pub bios_version: Option<String>,
    pub bios_date: Option<String>,
}

impl SystemBoardInfo {
    /// Collect OS, kernel, and non-identifying DMI board/BIOS strings from Linux sysfs/procfs.
    pub fn detect_linux() -> Self {
        Self {
            kernel_release: read_trimmed_file(Path::new("/proc/sys/kernel/osrelease")),
            os_pretty_name: fs::read_to_string("/etc/os-release")
                .ok()
                .and_then(|s| parse_os_release_pretty_name(&s)),
            board_vendor: read_trimmed_file(Path::new("/sys/devices/virtual/dmi/id/board_vendor")),
            board_name: read_trimmed_file(Path::new("/sys/devices/virtual/dmi/id/board_name")),
            bios_version: read_trimmed_file(Path::new("/sys/devices/virtual/dmi/id/bios_version")),
            bios_date: read_trimmed_file(Path::new("/sys/devices/virtual/dmi/id/bios_date")),
        }
    }
}

/// Single reference sensor reading from an allowlisted Linux hwmon chip
/// (`k10temp`, `zenpower`, or `amdgpu`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HwmonSensorReading {
    pub chip: String,
    pub label: String,
    pub value: String,
}

/// Non-zero finite float extracted from raw PM table bytes at a 4-byte aligned offset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PmTableFloatEntry {
    pub offset_hex: String,
    pub value: f32,
}

/// Options controlling what [`collect_debug_report`] probes and includes.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportOptions {
    pub include_raw_pm_table: bool,
    pub include_mailbox_probes: bool,
    pub user_note: Option<String>,
    pub no_cpuid_check: bool,
}

/// Complete privacy-safe hardware & SMU diagnostic report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugReport {
    pub zensmu_version: String,
    pub system: SystemBoardInfo,
    pub cpuid: Option<CpuidInfo>,
    pub topology: Option<CpuTopology>,
    pub driver: Option<DriverSysfsInfo>,
    pub driver_error: Option<String>,
    pub codename: Option<Codename>,
    pub capabilities_supported: bool,
    pub telemetry: Option<TelemetrySnapshot>,
    pub telemetry_error: Option<String>,
    pub raw_pm_table_floats: Vec<PmTableFloatEntry>,
    pub hwmon_reference: Vec<HwmonSensorReading>,
    pub rsmu_ping: Option<String>,
    pub mp1_ping: Option<String>,
    pub pbo_status: Option<PboStatus>,
    pub pbo_error: Option<String>,
    pub co_snapshot: Option<CoSnapshot>,
    pub co_error: Option<String>,
    pub shaper_state: Option<CurveShaperState>,
    pub shaper_error: Option<String>,
    pub user_note: Option<String>,
    #[serde(default)]
    pub secure_boot: Option<SecureBootStatus>,
}

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

fn collect_debug_report_inner(
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

impl DebugReport {
    fn cpu_label(&self) -> String {
        if let Some(ref c) = self.cpuid {
            let trimmed = c.brand.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
        if let Some(cn) = self.codename {
            return cn.display_name().to_string();
        }
        if let Some(ref drv) = self.driver {
            return format!("codename id={}", drv.codename_id);
        }
        "unknown CPU".to_string()
    }

    /// Suggest a concise GitHub issue title describing this hardware report.
    pub fn suggested_issue_title(&self) -> String {
        let cpu = self.cpu_label();
        if !self.capabilities_supported {
            if let Some(ref c) = self.cpuid {
                format!(
                    "CPU support request: {cpu} (family 0x{:X}, model 0x{:X})",
                    c.family, c.model
                )
            } else if let Some(ref drv) = self.driver {
                format!(
                    "CPU support request: {cpu} (codename id={})",
                    drv.codename_id
                )
            } else {
                format!("CPU support request: {cpu}")
            }
        } else if self.telemetry_error.is_some() {
            let ver = self
                .driver
                .as_ref()
                .and_then(|d| d.pm_table_version)
                .map(|v| format!("0x{v:08X}"))
                .unwrap_or_else(|| "unknown".to_string());
            let size = self
                .driver
                .as_ref()
                .and_then(|d| d.pm_table_size)
                .map(|s| format!("{s} bytes"))
                .unwrap_or_else(|| "unknown size".to_string());
            format!("PM table support: {cpu} (PM table {ver}, {size})")
        } else {
            let cn = self
                .codename
                .map(Codename::display_name)
                .unwrap_or("unknown");
            format!("Hardware report / issue: {cpu} ({cn})")
        }
    }

    /// Suggest GitHub issue labels for this report.
    pub fn suggested_labels(&self) -> Vec<&'static str> {
        if !self.capabilities_supported || self.telemetry_error.is_some() {
            vec!["hardware-support"]
        } else {
            vec!["bug"]
        }
    }

    /// Render the diagnostic report as clean GitHub-flavored Markdown.
    pub fn to_markdown(&self) -> String {
        let mut md = String::with_capacity(2048);

        // User notes section
        md.push_str("### Description / Notes\n\n");
        if let Some(ref note) = self.user_note {
            md.push_str(note);
            md.push_str("\n\n");
        } else {
            md.push_str("_Describe your issue, request, or observations here._\n\n");
        }

        // System & Hardware Summary table
        md.push_str("### System & Hardware Summary\n\n");
        md.push_str("| Field | Value |\n");
        md.push_str("| --- | --- |\n");

        let _ = writeln!(
            md,
            "| zensmu version | `{}` |",
            escape_md_cell(&self.zensmu_version)
        );

        let os_str = self
            .system
            .os_pretty_name
            .as_deref()
            .unwrap_or("unavailable");
        let _ = writeln!(md, "| OS | {} |", escape_md_cell(os_str));

        let kernel_str = self
            .system
            .kernel_release
            .as_deref()
            .unwrap_or("unavailable");
        let _ = writeln!(md, "| Kernel | `{}` |", escape_md_cell(kernel_str));

        if let Some(ref sb) = self.secure_boot {
            let sb_state = match sb.secure_boot_enabled {
                Some(true) => "enabled",
                Some(false) => "disabled",
                None => "unknown",
            };
            let lockdown = match sb.lockdown_mode.as_deref() {
                Some(mode) => format!("lockdown: `{}`", escape_md_cell(mode)),
                None => "lockdown: unavailable".to_string(),
            };
            let suffix = if sb.blocks_unsigned_modules {
                " (blocks unsigned modules)"
            } else {
                ""
            };
            let _ = writeln!(
                md,
                "| Secure Boot / Lockdown | {sb_state}, {lockdown}{suffix} |"
            );
        }

        let board_str = format_board_bios(&self.system);
        let _ = writeln!(
            md,
            "| Motherboard & BIOS | {} |",
            escape_md_cell(&board_str)
        );

        let brand_str = self
            .cpuid
            .as_ref()
            .map(|c| c.brand.as_str())
            .filter(|s| !s.is_empty())
            .unwrap_or("unavailable");
        let _ = writeln!(md, "| CPU Brand | {} |", escape_md_cell(brand_str));

        let cpuid_str = match self.cpuid {
            Some(ref c) => format!(
                "`{}` family `0x{:X}` model `0x{:X}` stepping `0x{:X}`",
                escape_md_cell(&c.vendor),
                c.family,
                c.model,
                c.stepping
            ),
            None => "unavailable".to_string(),
        };
        let _ = writeln!(md, "| CPUID | {cpuid_str} |");

        let codename_str = match (self.codename, &self.driver) {
            (Some(cn), Some(drv)) => format!(
                "`{}` (id `{}`, capabilities: {})",
                cn.display_name(),
                drv.codename_id,
                if self.capabilities_supported {
                    "supported"
                } else {
                    "unsupported"
                }
            ),
            (None, Some(drv)) => format!(
                "unrecognized (id `{}`, capabilities: unsupported)",
                drv.codename_id
            ),
            (Some(cn), None) => format!(
                "`{}` (capabilities: {})",
                cn.display_name(),
                if self.capabilities_supported {
                    "supported"
                } else {
                    "unsupported"
                }
            ),
            (None, None) => "unavailable".to_string(),
        };
        let _ = writeln!(md, "| Codename / ID | {codename_str} |");

        let topo_str = match self.topology {
            Some(ref t) => format!(
                "{} CCD(s), {} physical core(s), {} logical CPU(s)",
                t.ccd_count, t.physical_cores, t.logical_cpus
            ),
            None => "unavailable".to_string(),
        };
        let _ = writeln!(md, "| Topology | {topo_str} |");

        let driver_str = match &self.driver {
            Some(drv) => format!(
                "`ryzen_smu {}`, SMU `{}`, MP1 IF `{}`, RSMU: {}",
                escape_md_cell(&drv.drv_version),
                escape_md_cell(&drv.smu_version),
                drv.mp1_if_version,
                if drv.has_rsmu { "yes" } else { "no" }
            ),
            None => match &self.driver_error {
                Some(err) => format!("error: `{}`", escape_md_cell(err)),
                None => "unavailable".to_string(),
            },
        };
        let _ = writeln!(md, "| Driver / SMU | {driver_str} |");

        let pm_str = match &self.driver {
            Some(drv) if drv.has_pm_table => {
                let ver = drv
                    .pm_table_version
                    .map(|v| format!("`0x{v:08X}`"))
                    .unwrap_or_else(|| "unknown".to_string());
                let size = drv
                    .pm_table_size
                    .map(|s| format!("`{s}` bytes (`0x{s:X}`)"))
                    .unwrap_or_else(|| "unknown".to_string());
                format!("version {ver}, size {size}")
            }
            Some(_) => "not exposed by driver".to_string(),
            None => "unavailable".to_string(),
        };
        let _ = writeln!(md, "| PM Table | {pm_str} |");

        if self.driver.is_some() {
            if let Some(ref err) = self.driver_error {
                let _ = writeln!(
                    md,
                    "\n> **Driver / identity note:** `{}`",
                    escape_md_cell(err)
                );
            }
        }
        if self.driver.is_none() {
            if let Some(ref sb) = self.secure_boot {
                if sb.blocks_unsigned_modules {
                    if let Some(ref hint) = sb.hint {
                        let _ = writeln!(md, "\n> **Secure Boot / lockdown note:** {hint}");
                    }
                }
            }
        }

        // Telemetry & SMU Probes
        md.push_str("\n### Telemetry & SMU Probes\n\n");

        if let Some(ref t) = self.telemetry {
            md.push_str("- **Decoded PM Table Telemetry**:\n");
            if let Some(line) = format_limit_value("PPT", t.ppt_value_w, t.ppt_limit_w, "W") {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value("TDC", t.tdc_value_a, t.tdc_limit_a, "A") {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value("EDC", t.edc_value_a, t.edc_limit_a, "A") {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value("Tctl", t.tctl_value_c, t.tctl_limit_c, "°C") {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value("STAPM", t.stapm_value_w, t.stapm_limit_w, "W") {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value("Slow PPT", t.slow_value_w, t.slow_limit_w, "W")
            {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) =
                format_limit_value("APU Slow", t.apu_slow_value_w, t.apu_slow_limit_w, "W")
            {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) =
                format_limit_value("TDC SoC", t.tdc_soc_value_a, t.tdc_soc_limit_a, "A")
            {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) =
                format_limit_value("EDC SoC", t.edc_soc_value_a, t.edc_soc_limit_a, "A")
            {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value(
                "APU Skin Temp",
                t.apu_skin_temp_value_c,
                t.apu_skin_temp_limit_c,
                "°C",
            ) {
                let _ = writeln!(md, "  - {line}");
            }
            if let Some(line) = format_limit_value(
                "dGPU Skin Temp",
                t.dgpu_skin_temp_value_c,
                t.dgpu_skin_temp_limit_c,
                "°C",
            ) {
                let _ = writeln!(md, "  - {line}");
            }
            let clocks = format_optional_metrics(&[
                ("FCLK", t.fclk_mhz, "MHz", 0),
                ("UCLK", t.uclk_mhz, "MHz", 0),
                ("MCLK", t.mclk_mhz, "MHz", 0),
            ]);
            if !clocks.is_empty() {
                let _ = writeln!(md, "  - Clocks: {clocks}");
            }
            let rails = format_optional_metrics(&[
                ("VDDCR_SOC", t.vddcr_soc_v, "V", 3),
                ("VDDCR_MISC", t.vddcr_misc_v, "V", 3),
                ("CLDO_VDDP", t.cldo_vddp_v, "V", 3),
                ("CLDO_VDDG_IOD", t.cldo_vddg_iod_v, "V", 3),
                ("CLDO_VDDG_CCD", t.cldo_vddg_ccd_v, "V", 3),
            ]);
            if !rails.is_empty() {
                let _ = writeln!(md, "  - Voltages: {rails}");
            }
        } else if let Some(ref err) = self.telemetry_error {
            let _ = writeln!(md, "- **Telemetry**: error — `{err}`");
        } else {
            md.push_str("- **Telemetry**: unavailable\n");
        }

        let any_mailbox = self.rsmu_ping.is_some()
            || self.mp1_ping.is_some()
            || self.pbo_status.is_some()
            || self.pbo_error.is_some()
            || self.co_snapshot.is_some()
            || self.co_error.is_some()
            || self.shaper_state.is_some()
            || self.shaper_error.is_some();

        if let Some(ref ping) = self.rsmu_ping {
            let _ = writeln!(md, "- **RSMU ping (`0x01`)**: `{ping}`");
        }
        if let Some(ref ping) = self.mp1_ping {
            let _ = writeln!(md, "- **MP1 ping (`0x01`)**: `{ping}`");
        }

        if let Some(ref pbo) = self.pbo_status {
            let mut parts = Vec::new();
            if let Some(s) = pbo.scalar {
                parts.push(format!("scalar={s:.2}x"));
            }
            if let Some(fmax) = pbo.fmax_mhz {
                parts.push(format!("fmax={fmax} MHz"));
            }
            if let Some(v) = pbo.fused.ppt_w {
                parts.push(format!("fused_ppt={v}W"));
            }
            if let Some(v) = pbo.fused.vdd_tdc_a {
                parts.push(format!("fused_tdc={v}A"));
            }
            if let Some(v) = pbo.fused.edc_a {
                parts.push(format!("fused_edc={v}A"));
            }
            if let Some(v) = pbo.fused.tdp_w {
                parts.push(format!("tdp={v}W"));
            }
            if let Some(v) = pbo.fused.tjmax_c {
                parts.push(format!("tjmax={v}°C"));
            }
            if let Some(v) = pbo.flags.overclockable {
                parts.push(format!("oc={v}"));
            }
            if let Some(v) = pbo.flags.expo_active {
                parts.push(format!("expo={v}"));
            }
            if let Some(v) = pbo.flags.ln2_mode {
                parts.push(format!("ln2={v}"));
            }
            if !pbo.core_perf_scores.is_empty() {
                parts.push(format!("cppc_cores={}", pbo.core_perf_scores.len()));
            }
            let summary = if parts.is_empty() {
                "no fields reported".to_string()
            } else {
                parts.join(", ")
            };
            let _ = writeln!(md, "- **PBO Status**: {summary}");
        } else if let Some(ref err) = self.pbo_error {
            let _ = writeln!(md, "- **PBO Status**: error — `{err}`");
        }

        if let Some(ref co) = self.co_snapshot {
            let cores: Vec<String> = co
                .cores
                .iter()
                .map(|c| format!("c{}:{}", c.core_index, c.margin))
                .collect();
            let mut summary = cores.join(", ");
            if let Some(igpu) = co.igpu {
                if !summary.is_empty() {
                    summary.push_str("; ");
                }
                let _ = write!(summary, "igpu:{igpu}");
            }
            let _ = writeln!(md, "- **Curve Optimizer**: `{summary}`");
        } else if let Some(ref err) = self.co_error {
            let _ = writeln!(md, "- **Curve Optimizer**: error — `{err}`");
        }

        if let Some(ref cs) = self.shaper_state {
            let tiers: Vec<String> = FreqTier::ALL
                .iter()
                .map(|&t| {
                    let m = cs.tier(t);
                    format!("{}={}/{}/{}", t.label(), m.low, m.med, m.high)
                })
                .collect();
            let _ = writeln!(
                md,
                "- **Curve Shaper** (low/med/high temp): `{}`",
                tiers.join(", ")
            );
        } else if let Some(ref err) = self.shaper_error {
            let _ = writeln!(md, "- **Curve Shaper**: error — `{err}`");
        }

        if !any_mailbox {
            md.push_str(
                "- **SMU Mailbox Probes**: not run (enable privileged mailbox probes if needed)\n",
            );
        }

        // hwmon reference sensors
        if !self.hwmon_reference.is_empty() {
            md.push_str(
                "\n<details>\n<summary>hwmon reference sensors (k10temp/zenpower)</summary>\n\n",
            );
            md.push_str("| Chip | Sensor | Value |\n");
            md.push_str("| --- | --- | --- |\n");
            for r in &self.hwmon_reference {
                let _ = writeln!(
                    md,
                    "| `{}` | {} | {} |",
                    escape_md_cell(&r.chip),
                    escape_md_cell(&r.label),
                    escape_md_cell(&r.value)
                );
            }
            md.push_str("\n</details>\n");
        }

        // Raw PM table non-zero floats
        if !self.raw_pm_table_floats.is_empty() {
            let _ = write!(
                md,
                "\n<details>\n<summary>Raw PM table non-zero floats ({} entries)</summary>\n\n```text\n",
                self.raw_pm_table_floats.len()
            );
            for entry in &self.raw_pm_table_floats {
                let _ = writeln!(md, "{} = {:.3}", entry.offset_hex, entry.value);
            }
            md.push_str("```\n\n</details>\n");
        }

        // Topology details
        if let Some(ref topo) = self.topology {
            md.push_str("\n<details>\n<summary>Topology</summary>\n\n```text\n");
            let _ = writeln!(
                md,
                "ccds={} physical_cores={} logical_cpus={}",
                topo.ccd_count, topo.physical_cores, topo.logical_cpus
            );
            for core in &topo.cores {
                let _ = writeln!(
                    md,
                    "core {}: ccd={} ccx={} local={} cpus={:?}",
                    core.index, core.ccd, core.ccx, core.core_in_ccd, core.linux_cpus
                );
            }
            md.push_str("```\n\n</details>\n");
        }

        // Privacy footer
        let _ = write!(
            md,
            "\n---\n_Generated by zensmu v{} (strictly read-only). No hostnames, usernames, IP/MAC addresses, or hardware serial numbers were collected._\n",
            self.zensmu_version
        );

        md
    }

    /// Build a pre-filled GitHub "New Issue" URL with RFC 3986 percent-encoding.
    ///
    /// Guarantees the returned URL stays within `6500` bytes by cleanly
    /// truncating the encoded `body` (preserving the summary and errors) and
    /// appending a truncation note when necessary.
    pub fn github_issue_url(&self) -> String {
        let title = self.suggested_issue_title();
        let labels = self.suggested_labels().join(",");
        let prefix = format!(
            "{GITHUB_ISSUES_NEW_URL}?title={}&labels={}&body=",
            percent_encode(&title),
            percent_encode(&labels)
        );

        if prefix.len() >= MAX_ISSUE_URL_BYTES {
            // Pathological title fallback: cap at MAX_ISSUE_URL_BYTES on a valid boundary.
            return truncate_url_prefix(&prefix, MAX_ISSUE_URL_BYTES);
        }

        let body_budget = MAX_ISSUE_URL_BYTES - prefix.len();
        let body = self.to_markdown();
        let encoded_full = percent_encode(&body);

        if encoded_full.len() <= body_budget {
            let mut url = String::with_capacity(prefix.len() + encoded_full.len());
            url.push_str(&prefix);
            url.push_str(&encoded_full);
            return url;
        }

        let encoded_truncated = encode_truncated_body(&body, body_budget);
        let mut url = String::with_capacity(prefix.len() + encoded_truncated.len());
        url.push_str(&prefix);
        url.push_str(&encoded_truncated);
        url
    }
}

/// Zero-dependency RFC 3986 percent-encoder (`ALPHA / DIGIT / "-" / "." / "_" / "~"` unreserved).
pub fn percent_encode(input: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        if is_rfc3986_unreserved(byte) {
            out.push(byte as char);
        } else {
            out.push('%');
            out.push(HEX[(byte >> 4) as usize] as char);
            out.push(HEX[(byte & 0x0F) as usize] as char);
        }
    }
    out
}

fn is_rfc3986_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

fn encoded_char_len(ch: char) -> usize {
    let mut buf = [0u8; 4];
    let bytes = ch.encode_utf8(&mut buf).as_bytes();
    bytes
        .iter()
        .map(|&b| if is_rfc3986_unreserved(b) { 1 } else { 3 })
        .sum()
}

fn encode_truncated_body(body: &str, budget: usize) -> String {
    let encoded_note = percent_encode(TRUNCATION_NOTE);
    if budget <= encoded_note.len() {
        return truncate_encoded_to_budget(TRUNCATION_NOTE, budget);
    }

    let content_budget = budget - encoded_note.len();
    let mut enc_len = 0usize;
    let mut cut_idx = 0usize;
    let mut last_nl = None;

    for (idx, ch) in body.char_indices() {
        let ch_len = encoded_char_len(ch);
        if enc_len + ch_len > content_budget {
            break;
        }
        enc_len += ch_len;
        cut_idx = idx + ch.len_utf8();
        if ch == '\n' {
            last_nl = Some(idx);
        }
    }

    let mut slice = match last_nl {
        Some(nl) if nl >= cut_idx / 2 => &body[..nl],
        _ => &body[..cut_idx],
    };

    // If truncation fell inside an unclosed `<details>` section, back up to before
    // `<details>` so the summary + errors stay intact and the note is visible at top level.
    if let Some(open_pos) = slice.rfind("<details>") {
        if !slice[open_pos..].contains("</details>") && open_pos > 0 {
            slice = slice[..open_pos].trim_end();
        }
    }

    let mut out = percent_encode(slice);
    out.push_str(&encoded_note);
    out
}

fn truncate_encoded_to_budget(text: &str, budget: usize) -> String {
    let mut enc_len = 0usize;
    let mut cut_idx = 0usize;
    for (idx, ch) in text.char_indices() {
        let ch_len = encoded_char_len(ch);
        if enc_len + ch_len > budget {
            break;
        }
        enc_len += ch_len;
        cut_idx = idx + ch.len_utf8();
    }
    percent_encode(&text[..cut_idx])
}

fn truncate_url_prefix(prefix: &str, max_len: usize) -> String {
    let mut end = max_len.min(prefix.len());
    // Avoid splitting an ASCII `%XX` escape sequence.
    if end >= 1 && prefix.as_bytes()[end - 1] == b'%' {
        end -= 1;
    } else if end >= 2 && prefix.as_bytes()[end - 2] == b'%' {
        end -= 2;
    }
    prefix[..end].to_string()
}

fn read_trimmed_file(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_os_release_pretty_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("PRETTY_NAME=") {
            let unquoted = rest
                .trim()
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| {
                    rest.trim()
                        .strip_prefix('\'')
                        .and_then(|s| s.strip_suffix('\''))
                })
                .unwrap_or_else(|| rest.trim());
            let clean = unquoted.trim();
            if !clean.is_empty() {
                return Some(clean.to_string());
            }
        }
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SensorKind {
    Temp = 0,
    Power = 1,
    Curr = 2,
    In = 3,
}

struct ParsedSensorFile {
    kind: SensorKind,
    channel: u32,
    is_average: bool,
    prefix: String,
    path: PathBuf,
}

fn parse_hwmon_sensor_file(file_name: &str, path: PathBuf) -> Option<ParsedSensorFile> {
    let (prefix, is_average) = if let Some(p) = file_name.strip_suffix("_input") {
        (p, false)
    } else {
        (file_name.strip_suffix("_average")?, true)
    };

    let (kind, rest) = if let Some(r) = prefix.strip_prefix("temp") {
        if is_average {
            return None;
        }
        (SensorKind::Temp, r)
    } else if let Some(r) = prefix.strip_prefix("power") {
        (SensorKind::Power, r)
    } else if let Some(r) = prefix.strip_prefix("curr") {
        if is_average {
            return None;
        }
        (SensorKind::Curr, r)
    } else {
        let r = prefix.strip_prefix("in")?;
        if is_average {
            return None;
        }
        (SensorKind::In, r)
    };

    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let channel: u32 = rest.parse().ok()?;

    Some(ParsedSensorFile {
        kind,
        channel,
        is_average,
        prefix: prefix.to_string(),
        path,
    })
}

fn format_hwmon_value(kind: SensorKind, raw: i64) -> String {
    match kind {
        SensorKind::Temp => format!("{:.1} °C", raw as f64 / 1000.0),
        SensorKind::Curr => format!("{:.1} A", raw as f64 / 1000.0),
        SensorKind::In => format!("{:.3} V", raw as f64 / 1000.0),
        SensorKind::Power => format!("{:.1} W", raw as f64 / 1_000_000.0),
    }
}

fn scan_hwmon_dir(hwmon_root: &Path) -> Vec<HwmonSensorReading> {
    let Ok(entries) = fs::read_dir(hwmon_root) else {
        return Vec::new();
    };

    let mut dirs: Vec<(u32, String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(rest) = name.strip_prefix("hwmon") {
            let num = rest.parse::<u32>().unwrap_or(u32::MAX);
            dirs.push((num, name, entry.path()));
        }
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut readings = Vec::new();
    for (_, _, dir) in dirs {
        if readings.len() >= MAX_HWMON_READINGS {
            break;
        }
        let Some(chip) = read_trimmed_file(&dir.join("name")) else {
            continue;
        };
        if !ALLOWED_HWMON_CHIPS.contains(&chip.as_str()) {
            continue;
        }

        let Ok(files) = fs::read_dir(&dir) else {
            continue;
        };

        let mut sensors: Vec<ParsedSensorFile> = files
            .flatten()
            .filter_map(|e| {
                let fname = e.file_name().to_string_lossy().into_owned();
                parse_hwmon_sensor_file(&fname, e.path())
            })
            .collect();
        // Sort deterministically by kind, channel, and `_input` before `_average`.
        sensors.sort_by_key(|s| (s.kind, s.channel, s.is_average));

        let mut recorded_power_channels: HashSet<u32> = HashSet::new();
        for sensor in sensors {
            if readings.len() >= MAX_HWMON_READINGS {
                break;
            }
            if sensor.kind == SensorKind::Power
                && sensor.is_average
                && recorded_power_channels.contains(&sensor.channel)
            {
                continue;
            }

            let Some(raw_str) = read_trimmed_file(&sensor.path) else {
                continue;
            };
            let Ok(raw_val) = raw_str.parse::<i64>() else {
                continue;
            };

            if sensor.kind == SensorKind::Power {
                recorded_power_channels.insert(sensor.channel);
            }

            let label = read_trimmed_file(&dir.join(format!("{}_label", sensor.prefix)))
                .unwrap_or(sensor.prefix);

            readings.push(HwmonSensorReading {
                chip: chip.clone(),
                label,
                value: format_hwmon_value(sensor.kind, raw_val),
            });
        }
    }

    readings
}

fn format_board_bios(sys: &SystemBoardInfo) -> String {
    let mut board = String::new();
    if let Some(ref vendor) = sys.board_vendor {
        board.push_str(vendor);
    }
    if let Some(ref name) = sys.board_name {
        if !board.is_empty() {
            board.push(' ');
        }
        board.push_str(name);
    }

    let mut bios_parts = Vec::new();
    if let Some(ref ver) = sys.bios_version {
        bios_parts.push(format!("BIOS {ver}"));
    }
    if let Some(ref date) = sys.bios_date {
        bios_parts.push(date.clone());
    }

    match (board.is_empty(), bios_parts.is_empty()) {
        (false, false) => format!("{board} ({})", bios_parts.join(", ")),
        (false, true) => board,
        (true, false) => bios_parts.join(", "),
        (true, true) => "unavailable".to_string(),
    }
}

fn format_limit_value(
    name: &str,
    value: Option<f32>,
    limit: Option<f32>,
    unit: &str,
) -> Option<String> {
    match (value, limit) {
        (Some(v), Some(l)) => Some(format!("{name}: {v:.1} / {l:.1} {unit}")),
        (Some(v), None) => Some(format!("{name}: {v:.1} {unit}")),
        (None, Some(l)) => Some(format!("{name} limit: {l:.1} {unit}")),
        (None, None) => None,
    }
}

fn format_optional_metrics(items: &[(&str, Option<f32>, &str, usize)]) -> String {
    items
        .iter()
        .filter_map(|&(label, val, unit, prec)| val.map(|v| format!("{label}={v:.prec$} {unit}")))
        .collect::<Vec<_>>()
        .join(", ")
}

fn escape_md_cell(s: &str) -> String {
    s.replace('|', "\\|").replace(['\r', '\n'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Result;
    use crate::transport::mock::MockTransport;
    use crate::transport::MailboxResponse;
    use crate::types::{Mailbox, SmuArgs};

    struct FailingTransport;

    impl SmuTransport for FailingTransport {
        fn read_info(&self) -> Result<DriverSysfsInfo> {
            Err(Error::DriverNotLoaded {
                path: "/sys/kernel/ryzen_smu_drv".into(),
            })
        }

        fn read_pm_table(&self) -> Result<Vec<u8>> {
            Err(Error::DriverNotLoaded {
                path: "/sys/kernel/ryzen_smu_drv/pm_table".into(),
            })
        }

        fn send_command(
            &self,
            _mailbox: Mailbox,
            _opcode: u32,
            _args: SmuArgs,
            _verify_rsp_smn_addr: Option<u32>,
        ) -> Result<MailboxResponse> {
            Err(Error::DriverNotLoaded {
                path: "/sys/kernel/ryzen_smu_drv".into(),
            })
        }

        fn read_smn(&self, _addr: u32) -> Result<u32> {
            Err(Error::DriverNotLoaded {
                path: "/sys/kernel/ryzen_smu_drv/smn".into(),
            })
        }
    }

    fn mock_cpuid_9700x() -> CpuidInfo {
        CpuidInfo {
            vendor: "AuthenticAMD".into(),
            brand: "AMD Ryzen 7 9700X 8-Core Processor".into(),
            family: 0x1A,
            model: 0x44,
            stepping: 0,
        }
    }

    fn mock_system_info() -> SystemBoardInfo {
        SystemBoardInfo {
            kernel_release: Some("6.12.10-arch1-1".into()),
            os_pretty_name: Some("Arch Linux".into()),
            board_vendor: Some("ASRock".into()),
            board_name: Some("B650M-HDV/M.2".into()),
            bios_version: Some("3.10".into()),
            bios_date: Some("10/24/2024".into()),
        }
    }

    #[test]
    fn collect_report_with_mock_granite_ridge_captures_all_probes_and_markdown() {
        let transport = MockTransport::granite_ridge_9700x();
        let opts = ReportOptions {
            include_raw_pm_table: true,
            include_mailbox_probes: true,
            user_note: Some("Verifying full Granite Ridge diagnostic output.".into()),
            no_cpuid_check: true,
        };

        // Deterministic full-probe verification independent of CI host core count.
        let report = collect_debug_report_inner(
            &transport,
            &opts,
            mock_system_info(),
            Some(mock_cpuid_9700x()),
            Some(CpuTopology::synthetic(1, 8, 2)),
            vec![HwmonSensorReading {
                chip: "k10temp".into(),
                label: "Tctl".into(),
                value: "54.2 °C".into(),
            }],
            Some(crate::driver::secure_boot_status_from(
                Some(false),
                Some("none".into()),
            )),
        );

        assert_eq!(report.zensmu_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(report.codename, Some(Codename::GraniteRidge));
        assert!(report.capabilities_supported);
        assert!(report.driver.is_some());
        assert!(report.driver_error.is_none());
        assert!(report.telemetry.is_some());
        assert!(report.telemetry_error.is_none());
        assert!(!report.raw_pm_table_floats.is_empty());

        assert_eq!(report.rsmu_ping.as_deref(), Some("OK (0x1338)"));
        assert_eq!(report.mp1_ping.as_deref(), Some("OK (0x1338)"));
        assert!(report.pbo_status.is_some());
        assert!(report.pbo_error.is_none());
        let pbo = report.pbo_status.as_ref().unwrap();
        assert_eq!(pbo.fmax_mhz, Some(5550));
        assert_eq!(pbo.scalar, Some(1.0));
        assert!(report.co_snapshot.is_some());
        assert_eq!(report.co_snapshot.as_ref().unwrap().cores.len(), 8);
        assert!(report.co_error.is_none());
        assert!(report.shaper_state.is_some());
        assert!(report.shaper_error.is_none());

        assert_eq!(
            report.suggested_issue_title(),
            "Hardware report / issue: AMD Ryzen 7 9700X 8-Core Processor (Granite Ridge (Zen 5 Desktop))"
        );
        assert_eq!(report.suggested_labels(), vec!["bug"]);

        let md = report.to_markdown();
        assert!(md.contains("Verifying full Granite Ridge diagnostic output."));
        assert!(md.contains("ASRock B650M-HDV/M.2 (BIOS 3.10, 10/24/2024)"));
        assert!(md.contains("Granite Ridge (Zen 5 Desktop)"));
        assert!(md.contains("0x00620105"));
        assert!(md.contains("OK (0x1338)"));
        assert!(md.contains("hwmon reference sensors (k10temp/zenpower)"));
        assert!(md.contains("Raw PM table non-zero floats"));
        assert!(md.contains("<summary>Topology</summary>"));
        assert!(md.contains("No hostnames, usernames"));

        // Verify JSON serialization roundtrip.
        let json = serde_json::to_string(&report).unwrap();
        let parsed: DebugReport = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.codename, Some(Codename::GraniteRidge));
        assert_eq!(parsed.rsmu_ping.as_deref(), Some("OK (0x1338)"));

        // Also exercise the public entry point directly to confirm it never panics.
        let live_env_report = collect_debug_report(&transport, &opts);
        assert_eq!(live_env_report.codename, Some(Codename::GraniteRidge));
        assert!(live_env_report.telemetry.is_some());
        assert_eq!(live_env_report.rsmu_ping.as_deref(), Some("OK (0x1338)"));
    }

    #[test]
    fn collect_report_handles_unsupported_cpu_and_dumps_raw_pm_floats() {
        // Case 1: Supported CPU codename with unrecognized PM table version ->
        // telemetry_error is Some(...) and raw_pm_table_floats is auto-populated even with include_raw_pm_table=false.
        let mut unknown_pm_transport = MockTransport::granite_ridge_9700x();
        unknown_pm_transport.info.pm_table_version = Some(0xDEAD_BEEF);
        let opts = ReportOptions {
            include_raw_pm_table: false,
            include_mailbox_probes: false,
            user_note: None,
            no_cpuid_check: true,
        };

        let pm_report = collect_debug_report_inner(
            &unknown_pm_transport,
            &opts,
            mock_system_info(),
            Some(mock_cpuid_9700x()),
            Some(CpuTopology::synthetic(1, 8, 2)),
            Vec::new(),
            None,
        );

        assert!(pm_report.capabilities_supported);
        assert!(pm_report.telemetry.is_none());
        assert!(pm_report.telemetry_error.is_some());
        assert!(pm_report
            .telemetry_error
            .as_deref()
            .unwrap()
            .contains("0xDEADBEEF"));
        assert!(
            !pm_report.raw_pm_table_floats.is_empty(),
            "raw PM table floats must auto-populate when telemetry decoding fails"
        );
        assert!(pm_report
            .raw_pm_table_floats
            .iter()
            .any(|e| e.offset_hex == "0x008" && (e.value - 88.0).abs() < 0.01));
        assert!(pm_report
            .raw_pm_table_floats
            .iter()
            .any(|e| e.offset_hex == "0x11C" && (e.value - 2000.0).abs() < 0.01));
        assert!(pm_report
            .suggested_issue_title()
            .starts_with("PM table support:"));
        assert!(pm_report.suggested_issue_title().contains("0xDEADBEEF"));
        assert_eq!(pm_report.suggested_labels(), vec!["hardware-support"]);

        // Case 2: Completely unrecognized CPU codename + unrecognized PM table version ->
        // must not panic, must NOT send any mailbox commands, and must still dump raw PM floats.
        let mut unsupported_cpu_transport = MockTransport::granite_ridge_9700x();
        unsupported_cpu_transport.info.codename_id = 999;
        unsupported_cpu_transport.info.pm_table_version = Some(0xCAFE_0001);
        let probe_opts = ReportOptions {
            include_raw_pm_table: false,
            include_mailbox_probes: true,
            user_note: None,
            no_cpuid_check: true,
        };

        let cpu_report = collect_debug_report_inner(
            &unsupported_cpu_transport,
            &probe_opts,
            mock_system_info(),
            Some(CpuidInfo {
                vendor: "AuthenticAMD".into(),
                brand: "AMD Future Zen 6 Engineering Sample".into(),
                family: 0x1B,
                model: 0x01,
                stepping: 0,
            }),
            Some(CpuTopology::synthetic(1, 8, 2)),
            Vec::new(),
            None,
        );

        assert!(cpu_report.codename.is_none());
        assert!(!cpu_report.capabilities_supported);
        assert!(cpu_report.telemetry.is_none());
        assert!(cpu_report.telemetry_error.is_some());
        assert!(!cpu_report.raw_pm_table_floats.is_empty());
        assert!(
            unsupported_cpu_transport.history().is_empty(),
            "must never send mailbox commands to an unsupported CPU"
        );
        assert_eq!(
            cpu_report.suggested_issue_title(),
            "CPU support request: AMD Future Zen 6 Engineering Sample (family 0x1B, model 0x1)"
        );
        assert_eq!(cpu_report.suggested_labels(), vec!["hardware-support"]);

        let md = cpu_report.to_markdown();
        assert!(md.contains("Raw PM table non-zero floats"));
        assert!(md.contains("0x008 = 88.000"));

        // Case 3: Missing driver transport must never panic and must record driver_error.
        let missing_driver = collect_debug_report(&FailingTransport, &probe_opts);
        assert!(missing_driver.driver.is_none());
        assert!(missing_driver.driver_error.is_some());
        assert!(!missing_driver.capabilities_supported);
        assert!(missing_driver.raw_pm_table_floats.is_empty());
    }

    #[test]
    fn github_issue_url_encodes_properly_and_stays_under_length_limit() {
        // RFC 3986 percent-encoding sanity check
        assert_eq!(percent_encode("AZaz09-._~"), "AZaz09-._~");
        assert_eq!(
            percent_encode("a b&c=d?e#f%g\nh"),
            "a%20b%26c%3Dd%3Fe%23f%25g%0Ah"
        );
        assert_eq!(percent_encode("54.2 °C"), "54.2%20%C2%B0C");

        // Normal compact report URL
        let transport = MockTransport::granite_ridge_9700x();
        let compact = collect_debug_report_inner(
            &transport,
            &ReportOptions {
                include_raw_pm_table: false,
                include_mailbox_probes: true,
                user_note: Some("Quick test with spaces & symbols = 100%".into()),
                no_cpuid_check: true,
            },
            mock_system_info(),
            Some(mock_cpuid_9700x()),
            Some(CpuTopology::synthetic(1, 8, 2)),
            Vec::new(),
            None,
        );

        let url = compact.github_issue_url();
        assert!(url.starts_with(GITHUB_ISSUES_NEW_URL));
        assert!(url.contains("?title="));
        assert!(url.contains("&labels=bug&body="));
        assert!(!url.contains(' '));
        assert!(!url.contains('\n'));
        assert!(url.len() <= MAX_ISSUE_URL_BYTES);

        // Oversized report with 160 raw PM table floats + large user note must cleanly truncate <= 6500 bytes
        let mut oversized = compact;
        oversized.raw_pm_table_floats = (0..160usize)
            .map(|i| PmTableFloatEntry {
                offset_hex: format!("0x{:03X}", i * 4),
                value: 100.0 + i as f32 * 0.25,
            })
            .collect();
        oversized.user_note = Some("Detailed reproduction steps: ".repeat(120));

        let big_md = oversized.to_markdown();
        assert!(
            percent_encode(&big_md).len() > MAX_ISSUE_URL_BYTES,
            "test fixture must actually exceed URL limit before truncation"
        );

        let truncated_url = oversized.github_issue_url();
        assert!(
            truncated_url.len() <= MAX_ISSUE_URL_BYTES,
            "URL length {} exceeded limit {}",
            truncated_url.len(),
            MAX_ISSUE_URL_BYTES
        );
        assert!(!truncated_url.contains(' '));
        assert!(!truncated_url.contains('\n'));
        assert!(truncated_url.contains("Truncated%20for%20URL%20length"));
        assert!(truncated_url.contains("zensmu%20report"));
    }

    #[test]
    fn hwmon_scanner_filters_allowlisted_chips_and_scales_units() {
        let tmp = tempfile::tempdir().unwrap();

        let k10 = tmp.path().join("hwmon0");
        fs::create_dir(&k10).unwrap();
        fs::write(k10.join("name"), "k10temp\n").unwrap();
        fs::write(k10.join("temp1_label"), "Tctl\n").unwrap();
        fs::write(k10.join("temp1_input"), "54250\n").unwrap();

        let zen = tmp.path().join("hwmon1");
        fs::create_dir(&zen).unwrap();
        fs::write(zen.join("name"), "zenpower\n").unwrap();
        fs::write(zen.join("in1_label"), "SVI2_Core\n").unwrap();
        fs::write(zen.join("in1_input"), "1185\n").unwrap();
        fs::write(zen.join("curr1_label"), "SVI2_C_Core\n").unwrap();
        fs::write(zen.join("curr1_input"), "24500\n").unwrap();
        fs::write(zen.join("power1_label"), "SVI2_P_Core\n").unwrap();
        fs::write(zen.join("power1_input"), "29000000\n").unwrap();
        fs::write(zen.join("power1_average"), "28500000\n").unwrap();

        let gpu = tmp.path().join("hwmon2");
        fs::create_dir(&gpu).unwrap();
        fs::write(gpu.join("name"), "amdgpu\n").unwrap();
        fs::write(gpu.join("power1_average"), "12400000\n").unwrap();

        // Non-allowlisted chip must be ignored
        let other = tmp.path().join("hwmon3");
        fs::create_dir(&other).unwrap();
        fs::write(other.join("name"), "nct6775\n").unwrap();
        fs::write(other.join("temp1_input"), "40000\n").unwrap();

        let readings = scan_hwmon_dir(tmp.path());
        assert_eq!(readings.len(), 5);
        assert_eq!(
            readings[0],
            HwmonSensorReading {
                chip: "k10temp".into(),
                label: "Tctl".into(),
                value: "54.2 °C".into(),
            }
        );
        assert_eq!(
            readings[1],
            HwmonSensorReading {
                chip: "zenpower".into(),
                label: "SVI2_P_Core".into(),
                value: "29.0 W".into(),
            }
        );
        assert_eq!(
            readings[2],
            HwmonSensorReading {
                chip: "zenpower".into(),
                label: "SVI2_C_Core".into(),
                value: "24.5 A".into(),
            }
        );
        assert_eq!(
            readings[3],
            HwmonSensorReading {
                chip: "zenpower".into(),
                label: "SVI2_Core".into(),
                value: "1.185 V".into(),
            }
        );
        assert_eq!(
            readings[4],
            HwmonSensorReading {
                chip: "amdgpu".into(),
                label: "power1".into(),
                value: "12.4 W".into(),
            }
        );

        assert_eq!(
            parse_os_release_pretty_name("NAME=Arch\nPRETTY_NAME=\"Arch Linux\"\n"),
            Some("Arch Linux".into())
        );
        assert_eq!(
            parse_os_release_pretty_name("PRETTY_NAME='Ubuntu 24.04 LTS'\n"),
            Some("Ubuntu 24.04 LTS".into())
        );
    }

    #[test]
    fn report_renders_secure_boot_and_deserializes_legacy_json() {
        let sb_blocking =
            crate::driver::secure_boot_status_from(Some(true), Some("integrity".into()));
        let report = collect_debug_report_inner(
            &FailingTransport,
            &ReportOptions::default(),
            mock_system_info(),
            Some(mock_cpuid_9700x()),
            Some(CpuTopology::synthetic(1, 8, 2)),
            Vec::new(),
            Some(sb_blocking),
        );
        let md = report.to_markdown();
        assert!(md.contains(
            "| Secure Boot / Lockdown | enabled, lockdown: `integrity` (blocks unsigned modules) |"
        ));
        assert!(md.contains("**Secure Boot / lockdown note:**"));
        assert!(md.contains("mokutil --import"));

        // Verify legacy JSON without `secure_boot` still deserializes cleanly
        let mut val = serde_json::to_value(&report).unwrap();
        val.as_object_mut().unwrap().remove("secure_boot");
        let parsed: DebugReport = serde_json::from_value(val).unwrap();
        assert!(parsed.secure_boot.is_none());
    }
}
