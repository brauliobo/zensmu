use serde::{Deserialize, Serialize};
use std::fs;
use std::process::Command;

/// EFI variable path exposing UEFI Secure Boot state (5th byte: 1 = enabled, 0 = disabled).
pub const SECURE_BOOT_EFIVAR_PATH: &str =
    "/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c";

/// Linux Security Module kernel lockdown mode path (bracketed token is active).
pub const KERNEL_LOCKDOWN_PATH: &str = "/sys/kernel/security/lockdown";

/// UEFI Secure Boot and Linux kernel lockdown inspection state.
///
/// Out-of-tree DKMS modules such as `ryzen_smu` cannot be loaded unsigned when
/// Secure Boot is enabled or kernel lockdown is in `integrity`/`confidentiality` mode.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecureBootStatus {
    #[serde(default)]
    pub secure_boot_enabled: Option<bool>,
    #[serde(default)]
    pub lockdown_mode: Option<String>,
    #[serde(default)]
    pub blocks_unsigned_modules: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Parse raw bytes from `/sys/firmware/efi/efivars/SecureBoot-8be4df61-93ca-11d2-aa0d-00e098032b8c`.
///
/// EFI efivars begin with a 4-byte little-endian attribute header followed by the payload;
/// the 5th byte (`bytes[4]`) is `1` when Secure Boot is enabled and `0` when disabled.
pub fn parse_secure_boot_efivar(bytes: &[u8]) -> Option<bool> {
    match bytes.get(4) {
        Some(1) => Some(true),
        Some(0) => Some(false),
        _ => None,
    }
}

/// Parse stdout from `mokutil --sb-state` as a fallback when the efivar cannot be read.
pub fn parse_mokutil_sb_state(output: &str) -> Option<bool> {
    for line in output.lines() {
        let lower = line.trim().to_ascii_lowercase();
        if lower.contains("secureboot enabled") {
            return Some(true);
        }
        if lower.contains("secureboot disabled") {
            return Some(false);
        }
    }
    None
}

/// Parse `/sys/kernel/security/lockdown` and return the bracketed active mode
/// (e.g. `"none"`, `"integrity"`, or `"confidentiality"`).
pub fn parse_lockdown_mode(content: &str) -> Option<String> {
    let start = content.find('[')?;
    let rest = content.get(start + 1..)?;
    let end = rest.find(']')?;
    let mode = rest.get(..end)?.trim();
    if mode.is_empty() {
        None
    } else {
        Some(mode.to_ascii_lowercase())
    }
}

/// Build a [`SecureBootStatus`] from already-parsed Secure Boot and lockdown signals.
pub fn secure_boot_status_from(
    secure_boot_enabled: Option<bool>,
    lockdown_mode: Option<String>,
) -> SecureBootStatus {
    let lockdown_blocks = matches!(
        lockdown_mode.as_deref(),
        Some("integrity" | "confidentiality")
    );
    let blocks_unsigned_modules = secure_boot_enabled == Some(true) || lockdown_blocks;

    let hint = if blocks_unsigned_modules {
        match (secure_boot_enabled, lockdown_mode.as_deref()) {
            (Some(true), Some(mode @ ("integrity" | "confidentiality"))) => Some(format!(
                "UEFI Secure Boot is enabled (kernel lockdown: {mode}), which blocks unsigned out-of-tree modules like ryzen_smu. Sign the DKMS module with a Machine Owner Key (enroll via `sudo mokutil --import /var/lib/dkms/mok.pub` and reboot) or disable Secure Boot in UEFI firmware."
            )),
            (Some(true), _) => Some(
                "UEFI Secure Boot is enabled, which blocks unsigned out-of-tree modules like ryzen_smu. Sign the DKMS module with a Machine Owner Key (enroll via `sudo mokutil --import /var/lib/dkms/mok.pub` and reboot) or disable Secure Boot in UEFI firmware.".to_string(),
            ),
            (_, Some(mode)) => Some(format!(
                "Kernel lockdown mode is `{mode}`, which blocks unsigned out-of-tree modules like ryzen_smu. Sign the DKMS module with an enrolled MOK (`sudo mokutil --import /var/lib/dkms/mok.pub`) or disable Secure Boot / kernel lockdown."
            )),
            _ => None,
        }
    } else {
        None
    };

    SecureBootStatus {
        secure_boot_enabled,
        lockdown_mode,
        blocks_unsigned_modules,
        hint,
    }
}

fn run_mokutil_sb_state() -> Option<bool> {
    for bin in ["mokutil", "/usr/bin/mokutil", "/sbin/mokutil"] {
        if let Ok(out) = Command::new(bin).arg("--sb-state").output() {
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if let Some(state) = parse_mokutil_sb_state(&stdout) {
                    return Some(state);
                }
            }
            return None;
        }
    }
    None
}

/// Detect UEFI Secure Boot and Linux kernel lockdown status without requiring root.
pub fn detect_secure_boot() -> SecureBootStatus {
    let secure_boot_enabled = fs::read(SECURE_BOOT_EFIVAR_PATH)
        .ok()
        .and_then(|bytes| parse_secure_boot_efivar(&bytes))
        .or_else(run_mokutil_sb_state);

    let lockdown_mode = fs::read_to_string(KERNEL_LOCKDOWN_PATH)
        .ok()
        .and_then(|content| parse_lockdown_mode(&content));

    secure_boot_status_from(secure_boot_enabled, lockdown_mode)
}
