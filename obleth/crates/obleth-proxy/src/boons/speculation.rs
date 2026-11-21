//! The **speculation** boon: a draft-verify cascade that answers with a fast
//! drafter model whenever the target model itself verifies the draft.
//!
//! Why this beats routing: a router predicts from the PROMPT which model can
//! handle a request and nobody checks the answer; this boon always lets the
//! drafter answer and has the TARGET model audit the actual draft before the
//! client sees a byte. Verification is one prefill over the draft tokens with
//! `prompt_logprobs` — compute-bound and ~50x cheaper than the target decoding
//! the same tokens — so "always try cheap, always verify with the expensive
//! model" is affordable. The gate is (rank-1 agreement, mean logprob) floors,
//! calibrated against judged drafts.
//!
//! Contract with the proxy: [`run`] is called AFTER admission and budget
//! reserve but BEFORE the upstream dispatch. Nothing is sent to the client
//! until a verified span exists, so every pre-release failure — draft error,
//! verify error, gate abort, timeout — returns [`Outcome::Abstain`] and the
//! proxy falls through to its completely normal dispatch path (retries,
//! failover, caching untouched). Once a verified span is released the boon
//! owns the stream: a later gate failure escalates MID-STREAM by having the
//! target model continue the released prefix through its chat template
//! (`continue_final_message`), never by rewriting what the client already saw.
//!
//! Scoring details ported from the proven external prototype:
//! - The draft is scored after an EMPTY `<think></think>` block: conditioning
//!   the verifier on the drafter's reasoning leaks the drafter's confidence
//!   and collapses gate precision to the base rate.
//! - The verifier's first scoring of a not-yet-compiled prefill bucket shape
//!   can return prompt_logprobs entries with corrupt KEYS (an int32→int64
//!   widening signature on HPU backends). With `prompt_logprobs: 0` every key
//!   must equal the token id that was sent, so corruption is exactly
//!   detectable: >10% key misses → retry once.
//! - Drafts are requested with configurable `chat_template_kwargs` (e.g.
//!   `{"reasoning": false}`): no-think drafts measured both faster and more
//!   reliable than reasoning ones.
//! - The continuation stream is relayed with reasoning deltas REMAPPED to
//!   content: the target's chat template renders the continued message after a
//!   closed think block, but streaming reasoning parsers start in
//!   reasoning-until-`</think>` state and mislabel all of it.

use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::Stream;
use obleth_config::{ResolvedKey, ResolvedModel, SpeculationBoonSettings};
use serde_json::{json, Value};

use super::tool_stream::{chunk_usage, content_chunk, done, finish_chunk, usage_chunk};
use crate::proxy::UpstreamUsage;
use crate::state::AppState;
use crate::upstream_failure::{stream_cut_error, DEFAULT_FAILURE_STATUS};

/// Response header naming the drafter model whose verified text reached the
/// client. Set only when the cascade committed (released a verified draft);
/// an abstaining cascade leaves the answer, and the headers, to the target.
pub const DRAFTED_BY_HEADER: &str = "x-obleth-drafted-by";

/// Request parameters the cascade cannot honor on the draft path. Their
/// presence makes the request ineligible (the target model serves it normally).
const BYPASS_KEYS: &[&str] = &[
    "response_format",
    "logit_bias",
    "modalities",
    "audio",
    "logprobs",
    "top_logprobs",
    "prompt_logprobs",
    "echo",
];

/// Keys never forwarded on the ESCALATION dispatch: internals of this boon and
/// replica-killing logprobs parameters.
const ESCALATE_STRIP_KEYS: &[&str] = &[
    "logprobs",
    "top_logprobs",
    "prompt_logprobs",
    "echo",
    "chat_template_kwargs",
];

/// Everything `enrich_request` captures for a later [`run`] call.
pub struct SpeculationPlan {
    /// The fully enriched client body — the escalation dispatch base.
    pub request: Value,
    /// The request messages with text-part lists flattened to plain strings;
    /// what the drafter answers and the verifier scores.
    pub messages: Value,
    /// Settings snapshot so a hot reload cannot change gates mid-request.
    pub settings: SpeculationBoonSettings,
    /// The client asked for `stream: true`.
    pub client_stream: bool,
    /// The client asked for `stream_options.include_usage`.
    pub include_usage: bool,
    /// A gateway-tool boon (image generation or web search) armed the tool
    /// loop for this request, so the target could answer it by drawing
    /// something or searching first. The cascade cannot: the drafter is a
    /// different model and the scoring endpoint only scores, so a committed
    /// cascade means no image and no search at all.
    ///
    /// When this is set, an unclassified request abstains instead of
    /// speculating — the cost of being wrong is a missing picture or a stale
    /// answer, not a slow one, so silence from the classifier has to mean "let
    /// the target have it". See the guard in [`run`].
    pub gateway_tool_pending: bool,
}

/// Token/latency stats the streaming driver reports back for settlement,
/// mirroring `tool_stream::StreamStats`.
#[derive(Default)]
pub struct SpecStats {
    pub ttft_ms: u32,
    /// What the request settles with once `final_set`: the target's own
    /// usage after an escalation (cached prompt tokens included), or the
    /// gateway's count for a shipped draft.
    pub usage: UpstreamUsage,
    pub final_set: bool,
    /// The status to record when the continuation failed: it broke off,
    /// could not be dispatched, or reported an error of its own. The tokens
    /// above are then what reached the client.
    pub failure: Option<u16>,
    /// The cascade's trace spans (`boon:speculation` and its draft, verify
    /// and escalate children), set when it ends: by [`run`] for an abstain
    /// or a non-streaming ship, by the stream once a committed stream ends.
    pub spans: Vec<crate::tracer::PendingSpan>,
}

/// Times of the cascade's phases, for its trace spans.
#[derive(Clone, Default)]
struct SpecTrace {
    start_ms: i64,
    drafter: String,
    /// The draft call: dispatched, and ended (complete, or abandoned).
    draft: Option<(i64, Option<i64>)>,
    /// The continuation the target wrote after an escalation.
    escalate: Option<(i64, i64, &'static str)>,
}

impl SpecTrace {
    fn new() -> Self {
        SpecTrace {
            start_ms: crate::tracer::now_ms(),
            ..SpecTrace::default()
        }
    }

    fn draft_started(&mut self) {
        self.draft = Some((crate::tracer::now_ms(), None));
    }

    fn draft_ended(&mut self, at_ms: i64) {
        if let Some((_, end)) = self.draft.as_mut() {
            end.get_or_insert(at_ms);
        }
    }

