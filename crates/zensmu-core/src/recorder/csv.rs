use super::time::format_iso8601_utc_ms;
use super::RecordedSample;
use crate::error::{Error, Result};
use crate::ops::pm_table::TelemetrySnapshot;
use crate::types::Codename;
use std::collections::HashMap;
use std::fmt::Write as _;

/// Header row for `zensmu` CSV telemetry recordings (34 columns).
pub const CSV_HEADER: &str = "timestamp_unix_ms,timestamp_iso,codename,tag,ppt_w,ppt_limit_w,tdc_a,tdc_limit_a,edc_a,edc_limit_a,tctl_c,tctl_limit_c,stapm_w,stapm_limit_w,slow_w,slow_limit_w,apu_slow_w,apu_slow_limit_w,tdc_soc_a,tdc_soc_limit_a,edc_soc_a,edc_soc_limit_a,apu_skin_c,apu_skin_limit_c,dgpu_skin_c,dgpu_skin_limit_c,fclk_mhz,uclk_mhz,mclk_mhz,vddcr_soc_v,vddcr_misc_v,cldo_vddp_v,cldo_vddg_iod_v,cldo_vddg_ccd_v";

/// Append an RFC 4180 CSV-escaped field to `out`.
fn push_csv_escaped(out: &mut String, field: &str) {
    if field.contains([',', '"', '\n', '\r']) {
        out.push('"');
        for ch in field.chars() {
            if ch == '"' {
                out.push('"');
            }
            out.push(ch);
        }
        out.push('"');
    } else {
        out.push_str(field);
    }
}

fn push_opt_f32(out: &mut String, val: Option<f32>, precision: usize) {
    out.push(',');
    if let Some(v) = val.filter(|v| v.is_finite()) {
        let _ = write!(out, "{v:.precision$}");
    }
}

/// Format one CSV telemetry row matching [`CSV_HEADER`] column order (without trailing newline).
pub(super) fn format_csv_row(
    snap: &TelemetrySnapshot,
    timestamp_unix_ms: u64,
    timestamp_iso: &str,
    tag: Option<&str>,
) -> String {
    let mut row = String::with_capacity(256);
    let _ = write!(
        row,
        "{timestamp_unix_ms},{timestamp_iso},{},",
        snap.codename.slug()
    );
    if let Some(t) = tag {
        push_csv_escaped(&mut row, t);
    }

    push_opt_f32(&mut row, snap.ppt_value_w, 2);
    push_opt_f32(&mut row, snap.ppt_limit_w, 2);
    push_opt_f32(&mut row, snap.tdc_value_a, 2);
    push_opt_f32(&mut row, snap.tdc_limit_a, 2);
    push_opt_f32(&mut row, snap.edc_value_a, 2);
    push_opt_f32(&mut row, snap.edc_limit_a, 2);
    push_opt_f32(&mut row, snap.tctl_value_c, 2);
    push_opt_f32(&mut row, snap.tctl_limit_c, 2);
    push_opt_f32(&mut row, snap.stapm_value_w, 2);
    push_opt_f32(&mut row, snap.stapm_limit_w, 2);
    push_opt_f32(&mut row, snap.slow_value_w, 2);
    push_opt_f32(&mut row, snap.slow_limit_w, 2);
    push_opt_f32(&mut row, snap.apu_slow_value_w, 2);
    push_opt_f32(&mut row, snap.apu_slow_limit_w, 2);
    push_opt_f32(&mut row, snap.tdc_soc_value_a, 2);
    push_opt_f32(&mut row, snap.tdc_soc_limit_a, 2);
    push_opt_f32(&mut row, snap.edc_soc_value_a, 2);
    push_opt_f32(&mut row, snap.edc_soc_limit_a, 2);
    push_opt_f32(&mut row, snap.apu_skin_temp_value_c, 2);
    push_opt_f32(&mut row, snap.apu_skin_temp_limit_c, 2);
    push_opt_f32(&mut row, snap.dgpu_skin_temp_value_c, 2);
    push_opt_f32(&mut row, snap.dgpu_skin_temp_limit_c, 2);
    push_opt_f32(&mut row, snap.fclk_mhz, 2);
    push_opt_f32(&mut row, snap.uclk_mhz, 2);
    push_opt_f32(&mut row, snap.mclk_mhz, 2);
    push_opt_f32(&mut row, snap.vddcr_soc_v, 4);
    push_opt_f32(&mut row, snap.vddcr_misc_v, 4);
    push_opt_f32(&mut row, snap.cldo_vddp_v, 4);
    push_opt_f32(&mut row, snap.cldo_vddg_iod_v, 4);
    push_opt_f32(&mut row, snap.cldo_vddg_ccd_v, 4);

    row
}

