//! `POST /v1/verdicts` — native typed-decision evaluation.
//!
//! One state, N independent typed questions (boolean / choice / score), answered
//! by the route's own chat backend as N concurrent single-token calls whose
//! first-token `top_logprobs` become probability distributions. No external
//! API, no new model: any registered `chat` route (vLLM, SGLang, llama.cpp,
//! or a SaaS backend) serves it.
//!
//! The endpoint participates in the full pipeline: key auth, model resolution
//! (aliases and `auto`), ONE fairshare permit covering the whole fan-out, the
//! key- and tenant-level budget reserves, and a single `settle_request` with
//! the summed real usage under `request_type: "verdict"`. Per-question
//! visibility is in the trace (`verdict:q:<id>` spans), not extra ledger
//! rows. The prompt-prefix / label mechanics live in [`prompt`], the
//! distribution math in [`answers`], and the wire types in [`types`].

mod answers;
mod prompt;
mod types;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, Request, Response, StatusCode};
use axum::response::IntoResponse;
use obleth_config::Admission;
use obleth_tokenizer::{CostEstimate, Tokenizer};
use uuid::Uuid;

use crate::proxy::{
    self, build_targets, derive_intent, effective_admission_weight, error_json, finalize,
    gate_resolved_key, key_term_period_key, resolve_conversation, resolve_model, settle_request,
    surfaced_request_type, term_period_key, union_candidate_tags, RequestMeta, BODY_LIMIT,
};
use crate::state::AppState;

pub(crate) const VERDICTS_PATH: &str = "/v1/verdicts";

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

/// One prepared upstream call: the question id, its answer labels (for the
/// zero-label-mass retry decision), and both body variants.
pub(crate) struct QuestionCall {
    pub id: String,
    pub labels: Vec<String>,
    pub boolean: bool,
    pub body: serde_json::Value,
    pub prefill_body: serde_json::Value,
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
}

/// Fan the prepared question calls out concurrently against ONE upstream URL.
///
/// A single target for every question is deliberate: the questions share a
/// byte-identical prompt prefix, and scattering them across replicas would
/// re-prefill the state on each one instead of hitting the prefix cache.
///
/// Thinking models get one adaptive retry: when the plain call succeeds but
/// none of its top tokens is an answer label, the question is retried with
/// the empty-think prefill, and a success teaches [`THINK_PREFILL`] to send
/// the prefill first for this `model` from then on. Usage of both attempts is
/// billed.
pub(crate) async fn fan_out(
    http: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    model: &str,
    calls: Vec<QuestionCall>,
    timeout: Duration,
) -> Vec<QuestionOutcome> {
    let prefill_first = needs_think_prefill(model);
    let futures = calls.into_iter().map(|call| async move {
        let start_ms = crate::tracer::now_ms();
        let started = Instant::now();
        let first_body = if prefill_first {
            &call.prefill_body
        } else {
            &call.body
        };
        let mut result = one_call(http, url, api_key, first_body, timeout).await;
        if let Ok(success) = &mut result {
            success.used_prefill = prefill_first;
        }
        if !prefill_first {
            // Zero label mass on a *successful* plain call is the thinking-model
            // signature (the first token is scratchpad prose). Try once more
            // past an empty think block before declaring failure.
            let no_label = matches!(&result, Ok(s) if {
                let (_, in_set) = answers::label_masses(&s.entries, &call.labels, call.boolean);
                in_set.partial_cmp(&0.0) != Some(std::cmp::Ordering::Greater)
            });
            if no_label {
                let plain = result.expect("checked Ok above");
                match one_call(http, url, api_key, &call.prefill_body, timeout).await {
                    Ok(mut retried) => {
                        let (_, in_set) =
                            answers::label_masses(&retried.entries, &call.labels, call.boolean);
                        if in_set > 0.0 {
                            remember_think_prefill(model);
                        }
                        // Bill both attempts; the plain call really ran.
                        retried.input_tokens =
                            retried.input_tokens.saturating_add(plain.input_tokens);
                        retried.output_tokens =
                            retried.output_tokens.saturating_add(plain.output_tokens);
                        retried.used_prefill = true;
                        result = Ok(retried);
                    }
                    // Keep the plain result: its zero-label mass produces the
                    // clearer "no recognizable answer label" error downstream,
                    // and its usage is still billed.
                    Err(_) => result = Ok(plain),
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

async fn one_call(
    http: &reqwest::Client,
    url: &str,
    api_key: Option<&str>,
    body: &serde_json::Value,
    timeout: Duration,
) -> Result<QuestionSuccess, String> {
    let fut = async {
        let mut req = http.post(url).json(body);
        if let Some(key) = api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| format!("upstream unreachable: {e}"))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("upstream returned {status}"));
        }
        let completion: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| format!("upstream returned invalid JSON: {e}"))?;
        let entries = answers::parse_top_logprobs(&completion)?;
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
        })
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(result) => result,
        Err(_) => Err(format!("upstream call timed out after {timeout:?}")),
    }
}

