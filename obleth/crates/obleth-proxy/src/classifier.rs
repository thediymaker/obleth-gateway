//! The intent classifier for `auto` routing.
//!
//! When enabled and configured, an `auto` request is first shown to a fast
//! model (the "brain") whose only job is to say how hard the request is and
//! what it is about. Those answers pick the tier of model and bias
//! [`crate::router::select_model`] toward the best-matched one.
//!
//! The brain is asked verdict-style (see [`crate::verdicts`]), two questions
//! sent together over a byte-identical prompt prefix, each a greedy
//! single-token call whose answer is read off the first token's
//! `top_logprobs`:
//!
//! - **difficulty**, a score over three levels that spell out what a light, a
//!   mid and a strong model are for;
//! - **topic**, one choice among the routing tags that name a subject, with
//!   `general` for none of them.
//!
//! An earlier design asked one yes/no per routing tag, ten calls a request.
//! Asked cold, models say yes to most tags or to none, and ten calls took
//! longer than the timeout allows; one choice names the topic more reliably
//! and the two calls come back in about the time of one. What the levels say
//! matters as much as which model reads them: the same models read difficulty
//! right far more often with the levels spelled out than with "simple",
//! "moderate" and "hard". `classifier::eval` scores a brain on labelled
//! prompts; run it before changing a word here.
//!
//! The classifier is deliberately defensive: it is hard-timeout bounded, caches
//! results, and returns no answer on any error, so an `auto` request is never
//! blocked or failed because the brain is slow or down. Callers settle what it
//! returns against the heuristics ([`Classified::settle`]), which answer
//! whatever the brain did not.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use moka::future::Cache;
use obleth_config::{AutoRouterSettings, ResolvedModel};

use crate::router::Classified;
use crate::verdicts::prompt::{self as vprompt, LabelSemantics};
use crate::verdicts::types::Question;
use crate::verdicts::{answers, fan_out, QuestionCall, QuestionOutcome, SharedPrompt};

/// Hot-swappable classifier configuration plus a short-lived result cache.
#[derive(Clone)]
pub struct Classifier {
    settings: Arc<ArcSwap<AutoRouterSettings>>,
    /// hash(brain + prompt + available tags) -> what the brain said.
    cache: Cache<u64, Classified>,
    /// hash(brain + prompt + categories) -> the categories that apply.
    categories: Cache<u64, Vec<String>>,
    /// hash(brain + message) -> p(the message says the last answer was wrong).
    complaints: Cache<u64, Option<f64>>,
}

impl Classifier {
    pub fn new(initial: AutoRouterSettings) -> Self {
        Self {
            settings: Arc::new(ArcSwap::from_pointee(initial)),
            cache: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
            categories: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
            complaints: Cache::builder()
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

    /// Ask `brain` how hard `prompt` is and which of `available_tags` it is
    /// about.
    ///
    /// Returns no answer (`Classified::default()`) on timeout or transport
    /// error, and no answer for a question whose reply was not one of its
    /// labels; callers settle the rest from the heuristics.
    pub async fn classify(
        &self,
        http: &reqwest::Client,
        brain: &ResolvedModel,
        prompt: &str,
        available_tags: &[String],
    ) -> Classified {
        let key = cache_key(&brain.model_name, prompt, available_tags);
        if let Some(hit) = self.cache.get(&key).await {
            return hit;
        }

        let timeout = Duration::from_millis(self.settings().classifier_timeout_ms.max(1));
        let classified = match tokio::time::timeout(
            timeout,
            call_brain(http, brain, prompt, available_tags, timeout),
        )
        .await
        {
            Ok(Ok(classified)) => classified,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, model = %brain.model_name, "classifier call failed");
                Classified::default()
            }
            Err(_) => {
                tracing::debug!(model = %brain.model_name, "classifier call timed out");
                Classified::default()
            }
        };

        // Cache even an empty result briefly to avoid hammering a flaky brain.
        self.cache.insert(key, classified.clone()).await;
        classified
    }
}

