//! Profile save, load, validate, diff, list, and guard-clear handlers.

use super::mutation::{
    capture_and_store_snapshots, confirm_before_write, emit_mutation,
    wait_for_confirmation_or_rollback,
};
use super::{guard_path, parse_confirm_secs, probe_hw, state_dir, CliOutcome};
use crate::commands::{Cli, ProfileAction};
use crate::output::{
    print_json, print_profile_diff, print_profile_list, ClearGuardOutput, MutationOutput,
    ProfileDiffOutput, ProfileListItem, ProfileListOutput, ProfileSaveOutput,
    ProfileValidateOutput,
};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use zensmu_core::config::profile::Profile;
use zensmu_core::error::{Error, Result};
use zensmu_core::safety::guard::BootLoopGuard;
use zensmu_core::safety::snapshot::SnapshotStore;
use zensmu_core::session;
use zensmu_core::transport::SmuTransport;

// ============================================================================
// Profiles (`profile`)
// ============================================================================

pub(super) fn cmd_profile(
    cli: &Cli,
    transport: &dyn SmuTransport,
    action: &ProfileAction,
) -> Result<CliOutcome> {
    match action {
        ProfileAction::Save { path } => {
            let hw = probe_hw(cli, transport)?;
            let snap = session::capture_snapshot(transport, &hw)?;
            let profile = Profile::from_snapshot(&snap);
            profile.save(path)?;
            let out = ProfileSaveOutput {
                path: path.display().to_string(),
                codename: hw.codename,
                physical_cores: hw.topology.physical_cores,
            };
            if cli.json {
                print_json(&out);
            } else {
                println!(
                    "Saved profile for {} ({} physical cores) to {}",
                    hw.codename.display_name(),
                    out.physical_cores,
                    out.path
                );
            }
            Ok(CliOutcome::Success)
        }
        ProfileAction::Validate { path } => {
            let profile = Profile::load(path)?;
            let (host_codename, note) = match probe_hw(cli, transport) {
                Ok(hw) => {
                    profile.validate_for(
                        hw.codename,
                        hw.topology.physical_cores,
                        cli.allow_extreme,
                    )?;
                    (Some(hw.codename), None)
                }
                Err(Error::DriverNotLoaded { .. }) => {
                    let cores = profile.target.physical_cores.unwrap_or(16);
                    profile.validate_for(profile.target.codename, cores, cli.allow_extreme)?;
                    (
                        None,
                        Some(
                            "ryzen_smu not loaded: validated schema & bounds against declared profile target"
                                .into(),
                        ),
                    )
                }
                Err(err) => return Err(err),
            };

            let out = ProfileValidateOutput {
                path: path.display().to_string(),
                valid: true,
                target_codename: profile.target.codename,
                target_physical_cores: profile.target.physical_cores,
                host_codename,
                note,
            };
            if cli.json {
                print_json(&out);
            } else {
                println!(
                    "Profile {} is valid (target: {}, cores: {}).",
                    out.path,
                    out.target_codename.display_name(),
                    out.target_physical_cores
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "any".into())
                );
                if let Some(ref n) = out.note {
                    println!("Note: {n}");
                }
            }
            Ok(CliOutcome::Success)
        }
        ProfileAction::Diff { path } => {
            let hw = probe_hw(cli, transport)?;
            let profile = Profile::load(path)?;
            profile.validate_for(hw.codename, hw.topology.physical_cores, true)?;
            let snap = session::capture_snapshot(transport, &hw)?;
            let diffs = profile.diff_against(&snap);
            let out = ProfileDiffOutput {
                path: path.display().to_string(),
                identical: diffs.is_empty(),
                diffs,
            };
            if cli.json {
                print_json(&out);
            } else {
                print_profile_diff(&out);
            }
            Ok(CliOutcome::Success)
        }
        ProfileAction::Apply {
            path,
            boot_guard,
            settle_secs,
        } => {
            let confirm_secs = parse_confirm_secs(cli)?;
            let hw = probe_hw(cli, transport)?;
            let profile = Profile::load(path)?;
            profile.validate_for(hw.codename, hw.topology.physical_cores, cli.allow_extreme)?;

            // Build dry-run plan first (validates everything cleanly before touching guard or SMU)
            let planned =
                session::apply_profile(transport, &hw, &profile, cli.allow_extreme, true)?;

            if cli.dry_run {
                let out = MutationOutput {
                    action: format!("profile apply {}", path.display()),
                    dry_run: true,
                    applied: false,
                    rolled_back: false,
                    confirmed: None,
                    operations: planned,
                    note: None,
                };
                emit_mutation(cli, &out, false);
                return Ok(CliOutcome::Success);
            }

            if !confirm_before_write(cli, &planned, &format!("profile apply {}", path.display()))? {
                return Ok(CliOutcome::ExitWithCode(1));
            }

            // If --boot-guard is enabled, arm persistent sentinel BEFORE any SMU writes.
            // If already tripped from a previous crash, exits non-zero cleanly without touching SMU.
            let armed_guard = if *boot_guard {
                let guard = BootLoopGuard::new(guard_path(cli));
                Some(guard.check_and_arm(&path.display().to_string())?)
            } else {
                None
            };

            let store = SnapshotStore::new(state_dir(cli));
            let pre_snap = capture_and_store_snapshots(cli, transport, &hw, &store)?;

            let ops = session::apply_profile(transport, &hw, &profile, cli.allow_extreme, false)?;

            if let Some(secs) = confirm_secs {
                let outcome = wait_for_confirmation_or_rollback(
                    cli,
                    transport,
                    &hw,
                    pre_snap.as_ref(),
                    secs,
                    &format!("profile apply {}", path.display()),
                    ops,
                )?;
                if let Some(armed) = armed_guard {
                    // Cleanly disarm after interactive confirmation or clean rollback
                    armed.disarm()?;
                }
                return Ok(outcome);
            }

            // Boot-guard settle window: if the machine crashes/freezes during settle,
            // ArmedBootGuard is never disarmed and the sentinel stays on disk.
            if let Some(armed) = armed_guard {
                if *settle_secs > 0 {
                    if !cli.quiet && !cli.json {
                        eprintln!(
                            "Boot-loop guard armed: waiting {}s settle period before disarming...",
                            settle_secs
                        );
                    }
                    thread::sleep(Duration::from_secs(*settle_secs));
                }
                armed.disarm()?;
            }

            let note = if *boot_guard {
                Some(format!(
                    "boot-loop guard settled ({settle_secs}s) and disarmed cleanly"
                ))
            } else {
                None
            };

            let out = MutationOutput {
                action: format!("profile apply {}", path.display()),
                dry_run: false,
                applied: true,
                rolled_back: false,
                confirmed: None,
                operations: ops,
                note,
            };
            emit_mutation(cli, &out, true);
            Ok(CliOutcome::Success)
        }
        ProfileAction::ClearGuard => cmd_profile_clear_guard(cli),
        ProfileAction::List { dir } => cmd_profile_list(cli, dir),
    }
}

