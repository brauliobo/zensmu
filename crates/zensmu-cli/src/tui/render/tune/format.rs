//! Formatting, bounds summaries, and cell styling helpers for Tab 4 (Tune).

use super::super::super::app::App;
use super::super::super::format::{format_signed_margin, margin_color};
use super::super::super::palette;
use super::super::super::types::{format_board_cap, TuneParam, TuneRow, TuneValue};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Cell;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::ops::{apu, co, pbo, shaper};
use zensmu_core::types::ApuPowerMode;

pub(super) fn format_tune_param_label(param: TuneParam, hw: &HardwareContext) -> String {
    match param {
        TuneParam::CoAll => "CO All-Core".to_string(),
        TuneParam::CoCore(idx) => {
            if let Some(c) = hw.topology.core(idx) {
                format!("CO Core {idx} (CCD{}·L{})", c.ccd, c.core_in_ccd)
            } else {
                format!("CO Core {idx}")
            }
        }
        TuneParam::CoIgpu => "CO iGPU".to_string(),
        TuneParam::Shaper { tier, band } => {
            format!(
                "Shaper {:<4} / {:<4} °C",
                tier.label().to_ascii_uppercase(),
                band.label()
            )
        }
        TuneParam::PboPpt => "PPT Limit (W)".to_string(),
        TuneParam::PboTdc => "TDC Limit (A)".to_string(),
        TuneParam::PboEdc => "EDC Limit (A)".to_string(),
        TuneParam::PboTctl => "Tctl Max (°C)".to_string(),
        TuneParam::PboScalar => "PBO Scalar (x)".to_string(),
        TuneParam::PboFmax => "Fmax Boost (MHz)".to_string(),
        TuneParam::ApuStapm => "STAPM Limit (W)".to_string(),
        TuneParam::ApuStapmTime => "STAPM Time (s)".to_string(),
        TuneParam::ApuFast => "Fast PPT Limit (W)".to_string(),
        TuneParam::ApuSlow => "Slow PPT Limit (W)".to_string(),
        TuneParam::ApuSlowTime => "Slow Time (s)".to_string(),
        TuneParam::ApuApuSlow => "APU Slow Limit (W)".to_string(),
        TuneParam::ApuTdcSoc => "SoC TDC Limit (A)".to_string(),
        TuneParam::ApuEdcSoc => "SoC EDC Limit (A)".to_string(),
        TuneParam::ApuSkinTemp => "APU Skin Temp (°C)".to_string(),
        TuneParam::ApuDgpuSkinTemp => "dGPU Skin Temp (°C)".to_string(),
        TuneParam::ApuPowerMode => "Power Mode".to_string(),
    }
}

pub(super) fn format_tune_value(param: TuneParam, val: TuneValue) -> String {
    match (param, val) {
        (
            TuneParam::CoAll | TuneParam::CoCore(_) | TuneParam::CoIgpu | TuneParam::Shaper { .. },
            TuneValue::Int(v),
        ) => format_signed_margin(v as i16),
        (
            TuneParam::PboPpt
            | TuneParam::ApuStapm
            | TuneParam::ApuFast
            | TuneParam::ApuSlow
            | TuneParam::ApuApuSlow,
            TuneValue::Int(v),
        ) => format!("{v} W"),
        (TuneParam::ApuStapmTime | TuneParam::ApuSlowTime, TuneValue::Int(v)) => format!("{v} s"),
        (
            TuneParam::PboTdc | TuneParam::PboEdc | TuneParam::ApuTdcSoc | TuneParam::ApuEdcSoc,
            TuneValue::Int(v),
        ) => format!("{v} A"),
        (
            TuneParam::PboTctl | TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp,
            TuneValue::Int(v),
        ) => format!("{v} °C"),
        (TuneParam::PboScalar, TuneValue::Int(v)) => format!("{v}x"),
        (TuneParam::PboFmax, TuneValue::Int(v)) => format!("{v} MHz"),
        (TuneParam::ApuPowerMode, TuneValue::PowerMode(mode)) => match mode {
            None => "auto".to_string(),
            Some(ApuPowerMode::MaxPerformance) => "max-performance".to_string(),
            Some(ApuPowerMode::PowerSaving) => "power-saving".to_string(),
        },
        (_, TuneValue::Int(v)) => v.to_string(),
        (_, TuneValue::PowerMode(_)) => "—".to_string(),
    }
}

pub(super) fn format_live_param_display(app: &App, param: TuneParam) -> String {
    if param == TuneParam::CoAll {
        if let Some(snap) = app.co_state.as_loaded() {
            if let Some(first) = snap.cores.first() {
                if snap.cores.iter().all(|c| c.margin == first.margin) {
                    return format_signed_margin(first.margin);
                }
                return "mixed".to_string();
            }
        }
        return "—".to_string();
    }
    app.live_param_value(param)
        .map(|v| format_tune_value(param, v))
        .unwrap_or_else(|| "—".to_string())
}

