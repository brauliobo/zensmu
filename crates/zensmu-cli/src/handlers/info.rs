//! Inspection, diagnostics, driver, report, completions, and manpage handlers.

use super::{guard_path, probe_hw, sysfs_path, CliOutcome};
use crate::commands::{self, Cli, DriverAction};
use crate::doctor::{is_effective_root, run_doctor};
use crate::output::{
    print_doctor, print_driver_action, print_driver_status, print_info, print_json,
    print_report_footer, print_status, InfoOutput, ReportOutput, StatusOutput,
};
use clap::CommandFactory;
use clap_complete::Shell;
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::Path;
use std::process::{Command as ProcessCommand, Stdio};
use zensmu_core::driver::{self, DriverInstallOptions};
use zensmu_core::error::{Error, Result};
use zensmu_core::ops::{co, pbo, pm_table, shaper};
use zensmu_core::report::{collect_debug_report, ReportOptions, GITHUB_REPO};
use zensmu_core::safety::guard::BootLoopGuard;
use zensmu_core::transport::SmuTransport;

pub(super) fn cmd_completions(shell: commands::CompletionShell) -> Result<CliOutcome> {
    let mut cmd = Cli::command();
    let gen_shell: Shell = shell.into();
    let mut buf = Vec::new();
    clap_complete::generate(gen_shell, &mut cmd, "zensmu", &mut buf);
    if let Err(err) = io::stdout().write_all(&buf) {
        if err.kind() != io::ErrorKind::BrokenPipe {
            return Err(Error::Io {
                path: "stdout".into(),
                source: err,
            });
        }
    }
    Ok(CliOutcome::Success)
}

pub(super) fn render_manpage_bytes() -> Result<Vec<u8>> {
    let cmd = Cli::command();
    let mut buf = Vec::new();
    clap_mangen::Man::new(cmd)
        .render(&mut buf)
        .map_err(|source| Error::Io {
            path: "manpage".into(),
            source,
        })?;
    Ok(buf)
}

pub(super) fn cmd_manpage() -> Result<CliOutcome> {
    let buf = render_manpage_bytes()?;
    if let Err(err) = io::stdout().write_all(&buf) {
        if err.kind() != io::ErrorKind::BrokenPipe {
            return Err(Error::Io {
                path: "stdout".into(),
                source: err,
            });
        }
    }
    Ok(CliOutcome::Success)
}

// ============================================================================
// Inspection & Diagnostics Subcommands
// ============================================================================

pub(super) fn cmd_info(cli: &Cli, transport: &dyn SmuTransport) -> Result<CliOutcome> {
    let hw = probe_hw(cli, transport)?;
    let out = InfoOutput::from_hw(&hw);
    if cli.json {
        print_json(&out);
    } else {
        print_info(&out);
    }
    Ok(CliOutcome::Success)
}

pub(super) fn cmd_doctor(cli: &Cli, transport: &dyn SmuTransport) -> Result<CliOutcome> {
    let sp = sysfs_path(cli);
    let gp = guard_path(cli);
    let report = run_doctor(transport, &sp, &gp, cli.no_cpuid_check);
    let healthy = report.healthy;

    if cli.json {
        print_json(&report);
    } else {
        print_doctor(&report);
    }

    if healthy {
        Ok(CliOutcome::Success)
    } else {
        Ok(CliOutcome::ExitWithCode(1))
    }
}

