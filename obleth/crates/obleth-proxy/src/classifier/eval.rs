//! A scorecard for the classifier: how well does a brain read a request, and
//! how fast?
//!
//! The prompts are the labelled ones `obleth-config`'s `auto_eval` scores the
//! heuristics on (`tests/data/auto_prompts*.json`): each says which tier of
//! model should answer it and which topics it is about. Every prompt goes the
//! way a request does, through [`classifier_prompts`], [`classify_turns`] and
//! the same single-token questions, so what is scored is what the gateway
//! asks. The heuristics read the same prompts beside it, and so does the
//! order the gateway uses: the classifier's answer, or the heuristics' when it
//! gives none or is not asked, as for small talk.
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
//! - `OBLETH_EVAL_CONCURRENCY`: prompts in flight at once (default 4). Each is
//!   ten questions sent together, as in the gateway;
//! - `OBLETH_EVAL_BAR`: the share of held-out prompts whose difficulty must be
//!   read right, such as `0.85`. Without it the test only reports.
//!
//! Accuracy is measured with a long timeout, apart from speed. The report then
//! says how long a classification took and what share came back inside the
//! default `classifier_timeout_ms`, which is what a request would wait.

use super::*;
use crate::router::{classifier_prompts, classify_turns, heuristic_intent, is_small_talk};
use futures::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::Mutex;
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

fn cases(file: &str) -> Vec<Case> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../obleth-config/tests/data")
        .join(file);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
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
    /// The classifier's own answer; `None` when it gave no tags at all, which
    /// is the shape of a failure.
    classifier: Option<Reading>,
    heuristics: Reading,
    /// The newest message is small talk, which the gateway reads by rule.
    small_talk: bool,
    /// Questions whose call failed.
    unanswered: usize,
    /// Classifications whose difficulty question had no usable answer, which
    /// the gateway reads as difficulty 1.
    no_difficulty: usize,
    /// How long each fan-out took, in milliseconds.
    took: Vec<f64>,
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
    let unanswered = Mutex::new(0usize);
    let no_difficulty = Mutex::new(0usize);
    let took = Mutex::new(Vec::new());
    let classified = classify_turns(classifier_prompts(&body, Some(0)), |prompt| {
        let (unanswered, no_difficulty, took) = (&unanswered, &no_difficulty, &took);
        async move {
            let started = Instant::now();
            let outcomes = ask(http, brain, &prompt, routing_questions(tags), PATIENCE).await;
            took.lock()
                .unwrap()
                .push(started.elapsed().as_secs_f64() * 1000.0);
            match outcomes {
                Ok(outcomes) => {
                    *unanswered.lock().unwrap() +=
                        outcomes.iter().filter(|o| o.result.is_err()).count();
                    if difficulty_from_outcomes(&outcomes).is_none() {
                        *no_difficulty.lock().unwrap() += 1;
                    }
                    intent_from_outcomes(&outcomes)
                }
                Err(_) => {
                    *unanswered.lock().unwrap() += tags.len() + 1;
                    *no_difficulty.lock().unwrap() += 1;
                    Intent::default()
                }
            }
        }
    })
    .await;
    Read {
        classifier: classified.map(Reading::from),
        heuristics: heuristic_intent(&body, tokens).into(),
        small_talk: is_small_talk(&body),
        unanswered: unanswered.into_inner().unwrap(),
        no_difficulty: no_difficulty.into_inner().unwrap(),
        took: took.into_inner().unwrap(),
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
        .unwrap_or(4)
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
        tags.len() + 1
    );
    let mut held_out_right = None;
    for (name, file) in SETS {
        let cases = cases(file);
        let (http, brain, tags) = (&http, &brain, &tags);
        let mut reads: Vec<(usize, Read)> = futures::stream::iter(cases.iter().enumerate())
            .map(|(i, case)| async move { (i, read(http, brain, tags, case).await) })
            .buffer_unordered(concurrency)
            .collect()
            .await;
        reads.sort_by_key(|(i, _)| *i);

        let (mut classifier, mut heuristics, mut gateway) =
            (Tally::default(), Tally::default(), Tally::default());
        let (mut silent, mut unanswered, mut no_difficulty, mut small_talk) =
            (0usize, 0usize, 0usize, 0usize);
        let mut took: Vec<f64> = Vec::new();
        let mut misses: Vec<String> = Vec::new();
        for (i, read) in &reads {
            let case = &cases[*i];
            heuristics.add(case, &read.heuristics);
            unanswered += read.unanswered;
            no_difficulty += read.no_difficulty;
            small_talk += read.small_talk as usize;
            took.extend(&read.took);
            match &read.classifier {
                Some(reading) => {
                    classifier.add(case, reading);
                    gateway.add(case, reading);
                    if reading.difficulty != case.tier {
                        misses.push(format!(
                            "      {:<24} labelled {}, read {} (heuristics {})  {:?}",
                            case.id,
                            case.tier,
                            reading.difficulty,
                            read.heuristics.difficulty,
                            case.turns
                                .last()
                                .map(|t| t.chars().take(90).collect::<String>())
                        ));
                    }
                }
                None => {
                    silent += 1;
                    gateway.add(case, &read.heuristics);
                }
            }
        }
        took.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let inside = took.iter().filter(|ms| **ms <= budget).count();

        println!("\n{name}: {} prompts", cases.len());
        println!("{}", classifier.line("classifier, where it answered"));
        println!("{}", heuristics.line("heuristics"));
        println!("{}", gateway.line("as the gateway reads"));
        println!(
            "  left to the heuristics: {silent} prompts ({small_talk} of them small talk, not asked about); no difficulty answer on {no_difficulty} classifications; {unanswered} calls failed"
        );
        println!(
            "  a classification took p50 {:.0} ms, p95 {:.0} ms, {:.0}% inside the default {budget:.0} ms",
            percentile(&took, 0.5),
            percentile(&took, 0.95),
            100.0 * Tally::share(inside, took.len()),
        );
        if !misses.is_empty() {
            println!("    difficulty the classifier read differently from the label:");
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
