//! Dark cockpit UI panels, plots, tuning controls, and dry-run confirmation modal.

use crate::app::{Tab, ToastLevel, ZensmuApp, POLL_INTERVAL_OPTIONS_MS, TELEMETRY_RING_CAPACITY};
use crate::elevate::CliAction;
use eframe::egui::{self, Color32, RichText, Stroke, Ui};
use egui_plot::{HLine, Line, Plot, PlotPoints};
use std::collections::BTreeMap;
use zensmu_core::hw::identity::driver_has_false_ok_bug;
use zensmu_core::ops::co::{IGPU_CO_MAX, IGPU_CO_MIN, IGPU_CO_SOFT_MIN};
use zensmu_core::ops::pbo::{
    EDC_MAX_A, EDC_MIN_A, EDC_SOFT_MAX_A, FMAX_MAX_MHZ, FMAX_MIN_MHZ, PPT_MAX_W, PPT_MIN_W,
    PPT_SOFT_MAX_W, SCALAR_MAX, SCALAR_MIN, SCALAR_SOFT_MAX, TCTL_MAX_C, TCTL_MIN_C, TDC_MAX_A,
    TDC_MIN_A, TDC_SOFT_MAX_A,
};
use zensmu_core::ops::shaper::{SHAPER_MAX, SHAPER_MIN, SHAPER_SOFT_MIN};
use zensmu_core::transport::sysfs::{DEFAULT_LOCK_PATH, DEFAULT_SYSFS_PATH};
use zensmu_core::types::{CoreInfo, CurveShaperState, FreqTier};

// Dark cockpit palette
pub const BG_CANVAS: Color32 = Color32::from_rgb(13, 16, 23);
pub const BG_PANEL: Color32 = Color32::from_rgb(18, 22, 31);
pub const BG_CARD: Color32 = Color32::from_rgb(22, 27, 38);
pub const BORDER_SUBTLE: Color32 = Color32::from_rgb(42, 49, 66);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(148, 163, 184);
pub const ACCENT_CRIMSON: Color32 = Color32::from_rgb(224, 90, 71);
pub const ACCENT_CYAN: Color32 = Color32::from_rgb(56, 189, 248);
pub const ACCENT_AMBER: Color32 = Color32::from_rgb(245, 158, 11);
pub const ACCENT_EMERALD: Color32 = Color32::from_rgb(34, 197, 94);
pub const ACCENT_VIOLET: Color32 = Color32::from_rgb(167, 139, 250);

fn card_frame() -> egui::Frame {
    egui::Frame::NONE
        .fill(BG_CARD)
        .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::same(14))
}

fn pill(ui: &mut Ui, text: &str, fg: Color32, bg: Color32) {
    egui::Frame::NONE
        .fill(bg)
        .stroke(Stroke::new(1.0_f32, fg.linear_multiply(0.45)))
        .corner_radius(egui::CornerRadius::same(12))
        .inner_margin(egui::Margin::symmetric(9, 3))
        .show(ui, |ui| {
            ui.label(RichText::new(text).small().strong().color(fg));
        });
}

