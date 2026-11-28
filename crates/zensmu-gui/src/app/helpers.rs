//! Pure formatting, URL launch, UI zoom, dark cockpit theme, and profile template helpers.

use super::state::{UI_ZOOM_MAX, UI_ZOOM_MIN};
use crate::views;
use eframe::egui;
use std::process::Command;
use zensmu_core::config::profile::PROFILE_SCHEMA_VERSION;
use zensmu_core::hw::identity::HardwareContext;
use zensmu_core::report::SystemBoardInfo;

/// Returns true when a CLI/telemetry error string indicates an unexpected SMU rejection,
/// verification failure, unsupported CPU/PM table, or identity mismatch where filing a
/// diagnostic GitHub report is helpful.
pub fn error_message_suggests_report(msg: &str) -> bool {
    let lower = msg.to_ascii_lowercase();
    lower.contains("smu rejected")
        || lower.contains("verification failed")
        || lower.contains("unsupported")
        || lower.contains("mismatch")
        || lower.contains("mailbox health check failed")
}

/// Launch an `http://` or `https://` URL in the user's desktop browser via `xdg-open`
/// without any shell interpolation.
pub fn open_url_in_browser(url: &str) -> Result<(), String> {
    let trimmed = url.trim();
    if !(trimmed.starts_with("https://") || trimmed.starts_with("http://")) {
        return Err("Refused to open URL: only http:// and https:// URLs are supported.".into());
    }
    Command::new("xdg-open")
        .arg(trimmed)
        .spawn()
        .map(|_| ())
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "Could not find `xdg-open`. Copy the Markdown report and open GitHub manually."
                    .into()
            } else {
                format!("Failed to launch browser via `xdg-open`: {e}")
            }
        })
}

/// Format a compact motherboard & BIOS summary string from non-identifying DMI fields.
pub fn format_board_summary(info: &SystemBoardInfo) -> Option<String> {
    let vendor = info
        .board_vendor
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let name = info
        .board_name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let bios = info
        .bios_version
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());

    let board_part = match (vendor, name) {
        (Some(v), Some(n)) => {
            if n.to_ascii_lowercase().starts_with(&v.to_ascii_lowercase()) {
                Some(n.to_string())
            } else {
                Some(format!("{v} {n}"))
            }
        }
        (Some(v), None) => Some(v.to_string()),
        (None, Some(n)) => Some(n.to_string()),
        (None, None) => None,
    };

    match (board_part, bios) {
        (Some(b), Some(v)) => Some(format!("{b} · BIOS {v}")),
        (Some(b), None) => Some(b),
        (None, Some(v)) => Some(format!("BIOS {v}")),
        (None, None) => None,
    }
}

/// Clamp and round a UI zoom value to two decimal places within `UI_ZOOM_MIN..=UI_ZOOM_MAX`.
pub fn clamp_ui_zoom(zoom: f32) -> f32 {
    (zoom.clamp(UI_ZOOM_MIN, UI_ZOOM_MAX) * 100.0).round() / 100.0
}

