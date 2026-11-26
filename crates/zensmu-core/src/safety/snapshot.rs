//! Pre-change SMU state snapshots and atomic persistence for rollback.

use crate::error::{Error, Result};
use crate::safety::{map_io_err, now_unix_secs, write_atomic, write_atomic_create_new};
use crate::types::{Codename, CurveShaperState};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

/// Default runtime directory for ephemeral boot-scoped snapshots.
pub const DEFAULT_STATE_DIR: &str = "/run/zensmu";

/// Snapshot captured before the very first mutation of the current boot.
pub const BOOT_BASELINE_FILE: &str = "boot-baseline.json";

/// Snapshot captured immediately before the most recent mutation.
pub const LAST_STATE_FILE: &str = "last-state.json";

/// Current on-disk snapshot schema version.
pub const SNAPSHOT_SCHEMA_VERSION: u32 = 1;

/// Captured hardware SMU state used for rollback and profile diffing.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct SmuStateSnapshot {
    pub schema: u32,
    pub captured_unix_secs: u64,
    pub codename: Codename,
    pub physical_cores: usize,
    pub co_per_core: Option<Vec<(u32, i16)>>,
    pub igpu_co: Option<i16>,
    pub curve_shaper: Option<CurveShaperState>,
    pub pbo_scalar: Option<f32>,
    pub fmax_mhz: Option<u32>,
    pub ppt_w: Option<u32>,
    pub tdc_a: Option<u32>,
    pub edc_a: Option<u32>,
    pub tctl_c: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stapm_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slow_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apu_slow_w: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tdc_soc_a: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edc_soc_a: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub apu_skin_temp_c: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dgpu_skin_temp_c: Option<u32>,
}

impl SmuStateSnapshot {
    /// Create a new empty snapshot stamped with the current Unix timestamp and schema version `1`.
    pub fn new(codename: Codename, physical_cores: usize) -> Self {
        Self {
            schema: SNAPSHOT_SCHEMA_VERSION,
            captured_unix_secs: now_unix_secs(),
            codename,
            physical_cores,
            co_per_core: None,
            igpu_co: None,
            curve_shaper: None,
            pbo_scalar: None,
            fmax_mhz: None,
            ppt_w: None,
            tdc_a: None,
            edc_a: None,
            tctl_c: None,
            stapm_w: None,
            slow_w: None,
            apu_slow_w: None,
            tdc_soc_a: None,
            edc_soc_a: None,
            apu_skin_temp_c: None,
            dgpu_skin_temp_c: None,
        }
    }
}

/// Atomic JSON store for boot baseline and last-known pre-change snapshots.
#[derive(Debug, Clone)]
pub struct SnapshotStore {
    dir: PathBuf,
}

impl Default for SnapshotStore {
    fn default() -> Self {
        Self::new(PathBuf::from(DEFAULT_STATE_DIR))
    }
}

impl SnapshotStore {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn boot_baseline_path(&self) -> PathBuf {
        self.dir.join(BOOT_BASELINE_FILE)
    }

    pub fn last_state_path(&self) -> PathBuf {
        self.dir.join(LAST_STATE_FILE)
    }

    /// Persist `boot-baseline.json` atomically ONLY if it does not already exist.
    ///
    /// Returns `Ok(true)` if the boot baseline was created, or `Ok(false)` if one was already
    /// captured earlier in this boot.
    pub fn ensure_boot_baseline(&self, snap: &SmuStateSnapshot) -> Result<bool> {
        let json = serialize_snapshot(snap)?;
        write_atomic_create_new(&self.boot_baseline_path(), json.as_bytes())
    }

    /// Atomically save `last-state.json` (write to temp file + fsync + rename).
    pub fn save_last_state(&self, snap: &SmuStateSnapshot) -> Result<()> {
        let json = serialize_snapshot(snap)?;
        write_atomic(&self.last_state_path(), json.as_bytes())
    }

    /// Load `boot-baseline.json` if it exists, or `Ok(None)` if not yet captured.
    pub fn load_boot_baseline(&self) -> Result<Option<SmuStateSnapshot>> {
        load_snapshot_file(&self.boot_baseline_path())
    }

