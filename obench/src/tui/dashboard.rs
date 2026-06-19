use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Gauge, Paragraph};

use crate::engine::stats::Summary;
use crate::tui::theme;

pub fn draw(
    f: &mut Frame,
    title: &str,
    summary: &Summary,
    in_flight: u64,
    queued: u64,
    capacity: u32,
    ui_base: &str,
) {
    let area = f.area();
    let rows = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(6),
        Constraint::Length(3),
        Constraint::Min(3),
    ])
    .split(area);

    let head = Paragraph::new(title.to_string())
        .style(
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::MUTED)),
        );
    f.render_widget(head, rows[0]);

    let err_color = if summary.error_rate > 0.0 {
        theme::ALERT
    } else {
        theme::ACCENT
    };
    let body = Paragraph::new(vec![
        Line::from(format!(
            "req/s {:.0}    completed {}    429 {}",
            summary.req_per_s, summary.completed, summary.rejected
        )),
        Line::from(format!(
            "tokens in {}  out {}",
            summary.in_tokens, summary.out_tokens
        )),
        Line::from(Span::styled(
            format!("errors {:.2}%", summary.error_rate * 100.0),
            Style::default().fg(err_color),
        )),
        Line::from(format!(
            "ttfb p50 {}ms  p99 {}ms",
            summary.p50_ttfb_ms, summary.p99_ttfb_ms
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(body, rows[1]);

    let ratio = if capacity > 0 {
        (in_flight as f64 / capacity as f64).min(1.0)
    } else {
        0.0
    };
    let gauge = Gauge::default()
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(format!(
                    "in_flight {in_flight}/{capacity}  queued {queued}"
                )),
        )
        .gauge_style(Style::default().fg(theme::ACCENT))
        .ratio(ratio);
    f.render_widget(gauge, rows[2]);

    let ptr = Paragraph::new(vec![
        Line::from(Span::styled(
            "watch in the control plane:",
            Style::default().fg(theme::MUTED),
        )),
        Line::from(format!("  fairshare   {ui_base}/fairshare")),
        Line::from(format!("  accounting  {ui_base}/usage")),
        Line::from(Span::styled(
            "[q] quit & drain   [p] pause",
            Style::default().fg(theme::MUTED),
        )),
    ]);
    f.render_widget(ptr, rows[3]);
}
