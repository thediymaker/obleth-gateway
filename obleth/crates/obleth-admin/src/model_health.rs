//! Model health checks for registered model routes.
//!
//! A health check is a tiny chat completion sent through obleth's own proxy
//! path with a temporary Redis-only key. This validates the same auth, route
//! resolution, scheduler, model rewrite, and upstream call path that clients use.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use obleth_config::{
    generate_api_key, ModelHealthDetail, ModelHealthSummary, ModelRoute, ResolvedKey,
};
use obleth_store::{
    ModelHealthAlertEvent, ModelHealthClaim, ModelHealthConfigUpdate, ModelHealthRecordOutcome,
};
use rand::Rng;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{AdminError, AdminState, Result};

const HEALTH_TENANT_NAME: &str = "__obleth_model_health";
pub const HEALTH_GROUP: &str = "model-health";
const CHECK_LIMIT: i64 = 50;
const WORKER_CLAIM_LIMIT: i64 = 4;
const WORKER_SLEEP_SECS: u64 = 30;

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
    pub internal_proxy_base_url: String,
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
    let status = if model.enabled {
        gateway_health_probe(state, &model).await
    } else {
        ProbeResult {
            status: "disabled".to_string(),
            latency_ms: None,
            http_status: None,
            message: Some("model route is disabled".to_string()),
            response_excerpt: None,
        }
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

async fn gateway_health_probe(state: &AdminState, model: &ModelRoute) -> ProbeResult {
    let generated = generate_api_key();
    let resolved = ResolvedKey {
        key_id: Uuid::new_v4(),
        tenant_id: health_tenant_id(),
        tenant_name: HEALTH_TENANT_NAME.to_string(),
        fairshare_group: HEALTH_GROUP.to_string(),
        group_weight: 1,
        weight: 1,
        tokens_per_minute: 1_000_000,
        max_in_flight: Some(1),
        disabled: false,
        internal: true,
    };

    let write_key = state
        .redis
        .put_resolved_key(&generated.hash, &resolved)
        .await;
    if let Err(error) = write_key {
        return ProbeResult::unhealthy(None, None, format!("temporary key write failed: {error}"));
    }
    let _ = state.redis.publish_invalidation(&generated.hash).await;

    let result = send_health_probe_request(state, model, &generated.secret).await;
    let _ = state.redis.delete_resolved_key(&generated.hash).await;
    let _ = state.redis.publish_invalidation(&generated.hash).await;
    result
}

async fn send_health_probe_request(
    state: &AdminState,
    model: &ModelRoute,
    secret: &str,
) -> ProbeResult {
    let base = state.health.internal_proxy_base_url.trim_end_matches('/');
    let url = format!("{base}/v1/chat/completions");
    let started = Instant::now();
    let response = state
        .health
        .http
        .post(url)
        .bearer_auth(secret)
        .timeout(Duration::from_secs(state.health.timeout_secs.max(1)))
        .json(&serde_json::json!({
            "model": model.model_name,
            "messages": [
                { "role": "system", "content": "obleth model health check. Reply with ok." },
                { "role": "user", "content": "ok" }
            ],
            "max_tokens": 4,
            "temperature": 0,
            "stream": false
        }))
        .send()
        .await;
    let latency_ms = started.elapsed().as_millis().try_into().ok();

    match response {
        Ok(res) => {
            let status = res.status();
            let http_status = Some(status.as_u16() as i64);
            let body = res.text().await.unwrap_or_default();
            if status.is_success() {
                ProbeResult {
                    status: "healthy".to_string(),
                    latency_ms,
                    http_status,
                    message: Some("gateway health probe completed".to_string()),
                    response_excerpt: None,
                }
            } else {
                ProbeResult::unhealthy(
                    latency_ms,
                    http_status,
                    format!("gateway health probe returned {status}"),
                )
                .with_excerpt(body)
            }
        }
        Err(error) => ProbeResult::unhealthy(
            latency_ms,
            None,
            format!("gateway health probe request failed: {error}"),
        ),
    }
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
    fn unhealthy(latency_ms: Option<i64>, http_status: Option<i64>, message: String) -> Self {
        Self {
            status: "unhealthy".to_string(),
            latency_ms,
            http_status,
            message: Some(normalize_excerpt(&message, 240)),
            response_excerpt: None,
        }
    }

    fn with_excerpt(mut self, body: String) -> Self {
        let excerpt = normalize_excerpt(&body, 500);
        if !excerpt.is_empty() {
            self.response_excerpt = Some(excerpt);
        }
        self
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
}
