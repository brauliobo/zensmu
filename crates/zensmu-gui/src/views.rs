//! Dark cockpit UI panels, plots, tuning controls, and dry-run confirmation modal.

use crate::app::{
    PersistConfirmModal, PersistConfirmTarget, Tab, ToastLevel, ZensmuApp,
    POLL_INTERVAL_OPTIONS_MS, TELEMETRY_RING_CAPACITY, UI_ZOOM_DEFAULT, UI_ZOOM_MAX, UI_ZOOM_MIN,
    UI_ZOOM_STEP,
};
use crate::elevate::CliAction;
use eframe::egui::{self, Color32, RichText, Stroke, Ui};
use egui_plot::{HLine, Line, Plot, PlotPoints};
use std::collections::BTreeMap;
use zensmu_core::hw::identity::{driver_has_false_ok_bug, HardwareContext};
use zensmu_core::ops::apu::{
    APU_FAST_POWER_SOFT_MAX_W, APU_POWER_MAX_W, APU_POWER_MIN_W, APU_POWER_SOFT_MAX_W,
    APU_SKIN_TEMP_MAX_C, APU_SKIN_TEMP_MIN_C, APU_SKIN_TEMP_SOFT_MAX_C, APU_SOC_EDC_MAX_A,
    APU_SOC_EDC_MIN_A, APU_SOC_EDC_SOFT_MAX_A, APU_SOC_TDC_MAX_A, APU_SOC_TDC_MIN_A,
    APU_SOC_TDC_SOFT_MAX_A, APU_TIME_MAX_S, APU_TIME_MIN_S,
};
use zensmu_core::ops::co::{IGPU_CO_MAX, IGPU_CO_MIN, IGPU_CO_SOFT_MIN};
use zensmu_core::ops::pbo::{
    FusedLimits, EDC_MAX_A, EDC_MIN_A, EDC_SOFT_MAX_A, FMAX_MAX_MHZ, FMAX_MIN_MHZ, PPT_MAX_W,
    PPT_MIN_W, PPT_SOFT_MAX_W, SCALAR_MAX, SCALAR_MIN, SCALAR_SOFT_MAX, TCTL_MAX_C, TCTL_MIN_C,
    TDC_MAX_A, TDC_MIN_A, TDC_SOFT_MAX_A,
};
use zensmu_core::ops::shaper::{SHAPER_MAX, SHAPER_MIN, SHAPER_SOFT_MIN};
use zensmu_core::safety::persist::{
    DEFAULT_ACTIVE_PROFILE_PATH, DEFAULT_MODULES_LOAD_PATH, DEFAULT_RESUME_HOOK_PATH,
    DEFAULT_SERVICE_NAME,
};
use zensmu_core::transport::sysfs::{DEFAULT_LOCK_PATH, DEFAULT_SYSFS_PATH};
use zensmu_core::types::{ApuPowerMode, CoreInfo, FreqTier};

// Dark cockpit palette — every foreground/background pair is validated against WCAG 2.1 AA
// (>= 4.5:1 normal text, >= 3.0:1 graphical components) by the automated tests in `app::tests`.

// Dark surfaces
pub const BG_CANVAS: Color32 = Color32::from_rgb(13, 16, 23); // #0D1017 central backdrop
pub const BG_EXTREME: Color32 = Color32::from_rgb(11, 13, 18); // #0B0D12 inputs, progress tracks, plots
pub const BG_PANEL: Color32 = Color32::from_rgb(18, 22, 31); // #12161F top/bottom bars, sidebar
pub const BG_CARD: Color32 = Color32::from_rgb(22, 27, 38); // #161B26 cards & modals
pub const BG_STRIPE: Color32 = Color32::from_rgb(26, 32, 46); // #1A202E striped table rows (faint_bg_color)
pub const BG_ELEVATED: Color32 = Color32::from_rgb(31, 37, 51); // #1F2533 buttons, muted pills, code chips
pub const BG_HOVERED: Color32 = Color32::from_rgb(42, 49, 65); // #2A3141 hovered interactive controls
pub const BG_OPEN: Color32 = Color32::from_rgb(37, 44, 59); // #252C3B open combo/menu controls

// Borders
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(48, 57, 77); // #30394D decorative card/panel dividers
pub const BORDER_CONTROL: Color32 = Color32::from_rgb(96, 109, 136); // #606D88 interactive control boundaries (>= 3:1)
pub const BORDER_HOVER: Color32 = Color32::from_rgb(148, 163, 184); // #94A3B8 hovered control boundaries

// Text hierarchy
pub const TEXT_BRIGHT: Color32 = Color32::from_rgb(248, 250, 252); // #F8FAFC hero numbers, active/selected labels
pub const TEXT_PRIMARY: Color32 = Color32::from_rgb(226, 232, 240); // #E2E8F0 default body, buttons, inputs
pub const TEXT_MUTED: Color32 = Color32::from_rgb(156, 171, 192); // #9CABC0 secondary labels, hints, headers
pub const TEXT_ON_CTA: Color32 = Color32::WHITE; // #FFFFFF text on solid CTA buttons

// Semantic foreground & graphical accents (all >= 4.98:1 on all dark surfaces & >= 5.11:1 in tinted pills)
pub const ACCENT_CRIMSON: Color32 = Color32::from_rgb(255, 118, 102); // #FF7666 AMD coral-crimson alerts, brand, high util
pub const ACCENT_CYAN: Color32 = Color32::from_rgb(56, 189, 248); // #38BDF8 telemetry info, power, undervolt
pub const ACCENT_AMBER: Color32 = Color32::from_rgb(245, 166, 35); // #F5A623 thermal, soft-limit warnings
pub const ACCENT_EMERALD: Color32 = Color32::from_rgb(52, 211, 153); // #34D399 healthy, active, verified
pub const ACCENT_VIOLET: Color32 = Color32::from_rgb(176, 148, 255); // #B094FF EDC current & secondary metrics

// Solid CTA & selection fills (paired with TEXT_ON_CTA / TEXT_BRIGHT for >= 5.4:1 contrast)
pub const CTA_CRIMSON: Color32 = Color32::from_rgb(185, 34, 34); // #B92222 primary apply/danger buttons (6.34:1 w/ white)
pub const CTA_EMERALD: Color32 = Color32::from_rgb(18, 122, 62); // #127A3E primary persist/enable buttons (5.41:1 w/ white)
pub const SELECTION_BG: Color32 = Color32::from_rgb(104, 32, 36); // #682024 text/combo selection fill (11.02:1 w/ TEXT_BRIGHT)

// Standardized tinted overlay alphas (validated over BG_CANVAS, BG_PANEL, BG_CARD, BG_STRIPE)
pub const PILL_ALPHA_CRIMSON: u8 = 34;
pub const PILL_ALPHA_AMBER: u8 = 28;
pub const PILL_ALPHA_EMERALD: u8 = 28;
pub const PILL_ALPHA_CYAN: u8 = 24;
#[cfg(test)]
pub const PILL_ALPHA_VIOLET: u8 = 26;
pub const BANNER_ALPHA_CRIMSON: u8 = 30;
pub const BANNER_ALPHA_AMBER: u8 = 24;
pub const SIDEBAR_SELECTED_ALPHA: u8 = 36;

/// Construct a translucent tint color from an opaque accent color and alpha byte.
pub fn tinted_bg(accent: Color32, alpha: u8) -> Color32 {
    let [r, g, b, _] = accent.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, alpha)
}

/// Primary or destructive solid CTA button with guaranteed WCAG AA (`>= 5.4:1`) white label.
fn cta_button(label: impl Into<String>, fill: Color32) -> egui::Button<'static> {
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
fn split_or_wrap_row(
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

fn card_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(BG_CARD)
        .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(14))
}

fn panel_frame(margin: egui::Margin) -> egui::Frame {
    egui::Frame::NONE
        .fill(BG_PANEL)
        .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
        .inner_margin(margin)
}

fn pill(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::NONE
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, fg.linear_multiply(0.50)))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(fg));
        });
}

fn tinted_pill(ui: &mut Ui, text: &str, fg: Color32, alpha: u8) {
    pill(ui, text, fg, tinted_bg(fg, alpha));
}

fn muted_pill(ui: &mut Ui, text: &str) {
    pill(ui, text, TEXT_MUTED, BG_ELEVATED);
}

fn optional_fused_pill(ui: &mut Ui, label: &str, value: Option<u32>, unit: &str) {
    if let Some(v) = value {
        tinted_pill(
            ui,
            &format!("{label}: {v} {unit}"),
            ACCENT_CYAN,
            PILL_ALPHA_CYAN,
        );
    }
}

fn render_tab_header(ui: &mut Ui, title: &str, subtitle: &str) {
    ui.heading(title);
    ui.label(RichText::new(subtitle).small().color(TEXT_MUTED));
    ui.add_space(10.0);
}

fn render_unsupported_feature_card(ui: &mut Ui, feature: &str, cpu_name: &str, detail: &str) {
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

fn render_extreme_warning(ui: &mut Ui, msg: &str) {
    ui.label(RichText::new(msg).small().color(ACCENT_AMBER));
}

fn utilization_color(frac: f32, default_accent: Color32) -> Color32 {
    if frac >= 0.90 {
        ACCENT_CRIMSON
    } else if frac >= 0.75 {
        ACCENT_AMBER
    } else {
        default_accent
    }
}

fn capability_disabled_hint(readable: bool) -> &'static str {
    if readable {
        "(read-only on this CPU)"
    } else {
        "(unsupported on this CPU)"
    }
}

fn fused_soft_note(fused: Option<u32>, fused_prefix: &str, soft_max: u32, unit: &str) -> String {
    fused
        .map(|v| format!("{fused_prefix}: {v} {unit} · soft ≤ {soft_max} {unit}"))
        .unwrap_or_else(|| format!("Soft max ≤ {soft_max} {unit}"))
}

fn grid_header_row(ui: &mut Ui, headers: &[&str]) {
    for &h in headers {
        ui.label(RichText::new(h).strong().color(TEXT_MUTED));
    }
    ui.end_row();
}

struct LimitSliderRow<'a> {
    label: &'a str,
    supported: bool,
    enabled: &'a mut bool,
    value: &'a mut u32,
    dirty: Option<&'a mut bool>,
    range: std::ops::RangeInclusive<u32>,
    step_by: Option<f64>,
    suffix: &'a str,
    live_display: String,
    live_is_muted: bool,
    note: String,
    note_color: Color32,
}

fn render_limit_slider_row(ui: &mut Ui, row: LimitSliderRow<'_>) {
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

    ui.label(RichText::new(row.note).small().color(row.note_color));
    ui.end_row();
}

