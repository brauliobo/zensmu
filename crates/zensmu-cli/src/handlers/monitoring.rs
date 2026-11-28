//! Telemetry, live TUI, telemetry recorder, and Prometheus exporter handlers.

use super::signal::{
    install_sigint_handler, install_stop_signal_handlers, SignalStopBridge, INTERRUPTED,
};
use super::{probe_hw, CliOutcome};
use crate::commands::{
    Cli, ExportAction, ExportCommand, ExportServiceAction, RecordAction, RecordCommand,
    RecordFormatArg, RecordServiceAction, TelemetryAction,
};
use crate::output::{
    print_exporter_service_action, print_exporter_service_status, print_json, print_json_compact,
    print_record_export_pts, print_record_service_action, print_record_service_status,
    print_record_stats, print_telemetry, ExporterServiceActionOutput, RecordExportPtsOutput,
    RecordServiceActionOutput, TelemetryDumpOutput,
};
use crate::tui;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::{Duration, Instant};
use zensmu_core::error::{Error, Result};
use zensmu_core::metrics::{self, MetricsServerConfig};
use zensmu_core::ops::pm_table;
use zensmu_core::recorder::{self, RecordConfig, RecordFormat, TelemetryRecorder};
use zensmu_core::safety::bounds;
use zensmu_core::transport::SmuTransport;

// ============================================================================
// Telemetry & Live TUI (`telemetry`, `top`)
// ============================================================================

pub(super) fn cmd_telemetry(
    cli: &Cli,
    transport: &dyn SmuTransport,
    action: Option<&TelemetryAction>,
) -> Result<CliOutcome> {
    match action {
        None => {
            let hw = probe_hw(cli, transport)?;
            let snap = pm_table::read_telemetry(transport, &hw)?;
            if cli.json {
                print_json(&snap);
            } else {
                print_telemetry(&snap);
            }
            Ok(CliOutcome::Success)
        }
        Some(TelemetryAction::Watch { interval }) => {
            let interval_ms = bounds::parse_duration_ms(interval)?;
            let hw = probe_hw(cli, transport)?;
            let is_tty = io::stdout().is_terminal();

            install_sigint_handler();
            while !INTERRUPTED.load(Ordering::SeqCst) {
                let snap = pm_table::read_telemetry(transport, &hw)?;
                if cli.json {
                    print_json_compact(&snap);
                    let _ = io::stdout().flush();
                } else {
                    if is_tty {
                        print!("\x1b[2J\x1b[H");
                    } else {
                        println!("---");
                    }
                    print_telemetry(&snap);
                    if is_tty {
                        println!();
                        println!("Refreshing every {interval_ms} ms (Ctrl-C to exit)...");
                    }
                    let _ = io::stdout().flush();
                }

                let target = Instant::now() + Duration::from_millis(interval_ms);
                while Instant::now() < target && !INTERRUPTED.load(Ordering::SeqCst) {
                    let rem = target.saturating_duration_since(Instant::now());
                    thread::sleep(rem.min(Duration::from_millis(50)));
                }
            }
            Ok(CliOutcome::Success)
        }
        Some(TelemetryAction::Dump { out_path }) => {
            let info = transport.read_info()?;
            let bytes = pm_table::dump_raw_pm_table(transport, out_path)?;
            let out = TelemetryDumpOutput {
                path: out_path.display().to_string(),
                bytes,
                table_version: info.pm_table_version,
            };
            if cli.json {
                print_json(&out);
            } else {
                println!(
                    "Dumped {bytes} raw PM table bytes to {} (version: {})",
                    out.path,
                    out.table_version
                        .map(|v| format!("0x{v:08X}"))
                        .unwrap_or_else(|| "unknown".into())
                );
            }
            Ok(CliOutcome::Success)
        }
    }
}

pub(super) fn cmd_top(
    cli: &Cli,
    transport: &dyn SmuTransport,
    interval: &str,
) -> Result<CliOutcome> {
    let interval_ms = bounds::parse_duration_ms(interval)?;
    let hw = probe_hw(cli, transport)?;
    tui::run_tui(transport, &hw, interval_ms)?;
    Ok(CliOutcome::Success)
}

