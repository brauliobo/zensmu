//! Dry-run confirmation modal overlay and keybinding help popup.

use super::super::app::App;
use super::super::format::{badge_span, modal_frame_block};
use super::super::palette;
use super::super::types::ConfirmModal;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Cell, Clear, Paragraph, Row, Table, Wrap};
use ratatui::Frame;

// ---------------------------------------------------------------------------
// Dry-Run Confirmation Modal Overlay
// ---------------------------------------------------------------------------

pub(super) fn draw_confirm_modal(frame: &mut Frame, root: Rect, app: &App, modal: &ConfirmModal) {
    let pct_x = if root.width < 92 { 94 } else { 84 };
    let pct_y = if root.height < 28 { 86 } else { 76 };
    let popup = centered_rect(pct_x, pct_y, root);
    frame.render_widget(Clear, popup);

    let border_color = if !app.can_write() {
        palette::YELLOW
    } else if modal.allow_extreme {
        palette::ORANGE
    } else {
        palette::CYAN
    };

    let block = modal_frame_block(modal.title.as_str(), border_color);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    if inner.height < 5 || inner.width < 20 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(inner);

    draw_confirm_modal_header(frame, chunks[0], app, modal);

    let visible_items = chunks[1].height.saturating_sub(1).max(1) as usize;
    let total_items = modal.item_count();
    let start = modal.scroll.min(total_items.saturating_sub(1));

    if !modal.steps.is_empty() {
        draw_confirm_modal_steps(frame, chunks[1], modal, start, visible_items);
    } else {
        draw_confirm_modal_ops_table(frame, chunks[1], modal, start, visible_items);
    }

    draw_confirm_modal_footer(frame, chunks[2], app, start, visible_items, total_items);
}

pub(super) fn draw_confirm_modal_header(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    modal: &ConfirmModal,
) {
    let mut sub_spans = vec![Span::styled(
        modal.subtitle.as_str(),
        Style::default().fg(palette::FG),
    )];
    if modal.allow_extreme {
        sub_spans.push(Span::raw(" "));
        sub_spans.push(badge_span(" EXTREME ", palette::ORANGE));
    }
    if !app.can_write() {
        sub_spans.push(Span::raw(" "));
        sub_spans.push(badge_span(" UNPRIVILEGED ", palette::YELLOW));
    }

    let safety_note = if modal.action.is_persist_action() {
        "Boot persistence & systemd control · validated before writing · no SMU registers modified"
    } else {
        "Pre-change snapshot saved before writes · flock + SMN RSP + readback verification"
    };
    let sub_lines = vec![
        Line::from(sub_spans),
        Line::from(Span::styled(safety_note, Style::default().fg(palette::DIM))),
    ];
    frame.render_widget(Paragraph::new(sub_lines), area);
}

pub(super) fn draw_confirm_modal_steps(
    frame: &mut Frame,
    area: Rect,
    modal: &ConfirmModal,
    start: usize,
    visible_items: usize,
) {
    let mut step_lines: Vec<Line<'_>> = vec![Line::from(Span::styled(
        "Planned System Actions:",
        Style::default()
            .fg(palette::MUTED)
            .add_modifier(Modifier::BOLD),
    ))];
    for (step_label, detail) in modal.steps.iter().skip(start).take(visible_items) {
        step_lines.push(Line::from(vec![
            Span::styled(
                format!("  {step_label:<11} "),
                Style::default()
                    .fg(palette::CYAN)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(detail.as_str(), Style::default().fg(palette::FG)),
        ]));
    }
    frame.render_widget(Paragraph::new(step_lines).wrap(Wrap { trim: false }), area);
}