/// Top command bar with brand, compact CPU identity, global actions, and a clean non-overlapping status strip.
pub fn render_top_bar(app: &mut ZensmuApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("top_bar")
        .frame(panel_frame(egui::Margin::symmetric(14, 8)))
        .show(ctx, |ui| {
            let busy = app.runner.is_busy();
            let has_hw = app.hw.is_some();
            let cpu_badge = app.hw.as_ref().map(|hw| {
                let full_brand = hw
                    .cpuid
                    .as_ref()
                    .map(|c| c.brand.as_str())
                    .filter(|b| !b.is_empty())
                    .unwrap_or_else(|| hw.codename.display_name());
                let short_cpu = compact_cpu_label(full_brand);
                let badge = format!("{short_cpu} · SMU {}", hw.driver.smu_version);
                let hover = format!(
                    "{full_brand}\nCodename: {} · {}C/{}T ({} CCD) · SMU {}",
                    hw.codename.slug(),
                    hw.topology.physical_cores,
                    hw.topology.logical_cpus,
                    hw.topology.ccd_count,
                    hw.driver.smu_version
                );
                (badge, hover)
            });

            // Row 1: Brand + compact CPU pill (left) and global telemetry/SMU/zoom controls (right).
            split_or_wrap_row(
                ui,
                "top_bar_primary_row",
                |ui| {
                    ui.label(
                        RichText::new("zensmu")
                            .strong()
                            .size(21.0)
                            .color(ACCENT_CRIMSON),
                    );
                    ui.separator();

                    if let Some((badge, hover)) = &cpu_badge {
                        egui::Frame::NONE
                            .fill(BG_ELEVATED)
                            .stroke(Stroke::new(1.0_f32, BORDER_CONTROL))
                            .corner_radius(egui::CornerRadius::same(12))
                            .inner_margin(egui::Margin::symmetric(9, 3))
                            .show(ui, |ui| {
                                ui.label(RichText::new(badge).small().strong().color(TEXT_PRIMARY))
                                    .on_hover_text(hover);
                            });
                    } else {
                        tinted_pill(ui, "ryzen_smu not detected", ACCENT_AMBER, PILL_ALPHA_AMBER);
                    }
                },
                |ui| {
                    let mut selected_ms = app.poll_interval_ms;
                    egui::ComboBox::from_id_salt("poll_rate_combo")
                        .width(76.0)
                        .selected_text(format_interval_label(selected_ms))
                        .show_ui(ui, |ui| {
                            for ms in POLL_INTERVAL_OPTIONS_MS {
                                ui.selectable_value(
                                    &mut selected_ms,
                                    ms,
                                    format_interval_label(ms),
                                );
                            }
                        });
                    if selected_ms != app.poll_interval_ms {
                        app.set_poll_interval_ms(selected_ms);
                    }

                    let pause_label = if app.telemetry_paused {
                        "Resume"
                    } else {
                        "Pause"
                    };
                    if ui
                        .add_enabled(has_hw, egui::Button::new(pause_label))
                        .on_hover_text("Pause or resume live PM table telemetry polling")
                        .clicked()
                    {
                        app.set_telemetry_paused(!app.telemetry_paused);
                    }

                    ui.separator();

                    if ui
                        .add_enabled(has_hw && !busy, egui::Button::new("Refresh"))
                        .on_hover_text("Read current CO, Curve Shaper, and PBO state via pkexec")
                        .clicked()
                    {
                        app.dispatch_cli(CliAction::Status, ctx);
                    }

                    if ui
                        .add_enabled(has_hw && !busy, egui::Button::new("Rollback"))
                        .on_hover_text("Preview rollback to last pre-change snapshot (dry-run first)")
                        .clicked()
                    {
                        app.dispatch_cli(CliAction::Rollback { dry_run: true }, ctx);
                    }

                    if ui
                        .add_enabled(has_hw && !busy, egui::Button::new("Reset"))
                        .on_hover_text("Preview reset to boot baseline (dry-run first)")
                        .clicked()
                    {
                        app.dispatch_cli(CliAction::Reset { dry_run: true }, ctx);
                    }

                    ui.separator();

                    if ui
                        .add_enabled(
                            app.ui_zoom > UI_ZOOM_MIN + 0.001,
                            egui::Button::new("A-").small(),
                        )
                        .on_hover_text("Decrease UI scale (Ctrl -)")
                        .clicked()
                    {
                        app.adjust_ui_zoom(-UI_ZOOM_STEP, ctx);
                    }

                    let zoom_pct = format!("{:.0}%", app.ui_zoom * 100.0);
                    if ui
                        .add(egui::Button::new(RichText::new(zoom_pct).monospace()).small())
                        .on_hover_text("Reset UI scale to 100% (Ctrl 0)")
                        .clicked()
                    {
                        app.set_ui_zoom(UI_ZOOM_DEFAULT, ctx);
                    }

                    if ui
                        .add_enabled(
                            app.ui_zoom < UI_ZOOM_MAX - 0.001,
                            egui::Button::new("A+").small(),
                        )
                        .on_hover_text("Increase UI scale (Ctrl +)")
                        .clicked()
                    {
                        app.adjust_ui_zoom(UI_ZOOM_STEP, ctx);
                    }
                },
            );

            ui.add_space(4.0);

            // Row 2: Compact wrapped status strip (privilege, boot persistence, crash guard, topology).
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(8.0, 4.0);

                if app.is_root {
                    tinted_pill(
                        ui,
                        "ROOT (uid 0) · run unprivileged",
                        ACCENT_CRIMSON,
                        PILL_ALPHA_CRIMSON,
                    );
                } else {
                    tinted_pill(
                        ui,
                        "Unprivileged · pkexec on apply",
                        ACCENT_EMERALD,
                        PILL_ALPHA_EMERALD,
                    );
                }

                let boot_on = app.persist_status.service_enabled
                    && app.persist_status.active_profile_exists;
                let (boot_text, boot_fg, boot_bg, boot_stroke, boot_hover) = if boot_on {
                    (
                        "Boot: ON",
                        ACCENT_EMERALD,
                        tinted_bg(ACCENT_EMERALD, PILL_ALPHA_EMERALD),
                        ACCENT_EMERALD.linear_multiply(0.55),
                        "Boot persistence is active (zensmu-apply.service enabled with active profile). Click to manage in Profiles & Safety.",
                    )
                } else {
                    (
                        "Boot: OFF",
                        TEXT_MUTED,
                        BG_ELEVATED,
                        BORDER_CONTROL,
                        "Boot persistence is inactive. Click to configure in Profiles & Safety.",
                    )
                };
                if ui
                    .add(
                        egui::Button::new(RichText::new(boot_text).small().strong().color(boot_fg))
                            .fill(boot_bg)
                            .stroke(Stroke::new(1.0_f32, boot_stroke))
                            .corner_radius(egui::CornerRadius::same(12)),
                    )
                    .on_hover_text(boot_hover)
                    .clicked()
                {
                    app.active_tab = Tab::ProfilesSafety;
                    app.refresh_safety_files();
                    app.refresh_persist_status();
                }

                let (guard_text, guard_fg, guard_bg, guard_stroke, guard_hover) =
                    if app.profile_editor.boot_guard_tripped {
                        (
                            "Guard: TRIPPED",
                            ACCENT_CRIMSON,
                            tinted_bg(ACCENT_CRIMSON, PILL_ALPHA_CRIMSON),
                            ACCENT_CRIMSON.linear_multiply(0.60),
                            "Boot-loop crash guard tripped — auto-apply halted. Click to inspect or clear in Profiles & Safety.",
                        )
                    } else {
                        (
                            "Guard: Clean",
                            ACCENT_EMERALD,
                            tinted_bg(ACCENT_EMERALD, PILL_ALPHA_EMERALD),
                            ACCENT_EMERALD.linear_multiply(0.55),
                            "Boot-loop crash guard is armed and clean. Click to view safety status.",
                        )
                    };
                if ui
                    .add(
                        egui::Button::new(
                            RichText::new(guard_text).small().strong().color(guard_fg),
                        )
                        .fill(guard_bg)
                        .stroke(Stroke::new(1.0_f32, guard_stroke))
                        .corner_radius(egui::CornerRadius::same(12)),
                    )
                    .on_hover_text(guard_hover)
                    .clicked()
                {
                    app.active_tab = Tab::ProfilesSafety;
                    app.refresh_safety_files();
                    app.refresh_persist_status();
                }

                if let Some(hw) = &app.hw {
                    muted_pill(
                        ui,
                        &format!(
                            "{} · {}C/{}T",
                            hw.codename.slug(),
                            hw.topology.physical_cores,
                            hw.topology.logical_cpus
                        ),
                    );
                }
            });
        });
}

fn format_interval_label(ms: u64) -> String {
    if ms >= 1000 && ms.is_multiple_of(1000) {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}

/// Bottom status bar showing last command toast / spinner and telemetry sampling status.
pub fn render_bottom_bar(app: &mut ZensmuApp, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("bottom_bar")
        .frame(panel_frame(egui::Margin::symmetric(14, 6)))
        .show(ctx, |ui| {
            let rate_text = if app.hw.is_none() {
                "Telemetry offline".into()
            } else if app.telemetry_paused {
                format!(
                    "Paused · {}/{} samples",
                    app.samples.len(),
                    TELEMETRY_RING_CAPACITY
                )
            } else {
                let hz = 1000.0 / (app.poll_interval_ms.max(1) as f64);
                format!(
                    "{hz:.1} Hz ({}) · {}/{} samples",
                    format_interval_label(app.poll_interval_ms),
                    app.samples.len(),
                    TELEMETRY_RING_CAPACITY
                )
            };

            split_or_wrap_row(
                ui,
                "bottom_bar_row",
                |ui| {
                    if let Some(label) = app.runner.in_flight_label() {
                        ui.spinner();
                        ui.label(RichText::new(format!("Running: {label}…")).color(ACCENT_CYAN));
                    } else if let Some(toast) = &app.toast {
                        let color = match toast.level {
                            ToastLevel::Info => ACCENT_CYAN,
                            ToastLevel::Success => ACCENT_EMERALD,
                            ToastLevel::Warning => ACCENT_AMBER,
                            ToastLevel::Error => ACCENT_CRIMSON,
                        };
                        let age = toast.created_at.elapsed().as_secs();
                        ui.label(RichText::new(&toast.message).color(color));
                        if age > 2 {
                            ui.label(
                                RichText::new(format!("({age}s ago)"))
                                    .small()
                                    .color(TEXT_MUTED),
                            );
                        }
                    } else {
                        ui.label(
                            RichText::new("Ready — telemetry reads sysfs without root.")
                                .color(TEXT_MUTED),
                        );
                    }
                },
                |ui| {
                    ui.label(
                        RichText::new(rate_text)
                            .small()
                            .monospace()
                            .color(TEXT_MUTED),
                    );
                },
            );
        });
}

/// Left sidebar navigation and compact hardware summary card.
pub fn render_sidebar(app: &mut ZensmuApp, ctx: &egui::Context) {
    egui::SidePanel::left("sidebar")
        .resizable(false)
        .exact_width(245.0)
        .frame(panel_frame(egui::Margin::same(12)))
        .show(ctx, |ui| {
            ui.label(
                RichText::new("NAVIGATION")
                    .small()
                    .strong()
                    .color(TEXT_MUTED),
            );
            ui.add_space(6.0);

            let pbo_subtitle =
                if app.hw.as_ref().is_some_and(|h| {
                    h.codename.is_apu() || h.capabilities.set_stapm_limit.is_some()
                }) {
                    "PPT / TDC / EDC / APU DPTC"
                } else {
                    "PPT / TDC / EDC / Fmax"
                };
            let tabs = [
                (Tab::Dashboard, "Dashboard", "Live telemetry"),
                (
                    Tab::CurveOptimizer,
                    "Curve Optimizer",
                    "All-core & per-core",
                ),
                (Tab::CurveShaper, "Curve Shaper", "Zen 5 5×3 matrix"),
                (Tab::PboLimits, "PBO Limits", pbo_subtitle),
                (
                    Tab::ProfilesSafety,
                    "Profiles & Safety",
                    "Boot persist, TOML, guards",
                ),
            ];

            for (tab, title, subtitle) in tabs {
                let selected = app.active_tab == tab;
                let fill = if selected {
                    tinted_bg(ACCENT_CRIMSON, SIDEBAR_SELECTED_ALPHA)
                } else {
                    Color32::TRANSPARENT
                };
                let stroke = if selected {
                    Stroke::new(1.0_f32, ACCENT_CRIMSON)
                } else {
                    Stroke::NONE
                };

                let resp = egui::Frame::NONE
                    .fill(fill)
                    .stroke(stroke)
                    .corner_radius(egui::CornerRadius::same(6))
                    .inner_margin(egui::Margin::symmetric(10, 8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.vertical(|ui| {
                            ui.label(RichText::new(title).strong().color(if selected {
                                TEXT_BRIGHT
                            } else {
                                TEXT_PRIMARY
                            }));
                            ui.label(RichText::new(subtitle).small().color(TEXT_MUTED));
                        });
                    })
                    .response
                    .interact(egui::Sense::click());

                if resp.clicked() {
                    app.active_tab = tab;
                    if tab == Tab::ProfilesSafety {
                        app.refresh_safety_files();
                        app.refresh_persist_status();
                    }
                }
                ui.add_space(4.0);
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                card_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(RichText::new("SYSTEM").small().strong().color(TEXT_MUTED));
                    ui.add_space(4.0);
                    if let Some(hw) = &app.hw {
                        kv_small(ui, "Codename", hw.codename.slug());
                        kv_small(
                            ui,
                            "Topology",
                            &format!(
                                "{}C / {}T ({} CCD)",
                                hw.topology.physical_cores,
                                hw.topology.logical_cpus,
                                hw.topology.ccd_count
                            ),
                        );
                        kv_small(ui, "ryzen_smu", &hw.driver.drv_version);
                        if let Some(ver) = hw.driver.pm_table_version {
                            kv_small(ui, "PM Table", &format!("0x{ver:08X}"));
                        }
                    } else {
                        ui.label(RichText::new("No SMU hardware").small().color(ACCENT_AMBER));
                    }
                    kv_small(
                        ui,
                        "CLI",
                        app.runner
                            .zensmu_bin()
                            .file_name()
                            .and_then(|s| s.to_str())
                            .unwrap_or("zensmu"),
                    );
                });
            });
        });
}

fn kv_small(ui: &mut Ui, key: &str, val: &str) {
    split_or_wrap_row(
        ui,
        ("sidebar_kv", key),
        |ui| {
            ui.label(RichText::new(key).small().color(TEXT_MUTED));
        },
        |ui| {
            ui.label(RichText::new(val).small().monospace().color(TEXT_PRIMARY));
        },
    );
}

/// Warning / actionable banners shown above the main content area.
pub fn render_banners(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    if app.is_root {
        egui::Frame::NONE
            .fill(tinted_bg(ACCENT_CRIMSON, BANNER_ALPHA_CRIMSON))
            .stroke(Stroke::new(1.0_f32, ACCENT_CRIMSON))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(
                        RichText::new("WARNING: Running zensmu-gui as root (uid 0).")
                            .strong()
                            .color(ACCENT_CRIMSON),
                    );
                    ui.label(
                        RichText::new(
                            "Please launch zensmu-gui as your normal user account. Telemetry reads sysfs without root, and PolicyKit (pkexec) elevates only individual zensmu CLI commands.",
                        )
                        .color(TEXT_PRIMARY),
                    );
                });
            });
        ui.add_space(10.0);
    }

    if let Some(err) = app.hw_error.clone() {
        egui::Frame::NONE
            .fill(tinted_bg(ACCENT_AMBER, BANNER_ALPHA_AMBER))
            .stroke(Stroke::new(1.0_f32, ACCENT_AMBER))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(
                    RichText::new("ryzen_smu kernel module is not available")
                        .strong()
                        .color(ACCENT_AMBER),
                );
                ui.label(RichText::new(&err).small().color(TEXT_PRIMARY));
                ui.add_space(6.0);
                split_or_wrap_row(
                    ui,
                    "hw_error_banner_actions",
                    |ui| {
                        ui.label(RichText::new("Load the driver with:").color(TEXT_PRIMARY));
                        ui.code("sudo modprobe ryzen_smu");
                    },
                    |ui| {
                        if ui.button("Retry Hardware Probe").clicked() {
                            app.probe_hardware(ctx);
                        }
                    },
                );
            });
        ui.add_space(10.0);
    }
}

