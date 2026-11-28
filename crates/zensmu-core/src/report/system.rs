use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Privacy-safe OS, kernel, motherboard, and BIOS metadata.
///
/// Only reads non-identifying fields; never reads serial numbers, UUIDs,
/// hostnames, usernames, or network addresses.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemBoardInfo {
    pub kernel_release: Option<String>,
    pub os_pretty_name: Option<String>,
    pub board_vendor: Option<String>,
    pub board_name: Option<String>,
    pub bios_version: Option<String>,
    pub bios_date: Option<String>,
}

impl SystemBoardInfo {
    /// Collect OS, kernel, and non-identifying DMI board/BIOS strings from Linux sysfs/procfs.
    pub fn detect_linux() -> Self {
        Self {
            kernel_release: read_trimmed_file(Path::new("/proc/sys/kernel/osrelease")),
            os_pretty_name: fs::read_to_string("/etc/os-release")
                .ok()
                .and_then(|s| parse_os_release_pretty_name(&s)),
            board_vendor: read_trimmed_file(Path::new("/sys/devices/virtual/dmi/id/board_vendor")),
            board_name: read_trimmed_file(Path::new("/sys/devices/virtual/dmi/id/board_name")),
            bios_version: read_trimmed_file(Path::new("/sys/devices/virtual/dmi/id/bios_version")),
            bios_date: read_trimmed_file(Path::new("/sys/devices/virtual/dmi/id/bios_date")),
        }
    }
}

pub(super) fn read_trimmed_file(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

pub(super) fn parse_os_release_pretty_name(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("PRETTY_NAME=") {
            let unquoted = rest
                .trim()
                .strip_prefix('"')
                .and_then(|s| s.strip_suffix('"'))
                .or_else(|| {
                    rest.trim()
                        .strip_prefix('\'')
                        .and_then(|s| s.strip_suffix('\''))
                })
                .unwrap_or_else(|| rest.trim());
            let clean = unquoted.trim();
            if !clean.is_empty() {
                return Some(clean.to_string());
            }
        }
    }
    None
}
