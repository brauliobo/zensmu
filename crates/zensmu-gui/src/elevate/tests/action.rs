use super::super::*;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use zensmu_core::ops::apu::ApuSetRequest;
use zensmu_core::ops::pbo::PboSetRequest;
use zensmu_core::types::{ApuPowerMode, CurveShaperState, FreqTier, ShaperTierMargins};

#[test]
fn proc_status_root_detection_parses_uid_line() {
    let normal = "Name:\tzensmu-gui\nUid:\t1000\t1000\t1000\t1000\nGid:\t1000\t1000\n";
    assert!(!parse_is_root_from_proc_status(normal));

    let root_real = "Name:\tzensmu-gui\nUid:\t0\t0\t0\t0\n";
    assert!(parse_is_root_from_proc_status(root_real));

    let setuid_root = "Name:\tzensmu-gui\nUid:\t1000\t0\t0\t0\n";
    assert!(parse_is_root_from_proc_status(setuid_root));
}

#[test]
fn cli_arg_construction_matches_expected_flags() {
    assert_eq!(
        CliAction::Status.to_arg_batches(),
        vec![vec!["status", "--json"]]
    );
    assert_eq!(
        CliAction::CoGet.to_arg_batches(),
        vec![vec!["co", "get", "--json"]]
    );
    assert_eq!(
        CliAction::ShaperGet.to_arg_batches(),
        vec![vec!["shaper", "get", "--json"]]
    );
    assert_eq!(
        CliAction::PboGet.to_arg_batches(),
        vec![vec!["pbo", "get", "--json"]]
    );

    let co_dry = CliAction::CoSetAll {
        margin: -25,
        allow_extreme: false,
        dry_run: true,
    };
    assert_eq!(
        co_dry.to_arg_batches(),
        vec![vec!["co", "set", "--all=-25", "--dry-run", "--json"]]
    );
    assert!(!co_dry.requires_privilege());

    let co_live = co_dry.into_live();
    assert_eq!(
        co_live.to_arg_batches(),
        vec![vec!["co", "set", "--all=-25", "--yes", "--json"]]
    );
    assert!(co_live.requires_privilege());

    let co_cores = CliAction::CoSetCores {
        updates: vec![(0, -15), (3, -38)],
        allow_extreme: true,
        dry_run: true,
    };
    assert_eq!(
        co_cores.to_arg_batches(),
        vec![vec![
            "co",
            "set",
            "--core",
            "0=-15,3=-38",
            "--allow-extreme",
            "--dry-run",
            "--json"
        ]]
    );

    assert_eq!(
        CliAction::CoReset { dry_run: true }.to_arg_batches(),
        vec![vec!["co", "reset", "--dry-run", "--json"]]
    );

    let co_igpu = CliAction::CoSetIgpu {
        margin: -15,
        allow_extreme: false,
        dry_run: true,
    };
    assert!(!co_igpu.requires_privilege());
    assert_eq!(
        co_igpu.to_arg_batches(),
        vec![vec!["co", "set", "--igpu=-15", "--dry-run", "--json"]]
    );
    let co_igpu_live = co_igpu.into_live();
    assert!(co_igpu_live.requires_privilege());
    assert_eq!(
        co_igpu_live.to_arg_batches(),
        vec![vec!["co", "set", "--igpu=-15", "--yes", "--json"]]
    );

    let shaper_all = CliAction::ShaperSetAll {
        state: CurveShaperState::default(),
        allow_extreme: true,
        dry_run: true,
    };
    assert!(!shaper_all.requires_privilege());
    let shaper_batches = shaper_all.to_arg_batches();
    assert_eq!(shaper_batches.len(), 5);
    assert_eq!(
        shaper_batches[0],
        vec![
            "shaper",
            "set",
            "--freq",
            "min",
            "--temp",
            "low=0,med=0,high=0",
            "--allow-extreme",
            "--dry-run",
            "--json"
        ]
    );
    assert_eq!(shaper_batches[4][3], "max");

    let shaper_tier = CliAction::ShaperSetTier {
        tier: FreqTier::High,
        margins: ShaperTierMargins {
            low: -10,
            med: -15,
            high: -20,
        },
        allow_extreme: false,
        dry_run: false,
    };
    assert_eq!(
        shaper_tier.to_arg_batches(),
        vec![vec![
            "shaper",
            "set",
            "--freq",
            "high",
            "--temp",
            "low=-10,med=-15,high=-20",
            "--yes",
            "--json"
        ]]
    );

    assert_eq!(
        CliAction::ShaperReset { dry_run: false }.to_arg_batches(),
        vec![vec!["shaper", "reset", "--yes", "--json"]]
    );
    assert_eq!(
        CliAction::PboReset { dry_run: true }.to_arg_batches(),
        vec![vec!["pbo", "reset", "--dry-run", "--json"]]
    );

    let pbo = CliAction::PboSet {
        req: PboSetRequest {
            ppt_w: Some(142),
            tdc_a: Some(110),
            edc_a: Some(170),
            tctl_c: Some(85),
            scalar: Some(2),
            fmax_mhz: Some(5650),
        },
        allow_extreme: false,
        dry_run: true,
    };
    assert_eq!(
        pbo.to_arg_batches(),
        vec![vec![
            "pbo",
            "set",
            "--ppt",
            "142",
            "--tdc",
            "110",
            "--edc",
            "170",
            "--tctl",
            "85",
            "--scalar",
            "2",
            "--fmax",
            "5650",
            "--dry-run",
            "--json"
        ]]
    );

    let profile = CliAction::ProfileApply {
        path: PathBuf::from("/etc/zensmu/active.toml"),
        allow_extreme: false,
        dry_run: true,
    };
    assert_eq!(
        profile.to_arg_batches(),
        vec![vec![
            "profile",
            "apply",
            "/etc/zensmu/active.toml",
            "--dry-run",
            "--json"
        ]]
    );

    let export = CliAction::ProfileExport {
        path: PathBuf::from("/tmp/zensmu-export.toml"),
    };
    assert!(export.requires_privilege());
    assert_eq!(
        export.to_arg_batches(),
        vec![vec![
            "profile",
            "export",
            "/tmp/zensmu-export.toml",
            "--json"
        ]]
    );

    let rollback_dry = CliAction::Rollback { dry_run: true };
    assert!(rollback_dry.requires_privilege());
    assert_eq!(
        rollback_dry.to_arg_batches(),
        vec![vec!["rollback", "--dry-run", "--json"]]
    );
    let reset_live = CliAction::Reset { dry_run: false };
    assert!(reset_live.requires_privilege());
    assert_eq!(
        reset_live.to_arg_batches(),
        vec![vec!["reset", "--yes", "--json"]]
    );
    assert!(CliAction::PboReset { dry_run: true }.requires_privilege());
}