/// Tab 1: Live Dashboard with rolling plots, limit progress bars, clocks, and voltage rails.
pub fn render_dashboard(app: &mut ZensmuApp, ui: &mut Ui) {
    render_tab_header(
        ui,
        "Live SMU Telemetry Dashboard",
        "Unprivileged 0444 PM table polling from /sys/kernel/ryzen_smu_drv/pm_table",
    );

    if let Some(err) = &app.telemetry_error {
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new("PM Table Telemetry Unavailable")
                    .strong()
                    .color(ACCENT_AMBER),
            );
            ui.label(err);
            ui.label(
                RichText::new(
                    "zensmu strictly refuses to guess unverified PM table layouts or read tables exceeding 4096 bytes.",
                )
                .small()
                .color(TEXT_MUTED),
            );
        });
        ui.add_space(12.0);
    }

    let snap = app.latest_telemetry.as_ref();

    // Row 1: Power / Thermal / Current Limit Gauges (4 columns wide, or 2×2 when narrow/zoomed)
    let gauge_specs = [
        (
            "PPT Power",
            snap.and_then(|s| s.ppt_value_w),
            snap.and_then(|s| s.ppt_limit_w),
            "W",
            ACCENT_CYAN,
        ),
        (
            "Tctl Temp",
            snap.and_then(|s| s.tctl_value_c),
            snap.and_then(|s| s.tctl_limit_c),
            "°C",
            ACCENT_AMBER,
        ),
        (
            "TDC Current",
            snap.and_then(|s| s.tdc_value_a),
            snap.and_then(|s| s.tdc_limit_a),
            "A",
            ACCENT_EMERALD,
        ),
        (
            "EDC Current",
            snap.and_then(|s| s.edc_value_a),
            snap.and_then(|s| s.edc_limit_a),
            "A",
            ACCENT_VIOLET,
        ),
    ];
    if ui.available_width() >= 680.0 {
        ui.columns(4, |cols| {
            for (idx, (title, value, limit, unit, accent)) in gauge_specs.iter().enumerate() {
                render_gauge_card(&mut cols[idx], title, *value, *limit, unit, *accent);
            }
        });
    } else {
        for pair in gauge_specs.chunks(2) {
            ui.columns(2, |cols| {
                for (idx, (title, value, limit, unit, accent)) in pair.iter().enumerate() {
                    render_gauge_card(&mut cols[idx], title, *value, *limit, unit, *accent);
                }
            });
            ui.add_space(8.0);
        }
    }

    // Optional Row 1b: APU DPTC Limits card (only shown when APU telemetry fields are present)
    if let Some(s) = snap {
        struct ApuMetricRow<'a> {
            label: &'a str,
            value: Option<f32>,
            limit: Option<f32>,
            unit: &'a str,
            accent: Color32,
        }

        let apu_metrics = [
            ApuMetricRow {
                label: "STAPM Power",
                value: s.stapm_value_w,
                limit: s.stapm_limit_w,
                unit: "W",
                accent: ACCENT_CYAN,
            },
            ApuMetricRow {
                label: "Slow PPT",
                value: s.slow_value_w,
                limit: s.slow_limit_w,
                unit: "W",
                accent: ACCENT_CYAN,
            },
            ApuMetricRow {
                label: "APU Slow",
                value: s.apu_slow_value_w,
                limit: s.apu_slow_limit_w,
                unit: "W",
                accent: ACCENT_CYAN,
            },
            ApuMetricRow {
                label: "SoC TDC",
                value: s.tdc_soc_value_a,
                limit: s.tdc_soc_limit_a,
                unit: "A",
                accent: ACCENT_EMERALD,
            },
            ApuMetricRow {
                label: "SoC EDC",
                value: s.edc_soc_value_a,
                limit: s.edc_soc_limit_a,
                unit: "A",
                accent: ACCENT_VIOLET,
            },
            ApuMetricRow {
                label: "APU Skin Temp",
                value: s.apu_skin_temp_value_c,
                limit: s.apu_skin_temp_limit_c,
                unit: "°C",
                accent: ACCENT_AMBER,
            },
            ApuMetricRow {
                label: "dGPU Skin Temp",
                value: s.dgpu_skin_temp_value_c,
                limit: s.dgpu_skin_temp_limit_c,
                unit: "°C",
                accent: ACCENT_AMBER,
            },
        ];

        let present_apu: Vec<_> = apu_metrics
            .into_iter()
            .filter(|m| m.limit.is_some() || m.value.is_some())
            .collect();

        if !present_apu.is_empty() {
            ui.add_space(12.0);
            card_frame().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("APU DPTC Limits").strong());
                    tinted_pill(
                        ui,
                        "Mobile Power & Thermal Telemetry",
                        ACCENT_CYAN,
                        PILL_ALPHA_CYAN,
                    );
                });
                ui.add_space(8.0);

                egui::Grid::new("dashboard_apu_dptc_grid")
                    .num_columns(3)
                    .striped(true)
                    .spacing([14.0, 8.0])
                    .show(ui, |ui| {
                        for m in present_apu {
                            render_apu_limit_row(ui, m.label, m.value, m.limit, m.unit, m.accent);
                        }
                    });
            });
        }
    }

    ui.add_space(12.0);

    // Row 2: Live rolling time-series plots (PPT W and Tctl C)
    let latest_t = app.samples.back().map(|s| s.elapsed_s).unwrap_or(0.0);

    let ppt_points: PlotPoints<'_> = app
        .samples
        .iter()
        .filter_map(|s| {
            s.snapshot
                .ppt_value_w
                .map(|v| [s.elapsed_s - latest_t, v as f64])
        })
        .collect();
    let tctl_points: PlotPoints<'_> = app
        .samples
        .iter()
        .filter_map(|s| {
            s.snapshot
                .tctl_value_c
                .map(|v| [s.elapsed_s - latest_t, v as f64])
        })
        .collect();

    let ppt_limit = snap.and_then(|s| s.ppt_limit_w).map(f64::from);
    let tctl_limit = snap.and_then(|s| s.tctl_limit_c).map(f64::from);

    ui.columns(2, |cols| {
        render_rolling_plot_card(
            &mut cols[0],
            RollingPlotSpec {
                plot_id: "ppt_plot",
                title: "Package Power (PPT)",
                series_name: "PPT (W)",
                limit_name: "PPT Limit",
                unit: "W",
                accent: ACCENT_CYAN,
                current: snap.and_then(|s| s.ppt_value_w),
                limit: ppt_limit,
                include_y: &[0.0],
                points: ppt_points,
            },
        );

        render_rolling_plot_card(
            &mut cols[1],
            RollingPlotSpec {
                plot_id: "tctl_plot",
                title: "CPU Temperature (Tctl)",
                series_name: "Tctl (°C)",
                limit_name: "Tctl Limit",
                unit: "°C",
                accent: ACCENT_AMBER,
                current: snap.and_then(|s| s.tctl_value_c),
                limit: tctl_limit,
                include_y: &[20.0, 100.0],
                points: tctl_points,
            },
        );
    });

    ui.add_space(12.0);

    // Row 3: Clocks & Voltage Rails cards
    ui.columns(2, |cols| {
        card_frame().show(&mut cols[0], |ui| {
            ui.set_width(ui.available_width());
            split_or_wrap_row(
                ui,
                "clocks_card_header",
                |ui| {
                    ui.label(RichText::new("Fabric & Memory Clocks").strong());
                },
                |ui| {
                    if let (Some(uclk), Some(mclk)) =
                        (snap.and_then(|s| s.uclk_mhz), snap.and_then(|s| s.mclk_mhz))
                    {
                        let ratio = if (uclk - mclk).abs() < 25.0 {
                            "UCLK:MCLK 1:1"
                        } else if (uclk * 2.0 - mclk).abs() < 50.0 {
                            "UCLK:MCLK 1:2"
                        } else {
                            "Async"
                        };
                        tinted_pill(ui, ratio, ACCENT_CYAN, PILL_ALPHA_CYAN);
                    }
                },
            );
            ui.add_space(8.0);
            egui::Grid::new("clocks_grid")
                .num_columns(2)
                .striped(true)
                .min_col_width(180.0)
                .show(ui, |ui| {
                    metric_row(
                        ui,
                        "FCLK (Infinity Fabric)",
                        snap.and_then(|s| s.fclk_mhz),
                        "MHz",
                        0,
                    );
                    metric_row(
                        ui,
                        "UCLK (Memory Controller)",
                        snap.and_then(|s| s.uclk_mhz),
                        "MHz",
                        0,
                    );
                    metric_row(
                        ui,
                        "MCLK (Memory Clock)",
                        snap.and_then(|s| s.mclk_mhz),
                        "MHz",
                        0,
                    );
                });
        });

        card_frame().show(&mut cols[1], |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Voltage Rails (Read-Only)").strong());
            ui.add_space(8.0);
            egui::Grid::new("voltages_grid")
                .num_columns(2)
                .striped(true)
                .min_col_width(180.0)
                .show(ui, |ui| {
                    metric_row(ui, "VDDCR_SOC", snap.and_then(|s| s.vddcr_soc_v), "V", 3);
                    metric_row(ui, "VDDCR_MISC", snap.and_then(|s| s.vddcr_misc_v), "V", 3);
                    metric_row(ui, "CLDO_VDDP", snap.and_then(|s| s.cldo_vddp_v), "V", 3);
                    metric_row(ui, "VDDG IOD", snap.and_then(|s| s.cldo_vddg_iod_v), "V", 3);
                    metric_row(ui, "VDDG CCD", snap.and_then(|s| s.cldo_vddg_ccd_v), "V", 3);
                });
        });
    });
}

struct RollingPlotSpec<'a> {
    plot_id: &'a str,
    title: &'a str,
    series_name: &'a str,
    limit_name: &'a str,
    unit: &'a str,
    accent: Color32,
    current: Option<f32>,
    limit: Option<f64>,
    include_y: &'a [f64],
    points: PlotPoints<'a>,
}

fn render_rolling_plot_card(ui: &mut Ui, spec: RollingPlotSpec<'_>) {
    card_frame().show(ui, |ui| {
        split_or_wrap_row(
            ui,
            ("plot_header", spec.plot_id),
            |ui| {
                ui.label(RichText::new(spec.title).strong());
            },
            |ui| {
                if let Some(v) = spec.current {
                    ui.label(
                        RichText::new(format!("{v:.1} {}", spec.unit))
                            .monospace()
                            .strong()
                            .color(spec.accent),
                    );
                }
            },
        );
        ui.add_space(4.0);
        let mut plot = Plot::new(spec.plot_id)
            .height(185.0)
            .allow_drag(false)
            .allow_zoom(false)
            .allow_scroll(false)
            .allow_boxed_zoom(false)
            .y_axis_label(spec.unit)
            .x_axis_label("seconds ago");
        for &y in spec.include_y {
            plot = plot.include_y(y);
        }
        plot.show(ui, |plot_ui| {
            plot_ui.line(
                Line::new(spec.points)
                    .name(spec.series_name)
                    .color(spec.accent)
                    .width(2.0_f32),
            );
            if let Some(lim) = spec.limit {
                plot_ui.hline(
                    HLine::new(lim)
                        .name(spec.limit_name)
                        .color(ACCENT_CRIMSON)
                        .style(egui_plot::LineStyle::Dashed { length: 6.0 }),
                );
            }
        });
    });
}

fn render_gauge_card(
    ui: &mut Ui,
    title: &str,
    value: Option<f32>,
    limit: Option<f32>,
    unit: &str,
    accent: Color32,
) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new(title).small().strong().color(TEXT_MUTED));
        ui.add_space(4.0);
        match (value, limit) {
            (Some(val), Some(lim)) if lim > 0.0 => {
                let frac = (val / lim).clamp(0.0, 1.0);
                let fill = utilization_color(frac, accent);
                ui.label(
                    RichText::new(format!("{val:.1} {unit}"))
                        .size(24.0)
                        .strong()
                        .monospace()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                split_or_wrap_row(
                    ui,
                    ("gauge_caption", title),
                    |ui| {
                        ui.label(
                            RichText::new(format!("{:.0}%", frac * 100.0))
                                .small()
                                .strong()
                                .monospace()
                                .color(fill),
                        );
                    },
                    |ui| {
                        ui.label(
                            RichText::new(format!("of {lim:.0} {unit}"))
                                .small()
                                .monospace()
                                .color(TEXT_MUTED),
                        );
                    },
                );
                ui.add_space(3.0);
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4))
                        .fill(fill),
                );
            }
            (Some(val), _) => {
                ui.label(
                    RichText::new(format!("{val:.1} {unit}"))
                        .size(24.0)
                        .strong()
                        .monospace()
                        .color(TEXT_BRIGHT),
                );
                ui.add_space(4.0);
                ui.label(RichText::new("Limit N/A").small().color(TEXT_MUTED));
                ui.add_space(3.0);
                ui.add(
                    egui::ProgressBar::new(0.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4)),
                );
            }
            _ => {
                ui.label(
                    RichText::new("—")
                        .size(24.0)
                        .strong()
                        .monospace()
                        .color(TEXT_MUTED),
                );
                ui.add_space(4.0);
                ui.label(RichText::new("No data").small().color(TEXT_MUTED));
                ui.add_space(3.0);
                ui.add(
                    egui::ProgressBar::new(0.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4)),
                );
            }
        }
    });
}

