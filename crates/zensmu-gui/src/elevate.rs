//! Narrow privilege elevation (`pkexec`) and asynchronous `zensmu` CLI bridge.
//!
//! `zensmu-gui` runs unprivileged by default and never touches privileged sysfs
//! mailbox endpoints directly. Instead, privileged mailbox reads and writes are
//! delegated to the `zensmu` CLI binary via PolicyKit (`pkexec`) on a background
//! thread so the UI never blocks, and every write is preceded by a `--dry-run --json`
//! preview before the user confirms.

use eframe::egui;
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use zensmu_core::config::profile::ProfileDiffEntry;
use zensmu_core::ops::apu::ApuSetRequest;
use zensmu_core::ops::co::{CoSnapshot, CoreCoMargin};
use zensmu_core::ops::pbo::{PboSetRequest, PboStatus};
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::safety::persist::PersistStatus;
use zensmu_core::safety::snapshot::SmuStateSnapshot;
use zensmu_core::types::{ApuPowerMode, CurveShaperState, FreqTier, PlannedOp, ShaperTierMargins};

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

/// Structured representation of a `zensmu` CLI invocation requested by the GUI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CliAction {
    /// `zensmu status --json`
    Status,
    /// `zensmu co get --json`
    CoGet,
    /// `zensmu shaper get --json`
    ShaperGet,
    /// `zensmu pbo get --json`
    PboGet,
    /// `zensmu co set --all=<margin>`
    CoSetAll {
        margin: i32,
        allow_extreme: bool,
        dry_run: bool,
    },
    /// `zensmu co set --core <spec>`
    CoSetCores {
        updates: Vec<(u32, i32)>,
        allow_extreme: bool,
        dry_run: bool,
    },
    /// `zensmu co set --igpu=<margin>`
    CoSetIgpu {
        margin: i32,
        allow_extreme: bool,
        dry_run: bool,
    },
    /// `zensmu co reset`
    CoReset { dry_run: bool },
    /// `zensmu shaper set --freq <tier> --temp low=..,med=..,high=..`
    ShaperSetTier {
        tier: FreqTier,
        margins: ShaperTierMargins,
        allow_extreme: bool,
        dry_run: bool,
    },
    /// Batch of Curve Shaper tier updates (executed sequentially).
    ShaperSetAll {
        state: CurveShaperState,
        allow_extreme: bool,
        dry_run: bool,
    },
    /// `zensmu shaper reset`
    ShaperReset { dry_run: bool },
    /// `zensmu pbo set ...`
    PboSet {
        req: PboSetRequest,
        allow_extreme: bool,
        dry_run: bool,
    },
    /// `zensmu pbo reset`
    PboReset { dry_run: bool },
    /// `zensmu apu set ...`
    ApuSet {
        req: ApuSetRequest,
        allow_extreme: bool,
        dry_run: bool,
    },
    /// `zensmu profile apply <path>`
    ProfileApply {
        path: PathBuf,
        allow_extreme: bool,
        dry_run: bool,
    },
    /// `zensmu profile export <path>`
    ProfileExport { path: PathBuf },
    /// `zensmu rollback`
    Rollback { dry_run: bool },
    /// `zensmu reset`
    Reset { dry_run: bool },
    /// `zensmu persist save [--from <path>] [--enable] [--allow-extreme] --json`
    PersistSave {
        from_path: Option<String>,
        enable: bool,
        allow_extreme: bool,
    },
    /// `zensmu persist enable|disable [--remove-profile] [--allow-extreme] --json`
    PersistSetService {
        enable: bool,
        remove_profile: bool,
        allow_extreme: bool,
    },
    /// `zensmu profile clear-guard --json`
    ClearBootGuard,
}

impl CliAction {
    /// Human-readable title for progress indicator and modals.
    pub fn title(&self) -> String {
        match self {
            Self::Status => "Refresh SMU Status".into(),
            Self::CoGet => "Read Curve Optimizer".into(),
            Self::ShaperGet => "Read Curve Shaper".into(),
            Self::PboGet => "Read PBO Status".into(),
            Self::CoSetAll {
                margin, dry_run, ..
            } => {
                if *dry_run {
                    format!("Preview All-Core CO ({margin:+})")
                } else {
                    format!("Apply All-Core CO ({margin:+})")
                }
            }
            Self::CoSetCores {
                updates, dry_run, ..
            } => {
                if *dry_run {
                    format!("Preview Per-Core CO ({} cores)", updates.len())
                } else {
                    format!("Apply Per-Core CO ({} cores)", updates.len())
                }
            }
            Self::CoSetIgpu {
                margin, dry_run, ..
            } => {
                if *dry_run {
                    format!("Preview iGPU CO ({margin:+})")
                } else {
                    format!("Apply iGPU CO ({margin:+})")
                }
            }
            Self::CoReset { dry_run } => {
                if *dry_run {
                    "Preview Reset Curve Optimizer to 0".into()
                } else {
                    "Reset Curve Optimizer to 0".into()
                }
            }
            Self::ShaperSetTier { tier, dry_run, .. } => {
                if *dry_run {
                    format!("Preview Curve Shaper ({})", tier.label())
                } else {
                    format!("Apply Curve Shaper ({})", tier.label())
                }
            }
            Self::ShaperSetAll { dry_run, .. } => {
                if *dry_run {
                    "Preview Full Curve Shaper Matrix".into()
                } else {
                    "Apply Full Curve Shaper Matrix".into()
                }
            }
            Self::ShaperReset { dry_run } => {
                if *dry_run {
                    "Preview Reset Curve Shaper to 0".into()
                } else {
                    "Reset Curve Shaper to 0".into()
                }
            }
            Self::PboSet { dry_run, .. } => {
                if *dry_run {
                    "Preview PBO Limits".into()
                } else {
                    "Apply PBO Limits".into()
                }
            }
            Self::PboReset { dry_run } => {
                if *dry_run {
                    "Preview Reset PBO Limits".into()
                } else {
                    "Reset PBO Limits".into()
                }
            }
            Self::ApuSet { dry_run, .. } => {
                if *dry_run {
                    "Preview APU DPTC Limits".into()
                } else {
                    "Apply APU DPTC Limits".into()
                }
            }
            Self::ProfileApply { path, dry_run, .. } => {
                let name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("profile.toml");
                if *dry_run {
                    format!("Preview Profile ({name})")
                } else {
                    format!("Apply Profile ({name})")
                }
            }
            Self::ProfileExport { .. } => "Export Current SMU Profile".into(),
            Self::Rollback { dry_run } => {
                if *dry_run {
                    "Preview Rollback to Last State".into()
                } else {
                    "Rollback to Last State".into()
                }
            }
            Self::Reset { dry_run } => {
                if *dry_run {
                    "Preview Reset to Boot Baseline".into()
                } else {
                    "Reset to Boot Baseline".into()
                }
            }
            Self::PersistSave {
                from_path, enable, ..
            } => match (from_path.is_some(), *enable) {
                (true, true) => "Persist Editor TOML & Enable Boot Service".into(),
                (true, false) => "Persist Editor TOML Profile".into(),
                (false, true) => "Persist Live SMU & Enable Boot Service".into(),
                (false, false) => "Persist Live SMU Profile".into(),
            },
            Self::PersistSetService {
                enable,
                remove_profile,
                ..
            } => match (*enable, *remove_profile) {
                (true, _) => "Enable Boot Service".into(),
                (false, true) => "Disable Boot Service & Remove Profile".into(),
                (false, false) => "Disable Boot Service".into(),
            },
            Self::ClearBootGuard => "Clear Boot-Loop Crash Guard".into(),
        }
    }

