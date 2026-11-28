//! Unprivileged telemetry poller, recorder controls, user daemons, and Core-Cycler runner.

use super::state::{
    BenchUiMsg, TelemetryMsg, TelemetrySample, ToastLevel, TELEMETRY_RING_CAPACITY,
};
use super::ZensmuApp;
use eframe::egui;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use zensmu_core::bench::{run_core_cycler, BenchConfig};
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::metrics::{
    read_exporter_service_status, set_exporter_service_enabled, USER_EXPORTER_SERVICE_NAME,
};
use zensmu_core::ops::pm_table;
use zensmu_core::recorder::{
    default_recordings_dir, read_record_service_status, set_record_service_enabled,
    USER_RECORD_SERVICE_NAME,
};
use zensmu_core::transport::sysfs::SysfsTransport;
use zensmu_core::types::Codename;

impl ZensmuApp {
    /// Start unprivileged live telemetry recording to the configured format and path.
    pub fn start_recording(&mut self) {
        if self.recorder.is_recording() {
            return;
        }
        let codename = self
            .hw
            .as_ref()
            .map(|h| h.codename)
            .unwrap_or(Codename::Undefined);
        let path = self.recorder.resolve_output_path(codename);
        let fmt_label = self.recorder.format.as_str().to_ascii_uppercase();
        match self.recorder.start_with_path(path) {
            Ok(out_path) => {
                self.set_toast(
                    ToastLevel::Success,
                    format!("Recording {fmt_label} telemetry to {}", out_path.display()),
                );
            }
            Err(err) => {
                self.set_toast(
                    ToastLevel::Error,
                    format!("Failed to start telemetry recording: {err}"),
                );
            }
        }
    }

    /// Flush and stop the active telemetry recording, saving last-file summary metadata.
    pub fn stop_recording(&mut self) {
        match self.recorder.stop() {
            Ok(Some((path, samples))) => {
                self.set_toast(
                    ToastLevel::Success,
                    format!("Saved {samples} samples to {}", path.display()),
                );
            }
            Ok(None) => {}
            Err(err) => {
                self.set_toast(
                    ToastLevel::Error,
                    format!("Recording stopped with flush error: {err}"),
                );
            }
        }
    }

    /// Toggle live telemetry recording on or off.
    pub fn toggle_recording(&mut self) {
        if self.recorder.is_recording() {
            self.stop_recording();
        } else {
            self.start_recording();
        }
    }

    /// Enable or disable the unprivileged user-level `zensmu-record.service` daemon (no `pkexec`).
    pub fn toggle_record_daemon(&mut self) {
        let target_enable = !self.recorder.daemon_status.enabled;
        match set_record_service_enabled(target_enable) {
            Ok(status) => {
                self.recorder.daemon_status = status;
                let msg = if target_enable {
                    format!("Enabled background telemetry daemon ({USER_RECORD_SERVICE_NAME}).")
                } else {
                    format!("Disabled background telemetry daemon ({USER_RECORD_SERVICE_NAME}).")
                };
                self.set_toast(ToastLevel::Info, msg);
            }
            Err(err) => {
                self.recorder.daemon_status = read_record_service_status();
                self.set_toast(
                    ToastLevel::Error,
                    format!("Background recorder toggle failed: {err}"),
                );
            }
        }
    }

    /// Refresh unprivileged `zensmu-record.service` status via `systemctl --user`.
    pub fn refresh_record_daemon_status(&mut self) {
        self.recorder.daemon_status = read_record_service_status();
    }

    /// Enable or disable the unprivileged user-level `zensmu-exporter.service` daemon (no `pkexec`).
    pub fn toggle_exporter_daemon(&mut self) {
        let target_enable = !self.exporter_status.enabled;
        match set_exporter_service_enabled(target_enable) {
            Ok(status) => {
                self.exporter_status = status;
                let msg = if target_enable {
                    format!("Enabled Prometheus exporter daemon ({USER_EXPORTER_SERVICE_NAME}).")
                } else {
                    format!("Disabled Prometheus exporter daemon ({USER_EXPORTER_SERVICE_NAME}).")
                };
                self.set_toast(ToastLevel::Info, msg);
            }
            Err(err) => {
                self.exporter_status = read_exporter_service_status();
                self.set_toast(
                    ToastLevel::Error,
                    format!("Prometheus exporter toggle failed: {err}"),
                );
            }
        }
    }

    /// Refresh unprivileged `zensmu-exporter.service` status via `systemctl --user`.
    pub fn refresh_exporter_daemon_status(&mut self) {
        self.exporter_status = read_exporter_service_status();
    }