/// Minimal RFC 4180 CSV parser supporting quoted fields, doubled quotes (`""`),
/// and embedded commas/newlines.
pub(super) fn parse_csv_records(content: &str) -> Result<Vec<Vec<String>>> {
    let mut records: Vec<Vec<String>> = Vec::new();
    let mut current_row: Vec<String> = Vec::new();
    let mut current_field = String::new();
    let mut in_quotes = false;
    let mut field_started = false;
    let mut chars = content.chars().peekable();

    while let Some(ch) = chars.next() {
        if in_quotes {
            if ch == '"' {
                if chars.peek() == Some(&'"') {
                    let _ = chars.next();
                    current_field.push('"');
                } else {
                    in_quotes = false;
                }
            } else {
                current_field.push(ch);
            }
        } else {
            match ch {
                '"' if !field_started => {
                    in_quotes = true;
                    field_started = true;
                }
                ',' => {
                    current_row.push(std::mem::take(&mut current_field));
                    field_started = false;
                }
                '\r' => {
                    if chars.peek() == Some(&'\n') {
                        let _ = chars.next();
                    }
                    if field_started || !current_row.is_empty() {
                        current_row.push(std::mem::take(&mut current_field));
                        records.push(std::mem::take(&mut current_row));
                        field_started = false;
                    }
                }
                '\n' => {
                    if field_started || !current_row.is_empty() {
                        current_row.push(std::mem::take(&mut current_field));
                        records.push(std::mem::take(&mut current_row));
                        field_started = false;
                    }
                }
                other => {
                    current_field.push(other);
                    field_started = true;
                }
            }
        }
    }

    if in_quotes {
        return Err(Error::InvalidInput(
            "unterminated quoted field in CSV recording".to_string(),
        ));
    }
    if field_started || !current_row.is_empty() {
        current_row.push(current_field);
        records.push(current_row);
    }

    Ok(records)
}

