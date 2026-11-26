//! Human-friendly unit and spec parsers plus safe hardware range validators.

use crate::error::{Error, Result};
use crate::hw::profiles::{capabilities_for, CoBounds};
use crate::types::{Codename, TempBand};

// --- Curve Shaper bounds (Zen 5) ---
pub const SHAPER_MIN: i32 = -50;
pub const SHAPER_MAX: i32 = 50;
pub const SHAPER_SOFT_MIN: i32 = -35;

// --- PBO limit bounds ---
pub const PPT_MIN_W: u32 = 10;
pub const PPT_MAX_W: u32 = 1000;
pub const PPT_SOFT_MAX_W: u32 = 400;

pub const TDC_MIN_A: u32 = 10;
pub const TDC_MAX_A: u32 = 600;
pub const TDC_SOFT_MAX_A: u32 = 300;

pub const EDC_MIN_A: u32 = 10;
pub const EDC_MAX_A: u32 = 800;
pub const EDC_SOFT_MAX_A: u32 = 400;

pub const TCTL_MIN_C: u32 = 40;
pub const TCTL_MAX_C: u32 = 105;
pub const TCTL_SOFT_MAX_C: u32 = 95;

pub const PBO_SCALAR_MIN: u32 = 1;
pub const PBO_SCALAR_MAX: u32 = 10;

pub const FMAX_MIN_MHZ: u32 = 1000;
pub const FMAX_MAX_MHZ: u32 = 7000;
pub const FMAX_SOFT_MAX_MHZ: u32 = 6200;

// --- APU DPTC limit bounds ---
pub const APU_POWER_MIN_W: u32 = 5;
pub const APU_POWER_MAX_W: u32 = 300;
pub const APU_POWER_SOFT_MAX_W: u32 = 150;

pub const SKIN_TEMP_MIN_C: u32 = 30;
pub const SKIN_TEMP_MAX_C: u32 = 100;
pub const SKIN_TEMP_SOFT_MAX_C: u32 = 90;

/// Parse a positive unsigned integer with no leading sign or non-digit characters.
fn parse_strict_u64(num_str: &str, original: &str, kind: &str) -> Result<u64> {
    let trimmed = num_str.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::InvalidInput(format!(
            "invalid {kind} value '{original}': expected a positive integer"
        )));
    }
    trimmed.parse::<u64>().map_err(|_| {
        Error::InvalidInput(format!(
            "invalid {kind} value '{original}': numeric overflow"
        ))
    })
}

/// Parse a positive wattage value.
///
/// Accepts `"88"`, `"88W"`, `"88w"`, `"88000mW"`, `"88000mw"` (optional space before unit).
/// Rejects zero, negative values, non-numeric inputs, overflow, and milliwatt values `< 1000mW`.
pub fn parse_watts(s: &str) -> Result<u32> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput("wattage value cannot be empty".into()));
    }

    let lower = trimmed.to_ascii_lowercase();
    let watts_u64 = if let Some(num_part) = lower.strip_suffix("mw") {
        let mw = parse_strict_u64(num_part, s, "wattage")?;
        if mw < 1000 {
            return Err(Error::InvalidInput(format!(
                "invalid wattage value '{s}': milliwatt value must be at least 1000mW (1W)"
            )));
        }
        mw.checked_add(500)
            .ok_or_else(|| {
                Error::InvalidInput(format!("invalid wattage value '{s}': numeric overflow"))
            })?
            / 1000
    } else if let Some(num_part) = lower.strip_suffix('w') {
        parse_strict_u64(num_part, s, "wattage")?
    } else {
        parse_strict_u64(trimmed, s, "wattage")?
    };

    if watts_u64 == 0 {
        return Err(Error::InvalidInput(format!(
            "invalid wattage value '{s}': must be greater than zero"
        )));
    }

    u32::try_from(watts_u64).map_err(|_| {
        Error::InvalidInput(format!(
            "invalid wattage value '{s}': exceeds maximum u32 range"
        ))
    })
}