    /// The spans for a cascade that ended `outcome` (`abstain`, `shipped`,
    /// `escalated`, `error`), with the verifier's scorings when it had one.
    fn spans(
        mut self,
        outcome: &'static str,
        reason: Option<&'static str>,
        verify: Option<&VerifyTimes>,
    ) -> Vec<crate::tracer::PendingSpan> {
        use crate::tracer::PendingSpan;
        let now = crate::tracer::now_ms();
        self.draft_ended(now);
        let parent = "boon:speculation";
        let mut spans = Vec::new();
        if let Some((start, end)) = self.draft {
            spans.push(PendingSpan::between(
                "boon:speculation:draft",
                parent,
                start,
                end.unwrap_or(now),
                "ok",
                json!({ "model": self.drafter }),
            ));
        }
        if let Some(v) = verify.filter(|v| v.scorings > 0) {
            spans.push(PendingSpan::between(
                "boon:speculation:verify",
                parent,
                v.first_ms,
                v.last_ms,
                "ok",
                json!({ "scorings": v.scorings, "scoring_ms": v.busy_ms }),
            ));
        }
        if let Some((start, end, status)) = self.escalate {
            spans.push(PendingSpan::between(
                "boon:speculation:escalate",
                parent,
                start,
                end,
                status,
                json!({}),
            ));
        }
        spans.push(PendingSpan::between(
            parent,
            "proxy_request",
            self.start_ms,
            now,
            if outcome == "error" { "error" } else { "ok" },
            json!({ "outcome": outcome, "reason": reason, "drafter": self.drafter }),
        ));
        spans
    }
}

/// When the verifier scored, and for how long in all.
#[derive(Clone, Copy, Default)]
struct VerifyTimes {
    scorings: u32,
    first_ms: i64,
    last_ms: i64,
    busy_ms: i64,
}

fn set_spans(stats: &Mutex<SpecStats>, spans: Vec<crate::tracer::PendingSpan>) {
    if let Ok(mut st) = stats.lock() {
        st.spans = spans;
    }
}

/// What [`run`] decided. Both committed outcomes carry `drafted_by`, the
/// drafter whose text reaches the client: the decision is made before any
/// response header goes out, so the proxy can say so in one.
pub enum Outcome {
    /// Non-streaming ship: a complete, verified draft as a chat completion.
    ShippedJson {
        body: Value,
        input_tokens: u32,
        output_tokens: u32,
        drafted_by: String,
    },
    /// Streaming commit: at least one verified span exists; the driver owns
    /// the response from here (release, further verification, escalation).
    Stream {
        driver: Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>,
        drafted_by: String,
    },
    /// No commitment was made; the proxy proceeds exactly as if the boon
    /// never ran.
    Abstain(&'static str),
}

/// The last user message's text, for classification.
pub(crate) fn last_user_text(messages: &Value) -> String {
    messages
        .as_array()
        .and_then(|a| {
            a.iter()
                .rev()
                .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        })
        .and_then(|m| m.get("content").and_then(|c| c.as_str()))
        .unwrap_or_default()
        .to_string()
}

/// The speculation boon is eligible when the model was granted it, the boon is
/// globally active (enabled with a drafter and a verifier configured), the key
/// is not an internal probe (probes must measure the target model itself), and
/// the drafter is not the target. Request-body eligibility (bypass params,
/// multimodal content) is decided separately by
/// [`eligible_messages`].
pub(super) fn eligible(
    route: &obleth_config::ResolvedModel,
    settings: &obleth_config::BoonSettings,
    key: &obleth_config::ResolvedKey,
) -> bool {
    if key.internal
        || !settings.speculation.active()
        || !route.has_boon(obleth_config::Boon::Speculation)
    {
        return false;
    }
    // Both helpers resolve per target: its own drafter (fleet default as
    // fallback) and its own scoring endpoint. Missing either means the model
    // cannot speculate, decided here before anything is armed.
    if scoring_base(route, &settings.speculation.verify_url_template).is_empty() {
        return false;
    }
    let drafter = effective_draft_model(route, &settings.speculation);
    !drafter.is_empty() && drafter != route.model_name
}

/// Decide eligibility from the request body and return the flattened messages
/// when eligible. `None` = ineligible (multimodal parts, bypass params, n>1,
/// no messages).
pub(crate) fn eligible_messages(json: &Value) -> Option<Value> {
    let obj = json.as_object()?;
    for k in BYPASS_KEYS {
        if obj.get(*k).is_some_and(|v| !v.is_null()) {
            return None;
        }
    }
    if obj.get("n").and_then(|v| v.as_u64()).unwrap_or(1) > 1 {
        return None;
    }
    let messages = obj.get("messages")?.as_array()?;
    if messages.is_empty() {
        return None;
    }
    let mut flat = Vec::with_capacity(messages.len());
    for m in messages {
        let content = m.get("content");
        match content {
            None | Some(Value::Null) | Some(Value::String(_)) => flat.push(m.clone()),
            Some(Value::Array(parts)) => {
                // Some clients send pure text as a one-element part list;
                // flatten those. Any non-text part (images, audio) makes the
                // request ineligible.
                let mut text = String::new();
                for p in parts {
                    if p.get("type").and_then(|t| t.as_str()) != Some("text") {
                        return None;
                    }
                    if let Some(t) = p.get("text").and_then(|t| t.as_str()) {
                        text.push_str(t);
                    }
                }
                let mut m = m.clone();
                if let Some(o) = m.as_object_mut() {
                    o.insert("content".into(), Value::String(text));
                }
                flat.push(m);
            }
            _ => return None,
        }
    }
    Some(Value::Array(flat))
}

/// The three-zone streaming gate.
#[derive(Debug, PartialEq, Clone, Copy)]
pub(crate) enum Zone {
    /// At or above the ship gate: release.
    Pass,
    /// Below the abort floors: stop drafting, escalate now.
    Abort,
    /// In between: keep drafting, defer the decision.
    Defer,
}

/// The gate in effect for one request: the global floors, possibly overridden
/// by the request's category. Calibration shows precision is strongly
/// category-dependent, so the ship floors are per-category while the abort
/// floors (a coarse "this draft is dead" signal) stay global.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Gate {
    pub agree_min: f64,
    pub lp_min: f64,
    pub abort_agree: f64,
    pub abort_lp: f64,
}

impl Gate {
    pub(crate) fn global(s: &SpeculationBoonSettings) -> Gate {
        Gate {
            agree_min: s.agree_min,
            lp_min: s.lp_min,
            abort_agree: s.abort_agree,
            abort_lp: s.abort_lp,
        }
    }
}

/// Resolve the gate for a request classified into `tags`. `None` = this
/// request must not speculate at all (category excluded, or unlisted while
/// `unlisted_categories_speculate` is off).
/// The drafter for one target: the model's own `draft_model`, else the fleet
/// default from the boon settings, else empty (cannot speculate).
pub(crate) fn effective_draft_model(route: &ResolvedModel, s: &SpeculationBoonSettings) -> String {
    let own = route.draft_model.trim();
    if !own.is_empty() {
        return own.to_string();
    }
    s.draft_model
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string()
}

/// The scoring address for one target: its own `verify_api_base`, else the
/// fleet template with `{upstream}`/`{model}` substituted. Empty = cannot
/// speculate.
///
/// The model's upstream key is sent to this address, so a model's fields must
/// never be able to redirect it (see [`fill_template`]). A template the fill
/// refuses yields no scoring endpoint: speculation is skipped, the request is
/// served normally.
pub(crate) fn scoring_base(route: &ResolvedModel, template: &str) -> String {
    let own = route.verify_api_base.trim();
    if !own.is_empty() {
        return own.to_string();
    }
    let template = template.trim();
    if template.is_empty() {
        return String::new();
    }
    match fill_template(template, route) {
        Some(url) => url,
        None => {
            // Runs on every request to the model, so it cannot be a warn:
            // one misconfigured model would flood the logs at load.
            tracing::debug!(
                model = %route.model_name,
                "speculation verify_url_template cannot be filled safely for this model; \
                 speculation skipped"
            );
            String::new()
        }
    }
}

/// Fill `{upstream}`/`{model}` per position:
/// - in the host, the value must be plain DNS labels (no `@`, `:`, `/`, …) and
///   the filled host must not be an IP literal, so a model field can name a
///   per-model Service but never an address such as the metadata endpoint;
/// - in the path/query, the value is percent-encoded into one opaque component.
///
/// Placeholders in the scheme or credentials are refused outright (saves
/// reject them too). `None` = refuse.
fn fill_template(template: &str, route: &ResolvedModel) -> Option<String> {
    use obleth_admin::ssrf::{check_template_structure, VERIFY_TEMPLATE_PLACEHOLDERS};
    check_template_structure(template, VERIFY_TEMPLATE_PLACEHOLDERS).ok()?;

    // Byte range of the authority (`host[:port]`; userinfo is refused above).
    let authority_start = template.find("://").map_or(0, |i| i + 3);
    let authority_end = template[authority_start..]
        .find(['/', '?', '#'])
        .map_or(template.len(), |i| authority_start + i);

    let mut out = String::with_capacity(template.len() + 32);
    let mut host_filled = false;
    let mut rest = template;
    while !rest.is_empty() {
        let pos = template.len() - rest.len();
        let hit = [
            ("{upstream}", route.upstream_model.as_str()),
            ("{model}", route.model_name.as_str()),
        ]
        .into_iter()
        .find(|(placeholder, _)| rest.starts_with(placeholder));
        match hit {
            Some((placeholder, value)) => {
                if (authority_start..authority_end).contains(&pos) {
                    if !is_dns_name(value) {
                        return None;
                    }
                    out.push_str(value);
                    host_filled = true;
                } else {
                    out.push_str(&encode_template_value(value));
                }
                rest = &rest[placeholder.len()..];
            }
            None => {
                let ch = rest.chars().next()?;
                out.push(ch);
                rest = &rest[ch.len_utf8()..];
            }
        }
    }

    let url = reqwest::Url::parse(&out).ok()?;
    let host = url.host_str()?;
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    // Covers labels that together form an address (`169.254.169.254`) and
    // numeric forms the URL parser normalizes to one (`2852039166`). A fixed
    // literal host in the template itself was policy-checked on save.
    if host_filled && bare.parse::<std::net::IpAddr>().is_ok() {
        return None;
    }
    Some(out)
}

/// One or more dot-separated DNS labels: alphanumerics and inner hyphens.
fn is_dns_name(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|label| {
            !label.is_empty()
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// Percent-encode everything outside RFC 3986's unreserved set, so a value is
/// always a single opaque path/query component.
fn encode_template_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The verifier for one target, synthesized from the target itself with the
/// wire target swapped to its scoring endpoint. Billing therefore lands under
/// the target's name at the target's rates, which is honest: the scorer IS
/// the target's family. Returns `None` when the model has no scoring endpoint.
pub(crate) fn scoring_route(route: &ResolvedModel, template: &str) -> Option<Arc<ResolvedModel>> {
    let base = scoring_base(route, template);
    if base.is_empty() {
        return None;
    }
    let mut verifier = (*route).clone();
    verifier.api_base = base;
    let served = route.verify_upstream_model.trim();
    if !served.is_empty() {
        verifier.upstream_model = served.to_string();
    }
    // The scoring endpoint is a single direct URL; the target's endpoint list
    // must not override it.
    verifier.endpoints = Vec::new();
    Some(Arc::new(verifier))
}

pub(crate) fn resolve_gate(s: &SpeculationBoonSettings, tags: &[String]) -> Option<Gate> {
    for g in &s.category_gates {
        if tags.iter().any(|t| t == &g.tag) {
            if !g.speculate {
                return None;
            }
            return Some(Gate {
                agree_min: g.agree_min,
                lp_min: g.lp_min,
                abort_agree: s.abort_agree,
                abort_lp: s.abort_lp,
            });
        }
    }
    s.unlisted_categories_speculate.then(|| Gate::global(s))
}

pub(crate) fn zone(g: &Gate, agree: f64, mean_lp: f64) -> Zone {
    if agree >= g.agree_min && mean_lp >= g.lp_min {
        Zone::Pass
    } else if agree < g.abort_agree || mean_lp < g.abort_lp {
        Zone::Abort
    } else {
        Zone::Defer
    }
}

/// Strip the trailing `/v1` from an OpenAI-style api_base: the backend's
/// `/tokenize` endpoint lives at the server root, not under `/v1`.
pub(crate) fn backend_root(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    base.strip_suffix("/v1").unwrap_or(base).to_string()
}

/// One verify scoring: agreement rate, mean logprob, draft token count.
#[derive(Debug, PartialEq)]
pub(crate) struct Score {
    pub agree: f64,
    pub mean_lp: f64,
    pub n_draft: usize,
}

/// What scoring reads of one prompt position: the sent token's rank and
/// log-probability.
#[derive(Debug, Clone, Copy, PartialEq, serde::Deserialize)]
pub(crate) struct Pick {
    #[serde(default)]
    rank: Option<u64>,
    #[serde(default)]
    logprob: Option<f64>,
}

/// Read a `/v1/completions` reply for scoring: for each draft position
/// (`start` onward), the entry under the sent token id, or `None` when that
/// key is missing. Every other position, every other key and the rest of the
/// reply are skipped unread, so a long prompt's megabytes of logprobs are
/// never built into a tree. `Ok(None)` when the reply has no
/// `prompt_logprobs`.
pub(crate) fn read_picks(
    reply: &[u8],
    start: usize,
    draft_ids: &[i64],
) -> serde_json::Result<Option<Vec<Option<Pick>>>> {
    use serde::de::DeserializeSeed;
    let mut de = serde_json::Deserializer::from_slice(reply);
    let picks = picks::Reply { start, draft_ids }.deserialize(&mut de)?;
    de.end()?;
    Ok(picks)
}

/// The visitors behind [`read_picks`].
mod picks {
    use std::fmt;

    use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};

    use super::Pick;

    type Picks = Option<Vec<Option<Pick>>>;

    /// `{"choices": [{"prompt_logprobs": [...]}, ...], ...}`
    pub(super) struct Reply<'a> {
        pub(super) start: usize,
        pub(super) draft_ids: &'a [i64],
    }

    impl<'de> DeserializeSeed<'de> for Reply<'_> {
        type Value = Picks;
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Picks, D::Error> {
            d.deserialize_map(self)
        }
    }

    impl<'de> Visitor<'de> for Reply<'_> {
        type Value = Picks;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a completion")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Picks, A::Error> {
            let mut picks = None;
            while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                if key == "choices" && picks.is_none() {
                    picks = map.next_value_seed(Choices {
                        start: self.start,
                        draft_ids: self.draft_ids,
                    })?;
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(picks)
        }
    }

    /// The first choice's `prompt_logprobs`; later choices are skipped.
    struct Choices<'a> {
        start: usize,
        draft_ids: &'a [i64],
    }

    impl<'de> DeserializeSeed<'de> for Choices<'_> {
        type Value = Picks;
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Picks, D::Error> {
            d.deserialize_any(self)
        }
    }

    impl<'de> Visitor<'de> for Choices<'_> {
        type Value = Picks;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a list of choices")
        }
        fn visit_unit<E>(self) -> Result<Picks, E> {
            Ok(None)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Picks, A::Error> {
            let picks = seq
                .next_element_seed(Choice {
                    start: self.start,
                    draft_ids: self.draft_ids,
                })?
                .flatten();
            while seq.next_element::<IgnoredAny>()?.is_some() {}
            Ok(picks)
        }
    }

    struct Choice<'a> {
        start: usize,
        draft_ids: &'a [i64],
    }

    impl<'de> DeserializeSeed<'de> for Choice<'_> {
        type Value = Picks;
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Picks, D::Error> {
            d.deserialize_map(self)
        }
    }

    impl<'de> Visitor<'de> for Choice<'_> {
        type Value = Picks;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a choice")
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Picks, A::Error> {
            let mut picks = None;
            while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                if key == "prompt_logprobs" && picks.is_none() {
                    picks = map.next_value_seed(Positions {
                        start: self.start,
                        draft_ids: self.draft_ids,
                    })?;
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(picks)
        }
    }

    /// `prompt_logprobs`: one entry per prompt token, `null` for the first.
    struct Positions<'a> {
        start: usize,
        draft_ids: &'a [i64],
    }

    impl<'de> DeserializeSeed<'de> for Positions<'_> {
        type Value = Picks;
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Picks, D::Error> {
            d.deserialize_any(self)
        }
    }

    impl<'de> Visitor<'de> for Positions<'_> {
        type Value = Picks;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("prompt_logprobs")
        }
        fn visit_unit<E>(self) -> Result<Picks, E> {
            Ok(None)
        }
        fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Picks, A::Error> {
            let mut picks = vec![None; self.draft_ids.len()];
            let mut at = 0usize;
            loop {
                let wanted = at
                    .checked_sub(self.start)
                    .and_then(|k| self.draft_ids.get(k).map(|id| (k, id)));
                let more = match wanted {
                    Some((k, id)) => match seq.next_element_seed(Entry {
                        key: id.to_string(),
                    })? {
                        Some(pick) => {
                            picks[k] = pick;
                            true
                        }
                        None => false,
                    },
                    None => seq.next_element::<IgnoredAny>()?.is_some(),
                };
                if !more {
                    return Ok(Some(picks));
                }
                at += 1;
            }
        }
    }

    /// One position: `{"<token id>": {"logprob": .., "rank": ..}, ...}` or
    /// `null`; the entry under `key`, if any.
    struct Entry {
        key: String,
    }

    impl<'de> DeserializeSeed<'de> for Entry {
        type Value = Option<Pick>;
        fn deserialize<D: Deserializer<'de>>(self, d: D) -> Result<Option<Pick>, D::Error> {
            d.deserialize_any(self)
        }
    }

    impl<'de> Visitor<'de> for Entry {
        type Value = Option<Pick>;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("a prompt logprob entry")
        }
        fn visit_unit<E>(self) -> Result<Option<Pick>, E> {
            Ok(None)
        }
        fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Option<Pick>, A::Error> {
            let mut pick = None;
            while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                if pick.is_none() && *key == *self.key {
                    pick = Some(map.next_value::<Pick>()?);
                } else {
                    map.next_value::<IgnoredAny>()?;
                }
            }
            Ok(pick)
        }
    }
}

