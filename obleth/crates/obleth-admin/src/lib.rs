//! Management API: the versioned, authenticated control surface.
//!
//! Mounted on a separate admin port from the data plane. Every config write
//! follows a single path — **Postgres (durable) -> Redis (cache) -> pub/sub
//! invalidate** — and is recorded in the audit log. Usage/cost reads hit
//! ClickHouse. The Next.js dashboard and any CLI/Terraform consume these exact
//! endpoints.

pub mod alerts;
pub mod autotune;
mod error;
pub mod model_health;
mod openapi;
pub mod ssrf;
mod usage;
pub mod usage_retention;

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header::AUTHORIZATION, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use obleth_config::{
    hash_api_key, ApiKey, FairshareGroup, McpServer, ModelEndpoint, ModelRoute, ResolvedKey,
    ResolvedMcpServer, ResolvedModel, Tenant,
};
use obleth_config::{AlertSettings, AutoRouterSettings, BoonSettings, EmailSettings, VisionBoonSettings};
use obleth_fairshare::{FairShare, StaticCapacity, Stats};
use obleth_redis::RedisStore;
use obleth_store::{AuditEntry, Store};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use utoipa::ToSchema;
use uuid::Uuid;

pub use alerts::AlertDispatcher;
pub use error::AdminError;
pub use model_health::{AlertSink, ModelHealthRuntime};
pub use openapi::ApiDoc;

type Result<T> = std::result::Result<T, AdminError>;

/// Shared state for the Management API. All fields are cheap-clone handles.
#[derive(Clone)]
pub struct AdminState {
    pub store: Store,
    pub redis: RedisStore,
    pub capacity: Arc<StaticCapacity>,
    pub fairshare: FairShare,
    pub fairshare_stats: Arc<Stats>,
    pub clickhouse: clickhouse::Client,
    pub admin_token: String,
    pub health: ModelHealthRuntime,
    /// Default raw-usage retention in days, used when no runtime setting is
    /// persisted. Sourced from `OBLETH_USAGE_RETENTION_DAYS`.
    pub usage_retention_default_days: i64,
    /// SSRF allowlist policy applied to admin-supplied upstream URLs.
    pub ssrf: ssrf::SsrfPolicy,
    /// Runtime-reloadable alert dispatcher shared with the data plane.
    pub alerts: AlertDispatcher,
}

/// Build the `/api/v1` router. `/health` and the OpenAPI doc are public; every
/// other route requires a bearer admin token.
pub fn router(state: AdminState) -> Router {
    let public = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/openapi.json", get(openapi_json));

    let protected = Router::new()
        .route("/api/v1/tenants", post(create_tenant).get(list_tenants))
        .route(
            "/api/v1/tenants/:id",
            get(get_tenant).put(update_tenant).delete(delete_tenant),
        )
        .route("/api/v1/tenants/:id/status", patch(patch_tenant_status))
        .route("/api/v1/tenants/:id/schedule", patch(patch_tenant_schedule))
        .route("/api/v1/tenants/:id/budget", patch(patch_tenant_budget))
        .route(
            "/api/v1/tenants/:id/allowlist",
            patch(patch_tenant_allowlist),
        )
        .route("/api/v1/tenants/:id/weight", patch(patch_weight))
        .route("/api/v1/tenants/:id/quota", put(put_quota))
        .route("/api/v1/tenants/:id/keys", post(create_key))
        .route("/api/v1/keys", get(list_keys))
        .route("/api/v1/keys/:id", delete(delete_key))
        .route("/api/v1/keys/:id/disabled", put(set_key_disabled))
        .route("/api/v1/keys/:id/usage", get(get_key_usage))
        .route("/api/v1/usage", get(get_usage))
        .route("/api/v1/usage/keys", get(get_usage_keys))
        .route("/api/v1/usage/keys/summary", get(get_usage_keys_summary))
        .route("/api/v1/usage/models", get(get_usage_models))
        .route("/api/v1/usage/series", get(get_usage_series))
        .route(
            "/api/v1/usage/series/tenants",
            get(get_usage_series_tenants),
        )
        .route("/api/v1/usage/cache", get(get_cache_stats))
        .route("/api/v1/usage/logs", get(get_usage_logs))
        .route("/api/v1/usage/daily", get(get_usage_daily))
        .route("/api/v1/usage/compact", post(compact_usage))
        .route("/api/v1/costs", get(get_costs))
        .route("/api/v1/stats", get(get_stats))
        .route("/api/v1/fairshare/live", get(get_fairshare_live))
        .route(
            "/api/v1/fairshare/groups",
            post(create_fairshare_group).get(list_fairshare_groups),
        )
        .route(
            "/api/v1/fairshare/groups/:name/weight",
            patch(patch_fairshare_group_weight),
        )
        .route("/api/v1/tenants/:id/group", patch(patch_tenant_group))
        .route("/api/v1/models", post(create_model).get(list_models))
        .route(
            "/api/v1/models/health",
            get(model_health::list_health).post(model_health::check_all),
        )
        .route(
            "/api/v1/models/:id",
            get(get_model).put(update_model).delete(delete_model),
        )
        .route("/api/v1/models/:id/health", get(model_health::get_health))
        .route(
            "/api/v1/models/:id/health/check",
            post(model_health::check_one),
        )
        .route(
            "/api/v1/models/:id/health/config",
            put(model_health::update_config),
        )
        .route("/api/v1/models/:id/weight", put(set_model_weight))
        .route("/api/v1/models/:id/capacity", put(set_model_capacity))
        .route(
            "/api/v1/models/:id/capacity-mode",
            put(set_model_capacity_mode),
        )
        .route("/api/v1/models/:id/autotune", post(autotune_model))
        .route(
            "/api/v1/models/:id/autotune/apply",
            post(apply_autotune_capacity),
        )
        .route("/api/v1/models/:id/cache", put(set_model_cache))
        .route("/api/v1/models/:id/reliability", put(set_model_reliability))
        .route(
            "/api/v1/models/:id/endpoints",
            get(list_model_endpoints).post(create_model_endpoint),
        )
        .route(
            "/api/v1/models/:id/endpoints/:endpoint_id",
            put(update_model_endpoint).delete(delete_model_endpoint),
        )
        .route(
            "/api/v1/mcp-servers",
            post(create_mcp_server).get(list_mcp_servers),
        )
        .route(
            "/api/v1/mcp-servers/:id",
            get(get_mcp_server)
                .put(update_mcp_server)
                .delete(delete_mcp_server),
        )
        .route("/api/v1/audit", get(get_audit))
        .route("/api/v1/capacity", get(get_capacity).put(set_capacity))
        .route(
            "/api/v1/settings/alerts",
            get(get_alert_settings).put(put_alert_settings),
        )
        .route("/api/v1/settings/alerts/test", post(test_alert_settings))
        .route(
            "/api/v1/settings/auto-router",
            get(get_auto_router_settings).put(put_auto_router_settings),
        )
        .route(
            "/api/v1/settings/boons",
            get(get_boon_settings).put(put_boon_settings),
        )
        .route(
            "/api/v1/settings/usage-retention",
            get(get_usage_retention).put(put_usage_retention),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            require_admin,
        ));

    public.merge(protected).with_state(state)
}

