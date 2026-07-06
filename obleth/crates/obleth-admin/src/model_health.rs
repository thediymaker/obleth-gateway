//! Model health checks for registered model routes.
//!
//! Health is determined by the cheapest trustworthy signal, in order:
//!
//! 1. **Passive** — a recent real 2xx in the ClickHouse usage ledger (window
//!    follows the model's check interval). Only an observed *success* settles
//!    the check here; recent errors are never trusted to stand in for a probe
//!    (they may be stale, and acting on them would suppress the very probe that
//!    would clear them), so they fall through to step 2.
//! 2. **Active minimal inference** — a real forward pass per modality: one-token
//!    chat completion, one-string embedding, one-character speech, or a
//!    generated 0.1 s silence WAV transcription. Probe tokens are accounted
//!    under the internal `health_probe` tenant so they never appear in client
//!    billing.
//! 3. **Catalog existence** — for types with no cheap inference probe (image),
//!    `GET /models` membership, cached per `api_base`. A wildcard (`"*"`)
//!    catalog can never confirm membership and yields `unknown`, never healthy.
//!
//! A probe rejection (HTTP 400/404/422) is disambiguated against the catalog:
//! a listed model rejected by its modality endpoint is a `model_type` config
//! error (`degraded`, with a pointed message), not an outage.
//!
//! Transient conditions (overloaded upstream, an unverifiable catalog, a
//! single network blip) are classified as `degraded`/`unknown` rather than
//! `unhealthy` so a model doesn't flap to "down" and fire false alerts.

use std::collections::HashMap;
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

/// True when a model update changed a field the health probe depends on —
/// the existing failure streak / alert state describe a configuration that no
/// longer exists and must be reset (see `Store::reset_model_health`). Compares
/// post-normalization values, so a no-op PUT never wipes a real streak.
pub(crate) fn probe_config_changed(before: &ModelRoute, after: &ModelRoute) -> bool {
    before.api_base != after.api_base
        || before.upstream_model != after.upstream_model
        || before.model_type != after.model_type
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
    /// Short-TTL cache of upstream `/models` catalogs keyed by `api_base`, so
    /// a sweep over many models sharing one upstream fetches its catalog once.
    pub catalogs: CatalogCache,
}

type CatalogEntries = HashMap<String, (Instant, Arc<Catalog>)>;

/// Cached upstream catalog lookups (see [`fetch_upstream_catalog`]).
/// Best-effort and in-process: a miss just costs one extra GET.
#[derive(Clone, Default)]
pub struct CatalogCache(Arc<std::sync::Mutex<CatalogEntries>>);

const CATALOG_CACHE_TTL: Duration = Duration::from_secs(60);

/// What `GET {api_base}/models` revealed about an upstream.
#[derive(Debug)]
pub enum Catalog {
    /// A wildcard pass-through (`"*"` entry): membership is unknowable — this
    /// must never be read as confirmation that a model exists (a wildcard
    /// upstream once reported every model behind it healthy for 24 h).
    Wildcard,
    /// The upstream enumerates real model ids.
    Ids(std::collections::HashSet<String>),
}

impl CatalogCache {
    fn get(&self, api_base: &str) -> Option<Arc<Catalog>> {
        let cache = self.0.lock().expect("catalog cache poisoned");
        let (fetched_at, catalog) = cache.get(api_base)?;
        (fetched_at.elapsed() < CATALOG_CACHE_TTL).then(|| catalog.clone())
    }