/// Score the draft from its picks. Returns `Err(misses)` on the cold-bucket
/// corrupt-keys signature (>10% of entries missing the sent token id as their
/// key).
pub(crate) fn score_picks(picks: &[Option<Pick>]) -> Result<Score, usize> {
    let mut agree = 0usize;
    let mut lp_sum = 0.0f64;
    let mut lp_n = 0usize;
    let mut misses = 0usize;
    for pick in picks {
        let Some(pick) = pick else {
            misses += 1;
            continue;
        };
        if pick.rank == Some(1) {
            agree += 1;
        }
        if let Some(lp) = pick.logprob {
            lp_sum += lp;
            lp_n += 1;
        }
    }
    if misses > picks.len() / 10 {
        return Err(misses);
    }
    Ok(Score {
        agree: agree as f64 / picks.len().max(1) as f64,
        mean_lp: if lp_n > 0 {
            lp_sum / lp_n as f64
        } else {
            -99.0
        },
        n_draft: picks.len(),
    })
}

/// Pick the closer that lands scoring after an EMPTY think block: when the
/// generation prompt already ends inside an open `<think>`, close it;
/// otherwise splice a complete empty block.
pub(crate) fn think_closer(tail_token_strs: &[String]) -> &'static str {
    if tail_token_strs.concat().contains("<think>") {
        "</think>\n"
    } else {
        "<think></think>\n"
    }
}