fn render_apu_limit_row(
    ui: &mut Ui,
    label: &str,
    value: Option<f32>,
    limit: Option<f32>,
    unit: &str,
    accent: Color32,
) {
    ui.label(RichText::new(label).strong().color(TEXT_MUTED));
    match (value, limit) {
        (Some(val), Some(lim)) if lim > 0.0 => {
            let frac = (val / lim).clamp(0.0, 1.0);
            let fill = utilization_color(frac, accent);
            ui.label(
                RichText::new(format!("{val:.1} / {lim:.0} {unit}"))
                    .monospace()
                    .strong()
                    .color(TEXT_PRIMARY),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(frac)
                        .desired_width(165.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4))
                        .fill(fill),
                );
                ui.label(
                    RichText::new(format!("{:.0}%", frac * 100.0))
                        .small()
                        .strong()
                        .monospace()
                        .color(fill),
                );
            });
        }
        (Some(val), Some(_)) => {
            ui.label(
                RichText::new(format!("{val:.1} {unit}"))
                    .monospace()
                    .color(TEXT_MUTED),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(0.0)
                        .desired_width(165.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4)),
                );
                ui.label(
                    RichText::new("Inactive (0 limit)")
                        .small()
                        .color(TEXT_MUTED),
                );
            });
        }
        (Some(val), None) => {
            ui.label(
                RichText::new(format!("{val:.1} {unit}"))
                    .monospace()
                    .color(TEXT_PRIMARY),
            );
            ui.label(RichText::new("Limit N/A").small().color(TEXT_MUTED));
        }
        (None, Some(lim)) if lim > 0.0 => {
            ui.label(
                RichText::new(format!("— / {lim:.0} {unit}"))
                    .monospace()
                    .color(TEXT_MUTED),
            );
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(0.0)
                        .desired_width(165.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4)),
                );
                ui.label(
                    RichText::new(format!("Limit {lim:.0} {unit}"))
                        .small()
                        .monospace()
                        .color(TEXT_MUTED),
                );
            });
        }
        _ => {
            ui.label(RichText::new("—").monospace().color(TEXT_MUTED));
            ui.horizontal(|ui| {
                ui.add(
                    egui::ProgressBar::new(0.0)
                        .desired_width(165.0)
                        .desired_height(8.0)
                        .corner_radius(egui::CornerRadius::same(4)),
                );
                ui.label(RichText::new("No data").small().color(TEXT_MUTED));
            });
        }
    }
    ui.end_row();
}

fn metric_row(ui: &mut Ui, label: &str, value: Option<f32>, unit: &str, decimals: usize) {
    ui.label(RichText::new(label).color(TEXT_MUTED));
    let formatted = match value {
        Some(v) => format!("{v:.decimals$} {unit}"),
        None => "—".into(),
    };
    ui.label(RichText::new(formatted).monospace().strong());
    ui.end_row();
}

/// Tab 2: Curve Optimizer (All-Core, Per-Core grouped by CCD, and iGPU CO).
pub fn render_curve_optimizer(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    render_tab_header(
        ui,
        "Curve Optimizer",
        "Adjust voltage-frequency curve margins safely. Every write runs --dry-run first and verifies readback.",
    );

    let Some(hw) = app.hw.clone() else {
        render_no_hardware_placeholder(ui);
        return;
    };

    let Some(bounds) = hw.capabilities.co_bounds else {
        render_unsupported_feature_card(
            ui,
            "Curve Optimizer",
            hw.codename.display_name(),
            "Zen 2 and unsupported platforms do not expose Curve Optimizer SMU opcodes.",
        );
        return;
    };

    let busy = app.runner.is_busy();

    // Safety bar + All-core card
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "co_all_core_header",
            |ui| {
                ui.label(RichText::new("All-Core Curve Optimizer").strong());
                tinted_pill(
                    ui,
                    &format!(
                        "Bounds {}..=+{} · Soft floor {}",
                        bounds.min, bounds.max, bounds.soft_min
                    ),
                    ACCENT_CYAN,
                    PILL_ALPHA_CYAN,
                );
            },
            |ui| {
                if hw.capabilities.get_co_per_core.is_some()
                    && ui
                        .add_enabled(!busy, egui::Button::new("Read Current CO (pkexec)"))
                        .clicked()
                {
                    app.dispatch_cli(CliAction::CoGet, ctx);
                }
            },
        );

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label("All-Core Margin:");
            ui.add(
                egui::Slider::new(&mut app.co_editor.all_core, bounds.min..=bounds.max)
                    .show_value(true),
            );

            if ui.button("Copy to All Per-Core Sliders").clicked() {
                app.co_editor.per_core.fill(app.co_editor.all_core);
            }
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            ui.checkbox(
                &mut app.co_editor.allow_extreme,
                format!(
                    "Allow Extreme (< {} soft floor — requires stress testing)",
                    bounds.soft_min
                ),
            );
        });

        if app.co_editor.all_core < bounds.soft_min && !app.co_editor.allow_extreme {
            render_extreme_warning(
                ui,
                &format!(
                    "Value {} is below conservative soft floor ({}); enable 'Allow Extreme' to proceed.",
                    app.co_editor.all_core, bounds.soft_min
                ),
            );
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let can_all = hw.capabilities.set_co_all_cores.is_some()
                && !busy
                && (app.co_editor.all_core >= bounds.soft_min || app.co_editor.allow_extreme);
            if ui
                .add_enabled(
                    can_all,
                    cta_button("Preview (Dry-Run) & Apply All-Core", CTA_CRIMSON),
                )
                .clicked()
            {
                app.dispatch_cli(
                    CliAction::CoSetAll {
                        margin: app.co_editor.all_core,
                        allow_extreme: app.co_editor.allow_extreme,
                        dry_run: true,
                    },
                    ctx,
                );
            }

            if ui
                .add_enabled(
                    (hw.capabilities.set_co_all_cores.is_some()
                        || hw.capabilities.set_co_per_core.is_some())
                        && !busy,
                    egui::Button::new("Reset CO to 0"),
                )
                .clicked()
            {
                app.dispatch_cli(CliAction::CoReset { dry_run: true }, ctx);
            }
        });
    });

    ui.add_space(12.0);

    // Per-Core sliders grouped by CCD
    if hw.capabilities.set_co_per_core.is_some() {
        let mut by_ccd: BTreeMap<u32, Vec<&CoreInfo>> = BTreeMap::new();
        for core in &hw.topology.cores {
            by_ccd.entry(core.ccd).or_default().push(core);
        }

        if app.co_editor.per_core.len() < hw.topology.physical_cores {
            app.co_editor.per_core.resize(hw.topology.physical_cores, 0);
        }

        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            split_or_wrap_row(
                ui,
                "co_per_core_header",
                |ui| {
                    ui.label(RichText::new("Per-Core Curve Optimizer (by CCD)").strong());
                },
                |ui| {
                    let any_below_soft =
                        app.co_editor.per_core.iter().any(|&v| v < bounds.soft_min);
                    let can_apply = !busy && (!any_below_soft || app.co_editor.allow_extreme);
                    if ui
                        .add_enabled(
                            can_apply,
                            cta_button("Preview (Dry-Run) & Apply Per-Core", CTA_CRIMSON),
                        )
                        .clicked()
                    {
                        let updates: Vec<(u32, i32)> = app
                            .co_editor
                            .per_core
                            .iter()
                            .enumerate()
                            .filter(|(idx, &val)| {
                                // Send only modified cores when we have a live snapshot; otherwise all cores.
                                if let Some(snap) = &app.co_snapshot {
                                    snap.cores
                                        .iter()
                                        .find(|c| c.core_index == *idx as u32)
                                        .map(|c| i32::from(c.margin) != val)
                                        .unwrap_or(true)
                                } else {
                                    true
                                }
                            })
                            .map(|(idx, &val)| (idx as u32, val))
                            .collect();

                        if updates.is_empty() {
                            app.set_toast(
                                ToastLevel::Info,
                                "All per-core margins already match hardware state.",
                            );
                        } else {
                            app.dispatch_cli(
                                CliAction::CoSetCores {
                                    updates,
                                    allow_extreme: app.co_editor.allow_extreme,
                                    dry_run: true,
                                },
                                ctx,
                            );
                        }
                    }
                },
            );

            ui.add_space(8.0);
            for (ccd, cores) in by_ccd {
                ui.label(
                    RichText::new(format!("CCD {ccd} ({} physical cores)", cores.len()))
                        .small()
                        .strong()
                        .color(ACCENT_CYAN),
                );
                ui.add_space(4.0);

                egui::Grid::new(format!("co_ccd_{ccd}_grid"))
                    .num_columns(4)
                    .striped(true)
                    .spacing([12.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Physical Core").small().color(TEXT_MUTED));
                        ui.label(RichText::new("Linux CPUs").small().color(TEXT_MUTED));
                        ui.label(RichText::new("Current").small().color(TEXT_MUTED));
                        ui.label(RichText::new("Target Margin").small().color(TEXT_MUTED));
                        ui.end_row();

                        for core in cores {
                            let idx = core.index as usize;
                            let cpus = core
                                .linux_cpus
                                .iter()
                                .map(|c| format!("CPU {c}"))
                                .collect::<Vec<_>>()
                                .join(", ");

                            let current_str = app
                                .co_snapshot
                                .as_ref()
                                .and_then(|s| s.cores.iter().find(|c| c.core_index == core.index))
                                .map(|c| format!("{:+}", c.margin))
                                .unwrap_or_else(|| "—".into());

                            ui.label(
                                RichText::new(format!(
                                    "Core {} (local {})",
                                    core.index, core.core_in_ccd
                                ))
                                .monospace(),
                            );
                            ui.label(RichText::new(cpus).small().color(TEXT_MUTED));
                            ui.label(RichText::new(current_str).monospace().color(ACCENT_EMERALD));

                            if let Some(val) = app.co_editor.per_core.get_mut(idx) {
                                ui.horizontal(|ui| {
                                    ui.add(egui::Slider::new(val, bounds.min..=bounds.max));
                                    if *val < bounds.soft_min {
                                        ui.label(
                                            RichText::new("extreme").small().color(ACCENT_AMBER),
                                        );
                                    }
                                });
                            }
                            ui.end_row();
                        }
                    });
                ui.add_space(8.0);
            }
        });
    }

    // iGPU CO Card
    if hw.capabilities.set_igpu_co.is_some() {
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("iGPU Curve Optimizer").strong());
                if let Some(cur) = app.co_snapshot.as_ref().and_then(|s| s.igpu) {
                    tinted_pill(
                        ui,
                        &format!("Current: {cur:+}"),
                        ACCENT_EMERALD,
                        PILL_ALPHA_EMERALD,
                    );
                }
            });
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("iGPU Margin:");
                ui.add(egui::Slider::new(
                    &mut app.co_editor.igpu,
                    IGPU_CO_MIN..=IGPU_CO_MAX,
                ));
                let can_igpu = !busy
                    && (app.co_editor.igpu >= IGPU_CO_SOFT_MIN || app.co_editor.allow_extreme);
                if ui
                    .add_enabled(can_igpu, egui::Button::new("Preview & Apply iGPU CO"))
                    .clicked()
                {
                    app.dispatch_cli(
                        CliAction::CoSetIgpu {
                            margin: app.co_editor.igpu,
                            allow_extreme: app.co_editor.allow_extreme,
                            dry_run: true,
                        },
                        ctx,
                    );
                }
            });
        });
    }
}

