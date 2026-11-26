//! Shared zero-dependency telemetry recording (`CSV`, `JSONL`, and Phoronix Test Suite `composite.xml`).
//!
//! Supports unprivileged telemetry logging with optional size-based log rotation,
//! RFC 4180 CSV escaping, RFC 3339 UTC timestamps, Phoronix Test Suite XML export
//! and offline conversion, and user-level `systemd` service status/control helpers.

use crate::error::{Error, Result};
use crate::ops::pm_table::TelemetrySnapshot;
use crate::report::SystemBoardInfo;
use crate::safety::{map_io_err, sync_dir_best_effort, write_atomic};
use crate::types::Codename;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::ffi::OsString;
use std::fmt::{self, Write as _};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Default maximum in-memory samples buffered for a single PTS `composite.xml` recording.
const MAX_PTS_IN_MEMORY_SAMPLES: usize = 200_000;

/// User-level systemd unit name for background telemetry recording.
pub const USER_RECORD_SERVICE_NAME: &str = "zensmu-record.service";

/// Header row for `zensmu` CSV telemetry recordings (34 columns).
pub const CSV_HEADER: &str = "timestamp_unix_ms,timestamp_iso,codename,tag,ppt_w,ppt_limit_w,tdc_a,tdc_limit_a,edc_a,edc_limit_a,tctl_c,tctl_limit_c,stapm_w,stapm_limit_w,slow_w,slow_limit_w,apu_slow_w,apu_slow_limit_w,tdc_soc_a,tdc_soc_limit_a,edc_soc_a,edc_soc_limit_a,apu_skin_c,apu_skin_limit_c,dgpu_skin_c,dgpu_skin_limit_c,fclk_mhz,uclk_mhz,mclk_mhz,vddcr_soc_v,vddcr_misc_v,cldo_vddp_v,cldo_vddg_iod_v,cldo_vddg_ccd_v";

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

/// Return a non-empty environment variable value as a [`PathBuf`].
fn env_path(var: &str) -> Option<PathBuf> {
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

/// Current wall-clock Unix timestamp in milliseconds (`0` if system clock predates epoch).
fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Howard Hinnant's civil-from-days algorithm converting days since 1970-01-01 to `(year, month, day)`.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month as u32, day as u32)
}

fn split_utc_secs(unix_secs: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    let (year, month, day) = civil_from_days(days);
    (year, month, day, hour, minute, second)
}

