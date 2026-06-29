//! Model health checks for registered model routes.
//!
//! For each model we first look for a passive signal — recent real client
//! traffic in the ClickHouse usage ledger — and only fall back to an active
//! probe when a model has seen no traffic. The active probe issues a minimal
//! real inference request (`POST /chat/completions` or `POST /embeddings` with
//! `max_tokens: 1`) so the upstream actually executes a forward pass. Probe
//! tokens are accounted under the internal `health_probe` tenant so they never
//! appear in client billing.
//!
//! Transient conditions (overloaded upstream, an unsupported probe endpoint, a
//! single network blip) are classified as `degraded` rather than `unhealthy`
//! so a model doesn't flap to "down" and fire false alerts.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use clickhouse::Row;
use obleth_config::{ModelHealthDetail, ModelHealthSummary, ModelRoute};
use obleth_store::{
    ModelHealthAlertEvent, ModelHealthClaim, ModelHealthConfigUpdate, ModelHealthRecordOutcome,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{audit_actor, AdminError, AdminState, Result};

pub const HEALTH_GROUP: &str = "model-health";
const CHECK_LIMIT: i64 = 50;
const WORKER_CLAIM_LIMIT: i64 = 4;
const WORKER_SLEEP_SECS: u64 = 30;
/// Window of real traffic that counts as a passive health signal.
const PASSIVE_WINDOW_SECS: i64 = 300;
/// Total liveness-probe attempts (one initial try plus up to two retries)
/// before a transient network failure is recorded.
const LIVENESS_MAX_ATTEMPTS: u32 = 3;

/// `request_type` label for usage records emitted by health probes. Lets the
/// request log hide probe traffic by default while keeping every token in the
/// ledger (mirrors the boon pattern, e.g. `guardrails_boon`).
pub const HEALTH_PROBE_REQUEST_TYPE: &str = "health_probe";

pub trait AlertSink: Send + Sync + 'static {
    fn issue(&self, key: String, title: String, detail: String);
}

pub fn health_tenant_id() -> Uuid {
    Uuid::nil()
}

#[derive(Clone)]
pub struct ModelHealthRuntime {
    pub scheduled_enabled: bool,
    pub default_interval_secs: i64,
    pub timeout_secs: u64,
    pub retention_days: i64,
    pub http: reqwest::Client,
    pub alerts: Option<Arc<dyn AlertSink>>,
    /// Sink for emitting `health_probe` usage records. `None` in tests/tools.
    pub telemetry: Option<obleth_telemetry::TelemetrySink>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateModelHealthConfig {
    pub checks_enabled: bool,
    pub alerts_enabled: bool,
    pub check_interval_secs: i64,
    pub failure_threshold: i64,
    pub maintenance_until: Option<DateTime<Utc>>,
    pub maintenance_note: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct BulkModelHealthResult {
    pub checked: Vec<ModelHealthDetail>,
    pub skipped: usize,
}

#[utoipa::path(
    get, path = "/api/v1/models/health", tag = "models",
    responses((status = 200, body = [ModelHealthSummary]))
)]
pub async fn list_health(State(state): State<AdminState>) -> Result<Json<Vec<ModelHealthSummary>>> {
    Ok(Json(state.store.list_model_health_summaries().await?))
}