/// Tab 3: Zen 5 Curve Shaper (5 frequency tiers × 3 temperature bands).
pub fn render_curve_shaper(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    render_tab_header(
        ui,
        "Curve Shaper (Zen 5)",
        "15-point frequency × temperature voltage curve shaping (-50..=+30, soft floor -35).",
    );

    let Some(hw) = app.hw.clone() else {
        render_no_hardware_placeholder(ui);
        return;
    };

    if hw.capabilities.set_curve_shaper.is_none() {
        render_unsupported_feature_card(
            ui,
            "Curve Shaper",
            hw.codename.display_name(),
            "Curve Shaper (5 frequency tiers × 3 temperature bands) requires a Zen 5 processor such as Granite Ridge (Ryzen 9000 series). zensmu disables these controls on non-Zen-5 CPUs so invalid SMU opcodes are never emitted.",
        );
        return;
    }

    let busy = app.runner.is_busy();

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "shaper_matrix_header",
            |ui| {
                ui.label(RichText::new("5×3 Frequency / Temperature Matrix").strong());
                tinted_pill(
                    ui,
                    &format!("Range {SHAPER_MIN}..=+{SHAPER_MAX} · Soft floor {SHAPER_SOFT_MIN}"),
                    ACCENT_CYAN,
                    PILL_ALPHA_CYAN,
                );
            },
            |ui| {
                if hw.capabilities.get_curve_shaper.is_some()
                    && ui
                        .add_enabled(!busy, egui::Button::new("Read Current Matrix (pkexec)"))
                        .clicked()
                {
                    app.dispatch_cli(CliAction::ShaperGet, ctx);
                }
            },
        );

        ui.add_space(10.0);

        let mut single_tier_preview: Option<FreqTier> = None;

        egui::Grid::new("shaper_matrix_grid")
            .num_columns(5)
            .striped(true)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Freq Tier").strong().color(TEXT_MUTED));
                ui.label(RichText::new("Low Temp").strong().color(ACCENT_CYAN));
                ui.label(RichText::new("Med Temp").strong().color(ACCENT_EMERALD));
                ui.label(RichText::new("High Temp").strong().color(ACCENT_AMBER));
                ui.label(RichText::new("Row Action").strong().color(TEXT_MUTED));
                ui.end_row();

                for tier in FreqTier::ALL {
                    let cur = app.shaper_state.map(|s| s.tier(tier));
                    let margins = app.shaper_editor.draft.tier_mut(tier);

                    ui.label(
                        RichText::new(tier.label().to_uppercase())
                            .strong()
                            .monospace(),
                    );

                    shaper_cell(ui, &mut margins.low, cur.map(|c| c.low));
                    shaper_cell(ui, &mut margins.med, cur.map(|c| c.med));
                    shaper_cell(ui, &mut margins.high, cur.map(|c| c.high));

                    let row_extreme = margins.low < SHAPER_SOFT_MIN
                        || margins.med < SHAPER_SOFT_MIN
                        || margins.high < SHAPER_SOFT_MIN;
                    if ui
                        .add_enabled(
                            !busy && (!row_extreme || app.shaper_editor.allow_extreme),
                            egui::Button::new("Preview Row"),
                        )
                        .clicked()
                    {
                        single_tier_preview = Some(tier);
                    }
                    ui.end_row();
                }
            });

        if let Some(tier) = single_tier_preview {
            let margins = app.shaper_editor.draft.tier(tier);
            app.dispatch_cli(
                CliAction::ShaperSetTier {
                    tier,
                    margins,
                    allow_extreme: app.shaper_editor.allow_extreme,
                    dry_run: true,
                },
                ctx,
            );
        }

        ui.add_space(10.0);
        ui.checkbox(
            &mut app.shaper_editor.allow_extreme,
            format!("Allow Extreme (< {SHAPER_SOFT_MIN} soft floor)"),
        );

        let any_extreme = FreqTier::ALL.iter().any(|&t| {
            let m = app.shaper_editor.draft.tier(t);
            m.low < SHAPER_SOFT_MIN || m.med < SHAPER_SOFT_MIN || m.high < SHAPER_SOFT_MIN
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !busy && (!any_extreme || app.shaper_editor.allow_extreme),
                    cta_button("Preview (Dry-Run) & Apply Matrix", CTA_CRIMSON),
                )
                .clicked()
            {
                app.dispatch_cli(
                    CliAction::ShaperSetAll {
                        state: app.shaper_editor.draft,
                        allow_extreme: app.shaper_editor.allow_extreme,
                        dry_run: true,
                    },
                    ctx,
                );
            }

            if ui
                .add_enabled(!busy, egui::Button::new("Reset Shaper to 0"))
                .clicked()
            {
                app.dispatch_cli(CliAction::ShaperReset { dry_run: true }, ctx);
            }
        });
    });
}

fn shaper_cell(ui: &mut Ui, val: &mut i8, current: Option<i8>) {
    let tint = if *val < SHAPER_SOFT_MIN {
        ACCENT_AMBER
    } else if *val < 0 {
        ACCENT_CYAN
    } else if *val > 0 {
        ACCENT_CRIMSON
    } else {
        TEXT_PRIMARY
    };

    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(val)
                .range(SHAPER_MIN..=SHAPER_MAX)
                .speed(0.5),
        );
        ui.label(
            RichText::new(format!("{:+}", *val))
                .small()
                .monospace()
                .color(tint),
        );
        if let Some(cur) = current {
            if cur != *val {
                ui.label(
                    RichText::new(format!("(hw {cur:+})"))
                        .small()
                        .color(TEXT_MUTED),
                );
            }
        }
    });
}

/// Tab 4: PBO Power/Thermal Limits, Scalar, Fmax Boost Limit, and Fused Factory Limits.
pub fn render_pbo_limits(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    render_tab_header(
        ui,
        "Precision Boost Overdrive (PBO) Limits",
        "Configure package power, VRM current, thermal ceiling (hard max 95°C), scalar, and Fmax boost limit.",
    );

    let Some(hw) = app.hw.clone() else {
        render_no_hardware_placeholder(ui);
        return;
    };

    let busy = app.runner.is_busy();
    let fused = app.pbo_status.as_ref().map(|s| s.fused.clone());
    let telem = app.latest_telemetry.clone();
    let caps = &hw.capabilities;

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "pbo_controls_header",
            |ui| {
                ui.label(RichText::new("PBO Controls").strong());
            },
            |ui| {
                if ui
                    .button("Sync from Live")
                    .on_hover_text("Reset edited PBO sliders to live telemetry and mailbox values")
                    .clicked()
                {
                    app.pbo_editor.clear_dirty();
                    if let Some(snap) = app.latest_telemetry.clone() {
                        app.pbo_editor.sync_from_telemetry(&snap);
                    }
                    if let Some(status) = app.pbo_status.clone() {
                        app.pbo_editor.sync_from_status(&status, true);
                    }
                }
                if ui
                    .add_enabled(!busy, egui::Button::new("Read PBO Status & Fused Limits (pkexec)"))
                    .clicked()
                {
                    app.dispatch_cli(CliAction::PboGet, ctx);
                }
            },
        );

        ui.add_space(10.0);

        egui::Grid::new("pbo_controls_grid")
            .num_columns(4)
            .striped(true)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                grid_header_row(
                    ui,
                    &[
                        "Parameter",
                        "Target Value",
                        "Current Live",
                        "Fused Factory / Guard",
                    ],
                );

                // PPT
                let ppt_supported = caps.set_ppt.is_some();
                let ppt_live = telem.as_ref().and_then(|t| t.ppt_limit_w);
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "PPT Limit (W)",
                        supported: ppt_supported,
                        enabled: &mut app.pbo_editor.ppt_enabled,
                        value: &mut app.pbo_editor.ppt_w,
                        dirty: Some(&mut app.pbo_editor.ppt_dirty),
                        range: PPT_MIN_W..=PPT_MAX_W,
                        step_by: None,
                        suffix: " W",
                        live_display: opt_f32(ppt_live, "W"),
                        live_is_muted: false,
                        note: if ppt_supported {
                            fused_soft_note(
                                fused.as_ref().and_then(|f| f.ppt_w),
                                "Fused",
                                PPT_SOFT_MAX_W,
                                "W",
                            )
                        } else {
                            capability_disabled_hint(ppt_live.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // TDC
                let tdc_supported = caps.set_tdc.is_some();
                let tdc_live = telem.as_ref().and_then(|t| t.tdc_limit_a);
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "TDC Limit (A)",
                        supported: tdc_supported,
                        enabled: &mut app.pbo_editor.tdc_enabled,
                        value: &mut app.pbo_editor.tdc_a,
                        dirty: Some(&mut app.pbo_editor.tdc_dirty),
                        range: TDC_MIN_A..=TDC_MAX_A,
                        step_by: None,
                        suffix: " A",
                        live_display: opt_f32(tdc_live, "A"),
                        live_is_muted: false,
                        note: if tdc_supported {
                            fused_soft_note(
                                fused.as_ref().and_then(|f| f.vdd_tdc_a),
                                "Fused VDD",
                                TDC_SOFT_MAX_A,
                                "A",
                            )
                        } else {
                            capability_disabled_hint(tdc_live.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // EDC
                let edc_supported = caps.set_edc.is_some();
                let edc_live = telem.as_ref().and_then(|t| t.edc_limit_a);
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "EDC Limit (A)",
                        supported: edc_supported,
                        enabled: &mut app.pbo_editor.edc_enabled,
                        value: &mut app.pbo_editor.edc_a,
                        dirty: Some(&mut app.pbo_editor.edc_dirty),
                        range: EDC_MIN_A..=EDC_MAX_A,
                        step_by: None,
                        suffix: " A",
                        live_display: opt_f32(edc_live, "A"),
                        live_is_muted: false,
                        note: if edc_supported {
                            fused_soft_note(
                                fused.as_ref().and_then(|f| f.edc_a),
                                "Fused EDC",
                                EDC_SOFT_MAX_A,
                                "A",
                            )
                        } else {
                            capability_disabled_hint(edc_live.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // Tctl Max (Hard ceiling 95C)
                let tctl_supported = caps.set_tctl.is_some();
                let tctl_live = telem.as_ref().and_then(|t| t.tctl_limit_c);
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "Tctl Thermal Max (°C)",
                        supported: tctl_supported,
                        enabled: &mut app.pbo_editor.tctl_enabled,
                        value: &mut app.pbo_editor.tctl_c,
                        dirty: Some(&mut app.pbo_editor.tctl_dirty),
                        range: TCTL_MIN_C..=TCTL_MAX_C,
                        step_by: None,
                        suffix: " °C",
                        live_display: opt_f32(tctl_live, "°C"),
                        live_is_muted: false,
                        note: if tctl_supported {
                            "Hard safety ceiling: 95 °C (non-overridable)".into()
                        } else {
                            capability_disabled_hint(tctl_live.is_some()).into()
                        },
                        note_color: if tctl_supported {
                            ACCENT_AMBER
                        } else {
                            TEXT_MUTED
                        },
                    },
                );

                // PBO Scalar
                let scalar_supported = caps.set_pbo_scalar.is_some();
                let scalar_live = app
                    .pbo_status
                    .as_ref()
                    .and_then(|s| s.scalar)
                    .map(|s| format!("{s:.1}x"))
                    .unwrap_or_else(|| "—".into());
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "PBO Scalar (1–10x)",
                        supported: scalar_supported,
                        enabled: &mut app.pbo_editor.scalar_enabled,
                        value: &mut app.pbo_editor.scalar,
                        dirty: Some(&mut app.pbo_editor.scalar_dirty),
                        range: SCALAR_MIN..=SCALAR_MAX,
                        step_by: None,
                        suffix: "x",
                        live_display: scalar_live,
                        live_is_muted: false,
                        note: if scalar_supported {
                            format!("Soft max ≤ {SCALAR_SOFT_MAX}x")
                        } else {
                            capability_disabled_hint(caps.get_pbo_scalar.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // Fmax Boost Limit
                let fmax_supported = caps.set_fmax.is_some();
                let fmax_live = app
                    .pbo_status
                    .as_ref()
                    .and_then(|s| s.fmax_mhz)
                    .map(|m| format!("{m} MHz"))
                    .unwrap_or_else(|| "—".into());
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "Fmax Boost Limit (MHz)",
                        supported: fmax_supported,
                        enabled: &mut app.pbo_editor.fmax_enabled,
                        value: &mut app.pbo_editor.fmax_mhz,
                        dirty: Some(&mut app.pbo_editor.fmax_dirty),
                        range: FMAX_MIN_MHZ..=FMAX_MAX_MHZ,
                        step_by: Some(25.0),
                        suffix: " MHz",
                        live_display: fmax_live,
                        live_is_muted: false,
                        note: if fmax_supported {
                            "Soft delta ≤ +200 MHz above current".into()
                        } else {
                            capability_disabled_hint(caps.get_fmax.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );
            });

        ui.add_space(10.0);
        ui.checkbox(
            &mut app.pbo_editor.allow_extreme,
            "Allow Extreme PBO values (above conservative soft limits; 95°C Tctl ceiling still enforced)",
        );

        if app.pbo_editor.has_any_extreme_for(caps) && !app.pbo_editor.allow_extreme {
            render_extreme_warning(
                ui,
                "One or more enabled PBO values exceed conservative soft limits; enable 'Allow Extreme' to override.",
            );
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let can_apply = !busy && app.pbo_editor.has_any_enabled_for(caps);
            if ui
                .add_enabled(
                    can_apply,
                    cta_button("Preview (Dry-Run) & Apply PBO", CTA_CRIMSON),
                )
                .clicked()
            {
                app.dispatch_cli(
                    CliAction::PboSet {
                        req: app.pbo_editor.build_request_for(caps),
                        allow_extreme: app.pbo_editor.allow_extreme,
                        dry_run: true,
                    },
                    ctx,
                );
            }

            if ui
                .add_enabled(!busy, egui::Button::new("Reset PBO"))
                .on_hover_text("Preview restoring fused factory / boot baseline PBO limits")
                .clicked()
            {
                app.dispatch_cli(CliAction::PboReset { dry_run: true }, ctx);
            }
        });
    });

    if hw.codename.is_apu() || caps.set_stapm_limit.is_some() {
        ui.add_space(12.0);
        render_apu_dptc_card(app, ui, ctx, &hw, fused.as_ref(), busy);
    }

    if let Some(status) = &app.pbo_status {
        ui.add_space(12.0);
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Platform Flags & Fused Factory Telemetry").strong());
            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                flag_pill(ui, "Overclockable", status.flags.overclockable);
                flag_pill(ui, "EXPO Active", status.flags.expo_active);
                flag_pill(ui, "LN2 Mode", status.flags.ln2_mode);
                optional_fused_pill(ui, "Fused PPT", status.fused.ppt_w, "W");
                optional_fused_pill(ui, "Fused Fast", status.fused.fast_w, "W");
                optional_fused_pill(ui, "Fused Slow", status.fused.slow_w, "W");
                optional_fused_pill(ui, "Fused SoC TDC", status.fused.soc_tdc_a, "A");
                optional_fused_pill(ui, "Fused APU Slow", status.fused.apu_slow_w, "W");
            });
        });
    }
}

