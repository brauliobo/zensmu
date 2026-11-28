//! JSON and error output parsers for `zensmu` CLI responses.

use super::protocol::{CliError, PersistActionOutput, ReportPayload, StatusPayload};
use zensmu_core::driver::DriverActionOutput;
use zensmu_core::ops::co::{CoSnapshot, CoreCoMargin};
use zensmu_core::ops::pbo::PboStatus;
use zensmu_core::ops::pm_table::TelemetrySnapshot;
use zensmu_core::report::DebugReport;
use zensmu_core::safety::snapshot::SmuStateSnapshot;
use zensmu_core::types::{CurveShaperState, PlannedOp};

/// Decode a failed CLI invocation into a structured [`CliError`], distinguishing
/// motherboard/BIOS firmware clamps (`kind: "hardware_clamped"`) from generic errors.
pub fn parse_cli_error(
    stdout: &str,
    stderr: &str,
    exit_code: Option<i32>,
    uses_pkexec: bool,
) -> CliError {
    if uses_pkexec && exit_code == Some(126) {
        return CliError::Generic("PolicyKit authentication was dismissed or cancelled.".into());
    }
    if uses_pkexec && exit_code == Some(127) {
        return CliError::Generic(
            "PolicyKit could not execute `zensmu` (not found or not authorized).".into(),
        );
    }

    for candidate in [stdout, stderr] {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(candidate) {
            if let Some(msg) = val
                .get("error")
                .and_then(|v| v.as_str())
                .or_else(|| val.get("message").and_then(|v| v.as_str()))
            {
                let is_clamped_kind = val
                    .get("kind")
                    .and_then(|v| v.as_str())
                    .is_some_and(|k| k.eq_ignore_ascii_case("hardware_clamped"));
                if is_clamped_kind || looks_like_hardware_clamped(msg) {
                    let mut caps = extract_clamped_caps_from_json(&val);
                    if caps.is_empty() {
                        caps = parse_clamped_caps(msg);
                    }
                    return CliError::HardwareClamped {
                        message: msg.to_string(),
                        caps,
                    };
                }
                return CliError::Generic(msg.to_string());
            }
        }
    }

    let raw = if !stderr.is_empty() {
        stderr.to_string()
    } else if !stdout.is_empty() {
        stdout.to_string()
    } else {
        format!(
            "Command failed with exit status {}",
            exit_code
                .map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into())
        )
    };

    if looks_like_hardware_clamped(&raw) {
        let caps = parse_clamped_caps(&raw);
        return CliError::HardwareClamped { message: raw, caps };
    }

    CliError::Generic(raw)
}

fn looks_like_hardware_clamped(msg: &str) -> bool {
    msg.to_ascii_lowercase()
        .contains("motherboard/bios firmware clamped")
}

fn extract_clamped_caps_from_json(val: &serde_json::Value) -> Vec<(String, u32)> {
    let mut caps = Vec::new();
    let Some(arr) = val.get("clamped").and_then(|v| v.as_array()) else {
        return caps;
    };
    for item in arr {
        let field = item.get("field").and_then(|v| v.as_str());
        let actual = item
            .get("actual_value")
            .and_then(|v| v.as_u64())
            .and_then(|v| u32::try_from(v).ok());
        if let (Some(f), Some(v)) = (field, actual) {
            if !f.is_empty() {
                caps.push((f.to_string(), v));
            }
        }
    }
    caps
}

/// Parse `field (req ... -> cap N unit)` pairs from a `HardwareClamped` error summary.
pub fn parse_clamped_caps(text: &str) -> Vec<(String, u32)> {
    let mut caps = Vec::new();
    let needle = " (req ";
    let mut search_from = 0usize;

    while let Some(rel_pos) = text[search_from..].find(needle) {
        let open_pos = search_from + rel_pos;
        let prefix = text[..open_pos].trim_end();
        let field_start = prefix
            .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-'))
            .map(|i| i + 1)
            .unwrap_or(0);
        let field = &prefix[field_start..];

        let after_req = open_pos + needle.len();
        let Some(close_rel) = text[after_req..].find(')') else {
            break;
        };
        let inside = &text[after_req..after_req + close_rel];

        let cap_rest = inside
            .split_once("-> cap ")
            .or_else(|| inside.split_once("→ cap "))
            .map(|(_, rest)| rest.trim_start());

        if let Some(rest) = cap_rest {
            let num_token: String = rest
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            let parsed = num_token.parse::<u32>().ok().or_else(|| {
                num_token
                    .parse::<f32>()
                    .ok()
                    .filter(|v| v.is_finite() && *v >= 0.0)
                    .map(|v| v.round() as u32)
            });
            if let Some(val) = parsed {
                if !field.is_empty() {
                    caps.push((field.to_string(), val));
                }
            }
        }

        search_from = after_req + close_rel + 1;
    }

    caps
}

