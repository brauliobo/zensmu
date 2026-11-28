use super::system::read_trimmed_file;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

const ALLOWED_HWMON_CHIPS: &[&str] = &["k10temp", "zenpower", "amdgpu"];
const MAX_HWMON_READINGS: usize = 32;

/// Single reference sensor reading from an allowlisted Linux hwmon chip
/// (`k10temp`, `zenpower`, or `amdgpu`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HwmonSensorReading {
    pub chip: String,
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SensorKind {
    Temp = 0,
    Power = 1,
    Curr = 2,
    In = 3,
}

struct ParsedSensorFile {
    kind: SensorKind,
    channel: u32,
    is_average: bool,
    prefix: String,
    path: PathBuf,
}

fn parse_hwmon_sensor_file(file_name: &str, path: PathBuf) -> Option<ParsedSensorFile> {
    let (prefix, is_average) = if let Some(p) = file_name.strip_suffix("_input") {
        (p, false)
    } else {
        (file_name.strip_suffix("_average")?, true)
    };

    let (kind, rest) = if let Some(r) = prefix.strip_prefix("temp") {
        if is_average {
            return None;
        }
        (SensorKind::Temp, r)
    } else if let Some(r) = prefix.strip_prefix("power") {
        (SensorKind::Power, r)
    } else if let Some(r) = prefix.strip_prefix("curr") {
        if is_average {
            return None;
        }
        (SensorKind::Curr, r)
    } else {
        let r = prefix.strip_prefix("in")?;
        if is_average {
            return None;
        }
        (SensorKind::In, r)
    };

    if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let channel: u32 = rest.parse().ok()?;

    Some(ParsedSensorFile {
        kind,
        channel,
        is_average,
        prefix: prefix.to_string(),
        path,
    })
}

fn format_hwmon_value(kind: SensorKind, raw: i64) -> String {
    match kind {
        SensorKind::Temp => format!("{:.1} °C", raw as f64 / 1000.0),
        SensorKind::Curr => format!("{:.1} A", raw as f64 / 1000.0),
        SensorKind::In => format!("{:.3} V", raw as f64 / 1000.0),
        SensorKind::Power => format!("{:.1} W", raw as f64 / 1_000_000.0),
    }
}

pub(super) fn scan_hwmon_dir(hwmon_root: &Path) -> Vec<HwmonSensorReading> {
    let Ok(entries) = fs::read_dir(hwmon_root) else {
        return Vec::new();
    };

    let mut dirs: Vec<(u32, String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(rest) = name.strip_prefix("hwmon") {
            let num = rest.parse::<u32>().unwrap_or(u32::MAX);
            dirs.push((num, name, entry.path()));
        }
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

    let mut readings = Vec::new();
    for (_, _, dir) in dirs {
        if readings.len() >= MAX_HWMON_READINGS {
            break;
        }
        let Some(chip) = read_trimmed_file(&dir.join("name")) else {
            continue;
        };
        if !ALLOWED_HWMON_CHIPS.contains(&chip.as_str()) {
            continue;
        }

        let Ok(files) = fs::read_dir(&dir) else {
            continue;
        };

        let mut sensors: Vec<ParsedSensorFile> = files
            .flatten()
            .filter_map(|e| {
                let fname = e.file_name().to_string_lossy().into_owned();
                parse_hwmon_sensor_file(&fname, e.path())
            })
            .collect();
        // Sort deterministically by kind, channel, and `_input` before `_average`.
        sensors.sort_by_key(|s| (s.kind, s.channel, s.is_average));

        let mut recorded_power_channels: HashSet<u32> = HashSet::new();
        for sensor in sensors {
            if readings.len() >= MAX_HWMON_READINGS {
                break;
            }
            if sensor.kind == SensorKind::Power
                && sensor.is_average
                && recorded_power_channels.contains(&sensor.channel)
            {
                continue;
            }

            let Some(raw_str) = read_trimmed_file(&sensor.path) else {
                continue;
            };
            let Ok(raw_val) = raw_str.parse::<i64>() else {
                continue;
            };

            if sensor.kind == SensorKind::Power {
                recorded_power_channels.insert(sensor.channel);
            }

            let label = read_trimmed_file(&dir.join(format!("{}_label", sensor.prefix)))
                .unwrap_or(sensor.prefix);

            readings.push(HwmonSensorReading {
                chip: chip.clone(),
                label,
                value: format_hwmon_value(sensor.kind, raw_val),
            });
        }
    }

    readings
}
