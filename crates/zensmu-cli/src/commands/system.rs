//! System, driver, profile, and boot persistence subcommand definitions.

use clap::{Args, Subcommand, ValueEnum};
use std::path::PathBuf;
use zensmu_core::driver::DriverInstallMethod;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DriverMethodArg {
    /// Standard recommendation: build and install ryzen_smu via DKMS (bundled sources or upstream amkillam/ryzen_smu, auto-installing headers/dkms)
    Auto,
    /// First-class DKMS build from bundled /usr/share/zensmu/ryzen_smu-dkms (falling back to upstream amkillam/ryzen_smu)
    Dkms,
    /// Clone fresh upstream amkillam/ryzen_smu HEAD and build via DKMS
    Source,
    /// Download and install standalone ryzen-smu-dkms .deb or .rpm from GitHub Releases
    Package,
}

impl From<DriverMethodArg> for DriverInstallMethod {
    fn from(arg: DriverMethodArg) -> Self {
        match arg {
            DriverMethodArg::Auto => DriverInstallMethod::Auto,
            DriverMethodArg::Dkms => DriverInstallMethod::Dkms,
            DriverMethodArg::Source => DriverInstallMethod::Source,
            DriverMethodArg::Package => DriverInstallMethod::Package,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum DriverAction {
    /// Inspect ryzen_smu driver, DKMS, kernel headers, autoload, and install guidance (unprivileged)
    Status,

    /// Load ryzen_smu via modprobe and configure /etc/modules-load.d/ryzen_smu.conf (requires root)
    Load {
        /// Skip writing /etc/modules-load.d/ryzen_smu.conf
        #[arg(long)]
        no_autoload: bool,
    },

    /// Build, register, and load ryzen_smu via DKMS (auto-installs headers/dkms; requires root)
    Install {
        /// Installation method (auto/dkms recommended, source for fresh upstream clone, or package for standalone .deb/.rpm)
        #[arg(long, value_enum, default_value_t = DriverMethodArg::Auto)]
        method: DriverMethodArg,

        /// Specific GitHub release tag when using --method package (defaults to latest release)
        #[arg(long, value_name = "TAG")]
        release: Option<String>,

        /// Skip writing /etc/modules-load.d/ryzen_smu.conf
        #[arg(long)]
        no_autoload: bool,

        /// Rebuild/reinstall even if ryzen_smu is already installed for the running kernel
        #[arg(long)]
        force: bool,
    },

    /// Unload and remove zensmu-managed ryzen_smu DKMS module registration (requires root)
    #[command(visible_alias = "remove")]
    Uninstall {
        /// Also remove externally managed ryzen_smu DKMS trees under /usr/src/ryzen_smu-*
        #[arg(long)]
        force: bool,
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