#[utoipa::path(
    get, path = "/api/v1/models/{id}/health", tag = "models",
    params(("id" = String, Path, description = "Model route id")),
    responses((status = 200, body = ModelHealthDetail))
)]
pub async fn get_health(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ModelHealthDetail>> {
    Ok(Json(
        state.store.get_model_health_detail(id, CHECK_LIMIT).await?,
    ))
}

#[utoipa::path(
    post, path = "/api/v1/models/{id}/health/check", tag = "models",
    params(("id" = String, Path, description = "Model route id")),
    responses((status = 200, body = ModelHealthDetail))
)]
pub async fn check_one(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ModelHealthDetail>> {
    let model = state.store.get_model(id).await?;
    run_and_fetch_detail(&state, model, "manual").await
}

#[utoipa::path(
    post, path = "/api/v1/models/health/check", tag = "models",
    responses((status = 200, body = BulkModelHealthResult))
)]
pub async fn check_all(State(state): State<AdminState>) -> Result<Json<BulkModelHealthResult>> {
    let models = state.store.list_models().await?;
    let health = state.store.list_model_health_summaries().await?;
    let health_by_model: std::collections::HashMap<Uuid, ModelHealthSummary> =
        health.into_iter().map(|row| (row.model_id, row)).collect();
    let now = Utc::now();
    let mut checked = Vec::new();
    let mut skipped = 0usize;

    for model in models {
        let Some(summary) = health_by_model.get(&model.id) else {
            skipped += 1;
            continue;
        };
        let in_maintenance = summary
            .maintenance_until
            .map(|until| until > now)
            .unwrap_or(false);
        if !model.enabled || !summary.checks_enabled || in_maintenance {
            skipped += 1;
            continue;
        }
        checked.push(run_and_fetch_detail(&state, model, "bulk").await?.0);
    }

    Ok(Json(BulkModelHealthResult { checked, skipped }))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/health/config", tag = "models",
    params(("id" = String, Path, description = "Model route id")),
    request_body = UpdateModelHealthConfig,
    responses((status = 200, body = ModelHealthSummary))
)]
pub async fn update_config(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateModelHealthConfig>,
) -> Result<Json<ModelHealthSummary>> {
    let summary = state
        .store
        .update_model_health_config(
            id,
            ModelHealthConfigUpdate {
                checks_enabled: body.checks_enabled,
                alerts_enabled: body.alerts_enabled,
                check_interval_secs: body.check_interval_secs,
                failure_threshold: body.failure_threshold,
                maintenance_until: body.maintenance_until,
                maintenance_note: body
                    .maintenance_note
                    .and_then(|note| clean_optional_text(&note)),
            },
        )
        .await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "update_model_health_config",
            "model",
            &id.to_string(),
            serde_json::json!({
                "checks_enabled": summary.checks_enabled,
                "alerts_enabled": summary.alerts_enabled,
                "check_interval_secs": summary.check_interval_secs,
                "failure_threshold": summary.failure_threshold,
                "maintenance_until": summary.maintenance_until.clone(),
                "maintenance_note": summary.maintenance_note.clone(),
            }),
        )
        .await?;
    Ok(Json(summary))
}

pub fn spawn_worker(state: AdminState) {
    if !state.health.scheduled_enabled {
        tracing::info!("model health scheduler disabled");
        return;
    }

    tokio::spawn(async move {
        let mut next_cleanup = Utc::now();
        loop {
            match state
                .store
                .claim_due_model_health_checks(WORKER_CLAIM_LIMIT)
                .await
            {
                Ok(claims) => {
                    for claim in claims {
                        let state = state.clone();
                        tokio::spawn(async move {
                            if let Err(error) = run_claimed_check(&state, claim).await {
                                tracing::warn!(%error, "scheduled model health check failed");
                            }
                        });
                    }
                }
                Err(error) => tracing::warn!(%error, "failed to claim model health checks"),
            }

            if Utc::now() >= next_cleanup {
                let cutoff =
                    Utc::now() - chrono::Duration::days(state.health.retention_days.max(1));
                match state.store.delete_model_health_checks_before(cutoff).await {
                    Ok(deleted) if deleted > 0 => {
                        tracing::info!(deleted, "pruned old model health checks");
                    }
                    Ok(_) => {}
                    Err(error) => tracing::warn!(%error, "failed to prune model health checks"),
                }
                next_cleanup = Utc::now() + chrono::Duration::hours(1);
            }

            tokio::time::sleep(Duration::from_secs(WORKER_SLEEP_SECS)).await;
        }
    });
}

async fn run_claimed_check(state: &AdminState, claim: ModelHealthClaim) -> Result<()> {
    let outcome = run_model_health_check(state, claim.model, "scheduled").await?;
    maybe_alert(state, &outcome);
    Ok(())
}

async fn run_and_fetch_detail(
    state: &AdminState,
    model: ModelRoute,
    trigger: &str,
) -> Result<Json<ModelHealthDetail>> {
    let outcome = run_model_health_check(state, model, trigger).await?;
    maybe_alert(state, &outcome);
    Ok(Json(
        state
            .store
            .get_model_health_detail(outcome.summary.model_id, CHECK_LIMIT)
            .await?,
    ))
}

