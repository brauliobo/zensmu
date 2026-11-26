//! `zensmu` — Safety-first CLI and live TUI for AMD Ryzen SMU control and telemetry.

mod commands;
mod doctor;
mod output;
mod tui;

use clap::{CommandFactory, Parser};
use clap_complete::Shell;
use commands::{
    ApuAction, Cli, CoAction, Command, PboAction, PersistAction, PersistCommand, ProfileAction,
    ShaperAction, TelemetryAction,
};
use doctor::{is_effective_root, run_doctor};
use output::{
    print_co_table, print_doctor, print_error, print_info, print_json, print_json_compact,
    print_mutation, print_pbo_status, print_persist_action, print_persist_status,
    print_planned_ops, print_profile_diff, print_profile_list, print_shaper_matrix, print_status,
    print_telemetry, ActivePboLimits, ClearGuardOutput, CoGetOutput, InfoOutput, MutationOutput,
    PboGetOutput, PersistActionOutput, ProfileDiffOutput, ProfileListItem, ProfileListOutput,
    ProfileSaveOutput, ProfileValidateOutput, ShaperGetOutput, StatusOutput, TelemetryDumpOutput,
};
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};
use zensmu_core::config::profile::Profile;
use zensmu_core::error::{Error, Result};
use zensmu_core::hw::identity::{detect_hardware, HardwareContext};
use zensmu_core::ops::apu::{self, ApuSetRequest};
use zensmu_core::ops::co;
use zensmu_core::ops::pbo::{self, PboSetRequest};
use zensmu_core::ops::pm_table;
use zensmu_core::ops::shaper;
use zensmu_core::safety::bounds;
use zensmu_core::safety::guard::{BootLoopGuard, DEFAULT_GUARD_PATH};
use zensmu_core::safety::persist::{self, PersistPaths, PersistStatus};
use zensmu_core::safety::snapshot::{SmuStateSnapshot, SnapshotStore, DEFAULT_STATE_DIR};
use zensmu_core::session;
use zensmu_core::transport::sysfs::{SysfsTransport, DEFAULT_SYSFS_PATH};
use zensmu_core::transport::SmuTransport;
use zensmu_core::types::{
    Codename, CurveShaperState, FreqTier, PlannedOp, ShaperTierMargins, TempBand,
};

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn handle_sigint(_sig: libc::c_int) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

fn install_sigint_handler() {
    INTERRUPTED.store(false, Ordering::SeqCst);
    unsafe {
        libc::signal(libc::SIGINT, handle_sigint as *const () as usize);
    }
}

/// Outcome of CLI execution. `ExitWithCode` indicates structured output was
/// already printed to stdout and the process should exit with the given code
/// without printing an extra error message.
enum CliOutcome {
    Success,
    ExitWithCode(u8),
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;

    match run(cli) {
        Ok(CliOutcome::Success) => ExitCode::SUCCESS,
        Ok(CliOutcome::ExitWithCode(code)) => ExitCode::from(code),
        Err(err) => {
            print_error(&err, json);
            ExitCode::FAILURE
        }
    }
}

fn sysfs_path(cli: &Cli) -> PathBuf {
    cli.sysfs_path
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SYSFS_PATH))
}

fn state_dir(cli: &Cli) -> PathBuf {
    cli.state_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR))
}

fn guard_path(cli: &Cli) -> PathBuf {
    cli.guard_path
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_GUARD_PATH))
}

fn persist_paths(cli: &Cli) -> PersistPaths {
    PersistPaths {
        guard_path: guard_path(cli),
        ..PersistPaths::default()
    }
}

fn build_transport(cli: &Cli) -> SysfsTransport {
    SysfsTransport::new(sysfs_path(cli))
}

fn probe_hw(cli: &Cli, transport: &dyn SmuTransport) -> Result<HardwareContext> {
    detect_hardware(transport, !cli.no_cpuid_check)
}

fn parse_confirm_secs(cli: &Cli) -> Result<Option<u64>> {
    match &cli.confirm_within {
        Some(raw) => Ok(Some(bounds::parse_duration_secs(raw)?)),
        None => Ok(None),
    }
}