/// The think-block closer's token ids per (scoring root, upstream model,
/// closer): a constant string, so tokenized once per model rather than once
/// per request. An hour's life covers a model redeployed with a new
/// tokenizer.
type CloserKey = (String, String, &'static str);

static CLOSER_IDS: std::sync::LazyLock<moka::future::Cache<CloserKey, Arc<[i64]>>> =
    std::sync::LazyLock::new(|| {
        moka::future::Cache::builder()
            .max_capacity(256)
            .time_to_live(Duration::from_secs(3600))
            .build()
    });

/// The verifier client: tokenize + prompt_logprobs scoring against the
/// target's scoring endpoint.
struct Verifier {
    model: Arc<ResolvedModel>,
    root: String,
    /// The scoring prompt: the templated prompt ids ending in a closed empty
    /// think block (its first `prefix_len` ids, computed once per request),
    /// then the draft being scored. Reused across scorings.
    prompt: Vec<i64>,
    prefix_len: Option<usize>,
    /// Prefill tokens consumed across all scorings, for helper billing.
    billed_input: u32,
    scorings: u32,
    /// Each call's own bound: after the commit, a stalled scorer ends the
    /// stream through this instead of leaving it silent.
    timeout: Duration,
    /// Largest reply read; a `prompt_logprobs` answer is megabytes for a long prompt.
    reply_max: usize,
    /// When the scorings ran, for the trace.
    times: VerifyTimes,
}

impl Verifier {
    fn new(model: Arc<ResolvedModel>, s: &SpeculationBoonSettings) -> Self {
        let root = backend_root(&model.api_base);
        Verifier {
            model,
            root,
            prompt: Vec::new(),
            prefix_len: None,
            billed_input: 0,
            scorings: 0,
            timeout: Duration::from_millis(s.verify_timeout_ms.max(1)),
            reply_max: s.verify_reply_max_bytes.max(1),
            times: VerifyTimes::default(),
        }
    }

    async fn post(
        &self,
        http: &reqwest::Client,
        url: String,
        body: Value,
    ) -> anyhow::Result<Bytes> {
        let headers = reqwest::header::HeaderMap::new();
        let call = super::post_json_capped(
            http,
            url,
            &headers,
            self.model.api_key.as_deref(),
            &body,
            self.reply_max,
        );
        super::with_timeout("speculation verify", self.timeout, call)
            .await
            .inspect_err(|e| {
                tracing::warn!(model = %self.model.model_name, error = %e, "speculation verifier call failed")
            })
    }

    async fn post_json(
        &self,
        http: &reqwest::Client,
        url: String,
        body: Value,
    ) -> anyhow::Result<Value> {
        Ok(serde_json::from_slice(&self.post(http, url, body).await?)?)
    }

    async fn tokenize(&self, http: &reqwest::Client, mut body: Value) -> anyhow::Result<Value> {
        if let Some(o) = body.as_object_mut() {
            o.insert(
                "model".into(),
                Value::String(self.model.upstream_model.clone()),
            );
        }
        self.post_json(http, format!("{}/tokenize", self.root), body)
            .await
    }

    fn token_ids(v: &Value) -> Vec<i64> {
        v.get("tokens")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default()
    }

    /// Templated prompt ids for scoring, ending in a CLOSED empty think block.
    /// Started alongside the draft call, so the tokenizer round trips overlap
    /// the drafter's first token; a later call is a no-op once it succeeded.
    async fn prefix_ids(&mut self, http: &reqwest::Client, messages: &Value) -> anyhow::Result<()> {
        if self.prefix_len.is_some() {
            return Ok(());
        }
        let pre = self
            .tokenize(
                http,
                json!({
                    "messages": messages,
                    "add_generation_prompt": true,
                    "return_token_strs": true,
                }),
            )
            .await?;
        let tail: Vec<String> = pre
            .get("token_strs")
            .and_then(|t| t.as_array())
            .map(|a| {
                a.iter()
                    .rev()
                    .take(4)
                    .rev()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        let closer = think_closer(&tail);
        let key = (self.root.clone(), self.model.upstream_model.clone(), closer);
        let closer_ids = match CLOSER_IDS.get(&key).await {
            Some(ids) => ids,
            None => {
                let clo = self
                    .tokenize(
                        http,
                        json!({ "prompt": closer, "add_special_tokens": false }),
                    )
                    .await?;
                let ids: Arc<[i64]> = Self::token_ids(&clo).into();
                CLOSER_IDS.insert(key, ids.clone()).await;
                ids
            }
        };
        let mut ids = Self::token_ids(&pre);
        if ids.is_empty() {
            anyhow::bail!("verify tokenize returned no tokens");
        }
        ids.extend_from_slice(&closer_ids);
        self.prefix_len = Some(ids.len());
        self.prompt = ids;
        Ok(())
    }

    /// Score `draft_text`, timing the call for the trace.
    async fn score(
        &mut self,
        state: &AppState,
        messages: &Value,
        draft_text: &str,
    ) -> anyhow::Result<Score> {
        let start = crate::tracer::now_ms();
        let score = self.score_once(state, messages, draft_text).await;
        let end = crate::tracer::now_ms();
        let t = &mut self.times;
        if t.scorings == 0 {
            t.first_ms = start;
        }
        t.scorings += 1;
        t.last_ms = end;
        t.busy_ms += end - start;
        score
    }

    /// Score `draft_text` with one prefill over prefix+draft. Retries once on
    /// the cold-bucket corrupt-keys signature.
    async fn score_once(
        &mut self,
        state: &AppState,
        messages: &Value,
        draft_text: &str,
    ) -> anyhow::Result<Score> {
        self.prefix_ids(&state.http, messages).await?;
        let start = self.prefix_len.expect("prefix computed above");
        let dtok = self
            .tokenize(
                &state.http,
                json!({ "prompt": draft_text, "add_special_tokens": false }),
            )
            .await?;
        let draft_ids = Self::token_ids(&dtok);
        if draft_ids.is_empty() {
            anyhow::bail!("draft tokenized to nothing");
        }
        self.prompt.truncate(start);
        self.prompt.extend_from_slice(&draft_ids);
        let total = self.prompt.len() as u32;
        for attempt in 0..2u8 {
            let reply = self
                .post(
                    &state.http,
                    format!("{}/v1/completions", self.root),
                    json!({
                        "model": self.model.upstream_model,
                        "prompt": self.prompt,
                        "max_tokens": 1,
                        "temperature": 0.0,
                        "prompt_logprobs": 0,
                    }),
                )
                .await?;
            self.billed_input = self.billed_input.saturating_add(total);
            self.scorings += 1;
            let Some(picks) = read_picks(&reply, start, &draft_ids)? else {
                anyhow::bail!("verify backend returned no prompt_logprobs");
            };
            match score_picks(&picks) {
                Ok(score) => return Ok(score),
                Err(misses) => {
                    tracing::info!(
                        misses,
                        total = draft_ids.len(),
                        attempt,
                        "speculation: corrupt prompt_logprobs keys (cold-bucket signature), retrying"
                    );
                }
            }
        }
        anyhow::bail!("prompt_logprobs keys corrupt after retry")
    }

    fn prefix_len(&self) -> u32 {
        self.prefix_len.unwrap_or(0) as u32
    }
}

/// Dispatch a streaming chat call to a model (drafter or escalation target).
async fn dispatch_stream(
    state: &AppState,
    model: &ResolvedModel,
    body: &Value,
    timeout: Duration,
) -> anyhow::Result<reqwest::Response> {
    let target = super::helper_target(model, "", None)?;
    let mut req = state
        .http
        .post(super::build_chat_url(&target.base))
        .headers(target.headers.clone())
        .json(body);
    if let Some(api_key) = &target.api_key {
        req = req.bearer_auth(api_key);
    }
    let resp = super::with_timeout("speculation dispatch", timeout, async {
        Ok(req.send().await?)
    })
    .await?;
    if !resp.status().is_success() {
        anyhow::bail!("upstream returned {}", resp.status());
    }
    Ok(resp)
}

/// Incremental reader over a drafter SSE stream: accumulates content deltas
/// and captures finish_reason/usage.
struct DraftReader {
    turn: super::tool_stream::TurnReader,
    pub deltas: Vec<String>,
    pub finish: Option<String>,
    pub usage: Option<UpstreamUsage>,
    pub done: bool,
}

impl DraftReader {
    fn new(resp: reqwest::Response) -> Self {
        DraftReader {
            turn: super::tool_stream::TurnReader::new(
                resp,
                super::tool_stream::stream_idle_timeout(),
            ),
            deltas: Vec::new(),
            finish: None,
            usage: None,
            done: false,
        }
    }

    fn consume(&mut self, v: &Value) {
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(chunk_usage(u));
        }
        let Some(choice) = v.pointer("/choices/0") else {
            return;
        };
        if let Some(f) = choice.get("finish_reason").and_then(|f| f.as_str()) {
            self.finish = Some(f.to_string());
        }
        if let Some(d) = choice
            .pointer("/delta/content")
            .and_then(|c| c.as_str())
            .filter(|s| !s.is_empty())
        {
            self.deltas.push(d.to_string());
        }
    }

    /// Read until at least one more event is consumed or the stream ends.
    /// Returns false when the stream is exhausted.
    async fn advance(&mut self) -> anyhow::Result<bool> {
        use super::tool_stream::TurnEvent;
        match self.turn.next().await {
            TurnEvent::Chunk { value, .. } => self.consume(&value),
            // An error event carries no draft text; the stream ends after it.
            TurnEvent::Error { .. } => {}
            TurnEvent::Done => self.done = true,
            TurnEvent::Cut(e) => anyhow::bail!("draft stream read failed: {e}"),
            TurnEvent::End => {
                self.done = true;
                return Ok(false);
            }
        }
        Ok(true)
    }
}

fn draft_body(
    messages: &Value,
    upstream_model: &str,
    settings: &SpeculationBoonSettings,
    client_max_tokens: Option<u64>,
    stream: bool,
) -> Value {
    let max_tokens = client_max_tokens
        .unwrap_or(settings.max_draft_tokens as u64)
        .min(settings.max_draft_tokens as u64);
    let mut body = json!({
        "model": upstream_model,
        "messages": messages,
        "temperature": 0.0,
        "max_tokens": max_tokens,
        "stream": stream,
    });
    if stream {
        body["stream_options"] = json!({ "include_usage": true });
    }
    if let Some(kwargs) = &settings.draft_chat_template_kwargs {
        if !kwargs.is_null() {
            body["chat_template_kwargs"] = kwargs.clone();
        }
    }
    body
}

/// Build the escalation body: the enriched client request continued from the
/// released verified prefix through the target's chat template.
pub(crate) fn escalation_body(
    request: &Value,
    messages: &Value,
    upstream_model: &str,
    released_text: &str,
) -> Value {
    let mut body = request.clone();
    if let Some(o) = body.as_object_mut() {
        for k in ESCALATE_STRIP_KEYS {
            o.remove(*k);
        }
        o.insert("model".into(), Value::String(upstream_model.to_string()));
        o.insert("stream".into(), Value::Bool(true));
        o.insert("stream_options".into(), json!({ "include_usage": true }));
        let mut msgs = messages.as_array().cloned().unwrap_or_default();
        msgs.push(json!({ "role": "assistant", "content": released_text }));
        o.insert("messages".into(), Value::Array(msgs));
        o.insert("add_generation_prompt".into(), Value::Bool(false));
        o.insert("continue_final_message".into(), Value::Bool(true));
    }
    body
}

/// Everything [`run`] needs from the proxy.
pub struct SpecRequest<'a> {
    pub state: &'a AppState,
    pub route: Arc<ResolvedModel>,
    pub key: &'a ResolvedKey,
    pub session_id: &'a str,
    /// The client request being answered; the draft and verify calls are
    /// billed against it.
    pub request_id: uuid::Uuid,
    pub dispatch_timeout: Duration,
    pub started: Instant,
}

/// Run the cascade up to its commit point. See the module docs for the
/// contract; every failure before a verified span is released returns
/// [`Outcome::Abstain`]. An abstain or a non-streaming ship leaves its trace
/// spans in `stats` now; a committed stream leaves them when it ends.
pub async fn run(
    req: SpecRequest<'_>,
    plan: SpeculationPlan,
    stats: Arc<Mutex<SpecStats>>,
) -> Outcome {
    let mut trace = SpecTrace::new();
    let mut verify = None;
    let outcome = cascade(req, plan, stats.clone(), &mut trace, &mut verify).await;
    match &outcome {
        Outcome::Abstain(reason) => {
            set_spans(
                &stats,
                trace.spans("abstain", Some(reason), verify.as_ref()),
            );
        }
        Outcome::ShippedJson { .. } => {
            set_spans(&stats, trace.spans("shipped", None, verify.as_ref()));
        }
        Outcome::Stream { .. } => {}
    }
    outcome
}

/// The cascade behind [`run`]. Fills `trace` as it goes, and `verify` with the
/// verifier's scorings when it does not hand the verifier to a stream.
async fn cascade(
    req: SpecRequest<'_>,
    plan: SpeculationPlan,
    stats: Arc<Mutex<SpecStats>>,
    trace: &mut SpecTrace,
    verify: &mut Option<VerifyTimes>,
) -> Outcome {
    let s = &plan.settings;
    // The drafter is the target's own choice; the fleet default is the fallback.
    let draft_name = effective_draft_model(&req.route, s);
    if draft_name.is_empty() {
        return Outcome::Abstain("no draft model");
    }
    // The target must not be its own drafter (a self-cascade only adds cost).
    if draft_name == req.route.model_name {
        return Outcome::Abstain("target is the drafter");
    }
    let Some(drafter) = crate::proxy::resolve_model(req.state, &draft_name).await else {
        tracing::warn!(model = %draft_name, "speculation drafter is not registered; skipping");
        return Outcome::Abstain("drafter unresolved");
    };
    if !drafter.enabled {
        return Outcome::Abstain("helper disabled");
    }
    trace.drafter = drafter.model_name.clone();
    // Verification is the target's own scoring endpoint: a direct-URL
    // deployment of this model whose backend supports prompt_logprobs (its
    // own pods once patched, a canary until then). Not a registered route.
    let Some(verifier_model) = scoring_route(&req.route, &s.verify_url_template) else {
        tracing::info!(
            target = %req.route.model_name,
            "target has no scoring endpoint configured; answering directly"
        );
        return Outcome::Abstain("no scoring endpoint");
    };

    // Per-category gate: classify the request with this boon's own tag
    // vocabulary (fail-open to empty tags = the unlisted default). A
    // `speculate: false` category abstains HERE, before any draft cost.
    let mut tags: Vec<String> = Vec::new();
    if !s.category_gates.is_empty() {
        if let Some(brain_name) = s.classify_model.as_deref() {
            match crate::proxy::resolve_model(req.state, brain_name).await {
                Some(brain) if brain.enabled => {
                    let vocab: Vec<String> =
                        s.category_gates.iter().map(|g| g.tag.clone()).collect();
                    let prompt = last_user_text(&plan.messages);
                    let intent = req
                        .state
                        .classifier
                        .classify(&req.state.http, &brain, &prompt, &vocab)
                        .await;
                    tags = intent.tags;
                }
                _ => {
                    tracing::warn!(
                        model = %brain_name,
                        "speculation classify model unavailable; using the unlisted-category default"
                    );
                }
            }
        }
    }
    // With a gateway tool on the table, only a positive classification may
    // claim the request. No tags means the classifier was unavailable, or had
    // nothing to say, and guessing wrong here costs the user the picture or
    // the search they needed.
    if plan.gateway_tool_pending && tags.is_empty() {
        tracing::info!(
            target = %req.route.model_name,
            "a gateway tool is armed and the request is unclassified; target model answers directly"
        );
        return Outcome::Abstain("gateway tool pending, intent unclassified");
    }
    let Some(gate) = resolve_gate(s, &tags) else {
        tracing::info!(
            ?tags,
            "speculation category excluded; target model answers directly"
        );
        return Outcome::Abstain("category excluded");
    };

    let budget = Duration::from_millis(s.timeout_ms.max(1));
    let client_max_tokens = plan
        .request
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .filter(|v| *v > 0);
    let mut verifier = Verifier::new(verifier_model, s);
    trace.draft_started();

    if plan.client_stream {
        let precommitted = tokio::time::timeout(
            budget,
            precommit_stream(
                &req,
                &plan,
                gate,
                &drafter,
                &mut verifier,
                client_max_tokens,
            ),
        )
        .await;
        return match precommitted {
            Ok(Ok(committed)) => Outcome::Stream {
                drafted_by: drafter.model_name.clone(),
                driver: drive_stream(
                    req,
                    plan,
                    gate,
                    drafter,
                    verifier,
                    committed,
                    stats,
                    std::mem::take(trace),
                ),
            },
            Ok(Err(reason)) => {
                *verify = Some(verifier.times);
                abstain(&req, &verifier, reason)
            }
            Err(_) => {
                *verify = Some(verifier.times);
                abstain(&req, &verifier, "pre-release budget exhausted")
            }
        };
    }
    let outcome = tokio::time::timeout(
        budget,
        run_nonstream(
            &req,
            &plan,
            gate,
            &drafter,
            &mut verifier,
            client_max_tokens,
            trace,
        ),
    )
    .await;
    *verify = Some(verifier.times);
    match outcome {
        Ok(Outcome::Abstain(reason)) => abstain(&req, &verifier, reason),
        Ok(outcome) => outcome,
        Err(_) => abstain(&req, &verifier, "pre-release budget exhausted"),
    }
}

/// Log the fall-through and bill the verify prefills that were still paid.
/// (A pre-release draft abandoned mid-stream goes unbilled: its usage is
/// unknown and dropping the connection aborts the drafter's generation.)
fn abstain(req: &SpecRequest<'_>, verifier: &Verifier, reason: &'static str) -> Outcome {
    tracing::info!(
        reason,
        model = %req.route.model_name,
        "speculation abstained; falling through to the target model"
    );
    bill_verify(req, verifier);
    Outcome::Abstain(reason)
}

fn bill_verify(req: &SpecRequest<'_>, verifier: &Verifier) {
    if verifier.billed_input > 0 {
        super::bill_helper_call(
            req.state,
            &verifier.model,
            req.key,
            req.session_id,
            req.request_id,
            "speculation_verify",
            UpstreamUsage::uncached(verifier.billed_input, verifier.scorings),
        );
    }
}

fn bill_draft(req: &SpecRequest<'_>, drafter: &ResolvedModel, usage: Option<UpstreamUsage>) {
    let usage = usage.unwrap_or_default();
    if usage.input > 0 || usage.output > 0 {
        super::bill_helper_call(
            req.state,
            drafter,
            req.key,
            req.session_id,
            req.request_id,
            "speculation_draft",
            usage,
        );
    }
}

// ---------------------------------------------------------------------------
// non-streaming
// ---------------------------------------------------------------------------

async fn run_nonstream(
    req: &SpecRequest<'_>,
    plan: &SpeculationPlan,
    gate: Gate,
    drafter: &Arc<ResolvedModel>,
    verifier: &mut Verifier,
    client_max_tokens: Option<u64>,
    trace: &mut SpecTrace,
) -> Outcome {
    let s = &plan.settings;
    let body = draft_body(
        &plan.messages,
        &drafter.upstream_model,
        s,
        client_max_tokens,
        false,
    );
    // The scoring prefix is tokenized while the drafter writes; a failure
    // here is retried by the first score.
    let draft_call = async {
        let completion =
            super::chat_call_completion(req.state, drafter, &body, req.dispatch_timeout).await;
        (completion, crate::tracer::now_ms())
    };
    let ((completion, drafted_at), _) = tokio::join!(
        draft_call,
        verifier.prefix_ids(&req.state.http, &plan.messages),
    );
    trace.draft_ended(drafted_at);
    let completion = match completion {
        Ok(c) => c,
        Err(e) => {
            tracing::info!(error = %e, "speculation draft failed");
            return Outcome::Abstain("draft failed");
        }
    };
    let usage = completion
        .get("usage")
        .map(|_| super::reported_usage(&completion));
    bill_draft(req, drafter, usage);
    let draft = completion
        .pointer("/choices/0/message/content")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    let finish = completion
        .pointer("/choices/0/finish_reason")
        .and_then(|f| f.as_str())
        .unwrap_or_default();
    if draft.trim().is_empty() || finish != "stop" {
        return Outcome::Abstain("draft incomplete");
    }
    let score = match verifier.score(req.state, &plan.messages, &draft).await {
        Ok(sc) => sc,
        Err(e) => {
            tracing::info!(error = %e, "speculation verify failed");
            return Outcome::Abstain("verify failed");
        }
    };
    if !(score.agree >= gate.agree_min && score.mean_lp >= gate.lp_min) {
        tracing::info!(
            agree = score.agree,
            mean_lp = score.mean_lp,
            n = score.n_draft,
            "speculation gate fail (non-stream)"
        );
        return Outcome::Abstain("gate fail");
    }
    bill_verify(req, verifier);
    let input_tokens = verifier.prefix_len();
    let output_tokens = score.n_draft as u32;
    tracing::info!(
        agree = score.agree,
        mean_lp = score.mean_lp,
        n = score.n_draft,
        t_ms = req.started.elapsed().as_millis() as u64,
        model = %req.route.model_name,
        "speculation shipped (non-stream)"
    );
    let body = json!({
        "id": format!("chatcmpl-{}", uuid::Uuid::new_v4().simple()),
        "object": "chat.completion",
        "created": crate::tracer::now_ms() / 1000,
        "model": req.route.model_name,
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": { "role": "assistant", "content": draft },
        }],
        "usage": {
            "prompt_tokens": input_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens,
        },
    });
    Outcome::ShippedJson {
        body,
        input_tokens,
        output_tokens,
        drafted_by: drafter.model_name.clone(),
    }
}

