//! Management API: the versioned, authenticated control surface.
//!
//! Mounted on a separate admin port from the data plane. Every config write
//! follows a single path — **Postgres (durable) -> Redis (cache) -> pub/sub
//! invalidate** — and is recorded in the audit log. Usage/cost reads hit
//! ClickHouse. The Next.js dashboard and any CLI/Terraform consume these exact
//! endpoints.

pub mod alerts;
pub mod autotune;
mod backup;
pub mod energy_probe;
mod error;
pub mod model_health;
mod openapi;
pub mod recipes;
pub mod slurm_resources;
pub mod slurm_settings;
pub mod ssrf;
mod usage;
pub mod usage_retention;

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use obleth_config::{
    hash_api_key, ApiKey, FairshareGroup, ManagedModelSpec, McpServer, ModelEndpoint, ModelReplica,
    ModelRoute, ResolvedKey, ResolvedMcpServer, ResolvedModel, Tenant,
};
use obleth_config::{
    AlertSettings, AutoRouterSettings, BoonSettings, EmailSettings, StructuredOutputBoonSettings,
    ToolLoopSettings, VisionBoonSettings, STRUCTURED_OUTPUT_MAX_REPAIR_ATTEMPTS,
    TOOL_LOOP_MAX_TURNS,
};
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
const AUDIT_ACTOR_HEADER: &str = "x-obleth-audit-actor";

pub(crate) fn audit_actor(headers: &HeaderMap) -> String {
    headers
        .get(AUDIT_ACTOR_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.chars().take(256).collect())
        .unwrap_or_else(|| "admin".to_string())
}

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
    /// Direct in-process moka cache invalidation. Set by the binary that owns
    /// the key cache; None when admin and proxy run in separate processes.
    pub local_cache_tx: Option<tokio::sync::mpsc::UnboundedSender<String>>,
}

