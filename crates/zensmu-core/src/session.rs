//! High-level transactional SMU session helpers shared by CLI, TUI, and GUI:
//! snapshot capture, rollback/restore, and validated declarative profile application.

use crate::config::profile::Profile;
use crate::error::Result;
use crate::hw::identity::HardwareContext;
use crate::ops::{apu, co, pbo, pm_table, shaper};
use crate::safety::snapshot::SmuStateSnapshot;
use crate::transport::SmuTransport;
use crate::types::{FreqTier, PlannedOp, ShaperTierMargins};

/// Capture all currently readable SMU state into a `SmuStateSnapshot`.
/// Optional subsystems (e.g. Curve Shaper on Zen 4, PM table on unverified versions)
/// are recorded as `None` rather than failing the snapshot.
pub fn capture_snapshot(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
) -> Result<SmuStateSnapshot> {
    let mut snap = SmuStateSnapshot::new(hw.codename, hw.topology.physical_cores);

    if hw.capabilities.get_co_per_core.is_some() {
        if let Ok(co_snap) = co::get_co(transport, hw) {
            snap.co_per_core = Some(
                co_snap
                    .cores
                    .iter()
                    .map(|c| (c.core_index, c.margin))
                    .collect(),
            );
            snap.igpu_co = co_snap.igpu;
        }
    }

    if hw.capabilities.get_curve_shaper.is_some() {
        if let Ok(cs) = shaper::get_curve_shaper(transport, hw) {
            snap.curve_shaper = Some(cs);
        }
    }

    if let Ok(pbo_st) = pbo::get_pbo_status(transport, hw) {
        snap.pbo_scalar = pbo_st.scalar;
        snap.fmax_mhz = pbo_st.fmax_mhz;
    }

    if let Ok(telem) = pm_table::read_telemetry(transport, hw) {
        let pos_u32 = |opt: Option<f32>| opt.filter(|&v| v >= 1.0).map(|v| v.round() as u32);
        snap.ppt_w = pos_u32(telem.ppt_limit_w);
        snap.tdc_a = pos_u32(telem.tdc_limit_a);
        snap.edc_a = pos_u32(telem.edc_limit_a);
        snap.tctl_c = pos_u32(telem.tctl_limit_c);
        snap.stapm_w = pos_u32(telem.stapm_limit_w);
        snap.slow_w = pos_u32(telem.slow_limit_w);
        snap.apu_slow_w = pos_u32(telem.apu_slow_limit_w);
        snap.tdc_soc_a = pos_u32(telem.tdc_soc_limit_a);
        snap.edc_soc_a = pos_u32(telem.edc_soc_limit_a);
        snap.apu_skin_temp_c = pos_u32(telem.apu_skin_temp_limit_c);
        snap.dgpu_skin_temp_c = pos_u32(telem.dgpu_skin_temp_limit_c);
    }

    Ok(snap)
}

