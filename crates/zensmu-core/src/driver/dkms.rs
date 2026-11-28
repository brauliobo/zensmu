use super::cmd::{command_exists, ensure_command_available, make_temp_dir, require_root, run_cmd};
use super::status::inspect_driver_status;
use super::{
    DistroFamily, DriverActionOutput, DriverInstallOptions, DriverStatus, BUNDLED_DKMS_SRC_DIR,
    RYZEN_SMU_UPSTREAM_GIT, RYZEN_SMU_UPSTREAM_REPO, ZENSMU_MANAGED_MARKER,
    ZENSMU_MODULES_LOAD_MARKER,
};
use crate::error::{Error, Result};
use crate::safety::persist::{ensure_modules_load_conf, DEFAULT_MODULES_LOAD_PATH};
use std::fs;
use std::path::{Path, PathBuf};

/// Render the standard `dkms.conf` for `ryzen_smu` at a given DKMS version.
pub fn render_dkms_conf(version: &str) -> String {
    format!(
        "PACKAGE_NAME=\"ryzen_smu\"\n\
         PACKAGE_VERSION=\"{version}\"\n\
         MAKE[0]=\"make -C $kernel_source_dir M=$dkms_tree/$PACKAGE_NAME/$PACKAGE_VERSION/build modules\"\n\
         BUILT_MODULE_NAME[0]=\"ryzen_smu\"\n\
         DEST_MODULE_LOCATION[0]=\"/kernel/drivers/ryzen_smu\"\n\
         AUTOINSTALL=\"yes\"\n"
    )
}

/// Check whether kernel build headers exist for `kernel_release`.
pub fn kernel_headers_exist(kernel_release: &str) -> bool {
    let build_dir = Path::new("/lib/modules").join(kernel_release).join("build");
    build_dir.join("Makefile").is_file() || build_dir.is_dir()
}

/// Read Arch/CachyOS kernel `pkgbase` (e.g. `linux`, `linux-zen`, `linux-cachyos-bore`)
/// so the matching `<pkgbase>-headers` package can be installed accurately.
pub fn read_arch_kernel_pkgbase(kernel_release: &str) -> Option<String> {
    let path = Path::new("/usr/lib/modules")
        .join(kernel_release)
        .join("pkgbase");
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Build the distro package-manager command needed to install DKMS build prerequisites
/// (`dkms`, `make`, `gcc`, kernel headers, and optionally `git` when cloning upstream).
pub fn dkms_prereq_command(
    family: DistroFamily,
    kernel_release: &str,
    arch_pkgbase: Option<&str>,
    need_git: bool,
) -> Option<(&'static str, Vec<String>)> {
    match family {
        DistroFamily::Debian => {
            let mut args = vec![
                "install".to_string(),
                "-y".to_string(),
                "dkms".to_string(),
                "build-essential".to_string(),
                format!("linux-headers-{kernel_release}"),
            ];
            if need_git {
                args.push("git".to_string());
            }
            Some(("apt-get", args))
        }
        DistroFamily::Fedora => {
            let mut args = vec![
                "install".to_string(),
                "-y".to_string(),
                "dkms".to_string(),
                "gcc".to_string(),
                "make".to_string(),
                format!("kernel-devel-{kernel_release}"),
            ];
            if need_git {
                args.push("git".to_string());
            }
            Some(("dnf", args))
        }
        DistroFamily::OpenSuse => {
            let mut args = vec![
                "--non-interactive".to_string(),
                "install".to_string(),
                "dkms".to_string(),
                "gcc".to_string(),
                "make".to_string(),
                "kernel-default-devel".to_string(),
            ];
            if need_git {
                args.push("git".to_string());
            }
            Some(("zypper", args))
        }
        DistroFamily::Arch => {
            let headers = format!("{}-headers", arch_pkgbase.unwrap_or("linux"));
            let mut args = vec![
                "-S".to_string(),
                "--needed".to_string(),
                "--noconfirm".to_string(),
                "dkms".to_string(),
                "base-devel".to_string(),
                headers,
            ];
            if need_git {
                args.push("git".to_string());
            }
            Some(("pacman", args))
        }
        DistroFamily::Other => None,
    }
}

/// Find `zensmu`-managed `/usr/src/ryzen_smu-<ver>` directories containing `.zensmu-managed`.
pub fn find_managed_dkms_versions(usr_src: &Path) -> Vec<(String, PathBuf)> {
    find_ryzen_smu_dkms_versions(usr_src, true)
}

/// Find all `/usr/src/ryzen_smu-<ver>` directories regardless of manager.
pub fn find_all_ryzen_smu_dkms_versions(usr_src: &Path) -> Vec<(String, PathBuf)> {
    find_ryzen_smu_dkms_versions(usr_src, false)
}

fn find_ryzen_smu_dkms_versions(usr_src: &Path, managed_only: bool) -> Vec<(String, PathBuf)> {
    let Ok(entries) = fs::read_dir(usr_src) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(ver) = name.strip_prefix("ryzen_smu-") else {
            continue;
        };
        if ver.is_empty() {
            continue;
        }
        if managed_only && !path.join(ZENSMU_MANAGED_MARKER).is_file() {
            continue;
        }
        found.push((ver.to_string(), path));
    }
    found.sort_by(|a, b| a.0.cmp(&b.0));
    found
}

