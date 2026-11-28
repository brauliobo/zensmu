use crate::app::configure_dark_cockpit_theme;
use crate::views;
use eframe::egui;

/// Convert an 8-bit sRGB channel to linear light per WCAG 2.1 §1.4.3.
fn srgb_channel_to_linear(channel: u8) -> f64 {
    let s = f64::from(channel) / 255.0;
    if s <= 0.04045 {
        s / 12.92
    } else {
        ((s + 0.055) / 1.055).powf(2.4)
    }
}

/// WCAG 2.1 relative luminance for an opaque sRGB `Color32`.
fn relative_luminance(color: egui::Color32) -> f64 {
    let [r, g, b, _] = color.to_srgba_unmultiplied();
    0.2126 * srgb_channel_to_linear(r)
        + 0.7152 * srgb_channel_to_linear(g)
        + 0.0722 * srgb_channel_to_linear(b)
}

/// Alpha-composite a (possibly translucent) `egui::Color32` foreground over an opaque background.
fn composite_over(fg: egui::Color32, bg: egui::Color32) -> egui::Color32 {
    let [fr, fg_c, fb, fa] = fg.to_srgba_unmultiplied();
    let [br, bg_c, bb, _] = bg.to_srgba_unmultiplied();
    let a = f64::from(fa) / 255.0;
    let blend = |f: u8, b: u8| -> u8 {
        (f64::from(f) * a + f64::from(b) * (1.0 - a))
            .round()
            .clamp(0.0, 255.0) as u8
    };
    egui::Color32::from_rgb(blend(fr, br), blend(fg_c, bg_c), blend(fb, bb))
}

/// Exact WCAG 2.1 contrast ratio `(L1 + 0.05) / (L2 + 0.05)` in `[1.0, 21.0]`.
fn contrast_ratio(fg: egui::Color32, bg: egui::Color32) -> f64 {
    let fg_opaque = if fg.a() < 255 {
        composite_over(fg, bg)
    } else {
        fg
    };
    let l1 = relative_luminance(fg_opaque);
    let l2 = relative_luminance(bg);
    let (hi, lo) = if l1 >= l2 { (l1, l2) } else { (l2, l1) };
    (hi + 0.05) / (lo + 0.05)
}

fn assert_wcag(label: &str, fg: egui::Color32, bg: egui::Color32, min_ratio: f64) {
    let ratio = contrast_ratio(fg, bg);
    assert!(
        ratio >= min_ratio,
        "WCAG contrast failure for {label}: fg={fg:?} on bg={bg:?} -> {ratio:.2}:1 (required >= {min_ratio:.1}:1)"
    );
}

#[test]
fn wcag_contrast_math_matches_reference_values() {
    let bw = contrast_ratio(egui::Color32::BLACK, egui::Color32::WHITE);
    assert!(
        (bw - 21.0).abs() < 1e-6,
        "black/white expected 21.0, got {bw}"
    );

    let same = contrast_ratio(views::BG_CARD, views::BG_CARD);
    assert!(
        (same - 1.0).abs() < 1e-6,
        "identical colors expected 1.0, got {same}"
    );

    // #767676 on #FFFFFF is the canonical WCAG 2.1 AA boundary (~4.54:1).
    let aa_ref = contrast_ratio(
        egui::Color32::from_rgb(0x76, 0x76, 0x76),
        egui::Color32::WHITE,
    );
    assert!(
        (4.53..=4.56).contains(&aa_ref),
        "#767676 on white expected ~4.54:1, got {aa_ref:.3}"
    );
}