/// Restore hardware state from a previously captured `SmuStateSnapshot` (rollback / reset).
pub fn apply_snapshot(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    snap: &SmuStateSnapshot,
    allow_extreme: bool,
    dry_run: bool,
) -> Result<Vec<PlannedOp>> {
    let mut ops = Vec::new();

    if let Some(ref cores) = snap.co_per_core {
        if !cores.is_empty() && hw.capabilities.set_co_per_core.is_some() {
            let updates: Vec<(u32, i32)> = cores.iter().map(|&(idx, m)| (idx, m as i32)).collect();
            ops.extend(co::set_co_cores(
                transport,
                hw,
                &updates,
                allow_extreme,
                dry_run,
            )?);
        }
    }

    if let Some(igpu) = snap.igpu_co {
        if hw.capabilities.set_igpu_co.is_some() {
            ops.push(co::set_igpu_co(
                transport,
                hw,
                igpu as i32,
                allow_extreme,
                dry_run,
            )?);
        }
    }

    if let Some(ref cs) = snap.curve_shaper {
        if hw.capabilities.set_curve_shaper.is_some() {
            ops.extend(shaper::set_curve_shaper_all(
                transport,
                hw,
                cs,
                allow_extreme,
                dry_run,
            )?);
        }
    }

    // Compare against live hardware so rollback/reset skips unchanged PBO limits
    // (avoids redundant SMU writes and prevents failing on BIOS Motherboard-PBO limits).
    let live_telem = pm_table::read_telemetry(transport, hw).ok();
    let live_pbo = pbo::get_pbo_status(transport, hw).ok();
    let changed_u32 = |desired: Option<u32>, current: Option<f32>| -> Option<u32> {
        let d = desired?;
        if let Some(c) = current {
            if (c - d as f32).abs() <= 0.5 {
                return None;
            }
        }
        Some(d)
    };
    let desired_scalar = snap.pbo_scalar.map(|s| s.round().clamp(1.0, 10.0) as u32);
    let scalar_req = match (desired_scalar, live_pbo.as_ref().and_then(|p| p.scalar)) {
        (Some(d), Some(c)) if (c - d as f32).abs() <= 0.05 => None,
        (d, _) => d,
    };
    let fmax_req = match (snap.fmax_mhz, live_pbo.as_ref().and_then(|p| p.fmax_mhz)) {
        (Some(d), Some(c)) if d == c => None,
        (d, _) => d,
    };

    let pbo_req = pbo::PboSetRequest {
        ppt_w: if hw.capabilities.set_ppt.is_some() {
            changed_u32(snap.ppt_w, live_telem.as_ref().and_then(|t| t.ppt_limit_w))
        } else {
            None
        },
        tdc_a: if hw.capabilities.set_tdc.is_some() {
            changed_u32(snap.tdc_a, live_telem.as_ref().and_then(|t| t.tdc_limit_a))
        } else {
            None
        },
        edc_a: if hw.capabilities.set_edc.is_some() {
            changed_u32(snap.edc_a, live_telem.as_ref().and_then(|t| t.edc_limit_a))
        } else {
            None
        },
        tctl_c: if hw.capabilities.set_tctl.is_some() {
            changed_u32(snap.tctl_c, live_telem.as_ref().and_then(|t| t.tctl_limit_c))
        } else {
            None
        },
        scalar: if hw.capabilities.set_pbo_scalar.is_some() {
            scalar_req
        } else {
            None
        },
        fmax_mhz: if hw.capabilities.set_fmax.is_some() {
            fmax_req
        } else {
            None
        },
    };
    if pbo_req.ppt_w.is_some()
        || pbo_req.tdc_a.is_some()
        || pbo_req.edc_a.is_some()
        || pbo_req.tctl_c.is_some()
        || pbo_req.scalar.is_some()
        || pbo_req.fmax_mhz.is_some()
    {
        // Restoring a captured hardware snapshot permits BIOS-configured baseline limits.
        ops.extend(pbo::set_pbo(
            transport,
            hw,
            &pbo_req,
            true,
            dry_run,
        )?);
    }

    if hw.codename.is_apu() {
        let apu_req = apu::ApuSetRequest {
            stapm_w: if hw.capabilities.set_stapm_limit.is_some() {
                changed_u32(snap.stapm_w, live_telem.as_ref().and_then(|t| t.stapm_limit_w))
            } else {
                None
            },
            stapm_time_s: None,
            fast_w: None,
            slow_w: if hw.capabilities.set_slow_limit.is_some() {
                changed_u32(snap.slow_w, live_telem.as_ref().and_then(|t| t.slow_limit_w))
            } else {
                None
            },
            slow_time_s: None,
            apu_slow_w: if hw.capabilities.set_apu_slow_limit.is_some() {
                changed_u32(
                    snap.apu_slow_w,
                    live_telem.as_ref().and_then(|t| t.apu_slow_limit_w),
                )
            } else {
                None
            },
            tdc_soc_a: if hw.capabilities.set_tdc_soc.is_some() {
                changed_u32(
                    snap.tdc_soc_a,
                    live_telem.as_ref().and_then(|t| t.tdc_soc_limit_a),
                )
            } else {
                None
            },
            edc_soc_a: if hw.capabilities.set_edc_soc.is_some() {
                changed_u32(
                    snap.edc_soc_a,
                    live_telem.as_ref().and_then(|t| t.edc_soc_limit_a),
                )
            } else {
                None
            },
            apu_skin_temp_c: if hw.capabilities.set_apu_skin_temp.is_some() {
                changed_u32(
                    snap.apu_skin_temp_c,
                    live_telem.as_ref().and_then(|t| t.apu_skin_temp_limit_c),
                )
            } else {
                None
            },
            dgpu_skin_temp_c: if hw.capabilities.set_dgpu_skin_temp.is_some() {
                changed_u32(
                    snap.dgpu_skin_temp_c,
                    live_telem.as_ref().and_then(|t| t.dgpu_skin_temp_limit_c),
                )
            } else {
                None
            },
            power_mode: None,
        };
        if !apu_req.is_empty() {
            ops.extend(apu::set_apu(transport, hw, &apu_req, true, dry_run)?);
        }
    }

    Ok(ops)
}