fn render_apu_dptc_card(
    app: &mut ZensmuApp,
    ui: &mut Ui,
    ctx: &egui::Context,
    hw: &HardwareContext,
    fused: Option<&FusedLimits>,
    busy: bool,
) {
    let caps = &hw.capabilities;
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "apu_dptc_header",
            |ui| {
                ui.label(RichText::new("APU DPTC Limits").strong());
                tinted_pill(
                    ui,
                    &format!("{} · Mobile Power & Thermal", hw.codename.display_name()),
                    ACCENT_CYAN,
                    PILL_ALPHA_CYAN,
                );
            },
            |ui| {
                if ui
                    .button("Sync from Live")
                    .on_hover_text("Reset edited APU sliders to live PM table hardware limits")
                    .clicked()
                {
                    app.apu_editor.clear_dirty();
                    if let Some(snap) = app.latest_telemetry.clone() {
                        app.apu_editor.sync_from_telemetry(&snap);
                    }
                }
            },
        );

        ui.add_space(10.0);

        egui::Grid::new("apu_dptc_controls_grid")
            .num_columns(4)
            .striped(true)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                grid_header_row(
                    ui,
                    &["Parameter", "Target Value", "Current Live", "Fused / Guard"],
                );

                // STAPM Limit (W)
                let stapm_supported = caps.set_stapm_limit.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "STAPM Limit (W)",
                        supported: stapm_supported,
                        enabled: &mut app.apu_editor.stapm_enabled,
                        value: &mut app.apu_editor.stapm_w,
                        dirty: Some(&mut app.apu_editor.stapm_dirty),
                        range: APU_POWER_MIN_W..=APU_POWER_MAX_W,
                        step_by: None,
                        suffix: " W",
                        live_display: opt_f32(app.apu_editor.live_stapm_w, "W"),
                        live_is_muted: false,
                        note: if stapm_supported {
                            format!("Soft max ≤ {APU_POWER_SOFT_MAX_W} W")
                        } else {
                            capability_disabled_hint(app.apu_editor.live_stapm_w.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // Fast PPT (W)
                let fast_supported = hw.codename.is_apu() && caps.set_ppt.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "Fast PPT (W)",
                        supported: fast_supported,
                        enabled: &mut app.apu_editor.fast_enabled,
                        value: &mut app.apu_editor.fast_w,
                        dirty: Some(&mut app.apu_editor.fast_dirty),
                        range: APU_POWER_MIN_W..=APU_POWER_MAX_W,
                        step_by: None,
                        suffix: " W",
                        live_display: opt_f32(app.apu_editor.live_fast_w, "W"),
                        live_is_muted: false,
                        note: if fast_supported {
                            fused_soft_note(
                                fused.and_then(|f| f.fast_w),
                                "Fused",
                                APU_FAST_POWER_SOFT_MAX_W,
                                "W",
                            )
                        } else {
                            capability_disabled_hint(app.apu_editor.live_fast_w.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // Slow PPT (W)
                let slow_supported = caps.set_slow_limit.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "Slow PPT (W)",
                        supported: slow_supported,
                        enabled: &mut app.apu_editor.slow_enabled,
                        value: &mut app.apu_editor.slow_w,
                        dirty: Some(&mut app.apu_editor.slow_dirty),
                        range: APU_POWER_MIN_W..=APU_POWER_MAX_W,
                        step_by: None,
                        suffix: " W",
                        live_display: opt_f32(app.apu_editor.live_slow_w, "W"),
                        live_is_muted: false,
                        note: if slow_supported {
                            fused_soft_note(
                                fused.and_then(|f| f.slow_w),
                                "Fused",
                                APU_POWER_SOFT_MAX_W,
                                "W",
                            )
                        } else {
                            capability_disabled_hint(app.apu_editor.live_slow_w.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // APU Slow (W)
                let apu_slow_supported = caps.set_apu_slow_limit.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "APU Slow (W)",
                        supported: apu_slow_supported,
                        enabled: &mut app.apu_editor.apu_slow_enabled,
                        value: &mut app.apu_editor.apu_slow_w,
                        dirty: Some(&mut app.apu_editor.apu_slow_dirty),
                        range: APU_POWER_MIN_W..=APU_POWER_MAX_W,
                        step_by: None,
                        suffix: " W",
                        live_display: opt_f32(app.apu_editor.live_apu_slow_w, "W"),
                        live_is_muted: false,
                        note: if apu_slow_supported {
                            fused_soft_note(
                                fused.and_then(|f| f.apu_slow_w),
                                "Fused",
                                APU_POWER_SOFT_MAX_W,
                                "W",
                            )
                        } else {
                            capability_disabled_hint(app.apu_editor.live_apu_slow_w.is_some())
                                .into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // STAPM Time Constant (s)
                let stapm_time_supported = caps.set_stapm_time.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "STAPM Time Constant (s)",
                        supported: stapm_time_supported,
                        enabled: &mut app.apu_editor.stapm_time_enabled,
                        value: &mut app.apu_editor.stapm_time_s,
                        dirty: None,
                        range: APU_TIME_MIN_S..=APU_TIME_MAX_S,
                        step_by: None,
                        suffix: " s",
                        live_display: "—".into(),
                        live_is_muted: true,
                        note: if stapm_time_supported {
                            format!("Range {APU_TIME_MIN_S}–{APU_TIME_MAX_S} s")
                        } else {
                            capability_disabled_hint(false).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // Slow Time Constant (s)
                let slow_time_supported = caps.set_slow_time.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "Slow Time Constant (s)",
                        supported: slow_time_supported,
                        enabled: &mut app.apu_editor.slow_time_enabled,
                        value: &mut app.apu_editor.slow_time_s,
                        dirty: None,
                        range: APU_TIME_MIN_S..=APU_TIME_MAX_S,
                        step_by: None,
                        suffix: " s",
                        live_display: "—".into(),
                        live_is_muted: true,
                        note: if slow_time_supported {
                            format!("Range {APU_TIME_MIN_S}–{APU_TIME_MAX_S} s")
                        } else {
                            capability_disabled_hint(false).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // SoC TDC Limit (A)
                let tdc_soc_supported = caps.set_tdc_soc.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "SoC TDC Limit (A)",
                        supported: tdc_soc_supported,
                        enabled: &mut app.apu_editor.tdc_soc_enabled,
                        value: &mut app.apu_editor.tdc_soc_a,
                        dirty: Some(&mut app.apu_editor.tdc_soc_dirty),
                        range: APU_SOC_TDC_MIN_A..=APU_SOC_TDC_MAX_A,
                        step_by: None,
                        suffix: " A",
                        live_display: opt_f32(app.apu_editor.live_tdc_soc_a, "A"),
                        live_is_muted: false,
                        note: if tdc_soc_supported {
                            fused_soft_note(
                                fused.and_then(|f| f.soc_tdc_a),
                                "Fused SoC",
                                APU_SOC_TDC_SOFT_MAX_A,
                                "A",
                            )
                        } else {
                            capability_disabled_hint(app.apu_editor.live_tdc_soc_a.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // SoC EDC Limit (A)
                let edc_soc_supported = caps.set_edc_soc.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "SoC EDC Limit (A)",
                        supported: edc_soc_supported,
                        enabled: &mut app.apu_editor.edc_soc_enabled,
                        value: &mut app.apu_editor.edc_soc_a,
                        dirty: Some(&mut app.apu_editor.edc_soc_dirty),
                        range: APU_SOC_EDC_MIN_A..=APU_SOC_EDC_MAX_A,
                        step_by: None,
                        suffix: " A",
                        live_display: opt_f32(app.apu_editor.live_edc_soc_a, "A"),
                        live_is_muted: false,
                        note: if edc_soc_supported {
                            format!("Soft max ≤ {APU_SOC_EDC_SOFT_MAX_A} A")
                        } else {
                            capability_disabled_hint(app.apu_editor.live_edc_soc_a.is_some()).into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // APU Skin Temp (°C)
                let apu_skin_supported = caps.set_apu_skin_temp.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "APU Skin Temp (°C)",
                        supported: apu_skin_supported,
                        enabled: &mut app.apu_editor.apu_skin_temp_enabled,
                        value: &mut app.apu_editor.apu_skin_temp_c,
                        dirty: Some(&mut app.apu_editor.apu_skin_temp_dirty),
                        range: APU_SKIN_TEMP_MIN_C..=APU_SKIN_TEMP_MAX_C,
                        step_by: None,
                        suffix: " °C",
                        live_display: opt_f32(app.apu_editor.live_apu_skin_temp_c, "°C"),
                        live_is_muted: false,
                        note: if apu_skin_supported {
                            format!("Soft max ≤ {APU_SKIN_TEMP_SOFT_MAX_C} °C")
                        } else {
                            capability_disabled_hint(app.apu_editor.live_apu_skin_temp_c.is_some())
                                .into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // dGPU Skin Temp (°C)
                let dgpu_skin_supported = caps.set_dgpu_skin_temp.is_some();
                render_limit_slider_row(
                    ui,
                    LimitSliderRow {
                        label: "dGPU Skin Temp (°C)",
                        supported: dgpu_skin_supported,
                        enabled: &mut app.apu_editor.dgpu_skin_temp_enabled,
                        value: &mut app.apu_editor.dgpu_skin_temp_c,
                        dirty: Some(&mut app.apu_editor.dgpu_skin_temp_dirty),
                        range: APU_SKIN_TEMP_MIN_C..=APU_SKIN_TEMP_MAX_C,
                        step_by: None,
                        suffix: " °C",
                        live_display: opt_f32(app.apu_editor.live_dgpu_skin_temp_c, "°C"),
                        live_is_muted: false,
                        note: if dgpu_skin_supported {
                            format!("Soft max ≤ {APU_SKIN_TEMP_SOFT_MAX_C} °C")
                        } else {
                            capability_disabled_hint(
                                app.apu_editor.live_dgpu_skin_temp_c.is_some(),
                            )
                            .into()
                        },
                        note_color: TEXT_MUTED,
                    },
                );

                // Power Mode selector
                {
                    let has_perf = caps.set_max_performance.is_some();
                    let has_save = caps.set_power_saving.is_some();
                    let supported = has_perf || has_save;
                    if !supported {
                        app.apu_editor.power_mode = None;
                    }
                    ui.label(if supported {
                        RichText::new("Power Mode")
                    } else {
                        RichText::new("Power Mode").color(TEXT_MUTED)
                    });
                    ui.add_enabled_ui(supported, |ui| {
                        let selected_label = match app.apu_editor.power_mode {
                            None => "Keep Current",
                            Some(ApuPowerMode::MaxPerformance) => "Max Performance",
                            Some(ApuPowerMode::PowerSaving) => "Power Saving",
                        };
                        egui::ComboBox::from_id_salt("apu_power_mode_combo")
                            .width(170.0)
                            .selected_text(selected_label)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(
                                    &mut app.apu_editor.power_mode,
                                    None,
                                    "Keep Current",
                                );
                                if has_perf {
                                    ui.selectable_value(
                                        &mut app.apu_editor.power_mode,
                                        Some(ApuPowerMode::MaxPerformance),
                                        "Max Performance",
                                    );
                                }
                                if has_save {
                                    ui.selectable_value(
                                        &mut app.apu_editor.power_mode,
                                        Some(ApuPowerMode::PowerSaving),
                                        "Power Saving",
                                    );
                                }
                            });
                    });
                    ui.label(RichText::new("—").monospace().color(TEXT_MUTED));
                    let note = if supported {
                        "SMU performance / efficiency bias"
                    } else {
                        capability_disabled_hint(false)
                    };
                    ui.label(RichText::new(note).small().color(TEXT_MUTED));
                    ui.end_row();
                }
            });

        ui.add_space(10.0);
        ui.checkbox(
            &mut app.apu_editor.allow_extreme,
            "Allow Extreme APU limits (above conservative soft power, current, and skin temp limits)",
        );

        let any_extreme = app.apu_editor.has_any_extreme_for(hw);
        if any_extreme && !app.apu_editor.allow_extreme {
            render_extreme_warning(
                ui,
                "One or more enabled APU values exceed conservative soft limits; enable 'Allow Extreme' to proceed.",
            );
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let can_apply = !busy
                && app.apu_editor.has_any_enabled_for(hw)
                && (!any_extreme || app.apu_editor.allow_extreme);
            if ui
                .add_enabled(
                    can_apply,
                    cta_button("Preview (Dry-Run) & Apply APU Limits", CTA_CRIMSON),
                )
                .clicked()
            {
                app.preview_apply_apu(ctx);
            }
        });
    });
}

fn opt_f32(v: Option<f32>, unit: &str) -> String {
    match v {
        Some(x) => format!("{x:.0} {unit}"),
        None => "—".into(),
    }
}

fn flag_pill(ui: &mut Ui, label: &str, val: Option<bool>) {
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

/// Tab 5: Declarative TOML Profiles, Rollback/Reset, Boot-Loop Guard, and Driver Safety Status.
pub fn render_profiles_and_safety(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    render_tab_header(
        ui,
        "Profiles & Safety",
        "Load, validate, diff, export, and apply declarative TOML profiles, or manage rollback and driver safety guards.",
    );

    let busy = app.runner.is_busy();
    let has_hw = app.hw.is_some();

    // Boot & Resume Persistence (systemd) Card
    render_persistence_card(app, ui, ctx, has_hw, busy);
    ui.add_space(12.0);

    // TOML Profile Card
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Declarative TOML Profile").strong());
        ui.add_space(6.0);

        ui.horizontal_wrapped(|ui| {
            ui.label("Path:");
            ui.add(
                egui::TextEdit::singleline(&mut app.profile_editor.path)
                    .desired_width(400.0)
                    .hint_text("/etc/zensmu/active.toml"),
            );
            if ui.button("Load TOML").clicked() {
                app.load_profile_from_disk();
            }
            if ui
                .add_enabled(has_hw && !busy, egui::Button::new("Export Current State"))
                .clicked()
            {
                app.export_current_profile(ctx);
            }
        });

        ui.add_space(6.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(has_hw, egui::Button::new("Validate"))
                .clicked()
            {
                app.validate_profile_buffer();
            }
            if ui.add_enabled(has_hw, egui::Button::new("Diff")).clicked() {
                app.compute_profile_diff();
            }
            ui.checkbox(&mut app.profile_editor.allow_extreme, "Allow Extreme");
            if ui
                .add_enabled(
                    has_hw && !busy,
                    cta_button("Preview (Dry-Run) & Apply", CTA_CRIMSON),
                )
                .clicked()
            {
                app.preview_apply_profile(ctx);
            }
        });

        if let Some(res) = &app.profile_editor.validation {
            ui.add_space(4.0);
            match res {
                Ok(ok) => ui.label(RichText::new(format!("✓ {ok}")).color(ACCENT_EMERALD)),
                Err(err) => ui.label(RichText::new(format!("✗ {err}")).color(ACCENT_CRIMSON)),
            };
        }

        ui.add_space(6.0);
        egui::ScrollArea::vertical()
            .id_salt("toml_editor_scroll")
            .max_height(260.0)
            .show(ui, |ui| {
                ui.add(
                    egui::TextEdit::multiline(&mut app.profile_editor.toml_text)
                        .code_editor()
                        .desired_width(f32::INFINITY)
                        .desired_rows(12),
                );
            });

        if !app.profile_editor.diff_entries.is_empty() {
            ui.add_space(8.0);
            ui.label(RichText::new("Profile Diff vs Current State").strong());
            egui::Grid::new("profile_diff_grid")
                .num_columns(3)
                .striped(true)
                .spacing([14.0, 4.0])
                .show(ui, |ui| {
                    ui.label(RichText::new("Field").small().strong().color(TEXT_MUTED));
                    ui.label(RichText::new("Current").small().strong().color(TEXT_MUTED));
                    ui.label(RichText::new("Desired").small().strong().color(TEXT_MUTED));
                    ui.end_row();
                    for entry in &app.profile_editor.diff_entries {
                        ui.label(RichText::new(&entry.field).monospace());
                        ui.label(RichText::new(&entry.current).monospace().color(TEXT_MUTED));
                        ui.label(RichText::new(&entry.desired).monospace().color(ACCENT_CYAN));
                        ui.end_row();
                    }
                });
        }
    });

    ui.add_space(12.0);

    // Rollback, Reset & Driver Safety Card
    ui.columns(2, |cols| {
        card_frame().show(&mut cols[0], |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Rollback & Boot Baseline").strong());
            ui.add_space(6.0);

            ui.horizontal_wrapped(|ui| {
                ui.label("Last-change snapshot:");
                if app.profile_editor.has_last_state {
                    tinted_pill(
                        ui,
                        "Present (/run/zensmu/last-state.json)",
                        ACCENT_EMERALD,
                        PILL_ALPHA_EMERALD,
                    );
                } else {
                    muted_pill(ui, "None yet this boot");
                }
            });

            ui.horizontal_wrapped(|ui| {
                ui.label("Boot baseline snapshot:");
                if app.profile_editor.has_boot_baseline {
                    tinted_pill(
                        ui,
                        "Captured (/run/zensmu/boot-baseline.json)",
                        ACCENT_EMERALD,
                        PILL_ALPHA_EMERALD,
                    );
                } else {
                    muted_pill(ui, "Captured on first write");
                }
            });

            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        has_hw && !busy,
                        egui::Button::new("Rollback Last Change"),
                    )
                    .clicked()
                {
                    app.dispatch_cli(CliAction::Rollback { dry_run: true }, ctx);
                }
                if ui
                    .add_enabled(
                        has_hw && !busy,
                        egui::Button::new("Reset to Boot Baseline"),
                    )
                    .clicked()
                {
                    app.dispatch_cli(CliAction::Reset { dry_run: true }, ctx);
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.label("Boot-Loop Crash Guard:");
                if app.profile_editor.boot_guard_tripped {
                    tinted_pill(
                        ui,
                        "TRIPPED — auto-apply halted",
                        ACCENT_CRIMSON,
                        PILL_ALPHA_CRIMSON,
                    );
                } else {
                    tinted_pill(ui, "Armed & Clear", ACCENT_EMERALD, PILL_ALPHA_EMERALD);
                }
            });
            if let Some(info) = &app.profile_editor.boot_guard_sentinel {
                ui.label(
                    RichText::new(format!(
                        "Sentinel armed by profile '{}' (PID {}). Run `sudo zensmu profile clear-guard` after relaxing unstable settings.",
                        info.profile, info.pid
                    ))
                    .small()
                    .color(ACCENT_AMBER),
                );
            }
        });

        card_frame().show(&mut cols[1], |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Driver Safety & Mitigation Status").strong());
            ui.add_space(6.0);

            if let Some(hw) = &app.hw {
                let affected = driver_has_false_ok_bug(&hw.driver.drv_version);
                let rsmu_rsp = hw.capabilities.rsp_addrs.rsmu_rsp;
                let mp1_rsp = hw.capabilities.rsp_addrs.mp1_rsp;

                egui::Grid::new("driver_safety_grid")
                    .num_columns(2)
                    .striped(true)
                    .spacing([12.0, 5.0])
                    .show(ui, |ui| {
                        ui.label(RichText::new("Sysfs path").small().color(TEXT_MUTED));
                        ui.label(RichText::new(DEFAULT_SYSFS_PATH).small().monospace());
                        ui.end_row();

                        ui.label(RichText::new("Driver version").small().color(TEXT_MUTED));
                        ui.label(RichText::new(&hw.driver.drv_version).small().monospace());
                        ui.end_row();

                        ui.label(RichText::new("smu.c:213 false-OK").small().color(TEXT_MUTED));
                        if affected {
                            ui.label(
                                RichText::new("Affected version — MITIGATED via SMN RSP + readback")
                                    .small()
                                    .color(ACCENT_EMERALD),
                            );
                        } else {
                            ui.label(
                                RichText::new("SMN RSP verification + readback active")
                                    .small()
                                    .color(ACCENT_EMERALD),
                            );
                        }
                        ui.end_row();

                        ui.label(RichText::new("SMN RSP addrs").small().color(TEXT_MUTED));
                        ui.label(
                            RichText::new(format!(
                                "RSMU={} · MP1={}",
                                rsmu_rsp
                                    .map(|a| format!("0x{a:08X}"))
                                    .unwrap_or_else(|| "none".into()),
                                mp1_rsp
                                    .map(|a| format!("0x{a:08X}"))
                                    .unwrap_or_else(|| "none".into()),
                            ))
                            .small()
                            .monospace(),
                        );
                        ui.end_row();

                        ui.label(RichText::new("Transaction lock").small().color(TEXT_MUTED));
                        ui.label(
                            RichText::new(format!("flock ({DEFAULT_LOCK_PATH})"))
                                .small()
                                .monospace(),
                        );
                        ui.end_row();

                        ui.label(RichText::new("CPUID cross-check").small().color(TEXT_MUTED));
                        if let Some(cpuid) = &hw.cpuid {
                            ui.label(
                                RichText::new(format!(
                                    "Verified (Family 0x{:X} Model 0x{:X})",
                                    cpuid.family, cpuid.model
                                ))
                                .small()
                                .color(ACCENT_EMERALD),
                            );
                        } else {
                            ui.label(RichText::new("Unavailable").small().color(TEXT_MUTED));
                        }
                        ui.end_row();
                    });
            } else {
                ui.label(
                    RichText::new("Driver safety details will appear once ryzen_smu is loaded.")
                        .color(TEXT_MUTED),
                );
            }
        });
    });
}

