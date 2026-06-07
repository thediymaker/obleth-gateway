//! Model health checks for registered model routes.
//!
//! Checks are deliberately cheap and non-billable. For each model we first look
//! for a passive signal — recent real client traffic in the ClickHouse usage
//! ledger — and only fall back to an active probe when a model has seen no
//! traffic. The active probe is a token-free `GET {api_base}/models` liveness
//! call (optionally checking that the upstream actually lists the model), not a
//! real inference request, so probing never consumes a provider budget.
//!
//! Transient conditions (overloaded upstream, an unsupported probe endpoint, a
//! single network blip) are classified as `degraded` rather than `unhealthy`
//! so a model doesn't flap to "down" and fire false alerts.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
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

use crate::{AdminError, AdminState, Result};

pub const HEALTH_GROUP: &str = "model-health";
const CHECK_LIMIT: i64 = 50;
const WORKER_CLAIM_LIMIT: i64 = 4;
const WORKER_SLEEP_SECS: u64 = 30;
/// Window of real traffic that counts as a passive health signal.
const PASSIVE_WINDOW_SECS: i64 = 300;
/// Total liveness-probe attempts (one initial try plus one retry) before a
/// transient network failure is recorded.
const LIVENESS_MAX_ATTEMPTS: u32 = 2;

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
            "admin",
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

/// Token-free liveness probe: `GET {api_base}/models`. A 2xx proves the upstream
/// is serving; when the response lists models we also confirm the route's
/// `upstream_model` is actually loaded.
async fn liveness_probe(state: &AdminState, model: &ModelRoute) -> ProbeResult {
    let url = models_list_url(&model.api_base);
    let timeout = Duration::from_secs(state.health.timeout_secs.max(1));
    let client = &state.health.http;

    let mut attempt = 0u32;
    loop {
        attempt += 1;
        let mut request = client.get(&url).timeout(timeout);
        if let Some(key) = &model.api_key {
            request = request.bearer_auth(key);
        }
        let started = Instant::now();
        let response = request.send().await;
        let latency_ms = started.elapsed().as_millis().try_into().ok();

        match response {
            Ok(res) => {
                let status = res.status();
                let code = status.as_u16();
                let http_status = Some(code as i64);
                // Overload/throttle states are worth one quiet retry before we
                // record anything.
                let retryable = code == 408 || code == 429 || status.is_server_error();
                if retryable && attempt < LIVENESS_MAX_ATTEMPTS {
                    continue;
                }
                if status.is_success() {
                    let body = res.text().await.unwrap_or_default();
                    return match model_in_list(&body, &model.upstream_model) {
                        ModelPresence::Present => ProbeResult::healthy(
                            latency_ms,
                            http_status,
                            format!("upstream is serving and lists `{}`", model.upstream_model),
                        ),
                        // The upstream is up and answered, but doesn't advertise
                        // this model in `/v1/models`. Many shared gateways omit
                        // models from that list (or list slightly different ids),
                        // so absence is a soft "can't confirm" signal, not an
                        // outage \u2014 report `degraded` (no alert, no failure count)
                        // rather than flapping a working model to "down".
                        ModelPresence::Absent => ProbeResult::degraded(
                            latency_ms,
                            http_status,
                            format!(
                                "upstream is reachable but does not advertise `{}` in /v1/models",
                                model.upstream_model
                            ),
                        ),
                        ModelPresence::Unknown => ProbeResult::healthy(
                            latency_ms,
                            http_status,
                            "upstream model-list endpoint responded".to_string(),
                        ),
                    };
                }
                // Rejected credentials are real and actionable. Everything else
                // (unsupported endpoint, a lingering 5xx) is inconclusive and
                // must not flap the model to "down".
                if code == 401 || code == 403 {
                    return ProbeResult::unhealthy(
                        latency_ms,
                        http_status,
                        format!("upstream rejected the probe credentials (HTTP {code})"),
                    );
                }
                return ProbeResult::degraded(
                    latency_ms,
                    http_status,
                    format!("model-list probe was inconclusive (HTTP {code})"),
                );
            }
            Err(error) => {
                let retryable =
                    error.is_timeout() || error.is_connect() || error.is_request();
                if retryable && attempt < LIVENESS_MAX_ATTEMPTS {
                    continue;
                }
                return ProbeResult::unhealthy(
                    latency_ms,
                    None,
                    format!("upstream is unreachable: {error}"),
                );
            }
        }
    }
}

fn models_list_url(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    if base.ends_with("/models") {
        base.to_string()
    } else {
        format!("{base}/models")
    }
}

enum ModelPresence {
    Present,
    Absent,
    Unknown,
}

/// Look for `upstream_model` in an OpenAI-style `{ "data": [{ "id": ... }] }`
/// model list. `Unknown` means the body wasn't a recognizable, non-empty list,
/// so membership can't be judged (treated as a soft pass).
///
/// A `"*"` id is a wildcard (e.g. LiteLLM advertises every route as a single
/// `"*"` entry rather than enumerating them), so it matches any model.
fn model_in_list(body: &str, upstream_model: &str) -> ModelPresence {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return ModelPresence::Unknown;
    };
    let Some(data) = json.get("data").and_then(|d| d.as_array()) else {
        return ModelPresence::Unknown;
    };
    if data.is_empty() {
        return ModelPresence::Unknown;
    }
    let present = data.iter().any(|item| {
        matches!(
            item.get("id").and_then(|v| v.as_str()),
            Some(id) if id == "*" || id == upstream_model
        )
    });
    if present {
        ModelPresence::Present
    } else {
        ModelPresence::Absent
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excerpt_collapses_and_truncates() {
        let value = normalize_excerpt(" a\n b\t ccccc ", 7);
        assert_eq!(value, "a b cc...");
    }

    #[test]
    fn wildcard_model_list_matches_any_model() {
        let body = r#"{"data":[{"id":"*","object":"model"}],"object":"list"}"#;
        assert!(matches!(
            model_in_list(body, "qwen3-vl-32b-instruct"),
            ModelPresence::Present
        ));
    }

    #[test]
    fn exact_model_id_matches() {
        let body = r#"{"data":[{"id":"qwen35-27b-fp8"},{"id":"other"}]}"#;
        assert!(matches!(
            model_in_list(body, "qwen35-27b-fp8"),
            ModelPresence::Present
        ));
    }

    #[test]
    fn missing_model_id_is_absent() {
        let body = r#"{"data":[{"id":"other-model"}]}"#;
        assert!(matches!(
            model_in_list(body, "qwen35-27b-fp8"),
            ModelPresence::Absent
        ));
    }

    #[test]
    fn unrecognized_body_is_unknown() {
        assert!(matches!(model_in_list("not json", "m"), ModelPresence::Unknown));
        assert!(matches!(
            model_in_list(r#"{"data":[]}"#, "m"),
            ModelPresence::Unknown
        ));
    }
}
