//! `POST /v1/verdicts` — native typed-decision evaluation.
//!
//! One state (plus optional images), N independent typed questions (boolean /
//! choice / score), answered
//! by the route's own chat backend as N concurrent single-token calls whose
//! first-token `top_logprobs` become probability distributions. No external
//! API, no new model: any registered `chat` route (vLLM, SGLang, llama.cpp,
//! or a SaaS backend) serves it.
//!
//! The endpoint participates in the full pipeline: key auth, model resolution
//! (aliases and `auto`), ONE fairshare permit covering the whole fan-out, the
//! key- and tenant-level budget reserves, and a single guarded settlement with
//! the summed real usage under `request_type: "verdict"`. Per-question
//! visibility is in the trace (`verdict:q:<id>` spans), not extra ledger
//! rows. The prompt-prefix / label mechanics live in [`prompt`], the
//! distribution math in [`answers`], and the wire types in [`types`].

pub(crate) mod answers;
pub(crate) mod prompt;
pub(crate) mod types;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, Response, StatusCode};
use obleth_config::Admission;
use obleth_tokenizer::{CostEstimate, Tokenizer};
use uuid::Uuid;

use crate::proxy::{
    self, build_targets, derive_intent, effective_admission_weight, error_json, finalize,
    gate_resolved_key, key_term_period_key, resolve_conversation, resolve_model,
    surfaced_request_type, term_period_key, union_candidate_tags, RequestMeta, BODY_LIMIT,
};
use crate::state::AppState;

pub(crate) const VERDICTS_PATH: &str = "/v1/verdicts";

/// Below this much raw label mass the "answer" is noise, not a decision — a
/// thinking model's scratchpad occasionally leaks a label token at ~1e-4
/// probability, which renormalizes into a confident-looking distribution with
/// a near-zero confidence. Observed live on GLM: a 50/50 `yes`/`no` at
/// confidence 0.00 from exactly this leak. Treated the same as zero mass: the
/// question is retried with the empty-think prefill.
const MIN_IN_SET_MASS: f64 = 0.05;

/// Fixed, short delay before the one bonus retry granted to a transient
/// upstream failure (a 5xx from a sick replica behind a load balancer, or a
/// stale pooled socket). Mirrors `CONN_RETRY_BACKOFF` in the passthrough
/// pipeline.
const TRANSIENT_RETRY_BACKOFF: Duration = Duration::from_millis(50);

/// A two-letter code's first letter needs at least this much bare-letter mass
/// on the first call before its second letter is read with a continuation
/// call. Below it the letter's codes keep only what single-token codes gave.
const CONTINUE_MIN_MASS: f64 = 0.01;

/// Models observed to need the empty-think prefill (see
/// [`prompt::build_body_prefilled`]): their plain call's first token is
/// reasoning prose, never an answer label. Learned at request time from a
/// successful prefill retry, keyed by the gateway model name, so later
/// requests skip the doomed plain attempt. In-process only — worst case after
/// a restart is one wasted plain call per model; bounded by the registry.
static THINK_PREFILL: std::sync::LazyLock<std::sync::RwLock<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(std::collections::HashSet::new()));

fn needs_think_prefill(model: &str) -> bool {
    THINK_PREFILL
        .read()
        .map(|s| s.contains(model))
        .unwrap_or(false)
}

fn remember_think_prefill(model: &str) {
    if let Ok(mut s) = THINK_PREFILL.write() {
        s.insert(model.to_string());
    }
}

/// Models observed to need the harmony completions splice (see
/// [`prompt::build_body_spliced`]): their reasoning lives in harmony channels
/// (gpt-oss), so the plain chat call's first token is the `<|channel|>`
/// control token — deterministically, with all the mass — and the chat-side
/// think prefill is ignored by the backend's harmony renderer. Learned and
/// bounded exactly like [`THINK_PREFILL`].
static HARMONY_SPLICE: std::sync::LazyLock<std::sync::RwLock<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(|| std::sync::RwLock::new(std::collections::HashSet::new()));

fn needs_harmony_splice(model: &str) -> bool {
    HARMONY_SPLICE
        .read()
        .map(|s| s.contains(model))
        .unwrap_or(false)
}

fn remember_harmony_splice(model: &str) {
    if let Ok(mut s) = HARMONY_SPLICE.write() {
        s.insert(model.to_string());
    }
}

/// The `/v1/completions` URL next to a `/v1/chat/completions` URL — where the
/// harmony splice is sent. A URL that doesn't end in the chat path is left
/// alone (only reachable from tests).
fn completions_url(chat_url: &str) -> String {
    match chat_url.strip_suffix("/chat/completions") {
        Some(base) => format!("{base}/completions"),
        None => chat_url.to_string(),
    }
}

/// What every question of one request shares: the upstream model name, the
/// rendered state, and the images. Behind an `Arc`, so a request carrying
/// photos holds one copy of them however many questions it asks.
pub(crate) struct SharedPrompt {
    pub upstream_model: String,
    pub system: String,
    pub images: Vec<String>,
}

/// One prepared upstream call: the question id, its answer labels (for the
/// zero-label-mass retry decision), and the texts its bodies are built from.
pub(crate) struct QuestionCall {
    pub id: String,
    pub labels: Vec<String>,
    pub boolean: bool,
    /// The labels are two-letter codes (a choice past 26 options).
    pub two_letter: bool,
    pub shared: Arc<SharedPrompt>,
    pub user: String,
}

/// Which upstream body an attempt sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Variant {
    /// [`prompt::build_body`].
    Plain,
    /// [`prompt::build_body_prefilled`].
    Prefill,
    /// [`prompt::build_body_spliced`], sent to `/v1/completions`.
    Splice,
    /// [`prompt::build_body_continued`] after this first letter, without
    /// (`false`) or with (`true`) the empty think block.
    Continue(char, bool),
    /// [`prompt::build_body_spliced_continued`] after this first letter.
    SpliceContinue(char),
}

impl Variant {
    /// Sent to `/v1/completions` and answered in its logprobs shape.
    fn is_splice(self) -> bool {
        matches!(self, Variant::Splice | Variant::SpliceContinue(_))
    }
}

impl QuestionCall {
    /// Bodies are built per attempt and dropped once serialized. Held for the
    /// whole fan-out, they would keep a copy of every image per question and
    /// per variant.
    fn body(&self, variant: Variant) -> serde_json::Value {
        let p = &self.shared;
        match variant {
            Variant::Plain => {
                prompt::build_body(&p.upstream_model, &p.system, &self.user, &p.images)
            }
            Variant::Prefill => {
                prompt::build_body_prefilled(&p.upstream_model, &p.system, &self.user, &p.images)
            }
            Variant::Splice => prompt::build_body_spliced(&p.upstream_model, &p.system, &self.user),
            Variant::Continue(first, think) => prompt::build_body_continued(
                &p.upstream_model,
                &p.system,
                &self.user,
                &p.images,
                think,
                first,
            ),
            Variant::SpliceContinue(first) => prompt::build_body_spliced_continued(
                &p.upstream_model,
                &p.system,
                &self.user,
                first,
            ),
        }
    }

    /// The harmony splice is a hand-rendered text prompt: it cannot carry
    /// images, so a question with images never takes it.
    fn can_splice(&self) -> bool {
        self.shared.images.is_empty()
    }
}

/// The outcome of one question's upstream call: parsed first-token
/// distribution and real usage, or the reason it failed. Timing is captured
/// inside the future so spans can be recorded after the join.
pub(crate) struct QuestionOutcome {
    pub id: String,
    pub result: Result<QuestionSuccess, String>,
    pub start_ms: i64,
    pub duration_ms: u32,
}

#[derive(Debug)]
pub(crate) struct QuestionSuccess {
    pub entries: Vec<answers::TopLogprob>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    /// The answer came from the empty-think prefill variant.
    pub used_prefill: bool,
    /// The answer came from the harmony `/v1/completions` splice variant.
    pub used_splice: bool,
}