    /// Return the live (non-dry-run) counterpart of a dry-run action.
    pub fn into_live(self) -> Self {
        match self {
            Self::CoSetAll {
                margin,
                allow_extreme,
                ..
            } => Self::CoSetAll {
                margin,
                allow_extreme,
                dry_run: false,
            },
            Self::CoSetCores {
                updates,
                allow_extreme,
                ..
            } => Self::CoSetCores {
                updates,
                allow_extreme,
                dry_run: false,
            },
            Self::CoSetIgpu {
                margin,
                allow_extreme,
                ..
            } => Self::CoSetIgpu {
                margin,
                allow_extreme,
                dry_run: false,
            },
            Self::CoReset { .. } => Self::CoReset { dry_run: false },
            Self::ShaperSetTier {
                tier,
                margins,
                allow_extreme,
                ..
            } => Self::ShaperSetTier {
                tier,
                margins,
                allow_extreme,
                dry_run: false,
            },
            Self::ShaperSetAll {
                state,
                allow_extreme,
                ..
            } => Self::ShaperSetAll {
                state,
                allow_extreme,
                dry_run: false,
            },
            Self::ShaperReset { .. } => Self::ShaperReset { dry_run: false },
            Self::PboSet {
                req, allow_extreme, ..
            } => Self::PboSet {
                req,
                allow_extreme,
                dry_run: false,
            },
            Self::PboReset { .. } => Self::PboReset { dry_run: false },
            Self::ApuSet {
                req, allow_extreme, ..
            } => Self::ApuSet {
                req,
                allow_extreme,
                dry_run: false,
            },
            Self::ProfileApply {
                path,
                allow_extreme,
                ..
            } => Self::ProfileApply {
                path,
                allow_extreme,
                dry_run: false,
            },
            Self::Rollback { .. } => Self::Rollback { dry_run: false },
            Self::Reset { .. } => Self::Reset { dry_run: false },
            other => other,
        }
    }

    /// Whether this action is a dry-run preview.
    pub fn is_dry_run(&self) -> bool {
        matches!(
            self,
            Self::CoSetAll { dry_run: true, .. }
                | Self::CoSetCores { dry_run: true, .. }
                | Self::CoSetIgpu { dry_run: true, .. }
                | Self::CoReset { dry_run: true }
                | Self::ShaperSetTier { dry_run: true, .. }
                | Self::ShaperSetAll { dry_run: true, .. }
                | Self::ShaperReset { dry_run: true }
                | Self::PboSet { dry_run: true, .. }
                | Self::PboReset { dry_run: true }
                | Self::ApuSet { dry_run: true, .. }
                | Self::ProfileApply { dry_run: true, .. }
                | Self::Rollback { dry_run: true }
                | Self::Reset { dry_run: true }
        )
    }

    /// Whether this action requires root privileges via `pkexec` on first attempt.
    ///
    /// Dry-run previews that only inspect read-only `0444` sysfs attributes can try
    /// running unprivileged first (falling back to `pkexec` only if the CLI requests
    /// elevated permissions), avoiding unnecessary double authentication prompts.
    pub fn requires_privilege(&self) -> bool {
        match self {
            Self::Status
            | Self::CoGet
            | Self::ShaperGet
            | Self::PboGet
            | Self::ProfileExport { .. } => true,
            Self::Rollback { .. }
            | Self::Reset { .. }
            | Self::PboReset { .. }
            | Self::PersistSave { .. }
            | Self::PersistSetService { .. }
            | Self::ClearBootGuard => true,
            action => !action.is_dry_run(),
        }
    }