    /// Start the unprivileged per-core Curve Optimizer stability tester / Core-Cycler on a background thread.
    pub fn start_bench(&mut self, ctx: &egui::Context) {
        if self.bench.running {
            return;
        }
        let Some(hw) = self.hw.clone() else {
            let msg = "Cannot start Core-Cycler: no Ryzen SMU hardware detected.".to_string();
            self.bench.error = Some(msg.clone());
            self.set_toast(ToastLevel::Warning, msg);
            return;
        };

        let cfg = BenchConfig {
            cores: Vec::new(),
            duration_per_core_secs: self.bench.duration_per_core_secs,
            passes: self.bench.passes,
            workload: self.bench.workload,
            sample_interval_ms: 500,
            stop_on_error: self.bench.stop_on_error,
        };

        let stop_flag = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let stop_bg = Arc::clone(&stop_flag);
        let ctx_bg = ctx.clone();

        let spawn_res = thread::Builder::new()
            .name("zensmu-gui-bench".into())
            .spawn(move || {
                let transport = SysfsTransport::default();
                let tx_prog = tx.clone();
                let ctx_prog = ctx_bg.clone();
                let res = run_core_cycler(&transport, &hw, &cfg, &stop_bg, |progress| {
                    let _ = tx_prog.send(BenchUiMsg::Progress(progress.clone()));
                    ctx_prog.request_repaint();
                })
                .map_err(|e| e.to_string());
                let _ = tx.send(BenchUiMsg::Finished(res));
                ctx_bg.request_repaint();
            });

        match spawn_res {
            Ok(_) => {
                self.bench.running = true;
                self.bench.progress = None;
                self.bench.last_report = None;
                self.bench.error = None;
                self.bench.stop_flag = stop_flag;
                self.bench.rx = Some(rx);
                self.set_toast(
                    ToastLevel::Info,
                    format!(
                        "Started unprivileged Core-Cycler ({}, {}s/core, {} pass{}).",
                        self.bench.workload,
                        self.bench.duration_per_core_secs,
                        self.bench.passes,
                        if self.bench.passes == 1 { "" } else { "es" }
                    ),
                );
            }
            Err(err) => {
                let msg = format!("Failed to start Core-Cycler thread: {err}");
                self.bench.running = false;
                self.bench.rx = None;
                self.bench.error = Some(msg.clone());
                self.set_toast(ToastLevel::Error, msg);
            }
        }
    }

    /// Signal the running Core-Cycler background thread to stop cleanly.
    pub fn stop_bench(&mut self) {
        if self.bench.running {
            self.bench.stop_flag.store(true, Ordering::Relaxed);
            self.set_toast(ToastLevel::Info, "Stopping Core-Cycler stability test…");
        }
    }

    /// Drain progress and completion messages from the Core-Cycler background thread.
    pub fn drain_bench(&mut self) {
        let mut drained = Vec::new();
        let mut disconnected = false;
        if let Some(rx) = &self.bench.rx {
            loop {
                match rx.try_recv() {
                    Ok(msg) => drained.push(msg),
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        for msg in drained {
            match msg {
                BenchUiMsg::Progress(progress) => {
                    self.bench.progress = Some(progress);
                }
                BenchUiMsg::Finished(Ok(report)) => {
                    self.bench.running = false;
                    self.bench.progress = None;
                    self.bench.rx = None;
                    self.bench.error = None;
                    let (level, toast) = if report.cancelled {
                        (
                            ToastLevel::Warning,
                            format!(
                                "Core-Cycler stopped ({} core result(s) recorded).",
                                report.results.len()
                            ),
                        )
                    } else if report.passed {
                        (
                            ToastLevel::Success,
                            format!(
                                "Core-Cycler PASSED ({} pass(es), {:.1}s total).",
                                report.passes_completed, report.total_duration_secs
                            ),
                        )
                    } else {
                        let failed = report
                            .failed_cores
                            .iter()
                            .map(|c| format!("C{c}"))
                            .collect::<Vec<_>>()
                            .join(", ");
                        (
                            ToastLevel::Error,
                            format!("Core-Cycler FAILED on {failed} — relax CO margin."),
                        )
                    };
                    self.bench.last_report = Some(report);
                    self.set_toast(level, toast);
                }
                BenchUiMsg::Finished(Err(err)) => {
                    self.bench.running = false;
                    self.bench.progress = None;
                    self.bench.rx = None;
                    self.bench.error = Some(err.clone());
                    self.set_toast(ToastLevel::Error, format!("Core-Cycler error: {err}"));
                }
            }
        }

        if disconnected && self.bench.running {
            self.bench.running = false;
            self.bench.progress = None;
            self.bench.rx = None;
            let msg = "Core-Cycler worker exited unexpectedly.".to_string();
            self.bench.error = Some(msg.clone());
            self.set_toast(ToastLevel::Error, msg);
        }
    }

    /// Ensure the default telemetry recordings folder exists and open it via `xdg-open` (no shell).
    pub fn open_recordings_folder(&self) -> Result<PathBuf, String> {
        let dir = default_recordings_dir();
        fs::create_dir_all(&dir).map_err(|e| {
            format!(
                "Failed to create recordings directory '{}': {e}",
                dir.display()
            )
        })?;
        Command::new("xdg-open")
            .arg(&dir)
            .spawn()
            .map(|_| dir.clone())
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    format!(
                        "Could not find `xdg-open`. Recordings are stored in '{}'.",
                        dir.display()
                    )
                } else {
                    format!("Failed to open '{}': {e}", dir.display())
                }
            })
    }

