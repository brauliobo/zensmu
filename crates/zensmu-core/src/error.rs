use thiserror::Error;

/// SMU hardware return codes (see ryzen_smu `smu.h`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[repr(u32)]
pub enum SmuStatus {
    Ok = 0x01,
    Failed = 0xFF,
    UnknownCmd = 0xFE,
    CmdRejectedPrereq = 0xFD,
    CmdRejectedBusy = 0xFC,
    CommandTimeout = 0xFB,
    InvalidArgument = 0xFA,
    Unsupported = 0xF9,
    InsufficientSize = 0xF8,
    MappedError = 0xF7,
    PciFailed = 0xF6,
}

impl SmuStatus {
    pub fn from_raw(code: u32) -> std::result::Result<Self, u32> {
        match code {
            0x01 => Ok(Self::Ok),
            0xFF => Ok(Self::Failed),
            0xFE => Ok(Self::UnknownCmd),
            0xFD => Ok(Self::CmdRejectedPrereq),
            0xFC => Ok(Self::CmdRejectedBusy),
            0xFB => Ok(Self::CommandTimeout),
            0xFA => Ok(Self::InvalidArgument),
            0xF9 => Ok(Self::Unsupported),
            0xF8 => Ok(Self::InsufficientSize),
            0xF7 => Ok(Self::MappedError),
            0xF6 => Ok(Self::PciFailed),
            other => Err(other),
        }
    }

    pub fn is_ok(self) -> bool {
        matches!(self, Self::Ok)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "OK",
            Self::Failed => "FAILED (0xFF)",
            Self::UnknownCmd => "UNKNOWN_CMD (0xFE)",
            Self::CmdRejectedPrereq => "CMD_REJECTED_PREREQ (0xFD)",
            Self::CmdRejectedBusy => "CMD_REJECTED_BUSY (0xFC)",
            Self::CommandTimeout => "COMMAND_TIMEOUT (0xFB)",
            Self::InvalidArgument => "INVALID_ARGUMENT (0xFA)",
            Self::Unsupported => "UNSUPPORTED (0xF9)",
            Self::InsufficientSize => "INSUFFICIENT_SIZE (0xF8)",
            Self::MappedError => "MAPPED_ERROR (0xF7)",
            Self::PciFailed => "PCI_FAILED (0xF6)",
        }
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("ryzen_smu sysfs not found at {path} (is the ryzen_smu kernel module loaded? try: sudo modprobe ryzen_smu)")]
    DriverNotLoaded { path: String },

    #[error("permission denied accessing {path} (mailbox operations require root / sudo or pkexec)")]
    PermissionDenied { path: String },

    #[error("failed to acquire exclusive SMU transaction lock {path}: {reason}")]
    LockFailed { path: String, reason: String },

    #[error("unsupported CPU codename: {codename:?} ({detail})")]
    UnsupportedCpu { codename: String, detail: String },

    #[error("CPU identity mismatch: ryzen_smu reports {driver_codename} but CPUID reports family 0x{family:X} model 0x{model:X}")]
    IdentityMismatch {
        driver_codename: String,
        family: u32,
        model: u32,
    },

    #[error("capability '{capability}' is not supported on {codename}")]
    UnsupportedCapability {
        capability: &'static str,
        codename: String,
    },

    #[error("SMU rejected command 0x{opcode:02X} on {mailbox}: {status}")]
    SmuRejected {
        mailbox: &'static str,
        opcode: u32,
        status: String,
    },

    #[error("SMU mailbox health check failed on {mailbox}: expected {expected:#X}, got {actual:#X}")]
    MailboxUnhealthy {
        mailbox: &'static str,
        expected: u32,
        actual: u32,
    },

    #[error("read-after-write verification failed for {field}: requested {expected}, hardware reports {actual}")]
    VerificationFailed {
        field: String,
        expected: String,
        actual: String,
    },

    #[error("value out of safe bounds for {field}: {value} (allowed: {bounds})")]
    OutOfBounds {
        field: String,
        value: String,
        bounds: String,
    },

    #[error("value {value} for {field} exceeds conservative safety threshold ({threshold}); pass --allow-extreme to override")]
    ExtremeValueBlocked {
        field: String,
        value: String,
        threshold: String,
    },

    #[error("unsupported PM table version 0x{version:08X} (size 0x{size:X}) for {codename}")]
    UnsupportedPmTable {
        codename: String,
        version: u32,
        size: usize,
    },

    #[error("PM table size {size} exceeds safe sysfs PAGE_SIZE limit (4096 bytes)")]
    PmTableTooLarge { size: usize },

    #[error("invalid argument: {0}")]
    InvalidInput(String),

    #[error("boot-loop safety guard triggered: previous apply did not finish cleanly ({sentinel}); refusing to auto-apply")]
    BootGuardTripped { sentinel: String },

    #[error("profile target mismatch: profile targets {expected}, running on {actual}")]
    ProfileTargetMismatch { expected: String, actual: String },

    #[error("I/O error on {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },

    #[error("serialization error: {0}")]
    Serde(String),
}

pub type Result<T> = std::result::Result<T, Error>;
