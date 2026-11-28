use crate::error::{Error, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Pin the calling OS thread to a single Linux logical CPU ID via `sched_setaffinity`.
#[cfg(target_os = "linux")]
pub(super) fn pin_current_thread_to_cpu(logical_cpu: u32) -> Result<()> {
    let cpu_idx = usize::try_from(logical_cpu).map_err(|_| Error::Io {
        path: format!("sched_setaffinity(cpu={logical_cpu})"),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("logical CPU ID {logical_cpu} out of range"),
        ),
    })?;
    let max_cpus = usize::try_from(libc::CPU_SETSIZE).unwrap_or(1024);
    if cpu_idx >= max_cpus {
        return Err(Error::Io {
            path: format!("sched_setaffinity(cpu={logical_cpu})"),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("logical CPU ID {logical_cpu} exceeds CPU_SETSIZE ({max_cpus})"),
            ),
        });
    }

    // SAFETY: `cpu_set_t` is a POD bitset zero-initialized before setting a single
    // bounds-checked CPU index (`cpu_idx < CPU_SETSIZE`), then passed to
    // `sched_setaffinity(0, ...)` to pin the calling thread.
    let rc = unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu_idx, &mut set);
        libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set)
    };
    if rc != 0 {
        let source = std::io::Error::last_os_error();
        return Err(Error::Io {
            path: format!("sched_setaffinity(cpu={logical_cpu})"),
            source,
        });
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub(super) fn pin_current_thread_to_cpu(logical_cpu: u32) -> Result<()> {
    Err(Error::Io {
        path: format!("sched_setaffinity(cpu={logical_cpu})"),
        source: std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "CPU affinity pinning is only supported on Linux",
        ),
    })
}

/// Read live per-logical-CPU clock frequency in MHz from Linux `cpufreq` sysfs.
pub(super) fn read_cpu_freq_mhz(logical_cpu: u32) -> Option<f32> {
    let base = PathBuf::from(format!("/sys/devices/system/cpu/cpu{logical_cpu}/cpufreq"));
    for file in ["scaling_cur_freq", "cpuinfo_cur_freq"] {
        if let Ok(raw) = fs::read_to_string(base.join(file)) {
            if let Ok(khz) = raw.trim().parse::<u64>() {
                let mhz = (khz as f64 / 1000.0) as f32;
                if mhz.is_finite() && (100.0..=10_000.0).contains(&mhz) {
                    return Some(mhz);
                }
            }
        }
    }
    None
}

/// Locate an AMD SVI2 / Vcore `hwmon` input path once per run (best-effort).
pub(super) fn find_hwmon_vcore_path() -> Option<PathBuf> {
    let hwmon_root = Path::new("/sys/class/hwmon");
    let entries = fs::read_dir(hwmon_root).ok()?;
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(chip) = fs::read_to_string(dir.join("name"))
            .ok()
            .map(|s| s.trim().to_ascii_lowercase())
        else {
            continue;
        };
        let is_amd_cpu_sensor = matches!(
            chip.as_str(),
            "k10temp" | "zenpower" | "zenpower3" | "zenpower5"
        );
        if !is_amd_cpu_sensor {
            continue;
        }
        let Ok(files) = fs::read_dir(&dir) else {
            continue;
        };
        for f in files.flatten() {
            let fname = f.file_name().to_string_lossy().into_owned();
            let Some(prefix) = fname.strip_suffix("_label") else {
                continue;
            };
            if !prefix.starts_with("in") {
                continue;
            }
            let Ok(label) = fs::read_to_string(f.path()) else {
                continue;
            };
            let norm = label.trim().to_ascii_lowercase();
            if norm.contains("svi2_core") || norm == "vcore" || norm.contains("cpu core") {
                let input_path = dir.join(format!("{prefix}_input"));
                if input_path.is_file() {
                    return Some(input_path);
                }
            }
        }
    }
    None
}

/// Read an `in*_input` hwmon millivolt sensor and convert to Volts.
pub(super) fn read_hwmon_voltage_v(path: &Path) -> Option<f32> {
    let raw = fs::read_to_string(path).ok()?;
    let mv = raw.trim().parse::<i64>().ok()?;
    let volts = (mv as f64 / 1000.0) as f32;
    if volts.is_finite() && (0.2..=2.2).contains(&volts) {
        Some(volts)
    } else {
        None
    }
}

pub(super) fn update_max(slot: &mut Option<f32>, val: f32) {
    if !val.is_finite() {
        return;
    }
    *slot = Some(match *slot {
        Some(prev) if prev >= val => prev,
        _ => val,
    });
}

pub(super) fn update_min(slot: &mut Option<f32>, val: f32) {
    if !val.is_finite() {
        return;
    }
    *slot = Some(match *slot {
        Some(prev) if prev <= val => prev,
        _ => val,
    });
}