async fn require_admin(
    State(state): State<AdminState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response> {
    let expected = format!("Bearer {}", state.admin_token);
    let presented = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // Constant-time comparison to avoid leaking the token via response timing.
    let ok: bool = presented.as_bytes().ct_eq(expected.as_bytes()).unwrap_u8() == 1;
    if !ok {
        return Err(AdminError::Unauthorized);
    }
    Ok(next.run(req).await)
}

// ---- DTOs ----------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTenant {
    pub name: String,
    pub weight: Option<i64>,
    pub tokens_per_minute: Option<i64>,
    pub max_in_flight: Option<i64>,
    pub fairshare_group: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTenant {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default)]
    pub contact_email: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantStatus {
    pub status: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantSchedule {
    /// IANA timezone the windows/cutoffs are evaluated in (e.g. `America/Phoenix`).
    #[serde(default = "obleth_config::default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub active_from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub active_until: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub weekly_windows: Option<Vec<obleth_config::WeeklyWindow>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantBudget {
    /// Cumulative token ceiling for the current term. `null` clears the cap.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Cumulative USD-cost ceiling for the current term. `null` clears the cap.
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    /// Reset period: `lifetime`, `monthly`, or `term`. `null` = lifetime.
    #[serde(default)]
    pub budget_period: Option<String>,
    /// When the current term began (used by `term`/`lifetime` reset semantics).
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantAllowlist {
    /// Permitted model names. An empty list clears the allowlist (all permitted).
    #[serde(default)]
    pub allowed_models: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateFairshareGroup {
    pub name: String,
    pub weight: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateGroupWeight {
    pub weight: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTenantGroup {
    pub fairshare_group: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateWeight {
    pub weight: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateQuota {
    pub tokens_per_minute: i64,
    pub max_in_flight: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateKey {
    pub name: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedKey {
    pub key: ApiKey,
    /// The raw secret, shown exactly once. Store it now; it cannot be retrieved.
    pub secret: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetDisabled {
    pub disabled: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetCapacity {
    pub max_in_flight: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CapacityView {
    pub max_in_flight: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LiveStats {
    pub in_flight: usize,
    pub queued: i64,
    pub max_in_flight: usize,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct GroupFairshareView {
    pub name: String,
    pub weight: i64,
    pub in_flight: usize,
    pub queued: usize,
    pub slot_cap: usize,
    pub served_tokens: f64,
    pub share_score: f64,
    pub weight_share: f64,
    pub expected_slots: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TenantFairshareView {
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub name: String,
    pub fairshare_group: String,
    pub weight: i64,
    pub in_flight: usize,
    pub queued: usize,
    pub served_tokens: f64,
    pub share_score: f64,
    pub weight_share: f64,
    /// Steady-state slot share under sustained contention.
    pub expected_slots: f64,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FairshareLiveView {
    pub algorithm: String,
    pub max_in_flight: usize,
    pub global_in_flight: usize,
    pub global_queued: i64,
    pub groups: Vec<GroupFairshareView>,
    pub tenants: Vec<TenantFairshareView>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateModel {
    pub model_name: String,
    pub description: Option<String>,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub model_type: Option<String>,
    pub input_cost_per_token: Option<f64>,
    pub output_cost_per_token: Option<f64>,
    #[serde(default)]
    pub cost_per_image: Option<f64>,
    #[serde(default)]
    pub cost_per_audio_second: Option<f64>,
    #[serde(default)]
    pub cost_per_character: Option<f64>,
    pub context_window: Option<i64>,
    pub admission_weight: Option<i64>,
    pub max_in_flight: Option<i64>,
    pub supports_function_calling: Option<bool>,
    pub supports_system_messages: Option<bool>,
    pub supports_response_schema: Option<bool>,
    pub supports_tool_choice: Option<bool>,
    #[serde(default)]
    pub supports_vision: Option<bool>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub boons: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateModel {
    pub description: Option<String>,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    #[serde(default)]
    pub model_type: Option<String>,
    pub input_cost_per_token: Option<f64>,
    pub output_cost_per_token: Option<f64>,
    #[serde(default)]
    pub cost_per_image: Option<f64>,
    #[serde(default)]
    pub cost_per_audio_second: Option<f64>,
    #[serde(default)]
    pub cost_per_character: Option<f64>,
    pub context_window: Option<i64>,
    pub admission_weight: Option<i64>,
    pub max_in_flight: Option<i64>,
    pub supports_function_calling: Option<bool>,
    pub supports_system_messages: Option<bool>,
    pub supports_response_schema: Option<bool>,
    pub supports_tool_choice: Option<bool>,
    #[serde(default)]
    pub supports_vision: Option<bool>,
    pub enabled: Option<bool>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub boons: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelCache {
    pub cache_enabled: bool,
    pub cache_ttl_secs: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelReliability {
    /// Per-request upstream timeout in seconds. `null` defers to the global
    /// default.
    #[serde(default)]
    pub request_timeout_secs: Option<i64>,
    #[serde(default)]
    pub max_retries: i64,
    #[serde(default = "default_retry_backoff_ms_dto")]
    pub retry_backoff_ms: i64,
    #[serde(default = "default_selection_mode_dto")]
    pub endpoint_selection_mode: String,
}

fn default_retry_backoff_ms_dto() -> i64 {
    obleth_config::DEFAULT_RETRY_BACKOFF_MS
}

fn default_selection_mode_dto() -> String {
    obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string()
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateModelEndpoint {
    pub name: String,
    pub api_base: String,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_endpoint_priority")]
    pub priority: i64,
    #[serde(default = "default_endpoint_weight")]
    pub weight: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateModelEndpoint {
    pub name: String,
    pub api_base: String,
    /// Omit to keep the stored key; empty string clears it.
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default = "default_endpoint_priority")]
    pub priority: i64,
    #[serde(default = "default_endpoint_weight")]
    pub weight: i64,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_endpoint_priority() -> i64 {
    100
}

fn default_endpoint_weight() -> i64 {
    100
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelCapacity {
    pub max_in_flight: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelCapacityMode {
    pub capacity_mode: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ApplyAutotuneCapacity {
    pub max_in_flight: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelWeight {
    pub admission_weight: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateMcpServer {
    pub name: String,
    pub upstream_url: String,
    pub auth_header: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMcpServer {
    pub upstream_url: String,
    pub auth_header: Option<String>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct ListKeysQuery {
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct AuditQuery {
    pub limit: Option<i64>,
}

/// OpenAPI shape for audit-log rows (`GET /api/v1/audit`).
#[derive(Debug, Serialize, ToSchema)]
pub struct AuditEntryView {
    pub id: i64,
    #[schema(value_type = String)]
    pub ts: chrono::DateTime<chrono::Utc>,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub detail: serde_json::Value,
}

// ---- handlers ------------------------------------------------------------

async fn health() -> &'static str {
    "ok"
}

async fn openapi_json() -> Json<utoipa::openapi::OpenApi> {
    use utoipa::OpenApi;
    Json(ApiDoc::openapi())
}

#[utoipa::path(
    post, path = "/api/v1/tenants", tag = "tenants",
    request_body = CreateTenant,
    responses((status = 200, body = Tenant))
)]
async fn create_tenant(
    State(state): State<AdminState>,
    Json(body): Json<CreateTenant>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .create_tenant(
            &body.name,
            body.weight.unwrap_or(100),
            body.tokens_per_minute.unwrap_or(60_000),
            body.max_in_flight,
            body.fairshare_group.as_deref(),
        )
        .await?;
    state
        .store
        .record_audit(
            "admin",
            "create_tenant",
            "tenant",
            &tenant.id.to_string(),
            serde_json::to_value(&tenant).unwrap_or_default(),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(get, path = "/api/v1/tenants", tag = "tenants", responses((status = 200, body = [Tenant])))]
async fn list_tenants(State(state): State<AdminState>) -> Result<Json<Vec<Tenant>>> {
    Ok(Json(state.store.list_tenants().await?))
}

#[utoipa::path(
    get, path = "/api/v1/tenants/{id}", tag = "tenants",
    params(("id" = Uuid, Path, description = "Tenant id")),
    responses((status = 200, body = Tenant), (status = 404))
)]
async fn get_tenant(State(state): State<AdminState>, Path(id): Path<Uuid>) -> Result<Json<Tenant>> {
    Ok(Json(state.store.get_tenant(id).await?))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}", tag = "tenants",
    request_body = UpdateTenant,
    responses((status = 200, body = Tenant))
)]
async fn update_tenant(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateTenant>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .update_tenant_details(
            id,
            &body.name,
            &body.description,
            &body.organization,
            &body.contact_email,
        )
        .await?;
    // Name is denormalized into every resolved key; re-push the tenant's keys.
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            "admin",
            "update_tenant",
            "tenant",
            &id.to_string(),
            serde_json::to_value(&tenant).unwrap_or_default(),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    patch, path = "/api/v1/tenants/{id}/status", tag = "tenants",
    request_body = SetTenantStatus,
    responses((status = 200, body = Tenant))
)]
async fn patch_tenant_status(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetTenantStatus>,
) -> Result<Json<Tenant>> {
    let status = body.status.trim().to_lowercase();
    if !matches!(status.as_str(), "active" | "suspended" | "archived") {
        return Err(AdminError::BadRequest(
            "status must be one of: active, suspended, archived".into(),
        ));
    }
    let tenant = state.store.set_tenant_status(id, &status).await?;
    // Status gates admission in the data plane; refresh the cached keys.
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_tenant_status",
            "tenant",
            &id.to_string(),
            serde_json::json!({ "status": status }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    patch, path = "/api/v1/tenants/{id}/schedule", tag = "tenants",
    request_body = SetTenantSchedule,
    responses((status = 200, body = Tenant))
)]
async fn patch_tenant_schedule(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetTenantSchedule>,
) -> Result<Json<Tenant>> {
    let timezone = body.timezone.trim();
    if timezone.parse::<chrono_tz::Tz>().is_err() {
        return Err(AdminError::BadRequest(format!(
            "unknown timezone '{timezone}'; use an IANA name like 'America/Phoenix' or 'UTC'"
        )));
    }
    if let (Some(from), Some(until)) = (body.active_from, body.active_until) {
        if until <= from {
            return Err(AdminError::BadRequest(
                "active_until must be after active_from".into(),
            ));
        }
    }
    if let Some(windows) = &body.weekly_windows {
        for w in windows {
            if w.day > 6 {
                return Err(AdminError::BadRequest(
                    "weekly window day must be 0 (Sunday) through 6 (Saturday)".into(),
                ));
            }
            if w.start_min > 1440 || w.end_min > 1440 || w.start_min >= w.end_min {
                return Err(AdminError::BadRequest(
                    "weekly window minutes must satisfy 0 <= start_min < end_min <= 1440".into(),
                ));
            }
        }
    }
    let tenant = state
        .store
        .update_tenant_schedule(
            id,
            timezone,
            body.active_from,
            body.active_until,
            body.weekly_windows,
        )
        .await?;
    // Schedule gates admission in the data plane; refresh the cached keys.
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_tenant_schedule",
            "tenant",
            &id.to_string(),
            serde_json::json!({
                "timezone": tenant.timezone,
                "active_from": tenant.active_from,
                "active_until": tenant.active_until,
                "weekly_windows": tenant.weekly_windows,
            }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    patch, path = "/api/v1/tenants/{id}/budget", tag = "tenants",
    request_body = SetTenantBudget,
    responses((status = 200, body = Tenant))
)]
async fn patch_tenant_budget(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetTenantBudget>,
) -> Result<Json<Tenant>> {
    let period = match body.budget_period.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(p) => {
            let p = p.to_lowercase();
            if !matches!(p.as_str(), "lifetime" | "monthly" | "term") {
                return Err(AdminError::BadRequest(
                    "budget_period must be one of: lifetime, monthly, term".into(),
                ));
            }
            Some(p)
        }
    };
    if let Some(tokens) = body.budget_tokens {
        if tokens < 0 {
            return Err(AdminError::BadRequest(
                "budget_tokens must be non-negative".into(),
            ));
        }
    }
    if let Some(cost) = body.budget_cost_usd {
        if cost < 0.0 || !cost.is_finite() {
            return Err(AdminError::BadRequest(
                "budget_cost_usd must be a non-negative number".into(),
            ));
        }
    }
    // Default the term start to now when a cap is set without an explicit start.
    let started_at = body.budget_started_at.or_else(|| {
        (body.budget_tokens.is_some() || body.budget_cost_usd.is_some()).then(chrono::Utc::now)
    });
    let tenant = state
        .store
        .update_tenant_budget(
            id,
            body.budget_tokens,
            body.budget_cost_usd,
            period.as_deref(),
            started_at,
        )
        .await?;
    // Budget gates admission in the data plane; refresh the cached keys.
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_tenant_budget",
            "tenant",
            &id.to_string(),
            serde_json::json!({
                "budget_tokens": tenant.budget_tokens,
                "budget_cost_usd": tenant.budget_cost_usd,
                "budget_period": tenant.budget_period,
                "budget_started_at": tenant.budget_started_at,
            }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    patch, path = "/api/v1/tenants/{id}/allowlist", tag = "tenants",
    request_body = SetTenantAllowlist,
    responses((status = 200, body = Tenant))
)]
async fn patch_tenant_allowlist(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetTenantAllowlist>,
) -> Result<Json<Tenant>> {
    // Normalize: trim, drop blanks, de-duplicate while preserving order.
    let mut seen = std::collections::HashSet::new();
    let models: Vec<String> = body
        .allowed_models
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty() && seen.insert(m.clone()))
        .collect();
    let allowed = (!models.is_empty()).then_some(models);
    let tenant = state.store.update_tenant_allowlist(id, allowed).await?;
    // Allowlist gates admission in the data plane; refresh the cached keys.
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_tenant_allowlist",
            "tenant",
            &id.to_string(),
            serde_json::json!({ "allowed_models": tenant.allowed_models }),
        )
        .await?;
    Ok(Json(tenant))
}

// ---- alert settings ----

/// Read-only view of the saved alert settings. Secrets (webhook URL, SMTP
/// password) are never returned; presence is reported via boolean flags.
#[derive(Debug, Serialize, ToSchema)]
pub struct AlertSettingsView {
    pub slack_webhook_set: bool,
    pub min_interval_secs: u64,
    pub email: Option<EmailSettingsView>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EmailSettingsView {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub username: Option<String>,
    pub password_set: bool,
    pub from_address: String,
    pub recipients: Vec<String>,
    pub starttls: bool,
}

impl AlertSettingsView {
    fn from_settings(s: &AlertSettings) -> Self {
        AlertSettingsView {
            slack_webhook_set: s.slack_enabled(),
            min_interval_secs: s.min_interval_secs,
            email: s.email.as_ref().map(|e| EmailSettingsView {
                smtp_host: e.smtp_host.clone(),
                smtp_port: e.smtp_port,
                username: e.username.clone(),
                password_set: e.password.as_ref().is_some_and(|p| !p.is_empty()),
                from_address: e.from_address.clone(),
                recipients: e.recipients.clone(),
                starttls: e.starttls,
            }),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateAlertSettings {
    /// New Slack webhook URL. Empty/omitted leaves the existing value untouched
    /// unless `clear_slack_webhook` is set.
    #[serde(default)]
    pub slack_webhook_url: Option<String>,
    #[serde(default)]
    pub clear_slack_webhook: bool,
    /// Cooldown between repeat alerts for the same key. Omitted keeps current.
    #[serde(default)]
    pub min_interval_secs: Option<u64>,
    /// Email delivery config. `null`/omitted disables email alerts.
    #[serde(default)]
    pub email: Option<UpdateEmailSettings>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateEmailSettings {
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub username: Option<String>,
    /// New SMTP password. Empty/omitted keeps the existing value unless
    /// `clear_smtp_password` is set.
    #[serde(default)]
    pub smtp_password: Option<String>,
    #[serde(default)]
    pub clear_smtp_password: bool,
    pub from_address: String,
    #[serde(default)]
    pub recipients: Vec<String>,
    #[serde(default = "default_true_bool")]
    pub starttls: bool,
}

fn default_smtp_port() -> u16 {
    587
}
fn default_true_bool() -> bool {
    true
}

#[utoipa::path(
    get, path = "/api/v1/settings/alerts", tag = "settings",
    responses((status = 200, body = AlertSettingsView))
)]
async fn get_alert_settings(State(state): State<AdminState>) -> Result<Json<AlertSettingsView>> {
    let settings = state.store.get_alert_settings().await?.unwrap_or_default();
    Ok(Json(AlertSettingsView::from_settings(&settings)))
}

#[utoipa::path(
    put, path = "/api/v1/settings/alerts", tag = "settings",
    request_body = UpdateAlertSettings,
    responses((status = 200, body = AlertSettingsView))
)]
async fn put_alert_settings(
    State(state): State<AdminState>,
    Json(body): Json<UpdateAlertSettings>,
) -> Result<Json<AlertSettingsView>> {
    let existing = state.store.get_alert_settings().await?.unwrap_or_default();

    // Slack webhook: set / keep / clear.
    let slack_webhook_url = match body.slack_webhook_url.as_deref().map(str::trim) {
        Some(url) if !url.is_empty() => Some(url.to_string()),
        _ if body.clear_slack_webhook => None,
        _ => existing.slack_webhook_url.clone(),
    };

    let min_interval_secs = body.min_interval_secs.unwrap_or(existing.min_interval_secs);

    // Email block: present => build (carrying over the password unless changed),
    // absent => disabled.
    let email = match body.email {
        None => None,
        Some(upd) => {
            if upd.smtp_host.trim().is_empty() {
                return Err(AdminError::BadRequest("smtp_host is required".into()));
            }
            if upd.from_address.trim().is_empty() {
                return Err(AdminError::BadRequest("from_address is required".into()));
            }
            let prev_password = existing.email.as_ref().and_then(|e| e.password.clone());
            let password = match upd.smtp_password.as_deref().map(str::trim) {
                Some(p) if !p.is_empty() => Some(p.to_string()),
                _ if upd.clear_smtp_password => None,
                _ => prev_password,
            };
            Some(EmailSettings {
                smtp_host: upd.smtp_host.trim().to_string(),
                smtp_port: upd.smtp_port,
                username: upd
                    .username
                    .map(|u| u.trim().to_string())
                    .filter(|u| !u.is_empty()),
                password,
                from_address: upd.from_address.trim().to_string(),
                recipients: upd
                    .recipients
                    .into_iter()
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty())
                    .collect(),
                starttls: upd.starttls,
            })
        }
    };

    let settings = AlertSettings {
        slack_webhook_url,
        email,
        min_interval_secs,
    };

    state.store.put_alert_settings(&settings).await?;
    state.alerts.update(settings.clone());
    state
        .store
        .record_audit(
            "admin",
            "set_alert_settings",
            "settings",
            "alerts",
            serde_json::json!({
                "slack_enabled": settings.slack_enabled(),
                "email_enabled": settings.email_enabled(),
                "min_interval_secs": settings.min_interval_secs,
            }),
        )
        .await?;
    Ok(Json(AlertSettingsView::from_settings(&settings)))
}

// ---- auto router settings ----

/// View of the persisted `auto` router classifier settings.
#[derive(Debug, Serialize, ToSchema)]
pub struct AutoRouterSettingsView {
    pub classifier_enabled: bool,
    pub classifier_model: Option<String>,
    pub classifier_timeout_ms: u64,
    /// The fixed tag vocabulary, surfaced so the UI can render tag pickers.
    pub available_tags: Vec<String>,
}

impl AutoRouterSettingsView {
    fn from_settings(s: &AutoRouterSettings) -> Self {
        AutoRouterSettingsView {
            classifier_enabled: s.classifier_enabled,
            classifier_model: s.classifier_model.clone(),
            classifier_timeout_ms: s.classifier_timeout_ms,
            available_tags: obleth_config::MODEL_TAGS
                .iter()
                .map(|t| t.to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateAutoRouterSettings {
    #[serde(default)]
    pub classifier_enabled: Option<bool>,
    /// `model_name` of the classifier brain. Empty string clears it.
    #[serde(default)]
    pub classifier_model: Option<String>,
    #[serde(default)]
    pub classifier_timeout_ms: Option<u64>,
}

#[utoipa::path(
    get, path = "/api/v1/settings/auto-router", tag = "settings",
    responses((status = 200, body = AutoRouterSettingsView))
)]
async fn get_auto_router_settings(
    State(state): State<AdminState>,
) -> Result<Json<AutoRouterSettingsView>> {
    let settings = state
        .store
        .get_auto_router_settings()
        .await?
        .unwrap_or_default();
    Ok(Json(AutoRouterSettingsView::from_settings(&settings)))
}

#[utoipa::path(
    put, path = "/api/v1/settings/auto-router", tag = "settings",
    request_body = UpdateAutoRouterSettings,
    responses((status = 200, body = AutoRouterSettingsView))
)]
async fn put_auto_router_settings(
    State(state): State<AdminState>,
    Json(body): Json<UpdateAutoRouterSettings>,
) -> Result<Json<AutoRouterSettingsView>> {
    let existing = state
        .store
        .get_auto_router_settings()
        .await?
        .unwrap_or_default();

    let classifier_model = match body.classifier_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.classifier_model.clone(),
    };

    let settings = AutoRouterSettings {
        classifier_enabled: body
            .classifier_enabled
            .unwrap_or(existing.classifier_enabled),
        classifier_model,
        classifier_timeout_ms: body
            .classifier_timeout_ms
            .filter(|ms| *ms > 0)
            .unwrap_or(existing.classifier_timeout_ms),
    };

    state.store.put_auto_router_settings(&settings).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_auto_router_settings",
            "settings",
            "auto_router",
            serde_json::json!({
                "classifier_enabled": settings.classifier_enabled,
                "classifier_model": settings.classifier_model,
                "classifier_timeout_ms": settings.classifier_timeout_ms,
            }),
        )
        .await?;
    Ok(Json(AutoRouterSettingsView::from_settings(&settings)))
}

/// View of the persisted model-"boons" settings. Flattened to the vision boon's
/// fields since it is currently the only boon.
#[derive(Debug, Serialize, ToSchema)]
pub struct BoonSettingsView {
    pub vision_enabled: bool,
    pub vision_fallback_model: Option<String>,
    pub vision_describe_prompt: String,
    pub vision_max_images: u32,
    pub vision_timeout_ms: u64,
}

impl BoonSettingsView {
    fn from_settings(s: &BoonSettings) -> Self {
        BoonSettingsView {
            vision_enabled: s.vision.enabled,
            vision_fallback_model: s.vision.fallback_model.clone(),
            vision_describe_prompt: s.vision.describe_prompt.clone(),
            vision_max_images: s.vision.max_images,
            vision_timeout_ms: s.vision.timeout_ms,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateBoonSettings {
    #[serde(default)]
    pub vision_enabled: Option<bool>,
    /// `model_name` of the vision describer model. Empty string clears it.
    #[serde(default)]
    pub vision_fallback_model: Option<String>,
    #[serde(default)]
    pub vision_describe_prompt: Option<String>,
    #[serde(default)]
    pub vision_max_images: Option<u32>,
    #[serde(default)]
    pub vision_timeout_ms: Option<u64>,
}

#[utoipa::path(
    get, path = "/api/v1/settings/boons", tag = "settings",
    responses((status = 200, body = BoonSettingsView))
)]
async fn get_boon_settings(State(state): State<AdminState>) -> Result<Json<BoonSettingsView>> {
    let settings = state.store.get_boon_settings().await?.unwrap_or_default();
    Ok(Json(BoonSettingsView::from_settings(&settings)))
}

#[utoipa::path(
    put, path = "/api/v1/settings/boons", tag = "settings",
    request_body = UpdateBoonSettings,
    responses((status = 200, body = BoonSettingsView))
)]
async fn put_boon_settings(
    State(state): State<AdminState>,
    Json(body): Json<UpdateBoonSettings>,
) -> Result<Json<BoonSettingsView>> {
    let existing = state.store.get_boon_settings().await?.unwrap_or_default();

    let fallback_model = match body.vision_fallback_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.vision.fallback_model.clone(),
    };

    let describe_prompt = match body.vision_describe_prompt.as_deref().map(str::trim) {
        Some("") | None => existing.vision.describe_prompt.clone(),
        Some(p) => p.to_string(),
    };

    let settings = BoonSettings {
        vision: VisionBoonSettings {
            enabled: body.vision_enabled.unwrap_or(existing.vision.enabled),
            fallback_model,
            describe_prompt,
            max_images: body
                .vision_max_images
                .filter(|n| *n > 0)
                .unwrap_or(existing.vision.max_images),
            timeout_ms: body
                .vision_timeout_ms
                .filter(|ms| *ms > 0)
                .unwrap_or(existing.vision.timeout_ms),
        },
    };

    state.store.put_boon_settings(&settings).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_boon_settings",
            "settings",
            "boons",
            serde_json::json!({
                "vision_enabled": settings.vision.enabled,
                "vision_fallback_model": settings.vision.fallback_model,
                "vision_max_images": settings.vision.max_images,
                "vision_timeout_ms": settings.vision.timeout_ms,
            }),
        )
        .await?;
    Ok(Json(BoonSettingsView::from_settings(&settings)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TestAlertResult {
    pub results: Vec<alerts::ChannelResult>,
}

#[utoipa::path(
    post, path = "/api/v1/settings/alerts/test", tag = "settings",
    responses((status = 200, body = TestAlertResult))
)]
async fn test_alert_settings(State(state): State<AdminState>) -> Result<Json<TestAlertResult>> {
    if !state.alerts.enabled() {
        return Err(AdminError::BadRequest(
            "no alert channels are configured".into(),
        ));
    }
    let results = state.alerts.send_test().await;
    Ok(Json(TestAlertResult { results }))
}

#[utoipa::path(
    delete, path = "/api/v1/tenants/{id}", tag = "tenants",
    responses((status = 204))
)]
async fn delete_tenant(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    let hashes = state.store.delete_tenant(id).await?;
    // Evict every cascaded key from the data-plane cache.
    for hash in &hashes {
        let _ = state.redis.delete_resolved_key(hash).await;
        let _ = state.redis.publish_invalidation(hash).await;
    }
    state
        .store
        .record_audit(
            "admin",
            "delete_tenant",
            "tenant",
            &id.to_string(),
            serde_json::json!({ "keys_removed": hashes.len() }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    patch, path = "/api/v1/tenants/{id}/weight", tag = "tenants",
    request_body = UpdateWeight,
    responses((status = 200, body = Tenant))
)]
async fn patch_weight(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateWeight>,
) -> Result<Json<Tenant>> {
    let tenant = state.store.update_tenant_weight(id, body.weight).await?;
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            "admin",
            "update_weight",
            "tenant",
            &id.to_string(),
            serde_json::json!({ "weight": body.weight }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/quota", tag = "tenants",
    params(("id" = Uuid, Path, description = "Tenant id")),
    request_body = UpdateQuota,
    responses((status = 200, body = Tenant))
)]
async fn put_quota(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateQuota>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .update_tenant_quota(id, body.tokens_per_minute, body.max_in_flight)
        .await?;
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            "admin",
            "update_quota",
            "tenant",
            &id.to_string(),
            serde_json::json!({
                "tokens_per_minute": body.tokens_per_minute,
                "max_in_flight": body.max_in_flight
            }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    post, path = "/api/v1/tenants/{id}/keys", tag = "keys",
    request_body = CreateKey,
    responses((status = 200, body = CreatedKey))
)]
async fn create_key(
    State(state): State<AdminState>,
    Path(tenant_id): Path<Uuid>,
    Json(body): Json<CreateKey>,
) -> Result<Json<CreatedKey>> {
    let (key, secret) = state.store.create_api_key(tenant_id, &body.name).await?;
    let hash = hash_api_key(&secret);
    if let Some(resolved) = state.store.resolved_key_by_hash(&hash).await? {
        push_key(&state, &hash, &resolved).await?;
    }
    state
        .store
        .record_audit(
            "admin",
            "create_key",
            "api_key",
            &key.id.to_string(),
            serde_json::json!({ "tenant_id": tenant_id, "prefix": key.key_prefix }),
        )
        .await?;
    Ok(Json(CreatedKey { key, secret }))
}

#[utoipa::path(
    get, path = "/api/v1/keys", tag = "keys",
    params(ListKeysQuery),
    responses((status = 200, body = [ApiKey]))
)]
async fn list_keys(
    State(state): State<AdminState>,
    Query(q): Query<ListKeysQuery>,
) -> Result<Json<Vec<ApiKey>>> {
    Ok(Json(state.store.list_keys(q.tenant_id).await?))
}

#[utoipa::path(
    delete, path = "/api/v1/keys/{id}", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    responses((status = 204), (status = 404))
)]
async fn delete_key(State(state): State<AdminState>, Path(id): Path<Uuid>) -> Result<StatusCode> {
    let hash = state.store.delete_key(id).await?;
    let _ = state.redis.delete_resolved_key(&hash).await;
    let _ = state.redis.publish_invalidation(&hash).await;
    state
        .store
        .record_audit(
            "admin",
            "delete_key",
            "api_key",
            &id.to_string(),
            serde_json::json!({}),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put, path = "/api/v1/keys/{id}/disabled", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    request_body = SetDisabled,
    responses((status = 204), (status = 404))
)]
async fn set_key_disabled(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetDisabled>,
) -> Result<StatusCode> {
    let (hash, resolved) = state.store.set_key_disabled(id, body.disabled).await?;
    push_key(&state, &hash, &resolved).await?;
    state
        .store
        .record_audit(
            "admin",
            if body.disabled {
                "disable_key"
            } else {
                "enable_key"
            },
            "api_key",
            &id.to_string(),
            serde_json::json!({ "disabled": body.disabled }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get, path = "/api/v1/usage", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = [usage::UsageAgg]))
)]
async fn get_usage(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<Vec<usage::UsageAgg>>> {
    Ok(Json(usage::query_usage(&state.clickhouse, q).await?))
}

#[utoipa::path(
    get, path = "/api/v1/usage/keys", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = [usage::UsageKeyAgg]))
)]
async fn get_usage_keys(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<Vec<usage::UsageKeyAgg>>> {
    Ok(Json(usage::query_usage_by_key(&state.clickhouse, q).await?))
}

/// Activity summary for a single API key: last-used timestamp, last model and
/// status, and rolling request/token/cost totals. 404s when the key id is
/// unknown; a known key with no traffic yet returns a zeroed summary
/// (`last_used_ms = 0`) rather than 404.
#[utoipa::path(
    get, path = "/api/v1/keys/{id}/usage", tag = "keys",
    params(
        ("id" = Uuid, Path, description = "API key id"),
        usage::KeyUsageSummaryQuery
    ),
    responses((status = 200, body = usage::KeyUsageSummary), (status = 404))
)]
async fn get_key_usage(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Query(q): Query<usage::KeyUsageSummaryQuery>,
) -> Result<Json<usage::KeyUsageSummary>> {
    // Validate the key exists (and recover its tenant for the never-used case)
    // before touching ClickHouse, so unknown ids are a clean 404.
    let key = state
        .store
        .keys_by_ids(&[id])
        .await?
        .into_iter()
        .next()
        .ok_or(AdminError::NotFound)?;

    let summary = usage::query_key_usage_summary(&state.clickhouse, id, q.since_ms)
        .await?
        .unwrap_or(usage::KeyUsageSummary {
            key_id: id,
            tenant_id: key.tenant_id,
            last_used_ms: 0,
            last_model: String::new(),
            last_status_code: 0,
            requests: 0,
            input_tokens: 0,
            output_tokens: 0,
            total_tokens: 0,
            cost_usd: 0.0,
        });
    Ok(Json(summary))
}

/// Bulk per-key activity summary for the dashboard Keys table. Returns the
/// busiest keys (by token volume) that saw traffic in the window, each with
/// last-used metadata so the UI can show "last used" without N+1 log fetches.
#[utoipa::path(
    get, path = "/api/v1/usage/keys/summary", tag = "usage",
    params(usage::KeyUsageSummaryQuery),
    responses((status = 200, body = [usage::KeyUsageSummary]))
)]
async fn get_usage_keys_summary(
    State(state): State<AdminState>,
    Query(q): Query<usage::KeyUsageSummaryQuery>,
) -> Result<Json<Vec<usage::KeyUsageSummary>>> {
    Ok(Json(
        usage::query_keys_usage_summary(&state.clickhouse, q).await?,
    ))
}

#[utoipa::path(
    get, path = "/api/v1/usage/models", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = [usage::UsageModelAgg]))
)]
async fn get_usage_models(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<Vec<usage::UsageModelAgg>>> {
    Ok(Json(
        usage::query_usage_by_model(&state.clickhouse, q).await?,
    ))
}

#[utoipa::path(
    get, path = "/api/v1/usage/series", tag = "usage",
    params(usage::UsageSeriesQuery),
    responses((status = 200, body = [usage::UsageTimePoint]))
)]
async fn get_usage_series(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageSeriesQuery>,
) -> Result<Json<Vec<usage::UsageTimePoint>>> {
    Ok(Json(usage::query_usage_series(&state.clickhouse, q).await?))
}

