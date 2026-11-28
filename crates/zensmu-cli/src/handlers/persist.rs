//! Boot/suspend persistence (`persist`) and snapshot restore (`rollback`, `reset`) handlers.

use super::mutation::{confirm_before_write, emit_mutation};
use super::{persist_paths, probe_hw, state_dir, CliOutcome};
use crate::commands::{Cli, PersistAction, PersistCommand};
use crate::output::{
    print_json, print_persist_action, print_persist_status, MutationOutput, PersistActionOutput,
};
use zensmu_core::config::profile::Profile;
use zensmu_core::error::{Error, Result};
use zensmu_core::safety::persist::{self, PersistPaths, PersistStatus};
use zensmu_core::safety::snapshot::SnapshotStore;
use zensmu_core::session;
use zensmu_core::transport::SmuTransport;
use zensmu_core::types::Codename;

// ============================================================================
// Boot & Suspend Persistence (`persist`)
// ============================================================================

pub(super) fn emit_persist_action(
    cli: &Cli,
    paths: &PersistPaths,
    action: &str,
    dry_run: bool,
    status: PersistStatus,
    note: String,
) {
    let out = PersistActionOutput {
        action: action.into(),
        dry_run,
        active_profile_path: paths.active_profile.display().to_string(),
        service_enabled: status.service_enabled,
        status,
        note: Some(note),
    };
    if cli.json {
        print_json(&out);
    } else if dry_run || !cli.quiet {
        print_persist_action(&out);
    }
}

pub(super) fn cmd_persist(
    cli: &Cli,
    transport: &dyn SmuTransport,
    cmd: &PersistCommand,
) -> Result<CliOutcome> {
    let paths = persist_paths(cli);

    match &cmd.action {
        PersistAction::Status => {
            let (codename, cores) = match probe_hw(cli, transport) {
                Ok(hw) => (Some(hw.codename), Some(hw.topology.physical_cores)),
                Err(_) => (None, None),
            };
            let status = persist::read_persist_status(&paths, codename, cores);
            if cli.json {
                print_json(&status);
            } else {
                print_persist_status(&status);
            }
            Ok(CliOutcome::Success)
        }
        PersistAction::Save { from, enable } => {
            let hw = probe_hw(cli, transport)?;
            let profile = match from {
                Some(path) => Profile::load(path)?,
                None => {
                    let snap = session::capture_snapshot(transport, &hw)?;
                    Profile::from_snapshot(&snap)
                }
            };
            // Live hardware snapshots may reflect BIOS-configured Motherboard-PBO values
            // (e.g. PPT=1000W, Scalar=5x) above interactive soft thresholds, so validate
            // live snapshots against hard hardware bounds while keeping soft checks for --from files.
            let effective_allow_extreme = cli.allow_extreme || from.is_none();
            profile.validate_for(
                hw.codename,
                hw.topology.physical_cores,
                effective_allow_extreme,
            )?;

            if cli.dry_run {
                let status = persist::read_persist_status(
                    &paths,
                    Some(hw.codename),
                    Some(hw.topology.physical_cores),
                );
                let note = if *enable {
                    format!(
                        "would write active profile to {}, ensure {}, and enable {}",
                        paths.active_profile.display(),
                        paths.modules_load.display(),
                        paths.service_name
                    )
                } else {
                    format!(
                        "would write active profile to {} and ensure {}",
                        paths.active_profile.display(),
                        paths.modules_load.display()
                    )
                };
                emit_persist_action(cli, &paths, "persist save", true, status, note);
                return Ok(CliOutcome::Success);
            }

            persist::persist_profile(
                &paths,
                &profile,
                hw.codename,
                hw.topology.physical_cores,
                effective_allow_extreme,
            )?;
            if *enable {
                persist::set_service_enabled(
                    &paths,
                    true,
                    hw.codename,
                    hw.topology.physical_cores,
                    effective_allow_extreme,
                )?;
            }

            let status = persist::read_persist_status(
                &paths,
                Some(hw.codename),
                Some(hw.topology.physical_cores),
            );
            let note = if *enable {
                format!(
                    "wrote active profile, ensured {}, and enabled {}",
                    paths.modules_load.display(),
                    paths.service_name
                )
            } else {
                format!(
                    "wrote active profile and ensured {}",
                    paths.modules_load.display()
                )
            };
            emit_persist_action(cli, &paths, "persist save", false, status, note);
            Ok(CliOutcome::Success)
        }
        PersistAction::Enable => {
            let hw = probe_hw(cli, transport)?;

            if cli.dry_run {
                if !paths.active_profile.exists() {
                    return Err(Error::InvalidInput(format!(
                        "cannot enable {} because {} does not exist yet (save a profile first)",
                        paths.service_name,
                        paths.active_profile.display()
                    )));
                }
                let prof = Profile::load(&paths.active_profile)?;
                prof.validate_for(hw.codename, hw.topology.physical_cores, true)?;

                let status = persist::read_persist_status(
                    &paths,
                    Some(hw.codename),
                    Some(hw.topology.physical_cores),
                );
                emit_persist_action(
                    cli,
                    &paths,
                    "persist enable",
                    true,
                    status,
                    format!(
                        "would ensure {} and enable {}",
                        paths.modules_load.display(),
                        paths.service_name
                    ),
                );
                return Ok(CliOutcome::Success);
            }

            persist::set_service_enabled(
                &paths,
                true,
                hw.codename,
                hw.topology.physical_cores,
                true,
            )?;
            let status = persist::read_persist_status(
                &paths,
                Some(hw.codename),
                Some(hw.topology.physical_cores),
            );
            emit_persist_action(
                cli,
                &paths,
                "persist enable",
                false,
                status,
                format!("enabled {}", paths.service_name),
            );
            Ok(CliOutcome::Success)
        }
        PersistAction::Disable { remove_profile } => {
            let hw_opt = probe_hw(cli, transport).ok();
            let (codename_opt, cores_opt) = hw_opt
                .as_ref()
                .map(|h| (Some(h.codename), Some(h.topology.physical_cores)))
                .unwrap_or((None, None));

            if cli.dry_run {
                let status = persist::read_persist_status(&paths, codename_opt, cores_opt);
                let note = if *remove_profile {
                    format!(
                        "would disable {} and remove {}",
                        paths.service_name,
                        paths.active_profile.display()
                    )
                } else {
                    format!("would disable {}", paths.service_name)
                };
                emit_persist_action(cli, &paths, "persist disable", true, status, note);
                return Ok(CliOutcome::Success);
            }

            persist::set_service_enabled(
                &paths,
                false,
                codename_opt.unwrap_or(Codename::GraniteRidge),
                cores_opt.unwrap_or(0),
                cli.allow_extreme,
            )?;
            let removed = if *remove_profile {
                persist::remove_active_profile(&paths)?
            } else {
                false
            };

            let status = persist::read_persist_status(&paths, codename_opt, cores_opt);
            let note = if *remove_profile {
                if removed {
                    format!(
                        "disabled {} and removed {}",
                        paths.service_name,
                        paths.active_profile.display()
                    )
                } else {
                    format!(
                        "disabled {} ({} was already absent)",
                        paths.service_name,
                        paths.active_profile.display()
                    )
                }
            } else {
                format!("disabled {}", paths.service_name)
            };
            emit_persist_action(cli, &paths, "persist disable", false, status, note);
            Ok(CliOutcome::Success)
        }
    }
}