async fn run_model_health_check(
    state: &AdminState,
    model: ModelRoute,
    trigger: &str,
) -> Result<ModelHealthRecordOutcome> {
    // Probe each configured endpoint first, both so the data plane can route
    // around dead clusters independently of the model-level signal, and so
    // dynamic-endpoint models (Slurm-provisioned, with no static api_base) can
    // derive their model-level status from the live pool below.
    let endpoint_results = if model.enabled {
        probe_endpoints(state, &model).await
    } else {
        Vec::new()
    };

    let status = if !model.enabled {
        ProbeResult {
            status: "disabled".to_string(),
            latency_ms: None,
            http_status: None,
            message: Some("model route is disabled".to_string()),
            response_excerpt: None,
        }
    } else if let Some(passive) = passive_signal(state, &model).await {
        passive
    } else if model.api_base.trim().is_empty() {
        // No static api_base (Slurm-provisioned / dynamic endpoints): the model
        // is only as healthy as its live endpoint pool. Probing the empty base
        // would always report "unreachable" and flap the model to "down".
        // Use min_replicas from the managed spec as the health floor; fall back
        // to 1 when the spec is absent (e.g. a transient race or data gap).
        let min_replicas = state
            .store
            .get_managed_min_replicas(model.id)
            .await
            .ok()
            .flatten()
            .unwrap_or(1);
        aggregate_endpoint_health(&endpoint_results, min_replicas)
    } else {
        liveness_probe(state, &model).await
    };
    let summary = state.store.get_model_health_summary(model.id).await?;
    let next_check_at = jittered_next_check_at(summary.check_interval_secs);
    state
        .store
        .record_model_health_check(
            model.id,
            trigger,
            &status.status,
            status.latency_ms,
            status.http_status,
            status.message.as_deref(),
            status.response_excerpt.as_deref(),
            next_check_at,
        )
        .await
        .map_err(AdminError::from)
}

async fn passive_signal(state: &AdminState, model: &ModelRoute) -> Option<ProbeResult> {
    let row = recent_traffic(state, &model.model_name, PASSIVE_WINDOW_SECS).await?;
    if row.requests == 0 {
        // No recent traffic to judge by; fall back to an active probe.
        return None;
    }
    if row.successes > 0 {
        return Some(ProbeResult::healthy(
            None,
            None,
            format!(
                "passive: {} successful request(s) in the last {PASSIVE_WINDOW_SECS}s",
                row.successes
            ),
        ));
    }
    if row.server_errors > 0 {
        return Some(ProbeResult::unhealthy(
            None,
            None,
            format!(
                "passive: {} upstream server error(s) and no successes in the last {PASSIVE_WINDOW_SECS}s",
                row.server_errors
            ),
        ));
    }
    // Only client-side (4xx) errors in the window reflect caller mistakes, not
    // model health, so the signal is inconclusive — probe actively.
    None
}

async fn recent_traffic(
    state: &AdminState,
    model_name: &str,
    window_secs: i64,
) -> Option<RecentTraffic> {
    let since = Utc::now().timestamp_millis() - window_secs.max(1) * 1000;
    let sql = "select count() as requests, \
               countIf(status_code >= 200 and status_code < 300) as successes, \
               countIf(status_code >= 500) as server_errors \
               from usage where model = ? and ts_ms >= ?";
    match state
        .clickhouse
        .query(sql)
        .bind(model_name)
        .bind(since)
        .fetch_one::<RecentTraffic>()
        .await
    {
        Ok(row) => Some(row),
        Err(error) => {
            tracing::warn!(%error, model = model_name, "passive health lookup failed; using active probe");
            None
        }
    }
}

async fn liveness_probe(state: &AdminState, model: &ModelRoute) -> ProbeResult {
    inference_probe(state, model, &model.api_base, model.api_key.as_deref()).await
}

