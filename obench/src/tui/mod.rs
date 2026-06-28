pub mod art;
pub mod dashboard;
pub mod menu;
pub mod overview;
pub mod theme;

use std::collections::VecDeque;
use std::io;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph, Wrap};

use crate::admin::AdminClient;
use crate::cli::{Cli, Profile, Scope, Target};
use crate::engine::fleet;

// ── TUI wizard ────────────────────────────────────────────────────────────────
//
// A linear, self-explaining wizard. Every step tells you what it is and what it
// will do; the live path asks for a base URL + key, lists the upstream's models,
// and lets you pick which to benchmark — no config file required. Nothing is
// seeded until the final confirm screen, which spells out exactly which models,
// tenants, and keys obench is about to create.

#[derive(Clone, PartialEq)]
enum Step {
    PickTarget,
    LiveUrl,
    KeysList,
    KeyAdd,
    LivePickModels,
    FixtureScope,
    FixtureModel,
    Settings,
    Review,
    Error,
}

/// One real tenant key for a remote live run. The secret lives only in memory,
/// and keys are never persisted — they are re-entered fresh for each run.
#[derive(Clone)]
struct KeyEntry {
    label: String,
    weight: u32,
    secret: String,
}

struct Wizard {
    step: Step,
    cursor: usize,
    input: String,
    error: String,
    return_to: Step,
    target: Target,
    profile: Profile,
    input_tokens: u32,
    /// Parallel workers driving the closed loop (throughput lever).
    conc: u32,
    /// Max completion tokens per request (latency / throughput lever).
    output_tokens: u32,
    // live (remote obleth)
    live_url: String,
    keys: Vec<KeyEntry>,
    live_models: Vec<String>,
    live_selected: Vec<bool>,
    // demo
    fixture_all: bool,
    fixture_model: String,
    // settings form cursor (0 = profile, 1 = concurrency, 2 = output, 3 = prompt)
    settings_row: usize,
}

impl Wizard {
    /// A fresh wizard pre-filled from the last saved run (secrets excluded).
    fn from_saved(cli: &Cli, saved: &crate::persist::SavedSpec) -> Self {
        let target = if saved.target == "live" {
            Target::Live
        } else {
            Target::Demo
        };
        let profile = parse_profile(&saved.profile).unwrap_or(Profile::Smoke);
        let input_tokens = if saved.input_tokens > 0 {
            saved.input_tokens
        } else {
            cli.input_tokens
        };
        // Seed the throughput knobs from the saved run, falling back to the
        // chosen profile's baseline when unset.
        let base = crate::profiles::plan::base_plan(profile);
        let conc = if saved.conc > 0 {
            saved.conc
        } else {
            base.conc
        };
        let output_tokens = if saved.output_tokens > 0 {
            saved.output_tokens
        } else {
            base.output_tokens
        };
        // Tenant keys are never restored: a saved label without its secret is
        // useless, so the user always re-adds keys for each run.
        Self {
            step: Step::PickTarget,
            cursor: 0,
            input: String::new(),
            error: String::new(),
            return_to: Step::PickTarget,
            target,
            profile,
            input_tokens,
            conc,
            output_tokens,
            live_url: saved.live_url.clone(),
            keys: Vec::new(),
            live_models: saved.live_models.clone(),
            live_selected: vec![false; saved.live_models.len()],
            fixture_all: if saved.target.is_empty() {
                true
            } else {
                saved.fixture_all
            },
            fixture_model: saved.fixture_model.clone(),
            settings_row: 0,
        }
    }

    /// Snapshot the current selection for persistence (no secrets).
    fn to_saved(&self) -> crate::persist::SavedSpec {
        let selected_models: Vec<String> = self
            .live_models
            .iter()
            .zip(self.live_selected.iter())
            .filter(|(_, s)| **s)
            .map(|(n, _)| n.clone())
            .collect();
        crate::persist::SavedSpec {
            target: match self.target {
                Target::Demo => "demo".into(),
                Target::Live => "live".into(),
            },
            profile: format!("{:?}", self.profile).to_lowercase(),
            input_tokens: self.input_tokens,
            conc: self.conc,
            output_tokens: self.output_tokens,
            live_url: self.live_url.clone(),
            live_models: selected_models,
            fixture_all: self.fixture_all,
            fixture_model: self.fixture_model.clone(),
        }
    }

    /// The model names selected for a live run.
    fn selected_live_models(&self) -> Vec<String> {
        self.live_models
            .iter()
            .zip(self.live_selected.iter())
            .filter(|(_, s)| **s)
            .map(|(n, _)| n.clone())
            .collect()
    }
}

fn parse_profile(s: &str) -> Option<Profile> {
    match s {
        "smoke" => Some(Profile::Smoke),
        "light" => Some(Profile::Light),
        "heavy" => Some(Profile::Heavy),
        "extreme" => Some(Profile::Extreme),
        "manual" => Some(Profile::Manual),
        _ => None,
    }
}

/// A row on the review screen: either jump back to a step to edit it, or run.
#[derive(Clone)]
enum ReviewAction {
    Goto(Step),
    Run,
}

impl Wizard {
    /// Snap `profile` to a profile that is valid for the current target (e.g.
    /// `extreme` is demo-only, so switching to live moves off it).
    fn clamp_profile(&mut self) {
        let profiles = selectable_profiles(self.target);
        let ok = profiles
            .iter()
            .any(|(p, enabled)| *p == self.profile && *enabled);
        if !ok {
            if let Some((p, _)) = profiles.iter().find(|(_, e)| *e) {
                self.profile = *p;
            }
        }
    }

