use super::paths::{default_daemon_recording_path, env_path};
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// User-level systemd unit name for background telemetry recording.
pub const USER_RECORD_SERVICE_NAME: &str = "zensmu-record.service";

/// Read-only status of the unprivileged user-level `zensmu-record.service` unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordServiceStatus {
    pub service_name: String,
    pub installed: bool,
    pub enabled: bool,
    pub active: bool,
    pub state: String,
    pub default_output_path: String,
}

fn user_unit_candidate_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/usr/lib/systemd/user").join(USER_RECORD_SERVICE_NAME),
        PathBuf::from("/etc/systemd/user").join(USER_RECORD_SERVICE_NAME),
    ];
    if let Some(xdg_config) = env_path("XDG_CONFIG_HOME") {
        paths.push(
            xdg_config
                .join("systemd")
                .join("user")
                .join(USER_RECORD_SERVICE_NAME),
        );
    }
    if let Some(home) = env_path("HOME") {
        paths.push(
            home.join(".config")
                .join("systemd")
                .join("user")
                .join(USER_RECORD_SERVICE_NAME),
        );
    }
    paths
}

fn query_user_systemctl(subcommand: &str) -> (bool, String) {
    match Command::new("systemctl")
        .args(["--user", subcommand, USER_RECORD_SERVICE_NAME])
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
                "ok".to_string()
            } else {
                "unknown".to_string()
            };
            (out.status.success(), state)
        }
        Err(_) => (false, "systemctl-unavailable".to_string()),
    }
}

fn is_installed_unit_file_state(state: &str) -> bool {
    matches!(
        state,
        "enabled"
            | "enabled-runtime"
            | "linked"
            | "linked-runtime"
            | "alias"
            | "masked"
            | "masked-runtime"
            | "static"
            | "indirect"
            | "disabled"
            | "generated"
            | "transient"
    )
}

/// Inspect the unprivileged user-level `zensmu-record.service` unit status.
pub fn read_record_service_status() -> RecordServiceStatus {
    let unit_file_exists = user_unit_candidate_paths().iter().any(|p| p.exists());
    let (enabled_ok, enabled_state) = query_user_systemctl("is-enabled");
    let (active_ok, active_state) = query_user_systemctl("is-active");

    let installed = unit_file_exists || is_installed_unit_file_state(&enabled_state);
    let enabled =
        enabled_ok && matches!(enabled_state.as_str(), "enabled" | "enabled-runtime" | "ok");
    let active = active_ok && matches!(active_state.as_str(), "active" | "ok");

    let state = if active {
        "active".to_string()
    } else if matches!(
        active_state.as_str(),
        "activating" | "deactivating" | "reloading" | "failed"
    ) {
        active_state
    } else if enabled {
        "enabled".to_string()
    } else if installed {
        if is_installed_unit_file_state(&enabled_state) {
            enabled_state
        } else {
            "disabled".to_string()
        }
    } else if enabled_state == "systemctl-unavailable" {
        "systemctl-unavailable".to_string()
    } else {
        "not-installed".to_string()
    };

    RecordServiceStatus {
        service_name: USER_RECORD_SERVICE_NAME.to_string(),
        installed,
        enabled,
        active,
        state,
        default_output_path: default_daemon_recording_path().display().to_string(),
    }
}

/// Enable (`enable --now`) or disable (`disable --now`) the unprivileged user-level
/// `zensmu-record.service` unit and return its refreshed status.
pub fn set_record_service_enabled(enable: bool) -> Result<RecordServiceStatus> {
    let current = read_record_service_status();
    if !current.installed {
        return Err(Error::InvalidInput(format!(
            "{USER_RECORD_SERVICE_NAME} is not installed (expected in /usr/lib/systemd/user/ or ~/.config/systemd/user/)"
        )));
    }

    let verb = if enable { "enable" } else { "disable" };
    let out = Command::new("systemctl")
        .args(["--user", verb, "--now", USER_RECORD_SERVICE_NAME])
        .output()
        .map_err(|e| {
            Error::InvalidInput(format!(
                "failed to execute systemctl --user {verb} --now {USER_RECORD_SERVICE_NAME}: {e}"
            ))
        })?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
        let msg = if !stderr.is_empty() { stderr } else { stdout };
        return Err(Error::InvalidInput(format!(
            "systemctl --user {verb} --now {USER_RECORD_SERVICE_NAME} failed: {msg}"
        )));
    }

    Ok(read_record_service_status())
}