#[test]
fn command_spec_uses_pkexec_only_when_non_root_and_elevated() {
    let bin = Path::new("/usr/bin/zensmu");
    let args = vec!["status".to_string(), "--json".to_string()];

    let elevated = build_command_spec(bin, &args, true, false);
    assert!(elevated.uses_pkexec);
    assert_eq!(elevated.program, PathBuf::from("pkexec"));
    assert_eq!(elevated.args, vec!["/usr/bin/zensmu", "status", "--json"]);

    let as_root = build_command_spec(bin, &args, true, true);
    assert!(!as_root.uses_pkexec);
    assert_eq!(as_root.program, PathBuf::from("/usr/bin/zensmu"));
    assert_eq!(as_root.args, vec!["status", "--json"]);

    let unpriv = build_command_spec(bin, &args, false, false);
    assert!(!unpriv.uses_pkexec);
    assert_eq!(unpriv.program, PathBuf::from("/usr/bin/zensmu"));
}

#[test]
fn find_zensmu_binary_prefers_sibling_then_path_then_system_fallback() {
    let fallback = find_zensmu_binary_with(None, Some("/nonexistent/dir"));
    assert_eq!(fallback, PathBuf::from(SYSTEM_ZENSMU_PATH));

    let temp_base = env::temp_dir().join(format!("zensmu_gui_test_{}", std::process::id()));
    let sib_dir = temp_base.join("sib");
    let path_dir = temp_base.join("path");
    let _ = fs::create_dir_all(&sib_dir);
    let _ = fs::create_dir_all(&path_dir);

    let path_bin = path_dir.join("zensmu");
    fs::write(&path_bin, b"#!/bin/sh\n").unwrap();

    let found_path = find_zensmu_binary_with(Some(&sib_dir), path_dir.to_str());
    assert_eq!(found_path, path_bin);

    let sib_bin = sib_dir.join("zensmu");
    fs::write(&sib_bin, b"#!/bin/sh\n").unwrap();

    let found_sib = find_zensmu_binary_with(Some(&sib_dir), path_dir.to_str());
    assert_eq!(found_sib, sib_bin);

    let _ = fs::remove_dir_all(&temp_base);
}

