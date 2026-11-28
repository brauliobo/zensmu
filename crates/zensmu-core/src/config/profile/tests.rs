use super::*;
use crate::safety::snapshot::SmuStateSnapshot;
use crate::types::{CurveShaperState, ShaperTierMargins};

fn sample_zen5_profile() -> Profile {
    let mut per_core = BTreeMap::new();
    per_core.insert(0, -25);
    per_core.insert(1, -20);

    let mut shaper = BTreeMap::new();
    shaper.insert(
        FreqTier::High,
        ShaperTierProfile {
            low: Some(-10),
            med: Some(-15),
            high: Some(-5),
        },
    );
    shaper.insert(
        FreqTier::Max,
        ShaperTierProfile {
            low: Some(-5),
            med: Some(-10),
            high: Some(0),
        },
    );

    Profile {
        schema: PROFILE_SCHEMA_VERSION,
        target: ProfileTarget {
            codename: Codename::GraniteRidge,
            physical_cores: Some(8),
        },
        curve_optimizer: Some(CoProfile {
            all: Some(-15),
            per_core,
            igpu: Some(-10),
        }),
        curve_shaper: Some(shaper),
        pbo: Some(PboProfile {
            ppt_w: Some(88),
            tdc_a: Some(75),
            edc_a: Some(150),
            tctl_c: Some(85),
            scalar: Some(1),
            fmax_mhz: Some(5550),
        }),
        apu: None,
    }
}

#[test]
fn toml_roundtrip_and_disk_save_load() {
    let profile = sample_zen5_profile();
    let toml_str = profile.to_toml_string().unwrap();
    let parsed = Profile::from_toml_str(&toml_str).unwrap();
    assert_eq!(parsed, profile);

    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("profiles/9700x.toml");
    profile.save(&path).unwrap();
    let loaded = Profile::load(&path).unwrap();
    assert_eq!(loaded, profile);
}

#[test]
fn parses_handwritten_toml_profile() {
    let raw = r#"
schema = 1

[target]
codename = "granite-ridge"
physical_cores = 8

[curve_optimizer]
all = -20
igpu = -10

[curve_optimizer.per_core]
0 = -25
1 = -15

[curve_shaper.high]
low = -10
med = -15
high = -5

[pbo]
ppt_w = 88
tdc_a = 75
edc_a = 150
tctl_c = 85
scalar = 2
fmax_mhz = 5550
"#;
    let profile = Profile::from_toml_str(raw).unwrap();
    assert_eq!(profile.target.codename, Codename::GraniteRidge);
    assert_eq!(
        profile.curve_optimizer.as_ref().unwrap().per_core.get(&0),
        Some(&-25)
    );
    assert!(profile
        .validate_for(Codename::GraniteRidge, 8, false)
        .is_ok());
}

#[test]
fn validation_catches_target_capability_core_and_bounds_errors() {
    let mut p = sample_zen5_profile();
    assert!(p.validate_for(Codename::GraniteRidge, 8, false).is_ok());

    // Codename mismatch
    assert!(matches!(
        p.validate_for(Codename::Raphael, 8, false),
        Err(Error::ProfileTargetMismatch { .. })
    ));

    // Core count mismatch
    assert!(matches!(
        p.validate_for(Codename::GraniteRidge, 12, false),
        Err(Error::ProfileTargetMismatch { .. })
    ));

    // Schema mismatch
    p.schema = 2;
    assert!(matches!(
        p.validate_for(Codename::GraniteRidge, 8, false),
        Err(Error::InvalidInput(_))
    ));
    p.schema = 1;

    // Curve shaper unsupported on Raphael (Zen 4)
    let mut raphael = sample_zen5_profile();
    raphael.target.codename = Codename::Raphael;
    assert!(matches!(
        raphael.validate_for(Codename::Raphael, 8, false),
        Err(Error::UnsupportedCapability {
            capability: "curve_shaper",
            ..
        })
    ));

    // Out-of-bounds core index
    let mut bad_core = sample_zen5_profile();
    bad_core
        .curve_optimizer
        .as_mut()
        .unwrap()
        .per_core
        .insert(8, -10);
    assert!(matches!(
        bad_core.validate_for(Codename::GraniteRidge, 8, false),
        Err(Error::OutOfBounds { .. })
    ));

    // Extreme CO margin requires allow_extreme
    let mut extreme = sample_zen5_profile();
    extreme.curve_optimizer.as_mut().unwrap().all = Some(-45);
    assert!(matches!(
        extreme.validate_for(Codename::GraniteRidge, 8, false),
        Err(Error::ExtremeValueBlocked { .. })
    ));
    assert!(extreme
        .validate_for(Codename::GraniteRidge, 8, true)
        .is_ok());
}

