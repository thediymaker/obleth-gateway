//! Prompt construction for verdicts questions.
//!
//! Every question in a request shares a byte-identical system message carrying
//! the state, and differs only in the user message carrying the question. With
//! all questions pinned to one upstream target and identical sampling params,
//! the shared prefix makes each additional question roughly one prefix-cache
//! hit plus a single decoded token on vLLM/SGLang.
//!
//! Answer labels are chosen to be single tokens by construction — `yes`/`no`
//! for boolean, single letters for choice and score — so the first generated
//! token's `top_logprobs` is the entire answer distribution. Digits are
//! deliberately not used for score levels: `"10"` is two tokens on
//! SentencePiece vocabularies and `1` is an ambiguous prefix of it.

use super::types::{BooleanCriteria, Question};

/// Letters used for choice options and score levels, in assignment order.
const LETTERS: [char; 26] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R',
    'S', 'T', 'U', 'V', 'W', 'X', 'Y', 'Z',
];

/// What the answer labels of one question mean.
pub(crate) enum LabelSemantics {
    /// Labels are `["yes", "no"]`.
    Boolean,
    /// Labels are letters; index i maps to this option name.
    Choice(Vec<String>),
    /// Labels are letters; index i maps to level i+1.
    Score,
}

/// The answer labels for one question plus what they map back to.
pub(crate) struct LabelSet {
    pub labels: Vec<String>,
    pub semantics: LabelSemantics,
}

