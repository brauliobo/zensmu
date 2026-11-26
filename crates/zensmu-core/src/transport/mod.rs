pub mod mock;
pub mod sysfs;

use crate::error::{Result, SmuStatus};
use crate::types::{Mailbox, SmuArgs};

/// Metadata read from read-only sysfs attributes (unprivileged, `0444`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DriverSysfsInfo {
    pub drv_version: String,
    pub smu_version: String,
    pub codename_id: u32,
    pub mp1_if_version: u32,
    pub has_rsmu: bool,
    pub has_pm_table: bool,
    pub pm_table_version: Option<u32>,
    pub pm_table_size: Option<usize>,
}

/// Raw result of a single SMU mailbox command execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxResponse {
    /// Status returned by `ryzen_smu`'s `*_cmd` file.
    pub driver_status: SmuStatus,
    /// Optional true hardware RSP status read via read-only SMN register check
    /// (defeats the `smu.c:213` false-OK bug in `ryzen_smu 0.1.7`).
    pub hw_status: Option<SmuStatus>,
    /// Output arguments read back from `smu_args`.
    pub args: SmuArgs,
}

impl MailboxResponse {
    /// Effective status: prefers verified hardware RSP status over driver status.
    pub fn effective_status(&self) -> SmuStatus {
        self.hw_status.unwrap_or(self.driver_status)
    }
}

/// Hardware transport abstraction for `ryzen_smu` sysfs access.
///
/// Implementations MUST ensure:
/// - Exclusive transaction locking across `write(smu_args) -> write(cmd) -> read(rsp) -> read(smu_args)`.
/// - All 6 `u32` arguments (24 bytes) are written before every command.
/// - Read-only SMN reads only (4-byte writes to `smn`); NEVER arbitrary SMN writes.
pub trait SmuTransport: Send + Sync {
    /// Read unprivileged (`0444`) driver metadata files.
    fn read_info(&self) -> Result<DriverSysfsInfo>;

    /// Read raw PM table bytes (`0444`, unprivileged).
    fn read_pm_table(&self) -> Result<Vec<u8>>;

    /// Execute an SMU mailbox command under an exclusive transaction lock.
    /// If `verify_rsp_smn_addr` is `Some(addr)`, also reads that SMN register
    /// (read-only 4-byte SMN read) inside the same lock to get the true hardware RSP.
    fn send_command(
        &self,
        mailbox: Mailbox,
        opcode: u32,
        args: SmuArgs,
        verify_rsp_smn_addr: Option<u32>,
    ) -> Result<MailboxResponse>;

    /// Perform a read-only 32-bit SMN register read (4-byte write to `smn` + 4-byte read).
    fn read_smn(&self, addr: u32) -> Result<u32>;
}
