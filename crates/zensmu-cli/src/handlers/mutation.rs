//! Shared transactional mutation, snapshot capture, and dead-man's switch confirmation helpers.

use super::signal::{install_sigint_handler, INTERRUPTED};
use super::{state_dir, CliOutcome};
use crate::commands::Cli;
use crate::output::{print_json, print_mutation, print_planned_ops, MutationOutput};
use std::io::{self, IsTerminal, Read, Write};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};
use zensmu_core::error::{Error, Result};
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::safety::snapshot::{SmuStateSnapshot, SnapshotStore};
use zensmu_core::session;
use zensmu_core::transport::SmuTransport;
use zensmu_core::types::PlannedOp;

// ============================================================================
// Shared Transactional Mutation, Snapshot & Dead-Man's Switch Helpers
// ============================================================================

pub(super) fn emit_mutation(cli: &Cli, out: &MutationOutput, respect_quiet: bool) {
    if cli.json {
        print_json(out);
    } else if !respect_quiet || !cli.quiet {
        print_mutation(out);
    }
}

pub(super) fn execute_mutation<F>(
    cli: &Cli,
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    action: &str,
    confirm_secs: Option<u64>,
    mut run_ops: F,
) -> Result<CliOutcome>
where
    F: FnMut(bool) -> Result<Vec<PlannedOp>>,
{
    // Always compute validated plan first (dry_run = true touches zero write sysfs files)
    let planned = run_ops(true)?;

    if cli.dry_run {
        let out = MutationOutput {
            action: action.to_string(),
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

    if !confirm_before_write(cli, &planned, action)? {
        return Ok(CliOutcome::ExitWithCode(1));
    }

    let store = SnapshotStore::new(state_dir(cli));
    let pre_snap = capture_and_store_snapshots(cli, transport, hw, &store)?;

    let ops = run_ops(false)?;

    if let Some(secs) = confirm_secs {
        return wait_for_confirmation_or_rollback(
            cli,
            transport,
            hw,
            pre_snap.as_ref(),
            secs,
            action,
            ops,
        );
    }

    let out = MutationOutput {
        action: action.to_string(),
        dry_run: false,
        applied: true,
        rolled_back: false,
        confirmed: None,
        operations: ops,
        note: None,
    };
    emit_mutation(cli, &out, true);
    Ok(CliOutcome::Success)
}

pub(super) fn capture_and_store_snapshots(
    cli: &Cli,
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    store: &SnapshotStore,
) -> Result<Option<SmuStateSnapshot>> {
    let snap = session::capture_snapshot(transport, hw)?;
    if let Err(err) = store.ensure_boot_baseline(&snap) {
        if cli.verbose > 0 && !cli.json {
            eprintln!("warning: could not persist boot-baseline snapshot: {err}");
        }
    }
    if let Err(err) = store.save_last_state(&snap) {
        if cli.verbose > 0 && !cli.json {
            eprintln!("warning: could not persist last-state snapshot: {err}");
        }
    }
    Ok(Some(snap))
}

pub(super) fn confirm_before_write(cli: &Cli, planned: &[PlannedOp], action: &str) -> Result<bool> {
    if cli.yes || cli.json || !io::stdin().is_terminal() {
        return Ok(true);
    }

    print_planned_ops(
        planned,
        &format!("Planned SMU operations for '{action}' ({}):", planned.len()),
    );
    eprint!("Apply these SMU changes? [y/N]: ");
    let _ = io::stderr().flush();

    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|source| Error::Io {
            path: "stdin".into(),
            source,
        })?;
    let trimmed = line.trim();
    if trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes") {
        Ok(true)
    } else {
        eprintln!("Aborted; no SMU registers were modified.");
        Ok(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ConfirmOutcome {
    Confirmed,
    Rejected,
    TimedOut,
    Interrupted,
}

impl ConfirmOutcome {
    pub(super) fn rollback_reason(self) -> Option<&'static str> {
        match self {
            Self::Confirmed => None,
            Self::Rejected => Some("rejected by user"),
            Self::TimedOut => Some("confirmation timed out"),
            Self::Interrupted => Some("interrupted (Ctrl-C)"),
        }
    }
}

pub(super) fn wait_for_confirmation_or_rollback(
    cli: &Cli,
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    pre_snap: Option<&SmuStateSnapshot>,
    timeout_secs: u64,
    action: &str,
    ops: Vec<PlannedOp>,
) -> Result<CliOutcome> {
    install_sigint_handler();
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut last_shown = u64::MAX;

    let outcome = loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            break ConfirmOutcome::Interrupted;
        }

        let now = Instant::now();
        if now >= deadline {
            break ConfirmOutcome::TimedOut;
        }

        let rem_secs = deadline.saturating_duration_since(now).as_secs() + 1;
        if rem_secs != last_shown && !cli.quiet && !cli.json {
            eprint!(
                "\rApplied '{action}'. Press Enter or 'y' within {rem_secs:>3}s to keep, or 'n'/Ctrl-C to rollback... "
            );
            let _ = io::stderr().flush();
            last_shown = rem_secs;
        }

        let mut pfd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, 100) };
        if rc > 0 && (pfd.revents & libc::POLLIN) != 0 {
            let mut buf = [0u8; 64];
            let n = io::stdin().read(&mut buf).unwrap_or(0);
            if n == 0 {
                break ConfirmOutcome::Rejected;
            }
            let input = String::from_utf8_lossy(buf.get(..n).unwrap_or(&[]));
            let trimmed = input.trim();
            if trimmed.is_empty()
                || trimmed.eq_ignore_ascii_case("y")
                || trimmed.eq_ignore_ascii_case("yes")
            {
                break ConfirmOutcome::Confirmed;
            } else {
                break ConfirmOutcome::Rejected;
            }
        }
    };

    if !cli.quiet && !cli.json {
        eprintln!();
    }

    match outcome.rollback_reason() {
        None => {
            let out = MutationOutput {
                action: action.to_string(),
                dry_run: false,
                applied: true,
                rolled_back: false,
                confirmed: Some(true),
                operations: ops,
                note: Some("confirmed by user".into()),
            };
            emit_mutation(cli, &out, true);
            Ok(CliOutcome::Success)
        }
        Some(reason) => {
            if let Some(snap) = pre_snap {
                session::apply_snapshot(transport, hw, snap, true, false)?;
            }
            let out = MutationOutput {
                action: action.to_string(),
                dry_run: false,
                applied: true,
                rolled_back: true,
                confirmed: Some(false),
                operations: ops,
                note: Some(format!("automatically rolled back ({reason})")),
            };
            emit_mutation(cli, &out, false);
            Ok(CliOutcome::ExitWithCode(1))
        }
    }
}
