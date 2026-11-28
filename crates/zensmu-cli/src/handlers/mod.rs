//! CLI subcommand dispatch and shared execution context helpers.

mod bench;
mod info;
mod monitoring;
mod mutation;
mod persist;
mod profile;
mod signal;
mod tuning;

#[cfg(test)]
mod tests;

use crate::commands::{Cli, Command};
use std::path::PathBuf;
use zensmu_core::error::Result;
use zensmu_core::hw::identity::{detect_hardware, HardwareContext};
use zensmu_core::safety::bounds;
use zensmu_core::safety::guard::DEFAULT_GUARD_PATH;
use zensmu_core::safety::persist::PersistPaths;
use zensmu_core::safety::snapshot::DEFAULT_STATE_DIR;
use zensmu_core::transport::sysfs::{SysfsTransport, DEFAULT_SYSFS_PATH};
use zensmu_core::transport::SmuTransport;

/// Outcome of CLI execution. `ExitWithCode` indicates structured output was
/// already printed to stdout and the process should exit with the given code
/// without printing an extra error message.
pub(crate) enum CliOutcome {
    Success,
    ExitWithCode(u8),
}

pub(crate) fn sysfs_path(cli: &Cli) -> PathBuf {
    cli.sysfs_path
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_SYSFS_PATH))
}

pub(super) fn state_dir(cli: &Cli) -> PathBuf {
    cli.state_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_STATE_DIR))
}

pub(super) fn guard_path(cli: &Cli) -> PathBuf {
    cli.guard_path
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_GUARD_PATH))
}

pub(super) fn persist_paths(cli: &Cli) -> PersistPaths {
    PersistPaths {
        guard_path: guard_path(cli),
        ..PersistPaths::default()
    }
}

pub(super) fn build_transport(cli: &Cli) -> SysfsTransport {
    SysfsTransport::new(sysfs_path(cli))
}

pub(super) fn probe_hw(cli: &Cli, transport: &dyn SmuTransport) -> Result<HardwareContext> {
    detect_hardware(transport, !cli.no_cpuid_check)
}

pub(super) fn parse_confirm_secs(cli: &Cli) -> Result<Option<u64>> {
    match &cli.confirm_within {
        Some(raw) => Ok(Some(bounds::parse_duration_secs(raw)?)),
        None => Ok(None),
    }
}

pub(crate) fn run(cli: &Cli) -> Result<CliOutcome> {
    let transport = build_transport(cli);

    match &cli.command {
        Command::Completions { shell } => info::cmd_completions(*shell),
        Command::Manpage => info::cmd_manpage(),
        Command::Doctor => info::cmd_doctor(cli, &transport),
        Command::Driver { action } => info::cmd_driver(cli, action.as_ref()),
        Command::Report {
            note,
            raw_pm_table,
            no_mailbox,
            output,
            open,
            create,
        } => info::cmd_report(
            cli,
            &transport,
            note.as_deref(),
            *raw_pm_table,
            *no_mailbox,
            output.as_deref(),
            *open,
            *create,
        ),
        Command::Info => info::cmd_info(cli, &transport),
        Command::Status => info::cmd_status(cli, &transport),
        Command::Co { action } => tuning::cmd_co(cli, &transport, action),
        Command::Shaper { action } => tuning::cmd_shaper(cli, &transport, action),
        Command::Pbo { action } => tuning::cmd_pbo(cli, &transport, action),
        Command::Apu { action } => tuning::cmd_apu(cli, &transport, action),
        Command::Telemetry { action } => {
            monitoring::cmd_telemetry(cli, &transport, action.as_ref())
        }
        Command::Record(rec_cmd) => monitoring::cmd_record(cli, &transport, rec_cmd),
        Command::Test(test_cmd) => bench::cmd_test(cli, &transport, test_cmd),
        Command::Export(export_cmd) => monitoring::cmd_export(cli, &transport, export_cmd),
        Command::Top { interval } => monitoring::cmd_top(cli, &transport, interval),
        Command::Profile { action } => profile::cmd_profile(cli, &transport, action),
        Command::Persist(persist_cmd) => persist::cmd_persist(cli, &transport, persist_cmd),
        Command::Rollback => persist::cmd_rollback(cli, &transport),
        Command::Reset => persist::cmd_reset(cli, &transport),
    }
}
