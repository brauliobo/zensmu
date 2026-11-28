use super::mce::read_mce_sysfs_counters;
use super::workload::{run_workload_batch, CACHE_BUFFER_WORDS};
use super::*;
use crate::hw::profiles::capabilities_for;
use crate::hw::topology::CpuTopology;
use crate::transport::mock::MockTransport;
use crate::transport::SmuTransport;
use crate::types::{Codename, CoreInfo};
use std::fs;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Return the first Linux logical CPU allowed for the current process so
/// affinity tests succeed even inside constrained cgroups/taskset runners.
#[cfg(target_os = "linux")]
fn first_allowed_logical_cpu() -> u32 {
    // SAFETY: `cpu_set_t` is zero-initialized and populated by `sched_getaffinity`
    // for the calling thread (`pid = 0`), then queried for indices `< CPU_SETSIZE`.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) == 0 {
            let max_cpus = usize::try_from(libc::CPU_SETSIZE).unwrap_or(1024);
            for cpu in 0..max_cpus {
                if libc::CPU_ISSET(cpu, &set) {
                    return cpu as u32;
                }
            }
        }
    }
    0
}

#[cfg(not(target_os = "linux"))]
fn first_allowed_logical_cpu() -> u32 {
    0
}

fn mock_hw_9700x(transport: &MockTransport, first_cpu: u32) -> HardwareContext {
    let driver = transport.read_info().unwrap();
    let mut topology = CpuTopology::synthetic(1, 8, 2);
    if let Some(c0) = topology.cores.first_mut() {
        c0.linux_cpus = vec![first_cpu];
    }
    HardwareContext {
        driver,
        codename: Codename::GraniteRidge,
        cpuid: None,
        topology,
        capabilities: capabilities_for(Codename::GraniteRidge).unwrap(),
    }
}

#[test]
fn extract_mce_lines_filters_relevant_kernel_messages() {
    let log = "\
[Thu Nov 26 20:10:01 2026] usb 1-2: device descriptor read/64, error -71
[Thu Nov 26 20:10:05 2026] mce: [Hardware Error]: CPU 3: Machine Check: 0 Bank 5: bea0000000000108
[Thu Nov 26 20:10:05 2026] [Hardware Error]: TSC 0 ADDR 1ffff8123456 MISC d012000100000000
[Thu Nov 26 20:10:06 2026] WHEA: Corrected hardware error on processor core 3
[Thu Nov 26 20:10:07 2026] ext4-fs error (device nvme0n1p2): benign test line
[Thu Nov 26 20:10:08 2026] Machine check events logged
";
    let lines = extract_mce_lines(log);
    assert_eq!(lines.len(), 4);
    assert!(lines[0].contains("mce:"));
    assert!(lines[1].contains("[Hardware Error]"));
    assert!(lines[2].contains("WHEA:"));
    assert!(lines[3].contains("Machine check events logged"));
}

#[test]
fn diff_mce_lines_handles_appends_duplicates_and_sliding_windows() {
    let empty: Vec<String> = Vec::new();
    let a = vec!["mce: err 1".to_string(), "mce: err 2".to_string()];
    assert_eq!(diff_mce_lines(&empty, &a), a);
    assert!(diff_mce_lines(&a, &empty).is_empty());
    assert!(diff_mce_lines(&a, &a).is_empty());

    // Normal append including a duplicate of the previous line
    let b = vec![
        "mce: err 1".to_string(),
        "mce: err 2".to_string(),
        "mce: err 2".to_string(),
        "mce: err 3".to_string(),
    ];
    assert_eq!(
        diff_mce_lines(&a, &b),
        vec!["mce: err 2".to_string(), "mce: err 3".to_string()]
    );

    // Sliding ring-buffer window (oldest line rotated out, 1 new line added)
    let rotated = vec![
        "mce: err 2".to_string(),
        "mce: err 2".to_string(),
        "mce: err 3".to_string(),
        "mce: err 4".to_string(),
    ];
    assert_eq!(diff_mce_lines(&b, &rotated), vec!["mce: err 4".to_string()]);

    // Completely disjoint
    let disjoint = vec!["mce: brand new".to_string()];
    assert_eq!(diff_mce_lines(&a, &disjoint), disjoint);
}

