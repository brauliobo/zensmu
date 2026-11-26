//! Distro-aware `ryzen_smu` kernel driver inspection, loading, and DKMS installation.
//!
//! `zensmu` never hard-depends on `ryzen_smu` at package level so installations stay
//! clean across custom kernels and distros. This module powers:
//! - Unprivileged `zensmu driver status`, `zensmu doctor` diagnostics, and GUI banners.
//! - Privileged `sudo zensmu driver load` (`modprobe ryzen_smu` + autoload config).
//! - Privileged `sudo zensmu driver install` (verified GitHub Release `.deb`/`.rpm`
//!   installation or upstream `amkillam/ryzen_smu` DKMS source build, with `--dry-run`).

use crate::error::{Error, Result};
use crate::report::GITHUB_REPO;
use crate::safety::persist::{ensure_modules_load_conf, DEFAULT_MODULES_LOAD_PATH};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Upstream `ryzen_smu` DKMS source repository (maintained fork with Zen 4/5 & modern kernel fixes).
pub const RYZEN_SMU_UPSTREAM_REPO: &str = "https://github.com/amkillam/ryzen_smu";
pub const RYZEN_SMU_UPSTREAM_GIT: &str = "https://github.com/amkillam/ryzen_smu.git";
pub const RYZEN_SMU_CHECKSUMS_ASSET: &str = "ryzen-smu-dkms.SHA256SUMS.txt";

/// EFI variable path exposing UEFI Secure Boot state (5th byte: 1 = enabled, 0 = disabled).
pub const SECURE_BOOT_EFIVAR_PATH: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// Linux Security Module kernel lockdown mode path (bracketed token is active).
pub const KERNEL_LOCKDOWN_PATH: &str = "/sys/kernel/security/lockdown";

/// UEFI Secure Boot and Linux kernel lockdown inspection state.
///
/// Out-of-tree DKMS modules such as `ryzen_smu` cannot be loaded unsigned when
/// Secure Boot is enabled or kernel lockdown is in `integrity`/`confidentiality` mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecureBootStatus {
    #[serde(default)]
    pub secure_boot_enabled: Option<bool>,
    #[serde(default)]
    pub lockdown_mode: Option<String>,
    #[serde(default)]
    pub blocks_unsigned_modules: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

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
    /// Use prebuilt `ryzen-smu-dkms` `.deb`/`.rpm` on Debian/Ubuntu/Fedora/openSUSE,
    /// guide AUR `ryzen_smu-dkms-git` on Arch, or build via DKMS source on other distros.
    #[default]
    Auto,
    /// Download and install the prebuilt `ryzen-smu-dkms` `.deb` or `.rpm` from GitHub Releases.
    Package,
    /// Clone `amkillam/ryzen_smu` and install via `make dkms-install`.
    Source,
}

impl DriverInstallMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Package => "package",
            Self::Source => "source",
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

/// Parse `/etc/os-release` contents into `(DistroFamily, pretty_name)`.
pub fn parse_os_release(content: &str) -> (DistroFamily, String) {
    let mut id = String::new();
    let mut id_like = String::new();
    let mut pretty = String::new();

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((k, v)) = trimmed.split_once('=') else {
            continue;
        };
        let val = v.trim().trim_matches('"').trim_matches('\'').to_string();
        match k.trim() {
            "ID" => id = val.to_ascii_lowercase(),
            "ID_LIKE" => id_like = val.to_ascii_lowercase(),
            "PRETTY_NAME" => pretty = val,
            _ => {}
        }
    }

    let tokens: Vec<&str> = id
        .split_whitespace()
        .chain(id_like.split_whitespace())
        .collect();

    let has = |names: &[&str]| tokens.iter().any(|t| names.contains(t));

    let family = if has(&[
        "arch",
        "cachyos",
        "endeavouros",
        "manjaro",
        "garuda",
        "artix",
    ]) {
        DistroFamily::Arch
    } else if has(&[
        "debian",
        "ubuntu",
        "pop",
        "linuxmint",
        "elementary",
        "zorin",
        "kali",
        "neon",
        "raspbian",
    ]) {
        DistroFamily::Debian
    } else if has(&[
        "fedora",
        "rhel",
        "centos",
        "rocky",
        "almalinux",
        "nobara",
        "bazzite",
        "ol",
    ]) {
        DistroFamily::Fedora
    } else if has(&[
        "opensuse",
        "opensuse-tumbleweed",
        "opensuse-leap",
        "suse",
        "sles",
    ]) {
        DistroFamily::OpenSuse
    } else {
        DistroFamily::Other
    };

    let display = if pretty.is_empty() {
        if id.is_empty() {
            "Linux".to_string()
        } else {
            id
        }
    } else {
        pretty
    };

    (family, display)
}

/// Detect Linux distribution from `/etc/os-release`.
pub fn detect_distro() -> (DistroFamily, String) {
    match fs::read_to_string("/etc/os-release") {
        Ok(content) => parse_os_release(&content),
        Err(_) => (DistroFamily::Other, "Linux".to_string()),
    }
}

/// Read the running kernel release (`uname -r` / `/proc/sys/kernel/osrelease`).
pub fn read_kernel_release() -> String {
    if let Ok(s) = fs::read_to_string("/proc/sys/kernel/osrelease") {
        let t = s.trim();
        if !t.is_empty() {
            return t.to_string();
        }
    }
    Command::new("uname")
        .arg("-r")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string())
}

