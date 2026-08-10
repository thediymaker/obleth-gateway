//! The data-plane request pipeline.
//!
//! resolve key -> estimate cost -> fairshare admit -> reserve
//! budget -> stream to upstream -> reconcile actual cost -> emit telemetry.
//!
//! The fairshare permit is held inside the response stream and released only
//! when the stream finishes, so concurrency accounting matches real upstream
//! occupancy including streaming time.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, Request, Response, StatusCode};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use obleth_config::{
    hash_api_key, Admission, ResolvedEndpoint, ResolvedKey, ResolvedModel, UsageRecord,
};
use obleth_tokenizer::{CostEstimate, Tokenizer};
use tokio::time::timeout;
use tracing::Instrument;
use uuid::Uuid;

use crate::state::AppState;

const BODY_LIMIT: usize = 64 * 1024 * 1024;
const TAIL_CAP: usize = 16 * 1024;
/// Upper bound on a response we are willing to cache. Larger responses stream
/// through uncached so the cache can't be used to balloon Redis memory.
const CACHE_MAX_BYTES: usize = 512 * 1024;
/// Largest upstream completion a response-transforming boon will rewrite.
/// Bigger bodies pass through verbatim (fail-open).
const BOON_BUFFER_MAX: usize = 4 * 1024 * 1024;
/// Cap on the upstream error body recorded into a request trace. The body is
/// captured to explain *why* a backend rejected a request, but it can echo a
/// slice of the request and traces are retained, so the stored snippet is
/// bounded. The client still receives the full, untruncated body.
const ERROR_BODY_TRACE_CAP: usize = 4 * 1024;
/// Floor for the per-request upstream timeout when a response-transforming
/// boon is active: the upstream call is forced non-streaming, so the timeout
/// bounds the whole generation instead of time-to-first-byte.
const BOON_MIN_TIMEOUT: Duration = Duration::from_secs(120);
/// Tells an intermediary reverse proxy (nginx / ingress-nginx) not to buffer
/// the response, so streamed SSE tokens reach the client as they are produced
/// instead of in proxy-buffer-sized bursts. Harmless when no such proxy is in
/// front of the gateway. (HAProxy honours `option http-no-delay` instead.)
const NO_BUFFER_HEADER: (&str, &str) = ("x-accel-buffering", "no");
/// Fixed, short delay before the one bonus retry granted to a connection-level
/// upstream failure (a stale pooled keep-alive socket). Long enough to let a
/// fresh connection replace the dead one, short enough to stay invisible in TTFT.
const CONN_RETRY_BACKOFF: Duration = Duration::from_millis(50);

pub async fn proxy_handler(state: State<AppState>, req: Request<Body>) -> Response<Body> {
    let request_id = Uuid::new_v4();
    let mut resp = proxy_handler_inner(state, req, request_id).await;
    // Ensure every response — including error paths that build their own response —
    // carries the request id so callers (e.g. the Charo model-test console) can always
    // fetch the request's trace. Success/stream/cache paths set this already; this is a
    // no-op for them and only fills it in for the error branches.
    if !resp.headers().contains_key("x-obleth-request-id") {
        if let Ok(value) = header::HeaderValue::from_str(&request_id.to_string()) {
            resp.headers_mut().insert("x-obleth-request-id", value);
        }
    }
    resp
}

