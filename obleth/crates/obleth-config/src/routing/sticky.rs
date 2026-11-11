//! Conversation stickiness for `auto`.
//!
//! Routing every turn afresh lets one conversation hop between models as its
//! turns change difficulty, topic wording, or simply which model was least
//! busy at that moment, and each hop changes style, conventions and quality.
//! With stickiness on, the model that answered the previous turn (the
//! *incumbent*) keeps the conversation until there is evidence it is not
//! doing a good job, and then the conversation moves up a level, never down.
//!
//! The gateway is stateless per request, so the incumbent is found through the
//! conversation itself: clients send earlier answers back in `messages`, and
//! after serving a turn the gateway stores a [`TurnRecord`] under a
//! fingerprint of the answer it produced ([`turn_key`]). The next request
//! fingerprints its last assistant message the same way to find it.
//!
//! A client that names its conversations (a session id, see
//! `resolve_conversation`) gets a [`SessionRecord`] instead, found by that id.
//! It survives clients that rewrite earlier turns before sending them back
//! (Chat flattens tool turns to text, which no fingerprint matches), and it
//! remembers which model served each topic, so a conversation that moves from
//! code to docs and back to code returns to its coding model.
//!
//! Everything here is pure; the proxy owns the store and the classifier call.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{Candidate, Intent, GENERAL_DOMAIN, GENERAL_TAG};

/// How long a conversation's record outlives its last turn.
pub const STICKY_TTL_SECS: u64 = 2 * 60 * 60;

/// How long a named conversation's [`SessionRecord`] outlives its last turn:
/// people come back to a chat after lunch, or the next morning.
pub const SESSION_TTL_SECS: u64 = 24 * 60 * 60;

/// Consecutive tool turns with errors before the incumbent is replaced. One
/// failing command is ordinary agent work; a run of them is not.
pub const STRIKES_TO_ESCALATE: u8 = 2;

/// The same tool calls this many assistant turns in a row means the model is
/// stuck in a loop.
pub const TOOL_LOOP_REPEATS: usize = 3;

/// Above this p(yes) the newest user message counts as saying the previous
/// answer was wrong.
pub const COMPLAINT_THRESHOLD: f64 = 0.7;

/// Fingerprints read at most this much of each message, so a long answer
/// costs no more to hash than a short one.
const FINGERPRINT_MAX_BYTES: usize = 4096;

/// What the gateway remembers about the turn it just served.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnRecord {
    pub model: String,
    /// The domains `rung` is measured on: the turn's intent tags, or
    /// [`GENERAL_DOMAIN`] when it had none.
    pub domains: Vec<String>,
    /// The conversation's level. It only ever rises.
    pub rung: u8,
    /// Consecutive tool turns that came back with errors.
    #[serde(default)]
    pub strikes: u8,
    /// The answer was broken: no content and no tool call, or a tool call
    /// whose arguments are not JSON.
    #[serde(default)]
    pub bad_output: bool,
}

/// The model that last served one topic of a conversation, and its level there.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Standing {
    pub domain: String,
    pub model: String,
    pub rung: u8,
}

/// What the gateway remembers about a conversation the client names: its last
/// turn, and the model that served each topic it has been through.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SessionRecord {
    pub last: Option<TurnRecord>,
    #[serde(default)]
    pub topics: Vec<Standing>,
}

/// Topics a conversation remembers at most; the oldest is forgotten first.
const MAX_TOPICS: usize = 16;

impl SessionRecord {
    /// The model this conversation used last for one of `domains`, when it has
    /// been there before. General chat (the `general` tag, or an untagged turn)
    /// is not a topic to return to: nearly every turn carries it.
    pub fn standing(&self, domains: &[String]) -> Option<&Standing> {
        self.topics.iter().rev().find(|s| {
            s.domain != GENERAL_DOMAIN && s.domain != GENERAL_TAG && domains.contains(&s.domain)
        })
    }

    /// Take `record` as the conversation's last turn, and its model as the one
    /// for each of its topics.
    pub fn remember(&mut self, record: &TurnRecord) {
        for domain in &record.domains {
            self.topics.retain(|s| s.domain != *domain);
            self.topics.push(Standing {
                domain: domain.clone(),
                model: record.model.clone(),
                rung: record.rung,
            });
        }
        let extra = self.topics.len().saturating_sub(MAX_TOPICS);
        self.topics.drain(..extra);
        self.last = Some(record.clone());
    }
}

