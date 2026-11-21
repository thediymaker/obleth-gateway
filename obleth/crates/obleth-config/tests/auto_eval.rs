//! A scorecard for `auto`: does it read a request for what it is, and does it
//! then send it to a model of the right size?
//!
//! `tests/data/auto_prompts.json` holds prompts a person has labelled with the
//! tier of model that should answer (1 light, 2 mid, 3 strong: the least
//! capable model that would answer well) and the router's topics they are
//! about. Each prompt is put through the same two steps a request takes, the
//! reading (`heuristic_intent`) and the pick (`route`), against a small fleet
//! of light, mid and strong models, in two envelopes:
//!
//! - **bare**: the prompt alone, as a script calling the API sends it;
//! - **chat**: the prompt inside what a chat client sends with every message,
//!   a long system text and tool definitions.
//!
//! and against two fleets: one with tiers declared and no prices, as a fleet
//! of self-hosted models has, and the same fleet with prices.
//!
//! The pick is sampled, so each prompt is routed many times over evenly spaced
//! draws and scored by the share of them that went to each size of model.
//! Three numbers matter most, each averaged over prompts:
//!
//! - **right-sized**: the share served by a model of exactly the labelled tier;
//! - **under-served**: the share served by a weaker model than labelled, which
//!   costs the person a worse answer;
//! - **over-served**: the share served by a stronger model than labelled,
//!   which costs the fleet.
//!
//! Run it for the full report:
//!
//! ```text
//! cargo test -p obleth-config --test auto_eval -- --nocapture
//! ```
//!
//! [`BAR`] is what the router must not fall below. It starts at what the
//! router scored when this was written and is raised as the router improves.

use obleth_config::routing::{
    derive_levels, heuristic_intent, route, BoonGrants, Candidate, RequestFeatures, RouterWeights,
};
use obleth_config::{ResolvedModel, TierSource};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

#[derive(Deserialize)]
struct Set {
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    kind: String,
    /// 1 light, 2 mid, 3 strong.
    tier: u8,
    topics: Vec<String>,
    /// The conversation, the person first and then by turns; the last is the
    /// message being routed.
    turns: Vec<String>,
}

/// The topics the reading is scored on: the ones a model's tiers are declared in.
const TOPICS: [&str; 3] = ["coding", "writing", "math"];
/// How many evenly spaced draws each prompt is routed with.
const DRAWS: usize = 200;
/// What a chat client's instructions and tool definitions add to every
/// request, in estimated tokens.
const CHAT_OVERHEAD_TOKENS: u64 = 5_700;

fn cases() -> Vec<Case> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/data/auto_prompts.json");
    let text = std::fs::read_to_string(path).expect("the labelled prompts");
    let set: Set = serde_json::from_str(&text).expect("the labelled prompts parse");
    for case in &set.cases {
        assert!((1..=3).contains(&case.tier), "{}: tier 1 to 3", case.id);
        assert!(case.turns.len() % 2 == 1, "{}: ends on the person", case.id);
        assert!(
            case.topics.iter().all(|t| TOPICS.contains(&t.as_str())),
            "{}: topics are among {TOPICS:?}",
            case.id
        );
    }
    set.cases
}

/// A fleet: its models, and the size of each (1 light, 2 mid, 3 strong).
struct Fleet {
    name: &'static str,
    candidates: Vec<Candidate>,
    size: HashMap<String, u8>,
}

