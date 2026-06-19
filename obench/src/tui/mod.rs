pub mod dashboard;
pub mod menu;
pub mod overview;
pub mod theme;

use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::admin::AdminClient;
use crate::cli::{Cli, Profile, Scope, Target};

// ── TUI state machine ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum Screen {
    Overview,
    PickTarget { cursor: usize },
    PickProfile { target: Target, cursor: usize },
    PickScope { target: Target, profile: Profile, cursor: usize },
}

pub async fn run(cli: &Cli) -> Result<()> {
    let admin = AdminClient::new(cli.admin_base.clone(), cli.admin_token.clone());
    let snap = overview::fetch(&admin).await;

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_state_machine(&mut terminal, cli, &admin, &snap).await;

    // Teardown is UNCONDITIONAL — runs regardless of result.
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

async fn run_state_machine(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    cli: &Cli,
    admin: &AdminClient,
    snap: &overview::Snapshot,
) -> Result<()> {
    let mut screen = Screen::Overview;
    let targets = [Target::Fixture, Target::Live];

    loop {
        match &screen.clone() {
            Screen::Overview => {
                terminal.draw(|f| draw_overview(f, snap))?;
                if event::poll(Duration::from_millis(200))? {
                    if let Event::Key(k) = event::read()? {
                        match k.code {
                            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                            KeyCode::Enter | KeyCode::Char(' ') => {
                                screen = Screen::PickTarget { cursor: 0 };
                            }
                            _ => {}
                        }
                    }
                }
            }

            Screen::PickTarget { cursor } => {
                let cursor = *cursor;
                terminal.draw(|f| draw_pick_target(f, &targets, cursor))?;
                if event::poll(Duration::from_millis(200))? {
                    if let Event::Key(k) = event::read()? {
                        match k.code {
                            KeyCode::Char('q') | KeyCode::Esc => {
                                screen = Screen::Overview;
                            }
                            KeyCode::Up => {
                                screen = Screen::PickTarget {
                                    cursor: cursor.saturating_sub(1),
                                };
                            }
                            KeyCode::Down => {
                                screen = Screen::PickTarget {
                                    cursor: (cursor + 1).min(targets.len() - 1),
                                };
                            }
                            KeyCode::Enter | KeyCode::Char(' ') => {
                                let target = targets[cursor];
                                screen = Screen::PickProfile { target, cursor: 0 };
                            }
                            _ => {}
                        }
                    }
                }
            }

            Screen::PickProfile { target, cursor } => {
                let target = *target;
                let cursor = *cursor;
                let profiles = menu::valid_profiles(target);
                // Filter out Auto — not supported on the dashboard path.
                let selectable: Vec<(Profile, bool)> = profiles
                    .iter()
                    .filter(|(p, _)| *p != Profile::Auto)
                    .cloned()
                    .collect();

                terminal.draw(|f| draw_pick_profile(f, target, &selectable, cursor))?;
                if event::poll(Duration::from_millis(200))? {
                    if let Event::Key(k) = event::read()? {
                        match k.code {
                            KeyCode::Char('q') | KeyCode::Esc => {
                                screen = Screen::PickTarget { cursor: 0 };
                            }
                            KeyCode::Up => {
                                screen = Screen::PickProfile {
                                    target,
                                    cursor: cursor.saturating_sub(1),
                                };
                            }
                            KeyCode::Down => {
                                screen = Screen::PickProfile {
                                    target,
                                    cursor: (cursor + 1).min(selectable.len() - 1),
                                };
                            }
                            KeyCode::Enter | KeyCode::Char(' ') => {
                                let (profile, enabled) = selectable[cursor];
                                if enabled {
                                    screen =
                                        Screen::PickScope { target, profile, cursor: 0 };
                                }
                                // If not enabled, ignore — user must pick a valid one.
                            }
                            _ => {}
                        }
                    }
                }
            }

            Screen::PickScope { target, profile, cursor } => {
                let target = *target;
                let profile = *profile;
                let cursor = *cursor;
                let scopes = ["all", "single (use --model flag)"];

                terminal.draw(|f| draw_pick_scope(f, target, profile, &scopes, cursor))?;
                if event::poll(Duration::from_millis(200))? {
                    if let Event::Key(k) = event::read()? {
                        match k.code {
                            KeyCode::Char('q') | KeyCode::Esc => {
                                screen = Screen::PickProfile { target, cursor: 0 };
                            }
                            KeyCode::Up => {
                                screen = Screen::PickScope {
                                    target,
                                    profile,
                                    cursor: cursor.saturating_sub(1),
                                };
                            }
                            KeyCode::Down => {
                                screen = Screen::PickScope {
                                    target,
                                    profile,
                                    cursor: (cursor + 1).min(scopes.len() - 1),
                                };
                            }
                            KeyCode::Enter | KeyCode::Char(' ') => {
                                // Scope::All or Scope::Single from cli.model.
                                let scope = if cursor == 0 {
                                    Scope::All
                                } else {
                                    crate::cli::scope_from(cli.model.clone(), false)
                                };
                                // Start the run and go to dashboard.
                                let handles =
                                    crate::profiles::start_run(cli, target, profile, scope)
                                        .await?;
                                // Dashboard render loop.
                                run_dashboard(terminal, admin, handles).await?;
                                // After dashboard exits, return to overview.
                                screen = Screen::Overview;
                            }
                            _ => {}
                        }
                    }
                }
            }

        }
    }
}

// ── Dashboard loop ────────────────────────────────────────────────────────────

async fn run_dashboard(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    admin: &AdminClient,
    handles: crate::profiles::RunHandles,
) -> Result<()> {
    let started = Instant::now();
    let mut last_live_refresh = Instant::now();
    let mut live = crate::admin::FairshareLive { global_in_flight: 0, global_queued: 0 };

    loop {
        // Refresh fairshare roughly every 2 s.
        if last_live_refresh.elapsed() >= Duration::from_secs(2) {
            if let Ok(l) = admin.fairshare_live().await {
                live = l;
            }
            last_live_refresh = Instant::now();
        }

        let summary = {
            let s = handles.stats.lock().unwrap();
            s.summarize(
                started.elapsed().as_secs_f64().max(1.0),
                handles.plan.max_error_rate,
            )
        };

        terminal.draw(|f| {
            dashboard::draw(
                f,
                &handles.profile_name,
                &summary,
                live.global_in_flight,
                live.global_queued,
                handles.plan.capacity,
                &handles.ui_base,
            )
        })?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(k) = event::read()? {
                if matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) {
                    handles
                        .stop
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    break;
                }
            }
        }

        // Engine finished on its own (duration elapsed or stop set externally).
        if handles
            .stop
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            break;
        }
    }

    Ok(())
}