    pub(super) fn stop_telemetry_thread(&mut self) {
        self.telemetry_stop.store(true, Ordering::Relaxed);
        self.telemetry_rx = None;
    }

    pub(super) fn start_telemetry_thread(&mut self, hw: HardwareContext, ctx: egui::Context) {
        let stop = Arc::new(AtomicBool::new(false));
        let pause = Arc::new(AtomicBool::new(self.telemetry_paused));
        let interval_ms = Arc::new(AtomicU64::new(self.poll_interval_ms));
        let (tx, rx) = mpsc::channel();
        let start_time = self.start_time;

        self.telemetry_stop = Arc::clone(&stop);
        self.telemetry_pause_flag = Arc::clone(&pause);
        self.telemetry_interval_flag = Arc::clone(&interval_ms);
        self.telemetry_rx = Some(rx);

        thread::spawn(move || {
            let transport = SysfsTransport::default();
            while !stop.load(Ordering::Relaxed) {
                if !pause.load(Ordering::Relaxed) {
                    let msg = match pm_table::read_telemetry(&transport, &hw) {
                        Ok(snapshot) => TelemetryMsg::Sample(Box::new(TelemetrySample {
                            elapsed_s: start_time.elapsed().as_secs_f64(),
                            snapshot,
                        })),
                        Err(e) => TelemetryMsg::Error {
                            suggests_report: e.suggests_report(),
                            hint: e.report_hint(),
                            message: e.to_string(),
                        },
                    };
                    if tx.send(msg).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }

                let sleep_ms = interval_ms.load(Ordering::Relaxed).clamp(100, 10_000);
                let mut slept = 0u64;
                while slept < sleep_ms && !stop.load(Ordering::Relaxed) {
                    let step = (sleep_ms - slept).min(50);
                    thread::sleep(Duration::from_millis(step));
                    slept += step;
                }
            }
        });
    }

    pub fn set_telemetry_paused(&mut self, paused: bool) {
        self.telemetry_paused = paused;
        self.telemetry_pause_flag.store(paused, Ordering::Relaxed);
    }

    pub fn set_poll_interval_ms(&mut self, ms: u64) {
        self.poll_interval_ms = ms;
        self.telemetry_interval_flag.store(ms, Ordering::Relaxed);
    }

    pub(super) fn drain_telemetry(&mut self) {
        let mut drained = Vec::new();
        if let Some(rx) = &self.telemetry_rx {
            while let Ok(msg) = rx.try_recv() {
                drained.push(msg);
            }
        }

        for msg in drained {
            match msg {
                TelemetryMsg::Sample(sample) => {
                    let sample = *sample;
                    if let Err(err) = self.recorder.write_snapshot(&sample.snapshot) {
                        let _ = self.recorder.stop();
                        self.set_toast(
                            ToastLevel::Error,
                            format!("Telemetry recording stopped due to write error: {err}"),
                        );
                    }
                    self.pbo_editor.sync_from_telemetry(&sample.snapshot);
                    self.apu_editor.sync_from_telemetry(&sample.snapshot);
                    self.latest_telemetry = Some(sample.snapshot.clone());
                    self.telemetry_error = None;
                    self.telemetry_error_suggests_report = false;
                    self.telemetry_error_hint = None;
                    if self.samples.len() >= TELEMETRY_RING_CAPACITY {
                        self.samples.pop_front();
                    }
                    self.samples.push_back(sample);
                }
                TelemetryMsg::Error {
                    message,
                    suggests_report,
                    hint,
                } => {
                    self.telemetry_error = Some(message);
                    self.telemetry_error_suggests_report = suggests_report;
                    self.telemetry_error_hint = hint;
                }
            }
        }
    }
}
