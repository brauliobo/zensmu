use crate::error::{Error, Result, SmuStatus};
use crate::transport::{DriverSysfsInfo, MailboxResponse, SmuTransport};
use crate::types::{Mailbox, SmuArgs};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_SYSFS_PATH: &str = "/sys/kernel/ryzen_smu_drv";
pub const DEFAULT_LOCK_PATH: &str = "/run/lock/zensmu.lock";
pub const FALLBACK_LOCK_PATH: &str = "/tmp/zensmu.lock";
pub const MAX_SAFE_PM_TABLE_SIZE: usize = 4096;

/// Real Linux sysfs transport for `/sys/kernel/ryzen_smu_drv`.
#[derive(Debug, Clone)]
pub struct SysfsTransport {
    sysfs_dir: PathBuf,
    lock_path: PathBuf,
    lock_timeout: Duration,
}

impl Default for SysfsTransport {
    fn default() -> Self {
        Self::new(PathBuf::from(DEFAULT_SYSFS_PATH))
    }
}

impl SysfsTransport {
    pub fn new(sysfs_dir: PathBuf) -> Self {
        let lock_path = if Path::new("/run/lock").exists() {
            PathBuf::from(DEFAULT_LOCK_PATH)
        } else {
            PathBuf::from(FALLBACK_LOCK_PATH)
        };
        Self {
            sysfs_dir,
            lock_path,
            lock_timeout: Duration::from_secs(5),
        }
    }

    pub fn with_lock_path(mut self, lock_path: PathBuf) -> Self {
        self.lock_path = lock_path;
        self
    }

    pub fn sysfs_dir(&self) -> &Path {
        &self.sysfs_dir
    }

    fn ensure_loaded(&self) -> Result<()> {
        if !self.sysfs_dir.exists() {
            return Err(Error::DriverNotLoaded {
                path: self.sysfs_dir.display().to_string(),
            });
        }
        Ok(())
    }

    fn acquire_lock(&self) -> Result<LockGuard> {
        let open_res = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(&self.lock_path);

        let file = match open_res {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                // Fallback to /tmp if non-root can't open /run/lock
                OpenOptions::new()
                    .create(true)
                    .read(true)
                    .write(true)
                    .open(FALLBACK_LOCK_PATH)
                    .map_err(|e2| Error::LockFailed {
                        path: FALLBACK_LOCK_PATH.into(),
                        reason: e2.to_string(),
                    })?
            }
            Err(e) => {
                return Err(Error::LockFailed {
                    path: self.lock_path.display().to_string(),
                    reason: e.to_string(),
                });
            }
        };

        let deadline = Instant::now() + self.lock_timeout;
        loop {
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc == 0 {
                return Ok(LockGuard { file });
            }
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::EWOULDBLOCK)
                && err.raw_os_error() != Some(libc::EAGAIN)
            {
                return Err(Error::LockFailed {
                    path: self.lock_path.display().to_string(),
                    reason: err.to_string(),
                });
            }
            if Instant::now() >= deadline {
                return Err(Error::LockFailed {
                    path: self.lock_path.display().to_string(),
                    reason: format!("timed out after {:?}", self.lock_timeout),
                });
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn map_io_err(path: &Path, source: std::io::Error) -> Error {
        if source.kind() == std::io::ErrorKind::PermissionDenied {
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

    fn read_bytes(&self, rel: &str) -> Result<Vec<u8>> {
        let p = self.sysfs_dir.join(rel);
        fs::read(&p).map_err(|e| Self::map_io_err(&p, e))
    }

    fn read_string(&self, rel: &str) -> Result<String> {
        let p = self.sysfs_dir.join(rel);
        fs::read_to_string(&p)
            .map(|s| s.trim().to_string())
            .map_err(|e| Self::map_io_err(&p, e))
    }

    fn write_exact(&self, rel: &str, data: &[u8]) -> Result<()> {
        let p = self.sysfs_dir.join(rel);
        let mut f = OpenOptions::new()
            .write(true)
            .open(&p)
            .map_err(|e| Self::map_io_err(&p, e))?;
        f.write_all(data).map_err(|e| Self::map_io_err(&p, e))?;
        Ok(())
    }

    fn read_smn_unlocked(&self, addr: u32) -> Result<u32> {
        // SAFETY: Writing EXACTLY 4 bytes to `smn` triggers `smu_read_address` (READ-ONLY)
        // in `ryzen_smu/drv.c:243`. We NEVER write 8 bytes (which would write SMN).
        let addr_bytes = addr.to_le_bytes();
        debug_assert_eq!(addr_bytes.len(), 4);
        self.write_exact("smn", &addr_bytes)?;

        let p = self.sysfs_dir.join("smn");
        let mut buf = [0u8; 4];
        let mut f = File::open(&p).map_err(|e| Self::map_io_err(&p, e))?;
        f.read_exact(&mut buf).map_err(|e| Self::map_io_err(&p, e))?;
        Ok(u32::from_le_bytes(buf))
    }
}

struct LockGuard {
    file: File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        unsafe {
            libc::flock(self.file.as_raw_fd(), libc::LOCK_UN);
        }
    }
}

impl SmuTransport for SysfsTransport {
    fn read_info(&self) -> Result<DriverSysfsInfo> {
        self.ensure_loaded()?;

        let drv_version = self.read_string("drv_version")?;
        let smu_version = self.read_string("version")?;
        let codename_str = self.read_string("codename")?;
        let codename_id = codename_str.parse::<u32>().map_err(|e| {
            Error::InvalidInput(format!("invalid codename '{codename_str}': {e}"))
        })?;
        let mp1_str = self.read_string("mp1_if_version")?;
        let mp1_if_version = mp1_str.parse::<u32>().map_err(|e| {
            Error::InvalidInput(format!("invalid mp1_if_version '{mp1_str}': {e}"))
        })?;

        let has_rsmu = self.sysfs_dir.join("rsmu_cmd").exists();
        let has_pm_table = self.sysfs_dir.join("pm_table").exists();

        let pm_table_version = if self.sysfs_dir.join("pm_table_version").exists() {
            let b = self.read_bytes("pm_table_version")?;
            if b.len() >= 4 {
                Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            } else {
                None
            }
        } else {
            None
        };

        let pm_table_size = if self.sysfs_dir.join("pm_table_size").exists() {
            let b = self.read_bytes("pm_table_size")?;
            if b.len() >= 8 {
                Some(u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]) as usize)
            } else if b.len() >= 4 {
                Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            } else {
                None
            }
        } else {
            None
        };

        Ok(DriverSysfsInfo {
            drv_version,
            smu_version,
            codename_id,
            mp1_if_version,
            has_rsmu,
            has_pm_table,
            pm_table_version,
            pm_table_size,
        })
    }

