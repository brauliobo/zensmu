//! Right-hand inspector cards for Tab 4 (selected param, persistence, safety actions).

use super::super::super::app::App;
use super::super::super::format::{badge_span, styled_block};
use super::super::super::palette;
use super::super::super::types::{format_board_cap, param_unit, FlashKind, TuneSection};
use super::format::{
    estimate_wrapped_lines, format_live_param_display, format_tune_param_label, format_tune_value,
    param_base_bounds_summary,
};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Padding, Paragraph, Wrap};
use ratatui::Frame;
use zensmu_core::hw::identity::HardwareContext;

pub(super) fn draw_tune_inspector_pane(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    hw: &HardwareContext,
) {
    if let Some((ref msg, kind)) = app.last_alert {
        let inner_w = area.width.saturating_sub(4).max(12) as usize;
        let alert_h = (estimate_wrapped_lines(msg, inner_w).clamp(1, 5) as u16) + 2;
        let constraints = if area.height >= 26 {
            [
                Constraint::Length(alert_h),
                Constraint::Length(10),
                Constraint::Length(7),
                Constraint::Min(5),
            ]
        } else {
            [
                Constraint::Length(alert_h.min(area.height / 2).max(3)),
                Constraint::Percentage(36),
                Constraint::Percentage(32),
                Constraint::Percentage(32),
            ]
        };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(area);

        draw_last_event_card(frame, chunks[0], msg, kind);
        draw_selected_param_card(frame, chunks[1], app, hw);
        draw_boot_persistence_card(frame, chunks[2], app);
        draw_safety_actions_card(frame, chunks[3], app);
        return;
    }

    let persist_h = if app.persist_status.boot_guard_tripped {
        9
    } else {
        8
    };
    let constraints = if area.height >= 26 {
        [
            Constraint::Length(11),
            Constraint::Length(persist_h),
            Constraint::Min(8),
        ]
    } else {
        [
            Constraint::Percentage(36),
            Constraint::Percentage(32),
            Constraint::Percentage(32),
        ]
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(area);

    draw_selected_param_card(frame, chunks[0], app, hw);
    draw_boot_persistence_card(frame, chunks[1], app);
    draw_safety_actions_card(frame, chunks[2], app);
}

pub(super) fn draw_last_event_card(frame: &mut Frame, area: Rect, msg: &str, kind: FlashKind) {
    let accent = match kind {
        FlashKind::Warn => palette::YELLOW,
        FlashKind::Error => palette::RED,
        FlashKind::Success => palette::GREEN,
        FlashKind::Info => palette::TEAL,
    };
    let block = styled_block(" Last Event ", accent)
        .border_style(Style::default().fg(accent))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let para = Paragraph::new(Line::from(Span::styled(
        msg,
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    )))
    .wrap(Wrap { trim: true });
    frame.render_widget(para, inner);
}

pub(super) fn draw_selected_param_card(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    hw: &HardwareContext,
) {
    let block = styled_block(" Selected Parameter ", palette::CYAN).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(param) = app.selected_tune_param(hw) else {
        return;
    };
    let label = format_tune_param_label(param, hw);
    let live = format_live_param_display(app, param);
    let staged = app
        .staged
        .get(&param)
        .map(|v| format_tune_value(param, *v))
        .unwrap_or_else(|| "(unchanged)".to_string());
    let (fine, coarse) = App::param_steps(param);
    let base_bounds = param_base_bounds_summary(app, param, hw);
    let board_cap = app.board_caps.get(&param).copied();

    let mut bounds_spans = vec![
        Span::styled("Bounds:  ", Style::default().fg(palette::MUTED)),
        Span::styled(base_bounds, Style::default().fg(palette::FG)),
    ];
    if let Some(cap) = board_cap {
        bounds_spans.push(Span::styled(
            format!(" · {}", format_board_cap(param, cap)),
            Style::default()
                .fg(palette::ORANGE)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let mut lines = vec![
        Line::from(Span::styled(
            label,
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled("Section: ", Style::default().fg(palette::MUTED)),
            Span::styled(param.section().title(), Style::default().fg(palette::TEAL)),
        ]),
        Line::from(vec![
            Span::styled("Live:    ", Style::default().fg(palette::MUTED)),
            Span::styled(
                live,
                Style::default()
                    .fg(palette::FG)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Staged:  ", Style::default().fg(palette::MUTED)),
            Span::styled(
                staged,
                Style::default().fg(if app.staged.contains_key(&param) {
                    palette::ORANGE
                } else {
                    palette::DIM
                }),
            ),
        ]),
        Line::from(bounds_spans),
        Line::from(vec![
            Span::styled("Step:    ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format!("±{fine} (Shift/H/L/PgUp/Dn: ±{coarse})"),
                Style::default().fg(palette::MUTED),
            ),
        ]),
    ];

    if let Some(sv) = app.staged.get(&param).copied() {
        if app.param_exceeds_soft_limit(param, sv, hw) && !app.allow_extreme {
            lines.push(Line::from(Span::styled(
                "! Exceeds soft limit — press 'x' for Allow Extreme",
                Style::default()
                    .fg(palette::YELLOW)
                    .add_modifier(Modifier::BOLD),
            )));
        }
    }

    if let Some(cap) = board_cap {
        let unit = param_unit(param);
        let cap_label = if unit.is_empty() {
            format!("Board cap ≤{cap}")
        } else {
            format!("Board cap ≤{cap} {unit}")
        };
        lines.push(Line::from(Span::styled(
            format!("! {cap_label} (check BIOS PBO Limits)"),
            Style::default()
                .fg(palette::YELLOW)
                .add_modifier(Modifier::BOLD),
        )));
    } else if matches!(param.section(), TuneSection::Pbo | TuneSection::Apu) {
        lines.push(Line::from(Span::styled(
            crate::output::format_board_vrm_note(&app.board_info),
            Style::default().fg(palette::MUTED),
        )));
    }

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), inner);
}

pub(super) fn draw_boot_persistence_card(frame: &mut Frame, area: Rect, app: &App) {
    let ps = &app.persist_status;
    let persist_accent = if ps.boot_guard_tripped {
        palette::RED
    } else if ps.service_enabled {
        palette::GREEN
    } else {
        palette::TEAL
    };
    let block = styled_block(" Boot Persistence ", persist_accent).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let (profile_text, profile_color) = if !ps.active_profile_exists {
        (
            format!("missing ({})", ps.active_profile_path),
            palette::MUTED,
        )
    } else {
        match ps.active_profile_valid {
            Some(true) => {
                let cn = ps
                    .active_profile_codename
                    .map(|c| c.display_name())
                    .unwrap_or("valid");
                (format!("valid ({cn})"), palette::GREEN)
            }
            Some(false) => ("invalid (re-save with P)".to_string(), palette::RED),
            None => ("present".to_string(), palette::CYAN),
        }
    };

    let (svc_text, svc_color) = if ps.service_enabled {
        (format!("enabled ({})", ps.service_name), palette::GREEN)
    } else if !ps.service_installed && ps.service_state == "not-found" {
        (
            format!("not installed ({})", ps.service_name),
            palette::YELLOW,
        )
    } else {
        (format!("disabled ({})", ps.service_name), palette::MUTED)
    };

    let resume_span = if ps.resume_hook_installed {
        Span::styled("installed", Style::default().fg(palette::GREEN))
    } else {
        Span::styled("none", Style::default().fg(palette::MUTED))
    };
    let guard_span = if ps.boot_guard_tripped {
        Span::styled(
            "TRIPPED!",
            Style::default()
                .fg(palette::RED)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("clean", Style::default().fg(palette::GREEN))
    };

    let toggle_desc = if ps.service_enabled {
        "Disable boot service"
    } else {
        "Enable boot service"
    };

    let mut persist_lines = vec![
        Line::from(vec![
            Span::styled("Profile: ", Style::default().fg(palette::MUTED)),
            Span::styled(
                profile_text,
                Style::default()
                    .fg(profile_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Service: ", Style::default().fg(palette::MUTED)),
            Span::styled(
                svc_text,
                Style::default().fg(svc_color).add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Resume:  ", Style::default().fg(palette::MUTED)),
            resume_span,
            Span::styled("  ·  Guard: ", Style::default().fg(palette::MUTED)),
            guard_span,
        ]),
    ];
    if inner.height > 5 {
        persist_lines.push(Line::from(""));
    }
    persist_lines.push(tune_key_line("P", "Persist live & enable service"));
    persist_lines.push(tune_key_line("E", toggle_desc));
    if ps.boot_guard_tripped {
        persist_lines.push(tune_key_line("C", "Clear boot guard"));
    }

    frame.render_widget(
        Paragraph::new(persist_lines).wrap(Wrap { trim: true }),
        inner,
    );
}

pub(super) fn draw_safety_actions_card(frame: &mut Frame, area: Rect, app: &App) {
    let block =
        styled_block(" Safety & Quick Actions ", palette::PURPLE).padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let extreme_span = if app.allow_extreme {
        badge_span(" ON (hard bounds) ", palette::ORANGE)
    } else {
        Span::styled(" OFF (soft bounds)", Style::default().fg(palette::GREEN))
    };

    let write_span = if app.can_write() {
        Span::styled(
            "READY (root)",
            Style::default()
                .fg(palette::GREEN)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled("LOCKED (sudo)", Style::default().fg(palette::YELLOW))
    };

    let mut action_lines = vec![
        Line::from(vec![
            Span::styled("Writes:  ", Style::default().fg(palette::MUTED)),
            write_span,
            Span::styled(" · Ext:", Style::default().fg(palette::MUTED)),
            extreme_span,
        ]),
        Line::from(vec![
            Span::styled("Staged:  ", Style::default().fg(palette::MUTED)),
            Span::styled(
                format!("{} pending change(s)", app.staged_count()),
                Style::default()
                    .fg(if app.staged_count() > 0 {
                        palette::ORANGE
                    } else {
                        palette::DIM
                    })
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        tune_key_line("a / Enter", "Dry-run preview & apply"),
        tune_key_line("0 / Bksp", "Reset param to 0 / live"),
        tune_key_line("u / x", "Undo all · Toggle Extreme"),
        tune_key_line("R / B / Z", "Rollback · Baseline · CO=0"),
        tune_key_line("s", "Save live TOML profile"),
    ];
    if inner.height >= 8 {
        action_lines.push(Line::from(Span::styled(
            crate::output::format_board_vrm_note(&app.board_info),
            Style::default().fg(palette::DIM),
        )));
    }

    frame.render_widget(
        Paragraph::new(action_lines).wrap(Wrap { trim: true }),
        inner,
    );
}

pub(super) fn tune_key_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("{key:<10}"),
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(palette::FG)),
    ])
}
