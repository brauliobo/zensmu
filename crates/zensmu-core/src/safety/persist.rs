//! Boot & suspend persistence helpers (`/etc/zensmu/active.toml`,
//! `/etc/modules-load.d/ryzen_smu.conf`, and `zensmu-apply.service`).

use crate::config::profile::Profile;
use crate::error::{Error, Result};
use crate::safety::guard::{BootLoopGuard, DEFAULT_GUARD_PATH};
use crate::types::Codename;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_ACTIVE_PROFILE_PATH: &str = "/etc/zensmu/active.toml";
pub const DEFAULT_MODULES_LOAD_PATH: &str = "/etc/modules-load.d/ryzen_smu.conf";
pub const DEFAULT_SERVICE_NAME: &str = "zensmu-apply.service";
pub const SYSTEM_SERVICE_PATHS: [&str; 2] = [
    "/usr/lib/systemd/system/zensmu-apply.service",
    "/etc/systemd/system/zensmu-apply.service",
];
pub const DEFAULT_RESUME_HOOK_PATH: &str = "/usr/lib/systemd/system-sleep/zensmu-resume";

/// Read-only status of `zensmu` boot/resume persistence on this machine (works unprivileged).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersistStatus {
    pub active_profile_path: String,
    pub active_profile_exists: bool,
    pub active_profile_valid: Option<bool>,
    pub active_profile_error: Option<String>,
    pub active_profile_codename: Option<Codename>,
    pub modules_load_configured: bool,
    pub service_name: String,
    pub service_installed: bool,
    pub service_enabled: bool,
    pub service_state: String,
    pub resume_hook_installed: bool,
    pub boot_guard_tripped: bool,
}

/// Configurable paths for persistence operations (supports testing in temp dirs).
#[derive(Debug, Clone)]
pub struct PersistPaths {
    pub active_profile: PathBuf,
    pub modules_load: PathBuf,
    pub guard_path: PathBuf,
    pub resume_hook: PathBuf,
    pub service_name: String,
}

impl Default for PersistPaths {
    fn default() -> Self {
        Self {
            active_profile: PathBuf::from(DEFAULT_ACTIVE_PROFILE_PATH),
            modules_load: PathBuf::from(DEFAULT_MODULES_LOAD_PATH),
            guard_path: PathBuf::from(DEFAULT_GUARD_PATH),
            resume_hook: PathBuf::from(DEFAULT_RESUME_HOOK_PATH),
            service_name: DEFAULT_SERVICE_NAME.to_string(),
        }
    }
}

/// Inspect boot persistence & systemd service status (read-only, unprivileged).
pub fn read_persist_status(
    paths: &PersistPaths,
    host_codename: Option<Codename>,
    host_physical_cores: Option<usize>,
) -> PersistStatus {
    let active_profile_exists = paths.active_profile.exists();
    let mut active_profile_valid = None;
    let mut active_profile_error = None;
    let mut active_profile_codename = None;

    if active_profile_exists {
        match Profile::load(&paths.active_profile) {
            Ok(prof) => {
                active_profile_codename = Some(prof.target.codename);
                if let (Some(cn), Some(cores)) = (host_codename, host_physical_cores) {
                    match prof.validate_for(cn, cores, true) {
                        Ok(()) => active_profile_valid = Some(true),
                        Err(e) => {
                            active_profile_valid = Some(false);
                            active_profile_error = Some(e.to_string());
                        }
                    }
                } else {
                    active_profile_valid = Some(true);
                }
            }
            Err(e) => {
                active_profile_valid = Some(false);
                active_profile_error = Some(e.to_string());
            }
        }
    }

    let modules_load_configured = fs::read_to_string(&paths.modules_load)
        .ok()
        .map(|s| s.lines().any(|l| l.trim() == "ryzen_smu"))
        .unwrap_or(false);

    let service_installed = SYSTEM_SERVICE_PATHS.iter().any(|p| Path::new(p).exists());
    let (service_enabled, service_state) = query_systemd_enabled(&paths.service_name);
    let resume_hook_installed = paths.resume_hook.exists();
    let boot_guard_tripped = BootLoopGuard::new(paths.guard_path.clone()).is_tripped();

    PersistStatus {
        active_profile_path: paths.active_profile.display().to_string(),
        active_profile_exists,
        active_profile_valid,
        active_profile_error,
        active_profile_codename,
        modules_load_configured,
        service_name: paths.service_name.clone(),
        service_installed: service_installed || service_state != "not-found",
        service_enabled,
        service_state,
        resume_hook_installed,
        boot_guard_tripped,
    }
}

fn query_systemd_enabled(service: &str) -> (bool, String) {
    match Command::new("systemctl")
        .args(["is-enabled", service])
        .output()
    {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            let state = if !stdout.is_empty() {
                stdout
            } else if !stderr.is_empty() {
                stderr
            } else if out.status.success() {
                "enabled".to_string()
            } else {
                "disabled".to_string()
            };
            let enabled = out.status.success() && state == "enabled";
            (enabled, state)
        }
        Err(_) => (false, "systemctl-unavailable".to_string()),
    }
}

/// Ensure `/etc/modules-load.d/ryzen_smu.conf` contains `ryzen_smu` (idempotent).
pub fn ensure_modules_load_conf(path: &Path) -> Result<bool> {
    if let Ok(existing) = fs::read_to_string(path) {
        if existing.lines().any(|l| l.trim() == "ryzen_smu") {
            return Ok(false);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| map_io_err(parent, source))?;
    }
    fs::write(path, "# Autoload AMD Ryzen SMU driver for zensmu\nryzen_smu\n")
        .map_err(|source| map_io_err(path, source))?;
    Ok(true)
}

