//! Shared UI primitives, layout helpers, pill badges, slider rows, and formatting helpers.

use super::theme::*;
use crate::app::{ToastLevel, ZensmuApp};
use eframe::egui::{self, Color32, RichText, Stroke, Ui};
use std::collections::BTreeMap;
use zensmu_core::recorder::RecordFormat;

/// Primary or destructive solid CTA button with guaranteed WCAG AA (`>= 5.4:1`) white label.
pub(in crate::views) fn cta_button(
    label: impl Into<String>,
    fill: Color32,
) -> egui::Button<'static> {
    egui::Button::new(RichText::new(label.into()).strong().color(TEXT_ON_CTA)).fill(fill)
}

#[derive(Clone, Copy, Debug)]
struct SplitRowMemory {
    single_line_width: f32,
    wrapped: bool,
}

/// Lay out a left group and a right-aligned group on a single row when they fit within
/// `ui.available_width()`, or stack them into two cleanly wrapped rows when narrow.
///
/// Unlike placing `Layout::right_to_left` inside `horizontal` / `horizontal_wrapped`,
/// the right group is always positioned strictly past `left`, so widgets can never overlap
/// at any window width or UI zoom factor.
pub(in crate::views) fn split_or_wrap_row(
    ui: &mut Ui,
    id_salt: impl std::hash::Hash,
    left: impl FnOnce(&mut Ui),
    right: impl FnOnce(&mut Ui),
) {
    let id = ui.make_persistent_id(id_salt);
    let avail = ui.available_width();
    let item_sp = ui.spacing().item_spacing.x;
    let min_gap = (item_sp * 1.5).max(10.0);

    let prev = ui.data(|d| d.get_temp::<SplitRowMemory>(id));
    let use_wrapped = match prev {
        Some(mem) if mem.wrapped => avail < mem.single_line_width + 8.0,
        Some(mem) => avail < mem.single_line_width,
        None => false,
    };

    if use_wrapped {
        ui.vertical(|ui| {
            ui.set_width(avail);
            let l_rect = ui.horizontal_wrapped(left).response.rect;
            let r_rect = ui.horizontal_wrapped(right).response.rect;
            let single_row_h = ui.spacing().interact_size.y * 1.6;
            if l_rect.height() <= single_row_h && r_rect.height() <= single_row_h {
                let est = l_rect.width() + min_gap + r_rect.width();
                if let Some(mut mem) = prev {
                    if est + 2.0 < mem.single_line_width {
                        mem.single_line_width = est;
                        ui.data_mut(|d| d.insert_temp(id, mem));
                    }
                }
            }
        });
    } else {
        let prev_rw = ui.data(|d| d.get_temp::<f32>(id.with("rw"))).unwrap_or(0.0);
        let mut left_w = 0.0_f32;
        let mut right_w = 0.0_f32;
        ui.horizontal(|ui| {
            left_w = ui.horizontal(left).response.rect.width();
            let extra_min = (min_gap - item_sp).max(0.0);
            let extra_gap = if prev_rw > 0.0 {
                (avail - left_w - prev_rw - item_sp).max(extra_min)
            } else {
                extra_min
            };
            if extra_gap > 0.0 {
                ui.add_space(extra_gap);
            }
            right_w = ui.horizontal(right).response.rect.width();
        });
        let total = left_w + min_gap + right_w;
        let overflowed = total > avail + 0.5;
        ui.data_mut(|d| {
            d.insert_temp(id.with("rw"), right_w);
            d.insert_temp(
                id,
                SplitRowMemory {
                    single_line_width: total,
                    wrapped: overflowed,
                },
            );
        });
        if overflowed || (prev_rw - right_w).abs() > 1.0 {
            ui.ctx().request_repaint();
        }
    }
}

/// Shorten verbose CPUID brand strings (e.g. `"AMD Ryzen 7 9700X 8-Core Processor"` -> `"Ryzen 7 9700X"`,
/// `"AMD Ryzen 7 7840U w/ Radeon 780M Graphics"` -> `"Ryzen 7 7840U"`) so the top bar stays compact
/// while preserving the full brand in hover tooltips.
pub fn compact_cpu_label(raw: &str) -> String {
    let trimmed = raw.trim();
    let without_amd = trimmed
        .strip_prefix("AMD ")
        .or_else(|| trimmed.strip_prefix("amd "))
        .unwrap_or(trimmed);
    let mut words: Vec<&str> = without_amd.split_whitespace().collect();
    if words
        .last()
        .is_some_and(|w| w.eq_ignore_ascii_case("Graphics"))
    {
        if let Some(pos) = words
            .iter()
            .position(|w| w.eq_ignore_ascii_case("w/") || w.eq_ignore_ascii_case("with"))
        {
            words.truncate(pos);
        }
    }
    if words
        .last()
        .is_some_and(|w| w.eq_ignore_ascii_case("Processor"))
    {
        words.pop();
    }
    if words
        .last()
        .is_some_and(|w| w.to_ascii_lowercase().ends_with("-core"))
    {
        words.pop();
    }
    let compact = words.join(" ");
    if compact.is_empty() {
        trimmed.to_string()
    } else {
        compact
    }
}

