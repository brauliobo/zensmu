//! Curve Optimizer, Curve Shaper, PBO, and APU tuning command handlers.

use super::mutation::execute_mutation;
use super::{parse_confirm_secs, probe_hw, state_dir, CliOutcome};
use crate::commands::{ApuAction, Cli, CoAction, PboAction, ShaperAction};
use crate::output::{
    print_co_table, print_json, print_pbo_status, print_shaper_matrix, ActivePboLimits,
    CoGetOutput, PboGetOutput, ShaperGetOutput,
};
use zensmu_core::error::{Error, Result};
use zensmu_core::ops::apu::{self, ApuSetRequest};
use zensmu_core::ops::co;
use zensmu_core::ops::pbo::{self, PboSetRequest};
use zensmu_core::ops::pm_table;
use zensmu_core::ops::shaper;
use zensmu_core::safety::bounds;
use zensmu_core::safety::snapshot::SnapshotStore;
use zensmu_core::transport::SmuTransport;
use zensmu_core::types::{CurveShaperState, FreqTier, PlannedOp, ShaperTierMargins, TempBand};

// ============================================================================
// Curve Optimizer (`co`)
// ============================================================================

pub(super) fn cmd_co(
    cli: &Cli,
    transport: &dyn SmuTransport,
    action: &CoAction,
) -> Result<CliOutcome> {
    let hw = probe_hw(cli, transport)?;

    match action {
        CoAction::Get => {
            let snap = co::get_co(transport, &hw)?;
            let out = CoGetOutput {
                codename: hw.codename,
                cores: snap.cores,
                igpu: snap.igpu,
            };
            if cli.json {
                print_json(&out);
            } else {
                print_co_table(&out);
            }
            Ok(CliOutcome::Success)
        }
        CoAction::Set { all, cores, igpu } => {
            if all.is_none() && cores.is_empty() && igpu.is_none() {
                return Err(Error::InvalidInput(
                    "specify at least one of --all, --core, or --igpu".into(),
                ));
            }
            let confirm_secs = parse_confirm_secs(cli)?;

            let mut core_updates: Vec<(u32, i32)> = Vec::new();
            for spec in cores {
                core_updates.extend(bounds::parse_core_assignments(
                    spec,
                    hw.topology.physical_cores,
                )?);
            }

            let plan_fn = |dry_run: bool| -> Result<Vec<PlannedOp>> {
                let mut ops = Vec::new();
                if let Some(m) = *all {
                    ops.extend(co::set_co_all(
                        transport,
                        &hw,
                        m,
                        cli.allow_extreme,
                        dry_run,
                    )?);
                }
                if !core_updates.is_empty() {
                    ops.extend(co::set_co_cores(
                        transport,
                        &hw,
                        &core_updates,
                        cli.allow_extreme,
                        dry_run,
                    )?);
                }
                if let Some(im) = *igpu {
                    ops.push(co::set_igpu_co(
                        transport,
                        &hw,
                        im,
                        cli.allow_extreme,
                        dry_run,
                    )?);
                }
                Ok(ops)
            };

            execute_mutation(cli, transport, &hw, "co set", confirm_secs, plan_fn)
        }
        CoAction::Reset => {
            let confirm_secs = parse_confirm_secs(cli)?;
            let plan_fn = |dry_run: bool| -> Result<Vec<PlannedOp>> {
                let mut ops = Vec::new();
                if hw.capabilities.set_co_all_cores.is_some() {
                    ops.extend(co::set_co_all(transport, &hw, 0, false, dry_run)?);
                } else {
                    let zeros: Vec<(u32, i32)> =
                        hw.topology.cores.iter().map(|c| (c.index, 0)).collect();
                    ops.extend(co::set_co_cores(transport, &hw, &zeros, false, dry_run)?);
                }
                if hw.capabilities.set_igpu_co.is_some() {
                    match co::set_igpu_co(transport, &hw, 0, false, dry_run) {
                        Ok(op) => ops.push(op),
                        Err(Error::SmuRejected { .. }) if !dry_run => {
                            if !cli.quiet && !cli.json {
                                eprintln!(
                                    "note: iGPU CO reset skipped (SMU rejected; iGPU may be disabled in BIOS)"
                                );
                            }
                        }
                        Err(err) => return Err(err),
                    }
                }
                Ok(ops)
            };

            execute_mutation(cli, transport, &hw, "co reset", confirm_secs, plan_fn)
        }
    }
}

// ============================================================================
// Curve Shaper (`shaper`)
// ============================================================================