/// Validate and save a `Profile` to the active boot persistence path (`/etc/zensmu/active.toml`),
/// also configuring `ryzen_smu` module autoload.
pub fn persist_profile(
    paths: &PersistPaths,
    profile: &Profile,
    host_codename: Codename,
    host_physical_cores: usize,
    allow_extreme: bool,
) -> Result<()> {
    profile.validate_for(host_codename, host_physical_cores, allow_extreme)?;
    profile.save(&paths.active_profile)?;
    ensure_modules_load_conf(&paths.modules_load)?;
    Ok(())
}

/// Enable or disable `zensmu-apply.service` via `systemctl`.
///
/// Enabling refuses if `/etc/zensmu/active.toml` is missing or invalid for this CPU.
pub fn set_service_enabled(
    paths: &PersistPaths,
    enable: bool,
    host_codename: Codename,
    host_physical_cores: usize,
    allow_extreme: bool,
) -> Result<String> {
    if enable {
        if !paths.active_profile.exists() {
            return Err(Error::InvalidInput(format!(
                "cannot enable {} because {} does not exist yet (save a profile first)",
                paths.service_name,
                paths.active_profile.display()
            )));
        }
        let prof = Profile::load(&paths.active_profile)?;
        prof.validate_for(host_codename, host_physical_cores, allow_extreme)?;
        ensure_modules_load_conf(&paths.modules_load)?;
    }

    let verb = if enable { "enable" } else { "disable" };
    let out = Command::new("systemctl")
        .args([verb, &paths.service_name])
        .output()
        .map_err(|source| Error::Io {
            path: "systemctl".into(),
            source,
        })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        if msg.to_ascii_lowercase().contains("permission denied")
            || msg.to_ascii_lowercase().contains("access denied")
            || msg.to_ascii_lowercase().contains("authentication")
        {
            return Err(Error::PermissionDenied {
                path: format!("systemctl {verb} {}", paths.service_name),
            });
        }
        return Err(Error::InvalidInput(format!(
            "systemctl {verb} {} failed: {msg}",
            paths.service_name
        )));
    }

    Ok(if enable {
        "enabled".to_string()
    } else {
        "disabled".to_string()
    })
}

/// Remove the active boot profile (`/etc/zensmu/active.toml`) if present.
pub fn remove_active_profile(paths: &PersistPaths) -> Result<bool> {
    if !paths.active_profile.exists() {
        return Ok(false);
    }
    fs::remove_file(&paths.active_profile)
        .map_err(|source| map_io_err(&paths.active_profile, source))?;
    Ok(true)
}

fn map_io_err(path: &Path, source: std::io::Error) -> Error {
    if source.kind() == std::io::ErrorKind::PermissionDenied {
        Error::PermissionDenied {
            path: path.display().to_string(),
        }
    } else {
        Error::Io {
            path: path.display().to_string(),
            source,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::profile::{CoProfile, ProfileTarget, PROFILE_SCHEMA_VERSION};
    use std::collections::BTreeMap;

    #[test]
    fn persist_profile_writes_active_toml_and_modules_load() {
        let dir = tempfile::tempdir().unwrap();
        let paths = PersistPaths {
            active_profile: dir.path().join("etc/zensmu/active.toml"),
            modules_load: dir.path().join("etc/modules-load.d/ryzen_smu.conf"),
            guard_path: dir.path().join("var/lib/zensmu/applying.lock"),
            resume_hook: dir.path().join("usr/lib/systemd/system-sleep/zensmu-resume"),
            service_name: DEFAULT_SERVICE_NAME.to_string(),
        };

        let profile = Profile {
            schema: PROFILE_SCHEMA_VERSION,
            target: ProfileTarget {
                codename: Codename::GraniteRidge,
                physical_cores: Some(8),
            },
            curve_optimizer: Some(CoProfile {
                all: Some(-15),
                per_core: BTreeMap::new(),
                igpu: None,
            }),
            curve_shaper: None,
            pbo: None,
            apu: None,
        };

        persist_profile(&paths, &profile, Codename::GraniteRidge, 8, false).unwrap();
        assert!(paths.active_profile.exists());
        assert!(paths.modules_load.exists());

        let status = read_persist_status(&paths, Some(Codename::GraniteRidge), Some(8));
        assert!(status.active_profile_exists);
        assert_eq!(status.active_profile_valid, Some(true));
        assert_eq!(status.active_profile_codename, Some(Codename::GraniteRidge));
        assert!(status.modules_load_configured);
        assert!(!status.boot_guard_tripped);

        // Removing active profile cleans it up
        assert!(remove_active_profile(&paths).unwrap());
        assert!(!paths.active_profile.exists());
    }

    #[test]
    fn enable_refuses_when_active_profile_missing() {
        let dir = tempfile::tempdir().unwrap();
        let paths = PersistPaths {
            active_profile: dir.path().join("missing.toml"),
            modules_load: dir.path().join("ryzen_smu.conf"),
            guard_path: dir.path().join("applying.lock"),
            resume_hook: dir.path().join("resume"),
            service_name: DEFAULT_SERVICE_NAME.to_string(),
        };

        let err = set_service_enabled(&paths, true, Codename::GraniteRidge, 8, false).unwrap_err();
        assert!(matches!(err, Error::InvalidInput(_)));
    }
}
