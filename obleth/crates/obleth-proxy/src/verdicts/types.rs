//! Wire types for `POST /v1/verdicts`.
//!
//! A `state` (string | object | array) is evaluated against a map of typed
//! questions — `boolean`, `choice`, `score` — and each question comes back as
//! a typed verdict with a probability distribution and a confidence. Unknown
//! fields are ignored rather than rejected so the request shape can grow
//! without breaking older gateways.

use std::collections::BTreeMap;
use std::io;

use serde::{Deserialize, Serialize};

/// Upper bound on questions per request. Each question is one single-token
/// upstream call; the cap bounds the fan-out a single request can demand.
pub(crate) const MAX_QUESTIONS: usize = 32;
/// Choice options are answered with single-letter labels (`A`..`Z`), which are
/// single tokens in every mainstream vocabulary. Two-letter labels are not,
/// so until constrained decoding lands the cap is the alphabet.
pub(crate) const MAX_CHOICE_OPTIONS: usize = 26;
pub(crate) const MIN_CHOICE_OPTIONS: usize = 2;
/// Score levels are labeled `A`..`J`. Ten descriptive levels is already past
/// the point where adjacent levels stop being distinguishable.
pub(crate) const MIN_SCORE_LEVELS: usize = 2;
pub(crate) const MAX_SCORE_LEVELS: usize = 10;
/// Cap on the rendered state text. The request body limit (64 MiB) still
/// bounds the raw body; this bounds what is replicated into every question's
/// prompt.
pub(crate) const MAX_STATE_BYTES: usize = 2 * 1024 * 1024;
pub(crate) const MAX_QUESTION_ID_LEN: usize = 64;
pub(crate) const MAX_OPTION_NAME_LEN: usize = 200;

/// The top-level request: one state, one model, one or more typed questions.
///
/// `questions` is a `BTreeMap` so iteration order — and therefore choice label
/// assignment and span order — is deterministic. serde_json is built without
/// `preserve_order`, so client key order is not observable anyway.
#[derive(Debug, Deserialize)]
pub(crate) struct VerdictsRequest {
    /// The content to evaluate: a plain string, or structured JSON.
    pub state: serde_json::Value,
    pub model: String,
    pub questions: BTreeMap<String, Question>,
}

/// One typed question. All variants share free-form `instructions` (string |
/// object | array, rendered like `state`); each adds its own `criteria`.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub(crate) enum Question {
    /// Yes/no. Returns the probability the answer is yes.
    Boolean {
        instructions: serde_json::Value,
        #[serde(default)]
        criteria: Option<BooleanCriteria>,
    },
    /// Pick one option from a named set. `criteria` maps option name to an
    /// optional rubric description.
    Choice {
        instructions: serde_json::Value,
        criteria: BTreeMap<String, serde_json::Value>,
    },
    /// Rate against ordered level descriptions (index 0 = level 1).
    Score {
        instructions: serde_json::Value,
        criteria: Vec<serde_json::Value>,
    },
}

/// Optional descriptions of what a yes and a no mean.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct BooleanCriteria {
    #[serde(rename = "true", default)]
    pub yes: Option<serde_json::Value>,
    #[serde(rename = "false", default)]
    pub no: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub(crate) struct VerdictsResponse {
    /// The gateway's canonical model name (aliases and `auto` resolved).
    pub model: String,
    pub verdicts: BTreeMap<String, Verdict>,
    pub usage: Usage,
}

#[derive(Debug, Serialize)]
pub(crate) struct Verdict {
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// boolean: bool; choice: the chosen option name; score: 1-based level int.
    pub value: serde_json::Value,
    /// boolean: `{"true": p, "false": p}`; choice: `{option: p}`; score: a
    /// level-ordered array (object keys would sort `"10"` before `"2"`).
    pub probabilities: serde_json::Value,
    /// How decisively the model answered, separate from the probabilities:
    /// `(1 - H/ln K) * in_set_mass`, where H is the entropy of the
    /// renormalized distribution and `in_set_mass` is how much of the raw
    /// next-token mass landed on recognized answer labels at all.
    pub confidence: f64,
    /// Score only: the probability-weighted level, `Σ pᵢ · levelᵢ`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_value: Option<f64>,
}

#[derive(Debug, Default, Serialize)]
pub(crate) struct Usage {
    pub prompt_tokens: u32,
    pub completion_tokens: u32,
    pub total_tokens: u32,
}