pub(super) fn is_zensmu_managed_modules_load(path: &Path) -> bool {
    fs::read_to_string(path)
        .map(|s| {
            s.contains(ZENSMU_MODULES_LOAD_MARKER)
                || s.contains("# Autoload AMD Ryzen SMU driver for zensmu")
        })
        .unwrap_or(false)
}

fn ensure_dkms_prerequisites(
    before: &DriverStatus,
    need_git: bool,
    notes: &mut Vec<String>,
) -> Result<()> {
    let missing_tools = !command_exists("dkms")
        || !command_exists("make")
        || !command_exists("gcc")
        || (need_git && !command_exists("git"));
    let missing_headers = !before.kernel_headers_installed;

    if !missing_tools && !missing_headers {
        return Ok(());
    }

    let arch_pkgbase = if before.distro_family == DistroFamily::Arch {
        read_arch_kernel_pkgbase(&before.kernel_release)
    } else {
        None
    };

    let Some((bin, args)) = dkms_prereq_command(
        before.distro_family,
        &before.kernel_release,
        arch_pkgbase.as_deref(),
        need_git,
    ) else {
        return Ok(());
    };

    if !command_exists(bin) {
        return Ok(());
    }

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let envs: &[(&str, &str)] = if bin == "apt-get" {
        &[("DEBIAN_FRONTEND", "noninteractive")]
    } else {
        &[]
    };

    let out = run_cmd(bin, &arg_refs, envs)?;
    if out.status.success() {
        notes.push(format!(
            "Installed DKMS build prerequisites via `{bin} {}`",
            args.join(" ")
        ));
        return Ok(());
    }

    // Fallback when an exact kernel-version headers package isn't in the repo (e.g. rolling / generic meta)
    match before.distro_family {
        DistroFamily::Debian => {
            let mut fallback = vec![
                "install",
                "-y",
                "dkms",
                "build-essential",
                "linux-headers-generic",
            ];
            if need_git {
                fallback.push("git");
            }
            let retry = run_cmd("apt-get", &fallback, envs)?;
            if !retry.status.success() {
                let mut fallback_amd64 = vec![
                    "install",
                    "-y",
                    "dkms",
                    "build-essential",
                    "linux-headers-amd64",
                ];
                if need_git {
                    fallback_amd64.push("git");
                }
                let _ = run_cmd("apt-get", &fallback_amd64, envs);
            }
        }
        DistroFamily::Fedora => {
            let mut fallback = vec!["install", "-y", "dkms", "gcc", "make", "kernel-devel"];
            if need_git {
                fallback.push("git");
            }
            let _ = run_cmd("dnf", &fallback, &[]);
        }
        _ => {}
    }

    Ok(())
}

