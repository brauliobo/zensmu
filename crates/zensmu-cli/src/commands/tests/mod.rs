mod monitoring;
mod system;
mod tuning;

use super::*;
use clap::{CommandFactory, Parser};

#[test]
fn clap_command_debug_assert() {
    Cli::command().debug_assert();
}

#[test]
fn parses_global_flags_before_and_after_subcommand() {
    let cli = Cli::try_parse_from([
        "zensmu",
        "--json",
        "--dry-run",
        "--yes",
        "--allow-extreme",
        "--confirm-within",
        "30s",
        "--sysfs-path",
        "/tmp/mock_smu",
        "--state-dir",
        "/tmp/state",
        "--guard-path",
        "/tmp/guard.lock",
        "--no-cpuid-check",
        "-vv",
        "info",
    ])
    .unwrap();

    assert!(cli.json);
    assert!(cli.dry_run);
    assert!(cli.yes);
    assert!(cli.allow_extreme);
    assert_eq!(cli.confirm_within.as_deref(), Some("30s"));
    assert_eq!(cli.sysfs_path, Some(PathBuf::from("/tmp/mock_smu")));
    assert_eq!(cli.state_dir, Some(PathBuf::from("/tmp/state")));
    assert_eq!(cli.guard_path, Some(PathBuf::from("/tmp/guard.lock")));
    assert!(cli.no_cpuid_check);
    assert_eq!(cli.verbose, 2);
    assert!(!cli.quiet);
    assert!(matches!(cli.command, Command::Info));

    // Global flags placed AFTER subcommand (matching systemd/README invocations)
    let cli2 = Cli::try_parse_from([
        "zensmu",
        "profile",
        "apply",
        "/etc/zensmu/active.toml",
        "--yes",
        "--boot-guard",
        "--settle-secs",
        "15",
        "--json",
    ])
    .unwrap();
    assert!(cli2.yes);
    assert!(cli2.json);
    match cli2.command {
        Command::Profile {
            action:
                ProfileAction::Apply {
                    path,
                    boot_guard,
                    settle_secs,
                },
        } => {
            assert_eq!(path, PathBuf::from("/etc/zensmu/active.toml"));
            assert!(boot_guard);
            assert_eq!(settle_secs, 15);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_manpage_subcommand() {
    let cli = Cli::try_parse_from(["zensmu", "manpage"]).unwrap();
    assert!(matches!(cli.command, Command::Manpage));
}