/// The store key of a named conversation's [`SessionRecord`]. The id is hashed
/// so a client's own naming never shapes the key.
pub fn session_key(session_id: &str) -> String {
    let mut h = Sha256::new();
    h.update(session_id.as_bytes());
    format!("s:{}", hex(&h.finalize()))
}

/// Evidence that the incumbent is not doing a good job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signal {
    /// The client sent the same request again after it was answered.
    Regenerate,
    /// The user said the previous answer was wrong or did not work.
    Complaint,
    /// The previous answer was broken (see [`TurnRecord::bad_output`]).
    BadOutput,
    /// The model repeated the same tool calls [`TOOL_LOOP_REPEATS`] times.
    ToolLoop,
    /// [`STRIKES_TO_ESCALATE`] tool turns in a row came back with errors.
    ToolErrors,
}

impl Signal {
    pub fn as_str(self) -> &'static str {
        match self {
            Signal::Regenerate => "regenerate",
            Signal::Complaint => "complaint",
            Signal::BadOutput => "bad_output",
            Signal::ToolLoop => "tool_loop",
            Signal::ToolErrors => "tool_errors",
        }
    }
}

/// What the request itself shows, read before deciding.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Observed {
    pub regenerate: bool,
    pub complaint: bool,
    pub tool_loop: bool,
    /// The request carries tool results since the last answer.
    pub tool_turn: bool,
    /// At least one of those tool results reports an error.
    pub tool_errors: bool,
}

/// How a turn with a known incumbent is routed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Serve the incumbent if it can take the request.
    Keep,
    /// Move up a rung, away from the incumbent.
    Escalate(Signal),
    /// The turn is harder than the conversation's rung.
    Harder,
    /// The turn moved to a different topic; route within it, no lower than
    /// the conversation's rung.
    Topic,
}

/// The routing plan for one turn, and what the next record starts from.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub decision: Decision,
    /// Candidates must hold at least this level on `domains`.
    pub floor: u8,
    pub domains: Vec<String>,
    pub strikes: u8,
}

/// Why `auto` served the model it did, for the response header and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// No incumbent: the turn was routed as a new conversation.
    Fresh,
    Kept,
    Escalated(Signal),
    Harder,
    Topic,
    /// The turn went back to a topic the conversation had before, and to the
    /// model that served it then.
    Returned,
    /// The incumbent could not take the request (down, too small a context,
    /// a missing capability, or no longer allowed).
    Forced,
}

impl Reason {
    pub fn as_str(self) -> &'static str {
        match self {
            Reason::Fresh => "fresh",
            Reason::Kept => "kept",
            Reason::Escalated(Signal::Regenerate) => "escalated:regenerate",
            Reason::Escalated(Signal::Complaint) => "escalated:complaint",
            Reason::Escalated(Signal::BadOutput) => "escalated:bad_output",
            Reason::Escalated(Signal::ToolLoop) => "escalated:tool_loop",
            Reason::Escalated(Signal::ToolErrors) => "escalated:tool_errors",
            Reason::Harder => "harder",
            Reason::Topic => "topic",
            Reason::Returned => "returned",
            Reason::Forced => "forced",
        }
    }
}

/// The domains a turn's level is measured on, as the tier filter reads them.
pub fn domains_of(tags: &[String]) -> Vec<String> {
    if tags.is_empty() {
        vec![GENERAL_DOMAIN.to_string()]
    } else {
        tags.to_vec()
    }
}