/// Format Unix milliseconds as an RFC 3339 UTC string (e.g. `"2026-11-26T22:45:01.250Z"`).
pub fn format_iso8601_utc_ms(timestamp_unix_ms: u64) -> String {
    let secs = timestamp_unix_ms / 1_000;
    let ms = timestamp_unix_ms % 1_000;
    let (year, month, day, hour, minute, second) = split_utc_secs(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

fn format_compact_utc_timestamp(unix_secs: u64) -> String {
    let (year, month, day, hour, minute, second) = split_utc_secs(unix_secs);
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

fn format_pts_datetime_utc(unix_secs: u64) -> String {
    let (year, month, day, hour, minute, second) = split_utc_secs(unix_secs);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}

/// Append an RFC 4180 CSV-escaped field to `out`.
fn push_csv_escaped(out: &mut String, field: &str) {
    if field.contains([',', '"', '\n', '\r']) {
        out.push('"');
        for ch in field.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
    } else {
        out.push_str(field);
    }
}

fn push_opt_f32(out: &mut String, val: Option<f32>, precision: usize) {
    out.push(',');
    if let Some(v) = val.filter(|v| v.is_finite()) {
        let _ = write!(out, "{v:.precision$}");
    }
}

/// Format one CSV telemetry row matching [`CSV_HEADER`] column order (without trailing newline).
fn format_csv_row(
    snap: &TelemetrySnapshot,
    timestamp_unix_ms: u64,
    timestamp_iso: &str,
    tag: Option<&str>,
) -> String {
    let mut row = String::with_capacity(256);
    let _ = write!(
        row,
        "{timestamp_unix_ms},{timestamp_iso},{},",
        snap.codename.slug()
    );
    if let Some(t) = tag {
        push_csv_escaped(&mut row, t);
    }

    push_opt_f32(&mut row, snap.ppt_value_w, 2);
    push_opt_f32(&mut row, snap.ppt_limit_w, 2);
    push_opt_f32(&mut row, snap.tdc_value_a, 2);
    push_opt_f32(&mut row, snap.tdc_limit_a, 2);
    push_opt_f32(&mut row, snap.edc_value_a, 2);
    push_opt_f32(&mut row, snap.edc_limit_a, 2);
    push_opt_f32(&mut row, snap.tctl_value_c, 2);
    push_opt_f32(&mut row, snap.tctl_limit_c, 2);
    push_opt_f32(&mut row, snap.stapm_value_w, 2);
    push_opt_f32(&mut row, snap.stapm_limit_w, 2);
    push_opt_f32(&mut row, snap.slow_value_w, 2);
    push_opt_f32(&mut row, snap.slow_limit_w, 2);
    push_opt_f32(&mut row, snap.apu_slow_value_w, 2);
    push_opt_f32(&mut row, snap.apu_slow_limit_w, 2);
    push_opt_f32(&mut row, snap.tdc_soc_value_a, 2);
    push_opt_f32(&mut row, snap.tdc_soc_limit_a, 2);
    push_opt_f32(&mut row, snap.edc_soc_value_a, 2);
    push_opt_f32(&mut row, snap.edc_soc_limit_a, 2);
    push_opt_f32(&mut row, snap.apu_skin_temp_value_c, 2);
    push_opt_f32(&mut row, snap.apu_skin_temp_limit_c, 2);
    push_opt_f32(&mut row, snap.dgpu_skin_temp_value_c, 2);
    push_opt_f32(&mut row, snap.dgpu_skin_temp_limit_c, 2);
    push_opt_f32(&mut row, snap.fclk_mhz, 2);
    push_opt_f32(&mut row, snap.uclk_mhz, 2);
    push_opt_f32(&mut row, snap.mclk_mhz, 2);
    push_opt_f32(&mut row, snap.vddcr_soc_v, 4);
    push_opt_f32(&mut row, snap.vddcr_misc_v, 4);
    push_opt_f32(&mut row, snap.cldo_vddp_v, 4);
    push_opt_f32(&mut row, snap.cldo_vddg_iod_v, 4);
    push_opt_f32(&mut row, snap.cldo_vddg_ccd_v, 4);

    row
}

fn rotated_path(path: &Path, index: usize) -> PathBuf {
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamFormat {
    Csv,
    Jsonl,
}

enum RecorderBackend {
    Stream {
        stream_format: StreamFormat,
        writer: BufWriter<File>,
        current_file_bytes: u64,
        samples_since_flush: usize,
    },
    Pts {
        samples: Vec<RecordedSample>,
        dirty: bool,
    },
}

/// Streaming telemetry recorder supporting `CSV`, `JSONL`, and Phoronix Test Suite `composite.xml`.
pub struct TelemetryRecorder {
    config: RecordConfig,
    backend: RecorderBackend,
    samples_written: u64,
    bytes_written: u64,
    rotations: u64,
    started_unix_ms: u64,
}

impl TelemetryRecorder {
    /// Open or create a telemetry recording according to `config`.
    pub fn open(config: RecordConfig) -> Result<Self> {
        if let Some(parent) = config.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|e| map_io_err(parent, e))?;
        }

        let started_unix_ms = now_unix_ms();

        let stream_format = match config.format {
            RecordFormat::Csv => StreamFormat::Csv,
            RecordFormat::Jsonl => StreamFormat::Jsonl,
            RecordFormat::Pts => {
                return Ok(Self {
                    config,
                    backend: RecorderBackend::Pts {
                        samples: Vec::new(),
                        dirty: false,
                    },
                    samples_written: 0,
                    bytes_written: 0,
                    rotations: 0,
                    started_unix_ms,
                });
            }
        };

        let mut opts = OpenOptions::new();
        opts.write(true).create(true);
        if config.append {
            opts.append(true);
        } else {
            opts.truncate(true);
        }

        let file = opts
            .open(&config.path)
            .map_err(|e| map_io_err(&config.path, e))?;
        let mut current_file_bytes = file
            .metadata()
            .map_err(|e| map_io_err(&config.path, e))?
            .len();

        let mut writer = BufWriter::new(file);
        let mut bytes_written = 0u64;

        if stream_format == StreamFormat::Csv && current_file_bytes == 0 {
            writer
                .write_all(CSV_HEADER.as_bytes())
                .and_then(|()| writer.write_all(b"\n"))
                .and_then(|()| writer.flush())
                .map_err(|e| map_io_err(&config.path, e))?;
            let header_bytes = (CSV_HEADER.len() + 1) as u64;
            current_file_bytes = header_bytes;
            bytes_written = header_bytes;
        }

        Ok(Self {
            config,
            backend: RecorderBackend::Stream {
                stream_format,
                writer,
                current_file_bytes,
                samples_since_flush: 0,
            },
            samples_written: 0,
            bytes_written,
            rotations: 0,
            started_unix_ms,
        })
    }

    /// Record a telemetry snapshot using the current wall-clock UTC timestamp.
    pub fn write_snapshot(&mut self, snap: &TelemetrySnapshot) -> Result<()> {
        self.write_sample_at(snap, now_unix_ms())
    }

    /// Record a telemetry snapshot at an explicit Unix millisecond timestamp.
    pub fn write_sample_at(
        &mut self,
        snap: &TelemetrySnapshot,
        timestamp_unix_ms: u64,
    ) -> Result<()> {
        let timestamp_iso = format_iso8601_utc_ms(timestamp_unix_ms);

        match self.backend {
            RecorderBackend::Pts {
                ref mut samples,
                ref mut dirty,
            } => {
                if samples.len() < MAX_PTS_IN_MEMORY_SAMPLES {
                    samples.push(RecordedSample {
                        timestamp_unix_ms,
                        timestamp_iso,
                        tag: self.config.tag.clone(),
                        telemetry: snap.clone(),
                    });
                }
                *dirty = true;
                self.samples_written = self.samples_written.saturating_add(1);
                Ok(())
            }
            RecorderBackend::Stream {
                stream_format,
                current_file_bytes,
                ..
            } => {
                let mut line = match stream_format {
                    StreamFormat::Csv => format_csv_row(
                        snap,
                        timestamp_unix_ms,
                        &timestamp_iso,
                        self.config.tag.as_deref(),
                    ),
                    StreamFormat::Jsonl => {
                        let sample = RecordedSample {
                            timestamp_unix_ms,
                            timestamp_iso,
                            tag: self.config.tag.clone(),
                            telemetry: snap.clone(),
                        };
                        serde_json::to_string(&sample).map_err(|e| Error::Serde(e.to_string()))?
                    }
                };
                line.push('\n');

                let line_len = line.len() as u64;
                let min_bytes_before_rotate = if stream_format == StreamFormat::Csv {
                    (CSV_HEADER.len() + 1) as u64
                } else {
                    0
                };

                let needs_rotate = self.config.max_file_bytes.is_some_and(|max_bytes| {
                    current_file_bytes > min_bytes_before_rotate
                        && current_file_bytes.saturating_add(line_len) > max_bytes
                });

                if needs_rotate {
                    self.rotate_files()?;
                }

                if let RecorderBackend::Stream {
                    ref mut writer,
                    ref mut current_file_bytes,
                    ref mut samples_since_flush,
                    ..
                } = self.backend
                {
                    writer
                        .write_all(line.as_bytes())
                        .map_err(|e| map_io_err(&self.config.path, e))?;
                    self.samples_written = self.samples_written.saturating_add(1);
                    self.bytes_written = self.bytes_written.saturating_add(line_len);
                    *current_file_bytes = current_file_bytes.saturating_add(line_len);
                    *samples_since_flush = samples_since_flush.saturating_add(1);

                    let flush_every = self.config.flush_every.max(1);
                    if *samples_since_flush >= flush_every {
                        writer
                            .flush()
                            .map_err(|e| map_io_err(&self.config.path, e))?;
                        *samples_since_flush = 0;
                    }
                }

                Ok(())
            }
        }
    }

    fn rotate_files(&mut self) -> Result<()> {
        let stream_format = if let RecorderBackend::Stream {
            stream_format,
            ref mut writer,
            ..
        } = self.backend
        {
            writer
                .flush()
                .map_err(|e| map_io_err(&self.config.path, e))?;
            stream_format
        } else {
            return Ok(());
        };

        if self.config.max_rotated_files > 0 {
            for idx in (2..=self.config.max_rotated_files).rev() {
                let src = rotated_path(&self.config.path, idx - 1);
                let dst = rotated_path(&self.config.path, idx);
                match fs::rename(&src, &dst) {
                    Ok(()) => {}
                    Err(e) if e.kind() == ErrorKind::NotFound => {}
                    Err(e) => return Err(map_io_err(&dst, e)),
                }
            }

            let first_backup = rotated_path(&self.config.path, 1);
            match fs::rename(&self.config.path, &first_backup) {
                Ok(()) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => {}
                Err(e) => return Err(map_io_err(&first_backup, e)),
            }
        }

        let new_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&self.config.path)
            .map_err(|e| map_io_err(&self.config.path, e))?;

        let mut new_writer = BufWriter::new(new_file);
        let mut current_file_bytes = 0u64;

        if stream_format == StreamFormat::Csv {
            new_writer
                .write_all(CSV_HEADER.as_bytes())
                .and_then(|()| new_writer.write_all(b"\n"))
                .and_then(|()| new_writer.flush())
                .map_err(|e| map_io_err(&self.config.path, e))?;
            let header_bytes = (CSV_HEADER.len() + 1) as u64;
            current_file_bytes = header_bytes;
            self.bytes_written = self.bytes_written.saturating_add(header_bytes);
        }

        if let Some(parent) = self
            .config
            .path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
        {
            sync_dir_best_effort(parent);
        }

        self.backend = RecorderBackend::Stream {
            stream_format,
            writer: new_writer,
            current_file_bytes,
            samples_since_flush: 0,
        };
        self.rotations = self.rotations.saturating_add(1);
        Ok(())
    }

    fn pts_title_and_identifier(&self, samples: &[RecordedSample]) -> (String, String, Codename) {
        let codename = samples
            .first()
            .map(|s| s.telemetry.codename)
            .unwrap_or(Codename::Undefined);

        let path_label = derive_pts_label_from_path(&self.config.path);
        let tag = self
            .config
            .tag
            .as_deref()
            .map(str::trim)
            .filter(|t| !t.is_empty());

        let title = match tag {
            Some(t) => format!("zensmu telemetry ({t})"),
            None => format!("zensmu telemetry - {path_label}"),
        };
        let identifier = tag
            .map(str::to_string)
            .unwrap_or_else(|| codename.slug().to_string());

        (title, identifier, codename)
    }

    /// Flush buffered output to disk (or atomically write `composite.xml` for [`RecordFormat::Pts`]).
    pub fn flush(&mut self) -> Result<()> {
        match self.backend {
            RecorderBackend::Stream {
                ref mut writer,
                ref mut samples_since_flush,
                ..
            } => {
                writer
                    .flush()
                    .map_err(|e| map_io_err(&self.config.path, e))?;
                *samples_since_flush = 0;
                Ok(())
            }
            RecorderBackend::Pts { .. } => {
                let xml = if let RecorderBackend::Pts { ref samples, .. } = self.backend {
                    let (title, identifier, codename) = self.pts_title_and_identifier(samples);
                    render_pts_composite_xml(&title, &identifier, codename, samples)
                } else {
                    return Ok(());
                };
                write_atomic(&self.config.path, xml.as_bytes())?;
                self.bytes_written = xml.len() as u64;
                if let RecorderBackend::Pts { ref mut dirty, .. } = self.backend {
                    *dirty = false;
                }
                Ok(())
            }
        }
    }

    /// Current recording statistics.
    pub fn stats(&self) -> RecordStats {
        RecordStats {
            path: self.config.path.clone(),
            format: self.config.format,
            samples_written: self.samples_written,
            bytes_written: self.bytes_written,
            rotations: self.rotations,
            started_unix_ms: self.started_unix_ms,
        }
    }

    /// Active output path.
    pub fn path(&self) -> &Path {
        &self.config.path
    }

    /// Configured output format.
    pub fn format(&self) -> RecordFormat {
        self.config.format
    }
}

impl Drop for TelemetryRecorder {
    fn drop(&mut self) {
        match self.backend {
            RecorderBackend::Stream { ref mut writer, .. } => {
                let _ = writer.flush();
            }
            RecorderBackend::Pts { dirty: true, .. } => {
                let _ = self.flush();
            }
            RecorderBackend::Pts { dirty: false, .. } => {}
        }
    }
}

fn derive_pts_label_from_path(path: &Path) -> String {
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

/// XML-escape `&`, `<`, `>`, `"`, and `'`.
fn xml_escape(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

struct PtsMetricSpec {
    slug: &'static str,
    title: &'static str,
    unit: &'static str,
    proportion: &'static str,
    precision: usize,
    extract: fn(&TelemetrySnapshot) -> Option<f32>,
}

const PTS_METRICS: &[PtsMetricSpec] = &[
    PtsMetricSpec {
        slug: "ppt",
        title: "Package Power (PPT)",
        unit: "Watts",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.ppt_value_w,
    },
    PtsMetricSpec {
        slug: "tctl",
        title: "CPU Temperature (Tctl)",
        unit: "Celsius",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.tctl_value_c,
    },
    PtsMetricSpec {
        slug: "tdc",
        title: "VDD TDC Current",
        unit: "Amps",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.tdc_value_a,
    },
    PtsMetricSpec {
        slug: "edc",
        title: "VDD EDC Current",
        unit: "Amps",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.edc_value_a,
    },
    PtsMetricSpec {
        slug: "stapm",
        title: "STAPM Power",
        unit: "Watts",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.stapm_value_w,
    },
    PtsMetricSpec {
        slug: "slow-ppt",
        title: "Slow PPT",
        unit: "Watts",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.slow_value_w,
    },
    PtsMetricSpec {
        slug: "apu-slow-ppt",
        title: "APU Slow PPT",
        unit: "Watts",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.apu_slow_value_w,
    },
    PtsMetricSpec {
        slug: "tdc-soc",
        title: "SoC TDC",
        unit: "Amps",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.tdc_soc_value_a,
    },
    PtsMetricSpec {
        slug: "edc-soc",
        title: "SoC EDC",
        unit: "Amps",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.edc_soc_value_a,
    },
    PtsMetricSpec {
        slug: "apu-skin-temp",
        title: "APU Skin Temp",
        unit: "Celsius",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.apu_skin_temp_value_c,
    },
    PtsMetricSpec {
        slug: "dgpu-skin-temp",
        title: "dGPU Skin Temp",
        unit: "Celsius",
        proportion: "LIB",
        precision: 2,
        extract: |t| t.dgpu_skin_temp_value_c,
    },
    PtsMetricSpec {
        slug: "fclk",
        title: "Fabric Clock (FCLK)",
        unit: "MHz",
        proportion: "HIB",
        precision: 2,
        extract: |t| t.fclk_mhz,
    },
    PtsMetricSpec {
        slug: "uclk",
        title: "Memory Controller Clock (UCLK)",
        unit: "MHz",
        proportion: "HIB",
        precision: 2,
        extract: |t| t.uclk_mhz,
    },
    PtsMetricSpec {
        slug: "mclk",
        title: "Memory Clock (MCLK)",
        unit: "MHz",
        proportion: "HIB",
        precision: 2,
        extract: |t| t.mclk_mhz,
    },
    PtsMetricSpec {
        slug: "vddcr-soc",
        title: "VDDCR_SOC Voltage",
        unit: "Volts",
        proportion: "LIB",
        precision: 4,
        extract: |t| t.vddcr_soc_v,
    },
];

/// Render a Phoronix Test Suite `composite.xml` document from recorded telemetry samples.
pub fn render_pts_composite_xml(
    title: &str,
    identifier: &str,
    codename: Codename,
    samples: &[RecordedSample],
) -> String {
    let sys = SystemBoardInfo::detect_linux();
    render_pts_composite_xml_with_system(title, identifier, codename, samples, &sys)
}

fn render_pts_composite_xml_with_system(
    title: &str,
    identifier: &str,
    codename: Codename,
    samples: &[RecordedSample],
    sys: &SystemBoardInfo,
) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let last_modified_secs = samples
        .last()
        .map(|s| s.timestamp_unix_ms / 1_000)
        .unwrap_or_else(|| now_unix_ms() / 1_000);
    let last_modified = format_pts_datetime_utc(last_modified_secs);

    let description = format!(
        "zensmu telemetry recording on {} ({} samples)",
        codename.display_name(),
        samples.len()
    );

    let mut hardware = format!("Processor: {}", codename.display_name());
    let board = match (&sys.board_vendor, &sys.board_name) {
        (Some(v), Some(n)) => Some(format!("{v} {n}")),
        (None, Some(n)) => Some(n.clone()),
        (Some(v), None) => Some(v.clone()),
        (None, None) => None,
    };
    if let Some(board_str) = board {
        let _ = write!(hardware, ", Motherboard: {board_str}");
    }
    if let Some(ref bios) = sys.bios_version {
        let _ = write!(hardware, ", BIOS: {bios}");
        if let Some(ref bios_date) = sys.bios_date {
            let _ = write!(hardware, " ({bios_date})");
        }
    }

    let os_name = sys.os_pretty_name.as_deref().unwrap_or("Linux");
    let kernel = sys.kernel_release.as_deref().unwrap_or("unknown");
    let software = format!("OS: {os_name}, Kernel: {kernel}");

    let arguments = samples
        .iter()
        .find_map(|s| s.tag.as_deref().filter(|t| !t.is_empty()))
        .unwrap_or("live-telemetry");

    let escaped_title = xml_escape(title);
    let escaped_identifier = xml_escape(identifier);
    let escaped_description = xml_escape(&description);
    let escaped_hardware = xml_escape(&hardware);
    let escaped_software = xml_escape(&software);
    let escaped_arguments = xml_escape(arguments);
    let escaped_version = xml_escape(version);

    let mut xml = String::with_capacity(2048);
    xml.push_str("<?xml version=\"1.0\"?>\n");
    xml.push_str("<PhoronixTestSuite>\n");
    xml.push_str("  <Generated>\n");
    let _ = writeln!(xml, "    <Title>{escaped_title}</Title>");
    let _ = writeln!(xml, "    <LastModified>{last_modified}</LastModified>");
    let _ = writeln!(
        xml,
        "    <TestClient>zensmu v{escaped_version}</TestClient>"
    );
    let _ = writeln!(xml, "    <Description>{escaped_description}</Description>");
    xml.push_str("  </Generated>\n");
    xml.push_str("  <System>\n");
    let _ = writeln!(xml, "    <Identifier>{escaped_identifier}</Identifier>");
    let _ = writeln!(xml, "    <Hardware>{escaped_hardware}</Hardware>");
    let _ = writeln!(xml, "    <Software>{escaped_software}</Software>");
    xml.push_str("  </System>\n");

    let mut values: Vec<f32> = Vec::new();
    for metric in PTS_METRICS {
        values.clear();
        for sample in samples {
            if let Some(v) = (metric.extract)(&sample.telemetry).filter(|v| v.is_finite()) {
                values.push(v);
            }
        }
        if values.is_empty() {
            continue;
        }

        let mut sum = 0.0_f64;
        let mut min_v = f64::INFINITY;
        let mut max_v = f64::NEG_INFINITY;
        for &v in &values {
            let vf = f64::from(v);
            sum += vf;
            if vf < min_v {
                min_v = vf;
            }
            if vf > max_v {
                max_v = vf;
            }
        }
        let mean = sum / (values.len() as f64);
        let prec = metric.precision;

        let mut raw_series = String::with_capacity(values.len() * 8);
        for (i, &v) in values.iter().enumerate() {
            if i > 0 {
                raw_series.push(':');
            }
            let _ = write!(raw_series, "{v:.prec$}");
        }

        let json_summary = format!(
            "{{\"min\":{min_v:.prec$},\"max\":{max_v:.prec$},\"avg\":{mean:.prec$},\"samples\":{}}}",
            values.len()
        );

        xml.push_str("  <Result>\n");
        let _ = writeln!(xml, "    <Identifier>zensmu/{}</Identifier>", metric.slug);
        let _ = writeln!(xml, "    <Title>{}</Title>", xml_escape(metric.title));
        let _ = writeln!(xml, "    <AppVersion>{escaped_version}</AppVersion>");
        let _ = writeln!(xml, "    <Arguments>{escaped_arguments}</Arguments>");
        let _ = writeln!(
            xml,
            "    <Description>Recorded over {} samples</Description>",
            values.len()
        );
        let _ = writeln!(xml, "    <Scale>{}</Scale>", xml_escape(metric.unit));
        let _ = writeln!(xml, "    <Proportion>{}</Proportion>", metric.proportion);
        xml.push_str("    <DisplayFormat>LINE_GRAPH</DisplayFormat>\n");
        xml.push_str("    <Data>\n");
        xml.push_str("      <Entry>\n");
        let _ = writeln!(xml, "        <Identifier>{escaped_identifier}</Identifier>");
        let _ = writeln!(xml, "        <Value>{mean:.prec$}</Value>");
        let _ = writeln!(xml, "        <RawString>{raw_series}</RawString>");
        let _ = writeln!(xml, "        <JSON>{json_summary}</JSON>");
        xml.push_str("      </Entry>\n");
        xml.push_str("    </Data>\n");
        xml.push_str("  </Result>\n");
    }

    xml.push_str("</PhoronixTestSuite>\n");
    xml
}

/// Convert an existing `zensmu` CSV or JSONL recording file into a Phoronix Test Suite
/// `composite.xml` file, returning the number of converted samples.
pub fn convert_jsonl_or_csv_to_pts(
    input: &Path,
    output: &Path,
    title: Option<&str>,
    identifier: Option<&str>,
) -> Result<usize> {
    let content = fs::read_to_string(input).map_err(|e| map_io_err(input, e))?;
    let first_non_empty = content.lines().map(str::trim).find(|l| !l.is_empty());

    let is_jsonl = RecordFormat::from_path(input) == RecordFormat::Jsonl
        || first_non_empty.is_some_and(|line| line.starts_with('{'));

    let samples = if is_jsonl {
        parse_jsonl_samples(&content)?
    } else {
        parse_csv_samples(&content)?
    };

    if samples.is_empty() {
        return Err(Error::InvalidInput(format!(
            "no telemetry samples found in {}",
            input.display()
        )));
    }

    let codename = samples
        .first()
        .map(|s| s.telemetry.codename)
        .unwrap_or(Codename::Undefined);

    let resolved_title = title
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| format!("zensmu telemetry - {}", derive_pts_label_from_path(input)));

    let resolved_identifier = identifier
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| {
            samples
                .iter()
                .find_map(|s| s.tag.as_deref().filter(|t| !t.is_empty()))
                .map(str::to_string)
        })
        .unwrap_or_else(|| codename.slug().to_string());

    let xml = render_pts_composite_xml(&resolved_title, &resolved_identifier, codename, &samples);
    write_atomic(output, xml.as_bytes())?;
    Ok(samples.len())
}