    fn put(&self, api_base: &str, catalog: Arc<Catalog>) {
        let mut cache = self.0.lock().expect("catalog cache poisoned");
        cache.retain(|_, (fetched_at, _)| fetched_at.elapsed() < CATALOG_CACHE_TTL);
        cache.insert(api_base.to_string(), (Instant::now(), catalog));
    }
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

#[derive(Debug, Deserialize, ToSchema)]
pub struct ValidateModelRequest {
    pub api_base: String,
    pub api_key: Option<String>,
    pub upstream_model: String,
    #[serde(default)]
    pub model_type: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ValidateModelResult {
    /// The upstream answered `GET /models` with 2xx.
    pub reachable: bool,
    /// The catalog is a wildcard pass-through (membership unverifiable).
    pub wildcard: bool,
    /// Whether `upstream_model` appears in the catalog; `null` when
    /// unknowable (unreachable or wildcard).
    pub listed: Option<bool>,
    /// Human-readable advisories. Empty = nothing suspicious.
    pub warnings: Vec<String>,
}

/// Advisory pre-flight for a model registration: does the upstream actually
/// serve this id? Never blocks a write — the dashboard surfaces `warnings`
/// after save. Always HTTP 200 (an unreachable upstream is a *finding*, not
/// an error). Bypasses the probe catalog cache so a just-registered upstream
/// model is seen immediately.
#[utoipa::path(
    post, path = "/api/v1/models/validate", tag = "models",
    request_body = ValidateModelRequest,
    responses((status = 200, body = ValidateModelResult))
)]
pub async fn validate_model(
    State(state): State<AdminState>,
    Json(body): Json<ValidateModelRequest>,
) -> Result<Json<ValidateModelResult>> {
    let mut warnings = Vec::new();
    if let Some(model_type) = body.model_type.as_deref() {
        // The silent chat-default on an unknown type is exactly how models end
        // up probed against the wrong endpoint — flag it at the door.
        if !obleth_config::is_valid_model_type(&model_type.trim().to_ascii_lowercase()) {
            warnings.push(format!(
                "model_type `{model_type}` is not recognized and will be stored as `chat`"
            ));
        }
    }
    if body.api_base.trim().is_empty() {
        warnings.push(
            "no static api_base to validate (provisioned-only models are verified per endpoint)"
                .to_string(),
        );
        return Ok(Json(ValidateModelResult {
            reachable: false,
            wildcard: false,
            listed: None,
            warnings,
        }));
    }
    state.ssrf.validate(&body.api_base)?;

    match fetch_catalog_direct(&state, &body.api_base, body.api_key.as_deref()).await {
        Ok(catalog) => match catalog.as_ref() {
            Catalog::Wildcard => {
                warnings.push(
                    "upstream catalog is a wildcard pass-through; cannot verify the model id"
                        .to_string(),
                );
                Ok(Json(ValidateModelResult {
                    reachable: true,
                    wildcard: true,
                    listed: None,
                    warnings,
                }))
            }
            Catalog::Ids(ids) => {
                let listed = ids.contains(&body.upstream_model);
                if !listed {
                    warnings.push(format!(
                        "`{}` is not listed by the upstream catalog — check upstream_model \
                         (and that the model is deployed)",
                        body.upstream_model
                    ));
                }
                Ok(Json(ValidateModelResult {
                    reachable: true,
                    wildcard: false,
                    listed: Some(listed),
                    warnings,
                }))
            }
        },
        Err(error) => {
            warnings.push(format!("upstream validation failed: {}", error.message));
            Ok(Json(ValidateModelResult {
                reachable: false,
                wildcard: false,
                listed: None,
                warnings,
            }))
        }
    }
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
    // Fetched up front: the passive window follows the model's own check
    // interval, and the same summary row supplies `next_check_at` below.
    let summary = state.store.get_model_health_summary(model.id).await?;

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
    } else if let Some(passive) = passive_signal(
        state,
        &model,
        passive_window_secs(summary.check_interval_secs),
    )
    .await
    {
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

/// Passive-signal window for a model: at least [`PASSIVE_WINDOW_SECS`], widened
/// to the model's own check interval so a model with steady traffic is always
/// settled by the free ledger lookup instead of an active probe. (The default
/// interval is 900s; a fixed 300s window would miss two-thirds of it.)
fn passive_window_secs(check_interval_secs: i64) -> i64 {
    check_interval_secs.max(PASSIVE_WINDOW_SECS)
}

async fn passive_signal(
    state: &AdminState,
    model: &ModelRoute,
    window_secs: i64,
) -> Option<ProbeResult> {
    let row = recent_traffic(state, &model.model_name, window_secs).await?;
    classify_passive_traffic(&row, window_secs)
}

/// Turn a window of recent real traffic into a health verdict, or `None` to
/// defer to an active probe.
///
/// **Only a success short-circuits.** A real 2xx in the window proves the model
/// is currently serving — as trustworthy as an active forward pass, and free.
///
/// Server errors deliberately do *not* stand in as the verdict. They may be
/// stale (piled up before a recovery), and — worse — letting them decide would
/// skip the very active probe whose success would clear them, deadlocking a
/// recovered model at "unhealthy" until the window ages out. (Real 4xx are
/// caller mistakes, not model health, so they're inconclusive too.) So for
/// anything short of an observed success we return `None` and let the active
/// probe be the ground truth: if the model really is down, that probe fails and
/// reports it with a live HTTP status and latency.
fn classify_passive_traffic(row: &RecentTraffic, window_secs: i64) -> Option<ProbeResult> {
    (row.successes > 0).then(|| {
        ProbeResult::healthy(
            None,
            None,
            format!(
                "passive: {} successful request(s) in the last {window_secs}s",
                row.successes
            ),
        )
    })
}

async fn recent_traffic(
    state: &AdminState,
    model_name: &str,
    window_secs: i64,
) -> Option<RecentTraffic> {
    let since = Utc::now().timestamp_millis() - window_secs.max(1) * 1000;
    let sql = "select countIf(status_code >= 200 and status_code < 300) as successes \
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

/// A failed catalog fetch: HTTP status when the upstream answered, `None` on a
/// transport error — the same shape `classify_probe` consumes.
struct CatalogError {
    http: Option<u16>,
    message: String,
}

/// Fetch (or reuse a cached copy of) the upstream's `/models` catalog.
/// Successful fetches are cached per `api_base` for [`CATALOG_CACHE_TTL`] so a
/// sweep over many models sharing one upstream lists it once; errors are not
/// cached.
async fn fetch_upstream_catalog(
    state: &AdminState,
    api_base: &str,
    api_key: Option<&str>,
) -> std::result::Result<Arc<Catalog>, CatalogError> {
    if let Some(catalog) = state.health.catalogs.get(api_base) {
        return Ok(catalog);
    }
    let catalog = fetch_catalog_direct(state, api_base, api_key).await?;
    state.health.catalogs.put(api_base, catalog.clone());
    Ok(catalog)
}

/// Uncached catalog fetch. The validation endpoint uses this directly so an
/// operator who just registered a model upstream sees current truth, not a
/// ≤60 s-old snapshot.
async fn fetch_catalog_direct(
    state: &AdminState,
    api_base: &str,
    api_key: Option<&str>,
) -> std::result::Result<Arc<Catalog>, CatalogError> {
    let url = format!("{}/models", api_base.trim_end_matches('/'));
    let timeout = Duration::from_secs(state.health.timeout_secs.max(1));
    let mut request = state.health.http.get(&url).timeout(timeout);
    if let Some(key) = api_key {
        request = request.bearer_auth(key);
    }
    let response = request.send().await.map_err(|error| CatalogError {
        http: None,
        message: format!("catalog is unreachable: {error}"),
    })?;
    let code = response.status().as_u16();
    if !(200..300).contains(&code) {
        return Err(CatalogError {
            http: Some(code),
            message: format!("catalog request failed (HTTP {code})"),
        });
    }
    let body = response.text().await.unwrap_or_default();
    let json: serde_json::Value = serde_json::from_str(&body).map_err(|_| CatalogError {
        http: Some(code),
        message: "catalog response is not valid JSON".to_string(),
    })?;
    let ids: std::collections::HashSet<String> = json
        .get("data")
        .and_then(|d| d.as_array())
        .map(|models| {
            models
                .iter()
                .filter_map(|m| m.get("id").and_then(|id| id.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Ok(Arc::new(if ids.contains("*") {
        Catalog::Wildcard
    } else {
        Catalog::Ids(ids)
    }))
}

/// Active check for model types with no minimal inference probe (`image`,
/// unrecognized): does the upstream's catalog list the model at all? Cheaper
/// and weaker than an inference probe — the healthy message says so.
async fn existence_probe(
    state: &AdminState,
    model: &ModelRoute,
    api_base: &str,
    api_key: Option<&str>,
) -> ProbeResult {
    let started = Instant::now();
    let catalog = fetch_upstream_catalog(state, api_base, api_key).await;
    let latency_ms: Option<i64> = started.elapsed().as_millis().try_into().ok();
    match catalog {
        Ok(catalog) => classify_existence(&catalog, &model.upstream_model, latency_ms),
        Err(error) => ProbeResult {
            status: classify_probe(error.http).to_string(),
            latency_ms,
            http_status: error.http.map(i64::from),
            message: Some(normalize_excerpt(&error.message, 240)),
            response_excerpt: None,
        },
    }
}

/// Map a fetched catalog to an existence-check result. A wildcard catalog can
/// never confirm membership — `unknown`, NEVER healthy (regression guard: a
/// wildcard upstream once reported every model behind it healthy for 24 h,
/// see the 2026-06-28 spec).
fn classify_existence(
    catalog: &Catalog,
    upstream_model: &str,
    latency_ms: Option<i64>,
) -> ProbeResult {
    match catalog {
        Catalog::Wildcard => ProbeResult::unknown(format!(
            "upstream catalog is a wildcard pass-through; cannot verify `{upstream_model}` — status unverified"
        )),
        Catalog::Ids(ids) if ids.contains(upstream_model) => ProbeResult::healthy(
            latency_ms,
            None,
            format!("upstream lists `{upstream_model}` (existence check; inference not verified)"),
        ),
        Catalog::Ids(_) => ProbeResult::unhealthy(
            latency_ms,
            None,
            format!("`{upstream_model}` is not listed by the upstream catalog"),
        ),
    }
}

/// Refine an inference-probe rejection (HTTP 400/404/422) using the upstream
/// catalog: those codes mean either "model is gone" (a real outage) or "wrong
/// endpoint for this modality" (a `model_type` config error). May only refine,
/// never mask — on a wildcard catalog or a failed fetch the original result is
/// returned verbatim.
async fn disambiguate_rejection(
    state: &AdminState,
    model: &ModelRoute,
    api_base: &str,
    api_key: Option<&str>,
    code: u16,
    probe_url: &str,
    original: ProbeResult,
) -> ProbeResult {
    let Ok(catalog) = fetch_upstream_catalog(state, api_base, api_key).await else {
        return original;
    };
    refine_rejection(&catalog, model, code, probe_url, original)
}

/// Pure half of [`disambiguate_rejection`]: catalog in hand, decide whether
/// the rejection was a config error (listed → `degraded` with a pointer at
/// `model_type`) or a genuine absence (→ `unhealthy`, alertable). A wildcard
/// catalog adds no information — the original result passes through verbatim.
fn refine_rejection(
    catalog: &Catalog,
    model: &ModelRoute,
    code: u16,
    probe_url: &str,
    original: ProbeResult,
) -> ProbeResult {
    let endpoint = probe_url
        .rsplit_once("/v1")
        .map(|(_, path)| path)
        .unwrap_or(probe_url);
    match catalog {
        Catalog::Wildcard => original,
        Catalog::Ids(ids) if ids.contains(&model.upstream_model) => ProbeResult::degraded(
            original.latency_ms,
            original.http_status,
            format!(
                "upstream lists `{}` but `{endpoint}` rejected it (HTTP {code}) — model_type `{}` may be misconfigured",
                model.upstream_model, model.model_type
            ),
        ),
        Catalog::Ids(_) => ProbeResult::unhealthy(
            original.latency_ms,
            original.http_status,
            format!(
                "model `{}` not found upstream (HTTP {code}; not in /models)",
                model.upstream_model
            ),
        ),
    }
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
        // No minimal inference request exists for this type (image /
        // unrecognized): fall back to the catalog existence check.
        return existence_probe(state, model, api_base, api_key).await;
    };

    let timeout = Duration::from_secs(state.health.timeout_secs.max(1));
    let client = &state.health.http;
    let mut attempt = 0u32;

    loop {
        attempt += 1;
        let mut request = req.build(client, timeout);
        if let Some(key) = api_key {
            request = request.bearer_auth(key);
        }
        let started = Instant::now();
        let response = request.send().await;
        let latency_ms: Option<i64> = started.elapsed().as_millis().try_into().ok();

        match response {
            Ok(res) => {
                let st = res.status();
                let code = st.as_u16();
                let retryable = code == 408 || code == 429 || st.is_server_error();
                if retryable && attempt < LIVENESS_MAX_ATTEMPTS {
                    tokio::time::sleep(Duration::from_millis(100 * 2u64.pow(attempt - 1))).await;
                    continue;
                }
                let body = res.text().await.unwrap_or_default();
                let (input_tokens, output_tokens) = probe_token_usage(&body);
                emit_probe_usage(
                    state,
                    model,
                    Some(code),
                    latency_ms,
                    input_tokens,
                    output_tokens,
                );
                let status = classify_probe(Some(code));
                let message = match status {
                    "healthy" => format!("real probe succeeded (HTTP {code})"),
                    "unhealthy" => format!("real probe rejected by upstream (HTTP {code})"),
                    _ => format!("real probe inconclusive (HTTP {code})"),
                };
                let result = ProbeResult {
                    status: status.to_string(),
                    latency_ms,
                    http_status: Some(code as i64),
                    message: Some(normalize_excerpt(&message, 240)),
                    response_excerpt: None,
                };
                // 400/404/422 conflate "model gone" with "wrong endpoint for
                // this modality" — let the catalog tell them apart (401/403
                // skip: bad credentials fail /models identically).
                if status == "unhealthy" && matches!(code, 400 | 404 | 422) {
                    return disambiguate_rejection(
                        state,
                        model,
                        api_base,
                        api_key,
                        code,
                        req.url(),
                        result,
                    )
                    .await;
                }
                return result;
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
        sink.record(build_probe_usage(
            model,
            http_status,
            total_ms,
            input_tokens,
            output_tokens,
        ));
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
    // If every live endpoint returned `unknown` (e.g. a costly-mode model whose
    // type is not auto-probed), the pool is unverified rather than confirmed
    // unreachable — return `unknown` so it is non-alerting.
    let all_unknown = live.iter().all(|r| r.status == "unknown");
    if all_unknown {
        return ProbeResult::unknown(format!("{total} endpoint(s) unverified"));
    }
    ProbeResult::unhealthy(None, None, format!("all {total} endpoint(s) unreachable"))
}

/// The minimal real inference request for one probe attempt. Multipart is a
/// separate variant because `reqwest::multipart::Form` cannot be reused across
/// retry attempts — the form is rebuilt from these parts on every attempt.
enum ProbeRequest {
    Json {
        url: String,
        body: serde_json::Value,
    },
    Multipart {
        url: String,
        model: String,
        wav: Vec<u8>,
    },
}

impl ProbeRequest {
    fn url(&self) -> &str {
        match self {
            ProbeRequest::Json { url, .. } | ProbeRequest::Multipart { url, .. } => url,
        }
    }

    /// Assemble the reqwest builder for one attempt.
    fn build(&self, client: &reqwest::Client, timeout: Duration) -> reqwest::RequestBuilder {
        match self {
            ProbeRequest::Json { url, body } => client.post(url).timeout(timeout).json(body),
            ProbeRequest::Multipart { url, model, wav } => {
                let file = reqwest::multipart::Part::bytes(wav.clone())
                    .file_name("probe.wav")
                    .mime_str("audio/wav")
                    .expect("static mime type is valid");
                let form = reqwest::multipart::Form::new()
                    .text("model", model.clone())
                    .part("file", file);
                client.post(url).timeout(timeout).multipart(form)
            }
        }
    }
}

/// 0.1 s of 8 kHz 16-bit mono silence as a complete RIFF/WAV file (1644
/// bytes) — the cheapest input a transcription server accepts as a real
/// forward pass. Generated, not a repo asset, so it is fully deterministic.
fn probe_silence_wav() -> Vec<u8> {
    const SAMPLE_RATE: u32 = 8_000;
    const SAMPLES: u32 = 800; // 0.1 s
    const DATA_LEN: u32 = SAMPLES * 2; // 16-bit mono
    let mut wav = Vec::with_capacity(44 + DATA_LEN as usize);
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + DATA_LEN).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&1u16.to_le_bytes()); // mono
    wav.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    wav.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes()); // byte rate
    wav.extend_from_slice(&2u16.to_le_bytes()); // block align
    wav.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&DATA_LEN.to_le_bytes());
    wav.resize(44 + DATA_LEN as usize, 0);
    wav
}

/// Build the minimal real inference request used to verify a model actually
/// serves. `api_base` already includes the `/v1` suffix. Returns `None` for
/// `image` (a "minimal" generation is still costly) and unrecognized types —
/// those fall back to the catalog existence check.
fn build_probe_request(
    api_base: &str,
    model_type: &str,
    upstream_model: &str,
) -> Option<ProbeRequest> {
    let base = api_base.trim_end_matches('/');
    match model_type {
        "chat" => Some(ProbeRequest::Json {
            url: format!("{base}/chat/completions"),
            body: serde_json::json!({
                "model": upstream_model,
                "messages": [{ "role": "user", "content": "ping" }],
                "max_tokens": 1,
                "stream": false,
            }),
        }),
        "embedding" => Some(ProbeRequest::Json {
            url: format!("{base}/embeddings"),
            body: serde_json::json!({ "model": upstream_model, "input": "ping" }),
        }),
        // One character of speech: a real forward pass at negligible cost.
        // `voice` is required by the OpenAI schema; a server that rejects the
        // fixed name 400s into the catalog disambiguation path, which points
        // at configuration rather than declaring an outage.
        "audio_speech" => Some(ProbeRequest::Json {
            url: format!("{base}/audio/speech"),
            body: serde_json::json!({
                "model": upstream_model,
                "input": ".",
                "voice": "alloy",
            }),
        }),
        "audio_transcription" => Some(ProbeRequest::Multipart {
            url: format!("{base}/audio/transcriptions"),
            model: upstream_model.to_string(),
            wav: probe_silence_wav(),
        }),
        _ => None,
    }
}

#[derive(Debug, Clone, Row, Deserialize)]
struct RecentTraffic {
    /// Count of 2xx responses in the window — the only signal that short-circuits
    /// an active probe (see [`classify_passive_traffic`]).
    successes: u64,
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
        admission: HEALTH_PROBE_REQUEST_TYPE.to_string(),
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
        energy_wh: 0.0,
        energy_cost_usd: 0.0,
        co2_g: 0.0,
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

    fn ids(models: &[&str]) -> Catalog {
        Catalog::Ids(models.iter().map(|m| m.to_string()).collect())
    }

    #[test]
    fn passive_success_short_circuits_to_healthy() {
        let r = classify_passive_traffic(&RecentTraffic { successes: 3 }, 900)
            .expect("a success must yield a verdict");
        assert_eq!(r.status, "healthy");
        assert!(r.message.as_deref().unwrap_or("").contains("3 successful"));
    }

    #[test]
    fn passive_errors_only_defer_to_active_probe() {
        // Regression pin (2026-07-05): a recovered model was pinned "unhealthy"
        // because a window of stale server errors (no successes) stood in as the
        // verdict and skipped the active probe that would have cleared it. Errors
        // with zero successes must now defer (None), not decide.
        assert!(classify_passive_traffic(&RecentTraffic { successes: 0 }, 900).is_none());
    }

    #[test]
    fn passive_no_traffic_defers_to_active_probe() {
        assert!(classify_passive_traffic(&RecentTraffic { successes: 0 }, 300).is_none());
    }

    #[test]
    fn existence_wildcard_is_never_healthy() {
        // Regression pin (2026-06-28 spec): a wildcard catalog must yield
        // `unknown`, not a false healthy, for every model behind it.
        let r = classify_existence(&Catalog::Wildcard, "kimi-k2", None);
        assert_eq!(r.status, "unknown");
        assert!(r.message.as_deref().unwrap_or("").contains("wildcard"));
    }

    #[test]
    fn existence_listed_is_healthy_with_caveat() {
        let r = classify_existence(&ids(&["sdxl", "flux-dev"]), "sdxl", Some(12));
        assert_eq!(r.status, "healthy");
        assert!(r
            .message
            .as_deref()
            .unwrap_or("")
            .contains("inference not verified"));
    }

    #[test]
    fn existence_absent_is_unhealthy() {
        let r = classify_existence(&ids(&["sdxl"]), "flux-dev", None);
        assert_eq!(r.status, "unhealthy");
    }

    #[test]
    fn rejection_listed_model_becomes_degraded_misconfig_hint() {
        let mut model = sample_model();
        model.upstream_model = "tts-1".into();
        let original = ProbeResult::unhealthy(Some(9), Some(404), "rejected".into());
        let r = refine_rejection(
            &ids(&["tts-1"]),
            &model,
            404,
            "https://up/v1/chat/completions",
            original,
        );
        assert_eq!(r.status, "degraded");
        let msg = r.message.as_deref().unwrap_or("");
        assert!(msg.contains("model_type"), "misconfig hint missing: {msg}");
        assert!(msg.contains("/chat/completions"));
        // Latency/status of the original probe are preserved.
        assert_eq!(r.latency_ms, Some(9));
        assert_eq!(r.http_status, Some(404));
    }

    #[test]
    fn rejection_absent_model_stays_unhealthy_with_catalog_evidence() {
        let model = sample_model();
        let original = ProbeResult::unhealthy(None, Some(404), "rejected".into());
        let r = refine_rejection(
            &ids(&["something-else"]),
            &model,
            404,
            "https://up/v1/chat/completions",
            original,
        );
        assert_eq!(r.status, "unhealthy");
        assert!(r
            .message
            .as_deref()
            .unwrap_or("")
            .contains("not in /models"));
    }

    #[test]
    fn rejection_wildcard_catalog_passes_original_through() {
        let model = sample_model();
        let original = ProbeResult::unhealthy(Some(3), Some(404), "original message".into());
        let r = refine_rejection(
            &Catalog::Wildcard,
            &model,
            404,
            "https://up/v1/chat/completions",
            original,
        );
        assert_eq!(r.status, "unhealthy");
        assert_eq!(r.message.as_deref(), Some("original message"));
    }

    #[test]
    fn catalog_cache_round_trip_and_wildcard_detection() {
        let cache = CatalogCache::default();
        assert!(cache.get("https://up/v1").is_none());
        cache.put("https://up/v1", Arc::new(ids(&["a", "b"])));
        let hit = cache.get("https://up/v1").expect("cache hit");
        assert!(matches!(hit.as_ref(), Catalog::Ids(ids) if ids.len() == 2));
        assert!(cache.get("https://other/v1").is_none());
    }

    #[test]
    fn probe_config_change_detection() {
        let before = sample_model();
        // No-op update must not reset a real failure streak.
        assert!(!probe_config_changed(&before, &before.clone()));
        for mutate in [
            |m: &mut ModelRoute| m.api_base = "https://other/v1".into(),
            |m: &mut ModelRoute| m.upstream_model = "renamed".into(),
            |m: &mut ModelRoute| m.model_type = "embedding".into(),
        ] {
            let mut after = before.clone();
            mutate(&mut after);
            assert!(probe_config_changed(&before, &after));
        }
        // Non-probe fields don't reset.
        let mut after = before.clone();
        after.description = "new description".into();
        after.input_cost_per_token = 1.0;
        assert!(!probe_config_changed(&before, &after));
    }

    #[test]
    fn passive_window_follows_check_interval() {
        // Default interval (900s) widens the window past the 300s floor…
        assert_eq!(passive_window_secs(900), 900);
        // …while a short interval never narrows it below the floor.
        assert_eq!(passive_window_secs(60), 300);
        assert_eq!(passive_window_secs(300), 300);
    }

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
    fn aggregate_all_unknown_endpoints_is_unknown() {
        // All live endpoints returned `unknown` (e.g. costly-mode model type not
        // auto-probed) — must aggregate to `unknown`, NOT `unhealthy`, so no
        // false alert fires.
        let results = vec![
            ProbeResult::unknown("model type `image` is not auto-probed; status unverified".into()),
            ProbeResult::unknown("model type `image` is not auto-probed; status unverified".into()),
        ];
        let agg = aggregate_endpoint_health(&results, 1);
        assert_eq!(agg.status, "unknown");
        assert!(
            agg.message.as_deref().unwrap_or("").contains("unverified"),
            "message should mention unverified: {:?}",
            agg.message
        );
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

    fn expect_json(req: ProbeRequest) -> (String, serde_json::Value) {
        match req {
            ProbeRequest::Json { url, body } => (url, body),
            ProbeRequest::Multipart { .. } => panic!("expected a JSON probe request"),
        }
    }

    #[test]
    fn probe_request_chat_is_one_token_completion() {
        let r = build_probe_request("https://up/v1", "chat", "kimi-k2").expect("chat probe");
        let (url, body) = expect_json(r);
        assert_eq!(url, "https://up/v1/chat/completions");
        assert_eq!(body["model"], "kimi-k2");
        assert_eq!(body["max_tokens"], 1);
        assert_eq!(body["stream"], false);
        assert!(body["messages"].is_array());
    }

    #[test]
    fn probe_request_embedding_uses_embeddings_endpoint() {
        let r =
            build_probe_request("https://up/v1/", "embedding", "qwen4-embedding").expect("embed");
        let (url, body) = expect_json(r);
        assert_eq!(url, "https://up/v1/embeddings");
        assert_eq!(body["model"], "qwen4-embedding");
        assert_eq!(body["input"], "ping");
    }

    #[test]
    fn probe_request_speech_is_one_character() {
        let r = build_probe_request("https://up/v1", "audio_speech", "tts-1").expect("tts probe");
        let (url, body) = expect_json(r);
        assert_eq!(url, "https://up/v1/audio/speech");
        assert_eq!(body["model"], "tts-1");
        assert_eq!(body["input"], ".");
        assert_eq!(body["voice"], "alloy");
    }

    #[test]
    fn probe_request_transcription_is_multipart_silence_wav() {
        let r = build_probe_request("https://up/v1", "audio_transcription", "whisper-1")
            .expect("stt probe");
        let ProbeRequest::Multipart { url, model, wav } = r else {
            panic!("expected a multipart probe request");
        };
        assert_eq!(url, "https://up/v1/audio/transcriptions");
        assert_eq!(model, "whisper-1");
        // Complete RIFF/WAV container: header magic + declared sizes match.
        assert_eq!(wav.len(), 1644);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[36..40], b"data");
        assert_eq!(u32::from_le_bytes(wav[40..44].try_into().unwrap()), 1600);
        // Silence: every PCM sample is zero.
        assert!(wav[44..].iter().all(|b| *b == 0));
    }

    #[test]
    fn probe_request_costly_and_unknown_modes_are_none() {
        assert!(build_probe_request("https://up/v1", "image", "m").is_none());
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
        }))
        .expect("sample model")
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