/// Decide how to route a turn whose incumbent is `record`.
pub fn decide(record: &TurnRecord, intent: &Intent, observed: &Observed) -> Plan {
    let max = crate::MAX_TIER_LEVEL;
    let specific: Vec<&String> = intent.tags.iter().filter(|t| *t != GENERAL_TAG).collect();
    // A round that carries tool results continues the same piece of work,
    // however the classifier reads it: only a new user turn changes topic.
    let topic_changed = !observed.tool_turn
        && !specific.is_empty()
        && !specific
            .iter()
            .any(|t| record.domains.iter().any(|d| d == *t));
    let domains = if topic_changed {
        domains_of(&intent.tags)
    } else {
        record.domains.clone()
    };

    let strikes = if observed.tool_errors {
        record.strikes.saturating_add(1)
    } else {
        0
    };
    let signal = if observed.regenerate {
        Some(Signal::Regenerate)
    } else if observed.complaint {
        Some(Signal::Complaint)
    } else if record.bad_output {
        Some(Signal::BadOutput)
    } else if observed.tool_loop {
        Some(Signal::ToolLoop)
    } else if strikes >= STRIKES_TO_ESCALATE {
        Some(Signal::ToolErrors)
    } else {
        None
    };

    match signal {
        Some(signal) => Plan {
            decision: Decision::Escalate(signal),
            floor: record.rung.saturating_add(1).min(max),
            domains,
            strikes: 0,
        },
        None if topic_changed => Plan {
            decision: Decision::Topic,
            floor: record.rung.max(intent.difficulty).min(max),
            domains,
            strikes,
        },
        None if intent.difficulty > record.rung => Plan {
            decision: Decision::Harder,
            floor: intent.difficulty.min(max),
            domains,
            strikes,
        },
        None => Plan {
            decision: Decision::Keep,
            floor: record.rung,
            domains,
            strikes,
        },
    }
}

/// The candidates at or above `floor` on `domains`, without `exclude`.
pub fn floor_pool(
    candidates: &[Candidate],
    domains: &[String],
    floor: u8,
    exclude: Option<&str>,
) -> Vec<Candidate> {
    candidates
        .iter()
        .filter(|c| Some(c.model.model_name.as_str()) != exclude)
        .filter(|c| c.strength(domains) >= floor)
        .cloned()
        .collect()
}

/// Read what the request shows about the incumbent's last answer. `seen`
/// tells whether this exact request was already answered (see [`retry_key`]).
pub fn observe(json: &serde_json::Value, seen: bool) -> Observed {
    let messages = messages(json);
    let tail = match last_assistant(messages) {
        Some(i) => &messages[i + 1..],
        None => messages,
    };
    let results: Vec<String> = tail
        .iter()
        .filter(|m| role(m) == "tool")
        .map(|m| message_text(m.get("content")))
        .collect();
    Observed {
        regenerate: seen,
        complaint: false,
        tool_loop: tool_loop(messages),
        tool_turn: !results.is_empty(),
        tool_errors: results.iter().any(|r| tool_error(r)),
    }
}

/// True when the newest message after the last answer is the user's, so a
/// complaint about that answer is possible.
pub fn user_spoke_last(json: &serde_json::Value) -> bool {
    messages(json)
        .iter()
        .rev()
        .find(|m| matches!(role(m), "user" | "assistant" | "tool"))
        .is_some_and(|m| role(m) == "user")
}

/// The newest user message's text, as the complaint question reads it.
pub fn newest_user_text(json: &serde_json::Value) -> String {
    messages(json)
        .iter()
        .rev()
        .find(|m| role(m) == "user")
        .map(|m| message_text(m.get("content")))
        .unwrap_or_default()
}

/// The store key of the record for the request's last assistant message, or
/// `None` on a conversation's first turn.
pub fn incumbent_key(json: &serde_json::Value) -> Option<String> {
    let messages = messages(json);
    let i = last_assistant(messages)?;
    Some(turn_key(&user_before(messages, i), &messages[i]))
}

/// The key the answer to this request will be stored under. It pairs the
/// user message it answers with the answer, so two conversations only share
/// a record when they asked the same thing and got the same reply.
pub fn answer_key(json: &serde_json::Value, answer: &serde_json::Value) -> String {
    let messages = messages(json);
    turn_key(&user_before(messages, messages.len()), answer)
}