/// Parse `Vec<PlannedOp>` from `zensmu ... --dry-run --json` or live apply JSON output.
///
/// Accepts:
/// - Bare JSON array: `[ { ...PlannedOp... }, ... ]`
/// - Single `PlannedOp` JSON object
/// - Wrapped object containing `ops`, `planned_ops`, `operations`, `plan`, or `applied`
/// - Explicit error object `{ "error": "..." }`
pub fn parse_planned_ops(json: &str) -> Result<Vec<PlannedOp>, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }

    let val: serde_json::Value =
        serde_json::from_str(trimmed).map_err(|e| format!("Invalid JSON from zensmu: {e}"))?;

    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }

    if val.is_array() {
        return serde_json::from_value(val)
            .map_err(|e| format!("Failed to decode planned operations array: {e}"));
    }

    if let Some(obj) = val.as_object() {
        for key in ["ops", "planned_ops", "operations", "plan", "applied"] {
            if let Some(arr) = obj.get(key) {
                if arr.is_array() {
                    return serde_json::from_value(arr.clone())
                        .map_err(|e| format!("Failed to decode '{key}' array: {e}"));
                }
            }
        }
        // Single PlannedOp object?
        if obj.contains_key("opcode") && obj.contains_key("mailbox") {
            let op: PlannedOp = serde_json::from_value(val)
                .map_err(|e| format!("Failed to decode single PlannedOp: {e}"))?;
            return Ok(vec![op]);
        }
    }

    Err(format!(
        "Unexpected JSON payload while parsing planned SMU operations: {trimmed}"
    ))
}