// ── Draw helpers ──────────────────────────────────────────────────────────────

fn draw_overview(f: &mut Frame, snap: &overview::Snapshot) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(area);

    let title = Paragraph::new("obench — deployment overview")
        .style(
            Style::default()
                .fg(theme::ACCENT)
                .bg(theme::BASE)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::MUTED)),
        );
    f.render_widget(title, chunks[0]);

    let mut items: Vec<ListItem> = Vec::new();
    items.push(
        ListItem::new(format!("capacity max_in_flight: {}", snap.capacity))
            .style(Style::default().fg(theme::FG)),
    );
    items.push(
        ListItem::new(format!("in_flight {}  queued {}", snap.in_flight, snap.queued))
            .style(Style::default().fg(theme::FG)),
    );
    items.push(
        ListItem::new(format!(
            "models ({}): {}",
            snap.models.len(),
            snap.models.join(", ")
        ))
        .style(Style::default().fg(theme::FG)),
    );
    items.push(
        ListItem::new(format!(
            "tenants ({}): {}",
            snap.tenants.len(),
            snap.tenants.join(", ")
        ))
        .style(Style::default().fg(theme::MUTED)),
    );
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("snapshot")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(list, chunks[1]);

    let help = Paragraph::new("[Enter] start benchmark   [q] quit")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}

fn draw_pick_target(f: &mut Frame, targets: &[Target], cursor: usize) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .split(area);

    let title = Paragraph::new("obench — pick target")
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
    f.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = targets
        .iter()
        .enumerate()
        .map(|(i, t)| {
            let label = format!("{t:?}");
            if i == cursor {
                ListItem::new(format!("▶ {label}"))
                    .style(Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD))
            } else {
                ListItem::new(format!("  {label}")).style(Style::default().fg(theme::FG))
            }
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("target")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(list, chunks[1]);

    let help = Paragraph::new("[↑/↓] move   [Enter] select   [q/Esc] back")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}

fn draw_pick_profile(
    f: &mut Frame,
    target: Target,
    profiles: &[(Profile, bool)],
    cursor: usize,
) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .split(area);

    let title = Paragraph::new(format!("obench — pick profile  (target: {target:?})"))
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
    f.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = profiles
        .iter()
        .enumerate()
        .map(|(i, (p, enabled))| {
            let label = format!("{p:?}");
            let tag = if *enabled { "" } else { " (disabled)" };
            let color = if !enabled {
                theme::MUTED
            } else if i == cursor {
                theme::ACCENT
            } else {
                theme::FG
            };
            let prefix = if i == cursor && *enabled { "▶ " } else { "  " };
            let modifier = if i == cursor && *enabled {
                Modifier::BOLD
            } else {
                Modifier::empty()
            };
            ListItem::new(format!("{prefix}{label}{tag}"))
                .style(Style::default().fg(color).add_modifier(modifier))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("profile")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(list, chunks[1]);

    let help = Paragraph::new("[↑/↓] move   [Enter] select   [q/Esc] back")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}

fn draw_pick_scope(
    f: &mut Frame,
    target: Target,
    profile: Profile,
    scopes: &[&str],
    cursor: usize,
) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .split(area);

    let title = Paragraph::new(format!(
        "obench — pick scope  ({target:?} / {profile:?})"
    ))
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
    f.render_widget(title, chunks[0]);

    let items: Vec<ListItem> = scopes
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if i == cursor {
                ListItem::new(format!("▶ {s}"))
                    .style(Style::default().fg(theme::ACCENT).add_modifier(Modifier::BOLD))
            } else {
                ListItem::new(format!("  {s}")).style(Style::default().fg(theme::FG))
            }
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("scope")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(list, chunks[1]);

    let help = Paragraph::new("[↑/↓] move   [Enter] start run   [q/Esc] back")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}
