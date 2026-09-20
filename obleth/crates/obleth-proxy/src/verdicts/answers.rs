//! Turning a single-token completion's `top_logprobs` into a typed answer.
//!
//! The upstream call asks for one greedy token with `top_logprobs: 20`. The
//! entries of that first (only) position are the model's next-token
//! distribution over its whole vocabulary, truncated to the top 20. Mass on
//! recognized answer labels is renormalized into the reported probabilities;
//! mass elsewhere (a stray `The`, a `<think>` opener, an EOS) is not an
//! answer, so it lowers `confidence` instead.
//!
//! Everything here is a pure function over parsed JSON, so the distribution
//! math is tested on canned completions without any HTTP.

use super::prompt::{LabelSemantics, LabelSet};
use super::types::{Question, Verdict};

/// One entry of `choices[0].logprobs.content[0].top_logprobs`.
#[derive(Debug, Clone)]
pub(crate) struct TopLogprob {
    pub token: String,
    pub logprob: f64,
}

/// Extract the first generated token's `top_logprobs` from a chat completion.
///
/// Errors name what was missing: a backend that ignores `logprobs`, or a
/// completion whose first token was an immediate EOS (`content` is empty).
pub(crate) fn parse_top_logprobs(resp: &serde_json::Value) -> Result<Vec<TopLogprob>, String> {
    let logprobs = resp
        .pointer("/choices/0/logprobs")
        .filter(|v| !v.is_null())
        .ok_or_else(|| "backend returned no logprobs (does it support `logprobs`?)".to_string())?;
    let content = logprobs
        .get("content")
        .and_then(|c| c.as_array())
        .ok_or_else(|| "backend logprobs carry no `content` array".to_string())?;
    let first = content
        .first()
        .ok_or_else(|| "backend generated no token (immediate stop)".to_string())?;
    let top = first
        .get("top_logprobs")
        .and_then(|t| t.as_array())
        .ok_or_else(|| "backend returned no `top_logprobs`".to_string())?;
    let entries: Vec<TopLogprob> = top
        .iter()
        .filter_map(|e| {
            Some(TopLogprob {
                token: e.get("token")?.as_str()?.to_string(),
                logprob: e.get("logprob")?.as_f64()?,
            })
        })
        .collect();
    if entries.is_empty() {
        return Err("backend returned an empty `top_logprobs` list".to_string());
    }
    Ok(entries)
}

/// Normalize a candidate token for label matching: strip surrounding
/// whitespace (chat templates make the first sampled token ` A`), and for
/// boolean labels additionally lowercase and strip trailing punctuation
/// (`Yes`, `yes.`, `YES`).
fn normalize(token: &str, boolean: bool) -> String {
    let t = token.trim();
    if boolean {
        t.trim_end_matches(['.', ',', '!', ':'])
            .to_ascii_lowercase()
    } else {
        t.to_string()
    }
}

/// Sum probability mass per label across all token variants that normalize to
/// it. Returns the per-label masses and the total in-set mass.
pub(crate) fn label_masses(
    entries: &[TopLogprob],
    labels: &[String],
    boolean: bool,
) -> (Vec<f64>, f64) {
    let mut masses = vec![0.0f64; labels.len()];
    for entry in entries {
        let token = normalize(&entry.token, boolean);
        if let Some(i) = labels.iter().position(|l| l == &token) {
            masses[i] += entry.logprob.exp();
        }
    }
    let in_set: f64 = masses.iter().sum();
    (masses, in_set)
}

/// The renormalized distribution over labels plus derived quantities.
#[derive(Debug)]
pub(crate) struct Scored {
    /// Renormalized probabilities, summing to 1.0 over the label set.
    pub probs: Vec<f64>,
    /// Index of the most probable label.
    pub argmax: usize,
    /// `(1 - H/ln K) * in_set_mass`, clamped to [0, 1].
    pub confidence: f64,
    /// `Σ probs[i] * (i + 1)` — the probability-weighted 1-based level.
    pub expected: f64,
}

/// Renormalize label masses and derive argmax, confidence, and the expected
/// level. Zero in-set mass means the model's next token was not any answer
/// label at all — that is a failed evaluation, not a low-confidence one.
pub(crate) fn score_masses(masses: &[f64], in_set: f64) -> Result<Scored, String> {
    // NaN-safe: anything that is not strictly positive (zero, negative, NaN)
    // means no usable answer mass.
    if in_set.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater) {
        return Err(
            "model produced no recognizable answer label (is this a reasoning/thinking model?)"
                .to_string(),
        );
    }
    let probs: Vec<f64> = masses.iter().map(|m| m / in_set).collect();
    let argmax = probs
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.total_cmp(b.1))
        .map(|(i, _)| i)
        .unwrap_or(0);
    let k = probs.len().max(2) as f64;
    let entropy: f64 = probs
        .iter()
        .filter(|p| **p > 0.0)
        .map(|p| -p * p.ln())
        .sum();
    let confidence = ((1.0 - entropy / k.ln()) * in_set.min(1.0)).clamp(0.0, 1.0);
    let expected = probs
        .iter()
        .enumerate()
        .map(|(i, p)| p * (i + 1) as f64)
        .sum();
    Ok(Scored {
        probs,
        argmax,
        confidence,
        expected,
    })
}

