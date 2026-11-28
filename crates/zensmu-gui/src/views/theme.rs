//! Dark cockpit WCAG AA color palette and translucent tint helpers.

use eframe::egui::Color32;

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
