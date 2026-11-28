use super::distro::{
    detect_distro, manual_install_commands, read_kernel_release, recommended_driver_command,
};
use super::dkms::{find_managed_dkms_versions, kernel_headers_exist};
use super::lockdown::detect_secure_boot;
use super::{DriverStatus, BUNDLED_DKMS_SRC_DIR};
use crate::safety::persist::DEFAULT_MODULES_LOAD_PATH;
use std::fs;
use std::path::Path;
use std::process::Command;

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
    let bundled_dkms_available = Path::new(BUNDLED_DKMS_SRC_DIR).join("dkms.conf").is_file();
    let zensmu_managed_dkms = !find_managed_dkms_versions(Path::new("/usr/src")).is_empty();

    let autoload_configured = fs::read_to_string(DEFAULT_MODULES_LOAD_PATH)
        .map(|s| s.lines().any(|l| l.trim() == "ryzen_smu"))
        .unwrap_or(false);

    let kernel_release = read_kernel_release();
    let kernel_headers_installed = kernel_headers_exist(&kernel_release);
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
        if !kernel_headers_installed {
            format!(
                "ryzen_smu DKMS source is present but kernel headers for {kernel_release} are missing — run `sudo zensmu driver install`"
            )
        } else {
            format!(
                "ryzen_smu DKMS source is present but no compiled module was found for kernel {kernel_release}"
            )
        }
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
        bundled_dkms_available,
        zensmu_managed_dkms,
        kernel_headers_installed,
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
        "ryzen_smu is installed but not loaded — run: sudo zensmu driver load (or: sudo modprobe ryzen_smu)".to_string()
    } else {
        format!(
            "ryzen_smu is not installed ({}) — build & install via DKMS with: sudo zensmu driver install (inspect: zensmu driver status)",
            st.distro_name
        )
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
