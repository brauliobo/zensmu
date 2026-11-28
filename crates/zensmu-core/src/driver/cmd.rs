use crate::error::{Error, Result};
use std::fs;
use std::path::PathBuf;
use std::process::Command;

pub(super) fn require_root(action: &str) -> Result<()> {
    if unsafe { libc::geteuid() != 0 } {
        return Err(Error::PermissionDenied {
            path: format!("{action} requires root (run with sudo or pkexec)"),
        });
    }
    Ok(())
}

pub(super) fn command_exists(cmd: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {cmd} >/dev/null 2>&1")])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

pub(super) fn ensure_command_available(cmd: &str, hint: &str) -> Result<()> {
    if !command_exists(cmd) {
        return Err(Error::InvalidInput(format!(
            "required command `{cmd}` not found ({hint})"
        )));
    }
    Ok(())
}

pub(super) fn run_cmd(
    bin: &str,
    args: &[&str],
    envs: &[(&str, &str)],
) -> Result<std::process::Output> {
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

pub(super) fn make_temp_dir(prefix: &str) -> Result<PathBuf> {
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