#[tracing::instrument(
    skip_all,
    name = "proxy_request",
    fields(session.id = tracing::field::Empty, session.id.source = tracing::field::Empty)
)]
async fn proxy_handler_inner(
    State(state): State<AppState>,
    req: Request<Body>,
    request_id: Uuid,
) -> Response<Body> {
    let request_start = Instant::now();
    let proxy_start_ms = crate::tracer::now_ms();
    let (parts, body) = req.into_parts();
    let method = parts.method;
    let path = parts.uri.path().to_string();
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let headers = parts.headers;

    // ---- reject path traversal before any upstream work ----
    if has_path_traversal(&path) {
        return error_json(StatusCode::BAD_REQUEST, "invalid request path");
    }

    // ---- auth ----
    let Some(secret) = bearer(&headers) else {
        return error_json(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    let hash = hash_api_key(&secret);
    let auth_start = crate::tracer::now_ms();
    let resolved = match resolve_key(&state, &hash).await {
        Some(r) => r,
        None => return error_json(StatusCode::UNAUTHORIZED, "invalid api key"),
    };
    let auth_duration = (crate::tracer::now_ms() - auth_start) as u32;
    if resolved.disabled {
        return error_json(StatusCode::FORBIDDEN, "api key disabled");
    }
    // Internal probe keys bypass tenant lifecycle gating.
    if !resolved.internal && resolved.status != "active" {
        return error_json(StatusCode::FORBIDDEN, "tenant is not active");
    }
    // Schedule gate: activation start, expiry cutoff, and recurring weekly windows.
    if !resolved.internal {
        let now = chrono::Utc::now();
        if let Err(reason) = tenant_active_now(&resolved, now) {
            return error_json(StatusCode::FORBIDDEN, reason);
        }
        // Phase 5: warn operators when a tenant is within 72h of expiry.
        if let Some(until) = resolved.active_until {
            let remaining = until - now;
            if remaining > chrono::Duration::zero() && remaining <= chrono::Duration::hours(72) {
                state.alerts.issue(
                    format!("tenant_expiry:{}", resolved.tenant_id),
                    "Tenant access expiring soon",
                    format!(
                        "tenant `{}` expires at {} (~{}h remaining)",
                        resolved.tenant_name,
                        until.to_rfc3339(),
                        remaining.num_hours()
                    ),
                );
            }
        }
    }

    // ---- request flight-recorder tracer ----
    let mut tracer: Option<crate::tracer::SpanRecorder> = if resolved.tracing_enabled {
        tracing::debug!(request_id = %request_id, "tracing enabled — recording spans");
        Some(crate::tracer::SpanRecorder::new(
            request_id,
            proxy_start_ms,
            state.telemetry.clone(),
        ))
    } else {
        tracing::debug!(request_id = %request_id, "tracing disabled for this key");
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
            }),
        );
    }

    // ---- read + parse body ----
    let mut body_bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => return error_json(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };
    let mut json: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
    // Audio transcription/translation send the model as a `multipart/form-data`
    // field alongside the uploaded file, not as JSON. Parse the fields once so
    // we can resolve the model and later rebuild the upstream form with the
    // model name swapped.
    let content_type_in = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut multipart_fields =
        if is_multipart_endpoint(&path) && content_type_in.starts_with("multipart/form-data") {
            match multer::parse_boundary(&content_type_in) {
                Ok(boundary) => match parse_multipart(&body_bytes, &boundary).await {
                    Ok(fields) => Some(fields),
                    Err(_) => return error_json(StatusCode::BAD_REQUEST, "invalid multipart body"),
                },
                Err(_) => return error_json(StatusCode::BAD_REQUEST, "invalid multipart boundary"),
            }
        } else {
            None
        };

    let mut model = if let Some(fields) = &multipart_fields {
        fields
            .iter()
            .find(|f| f.name == "model")
            .and_then(|f| std::str::from_utf8(&f.data).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        json.get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string()
    };

    // ---- model listing (OpenAI `GET /v1/models`) ----
    // Answer the plain list call from the gateway's own registry so every
    // registered model — including Slurm-hosted ones on their own endpoints — is
    // listed, not just whatever a single upstream reports. A request that names a
    // model (the non-standard `{"model": …}` detail probe) falls through and is
    // forwarded upstream untouched, as does `GET /v1/models/{id}`.
    if method == Method::GET && path == "/v1/models" && model == "unknown" {
        return models_list_response(&state).await;
    }

    // Request-log metadata, captured once so every `finalize` path (cache hit,
    // rejection, upstream error, streamed success) records the same session and
    // request class.
    let conversation = resolve_conversation(
        &headers,
        &json,
        resolved.tenant_id,
        state.session_id_derivation,
    );
    let req_meta = RequestMeta {
        session_id: conversation.value,
        session_id_source: conversation.source.as_str(),
        request_type: effective_request_type(&resolved, &path),
    };
    // Surface the conversation id on the OTLP/Jaeger root span for cross-request
    // grouping (the field is declared Empty on the #[instrument] below).
    tracing::Span::current().record("session.id", req_meta.session_id.as_str());
    tracing::Span::current().record("session.id.source", req_meta.session_id_source);
    if let Some(t) = tracer.as_mut() {
        t.set_conversation(&req_meta.session_id, req_meta.session_id_source);
    }

    // Cost estimate, computed once per request body. Re-estimated below only
    // when a boon actually rewrites the body; the later upstream model-name
    // swap does not affect it (the tokenizer ignores the `model` field).
    let mut est = state.tokenizer.estimate_request(&json);

    // ---- auto model selection ----
    // `model: "auto"` is resolved to a concrete registered model from request
    // shape (estimated context size, required capabilities) and live load. From
    // here on, everything downstream — admission, budgets, caching, telemetry,
    // upstream dispatch — sees the concrete model as if the client named it.
    let auto_start = crate::tracer::now_ms();
    let route = if model == crate::router::AUTO_MODEL_NAME {
        let max_tokens = json.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let features = crate::router::RequestFeatures::from_request(
            &json,
            est.input_tokens as u64,
            max_tokens,
        );
        let candidates = state.model_registry.load();
        // Cheap shared-map read; the full scheduler snapshot is dashboard-only.
        let busyness = state.fairshare.model_load();
        let allowed = if resolved.internal {
            None
        } else {
            resolved.allowed_models.as_deref()
        };

        // Derive intent tags: classifier (when enabled + resolvable) first,
        // then cheap heuristics, then neutral capacity/cost routing.
        let available_tags = union_candidate_tags(&candidates, allowed);
        let desired_tags =
            derive_desired_tags(&state, &json, est.input_tokens as u64, &available_tags).await;

        // Boon-granted capabilities count as native in the hard filters: a
        // model carrying the structured_output boon can serve requests that
        // need it, because the boon engine emulates the capability.
        let grants = crate::router::BoonGrants::from_settings(&state.boons.settings());
        match crate::router::select_model(
            &candidates,
            &features,
            &busyness,
            allowed,
            &desired_tags,
            grants,
        ) {
            Some(chosen) => {
                tracing::debug!(chosen = %chosen.model_name, "auto-routed request");
                model = chosen.model_name.clone();
                if let Some(ref mut t) = tracer {
                    t.record(
                        "auto_route",
                        "proxy_request",
                        auto_start,
                        (crate::tracer::now_ms() - auto_start) as u32,
                        "ok",
                        serde_json::json!({
                            "chosen": chosen.model_name,
                            "candidates": candidates.len(),
                            "tags": desired_tags,
                        }),
                    );
                }
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
        resolve_model(&state, &model).await
    };

    if requires_registered_model(&path) {
        if model == "unknown" {
            return error_json(StatusCode::BAD_REQUEST, "model is required");
        }
        let Some(route) = route.as_ref() else {
            return error_json(
                StatusCode::NOT_FOUND,
                &format!("model '{model}' is not registered"),
            );
        };
        if !route.enabled {
            return error_json(StatusCode::FORBIDDEN, "model is disabled");
        }
    }
    // ---- per-tenant model allowlist (Phase 4) ----
    if !resolved.internal {
        if let Some(allowed) = &resolved.allowed_models {
            if !allowed.iter().any(|m| m == &model) {
                return error_json(StatusCode::FORBIDDEN, "model not permitted for tenant");
            }
        }
    }

    // ---- model boons (gateway-granted capabilities) ----
    // e.g. the vision boon rewrites image content into text descriptions for
    // models that lack native vision. Runs before estimation/caching/dispatch so
    // every downstream stage sees the rewritten body. Fail-open: on any error
    // the body is left unchanged. `x-obleth-boons: off` skips boons for one
    // request; the structured-output boon and the gateway tool loop may arm a
    // response plan that intercepts and rewrites the completion below (streaming
    // clients of the tool loop are driven live; see `stream_tap`).
    // `x-obleth-boons` is a comma-separated control list: `off` disables all
    // boons for the request; `lossy` forces the compression boon's lossy pass on
    // (for back-to-back A/B testing). `off` wins if both are present.
    let (boons_opt_out, boons_force_lossy) = headers
        .get(crate::boons::BOONS_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let mut opt_out = false;
            let mut force_lossy = false;
            for tok in v.split(',') {
                match tok.trim().to_ascii_lowercase().as_str() {
                    "off" => opt_out = true,
                    "lossy" => force_lossy = true,
                    _ => {}
                }
            }
            (opt_out, force_lossy)
        })
        .unwrap_or((false, false));
    let boon_outcome = state
        .boons
        .enrich_request(
            &state,
            route.as_deref(),
            &resolved,
            &req_meta.session_id,
            boons_opt_out,
            boons_force_lossy,
            req_meta.request_type == "chat",
            &mut json,
            tracer.as_mut(),
        )
        .await;
    if let Some(block) = boon_outcome.blocked {
        if let Some(t) = tracer.take() {
            t.finish("error");
        }
        return error_json(block.status, block.reason);
    }
    if boon_outcome.rewritten {
        match serde_json::to_vec(&json) {
            Ok(bytes) => body_bytes = Bytes::from(bytes),
            Err(e) => tracing::warn!(error = %e, "failed to re-serialize boon-rewritten body"),
        }
        // The body changed (e.g. images became text descriptions); the
        // admission estimate must reflect what is actually sent upstream.
        est = state.tokenizer.estimate_request(&json);
    }
    let boons_applied = boon_outcome.applied;
    // Compact compression summary for the `x-obleth-compression` response header,
    // so a back-to-back A/B client can diff savings without reading traces.
    let compression_header = boon_outcome.compression_tokens.map(|(before, after)| {
        format!(
            "before={};after={};saved={}",
            before,
            after,
            before.saturating_sub(after)
        )
    });
    let response_plan = boon_outcome.response_plan;

    // Live streaming tool loop: when the *only* response transform is the
    // gateway tool loop and the client asked to stream, keep the upstream call
    // streaming and drive it through `tool_stream`. Content/reasoning deltas
    // stream straight through, a visible marker shows when a gateway tool runs,
    // and only the tool execution between turns pauses the stream. The
    // structured-output transform still needs a fully buffered completion, so it
    // keeps forcing the upstream non-streaming.
    let stream_tap = response_plan.as_ref().is_some_and(|p| {
        p.tool_loop.is_some()
            && p.structured.is_none()
            && p.client_stream
            && p.guardrails
                .as_ref()
                .map(|g| matches!(g.policy.action, obleth_config::GuardrailsAction::LogOnly))
                .unwrap_or(true)
    }) && route.is_some();

    let effective_weight = effective_admission_weight(resolved.weight, route.as_deref());

    // Cost rates and per-request modality surcharge captured up front so every
    // `finalize` path (cache hit, rejection, upstream error, streamed success)
    // can freeze an identical USD cost, and so the post-stream term-usage commit
    // can reuse them after `route` is moved out of the request scope.
    let (in_cost_rate, out_cost_rate) = route
        .as_ref()
        .map(|r| (r.input_cost_per_token, r.output_cost_per_token))
        .unwrap_or((0.0, 0.0));
    let modality_cost = compute_modality_cost(route.as_deref(), &json);
    let energy_slots = route.as_ref().map(|r| r.energy_slots_per_node).unwrap_or(0);

    // ---- response cache (exact-match, before admission so hits cost nothing) ----
    // Tool-loop answers depend on live tool results (e.g. a web search), so a
    // cached answer would be wrong by definition: skip the cache entirely.
    let tool_loop_armed = response_plan
        .as_ref()
        .is_some_and(|p| p.tool_loop.is_some());
    // The response cache is keyed on (model, body) and shared across tenants. A
    // tenant with an output guardrails policy (block/redact) must never serve —
    // or populate — a shared cache entry, or it would bypass its own scanning by
    // replaying another tenant's un-scanned response. Disable the cache for the
    // request whenever output guardrails are armed.
    let output_guardrails_armed = response_plan
        .as_ref()
        .is_some_and(|p| p.guardrails.is_some());
    let cache_enabled = route.as_ref().map(|r| r.cache_enabled).unwrap_or(false)
        && !tool_loop_armed
        && !output_guardrails_armed;
    let cache_ttl = route.as_ref().map(|r| r.cache_ttl_secs).unwrap_or(0);
    let cache_key = cache_enabled.then(|| obleth_config::cache_key(&model, &body_bytes));
    if let Some(ck) = &cache_key {
        let cache_start = crate::tracer::now_ms();
        let cache_result = state
            .redis
            .cache_get(ck)
            .instrument(tracing::info_span!("cache_lookup"))
            .await;
        let cache_ms = (crate::tracer::now_ms() - cache_start) as u32;
        match cache_result {
            Ok(Some(cached)) => {
                if let Some(mut t) = tracer.take() {
                    t.record(
                        "cache_lookup",
                        "proxy_request",
                        cache_start,
                        cache_ms,
                        "hit",
                        serde_json::json!({
                            "result": "hit",
                            "tokens_saved": cached.input_tokens.saturating_add(cached.output_tokens),
                        }),
                    );
                    t.finish("ok");
                }
                state.metrics.record_cache(
                    true,
                    cached.input_tokens.saturating_add(cached.output_tokens),
                );
                finalize(
                    &state,
                    request_id,
                    &resolved,
                    &req_meta,
                    &model,
                    Admission::Fast,
                    est,
                    cached.input_tokens,
                    cached.output_tokens,
                    0,
                    0,
                    0,
                    cached.status,
                    "hit",
                    (cached.input_tokens as f64) * in_cost_rate
                        + (cached.output_tokens as f64) * out_cost_rate
                        + modality_cost,
                    crate::energy::EnergyFigures::default(),
                );
                return cached_response(cached, request_id);
            }
            Ok(None) => {
                if let Some(ref mut t) = tracer {
                    t.record(
                        "cache_lookup",
                        "proxy_request",
                        cache_start,
                        cache_ms,
                        "miss",
                        serde_json::json!({ "result": "miss" }),
                    );
                }
                state.metrics.record_cache(false, 0);
            }
            Err(e) => {
                if let Some(ref mut t) = tracer {
                    t.record(
                        "cache_lookup",
                        "proxy_request",
                        cache_start,
                        cache_ms,
                        "error",
                        serde_json::json!({}),
                    );
                }
                tracing::warn!(error = %e, "cache lookup failed; treating as miss");
                state.alerts.issue(
                    "redis_cache_lookup_failed",
                    "Redis response-cache lookup failed",
                    format!("model `{model}` tenant `{}`: {e}", resolved.tenant_name),
                );
            }
        }
    }
    // Telemetry label for everything that isn't a cache hit.
    let cache_status_label = if cache_enabled { "miss" } else { "off" };

    // ---- fairshare admission (global concurrency + weighted/hierarchical queue) ----
    let admission_start = crate::tracer::now_ms();
    let admitted = match state
        .fairshare
        .admit(obleth_fairshare::AdmitRequest {
            tenant: resolved.tenant_id,
            weight: effective_weight,
            group: resolved.fairshare_group.clone(),
            group_weight: resolved.group_weight,
            model: model.clone(),
            model_max_in_flight: route.as_ref().and_then(|r| r.max_in_flight),
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
                    "tenant `{}` model `{model}` path `{path}`",
                    resolved.tenant_name
                ),
            );
            return error_json(StatusCode::SERVICE_UNAVAILABLE, "scheduler unavailable");
        }
    };
    let admission = admitted.admission;
    let permit = admitted.permit;
    let queue_wait_ms = admitted.waited.as_millis() as u32;
    let admission_ms = (crate::tracer::now_ms() - admission_start) as u32;
    if let Some(ref mut t) = tracer {
        t.record(
            "admission",
            "proxy_request",
            admission_start,
            admission_ms,
            "ok",
            serde_json::json!({
                "decision": admission.as_str(),
                "queue_wait_ms": queue_wait_ms,
            }),
        );
    }

    let send_bytes = body_bytes;

    // ---- token budget reserve + cumulative term gate (atomic, cross-pod) ----
    // One Redis round trip covers both checks. The term gate (Phase 3: caps on
    // lifetime/monthly/term usage) runs first inside the script, so a
    // term-exhausted request never reserves per-minute tokens it has no
    // completion path to refund.
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
    if let Some(gate) = key_term_gate {
        match state
            .redis
            .reserve_budget_with_term(&resolved.key_id, 0, 0, est.total(), Some(gate))
            .instrument(tracing::info_span!("reserve_key_budget"))
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
                    403,
                    cache_status_label,
                    0.0,
                    crate::energy::EnergyFigures::default(),
                );
                if let Some(t) = tracer.take() {
                    t.finish("error");
                }
                return error_json(StatusCode::FORBIDDEN, "api key term budget exhausted");
            }
            Err(e) => {
                if !state.fail_open {
                    drop(permit);
                    state.alerts.issue(
                        "redis_key_budget_reserve_failed_closed",
                        "Redis key-budget reserve failed",
                        format!(
                            "fail-open is disabled; rejecting tenant `{}` key `{}` model `{model}`: {e}",
                            resolved.tenant_name,
                            resolved.key_id
                        ),
                    );
                    if let Some(t) = tracer.take() {
                        t.finish("error");
                    }
                    return error_json(StatusCode::SERVICE_UNAVAILABLE, "key budget check failed");
                }
                tracing::warn!(error = %e, "key budget reserve failed; failing open");
                state.alerts.issue(
                    "redis_key_budget_reserve_failed_open",
                    "Redis key-budget reserve failed",
                    format!(
                        "fail-open is enabled; admitting tenant `{}` key `{}` model `{model}` without key budget enforcement: {e}",
                        resolved.tenant_name,
                        resolved.key_id
                    ),
                );
            }
        }
    }
    let should_check_budget = capacity > 0 || term_gate.is_some();
    if should_check_budget {
        match state
            .redis
            .reserve_budget_with_term(
                &resolved.tenant_id,
                capacity,
                resolved.tokens_per_minute,
                est.total(),
                term_gate,
            )
            .instrument(tracing::info_span!("reserve_budget"))
            .await
        {
            Ok(obleth_redis::ReserveOutcome::Reserved { .. }) => {}
            Ok(obleth_redis::ReserveOutcome::RateLimited { .. }) => {
                drop(permit);
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
                    429,
                    cache_status_label,
                    0.0,
                    crate::energy::EnergyFigures::default(),
                );
                if let Some(t) = tracer.take() {
                    t.finish("error");
                }
                return error_json(StatusCode::TOO_MANY_REQUESTS, "token budget exceeded");
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
                    403,
                    cache_status_label,
                    0.0,
                    crate::energy::EnergyFigures::default(),
                );
                if let Some(t) = tracer.take() {
                    t.finish("error");
                }
                return error_json(StatusCode::FORBIDDEN, "tenant term budget exhausted");
            }
            Err(e) => {
                if !state.fail_open {
                    drop(permit);
                    state.alerts.issue(
                        "redis_budget_reserve_failed_closed",
                        "Redis budget reserve failed",
                        format!(
                            "fail-open is disabled; rejecting tenant `{}` model `{model}`: {e}",
                            resolved.tenant_name
                        ),
                    );
                    if let Some(t) = tracer.take() {
                        t.finish("error");
                    }
                    return error_json(StatusCode::SERVICE_UNAVAILABLE, "budget check failed");
                }
                tracing::warn!(error = %e, "budget reserve failed; failing open");
                state.alerts.issue(
                    "redis_budget_reserve_failed_open",
                    "Redis budget reserve failed",
                    format!(
                        "fail-open is enabled; admitting tenant `{}` model `{model}` without budget enforcement: {e}",
                        resolved.tenant_name
                    ),
                );
            }
        }
    }

    // ---- proxy upstream ----
    // Resolve the per-request timeout and retry policy. Both default to the
    // model-level config, falling back to the global gateway settings.
    let mut req_timeout = route
        .as_ref()
        .and_then(|r| r.request_timeout_secs)
        .filter(|s| *s >= 1)
        .map(|s| Duration::from_secs(s as u64))
        .unwrap_or(state.upstream_timeout);
    // With a response-transforming boon the upstream call is non-streaming, so
    // the send timeout covers the entire generation, not just the headers.
    if response_plan.is_some() {
        req_timeout = req_timeout.max(BOON_MIN_TIMEOUT);
    }
    let max_retries = route.as_ref().map(|r| r.max_retries.max(0)).unwrap_or(0);
    let backoff = Duration::from_millis(
        route
            .as_ref()
            .map(|r| r.retry_backoff_ms.max(0) as u64)
            .unwrap_or(0),
    );
    let selection_mode = route
        .as_ref()
        .map(|r| r.endpoint_selection_mode.as_str())
        .unwrap_or(obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE);

    // Build the ordered list of upstream targets. When a model defines explicit
    // endpoints we route across the healthy/enabled ones (priority order for
    // failover, weighted order for load_balance); otherwise we fall back to the
    // legacy single api_base/api_key on the model (or the global default).
    let targets = build_targets(
        route.as_deref(),
        &state.upstream_base,
        selection_mode,
        &req_meta.session_id,
    );

    // The JSON body is rebuilt once and replayed on each attempt/endpoint.
    // Multipart bodies cannot be replayed, so they get a single attempt against
    // the first target only.
    let replayable = multipart_fields.is_none();
    let prepared_body: Option<Bytes> = replayable.then(|| {
        prepare_upstream_body(
            route.as_deref(),
            &mut json,
            send_bytes,
            response_plan.is_some() && !stream_tap,
            stream_tap,
        )
    });

    // TTFT is measured from the moment we dispatch the *successful* upstream
    // request, *after* fairshare admission. Time spent waiting in the queue is
    // reported separately as `queue_wait_ms`; folding it into TTFT would
    // double-count the wait and make a fast model look slow under contention.
    let dispatch_start = crate::tracer::now_ms();
    let mut upstream_start = Instant::now();
    let mut upstream_resp: Option<reqwest::Response> = None;
    let mut last_url = String::new();
    let mut last_err: Option<String> = None;
    let mut timed_out = false;
    let total_targets = targets.len();

    'targets: for (ti, target) in targets.iter().enumerate() {
        let url = build_upstream_url(&target.base, &path, &query);
        last_url = url.clone();
        // Base budget = configured retries + 1. Plus one extra slot reserved for
        // connection-level failures: a reused keep-alive socket the upstream
        // already closed surfaces as a send error with no response, so the request
        // never executed and is safe to retry on a fresh connection even when
        // max_retries == 0. The bonus slot is only ever consumed by a connection
        // error — status/timeout failures break out to failover instead.
        let base_attempts: u32 = if replayable {
            max_retries as u32 + 1
        } else {
            1
        };
        let attempts: u32 = base_attempts + if replayable { 1 } else { 0 };
        for attempt in 0..attempts {
            let configured_left = attempt + 1 < base_attempts;
            let bonus_left = attempt + 1 < attempts;
            let more_targets = replayable && ti + 1 < total_targets;

            let mut fwd_headers = forward_headers(&headers);
            if let Some(key) = &target.api_key {
                if let Ok(v) = header::HeaderValue::from_str(&format!("Bearer {key}")) {
                    fwd_headers.insert(header::AUTHORIZATION, v);
                }
            }
            let req_builder = if let Some(body) = &prepared_body {
                state
                    .http
                    .request(method.clone(), &url)
                    .headers(fwd_headers)
                    .body(body.clone())
            } else {
                // Rebuild the multipart form, swapping the client model name for
                // the upstream one. reqwest regenerates the Content-Type (with
                // boundary), so the inbound multipart content-type must be dropped.
                fwd_headers.remove(header::CONTENT_TYPE);
                let upstream_model = route
                    .as_ref()
                    .map(|r| r.upstream_model.clone())
                    .unwrap_or_else(|| model.clone());
                // Multipart bodies are not replayable, so this branch only runs on
                // the single attempt against the first target (`replayable` is false
                // ⇒ one attempt, no failover). The `take` therefore yields `Some`
                // exactly once; we still handle `None` gracefully instead of
                // panicking should that invariant ever change.
                let Some(fields) = multipart_fields.take() else {
                    return error_json(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "multipart request body was already consumed",
                    );
                };
                let form = build_multipart_form(fields, &upstream_model);
                state
                    .http
                    .request(method.clone(), &url)
                    .headers(fwd_headers)
                    .multipart(form)
            };

            upstream_start = Instant::now();
            let send_fut = req_builder
                .send()
                .instrument(tracing::info_span!("upstream_request"));
            match timeout(req_timeout, send_fut).await {
                Ok(Ok(resp)) => {
                    let status = resp.status().as_u16();
                    if is_retryable_status(status) && (configured_left || more_targets) {
                        last_err = Some(format!("upstream status {status}"));
                        if configured_left {
                            state.metrics.record_upstream_attempt("retry");
                            tokio::time::sleep(backoff_for(backoff, attempt)).await;
                            continue;
                        }
                        state.metrics.record_upstream_attempt("failover");
                        continue 'targets;
                    }
                    state.metrics.record_upstream_attempt("success");
                    upstream_resp = Some(resp);
                    break 'targets;
                }
                Ok(Err(e)) => {
                    let connection_error = is_connection_error(&e);
                    last_err = Some(e.to_string());
                    if configured_left {
                        state.metrics.record_upstream_attempt("retry");
                        tokio::time::sleep(backoff_for(backoff, attempt)).await;
                        continue;
                    }
                    // Configured retries are spent, but a connection-level send
                    // failure never reached the upstream (e.g. a stale pooled
                    // keep-alive socket): spend the reserved bonus slot on one
                    // fresh-connection retry before failing over.
                    if connection_error && bonus_left {
                        state.metrics.record_upstream_attempt("conn_retry");
                        tokio::time::sleep(CONN_RETRY_BACKOFF).await;
                        continue;
                    }
                    break;
                }
                Err(_) => {
                    timed_out = true;
                    last_err = Some(format!("timed out after {req_timeout:?}"));
                    if configured_left {
                        state.metrics.record_upstream_attempt("timeout");
                        tokio::time::sleep(backoff_for(backoff, attempt)).await;
                        continue;
                    }
                    break;
                }
            }
        }
        // Attempts on this target are exhausted. Fail over to the next target
        // when one is available and the request is replayable.
        if replayable && ti + 1 < total_targets {
            state.metrics.record_upstream_attempt("failover");
            continue 'targets;
        }
        break;
    }

    let url = last_url;
    let dispatch_ms = (crate::tracer::now_ms() - dispatch_start) as u32;
    let upstream = match upstream_resp {
        Some(r) => r,
        None => {
            if let Some(mut t) = tracer.take() {
                t.record(
                    "upstream",
                    "proxy_request",
                    dispatch_start,
                    dispatch_ms,
                    "error",
                    serde_json::json!({
                        "model": model,
                        "url": url.as_str(),
                        "targets": total_targets,
                        "error": last_err.clone().unwrap_or_default(),
                    }),
                );
                // Opt-in upstream diagnostics: only when this model has the flag
                // on (and we're already tracing). Bounded, read-only DNS + TCP
                // probe recorded as its own span before the trace is finished —
                // it never alters the 502/504 returned below.
                if route.as_ref().map(|r| r.debug_diagnostics).unwrap_or(false) {
                    let diag_start = crate::tracer::now_ms();
                    let diag = crate::diagnostics::probe_upstream(
                        &url,
                        &last_err.clone().unwrap_or_default(),
                        std::time::Duration::from_millis(1500),
                    )
                    .await;
                    let diag_ms = (crate::tracer::now_ms() - diag_start) as u32;
                    let status = if diag.dns.ok && diag.tcp.ok {
                        "ok"
                    } else {
                        "error"
                    };
                    t.record(
                        "upstream_diagnostics",
                        "proxy_request",
                        diag_start,
                        diag_ms,
                        status,
                        serde_json::to_value(&diag).unwrap_or_default(),
                    );
                }
                t.finish("error");
            }
            state.metrics.record_upstream_attempt("exhausted");
            let msg = last_err.unwrap_or_else(|| "upstream request failed".to_string());
            tracing::warn!(error = %msg, "upstream request failed after all attempts");
            state.alerts.issue(
                "upstream_request_failed",
                "Upstream request failed",
                format!(
                    "tenant `{}` model `{model}` path `{path}` upstream `{url}`: {msg}",
                    resolved.tenant_name
                ),
            );
            drop(permit);
            let (code, http_status) = if timed_out {
                (504u16, StatusCode::GATEWAY_TIMEOUT)
            } else {
                (502u16, StatusCode::BAD_GATEWAY)
            };
            finalize(
                &state,
                request_id,
                &resolved,
                &req_meta,
                &model,
                admission,
                est,
                0,
                0,
                queue_wait_ms,
                0,
                0,
                code,
                cache_status_label,
                0.0,
                crate::energy::EnergyFigures::default(),
            );
            let detail = if timed_out {
                "upstream request timed out"
            } else {
                "upstream request failed"
            };
            return error_json(http_status, detail);
        }
    };
    let status_code = upstream.status().as_u16();
    if status_code >= 500 {
        state.alerts.issue(
            "upstream_5xx_response",
            "Upstream returned a server error",
            format!(
                "tenant `{}` model `{model}` path `{path}` upstream `{url}` status `{status_code}`",
                resolved.tenant_name
            ),
        );
    }
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| header::HeaderValue::from_static("application/json"));
    let content_type_str = content_type
        .to_str()
        .unwrap_or("application/json")
        .to_string();

    // An upstream error (4xx/5xx) is a small, non-streaming JSON body, so buffer
    // it and fold it into the trace's `upstream` span — otherwise the tracer only
    // ever sees a bare status code and operators can't tell *why* a backend
    // rejected the request (e.g. vLLM's "model `x` does not exist" or a
    // context-length 400). The body is then replayed to the client verbatim.
    // (4xx never reaches the streaming/boon paths below — they short-circuit
    // here — so the only behaviour change is the captured body + buffered reply.)
    if status_code >= 400 {
        let mut buf: Vec<u8> = Vec::new();
        let mut ttft_ms = 0u32;
        let mut byte_stream = upstream.bytes_stream();
        while let Some(item) = byte_stream.next().await {
            match item {
                Ok(chunk) => {
                    if buf.is_empty() && !chunk.is_empty() {
                        ttft_ms = upstream_start.elapsed().as_millis() as u32;
                    }
                    let room = BODY_LIMIT.saturating_sub(buf.len());
                    if chunk.len() > room {
                        buf.extend_from_slice(&chunk[..room]);
                        break;
                    }
                    buf.extend_from_slice(&chunk);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "upstream read failed reading error body");
                    break;
                }
            }
        }
        drop(permit);

        if let Some(ref mut t) = tracer {
            let body = String::from_utf8_lossy(&buf);
            let snippet: String = body.chars().take(ERROR_BODY_TRACE_CAP).collect();
            t.record(
                "upstream",
                "proxy_request",
                dispatch_start,
                dispatch_ms,
                "error",
                serde_json::json!({
                    "model": model,
                    "url": url.as_str(),
                    "status": status_code,
                    "targets": total_targets,
                    "body": snippet,
                }),
            );
        }

        // Mirror the streaming pass-through's accounting for a non-200: usage is
        // whatever the (error) body reports, falling back to the admission
        // estimate. Errors are never cached.
        let (input_tokens, output_tokens) = extract_usage(&String::from_utf8_lossy(&buf))
            .unwrap_or((est.input_tokens, est.estimated_output_tokens));
        let total_ms = request_start.elapsed().as_millis() as u32;
        settle_request(
            &state,
            request_id,
            &resolved,
            &req_meta,
            &model,
            admission,
            est,
            input_tokens,
            output_tokens,
            queue_wait_ms,
            ttft_ms,
            total_ms,
            status_code,
            cache_status_label,
            capacity,
            term_period.as_deref(),
            key_term_period.as_deref(),
            in_cost_rate,
            out_cost_rate,
            modality_cost,
            energy_slots,
            None,
        )
        .await;
        if let Some(t) = tracer.take() {
            t.finish("error");
        }

        let mut builder = Response::builder()
            .status(status_code)
            .header(header::CONTENT_TYPE, content_type)
            .header("x-obleth-request-id", request_id.to_string())
            .header(NO_BUFFER_HEADER.0, NO_BUFFER_HEADER.1);
        if !boons_applied.is_empty() {
            builder = builder.header(crate::boons::BOONS_HEADER, boons_applied.join(","));
            if let Some(h) = &compression_header {
                builder = builder.header(crate::boons::COMPRESSION_HEADER, h);
            }
        }
        return builder.body(Body::from(buf)).unwrap_or_else(|_| {
            error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
        });
    }

    // Success: record the `upstream` span here (the error path above records its
    // own, enriched with the body).
    if let Some(ref mut t) = tracer {
        t.record(
            "upstream",
            "proxy_request",
            dispatch_start,
            dispatch_ms,
            "ok",
            serde_json::json!({
                "model": model,
                "url": url.as_str(),
                "status": status_code,
                "targets": total_targets,
            }),
        );
    }

    // Cache only successful responses.
    let store_in_cache = cache_key.clone();

    // ---- streaming gateway tool loop ----
    // When the only response transform is the tool loop and the client asked to
    // stream, drive the loop live (see `stream_tap`): the model's content and
    // reasoning stream straight through, a visible marker is shown when a
    // gateway tool runs, and only the tool execution between turns pauses the
    // stream. The first turn reuses the upstream response already opened above.
    if stream_tap && status_code == 200 {
        if let (Some(plan), Some(route_owned)) = (response_plan.as_ref(), route.clone()) {
            if let Some(loop_plan) = &plan.tool_loop {
                let stats = std::sync::Arc::new(std::sync::Mutex::new(
                    crate::boons::tool_stream::StreamStats::default(),
                ));
                let driver = crate::boons::tool_stream::run(
                    crate::boons::tool_stream::StreamLoop {
                        state: state.clone(),
                        route: (*route_owned).clone(),
                        key: (*resolved).clone(),
                        session_id: req_meta.session_id.clone(),
                        base_request: loop_plan.request.clone(),
                        tool_servers: loop_plan.tool_servers.clone(),
                        settings: loop_plan.settings.clone(),
                        passthrough_unmapped: loop_plan.passthrough_unmapped,
                        dispatch_timeout: req_timeout,
                        client_include_usage: plan.include_usage,
                        upstream_start,
                    },
                    upstream,
                    stats.clone(),
                );

                let stream_state = state.clone();
                let resolved_for_stream = resolved.clone();
                let meta_for_stream = req_meta.clone();
                let model_for_stream = model.clone();
                let term_for_stream = term_period.clone();
                let key_term_for_stream = key_term_period.clone();
                let body_stream = async_stream::stream! {
                    futures_util::pin_mut!(driver);
                    while let Some(item) = driver.next().await {
                        yield item;
                    }
                    drop(permit);
                    let (ttft_ms, input_tokens, output_tokens) = {
                        let s = stats.lock().unwrap_or_else(|e| e.into_inner());
                        let toks = if s.final_set {
                            (s.input_tokens, s.output_tokens)
                        } else {
                            (est.input_tokens, est.estimated_output_tokens)
                        };
                        (s.ttft_ms, toks.0, toks.1)
                    };
                    let total_ms = request_start.elapsed().as_millis() as u32;
                    settle_request(
                        &stream_state,
                        request_id,
                        &resolved_for_stream,
                        &meta_for_stream,
                        &model_for_stream,
                        admission,
                        est,
                        input_tokens,
                        output_tokens,
                        queue_wait_ms,
                        ttft_ms,
                        total_ms,
                        status_code,
                        cache_status_label,
                        capacity,
                        term_for_stream.as_deref(),
                        key_term_for_stream.as_deref(),
                        in_cost_rate,
                        out_cost_rate,
                        modality_cost,
                        energy_slots,
                        None,
                    )
                    .await;
                };
                if let Some(t) = tracer.take() {
                    t.finish(if status_code < 400 { "ok" } else { "error" });
                }
                let mut builder = Response::builder()
                    .status(status_code)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .header("x-obleth-request-id", request_id.to_string())
                    .header(NO_BUFFER_HEADER.0, NO_BUFFER_HEADER.1);
                if !boons_applied.is_empty() {
                    builder = builder.header(crate::boons::BOONS_HEADER, boons_applied.join(","));
                    if let Some(h) = &compression_header {
                        builder = builder.header(crate::boons::COMPRESSION_HEADER, h);
                    }
                }
                return builder
                    .body(Body::from_stream(body_stream))
                    .unwrap_or_else(|_| {
                        error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
                    });
            }
        }
    }

    // ---- boon response interception (structured output / buffered tool loop) ----
    // The upstream call was forced non-streaming; buffer the completion,
    // transform it, and reply — synthesizing SSE when the client asked for a
    // stream. Fail-open: a body that can't be buffered or parsed passes
    // through verbatim. Non-200 responses skip transformation entirely and
    // fall through to the normal pass-through path below.
    if let Some(plan) = response_plan.filter(|_| status_code == 200) {
        // Buffer the upstream body, recording TTFT at the first byte for
        // metric continuity with the streaming path.
        let mut buf: Vec<u8> = Vec::new();
        let mut ttft_ms = 0u32;
        let mut truncated = false;
        let mut byte_stream = upstream.bytes_stream();
        while let Some(item) = byte_stream.next().await {
            match item {
                Ok(chunk) => {
                    if buf.is_empty() && !chunk.is_empty() {
                        ttft_ms = upstream_start.elapsed().as_millis() as u32;
                    }
                    if buf.len().saturating_add(chunk.len()) > BODY_LIMIT {
                        truncated = true;
                        break;
                    }
                    buf.extend_from_slice(&chunk);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "upstream read failed during boon interception");
                    truncated = true;
                    break;
                }
            }
        }

        // Parse + transform. Oversized, truncated, or unparseable bodies pass
        // through unchanged (fail-open); only well-formed completions are
        // rewritten.
        let mut warning: Option<&'static str> = None;
        let mut completion: Option<serde_json::Value> = (!truncated
            && buf.len() <= BOON_BUFFER_MAX)
            .then(|| serde_json::from_slice::<serde_json::Value>(&buf).ok())
            .flatten();
        let (final_body, final_content_type): (Bytes, String) = match completion.as_mut() {
            Some(body_json) => {
                // Tool-loop and repair calls run while the fairshare permit is
                // still held — they occupy upstream capacity just like the
                // original call. `tool_loop::run` falls through to the plain
                // boon transform when no tool loop is armed.
                let outcome = crate::boons::tool_loop::run(
                    &state,
                    &plan,
                    route.as_deref(),
                    &resolved,
                    &req_meta.session_id,
                    req_timeout,
                    body_json,
                    tracer.as_mut(),
                )
                .await;
                warning = outcome.warning;
                // guardrails output scan (block/redact action)
                if let Some(guard_plan) = &plan.guardrails {
                    match crate::boons::guardrails::apply_output(
                        &state,
                        &guard_plan.settings,
                        &guard_plan.policy,
                        &resolved,
                        &req_meta.session_id,
                        body_json,
                        tracer.as_mut(),
                    )
                    .await
                    {
                        crate::boons::guardrails::ApplyOutputResult::Block(block) => {
                            drop(permit);
                            if let Some(t) = tracer.take() {
                                t.finish("error");
                            }
                            return error_json(block.status, block.reason);
                        }
                        crate::boons::guardrails::ApplyOutputResult::Pass => {}
                    }
                }
                if plan.client_stream {
                    (
                        Bytes::from(crate::boons::respond::synthesize_sse(
                            body_json,
                            plan.include_usage,
                        )),
                        "text/event-stream".to_string(),
                    )
                } else {
                    (
                        serde_json::to_vec(body_json)
                            .map(Bytes::from)
                            .unwrap_or_else(|_| Bytes::from(std::mem::take(&mut buf))),
                        content_type_str.clone(),
                    )
                }
            }
            None => (
                Bytes::from(std::mem::take(&mut buf)),
                content_type_str.clone(),
            ),
        };
        drop(permit);

        let (input_tokens, output_tokens) = completion
            .as_ref()
            .and_then(|c| {
                let input = c.pointer("/usage/prompt_tokens")?.as_u64()? as u32;
                let output = c
                    .pointer("/usage/completion_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32;
                Some((input, output))
            })
            .unwrap_or((est.input_tokens, est.estimated_output_tokens));
        let total_ms = request_start.elapsed().as_millis() as u32;

        // Cache the *transformed* body so cache hits replay exactly what the
        // client received. The cache key was computed from the client body
        // (original `stream` flag included), so JSON and SSE representations
        // never collide.
        let cache_body = (!truncated && final_body.len() <= CACHE_MAX_BYTES)
            .then(|| String::from_utf8_lossy(&final_body).into_owned());
        let cache_put = match (&store_in_cache, cache_body) {
            (Some(ck), Some(body)) => {
                Some((ck.as_str(), cache_ttl, final_content_type.as_str(), body))
            }
            _ => None,
        };
        settle_request(
            &state,
            request_id,
            &resolved,
            &req_meta,
            &model,
            admission,
            est,
            input_tokens,
            output_tokens,
            queue_wait_ms,
            ttft_ms,
            total_ms,
            status_code,
            cache_status_label,
            capacity,
            term_period.as_deref(),
            key_term_period.as_deref(),
            in_cost_rate,
            out_cost_rate,
            modality_cost,
            energy_slots,
            cache_put,
        )
        .await;

        let mut builder = Response::builder()
            .status(status_code)
            .header(header::CONTENT_TYPE, final_content_type)
            .header("x-obleth-request-id", request_id.to_string())
            .header(NO_BUFFER_HEADER.0, NO_BUFFER_HEADER.1);
        if !boons_applied.is_empty() {
            builder = builder.header(crate::boons::BOONS_HEADER, boons_applied.join(","));
            if let Some(h) = &compression_header {
                builder = builder.header(crate::boons::COMPRESSION_HEADER, h);
            }
        }
        if let Some(w) = warning {
            builder = builder.header(crate::boons::BOONS_WARNING_HEADER, w);
        }
        if let Some(t) = tracer.take() {
            t.finish(if status_code < 400 { "ok" } else { "error" });
        }
        return builder.body(Body::from(final_body)).unwrap_or_else(|_| {
            error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
        });
    }

    // Finish the tracer before entering the async stream body — the stream macro
    // cannot capture a non-Clone, non-Send value.
    if let Some(t) = tracer.take() {
        t.finish(if status_code < 400 { "ok" } else { "error" });
    }

    // Extract guardrails policy for log_only output scanning (evaluated after stream drains).
    let scan_policy = resolved
        .guardrails_policy
        .as_ref()
        .filter(|p| {
            !p.output_scanners.is_empty()
                && matches!(p.action, obleth_config::GuardrailsAction::LogOnly)
        })
        .cloned();
    let scan_output = scan_policy.is_some();

    // ---- stream back, inspecting for actual usage, then reconcile ----
    let stream_state = state.clone();
    let resolved_for_stream = resolved.clone();
    let meta_for_stream = req_meta.clone();
    let body_stream = async_stream::stream! {
        let mut byte_stream = upstream.bytes_stream();
        let mut first = true;
        let mut ttft_ms = 0u32;
        let mut tail: Vec<u8> = Vec::with_capacity(TAIL_CAP.min(4 * 1024));
        let mut full: Vec<u8> = Vec::new();
        let mut cacheable = store_in_cache.is_some();

        while let Some(item) = byte_stream.next().await {
            match item {
                Ok(chunk) => {
                    if first {
                        ttft_ms = upstream_start.elapsed().as_millis() as u32;
                        first = false;
                    }
                    append_tail(&mut tail, &chunk);
                    if cacheable || scan_output {
                        if full.len() + chunk.len() <= CACHE_MAX_BYTES {
                            full.extend_from_slice(&chunk);
                        } else {
                            cacheable = false;
                            // Response exceeded cache cap; clear so the log_only scan is skipped.
                            full = Vec::new();
                        }
                    }
                    yield Ok::<Bytes, std::io::Error>(chunk);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "upstream stream error");
                    stream_state.alerts.issue(
                        "upstream_stream_error",
                        "Upstream stream failed",
                        format!(
                            "tenant `{}` model `{model}` status `{status_code}`: {e}",
                            resolved_for_stream.tenant_name
                        ),
                    );
                    cacheable = false;
                    break;
                }
            }
        }

        // The upstream stream has fully drained: the request no longer occupies
        // upstream capacity, so release the fairshare slot *before* the Redis
        // bookkeeping below. Cache stores and budget reconciliation are
        // accounting, not occupancy; holding the permit through them would
        // shrink effective concurrency whenever Redis is slow.
        drop(permit);

        let (input_tokens, output_tokens) = extract_usage(&String::from_utf8_lossy(&tail))
            .unwrap_or((est.input_tokens, est.estimated_output_tokens));
        let total_ms = request_start.elapsed().as_millis() as u32;

        // Capture for log_only scan before cache_put potentially consumes full.
        let scan_full: Vec<u8> = if scan_output && !full.is_empty() && status_code == 200 {
            full.clone()
        } else {
            Vec::new()
        };

        // store the full response for identical future requests
        let cache_put = if cacheable && status_code == 200 {
            store_in_cache.as_deref().map(|ck| {
                // Take ownership of the buffer instead of copying it; the
                // lossy re-encode only runs for invalid UTF-8 (never for the
                // JSON/SSE bodies this cache is meant for).
                let body = String::from_utf8(std::mem::take(&mut full))
                    .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                (ck, cache_ttl, content_type_str.as_str(), body)
            })
        } else {
            None
        };

        settle_request(
            &stream_state,
            request_id,
            &resolved_for_stream,
            &meta_for_stream,
            &model,
            admission,
            est,
            input_tokens,
            output_tokens,
            queue_wait_ms,
            ttft_ms,
            total_ms,
            status_code,
            cache_status_label,
            capacity,
            term_period.as_deref(),
            key_term_period.as_deref(),
            in_cost_rate,
            out_cost_rate,
            modality_cost,
            energy_slots,
            cache_put,
        )
        .await;

        // log_only guardrails output scan — fire-and-forget after stream drains.
        // tier-1 detection runs inline (microseconds); the harm scan, if armed,
        // is dispatched async by `monitor_output`.
        if let Some(policy_clone) = scan_policy {
            if !scan_full.is_empty() {
                if let Ok(completion) =
                    serde_json::from_slice::<serde_json::Value>(&scan_full)
                {
                    let guardrails_settings =
                        stream_state.boons.settings().guardrails.clone();
                    crate::boons::guardrails::monitor_output(
                        &stream_state,
                        &guardrails_settings,
                        &policy_clone,
                        &resolved_for_stream,
                        &meta_for_stream.session_id,
                        request_id,
                        &completion,
                    );
                }
            }
        }
    };

    let mut builder = Response::builder().status(status_code);
    builder = builder
        .header(header::CONTENT_TYPE, content_type)
        .header("x-obleth-request-id", request_id.to_string())
        .header(NO_BUFFER_HEADER.0, NO_BUFFER_HEADER.1);
    if !boons_applied.is_empty() {
        builder = builder.header(crate::boons::BOONS_HEADER, boons_applied.join(","));
        if let Some(h) = &compression_header {
            builder = builder.header(crate::boons::COMPRESSION_HEADER, h);
        }
    }
    builder
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

