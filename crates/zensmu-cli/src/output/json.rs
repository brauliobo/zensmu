//! JSON serialization and error formatting helpers.

use super::types::ErrorOutput;
use serde::Serialize;
use zensmu_core::error::Error;

pub fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("failed to serialize JSON output: {e}"),
    }
}

pub fn print_json_compact<T: Serialize>(value: &T) {
    match serde_json::to_string(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("failed to serialize JSON output: {e}"),
    }
}

pub fn error_kind(err: &Error) -> &'static str {
    match err {
        Error::DriverNotLoaded { .. } => "driver_not_loaded",
        Error::PermissionDenied { .. } => "permission_denied",
        Error::LockFailed { .. } => "lock_failed",
        Error::UnsupportedCpu { .. } => "unsupported_cpu",
        Error::IdentityMismatch { .. } => "identity_mismatch",
        Error::UnsupportedCapability { .. } => "unsupported_capability",
        Error::SmuRejected { .. } => "smu_rejected",
        Error::MailboxUnhealthy { .. } => "mailbox_unhealthy",
        Error::VerificationFailed { .. } => "verification_failed",
        Error::HardwareClamped { .. } => "hardware_clamped",
        Error::OutOfBounds { .. } => "out_of_bounds",
        Error::ExtremeValueBlocked { .. } => "extreme_value_blocked",
        Error::UnsupportedPmTable { .. } => "unsupported_pm_table",
        Error::PmTableTooLarge { .. } => "pm_table_too_large",
        Error::InvalidInput(_) => "invalid_input",
        Error::BootGuardTripped { .. } => "boot_guard_tripped",
        Error::ProfileTargetMismatch { .. } => "profile_target_mismatch",
        Error::Io { .. } => "io",
        Error::Serde(_) => "serde",
    }
}

pub fn print_error(err: &Error, json: bool) {
    let hint = match err {
        Error::DriverNotLoaded { path } => Some(zensmu_core::driver::driver_remediation_hint(
            std::path::Path::new(path),
        )),
        _ => err.report_hint().map(str::to_string),
    };
    if json {
        let payload = ErrorOutput {
            ok: false,
            kind: error_kind(err),
            error: err.to_string(),
            hint,
        };
        if let Ok(s) = serde_json::to_string(&payload) {
            eprintln!("{s}");
        } else {
            eprintln!("error: {err}");
        }
    } else {
        eprintln!("error: {err}");
        if let Some(hint) = hint {
            eprintln!("hint: {hint}");
        }
    }
}
