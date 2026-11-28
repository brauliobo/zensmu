use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;
use std::process::Command;

/// Snapshot of Linux hardware Machine Check / EDAC counters and kernel error lines.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MceCounters {
    /// Sum of corrected error counts from `machinecheck` threshold blocks and EDAC `ce_count`.
    pub corrected_errors: u64,
    /// Sum of uncorrected error counts from EDAC `ue_count`.
    pub uncorrected_errors: u64,
    /// Matching MCE / WHEA / Hardware Error lines from `dmesg` or `journalctl -k`.
    pub dmesg_mce_lines: Vec<String>,
    /// Whether `dmesg` or `journalctl -k` was readable in this execution context.
    pub dmesg_readable: bool,
}

/// Extract MCE / WHEA / Hardware Error lines from kernel log text.
pub fn extract_mce_lines(log: &str) -> Vec<String> {
    log.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && is_mce_log_line(line))
        .map(str::to_string)
        .collect()
}

fn is_mce_log_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    lower.contains("mce:")
        || lower.contains("hardware error")
        || lower.contains("whea")
        || lower.contains("machine check")
}

/// Compute newly appended MCE log lines between a `before` and `after` snapshot.
///
/// Uses longest suffix-prefix overlap so both normal appends, repeated identical
/// MCE lines, and sliding kernel ring-buffer windows (e.g. `journalctl -n 200`)
/// are handled accurately.
pub fn diff_mce_lines(before: &[String], after: &[String]) -> Vec<String> {
    if after.is_empty() {
        return Vec::new();
    }
    if before.is_empty() {
        return after.to_vec();
    }
    let max_overlap = before.len().min(after.len());
    for overlap in (1..=max_overlap).rev() {
        let before_start = before.len() - overlap;
        if let (Some(b_tail), Some(a_head)) = (before.get(before_start..), after.get(..overlap)) {
            if b_tail == a_head {
                return after
                    .get(overlap..)
                    .map(<[String]>::to_vec)
                    .unwrap_or_default();
            }
        }
    }
    after.to_vec()
}

/// Snapshot Linux MCE / EDAC sysfs counters and kernel log MCE lines (best-effort).
pub fn read_mce_counters() -> MceCounters {
    let (corrected_errors, uncorrected_errors) = read_mce_sysfs_counters(
        Path::new("/sys/devices/system/machinecheck"),
        Path::new("/sys/devices/system/edac/mc"),
    );
    let (dmesg_readable, dmesg_mce_lines) = read_kernel_mce_log();

    MceCounters {
        corrected_errors,
        uncorrected_errors,
        dmesg_mce_lines,
        dmesg_readable,
    }
}

pub(super) fn read_mce_sysfs_counters(machinecheck_root: &Path, edac_mc_root: &Path) -> (u64, u64) {
    let mut corrected = 0u64;
    let mut uncorrected = 0u64;

    if let Ok(entries) = fs::read_dir(machinecheck_root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            if !name.to_string_lossy().starts_with("machinecheck") {
                continue;
            }
            if entry.file_type().is_ok_and(|ft| ft.is_dir()) {
                collect_machinecheck_error_counts(&entry.path(), 0, &mut corrected);
            }
        }
    }

    if let Ok(entries) = fs::read_dir(edac_mc_root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let s = name.to_string_lossy();
            let Some(rest) = s.strip_prefix("mc") else {
                continue;
            };
            if rest.parse::<u32>().is_err() {
                continue;
            }
            let path = entry.path();
            if let Some(ce) = read_u64_file(&path.join("ce_count")) {
                corrected = corrected.saturating_add(ce);
            }
            if let Some(ue) = read_u64_file(&path.join("ue_count")) {
                uncorrected = uncorrected.saturating_add(ue);
            }
        }
    }

    (corrected, uncorrected)
}

/// Recursively scan real subdirectories (never following symlinks) under a
/// `machinecheck<N>` directory up to depth 3 for AMD threshold `error_count` files.
fn collect_machinecheck_error_counts(dir: &Path, depth: usize, total: &mut u64) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(ft) = entry.file_type() else {
            continue;
        };
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if ft.is_dir() {
            if name_str == "power" {
                continue;
            }
            collect_machinecheck_error_counts(&entry.path(), depth + 1, total);
        } else if ft.is_file() && (name_str == "error_count" || name_str == "ce_count") {
            if let Some(val) = read_u64_file(&entry.path()) {
                *total = total.saturating_add(val);
            }
        }
    }
}

fn read_u64_file(path: &Path) -> Option<u64> {
    let raw = fs::read_to_string(path).ok()?;
    let trimmed = raw.trim();
    if let Some(hex) = trimmed
        .strip_prefix("0x")
        .or_else(|| trimmed.strip_prefix("0X"))
    {
        u64::from_str_radix(hex, 16).ok()
    } else {
        trimmed.parse::<u64>().ok()
    }
}

fn read_kernel_mce_log() -> (bool, Vec<String>) {
    if let Ok(output) = Command::new("dmesg")
        .args(["-T", "--level=err,crit,alert,emerg"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            return (true, extract_mce_lines(&text));
        }
    }

    if let Ok(output) = Command::new("journalctl")
        .args(["-k", "-b", "-p", "err", "--no-pager", "-n", "200"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            return (true, extract_mce_lines(&text));
        }
    }

    (false, Vec::new())
}