fn parse_jsonl_samples(content: &str) -> Result<Vec<RecordedSample>> {
    let mut samples = Vec::new();
    for (idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let sample: RecordedSample = serde_json::from_str(trimmed).map_err(|e| {
            Error::Serde(format!(
                "failed to parse JSONL telemetry sample on line {}: {e}",
                idx + 1
            ))
        })?;
        samples.push(sample);
    }
    Ok(samples)
}

/// Minimal RFC 4180 CSV parser supporting quoted fields, doubled quotes (`""`),
/// and embedded commas/newlines.
fn parse_csv_records(content: &str) -> Result<Vec<Vec<String>>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_field = String::new();
    let mut in_quotes = false;
    let mut field_started = false;
    let mut chars = content.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    let _ = chars.next();
                    current_field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                current_field.push(ch);
            }
        } else {
            match ch {
                '"' if !field_started => {
                    in_quotes = true;
                    field_started = true;
                }
                ',' => {
                    current_row.push(std::mem::take(&mut current_field));
                    field_started = false;
                }
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        let _ = chars.next();
                    }
                    if field_started || !current_row.is_empty() {
                        current_row.push(std::mem::take(&mut current_field));
                        records.push(std::mem::take(&mut current_row));
                        field_started = false;
                    }
                }
                '\n' => {
                    if field_started || !current_row.is_empty() {
                        current_row.push(std::mem::take(&mut current_field));
                        records.push(std::mem::take(&mut current_row));
                        field_started = false;
                    }
                }
                other => {
                    current_field.push(other);
                    field_started = true;
                }
            }
        }
    }

    if in_quotes {
        return Err(Error::InvalidInput(
            "unterminated quoted field in CSV recording".to_string(),
        ));
    }
    if field_started || !current_row.is_empty() {
        current_row.push(current_field);
        records.push(current_row);
    }

    Ok(records)
}