#[utoipa::path(
    get, path = "/api/v1/usage/series/tenants", tag = "usage",
    params(usage::UsageSeriesQuery),
    responses((status = 200, body = [usage::TenantUsageTimePoint]))
)]
async fn get_usage_series_tenants(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageSeriesQuery>,
) -> Result<Json<Vec<usage::TenantUsageTimePoint>>> {
    Ok(Json(
        usage::query_usage_series_by_tenant(&state.clickhouse, q).await?,
    ))
}

#[utoipa::path(
    get, path = "/api/v1/usage/cache", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = usage::CacheStats))
)]
async fn get_cache_stats(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<usage::CacheStats>> {
    Ok(Json(usage::query_cache_stats(&state.clickhouse, q).await?))
}

#[utoipa::path(
    get, path = "/api/v1/costs", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = [usage::CostAgg]))
)]
async fn get_costs(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<Vec<usage::CostAgg>>> {
    let models = state.store.list_models().await?;
    let costs: Vec<(String, f64, f64)> = models
        .iter()
        .map(|m| {
            (
                m.model_name.clone(),
                m.input_cost_per_token,
                m.output_cost_per_token,
            )
        })
        .collect();
    Ok(Json(
        usage::query_costs(&state.clickhouse, q.since_ms, &costs).await?,
    ))
}

/// A request-log row enriched with the human-readable tenant and key names
/// resolved from Postgres, so the live log view does not have to display bare
/// UUIDs.
#[derive(Debug, Serialize)]
pub struct UsageLogEntry {
    #[serde(flatten)]
    pub row: usage::UsageLogRow,
    pub tenant_name: String,
    pub key_name: String,
    pub key_prefix: String,
}

/// Newest-first feed of individual requests for the live log view. ClickHouse
/// stores only UUIDs, so the page's tenant/key ids are resolved to names in a
/// pair of bounded Postgres lookups (tenants are few; keys are fetched by the
/// exact ids on the page rather than the full fleet).
#[utoipa::path(
    get, path = "/api/v1/usage/logs", tag = "usage",
    params(usage::UsageLogQuery),
    responses((status = 200, body = [usage::UsageLogRow],
        description = "Each row also includes tenant_name, key_name, and key_prefix resolved from Postgres"))
)]
async fn get_usage_logs(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageLogQuery>,
) -> Result<Json<Vec<UsageLogEntry>>> {
    let rows = usage::query_usage_logs(&state.clickhouse, q).await?;

    let tenant_names: std::collections::HashMap<Uuid, String> = state
        .store
        .list_tenants()
        .await?
        .into_iter()
        .map(|t| (t.id, t.name))
        .collect();

    let key_ids: Vec<Uuid> = {
        let mut ids: Vec<Uuid> = rows.iter().map(|r| r.key_id).collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    };
    let key_meta: std::collections::HashMap<Uuid, (String, String)> = state
        .store
        .keys_by_ids(&key_ids)
        .await?
        .into_iter()
        .map(|k| (k.id, (k.name, k.key_prefix)))
        .collect();

    let entries = rows
        .into_iter()
        .map(|row| {
            let tenant_name = tenant_names
                .get(&row.tenant_id)
                .cloned()
                .unwrap_or_default();
            let (key_name, key_prefix) = key_meta.get(&row.key_id).cloned().unwrap_or_default();
            UsageLogEntry {
                row,
                tenant_name,
                key_name,
                key_prefix,
            }
        })
        .collect();

    Ok(Json(entries))
}