pub(super) fn cmd_driver(cli: &Cli, action: Option<&DriverAction>) -> Result<CliOutcome> {
    let sp = sysfs_path(cli);
    match action.unwrap_or(&DriverAction::Status) {
        DriverAction::Status => {
            let status = driver::inspect_driver_status(&sp);
            if cli.json {
                print_json(&status);
            } else {
                print_driver_status(&status);
            }
            if status.loaded {
                Ok(CliOutcome::Success)
            } else {
                Ok(CliOutcome::ExitWithCode(1))
            }
        }
        DriverAction::Load { no_autoload } => {
            let out = driver::load_driver(&sp, !no_autoload, cli.dry_run)?;
            if cli.json {
                print_json(&out);
            } else {
                print_driver_action(&out);
            }
            Ok(CliOutcome::Success)
        }
        DriverAction::Install {
            method,
            release,
            no_autoload,
            force,
        } => {
            let opts = DriverInstallOptions {
                method: (*method).into(),
                release_tag: release.clone(),
                configure_autoload: !no_autoload,
                force: *force,
                dry_run: cli.dry_run,
            };
            let out = driver::install_driver(&sp, &opts)?;
            if cli.json {
                print_json(&out);
            } else {
                print_driver_action(&out);
            }
            Ok(CliOutcome::Success)
        }
        DriverAction::Uninstall { force } => {
            let out = driver::uninstall_driver(&sp, *force, cli.dry_run)?;
            if cli.json {
                print_json(&out);
            } else {
                print_driver_action(&out);
            }
            Ok(CliOutcome::Success)
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn cmd_report(
    cli: &Cli,
    transport: &dyn SmuTransport,
    note: Option<&str>,
    raw_pm_table: bool,
    no_mailbox: bool,
    output: Option<&Path>,
    open: bool,
    create: bool,
) -> Result<CliOutcome> {
    let opts = ReportOptions {
        include_raw_pm_table: raw_pm_table,
        include_mailbox_probes: !no_mailbox && is_effective_root(),
        user_note: note.map(str::to_string),
        no_cpuid_check: cli.no_cpuid_check,
    };

    let report = collect_debug_report(transport, &opts);
    let issue_title = report.suggested_issue_title();
    let issue_labels: Vec<String> = report
        .suggested_labels()
        .into_iter()
        .map(str::to_string)
        .collect();
    let issue_url = report.github_issue_url();
    let markdown = report.to_markdown();

    let out = ReportOutput {
        report,
        issue_title,
        issue_labels,
        issue_url,
        markdown,
    };

    if let Some(path) = output {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|source| Error::Io {
                    path: parent.display().to_string(),
                    source,
                })?;
            }
        }
        fs::write(path, &out.markdown).map_err(|source| Error::Io {
            path: path.display().to_string(),
            source,
        })?;
    }

    if cli.json {
        print_json(&out);
        return Ok(CliOutcome::Success);
    }

    print!("{}", out.markdown);
    if !cli.quiet {
        print_report_footer(&out, output);
    }

    if open {
        if cli.dry_run {
            println!(
                "[DRY-RUN] Would open GitHub issue URL via xdg-open: {}",
                out.issue_url
            );
        } else {
            ProcessCommand::new("xdg-open")
                .arg(&out.issue_url)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .map_err(|source| Error::Io {
                    path: "xdg-open".into(),
                    source,
                })?;
            if !cli.quiet {
                println!("Opened prefilled GitHub issue URL in default browser via xdg-open.");
            }
        }
    }

    if create {
        if cli.dry_run {
            println!(
                "[DRY-RUN] Would run: gh issue create --repo {GITHUB_REPO} --title {:?} --body-file -",
                out.issue_title
            );
            return Ok(CliOutcome::Success);
        }

        if !confirm_github_issue_create(cli, &out.issue_title)? {
            return Ok(CliOutcome::ExitWithCode(1));
        }

        let mut child = ProcessCommand::new("gh")
            .args([
                "issue",
                "create",
                "--repo",
                GITHUB_REPO,
                "--title",
                &out.issue_title,
                "--body-file",
                "-",
            ])
            .stdin(Stdio::piped())
            .spawn()
            .map_err(|source| Error::Io {
                path: "gh".into(),
                source,
            })?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(out.markdown.as_bytes())
                .map_err(|source| Error::Io {
                    path: "gh stdin".into(),
                    source,
                })?;
        }

        let status = child.wait().map_err(|source| Error::Io {
            path: "gh".into(),
            source,
        })?;
        if !status.success() {
            return Err(Error::InvalidInput(format!(
                "gh issue create exited with status {status} (ensure `gh` is installed and authenticated via `gh auth login`)"
            )));
        }
    }

    Ok(CliOutcome::Success)
}

pub(super) fn confirm_github_issue_create(cli: &Cli, title: &str) -> Result<bool> {
    if cli.yes || !io::stdin().is_terminal() {
        return Ok(true);
    }

    eprintln!();
    eprintln!("About to create a public GitHub issue on {GITHUB_REPO}:");
    eprintln!("  Title: {title}");
    eprint!("Create this GitHub issue now via `gh`? [y/N]: ");
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
        eprintln!("Aborted; no GitHub issue was created.");
        Ok(false)
    }
}

pub(super) fn mailbox_read_outcome<T>(res: Result<T>) -> (Option<T>, Option<String>) {
    match res {
        Ok(val) => (Some(val), None),
        Err(Error::PermissionDenied { .. }) => (None, Some("requires root".into())),
        Err(err) => (None, Some(err.to_string())),
    }
}

pub(super) fn cmd_status(cli: &Cli, transport: &dyn SmuTransport) -> Result<CliOutcome> {
    let hw = probe_hw(cli, transport)?;
    let is_root = is_effective_root();
    let guard = BootLoopGuard::new(guard_path(cli));
    let guard_tripped = guard.is_tripped();

    let mut notes = Vec::new();
    if !is_root {
        notes.push(
            "Running unprivileged: PM table telemetry is readable, while SMU mailbox reads (CO, Curve Shaper, PBO) require root (sudo or pkexec)."
                .into(),
        );
    }

    let (telemetry, telemetry_error) = match pm_table::read_telemetry(transport, &hw) {
        Ok(t) => (Some(t), None),
        Err(e) => (None, Some(e.to_string())),
    };

    let (co_snap, co_error) = if hw.capabilities.get_co_per_core.is_some() {
        mailbox_read_outcome(co::get_co(transport, &hw))
    } else {
        (None, None)
    };

    let (curve_shaper, curve_shaper_error) = if hw.capabilities.get_curve_shaper.is_some() {
        mailbox_read_outcome(shaper::get_curve_shaper(transport, &hw))
    } else {
        (None, None)
    };

    let (pbo_st, pbo_error) = mailbox_read_outcome(pbo::get_pbo_status(transport, &hw));

    let out = StatusOutput {
        codename: hw.codename,
        codename_name: hw.codename.display_name().to_string(),
        is_root,
        telemetry,
        telemetry_error,
        co: co_snap,
        co_error,
        curve_shaper,
        curve_shaper_error,
        pbo: pbo_st,
        pbo_error,
        guard_tripped,
        notes,
    };

    if cli.json {
        print_json(&out);
    } else {
        print_status(&out);
    }
    Ok(CliOutcome::Success)
}