/// Build the `/api/v1` router. `/health` and the OpenAPI doc are public; every
/// other route requires a bearer admin token.
pub fn router(state: AdminState) -> Router {
    let public = Router::new()
        .route("/api/v1/health", get(health))
        .route("/api/v1/version", get(get_version))
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
        .route(
            "/api/v1/tenants/:id/guardrails",
            patch(patch_tenant_guardrails),
        )
        .route(
            "/api/v1/tenants/:id/compression",
            patch(patch_tenant_compression),
        )
        .route("/api/v1/tenants/:id/weight", patch(patch_weight))
        .route("/api/v1/tenants/:id/quota", put(put_quota))
        .route("/api/v1/tenants/:id/keys", post(create_key))
        .route(
            "/api/v1/tenants/:id/tracing",
            put(set_tenant_tracing_handler),
        )
        .route("/api/v1/keys", get(list_keys))
        .route("/api/v1/keys/:id", put(update_key).delete(delete_key))
        .route("/api/v1/keys/:id/disabled", put(set_key_disabled))
        .route("/api/v1/keys/:id/tracing", put(set_key_tracing_handler))
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
        .route("/api/v1/usage/series/models", get(get_usage_series_models))
        .route("/api/v1/usage/breakdown", get(get_usage_breakdown))
        .route("/api/v1/usage/cache", get(get_cache_stats))
        .route("/api/v1/usage/logs", get(get_usage_logs))
        .route(
            "/api/v1/usage/logs/:request_id/spans",
            get(get_request_spans),
        )
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
            "/api/v1/recipes",
            get(recipes::list_recipes).post(recipes::create_recipe),
        )
        .route(
            "/api/v1/recipes/:id",
            put(recipes::update_recipe).delete(recipes::delete_recipe),
        )
        .route("/api/v1/managed", get(list_managed_models))
        .route(
            "/api/v1/models/:id/managed",
            get(get_managed_model)
                .put(put_managed_model)
                .delete(delete_managed_model),
        )
        .route(
            "/api/v1/models/:id/managed/provision-error",
            patch(set_provision_error),
        )
        .route("/api/v1/replicas", get(list_all_replicas))
        .route(
            "/api/v1/replicas/:id",
            patch(patch_replica).delete(delete_replica),
        )
        .route("/api/v1/replicas/:id/restart", post(restart_replica))
        .route(
            "/api/v1/models/:id/replicas",
            get(list_replicas).post(create_replica),
        )
        .route(
            "/api/v1/models/:id/replicas/clear-lost",
            post(clear_lost_replicas),
        )
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
            "/api/v1/settings/energy",
            get(get_energy_settings).put(put_energy_settings),
        )
        .route("/api/v1/settings/energy/test", post(test_energy_query))
        .route("/api/v1/settings/compressor", get(get_compressor_status))
        .route(
            "/api/v1/settings/charo",
            get(get_charo_settings).put(put_charo_settings),
        )
        .route(
            "/api/v1/settings/usage-retention",
            get(get_usage_retention).put(put_usage_retention),
        )
        .route(
            "/api/v1/settings/slurm",
            get(slurm_settings::get_slurm_settings).put(slurm_settings::put_slurm_settings),
        )
        .route(
            "/api/v1/settings/slurm/test",
            post(slurm_settings::test_slurm_settings),
        )
        .route(
            "/api/v1/settings/slurm/resolved",
            get(slurm_settings::get_slurm_settings_resolved),
        )
        .route(
            "/api/v1/slurm/resources",
            get(slurm_resources::get_slurm_resources),
        )
        .route("/api/v1/backup/export", get(backup::export_backup))
        .route(
            "/api/v1/system/control-plane-key",
            get(get_control_plane_key),
        )
        .route(
            "/api/v1/backup/restore",
            // Backups with large key fleets exceed axum's 2 MB default body
            // limit; raise it for this route only.
            post(backup::restore_backup)
                .layer(axum::extract::DefaultBodyLimit::max(64 * 1024 * 1024)),
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
pub struct SetTenantGuardrails {
    /// `null` clears the guardrails policy (no scanning for this tenant).
    pub policy: Option<obleth_config::GuardrailsPolicy>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantCompression {
    /// `null` clears the compression policy (tenant follows the global default).
    pub policy: Option<obleth_config::CompressionPolicy>,
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
    #[serde(default)]
    pub description: String,
    /// Cumulative token ceiling for the current key term. `null` clears the cap.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Cumulative USD-cost ceiling for the current key term. `null` clears the cap.
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    /// Reset period: `lifetime`, `monthly`, or `term`. `null` = lifetime.
    #[serde(default)]
    pub budget_period: Option<String>,
    /// When the current key term began.
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateKey {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Cumulative token ceiling for the current key term. `null` clears the cap.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Cumulative USD-cost ceiling for the current key term. `null` clears the cap.
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    /// Reset period: `lifetime`, `monthly`, or `term`. `null` = lifetime.
    #[serde(default)]
    pub budget_period: Option<String>,
    /// When the current key term began.
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
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

#[derive(Debug, Deserialize)]
struct SetKeyTracing {
    tracing_enabled: bool,
}

fn normalize_budget_fields(
    budget_tokens: Option<i64>,
    budget_cost_usd: Option<f64>,
    budget_period: Option<&str>,
    budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    token_field: &str,
    cost_field: &str,
    period_field: &str,
) -> Result<(Option<String>, Option<chrono::DateTime<chrono::Utc>>)> {
    let period = match budget_period.map(str::trim) {
        None | Some("") => None,
        Some(p) => {
            let p = p.to_lowercase();
            if !matches!(p.as_str(), "lifetime" | "monthly" | "term") {
                return Err(AdminError::BadRequest(format!(
                    "{period_field} must be one of: lifetime, monthly, term",
                )));
            }
            Some(p)
        }
    };
    if let Some(tokens) = budget_tokens {
        if tokens < 0 {
            return Err(AdminError::BadRequest(format!(
                "{token_field} must be non-negative",
            )));
        }
    }
    if let Some(cost) = budget_cost_usd {
        if cost < 0.0 || !cost.is_finite() {
            return Err(AdminError::BadRequest(format!(
                "{cost_field} must be a non-negative number",
            )));
        }
    }
    let started_at = budget_started_at
        .or_else(|| (budget_tokens.is_some() || budget_cost_usd.is_some()).then(chrono::Utc::now));
    Ok((period, started_at))
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
    /// Live in-flight request count per model name.
    #[serde(default)]
    pub model_in_flight: std::collections::HashMap<String, usize>,
    /// Live queued request count per model name.
    #[serde(default)]
    pub model_queued: std::collections::HashMap<String, usize>,
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
    /// Registered MCP servers whose tools this model may use (gateway tool loop).
    #[serde(default)]
    pub tool_servers: Option<Vec<String>>,
    /// Energy accounting: concurrent sequences that saturate one node.
    /// 0 disables energy accounting for this model.
    #[serde(default)]
    pub energy_slots_per_node: Option<i64>,
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
    /// Registered MCP servers whose tools this model may use (gateway tool loop).
    #[serde(default)]
    pub tool_servers: Option<Vec<String>>,
    /// Energy accounting: concurrent sequences that saturate one node.
    /// 0 disables energy accounting for this model.
    #[serde(default)]
    pub energy_slots_per_node: Option<i64>,
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
    #[serde(default)]
    pub debug_diagnostics: bool,
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
pub(crate) struct PutManagedModel {
    #[serde(default = "default_true")]
    enabled: bool,
    partition: String,
    #[serde(default)]
    gres: String,
    #[serde(default = "one")]
    nodes: i64,
    constraints: Option<String>,
    exclude: Option<String>,
    account: Option<String>,
    qos: Option<String>,
    time_limit: Option<String>,
    #[serde(default)]
    cpus_per_task: Option<i64>,
    #[serde(default)]
    mem: Option<String>,
    #[serde(default)]
    image: String,
    #[serde(default)]
    preamble: String,
    #[serde(default)]
    log_output_dir: String,
    #[serde(default)]
    launch_command: String,
    #[serde(default)]
    script_body: String,
    serving_port: i64,
    #[serde(default = "default_health_path")]
    health_path: String,
    #[serde(default = "two")]
    target_replicas: i64,
    #[serde(default = "one")]
    min_replicas: i64,
    #[serde(default)]
    max_job_failures: i64,
    #[serde(default)]
    launcher_spec: Option<serde_json::Value>,
}
fn one() -> i64 {
    1
}
fn two() -> i64 {
    2
}
fn default_health_path() -> String {
    "/health".into()
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CreateReplica {
    slurm_job_id: String,
    #[serde(default)]
    port_base: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct PatchReplica {
    state: Option<String>,
    message: Option<String>,
    nodes: Option<String>,
    #[schema(value_type = Option<String>)]
    endpoint_id: Option<Uuid>,
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

/// Build/version identity of the running gateway binary.
#[derive(Debug, Serialize, ToSchema)]
pub struct VersionInfo {
    /// Crate version, in lockstep with the release tag (`vX.Y.Z`).
    pub version: String,
    /// Git commit the binary was built from; absent for local builds.
    pub git_sha: Option<String>,
    /// RFC 3339 build timestamp; absent for local builds.
    pub built_at: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/version", tag = "meta",
    responses((status = 200, body = VersionInfo))
)]
async fn get_version() -> Json<VersionInfo> {
    Json(VersionInfo {
        version: env!("CARGO_PKG_VERSION").to_string(),
        // Docker sets these to "" when the build-args are omitted (local
        // builds); treat empty as absent.
        git_sha: option_env!("OBLETH_BUILD_SHA")
            .filter(|s| !s.is_empty())
            .map(str::to_string),
        built_at: option_env!("OBLETH_BUILD_TIMESTAMP")
            .filter(|s| !s.is_empty())
            .map(str::to_string),
    })
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
    headers: HeaderMap,
    Json(body): Json<CreateTenant>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .create_tenant(
            &body.name,
            body.weight.unwrap_or(100),
            body.tokens_per_minute.unwrap_or(0),
            body.max_in_flight,
            body.fairshare_group.as_deref(),
        )
        .await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
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
    let mut tenants = state.store.list_tenants().await?;
    // The reserved control-plane identity (Charo) is system-owned: hide it from
    // the management surface so it can't be edited/deleted by mistake.
    tenants.retain(|t| t.id != Store::CONTROL_PLANE_TENANT_ID);
    Ok(Json(tenants))
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
    Json(body): Json<SetTenantBudget>,
) -> Result<Json<Tenant>> {
    let (period, started_at) = normalize_budget_fields(
        body.budget_tokens,
        body.budget_cost_usd,
        body.budget_period.as_deref(),
        body.budget_started_at,
        "budget_tokens",
        "budget_cost_usd",
        "budget_period",
    )?;
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
            "set_tenant_allowlist",
            "tenant",
            &id.to_string(),
            serde_json::json!({ "allowed_models": tenant.allowed_models }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    patch, path = "/api/v1/tenants/{id}/guardrails", tag = "tenants",
    request_body = SetTenantGuardrails,
    responses((status = 200, body = Tenant))
)]
async fn patch_tenant_guardrails(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetTenantGuardrails>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .update_tenant_guardrails_policy(id, body.policy)
        .await?;
    // Guardrails gate data-plane behaviour; refresh the cached keys.
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "set_tenant_guardrails",
            "tenant",
            &id.to_string(),
            serde_json::json!({ "guardrails_policy": tenant.guardrails_policy }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    patch, path = "/api/v1/tenants/{id}/compression", tag = "tenants",
    request_body = SetTenantCompression,
    responses((status = 200, body = Tenant))
)]
async fn patch_tenant_compression(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetTenantCompression>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .update_tenant_compression_policy(id, body.policy)
        .await?;
    // Compression gating reads the cached key; refresh it.
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "set_tenant_compression",
            "tenant",
            &id.to_string(),
            serde_json::json!({ "compression_policy": tenant.compression_policy }),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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

/// View of the persisted model-"boons" settings, flattened per boon
/// (vision, structured output, tool loop).
#[derive(Debug, Serialize, ToSchema)]
pub struct BoonSettingsView {
    pub vision_enabled: bool,
    pub vision_fallback_model: Option<String>,
    pub vision_describe_prompt: String,
    pub vision_max_images: u32,
    pub vision_timeout_ms: u64,
    pub structured_output_enabled: bool,
    pub structured_output_fixer_model: Option<String>,
    pub structured_output_max_repair_attempts: u32,
    pub structured_output_timeout_ms: u64,
    pub tool_loop_enabled: bool,
    pub tool_loop_max_turns: u32,
    pub tool_loop_tool_timeout_ms: u64,
    pub tool_loop_nudge: String,
    pub compression_enabled: bool,
    pub compression_min_tokens: u32,
    pub compression_max_segments: u32,
    pub compression_original_ttl_secs: u64,
    pub compression_max_lossy_segments: u32,
    pub compression_code_compaction: bool,
    pub compression_dedup: bool,
    pub compression_compact_logs: bool,
    pub compression_allow_lossy: bool,
    pub compression_neural_keep_ratio: f32,
}

impl BoonSettingsView {
    fn from_settings(s: &BoonSettings) -> Self {
        BoonSettingsView {
            vision_enabled: s.vision.enabled,
            vision_fallback_model: s.vision.fallback_model.clone(),
            vision_describe_prompt: s.vision.describe_prompt.clone(),
            vision_max_images: s.vision.max_images,
            vision_timeout_ms: s.vision.timeout_ms,
            structured_output_enabled: s.structured_output.enabled,
            structured_output_fixer_model: s.structured_output.fixer_model.clone(),
            structured_output_max_repair_attempts: s.structured_output.max_repair_attempts,
            structured_output_timeout_ms: s.structured_output.timeout_ms,
            tool_loop_enabled: s.tool_loop.enabled,
            tool_loop_max_turns: s.tool_loop.max_turns,
            tool_loop_tool_timeout_ms: s.tool_loop.tool_timeout_ms,
            tool_loop_nudge: s.tool_loop.nudge.clone(),
            compression_enabled: s.compression.enabled,
            compression_min_tokens: s.compression.min_tokens,
            compression_max_segments: s.compression.max_segments,
            compression_original_ttl_secs: s.compression.original_ttl_secs,
            compression_max_lossy_segments: s.compression.max_lossy_segments,
            compression_code_compaction: s.compression.code_compaction,
            compression_dedup: s.compression.dedup,
            compression_compact_logs: s.compression.compact_logs,
            compression_allow_lossy: s.compression.allow_lossy,
            compression_neural_keep_ratio: s.compression.neural_keep_ratio,
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
    #[serde(default)]
    pub structured_output_enabled: Option<bool>,
    /// `model_name` of the JSON-repair fixer model. Empty string clears it
    /// (repairs then re-prompt the request's own model).
    #[serde(default)]
    pub structured_output_fixer_model: Option<String>,
    #[serde(default)]
    pub structured_output_max_repair_attempts: Option<u32>,
    #[serde(default)]
    pub structured_output_timeout_ms: Option<u64>,
    #[serde(default)]
    pub tool_loop_enabled: Option<bool>,
    #[serde(default)]
    pub tool_loop_max_turns: Option<u32>,
    #[serde(default)]
    pub tool_loop_tool_timeout_ms: Option<u64>,
    /// System nudge injected with granted tools. Empty string resets it to the
    /// built-in default; omit the field to leave it unchanged.
    #[serde(default)]
    pub tool_loop_nudge: Option<String>,
    /// Enable or disable the compression boon globally. Omit to leave unchanged.
    #[serde(default)]
    pub compression_enabled: Option<bool>,
    /// Minimum heuristic token count for a segment to be considered for compaction.
    /// A value of `0` is a no-op and leaves the existing setting unchanged.
    #[serde(default)]
    pub compression_min_tokens: Option<u32>,
    /// Maximum number of segments that may be compacted per request.
    /// A value of `0` is a no-op and leaves the existing setting unchanged.
    #[serde(default)]
    pub compression_max_segments: Option<u32>,
    /// Redis TTL for stashed originals (secs). Omit/zero leaves unchanged.
    #[serde(default)]
    pub compression_original_ttl_secs: Option<u64>,
    /// Lossy segment cap. Omit/zero leaves unchanged.
    #[serde(default)]
    pub compression_max_lossy_segments: Option<u32>,
    /// Toggle conservative code compaction. Omit to leave unchanged.
    #[serde(default)]
    pub compression_code_compaction: Option<bool>,
    /// Global default for cross-turn dedup (tenant policy overrides). Omit to leave unchanged.
    #[serde(default)]
    pub compression_dedup: Option<bool>,
    /// Global default for log template-collapse (tenant policy overrides). Omit to leave unchanged.
    #[serde(default)]
    pub compression_compact_logs: Option<bool>,
    /// Global default for lossy text compaction (tenant policy overrides). Omit to leave unchanged.
    #[serde(default)]
    pub compression_allow_lossy: Option<bool>,
    /// Fraction of sentences the neural (compressor) prose pass keeps. Must be in
    /// `(0.0, 1.0]`; values outside that range (or omitted) leave it unchanged.
    #[serde(default)]
    pub compression_neural_keep_ratio: Option<f32>,
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
    headers: HeaderMap,
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

    let fixer_model = match body.structured_output_fixer_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.structured_output.fixer_model.clone(),
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
        structured_output: StructuredOutputBoonSettings {
            enabled: body
                .structured_output_enabled
                .unwrap_or(existing.structured_output.enabled),
            fixer_model,
            max_repair_attempts: body
                .structured_output_max_repair_attempts
                .map(|n| n.min(STRUCTURED_OUTPUT_MAX_REPAIR_ATTEMPTS))
                .unwrap_or(existing.structured_output.max_repair_attempts),
            timeout_ms: body
                .structured_output_timeout_ms
                .filter(|ms| *ms > 0)
                .unwrap_or(existing.structured_output.timeout_ms),
        },
        tool_loop: ToolLoopSettings {
            enabled: body.tool_loop_enabled.unwrap_or(existing.tool_loop.enabled),
            max_turns: body
                .tool_loop_max_turns
                .filter(|n| *n > 0)
                .map(|n| n.min(TOOL_LOOP_MAX_TURNS))
                .unwrap_or(existing.tool_loop.max_turns),
            tool_timeout_ms: body
                .tool_loop_tool_timeout_ms
                .filter(|ms| *ms > 0)
                .unwrap_or(existing.tool_loop.tool_timeout_ms),
            nudge: match body.tool_loop_nudge.as_deref().map(str::trim) {
                Some("") => obleth_config::default_tool_loop_nudge(),
                Some(n) => n.to_string(),
                None => existing.tool_loop.nudge.clone(),
            },
        },
        guardrails: existing.guardrails.clone(),
        compression: obleth_config::CompressionBoonSettings {
            enabled: body
                .compression_enabled
                .unwrap_or(existing.compression.enabled),
            min_tokens: body
                .compression_min_tokens
                .filter(|n| *n > 0)
                .unwrap_or(existing.compression.min_tokens),
            max_segments: body
                .compression_max_segments
                .filter(|n| *n > 0)
                .unwrap_or(existing.compression.max_segments),
            original_ttl_secs: body
                .compression_original_ttl_secs
                .filter(|n| *n > 0)
                .unwrap_or(existing.compression.original_ttl_secs),
            max_lossy_segments: body
                .compression_max_lossy_segments
                .filter(|n| *n > 0)
                .unwrap_or(existing.compression.max_lossy_segments),
            code_compaction: body
                .compression_code_compaction
                .unwrap_or(existing.compression.code_compaction),
            dedup: body.compression_dedup.unwrap_or(existing.compression.dedup),
            compact_logs: body
                .compression_compact_logs
                .unwrap_or(existing.compression.compact_logs),
            allow_lossy: body
                .compression_allow_lossy
                .unwrap_or(existing.compression.allow_lossy),
            neural_keep_ratio: body
                .compression_neural_keep_ratio
                .filter(|r| *r > 0.0 && *r <= 1.0)
                .unwrap_or(existing.compression.neural_keep_ratio),
        },
    };

    state.store.put_boon_settings(&settings).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "set_boon_settings",
            "settings",
            "boons",
            serde_json::json!({
                "vision_enabled": settings.vision.enabled,
                "vision_fallback_model": settings.vision.fallback_model,
                "vision_max_images": settings.vision.max_images,
                "vision_timeout_ms": settings.vision.timeout_ms,
                "structured_output_enabled": settings.structured_output.enabled,
                "structured_output_fixer_model": settings.structured_output.fixer_model,
                "structured_output_max_repair_attempts": settings.structured_output.max_repair_attempts,
                "structured_output_timeout_ms": settings.structured_output.timeout_ms,
                "tool_loop_enabled": settings.tool_loop.enabled,
                "tool_loop_max_turns": settings.tool_loop.max_turns,
                "tool_loop_tool_timeout_ms": settings.tool_loop.tool_timeout_ms,
                "tool_loop_nudge_len": settings.tool_loop.nudge.len(),
            }),
        )
        .await?;
    Ok(Json(BoonSettingsView::from_settings(&settings)))
}

/// View of the persisted energy-accounting settings.
#[derive(Debug, Serialize, ToSchema)]
pub struct EnergySettingsView {
    pub enabled: bool,
    pub prometheus_url: String,
    pub power_query: String,
    pub poll_interval_secs: u64,
    pub energy_cost_per_kwh: f64,
    pub carbon_g_per_kwh: f64,
    pub pue: f64,
}

impl EnergySettingsView {
    fn from_settings(s: &obleth_config::EnergySettings) -> Self {
        EnergySettingsView {
            enabled: s.enabled,
            prometheus_url: s.prometheus_url.clone(),
            power_query: s.power_query.clone(),
            poll_interval_secs: s.poll_interval_secs,
            energy_cost_per_kwh: s.energy_cost_per_kwh,
            carbon_g_per_kwh: s.carbon_g_per_kwh,
            pue: s.pue,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateEnergySettings {
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Base URL of the operator's Prometheus. Omit to leave unchanged.
    #[serde(default)]
    pub prometheus_url: Option<String>,
    /// PromQL returning one power series (watts) per node. Omit to leave unchanged.
    #[serde(default)]
    pub power_query: Option<String>,
    /// Poll cadence in seconds. Omit/zero leaves unchanged.
    #[serde(default)]
    pub poll_interval_secs: Option<u64>,
    /// Electricity rate (USD/kWh). Omit to leave unchanged.
    #[serde(default)]
    pub energy_cost_per_kwh: Option<f64>,
    /// Grid carbon intensity (gCO2/kWh). Omit to leave unchanged.
    #[serde(default)]
    pub carbon_g_per_kwh: Option<f64>,
    /// Facility overhead multiplier. Omit or non-positive leaves unchanged.
    #[serde(default)]
    pub pue: Option<f64>,
}

fn merge_energy_settings(
    existing: &obleth_config::EnergySettings,
    body: &UpdateEnergySettings,
) -> obleth_config::EnergySettings {
    obleth_config::EnergySettings {
        enabled: body.enabled.unwrap_or(existing.enabled),
        prometheus_url: body
            .prometheus_url
            .as_deref()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| existing.prometheus_url.clone()),
        power_query: body
            .power_query
            .as_deref()
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| existing.power_query.clone()),
        poll_interval_secs: body
            .poll_interval_secs
            .filter(|n| *n > 0)
            .unwrap_or(existing.poll_interval_secs),
        energy_cost_per_kwh: body
            .energy_cost_per_kwh
            .filter(|v| *v >= 0.0)
            .unwrap_or(existing.energy_cost_per_kwh),
        carbon_g_per_kwh: body
            .carbon_g_per_kwh
            .filter(|v| *v >= 0.0)
            .unwrap_or(existing.carbon_g_per_kwh),
        pue: body.pue.filter(|v| *v > 0.0).unwrap_or(existing.pue),
    }
}

#[utoipa::path(
    get, path = "/api/v1/settings/energy", tag = "settings",
    responses((status = 200, body = EnergySettingsView))
)]
async fn get_energy_settings(State(state): State<AdminState>) -> Result<Json<EnergySettingsView>> {
    let settings = state.store.get_energy_settings().await?.unwrap_or_default();
    Ok(Json(EnergySettingsView::from_settings(&settings)))
}

#[utoipa::path(
    put, path = "/api/v1/settings/energy", tag = "settings",
    request_body = UpdateEnergySettings,
    responses((status = 200, body = EnergySettingsView))
)]
async fn put_energy_settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<UpdateEnergySettings>,
) -> Result<Json<EnergySettingsView>> {
    let existing = state.store.get_energy_settings().await?.unwrap_or_default();
    let settings = merge_energy_settings(&existing, &body);
    state.store.put_energy_settings(&settings).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "set_energy_settings",
            "settings",
            "energy",
            serde_json::json!({
                "enabled": settings.enabled,
                "prometheus_url": settings.prometheus_url,
                "power_query": settings.power_query,
                "poll_interval_secs": settings.poll_interval_secs,
                "energy_cost_per_kwh": settings.energy_cost_per_kwh,
                "carbon_g_per_kwh": settings.carbon_g_per_kwh,
                "pue": settings.pue,
            }),
        )
        .await?;
    Ok(Json(EnergySettingsView::from_settings(&settings)))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct TestEnergyQuery {
    pub prometheus_url: String,
    pub power_query: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EnergyTestResult {
    pub cluster_watts: f64,
    pub node_count: u64,
}

#[utoipa::path(
    post, path = "/api/v1/settings/energy/test", tag = "settings",
    request_body = TestEnergyQuery,
    responses((status = 200, body = EnergyTestResult))
)]
async fn test_energy_query(
    State(state): State<AdminState>,
    Json(body): Json<TestEnergyQuery>,
) -> Result<Json<EnergyTestResult>> {
    state.ssrf.validate(&body.prometheus_url)?;
    let q = body.power_query.trim();
    let base = body.prometheus_url.trim();
    let http = reqwest::Client::new();
    let watts = crate::energy_probe::instant_query(&http, base, &format!("sum({q})"))
        .await
        .map_err(AdminError::BadRequest)?;
    let nodes = crate::energy_probe::instant_query(&http, base, &format!("count({q})"))
        .await
        .map_err(AdminError::BadRequest)?;
    Ok(Json(EnergyTestResult {
        cluster_watts: watts,
        node_count: nodes.max(0.0) as u64,
    }))
}

/// Whether the Charo control-plane assistant is shown in the dashboard.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CharoSettingsView {
    pub enabled: bool,
}

/// Live status of the optional neural compression sidecar. Its URL is env-only
/// operator config (`OBLETH_COMPRESSOR_URL`), not stored in the DB, so this is a
/// live on-demand probe of the sidecar's `/health` — mirroring how the Slurm tab
/// surfaces provisioner health. Fail-soft: any error yields `reachable=false`
/// with the reason, never an error response.
#[derive(Debug, Serialize, ToSchema)]
pub struct CompressorStatusView {
    /// True when `OBLETH_COMPRESSOR_URL` is set (the feature is wired at all).
    pub configured: bool,
    /// The configured sidecar base URL (empty when unconfigured).
    pub url: String,
    /// True when the sidecar answered `/health` with status `"ok"`.
    pub reachable: bool,
    /// Model name the sidecar reports (e.g. `"kompress-v2-base"`).
    pub model: Option<String>,
    /// Model commit revision the sidecar reports.
    pub revision: Option<String>,
    /// Human-readable reason when the sidecar is configured but not reachable.
    pub error: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/settings/compressor", tag = "settings",
    responses((status = 200, body = CompressorStatusView))
)]
async fn get_compressor_status() -> Result<Json<CompressorStatusView>> {
    let url = std::env::var("OBLETH_COMPRESSOR_URL")
        .unwrap_or_default()
        .trim()
        .trim_end_matches('/')
        .to_string();

    if url.is_empty() {
        return Ok(Json(CompressorStatusView {
            configured: false,
            url: String::new(),
            reachable: false,
            model: None,
            revision: None,
            error: None,
        }));
    }

    // Give the settings probe a little more room than the 800ms request-path
    // timeout — a cold sidecar can be slow to answer its first /health.
    let timeout_ms = std::env::var("OBLETH_COMPRESSOR_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|ms| *ms > 0)
        .unwrap_or(2000)
        .max(2000);

    let unreachable = |url: String, error: String| {
        Json(CompressorStatusView {
            configured: true,
            url,
            reachable: false,
            model: None,
            revision: None,
            error: Some(error),
        })
    };

    let client = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(timeout_ms))
        .build()
    {
        Ok(c) => c,
        Err(e) => return Ok(unreachable(url, e.to_string())),
    };

    match client.get(format!("{url}/health")).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(v) => {
                let ok = v.get("status").and_then(|s| s.as_str()) == Some("ok");
                Ok(Json(CompressorStatusView {
                    configured: true,
                    url,
                    reachable: ok,
                    model: v.get("model").and_then(|s| s.as_str()).map(str::to_string),
                    revision: v
                        .get("revision")
                        .and_then(|s| s.as_str())
                        .map(str::to_string),
                    error: if ok {
                        None
                    } else {
                        Some("sidecar reported a non-ok status".to_string())
                    },
                }))
            }
            Err(e) => Ok(unreachable(url, format!("invalid /health response: {e}"))),
        },
        Ok(resp) => Ok(unreachable(
            url,
            format!("sidecar returned HTTP {}", resp.status()),
        )),
        Err(e) => Ok(unreachable(url, e.to_string())),
    }
}