/// The heuristic reserve for the fan-out. Each question really does prefill
/// the state again on a cold cache, so the honest estimate is per-question
/// (state + question + message overhead); prefix-cache hits show up as the
/// reserve reconciling down at settlement.
fn estimate(tokenizer: &dyn Tokenizer, system: &str, users: &[String]) -> CostEstimate {
    let system_tokens = tokenizer.count_text(system);
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
    let system = prompt::render_system(&request.state);
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
    let users: Vec<String> = request
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
    let req_meta = RequestMeta {
        session_id: conversation.value,
        session_id_source: conversation.source.as_str(),
        request_type: surfaced_request_type(&resolved, VERDICTS_PATH, &headers),
        device_id,
    };
    if let Some(t) = tracer.as_mut() {
        t.set_conversation(&req_meta.session_id, req_meta.session_id_source);
    }
    let est = estimate(&*state.tokenizer, &system, &users);

    // ---- model resolution (`auto` or a named route) ----
    let mut model = request.model.clone();
    let route = if model == crate::router::AUTO_MODEL_NAME {
        // The router's inputs are chat-shaped; synthesize the state + first
        // question as a pseudo chat body for feature extraction and intent
        // derivation. Candidates are already hard-filtered to chat models.
        let pseudo = serde_json::json!({
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": users.first().cloned().unwrap_or_default()},
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
        )
        .await;
        let grants = crate::router::BoonGrants::from_settings(&state.boons.settings());
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
        let route = resolve_model(&state, &model).await;
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
            if !allowed.iter().any(|m| m == &model) {
                return error_json(StatusCode::FORBIDDEN, "model not permitted for tenant");
            }
        }
    }

    let (in_cost_rate, out_cost_rate) = (route.input_cost_per_token, route.output_cost_per_token);
    let energy_slots = route.energy_slots_per_node;

    // ---- fairshare admission: ONE permit covers the whole fan-out ----
    let effective_weight = effective_admission_weight(resolved.weight, Some(&route));
    let admission_start = crate::tracer::now_ms();
    let admitted = match state
        .fairshare
        .admit(obleth_fairshare::AdmitRequest {
            tenant: resolved.tenant_id,
            weight: effective_weight,
            group: resolved.fairshare_group.clone(),
            group_weight: resolved.group_weight,
            model: model.clone(),
            model_max_in_flight: route.max_in_flight,
            cost: est.total(),
        })
        .await
    {
        Some(a) => a,
        None => {
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
    };
    let admission = admitted.admission;
    let permit = admitted.permit;
    let queue_wait_ms = admitted.waited.as_millis() as u32;
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
    if let Some(gate) = key_term_gate {
        match state
            .redis
            .reserve_budget_with_term(&resolved.key_id, 0, 0, est.total(), Some(gate))
            .await
        {
            Ok(obleth_redis::ReserveOutcome::Reserved { .. }) => {}
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
    if capacity > 0 || term_gate.is_some() {
        match state
            .redis
            .reserve_budget_with_term(
                &resolved.tenant_id,
                capacity,
                resolved.tokens_per_minute,
                est.total(),
                term_gate,
            )
            .await
        {
            Ok(obleth_redis::ReserveOutcome::Reserved { .. }) => {}
            Ok(obleth_redis::ReserveOutcome::RateLimited { .. }) => {
                drop(permit);
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
                    if let Some(t) = tracer.take() {
                        t.finish("error");
                    }
                    return error_json(StatusCode::SERVICE_UNAVAILABLE, "budget check failed");
                }
                tracing::warn!(error = %e, "budget reserve failed; failing open");
            }
        }
    }

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
        return reject(
            StatusCode::BAD_GATEWAY,
            "model has no usable upstream endpoint",
            tracer.take(),
        );
    };
    let url = format!("{}/chat/completions", target.base.trim_end_matches('/'));
    let req_timeout = route
        .request_timeout_secs
        .filter(|s| *s >= 1)
        .map(|s| Duration::from_secs(s as u64))
        .unwrap_or(state.upstream_timeout);

    let calls: Vec<QuestionCall> = request
        .questions
        .iter()
        .zip(users.iter())
        .map(|((id, _), user)| {
            let labels = &label_sets[id];
            QuestionCall {
                id: id.clone(),
                labels: labels.labels.clone(),
                boolean: matches!(labels.semantics, prompt::LabelSemantics::Boolean),
                body: prompt::build_body(&route.upstream_model, &system, user),
                prefill_body: prompt::build_body_prefilled(&route.upstream_model, &system, user),
            }
        })
        .collect();
    let outcomes = fan_out(
        &state.http,
        &url,
        target.api_key.as_deref(),
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
                        });
                        verdicts_map.insert(outcome.id.clone(), answer);
                        ("ok", attrs)
                    }
                    Err(reason) => {
                        if failure.is_none() {
                            failure = Some(format!("question '{}': {reason}", outcome.id));
                        }
                        ("error", serde_json::json!({ "error": reason }))
                    }
                }
            }
            Err(reason) => {
                if failure.is_none() {
                    failure = Some(format!("question '{}': {reason}", outcome.id));
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

    settle_request(
        &state,
        request_id,
        &resolved,
        &req_meta,
        &model,
        admission,
        est,
        usage.prompt_tokens,
        usage.completion_tokens,
        queue_wait_ms,
        ttft_ms,
        total_ms,
        status_code,
        "off",
        capacity,
        term_period.as_deref(),
        key_term_period.as_deref(),
        in_cost_rate,
        out_cost_rate,
        0.0,
        energy_slots,
        None,
    )
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
    let mut resp = (StatusCode::OK, axum::Json(response)).into_response();
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
        QuestionCall {
            id: id.to_string(),
            // The canned upstream answers ` A`/` B`, so these labels match.
            labels: vec!["A".to_string(), "B".to_string()],
            boolean: false,
            body: prompt::build_body("upstream-model", "shared system", user),
            prefill_body: prompt::build_body_prefilled("upstream-model", "shared system", user),
        }
    }

    #[tokio::test]
    async fn fan_out_parses_distributions_and_usage_from_each_call() {
        let (url, hits, bodies) = serve_upstream().await;
        let http = reqwest::Client::new();
        let outcomes = fan_out(
            &http,
            &url,
            Some("test-key"),
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
                        serde_json::json!([
                            {"token": "The", "logprob": -0.5},
                            {"token": "State", "logprob": -1.0}
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

    #[test]
    fn the_estimate_reserves_the_full_no_cache_fan_out() {
        let tokenizer = obleth_tokenizer::HeuristicTokenizer;
        let system = "x".repeat(400); // ~100 tokens heuristic
        let users = vec!["y".repeat(40), "y".repeat(40)]; // ~10 tokens each
        let est = estimate(&tokenizer, &system, &users);
        // Each question pays the state prefill again: 2 × (100 + 10 + 8).
        assert_eq!(est.input_tokens, 2 * (100 + 10 + 8));
        assert_eq!(est.estimated_output_tokens, 2);
    }
}
