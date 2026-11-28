use super::cmd::{command_exists, require_root, run_cmd};
use super::dkms::{
    find_all_ryzen_smu_dkms_versions, find_managed_dkms_versions, install_via_dkms,
    is_zensmu_managed_modules_load,
};
use super::package::install_via_release_package;
use super::status::inspect_driver_status;
use super::{DriverActionOutput, DriverInstallMethod, DriverInstallOptions};
use crate::error::{Error, Result};
use crate::safety::persist::{ensure_modules_load_conf, DEFAULT_MODULES_LOAD_PATH};
use std::fs;
use std::path::Path;

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

/// Install and load `ryzen_smu` via first-class DKMS build (default) or release package.
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
                "ryzen_smu is already installed for kernel {} (pass --force to rebuild/reinstall)",
                before.kernel_release
            ),
        );
        return Ok(out);
    }

    let effective_method = match opts.method {
        DriverInstallMethod::Auto => DriverInstallMethod::Dkms,
        explicit => explicit,
    };

    if effective_method == DriverInstallMethod::Package
        && before.distro_family.package_extension().is_none()
    {
        return Err(Error::InvalidInput(format!(
            "No standalone .deb/.rpm package method for distro '{}'; run `sudo zensmu driver install` (DKMS build)",
            before.distro_name
        )));
    }

    match effective_method {
        DriverInstallMethod::Dkms => install_via_dkms(sysfs_path, &before, opts, false),
        DriverInstallMethod::Source => install_via_dkms(sysfs_path, &before, opts, true),
        DriverInstallMethod::Package => install_via_release_package(sysfs_path, &before, opts),
        DriverInstallMethod::Auto => Err(Error::InvalidInput(
            "unable to resolve driver install method".to_string(),
        )),
    }
}

/// Unload and uninstall `ryzen_smu` DKMS registrations managed by `zensmu`
/// (or all `ryzen_smu` DKMS registrations when `force` is true).
pub fn uninstall_driver(
    sysfs_path: &Path,
    force: bool,
    dry_run: bool,
) -> Result<DriverActionOutput> {
    let before = inspect_driver_status(sysfs_path);
    let usr_src = Path::new("/usr/src");
    let targets = if force {
        find_all_ryzen_smu_dkms_versions(usr_src)
    } else {
        find_managed_dkms_versions(usr_src)
    };

    let mut steps = Vec::new();
    let mut notes = Vec::new();

    if targets.is_empty() && !force {
        if before.dkms_installed {
            notes.push(
                "ryzen_smu DKMS appears to be managed externally (not by zensmu); pass --force to remove it anyway, or remove the external package (e.g. ryzen_smu-dkms-git / ryzen-smu-dkms)."
                    .to_string(),
            );
        } else {
            notes.push("No zensmu-managed ryzen_smu DKMS installation found.".to_string());
        }
        return Ok(DriverActionOutput {
            action: "uninstall".to_string(),
            dry_run,
            method: "dkms".to_string(),
            changed: false,
            loaded: before.loaded,
            autoload_configured: before.autoload_configured,
            steps,
            notes,
            status: before,
        });
    }

    if before.loaded {
        steps.push("modprobe -r ryzen_smu".to_string());
    }
    for (ver, dir) in &targets {
        steps.push(format!("dkms remove -m ryzen_smu -v {ver} --all"));
        steps.push(format!("Remove {}", dir.display()));
    }
    let remove_autoload = is_zensmu_managed_modules_load(Path::new(DEFAULT_MODULES_LOAD_PATH));
    if remove_autoload {
        steps.push(format!("Remove {DEFAULT_MODULES_LOAD_PATH}"));
    }

    if dry_run {
        notes.push("Dry-run only: no module unload or DKMS removal executed.".to_string());
        return Ok(DriverActionOutput {
            action: "uninstall".to_string(),
            dry_run: true,
            method: "dkms".to_string(),
            changed: !steps.is_empty(),
            loaded: before.loaded,
            autoload_configured: before.autoload_configured,
            steps,
            notes,
            status: before,
        });
    }

    require_root("uninstalling ryzen_smu DKMS kernel driver")?;

    let mut changed = false;
    if before.loaded {
        let out = run_cmd("modprobe", &["-r", "ryzen_smu"], &[])?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            notes.push(format!(
                "Warning: `modprobe -r ryzen_smu` returned non-zero ({stderr}); continuing DKMS removal"
            ));
        } else {
            changed = true;
        }
    }

    for (ver, dir) in &targets {
        if command_exists("dkms") {
            let _ = run_cmd(
                "dkms",
                &["remove", "-m", "ryzen_smu", "-v", ver, "--all"],
                &[],
            );
        }
        if dir.exists() {
            fs::remove_dir_all(dir).map_err(|source| Error::Io {
                path: dir.display().to_string(),
                source,
            })?;
        }
        changed = true;
    }

    if remove_autoload {
        let p = Path::new(DEFAULT_MODULES_LOAD_PATH);
        if p.exists() {
            fs::remove_file(p).map_err(|source| Error::Io {
                path: p.display().to_string(),
                source,
            })?;
            changed = true;
        }
    }

    let after = inspect_driver_status(sysfs_path);
    Ok(DriverActionOutput {
        action: "uninstall".to_string(),
        dry_run: false,
        method: "dkms".to_string(),
        changed,
        loaded: after.loaded,
        autoload_configured: after.autoload_configured,
        steps,
        notes,
        status: after,
    })
}
