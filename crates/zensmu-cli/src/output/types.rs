//! Structured JSON output schemas for CLI subcommands.

use serde::Serialize;
use zensmu_core::config::profile::ProfileDiffEntry;
use zensmu_core::hw::identity::{driver_has_false_ok_bug, CpuidInfo, HardwareContext};
use zensmu_core::hw::profiles::CoBounds;
use zensmu_core::hw::topology::CpuTopology;
use zensmu_core::metrics::ExporterServiceStatus;
use zensmu_core::ops::co::{CoSnapshot, CoreCoMargin};
use zensmu_core::ops::pbo::PboStatus;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::recorder::RecordServiceStatus;
use zensmu_core::report::DebugReport;
use zensmu_core::safety::persist::PersistStatus;
use zensmu_core::transport::DriverSysfsInfo;
use zensmu_core::types::{Codename, CurveShaperState, PlannedOp};

#[derive(Debug, Serialize)]
pub struct CapabilityItem {
    pub name: String,
    pub supported: bool,
}

#[derive(Debug, Serialize)]
pub struct InfoOutput {
    pub codename: Codename,
    pub codename_name: String,
    pub cpuid: Option<CpuidInfo>,
    pub driver: DriverSysfsInfo,
    pub topology: CpuTopology,
    pub capabilities: Vec<CapabilityItem>,
    pub co_bounds: Option<CoBounds>,
    pub false_ok_bug_affected: bool,
}

impl InfoOutput {
    pub fn from_hw(hw: &HardwareContext) -> Self {
        let capabilities = hw
            .capabilities
            .feature_summary()
            .into_iter()
            .map(|(name, supported)| CapabilityItem {
                name: name.to_string(),
                supported,
            })
            .collect();
        Self {
            codename: hw.codename,
            codename_name: hw.codename.display_name().to_string(),
            cpuid: hw.cpuid.clone(),
            driver: hw.driver.clone(),
            topology: hw.topology.clone(),
            capabilities,
            co_bounds: hw.capabilities.co_bounds,
            false_ok_bug_affected: driver_has_false_ok_bug(&hw.driver.drv_version),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct StatusOutput {
    pub codename: Codename,
    pub codename_name: String,
    pub is_root: bool,
    pub telemetry: Option<TelemetrySnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub telemetry_error: Option<String>,
    pub co: Option<CoSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub co_error: Option<String>,
    pub curve_shaper: Option<CurveShaperState>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub curve_shaper_error: Option<String>,
    pub pbo: Option<PboStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pbo_error: Option<String>,
    pub guard_tripped: bool,
    pub notes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CoGetOutput {
    pub codename: Codename,
    pub cores: Vec<CoreCoMargin>,
    pub igpu: Option<i16>,
}

#[derive(Debug, Serialize)]
pub struct ShaperGetOutput {
    pub codename: Codename,
    pub shaper: CurveShaperState,
}

#[derive(Debug, Default, Serialize)]
pub struct ActivePboLimits {
    pub ppt_w: Option<f32>,
    pub tdc_a: Option<f32>,
    pub edc_a: Option<f32>,
    pub tctl_c: Option<f32>,
}

impl ActivePboLimits {
    pub fn from_telemetry(t: &TelemetrySnapshot) -> Self {
        Self {
            ppt_w: t.ppt_limit_w,
            tdc_a: t.tdc_limit_a,
            edc_a: t.edc_limit_a,
            tctl_c: t.tctl_limit_c,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct PboGetOutput {
    pub codename: Codename,
    pub active_limits: ActivePboLimits,
    pub status: PboStatus,
}

#[derive(Debug, Serialize)]
pub struct TelemetryDumpOutput {
    pub path: String,
    pub bytes: usize,
    pub table_version: Option<u32>,
}

#[derive(Debug, Serialize)]
pub struct RecordServiceActionOutput {
    pub action: String,
    pub dry_run: bool,
    pub status: RecordServiceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ExporterServiceActionOutput {
    pub action: String,
    pub dry_run: bool,
    pub status: ExporterServiceStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RecordExportPtsOutput {
    pub input: String,
    pub output: String,
    pub samples: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pts_name: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct MutationOutput {
    pub action: String,
    pub dry_run: bool,
    pub applied: bool,
    pub rolled_back: bool,
    pub confirmed: Option<bool>,
    pub operations: Vec<PlannedOp>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProfileSaveOutput {
    pub path: String,
    pub codename: Codename,
    pub physical_cores: usize,
}

#[derive(Debug, Serialize)]
pub struct ProfileValidateOutput {
    pub path: String,
    pub valid: bool,
    pub target_codename: Codename,
    pub target_physical_cores: Option<usize>,
    pub host_codename: Option<Codename>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProfileDiffOutput {
    pub path: String,
    pub identical: bool,
    pub diffs: Vec<ProfileDiffEntry>,
}

#[derive(Debug, Serialize)]
pub struct ProfileListItem {
    pub name: String,
    pub path: String,
    pub codename: Option<Codename>,
    pub valid: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ProfileListOutput {
    pub dir: String,
    pub profiles: Vec<ProfileListItem>,
}

#[derive(Debug, Serialize)]
pub struct ClearGuardOutput {
    pub guard_path: String,
    pub was_tripped: bool,
    pub cleared: bool,
}

#[derive(Debug, Serialize)]
pub struct PersistActionOutput {
    pub action: String,
    pub dry_run: bool,
    pub active_profile_path: String,
    pub service_enabled: bool,
    pub status: PersistStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReportOutput {
    pub report: DebugReport,
    pub issue_title: String,
    pub issue_labels: Vec<String>,
    pub issue_url: String,
    pub markdown: String,
}

#[derive(Debug, Serialize)]
pub struct ErrorOutput {
    pub ok: bool,
    pub kind: &'static str,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}
