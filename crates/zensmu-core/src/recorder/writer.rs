use super::csv::{format_csv_row, CSV_HEADER};
use super::paths::{derive_pts_label_from_path, rotated_path};
use super::pts::render_pts_composite_xml;
use super::time::{format_iso8601_utc_ms, now_unix_ms};
use super::{RecordConfig, RecordFormat, RecordStats, RecordedSample};
use crate::error::{Error, Result};
use crate::ops::pm_table::TelemetrySnapshot;
use crate::safety::{map_io_err, sync_dir_best_effort, write_atomic};
use crate::types::Codename;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, ErrorKind, Write};
use std::path::Path;

/// Default maximum in-memory samples buffered for a single PTS `composite.xml` recording.
const MAX_PTS_IN_MEMORY_SAMPLES: usize = 200_000;

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