/// Build distro-specific manual installation commands for `ryzen_smu`.
pub fn manual_install_commands(family: DistroFamily) -> Vec<String> {
    let releases = format!("https://github.com/{GITHUB_REPO}/releases");
    match family {
        DistroFamily::Arch => vec![
            "paru -S ryzen_smu-dkms-git  # AUR (note underscore '_')".to_string(),
            "sudo modprobe ryzen_smu".to_string(),
        ],
        DistroFamily::Debian => vec![
            "sudo zensmu driver install  # downloads & verifies ryzen-smu-dkms .deb".to_string(),
            format!("# Or manually from {releases}:"),
            "sudo apt install linux-headers-$(uname -r) ./ryzen-smu-dkms_*_all.deb".to_string(),
            format!("# Or build from source ({RYZEN_SMU_UPSTREAM_REPO}):"),
            format!("git clone {RYZEN_SMU_UPSTREAM_GIT} /tmp/ryzen_smu && sudo make -C /tmp/ryzen_smu dkms-install && sudo modprobe ryzen_smu"),
        ],
        DistroFamily::Fedora => vec![
            "sudo zensmu driver install  # downloads & verifies ryzen-smu-dkms .rpm".to_string(),
            format!("# Or manually from {releases}:"),
            "sudo dnf install kernel-devel-$(uname -r) ./ryzen-smu-dkms-*.noarch.rpm".to_string(),
            format!("# Or build from source ({RYZEN_SMU_UPSTREAM_REPO}):"),
            format!("git clone {RYZEN_SMU_UPSTREAM_GIT} /tmp/ryzen_smu && sudo make -C /tmp/ryzen_smu dkms-install && sudo modprobe ryzen_smu"),
        ],
        DistroFamily::OpenSuse => vec![
            "sudo zensmu driver install  # downloads & verifies ryzen-smu-dkms .rpm".to_string(),
            format!("# Or manually from {releases}:"),
            "sudo zypper install --allow-unsigned-rpm ./ryzen-smu-dkms-*.noarch.rpm".to_string(),
            format!("# Or build from source ({RYZEN_SMU_UPSTREAM_REPO}):"),
            format!("git clone {RYZEN_SMU_UPSTREAM_GIT} /tmp/ryzen_smu && sudo make -C /tmp/ryzen_smu dkms-install && sudo modprobe ryzen_smu"),
        ],
        DistroFamily::Other => vec![
            "sudo zensmu driver install --method source".to_string(),
            format!("git clone {RYZEN_SMU_UPSTREAM_GIT} /tmp/ryzen_smu && sudo make -C /tmp/ryzen_smu dkms-install && sudo modprobe ryzen_smu"),
        ],
    }
}

/// Recommended single command for a given driver state + distro.
pub fn recommended_driver_command(
    loaded: bool,
    module_installed: bool,
    family: DistroFamily,
) -> String {
    if loaded {
        return "zensmu doctor".to_string();
    }
    if module_installed {
        return "sudo zensmu driver load".to_string();
    }
    match family {
        DistroFamily::Arch => "paru -S ryzen_smu-dkms-git".to_string(),
        DistroFamily::Debian | DistroFamily::Fedora | DistroFamily::OpenSuse => {
            "sudo zensmu driver install".to_string()
        }
        DistroFamily::Other => "sudo zensmu driver install --method source".to_string(),
    }
}

/// Parse raw bytes from `/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c`.
///
/// EFI efivars begin with a 4-byte little-endian attribute header followed by the payload;
/// the 5th byte (`bytes[4]`) is `1` when Secure Boot is enabled and `0` when disabled.
pub fn parse_secure_boot_efivar(bytes: &[u8]) -> Option<bool> {
    match bytes.get(4) {
        Some(1) => Some(true),
        Some(0) => Some(false),
        _ => None,
    }
}

/// Parse stdout from `mokutil --sb-state` as a fallback when the efivar cannot be read.
pub fn parse_mokutil_sb_state(output: &str) -> Option<bool> {
    for line in output.lines() {
        let lower = line.trim().to_ascii_lowercase();
        if lower.contains("secureboot enabled") {
            return Some(true);
        }
        if lower.contains("secureboot disabled") {
            return Some(false);
        }
    }
    None
}

/// Parse `/sys/kernel/security/lockdown` and return the bracketed active mode
/// (e.g. `"none"`, `"integrity"`, or `"confidentiality"`).
pub fn parse_lockdown_mode(content: &str) -> Option<String> {
    let start = content.find('[')?;
    let rest = content.get(start + 1..)?;
    let end = rest.find(']')?;
    let mode = rest.get(..end)?.trim();
    if mode.is_empty() {
        None
    } else {
        Some(mode.to_ascii_lowercase())
    }
}

/// Build a [`SecureBootStatus`] from already-parsed Secure Boot and lockdown signals.
pub fn secure_boot_status_from(
    secure_boot_enabled: Option<bool>,
    lockdown_mode: Option<String>,
) -> SecureBootStatus {
    let lockdown_blocks = matches!(
        lockdown_mode.as_deref(),
        Some("integrity" | "confidentiality")
    );
    let blocks_unsigned_modules = secure_boot_enabled == Some(true) || lockdown_blocks;

    let hint = if blocks_unsigned_modules {
        match (secure_boot_enabled, lockdown_mode.as_deref()) {
            (Some(true), Some(mode @ ("integrity" | "confidentiality"))) => Some(format!(
                "UEFI Secure Boot is enabled (kernel lockdown: {mode}), which blocks unsigned out-of-tree modules like ryzen_smu. Sign the DKMS module with a Machine Owner Key (enroll via `sudo mokutil --import /var/lib/dkms/mok.pub` and reboot) or disable Secure Boot in UEFI firmware."
            )),
            (Some(true), _) => Some(
                "UEFI Secure Boot is enabled, which blocks unsigned out-of-tree modules like ryzen_smu. Sign the DKMS module with a Machine Owner Key (enroll via `sudo mokutil --import /var/lib/dkms/mok.pub` and reboot) or disable Secure Boot in UEFI firmware.".to_string(),
            ),
            (_, Some(mode)) => Some(format!(
                "Kernel lockdown mode is `{mode}`, which blocks unsigned out-of-tree modules like ryzen_smu. Sign the DKMS module with an enrolled MOK (`sudo mokutil --import /var/lib/dkms/mok.pub`) or disable Secure Boot / kernel lockdown."
            )),
            _ => None,
        }
    } else {
        None
    };

    SecureBootStatus {
        secure_boot_enabled,
        lockdown_mode,
        blocks_unsigned_modules,
        hint,
    }
}

