//! Boot-loop crash guard for unattended (`systemd`) profile auto-apply.
//!
//! Before applying SMU mutations at boot, `BootLoopGuard::check_and_arm` durably writes a
//! sentinel file to persistent storage (`fsync`-ed to disk before any SMU register write).
//! If the system crashes, panics, or hard-reboots during or immediately after SMU writes,
//! `ArmedBootGuard::disarm()` is never reached and the sentinel remains on disk. On the next
//! boot, `check_and_arm` detects the sentinel and refuses to auto-apply (`Error::BootGuardTripped`),
//! preventing an infinite boot-crash loop.

use crate::error::{Error, Result};
use crate::safety::{map_io_err, now_unix_secs, remove_file_if_exists, write_atomic_create_new};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Default persistent path for the boot-loop crash guard sentinel.
pub const DEFAULT_GUARD_PATH: &str = "/var/lib/zensmu/applying.lock";

/// Metadata recorded inside the boot-loop guard sentinel file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentinelInfo {
    pub profile: String,
    pub armed_unix_secs: u64,
    pub pid: u32,
}

/// Manages the persistent boot-loop crash guard sentinel.
#[derive(Debug, Clone)]
pub struct BootLoopGuard {
    sentinel_path: PathBuf,
}

impl Default for BootLoopGuard {
    fn default() -> Self {
        Self::new(PathBuf::from(DEFAULT_GUARD_PATH))
    }
}

impl BootLoopGuard {
    pub fn new(sentinel_path: PathBuf) -> Self {
        Self { sentinel_path }
    }

    pub fn sentinel_path(&self) -> &Path {
        &self.sentinel_path
    }

    /// Returns `true` if a previous apply left the sentinel on disk.
    pub fn is_tripped(&self) -> bool {
        self.sentinel_path.exists()
    }

    /// Read the sentinel metadata if the guard is currently tripped.
    pub fn read_sentinel(&self) -> Result<Option<SentinelInfo>> {
        let raw = match fs::read_to_string(&self.sentinel_path) {
            Ok(s) => s,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(map_io_err(&self.sentinel_path, e)),
        };
        let info: SentinelInfo =
            serde_json::from_str(&raw).map_err(|e| Error::Serde(e.to_string()))?;
        Ok(Some(info))
    }

    /// Verify the guard is not tripped, then atomically and durably arm it before SMU writes.
    ///
    /// - If the sentinel already exists, returns `Err(Error::BootGuardTripped { sentinel })`.
    /// - Otherwise creates parent directories if needed, writes sentinel metadata with timestamp
    ///   and profile name, calls `sync_all()` so it hits disk before returning, and returns an
    ///   [`ArmedBootGuard`] handle.
    pub fn check_and_arm(&self, profile_name: &str) -> Result<ArmedBootGuard> {
        let info = SentinelInfo {
            profile: profile_name.to_string(),
            armed_unix_secs: now_unix_secs(),
            pid: std::process::id(),
        };
        let payload =
            serde_json::to_string_pretty(&info).map_err(|e| Error::Serde(e.to_string()))?;

        if !write_atomic_create_new(&self.sentinel_path, payload.as_bytes())? {
            return Err(Error::BootGuardTripped {
                sentinel: self.sentinel_path.display().to_string(),
            });
        }

        Ok(ArmedBootGuard {
            sentinel_path: self.sentinel_path.clone(),
            disarmed: false,
        })
    }

    /// Manually clear a tripped guard sentinel (e.g., `zensmu profile clear-guard`).
    ///
    /// Succeeds idempotently if the sentinel does not exist.
    pub fn clear(&self) -> Result<()> {
        let _ = remove_file_if_exists(&self.sentinel_path)?;
        Ok(())
    }
}

/// Active boot-loop guard token returned by [`BootLoopGuard::check_and_arm`].
///
/// **Safety invariant:** `Drop` intentionally does **NOT** remove the sentinel file if
/// [`Self::disarm`] was not called. Any panic, early error return, or sudden power loss /
/// kernel crash leaves the sentinel armed on disk so the next boot will refuse to auto-apply.
#[derive(Debug)]
pub struct ArmedBootGuard {
    sentinel_path: PathBuf,
    disarmed: bool,
}

impl ArmedBootGuard {
    pub fn sentinel_path(&self) -> &Path {
        &self.sentinel_path
    }

    /// Remove the sentinel cleanly after profile application and stability verification succeed.
    pub fn disarm(mut self) -> Result<()> {
        let _ = remove_file_if_exists(&self.sentinel_path)?;
        self.disarmed = true;
        Ok(())
    }
}

impl Drop for ArmedBootGuard {
    fn drop(&mut self) {
        // Intentionally do NOT delete `self.sentinel_path` when `!self.disarmed`:
        // crashes, panics, or aborted applies MUST leave the sentinel on disk.
        let _ = self.disarmed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arm_and_disarm_clears_sentinel() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("var/lib/zensmu/applying.lock");
        let guard = BootLoopGuard::new(sentinel.clone());

        assert!(!guard.is_tripped());
        let armed = guard.check_and_arm("daily-undervolt").unwrap();
        assert!(guard.is_tripped());
        assert!(sentinel.exists());

        let info = guard.read_sentinel().unwrap().unwrap();
        assert_eq!(info.profile, "daily-undervolt");
        assert_eq!(info.pid, std::process::id());

        armed.disarm().unwrap();
        assert!(!guard.is_tripped());
        assert!(!sentinel.exists());
    }

    #[test]
    fn dropping_armed_guard_without_disarm_leaves_sentinel_tripped() {
        let tmp = tempfile::tempdir().unwrap();
        let sentinel = tmp.path().join("applying.lock");
        let guard = BootLoopGuard::new(sentinel.clone());

        {
            let _armed = guard.check_and_arm("unstable-co").unwrap();
            assert!(guard.is_tripped());
            // Simulates panic / early return / crash without calling disarm()
        }

        assert!(guard.is_tripped());
        assert!(sentinel.exists());

        // Subsequent boot auto-apply MUST be refused
        let err = guard.check_and_arm("unstable-co").unwrap_err();
        assert!(matches!(err, Error::BootGuardTripped { .. }));

        // Manual recovery clears the guard and allows arming again
        guard.clear().unwrap();
        assert!(!guard.is_tripped());
        guard.clear().unwrap(); // idempotent

        let armed = guard.check_and_arm("safe-profile").unwrap();
        armed.disarm().unwrap();
        assert!(!guard.is_tripped());
    }
}