/// Fingerprint of one assistant turn: the user message before it and the
/// answer's text, or its tool-call ids when it called tools. Reasoning is
/// left out because clients often drop it before sending the turn back.
pub fn turn_key(user: &str, assistant: &serde_json::Value) -> String {
    let mut h = Sha256::new();
    h.update(normalize(user).as_bytes());
    h.update([0]);
    h.update(normalize(&message_text(assistant.get("content"))).as_bytes());
    for call in tool_calls(assistant) {
        h.update([0]);
        match call.get("id").and_then(|v| v.as_str()) {
            Some(id) if !id.is_empty() => h.update(id.as_bytes()),
            _ => h.update(call_signature(call).as_bytes()),
        }
    }
    hex(&h.finalize())
}

/// Fingerprint of the request after its last assistant turn. The same key
/// twice means the client sent the same request again after it was answered.
pub fn retry_key(json: &serde_json::Value) -> Option<String> {
    let messages = messages(json);
    let i = last_assistant(messages)?;
    let mut h = Sha256::new();
    h.update(turn_key(&user_before(messages, i), &messages[i]).as_bytes());
    for m in &messages[i + 1..] {
        h.update([0]);
        h.update(role(m).as_bytes());
        h.update([0]);
        h.update(normalize(&message_text(m.get("content"))).as_bytes());
        if let Some(id) = m.get("tool_call_id").and_then(|v| v.as_str()) {
            h.update(id.as_bytes());
        }
    }
    Some(hex(&h.finalize()))
}

/// True when a completion is broken: no answer and no tool call (an empty
/// reply, or a thinker cut off before it answered), or a tool call whose
/// arguments are not JSON.
pub fn bad_output(completion: &serde_json::Value) -> bool {
    let Some(message) = completion.pointer("/choices/0/message") else {
        return false;
    };
    let calls = tool_calls(message);
    if calls.is_empty() {
        return normalize(&message_text(message.get("content"))).is_empty();
    }
    calls.iter().any(|c| {
        let args = c
            .pointer("/function/arguments")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        !args.trim().is_empty() && serde_json::from_str::<serde_json::Value>(args).is_err()
    })
}

/// True when a tool result reads as a failure: a traceback, a panic, a
/// non-zero exit, or a failing test run.
pub fn tool_error(result: &str) -> bool {
    const MARKERS: &[&str] = &[
        "Traceback (most recent call last)",
        "panicked at",
        "command not found",
        "npm ERR!",
        "error[E",
        "SyntaxError:",
        "TypeError:",
        "NameError:",
        "ModuleNotFoundError:",
        "ImportError:",
        "Segmentation fault",
        "FAILED",
        "Test failed",
        "tests failed",
        "BUILD FAILURE",
    ];
    if MARKERS.iter().any(|m| result.contains(m)) {
        return true;
    }
    let lower = result.to_ascii_lowercase();
    ["exit code", "exit status", "exited with code", "exitcode"]
        .iter()
        .any(|p| nonzero_after(&lower, p))
}

/// True when `pattern` is followed (after `:`, `=` or spaces) by a non-zero
/// number somewhere in `text`.
fn nonzero_after(text: &str, pattern: &str) -> bool {
    text.match_indices(pattern).any(|(at, _)| {
        let rest = text[at + pattern.len()..].trim_start_matches([' ', ':', '=']);
        let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
        !digits.is_empty() && digits.chars().any(|c| c != '0')
    })
}

/// True when the last [`TOOL_LOOP_REPEATS`] assistant turns all made the
/// same tool calls.
fn tool_loop(messages: &[serde_json::Value]) -> bool {
    let recent: Vec<Vec<String>> = messages
        .iter()
        .rev()
        .filter(|m| role(m) == "assistant")
        .take(TOOL_LOOP_REPEATS)
        .map(|m| {
            let mut calls: Vec<String> = tool_calls(m).iter().map(call_signature).collect();
            calls.sort();
            calls
        })
        .collect();
    recent.len() == TOOL_LOOP_REPEATS
        && !recent[0].is_empty()
        && recent.iter().all(|calls| *calls == recent[0])
}

fn call_signature(call: &serde_json::Value) -> String {
    let name = call
        .pointer("/function/name")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let args = call
        .pointer("/function/arguments")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // Key order and spacing are not part of a call's identity.
    let args = serde_json::from_str::<serde_json::Value>(args)
        .map(|v| v.to_string())
        .unwrap_or_else(|_| args.trim().to_string());
    format!("{name}\0{args}")
}