fn parse_csv_samples(content: &str) -> Result<Vec<RecordedSample>> {
    let records = parse_csv_records(content)?;
    let Some(header) = records.first() else {
        return Ok(Vec::new());
    };

    let col_map: HashMap<&str, usize> = header
        .iter()
        .enumerate()
        .map(|(i, name)| (name.trim(), i))
        .collect();

    if !col_map.contains_key("timestamp_unix_ms") || !col_map.contains_key("codename") {
        return Err(Error::InvalidInput(
            "CSV recording is missing required header columns (timestamp_unix_ms, codename)"
                .to_string(),
        ));
    }

    let mut samples = Vec::with_capacity(records.len().saturating_sub(1));
    for (row_idx, row) in records.iter().enumerate().skip(1) {
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let get = |col: &str| -> &str {
            col_map
                .get(col)
                .and_then(|&idx| row.get(idx))
                .map(String::as_str)
                .unwrap_or("")
        };
        let get_opt_f32 = |col: &str| -> Result<Option<f32>> {
            let raw = get(col).trim();
            if raw.is_empty() {
                return Ok(None);
            }
            let val = raw.parse::<f32>().map_err(|e| {
                Error::InvalidInput(format!(
                    "invalid float '{raw}' for column '{col}' on CSV row {}: {e}",
                    row_idx + 2
                ))
            })?;
            Ok(val.is_finite().then_some(val))
        };

        let ts_raw = get("timestamp_unix_ms").trim();
        let timestamp_unix_ms = ts_raw.parse::<u64>().map_err(|e| {
            Error::InvalidInput(format!(
                "invalid timestamp_unix_ms '{ts_raw}' on CSV row {}: {e}",
                row_idx + 2
            ))
        })?;

        let iso_raw = get("timestamp_iso").trim();
        let timestamp_iso = if iso_raw.is_empty() {
            format_iso8601_utc_ms(timestamp_unix_ms)
        } else {
            iso_raw.to_string()
        };

        let codename = codename_from_slug(get("codename").trim());
        let tag_raw = get("tag");
        let tag = (!tag_raw.is_empty()).then(|| tag_raw.to_string());

        let telemetry = TelemetrySnapshot {
            codename,
            table_version: 0,
            table_size: 0,
            ppt_limit_w: get_opt_f32("ppt_limit_w")?,
            ppt_value_w: get_opt_f32("ppt_w")?,
            tdc_limit_a: get_opt_f32("tdc_limit_a")?,
            tdc_value_a: get_opt_f32("tdc_a")?,
            edc_limit_a: get_opt_f32("edc_limit_a")?,
            edc_value_a: get_opt_f32("edc_a")?,
            tctl_limit_c: get_opt_f32("tctl_limit_c")?,
            tctl_value_c: get_opt_f32("tctl_c")?,
            stapm_limit_w: get_opt_f32("stapm_limit_w")?,
            stapm_value_w: get_opt_f32("stapm_w")?,
            slow_limit_w: get_opt_f32("slow_limit_w")?,
            slow_value_w: get_opt_f32("slow_w")?,
            apu_slow_limit_w: get_opt_f32("apu_slow_limit_w")?,
            apu_slow_value_w: get_opt_f32("apu_slow_w")?,
            tdc_soc_limit_a: get_opt_f32("tdc_soc_limit_a")?,
            tdc_soc_value_a: get_opt_f32("tdc_soc_a")?,
            edc_soc_limit_a: get_opt_f32("edc_soc_limit_a")?,
            edc_soc_value_a: get_opt_f32("edc_soc_a")?,
            apu_skin_temp_limit_c: get_opt_f32("apu_skin_limit_c")?,
            apu_skin_temp_value_c: get_opt_f32("apu_skin_c")?,
            dgpu_skin_temp_limit_c: get_opt_f32("dgpu_skin_limit_c")?,
            dgpu_skin_temp_value_c: get_opt_f32("dgpu_skin_c")?,
            fclk_mhz: get_opt_f32("fclk_mhz")?,
            uclk_mhz: get_opt_f32("uclk_mhz")?,
            mclk_mhz: get_opt_f32("mclk_mhz")?,
            vddcr_soc_v: get_opt_f32("vddcr_soc_v")?,
            vddcr_misc_v: get_opt_f32("vddcr_misc_v")?,
            cldo_vddp_v: get_opt_f32("cldo_vddp_v")?,
            cldo_vddg_iod_v: get_opt_f32("cldo_vddg_iod_v")?,
            cldo_vddg_ccd_v: get_opt_f32("cldo_vddg_ccd_v")?,
        };

        samples.push(RecordedSample {
            timestamp_unix_ms,
            timestamp_iso,
            tag,
            telemetry,
        });
    }

    Ok(samples)
}