#[utoipa::path(
    get, path = "/api/v1/usage/daily", tag = "usage",
    params(usage::UsageDailyQuery),
    responses((status = 200, body = [usage::UsageDailyRow]))
)]
async fn get_usage_daily(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageDailyQuery>,
) -> Result<Json<Vec<usage::UsageDailyRow>>> {
    let key_ids = usage::parse_key_ids(q.key_id.as_deref())
        .map_err(|e| AdminError::BadRequest(format!("invalid key_id: {e}")))?;
    Ok(Json(
        usage::query_usage_daily(&state.clickhouse, q, &key_ids).await?,
    ))
}

/// View of the persisted raw-usage retention window.
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageRetentionView {
    /// Days of raw per-request history retained before pruning.
    pub days: i64,
    /// True when this reflects a saved setting rather than the env default.
    pub configured: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateUsageRetention {
    pub days: i64,
}

#[utoipa::path(
    get, path = "/api/v1/settings/usage-retention", tag = "settings",
    responses((status = 200, body = UsageRetentionView))
)]
async fn get_usage_retention(
    State(state): State<AdminState>,
) -> Result<Json<UsageRetentionView>> {
    let saved = state.store.get_usage_retention_settings().await?;
    let view = match saved {
        Some(s) => UsageRetentionView {
            days: s.days,
            configured: true,
        },
        None => UsageRetentionView {
            days: state.usage_retention_default_days,
            configured: false,
        },
    };
    Ok(Json(view))
}

