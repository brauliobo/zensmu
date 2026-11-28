use super::mce::{diff_mce_lines, read_mce_counters};
use super::system::{
    find_hwmon_vcore_path, pin_current_thread_to_cpu, read_cpu_freq_mhz, read_hwmon_voltage_v,
    update_max, update_min,
};
use super::workload::{run_workload_batch, CACHE_BUFFER_WORDS};
use super::{
    validate_bench_config, BenchConfig, BenchProgress, BenchReport, BenchWorkload, CoreBenchResult,
    CoreBenchStatus,
};
use crate::error::{Error, Result};
use crate::hw::identity::HardwareContext;
use crate::ops::pm_table;
use crate::transport::SmuTransport;
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Responsive sleep slice for coordinator polling (milliseconds).
const POLL_SLICE_MS: u64 = 25;

enum WorkerOutcome {
    Completed,
    PinError(Error),
    ComputeError(String),
}

/// Execute the Per-Core Curve Optimizer stability tester / core-cycler.
///
/// Validates `config`, then iterates over `1..=config.passes` and each target
/// core. For each core, pins a dedicated stress worker thread to that core's
/// first Linux logical CPU via `sched_setaffinity`, samples telemetry at
/// `config.sample_interval_ms`, invokes `on_progress`, detects MCE/EDAC/dmesg
/// deltas before and after the core run, and stops promptly when `stop_flag` is
/// set or when `config.stop_on_error` triggers on a failure.
pub fn run_core_cycler<F>(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    config: &BenchConfig,
    stop_flag: &Arc<AtomicBool>,
    mut on_progress: F,
) -> Result<BenchReport>
where
    F: FnMut(&BenchProgress),
{
    let targets = validate_bench_config(config, hw)?;
    let suite_start = Instant::now();
    let vcore_path = find_hwmon_vcore_path();

    let total_expected = targets.len().saturating_mul(config.passes as usize);
    let mut results = Vec::with_capacity(total_expected);
    let mut failed_cores: Vec<u32> = Vec::new();
    let mut passes_completed = 0u32;
    let mut cancelled = false;
    let mut aborted_on_error = false;
    let mut any_dmesg_readable = false;

    let target_duration = Duration::from_secs(config.duration_per_core_secs);
    let sample_interval = Duration::from_millis(config.sample_interval_ms);
    let poll_slice = Duration::from_millis(POLL_SLICE_MS);
    let total_cores = targets.len();

    for pass in 1..=config.passes {
        if stop_flag.load(Ordering::Relaxed) {
            cancelled = true;
            break;
        }

        let mut pass_finished_all_cores = true;

        for (core_pos, &(core_index, logical_cpu)) in targets.iter().enumerate() {
            if stop_flag.load(Ordering::Relaxed) {
                cancelled = true;
                pass_finished_all_cores = false;
                break;
            }

            let mce_before = read_mce_counters();
            any_dmesg_readable |= mce_before.dmesg_readable;

            let core_stop = Arc::new(AtomicBool::new(false));
            let iterations = Arc::new(AtomicU64::new(0));
            let worker_done = Arc::new(AtomicBool::new(false));

            let w_core_stop = Arc::clone(&core_stop);
            let w_global_stop = Arc::clone(stop_flag);
            let w_iterations = Arc::clone(&iterations);
            let w_done = Arc::clone(&worker_done);
            let workload = config.workload;

            let worker_handle = thread::Builder::new()
                .name(format!("zensmu-bench-c{core_index}"))
                .spawn(move || {
                    if let Err(err) = pin_current_thread_to_cpu(logical_cpu) {
                        w_done.store(true, Ordering::Release);
                        return WorkerOutcome::PinError(err);
                    }

                    let mut cache_buf = match workload {
                        BenchWorkload::Cache | BenchWorkload::Mixed => {
                            vec![0u64; CACHE_BUFFER_WORDS]
                        }
                        BenchWorkload::Integer | BenchWorkload::FloatingPoint => Vec::new(),
                    };

                    let mut batch_idx = 0u64;
                    while !w_core_stop.load(Ordering::Relaxed)
                        && !w_global_stop.load(Ordering::Relaxed)
                    {
                        match run_workload_batch(workload, &mut cache_buf, batch_idx) {
                            Ok(digest) => {
                                black_box(digest);
                                batch_idx = batch_idx.wrapping_add(1);
                                w_iterations.fetch_add(1, Ordering::Relaxed);
                            }
                            Err(msg) => {
                                w_done.store(true, Ordering::Release);
                                return WorkerOutcome::ComputeError(msg);
                            }
                        }
                    }

                    w_done.store(true, Ordering::Release);
                    WorkerOutcome::Completed
                })
                .map_err(|source| Error::Io {
                    path: format!("spawn worker thread for core {core_index}"),
                    source,
                })?;

            let core_start = Instant::now();
            let mut peak_freq_mhz: Option<f32> = None;
            let mut peak_temp_c: Option<f32> = None;
            let mut peak_power_w: Option<f32> = None;
            let mut min_voltage_v: Option<f32> = None;
            let mut max_voltage_v: Option<f32> = None;

            loop {
                let latest_freq_mhz = read_cpu_freq_mhz(logical_cpu);
                if let Some(mhz) = latest_freq_mhz {
                    update_max(&mut peak_freq_mhz, mhz);
                }

                let mut latest_temp_c = None;
                if let Ok(snap) = pm_table::read_telemetry(transport, hw) {
                    if let Some(tctl) = snap.tctl_value_c {
                        latest_temp_c = Some(tctl);
                        update_max(&mut peak_temp_c, tctl);
                    }
                    if let Some(pwr) = snap.ppt_value_w.or(snap.stapm_value_w) {
                        update_max(&mut peak_power_w, pwr);
                    }
                }

                if let Some(volts) = vcore_path.as_deref().and_then(read_hwmon_voltage_v) {
                    update_min(&mut min_voltage_v, volts);
                    update_max(&mut max_voltage_v, volts);
                }

                let elapsed = core_start.elapsed();
                on_progress(&BenchProgress {
                    pass,
                    total_passes: config.passes,
                    core_index,
                    core_pos,
                    total_cores,
                    elapsed_secs: elapsed.as_secs_f64(),
                    target_secs: config.duration_per_core_secs,
                    latest_freq_mhz,
                    latest_temp_c,
                });

                if stop_flag.load(Ordering::Relaxed) {
                    cancelled = true;
                    break;
                }
                if worker_done.load(Ordering::Acquire) {
                    break;
                }
                if elapsed >= target_duration {
                    break;
                }

                let remaining_target = target_duration.saturating_sub(elapsed);
                let sleep_budget = remaining_target.min(sample_interval);
                let sleep_start = Instant::now();

                while sleep_start.elapsed() < sleep_budget {
                    if stop_flag.load(Ordering::Relaxed) || worker_done.load(Ordering::Acquire) {
                        break;
                    }
                    let left = sleep_budget.saturating_sub(sleep_start.elapsed());
                    if left.is_zero() {
                        break;
                    }
                    thread::sleep(left.min(poll_slice));
                }
            }

            core_stop.store(true, Ordering::Release);

            let worker_outcome = match worker_handle.join() {
                Ok(outcome) => outcome,
                Err(_) => {
                    WorkerOutcome::ComputeError("benchmark worker thread exited abnormally".into())
                }
            };

            if let WorkerOutcome::PinError(err) = worker_outcome {
                return Err(err);
            }

            let duration_secs = core_start.elapsed().as_secs_f64();
            let completed_iters = iterations.load(Ordering::Relaxed);

            let mce_after = read_mce_counters();
            any_dmesg_readable |= mce_after.dmesg_readable;

            let mce_delta_corrected = mce_after
                .corrected_errors
                .saturating_sub(mce_before.corrected_errors);
            let mce_delta_uncorrected = mce_after
                .uncorrected_errors
                .saturating_sub(mce_before.uncorrected_errors);
            let new_mce_lines =
                diff_mce_lines(&mce_before.dmesg_mce_lines, &mce_after.dmesg_mce_lines);
            let mce_failed =
                mce_delta_uncorrected > 0 || mce_delta_corrected > 0 || !new_mce_lines.is_empty();

            let (status, error) = if mce_failed {
                let mut detail = format!(
                    "hardware MCE detected on core {core_index} (CE +{mce_delta_corrected}, UE +{mce_delta_uncorrected}, {} new kernel log line(s))",
                    new_mce_lines.len()
                );
                if let WorkerOutcome::ComputeError(ref compute_msg) = worker_outcome {
                    detail.push_str("; ");
                    detail.push_str(compute_msg);
                }
                (CoreBenchStatus::FailedMce, Some(detail))
            } else if let WorkerOutcome::ComputeError(compute_msg) = worker_outcome {
                (CoreBenchStatus::FailedCompute, Some(compute_msg))
            } else if cancelled || stop_flag.load(Ordering::Relaxed) {
                cancelled = true;
                (CoreBenchStatus::Cancelled, None)
            } else {
                (CoreBenchStatus::Passed, None)
            };

            results.push(CoreBenchResult {
                pass,
                core_index,
                logical_cpu,
                status,
                duration_secs,
                iterations: completed_iters,
                peak_freq_mhz,
                peak_temp_c,
                peak_power_w,
                min_voltage_v,
                max_voltage_v,
                mce_delta_corrected,
                mce_delta_uncorrected,
                new_mce_lines,
                error,
            });

            if matches!(
                status,
                CoreBenchStatus::FailedMce | CoreBenchStatus::FailedCompute
            ) {
                if !failed_cores.contains(&core_index) {
                    failed_cores.push(core_index);
                }
                if config.stop_on_error {
                    aborted_on_error = true;
                    pass_finished_all_cores = false;
                    break;
                }
            }

            if cancelled {
                pass_finished_all_cores = false;
                break;
            }
        }

        if pass_finished_all_cores {
            passes_completed = pass;
        }
        if cancelled || aborted_on_error {
            break;
        }
    }

    let passed = !cancelled && failed_cores.is_empty() && !results.is_empty();

    Ok(BenchReport {
        codename: hw.codename.display_name().to_string(),
        workload: config.workload,
        passes_completed,
        total_duration_secs: suite_start.elapsed().as_secs_f64(),
        passed,
        cancelled,
        failed_cores,
        dmesg_readable: any_dmesg_readable,
        results,
    })
}