pub(in crate::views) fn card_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(BG_CARD)
        .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(14))
}

pub(in crate::views) fn panel_frame(margin: egui::Margin) -> egui::Frame {
    egui::Frame::NONE
        .fill(BG_PANEL)
        .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
        .inner_margin(margin)
}

pub(in crate::views) fn pill(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::NONE
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, fg.linear_multiply(0.50)))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(fg));
        });
}

pub(in crate::views) fn tinted_pill(ui: &mut Ui, text: &str, fg: Color32, alpha: u8) {
    pill(ui, text, fg, tinted_bg(fg, alpha));
}

pub(in crate::views) fn muted_pill(ui: &mut Ui, text: &str) {
    pill(ui, text, TEXT_MUTED, BG_ELEVATED);
}

pub(in crate::views) fn optional_fused_pill(
    ui: &mut Ui,
    label: &str,
    value: Option<u32>,
    unit: &str,
) {
    if let Some(v) = value {
        tinted_pill(
            ui,
            &format!("{label}: {v} {unit}"),
            ACCENT_CYAN,
            PILL_ALPHA_CYAN,
        );
    }
}

pub(in crate::views) fn render_tab_header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.heading(title);
    ui.label(RichText::new(subtitle).small().color(TEXT_MUTED));
    ui.add_space(10.0);
}

pub(in crate::views) fn render_unsupported_feature_card(
    ui: &mut Ui,
    feature: &str,
    cpu_name: &str,
    detail: &str,
) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new(format!("{feature} is not supported on {cpu_name}."))
                .strong()
                .color(ACCENT_AMBER),
        );
        ui.add_space(4.0);
        ui.label(detail);
    });
}

pub(in crate::views) fn render_extreme_warning(ui: &mut Ui, msg: &str) {
    ui.label(RichText::new(msg).small().color(ACCENT_AMBER));
}

pub(in crate::views) fn utilization_color(frac: f32, default_accent: Color32) -> Color32 {
    if frac >= 0.90 {
        ACCENT_CRIMSON
    } else if frac >= 0.75 {
        ACCENT_AMBER
    } else {
        default_accent
    }
}

pub(in crate::views) fn capability_disabled_hint(readable: bool) -> &'static str {
    if readable {
        "(read-only on this CPU)"
    } else {
        "(unsupported on this CPU)"
    }
}

pub(in crate::views) fn fused_soft_note(
    fused: Option<u32>,
    fused_prefix: &str,
    soft_max: u32,
    unit: &str,
) -> String {
    fused
        .map(|v| format!("{fused_prefix}: {v} {unit} · soft ≤ {soft_max} {unit}"))
        .unwrap_or_else(|| format!("Soft max ≤ {soft_max} {unit}"))
}

pub(in crate::views) fn grid_header_row(ui: &mut Ui, headers: &[&str]) {
    for &h in headers {
        ui.label(RichText::new(h).strong().color(TEXT_MUTED));
    }
    ui.end_row();
}

pub(in crate::views) struct LimitSliderRow<'a> {
    pub(in crate::views) label: &'a str,
    pub(in crate::views) supported: bool,
    pub(in crate::views) enabled: &'a mut bool,
    pub(in crate::views) value: &'a mut u32,
    pub(in crate::views) dirty: Option<&'a mut bool>,
    pub(in crate::views) range: std::ops::RangeInclusive<u32>,
    pub(in crate::views) step_by: Option<f64>,
    pub(in crate::views) suffix: &'a str,
    pub(in crate::views) live_display: String,
    pub(in crate::views) live_is_muted: bool,
    pub(in crate::views) note: String,
    pub(in crate::views) note_color: Color32,
    pub(in crate::views) board_cap: Option<u32>,
}