impl Classifier {
    /// Which of `categories` describe `prompt`, strongest first, at most
    /// three: one yes/no question per category, for a caller with its own
    /// vocabulary (the speculation boon's category gates). Empty on timeout,
    /// transport error, or when the brain says yes to none.
    pub async fn categories(
        &self,
        http: &reqwest::Client,
        brain: &ResolvedModel,
        prompt: &str,
        categories: &[String],
    ) -> Vec<String> {
        if categories.is_empty() {
            return Vec::new();
        }
        let key = cache_key(&brain.model_name, prompt, categories);
        if let Some(hit) = self.categories.get(&key).await {
            return hit;
        }
        let timeout = Duration::from_millis(self.settings().classifier_timeout_ms.max(1));
        let questions: Vec<(String, Question)> = categories
            .iter()
            .take(MAX_CATEGORY_QUESTIONS)
            .map(|c| (format!("tag:{c}"), category_question(c)))
            .collect();
        let chosen =
            match tokio::time::timeout(timeout, ask(http, brain, prompt, questions, timeout)).await
            {
                Ok(Ok(outcomes)) => categories_from_outcomes(&outcomes),
                Ok(Err(e)) => {
                    tracing::debug!(error = %e, model = %brain.model_name, "category call failed");
                    Vec::new()
                }
                Err(_) => {
                    tracing::debug!(model = %brain.model_name, "category call timed out");
                    Vec::new()
                }
            };
        self.categories.insert(key, chosen.clone()).await;
        chosen
    }

    /// p(yes) that `message` says the previous answer was wrong, did not
    /// work, or missed what was asked; `None` when the brain gave no usable
    /// answer. Bounded by the same timeout as [`Classifier::classify`].
    pub async fn complaint(
        &self,
        http: &reqwest::Client,
        brain: &ResolvedModel,
        message: &str,
    ) -> Option<f64> {
        let key = cache_key(&brain.model_name, message, &[COMPLAINT_ID.to_string()]);
        if let Some(hit) = self.complaints.get(&key).await {
            return hit;
        }
        let timeout = Duration::from_millis(self.settings().classifier_timeout_ms.max(1));
        let questions = vec![(COMPLAINT_ID.to_string(), complaint_question())];
        let p = match tokio::time::timeout(timeout, ask(http, brain, message, questions, timeout))
            .await
        {
            Ok(Ok(outcomes)) => complaint_from_outcomes(&outcomes),
            Ok(Err(e)) => {
                tracing::debug!(error = %e, model = %brain.model_name, "complaint call failed");
                None
            }
            Err(_) => {
                tracing::debug!(model = %brain.model_name, "complaint call timed out");
                None
            }
        };
        self.complaints.insert(key, p).await;
        p
    }
}

/// The brain is part of the key: after `classifier_model` changes, answers
/// cached from the previous brain -- including the empty ones cached after a
/// failure -- must not be served as the new brain's for the rest of the TTL.
fn cache_key(brain: &str, prompt: &str, available_tags: &[String]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    brain.hash(&mut hasher);
    prompt.hash(&mut hasher);
    for t in available_tags {
        t.hash(&mut hasher);
    }
    hasher.finish()
}

/// Cap on category questions per call. Each category is one single-token
/// call; the ones past the cap are not asked about.
const MAX_CATEGORY_QUESTIONS: usize = 16;

/// Above this renormalized p(yes) a category question counts as "it applies".
const CATEGORY_THRESHOLD: f64 = 0.5;

/// The fixed question id for the difficulty score.
const DIFFICULTY_ID: &str = "difficulty";

/// The fixed question id for the topic choice.
const TOPIC_ID: &str = "topic";

/// The question id for "does the user say the last answer was wrong".
const COMPLAINT_ID: &str = "complaint";

/// The share of the answer that must be on "strong" for a request to be read
/// as hard. Below an even three-way split on purpose: a hard request sent to a
/// weaker model costs the person the answer, an ordinary one sent to a
/// stronger model costs the fleet a little.
const HARD_THRESHOLD: f64 = 0.3;

/// A second topic is named beside the first when it holds at least this share
/// of the answer: a request that is about two things evenly.
const SECOND_TOPIC_THRESHOLD: f64 = 0.3;