#[utoipa::path(
    put, path = "/api/v1/settings/usage-retention", tag = "settings",
    request_body = UpdateUsageRetention,
    responses((status = 200, body = UsageRetentionView))
)]
async fn put_usage_retention(
    State(state): State<AdminState>,
    Json(body): Json<UpdateUsageRetention>,
) -> Result<Json<UsageRetentionView>> {
    if body.days < 1 {
        return Err(AdminError::BadRequest(
            "retention days must be at least 1".into(),
        ));
    }
    let settings = obleth_config::UsageRetentionSettings { days: body.days };
    state.store.put_usage_retention_settings(&settings).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_usage_retention",
            "settings",
            "usage_retention",
            serde_json::json!({ "days": settings.days }),
        )
        .await?;
    Ok(Json(UsageRetentionView {
        days: settings.days,
        configured: true,
    }))
}

/// Result of a manual compaction run.
#[derive(Debug, Serialize, ToSchema)]
pub struct CompactUsageResult {
    pub retention_days: i64,
    pub partitions_dropped: usize,
}

#[utoipa::path(
    post, path = "/api/v1/usage/compact", tag = "usage",
    responses((status = 200, body = CompactUsageResult))
)]
async fn compact_usage(State(state): State<AdminState>) -> Result<Json<CompactUsageResult>> {
    let result = usage_retention::compact_usage_now(&state).await?;
    state
        .store
        .record_audit(
            "admin",
            "compact_usage",
            "usage",
            "usage",
            serde_json::json!({
                "retention_days": result.retention_days,
                "partitions_dropped": result.partitions_dropped,
            }),
        )
        .await?;
    Ok(Json(CompactUsageResult {
        retention_days: result.retention_days,
        partitions_dropped: result.partitions_dropped,
    }))
}