#[test]
fn from_snapshot_roundtrip_has_empty_diff() {
    let mut snap = SmuStateSnapshot::new(Codename::GraniteRidge, 4);
    snap.co_per_core = Some(vec![(0, -25), (1, -20), (2, -20), (3, -15)]);
    snap.igpu_co = Some(-10);
    snap.curve_shaper = Some(CurveShaperState {
        high: ShaperTierMargins {
            low: -10,
            med: -15,
            high: -5,
        },
        ..CurveShaperState::default()
    });
    snap.pbo_scalar = Some(1.0);
    snap.fmax_mhz = Some(5550);
    snap.ppt_w = Some(88);
    snap.tdc_a = Some(75);
    snap.edc_a = Some(150);
    snap.tctl_c = Some(85);

    let profile = Profile::from_snapshot(&snap);
    assert!(profile
        .validate_for(Codename::GraniteRidge, 4, false)
        .is_ok());
    assert!(profile.diff_against(&snap).is_empty());
}

#[test]
fn diff_against_detects_changed_fields() {
    let mut snap = SmuStateSnapshot::new(Codename::GraniteRidge, 4);
    snap.co_per_core = Some(vec![(0, -15), (1, -15), (2, -15), (3, -15)]);
    snap.igpu_co = Some(0);
    snap.ppt_w = Some(88);
    snap.tctl_c = Some(95);

    let mut per_core = BTreeMap::new();
    per_core.insert(0, -25);
    let profile = Profile {
        schema: PROFILE_SCHEMA_VERSION,
        target: ProfileTarget {
            codename: Codename::GraniteRidge,
            physical_cores: Some(4),
        },
        curve_optimizer: Some(CoProfile {
            all: Some(-15),
            per_core,
            igpu: Some(-10),
        }),
        curve_shaper: None,
        pbo: Some(PboProfile {
            ppt_w: Some(88),
            tctl_c: Some(85),
            ..PboProfile::default()
        }),
        apu: None,
    };

    let diffs = profile.diff_against(&snap);
    assert_eq!(
        diffs,
        vec![
            ProfileDiffEntry {
                field: "curve_optimizer.core.0".into(),
                current: "-15".into(),
                desired: "-25".into(),
            },
            ProfileDiffEntry {
                field: "curve_optimizer.igpu".into(),
                current: "0".into(),
                desired: "-10".into(),
            },
            ProfileDiffEntry {
                field: "pbo.tctl_c".into(),
                current: "95°C".into(),
                desired: "85°C".into(),
            },
        ]
    );
}

#[test]
fn apu_profile_validates_on_phoenix_and_rejects_on_desktop() {
    let apu_profile = Profile {
        schema: PROFILE_SCHEMA_VERSION,
        target: ProfileTarget {
            codename: Codename::Phoenix,
            physical_cores: Some(8),
        },
        curve_optimizer: None,
        curve_shaper: None,
        pbo: None,
        apu: Some(ApuProfile {
            stapm_w: Some(35),
            stapm_time_s: Some(120),
            fast_w: Some(54),
            slow_w: Some(45),
            slow_time_s: Some(10),
            apu_slow_w: Some(35),
            tdc_soc_a: Some(20),
            edc_soc_a: Some(35),
            apu_skin_temp_c: Some(80),
            dgpu_skin_temp_c: Some(75),
            power_mode: Some(ApuPowerMode::MaxPerformance),
        }),
    };

    assert!(apu_profile
        .validate_for(Codename::Phoenix, 8, false)
        .is_ok());

    let mut desktop_apu = apu_profile.clone();
    desktop_apu.target.codename = Codename::GraniteRidge;
    assert!(matches!(
        desktop_apu.validate_for(Codename::GraniteRidge, 8, false),
        Err(Error::UnsupportedCapability { .. })
    ));
}

#[test]
fn from_snapshot_on_lucienne_filters_unwritable_fmax_and_captures_all_apu_limits() {
    let mut snap = SmuStateSnapshot::new(Codename::Lucienne, 8);
    snap.pbo_scalar = Some(1.0);
    snap.fmax_mhz = Some(4350); // readable via get_fmax on Lucienne, NOT writable!
    snap.ppt_w = Some(30);
    snap.tdc_a = Some(33);
    snap.edc_a = Some(70);
    snap.tctl_c = Some(80);
    snap.stapm_w = Some(20);
    snap.slow_w = Some(25);
    snap.apu_slow_w = Some(25);
    snap.tdc_soc_a = Some(13);
    snap.edc_soc_a = Some(17);
    snap.apu_skin_temp_c = Some(80);
    snap.dgpu_skin_temp_c = Some(80);

    let profile = Profile::from_snapshot(&snap);
    let pbo = profile.pbo.as_ref().expect("pbo section populated");
    assert_eq!(pbo.fmax_mhz, None, "read-only fmax must not be emitted");
    assert_eq!(pbo.ppt_w, Some(30));
    assert_eq!(pbo.tdc_a, Some(33));
    assert_eq!(pbo.edc_a, Some(70));
    assert_eq!(pbo.tctl_c, Some(80));
    assert_eq!(pbo.scalar, Some(1));

    let apu = profile.apu.as_ref().expect("apu section populated");
    assert_eq!(apu.stapm_w, Some(20));
    assert_eq!(apu.slow_w, Some(25));
    assert_eq!(apu.apu_slow_w, Some(25));
    assert_eq!(apu.tdc_soc_a, Some(13));
    assert_eq!(apu.edc_soc_a, Some(17));
    assert_eq!(apu.apu_skin_temp_c, Some(80));
    assert_eq!(apu.dgpu_skin_temp_c, Some(80));

    // Validates cleanly with allow_extreme=true (as persist/boot service does) and diffs empty
    assert!(profile.validate_for(Codename::Lucienne, 8, true).is_ok());
    assert!(profile.diff_against(&snap).is_empty());
}