#[test]
fn dark_cockpit_palette_and_widgets_pass_wcag_aa() {
    const AA_TEXT: f64 = 4.5;
    const AAA_TEXT: f64 = 7.0;
    const AA_GRAPHICAL: f64 = 3.0;

    let ctx = egui::Context::default();
    configure_dark_cockpit_theme(&ctx);
    let visuals = ctx.style().visuals.clone();

    // 1. Every semantic foreground/text color against every dark surface
    let dark_surfaces = [
        ("BG_CANVAS", views::BG_CANVAS),
        ("BG_EXTREME", views::BG_EXTREME),
        ("BG_PANEL", views::BG_PANEL),
        ("BG_CARD", views::BG_CARD),
        ("BG_STRIPE", views::BG_STRIPE),
        ("BG_ELEVATED", views::BG_ELEVATED),
        ("BG_HOVERED", views::BG_HOVERED),
        ("BG_OPEN", views::BG_OPEN),
    ];
    let text_colors = [
        ("TEXT_BRIGHT", views::TEXT_BRIGHT),
        ("TEXT_PRIMARY", views::TEXT_PRIMARY),
        ("TEXT_MUTED", views::TEXT_MUTED),
        ("ACCENT_CRIMSON", views::ACCENT_CRIMSON),
        ("ACCENT_CYAN", views::ACCENT_CYAN),
        ("ACCENT_AMBER", views::ACCENT_AMBER),
        ("ACCENT_EMERALD", views::ACCENT_EMERALD),
        ("ACCENT_VIOLET", views::ACCENT_VIOLET),
    ];
    for (fg_name, fg) in text_colors {
        for (bg_name, bg) in dark_surfaces {
            assert_wcag(&format!("{fg_name} on {bg_name}"), fg, bg, AA_TEXT);
        }
    }

    // Primary body text should hit AAA (>= 7:1) across all main surfaces.
    for (bg_name, bg) in dark_surfaces {
        assert_wcag(
            &format!("TEXT_PRIMARY AAA on {bg_name}"),
            views::TEXT_PRIMARY,
            bg,
            AAA_TEXT,
        );
        assert_wcag(
            &format!("TEXT_BRIGHT AAA on {bg_name}"),
            views::TEXT_BRIGHT,
            bg,
            AAA_TEXT,
        );
    }

    // 2. Configured egui::Visuals widget states (buttons, inputs, sliders, checkboxes, selection)
    assert_wcag(
        "visuals.noninteractive text on BG_CARD",
        visuals.widgets.noninteractive.fg_stroke.color,
        views::BG_CARD,
        AAA_TEXT,
    );
    assert_wcag(
        "visuals.inactive button text on weak_bg_fill",
        visuals.widgets.inactive.fg_stroke.color,
        visuals.widgets.inactive.weak_bg_fill,
        AAA_TEXT,
    );
    assert_wcag(
        "visuals.inactive control fg on bg_fill",
        visuals.widgets.inactive.fg_stroke.color,
        visuals.widgets.inactive.bg_fill,
        AAA_TEXT,
    );
    assert_wcag(
        "visuals.hovered button text on weak_bg_fill",
        visuals.widgets.hovered.fg_stroke.color,
        visuals.widgets.hovered.weak_bg_fill,
        AAA_TEXT,
    );
    assert_wcag(
        "visuals.active button text on weak_bg_fill",
        visuals.widgets.active.fg_stroke.color,
        visuals.widgets.active.weak_bg_fill,
        AA_TEXT,
    );
    assert_wcag(
        "visuals.active control fg on bg_fill",
        visuals.widgets.active.fg_stroke.color,
        visuals.widgets.active.bg_fill,
        AA_TEXT,
    );
    assert_wcag(
        "visuals.open text on weak_bg_fill",
        visuals.widgets.open.fg_stroke.color,
        visuals.widgets.open.weak_bg_fill,
        AAA_TEXT,
    );
    for (bg_name, bg) in [
        ("BG_PANEL", views::BG_PANEL),
        ("BG_CARD", views::BG_CARD),
        ("BG_EXTREME", views::BG_EXTREME),
    ] {
        let sel_bg = composite_over(visuals.selection.bg_fill, bg);
        assert_wcag(
            &format!("visuals.selection stroke on selection bg over {bg_name}"),
            visuals.selection.stroke.color,
            sel_bg,
            AAA_TEXT,
        );
        assert_wcag(
            &format!("TEXT_PRIMARY on selection bg over {bg_name}"),
            views::TEXT_PRIMARY,
            sel_bg,
            AAA_TEXT,
        );
    }
    assert_wcag(
        "ui.code text on code_bg_color",
        visuals.widgets.noninteractive.fg_stroke.color,
        visuals.code_bg_color,
        AAA_TEXT,
    );
    assert_wcag(
        "TextEdit text on extreme_bg_color",
        visuals.widgets.inactive.fg_stroke.color,
        visuals.extreme_bg_color,
        AAA_TEXT,
    );
    assert_wcag(
        "TextEdit hint (weak_text_color) on extreme_bg_color",
        visuals.weak_text_color(),
        visuals.extreme_bg_color,
        AA_TEXT,
    );
    for (bg_name, bg) in [
        ("BG_CANVAS", views::BG_CANVAS),
        ("BG_PANEL", views::BG_PANEL),
        ("BG_CARD", views::BG_CARD),
        ("BG_STRIPE", views::BG_STRIPE),
    ] {
        assert_wcag(
            &format!("hyperlink_color on {bg_name}"),
            visuals.hyperlink_color,
            bg,
            AA_TEXT,
        );
        assert_wcag(
            &format!("warn_fg_color on {bg_name}"),
            visuals.warn_fg_color,
            bg,
            AA_TEXT,
        );
        assert_wcag(
            &format!("error_fg_color on {bg_name}"),
            visuals.error_fg_color,
            bg,
            AA_TEXT,
        );
    }

    // 3. Solid CTA buttons
    assert_wcag(
        "CTA_CRIMSON button label",
        views::TEXT_ON_CTA,
        views::CTA_CRIMSON,
        AA_TEXT,
    );
    assert_wcag(
        "CTA_EMERALD button label",
        views::TEXT_ON_CTA,
        views::CTA_EMERALD,
        AA_TEXT,
    );

    // 4. Tinted pills composited over panel, card, and striped table rows
    let pill_specs = [
        (
            "crimson pill",
            views::ACCENT_CRIMSON,
            views::PILL_ALPHA_CRIMSON,
        ),
        ("amber pill", views::ACCENT_AMBER, views::PILL_ALPHA_AMBER),
        (
            "emerald pill",
            views::ACCENT_EMERALD,
            views::PILL_ALPHA_EMERALD,
        ),
        ("cyan pill", views::ACCENT_CYAN, views::PILL_ALPHA_CYAN),
        (
            "violet pill",
            views::ACCENT_VIOLET,
            views::PILL_ALPHA_VIOLET,
        ),
    ];
    for (name, fg, alpha) in pill_specs {
        let tint = views::tinted_bg(fg, alpha);
        for (bg_name, under) in [
            ("BG_PANEL", views::BG_PANEL),
            ("BG_CARD", views::BG_CARD),
            ("BG_STRIPE", views::BG_STRIPE),
        ] {
            let effective_bg = composite_over(tint, under);
            assert_wcag(&format!("{name} on {bg_name}"), fg, effective_bg, AA_TEXT);
        }
    }
    assert_wcag("muted_pill", views::TEXT_MUTED, views::BG_ELEVATED, AA_TEXT);
    assert_wcag(
        "identity_pill",
        views::TEXT_PRIMARY,
        views::BG_ELEVATED,
        AAA_TEXT,
    );

    // 5. Alert banners (root warning & missing driver) on BG_CANVAS
    let root_banner_bg = composite_over(
        views::tinted_bg(views::ACCENT_CRIMSON, views::BANNER_ALPHA_CRIMSON),
        views::BG_CANVAS,
    );
    assert_wcag(
        "root banner title",
        views::ACCENT_CRIMSON,
        root_banner_bg,
        AA_TEXT,
    );
    assert_wcag(
        "root banner body",
        views::TEXT_PRIMARY,
        root_banner_bg,
        AAA_TEXT,
    );

    let hw_banner_bg = composite_over(
        views::tinted_bg(views::ACCENT_AMBER, views::BANNER_ALPHA_AMBER),
        views::BG_CANVAS,
    );
    assert_wcag(
        "hw_error banner title",
        views::ACCENT_AMBER,
        hw_banner_bg,
        AA_TEXT,
    );
    assert_wcag(
        "hw_error banner body",
        views::TEXT_PRIMARY,
        hw_banner_bg,
        AAA_TEXT,
    );

    // 6. Selected sidebar navigation tab on BG_PANEL
    let sidebar_sel_bg = composite_over(
        views::tinted_bg(views::ACCENT_CRIMSON, views::SIDEBAR_SELECTED_ALPHA),
        views::BG_PANEL,
    );
    assert_wcag(
        "sidebar selected title",
        views::TEXT_BRIGHT,
        sidebar_sel_bg,
        AAA_TEXT,
    );
    assert_wcag(
        "sidebar selected subtitle",
        views::TEXT_MUTED,
        sidebar_sel_bg,
        AA_TEXT,
    );

    // 7. Graphical UI components (>= 3.0:1): progress bar fills on dark track, plot lines, interactive control borders
    for (name, bar_color) in [
        ("ACCENT_CYAN", views::ACCENT_CYAN),
        ("ACCENT_AMBER", views::ACCENT_AMBER),
        ("ACCENT_EMERALD", views::ACCENT_EMERALD),
        ("ACCENT_VIOLET", views::ACCENT_VIOLET),
        ("ACCENT_CRIMSON", views::ACCENT_CRIMSON),
    ] {
        assert_wcag(
            &format!("progress bar / plot {name} on BG_EXTREME"),
            bar_color,
            views::BG_EXTREME,
            AA_GRAPHICAL,
        );
        assert_wcag(
            &format!("plot {name} on BG_CARD"),
            bar_color,
            views::BG_CARD,
            AA_GRAPHICAL,
        );
    }
    for (bg_name, bg) in [
        ("BG_EXTREME", views::BG_EXTREME),
        ("BG_PANEL", views::BG_PANEL),
        ("BG_CARD", views::BG_CARD),
        ("BG_STRIPE", views::BG_STRIPE),
    ] {
        assert_wcag(
            &format!("BORDER_CONTROL on {bg_name}"),
            views::BORDER_CONTROL,
            bg,
            AA_GRAPHICAL,
        );
        assert_wcag(
            &format!("BORDER_HOVER on {bg_name}"),
            views::BORDER_HOVER,
            bg,
            AA_GRAPHICAL,
        );
    }
}

#[test]
fn compact_cpu_label_shortens_verbose_ryzen_brands() {
    assert_eq!(
        views::compact_cpu_label("AMD Ryzen 7 9700X 8-Core Processor"),
        "Ryzen 7 9700X"
    );
    assert_eq!(
        views::compact_cpu_label("AMD Ryzen 9 9950X3D 16-Core Processor"),
        "Ryzen 9 9950X3D"
    );
    assert_eq!(
        views::compact_cpu_label("AMD Ryzen 7 7840U w/ Radeon 780M Graphics"),
        "Ryzen 7 7840U"
    );
    assert_eq!(
        views::compact_cpu_label("AMD Ryzen 7 5700G with Radeon Graphics"),
        "Ryzen 7 5700G"
    );
}