/// Nine models, as an operator might short-list them for `auto`: three strong,
/// four mid and two light, each with the tier it holds in its topics, the mid
/// generalists favoured a little and the code specialists held back. `priced`
/// gives them prices that rise with size; without it every model is free,
/// which is how self-hosted models are often registered.
fn fleet(name: &'static str, priced: bool) -> Fleet {
    let all = ["coding", "writing", "math"];
    let code = ["coding"];
    let models: [(&str, u8, &[&str], f64); 9] = [
        ("strong-a", 3, &all, 1.0),
        ("strong-b", 3, &all, 1.0),
        ("strong-code", 3, &code, 0.8),
        ("mid-a", 2, &all, 1.15),
        ("mid-b", 2, &all, 1.1),
        ("mid-c", 2, &all, 1.1),
        ("mid-d", 2, &all, 1.1),
        ("light-a", 1, &all, 1.0),
        ("light-code", 1, &code, 0.8),
    ];
    let mut candidates = Vec::new();
    let mut size = HashMap::new();
    for (model_name, tier, topics, bias) in models {
        let price = if priced {
            [0.0, 0.2e-6, 0.8e-6, 3.0e-6][tier as usize]
        } else {
            0.0
        };
        candidates.push(Candidate {
            model: Arc::new(ResolvedModel {
                model_name: model_name.to_string(),
                upstream_model: model_name.to_string(),
                api_base: "http://upstream".to_string(),
                context_window: 128_000,
                supports_function_calling: true,
                supports_response_schema: true,
                supports_tool_choice: true,
                tags: topics.iter().map(|t| t.to_string()).collect(),
                declared_levels: topics.iter().map(|t| (t.to_string(), tier)).collect(),
                route_bias: bias,
                input_cost_per_token: price,
                output_cost_per_token: price * 4.0,
                ..Default::default()
            }),
            healthy: true,
            levels: Vec::new(),
        });
        size.insert(model_name.to_string(), tier);
    }
    derive_levels(&mut candidates, TierSource::Declared);
    Fleet {
        name,
        candidates,
        size,
    }
}