    /// Build the CLI argument list(s) passed to `zensmu`.
    ///
    /// Most actions produce a single CLI invocation; `ShaperSetAll` expands to one
    /// `shaper set` invocation per frequency tier so every tier is planned and verified.
    pub fn to_arg_batches(&self) -> Vec<Vec<String>> {
        match self {
            Self::Status => vec![vec!["status".into(), "--json".into()]],
            Self::CoGet => vec![vec!["co".into(), "get".into(), "--json".into()]],
            Self::ShaperGet => vec![vec!["shaper".into(), "get".into(), "--json".into()]],
            Self::PboGet => vec![vec!["pbo".into(), "get".into(), "--json".into()]],
            Self::CoSetAll {
                margin,
                allow_extreme,
                dry_run,
            } => {
                let mut args = vec!["co".into(), "set".into(), format!("--all={margin}")];
                append_write_flags(&mut args, *allow_extreme, *dry_run);
                args.push("--json".into());
                vec![args]
            }
            Self::CoSetCores {
                updates,
                allow_extreme,
                dry_run,
            } => {
                let spec = updates
                    .iter()
                    .map(|(core, margin)| format!("{core}={margin}"))
                    .collect::<Vec<_>>()
                    .join(",");
                let mut args = vec!["co".into(), "set".into(), "--core".into(), spec];
                append_write_flags(&mut args, *allow_extreme, *dry_run);
                args.push("--json".into());
                vec![args]
            }
            Self::CoSetIgpu {
                margin,
                allow_extreme,
                dry_run,
            } => {
                let mut args = vec!["co".into(), "set".into(), format!("--igpu={margin}")];
                append_write_flags(&mut args, *allow_extreme, *dry_run);
                args.push("--json".into());
                vec![args]
            }
            Self::CoReset { dry_run } => {
                let mut args = vec!["co".into(), "reset".into()];
                append_write_flags(&mut args, false, *dry_run);
                args.push("--json".into());
                vec![args]
            }
            Self::ShaperSetTier {
                tier,
                margins,
                allow_extreme,
                dry_run,
            } => {
                vec![build_shaper_tier_args(
                    *tier,
                    *margins,
                    *allow_extreme,
                    *dry_run,
                )]
            }
            Self::ShaperSetAll {
                state,
                allow_extreme,
                dry_run,
            } => FreqTier::ALL
                .into_iter()
                .map(|tier| {
                    build_shaper_tier_args(tier, state.tier(tier), *allow_extreme, *dry_run)
                })
                .collect(),
            Self::ShaperReset { dry_run } => {
                let mut args = vec!["shaper".into(), "reset".into()];
                append_write_flags(&mut args, false, *dry_run);
                args.push("--json".into());
                vec![args]
            }
            Self::PboSet {
                req,
                allow_extreme,
                dry_run,
            } => {
                let mut args = vec!["pbo".into(), "set".into()];
                if let Some(ppt) = req.ppt_w {
                    args.push("--ppt".into());
                    args.push(ppt.to_string());
                }
                if let Some(tdc) = req.tdc_a {
                    args.push("--tdc".into());
                    args.push(tdc.to_string());
                }
                if let Some(edc) = req.edc_a {
                    args.push("--edc".into());
                    args.push(edc.to_string());
                }
                if let Some(tctl) = req.tctl_c {
                    args.push("--tctl".into());
                    args.push(tctl.to_string());
                }
                if let Some(scalar) = req.scalar {
                    args.push("--scalar".into());
                    args.push(scalar.to_string());
                }
                if let Some(fmax) = req.fmax_mhz {
                    args.push("--fmax".into());
                    args.push(fmax.to_string());
                }
                append_write_flags(&mut args, *allow_extreme, *dry_run);
                args.push("--json".into());
                vec![args]
            }
            Self::PboReset { dry_run } => {
                let mut args = vec!["pbo".into(), "reset".into()];
                append_write_flags(&mut args, false, *dry_run);
                args.push("--json".into());
                vec![args]
            }
            Self::ApuSet {
                req,
                allow_extreme,
                dry_run,
            } => {
                let mut args = vec!["apu".into(), "set".into()];
                if let Some(stapm) = req.stapm_w {
                    args.push("--stapm".into());
                    args.push(format!("{stapm}W"));
                }
                if let Some(stapm_time) = req.stapm_time_s {
                    args.push("--stapm-time".into());
                    args.push(format!("{stapm_time}s"));
                }
                if let Some(fast) = req.fast_w {
                    args.push("--fast".into());
                    args.push(format!("{fast}W"));
                }
                if let Some(slow) = req.slow_w {
                    args.push("--slow".into());
                    args.push(format!("{slow}W"));
                }
                if let Some(slow_time) = req.slow_time_s {
                    args.push("--slow-time".into());
                    args.push(format!("{slow_time}s"));
                }
                if let Some(apu_slow) = req.apu_slow_w {
                    args.push("--apu-slow".into());
                    args.push(format!("{apu_slow}W"));
                }
                if let Some(tdc_soc) = req.tdc_soc_a {
                    args.push("--tdc-soc".into());
                    args.push(format!("{tdc_soc}A"));
                }
                if let Some(edc_soc) = req.edc_soc_a {
                    args.push("--edc-soc".into());
                    args.push(format!("{edc_soc}A"));
                }
                if let Some(apu_skin) = req.apu_skin_temp_c {
                    args.push("--apu-skin".into());
                    args.push(format!("{apu_skin}C"));
                }
                if let Some(dgpu_skin) = req.dgpu_skin_temp_c {
                    args.push("--dgpu-skin".into());
                    args.push(format!("{dgpu_skin}C"));
                }
                if let Some(mode) = req.power_mode {
                    args.push("--mode".into());
                    args.push(match mode {
                        ApuPowerMode::MaxPerformance => "max-performance".into(),
                        ApuPowerMode::PowerSaving => "power-saving".into(),
                    });
                }
                append_write_flags(&mut args, *allow_extreme, *dry_run);
                args.push("--json".into());
                vec![args]
            }
            Self::ProfileApply {
                path,
                allow_extreme,
                dry_run,
            } => {
                let mut args = vec![
                    "profile".into(),
                    "apply".into(),
                    path.display().to_string(),
                ];
                append_write_flags(&mut args, *allow_extreme, *dry_run);
                args.push("--json".into());
                vec![args]
            }
            Self::ProfileExport { path } => vec![vec![
                "profile".into(),
                "export".into(),
                path.display().to_string(),
                "--json".into(),
            ]],
            Self::Rollback { dry_run } => {
                let mut args = vec!["rollback".into()];
                append_write_flags(&mut args, false, *dry_run);
                args.push("--json".into());
                vec![args]
            }
            Self::Reset { dry_run } => {
                let mut args = vec!["reset".into()];
                append_write_flags(&mut args, false, *dry_run);
                args.push("--json".into());
                vec![args]
            }
            Self::PersistSave {
                from_path,
                enable,
                allow_extreme,
            } => {
                let mut args = vec!["persist".into(), "save".into()];
                if let Some(path) = from_path {
                    args.push("--from".into());
                    args.push(path.clone());
                }
                if *enable {
                    args.push("--enable".into());
                }
                if *allow_extreme {
                    args.push("--allow-extreme".into());
                }
                args.push("--json".into());
                vec![args]
            }
            Self::PersistSetService {
                enable,
                remove_profile,
                allow_extreme,
            } => {
                let mut args = vec![
                    "persist".into(),
                    if *enable {
                        "enable".into()
                    } else {
                        "disable".into()
                    },
                ];
                if !*enable && *remove_profile {
                    args.push("--remove-profile".into());
                }
                if *enable && *allow_extreme {
                    args.push("--allow-extreme".into());
                }
                args.push("--json".into());
                vec![args]
            }
            Self::ClearBootGuard => vec![vec![
                "profile".into(),
                "clear-guard".into(),
                "--json".into(),
            ]],
        }
    }
}

fn append_write_flags(args: &mut Vec<String>, allow_extreme: bool, dry_run: bool) {
    if allow_extreme {
        args.push("--allow-extreme".into());
    }
    if dry_run {
        args.push("--dry-run".into());
    } else {
        args.push("--yes".into());
    }
}