/// Fan the prepared question calls out concurrently against ONE upstream URL.
///
/// A single target for every question is deliberate: the questions share a
/// byte-identical prompt prefix, and scattering them across replicas would
/// re-prefill the state on each one instead of hitting the prefix cache.
///
/// Reasoning models get one adaptive retry: when the plain call succeeds but
/// none of its top tokens is an answer label, the failing attempt's own top
/// token says which envelope the scratchpad lives in. A harmony control token
/// (`<|channel|>` — gpt-oss) sends the question to `/v1/completions` with a
/// hand-rendered harmony prompt ending in the open final channel; anything
/// else (think-style prose) retries with the empty-think prefill. A success
/// teaches [`HARMONY_SPLICE`] or [`THINK_PREFILL`] respectively, so later
/// requests on this `model` skip the doomed plain attempt. Usage of both
/// attempts is billed.
pub(crate) async fn fan_out(
    http: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    headers: &reqwest::header::HeaderMap,
    model: &str,
    calls: Vec<QuestionCall>,
    timeout: Duration,
) -> Vec<QuestionOutcome> {
    let splice_known = needs_harmony_splice(model);
    let prefill_known = needs_think_prefill(model);
    let splice_url = completions_url(url);
    let splice_url = splice_url.as_str();
    let futures = calls.into_iter().map(|call| async move {
        let start_ms = crate::tracer::now_ms();
        let started = Instant::now();
        // A two-letter code split by the vocabulary starts with a bare first
        // letter: that is answer mass too, read out by the continuation below.
        let in_set_of = |s: &QuestionSuccess| {
            let direct = answers::label_masses(&s.entries, &call.labels, call.boolean).1;
            if call.two_letter {
                direct
                    + answers::first_letter_masses(&s.entries, &call.labels)
                        .iter()
                        .map(|(_, m)| m)
                        .sum::<f64>()
            } else {
                direct
            }
        };
        let splice_first = splice_known && call.can_splice();
        let prefill_first = !splice_first && prefill_known;
        let (first_url, first_variant) = if splice_first {
            (splice_url, Variant::Splice)
        } else if prefill_first {
            (url, Variant::Prefill)
        } else {
            (url, Variant::Plain)
        };
        let mut result = one_call(
            http,
            first_url,
            api_key,
            headers,
            &call,
            first_variant,
            timeout,
        )
        .await;
        if let Ok(success) = &mut result {
            success.used_prefill = prefill_first;
            success.used_splice = splice_first;
        }
        if !prefill_first && !splice_first {
            // Sub-threshold label mass on a *successful* plain call is the
            // reasoning-model signature: the first token is scratchpad prose
            // or a channel opener (possibly leaking a label at ~1e-4). Try
            // once more past the scratchpad before settling.
            let no_label = matches!(&result, Ok(s) if in_set_of(s) < MIN_IN_SET_MASS);
            if no_label {
                let plain = result.expect("checked Ok above");
                let harmony = answers::top_entry_is_control_token(&plain.entries);
                let retry = if harmony {
                    call.can_splice().then_some((splice_url, Variant::Splice))
                } else {
                    Some((url, Variant::Prefill))
                };
                result = match retry {
                    // A harmony model asked about images: there is no splice
                    // to retry with, so the plain result (and its "no
                    // recognizable answer label" error) stands.
                    None => Ok(plain),
                    Some((retry_url, variant)) => {
                        match one_call(http, retry_url, api_key, headers, &call, variant, timeout)
                            .await
                        {
                            Ok(mut retried) => {
                                let retried_mass = in_set_of(&retried);
                                if retried_mass >= MIN_IN_SET_MASS {
                                    if harmony {
                                        remember_harmony_splice(model);
                                    } else {
                                        remember_think_prefill(model);
                                    }
                                }
                                // Keep whichever attempt actually carried
                                // answer mass; bill both either way — both
                                // calls really ran.
                                if retried_mass > in_set_of(&plain) {
                                    retried.input_tokens =
                                        retried.input_tokens.saturating_add(plain.input_tokens);
                                    retried.output_tokens =
                                        retried.output_tokens.saturating_add(plain.output_tokens);
                                    retried.used_prefill = !harmony;
                                    retried.used_splice = harmony;
                                    Ok(retried)
                                } else {
                                    let mut plain = plain;
                                    plain.input_tokens =
                                        plain.input_tokens.saturating_add(retried.input_tokens);
                                    plain.output_tokens =
                                        plain.output_tokens.saturating_add(retried.output_tokens);
                                    Ok(plain)
                                }
                            }
                            // Keep the plain result: its label mass (or the
                            // clearer "no recognizable answer label" error)
                            // stands, and its usage is still billed.
                            Err(_) => Ok(plain),
                        }
                    }
                };
            }
        }
        // Two-letter codes on a vocabulary that splits them: the call above
        // ended on a first letter, so read each likely first letter's second
        // letter with one more call, in the same envelope the answer came
        // from. Codes the vocabulary holds whole need none of this.
        if call.two_letter {
            if let Ok(success) = &mut result {
                let pending: Vec<(char, f64)> =
                    answers::first_letter_masses(&success.entries, &call.labels)
                        .into_iter()
                        .filter(|(_, m)| *m >= CONTINUE_MIN_MASS)
                        .collect();
                let (cont_url, think, splice) = (
                    if success.used_splice { splice_url } else { url },
                    success.used_prefill,
                    success.used_splice,
                );
                let continued =
                    futures_util::future::join_all(pending.iter().map(|&(first, _)| {
                        let variant = if splice {
                            Variant::SpliceContinue(first)
                        } else {
                            Variant::Continue(first, think)
                        };
                        one_call(http, cont_url, api_key, headers, &call, variant, timeout)
                    }))
                    .await;
                for ((first, mass), cont) in pending.into_iter().zip(continued) {
                    // A failed continuation leaves that letter's codes with
                    // only their single-token mass, which lowers confidence.
                    if let Ok(cont) = cont {
                        success.entries.extend(answers::continuation_entries(
                            first,
                            mass,
                            &cont.entries,
                            &call.labels,
                        ));
                        success.input_tokens =
                            success.input_tokens.saturating_add(cont.input_tokens);
                        success.output_tokens =
                            success.output_tokens.saturating_add(cont.output_tokens);
                    }
                }
            }
        }
        QuestionOutcome {
            id: call.id,
            result,
            start_ms,
            duration_ms: started.elapsed().as_millis() as u32,
        }
    });
    futures_util::future::join_all(futures).await
}

/// One upstream call, with one bonus retry on a transient failure (5xx or a
/// connection error). A sick replica behind a load balancer answering one of
/// N concurrent single-token calls with a 500 would otherwise fail the whole
/// request — observed live against a fleet with crash-looping pods.
async fn one_call(
    http: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    headers: &reqwest::header::HeaderMap,
    call: &QuestionCall,
    variant: Variant,
    timeout: Duration,
) -> Result<QuestionSuccess, String> {
    let first = one_attempt(http, url, api_key, headers, call, variant, timeout).await;
    // "backend returned …" covers a 200 with missing or empty logprobs —
    // observed live from a crash-looping replica behind a load balancer that
    // answers with degenerate bodies. A backend that genuinely lacks logprobs
    // support pays one extra doomed call on a path that already fails.
    let transient = matches!(&first,
        Err(e) if e.contains("upstream returned 5")
            || e.contains("unreachable")
            || e.contains("invalid JSON")
            || e.contains("backend returned"));
    if transient {
        tokio::time::sleep(TRANSIENT_RETRY_BACKOFF).await;
        if let Ok(success) = one_attempt(http, url, api_key, headers, call, variant, timeout).await
        {
            return Ok(success);
        }
        // Fall through to the first attempt's error: it names the original
        // failure rather than whatever the retry hit.
    }
    first
}

async fn one_attempt(
    http: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    headers: &reqwest::header::HeaderMap,
    call: &QuestionCall,
    variant: Variant,
    timeout: Duration,
) -> Result<QuestionSuccess, String> {
    let fut = async {
        let body = call.body(variant);
        let mut req = http.post(url).headers(headers.clone()).json(&body);
        // Serialized into the request; the value and its image copies go now.
        drop(body);
        if let Some(key) = api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("upstream unreachable: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            // The backend's own message says why (a 400 for too many images,
            // a context overflow); the status alone does not.
            let detail = upstream_error_detail(&resp.text().await.unwrap_or_default());
            return Err(match detail {
                Some(detail) => format!("upstream returned {status}: {detail}"),
                None => format!("upstream returned {status}"),
            });
        }
        let completion: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("upstream returned invalid JSON: {e}"))?;
        let entries = if variant.is_splice() {
            answers::parse_completions_top_logprobs(&completion)?
        } else {
            answers::parse_top_logprobs(&completion)?
        };
        let input_tokens = completion
            .pointer("/usage/prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let output_tokens = completion
            .pointer("/usage/completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        Ok(QuestionSuccess {
            entries,
            input_tokens,
            output_tokens,
            used_prefill: false,
            used_splice: false,
        })
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(result) => result,
        Err(_) => Err(format!("upstream call timed out after {timeout:?}")),
    }
}

/// The `error.message` (or a top-level `message`) of an upstream error body,
/// trimmed to one short line. `None` when the body carries neither.
fn upstream_error_detail(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let message = json
        .pointer("/error/message")
        .or_else(|| json.get("message"))
        .or_else(|| json.get("error"))
        .and_then(|m| m.as_str())?;
    let mut line: String = message.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.len() > 300 {
        let mut end = 300;
        while !line.is_char_boundary(end) {
            end -= 1;
        }
        line.truncate(end);
        line.push('…');
    }
    (!line.is_empty()).then_some(line)
}

/// The heuristic reserve for the fan-out. Each question really does prefill
/// the state (and the images) again on a cold cache, so the honest estimate is
/// per-question (state + images + question + message overhead); prefix-cache
/// hits show up as the reserve reconciling down at settlement.
fn estimate(
    tokenizer: &dyn Tokenizer,
    system: &str,
    users: &[String],
    images: usize,
) -> CostEstimate {
    let image_tokens = (images as u32).saturating_mul(proxy::IMAGE_PART_TOKEN_ESTIMATE as u32);
    let system_tokens = tokenizer.count_text(system).saturating_add(image_tokens);
    let input_tokens = users
        .iter()
        .map(|u| {
            tokenizer
                .count_text(u)
                .saturating_add(system_tokens)
                .saturating_add(8)
        })
        .fold(0u32, u32::saturating_add);
    CostEstimate {
        input_tokens,
        estimated_output_tokens: users.len() as u32,
    }
}

/// The model a request is for: the one it names, else the operator's verdicts
/// default, else `auto`.
fn requested_model(named: &str, default: Option<&str>) -> String {
    [Some(named), default]
        .into_iter()
        .flatten()
        .map(str::trim)
        .find(|m| !m.is_empty())
        .unwrap_or(crate::router::AUTO_MODEL_NAME)
        .to_string()
}

/// The route for a named model, and the family it was called through. A
/// family is answered by its `none` member: a verdict is read off the first
/// answer token, so the member that answers without thinking first is the one
/// that can give it. The error is a family whose member is not registered.
async fn resolve_named(
    state: &AppState,
    name: &str,
) -> Result<(Option<Arc<obleth_config::ResolvedModel>>, Option<String>), String> {
    if let Some(route) = resolve_model(state, name).await {
        return Ok((Some(route), None));
    }
    let Some(family) = state.families.get(name) else {
        return Ok((None, None));
    };
    let (_, member) = family.member_for(Some(obleth_config::ReasoningLevel::None));
    match resolve_model(state, member).await {
        Some(route) => Ok((Some(route), Some(family.name.clone()))),
        None => Err(format!(
            "model family '{name}' sends `none` to '{member}', which is not registered"
        )),
    }
}

pub async fn handler(state: State<AppState>, req: Request<Body>) -> Response<Body> {
    let request_id = Uuid::new_v4();
    let mut resp = handler_inner(state, req, request_id).await;
    if !resp.headers().contains_key("x-obleth-request-id") {
        if let Ok(value) = header::HeaderValue::from_str(&request_id.to_string()) {
            resp.headers_mut().insert("x-obleth-request-id", value);
        }
    }
    resp
}