/// End-of-request bookkeeping shared by the streaming pass-through path and
/// the boon interception path: cache store, per-minute budget reconciliation,
/// term-usage commit + budget alerts, and the usage-ledger record.
///
/// `cache_put` carries `(key, ttl, content_type, body)` when the response
/// should be stored for identical future requests; callers gate it on a
/// successful (200) response.
#[allow(clippy::too_many_arguments)]
async fn settle_request(
    state: &AppState,
    request_id: Uuid,
    resolved: &ResolvedKey,
    meta: &RequestMeta,
    model: &str,
    admission: Admission,
    est: CostEstimate,
    input_tokens: u32,
    output_tokens: u32,
    queue_wait_ms: u32,
    ttft_ms: u32,
    total_ms: u32,
    status_code: u16,
    cache_status: &str,
    capacity: i64,
    term_period: Option<&str>,
    key_term_period: Option<&str>,
    in_cost_rate: f64,
    out_cost_rate: f64,
    modality_cost: f64,
    energy_slots: i64,
    cache_put: Option<(&str, i64, &str, String)>,
) {
    // store the full response for identical future requests
    if let Some((ck, ttl, content_type, body)) = cache_put {
        let cached = obleth_config::CachedResponse {
            status: status_code,
            content_type: content_type.to_string(),
            body,
            input_tokens,
            output_tokens,
        };
        if let Err(e) = state.redis.cache_put(ck, &cached, ttl).await {
            tracing::warn!(error = %e, "cache store failed");
            state.alerts.issue(
                "redis_cache_store_failed",
                "Redis response-cache store failed",
                format!(
                    "tenant `{}` model `{model}` cache ttl `{ttl}`: {e}",
                    resolved.tenant_name
                ),
            );
        }
    }

    // Reconcile estimate vs actual against the per-minute budget bucket.
    // A zero token rate means the tenant has no per-minute limiter.
    if capacity > 0 {
        if let Err(e) = state
            .redis
            .reconcile_budget(
                &resolved.tenant_id,
                capacity,
                est.total(),
                input_tokens.saturating_add(output_tokens),
            )
            .await
        {
            tracing::warn!(error = %e, "budget reconcile failed");
            state.alerts.issue(
                "redis_budget_reconcile_failed",
                "Redis budget reconcile failed",
                format!(
                    "tenant `{}` model `{model}` estimated `{}` actual `{}`: {e}",
                    resolved.tenant_name,
                    est.total(),
                    input_tokens.saturating_add(output_tokens),
                ),
            );
        }
    }

    // ---- term-usage commit + budget alerts (Phase 3 + Phase 5) ----
    // Frozen request cost: per-token rates (captured at admission) plus any
    // per-request modality surcharge. Computed once and used for both the
    // term-budget commit and the persisted usage ledger so they agree.
    let cost_usd = (input_tokens as f64) * in_cost_rate
        + (output_tokens as f64) * out_cost_rate
        + modality_cost;
    // Frozen energy figures: slot-share of live cluster power over serving
    // time (queue wait excluded). Zeros when accounting is off. Frozen like
    // `cost_usd` so later settings edits never rewrite history.
    let energy = state.energy.compute(energy_slots, total_ms, queue_wait_ms);
    if let Some(period) = term_period {
        let added = input_tokens.saturating_add(output_tokens) as i64;
        match state
            .redis
            .term_usage_add(&resolved.tenant_id, period, added, cost_usd)
            .await
        {
            Ok((total_tokens, total_cost)) => {
                maybe_alert_budget(state, resolved, total_tokens, total_cost);
            }
            Err(e) => {
                tracing::warn!(error = %e, "term usage commit failed");
                state.alerts.issue(
                    "redis_term_usage_failed",
                    "Redis term-usage commit failed",
                    format!("tenant `{}` model `{model}`: {e}", resolved.tenant_name),
                );
            }
        }
    }
    if let Some(period) = key_term_period {
        let added = input_tokens.saturating_add(output_tokens) as i64;
        match state
            .redis
            .term_usage_add(&resolved.key_id, period, added, cost_usd)
            .await
        {
            Ok((total_tokens, total_cost)) => {
                maybe_alert_key_budget(state, resolved, total_tokens, total_cost);
            }
            Err(e) => {
                tracing::warn!(error = %e, "key term usage commit failed");
                state.alerts.issue(
                    "redis_key_term_usage_failed",
                    "Redis key term-usage commit failed",
                    format!(
                        "tenant `{}` key `{}` model `{model}`: {e}",
                        resolved.tenant_name, resolved.key_id
                    ),
                );
            }
        }
    }

    finalize(
        state,
        request_id,
        resolved,
        meta,
        model,
        admission,
        est,
        input_tokens,
        output_tokens,
        queue_wait_ms,
        ttft_ms,
        total_ms,
        status_code,
        cache_status,
        cost_usd,
        energy,
    );
    state.metrics.total_ms.observe(total_ms as f64);
}