    fn read_pm_table(&self) -> Result<Vec<u8>> {
        self.ensure_loaded()?;
        let info = self.read_info()?;
        if let Some(sz) = info.pm_table_size {
            if sz > MAX_SAFE_PM_TABLE_SIZE {
                return Err(Error::PmTableTooLarge { size: sz });
            }
        }
        self.read_bytes("pm_table")
    }

    fn send_command(
        &self,
        mailbox: Mailbox,
        opcode: u32,
        args: SmuArgs,
        verify_rsp_smn_addr: Option<u32>,
    ) -> Result<MailboxResponse> {
        self.ensure_loaded()?;
        if opcode == 0 || opcode > 0xFF {
            return Err(Error::InvalidInput(format!(
                "SMU opcode 0x{opcode:X} is out of valid range 0x01..=0xFF"
            )));
        }

        // Hold exclusive cross-process lock across the full 4-step sysfs transaction.
        let _lock = self.acquire_lock()?;

        // Step 1: Write all 24 bytes (6 x u32 LE) to smu_args.
        let mut arg_buf = [0u8; 24];
        for (i, w) in args.iter().enumerate() {
            arg_buf[i * 4..(i + 1) * 4].copy_from_slice(&w.to_le_bytes());
        }
        self.write_exact("smu_args", &arg_buf)?;

        // Step 2: Write 4-byte LE opcode to rsmu_cmd / mp1_smu_cmd.
        let cmd_file = mailbox.sysfs_file();
        self.write_exact(cmd_file, &opcode.to_le_bytes())?;

        // Step 3: Read 4-byte LE driver response code.
        let rsp_bytes = self.read_bytes(cmd_file)?;
        if rsp_bytes.len() < 4 {
            return Err(Error::InvalidInput(format!(
                "short read ({} bytes) from {cmd_file}",
                rsp_bytes.len()
            )));
        }
        let raw_drv_rsp = u32::from_le_bytes([rsp_bytes[0], rsp_bytes[1], rsp_bytes[2], rsp_bytes[3]]);
        let driver_status = SmuStatus::from_raw(raw_drv_rsp).unwrap_or(SmuStatus::Failed);

        // Step 4: Read 24-byte LE output args from smu_args.
        let out_bytes = self.read_bytes("smu_args")?;
        if out_bytes.len() < 24 {
            return Err(Error::InvalidInput(format!(
                "short read ({} bytes) from smu_args",
                out_bytes.len()
            )));
        }
        let mut out_args = [0u32; 6];
        for i in 0..6 {
            out_args[i] = u32::from_le_bytes([
                out_bytes[i * 4],
                out_bytes[i * 4 + 1],
                out_bytes[i * 4 + 2],
                out_bytes[i * 4 + 3],
            ]);
        }

        // Step 5 (Safety Defense against ryzen_smu smu.c:213 false-OK bug):
        // Read the true hardware RSP register via read-only 4-byte SMN query while still locked.
        let hw_status = if let Some(rsp_addr) = verify_rsp_smn_addr {
            if self.sysfs_dir.join("smn").exists() {
                match self.read_smn_unlocked(rsp_addr) {
                    Ok(raw_hw) => SmuStatus::from_raw(raw_hw).ok(),
                    Err(_) => None,
                }
            } else {
                None
            }
        } else {
            None
        };

        Ok(MailboxResponse {
            driver_status,
            hw_status,
            args: out_args,
        })
    }

    fn read_smn(&self, addr: u32) -> Result<u32> {
        self.ensure_loaded()?;
        let _lock = self.acquire_lock()?;
        self.read_smn_unlocked(addr)
    }
}