#[test]
fn mce_sysfs_reader_sums_machinecheck_error_counts_and_edac() {
    let dir = tempfile::tempdir().unwrap();
    let mc_root = dir.path().join("machinecheck");
    let edac_root = dir.path().join("edac_mc");

    // AMD Zen machinecheck hierarchy: bank0 is a config mask and must be ignored
    let mc0 = mc_root.join("machinecheck0");
    fs::create_dir_all(mc0.join("execution_unit/execution_unit")).unwrap();
    fs::create_dir_all(mc0.join("l2_cache/l2_cache")).unwrap();
    fs::create_dir_all(mc0.join("power")).unwrap();
    fs::write(mc0.join("bank0"), "0xffffffffffffffff\n").unwrap();
    fs::write(mc0.join("execution_unit/execution_unit/error_count"), "2\n").unwrap();
    fs::write(mc0.join("l2_cache/l2_cache/error_count"), "0x3\n").unwrap();

    let edac_mc0 = edac_root.join("mc0");
    fs::create_dir_all(&edac_mc0).unwrap();
    fs::write(edac_mc0.join("ce_count"), "4\n").unwrap();
    fs::write(edac_mc0.join("ue_count"), "1\n").unwrap();

    let (ce, ue) = read_mce_sysfs_counters(&mc_root, &edac_root);
    assert_eq!(ce, 2 + 3 + 4);
    assert_eq!(ue, 1);
}

#[test]
fn all_four_workloads_pass_deterministic_self_check() {
    let mut cache_buf = vec![0u64; CACHE_BUFFER_WORDS];
    for workload in [
        BenchWorkload::Mixed,
        BenchWorkload::Integer,
        BenchWorkload::FloatingPoint,
        BenchWorkload::Cache,
    ] {
        for batch in 0..3u64 {
            let res = run_workload_batch(workload, &mut cache_buf, batch);
            assert!(
                res.is_ok(),
                "workload {workload} failed deterministic self-check on batch {batch}: {res:?}"
            );
        }
    }
}

#[test]
fn validate_bench_config_checks_bounds_and_core_resolution() {
    let transport = MockTransport::granite_ridge_9700x();
    let hw = mock_hw_9700x(&transport, 0);

    // Default config expands empty `cores` to all 8 physical cores
    let all = validate_bench_config(&BenchConfig::default(), &hw).unwrap();
    assert_eq!(all.len(), 8);
    assert_eq!(all[0], (0, 0));
    assert_eq!(all[7], (7, 7));

    // Explicit subset deduplicates while preserving order
    let subset_cfg = BenchConfig {
        cores: vec![3, 1, 3, 5],
        ..BenchConfig::default()
    };
    let subset = validate_bench_config(&subset_cfg, &hw).unwrap();
    assert_eq!(subset, vec![(3, 3), (1, 1), (5, 5)]);

    // Invalid core index
    let bad_core = BenchConfig {
        cores: vec![99],
        ..BenchConfig::default()
    };
    assert!(matches!(
        validate_bench_config(&bad_core, &hw),
        Err(Error::InvalidInput(_))
    ));

    // Out-of-bounds duration, passes, and sample interval
    for bad_cfg in [
        BenchConfig {
            duration_per_core_secs: 0,
            ..BenchConfig::default()
        },
        BenchConfig {
            duration_per_core_secs: 3601,
            ..BenchConfig::default()
        },
        BenchConfig {
            passes: 0,
            ..BenchConfig::default()
        },
        BenchConfig {
            passes: 101,
            ..BenchConfig::default()
        },
        BenchConfig {
            sample_interval_ms: 50,
            ..BenchConfig::default()
        },
    ] {
        assert!(matches!(
            validate_bench_config(&bad_cfg, &hw),
            Err(Error::OutOfBounds { .. })
        ));
    }

    // Core with no mapped logical CPU is rejected
    let mut broken_hw = hw.clone();
    broken_hw.topology.cores[0] = CoreInfo {
        index: 0,
        ccd: 0,
        ccx: 0,
        core_in_ccd: 0,
        linux_cpus: Vec::new(),
    };
    assert!(matches!(
        validate_bench_config(&BenchConfig::default(), &broken_hw),
        Err(Error::InvalidInput(_))
    ));
}

