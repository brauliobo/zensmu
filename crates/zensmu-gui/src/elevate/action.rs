//! Structured CLI action definitions and argument batch builders.

use std::path::PathBuf;
use zensmu_core::driver::DriverInstallMethod;
use zensmu_core::ops::apu::ApuSetRequest;
use zensmu_core::ops::pbo::PboSetRequest;
use zensmu_core::types::{ApuPowerMode, CurveShaperState, FreqTier, ShaperTierMargins};

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
    /// `zensmu driver load --json`
    DriverLoad,
    /// `zensmu driver install --method <method> --yes --json`
    DriverInstall { method: DriverInstallMethod },
    /// `zensmu report --json [--raw-pm-table] [--note <text>]`
    Report {
        raw_pm_table: bool,
        note: Option<String>,
    },
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
            Self::DriverLoad => "Load ryzen_smu Driver".into(),
            Self::DriverInstall { .. } => "Install ryzen_smu Driver".into(),
            Self::Report { .. } => "Collect Hardware Report".into(),
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
            | Self::ClearBootGuard
            | Self::DriverLoad
            | Self::DriverInstall { .. }
            | Self::Report { .. } => true,
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
                let mut args = vec!["profile".into(), "apply".into(), path.display().to_string()];
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
            Self::DriverLoad => vec![vec!["driver".into(), "load".into(), "--json".into()]],
            Self::DriverInstall { method } => vec![vec![
                "driver".into(),
                "install".into(),
                "--method".into(),
                method.as_str().into(),
                "--yes".into(),
                "--json".into(),
            ]],
            Self::Report { raw_pm_table, note } => {
                let mut args = vec!["report".into(), "--json".into()];
                if *raw_pm_table {
                    args.push("--raw-pm-table".into());
                }
                if let Some(text) = note.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
                    args.push("--note".into());
                    args.push(text.to_string());
                }
                vec![args]
            }
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
