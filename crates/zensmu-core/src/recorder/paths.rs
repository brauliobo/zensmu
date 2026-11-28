use super::time::{format_compact_utc_timestamp, now_unix_ms};
use super::RecordFormat;
use crate::types::Codename;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Return a non-empty environment variable value as a [`PathBuf`].
pub(super) fn env_path(var: &str) -> Option<PathBuf> {
    std::env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Default directory for user telemetry recordings (zero root required).
///
/// Respects `$XDG_DATA_HOME/zensmu/recordings`, else `$HOME/.local/share/zensmu/recordings`,
/// else `/tmp/zensmu-recordings`.
pub fn default_recordings_dir() -> PathBuf {
    if let Some(xdg_data) = env_path("XDG_DATA_HOME") {
        return xdg_data.join("zensmu").join("recordings");
    }
    if let Some(home) = env_path("HOME") {
        return home
            .join(".local")
            .join("share")
            .join("zensmu")
            .join("recordings");
    }
    PathBuf::from("/tmp/zensmu-recordings")
}

/// Default timestamped recording path:
/// `<default_recordings_dir>/zensmu-<codename.slug()>-<YYYYMMDD-HHMMSS>.<ext>`.
pub fn default_recording_path(codename: Codename, format: RecordFormat) -> PathBuf {
    let now_secs = now_unix_ms() / 1000;
    let stamp = format_compact_utc_timestamp(now_secs);
    let file_name = format!("zensmu-{}-{stamp}.{}", codename.slug(), format.extension());
    default_recordings_dir().join(file_name)
}

/// Default output path for the unprivileged background recording daemon.
///
/// Respects `$XDG_DATA_HOME/zensmu/telemetry.csv`, else `$HOME/.local/share/zensmu/telemetry.csv`,
/// else `/tmp/zensmu-telemetry.csv`.
pub fn default_daemon_recording_path() -> PathBuf {
    if let Some(xdg_data) = env_path("XDG_DATA_HOME") {
        return xdg_data.join("zensmu").join("telemetry.csv");
    }
    if let Some(home) = env_path("HOME") {
        return home
            .join(".local")
            .join("share")
            .join("zensmu")
            .join("telemetry.csv");
    }
    PathBuf::from("/tmp/zensmu-telemetry.csv")
}

/// Sanitize a Phoronix Test Suite result name and return
/// `$HOME/.phoronix-test-suite/test-results/<sanitized>/composite.xml`
/// (falling back to `/tmp/phoronix-test-suite/test-results/<sanitized>/composite.xml`
/// if `$HOME` is unset).
pub fn pts_result_path(result_name: &str) -> PathBuf {
    let sanitized = sanitize_pts_result_name(result_name);
    let base = env_path("HOME")
        .map(|h| h.join(".phoronix-test-suite"))
        .unwrap_or_else(|| PathBuf::from("/tmp/phoronix-test-suite"));
    base.join("test-results")
        .join(sanitized)
        .join("composite.xml")
}

fn sanitize_pts_result_name(name: &str) -> String {
    let sanitized: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "zensmu-telemetry".to_string()
    } else {
        sanitized
    }
}

pub(super) fn rotated_path(path: &Path, index: usize) -> PathBuf {
    let mut name = match path.file_stem() {
        Some(stem) => stem.to_os_string(),
        None => OsString::from("telemetry"),
    };
    name.push(format!(".{index}"));
    if let Some(ext) = path.extension() {
        name.push(".");
        name.push(ext);
    }
    match path.parent().filter(|p| !p.as_os_str().is_empty()) {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

pub(super) fn derive_pts_label_from_path(path: &Path) -> String {
    let is_composite = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.eq_ignore_ascii_case("composite.xml"));
    if is_composite {
        if let Some(parent_name) = path
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty())
        {
            return parent_name.to_string();
        }
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("zensmu-telemetry")
        .to_string()
}