fn codename_from_slug(slug: &str) -> Codename {
    match slug.to_ascii_lowercase().as_str() {
        "colfax" => Codename::Colfax,
        "renoir" => Codename::Renoir,
        "picasso" => Codename::Picasso,
        "matisse" => Codename::Matisse,
        "threadripper" | "thread-ripper" => Codename::ThreadRipper,
        "castle-peak" => Codename::CastlePeak,
        "raven-ridge" => Codename::RavenRidge,
        "raven-ridge-2" | "raven-ridge2" => Codename::RavenRidge2,
        "summit-ridge" => Codename::SummitRidge,
        "pinnacle-ridge" => Codename::PinnacleRidge,
        "rembrandt" => Codename::Rembrandt,
        "vermeer" => Codename::Vermeer,
        "van-gogh" => Codename::VanGogh,
        "cezanne" => Codename::Cezanne,
        "milan" => Codename::Milan,
        "dali" => Codename::Dali,
        "lucienne" => Codename::Lucienne,
        "naples" => Codename::Naples,
        "chagall" => Codename::Chagall,
        "raphael" => Codename::Raphael,
        "phoenix" => Codename::Phoenix,
        "strix-point" => Codename::StrixPoint,
        "granite-ridge" => Codename::GraniteRidge,
        "hawk-point" => Codename::HawkPoint,
        "storm-peak" => Codename::StormPeak,
        "strix-halo" => Codename::StrixHalo,
        _ => Codename::Undefined,
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_snapshot(ppt: f32, tctl: f32) -> TelemetrySnapshot {
        TelemetrySnapshot {
            codename: Codename::GraniteRidge,
            table_version: 0x0062_0105,
            table_size: 0x724,
            ppt_limit_w: Some(142.0),
            ppt_value_w: Some(ppt),
            tdc_limit_a: Some(110.0),
            tdc_value_a: Some(65.5),
            edc_limit_a: Some(170.0),
            edc_value_a: Some(92.25),
            tctl_limit_c: Some(95.0),
            tctl_value_c: Some(tctl),
            stapm_limit_w: None,
            stapm_value_w: None,
            slow_limit_w: None,
            slow_value_w: None,
            apu_slow_limit_w: None,
            apu_slow_value_w: None,
            tdc_soc_limit_a: None,
            tdc_soc_value_a: None,
            edc_soc_limit_a: None,
            edc_soc_value_a: None,
            apu_skin_temp_limit_c: None,
            apu_skin_temp_value_c: None,
            dgpu_skin_temp_limit_c: None,
            dgpu_skin_temp_value_c: None,
            fclk_mhz: Some(2000.0),
            uclk_mhz: Some(3000.0),
            mclk_mhz: Some(3000.0),
            vddcr_soc_v: Some(1.245),
            vddcr_misc_v: Some(1.1),
            cldo_vddp_v: Some(0.95),
            cldo_vddg_iod_v: Some(0.95),
            cldo_vddg_ccd_v: Some(0.95),
        }
    }

    #[test]
    fn csv_recording_writes_header_once_and_appends_without_duplicate_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.csv");
        let expected_cols = CSV_HEADER.split(',').count();

        {
            let mut rec = TelemetryRecorder::open(RecordConfig {
                path: path.clone(),
                format: RecordFormat::Csv,
                append: true,
                max_file_bytes: None,
                max_rotated_files: 0,
                flush_every: 1,
                tag: Some("prime95, \"small-fft\"".to_string()),
            })
            .unwrap();
            rec.write_sample_at(&sample_snapshot(88.5, 72.25), 1_700_000_000_125)
                .unwrap();
            assert_eq!(rec.stats().samples_written, 1);
            assert_eq!(rec.format(), RecordFormat::Csv);
            assert_eq!(rec.path(), path.as_path());
        }

        // Re-open in append mode: must NOT write a duplicate header.
        {
            let mut rec = TelemetryRecorder::open(RecordConfig {
                path: path.clone(),
                format: RecordFormat::Csv,
                append: true,
                max_file_bytes: None,
                max_rotated_files: 0,
                flush_every: 1,
                tag: None,
            })
            .unwrap();
            rec.write_sample_at(&sample_snapshot(91.0, 74.5), 1_700_000_001_500)
                .unwrap();
        }

        let raw = fs::read_to_string(&path).unwrap();
        let records = parse_csv_records(&raw).unwrap();
        assert_eq!(records.len(), 3, "expected 1 header + 2 data rows");
        assert_eq!(records[0].join(","), CSV_HEADER);
        assert_eq!(records[1].len(), expected_cols);
        assert_eq!(records[2].len(), expected_cols);

        // Verify RFC 4180 tag quoting in raw line and parsed field value.
        let lines: Vec<&str> = raw.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(lines[1].contains("\"prime95, \"\"small-fft\"\"\""));
        assert_eq!(records[1][2], "granite-ridge");
        assert_eq!(records[1][3], "prime95, \"small-fft\"");
        assert_eq!(records[1][4], "88.50");
        assert_eq!(records[1][5], "142.00");
        assert_eq!(records[1][10], "72.25");
        assert_eq!(records[1][11], "95.00");
        // Optional APU fields must be empty strings when None.
        assert_eq!(records[1][12], "");
        assert_eq!(records[1][13], "");
        // Voltage fields have 4 decimal places.
        assert_eq!(records[1][29], "1.2450");

        // Second row has empty tag.
        assert_eq!(records[2][3], "");
        assert_eq!(records[2][4], "91.00");
    }

    #[test]
    fn jsonl_recording_parses_back_cleanly_per_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("telemetry.jsonl");
        assert_eq!(RecordFormat::from_path(&path), RecordFormat::Jsonl);
        assert_eq!(
            RecordFormat::from_path(Path::new("run.ndjson")),
            RecordFormat::Jsonl
        );

        let mut rec = TelemetryRecorder::open(RecordConfig {
            path: path.clone(),
            format: RecordFormat::Jsonl,
            append: false,
            max_file_bytes: None,
            max_rotated_files: 0,
            flush_every: 2,
            tag: Some("co-20".to_string()),
        })
        .unwrap();

        rec.write_sample_at(&sample_snapshot(80.0, 65.0), 1_700_000_000_000)
            .unwrap();
        rec.write_sample_at(&sample_snapshot(105.25, 78.75), 1_700_000_002_250)
            .unwrap();
        rec.flush().unwrap();

        let content = fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2);

        let s0: RecordedSample = serde_json::from_str(lines[0]).unwrap();
        let s1: RecordedSample = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(s0.timestamp_unix_ms, 1_700_000_000_000);
        assert_eq!(s0.tag.as_deref(), Some("co-20"));
        assert_eq!(s0.telemetry.codename, Codename::GraniteRidge);
        assert_eq!(s0.telemetry.ppt_value_w, Some(80.0));
        assert_eq!(s1.timestamp_unix_ms, 1_700_000_002_250);
        assert_eq!(s1.telemetry.ppt_value_w, Some(105.25));
        assert_eq!(s1.telemetry.tctl_value_c, Some(78.75));
    }

    #[test]
    fn size_based_rotation_shifts_numbered_backups_and_rewrites_csv_header() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("metrics.csv");

        // Header is ~374 bytes, each row is ~145 bytes. Set max_file_bytes so
        // 1 sample fits per file and each subsequent sample triggers rotation.
        let max_bytes = (CSV_HEADER.len() + 1 + 220) as u64;

        let mut rec = TelemetryRecorder::open(RecordConfig {
            path: path.clone(),
            format: RecordFormat::Csv,
            append: false,
            max_file_bytes: Some(max_bytes),
            max_rotated_files: 2,
            flush_every: 1,
            tag: Some("stress".to_string()),
        })
        .unwrap();

        rec.write_sample_at(&sample_snapshot(50.0, 55.0), 1_700_000_001_000)
            .unwrap();
        rec.write_sample_at(&sample_snapshot(60.0, 60.0), 1_700_000_002_000)
            .unwrap();
        rec.write_sample_at(&sample_snapshot(70.0, 65.0), 1_700_000_003_000)
            .unwrap();
        rec.write_sample_at(&sample_snapshot(80.0, 70.0), 1_700_000_004_000)
            .unwrap();

        let stats = rec.stats();
        assert_eq!(stats.samples_written, 4);
        assert_eq!(stats.rotations, 3);

        let active = fs::read_to_string(&path).unwrap();
        let b1 = fs::read_to_string(dir.path().join("metrics.1.csv")).unwrap();
        let b2 = fs::read_to_string(dir.path().join("metrics.2.csv")).unwrap();
        assert!(!dir.path().join("metrics.3.csv").exists());

        for (label, content, expected_ppt) in [
            ("active", &active, "80.00"),
            ("backup1", &b1, "70.00"),
            ("backup2", &b2, "60.00"),
        ] {
            let lines: Vec<&str> = content.lines().collect();
            assert_eq!(lines.len(), 2, "{label} should have header + 1 row");
            assert_eq!(lines[0], CSV_HEADER, "{label} must start with CSV_HEADER");
            assert!(
                lines[1].contains(expected_ppt),
                "{label} expected ppt {expected_ppt}, got: {}",
                lines[1]
            );
        }
    }

    #[test]
    fn iso8601_formatting_handles_epoch_and_leap_year() {
        assert_eq!(format_iso8601_utc_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(format_iso8601_utc_ms(123), "1970-01-01T00:00:00.123Z");
        // 2000-02-29T12:34:56.789Z (leap century divisible by 400)
        // Days from 1970-01-01 to 2000-02-29 = 30*365 + 7 leap years (72..96) + 31 (Jan) + 28 (Feb) = 11016
        let y2k_leap_ms = (11_016u64 * 86_400 + 12 * 3600 + 34 * 60 + 56) * 1_000 + 789;
        assert_eq!(
            format_iso8601_utc_ms(y2k_leap_ms),
            "2000-02-29T12:34:56.789Z"
        );
        // 2024-02-29T23:59:59.999Z
        // Days from 1970-01-01 to 2024-02-29 = 54*365 + 13 leap years + 31 + 28 = 19782
        let leap_2024_ms = (19_782u64 * 86_400 + 86_399) * 1_000 + 999;
        assert_eq!(
            format_iso8601_utc_ms(leap_2024_ms),
            "2024-02-29T23:59:59.999Z"
        );
        // Next millisecond rolls over to 2024-03-01T00:00:00.000Z
        assert_eq!(
            format_iso8601_utc_ms(leap_2024_ms + 1),
            "2024-03-01T00:00:00.000Z"
        );
    }

    #[test]
    fn pts_recording_writes_valid_composite_xml_with_escaping_and_stats() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench-run").join("composite.xml");
        assert_eq!(RecordFormat::from_path(&path), RecordFormat::Pts);
        assert_eq!(RecordFormat::Pts.extension(), "xml");
        assert_eq!(RecordFormat::Pts.as_str(), "pts");

        let pts_path = pts_result_path("my run/v1.0 <test>");
        assert!(pts_path
            .display()
            .to_string()
            .ends_with("test-results/my-run-v1-0--test-/composite.xml"));

        {
            let mut rec = TelemetryRecorder::open(RecordConfig {
                path: path.clone(),
                format: RecordFormat::Pts,
                append: false,
                max_file_bytes: None,
                max_rotated_files: 0,
                flush_every: 1,
                tag: Some("co-20 & <\"stress\"> 'run'".to_string()),
            })
            .unwrap();

            rec.write_sample_at(&sample_snapshot(80.0, 65.0), 1_700_000_000_000)
                .unwrap();
            rec.write_sample_at(&sample_snapshot(100.0, 75.0), 1_700_000_001_000)
                .unwrap();
            rec.flush().unwrap();
            assert_eq!(rec.stats().samples_written, 2);
            assert!(rec.stats().bytes_written > 0);
        }

        let xml = fs::read_to_string(&path).unwrap();
        assert!(xml.starts_with("<?xml version=\"1.0\"?>"));
        assert!(xml.contains("<PhoronixTestSuite>"));
        assert!(xml.contains("</PhoronixTestSuite>"));
        assert!(xml.contains("<Result>"));
        assert!(xml.contains("<Identifier>zensmu/ppt</Identifier>"));
        assert!(xml.contains("<Title>Package Power (PPT)</Title>"));
        assert!(xml.contains("<Scale>Watts</Scale>"));
        assert!(xml.contains("<Proportion>LIB</Proportion>"));
        assert!(xml.contains("<DisplayFormat>LINE_GRAPH</DisplayFormat>"));
        // Mean of 80.0 and 100.0 is 90.00
        assert!(xml.contains("<Value>90.00</Value>"));
        assert!(xml.contains("<RawString>80.00:100.00</RawString>"));
        assert!(
            xml.contains("<JSON>{\"min\":80.00,\"max\":100.00,\"avg\":90.00,\"samples\":2}</JSON>")
        );
        // Clock metric should be Higher Is Better (HIB)
        assert!(xml.contains("<Identifier>zensmu/fclk</Identifier>"));
        assert!(xml.contains("<Scale>MHz</Scale>"));
        assert!(xml.contains("<Proportion>HIB</Proportion>"));
        // XML escaping of &, <, >, ", '
        assert!(xml.contains("co-20 &amp; &lt;&quot;stress&quot;&gt; &apos;run&apos;"));
        // Unused APU-only metrics should not be emitted
        assert!(!xml.contains("zensmu/stapm"));
    }

    #[test]
    fn convert_jsonl_or_csv_to_pts_handles_both_formats() {
        let dir = tempfile::tempdir().unwrap();
        let csv_path = dir.path().join("input.csv");
        let jsonl_path = dir.path().join("input.jsonl");
        let csv_pts_out = dir.path().join("from_csv.xml");
        let jsonl_pts_out = dir.path().join("from_jsonl.xml");

        {
            let mut csv_rec = TelemetryRecorder::open(RecordConfig {
                path: csv_path.clone(),
                format: RecordFormat::Csv,
                append: false,
                max_file_bytes: None,
                max_rotated_files: 0,
                flush_every: 1,
                tag: Some("cinebench, r23".to_string()),
            })
            .unwrap();
            csv_rec
                .write_sample_at(&sample_snapshot(90.0, 70.0), 1_700_000_000_000)
                .unwrap();
            csv_rec
                .write_sample_at(&sample_snapshot(110.0, 80.0), 1_700_000_001_000)
                .unwrap();
        }

        {
            let mut jsonl_rec = TelemetryRecorder::open(RecordConfig {
                path: jsonl_path.clone(),
                format: RecordFormat::Jsonl,
                append: false,
                max_file_bytes: None,
                max_rotated_files: 0,
                flush_every: 1,
                tag: Some("cinebench, r23".to_string()),
            })
            .unwrap();
            jsonl_rec
                .write_sample_at(&sample_snapshot(90.0, 70.0), 1_700_000_000_000)
                .unwrap();
            jsonl_rec
                .write_sample_at(&sample_snapshot(110.0, 80.0), 1_700_000_001_000)
                .unwrap();
        }

        let csv_count = convert_jsonl_or_csv_to_pts(
            &csv_path,
            &csv_pts_out,
            Some("R23 & Thermal <Test>"),
            Some("9700x-co-20"),
        )
        .unwrap();
        let jsonl_count = convert_jsonl_or_csv_to_pts(
            &jsonl_path,
            &jsonl_pts_out,
            Some("R23 & Thermal <Test>"),
            Some("9700x-co-20"),
        )
        .unwrap();

        assert_eq!(csv_count, 2);
        assert_eq!(jsonl_count, 2);

        let csv_xml = fs::read_to_string(&csv_pts_out).unwrap();
        let jsonl_xml = fs::read_to_string(&jsonl_pts_out).unwrap();

        for xml in [&csv_xml, &jsonl_xml] {
            assert!(xml.contains("<PhoronixTestSuite>"));
            assert!(xml.contains("<Title>R23 &amp; Thermal &lt;Test&gt;</Title>"));
            assert!(xml.contains("<Identifier>9700x-co-20</Identifier>"));
            assert!(xml.contains("<Arguments>cinebench, r23</Arguments>"));
            assert!(xml.contains("<Value>100.00</Value>"));
            assert!(xml.contains("<RawString>90.00:110.00</RawString>"));
            assert!(xml.contains("<Value>75.00</Value>"));
            assert!(xml.contains("<RawString>70.00:80.00</RawString>"));
        }
    }
}