// ============================================================================
// Telemetry Recording (`record`) — Zero Root Required
// ============================================================================

pub(super) fn resolved_pts_result_name(output: &Path, requested: &str) -> String {
    output
        .parent()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| requested.to_string())
}

pub(super) fn emit_record_service_action(
    cli: &Cli,
    action: &str,
    dry_run: bool,
    status: recorder::RecordServiceStatus,
    note: String,
) {
    let out = RecordServiceActionOutput {
        action: action.to_string(),
        dry_run,
        status,
        note: Some(note),
    };
    if cli.json {
        print_json(&out);
    } else if dry_run || !cli.quiet {
        print_record_service_action(&out);
    }
}

pub(super) fn cmd_record(
    cli: &Cli,
    transport: &dyn SmuTransport,
    rec: &RecordCommand,
) -> Result<CliOutcome> {
    match &rec.action {
        Some(RecordAction::Service { action }) => match action {
            RecordServiceAction::Status => {
                let status = recorder::read_record_service_status();
                if cli.json {
                    print_json(&status);
                } else {
                    print_record_service_status(&status);
                }
                Ok(CliOutcome::Success)
            }
            RecordServiceAction::Enable => {
                if cli.dry_run {
                    let status = recorder::read_record_service_status();
                    emit_record_service_action(
                        cli,
                        "record service enable",
                        true,
                        status,
                        format!(
                            "would run `systemctl --user enable --now {}`",
                            recorder::USER_RECORD_SERVICE_NAME
                        ),
                    );
                    return Ok(CliOutcome::Success);
                }
                let status = recorder::set_record_service_enabled(true)?;
                emit_record_service_action(
                    cli,
                    "record service enable",
                    false,
                    status,
                    format!("enabled and started {}", recorder::USER_RECORD_SERVICE_NAME),
                );
                Ok(CliOutcome::Success)
            }
            RecordServiceAction::Disable => {
                if cli.dry_run {
                    let status = recorder::read_record_service_status();
                    emit_record_service_action(
                        cli,
                        "record service disable",
                        true,
                        status,
                        format!(
                            "would run `systemctl --user disable --now {}`",
                            recorder::USER_RECORD_SERVICE_NAME
                        ),
                    );
                    return Ok(CliOutcome::Success);
                }
                let status = recorder::set_record_service_enabled(false)?;
                emit_record_service_action(
                    cli,
                    "record service disable",
                    false,
                    status,
                    format!(
                        "disabled and stopped {}",
                        recorder::USER_RECORD_SERVICE_NAME
                    ),
                );
                Ok(CliOutcome::Success)
            }
        },
        Some(RecordAction::ExportPts {
            input,
            output,
            pts_name,
            title,
            identifier,
        }) => {
            if output.is_some() && pts_name.is_some() {
                return Err(Error::InvalidInput(
                    "cannot combine --output and --pts-name".into(),
                ));
            }
            let output_path = if let Some(name) = pts_name {
                recorder::pts_result_path(name)
            } else if let Some(path) = output {
                path.clone()
            } else {
                let stem = input
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("telemetry");
                input.with_file_name(format!("{stem}-pts.xml"))
            };

            let samples = recorder::convert_jsonl_or_csv_to_pts(
                input,
                &output_path,
                title.as_deref(),
                identifier.as_deref(),
            )?;

            let resolved_pts = pts_name
                .as_deref()
                .map(|name| resolved_pts_result_name(&output_path, name));
            let out = RecordExportPtsOutput {
                input: input.display().to_string(),
                output: output_path.display().to_string(),
                samples,
                pts_name: resolved_pts,
            };
            if cli.json {
                print_json(&out);
            } else if !cli.quiet {
                print_record_export_pts(&out);
            }
            Ok(CliOutcome::Success)
        }
        None => cmd_record_live(cli, transport, rec),
    }
}