fn run_mokutil_sb_state() -> Option<bool> {
    for bin in ["mokutil", "/usr/bin/mokutil", "/sbin/mokutil"] {
        if let Ok(out) = Command::new(bin).arg("--sb-state").output() {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if let Some(state) = parse_mokutil_sb_state(&stdout) {
                    return Some(state);
                }
            }
            return None;
        }
    }
    None
}

/// Detect UEFI Secure Boot and Linux kernel lockdown status without requiring root.
pub fn detect_secure_boot() -> SecureBootStatus {
    let secure_boot_enabled = fs::read(SECURE_BOOT_EFIVAR_PATH)
        .ok()
        .and_then(|bytes| parse_secure_boot_efivar(&bytes))
        .or_else(run_mokutil_sb_state);

    let lockdown_mode = fs::read_to_string(KERNEL_LOCKDOWN_PATH)
        .ok()
        .and_then(|content| parse_lockdown_mode(&content));

    secure_boot_status_from(secure_boot_enabled, lockdown_mode)
}

/// Unprivileged inspection of `ryzen_smu` kernel module, DKMS, autoload, and distro state.
pub fn inspect_driver_status(sysfs_path: &Path) -> DriverStatus {
    let loaded = sysfs_path.exists();
    let drv_version = if loaded {
        fs::read_to_string(sysfs_path.join("drv_version"))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    } else {
        None
    };

    let module_path = run_modinfo_path();
    let module_installed = module_path.is_some() || loaded;

    let dkms_status = run_dkms_status();
    let dkms_installed = dkms_status.is_some();

    let autoload_configured = fs::read_to_string(DEFAULT_MODULES_LOAD_PATH)
        .map(|s| s.lines().any(|l| l.trim() == "ryzen_smu"))
        .unwrap_or(false);

    let kernel_release = read_kernel_release();
    let (distro_family, distro_name) = detect_distro();
    let recommended_command = recommended_driver_command(loaded, module_installed, distro_family);
    let manual_commands = manual_install_commands(distro_family);
    let secure_boot = detect_secure_boot();

    let summary = if loaded {
        match &drv_version {
            Some(v) => format!("ryzen_smu {v} is loaded at {}", sysfs_path.display()),
            None => format!("ryzen_smu is loaded at {}", sysfs_path.display()),
        }
    } else if module_installed {
        if secure_boot.blocks_unsigned_modules {
            format!(
                "ryzen_smu kernel module is installed for kernel {kernel_release} but not currently loaded (Secure Boot / kernel lockdown may block unsigned modules)"
            )
        } else {
            format!(
                "ryzen_smu kernel module is installed for kernel {kernel_release} but not currently loaded"
            )
        }
    } else if dkms_installed {
        format!(
            "ryzen_smu DKMS source is present but no compiled module was found for kernel {kernel_release}"
        )
    } else {
        format!(
            "ryzen_smu kernel module is not installed for kernel {kernel_release} ({distro_name})"
        )
    };

    DriverStatus {
        sysfs_path: sysfs_path.display().to_string(),
        loaded,
        drv_version,
        module_installed,
        module_path,
        dkms_installed,
        dkms_status,
        autoload_configured,
        kernel_release,
        distro_family,
        distro_name,
        summary,
        recommended_command,
        manual_commands,
        secure_boot,
    }
}

/// Format a concise, distro- and Secure-Boot-aware remediation hint from a [`DriverStatus`].
pub fn format_driver_remediation_hint(st: &DriverStatus) -> String {
    if st.loaded {
        return format!("ryzen_smu is loaded at {}", st.sysfs_path);
    }
    let base = if st.module_installed {
        "ryzen_smu is installed but not loaded — run: sudo modprobe ryzen_smu (or: sudo zensmu driver load)".to_string()
    } else {
        match st.distro_family {
            DistroFamily::Arch => {
                "ryzen_smu is not installed — install from AUR: paru -S ryzen_smu-dkms-git (or inspect with: zensmu driver status)".to_string()
            }
            DistroFamily::Debian | DistroFamily::Fedora | DistroFamily::OpenSuse => {
                format!(
                    "ryzen_smu is not installed ({}) — run: sudo zensmu driver install (or see: zensmu driver status)",
                    st.distro_name
                )
            }
            DistroFamily::Other => {
                "ryzen_smu is not installed — run: sudo zensmu driver install --method source (or see: zensmu driver status)".to_string()
            }
        }
    };

    if st.secure_boot.blocks_unsigned_modules {
        if let Some(ref sb_hint) = st.secure_boot.hint {
            return format!("{base} — Secure Boot / MOK signing required: {sb_hint}");
        }
        return format!(
            "{base} — Secure Boot / kernel lockdown is active; sign ryzen_smu with an enrolled MOK (`sudo mokutil --import /var/lib/dkms/mok.pub`) or disable Secure Boot"
        );
    }

    base
}

/// Concise, distro-aware remediation hint when `Error::DriverNotLoaded` occurs.
pub fn driver_remediation_hint(sysfs_path: &Path) -> String {
    let st = inspect_driver_status(sysfs_path);
    format_driver_remediation_hint(&st)
}