fn render_persistence_card(
    app: &mut ZensmuApp,
    ui: &mut Ui,
    ctx: &egui::Context,
    has_hw: bool,
    busy: bool,
) {
    let status = app.persist_status.clone();
    let service_name = if status.service_name.is_empty() {
        DEFAULT_SERVICE_NAME
    } else {
        status.service_name.as_str()
    };
    let active_path = if status.active_profile_path.is_empty() {
        DEFAULT_ACTIVE_PROFILE_PATH
    } else {
        status.active_profile_path.as_str()
    };

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "persist_card_header",
            |ui| {
                ui.label(RichText::new("Boot & Resume Persistence (systemd)").strong());
                if status.boot_guard_tripped {
                    tinted_pill(ui, "GUARD TRIPPED", ACCENT_CRIMSON, PILL_ALPHA_CRIMSON);
                } else if status.service_enabled
                    && status.active_profile_exists
                    && status.active_profile_valid != Some(false)
                {
                    tinted_pill(ui, "ACTIVE", ACCENT_EMERALD, PILL_ALPHA_EMERALD);
                } else {
                    muted_pill(ui, "INACTIVE");
                }
            },
            |ui| {
                if ui
                    .button("Refresh Status")
                    .on_hover_text("Re-check boot persistence & systemd status (unprivileged)")
                    .clicked()
                {
                    app.refresh_persist_status();
                    app.refresh_safety_files();
                    app.set_toast(ToastLevel::Info, "Refreshed boot persistence status.");
                }
            },
        );

        ui.add_space(6.0);
        egui::Grid::new("persist_status_grid")
            .num_columns(2)
            .striped(true)
            .spacing([16.0, 6.0])
            .show(ui, |ui| {
                // 1. Active Profile
                ui.label(
                    RichText::new(format!("Active Profile ({active_path})"))
                        .small()
                        .color(TEXT_MUTED),
                );
                if status.active_profile_exists {
                    match status.active_profile_valid {
                        Some(true) => {
                            let label = match status.active_profile_codename {
                                Some(cn) => format!("Valid ({})", cn.display_name()),
                                None => "Valid".to_string(),
                            };
                            ui.label(RichText::new(label).small().strong().color(ACCENT_EMERALD));
                        }
                        Some(false) => {
                            let err = status
                                .active_profile_error
                                .as_deref()
                                .unwrap_or("validation error");
                            ui.label(
                                RichText::new(format!("Invalid ({err})"))
                                    .small()
                                    .strong()
                                    .color(ACCENT_CRIMSON),
                            );
                        }
                        None => {
                            ui.label(RichText::new("Present").small().color(ACCENT_CYAN));
                        }
                    }
                } else {
                    ui.label(RichText::new("Not saved").small().color(TEXT_MUTED));
                }
                ui.end_row();

                // 2. Kernel Module Autoload
                ui.label(
                    RichText::new(format!(
                        "Kernel Module Autoload ({DEFAULT_MODULES_LOAD_PATH})"
                    ))
                    .small()
                    .color(TEXT_MUTED),
                );
                if status.modules_load_configured {
                    ui.label(
                        RichText::new("Configured")
                            .small()
                            .strong()
                            .color(ACCENT_EMERALD),
                    );
                } else {
                    ui.label(RichText::new("Not configured").small().color(TEXT_MUTED));
                }
                ui.end_row();

                // 3. Boot Service
                ui.label(
                    RichText::new(format!("Boot Service ({service_name})"))
                        .small()
                        .color(TEXT_MUTED),
                );
                if status.service_enabled {
                    ui.label(RichText::new("Enabled").small().strong().color(ACCENT_EMERALD));
                } else if !status.service_installed {
                    ui.label(
                        RichText::new(format!("Not installed ({})", status.service_state))
                            .small()
                            .color(ACCENT_AMBER),
                    );
                } else {
                    ui.label(RichText::new("Disabled").small().color(TEXT_MUTED));
                }
                ui.end_row();

                // 4. Suspend/Resume Hook
                ui.label(
                    RichText::new("Suspend/Resume Hook (zensmu-resume)")
                        .small()
                        .color(TEXT_MUTED),
                );
                if status.resume_hook_installed {
                    ui.label(
                        RichText::new("Installed")
                            .small()
                            .strong()
                            .color(ACCENT_EMERALD),
                    );
                } else {
                    ui.label(
                        RichText::new(format!("Missing ({DEFAULT_RESUME_HOOK_PATH})"))
                            .small()
                            .color(ACCENT_AMBER),
                    );
                }
                ui.end_row();

                // 5. Boot-Loop Crash Guard
                ui.label(
                    RichText::new("Boot-Loop Crash Guard")
                        .small()
                        .color(TEXT_MUTED),
                );
                if status.boot_guard_tripped {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(
                            RichText::new("TRIPPED — auto-apply halted")
                                .small()
                                .strong()
                                .color(ACCENT_CRIMSON),
                        );
                        let clear_label = if app.is_root {
                            "Clear Boot Guard"
                        } else {
                            "Clear Boot Guard (pkexec)"
                        };
                        if ui
                            .add_enabled(!busy, cta_button(clear_label, CTA_CRIMSON).small())
                            .clicked()
                        {
                            app.dispatch_cli(CliAction::ClearBootGuard, ctx);
                        }
                    });
                } else {
                    ui.label(RichText::new("Clean").small().strong().color(ACCENT_EMERALD));
                }
                ui.end_row();
            });

        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    has_hw && !busy,
                    cta_button("Persist Live SMU & Enable Service", CTA_EMERALD),
                )
                .on_hover_text(
                    "Snapshot live SMU settings to /etc/zensmu/active.toml, configure ryzen_smu autoload, and enable zensmu-apply.service",
                )
                .clicked()
            {
                app.persist_confirm = Some(PersistConfirmModal {
                    title: "Persist Live SMU & Enable Boot Service".into(),
                    summary: "This will save your current live Ryzen SMU tuning for automatic re-application on boot and resume.".into(),
                    changes: vec![
                        format!("Export live SMU state and write {active_path}"),
                        format!("Ensure `ryzen_smu` is configured in {DEFAULT_MODULES_LOAD_PATH}"),
                        format!("Run `systemctl enable {service_name}`"),
                    ],
                    confirm_label: "Persist & Enable".into(),
                    is_destructive: false,
                    target: PersistConfirmTarget::Cli(CliAction::PersistSave {
                        from_path: None,
                        enable: true,
                        allow_extreme: app.profile_editor.allow_extreme,
                    }),
                });
            }

            let editor_non_empty = !app.profile_editor.toml_text.trim().is_empty();
            if ui
                .add_enabled(
                    has_hw && !busy && editor_non_empty,
                    egui::Button::new("Persist Editor TOML & Enable Service"),
                )
                .on_hover_text(
                    "Validate the TOML editor below, install it to /etc/zensmu/active.toml, and enable zensmu-apply.service",
                )
                .clicked()
                && app.validate_editor_toml_for_persist()
            {
                app.persist_confirm = Some(PersistConfirmModal {
                    title: "Persist Editor TOML & Enable Boot Service".into(),
                    summary: "This will install the validated TOML from the editor below as the active boot profile.".into(),
                    changes: vec![
                        format!("Install validated editor TOML to {active_path}"),
                        format!("Ensure `ryzen_smu` is configured in {DEFAULT_MODULES_LOAD_PATH}"),
                        format!("Run `systemctl enable {service_name}`"),
                    ],
                    confirm_label: "Persist Editor & Enable".into(),
                    is_destructive: false,
                    target: PersistConfirmTarget::EditorToml { enable: true },
                });
            }

            if status.service_enabled {
                if ui
                    .add_enabled(!busy, egui::Button::new("Disable Boot Service"))
                    .on_hover_text(
                        "Disable zensmu-apply.service while keeping /etc/zensmu/active.toml on disk",
                    )
                    .clicked()
                {
                    app.persist_confirm = Some(PersistConfirmModal {
                        title: "Disable Boot Service".into(),
                        summary: "Automatic tuning on boot will be disabled. Your saved profile file will remain on disk.".into(),
                        changes: vec![
                            format!("Run `systemctl disable {service_name}`"),
                            format!("Keep {active_path} intact for future use"),
                        ],
                        confirm_label: "Disable Service".into(),
                        is_destructive: false,
                        target: PersistConfirmTarget::Cli(CliAction::PersistSetService {
                            enable: false,
                            remove_profile: false,
                            allow_extreme: false,
                        }),
                    });
                }
            } else {
                let can_enable = has_hw
                    && !busy
                    && status.active_profile_exists
                    && status.active_profile_valid != Some(false);
                let hover = if !status.active_profile_exists {
                    "Save an active profile first before enabling zensmu-apply.service"
                } else if status.active_profile_valid == Some(false) {
                    "Active profile is invalid — save a valid profile first"
                } else {
                    "Enable zensmu-apply.service using the existing /etc/zensmu/active.toml"
                };
                if ui
                    .add_enabled(can_enable, egui::Button::new("Enable Boot Service"))
                    .on_hover_text(hover)
                    .clicked()
                {
                    app.persist_confirm = Some(PersistConfirmModal {
                        title: "Enable Boot Service".into(),
                        summary: "This will enable automatic application of the existing active profile on boot and resume.".into(),
                        changes: vec![
                            format!("Validate existing {active_path}"),
                            format!("Ensure `ryzen_smu` is configured in {DEFAULT_MODULES_LOAD_PATH}"),
                            format!("Run `systemctl enable {service_name}`"),
                        ],
                        confirm_label: "Enable Service".into(),
                        is_destructive: false,
                        target: PersistConfirmTarget::Cli(CliAction::PersistSetService {
                            enable: true,
                            remove_profile: false,
                            allow_extreme: app.profile_editor.allow_extreme,
                        }),
                    });
                }
            }

            if status.active_profile_exists
                && ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(
                            RichText::new("Disable & Remove Active Profile").color(ACCENT_CRIMSON),
                        ),
                    )
                    .on_hover_text(
                        "Disable zensmu-apply.service and delete /etc/zensmu/active.toml",
                    )
                    .clicked()
            {
                app.persist_confirm = Some(PersistConfirmModal {
                    title: "Disable & Remove Active Profile".into(),
                    summary: "This will disable boot persistence and delete the active boot profile file.".into(),
                    changes: vec![
                        format!("Run `systemctl disable {service_name}`"),
                        format!("Delete {active_path}"),
                    ],
                    confirm_label: "Disable & Remove".into(),
                    is_destructive: true,
                    target: PersistConfirmTarget::Cli(CliAction::PersistSetService {
                        enable: false,
                        remove_profile: true,
                        allow_extreme: false,
                    }),
                });
            }
        });
    });
}