/// Resolve a key via moka, falling back to Redis and caching the result.
#[tracing::instrument(skip_all, name = "auth_resolve")]
pub(crate) async fn resolve_key(state: &AppState, hash: &str) -> Option<Arc<ResolvedKey>> {
    if let Some(r) = state.key_cache.get(hash).await {
        return Some(r);
    }
    match state.redis.get_resolved_key(hash).await {
        Ok(Some(r)) => {
            let r = Arc::new(r);
            state.key_cache.insert(hash.to_string(), r.clone()).await;
            Some(r)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "redis key lookup failed");
            state.alerts.issue(
                "redis_key_lookup_failed",
                "Redis key lookup failed",
                format!("API key resolution failed against Redis: {e}"),
            );
            None
        }
    }
}

/// Union of routing tags across the candidates the request may actually use.
/// Restricting the classifier to achievable tags keeps it honest and cheap.
fn union_candidate_tags(
    candidates: &[crate::router::Candidate],
    allowed_models: Option<&[String]>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in candidates {
        if let Some(allowed) = allowed_models {
            if !allowed.iter().any(|m| m == &c.model.model_name) {
                continue;
            }
        }
        for t in &c.model.tags {
            if !out.contains(t) {
                out.push(t.clone());
            }
        }
    }
    out
}

/// Intent tags for an `auto` request. Tries the classifier brain first (when
/// enabled, configured, resolvable, and not itself `auto`), then falls back to
/// cheap heuristics. Either may return empty, which routes on capacity/cost.
async fn derive_desired_tags(
    state: &AppState,
    json: &serde_json::Value,
    est_input_tokens: u64,
    available_tags: &[String],
) -> Vec<String> {
    let settings = state.classifier.settings();
    if settings.classifier_active() && !available_tags.is_empty() {
        if let Some(name) = settings.classifier_model.as_deref() {
            if name != crate::router::AUTO_MODEL_NAME {
                if let Some(brain) = resolve_model(state, name).await {
                    let prompt = classifier_prompt(json);
                    if !prompt.trim().is_empty() {
                        let tags = state
                            .classifier
                            .classify(&state.http, &brain, &prompt, available_tags)
                            .await;
                        if !tags.is_empty() {
                            return tags;
                        }
                    }
                }
            }
        }
    }
    // Heuristic fallback (also used when the classifier is off or returns empty).
    crate::router::heuristic_tags(json, est_input_tokens)
}

/// Build a compact prompt for the classifier: the system message (if any) plus
/// the first user message's text.
fn classifier_prompt(json: &serde_json::Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
        let mut have_user = false;
        for msg in messages {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let text = message_text(msg.get("content"));
            if role == "system" && !text.is_empty() {
                parts.push(text);
            } else if role == "user" && !have_user && !text.is_empty() {
                parts.push(text);
                have_user = true;
            }
            if have_user {
                break;
            }
        }
    }
    parts.join("\n")
}

fn message_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                    out.push_str(t);
                    out.push('\n');
                }
            }
            out
        }
        _ => String::new(),
    }
}

pub(crate) async fn resolve_model(state: &AppState, name: &str) -> Option<Arc<ResolvedModel>> {
    if let Some(r) = state.model_cache.get(name).await {
        return Some(r);
    }
    match state.redis.get_resolved_model(name).await {
        Ok(Some(r)) => {
            let r = Arc::new(r);
            state.model_cache.insert(name.to_string(), r.clone()).await;
            Some(r)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "redis model lookup failed");
            state.alerts.issue(
                "redis_model_lookup_failed",
                "Redis model lookup failed",
                format!("model `{name}` lookup failed against Redis: {e}"),
            );
            None
        }
    }
}

