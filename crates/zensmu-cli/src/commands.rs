//! Clap CLI argument definitions for `zensmu`.

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;
use zensmu_core::types::ApuPowerMode;

/// Safety-first CLI and live TUI for AMD Ryzen SMU control and telemetry.
#[derive(Debug, Parser)]
#[command(
    name = "zensmu",
    version,
    about = "Safety-first CLI and live TUI for AMD Ryzen SMU control and telemetry",
    propagate_version = true
)]
pub struct Cli {
    /// Output structured JSON (suitable for scripting and GUI IPC)
    #[arg(long, global = true)]
    pub json: bool,

    /// Simulate writes and print planned SMU operations without writing to sysfs
    #[arg(long, global = true)]
    pub dry_run: bool,

    /// Skip interactive confirmation prompts
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,

    /// Allow values beyond conservative soft safety thresholds (hard bounds still enforced)
    #[arg(long, global = true)]
    pub allow_extreme: bool,

    /// Wait up to DURATION (e.g. 30s) for confirmation after applying writes; auto-rolls back on timeout, 'n', or Ctrl-C
    #[arg(long, global = true, value_name = "DURATION")]
    pub confirm_within: Option<String>,

    /// Override ryzen_smu sysfs path (default: /sys/kernel/ryzen_smu_drv)
    #[arg(long, global = true, value_name = "PATH")]
    pub sysfs_path: Option<PathBuf>,

    /// Override snapshot state directory (default: /run/zensmu)
    #[arg(long, global = true, value_name = "PATH")]
    pub state_dir: Option<PathBuf>,

    /// Override boot-loop crash guard sentinel path (default: /var/lib/zensmu/applying.lock)
    #[arg(long, global = true, value_name = "PATH")]
    pub guard_path: Option<PathBuf>,

    /// Skip host CPUID cross-check against driver codename (useful for mock sysfs testing)
    #[arg(long, global = true)]
    pub no_cpuid_check: bool,

    /// Increase diagnostic verbosity (-v, -vv)
    #[arg(short = 'v', long, global = true, action = ArgAction::Count)]
    pub verbose: u8,

    /// Suppress non-essential output
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Display CPU identity, driver metadata, topology, and supported capabilities (unprivileged)
    Info,

    /// Run safety diagnostics on ryzen_smu, permissions, CPUID, PM table, boot guard, and mailboxes
    Doctor,

    /// Show unified snapshot of all readable SMU state (telemetry unprivileged; mailboxes require root)
    Status,

    /// Read or configure Curve Optimizer (per-core, all-core, and iGPU)
    Co {
        #[command(subcommand)]
        action: CoAction,
    },

    /// Read or configure Zen 5 Curve Shaper (5 frequency tiers x 3 temperature bands)
    Shaper {
        #[command(subcommand)]
        action: ShaperAction,
    },

    /// Read or configure Precision Boost Overdrive limits, scalar, and Fmax
    Pbo {
        #[command(subcommand)]
        action: PboAction,
    },

    /// Configure mobile APU Dynamic Power and Thermal Configuration (DPTC) limits
    Apu {
        #[command(subcommand)]
        action: ApuAction,
    },

    /// Read unprivileged live PM table telemetry (one-shot, watch, or raw dump)
    Telemetry {
        #[command(subcommand)]
        action: Option<TelemetryAction>,
    },

    /// Launch btop-style live terminal telemetry dashboard (unprivileged)
    Top {
        /// Refresh interval (e.g. 500ms, 1s)
        #[arg(long, default_value = "500ms", value_name = "DURATION")]
        interval: String,
    },

    /// Save, validate, diff, apply, or list declarative TOML tuning profiles
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },

    /// Manage boot & suspend persistence (/etc/zensmu/active.toml, modules-load.d, zensmu-apply.service)
    Persist(PersistCommand),

    /// Restore the pre-change SMU state snapshot from last-state.json
    Rollback,

    /// Restore the initial boot-baseline SMU state snapshot from boot-baseline.json
    Reset,

    /// Generate shell completion scripts
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: CompletionShell,
    },
}

#[derive(Debug, Subcommand)]
pub enum CoAction {
    /// Read current per-core and iGPU Curve Optimizer margins
    Get,

