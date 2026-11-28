//! A scorecard for the classifier: how well does a brain read a request, and
//! how fast?
//!
//! The prompts are the labelled ones `obleth-config`'s `auto_eval` scores the
//! heuristics on (`tests/data/auto_prompts*.json`): each says which tier of
//! model should answer it and which topics it is about. Every prompt goes the
//! way a request does, through [`classifier_prompt`] and the same single-token
//! questions, so what is scored is what the gateway asks. Three readings are
//! reported side by side:
//!
//! - **the classifier's own**, on the prompts it is asked about and answers;
//! - **the heuristics'**, on every prompt;
//! - **the gateway's**: the classifier's answer where it gave one and the
//!   heuristics' where it did not, as for small talk, which it is not asked
//!   about.
//!
//! It needs a model, so it runs only when asked for:
//!
//! ```text
//! OBLETH_EVAL_BASE=http://127.0.0.1:8000/v1 OBLETH_EVAL_MODEL=my-judge \
//!   cargo test -p obleth-proxy classifier::eval -- --ignored --nocapture
//! ```
//!
//! - `OBLETH_EVAL_BASE`: the brain's OpenAI-style base address, as a model's
//!   `api_base` is given;
//! - `OBLETH_EVAL_MODEL`: the model name that address serves it under;
//! - `OBLETH_EVAL_KEY`: a bearer key, when the address wants one;
//! - `OBLETH_EVAL_CONCURRENCY`: prompts in flight at once (default 1, which
//!   times a classification as a lone request sees it). Each is two questions
//!   sent together, as in the gateway;
//! - `OBLETH_EVAL_BAR`: the share of held-out prompts whose difficulty must be
//!   read right, such as `0.9`. Without it the test only reports;
//! - `OBLETH_EVAL_PROMPTS`: a file of your own labelled prompts, in the format
//!   of the two sets, scored after them.
//!
//! Accuracy is measured with a long timeout, apart from speed. The report then
//! says how long a classification took and what share came back inside the
//! default `classifier_timeout_ms`, which is what a request would wait.
//!
//! Which model reads requests best is not obvious from its size, and the
//! wording of the questions suits some models better than others: score a
//! brain before setting it as the classifier.

use super::*;
use crate::router::{classifier_prompt, heuristic_intent, is_small_talk, Intent};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Instant;

#[derive(Deserialize)]
struct Set {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    tier: u8,
    topics: Vec<String>,
    turns: Vec<String>,
}

/// The two sets, as `auto_eval` names them.
const SETS: [(&str, &str); 2] = [
    ("tuned on", "auto_prompts.json"),
    ("held out", "auto_prompts_held_out.json"),
];

/// The topics the prompts are labelled with. A tag outside them (`general`,
/// `fast`, ...) is neither right nor wrong here.
const LABELLED_TOPICS: [&str; 3] = ["coding", "math", "writing"];

/// Long enough that a slow brain still answers: speed is reported apart.
const PATIENCE: Duration = Duration::from_secs(60);

fn cases(path: &std::path::Path) -> Vec<Case> {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str::<Set>(&text)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .cases
}

/// The chat request a case becomes: its turns, the person's and the model's
/// in alternation, with no system text.
fn request(case: &Case) -> (Value, u64) {
    let messages: Vec<Value> = case
        .turns
        .iter()
        .enumerate()
        .map(|(i, turn)| json!({"role": if i % 2 == 0 { "user" } else { "assistant" }, "content": turn}))
        .collect();
    let bytes: usize = case.turns.iter().map(String::len).sum();
    (
        json!({"model": "auto", "messages": messages, "stream": true}),
        (bytes / 4) as u64 + 8,
    )
}

/// One reading of one prompt.
#[derive(Clone, Default)]
struct Reading {
    difficulty: u8,
    tags: Vec<String>,
}

impl From<Intent> for Reading {
    fn from(intent: Intent) -> Self {
        Reading {
            difficulty: intent.difficulty,
            tags: intent.tags,
        }
    }
}

