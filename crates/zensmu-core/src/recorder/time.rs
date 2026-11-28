use std::time::{SystemTime, UNIX_EPOCH};

/// Current wall-clock Unix timestamp in milliseconds (`0` if system clock predates epoch).
pub(super) fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

/// Howard Hinnant's civil-from-days algorithm converting days since 1970-01-01 to `(year, month, day)`.
fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if month <= 2 { 1 } else { 0 };
    (year, month as u32, day as u32)
}

fn split_utc_secs(unix_secs: u64) -> (i64, u32, u32, u64, u64, u64) {
    let days = (unix_secs / 86_400) as i64;
    let rem = unix_secs % 86_400;
    let hour = rem / 3_600;
    let minute = (rem % 3_600) / 60;
    let second = rem % 60;
    let (year, month, day) = civil_from_days(days);
    (year, month, day, hour, minute, second)
}

/// Format Unix milliseconds as an RFC 3339 UTC string (e.g. `"2026-11-26T22:45:01.250Z"`).
pub fn format_iso8601_utc_ms(timestamp_unix_ms: u64) -> String {
    let secs = timestamp_unix_ms / 1_000;
    let ms = timestamp_unix_ms % 1_000;
    let (year, month, day, hour, minute, second) = split_utc_secs(secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

pub(super) fn format_compact_utc_timestamp(unix_secs: u64) -> String {
    let (year, month, day, hour, minute, second) = split_utc_secs(unix_secs);
    format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}")
}

pub(super) fn format_pts_datetime_utc(unix_secs: u64) -> String {
    let (year, month, day, hour, minute, second) = split_utc_secs(unix_secs);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}")
}