fn effective_admission_weight(tenant_weight: i64, route: Option<&ResolvedModel>) -> i64 {
    let Some(route) = route else {
        return tenant_weight.max(1);
    };
    ((tenant_weight as f64 * route.admission_weight as f64) / 100.0)
        .round()
        .max(1.0) as i64
}

/// OpenAI-style endpoints that must resolve to a registered model route.
/// Unregistered models must not fall through to the default benchmark fixture upstream.
/// Evaluate a tenant's schedule against the current instant. Returns `Ok(())`
/// when traffic is permitted, or `Err(reason)` with a client-facing message when
/// the tenant is outside its activation window, expired, or outside its
/// recurring weekly windows.
fn tenant_active_now(
    resolved: &ResolvedKey,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), &'static str> {
    if let Some(from) = resolved.active_from {
        if now < from {
            return Err("tenant is not active yet");
        }
    }
    if let Some(until) = resolved.active_until {
        if now >= until {
            return Err("tenant access has expired");
        }
    }
    if let Some(windows) = resolved.weekly_windows.as_ref().filter(|w| !w.is_empty()) {
        // Evaluate the recurring windows in the tenant's local timezone. An
        // unparseable timezone falls back to UTC rather than blocking traffic.
        let tz: chrono_tz::Tz = resolved.timezone.parse().unwrap_or(chrono_tz::UTC);
        let local = now.with_timezone(&tz);
        use chrono::{Datelike, Timelike};
        let day = local.weekday().num_days_from_sunday() as u8; // 0=Sunday
        let minute_of_day = (local.hour() * 60 + local.minute()) as u16;
        let in_window = windows
            .iter()
            .any(|w| w.day == day && minute_of_day >= w.start_min && minute_of_day < w.end_min);
        if !in_window {
            return Err("tenant is outside its scheduled access window");
        }
    }
    Ok(())
}

/// Compute the term-usage period key for a tenant, or `None` when no cumulative
/// budget cap is configured. The key namespaces the Redis counters so that
/// `monthly` budgets roll over at each calendar month (in the tenant timezone),
/// `term` budgets reset whenever `budget_started_at` changes, and `lifetime`
/// budgets never reset.
fn term_period_key(resolved: &ResolvedKey, now: chrono::DateTime<chrono::Utc>) -> Option<String> {
    budget_period_key(
        resolved.budget_tokens,
        resolved.budget_cost_usd,
        resolved.budget_period.as_deref(),
        resolved.budget_started_at,
        &resolved.timezone,
        now,
    )
}

fn key_term_period_key(
    resolved: &ResolvedKey,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    budget_period_key(
        resolved.key_budget_tokens,
        resolved.key_budget_cost_usd,
        resolved.key_budget_period.as_deref(),
        resolved.key_budget_started_at,
        &resolved.timezone,
        now,
    )
}

fn budget_period_key(
    budget_tokens: Option<i64>,
    budget_cost_usd: Option<f64>,
    budget_period: Option<&str>,
    budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    timezone: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    if budget_tokens.is_none() && budget_cost_usd.is_none() {
        return None;
    }
    let period = budget_period.unwrap_or("lifetime");
    let key = match period {
        "monthly" => {
            use chrono::Datelike;
            let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
            let local = now.with_timezone(&tz);
            format!("m:{}-{:02}", local.year(), local.month())
        }
        "term" => {
            let anchor = budget_started_at.map(|t| t.timestamp()).unwrap_or(0);
            format!("t:{anchor}")
        }
        // "lifetime" and any unknown value: a single non-rolling bucket.
        _ => {
            let anchor = budget_started_at.map(|t| t.timestamp()).unwrap_or(0);
            format!("l:{anchor}")
        }
    };
    Some(key)
}

/// Emit warning/exhaustion alerts when a tenant crosses 80% / 100% of either
/// cumulative budget cap. Cooldown dedup lives in `SlackAlerts`.
fn maybe_alert_budget(state: &AppState, resolved: &ResolvedKey, used_tokens: i64, used_cost: f64) {
    let token_pct = resolved
        .budget_tokens
        .filter(|c| *c > 0)
        .map(|cap| used_tokens as f64 / cap as f64);
    let cost_pct = resolved
        .budget_cost_usd
        .filter(|c| *c > 0.0)
        .map(|cap| used_cost / cap);
    let pct = [token_pct, cost_pct]
        .into_iter()
        .flatten()
        .fold(0.0_f64, f64::max);
    if pct >= 1.0 {
        state.alerts.issue(
            format!("term_budget_exhausted:{}", resolved.tenant_id),
            "Tenant term budget exhausted",
            format!(
                "tenant `{}` reached its budget cap (used {used_tokens} tokens / ${used_cost:.4})",
                resolved.tenant_name
            ),
        );
    } else if pct >= 0.8 {
        state.alerts.issue(
            format!("term_budget_warn:{}", resolved.tenant_id),
            "Tenant term budget at 80%",
            format!(
                "tenant `{}` is at {:.0}% of its budget (used {used_tokens} tokens / ${used_cost:.4})",
                resolved.tenant_name,
                pct * 100.0
            ),
        );
    }
}

fn maybe_alert_key_budget(
    state: &AppState,
    resolved: &ResolvedKey,
    used_tokens: i64,
    used_cost: f64,
) {
    let token_pct = resolved
        .key_budget_tokens
        .filter(|c| *c > 0)
        .map(|cap| used_tokens as f64 / cap as f64);
    let cost_pct = resolved
        .key_budget_cost_usd
        .filter(|c| *c > 0.0)
        .map(|cap| used_cost / cap);
    let pct = [token_pct, cost_pct]
        .into_iter()
        .flatten()
        .fold(0.0_f64, f64::max);
    if pct >= 1.0 {
        state.alerts.issue(
            format!("key_term_budget_exhausted:{}", resolved.key_id),
            "API key term budget exhausted",
            format!(
                "tenant `{}` key `{}` reached its budget cap (used {used_tokens} tokens / ${used_cost:.4})",
                resolved.tenant_name, resolved.key_id
            ),
        );
    } else if pct >= 0.8 {
        state.alerts.issue(
            format!("key_term_budget_warn:{}", resolved.key_id),
            "API key term budget at 80%",
            format!(
                "tenant `{}` key `{}` is at {:.0}% of its budget (used {used_tokens} tokens / ${used_cost:.4})",
                resolved.tenant_name,
                resolved.key_id,
                pct * 100.0
            ),
        );
    }
}

/// Serve `GET /v1/models` by aggregating what the upstreams actually report.
///
/// The single default upstream only lists its own models (e.g. the litellm or
/// aibrix gateway), so Slurm-hosted models on their own endpoints never show up.
/// We instead ask every distinct upstream that backs a registered model (the
/// default base plus each model's endpoints) for its own `/v1/models` and union
/// the entries **verbatim** — each model keeps the real `id` and `owned_by` its
/// serving engine reports (litellm `openai`, vLLM `vllm`, llama.cpp `llamacpp`,
/// Ollama `library`, …). The one addition: entries whose id matches a
/// registered model are annotated with that route's modality (`model_type` +
/// `mode`) so clients don't have to guess it from the id. Lookups are
/// best-effort and concurrent, so a slow or down upstream is simply skipped.
async fn models_list_response(state: &AppState) -> Response<Body> {
    let candidates = state.model_registry.load();

    // Each registered model's effective upstream target(s), paired with the key
    // needed to reach them. Most upstreams (litellm / aibrix / openai-compatible)
    // require auth on `/v1/models`, so an unauthenticated probe 401s and the
    // model silently drops out. Deduped by base; a base that carries a key wins
    // over one that doesn't. Only models with neither endpoints nor an api_base
    // fall back to the global default base.
    let mut by_base: std::collections::HashMap<String, Option<String>> =
        std::collections::HashMap::new();
    for c in candidates.iter() {
        let targets: Vec<(String, Option<String>)> = if !c.model.endpoints.is_empty() {
            c.model
                .endpoints
                .iter()
                .filter(|e| e.enabled)
                .map(|e| {
                    (
                        e.api_base.clone(),
                        e.api_key.clone().or_else(|| c.model.api_key.clone()),
                    )
                })
                .collect()
        } else if !c.model.api_base.is_empty() {
            vec![(c.model.api_base.clone(), c.model.api_key.clone())]
        } else {
            vec![(state.upstream_base.clone(), None)]
        };
        for (base, key) in targets {
            if base.is_empty() {
                continue;
            }
            let slot = by_base.entry(base).or_insert(None);
            if slot.is_none() {
                *slot = key;
            }
        }
    }

    // Fan out concurrently (authenticating each probe), then union verbatim.
    let results = futures_util::future::join_all(
        by_base
            .iter()
            .map(|(base, key)| fetch_upstream_models(state, base, key.as_deref())),
    )
    .await;

    // Ids the gateway can vouch for: a client-facing `model_name` always wins
    // over an `upstream_model` alias, because `model_name` is what request
    // resolution actually matches on.
    let mut types_by_id: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    for c in candidates.iter() {
        if !c.model.upstream_model.is_empty() {
            types_by_id
                .entry(c.model.upstream_model.clone())
                .or_insert_with(|| c.model.model_type.clone());
        }
    }
    for c in candidates.iter() {
        types_by_id.insert(c.model.model_name.clone(), c.model.model_type.clone());
    }

    let mut merged = merge_upstream_models(results);
    annotate_model_types(&mut merged, &types_by_id);
    (StatusCode::OK, axum::Json(merged)).into_response()
}

/// Annotate aggregated `/v1/models` entries with the registered route's
/// modality, so clients can group models (chat vs. image vs. audio) from
/// gateway-reported fact instead of guessing from the id. Each matched entry
/// gains `model_type` (obleth's [`MODEL_TYPES`](obleth_config::MODEL_TYPES)
/// vocabulary) and `mode` (the LiteLLM-convention alias many clients already
/// read, where `image` is spelled `image_generation`). Entries whose id
/// matches no registered model — wildcard passthroughs — stay verbatim.
fn annotate_model_types(
    list: &mut serde_json::Value,
    types_by_id: &std::collections::HashMap<String, String>,
) {
    let Some(data) = list.get_mut("data").and_then(|d| d.as_array_mut()) else {
        return;
    };
    for entry in data {
        let Some(model_type) = entry
            .get("id")
            .and_then(|i| i.as_str())
            .and_then(|id| types_by_id.get(id))
        else {
            continue;
        };
        let mode = match model_type.as_str() {
            "image" => "image_generation",
            other => other,
        };
        if let Some(obj) = entry.as_object_mut() {
            obj.insert("model_type".into(), model_type.clone().into());
            obj.insert("mode".into(), mode.into());
        }
    }
}

/// Union upstream `/v1/models` entries into one OpenAI `{object:"list", data:[…]}`
/// payload, keeping each entry exactly as its upstream reported it (real `id`,
/// real `owned_by`) and de-duping by `id` — the first upstream to report an id
/// wins. Sorted by id for stable output. Pure so it can be unit-tested without
/// the network fan-out.
fn merge_upstream_models(
    lists: impl IntoIterator<Item = Vec<serde_json::Value>>,
) -> serde_json::Value {
    let mut seen = std::collections::HashSet::new();
    let mut data: Vec<serde_json::Value> = Vec::new();
    for entry in lists.into_iter().flatten() {
        let Some(id) = entry.get("id").and_then(|i| i.as_str()) else {
            continue;
        };
        if seen.insert(id.to_string()) {
            data.push(entry);
        }
    }
    data.sort_by(|a, b| {
        a.get("id")
            .and_then(|i| i.as_str())
            .cmp(&b.get("id").and_then(|i| i.as_str()))
    });
    serde_json::json!({ "object": "list", "data": data })
}

/// Best-effort `GET {base}/v1/models`, returning the upstream's `data` entries
/// verbatim. Any timeout/error/parse failure yields an empty list so one bad
/// upstream never breaks or stalls the aggregate listing.
async fn fetch_upstream_models(
    state: &AppState,
    base: &str,
    api_key: Option<&str>,
) -> Vec<serde_json::Value> {
    async fn inner(
        state: &AppState,
        base: &str,
        api_key: Option<&str>,
    ) -> Option<Vec<serde_json::Value>> {
        let url = build_upstream_url(base, "/v1/models", "");
        let mut req = state.http.get(&url);
        if let Some(key) = api_key {
            req = req.bearer_auth(key);
        }
        let resp = timeout(Duration::from_secs(4), req.send())
            .await
            .ok()?
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let v: serde_json::Value = resp.json().await.ok()?;
        Some(v.get("data")?.as_array()?.clone())
    }
    inner(state, base, api_key).await.unwrap_or_default()
}

fn requires_registered_model(path: &str) -> bool {
    matches!(
        path,
        "/v1/chat/completions"
            | "/v1/completions"
            | "/v1/embeddings"
            | "/v1/responses"
            | "/v1/audio/transcriptions"
            | "/v1/audio/translations"
            | "/v1/audio/speech"
            | "/v1/images/generations"
            | "/v1/images/edits"
            | "/v1/images/variations"
    )
}

/// True when the endpoint carries the model name in a multipart/form-data body
/// (audio transcription/translation file uploads) rather than JSON.
fn is_multipart_endpoint(path: &str) -> bool {
    matches!(path, "/v1/audio/transcriptions" | "/v1/audio/translations")
}

/// A single parsed `multipart/form-data` field, held in memory so it can be
/// rebuilt into an upstream form after model resolution.
struct MultipartField {
    name: String,
    file_name: Option<String>,
    content_type: Option<String>,
    data: Bytes,
}

/// Parse an in-memory multipart body into its fields. The whole body is already
/// buffered (bounded by `BODY_LIMIT`), so this just re-reads it through `multer`.
async fn parse_multipart(
    body: &Bytes,
    boundary: &str,
) -> Result<Vec<MultipartField>, multer::Error> {
    let bytes = body.clone();
    let stream =
        futures_util::stream::once(async move { Ok::<Bytes, std::convert::Infallible>(bytes) });
    let mut multipart = multer::Multipart::new(stream, boundary.to_string());
    let mut fields = Vec::new();
    while let Some(field) = multipart.next_field().await? {
        let name = field.name().unwrap_or("").to_string();
        let file_name = field.file_name().map(|s| s.to_string());
        let content_type = field.content_type().map(|m| m.to_string());
        let data = field.bytes().await?;
        fields.push(MultipartField {
            name,
            file_name,
            content_type,
            data,
        });
    }
    Ok(fields)
}