/// Apply the dark cockpit visual theme and legible desktop typography to `egui`.
pub fn configure_dark_cockpit_theme(ctx: &egui::Context) {
    // Manage Ctrl+/Ctrl-/Ctrl+0 ourselves so `ZensmuApp::ui_zoom` stays in sync and clamped.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);

    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = views::BG_PANEL;
    visuals.window_fill = views::BG_CARD;
    visuals.faint_bg_color = views::BG_STRIPE;
    visuals.extreme_bg_color = views::BG_EXTREME;
    visuals.code_bg_color = views::BG_ELEVATED;
    visuals.window_stroke = egui::Stroke::new(1.0_f32, views::BORDER_SUBTLE);

    visuals.hyperlink_color = views::ACCENT_CYAN;
    visuals.warn_fg_color = views::ACCENT_AMBER;
    visuals.error_fg_color = views::ACCENT_CRIMSON;

    // Non-interactive body text, headings, dividers
    visuals.widgets.noninteractive.weak_bg_fill = views::BG_CARD;
    visuals.widgets.noninteractive.bg_fill = views::BG_CARD;
    visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, views::BORDER_SUBTLE);
    visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, views::TEXT_PRIMARY);

    // Inactive buttons (`weak_bg_fill`), checkboxes, sliders, text inputs (`bg_fill`)
    visuals.widgets.inactive.weak_bg_fill = views::BG_ELEVATED;
    visuals.widgets.inactive.bg_fill = views::BG_ELEVATED;
    visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, views::BORDER_CONTROL);
    visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, views::TEXT_PRIMARY);

    // Hovered controls
    visuals.widgets.hovered.weak_bg_fill = views::BG_HOVERED;
    visuals.widgets.hovered.bg_fill = views::BG_HOVERED;
    visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.0_f32, views::BORDER_HOVER);
    visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5_f32, views::TEXT_BRIGHT);

    // Active / pressed controls & dragged slider handle
    visuals.widgets.active.weak_bg_fill = views::CTA_CRIMSON;
    visuals.widgets.active.bg_fill = views::CTA_CRIMSON;
    visuals.widgets.active.bg_stroke = egui::Stroke::new(1.0_f32, views::TEXT_ON_CTA);
    visuals.widgets.active.fg_stroke = egui::Stroke::new(2.0_f32, views::TEXT_ON_CTA);

    // Open ComboBox / dropdown headers
    visuals.widgets.open.weak_bg_fill = views::BG_OPEN;
    visuals.widgets.open.bg_fill = views::BG_OPEN;
    visuals.widgets.open.bg_stroke = egui::Stroke::new(1.0_f32, views::BORDER_HOVER);
    visuals.widgets.open.fg_stroke = egui::Stroke::new(1.0_f32, views::TEXT_BRIGHT);

    // Selection highlight (ComboBox selected row, TextEdit selection, ProgressBar default text)
    visuals.selection.bg_fill = views::SELECTION_BG;
    visuals.selection.stroke = egui::Stroke::new(1.0_f32, views::TEXT_BRIGHT);

    ctx.set_visuals(visuals);

    let mut style = (*ctx.style()).clone();
    style.text_styles = [
        (
            egui::TextStyle::Heading,
            egui::FontId::new(24.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Body,
            egui::FontId::new(16.0, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Button,
            egui::FontId::new(15.5, egui::FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Monospace,
            egui::FontId::new(15.0, egui::FontFamily::Monospace),
        ),
        (
            egui::TextStyle::Small,
            egui::FontId::new(13.5, egui::FontFamily::Proportional),
        ),
    ]
    .into();
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    style.spacing.button_padding = egui::vec2(12.0, 6.0);
    style.spacing.interact_size.y = 26.0;
    style.spacing.slider_width = 150.0;
    ctx.set_style(style);
}

pub(super) fn default_profile_template(hw: &HardwareContext) -> String {
    let mut out = format!(
        "schema = {PROFILE_SCHEMA_VERSION}\n\n[target]\ncodename = \"{}\"\nphysical_cores = {}\n",
        hw.codename.slug(),
        hw.topology.physical_cores
    );

    if hw.capabilities.co_bounds.is_some() {
        out.push_str("\n[curve_optimizer]\nall = -15\n# per_core = { \"0\" = -10, \"1\" = -20 }\n");
        if hw.capabilities.set_igpu_co.is_some() {
            out.push_str("# igpu = -10\n");
        }
    }

    if hw.capabilities.set_curve_shaper.is_some() {
        out.push_str(
            "\n[curve_shaper.high]\nlow = -10\nmed = -15\nhigh = -10\n\n[curve_shaper.max]\nlow = -5\nmed = -10\nhigh = -5\n",
        );
    }

    if hw.capabilities.set_ppt.is_some() {
        out.push_str("\n[pbo]\nppt_w = 105\ntdc_a = 80\nedc_a = 150\ntctl_c = 85\n# scalar = 1\n");
    }

    out
}