    /// Reset the throughput knobs to the current profile's baseline. Called when
    /// the user switches profile so the profile acts as a preset to fine-tune.
    fn apply_profile_defaults(&mut self) {
        let base = crate::profiles::plan::base_plan(self.profile);
        self.conc = base.conc;
        self.output_tokens = base.output_tokens;
    }
}

/// Decrement `v` by `step` without underflowing below `min`.
fn step_down(v: u32, step: u32, min: u32) -> u32 {
    v.saturating_sub(step).max(min)
}

/// Move `w.profile` to the next/previous enabled profile for its target.
fn cycle_profile(w: &mut Wizard, profiles: &[(Profile, bool)], forward: bool) {
    let enabled: Vec<Profile> = profiles
        .iter()
        .filter(|(_, e)| *e)
        .map(|(p, _)| *p)
        .collect();
    if enabled.is_empty() {
        return;
    }
    let cur = enabled.iter().position(|p| *p == w.profile).unwrap_or(0);
    let next = if forward {
        (cur + 1) % enabled.len()
    } else {
        (cur + enabled.len() - 1) % enabled.len()
    };
    w.profile = enabled[next];
}

/// Editable rows shown on the review screen, paired with their jump-to action.
fn review_items(w: &Wizard) -> Vec<(String, ReviewAction)> {
    let mut v: Vec<(String, ReviewAction)> = Vec::new();
    let tname = match w.target {
        Target::Demo => "demo  (local GPU-free backend)",
        Target::Live => "live  (remote obleth gateway)",
    };
    v.push((
        format!("target:    {tname}"),
        ReviewAction::Goto(Step::PickTarget),
    ));

    match w.target {
        Target::Live => {
            let url = if w.live_url.is_empty() {
                "(not set)".to_string()
            } else {
                w.live_url.clone()
            };
            v.push((
                format!("endpoint:  {url}"),
                ReviewAction::Goto(Step::LiveUrl),
            ));
            let withsec = w
                .keys
                .iter()
                .filter(|k| !k.secret.trim().is_empty())
                .count();
            let keysum = if w.keys.is_empty() {
                "(none — press to add)".to_string()
            } else {
                format!(
                    "{} tenant key(s), {withsec} ready this session",
                    w.keys.len()
                )
            };
            v.push((
                format!("keys:      {keysum}"),
                ReviewAction::Goto(Step::KeysList),
            ));
            let models = w.selected_live_models();
            let msum = if models.is_empty() {
                "(none selected)".to_string()
            } else {
                models.join(", ")
            };
            v.push((
                format!("models:    {msum}"),
                ReviewAction::Goto(Step::LivePickModels),
            ));
        }
        Target::Demo => {
            let scope = if w.fixture_all {
                "all demo models".to_string()
            } else {
                format!("single: {}", w.fixture_model)
            };
            v.push((
                format!("scope:     {scope}"),
                ReviewAction::Goto(Step::FixtureScope),
            ));
        }
    }

    v.push((
        format!(
            "load:      {} · {} workers · out {} · in {} tokens",
            format!("{:?}", w.profile).to_lowercase(),
            w.conc,
            w.output_tokens,
            w.input_tokens
        ),
        ReviewAction::Goto(Step::Settings),
    ));
    v.push(("run benchmark".to_string(), ReviewAction::Run));
    v
}

/// Optional advisory note shown under the review rows (e.g. the fairshare hint).
fn review_note(w: &Wizard) -> Option<String> {
    if w.target == Target::Live {
        let withsec = w
            .keys
            .iter()
            .filter(|k| !k.secret.trim().is_empty())
            .count();
        if withsec < 2 {
            return Some(
                "tip: add 2+ tenant keys to see fairshare contention — one key just measures throughput"
                    .to_string(),
            );
        }
    }
    None
}

/// Translate the wizard into the scope + optional in-memory live config used to
/// start a run.
fn build_run_inputs(w: &Wizard) -> (Scope, Option<crate::config::LiveConfig>) {
    match w.target {
        Target::Demo => {
            let scope = if w.fixture_all {
                Scope::All
            } else {
                Scope::Single(w.fixture_model.clone())
            };
            (scope, None)
        }
        Target::Live => {
            let names = w.selected_live_models();
            let scope = if names.len() == 1 {
                Scope::Single(names[0].clone())
            } else {
                Scope::All
            };
            let keys: Vec<crate::config::LiveKey> = w
                .keys
                .iter()
                .filter(|k| !k.secret.trim().is_empty())
                .map(|k| crate::config::LiveKey {
                    label: k.label.clone(),
                    weight: k.weight,
                    secret: k.secret.clone(),
                })
                .collect();
            let cfg = crate::config::live_config_from_selection(&w.live_url, &keys, &names);
            (scope, Some(cfg))
        }
    }
}

/// Last-chance validation before a run, with a friendly message back to the TUI.
fn preflight(w: &Wizard) -> Result<(), String> {
    if w.target == Target::Live {
        if w.live_url.trim().is_empty() {
            return Err("set the endpoint URL first (edit the 'endpoint' row)".into());
        }
        if !w.keys.iter().any(|k| !k.secret.trim().is_empty()) {
            return Err("add at least one tenant key with a secret (edit the 'keys' row)".into());
        }
        if w.selected_live_models().is_empty() {
            return Err("select at least one model to benchmark (edit the 'models' row)".into());
        }
    }
    Ok(())
}

