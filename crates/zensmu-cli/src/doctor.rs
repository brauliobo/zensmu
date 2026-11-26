//! Safety diagnostics (`zensmu doctor`) inspecting driver, sysfs permissions,
//! `smu.c:213` false-OK bug mitigation, CPUID consistency, PM table layout support,
//! boot-loop guard state, conflicting tools, and live mailbox health.

use serde::Serialize;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use zensmu_core::error::Error;
use zensmu_core::hw::identity::{
    detect_hardware, driver_has_false_ok_bug, read_host_cpuid, verify_codename_against_cpuid,
};
use zensmu_core::ops::co::check_mailbox_health;
use zensmu_core::ops::pm_table::{self, MAX_SAFE_PM_TABLE_SIZE};
use zensmu_core::safety::guard::BootLoopGuard;
use zensmu_core::transport::SmuTransport;
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
    let driver_info = match transport.read_info() {
        Ok(info) => {
            checks.push(DoctorCheck {
                id: "driver_loaded".into(),
                name: "ryzen_smu kernel module".into(),
                status: CheckStatus::Pass,
                message: format!(
                    "loaded at {} (drv {}, SMU {}, mp1_if={})",
                    sysfs_path.display(),
                    info.drv_version,
                    info.smu_version,
                    info.mp1_if_version
                ),
                remediation: None,
            });
            Some(info)
        }
        Err(Error::DriverNotLoaded { path }) => {
            checks.push(DoctorCheck {
                id: "driver_loaded".into(),
                name: "ryzen_smu kernel module".into(),
                status: CheckStatus::Fail,
                message: format!("sysfs directory not found at {path}"),
                remediation: Some("Load the kernel module: sudo modprobe ryzen_smu".into()),
            });
            None
        }
        Err(err) => {
            checks.push(DoctorCheck {
                id: "driver_loaded".into(),
                name: "ryzen_smu kernel module".into(),
                status: CheckStatus::Fail,
                message: format!("failed to read ryzen_smu metadata: {err}"),
                remediation: Some(
                    "Verify ryzen_smu is properly installed and check dmesg for errors".into(),
                ),
            });
            None
        }
    };

    // 2. Sysfs permissions
    if sysfs_path.exists() {
        let mut details = Vec::new();
        for file in ["drv_version", "pm_table", "smu_args", "rsmu_cmd", "mp1_smu_cmd", "smn"] {
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
            checks.push(DoctorCheck {
                id: "sysfs_permissions".into(),
                name: "Sysfs permissions".into(),
                status: CheckStatus::Pass,
                message: format!("running as root; mailbox + telemetry accessible ({mode_summary})"),
                remediation: None,
            });
        } else {
            checks.push(DoctorCheck {
                id: "sysfs_permissions".into(),
                name: "Sysfs permissions".into(),
                status: CheckStatus::Warn,
                message: format!(
                    "running unprivileged (UID {}); read-only telemetry works, mailbox read/write requires root ({mode_summary})",
                    unsafe { libc::geteuid() }
                ),
                remediation: Some(
                    "Run mailbox commands via sudo or pkexec (telemetry/info/top work unprivileged)".into(),
                ),
            });
        }
    } else {
        checks.push(DoctorCheck {
            id: "sysfs_permissions".into(),
            name: "Sysfs permissions".into(),
            status: CheckStatus::Skip,
            message: format!("skipped: {} does not exist", sysfs_path.display()),
            remediation: None,
        });
    }

    // 3. drv_version / smu.c:213 false-OK bug check
    if let Some(ref info) = driver_info {
        if driver_has_false_ok_bug(&info.drv_version) {
            let has_smn = sysfs_path.join("smn").exists();
            checks.push(DoctorCheck {
                id: "drv_false_ok_bug".into(),
                name: "ryzen_smu smu.c:213 false-OK bug".into(),
                status: CheckStatus::Warn,
                message: format!(
                    "ryzen_smu {} is affected by upstream smu.c:213 false-OK bug (driver may report OK 0x01 on rejected SMU commands); zensmu actively mitigates this via read-only SMN RSP register verification ({}) + read-after-write verification",
                    info.drv_version,
                    if has_smn { "smn present" } else { "smn missing!" }
                ),
                remediation: None,
            });
        } else {
            checks.push(DoctorCheck {
                id: "drv_false_ok_bug".into(),
                name: "ryzen_smu smu.c:213 false-OK bug".into(),
                status: CheckStatus::Pass,
                message: format!(
                    "ryzen_smu {} is not in the known smu.c:213 affected list (zensmu still enforces SMN RSP + read-after-write verification)",
                    info.drv_version
                ),
                remediation: None,
            });
        }
    } else {
        checks.push(DoctorCheck {
            id: "drv_false_ok_bug".into(),
            name: "ryzen_smu smu.c:213 false-OK bug".into(),
            status: CheckStatus::Skip,
            message: "skipped: driver version unavailable".into(),
            remediation: None,
        });
    }

    // 4. CPUID consistency
    let host_cpuid = read_host_cpuid();
    if let Some(ref info) = driver_info {
        match Codename::from_driver_id(info.codename_id) {
            Some(codename) => {
                if no_cpuid_check {
                    checks.push(DoctorCheck {
                        id: "cpuid_consistency".into(),
                        name: "CPUID identity cross-check".into(),
                        status: CheckStatus::Warn,
                        message: format!(
                            "skipped via --no-cpuid-check (driver codename: {})",
                            codename.display_name()
                        ),
                        remediation: None,
                    });
                } else if let Some(ref cpuid) = host_cpuid {
                    match verify_codename_against_cpuid(codename, cpuid) {
                        Ok(()) => {
                            checks.push(DoctorCheck {
                                id: "cpuid_consistency".into(),
                                name: "CPUID identity cross-check".into(),
                                status: CheckStatus::Pass,
                                message: format!(
                                    "{} matches host CPUID ({}, family 0x{:X}, model 0x{:X}, stepping {})",
                                    codename.display_name(),
                                    cpuid.brand,
                                    cpuid.family,
                                    cpuid.model,
                                    cpuid.stepping
                                ),
                                remediation: None,
                            });
                        }
                        Err(err) => {
                            checks.push(DoctorCheck {
                                id: "cpuid_consistency".into(),
                                name: "CPUID identity cross-check".into(),
                                status: CheckStatus::Fail,
                                message: err.to_string(),
                                remediation: Some(
                                    "Do not write SMU commands when driver codename disagrees with host CPUID (pass --no-cpuid-check only for mock sysfs tests)".into(),
                                ),
                            });
                        }
                    }
                } else {
                    checks.push(DoctorCheck {
                        id: "cpuid_consistency".into(),
                        name: "CPUID identity cross-check".into(),
                        status: CheckStatus::Skip,
                        message: "host CPUID unavailable on this architecture".into(),
                        remediation: None,
                    });
                }
            }
            None => {
                checks.push(DoctorCheck {
                    id: "cpuid_consistency".into(),
                    name: "CPUID identity cross-check".into(),
                    status: CheckStatus::Fail,
                    message: format!("unrecognized driver codename id={}", info.codename_id),
                    remediation: Some("Update zensmu or check ryzen_smu version".into()),
                });
            }
        }
    } else {
        checks.push(DoctorCheck {
            id: "cpuid_consistency".into(),
            name: "CPUID identity cross-check".into(),
            status: CheckStatus::Skip,
            message: "skipped: driver metadata unavailable".into(),
            remediation: None,
        });
    }

    // Resolve full HardwareContext (for PM table & mailbox health checks)
    let hw_res = detect_hardware(transport, !no_cpuid_check);
    let hw_opt = match &hw_res {
        Ok(hw) => Some(hw),
        Err(Error::UnsupportedCpu { codename, detail }) => {
            checks.push(DoctorCheck {
                id: "capability_profile".into(),
                name: "CPU capability profile".into(),
                status: CheckStatus::Fail,
                message: format!("unsupported CPU {codename}: {detail}"),
                remediation: Some(
                    "zensmu refuses unverified CPUs rather than guessing SMU opcodes".into(),
                ),
            });
            None
        }
        Err(_) => None,
    };

    // 5. PM table layout support
    if let Some(hw) = hw_opt {
        if !hw.driver.has_pm_table {
            checks.push(DoctorCheck {
                id: "pm_table".into(),
                name: "PM table telemetry layout".into(),
                status: CheckStatus::Warn,
                message: "ryzen_smu did not expose pm_table for this platform".into(),
                remediation: None,
            });
        } else if let Some(sz) = hw.driver.pm_table_size.filter(|&s| s > MAX_SAFE_PM_TABLE_SIZE) {
            checks.push(DoctorCheck {
                id: "pm_table".into(),
                name: "PM table telemetry layout".into(),
                status: CheckStatus::Fail,
                message: format!(
                    "pm_table_size ({sz} bytes) exceeds safe Linux sysfs PAGE_SIZE limit ({MAX_SAFE_PM_TABLE_SIZE} bytes)"
                ),
                remediation: Some(
                    "zensmu refuses oversized PM table reads to avoid kernel sysfs buffer bugs".into(),
                ),
            });
        } else {
            match pm_table::read_telemetry(transport, hw) {
                Ok(snap) => {
                    checks.push(DoctorCheck {
                        id: "pm_table".into(),
                        name: "PM table telemetry layout".into(),
                        status: CheckStatus::Pass,
                        message: format!(
                            "verified layout 0x{:08X} ({} / 0x{:X} bytes) decoded cleanly for {}",
                            snap.table_version,
                            snap.table_size,
                            snap.table_size,
                            snap.codename.display_name()
                        ),
                        remediation: None,
                    });
                }
                Err(Error::UnsupportedPmTable {
                    codename,
                    version,
                    size,
                }) => {
                    checks.push(DoctorCheck {
                        id: "pm_table".into(),
                        name: "PM table telemetry layout".into(),
                        status: CheckStatus::Warn,
                        message: format!(
                            "PM table version 0x{version:08X} (size 0x{size:X}) is not in zensmu's verified allowlist for {codename}"
                        ),
                        remediation: Some(
                            "Run `zensmu telemetry dump pm_table.bin` to inspect or report this layout".into(),
                        ),
                    });
                }
                Err(err) => {
                    checks.push(DoctorCheck {
                        id: "pm_table".into(),
                        name: "PM table telemetry layout".into(),
                        status: CheckStatus::Fail,
                        message: format!("failed to read/decode PM table: {err}"),
                        remediation: None,
                    });
                }
            }
        }
    } else {
        checks.push(DoctorCheck {
            id: "pm_table".into(),
            name: "PM table telemetry layout".into(),
            status: CheckStatus::Skip,
            message: "skipped: verified hardware context unavailable".into(),
            remediation: None,
        });
    }

    // 6. Boot-loop guard status
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
            _ => format!("TRIPPED at {} (sentinel file present)", guard_path.display()),
        };
        checks.push(DoctorCheck {
            id: "boot_guard".into(),
            name: "Boot-loop crash guard".into(),
            status: CheckStatus::Warn,
            message: detail,
            remediation: Some(
                "Inspect/relax your profile, then run: sudo zensmu profile clear-guard".into(),
            ),
        });
    } else {
        checks.push(DoctorCheck {
            id: "boot_guard".into(),
            name: "Boot-loop crash guard".into(),
            status: CheckStatus::Pass,
            message: format!("clean / disarmed ({})", guard_path.display()),
            remediation: None,
        });
    }

    // 7. Conflicting tools check (/usr/bin/ruv-gui etc.)
    let mut found_conflicts = Vec::new();
    for &(bin_path, note) in CONFLICTING_TOOLS {
        if Path::new(bin_path).exists() {
            found_conflicts.push((bin_path, note));
        }
    }
    if found_conflicts.is_empty() {
        checks.push(DoctorCheck {
            id: "conflicting_tools".into(),
            name: "Conflicting SMU tools".into(),
            status: CheckStatus::Pass,
            message: "no conflicting SMU tools detected (e.g. /usr/bin/ruv-gui not present)".into(),
            remediation: None,
        });
    } else {
        let paths: Vec<&str> = found_conflicts.iter().map(|(p, _)| *p).collect();
        checks.push(DoctorCheck {
            id: "conflicting_tools".into(),
            name: "Conflicting SMU tools".into(),
            status: CheckStatus::Warn,
            message: format!(
                "detected potentially conflicting tool(s): {} ({})",
                paths.join(", "),
                found_conflicts[0].1
            ),
            remediation: Some(
                "Avoid running ruv-gui concurrently with zensmu (or uninstall ruv-gui to prevent unlocked sysfs races)".into(),
            ),
        });
    }

    // 8. Live TestMessage (0x01) mailbox health checks on RSMU & MP1 (if root)
    if let Some(hw) = hw_opt {
        if !is_root {
            checks.push(DoctorCheck {
                id: "mailbox_rsmu".into(),
                name: "RSMU mailbox TestMessage (0x01)".into(),
                status: CheckStatus::Skip,
                message: "skipped: mailbox health check requires root privileges".into(),
                remediation: Some("Re-run with: sudo zensmu doctor".into()),
            });
            checks.push(DoctorCheck {
                id: "mailbox_mp1".into(),
                name: "MP1 mailbox TestMessage (0x01)".into(),
                status: CheckStatus::Skip,
                message: "skipped: mailbox health check requires root privileges".into(),
                remediation: Some("Re-run with: sudo zensmu doctor".into()),
            });
        } else {
            // RSMU
            if hw.driver.has_rsmu && hw.capabilities.test_message_rsmu.is_some() {
                match check_mailbox_health(transport, hw, Mailbox::Rsmu) {
                    Ok(()) => checks.push(DoctorCheck {
                        id: "mailbox_rsmu".into(),
                        name: "RSMU mailbox TestMessage (0x01)".into(),
                        status: CheckStatus::Pass,
                        message: "RSMU responded 0x1338 to TestMessage(0x1337) with verified RSP OK"
                            .into(),
                        remediation: None,
                    }),
                    Err(err) => checks.push(DoctorCheck {
                        id: "mailbox_rsmu".into(),
                        name: "RSMU mailbox TestMessage (0x01)".into(),
                        status: CheckStatus::Fail,
                        message: format!("RSMU mailbox health check failed: {err}"),
                        remediation: Some(
                            "Check BIOS SMU/PBO settings and kernel dmesg for ryzen_smu errors"
                                .into(),
                        ),
                    }),
                }
            } else {
                checks.push(DoctorCheck {
                    id: "mailbox_rsmu".into(),
                    name: "RSMU mailbox TestMessage (0x01)".into(),
                    status: CheckStatus::Skip,
                    message: "RSMU mailbox not exposed on this platform".into(),
                    remediation: None,
                });
            }

            // MP1
            if hw.capabilities.test_message_mp1.is_some() {
                match check_mailbox_health(transport, hw, Mailbox::Mp1) {
                    Ok(()) => checks.push(DoctorCheck {
                        id: "mailbox_mp1".into(),
                        name: "MP1 mailbox TestMessage (0x01)".into(),
                        status: CheckStatus::Pass,
                        message: "MP1 responded 0x1338 to TestMessage(0x1337) with verified RSP OK"
                            .into(),
                        remediation: None,
                    }),
                    Err(err) => checks.push(DoctorCheck {
                        id: "mailbox_mp1".into(),
                        name: "MP1 mailbox TestMessage (0x01)".into(),
                        status: CheckStatus::Fail,
                        message: format!("MP1 mailbox health check failed: {err}"),
                        remediation: Some(
                            "Check kernel dmesg for ryzen_smu MP1 timeout or rejection".into(),
                        ),
                    }),
                }
            } else {
                checks.push(DoctorCheck {
                    id: "mailbox_mp1".into(),
                    name: "MP1 mailbox TestMessage (0x01)".into(),
                    status: CheckStatus::Skip,
                    message: "MP1 TestMessage not configured for this platform".into(),
                    remediation: None,
                });
            }
        }
    } else {
        checks.push(DoctorCheck {
            id: "mailbox_health".into(),
            name: "SMU mailbox TestMessage (0x01)".into(),
            status: CheckStatus::Skip,
            message: "skipped: verified hardware context unavailable".into(),
            remediation: None,
        });
    }

    let healthy = !checks.iter().any(|c| c.status == CheckStatus::Fail);

    DoctorReport {
        healthy,
        is_root,
        sysfs_path: sysfs_path.display().to_string(),
        guard_path: guard_path.display().to_string(),
        checks,
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
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.id == "drv_false_ok_bug" && c.status == CheckStatus::Warn)
        );
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.id == "pm_table" && c.status == CheckStatus::Pass)
        );
        assert!(
            report
                .checks
                .iter()
                .any(|c| c.id == "boot_guard" && c.status == CheckStatus::Pass)
        );

        // Trip boot guard and verify doctor detects it
        let guard = BootLoopGuard::new(guard_file.clone());
        let _armed = guard.check_and_arm("test-profile").unwrap();
        let report2 = run_doctor(&transport, tmp.path(), &guard_file, true);
        assert!(
            report2
                .checks
                .iter()
                .any(|c| c.id == "boot_guard" && c.status == CheckStatus::Warn)
        );
    }
}