    /// Load `last-state.json` if it exists, or `Ok(None)` if not yet captured.
    pub fn load_last_state(&self) -> Result<Option<SmuStateSnapshot>> {
        load_snapshot_file(&self.last_state_path())
    }
}

fn serialize_snapshot(snap: &SmuStateSnapshot) -> Result<String> {
    if snap.schema != SNAPSHOT_SCHEMA_VERSION {
        return Err(Error::InvalidInput(format!(
            "unsupported snapshot schema version {} (expected {})",
            snap.schema, SNAPSHOT_SCHEMA_VERSION
        )));
    }
    serde_json::to_string_pretty(snap).map_err(|e| Error::Serde(e.to_string()))
}

fn load_snapshot_file(path: &Path) -> Result<Option<SmuStateSnapshot>> {
    let raw = match fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(map_io_err(path, e)),
    };

    let snap: SmuStateSnapshot =
        serde_json::from_str(&raw).map_err(|e| Error::Serde(e.to_string()))?;
    if snap.schema != SNAPSHOT_SCHEMA_VERSION {
        return Err(Error::InvalidInput(format!(
            "unsupported snapshot schema version {} in {} (expected {})",
            snap.schema,
            path.display(),
            SNAPSHOT_SCHEMA_VERSION
        )));
    }
    Ok(Some(snap))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ShaperTierMargins;

    fn sample_snapshot(ppt_w: u32, co0: i16) -> SmuStateSnapshot {
        let mut snap = SmuStateSnapshot::new(Codename::GraniteRidge, 8);
        snap.captured_unix_secs = 1_700_000_000;
        snap.co_per_core = Some(vec![(0, co0), (1, -20)]);
        snap.igpu_co = Some(-10);
        snap.curve_shaper = Some(CurveShaperState {
            min: ShaperTierMargins {
                low: -5,
                med: -10,
                high: 0,
            },
            ..CurveShaperState::default()
        });
        snap.pbo_scalar = Some(1.0);
        snap.fmax_mhz = Some(5550);
        snap.ppt_w = Some(ppt_w);
        snap.tdc_a = Some(75);
        snap.edc_a = Some(150);
        snap.tctl_c = Some(85);
        snap
    }

    #[test]
    fn empty_store_returns_none() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SnapshotStore::new(tmp.path().join("state"));
        assert_eq!(store.load_boot_baseline().unwrap(), None);
        assert_eq!(store.load_last_state().unwrap(), None);
    }

    #[test]
    fn boot_baseline_is_written_once_and_never_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SnapshotStore::new(tmp.path().join("zensmu"));

        let first = sample_snapshot(88, 0);
        let second = sample_snapshot(120, -25);

        assert!(store.ensure_boot_baseline(&first).unwrap());
        assert!(!store.ensure_boot_baseline(&second).unwrap());

        let loaded = store.load_boot_baseline().unwrap().unwrap();
        assert_eq!(loaded, first);
    }

    #[test]
    fn last_state_overwrites_atomically_without_leaking_temp_files() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SnapshotStore::new(tmp.path().join("zensmu"));

        let first = sample_snapshot(88, -10);
        let second = sample_snapshot(105, -25);

        store.save_last_state(&first).unwrap();
        assert_eq!(store.load_last_state().unwrap(), Some(first));

        store.save_last_state(&second).unwrap();
        assert_eq!(store.load_last_state().unwrap(), Some(second));

        // Verify no stray .tmp files remain
        let entries: Vec<_> = fs::read_dir(store.dir())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(entries, vec![LAST_STATE_FILE.to_string()]);
    }

    #[test]
    fn rejects_unsupported_schema_version() {
        let tmp = tempfile::tempdir().unwrap();
        let store = SnapshotStore::new(tmp.path().to_path_buf());
        let mut bad = sample_snapshot(88, 0);
        bad.schema = 99;

        assert!(store.save_last_state(&bad).is_err());

        let bad_json = serde_json::to_string(&bad).unwrap();
        fs::write(store.last_state_path(), bad_json).unwrap();
        assert!(store.load_last_state().is_err());
    }
}