fn run(cli: Cli) -> Result<CliOutcome> {
    let transport = build_transport(&cli);

    match &cli.command {
        Command::Completions { shell } => cmd_completions(*shell),
        Command::Doctor => cmd_doctor(&cli, &transport),
        Command::Info => cmd_info(&cli, &transport),
        Command::Status => cmd_status(&cli, &transport),
        Command::Co { action } => cmd_co(&cli, &transport, action),
        Command::Shaper { action } => cmd_shaper(&cli, &transport, action),
        Command::Pbo { action } => cmd_pbo(&cli, &transport, action),
        Command::Apu { action } => cmd_apu(&cli, &transport, action),
        Command::Telemetry { action } => cmd_telemetry(&cli, &transport, action.as_ref()),
        Command::Top { interval } => cmd_top(&cli, &transport, interval),
        Command::Profile { action } => cmd_profile(&cli, &transport, action),
        Command::Persist(persist_cmd) => cmd_persist(&cli, &transport, persist_cmd),
        Command::Rollback => cmd_rollback(&cli, &transport),
        Command::Reset => cmd_reset(&cli, &transport),
    }
}

fn cmd_completions(shell: commands::CompletionShell) -> Result<CliOutcome> {
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

// ============================================================================
// Inspection & Diagnostics Subcommands
// ============================================================================

fn cmd_info(cli: &Cli, transport: &dyn SmuTransport) -> Result<CliOutcome> {
    let hw = probe_hw(cli, transport)?;
    let out = InfoOutput::from_hw(&hw);
    if cli.json {
        print_json(&out);
    } else {
        print_info(&out);
    }
    Ok(CliOutcome::Success)
}

fn cmd_doctor(cli: &Cli, transport: &dyn SmuTransport) -> Result<CliOutcome> {
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

fn mailbox_read_outcome<T>(res: Result<T>) -> (Option<T>, Option<String>) {
    match res {
        Ok(val) => (Some(val), None),
        Err(Error::PermissionDenied { .. }) => (None, Some("requires root".into())),
        Err(err) => (None, Some(err.to_string())),
    }
}

fn cmd_status(cli: &Cli, transport: &dyn SmuTransport) -> Result<CliOutcome> {
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

// ============================================================================
// Curve Optimizer (`co`)
// ============================================================================

fn cmd_co(cli: &Cli, transport: &dyn SmuTransport, action: &CoAction) -> Result<CliOutcome> {
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

fn cmd_shaper(
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

fn cmd_pbo(cli: &Cli, transport: &dyn SmuTransport, action: &PboAction) -> Result<CliOutcome> {
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

fn parse_secs_u32(s: &str) -> Result<u32> {
    let secs = bounds::parse_duration_secs(s)?;
    u32::try_from(secs)
        .map_err(|_| Error::InvalidInput(format!("duration '{s}' exceeds u32 seconds range")))
}

fn cmd_apu(cli: &Cli, transport: &dyn SmuTransport, action: &ApuAction) -> Result<CliOutcome> {
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

// ============================================================================
// Telemetry & Live TUI (`telemetry`, `top`)
// ============================================================================

fn cmd_telemetry(
    cli: &Cli,
    transport: &dyn SmuTransport,
    action: Option<&TelemetryAction>,
) -> Result<CliOutcome> {
    match action {
        None => {
            let hw = probe_hw(cli, transport)?;
            let snap = pm_table::read_telemetry(transport, &hw)?;
            if cli.json {
                print_json(&snap);
            } else {
                print_telemetry(&snap);
            }
            Ok(CliOutcome::Success)
        }
        Some(TelemetryAction::Watch { interval }) => {
            let interval_ms = bounds::parse_duration_ms(interval)?;
            let hw = probe_hw(cli, transport)?;
            let is_tty = io::stdout().is_terminal();

            install_sigint_handler();
            while !INTERRUPTED.load(Ordering::SeqCst) {
                let snap = pm_table::read_telemetry(transport, &hw)?;
                if cli.json {
                    print_json_compact(&snap);
                    let _ = io::stdout().flush();
                } else {
                    if is_tty {
                        print!("\x1b[2J\x1b[H");
                    } else {
                        println!("---");
                    }
                    print_telemetry(&snap);
                    if is_tty {
                        println!();
                        println!("Refreshing every {interval_ms} ms (Ctrl-C to exit)...");
                    }
                    let _ = io::stdout().flush();
                }

                let target = Instant::now() + Duration::from_millis(interval_ms);
                while Instant::now() < target && !INTERRUPTED.load(Ordering::SeqCst) {
                    let rem = target.saturating_duration_since(Instant::now());
                    thread::sleep(rem.min(Duration::from_millis(50)));
                }
            }
            Ok(CliOutcome::Success)
        }
        Some(TelemetryAction::Dump { out_path }) => {
            let info = transport.read_info()?;
            let bytes = pm_table::dump_raw_pm_table(transport, out_path)?;
            let out = TelemetryDumpOutput {
                path: out_path.display().to_string(),
                bytes,
                table_version: info.pm_table_version,
            };
            if cli.json {
                print_json(&out);
            } else {
                println!(
                    "Dumped {bytes} raw PM table bytes to {} (version: {})",
                    out.path,
                    out.table_version
                        .map(|v| format!("0x{v:08X}"))
                        .unwrap_or_else(|| "unknown".into())
                );
            }
            Ok(CliOutcome::Success)
        }
    }
}

fn cmd_top(cli: &Cli, transport: &dyn SmuTransport, interval: &str) -> Result<CliOutcome> {
    let interval_ms = bounds::parse_duration_ms(interval)?;
    let hw = probe_hw(cli, transport)?;
    tui::run_tui(transport, &hw, interval_ms)?;
    Ok(CliOutcome::Success)
}

// ============================================================================
// Profiles (`profile`)
// ============================================================================

fn cmd_profile(
    cli: &Cli,
    transport: &dyn SmuTransport,
    action: &ProfileAction,
) -> Result<CliOutcome> {
    match action {
        ProfileAction::Save { path } => {
            let hw = probe_hw(cli, transport)?;
            let snap = session::capture_snapshot(transport, &hw)?;
            let profile = Profile::from_snapshot(&snap);
            profile.save(path)?;
            let out = ProfileSaveOutput {
                path: path.display().to_string(),
                codename: hw.codename,
                physical_cores: hw.topology.physical_cores,
            };
            if cli.json {
                print_json(&out);
            } else {
                println!(
                    "Saved profile for {} ({} physical cores) to {}",
                    hw.codename.display_name(),
                    out.physical_cores,
                    out.path
                );
            }
            Ok(CliOutcome::Success)
        }
        ProfileAction::Validate { path } => {
            let profile = Profile::load(path)?;
            let (host_codename, note) = match probe_hw(cli, transport) {
                Ok(hw) => {
                    profile.validate_for(
                        hw.codename,
                        hw.topology.physical_cores,
                        cli.allow_extreme,
                    )?;
                    (Some(hw.codename), None)
                }
                Err(Error::DriverNotLoaded { .. }) => {
                    let cores = profile.target.physical_cores.unwrap_or(16);
                    profile.validate_for(profile.target.codename, cores, cli.allow_extreme)?;
                    (
                        None,
                        Some(
                            "ryzen_smu not loaded: validated schema & bounds against declared profile target"
                                .into(),
                        ),
                    )
                }
                Err(err) => return Err(err),
            };

            let out = ProfileValidateOutput {
                path: path.display().to_string(),
                valid: true,
                target_codename: profile.target.codename,
                target_physical_cores: profile.target.physical_cores,
                host_codename,
                note,
            };
            if cli.json {
                print_json(&out);
            } else {
                println!(
                    "Profile {} is valid (target: {}, cores: {}).",
                    out.path,
                    out.target_codename.display_name(),
                    out.target_physical_cores
                        .map(|n| n.to_string())
                        .unwrap_or_else(|| "any".into())
                );
                if let Some(ref n) = out.note {
                    println!("Note: {n}");
                }
            }
            Ok(CliOutcome::Success)
        }
        ProfileAction::Diff { path } => {
            let hw = probe_hw(cli, transport)?;
            let profile = Profile::load(path)?;
            profile.validate_for(hw.codename, hw.topology.physical_cores, true)?;
            let snap = session::capture_snapshot(transport, &hw)?;
            let diffs = profile.diff_against(&snap);
            let out = ProfileDiffOutput {
                path: path.display().to_string(),
                identical: diffs.is_empty(),
                diffs,
            };
            if cli.json {
                print_json(&out);
            } else {
                print_profile_diff(&out);
            }
            Ok(CliOutcome::Success)
        }
        ProfileAction::Apply {
            path,
            boot_guard,
            settle_secs,
        } => {
            let confirm_secs = parse_confirm_secs(cli)?;
            let hw = probe_hw(cli, transport)?;
            let profile = Profile::load(path)?;
            profile.validate_for(hw.codename, hw.topology.physical_cores, cli.allow_extreme)?;

            // Build dry-run plan first (validates everything cleanly before touching guard or SMU)
            let planned =
                session::apply_profile(transport, &hw, &profile, cli.allow_extreme, true)?;

            if cli.dry_run {
                let out = MutationOutput {
                    action: format!("profile apply {}", path.display()),
                    dry_run: true,
                    applied: false,
                    rolled_back: false,
                    confirmed: None,
                    operations: planned,
                    note: None,
                };
                emit_mutation(cli, &out, false);
                return Ok(CliOutcome::Success);
            }

            if !confirm_before_write(cli, &planned, &format!("profile apply {}", path.display()))? {
                return Ok(CliOutcome::ExitWithCode(1));
            }

            // If --boot-guard is enabled, arm persistent sentinel BEFORE any SMU writes.
            // If already tripped from a previous crash, exits non-zero cleanly without touching SMU.
            let armed_guard = if *boot_guard {
                let guard = BootLoopGuard::new(guard_path(cli));
                Some(guard.check_and_arm(&path.display().to_string())?)
            } else {
                None
            };

            let store = SnapshotStore::new(state_dir(cli));
            let pre_snap = capture_and_store_snapshots(cli, transport, &hw, &store)?;

            let ops = session::apply_profile(transport, &hw, &profile, cli.allow_extreme, false)?;

            if let Some(secs) = confirm_secs {
                let outcome = wait_for_confirmation_or_rollback(
                    cli,
                    transport,
                    &hw,
                    pre_snap.as_ref(),
                    secs,
                    &format!("profile apply {}", path.display()),
                    ops,
                )?;
                if let Some(armed) = armed_guard {
                    // Cleanly disarm after interactive confirmation or clean rollback
                    armed.disarm()?;
                }
                return Ok(outcome);
            }

            // Boot-guard settle window: if the machine crashes/freezes during settle,
            // ArmedBootGuard is never disarmed and the sentinel stays on disk.
            if let Some(armed) = armed_guard {
                if *settle_secs > 0 {
                    if !cli.quiet && !cli.json {
                        eprintln!(
                            "Boot-loop guard armed: waiting {}s settle period before disarming...",
                            settle_secs
                        );
                    }
                    thread::sleep(Duration::from_secs(*settle_secs));
                }
                armed.disarm()?;
            }

            let note = if *boot_guard {
                Some(format!(
                    "boot-loop guard settled ({settle_secs}s) and disarmed cleanly"
                ))
            } else {
                None
            };

            let out = MutationOutput {
                action: format!("profile apply {}", path.display()),
                dry_run: false,
                applied: true,
                rolled_back: false,
                confirmed: None,
                operations: ops,
                note,
            };
            emit_mutation(cli, &out, true);
            Ok(CliOutcome::Success)
        }
        ProfileAction::ClearGuard => cmd_profile_clear_guard(cli),
        ProfileAction::List { dir } => cmd_profile_list(cli, dir),
    }
}

fn cmd_profile_clear_guard(cli: &Cli) -> Result<CliOutcome> {
    let gp = guard_path(cli);
    let guard = BootLoopGuard::new(gp.clone());
    let was_tripped = guard.is_tripped();
    guard.clear()?;

    let out = ClearGuardOutput {
        guard_path: gp.display().to_string(),
        was_tripped,
        cleared: true,
    };
    if cli.json {
        print_json(&out);
    } else if was_tripped {
        println!(
            "Cleared tripped boot-loop guard sentinel at {}",
            out.guard_path
        );
    } else {
        println!(
            "Boot-loop guard sentinel at {} was already clear.",
            out.guard_path
        );
    }
    Ok(CliOutcome::Success)
}

fn cmd_profile_list(cli: &Cli, dir: &Path) -> Result<CliOutcome> {
    let mut items = Vec::new();
    if dir.exists() {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .map_err(|source| Error::Io {
                path: dir.display().to_string(),
                source,
            })?
            .filter_map(|e| e.ok().map(|ent| ent.path()))
            .filter(|p| p.is_file() && p.extension().and_then(|s| s.to_str()) == Some("toml"))
            .collect();
        entries.sort();

        for path in entries {
            let name = path
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or_default()
                .to_string();
            match Profile::load(&path) {
                Ok(p) => items.push(ProfileListItem {
                    name,
                    path: path.display().to_string(),
                    codename: Some(p.target.codename),
                    valid: true,
                    error: None,
                }),
                Err(err) => items.push(ProfileListItem {
                    name,
                    path: path.display().to_string(),
                    codename: None,
                    valid: false,
                    error: Some(err.to_string()),
                }),
            }
        }
    }

    let out = ProfileListOutput {
        dir: dir.display().to_string(),
        profiles: items,
    };
    if cli.json {
        print_json(&out);
    } else {
        print_profile_list(&out);
    }
    Ok(CliOutcome::Success)
}

// ============================================================================
// Boot & Suspend Persistence (`persist`)
// ============================================================================

fn emit_persist_action(
    cli: &Cli,
    paths: &PersistPaths,
    action: &str,
    dry_run: bool,
    status: PersistStatus,
    note: String,
) {
    let out = PersistActionOutput {
        action: action.into(),
        dry_run,
        active_profile_path: paths.active_profile.display().to_string(),
        service_enabled: status.service_enabled,
        status,
        note: Some(note),
    };
    if cli.json {
        print_json(&out);
    } else if dry_run || !cli.quiet {
        print_persist_action(&out);
    }
}

fn cmd_persist(
    cli: &Cli,
    transport: &dyn SmuTransport,
    cmd: &PersistCommand,
) -> Result<CliOutcome> {
    let paths = persist_paths(cli);

    match &cmd.action {
        PersistAction::Status => {
            let (codename, cores) = match probe_hw(cli, transport) {
                Ok(hw) => (Some(hw.codename), Some(hw.topology.physical_cores)),
                Err(_) => (None, None),
            };
            let status = persist::read_persist_status(&paths, codename, cores);
            if cli.json {
                print_json(&status);
            } else {
                print_persist_status(&status);
            }
            Ok(CliOutcome::Success)
        }
        PersistAction::Save { from, enable } => {
            let hw = probe_hw(cli, transport)?;
            let profile = match from {
                Some(path) => Profile::load(path)?,
                None => {
                    let snap = session::capture_snapshot(transport, &hw)?;
                    Profile::from_snapshot(&snap)
                }
            };
            // Live hardware snapshots may reflect BIOS-configured Motherboard-PBO values
            // (e.g. PPT=1000W, Scalar=5x) above interactive soft thresholds, so validate
            // live snapshots against hard hardware bounds while keeping soft checks for --from files.
            let effective_allow_extreme = cli.allow_extreme || from.is_none();
            profile.validate_for(
                hw.codename,
                hw.topology.physical_cores,
                effective_allow_extreme,
            )?;

            if cli.dry_run {
                let status = persist::read_persist_status(
                    &paths,
                    Some(hw.codename),
                    Some(hw.topology.physical_cores),
                );
                let note = if *enable {
                    format!(
                        "would write active profile to {}, ensure {}, and enable {}",
                        paths.active_profile.display(),
                        paths.modules_load.display(),
                        paths.service_name
                    )
                } else {
                    format!(
                        "would write active profile to {} and ensure {}",
                        paths.active_profile.display(),
                        paths.modules_load.display()
                    )
                };
                emit_persist_action(cli, &paths, "persist save", true, status, note);
                return Ok(CliOutcome::Success);
            }

            persist::persist_profile(
                &paths,
                &profile,
                hw.codename,
                hw.topology.physical_cores,
                effective_allow_extreme,
            )?;
            if *enable {
                persist::set_service_enabled(
                    &paths,
                    true,
                    hw.codename,
                    hw.topology.physical_cores,
                    effective_allow_extreme,
                )?;
            }

            let status = persist::read_persist_status(
                &paths,
                Some(hw.codename),
                Some(hw.topology.physical_cores),
            );
            let note = if *enable {
                format!(
                    "wrote active profile, ensured {}, and enabled {}",
                    paths.modules_load.display(),
                    paths.service_name
                )
            } else {
                format!(
                    "wrote active profile and ensured {}",
                    paths.modules_load.display()
                )
            };
            emit_persist_action(cli, &paths, "persist save", false, status, note);
            Ok(CliOutcome::Success)
        }
        PersistAction::Enable => {
            let hw = probe_hw(cli, transport)?;

            if cli.dry_run {
                if !paths.active_profile.exists() {
                    return Err(Error::InvalidInput(format!(
                        "cannot enable {} because {} does not exist yet (save a profile first)",
                        paths.service_name,
                        paths.active_profile.display()
                    )));
                }
                let prof = Profile::load(&paths.active_profile)?;
                prof.validate_for(hw.codename, hw.topology.physical_cores, true)?;

                let status = persist::read_persist_status(
                    &paths,
                    Some(hw.codename),
                    Some(hw.topology.physical_cores),
                );
                emit_persist_action(
                    cli,
                    &paths,
                    "persist enable",
                    true,
                    status,
                    format!(
                        "would ensure {} and enable {}",
                        paths.modules_load.display(),
                        paths.service_name
                    ),
                );
                return Ok(CliOutcome::Success);
            }

            persist::set_service_enabled(
                &paths,
                true,
                hw.codename,
                hw.topology.physical_cores,
                true,
            )?;
            let status = persist::read_persist_status(
                &paths,
                Some(hw.codename),
                Some(hw.topology.physical_cores),
            );
            emit_persist_action(
                cli,
                &paths,
                "persist enable",
                false,
                status,
                format!("enabled {}", paths.service_name),
            );
            Ok(CliOutcome::Success)
        }
        PersistAction::Disable { remove_profile } => {
            let hw_opt = probe_hw(cli, transport).ok();
            let (codename_opt, cores_opt) = hw_opt
                .as_ref()
                .map(|h| (Some(h.codename), Some(h.topology.physical_cores)))
                .unwrap_or((None, None));

            if cli.dry_run {
                let status = persist::read_persist_status(&paths, codename_opt, cores_opt);
                let note = if *remove_profile {
                    format!(
                        "would disable {} and remove {}",
                        paths.service_name,
                        paths.active_profile.display()
                    )
                } else {
                    format!("would disable {}", paths.service_name)
                };
                emit_persist_action(cli, &paths, "persist disable", true, status, note);
                return Ok(CliOutcome::Success);
            }

            persist::set_service_enabled(
                &paths,
                false,
                codename_opt.unwrap_or(Codename::GraniteRidge),
                cores_opt.unwrap_or(0),
                cli.allow_extreme,
            )?;
            let removed = if *remove_profile {
                persist::remove_active_profile(&paths)?
            } else {
                false
            };

            let status = persist::read_persist_status(&paths, codename_opt, cores_opt);
            let note = if *remove_profile {
                if removed {
                    format!(
                        "disabled {} and removed {}",
                        paths.service_name,
                        paths.active_profile.display()
                    )
                } else {
                    format!(
                        "disabled {} ({} was already absent)",
                        paths.service_name,
                        paths.active_profile.display()
                    )
                }
            } else {
                format!("disabled {}", paths.service_name)
            };
            emit_persist_action(cli, &paths, "persist disable", false, status, note);
            Ok(CliOutcome::Success)
        }
    }
}

// ============================================================================
// Rollback & Reset (`rollback`, `reset`)
// ============================================================================

enum SnapshotRestoreKind {
    Rollback,
    Reset,
}

fn execute_snapshot_restore(
    cli: &Cli,
    transport: &dyn SmuTransport,
    kind: SnapshotRestoreKind,
) -> Result<CliOutcome> {
    let hw = probe_hw(cli, transport)?;
    let store = SnapshotStore::new(state_dir(cli));
    let (action, snap, source_path, save_current_as_last_state) = match kind {
        SnapshotRestoreKind::Rollback => {
            let path = store.last_state_path();
            let snap = store.load_last_state()?.ok_or_else(|| {
                Error::InvalidInput(format!(
                    "no last-state snapshot found at {} (no prior mutation recorded in this boot)",
                    path.display()
                ))
            })?;
            ("rollback", snap, path, false)
        }
        SnapshotRestoreKind::Reset => {
            let path = store.boot_baseline_path();
            let snap = store.load_boot_baseline()?.ok_or_else(|| {
                Error::InvalidInput(format!(
                    "no boot-baseline snapshot found at {} (no prior mutation recorded in this boot)",
                    path.display()
                ))
            })?;
            ("reset", snap, path, true)
        }
    };

    let planned = session::apply_snapshot(transport, &hw, &snap, true, true)?;
    if cli.dry_run {
        let out = MutationOutput {
            action: action.into(),
            dry_run: true,
            applied: false,
            rolled_back: false,
            confirmed: None,
            operations: planned,
            note: Some(format!("from {}", source_path.display())),
        };
        emit_mutation(cli, &out, false);
        return Ok(CliOutcome::Success);
    }

    if !confirm_before_write(cli, &planned, action)? {
        return Ok(CliOutcome::ExitWithCode(1));
    }

    if save_current_as_last_state {
        // Save current state as last-state so `reset` itself can be rolled back if desired.
        if let Ok(cur) = session::capture_snapshot(transport, &hw) {
            let _ = store.save_last_state(&cur);
        }
    }

    let ops = session::apply_snapshot(transport, &hw, &snap, true, false)?;
    let out = MutationOutput {
        action: action.into(),
        dry_run: false,
        applied: true,
        rolled_back: false,
        confirmed: None,
        operations: ops,
        note: Some(format!("restored {}", source_path.display())),
    };
    emit_mutation(cli, &out, true);
    Ok(CliOutcome::Success)
}

fn cmd_rollback(cli: &Cli, transport: &dyn SmuTransport) -> Result<CliOutcome> {
    execute_snapshot_restore(cli, transport, SnapshotRestoreKind::Rollback)
}

fn cmd_reset(cli: &Cli, transport: &dyn SmuTransport) -> Result<CliOutcome> {
    execute_snapshot_restore(cli, transport, SnapshotRestoreKind::Reset)
}

// ============================================================================
// Shared Transactional Mutation, Snapshot & Dead-Man's Switch Helpers
// ============================================================================

fn emit_mutation(cli: &Cli, out: &MutationOutput, respect_quiet: bool) {
    if cli.json {
        print_json(out);
    } else if !respect_quiet || !cli.quiet {
        print_mutation(out);
    }
}

fn execute_mutation<F>(
    cli: &Cli,
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    action: &str,
    confirm_secs: Option<u64>,
    mut run_ops: F,
) -> Result<CliOutcome>
where
    F: FnMut(bool) -> Result<Vec<PlannedOp>>,
{
    // Always compute validated plan first (dry_run = true touches zero write sysfs files)
    let planned = run_ops(true)?;

    if cli.dry_run {
        let out = MutationOutput {
            action: action.to_string(),
            dry_run: true,
            applied: false,
            rolled_back: false,
            confirmed: None,
            operations: planned,
            note: None,
        };
        emit_mutation(cli, &out, false);
        return Ok(CliOutcome::Success);
    }

    if !confirm_before_write(cli, &planned, action)? {
        return Ok(CliOutcome::ExitWithCode(1));
    }

    let store = SnapshotStore::new(state_dir(cli));
    let pre_snap = capture_and_store_snapshots(cli, transport, hw, &store)?;

    let ops = run_ops(false)?;

    if let Some(secs) = confirm_secs {
        return wait_for_confirmation_or_rollback(
            cli,
            transport,
            hw,
            pre_snap.as_ref(),
            secs,
            action,
            ops,
        );
    }

    let out = MutationOutput {
        action: action.to_string(),
        dry_run: false,
        applied: true,
        rolled_back: false,
        confirmed: None,
        operations: ops,
        note: None,
    };
    emit_mutation(cli, &out, true);
    Ok(CliOutcome::Success)
}

fn capture_and_store_snapshots(
    cli: &Cli,
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    store: &SnapshotStore,
) -> Result<Option<SmuStateSnapshot>> {
    let snap = session::capture_snapshot(transport, hw)?;
    if let Err(err) = store.ensure_boot_baseline(&snap) {
        if cli.verbose > 0 && !cli.json {
            eprintln!("warning: could not persist boot-baseline snapshot: {err}");
        }
    }
    if let Err(err) = store.save_last_state(&snap) {
        if cli.verbose > 0 && !cli.json {
            eprintln!("warning: could not persist last-state snapshot: {err}");
        }
    }
    Ok(Some(snap))
}

fn confirm_before_write(cli: &Cli, planned: &[PlannedOp], action: &str) -> Result<bool> {
    if cli.yes || cli.json || !io::stdin().is_terminal() {
        return Ok(true);
    }

    print_planned_ops(
        planned,
        &format!("Planned SMU operations for '{action}' ({}):", planned.len()),
    );
    eprint!("Apply these SMU changes? [y/N]: ");
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
        eprintln!("Aborted; no SMU registers were modified.");
        Ok(false)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmOutcome {
    Confirmed,
    Rejected,
    TimedOut,
    Interrupted,
}

impl ConfirmOutcome {
    fn rollback_reason(self) -> Option<&'static str> {
        match self {
            Self::Confirmed => None,
            Self::Rejected => Some("rejected by user"),
            Self::TimedOut => Some("confirmation timed out"),
            Self::Interrupted => Some("interrupted (Ctrl-C)"),
        }
    }
}

fn wait_for_confirmation_or_rollback(
    cli: &Cli,
    transport: &dyn SmuTransport,
    hw: &HardwareContext,
    pre_snap: Option<&SmuStateSnapshot>,
    timeout_secs: u64,
    action: &str,
    ops: Vec<PlannedOp>,
) -> Result<CliOutcome> {
    install_sigint_handler();
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    let mut last_shown = u64::MAX;

    let outcome = loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            break ConfirmOutcome::Interrupted;
        }

        let now = Instant::now();
        if now >= deadline {
            break ConfirmOutcome::TimedOut;
        }

        let rem_secs = deadline.saturating_duration_since(now).as_secs() + 1;
        if rem_secs != last_shown && !cli.quiet && !cli.json {
            eprint!(
                "\rApplied '{action}'. Press Enter or 'y' within {rem_secs:>3}s to keep, or 'n'/Ctrl-C to rollback... "
            );
            let _ = io::stderr().flush();
            last_shown = rem_secs;
        }

        let mut pfd = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut pfd, 1, 100) };
        if rc > 0 && (pfd.revents & libc::POLLIN) != 0 {
            let mut buf = [0u8; 64];
            let n = io::stdin().read(&mut buf).unwrap_or(0);
            if n == 0 {
                break ConfirmOutcome::Rejected;
            }
            let input = String::from_utf8_lossy(buf.get(..n).unwrap_or(&[]));
            let trimmed = input.trim();
            if trimmed.is_empty()
                || trimmed.eq_ignore_ascii_case("y")
                || trimmed.eq_ignore_ascii_case("yes")
            {
                break ConfirmOutcome::Confirmed;
            } else {
                break ConfirmOutcome::Rejected;
            }
        }
    };

    if !cli.quiet && !cli.json {
        eprintln!();
    }

    match outcome.rollback_reason() {
        None => {
            let out = MutationOutput {
                action: action.to_string(),
                dry_run: false,
                applied: true,
                rolled_back: false,
                confirmed: Some(true),
                operations: ops,
                note: Some("confirmed by user".into()),
            };
            emit_mutation(cli, &out, true);
            Ok(CliOutcome::Success)
        }
        Some(reason) => {
            if let Some(snap) = pre_snap {
                session::apply_snapshot(transport, hw, snap, true, false)?;
            }
            let out = MutationOutput {
                action: action.to_string(),
                dry_run: false,
                applied: true,
                rolled_back: true,
                confirmed: Some(false),
                operations: ops,
                note: Some(format!("automatically rolled back ({reason})")),
            };
            emit_mutation(cli, &out, false);
            Ok(CliOutcome::ExitWithCode(1))
        }
    }
}
