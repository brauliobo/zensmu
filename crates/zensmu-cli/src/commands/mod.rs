//! Clap CLI argument definitions for `zensmu`.

mod monitoring;
mod system;
mod tuning;

#[cfg(test)]
mod tests;

pub use monitoring::{
    ExportAction, ExportCommand, ExportServiceAction, RecordAction, RecordCommand, RecordFormatArg,
    RecordServiceAction, TelemetryAction, TestCommand,
};
pub use system::{DriverAction, PersistAction, PersistCommand, ProfileAction};
pub use tuning::{ApuAction, CoAction, PboAction, ShaperAction};

#[cfg(test)]
pub use monitoring::BenchWorkloadArg;
#[cfg(test)]
pub use system::DriverMethodArg;
#[cfg(test)]
pub use tuning::ApuModeArg;

use clap::{ArgAction, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use std::path::PathBuf;

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