/// Top command bar with identity pill, privilege badge, polling controls, and global actions.
pub fn render_top_bar(app: &mut ZensmuApp, ctx: &egui::Context) {
    egui::TopBottomPanel::top("top_bar")
        .frame(
            egui::Frame::NONE
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
                .inner_margin(egui::Margin::symmetric(14, 9)),
        )
        .show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new("zensmu")
                        .strong()
                        .size(18.0)
                        .color(ACCENT_CRIMSON),
                );
                ui.separator();

                if let Some(hw) = &app.hw {
                    let cpu_label = hw
                        .cpuid
                        .as_ref()
                        .map(|c| c.brand.as_str())
                        .filter(|b| !b.is_empty())
                        .unwrap_or_else(|| hw.codename.display_name());
                    pill(
                        ui,
                        &format!(
                            "{} · {} · SMU {}",
                            cpu_label,
                            hw.codename.slug(),
                            hw.driver.smu_version
                        ),
                        Color32::from_rgb(226, 232, 240),
                        Color32::from_rgb(30, 38, 54),
                    );
                } else {
                    pill(
                        ui,
                        "ryzen_smu not detected",
                        ACCENT_AMBER,
                        Color32::from_rgba_unmultiplied(245, 158, 11, 28),
                    );
                }

                if app.is_root {
                    pill(
                        ui,
                        "ROOT (uid 0) — run unprivileged!",
                        ACCENT_CRIMSON,
                        Color32::from_rgba_unmultiplied(224, 90, 71, 38),
                    );
                } else {
                    pill(
                        ui,
                        "Unprivileged · pkexec on apply",
                        ACCENT_EMERALD,
                        Color32::from_rgba_unmultiplied(34, 197, 94, 24),
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let busy = app.runner.is_busy();
                    let has_hw = app.hw.is_some();

                    if ui
                        .add_enabled(has_hw && !busy, egui::Button::new("Reset"))
                        .on_hover_text("Preview reset to boot baseline (dry-run first)")
                        .clicked()
                    {
                        app.dispatch_cli(CliAction::Reset { dry_run: true }, ctx);
                    }

                    if ui
                        .add_enabled(has_hw && !busy, egui::Button::new("Rollback"))
                        .on_hover_text("Preview rollback to last pre-change snapshot (dry-run first)")
                        .clicked()
                    {
                        app.dispatch_cli(CliAction::Rollback { dry_run: true }, ctx);
                    }

                    if ui
                        .add_enabled(has_hw && !busy, egui::Button::new("Refresh SMU State"))
                        .on_hover_text("Read current CO, Curve Shaper, and PBO state via pkexec")
                        .clicked()
                    {
                        app.dispatch_cli(CliAction::Status, ctx);
                    }

                    ui.separator();

                    let pause_label = if app.telemetry_paused {
                        "Resume"
                    } else {
                        "Pause"
                    };
                    if ui
                        .add_enabled(has_hw, egui::Button::new(pause_label))
                        .clicked()
                    {
                        app.set_telemetry_paused(!app.telemetry_paused);
                    }

                    let mut selected_ms = app.poll_interval_ms;
                    egui::ComboBox::from_id_salt("poll_rate_combo")
                        .width(74.0)
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
                });
            });
        });
}

fn format_interval_label(ms: u64) -> String {
    if ms >= 1000 && ms % 1000 == 0 {
        format!("{}s", ms / 1000)
    } else {
        format!("{ms}ms")
    }
}

/// Bottom status bar showing last command toast / spinner and telemetry sampling status.
pub fn render_bottom_bar(app: &mut ZensmuApp, ctx: &egui::Context) {
    egui::TopBottomPanel::bottom("bottom_bar")
        .frame(
            egui::Frame::NONE
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
                .inner_margin(egui::Margin::symmetric(14, 6)),
        )
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
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

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
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
                    ui.label(RichText::new(rate_text).small().monospace().color(TEXT_MUTED));
                });
            });
        });
}

/// Left sidebar navigation and compact hardware summary card.
pub fn render_sidebar(app: &mut ZensmuApp, ctx: &egui::Context) {
    egui::SidePanel::left("sidebar")
        .resizable(false)
        .exact_width(210.0)
        .frame(
            egui::Frame::NONE
                .fill(BG_PANEL)
                .stroke(Stroke::new(1.0_f32, BORDER_SUBTLE))
                .inner_margin(egui::Margin::same(12)),
        )
        .show(ctx, |ui| {
            ui.label(
                RichText::new("NAVIGATION")
                    .small()
                    .strong()
                    .color(TEXT_MUTED),
            );
            ui.add_space(6.0);

            let tabs = [
                (Tab::Dashboard, "Dashboard", "Live telemetry"),
                (Tab::CurveOptimizer, "Curve Optimizer", "All-core & per-core"),
                (Tab::CurveShaper, "Curve Shaper", "Zen 5 5×3 matrix"),
                (Tab::PboLimits, "PBO Limits", "PPT / TDC / EDC / Fmax"),
                (Tab::ProfilesSafety, "Profiles & Safety", "TOML, rollback, guards"),
            ];

            for (tab, title, subtitle) in tabs {
                let selected = app.active_tab == tab;
                let fill = if selected {
                    Color32::from_rgba_unmultiplied(224, 90, 71, 36)
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
                            ui.label(
                                RichText::new(title)
                                    .strong()
                                    .color(if selected {
                                        Color32::WHITE
                                    } else {
                                        Color32::from_rgb(203, 213, 225)
                                    }),
                            );
                            ui.label(RichText::new(subtitle).small().color(TEXT_MUTED));
                        });
                    })
                    .response
                    .interact(egui::Sense::click());

                if resp.clicked() {
                    app.active_tab = tab;
                    if tab == Tab::ProfilesSafety {
                        app.refresh_safety_files();
                    }
                }
                ui.add_space(4.0);
            }

            ui.with_layout(egui::Layout::bottom_up(egui::Align::LEFT), |ui| {
                card_frame().show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.label(
                        RichText::new("SYSTEM")
                            .small()
                            .strong()
                            .color(TEXT_MUTED),
                    );
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
    ui.horizontal(|ui| {
        ui.label(RichText::new(key).small().color(TEXT_MUTED));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(RichText::new(val).small().monospace());
        });
    });
}

