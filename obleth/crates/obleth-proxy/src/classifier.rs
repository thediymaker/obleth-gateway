//! Tiny-model intent classifier for `auto` routing.
//!
//! When enabled and configured, an `auto` request is first shown to a small,
//! fast model (the "brain", e.g. a sub-1B model) whose only job is to map the
//! prompt to one or more routing tags from the fixed vocabulary. Those tags
//! bias [`crate::router::select_model`] toward the best-matched model.
//!
//! The brain is asked verdict-style (see [`crate::verdicts`]): one greedy
//! single-token call per routing tag (a yes/no) plus one for difficulty (a
//! 3-level score), fanned out concurrently over a byte-identical prompt
//! prefix, with the answer read off the first token's `top_logprobs`. This
//! replaced a free-text "reply with a JSON object" call: it cannot come back
//! unparseable, costs one output token per question instead of ~48 total,
//! and yields real probabilities, so tag selection is a threshold instead of
//! a substring match.
//!
//! The classifier is deliberately defensive: it is hard-timeout bounded, caches
//! results, and returns an empty tag list (difficulty 1) on any error so an
//! `auto` request is never blocked or failed, and never routed more expensive,
//! because the brain is slow or down. Callers fall back to cheap heuristics
//! (see [`crate::router::heuristic_intent`]) in that case.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use moka::future::Cache;
use obleth_config::{AutoRouterSettings, ResolvedModel};

use crate::router::Intent;
use crate::verdicts::prompt::{self as vprompt, LabelSemantics};
use crate::verdicts::types::Question;
use crate::verdicts::{answers, fan_out, QuestionCall, QuestionOutcome};

/// Hot-swappable classifier configuration plus a short-lived result cache.
#[derive(Clone)]
pub struct Classifier {
    settings: Arc<ArcSwap<AutoRouterSettings>>,
    /// hash(prompt + available tags) -> classified intent.
    cache: Cache<u64, Intent>,
}

impl Classifier {
    pub fn new(initial: AutoRouterSettings) -> Self {
        Self {
            settings: Arc::new(ArcSwap::from_pointee(initial)),
            cache: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
        }
    }

    /// Current settings snapshot (cheap `Arc` clone).
    pub fn settings(&self) -> Arc<AutoRouterSettings> {
        self.settings.load_full()
    }

    /// Replace the settings (called by the periodic refresh task).
    pub fn update(&self, settings: AutoRouterSettings) {
        self.settings.store(Arc::new(settings));
    }

    /// Classify `prompt` into a subset of `available_tags` (plus a difficulty)
    /// using `brain`.
    ///
    /// Returns `Intent::default()` (empty tags, difficulty 1) on timeout,
    /// transport error, or unparseable output; callers should treat empty tags
    /// as "no signal" and fall back to heuristics. Every failure path lowers
    /// difficulty rather than raising it.
    pub async fn classify(
        &self,
        http: &reqwest::Client,
        brain: &ResolvedModel,
        prompt: &str,
        available_tags: &[String],
    ) -> Intent {
        if available_tags.is_empty() {
            return Intent::default();
        }

        let key = cache_key(prompt, available_tags);
        if let Some(hit) = self.cache.get(&key).await {
            return hit;
        }

        let timeout = Duration::from_millis(self.settings().classifier_timeout_ms.max(1));
        let intent = match tokio::time::timeout(
            timeout,
            call_brain(http, brain, prompt, available_tags, timeout),
        )
        .await
        {
            Ok(Ok(intent)) => intent,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, model = %brain.model_name, "classifier call failed");
                Intent::default()
            }
            Err(_) => {
                tracing::debug!(model = %brain.model_name, "classifier call timed out");
                Intent::default()
            }
        };

        // Cache even empty/default results briefly to avoid hammering a flaky brain.
        self.cache.insert(key, intent.clone()).await;
        intent
    }
}

fn cache_key(prompt: &str, available_tags: &[String]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    prompt.hash(&mut hasher);
    for t in available_tags {
        t.hash(&mut hasher);
    }
    hasher.finish()
}

/// Cap on tag questions per classification. Each tag is one single-token
/// call; a vocabulary larger than this stops being a routing signal, so the
/// extra tags are simply not asked about (they can still arrive via
/// heuristics).
const MAX_TAG_QUESTIONS: usize = 16;

/// Above this renormalized p(yes) a tag question counts as "the tag applies".
const TAG_THRESHOLD: f64 = 0.5;

/// The fixed question id for the difficulty score; tag questions use
/// `tag:<name>`.
const DIFFICULTY_ID: &str = "difficulty";