#[test]
fn persist_and_clear_guard_cli_args_match_frozen_contract() {
    let save_live = CliAction::PersistSave {
        from_path: None,
        enable: true,
        allow_extreme: false,
    };
    assert!(save_live.requires_privilege());
    assert_eq!(
        save_live.to_arg_batches(),
        vec![vec!["persist", "save", "--enable", "--json"]]
    );

    let save_from = CliAction::PersistSave {
        from_path: Some("/tmp/zensmu-gui-persist-42.toml".into()),
        enable: true,
        allow_extreme: true,
    };
    assert!(save_from.requires_privilege());
    assert_eq!(
        save_from.to_arg_batches(),
        vec![vec![
            "persist",
            "save",
            "--from",
            "/tmp/zensmu-gui-persist-42.toml",
            "--enable",
            "--allow-extreme",
            "--json"
        ]]
    );

    let enable_svc = CliAction::PersistSetService {
        enable: true,
        remove_profile: false,
        allow_extreme: true,
    };
    assert!(enable_svc.requires_privilege());
    assert_eq!(
        enable_svc.to_arg_batches(),
        vec![vec!["persist", "enable", "--allow-extreme", "--json"]]
    );

    let disable_svc = CliAction::PersistSetService {
        enable: false,
        remove_profile: false,
        allow_extreme: false,
    };
    assert!(disable_svc.requires_privilege());
    assert_eq!(
        disable_svc.to_arg_batches(),
        vec![vec!["persist", "disable", "--json"]]
    );

    let disable_rm = CliAction::PersistSetService {
        enable: false,
        remove_profile: true,
        allow_extreme: false,
    };
    assert!(disable_rm.requires_privilege());
    assert_eq!(
        disable_rm.to_arg_batches(),
        vec![vec!["persist", "disable", "--remove-profile", "--json"]]
    );

    assert!(CliAction::ClearBootGuard.requires_privilege());
    assert_eq!(
        CliAction::ClearBootGuard.to_arg_batches(),
        vec![vec!["profile", "clear-guard", "--json"]]
    );
}