fn build_shaper_tier_args(
    tier: FreqTier,
    margins: ShaperTierMargins,
    allow_extreme: bool,
    dry_run: bool,
) -> Vec<String> {
    let mut args = vec![
        "shaper".into(),
        "set".into(),
        "--freq".into(),
        tier.label().into(),
        "--temp".into(),
        format!(
            "low={},med={},high={}",
            margins.low, margins.med, margins.high
        ),
    ];
    append_write_flags(&mut args, allow_extreme, dry_run);
    args.push("--json".into());
    args
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

/// Result delivered asynchronously from the background CLI worker thread back to the GUI.
#[derive(Debug, Clone)]
pub enum ElevatedResult {
    Status(StatusPayload),
    Co(CoSnapshot),
    Shaper(CurveShaperState),
    Pbo(PboStatus),
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
    #[allow(dead_code)]
    ProfileDiff {
        entries: Vec<ProfileDiffEntry>,
    },
    PersistUpdated {
        action: String,
        status: PersistStatus,
        note: Option<String>,
    },
    GuardCleared {
        cleared: bool,
    },
    Error {
        title: String,
        message: String,
    },
}

/// Non-blocking background command dispatcher using `std::sync::mpsc`.
pub struct CommandRunner {
    tx: Sender<ElevatedResult>,
    rx: Receiver<ElevatedResult>,
    in_flight: Option<String>,
    zensmu_bin: PathBuf,
    is_root: bool,
}

impl CommandRunner {
    pub fn new(is_root: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            in_flight: None,
            zensmu_bin: find_zensmu_binary(),
            is_root,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.in_flight.is_some()
    }

    pub fn in_flight_label(&self) -> Option<&str> {
        self.in_flight.as_deref()
    }

    pub fn zensmu_bin(&self) -> &Path {
        &self.zensmu_bin
    }

    /// Spawn a `CliAction` on a background thread and request an `egui` repaint upon completion.
    pub fn dispatch(&mut self, action: CliAction, ctx: egui::Context) {
        if self.in_flight.is_some() {
            return;
        }
        let title = action.title();
        self.in_flight = Some(title.clone());
        let tx = self.tx.clone();
        let zensmu_bin = self.zensmu_bin.clone();
        let is_root = self.is_root;

        thread::spawn(move || {
            let result = execute_action(&zensmu_bin, is_root, action);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    /// Drain all completed results from the background worker.
    pub fn poll_results(&mut self) -> Vec<ElevatedResult> {
        let mut results = Vec::new();
        while let Ok(res) = self.rx.try_recv() {
            self.in_flight = None;
            results.push(res);
        }
        results
    }
}

fn execute_action(zensmu_bin: &Path, is_root: bool, action: CliAction) -> ElevatedResult {
    let title = action.title();
    let batches = action.to_arg_batches();
    let elevate = action.requires_privilege();

    let mut stdouts = Vec::with_capacity(batches.len());
    for batch in &batches {
        match run_cli_with_fallback(zensmu_bin, batch, elevate, is_root) {
            Ok(out) => stdouts.push(out),
            Err(message) => return ElevatedResult::Error { title, message },
        }
    }

    let first = stdouts.first().map(String::as_str).unwrap_or("");
    match action {
        CliAction::Status => match parse_status_payload(first) {
            Ok(payload) => ElevatedResult::Status(payload),
            Err(message) => ElevatedResult::Error { title, message },
        },
        CliAction::CoGet => match parse_co_snapshot(first) {
            Ok(snap) => ElevatedResult::Co(snap),
            Err(message) => ElevatedResult::Error { title, message },
        },
        CliAction::ShaperGet => match parse_shaper_state(first) {
            Ok(state) => ElevatedResult::Shaper(state),
            Err(message) => ElevatedResult::Error { title, message },
        },
        CliAction::PboGet => match parse_pbo_status(first) {
            Ok(status) => ElevatedResult::Pbo(status),
            Err(message) => ElevatedResult::Error { title, message },
        },
        CliAction::ProfileExport { path } => match fs::read_to_string(&path) {
            Ok(toml) => ElevatedResult::ProfileExported { path, toml },
            Err(e) => ElevatedResult::Error {
                title,
                message: format!(
                    "Export succeeded, but reading '{}' failed: {e}",
                    path.display()
                ),
            },
        },
        CliAction::PersistSave { .. } | CliAction::PersistSetService { .. } => {
            match parse_persist_action_output(first) {
                Ok(out) => ElevatedResult::PersistUpdated {
                    action: out.action,
                    status: out.status,
                    note: out.note,
                },
                Err(message) => ElevatedResult::Error { title, message },
            }
        }
        CliAction::ClearBootGuard => match parse_clear_guard_output(first) {
            Ok(cleared) => ElevatedResult::GuardCleared { cleared },
            Err(message) => ElevatedResult::Error { title, message },
        },
        write_action => {
            let mut all_ops = Vec::new();
            for out in &stdouts {
                match parse_planned_ops(out) {
                    Ok(ops) => all_ops.extend(ops),
                    Err(message) => return ElevatedResult::Error { title, message },
                }
            }
            if write_action.is_dry_run() {
                ElevatedResult::DryRunPlan {
                    title,
                    live_action: write_action.into_live(),
                    ops: all_ops,
                }
            } else {
                ElevatedResult::Applied {
                    title,
                    ops: all_ops,
                }
            }
        }
    }
}

fn run_cli_with_fallback(
    zensmu_bin: &Path,
    cli_args: &[String],
    elevate: bool,
    is_root: bool,
) -> Result<String, String> {
    let spec = build_command_spec(zensmu_bin, cli_args, elevate, is_root);
    match run_command_spec(&spec) {
        Ok(stdout) => Ok(stdout),
        Err(err) if !elevate && !is_root && looks_like_permission_error(&err) => {
            let elevated = build_command_spec(zensmu_bin, cli_args, true, false);
            run_command_spec(&elevated)
        }
        Err(err) => Err(err),
    }
}

fn run_command_spec(spec: &CommandSpec) -> Result<String, String> {
    let output = Command::new(&spec.program)
        .args(&spec.args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "Could not find executable '{}'. Ensure `zensmu` and `polkit` (`pkexec`) are installed.",
                    spec.program.display()
                )
            } else {
                format!("Failed to launch '{}': {e}", spec.program.display())
            }
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        Ok(stdout)
    } else {
        Err(parse_cli_error(
            &stdout,
            &stderr,
            output.status.code(),
            spec.uses_pkexec,
        ))
    }
}

fn looks_like_permission_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("permission denied")
        || lower.contains("requires root")
        || lower.contains("sudo")
        || lower.contains("pkexec")
}

/// Extract a clean user-facing error message from CLI stdout/stderr and exit status.
pub fn parse_cli_error(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    uses_pkexec: bool,
) -> String {
    if uses_pkexec && exit_code == Some(126) {
        return "PolicyKit authentication was dismissed or cancelled.".into();
    }
    if uses_pkexec && exit_code == Some(127) {
        return "PolicyKit could not execute `zensmu` (not found or not authorized).".into();
    }

    for candidate in [stdout, stderr] {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(candidate) {
            if let Some(msg) = val
                .get("error")
                .and_then(|v| v.as_str())
                .or_else(|| val.get("message").and_then(|v| v.as_str()))
            {
                return msg.to_string();
            }
        }
    }

    if !stderr.is_empty() {
        stderr.to_string()
    } else if !stdout.is_empty() {
        stdout.to_string()
    } else {
        format!(
            "Command failed with exit status {}",
            exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into())
        )
    }
}