#[utoipa::path(
    get, path = "/api/v1/settings/charo", tag = "settings",
    responses((status = 200, body = CharoSettingsView))
)]
async fn get_charo_settings(State(state): State<AdminState>) -> Result<Json<CharoSettingsView>> {
    let enabled = state.store.get_charo_enabled().await?.unwrap_or(true);
    Ok(Json(CharoSettingsView { enabled }))
}

#[utoipa::path(
    put, path = "/api/v1/settings/charo", tag = "settings",
    request_body = CharoSettingsView,
    responses((status = 200, body = CharoSettingsView))
)]
async fn put_charo_settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<CharoSettingsView>,
) -> Result<Json<CharoSettingsView>> {
    state.store.set_charo_enabled(body.enabled).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "set_charo_settings",
            "settings",
            "charo",
            serde_json::json!({ "enabled": body.enabled }),
        )
        .await?;
    Ok(Json(CharoSettingsView {
        enabled: body.enabled,
    }))
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
    Json(body): Json<UpdateWeight>,
) -> Result<Json<Tenant>> {
    let tenant = state.store.update_tenant_weight(id, body.weight).await?;
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
    Json(body): Json<CreateKey>,
) -> Result<Json<CreatedKey>> {
    // The reserved control-plane tenant is system-owned and not user-manageable.
    if tenant_id == Store::CONTROL_PLANE_TENANT_ID {
        return Err(AdminError::Store(obleth_store::StoreError::Protected(
            "the reserved control-plane tenant cannot be modified".into(),
        )));
    }
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AdminError::BadRequest("key name is required".into()));
    }
    let description = body.description.trim().to_string();
    let (period, started_at) = normalize_budget_fields(
        body.budget_tokens,
        body.budget_cost_usd,
        body.budget_period.as_deref(),
        body.budget_started_at,
        "budget_tokens",
        "budget_cost_usd",
        "budget_period",
    )?;
    let (key, secret) = state
        .store
        .create_api_key(
            tenant_id,
            &name,
            &description,
            body.budget_tokens,
            body.budget_cost_usd,
            period.as_deref(),
            started_at,
        )
        .await?;
    let hash = hash_api_key(&secret);
    if let Some(resolved) = state.store.resolved_key_by_hash(&hash).await? {
        push_key(&state, &hash, &resolved).await?;
    }
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "create_key",
            "api_key",
            &key.id.to_string(),
            serde_json::json!({
                "tenant_id": tenant_id,
                "prefix": key.key_prefix,
                "budget_tokens": key.budget_tokens,
                "budget_cost_usd": key.budget_cost_usd,
                "budget_period": key.budget_period,
                "budget_started_at": key.budget_started_at,
            }),
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
    let mut keys = state.store.list_keys(q.tenant_id).await?;
    // Hide the reserved control-plane key (Charo's) from the management surface.
    keys.retain(|k| k.tenant_id != Store::CONTROL_PLANE_TENANT_ID);
    Ok(Json(keys))
}

