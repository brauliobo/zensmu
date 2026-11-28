use super::super::*;
use clap::Parser;

#[test]
fn parses_co_subcommands_with_negative_values() {
    let get = Cli::try_parse_from(["zensmu", "co", "get"]).unwrap();
    assert!(matches!(
        get.command,
        Command::Co {
            action: CoAction::Get
        }
    ));

    let set = Cli::try_parse_from([
        "zensmu",
        "co",
        "set",
        "--all",
        "-20",
        "--core",
        "0=-25",
        "--core",
        "2..7=-15",
        "--igpu",
        "-10",
        "--dry-run",
    ])
    .unwrap();
    assert!(set.dry_run);
    match set.command {
        Command::Co {
            action: CoAction::Set { all, cores, igpu },
        } => {
            assert_eq!(all, Some(-20));
            assert_eq!(cores, vec!["0=-25".to_string(), "2..7=-15".to_string()]);
            assert_eq!(igpu, Some(-10));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let reset = Cli::try_parse_from(["zensmu", "co", "reset"]).unwrap();
    assert!(matches!(
        reset.command,
        Command::Co {
            action: CoAction::Reset
        }
    ));
}

#[test]
fn parses_shaper_subcommands() {
    let get = Cli::try_parse_from(["zensmu", "shaper", "get"]).unwrap();
    assert!(matches!(
        get.command,
        Command::Shaper {
            action: ShaperAction::Get
        }
    ));

    let set_tier = Cli::try_parse_from([
        "zensmu",
        "shaper",
        "set",
        "--freq",
        "high",
        "--temp",
        "low=-10,med=-15,high=-5",
    ])
    .unwrap();
    match set_tier.command {
        Command::Shaper {
            action: ShaperAction::Set { freq, temp, all },
        } => {
            assert_eq!(freq.as_deref(), Some("high"));
            assert_eq!(temp.as_deref(), Some("low=-10,med=-15,high=-5"));
            assert_eq!(all, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let set_all = Cli::try_parse_from(["zensmu", "shaper", "set", "--all", "-10"]).unwrap();
    match set_all.command {
        Command::Shaper {
            action: ShaperAction::Set { freq, temp, all },
        } => {
            assert_eq!(freq, None);
            assert_eq!(temp, None);
            assert_eq!(all, Some(-10));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let reset = Cli::try_parse_from(["zensmu", "shaper", "reset"]).unwrap();
    assert!(matches!(
        reset.command,
        Command::Shaper {
            action: ShaperAction::Reset
        }
    ));
}

#[test]
fn parses_pbo_subcommands_and_tjmax_alias() {
    let get = Cli::try_parse_from(["zensmu", "pbo", "get"]).unwrap();
    assert!(matches!(
        get.command,
        Command::Pbo {
            action: PboAction::Get
        }
    ));

    let set = Cli::try_parse_from([
        "zensmu", "pbo", "set", "--ppt", "142W", "--tdc", "110A", "--edc", "170A", "--tctl", "85C",
        "--scalar", "2", "--fmax", "5.65GHz",
    ])
    .unwrap();
    match set.command {
        Command::Pbo {
            action:
                PboAction::Set {
                    ppt,
                    tdc,
                    edc,
                    tctl,
                    scalar,
                    fmax,
                },
        } => {
            assert_eq!(ppt.as_deref(), Some("142W"));
            assert_eq!(tdc.as_deref(), Some("110A"));
            assert_eq!(edc.as_deref(), Some("170A"));
            assert_eq!(tctl.as_deref(), Some("85C"));
            assert_eq!(scalar, Some(2));
            assert_eq!(fmax.as_deref(), Some("5.65GHz"));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let set_alias = Cli::try_parse_from(["zensmu", "pbo", "set", "--tjmax", "85"]).unwrap();
    match set_alias.command {
        Command::Pbo {
            action: PboAction::Set { tctl, .. },
        } => assert_eq!(tctl.as_deref(), Some("85")),
        other => panic!("unexpected command: {other:?}"),
    }

    let reset = Cli::try_parse_from(["zensmu", "pbo", "reset"]).unwrap();
    assert!(matches!(
        reset.command,
        Command::Pbo {
            action: PboAction::Reset
        }
    ));
}

#[test]
fn parses_apu_set_all_flags() {
    let cli = Cli::try_parse_from([
        "zensmu",
        "apu",
        "set",
        "--stapm",
        "28W",
        "--stapm-time",
        "120s",
        "--fast",
        "45W",
        "--slow",
        "35W",
        "--slow-time",
        "10s",
        "--apu-slow",
        "30W",
        "--tdc-soc",
        "30A",
        "--edc-soc",
        "50A",
        "--apu-skin",
        "45C",
        "--dgpu-skin",
        "60C",
        "--mode",
        "power-saving",
    ])
    .unwrap();

    match cli.command {
        Command::Apu {
            action:
                ApuAction::Set {
                    stapm,
                    stapm_time,
                    fast,
                    slow,
                    slow_time,
                    apu_slow,
                    tdc_soc,
                    edc_soc,
                    apu_skin,
                    dgpu_skin,
                    mode,
                },
        } => {
            assert_eq!(stapm.as_deref(), Some("28W"));
            assert_eq!(stapm_time.as_deref(), Some("120s"));
            assert_eq!(fast.as_deref(), Some("45W"));
            assert_eq!(slow.as_deref(), Some("35W"));
            assert_eq!(slow_time.as_deref(), Some("10s"));
            assert_eq!(apu_slow.as_deref(), Some("30W"));
            assert_eq!(tdc_soc.as_deref(), Some("30A"));
            assert_eq!(edc_soc.as_deref(), Some("50A"));
            assert_eq!(apu_skin.as_deref(), Some("45C"));
            assert_eq!(dgpu_skin.as_deref(), Some("60C"));
            assert_eq!(mode, Some(ApuModeArg::PowerSaving));
        }
        other => panic!("unexpected command: {other:?}"),
    }
}