pub(super) fn cmd_profile_clear_guard(cli: &Cli) -> Result<CliOutcome> {
    let gp = guard_path(cli);
    let guard = BootLoopGuard::new(gp.clone());
    let was_tripped = guard.is_tripped();
    guard.clear()?;

    let out = ClearGuardOutput {
        guard_path: gp.display().to_string(),
        was_tripped,
        cleared: true,
    };
    if cli.json {
        print_json(&out);
    } else if was_tripped {
        println!(
            "Cleared tripped boot-loop guard sentinel at {}",
            out.guard_path
        );
    } else {
        println!(
            "Boot-loop guard sentinel at {} was already clear.",
            out.guard_path
        );
    }
    Ok(CliOutcome::Success)
}

pub(super) fn cmd_profile_list(cli: &Cli, dir: &Path) -> Result<CliOutcome> {
    let mut items = Vec::new();
    if dir.exists() {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .map_err(|source| Error::Io {
                path: dir.display().to_string(),
                source,
            })?
            .filter_map(|e| e.ok().map(|ent| ent.path()))
            .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("toml"))
            .collect();
        entries.sort();

        for path in entries {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            match Profile::load(&path) {
                Ok(p) => items.push(ProfileListItem {
                    name,
                    path: path.display().to_string(),
                    codename: Some(p.target.codename),
                    valid: true,
                    error: None,
                }),
                Err(err) => items.push(ProfileListItem {
                    name,
                    path: path.display().to_string(),
                    codename: None,
                    valid: false,
                    error: Some(err.to_string()),
                }),
            }
        }
    }

    let out = ProfileListOutput {
        dir: dir.display().to_string(),
        profiles: items,
    };
    if cli.json {
        print_json(&out);
    } else {
        print_profile_list(&out);
    }
    Ok(CliOutcome::Success)
}