/// Parse a positive current value in amperes.
///
/// Accepts `"75"`, `"75A"`, `"75a"`, `"75000mA"`, `"75000ma"` (optional space before unit).
/// Rejects zero, negative values, non-numeric inputs, overflow, and milliamp values `< 1000mA`.
pub fn parse_amps(s: &str) -> Result<u32> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput("current value cannot be empty".into()));
    }

    let lower = trimmed.to_ascii_lowercase();
    let amps_u64 = if let Some(num_part) = lower.strip_suffix("ma") {
        let ma = parse_strict_u64(num_part, s, "current")?;
        if ma < 1000 {
            return Err(Error::InvalidInput(format!(
                "invalid current value '{s}': milliamp value must be at least 1000mA (1A)"
            )));
        }
        ma.checked_add(500)
            .ok_or_else(|| {
                Error::InvalidInput(format!("invalid current value '{s}': numeric overflow"))
            })?
            / 1000
    } else if let Some(num_part) = lower.strip_suffix('a') {
        parse_strict_u64(num_part, s, "current")?
    } else {
        parse_strict_u64(trimmed, s, "current")?
    };

    if amps_u64 == 0 {
        return Err(Error::InvalidInput(format!(
            "invalid current value '{s}': must be greater than zero"
        )));
    }

    u32::try_from(amps_u64).map_err(|_| {
        Error::InvalidInput(format!(
            "invalid current value '{s}': exceeds maximum u32 range"
        ))
    })
}

/// Parse a positive temperature value in degrees Celsius.
///
/// Accepts `"85"`, `"85C"`, `"85c"`, `"85°C"`, `"85°c"`.
/// Rejects zero, negative values, non-numeric inputs, and overflow.
pub fn parse_celsius(s: &str) -> Result<u32> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput(
            "temperature value cannot be empty".into(),
        ));
    }

    let num_part = trimmed
        .strip_suffix("°C")
        .or_else(|| trimmed.strip_suffix("°c"))
        .or_else(|| trimmed.strip_suffix('C'))
        .or_else(|| trimmed.strip_suffix('c'))
        .unwrap_or(trimmed);

    let temp_u64 = parse_strict_u64(num_part, s, "temperature")?;
    if temp_u64 == 0 {
        return Err(Error::InvalidInput(format!(
            "invalid temperature value '{s}': must be greater than zero"
        )));
    }

    u32::try_from(temp_u64).map_err(|_| {
        Error::InvalidInput(format!(
            "invalid temperature value '{s}': exceeds maximum u32 range"
        ))
    })
}

/// Parse a positive frequency value in MHz.
///
/// Accepts `"5550"`, `"5550MHz"`, `"5550mhz"`, `"5.55GHz"`, `"5.55ghz"`.
/// Rejects zero, negative values, non-numeric inputs, NaN/Inf, and overflow.
pub fn parse_mhz(s: &str) -> Result<u32> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput(
            "frequency value cannot be empty".into(),
        ));
    }

    let lower = trimmed.to_ascii_lowercase();
    if let Some(num_part) = lower.strip_suffix("ghz") {
        let num_str = num_part.trim();
        if num_str.is_empty()
            || num_str.starts_with('-')
            || num_str.starts_with('+')
            || !num_str
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'.')
            || num_str.bytes().filter(|&b| b == b'.').count() > 1
        {
            return Err(Error::InvalidInput(format!(
                "invalid frequency value '{s}': expected positive number before GHz"
            )));
        }

        let ghz: f64 = num_str.parse().map_err(|_| {
            Error::InvalidInput(format!("invalid frequency value '{s}'"))
        })?;
        if !ghz.is_finite() || ghz <= 0.0 {
            return Err(Error::InvalidInput(format!(
                "invalid frequency value '{s}': must be greater than zero"
            )));
        }

        let mhz_f = (ghz * 1000.0).round();
        if !mhz_f.is_finite() || mhz_f < 1.0 || mhz_f > u32::MAX as f64 {
            return Err(Error::InvalidInput(format!(
                "invalid frequency value '{s}': out of u32 MHz range"
            )));
        }
        return Ok(mhz_f as u32);
    }

    let num_part = lower.strip_suffix("mhz").unwrap_or(trimmed);
    let mhz_u64 = parse_strict_u64(num_part, s, "frequency")?;
    if mhz_u64 == 0 {
        return Err(Error::InvalidInput(format!(
            "invalid frequency value '{s}': must be greater than zero"
        )));
    }

    u32::try_from(mhz_u64).map_err(|_| {
        Error::InvalidInput(format!(
            "invalid frequency value '{s}': exceeds maximum u32 range"
        ))
    })
}