/// Load `ryzen_smu` via `modprobe` and optionally configure `/etc/modules-load.d/ryzen_smu.conf`.
pub fn load_driver(
    sysfs_path: &Path,
    configure_autoload: bool,
    dry_run: bool,
) -> Result<DriverActionOutput> {
    let before = inspect_driver_status(sysfs_path);
    let mut steps = Vec::new();
    let mut notes = Vec::new();

    if !before.loaded {
        steps.push("modprobe ryzen_smu".to_string());
        if before.secure_boot.blocks_unsigned_modules {
            if let Some(ref hint) = before.secure_boot.hint {
                notes.push(hint.clone());
            }
        }
    } else {
        notes.push(format!(
            "ryzen_smu already loaded at {}",
            sysfs_path.display()
        ));
    }
    if configure_autoload && !before.autoload_configured {
        steps.push(format!("Write 'ryzen_smu' to {DEFAULT_MODULES_LOAD_PATH}"));
    }

    if dry_run {
        return Ok(DriverActionOutput {
            action: "load".to_string(),
            dry_run: true,
            method: "modprobe".to_string(),
            changed: !steps.is_empty(),
            loaded: before.loaded,
            autoload_configured: before.autoload_configured,
            steps,
            notes,
            status: before,
        });
    }

    require_root("loading kernel module ryzen_smu")?;

    let mut changed = false;
    if !before.loaded {
        let out = run_cmd("modprobe", &["ryzen_smu"], &[])?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let mut detail = if stderr.is_empty() {
                "modprobe ryzen_smu failed".to_string()
            } else {
                format!("modprobe ryzen_smu failed: {stderr}")
            };
            if before.secure_boot.blocks_unsigned_modules {
                if let Some(ref hint) = before.secure_boot.hint {
                    detail.push_str("; ");
                    detail.push_str(hint);
                }
            }
            return Err(Error::DriverNotLoaded {
                path: format!("{} ({detail})", sysfs_path.display()),
            });
        }
        changed = true;
    }

    if configure_autoload && !before.autoload_configured {
        ensure_modules_load_conf(Path::new(DEFAULT_MODULES_LOAD_PATH))?;
        changed = true;
    }

    let after = inspect_driver_status(sysfs_path);
    if !after.loaded {
        return Err(Error::DriverNotLoaded {
            path: format!(
                "{} (modprobe exited 0 but sysfs path did not appear; check dmesg)",
                sysfs_path.display()
            ),
        });
    }

    Ok(DriverActionOutput {
        action: "load".to_string(),
        dry_run: false,
        method: "modprobe".to_string(),
        changed,
        loaded: after.loaded,
        autoload_configured: after.autoload_configured,
        steps,
        notes,
        status: after,
    })
}

/// Install and load `ryzen_smu` via verified GitHub Release `.deb`/`.rpm` or upstream DKMS source.
pub fn install_driver(
    sysfs_path: &Path,
    opts: &DriverInstallOptions,
) -> Result<DriverActionOutput> {
    let before = inspect_driver_status(sysfs_path);

    // If module is already installed for the running kernel and --force is not set, just load it.
    if before.module_installed && !opts.force {
        let mut out = load_driver(sysfs_path, opts.configure_autoload, opts.dry_run)?;
        out.action = "install".to_string();
        out.method = "existing".to_string();
        out.notes.insert(
            0,
            format!(
                "ryzen_smu is already installed for kernel {} (pass --force to reinstall)",
                before.kernel_release
            ),
        );
        return Ok(out);
    }

    let effective_method = match opts.method {
        DriverInstallMethod::Auto => match before.distro_family {
            DistroFamily::Debian | DistroFamily::Fedora | DistroFamily::OpenSuse => {
                DriverInstallMethod::Package
            }
            DistroFamily::Arch => {
                return Err(Error::InvalidInput(
                    "On Arch Linux, install ryzen_smu from AUR so pacman tracks it: `paru -S ryzen_smu-dkms-git` (or pass `--method source` to build via DKMS directly)".to_string(),
                ));
            }
            DistroFamily::Other => DriverInstallMethod::Source,
        },
        explicit => explicit,
    };

    if effective_method == DriverInstallMethod::Package
        && before.distro_family.package_extension().is_none()
    {
        return Err(Error::InvalidInput(format!(
            "No prebuilt .deb/.rpm package method for distro '{}'; use `--method source` (or on Arch: `paru -S ryzen_smu-dkms-git`)",
            before.distro_name
        )));
    }

    match effective_method {
        DriverInstallMethod::Package => install_via_release_package(sysfs_path, &before, opts),
        DriverInstallMethod::Source => install_via_dkms_source(sysfs_path, &before, opts),
        DriverInstallMethod::Auto => Err(Error::InvalidInput(
            "unable to resolve driver install method".to_string(),
        )),
    }
}