/// Assemble the wire answer for one question from its scored distribution.
pub(crate) fn build_answer(q: &Question, labels: &LabelSet, scored: &Scored) -> Verdict {
    match (&labels.semantics, q) {
        (LabelSemantics::Boolean, _) => Verdict {
            kind: "boolean",
            value: serde_json::json!(scored.argmax == 0),
            probabilities: serde_json::json!({
                "true": scored.probs[0],
                "false": scored.probs[1],
            }),
            confidence: scored.confidence,
            expected_value: None,
        },
        (LabelSemantics::Choice(options), _) => Verdict {
            kind: "choice",
            value: serde_json::json!(options[scored.argmax]),
            probabilities: serde_json::Value::Object(
                options
                    .iter()
                    .zip(scored.probs.iter())
                    .map(|(o, p)| (o.clone(), serde_json::json!(p)))
                    .collect(),
            ),
            confidence: scored.confidence,
            expected_value: None,
        },
        (LabelSemantics::Score, _) => Verdict {
            kind: "score",
            value: serde_json::json!(scored.argmax + 1),
            probabilities: serde_json::json!(scored.probs),
            confidence: scored.confidence,
            expected_value: Some(scored.expected),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A canned chat completion with the given first-token top_logprobs.
    fn completion(entries: &[(&str, f64)]) -> serde_json::Value {
        serde_json::json!({
            "id": "cmpl-1",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "A"},
                "logprobs": {
                    "content": [{
                        "token": entries.first().map(|(t, _)| *t).unwrap_or(""),
                        "logprob": entries.first().map(|(_, l)| *l).unwrap_or(0.0),
                        "top_logprobs": entries
                            .iter()
                            .map(|(t, l)| serde_json::json!({"token": t, "logprob": l}))
                            .collect::<Vec<_>>()
                    }]
                },
                "finish_reason": "length"
            }],
            "usage": {"prompt_tokens": 100, "completion_tokens": 1, "total_tokens": 101}
        })
    }

    fn labels(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_top_logprobs_from_a_real_shaped_completion() {
        let resp = completion(&[(" A", -0.1), (" B", -2.5), ("The", -4.0)]);
        let entries = parse_top_logprobs(&resp).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].token, " A");
        assert!((entries[0].logprob - (-0.1)).abs() < 1e-12);
    }

    #[test]
    fn missing_logprobs_and_immediate_eos_are_named_errors() {
        let no_logprobs = serde_json::json!({
            "choices": [{"message": {"content": "A"}, "logprobs": null}]
        });
        assert!(parse_top_logprobs(&no_logprobs)
            .unwrap_err()
            .contains("no logprobs"));

        let empty_content = serde_json::json!({
            "choices": [{"logprobs": {"content": []}}]
        });
        assert!(parse_top_logprobs(&empty_content)
            .unwrap_err()
            .contains("no token"));
    }

    #[test]
    fn space_prefixed_variants_sum_into_the_same_label() {
        // Chat templates make the first sampled token ` A`; both spellings
        // are the same answer.
        let entries = vec![
            TopLogprob {
                token: " A".into(),
                logprob: (0.5f64).ln(),
            },
            TopLogprob {
                token: "A".into(),
                logprob: (0.2f64).ln(),
            },
            TopLogprob {
                token: " B".into(),
                logprob: (0.3f64).ln(),
            },
        ];
        let (masses, in_set) = label_masses(&entries, &labels(&["A", "B"]), false);
        assert!((masses[0] - 0.7).abs() < 1e-9);
        assert!((masses[1] - 0.3).abs() < 1e-9);
        assert!((in_set - 1.0).abs() < 1e-9);
    }

    #[test]
    fn boolean_matching_is_case_and_punctuation_insensitive() {
        let entries = vec![
            TopLogprob {
                token: " Yes".into(),
                logprob: (0.6f64).ln(),
            },
            TopLogprob {
                token: "yes.".into(),
                logprob: (0.2f64).ln(),
            },
            TopLogprob {
                token: " NO".into(),
                logprob: (0.1f64).ln(),
            },
        ];
        let (masses, in_set) = label_masses(&entries, &labels(&["yes", "no"]), true);
        assert!((masses[0] - 0.8).abs() < 1e-9);
        assert!((masses[1] - 0.1).abs() < 1e-9);
        assert!((in_set - 0.9).abs() < 1e-9);
    }

    #[test]
    fn out_of_set_mass_lowers_confidence_not_probabilities() {
        // 50% of the raw mass is off-label chatter; the reported distribution
        // still sums to 1, and the loss shows up in confidence alone.
        let entries = vec![
            TopLogprob {
                token: " A".into(),
                logprob: (0.5f64).ln(),
            },
            TopLogprob {
                token: "The".into(),
                logprob: (0.3f64).ln(),
            },
            TopLogprob {
                token: "<think>".into(),
                logprob: (0.2f64).ln(),
            },
        ];
        let (masses, in_set) = label_masses(&entries, &labels(&["A", "B"]), false);
        let scored = score_masses(&masses, in_set).unwrap();
        assert!((scored.probs[0] - 1.0).abs() < 1e-9);
        assert!((scored.probs.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        // Degenerate distribution (H = 0) times in-set mass 0.5.
        assert!((scored.confidence - 0.5).abs() < 1e-9);
    }

    #[test]
    fn a_uniform_distribution_has_zero_confidence() {
        let masses = vec![0.25, 0.25, 0.25, 0.25];
        let scored = score_masses(&masses, 1.0).unwrap();
        assert!(scored.confidence.abs() < 1e-9);
    }

    #[test]
    fn a_decisive_distribution_has_full_confidence() {
        let masses = vec![1.0, 0.0, 0.0];
        let scored = score_masses(&masses, 1.0).unwrap();
        assert!((scored.confidence - 1.0).abs() < 1e-9);
        assert_eq!(scored.argmax, 0);
    }

    #[test]
    fn zero_in_set_mass_is_an_error_not_an_answer() {
        // A thinking model whose first token is `<think>` has not answered.
        let err = score_masses(&[0.0, 0.0], 0.0).unwrap_err();
        assert!(err.contains("no recognizable answer label"));
    }

    #[test]
    fn score_expected_value_is_the_probability_weighted_level() {
        // 10% level 1, 70% level 2, 20% level 3 → 2.1.
        let scored = score_masses(&[0.1, 0.7, 0.2], 1.0).unwrap();
        assert!((scored.expected - 2.1).abs() < 1e-9);
        assert_eq!(scored.argmax, 1);
    }

    #[test]
    fn answers_carry_the_right_shape_per_question_type() {
        use crate::verdicts::prompt::labels_for;

        let boolean = Question::Boolean {
            instructions: serde_json::json!("?"),
            criteria: None,
        };
        let scored = score_masses(&[0.9, 0.1], 1.0).unwrap();
        let a = build_answer(&boolean, &labels_for(&boolean), &scored);
        assert_eq!(a.kind, "boolean");
        assert_eq!(a.value, serde_json::json!(true));
        assert!((a.probabilities["true"].as_f64().unwrap() - 0.9).abs() < 1e-9);

        let choice = Question::Choice {
            instructions: serde_json::json!("?"),
            criteria: [
                ("billing".to_string(), serde_json::json!("b")),
                ("technical".to_string(), serde_json::json!("t")),
            ]
            .into_iter()
            .collect::<BTreeMap<_, _>>(),
        };
        let scored = score_masses(&[0.2, 0.8], 1.0).unwrap();
        let a = build_answer(&choice, &labels_for(&choice), &scored);
        assert_eq!(a.kind, "choice");
        assert_eq!(a.value, serde_json::json!("technical"));
        assert!((a.probabilities["billing"].as_f64().unwrap() - 0.2).abs() < 1e-9);

        let score = Question::Score {
            instructions: serde_json::json!("?"),
            criteria: vec![
                serde_json::json!("calm"),
                serde_json::json!("annoyed"),
                serde_json::json!("angry"),
            ],
        };
        let scored = score_masses(&[0.1, 0.7, 0.2], 1.0).unwrap();
        let a = build_answer(&score, &labels_for(&score), &scored);
        assert_eq!(a.kind, "score");
        assert_eq!(a.value, serde_json::json!(2));
        assert!(a.probabilities.is_array());
        assert!((a.expected_value.unwrap() - 2.1).abs() < 1e-9);
    }

    #[test]
    fn end_to_end_from_canned_completion_to_answer() {
        let resp = completion(&[(" B", -0.223), (" A", -1.61), ("Well", -3.0)]);
        let entries = parse_top_logprobs(&resp).unwrap();
        let (masses, in_set) = label_masses(&entries, &labels(&["A", "B", "C"]), false);
        let scored = score_masses(&masses, in_set).unwrap();
        assert_eq!(scored.argmax, 1);
        assert!(scored.probs[1] > 0.7);
        assert!(scored.confidence > 0.0 && scored.confidence < 1.0);
    }
}