/// Ask the brain one single-token verdict question per available tag plus one
/// for difficulty, concurrently, and assemble the [`Intent`] from the
/// first-token label distributions. Thinking and harmony-format brains are
/// carried by the same learned retries as `/v1/verdicts` ([`fan_out`]).
async fn call_brain(
    http: &reqwest::Client,
    brain: &ResolvedModel,
    prompt: &str,
    available_tags: &[String],
    timeout: Duration,
) -> anyhow::Result<Intent> {
    // Cap the prompt we forward so the classifier stays fast and cheap,
    // floored to a char boundary (`String::truncate` panics mid-code-point).
    let mut user = prompt.trim();
    if user.len() > 2_000 {
        let mut end = 2_000;
        while !user.is_char_boundary(end) {
            end -= 1;
        }
        user = &user[..end];
    }
    // Byte-identical across every question of one classification — the prefix
    // the backend's cache amortizes, exactly like a verdicts request's state.
    let system = format!(
        "You answer routing questions about the REQUEST below. Reply with exactly one \
         answer label and nothing else — no explanation, no punctuation, no preamble.\n\n\
         # Request\n{user}"
    );

    let mut questions: Vec<(String, Question)> =
        vec![(DIFFICULTY_ID.to_string(), difficulty_question())];
    for tag in available_tags.iter().take(MAX_TAG_QUESTIONS) {
        questions.push((format!("tag:{tag}"), tag_question(tag)));
    }

    let calls: Vec<QuestionCall> = questions
        .iter()
        .map(|(id, q)| {
            let labels = vprompt::labels_for(q);
            let user_msg = vprompt::render_user(q, &labels);
            QuestionCall {
                id: id.clone(),
                boolean: matches!(labels.semantics, LabelSemantics::Boolean),
                labels: labels.labels,
                body: vprompt::build_body(&brain.upstream_model, &system, &user_msg),
                prefill_body: vprompt::build_body_prefilled(
                    &brain.upstream_model,
                    &system,
                    &user_msg,
                ),
                splice_body: vprompt::build_body_spliced(&brain.upstream_model, &system, &user_msg),
            }
        })
        .collect();

    let outcomes = fan_out(
        http,
        &build_chat_url(&brain.api_base),
        brain.api_key.as_deref(),
        &brain.model_name,
        calls,
        timeout,
    )
    .await;
    Ok(intent_from_outcomes(&outcomes))
}

/// Difficulty as a 3-level score question; level i maps to difficulty i.
fn difficulty_question() -> Question {
    Question::Score {
        instructions: serde_json::json!("How hard is this request to answer well?"),
        criteria: vec![
            serde_json::json!("Simple or factual — a small model answers it well"),
            serde_json::json!("Moderate — needs solid general capability"),
            serde_json::json!("Hard — needs careful, multi-step reasoning"),
        ],
    }
}

/// One yes/no per routing tag, judged independently so unrelated tags never
/// compete for one answer slot.
fn tag_question(tag: &str) -> Question {
    Question::Boolean {
        instructions: serde_json::json!(format!(
            "Does the routing tag '{tag}' describe this request?"
        )),
        criteria: None,
    }
}

/// Assemble tags and difficulty from the per-question outcomes: tags whose
/// p(yes) clears [`TAG_THRESHOLD`], strongest first, capped at 3; difficulty
/// from the score question's argmax. Every failure path yields less — a
/// failed tag question is an unselected tag, a failed difficulty question is
/// difficulty 1 — so a confused brain makes routing cheaper, never more
/// expensive.
fn intent_from_outcomes(outcomes: &[QuestionOutcome]) -> Intent {
    let mut difficulty = 1u8;
    let mut chosen: Vec<(String, f64)> = Vec::new();
    for outcome in outcomes {
        let Ok(success) = &outcome.result else {
            continue;
        };
        if outcome.id == DIFFICULTY_ID {
            // Labels as [`difficulty_question`] renders them: A/B/C = 1/2/3.
            let labels: Vec<String> = ["A", "B", "C"].iter().map(|s| s.to_string()).collect();
            let (masses, in_set) = answers::label_masses(&success.entries, &labels, false);
            if let Ok(scored) = answers::score_masses(&masses, in_set) {
                difficulty = ((scored.argmax as u8) + 1).clamp(1, obleth_config::MAX_TIER_LEVEL);
            }
        } else if let Some(tag) = outcome.id.strip_prefix("tag:") {
            let labels: Vec<String> = ["yes", "no"].iter().map(|s| s.to_string()).collect();
            let (masses, in_set) = answers::label_masses(&success.entries, &labels, true);
            if let Ok(scored) = answers::score_masses(&masses, in_set) {
                if scored.probs[0] > TAG_THRESHOLD {
                    chosen.push((tag.to_string(), scored.probs[0]));
                }
            }
        }
    }
    chosen.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    chosen.truncate(3);
    Intent {
        tags: chosen.into_iter().map(|(t, _)| t).collect(),
        difficulty,
        source: crate::router::IntentSource::Classifier,
    }
}