#[derive(Serialize)]
struct ControlPlaneKeyView {
    secret: String,
}

/// Admin-gated: hand the server-side control-plane (Charo) its reserved API key
/// secret so it can call the data plane on the operator's behalf. The secret is
/// decrypted from `app_settings` and must never reach the browser.
async fn get_control_plane_key(
    State(state): State<AdminState>,
) -> Result<Json<ControlPlaneKeyView>> {
    match state.store.control_plane_key_secret().await? {
        Some(secret) => Ok(Json(ControlPlaneKeyView { secret })),
        None => Err(AdminError::Internal(
            "control-plane identity not provisioned".into(),
        )),
    }
}

#[utoipa::path(
    put, path = "/api/v1/keys/{id}", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    request_body = UpdateKey,
    responses((status = 200, body = ApiKey), (status = 404))
)]
async fn update_key(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateKey>,
) -> Result<Json<ApiKey>> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AdminError::BadRequest("key name is required".into()));
    }
    let description = body.description.trim().to_string();
    let (period, started_at) = normalize_budget_fields(
        body.budget_tokens,
        body.budget_cost_usd,
        body.budget_period.as_deref(),
        body.budget_started_at,
        "budget_tokens",
        "budget_cost_usd",
        "budget_period",
    )?;
    let (hash, key, resolved) = state
        .store
        .update_api_key(
            id,
            &name,
            &description,
            body.budget_tokens,
            body.budget_cost_usd,
            period.as_deref(),
            started_at,
        )
        .await?;
    push_key(&state, &hash, &resolved).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "update_key",
            "api_key",
            &id.to_string(),
            serde_json::json!({
                "tenant_id": key.tenant_id,
                "prefix": key.key_prefix,
                "budget_tokens": key.budget_tokens,
                "budget_cost_usd": key.budget_cost_usd,
                "budget_period": key.budget_period,
                "budget_started_at": key.budget_started_at,
            }),
        )
        .await?;
    Ok(Json(key))
}