/// Parse per-core Curve Optimizer assignment specs into `(core_index, margin)` pairs.
///
/// Supported formats:
/// - Single core: `"0=-25"`
/// - Inclusive core range: `"2..7=-20"`, `"2..=7=-20"`, `"2-7=-20"`
/// - Core list with shared margin: `"0,2,4=-15"`
/// - Multiple clauses separated by `,` or `;`: `"0=-25,1=-20,2..7=-15"` or `"0,2=-25;1,3=-20"`
///
/// Validates every `core_index < max_cores`.
pub fn parse_core_assignments(spec: &str, max_cores: usize) -> Result<Vec<(u32, i32)>> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput(
            "core assignment specification cannot be empty".into(),
        ));
    }
    if max_cores == 0 {
        return Err(Error::InvalidInput(
            "cannot assign cores when max_cores is 0".into(),
        ));
    }

    let mut result = Vec::new();

    for clause in trimmed.split(';') {
        let clause = clause.trim();
        if clause.is_empty() {
            return Err(Error::InvalidInput(format!(
                "empty clause in core assignment spec '{spec}'"
            )));
        }

        let mut pending_targets: Vec<&str> = Vec::new();
        for token in clause.split(',') {
            let token = token.trim();
            if token.is_empty() {
                return Err(Error::InvalidInput(format!(
                    "empty item in core assignment spec '{spec}'"
                )));
            }

            if let Some((lhs, rhs)) = split_assignment(token)? {
                let lhs = lhs.trim();
                let rhs = rhs.trim();
                if lhs.is_empty() || rhs.is_empty() {
                    return Err(Error::InvalidInput(format!(
                        "invalid core assignment '{token}': expected <core>=<margin>"
                    )));
                }

                let margin: i32 = rhs.parse().map_err(|_| {
                    Error::InvalidInput(format!(
                        "invalid margin '{rhs}' in core assignment '{token}'"
                    ))
                })?;

                pending_targets.push(lhs);
                for target in pending_targets.drain(..) {
                    for core_idx in expand_core_target(target, max_cores)? {
                        result.push((core_idx, margin));
                    }
                }
            } else {
                pending_targets.push(token);
            }
        }

        if !pending_targets.is_empty() {
            return Err(Error::InvalidInput(format!(
                "missing '=<margin>' for core target(s) '{}' in '{spec}'",
                pending_targets.join(",")
            )));
        }
    }

    if result.is_empty() {
        return Err(Error::InvalidInput(format!(
            "no core assignments found in '{spec}'"
        )));
    }

    Ok(result)
}

fn split_assignment(token: &str) -> Result<Option<(&str, &str)>> {
    let bytes = token.as_bytes();
    let mut eq_pos = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'=' {
            let is_inclusive_range = i >= 2 && &bytes[i - 2..i] == b"..";
            if !is_inclusive_range {
                if eq_pos.is_some() {
                    return Err(Error::InvalidInput(format!(
                        "invalid core assignment '{token}': multiple '=' characters"
                    )));
                }
                eq_pos = Some(i);
            }
        }
    }
    Ok(eq_pos.map(|i| (&token[..i], &token[i + 1..])))
}

fn expand_core_target(target: &str, max_cores: usize) -> Result<Vec<u32>> {
    let target = target.trim();
    let range_parts = if let Some((a, b)) = target.split_once("..=") {
        Some((a, b))
    } else if let Some((a, b)) = target.split_once("..") {
        Some((a, b))
    } else {
        target.split_once('-')
    };

    if let Some((start_s, end_s)) = range_parts {
        let start = parse_core_index(start_s, max_cores)?;
        let end = parse_core_index(end_s, max_cores)?;
        if start > end {
            return Err(Error::InvalidInput(format!(
                "invalid core range '{target}': start {start} > end {end}"
            )));
        }
        Ok((start..=end).collect())
    } else {
        let idx = parse_core_index(target, max_cores)?;
        Ok(vec![idx])
    }
}