/// Decimal places kept on every float in the response body.
pub(crate) const FLOAT_DECIMALS: usize = 6;

/// Serialize the response with every float written as a fixed-point decimal.
///
/// serde_json's default float output is the shortest round-trip form, which
/// switches to scientific notation below 1e-5: a distribution comes back as
/// `{"billing": 0.9999, "account": 1.55e-6}`, two notations in one object.
/// Probabilities, confidences, and expected values are all written as plain
/// decimals rounded to [`FLOAT_DECIMALS`] places instead, so a client (or a
/// person reading the body) never has to parse an exponent. Six places is
/// far more than a first-token distribution can be trusted to.
pub(crate) fn to_json_bytes(response: &VerdictsResponse) -> Result<Vec<u8>, serde_json::Error> {
    let mut ser = serde_json::Serializer::with_formatter(Vec::new(), FixedPointFormatter);
    response.serialize(&mut ser)?;
    Ok(ser.into_inner())
}

/// serde_json's compact formatter with `write_f64` replaced by fixed-point
/// output. Every other method keeps its default (compact) behavior.
struct FixedPointFormatter;

impl serde_json::ser::Formatter for FixedPointFormatter {
    fn write_f64<W: ?Sized + io::Write>(&mut self, writer: &mut W, value: f64) -> io::Result<()> {
        if !value.is_finite() {
            return writer.write_all(b"null");
        }
        writer.write_all(fixed_point(value).as_bytes())
    }
}

/// Render a finite float as a decimal with at most [`FLOAT_DECIMALS`] places,
/// trailing zeros trimmed but at least one digit after the point so the token
/// still reads as a float (`1.0`, not `1`). Values that round to zero come out
/// as `0.0`, never `-0.0`.
fn fixed_point(value: f64) -> String {
    let mut s = format!("{value:.FLOAT_DECIMALS$}");
    if s.contains('.') {
        let trimmed = s.trim_end_matches('0').len();
        s.truncate(trimmed);
        if s.ends_with('.') {
            s.push('0');
        }
    }
    if s == "-0.0" {
        return "0.0".to_string();
    }
    s
}

