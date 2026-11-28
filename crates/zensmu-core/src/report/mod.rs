//! Privacy-safe, strictly read-only hardware and SMU diagnostic report generator.
//!
//! Designed so users on untested or unsupported AMD Ryzen CPUs/motherboards can
//! produce a compact Markdown report and pre-filled GitHub issue URL with all
//! diagnostic data needed to add support or debug issues — without ever sending
//! mutating SMU commands or collecting hostnames, usernames, IP/MAC addresses,
//! or hardware serial numbers.

mod collect;
mod hwmon;
mod markdown;
mod system;
#[cfg(test)]
mod tests;
mod url;

use crate::driver::SecureBootStatus;
use crate::hw::identity::CpuidInfo;
use crate::hw::topology::CpuTopology;
use crate::ops::co::CoSnapshot;
use crate::ops::pbo::PboStatus;
use crate::ops::pm_table::TelemetrySnapshot;
use crate::transport::DriverSysfsInfo;
use crate::types::{Codename, CurveShaperState};
use serde::{Deserialize, Serialize};

pub use collect::{collect_debug_report, extract_raw_pm_table_floats};
pub use hwmon::HwmonSensorReading;
pub use system::SystemBoardInfo;
pub use url::percent_encode;

/// Upstream GitHub repository for issue filing.
pub const GITHUB_REPO: &str = "brauliobo/zensmu";

/// GitHub "New Issue" URL for `zensmu`.
pub const GITHUB_ISSUES_NEW_URL: &str = "https://github.com/brauliobo/zensmu/issues/new";

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
