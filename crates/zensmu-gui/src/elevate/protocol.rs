//! Elevation environment checks, command specifications, and IPC payload types.

use super::action::CliAction;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use zensmu_core::driver::DriverActionOutput;
use zensmu_core::ops::co::CoSnapshot;
use zensmu_core::ops::pbo::PboStatus;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::report::DebugReport;
use zensmu_core::safety::persist::PersistStatus;
use zensmu_core::types::{CurveShaperState, PlannedOp};

/// Fallback system path matching `packaging/polkit/org.zensmu.policy`.
pub const SYSTEM_ZENSMU_PATH: &str = "/usr/bin/zensmu";

/// Detect whether the current process is running with real or effective UID 0.
///
/// Parses `/proc/self/status` safely without requiring `unsafe` or `libc`.
pub fn is_running_as_root() -> bool {
    let Ok(status) = fs::read_to_string("/proc/self/status") else {
        return false;
    };
    parse_is_root_from_proc_status(&status)
}

/// Pure helper to parse `Uid:` from `/proc/self/status` contents.
pub fn parse_is_root_from_proc_status(status: &str) -> bool {
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            let mut parts = rest.split_whitespace();
            let ruid = parts.next().and_then(|s| s.parse::<u32>().ok());
            let euid = parts.next().and_then(|s| s.parse::<u32>().ok());
            return ruid == Some(0) || euid == Some(0);
        }
    }
    false
}

/// Locate the `zensmu` CLI binary:
/// 1. Sibling directory of the current `zensmu-gui` executable.
/// 2. Directories listed in `$PATH`.
/// 3. Fallback to `/usr/bin/zensmu`.
pub fn find_zensmu_binary() -> PathBuf {
    let exe_dir = env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(Path::to_path_buf));
    let path_var = env::var("PATH").ok();
    find_zensmu_binary_with(exe_dir.as_deref(), path_var.as_deref())
}

/// Testable binary locator helper.
pub fn find_zensmu_binary_with(exe_dir: Option<&Path>, path_var: Option<&str>) -> PathBuf {
    if let Some(dir) = exe_dir {
        let sibling = dir.join("zensmu");
        if sibling.is_file() {
            return sibling;
        }
    }

    if let Some(paths) = path_var {
        for dir in env::split_paths(paths) {
            let candidate = dir.join("zensmu");
            if candidate.is_file() {
                return candidate;
            }
        }
    }

    PathBuf::from(SYSTEM_ZENSMU_PATH)
}

/// Concrete OS command specification built from a `CliAction` argument vector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub uses_pkexec: bool,
}

/// Build the exact program + argument vector for spawning `zensmu` directly or via `pkexec`.
pub fn build_command_spec(
    zensmu_bin: &Path,
    cli_args: &[String],
    elevate: bool,
    is_root: bool,
) -> CommandSpec {
    if elevate && !is_root {
        let mut args = Vec::with_capacity(cli_args.len() + 1);
        args.push(zensmu_bin.display().to_string());
        args.extend(cli_args.iter().cloned());
        CommandSpec {
            program: PathBuf::from("pkexec"),
            args,
            uses_pkexec: true,
        }
    } else {
        CommandSpec {
            program: zensmu_bin.to_path_buf(),
            args: cli_args.to_vec(),
            uses_pkexec: false,
        }
    }
}

/// Combined hardware SMU state parsed from `zensmu status --json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StatusPayload {
    pub co: Option<CoSnapshot>,
    pub shaper: Option<CurveShaperState>,
    pub pbo: Option<PboStatus>,
    pub telemetry: Option<TelemetrySnapshot>,
}

/// Response payload parsed from `zensmu persist save|enable|disable --json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistActionOutput {
    pub action: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub active_profile_path: String,
    #[serde(default)]
    pub service_enabled: bool,
    pub status: PersistStatus,
    #[serde(default)]
    pub note: Option<String>,
}

/// Diagnostic hardware & SMU report payload parsed from `zensmu report --json`
/// or constructed directly from [`DebugReport`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportPayload {
    pub report: DebugReport,
    pub issue_title: String,
    pub issue_url: String,
    pub markdown: String,
}

impl ReportPayload {
    /// Construct a [`ReportPayload`] directly from an in-process [`DebugReport`].
    pub fn from_report(report: DebugReport) -> Self {
        let issue_title = report.suggested_issue_title();
        let issue_url = report.github_issue_url();
        let markdown = report.to_markdown();
        Self {
            report,
            issue_title,
            issue_url,
            markdown,
        }
    }
}

/// Structured error decoded from a failed `zensmu` CLI invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliError {
    /// Motherboard/BIOS firmware clamped one or more requested power/current/thermal limits.
    HardwareClamped {
        message: String,
        caps: Vec<(String, u32)>,
    },
    /// Any other CLI, PolicyKit, or transport error.
    Generic(String),
}

impl CliError {
    /// Return the human-readable error message.
    #[cfg(test)]
    pub fn into_message(self) -> String {
        match self {
            Self::HardwareClamped { message, .. } | Self::Generic(message) => message,
        }
    }
}

/// Result delivered asynchronously from the background CLI worker thread back to the GUI.
#[derive(Debug, Clone)]
pub enum ElevatedResult {
    Status(Box<StatusPayload>),
    Co(CoSnapshot),
    Shaper(CurveShaperState),
    Pbo(PboStatus),
    Report(Box<ReportPayload>),
    DryRunPlan {
        title: String,
        live_action: CliAction,
        ops: Vec<PlannedOp>,
    },
    Applied {
        title: String,
        ops: Vec<PlannedOp>,
    },
    ProfileExported {
        path: PathBuf,
        toml: String,
    },
    PersistUpdated {
        action: String,
        status: PersistStatus,
        note: Option<String>,
    },
    GuardCleared {
        cleared: bool,
    },
    DriverLoaded(Box<DriverActionOutput>),
    HardwareClamped {
        message: String,
        caps: Vec<(String, u32)>,
    },
    Error {
        title: String,
        message: String,
    },
}