/// Validate the request against the MVP limits. The returned string becomes
/// the message of an OpenAI-style 400 error body.
pub(crate) fn validate(req: &VerdictsRequest) -> Result<(), String> {
    match &req.state {
        serde_json::Value::String(_)
        | serde_json::Value::Object(_)
        | serde_json::Value::Array(_) => {}
        _ => {
            return Err(
                "`state` must be a string, object, or array (see the verdicts docs)".to_string(),
            )
        }
    }
    if req.model.trim().is_empty() {
        return Err("`model` is required".to_string());
    }
    if req.questions.is_empty() {
        return Err("`questions` must contain at least one question".to_string());
    }
    if req.questions.len() > MAX_QUESTIONS {
        return Err(format!(
            "too many questions: {} (maximum {MAX_QUESTIONS})",
            req.questions.len()
        ));
    }
    for (id, q) in &req.questions {
        if id.trim().is_empty() {
            return Err("question ids must be non-empty".to_string());
        }
        if id.len() > MAX_QUESTION_ID_LEN {
            return Err(format!(
                "question id '{}…' is too long (maximum {MAX_QUESTION_ID_LEN} bytes)",
                &id[..id
                    .char_indices()
                    .nth(20)
                    .map(|(i, _)| i)
                    .unwrap_or(id.len())]
            ));
        }
        match q {
            Question::Boolean { .. } => {}
            Question::Choice { criteria, .. } => {
                if criteria.len() < MIN_CHOICE_OPTIONS {
                    return Err(format!(
                        "question '{id}': a choice needs at least {MIN_CHOICE_OPTIONS} options"
                    ));
                }
                if criteria.len() > MAX_CHOICE_OPTIONS {
                    return Err(format!(
                        "question '{id}': too many options: {} (maximum {MAX_CHOICE_OPTIONS} \
                         until constrained decoding is supported)",
                        criteria.len()
                    ));
                }
                for option in criteria.keys() {
                    if option.trim().is_empty() {
                        return Err(format!("question '{id}': option names must be non-empty"));
                    }
                    if option.len() > MAX_OPTION_NAME_LEN {
                        return Err(format!(
                            "question '{id}': option name too long (maximum {MAX_OPTION_NAME_LEN} bytes)"
                        ));
                    }
                }
            }
            Question::Score { criteria, .. } => {
                if criteria.len() < MIN_SCORE_LEVELS || criteria.len() > MAX_SCORE_LEVELS {
                    return Err(format!(
                        "question '{id}': a score needs {MIN_SCORE_LEVELS}–{MAX_SCORE_LEVELS} \
                         levels, got {}",
                        criteria.len()
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: serde_json::Value) -> VerdictsRequest {
        serde_json::from_value(body).expect("request should deserialize")
    }

    fn valid_request() -> serde_json::Value {
        serde_json::json!({
            "state": "Help! My payouts have been failing for 3 days.",
            "model": "glm-5-3",
            "questions": {
                "is_urgent": {"type": "boolean", "instructions": "Does this convey urgency?"},
                "department": {
                    "type": "choice",
                    "instructions": "Which team should handle this?",
                    "criteria": {"billing": "Payments", "technical": "Bugs", "sales": "Pricing"}
                },
                "frustration": {
                    "type": "score",
                    "instructions": "How frustrated is the customer?",
                    "criteria": ["Calm", "Frustrated", "Very angry"]
                }
            }
        })
    }

    #[test]
    fn a_mixed_request_parses_and_validates() {
        let req = parse(valid_request());
        assert!(validate(&req).is_ok());
        assert_eq!(req.questions.len(), 3);
    }

    #[test]
    fn unknown_fields_are_ignored_for_forward_compatibility() {
        let mut body = valid_request();
        body["some_future_field"] = serde_json::json!({"nested": true});
        body["questions"]["is_urgent"]["another"] = serde_json::json!(1);
        let req = parse(body);
        assert!(validate(&req).is_ok());
    }

    #[test]
    fn state_must_be_string_object_or_array() {
        for bad in [
            serde_json::json!(42),
            serde_json::json!(true),
            serde_json::Value::Null,
        ] {
            let mut body = valid_request();
            body["state"] = bad;
            let req = parse(body);
            assert!(validate(&req).unwrap_err().contains("state"));
        }
        for good in [serde_json::json!({"k": "v"}), serde_json::json!(["a", "b"])] {
            let mut body = valid_request();
            body["state"] = good;
            assert!(validate(&parse(body)).is_ok());
        }
    }

    #[test]
    fn question_count_and_id_limits_are_enforced() {
        let mut body = valid_request();
        body["questions"] = serde_json::json!({});
        assert!(validate(&parse(body)).is_err());

        let mut questions = serde_json::Map::new();
        for i in 0..(MAX_QUESTIONS + 1) {
            questions.insert(
                format!("q{i}"),
                serde_json::json!({"type": "boolean", "instructions": "?"}),
            );
        }
        let mut body = valid_request();
        body["questions"] = serde_json::Value::Object(questions);
        assert!(validate(&parse(body))
            .unwrap_err()
            .contains("too many questions"));

        let mut body = valid_request();
        body["questions"] = serde_json::json!({
            "x".repeat(MAX_QUESTION_ID_LEN + 1): {"type": "boolean", "instructions": "?"}
        });
        assert!(validate(&parse(body)).unwrap_err().contains("too long"));
    }

    #[test]
    fn choice_option_bounds_are_enforced() {
        let mut body = valid_request();
        body["questions"] = serde_json::json!({
            "one": {"type": "choice", "instructions": "?", "criteria": {"only": "one"}}
        });
        assert!(validate(&parse(body)).unwrap_err().contains("at least"));

        let mut options = serde_json::Map::new();
        for i in 0..(MAX_CHOICE_OPTIONS + 1) {
            options.insert(format!("opt{i:02}"), serde_json::json!("desc"));
        }
        let mut body = valid_request();
        body["questions"] = serde_json::json!({
            "many": {"type": "choice", "instructions": "?", "criteria": options}
        });
        assert!(validate(&parse(body))
            .unwrap_err()
            .contains("too many options"));
    }

    #[test]
    fn score_level_bounds_are_enforced() {
        for levels in [1usize, MAX_SCORE_LEVELS + 1] {
            let mut body = valid_request();
            body["questions"] = serde_json::json!({
                "s": {
                    "type": "score",
                    "instructions": "?",
                    "criteria": (0..levels).map(|i| format!("level {i}")).collect::<Vec<_>>()
                }
            });
            assert!(validate(&parse(body)).unwrap_err().contains("levels"));
        }
    }

    #[test]
    fn boolean_criteria_accept_the_true_false_spelling() {
        let body = serde_json::json!({
            "state": "s",
            "model": "m",
            "questions": {
                "q": {
                    "type": "boolean",
                    "instructions": "urgent?",
                    "criteria": {"true": "time-sensitive", "false": "can wait"}
                }
            }
        });
        let req = parse(body);
        assert!(validate(&req).is_ok());
        let Question::Boolean { criteria, .. } = &req.questions["q"] else {
            panic!("expected a boolean question");
        };
        let criteria = criteria.as_ref().expect("criteria should parse");
        assert_eq!(
            criteria.yes.as_ref().and_then(|v| v.as_str()),
            Some("time-sensitive")
        );
        assert_eq!(
            criteria.no.as_ref().and_then(|v| v.as_str()),
            Some("can wait")
        );
    }

    #[test]
    fn score_probabilities_serialize_as_an_ordered_array() {
        let answer = Verdict {
            kind: "score",
            value: serde_json::json!(2),
            probabilities: serde_json::json!([0.1, 0.7, 0.2]),
            confidence: 0.61,
            expected_value: Some(2.1),
        };
        let v = serde_json::to_value(&answer).unwrap();
        assert!(v["probabilities"].is_array());
        assert_eq!(v["type"], "score");
        assert!((v["expected_value"].as_f64().unwrap() - 2.1).abs() < 1e-9);
    }

    #[test]
    fn floats_are_written_as_fixed_point_decimals() {
        assert_eq!(fixed_point(0.9999305114863566), "0.999931");
        assert_eq!(fixed_point(1.5534903124299995e-6), "0.000002");
        assert_eq!(fixed_point(2.26e-9), "0.0");
        assert_eq!(fixed_point(-0.0), "0.0");
        assert_eq!(fixed_point(1.0), "1.0");
        assert_eq!(fixed_point(0.0), "0.0");
        assert_eq!(fixed_point(2.9956345671093089), "2.995635");
        assert_eq!(fixed_point(0.5), "0.5");
    }

    #[test]
    fn the_response_body_never_uses_scientific_notation() {
        let mut verdicts = BTreeMap::new();
        verdicts.insert(
            "department".to_string(),
            Verdict {
                kind: "choice",
                value: serde_json::json!("billing"),
                probabilities: serde_json::json!({
                    "account": 1.5534903124299995e-6,
                    "billing": 0.9999961861946203,
                    "technical": 2.26031506727819e-6,
                }),
                confidence: 0.9999305114863566,
                expected_value: None,
            },
        );
        verdicts.insert(
            "frustration".to_string(),
            Verdict {
                kind: "score",
                value: serde_json::json!(3),
                probabilities: serde_json::json!([
                    0.0021827164453451808,
                    0.0,
                    0.9978172835546547,
                    0.0
                ]),
                confidence: 0.733957935578729,
                expected_value: Some(2.9956345671093089),
            },
        );
        let response = VerdictsResponse {
            model: "glm-5-3".to_string(),
            verdicts,
            usage: Usage {
                prompt_tokens: 590,
                completion_tokens: 5,
                total_tokens: 595,
            },
        };
        let bytes = to_json_bytes(&response).unwrap();
        let text = std::str::from_utf8(&bytes).unwrap();
        // An exponent is a digit followed by `e` (`1.5e-6`); words carry `e`
        // too, so look for the numeric form specifically.
        let has_exponent = text
            .as_bytes()
            .windows(2)
            .any(|w| w[0].is_ascii_digit() && (w[1] == b'e' || w[1] == b'E'));
        assert!(!has_exponent, "scientific notation in body: {text}");
        assert!(text.contains("\"account\":0.000002"), "{text}");
        assert!(
            text.contains("\"probabilities\":[0.002183,0.0,0.997817,0.0]"),
            "{text}"
        );
        assert!(text.contains("\"expected_value\":2.995635"), "{text}");
        // Still valid JSON with the same shape and integer fields intact.
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["usage"]["prompt_tokens"], 590);
        assert_eq!(v["verdicts"]["frustration"]["value"], 3);
        assert!(
            (v["verdicts"]["department"]["probabilities"]["billing"]
                .as_f64()
                .unwrap()
                - 0.999996)
                .abs()
                < 1e-12
        );
    }
}