fn messages(json: &serde_json::Value) -> &[serde_json::Value] {
    json.get("messages")
        .and_then(|m| m.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn role(m: &serde_json::Value) -> &str {
    m.get("role").and_then(|r| r.as_str()).unwrap_or("")
}

fn last_assistant(messages: &[serde_json::Value]) -> Option<usize> {
    messages.iter().rposition(|m| role(m) == "assistant")
}

/// Text of the last user message before index `end`.
fn user_before(messages: &[serde_json::Value], end: usize) -> String {
    messages[..end]
        .iter()
        .rev()
        .find(|m| role(m) == "user")
        .map(|m| message_text(m.get("content")))
        .unwrap_or_default()
}

fn tool_calls(message: &serde_json::Value) -> &[serde_json::Value] {
    message
        .get("tool_calls")
        .and_then(|v| v.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn message_text(content: Option<&serde_json::Value>) -> String {
    super::classifier_message_text(content)
}

/// Text as it is compared across round trips: reasoning blocks some clients
/// keep inline are removed, whitespace is collapsed, and only the first
/// [`FINGERPRINT_MAX_BYTES`] count.
fn normalize(text: &str) -> String {
    let mut text = std::borrow::Cow::Borrowed(text);
    for (open, close) in [("<think>", "</think>"), ("<details", "</details>")] {
        if text.contains(open) {
            text = std::borrow::Cow::Owned(without_blocks(&text, open, close));
        }
    }
    // Only the head counts, so only the head is collapsed: the text is a
    // client's, and can be as long as a request may be.
    let mut collapsed = String::new();
    for word in text.split_whitespace() {
        if !collapsed.is_empty() {
            collapsed.push(' ');
        }
        collapsed.push_str(word);
        if collapsed.len() >= FINGERPRINT_MAX_BYTES {
            break;
        }
    }
    super::head_bytes(&collapsed, FINGERPRINT_MAX_BYTES).to_string()
}

/// `text` with each `open … close` block replaced by a space, and everything
/// from an unclosed `open` on dropped. One pass: the time follows the text's
/// length however many blocks it holds.
fn without_blocks(text: &str, open: &str, close: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        out.push_str(&rest[..start]);
        match rest[start..].find(close) {
            Some(len) => {
                out.push(' ');
                rest = &rest[start + len + close.len()..];
            }
            None => return out,
        }
    }
    out.push_str(rest);
    out
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The text is a client's and can be megabytes of blocks: reading it takes
    /// time that follows its length, and only its head is kept.
    #[test]
    fn normalizing_takes_time_that_follows_the_text() {
        assert_eq!(
            normalize("a <think>plan</think>  b\n<details open>x</details>c <think>cut off"),
            "a b c"
        );
        let started = std::time::Instant::now();
        let blocks = "<think></think>".repeat(400_000);
        assert_eq!(normalize(&format!("{blocks}tail")), "tail");
        let words = "w ".repeat(4_000_000);
        assert_eq!(normalize(&words).len(), FINGERPRINT_MAX_BYTES);
        assert!(started.elapsed() < std::time::Duration::from_secs(20));
    }

    use crate::routing::IntentSource;
    use serde_json::json;

    fn record(rung: u8) -> TurnRecord {
        TurnRecord {
            model: "mid-coder".into(),
            domains: vec!["coding".into()],
            rung,
            strikes: 0,
            bad_output: false,
        }
    }

    fn intent(tags: &[&str], difficulty: u8) -> Intent {
        Intent {
            tags: tags.iter().map(|t| t.to_string()).collect(),
            difficulty,
            source: IntentSource::Classifier,
        }
    }

    fn turn(model: &str, domains: &[&str], rung: u8) -> TurnRecord {
        TurnRecord {
            model: model.into(),
            domains: domains.iter().map(|d| d.to_string()).collect(),
            rung,
            strikes: 0,
            bad_output: false,
        }
    }

    #[test]
    fn a_session_remembers_the_model_for_each_topic() {
        let mut session = SessionRecord::default();
        session.remember(&turn("north-mini", &[GENERAL_DOMAIN], 1));
        session.remember(&turn("glm", &["coding"], 3));
        session.remember(&turn("hemmingway", &["writing"], 3));
        assert_eq!(session.last.as_ref().unwrap().model, "hemmingway");
        let coding = session.standing(&["coding".into()]).unwrap();
        assert_eq!((coding.model.as_str(), coding.rung), ("glm", 3));
        // Chit-chat is not a topic to return to, tagged or not.
        assert!(session.standing(&[GENERAL_DOMAIN.into()]).is_none());
        session.remember(&turn("north-mini", &[GENERAL_TAG], 1));
        assert!(session
            .standing(&[GENERAL_TAG.into(), "creative".into()])
            .is_none());
        assert_eq!(
            session
                .standing(&["coding".into(), GENERAL_TAG.into()])
                .unwrap()
                .model,
            "glm"
        );
        // A later coding turn replaces the coding model, not the others.
        session.remember(&turn("kimi", &["coding"], 4));
        assert_eq!(session.standing(&["coding".into()]).unwrap().model, "kimi");
        assert_eq!(
            session.standing(&["writing".into()]).unwrap().model,
            "hemmingway"
        );
        assert_eq!(session.topics.len(), 4);
    }

    #[test]
    fn a_session_forgets_its_oldest_topics_first() {
        let mut session = SessionRecord::default();
        for i in 0..MAX_TOPICS + 2 {
            session.remember(&turn("m", &[&format!("t{i}")], 1));
        }
        assert_eq!(session.topics.len(), MAX_TOPICS);
        assert!(session.standing(&["t0".into()]).is_none());
        assert!(session
            .standing(&[format!("t{}", MAX_TOPICS + 1)])
            .is_some());
    }

    #[test]
    fn session_keys_hash_the_id() {
        let key = session_key("20e5a101-f62b-4c98-8f2e-d6c3aa1137c2");
        assert!(key.starts_with("s:"));
        assert_eq!(key.len(), 2 + 64);
        assert_ne!(key, session_key("another chat"));
        assert_eq!(key, session_key("20e5a101-f62b-4c98-8f2e-d6c3aa1137c2"));
    }

    #[test]
    fn a_tool_round_never_changes_topic() {
        let tool_round = Observed {
            tool_turn: true,
            ..Default::default()
        };
        let plan = decide(&record(2), &intent(&["writing"], 1), &tool_round);
        assert_eq!(plan.decision, Decision::Keep);
        assert_eq!(plan.domains, vec!["coding".to_string()]);
        // A new user turn can.
        let plan = decide(&record(2), &intent(&["writing"], 1), &Observed::default());
        assert_eq!(plan.decision, Decision::Topic);
    }

    #[test]
    fn a_quiet_follow_up_keeps_the_incumbent_at_its_rung() {
        let plan = decide(&record(2), &intent(&["coding"], 1), &Observed::default());
        assert_eq!(plan.decision, Decision::Keep);
        assert_eq!(plan.floor, 2);
        // A general-only turn is not a topic change.
        let plan = decide(&record(2), &intent(&["general"], 1), &Observed::default());
        assert_eq!(plan.decision, Decision::Keep);
        assert_eq!(plan.domains, vec!["coding".to_string()]);
    }

    #[test]
    fn hard_signals_move_up_one_rung_and_stop_at_the_top() {
        for (observed, signal) in [
            (
                Observed {
                    regenerate: true,
                    ..Default::default()
                },
                Signal::Regenerate,
            ),
            (
                Observed {
                    complaint: true,
                    ..Default::default()
                },
                Signal::Complaint,
            ),
            (
                Observed {
                    tool_loop: true,
                    ..Default::default()
                },
                Signal::ToolLoop,
            ),
        ] {
            let plan = decide(&record(1), &intent(&["coding"], 1), &observed);
            assert_eq!(plan.decision, Decision::Escalate(signal));
            assert_eq!(plan.floor, 2);
        }
        let top = crate::MAX_TIER_LEVEL;
        let observed = Observed {
            complaint: true,
            ..Default::default()
        };
        assert_eq!(
            decide(&record(top), &intent(&["coding"], 1), &observed).floor,
            top
        );

        let broken = TurnRecord {
            bad_output: true,
            ..record(1)
        };
        let plan = decide(&broken, &intent(&["coding"], 1), &Observed::default());
        assert_eq!(plan.decision, Decision::Escalate(Signal::BadOutput));
    }

    #[test]
    fn tool_errors_escalate_on_the_second_turn_in_a_row_and_a_clean_turn_resets() {
        let failing = Observed {
            tool_turn: true,
            tool_errors: true,
            ..Default::default()
        };
        let first = decide(&record(1), &intent(&["coding"], 1), &failing);
        assert_eq!(first.decision, Decision::Keep);
        assert_eq!(first.strikes, 1);

        let struck = TurnRecord {
            strikes: first.strikes,
            ..record(1)
        };
        let second = decide(&struck, &intent(&["coding"], 1), &failing);
        assert_eq!(second.decision, Decision::Escalate(Signal::ToolErrors));
        assert_eq!(second.strikes, 0);

        let clean = Observed {
            tool_turn: true,
            ..Default::default()
        };
        assert_eq!(decide(&struck, &intent(&["coding"], 1), &clean).strikes, 0);
    }

    #[test]
    fn a_new_topic_reroutes_without_dropping_the_rung() {
        let plan = decide(&record(3), &intent(&["writing"], 1), &Observed::default());
        assert_eq!(plan.decision, Decision::Topic);
        assert_eq!(plan.floor, 3);
        assert_eq!(plan.domains, vec!["writing".to_string()]);

        let opener = TurnRecord {
            domains: vec![GENERAL_DOMAIN.into()],
            ..record(1)
        };
        let plan = decide(&opener, &intent(&["coding"], 2), &Observed::default());
        assert_eq!(plan.decision, Decision::Topic);
        assert_eq!(plan.floor, 2);
    }

    #[test]
    fn a_harder_turn_raises_the_floor_and_an_easier_one_never_lowers_it() {
        let plan = decide(&record(1), &intent(&["coding"], 3), &Observed::default());
        assert_eq!(plan.decision, Decision::Harder);
        assert_eq!(plan.floor, 3);
        let plan = decide(&record(3), &intent(&["coding"], 1), &Observed::default());
        assert_eq!(plan.floor, 3);
    }

    #[test]
    fn the_answer_key_matches_the_turn_when_the_client_sends_it_back() {
        let request = json!({"messages": [
            {"role": "system", "content": "be brief"},
            {"role": "user", "content": "write a sort"},
        ]});
        let answer = json!({"role": "assistant", "content": "<think>hmm</think>def sort(x):\n    return sorted(x)"});
        let stored = answer_key(&request, &answer);

        // The client drops the reasoning and reflows whitespace.
        let next = json!({"messages": [
            {"role": "system", "content": "be brief"},
            {"role": "user", "content": "write a sort"},
            {"role": "assistant", "content": "def sort(x):\n  return sorted(x)\n"},
            {"role": "user", "content": "now in rust"},
        ]});
        assert_eq!(incumbent_key(&next), Some(stored.clone()));

        // The same reply to a different question is a different conversation.
        let other = json!({"messages": [
            {"role": "user", "content": "write a different sort"},
            {"role": "assistant", "content": "def sort(x):\n    return sorted(x)"},
            {"role": "user", "content": "now in rust"},
        ]});
        assert_ne!(incumbent_key(&other), Some(stored));
        assert_eq!(incumbent_key(&request), None);
    }

    #[test]
    fn tool_call_turns_are_keyed_by_their_call_ids() {
        let request = json!({"messages": [{"role": "user", "content": "fix the test"}]});
        let answer = json!({"role": "assistant", "content": null, "tool_calls": [
            {"id": "call_1", "type": "function", "function": {"name": "bash", "arguments": "{\"cmd\":\"pytest\"}"}}
        ]});
        let next = json!({"messages": [
            {"role": "user", "content": "fix the test"},
            {"role": "assistant", "content": "", "tool_calls": [
                {"id": "call_1", "type": "function", "function": {"name": "bash", "arguments": "{\"cmd\": \"pytest\"}"}}
            ]},
            {"role": "tool", "tool_call_id": "call_1", "content": "1 passed"},
        ]});
        assert_eq!(incumbent_key(&next), Some(answer_key(&request, &answer)));
    }

    #[test]
    fn retry_keys_match_only_the_same_request_after_the_same_answer() {
        let a = json!({"messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": "hello"},
            {"role": "user", "content": "write a poem"},
        ]});
        let mut b = a.clone();
        b["messages"][2]["content"] = json!("write a haiku");
        assert_eq!(retry_key(&a), retry_key(&a.clone()));
        assert_ne!(retry_key(&a), retry_key(&b));
        assert_eq!(
            retry_key(&json!({"messages": [{"role": "user", "content": "hi"}]})),
            None
        );
    }

    #[test]
    fn observe_reads_tool_results_after_the_last_answer() {
        let call = |id: &str| json!({"id": id, "type": "function", "function": {"name": "bash", "arguments": "{\"cmd\":\"cargo test\"}"}});
        let json = json!({"messages": [
            {"role": "user", "content": "make the tests pass"},
            {"role": "assistant", "tool_calls": [call("a")]},
            {"role": "tool", "tool_call_id": "a", "content": "error[E0308]: mismatched types"},
            {"role": "assistant", "tool_calls": [call("b")]},
            {"role": "tool", "tool_call_id": "b", "content": "test result: ok. 3 passed"},
        ]});
        let seen = observe(&json, false);
        assert!(seen.tool_turn);
        assert!(
            !seen.tool_errors,
            "only results after the last answer count"
        );
        assert!(!seen.tool_loop);
        assert!(!user_spoke_last(&json));

        let mut looping = json.clone();
        let msgs = looping["messages"].as_array_mut().unwrap();
        msgs.push(json!({"role": "assistant", "tool_calls": [call("c")]}));
        msgs.push(json!({"role": "tool", "tool_call_id": "c", "content": "exit code: 101"}));
        let seen = observe(&looping, false);
        assert!(seen.tool_loop, "three identical calls in a row");
        assert!(seen.tool_errors);
    }

    #[test]
    fn tool_errors_are_failures_not_mentions() {
        assert!(tool_error(
            "Traceback (most recent call last):\n  File \"x.py\""
        ));
        assert!(tool_error("Process exited with code 2"));
        assert!(tool_error("Exit code: 1"));
        assert!(!tool_error("Exit code: 0"));
        assert!(!tool_error("src/lib.rs:12: fn handle_error(e: Error)"));
        assert!(!tool_error("3 passed in 0.4s"));
    }

    #[test]
    fn broken_answers_are_flagged() {
        let ok = json!({"choices": [{"message": {"content": "done"}, "finish_reason": "stop"}]});
        let empty = json!({"choices": [{"message": {"content": "<think>long</think>"}, "finish_reason": "length"}]});
        let bad_args = json!({"choices": [{"message": {"content": "", "tool_calls": [
            {"id": "x", "function": {"name": "bash", "arguments": "{\"cmd\": \"ls"}}
        ]}}]});
        let good_args = json!({"choices": [{"message": {"content": "", "tool_calls": [
            {"id": "x", "function": {"name": "bash", "arguments": "{\"cmd\": \"ls\"}"}}
        ]}}]});
        assert!(!bad_output(&ok));
        assert!(bad_output(&empty));
        assert!(bad_output(&bad_args));
        assert!(!bad_output(&good_args));
    }

    #[test]
    fn the_floor_pool_drops_weaker_models_and_the_excluded_one() {
        let mut low = crate::routing::tests::healthy(crate::routing::tests::model("low"));
        low.levels = vec![("coding".into(), 1)];
        let mut mid = crate::routing::tests::healthy(crate::routing::tests::model("mid"));
        mid.levels = vec![("coding".into(), 2)];
        let mut top = crate::routing::tests::healthy(crate::routing::tests::model("top"));
        top.levels = vec![("coding".into(), 3)];
        let all = vec![low, mid, top];
        let names = |pool: Vec<Candidate>| -> Vec<String> {
            pool.into_iter().map(|c| c.model.model_name).collect()
        };
        let coding = vec!["coding".to_string()];
        assert_eq!(names(floor_pool(&all, &coding, 2, None)), ["mid", "top"]);
        assert_eq!(names(floor_pool(&all, &coding, 2, Some("mid"))), ["top"]);
    }
}