#[utoipa::path(
    delete, path = "/api/v1/keys/{id}", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    responses((status = 204), (status = 404))
)]
async fn delete_key(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let hash = state.store.delete_key(id).await?;
    let _ = state.redis.delete_resolved_key(&hash).await;
    let _ = state.redis.publish_invalidation(&hash).await;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
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
    headers: HeaderMap,
    Json(body): Json<SetDisabled>,
) -> Result<StatusCode> {
    let (hash, resolved) = state.store.set_key_disabled(id, body.disabled).await?;
    push_key(&state, &hash, &resolved).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
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

async fn set_key_tracing_handler(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetKeyTracing>,
) -> Result<StatusCode> {
    let (hash, resolved) = state
        .store
        .set_key_tracing(id, body.tracing_enabled)
        .await?;
    push_key(&state, &hash, &resolved).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            if body.tracing_enabled {
                "enable_key_tracing"
            } else {
                "disable_key_tracing"
            },
            "api_key",
            &id.to_string(),
            serde_json::json!({ "tracing_enabled": body.tracing_enabled }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn set_tenant_tracing_handler(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetKeyTracing>,
) -> Result<StatusCode> {
    state
        .store
        .set_tenant_tracing(id, body.tracing_enabled)
        .await?;
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            if body.tracing_enabled {
                "enable_tenant_tracing"
            } else {
                "disable_tenant_tracing"
            },
            "tenant",
            &id.to_string(),
            serde_json::json!({ "tracing_enabled": body.tracing_enabled }),
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
            energy_wh: 0.0,
            energy_cost_usd: 0.0,
            co2_g: 0.0,
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
    get, path = "/api/v1/usage/series/models", tag = "usage",
    params(usage::UsageSeriesQuery),
    responses((status = 200, body = [usage::ModelUsageTimePoint]))
)]
async fn get_usage_series_models(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageSeriesQuery>,
) -> Result<Json<Vec<usage::ModelUsageTimePoint>>> {
    Ok(Json(
        usage::query_usage_series_by_model(&state.clickhouse, q).await?,
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
    /// `true` when at least one span for this request exists in ClickHouse.
    #[serde(default)]
    pub has_trace: bool,
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

    let request_ids: Vec<Uuid> = rows.iter().map(|r| r.request_id).collect();
    let traced = usage::batch_has_trace(&state.clickhouse, &request_ids).await;

    let entries = rows
        .into_iter()
        .map(|row| {
            let tenant_name = tenant_names
                .get(&row.tenant_id)
                .cloned()
                .unwrap_or_default();
            let (key_name, key_prefix) = key_meta.get(&row.key_id).cloned().unwrap_or_default();
            let has_trace = traced.contains(&row.request_id);
            UsageLogEntry {
                row,
                tenant_name,
                key_name,
                key_prefix,
                has_trace,
            }
        })
        .collect();

    Ok(Json(entries))
}

async fn get_request_spans(
    State(state): State<AdminState>,
    Path(request_id): Path<Uuid>,
) -> Result<Json<Vec<usage::SpanEntry>>> {
    Ok(Json(
        usage::query_request_spans(&state.clickhouse, request_id).await?,
    ))
}

/// A per-model breakdown row enriched with human-readable tenant/key names and
/// the tenant's fairshare group, resolved from Postgres so the model card's
/// breakdown table does not have to display bare UUIDs.
#[derive(Debug, Serialize)]
pub struct UsageBreakdownEntry {
    #[serde(flatten)]
    pub row: usage::UsageKeyModelBreakdown,
    pub tenant_name: String,
    pub fairshare_group: String,
    pub key_name: String,
    pub key_prefix: String,
}

/// Per tenant/key breakdown of one model's traffic over the window, powering
/// the breakdown table in the expanded model card. UUIDs are resolved to
/// tenant/key names in two bounded Postgres lookups, mirroring `/usage/logs`.
#[utoipa::path(
    get, path = "/api/v1/usage/breakdown", tag = "usage",
    params(usage::UsageBreakdownQuery),
    responses((status = 200, body = [usage::UsageKeyModelBreakdown],
        description = "Each row also includes tenant_name, fairshare_group, key_name, and key_prefix resolved from Postgres"))
)]
async fn get_usage_breakdown(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageBreakdownQuery>,
) -> Result<Json<Vec<UsageBreakdownEntry>>> {
    let rows =
        usage::query_usage_breakdown_by_model(&state.clickhouse, &q.model, q.since_ms, q.limit)
            .await?;

    let tenant_meta: std::collections::HashMap<Uuid, (String, String)> = state
        .store
        .list_tenants()
        .await?
        .into_iter()
        .map(|t| (t.id, (t.name, t.fairshare_group)))
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
            let (tenant_name, fairshare_group) =
                tenant_meta.get(&row.tenant_id).cloned().unwrap_or_default();
            let (key_name, key_prefix) = key_meta.get(&row.key_id).cloned().unwrap_or_default();
            UsageBreakdownEntry {
                row,
                tenant_name,
                fairshare_group,
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
async fn get_usage_retention(State(state): State<AdminState>) -> Result<Json<UsageRetentionView>> {
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
async fn compact_usage(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<CompactUsageResult>> {
    let result = usage_retention::compact_usage_now(&state).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
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
    let model_in_flight = snap.model_in_flight.clone();
    let model_queued = snap.model_queued.clone();
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
        model_in_flight,
        model_queued,
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
    headers: HeaderMap,
    Json(body): Json<CreateFairshareGroup>,
) -> Result<Json<FairshareGroup>> {
    let group = state
        .store
        .create_fairshare_group(&body.name, body.weight.unwrap_or(100))
        .await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
    Json(body): Json<CreateModel>,
) -> Result<Json<ModelRoute>> {
    // A blank api_base is allowed: Slurm-provisioned models have no static
    // upstream until a replica is promoted into the endpoint rotation. Only
    // validate a non-empty URL.
    if !body.api_base.trim().is_empty() {
        state.ssrf.validate(&body.api_base)?;
    }
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
            &body.tool_servers.clone().unwrap_or_default(),
            body.energy_slots_per_node.unwrap_or(0),
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
    Json(body): Json<UpdateModel>,
) -> Result<Json<ModelRoute>> {
    // Blank api_base allowed for provisioned-only (Slurm) models — see create_model.
    if !body.api_base.trim().is_empty() {
        state.ssrf.validate(&body.api_base)?;
    }
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
            &body
                .tool_servers
                .clone()
                .unwrap_or_else(|| existing.tool_servers.clone()),
            body.energy_slots_per_node
                .unwrap_or(existing.energy_slots_per_node),
        )
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
    Json(body): Json<autotune::AutotuneRequest>,
) -> Result<Json<autotune::AutotuneReport>> {
    let model = state.store.get_model(id).await?;
    let report = autotune::run_probe(&state.health.http, &model, &body)
        .await
        .map_err(|e| AdminError::BadRequest(e.to_string()))?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            body.debug_diagnostics,
        )
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
) -> Result<StatusCode> {
    let model = state.store.get_model(id).await?;
    state.store.delete_model_endpoint(endpoint_id).await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "delete_model_endpoint",
            "model_endpoint",
            &endpoint_id.to_string(),
            serde_json::json!({ "model": model.model_name }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- managed model spec --------------------------------------------------

#[utoipa::path(get, path = "/api/v1/managed",
    responses((status = 200, body = [ManagedModelSpec])))]
async fn list_managed_models(
    State(state): State<AdminState>,
) -> Result<Json<Vec<ManagedModelSpec>>> {
    Ok(Json(state.store.list_managed_models().await?))
}

#[utoipa::path(get, path = "/api/v1/models/{id}/managed",
    responses((status = 200, body = Option<ManagedModelSpec>)))]
async fn get_managed_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Option<ManagedModelSpec>>> {
    Ok(Json(state.store.get_managed_model(id).await?))
}

#[utoipa::path(put, path = "/api/v1/models/{id}/managed",
    request_body = PutManagedModel,
    responses((status = 200, body = ManagedModelSpec)))]
