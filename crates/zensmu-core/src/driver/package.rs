use super::cmd::{ensure_command_available, make_temp_dir, require_root, run_cmd};
use super::status::inspect_driver_status;
use super::{
    DistroFamily, DriverActionOutput, DriverInstallOptions, DriverStatus, RYZEN_SMU_CHECKSUMS_ASSET,
};
use crate::error::{Error, Result};
use crate::report::GITHUB_REPO;
use crate::safety::persist::{ensure_modules_load_conf, DEFAULT_MODULES_LOAD_PATH};
use std::fs;
use std::path::Path;

pub(super) fn install_via_release_package(
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