pub(super) fn cmd_record_live(
    cli: &Cli,
    transport: &dyn SmuTransport,
    rec: &RecordCommand,
) -> Result<CliOutcome> {
    if rec.pts_name.is_some() && rec.output.is_some() {
        return Err(Error::InvalidInput(
            "cannot combine --output and --pts-name".into(),
        ));
    }
    if rec.pts_name.is_some() && rec.format.is_some_and(|f| f != RecordFormatArg::Pts) {
        return Err(Error::InvalidInput(
            "--pts-name can only be used with --format pts".into(),
        ));
    }

    let interval_ms = bounds::parse_duration_ms(&rec.interval)?.max(100);
    let duration_ms = rec
        .duration
        .as_deref()
        .map(bounds::parse_duration_ms)
        .transpose()?;

    if rec.samples == Some(0) {
        return Err(Error::InvalidInput(
            "--samples must be greater than zero".into(),
        ));
    }

    let max_file_bytes =
        match rec.max_size {
            Some(0) => {
                return Err(Error::InvalidInput(
                    "--max-size must be greater than zero (in MiB)".into(),
                ));
            }
            Some(mb) => Some(mb.checked_mul(1024 * 1024).ok_or_else(|| {
                Error::InvalidInput("--max-size value overflows byte count".into())
            })?),
            None => None,
        };

    let hw = probe_hw(cli, transport)?;

    let (path, format, resolved_pts_name) = if let Some(ref pts_name) = rec.pts_name {
        let p = recorder::pts_result_path(pts_name);
        let resolved = resolved_pts_result_name(&p, pts_name);
        (p, RecordFormat::Pts, Some(resolved))
    } else if let Some(ref out_path) = rec.output {
        let fmt = rec
            .format
            .map(Into::into)
            .unwrap_or_else(|| RecordFormat::from_path(out_path));
        (out_path.clone(), fmt, None)
    } else if rec.append {
        let fmt = rec.format.map(Into::into).unwrap_or(RecordFormat::Csv);
        let default_daemon = recorder::default_daemon_recording_path();
        let p = if fmt == RecordFormat::Csv {
            default_daemon
        } else {
            default_daemon.with_extension(fmt.extension())
        };
        (p, fmt, None)
    } else {
        let fmt = rec.format.map(Into::into).unwrap_or(RecordFormat::Csv);
        (
            recorder::default_recording_path(hw.codename, fmt),
            fmt,
            None,
        )
    };

    let config = RecordConfig {
        path,
        format,
        append: rec.append,
        max_file_bytes,
        max_rotated_files: rec.rotate,
        flush_every: 1,
        tag: rec.tag.clone(),
    };

    let mut recorder = TelemetryRecorder::open(config)?;
    install_stop_signal_handlers();

    if !cli.json && !cli.quiet {
        let mut details = vec![
            format!("format={}", recorder.format().as_str()),
            format!("interval={interval_ms}ms"),
        ];
        if let Some(mb) = rec.max_size {
            details.push(format!("max-size={mb}MiB"));
            details.push(format!("rotate={}", rec.rotate));
        }
        if let Some(ref dur) = rec.duration {
            details.push(format!("duration={dur}"));
        }
        if let Some(n) = rec.samples {
            details.push(format!("samples={n}"));
        }
        if let Some(ref tag) = rec.tag {
            details.push(format!("tag={tag}"));
        }
        println!(
            "Recording {} telemetry to {} ({}) — press Ctrl-C to stop...",
            hw.codename.display_name(),
            recorder.path().display(),
            details.join(", ")
        );
        let _ = io::stdout().flush();
    }

    let started = Instant::now();
    let deadline = duration_ms.map(|ms| started + Duration::from_millis(ms));
    let interval = Duration::from_millis(interval_ms);

    while !INTERRUPTED.load(Ordering::SeqCst) {
        let snap = pm_table::read_telemetry(transport, &hw)?;
        recorder.write_snapshot(&snap)?;

        if let Some(max_samples) = rec.samples {
            if recorder.stats().samples_written >= max_samples {
                break;
            }
        }
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                break;
            }
        }

        let next_tick = Instant::now() + interval;
        let sleep_until = match deadline {
            Some(dl) => next_tick.min(dl),
            None => next_tick,
        };
        while Instant::now() < sleep_until && !INTERRUPTED.load(Ordering::SeqCst) {
            let rem = sleep_until.saturating_duration_since(Instant::now());
            thread::sleep(rem.min(Duration::from_millis(50)));
        }
        if let Some(dl) = deadline {
            if Instant::now() >= dl {
                break;
            }
        }
    }

    recorder.flush()?;
    let stats = recorder.stats();

    if cli.json {
        print_json(&stats);
    } else if !cli.quiet {
        print_record_stats(&stats, resolved_pts_name.as_deref());
    }

    Ok(CliOutcome::Success)
}