#[utoipa::path(
    get, path = "/api/v1/stats", tag = "usage",
    responses((status = 200, body = LiveStats))
)]
async fn get_stats(State(state): State<AdminState>) -> Json<LiveStats> {
    use obleth_fairshare::CapacityProvider;
    use std::sync::atomic::Ordering;
    Json(LiveStats {
        in_flight: state.fairshare_stats.in_flight.load(Ordering::Relaxed),
        queued: state.fairshare_stats.queued.load(Ordering::Relaxed),
        max_in_flight: state.capacity.max_in_flight(),
    })
}

#[utoipa::path(
    get, path = "/api/v1/fairshare/live", tag = "fairshare",
    responses((status = 200, body = FairshareLiveView))
)]
async fn get_fairshare_live(State(state): State<AdminState>) -> Result<Json<FairshareLiveView>> {
    let snap = state
        .fairshare
        .snapshot()
        .await
        .ok_or(AdminError::Internal("fairshare unavailable".into()))?;
    let tenants = state.store.list_tenants().await?;
    let names: std::collections::HashMap<Uuid, String> =
        tenants.into_iter().map(|t| (t.id, t.name)).collect();
    let hidden_group = snap
        .groups
        .iter()
        .find(|g| g.name == model_health::HEALTH_GROUP)
        .cloned();
    let hidden_in_flight = hidden_group.as_ref().map(|g| g.in_flight).unwrap_or(0);
    let hidden_queued = hidden_group.as_ref().map(|g| g.queued).unwrap_or(0);
    Ok(Json(FairshareLiveView {
        algorithm: snap.algorithm,
        max_in_flight: snap.max_in_flight,
        global_in_flight: snap.global_in_flight.saturating_sub(hidden_in_flight),
        global_queued: snap.global_queued.saturating_sub(hidden_queued) as i64,
        groups: snap
            .groups
            .into_iter()
            .filter(|g| g.name != model_health::HEALTH_GROUP)
            .map(|g| GroupFairshareView {
                name: g.name,
                weight: g.weight,
                in_flight: g.in_flight,
                queued: g.queued,
                slot_cap: g.slot_cap,
                served_tokens: g.served_tokens,
                share_score: g.share_score,
                weight_share: g.weight_share,
                expected_slots: g.weight_share * snap.max_in_flight as f64,
            })
            .collect(),
        tenants: snap
            .tenants
            .into_iter()
            .filter(|t| {
                t.tenant_id != model_health::health_tenant_id()
                    && t.fairshare_group != model_health::HEALTH_GROUP
            })
            .map(|t| TenantFairshareView {
                name: names
                    .get(&t.tenant_id)
                    .cloned()
                    .unwrap_or_else(|| t.tenant_id.to_string()),
                fairshare_group: t.fairshare_group,
                expected_slots: t.weight_share * snap.max_in_flight as f64,
                tenant_id: t.tenant_id,
                weight: t.weight,
                in_flight: t.in_flight,
                queued: t.queued,
                served_tokens: t.served_tokens,
                share_score: t.share_score,
                weight_share: t.weight_share,
            })
            .collect(),
    }))
}

#[utoipa::path(
    get, path = "/api/v1/fairshare/groups", tag = "fairshare",
    responses((status = 200, body = [FairshareGroup]))
)]
async fn list_fairshare_groups(
    State(state): State<AdminState>,
) -> Result<Json<Vec<FairshareGroup>>> {
    Ok(Json(state.store.list_fairshare_groups().await?))
}

#[utoipa::path(
    post, path = "/api/v1/fairshare/groups", tag = "fairshare",
    request_body = CreateFairshareGroup,
    responses((status = 200, body = FairshareGroup))
)]
async fn create_fairshare_group(
    State(state): State<AdminState>,
    Json(body): Json<CreateFairshareGroup>,
) -> Result<Json<FairshareGroup>> {
    let group = state
        .store
        .create_fairshare_group(&body.name, body.weight.unwrap_or(100))
        .await?;
    state
        .store
        .record_audit(
            "admin",
            "create_fairshare_group",
            "fairshare_group",
            &group.name,
            serde_json::to_value(&group).unwrap_or_default(),
        )
        .await?;
    Ok(Json(group))
}

#[utoipa::path(
    patch, path = "/api/v1/fairshare/groups/{name}/weight", tag = "fairshare",
    params(("name" = String, Path, description = "Fairshare group name")),
    request_body = UpdateGroupWeight,
    responses((status = 200, body = FairshareGroup))
)]
async fn patch_fairshare_group_weight(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    Json(body): Json<UpdateGroupWeight>,
) -> Result<Json<FairshareGroup>> {
    let group = state
        .store
        .update_fairshare_group_weight(&name, body.weight)
        .await?;
    resync_all_keys(&state).await?;
    state
        .store
        .record_audit(
            "admin",
            "update_fairshare_group_weight",
            "fairshare_group",
            &name,
            serde_json::json!({ "weight": body.weight }),
        )
        .await?;
    Ok(Json(group))
}

#[utoipa::path(
    patch, path = "/api/v1/tenants/{id}/group", tag = "tenants",
    params(("id" = Uuid, Path, description = "Tenant id")),
    request_body = UpdateTenantGroup,
    responses((status = 200, body = Tenant))
)]
async fn patch_tenant_group(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateTenantGroup>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .update_tenant_fairshare_group(id, &body.fairshare_group)
        .await?;
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            "admin",
            "update_tenant_group",
            "tenant",
            &id.to_string(),
            serde_json::json!({ "fairshare_group": body.fairshare_group }),
        )
        .await?;
    Ok(Json(tenant))
}

async fn resync_all_keys(state: &AdminState) -> Result<()> {
    let keys = state.store.all_resolved_keys().await?;
    for (hash, resolved) in keys {
        push_key(state, &hash, &resolved).await?;
    }
    Ok(())
}

#[utoipa::path(
    post, path = "/api/v1/models", tag = "models",
    request_body = CreateModel,
    responses((status = 200, body = ModelRoute))
)]
async fn create_model(
    State(state): State<AdminState>,
    Json(body): Json<CreateModel>,
) -> Result<Json<ModelRoute>> {
    state.ssrf.validate(&body.api_base)?;
    let model = state
        .store
        .create_model(
            &body.model_name,
            body.description.as_deref().unwrap_or_default(),
            &body.upstream_model,
            &body.api_base,
            body.api_key.as_deref(),
            body.model_type.as_deref().unwrap_or("chat"),
            body.input_cost_per_token.unwrap_or(0.0),
            body.output_cost_per_token.unwrap_or(0.0),
            body.cost_per_image.unwrap_or(0.0),
            body.cost_per_audio_second.unwrap_or(0.0),
            body.cost_per_character.unwrap_or(0.0),
            body.context_window.unwrap_or(8192),
            body.admission_weight.unwrap_or(100),
            body.max_in_flight,
            body.supports_function_calling.unwrap_or(false),
            body.supports_system_messages.unwrap_or(true),
            body.supports_response_schema.unwrap_or(false),
            body.supports_tool_choice.unwrap_or(false),
            body.supports_vision.unwrap_or(false),
            &body.tags.clone().unwrap_or_default(),
            &body.boons.clone().unwrap_or_default(),
        )
        .await?;
    if state.health.default_interval_secs != 900 {
        let _ = state
            .store
            .update_model_health_config(
                model.id,
                obleth_store::ModelHealthConfigUpdate {
                    checks_enabled: true,
                    alerts_enabled: true,
                    check_interval_secs: state.health.default_interval_secs,
                    failure_threshold: 2,
                    maintenance_until: None,
                    maintenance_note: None,
                },
            )
            .await?;
    }
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            "admin",
            "create_model",
            "model",
            &model.id.to_string(),
            serde_json::to_value(&model).unwrap_or_default(),
        )
        .await?;
    Ok(Json(model))
}

#[utoipa::path(
    get, path = "/api/v1/models", tag = "models",
    responses((status = 200, body = [ModelRoute]))
)]
async fn list_models(State(state): State<AdminState>) -> Result<Json<Vec<ModelRoute>>> {
    Ok(Json(state.store.list_models().await?))
}

