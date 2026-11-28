//! `zensmu` — Safety-first CLI and live TUI for AMD Ryzen SMU control and telemetry.

mod commands;
mod doctor;
mod handlers;
mod output;
mod tui;

use clap::Parser;
use commands::{Cli, Command};
use doctor::is_effective_root;
use handlers::{run, sysfs_path, CliOutcome};
use output::print_error;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode};
use zensmu_core::driver;
use zensmu_core::error::Error;

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;

    match run(&cli) {
        Ok(CliOutcome::Success) => ExitCode::SUCCESS,
        Ok(CliOutcome::ExitWithCode(code)) => ExitCode::from(code),
        Err(err) => {
            if matches!(err, Error::DriverNotLoaded { .. }) && maybe_prompt_and_setup_driver(&cli) {
                match run(&cli) {
                    Ok(CliOutcome::Success) => return ExitCode::SUCCESS,
                    Ok(CliOutcome::ExitWithCode(code)) => return ExitCode::from(code),
                    Err(retry_err) => {
                        print_error(&retry_err, json);
                        return ExitCode::FAILURE;
                    }
                }
            }
            print_error(&err, json);
            ExitCode::FAILURE
        }
    }
}

/// Interactive helper invoked when a command fails because `ryzen_smu` is not loaded.
/// Returns `true` if the driver was successfully loaded/installed and the caller should retry.
fn maybe_prompt_and_setup_driver(cli: &Cli) -> bool {
    // Never prompt in JSON mode, quiet mode, dry-run mode, non-TTY pipelines,
    // custom mock sysfs paths, or when already running `zensmu driver`.
    if cli.json
        || cli.quiet
        || cli.dry_run
        || cli.sysfs_path.is_some()
        || matches!(cli.command, Command::Driver { .. })
        || !io::stdin().is_terminal()
        || !io::stderr().is_terminal()
    {
        return false;
    }

    let sp = sysfs_path(cli);
    let status = driver::inspect_driver_status(&sp);
    if status.loaded {
        return false;
    }

    let is_root = is_effective_root();
    let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("zensmu"));

    if status.module_installed {
        eprintln!();
        eprintln!("ryzen_smu kernel module is installed but not loaded.");
        if !prompt_yes_no("Load ryzen_smu now (and enable boot autoload)?", true) {
            return false;
        }
        return run_driver_setup_command(&current_exe, is_root, &["driver", "load"]);
    }

    eprintln!();
    eprintln!(
        "ryzen_smu kernel driver is not installed on {} (kernel {}).",
        status.distro_name, status.kernel_release
    );

    let desc = if status.bundled_dkms_available {
        "Build & install `ryzen_smu` via DKMS from bundled sources now (auto-installs headers/dkms if needed)?"
    } else {
        "Build & install `ryzen_smu` via DKMS from upstream amkillam/ryzen_smu now (auto-installs headers/dkms if needed)?"
    };
    if !prompt_yes_no(desc, true) {
        return false;
    }
    run_driver_setup_command(&current_exe, is_root, &["driver", "install"])
}

fn prompt_yes_no(question: &str, default_yes: bool) -> bool {
    let suffix = if default_yes { "[Y/n]" } else { "[y/N]" };
    eprint!("{question} {suffix}: ");
    let _ = io::stderr().flush();
    let mut line = String::new();
    if io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    let ans = line.trim().to_ascii_lowercase();
    if ans.is_empty() {
        default_yes
    } else {
        matches!(ans.as_str(), "y" | "yes")
    }
}

fn run_driver_setup_command(exe: &Path, is_root: bool, args: &[&str]) -> bool {
    let status = if is_root {
        ProcessCommand::new(exe).args(args).status()
    } else {
        let mut cmd = ProcessCommand::new("sudo");
        cmd.arg(exe).args(args);
        cmd.status()
    };
    match status {
        Ok(s) if s.success() => {
            eprintln!();
            true
        }
        _ => false,
    }
}