// ============================================================================
// Prometheus / OpenMetrics Telemetry Exporter (`export`) — Zero Root Required
// ============================================================================

pub(super) fn emit_exporter_service_action(
    cli: &Cli,
    action: &str,
    dry_run: bool,
    status: metrics::ExporterServiceStatus,
    note: String,
) {
    let out = ExporterServiceActionOutput {
        action: action.to_string(),
        dry_run,
        status,
        note: Some(note),
    };
    if cli.json {
        print_json(&out);
    } else if dry_run || !cli.quiet {
        print_exporter_service_action(&out);
    }
}

pub(super) fn cmd_export(
    cli: &Cli,
    transport: &dyn SmuTransport,
    export: &ExportCommand,
) -> Result<CliOutcome> {
    match &export.action {
        Some(ExportAction::Service { action }) => match action {
            ExportServiceAction::Status => {
                let status = metrics::read_exporter_service_status();
                if cli.json {
                    print_json(&status);
                } else {
                    print_exporter_service_status(&status);
                }
                Ok(CliOutcome::Success)
            }
            ExportServiceAction::Enable => {
                if cli.dry_run {
                    let status = metrics::read_exporter_service_status();
                    emit_exporter_service_action(
                        cli,
                        "export service enable",
                        true,
                        status,
                        format!(
                            "would run `systemctl --user enable --now {}`",
                            metrics::USER_EXPORTER_SERVICE_NAME
                        ),
                    );
                    return Ok(CliOutcome::Success);
                }
                let status = metrics::set_exporter_service_enabled(true)?;
                emit_exporter_service_action(
                    cli,
                    "export service enable",
                    false,
                    status,
                    format!(
                        "enabled and started {}",
                        metrics::USER_EXPORTER_SERVICE_NAME
                    ),
                );
                Ok(CliOutcome::Success)
            }
            ExportServiceAction::Disable => {
                if cli.dry_run {
                    let status = metrics::read_exporter_service_status();
                    emit_exporter_service_action(
                        cli,
                        "export service disable",
                        true,
                        status,
                        format!(
                            "would run `systemctl --user disable --now {}`",
                            metrics::USER_EXPORTER_SERVICE_NAME
                        ),
                    );
                    return Ok(CliOutcome::Success);
                }
                let status = metrics::set_exporter_service_enabled(false)?;
                emit_exporter_service_action(
                    cli,
                    "export service disable",
                    false,
                    status,
                    format!(
                        "disabled and stopped {}",
                        metrics::USER_EXPORTER_SERVICE_NAME
                    ),
                );
                Ok(CliOutcome::Success)
            }
        },
        None => {
            let hw = probe_hw(cli, transport)?;
            let cfg = MetricsServerConfig {
                listen_addr: export.listen.clone(),
                include_per_core: !export.no_per_core,
            };
            let sig_bridge = SignalStopBridge::install();
            metrics::serve_metrics(transport, &hw, &cfg, sig_bridge.stop_flag(), |addr| {
                if !cli.json && !cli.quiet {
                    eprintln!("Listening on http://{addr}/metrics (Ctrl-C to stop)");
                }
            })?;
            Ok(CliOutcome::Success)
        }
    }
}
