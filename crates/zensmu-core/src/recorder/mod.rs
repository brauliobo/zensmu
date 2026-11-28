//! Shared zero-dependency telemetry recording (`CSV`, `JSONL`, and Phoronix Test Suite `composite.xml`).
//!
//! Supports unprivileged telemetry logging with optional size-based log rotation,
//! RFC 4180 CSV escaping, RFC 3339 UTC timestamps, Phoronix Test Suite XML export
//! and offline conversion, and user-level `systemd` service status/control helpers.

mod csv;
mod paths;
mod pts;
mod service;
#[cfg(test)]
mod tests;
mod time;
mod writer;

use crate::ops::pm_table::TelemetrySnapshot;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

pub use csv::CSV_HEADER;
pub use paths::{
    default_daemon_recording_path, default_recording_path, default_recordings_dir, pts_result_path,
};
pub use pts::{convert_jsonl_or_csv_to_pts, render_pts_composite_xml};
pub use service::{
    read_record_service_status, set_record_service_enabled, RecordServiceStatus,
    USER_RECORD_SERVICE_NAME,
};
pub use time::format_iso8601_utc_ms;
pub use writer::TelemetryRecorder;

/// Supported telemetry recording output formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RecordFormat {
    Csv,
    Jsonl,
    Pts,
}

impl RecordFormat {
    /// Infer recording format from a file path extension.
    ///
    /// `.xml` -> [`RecordFormat::Pts`], `.jsonl` / `.ndjson` -> [`RecordFormat::Jsonl`],
    /// everything else -> [`RecordFormat::Csv`].
    pub fn from_path(path: &Path) -> Self {
        match path.extension().and_then(|e| e.to_str()) {
            Some(ext) if ext.eq_ignore_ascii_case("xml") => Self::Pts,
            Some(ext)
                if ext.eq_ignore_ascii_case("jsonl") || ext.eq_ignore_ascii_case("ndjson") =>
            {
                Self::Jsonl
            }
            _ => Self::Csv,
        }
    }

    /// Canonical file extension (without leading dot).
    pub fn extension(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Jsonl => "jsonl",
            Self::Pts => "xml",
        }
    }

    /// Lowercase format identifier.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Csv => "csv",
            Self::Jsonl => "jsonl",
            Self::Pts => "pts",
        }
    }
}

impl fmt::Display for RecordFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Configuration for [`TelemetryRecorder`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordConfig {
    pub path: PathBuf,
    pub format: RecordFormat,
    pub append: bool,
    /// Rotate when `current_file_bytes + next_line_len` exceeds this limit (CSV/JSONL).
    pub max_file_bytes: Option<u64>,
    /// Number of rotated backup files to retain (`0` disables backups and truncates/restarts).
    pub max_rotated_files: usize,
    /// Flush underlying `BufWriter` every N samples (`1` = flush every sample).
    pub flush_every: usize,
    /// Optional workload label (e.g. `"prime95"` or `"co-20"`), CSV-escaped when written.
    pub tag: Option<String>,
}

impl Default for RecordConfig {
    fn default() -> Self {
        Self {
            path: default_daemon_recording_path(),
            format: RecordFormat::Csv,
            append: true,
            max_file_bytes: Some(50 * 1024 * 1024),
            max_rotated_files: 5,
            flush_every: 1,
            tag: None,
        }
    }
}

/// A single timestamped telemetry sample serialized in JSONL and used for PTS export.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecordedSample {
    pub timestamp_unix_ms: u64,
    pub timestamp_iso: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(flatten)]
    pub telemetry: TelemetrySnapshot,
}

/// Live recording statistics reported by [`TelemetryRecorder::stats`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecordStats {
    pub path: PathBuf,
    pub format: RecordFormat,
    pub samples_written: u64,
    pub bytes_written: u64,
    pub rotations: u64,
    pub started_unix_ms: u64,
}