/// Validate and apply a declarative `Profile` cleanly (supports `--dry-run`).
pub fn apply_profile(
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    profile: &Profile,
    allow_extreme: bool,
    dry_run: bool,
) -> Result<Vec<PlannedOp>> {
    profile.validate_for(hw.codename, hw.topology.physical_cores, allow_extreme)?;

    let mut ops = Vec::new();

    if let Some(ref co_prof) = profile.curve_optimizer {
        if let Some(all_margin) = co_prof.all {
            ops.extend(co::set_co_all(
                transport,
                hw,
                all_margin,
                allow_extreme,
                dry_run,
            )?);
        }
        if !co_prof.per_core.is_empty() {
            let updates: Vec<(u32, i32)> = co_prof.per_core.iter().map(|(&k, &v)| (k, v)).collect();
            ops.extend(co::set_co_cores(
                transport,
                hw,
                &updates,
                allow_extreme,
                dry_run,
            )?);
        }
        if let Some(igpu) = co_prof.igpu {
            ops.push(co::set_igpu_co(
                transport,
                hw,
                igpu,
                allow_extreme,
                dry_run,
            )?);
        }
    }

    if let Some(ref shaper_map) = profile.curve_shaper {
        let current = if !dry_run && hw.capabilities.get_curve_shaper.is_some() {
            shaper::get_curve_shaper(transport, hw).ok()
        } else {
            None
        };

        for tier in FreqTier::ALL {
            if let Some(t_prof) = shaper_map.get(&tier) {
                let base = current
                    .map(|c| c.tier(tier))
                    .unwrap_or(ShaperTierMargins::default());
                let margins = ShaperTierMargins {
                    low: t_prof.low.unwrap_or(base.low),
                    med: t_prof.med.unwrap_or(base.med),
                    high: t_prof.high.unwrap_or(base.high),
                };
                ops.push(shaper::set_curve_shaper_tier(
                    transport,
                    hw,
                    tier,
                    margins,
                    allow_extreme,
                    dry_run,
                )?);
            }
        }
    }

    if let Some(ref pbo_prof) = profile.pbo {
        // When applying live, skip PBO fields already matching hardware so BIOS-set
        // Motherboard-PBO values don't trigger unnecessary SMU writes or rejections.
        let (live_telem, live_pbo) = if dry_run {
            (None, None)
        } else {
            (
                pm_table::read_telemetry(transport, hw).ok(),
                pbo::get_pbo_status(transport, hw).ok(),
            )
        };
        let skip_same_f32 = |desired: Option<u32>, current: Option<f32>| -> Option<u32> {
            let d = desired?;
            if let Some(c) = current {
                if (c - d as f32).abs() <= 0.5 {
                    return None;
                }
            }
            Some(d)
        };
        let scalar = match (pbo_prof.scalar, live_pbo.as_ref().and_then(|p| p.scalar)) {
            (Some(d), Some(c)) if (c - d as f32).abs() <= 0.05 => None,
            (d, _) => d,
        };
        let fmax_mhz = match (pbo_prof.fmax_mhz, live_pbo.as_ref().and_then(|p| p.fmax_mhz)) {
            (Some(d), Some(c)) if d == c => None,
            (d, _) => d,
        };

        let req = pbo::PboSetRequest {
            ppt_w: skip_same_f32(pbo_prof.ppt_w, live_telem.as_ref().and_then(|t| t.ppt_limit_w)),
            tdc_a: skip_same_f32(pbo_prof.tdc_a, live_telem.as_ref().and_then(|t| t.tdc_limit_a)),
            edc_a: skip_same_f32(pbo_prof.edc_a, live_telem.as_ref().and_then(|t| t.edc_limit_a)),
            tctl_c: skip_same_f32(pbo_prof.tctl_c, live_telem.as_ref().and_then(|t| t.tctl_limit_c)),
            scalar,
            fmax_mhz,
        };
        if req.ppt_w.is_some()
            || req.tdc_a.is_some()
            || req.edc_a.is_some()
            || req.tctl_c.is_some()
            || req.scalar.is_some()
            || req.fmax_mhz.is_some()
        {
            ops.extend(pbo::set_pbo(transport, hw, &req, allow_extreme, dry_run)?);
        }
    }

    if let Some(ref apu_prof) = profile.apu {
        let live_telem = if dry_run {
            None
        } else {
            pm_table::read_telemetry(transport, hw).ok()
        };
        let skip_same_f32 = |desired: Option<u32>, current: Option<f32>| -> Option<u32> {
            let d = desired?;
            if let Some(c) = current {
                if (c - d as f32).abs() <= 0.5 {
                    return None;
                }
            }
            Some(d)
        };
        let req = apu::ApuSetRequest {
            stapm_w: skip_same_f32(
                apu_prof.stapm_w,
                live_telem.as_ref().and_then(|t| t.stapm_limit_w),
            ),
            stapm_time_s: apu_prof.stapm_time_s,
            fast_w: skip_same_f32(
                apu_prof.fast_w,
                live_telem.as_ref().and_then(|t| t.ppt_limit_w),
            ),
            slow_w: skip_same_f32(
                apu_prof.slow_w,
                live_telem.as_ref().and_then(|t| t.slow_limit_w),
            ),
            slow_time_s: apu_prof.slow_time_s,
            apu_slow_w: skip_same_f32(
                apu_prof.apu_slow_w,
                live_telem.as_ref().and_then(|t| t.apu_slow_limit_w),
            ),
            tdc_soc_a: skip_same_f32(
                apu_prof.tdc_soc_a,
                live_telem.as_ref().and_then(|t| t.tdc_soc_limit_a),
            ),
            edc_soc_a: skip_same_f32(
                apu_prof.edc_soc_a,
                live_telem.as_ref().and_then(|t| t.edc_soc_limit_a),
            ),
            apu_skin_temp_c: skip_same_f32(
                apu_prof.apu_skin_temp_c,
                live_telem.as_ref().and_then(|t| t.apu_skin_temp_limit_c),
            ),
            dgpu_skin_temp_c: skip_same_f32(
                apu_prof.dgpu_skin_temp_c,
                live_telem.as_ref().and_then(|t| t.dgpu_skin_temp_limit_c),
            ),
            power_mode: apu_prof.power_mode,
        };
        if !req.is_empty() {
            ops.extend(apu::set_apu(transport, hw, &req, allow_extreme, dry_run)?);
        }
    }

    Ok(ops)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hw::identity::detect_hardware;
    use crate::hw::topology::CpuTopology;
    use crate::transport::mock::MockTransport;

    fn mock_hw(transport: &MockTransport) -> HardwareContext {
        let mut hw = detect_hardware(transport, false).unwrap();
        hw.topology = CpuTopology::synthetic(1, 8, 2);
        hw
    }

    #[test]
    fn profile_apply_and_snapshot_rollback_roundtrip() {
        let transport = MockTransport::granite_ridge_9700x();
        let hw = mock_hw(&transport);

        let baseline = capture_snapshot(&transport, &hw).unwrap();
        assert_eq!(baseline.ppt_w, Some(88));
        assert_eq!(baseline.co_per_core.as_ref().unwrap()[0].1, 0);

        let toml = r#"
schema = 1
[target]
codename = "granite-ridge"
physical_cores = 8

[curve_optimizer]
all = -20
per_core = { 0 = -15, 3 = -25 }

[curve_shaper.high]
low = -10
med = -15
high = -5

[pbo]
ppt_w = 105
tdc_a = 80
edc_a = 160
tctl_c = 85
scalar = 1
"#;
        let profile = Profile::from_toml_str(toml).unwrap();

        // Dry run must leave hardware untouched
        let dry_ops = apply_profile(&transport, &hw, &profile, false, true).unwrap();
        assert!(!dry_ops.is_empty());
        let still_baseline = capture_snapshot(&transport, &hw).unwrap();
        assert_eq!(still_baseline.ppt_w, Some(88));
        assert_eq!(still_baseline.co_per_core.as_ref().unwrap()[0].1, 0);

        // Live apply
        let live_ops = apply_profile(&transport, &hw, &profile, false, false).unwrap();
        assert!(!live_ops.is_empty());
        let after = capture_snapshot(&transport, &hw).unwrap();
        assert_eq!(after.ppt_w, Some(105));
        assert_eq!(after.tctl_c, Some(85));
        assert_eq!(after.co_per_core.as_ref().unwrap()[0].1, -15);
        assert_eq!(after.co_per_core.as_ref().unwrap()[1].1, -20);
        assert_eq!(after.co_per_core.as_ref().unwrap()[3].1, -25);
        assert_eq!(after.curve_shaper.unwrap().high.med, -15);

        // Rollback to baseline
        apply_snapshot(&transport, &hw, &baseline, false, false).unwrap();
        let restored = capture_snapshot(&transport, &hw).unwrap();
        assert_eq!(restored.ppt_w, Some(88));
        assert_eq!(restored.tctl_c, Some(95));
        assert_eq!(restored.co_per_core.as_ref().unwrap()[0].1, 0);
        assert_eq!(restored.curve_shaper.unwrap().high.med, 0);
    }
}