#[utoipa::path(
    get, path = "/api/v1/models/{id}", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 200, body = ModelRoute), (status = 404))
)]
async fn get_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ModelRoute>> {
    Ok(Json(state.store.get_model(id).await?))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = UpdateModel,
    responses((status = 200, body = ModelRoute))
)]
async fn update_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateModel>,
) -> Result<Json<ModelRoute>> {
    state.ssrf.validate(&body.api_base)?;
    let existing = state.store.get_model(id).await?;
    let api_key = body.api_key.as_deref().or(existing.api_key.as_deref());
    let model = state
        .store
        .update_model(
            id,
            body.description.as_deref().unwrap_or(&existing.description),
            &body.upstream_model,
            &body.api_base,
            api_key,
            body.model_type.as_deref().unwrap_or(&existing.model_type),
            body.input_cost_per_token
                .unwrap_or(existing.input_cost_per_token),
            body.output_cost_per_token
                .unwrap_or(existing.output_cost_per_token),
            body.cost_per_image.unwrap_or(existing.cost_per_image),
            body.cost_per_audio_second
                .unwrap_or(existing.cost_per_audio_second),
            body.cost_per_character
                .unwrap_or(existing.cost_per_character),
            body.context_window.unwrap_or(existing.context_window),
            body.admission_weight.unwrap_or(existing.admission_weight),
            body.max_in_flight.or(existing.max_in_flight),
            body.supports_function_calling
                .unwrap_or(existing.supports_function_calling),
            body.supports_system_messages
                .unwrap_or(existing.supports_system_messages),
            body.supports_response_schema
                .unwrap_or(existing.supports_response_schema),
            body.supports_tool_choice
                .unwrap_or(existing.supports_tool_choice),
            body.supports_vision.unwrap_or(existing.supports_vision),
            body.enabled.unwrap_or(existing.enabled),
            &body.tags.clone().unwrap_or_else(|| existing.tags.clone()),
            &body.boons.clone().unwrap_or_else(|| existing.boons.clone()),
        )
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            "admin",
            "update_model",
            "model",
            &id.to_string(),
            serde_json::json!({ "model_name": model.model_name }),
        )
        .await?;
    Ok(Json(model))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/capacity", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelCapacity,
    responses((status = 200, body = ModelRoute))
)]
async fn set_model_capacity(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetModelCapacity>,
) -> Result<Json<ModelRoute>> {
    let model = state
        .store
        .update_model_capacity(id, body.max_in_flight)
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_model_capacity",
            "model",
            &id.to_string(),
            serde_json::json!({ "max_in_flight": model.max_in_flight }),
        )
        .await?;
    Ok(Json(model))
}

#[utoipa::path(
    put,
    path = "/api/v1/models/{id}/capacity-mode",
    tag = "models",
    request_body = SetModelCapacityMode,
    responses((status = 200, body = ModelRoute))
)]
async fn set_model_capacity_mode(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetModelCapacityMode>,
) -> Result<Json<ModelRoute>> {
    if !obleth_config::is_valid_capacity_mode(body.capacity_mode.trim()) {
        return Err(AdminError::BadRequest(format!(
            "invalid capacity_mode `{}` (expected `static` or `tuned`)",
            body.capacity_mode
        )));
    }
    let model = state
        .store
        .update_model_capacity_mode(id, &body.capacity_mode)
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_model_capacity_mode",
            "model",
            &id.to_string(),
            serde_json::json!({ "capacity_mode": model.capacity_mode }),
        )
        .await?;
    Ok(Json(model))
}

/// Run an auto-tune ramp probe against the model's upstream and return a
/// recommendation. Recommend-only: this writes no config. The probe drives
/// real load directly at the upstream, so it costs upstream tokens.
#[utoipa::path(
    post,
    path = "/api/v1/models/{id}/autotune",
    tag = "models",
    request_body = autotune::AutotuneRequest,
    responses((status = 200, body = autotune::AutotuneReport))
)]
async fn autotune_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<autotune::AutotuneRequest>,
) -> Result<Json<autotune::AutotuneReport>> {
    let model = state.store.get_model(id).await?;
    let report = autotune::run_probe(&state.health.http, &model, &body)
        .await
        .map_err(|e| AdminError::BadRequest(e.to_string()))?;
    state
        .store
        .record_audit(
            "admin",
            "autotune_model",
            "model",
            &id.to_string(),
            serde_json::json!({
                "recommended_max_in_flight": report.recommended_max_in_flight,
                "knee_reason": report.knee_reason,
                "baseline_p99_ms": report.baseline_p99_ms,
                "latency_ceiling_ms": report.latency_ceiling_ms,
                "latency_headroom": report.latency_headroom,
                "workload": report.workload,
                "max_concurrency": report.max_concurrency,
            }),
        )
        .await?;
    Ok(Json(report))
}

/// Apply an auto-tune recommendation: set `max_in_flight`, flip the model to
/// `tuned` mode, and stamp the tuned timestamp.
#[utoipa::path(
    post,
    path = "/api/v1/models/{id}/autotune/apply",
    tag = "models",
    request_body = ApplyAutotuneCapacity,
    responses((status = 200, body = ModelRoute))
)]
async fn apply_autotune_capacity(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<ApplyAutotuneCapacity>,
) -> Result<Json<ModelRoute>> {
    if body.max_in_flight < 1 {
        return Err(AdminError::BadRequest(
            "max_in_flight must be >= 1".to_string(),
        ));
    }
    let model = state
        .store
        .apply_tuned_model_capacity(id, body.max_in_flight)
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            "admin",
            "apply_autotune_capacity",
            "model",
            &id.to_string(),
            serde_json::json!({
                "max_in_flight": model.max_in_flight,
                "capacity_mode": model.capacity_mode,
            }),
        )
        .await?;
    Ok(Json(model))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/weight", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelWeight,
    responses((status = 200, body = ModelRoute))
)]
async fn set_model_weight(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetModelWeight>,
) -> Result<Json<ModelRoute>> {
    let model = state
        .store
        .update_model_admission_weight(id, body.admission_weight)
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_model_weight",
            "model",
            &id.to_string(),
            serde_json::json!({ "admission_weight": model.admission_weight }),
        )
        .await?;
    Ok(Json(model))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/cache", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelCache,
    responses((status = 200, body = ModelRoute))
)]
async fn set_model_cache(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetModelCache>,
) -> Result<Json<ModelRoute>> {
    let model = state
        .store
        .update_model_cache(id, body.cache_enabled, body.cache_ttl_secs.unwrap_or(300))
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_model_cache",
            "model",
            &id.to_string(),
            serde_json::json!({
                "cache_enabled": model.cache_enabled,
                "cache_ttl_secs": model.cache_ttl_secs
            }),
        )
        .await?;
    Ok(Json(model))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/reliability", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelReliability,
    responses((status = 200, body = ModelRoute))
)]
async fn set_model_reliability(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SetModelReliability>,
) -> Result<Json<ModelRoute>> {
    let model = state
        .store
        .update_model_reliability(
            id,
            body.request_timeout_secs,
            body.max_retries,
            body.retry_backoff_ms,
            &body.endpoint_selection_mode,
        )
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_model_reliability",
            "model",
            &id.to_string(),
            serde_json::json!({
                "request_timeout_secs": model.request_timeout_secs,
                "max_retries": model.max_retries,
                "retry_backoff_ms": model.retry_backoff_ms,
                "endpoint_selection_mode": model.endpoint_selection_mode,
            }),
        )
        .await?;
    Ok(Json(model))
}

// ---- model endpoints -----------------------------------------------------