async fn put_managed_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<PutManagedModel>,
) -> Result<Json<ManagedModelSpec>> {
    if body.serving_port < 1 || body.serving_port > 65535 {
        return Err(AdminError::BadRequest(
            "serving_port must be 1..=65535".into(),
        ));
    }
    if body.partition.trim().is_empty() {
        return Err(AdminError::BadRequest("partition must not be empty".into()));
    }
    // image is optional: an empty image means bare-metal (no apptainer wrap).
    // A model is launchable if it has either a rendered script_body or a launch_command.
    if body.script_body.trim().is_empty() && body.launch_command.trim().is_empty() {
        return Err(AdminError::BadRequest(
            "launch_command or script_body must not be empty".into(),
        ));
    }
    let spec = state
        .store
        .upsert_managed_model(obleth_store::UpsertManagedModel {
            model_id: id,
            enabled: body.enabled,
            partition: body.partition,
            gres: body.gres,
            nodes: body.nodes,
            constraints: body.constraints,
            exclude: body.exclude,
            account: body.account,
            qos: body.qos,
            time_limit: body.time_limit,
            cpus_per_task: body.cpus_per_task,
            mem: body.mem,
            image: body.image,
            preamble: body.preamble,
            log_output_dir: body.log_output_dir,
            launch_command: body.launch_command,
            script_body: body.script_body,
            serving_port: body.serving_port,
            health_path: body.health_path,
            target_replicas: body.target_replicas,
            min_replicas: body.min_replicas,
            max_job_failures: body.max_job_failures,
            launcher_spec: body.launcher_spec,
        })
        .await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "put_managed_model",
            "managed_model",
            &id.to_string(),
            serde_json::to_value(&spec).unwrap_or_default(),
        )
        .await?;
    Ok(Json(spec))
}

