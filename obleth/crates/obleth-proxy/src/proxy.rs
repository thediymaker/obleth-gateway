//! The data-plane request pipeline.
//!
//! resolve key -> estimate cost -> fairshare admit -> (brownout) -> reserve
//! budget -> stream to upstream -> reconcile actual cost -> emit telemetry.
//!
//! The fairshare permit is held inside the response stream and released only
//! when the stream finishes, so concurrency accounting matches real upstream
//! occupancy including streaming time.

use std::time::Instant;

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Request, Response, StatusCode};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use obleth_config::{hash_api_key, Admission, ResolvedKey, ResolvedModel, UsageRecord};
use obleth_tokenizer::{CostEstimate, Tokenizer};
use tracing::Instrument;
use uuid::Uuid;

use crate::state::AppState;

const BODY_LIMIT: usize = 64 * 1024 * 1024;
const BROWNOUT_MAX_TOKENS: u64 = 256;
const TAIL_CAP: usize = 16 * 1024;
/// Upper bound on a response we are willing to cache. Larger responses stream
/// through uncached so the cache can't be used to balloon Redis memory.
const CACHE_MAX_BYTES: usize = 512 * 1024;

#[tracing::instrument(skip_all, name = "proxy_request")]
pub async fn proxy_handler(State(state): State<AppState>, req: Request<Body>) -> Response<Body> {
    let request_start = Instant::now();
    let (parts, body) = req.into_parts();
    let method = parts.method;
    let path = parts.uri.path().to_string();
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let headers = parts.headers;

    // ---- auth ----
    let Some(secret) = bearer(&headers) else {
        return error_json(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    let hash = hash_api_key(&secret);
    let resolved = match resolve_key(&state, &hash).await {
        Some(r) => r,
        None => return error_json(StatusCode::UNAUTHORIZED, "invalid api key"),
    };
    if resolved.disabled {
        return error_json(StatusCode::FORBIDDEN, "api key disabled");
    }

    // ---- read + parse body ----
    let body_bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => return error_json(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };
    let mut json: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
    let model = json
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();
    let route = resolve_model(&state, &model).await;
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
    let effective_weight = effective_admission_weight(resolved.weight, route.as_ref());
    let mut est = state.tokenizer.estimate_request(&json);

    // ---- response cache (exact-match, before admission so hits cost nothing) ----
    let cache_enabled = route.as_ref().map(|r| r.cache_enabled).unwrap_or(false);
    let cache_ttl = route.as_ref().map(|r| r.cache_ttl_secs).unwrap_or(0);
    let cache_key = cache_enabled.then(|| obleth_config::cache_key(&model, &body_bytes));
    if let Some(ck) = &cache_key {
        match state
            .redis
            .cache_get(ck)
            .instrument(tracing::info_span!("cache_lookup"))
            .await
        {
            Ok(Some(cached)) => {
                state.metrics.record_cache(
                    true,
                    cached.input_tokens.saturating_add(cached.output_tokens),
                );
                finalize(
                    &state,
                    &resolved,
                    &model,
                    Admission::Fast,
                    est,
                    cached.input_tokens,
                    cached.output_tokens,
                    0,
                    0,
                    cached.status,
                    "hit",
                );
                return cached_response(cached);
            }
            Ok(None) => state.metrics.record_cache(false, 0),
            Err(e) => {
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

    // ---- brownout: degrade low-priority requests instead of rejecting ----
    let mut send_bytes = body_bytes;
    if matches!(admission, Admission::Brownout) {
        if let Some(degraded) = apply_brownout(&mut json) {
            send_bytes = Bytes::from(degraded);
            est = state.tokenizer.estimate_request(&json);
        }
    }

    // ---- token budget reserve (atomic, cross-pod) ----
    let capacity = resolved.tokens_per_minute.max(1);
    match state
        .redis
        .reserve_budget(
            &resolved.tenant_id,
            capacity,
            resolved.tokens_per_minute,
            est.total(),
        )
        .instrument(tracing::info_span!("reserve_budget"))
        .await
    {
        Ok((true, _)) => {}
        Ok((false, _)) => {
            drop(permit);
            finalize(
                &state,
                &resolved,
                &model,
                Admission::Rejected,
                est,
                0,
                0,
                queue_wait_ms,
                0,
                429,
                cache_status_label,
            );
            return error_json(StatusCode::TOO_MANY_REQUESTS, "token budget exceeded");
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

    // ---- proxy upstream ----
    let (upstream_base, mut send_bytes, upstream_model) =
        prepare_upstream(&route, &state.upstream_base, &model, &json, send_bytes);
    if let Some(upstream_model) = upstream_model {
        if let Ok(bytes) = serde_json::to_vec(&upstream_model) {
            send_bytes = Bytes::from(bytes);
            est = state.tokenizer.estimate_request(&upstream_model);
        }
    }
    let url = build_upstream_url(&upstream_base, &path, &query);
    let mut fwd_headers = forward_headers(&headers);
    if let Some(route) = &route {
        if let Some(key) = &route.api_key {
            if let Ok(v) = header::HeaderValue::from_str(&format!("Bearer {key}")) {
                fwd_headers.insert(header::AUTHORIZATION, v);
            }
        }
    }
    let upstream = state
        .http
        .request(method, &url)
        .headers(fwd_headers)
        .body(send_bytes)
        .send()
        .instrument(tracing::info_span!("upstream_request"))
        .await;
    let upstream = match upstream {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "upstream request failed");
            state.alerts.issue(
                "upstream_request_failed",
                "Upstream request failed",
                format!(
                    "tenant `{}` model `{model}` path `{path}` upstream `{url}`: {e}",
                    resolved.tenant_name
                ),
            );
            drop(permit);
            finalize(
                &state,
                &resolved,
                &model,
                admission,
                est,
                0,
                0,
                queue_wait_ms,
                0,
                502,
                cache_status_label,
            );
            return error_json(StatusCode::BAD_GATEWAY, "upstream request failed");
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

    // Cache only successful, non-degraded responses. Brownout output is capped,
    // so storing it would poison later (unsaturated) reads.
    let store_in_cache = cache_key
        .clone()
        .filter(|_| !matches!(admission, Admission::Brownout));

    // ---- stream back, inspecting for actual usage, then reconcile ----
    let stream_state = state.clone();
    let resolved_for_stream = resolved.clone();
    let body_stream = async_stream::stream! {
        let mut byte_stream = upstream.bytes_stream();
        let mut first = true;
        let mut ttft_ms = 0u32;
        let mut tail = String::new();
        let mut full: Vec<u8> = Vec::new();
        let mut cacheable = store_in_cache.is_some();

        while let Some(item) = byte_stream.next().await {
            match item {
                Ok(chunk) => {
                    if first {
                        ttft_ms = request_start.elapsed().as_millis() as u32;
                        first = false;
                    }
                    append_tail(&mut tail, &chunk);
                    if cacheable {
                        if full.len() + chunk.len() <= CACHE_MAX_BYTES {
                            full.extend_from_slice(&chunk);
                        } else {
                            // Too large to cache; stop buffering and free memory.
                            cacheable = false;
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

        let (input_tokens, output_tokens) = extract_usage(&tail)
            .unwrap_or((est.input_tokens, est.estimated_output_tokens));
        let total_ms = request_start.elapsed().as_millis() as u32;

        // store the full response for identical future requests
        if cacheable && status_code == 200 {
            if let Some(ck) = &store_in_cache {
                let cached = obleth_config::CachedResponse {
                    status: status_code,
                    content_type: content_type_str.clone(),
                    body: String::from_utf8_lossy(&full).into_owned(),
                    input_tokens,
                    output_tokens,
                };
                if let Err(e) = stream_state.redis.cache_put(ck, &cached, cache_ttl).await {
                    tracing::warn!(error = %e, "cache store failed");
                    stream_state.alerts.issue(
                        "redis_cache_store_failed",
                        "Redis response-cache store failed",
                        format!(
                            "tenant `{}` model `{model}` cache ttl `{cache_ttl}`: {e}",
                            resolved_for_stream.tenant_name
                        ),
                    );
                }
            }
        }

        // reconcile estimate vs actual against the budget bucket
        if let Err(e) = stream_state
            .redis
            .reconcile_budget(
                &resolved_for_stream.tenant_id,
                capacity,
                est.total(),
                input_tokens.saturating_add(output_tokens),
            )
            .await
        {
            tracing::warn!(error = %e, "budget reconcile failed");
            stream_state.alerts.issue(
                "redis_budget_reconcile_failed",
                "Redis budget reconcile failed",
                format!(
                    "tenant `{}` model `{model}` estimated `{}` actual `{}`: {e}",
                    resolved_for_stream.tenant_name,
                    est.total(),
                    input_tokens.saturating_add(output_tokens),
                ),
            );
        }

        finalize(
            &stream_state,
            &resolved_for_stream,
            &model,
            admission,
            est,
            input_tokens,
            output_tokens,
            queue_wait_ms,
            ttft_ms,
            status_code,
            cache_status_label,
        );
        stream_state.metrics.total_ms.observe(total_ms as f64);

        // permit released here, after the full stream has drained
        drop(permit);
    };

    let mut builder = Response::builder().status(status_code);
    builder = builder.header(header::CONTENT_TYPE, content_type);
    builder
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

/// Resolve a key via moka, falling back to Redis and caching the result.
#[tracing::instrument(skip_all, name = "auth_resolve")]
pub(crate) async fn resolve_key(state: &AppState, hash: &str) -> Option<ResolvedKey> {
    if let Some(r) = state.key_cache.get(hash).await {
        return Some(r);
    }
    match state.redis.get_resolved_key(hash).await {
        Ok(Some(r)) => {
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

async fn resolve_model(state: &AppState, name: &str) -> Option<ResolvedModel> {
    if let Some(r) = state.model_cache.get(name).await {
        return Some(r);
    }
    match state.redis.get_resolved_model(name).await {
        Ok(Some(r)) => {
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
fn requires_registered_model(path: &str) -> bool {
    matches!(
        path,
        "/v1/chat/completions"
            | "/v1/completions"
            | "/v1/embeddings"
            | "/v1/responses"
            | "/v1/audio/transcriptions"
            | "/v1/audio/translations"
    )
}

fn prepare_upstream(
    route: &Option<ResolvedModel>,
    default_base: &str,
    _client_model: &str,
    json: &serde_json::Value,
    body: Bytes,
) -> (String, Bytes, Option<serde_json::Value>) {
    let Some(route) = route else {
        return (default_base.to_string(), body, None);
    };
    let mut upstream_json = json.clone();
    if let Some(obj) = upstream_json.as_object_mut() {
        obj.insert(
            "model".into(),
            serde_json::Value::String(route.upstream_model.clone()),
        );
    }
    (route.api_base.clone(), body, Some(upstream_json))
}

fn build_upstream_url(base: &str, path: &str, query: &str) -> String {
    let base = base.trim_end_matches('/');
    let mut rel = path.trim_start_matches('/').to_string();
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
            // strip hop-by-hop / auth / encoding so the body stays inspectable
            "host" | "content-length" | "authorization" | "x-api-key" | "accept-encoding"
            | "connection" => continue,
            _ => {
                out.insert(name.clone(), value.clone());
            }
        }
    }
    out
}

/// Cap output under saturation. Returns the re-serialized body if it changed.
fn apply_brownout(json: &mut serde_json::Value) -> Option<Vec<u8>> {
    let obj = json.as_object_mut()?;
    let current = obj
        .get("max_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(u64::MAX);
    let capped = current.min(BROWNOUT_MAX_TOKENS);
    obj.insert("max_tokens".to_string(), serde_json::json!(capped));
    serde_json::to_vec(json).ok()
}

fn append_tail(tail: &mut String, chunk: &[u8]) {
    tail.push_str(&String::from_utf8_lossy(chunk));
    if tail.len() > TAIL_CAP {
        let cut = tail.len() - TAIL_CAP;
        // keep the last TAIL_CAP bytes on a char boundary
        let boundary = (cut..tail.len())
            .find(|i| tail.is_char_boundary(*i))
            .unwrap_or(tail.len());
        *tail = tail[boundary..].to_string();
    }
}

/// Pull `prompt_tokens` / `completion_tokens` out of the (possibly streamed)
/// response tail. Returns `None` if the upstream didn't report usage.
fn extract_usage(tail: &str) -> Option<(u32, u32)> {
    let input = find_int_after(tail, "\"prompt_tokens\"");
    let output = find_int_after(tail, "\"completion_tokens\"");
    match (input, output) {
        (Some(i), Some(o)) => Some((i, o)),
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

#[allow(clippy::too_many_arguments)]
fn finalize(
    state: &AppState,
    resolved: &ResolvedKey,
    model: &str,
    admission: Admission,
    est: CostEstimate,
    input_tokens: u32,
    output_tokens: u32,
    queue_wait_ms: u32,
    ttft_ms: u32,
    status_code: u16,
    cache_status: &str,
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
        request_id: Uuid::new_v4(),
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
        total_ms: 0,
        status_code,
        cache_status: cache_status.to_string(),
        ts_ms: now_ms(),
    });
}

/// Build an HTTP response from a cached entry, replaying the stored body and
/// content-type (works for both JSON and buffered SSE).
fn cached_response(cached: obleth_config::CachedResponse) -> Response<Body> {
    let content_type = header::HeaderValue::from_str(&cached.content_type)
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/json"));
    let status = StatusCode::from_u16(cached.status).unwrap_or(StatusCode::OK);
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header("x-obleth-cache", "hit")
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
    use super::build_upstream_url;

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
}