/// Parse `Vec<PlannedOp>` from `zensmu ... --dry-run --json` or live apply JSON output.
///
/// Accepts:
/// - Bare JSON array: `[ { ...PlannedOp... }, ... ]`
/// - Single `PlannedOp` JSON object
/// - Wrapped object containing `ops`, `planned_ops`, `operations`, `plan`, or `applied`
/// - Explicit error object `{ "error": "..." }`
pub fn parse_planned_ops(json: &str) -> Result<Vec<PlannedOp>, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let val: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| format!("Invalid JSON from zensmu: {e}"))?;

    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }

    if val.is_array() {
        return serde_json::from_value(val)
            .map_err(|e| format!("Failed to decode planned operations array: {e}"));
    }

    if let Some(obj) = val.as_object() {
        for key in ["ops", "planned_ops", "operations", "plan", "applied"] {
            if let Some(arr) = obj.get(key) {
                if arr.is_array() {
                    return serde_json::from_value(arr.clone())
                        .map_err(|e| format!("Failed to decode '{key}' array: {e}"));
                }
            }
        }
        // Single PlannedOp object?
        if obj.contains_key("opcode") && obj.contains_key("mailbox") {
            let op: PlannedOp = serde_json::from_value(val)
                .map_err(|e| format!("Failed to decode single PlannedOp: {e}"))?;
            return Ok(vec![op]);
        }
    }

    Err(format!(
        "Unexpected JSON payload while parsing planned SMU operations: {trimmed}"
    ))
}