fn parse_core_index(s: &str, max_cores: usize) -> Result<u32> {
    let trimmed = s.trim();
    if trimmed.is_empty() || !trimmed.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::InvalidInput(format!(
            "invalid core index '{s}': expected non-negative integer"
        )));
    }
    let idx: u32 = trimmed.parse().map_err(|_| {
        Error::InvalidInput(format!("invalid core index '{s}': numeric overflow"))
    })?;
    if (idx as usize) >= max_cores {
        return Err(Error::OutOfBounds {
            field: "core_index".into(),
            value: idx.to_string(),
            bounds: format!("0..{max_cores}"),
        });
    }
    Ok(idx)
}

/// Parse Curve Shaper temperature-band assignments.
///
/// Accepts `"low=-10,med=-15,high=-5"` or `"all=-10"` (case-insensitive; `"medium"` is also accepted).
/// Later entries override earlier entries for the same band (e.g. `"all=-10,high=-5"`).
pub fn parse_shaper_temps(spec: &str) -> Result<Vec<(TempBand, i32)>> {
    let trimmed = spec.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput(
            "curve shaper temperature spec cannot be empty".into(),
        ));
    }

    let mut entries: Vec<(TempBand, i32)> = Vec::new();

    for part in trimmed.split(',') {
        let part = part.trim();
        if part.is_empty() {
            return Err(Error::InvalidInput(format!(
                "empty item in curve shaper temperature spec '{spec}'"
            )));
        }
        let Some((band_raw, margin_raw)) = part.split_once('=') else {
            return Err(Error::InvalidInput(format!(
                "invalid curve shaper item '{part}': expected <low|med|high|all>=<margin>"
            )));
        };
        if margin_raw.contains('=') {
            return Err(Error::InvalidInput(format!(
                "invalid curve shaper item '{part}': multiple '=' characters"
            )));
        }

        let band_str = band_raw.trim();
        let margin_str = margin_raw.trim();
        let margin: i32 = margin_str.parse().map_err(|_| {
            Error::InvalidInput(format!(
                "invalid curve shaper margin '{margin_str}' in '{part}'"
            ))
        })?;

        if band_str.eq_ignore_ascii_case("all") {
            for band in TempBand::ALL {
                upsert_temp_band(&mut entries, band, margin);
            }
        } else if let Some(band) = TempBand::parse(band_str) {
            upsert_temp_band(&mut entries, band, margin);
        } else {
            return Err(Error::InvalidInput(format!(
                "unknown temperature band '{band_str}' in '{part}' (expected low, med, high, or all)"
            )));
        }
    }

    Ok(entries)
}

fn upsert_temp_band(entries: &mut Vec<(TempBand, i32)>, band: TempBand, margin: i32) {
    if let Some((_, existing)) = entries.iter_mut().find(|(b, _)| *b == band) {
        *existing = margin;
    } else {
        entries.push((band, margin));
    }
}

/// Parse a positive duration into milliseconds.
///
/// Accepts `"500ms"`, `"30"`, `"30s"`, `"1m"`, `"1h"` (case-insensitive).
/// Rejects zero, negative values, non-numeric inputs, and overflow.
pub fn parse_duration_ms(s: &str) -> Result<u64> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(Error::InvalidInput("duration value cannot be empty".into()));
    }

    let lower = trimmed.to_ascii_lowercase();
    let (num_part, multiplier): (&str, u64) = if let Some(n) = lower.strip_suffix("ms") {
        (n, 1)
    } else if let Some(n) = lower.strip_suffix('s') {
        (n, 1_000)
    } else if let Some(n) = lower.strip_suffix('m') {
        (n, 60_000)
    } else if let Some(n) = lower.strip_suffix('h') {
        (n, 3_600_000)
    } else {
        (trimmed, 1_000)
    };

    let raw = parse_strict_u64(num_part, s, "duration")?;
    if raw == 0 {
        return Err(Error::InvalidInput(format!(
            "invalid duration value '{s}': must be greater than zero"
        )));
    }

    raw.checked_mul(multiplier).ok_or_else(|| {
        Error::InvalidInput(format!("invalid duration value '{s}': numeric overflow"))
    })
}

