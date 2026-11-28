use super::super::*;
use clap::Parser;
use std::path::PathBuf;

#[test]
fn parses_telemetry_top_profile_rollback_reset_completions() {
    // Default one-shot telemetry
    let t1 = Cli::try_parse_from(["zensmu", "telemetry", "--json"]).unwrap();
    assert!(t1.json);
    assert!(matches!(t1.command, Command::Telemetry { action: None }));

    // Telemetry watch
    let t2 = Cli::try_parse_from(["zensmu", "telemetry", "watch", "--interval", "250ms"]).unwrap();
    match t2.command {
        Command::Telemetry {
            action: Some(TelemetryAction::Watch { interval }),
        } => assert_eq!(interval, "250ms"),
        other => panic!("unexpected command: {other:?}"),
    }

    // Telemetry dump
    let t3 = Cli::try_parse_from(["zensmu", "telemetry", "dump", "/tmp/pm.bin"]).unwrap();
    match t3.command {
        Command::Telemetry {
            action: Some(TelemetryAction::Dump { out_path }),
        } => assert_eq!(out_path, PathBuf::from("/tmp/pm.bin")),
        other => panic!("unexpected command: {other:?}"),
    }

    // Top
    let top = Cli::try_parse_from(["zensmu", "top", "--interval", "200ms"]).unwrap();
    match top.command {
        Command::Top { interval } => assert_eq!(interval, "200ms"),
        other => panic!("unexpected command: {other:?}"),
    }

    // Profile subcommands (including export alias)
    let psave = Cli::try_parse_from(["zensmu", "profile", "save", "/tmp/p.toml"]).unwrap();
    assert!(matches!(
        psave.command,
        Command::Profile {
            action: ProfileAction::Save { .. }
        }
    ));
    let pexp = Cli::try_parse_from(["zensmu", "profile", "export", "/tmp/p.toml"]).unwrap();
    assert!(matches!(
        pexp.command,
        Command::Profile {
            action: ProfileAction::Save { .. }
        }
    ));
    let pval = Cli::try_parse_from(["zensmu", "profile", "validate", "/tmp/p.toml"]).unwrap();
    assert!(matches!(
        pval.command,
        Command::Profile {
            action: ProfileAction::Validate { .. }
        }
    ));
    let pdiff = Cli::try_parse_from(["zensmu", "profile", "diff", "/tmp/p.toml"]).unwrap();
    assert!(matches!(
        pdiff.command,
        Command::Profile {
            action: ProfileAction::Diff { .. }
        }
    ));
    let pclear = Cli::try_parse_from(["zensmu", "profile", "clear-guard"]).unwrap();
    assert!(matches!(
        pclear.command,
        Command::Profile {
            action: ProfileAction::ClearGuard
        }
    ));
    let plist =
        Cli::try_parse_from(["zensmu", "profile", "list", "--dir", "/tmp/profiles"]).unwrap();
    match plist.command {
        Command::Profile {
            action: ProfileAction::List { dir },
        } => assert_eq!(dir, PathBuf::from("/tmp/profiles")),
        other => panic!("unexpected command: {other:?}"),
    }

    // Doctor, Status, Rollback, Reset, Completions
    assert!(matches!(
        Cli::try_parse_from(["zensmu", "doctor"]).unwrap().command,
        Command::Doctor
    ));
    assert!(matches!(
        Cli::try_parse_from(["zensmu", "status"]).unwrap().command,
        Command::Status
    ));
    assert!(matches!(
        Cli::try_parse_from(["zensmu", "rollback"]).unwrap().command,
        Command::Rollback
    ));
    assert!(matches!(
        Cli::try_parse_from(["zensmu", "reset"]).unwrap().command,
        Command::Reset
    ));
    match Cli::try_parse_from(["zensmu", "completions", "fish"])
        .unwrap()
        .command
    {
        Command::Completions { shell } => assert_eq!(shell, CompletionShell::Fish),
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_persist_subcommands() {
    let st = Cli::try_parse_from(["zensmu", "persist", "status", "--json"]).unwrap();
    assert!(st.json);
    assert!(matches!(
        st.command,
        Command::Persist(PersistCommand {
            action: PersistAction::Status
        })
    ));

    let save_live = Cli::try_parse_from(["zensmu", "persist", "save"]).unwrap();
    match save_live.command {
        Command::Persist(PersistCommand {
            action: PersistAction::Save { from, enable },
        }) => {
            assert_eq!(from, None);
            assert!(!enable);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let save_from_enable = Cli::try_parse_from([
        "zensmu",
        "persist",
        "save",
        "--from",
        "/tmp/daily.toml",
        "--enable",
        "--dry-run",
    ])
    .unwrap();
    assert!(save_from_enable.dry_run);
    match save_from_enable.command {
        Command::Persist(PersistCommand {
            action: PersistAction::Save { from, enable },
        }) => {
            assert_eq!(from, Some(PathBuf::from("/tmp/daily.toml")));
            assert!(enable);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let enable = Cli::try_parse_from(["zensmu", "persist", "enable", "--dry-run"]).unwrap();
    assert!(enable.dry_run);
    assert!(matches!(
        enable.command,
        Command::Persist(PersistCommand {
            action: PersistAction::Enable
        })
    ));

    let disable = Cli::try_parse_from(["zensmu", "persist", "disable"]).unwrap();
    match disable.command {
        Command::Persist(PersistCommand {
            action: PersistAction::Disable { remove_profile },
        }) => assert!(!remove_profile),
        other => panic!("unexpected command: {other:?}"),
    }

    let disable_rm =
        Cli::try_parse_from(["zensmu", "persist", "disable", "--remove-profile", "--json"])
            .unwrap();
    assert!(disable_rm.json);
    match disable_rm.command {
        Command::Persist(PersistCommand {
            action: PersistAction::Disable { remove_profile },
        }) => assert!(remove_profile),
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_report_subcommand_and_aliases() {
    let cli = Cli::try_parse_from([
        "zensmu",
        "report",
        "--note",
        "Ryzen 9950X3D PM table unknown",
        "--raw-pm-table",
        "--no-mailbox",
        "--output",
        "/tmp/zensmu-report.md",
        "--open",
        "--create",
        "--json",
    ])
    .unwrap();

    assert!(cli.json);
    match cli.command {
        Command::Report {
            note,
            raw_pm_table,
            no_mailbox,
            output,
            open,
            create,
        } => {
            assert_eq!(note.as_deref(), Some("Ryzen 9950X3D PM table unknown"));
            assert!(raw_pm_table);
            assert!(no_mailbox);
            assert_eq!(output, Some(PathBuf::from("/tmp/zensmu-report.md")));
            assert!(open);
            assert!(create);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // Test `bug-report` visible alias, `-o` short flag, and `--gh` alias for `--create`
    let alias_cli = Cli::try_parse_from([
        "zensmu",
        "bug-report",
        "-o",
        "/tmp/out.md",
        "--gh",
        "--dry-run",
        "--yes",
    ])
    .unwrap();
    assert!(alias_cli.dry_run);
    assert!(alias_cli.yes);
    match alias_cli.command {
        Command::Report {
            note,
            raw_pm_table,
            no_mailbox,
            output,
            open,
            create,
        } => {
            assert_eq!(note, None);
            assert!(!raw_pm_table);
            assert!(!no_mailbox);
            assert_eq!(output, Some(PathBuf::from("/tmp/out.md")));
            assert!(!open);
            assert!(create);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_driver_status_load_and_install_subcommands() {
    let bare = Cli::try_parse_from(["zensmu", "driver"]).unwrap();
    assert!(matches!(bare.command, Command::Driver { action: None }));

    let st = Cli::try_parse_from(["zensmu", "driver", "status", "--json"]).unwrap();
    assert!(st.json);
    assert!(matches!(
        st.command,
        Command::Driver {
            action: Some(DriverAction::Status)
        }
    ));

    let load = Cli::try_parse_from(["zensmu", "driver", "load", "--no-autoload"]).unwrap();
    assert!(matches!(
        load.command,
        Command::Driver {
            action: Some(DriverAction::Load { no_autoload: true })
        }
    ));

    let inst = Cli::try_parse_from([
        "zensmu",
        "driver",
        "install",
        "--method",
        "source",
        "--release",
        "v0.2.1",
        "--force",
        "--dry-run",
    ])
    .unwrap();
    assert!(inst.dry_run);
    match inst.command {
        Command::Driver {
            action:
                Some(DriverAction::Install {
                    method,
                    release,
                    no_autoload,
                    force,
                }),
        } => {
            assert_eq!(method, DriverMethodArg::Source);
            assert_eq!(release.as_deref(), Some("v0.2.1"));
            assert!(!no_autoload);
            assert!(force);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let dkms_inst =
        Cli::try_parse_from(["zensmu", "driver", "install", "--method", "dkms"]).unwrap();
    assert!(matches!(
        dkms_inst.command,
        Command::Driver {
            action: Some(DriverAction::Install {
                method: DriverMethodArg::Dkms,
                ..
            })
        }
    ));

    let uninst =
        Cli::try_parse_from(["zensmu", "driver", "uninstall", "--force", "--dry-run"]).unwrap();
    assert!(uninst.dry_run);
    assert!(matches!(
        uninst.command,
        Command::Driver {
            action: Some(DriverAction::Uninstall { force: true })
        }
    ));

    let rem = Cli::try_parse_from(["zensmu", "driver", "remove"]).unwrap();
    assert!(matches!(
        rem.command,
        Command::Driver {
            action: Some(DriverAction::Uninstall { force: false })
        }
    ));
}