pub(super) fn cmd_shaper(
    cli: &Cli,
    transport: &dyn SmuTransport,
    action: &ShaperAction,
) -> Result<CliOutcome> {
    let hw = probe_hw(cli, transport)?;

    match action {
        ShaperAction::Get => {
            let state = shaper::get_curve_shaper(transport, &hw)?;
            let out = ShaperGetOutput {
                codename: hw.codename,
                shaper: state,
            };
            if cli.json {
                print_json(&out);
            } else {
                print_shaper_matrix(&out.shaper, out.codename);
            }
            Ok(CliOutcome::Success)
        }
        ShaperAction::Set { freq, temp, all } => {
            let confirm_secs = parse_confirm_secs(cli)?;

            match (freq, temp, all) {
                (None, None, Some(all_margin)) => {
                    let m = shaper::validate_shaper_margin(*all_margin, cli.allow_extreme)?;
                    let uniform = ShaperTierMargins {
                        low: m,
                        med: m,
                        high: m,
                    };
                    let state = CurveShaperState {
                        min: uniform,
                        low: uniform,
                        med: uniform,
                        high: uniform,
                        max: uniform,
                    };
                    execute_mutation(cli, transport, &hw, "shaper set", confirm_secs, |dry_run| {
                        shaper::set_curve_shaper_all(
                            transport,
                            &hw,
                            &state,
                            cli.allow_extreme,
                            dry_run,
                        )
                    })
                }
                (Some(freq_str), Some(temp_str), None) => {
                    let tier = FreqTier::parse(freq_str).ok_or_else(|| {
                        Error::InvalidInput(format!(
                            "invalid frequency tier '{freq_str}' (expected min, low, med, high, or max)"
                        ))
                    })?;
                    let band_updates = bounds::parse_shaper_temps(temp_str)?;
                    let mut validated_updates = Vec::with_capacity(band_updates.len());
                    for (band, margin) in band_updates {
                        let m = shaper::validate_shaper_margin(margin, cli.allow_extreme)?;
                        validated_updates.push((band, m));
                    }

                    // Read existing tier first so partial temperature updates preserve unspecified bands.
                    // In --dry-run mode, fall back to zeroed margins if unprivileged readback fails.
                    let existing_tier = match shaper::get_curve_shaper(transport, &hw) {
                        Ok(st) => st.tier(tier),
                        Err(_) if cli.dry_run => ShaperTierMargins::default(),
                        Err(err) => return Err(err),
                    };

                    let mut target = existing_tier;
                    for (band, m) in validated_updates {
                        match band {
                            TempBand::Low => target.low = m,
                            TempBand::Med => target.med = m,
                            TempBand::High => target.high = m,
                        }
                    }

                    execute_mutation(cli, transport, &hw, "shaper set", confirm_secs, |dry_run| {
                        Ok(vec![shaper::set_curve_shaper_tier(
                            transport,
                            &hw,
                            tier,
                            target,
                            cli.allow_extreme,
                            dry_run,
                        )?])
                    })
                }
                (Some(_), None, None) | (None, Some(_), None) => Err(Error::InvalidInput(
                    "both --freq <TIER> and --temp <SPEC> must be provided together".into(),
                )),
                (Some(_), _, Some(_)) | (_, Some(_), Some(_)) => Err(Error::InvalidInput(
                    "cannot combine --all with --freq/--temp".into(),
                )),
                (None, None, None) => Err(Error::InvalidInput(
                    "specify either --all <N> or --freq <TIER> --temp <SPEC>".into(),
                )),
            }
        }
        ShaperAction::Reset => {
            let confirm_secs = parse_confirm_secs(cli)?;
            let zeros = CurveShaperState::default();
            execute_mutation(
                cli,
                transport,
                &hw,
                "shaper reset",
                confirm_secs,
                |dry_run| shaper::set_curve_shaper_all(transport, &hw, &zeros, false, dry_run),
            )
        }
    }
}

// ============================================================================
// Precision Boost Overdrive (`pbo`)
// ============================================================================