/// Rebuild a reqwest multipart form from parsed fields, replacing the client
/// `model` value with the upstream model name. File parts preserve their
/// filename and content-type.
fn build_multipart_form(
    fields: Vec<MultipartField>,
    upstream_model: &str,
) -> reqwest::multipart::Form {
    let mut form = reqwest::multipart::Form::new();
    let mut had_model = false;
    for f in fields {
        if f.name == "model" {
            had_model = true;
            form = form.text("model", upstream_model.to_string());
            continue;
        }
        if f.file_name.is_some() {
            let file_name = f.file_name.unwrap_or_default();
            let data = f.data.to_vec();
            let part = match f.content_type {
                // content_type came from a parsed Mime, so mime_str won't fail.
                Some(ct) => reqwest::multipart::Part::bytes(data)
                    .file_name(file_name)
                    .mime_str(&ct)
                    .unwrap_or_else(|_| reqwest::multipart::Part::text("")),
                None => reqwest::multipart::Part::bytes(data).file_name(file_name),
            };
            form = form.part(f.name, part);
        } else {
            form = form.text(f.name, String::from_utf8_lossy(&f.data).into_owned());
        }
    }
    if !had_model {
        form = form.text("model", upstream_model.to_string());
    }
    form
}

/// Per-request surcharge for non-token-billed modalities: image generations are
/// billed per image, text-to-speech per input character. Returns `0.0` for
/// token-billed modalities (chat, embeddings) and audio transcription.
fn compute_modality_cost(route: Option<&ResolvedModel>, json: &serde_json::Value) -> f64 {
    let Some(route) = route else {
        return 0.0;
    };
    match route.model_type.as_str() {
        "image" => {
            let n = json.get("n").and_then(|v| v.as_u64()).unwrap_or(1).max(1);
            n as f64 * route.cost_per_image
        }
        "audio_speech" => {
            let chars = json
                .get("input")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count())
                .unwrap_or(0);
            chars as f64 * route.cost_per_character
        }
        _ => 0.0,
    }
}

/// Swap the body's `model` field for the upstream model name, mutating the
/// already-parsed JSON in place (no deep clone) and re-serializing once.
/// Bodies that are not JSON objects (e.g. multipart audio uploads, handled
/// separately) pass through untouched, as does everything when the model has
/// no registered route. The token estimate is unaffected by this rewrite —
/// the tokenizer never reads the `model` field.
///
/// `force_non_streaming` is set when a response-transforming boon is armed:
/// the upstream call is made non-streaming regardless of what the client
/// asked for. This happens here — after the cache key was computed from the
/// client body — so streaming and non-streaming clients keep distinct cache
/// entries holding the representation each actually receives (JSON vs SSE).
///
/// `stream_with_usage` is set for the streaming tool loop's turn-0 dispatch:
/// the upstream stays streaming but is asked to include a final usage chunk so
/// turn-0 tokens are billed exactly instead of estimated. (`tool_stream`
/// captures that usage but never forwards it to the client unless the client
/// itself asked for usage.) The two flags are mutually exclusive.
fn prepare_upstream_body(
    route: Option<&ResolvedModel>,
    json: &mut serde_json::Value,
    body: Bytes,
    force_non_streaming: bool,
    stream_with_usage: bool,
) -> Bytes {
    let Some(route) = route else {
        return body;
    };
    let Some(obj) = json.as_object_mut() else {
        return body;
    };
    obj.insert(
        "model".into(),
        serde_json::Value::String(route.upstream_model.clone()),
    );
    if force_non_streaming {
        obj.insert("stream".into(), serde_json::Value::Bool(false));
        obj.remove("stream_options");
    } else if stream_with_usage {
        obj.insert("stream".into(), serde_json::Value::Bool(true));
        obj.insert(
            "stream_options".into(),
            serde_json::json!({ "include_usage": true }),
        );
    }
    serde_json::to_vec(&*json).map(Bytes::from).unwrap_or(body)
}

/// One resolved upstream target: a base URL plus an optional bearer key.
struct Target {
    base: String,
    api_key: Option<String>,
}

/// Build the ordered list of upstream targets for a request.
///
/// When the model defines explicit endpoints we route across the ones that are
/// both `enabled` and `healthy`. `failover` orders them by ascending priority
/// (lowest first); `load_balance` orders them by a weighted random shuffle so
/// traffic spreads across clusters in proportion to their weights; `session_hash`
/// pins a session to one endpoint via rendezvous hashing of the session key,
/// with the rest following for failover. When no usable endpoints exist we fall
/// back to the model's own `api_base`/`api_key` (or the global default base),
/// preserving the legacy single-upstream path.
fn build_targets(
    route: Option<&ResolvedModel>,
    default_base: &str,
    selection_mode: &str,
    session_key: &str,
) -> Vec<Target> {
    let mut targets: Vec<Target> = Vec::new();
    if let Some(r) = route {
        let mut eligible: Vec<&ResolvedEndpoint> = r
            .endpoints
            .iter()
            .filter(|e| e.enabled && e.healthy)
            .collect();
        if !eligible.is_empty() {
            match selection_mode {
                "load_balance" => eligible = weighted_order(eligible),
                "session_hash" => eligible = session_hash_order(eligible, session_key),
                _ => eligible.sort_by_key(|e| e.priority), // failover (default)
            }
            for e in eligible {
                targets.push(Target {
                    base: e.api_base.clone(),
                    api_key: e.api_key.clone().or_else(|| r.api_key.clone()),
                });
            }
            return targets;
        }
    }
    targets.push(Target {
        base: route
            .map(|r| r.api_base.clone())
            .unwrap_or_else(|| default_base.to_string()),
        api_key: route.and_then(|r| r.api_key.clone()),
    });
    targets
}

/// Order endpoints by weighted random sampling (A-Res): each endpoint gets a
/// key `u^(1/weight)` for a uniform random `u`, and we sort by descending key.
/// Higher-weight endpoints land earlier more often, so the first eligible
/// target is chosen in proportion to weight while the rest stay available for
/// failover.
fn weighted_order(items: Vec<&ResolvedEndpoint>) -> Vec<&ResolvedEndpoint> {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut next = move || {
        seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut keyed: Vec<(f64, &ResolvedEndpoint)> = items
        .into_iter()
        .map(|e| {
            let w = e.weight.max(1) as f64;
            let u = (next() as f64 / u64::MAX as f64).max(1e-12);
            (u.powf(1.0 / w), e)
        })
        .collect();
    keyed.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    keyed.into_iter().map(|(_, e)| e).collect()
}

/// FNV-1a over `session_key\0endpoint_id` — a small, dependency-free, stable
/// hash. Stability matters: the same key must score an endpoint identically on
/// every request so a session keeps landing on the same replica.
fn rendezvous_score(session_key: &str, endpoint_id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in session_key
        .bytes()
        .chain(std::iter::once(0u8))
        .chain(endpoint_id.bytes())
    {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Order endpoints so a session sticks to one replica: rendezvous (highest-
/// random-weight) hashing scores each endpoint by `hash(session_key, id)`; the
/// highest score is the session's home and goes first, the rest follow for
/// failover. Deterministic for a given key; adding/removing an endpoint
/// reshuffles only minimally. With no session key, falls back to weighted order.
fn session_hash_order<'a>(
    items: Vec<&'a ResolvedEndpoint>,
    session_key: &str,
) -> Vec<&'a ResolvedEndpoint> {
    if session_key.is_empty() {
        // No session key on the request, so stickiness is impossible: this falls
        // back to weighted_order, i.e. session_hash behaves like load_balance.
        // Logged (debug, not warn — keyless requests are common and expected) so
        // operators can see why a session_hash model isn't actually sticking.
        tracing::debug!(
            "session_hash selected but request has no session key; falling back to weighted order"
        );
        return weighted_order(items);
    }
    let mut scored: Vec<(u64, &ResolvedEndpoint)> = items
        .into_iter()
        .map(|e| (rendezvous_score(session_key, &e.id), e))
        .collect();
    // Highest score first; tie-break on id for a fully deterministic order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    scored.into_iter().map(|(_, e)| e).collect()
}

/// Whether an upstream HTTP status is worth retrying or failing over for.
/// Transient transport/overload conditions only — never 4xx client errors
/// (except 408 request-timeout and 429 too-many-requests).
fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

/// Whether a reqwest send error means the request never reached the upstream, so
/// a fresh-connection retry is safe. Covers connection establishment failures and
/// the "reused a keep-alive socket the server already closed" race — hyper
/// surfaces the latter as an incomplete/closed/reset error in the source chain
/// rather than via `is_connect()`, so we also scan the error chain for the
/// well-known signatures.
fn is_connection_error(e: &reqwest::Error) -> bool {
    use std::error::Error;
    if e.is_connect() || e.is_request() {
        return true;
    }
    let mut source: Option<&dyn Error> = Some(e);
    while let Some(err) = source {
        let msg = err.to_string().to_ascii_lowercase();
        if msg.contains("connection closed")
            || msg.contains("connection reset")
            || msg.contains("connection aborted")
            || msg.contains("broken pipe")
            || msg.contains("channel closed")
            || msg.contains("unexpected end of file")
            || msg.contains("incompletemessage")
        {
            return true;
        }
        source = err.source();
    }
    false
}

/// Exponential backoff for retry `attempt` (0-based), capped to avoid overflow.
fn backoff_for(base: Duration, attempt: u32) -> Duration {
    if base.is_zero() {
        return base;
    }
    base.saturating_mul(1u32 << attempt.min(6))
}

/// Reject client request paths that could escape the configured upstream base.
///
/// The upstream host is fixed by the operator-registered `api_base` (and is
/// SSRF-screened at registration), but the client-supplied path is appended to
/// it. A `..` segment — literal or percent-encoded — could walk above the
/// intended API prefix and reach a different path on that host, so we refuse it
/// outright. Legitimate OpenAI-compatible paths never contain `..`, encoded
/// dots, or encoded separators.
pub(crate) fn has_path_traversal(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    // Encoded dot (`%2e`) or separators (`%2f` `/`, `%5c` `\`) can reconstruct a
    // traversal after the upstream decodes them; none are valid here.
    if lowered.contains("%2e") || lowered.contains("%2f") || lowered.contains("%5c") {
        return true;
    }
    path.split(['/', '\\']).any(|seg| seg == "..")
}

fn build_upstream_url(base: &str, path: &str, query: &str) -> String {
    let base = base.trim_end_matches('/');
    let rel_raw = path.trim_start_matches('/');
    // Defensive: operators sometimes paste the full endpoint URL as api_base
    // (e.g. ".../v1/embeddings" or ".../v1/audio/speech") instead of the base
    // (".../v1"). If the configured base already ends with the request path,
    // don't append it again — that would produce ".../v1/embeddings/v1/embeddings".
    if !rel_raw.is_empty() && (base.ends_with(rel_raw) || base.ends_with(&format!("/{rel_raw}"))) {
        return format!("{base}{query}");
    }
    let mut rel = rel_raw.to_string();
    // api_base is typically `…/v1` while clients call `/v1/chat/completions` — avoid `/v1/v1/…`
    if base.ends_with("/v1") && rel.starts_with("v1/") {
        rel = rel.trim_start_matches("v1/").to_string();
    }
    if rel.is_empty() {
        format!("{base}{query}")
    } else {
        format!("{base}/{rel}{query}")
    }
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(rest) = v.strip_prefix("Bearer ") {
            return Some(rest.trim().to_string());
        }
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
}

pub(crate) fn forward_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        match name.as_str() {
            // strip hop-by-hop / auth / encoding so the body stays inspectable;
            // x-obleth-boons is a gateway directive, not an upstream header
            "host" | "content-length" | "authorization" | "x-api-key" | "accept-encoding"
            | "connection" | "x-obleth-boons" => continue,
            _ => {
                out.insert(name.clone(), value.clone());
            }
        }
    }
    out
}

/// Append a chunk to the rolling response tail, keeping only the last
/// `TAIL_CAP` bytes. Works on raw bytes with `copy_within` so a long stream
/// never re-allocates per chunk; the one UTF-8 conversion happens at stream
/// end (a partial char at the buffer start is replaced lossily there, which is
/// harmless — `extract_usage` only scans for ASCII JSON keys).
fn append_tail(tail: &mut Vec<u8>, chunk: &[u8]) {
    if chunk.len() >= TAIL_CAP {
        tail.clear();
        tail.extend_from_slice(&chunk[chunk.len() - TAIL_CAP..]);
        return;
    }
    let overflow = (tail.len() + chunk.len()).saturating_sub(TAIL_CAP);
    if overflow > 0 {
        tail.copy_within(overflow.., 0);
        tail.truncate(tail.len() - overflow);
    }
    tail.extend_from_slice(chunk);
}

/// Pull `prompt_tokens` / `completion_tokens` out of the (possibly streamed)
/// response tail. Returns `None` if the upstream didn't report usage.
///
/// Embedding responses report `prompt_tokens` (and `total_tokens`) but no
/// `completion_tokens`; those are treated as input-only usage.
fn extract_usage(tail: &str) -> Option<(u32, u32)> {
    let input = find_int_after(tail, "\"prompt_tokens\"");
    let output = find_int_after(tail, "\"completion_tokens\"");
    match (input, output) {
        (Some(i), Some(o)) => Some((i, o)),
        // Embeddings and other input-only modalities: count prompt tokens.
        (Some(i), None) => Some((i, 0)),
        _ => None,
    }
}

fn find_int_after(haystack: &str, key: &str) -> Option<u32> {
    let idx = haystack.rfind(key)? + key.len();
    let rest = &haystack[idx..];
    let digits: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Per-request metadata captured once at the top of the handler and shared by
/// every `finalize` path, so the request-log columns are identical regardless
/// of where the request terminates (cache hit, rejection, upstream error, or
/// streamed success). Cheap to clone for the response-stream closure.
#[derive(Clone)]
struct RequestMeta {
    /// Conversation grouping id (client-supplied or derived), or empty.
    session_id: String,
    /// How `session_id` was obtained: "client" | "derived" | "none".
    session_id_source: &'static str,
    /// Coarse request class derived from the request path, except synthetic
    /// tenants' requests are stamped `benchmark` instead (see
    /// [`effective_request_type`]).
    request_type: &'static str,
}

/// Classify a request by its OpenAI-style path suffix. Matching the suffix (not
/// the full path) keeps this robust to version prefixes (`/v1/...`) or any
/// future routing prefix.
fn request_type_for_path(path: &str) -> &'static str {
    if path.ends_with("/chat/completions") {
        "chat"
    } else if path.ends_with("/responses") {
        "responses"
    } else if path.ends_with("/completions") {
        "completion"
    } else if path.ends_with("/embeddings") {
        "embedding"
    } else if path.contains("/audio/") {
        "audio"
    } else if path.contains("/images/") {
        "image"
    } else if path.ends_with("/rerank") || path.ends_with("/reranking") {
        "rerank"
    } else if path.ends_with("/moderations") {
        "moderation"
    } else {
        "other"
    }
}

/// The request class recorded in the ledger: synthetic tenants' traffic is
/// tagged `benchmark` (replacing the path-derived class, mirroring how health
/// probes record `health_probe`), everything else classifies by path.
fn effective_request_type(resolved: &ResolvedKey, path: &str) -> &'static str {
    if resolved.synthetic {
        obleth_config::BENCHMARK_REQUEST_TYPE
    } else {
        request_type_for_path(path)
    }
}

/// Provenance of a resolved conversation id.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SessionSource {
    Client,
    Derived,
    None,
}