pub(super) fn draw_confirm_modal_ops_table(
    frame: &mut Frame,
    area: Rect,
    modal: &ConfirmModal,
    start: usize,
    visible_items: usize,
) {
    let op_rows: Vec<Row<'_>> = modal
        .ops
        .iter()
        .enumerate()
        .skip(start)
        .take(visible_items)
        .map(|(idx, op)| {
            let cur = op.current.as_deref().unwrap_or("—");
            let change = format!("{cur} → {}", op.target);
            Row::new(vec![
                Cell::from(format!("{}", idx + 1)).style(Style::default().fg(palette::DIM)),
                Cell::from(op.mailbox.as_str()).style(Style::default().fg(palette::TEAL)),
                Cell::from(format!("0x{:02X}", op.opcode)).style(
                    Style::default()
                        .fg(palette::CYAN)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(format!("0x{:08X}", op.args[0]))
                    .style(Style::default().fg(palette::MUTED)),
                Cell::from(change).style(
                    Style::default()
                        .fg(palette::ORANGE)
                        .add_modifier(Modifier::BOLD),
                ),
                Cell::from(op.description.as_str()).style(Style::default().fg(palette::FG)),
            ])
        })
        .collect();

    let header = Row::new(vec![
        "#",
        "MB",
        "Opcode",
        "Arg0",
        "Current → Target",
        "Description",
    ])
    .style(
        Style::default()
            .fg(palette::MUTED)
            .add_modifier(Modifier::BOLD),
    );

    let widths = if area.width < 72 {
        [
            Constraint::Length(2),
            Constraint::Length(4),
            Constraint::Length(6),
            Constraint::Length(10),
            Constraint::Length(15),
            Constraint::Min(12),
        ]
    } else {
        [
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Length(7),
            Constraint::Length(11),
            Constraint::Length(22),
            Constraint::Min(20),
        ]
    };

    let table = Table::new(op_rows, widths).header(header).column_spacing(1);
    frame.render_widget(table, area);
}

pub(super) fn draw_confirm_modal_footer(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    start: usize,
    visible_items: usize,
    total_items: usize,
) {
    let scroll_hint = if total_items > visible_items {
        format!(
            "  [{}-{}/{} · ↑/↓ Scroll]",
            start + 1,
            (start + visible_items).min(total_items),
            total_items
        )
    } else {
        String::new()
    };

    let confirm_bg = if app.can_write() {
        palette::GREEN
    } else {
        palette::YELLOW
    };
    let actions = vec![
        Line::from(""),
        Line::from(vec![
            badge_span(" [y / Enter] Confirm & Apply ", confirm_bg),
            Span::raw("   "),
            Span::styled(
                " [n / Esc] Cancel ",
                Style::default()
                    .fg(palette::FG)
                    .bg(palette::ROW_HL)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(scroll_hint, Style::default().fg(palette::DIM)),
        ]),
    ];
    frame.render_widget(Paragraph::new(actions).alignment(Alignment::Center), area);
}

// ---------------------------------------------------------------------------
// Help Modal Overlay
// ---------------------------------------------------------------------------

pub(super) fn draw_help_popup(frame: &mut Frame, root: Rect) {
    let pct_x = if root.width < 92 { 92 } else { 74 };
    let pct_y = if root.height < 28 { 90 } else { 82 };
    let popup = centered_rect(pct_x, pct_y, root);
    frame.render_widget(Clear, popup);

    let block = modal_frame_block(" zensmu top — Help & Keybindings ", palette::CYAN);

    let help_lines = vec![
        Line::from(Span::styled(
            "Navigation & Telemetry",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        )),
        help_entry(
            "1 / 2 / 3 / 4",
            "Switch to Overview, Clocks & Volts, CO & Shaper, or Tune",
        ),
        help_entry("t", "Jump straight to [4:Tune] from any tab"),
        help_entry("Tab / Shift+Tab", "Cycle forward / backward through views"),
        help_entry("p / Space", "Pause or resume live PM table polling"),
        help_entry("w", "Start or stop live telemetry recording (unprivileged)"),
        help_entry("W", "Cycle recording format (CSV / JSONL / PTS) when idle"),
        help_entry(
            "r",
            "Force refresh of PM telemetry and SMU mailbox readback",
        ),
        help_entry(
            "+ / -",
            "Increase or decrease polling interval (100ms..10000ms)",
        ),
        help_entry("? / Esc", "Toggle or close this help overlay"),
        help_entry("q / Ctrl+C", "Quit and cleanly restore terminal state"),
        Line::from(""),
        Line::from(Span::styled(
            "Interactive Tuning ([4:Tune] · Live Telemetry Always Active)",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        )),
        help_entry("↑↓ / j / k", "Move parameter selection up / down"),
        help_entry("[ / ]", "Jump to previous / next capability section"),
        help_entry("←→ / h / l", "Decrement / increment selected parameter"),
        help_entry(
            "H / L / Shift+←→",
            "Coarse step decrement / increment (or PgDn / PgUp)",
        ),
        help_entry(
            "0 / Backspace",
            "Reset selected param to 0 (CO/Shaper) or live value",
        ),
        help_entry("u", "Undo / discard all staged changes"),
        help_entry(
            "x",
            "Toggle Allow Extreme (hard bounds & 95°C Tctl always enforced)",
        ),
        help_entry(
            "a / Enter",
            "Run dry-run validation & open Confirmation Modal",
        ),
        help_entry(
            "R / B / Z",
            "Preview Rollback (last-state), Boot Baseline reset, or CO=0",
        ),
        help_entry("s", "Save live SMU state to TOML profile"),
        help_entry(
            "P",
            "Persist+Enable: save live state to /etc/zensmu/active.toml & enable service",
        ),
        help_entry(
            "E",
            "Toggle Service: enable or disable zensmu-apply.service",
        ),
        help_entry("C", "Clear tripped boot-loop crash guard sentinel"),
        Line::from(""),
        Line::from(Span::styled(
            "Safety & Privileges",
            Style::default()
                .fg(palette::TEAL)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            "  PM table telemetry works unprivileged. Run with sudo to read/write SMU mailboxes.",
            Style::default().fg(palette::MUTED),
        )),
        Line::from(Span::styled(
            "  Every write previews exact opcodes first, snapshots state, and verifies readback.",
            Style::default().fg(palette::MUTED),
        )),
    ];

    frame.render_widget(
        Paragraph::new(help_lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        popup,
    );
}

pub(super) fn help_entry<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(
            format!("  {key:<18}"),
            Style::default()
                .fg(palette::CYAN)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(palette::FG)),
    ])
}

pub(super) fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {
    let pad_y = 100u16.saturating_sub(percent_y) / 2;
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(pad_y),
            Constraint::Percentage(percent_y.min(100)),
            Constraint::Percentage(pad_y),
        ])
        .split(area);

    let pad_x = 100u16.saturating_sub(percent_x) / 2;
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(pad_x),
            Constraint::Percentage(percent_x.min(100)),
            Constraint::Percentage(pad_x),
        ])
        .split(vertical[1])[1]
}