#[test]
fn from_snapshot_on_vermeer_filters_unwritable_fmax() {
    let mut snap = SmuStateSnapshot::new(Codename::Vermeer, 16);
    snap.co_per_core = Some((0..16).map(|i| (i, -10)).collect());
    snap.pbo_scalar = Some(1.0);
    snap.fmax_mhz = Some(5150); // read-only on Vermeer
    snap.ppt_w = Some(1000);
    snap.tdc_a = Some(140);
    snap.edc_a = Some(220);
    snap.tctl_c = Some(75);

    let profile = Profile::from_snapshot(&snap);
    assert_eq!(profile.pbo.as_ref().unwrap().fmax_mhz, None);
    assert!(profile.apu.is_none());
    assert!(profile.validate_for(Codename::Vermeer, 16, true).is_ok());
    assert!(profile.diff_against(&snap).is_empty());
}

#[test]
fn from_snapshot_filters_zero_nan_empty_and_unsupported_fields() {
    let mut snap = SmuStateSnapshot::new(Codename::GraniteRidge, 8);
    snap.co_per_core = Some(vec![]);
    snap.pbo_scalar = Some(f32::NAN);
    snap.ppt_w = Some(0);
    snap.fmax_mhz = Some(0);

    let profile = Profile::from_snapshot(&snap);
    assert!(profile.curve_optimizer.is_none());
    assert!(profile.pbo.is_none());
    assert!(profile
        .validate_for(Codename::GraniteRidge, 8, false)
        .is_ok());

    // Non-finite or out-of-range scalar values are dropped cleanly
    snap.pbo_scalar = Some(99.0);
    let p2 = Profile::from_snapshot(&snap);
    assert!(p2.pbo.is_none());

    // Unsupported codename emits no writable sections
    let mut unknown = SmuStateSnapshot::new(Codename::Undefined, 4);
    unknown.ppt_w = Some(88);
    let p3 = Profile::from_snapshot(&unknown);
    assert!(p3.pbo.is_none());
    assert!(p3.apu.is_none());
}

#[test]
fn validate_for_matches_live_op_bounds_on_edge_cases() {
    // Hard Tctl ceiling 95°C cannot be bypassed with allow_extreme
    let mut p = sample_zen5_profile();
    p.pbo.as_mut().unwrap().tctl_c = Some(96);
    assert!(matches!(
        p.validate_for(Codename::GraniteRidge, 8, true),
        Err(Error::OutOfBounds { .. })
    ));

    // iGPU CO enforces -30..=30 (soft -25), even on Zen 5 where CPU CO is -50..=50
    let mut p = sample_zen5_profile();
    p.curve_optimizer.as_mut().unwrap().igpu = Some(-28);
    assert!(matches!(
        p.validate_for(Codename::GraniteRidge, 8, false),
        Err(Error::ExtremeValueBlocked { .. })
    ));
    p.curve_optimizer.as_mut().unwrap().igpu = Some(-35);
    assert!(matches!(
        p.validate_for(Codename::GraniteRidge, 8, true),
        Err(Error::OutOfBounds { .. })
    ));

    // PBO scalar > 4x requires allow_extreme
    let mut p = sample_zen5_profile();
    p.pbo.as_mut().unwrap().scalar = Some(6);
    assert!(matches!(
        p.validate_for(Codename::GraniteRidge, 8, false),
        Err(Error::ExtremeValueBlocked { .. })
    ));
    assert!(p.validate_for(Codename::GraniteRidge, 8, true).is_ok());

    // Curve Shaper hard max is +30 (not +50)
    let mut p = sample_zen5_profile();
    p.curve_shaper
        .as_mut()
        .unwrap()
        .get_mut(&FreqTier::High)
        .unwrap()
        .high = Some(31);
    assert!(matches!(
        p.validate_for(Codename::GraniteRidge, 8, true),
        Err(Error::OutOfBounds { .. })
    ));
}
