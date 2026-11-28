//! Non-blocking background command dispatcher and process execution helpers.

use super::action::CliAction;
use super::parse::{
    parse_clear_guard_output, parse_cli_error, parse_co_snapshot, parse_driver_action_output,
    parse_pbo_status, parse_persist_action_output, parse_planned_ops, parse_report_payload,
    parse_shaper_state, parse_status_payload,
};
use super::protocol::{
    build_command_spec, find_zensmu_binary, CliError, CommandSpec, ElevatedResult,
};
use eframe::egui;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

/// Non-blocking background command dispatcher using `std::sync::mpsc`.
pub struct CommandRunner {
    tx: Sender<ElevatedResult>,
    rx: Receiver<ElevatedResult>,
    in_flight: Option<String>,
    zensmu_bin: PathBuf,
    is_root: bool,
}

impl CommandRunner {
    pub fn new(is_root: bool) -> Self {
        let (tx, rx) = mpsc::channel();
        Self {
            tx,
            rx,
            in_flight: None,
            zensmu_bin: find_zensmu_binary(),
            is_root,
        }
    }

    pub fn is_busy(&self) -> bool {
        self.in_flight.is_some()
    }

    pub fn in_flight_label(&self) -> Option<&str> {
        self.in_flight.as_deref()
    }

    pub fn zensmu_bin(&self) -> &Path {
        &self.zensmu_bin
    }

    /// Spawn a `CliAction` on a background thread and request an `egui` repaint upon completion.
    pub fn dispatch(&mut self, action: CliAction, ctx: egui::Context) {
        if self.in_flight.is_some() {
            return;
        }
        let title = action.title();
        self.in_flight = Some(title.clone());
        let tx = self.tx.clone();
        let zensmu_bin = self.zensmu_bin.clone();
        let is_root = self.is_root;

        thread::spawn(move || {
            let result = execute_action(&zensmu_bin, is_root, action);
            let _ = tx.send(result);
            ctx.request_repaint();
        });
    }

    /// Drain all completed results from the background worker.
    pub fn poll_results(&mut self) -> Vec<ElevatedResult> {
        let mut results = Vec::new();
        while let Ok(res) = self.rx.try_recv() {
            self.in_flight = None;
            results.push(res);
        }
        results
    }
}

fn execute_action(zensmu_bin: &Path, is_root: bool, action: CliAction) -> ElevatedResult {
    let title = action.title();
    let batches = action.to_arg_batches();
    let elevate = action.requires_privilege();

    let mut stdouts = Vec::with_capacity(batches.len());
    for batch in &batches {
        match run_cli_with_fallback(zensmu_bin, batch, elevate, is_root) {
            Ok(out) => stdouts.push(out),
            Err(CliError::HardwareClamped { message, caps }) => {
                return ElevatedResult::HardwareClamped { message, caps };
            }
            Err(CliError::Generic(message)) => return ElevatedResult::Error { title, message },
        }
    }

    let first = stdouts.first().map(String::as_str).unwrap_or("");
    match action {
        CliAction::Status => match parse_status_payload(first) {
            Ok(payload) => ElevatedResult::Status(Box::new(payload)),
            Err(message) => ElevatedResult::Error { title, message },
        },
        CliAction::CoGet => match parse_co_snapshot(first) {
            Ok(snap) => ElevatedResult::Co(snap),
            Err(message) => ElevatedResult::Error { title, message },
        },
        CliAction::ShaperGet => match parse_shaper_state(first) {
            Ok(state) => ElevatedResult::Shaper(state),
            Err(message) => ElevatedResult::Error { title, message },
        },
        CliAction::PboGet => match parse_pbo_status(first) {
            Ok(status) => ElevatedResult::Pbo(status),
            Err(message) => ElevatedResult::Error { title, message },
        },
        CliAction::Report { .. } => match parse_report_payload(first) {
            Ok(payload) => ElevatedResult::Report(Box::new(payload)),
            Err(message) => ElevatedResult::Error { title, message },
        },
        CliAction::ProfileExport { path } => match fs::read_to_string(&path) {
            Ok(toml) => ElevatedResult::ProfileExported { path, toml },
            Err(e) => ElevatedResult::Error {
                title,
                message: format!(
                    "Export succeeded, but reading '{}' failed: {e}",
                    path.display()
                ),
            },
        },
        CliAction::PersistSave { .. } | CliAction::PersistSetService { .. } => {
            match parse_persist_action_output(first) {
                Ok(out) => ElevatedResult::PersistUpdated {
                    action: out.action,
                    status: out.status,
                    note: out.note,
                },
                Err(message) => ElevatedResult::Error { title, message },
            }
        }
        CliAction::ClearBootGuard => match parse_clear_guard_output(first) {
            Ok(cleared) => ElevatedResult::GuardCleared { cleared },
            Err(message) => ElevatedResult::Error { title, message },
        },
        CliAction::DriverLoad | CliAction::DriverInstall { .. } => {
            match parse_driver_action_output(first) {
                Ok(out) => ElevatedResult::DriverLoaded(Box::new(out)),
                Err(message) => ElevatedResult::Error { title, message },
            }
        }
        write_action => {
            let mut all_ops = Vec::new();
            for out in &stdouts {
                match parse_planned_ops(out) {
                    Ok(ops) => all_ops.extend(ops),
                    Err(message) => return ElevatedResult::Error { title, message },
                }
            }
            if write_action.is_dry_run() {
                ElevatedResult::DryRunPlan {
                    title,
                    live_action: write_action.into_live(),
                    ops: all_ops,
                }
            } else {
                ElevatedResult::Applied {
                    title,
                    ops: all_ops,
                }
            }
        }
    }
}

fn run_cli_with_fallback(
    zensmu_bin: &Path,
    cli_args: &[String],
    elevate: bool,
    is_root: bool,
) -> Result<String, CliError> {
    let spec = build_command_spec(zensmu_bin, cli_args, elevate, is_root);
    match run_command_spec(&spec) {
        Ok(stdout) => Ok(stdout),
        Err(CliError::Generic(err))
            if !elevate && !is_root && looks_like_permission_error(&err) =>
        {
            let elevated = build_command_spec(zensmu_bin, cli_args, true, false);
            run_command_spec(&elevated)
        }
        Err(err) => Err(err),
    }
}

fn run_command_spec(spec: &CommandSpec) -> Result<String, CliError> {
    let output = Command::new(&spec.program)
        .args(&spec.args)
        .output()
        .map_err(|e| {
            let msg = if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "Could not find executable '{}'. Ensure `zensmu` and `polkit` (`pkexec`) are installed.",
                    spec.program.display()
                )
            } else {
                format!("Failed to launch '{}': {e}", spec.program.display())
            };
            CliError::Generic(msg)
        })?;

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();

    if output.status.success() {
        Ok(stdout)
    } else {
        Err(parse_cli_error(
            &stdout,
            &stderr,
            output.status.code(),
            spec.uses_pkexec,
        ))
    }
}

fn looks_like_permission_error(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("permission denied")
        || lower.contains("access denied")
        || lower.contains("eacces")
        || lower.contains("requires root")
        || lower.contains("sudo")
        || lower.contains("pkexec")
}