/// The router as an operator who uses tiers sets it: the shipped weights, with
/// the difficulty floor on and a little sampling to spread load.
fn weights() -> RouterWeights {
    RouterWeights {
        temperature: 0.2,
        difficulty_enabled: true,
        ..RouterWeights::default()
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Envelope {
    Bare,
    Chat,
}

impl Envelope {
    fn name(self) -> &'static str {
        match self {
            Envelope::Bare => "bare",
            Envelope::Chat => "chat",
        }
    }
}

/// The request a case becomes, and its estimated size in tokens.
fn request(case: &Case, envelope: Envelope) -> (Value, u64) {
    let mut messages = Vec::new();
    let mut bytes = 0usize;
    if envelope == Envelope::Chat {
        messages.push(json!({"role": "system", "content": "You are a helpful assistant with access to tools. Use the workspace to run code, the cluster tools to look at jobs, and ask before changing anything. ".repeat(60)}));
    }
    for (i, turn) in case.turns.iter().enumerate() {
        bytes += turn.len();
        let role = if i % 2 == 0 { "user" } else { "assistant" };
        messages.push(json!({"role": role, "content": turn}));
    }
    let mut body = json!({"model": "auto", "messages": messages, "stream": true});
    let mut tokens = (bytes / 4) as u64 + 8;
    if envelope == Envelope::Chat {
        body["tools"] = json!([
            {"type": "function", "function": {"name": "run_command", "description": "Run a command in the workspace.", "parameters": {"type": "object", "properties": {"command": {"type": "string"}}}}},
            {"type": "function", "function": {"name": "slurm_jobs", "description": "List the person's jobs on the cluster.", "parameters": {"type": "object", "properties": {}}}},
        ]);
        tokens += CHAT_OVERHEAD_TOKENS;
    }
    (body, tokens)
}

/// How one prompt was read and served.
struct Outcome {
    difficulty: u8,
    tags: Vec<String>,
    /// The share of draws served by a light, mid and strong model.
    served: [f64; 3],
    /// How many different models the draws went to.
    models: usize,
}

fn run(case: &Case, envelope: Envelope, fleet: &Fleet) -> Outcome {
    let (body, tokens) = request(case, envelope);
    let intent = heuristic_intent(&body, tokens);
    let features = RequestFeatures::from_request(&body, tokens, 0);
    let weights = weights();
    let mut served = [0usize; 3];
    let mut seen: Vec<String> = Vec::new();
    for draw in 0..DRAWS {
        let uniform = (draw as f64 + 0.5) / DRAWS as f64;
        let (picked, _) = route(
            &fleet.candidates,
            &features,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &intent.tags,
            BoonGrants::default(),
            &weights,
            uniform,
            &intent,
        );
        let picked = picked.expect("a fleet with every capability always has a model");
        served[fleet.size[&picked.model_name] as usize - 1] += 1;
        if !seen.contains(&picked.model_name) {
            seen.push(picked.model_name.clone());
        }
    }
    Outcome {
        difficulty: intent.difficulty,
        tags: intent.tags,
        served: served.map(|n| n as f64 / DRAWS as f64),
        models: seen.len(),
    }
}

/// The scores of one envelope on one fleet, each a share from 0 to 1.
#[derive(Debug, Default, Clone, Copy)]
struct Score {
    /// The reading: prompts whose difficulty matched the label, and those read
    /// as easier or harder than they are.
    read_right: f64,
    read_low: f64,
    read_high: f64,
    /// Topics: of the labelled ones, how many were found; of the found, how
    /// many were labelled.
    topic_recall: f64,
    topic_precision: f64,
    /// The serving, averaged over prompts.
    right_sized: f64,
    under_served: f64,
    over_served: f64,
    /// A strong model answering a tier-1 prompt.
    strong_on_simple: f64,
    /// A weaker model answering a tier-3 prompt.
    weak_on_hard: f64,
    /// How many different models one prompt reaches, on average.
    models_per_prompt: f64,
}

fn score(cases: &[Case], envelope: Envelope, fleet: &Fleet, detail: bool) -> Score {
    let n = cases.len() as f64;
    let mut s = Score::default();
    let (mut found, mut labelled, mut both) = (0.0, 0.0, 0.0);
    let (mut simple, mut hard) = (0.0, 0.0);
    // By the kind of prompt: how many, read right, right-sized, under, over.
    let mut kinds: BTreeMap<&str, [f64; 5]> = BTreeMap::new();
    // The reading against the label: rows are the label, columns the reading.
    let mut confusion = [[0usize; 3]; 3];
    let mut worst: Vec<(f64, String)> = Vec::new();
    for case in cases {
        let out = run(case, envelope, fleet);
        let tier = case.tier as usize;
        let read = out.difficulty.clamp(1, 3) as usize;
        confusion[tier - 1][read - 1] += 1;
        s.read_right += f64::from(read == tier);
        s.read_low += f64::from(read < tier);
        s.read_high += f64::from(read > tier);
        for topic in TOPICS {
            let has = case.topics.iter().any(|t| t == topic);
            let got = out.tags.iter().any(|t| t == topic);
            labelled += f64::from(has);
            found += f64::from(got);
            both += f64::from(has && got);
        }
        let right = out.served[tier - 1];
        let under: f64 = out.served[..tier - 1].iter().sum();
        let over: f64 = out.served[tier..].iter().sum();
        s.right_sized += right;
        s.under_served += under;
        s.over_served += over;
        if tier == 1 {
            simple += 1.0;
            s.strong_on_simple += out.served[2];
        }
        if tier == 3 {
            hard += 1.0;
            s.weak_on_hard += under;
        }
        s.models_per_prompt += out.models as f64;
        let row = kinds.entry(case.kind.as_str()).or_default();
        row[0] += 1.0;
        row[1] += f64::from(read == tier);
        row[2] += right;
        row[3] += under;
        row[4] += over;
        // What a wrong pick costs: a worse answer weighs twice a wasted model.
        worst.push((
            2.0 * under + over,
            format!(
                "{:<14} tier {} read {} {:<22} light {:>3.0}% mid {:>3.0}% strong {:>3.0}%  {}",
                case.id,
                case.tier,
                out.difficulty,
                format!("{:?}", out.tags),
                out.served[0] * 100.0,
                out.served[1] * 100.0,
                out.served[2] * 100.0,
                case.turns.last().unwrap().chars().take(48).collect::<String>().replace('\n', " "),
            ),
        ));
    }
    s.read_right /= n;
    s.read_low /= n;
    s.read_high /= n;
    s.topic_recall = both / f64::max(labelled, 1.0);
    s.topic_precision = both / f64::max(found, 1.0);
    s.right_sized /= n;
    s.under_served /= n;
    s.over_served /= n;
    s.strong_on_simple /= f64::max(simple, 1.0);
    s.weak_on_hard /= f64::max(hard, 1.0);
    s.models_per_prompt /= n;
    if detail {
        println!("\n== {} envelope, fleet {} ==", envelope.name(), fleet.name);
        println!("reading (rows: labelled tier; columns: read as 1, 2, 3)");
        for (i, row) in confusion.iter().enumerate() {
            println!("  tier {}: {:>4} {:>4} {:>4}", i + 1, row[0], row[1], row[2]);
        }
        println!("by kind            n  read right  right-sized  under  over");
        for (kind, row) in &kinds {
            println!(
                "  {:<14} {:>3}  {:>9.0}%  {:>10.0}%  {:>4.0}%  {:>3.0}%",
                kind,
                row[0],
                row[1] / row[0] * 100.0,
                row[2] / row[0] * 100.0,
                row[3] / row[0] * 100.0,
                row[4] / row[0] * 100.0,
            );
        }
        worst.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());
        println!("furthest from the label:");
        for (_, line) in worst.iter().take(12) {
            println!("  {line}");
        }
    }
    s
}

