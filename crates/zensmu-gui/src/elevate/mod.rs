//! Narrow privilege elevation (`pkexec`) and asynchronous `zensmu` CLI bridge.
//!
//! `zensmu-gui` runs unprivileged by default and never touches privileged sysfs
//! mailbox endpoints directly. Instead, privileged mailbox reads and writes are
//! delegated to the `zensmu` CLI binary via PolicyKit (`pkexec`) on a background
//! thread so the UI never blocks, and every write is preceded by a `--dry-run --json`
//! preview before the user confirms.

mod action;
mod parse;
mod protocol;
mod runner;

#[cfg(test)]
mod tests;

pub use action::CliAction;
#[cfg(test)]
pub use parse::{
    parse_clear_guard_output, parse_cli_error, parse_co_snapshot, parse_pbo_status,
    parse_persist_action_output, parse_planned_ops, parse_report_payload, parse_shaper_state,
    parse_status_payload,
};
#[cfg(test)]
pub use protocol::{
    build_command_spec, find_zensmu_binary_with, parse_is_root_from_proc_status, CliError,
    SYSTEM_ZENSMU_PATH,
};
pub use protocol::{is_running_as_root, ElevatedResult, ReportPayload, StatusPayload};
pub use runner::CommandRunner;