    /// Set all-core, per-core, and/or iGPU Curve Optimizer margins
    Set {
        /// Set an all-core Curve Optimizer margin (e.g. -20)
        #[arg(long, allow_hyphen_values = true, value_name = "MARGIN")]
        all: Option<i32>,

        /// Set per-core margins (repeatable), e.g. --core 0=-25 --core 2..7=-20
        #[arg(long = "core", allow_hyphen_values = true, value_name = "SPEC")]
        cores: Vec<String>,

        /// Set iGPU Curve Optimizer margin (e.g. -15)
        #[arg(long, allow_hyphen_values = true, value_name = "MARGIN")]
        igpu: Option<i32>,
    },

    /// Reset all Curve Optimizer margins (and iGPU if supported) to 0
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum ShaperAction {
    /// Read the full 5x3 Zen 5 Curve Shaper matrix
    Get,

    /// Set Curve Shaper margins for a specific frequency tier or across all tiers
    Set {
        /// Frequency tier: min, low, med, high, or max
        #[arg(long, value_name = "TIER")]
        freq: Option<String>,

        /// Temperature band spec, e.g. low=-10,med=-15,high=-5 or all=-10
        #[arg(long, allow_hyphen_values = true, value_name = "SPEC")]
        temp: Option<String>,

        /// Set all 15 Curve Shaper points (5 tiers x 3 bands) to a single margin
        #[arg(long, allow_hyphen_values = true, value_name = "MARGIN")]
        all: Option<i32>,
    },

    /// Reset all 5 Curve Shaper frequency tiers to (0, 0, 0)
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum PboAction {
    /// Read active PM table limits, fused factory limits, scalar, Fmax, flags, and CPPC scores
    Get,

    /// Set PBO power/current/thermal limits, scalar, and/or Fmax boost clock
    Set {
        /// Package Power Tracking limit (e.g. 142, 142W, 142000mW)
        #[arg(long, value_name = "WATTS")]
        ppt: Option<String>,

        /// Thermal Design Current limit (e.g. 110, 110A, 110000mA)
        #[arg(long, value_name = "AMPS")]
        tdc: Option<String>,

        /// Electrical Design Current limit (e.g. 170, 170A, 170000mA)
        #[arg(long, value_name = "AMPS")]
        edc: Option<String>,

        /// Thermal throttle limit Tctl/TjMax (e.g. 85, 85C)
        #[arg(long, alias = "tjmax", value_name = "CELSIUS")]
        tctl: Option<String>,

        /// PBO scalar multiplier (1..=10)
        #[arg(long, value_name = "1..=10")]
        scalar: Option<u32>,

        /// Fmax boost clock ceiling (e.g. 5550, 5550MHz, 5.55GHz)
        #[arg(long, value_name = "FREQ")]
        fmax: Option<String>,
    },

    /// Restore fused factory PBO limits (and boot baseline PBO state if available)
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum ApuAction {
    /// Set mobile APU DPTC power, current, thermal skin, and power mode parameters
    Set {
        /// Sustained power limit STAPM (e.g. 28, 28W)
        #[arg(long, value_name = "WATTS")]
        stapm: Option<String>,

        /// STAPM time constant in seconds (e.g. 120, 120s)
        #[arg(long, value_name = "SECS")]
        stapm_time: Option<String>,

        /// Fast PPT boost power limit (e.g. 45, 45W)
        #[arg(long, value_name = "WATTS")]
        fast: Option<String>,

        /// Slow PPT sustained power limit (e.g. 35, 35W)
        #[arg(long, value_name = "WATTS")]
        slow: Option<String>,

        /// Slow PPT time constant in seconds (e.g. 10, 10s)
        #[arg(long, value_name = "SECS")]
        slow_time: Option<String>,

        /// APU-only slow PPT power limit (e.g. 30, 30W)
        #[arg(long, value_name = "WATTS")]
        apu_slow: Option<String>,

        /// SoC TDC current limit (e.g. 30, 30A)
        #[arg(long, value_name = "AMPS")]
        tdc_soc: Option<String>,

        /// SoC EDC current limit (e.g. 50, 50A)
        #[arg(long, value_name = "AMPS")]
        edc_soc: Option<String>,

        /// APU skin temperature target (e.g. 45, 45C)
        #[arg(long, alias = "apu-skin-temp", value_name = "CELSIUS")]
        apu_skin: Option<String>,

        /// dGPU skin temperature target (e.g. 60, 60C)
        #[arg(long, alias = "dgpu-skin-temp", value_name = "CELSIUS")]
        dgpu_skin: Option<String>,

        /// APU power/performance mode
        #[arg(long, value_enum)]
        mode: Option<ApuModeArg>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ApuModeArg {
    #[value(name = "max-performance")]
    MaxPerformance,
    #[value(name = "power-saving")]
    PowerSaving,
}

impl From<ApuModeArg> for ApuPowerMode {
    fn from(value: ApuModeArg) -> Self {
        match value {
            ApuModeArg::MaxPerformance => ApuPowerMode::MaxPerformance,
            ApuModeArg::PowerSaving => ApuPowerMode::PowerSaving,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum TelemetryAction {
    /// Continuously refresh PM table telemetry until Ctrl-C
    Watch {
        /// Polling interval (e.g. 500ms, 1s)
        #[arg(long, default_value = "1s", value_name = "DURATION")]
        interval: String,
    },

    /// Safely dump raw pm_table bytes to a file for offline inspection
    Dump {
        /// Output file path
        #[arg(value_name = "OUT_PATH")]
        out_path: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProfileAction {
    /// Capture current live SMU state and write a TOML profile
    #[command(visible_alias = "export")]
    Save {
        /// Destination TOML profile path
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },

    /// Validate a TOML profile schema, target CPU, and safety bounds
    Validate {
        /// Path to TOML profile
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },

    /// Show differences between a TOML profile and current live SMU state
    Diff {
        /// Path to TOML profile
        #[arg(value_name = "PATH")]
        path: PathBuf,
    },

    /// Apply a validated TOML profile to hardware
    Apply {
        /// Path to TOML profile
        #[arg(value_name = "PATH")]
        path: PathBuf,

        /// Arm persistent boot-loop crash guard before writing and disarm only after settle period
        #[arg(long)]
        boot_guard: bool,

        /// Seconds to wait after applying before disarming --boot-guard (e.g. 15 from systemd)
        #[arg(long, default_value_t = 0, value_name = "N")]
        settle_secs: u64,
    },

    /// Clear a tripped boot-loop crash guard sentinel
    ClearGuard,

    /// List TOML profiles in a directory
    List {
        /// Directory to scan for .toml profiles
        #[arg(long, default_value = "/etc/zensmu", value_name = "DIR")]
        dir: PathBuf,
    },
}

#[derive(Debug, Args)]
pub struct PersistCommand {
    #[command(subcommand)]
    pub action: PersistAction,
}

#[derive(Debug, Subcommand)]
pub enum PersistAction {
    /// Inspect active boot profile, module autoload, systemd service, resume hook, and boot guard (unprivileged)
    Status,

    /// Save a validated profile to /etc/zensmu/active.toml and configure ryzen_smu autoload
    Save {
        /// Load profile from an existing TOML file instead of capturing current live SMU state
        #[arg(long, value_name = "PATH")]
        from: Option<PathBuf>,

        /// Also enable zensmu-apply.service after saving
        #[arg(long)]
        enable: bool,
    },

    /// Enable zensmu-apply.service (requires a valid /etc/zensmu/active.toml)
    Enable,

    /// Disable zensmu-apply.service (optionally removing /etc/zensmu/active.toml)
    Disable {
        /// Also remove /etc/zensmu/active.toml
        #[arg(long)]
        remove_profile: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    Bash,
    Zsh,
    Fish,
}

impl From<CompletionShell> for Shell {
    fn from(value: CompletionShell) -> Self {
        match value {
            CompletionShell::Bash => Shell::Bash,
            CompletionShell::Zsh => Shell::Zsh,
            CompletionShell::Fish => Shell::Fish,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn clap_command_debug_assert() {
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_global_flags_before_and_after_subcommand() {
        let cli = Cli::try_parse_from([
            "zensmu",
            "--json",
            "--dry-run",
            "--yes",
            "--allow-extreme",
            "--confirm-within",
            "30s",
            "--sysfs-path",
            "/tmp/mock_smu",
            "--state-dir",
            "/tmp/state",
            "--guard-path",
            "/tmp/guard.lock",
            "--no-cpuid-check",
            "-vv",
            "info",
        ])
        .unwrap();

        assert!(cli.json);
        assert!(cli.dry_run);
        assert!(cli.yes);
        assert!(cli.allow_extreme);
        assert_eq!(cli.confirm_within.as_deref(), Some("30s"));
        assert_eq!(cli.sysfs_path, Some(PathBuf::from("/tmp/mock_smu")));
        assert_eq!(cli.state_dir, Some(PathBuf::from("/tmp/state")));
        assert_eq!(cli.guard_path, Some(PathBuf::from("/tmp/guard.lock")));
        assert!(cli.no_cpuid_check);
        assert_eq!(cli.verbose, 2);
        assert!(!cli.quiet);
        assert!(matches!(cli.command, Command::Info));

        // Global flags placed AFTER subcommand (matching systemd/README invocations)
        let cli2 = Cli::try_parse_from([
            "zensmu",
            "profile",
            "apply",
            "/etc/zensmu/active.toml",
            "--yes",
            "--boot-guard",
            "--settle-secs",
            "15",
            "--json",
        ])
        .unwrap();
        assert!(cli2.yes);
        assert!(cli2.json);
        match cli2.command {
            Command::Profile {
                action:
                    ProfileAction::Apply {
                        path,
                        boot_guard,
                        settle_secs,
                    },
            } => {
                assert_eq!(path, PathBuf::from("/etc/zensmu/active.toml"));
                assert!(boot_guard);
                assert_eq!(settle_secs, 15);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_co_subcommands_with_negative_values() {
        let get = Cli::try_parse_from(["zensmu", "co", "get"]).unwrap();
        assert!(matches!(
            get.command,
            Command::Co {
                action: CoAction::Get
            }
        ));

        let set = Cli::try_parse_from([
            "zensmu",
            "co",
            "set",
            "--all",
            "-20",
            "--core",
            "0=-25",
            "--core",
            "2..7=-15",
            "--igpu",
            "-10",
            "--dry-run",
        ])
        .unwrap();
        assert!(set.dry_run);
        match set.command {
            Command::Co {
                action: CoAction::Set { all, cores, igpu },
            } => {
                assert_eq!(all, Some(-20));
                assert_eq!(cores, vec!["0=-25".to_string(), "2..7=-15".to_string()]);
                assert_eq!(igpu, Some(-10));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let reset = Cli::try_parse_from(["zensmu", "co", "reset"]).unwrap();
        assert!(matches!(
            reset.command,
            Command::Co {
                action: CoAction::Reset
            }
        ));
    }

    #[test]
    fn parses_shaper_subcommands() {
        let get = Cli::try_parse_from(["zensmu", "shaper", "get"]).unwrap();
        assert!(matches!(
            get.command,
            Command::Shaper {
                action: ShaperAction::Get
            }
        ));

        let set_tier = Cli::try_parse_from([
            "zensmu",
            "shaper",
            "set",
            "--freq",
            "high",
            "--temp",
            "low=-10,med=-15,high=-5",
        ])
        .unwrap();
        match set_tier.command {
            Command::Shaper {
                action: ShaperAction::Set { freq, temp, all },
            } => {
                assert_eq!(freq.as_deref(), Some("high"));
                assert_eq!(temp.as_deref(), Some("low=-10,med=-15,high=-5"));
                assert_eq!(all, None);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let set_all = Cli::try_parse_from(["zensmu", "shaper", "set", "--all", "-10"]).unwrap();
        match set_all.command {
            Command::Shaper {
                action: ShaperAction::Set { freq, temp, all },
            } => {
                assert_eq!(freq, None);
                assert_eq!(temp, None);
                assert_eq!(all, Some(-10));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let reset = Cli::try_parse_from(["zensmu", "shaper", "reset"]).unwrap();
        assert!(matches!(
            reset.command,
            Command::Shaper {
                action: ShaperAction::Reset
            }
        ));
    }

    #[test]
    fn parses_pbo_subcommands_and_tjmax_alias() {
        let get = Cli::try_parse_from(["zensmu", "pbo", "get"]).unwrap();
        assert!(matches!(
            get.command,
            Command::Pbo {
                action: PboAction::Get
            }
        ));

        let set = Cli::try_parse_from([
            "zensmu",
            "pbo",
            "set",
            "--ppt",
            "142W",
            "--tdc",
            "110A",
            "--edc",
            "170A",
            "--tctl",
            "85C",
            "--scalar",
            "2",
            "--fmax",
            "5.65GHz",
        ])
        .unwrap();
        match set.command {
            Command::Pbo {
                action:
                    PboAction::Set {
                        ppt,
                        tdc,
                        edc,
                        tctl,
                        scalar,
                        fmax,
                    },
            } => {
                assert_eq!(ppt.as_deref(), Some("142W"));
                assert_eq!(tdc.as_deref(), Some("110A"));
                assert_eq!(edc.as_deref(), Some("170A"));
                assert_eq!(tctl.as_deref(), Some("85C"));
                assert_eq!(scalar, Some(2));
                assert_eq!(fmax.as_deref(), Some("5.65GHz"));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let set_alias =
            Cli::try_parse_from(["zensmu", "pbo", "set", "--tjmax", "85"]).unwrap();
        match set_alias.command {
            Command::Pbo {
                action: PboAction::Set { tctl, .. },
            } => assert_eq!(tctl.as_deref(), Some("85")),
            other => panic!("unexpected command: {other:?}"),
        }

        let reset = Cli::try_parse_from(["zensmu", "pbo", "reset"]).unwrap();
        assert!(matches!(
            reset.command,
            Command::Pbo {
                action: PboAction::Reset
            }
        ));
    }

    #[test]
    fn parses_apu_set_all_flags() {
        let cli = Cli::try_parse_from([
            "zensmu",
            "apu",
            "set",
            "--stapm",
            "28W",
            "--stapm-time",
            "120s",
            "--fast",
            "45W",
            "--slow",
            "35W",
            "--slow-time",
            "10s",
            "--apu-slow",
            "30W",
            "--tdc-soc",
            "30A",
            "--edc-soc",
            "50A",
            "--apu-skin",
            "45C",
            "--dgpu-skin",
            "60C",
            "--mode",
            "power-saving",
        ])
        .unwrap();

        match cli.command {
            Command::Apu {
                action:
                    ApuAction::Set {
                        stapm,
                        stapm_time,
                        fast,
                        slow,
                        slow_time,
                        apu_slow,
                        tdc_soc,
                        edc_soc,
                        apu_skin,
                        dgpu_skin,
                        mode,
                    },
            } => {
                assert_eq!(stapm.as_deref(), Some("28W"));
                assert_eq!(stapm_time.as_deref(), Some("120s"));
                assert_eq!(fast.as_deref(), Some("45W"));
                assert_eq!(slow.as_deref(), Some("35W"));
                assert_eq!(slow_time.as_deref(), Some("10s"));
                assert_eq!(apu_slow.as_deref(), Some("30W"));
                assert_eq!(tdc_soc.as_deref(), Some("30A"));
                assert_eq!(edc_soc.as_deref(), Some("50A"));
                assert_eq!(apu_skin.as_deref(), Some("45C"));
                assert_eq!(dgpu_skin.as_deref(), Some("60C"));
                assert_eq!(mode, Some(ApuModeArg::PowerSaving));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_telemetry_top_profile_rollback_reset_completions() {
        // Default one-shot telemetry
        let t1 = Cli::try_parse_from(["zensmu", "telemetry", "--json"]).unwrap();
        assert!(t1.json);
        assert!(matches!(t1.command, Command::Telemetry { action: None }));

        // Telemetry watch
        let t2 = Cli::try_parse_from(["zensmu", "telemetry", "watch", "--interval", "250ms"])
            .unwrap();
        match t2.command {
            Command::Telemetry {
                action: Some(TelemetryAction::Watch { interval }),
            } => assert_eq!(interval, "250ms"),
            other => panic!("unexpected command: {other:?}"),
        }

        // Telemetry dump
        let t3 = Cli::try_parse_from(["zensmu", "telemetry", "dump", "/tmp/pm.bin"]).unwrap();
        match t3.command {
            Command::Telemetry {
                action: Some(TelemetryAction::Dump { out_path }),
            } => assert_eq!(out_path, PathBuf::from("/tmp/pm.bin")),
            other => panic!("unexpected command: {other:?}"),
        }

        // Top
        let top = Cli::try_parse_from(["zensmu", "top", "--interval", "200ms"]).unwrap();
        match top.command {
            Command::Top { interval } => assert_eq!(interval, "200ms"),
            other => panic!("unexpected command: {other:?}"),
        }

        // Profile subcommands (including export alias)
        let psave = Cli::try_parse_from(["zensmu", "profile", "save", "/tmp/p.toml"]).unwrap();
        assert!(matches!(
            psave.command,
            Command::Profile {
                action: ProfileAction::Save { .. }
            }
        ));
        let pexp = Cli::try_parse_from(["zensmu", "profile", "export", "/tmp/p.toml"]).unwrap();
        assert!(matches!(
            pexp.command,
            Command::Profile {
                action: ProfileAction::Save { .. }
            }
        ));
        let pval = Cli::try_parse_from(["zensmu", "profile", "validate", "/tmp/p.toml"]).unwrap();
        assert!(matches!(
            pval.command,
            Command::Profile {
                action: ProfileAction::Validate { .. }
            }
        ));
        let pdiff = Cli::try_parse_from(["zensmu", "profile", "diff", "/tmp/p.toml"]).unwrap();
        assert!(matches!(
            pdiff.command,
            Command::Profile {
                action: ProfileAction::Diff { .. }
            }
        ));
        let pclear = Cli::try_parse_from(["zensmu", "profile", "clear-guard"]).unwrap();
        assert!(matches!(
            pclear.command,
            Command::Profile {
                action: ProfileAction::ClearGuard
            }
        ));
        let plist =
            Cli::try_parse_from(["zensmu", "profile", "list", "--dir", "/tmp/profiles"]).unwrap();
        match plist.command {
            Command::Profile {
                action: ProfileAction::List { dir },
            } => assert_eq!(dir, PathBuf::from("/tmp/profiles")),
            other => panic!("unexpected command: {other:?}"),
        }

        // Doctor, Status, Rollback, Reset, Completions
        assert!(matches!(
            Cli::try_parse_from(["zensmu", "doctor"]).unwrap().command,
            Command::Doctor
        ));
        assert!(matches!(
            Cli::try_parse_from(["zensmu", "status"]).unwrap().command,
            Command::Status
        ));
        assert!(matches!(
            Cli::try_parse_from(["zensmu", "rollback"]).unwrap().command,
            Command::Rollback
        ));
        assert!(matches!(
            Cli::try_parse_from(["zensmu", "reset"]).unwrap().command,
            Command::Reset
        ));
        match Cli::try_parse_from(["zensmu", "completions", "fish"])
            .unwrap()
            .command
        {
            Command::Completions { shell } => assert_eq!(shell, CompletionShell::Fish),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_persist_subcommands() {
        let st = Cli::try_parse_from(["zensmu", "persist", "status", "--json"]).unwrap();
        assert!(st.json);
        assert!(matches!(
            st.command,
            Command::Persist(PersistCommand {
                action: PersistAction::Status
            })
        ));

        let save_live = Cli::try_parse_from(["zensmu", "persist", "save"]).unwrap();
        match save_live.command {
            Command::Persist(PersistCommand {
                action: PersistAction::Save { from, enable },
            }) => {
                assert_eq!(from, None);
                assert!(!enable);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let save_from_enable = Cli::try_parse_from([
            "zensmu",
            "persist",
            "save",
            "--from",
            "/tmp/daily.toml",
            "--enable",
            "--dry-run",
        ])
        .unwrap();
        assert!(save_from_enable.dry_run);
        match save_from_enable.command {
            Command::Persist(PersistCommand {
                action: PersistAction::Save { from, enable },
            }) => {
                assert_eq!(from, Some(PathBuf::from("/tmp/daily.toml")));
                assert!(enable);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let enable = Cli::try_parse_from(["zensmu", "persist", "enable", "--dry-run"]).unwrap();
        assert!(enable.dry_run);
        assert!(matches!(
            enable.command,
            Command::Persist(PersistCommand {
                action: PersistAction::Enable
            })
        ));

        let disable = Cli::try_parse_from(["zensmu", "persist", "disable"]).unwrap();
        match disable.command {
            Command::Persist(PersistCommand {
                action: PersistAction::Disable { remove_profile },
            }) => assert!(!remove_profile),
            other => panic!("unexpected command: {other:?}"),
        }

        let disable_rm = Cli::try_parse_from([
            "zensmu",
            "persist",
            "disable",
            "--remove-profile",
            "--json",
        ])
        .unwrap();
        assert!(disable_rm.json);
        match disable_rm.command {
            Command::Persist(PersistCommand {
                action: PersistAction::Disable { remove_profile },
            }) => assert!(remove_profile),
            other => panic!("unexpected command: {other:?}"),
        }
    }
}
