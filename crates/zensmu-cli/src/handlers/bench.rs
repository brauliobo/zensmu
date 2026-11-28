//! Per-core CO stability tester (`test` / `bench`) handler.

use super::signal::{SignalStopBridge, INTERRUPTED};
use super::{probe_hw, state_dir, CliOutcome};
use crate::commands::{Cli, TestCommand};
use crate::doctor::is_effective_root;
use crate::output::{print_bench_report, print_json};
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::Ordering;
use zensmu_core::bench::{self, BenchConfig};
use zensmu_core::error::{Error, Result};
use zensmu_core::safety::bounds;
use zensmu_core::safety::snapshot::SnapshotStore;
use zensmu_core::session;
use zensmu_core::transport::SmuTransport;

// ============================================================================
// Per-Core CO Stability Tester / Core-Cycler (`test`, alias `bench`)
// ============================================================================

pub(super) fn parse_core_selection(spec: &str, max_cores: usize) -> Result<Vec<u32>> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput(
            "core selection specification cannot be empty".into(),
        ));
    }
    if max_cores == 0 {
        return Err(Error::InvalidInput(
            "no physical cores detected in CPU topology".into(),
        ));
    }

    let mut cores = Vec::new();
    for raw_item in trimmed.split(',') {
        let item = raw_item.trim();
        if item.is_empty() {
            return Err(Error::InvalidInput(format!(
                "empty item in core selection '{spec}'"
            )));
        }

        let range_parts = if let Some((a, b)) = item.split_once("..=") {
            Some((a, b))
        } else if let Some((a, b)) = item.split_once("..") {
            Some((a, b))
        } else {
            item.split_once('-')
        };

        if let Some((start_s, end_s)) = range_parts {
            let start = parse_core_selection_index(start_s, max_cores)?;
            let end = parse_core_selection_index(end_s, max_cores)?;
            if start > end {
                return Err(Error::InvalidInput(format!(
                    "invalid core range '{item}': start {start} > end {end}"
                )));
            }
            for idx in start..=end {
                if !cores.contains(&idx) {
                    cores.push(idx);
                }
            }
        } else {
            let idx = parse_core_selection_index(item, max_cores)?;
            if !cores.contains(&idx) {
                cores.push(idx);
            }
        }
    }

    if cores.is_empty() {
        return Err(Error::InvalidInput(format!(
            "no core indices found in '{spec}'"
        )));
    }

    Ok(cores)
}

pub(super) fn parse_core_selection_index(s: &str, max_cores: usize) -> Result<u32> {
    let trimmed = s.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::InvalidInput(format!(
            "invalid core index '{s}': expected non-negative integer"
        )));
    }
    let idx: u32 = trimmed
        .parse()
        .map_err(|_| Error::InvalidInput(format!("invalid core index '{s}': numeric overflow")))?;
    if (idx as usize) >= max_cores {
        return Err(Error::OutOfBounds {
            field: "core_index".into(),
            value: idx.to_string(),
            bounds: format!("0..{max_cores}"),
        });
    }
    Ok(idx)
}