/// How a prompt was read each way.
struct Read {
    /// What the classifier said; `None` when it was not asked.
    classified: Option<Classified>,
    heuristics: Reading,
    /// What the gateway routes on: the two settled.
    gateway: Reading,
    /// The newest message is small talk, which the gateway reads by rule.
    small_talk: bool,
    /// Questions whose call failed.
    failed: usize,
    /// How long the classification took, in milliseconds.
    took: Option<f64>,
}

#[derive(Default)]
struct Tally {
    n: usize,
    right: usize,
    low: usize,
    high: usize,
    grid: [[usize; 3]; 3],
    topics_labelled: usize,
    topics_found: usize,
    topics_named: usize,
    topics_true: usize,
}

impl Tally {
    fn add(&mut self, case: &Case, reading: &Reading) {
        self.n += 1;
        let got = reading.difficulty.clamp(1, 3);
        match got.cmp(&case.tier) {
            std::cmp::Ordering::Equal => self.right += 1,
            std::cmp::Ordering::Less => self.low += 1,
            std::cmp::Ordering::Greater => self.high += 1,
        }
        self.grid[(case.tier - 1) as usize][(got - 1) as usize] += 1;
        self.topics_labelled += case.topics.len();
        self.topics_found += case
            .topics
            .iter()
            .filter(|t| reading.tags.contains(t))
            .count();
        let named: Vec<&String> = reading
            .tags
            .iter()
            .filter(|t| LABELLED_TOPICS.contains(&t.as_str()))
            .collect();
        self.topics_named += named.len();
        self.topics_true += named.iter().filter(|t| case.topics.contains(t)).count();
    }

    fn share(part: usize, whole: usize) -> f64 {
        if whole == 0 {
            1.0
        } else {
            part as f64 / whole as f64
        }
    }

    fn read_right(&self) -> f64 {
        Self::share(self.right, self.n)
    }

    fn line(&self, name: &str) -> String {
        format!(
            "  {name:<30} right {:>4.0}%   low {:>4.0}%   high {:>4.0}%   topics found {:>4.0}% / true {:>4.0}%   1→{:?} 2→{:?} 3→{:?}",
            100.0 * self.read_right(),
            100.0 * Self::share(self.low, self.n),
            100.0 * Self::share(self.high, self.n),
            100.0 * Self::share(self.topics_found, self.topics_labelled),
            100.0 * Self::share(self.topics_true, self.topics_named),
            self.grid[0],
            self.grid[1],
            self.grid[2],
        )
    }
}

fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let at = ((sorted.len() as f64 - 1.0) * p).round() as usize;
    sorted[at]
}

/// Read one case the way a request is read.
async fn read(http: &reqwest::Client, brain: &ResolvedModel, tags: &[String], case: &Case) -> Read {
    let (body, tokens) = request(case);
    let heuristics = heuristic_intent(&body, tokens);
    let (mut classified, mut failed, mut took) = (None, 0, None);
    if let Some(prompt) = classifier_prompt(&body, Some(0)) {
        let started = Instant::now();
        let outcomes = ask(http, brain, &prompt, routing_questions(tags), PATIENCE).await;
        took = Some(started.elapsed().as_secs_f64() * 1000.0);
        classified = Some(match outcomes {
            Ok(outcomes) => {
                failed = outcomes.iter().filter(|o| o.result.is_err()).count();
                classified_from_outcomes(&outcomes, tags)
            }
            Err(_) => {
                failed = routing_questions(tags).len();
                Classified::default()
            }
        });
    }
    Read {
        gateway: classified
            .clone()
            .unwrap_or_default()
            .settle(heuristics.clone())
            .into(),
        classified,
        heuristics: heuristics.into(),
        small_talk: is_small_talk(&body),
        failed,
        took,
    }
}

