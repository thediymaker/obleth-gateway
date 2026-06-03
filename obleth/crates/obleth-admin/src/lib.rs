//! Management API: the versioned, authenticated control surface.
//!
//! Mounted on a separate admin port from the data plane. Every config write
//! follows a single path — **Postgres (durable) -> Redis (cache) -> pub/sub
//! invalidate** — and is recorded in the audit log. Usage/cost reads hit
//! ClickHouse. The Next.js dashboard and any CLI/Terraform consume these exact
//! endpoints.

mod error;
pub mod model_health;
mod openapi;
pub mod ssrf;
mod usage;

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header::AUTHORIZATION, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use obleth_config::{
    hash_api_key, ApiKey, FairshareGroup, McpServer, ModelRoute, ResolvedKey, ResolvedMcpServer,
    ResolvedModel, Tenant,
};
use obleth_fairshare::{FairShare, StaticCapacity, Stats};
use obleth_redis::RedisStore;
use obleth_store::{AuditEntry, Store};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use utoipa::ToSchema;
use uuid::Uuid;

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
    /// SSRF allowlist policy applied to admin-supplied upstream URLs.
    pub ssrf: ssrf::SsrfPolicy,
}

/// Build the `/api/v1` router. `/health` and the OpenAPI doc are public; every
/// other route requires a bearer admin token.
pub fn router(state: AdminState) -> Router {
    let public = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/openapi.json", get(openapi_json));

    let protected = Router::new()
        .route("/api/v1/tenants", post(create_tenant).get(list_tenants))
        .route("/api/v1/tenants/:id", get(get_tenant))
        .route("/api/v1/tenants/:id/weight", patch(patch_weight))
        .route("/api/v1/tenants/:id/quota", put(put_quota))
        .route("/api/v1/tenants/:id/keys", post(create_key))
        .route("/api/v1/keys", get(list_keys))
        .route("/api/v1/keys/:id", delete(delete_key))
        .route("/api/v1/keys/:id/disabled", put(set_key_disabled))
        .route("/api/v1/usage", get(get_usage))
        .route("/api/v1/usage/keys", get(get_usage_keys))
        .route("/api/v1/usage/models", get(get_usage_models))
        .route("/api/v1/usage/series", get(get_usage_series))
        .route(
            "/api/v1/usage/series/tenants",
            get(get_usage_series_tenants),
        )
        .route("/api/v1/usage/cache", get(get_cache_stats))
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
        .route("/api/v1/models/:id/cache", put(set_model_cache))
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
    pub input_cost_per_token: Option<f64>,
    pub output_cost_per_token: Option<f64>,
    pub context_window: Option<i64>,
    pub admission_weight: Option<i64>,
    pub max_in_flight: Option<i64>,
    pub supports_function_calling: Option<bool>,
    pub supports_system_messages: Option<bool>,
    pub supports_response_schema: Option<bool>,
    pub supports_tool_choice: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateModel {
    pub description: Option<String>,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub input_cost_per_token: Option<f64>,
    pub output_cost_per_token: Option<f64>,
    pub context_window: Option<i64>,
    pub admission_weight: Option<i64>,
    pub max_in_flight: Option<i64>,
    pub supports_function_calling: Option<bool>,
    pub supports_system_messages: Option<bool>,
    pub supports_response_schema: Option<bool>,
    pub supports_tool_choice: Option<bool>,
    pub enabled: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelCache {
    pub cache_enabled: bool,
    pub cache_ttl_secs: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelCapacity {
    pub max_in_flight: Option<i64>,
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

#[derive(Debug, Deserialize)]
pub struct ListKeysQuery {
    pub tenant_id: Option<Uuid>,
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

async fn get_tenant(State(state): State<AdminState>, Path(id): Path<Uuid>) -> Result<Json<Tenant>> {
    Ok(Json(state.store.get_tenant(id).await?))
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

async fn list_keys(
    State(state): State<AdminState>,
    Query(q): Query<ListKeysQuery>,
) -> Result<Json<Vec<ApiKey>>> {
    Ok(Json(state.store.list_keys(q.tenant_id).await?))
}

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

#[utoipa::path(get, path = "/api/v1/usage", tag = "usage", responses((status = 200, body = [usage::UsageAgg])))]
async fn get_usage(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<Vec<usage::UsageAgg>>> {
    Ok(Json(usage::query_usage(&state.clickhouse, q).await?))
}

async fn get_usage_keys(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<Vec<usage::UsageKeyAgg>>> {
    Ok(Json(usage::query_usage_by_key(&state.clickhouse, q).await?))
}

async fn get_usage_models(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<Vec<usage::UsageModelAgg>>> {
    Ok(Json(
        usage::query_usage_by_model(&state.clickhouse, q).await?,
    ))
}

async fn get_usage_series(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageSeriesQuery>,
) -> Result<Json<Vec<usage::UsageTimePoint>>> {
    Ok(Json(usage::query_usage_series(&state.clickhouse, q).await?))
}

async fn get_usage_series_tenants(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageSeriesQuery>,
) -> Result<Json<Vec<usage::TenantUsageTimePoint>>> {
    Ok(Json(
        usage::query_usage_series_by_tenant(&state.clickhouse, q).await?,
    ))
}

async fn get_cache_stats(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<usage::CacheStats>> {
    Ok(Json(usage::query_cache_stats(&state.clickhouse, q).await?))
}

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

async fn get_stats(State(state): State<AdminState>) -> Json<LiveStats> {
    use obleth_fairshare::CapacityProvider;
    use std::sync::atomic::Ordering;
    Json(LiveStats {
        in_flight: state.fairshare_stats.in_flight.load(Ordering::Relaxed),
        queued: state.fairshare_stats.queued.load(Ordering::Relaxed),
        max_in_flight: state.capacity.max_in_flight(),
    })
}

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

async fn list_fairshare_groups(
    State(state): State<AdminState>,
) -> Result<Json<Vec<FairshareGroup>>> {
    Ok(Json(state.store.list_fairshare_groups().await?))
}

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
            body.input_cost_per_token.unwrap_or(0.0),
            body.output_cost_per_token.unwrap_or(0.0),
            body.context_window.unwrap_or(8192),
            body.admission_weight.unwrap_or(100),
            body.max_in_flight,
            body.supports_function_calling.unwrap_or(false),
            body.supports_system_messages.unwrap_or(true),
            body.supports_response_schema.unwrap_or(false),
            body.supports_tool_choice.unwrap_or(false),
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

async fn list_models(State(state): State<AdminState>) -> Result<Json<Vec<ModelRoute>>> {
    Ok(Json(state.store.list_models().await?))
}

async fn get_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ModelRoute>> {
    Ok(Json(state.store.get_model(id).await?))
}

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
            body.input_cost_per_token
                .unwrap_or(existing.input_cost_per_token),
            body.output_cost_per_token
                .unwrap_or(existing.output_cost_per_token),
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
            body.enabled.unwrap_or(existing.enabled),
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

async fn list_mcp_servers(State(state): State<AdminState>) -> Result<Json<Vec<McpServer>>> {
    Ok(Json(state.store.list_mcp_servers().await?))
}

async fn get_mcp_server(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServer>> {
    Ok(Json(state.store.get_mcp_server(id).await?))
}

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

async fn get_audit(
    State(state): State<AdminState>,
    Query(q): Query<AuditQuery>,
) -> Result<Json<Vec<AuditEntry>>> {
    Ok(Json(state.store.list_audit(q.limit.unwrap_or(100)).await?))
}

#[derive(Debug, Deserialize)]
pub struct AuditQuery {
    pub limit: Option<i64>,
}

async fn get_capacity(State(state): State<AdminState>) -> Json<CapacityView> {
    use obleth_fairshare::CapacityProvider;
    Json(CapacityView {
        max_in_flight: state.capacity.max_in_flight(),
    })
}

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
    let resolved = ResolvedModel {
        model_name: model.model_name.clone(),
        upstream_model: model.upstream_model.clone(),
        api_base: model.api_base.clone(),
        api_key: model.api_key.clone(),
        admission_weight: model.admission_weight,
        max_in_flight: model.max_in_flight.and_then(|n| usize::try_from(n).ok()),
        enabled: model.enabled,
        cache_enabled: model.cache_enabled,
        cache_ttl_secs: model.cache_ttl_secs,
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