impl SessionSource {
    fn as_str(self) -> &'static str {
        match self {
            SessionSource::Client => "client",
            SessionSource::Derived => "derived",
            SessionSource::None => "none",
        }
    }
}

/// A resolved conversation grouping key plus how it was obtained.
struct Conversation {
    value: String,
    source: SessionSource,
}

/// Resolve a conversation id. Precedence: explicit client signal (header or
/// body) > deterministic hash of the conversation seed > none. Total: never
/// errors. The OpenAI `user` field is intentionally NOT a session source (it
/// identifies an end-user, not a conversation).
fn resolve_conversation(
    headers: &HeaderMap,
    json: &serde_json::Value,
    tenant_id: Uuid,
    derivation_enabled: bool,
) -> Conversation {
    const MAX: usize = 200;
    let capped = |s: &str| -> String { s.trim().chars().take(MAX).collect() };

    // 1. Explicit client id: header wins, then body conventions.
    if let Some(s) = headers
        .get("x-obleth-session-id")
        .or_else(|| headers.get("x-session-id"))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Conversation {
            value: capped(s),
            source: SessionSource::Client,
        };
    }
    let body_ids = [
        json.get("session_id").and_then(|v| v.as_str()),
        json.get("metadata")
            .and_then(|m| m.get("session_id"))
            .and_then(|v| v.as_str()),
    ];
    for c in body_ids.into_iter().flatten() {
        if !c.trim().is_empty() {
            return Conversation {
                value: capped(c),
                source: SessionSource::Client,
            };
        }
    }

    // 2. Derived: hash the conversation seed (tenant + leading system/developer
    //    + first user message). Stable across turns.
    if derivation_enabled {
        if let Some(seed) = conversation_seed(json) {
            let h = fnv1a_continue(fnv1a(tenant_id.as_bytes()), seed.as_bytes());
            return Conversation {
                value: format!("{h:016x}"),
                source: SessionSource::Derived,
            };
        }
    }

    // 3. Nothing to go on.
    Conversation {
        value: String::new(),
        source: SessionSource::None,
    }
}

/// Maximum seed text hashed; bounds work on huge system prompts. The same
/// leading bytes are replayed every turn, so capping stays stable.
const SEED_CAP: usize = 8 * 1024;

/// Build the stable conversation seed: leading system/developer message text up
/// to and including the first user message. `None` when there are no messages
/// or no text to hash (e.g. embeddings, image-only first turn with no system).
fn conversation_seed(json: &serde_json::Value) -> Option<String> {
    let messages = json.get("messages")?.as_array()?;
    let mut seed = String::new();
    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        match role {
            "system" | "developer" => push_capped(&mut seed, &message_text(msg.get("content"))),
            "user" => {
                push_capped(&mut seed, &message_text(msg.get("content")));
                break; // first user turn closes the seed
            }
            _ => {}
        }
    }
    if seed.trim().is_empty() {
        None
    } else {
        Some(seed)
    }
}

/// Append up to the seed cap, with a separator so adjacent messages can't merge
/// into a colliding blob.
fn push_capped(seed: &mut String, text: &str) {
    if seed.len() >= SEED_CAP || text.is_empty() {
        return;
    }
    if !seed.is_empty() {
        seed.push('\u{1f}'); // unit separator
    }
    let room = SEED_CAP - seed.len();
    if text.len() <= room {
        seed.push_str(text);
    } else {
        let mut end = room;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        seed.push_str(&text[..end]);
    }
}

/// FNV-1a 64-bit (same family as `rendezvous_score`): fixed-seed, deterministic
/// across processes. Not for cryptographic use.
fn fnv1a(bytes: &[u8]) -> u64 {
    fnv1a_continue(0xcbf2_9ce4_8422_2325, bytes)
}

/// Continue an FNV-1a 64-bit hash over additional bytes (lets us chain tenant + seed).
fn fnv1a_continue(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[allow(clippy::too_many_arguments)]
fn finalize(
    state: &AppState,
    request_id: Uuid,
    resolved: &ResolvedKey,
    meta: &RequestMeta,
    model: &str,
    admission: Admission,
    est: CostEstimate,
    input_tokens: u32,
    output_tokens: u32,
    queue_wait_ms: u32,
    ttft_ms: u32,
    total_ms: u32,
    status_code: u16,
    cache_status: &str,
    cost_usd: f64,
    energy: crate::energy::EnergyFigures,
) {
    state
        .metrics
        .record_request(admission.as_str(), status_code, input_tokens, output_tokens);
    if ttft_ms > 0 {
        state.metrics.ttft_ms.observe(ttft_ms as f64);
    }
    if resolved.internal {
        return;
    }
    state.telemetry.record(UsageRecord {
        request_id,
        tenant_id: resolved.tenant_id,
        key_id: resolved.key_id,
        model: model.to_string(),
        admission: admission.as_str().to_string(),
        weight: resolved.weight,
        input_tokens,
        output_tokens,
        estimated_tokens: est.total(),
        queue_wait_ms,
        ttft_ms,
        total_ms,
        status_code,
        cache_status: cache_status.to_string(),
        cost_usd,
        energy_wh: energy.energy_wh,
        energy_cost_usd: energy.energy_cost_usd,
        co2_g: energy.co2_g,
        ts_ms: now_ms(),
        session_id: meta.session_id.clone(),
        session_id_source: meta.session_id_source.to_string(),
        request_type: meta.request_type.to_string(),
    });
}

/// Build an HTTP response from a cached entry, replaying the stored body and
/// content-type (works for both JSON and buffered SSE).
fn cached_response(cached: obleth_config::CachedResponse, request_id: Uuid) -> Response<Body> {
    let content_type = header::HeaderValue::from_str(&cached.content_type)
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/json"));
    let status = StatusCode::from_u16(cached.status).unwrap_or(StatusCode::OK);
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header("x-obleth-cache", "hit")
        .header("x-obleth-request-id", request_id.to_string())
        .body(Body::from(cached.body))
        .unwrap_or_else(|_| {
            error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cache response build failed",
            )
        })
}