#[utoipa::path(delete, path = "/api/v1/models/{id}/managed",
    responses((status = 200)))]
async fn delete_managed_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    state.store.delete_managed_model(id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "delete_managed_model",
            "managed_model",
            &id.to_string(),
            serde_json::json!({}),
        )
        .await?;
    Ok(Json(serde_json::json!({"deleted": true})))
}

#[derive(serde::Deserialize)]
struct ProvisionErrorBody {
    #[serde(default)]
    error: Option<String>,
}

#[utoipa::path(patch, path = "/api/v1/models/{id}/managed/provision-error",
    responses((status = 200)))]
async fn set_provision_error(
    State(state): State<AdminState>,
    Path(id): Path<uuid::Uuid>,
    Json(body): Json<ProvisionErrorBody>,
) -> Result<Json<serde_json::Value>> {
    state
        .store
        .set_provision_error(id, body.error.as_deref())
        .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ---- replica registry ----------------------------------------------------

#[utoipa::path(get, path = "/api/v1/models/{id}/replicas",
    responses((status = 200, body = [ModelReplica])))]
async fn list_replicas(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ModelReplica>>> {
    Ok(Json(state.store.list_replicas(id).await?))
}

#[utoipa::path(get, path = "/api/v1/replicas",
    responses((status = 200, body = [ModelReplica])))]
async fn list_all_replicas(State(state): State<AdminState>) -> Result<Json<Vec<ModelReplica>>> {
    Ok(Json(state.store.all_replicas().await?))
}

#[utoipa::path(post, path = "/api/v1/models/{id}/replicas",
    request_body = CreateReplica,
    responses((status = 200, body = ModelReplica)))]
async fn create_replica(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<CreateReplica>,
) -> Result<Json<ModelReplica>> {
    let r = state
        .store
        .create_replica(id, &body.slurm_job_id, body.port_base)
        .await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "create_replica",
            "model_replica",
            &r.id.to_string(),
            serde_json::to_value(&r).unwrap_or_default(),
        )
        .await?;
    Ok(Json(r))
}

#[utoipa::path(patch, path = "/api/v1/replicas/{id}",
    request_body = PatchReplica,
    responses((status = 200, body = ModelReplica)))]