pub(super) fn cmd_test(
    cli: &Cli,
    transport: &dyn SmuTransport,
    test: &TestCommand,
) -> Result<CliOutcome> {
    let duration_per_core_secs = bounds::parse_duration_secs(&test.duration)?;
    let sample_interval_ms = bounds::parse_duration_ms(&test.interval)?;
    let hw = probe_hw(cli, transport)?;

    let cores = match test.cores.as_deref() {
        Some(spec) => parse_core_selection(spec, hw.topology.physical_cores)?,
        None => Vec::new(),
    };

    let config = BenchConfig {
        cores,
        duration_per_core_secs,
        passes: test.passes,
        workload: test.workload.into(),
        sample_interval_ms,
        stop_on_error: !test.continue_on_error,
    };

    let targets = bench::validate_bench_config(&config, &hw)?;

    let is_root = is_effective_root();
    if test.rollback_on_fail && !is_root && !cli.json && !cli.quiet {
        eprintln!(
            "note: --rollback-on-fail requires root privileges (re-run with sudo to enable automatic rollback on failure)"
        );
    }

    let show_progress = !cli.json && !cli.quiet;
    let stderr_is_tty = io::stderr().is_terminal();
    if show_progress {
        eprintln!(
            "Testing {} ({} core(s), {} pass(es), {}s/core, workload={}) — Ctrl-C to stop...",
            hw.codename.display_name(),
            targets.len(),
            config.passes,
            config.duration_per_core_secs,
            config.workload
        );
    }

    let sig_bridge = SignalStopBridge::install();
    let stop_flag = sig_bridge.stop_flag();
    let mut saw_progress = false;
    let mut last_logged_core: Option<(u32, u32)> = None;

    let report = bench::run_core_cycler(transport, &hw, &config, stop_flag, |p| {
        if INTERRUPTED.load(Ordering::SeqCst) {
            stop_flag.store(true, Ordering::Release);
        }
        if !show_progress {
            return;
        }
        saw_progress = true;
        let freq = p
            .latest_freq_mhz
            .map(|m| format!("{m:.0} MHz"))
            .unwrap_or_else(|| "n/a".into());
        let temp = p
            .latest_temp_c
            .map(|t| format!("{t:.1}°C"))
            .unwrap_or_else(|| "n/a".into());
        if stderr_is_tty {
            eprint!(
                "\rPass {}/{} | Core {} ({}/{}) | {:>5.1}/{}s | {} | {}   ",
                p.pass,
                p.total_passes,
                p.core_index,
                p.core_pos + 1,
                p.total_cores,
                p.elapsed_secs,
                p.target_secs,
                freq,
                temp
            );
            let _ = io::stderr().flush();
        } else if last_logged_core != Some((p.pass, p.core_index)) {
            last_logged_core = Some((p.pass, p.core_index));
            eprintln!(
                "Pass {}/{} | Core {} ({}/{}) | target {}s | {} | {}",
                p.pass,
                p.total_passes,
                p.core_index,
                p.core_pos + 1,
                p.total_cores,
                p.target_secs,
                freq,
                temp
            );
        }
    })?;

    drop(sig_bridge);

    if show_progress && saw_progress && stderr_is_tty {
        eprintln!();
    }

    let mut rollback_note: Option<String> = None;
    if test.rollback_on_fail && !report.failed_cores.is_empty() {
        if !is_root {
            rollback_note = Some(
                "--rollback-on-fail skipped: requires root privileges (re-run with sudo)".into(),
            );
        } else {
            let store = SnapshotStore::new(state_dir(cli));
            let path = store.last_state_path();
            match store.load_last_state() {
                Ok(Some(snap)) => {
                    match session::apply_snapshot(transport, &hw, &snap, true, cli.dry_run) {
                        Ok(ops) => {
                            let prefix = if cli.dry_run {
                                "[DRY-RUN] would roll back"
                            } else {
                                "automatically rolled back"
                            };
                            rollback_note = Some(format!(
                                "{prefix} SMU state from {} ({} operation(s))",
                                path.display(),
                                ops.len()
                            ));
                        }
                        Err(err) => {
                            rollback_note = Some(format!(
                                "automatic rollback from {} failed: {err}",
                                path.display()
                            ));
                        }
                    }
                }
                Ok(None) => {
                    rollback_note = Some(format!(
                        "--rollback-on-fail skipped: no last-state snapshot found at {}",
                        path.display()
                    ));
                }
                Err(err) => {
                    rollback_note = Some(format!(
                        "--rollback-on-fail failed to load {}: {err}",
                        path.display()
                    ));
                }
            }
        }
    }

    if cli.json {
        print_json(&report);
        if let Some(ref note) = rollback_note {
            if !cli.quiet {
                eprintln!("rollback: {note}");
            }
        }
    } else {
        print_bench_report(&report);
        if let Some(ref note) = rollback_note {
            println!();
            println!("Rollback : {note}");
        }
    }

    if report.passed {
        Ok(CliOutcome::Success)
    } else {
        Ok(CliOutcome::ExitWithCode(1))
    }
}