pub(crate) fn error_json(status: StatusCode, msg: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": { "message": msg, "type": "obleth_gateway_error" } });
    (status, axum::Json(body)).into_response()
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        backoff_for, build_targets, build_upstream_url, effective_request_type, has_path_traversal,
        is_retryable_status, prepare_upstream_body, resolve_conversation, session_hash_order,
        tenant_active_now, weighted_order,
    };
    use axum::http::HeaderMap;
    use chrono::{DateTime, TimeZone, Utc};
    use obleth_config::{ResolvedEndpoint, ResolvedKey, WeeklyWindow};
    use std::time::Duration;
    use uuid::Uuid;

    fn endpoint(
        id: &str,
        base: &str,
        priority: i64,
        weight: i64,
        enabled: bool,
        healthy: bool,
    ) -> ResolvedEndpoint {
        ResolvedEndpoint {
            id: id.into(),
            api_base: base.into(),
            api_key: None,
            priority,
            weight,
            enabled,
            healthy,
        }
    }

    fn key_with_schedule(
        timezone: &str,
        active_from: Option<DateTime<Utc>>,
        active_until: Option<DateTime<Utc>>,
        weekly_windows: Option<Vec<WeeklyWindow>>,
    ) -> ResolvedKey {
        ResolvedKey {
            key_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            tenant_name: "t".into(),
            fairshare_group: "default".into(),
            group_weight: 100,
            weight: 100,
            tokens_per_minute: 1000,
            max_in_flight: None,
            disabled: false,
            status: "active".into(),
            timezone: timezone.into(),
            active_from,
            active_until,
            weekly_windows,
            budget_tokens: None,
            budget_cost_usd: None,
            budget_period: None,
            budget_started_at: None,
            key_budget_tokens: None,
            key_budget_cost_usd: None,
            key_budget_period: None,
            key_budget_started_at: None,
            allowed_models: None,
            internal: false,
            tracing_enabled: false,
            guardrails_policy: None,
            compression_policy: None,
            synthetic: false,
        }
    }

    #[test]
    fn synthetic_tenant_requests_are_tagged_benchmark() {
        let mut resolved = key_with_schedule("UTC", None, None, None);
        assert_eq!(
            effective_request_type(&resolved, "/v1/chat/completions"),
            "chat"
        );
        resolved.synthetic = true;
        assert_eq!(
            effective_request_type(&resolved, "/v1/chat/completions"),
            obleth_config::BENCHMARK_REQUEST_TYPE
        );
    }

    #[test]
    fn avoids_duplicate_v1_prefix() {
        assert_eq!(
            build_upstream_url("https://openai.rc.asu.edu/v1", "/v1/chat/completions", ""),
            "https://openai.rc.asu.edu/v1/chat/completions"
        );
    }

    #[test]
    fn benchmark_backend_path_unchanged() {
        assert_eq!(
            build_upstream_url("http://benchmark-backend:8081", "/v1/chat/completions", ""),
            "http://benchmark-backend:8081/v1/chat/completions"
        );
    }

    #[test]
    fn api_base_with_full_endpoint_is_not_doubled() {
        // Operator pasted the full endpoint URL as api_base instead of the base.
        assert_eq!(
            build_upstream_url(
                "https://openai.rc.asu.edu/v1/embeddings",
                "/v1/embeddings",
                ""
            ),
            "https://openai.rc.asu.edu/v1/embeddings"
        );
        assert_eq!(
            build_upstream_url(
                "https://openai.rc.asu.edu/v1/audio/speech",
                "/v1/audio/speech",
                ""
            ),
            "https://openai.rc.asu.edu/v1/audio/speech"
        );
    }

    #[test]
    fn path_traversal_is_detected() {
        // Literal `..` segments anywhere in the path.
        assert!(has_path_traversal("/v1/../admin"));
        assert!(has_path_traversal("/v1/chat/../../secret"));
        assert!(has_path_traversal("/.."));
        // Percent-encoded dots and separators that could decode to a traversal.
        assert!(has_path_traversal("/v1/%2e%2e/admin"));
        assert!(has_path_traversal("/v1%2fadmin"));
        assert!(has_path_traversal("/v1%5c..%5cadmin"));
        // Backslash separators.
        assert!(has_path_traversal("\\..\\admin"));
    }

    #[test]
    fn legitimate_paths_are_allowed() {
        assert!(!has_path_traversal("/v1/chat/completions"));
        assert!(!has_path_traversal("/v1/audio/transcriptions"));
        assert!(!has_path_traversal("/health"));
        // A literal `..` only inside a longer segment is not a traversal segment.
        assert!(!has_path_traversal("/v1/models/gpt..4"));
    }

    #[test]
    fn no_schedule_is_always_active() {
        let key = key_with_schedule("UTC", None, None, None);
        assert!(tenant_active_now(&key, Utc::now()).is_ok());
    }

    #[test]
    fn before_active_from_is_blocked() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let from = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let key = key_with_schedule("UTC", Some(from), None, None);
        assert!(tenant_active_now(&key, now).is_err());
    }

    #[test]
    fn after_active_until_is_blocked() {
        let now = Utc.with_ymd_and_hms(2026, 1, 3, 12, 0, 0).unwrap();
        let until = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let key = key_with_schedule("UTC", None, Some(until), None);
        assert!(tenant_active_now(&key, now).is_err());
    }

    #[test]
    fn inside_weekly_window_is_allowed() {
        // 2026-01-01 is a Thursday (weekday 4). 12:00 UTC = 720 minutes.
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let key = key_with_schedule(
            "UTC",
            None,
            None,
            Some(vec![WeeklyWindow {
                day: 4,
                start_min: 9 * 60,
                end_min: 17 * 60,
            }]),
        );
        assert!(tenant_active_now(&key, now).is_ok());
    }

    #[test]
    fn outside_weekly_window_is_blocked() {
        // Thursday 20:00 UTC, window only covers 09:00-17:00.
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 20, 0, 0).unwrap();
        let key = key_with_schedule(
            "UTC",
            None,
            None,
            Some(vec![WeeklyWindow {
                day: 4,
                start_min: 9 * 60,
                end_min: 17 * 60,
            }]),
        );
        assert!(tenant_active_now(&key, now).is_err());
    }

    #[test]
    fn timezone_shifts_the_local_weekday() {
        // 2026-01-01 02:00 UTC is still Wednesday (day 3) in New York (UTC-5).
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 2, 0, 0).unwrap();
        let key = key_with_schedule(
            "America/New_York",
            None,
            None,
            Some(vec![WeeklyWindow {
                day: 3,
                start_min: 0,
                end_min: 24 * 60,
            }]),
        );
        assert!(tenant_active_now(&key, now).is_ok());
    }

    #[test]
    fn merge_models_unions_upstreams_verbatim_and_dedups() {
        use super::merge_upstream_models;
        // litellm front (its models reported as "openai") and a Slurm/Ollama
        // endpoint (reported as "library"); ids overlap on gemma.
        let litellm = vec![
            serde_json::json!({"id": "gemma4-31b-it", "object": "model", "owned_by": "openai"}),
            serde_json::json!({"id": "minimax-m2-7-fast", "object": "model", "owned_by": "openai"}),
        ];
        let ollama = vec![
            serde_json::json!({"id": "glm-5.2", "object": "model", "owned_by": "library"}),
            // duplicate id already seen from litellm — first upstream wins.
            serde_json::json!({"id": "gemma4-31b-it", "object": "model", "owned_by": "library"}),
        ];

        let payload = merge_upstream_models(vec![litellm, ollama]);
        assert_eq!(payload["object"], "list");
        let data = payload["data"].as_array().unwrap();
        let ids: Vec<&str> = data.iter().map(|m| m["id"].as_str().unwrap()).collect();
        // Sorted union, deduped by id.
        assert_eq!(ids, vec!["gemma4-31b-it", "glm-5.2", "minimax-m2-7-fast"]);
        let owner_of = |id: &str| {
            data.iter().find(|m| m["id"] == id).unwrap()["owned_by"]
                .as_str()
                .unwrap()
        };
        // Owners are verbatim from upstream; nothing synthesized.
        assert_eq!(owner_of("gemma4-31b-it"), "openai"); // first upstream wins over the dup
        assert_eq!(owner_of("glm-5.2"), "library");
        assert_eq!(owner_of("minimax-m2-7-fast"), "openai");
    }

    #[test]
    fn models_listing_annotates_registered_modalities() {
        use super::annotate_model_types;
        let mut list = serde_json::json!({
            "object": "list",
            "data": [
                {"id": "flux-2-dev", "object": "model", "owned_by": "vllm"},
                {"id": "gemma4-31b-it", "object": "model", "owned_by": "openai"},
                {"id": "wildcard-passthrough", "object": "model", "owned_by": "library"},
            ]
        });
        let types: std::collections::HashMap<String, String> = [
            ("flux-2-dev".to_string(), "image".to_string()),
            ("gemma4-31b-it".to_string(), "chat".to_string()),
        ]
        .into();
        annotate_model_types(&mut list, &types);

        let field = |id: &str, key: &str| {
            list["data"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["id"] == id)
                .unwrap()[key]
                .clone()
        };
        // Registered models gain both the obleth vocabulary and the
        // LiteLLM-convention alias (`image` is spelled `image_generation`).
        assert_eq!(field("flux-2-dev", "model_type"), "image");
        assert_eq!(field("flux-2-dev", "mode"), "image_generation");
        assert_eq!(field("gemma4-31b-it", "model_type"), "chat");
        assert_eq!(field("gemma4-31b-it", "mode"), "chat");
        // An id no route claims stays verbatim — no guessed fields.
        assert!(field("wildcard-passthrough", "model_type").is_null());
        assert!(field("wildcard-passthrough", "mode").is_null());
    }

    fn model_with(endpoints: Vec<ResolvedEndpoint>) -> obleth_config::ResolvedModel {
        obleth_config::ResolvedModel {
            model_name: "m".into(),
            upstream_model: "m".into(),
            api_base: "http://primary/v1".into(),
            api_key: Some("model-key".into()),
            model_type: obleth_config::DEFAULT_MODEL_TYPE.to_string(),
            admission_weight: 100,
            max_in_flight: None,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 0,
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            context_window: 128_000,
            supports_function_calling: true,
            supports_system_messages: true,
            supports_response_schema: true,
            supports_tool_choice: true,
            supports_vision: false,
            tags: Vec::new(),
            boons: Vec::new(),
            tool_servers: Vec::new(),
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: obleth_config::DEFAULT_RETRY_BACKOFF_MS,
            endpoint_selection_mode: obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string(),
            debug_diagnostics: false,
            energy_slots_per_node: 0,
            endpoints,
        }
    }

    #[test]
    fn append_tail_keeps_last_cap_bytes() {
        use super::{append_tail, extract_usage, TAIL_CAP};
        let mut tail = Vec::new();
        // Small chunks accumulate verbatim.
        append_tail(&mut tail, b"hello ");
        append_tail(&mut tail, b"world");
        assert_eq!(tail, b"hello world");
        // Filling past the cap keeps only the newest TAIL_CAP bytes.
        append_tail(&mut tail, &vec![b'x'; TAIL_CAP]);
        assert_eq!(tail.len(), TAIL_CAP);
        assert!(tail.iter().all(|b| *b == b'x'));
        // A usage blob appended at the end survives intact and parses.
        append_tail(
            &mut tail,
            br#"{"usage":{"prompt_tokens":12,"completion_tokens":34}}"#,
        );
        assert_eq!(tail.len(), TAIL_CAP);
        assert_eq!(
            extract_usage(&String::from_utf8_lossy(&tail)),
            Some((12, 34))
        );
        // A chunk larger than the cap replaces the buffer with its own tail.
        append_tail(&mut tail, &vec![b'y'; TAIL_CAP * 2]);
        assert_eq!(tail.len(), TAIL_CAP);
        assert!(tail.iter().all(|b| *b == b'y'));
    }

    #[test]
    fn retryable_status_classification() {
        for s in [408, 429, 500, 502, 503, 504] {
            assert!(is_retryable_status(s), "{s} should be retryable");
        }
        for s in [200, 201, 400, 401, 403, 404, 422] {
            assert!(!is_retryable_status(s), "{s} should be fatal");
        }
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let base = Duration::from_millis(100);
        assert_eq!(backoff_for(base, 0), Duration::from_millis(100));
        assert_eq!(backoff_for(base, 1), Duration::from_millis(200));
        assert_eq!(backoff_for(base, 2), Duration::from_millis(400));
        // Cap kicks in at attempt 6 (×64); higher attempts stay flat.
        assert_eq!(backoff_for(base, 10), backoff_for(base, 6));
        // Zero base stays zero (no backoff configured).
        assert_eq!(backoff_for(Duration::ZERO, 3), Duration::ZERO);
    }

    #[test]
    fn no_endpoints_falls_back_to_model_base() {
        let model = model_with(Vec::new());
        let targets = build_targets(Some(&model), "http://global/v1", "failover", "");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].base, "http://primary/v1");
        assert_eq!(targets[0].api_key.as_deref(), Some("model-key"));
    }

    #[test]
    fn none_route_uses_global_default() {
        let targets = build_targets(None, "http://global/v1", "failover", "");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].base, "http://global/v1");
        assert!(targets[0].api_key.is_none());
    }

    #[test]
    fn failover_orders_by_priority_and_skips_unusable() {
        let model = model_with(vec![
            endpoint("c", "http://c", 30, 100, true, true),
            endpoint("a", "http://a", 10, 100, true, true),
            endpoint("disabled", "http://x", 5, 100, false, true),
            endpoint("unhealthy", "http://y", 1, 100, true, false),
            endpoint("b", "http://b", 20, 100, true, true),
        ]);
        let targets = build_targets(Some(&model), "http://global/v1", "failover", "");
        let bases: Vec<&str> = targets.iter().map(|t| t.base.as_str()).collect();
        assert_eq!(bases, vec!["http://a", "http://b", "http://c"]);
    }

    #[test]
    fn endpoint_key_falls_back_to_model_key() {
        let mut ep = endpoint("a", "http://a", 10, 100, true, true);
        ep.api_key = None;
        let model = model_with(vec![ep]);
        let targets = build_targets(Some(&model), "http://global/v1", "failover", "");
        assert_eq!(targets[0].api_key.as_deref(), Some("model-key"));
    }

    #[test]
    fn prepare_upstream_body_forces_non_streaming() {
        let model = model_with(Vec::new());
        let mut json = serde_json::json!({
            "model": "client-name",
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": []
        });
        let body = prepare_upstream_body(
            Some(&model),
            &mut json,
            axum::body::Bytes::new(),
            true,
            false,
        );
        let sent: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sent["stream"], false);
        assert!(sent.get("stream_options").is_none());
        // The model-name swap still happens.
        assert_eq!(sent["model"], "m");
    }

    #[test]
    fn prepare_upstream_body_adds_usage_for_stream_tap() {
        // Turn-0 of the streaming tool loop: stay streaming but ask for a final
        // usage chunk so turn-0 billing is exact.
        let model = model_with(Vec::new());
        let mut json = serde_json::json!({
            "model": "client-name",
            "stream": true,
            "messages": []
        });
        let body = prepare_upstream_body(
            Some(&model),
            &mut json,
            axum::body::Bytes::new(),
            false,
            true,
        );
        let sent: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["stream_options"]["include_usage"], true);
    }

    #[test]
    fn prepare_upstream_body_preserves_stream_without_force() {
        let model = model_with(Vec::new());
        let mut json = serde_json::json!({
            "model": "client-name",
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": []
        });
        let body = prepare_upstream_body(
            Some(&model),
            &mut json,
            axum::body::Bytes::new(),
            false,
            false,
        );
        let sent: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sent["stream"], true);
        assert!(sent.get("stream_options").is_some());
    }

    #[test]
    fn cache_key_diverges_on_client_stream_flag() {
        // The cache key is computed from the client body *before* the boon
        // interception forces `stream: false` upstream, so streaming and
        // non-streaming clients must land on different cache entries.
        let streaming = serde_json::to_vec(
            &serde_json::json!({ "model": "m", "stream": true, "messages": [] }),
        )
        .unwrap();
        let plain = serde_json::to_vec(
            &serde_json::json!({ "model": "m", "stream": false, "messages": [] }),
        )
        .unwrap();
        assert_ne!(
            obleth_config::cache_key("m", &streaming),
            obleth_config::cache_key("m", &plain)
        );
    }

    #[test]
    fn weighted_order_preserves_membership() {
        let eps = [
            endpoint("a", "http://a", 10, 100, true, true),
            endpoint("b", "http://b", 20, 50, true, true),
            endpoint("c", "http://c", 30, 1, true, true),
        ];
        let refs: Vec<&ResolvedEndpoint> = eps.iter().collect();
        let ordered = weighted_order(refs);
        assert_eq!(ordered.len(), 3);
        let mut ids: Vec<&str> = ordered.iter().map(|e| e.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn session_hash_is_deterministic_for_a_key() {
        let eps = [
            endpoint("a", "http://a", 10, 100, true, true),
            endpoint("b", "http://b", 20, 100, true, true),
            endpoint("c", "http://c", 30, 100, true, true),
        ];
        let refs: Vec<&ResolvedEndpoint> = eps.iter().collect();
        let first = session_hash_order(refs.clone(), "session-xyz");
        let second = session_hash_order(refs.clone(), "session-xyz");
        let firsts: Vec<&str> = first.iter().map(|e| e.id.as_str()).collect();
        let seconds: Vec<&str> = second.iter().map(|e| e.id.as_str()).collect();
        // Same key ⇒ identical ordering (so the session sticks to one replica).
        assert_eq!(firsts, seconds);
        // All members retained for failover.
        assert_eq!(first.len(), 3);
    }

    #[test]
    fn session_hash_distributes_across_keys() {
        let eps = [
            endpoint("a", "http://a", 10, 100, true, true),
            endpoint("b", "http://b", 20, 100, true, true),
            endpoint("c", "http://c", 30, 100, true, true),
        ];
        let refs: Vec<&ResolvedEndpoint> = eps.iter().collect();
        // Different keys should not all land on the same home endpoint.
        let homes: std::collections::HashSet<String> = (0..40)
            .map(|i| {
                session_hash_order(refs.clone(), &format!("k{i}"))[0]
                    .id
                    .clone()
            })
            .collect();
        assert!(
            homes.len() > 1,
            "session_hash should spread homes across keys"
        );
    }

    #[test]
    fn session_hash_empty_key_keeps_membership() {
        let eps = [
            endpoint("a", "http://a", 10, 100, true, true),
            endpoint("b", "http://b", 20, 100, true, true),
        ];
        let refs: Vec<&ResolvedEndpoint> = eps.iter().collect();
        let ordered = session_hash_order(refs, "");
        assert_eq!(ordered.len(), 2);
    }

    // --- conversation resolver tests ---

    #[test]
    fn resolve_prefers_explicit_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", "sess-abc".parse().unwrap());
        let json = serde_json::json!({"messages":[{"role":"user","content":"hi"}]});
        let c = resolve_conversation(&headers, &json, Uuid::nil(), true);
        assert_eq!(c.value, "sess-abc");
        assert_eq!(c.source.as_str(), "client");
    }

    #[test]
    fn resolve_prefers_body_session_id_over_derivation() {
        let json = serde_json::json!({
            "session_id": "body-1",
            "messages":[{"role":"user","content":"hi"}]
        });
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), true);
        assert_eq!(c.value, "body-1");
        assert_eq!(c.source.as_str(), "client");
    }

    #[test]
    fn resolve_reads_metadata_session_id() {
        let json = serde_json::json!({
            "metadata": {"session_id": "meta-9"},
            "messages":[{"role":"user","content":"hi"}]
        });
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), true);
        assert_eq!(c.value, "meta-9");
        assert_eq!(c.source.as_str(), "client");
    }

    #[test]
    fn resolve_ignores_user_field() {
        // The OpenAI `user` field must NOT be treated as a session source.
        let json = serde_json::json!({
            "user": "user-123",
            "messages":[{"role":"user","content":"hi"}]
        });
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), true);
        assert_eq!(c.source.as_str(), "derived");
        assert_ne!(c.value, "user-123");
    }

    #[test]
    fn derived_is_stable_across_turns() {
        let tid = Uuid::nil();
        let turn1 = serde_json::json!({"messages":[
            {"role":"system","content":"You are helpful."},
            {"role":"user","content":"What is Rust?"}
        ]});
        let turn3 = serde_json::json!({"messages":[
            {"role":"system","content":"You are helpful."},
            {"role":"user","content":"What is Rust?"},
            {"role":"assistant","content":"A language."},
            {"role":"user","content":"And Go?"}
        ]});
        let a = resolve_conversation(&HeaderMap::new(), &turn1, tid, true);
        let b = resolve_conversation(&HeaderMap::new(), &turn3, tid, true);
        assert_eq!(a.source.as_str(), "derived");
        assert_eq!(a.value, b.value, "same seed across turns must hash equal");
    }

    #[test]
    fn derived_differs_by_opening_and_tenant() {
        let t1 = Uuid::from_u128(1);
        let t2 = Uuid::from_u128(2);
        let q1 = serde_json::json!({"messages":[{"role":"user","content":"alpha"}]});
        let q2 = serde_json::json!({"messages":[{"role":"user","content":"beta"}]});
        let a = resolve_conversation(&HeaderMap::new(), &q1, t1, true);
        let b = resolve_conversation(&HeaderMap::new(), &q2, t1, true);
        let c = resolve_conversation(&HeaderMap::new(), &q1, t2, true);
        assert_ne!(a.value, b.value, "different opener -> different id");
        assert_ne!(a.value, c.value, "different tenant -> different id");
        assert_eq!(a.value.len(), 16, "16 hex chars");
    }

    #[test]
    fn derivation_disabled_yields_none() {
        let json = serde_json::json!({"messages":[{"role":"user","content":"hi"}]});
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), false);
        assert_eq!(c.source.as_str(), "none");
        assert!(c.value.is_empty());
    }

    #[test]
    fn no_messages_yields_none() {
        let json = serde_json::json!({"input": "embed me"});
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), true);
        assert_eq!(c.source.as_str(), "none");
        assert!(c.value.is_empty());
    }

    #[test]
    fn multimodal_first_user_uses_text_parts() {
        let json = serde_json::json!({"messages":[
            {"role":"user","content":[
                {"type":"text","text":"describe"},
                {"type":"image_url","image_url":{"url":"data:..."}}
            ]}
        ]});
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), true);
        assert_eq!(c.source.as_str(), "derived");
        assert_eq!(c.value.len(), 16);
    }

    #[test]
    fn session_hash_sticks_and_repins_when_endpoint_removed() {
        // Two endpoints; a fixed key picks one deterministically.
        let ep_a = "http://a/v1";
        let ep_b = "http://b/v1";
        let m_all = model_with(vec![
            endpoint("a", ep_a, 100, 100, true, true),
            endpoint("b", ep_b, 100, 100, true, true),
        ]);
        let key = "deadbeefdeadbeef";
        let first = build_targets(Some(&m_all), "http://global/v1", "session_hash", key);
        let again = build_targets(Some(&m_all), "http://global/v1", "session_hash", key);
        assert_eq!(first[0].base, again[0].base, "same key -> same primary");

        // Remove whichever endpoint was primary; the survivor must take over.
        let primary = first[0].base.clone();
        let survivor = if primary == ep_a { ep_b } else { ep_a };
        let survivors: Vec<_> = vec![
            endpoint("a", ep_a, 100, 100, true, true),
            endpoint("b", ep_b, 100, 100, true, true),
        ]
        .into_iter()
        .filter(|e| e.api_base != primary)
        .collect();
        let m_one = model_with(survivors);
        let after = build_targets(Some(&m_one), "http://global/v1", "session_hash", key);
        assert_eq!(after.len(), 1);
        assert_eq!(
            after[0].base, survivor,
            "re-pinned to the survivor, not the global fallback"
        );
    }
}