/// Parse a `CoSnapshot` from `zensmu co get --json`.
pub fn parse_co_snapshot(json: &str) -> Result<CoSnapshot, String> {
    let val: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("Invalid CO JSON: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }
    if let Ok(snap) = serde_json::from_value::<CoSnapshot>(val.clone()) {
        return Ok(snap);
    }
    for key in ["co", "curve_optimizer", "data"] {
        if let Some(inner) = val.get(key) {
            if let Ok(snap) = serde_json::from_value::<CoSnapshot>(inner.clone()) {
                return Ok(snap);
            }
        }
    }
    Err("Failed to parse Curve Optimizer snapshot from zensmu JSON output".into())
}

/// Parse a `CurveShaperState` from `zensmu shaper get --json`.
pub fn parse_shaper_state(json: &str) -> Result<CurveShaperState, String> {
    let val: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("Invalid Curve Shaper JSON: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }
    if let Ok(state) = serde_json::from_value::<CurveShaperState>(val.clone()) {
        return Ok(state);
    }
    for key in ["curve_shaper", "shaper", "data"] {
        if let Some(inner) = val.get(key) {
            if let Ok(state) = serde_json::from_value::<CurveShaperState>(inner.clone()) {
                return Ok(state);
            }
        }
    }
    Err("Failed to parse Curve Shaper state from zensmu JSON output".into())
}

/// Parse `DriverActionOutput` from `zensmu driver load|install --json`.
pub fn parse_driver_action_output(json: &str) -> Result<DriverActionOutput, String> {
    let val: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("Invalid driver JSON: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }
    serde_json::from_value(val).map_err(|e| format!("Failed to decode driver action output: {e}"))
}

/// Parse a `PboStatus` from `zensmu pbo get --json`.
pub fn parse_pbo_status(json: &str) -> Result<PboStatus, String> {
    let val: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("Invalid PBO JSON: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }
    for key in ["status", "pbo", "pbo_status", "data"] {
        if let Some(inner) = val.get(key) {
            if let Ok(status) = serde_json::from_value::<PboStatus>(inner.clone()) {
                return Ok(status);
            }
        }
    }
    if let Ok(status) = serde_json::from_value::<PboStatus>(val) {
        return Ok(status);
    }
    Err("Failed to parse PBO status from zensmu JSON output".into())
}

/// Parse combined SMU state from `zensmu status --json`.
///
/// Accepts either a composite status object (`{ co, curve_shaper, pbo, telemetry }`)
/// or a serialized `SmuStateSnapshot`.
pub fn parse_status_payload(json: &str) -> Result<StatusPayload, String> {
    let val: serde_json::Value =
        serde_json::from_str(json.trim()).map_err(|e| format!("Invalid status JSON: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }

    let mut payload = StatusPayload::default();

    for key in ["co", "curve_optimizer"] {
        if let Some(v) = val.get(key) {
            if let Ok(co) = serde_json::from_value::<CoSnapshot>(v.clone()) {
                payload.co = Some(co);
                break;
            }
        }
    }

    for key in ["curve_shaper", "shaper"] {
        if let Some(v) = val.get(key) {
            if let Ok(cs) = serde_json::from_value::<CurveShaperState>(v.clone()) {
                payload.shaper = Some(cs);
                break;
            }
        }
    }

    for key in ["pbo", "pbo_status"] {
        if let Some(v) = val.get(key) {
            if let Ok(pbo) = serde_json::from_value::<PboStatus>(v.clone()) {
                payload.pbo = Some(pbo);
                break;
            }
        }
    }

    if let Some(v) = val.get("telemetry") {
        if let Ok(t) = serde_json::from_value::<TelemetrySnapshot>(v.clone()) {
            payload.telemetry = Some(t);
        }
    }

    // Fallback: if status returned a raw SmuStateSnapshot, unpack its fields.
    if payload.co.is_none() && payload.shaper.is_none() && payload.pbo.is_none() {
        if let Ok(snap) = serde_json::from_value::<SmuStateSnapshot>(val) {
            if let Some(cores) = snap.co_per_core {
                payload.co = Some(CoSnapshot {
                    cores: cores
                        .into_iter()
                        .map(|(core_index, margin)| CoreCoMargin {
                            core_index,
                            ccd: 0,
                            core_in_ccd: core_index,
                            linux_cpus: vec![core_index],
                            margin,
                        })
                        .collect(),
                    igpu: snap.igpu_co,
                });
            }
            payload.shaper = snap.curve_shaper;
            if snap.pbo_scalar.is_some() || snap.fmax_mhz.is_some() {
                payload.pbo = Some(PboStatus {
                    scalar: snap.pbo_scalar,
                    fmax_mhz: snap.fmax_mhz,
                    ..PboStatus::default()
                });
            }
        }
    }

    Ok(payload)
}

/// Parse `PersistActionOutput` from `zensmu persist save|enable|disable --json`.
pub fn parse_persist_action_output(json: &str) -> Result<PersistActionOutput, String> {
    let val: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| format!("Invalid persist JSON from zensmu: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }
    serde_json::from_value(val)
        .map_err(|e| format!("Failed to decode persist action output from zensmu: {e}"))
}

/// Parse `cleared` boolean from `zensmu profile clear-guard --json`.
pub fn parse_clear_guard_output(json: &str) -> Result<bool, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Ok(true);
    }
    let val: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("Invalid clear-guard JSON from zensmu: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }
    val.get("cleared")
        .and_then(|v| v.as_bool())
        .ok_or_else(|| format!("Unexpected JSON payload from clear-guard: {trimmed}"))
}

/// Parse `ReportPayload` from `zensmu report --json`.
///
/// Accepts the wrapped CLI JSON object `{ report, issue_title, issue_url, markdown, ... }`
/// or a bare serialized `DebugReport` (filling derived fields from `DebugReport` methods).
pub fn parse_report_payload(json: &str) -> Result<ReportPayload, String> {
    let val: serde_json::Value = serde_json::from_str(json.trim())
        .map_err(|e| format!("Invalid report JSON from zensmu: {e}"))?;
    if let Some(err) = extract_error_field(&val) {
        return Err(err);
    }

    if let Some(report_val) = val.get("report") {
        let report: DebugReport = serde_json::from_value(report_val.clone())
            .map_err(|e| format!("Failed to decode report field from zensmu: {e}"))?;
        let issue_title = val
            .get("issue_title")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| report.suggested_issue_title());
        let issue_url = val
            .get("issue_url")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| report.github_issue_url());
        let markdown = val
            .get("markdown")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(ToString::to_string)
            .unwrap_or_else(|| report.to_markdown());
        return Ok(ReportPayload {
            report,
            issue_title,
            issue_url,
            markdown,
        });
    }

    if let Ok(report) = serde_json::from_value::<DebugReport>(val) {
        return Ok(ReportPayload::from_report(report));
    }

    Err("Failed to decode diagnostic report from zensmu JSON output".into())
}

fn extract_error_field(val: &serde_json::Value) -> Option<String> {
    if val.get("ok").and_then(|v| v.as_bool()) == Some(false) {
        return val
            .get("error")
            .and_then(|v| v.as_str())
            .or_else(|| val.get("message").and_then(|v| v.as_str()))
            .map(ToString::to_string)
            .or_else(|| Some("CLI reported failure".into()));
    }
    val.get("error")
        .and_then(|v| v.as_str())
        .map(ToString::to_string)
}
