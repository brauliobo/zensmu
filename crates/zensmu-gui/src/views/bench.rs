//! Unprivileged per-core Curve Optimizer stability tester (Core-Cycler) card.

use super::theme::*;
use super::widgets::{card_frame, cta_button, grid_header_row, split_or_wrap_row, tinted_pill};
use crate::app::ZensmuApp;
use eframe::egui::{self, RichText, Ui};
use zensmu_core::bench::{BenchWorkload, CoreBenchStatus};

fn format_bench_workload_label(workload: BenchWorkload) -> &'static str {
    match workload {
        BenchWorkload::Mixed => "Mixed",
        BenchWorkload::Integer => "Integer",
        BenchWorkload::FloatingPoint => "FP",
        BenchWorkload::Cache => "Cache",
    }
}

pub(super) fn render_core_cycler_card(app: &mut ZensmuApp, ui: &mut Ui, ctx: &egui::Context) {
    let running = app.bench.running;
    let mut start_clicked = false;
    let mut stop_clicked = false;

    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        split_or_wrap_row(
            ui,
            "co_core_cycler_header",
            |ui| {
                ui.label(RichText::new("Core-Cycler Stability Tester").strong());
                if running {
                    tinted_pill(ui, "RUNNING", ACCENT_CYAN, PILL_ALPHA_CYAN);
                } else if let Some(report) = &app.bench.last_report {
                    if report.cancelled {
                        tinted_pill(ui, "CANCELLED", ACCENT_AMBER, PILL_ALPHA_AMBER);
                    } else if report.passed {
                        tinted_pill(ui, "PASSED", ACCENT_EMERALD, PILL_ALPHA_EMERALD);
                    } else {
                        tinted_pill(ui, "FAILED", ACCENT_CRIMSON, PILL_ALPHA_CRIMSON);
                    }
                } else {
                    tinted_pill(
                        ui,
                        "Unprivileged · no pkexec",
                        ACCENT_EMERALD,
                        PILL_ALPHA_EMERALD,
                    );
                }
            },
            |ui| {
                if running {
                    if ui
                        .add(cta_button("Stop Test", CTA_CRIMSON))
                        .on_hover_text("Stop the running per-core stability test cleanly")
                        .clicked()
                    {
                        stop_clicked = true;
                    }
                } else if ui
                    .add_enabled(
                        app.hw.is_some(),
                        cta_button("Run Core-Cycler", CTA_EMERALD),
                    )
                    .on_hover_text(
                        "Pin deterministic self-checking stress threads core-by-core without root",
                    )
                    .clicked()
                {
                    start_clicked = true;
                }
            },
        );

        ui.add_space(6.0);
        ui.label(
            RichText::new(
                "Cycles a deterministic self-verifying stress worker across every physical core to catch unstable Curve Optimizer undervolts (compute errors & MCE/EDAC deltas) without requiring root.",
            )
            .small()
            .color(TEXT_MUTED),
        );

        ui.add_space(8.0);
        ui.add_enabled_ui(!running, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Workload:").small().color(TEXT_MUTED));
                egui::ComboBox::from_id_salt("co_bench_workload_combo")
                    .width(96.0)
                    .selected_text(format_bench_workload_label(app.bench.workload))
                    .show_ui(ui, |ui| {
                        for workload in [
                            BenchWorkload::Mixed,
                            BenchWorkload::Integer,
                            BenchWorkload::FloatingPoint,
                            BenchWorkload::Cache,
                        ] {
                            ui.selectable_value(
                                &mut app.bench.workload,
                                workload,
                                format_bench_workload_label(workload),
                            );
                        }
                    });

                ui.label(RichText::new("Per-Core:").small().color(TEXT_MUTED));
                egui::ComboBox::from_id_salt("co_bench_duration_combo")
                    .width(74.0)
                    .selected_text(format!("{}s", app.bench.duration_per_core_secs))
                    .show_ui(ui, |ui| {
                        for secs in [5u64, 15, 30, 60] {
                            ui.selectable_value(
                                &mut app.bench.duration_per_core_secs,
                                secs,
                                format!("{secs}s"),
                            );
                        }
                    });

                ui.label(RichText::new("Passes:").small().color(TEXT_MUTED));
                egui::ComboBox::from_id_salt("co_bench_passes_combo")
                    .width(60.0)
                    .selected_text(app.bench.passes.to_string())
                    .show_ui(ui, |ui| {
                        for passes in [1u32, 2, 3] {
                            ui.selectable_value(
                                &mut app.bench.passes,
                                passes,
                                passes.to_string(),
                            );
                        }
                    });

                ui.checkbox(&mut app.bench.stop_on_error, "Stop on error");
            });
        });

        if running {
            ui.add_space(8.0);
            if let Some(p) = &app.bench.progress {
                let total_steps =
                    (p.total_cores.max(1) as f64) * (p.total_passes.max(1) as f64);
                let completed_steps =
                    ((p.pass.saturating_sub(1) as usize) * p.total_cores + p.core_pos) as f64;
                let core_frac = if p.target_secs > 0 {
                    (p.elapsed_secs / (p.target_secs as f64)).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let overall_frac = ((completed_steps + core_frac) / total_steps).clamp(0.0, 1.0)
                    as f32;

                let mut readout = format!(
                    "Pass {}/{} · Core C{} ({}/{}) · {:.1}s / {}s",
                    p.pass,
                    p.total_passes,
                    p.core_index,
                    p.core_pos + 1,
                    p.total_cores,
                    p.elapsed_secs,
                    p.target_secs
                );
                if let Some(mhz) = p.latest_freq_mhz {
                    readout.push_str(&format!(" · {mhz:.0} MHz"));
                }
                if let Some(temp_c) = p.latest_temp_c {
                    readout.push_str(&format!(" · {temp_c:.1} °C"));
                }

                split_or_wrap_row(
                    ui,
                    "co_bench_progress_caption",
                    |ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new(&readout)
                                .small()
                                .monospace()
                                .color(TEXT_PRIMARY),
                        );
                    },
                    |ui| {
                        ui.label(
                            RichText::new(format!("{:.0}%", overall_frac * 100.0))
                                .small()
                                .strong()
                                .monospace()
                                .color(ACCENT_CYAN),
                        );
                    },
                );
                ui.add_space(4.0);
                ui.add(
                    egui::ProgressBar::new(overall_frac)
                        .desired_height(10.0)
                        .corner_radius(egui::CornerRadius::same(4))
                        .fill(ACCENT_CYAN),
                );
            } else {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        RichText::new("Initializing Core-Cycler worker…")
                            .small()
                            .color(TEXT_MUTED),
                    );
                });
            }
        }

        if let Some(err) = &app.bench.error {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("✗ {err}"))
                    .small()
                    .color(ACCENT_CRIMSON),
            );
        }

        if let Some(report) = &app.bench.last_report {
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(6.0);

            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(format!(
                        "Last Run · {} · {} pass(es) · {:.1}s total · {} core result(s)",
                        format_bench_workload_label(report.workload),
                        report.passes_completed,
                        report.total_duration_secs,
                        report.results.len()
                    ))
                    .small()
                    .color(TEXT_MUTED),
                );
            });

            if !report.dmesg_readable {
                ui.add_space(4.0);
                ui.label(
                    RichText::new(
                        "Kernel dmesg is restricted unprivileged — run `sudo zensmu test` to also scan kernel dmesg for WHEA/MCE logs.",
                    )
                    .small()
                    .color(ACCENT_AMBER),
                );
            }

            if !report.results.is_empty() {
                let multi_pass =
                    report.passes_completed > 1 || report.results.iter().any(|r| r.pass > 1);
                ui.add_space(6.0);
                egui::Grid::new("co_bench_results_grid")
                    .num_columns(5)
                    .striped(true)
                    .spacing([14.0, 5.0])
                    .show(ui, |ui| {
                        grid_header_row(
                            ui,
                            &["Core / Result", "Peak Clock", "Peak Temp", "MCE Delta", "Detail"],
                        );

                        for res in &report.results {
                            let (status_tag, status_color) = match res.status {
                                CoreBenchStatus::Passed => ("PASS", ACCENT_EMERALD),
                                CoreBenchStatus::FailedMce => ("FAIL (MCE)", ACCENT_CRIMSON),
                                CoreBenchStatus::FailedCompute => {
                                    ("FAIL (COMPUTE)", ACCENT_CRIMSON)
                                }
                                CoreBenchStatus::Cancelled => ("CANCELLED", ACCENT_AMBER),
                            };
                            let core_label = if multi_pass {
                                format!("P{} C{} {status_tag}", res.pass, res.core_index)
                            } else {
                                format!("C{} {status_tag}", res.core_index)
                            };
                            ui.label(
                                RichText::new(core_label)
                                    .small()
                                    .strong()
                                    .monospace()
                                    .color(status_color),
                            );

                            let freq_text = res
                                .peak_freq_mhz
                                .map(|mhz| format!("{mhz:.0} MHz"))
                                .unwrap_or_else(|| "—".into());
                            ui.label(
                                RichText::new(freq_text)
                                    .small()
                                    .monospace()
                                    .color(TEXT_PRIMARY),
                            );

                            let temp_text = res
                                .peak_temp_c
                                .map(|c| format!("{c:.1} °C"))
                                .unwrap_or_else(|| "—".into());
                            ui.label(
                                RichText::new(temp_text)
                                    .small()
                                    .monospace()
                                    .color(TEXT_PRIMARY),
                            );

                            let has_mce = res.mce_delta_corrected > 0
                                || res.mce_delta_uncorrected > 0
                                || !res.new_mce_lines.is_empty();
                            let mce_text = if !res.new_mce_lines.is_empty() {
                                format!(
                                    "CE +{} · UE +{} · {} log",
                                    res.mce_delta_corrected,
                                    res.mce_delta_uncorrected,
                                    res.new_mce_lines.len()
                                )
                            } else {
                                format!(
                                    "CE +{} · UE +{}",
                                    res.mce_delta_corrected, res.mce_delta_uncorrected
                                )
                            };
                            ui.label(
                                RichText::new(mce_text)
                                    .small()
                                    .monospace()
                                    .color(if has_mce { ACCENT_CRIMSON } else { TEXT_MUTED }),
                            );

                            let detail_text = match &res.error {
                                Some(err) => err.clone(),
                                None => format!("{:.1}s · {} iter", res.duration_secs, res.iterations),
                            };
                            ui.label(
                                RichText::new(detail_text)
                                    .small()
                                    .color(if res.error.is_some() {
                                        ACCENT_CRIMSON
                                    } else {
                                        TEXT_MUTED
                                    }),
                            );
                            ui.end_row();
                        }
                    });
            }
        }
    });

    if start_clicked {
        app.start_bench(ctx);
    }
    if stop_clicked {
        app.stop_bench();
    }
}
