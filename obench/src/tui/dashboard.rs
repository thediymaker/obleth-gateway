use ratatui::prelude::*;
use ratatui::symbols::Marker;
use ratatui::widgets::{Axis, Block, Borders, Chart, Dataset, Gauge, GraphType, Paragraph};

use crate::engine::stats::Summary;
use crate::tui::{art, theme};

/// Everything the live dashboard needs for one frame. Bundled into a struct so
/// the call site stays readable as the dashboard grows richer.
pub struct DashFrame<'a> {
    pub title: &'a str,
    pub elapsed_s: u64,
    pub summary: &'a Summary,
    /// Rolling per-second completion counts (oldest → newest) for the sparkline.
    pub rps_history: &'a [u64],
    pub in_flight: u64,
    pub queued: u64,
    pub capacity: u32,
    pub ui_base: &'a str,
    pub gateway_observable: bool,
    /// Tenant key display names, parallel to `key_counts`.
    pub key_labels: &'a [String],
    /// Dispatched-request totals per key, parallel to `key_labels`.
    pub key_counts: &'a [u64],
    /// True once the run has finished on its own (the final, parked frame).
    pub complete: bool,
}

pub fn draw(f: &mut Frame, d: &DashFrame) {
    let area = f.area();
    let rows = Layout::vertical([
        Constraint::Length(3), // header
        Constraint::Length(4), // KPI tiles
        Constraint::Length(1), // tokens + latency strip
        Constraint::Length(8), // throughput chart
        Constraint::Min(4),    // per-key fairshare bars
        Constraint::Length(3), // in_flight gauge / remote note
        Constraint::Length(4), // footer
    ])
    .split(area);

    draw_header(f, rows[0], d);
    draw_kpis(f, rows[1], d.summary);
    draw_strip(f, rows[2], d.summary);
    draw_sparkline(f, rows[3], d.rps_history, d.summary.req_per_s);
    draw_keys(f, rows[4], d.key_labels, d.key_counts);
    draw_gauge(f, rows[5], d);
    draw_footer(f, rows[6], d.ui_base, d.complete);
}

fn fmt_clock(secs: u64) -> String {
    format!("{:02}:{:02}", secs / 60, secs % 60)
}

/// One-line secondary readout: token throughput + TTFB latency percentiles.
fn draw_strip(f: &mut Frame, area: Rect, s: &Summary) {
    let est = if s.any_estimated { " ~" } else { "" };
    let strip = Paragraph::new(Line::from(vec![
        Span::styled("tokens ", Style::default().fg(theme::MUTED)),
        Span::styled(format!("in {}", s.in_tokens), Style::default().fg(theme::FG)),
        Span::styled(
            format!(" · out {}{est}   ", s.out_tokens),
            Style::default().fg(theme::FG),
        ),
        Span::styled("ttfb ", Style::default().fg(theme::MUTED)),
        Span::styled(
            format!("p50 {}ms", s.p50_ttfb_ms),
            Style::default().fg(theme::FG),
        ),
        Span::styled(
            format!(" · p90 {}ms", s.p90_ttfb_ms),
            Style::default().fg(theme::MUTED),
        ),
        Span::styled(
            format!(" · p99 {}ms", s.p99_ttfb_ms),
            Style::default().fg(theme::MUTED),
        ),
    ]));
    f.render_widget(strip, area);
}

fn draw_header(f: &mut Frame, area: Rect, d: &DashFrame) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::MUTED));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::horizontal([Constraint::Min(10), Constraint::Length(16)]).split(inner);
    let scope = if d.gateway_observable { "local demo" } else { "remote live" };
    let mut left_spans = vec![
        Span::styled(
            "obench ",
            Style::default().fg(theme::MUTED).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            d.title.to_string(),
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  ·  {scope}"), Style::default().fg(theme::MUTED)),
    ];
    if d.complete {
        left_spans.push(Span::styled(
            "  ·  ✓ complete",
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        ));
    }
    let left = Paragraph::new(Line::from(left_spans));
    f.render_widget(left, cols[0]);

    let right = Paragraph::new(Line::from(vec![
        Span::styled("elapsed ", Style::default().fg(theme::MUTED)),
        Span::styled(
            fmt_clock(d.elapsed_s),
            Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
        ),
    ]))
    .alignment(Alignment::Right);
    f.render_widget(right, cols[1]);
}

/// Four side-by-side stat tiles with big, colour-coded values.
fn draw_kpis(f: &mut Frame, area: Rect, s: &Summary) {
    let cells = Layout::horizontal([
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
        Constraint::Ratio(1, 4),
    ])
    .split(area);

    let err_color = if s.error_rate > 0.0 { theme::ALERT } else { theme::ACCENT };
    let rej_color = if s.rejected > 0 { theme::FG } else { theme::MUTED };

    let tiles = [
        ("req/s", format!("{:.0}", s.req_per_s), theme::ACCENT),
        ("completed", s.completed.to_string(), theme::FG),
        ("429 shed", s.rejected.to_string(), rej_color),
        ("errors", format!("{:.2}%", s.error_rate * 100.0), err_color),
    ];

    for (i, (label, value, color)) in tiles.iter().enumerate() {
        let block = Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::MUTED))
            .title(Span::styled(
                format!(" {label} "),
                Style::default().fg(theme::MUTED),
            ));
        let inner = block.inner(cells[i]);
        f.render_widget(block, cells[i]);
        let val = Paragraph::new(Line::from(Span::styled(
            value.clone(),
            Style::default().fg(*color).add_modifier(Modifier::BOLD),
        )))
        .alignment(Alignment::Center);
        f.render_widget(val, inner);
    }
}