fn install_via_release_package(
    sysfs_path: &Path,
    before: &DriverStatus,
    opts: &DriverInstallOptions,
) -> Result<DriverActionOutput> {
    let ext = before.distro_family.package_extension().ok_or_else(|| {
        Error::InvalidInput(format!(
            "distro '{}' does not use .deb or .rpm packages",
            before.distro_name
        ))
    })?;
    let api_url = match &opts.release_tag {
        Some(tag) => format!("https://api.github.com/repos/{GITHUB_REPO}/releases/tags/{tag}"),
        None => format!("https://api.github.com/repos/{GITHUB_REPO}/releases/latest"),
    };

    let mut steps = vec![
        format!("Query GitHub Release metadata: {api_url}"),
        format!("Download matching ryzen-smu-dkms*{ext} and verify SHA256 against {RYZEN_SMU_CHECKSUMS_ASSET}"),
    ];
    match before.distro_family {
        DistroFamily::Debian => {
            steps.push(format!(
                "apt-get install -y linux-headers-{} <downloaded.deb>",
                before.kernel_release
            ));
        }
        DistroFamily::Fedora => {
            steps.push(format!(
                "dnf install -y kernel-devel-{} <downloaded.rpm>",
                before.kernel_release
            ));
        }
        DistroFamily::OpenSuse => {
            steps.push(
                "zypper --non-interactive install --allow-unsigned-rpm <downloaded.rpm>"
                    .to_string(),
            );
        }
        DistroFamily::Arch | DistroFamily::Other => {}
    }
    if opts.configure_autoload {
        steps.push(format!("Ensure 'ryzen_smu' in {DEFAULT_MODULES_LOAD_PATH}"));
    }
    steps.push("modprobe ryzen_smu".to_string());

    if opts.dry_run {
        let mut dry_notes = vec![
            "Dry-run only: no network download, package install, or module load executed."
                .to_string(),
        ];
        if !before.loaded && before.secure_boot.blocks_unsigned_modules {
            if let Some(ref hint) = before.secure_boot.hint {
                dry_notes.push(hint.clone());
            }
        }
        return Ok(DriverActionOutput {
            action: "install".to_string(),
            dry_run: true,
            method: "package".to_string(),
            changed: true,
            loaded: before.loaded,
            autoload_configured: before.autoload_configured,
            steps,
            notes: dry_notes,
            status: before.clone(),
        });
    }

    require_root("installing ryzen-smu-dkms package")?;
    ensure_command_available("curl", "install `curl` or use `--method source`")?;

    let meta_out = run_cmd(
        "curl",
        &[
            "-fsSL",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "User-Agent: zensmu",
            &api_url,
        ],
        &[],
    )?;
    if !meta_out.status.success() {
        return Err(Error::InvalidInput(format!(
            "failed to fetch GitHub release metadata from {api_url}: {}",
            String::from_utf8_lossy(&meta_out.stderr).trim()
        )));
    }
    let meta_json = String::from_utf8_lossy(&meta_out.stdout);
    let (pkg_url, sums_url) = select_release_assets(&meta_json, ext)?;
    let filename = pkg_url
        .rsplit('/')
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or("ryzen-smu-dkms.pkg");

    let workdir = make_temp_dir("zensmu-driver-pkg")?;
    let pkg_path = workdir.join(filename);
    let pkg_path_str = pkg_path.display().to_string();

    let mut notes = vec![format!("Downloaded {pkg_url}")];

    let result = (|| -> Result<()> {
        let dl_out = run_cmd("curl", &["-fsSL", "-o", &pkg_path_str, &pkg_url], &[])?;
        if !dl_out.status.success() {
            return Err(Error::InvalidInput(format!(
                "failed to download {pkg_url}: {}",
                String::from_utf8_lossy(&dl_out.stderr).trim()
            )));
        }

        if let Some(sums_url) = sums_url {
            let sums_out = run_cmd("curl", &["-fsSL", &sums_url], &[])?;
            if !sums_out.status.success() {
                return Err(Error::InvalidInput(format!(
                    "failed to download checksums {sums_url}: {}",
                    String::from_utf8_lossy(&sums_out.stderr).trim()
                )));
            }
            let sums_txt = String::from_utf8_lossy(&sums_out.stdout);
            let expected = parse_sha256_for_filename(&sums_txt, filename).ok_or_else(|| {
                Error::InvalidInput(format!(
                    "checksum file {RYZEN_SMU_CHECKSUMS_ASSET} does not contain an entry for {filename}"
                ))
            })?;
            let actual = compute_sha256_via_tool(&pkg_path)?;
            if actual != expected {
                return Err(Error::InvalidInput(format!(
                    "SHA256 mismatch for {filename}: expected {expected}, got {actual}"
                )));
            }
            notes.push(format!("Verified SHA256 ({expected})"));
        }

        match before.distro_family {
            DistroFamily::Debian => {
                let hdr_pkg = format!("linux-headers-{}", before.kernel_release);
                let out = run_cmd(
                    "apt-get",
                    &["install", "-y", &hdr_pkg, &pkg_path_str],
                    &[("DEBIAN_FRONTEND", "noninteractive")],
                )?;
                if !out.status.success() {
                    // Retry without exact kernel header package in case distro uses generic headers
                    let retry = run_cmd(
                        "apt-get",
                        &["install", "-y", &pkg_path_str],
                        &[("DEBIAN_FRONTEND", "noninteractive")],
                    )?;
                    if !retry.status.success() {
                        return Err(Error::InvalidInput(format!(
                            "apt-get install failed: {}",
                            String::from_utf8_lossy(&retry.stderr).trim()
                        )));
                    }
                    notes.push(format!(
                        "Note: {hdr_pkg} was unavailable; installed package directly — ensure matching kernel headers are installed if DKMS build was skipped"
                    ));
                }
            }
            DistroFamily::Fedora => {
                let hdr_pkg = format!("kernel-devel-{}", before.kernel_release);
                let out = run_cmd("dnf", &["install", "-y", &hdr_pkg, &pkg_path_str], &[])?;
                if !out.status.success() {
                    let retry = run_cmd("dnf", &["install", "-y", &pkg_path_str], &[])?;
                    if !retry.status.success() {
                        return Err(Error::InvalidInput(format!(
                            "dnf install failed: {}",
                            String::from_utf8_lossy(&retry.stderr).trim()
                        )));
                    }
                }
            }
            DistroFamily::OpenSuse => {
                let out = run_cmd(
                    "zypper",
                    &[
                        "--non-interactive",
                        "install",
                        "--allow-unsigned-rpm",
                        &pkg_path_str,
                    ],
                    &[],
                )?;
                if !out.status.success() {
                    return Err(Error::InvalidInput(format!(
                        "zypper install failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    )));
                }
            }
            DistroFamily::Arch | DistroFamily::Other => {}
        }

        if opts.configure_autoload {
            ensure_modules_load_conf(Path::new(DEFAULT_MODULES_LOAD_PATH))?;
        }
        let mod_out = run_cmd("modprobe", &["ryzen_smu"], &[])?;
        if !mod_out.status.success() {
            let extra = if before.secure_boot.blocks_unsigned_modules {
                before.secure_boot.hint.as_deref().unwrap_or(
                    "Secure Boot / kernel lockdown blocks unsigned modules; sign with MOK",
                )
            } else {
                "check Secure Boot / kernel headers / dkms status"
            };
            return Err(Error::DriverNotLoaded {
                path: format!(
                    "{} (package installed, but `modprobe ryzen_smu` failed: {}; {extra})",
                    sysfs_path.display(),
                    String::from_utf8_lossy(&mod_out.stderr).trim()
                ),
            });
        }
        Ok(())
    })();

    let _ = fs::remove_dir_all(&workdir);
    result?;

    let after = inspect_driver_status(sysfs_path);
    if !after.loaded && after.secure_boot.blocks_unsigned_modules {
        if let Some(ref hint) = after.secure_boot.hint {
            notes.push(hint.clone());
        }
    }
    Ok(DriverActionOutput {
        action: "install".to_string(),
        dry_run: false,
        method: "package".to_string(),
        changed: true,
        loaded: after.loaded,
        autoload_configured: after.autoload_configured,
        steps,
        notes,
        status: after,
    })
}

fn install_via_dkms_source(
    sysfs_path: &Path,
    before: &DriverStatus,
    opts: &DriverInstallOptions,
) -> Result<DriverActionOutput> {
    let mut steps = vec![
        format!("git clone --depth 1 {RYZEN_SMU_UPSTREAM_GIT} <tmpdir>"),
        "make -C <tmpdir> dkms-install".to_string(),
    ];
    if opts.configure_autoload {
        steps.push(format!("Ensure 'ryzen_smu' in {DEFAULT_MODULES_LOAD_PATH}"));
    }
    steps.push("modprobe ryzen_smu".to_string());

    if opts.dry_run {
        let mut dry_notes = vec![
            "Dry-run only: requires `git`, `make`, `gcc`, `dkms`, and kernel headers for the running kernel."
                .to_string(),
        ];
        if !before.loaded && before.secure_boot.blocks_unsigned_modules {
            if let Some(ref hint) = before.secure_boot.hint {
                dry_notes.push(hint.clone());
            }
        }
        return Ok(DriverActionOutput {
            action: "install".to_string(),
            dry_run: true,
            method: "source".to_string(),
            changed: true,
            loaded: before.loaded,
            autoload_configured: before.autoload_configured,
            steps,
            notes: dry_notes,
            status: before.clone(),
        });
    }

    require_root("building and installing ryzen_smu via DKMS")?;
    ensure_command_available("git", "install `git` first")?;
    ensure_command_available("make", "install `make` / build tools first")?;
    ensure_command_available("dkms", "install `dkms` and kernel headers first")?;

    let workdir = make_temp_dir("zensmu-driver-src")?;
    let src_dir = workdir.join("ryzen_smu");
    let src_str = src_dir.display().to_string();

    let result = (|| -> Result<()> {
        let clone_out = run_cmd(
            "git",
            &["clone", "--depth", "1", RYZEN_SMU_UPSTREAM_GIT, &src_str],
            &[],
        )?;
        if !clone_out.status.success() {
            return Err(Error::InvalidInput(format!(
                "git clone failed: {}",
                String::from_utf8_lossy(&clone_out.stderr).trim()
            )));
        }

        // If force reinstalling over an existing 0.1.7 DKMS tree, remove old registration first.
        if opts.force {
            let _ = run_cmd("make", &["-C", &src_str, "dkms-uninstall"], &[]);
        }

        let make_out = run_cmd("make", &["-C", &src_str, "dkms-install"], &[])?;
        if !make_out.status.success() {
            return Err(Error::InvalidInput(format!(
                "make dkms-install failed (ensure kernel headers for {} are installed): {}",
                before.kernel_release,
                String::from_utf8_lossy(&make_out.stderr).trim()
            )));
        }

        if opts.configure_autoload {
            ensure_modules_load_conf(Path::new(DEFAULT_MODULES_LOAD_PATH))?;
        }

        let mod_out = run_cmd("modprobe", &["ryzen_smu"], &[])?;
        if !mod_out.status.success() {
            let extra = if before.secure_boot.blocks_unsigned_modules {
                before.secure_boot.hint.as_deref().unwrap_or(
                    "Secure Boot / kernel lockdown blocks unsigned modules; sign with MOK",
                )
            } else {
                "check Secure Boot / dmesg"
            };
            return Err(Error::DriverNotLoaded {
                path: format!(
                    "{} (DKMS built, but `modprobe ryzen_smu` failed: {}; {extra})",
                    sysfs_path.display(),
                    String::from_utf8_lossy(&mod_out.stderr).trim()
                ),
            });
        }
        Ok(())
    })();

    let _ = fs::remove_dir_all(&workdir);
    result?;

    let after = inspect_driver_status(sysfs_path);
    let mut notes = vec![format!(
        "Built and installed via DKMS from {RYZEN_SMU_UPSTREAM_REPO}"
    )];
    if !after.loaded && after.secure_boot.blocks_unsigned_modules {
        if let Some(ref hint) = after.secure_boot.hint {
            notes.push(hint.clone());
        }
    }
    Ok(DriverActionOutput {
        action: "install".to_string(),
        dry_run: false,
        method: "source".to_string(),
        changed: true,
        loaded: after.loaded,
        autoload_configured: after.autoload_configured,
        steps,
        notes,
        status: after,
    })
}

/// Extract `(package_download_url, optional_checksums_url)` from GitHub Release JSON.
pub fn select_release_assets(release_json: &str, ext: &str) -> Result<(String, Option<String>)> {
    let parsed: serde_json::Value = serde_json::from_str(release_json)
        .map_err(|e| Error::Serde(format!("invalid GitHub release JSON: {e}")))?;

    let assets = parsed
        .get("assets")
        .and_then(|v| v.as_array())
        .ok_or_else(|| {
            Error::InvalidInput("GitHub release metadata does not contain an `assets` array".into())
        })?;

    let mut pkg_url: Option<String> = None;
    let mut sums_url: Option<String> = None;

    for asset in assets {
        let Some(name) = asset.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(url) = asset.get("browser_download_url").and_then(|v| v.as_str()) else {
            continue;
        };
        if name.starts_with("ryzen-smu-dkms") && name.ends_with(ext) {
            pkg_url = Some(url.to_string());
        } else if name == RYZEN_SMU_CHECKSUMS_ASSET {
            sums_url = Some(url.to_string());
        }
    }

    let pkg_url = pkg_url.ok_or_else(|| {
        Error::InvalidInput(format!(
            "no `ryzen-smu-dkms*{ext}` asset found in GitHub Release (use `--method source` to build from upstream DKMS)"
        ))
    })?;

    Ok((pkg_url, sums_url))
}

/// Parse a standard `sha256sum` output text and return the lowercase hex digest for `filename`.
pub fn parse_sha256_for_filename(sums_txt: &str, filename: &str) -> Option<String> {
    for line in sums_txt.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.split_whitespace();
        let hash = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        if name == filename && hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

fn compute_sha256_via_tool(path: &Path) -> Result<String> {
    let out = run_cmd("sha256sum", &[&path.display().to_string()], &[])?;
    if !out.status.success() {
        return Err(Error::InvalidInput(format!(
            "sha256sum failed on {}: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .split_whitespace()
        .next()
        .filter(|h| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|h| h.to_ascii_lowercase())
        .ok_or_else(|| Error::InvalidInput("unexpected sha256sum output".to_string()))
}

fn run_modinfo_path() -> Option<String> {
    for bin in ["modinfo", "/sbin/modinfo", "/usr/sbin/modinfo"] {
        if let Ok(out) = Command::new(bin).args(["-n", "ryzen_smu"]).output() {
            if out.status.success() {
                let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !path.is_empty() {
                    return Some(path);
                }
            }
            // Binary existed and ran; no need to try sbin aliases
            return None;
        }
    }
    None
}

fn run_dkms_status() -> Option<String> {
    let out = Command::new("dkms")
        .args(["status", "ryzen_smu"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn require_root(action: &str) -> Result<()> {
    if unsafe { libc::geteuid() != 0 } {
        return Err(Error::PermissionDenied {
            path: format!("{action} requires root (run with sudo or pkexec)"),
        });
    }
    Ok(())
}

fn ensure_command_available(cmd: &str, hint: &str) -> Result<()> {
    let ok = Command::new("sh")
        .args(["-c", &format!("command -v {cmd} >/dev/null 2>&1")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        return Err(Error::InvalidInput(format!(
            "required command `{cmd}` not found ({hint})"
        )));
    }
    Ok(())
}

fn run_cmd(bin: &str, args: &[&str], envs: &[(&str, &str)]) -> Result<std::process::Output> {
    let mut cmd = Command::new(bin);
    cmd.args(args);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    cmd.output().map_err(|e| Error::Io {
        path: bin.to_string(),
        source: e,
    })
}

fn make_temp_dir(prefix: &str) -> Result<PathBuf> {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("{prefix}-{pid}-{nanos}"));
    fs::create_dir_all(&dir).map_err(|e| Error::Io {
        path: dir.display().to_string(),
        source: e,
    })?;
    Ok(dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_major_linux_distros_from_os_release() {
        let (fam, name) = parse_os_release(
            "NAME=\"CachyOS Linux\"\nID=cachyos\nID_LIKE=arch\nPRETTY_NAME=\"CachyOS\"\n",
        );
        assert_eq!(fam, DistroFamily::Arch);
        assert_eq!(name, "CachyOS");

        let (fam, name) =
            parse_os_release("ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\n");
        assert_eq!(fam, DistroFamily::Debian);
        assert_eq!(name, "Ubuntu 24.04.1 LTS");

        let (fam, _) = parse_os_release("ID=fedora\nPRETTY_NAME=\"Fedora Linux 41\"\n");
        assert_eq!(fam, DistroFamily::Fedora);

        let (fam, _) = parse_os_release(
            "ID=\"opensuse-tumbleweed\"\nID_LIKE=\"opensuse suse\"\nPRETTY_NAME=\"openSUSE Tumbleweed\"\n",
        );
        assert_eq!(fam, DistroFamily::OpenSuse);

        let (fam, _) = parse_os_release("ID=nixos\nPRETTY_NAME=\"NixOS 25.05\"\n");
        assert_eq!(fam, DistroFamily::Other);
    }

    #[test]
    fn selects_deb_and_rpm_assets_from_release_json() {
        let json = r#"{
            "tag_name": "v0.2.1",
            "assets": [
                {"name": "zensmu_0.2.1_amd64.deb", "browser_download_url": "https://example.invalid/zensmu.deb"},
                {"name": "ryzen-smu-dkms_0.1.7.r191.gd298366_all.deb", "browser_download_url": "https://example.invalid/ryzen-smu-dkms.deb"},
                {"name": "ryzen-smu-dkms-0.1.7.r191.gd298366-1.noarch.rpm", "browser_download_url": "https://example.invalid/ryzen-smu-dkms.rpm"},
                {"name": "ryzen-smu-dkms.SHA256SUMS.txt", "browser_download_url": "https://example.invalid/sums.txt"}
            ]
        }"#;

        let (deb, sums) = select_release_assets(json, ".deb").unwrap();
        assert_eq!(deb, "https://example.invalid/ryzen-smu-dkms.deb");
        assert_eq!(sums.as_deref(), Some("https://example.invalid/sums.txt"));

        let (rpm, sums) = select_release_assets(json, ".rpm").unwrap();
        assert_eq!(rpm, "https://example.invalid/ryzen-smu-dkms.rpm");
        assert_eq!(sums.as_deref(), Some("https://example.invalid/sums.txt"));
    }

    #[test]
    fn parses_sha256sum_entries_accurately() {
        let h1 = "a".repeat(64);
        let h2 = "b".repeat(64);
        let txt = format!(
            "{h1}  ryzen-smu-dkms_0.1.7_all.deb\n{h2} *ryzen-smu-dkms-0.1.7-1.noarch.rpm\n"
        );
        assert_eq!(
            parse_sha256_for_filename(&txt, "ryzen-smu-dkms_0.1.7_all.deb").as_deref(),
            Some(h1.as_str())
        );
        assert_eq!(
            parse_sha256_for_filename(&txt, "ryzen-smu-dkms-0.1.7-1.noarch.rpm").as_deref(),
            Some(h2.as_str())
        );
        assert!(parse_sha256_for_filename(&txt, "other.deb").is_none());
    }

    #[test]
    fn recommended_command_adapts_to_driver_state_and_distro() {
        assert_eq!(
            recommended_driver_command(true, true, DistroFamily::Debian),
            "zensmu doctor"
        );
        assert_eq!(
            recommended_driver_command(false, true, DistroFamily::Debian),
            "sudo zensmu driver load"
        );
        assert_eq!(
            recommended_driver_command(false, false, DistroFamily::Arch),
            "paru -S ryzen_smu-dkms-git"
        );
        assert_eq!(
            recommended_driver_command(false, false, DistroFamily::Debian),
            "sudo zensmu driver install"
        );
        assert_eq!(
            recommended_driver_command(false, false, DistroFamily::Other),
            "sudo zensmu driver install --method source"
        );
    }

    #[test]
    fn parses_secure_boot_efivar_and_mokutil_output() {
        // 4-byte EFI attribute header + payload byte
        assert_eq!(
            parse_secure_boot_efivar(&[0x06, 0x00, 0x00, 0x00, 0x01]),
            Some(true)
        );
        assert_eq!(
            parse_secure_boot_efivar(&[0x06, 0x00, 0x00, 0x00, 0x00]),
            Some(false)
        );
        assert_eq!(
            parse_secure_boot_efivar(&[0x06, 0x00, 0x00, 0x00, 0x02]),
            None
        );
        assert_eq!(parse_secure_boot_efivar(&[0x06, 0x00, 0x00, 0x00]), None);
        assert_eq!(parse_secure_boot_efivar(&[]), None);

        assert_eq!(parse_mokutil_sb_state("SecureBoot enabled\n"), Some(true));
        assert_eq!(
            parse_mokutil_sb_state("SecureBoot disabled\nPlatform is in Setup Mode\n"),
            Some(false)
        );
        assert_eq!(
            parse_mokutil_sb_state("EFI variables are not supported on this system\n"),
            None
        );
    }

    #[test]
    fn parses_kernel_lockdown_modes() {
        assert_eq!(
            parse_lockdown_mode("[none] integrity confidentiality\n").as_deref(),
            Some("none")
        );
        assert_eq!(
            parse_lockdown_mode("none [integrity] confidentiality\n").as_deref(),
            Some("integrity")
        );
        assert_eq!(
            parse_lockdown_mode("none integrity [confidentiality]").as_deref(),
            Some("confidentiality")
        );
        assert_eq!(parse_lockdown_mode("none integrity confidentiality"), None);
        assert_eq!(parse_lockdown_mode("[]"), None);
        assert_eq!(parse_lockdown_mode("[unclosed"), None);
        assert_eq!(parse_lockdown_mode(""), None);
    }

    #[test]
    fn computes_blocks_unsigned_modules_and_remediation_hint() {
        let disabled = secure_boot_status_from(Some(false), Some("none".into()));
        assert!(!disabled.blocks_unsigned_modules);
        assert!(disabled.hint.is_none());

        let unknown = secure_boot_status_from(None, None);
        assert!(!unknown.blocks_unsigned_modules);
        assert!(unknown.hint.is_none());

        let sb_only = secure_boot_status_from(Some(true), Some("none".into()));
        assert!(sb_only.blocks_unsigned_modules);
        assert!(sb_only
            .hint
            .as_deref()
            .unwrap()
            .contains("mokutil --import"));

        let integrity_only = secure_boot_status_from(Some(false), Some("integrity".into()));
        assert!(integrity_only.blocks_unsigned_modules);
        assert!(integrity_only
            .hint
            .as_deref()
            .unwrap()
            .contains("integrity"));

        let confidentiality = secure_boot_status_from(None, Some("confidentiality".into()));
        assert!(confidentiality.blocks_unsigned_modules);
        assert!(confidentiality
            .hint
            .as_deref()
            .unwrap()
            .contains("confidentiality"));

        let both = secure_boot_status_from(Some(true), Some("integrity".into()));
        assert!(both.blocks_unsigned_modules);
        let hint = both.hint.as_deref().unwrap();
        assert!(hint.contains("Secure Boot"));
        assert!(hint.contains("integrity"));
        assert!(hint.contains("mokutil --import"));

        // Verify DriverStatus JSON backwards compatibility when `secure_boot` is omitted
        let legacy_json = r#"{
            "sysfs_path": "/sys/kernel/ryzen_smu_drv",
            "loaded": false,
            "module_installed": true,
            "dkms_installed": true,
            "autoload_configured": false,
            "kernel_release": "6.12.0",
            "distro_family": "fedora",
            "distro_name": "Fedora Linux 41",
            "summary": "not loaded",
            "recommended_command": "sudo zensmu driver load",
            "manual_commands": []
        }"#;
        let mut parsed: DriverStatus = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(parsed.secure_boot, SecureBootStatus::default());
        assert!(!format_driver_remediation_hint(&parsed).contains("mokutil"));

        parsed.secure_boot = both;
        let rem = format_driver_remediation_hint(&parsed);
        assert!(rem.contains("Secure Boot"));
        assert!(rem.contains("mokutil --import"));
    }
}