#[test]
fn workload_and_status_display_parse_and_serde_roundtrip() {
    assert_eq!(BenchWorkload::default(), BenchWorkload::Mixed);
    assert_eq!(BenchWorkload::Mixed.to_string(), "mixed");
    assert_eq!(BenchWorkload::Integer.to_string(), "integer");
    assert_eq!(BenchWorkload::FloatingPoint.to_string(), "floating-point");
    assert_eq!(BenchWorkload::Cache.to_string(), "cache");

    assert_eq!(
        BenchWorkload::parse("FP"),
        Some(BenchWorkload::FloatingPoint)
    );
    assert_eq!(BenchWorkload::parse("int"), Some(BenchWorkload::Integer));
    assert_eq!(BenchWorkload::parse("cache"), Some(BenchWorkload::Cache));
    assert_eq!(BenchWorkload::parse("unknown"), None);

    let cfg = BenchConfig::default();
    let json = serde_json::to_string(&cfg).unwrap();
    let decoded: BenchConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded, cfg);
}

#[test]
fn run_core_cycler_one_second_mock_transport() {
    let transport = MockTransport::granite_ridge_9700x();
    let cpu = first_allowed_logical_cpu();
    let hw = mock_hw_9700x(&transport, cpu);

    let config = BenchConfig {
        cores: vec![0],
        duration_per_core_secs: 1,
        passes: 1,
        workload: BenchWorkload::Mixed,
        sample_interval_ms: 200,
        stop_on_error: true,
    };
    let stop_flag = Arc::new(AtomicBool::new(false));
    let mut progress_events = Vec::new();

    let report = run_core_cycler(&transport, &hw, &config, &stop_flag, |p| {
        progress_events.push(p.clone());
    })
    .unwrap();

    assert_eq!(report.codename, Codename::GraniteRidge.display_name());
    assert_eq!(report.workload, BenchWorkload::Mixed);
    assert_eq!(report.passes_completed, 1);
    assert!(!report.cancelled);
    assert_eq!(report.results.len(), 1);
    assert!(!progress_events.is_empty());

    let r = &report.results[0];
    assert_eq!(r.pass, 1);
    assert_eq!(r.core_index, 0);
    assert_eq!(r.logical_cpu, cpu);
    assert!(
        r.iterations > 0,
        "expected worker to complete stress batches"
    );
    assert!(r.duration_secs >= 0.9);
    assert_eq!(r.peak_temp_c, Some(54.2));
    assert_eq!(r.peak_power_w, Some(42.5));

    if r.status == CoreBenchStatus::Passed {
        assert!(report.passed);
        assert!(report.failed_cores.is_empty());
    }
}

#[test]
fn run_core_cycler_cancels_cleanly_via_stop_flag() {
    let transport = MockTransport::granite_ridge_9700x();
    let cpu = first_allowed_logical_cpu();
    let hw = mock_hw_9700x(&transport, cpu);

    let config = BenchConfig {
        cores: vec![0],
        duration_per_core_secs: 10,
        passes: 1,
        workload: BenchWorkload::Integer,
        sample_interval_ms: 100,
        stop_on_error: true,
    };
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_from_cb = Arc::clone(&stop_flag);

    let report = run_core_cycler(&transport, &hw, &config, &stop_flag, |_| {
        stop_from_cb.store(true, Ordering::Relaxed);
    })
    .unwrap();

    assert!(report.cancelled);
    assert!(!report.passed);
    assert_eq!(report.passes_completed, 0);
    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].status, CoreBenchStatus::Cancelled);
    assert!(report.total_duration_secs < 2.0);
}