pub(in crate::views) fn render_limit_slider_row(ui: &mut Ui, row: LimitSliderRow<'_>) {
    if !row.supported {
        *row.enabled = false;
    }
    ui.add_enabled(row.supported, egui::Checkbox::new(row.enabled, row.label));

    let mut slider = egui::Slider::new(row.value, row.range).suffix(row.suffix);
    if let Some(step) = row.step_by {
        slider = slider.step_by(step);
    }
    if ui
        .add_enabled(row.supported && *row.enabled, slider)
        .changed()
    {
        if let Some(dirty) = row.dirty {
            *dirty = true;
        }
    }

    let mut live_text = RichText::new(row.live_display).monospace();
    if row.live_is_muted {
        live_text = live_text.color(TEXT_MUTED);
    }
    ui.label(live_text);

    if let Some(cap) = row.board_cap {
        let unit = row.suffix.trim();
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            ui.label(RichText::new(row.note).small().color(row.note_color));
            ui.label(
                RichText::new(format!("· Board cap ≤{cap} {unit}"))
                    .small()
                    .strong()
                    .color(ACCENT_AMBER),
            )
            .on_hover_text(
                "Motherboard/BIOS firmware clamped this limit on a previous apply — check BIOS PBO Limits.",
            );
        });
    } else {
        ui.label(RichText::new(row.note).small().color(row.note_color));
    }
    ui.end_row();
}

pub(in crate::views) fn format_board_caps_summary(caps: &BTreeMap<String, u32>) -> String {
    caps.iter()
        .map(|(field, val)| {
            let (label, unit) = match field.as_str() {
                "pbo.ppt_w" => ("PPT", "W"),
                "pbo.tdc_a" => ("TDC", "A"),
                "pbo.edc_a" => ("EDC", "A"),
                "pbo.tctl_c" => ("Tctl", "°C"),
                "apu.stapm_w" => ("STAPM", "W"),
                "apu.fast_w" => ("Fast PPT", "W"),
                "apu.slow_w" => ("Slow PPT", "W"),
                "apu.apu_slow_w" => ("APU Slow", "W"),
                "apu.tdc_soc_a" => ("SoC TDC", "A"),
                "apu.edc_soc_a" => ("SoC EDC", "A"),
                "apu.apu_skin_temp_c" => ("APU Skin", "°C"),
                "apu.dgpu_skin_temp_c" => ("dGPU Skin", "°C"),
                other => (other, ""),
            };
            if unit.is_empty() {
                format!("{label} ≤{val}")
            } else {
                format!("{label} ≤{val} {unit}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

pub(in crate::views) fn format_interval_label(ms: u64) -> String {
    if ms >= 1000 && ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}

pub(in crate::views) fn format_duration_mmss(total_secs: u64) -> String {
    let hours = total_secs / 3600;
    let mins = (total_secs % 3600) / 60;
    let secs = total_secs % 60;
    if hours > 0 {
        format!("{hours}:{mins:02}:{secs:02}")
    } else {
        format!("{mins:02}:{secs:02}")
    }
}

pub(in crate::views) fn format_record_format_short(fmt: RecordFormat) -> &'static str {
    match fmt {
        RecordFormat::Csv => "CSV",
        RecordFormat::Jsonl => "JSONL",
        RecordFormat::Pts => "PTS",
    }
}

pub(in crate::views) fn format_record_format_label(fmt: RecordFormat) -> &'static str {
    match fmt {
        RecordFormat::Csv => "CSV",
        RecordFormat::Jsonl => "JSONL",
        RecordFormat::Pts => "PTS (Phoronix)",
    }
}

pub(in crate::views) fn format_bytes_compact(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.2} MiB", (bytes as f64) / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{:.1} KiB", (bytes as f64) / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

pub(in crate::views) fn handle_open_recordings_folder(app: &mut ZensmuApp) {
    match app.open_recordings_folder() {
        Ok(dir) => {
            app.set_toast(
                ToastLevel::Info,
                format!("Opened recordings folder {}", dir.display()),
            );
        }
        Err(err) => {
            app.set_toast(ToastLevel::Error, err);
        }
    }
}

pub(in crate::views) fn opt_f32(v: Option<f32>, unit: &str) -> String {
    match v {
        Some(x) => format!("{x:.0} {unit}"),
        None => "—".into(),
    }
}

pub(in crate::views) fn flag_pill(ui: &mut Ui, label: &str, val: Option<bool>) {
    match val {
        Some(true) => tinted_pill(
            ui,
            &format!("{label}: Yes"),
            ACCENT_EMERALD,
            PILL_ALPHA_EMERALD,
        ),
        Some(false) => muted_pill(ui, &format!("{label}: No")),
        None => {}
    }
}

pub(in crate::views) fn render_no_hardware_placeholder(app: &mut ZensmuApp, ui: &mut Ui) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("Waiting for ryzen_smu kernel driver")
                .strong()
                .color(ACCENT_AMBER),
        );
        ui.label("Load the driver (`sudo modprobe ryzen_smu`) and click 'Retry Hardware Probe' in the banner above.");
        ui.add_space(8.0);
        if ui
            .add(cta_button("Report Unsupported CPU on GitHub", CTA_EMERALD))
            .on_hover_text(
                "Collect privacy-safe CPU & PM table diagnostics to request hardware support",
            )
            .clicked()
        {
            app.open_report_modal_for_unsupported();
        }
    });
}
