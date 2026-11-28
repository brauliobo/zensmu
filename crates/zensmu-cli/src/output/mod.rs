//! Human-readable terminal rendering and structured JSON serialization for all CLI subcommands.

mod format;
mod json;
mod print;
mod types;

#[cfg(test)]
mod tests;

pub(crate) use format::{detected_board_info, format_board_vrm_note, has_apu_telemetry};
pub use json::{print_error, print_json, print_json_compact};
pub use print::{
    print_bench_report, print_co_table, print_doctor, print_driver_action, print_driver_status,
    print_exporter_service_action, print_exporter_service_status, print_info, print_mutation,
    print_pbo_status, print_persist_action, print_persist_status, print_planned_ops,
    print_profile_diff, print_profile_list, print_record_export_pts, print_record_service_action,
    print_record_service_status, print_record_stats, print_report_footer, print_shaper_matrix,
    print_status, print_telemetry,
};
pub use types::{
    ActivePboLimits, ClearGuardOutput, CoGetOutput, ExporterServiceActionOutput, InfoOutput,
    MutationOutput, PboGetOutput, PersistActionOutput, ProfileDiffOutput, ProfileListItem,
    ProfileListOutput, ProfileSaveOutput, ProfileValidateOutput, RecordExportPtsOutput,
    RecordServiceActionOutput, ReportOutput, ShaperGetOutput, StatusOutput, TelemetryDumpOutput,
};