/// Parse a `CoSnapshot` from `zensmu co get --json`.
pub fn parse_co_snapshot(json: &str) -> Result<CoSnapshot, String> {
    let val: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("Invalid CO JSON: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }
    if let Ok(snap) = serde_json::from_value::<CoSnapshot>(val.clone()) {
        return Ok(snap);
    }
    for key in ["co", "curve_optimizer", "data"] {
        if let Some(inner) = val.get(key) {
            if let Ok(snap) = serde_json::from_value::<CoSnapshot>(inner.clone()) {
                return Ok(snap);
            }
        }
    }
    Err("Failed to parse Curve Optimizer snapshot from zensmu JSON output".into())
}

/// Parse a `CurveShaperState` from `zensmu shaper get --json`.
pub fn parse_shaper_state(json: &str) -> Result<CurveShaperState, String> {
    let val: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("Invalid Curve Shaper JSON: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }
    if let Ok(state) = serde_json::from_value::<CurveShaperState>(val.clone()) {
        return Ok(state);
    }
    for key in ["curve_shaper", "shaper", "data"] {
        if let Some(inner) = val.get(key) {
            if let Ok(state) = serde_json::from_value::<CurveShaperState>(inner.clone()) {
                return Ok(state);
            }
        }
    }
    Err("Failed to parse Curve Shaper state from zensmu JSON output".into())
}

/// Parse a `PboStatus` from `zensmu pbo get --json`.
pub fn parse_pbo_status(json: &str) -> Result<PboStatus, String> {
    let val: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("Invalid PBO JSON: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }
    for key in ["status", "pbo", "pbo_status", "data"] {
        if let Some(inner) = val.get(key) {
            if let Ok(status) = serde_json::from_value::<PboStatus>(inner.clone()) {
                return Ok(status);
            }
        }
    }
    if let Ok(status) = serde_json::from_value::<PboStatus>(val) {
        return Ok(status);
    }
    Err("Failed to parse PBO status from zensmu JSON output".into())
}

/// Parse combined SMU state from `zensmu status --json`.
///
/// Accepts either a composite status object (`{ co, curve_shaper, pbo, telemetry }`)
/// or a serialized `SmuStateSnapshot`.
pub fn parse_status_payload(json: &str) -> Result<StatusPayload, String> {
    let val: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("Invalid status JSON: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }

    let mut payload = StatusPayload::default();

    for key in ["co", "curve_optimizer"] {
        if let Some(v) = val.get(key) {
            if let Ok(co) = serde_json::from_value::<CoSnapshot>(v.clone()) {
                payload.co = Some(co);
                break;
            }
        }
    }

    for key in ["curve_shaper", "shaper"] {
        if let Some(v) = val.get(key) {
            if let Ok(cs) = serde_json::from_value::<CurveShaperState>(v.clone()) {
                payload.shaper = Some(cs);
                break;
            }
        }
    }

    for key in ["pbo", "pbo_status"] {
        if let Some(v) = val.get(key) {
            if let Ok(pbo) = serde_json::from_value::<PboStatus>(v.clone()) {
                payload.pbo = Some(pbo);
                break;
            }
        }
    }

    if let Some(v) = val.get("telemetry") {
        if let Ok(t) = serde_json::from_value::<TelemetrySnapshot>(v.clone()) {
            payload.telemetry = Some(t);
        }
    }

    // Fallback: if status returned a raw SmuStateSnapshot, unpack its fields.
    if payload.co.is_none() && payload.shaper.is_none() && payload.pbo.is_none() {
        if let Ok(snap) = serde_json::from_value::<SmuStateSnapshot>(val) {
            if let Some(cores) = snap.co_per_core {
                payload.co = Some(CoSnapshot {
                    cores: cores
                        .into_iter()
                        .map(|(core_index, margin)| CoreCoMargin {
                            core_index,
                            ccd: 0,
                            core_in_ccd: core_index,
                            linux_cpus: vec![core_index],
                            margin,
                        })
                        .collect(),
                    igpu: snap.igpu_co,
                });
            }
            payload.shaper = snap.curve_shaper;
            if snap.pbo_scalar.is_some() || snap.fmax_mhz.is_some() {
                payload.pbo = Some(PboStatus {
                    scalar: snap.pbo_scalar,
                    fmax_mhz: snap.fmax_mhz,
                    ..PboStatus::default()
                });
            }
        }
    }

    Ok(payload)
}

/// Parse `PersistActionOutput` from `zensmu persist save|enable|disable --json`.
pub fn parse_persist_action_output(json: &str) -> Result<PersistActionOutput, String> {
    let val: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| format!("Invalid persist JSON from zensmu: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }
    serde_json::from_value(val)
        .map_err(|e| format!("Failed to decode persist action output from zensmu: {e}"))
}

/// Parse `cleared` boolean from `zensmu profile clear-guard --json`.
pub fn parse_clear_guard_output(json: &str) -> Result<bool, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Ok(true);
    }
    let val: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("Invalid clear-guard JSON from zensmu: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }
    val.get("cleared")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| format!("Unexpected JSON payload from clear-guard: {trimmed}"))
}

fn extract_error_field(val: &serde_json::Value) -> Option<String> {
    if val.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        return val
            .get("error")
            .and_then(|v| v.as_str())
            .or_else(|| val.get("message").and_then(|v| v.as_str()))
            .map(ToString::to_string)
            .or_else(|| Some("CLI reported failure".into()));
    }
    val.get("error")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zensmu_core::types::{Codename, Mailbox};

    #[test]
    fn proc_status_root_detection_parses_uid_line() {
        let normal = "Name:\tzensmu-gui\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\n";
        assert!(!parse_is_root_from_proc_status(normal));

        let root_real = "Name:\tzensmu-gui\nUid:\t0\t0\t0\t0\n";
        assert!(parse_is_root_from_proc_status(root_real));

        let setuid_root = "Name:\tzensmu-gui\nUid:\t1000\t0\t0\t0\n";
        assert!(parse_is_root_from_proc_status(setuid_root));
    }

    #[test]
    fn cli_arg_construction_matches_expected_flags() {
        assert_eq!(CliAction::Status.to_arg_batches(), vec![vec!["status", "--json"]]);
        assert_eq!(
            CliAction::CoGet.to_arg_batches(),
            vec![vec!["co", "get", "--json"]]
        );
        assert_eq!(
            CliAction::ShaperGet.to_arg_batches(),
            vec![vec!["shaper", "get", "--json"]]
        );
        assert_eq!(
            CliAction::PboGet.to_arg_batches(),
            vec![vec!["pbo", "get", "--json"]]
        );

        let co_dry = CliAction::CoSetAll {
            margin: -25,
            allow_extreme: false,
            dry_run: true,
        };
        assert_eq!(
            co_dry.to_arg_batches(),
            vec![vec!["co", "set", "--all=-25", "--dry-run", "--json"]]
        );
        assert!(!co_dry.requires_privilege());

        let co_live = co_dry.into_live();
        assert_eq!(
            co_live.to_arg_batches(),
            vec![vec!["co", "set", "--all=-25", "--yes", "--json"]]
        );
        assert!(co_live.requires_privilege());

        let co_cores = CliAction::CoSetCores {
            updates: vec![(0, -15), (3, -38)],
            allow_extreme: true,
            dry_run: true,
        };
        assert_eq!(
            co_cores.to_arg_batches(),
            vec![vec![
                "co",
                "set",
                "--core",
                "0=-15,3=-38",
                "--allow-extreme",
                "--dry-run",
                "--json"
            ]]
        );

        assert_eq!(
            CliAction::CoReset { dry_run: true }.to_arg_batches(),
            vec![vec!["co", "reset", "--dry-run", "--json"]]
        );

        let shaper_tier = CliAction::ShaperSetTier {
            tier: FreqTier::High,
            margins: ShaperTierMargins {
                low: -10,
                med: -15,
                high: -20,
            },
            allow_extreme: false,
            dry_run: false,
        };
        assert_eq!(
            shaper_tier.to_arg_batches(),
            vec![vec![
                "shaper",
                "set",
                "--freq",
                "high",
                "--temp",
                "low=-10,med=-15,high=-20",
                "--yes",
                "--json"
            ]]
        );

        assert_eq!(
            CliAction::ShaperReset { dry_run: false }.to_arg_batches(),
            vec![vec!["shaper", "reset", "--yes", "--json"]]
        );
        assert_eq!(
            CliAction::PboReset { dry_run: true }.to_arg_batches(),
            vec![vec!["pbo", "reset", "--dry-run", "--json"]]
        );

        let pbo = CliAction::PboSet {
            req: PboSetRequest {
                ppt_w: Some(142),
                tdc_a: Some(110),
                edc_a: Some(170),
                tctl_c: Some(85),
                scalar: Some(2),
                fmax_mhz: Some(5650),
            },
            allow_extreme: false,
            dry_run: true,
        };
        assert_eq!(
            pbo.to_arg_batches(),
            vec![vec![
                "pbo",
                "set",
                "--ppt",
                "142",
                "--tdc",
                "110",
                "--edc",
                "170",
                "--tctl",
                "85",
                "--scalar",
                "2",
                "--fmax",
                "5650",
                "--dry-run",
                "--json"
            ]]
        );

        let profile = CliAction::ProfileApply {
            path: PathBuf::from("/etc/zensmu/active.toml"),
            allow_extreme: false,
            dry_run: true,
        };
        assert_eq!(
            profile.to_arg_batches(),
            vec![vec![
                "profile",
                "apply",
                "/etc/zensmu/active.toml",
                "--dry-run",
                "--json"
            ]]
        );

        assert_eq!(
            CliAction::Rollback { dry_run: true }.to_arg_batches(),
            vec![vec!["rollback", "--dry-run", "--json"]]
        );
        assert_eq!(
            CliAction::Reset { dry_run: false }.to_arg_batches(),
            vec![vec!["reset", "--yes", "--json"]]
        );
    }

    #[test]
    fn command_spec_uses_pkexec_only_when_non_root_and_elevated() {
        let bin = Path::new("/usr/bin/zensmu");
        let args = vec!["status".to_string(), "--json".to_string()];

        let elevated = build_command_spec(bin, &args, true, false);
        assert!(elevated.uses_pkexec);
        assert_eq!(elevated.program, PathBuf::from("pkexec"));
        assert_eq!(elevated.args, vec!["/usr/bin/zensmu", "status", "--json"]);

        let as_root = build_command_spec(bin, &args, true, true);
        assert!(!as_root.uses_pkexec);
        assert_eq!(as_root.program, PathBuf::from("/usr/bin/zensmu"));
        assert_eq!(as_root.args, vec!["status", "--json"]);

        let unpriv = build_command_spec(bin, &args, false, false);
        assert!(!unpriv.uses_pkexec);
        assert_eq!(unpriv.program, PathBuf::from("/usr/bin/zensmu"));
    }

    #[test]
    fn find_zensmu_binary_prefers_sibling_then_path_then_system_fallback() {
        let fallback = find_zensmu_binary_with(None, Some("/nonexistent/dir"));
        assert_eq!(fallback, PathBuf::from(SYSTEM_ZENSMU_PATH));

        let temp_base = env::temp_dir().join(format!("zensmu_gui_test_{}", std::process::id()));
        let sib_dir = temp_base.join("sib");
        let path_dir = temp_base.join("path");
        let _ = fs::create_dir_all(&sib_dir);
        let _ = fs::create_dir_all(&path_dir);

        let path_bin = path_dir.join("zensmu");
        fs::write(&path_bin, b"#!/bin/sh\n").unwrap();

        let found_path = find_zensmu_binary_with(Some(&sib_dir), path_dir.to_str());
        assert_eq!(found_path, path_bin);

        let sib_bin = sib_dir.join("zensmu");
        fs::write(&sib_bin, b"#!/bin/sh\n").unwrap();

        let found_sib = find_zensmu_binary_with(Some(&sib_dir), path_dir.to_str());
        assert_eq!(found_sib, sib_bin);

        let _ = fs::remove_dir_all(&temp_base);
    }

    #[test]
    fn parse_planned_ops_supports_array_single_wrapped_and_error() {
        let op = PlannedOp {
            description: "Set all-core CO to -20".into(),
            mailbox: Mailbox::Rsmu,
            opcode: 0x07,
            args: [0xFFEC, 0, 0, 0, 0, 0],
            current: Some("0".into()),
            target: "-20".into(),
        };

        let arr_json = serde_json::to_string(&vec![op.clone()]).unwrap();
        assert_eq!(parse_planned_ops(&arr_json).unwrap(), vec![op.clone()]);

        let single_json = serde_json::to_string(&op).unwrap();
        assert_eq!(parse_planned_ops(&single_json).unwrap(), vec![op.clone()]);

        let wrapped = serde_json::json!({
            "dry_run": true,
            "ops": [op.clone()]
        })
        .to_string();
        assert_eq!(parse_planned_ops(&wrapped).unwrap(), vec![op.clone()]);

        let wrapped2 = serde_json::json!({
            "action": "co set",
            "dry_run": true,
            "applied": false,
            "rolled_back": false,
            "operations": [op.clone()]
        })
        .to_string();
        assert_eq!(parse_planned_ops(&wrapped2).unwrap(), vec![op]);

        let err_json = r#"{"ok": false, "error": "value exceeds soft limit"}"#;
        assert!(parse_planned_ops(err_json)
            .unwrap_err()
            .contains("soft limit"));
    }

    #[test]
    fn parse_state_payloads_decode_co_shaper_pbo_and_status() {
        let co = CoSnapshot {
            cores: vec![CoreCoMargin {
                core_index: 0,
                ccd: 0,
                core_in_ccd: 0,
                linux_cpus: vec![0, 8],
                margin: -20,
            }],
            igpu: Some(-10),
        };
        let co_json = serde_json::to_string(&co).unwrap();
        assert_eq!(parse_co_snapshot(&co_json).unwrap(), co);

        let shaper = CurveShaperState {
            high: ShaperTierMargins {
                low: -10,
                med: -15,
                high: -20,
            },
            ..CurveShaperState::default()
        };
        let shaper_wrapped = serde_json::json!({ "curve_shaper": shaper }).to_string();
        assert_eq!(parse_shaper_state(&shaper_wrapped).unwrap(), shaper);

        let pbo = PboStatus {
            scalar: Some(2.0),
            fmax_mhz: Some(5550),
            ..PboStatus::default()
        };
        let pbo_json = serde_json::to_string(&pbo).unwrap();
        assert_eq!(parse_pbo_status(&pbo_json).unwrap(), pbo);

        // Wrapped PboGetOutput format emitted by `zensmu pbo get --json`
        let pbo_wrapped = serde_json::json!({
            "codename": "granite-ridge",
            "active_limits": { "ppt_w": 88.0 },
            "status": pbo
        })
        .to_string();
        assert_eq!(parse_pbo_status(&pbo_wrapped).unwrap(), pbo);

        let status_json = serde_json::json!({
            "co": co,
            "curve_shaper": shaper,
            "pbo": pbo,
        })
        .to_string();
        let parsed = parse_status_payload(&status_json).unwrap();
        assert_eq!(parsed.co.unwrap().igpu, Some(-10));
        assert_eq!(parsed.shaper.unwrap().high.med, -15);
        assert_eq!(parsed.pbo.unwrap().fmax_mhz, Some(5550));

        // Raw SmuStateSnapshot fallback
        let mut snap = SmuStateSnapshot::new(Codename::GraniteRidge, 8);
        snap.co_per_core = Some(vec![(0, -15), (1, -20)]);
        snap.fmax_mhz = Some(5600);
        let snap_json = serde_json::to_string(&snap).unwrap();
        let parsed_snap = parse_status_payload(&snap_json).unwrap();
        assert_eq!(parsed_snap.co.unwrap().cores[1].margin, -20);
        assert_eq!(parsed_snap.pbo.unwrap().fmax_mhz, Some(5600));
    }

    #[test]
    fn parse_cli_error_handles_pkexec_cancel_and_json_errors() {
        assert!(parse_cli_error("", "", Some(126), true).contains("cancelled"));
        assert!(parse_cli_error("", "", Some(127), true).contains("PolicyKit"));
        assert_eq!(
            parse_cli_error(r#"{"error":"SMU rejected 0x07"}"#, "", Some(1), false),
            "SMU rejected 0x07"
        );
        assert_eq!(
            parse_cli_error("", "plain stderr error", Some(1), false),
            "plain stderr error"
        );
    }

    #[test]
    fn persist_and_clear_guard_cli_args_match_frozen_contract() {
        let save_live = CliAction::PersistSave {
            from_path: None,
            enable: true,
            allow_extreme: false,
        };
        assert!(save_live.requires_privilege());
        assert_eq!(
            save_live.to_arg_batches(),
            vec![vec!["persist", "save", "--enable", "--json"]]
        );

        let save_from = CliAction::PersistSave {
            from_path: Some("/tmp/zensmu-gui-persist-42.toml".into()),
            enable: true,
            allow_extreme: true,
        };
        assert!(save_from.requires_privilege());
        assert_eq!(
            save_from.to_arg_batches(),
            vec![vec![
                "persist",
                "save",
                "--from",
                "/tmp/zensmu-gui-persist-42.toml",
                "--enable",
                "--allow-extreme",
                "--json"
            ]]
        );

        let enable_svc = CliAction::PersistSetService {
            enable: true,
            remove_profile: false,
            allow_extreme: true,
        };
        assert!(enable_svc.requires_privilege());
        assert_eq!(
            enable_svc.to_arg_batches(),
            vec![vec!["persist", "enable", "--allow-extreme", "--json"]]
        );

        let disable_svc = CliAction::PersistSetService {
            enable: false,
            remove_profile: false,
            allow_extreme: false,
        };
        assert!(disable_svc.requires_privilege());
        assert_eq!(
            disable_svc.to_arg_batches(),
            vec![vec!["persist", "disable", "--json"]]
        );

        let disable_rm = CliAction::PersistSetService {
            enable: false,
            remove_profile: true,
            allow_extreme: false,
        };
        assert!(disable_rm.requires_privilege());
        assert_eq!(
            disable_rm.to_arg_batches(),
            vec![vec![
                "persist",
                "disable",
                "--remove-profile",
                "--json"
            ]]
        );

        assert!(CliAction::ClearBootGuard.requires_privilege());
        assert_eq!(
            CliAction::ClearBootGuard.to_arg_batches(),
            vec![vec!["profile", "clear-guard", "--json"]]
        );
    }

    #[test]
    fn parse_persist_action_and_clear_guard_decode_json_responses() {
        let status = PersistStatus {
            active_profile_path: "/etc/zensmu/active.toml".into(),
            active_profile_exists: true,
            active_profile_valid: Some(true),
            active_profile_error: None,
            active_profile_codename: Some(Codename::GraniteRidge),
            modules_load_configured: true,
            service_name: "zensmu-apply.service".into(),
            service_installed: true,
            service_enabled: true,
            service_state: "enabled".into(),
            resume_hook_installed: true,
            boot_guard_tripped: false,
        };

        let payload = serde_json::json!({
            "action": "persist save",
            "dry_run": false,
            "active_profile_path": "/etc/zensmu/active.toml",
            "service_enabled": true,
            "status": status,
            "note": "Enabled zensmu-apply.service"
        })
        .to_string();

        let decoded = parse_persist_action_output(&payload).unwrap();
        assert_eq!(decoded.action, "persist save");
        assert!(!decoded.dry_run);
        assert!(decoded.service_enabled);
        assert_eq!(decoded.status.active_profile_codename, Some(Codename::GraniteRidge));
        assert_eq!(decoded.note.as_deref(), Some("Enabled zensmu-apply.service"));

        let err_payload = r#"{"ok": false, "error": "cannot enable service without active profile"}"#;
        assert!(parse_persist_action_output(err_payload)
            .unwrap_err()
            .contains("without active profile"));

        let guard_json = r#"{"guard_path":"/var/lib/zensmu/applying.lock","was_tripped":true,"cleared":true}"#;
        assert!(parse_clear_guard_output(guard_json).unwrap());

        let guard_noop = r#"{"guard_path":"/var/lib/zensmu/applying.lock","was_tripped":false,"cleared":false}"#;
        assert!(!parse_clear_guard_output(guard_noop).unwrap());

        let guard_err = r#"{"ok":false,"error":"permission denied"}"#;
        assert!(parse_clear_guard_output(guard_err)
            .unwrap_err()
            .contains("permission denied"));
    }

    #[test]
    fn apu_set_cli_args_build_all_and_partial_fields_with_write_flags() {
        let full_dry = CliAction::ApuSet {
            req: ApuSetRequest {
                stapm_w: Some(45),
                stapm_time_s: Some(120),
                fast_w: Some(54),
                slow_w: Some(45),
                slow_time_s: Some(10),
                apu_slow_w: Some(35),
                tdc_soc_a: Some(30),
                edc_soc_a: Some(50),
                apu_skin_temp_c: Some(55),
                dgpu_skin_temp_c: Some(60),
                power_mode: Some(ApuPowerMode::MaxPerformance),
            },
            allow_extreme: false,
            dry_run: true,
        };
        assert!(full_dry.is_dry_run());
        assert!(!full_dry.requires_privilege());
        assert_eq!(full_dry.title(), "Preview APU DPTC Limits");
        assert_eq!(
            full_dry.to_arg_batches(),
            vec![vec![
                "apu",
                "set",
                "--stapm",
                "45W",
                "--stapm-time",
                "120s",
                "--fast",
                "54W",
                "--slow",
                "45W",
                "--slow-time",
                "10s",
                "--apu-slow",
                "35W",
                "--tdc-soc",
                "30A",
                "--edc-soc",
                "50A",
                "--apu-skin",
                "55C",
                "--dgpu-skin",
                "60C",
                "--mode",
                "max-performance",
                "--dry-run",
                "--json",
            ]]
        );

        let full_live = full_dry.into_live();
        assert!(!full_live.is_dry_run());
        assert!(full_live.requires_privilege());
        assert_eq!(full_live.title(), "Apply APU DPTC Limits");
        assert_eq!(
            full_live.to_arg_batches(),
            vec![vec![
                "apu",
                "set",
                "--stapm",
                "45W",
                "--stapm-time",
                "120s",
                "--fast",
                "54W",
                "--slow",
                "45W",
                "--slow-time",
                "10s",
                "--apu-slow",
                "35W",
                "--tdc-soc",
                "30A",
                "--edc-soc",
                "50A",
                "--apu-skin",
                "55C",
                "--dgpu-skin",
                "60C",
                "--mode",
                "max-performance",
                "--yes",
                "--json",
            ]]
        );

        let partial_extreme = CliAction::ApuSet {
            req: ApuSetRequest {
                stapm_w: Some(115),
                apu_skin_temp_c: Some(70),
                power_mode: Some(ApuPowerMode::PowerSaving),
                ..Default::default()
            },
            allow_extreme: true,
            dry_run: true,
        };
        assert!(partial_extreme.is_dry_run());
        assert!(!partial_extreme.requires_privilege());
        assert_eq!(
            partial_extreme.to_arg_batches(),
            vec![vec![
                "apu",
                "set",
                "--stapm",
                "115W",
                "--apu-skin",
                "70C",
                "--mode",
                "power-saving",
                "--allow-extreme",
                "--dry-run",
                "--json",
            ]]
        );

        let partial_live = partial_extreme.into_live();
        assert!(partial_live.requires_privilege());
        assert_eq!(
            partial_live.to_arg_batches(),
            vec![vec![
                "apu",
                "set",
                "--stapm",
                "115W",
                "--apu-skin",
                "70C",
                "--mode",
                "power-saving",
                "--allow-extreme",
                "--yes",
                "--json",
            ]]
        );
    }
}