fn build_chat_url(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    format!("{base}/chat/completions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verdicts::answers::TopLogprob;
    use crate::verdicts::QuestionSuccess;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn ok_outcome(id: &str, entries: &[(&str, f64)]) -> QuestionOutcome {
        QuestionOutcome {
            id: id.to_string(),
            result: Ok(QuestionSuccess {
                entries: entries
                    .iter()
                    .map(|(t, l)| TopLogprob {
                        token: t.to_string(),
                        logprob: *l,
                    })
                    .collect(),
                input_tokens: 10,
                output_tokens: 1,
                used_prefill: false,
                used_splice: false,
            }),
            start_ms: 0,
            duration_ms: 1,
        }
    }

    fn failed_outcome(id: &str) -> QuestionOutcome {
        QuestionOutcome {
            id: id.to_string(),
            result: Err("upstream returned 500".to_string()),
            start_ms: 0,
            duration_ms: 1,
        }
    }

    #[test]
    fn tags_above_threshold_are_chosen_strongest_first_and_capped_at_three() {
        let outcomes = vec![
            ok_outcome("difficulty", &[(" B", -0.1), (" A", -2.0), (" C", -3.0)]),
            ok_outcome("tag:coding", &[(" yes", -0.1), (" no", -2.5)]),
            ok_outcome("tag:math", &[(" no", -0.05), (" yes", -3.0)]),
            ok_outcome("tag:vision", &[(" yes", -0.3), (" no", -1.6)]),
            ok_outcome("tag:tools", &[(" yes", -0.2), (" no", -1.9)]),
            ok_outcome("tag:writing", &[(" yes", -0.25), (" no", -1.8)]),
        ];
        let intent = intent_from_outcomes(&outcomes);
        // Five tags say yes-ish, but math says no, and only the strongest
        // three survive, ordered by p(yes).
        assert_eq!(intent.tags, vec!["coding", "tools", "writing"]);
        assert_eq!(intent.difficulty, 2, "argmax level B = difficulty 2");
        assert!(matches!(
            intent.source,
            crate::router::IntentSource::Classifier
        ));
    }

    #[test]
    fn failures_and_out_of_set_answers_yield_the_cheap_default() {
        // A failed difficulty call, a failed tag call, and a tag whose answer
        // mass is entirely off the labels (a rambling brain) — nothing is
        // selected and routing stays cheap.
        let outcomes = vec![
            failed_outcome("difficulty"),
            failed_outcome("tag:coding"),
            ok_outcome("tag:math", &[("The", -0.1), ("Request", -2.0)]),
        ];
        let intent = intent_from_outcomes(&outcomes);
        assert!(intent.tags.is_empty());
        assert_eq!(intent.difficulty, 1);
    }

    #[test]
    fn a_borderline_tag_below_the_threshold_is_not_selected() {
        // p(yes) renormalizes to exactly 0.5 — not strictly above the
        // threshold, so the tag does not apply.
        let outcomes = vec![ok_outcome("tag:coding", &[(" yes", -1.0), (" no", -1.0)])];
        let intent = intent_from_outcomes(&outcomes);
        assert!(intent.tags.is_empty());
    }

    #[test]
    fn difficulty_is_clamped_to_the_tier_ceiling() {
        let outcomes = vec![ok_outcome(
            "difficulty",
            &[(" C", -0.05), (" B", -3.0), (" A", -4.0)],
        )];
        assert_eq!(intent_from_outcomes(&outcomes).difficulty, 3);
    }

    /// The exact strings the brain is sent, pinned so they cannot drift away
    /// from the training-time renderer that reproduces them — currently
    /// `distill/tasks/router_intent.py` in the rc-k8s-gaudi repository, which
    /// carries these same literals. A distilled classifier is trained on
    /// these bytes; changing one here without regenerating its training data
    /// silently degrades the deployed model, so this test is a deliberate
    /// cross-repository tripwire, not a tautology. Update both together.
    #[test]
    fn the_rendered_prompts_match_the_training_goldens() {
        let system = format!(
            "You answer routing questions about the REQUEST below. Reply with exactly one \
             answer label and nothing else — no explanation, no punctuation, no preamble.\n\n\
             # Request\n{}",
            "Write a Rust function that parses JSON"
        );
        assert_eq!(
            system,
            "You answer routing questions about the REQUEST below. Reply with exactly one \
             answer label and nothing else — no explanation, no punctuation, no preamble.\n\n\
             # Request\nWrite a Rust function that parses JSON"
        );

        let q = tag_question("coding");
        let rendered = vprompt::render_user(&q, &vprompt::labels_for(&q));
        assert_eq!(
            rendered,
            "# Question\nDoes the routing tag 'coding' describe this request?\n\nAnswer \"yes\" or \"no\"."
        );

        let d = difficulty_question();
        let labels = vprompt::labels_for(&d);
        assert_eq!(labels.labels, ["A", "B", "C"]);
        assert_eq!(
            vprompt::render_user(&d, &labels),
            "# Question\nHow hard is this request to answer well?\n\n# Levels\n\
             A) level 1 — Simple or factual — a small model answers it well\n\
             B) level 2 — Moderate — needs solid general capability\n\
             C) level 3 — Hard — needs careful, multi-step reasoning\n\
             \nAnswer with the letter of the level that best matches."
        );
    }

    fn brain(api_base: &str) -> ResolvedModel {
        ResolvedModel {
            model_name: "brain-test".to_string(),
            aliases: Vec::new(),
            quantization: "unknown".into(),
            upstream_model: "brain-upstream".to_string(),
            api_base: api_base.to_string(),
            api_key: None,
            model_type: "chat".to_string(),
            admission_weight: 1,
            max_in_flight: None,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 0,
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            context_window: 0,
            supports_function_calling: false,
            supports_system_messages: false,
            supports_response_schema: false,
            supports_tool_choice: false,
            supports_vision: false,
            tags: vec![],
            declared_levels: vec![],
            boons: vec![],
            tool_servers: vec![],
            knowledge_collections: vec![],
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: 200,
            endpoint_selection_mode: "failover".to_string(),
            debug_diagnostics: false,
            energy_slots_per_node: 0,
            route_bias: 1.0,
            auto_eligible: true,
            draft_model: String::new(),
            verify_api_base: String::new(),
            verify_upstream_model: String::new(),
            endpoints: vec![],
        }
    }

    /// A canned brain: answers every single-token question by looking at
    /// which question it was asked, exercising the real prompt construction,
    /// fan-out, and distribution math end to end.
    #[tokio::test]
    async fn classify_asks_single_token_questions_and_reads_the_labels() {
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let hits = hits_clone.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    assert_eq!(body["max_tokens"], 1, "single-token calls only");
                    assert_eq!(body["logprobs"], true);
                    let question = body["messages"][1]["content"].as_str().unwrap();
                    let top = if question.contains("How hard") {
                        serde_json::json!([
                            {"token": " C", "logprob": -0.1},
                            {"token": " A", "logprob": -3.0}
                        ])
                    } else if question.contains("\'coding\'") {
                        serde_json::json!([
                            {"token": " yes", "logprob": -0.05},
                            {"token": " no", "logprob": -3.5}
                        ])
                    } else {
                        serde_json::json!([
                            {"token": " no", "logprob": -0.05},
                            {"token": " yes", "logprob": -3.5}
                        ])
                    };
                    axum::Json(serde_json::json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": "x"},
                            "logprobs": {"content": [{
                                "token": "x", "logprob": -0.1, "top_logprobs": top
                            }]},
                            "finish_reason": "length"
                        }],
                        "usage": {"prompt_tokens": 50, "completion_tokens": 1}
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let settings: AutoRouterSettings = serde_json::from_value(serde_json::json!({})).unwrap();
        let classifier = Classifier::new(settings);
        let tags = vec!["coding".to_string(), "math".to_string()];
        let intent = classifier
            .classify(
                &reqwest::Client::new(),
                &brain(&format!("http://{addr}/v1")),
                "Write a Rust function that parses logprobs",
                &tags,
            )
            .await;
        assert_eq!(intent.tags, vec!["coding"]);
        assert_eq!(intent.difficulty, 3, "argmax level C");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            3,
            "one call per tag plus difficulty"
        );

        // The result is cached: a second classify makes no upstream calls.
        let again = classifier
            .classify(
                &reqwest::Client::new(),
                &brain(&format!("http://{addr}/v1")),
                "Write a Rust function that parses logprobs",
                &tags,
            )
            .await;
        assert_eq!(again.tags, vec!["coding"]);
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }
}