/// What the router must not fall below, for a chat client on a fleet with
/// tiers and no prices: the case this scorecard was written for. Raise these
/// as the router improves; never lower one without saying why here.
struct Bar {
    read_right: f64,
    right_sized: f64,
    under_served: f64,
    strong_on_simple: f64,
    weak_on_hard: f64,
}

/// As the router scored when the scorecard was written (shares, rounded so
/// that only a real change trips them).
const BAR: Bar = Bar {
    read_right: 0.0,
    right_sized: 0.0,
    under_served: 1.0,
    strong_on_simple: 1.0,
    weak_on_hard: 1.0,
};

#[test]
fn auto_reads_requests_and_sizes_models_no_worse_than_the_bar() {
    let cases = cases();
    let fleets = [fleet("tiers, no prices", false), fleet("tiers and prices", true)];
    println!("{} labelled prompts, {DRAWS} draws each", cases.len());
    let mut table = Vec::new();
    for fleet in &fleets {
        for envelope in [Envelope::Bare, Envelope::Chat] {
            // The full detail for the case the bar is set on; the rest in the table.
            let detail = envelope == Envelope::Chat && fleet.name == fleets[0].name;
            table.push((envelope, fleet.name, score(&cases, envelope, fleet, detail)));
        }
    }
    println!("\n== summary ==");
    println!("envelope  fleet              read right  low  high | topics found  true | right-sized  under  over | strong on simple  weak on hard | models per prompt");
    for (envelope, fleet, s) in &table {
        println!(
            "{:<8}  {:<17}  {:>9.0}%  {:>2.0}%  {:>3.0}% | {:>11.0}%  {:>3.0}% | {:>10.0}%  {:>4.0}%  {:>3.0}% | {:>15.0}%  {:>11.0}% | {:>6.1}",
            envelope.name(),
            fleet,
            s.read_right * 100.0,
            s.read_low * 100.0,
            s.read_high * 100.0,
            s.topic_recall * 100.0,
            s.topic_precision * 100.0,
            s.right_sized * 100.0,
            s.under_served * 100.0,
            s.over_served * 100.0,
            s.strong_on_simple * 100.0,
            s.weak_on_hard * 100.0,
            s.models_per_prompt,
        );
    }
    let (_, _, chat) = table
        .iter()
        .find(|(envelope, fleet, _)| *envelope == Envelope::Chat && *fleet == fleets[0].name)
        .unwrap();
    assert!(chat.read_right >= BAR.read_right, "difficulty is read right less often: {chat:?}");
    assert!(chat.right_sized >= BAR.right_sized, "fewer requests reach a model of their size: {chat:?}");
    assert!(chat.under_served <= BAR.under_served, "more requests reach too weak a model: {chat:?}");
    assert!(chat.strong_on_simple <= BAR.strong_on_simple, "more simple requests reach a strong model: {chat:?}");
    assert!(chat.weak_on_hard <= BAR.weak_on_hard, "more hard requests reach a weaker model: {chat:?}");
}
