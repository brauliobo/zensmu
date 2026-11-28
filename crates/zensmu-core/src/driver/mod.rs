//! Distro-aware `ryzen_smu` kernel driver inspection, loading, and first-class DKMS lifecycle.
//!
//! `zensmu` ships bundled `ryzen_smu` DKMS sources in `/usr/share/zensmu/ryzen_smu-dkms`
//! (automatically built/installed on package install and cleanly removed on package
//! uninstall unless another `ryzen_smu` provider is already present). This module powers:
//! - Unprivileged `zensmu driver status`, `zensmu doctor` diagnostics, and GUI banners.
//! - Privileged `sudo zensmu driver load` (`modprobe ryzen_smu` + autoload config).
//! - Privileged `sudo zensmu driver install` (first-class DKMS build from bundled sources
//!   or upstream `amkillam/ryzen_smu`, auto-installing missing DKMS/header prerequisites,
//!   or optional standalone `.deb`/`.rpm` package download — all with `--dry-run`).
//! - Privileged `sudo zensmu driver uninstall` (clean unload + DKMS removal).

mod cmd;
mod distro;
mod dkms;
mod lifecycle;
mod lockdown;
mod package;
mod status;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};
use std::fmt;

pub use distro::{
    detect_distro, manual_install_commands, parse_os_release, read_kernel_release,
    recommended_driver_command,
};
pub use dkms::{
    dkms_prereq_command, find_all_ryzen_smu_dkms_versions, find_managed_dkms_versions,
    kernel_headers_exist, read_arch_kernel_pkgbase, render_dkms_conf,
};
pub use lifecycle::{install_driver, load_driver, uninstall_driver};
pub use lockdown::{
    detect_secure_boot, parse_lockdown_mode, parse_mokutil_sb_state, parse_secure_boot_efivar,
    secure_boot_status_from, SecureBootStatus, KERNEL_LOCKDOWN_PATH, SECURE_BOOT_EFIVAR_PATH,
};
pub use package::{parse_sha256_for_filename, select_release_assets};
pub use status::{driver_remediation_hint, format_driver_remediation_hint, inspect_driver_status};

/// Upstream `ryzen_smu` DKMS source repository (maintained fork with Zen 4/5 & modern kernel fixes).
pub const RYZEN_SMU_UPSTREAM_REPO: &str = "https://github.com/amkillam/ryzen_smu";
pub const RYZEN_SMU_UPSTREAM_GIT: &str = "https://github.com/amkillam/ryzen_smu.git";
pub const RYZEN_SMU_CHECKSUMS_ASSET: &str = "ryzen-smu-dkms.SHA256SUMS.txt";

/// Bundled `ryzen_smu` DKMS source directory shipped by `zensmu` packages.
pub const BUNDLED_DKMS_SRC_DIR: &str = "/usr/share/zensmu/ryzen_smu-dkms";

/// Marker file placed inside `/usr/src/ryzen_smu-<ver>/` for DKMS trees managed by `zensmu`.
pub const ZENSMU_MANAGED_MARKER: &str = ".zensmu-managed";

/// Header comment written to `/etc/modules-load.d/ryzen_smu.conf` when created by `zensmu`.
pub const ZENSMU_MODULES_LOAD_MARKER: &str = "# Managed by zensmu";

/// Detected Linux distribution family for `ryzen_smu` installation guidance and packaging.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DistroFamily {
    Arch,
    Debian,
    Fedora,
    OpenSuse,
    Other,
}

impl DistroFamily {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Arch => "arch",
            Self::Debian => "debian",
            Self::Fedora => "fedora",
            Self::OpenSuse => "opensuse",
            Self::Other => "other",
        }
    }

    pub fn package_extension(self) -> Option<&'static str> {
        match self {
            Self::Debian => Some(".deb"),
            Self::Fedora | Self::OpenSuse => Some(".rpm"),
            Self::Arch | Self::Other => None,
        }
    }
}

impl fmt::Display for DistroFamily {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Strategy for `zensmu driver install`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DriverInstallMethod {
    /// Standard recommendation: build and register `ryzen_smu` via DKMS (prefers bundled
    /// `/usr/share/zensmu/ryzen_smu-dkms` sources, falls back to upstream `amkillam/ryzen_smu`,
    /// and auto-installs missing DKMS / kernel-header prerequisites).
    #[default]
    Auto,
    /// First-class DKMS build from bundled `/usr/share/zensmu/ryzen_smu-dkms` sources
    /// (falling back to upstream `amkillam/ryzen_smu` when bundled sources are absent).
    Dkms,
    /// Clone fresh upstream `amkillam/ryzen_smu` HEAD and build/register via DKMS.
    Source,
    /// Download and install the standalone `ryzen-smu-dkms` `.deb` or `.rpm` from GitHub Releases.
    Package,
}

impl DriverInstallMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Dkms => "dkms",
            Self::Source => "source",
            Self::Package => "package",
        }
    }
}

impl fmt::Display for DriverInstallMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Unprivileged snapshot of `ryzen_smu` driver, DKMS, autoload, and distro state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverStatus {
    pub sysfs_path: String,
    pub loaded: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub drv_version: Option<String>,
    pub module_installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub module_path: Option<String>,
    pub dkms_installed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dkms_status: Option<String>,
    #[serde(default)]
    pub bundled_dkms_available: bool,
    #[serde(default)]
    pub zensmu_managed_dkms: bool,
    #[serde(default)]
    pub kernel_headers_installed: bool,
    pub autoload_configured: bool,
    pub kernel_release: String,
    pub distro_family: DistroFamily,
    pub distro_name: String,
    pub summary: String,
    pub recommended_command: String,
    pub manual_commands: Vec<String>,
    #[serde(default)]
    pub secure_boot: SecureBootStatus,
}

/// Options controlling `zensmu driver install`.
#[derive(Debug, Clone)]
pub struct DriverInstallOptions {
    pub method: DriverInstallMethod,
    pub release_tag: Option<String>,
    pub configure_autoload: bool,
    pub force: bool,
    pub dry_run: bool,
}

impl Default for DriverInstallOptions {
    fn default() -> Self {
        Self {
            method: DriverInstallMethod::Auto,
            release_tag: None,
            configure_autoload: true,
            force: false,
            dry_run: false,
        }
    }
}

/// Structured output returned by `load_driver` and `install_driver`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DriverActionOutput {
    pub action: String,
    pub dry_run: bool,
    pub method: String,
    pub changed: bool,
    pub loaded: bool,
    pub autoload_configured: bool,
    pub steps: Vec<String>,
    pub notes: Vec<String>,
    pub status: DriverStatus,
}
