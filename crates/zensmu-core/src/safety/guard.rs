//! Boot-loop crash guard for unattended (`systemd`) profile auto-apply.
//!
//! Before applying SMU mutations at boot, `BootLoopGuard::check_and_arm` durably writes a
//! sentinel file to persistent storage (`fsync`-ed to disk before any SMU register write).
//! If the system crashes, panics, or hard-reboots during or immediately after SMU writes,
//! `ArmedBootGuard::disarm()` is never reached and the sentinel remains on disk. On the next
//! boot, `check_and_arm` detects the sentinel and refuses to auto-apply (`Error::BootGuardTripped`),
//! preventing an infinite boot-crash loop.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

/// Default persistent path for the boot-loop crash guard sentinel.
pub const DEFAULT_GUARD_PATH: &str = "/var/lib/zensmu/applying.lock";

static GUARD_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

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
        if self.is_tripped() {
            return Err(Error::BootGuardTripped {
                sentinel: self.sentinel_path.display().to_string(),
            });
        }

        let parent = self
            .sentinel_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|e| map_io_err(parent, e))?;

        let info = SentinelInfo {
            profile: profile_name.to_string(),
            armed_unix_secs: now_unix_secs(),
            pid: std::process::id(),
        };
        let payload = serde_json::to_string_pretty(&info).map_err(|e| Error::Serde(e.to_string()))?;

        let seq = GUARD_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let file_name = self
            .sentinel_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("applying.lock");
        let tmp_path = parent.join(format!(".{file_name}.{}.{}.tmp", info.pid, seq));

        let mut file = File::create(&tmp_path).map_err(|e| map_io_err(&tmp_path, e))?;
        if let Err(e) = file
            .write_all(payload.as_bytes())
            .and_then(|()| file.sync_all())
        {
            let _ = fs::remove_file(&tmp_path);
            return Err(map_io_err(&tmp_path, e));
        }
        drop(file);

        // Atomically link sentinel only if absent, preventing concurrent race overwrites.
        match fs::hard_link(&tmp_path, &self.sentinel_path) {
            Ok(()) => {
                let _ = fs::remove_file(&tmp_path);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&tmp_path);
                return Err(Error::BootGuardTripped {
                    sentinel: self.sentinel_path.display().to_string(),
                });
            }
            Err(_) => {
                if self.sentinel_path.exists() {
                    let _ = fs::remove_file(&tmp_path);
                    return Err(Error::BootGuardTripped {
                        sentinel: self.sentinel_path.display().to_string(),
                    });
                }
                fs::rename(&tmp_path, &self.sentinel_path).map_err(|e| {
                    let _ = fs::remove_file(&tmp_path);
                    map_io_err(&self.sentinel_path, e)
                })?;
            }
        }

        // Flush directory entry to persistent storage before any SMU writes occur.
        sync_dir_best_effort(parent);

        Ok(ArmedBootGuard {
            sentinel_path: self.sentinel_path.clone(),
            disarmed: false,
        })
    }

    /// Manually clear a tripped guard sentinel (e.g., `zensmu profile clear-guard`).
    ///
    /// Succeeds idempotently if the sentinel does not exist.
    pub fn clear(&self) -> Result<()> {
        remove_if_exists(&self.sentinel_path)
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
        remove_if_exists(&self.sentinel_path)?;
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

fn remove_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                sync_dir_best_effort(parent);
            }
            Ok(())
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(e) => Err(map_io_err(path, e)),
    }
}

fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn sync_dir_best_effort(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

fn map_io_err(path: &Path, source: std::io::Error) -> Error {
    if source.kind() == ErrorKind::PermissionDenied {
        Error::PermissionDenied {
            path: path.display().to_string(),
        }
    } else {
        Error::Io {
            path: path.display().to_string(),
            source,
        }
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