/// Minimal real-inference liveness probe against an arbitrary `api_base`.
/// Used for both the model-level probe and per-endpoint probes. Emits a
/// `health_probe` usage record (internal tenant) so probe tokens are accounted.
async fn inference_probe(
    state: &AdminState,
    model: &ModelRoute,
    api_base: &str,
    api_key: Option<&str>,
) -> ProbeResult {
    let Some(req) = build_probe_request(api_base, &model.model_type, &model.upstream_model) else {
        return ProbeResult::unknown(format!(
            "model type `{}` is not auto-probed; status unverified",
            model.model_type
        ));
    };

    let timeout = Duration::from_secs(state.health.timeout_secs.max(1));
    let client = &state.health.http;
    let mut attempt = 0u32;

    loop {
        attempt += 1;
        let mut request = client.post(&req.url).timeout(timeout).json(&req.body);
        if let Some(key) = api_key {
            request = request.bearer_auth(key);
        }
        let started = Instant::now();
        let response = request.send().await;
        let latency_ms: Option<i64> = started.elapsed().as_millis().try_into().ok();

        match response {
            Ok(res) => {
                let code = res.status().as_u16();
                let retryable = code == 408 || code == 429 || res.status().is_server_error();
                if retryable && attempt < LIVENESS_MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(100 * 2u64.pow(attempt - 1))).await;
                    continue;
                }
                let body = res.text().await.unwrap_or_default();
                let (input_tokens, output_tokens) = probe_token_usage(&body);
                emit_probe_usage(state, model, Some(code), latency_ms, input_tokens, output_tokens);
                let status = classify_probe(Some(code));
                let message = match status {
                    "healthy" => format!("real probe succeeded (HTTP {code})"),
                    "unhealthy" => format!("real probe rejected by upstream (HTTP {code})"),
                    _ => format!("real probe inconclusive (HTTP {code})"),
                };
                return ProbeResult {
                    status: status.to_string(),
                    latency_ms,
                    http_status: Some(code as i64),
                    message: Some(normalize_excerpt(&message, 240)),
                    response_excerpt: None,
                };
            }
            Err(error) => {
                let retryable = error.is_timeout() || error.is_connect() || error.is_request();
                if retryable && attempt < LIVENESS_MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(100 * 2u64.pow(attempt - 1))).await;
                    continue;
                }
                emit_probe_usage(state, model, None, latency_ms, 0, 0);
                return ProbeResult {
                    status: classify_probe(None).to_string(),
                    latency_ms,
                    http_status: None,
                    message: Some(normalize_excerpt(
                        &format!("upstream is unreachable: {error}"),
                        240,
                    )),
                    response_excerpt: None,
                };
            }
        }
    }
}

/// Pull `usage.prompt_tokens` / `usage.completion_tokens` from an OpenAI-style
/// response body; returns (0, 0) when absent.
fn probe_token_usage(body: &str) -> (u32, u32) {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return (0, 0);
    };
    let usage = json.get("usage");
    let get = |k: &str| {
        usage
            .and_then(|u| u.get(k))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32
    };
    (get("prompt_tokens"), get("completion_tokens"))
}

/// Record a probe's token usage in the ledger when a sink is configured.
fn emit_probe_usage(
    state: &AdminState,
    model: &ModelRoute,
    http_status: Option<u16>,
    latency_ms: Option<i64>,
    input_tokens: u32,
    output_tokens: u32,
) {
    if let Some(sink) = state.health.telemetry.as_ref() {
        let total_ms = latency_ms.unwrap_or(0).max(0) as u32;
        sink.record(build_probe_usage(model, http_status, total_ms, input_tokens, output_tokens));
    }
}

/// Actively probe each enabled endpoint of a model and record its health so the
/// data plane can route around dead clusters. Disabled endpoints are recorded
/// as `disabled` (which resets their failure count) without a network call.
/// Returns each endpoint's probe result so callers can derive a model-level
/// status for dynamic-endpoint models (see `aggregate_endpoint_health`).
async fn probe_endpoints(state: &AdminState, model: &ModelRoute) -> Vec<ProbeResult> {
    let endpoints = match state.store.list_model_endpoints(model.id).await {
        Ok(e) => e,
        Err(error) => {
            tracing::warn!(%error, model = %model.model_name, "failed to list endpoints for health probe");
            return Vec::new();
        }
    };
    if endpoints.is_empty() {
        return Vec::new();
    }
    // Decrypted keys for the actual probe call.
    let resolved = state
        .store
        .resolved_endpoints_for(model.id)
        .await
        .unwrap_or_default();
    let key_by_id: std::collections::HashMap<&str, Option<&str>> = resolved
        .iter()
        .map(|e| (e.id.as_str(), e.api_key.as_deref()))
        .collect();

    let mut results = Vec::with_capacity(endpoints.len());
    for endpoint in &endpoints {
        let result = if !endpoint.enabled {
            ProbeResult {
                status: "disabled".to_string(),
                latency_ms: None,
                http_status: None,
                message: Some("endpoint is disabled".to_string()),
                response_excerpt: None,
            }
        } else {
            let api_key = key_by_id
                .get(endpoint.id.to_string().as_str())
                .copied()
                .flatten();
            inference_probe(state, model, &endpoint.api_base, api_key).await
        };
        if let Err(error) = state
            .store
            .record_endpoint_health(
                endpoint.id,
                &result.status,
                result.latency_ms,
                result.http_status,
                result.message.as_deref(),
            )
            .await
        {
            tracing::warn!(%error, endpoint = %endpoint.name, "failed to record endpoint health");
        }
        results.push(result);
    }
    results
}