pub(super) fn param_base_bounds_summary(
    app: &App,
    param: TuneParam,
    hw: &HardwareContext,
) -> String {
    match param {
        TuneParam::CoAll | TuneParam::CoCore(_) => hw
            .capabilities
            .co_bounds
            .map(|b| format!("{}..={} (soft >= {})", b.min, b.max, b.soft_min))
            .unwrap_or_else(|| "—".to_string()),
        TuneParam::CoIgpu => format!(
            "{}..={} (soft >= {})",
            co::IGPU_CO_MIN,
            co::IGPU_CO_MAX,
            co::IGPU_CO_SOFT_MIN
        ),
        TuneParam::Shaper { .. } => format!(
            "{}..=+{} (soft >= {})",
            shaper::SHAPER_MIN,
            shaper::SHAPER_MAX,
            shaper::SHAPER_SOFT_MIN
        ),
        TuneParam::PboPpt => format!(
            "{}..={} W (soft <= {} W)",
            pbo::PPT_MIN_W,
            pbo::PPT_MAX_W,
            pbo::PPT_SOFT_MAX_W
        ),
        TuneParam::PboTdc => format!(
            "{}..={} A (soft <= {} A)",
            pbo::TDC_MIN_A,
            pbo::TDC_MAX_A,
            pbo::TDC_SOFT_MAX_A
        ),
        TuneParam::PboEdc => format!(
            "{}..={} A (soft <= {} A)",
            pbo::EDC_MIN_A,
            pbo::EDC_MAX_A,
            pbo::EDC_SOFT_MAX_A
        ),
        TuneParam::PboTctl => format!(
            "{}..={} °C (hard ceiling)",
            pbo::TCTL_MIN_C,
            pbo::TCTL_MAX_C
        ),
        TuneParam::PboScalar => format!(
            "{}..={}x (soft <= {}x)",
            pbo::SCALAR_MIN,
            pbo::SCALAR_MAX,
            pbo::SCALAR_SOFT_MAX
        ),
        TuneParam::PboFmax => {
            if let Some(cur) = app.pbo_state.as_loaded().and_then(|p| p.fmax_mhz) {
                format!(
                    "{}..={} MHz (soft <= {})",
                    pbo::FMAX_MIN_MHZ,
                    pbo::FMAX_MAX_MHZ,
                    cur.saturating_add(pbo::FMAX_SOFT_DELTA_MHZ)
                )
            } else {
                format!("{}..={} MHz", pbo::FMAX_MIN_MHZ, pbo::FMAX_MAX_MHZ)
            }
        }
        TuneParam::ApuStapm | TuneParam::ApuSlow | TuneParam::ApuApuSlow => format!(
            "{}..={} W (soft <= {} W)",
            apu::APU_POWER_MIN_W,
            apu::APU_POWER_MAX_W,
            apu::APU_POWER_SOFT_MAX_W
        ),
        TuneParam::ApuStapmTime | TuneParam::ApuSlowTime => {
            format!("{}..={} s", apu::APU_TIME_MIN_S, apu::APU_TIME_MAX_S)
        }
        TuneParam::ApuFast => format!(
            "{}..={} W (soft <= {} W)",
            apu::APU_POWER_MIN_W,
            apu::APU_POWER_MAX_W,
            apu::APU_FAST_POWER_SOFT_MAX_W
        ),
        TuneParam::ApuTdcSoc => format!(
            "{}..={} A (soft <= {} A)",
            apu::APU_SOC_TDC_MIN_A,
            apu::APU_SOC_TDC_MAX_A,
            apu::APU_SOC_TDC_SOFT_MAX_A
        ),
        TuneParam::ApuEdcSoc => format!(
            "{}..={} A (soft <= {} A)",
            apu::APU_SOC_EDC_MIN_A,
            apu::APU_SOC_EDC_MAX_A,
            apu::APU_SOC_EDC_SOFT_MAX_A
        ),
        TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp => format!(
            "{}..={} °C (soft <= {} °C)",
            apu::APU_SKIN_TEMP_MIN_C,
            apu::APU_SKIN_TEMP_MAX_C,
            apu::APU_SKIN_TEMP_SOFT_MAX_C
        ),
        TuneParam::ApuPowerMode => "auto / max-performance / power-saving".to_string(),
    }
}

pub(in crate::tui) fn param_bounds_summary(
    app: &App,
    param: TuneParam,
    hw: &HardwareContext,
) -> String {
    let mut base = param_base_bounds_summary(app, param, hw);
    if let Some(&cap) = app.board_caps.get(&param) {
        base.push_str(&format!(" · {}", format_board_cap(param, cap)));
    }
    base
}