/// Parse a positive duration into whole seconds (minimum 1 second for sub-second values like `"500ms"`).
///
/// Accepts `"30"`, `"30s"`, `"1m"`, `"500ms"`, `"1h"`.
pub fn parse_duration_secs(s: &str) -> Result<u64> {
    let ms = parse_duration_ms(s)?;
    Ok(ms.div_ceil(1_000).max(1))
}

// ============================================================================
// Safe hardware range validators
// ============================================================================

/// Validate a Curve Optimizer margin against explicit generation `CoBounds`.
pub fn validate_co_margin(
    field: &str,
    margin: i32,
    bounds: CoBounds,
    allow_extreme: bool,
) -> Result<()> {
    if margin < bounds.min || margin > bounds.max {
        return Err(Error::OutOfBounds {
            field: field.to_string(),
            value: margin.to_string(),
            bounds: format!("{}..={}", bounds.min, bounds.max),
        });
    }
    if !allow_extreme && margin < bounds.soft_min {
        return Err(Error::ExtremeValueBlocked {
            field: field.to_string(),
            value: margin.to_string(),
            threshold: format!(">= {}", bounds.soft_min),
        });
    }
    Ok(())
}

/// Validate a Curve Optimizer margin for a given `Codename`.
pub fn validate_co_margin_for_codename(
    field: &str,
    margin: i32,
    codename: Codename,
    allow_extreme: bool,
) -> Result<()> {
    let caps = capabilities_for(codename).ok_or_else(|| Error::UnsupportedCpu {
        codename: codename.to_string(),
        detail: "no verified capability profile for this CPU".into(),
    })?;
    let bounds = caps.co_bounds.ok_or_else(|| Error::UnsupportedCapability {
        capability: "curve_optimizer",
        codename: codename.to_string(),
    })?;
    validate_co_margin(field, margin, bounds, allow_extreme)
}

/// Validate a Zen 5 Curve Shaper margin (`-50..=50`, conservative soft limit `>= -35`).
pub fn validate_shaper_margin(field: &str, margin: i32, allow_extreme: bool) -> Result<()> {
    if !(SHAPER_MIN..=SHAPER_MAX).contains(&margin) {
        return Err(Error::OutOfBounds {
            field: field.to_string(),
            value: margin.to_string(),
            bounds: format!("{SHAPER_MIN}..={SHAPER_MAX}"),
        });
    }
    if !allow_extreme && margin < SHAPER_SOFT_MIN {
        return Err(Error::ExtremeValueBlocked {
            field: field.to_string(),
            value: margin.to_string(),
            threshold: format!(">= {SHAPER_SOFT_MIN}"),
        });
    }
    Ok(())
}

/// Validate PBO Package Power Tracking (PPT) limit in watts.
pub fn validate_ppt_watts(watts: u32, allow_extreme: bool) -> Result<()> {
    validate_u32_with_soft_max(
        "pbo.ppt_w",
        watts,
        PPT_MIN_W,
        PPT_MAX_W,
        PPT_SOFT_MAX_W,
        "W",
        allow_extreme,
    )
}

/// Validate PBO Thermal Design Current (TDC) limit in amps.
pub fn validate_tdc_amps(amps: u32, allow_extreme: bool) -> Result<()> {
    validate_u32_with_soft_max(
        "pbo.tdc_a",
        amps,
        TDC_MIN_A,
        TDC_MAX_A,
        TDC_SOFT_MAX_A,
        "A",
        allow_extreme,
    )
}