/// Derive a model-level status from its endpoint pool, for models with no static
/// `api_base` (Slurm-provisioned / dynamic). `disabled` endpoints are ignored.
///
/// For provisioner-managed endpoints the individual probe returns `degraded`
/// when the endpoint is reachable but the upstream_model doesn't appear in
/// `/v1/models` — this happens when the model ID format in the gateway (e.g.
/// `qwen2.5-0.5b`) differs from what the serving framework advertises (e.g.
/// `qwen2.5:0.5b` in Ollama). The provisioner's own health check already
/// confirmed the model is serving before promoting the replica, so reachability
/// is the correct signal here. Both `healthy` and `degraded` endpoint results
/// are therefore treated as "serving" for the model-level aggregate.
///
/// Model status is then determined by capacity relative to `min_replicas`:
/// - `serving >= min_replicas` → healthy (the configured health floor is met)
/// - `0 < serving < min_replicas` → degraded (partial capacity, below the floor)
/// - `serving == 0` → unhealthy
fn aggregate_endpoint_health(results: &[ProbeResult], min_replicas: i64) -> ProbeResult {
    let live: Vec<&ProbeResult> = results.iter().filter(|r| r.status != "disabled").collect();
    let total = live.len();
    if total == 0 {
        return ProbeResult::unhealthy(
            None,
            None,
            "no live endpoints registered for this model".to_string(),
        );
    }
    // "serving" = reachable (200 response), regardless of whether the upstream
    // model was confirmed present in /v1/models.
    let serving = live
        .iter()
        .filter(|r| r.status == "healthy" || r.status == "degraded")
        .count();
    let floor = min_replicas.max(1) as usize;
    if serving >= floor {
        return ProbeResult::healthy(
            None,
            None,
            format!("{serving} of {total} endpoint(s) serving"),
        );
    }
    if serving > 0 {
        return ProbeResult::degraded(
            None,
            None,
            format!("{serving} of {total} endpoint(s) serving (below min_replicas {floor})"),
        );
    }
    ProbeResult::unhealthy(None, None, format!("all {total} endpoint(s) unreachable"))
}

struct ProbeRequest {
    url: String,
    body: serde_json::Value,
}