/// Warning / actionable banners shown above the main content area.
pub fn render_banners(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    if app.is_root {
        egui::Frame::NONE
            .fill(Color32::from_rgba_unmultiplied(224, 90, 71, 32))
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
                        "Please launch zensmu-gui as your normal user account. Telemetry reads sysfs without root, and PolicyKit (pkexec) elevates only individual zensmu CLI commands.",
                    );
                });
            });
        ui.add_space(10.0);
    }

    if let Some(err) = app.hw_error.clone() {
        egui::Frame::NONE
            .fill(Color32::from_rgba_unmultiplied(245, 158, 11, 24))
            .stroke(Stroke::new(1.0_f32, ACCENT_AMBER))
            .corner_radius(egui::CornerRadius::same(8))
            .inner_margin(egui::Margin::same(12))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal_wrapped(|ui| {
                    ui.vertical(|ui| {
                        ui.label(
                            RichText::new("ryzen_smu kernel module is not available")
                                .strong()
                                .color(ACCENT_AMBER),
                        );
                        ui.label(RichText::new(&err).small().color(Color32::from_rgb(226, 232, 240)));
                        ui.add_space(4.0);
                        ui.horizontal(|ui| {
                            ui.label("Load the driver with:");
                            ui.code("sudo modprobe ryzen_smu");
                        });
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Retry Hardware Probe").clicked() {
                            app.probe_hardware(ctx);
                        }
                    });
                });
            });
        ui.add_space(10.0);
    }
}

