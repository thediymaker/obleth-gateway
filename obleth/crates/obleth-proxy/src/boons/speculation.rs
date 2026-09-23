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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::Stream;
use obleth_config::{ResolvedKey, ResolvedModel, SpeculationBoonSettings};
use serde_json::{json, Value};

use super::tool_stream::{
    content_chunk, done, finish_chunk, parse_data_lines, split_event, usage_chunk,
};
use crate::state::AppState;

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
    /// The image-generation boon armed the tool loop for this request, so the
    /// target could answer it by drawing something. The cascade cannot: the
    /// drafter is a different model and the scoring endpoint only scores, so a
    /// committed cascade means no image at all.
    ///
    /// When this is set, an unclassified request abstains instead of
    /// speculating — the cost of being wrong is a missing picture, not a slow
    /// answer, so silence from the classifier has to mean "let the target
    /// have it". See the guard in [`run`].
    pub image_tool_pending: bool,
}

/// Token/latency stats the streaming driver reports back for settlement,
/// mirroring `tool_stream::StreamStats`.
#[derive(Default)]
pub struct SpecStats {
    pub ttft_ms: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub final_set: bool,
}

/// What [`run`] decided.
pub enum Outcome {
    /// Non-streaming ship: a complete, verified draft as a chat completion.
    ShippedJson {
        body: Value,
        input_tokens: u32,
        output_tokens: u32,
    },
    /// Streaming commit: at least one verified span exists; the driver owns
    /// the response from here (release, further verification, escalation).
    Stream(Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>>),
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

/// Parse one `prompt_logprobs` response against the draft ids that were sent.
/// Returns `Err(misses)` on the cold-bucket corrupt-keys signature (>10% of
/// entries missing the sent token id as their key).
pub(crate) fn parse_score(
    prompt_logprobs: &Value,
    start: usize,
    draft_ids: &[i64],
) -> Result<Score, usize> {
    let plp = prompt_logprobs.as_array();
    let mut agree = 0usize;
    let mut lp_sum = 0.0f64;
    let mut lp_n = 0usize;
    let mut misses = 0usize;
    for (k, tid) in draft_ids.iter().enumerate() {
        let info = plp
            .and_then(|a| a.get(start + k))
            .and_then(|e| e.get(tid.to_string()));
        let Some(info) = info else {
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

/// The verifier client: tokenize + prompt_logprobs scoring against the
/// registered verify model's backend.
struct Verifier {
    model: Arc<ResolvedModel>,
    root: String,
    /// Templated prompt ids ending in a closed empty think block, computed
    /// once per request.
    prefix: Option<Vec<i64>>,
    /// Prefill tokens consumed across all scorings, for helper billing.
    billed_input: u32,
    scorings: u32,
}

impl Verifier {
    fn new(model: Arc<ResolvedModel>) -> Self {
        let root = backend_root(&model.api_base);
        Verifier {
            model,
            root,
            prefix: None,
            billed_input: 0,
            scorings: 0,
        }
    }

    async fn post_json(&self, state: &AppState, url: String, body: Value) -> anyhow::Result<Value> {
        let mut req = state.http.post(url).json(&body);
        if let Some(api_key) = &self.model.api_key {
            req = req.bearer_auth(api_key);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("verify backend returned {}", resp.status());
        }
        Ok(resp.json::<Value>().await?)
    }

    async fn tokenize(&self, state: &AppState, mut body: Value) -> anyhow::Result<Value> {
        if let Some(o) = body.as_object_mut() {
            o.insert(
                "model".into(),
                Value::String(self.model.upstream_model.clone()),
            );
        }
        self.post_json(state, format!("{}/tokenize", self.root), body)
            .await
    }

    fn token_ids(v: &Value) -> Vec<i64> {
        v.get("tokens")
            .and_then(|t| t.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default()
    }

    /// Templated prompt ids for scoring, ending in a CLOSED empty think block.
    async fn prefix_ids(&mut self, state: &AppState, messages: &Value) -> anyhow::Result<()> {
        if self.prefix.is_some() {
            return Ok(());
        }
        let pre = self
            .tokenize(
                state,
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
        let clo = self
            .tokenize(
                state,
                json!({ "prompt": closer, "add_special_tokens": false }),
            )
            .await?;
        let mut ids = Self::token_ids(&pre);
        if ids.is_empty() {
            anyhow::bail!("verify tokenize returned no tokens");
        }
        ids.extend(Self::token_ids(&clo));
        self.prefix = Some(ids);
        Ok(())
    }

    /// Score `draft_text` with one prefill over prefix+draft. Retries once on
    /// the cold-bucket corrupt-keys signature.
    async fn score(
        &mut self,
        state: &AppState,
        messages: &Value,
        draft_text: &str,
    ) -> anyhow::Result<Score> {
        self.prefix_ids(state, messages).await?;
        let prefix = self.prefix.as_ref().expect("prefix computed above");
        let dtok = self
            .tokenize(
                state,
                json!({ "prompt": draft_text, "add_special_tokens": false }),
            )
            .await?;
        let draft_ids = Self::token_ids(&dtok);
        if draft_ids.is_empty() {
            anyhow::bail!("draft tokenized to nothing");
        }
        let mut prompt: Vec<i64> = prefix.clone();
        let start = prompt.len();
        prompt.extend(&draft_ids);
        let total = prompt.len() as u32;
        for attempt in 0..2u8 {
            let resp = self
                .post_json(
                    state,
                    format!("{}/v1/completions", self.root),
                    json!({
                        "model": self.model.upstream_model,
                        "prompt": prompt,
                        "max_tokens": 1,
                        "temperature": 0.0,
                        "prompt_logprobs": 0,
                    }),
                )
                .await?;
            self.billed_input = self.billed_input.saturating_add(total);
            self.scorings += 1;
            let plp = resp
                .pointer("/choices/0/prompt_logprobs")
                .cloned()
                .unwrap_or(Value::Null);
            if plp.is_null() {
                anyhow::bail!("verify backend returned no prompt_logprobs");
            }
            match parse_score(&plp, start, &draft_ids) {
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
        self.prefix.as_ref().map(|p| p.len() as u32).unwrap_or(0)
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
        .json(body);
    if let Some(api_key) = &target.api_key {
        req = req.bearer_auth(api_key);
    }
    let resp = tokio::time::timeout(timeout, req.send())
        .await
        .map_err(|_| anyhow::anyhow!("speculation dispatch timed out"))??;
    if !resp.status().is_success() {
        anyhow::bail!("upstream returned {}", resp.status());
    }
    Ok(resp)
}

/// Incremental reader over a drafter SSE stream: accumulates content deltas
/// and captures finish_reason/usage.
struct DraftReader {
    bytes: Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    buf: Vec<u8>,
    pub deltas: Vec<String>,
    pub finish: Option<String>,
    pub usage: Option<(u32, u32)>,
    pub done: bool,
}

impl DraftReader {
    fn new(resp: reqwest::Response) -> Self {
        DraftReader {
            bytes: Box::pin(resp.bytes_stream()),
            buf: Vec::new(),
            deltas: Vec::new(),
            finish: None,
            usage: None,
            done: false,
        }
    }

    fn consume_data(&mut self, data: &str) {
        if data == "[DONE]" {
            self.done = true;
            return;
        }
        let Ok(v) = serde_json::from_str::<Value>(data) else {
            return;
        };
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            let it = u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
            let ot = u
                .get("completion_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0) as u32;
            self.usage = Some((it, ot));
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
        loop {
            if let Some(event) = split_event(&mut self.buf) {
                for data in parse_data_lines(&event) {
                    self.consume_data(&data);
                }
                return Ok(true);
            }
            match super::tool_stream::next_within(
                &mut self.bytes,
                super::tool_stream::stream_idle_timeout(),
            )
            .await
            {
                Some(Ok(chunk)) => self.buf.extend_from_slice(&chunk),
                Some(Err(e)) => anyhow::bail!("draft stream read failed: {e}"),
                None => {
                    self.done = true;
                    return Ok(false);
                }
            }
        }
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
    pub dispatch_timeout: Duration,
    pub started: Instant,
}

/// Run the cascade up to its commit point. See the module docs for the
/// contract; every failure before a verified span is released returns
/// [`Outcome::Abstain`].
pub async fn run(
    req: SpecRequest<'_>,
    plan: SpeculationPlan,
    stats: Arc<Mutex<SpecStats>>,
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
    // With the image tool on the table, only a positive classification may
    // claim the request. No tags means the classifier was unavailable, or had
    // nothing to say, and guessing wrong here costs the user the picture they
    // asked for.
    if plan.image_tool_pending && tags.is_empty() {
        tracing::info!(
            target = %req.route.model_name,
            "image tool is armed and the request is unclassified; target model answers directly"
        );
        return Outcome::Abstain("image tool pending, intent unclassified");
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
    let mut verifier = Verifier::new(verifier_model);

    if plan.client_stream {
        return match tokio::time::timeout(
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
        .await
        {
            Ok(Ok(committed)) => Outcome::Stream(drive_stream(
                req, plan, gate, drafter, verifier, committed, stats,
            )),
            Ok(Err(reason)) => abstain(&req, &verifier, reason),
            Err(_) => abstain(&req, &verifier, "pre-release budget exhausted"),
        };
    }
    match tokio::time::timeout(
        budget,
        run_nonstream(
            &req,
            &plan,
            gate,
            &drafter,
            &mut verifier,
            client_max_tokens,
        ),
    )
    .await
    {
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
            "speculation_verify",
            verifier.billed_input,
            verifier.scorings,
        );
    }
}

fn bill_draft(req: &SpecRequest<'_>, drafter: &ResolvedModel, usage: Option<(u32, u32)>) {
    let (it, ot) = usage.unwrap_or((0, 0));
    if it > 0 || ot > 0 {
        super::bill_helper_call(
            req.state,
            drafter,
            req.key,
            req.session_id,
            "speculation_draft",
            it,
            ot,
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
) -> Outcome {
    let s = &plan.settings;
    let body = draft_body(
        &plan.messages,
        &drafter.upstream_model,
        s,
        client_max_tokens,
        false,
    );
    let completion =
        match super::chat_call_completion(req.state, drafter, body, req.dispatch_timeout).await {
            Ok(c) => c,
            Err(e) => {
                tracing::info!(error = %e, "speculation draft failed");
                return Outcome::Abstain("draft failed");
            }
        };
    let usage = completion.get("usage").map(|u| {
        (
            u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
            u.get("completion_tokens")
                .and_then(|x| x.as_u64())
                .unwrap_or(0) as u32,
        )
    });
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
        "created": super::now_ms() / 1000,
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
    let resp = match dispatch_stream(req.state, drafter, &body, req.dispatch_timeout).await {
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

/// The committed streaming driver: release verified spans paced, keep
/// verifying the growing draft, soft-ship or escalate mid-stream at the end.
fn drive_stream(
    req: SpecRequest<'_>,
    plan: SpeculationPlan,
    gate: Gate,
    drafter: Arc<ResolvedModel>,
    mut verifier: Verifier,
    committed: Committed,
    stats: Arc<Mutex<SpecStats>>,
) -> Pin<Box<dyn Stream<Item = Result<Bytes, std::io::Error>> + Send>> {
    let state = req.state.clone();
    let route = req.route.clone();
    let key = req.key.clone();
    let session_id = req.session_id.to_string();
    let dispatch_timeout = req.dispatch_timeout;
    let started = req.started;

    Box::pin(async_stream::stream! {
        let s = plan.settings.clone();
        let Committed { mut reader, mut last_check, release_upto } = committed;
        let id = format!("chatcmpl-{}", uuid::Uuid::new_v4().simple());
        let created = super::now_ms() / 1000;
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

        // Paced release of buffered deltas — `yield` cannot live inside a
        // nested macro (async_stream rewrites yields before macro expansion),
        // so the release loop is inlined at each site.
        while released < release_upto {
            let d = reader.deltas[released].clone();
            released += 1;
            if !ttft_set {
                ttft_set = true;
                if let Ok(mut st) = stats.lock() {
                    st.ttft_ms = started.elapsed().as_millis() as u32;
                }
            }
            yield Ok(Bytes::from(content_chunk(&id, &model_name, created, &d)));
            let pace = if reader.done { s.pace_ms / 3 } else { s.pace_ms };
            if pace > 0 {
                tokio::time::sleep(Duration::from_millis(pace)).await;
            }
        }

        // Continue drafting + verifying until the draft completes or dies.
        let mut gate_dead = false;
        while !reader.done && !gate_dead {
            match reader.advance().await {
                Ok(_) => {}
                Err(e) => {
                    tracing::info!(error = %e, "speculation draft stream failed mid-release");
                    gate_dead = true;
                    break;
                }
            }
            let boundary = reader.deltas.len() - last_check >= (s.chunk_tokens as usize).max(1);
            if !boundary && !reader.done {
                continue;
            }
            if reader.deltas.len() == last_check {
                continue; // done, nothing new since the last score
            }
            last_check = reader.deltas.len();
            if reader.done {
                break; // the final verify below handles the complete draft
            }
            let text: String = reader.deltas.concat();
            match verifier.score(&state, &plan.messages, &text).await {
                Ok(score) => match zone(&gate, score.agree, score.mean_lp) {
                    Zone::Pass => {
                        while released < last_check {
                            let d = reader.deltas[released].clone();
                            released += 1;
                            yield Ok(Bytes::from(content_chunk(&id, &model_name, created, &d)));
                            let pace = if reader.done { s.pace_ms / 3 } else { s.pace_ms };
                            if pace > 0 {
                                tokio::time::sleep(Duration::from_millis(pace)).await;
                            }
                        }
                    }
                    Zone::Abort => {
                        tracing::info!(
                            agree = score.agree, mean_lp = score.mean_lp, n = score.n_draft,
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

        // Final verify over the complete draft. A COMPLETE draft above the
        // abort floors with content already released is SHIPPED ("soft ship"):
        // handing the target a finished document to continue because it missed
        // the gate by a hair makes it ramble corrections.
        if !gate_dead {
            let text: String = reader.deltas.concat();
            if !text.trim().is_empty() && reader.finish.as_deref() == Some("stop") {
                match verifier.score(&state, &plan.messages, &text).await {
                    Ok(score) => {
                        let pass = score.agree >= gate.agree_min && score.mean_lp >= gate.lp_min;
                        let soft = released > 0
                            && score.agree >= gate.abort_agree
                            && score.mean_lp >= gate.abort_lp;
                        if pass || soft {
                            if soft && !pass {
                                tracing::info!(
                                    agree = score.agree, mean_lp = score.mean_lp, n = score.n_draft,
                                    "speculation soft ship"
                                );
                            }
                            while released < reader.deltas.len() {
                                let d = reader.deltas[released].clone();
                                released += 1;
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
                            bill_draft(&breq, &drafter, reader.usage);
                            bill_verify(&breq, &verifier);
                            yield Ok(Bytes::from(finish_chunk(&id, &model_name, created, None)));
                            if plan.include_usage {
                                yield Ok(Bytes::from(usage_chunk(&id, &model_name, created, input, output)));
                            }
                            yield Ok(Bytes::from(done()));
                            if let Ok(mut st) = stats.lock() {
                                st.input_tokens = input;
                                st.output_tokens = output;
                                st.final_set = true;
                            }
                            return;
                        }
                        tracing::info!(
                            agree = score.agree, mean_lp = score.mean_lp, n = score.n_draft,
                            "speculation final gate fail"
                        );
                    }
                    Err(e) => {
                        tracing::info!(error = %e, "speculation final verify failed");
                    }
                }
            }
            gate_dead = true;
        }
        let _ = gate_dead;

        // --- mid-stream escalation -----------------------------------------
        // Content is already on the wire, so the target model CONTINUES the
        // released verified prefix through its chat template. The template
        // renders the continued message after a CLOSED think block, but
        // streaming reasoning parsers mislabel the continuation as reasoning —
        // remap those deltas to content deterministically.
        bill_draft(&breq, &drafter, reader.usage.or(Some((0, reader.deltas.len() as u32))));
        bill_verify(&breq, &verifier);
        let released_text: String = reader.deltas[..released].concat();
        drop(reader); // disconnect aborts any still-running draft generation
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
        let mut cont_usage: Option<(u32, u32)> = None;
        let mut finish: Option<Value> = None;
        match dispatch_stream(&state, &route, &body, dispatch_timeout).await {
            Ok(resp) => {
                let mut bytes = Box::pin(resp.bytes_stream());
                let mut buf: Vec<u8> = Vec::new();
                'relay: loop {
                    while let Some(event) = split_event(&mut buf) {
                        for data in parse_data_lines(&event) {
                            if data == "[DONE]" {
                                break 'relay;
                            }
                            let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
                            if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                                cont_usage = Some((
                                    u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                                    u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                                ));
                            }
                            let Some(choice) = v.pointer("/choices/0") else { continue };
                            if let Some(f) = choice.get("finish_reason").filter(|f| !f.is_null()) {
                                finish = Some(f.clone());
                            }
                            let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
                            let mut text = String::new();
                            for k in ["reasoning", "reasoning_content", "content"] {
                                if let Some(t) = delta.get(k).and_then(|t| t.as_str()) {
                                    text.push_str(t);
                                }
                            }
                            if !text.is_empty() {
                                yield Ok(Bytes::from(content_chunk(&id, &model_name, created, &text)));
                            }
                        }
                    }
                    match super::tool_stream::next_within(&mut bytes, super::tool_stream::stream_idle_timeout()).await {
                        Some(Ok(chunk)) => buf.extend_from_slice(&chunk),
                        Some(Err(e)) => {
                            tracing::warn!(error = %e, "speculation continuation stream failed");
                            yield Ok(Bytes::from(content_chunk(
                                &id, &model_name, created,
                                "\n\n[the answer above may be incomplete: the continuation stream failed]\n",
                            )));
                            break 'relay;
                        }
                        None => break 'relay,
                    }
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "speculation continuation dispatch failed");
                yield Ok(Bytes::from(content_chunk(
                    &id, &model_name, created,
                    "\n\n[the answer above may be incomplete: the continuation failed to start]\n",
                )));
            }
        }
        // Settle with what the target actually processed: its prompt covers
        // the original request plus the released prefix; the client received
        // released draft deltas plus the continuation's completion tokens.
        let (cin, cout) = cont_usage.unwrap_or((0, 0));
        let input = if cin > 0 { cin } else { verifier.prefix_len() };
        let output = (released as u32).saturating_add(cout);
        yield Ok(Bytes::from(finish_chunk(&id, &model_name, created, finish)));
        if plan.include_usage {
            yield Ok(Bytes::from(usage_chunk(&id, &model_name, created, input, output)));
        }
        yield Ok(Bytes::from(done()));
        if let Ok(mut st) = stats.lock() {
            st.input_tokens = input;
            st.output_tokens = output;
            st.final_set = true;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn settings() -> SpeculationBoonSettings {
        SpeculationBoonSettings {
            enabled: true,
            draft_model: Some("drafter".into()),
            verify_model: Some("verifier".into()),
            ..Default::default()
        }
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