/// The routing tags that name what a request is about, and how each is put to
/// the brain. `general` is the answer for none of the others. The remaining
/// tags are not asked about: `vision` and `long-context` show in the request
/// itself, and `fast` describes a model, not a request.
const TOPICS: [(&str, &str); 6] = [
    (
        "coding",
        "code, scripts, queries, commands or configuration to write, read, fix or run",
    ),
    (
        "creative",
        "fiction, verse, jokes, role-play or other imaginative writing",
    ),
    (
        GENERAL_TOPIC,
        "none of the others: conversation, a question of fact, advice, an explanation",
    ),
    (
        "math",
        "a calculation, an equation, a proof, statistics or a word problem with quantities",
    ),
    (
        "reasoning",
        "a puzzle, a plan, an analysis or a decision to think through that is neither code nor math",
    ),
    (
        "writing",
        "prose to draft, edit, shorten, translate or proofread: an email, a letter, an essay, a post",
    ),
];

/// The topic that stands for "none of the others". It is always on offer, so
/// the brain is never made to pick a subject a request does not have.
const GENERAL_TOPIC: &str = "general";

/// Ask the brain its two questions about `prompt`, together, and read the
/// answers off the first-token label distributions. Thinking and
/// harmony-format brains are carried by the same learned retries as
/// `/v1/verdicts` ([`fan_out`]).
async fn call_brain(
    http: &reqwest::Client,
    brain: &ResolvedModel,
    prompt: &str,
    available_tags: &[String],
    timeout: Duration,
) -> anyhow::Result<Classified> {
    let questions = routing_questions(available_tags);
    let outcomes = ask(http, brain, prompt, questions, timeout).await?;
    Ok(classified_from_outcomes(&outcomes, available_tags))
}

/// What one classification asks: the difficulty score, and the topic choice
/// when a model on offer carries a tag that names a subject.
fn routing_questions(available_tags: &[String]) -> Vec<(String, Question)> {
    let mut questions = vec![(DIFFICULTY_ID.to_string(), difficulty_question())];
    if let Some(topic) = topic_question(available_tags) {
        questions.push((TOPIC_ID.to_string(), topic));
    }
    questions
}

/// The topics on offer for `available_tags`, in the order their labels are
/// assigned: the tags that name a subject, and `general` always.
fn topics_on_offer(available_tags: &[String]) -> Vec<&'static str> {
    TOPICS
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| *name == GENERAL_TOPIC || available_tags.iter().any(|t| t == name))
        .collect()
}