/// Validate PBO Electrical Design Current (EDC) limit in amps.
pub fn validate_edc_amps(amps: u32, allow_extreme: bool) -> Result<()> {
    validate_u32_with_soft_max(
        "pbo.edc_a",
        amps,
        EDC_MIN_A,
        EDC_MAX_A,
        EDC_SOFT_MAX_A,
        "A",
        allow_extreme,
    )
}

/// Validate PBO thermal throttle limit (Tctl / TjMax) in degrees Celsius.
pub fn validate_tctl_celsius(temp_c: u32, allow_extreme: bool) -> Result<()> {
    validate_u32_with_soft_max(
        "pbo.tctl_c",
        temp_c,
        TCTL_MIN_C,
        TCTL_MAX_C,
        TCTL_SOFT_MAX_C,
        "°C",
        allow_extreme,
    )
}

/// Validate PBO scalar (`1..=10`).
pub fn validate_pbo_scalar(scalar: u32) -> Result<()> {
    if !(PBO_SCALAR_MIN..=PBO_SCALAR_MAX).contains(&scalar) {
        return Err(Error::OutOfBounds {
            field: "pbo.scalar".into(),
            value: format!("{scalar}x"),
            bounds: format!("{PBO_SCALAR_MIN}x..={PBO_SCALAR_MAX}x"),
        });
    }
    Ok(())
}

/// Validate Fmax boost clock ceiling in MHz.
pub fn validate_fmax_mhz(mhz: u32, allow_extreme: bool) -> Result<()> {
    validate_u32_with_soft_max(
        "pbo.fmax_mhz",
        mhz,
        FMAX_MIN_MHZ,
        FMAX_MAX_MHZ,
        FMAX_SOFT_MAX_MHZ,
        "MHz",
        allow_extreme,
    )
}

/// Validate an APU power limit (STAPM, Fast, Slow, APU Slow) in watts.
pub fn validate_apu_watts(field: &str, watts: u32, allow_extreme: bool) -> Result<()> {
    validate_u32_with_soft_max(
        field,
        watts,
        APU_POWER_MIN_W,
        APU_POWER_MAX_W,
        APU_POWER_SOFT_MAX_W,
        "W",
        allow_extreme,
    )
}

/// Validate an APU or dGPU skin temperature limit in degrees Celsius.
pub fn validate_skin_temp_celsius(field: &str, temp_c: u32, allow_extreme: bool) -> Result<()> {
    validate_u32_with_soft_max(
        field,
        temp_c,
        SKIN_TEMP_MIN_C,
        SKIN_TEMP_MAX_C,
        SKIN_TEMP_SOFT_MAX_C,
        "°C",
        allow_extreme,
    )
}