#[utoipa::path(
    get, path = "/api/v1/models/{id}/endpoints", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 200, body = [ModelEndpoint]))
)]
async fn list_model_endpoints(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ModelEndpoint>>> {
    // Confirm the model exists so callers get 404 (not an empty list) for a
    // bad id.
    state.store.get_model(id).await?;
    Ok(Json(state.store.list_model_endpoints(id).await?))
}

#[utoipa::path(
    post, path = "/api/v1/models/{id}/endpoints", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = CreateModelEndpoint,
    responses((status = 200, body = ModelEndpoint))
)]
async fn create_model_endpoint(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<CreateModelEndpoint>,
) -> Result<Json<ModelEndpoint>> {
    state.ssrf.validate(&body.api_base)?;
    let model = state.store.get_model(id).await?;
    let endpoint = state
        .store
        .create_model_endpoint(
            id,
            &body.name,
            &body.api_base,
            body.api_key.as_deref(),
            body.priority,
            body.weight,
            body.enabled,
        )
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            "admin",
            "create_model_endpoint",
            "model_endpoint",
            &endpoint.id.to_string(),
            serde_json::json!({
                "model": model.model_name,
                "name": endpoint.name,
                "api_base": endpoint.api_base,
            }),
        )
        .await?;
    Ok(Json(endpoint))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/endpoints/{endpoint_id}", tag = "models",
    params(
        ("id" = Uuid, Path, description = "Model id"),
        ("endpoint_id" = Uuid, Path, description = "Endpoint id")
    ),
    request_body = UpdateModelEndpoint,
    responses((status = 200, body = ModelEndpoint))
)]
async fn update_model_endpoint(
    State(state): State<AdminState>,
    Path((id, endpoint_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<UpdateModelEndpoint>,
) -> Result<Json<ModelEndpoint>> {
    state.ssrf.validate(&body.api_base)?;
    let model = state.store.get_model(id).await?;
    let endpoint = state
        .store
        .update_model_endpoint(
            endpoint_id,
            &body.name,
            &body.api_base,
            body.api_key.as_deref(),
            body.priority,
            body.weight,
            body.enabled,
        )
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            "admin",
            "update_model_endpoint",
            "model_endpoint",
            &endpoint.id.to_string(),
            serde_json::json!({
                "model": model.model_name,
                "name": endpoint.name,
                "api_base": endpoint.api_base,
                "enabled": endpoint.enabled,
            }),
        )
        .await?;
    Ok(Json(endpoint))
}

#[utoipa::path(
    delete, path = "/api/v1/models/{id}/endpoints/{endpoint_id}", tag = "models",
    params(
        ("id" = Uuid, Path, description = "Model id"),
        ("endpoint_id" = Uuid, Path, description = "Endpoint id")
    ),
    responses((status = 204), (status = 404))
)]
async fn delete_model_endpoint(
    State(state): State<AdminState>,
    Path((id, endpoint_id)): Path<(Uuid, Uuid)>,
) -> Result<StatusCode> {
    let model = state.store.get_model(id).await?;
    state.store.delete_model_endpoint(endpoint_id).await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            "admin",
            "delete_model_endpoint",
            "model_endpoint",
            &endpoint_id.to_string(),
            serde_json::json!({ "model": model.model_name }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    delete, path = "/api/v1/models/{id}", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 204), (status = 404))
)]
async fn delete_model(State(state): State<AdminState>, Path(id): Path<Uuid>) -> Result<StatusCode> {
    let model = state.store.get_model(id).await?;
    state.store.delete_model(id).await?;
    let _ = state.redis.delete_resolved_model(&model.model_name).await;
    let _ = state
        .redis
        .publish_invalidation(&format!("model:{}", model.model_name))
        .await;
    state
        .store
        .record_audit(
            "admin",
            "delete_model",
            "model",
            &id.to_string(),
            serde_json::json!({ "model_name": model.model_name }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- mcp servers ---------------------------------------------------------

#[utoipa::path(
    post, path = "/api/v1/mcp-servers", tag = "mcp",
    request_body = CreateMcpServer,
    responses((status = 200, body = McpServer))
)]
async fn create_mcp_server(
    State(state): State<AdminState>,
    Json(body): Json<CreateMcpServer>,
) -> Result<Json<McpServer>> {
    state.ssrf.validate(&body.upstream_url)?;
    let server = state
        .store
        .create_mcp_server(&body.name, &body.upstream_url, body.auth_header.as_deref())
        .await?;
    sync_mcp_server(&state, &server).await?;
    state
        .store
        .record_audit(
            "admin",
            "create_mcp_server",
            "mcp_server",
            &server.id.to_string(),
            serde_json::json!({ "name": server.name, "upstream_url": server.upstream_url }),
        )
        .await?;
    Ok(Json(server))
}

#[utoipa::path(
    get, path = "/api/v1/mcp-servers", tag = "mcp",
    responses((status = 200, body = [McpServer]))
)]
async fn list_mcp_servers(State(state): State<AdminState>) -> Result<Json<Vec<McpServer>>> {
    Ok(Json(state.store.list_mcp_servers().await?))
}

#[utoipa::path(
    get, path = "/api/v1/mcp-servers/{id}", tag = "mcp",
    params(("id" = Uuid, Path, description = "MCP server id")),
    responses((status = 200, body = McpServer), (status = 404))
)]
async fn get_mcp_server(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServer>> {
    Ok(Json(state.store.get_mcp_server(id).await?))
}

#[utoipa::path(
    put, path = "/api/v1/mcp-servers/{id}", tag = "mcp",
    params(("id" = Uuid, Path, description = "MCP server id")),
    request_body = UpdateMcpServer,
    responses((status = 200, body = McpServer))
)]
async fn update_mcp_server(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateMcpServer>,
) -> Result<Json<McpServer>> {
    state.ssrf.validate(&body.upstream_url)?;
    let existing = state.store.get_mcp_server(id).await?;
    let auth = body
        .auth_header
        .as_deref()
        .or(existing.auth_header.as_deref());
    let server = state
        .store
        .update_mcp_server(
            id,
            &body.upstream_url,
            auth,
            body.enabled.unwrap_or(existing.enabled),
        )
        .await?;
    sync_mcp_server(&state, &server).await?;
    state
        .store
        .record_audit(
            "admin",
            "update_mcp_server",
            "mcp_server",
            &id.to_string(),
            serde_json::json!({ "name": server.name }),
        )
        .await?;
    Ok(Json(server))
}

#[utoipa::path(
    delete, path = "/api/v1/mcp-servers/{id}", tag = "mcp",
    params(("id" = Uuid, Path, description = "MCP server id")),
    responses((status = 204), (status = 404))
)]
async fn delete_mcp_server(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<StatusCode> {
    let server = state.store.get_mcp_server(id).await?;
    state.store.delete_mcp_server(id).await?;
    let _ = state.redis.delete_resolved_mcp_server(&server.name).await;
    let _ = state
        .redis
        .publish_invalidation(&format!("mcp:{}", server.name))
        .await;
    state
        .store
        .record_audit(
            "admin",
            "delete_mcp_server",
            "mcp_server",
            &id.to_string(),
            serde_json::json!({ "name": server.name }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    get, path = "/api/v1/audit", tag = "audit",
    params(AuditQuery),
    responses((status = 200, body = [AuditEntryView]))
)]
async fn get_audit(
    State(state): State<AdminState>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Vec<AuditEntry>>> {
    Ok(Json(state.store.list_audit(q.limit.unwrap_or(100)).await?))
}

#[utoipa::path(
    get, path = "/api/v1/capacity", tag = "capacity",
    responses((status = 200, body = CapacityView))
)]
async fn get_capacity(State(state): State<AdminState>) -> Json<CapacityView> {
    use obleth_fairshare::CapacityProvider;
    Json(CapacityView {
        max_in_flight: state.capacity.max_in_flight(),
    })
}

#[utoipa::path(
    put, path = "/api/v1/capacity", tag = "capacity",
    request_body = SetCapacity,
    responses((status = 200, body = CapacityView))
)]
async fn set_capacity(
    State(state): State<AdminState>,
    Json(body): Json<SetCapacity>,
) -> Result<Json<CapacityView>> {
    state.capacity.set(body.max_in_flight);
    state
        .store
        .record_audit(
            "admin",
            "set_capacity",
            "gateway",
            "global",
            serde_json::json!({ "max_in_flight": body.max_in_flight }),
        )
        .await?;
    Ok(Json(CapacityView {
        max_in_flight: body.max_in_flight,
    }))
}

// ---- sync helpers --------------------------------------------------------

async fn push_key(state: &AdminState, hash: &str, resolved: &ResolvedKey) -> Result<()> {
    state.redis.put_resolved_key(hash, resolved).await?;
    state.redis.publish_invalidation(hash).await?;
    Ok(())
}

async fn sync_model(state: &AdminState, model: &ModelRoute) -> Result<()> {
    // Endpoints carry the per-cluster wire targets and health; the data plane
    // prefers them over the legacy single api_base/api_key when present.
    let endpoints = state
        .store
        .resolved_endpoints_for(model.id)
        .await
        .unwrap_or_default();
    let resolved = ResolvedModel {
        model_name: model.model_name.clone(),
        upstream_model: model.upstream_model.clone(),
        api_base: model.api_base.clone(),
        api_key: model.api_key.clone(),
        model_type: model.model_type.clone(),
        admission_weight: model.admission_weight,
        max_in_flight: model.max_in_flight.and_then(|n| usize::try_from(n).ok()),
        enabled: model.enabled,
        cache_enabled: model.cache_enabled,
        cache_ttl_secs: model.cache_ttl_secs,
        input_cost_per_token: model.input_cost_per_token,
        output_cost_per_token: model.output_cost_per_token,
        cost_per_image: model.cost_per_image,
        cost_per_audio_second: model.cost_per_audio_second,
        cost_per_character: model.cost_per_character,
        context_window: model.context_window,
        supports_function_calling: model.supports_function_calling,
        supports_system_messages: model.supports_system_messages,
        supports_response_schema: model.supports_response_schema,
        supports_tool_choice: model.supports_tool_choice,
        supports_vision: model.supports_vision,
        tags: model.tags.clone(),
        boons: model.boons.clone(),
        request_timeout_secs: model.request_timeout_secs,
        max_retries: model.max_retries,
        retry_backoff_ms: model.retry_backoff_ms,
        endpoint_selection_mode: model.endpoint_selection_mode.clone(),
        endpoints,
    };
    if model.enabled {
        state
            .redis
            .put_resolved_model(&model.model_name, &resolved)
            .await?;
    } else {
        let _ = state.redis.delete_resolved_model(&model.model_name).await;
    }
    state
        .redis
        .publish_invalidation(&format!("model:{}", model.model_name))
        .await?;
    Ok(())
}

async fn sync_mcp_server(state: &AdminState, server: &McpServer) -> Result<()> {
    let resolved = ResolvedMcpServer {
        name: server.name.clone(),
        upstream_url: server.upstream_url.clone(),
        auth_header: server.auth_header.clone(),
        enabled: server.enabled,
    };
    if server.enabled {
        state
            .redis
            .put_resolved_mcp_server(&server.name, &resolved)
            .await?;
    } else {
        let _ = state.redis.delete_resolved_mcp_server(&server.name).await;
    }
    state
        .redis
        .publish_invalidation(&format!("mcp:{}", server.name))
        .await?;
    Ok(())
}

/// Re-push every key of a tenant after a weight/quota change.
async fn sync_tenant_keys(state: &AdminState, tenant_id: Uuid) -> Result<()> {
    for (hash, resolved) in state.store.resolved_keys_for_tenant(tenant_id).await? {
        push_key(state, &hash, &resolved).await?;
    }
    Ok(())
}