/// A sparkline of recent throughput plus a current/peak readout beside it.
fn draw_sparkline(f: &mut Frame, area: Rect, history: &[u64], cur_rps: f64) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::MUTED))
        .title(Span::styled(" throughput (req/s) ", Style::default().fg(theme::MUTED)));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let cols = Layout::horizontal([Constraint::Min(10), Constraint::Length(12)]).split(inner);

    let peak = history.iter().copied().max().unwrap_or(0).max(1);

    // Plot the rolling history as a smooth braille line. Empty/short histories
    // still get a flat baseline so the panel never looks broken.
    let points: Vec<(f64, f64)> = history
        .iter()
        .enumerate()
        .map(|(i, &v)| (i as f64, v as f64))
        .collect();
    let x_max = (history.len().saturating_sub(1)).max(1) as f64;
    let y_max = peak as f64 * 1.2;

    let datasets = vec![Dataset::default()
        .marker(Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(theme::ACCENT))
        .data(&points)];
    let chart = Chart::new(datasets)
        .x_axis(Axis::default().bounds([0.0, x_max]))
        .y_axis(
            Axis::default()
                .style(Style::default().fg(theme::MUTED))
                .bounds([0.0, y_max])
                .labels(vec![
                    Span::styled("0", Style::default().fg(theme::MUTED)),
                    Span::styled(format!("{peak}"), Style::default().fg(theme::MUTED)),
                ]),
        );
    f.render_widget(chart, cols[0]);

    let side = Paragraph::new(vec![
        Line::from(vec![Span::styled("now", Style::default().fg(theme::MUTED))]),
        Line::from(vec![Span::styled(
            format!("{cur_rps:.0}/s"),
            Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD),
        )]),
        Line::from(vec![Span::styled(
            format!("peak {peak}"),
            Style::default().fg(theme::MUTED),
        )]),
    ])
    .alignment(Alignment::Right);
    f.render_widget(side, cols[1]);
}

/// Per-key request distribution — the fairshare story. Each tenant key gets a
/// smooth meter sized to its share of total dispatched requests.
fn draw_keys(f: &mut Frame, area: Rect, labels: &[String], counts: &[u64]) {
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::MUTED))
        .title(Span::styled(
            format!(" tenant load · {} key(s) ", labels.len()),
            Style::default().fg(theme::MUTED),
        ));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if labels.is_empty() {
        let p = Paragraph::new("no tenant keys").style(Style::default().fg(theme::MUTED));
        f.render_widget(p, inner);
        return;
    }

    let total: u64 = counts.iter().copied().sum::<u64>().max(1);
    let label_w = labels.iter().map(|l| l.len()).max().unwrap_or(6).min(20);
    // Reserve room for "  12345 (100%)" on the right + label + padding.
    let bar_w = (inner.width as usize).saturating_sub(label_w + 16).max(6);

    // Cycle a few shades so stacked bars are visually distinct.
    let palette = [theme::ACCENT, theme::FG, theme::MUTED];

    let mut lines: Vec<Line> = Vec::with_capacity(labels.len());
    for (i, label) in labels.iter().enumerate() {
        let count = counts.get(i).copied().unwrap_or(0);
        let ratio = count as f64 / total as f64;
        let color = palette[i % palette.len()];
        let mut spans = vec![Span::styled(
            format!("{label:<label_w$}  "),
            Style::default().fg(theme::FG),
        )];
        spans.extend(art::meter(ratio, bar_w, color, theme::MUTED));
        spans.push(Span::styled(
            format!("  {count:>6} ({:>3.0}%)", ratio * 100.0),
            Style::default().fg(theme::MUTED),
        ));
        lines.push(Line::from(spans));
    }
    f.render_widget(Paragraph::new(lines), inner);
}

fn draw_gauge(f: &mut Frame, area: Rect, d: &DashFrame) {
    if d.gateway_observable {
        let ratio = if d.capacity > 0 {
            (d.in_flight as f64 / d.capacity as f64).min(1.0)
        } else {
            0.0
        };
        let color = if ratio >= 0.95 { theme::ALERT } else { theme::ACCENT };
        let gauge = Gauge::default()
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::MUTED))
                    .title(format!(
                        " in_flight {}/{}  ·  queued {} ",
                        d.in_flight, d.capacity, d.queued
                    )),
            )
            .gauge_style(Style::default().fg(color))
            .ratio(ratio);
        f.render_widget(gauge, area);
    } else {
        let note = Paragraph::new(Line::from(vec![
            Span::styled(
                "remote gateway",
                Style::default().fg(theme::FG).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "  —  in_flight / queued live on its own control plane",
                Style::default().fg(theme::MUTED),
            ),
        ]))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::MUTED))
                .title(Span::styled(" gateway ", Style::default().fg(theme::MUTED))),
        );
        f.render_widget(note, area);
    }
}

fn draw_footer(f: &mut Frame, area: Rect, ui_base: &str, complete: bool) {
    let quit_hint = if complete {
        "✓ run complete · [any key] return"
    } else {
        "[q] quit & drain"
    };
    let quit_color = if complete { theme::ACCENT } else { theme::MUTED };
    let ptr = Paragraph::new(vec![
        Line::from(Span::styled(
            "watch in the control plane:",
            Style::default().fg(theme::MUTED),
        )),
        Line::from(vec![
            Span::styled("  fairshare   ", Style::default().fg(theme::MUTED)),
            Span::styled(format!("{ui_base}/fairshare"), Style::default().fg(theme::ACCENT)),
        ]),
        Line::from(vec![
            Span::styled("  accounting  ", Style::default().fg(theme::MUTED)),
            Span::styled(format!("{ui_base}/usage"), Style::default().fg(theme::ACCENT)),
        ]),
        Line::from(Span::styled(
            quit_hint,
            Style::default().fg(quit_color),
        )),
    ]);
    f.render_widget(ptr, area);
}