fn validate_u32_with_soft_max(
    field: &str,
    value: u32,
    min: u32,
    max: u32,
    soft_max: u32,
    unit: &str,
    allow_extreme: bool,
) -> Result<()> {
    if value < min || value > max {
        return Err(Error::OutOfBounds {
            field: field.to_string(),
            value: format!("{value}{unit}"),
            bounds: format!("{min}{unit}..={max}{unit}"),
        });
    }
    if !allow_extreme && value > soft_max {
        return Err(Error::ExtremeValueBlocked {
            field: field.to_string(),
            value: format!("{value}{unit}"),
            threshold: format!("<= {soft_max}{unit}"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_watts_accepts_units_and_rounds_mw() {
        assert_eq!(parse_watts("88").unwrap(), 88);
        assert_eq!(parse_watts("88W").unwrap(), 88);
        assert_eq!(parse_watts("88w").unwrap(), 88);
        assert_eq!(parse_watts(" 142 W ").unwrap(), 142);
        assert_eq!(parse_watts("88000mW").unwrap(), 88);
        assert_eq!(parse_watts("88000mw").unwrap(), 88);
        assert_eq!(parse_watts("88500mW").unwrap(), 89);
        assert_eq!(parse_watts("88499mW").unwrap(), 88);
    }

    #[test]
    fn parse_watts_rejects_invalid_values() {
        assert!(parse_watts("").is_err());
        assert!(parse_watts("0").is_err());
        assert!(parse_watts("0W").is_err());
        assert!(parse_watts("500mW").is_err());
        assert!(parse_watts("-10W").is_err());
        assert!(parse_watts("abc").is_err());
        assert!(parse_watts("999999999999999999999W").is_err());
        assert!(parse_watts("5000000000W").is_err());
    }

    #[test]
    fn parse_amps_accepts_units_and_rejects_invalid() {
        assert_eq!(parse_amps("75").unwrap(), 75);
        assert_eq!(parse_amps("75A").unwrap(), 75);
        assert_eq!(parse_amps("75a").unwrap(), 75);
        assert_eq!(parse_amps("75000mA").unwrap(), 75);
        assert_eq!(parse_amps("75000ma").unwrap(), 75);
        assert_eq!(parse_amps("75600mA").unwrap(), 76);

        assert!(parse_amps("0").is_err());
        assert!(parse_amps("999mA").is_err());
        assert!(parse_amps("-75A").is_err());
        assert!(parse_amps("75W").is_err());
    }

    #[test]
    fn parse_celsius_accepts_formats_and_rejects_invalid() {
        assert_eq!(parse_celsius("85").unwrap(), 85);
        assert_eq!(parse_celsius("85C").unwrap(), 85);
        assert_eq!(parse_celsius("85c").unwrap(), 85);
        assert_eq!(parse_celsius("85°C").unwrap(), 85);
        assert_eq!(parse_celsius(" 95 °c ").unwrap(), 95);

        assert!(parse_celsius("0").is_err());
        assert!(parse_celsius("-10C").is_err());
        assert!(parse_celsius("85F").is_err());
    }

    #[test]
    fn parse_mhz_accepts_mhz_and_ghz() {
        assert_eq!(parse_mhz("5550").unwrap(), 5550);
        assert_eq!(parse_mhz("5550MHz").unwrap(), 5550);
        assert_eq!(parse_mhz("5550mhz").unwrap(), 5550);
        assert_eq!(parse_mhz("5.55GHz").unwrap(), 5550);
        assert_eq!(parse_mhz("5.55ghz").unwrap(), 5550);
        assert_eq!(parse_mhz("6GHz").unwrap(), 6000);
        assert_eq!(parse_mhz("5.725 GHz").unwrap(), 5725);

        assert!(parse_mhz("0").is_err());
        assert!(parse_mhz("0GHz").is_err());
        assert!(parse_mhz("-5GHz").is_err());
        assert!(parse_mhz("NaNGHz").is_err());
        assert!(parse_mhz("5.5.5GHz").is_err());
        assert!(parse_mhz("5550kHz").is_err());
    }

    #[test]
    fn parse_core_assignments_handles_single_range_list_and_multi() {
        assert_eq!(parse_core_assignments("0=-25", 8).unwrap(), vec![(0, -25)]);
        assert_eq!(
            parse_core_assignments("2..7=-20", 8).unwrap(),
            vec![(2, -20), (3, -20), (4, -20), (5, -20), (6, -20), (7, -20)]
        );
        assert_eq!(
            parse_core_assignments("2..=4=-10", 8).unwrap(),
            vec![(2, -10), (3, -10), (4, -10)]
        );
        assert_eq!(
            parse_core_assignments("2..=4,6=-10", 8).unwrap(),
            vec![(2, -10), (3, -10), (4, -10), (6, -10)]
        );
        assert_eq!(
            parse_core_assignments("0,2,4=-15", 8).unwrap(),
            vec![(0, -15), (2, -15), (4, -15)]
        );
        assert_eq!(
            parse_core_assignments("0=-25,1=-20,2..4=-15", 8).unwrap(),
            vec![(0, -25), (1, -20), (2, -15), (3, -15), (4, -15)]
        );
        assert_eq!(
            parse_core_assignments("0,1=-25;2,3=+5", 8).unwrap(),
            vec![(0, -25), (1, -25), (2, 5), (3, 5)]
        );
    }

    #[test]
    fn parse_core_assignments_rejects_out_of_bounds_and_malformed() {
        assert!(matches!(
            parse_core_assignments("8=-20", 8),
            Err(Error::OutOfBounds { .. })
        ));
        assert!(matches!(
            parse_core_assignments("2..8=-20", 8),
            Err(Error::OutOfBounds { .. })
        ));
        assert!(parse_core_assignments("7..2=-20", 8).is_err());
        assert!(parse_core_assignments("0,2", 8).is_err());
        assert!(parse_core_assignments("", 8).is_err());
        assert!(parse_core_assignments("0=-25", 0).is_err());
        assert!(parse_core_assignments("0=abc", 8).is_err());
        assert!(parse_core_assignments("0=-25=-10", 8).is_err());
        assert!(parse_core_assignments("2..=4", 8).is_err());
    }

    #[test]
    fn parse_shaper_temps_handles_bands_and_all() {
        assert_eq!(
            parse_shaper_temps("low=-10,med=-15,high=-5").unwrap(),
            vec![
                (TempBand::Low, -10),
                (TempBand::Med, -15),
                (TempBand::High, -5),
            ]
        );
        assert_eq!(
            parse_shaper_temps("all=-10").unwrap(),
            vec![
                (TempBand::Low, -10),
                (TempBand::Med, -10),
                (TempBand::High, -10),
            ]
        );
        assert_eq!(
            parse_shaper_temps("all=-10,high=-5").unwrap(),
            vec![
                (TempBand::Low, -10),
                (TempBand::Med, -10),
                (TempBand::High, -5),
            ]
        );
        assert!(parse_shaper_temps("ultra=-5").is_err());
        assert!(parse_shaper_temps("").is_err());
        assert!(parse_shaper_temps("low").is_err());
    }

    #[test]
    fn parse_duration_supports_ms_secs_mins_hours() {
        assert_eq!(parse_duration_ms("500ms").unwrap(), 500);
        assert_eq!(parse_duration_ms("30").unwrap(), 30_000);
        assert_eq!(parse_duration_ms("30s").unwrap(), 30_000);
        assert_eq!(parse_duration_ms("1m").unwrap(), 60_000);
        assert_eq!(parse_duration_ms("2h").unwrap(), 7_200_000);

        assert_eq!(parse_duration_secs("30").unwrap(), 30);
        assert_eq!(parse_duration_secs("30s").unwrap(), 30);
        assert_eq!(parse_duration_secs("1m").unwrap(), 60);
        assert_eq!(parse_duration_secs("500ms").unwrap(), 1);
        assert_eq!(parse_duration_secs("1500ms").unwrap(), 2);

        assert!(parse_duration_ms("0").is_err());
        assert!(parse_duration_ms("0ms").is_err());
        assert!(parse_duration_secs("-5s").is_err());
        assert!(parse_duration_secs("10d").is_err());
    }

    #[test]
    fn validators_enforce_hard_and_soft_bounds() {
        let zen5_co = CoBounds {
            min: -50,
            max: 50,
            soft_min: -35,
        };
        assert!(validate_co_margin("co", -35, zen5_co, false).is_ok());
        assert!(matches!(
            validate_co_margin("co", -40, zen5_co, false),
            Err(Error::ExtremeValueBlocked { .. })
        ));
        assert!(validate_co_margin("co", -40, zen5_co, true).is_ok());
        assert!(matches!(
            validate_co_margin("co", -51, zen5_co, true),
            Err(Error::OutOfBounds { .. })
        ));

        assert!(validate_tctl_celsius(95, false).is_ok());
        assert!(matches!(
            validate_tctl_celsius(100, false),
            Err(Error::ExtremeValueBlocked { .. })
        ));
        assert!(validate_tctl_celsius(100, true).is_ok());
        assert!(matches!(
            validate_tctl_celsius(110, true),
            Err(Error::OutOfBounds { .. })
        ));

        assert!(validate_pbo_scalar(1).is_ok());
        assert!(validate_pbo_scalar(10).is_ok());
        assert!(matches!(
            validate_pbo_scalar(0),
            Err(Error::OutOfBounds { .. })
        ));
        assert!(matches!(
            validate_pbo_scalar(11),
            Err(Error::OutOfBounds { .. })
        ));
    }
}
