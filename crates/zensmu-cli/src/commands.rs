//! Clap CLI argument definitions for `zensmu`.

use clap::{ArgAction, Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;
use zensmu_core::bench::BenchWorkload;
use zensmu_core::driver::DriverInstallMethod;
use zensmu_core::metrics::DEFAULT_METRICS_LISTEN;
use zensmu_core::recorder::RecordFormat;
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

    /// Inspect, load, or install the ryzen_smu DKMS kernel driver
    Driver {
        #[command(subcommand)]
        action: Option<DriverAction>,
    },

    /// Generate a privacy-safe diagnostic report for GitHub issues or CPU support requests (unprivileged)
    #[command(visible_alias = "bug-report")]
    Report {
        /// Optional description of the issue or CPU support request
        #[arg(long, value_name = "TEXT")]
        note: Option<String>,

        /// Always include non-zero PM table float entries (auto-included when CPU or PM table is unsupported)
        #[arg(long)]
        raw_pm_table: bool,

        /// Skip privileged SMU mailbox probes even if running as root
        #[arg(long)]
        no_mailbox: bool,

        /// Write Markdown report to a file
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,

        /// Open the prefilled GitHub new-issue URL in the browser via xdg-open
        #[arg(long)]
        open: bool,

        /// Create a GitHub issue directly via `gh issue create`
        #[arg(long, alias = "gh")]
        create: bool,
    },

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

    /// Record unprivileged live PM table telemetry (CSV, JSONL, or Phoronix Test Suite XML)
    Record(RecordCommand),

    /// Run per-core Curve Optimizer stability tester / core-cycler (unprivileged; --rollback-on-fail requires root)
    #[command(visible_alias = "bench")]
    Test(TestCommand),

    /// Export live PM table telemetry as an unprivileged Prometheus / OpenMetrics HTTP endpoint
    Export(ExportCommand),

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

    /// Generate the zensmu(1) man page to stdout
    Manpage,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DriverMethodArg {
    /// Use prebuilt ryzen-smu-dkms .deb/.rpm on Debian/Ubuntu/Fedora/openSUSE, guide AUR on Arch, or DKMS source on other distros
    Auto,
    /// Download and install verified ryzen-smu-dkms .deb or .rpm from GitHub Releases
    Package,
    /// Clone upstream amkillam/ryzen_smu and install via `make dkms-install`
    Source,
}

impl From<DriverMethodArg> for DriverInstallMethod {
    fn from(arg: DriverMethodArg) -> Self {
        match arg {
            DriverMethodArg::Auto => DriverInstallMethod::Auto,
            DriverMethodArg::Package => DriverInstallMethod::Package,
            DriverMethodArg::Source => DriverInstallMethod::Source,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum DriverAction {
    /// Inspect ryzen_smu driver, DKMS, autoload, and distro install guidance (unprivileged)
    Status,

    /// Load ryzen_smu via modprobe and configure /etc/modules-load.d/ryzen_smu.conf (requires root)
    Load {
        /// Skip writing /etc/modules-load.d/ryzen_smu.conf
        #[arg(long)]
        no_autoload: bool,
    },

    /// Install and load ryzen_smu DKMS driver via verified GitHub Release package or upstream source (requires root)
    Install {
        /// Installation method (auto, package, or source)
        #[arg(long, value_enum, default_value_t = DriverMethodArg::Auto)]
        method: DriverMethodArg,

        /// Specific GitHub release tag to download ryzen-smu-dkms from (defaults to latest release)
        #[arg(long, value_name = "TAG")]
        release: Option<String>,

        /// Skip writing /etc/modules-load.d/ryzen_smu.conf
        #[arg(long)]
        no_autoload: bool,

        /// Reinstall even if ryzen_smu is already installed for the running kernel
        #[arg(long)]
        force: bool,
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

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordFormatArg {
    Csv,
    Jsonl,
    Pts,
}

impl From<RecordFormatArg> for RecordFormat {
    fn from(value: RecordFormatArg) -> Self {
        match value {
            RecordFormatArg::Csv => RecordFormat::Csv,
            RecordFormatArg::Jsonl => RecordFormat::Jsonl,
            RecordFormatArg::Pts => RecordFormat::Pts,
        }
    }
}

#[derive(Debug, Args)]
pub struct RecordCommand {
    /// Output file path (defaults to ~/.local/share/zensmu/recordings/zensmu-<cpu>-<ts>.<ext>,
    /// or ~/.local/share/zensmu/telemetry.csv when --append is set without -o)
    #[arg(short = 'o', long, value_name = "PATH")]
    pub output: Option<PathBuf>,

    /// Shortcut: set --format pts and write directly to ~/.phoronix-test-suite/test-results/<NAME>/composite.xml
    #[arg(long, value_name = "NAME")]
    pub pts_name: Option<String>,

    /// Output format (inferred from --output extension if omitted, else csv)
    #[arg(short = 'f', long, value_enum)]
    pub format: Option<RecordFormatArg>,

    /// Sampling interval (e.g. 500ms, 1s, 2s; minimum 100ms)
    #[arg(short = 'i', long, default_value = "1s", value_name = "DURATION")]
    pub interval: String,

    /// Optional recording duration before stopping cleanly (e.g. 60s, 10m, 1h)
    #[arg(short = 'd', long, alias = "period", value_name = "DURATION")]
    pub duration: Option<String>,

    /// Optional maximum number of samples to record before exiting
    #[arg(short = 'n', long, value_name = "N")]
    pub samples: Option<u64>,

    /// Optional maximum file size in MiB before size-based log rotation (CSV/JSONL)
    #[arg(long, value_name = "MB")]
    pub max_size: Option<u64>,

    /// Number of rotated backup files to retain when --max-size is set
    #[arg(long, default_value_t = 5, value_name = "N")]
    pub rotate: usize,

    /// Append to an existing file without duplicating the CSV header
    #[arg(long)]
    pub append: bool,

    /// Optional workload tag recorded with each sample (e.g. "prime95", "co-20")
    #[arg(long, value_name = "LABEL")]
    pub tag: Option<String>,

    #[command(subcommand)]
    pub action: Option<RecordAction>,
}

#[derive(Debug, Subcommand)]
pub enum RecordAction {
    /// Inspect, enable, or disable the unprivileged user systemd recording daemon (zensmu-record.service)
    Service {
        #[command(subcommand)]
        action: RecordServiceAction,
    },

    /// Convert an existing CSV or JSONL recording to a Phoronix Test Suite composite.xml
    ExportPts {
        /// Input CSV or JSONL recording path
        #[arg(value_name = "INPUT")]
        input: PathBuf,

        /// Destination XML path (defaults to <input_stem>-pts.xml unless --pts-name is set)
        #[arg(short = 'o', long, value_name = "PATH")]
        output: Option<PathBuf>,

        /// Write directly into ~/.phoronix-test-suite/test-results/<NAME>/composite.xml
        #[arg(long, value_name = "NAME")]
        pts_name: Option<String>,

        /// Custom PTS result title
        #[arg(long, value_name = "TITLE")]
        title: Option<String>,

        /// Custom PTS system/run identifier
        #[arg(long, value_name = "ID")]
        identifier: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum RecordServiceAction {
    /// Inspect status of the unprivileged user zensmu-record.service daemon
    Status,

    /// Enable and start zensmu-record.service via `systemctl --user enable --now`
    Enable,

    /// Disable and stop zensmu-record.service via `systemctl --user disable --now`
    Disable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BenchWorkloadArg {
    /// Interleaved ALU/integer, FP64 FMA, and L1/L2 cache stress
    Mixed,
    /// Multi-lane 64-bit integer ALU/IMUL/Feistel stress
    Integer,
    /// Multi-lane IEEE-754 FP64 FMA multiply-add stress
    #[value(name = "fp", alias = "floating-point")]
    Fp,
    /// Prime-strided 64-byte cache-line L1/L2 read-modify-write stress
    Cache,
}

impl From<BenchWorkloadArg> for BenchWorkload {
    fn from(value: BenchWorkloadArg) -> Self {
        match value {
            BenchWorkloadArg::Mixed => BenchWorkload::Mixed,
            BenchWorkloadArg::Integer => BenchWorkload::Integer,
            BenchWorkloadArg::Fp => BenchWorkload::FloatingPoint,
            BenchWorkloadArg::Cache => BenchWorkload::Cache,
        }
    }
}

#[derive(Debug, Args)]
pub struct TestCommand {
    /// Physical core selection (e.g. "0,1,4..7" or "0-3"; default: all physical cores)
    #[arg(long, value_name = "SPEC")]
    pub cores: Option<String>,

    /// Stress duration per core per pass (e.g. 30s, 2m; 1s..=3600s)
    #[arg(short = 'd', long, default_value = "30s", value_name = "DURATION")]
    pub duration: String,

    /// Number of full passes across the selected cores (1..=100)
    #[arg(short = 'p', long, default_value_t = 1, value_name = "N")]
    pub passes: u32,

    /// Deterministic self-verifying stress workload (mixed, integer, fp, cache)
    #[arg(short = 'w', long, value_enum, default_value_t = BenchWorkloadArg::Mixed)]
    pub workload: BenchWorkloadArg,

    /// Telemetry and progress poll interval (e.g. 500ms, 1s; minimum 100ms)
    #[arg(short = 'i', long, default_value = "500ms", value_name = "DURATION")]
    pub interval: String,

    /// Continue testing remaining cores after an MCE or compute self-check failure
    #[arg(long)]
    pub continue_on_error: bool,

    /// Automatically restore last-state.json if any core fails with MCE/compute errors (requires root)
    #[arg(long)]
    pub rollback_on_fail: bool,
}

#[derive(Debug, Args)]
pub struct ExportCommand {
    /// Listen address for the HTTP/1.1 Prometheus exporter (default: 127.0.0.1:9452)
    #[arg(long, default_value = DEFAULT_METRICS_LISTEN, value_name = "ADDR")]
    pub listen: String,

    /// Omit per-core metrics and export package/socket-level metrics only
    #[arg(long)]
    pub no_per_core: bool,

    #[command(subcommand)]
    pub action: Option<ExportAction>,
}

#[derive(Debug, Subcommand)]
pub enum ExportAction {
    /// Inspect, enable, or disable the unprivileged user systemd exporter daemon (zensmu-exporter.service)
    Service {
        #[command(subcommand)]
        action: ExportServiceAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum ExportServiceAction {
    /// Inspect status of the unprivileged user zensmu-exporter.service daemon
    Status,

    /// Enable and start zensmu-exporter.service via `systemctl --user enable --now`
    Enable,

    /// Disable and stop zensmu-exporter.service via `systemctl --user disable --now`
    Disable,
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
            "zensmu", "pbo", "set", "--ppt", "142W", "--tdc", "110A", "--edc", "170A", "--tctl",
            "85C", "--scalar", "2", "--fmax", "5.65GHz",
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

        let set_alias = Cli::try_parse_from(["zensmu", "pbo", "set", "--tjmax", "85"]).unwrap();
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
        let t2 =
            Cli::try_parse_from(["zensmu", "telemetry", "watch", "--interval", "250ms"]).unwrap();
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

        let disable_rm =
            Cli::try_parse_from(["zensmu", "persist", "disable", "--remove-profile", "--json"])
                .unwrap();
        assert!(disable_rm.json);
        match disable_rm.command {
            Command::Persist(PersistCommand {
                action: PersistAction::Disable { remove_profile },
            }) => assert!(remove_profile),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_report_subcommand_and_aliases() {
        let cli = Cli::try_parse_from([
            "zensmu",
            "report",
            "--note",
            "Ryzen 9950X3D PM table unknown",
            "--raw-pm-table",
            "--no-mailbox",
            "--output",
            "/tmp/zensmu-report.md",
            "--open",
            "--create",
            "--json",
        ])
        .unwrap();

        assert!(cli.json);
        match cli.command {
            Command::Report {
                note,
                raw_pm_table,
                no_mailbox,
                output,
                open,
                create,
            } => {
                assert_eq!(note.as_deref(), Some("Ryzen 9950X3D PM table unknown"));
                assert!(raw_pm_table);
                assert!(no_mailbox);
                assert_eq!(output, Some(PathBuf::from("/tmp/zensmu-report.md")));
                assert!(open);
                assert!(create);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        // Test `bug-report` visible alias, `-o` short flag, and `--gh` alias for `--create`
        let alias_cli = Cli::try_parse_from([
            "zensmu",
            "bug-report",
            "-o",
            "/tmp/out.md",
            "--gh",
            "--dry-run",
            "--yes",
        ])
        .unwrap();
        assert!(alias_cli.dry_run);
        assert!(alias_cli.yes);
        match alias_cli.command {
            Command::Report {
                note,
                raw_pm_table,
                no_mailbox,
                output,
                open,
                create,
            } => {
                assert_eq!(note, None);
                assert!(!raw_pm_table);
                assert!(!no_mailbox);
                assert_eq!(output, Some(PathBuf::from("/tmp/out.md")));
                assert!(!open);
                assert!(create);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_record_direct_and_pts_flags() {
        let default_rec = Cli::try_parse_from(["zensmu", "record"]).unwrap();
        match default_rec.command {
            Command::Record(rec) => {
                assert_eq!(rec.output, None);
                assert_eq!(rec.pts_name, None);
                assert_eq!(rec.format, None);
                assert_eq!(rec.interval, "1s");
                assert_eq!(rec.duration, None);
                assert_eq!(rec.samples, None);
                assert_eq!(rec.max_size, None);
                assert_eq!(rec.rotate, 5);
                assert!(!rec.append);
                assert_eq!(rec.tag, None);
                assert!(rec.action.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let daemon_rec = Cli::try_parse_from([
            "zensmu",
            "record",
            "--interval",
            "2s",
            "--max-size",
            "50",
            "--rotate",
            "5",
            "--append",
            "--tag",
            "co-20",
            "-n",
            "100",
            "-d",
            "60s",
            "-o",
            "/tmp/out.jsonl",
            "-f",
            "jsonl",
        ])
        .unwrap();
        match daemon_rec.command {
            Command::Record(rec) => {
                assert_eq!(rec.output, Some(PathBuf::from("/tmp/out.jsonl")));
                assert_eq!(rec.format, Some(RecordFormatArg::Jsonl));
                assert_eq!(rec.interval, "2s");
                assert_eq!(rec.duration.as_deref(), Some("60s"));
                assert_eq!(rec.samples, Some(100));
                assert_eq!(rec.max_size, Some(50));
                assert_eq!(rec.rotate, 5);
                assert!(rec.append);
                assert_eq!(rec.tag.as_deref(), Some("co-20"));
                assert!(rec.action.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }

        // `--period` alias for `--duration`
        let period_rec = Cli::try_parse_from(["zensmu", "record", "--period", "5s"]).unwrap();
        match period_rec.command {
            Command::Record(rec) => {
                assert_eq!(rec.duration.as_deref(), Some("5s"));
                assert!(rec.action.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }

        // `--pts-name` shortcut
        let pts_rec = Cli::try_parse_from([
            "zensmu",
            "record",
            "--pts-name",
            "ryzen-9700x-co20",
            "-i",
            "500ms",
        ])
        .unwrap();
        match pts_rec.command {
            Command::Record(rec) => {
                assert_eq!(rec.pts_name.as_deref(), Some("ryzen-9700x-co20"));
                assert_eq!(rec.interval, "500ms");
                assert!(rec.action.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_record_service_and_export_pts_subcommands() {
        let st = Cli::try_parse_from(["zensmu", "record", "service", "status", "--json"]).unwrap();
        assert!(st.json);
        assert!(matches!(
            st.command,
            Command::Record(RecordCommand {
                action: Some(RecordAction::Service {
                    action: RecordServiceAction::Status
                }),
                ..
            })
        ));

        let en = Cli::try_parse_from(["zensmu", "record", "service", "enable"]).unwrap();
        assert!(matches!(
            en.command,
            Command::Record(RecordCommand {
                action: Some(RecordAction::Service {
                    action: RecordServiceAction::Enable
                }),
                ..
            })
        ));

        let dis = Cli::try_parse_from(["zensmu", "record", "service", "disable"]).unwrap();
        assert!(matches!(
            dis.command,
            Command::Record(RecordCommand {
                action: Some(RecordAction::Service {
                    action: RecordServiceAction::Disable
                }),
                ..
            })
        ));

        let exp_pts = Cli::try_parse_from([
            "zensmu",
            "record",
            "export-pts",
            "/tmp/telemetry.csv",
            "--pts-name",
            "prime95-run",
            "--title",
            "Prime95 Small FFT",
            "--identifier",
            "co-20",
        ])
        .unwrap();
        match exp_pts.command {
            Command::Record(RecordCommand {
                action:
                    Some(RecordAction::ExportPts {
                        input,
                        output,
                        pts_name,
                        title,
                        identifier,
                    }),
                ..
            }) => {
                assert_eq!(input, PathBuf::from("/tmp/telemetry.csv"));
                assert_eq!(output, None);
                assert_eq!(pts_name.as_deref(), Some("prime95-run"));
                assert_eq!(title.as_deref(), Some("Prime95 Small FFT"));
                assert_eq!(identifier.as_deref(), Some("co-20"));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let exp_out = Cli::try_parse_from([
            "zensmu",
            "record",
            "export-pts",
            "/tmp/telemetry.jsonl",
            "-o",
            "/tmp/composite.xml",
            "--json",
        ])
        .unwrap();
        assert!(exp_out.json);
        match exp_out.command {
            Command::Record(RecordCommand {
                action:
                    Some(RecordAction::ExportPts {
                        input,
                        output,
                        pts_name,
                        title,
                        identifier,
                    }),
                ..
            }) => {
                assert_eq!(input, PathBuf::from("/tmp/telemetry.jsonl"));
                assert_eq!(output, Some(PathBuf::from("/tmp/composite.xml")));
                assert_eq!(pts_name, None);
                assert_eq!(title, None);
                assert_eq!(identifier, None);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_driver_status_load_and_install_subcommands() {
        let bare = Cli::try_parse_from(["zensmu", "driver"]).unwrap();
        assert!(matches!(bare.command, Command::Driver { action: None }));

        let st = Cli::try_parse_from(["zensmu", "driver", "status", "--json"]).unwrap();
        assert!(st.json);
        assert!(matches!(
            st.command,
            Command::Driver {
                action: Some(DriverAction::Status)
            }
        ));

        let load = Cli::try_parse_from(["zensmu", "driver", "load", "--no-autoload"]).unwrap();
        assert!(matches!(
            load.command,
            Command::Driver {
                action: Some(DriverAction::Load { no_autoload: true })
            }
        ));

        let inst = Cli::try_parse_from([
            "zensmu",
            "driver",
            "install",
            "--method",
            "source",
            "--release",
            "v0.2.1",
            "--force",
            "--dry-run",
        ])
        .unwrap();
        assert!(inst.dry_run);
        match inst.command {
            Command::Driver {
                action:
                    Some(DriverAction::Install {
                        method,
                        release,
                        no_autoload,
                        force,
                    }),
            } => {
                assert_eq!(method, DriverMethodArg::Source);
                assert_eq!(release.as_deref(), Some("v0.2.1"));
                assert!(!no_autoload);
                assert!(force);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_test_and_bench_alias_subcommands() {
        let defaults = Cli::try_parse_from(["zensmu", "test"]).unwrap();
        match defaults.command {
            Command::Test(t) => {
                assert_eq!(t.cores, None);
                assert_eq!(t.duration, "30s");
                assert_eq!(t.passes, 1);
                assert_eq!(t.workload, BenchWorkloadArg::Mixed);
                assert_eq!(t.interval, "500ms");
                assert!(!t.continue_on_error);
                assert!(!t.rollback_on_fail);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let custom = Cli::try_parse_from([
            "zensmu",
            "test",
            "--cores",
            "0,1,4..7",
            "-d",
            "45s",
            "-p",
            "3",
            "-w",
            "fp",
            "-i",
            "250ms",
            "--continue-on-error",
            "--rollback-on-fail",
            "--json",
        ])
        .unwrap();
        assert!(custom.json);
        match custom.command {
            Command::Test(t) => {
                assert_eq!(t.cores.as_deref(), Some("0,1,4..7"));
                assert_eq!(t.duration, "45s");
                assert_eq!(t.passes, 3);
                assert_eq!(t.workload, BenchWorkloadArg::Fp);
                assert_eq!(
                    BenchWorkload::from(t.workload),
                    BenchWorkload::FloatingPoint
                );
                assert_eq!(t.interval, "250ms");
                assert!(t.continue_on_error);
                assert!(t.rollback_on_fail);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let bench_alias = Cli::try_parse_from([
            "zensmu",
            "bench",
            "--workload",
            "cache",
            "--duration",
            "10s",
            "--passes",
            "2",
        ])
        .unwrap();
        match bench_alias.command {
            Command::Test(t) => {
                assert_eq!(t.workload, BenchWorkloadArg::Cache);
                assert_eq!(BenchWorkload::from(t.workload), BenchWorkload::Cache);
                assert_eq!(t.duration, "10s");
                assert_eq!(t.passes, 2);
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let integer_wl = Cli::try_parse_from(["zensmu", "test", "-w", "integer"]).unwrap();
        match integer_wl.command {
            Command::Test(t) => {
                assert_eq!(t.workload, BenchWorkloadArg::Integer);
                assert_eq!(BenchWorkload::from(t.workload), BenchWorkload::Integer);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_export_and_export_service_subcommands() {
        let defaults = Cli::try_parse_from(["zensmu", "export"]).unwrap();
        match defaults.command {
            Command::Export(e) => {
                assert_eq!(e.listen, DEFAULT_METRICS_LISTEN);
                assert!(!e.no_per_core);
                assert!(e.action.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let custom = Cli::try_parse_from([
            "zensmu",
            "export",
            "--listen",
            "0.0.0.0:9100",
            "--no-per-core",
        ])
        .unwrap();
        match custom.command {
            Command::Export(e) => {
                assert_eq!(e.listen, "0.0.0.0:9100");
                assert!(e.no_per_core);
                assert!(e.action.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let st = Cli::try_parse_from(["zensmu", "export", "service", "status", "--json"]).unwrap();
        assert!(st.json);
        assert!(matches!(
            st.command,
            Command::Export(ExportCommand {
                action: Some(ExportAction::Service {
                    action: ExportServiceAction::Status
                }),
                ..
            })
        ));

        let en =
            Cli::try_parse_from(["zensmu", "export", "service", "enable", "--dry-run"]).unwrap();
        assert!(en.dry_run);
        assert!(matches!(
            en.command,
            Command::Export(ExportCommand {
                action: Some(ExportAction::Service {
                    action: ExportServiceAction::Enable
                }),
                ..
            })
        ));

        let dis = Cli::try_parse_from(["zensmu", "export", "service", "disable"]).unwrap();
        assert!(matches!(
            dis.command,
            Command::Export(ExportCommand {
                action: Some(ExportAction::Service {
                    action: ExportServiceAction::Disable
                }),
                ..
            })
        ));
    }

    #[test]
    fn parses_manpage_subcommand() {
        let cli = Cli::try_parse_from(["zensmu", "manpage"]).unwrap();
        assert!(matches!(cli.command, Command::Manpage));
    }
}