/// Modal confirming boot persistence & systemd changes before invoking `pkexec`.
pub fn render_persist_confirm_modal(app: &mut ZensmuApp, ctx: &egui::Context) {
    let Some(modal) = app.persist_confirm.clone() else {
        return;
    };

    let mut close_modal = false;
    let mut confirmed_target: Option<PersistConfirmTarget> = None;

    egui::Window::new(format!("Confirm System Change — {}", modal.title))
        .collapsible(false)
        .resizable(false)
        .default_width(560.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(&modal.summary);
            ui.add_space(8.0);

            egui::Frame::NONE
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
                .corner_radius(egui::CornerRadius::same(6))
                .inner_margin(egui::Margin::same(10))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("EXACT SYSTEM CHANGES")
                            .small()
                            .strong()
                            .color(TEXT_MUTED),
                    );
                    ui.add_space(4.0);
                    for change in &modal.changes {
                        ui.label(
                            RichText::new(format!("• {change}"))
                                .small()
                                .monospace()
                                .color(TEXT_PRIMARY),
                        );
                    }
                });

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);

            let mut cancel_clicked = false;
            let mut confirm_clicked = false;
            split_or_wrap_row(
                ui,
                "persist_modal_actions",
                |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel_clicked = true;
                    }
                },
                |ui| {
                    let btn_label = if app.is_root {
                        modal.confirm_label.clone()
                    } else {
                        format!("{} (pkexec)", modal.confirm_label)
                    };
                    let fill = if modal.is_destructive {
                        CTA_CRIMSON
                    } else {
                        CTA_EMERALD
                    };
                    if ui
                        .add_enabled(!app.runner.is_busy(), cta_button(btn_label, fill))
                        .clicked()
                    {
                        confirmed_target = Some(modal.target.clone());
                        confirm_clicked = true;
                    }
                },
            );
            close_modal = cancel_clicked || confirm_clicked;
        });

    if close_modal {
        app.persist_confirm = None;
    }
    if let Some(target) = confirmed_target {
        match target {
            PersistConfirmTarget::Cli(action) => app.dispatch_cli(action, ctx),
            PersistConfirmTarget::EditorToml { enable } => app.persist_editor_toml(enable, ctx),
        }
    }
}

/// Modal displaying every planned SMU command from `--dry-run --json` before applying.
pub fn render_dry_run_modal(app: &mut ZensmuApp, ctx: &egui::Context) {
    let Some(modal) = app.pending_modal.clone() else {
        return;
    };

    let mut close_modal = false;
    let mut confirm_action: Option<CliAction> = None;

    egui::Window::new(format!("Dry-Run Confirmation — {}", modal.title))
        .collapsible(false)
        .resizable(true)
        .default_width(860.0)
        .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                RichText::new(
                    "Review every planned SMU mailbox command below before elevating privileges to write to hardware.",
                )
                .color(TEXT_MUTED),
            );
            ui.add_space(8.0);

            if modal.ops.is_empty() {
                ui.label(
                    RichText::new("No SMU write operations are required for this request.")
                        .color(ACCENT_EMERALD),
                );
            } else {
                egui::ScrollArea::vertical()
                    .max_height(320.0)
                    .show(ui, |ui| {
                        egui::Grid::new("dry_run_ops_table")
                            .num_columns(6)
                            .striped(true)
                            .spacing([10.0, 6.0])
                            .show(ui, |ui| {
                                ui.label(RichText::new("#").small().strong().color(TEXT_MUTED));
                                ui.label(
                                    RichText::new("Operation").small().strong().color(TEXT_MUTED),
                                );
                                ui.label(
                                    RichText::new("Mailbox").small().strong().color(TEXT_MUTED),
                                );
                                ui.label(
                                    RichText::new("Opcode").small().strong().color(TEXT_MUTED),
                                );
                                ui.label(
                                    RichText::new("Encoded Args").small().strong().color(TEXT_MUTED),
                                );
                                ui.label(
                                    RichText::new("Current → Target")
                                        .small()
                                        .strong()
                                        .color(TEXT_MUTED),
                                );
                                ui.end_row();

                                for (i, op) in modal.ops.iter().enumerate() {
                                    ui.label(RichText::new(format!("{}", i + 1)).monospace());
                                    ui.label(&op.description);
                                    ui.label(
                                        RichText::new(op.mailbox.as_str())
                                            .monospace()
                                            .color(ACCENT_CYAN),
                                    );
                                    ui.label(
                                        RichText::new(format!("0x{:02X}", op.opcode))
                                            .monospace()
                                            .color(ACCENT_AMBER),
                                    );
                                    ui.label(
                                        RichText::new(format!(
                                            "[0x{:08X}, 0x{:X}, 0x{:X}, 0x{:X}, 0x{:X}, 0x{:X}]",
                                            op.args[0],
                                            op.args[1],
                                            op.args[2],
                                            op.args[3],
                                            op.args[4],
                                            op.args[5],
                                        ))
                                        .small()
                                        .monospace(),
                                    );
                                    let transition = format!(
                                        "{} → {}",
                                        op.current.as_deref().unwrap_or("—"),
                                        op.target
                                    );
                                    ui.label(
                                        RichText::new(transition)
                                            .monospace()
                                            .color(ACCENT_EMERALD),
                                    );
                                    ui.end_row();
                                }
                            });
                    });
            }

            ui.add_space(12.0);
            ui.separator();
            ui.add_space(6.0);

            let mut cancel_clicked = false;
            let mut confirm_clicked = false;
            split_or_wrap_row(
                ui,
                "dry_run_modal_actions",
                |ui| {
                    if ui.button("Cancel").clicked() {
                        cancel_clicked = true;
                    }
                },
                |ui| {
                    let confirm_label = if app.is_root {
                        "Confirm & Apply"
                    } else {
                        "Confirm & Apply (pkexec)"
                    };
                    if ui
                        .add_enabled(
                            !modal.ops.is_empty() && !app.runner.is_busy(),
                            cta_button(confirm_label, CTA_CRIMSON),
                        )
                        .clicked()
                    {
                        confirm_action = Some(modal.live_action.clone());
                        confirm_clicked = true;
                    }
                },
            );
            close_modal = cancel_clicked || confirm_clicked;
        });

    if let Some(action) = confirm_action {
        app.pending_modal = None;
        app.dispatch_cli(action, ctx);
    } else if close_modal {
        app.dismiss_dry_run_modal();
    }
}

fn render_no_hardware_placeholder(ui: &mut Ui) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(
            RichText::new("Waiting for ryzen_smu kernel driver")
                .strong()
                .color(ACCENT_AMBER),
        );
        ui.label("Load the driver (`sudo modprobe ryzen_smu`) and click 'Retry Hardware Probe' in the banner above.");
    });
}