fn stage_dkms_tree(src_dir: &Path, version: &str) -> Result<PathBuf> {
    let dst_dir = Path::new("/usr/src").join(format!("ryzen_smu-{version}"));
    fs::create_dir_all(&dst_dir).map_err(|source| Error::Io {
        path: dst_dir.display().to_string(),
        source,
    })?;

    for file in ["drv.c", "smu.c", "smu.h", "Makefile", "LICENSE"] {
        let src_file = src_dir.join(file);
        if src_file.is_file() {
            let dst_file = dst_dir.join(file);
            fs::copy(&src_file, &dst_file).map_err(|source| Error::Io {
                path: dst_file.display().to_string(),
                source,
            })?;
        }
    }

    let dkms_conf_path = dst_dir.join("dkms.conf");
    fs::write(&dkms_conf_path, render_dkms_conf(version)).map_err(|source| Error::Io {
        path: dkms_conf_path.display().to_string(),
        source,
    })?;

    let marker_path = dst_dir.join(ZENSMU_MANAGED_MARKER);
    fs::write(&marker_path, format!("{version}\n")).map_err(|source| Error::Io {
        path: marker_path.display().to_string(),
        source,
    })?;

    Ok(dst_dir)
}

pub(super) fn install_via_dkms(
    sysfs_path: &Path,
    before: &DriverStatus,
    opts: &DriverInstallOptions,
    force_upstream: bool,
) -> Result<DriverActionOutput> {
    let bundled_dir = Path::new(BUNDLED_DKMS_SRC_DIR);
    let use_bundled = !force_upstream && bundled_dir.join("dkms.conf").is_file();
    let method_label = if use_bundled { "dkms" } else { "source" };

    let mut steps = Vec::new();
    let need_git = !use_bundled;
    let missing_prereqs = !command_exists("dkms")
        || !command_exists("make")
        || !command_exists("gcc")
        || (need_git && !command_exists("git"))
        || !before.kernel_headers_installed;

    if missing_prereqs {
        let arch_pkgbase = if before.distro_family == DistroFamily::Arch {
            read_arch_kernel_pkgbase(&before.kernel_release)
        } else {
            None
        };
        if let Some((bin, args)) = dkms_prereq_command(
            before.distro_family,
            &before.kernel_release,
            arch_pkgbase.as_deref(),
            need_git,
        ) {
            steps.push(format!("{bin} {}", args.join(" ")));
        }
    }

    if use_bundled {
        steps.push(format!(
            "Stage bundled DKMS sources from {BUNDLED_DKMS_SRC_DIR} into /usr/src/ryzen_smu-<ver>"
        ));
    } else {
        steps.push(format!(
            "git clone --depth 1 {RYZEN_SMU_UPSTREAM_GIT} <tmpdir>"
        ));
        steps.push("Stage DKMS sources + dkms.conf into /usr/src/ryzen_smu-<ver>".to_string());
    }
    steps.push("dkms add / build / install -m ryzen_smu -v <ver> --force".to_string());
    if opts.configure_autoload {
        steps.push(format!("Ensure 'ryzen_smu' in {DEFAULT_MODULES_LOAD_PATH}"));
    }
    steps.push("modprobe ryzen_smu".to_string());

    if opts.dry_run {
        let mut dry_notes = vec![if use_bundled {
            format!(
                "Dry-run only: will build via DKMS from bundled sources in {BUNDLED_DKMS_SRC_DIR}."
            )
        } else {
            format!("Dry-run only: will clone {RYZEN_SMU_UPSTREAM_REPO} and build via DKMS.")
        }];
        if !before.loaded && before.secure_boot.blocks_unsigned_modules {
            if let Some(ref hint) = before.secure_boot.hint {
                dry_notes.push(hint.clone());
            }
        }
        return Ok(DriverActionOutput {
            action: "install".to_string(),
            dry_run: true,
            method: method_label.to_string(),
            changed: true,
            loaded: before.loaded,
            autoload_configured: before.autoload_configured,
            steps,
            notes: dry_notes,
            status: before.clone(),
        });
    }

    require_root("building and installing ryzen_smu via DKMS")?;

    let mut notes = Vec::new();
    ensure_dkms_prerequisites(before, need_git, &mut notes)?;

    if need_git {
        ensure_command_available("git", "install `git` first")?;
    }
    ensure_command_available("make", "install `make` / build tools first")?;
    ensure_command_available("gcc", "install `gcc` / C compiler first")?;
    ensure_command_available("dkms", "install `dkms` and kernel headers first")?;

    let mut temp_dir: Option<PathBuf> = None;

    let result = (|| -> Result<String> {
        let (src_dir, smu_ver) = if use_bundled {
            let ver = fs::read_to_string(bundled_dir.join("VERSION"))
                .ok()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "0.1.7".to_string());
            (bundled_dir.to_path_buf(), ver)
        } else {
            let workdir = make_temp_dir("zensmu-driver-src")?;
            let clone_dir = workdir.join("ryzen_smu");
            let clone_str = clone_dir.display().to_string();
            temp_dir = Some(workdir);

            let clone_out = run_cmd("git", &["clone", RYZEN_SMU_UPSTREAM_GIT, &clone_str], &[])?;
            if !clone_out.status.success() {
                return Err(Error::InvalidInput(format!(
                    "git clone failed: {}",
                    String::from_utf8_lossy(&clone_out.stderr).trim()
                )));
            }
            let rev = run_cmd(
                "git",
                &["-C", &clone_str, "rev-list", "--count", "HEAD"],
                &[],
            )
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());
            let sha = run_cmd(
                "git",
                &["-C", &clone_str, "rev-parse", "--short=7", "HEAD"],
                &[],
            )
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());

            let ver = match (rev, sha) {
                (Some(r), Some(s)) => format!("0.1.7.r{r}.g{s}"),
                _ => "0.1.7".to_string(),
            };
            (clone_dir, ver)
        };

        // Remove any older zensmu-managed DKMS version (or current version on --force)
        let usr_src = Path::new("/usr/src");
        for (old_ver, old_dir) in find_managed_dkms_versions(usr_src) {
            if opts.force || old_ver != smu_ver {
                let _ = run_cmd(
                    "dkms",
                    &["remove", "-m", "ryzen_smu", "-v", &old_ver, "--all"],
                    &[],
                );
                let _ = fs::remove_dir_all(&old_dir);
            }
        }

        stage_dkms_tree(&src_dir, &smu_ver)?;

        let status_out = run_cmd("dkms", &["status", "-m", "ryzen_smu", "-v", &smu_ver], &[])?;
        let already_added = status_out.status.success()
            && String::from_utf8_lossy(&status_out.stdout).contains("ryzen_smu");
        if !already_added {
            let add_out = run_cmd("dkms", &["add", "-m", "ryzen_smu", "-v", &smu_ver], &[])?;
            if !add_out.status.success() {
                return Err(Error::InvalidInput(format!(
                    "dkms add failed: {}",
                    String::from_utf8_lossy(&add_out.stderr).trim()
                )));
            }
        }

        let build_out = run_cmd("dkms", &["build", "-m", "ryzen_smu", "-v", &smu_ver], &[])?;
        if !build_out.status.success() {
            return Err(Error::InvalidInput(format!(
                "dkms build failed for kernel {} (ensure matching kernel headers are installed): {}",
                before.kernel_release,
                String::from_utf8_lossy(&build_out.stderr).trim()
            )));
        }

        let inst_out = run_cmd(
            "dkms",
            &["install", "-m", "ryzen_smu", "-v", &smu_ver, "--force"],
            &[],
        )?;
        if !inst_out.status.success() {
            return Err(Error::InvalidInput(format!(
                "dkms install failed: {}",
                String::from_utf8_lossy(&inst_out.stderr).trim()
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
                    "{} (DKMS built & installed ryzen_smu/{smu_ver}, but `modprobe ryzen_smu` failed: {}; {extra})",
                    sysfs_path.display(),
                    String::from_utf8_lossy(&mod_out.stderr).trim()
                ),
            });
        }
        Ok(smu_ver)
    })();

    if let Some(dir) = temp_dir {
        let _ = fs::remove_dir_all(&dir);
    }
    let smu_ver = result?;

    let after = inspect_driver_status(sysfs_path);
    notes.push(if use_bundled {
        format!(
            "Built & installed ryzen_smu/{smu_ver} via DKMS from bundled {BUNDLED_DKMS_SRC_DIR}"
        )
    } else {
        format!("Built & installed ryzen_smu/{smu_ver} via DKMS from {RYZEN_SMU_UPSTREAM_REPO}")
    });
    if !after.loaded && after.secure_boot.blocks_unsigned_modules {
        if let Some(ref hint) = after.secure_boot.hint {
            notes.push(hint.clone());
        }
    }
    Ok(DriverActionOutput {
        action: "install".to_string(),
        dry_run: false,
        method: method_label.to_string(),
        changed: true,
        loaded: after.loaded,
        autoload_configured: after.autoload_configured,
        steps,
        notes,
        status: after,
    })
}