// ---------------------------------------------------------------------------
// streaming
// ---------------------------------------------------------------------------

/// State handed from the pre-commit phase to the committed driver.
struct Committed {
    reader: DraftReader,
    /// Deltas verified by the passing score — released first by the driver.
    release_upto: usize,
    /// Delta count at the last verification.
    last_check: usize,
}

/// Draft and verify until the first gate PASS (commit), a gate abort, defer
/// patience running out, or an error — everything except commit abstains.
async fn precommit_stream(
    req: &SpecRequest<'_>,
    plan: &SpeculationPlan,
    gate: Gate,
    drafter: &Arc<ResolvedModel>,
    verifier: &mut Verifier,
    client_max_tokens: Option<u64>,
) -> Result<Committed, &'static str> {
    let s = &plan.settings;
    let body = draft_body(
        &plan.messages,
        &drafter.upstream_model,
        s,
        client_max_tokens,
        true,
    );
    // The scoring prefix is tokenized while the drafter starts; a failure
    // here is retried by the first score.
    let (resp, _) = tokio::join!(
        dispatch_stream(req.state, drafter, &body, req.dispatch_timeout),
        verifier.prefix_ids(&req.state.http, &plan.messages),
    );
    let resp = match resp {
        Ok(r) => r,
        Err(e) => {
            tracing::info!(error = %e, "speculation draft dispatch failed");
            return Err("draft dispatch failed");
        }
    };
    let mut reader = DraftReader::new(resp);
    let mut last_check = 0usize;
    loop {
        if !reader.done {
            match reader.advance().await {
                Ok(_) => {}
                Err(e) => {
                    tracing::info!(error = %e, "speculation draft stream failed");
                    return Err("draft stream failed");
                }
            }
        }
        let chunk = if last_check == 0 {
            s.first_chunk_tokens as usize
        } else {
            s.chunk_tokens as usize
        };
        let boundary = reader.deltas.len() - last_check >= chunk.max(1);
        let finished = reader.done;
        if !boundary && !finished {
            continue;
        }
        if finished && reader.deltas.len() == last_check && last_check > 0 {
            // Nothing new since the last (deferring) score and the draft is
            // done below the gate: it will not recover.
            return Err("gate fail at end of draft");
        }
        if finished {
            let text: String = reader.deltas.concat();
            if text.trim().is_empty() || reader.finish.as_deref() != Some("stop") {
                return Err("draft incomplete");
            }
        }
        last_check = reader.deltas.len();
        let text: String = reader.deltas.concat();
        let score = match verifier.score(req.state, &plan.messages, &text).await {
            Ok(sc) => sc,
            Err(e) => {
                tracing::info!(error = %e, "speculation verify failed pre-release");
                return Err("verify failed");
            }
        };
        match zone(&gate, score.agree, score.mean_lp) {
            Zone::Pass => {
                tracing::info!(
                    agree = score.agree,
                    mean_lp = score.mean_lp,
                    n = score.n_draft,
                    "speculation committed (first verified span)"
                );
                return Ok(Committed {
                    release_upto: last_check,
                    last_check,
                    reader,
                });
            }
            Zone::Abort => {
                tracing::info!(
                    agree = score.agree,
                    mean_lp = score.mean_lp,
                    n = score.n_draft,
                    "speculation chunk abort pre-release"
                );
                return Err("gate abort");
            }
            Zone::Defer => {
                if finished || score.n_draft as u32 >= s.decide_by_tokens {
                    tracing::info!(
                        agree = score.agree,
                        mean_lp = score.mean_lp,
                        n = score.n_draft,
                        "speculation defer patience out pre-release"
                    );
                    return Err("defer patience out");
                }
            }
        }
    }
}

/// Most verified spans waiting for the stream to release them; past this the
/// verification task waits, and so stops reading the draft.
const VERIFIED_SPANS_BUFFERED: usize = 4;

/// What the verification task sends the stream.
enum Verified<S> {
    /// The next verified deltas, in order.
    Span(Vec<String>),
    /// How the draft ended; nothing follows.
    End(Box<Ended<S>>),
}

/// How a committed draft ended, with what the stream needs to finish.
struct Ended<S> {
    /// The complete draft passed its final verify: ship it, releasing `tail`
    /// after the spans already sent. `None`: escalate.
    ship: Option<Score>,
    tail: Vec<String>,
    /// The drafter's reported usage.
    usage: Option<UpstreamUsage>,
    /// Deltas the drafter wrote, for billing a draft that reported no usage.
    drafted: usize,
    /// When the draft ended (complete, or abandoned at a failed span).
    draft_end_ms: i64,
    scorer: S,
}

/// Where the committed draft stands when the task takes it over.
struct SpanPlan {
    gate: Gate,
    chunk_tokens: usize,
    /// Delta count at the last verification.
    last_check: usize,
    /// Deltas already handed to the stream.
    sent: usize,
}

/// A draft being read: [`DraftReader`], or a stand-in in tests.
trait DraftSource: Send {
    /// Read at least one more event; false once the stream is exhausted.
    fn advance(&mut self) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send;
    fn deltas(&self) -> &[String];
    fn done(&self) -> bool;
    /// The drafter finished its answer (`finish_reason: stop`).
    fn stopped(&self) -> bool;
    fn usage(&self) -> Option<UpstreamUsage>;
}

impl DraftSource for DraftReader {
    fn advance(&mut self) -> impl std::future::Future<Output = anyhow::Result<bool>> + Send {
        DraftReader::advance(self)
    }
    fn deltas(&self) -> &[String] {
        &self.deltas
    }
    fn done(&self) -> bool {
        self.done
    }
    fn stopped(&self) -> bool {
        self.finish.as_deref() == Some("stop")
    }
    fn usage(&self) -> Option<UpstreamUsage> {
        self.usage
    }
}

