use super::super::*;
use clap::Parser;
use std::path::PathBuf;
use zensmu_core::bench::BenchWorkload;
use zensmu_core::metrics::DEFAULT_METRICS_LISTEN;

#[test]
fn parses_record_direct_and_pts_flags() {
    let default_rec = Cli::try_parse_from(["zensmu", "record"]).unwrap();
    match default_rec.command {
        Command::Record(rec) => {
            assert_eq!(rec.output, None);
            assert_eq!(rec.pts_name, None);
            assert_eq!(rec.format, None);
            assert_eq!(rec.interval, "1s");
            assert_eq!(rec.duration, None);
            assert_eq!(rec.samples, None);
            assert_eq!(rec.max_size, None);
            assert_eq!(rec.rotate, 5);
            assert!(!rec.append);
            assert_eq!(rec.tag, None);
            assert!(rec.action.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let daemon_rec = Cli::try_parse_from([
        "zensmu",
        "record",
        "--interval",
        "2s",
        "--max-size",
        "50",
        "--rotate",
        "5",
        "--append",
        "--tag",
        "co-20",
        "-n",
        "100",
        "-d",
        "60s",
        "-o",
        "/tmp/out.jsonl",
        "-f",
        "jsonl",
    ])
    .unwrap();
    match daemon_rec.command {
        Command::Record(rec) => {
            assert_eq!(rec.output, Some(PathBuf::from("/tmp/out.jsonl")));
            assert_eq!(rec.format, Some(RecordFormatArg::Jsonl));
            assert_eq!(rec.interval, "2s");
            assert_eq!(rec.duration.as_deref(), Some("60s"));
            assert_eq!(rec.samples, Some(100));
            assert_eq!(rec.max_size, Some(50));
            assert_eq!(rec.rotate, 5);
            assert!(rec.append);
            assert_eq!(rec.tag.as_deref(), Some("co-20"));
            assert!(rec.action.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // `--period` alias for `--duration`
    let period_rec = Cli::try_parse_from(["zensmu", "record", "--period", "5s"]).unwrap();
    match period_rec.command {
        Command::Record(rec) => {
            assert_eq!(rec.duration.as_deref(), Some("5s"));
            assert!(rec.action.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    // `--pts-name` shortcut
    let pts_rec = Cli::try_parse_from([
        "zensmu",
        "record",
        "--pts-name",
        "ryzen-9700x-co20",
        "-i",
        "500ms",
    ])
    .unwrap();
    match pts_rec.command {
        Command::Record(rec) => {
            assert_eq!(rec.pts_name.as_deref(), Some("ryzen-9700x-co20"));
            assert_eq!(rec.interval, "500ms");
            assert!(rec.action.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_record_service_and_export_pts_subcommands() {
    let st = Cli::try_parse_from(["zensmu", "record", "service", "status", "--json"]).unwrap();
    assert!(st.json);
    assert!(matches!(
        st.command,
        Command::Record(RecordCommand {
            action: Some(RecordAction::Service {
                action: RecordServiceAction::Status
            }),
            ..
        })
    ));

    let en = Cli::try_parse_from(["zensmu", "record", "service", "enable"]).unwrap();
    assert!(matches!(
        en.command,
        Command::Record(RecordCommand {
            action: Some(RecordAction::Service {
                action: RecordServiceAction::Enable
            }),
            ..
        })
    ));

    let dis = Cli::try_parse_from(["zensmu", "record", "service", "disable"]).unwrap();
    assert!(matches!(
        dis.command,
        Command::Record(RecordCommand {
            action: Some(RecordAction::Service {
                action: RecordServiceAction::Disable
            }),
            ..
        })
    ));

    let exp_pts = Cli::try_parse_from([
        "zensmu",
        "record",
        "export-pts",
        "/tmp/telemetry.csv",
        "--pts-name",
        "prime95-run",
        "--title",
        "Prime95 Small FFT",
        "--identifier",
        "co-20",
    ])
    .unwrap();
    match exp_pts.command {
        Command::Record(RecordCommand {
            action:
                Some(RecordAction::ExportPts {
                    input,
                    output,
                    pts_name,
                    title,
                    identifier,
                }),
            ..
        }) => {
            assert_eq!(input, PathBuf::from("/tmp/telemetry.csv"));
            assert_eq!(output, None);
            assert_eq!(pts_name.as_deref(), Some("prime95-run"));
            assert_eq!(title.as_deref(), Some("Prime95 Small FFT"));
            assert_eq!(identifier.as_deref(), Some("co-20"));
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let exp_out = Cli::try_parse_from([
        "zensmu",
        "record",
        "export-pts",
        "/tmp/telemetry.jsonl",
        "-o",
        "/tmp/composite.xml",
        "--json",
    ])
    .unwrap();
    assert!(exp_out.json);
    match exp_out.command {
        Command::Record(RecordCommand {
            action:
                Some(RecordAction::ExportPts {
                    input,
                    output,
                    pts_name,
                    title,
                    identifier,
                }),
            ..
        }) => {
            assert_eq!(input, PathBuf::from("/tmp/telemetry.jsonl"));
            assert_eq!(output, Some(PathBuf::from("/tmp/composite.xml")));
            assert_eq!(pts_name, None);
            assert_eq!(title, None);
            assert_eq!(identifier, None);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_test_and_bench_alias_subcommands() {
    let defaults = Cli::try_parse_from(["zensmu", "test"]).unwrap();
    match defaults.command {
        Command::Test(t) => {
            assert_eq!(t.cores, None);
            assert_eq!(t.duration, "30s");
            assert_eq!(t.passes, 1);
            assert_eq!(t.workload, BenchWorkloadArg::Mixed);
            assert_eq!(t.interval, "500ms");
            assert!(!t.continue_on_error);
            assert!(!t.rollback_on_fail);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let custom = Cli::try_parse_from([
        "zensmu",
        "test",
        "--cores",
        "0,1,4..7",
        "-d",
        "45s",
        "-p",
        "3",
        "-w",
        "fp",
        "-i",
        "250ms",
        "--continue-on-error",
        "--rollback-on-fail",
        "--json",
    ])
    .unwrap();
    assert!(custom.json);
    match custom.command {
        Command::Test(t) => {
            assert_eq!(t.cores.as_deref(), Some("0,1,4..7"));
            assert_eq!(t.duration, "45s");
            assert_eq!(t.passes, 3);
            assert_eq!(t.workload, BenchWorkloadArg::Fp);
            assert_eq!(
                BenchWorkload::from(t.workload),
                BenchWorkload::FloatingPoint
            );
            assert_eq!(t.interval, "250ms");
            assert!(t.continue_on_error);
            assert!(t.rollback_on_fail);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let bench_alias = Cli::try_parse_from([
        "zensmu",
        "bench",
        "--workload",
        "cache",
        "--duration",
        "10s",
        "--passes",
        "2",
    ])
    .unwrap();
    match bench_alias.command {
        Command::Test(t) => {
            assert_eq!(t.workload, BenchWorkloadArg::Cache);
            assert_eq!(BenchWorkload::from(t.workload), BenchWorkload::Cache);
            assert_eq!(t.duration, "10s");
            assert_eq!(t.passes, 2);
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let integer_wl = Cli::try_parse_from(["zensmu", "test", "-w", "integer"]).unwrap();
    match integer_wl.command {
        Command::Test(t) => {
            assert_eq!(t.workload, BenchWorkloadArg::Integer);
            assert_eq!(BenchWorkload::from(t.workload), BenchWorkload::Integer);
        }
        other => panic!("unexpected command: {other:?}"),
    }
}

#[test]
fn parses_export_and_export_service_subcommands() {
    let defaults = Cli::try_parse_from(["zensmu", "export"]).unwrap();
    match defaults.command {
        Command::Export(e) => {
            assert_eq!(e.listen, DEFAULT_METRICS_LISTEN);
            assert!(!e.no_per_core);
            assert!(e.action.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let custom = Cli::try_parse_from([
        "zensmu",
        "export",
        "--listen",
        "0.0.0.0:9100",
        "--no-per-core",
    ])
    .unwrap();
    match custom.command {
        Command::Export(e) => {
            assert_eq!(e.listen, "0.0.0.0:9100");
            assert!(e.no_per_core);
            assert!(e.action.is_none());
        }
        other => panic!("unexpected command: {other:?}"),
    }

    let st = Cli::try_parse_from(["zensmu", "export", "service", "status", "--json"]).unwrap();
    assert!(st.json);
    assert!(matches!(
        st.command,
        Command::Export(ExportCommand {
            action: Some(ExportAction::Service {
                action: ExportServiceAction::Status
            }),
            ..
        })
    ));

    let en = Cli::try_parse_from(["zensmu", "export", "service", "enable", "--dry-run"]).unwrap();
    assert!(en.dry_run);
    assert!(matches!(
        en.command,
        Command::Export(ExportCommand {
            action: Some(ExportAction::Service {
                action: ExportServiceAction::Enable
            }),
            ..
        })
    ));

    let dis = Cli::try_parse_from(["zensmu", "export", "service", "disable"]).unwrap();
    assert!(matches!(
        dis.command,
        Command::Export(ExportCommand {
            action: Some(ExportAction::Service {
                action: ExportServiceAction::Disable
            }),
            ..
        })
    ));
}