pub(super) fn estimate_wrapped_lines(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let mut lines = 1usize;
    let mut col = 0usize;
    for word in text.split_whitespace() {
        let wlen = word.chars().count();
        if col == 0 {
            if wlen > width {
                lines += wlen.saturating_sub(1) / width;
                col = ((wlen - 1) % width) + 1;
            } else {
                col = wlen;
            }
        } else if col + 1 + wlen <= width {
            col += 1 + wlen;
        } else {
            lines += 1;
            if wlen > width {
                lines += wlen.saturating_sub(1) / width;
                col = ((wlen - 1) % width) + 1;
            } else {
                col = wlen;
            }
        }
    }
    lines
}

pub(super) fn tune_param_live_style(param: TuneParam, live: Option<TuneValue>) -> Style {
    match (param, live) {
        (
            TuneParam::CoAll | TuneParam::CoCore(_) | TuneParam::CoIgpu | TuneParam::Shaper { .. },
            Some(TuneValue::Int(v)),
        ) => Style::default()
            .fg(margin_color(v as i16))
            .add_modifier(Modifier::BOLD),
        (
            TuneParam::PboPpt
            | TuneParam::ApuStapm
            | TuneParam::ApuFast
            | TuneParam::ApuSlow
            | TuneParam::ApuApuSlow
            | TuneParam::ApuPowerMode,
            Some(_),
        ) => Style::default()
            .fg(palette::CYAN)
            .add_modifier(Modifier::BOLD),
        (
            TuneParam::PboTdc | TuneParam::PboEdc | TuneParam::ApuTdcSoc | TuneParam::ApuEdcSoc,
            Some(_),
        ) => Style::default()
            .fg(palette::TEAL)
            .add_modifier(Modifier::BOLD),
        (TuneParam::ApuStapmTime | TuneParam::ApuSlowTime | TuneParam::PboScalar, Some(_)) => {
            Style::default()
                .fg(palette::PURPLE)
                .add_modifier(Modifier::BOLD)
        }
        (
            TuneParam::PboTctl | TuneParam::ApuSkinTemp | TuneParam::ApuDgpuSkinTemp,
            Some(TuneValue::Int(c)),
        ) => {
            let col = if c >= 90 {
                palette::ORANGE
            } else {
                palette::YELLOW
            };
            Style::default().fg(col).add_modifier(Modifier::BOLD)
        }
        (TuneParam::PboFmax, Some(_)) => Style::default()
            .fg(palette::GREEN)
            .add_modifier(Modifier::BOLD),
        _ => Style::default().fg(palette::MUTED),
    }
}

pub(super) fn format_staged_cell(
    app: &App,
    hw: &HardwareContext,
    param: TuneParam,
    staged_val: Option<TuneValue>,
    is_selected: bool,
) -> Cell<'static> {
    if let Some(sv) = staged_val {
        let exceeds = app.param_exceeds_soft_limit(param, sv, hw);
        let exceeds_cap = app
            .board_caps
            .get(&param)
            .is_some_and(|&cap| sv.as_int().is_some_and(|v| v > cap as i32));
        let suffix = if exceeds && !app.allow_extreme {
            " [!soft]"
        } else if exceeds_cap {
            " [!cap]"
        } else if exceeds {
            " [EXT]"
        } else {
            " *"
        };
        let col = if exceeds && !app.allow_extreme {
            palette::RED
        } else if exceeds_cap {
            palette::YELLOW
        } else {
            palette::ORANGE
        };
        Cell::from(format!("→ {}{suffix}", format_tune_value(param, sv)))
            .style(Style::default().fg(col).add_modifier(Modifier::BOLD))
    } else if let Some(&cap) = app.board_caps.get(&param) {
        Cell::from(format!("· {}", format_board_cap(param, cap))).style(
            Style::default()
                .fg(palette::ORANGE)
                .add_modifier(Modifier::BOLD),
        )
    } else if is_selected {
        Cell::from("←/→ adjust").style(Style::default().fg(palette::CYAN))
    } else {
        Cell::from("·").style(Style::default().fg(palette::DIM))
    }
}

pub(super) fn locate_selected_row_and_scroll(
    tune_rows: &[TuneRow],
    clamped_sel: usize,
    visible_rows: usize,
) -> usize {
    let mut selected_row_idx = 0usize;
    let mut seen_params = 0usize;
    for (row_idx, row) in tune_rows.iter().enumerate() {
        if matches!(row, TuneRow::Param(_)) {
            if seen_params == clamped_sel {
                selected_row_idx = row_idx;
                break;
            }
            seen_params += 1;
        }
    }

    if tune_rows.len() <= visible_rows {
        0
    } else {
        let desired = selected_row_idx.saturating_sub(visible_rows / 2);
        desired.min(tune_rows.len().saturating_sub(visible_rows))
    }
}