/// Ask the brain `questions` about `prompt`, one single-token call each,
/// concurrently over a shared prompt prefix.
async fn ask(
    http: &reqwest::Client,
    brain: &ResolvedModel,
    prompt: &str,
    questions: Vec<(String, Question)>,
    timeout: Duration,
) -> anyhow::Result<Vec<QuestionOutcome>> {
    // Cap the prompt we forward so the classifier stays fast and cheap,
    // floored to a char boundary (`String::truncate` panics mid-code-point).
    let mut user = prompt.trim();
    if user.len() > crate::router::CLASSIFIER_PROMPT_MAX_BYTES {
        let mut end = crate::router::CLASSIFIER_PROMPT_MAX_BYTES;
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

    let shared = std::sync::Arc::new(SharedPrompt {
        upstream_model: brain.upstream_model.clone(),
        system,
        images: Vec::new(),
    });
    let calls: Vec<QuestionCall> = questions
        .iter()
        .map(|(id, q)| {
            let labels = vprompt::labels_for(q);
            QuestionCall {
                id: id.clone(),
                boolean: matches!(labels.semantics, LabelSemantics::Boolean),
                two_letter: vprompt::is_two_letter(&labels.labels),
                user: vprompt::render_user(q, &labels),
                labels: labels.labels,
                shared: shared.clone(),
            }
        })
        .collect();

    // Endpoint-aware like every other helper call: a brain provisioned on
    // Slurm has a blank `api_base` and lives only in `endpoints`.
    let target = crate::boons::helper_target(brain, "", None)?;
    let outcomes = fan_out(
        http,
        &crate::boons::build_chat_url(&target.base),
        target.api_key.as_deref(),
        &target.headers,
        &brain.model_name,
        calls,
        timeout,
    )
    .await;
    Ok(outcomes)
}

/// Difficulty as a 3-level score question; level i maps to difficulty i. Each
/// level says what that size of model is for, in the terms of the requests
/// that reach it.
fn difficulty_question() -> Question {
    Question::Score {
        instructions: serde_json::json!("Which level of model does this request need?"),
        criteria: vec![
            serde_json::json!(
                "Light: small talk, a quick fact or a definition, the name of a command, or a tiny edit to a short text given in the request"
            ),
            serde_json::json!(
                "Mid: everyday work such as writing or explaining code, drafting or revising a text, a routine calculation, a summary or step-by-step instructions"
            ),
            serde_json::json!(
                "Strong: hard reasoning such as a proof or a derivation, tracking down an intermittent or unexplained fault, designing a system, weighing trade-offs among several options, or building something with many strict requirements at once"
            ),
        ],
    }
}

/// One choice among the topics on offer, or `None` when `general` would be
/// the only option: there is then nothing to ask.
fn topic_question(available_tags: &[String]) -> Option<Question> {
    let offered = topics_on_offer(available_tags);
    if offered.len() < 2 {
        return None;
    }
    Some(Question::Choice {
        instructions: serde_json::json!("What is this request mainly about?"),
        criteria: TOPICS
            .iter()
            .filter(|(name, _)| offered.contains(name))
            .map(|(name, what)| (name.to_string(), serde_json::json!(what)))
            .collect(),
    })
}

/// One yes/no per category, judged independently so unrelated categories
/// never compete for one answer slot.
fn category_question(category: &str) -> Question {
    Question::Boolean {
        instructions: serde_json::json!(format!(
            "Does the routing tag '{category}' describe this request?"
        )),
        criteria: None,
    }
}

/// The categories whose p(yes) clears [`CATEGORY_THRESHOLD`], strongest
/// first, capped at 3. A failed question is a category that does not apply.
fn categories_from_outcomes(outcomes: &[QuestionOutcome]) -> Vec<String> {
    let labels: Vec<String> = ["yes", "no"].iter().map(|s| s.to_string()).collect();
    let mut chosen: Vec<(String, f64)> = Vec::new();
    for outcome in outcomes {
        let (Ok(success), Some(category)) = (&outcome.result, outcome.id.strip_prefix("tag:"))
        else {
            continue;
        };
        let (masses, in_set) = answers::label_masses(&success.entries, &labels, true);
        if let Ok(scored) = answers::score_masses(&masses, in_set) {
            if scored.probs[0] > CATEGORY_THRESHOLD {
                chosen.push((category.to_string(), scored.probs[0]));
            }
        }
    }
    chosen.sort_by(|a, b| b.1.total_cmp(&a.1));
    drop_exclusive_losers(&mut chosen);
    chosen.truncate(3);
    chosen.into_iter().map(|(c, _)| c).collect()
}

/// Whether the user is reporting a problem with the answer they got.
fn complaint_question() -> Question {
    Question::Boolean {
        instructions: serde_json::json!(
            "Is this message saying that a previous answer was wrong, did not work, \
             or missed what was asked?"
        ),
        criteria: None,
    }
}

/// p(yes) of the complaint question, or `None` when it failed.
fn complaint_from_outcomes(outcomes: &[QuestionOutcome]) -> Option<f64> {
    let outcome = outcomes.iter().find(|o| o.id == COMPLAINT_ID)?;
    let success = outcome.result.as_ref().ok()?;
    let labels: Vec<String> = ["yes", "no"].iter().map(|s| s.to_string()).collect();
    let (masses, in_set) = answers::label_masses(&success.entries, &labels, true);
    answers::score_masses(&masses, in_set)
        .ok()
        .map(|scored| scored.probs[0])
}

/// What the brain said, from the per-question outcomes. A question with no
/// usable answer leaves its part `None`, for the heuristics to settle.
fn classified_from_outcomes(outcomes: &[QuestionOutcome], available_tags: &[String]) -> Classified {
    Classified {
        difficulty: difficulty_from_outcomes(outcomes),
        tags: topics_from_outcomes(outcomes, available_tags),
    }
}

/// The difficulty question's answer, or `None` when the call failed or the
/// brain put its first token outside the level labels. Level 3 at
/// [`HARD_THRESHOLD`] of the answer; otherwise the likelier of 1 and 2.
fn difficulty_from_outcomes(outcomes: &[QuestionOutcome]) -> Option<u8> {
    let outcome = outcomes.iter().find(|o| o.id == DIFFICULTY_ID)?;
    let success = outcome.result.as_ref().ok()?;
    // Labels as [`difficulty_question`] renders them: A/B/C = 1/2/3.
    let labels: Vec<String> = ["A", "B", "C"].iter().map(|s| s.to_string()).collect();
    let (masses, in_set) = answers::label_masses(&success.entries, &labels, false);
    let probs = answers::score_masses(&masses, in_set).ok()?.probs;
    Some(if probs[2] >= HARD_THRESHOLD {
        3
    } else if probs[0] >= probs[1] {
        1
    } else {
        2
    })
}

/// The topic question's answer as routing tags, strongest first: the chosen
/// topic, and a second when the answer is split between two. `general` is a
/// tag only when it is the first choice: beside a subject it adds nothing.
/// `None` when the question was not asked, its call failed, or the brain put
/// its first token outside the option labels.
fn topics_from_outcomes(
    outcomes: &[QuestionOutcome],
    available_tags: &[String],
) -> Option<Vec<String>> {
    let outcome = outcomes.iter().find(|o| o.id == TOPIC_ID)?;
    let success = outcome.result.as_ref().ok()?;
    let offered = topics_on_offer(available_tags);
    // Labels as [`topic_question`] renders them: a letter per option, in the
    // options' sorted order, which is the order of [`TOPICS`].
    let labels: Vec<String> = (0..offered.len())
        .map(|i| ((b'A' + i as u8) as char).to_string())
        .collect();
    let (masses, in_set) = answers::label_masses(&success.entries, &labels, false);
    let probs = answers::score_masses(&masses, in_set).ok()?.probs;
    let mut ranked: Vec<(&str, f64)> = offered.iter().copied().zip(probs).collect();
    ranked.sort_by(|a, b| b.1.total_cmp(&a.1));

    let mut chosen: Vec<(String, f64)> = vec![(ranked[0].0.to_string(), ranked[0].1)];
    if let Some((second, p)) = ranked.get(1) {
        if *p >= SECOND_TOPIC_THRESHOLD && *second != GENERAL_TOPIC {
            chosen.push((second.to_string(), *p));
        }
    }
    drop_exclusive_losers(&mut chosen);
    Some(
        chosen
            .into_iter()
            .map(|(topic, _)| topic)
            // `general` is always offered; it is a tag only where a model carries it.
            .filter(|topic| available_tags.contains(topic))
            .collect(),
    )
}

/// Drop the weaker tag of each [`obleth_config::EXCLUSIVE_TAG_PAIRS`] pair
/// when both were chosen. `chosen` is sorted strongest first, so the weaker
/// one is whichever comes later. Runs before the cap, so the slot it frees
/// goes to the next tag.
fn drop_exclusive_losers(chosen: &mut Vec<(String, f64)>) {
    for (a, b) in obleth_config::EXCLUSIVE_TAG_PAIRS {
        let pa = chosen.iter().position(|(t, _)| t == a);
        let pb = chosen.iter().position(|(t, _)| t == b);
        if let (Some(pa), Some(pb)) = (pa, pb) {
            chosen.remove(pa.max(pb));
        }
    }
}

#[cfg(test)]
mod eval;

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
            result: Err(crate::verdicts::VerdictError::Upstream {
                status: 500,
                detail: None,
            }),
            start_ms: 0,
            duration_ms: 1,
        }
    }

    fn tags(names: &[&str]) -> Vec<String> {
        names.iter().map(|t| t.to_string()).collect()
    }

    #[test]
    fn the_levels_map_to_difficulty_and_hard_needs_less_than_half() {
        let read = |entries: &[(&str, f64)]| {
            difficulty_from_outcomes(&[ok_outcome("difficulty", entries)])
        };
        assert_eq!(read(&[(" A", -0.1), (" B", -2.5), (" C", -5.0)]), Some(1));
        assert_eq!(read(&[(" B", -0.1), (" A", -2.0), (" C", -3.0)]), Some(2));
        assert_eq!(read(&[(" C", -0.05), (" B", -3.0), (" A", -4.0)]), Some(3));
        // Mid is the likeliest level, but a third of the answer says strong:
        // the request is read as hard.
        assert_eq!(
            read(&[
                ("B", 0.55f64.ln()),
                ("C", 0.35f64.ln()),
                ("A", 0.10f64.ln())
            ]),
            Some(3)
        );
        assert_eq!(
            read(&[
                ("B", 0.65f64.ln()),
                ("C", 0.25f64.ln()),
                ("A", 0.10f64.ln())
            ]),
            Some(2)
        );
    }

    #[test]
    fn the_topic_is_the_chosen_option_among_the_tags_on_offer() {
        // Offered: coding (A), general (B), math (C), writing (D).
        let menu = tags(&["writing", "coding", "fast", "general", "math", "vision"]);
        let read =
            |entries: &[(&str, f64)]| topics_from_outcomes(&[ok_outcome("topic", entries)], &menu);
        assert_eq!(read(&[(" C", -0.05), (" A", -3.5)]), Some(tags(&["math"])));
        assert_eq!(
            read(&[(" B", -0.05), (" A", -3.5)]),
            Some(tags(&["general"]))
        );
        // An even split between two subjects names both, strongest first.
        assert_eq!(
            read(&[
                ("A", 0.55f64.ln()),
                ("C", 0.40f64.ln()),
                ("B", 0.05f64.ln())
            ]),
            Some(tags(&["coding", "math"]))
        );
        // `general` beside a subject adds nothing.
        assert_eq!(
            read(&[("A", 0.55f64.ln()), ("B", 0.45f64.ln())]),
            Some(tags(&["coding"]))
        );
    }

    #[test]
    fn coding_and_writing_keep_only_the_stronger() {
        // "Write a SQL query": the answer is split between coding and
        // writing. They are never applied together, so the stronger stays.
        let menu = tags(&["coding", "writing"]);
        // Offered: coding (A), general (B), writing (C).
        let outcomes = [ok_outcome(
            "topic",
            &[("A", 0.6f64.ln()), ("C", 0.4f64.ln())],
        )];
        assert_eq!(
            topics_from_outcomes(&outcomes, &menu),
            Some(tags(&["coding"]))
        );
    }

    #[test]
    fn general_is_always_offered_and_is_a_tag_only_where_a_model_carries_it() {
        // No model is tagged `general`: a request about none of the subjects
        // is answered, and has no tags.
        let menu = tags(&["coding", "math"]);
        assert_eq!(topics_on_offer(&menu), ["coding", "general", "math"]);
        let outcomes = [ok_outcome("topic", &[(" B", -0.05), (" A", -3.5)])];
        assert_eq!(topics_from_outcomes(&outcomes, &menu), Some(vec![]));
    }

    #[test]
    fn the_topic_is_not_asked_when_no_tag_on_offer_names_a_subject() {
        let menu = tags(&["fast", "vision", "long-context", "general"]);
        let questions = routing_questions(&menu);
        assert_eq!(questions.len(), 1);
        assert_eq!(questions[0].0, "difficulty");
        assert_eq!(routing_questions(&tags(&["coding"])).len(), 2);
    }

    #[test]
    fn failures_and_out_of_set_answers_are_no_answer() {
        // A failed difficulty call and a topic whose answer mass is entirely
        // off the labels (a rambling brain): neither is read as anything, so
        // the heuristics settle both.
        let outcomes = vec![
            failed_outcome("difficulty"),
            ok_outcome("topic", &[("The", -0.1), ("Request", -2.0)]),
        ];
        assert_eq!(
            classified_from_outcomes(&outcomes, &tags(&["coding", "math"])),
            Classified::default()
        );
        // One answered, one not: the answered one stands.
        let outcomes = vec![
            failed_outcome("topic"),
            ok_outcome("difficulty", &[(" C", -0.1), (" B", -3.0)]),
        ];
        assert_eq!(
            classified_from_outcomes(&outcomes, &tags(&["coding", "math"])),
            Classified {
                difficulty: Some(3),
                tags: None
            }
        );
    }

    #[test]
    fn categories_above_the_threshold_are_chosen_strongest_first_and_capped_at_three() {
        let outcomes = vec![
            ok_outcome("tag:coding", &[(" yes", -0.1), (" no", -2.5)]),
            ok_outcome("tag:math", &[(" no", -0.05), (" yes", -3.0)]),
            ok_outcome("tag:vision", &[(" yes", -0.3), (" no", -1.6)]),
            ok_outcome("tag:tools", &[(" yes", -0.2), (" no", -1.9)]),
            ok_outcome("tag:creative", &[(" yes", -0.25), (" no", -1.8)]),
            failed_outcome("tag:legal"),
            // Exactly 0.5 is not above the threshold.
            ok_outcome("tag:fast", &[(" yes", -1.0), (" no", -1.0)]),
        ];
        assert_eq!(
            categories_from_outcomes(&outcomes),
            vec!["coding", "tools", "creative"]
        );
        let q = category_question("coding");
        assert_eq!(
            vprompt::render_user(&q, &vprompt::labels_for(&q)),
            "# Question\nDoes the routing tag 'coding' describe this request?\n\nAnswer \"yes\" or \"no\"."
        );
    }

    #[test]
    fn switching_the_brain_does_not_reuse_the_previous_brains_cached_answers() {
        // Without the brain in the key, a switch to a new classifier_model
        // served the old brain's answers (and its cached failures) for the
        // rest of the TTL.
        let tags = vec!["coding".to_string(), "math".to_string()];
        let prompt = "What is the capital of France?";
        assert_ne!(
            cache_key("brain-a", prompt, &tags),
            cache_key("brain-b", prompt, &tags)
        );
        assert_eq!(
            cache_key("brain-b", prompt, &tags),
            cache_key("brain-b", prompt, &tags)
        );
    }

    #[test]
    fn the_complaint_answer_is_its_yes_probability_or_none_on_failure() {
        let yes =
            complaint_from_outcomes(&[ok_outcome("complaint", &[(" yes", -0.1), (" no", -3.0)])]);
        assert!(yes.unwrap() > 0.9);
        assert_eq!(
            complaint_from_outcomes(&[failed_outcome("complaint")]),
            None
        );
        assert_eq!(complaint_from_outcomes(&[]), None);
    }

    /// The exact strings the brain is sent, pinned so that nobody changes
    /// them by accident. What they say decides how well a brain reads
    /// requests, and differently for each model: score the brain with
    /// `classifier::eval` before and after any change here. A classifier
    /// distilled for the gateway is trained on these bytes, so changing one
    /// without regenerating its training data silently degrades it: this test
    /// is a deliberate cross-repository tripwire, not a tautology. Update both
    /// together.
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

        let d = difficulty_question();
        let labels = vprompt::labels_for(&d);
        assert_eq!(labels.labels, ["A", "B", "C"]);
        assert_eq!(
            vprompt::render_user(&d, &labels),
            "# Question\nWhich level of model does this request need?\n\n# Levels\n\
             A) level 1 — Light: small talk, a quick fact or a definition, the name of a command, or a tiny edit to a short text given in the request\n\
             B) level 2 — Mid: everyday work such as writing or explaining code, drafting or revising a text, a routine calculation, a summary or step-by-step instructions\n\
             C) level 3 — Strong: hard reasoning such as a proof or a derivation, tracking down an intermittent or unexplained fault, designing a system, weighing trade-offs among several options, or building something with many strict requirements at once\n\
             \nAnswer with the letter of the level that best matches."
        );

        let all: Vec<String> = obleth_config::MODEL_TAGS
            .iter()
            .map(|t| t.to_string())
            .collect();
        let t = topic_question(&all).unwrap();
        let labels = vprompt::labels_for(&t);
        assert_eq!(labels.labels, ["A", "B", "C", "D", "E", "F"]);
        assert_eq!(
            vprompt::render_user(&t, &labels),
            "# Question\nWhat is this request mainly about?\n\n# Options\n\
             A) coding — code, scripts, queries, commands or configuration to write, read, fix or run\n\
             B) creative — fiction, verse, jokes, role-play or other imaginative writing\n\
             C) general — none of the others: conversation, a question of fact, advice, an explanation\n\
             D) math — a calculation, an equation, a proof, statistics or a word problem with quantities\n\
             E) reasoning — a puzzle, a plan, an analysis or a decision to think through that is neither code nor math\n\
             F) writing — prose to draft, edit, shorten, translate or proofread: an email, a letter, an essay, a post\n\
             \nAnswer with the letter of the best option."
        );
    }

    /// The labels are read back by position, so the topics must be listed in
    /// the order the options are lettered: sorted by name.
    #[test]
    fn the_topics_are_listed_in_the_order_their_options_are_lettered() {
        let names: Vec<&str> = TOPICS.iter().map(|(name, _)| *name).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
        for name in names {
            assert!(obleth_config::is_valid_tag(name), "{name} is a routing tag");
        }
    }

    pub(super) fn brain(api_base: &str) -> ResolvedModel {
        ResolvedModel {
            description: String::new(),
            model_name: "brain-test".to_string(),
            aliases: Vec::new(),
            lifecycle: Default::default(),
            reasoning: Default::default(),
            listed: true,
            rejected_request_fields: Vec::new(),
            session_affinity: Default::default(),
            variants: Vec::new(),
            quantization: "unknown".into(),
            upstream_model: "brain-upstream".to_string(),
            api_base: api_base.to_string(),
            api_key: None,
            upstream_headers: Default::default(),
            model_type: "chat".to_string(),
            admission_weight: 1,
            max_in_flight: None,
            capacity_mode: "static".into(),
            capacity_source: "endpoints".into(),
            capacity_namespace: None,
            capacity_service: None,
            per_replica_max_in_flight: None,
            capacity_headroom: 1.0,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 0,
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            cost_per_video: 0.0,
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

    /// A brain provisioned on Slurm has a blank `api_base` and one healthy
    /// endpoint; classification must reach it through the endpoint.
    #[tokio::test]
    async fn classify_reaches_an_endpoint_only_brain() {
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move || {
                let hits = hits_clone.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    axum::Json(serde_json::json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": "x"},
                            "logprobs": {"content": [{
                                "token": " A", "logprob": -0.05,
                                "top_logprobs": [
                                    {"token": " A", "logprob": -0.05},
                                    {"token": " B", "logprob": -3.5}
                                ]
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

        let mut slurm_brain = brain("");
        slurm_brain.endpoints = vec![obleth_config::ResolvedEndpoint {
            id: "replica-1".into(),
            api_base: format!("http://{addr}/v1"),
            api_key: None,
            priority: 0,
            weight: 1,
            enabled: true,
            healthy: true,
            max_in_flight: None,
        }];
        // Far past the 250 ms default: a loaded test run must not time the
        // mock brain out.
        let settings: AutoRouterSettings =
            serde_json::from_value(serde_json::json!({ "classifier_timeout_ms": 10_000 })).unwrap();
        let classified = Classifier::new(settings)
            .classify(
                &reqwest::Client::new(),
                &slurm_brain,
                "Write a Rust function",
                &["coding".to_string()],
            )
            .await;
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "both questions reach the endpoint"
        );
        // "A" is level 1, and the first of the options coding and general.
        assert_eq!(classified.difficulty, Some(1));
        assert_eq!(classified.tags, Some(vec!["coding".to_string()]));
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
                    let top = if question.contains("Which level of model") {
                        serde_json::json!([
                            {"token": " C", "logprob": -0.1},
                            {"token": " A", "logprob": -3.0}
                        ])
                    } else {
                        // The topic: coding (A), general (B), math (C).
                        assert!(question.contains("A) coding"));
                        assert!(question.contains("C) math"));
                        serde_json::json!([
                            {"token": " A", "logprob": -0.05},
                            {"token": " B", "logprob": -3.5}
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

        // Far past the 250 ms default: a loaded test run must not time the
        // mock brain out.
        let settings: AutoRouterSettings =
            serde_json::from_value(serde_json::json!({ "classifier_timeout_ms": 10_000 })).unwrap();
        let classifier = Classifier::new(settings);
        let tags = vec!["coding".to_string(), "math".to_string()];
        let classified = classifier
            .classify(
                &reqwest::Client::new(),
                &brain(&format!("http://{addr}/v1")),
                "Write a Rust function that parses logprobs",
                &tags,
            )
            .await;
        assert_eq!(classified.tags, Some(vec!["coding".to_string()]));
        assert_eq!(classified.difficulty, Some(3), "level C");
        assert_eq!(
            hits.load(Ordering::SeqCst),
            2,
            "one call for difficulty and one for the topic"
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
        assert_eq!(again, classified);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
    }
}