async fn handler_inner(
    State(state): State<AppState>,
    req: Request<Body>,
    request_id: Uuid,
) -> Response<Body> {
    let request_start = Instant::now();
    let proxy_start_ms = crate::tracer::now_ms();
    let (parts, body) = req.into_parts();
    let headers = parts.headers;

    // ---- auth (same gates as the passthrough pipeline) ----
    let Some(secret) = proxy::bearer(&headers) else {
        return error_json(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    let auth_start = crate::tracer::now_ms();
    let (resolved, device_id, auth_kind) =
        match crate::jwt_auth::authenticate_credential(&state, &secret).await {
            Ok(cred) => (cred.resolved, cred.device_id, cred.auth_kind),
            Err(resp) => return resp,
        };
    let auth_duration = (crate::tracer::now_ms() - auth_start) as u32;
    if let Err(resp) = gate_resolved_key(&state, &resolved) {
        return resp;
    }

    let mut tracer: Option<crate::tracer::SpanRecorder> = if resolved.tracing_enabled {
        Some(crate::tracer::SpanRecorder::new(
            request_id,
            proxy_start_ms,
            state.telemetry.clone(),
        ))
    } else {
        None
    };
    if let Some(ref mut t) = tracer {
        t.record(
            "auth_resolve",
            "proxy_request",
            auth_start,
            auth_duration,
            "ok",
            serde_json::json!({
                "tenant": resolved.tenant_name,
                "tenant_id": resolved.tenant_id.to_string(),
                "auth": auth_kind,
            }),
        );
    }

    // ---- read + parse + validate body ----
    let body_bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => return error_json(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };
    let json: serde_json::Value = match serde_json::from_slice(&body_bytes) {
        Ok(v) => v,
        Err(_) => return error_json(StatusCode::BAD_REQUEST, "invalid JSON body"),
    };
    let request: types::VerdictsRequest = match serde_json::from_value(json.clone()) {
        Ok(r) => r,
        Err(e) => return error_json(StatusCode::BAD_REQUEST, &format!("invalid request: {e}")),
    };
    if let Err(msg) = types::validate(&request) {
        return error_json(StatusCode::BAD_REQUEST, &msg);
    }

    // ---- prompt prep (shared prefix + per-question bodies) ----
    let mut system = prompt::render_system(&request.state);
    if system.len() > types::MAX_STATE_BYTES {
        return error_json(
            StatusCode::PAYLOAD_TOO_LARGE,
            &format!(
                "rendered state is too large ({} bytes, maximum {})",
                system.len(),
                types::MAX_STATE_BYTES
            ),
        );
    }
    let label_sets: BTreeMap<&String, prompt::LabelSet> = request
        .questions
        .iter()
        .map(|(id, q)| (id, prompt::labels_for(q)))
        .collect();
    let mut users: Vec<String> = request
        .questions
        .iter()
        .map(|(id, q)| prompt::render_user(q, &label_sets[id]))
        .collect();

    // ---- request-log metadata + cost estimate ----
    let conversation = resolve_conversation(
        &headers,
        &json,
        resolved.tenant_id,
        state.session_id_derivation,
    );
    let end_user = proxy::end_user_for(&resolved, &headers, &json);
    let mut req_meta = RequestMeta {
        session_id: conversation.value,
        session_id_source: conversation.source.as_str(),
        request_type: surfaced_request_type(&resolved, VERDICTS_PATH, &headers),
        device_id,
        end_user: end_user.clone().unwrap_or_default(),
        model_variant: String::new(),
        reasoning_effort: String::new(),
        reasoning_effort_requested: String::new(),
        model_family: String::new(),
        streamed: false,
    };
    if let Some(t) = tracer.as_mut() {
        t.set_conversation(&req_meta.session_id, req_meta.session_id_source);
    }
    // ---- tenant input guardrails ----
    // The state and every question reach the model, so the tenant's input
    // policy applies here exactly as on chat. Scanned as one chat-shaped body
    // (system = rendered state, one user turn per question); a redaction is
    // read back into the prompts before estimating and dispatching.
    let mut scan_body = serde_json::json!({
        "messages": std::iter::once(serde_json::json!({"role": "system", "content": system}))
            .chain(users.iter().map(|u| serde_json::json!({"role": "user", "content": u})))
            .collect::<Vec<_>>()
    });
    match state
        .boons
        .scan_input(
            &state,
            &resolved,
            &req_meta.session_id,
            request_id,
            &mut scan_body,
            tracer.as_mut(),
        )
        .await
    {
        Err(block) => {
            if let Some(t) = tracer.take() {
                t.finish("error");
            }
            return error_json(block.status, block.reason);
        }
        Ok(true) => {
            let texts: Vec<String> = scan_body["messages"]
                .as_array()
                .map(|m| {
                    m.iter()
                        .map(|msg| msg["content"].as_str().unwrap_or_default().to_string())
                        .collect()
                })
                .unwrap_or_default();
            // Fail closed: sending the unredacted prompts would bypass the
            // policy the scan just applied.
            if texts.len() != users.len() + 1 {
                if let Some(t) = tracer.take() {
                    t.finish("error");
                }
                return error_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "guardrails redaction could not be applied",
                );
            }
            system = texts[0].clone();
            users = texts[1..].to_vec();
        }
        Ok(false) => {}
    }
    let image_count = request.images.len();
    let est = estimate(&*state.tokenizer, &system, &users, image_count);

    // ---- model resolution (`auto` or a named route) ----
    let mut model = requested_model(
        &request.model,
        state
            .classifier
            .settings()
            .verdicts_default_model
            .as_deref(),
    );
    // The family a named model came through, which the tenant allowlist also
    // accepts (as it does for chat).
    let mut family: Option<String> = None;
    let route = if model == crate::router::AUTO_MODEL_NAME {
        // The router's inputs are chat-shaped; synthesize the state + first
        // question as a pseudo chat body for feature extraction and intent
        // derivation. Candidates are already hard-filtered to chat models.
        // Image parts make vision a hard requirement of the pick.
        let pseudo = serde_json::json!({
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": prompt::user_content(
                    users.first().map(String::as_str).unwrap_or_default(),
                    &request.images,
                )},
            ]
        });
        let features =
            crate::router::RequestFeatures::from_request(&pseudo, est.input_tokens as u64, 1);
        let candidates = state.model_registry.load();
        let busyness = state.fairshare.model_load();
        let expected_output = state.output_stats.snapshot();
        let allowed = if resolved.internal {
            None
        } else {
            resolved.allowed_models.as_deref()
        };
        let router_settings = state.classifier.settings();
        let available_tags = union_candidate_tags(&candidates, allowed);
        let effort_header = headers.get("x-obleth-effort").and_then(|v| v.to_str().ok());
        let auto_start = crate::tracer::now_ms();
        let intent = derive_intent(
            &state,
            &pseudo,
            est.input_tokens as u64,
            &available_tags,
            &router_settings,
            effort_header,
            // Input guardrails already ran on this state and these questions.
            false,
        )
        .await;
        let mut grants = crate::router::BoonGrants::from_settings(&state.boons.settings());
        // Verdicts runs no boons: a model that sees images only through the
        // vision boon's describer cannot answer about them here.
        grants.vision_active = false;
        let weights = crate::router::RouterWeights::from_settings(&router_settings);
        let uniform = if weights.samples() {
            crate::router::splitmix_uniform()
        } else {
            0.0
        };
        let picked = crate::router::select_model(
            &candidates,
            &features,
            &busyness,
            &expected_output,
            allowed,
            &intent.tags,
            grants,
            &weights,
            uniform,
            intent.difficulty,
        );
        match picked {
            Some(chosen) => {
                if let Some(ref mut t) = tracer {
                    t.record(
                        "auto_route",
                        "proxy_request",
                        auto_start,
                        (crate::tracer::now_ms() - auto_start) as u32,
                        "ok",
                        serde_json::json!({ "chosen": chosen.model_name }),
                    );
                }
                model = chosen.model_name.clone();
                Some(Arc::new(chosen))
            }
            None => {
                return error_json(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no model is available to satisfy the auto request",
                );
            }
        }
    } else {
        let route = match resolve_named(&state, &model).await {
            Ok((route, via)) => {
                family = via;
                route
            }
            Err(msg) => return error_json(StatusCode::NOT_FOUND, &msg),
        };
        if let Some(r) = route.as_ref() {
            if r.model_name != model {
                model = r.model_name.clone();
            }
        }
        route
    };

    let Some(route) = route else {
        return error_json(
            StatusCode::NOT_FOUND,
            &format!("model '{model}' is not registered"),
        );
    };
    if !route.enabled {
        return error_json(StatusCode::FORBIDDEN, "model is disabled");
    }
    // A retired model is refused (or stood in for by its replacement) here as
    // on every other endpoint.
    let route = match crate::lifecycle::gate(&state, &route, chrono::Utc::now()).await {
        crate::lifecycle::Gate::Serve(_) => route,
        crate::lifecycle::Gate::Redirect(next, _) => {
            model = next.model_name.clone();
            next
        }
        crate::lifecycle::Gate::Refuse(resp) => return resp,
    };
    // verdicts reads chat-completions logprobs; only chat routes can serve it.
    if route.model_type != "chat" {
        return error_json(
            StatusCode::BAD_REQUEST,
            &format!(
                "verdicts requires a chat model; '{model}' is registered as '{}'",
                route.model_type
            ),
        );
    }
    if !resolved.internal {
        if let Some(allowed) = &resolved.allowed_models {
            if !allowed
                .iter()
                .any(|m| m == &model || family.as_deref() == Some(m.as_str()))
            {
                return error_json(StatusCode::FORBIDDEN, "model not permitted for tenant");
            }
        }
    }
    if let Some(f) = family {
        req_meta.model_family = f;
    }
    // The images go to the model itself (no vision boon here), so it has to
    // accept them.
    if image_count > 0 && !route.supports_vision {
        return error_json(
            StatusCode::BAD_REQUEST,
            &format!("model '{model}' does not accept images; use a model with vision"),
        );
    }

    let (in_cost_rate, out_cost_rate) = (route.input_cost_per_token, route.output_cost_per_token);

    // ---- fairshare admission: ONE permit covers the whole fan-out ----
    // Bounded by the same `OBLETH_ADMISSION_TIMEOUT_SECS` wrapper the
    // chat/completions and responses paths use, so a saturated pool cannot
    // park a verdicts caller indefinitely.
    let effective_weight = effective_admission_weight(resolved.weight, Some(&route));
    let admission_start = crate::tracer::now_ms();
    let admit_wait = proxy::admission_timeout();
    let admit = state.fairshare.admit(crate::proxy::admit_request_for(
        &resolved,
        &model,
        Some(&route),
        effective_weight,
        est.total(),
        end_user.as_deref(),
    ));
    let admitted = match tokio::time::timeout(admit_wait, admit).await {
        Ok(Some(a)) => a,
        Ok(None) => {
            if let Some(t) = tracer.take() {
                t.finish("error");
            }
            state.alerts.issue(
                "scheduler_unavailable",
                "Fairshare scheduler unavailable",
                format!(
                    "tenant `{}` model `{model}` path `{VERDICTS_PATH}`",
                    resolved.tenant_name
                ),
            );
            return error_json(StatusCode::SERVICE_UNAVAILABLE, "scheduler unavailable");
        }
        Err(_) => {
            if let Some(t) = tracer.take() {
                t.finish("error");
            }
            let queued_ms = (crate::tracer::now_ms() - admission_start) as u32;
            finalize(
                &state,
                request_id,
                &resolved,
                &req_meta,
                &model,
                Admission::Rejected,
                est,
                0,
                0,
                0,
                queued_ms,
                0,
                request_start.elapsed().as_millis() as u32,
                503,
                "off",
                0.0,
                crate::energy::EnergyFigures::default(),
            );
            let mut resp = error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "timed out waiting for model capacity",
            );
            resp.headers_mut().insert(
                header::RETRY_AFTER,
                header::HeaderValue::from_static(proxy::ADMISSION_RETRY_AFTER_SECS),
            );
            return resp;
        }
    };
    let admission = admitted.admission;
    let permit = admitted.permit;
    let queue_wait_ms = admitted.waited.as_millis() as u32;
    let request_energy = state.energy.request(Some(&route));
    if let Some(ref mut t) = tracer {
        t.record(
            "admission",
            "proxy_request",
            admission_start,
            (crate::tracer::now_ms() - admission_start) as u32,
            "ok",
            serde_json::json!({
                "decision": admission.as_str(),
                "queue_wait_ms": queue_wait_ms,
                "questions": request.questions.len(),
                "images": image_count,
            }),
        );
    }

    // ---- token budget reserve + term gates ----
    // Same semantics as the passthrough pipeline (see proxy.rs, "token budget
    // reserve + cumulative term gate"): key-level gate first, then the tenant
    // reserve; term exhaustion is 403, rate limiting 429, Redis failure obeys
    // `fail_open`.
    let capacity = resolved.tokens_per_minute.max(0);
    let now = chrono::Utc::now();
    let term_period = term_period_key(&resolved, now);
    let term_gate = term_period
        .as_deref()
        .map(|period_key| obleth_redis::TermGate {
            period_key,
            budget_tokens: resolved.budget_tokens,
            budget_cost_usd: resolved.budget_cost_usd,
        });
    let key_term_period = key_term_period_key(&resolved, now);
    let key_term_gate = key_term_period
        .as_deref()
        .map(|period_key| obleth_redis::TermGate {
            period_key,
            budget_tokens: resolved.key_budget_tokens,
            budget_cost_usd: resolved.key_budget_cost_usd,
        });
    let reject = |status: StatusCode, msg: &str, tracer: Option<crate::tracer::SpanRecorder>| {
        finalize(
            &state,
            request_id,
            &resolved,
            &req_meta,
            &model,
            Admission::Rejected,
            est,
            0,
            0,
            0,
            queue_wait_ms,
            0,
            0,
            status.as_u16(),
            "off",
            0.0,
            crate::energy::EnergyFigures::default(),
        );
        if let Some(t) = tracer {
            t.finish("error");
        }
        error_json(status, msg)
    };
    // Reserve-then-reconcile, as in the passthrough pipeline: an admitted
    // request holds its estimate against each term cap until settlement.
    let est_cost = proxy::estimated_cost(est, in_cost_rate, out_cost_rate, 0.0);
    let mut key_term_held = false;
    let mut tenant_term_held = false;
    let mut key_hold: Option<proxy::PendingTermHold> = None;
    if let Some(gate) = key_term_gate {
        match state
            .redis
            .reserve_with_term(&resolved.key_id, 0, 0, est.total(), Some(gate), est_cost)
            .await
        {
            Ok(obleth_redis::ReserveOutcome::Reserved { .. }) => {
                key_term_held = true;
                key_hold = key_term_period.clone().map(|period| {
                    proxy::PendingTermHold::new(&state, resolved.key_id, period, est, est_cost)
                });
            }
            Ok(obleth_redis::ReserveOutcome::RateLimited { .. }) => {}
            Ok(obleth_redis::ReserveOutcome::TermExhausted {
                used_tokens,
                used_cost,
            }) => {
                drop(permit);
                state.alerts.issue(
                    format!("key_term_budget_exhausted:{}", resolved.key_id),
                    "API key term budget exhausted",
                    format!(
                        "tenant `{}` key `{}` blocked: used {used_tokens} tokens / ${used_cost:.4} against caps tokens={:?} cost={:?}",
                        resolved.tenant_name,
                        resolved.key_id,
                        resolved.key_budget_tokens,
                        resolved.key_budget_cost_usd,
                    ),
                );
                return reject(
                    StatusCode::FORBIDDEN,
                    "api key term budget exhausted",
                    tracer.take(),
                );
            }
            Err(e) => {
                if !state.fail_open {
                    drop(permit);
                    if let Some(t) = tracer.take() {
                        t.finish("error");
                    }
                    return error_json(StatusCode::SERVICE_UNAVAILABLE, "key budget check failed");
                }
                tracing::warn!(error = %e, "key budget reserve failed; failing open");
            }
        }
    }
    let tenant_gate_armed = term_gate.is_some();
    if capacity > 0 || tenant_gate_armed {
        match state
            .redis
            .reserve_with_term(
                &resolved.tenant_id,
                capacity,
                resolved.tokens_per_minute,
                est.total(),
                term_gate,
                est_cost,
            )
            .await
        {
            Ok(obleth_redis::ReserveOutcome::Reserved { .. }) => {
                tenant_term_held = tenant_gate_armed;
            }
            Ok(obleth_redis::ReserveOutcome::RateLimited { .. }) => {
                drop(permit);
                if let Some(hold) = key_hold.take() {
                    hold.release().await;
                }
                return reject(
                    StatusCode::TOO_MANY_REQUESTS,
                    "token budget exceeded",
                    tracer.take(),
                );
            }
            Ok(obleth_redis::ReserveOutcome::TermExhausted {
                used_tokens,
                used_cost,
            }) => {
                drop(permit);
                if let Some(hold) = key_hold.take() {
                    hold.release().await;
                }
                state.alerts.issue(
                    format!("term_budget_exhausted:{}", resolved.tenant_id),
                    "Tenant term budget exhausted",
                    format!(
                        "tenant `{}` blocked: used {used_tokens} tokens / ${used_cost:.4} against caps tokens={:?} cost={:?}",
                        resolved.tenant_name,
                        resolved.budget_tokens,
                        resolved.budget_cost_usd,
                    ),
                );
                return reject(
                    StatusCode::FORBIDDEN,
                    "tenant term budget exhausted",
                    tracer.take(),
                );
            }
            Err(e) => {
                if !state.fail_open {
                    drop(permit);
                    if let Some(hold) = key_hold.take() {
                        hold.release().await;
                    }
                    if let Some(t) = tracer.take() {
                        t.finish("error");
                    }
                    return error_json(StatusCode::SERVICE_UNAVAILABLE, "budget check failed");
                }
                tracing::warn!(error = %e, "budget reserve failed; failing open");
            }
        }
    }
    if let Some(hold) = key_hold.take() {
        hold.hand_off();
    }

    // Every exit from here settles through `settle_guard` exactly once; a
    // client that leaves mid fan-out settles as unbilled 499 and refunds.
    let accounting = proxy::StreamAccounting {
        state: state.clone(),
        request_id,
        resolved: resolved.clone(),
        meta: req_meta.clone(),
        model: model.clone(),
        admission,
        est,
        queue_wait_ms,
        request_start,
        cache_status: "off".to_string(),
        capacity,
        term_period: term_period.clone(),
        key_term_period: key_term_period.clone(),
        in_cost_rate,
        out_cost_rate,
        modality_cost: 0.0,
        energy: request_energy,
        holds: proxy::TermHolds {
            tenant: tenant_term_held,
            key: key_term_held,
            est_cost,
        },
    };
    let settle_guard = accounting.unbilled_guard();

    // ---- fan out against ONE pinned target (prefix-cache locality) ----
    let target = build_targets(
        Some(&route),
        &state.upstream_base,
        &route.endpoint_selection_mode,
        &req_meta.session_id,
    )
    .into_iter()
    .next();
    let Some(target) = target else {
        drop(permit);
        let total_ms = request_start.elapsed().as_millis() as u32;
        let _ = settle_guard
            .complete(accounting.settle_unbilled(0, total_ms, 502))
            .await;
        if let Some(t) = tracer.take() {
            t.finish("error");
        }
        return error_json(
            StatusCode::BAD_GATEWAY,
            "model has no usable upstream endpoint",
        );
    };
    let url = format!("{}/chat/completions", target.base.trim_end_matches('/'));
    let req_timeout = route
        .request_timeout_secs
        .filter(|s| *s >= 1)
        .map(|s| Duration::from_secs(s as u64))
        .unwrap_or(state.upstream_timeout);

    let shared = Arc::new(SharedPrompt {
        upstream_model: route.upstream_model.clone(),
        system,
        images: request.images,
    });
    let calls: Vec<QuestionCall> = request
        .questions
        .iter()
        .zip(users)
        .map(|((id, _), user)| {
            let labels = &label_sets[id];
            QuestionCall {
                id: id.clone(),
                labels: labels.labels.clone(),
                boolean: matches!(labels.semantics, prompt::LabelSemantics::Boolean),
                two_letter: prompt::is_two_letter(&labels.labels),
                shared: shared.clone(),
                user,
            }
        })
        .collect();
    let outcomes = fan_out(
        &state.http,
        &url,
        target.api_key.as_deref(),
        &target.headers,
        &model,
        calls,
        req_timeout,
    )
    .await;
    drop(permit);

    // ---- assemble answers, spans, and usage ----
    let mut verdicts_map: BTreeMap<String, types::Verdict> = BTreeMap::new();
    let mut usage = types::Usage::default();
    let mut ttft_ms = u32::MAX;
    let mut failure: Option<String> = None;
    for outcome in &outcomes {
        ttft_ms = ttft_ms.min(outcome.duration_ms);
        let (status, attrs) = match &outcome.result {
            Ok(success) => {
                usage.prompt_tokens = usage.prompt_tokens.saturating_add(success.input_tokens);
                usage.completion_tokens = usage
                    .completion_tokens
                    .saturating_add(success.output_tokens);
                let question = &request.questions[&outcome.id];
                let labels = &label_sets[&outcome.id];
                let boolean = matches!(labels.semantics, prompt::LabelSemantics::Boolean);
                let (masses, in_set) =
                    answers::label_masses(&success.entries, &labels.labels, boolean);
                match answers::score_masses(&masses, in_set) {
                    Ok(scored) => {
                        let answer = answers::build_answer(question, labels, &scored);
                        let attrs = serde_json::json!({
                            "type": answer.kind,
                            "value": answer.value,
                            "confidence": answer.confidence,
                            "in_set_mass": in_set,
                            "input_tokens": success.input_tokens,
                            "output_tokens": success.output_tokens,
                            "think_prefill": success.used_prefill,
                            "harmony_splice": success.used_splice,
                        });
                        verdicts_map.insert(outcome.id.clone(), answer);
                        ("ok", attrs)
                    }
                    // No label at all, and the caller asked for an answer
                    // anyway: an even distribution at confidence 0.
                    Err(reason) if request.on_no_answer == types::OnNoAnswer::LowConfidence => {
                        let scored = answers::unanswered(labels.labels.len());
                        let mut answer = answers::build_answer(question, labels, &scored);
                        answer.answered = Some(false);
                        let attrs = serde_json::json!({
                            "type": answer.kind,
                            "answered": false,
                            "reason": reason,
                            "input_tokens": success.input_tokens,
                            "output_tokens": success.output_tokens,
                            "think_prefill": success.used_prefill,
                            "harmony_splice": success.used_splice,
                        });
                        verdicts_map.insert(outcome.id.clone(), answer);
                        ("ok", attrs)
                    }
                    Err(reason) => {
                        if failure.is_none() {
                            failure = Some(format!(
                                "question '{}' on model '{model}': {reason}",
                                outcome.id
                            ));
                        }
                        ("error", serde_json::json!({ "error": reason }))
                    }
                }
            }
            Err(reason) => {
                if failure.is_none() {
                    failure = Some(format!(
                        "question '{}' on model '{model}': {reason}",
                        outcome.id
                    ));
                }
                ("error", serde_json::json!({ "error": reason }))
            }
        };
        if let Some(ref mut t) = tracer {
            t.record(
                &format!("verdict:q:{}", outcome.id),
                "proxy_request",
                outcome.start_ms,
                outcome.duration_ms,
                status,
                attrs,
            );
        }
    }
    if ttft_ms == u32::MAX {
        ttft_ms = 0;
    }
    usage.total_tokens = usage.prompt_tokens.saturating_add(usage.completion_tokens);
    let total_ms = request_start.elapsed().as_millis() as u32;
    let status_code: u16 = if failure.is_some() { 502 } else { 200 };

    let _ = settle_guard
        .complete(accounting.settle(
            proxy::UpstreamUsage::uncached(usage.prompt_tokens, usage.completion_tokens),
            ttft_ms,
            total_ms,
            status_code,
            None,
        ))
        .await;

    if let Some(msg) = failure {
        if let Some(t) = tracer.take() {
            t.finish("error");
        }
        return error_json(StatusCode::BAD_GATEWAY, &msg);
    }
    if let Some(t) = tracer.take() {
        t.finish("ok");
    }

    let response = types::VerdictsResponse {
        model,
        verdicts: verdicts_map,
        usage,
    };
    // Not `axum::Json`: floats are written fixed-point (see `types::to_json_bytes`).
    let body = match types::to_json_bytes(&response) {
        Ok(bytes) => bytes,
        Err(e) => {
            return error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                &format!("failed to serialize verdicts response: {e}"),
            )
        }
    };
    let mut resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"));
    if let Ok(value) = header::HeaderValue::from_str(&request_id.to_string()) {
        resp.headers_mut().insert("x-obleth-request-id", value);
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    /// Loopback mock upstream serving `/v1/chat/completions` (the pattern used
    /// by `jwt_auth::tests::serve_jwks`). The behavior is keyed on the user
    /// message so one server exercises success, error, and no-logprobs paths.
    async fn serve_upstream() -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<serde_json::Value>>>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let bodies: Arc<Mutex<Vec<serde_json::Value>>> = Arc::new(Mutex::new(Vec::new()));
        let hits_clone = hits.clone();
        let bodies_clone = bodies.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let hits = hits_clone.clone();
                let bodies = bodies_clone.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let user = body
                        .pointer("/messages/1/content")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    bodies.lock().unwrap().push(body);
                    if user.contains("FAIL") {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            axum::Json(serde_json::json!({"error": "boom"})),
                        );
                    }
                    if user.contains("NOLOGPROBS") {
                        return (
                            StatusCode::OK,
                            axum::Json(serde_json::json!({
                                "choices": [{"message": {"content": "A"}, "logprobs": null}],
                                "usage": {"prompt_tokens": 10, "completion_tokens": 1}
                            })),
                        );
                    }
                    (
                        StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "choices": [{
                                "message": {"role": "assistant", "content": "A"},
                                "logprobs": {"content": [{
                                    "token": " A",
                                    "logprob": -0.105,
                                    "top_logprobs": [
                                        {"token": " A", "logprob": -0.105},
                                        {"token": " B", "logprob": -2.4},
                                        {"token": "The", "logprob": -5.0}
                                    ]
                                }]},
                                "finish_reason": "length"
                            }],
                            "usage": {"prompt_tokens": 120, "completion_tokens": 1, "total_tokens": 121}
                        })),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1/chat/completions"), hits, bodies)
    }

    fn call(id: &str, user: &str) -> QuestionCall {
        call_with_images(id, user, Vec::new())
    }

    fn call_with_images(id: &str, user: &str, images: Vec<String>) -> QuestionCall {
        QuestionCall {
            id: id.to_string(),
            // The canned upstream answers ` A`/` B`, so these labels match.
            labels: vec!["A".to_string(), "B".to_string()],
            boolean: false,
            two_letter: false,
            shared: Arc::new(SharedPrompt {
                upstream_model: "upstream-model".to_string(),
                system: "shared system".to_string(),
                images,
            }),
            user: user.to_string(),
        }
    }

    #[tokio::test]
    async fn images_reach_every_question_ahead_of_its_text() {
        let (url, hits, bodies) = serve_upstream().await;
        let http = reqwest::Client::new();
        let images = vec!["data:image/png;base64,AAAA".to_string()];
        let outcomes = fan_out(
            &http,
            &url,
            None,
            &reqwest::header::HeaderMap::new(),
            "vision-model",
            vec![
                call_with_images("q1", "first question", images.clone()),
                call_with_images("q2", "second question", images),
            ],
            Duration::from_secs(5),
        )
        .await;
        assert!(outcomes.iter().all(|o| o.result.is_ok()));
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let bodies = bodies.lock().unwrap();
        for body in bodies.iter() {
            assert_eq!(body["messages"][0]["content"], "shared system");
            let parts = body["messages"][1]["content"].as_array().expect("parts");
            assert_eq!(parts[0]["image_url"]["url"], "data:image/png;base64,AAAA");
            assert_eq!(parts[1]["type"], "text");
        }
    }

    #[tokio::test]
    async fn a_harmony_model_asked_about_images_is_not_spliced() {
        // The splice is text-only: with images, the plain result stands and
        // /v1/completions is never called, even for a model known to need it.
        let (url, chat_hits, text_hits) = serve_harmony_upstream().await;
        let http = reqwest::Client::new();
        let model = "harmony-model-images";
        remember_harmony_splice(model);
        let outcomes = fan_out(
            &http,
            &url,
            None,
            &reqwest::header::HeaderMap::new(),
            model,
            vec![call_with_images(
                "q",
                "look",
                vec!["https://example.com/a.png".to_string()],
            )],
            Duration::from_secs(5),
        )
        .await;
        let success = outcomes[0].result.as_ref().expect("the plain call ran");
        assert!(!success.used_splice && !success.used_prefill);
        let (_, in_set) = answers::label_masses(&success.entries, &call("q", "x").labels, false);
        assert!(
            in_set < MIN_IN_SET_MASS,
            "no answer label, so the request fails"
        );
        assert_eq!(chat_hits.load(Ordering::SeqCst), 1);
        assert_eq!(text_hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn upstream_error_detail_reads_the_backend_message() {
        assert_eq!(
            upstream_error_detail(
                r#"{"error": {"message": "At most 1 image(s)\n may be provided", "type": "BadRequestError"}}"#
            )
            .as_deref(),
            Some("At most 1 image(s) may be provided")
        );
        assert_eq!(
            upstream_error_detail(r#"{"message": "bad"}"#).as_deref(),
            Some("bad")
        );
        assert_eq!(
            upstream_error_detail(r#"{"error": "boom"}"#).as_deref(),
            Some("boom")
        );
        assert_eq!(upstream_error_detail("<html>502</html>"), None);
        let long = format!(r#"{{"error": {{"message": "{}"}}}}"#, "é".repeat(400));
        let detail = upstream_error_detail(&long).unwrap();
        assert!(detail.len() <= 300 + '…'.len_utf8());
    }

    #[tokio::test]
    async fn fan_out_parses_distributions_and_usage_from_each_call() {
        let (url, hits, bodies) = serve_upstream().await;
        let http = reqwest::Client::new();
        let outcomes = fan_out(
            &http,
            &url,
            Some("test-key"),
            &reqwest::header::HeaderMap::new(),
            "plain-model",
            vec![call("q1", "first question"), call("q2", "second question")],
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(outcomes.len(), 2);
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        for outcome in &outcomes {
            let success = outcome.result.as_ref().expect("call should succeed");
            assert_eq!(success.input_tokens, 120);
            assert_eq!(success.output_tokens, 1);
            assert_eq!(success.entries.len(), 3);
        }
        // Every call carried the identical shared prefix (the prefix-cache
        // contract) and the single-token sampling params.
        let bodies = bodies.lock().unwrap();
        for body in bodies.iter() {
            assert_eq!(body["messages"][0]["content"], "shared system");
            assert_eq!(body["max_tokens"], 1);
            assert_eq!(body["top_logprobs"], 20);
        }
    }

    #[tokio::test]
    async fn upstream_errors_and_missing_logprobs_fail_only_that_question() {
        let (url, _, _) = serve_upstream().await;
        let http = reqwest::Client::new();
        let outcomes = fan_out(
            &http,
            &url,
            None,
            &reqwest::header::HeaderMap::new(),
            "plain-model-2",
            vec![
                call("ok", "fine"),
                call("bad", "FAIL"),
                call("nolp", "NOLOGPROBS"),
            ],
            Duration::from_secs(5),
        )
        .await;
        let by_id = |id: &str| outcomes.iter().find(|o| o.id == id).unwrap();
        assert!(by_id("ok").result.is_ok());
        assert!(by_id("bad").result.as_ref().unwrap_err().contains("500"));
        assert!(by_id("nolp")
            .result
            .as_ref()
            .unwrap_err()
            .contains("no logprobs"));
    }

    #[tokio::test]
    async fn an_unreachable_upstream_is_a_named_error_not_a_hang() {
        // 127.0.0.1:1 refuses connections (the idiom used by image_gen tests).
        let http = reqwest::Client::new();
        let outcomes = fan_out(
            &http,
            "http://127.0.0.1:1/v1/chat/completions",
            None,
            &reqwest::header::HeaderMap::new(),
            "plain-model-3",
            vec![call("q", "hello")],
            Duration::from_secs(2),
        )
        .await;
        let err = outcomes[0].result.as_ref().unwrap_err();
        assert!(
            err.contains("unreachable") || err.contains("timed out"),
            "unexpected error: {err}"
        );
    }

    /// A mock thinking model: the plain call's first token is scratchpad
    /// prose; the empty-think prefill (`continue_final_message`) yields labels.
    async fn serve_thinking_upstream() -> (String, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_clone = hits.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let hits = hits_clone.clone();
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let prefilled =
                        body.get("continue_final_message").and_then(|v| v.as_bool()) == Some(true);
                    let top = if prefilled {
                        serde_json::json!([
                            {"token": " B", "logprob": -0.2},
                            {"token": " A", "logprob": -1.8}
                        ])
                    } else {
                        // Thinking prose that also LEAKS a label at ~5e-5
                        // probability — the live GLM signature. The leak must
                        // not count as an answer (it renormalizes into a
                        // confident-looking noise distribution).
                        serde_json::json!([
                            {"token": "The", "logprob": -0.5},
                            {"token": "State", "logprob": -1.0},
                            {"token": " A", "logprob": -10.0}
                        ])
                    };
                    axum::Json(serde_json::json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": ""},
                            "logprobs": {"content": [{
                                "token": "x", "logprob": -0.5, "top_logprobs": top
                            }]},
                            "finish_reason": "length"
                        }],
                        "usage": {"prompt_tokens": 100, "completion_tokens": 1}
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1/chat/completions"), hits)
    }

    #[tokio::test]
    async fn a_thinking_model_is_retried_with_the_empty_think_prefill_and_remembered() {
        let (url, hits) = serve_thinking_upstream().await;
        let http = reqwest::Client::new();

        // First request: plain call yields no label mass, prefill retry
        // succeeds; both attempts are billed.
        let outcomes = fan_out(
            &http,
            &url,
            None,
            &reqwest::header::HeaderMap::new(),
            "thinking-model-e2e",
            vec![call("q1", "first")],
            Duration::from_secs(5),
        )
        .await;
        let success = outcomes[0].result.as_ref().expect("retry should succeed");
        assert!(success.used_prefill);
        assert_eq!(success.input_tokens, 200, "both attempts billed");
        assert_eq!(hits.load(Ordering::SeqCst), 2, "plain + prefill");
        let (masses, in_set) =
            answers::label_masses(&success.entries, &call("q1", "x").labels, false);
        assert!(in_set > 0.0 && masses[1] > masses[0]);

        // Second request against the same model skips the doomed plain call.
        let outcomes = fan_out(
            &http,
            &url,
            None,
            &reqwest::header::HeaderMap::new(),
            "thinking-model-e2e",
            vec![call("q2", "second")],
            Duration::from_secs(5),
        )
        .await;
        assert!(outcomes[0].result.as_ref().unwrap().used_prefill);
        assert_eq!(
            hits.load(Ordering::SeqCst),
            3,
            "prefill only on the second request"
        );
    }

    /// A gpt-oss-style upstream: the chat endpoint deterministically answers
    /// `<|channel|>` (all the mass — observed live on gpt-oss-120b), and only
    /// the raw completions endpoint, given a prompt whose assistant turn is
    /// already open in the final channel, yields answer labels — in the
    /// completions `{token: logprob}` map format.
    async fn serve_harmony_upstream() -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let chat_hits = Arc::new(AtomicUsize::new(0));
        let text_hits = Arc::new(AtomicUsize::new(0));
        let (chat_c, text_c) = (chat_hits.clone(), text_hits.clone());
        let app = axum::Router::new()
            .route(
                "/v1/chat/completions",
                axum::routing::post(move || {
                    let hits = chat_c.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        axum::Json(serde_json::json!({
                            "choices": [{
                                "message": {"role": "assistant", "content": "<|channel|>"},
                                "logprobs": {"content": [{
                                    "token": "<|channel|>", "logprob": 0.0,
                                    "top_logprobs": [
                                        {"token": "<|channel|>", "logprob": 0.0},
                                        {"token": "<|constrain|>", "logprob": -20.1}
                                    ]
                                }]},
                                "finish_reason": "length"
                            }],
                            "usage": {"prompt_tokens": 100, "completion_tokens": 1}
                        }))
                    }
                }),
            )
            .route(
                "/v1/completions",
                axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let hits = text_c.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        let prompt = body["prompt"].as_str().unwrap_or_default();
                        assert!(
                            prompt.ends_with("<|start|>assistant<|channel|>final<|message|>"),
                            "splice must open the final channel"
                        );
                        axum::Json(serde_json::json!({
                            "choices": [{
                                "text": " B",
                                "logprobs": {
                                    "text_offset": [0],
                                    "token_logprobs": [-0.2],
                                    "tokens": [" B"],
                                    "top_logprobs": [{" B": -0.2, " A": -1.7}]
                                },
                                "finish_reason": "length"
                            }],
                            "usage": {"prompt_tokens": 90, "completion_tokens": 1}
                        }))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (
            format!("http://{addr}/v1/chat/completions"),
            chat_hits,
            text_hits,
        )
    }

    #[tokio::test]
    async fn a_harmony_model_is_retried_via_the_completions_splice_and_remembered() {
        let (url, chat_hits, text_hits) = serve_harmony_upstream().await;
        let http = reqwest::Client::new();

        // First request: the plain chat call's top token is <|channel|>, so
        // the retry goes to /v1/completions, not the think prefill.
        let outcomes = fan_out(
            &http,
            &url,
            None,
            &reqwest::header::HeaderMap::new(),
            "harmony-model-e2e",
            vec![call("q1", "first")],
            Duration::from_secs(5),
        )
        .await;
        let success = outcomes[0].result.as_ref().expect("splice should succeed");
        assert!(success.used_splice);
        assert!(!success.used_prefill);
        assert_eq!(success.input_tokens, 190, "both attempts billed");
        assert_eq!(chat_hits.load(Ordering::SeqCst), 1);
        assert_eq!(text_hits.load(Ordering::SeqCst), 1);
        let (masses, in_set) =
            answers::label_masses(&success.entries, &call("q1", "x").labels, false);
        assert!(in_set > 0.5 && masses[1] > masses[0], "answer is B");

        // Second request on the same model goes splice-first: no doomed chat
        // call at all.
        let outcomes = fan_out(
            &http,
            &url,
            None,
            &reqwest::header::HeaderMap::new(),
            "harmony-model-e2e",
            vec![call("q2", "second")],
            Duration::from_secs(5),
        )
        .await;
        assert!(outcomes[0].result.as_ref().unwrap().used_splice);
        assert_eq!(chat_hits.load(Ordering::SeqCst), 1, "chat not called again");
        assert_eq!(text_hits.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn a_single_transient_500_is_retried_not_fatal() {
        // One sick replica behind a load balancer answers the first attempt
        // with a 500; the bonus retry lands on a healthy one.
        let flake = Arc::new(AtomicUsize::new(0));
        let flake_clone = flake.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move || {
                let flake = flake_clone.clone();
                async move {
                    if flake.fetch_add(1, Ordering::SeqCst) == 0 {
                        return (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            axum::Json(serde_json::json!({"error": "sick replica"})),
                        );
                    }
                    (
                        StatusCode::OK,
                        axum::Json(serde_json::json!({
                            "choices": [{
                                "message": {"role": "assistant", "content": "A"},
                                "logprobs": {"content": [{
                                    "token": " A", "logprob": -0.1,
                                    "top_logprobs": [
                                        {"token": " A", "logprob": -0.1},
                                        {"token": " B", "logprob": -2.4}
                                    ]
                                }]},
                                "finish_reason": "length"
                            }],
                            "usage": {"prompt_tokens": 100, "completion_tokens": 1}
                        })),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let http = reqwest::Client::new();
        let outcomes = fan_out(
            &http,
            &format!("http://{addr}/v1/chat/completions"),
            None,
            &reqwest::header::HeaderMap::new(),
            "flaky-model",
            vec![call("q", "hello")],
            Duration::from_secs(5),
        )
        .await;
        let success = outcomes[0]
            .result
            .as_ref()
            .expect("the retry should recover from one 500");
        assert!(!success.used_prefill);
        assert_eq!(flake.load(Ordering::SeqCst), 2, "first attempt + one retry");
    }

    #[test]
    fn the_model_named_wins_then_the_default_then_auto() {
        assert_eq!(requested_model("glm-5-3", Some("judge")), "glm-5-3");
        assert_eq!(requested_model("", Some("judge")), "judge");
        assert_eq!(requested_model("  ", Some(" judge ")), "judge");
        assert_eq!(requested_model("", None), "auto");
        assert_eq!(requested_model("", Some("")), "auto");
    }

    #[test]
    fn the_estimate_reserves_the_full_no_cache_fan_out() {
        let tokenizer = obleth_tokenizer::HeuristicTokenizer;
        let system = "x".repeat(400); // ~100 tokens heuristic
        let users = vec!["y".repeat(40), "y".repeat(40)]; // ~10 tokens each
        let est = estimate(&tokenizer, &system, &users, 0);
        // Each question pays the state prefill again: 2 × (100 + 10 + 8).
        assert_eq!(est.input_tokens, 2 * (100 + 10 + 8));
        assert_eq!(est.estimated_output_tokens, 2);
        // And the images, at the chat estimate's flat cost per image part.
        let est = estimate(&tokenizer, &system, &users, 2);
        let per_image = proxy::IMAGE_PART_TOKEN_ESTIMATE as u32;
        assert_eq!(est.input_tokens, 2 * (100 + 2 * per_image + 10 + 8));
    }

    /// An upstream for two-letter codes. `split` answers like a vocabulary
    /// that splits codes: the first token is the bare letter `B`, and after a
    /// continued `B` the next token is `M`. Otherwise it answers the whole
    /// code `BM` as one token.
    async fn serve_codes(
        split: bool,
    ) -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<serde_json::Value>>>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let bodies: Arc<Mutex<Vec<serde_json::Value>>> = Arc::default();
        let (h, b) = (hits.clone(), bodies.clone());
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |axum::Json(body): axum::Json<serde_json::Value>| {
                let (hits, bodies) = (h.clone(), b.clone());
                async move {
                    hits.fetch_add(1, Ordering::SeqCst);
                    let continued = body.pointer("/messages/2/content").and_then(|v| v.as_str());
                    let top = match (split, continued) {
                        (true, Some("B")) => serde_json::json!([
                            {"token": "M", "logprob": (0.9f64).ln()},
                            {"token": "N", "logprob": (0.1f64).ln()}
                        ]),
                        (true, _) => serde_json::json!([
                            {"token": " B", "logprob": (0.8f64).ln()},
                            {"token": "The", "logprob": (0.2f64).ln()}
                        ]),
                        (false, _) => serde_json::json!([
                            {"token": "BM", "logprob": (0.95f64).ln()},
                            {"token": " BM", "logprob": (0.05f64).ln()}
                        ]),
                    };
                    bodies.lock().unwrap().push(body);
                    axum::Json(serde_json::json!({
                        "choices": [{"logprobs": {"content": [{"token": "x", "logprob": 0.0, "top_logprobs": top}]}}],
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
        (format!("http://{addr}/v1/chat/completions"), hits, bodies)
    }

    fn code_call() -> QuestionCall {
        let labels: Vec<String> = (0..60)
            .map(|i| {
                let l = |k: usize| (b'A' + k as u8) as char;
                format!("{}{}", l(i / 26), l(i % 26))
            })
            .collect();
        QuestionCall {
            id: "q".to_string(),
            labels,
            boolean: false,
            two_letter: true,
            shared: Arc::new(SharedPrompt {
                upstream_model: "upstream-model".to_string(),
                system: "shared system".to_string(),
                images: Vec::new(),
            }),
            user: "question".to_string(),
        }
    }

    #[tokio::test]
    async fn whole_code_tokens_need_no_second_call() {
        let (url, hits, _) = serve_codes(false).await;
        let call = code_call();
        let labels = call.labels.clone();
        let outcomes = fan_out(
            &reqwest::Client::new(),
            &url,
            None,
            &reqwest::header::HeaderMap::new(),
            "codes-whole",
            vec![call],
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(hits.load(Ordering::SeqCst), 1);
        let success = outcomes[0].result.as_ref().unwrap();
        let (masses, in_set) = answers::label_masses(&success.entries, &labels, false);
        assert!((in_set - 1.0).abs() < 1e-9);
        assert!((masses[26 + 12] - 1.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn a_split_code_is_read_with_a_continuation_call() {
        let (url, hits, bodies) = serve_codes(true).await;
        let call = code_call();
        let labels = call.labels.clone();
        let outcomes = fan_out(
            &reqwest::Client::new(),
            &url,
            None,
            &reqwest::header::HeaderMap::new(),
            "codes-split",
            vec![call],
            Duration::from_secs(5),
        )
        .await;
        // The first call, then one continuation for `B` (`The` starts no code).
        assert_eq!(hits.load(Ordering::SeqCst), 2);
        let continued = bodies.lock().unwrap()[1].clone();
        assert_eq!(continued["messages"][2]["content"], "B");
        assert_eq!(continued["continue_final_message"], true);
        let success = outcomes[0].result.as_ref().unwrap();
        assert_eq!(success.input_tokens, 100);
        assert!(
            !success.used_prefill,
            "bare-letter mass is an answer, not thinking"
        );
        let (masses, in_set) = answers::label_masses(&success.entries, &labels, false);
        assert!((masses[26 + 12] - 0.72).abs() < 1e-9, "BM = 0.8 * 0.9");
        assert!((masses[26 + 13] - 0.08).abs() < 1e-9, "BN = 0.8 * 0.1");
        assert!((in_set - 0.8).abs() < 1e-9);
    }
}

#[cfg(test)]
mod settlement_tests {
    /// The handler's source with all whitespace removed. There is no AppState
    /// harness here (it needs live Redis), so the reservation contract is
    /// pinned at the source level, like the passthrough pipeline's.
    fn handler() -> String {
        let full = include_str!("mod.rs");
        let src = &full[..full.find("\nmod tests {").expect("the test module")];
        let start = src.find("async fn handler_inner(").unwrap_or(0);
        src[start..]
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect()
    }

    #[test]
    fn verdicts_reserve_term_budget_and_settle_through_the_guard() {
        let h = handler();
        assert!(!h.contains("reserve_budget_with_term("), "check-only gate");
        assert!(!h.contains("settle_request("), "unguarded settlement");
        assert!(
            h.contains(".reserve_with_term(&resolved.key_id,0,0,est.total(),Some(gate),est_cost)")
        );
        let tenant = h
            .find(".reserve_with_term(&resolved.tenant_id,")
            .expect("tenant reserve");
        let guard = h
            .find("letsettle_guard=accounting.unbilled_guard();")
            .expect("guard");
        assert!(tenant < guard);
        // Rate limited, term exhausted, and fail-closed error release the key hold.
        assert_eq!(h[tenant..guard].matches("hold.release().await").count(), 3);
        assert!(h[tenant..guard].contains("hold.hand_off()"));
        // No legacy ledger-only rejection after the reservation owns budget.
        assert!(!h[guard..].contains("returnreject("));
        assert_eq!(h[guard..].matches("settle_guard.complete(").count(), 2);
    }

    #[test]
    fn verdicts_scan_tenant_input_guardrails_before_estimate_and_admission() {
        let h = handler();
        let scan = h
            .find(".scan_input(")
            .expect("tenant input guardrails scan");
        let estimate = h
            .find("letest=estimate(&*state.tokenizer,&system,&users,image_count);")
            .expect("cost estimate");
        let admit = h.find("state.fairshare.admit(").expect("admission");
        assert!(scan < estimate && estimate < admit);
        // A block answers with the policy's status; a redaction is read back.
        assert!(h[scan..estimate].contains("returnerror_json(block.status,block.reason);"));
        assert!(h[scan..estimate].contains("system=texts[0].clone();"));
    }

    #[test]
    fn verdicts_admission_is_bounded_by_the_shared_admission_timeout() {
        let h = handler();
        // Same wrapper as the passthrough pipeline (proxy.rs): bounded by
        // `OBLETH_ADMISSION_TIMEOUT_SECS`, and a timed-out wait answers 503
        // with the shared `Retry-After` value rather than parking forever.
        assert!(h.contains("letadmit_wait=proxy::admission_timeout();"));
        assert!(h.contains("tokio::time::timeout(admit_wait,admit).await"));
        let timeout_branch = h.find("Err(_)=>{").expect("the timeout branch");
        let scope = &h[timeout_branch..];
        let end = scope
            .find("letadmission=admitted.admission;")
            .expect("the match ends before the admission is unpacked");
        let scope = &scope[..end];
        assert!(scope.contains("StatusCode::SERVICE_UNAVAILABLE"));
        assert!(scope.contains("\"timedoutwaitingformodelcapacity\""));
        assert!(scope.contains("header::RETRY_AFTER"));
        assert!(scope.contains("proxy::ADMISSION_RETRY_AFTER_SECS"));
    }
}

/// The handler end to end against a canned backend, for how a request's model
/// is chosen. Needs Postgres and Redis (`OBLETH_TEST_DATABASE_URL`,
/// `OBLETH_TEST_REDIS_URL`); skipped without them.
#[cfg(test)]
mod model_choice_tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{header, Method, Request, StatusCode};
    use axum::routing::post;
    use axum::{Json, Router};
    use moka::future::Cache;
    use obleth_config::{
        AutoRouterSettings, FamilyMembers, ModelFamily, ReasoningLevel, ResolvedModel,
    };
    use serde_json::{json, Value};
    use uuid::Uuid;

    use crate::state::AppState;

    type Seen = Arc<Mutex<Vec<Value>>>;

    /// Answers every question `yes`, and keeps each body it was sent.
    async fn spawn_upstream() -> (Seen, String) {
        let seen: Seen = Arc::default();
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|State(seen): State<Seen>, Json(body): Json<Value>| async move {
                    seen.lock().unwrap().push(body);
                    Json(json!({
                        "choices": [{
                            "message": {"role": "assistant", "content": "yes"},
                            "logprobs": {"content": [{
                                "token": "yes", "logprob": -0.01,
                                "top_logprobs": [
                                    {"token": "yes", "logprob": -0.01},
                                    {"token": "no", "logprob": -4.6}
                                ]
                            }]},
                            "finish_reason": "length"
                        }],
                        "usage": {"prompt_tokens": 20, "completion_tokens": 1, "total_tokens": 21}
                    }))
                }),
            )
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (seen, format!("http://{addr}/v1"))
    }

    struct Gateway {
        state: AppState,
        wal_dir: std::path::PathBuf,
    }

    impl Drop for Gateway {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.wal_dir);
        }
    }

    impl Gateway {
        async fn register(&self, name: &str, base: &str) {
            let mut r: ResolvedModel = crate::boons::test_support::test_route();
            r.model_name = name.into();
            r.upstream_model = format!("{name}-upstream");
            r.api_base = base.into();
            self.state
                .model_cache
                .insert(r.model_name.clone(), Arc::new(r))
                .await;
        }

        async fn key(&self) -> String {
            let mut key = crate::boons::test_support::test_key();
            key.key_id = Uuid::new_v4();
            key.tenant_id = Uuid::new_v4();
            let secret = format!("sk-verdicts-{}", Uuid::new_v4());
            self.state
                .key_cache
                .insert(obleth_config::hash_api_key(&secret), Arc::new(key))
                .await;
            secret
        }

        async fn verdicts(&self, secret: &str, body: Value) -> (StatusCode, Value) {
            let req = Request::builder()
                .method(Method::POST)
                .uri(super::VERDICTS_PATH)
                .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let resp = super::handler(State(self.state.clone()), req).await;
            let status = resp.status();
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            )
        }
    }

    async fn gateway(router: AutoRouterSettings) -> Option<Gateway> {
        let (Ok(db), Ok(redis_url)) = (
            std::env::var("OBLETH_TEST_DATABASE_URL"),
            std::env::var("OBLETH_TEST_REDIS_URL"),
        ) else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return None;
        };
        let store = obleth_store::Store::connect(&db).await.expect("postgres");
        let redis = obleth_redis::RedisStore::connect(&redis_url)
            .await
            .expect("redis");
        let wal_dir = std::env::temp_dir().join(format!("obleth-verdicts-wal-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&wal_dir).unwrap();
        let telemetry = obleth_telemetry::TelemetrySink::start(
            "http://127.0.0.1:1",
            "obleth_test",
            "default",
            "",
            wal_dir.join("usage.wal").to_str().unwrap(),
            Duration::from_millis(100),
            true,
        )
        .await
        .expect("telemetry sink");
        let http = reqwest::Client::new();
        let state = AppState {
            redis,
            fairshare: obleth_fairshare::FairShare::start(
                Arc::new(obleth_fairshare::StaticCapacity::new(64)),
                obleth_config::FairshareAlgorithm::default(),
                64,
            ),
            tokenizer: Arc::new(obleth_tokenizer::HeuristicTokenizer::new()),
            telemetry,
            http: http.clone(),
            upstream_base: "http://127.0.0.1:1/v1".into(),
            upstream_timeout: Duration::from_secs(30),
            key_cache: Cache::builder().build(),
            model_cache: Cache::builder().build(),
            mcp_cache: Cache::builder().build(),
            model_registry: crate::router::ModelRegistry::new(),
            families: crate::reasoning::FamilyRegistry::new(),
            classifier: crate::classifier::Classifier::new(router),
            output_stats: Default::default(),
            boons: crate::boons::BoonEngine::new(Default::default()),
            tool_cache: Cache::builder().build(),
            metrics: Arc::new(crate::metrics::Metrics::new()),
            fail_open: false,
            alerts: obleth_admin::AlertDispatcher::new(http, Default::default()),
            session_id_derivation: false,
            session_affinity: None,
            session_affinity_settings: Default::default(),
            compressor: None,
            energy: crate::energy::EnergyEngine::new(Default::default()),
            jwt: None,
            knowledge: Arc::new(crate::knowledge::KnowledgeIndex::new()),
            jobs: crate::jobs::JobStore::new(store, Default::default()),
            load: None,
        };
        Some(Gateway { state, wal_dir })
    }

    fn family(none: &str) -> ModelFamily {
        ModelFamily {
            id: Uuid::new_v4(),
            name: "acme".into(),
            description: String::new(),
            // The family's chat default is a thinking level; verdicts still
            // takes the `none` member.
            default_level: ReasoningLevel::High,
            members: FamilyMembers {
                none: none.into(),
                low: "acme-think".into(),
                medium: "acme-think".into(),
                high: "acme-think".into(),
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn question() -> Value {
        json!({"q": {"type": "boolean", "instructions": "Is it?"}})
    }

    #[tokio::test]
    async fn a_family_name_is_answered_by_its_none_member() {
        let Some(gw) = gateway(AutoRouterSettings::default()).await else {
            return;
        };
        let (seen, base) = spawn_upstream().await;
        gw.register("acme-instruct", &base).await;
        gw.register("acme-think", &base).await;
        gw.state.families.store(vec![family("acme-instruct")]);
        let secret = gw.key().await;

        let (status, body) = gw
            .verdicts(
                &secret,
                json!({"model": "acme", "state": "x", "questions": question()}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["model"], "acme-instruct");
        assert_eq!(body["verdicts"]["q"]["value"], true);
        assert_eq!(
            seen.lock().unwrap().pop().unwrap()["model"],
            "acme-instruct-upstream"
        );
    }

    #[tokio::test]
    async fn a_family_whose_none_member_is_missing_says_so() {
        let Some(gw) = gateway(AutoRouterSettings::default()).await else {
            return;
        };
        gw.state.families.store(vec![family("acme-gone")]);
        let secret = gw.key().await;

        let (status, body) = gw
            .verdicts(
                &secret,
                json!({"model": "acme", "state": "x", "questions": question()}),
            )
            .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("'acme-gone', which is not registered"));
    }

    #[tokio::test]
    async fn a_request_without_a_model_gets_the_verdicts_default() {
        let Some(gw) = gateway(AutoRouterSettings {
            verdicts_default_model: Some("judge".into()),
            ..Default::default()
        })
        .await
        else {
            return;
        };
        let (seen, base) = spawn_upstream().await;
        gw.register("judge", &base).await;
        let secret = gw.key().await;

        let (status, body) = gw
            .verdicts(&secret, json!({"state": "x", "questions": question()}))
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["model"], "judge");
        assert_eq!(
            seen.lock().unwrap().pop().unwrap()["model"],
            "judge-upstream"
        );

        // A named model still wins over the default.
        gw.register("other", &base).await;
        let (status, body) = gw
            .verdicts(
                &secret,
                json!({"model": "other", "state": "x", "questions": question()}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["model"], "other");
    }

    #[tokio::test]
    async fn without_a_default_a_request_without_a_model_goes_to_auto() {
        let Some(gw) = gateway(AutoRouterSettings::default()).await else {
            return;
        };
        let secret = gw.key().await;

        // No candidates are registered, so `auto` has nothing to pick.
        let (status, body) = gw
            .verdicts(&secret, json!({"state": "x", "questions": question()}))
            .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            body["error"]["message"],
            "no model is available to satisfy the auto request"
        );
    }

    /// Never answers with a label: its only likely first token is `The`.
    async fn spawn_unlabeled_upstream() -> String {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(|| async {
                Json(json!({
                    "choices": [{"logprobs": {"content": [{
                        "token": "The", "logprob": -0.01,
                        "top_logprobs": [{"token": "The", "logprob": -0.01}]
                    }]}}],
                    "usage": {"prompt_tokens": 20, "completion_tokens": 1}
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/v1")
    }

    #[tokio::test]
    async fn no_answer_is_an_error_unless_the_request_asks_for_low_confidence() {
        let Some(gw) = gateway(AutoRouterSettings::default()).await else {
            return;
        };
        let base = spawn_unlabeled_upstream().await;
        gw.register("mute", &base).await;
        let secret = gw.key().await;
        let questions = json!({"team": {"type": "choice", "instructions": "?", "criteria": {"a": null, "b": null}}});

        let (status, body) = gw
            .verdicts(
                &secret,
                json!({"model": "mute", "state": "x", "questions": questions}),
            )
            .await;
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{body}");

        let (status, body) = gw
            .verdicts(
                &secret,
                json!({"model": "mute", "state": "x", "questions": questions, "on_no_answer": "low_confidence"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let v = &body["verdicts"]["team"];
        assert_eq!(v["answered"], false);
        assert_eq!(v["confidence"], 0.0);
        assert_eq!(v["probabilities"], json!({"a": 0.5, "b": 0.5}));
    }

    #[tokio::test]
    async fn an_answered_verdict_carries_no_answered_field() {
        let Some(gw) = gateway(AutoRouterSettings::default()).await else {
            return;
        };
        let (_, base) = spawn_upstream().await;
        gw.register("judge", &base).await;
        let secret = gw.key().await;
        let (status, body) = gw
            .verdicts(
                &secret,
                json!({"model": "judge", "state": "x", "questions": question(), "on_no_answer": "low_confidence"}),
            )
            .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["verdicts"]["q"].get("answered").is_none(), "{body}");
    }
}