/// Render a state or instructions value into prompt text: strings verbatim,
/// structured values as pretty-printed JSON.
pub(crate) fn render_value(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

/// The shared system message. Byte-identical across every question of one
/// request — this is the prefix the backend's cache amortizes.
pub(crate) fn render_system(state: &serde_json::Value) -> String {
    format!(
        "You answer classification questions about the STATE below. Reply with exactly one \
         answer label and nothing else — no explanation, no punctuation, no preamble.\n\n\
         # State\n{}",
        render_value(state)
    )
}

/// Assign answer labels to a question. Choice options take letters in the
/// map's (sorted) iteration order; the order is part of the documented API.
pub(crate) fn labels_for(q: &Question) -> LabelSet {
    match q {
        Question::Boolean { .. } => LabelSet {
            labels: vec!["yes".to_string(), "no".to_string()],
            semantics: LabelSemantics::Boolean,
        },
        Question::Choice { criteria, .. } => {
            let options: Vec<String> = criteria.keys().cloned().collect();
            LabelSet {
                labels: options
                    .iter()
                    .enumerate()
                    .map(|(i, _)| LETTERS[i].to_string())
                    .collect(),
                semantics: LabelSemantics::Choice(options),
            }
        }
        Question::Score { criteria, .. } => LabelSet {
            labels: (0..criteria.len()).map(|i| LETTERS[i].to_string()).collect(),
            semantics: LabelSemantics::Score,
        },
    }
}

/// The per-question user message: instructions, criteria, and the answer
/// instruction naming the label format.
pub(crate) fn render_user(q: &Question, labels: &LabelSet) -> String {
    match q {
        Question::Boolean { instructions, criteria } => {
            let mut out = format!("# Question\n{}\n", render_value(instructions));
            let (yes, no) = match criteria {
                Some(BooleanCriteria { yes, no }) => (yes.as_ref(), no.as_ref()),
                None => (None, None),
            };
            if yes.is_some() || no.is_some() {
                out.push_str("\n# Answer meanings\n");
                if let Some(y) = yes {
                    out.push_str(&format!("yes — {}\n", render_value(y)));
                }
                if let Some(n) = no {
                    out.push_str(&format!("no — {}\n", render_value(n)));
                }
            }
            out.push_str("\nAnswer \"yes\" or \"no\".");
            out
        }
        Question::Choice { instructions, criteria } => {
            let mut out = format!("# Question\n{}\n\n# Options\n", render_value(instructions));
            for (label, (option, description)) in labels.labels.iter().zip(criteria.iter()) {
                match description {
                    serde_json::Value::Null => out.push_str(&format!("{label}) {option}\n")),
                    d => out.push_str(&format!("{label}) {option} — {}\n", render_value(d))),
                }
            }
            out.push_str("\nAnswer with the letter of the best option.");
            out
        }
        Question::Score { instructions, criteria } => {
            let mut out = format!("# Question\n{}\n\n# Levels\n", render_value(instructions));
            for (i, (label, description)) in labels.labels.iter().zip(criteria.iter()).enumerate() {
                out.push_str(&format!(
                    "{label}) level {} — {}\n",
                    i + 1,
                    render_value(description)
                ));
            }
            out.push_str("\nAnswer with the letter of the level that best matches.");
            out
        }
    }
}

/// The upstream chat-completions body for one question: greedy, one token,
/// full first-token distribution. `stream:false` — the answer is one token,
/// there is nothing to stream.
pub(crate) fn build_body(
    upstream_model: &str,
    system: &str,
    user: &str,
) -> serde_json::Value {
    serde_json::json!({
        "model": upstream_model,
        "messages": [
            {"role": "system", "content": system},
            {"role": "user", "content": user},
        ],
        "max_tokens": 1,
        "temperature": 0,
        "logprobs": true,
        "top_logprobs": 20,
        "stream": false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn choice(options: &[(&str, serde_json::Value)]) -> Question {
        Question::Choice {
            instructions: serde_json::json!("pick one"),
            criteria: options
                .iter()
                .map(|(k, v)| (k.to_string(), v.clone()))
                .collect::<BTreeMap<_, _>>(),
        }
    }

    #[test]
    fn the_system_message_is_byte_identical_across_questions() {
        // The prefix-cache contract: the state renders once, identically,
        // regardless of which question follows.
        let state = serde_json::json!({"ticket": "duplicate charge", "order": "A-104"});
        assert_eq!(render_system(&state), render_system(&state));
        // Strings render verbatim; structured state renders as pretty JSON.
        let text = render_system(&serde_json::json!("plain text state"));
        assert!(text.ends_with("# State\nplain text state"));
        let structured = render_system(&state);
        assert!(structured.contains("\"order\": \"A-104\""));
    }

    #[test]
    fn choice_labels_follow_sorted_option_order() {
        // BTreeMap iteration is sorted; label assignment is therefore
        // deterministic and independent of client key order.
        let q = choice(&[
            ("zebra", serde_json::json!("z")),
            ("alpha", serde_json::json!("a")),
            ("mid", serde_json::json!(null)),
        ]);
        let labels = labels_for(&q);
        let LabelSemantics::Choice(options) = &labels.semantics else {
            panic!("expected choice semantics");
        };
        assert_eq!(options, &["alpha", "mid", "zebra"]);
        assert_eq!(labels.labels, ["A", "B", "C"]);

        let user = render_user(&q, &labels);
        assert!(user.contains("A) alpha — a"));
        // A null description renders the bare option, no dangling dash.
        assert!(user.contains("B) mid\n"));
        assert!(user.contains("C) zebra — z"));
        assert!(user.contains("letter of the best option"));
    }

    #[test]
    fn boolean_renders_criteria_and_the_yes_no_instruction() {
        let q = Question::Boolean {
            instructions: serde_json::json!("Is it urgent?"),
            criteria: Some(BooleanCriteria {
                yes: Some(serde_json::json!("time-sensitive")),
                no: Some(serde_json::json!("can wait")),
            }),
        };
        let labels = labels_for(&q);
        assert_eq!(labels.labels, ["yes", "no"]);
        let user = render_user(&q, &labels);
        assert!(user.contains("yes — time-sensitive"));
        assert!(user.contains("no — can wait"));
        assert!(user.contains("Answer \"yes\" or \"no\"."));
    }

    #[test]
    fn score_levels_use_letters_not_digits() {
        // Digits are multi-token / prefix-ambiguous ("1" vs "10") on
        // SentencePiece vocabs; letters are single tokens everywhere.
        let q = Question::Score {
            instructions: serde_json::json!("frustration?"),
            criteria: (0..10).map(|i| serde_json::json!(format!("level-{i}"))).collect(),
        };
        let labels = labels_for(&q);
        assert_eq!(labels.labels.len(), 10);
        assert_eq!(labels.labels[9], "J");
        let user = render_user(&q, &labels);
        assert!(user.contains("J) level 10 — level-9"));
    }

    #[test]
    fn the_upstream_body_is_greedy_single_token_with_full_logprobs() {
        let body = build_body("qwen", "sys", "user");
        assert_eq!(body["max_tokens"], 1);
        assert_eq!(body["temperature"], 0);
        assert_eq!(body["logprobs"], true);
        assert_eq!(body["top_logprobs"], 20);
        assert_eq!(body["stream"], false);
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn structured_instructions_render_as_pretty_json() {
        let q = Question::Boolean {
            instructions: serde_json::json!({
                "question": "Same person as `candidate`?",
                "candidate": {"name": "John Smith"}
            }),
            criteria: None,
        };
        let user = render_user(&q, &labels_for(&q));
        assert!(user.contains("\"name\": \"John Smith\""));
    }
}