/// Selectable profiles for a target: the valid set minus Auto (Auto self-
/// calibrates and is not wired into the dashboard render path).
fn selectable_profiles(target: Target) -> Vec<(Profile, bool)> {
    menu::valid_profiles(target)
        .into_iter()
        .filter(|(p, _)| *p != Profile::Auto)
        .collect()
}

fn fixture_model_list() -> Vec<String> {
    fleet::FIXTURE_MODELS
        .iter()
        .map(|s| s.to_string())
        .collect()
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
    let saved = crate::persist::load();
    let mut w = Wizard::from_saved(cli, &saved);

    loop {
        // ── draw ──────────────────────────────────────────────────────────────
        match w.step {
            Step::PickTarget => {
                terminal.draw(|f| draw_pick_target(f, w.cursor, snap))?;
            }
            Step::LiveUrl => {
                terminal.draw(|f| {
                    draw_text_input(
                        f,
                        "live — remote obleth proxy URL",
                        "Base URL of the obleth gateway you want to benchmark (OpenAI-compatible).",
                        "e.g. https://gateway.example.com   or   https://gateway.example.com/v1",
                        &w.input,
                        false,
                    )
                })?;
            }
            Step::KeysList => {
                terminal.draw(|f| draw_keys_list(f, &w.live_url, &w.keys, w.cursor))?;
            }
            Step::KeyAdd => {
                terminal.draw(|f| {
                    draw_text_input(
                        f,
                        "live — add a tenant key",
                        "Paste a real API key for a tenant on this gateway. Each key is a distinct tenant; add several to drive fairshare contention.",
                        "input hidden — paste the key and press Enter",
                        &w.input,
                        true,
                    )
                })?;
            }
            Step::LivePickModels => {
                terminal.draw(|f| {
                    draw_multiselect(f, &w.live_url, &w.live_models, &w.live_selected, w.cursor)
                })?;
            }
            Step::FixtureScope => {
                let scopes: &[(&str, &str)] = &[
                    (
                        "all models",
                        "drive the whole obench-* demo fleet (5 models)",
                    ),
                    (
                        "single model",
                        "drive one model you pick on the next screen",
                    ),
                ];
                terminal.draw(|f| draw_pick_scope(f, w.target, scopes, w.cursor))?;
            }
            Step::FixtureModel => {
                let models = fixture_model_list();
                terminal.draw(|f| {
                    draw_model_picker(
                        f,
                        "demo — pick a model",
                        "These demo models are seeded against the GPU-free benchmark backend.",
                        &models,
                        w.cursor,
                    )
                })?;
            }
            Step::Settings => {
                let profiles = selectable_profiles(w.target);
                terminal.draw(|f| draw_settings(f, &w, &profiles))?;
            }
            Step::Review => {
                let items = review_items(&w);
                let labels: Vec<String> = items.iter().map(|(l, _)| l.clone()).collect();
                let note = review_note(&w);
                terminal.draw(|f| draw_review(f, &labels, note.as_deref(), w.cursor))?;
            }
            Step::Error => {
                let msg = w.error.clone();
                terminal.draw(|f| draw_message(f, "something went wrong", &[msg], theme::ALERT))?;
            }
        }

        // ── input ─────────────────────────────────────────────────────────────
        if !event::poll(Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(k) = event::read()? else {
            continue;
        };
        // Windows reports a separate key-release event for every keystroke.
        // Ignore releases so input isn't processed twice (and so a key release
        // can't instantly dismiss the screen its press just navigated to).
        if k.kind == KeyEventKind::Release {
            continue;
        }
        let code = k.code;

        match w.step {
            Step::PickTarget => match code {
                KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                KeyCode::Up => w.cursor = w.cursor.saturating_sub(1),
                KeyCode::Down => w.cursor = (w.cursor + 1).min(1),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if w.cursor == 0 {
                        w.target = Target::Demo;
                        w.clamp_profile();
                        w.step = Step::FixtureScope;
                    } else {
                        w.target = Target::Live;
                        w.clamp_profile();
                        w.input = w.live_url.clone();
                        w.step = Step::LiveUrl;
                    }
                    w.cursor = 0;
                }
                _ => {}
            },

            Step::LiveUrl => match code {
                KeyCode::Esc => {
                    w.step = Step::PickTarget;
                    w.cursor = 0;
                }
                KeyCode::Enter => {
                    let v = w.input.trim().to_string();
                    if !v.is_empty() {
                        w.live_url = v;
                        w.step = Step::KeysList;
                        w.cursor = 0;
                    }
                }
                KeyCode::Backspace => {
                    w.input.pop();
                }
                KeyCode::Char(c) => w.input.push(c),
                _ => {}
            },

            Step::KeysList => match code {
                KeyCode::Esc => {
                    w.input = w.live_url.clone();
                    w.step = Step::LiveUrl;
                }
                KeyCode::Up => w.cursor = w.cursor.saturating_sub(1),
                KeyCode::Down => w.cursor = (w.cursor + 1).min(w.keys.len().saturating_sub(1)),
                KeyCode::Char('a') => {
                    w.input.clear();
                    w.step = Step::KeyAdd;
                }
                KeyCode::Char('d') => {
                    if w.cursor < w.keys.len() {
                        w.keys.remove(w.cursor);
                        w.cursor = w.cursor.min(w.keys.len().saturating_sub(1));
                    }
                }
                KeyCode::Char('+') | KeyCode::Right => {
                    if let Some(k) = w.keys.get_mut(w.cursor) {
                        k.weight = (k.weight + 50).min(100_000);
                    }
                }
                KeyCode::Char('-') | KeyCode::Left => {
                    if let Some(k) = w.keys.get_mut(w.cursor) {
                        k.weight = k.weight.saturating_sub(50).max(1);
                    }
                }
                KeyCode::Enter => {
                    let usable: Vec<String> = w
                        .keys
                        .iter()
                        .filter(|k| !k.secret.trim().is_empty())
                        .map(|k| k.secret.clone())
                        .collect();
                    if usable.is_empty() {
                        w.error =
                            "add at least one tenant key (press [a]) before continuing".into();
                        w.return_to = Step::KeysList;
                        w.step = Step::Error;
                    } else {
                        terminal.draw(|f| {
                            draw_message(
                                f,
                                "live",
                                &[format!("listing models from {} …", w.live_url)],
                                theme::ACCENT,
                            )
                        })?;
                        match crate::admin::fetch_upstream_models(&w.live_url, &usable[0]).await {
                            Ok(models) => {
                                let prev = w.selected_live_models();
                                w.live_selected = models.iter().map(|m| prev.contains(m)).collect();
                                w.live_models = models;
                                w.step = Step::LivePickModels;
                                w.cursor = 0;
                            }
                            Err(e) => {
                                w.error = format!("could not list models: {e}");
                                w.return_to = Step::KeysList;
                                w.step = Step::Error;
                            }
                        }
                    }
                }
                _ => {}
            },

            Step::KeyAdd => match code {
                KeyCode::Esc => {
                    w.input.clear();
                    w.step = Step::KeysList;
                }
                KeyCode::Enter => {
                    let v = w.input.trim().to_string();
                    if !v.is_empty() {
                        let n = w.keys.len() + 1;
                        w.keys.push(KeyEntry {
                            label: format!("tenant-{n}"),
                            weight: 100,
                            secret: v,
                        });
                        w.cursor = w.keys.len() - 1;
                    }
                    w.input.clear();
                    w.step = Step::KeysList;
                }
                KeyCode::Backspace => {
                    w.input.pop();
                }
                KeyCode::Char(c) => w.input.push(c),
                _ => {}
            },

            Step::LivePickModels => match code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    w.step = Step::KeysList;
                    w.cursor = 0;
                }
                KeyCode::Up => w.cursor = w.cursor.saturating_sub(1),
                KeyCode::Down => {
                    w.cursor = (w.cursor + 1).min(w.live_models.len().saturating_sub(1))
                }
                KeyCode::Char(' ') => {
                    if let Some(s) = w.live_selected.get_mut(w.cursor) {
                        *s = !*s;
                    }
                }
                KeyCode::Enter => {
                    if w.live_selected.iter().any(|s| *s) {
                        w.clamp_profile();
                        w.step = Step::Settings;
                        w.settings_row = 0;
                    }
                }
                _ => {}
            },

            Step::FixtureScope => match code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    w.step = Step::PickTarget;
                    w.cursor = 0;
                }
                KeyCode::Up => w.cursor = w.cursor.saturating_sub(1),
                KeyCode::Down => w.cursor = (w.cursor + 1).min(1),
                KeyCode::Enter | KeyCode::Char(' ') => {
                    if w.cursor == 0 {
                        w.fixture_all = true;
                        w.clamp_profile();
                        w.step = Step::Settings;
                        w.settings_row = 0;
                    } else {
                        w.fixture_all = false;
                        w.step = Step::FixtureModel;
                    }
                    w.cursor = 0;
                }
                _ => {}
            },

            Step::FixtureModel => {
                let models = fixture_model_list();
                match code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        w.step = Step::FixtureScope;
                        w.cursor = 0;
                    }
                    KeyCode::Up => w.cursor = w.cursor.saturating_sub(1),
                    KeyCode::Down => w.cursor = (w.cursor + 1).min(models.len().saturating_sub(1)),
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        w.fixture_model = models[w.cursor].clone();
                        w.clamp_profile();
                        w.step = Step::Settings;
                        w.settings_row = 0;
                    }
                    _ => {}
                }
            }

            Step::Settings => {
                let profiles = selectable_profiles(w.target);
                match code {
                    KeyCode::Esc => {
                        w.step = match w.target {
                            Target::Demo => {
                                if w.fixture_all {
                                    Step::FixtureScope
                                } else {
                                    Step::FixtureModel
                                }
                            }
                            Target::Live => Step::LivePickModels,
                        };
                        w.cursor = 0;
                    }
                    KeyCode::Up => w.settings_row = w.settings_row.saturating_sub(1),
                    KeyCode::Down => w.settings_row = (w.settings_row + 1).min(3),
                    KeyCode::Left => match w.settings_row {
                        0 => {
                            cycle_profile(&mut w, &profiles, false);
                            w.apply_profile_defaults();
                        }
                        1 => w.conc = step_down(w.conc, 16, 1),
                        2 => w.output_tokens = step_down(w.output_tokens, 16, 1),
                        _ => w.input_tokens = w.input_tokens.saturating_sub(64).max(16),
                    },
                    KeyCode::Right => match w.settings_row {
                        0 => {
                            cycle_profile(&mut w, &profiles, true);
                            w.apply_profile_defaults();
                        }
                        1 => w.conc = (w.conc + 16).min(1024),
                        2 => w.output_tokens = (w.output_tokens + 16).min(4096),
                        _ => w.input_tokens = (w.input_tokens + 64).min(32_768),
                    },
                    KeyCode::Enter => {
                        w.step = Step::Review;
                        // Default the cursor to the run row so review is
                        // "ready to submit" — not parked on the first field.
                        w.cursor = review_items(&w).len().saturating_sub(1);
                    }
                    _ => {}
                }
            }

            Step::Review => {
                let items = review_items(&w);
                match code {
                    KeyCode::Char('q') | KeyCode::Esc => {
                        w.step = Step::Settings;
                        w.settings_row = 0;
                    }
                    KeyCode::Up => w.cursor = w.cursor.saturating_sub(1),
                    KeyCode::Down => w.cursor = (w.cursor + 1).min(items.len().saturating_sub(1)),
                    KeyCode::Enter | KeyCode::Char(' ') => {
                        match items[w.cursor].1.clone() {
                            ReviewAction::Goto(step) => {
                                if step == Step::LiveUrl {
                                    w.input = w.live_url.clone();
                                }
                                w.step = step;
                                w.cursor = 0;
                            }
                            ReviewAction::Run => {
                                if let Err(msg) = preflight(&w) {
                                    w.error = msg;
                                    w.return_to = Step::Review;
                                    w.step = Step::Error;
                                } else {
                                    crate::persist::save(&w.to_saved());
                                    let (scope, live_cfg) = build_run_inputs(&w);
                                    let mut run_cli = cli.clone();
                                    run_cli.input_tokens = w.input_tokens;
                                    run_cli.conc = Some(w.conc);
                                    run_cli.output_tokens = Some(w.output_tokens);
                                    match crate::profiles::start_run(
                                        &run_cli,
                                        w.target,
                                        w.profile,
                                        scope,
                                        live_cfg.as_ref(),
                                    )
                                    .await
                                    {
                                        Ok(handles) => {
                                            run_dashboard(terminal, admin, handles).await?;
                                            // Reload the just-saved spec so the next
                                            // run pre-fills the same endpoint.
                                            let saved = crate::persist::load();
                                            w = Wizard::from_saved(cli, &saved);
                                        }
                                        Err(e) => {
                                            w.error = e.to_string();
                                            w.return_to = Step::Review;
                                            w.step = Step::Error;
                                        }
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }

            Step::Error => {
                // Any key dismisses and returns to where the error came from.
                w.step = w.return_to.clone();
                w.cursor = 0;
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
    let mut live = crate::admin::FairshareLive {
        global_in_flight: 0,
        global_queued: 0,
    };

    // Destructure so we can move `handle` out after the loop.
    let crate::profiles::RunHandles {
        stats,
        stop,
        handle,
        plan,
        ui_base,
        profile_name,
        teardown,
        gateway_observable,
        key_labels,
        key_counts,
    } = handles;

    // Rolling instantaneous throughput for the sparkline (one sample/second).
    let mut rps_hist: VecDeque<u64> = VecDeque::with_capacity(240);
    let mut last_sample = Instant::now();
    let mut last_completed: u64 = 0;
    // Whether the user asked to quit vs. the engine finishing on its own.
    let mut user_quit = false;

    loop {
        // Refresh fairshare roughly every 2 s — only when obench owns the local
        // gateway (demo). A remote live gateway has no admin token, so we render
        // client-side metrics only and leave in_flight/queued at zero.
        if gateway_observable && last_live_refresh.elapsed() >= Duration::from_secs(2) {
            if let Ok(l) = admin.fairshare_live().await {
                live = l;
            }
            last_live_refresh = Instant::now();
        }

        let summary = {
            let s = stats.lock().unwrap();
            s.summarize(
                started.elapsed().as_secs_f64().max(1.0),
                plan.max_error_rate,
            )
        };

        // Sample instantaneous req/s once per second.
        if last_sample.elapsed() >= Duration::from_secs(1) {
            let dt = last_sample.elapsed().as_secs_f64().max(0.001);
            let delta = summary.completed.saturating_sub(last_completed);
            rps_hist.push_back((delta as f64 / dt).round() as u64);
            while rps_hist.len() > 240 {
                rps_hist.pop_front();
            }
            last_completed = summary.completed;
            last_sample = Instant::now();
        }

        let hist: Vec<u64> = rps_hist.iter().copied().collect();
        let counts: Vec<u64> = key_counts
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect();
        let frame = dashboard::DashFrame {
            title: &profile_name,
            elapsed_s: started.elapsed().as_secs(),
            summary: &summary,
            rps_history: &hist,
            in_flight: live.global_in_flight,
            queued: live.global_queued,
            capacity: plan.capacity,
            ui_base: &ui_base,
            gateway_observable,
            key_labels: &key_labels,
            key_counts: &counts,
            complete: false,
        };
        terminal.draw(|f| dashboard::draw(f, &frame))?;

        if event::poll(Duration::from_millis(250))? {
            if let Event::Key(k) = event::read()? {
                if k.kind == KeyEventKind::Press
                    && matches!(k.code, KeyCode::Char('q') | KeyCode::Esc)
                {
                    user_quit = true;
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
        }

        // Engine finished on its own (duration elapsed or stop set externally).
        if stop.load(Ordering::Relaxed) {
            break;
        }
    }

    // Drain: wait for the engine task to finish before returning.
    // The engine exits promptly once `stop` is set; this just ensures in-flight
    // requests complete before we leave the alternate screen.
    let _ = handle.await;

    // If the run completed on its own (not a user quit), park on a final frame
    // showing the last numbers until the user acknowledges with a keypress.
    if !user_quit {
        let summary = {
            let s = stats.lock().unwrap();
            s.summarize(
                started.elapsed().as_secs_f64().max(1.0),
                plan.max_error_rate,
            )
        };
        let hist: Vec<u64> = rps_hist.iter().copied().collect();
        let counts: Vec<u64> = key_counts
            .iter()
            .map(|c| c.load(Ordering::Relaxed))
            .collect();
        loop {
            let frame = dashboard::DashFrame {
                title: &profile_name,
                elapsed_s: started.elapsed().as_secs(),
                summary: &summary,
                rps_history: &hist,
                in_flight: 0,
                queued: 0,
                capacity: plan.capacity,
                ui_base: &ui_base,
                gateway_observable,
                key_labels: &key_labels,
                key_counts: &counts,
                complete: true,
            };
            terminal.draw(|f| dashboard::draw(f, &frame))?;
            if event::poll(Duration::from_millis(250))? {
                if let Event::Key(k) = event::read()? {
                    if k.kind == KeyEventKind::Press {
                        break;
                    }
                }
            }
        }
    }

    // Tear down the synthetic models/tenants/keys obench created for this run.
    // API key secrets are never written to disk and are removed here.
    admin.teardown(&teardown).await;

    Ok(())
}

// ── Draw helpers ──────────────────────────────────────────────────────────────

fn draw_pick_target(f: &mut Frame, cursor: usize, snap: &overview::Snapshot) {
    let area = f.area();
    let outer = Layout::vertical([Constraint::Min(8), Constraint::Length(3)]).split(area);

    // Top band splits into a left "controls" column and a right "brand" column.
    let cols = Layout::horizontal([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(outer[0]);

    let left = Layout::vertical([
        Constraint::Length(3),
        Constraint::Length(4),
        Constraint::Min(6),
    ])
    .split(cols[0]);

    let title = Paragraph::new("what do you want to benchmark?")
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
    f.render_widget(title, left[0]);

    // Compact deployment snapshot so the user sees what's already running on the
    // gateway before choosing a target.
    let snapshot = Paragraph::new(vec![
        Line::from(Span::styled(
            format!(
                "gateway capacity {}  ·  in_flight {}  ·  queued {}",
                snap.capacity, snap.in_flight, snap.queued
            ),
            Style::default().fg(theme::FG),
        )),
        Line::from(Span::styled(
            format!(
                "{} models · {} tenants registered",
                snap.models.len(),
                snap.tenants.len()
            ),
            Style::default().fg(theme::MUTED),
        )),
    ])
    .block(
        Block::default()
            .borders(Borders::ALL)
            .title("deployment")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(snapshot, left[1]);

    // (label, description) for each target. Rendered as a wrapping Paragraph so
    // the explanation is never truncated on narrow terminals.
    let targets: [(&str, &str); 2] = [
        (
            "demo",
            "Local, GPU-free benchmark backend. Seeds demo obench-* models + tenants for you, drives the local gateway, then tears it all down. No real keys, no cost.",
        ),
        (
            "live",
            "A remote obleth gateway you already run. You supply its URL + one or more real tenant keys; obench drives load as a black-box client. Sends real, billable requests.",
        ),
    ];

    let mut body: Vec<Line> = Vec::new();
    for (i, (label, desc)) in targets.iter().enumerate() {
        let selected = i == cursor;
        let prefix = if selected { "▶ " } else { "  " };
        let color = if selected { theme::ACCENT } else { theme::FG };
        let modifier = if selected {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        body.push(Line::from(Span::styled(
            format!("{prefix}{label}"),
            Style::default().fg(color).add_modifier(modifier),
        )));
        body.push(Line::from(Span::styled(
            format!("    {desc}"),
            Style::default().fg(theme::MUTED),
        )));
        body.push(Line::from(""));
    }
    let para = Paragraph::new(body).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("target")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(para, left[2]);

    // Right column: the obleth emblem + wordmark lockup, centred in its panel.
    let brand_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::MUTED));
    let inner = brand_block.inner(cols[1]);
    f.render_widget(brand_block, cols[1]);
    let logo = art::lockup_lines("load & fairshare benchmark", inner.width, inner.height);
    let pad = (inner.height as usize).saturating_sub(logo.len()) / 2;
    let mut framed: Vec<Line> = Vec::with_capacity(logo.len() + pad);
    for _ in 0..pad {
        framed.push(Line::from(""));
    }
    framed.extend(logo);
    // The emblem rows are all equal width, so per-line centring keeps the circle
    // intact while also centring the wordmark + subtitle beneath it.
    f.render_widget(Paragraph::new(framed).alignment(Alignment::Center), inner);

    let help = Paragraph::new("[↑/↓] move   [Enter] select   [q/Esc] quit")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, outer[1]);
}

fn draw_settings(f: &mut Frame, w: &Wizard, profiles: &[(Profile, bool)]) {
    let row = w.settings_row;
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(8),
        Constraint::Length(3),
    ])
    .split(area);

    let title = Paragraph::new("obench — settings")
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

    let enabled: Vec<String> = profiles
        .iter()
        .filter(|(_, e)| *e)
        .map(|(p, _)| format!("{p:?}").to_lowercase())
        .collect();
    let pname = format!("{:?}", w.profile).to_lowercase();

    let row_style = |i: usize| {
        if i == row {
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme::FG)
        }
    };
    let caret = |i: usize| if i == row { "▶ " } else { "  " };
    let muted = |s: String| Line::from(Span::styled(s, Style::default().fg(theme::MUTED)));

    // Rough closed-loop expectation: rps ≈ concurrency / per-request latency.
    // We can't know latency ahead of time, but we can hint at the levers.
    let body = vec![
        Line::from(Span::styled(
            format!("{}load profile  ◄ {pname} ►", caret(0)),
            row_style(0),
        )),
        muted(format!(
            "    preset baseline · available: {}",
            enabled.join(", ")
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("{}concurrency   ◄ {} workers ►", caret(1), w.conc),
            row_style(1),
        )),
        muted("    parallel in-flight requests — the main throughput lever".into()),
        Line::from(""),
        Line::from(Span::styled(
            format!("{}max output    ◄ {} tokens ►", caret(2), w.output_tokens),
            row_style(2),
        )),
        muted("    completion length cap — fewer tokens = faster requests".into()),
        Line::from(""),
        Line::from(Span::styled(
            format!("{}prompt size   ◄ {} tokens ►", caret(3), w.input_tokens),
            row_style(3),
        )),
        muted("    approximate input/context tokens sent per request".into()),
    ];
    let para = Paragraph::new(body).block(
        Block::default()
            .borders(Borders::ALL)
            .title("settings")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(para, chunks[1]);

    let help = Paragraph::new("[↑/↓] field   [←/→] change   [Enter] review   [Esc] back")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}

fn draw_pick_scope(f: &mut Frame, target: Target, scopes: &[(&str, &str)], cursor: usize) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(3),
    ])
    .split(area);

    let title = Paragraph::new(format!("obench — scope  ({target:?})"))
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

    let mut body: Vec<Line> = Vec::new();
    for (i, (label, desc)) in scopes.iter().enumerate() {
        let selected = i == cursor;
        let prefix = if selected { "▶ " } else { "  " };
        let color = if selected { theme::ACCENT } else { theme::FG };
        let modifier = if selected {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        body.push(Line::from(Span::styled(
            format!("{prefix}{label}"),
            Style::default().fg(color).add_modifier(modifier),
        )));
        body.push(Line::from(Span::styled(
            format!("    {desc}"),
            Style::default().fg(theme::MUTED),
        )));
        body.push(Line::from(""));
    }
    let para = Paragraph::new(body).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("scope")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(para, chunks[1]);

    let help = Paragraph::new("[↑/↓] move   [Enter] select   [q/Esc] back")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}

/// Single-line title/prompt + editable text field. `masked` renders the value as
/// bullets (used for the API key).
fn draw_text_input(
    f: &mut Frame,
    title: &str,
    prompt: &str,
    hint: &str,
    value: &str,
    masked: bool,
) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(area);

    let title_w = Paragraph::new(format!("obench — {title}"))
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
    f.render_widget(title_w, chunks[0]);

    let shown = if masked {
        value.chars().map(|_| '•').collect::<String>()
    } else {
        value.to_string()
    };
    let body = vec![
        Line::from(Span::styled(prompt, Style::default().fg(theme::FG))),
        Line::from(Span::styled(hint, Style::default().fg(theme::MUTED))),
        Line::from(""),
        Line::from(Span::styled(
            format!("> {shown}_"),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )),
    ];
    let para = Paragraph::new(body).block(
        Block::default()
            .borders(Borders::ALL)
            .title("input")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(para, chunks[1]);

    let help = Paragraph::new("[type] edit   [Enter] continue   [Esc] back")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}

/// Multi-select list of upstream models pulled from the live endpoint.
fn draw_multiselect(
    f: &mut Frame,
    base: &str,
    models: &[String],
    selected: &[bool],
    cursor: usize,
) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(area);

    let count = selected.iter().filter(|s| **s).count();
    let title = Paragraph::new(format!(
        "obench — pick models to benchmark  ({count} selected, from {base})"
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

    let items: Vec<ListItem> = models
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let on = selected.get(i).copied().unwrap_or(false);
            let box_ = if on { "[x]" } else { "[ ]" };
            let selected_row = i == cursor;
            let prefix = if selected_row { "▶ " } else { "  " };
            let color = if selected_row {
                theme::ACCENT
            } else {
                theme::FG
            };
            let modifier = if selected_row {
                Modifier::BOLD
            } else {
                Modifier::empty()
            };
            ListItem::new(format!("{prefix}{box_} {name}"))
                .style(Style::default().fg(color).add_modifier(modifier))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("models")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(list, chunks[1]);

    let help = Paragraph::new("[↑/↓] move   [Space] toggle   [Enter] continue   [Esc] back")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}

/// Single-choice list of model names (fixture single-model scope).
fn draw_model_picker(f: &mut Frame, title: &str, subtitle: &str, models: &[String], cursor: usize) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(5),
        Constraint::Length(3),
    ])
    .split(area);

    let title_w = Paragraph::new(format!("obench — {title}"))
        .style(
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        )
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(subtitle)
                .border_style(Style::default().fg(theme::MUTED)),
        );
    f.render_widget(title_w, chunks[0]);

    let items: Vec<ListItem> = models
        .iter()
        .enumerate()
        .map(|(i, name)| {
            let selected = i == cursor;
            let prefix = if selected { "▶ " } else { "  " };
            let color = if selected { theme::ACCENT } else { theme::FG };
            let modifier = if selected {
                Modifier::BOLD
            } else {
                Modifier::empty()
            };
            ListItem::new(format!("{prefix}{name}"))
                .style(Style::default().fg(color).add_modifier(modifier))
        })
        .collect();
    let list = List::new(items).block(
        Block::default()
            .borders(Borders::ALL)
            .title("models")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(list, chunks[1]);

    let help = Paragraph::new("[↑/↓] move   [Enter] select   [q/Esc] back")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}

/// List of tenant keys for a live run, with add/remove/weight controls.
fn draw_keys_list(f: &mut Frame, url: &str, keys: &[KeyEntry], cursor: usize) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(3),
    ])
    .split(area);

    let title = Paragraph::new(format!("obench — tenant keys  (gateway {url})"))
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

    let body: Vec<Line> = if keys.is_empty() {
        vec![
            Line::from(Span::styled(
                "no keys yet — press [a] to add a tenant API key",
                Style::default().fg(theme::MUTED),
            )),
            Line::from(Span::styled(
                "each key is a distinct tenant; add 2+ to drive fairshare contention",
                Style::default().fg(theme::MUTED),
            )),
        ]
    } else {
        keys.iter()
            .enumerate()
            .map(|(i, k)| {
                let sel = i == cursor;
                let prefix = if sel { "▶ " } else { "  " };
                let color = if sel { theme::ACCENT } else { theme::FG };
                let modifier = if sel {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                };
                let secret = if k.secret.trim().is_empty() {
                    "(secret needed this session)".to_string()
                } else {
                    format!("••••{}", &k.secret[k.secret.len().saturating_sub(4)..])
                };
                Line::from(Span::styled(
                    format!("{prefix}{}  weight {}  {secret}", k.label, k.weight),
                    Style::default().fg(color).add_modifier(modifier),
                ))
            })
            .collect()
    };
    let para = Paragraph::new(body).block(
        Block::default()
            .borders(Borders::ALL)
            .title("keys")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(para, chunks[1]);

    let help = Paragraph::new(
        "[a] add  [d] delete  [←/→] weight  [↑/↓] move  [Enter] list models  [Esc] back",
    )
    .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}

/// Editable review screen — every setting is a row you can jump into, plus a
/// final "run" action.
fn draw_review(f: &mut Frame, rows: &[String], note: Option<&str>, cursor: usize) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(6),
        Constraint::Length(if note.is_some() { 2 } else { 0 }),
        Constraint::Length(3),
    ])
    .split(area);

    let title = Paragraph::new("obench — review  (edit any row, then run)")
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

    let body: Vec<Line> = rows
        .iter()
        .enumerate()
        .map(|(i, label)| {
            let sel = i == cursor;
            let is_run = i == rows.len().saturating_sub(1);
            // The ▶ arrow only ever marks the actual cursor position.
            let prefix = if sel { "▶ " } else { "  " };
            let base = if is_run { theme::ACCENT } else { theme::FG };
            let color = if sel { theme::ACCENT } else { base };
            let modifier = if sel || is_run {
                Modifier::BOLD
            } else {
                Modifier::empty()
            };
            let text = format!("{prefix}{label}");
            Line::from(Span::styled(
                text,
                Style::default().fg(color).add_modifier(modifier),
            ))
        })
        .collect();
    let para = Paragraph::new(body).wrap(Wrap { trim: true }).block(
        Block::default()
            .borders(Borders::ALL)
            .title("plan")
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(para, chunks[1]);

    if let Some(n) = note {
        let note_w = Paragraph::new(n)
            .wrap(Wrap { trim: true })
            .style(Style::default().fg(theme::MUTED));
        f.render_widget(note_w, chunks[2]);
    }

    let help = Paragraph::new("[↑/↓] move   [Enter] edit / run   [Esc] back")
        .style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[3]);
}

/// Generic centered message box (used for "fetching…" and error screens).
fn draw_message(f: &mut Frame, title: &str, lines: &[String], color: Color) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(4),
        Constraint::Length(3),
    ])
    .split(area);

    let title_w = Paragraph::new(format!("obench — {title}"))
        .style(Style::default().fg(color).add_modifier(Modifier::BOLD))
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::MUTED)),
        );
    f.render_widget(title_w, chunks[0]);

    let body: Vec<Line> = lines
        .iter()
        .map(|l| Line::from(Span::styled(l.clone(), Style::default().fg(theme::FG))))
        .collect();
    let para = Paragraph::new(body).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::MUTED)),
    );
    f.render_widget(para, chunks[1]);

    let help = Paragraph::new("[any key] dismiss").style(Style::default().fg(theme::MUTED));
    f.render_widget(help, chunks[2]);
}