/// Scores a draft's text: [`Scoring`], or a stand-in in tests.
trait SpanScorer: Send {
    fn score(
        &mut self,
        text: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<Score>> + Send;
}

/// The verifier with what its calls need, owned by the verification task.
struct Scoring {
    verifier: Verifier,
    state: AppState,
    messages: Value,
}

impl SpanScorer for Scoring {
    fn score(
        &mut self,
        text: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<Score>> + Send {
        self.verifier.score(&self.state, &self.messages, text)
    }
}

/// Aborts a spawned task when dropped, so a stream the client abandoned
/// leaves no work running.
struct AbortOnDrop<T>(tokio::task::JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// The committed draft's verification: keep reading the draft, score it at
/// each chunk boundary, and send each span that passes; then verify the
/// complete draft and send how it ended. A span that aborts, a failed read or
/// score, or a final verify that fails ends it with an escalation. The draft
/// stream is dropped before the end is sent, which stops the drafter.
async fn verify_spans<D: DraftSource, S: SpanScorer>(
    mut draft: D,
    mut scorer: S,
    plan: SpanPlan,
    draft_done: Arc<AtomicBool>,
    tx: tokio::sync::mpsc::Sender<Verified<S>>,
) {
    let SpanPlan {
        gate,
        chunk_tokens,
        mut last_check,
        mut sent,
    } = plan;
    let mut gate_dead = false;
    while !draft.done() && !gate_dead {
        if let Err(e) = draft.advance().await {
            tracing::info!(error = %e, "speculation draft stream failed mid-release");
            gate_dead = true;
            break;
        }
        if draft.done() {
            draft_done.store(true, Ordering::Relaxed);
        }
        let drafted = draft.deltas().len();
        let boundary = drafted - last_check >= chunk_tokens.max(1);
        if !boundary && !draft.done() {
            continue;
        }
        if drafted == last_check {
            continue; // done, nothing new since the last score
        }
        last_check = drafted;
        if draft.done() {
            break; // the final verify below handles the complete draft
        }
        let text: String = draft.deltas().concat();
        match scorer.score(&text).await {
            Ok(score) => match zone(&gate, score.agree, score.mean_lp) {
                Zone::Pass => {
                    let span = draft.deltas()[sent..last_check].to_vec();
                    sent = last_check;
                    if tx.send(Verified::Span(span)).await.is_err() {
                        return; // the stream is gone
                    }
                }
                Zone::Abort => {
                    tracing::info!(
                        agree = score.agree,
                        mean_lp = score.mean_lp,
                        n = score.n_draft,
                        "speculation chunk abort mid-stream"
                    );
                    gate_dead = true;
                }
                // Post-release defers just wait for the draft to grow: the
                // final verify decides.
                Zone::Defer => {}
            },
            Err(e) => {
                tracing::info!(error = %e, "speculation verify failed mid-stream");
                gate_dead = true;
            }
        }
    }

    let draft_end_ms = crate::tracer::now_ms();
    // Final verify over the complete draft. A COMPLETE draft above the abort
    // floors with content already released is SHIPPED ("soft ship"): handing
    // the target a finished document to continue because it missed the gate
    // by a hair makes it ramble corrections.
    let mut ship = None;
    if !gate_dead {
        let text: String = draft.deltas().concat();
        if !text.trim().is_empty() && draft.stopped() {
            match scorer.score(&text).await {
                Ok(score) => {
                    let pass = score.agree >= gate.agree_min && score.mean_lp >= gate.lp_min;
                    let soft = sent > 0
                        && score.agree >= gate.abort_agree
                        && score.mean_lp >= gate.abort_lp;
                    if pass || soft {
                        if soft && !pass {
                            tracing::info!(
                                agree = score.agree,
                                mean_lp = score.mean_lp,
                                n = score.n_draft,
                                "speculation soft ship"
                            );
                        }
                        ship = Some(score);
                    } else {
                        tracing::info!(
                            agree = score.agree,
                            mean_lp = score.mean_lp,
                            n = score.n_draft,
                            "speculation final gate fail"
                        );
                    }
                }
                Err(e) => {
                    tracing::info!(error = %e, "speculation final verify failed");
                }
            }
        }
    }
    let ended = Ended {
        tail: if ship.is_some() {
            draft.deltas()[sent..].to_vec()
        } else {
            Vec::new()
        },
        ship,
        usage: draft.usage(),
        drafted: draft.deltas().len(),
        draft_end_ms,
        scorer,
    };
    drop(draft); // disconnect aborts any still-running draft generation
    let _ = tx.send(Verified::End(Box::new(ended))).await;
}

/// The committed streaming driver: release verified spans paced, keep
/// verifying the growing draft, soft-ship or escalate mid-stream at the end.
/// Leaves the cascade's trace spans in `stats` when it ends.
#[allow(clippy::too_many_arguments)]
fn drive_stream(
    req: SpecRequest<'_>,
    plan: SpeculationPlan,
    gate: Gate,
    drafter: Arc<ResolvedModel>,
    verifier: Verifier,
    committed: Committed,
    stats: Arc<Mutex<SpecStats>>,
    mut trace: SpecTrace,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    let state = req.state.clone();
    let route = req.route.clone();
    let key = req.key.clone();
    let session_id = req.session_id.to_string();
    let request_id = req.request_id;
    let dispatch_timeout = req.dispatch_timeout;
    let started = req.started;

    Box::pin(async_stream::stream! {
        let s = plan.settings.clone();
        let Committed { reader, last_check, release_upto } = committed;
        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
        let created = crate::tracer::now_ms() / 1000;
        let model_name = route.model_name.clone();
        let mut released = 0usize;
        let mut ttft_set = false;

        // Rebuild a SpecRequest-alike for the billing helpers (the borrow of
        // the proxy's locals did not move into the stream).
        let breq = SpecRequest {
            state: &state,
            route: route.clone(),
            key: &key,
            session_id: &session_id,
            request_id,
            dispatch_timeout,
            started,
        };

        // Role delta first, exactly like a native OpenAI stream.
        yield Ok(Bytes::from(format!(
            "data: {}\n\n",
            json!({
                "id": id, "object": "chat.completion.chunk", "created": created,
                "model": model_name,
                "choices": [{ "index": 0, "delta": { "role": "assistant" }, "finish_reason": null }],
            })
        )));

        // Drafting and verification run in their own task, which sends each
        // verified span here as it passes; this stream only paces them out,
        // so verifying the next span overlaps releasing this one. Dropping
        // the stream (the client went away) aborts the task.
        let draft_done = Arc::new(AtomicBool::new(reader.done));
        // Scorings so far, for the trace should the verification task not
        // come back with the verifier.
        let committed_times = verifier.times;
        let mut pending: std::collections::VecDeque<String> =
            reader.deltas[..release_upto].iter().cloned().collect();
        let (tx, mut rx) = tokio::sync::mpsc::channel(VERIFIED_SPANS_BUFFERED);
        let _worker = AbortOnDrop(tokio::spawn(verify_spans(
            reader,
            Scoring {
                verifier,
                state: state.clone(),
                messages: plan.messages.clone(),
            },
            SpanPlan {
                gate,
                chunk_tokens: s.chunk_tokens as usize,
                last_check,
                sent: release_upto,
            },
            draft_done.clone(),
            tx,
        )));
        let mut released_text = String::new();
        // Paced release — `yield` cannot live inside a nested macro or a
        // helper (async_stream rewrites yields before macro expansion), so
        // the release loop is written out at each site.
        let ended = loop {
            while let Some(d) = pending.pop_front() {
                released += 1;
                if !ttft_set {
                    ttft_set = true;
                    if let Ok(mut st) = stats.lock() {
                        st.ttft_ms = started.elapsed().as_millis() as u32;
                    }
                }
                yield Ok(Bytes::from(content_chunk(&id, &model_name, created, &d)));
                released_text.push_str(&d);
                let pace = if draft_done.load(Ordering::Relaxed) { s.pace_ms / 3 } else { s.pace_ms };
                if pace > 0 {
                    tokio::time::sleep(Duration::from_millis(pace)).await;
                }
            }
            match rx.recv().await {
                Some(Verified::Span(span)) => pending.extend(span),
                Some(Verified::End(ended)) => break Some(*ended),
                None => break None,
            }
        };
        let Some(Ended { ship, tail, usage: draft_usage, drafted, draft_end_ms, scorer }) = ended else {
            // The task ended without an outcome (it panicked): the answer
            // stops short, as a broken-off continuation does.
            tracing::warn!(model = %model_name, "speculation verification ended without an outcome");
            let spans = trace.spans("error", Some("verification ended without an outcome"), Some(&committed_times));
            if let Ok(mut st) = stats.lock() {
                st.usage = UpstreamUsage::uncached(0, released as u32);
                st.final_set = true;
                st.failure = Some(DEFAULT_FAILURE_STATUS);
                st.spans = spans;
            }
            yield Err(stream_cut_error());
            return;
        };
        let verifier = scorer.verifier;
        trace.draft_ended(draft_end_ms);

        // The complete draft passed its final verify (or, with content
        // already released, cleared the abort floors: a "soft ship"): send
        // the rest of it and close.
        if let Some(score) = ship {
            for d in tail {
                yield Ok(Bytes::from(content_chunk(&id, &model_name, created, &d)));
                if s.pace_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(s.pace_ms / 3)).await;
                }
            }
            let input = verifier.prefix_len();
            let output = score.n_draft as u32;
            tracing::info!(
                agree = score.agree, mean_lp = score.mean_lp, n = score.n_draft,
                t_ms = started.elapsed().as_millis() as u64,
                model = %model_name,
                "speculation shipped (stream)"
            );
            bill_draft(&breq, &drafter, draft_usage);
            bill_verify(&breq, &verifier);
            yield Ok(Bytes::from(finish_chunk(&id, &model_name, created, None)));
            let usage = UpstreamUsage::uncached(input, output);
            if plan.include_usage {
                yield Ok(Bytes::from(usage_chunk(&id, &model_name, created, usage)));
            }
            yield Ok(Bytes::from(done()));
            let spans = trace.spans("shipped", None, Some(&verifier.times));
            if let Ok(mut st) = stats.lock() {
                st.usage = usage;
                st.final_set = true;
                st.spans = spans;
            }
            return;
        }

        // --- mid-stream escalation -----------------------------------------
        // Content is already on the wire, so the target model CONTINUES the
        // released verified prefix through its chat template. The template
        // renders the continued message after a CLOSED think block, but
        // streaming reasoning parsers mislabel the continuation as reasoning —
        // remap those deltas to content deterministically. The task already
        // dropped the draft stream, which aborts the drafter's generation.
        bill_draft(
            &breq,
            &drafter,
            draft_usage.or(Some(UpstreamUsage::uncached(0, drafted as u32))),
        );
        bill_verify(&breq, &verifier);
        tracing::info!(
            released_chars = released_text.len(),
            model = %model_name,
            "speculation escalating mid-stream"
        );
        let body = escalation_body(
            &plan.request,
            &plan.messages,
            &route.upstream_model,
            &released_text,
        );
        let mut cont_usage: Option<UpstreamUsage> = None;
        let mut finish: Option<Value> = None;
        // How the continuation ended: `[DONE]`, broken off (its connection
        // broke or went silent, or it never started), or with an error event
        // of its own, forwarded as sent.
        let mut done_seen = false;
        let mut broke_off = false;
        let mut failure: Option<u16> = None;
        let mut error_forwarded = false;
        let mut cont_chars = 0usize;
        let escalate_start = crate::tracer::now_ms();
        match dispatch_stream(&state, &route, &body, dispatch_timeout).await {
            Ok(resp) => {
                use super::tool_stream::TurnEvent;
                let mut reader = super::tool_stream::TurnReader::new(
                    resp,
                    super::tool_stream::stream_idle_timeout(),
                );
                loop {
                    let (v, reported) = match reader.next().await {
                        TurnEvent::Chunk { value, failure } => (value, failure),
                        TurnEvent::Error { data, status, message } => {
                            tracing::warn!(status, %message, "speculation continuation reported an error");
                            failure.get_or_insert(status);
                            yield Ok(Bytes::from(format!("data: {data}\n\n")));
                            error_forwarded = true;
                            break;
                        }
                        TurnEvent::Done => {
                            done_seen = true;
                            break;
                        }
                        TurnEvent::Cut(e) => {
                            tracing::warn!(error = %e, "speculation continuation stream failed");
                            broke_off = true;
                            break;
                        }
                        TurnEvent::End => break,
                    };
                    if let Some(status) = reported {
                        failure.get_or_insert(status);
                    }
                    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                        cont_usage = Some(chunk_usage(u));
                    }
                    let Some(choice) = v.pointer("/choices/0") else { continue };
                    if let Some(f) = choice.get("finish_reason").filter(|f| !f.is_null()) {
                        finish = Some(f.clone());
                    }
                    let delta = choice.get("delta").unwrap_or(&Value::Null);
                    let mut text = String::new();
                    for k in ["reasoning", "reasoning_content", "content"] {
                        if let Some(t) = delta.get(k).and_then(|t| t.as_str()) {
                            text.push_str(t);
                        }
                    }
                    if !text.is_empty() {
                        cont_chars += text.chars().count();
                        yield Ok(Bytes::from(content_chunk(&id, &model_name, created, &text)));
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "speculation continuation dispatch failed");
                broke_off = true;
            }
        }
        let escalated_ok = !broke_off && failure.is_none() && (done_seen || finish.is_some());
        trace.escalate = Some((
            escalate_start,
            crate::tracer::now_ms(),
            if escalated_ok { "ok" } else { "error" },
        ));
        let spans = trace.spans("escalated", None, Some(&verifier.times));
        // Settle with what the target actually processed: its prompt covers
        // the original request plus the released prefix; the client received
        // released draft deltas plus the continuation's completion tokens.
        let cont = cont_usage.unwrap_or_default();
        // The target's own prompt count, with the part its prefix cache
        // served; the scoring prefix's length when it reported none.
        let (input, cached) = if cont.input > 0 {
            (cont.input, cont.cached)
        } else {
            (verifier.prefix_len(), 0)
        };

        // A continuation that stopped short, or closed with neither `[DONE]`
        // nor a finish reason, ends with an error item and no finish chunk or
        // `[DONE]`: the proxy settles the request, then tells the client with
        // an error event and aborts the response, as for every other stream
        // cut short. It is billed for what reached the client; unreported
        // continuation tokens are estimated from its text (~4 characters a
        // token).
        if !error_forwarded && (broke_off || (!done_seen && finish.is_none())) {
            let cont_out = match cont_usage {
                Some(_) => cont.output,
                None => obleth_tokenizer::output_tokens_from_chars(cont_chars),
            };
            if let Ok(mut st) = stats.lock() {
                st.usage =
                    UpstreamUsage::new(input, (released as u32).saturating_add(cont_out), cached);
                st.final_set = true;
                st.failure = Some(failure.unwrap_or(DEFAULT_FAILURE_STATUS));
                st.spans = spans;
            }
            yield Err(stream_cut_error());
            return;
        }

        let usage = UpstreamUsage::new(input, (released as u32).saturating_add(cont.output), cached);
        // The upstream's own error event already ended the answer.
        if !error_forwarded {
            yield Ok(Bytes::from(finish_chunk(&id, &model_name, created, finish)));
            if plan.include_usage {
                yield Ok(Bytes::from(usage_chunk(&id, &model_name, created, usage)));
            }
        }
        yield Ok(Bytes::from(done()));
        if let Ok(mut st) = stats.lock() {
            st.usage = usage;
            st.final_set = true;
            st.failure = failure;
            st.spans = spans;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cascade's trace: the `boon:speculation` span with its draft, verify
    /// and escalate phases under it, and the outcome on the parent.
    #[test]
    fn a_speculation_traces_its_draft_verify_and_escalation() {
        let mut trace = SpecTrace::new();
        trace.drafter = "drafter".into();
        trace.draft_started();
        let start = trace.start_ms;
        trace.draft_ended(start + 40);
        trace.escalate = Some((start + 60, start + 90, "ok"));
        let verify = VerifyTimes {
            scorings: 3,
            first_ms: start + 10,
            last_ms: start + 50,
            busy_ms: 30,
        };
        let spans = trace.clone().spans("escalated", None, Some(&verify));
        let names: Vec<(&str, &str)> = spans
            .iter()
            .map(|s| (s.name.as_str(), s.parent.as_str()))
            .collect();
        assert_eq!(
            names,
            vec![
                ("boon:speculation:draft", "boon:speculation"),
                ("boon:speculation:verify", "boon:speculation"),
                ("boon:speculation:escalate", "boon:speculation"),
                ("boon:speculation", "proxy_request"),
            ]
        );
        assert_eq!(spans[1].attributes["scorings"], 3);
        assert_eq!(spans[1].duration_ms, 40);
        assert_eq!(spans[3].attributes["outcome"], "escalated");
        assert_eq!(spans[3].attributes["drafter"], "drafter");

        // An abstain before any draft is the parent alone, with its reason.
        let spans = SpecTrace::new().spans("abstain", Some("no scoring endpoint"), None);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].name, "boon:speculation");
        assert_eq!(spans[0].attributes["reason"], "no scoring endpoint");
    }

    fn target(name: &str) -> ResolvedModel {
        serde_json::from_value(serde_json::json!({
            "model_name": name,
            "upstream_model": format!("{name}-mxfp4"),
            "api_base": "http://gateway.test/v1",
            "api_key": null,
            "admission_weight": 1,
            "max_in_flight": null,
            "enabled": true,
            "cache_enabled": false,
            "cache_ttl_secs": 0,
        }))
        .expect("minimal resolved model")
    }

    #[test]
    fn scoring_base_prefers_the_model_then_the_fleet_template() {
        let mut route = target("glm-5-3");
        assert_eq!(scoring_base(&route, ""), "");
        let tpl = "http://{upstream}.serving.svc.cluster.local:8000/v1";
        assert_eq!(
            scoring_base(&route, tpl),
            "http://glm-5-3-mxfp4.serving.svc.cluster.local:8000/v1"
        );
        route.verify_api_base = "http://canary:8000/v1".into();
        assert_eq!(scoring_base(&route, tpl), "http://canary:8000/v1");
    }

    #[test]
    fn host_placeholders_take_dns_labels_only() {
        let mut route = target("glm-5-3");
        route.upstream_model = "my-model".into();
        let base = scoring_base(&route, "http://{upstream}.serving.svc/v1");
        let url = reqwest::Url::parse(&base).expect("filled template parses");
        assert_eq!(url.host_str(), Some("my-model.serving.svc"), "{base}");

        // Values that would name an address or smuggle URL structure: no
        // scoring route, so speculation is skipped rather than keyed there.
        for upstream in [
            "x@169.254.169.254",
            "169.254.169.254",
            "2852039166",
            "a/b",
            "a:1",
            "-bad",
            "",
        ] {
            route.upstream_model = upstream.into();
            assert_eq!(
                scoring_base(&route, "http://{upstream}/v1"),
                "",
                "{upstream}"
            );
            assert!(
                scoring_route(&route, "http://{upstream}/v1").is_none(),
                "{upstream}"
            );
        }
    }

    #[test]
    fn template_values_cannot_move_the_scoring_host() {
        let mut route = target("glm-5-3");
        route.upstream_model = "x@169.254.169.254".into();
        route.model_name = "a/b?c#d:e".into();
        let base = scoring_base(&route, "http://10.0.0.5:8000/{upstream}/{model}/v1");
        let url = reqwest::Url::parse(&base).expect("filled template parses");
        assert_eq!(url.host_str(), Some("10.0.0.5"), "{base}");
        assert_eq!(url.port(), Some(8000));
        assert_eq!(url.username(), "");
        assert_eq!(
            url.path(),
            "/x%40169.254.169.254/a%2Fb%3Fc%23d%3Ae/v1",
            "{base}"
        );
        assert!(url.query().is_none() && url.fragment().is_none());
    }

    #[test]
    fn scheme_userinfo_and_port_placeholders_yield_no_scoring_endpoint() {
        let route = target("glm-5-3");
        for tpl in [
            "http://{model}@10.0.0.5/v1",
            "http://user:{model}@10.0.0.5/v1",
            "{upstream}://10.0.0.5/v1",
            "http://10.0.0.5:{model}/v1",
        ] {
            assert_eq!(scoring_base(&route, tpl), "", "{tpl}");
            assert!(scoring_route(&route, tpl).is_none(), "{tpl}");
        }
    }

    #[test]
    fn drafter_and_scorer_come_from_the_target_model() {
        let mut route = target("glm-5-3");
        let mut s = SpeculationBoonSettings {
            draft_model: Some("fleet-default-drafter".into()),
            ..Default::default()
        };

        // Drafter: the model's own choice wins; fleet default is the fallback.
        assert_eq!(effective_draft_model(&route, &s), "fleet-default-drafter");
        route.draft_model = "north-mini-code".into();
        assert_eq!(effective_draft_model(&route, &s), "north-mini-code");
        route.draft_model = "  ".into();
        s.draft_model = None;
        assert_eq!(effective_draft_model(&route, &s), "");

        // Scorer: none configured = cannot speculate.
        assert!(scoring_route(&route, "").is_none());

        // A canary endpoint serving its own name: URL and served name swap,
        // identity (name, rates) stays the target's.
        route.verify_api_base = "http://canary.test:8000/v1".into();
        route.verify_upstream_model = "glm-5-3-verify-canary".into();
        let v = scoring_route(&route, "").expect("scoring endpoint set");
        assert_eq!(v.model_name, "glm-5-3");
        assert_eq!(v.api_base, "http://canary.test:8000/v1");
        assert_eq!(v.upstream_model, "glm-5-3-verify-canary");
        assert!(v.endpoints.is_empty());

        // Once the target's own pods are patched, only the URL is set and the
        // scorer serves the target's own upstream name.
        route.verify_upstream_model = String::new();
        let v = scoring_route(&route, "").expect("scoring endpoint set");
        assert_eq!(v.upstream_model, "glm-5-3-mxfp4");
    }

    /// A scorer that accepts the call and never answers: after a draft is
    /// committed this is what would leave the client's stream silent, so the
    /// verifier's own bound ends the call, as a typed timeout, within it.
    #[tokio::test]
    async fn a_stalled_scorer_is_cut_off_by_the_verifiers_own_bound() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hold = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(socket);
        });
        let mut route = target("glm-5-3");
        route.verify_api_base = format!("http://{addr}/v1");
        let scorer = scoring_route(&route, "").expect("scoring endpoint set");
        let s = SpeculationBoonSettings {
            verify_timeout_ms: 150,
            ..settings()
        };
        let verifier = Verifier::new(scorer, &s);
        let started = Instant::now();
        let err = verifier
            .post_json(
                &reqwest::Client::new(),
                format!("{}/tokenize", verifier.root),
                json!({"prompt": "hi"}),
            )
            .await
            .unwrap_err();
        assert!(crate::boons::HelperTimeout::is(&err), "{err:#}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{:?}",
            started.elapsed()
        );
        hold.abort();
    }

    fn settings() -> SpeculationBoonSettings {
        SpeculationBoonSettings {
            enabled: true,
            draft_model: Some("drafter".into()),
            ..Default::default()
        }
    }

    /// A drafter writing one delta per millisecond.
    struct FakeDraft {
        deltas: Vec<String>,
        total: usize,
        dropped: Arc<AtomicBool>,
    }

    impl DraftSource for FakeDraft {
        async fn advance(&mut self) -> anyhow::Result<bool> {
            tokio::time::sleep(Duration::from_millis(1)).await;
            if self.deltas.len() < self.total {
                self.deltas.push(format!("t{} ", self.deltas.len()));
            }
            Ok(true)
        }
        fn deltas(&self) -> &[String] {
            &self.deltas
        }
        fn done(&self) -> bool {
            self.deltas.len() >= self.total
        }
        fn stopped(&self) -> bool {
            true
        }
        fn usage(&self) -> Option<UpstreamUsage> {
            Some(UpstreamUsage::uncached(10, self.deltas.len() as u32))
        }
    }

    impl Drop for FakeDraft {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::SeqCst);
        }
    }

    /// A scorer that takes `ms` per call and answers `(agree, mean_lp)` for
    /// each call in turn, then the last one.
    struct FakeScorer {
        ms: u64,
        answers: Vec<(f64, f64)>,
        calls: usize,
    }

    impl SpanScorer for FakeScorer {
        fn score(
            &mut self,
            text: &str,
        ) -> impl std::future::Future<Output = anyhow::Result<Score>> + Send {
            let (agree, mean_lp) = self.answers[self.calls.min(self.answers.len() - 1)];
            self.calls += 1;
            let n_draft = text.split_whitespace().count();
            let ms = self.ms;
            async move {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                Ok(Score {
                    agree,
                    mean_lp,
                    n_draft,
                })
            }
        }
    }

    /// Run the verification task over a committed draft of `total` deltas
    /// (the first 50 already verified) and release what it sends at `pace`
    /// ms a delta, as the stream does. Returns the released deltas, the
    /// longest gap between two releases, and how it ended.
    async fn pipeline(
        total: usize,
        answers: Vec<(f64, f64)>,
        pace: u64,
    ) -> (
        Vec<String>,
        Duration,
        Option<Ended<FakeScorer>>,
        Arc<AtomicBool>,
    ) {
        let dropped = Arc::new(AtomicBool::new(false));
        let draft = FakeDraft {
            deltas: (0..50).map(|i| format!("t{i} ")).collect(),
            total,
            dropped: dropped.clone(),
        };
        let mut pending: std::collections::VecDeque<String> =
            draft.deltas.iter().cloned().collect();
        let draft_done = Arc::new(AtomicBool::new(false));
        let (tx, mut rx) = tokio::sync::mpsc::channel(VERIFIED_SPANS_BUFFERED);
        let _worker = AbortOnDrop(tokio::spawn(verify_spans(
            draft,
            FakeScorer {
                ms: 300,
                answers,
                calls: 0,
            },
            SpanPlan {
                gate: Gate::global(&settings()),
                chunk_tokens: 50,
                last_check: 50,
                sent: 50,
            },
            draft_done.clone(),
            tx,
        )));
        let (mut released, mut gap, mut last) =
            (Vec::new(), Duration::ZERO, tokio::time::Instant::now());
        let ended = loop {
            while let Some(d) = pending.pop_front() {
                gap = gap.max(last.elapsed());
                last = tokio::time::Instant::now();
                released.push(d);
                let pace = if draft_done.load(Ordering::Relaxed) {
                    pace / 3
                } else {
                    pace
                };
                tokio::time::sleep(Duration::from_millis(pace)).await;
            }
            match rx.recv().await {
                Some(Verified::Span(span)) => pending.extend(span),
                Some(Verified::End(ended)) => break Some(*ended),
                None => break None,
            }
        };
        (released, gap, ended, dropped)
    }

    #[tokio::test(start_paused = true)]
    async fn verification_of_the_next_span_overlaps_releasing_this_one() {
        // 300 ms a score, 9 ms a delta: one span of 50 takes 450 ms to send,
        // so the next is verified before the stream needs it.
        let (released, gap, ended, dropped) = pipeline(200, vec![(0.9, -0.2)], 9).await;
        let ended = ended.expect("an outcome");
        assert!(ended.ship.is_some(), "a passing draft ships");
        assert_eq!(released.len(), 150, "every verified span went out");
        assert_eq!(ended.tail.len(), 50);
        assert_eq!(ended.drafted, 200);
        // Scored at 100 and 150 deltas, then the complete draft.
        assert_eq!(ended.scorer.calls, 3);
        assert!(
            gap < Duration::from_millis(50),
            "a release waited on verification: {gap:?}"
        );
        assert!(dropped.load(Ordering::SeqCst), "the draft stream is closed");
    }

    #[tokio::test(start_paused = true)]
    async fn an_abort_escalates_after_the_spans_that_passed() {
        // The first span after the commit passes, the second aborts.
        let (released, _, ended, _) = pipeline(400, vec![(0.9, -0.2), (0.1, -5.0)], 9).await;
        let ended = ended.expect("an outcome");
        assert!(ended.ship.is_none(), "an abort escalates");
        assert!(ended.tail.is_empty());
        assert_eq!(
            released.len(),
            100,
            "the committed span and the one that passed"
        );
        assert_eq!(ended.scorer.calls, 2);
        // A complete draft below the ship gate but above the abort floors
        // ships softly, as content is already out.
        let gate = Gate::global(&settings());
        let soft = ((gate.agree_min + gate.abort_agree) / 2.0, gate.lp_min);
        let (_, _, ended, _) = pipeline(60, vec![soft], 9).await;
        assert!(ended.expect("an outcome").ship.is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn dropping_the_stream_stops_the_verification_task() {
        let dropped = Arc::new(AtomicBool::new(false));
        let (tx, rx) = tokio::sync::mpsc::channel(VERIFIED_SPANS_BUFFERED);
        let worker = AbortOnDrop(tokio::spawn(verify_spans(
            FakeDraft {
                deltas: vec!["t ".into(); 50],
                total: 100_000,
                dropped: dropped.clone(),
            },
            FakeScorer {
                ms: 300,
                answers: vec![(0.9, -0.2)],
                calls: 0,
            },
            SpanPlan {
                gate: Gate::global(&settings()),
                chunk_tokens: 50,
                last_check: 50,
                sent: 50,
            },
            Arc::new(AtomicBool::new(false)),
            tx,
        )));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!dropped.load(Ordering::SeqCst), "still drafting");
        drop(rx);
        drop(worker);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(
            dropped.load(Ordering::SeqCst),
            "the task ended and closed the draft"
        );
    }

    #[test]
    fn zone_is_three_valued() {
        let g = Gate::global(&settings());
        assert_eq!(zone(&g, 0.9, -0.2), Zone::Pass);
        assert_eq!(zone(&g, 0.5, -1.0), Zone::Pass, "gate floors are inclusive");
        assert_eq!(
            zone(&g, 0.44, -0.2),
            Zone::Abort,
            "agreement below the abort floor"
        );
        assert_eq!(
            zone(&g, 0.9, -1.7),
            Zone::Abort,
            "logprob below the abort floor"
        );
        assert_eq!(
            zone(&g, 0.47, -1.2),
            Zone::Defer,
            "between the floors defers"
        );
    }

    #[test]
    fn resolve_gate_prefers_the_category_and_respects_exclusions() {
        let mut s = settings();
        s.category_gates = vec![
            obleth_config::SpeculationCategoryGate {
                tag: "infra".into(),
                speculate: false,
                agree_min: 0.5,
                lp_min: -1.0,
            },
            obleth_config::SpeculationCategoryGate {
                tag: "coding".into(),
                speculate: true,
                agree_min: 0.4,
                lp_min: -0.8,
            },
        ];
        // Category override applies.
        let g = resolve_gate(&s, &["coding".into()]).expect("coding speculates");
        assert_eq!(g.agree_min, 0.4);
        assert_eq!(g.lp_min, -0.8);
        // Excluded category abstains before drafting.
        assert!(resolve_gate(&s, &["infra".into()]).is_none());
        // First listed gate wins on multi-tag intents.
        assert!(resolve_gate(&s, &["coding".into(), "infra".into()]).is_none());
        // Unlisted falls back to the global gate by default...
        let g = resolve_gate(&s, &["math".into()]).expect("unlisted speculates by default");
        assert_eq!(g.agree_min, s.agree_min);
        // ...and abstains when the default is off.
        s.unlisted_categories_speculate = false;
        assert!(resolve_gate(&s, &["math".into()]).is_none());
        assert!(resolve_gate(&s, &[]).is_none());
    }

    /// Score a `prompt_logprobs` array as the verifier reads it off the wire.
    fn parse_score(plp: &Value, start: usize, draft_ids: &[i64]) -> Result<Score, usize> {
        let reply = serde_json::to_vec(&json!({
            "id": "cmpl-1",
            "choices": [{ "index": 0, "text": "x", "prompt_logprobs": plp }, { "index": 1 }],
            "usage": { "prompt_tokens": 4 },
        }))
        .unwrap();
        let picks = read_picks(&reply, start, draft_ids)
            .expect("a well-formed reply")
            .expect("prompt_logprobs present");
        score_picks(&picks)
    }

    /// The scoring the gateway did before the reply was read without a tree,
    /// kept to pin that both decide the same.
    fn reference_score(plp: &Value, start: usize, draft_ids: &[i64]) -> Result<Score, usize> {
        let plp = plp.as_array();
        let (mut agree, mut lp_sum, mut lp_n, mut misses) = (0usize, 0.0f64, 0usize, 0usize);
        for (k, tid) in draft_ids.iter().enumerate() {
            let Some(info) = plp
                .and_then(|a| a.get(start + k))
                .and_then(|e| e.get(tid.to_string()))
            else {
                misses += 1;
                continue;
            };
            if info.get("rank").and_then(|r| r.as_u64()) == Some(1) {
                agree += 1;
            }
            if let Some(lp) = info.get("logprob").and_then(|l| l.as_f64()) {
                lp_sum += lp;
                lp_n += 1;
            }
        }
        if misses > draft_ids.len() / 10 {
            return Err(misses);
        }
        Ok(Score {
            agree: agree as f64 / draft_ids.len().max(1) as f64,
            mean_lp: if lp_n > 0 {
                lp_sum / lp_n as f64
            } else {
                -99.0
            },
            n_draft: draft_ids.len(),
        })
    }

    #[test]
    fn the_streamed_reader_decides_as_the_tree_did_on_recorded_shapes() {
        // vLLM's shape: a null first entry, then per position the sent
        // token plus the top-1 when they differ, with decoded_token.
        let entry = |id: i64, lp: f64, rank: u64, top: Option<i64>| {
            let mut m = serde_json::Map::new();
            m.insert(
                id.to_string(),
                json!({ "logprob": lp, "rank": rank, "decoded_token": "t" }),
            );
            if let Some(top) = top {
                m.insert(
                    top.to_string(),
                    json!({ "logprob": -0.01, "rank": 1, "decoded_token": "u" }),
                );
            }
            Value::Object(m)
        };
        let ids: Vec<i64> = (100..140).collect();
        let mut clean = vec![
            Value::Null,
            entry(1, -1.0, 1, None),
            entry(2, -1.0, 1, None),
        ];
        for (k, id) in ids.iter().enumerate() {
            let agrees = k % 3 != 0;
            clean.push(entry(
                *id,
                -(k as f64) / 10.0,
                if agrees { 1 } else { 2 },
                (!agrees).then_some(9),
            ));
        }
        let clean = Value::Array(clean);
        // Corrupt keys at a few positions (under and over the 10% line).
        let corrupt = |n: usize| {
            let mut v = clean.clone();
            for k in 0..n {
                v[3 + k * 3] = entry(896_757_760 + k as i64, -0.1, 1, None);
            }
            v
        };
        // Short (the reply ends early), a missing rank, a `null` mid-draft.
        let mut odd = clean.clone();
        odd.as_array_mut().unwrap().truncate(30);
        odd[5] = json!({ "103": { "logprob": -0.5 } });
        odd[6] = Value::Null;
        for (name, plp, start) in [
            ("clean", clean.clone(), 3),
            ("corrupt under", corrupt(4), 3),
            ("corrupt over", corrupt(5), 3),
            ("odd", odd, 3),
            ("start past the end", clean.clone(), 60),
            ("no logprobs at all", Value::Array(vec![]), 0),
        ] {
            let (new, old) = (
                parse_score(&plp, start, &ids),
                reference_score(&plp, start, &ids),
            );
            match (&new, &old) {
                (Ok(a), Ok(b)) => {
                    assert_eq!(a.n_draft, b.n_draft, "{name}");
                    assert!((a.agree - b.agree).abs() < 1e-12, "{name}");
                    assert!((a.mean_lp - b.mean_lp).abs() < 1e-12, "{name}");
                }
                _ => assert_eq!(new, old, "{name}"),
            }
        }
    }

    /// The closer is a constant string: a second request to the same model
    /// tokenizes only its own prompt.
    #[tokio::test]
    async fn the_think_closer_is_tokenized_once_per_model() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new((AtomicUsize::new(0), AtomicUsize::new(0)));
        let seen = calls.clone();
        let app = axum::Router::new().route(
            "/tokenize",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let seen = seen.clone();
                async move {
                    if body.get("messages").is_some() {
                        seen.0.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({ "tokens": [1, 2, 3], "token_strs": ["a", "b", "c"] }))
                    } else {
                        seen.1.fetch_add(1, Ordering::SeqCst);
                        axum::Json(json!({ "tokens": [7, 8] }))
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await });
        let mut route = target("closer-once");
        route.verify_api_base = format!("http://{addr}/v1");
        let http = reqwest::Client::new();
        let messages = json!([{ "role": "user", "content": "hi" }]);
        for _ in 0..3 {
            let scorer = scoring_route(&route, "").expect("scoring endpoint set");
            let mut verifier = Verifier::new(scorer, &settings());
            verifier.prefix_ids(&http, &messages).await.unwrap();
            assert_eq!(verifier.prompt, vec![1, 2, 3, 7, 8]);
            assert_eq!(verifier.prefix_len(), 5);
        }
        assert_eq!(
            calls.0.load(Ordering::SeqCst),
            3,
            "each request's own prompt"
        );
        assert_eq!(calls.1.load(Ordering::SeqCst), 1, "the closer once");
        server.abort();
    }

    #[test]
    fn a_reply_without_prompt_logprobs_reads_as_none() {
        for reply in [
            json!({ "choices": [{ "index": 0, "text": "x" }] }),
            json!({ "choices": [{ "prompt_logprobs": null }] }),
            json!({ "choices": [] }),
            json!({ "error": { "message": "no" } }),
        ] {
            let bytes = serde_json::to_vec(&reply).unwrap();
            assert_eq!(read_picks(&bytes, 0, &[1]).unwrap(), None, "{reply}");
        }
        assert!(read_picks(b"{\"choices\": [", 0, &[1]).is_err());
    }

    #[test]
    fn parse_score_counts_rank_one_agreement_and_mean_logprob() {
        // prefix len 2, draft ids [10, 11]; token 10 agrees (rank 1), 11 does not.
        let plp = serde_json::json!([
            null, null,
            { "10": { "logprob": -0.1, "rank": 1 } },
            { "11": { "logprob": -2.0, "rank": 3 } },
        ]);
        let score = parse_score(&plp, 2, &[10, 11]).expect("clean keys");
        assert_eq!(score.n_draft, 2);
        assert!((score.agree - 0.5).abs() < 1e-9);
        assert!((score.mean_lp - (-1.05)).abs() < 1e-9);
    }

    #[test]
    fn parse_score_detects_cold_bucket_corrupt_keys() {
        // Keys do not match the sent token ids (the HPU cold-bucket
        // signature): every entry is a miss and the caller must retry.
        let plp = serde_json::json!([
            { "896757760": { "logprob": -0.1, "rank": 1 } },
            { "1386629632": { "logprob": -0.2, "rank": 1 } },
        ]);
        assert_eq!(parse_score(&plp, 0, &[10, 11]), Err(2));
    }

    #[test]
    fn think_closer_closes_an_open_block_and_splices_an_empty_one() {
        let open = vec!["<".to_string(), "think".to_string(), ">".to_string()];
        assert_eq!(think_closer(&open), "</think>\n");
        let closed = vec!["assistant".to_string(), "\n".to_string()];
        assert_eq!(think_closer(&closed), "<think></think>\n");
    }

    #[test]
    fn backend_root_strips_the_v1_suffix_only() {
        assert_eq!(backend_root("http://canary:8000/v1"), "http://canary:8000");
        assert_eq!(backend_root("http://canary:8000/v1/"), "http://canary:8000");
        assert_eq!(backend_root("http://canary:8000"), "http://canary:8000");
    }

    #[test]
    fn eligible_messages_flattens_text_parts_and_rejects_everything_else() {
        let ok = serde_json::json!({
            "messages": [
                { "role": "user", "content": [ { "type": "text", "text": "a" }, { "type": "text", "text": "b" } ] },
            ],
        });
        let flat = eligible_messages(&ok).expect("text parts flatten");
        assert_eq!(flat[0]["content"], "ab");

        let image = serde_json::json!({
            "messages": [ { "role": "user", "content": [ { "type": "image_url", "image_url": {"url": "x"} } ] } ],
        });
        assert!(eligible_messages(&image).is_none(), "images are ineligible");

        let logprobs = serde_json::json!({
            "messages": [ { "role": "user", "content": "hi" } ],
            "logprobs": true,
        });
        assert!(eligible_messages(&logprobs).is_none(), "logprobs bypass");

        let multi = serde_json::json!({
            "messages": [ { "role": "user", "content": "hi" } ],
            "n": 2,
        });
        assert!(eligible_messages(&multi).is_none(), "n>1 bypass");
    }

    #[test]
    fn escalation_body_continues_the_released_prefix_and_strips_internals() {
        let request = serde_json::json!({
            "model": "glm-5-3",
            "messages": [ { "role": "user", "content": "q" } ],
            "temperature": 0.7,
            "logprobs": true,
            "chat_template_kwargs": { "reasoning": false },
        });
        let messages = serde_json::json!([ { "role": "user", "content": "q" } ]);
        let body = escalation_body(&request, &messages, "glm-5-3-mxfp4", "released text");
        assert_eq!(body["model"], "glm-5-3-mxfp4");
        assert_eq!(body["stream"], true);
        assert_eq!(body["continue_final_message"], true);
        assert_eq!(body["add_generation_prompt"], false);
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"][1]["content"], "released text");
        assert!(body.get("logprobs").is_none(), "logprobs must be stripped");
        assert!(
            body.get("chat_template_kwargs").is_none(),
            "draft-only kwargs must not reach the target"
        );
        assert_eq!(body["temperature"], 0.7, "client sampling params survive");
    }
}