#[tokio::test]
#[ignore = "needs a model: set OBLETH_EVAL_BASE and OBLETH_EVAL_MODEL"]
async fn the_classifier_reads_the_labelled_prompts() {
    let var = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let (Some(base), Some(model)) = (var("OBLETH_EVAL_BASE"), var("OBLETH_EVAL_MODEL")) else {
        panic!("set OBLETH_EVAL_BASE and OBLETH_EVAL_MODEL to the brain to score");
    };
    let mut brain = tests::brain(&base);
    brain.model_name = model.clone();
    brain.upstream_model = model.clone();
    brain.api_key = var("OBLETH_EVAL_KEY");
    let concurrency: usize = var("OBLETH_EVAL_CONCURRENCY")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    let bar: Option<f64> = var("OBLETH_EVAL_BAR").and_then(|v| v.parse().ok());
    let budget = AutoRouterSettings::default().classifier_timeout_ms as f64;
    let tags: Vec<String> = obleth_config::MODEL_TAGS
        .iter()
        .map(|t| t.to_string())
        .collect();
    let http = reqwest::Client::new();

    println!(
        "\nclassifier scorecard: {model} at {base}, {} questions a prompt",
        routing_questions(&tags).len()
    );
    let mut held_out_right = None;
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../obleth-config/tests/data");
    let mut sets: Vec<(&str, PathBuf)> = SETS
        .iter()
        .map(|(name, file)| (*name, data.join(file)))
        .collect();
    if let Some(own) = var("OBLETH_EVAL_PROMPTS") {
        sets.push(("your prompts", PathBuf::from(own)));
    }
    for (name, path) in sets {
        let cases = cases(&path);
        let (http, brain, tags) = (&http, &brain, &tags);
        let mut reads: Vec<(usize, Read)> = futures::stream::iter(cases.iter().enumerate())
            .map(|(i, case)| async move { (i, read(http, brain, tags, case).await) })
            .buffer_unordered(concurrency)
            .collect()
            .await;
        reads.sort_by_key(|(i, _)| *i);

        let (mut classifier, mut heuristics, mut gateway) =
            (Tally::default(), Tally::default(), Tally::default());
        let (mut not_asked, mut small_talk, mut no_difficulty, mut no_topic, mut failed) =
            (0usize, 0usize, 0usize, 0usize, 0usize);
        let mut took: Vec<f64> = Vec::new();
        let mut misses: Vec<String> = Vec::new();
        for (i, read) in &reads {
            let case = &cases[*i];
            heuristics.add(case, &read.heuristics);
            gateway.add(case, &read.gateway);
            failed += read.failed;
            small_talk += read.small_talk as usize;
            took.extend(read.took);
            let Some(classified) = &read.classified else {
                not_asked += 1;
                continue;
            };
            no_difficulty += classified.difficulty.is_none() as usize;
            no_topic += classified.tags.is_none() as usize;
            if let (Some(difficulty), Some(tags)) = (classified.difficulty, &classified.tags) {
                classifier.add(
                    case,
                    &Reading {
                        difficulty,
                        tags: tags.clone(),
                    },
                );
            }
            if read.gateway.difficulty != case.tier {
                misses.push(format!(
                    "      {:<24} labelled {}, read {} (heuristics {})  {:?}",
                    case.id,
                    case.tier,
                    read.gateway.difficulty,
                    read.heuristics.difficulty,
                    case.turns
                        .last()
                        .map(|t| t.chars().take(90).collect::<String>())
                        .unwrap_or_default()
                ));
            }
        }
        took.sort_by(|a, b| a.total_cmp(b));
        let inside = took.iter().filter(|ms| **ms <= budget).count();

        println!("\n{name}: {} prompts", cases.len());
        println!("{}", classifier.line("classifier, where it answered"));
        println!("{}", heuristics.line("heuristics"));
        println!("{}", gateway.line("as the gateway reads"));
        println!(
            "  not asked: {not_asked} ({small_talk} small talk); no difficulty answer: {no_difficulty}; no topic answer: {no_topic}; calls failed: {failed}"
        );
        println!(
            "  a classification took p50 {:.0} ms, p95 {:.0} ms, {:.0}% inside the default {budget:.0} ms",
            percentile(&took, 0.5),
            percentile(&took, 0.95),
            100.0 * Tally::share(inside, took.len()),
        );
        if !misses.is_empty() {
            println!("    difficulty the gateway read differently from the label:");
            for miss in &misses {
                println!("{miss}");
            }
        }
        if name == "held out" {
            held_out_right = Some(gateway.read_right());
        }
    }

    if let (Some(bar), Some(right)) = (bar, held_out_right) {
        assert!(
            right >= bar,
            "held-out prompts read right {:.0}%, below the bar of {:.0}%",
            100.0 * right,
            100.0 * bar
        );
    }
}