/// Build the minimal real inference request used to verify a model actually
/// serves. `api_base` already includes the `/v1` suffix. Returns `None` for
/// model types we deliberately do not auto-probe because a "minimal" request
/// is still costly (image / audio) or the type is unrecognized.
fn build_probe_request(api_base: &str, model_type: &str, upstream_model: &str) -> Option<ProbeRequest> {
    let base = api_base.trim_end_matches('/');
    match model_type {
        "chat" => Some(ProbeRequest {
            url: format!("{base}/chat/completions"),
            body: serde_json::json!({
                "model": upstream_model,
                "messages": [{ "role": "user", "content": "ping" }],
                "max_tokens": 1,
                "stream": false,
            }),
        }),
        "embedding" => Some(ProbeRequest {
            url: format!("{base}/embeddings"),
            body: serde_json::json!({ "model": upstream_model, "input": "ping" }),
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, Row, Deserialize)]
struct RecentTraffic {
    requests: u64,
    successes: u64,
    server_errors: u64,
}

fn maybe_alert(state: &AdminState, outcome: &ModelHealthRecordOutcome) {
    let Some(alerts) = state.health.alerts.as_ref() else {
        return;
    };
    let Some(event) = outcome.alert_event else {
        return;
    };
    let model = &outcome.summary.model_name;
    match event {
        ModelHealthAlertEvent::Down => alerts.issue(
            format!("model_health_down:{}", outcome.summary.model_id),
            format!("Model `{model}` is unhealthy"),
            format!(
                "Status `{}` after {} consecutive failed check(s). Last HTTP status: {}. {}",
                outcome.summary.status,
                outcome.summary.consecutive_failures,
                outcome
                    .summary
                    .last_http_status
                    .map(|status| status.to_string())
                    .unwrap_or_else(|| "none".to_string()),
                outcome
                    .summary
                    .last_message
                    .clone()
                    .unwrap_or_else(|| "No message recorded.".to_string()),
            ),
        ),
        ModelHealthAlertEvent::Recovery => alerts.issue(
            format!("model_health_recovery:{}", outcome.summary.model_id),
            format!("Model `{model}` recovered"),
            format!(
                "Latest health check succeeded in {} ms.",
                outcome
                    .summary
                    .last_latency_ms
                    .map(|ms| ms.to_string())
                    .unwrap_or_else(|| "unknown".to_string()),
            ),
        ),
    }
}

fn jittered_next_check_at(interval_secs: i64) -> DateTime<Utc> {
    let base = interval_secs.max(60);
    let jitter_limit = (base / 10).clamp(1, 120);
    let jitter = rand::thread_rng().gen_range(0..=jitter_limit);
    Utc::now() + chrono::Duration::seconds(base + jitter)
}

fn build_probe_usage(
    model: &ModelRoute,
    http_status: Option<u16>,
    total_ms: u32,
    input_tokens: u32,
    output_tokens: u32,
) -> obleth_config::UsageRecord {
    let cost_usd = input_tokens as f64 * model.input_cost_per_token
        + output_tokens as f64 * model.output_cost_per_token;
    obleth_config::UsageRecord {
        request_id: Uuid::new_v4(),
        tenant_id: health_tenant_id(),
        key_id: Uuid::nil(),
        model: model.model_name.clone(),
        admission: "health_probe".to_string(),
        weight: 0,
        input_tokens,
        output_tokens,
        estimated_tokens: input_tokens,
        queue_wait_ms: 0,
        ttft_ms: 0,
        total_ms,
        status_code: http_status.unwrap_or(0),
        cache_status: "off".to_string(),
        cost_usd,
        ts_ms: Utc::now().timestamp_millis(),
        session_id: String::new(),
        session_id_source: "none".to_string(),
        request_type: HEALTH_PROBE_REQUEST_TYPE.to_string(),
    }
}

fn clean_optional_text(value: &str) -> Option<String> {
    let value = normalize_excerpt(value, 240);
    (!value.is_empty()).then_some(value)
}

#[derive(Debug)]
struct ProbeResult {
    status: String,
    latency_ms: Option<i64>,
    http_status: Option<i64>,
    message: Option<String>,
    response_excerpt: Option<String>,
}

impl ProbeResult {
    fn healthy(latency_ms: Option<i64>, http_status: Option<i64>, message: String) -> Self {
        Self {
            status: "healthy".to_string(),
            latency_ms,
            http_status,
            message: Some(normalize_excerpt(&message, 240)),
            response_excerpt: None,
        }
    }

    fn degraded(latency_ms: Option<i64>, http_status: Option<i64>, message: String) -> Self {
        Self {
            status: "degraded".to_string(),
            latency_ms,
            http_status,
            message: Some(normalize_excerpt(&message, 240)),
            response_excerpt: None,
        }
    }

    fn unhealthy(latency_ms: Option<i64>, http_status: Option<i64>, message: String) -> Self {
        Self {
            status: "unhealthy".to_string(),
            latency_ms,
            http_status,
            message: Some(normalize_excerpt(&message, 240)),
            response_excerpt: None,
        }
    }

    fn unknown(message: String) -> Self {
        Self {
            status: "unknown".to_string(),
            latency_ms: None,
            http_status: None,
            message: Some(normalize_excerpt(&message, 240)),
            response_excerpt: None,
        }
    }
}

fn normalize_excerpt(value: &str, max: usize) -> String {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.len() <= max {
        return normalized;
    }
    let mut end = max.saturating_sub(1);
    while !normalized.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}...", &normalized[..end])
}

/// Map a probe's final HTTP status to a health status. A clear model/route or
/// auth error is actionable (`unhealthy`, alertable); transient/overload and
/// other inconclusive codes stay `degraded` so a working model never flaps to
/// down on a single blip. `None` = transport failure after retries.
fn classify_probe(http: Option<u16>) -> &'static str {
    match http {
        Some(c) if (200..300).contains(&c) => "healthy",
        Some(400) | Some(401) | Some(403) | Some(404) | Some(422) => "unhealthy",
        Some(408) | Some(429) => "degraded",
        Some(c) if c >= 500 => "degraded",
        Some(_) => "degraded",
        None => "unhealthy",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_collapses_and_truncates() {
        let value = normalize_excerpt(" a\n b\t ccccc ", 7);
        assert_eq!(value, "a b cc...");
    }

    #[test]
    fn aggregate_empty_pool_is_unhealthy() {
        let agg = aggregate_endpoint_health(&[], 1);
        assert_eq!(agg.status, "unhealthy");
    }

    #[test]
    fn aggregate_all_serving_is_healthy() {
        // healthy and degraded both count as "serving"; all-serving → healthy
        let results = vec![
            ProbeResult::healthy(None, None, "up".into()),
            ProbeResult::degraded(None, None, "model-id-mismatch".into()),
        ];
        assert_eq!(aggregate_endpoint_health(&results, 1).status, "healthy");
    }

    #[test]
    fn aggregate_partial_serving_is_degraded() {
        // some (but not all) endpoints serving → degraded (partial capacity)
        let results = vec![
            ProbeResult::unhealthy(None, None, "down".into()),
            ProbeResult::degraded(None, None, "model-id-mismatch".into()),
        ];
        assert_eq!(aggregate_endpoint_health(&results, 2).status, "degraded");
    }

    #[test]
    fn aggregate_all_down_is_unhealthy() {
        let results = vec![
            ProbeResult::unhealthy(None, None, "down".into()),
            ProbeResult::unhealthy(None, None, "down".into()),
        ];
        assert_eq!(aggregate_endpoint_health(&results, 1).status, "unhealthy");
    }

    #[test]
    fn aggregate_ignores_disabled_endpoints() {
        let disabled = ProbeResult {
            status: "disabled".to_string(),
            latency_ms: None,
            http_status: None,
            message: None,
            response_excerpt: None,
        };
        // Only a disabled endpoint -> treated as no live endpoints -> unhealthy.
        assert_eq!(
            aggregate_endpoint_health(&[disabled], 1).status,
            "unhealthy"
        );
    }

    // --- min_replicas floor tests ---

    #[test]
    fn min_replicas_floor_zero_healthy_is_down() {
        // 0 serving, min_replicas=2 → unhealthy
        let results = vec![
            ProbeResult::unhealthy(None, None, "down".into()),
            ProbeResult::unhealthy(None, None, "down".into()),
        ];
        assert_eq!(aggregate_endpoint_health(&results, 2).status, "unhealthy");
    }

    #[test]
    fn min_replicas_floor_below_threshold_is_degraded() {
        // 1 serving, min_replicas=2 → degraded (below the healthy floor)
        let results = vec![
            ProbeResult::healthy(None, None, "up".into()),
            ProbeResult::unhealthy(None, None, "down".into()),
        ];
        assert_eq!(aggregate_endpoint_health(&results, 2).status, "degraded");
    }

    #[test]
    fn min_replicas_floor_at_threshold_is_healthy() {
        // 2 serving, min_replicas=2 → healthy (at the floor)
        let results = vec![
            ProbeResult::healthy(None, None, "up".into()),
            ProbeResult::healthy(None, None, "up".into()),
        ];
        assert_eq!(aggregate_endpoint_health(&results, 2).status, "healthy");
    }

    #[test]
    fn min_replicas_floor_above_threshold_is_healthy() {
        // 3 serving, min_replicas=2 → healthy (above the floor)
        let results = vec![
            ProbeResult::healthy(None, None, "up".into()),
            ProbeResult::healthy(None, None, "up".into()),
            ProbeResult::healthy(None, None, "up".into()),
        ];
        assert_eq!(aggregate_endpoint_health(&results, 2).status, "healthy");
    }

    #[test]
    fn probe_request_chat_is_one_token_completion() {
        let r = build_probe_request("https://up/v1", "chat", "kimi-k2").expect("chat probe");
        assert_eq!(r.url, "https://up/v1/chat/completions");
        assert_eq!(r.body["model"], "kimi-k2");
        assert_eq!(r.body["max_tokens"], 1);
        assert_eq!(r.body["stream"], false);
        assert!(r.body["messages"].is_array());
    }

    #[test]
    fn probe_request_embedding_uses_embeddings_endpoint() {
        let r = build_probe_request("https://up/v1/", "embedding", "qwen4-embedding").expect("embed");
        assert_eq!(r.url, "https://up/v1/embeddings");
        assert_eq!(r.body["model"], "qwen4-embedding");
        assert_eq!(r.body["input"], "ping");
    }

    #[test]
    fn probe_request_costly_and_unknown_modes_are_none() {
        assert!(build_probe_request("https://up/v1", "image", "m").is_none());
        assert!(build_probe_request("https://up/v1", "audio_transcription", "m").is_none());
        assert!(build_probe_request("https://up/v1", "audio_speech", "m").is_none());
        assert!(build_probe_request("https://up/v1", "something-else", "m").is_none());
    }

    #[test]
    fn classify_probe_success_is_healthy() {
        assert_eq!(classify_probe(Some(200)), "healthy");
        assert_eq!(classify_probe(Some(201)), "healthy");
    }

    #[test]
    fn classify_probe_model_and_auth_errors_are_unhealthy() {
        // model removed upstream → LiteLLM returns 400/404; bad creds → 401/403.
        assert_eq!(classify_probe(Some(400)), "unhealthy");
        assert_eq!(classify_probe(Some(404)), "unhealthy");
        assert_eq!(classify_probe(Some(422)), "unhealthy");
        assert_eq!(classify_probe(Some(401)), "unhealthy");
        assert_eq!(classify_probe(Some(403)), "unhealthy");
    }

    #[test]
    fn classify_probe_transient_is_degraded() {
        assert_eq!(classify_probe(Some(408)), "degraded");
        assert_eq!(classify_probe(Some(429)), "degraded");
        assert_eq!(classify_probe(Some(500)), "degraded");
        assert_eq!(classify_probe(Some(503)), "degraded");
        assert_eq!(classify_probe(Some(418)), "degraded"); // other 4xx: inconclusive
    }

    #[test]
    fn classify_probe_transport_error_is_unhealthy() {
        assert_eq!(classify_probe(None), "unhealthy");
    }

    fn sample_model() -> ModelRoute {
        serde_json::from_value(serde_json::json!({
            "id": uuid::Uuid::nil(),
            "model_name": "m", "description": "", "upstream_model": "m",
            "api_base": "https://up/v1", "api_key": null, "model_type": "chat",
            "input_cost_per_token": 0.0, "output_cost_per_token": 0.0,
            "context_window": 8192, "admission_weight": 100, "max_in_flight": null,
            "supports_function_calling": false, "supports_system_messages": true,
            "supports_response_schema": false, "supports_tool_choice": false,
            "enabled": true, "cache_enabled": false, "cache_ttl_secs": 300,
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
        })).expect("sample model")
    }

    #[test]
    fn probe_usage_is_internal_and_tagged() {
        let mut model = sample_model();
        model.model_name = "kimi-k2".into();
        model.input_cost_per_token = 0.001;
        model.output_cost_per_token = 0.002;
        let rec = build_probe_usage(&model, Some(200), 42, 3, 1);
        assert_eq!(rec.tenant_id, health_tenant_id());
        assert_eq!(rec.key_id, uuid::Uuid::nil());
        assert_eq!(rec.request_type, HEALTH_PROBE_REQUEST_TYPE);
        assert_eq!(rec.model, "kimi-k2");
        assert_eq!(rec.status_code, 200);
        assert_eq!(rec.input_tokens, 3);
        assert_eq!(rec.output_tokens, 1);
        // cost = 3*0.001 + 1*0.002
        assert!((rec.cost_usd - 0.005).abs() < 1e-9);
    }
}
