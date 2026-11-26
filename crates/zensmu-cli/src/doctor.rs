//! Safety diagnostics (`zensmu doctor`) inspecting driver, sysfs permissions,
//! `smu.c:213` false-OK bug mitigation, CPUID consistency, PM table layout support,
//! boot-loop guard state, conflicting tools, and live mailbox health.

use serde::Serialize;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use zensmu_core::driver::{detect_secure_boot, SecureBootStatus};
use zensmu_core::error::Error;
use zensmu_core::hw::identity::{
    detect_hardware, driver_has_false_ok_bug, read_host_cpuid, verify_codename_against_cpuid,
    HardwareContext,
};
use zensmu_core::ops::co::check_mailbox_health;
use zensmu_core::ops::pm_table::{self, MAX_SAFE_PM_TABLE_SIZE};
use zensmu_core::safety::guard::BootLoopGuard;
use zensmu_core::transport::{DriverSysfsInfo, SmuTransport};
use zensmu_core::types::{Codename, Mailbox};

const CONFLICTING_TOOLS: &[(&str, &str)] = &[
    (
        "/usr/bin/ruv-gui",
        "ruv-gui writes to ryzen_smu sysfs without zensmu's flock transaction lock; do not run concurrently with zensmu",
    ),
    (
        "/usr/local/bin/ruv-gui",
        "ruv-gui writes to ryzen_smu sysfs without zensmu's flock transaction lock; do not run concurrently with zensmu",
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    Pass,
    Warn,
    Fail,
    Skip,
}

impl CheckStatus {
    pub fn tag(self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Warn => "WARN",
            Self::Fail => "FAIL",
            Self::Skip => "SKIP",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorCheck {
    pub id: String,
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl DoctorCheck {
    fn new(
        id: &str,
        name: &str,
        status: CheckStatus,
        message: impl Into<String>,
        remediation: Option<&str>,
    ) -> Self {
        Self {
            id: id.into(),
            name: name.into(),
            status,
            message: message.into(),
            remediation: remediation.map(Into::into),
        }
    }

    fn pass(id: &str, name: &str, message: impl Into<String>) -> Self {
        Self::new(id, name, CheckStatus::Pass, message, None)
    }

    fn warn(id: &str, name: &str, message: impl Into<String>, remediation: Option<&str>) -> Self {
        Self::new(id, name, CheckStatus::Warn, message, remediation)
    }

    fn fail(id: &str, name: &str, message: impl Into<String>, remediation: Option<&str>) -> Self {
        Self::new(id, name, CheckStatus::Fail, message, remediation)
    }

    fn skip(id: &str, name: &str, message: impl Into<String>, remediation: Option<&str>) -> Self {
        Self::new(id, name, CheckStatus::Skip, message, remediation)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct DoctorReport {
    pub healthy: bool,
    pub is_root: bool,
    pub sysfs_path: String,
    pub guard_path: String,
    pub checks: Vec<DoctorCheck>,
}

pub fn is_effective_root() -> bool {
    unsafe { libc::geteuid() == 0 }
}

/// Run the complete diagnostic suite without aborting on individual failures.
pub fn run_doctor(
    transport: &dyn SmuTransport,
    sysfs_path: &Path,
    guard_path: &Path,
    no_cpuid_check: bool,
) -> DoctorReport {
    let is_root = is_effective_root();
    let mut checks = Vec::new();

    // 1. ryzen_smu loaded & readable
    let (driver_check, driver_info) = check_driver_loaded(transport, sysfs_path);
    let driver_loaded = driver_info.is_some();
    checks.push(driver_check);

    // 2. UEFI Secure Boot / kernel lockdown status
    checks.push(check_secure_boot(driver_loaded));

    // 3. Sysfs permissions
    checks.push(check_sysfs_permissions(sysfs_path, is_root));

    // 3. drv_version / smu.c:213 false-OK bug check
    checks.push(check_false_ok_bug(driver_info.as_ref(), sysfs_path));

    // 4. CPUID consistency
    checks.push(check_cpuid_consistency(
        driver_info.as_ref(),
        no_cpuid_check,
    ));

    // Resolve full HardwareContext (for PM table & mailbox health checks)
    let hw_res = detect_hardware(transport, !no_cpuid_check);
    let hw_opt = match &hw_res {
        Ok(hw) => Some(hw),
        Err(Error::UnsupportedCpu { codename, detail }) => {
            checks.push(DoctorCheck::fail(
                "capability_profile",
                "CPU capability profile",
                format!("unsupported CPU {codename}: {detail}"),
                Some("zensmu refuses unverified CPUs rather than guessing SMU opcodes; help add support with: zensmu report --raw-pm-table --open"),
            ));
            None
        }
        Err(_) => None,
    };

    // 5. PM table layout support
    checks.push(check_pm_table(transport, hw_opt));

    // 6. Boot-loop guard status
    checks.push(check_boot_guard(guard_path));

    // 7. Conflicting tools check (/usr/bin/ruv-gui etc.)
    checks.push(check_conflicting_tools());

    // 8. Live TestMessage (0x01) mailbox health checks on RSMU & MP1 (if root)
    check_mailbox_health_all(transport, hw_opt, is_root, &mut checks);

    let healthy = !checks.iter().any(|c| c.status == CheckStatus::Fail);

    DoctorReport {
        healthy,
        is_root,
        sysfs_path: sysfs_path.display().to_string(),
        guard_path: guard_path.display().to_string(),
        checks,
    }
}

fn check_driver_loaded(
    transport: &dyn SmuTransport,
    sysfs_path: &Path,
) -> (DoctorCheck, Option<DriverSysfsInfo>) {
    match transport.read_info() {
        Ok(info) => {
            let check = DoctorCheck::pass(
                "driver_loaded",
                "ryzen_smu kernel module",
                format!(
                    "loaded at {} (drv {}, SMU {}, mp1_if={})",
                    sysfs_path.display(),
                    info.drv_version,
                    info.smu_version,
                    info.mp1_if_version
                ),
            );
            (check, Some(info))
        }
        Err(Error::DriverNotLoaded { path }) => {
            let hint = zensmu_core::driver::driver_remediation_hint(sysfs_path);
            (
                DoctorCheck::fail(
                    "driver_loaded",
                    "ryzen_smu kernel module",
                    format!("sysfs directory not found at {path}"),
                    Some(hint.as_str()),
                ),
                None,
            )
        }
        Err(err) => (
            DoctorCheck::fail(
                "driver_loaded",
                "ryzen_smu kernel module",
                format!("failed to read ryzen_smu metadata: {err}"),
                Some("Verify ryzen_smu is properly installed and check dmesg for errors"),
            ),
            None,
        ),
    }
}

fn check_secure_boot(driver_loaded: bool) -> DoctorCheck {
    let status = detect_secure_boot();
    check_secure_boot_with(&status, driver_loaded)
}

fn check_secure_boot_with(status: &SecureBootStatus, driver_loaded: bool) -> DoctorCheck {
    const ID: &str = "secure_boot";
    const NAME: &str = "Secure Boot / kernel lockdown";

    let sb_label = match status.secure_boot_enabled {
        Some(true) => "enabled",
        Some(false) => "disabled",
        None => "unknown",
    };
    let lockdown_label = status.lockdown_mode.as_deref().unwrap_or("none");

    if status.blocks_unsigned_modules {
        if driver_loaded {
            DoctorCheck::pass(
                ID,
                NAME,
                format!(
                    "Secure Boot / lockdown active (Secure Boot={sb_label}, lockdown={lockdown_label}); ryzen_smu is loaded/signed"
                ),
            )
        } else {
            let fallback = "Sign the ryzen_smu DKMS module with an enrolled MOK (`sudo mokutil --import /var/lib/dkms/mok.pub`) or disable Secure Boot in UEFI firmware";
            DoctorCheck::warn(
                ID,
                NAME,
                format!(
                    "Secure Boot / lockdown active (Secure Boot={sb_label}, lockdown={lockdown_label}) and ryzen_smu is not loaded; unsigned out-of-tree modules are blocked"
                ),
                Some(status.hint.as_deref().unwrap_or(fallback)),
            )
        }
    } else if status.secure_boot_enabled == Some(false) && lockdown_label == "none" {
        DoctorCheck::pass(ID, NAME, "Secure Boot disabled / kernel lockdown=none")
    } else {
        DoctorCheck::pass(
            ID,
            NAME,
            format!(
                "Secure Boot={sb_label} / kernel lockdown={lockdown_label} (unsigned modules permitted)"
            ),
        )
    }
}

fn check_sysfs_permissions(sysfs_path: &Path, is_root: bool) -> DoctorCheck {
    const ID: &str = "sysfs_permissions";
    const NAME: &str = "Sysfs permissions";

    if !sysfs_path.exists() {
        return DoctorCheck::skip(
            ID,
            NAME,
            format!("skipped: {} does not exist", sysfs_path.display()),
            None,
        );
    }

    let mut details = Vec::new();
    for file in [
        "drv_version",
        "pm_table",
        "smu_args",
        "rsmu_cmd",
        "mp1_smu_cmd",
        "smn",
    ] {
        let p = sysfs_path.join(file);
        if let Ok(meta) = fs::metadata(&p) {
            let mode = meta.permissions().mode() & 0o777;
            details.push(format!("{file}=0{mode:03o}"));
        }
    }
    let mode_summary = if details.is_empty() {
        "no attributes inspected".to_string()
    } else {
        details.join(", ")
    };

    if is_root {
        DoctorCheck::pass(
            ID,
            NAME,
            format!("running as root; mailbox + telemetry accessible ({mode_summary})"),
        )
    } else {
        DoctorCheck::warn(
            ID,
            NAME,
            format!(
                "running unprivileged (UID {}); read-only telemetry works, mailbox read/write requires root ({mode_summary})",
                unsafe { libc::geteuid() }
            ),
            Some("Run mailbox commands via sudo or pkexec (telemetry/info/top work unprivileged)"),
        )
    }
}

fn check_false_ok_bug(driver_info: Option<&DriverSysfsInfo>, sysfs_path: &Path) -> DoctorCheck {
    const ID: &str = "drv_false_ok_bug";
    const NAME: &str = "ryzen_smu smu.c:213 false-OK bug";

    let Some(info) = driver_info else {
        return DoctorCheck::skip(ID, NAME, "skipped: driver version unavailable", None);
    };

    if driver_has_false_ok_bug(&info.drv_version) {
        let has_smn = sysfs_path.join("smn").exists();
        DoctorCheck::warn(
            ID,
            NAME,
            format!(
                "ryzen_smu {} is affected by upstream smu.c:213 false-OK bug (driver may report OK 0x01 on rejected SMU commands); zensmu actively mitigates this via read-only SMN RSP register verification ({}) + read-after-write verification",
                info.drv_version,
                if has_smn { "smn present" } else { "smn missing!" }
            ),
            None,
        )
    } else {
        DoctorCheck::pass(
            ID,
            NAME,
            format!(
                "ryzen_smu {} is not in the known smu.c:213 affected list (zensmu still enforces SMN RSP + read-after-write verification)",
                info.drv_version
            ),
        )
    }
}

fn check_cpuid_consistency(
    driver_info: Option<&DriverSysfsInfo>,
    no_cpuid_check: bool,
) -> DoctorCheck {
    const ID: &str = "cpuid_consistency";
    const NAME: &str = "CPUID identity cross-check";

    let host_cpuid = read_host_cpuid();
    let Some(info) = driver_info else {
        return DoctorCheck::skip(ID, NAME, "skipped: driver metadata unavailable", None);
    };

    let Some(codename) = Codename::from_driver_id(info.codename_id) else {
        return DoctorCheck::fail(
            ID,
            NAME,
            format!("unrecognized driver codename id={}", info.codename_id),
            Some("Update zensmu, check ryzen_smu version, or file a report with: zensmu report --raw-pm-table --open"),
        );
    };

    if no_cpuid_check {
        return DoctorCheck::warn(
            ID,
            NAME,
            format!(
                "skipped via --no-cpuid-check (driver codename: {})",
                codename.display_name()
            ),
            None,
        );
    }

    let Some(ref cpuid) = host_cpuid else {
        return DoctorCheck::skip(
            ID,
            NAME,
            "host CPUID unavailable on this architecture",
            None,
        );
    };

    match verify_codename_against_cpuid(codename, cpuid) {
        Ok(()) => DoctorCheck::pass(
            ID,
            NAME,
            format!(
                "{} matches host CPUID ({}, family 0x{:X}, model 0x{:X}, stepping {})",
                codename.display_name(),
                cpuid.brand,
                cpuid.family,
                cpuid.model,
                cpuid.stepping
            ),
        ),
        Err(err) => DoctorCheck::fail(
            ID,
            NAME,
            err.to_string(),
            Some("Do not write SMU commands when driver codename disagrees with host CPUID; file a report with: zensmu report --raw-pm-table --open"),
        ),
    }
}

fn check_pm_table(transport: &dyn SmuTransport, hw_opt: Option<&HardwareContext>) -> DoctorCheck {
    const ID: &str = "pm_table";
    const NAME: &str = "PM table telemetry layout";

    let Some(hw) = hw_opt else {
        return DoctorCheck::skip(
            ID,
            NAME,
            "skipped: verified hardware context unavailable",
            None,
        );
    };

    if !hw.driver.has_pm_table {
        return DoctorCheck::warn(
            ID,
            NAME,
            "ryzen_smu did not expose pm_table for this platform",
            None,
        );
    }
    if let Some(sz) = hw
        .driver
        .pm_table_size
        .filter(|&s| s > MAX_SAFE_PM_TABLE_SIZE)
    {
        return DoctorCheck::fail(
            ID,
            NAME,
            format!(
                "pm_table_size ({sz} bytes) exceeds safe Linux sysfs PAGE_SIZE limit ({MAX_SAFE_PM_TABLE_SIZE} bytes)"
            ),
            Some("zensmu refuses oversized PM table reads to avoid kernel sysfs buffer bugs"),
        );
    }

    match pm_table::read_telemetry(transport, hw) {
        Ok(snap) => DoctorCheck::pass(
            ID,
            NAME,
            format!(
                "verified layout 0x{:08X} ({} / 0x{:X} bytes) decoded cleanly for {}",
                snap.table_version,
                snap.table_size,
                snap.table_size,
                snap.codename.display_name()
            ),
        ),
        Err(Error::UnsupportedPmTable {
            codename,
            version,
            size,
        }) => DoctorCheck::warn(
            ID,
            NAME,
            format!(
                "PM table version 0x{version:08X} (size 0x{size:X}) is not in zensmu's verified allowlist for {codename}"
            ),
            Some("Help add support for this PM table layout: run `zensmu report --raw-pm-table --open`"),
        ),
        Err(err) => DoctorCheck::fail(
            ID,
            NAME,
            format!("failed to read/decode PM table: {err}"),
            None,
        ),
    }
}

fn check_boot_guard(guard_path: &Path) -> DoctorCheck {
    const ID: &str = "boot_guard";
    const NAME: &str = "Boot-loop crash guard";

    let guard = BootLoopGuard::new(guard_path.to_path_buf());
    if guard.is_tripped() {
        let detail = match guard.read_sentinel() {
            Ok(Some(info)) => format!(
                "TRIPPED at {} (profile='{}', armed_unix_secs={}, pid={})",
                guard_path.display(),
                info.profile,
                info.armed_unix_secs,
                info.pid
            ),
            _ => format!(
                "TRIPPED at {} (sentinel file present)",
                guard_path.display()
            ),
        };
        DoctorCheck::warn(
            ID,
            NAME,
            detail,
            Some("Inspect/relax your profile, then run: sudo zensmu profile clear-guard"),
        )
    } else {
        DoctorCheck::pass(
            ID,
            NAME,
            format!("clean / disarmed ({})", guard_path.display()),
        )
    }
}

fn check_conflicting_tools() -> DoctorCheck {
    const ID: &str = "conflicting_tools";
    const NAME: &str = "Conflicting SMU tools";

    let found_conflicts: Vec<(&str, &str)> = CONFLICTING_TOOLS
        .iter()
        .copied()
        .filter(|(bin_path, _)| Path::new(bin_path).exists())
        .collect();

    if let Some((_, first_note)) = found_conflicts.first() {
        let paths: Vec<&str> = found_conflicts.iter().map(|(p, _)| *p).collect();
        DoctorCheck::warn(
            ID,
            NAME,
            format!(
                "detected potentially conflicting tool(s): {} ({first_note})",
                paths.join(", ")
            ),
            Some("Avoid running ruv-gui concurrently with zensmu (or uninstall ruv-gui to prevent unlocked sysfs races)"),
        )
    } else {
        DoctorCheck::pass(
            ID,
            NAME,
            "no conflicting SMU tools detected (e.g. /usr/bin/ruv-gui not present)",
        )
    }
}

fn check_mailbox_health_all(
    transport: &dyn SmuTransport,
    hw_opt: Option<&HardwareContext>,
    is_root: bool,
    checks: &mut Vec<DoctorCheck>,
) {
    let Some(hw) = hw_opt else {
        checks.push(DoctorCheck::skip(
            "mailbox_health",
            "SMU mailbox TestMessage (0x01)",
            "skipped: verified hardware context unavailable",
            None,
        ));
        return;
    };

    if !is_root {
        for (id, name) in [
            ("mailbox_rsmu", "RSMU mailbox TestMessage (0x01)"),
            ("mailbox_mp1", "MP1 mailbox TestMessage (0x01)"),
        ] {
            checks.push(DoctorCheck::skip(
                id,
                name,
                "skipped: mailbox health check requires root privileges",
                Some("Re-run with: sudo zensmu doctor"),
            ));
        }
        return;
    }

    checks.push(check_single_mailbox(
        transport,
        hw,
        Mailbox::Rsmu,
        hw.driver.has_rsmu && hw.capabilities.test_message_rsmu.is_some(),
        "mailbox_rsmu",
        "RSMU mailbox TestMessage (0x01)",
        "RSMU mailbox not exposed on this platform",
        "Check BIOS SMU/PBO settings and kernel dmesg for ryzen_smu errors",
    ));
    checks.push(check_single_mailbox(
        transport,
        hw,
        Mailbox::Mp1,
        hw.capabilities.test_message_mp1.is_some(),
        "mailbox_mp1",
        "MP1 mailbox TestMessage (0x01)",
        "MP1 TestMessage not configured for this platform",
        "Check kernel dmesg for ryzen_smu MP1 timeout or rejection",
    ));
}

#[allow(clippy::too_many_arguments)]
fn check_single_mailbox(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    mailbox: Mailbox,
    available: bool,
    id: &str,
    name: &str,
    unavailable_msg: &str,
    fail_remediation: &str,
) -> DoctorCheck {
    if !available {
        return DoctorCheck::skip(id, name, unavailable_msg, None);
    }
    let label = mailbox.as_str();
    match check_mailbox_health(transport, hw, mailbox) {
        Ok(()) => DoctorCheck::pass(
            id,
            name,
            format!("{label} responded 0x1338 to TestMessage(0x1337) with verified RSP OK"),
        ),
        Err(err) => DoctorCheck::fail(
            id,
            name,
            format!("{label} mailbox health check failed: {err}"),
            Some(fail_remediation),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zensmu_core::transport::mock::MockTransport;

    #[test]
    fn doctor_reports_false_ok_bug_and_guard_status_with_mock_transport() {
        let transport = MockTransport::granite_ridge_9700x();
        let tmp = tempfile::tempdir().unwrap();
        let guard_file = tmp.path().join("applying.lock");

        let report = run_doctor(&transport, tmp.path(), &guard_file, true);
        assert!(report.healthy);
        assert!(report
            .checks
            .iter()
            .any(|c| c.id == "drv_false_ok_bug" && c.status == CheckStatus::Warn));
        assert!(report
            .checks
            .iter()
            .any(|c| c.id == "pm_table" && c.status == CheckStatus::Pass));
        assert!(report
            .checks
            .iter()
            .any(|c| c.id == "boot_guard" && c.status == CheckStatus::Pass));

        // Trip boot guard and verify doctor detects it
        let guard = BootLoopGuard::new(guard_file.clone());
        let _armed = guard.check_and_arm("test-profile").unwrap();
        let report2 = run_doctor(&transport, tmp.path(), &guard_file, true);
        assert!(report2
            .checks
            .iter()
            .any(|c| c.id == "boot_guard" && c.status == CheckStatus::Warn));
        assert!(report.checks.iter().any(|c| c.id == "secure_boot"));
    }

    #[test]
    fn secure_boot_check_covers_blocked_loaded_and_disabled_states() {
        use zensmu_core::driver::secure_boot_status_from;

        let blocked = secure_boot_status_from(Some(true), Some("integrity".into()));
        let loaded_ok = check_secure_boot_with(&blocked, true);
        assert_eq!(loaded_ok.id, "secure_boot");
        assert_eq!(loaded_ok.status, CheckStatus::Pass);
        assert!(loaded_ok.message.contains("loaded/signed"));
        assert!(loaded_ok.remediation.is_none());

        let unloaded_warn = check_secure_boot_with(&blocked, false);
        assert_eq!(unloaded_warn.id, "secure_boot");
        assert_eq!(unloaded_warn.status, CheckStatus::Warn);
        assert!(unloaded_warn
            .remediation
            .as_deref()
            .is_some_and(|h| h.contains("mokutil")));

        let open = secure_boot_status_from(Some(false), Some("none".into()));
        let open_pass = check_secure_boot_with(&open, false);
        assert_eq!(open_pass.status, CheckStatus::Pass);
        assert!(open_pass
            .message
            .contains("Secure Boot disabled / kernel lockdown=none"));
    }
}