async fn patch_replica(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<PatchReplica>,
) -> Result<Json<ModelReplica>> {
    // Validate up front so an invalid state can't partially apply runtime changes.
    if let Some(state_val) = body.state.as_deref() {
        if !obleth_config::REPLICA_STATES.contains(&state_val) {
            return Err(AdminError::BadRequest("invalid replica state".into()));
        }
    }
    // Apply runtime (nodes/endpoint) BEFORE flipping state. If linking the
    // endpoint fails (e.g. an invalid endpoint_id), we must not have already
    // marked the replica healthy — that would strand it as "healthy" with no
    // endpoint, which the planner never re-promotes.
    if body.nodes.is_some() || body.endpoint_id.is_some() {
        state
            .store
            .set_replica_runtime(id, body.nodes.as_deref(), body.endpoint_id)
            .await?;
    }
    if let Some(state_val) = body.state.as_deref() {
        state
            .store
            .update_replica_state(id, state_val, body.message.as_deref())
            .await?;
    } else if let Some(msg) = body.message.as_deref() {
        state.store.set_replica_message(id, msg).await?;
    }
    let current = state
        .store
        .get_replica(id)
        .await?
        .ok_or(AdminError::NotFound)?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "patch_replica",
            "model_replica",
            &id.to_string(),
            serde_json::to_value(&current).unwrap_or_default(),
        )
        .await?;
    Ok(Json(current))
}

#[utoipa::path(post, path = "/api/v1/replicas/{id}/restart",
    responses((status = 200)))]
async fn restart_replica(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    if !state.store.request_replica_cancel(id).await? {
        return Err(AdminError::NotFound);
    }
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "restart_replica",
            "model_replica",
            &id.to_string(),
            serde_json::json!({}),
        )
        .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[utoipa::path(delete, path = "/api/v1/replicas/{id}",
    responses((status = 200)))]
async fn delete_replica(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    state.store.delete_replica(id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "delete_replica",
            "model_replica",
            &id.to_string(),
            serde_json::json!({}),
        )
        .await?;
    Ok(Json(serde_json::json!({"deleted": true})))
}

#[utoipa::path(post, path = "/api/v1/models/{id}/replicas/clear-lost",
    responses((status = 200, body = serde_json::Value)))]
pub async fn clear_lost_replicas(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>> {
    let n = state.store.delete_lost_replicas(id).await?;
    Ok(Json(serde_json::json!({ "deleted": n })))
}

#[utoipa::path(
    delete, path = "/api/v1/models/{id}", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 204), (status = 404))
)]
async fn delete_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode> {
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
    Json(body): Json<SetCapacity>,
) -> Result<Json<CapacityView>> {
    state.capacity.set(body.max_in_flight);
    state
        .store
        .record_audit(
            &audit_actor(&headers),
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
    if let Some(tx) = &state.local_cache_tx {
        let _ = tx.send(hash.to_string());
    }
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
        tool_servers: model.tool_servers.clone(),
        request_timeout_secs: model.request_timeout_secs,
        max_retries: model.max_retries,
        retry_backoff_ms: model.retry_backoff_ms,
        endpoint_selection_mode: model.endpoint_selection_mode.clone(),
        debug_diagnostics: model.debug_diagnostics,
        energy_slots_per_node: model.energy_slots_per_node,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boon_view_round_trips_compression() {
        use obleth_config::{BoonSettings, CompressionBoonSettings};
        let s = BoonSettings {
            compression: CompressionBoonSettings {
                enabled: true,
                min_tokens: 256,
                max_segments: 8,
                ..Default::default()
            },
            ..Default::default()
        };
        let view = BoonSettingsView::from_settings(&s);
        assert!(view.compression_enabled);
        assert_eq!(view.compression_min_tokens, 256);
        assert_eq!(view.compression_max_segments, 8);
        // Defaults surface the neural keep ratio so operators can read/tune it.
        assert_eq!(view.compression_neural_keep_ratio, 0.5);
    }

    #[test]
    fn boon_view_round_trips_neural_keep_ratio() {
        use obleth_config::BoonSettings;
        let mut s = BoonSettings::default();
        s.compression.neural_keep_ratio = 0.3;
        let view = BoonSettingsView::from_settings(&s);
        assert_eq!(view.compression_neural_keep_ratio, 0.3);
    }

    #[test]
    fn boon_view_round_trips_lossy_compression() {
        use obleth_config::BoonSettings;
        let mut s = BoonSettings::default();
        s.compression.enabled = true;
        s.compression.original_ttl_secs = 999;
        s.compression.max_lossy_segments = 7;
        let view = BoonSettingsView::from_settings(&s);
        assert_eq!(view.compression_original_ttl_secs, 999);
        assert_eq!(view.compression_max_lossy_segments, 7);
    }

    #[test]
    fn boon_view_round_trips_code_compaction() {
        use obleth_config::{BoonSettings, CompressionBoonSettings};
        let s = BoonSettings {
            compression: CompressionBoonSettings {
                enabled: true,
                code_compaction: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let view = BoonSettingsView::from_settings(&s);
        assert!(view.compression_code_compaction);
    }

    #[test]
    fn set_tenant_compression_deserializes_null_and_policy() {
        // null clears the policy.
        let cleared: SetTenantCompression = serde_json::from_str(r#"{"policy": null}"#).unwrap();
        assert!(cleared.policy.is_none());

        // A present policy round-trips.
        let set: SetTenantCompression =
            serde_json::from_str(r#"{"policy": {"enabled": true, "allow_lossy": false}}"#).unwrap();
        let p = set.policy.expect("policy present");
        assert!(p.enabled);
        assert!(!p.allow_lossy);
    }

    #[test]
    fn set_tenant_compression_carries_per_piece_flags() {
        let set: SetTenantCompression = serde_json::from_str(
            r#"{"policy":{"enabled":true,"code_compaction":true,"dedup":true,"allow_lossy":true}}"#,
        )
        .unwrap();
        let p = set.policy.expect("policy present");
        assert!(p.enabled && p.code_compaction && p.dedup && p.allow_lossy);
    }

    #[test]
    fn energy_settings_view_round_trip() {
        let s = obleth_config::EnergySettings {
            enabled: true,
            prometheus_url: "http://prom:9090".into(),
            power_query: "habana_device_power_watts".into(),
            poll_interval_secs: 30,
            energy_cost_per_kwh: 0.12,
            carbon_g_per_kwh: 400.0,
            pue: 1.2,
        };
        let view = EnergySettingsView::from_settings(&s);
        assert!(view.enabled);
        assert_eq!(view.poll_interval_secs, 30);
        assert_eq!(view.pue, 1.2);
    }

    #[test]
    fn update_energy_settings_merges_partials() {
        let existing = obleth_config::EnergySettings::default();
        let body = UpdateEnergySettings {
            enabled: Some(true),
            prometheus_url: Some("http://prom:9090".into()),
            power_query: Some("watts".into()),
            poll_interval_secs: None,
            energy_cost_per_kwh: Some(0.15),
            carbon_g_per_kwh: None,
            pue: None,
        };
        let merged = merge_energy_settings(&existing, &body);
        assert!(merged.enabled);
        assert_eq!(merged.poll_interval_secs, 60); // untouched default
        assert_eq!(merged.energy_cost_per_kwh, 0.15);
        assert_eq!(merged.pue, 1.0);
    }
}
