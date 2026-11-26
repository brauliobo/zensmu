pub mod bounds;
pub mod guard;
pub mod persist;
pub mod snapshot;

use crate::error::{Error, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static ATOMIC_TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Map a `std::io::Error` into `Error::PermissionDenied` or `Error::Io`.
pub(crate) fn map_io_err(path: &Path, source: std::io::Error) -> Error {
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

/// Current Unix timestamp in whole seconds (0 if system clock predates epoch).
pub(crate) fn now_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Best-effort directory `fsync` to make rename/link/unlink durable across crashes.
pub(crate) fn sync_dir_best_effort(dir: &Path) {
    if let Ok(d) = File::open(dir) {
        let _ = d.sync_all();
    }
}

fn parent_dir(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

/// Create a unique sibling temporary file with `O_EXCL` (`create_new(true)`),
/// write all `bytes`, `fsync`, and close before returning its path.
/// Cleans up the temp file on any write/sync failure.
fn write_temp_sibling(target: &Path, bytes: &[u8]) -> Result<(PathBuf, PathBuf)> {
    let parent = parent_dir(target).to_path_buf();
    fs::create_dir_all(&parent).map_err(|e| map_io_err(&parent, e))?;

    let file_name = target
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("zensmu");
    let pid = std::process::id();

    let (mut file, tmp_path) = loop {
        let seq = ATOMIC_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(".{file_name}.{pid}.{seq}.tmp"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(f) => break (f, candidate),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(map_io_err(&candidate, e)),
        }
    };

    let write_res = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    if let Err(e) = write_res {
        let _ = fs::remove_file(&tmp_path);
        return Err(map_io_err(&tmp_path, e));
    }

    Ok((parent, tmp_path))
}

/// Atomically write `bytes` to `target` (temp file + `fsync` + `rename` + dir `fsync`).
pub(crate) fn write_atomic(target: &Path, bytes: &[u8]) -> Result<()> {
    let (parent, tmp_path) = write_temp_sibling(target, bytes)?;
    fs::rename(&tmp_path, target).map_err(|e| {
        let _ = fs::remove_file(&tmp_path);
        map_io_err(target, e)
    })?;
    sync_dir_best_effort(&parent);
    Ok(())
}

/// Atomically create `target` ONLY if it does not already exist.
///
/// Uses POSIX `hard_link` for race-free create-if-absent, falling back to `rename`
/// if the filesystem does not support hard links. Always cleans up its temp file.
/// Returns `Ok(true)` if created, or `Ok(false)` if `target` already existed.
pub(crate) fn write_atomic_create_new(target: &Path, bytes: &[u8]) -> Result<bool> {
    if target.exists() {
        return Ok(false);
    }

    let (parent, tmp_path) = write_temp_sibling(target, bytes)?;
    match fs::hard_link(&tmp_path, target) {
        Ok(()) => {
            let _ = fs::remove_file(&tmp_path);
            sync_dir_best_effort(&parent);
            Ok(true)
        }
        Err(e) if e.kind() == ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&tmp_path);
            Ok(false)
        }
        Err(_) => {
            if target.exists() {
                let _ = fs::remove_file(&tmp_path);
                return Ok(false);
            }
            fs::rename(&tmp_path, target).map_err(|e| {
                let _ = fs::remove_file(&tmp_path);
                map_io_err(target, e)
            })?;
            sync_dir_best_effort(&parent);
            Ok(true)
        }
    }
}

/// Remove `path` if it exists (race-free), syncing the parent directory on deletion.
/// Returns `Ok(true)` if the file was removed, `Ok(false)` if it did not exist.
pub(crate) fn remove_file_if_exists(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => {
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
                sync_dir_best_effort(parent);
            }
            Ok(true)
        }
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(map_io_err(path, e)),
    }
}