/// Tab 1: Live Dashboard with rolling plots, limit progress bars, clocks, and voltage rails.
pub fn render_dashboard(app: &mut ZensmuApp, ui: &mut Ui) {
    ui.heading("Live SMU Telemetry Dashboard");
    ui.label(
        RichText::new("Unprivileged 0444 PM table polling from /sys/kernel/ryzen_smu_drv/pm_table")
            .small()
            .color(TEXT_MUTED),
    );
    ui.add_space(10.0);

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

    // Row 1: Power / Thermal / Current Limit Gauges
    ui.columns(4, |cols| {
        render_gauge_card(
            &mut cols[0],
            "PPT Power",
            snap.and_then(|s| s.ppt_value_w),
            snap.and_then(|s| s.ppt_limit_w),
            "W",
            ACCENT_CYAN,
        );
        render_gauge_card(
            &mut cols[1],
            "Tctl Temp",
            snap.and_then(|s| s.tctl_value_c),
            snap.and_then(|s| s.tctl_limit_c),
            "°C",
            ACCENT_AMBER,
        );
        render_gauge_card(
            &mut cols[2],
            "TDC Current",
            snap.and_then(|s| s.tdc_value_a),
            snap.and_then(|s| s.tdc_limit_a),
            "A",
            ACCENT_EMERALD,
        );
        render_gauge_card(
            &mut cols[3],
            "EDC Current",
            snap.and_then(|s| s.edc_value_a),
            snap.and_then(|s| s.edc_limit_a),
            "A",
            ACCENT_VIOLET,
        );
    });

    ui.add_space(12.0);

    // Row 2: Live rolling time-series plots (PPT W and Tctl C)
    let latest_t = app.samples.back().map(|s| s.elapsed_s).unwrap_or(0.0);

    let ppt_points: PlotPoints<'_> = app
        .samples
        .iter()
        .filter_map(|s| s.snapshot.ppt_value_w.map(|v| [s.elapsed_s - latest_t, v as f64]))
        .collect();
    let tctl_points: PlotPoints<'_> = app
        .samples
        .iter()
        .filter_map(|s| s.snapshot.tctl_value_c.map(|v| [s.elapsed_s - latest_t, v as f64]))
        .collect();

    let ppt_limit = snap.and_then(|s| s.ppt_limit_w).map(f64::from);
    let tctl_limit = snap.and_then(|s| s.tctl_limit_c).map(f64::from);

    ui.columns(2, |cols| {
        card_frame().show(&mut cols[0], |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Package Power (PPT)").strong());
                if let Some(v) = snap.and_then(|s| s.ppt_value_w) {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("{v:.1} W")).monospace().strong().color(ACCENT_CYAN));
                    });
                }
            });
            ui.add_space(4.0);
            Plot::new("ppt_plot")
                .height(185.0)
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .allow_boxed_zoom(false)
                .include_y(0.0)
                .y_axis_label("W")
                .x_axis_label("seconds ago")
                .show(ui, |plot_ui| {
                    plot_ui.line(
                        Line::new(ppt_points)
                            .name("PPT (W)")
                            .color(ACCENT_CYAN)
                            .width(2.0_f32),
                    );
                    if let Some(lim) = ppt_limit {
                        plot_ui.hline(
                            HLine::new(lim)
                                .name("PPT Limit")
                                .color(ACCENT_CRIMSON.linear_multiply(0.7))
                                .style(egui_plot::LineStyle::Dashed { length: 6.0 }),
                        );
                    }
                });
        });

        card_frame().show(&mut cols[1], |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("CPU Temperature (Tctl)").strong());
                if let Some(v) = snap.and_then(|s| s.tctl_value_c) {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(RichText::new(format!("{v:.1} °C")).monospace().strong().color(ACCENT_AMBER));
                    });
                }
            });
            ui.add_space(4.0);
            Plot::new("tctl_plot")
                .height(185.0)
                .allow_drag(false)
                .allow_zoom(false)
                .allow_scroll(false)
                .allow_boxed_zoom(false)
                .include_y(20.0)
                .include_y(100.0)
                .y_axis_label("°C")
                .x_axis_label("seconds ago")
                .show(ui, |plot_ui| {
                    plot_ui.line(
                        Line::new(tctl_points)
                            .name("Tctl (°C)")
                            .color(ACCENT_AMBER)
                            .width(2.0_f32),
                    );
                    if let Some(lim) = tctl_limit {
                        plot_ui.hline(
                            HLine::new(lim)
                                .name("Tctl Limit")
                                .color(ACCENT_CRIMSON.linear_multiply(0.7))
                                .style(egui_plot::LineStyle::Dashed { length: 6.0 }),
                        );
                    }
                });
        });
    });

    ui.add_space(12.0);

    // Row 3: Clocks & Voltage Rails cards
    ui.columns(2, |cols| {
        card_frame().show(&mut cols[0], |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("Fabric & Memory Clocks").strong());
                if let (Some(uclk), Some(mclk)) = (
                    snap.and_then(|s| s.uclk_mhz),
                    snap.and_then(|s| s.mclk_mhz),
                ) {
                    let ratio = if (uclk - mclk).abs() < 25.0 {
                        "UCLK:MCLK 1:1"
                    } else if (uclk * 2.0 - mclk).abs() < 50.0 {
                        "UCLK:MCLK 1:2"
                    } else {
                        "Async"
                    };
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        pill(
                            ui,
                            ratio,
                            ACCENT_CYAN,
                            Color32::from_rgba_unmultiplied(56, 189, 248, 24),
                        );
                    });
                }
            });
            ui.add_space(8.0);
            egui::Grid::new("clocks_grid")
                .num_columns(2)
                .striped(true)
                .min_col_width(140.0)
                .show(ui, |ui| {
                    metric_row(ui, "FCLK (Infinity Fabric)", snap.and_then(|s| s.fclk_mhz), "MHz", 0);
                    metric_row(ui, "UCLK (Memory Controller)", snap.and_then(|s| s.uclk_mhz), "MHz", 0);
                    metric_row(ui, "MCLK (Memory Clock)", snap.and_then(|s| s.mclk_mhz), "MHz", 0);
                });
        });

        card_frame().show(&mut cols[1], |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("Voltage Rails (Read-Only)").strong());
            ui.add_space(8.0);
            egui::Grid::new("voltages_grid")
                .num_columns(2)
                .striped(true)
                .min_col_width(140.0)
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
                let fill = if frac >= 0.90 {
                    ACCENT_CRIMSON
                } else if frac >= 0.75 {
                    ACCENT_AMBER
                } else {
                    accent
                };
                ui.label(
                    RichText::new(format!("{val:.1} {unit}"))
                        .size(20.0)
                        .strong()
                        .monospace(),
                );
                ui.add_space(4.0);
                ui.add(
                    egui::ProgressBar::new(frac)
                        .fill(fill)
                        .text(format!("{:.0}% of {:.0} {unit}", frac * 100.0, lim)),
                );
            }
            (Some(val), _) => {
                ui.label(
                    RichText::new(format!("{val:.1} {unit}"))
                        .size(20.0)
                        .strong()
                        .monospace(),
                );
                ui.add_space(4.0);
                ui.label(RichText::new("Limit N/A").small().color(TEXT_MUTED));
            }
            _ => {
                ui.label(
                    RichText::new("—")
                        .size(20.0)
                        .strong()
                        .monospace()
                        .color(TEXT_MUTED),
                );
                ui.add_space(4.0);
                ui.add(egui::ProgressBar::new(0.0).text("No data"));
            }
        }
    });
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
    ui.heading("Curve Optimizer");
    ui.label(
        RichText::new(
            "Adjust voltage-frequency curve margins safely. Every write runs --dry-run first and verifies readback.",
        )
        .small()
        .color(TEXT_MUTED),
    );
    ui.add_space(10.0);

    let Some(hw) = app.hw.clone() else {
        render_no_hardware_placeholder(ui);
        return;
    };

    let Some(bounds) = hw.capabilities.co_bounds else {
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(format!(
                    "Curve Optimizer is not supported on {}.",
                    hw.codename.display_name()
                ))
                .strong()
                .color(ACCENT_AMBER),
            );
            ui.label("Zen 2 and unsupported platforms do not expose Curve Optimizer SMU opcodes.");
        });
        return;
    };

    let busy = app.runner.is_busy();

    // Safety bar + All-core card
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("All-Core Curve Optimizer").strong());
            pill(
                ui,
                &format!(
                    "Bounds {}..=+{} · Soft floor {}",
                    bounds.min, bounds.max, bounds.soft_min
                ),
                ACCENT_CYAN,
                Color32::from_rgba_unmultiplied(56, 189, 248, 22),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if hw.capabilities.get_co_per_core.is_some()
                    && ui
                        .add_enabled(!busy, egui::Button::new("Read Current CO (pkexec)"))
                        .clicked()
                {
                    app.dispatch_cli(CliAction::CoGet, ctx);
                }
            });
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.label("All-Core Margin:");
            ui.add(
                egui::Slider::new(&mut app.co_editor.all_core, bounds.min..=bounds.max)
                    .show_value(true),
            );

            if ui.button("Copy to All Per-Core Sliders").clicked() {
                for slot in &mut app.co_editor.per_core {
                    *slot = app.co_editor.all_core;
                }
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
            ui.label(
                RichText::new(format!(
                    "Value {} is below conservative soft floor ({}); enable 'Allow Extreme' to proceed.",
                    app.co_editor.all_core, bounds.soft_min
                ))
                .small()
                .color(ACCENT_AMBER),
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
                    egui::Button::new("Preview (Dry-Run) & Apply All-Core")
                        .fill(ACCENT_CRIMSON),
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
                app.co_editor.all_core = 0;
                for slot in &mut app.co_editor.per_core {
                    *slot = 0;
                }
                app.co_editor.igpu = 0;
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
            app.co_editor
                .per_core
                .resize(hw.topology.physical_cores, 0);
        }

        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Per-Core Curve Optimizer (by CCD)").strong());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let any_below_soft = app
                        .co_editor
                        .per_core
                        .iter()
                        .any(|&v| v < bounds.soft_min);
                    let can_apply =
                        !busy && (!any_below_soft || app.co_editor.allow_extreme);
                    if ui
                        .add_enabled(
                            can_apply,
                            egui::Button::new("Preview (Dry-Run) & Apply Per-Core")
                                .fill(ACCENT_CRIMSON),
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
                });
            });

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
                                            RichText::new("extreme")
                                                .small()
                                                .color(ACCENT_AMBER),
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
                    pill(
                        ui,
                        &format!("Current: {cur:+}"),
                        ACCENT_EMERALD,
                        Color32::from_rgba_unmultiplied(34, 197, 94, 24),
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
    ui.heading("Curve Shaper (Zen 5)");
    ui.label(
        RichText::new(
            "15-point frequency × temperature voltage curve shaping (-50..=+30, soft floor -35).",
        )
        .small()
        .color(TEXT_MUTED),
    );
    ui.add_space(10.0);

    let Some(hw) = app.hw.clone() else {
        render_no_hardware_placeholder(ui);
        return;
    };

    if hw.capabilities.set_curve_shaper.is_none() {
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(
                RichText::new(format!(
                    "Curve Shaper is not supported on {}.",
                    hw.codename.display_name()
                ))
                .strong()
                .color(ACCENT_AMBER),
            );
            ui.add_space(4.0);
            ui.label(
                "Curve Shaper (5 frequency tiers × 3 temperature bands) requires a Zen 5 processor such as Granite Ridge (Ryzen 9000 series). zensmu disables these controls on non-Zen-5 CPUs so invalid SMU opcodes are never emitted.",
            );
        });
        return;
    }

    let busy = app.runner.is_busy();

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("5×3 Frequency / Temperature Matrix").strong());
            pill(
                ui,
                &format!("Range {SHAPER_MIN}..=+{SHAPER_MAX} · Soft floor {SHAPER_SOFT_MIN}"),
                ACCENT_CYAN,
                Color32::from_rgba_unmultiplied(56, 189, 248, 22),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if hw.capabilities.get_curve_shaper.is_some()
                    && ui
                        .add_enabled(!busy, egui::Button::new("Read Current Matrix (pkexec)"))
                        .clicked()
                {
                    app.dispatch_cli(CliAction::ShaperGet, ctx);
                }
            });
        });

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
                    egui::Button::new("Preview (Dry-Run) & Apply Matrix").fill(ACCENT_CRIMSON),
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
                app.shaper_editor.draft = CurveShaperState::default();
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
        Color32::from_rgb(203, 213, 225)
    };

    ui.horizontal(|ui| {
        ui.add(
            egui::DragValue::new(val)
                .range(SHAPER_MIN..=SHAPER_MAX)
                .speed(0.5),
        );
        ui.label(RichText::new(format!("{:+}", *val)).small().monospace().color(tint));
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
    ui.heading("Precision Boost Overdrive (PBO) Limits");
    ui.label(
        RichText::new(
            "Configure package power, VRM current, thermal ceiling (hard max 95°C), scalar, and Fmax boost limit.",
        )
        .small()
        .color(TEXT_MUTED),
    );
    ui.add_space(10.0);

    let Some(hw) = app.hw.clone() else {
        render_no_hardware_placeholder(ui);
        return;
    };

    let busy = app.runner.is_busy();
    let fused = app.pbo_status.as_ref().map(|s| s.fused.clone());
    let telem = app.latest_telemetry.clone();

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("PBO Controls").strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add_enabled(!busy, egui::Button::new("Read PBO Status & Fused Limits (pkexec)"))
                    .clicked()
                {
                    app.dispatch_cli(CliAction::PboGet, ctx);
                }
            });
        });

        ui.add_space(10.0);

        egui::Grid::new("pbo_controls_grid")
            .num_columns(4)
            .striped(true)
            .spacing([14.0, 10.0])
            .show(ui, |ui| {
                ui.label(RichText::new("Parameter").strong().color(TEXT_MUTED));
                ui.label(RichText::new("Target Value").strong().color(TEXT_MUTED));
                ui.label(RichText::new("Current Live").strong().color(TEXT_MUTED));
                ui.label(RichText::new("Fused Factory / Guard").strong().color(TEXT_MUTED));
                ui.end_row();

                // PPT
                if hw.capabilities.set_ppt.is_some() {
                    ui.checkbox(&mut app.pbo_editor.ppt_enabled, "PPT Limit (W)");
                    ui.add_enabled(
                        app.pbo_editor.ppt_enabled,
                        egui::Slider::new(&mut app.pbo_editor.ppt_w, PPT_MIN_W..=PPT_MAX_W)
                            .suffix(" W"),
                    );
                    ui.label(
                        RichText::new(opt_f32(telem.as_ref().and_then(|t| t.ppt_limit_w), "W"))
                            .monospace(),
                    );
                    let fused_txt = fused
                        .as_ref()
                        .and_then(|f| f.ppt_w)
                        .map(|w| format!("Fused: {w} W · soft ≤ {PPT_SOFT_MAX_W} W"))
                        .unwrap_or_else(|| format!("Soft max ≤ {PPT_SOFT_MAX_W} W"));
                    ui.label(RichText::new(fused_txt).small().color(TEXT_MUTED));
                    ui.end_row();
                }

                // TDC
                if hw.capabilities.set_tdc.is_some() {
                    ui.checkbox(&mut app.pbo_editor.tdc_enabled, "TDC Limit (A)");
                    ui.add_enabled(
                        app.pbo_editor.tdc_enabled,
                        egui::Slider::new(&mut app.pbo_editor.tdc_a, TDC_MIN_A..=TDC_MAX_A)
                            .suffix(" A"),
                    );
                    ui.label(
                        RichText::new(opt_f32(telem.as_ref().and_then(|t| t.tdc_limit_a), "A"))
                            .monospace(),
                    );
                    let fused_txt = fused
                        .as_ref()
                        .and_then(|f| f.vdd_tdc_a)
                        .map(|a| format!("Fused VDD: {a} A · soft ≤ {TDC_SOFT_MAX_A} A"))
                        .unwrap_or_else(|| format!("Soft max ≤ {TDC_SOFT_MAX_A} A"));
                    ui.label(RichText::new(fused_txt).small().color(TEXT_MUTED));
                    ui.end_row();
                }

                // EDC
                if hw.capabilities.set_edc.is_some() {
                    ui.checkbox(&mut app.pbo_editor.edc_enabled, "EDC Limit (A)");
                    ui.add_enabled(
                        app.pbo_editor.edc_enabled,
                        egui::Slider::new(&mut app.pbo_editor.edc_a, EDC_MIN_A..=EDC_MAX_A)
                            .suffix(" A"),
                    );
                    ui.label(
                        RichText::new(opt_f32(telem.as_ref().and_then(|t| t.edc_limit_a), "A"))
                            .monospace(),
                    );
                    ui.label(
                        RichText::new(format!("Soft max ≤ {EDC_SOFT_MAX_A} A"))
                            .small()
                            .color(TEXT_MUTED),
                    );
                    ui.end_row();
                }

                // Tctl Max (Hard ceiling 95C)
                if hw.capabilities.set_tctl.is_some() {
                    ui.checkbox(&mut app.pbo_editor.tctl_enabled, "Tctl Thermal Max (°C)");
                    ui.add_enabled(
                        app.pbo_editor.tctl_enabled,
                        egui::Slider::new(&mut app.pbo_editor.tctl_c, TCTL_MIN_C..=TCTL_MAX_C)
                            .suffix(" °C"),
                    );
                    ui.label(
                        RichText::new(opt_f32(telem.as_ref().and_then(|t| t.tctl_limit_c), "°C"))
                            .monospace(),
                    );
                    ui.label(
                        RichText::new("Hard safety ceiling: 95 °C (non-overridable)")
                            .small()
                            .color(ACCENT_AMBER),
                    );
                    ui.end_row();
                }

                // PBO Scalar
                if hw.capabilities.set_pbo_scalar.is_some() {
                    ui.checkbox(&mut app.pbo_editor.scalar_enabled, "PBO Scalar (1–10x)");
                    ui.add_enabled(
                        app.pbo_editor.scalar_enabled,
                        egui::Slider::new(&mut app.pbo_editor.scalar, SCALAR_MIN..=SCALAR_MAX)
                            .suffix("x"),
                    );
                    let cur = app
                        .pbo_status
                        .as_ref()
                        .and_then(|s| s.scalar)
                        .map(|s| format!("{s:.1}x"))
                        .unwrap_or_else(|| "—".into());
                    ui.label(RichText::new(cur).monospace());
                    ui.label(
                        RichText::new(format!("Soft max ≤ {SCALAR_SOFT_MAX}x"))
                            .small()
                            .color(TEXT_MUTED),
                    );
                    ui.end_row();
                }

                // Fmax Boost Limit
                if hw.capabilities.set_fmax.is_some() {
                    ui.checkbox(&mut app.pbo_editor.fmax_enabled, "Fmax Boost Limit (MHz)");
                    ui.add_enabled(
                        app.pbo_editor.fmax_enabled,
                        egui::Slider::new(
                            &mut app.pbo_editor.fmax_mhz,
                            FMAX_MIN_MHZ..=FMAX_MAX_MHZ,
                        )
                        .step_by(25.0)
                        .suffix(" MHz"),
                    );
                    let cur = app
                        .pbo_status
                        .as_ref()
                        .and_then(|s| s.fmax_mhz)
                        .map(|m| format!("{m} MHz"))
                        .unwrap_or_else(|| "—".into());
                    ui.label(RichText::new(cur).monospace());
                    ui.label(
                        RichText::new("Soft delta ≤ +200 MHz above current")
                            .small()
                            .color(TEXT_MUTED),
                    );
                    ui.end_row();
                }
            });

        ui.add_space(10.0);
        ui.checkbox(
            &mut app.pbo_editor.allow_extreme,
            "Allow Extreme PBO values (above conservative soft limits; 95°C Tctl ceiling still enforced)",
        );

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            let can_apply = !busy && app.pbo_editor.has_any_enabled();
            if ui
                .add_enabled(
                    can_apply,
                    egui::Button::new("Preview (Dry-Run) & Apply PBO").fill(ACCENT_CRIMSON),
                )
                .clicked()
            {
                app.dispatch_cli(
                    CliAction::PboSet {
                        req: app.pbo_editor.build_request(),
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
                if let Some(w) = status.fused.ppt_w {
                    pill(
                        ui,
                        &format!("Fused PPT: {w} W"),
                        ACCENT_CYAN,
                        Color32::from_rgba_unmultiplied(56, 189, 248, 20),
                    );
                }
                if let Some(a) = status.fused.soc_tdc_a {
                    pill(
                        ui,
                        &format!("Fused SoC TDC: {a} A"),
                        ACCENT_CYAN,
                        Color32::from_rgba_unmultiplied(56, 189, 248, 20),
                    );
                }
                if let Some(w) = status.fused.apu_slow_w {
                    pill(
                        ui,
                        &format!("Fused Slow: {w} W"),
                        ACCENT_CYAN,
                        Color32::from_rgba_unmultiplied(56, 189, 248, 20),
                    );
                }
            });
        });
    }
}

