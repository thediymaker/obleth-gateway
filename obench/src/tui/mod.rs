pub mod overview;
pub mod theme;

use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::execute;
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::admin::AdminClient;
use crate::cli::Cli;

pub async fn run(cli: &Cli) -> Result<()> {
    let admin = AdminClient::new(cli.admin_base.clone(), cli.admin_token.clone());
    let snap = overview::fetch(&admin).await;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let loop_result = run_event_loop(&mut terminal, &snap);

    // teardown ALWAYS runs, regardless of loop_result
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    loop_result
}

fn run_event_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    snap: &overview::Snapshot,
) -> Result<()> {
    loop {
        terminal.draw(|f| draw_overview(f, snap))?;
        if event::poll(Duration::from_millis(200))? {
            if let Event::Key(k) = event::read()? {
                if matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                    break;
                }
            }
        }
    }
    Ok(())
}

fn draw_overview(f: &mut Frame, snap: &overview::Snapshot) {
    let area = f.area();
    let chunks = Layout::vertical([Constraint::Length(3), Constraint::Min(5), Constraint::Length(3)]).split(area);

    let title = Paragraph::new("obench — deployment overview")
        .style(Style::default().fg(theme::ACCENT).bg(theme::BASE).add_modifier(Modifier::BOLD))
        .block(Block::default().borders(Borders::ALL).border_style(Style::default().fg(theme::MUTED)));
    f.render_widget(title, chunks[0]);

    let mut items: Vec<ListItem> = Vec::new();
    items.push(ListItem::new(format!("capacity max_in_flight: {}", snap.capacity)).style(Style::default().fg(theme::FG)));
    items.push(ListItem::new(format!("in_flight {}  queued {}", snap.in_flight, snap.queued)).style(Style::default().fg(theme::FG)));
    items.push(ListItem::new(format!("models ({}): {}", snap.models.len(), snap.models.join(", "))).style(Style::default().fg(theme::FG)));
    items.push(ListItem::new(format!("tenants ({}): {}", snap.tenants.len(), snap.tenants.join(", "))).style(Style::default().fg(theme::MUTED)));
    let list = List::new(items).block(Block::default().borders(Borders::ALL).title("snapshot").border_style(Style::default().fg(theme::MUTED)));
    f.render_widget(list, chunks[1]);

    let help = Paragraph::new("[q] quit   (menu/pick/dashboard land in the next task)")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}