pub(super) fn cmd_pbo(
    cli: &Cli,
    transport: &dyn SmuTransport,
    action: &PboAction,
) -> Result<CliOutcome> {
    let hw = probe_hw(cli, transport)?;

    match action {
        PboAction::Get => {
            let status = pbo::get_pbo_status(transport, &hw)?;
            let active_limits = pm_table::read_telemetry(transport, &hw)
                .map(|t| ActivePboLimits::from_telemetry(&t))
                .unwrap_or_default();

            let out = PboGetOutput {
                codename: hw.codename,
                active_limits,
                status,
            };
            if cli.json {
                print_json(&out);
            } else {
                print_pbo_status(&out);
            }
            Ok(CliOutcome::Success)
        }
        PboAction::Set {
            ppt,
            tdc,
            edc,
            tctl,
            scalar,
            fmax,
        } => {
            let confirm_secs = parse_confirm_secs(cli)?;
            let req = PboSetRequest {
                ppt_w: ppt.as_deref().map(bounds::parse_watts).transpose()?,
                tdc_a: tdc.as_deref().map(bounds::parse_amps).transpose()?,
                edc_a: edc.as_deref().map(bounds::parse_amps).transpose()?,
                tctl_c: tctl.as_deref().map(bounds::parse_celsius).transpose()?,
                scalar: *scalar,
                fmax_mhz: fmax.as_deref().map(bounds::parse_mhz).transpose()?,
            };
            if req == PboSetRequest::default() {
                return Err(Error::InvalidInput(
                    "specify at least one of --ppt, --tdc, --edc, --tctl, --scalar, or --fmax"
                        .into(),
                ));
            }

            execute_mutation(cli, transport, &hw, "pbo set", confirm_secs, |dry_run| {
                pbo::set_pbo(transport, &hw, &req, cli.allow_extreme, dry_run)
            })
        }
        PboAction::Reset => {
            let confirm_secs = parse_confirm_secs(cli)?;
            let store = SnapshotStore::new(state_dir(cli));
            let baseline = store.load_boot_baseline().ok().flatten();
            let pbo_st = pbo::get_pbo_status(transport, &hw).ok();
            let fused = pbo_st.as_ref().map(|s| &s.fused);

            let ppt_w = fused
                .and_then(|f| f.ppt_w)
                .or_else(|| baseline.as_ref().and_then(|b| b.ppt_w));
            let tdc_a = fused
                .and_then(|f| f.vdd_tdc_a)
                .or_else(|| baseline.as_ref().and_then(|b| b.tdc_a));
            let edc_a = baseline.as_ref().and_then(|b| b.edc_a);
            let tctl_c = baseline.as_ref().and_then(|b| b.tctl_c);
            let scalar = baseline
                .as_ref()
                .and_then(|b| b.pbo_scalar)
                .map(|s| s.round().clamp(1.0, 10.0) as u32)
                .or_else(|| hw.capabilities.set_pbo_scalar.map(|_| 1));
            let fmax_mhz = baseline.as_ref().and_then(|b| b.fmax_mhz);

            let req = PboSetRequest {
                ppt_w,
                tdc_a,
                edc_a,
                tctl_c,
                scalar,
                fmax_mhz,
            };

            if req == PboSetRequest::default() {
                return Err(Error::InvalidInput(
                    "no fused factory limits or boot-baseline snapshot available to restore PBO"
                        .into(),
                ));
            }

            execute_mutation(cli, transport, &hw, "pbo reset", confirm_secs, |dry_run| {
                pbo::set_pbo(transport, &hw, &req, true, dry_run)
            })
        }
    }
}

// ============================================================================
// Mobile APU DPTC (`apu`)
// ============================================================================

pub(super) fn parse_secs_u32(s: &str) -> Result<u32> {
    let secs = bounds::parse_duration_secs(s)?;
    u32::try_from(secs)
        .map_err(|_| Error::InvalidInput(format!("duration '{s}' exceeds u32 seconds range")))
}

pub(super) fn cmd_apu(
    cli: &Cli,
    transport: &dyn SmuTransport,
    action: &ApuAction,
) -> Result<CliOutcome> {
    let hw = probe_hw(cli, transport)?;

    match action {
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
        } => {
            let confirm_secs = parse_confirm_secs(cli)?;
            let req = ApuSetRequest {
                stapm_w: stapm.as_deref().map(bounds::parse_watts).transpose()?,
                stapm_time_s: stapm_time.as_deref().map(parse_secs_u32).transpose()?,
                fast_w: fast.as_deref().map(bounds::parse_watts).transpose()?,
                slow_w: slow.as_deref().map(bounds::parse_watts).transpose()?,
                slow_time_s: slow_time.as_deref().map(parse_secs_u32).transpose()?,
                apu_slow_w: apu_slow.as_deref().map(bounds::parse_watts).transpose()?,
                tdc_soc_a: tdc_soc.as_deref().map(bounds::parse_amps).transpose()?,
                edc_soc_a: edc_soc.as_deref().map(bounds::parse_amps).transpose()?,
                apu_skin_temp_c: apu_skin.as_deref().map(bounds::parse_celsius).transpose()?,
                dgpu_skin_temp_c: dgpu_skin
                    .as_deref()
                    .map(bounds::parse_celsius)
                    .transpose()?,
                power_mode: mode.map(Into::into),
            };

            if req == ApuSetRequest::default() {
                return Err(Error::InvalidInput(
                    "specify at least one APU parameter to set".into(),
                ));
            }

            execute_mutation(cli, transport, &hw, "apu set", confirm_secs, |dry_run| {
                apu::set_apu(transport, &hw, &req, cli.allow_extreme, dry_run)
            })
        }
    }
}