pub(super) fn parse_csv_samples(content: &str) -> Result<Vec<RecordedSample>> {
    let records = parse_csv_records(content)?;
    let Some(header) = records.first() else {
        return Ok(Vec::new());
    };

    let col_map: HashMap<&str, usize> = header
        .iter()
        .enumerate()
        .map(|(i, name)| (name.trim(), i))
        .collect();

    if !col_map.contains_key("timestamp_unix_ms") || !col_map.contains_key("codename") {
        return Err(Error::InvalidInput(
            "CSV recording is missing required header columns (timestamp_unix_ms, codename)"
                .to_string(),
        ));
    }

    let mut samples = Vec::with_capacity(records.len().saturating_sub(1));
    for (row_idx, row) in records.iter().enumerate().skip(1) {
        if row.iter().all(|c| c.trim().is_empty()) {
            continue;
        }
        let get = |col: &str| -> &str {
            col_map
                .get(col)
                .and_then(|&idx| row.get(idx))
                .map(String::as_str)
                .unwrap_or("")
        };
        let get_opt_f32 = |col: &str| -> Result<Option<f32>> {
            let raw = get(col).trim();
            if raw.is_empty() {
                return Ok(None);
            }
            let val = raw.parse::<f32>().map_err(|e| {
                Error::InvalidInput(format!(
                    "invalid float '{raw}' for column '{col}' on CSV row {}: {e}",
                    row_idx + 2
                ))
            })?;
            Ok(val.is_finite().then_some(val))
        };

        let ts_raw = get("timestamp_unix_ms").trim();
        let timestamp_unix_ms = ts_raw.parse::<u64>().map_err(|e| {
            Error::InvalidInput(format!(
                "invalid timestamp_unix_ms '{ts_raw}' on CSV row {}: {e}",
                row_idx + 2
            ))
        })?;

        let iso_raw = get("timestamp_iso").trim();
        let timestamp_iso = if iso_raw.is_empty() {
            format_iso8601_utc_ms(timestamp_unix_ms)
        } else {
            iso_raw.to_string()
        };

        let codename = codename_from_slug(get("codename").trim());
        let tag_raw = get("tag");
        let tag = (!tag_raw.is_empty()).then(|| tag_raw.to_string());

        let telemetry = TelemetrySnapshot {
            codename,
            table_version: 0,
            table_size: 0,
            ppt_limit_w: get_opt_f32("ppt_limit_w")?,
            ppt_value_w: get_opt_f32("ppt_w")?,
            tdc_limit_a: get_opt_f32("tdc_limit_a")?,
            tdc_value_a: get_opt_f32("tdc_a")?,
            edc_limit_a: get_opt_f32("edc_limit_a")?,
            edc_value_a: get_opt_f32("edc_a")?,
            tctl_limit_c: get_opt_f32("tctl_limit_c")?,
            tctl_value_c: get_opt_f32("tctl_c")?,
            stapm_limit_w: get_opt_f32("stapm_limit_w")?,
            stapm_value_w: get_opt_f32("stapm_w")?,
            slow_limit_w: get_opt_f32("slow_limit_w")?,
            slow_value_w: get_opt_f32("slow_w")?,
            apu_slow_limit_w: get_opt_f32("apu_slow_limit_w")?,
            apu_slow_value_w: get_opt_f32("apu_slow_w")?,
            tdc_soc_limit_a: get_opt_f32("tdc_soc_limit_a")?,
            tdc_soc_value_a: get_opt_f32("tdc_soc_a")?,
            edc_soc_limit_a: get_opt_f32("edc_soc_limit_a")?,
            edc_soc_value_a: get_opt_f32("edc_soc_a")?,
            apu_skin_temp_limit_c: get_opt_f32("apu_skin_limit_c")?,
            apu_skin_temp_value_c: get_opt_f32("apu_skin_c")?,
            dgpu_skin_temp_limit_c: get_opt_f32("dgpu_skin_limit_c")?,
            dgpu_skin_temp_value_c: get_opt_f32("dgpu_skin_c")?,
            fclk_mhz: get_opt_f32("fclk_mhz")?,
            uclk_mhz: get_opt_f32("uclk_mhz")?,
            mclk_mhz: get_opt_f32("mclk_mhz")?,
            vddcr_soc_v: get_opt_f32("vddcr_soc_v")?,
            vddcr_misc_v: get_opt_f32("vddcr_misc_v")?,
            cldo_vddp_v: get_opt_f32("cldo_vddp_v")?,
            cldo_vddg_iod_v: get_opt_f32("cldo_vddg_iod_v")?,
            cldo_vddg_ccd_v: get_opt_f32("cldo_vddg_ccd_v")?,
        };

        samples.push(RecordedSample {
            timestamp_unix_ms,
            timestamp_iso,
            tag,
            telemetry,
        });
    }

    Ok(samples)
}

fn codename_from_slug(slug: &str) -> Codename {
    match slug.to_ascii_lowercase().as_str() {
        "colfax" => Codename::Colfax,
        "renoir" => Codename::Renoir,
        "picasso" => Codename::Picasso,
        "matisse" => Codename::Matisse,
        "threadripper" | "thread-ripper" => Codename::ThreadRipper,
        "castle-peak" => Codename::CastlePeak,
        "raven-ridge" => Codename::RavenRidge,
        "raven-ridge-2" | "raven-ridge2" => Codename::RavenRidge2,
        "summit-ridge" => Codename::SummitRidge,
        "pinnacle-ridge" => Codename::PinnacleRidge,
        "rembrandt" => Codename::Rembrandt,
        "vermeer" => Codename::Vermeer,
        "van-gogh" => Codename::VanGogh,
        "cezanne" => Codename::Cezanne,
        "milan" => Codename::Milan,
        "dali" => Codename::Dali,
        "lucienne" => Codename::Lucienne,
        "naples" => Codename::Naples,
        "chagall" => Codename::Chagall,
        "raphael" => Codename::Raphael,
        "phoenix" => Codename::Phoenix,
        "strix-point" => Codename::StrixPoint,
        "granite-ridge" => Codename::GraniteRidge,
        "hawk-point" => Codename::HawkPoint,
        "storm-peak" => Codename::StormPeak,
        "strix-halo" => Codename::StrixHalo,
        _ => Codename::Undefined,
    }
}