#[test]
fn apu_set_cli_args_build_all_and_partial_fields_with_write_flags() {
    let full_dry = CliAction::ApuSet {
        req: ApuSetRequest {
            stapm_w: Some(45),
            stapm_time_s: Some(120),
            fast_w: Some(54),
            slow_w: Some(45),
            slow_time_s: Some(10),
            apu_slow_w: Some(35),
            tdc_soc_a: Some(30),
            edc_soc_a: Some(50),
            apu_skin_temp_c: Some(55),
            dgpu_skin_temp_c: Some(60),
            power_mode: Some(ApuPowerMode::MaxPerformance),
        },
        allow_extreme: false,
        dry_run: true,
    };
    assert!(full_dry.is_dry_run());
    assert!(!full_dry.requires_privilege());
    assert_eq!(full_dry.title(), "Preview APU DPTC Limits");
    assert_eq!(
        full_dry.to_arg_batches(),
        vec![vec![
            "apu",
            "set",
            "--stapm",
            "45W",
            "--stapm-time",
            "120s",
            "--fast",
            "54W",
            "--slow",
            "45W",
            "--slow-time",
            "10s",
            "--apu-slow",
            "35W",
            "--tdc-soc",
            "30A",
            "--edc-soc",
            "50A",
            "--apu-skin",
            "55C",
            "--dgpu-skin",
            "60C",
            "--mode",
            "max-performance",
            "--dry-run",
            "--json",
        ]]
    );

    let full_live = full_dry.into_live();
    assert!(!full_live.is_dry_run());
    assert!(full_live.requires_privilege());
    assert_eq!(full_live.title(), "Apply APU DPTC Limits");
    assert_eq!(
        full_live.to_arg_batches(),
        vec![vec![
            "apu",
            "set",
            "--stapm",
            "45W",
            "--stapm-time",
            "120s",
            "--fast",
            "54W",
            "--slow",
            "45W",
            "--slow-time",
            "10s",
            "--apu-slow",
            "35W",
            "--tdc-soc",
            "30A",
            "--edc-soc",
            "50A",
            "--apu-skin",
            "55C",
            "--dgpu-skin",
            "60C",
            "--mode",
            "max-performance",
            "--yes",
            "--json",
        ]]
    );

    let partial_extreme = CliAction::ApuSet {
        req: ApuSetRequest {
            stapm_w: Some(115),
            apu_skin_temp_c: Some(70),
            power_mode: Some(ApuPowerMode::PowerSaving),
            ..Default::default()
        },
        allow_extreme: true,
        dry_run: true,
    };
    assert!(partial_extreme.is_dry_run());
    assert!(!partial_extreme.requires_privilege());
    assert_eq!(
        partial_extreme.to_arg_batches(),
        vec![vec![
            "apu",
            "set",
            "--stapm",
            "115W",
            "--apu-skin",
            "70C",
            "--mode",
            "power-saving",
            "--allow-extreme",
            "--dry-run",
            "--json",
        ]]
    );

    let partial_live = partial_extreme.into_live();
    assert!(partial_live.requires_privilege());
    assert_eq!(
        partial_live.to_arg_batches(),
        vec![vec![
            "apu",
            "set",
            "--stapm",
            "115W",
            "--apu-skin",
            "70C",
            "--mode",
            "power-saving",
            "--allow-extreme",
            "--yes",
            "--json",
        ]]
    );
}

#[test]
fn report_cli_action_builds_expected_args_and_requires_privilege() {
    let minimal = CliAction::Report {
        raw_pm_table: false,
        note: None,
    };
    assert!(minimal.requires_privilege());
    assert!(!minimal.is_dry_run());
    assert_eq!(minimal.title(), "Collect Hardware Report");
    assert_eq!(minimal.to_arg_batches(), vec![vec!["report", "--json"]]);

    let blank_note = CliAction::Report {
        raw_pm_table: true,
        note: Some("   ".into()),
    };
    assert_eq!(
        blank_note.to_arg_batches(),
        vec![vec!["report", "--json", "--raw-pm-table"]]
    );

    let full = CliAction::Report {
        raw_pm_table: true,
        note: Some("  B450M DS3H V2 EDC clamped at 120A  ".into()),
    };
    assert!(full.requires_privilege());
    assert!(!full.is_dry_run());
    assert_eq!(
        full.to_arg_batches(),
        vec![vec![
            "report",
            "--json",
            "--raw-pm-table",
            "--note",
            "B450M DS3H V2 EDC clamped at 120A",
        ]]
    );
}