// ============================================================================
// Rollback & Reset (`rollback`, `reset`)
// ============================================================================

pub(super) enum SnapshotRestoreKind {
    Rollback,
    Reset,
}

pub(super) fn execute_snapshot_restore(
    cli: &Cli,
    transport: &dyn SmuTransport,
    kind: SnapshotRestoreKind,
) -> Result<CliOutcome> {
    let hw = probe_hw(cli, transport)?;
    let store = SnapshotStore::new(state_dir(cli));
    let (action, snap, source_path, save_current_as_last_state) = match kind {
        SnapshotRestoreKind::Rollback => {
            let path = store.last_state_path();
            let snap = store.load_last_state()?.ok_or_else(|| {
                Error::InvalidInput(format!(
                    "no last-state snapshot found at {} (no prior mutation recorded in this boot)",
                    path.display()
                ))
            })?;
            ("rollback", snap, path, false)
        }
        SnapshotRestoreKind::Reset => {
            let path = store.boot_baseline_path();
            let snap = store.load_boot_baseline()?.ok_or_else(|| {
                Error::InvalidInput(format!(
                    "no boot-baseline snapshot found at {} (no prior mutation recorded in this boot)",
                    path.display()
                ))
            })?;
            ("reset", snap, path, true)
        }
    };

    let planned = session::apply_snapshot(transport, &hw, &snap, true, true)?;
    if cli.dry_run {
        let out = MutationOutput {
            action: action.into(),
            dry_run: true,
            applied: false,
            rolled_back: false,
            confirmed: None,
            operations: planned,
            note: Some(format!("from {}", source_path.display())),
        };
        emit_mutation(cli, &out, false);
        return Ok(CliOutcome::Success);
    }

    if !confirm_before_write(cli, &planned, action)? {
        return Ok(CliOutcome::ExitWithCode(1));
    }

    if save_current_as_last_state {
        // Save current state as last-state so `reset` itself can be rolled back if desired.
        if let Ok(cur) = session::capture_snapshot(transport, &hw) {
            let _ = store.save_last_state(&cur);
        }
    }

    let ops = session::apply_snapshot(transport, &hw, &snap, true, false)?;
    let out = MutationOutput {
        action: action.into(),
        dry_run: false,
        applied: true,
        rolled_back: false,
        confirmed: None,
        operations: ops,
        note: Some(format!("restored {}", source_path.display())),
    };
    emit_mutation(cli, &out, true);
    Ok(CliOutcome::Success)
}

pub(super) fn cmd_rollback(cli: &Cli, transport: &dyn SmuTransport) -> Result<CliOutcome> {
    execute_snapshot_restore(cli, transport, SnapshotRestoreKind::Rollback)
}

pub(super) fn cmd_reset(cli: &Cli, transport: &dyn SmuTransport) -> Result<CliOutcome> {
    execute_snapshot_restore(cli, transport, SnapshotRestoreKind::Reset)
}