fn opt_f32(v: Option<f32>, unit: &str) -> String {
    match v {
        Some(x) => format!("{x:.0} {unit}"),
        None => "—".into(),
    }
}

fn flag_pill(ui: &mut Ui, label: &str, val: Option<bool>) {
    match val {
        Some(true) => pill(
            ui,
            &format!("{label}: Yes"),
            ACCENT_EMERALD,
            Color32::from_rgba_unmultiplied(34, 197, 94, 24),
        ),
        Some(false) => pill(
            ui,
            &format!("{label}: No"),
            TEXT_MUTED,
            Color32::from_rgb(30, 36, 48),
        ),
        None => {}
    }
}

/// Tab 5: Declarative TOML Profiles, Rollback/Reset, Boot-Loop Guard, and Driver Safety Status.
pub fn render_profiles_and_safety(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    ui.heading("Profiles & Safety");
    ui.label(
        RichText::new(
            "Load, validate, diff, export, and apply declarative TOML profiles, or manage rollback and driver safety guards.",
        )
        .small()
        .color(TEXT_MUTED),
    );
    ui.add_space(10.0);

    let busy = app.runner.is_busy();
    let has_hw = app.hw.is_some();

    // TOML Profile Card
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.label(RichText::new("Declarative TOML Profile").strong());
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            ui.label("Path:");
            ui.add(
                egui::TextEdit::singleline(&mut app.profile_editor.path)
                    .desired_width(340.0)
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
                    egui::Button::new("Preview (Dry-Run) & Apply").fill(ACCENT_CRIMSON),
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
            .max_height(240.0)
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

            ui.horizontal(|ui| {
                ui.label("Last-change snapshot:");
                if app.profile_editor.has_last_state {
                    pill(
                        ui,
                        "Present (/run/zensmu/last-state.json)",
                        ACCENT_EMERALD,
                        Color32::from_rgba_unmultiplied(34, 197, 94, 24),
                    );
                } else {
                    pill(
                        ui,
                        "None yet this boot",
                        TEXT_MUTED,
                        Color32::from_rgb(30, 36, 48),
                    );
                }
            });

            ui.horizontal(|ui| {
                ui.label("Boot baseline snapshot:");
                if app.profile_editor.has_boot_baseline {
                    pill(
                        ui,
                        "Captured (/run/zensmu/boot-baseline.json)",
                        ACCENT_EMERALD,
                        Color32::from_rgba_unmultiplied(34, 197, 94, 24),
                    );
                } else {
                    pill(
                        ui,
                        "Captured on first write",
                        TEXT_MUTED,
                        Color32::from_rgb(30, 36, 48),
                    );
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
            ui.horizontal(|ui| {
                ui.label("Boot-Loop Crash Guard:");
                if app.profile_editor.boot_guard_tripped {
                    pill(
                        ui,
                        "TRIPPED — auto-apply halted",
                        ACCENT_CRIMSON,
                        Color32::from_rgba_unmultiplied(224, 90, 71, 36),
                    );
                } else {
                    pill(
                        ui,
                        "Armed & Clear",
                        ACCENT_EMERALD,
                        Color32::from_rgba_unmultiplied(34, 197, 94, 24),
                    );
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
        .default_width(760.0)
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

            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close_modal = true;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let confirm_label = if app.is_root {
                        "Confirm & Apply"
                    } else {
                        "Confirm & Apply (pkexec)"
                    };
                    if ui
                        .add_enabled(
                            !modal.ops.is_empty() && !app.runner.is_busy(),
                            egui::Button::new(confirm_label).fill(ACCENT_CRIMSON),
                        )
                        .clicked()
                    {
                        confirm_action = Some(modal.live_action.clone());
                        close_modal = true;
                    }
                });
            });
        });

    if close_modal {
        app.pending_modal = None;
    }
    if let Some(action) = confirm_action {
        app.dispatch_cli(action, ctx);
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
