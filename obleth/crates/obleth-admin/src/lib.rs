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
pub mod capacity_discovery;
pub mod energy_probe;
mod error;
pub mod knowledge;
pub mod model_health;
mod models_io;
mod openapi;
pub mod recipes;
pub mod router_readiness;
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
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use obleth_config::routing::route_explain::RouteExplain;
use obleth_config::routing::{
    difficulty_from_header, explain_selection, heuristic_intent, BoonGrants, IntentSource,
    RequestFeatures, RouterWeights,
};
use obleth_config::{
    hash_api_key, ApiKey, FairshareGroup, ManagedModelSpec, McpServer, ModelEndpoint, ModelReplica,
    ModelRoute, ResolvedKey, ResolvedMcpServer, ResolvedModel, Tenant,
};
use obleth_config::{
    AlertSettings, AutoRouterSettings, BoonSettings, EmailSettings, StructuredOutputBoonSettings,
    ToolLoopSettings, VisionBoonSettings, STRUCTURED_OUTPUT_MAX_REPAIR_ATTEMPTS,
    TOOL_LOOP_MAX_DEADLINE_SECS, TOOL_LOOP_MAX_TURNS,
};
use obleth_fairshare::{replica_share, FairShare, FairshareHistory, StaticCapacity, Stats};
use obleth_redis::RedisStore;
use obleth_store::{AuditEntry, Store};
use obleth_tokenizer::Tokenizer;
use rand::Rng;
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

/// Serde helper for a patch field that tells "absent" (keep) from `null`
/// (clear): use with `#[serde(default, deserialize_with = "nullable")]` on an
/// `Option<Option<T>>`.
fn nullable<'de, D, T>(d: D) -> std::result::Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(Some)
}

/// Shared state for the Management API. All fields are cheap-clone handles.
#[derive(Clone)]
pub struct AdminState {
    pub store: Store,
    pub redis: RedisStore,
    pub capacity: Arc<StaticCapacity>,
    pub fairshare: FairShare,
    /// Per-model in-flight cap used for models that don't set their own
    /// `max_in_flight`, sourced from `cfg.default_model_max_in_flight`.
    pub default_model_max_in_flight: usize,
    /// Sampled scheduler history for `/api/v1/fairshare/history`.
    pub fairshare_history: Arc<FairshareHistory>,
    /// Configured retention in seconds; `0` means the sampler is off.
    pub fairshare_history_secs: u64,
    /// Whether fairshare divides its limits across the live replicas
    /// (`OBLETH_FAIRSHARE_REPLICA_AWARE`). Reported by the live view.
    pub fairshare_replica_aware: bool,
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
    /// The data plane's rolling per-model completion-length averages, shared
    /// in-process so the simulate endpoint scores with the same expected
    /// output tokens the live router uses. `Default` (empty) in a standalone
    /// admin process; scoring then falls back to the documented default.
    pub output_stats: obleth_config::routing::OutputStats,
    /// Bridge to the data plane's intent classifier, for `simulate` calls that
    /// opt into a real classification (`classify: true`). Injected by the
    /// binary that owns the classifier so this crate never depends on the
    /// proxy; `None` (a standalone admin process) means simulate stays
    /// heuristic-only and says so.
    pub classify: Option<ClassifyFn>,
    /// The gateway's capacity discovery: its settings, which model writes are
    /// checked against.
    pub capacity_discovery: capacity_discovery::CapacityDiscovery,
}

/// A boxed call into the data plane's classifier: `(prompt, available_tags)`
/// to the derived [`obleth_config::routing::Intent`]. Timeout, caching and
/// every failure-lowers-difficulty guarantee live behind the closure, in the
/// classifier itself.
pub type ClassifyFn = std::sync::Arc<
    dyn Fn(
            String,
            Vec<String>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = obleth_config::routing::Intent> + Send>,
        > + Send
        + Sync,
>;

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
        .route(
            "/api/v1/tenants/:id/synthetic",
            put(set_tenant_synthetic_handler),
        )
        .route("/api/v1/keys", get(list_keys))
        .route("/api/v1/resync", post(resync_resolver_cache))
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
        .route("/api/v1/overview/summary", get(get_overview_summary))
        .route("/api/v1/fairshare/live", get(get_fairshare_live))
        .route("/api/v1/capacity/discovery", get(get_capacity_discovery))
        .route("/api/v1/fairshare/history", get(get_fairshare_history))
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
            "/api/v1/models/validate",
            post(model_health::validate_model),
        )
        // Static segments must stay above `/api/v1/models/:id` so they are not
        // swallowed by the uuid param route, same as `health` and `validate`.
        .route("/api/v1/models/export", get(models_io::export_models))
        .route("/api/v1/models/import", post(models_io::import_models))
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
            "/api/v1/models/:id/knowledge",
            get(knowledge::get_model_collections).put(knowledge::set_model_collections),
        )
        .route(
            "/api/v1/knowledge/collections",
            post(knowledge::create_collection).get(knowledge::list_collections),
        )
        .route(
            "/api/v1/knowledge/collections/:id",
            get(knowledge::get_collection)
                .put(knowledge::update_collection)
                .delete(knowledge::delete_collection),
        )
        .route(
            "/api/v1/knowledge/collections/:id/documents",
            // Documents arrive base64-encoded in JSON, well past axum's 2 MB
            // default; size the limit for the largest permitted upload.
            get(knowledge::list_documents)
                .post(knowledge::upload_document)
                .layer(axum::extract::DefaultBodyLimit::max(
                    knowledge::UPLOAD_BODY_LIMIT_BYTES,
                )),
        )
        .route(
            "/api/v1/knowledge/collections/:id/reindex",
            post(knowledge::reindex_collection),
        )
        .route(
            "/api/v1/knowledge/collections/:id/search",
            post(knowledge::search_collection),
        )
        .route(
            "/api/v1/knowledge/documents/:id",
            delete(knowledge::delete_document),
        )
        .route(
            "/api/v1/knowledge/documents/:id/reindex",
            post(knowledge::reindex_document),
        )
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
        .route("/api/v1/router/simulate", post(simulate_route))
        .route(
            "/api/v1/router/readiness",
            get(router_readiness::get_router_readiness),
        )
        .route(
            "/api/v1/settings/boons",
            get(get_boon_settings).put(put_boon_settings),
        )
        .route(
            "/api/v1/settings/knowledge",
            get(knowledge::get_knowledge_settings).put(knowledge::put_knowledge_settings),
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
    /// Mark the tenant synthetic at creation (benchmark/test traffic).
    #[serde(default)]
    pub synthetic: Option<bool>,
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
pub struct SetTenantSynthetic {
    pub synthetic: bool,
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
    /// Fairshare weight among the tenant's keys. Default 100, minimum 1.
    #[serde(default = "obleth_config::default_key_weight")]
    pub weight: i64,
    /// Per-model in-flight ceiling for this key. `null` clears the cap.
    #[serde(default)]
    pub max_in_flight: Option<i64>,
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
    /// Fairshare weight among the tenant's keys. Default 100, minimum 1.
    #[serde(default = "obleth_config::default_key_weight")]
    pub weight: i64,
    /// Per-model in-flight ceiling for this key. `null` clears the cap.
    #[serde(default)]
    pub max_in_flight: Option<i64>,
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
pub struct SetKeyTracing {
    pub tracing_enabled: bool,
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

/// Total in-flight ceiling across all model pools, not a fairness budget.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetCapacity {
    pub max_in_flight: usize,
}

/// Total in-flight ceiling across all model pools, not a fairness budget.
#[derive(Debug, Serialize, ToSchema)]
pub struct CapacityView {
    pub max_in_flight: usize,
}

/// This replica's live counters. `max_in_flight` is its share of the enabled
/// models' pool sizes, the capacity `in_flight` is measured against.
#[derive(Debug, Serialize, ToSchema)]
pub struct LiveStats {
    pub in_flight: usize,
    pub queued: i64,
    pub max_in_flight: usize,
    /// Live gateway replicas the configured limits are divided across.
    pub replicas: usize,
}

/// At-a-glance dashboard summary: config counts from Postgres plus usage
/// totals over the window from ClickHouse. Backs the overview/footer poll
/// without deserializing the full tenant/key/model lists per tick.
#[derive(Debug, Serialize, ToSchema)]
pub struct OverviewSummaryView {
    pub requests: u64,
    pub tokens: u64,
    pub cost: f64,
    pub has_pricing: bool,
    pub tenant_count: i64,
    pub active_tenants: u64,
    pub model_count: i64,
    pub enabled_models: i64,
    pub key_count: i64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GroupFairshareView {
    pub name: String,
    pub weight: i64,
    pub in_flight: usize,
    pub queued: usize,
    pub slot_cap: usize,
    /// Slots held above this group's cap, borrowed from siblings leaving
    /// capacity idle. Non-zero only while some other group is under-demand.
    pub borrowed: usize,
    pub served_tokens: f64,
    pub share_score: f64,
    pub weight_share: f64,
    pub expected_slots: f64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TenantFairshareView {
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub name: String,
    pub fairshare_group: String,
    pub weight: i64,
    pub max_in_flight: Option<usize>,
    pub in_flight: usize,
    pub queued: usize,
    pub served_tokens: f64,
    pub share_score: f64,
    pub weight_share: f64,
    /// Steady-state slot share under sustained contention.
    pub expected_slots: f64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct KeyFairshareView {
    #[schema(value_type = String)]
    pub key_id: Uuid,
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub name: String,
    pub weight: i64,
    pub max_in_flight: Option<usize>,
    pub in_flight: usize,
    pub queued: usize,
    pub served_tokens: f64,
    pub share_score: f64,
    pub weight_share: f64,
    pub expected_slots: f64,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelPoolView {
    pub model: String,
    /// Slots this replica enforces: its share of `configured_cap`.
    pub cap: usize,
    /// The pool size as configured, before it is divided across replicas.
    pub configured_cap: usize,
    pub in_flight: usize,
    pub queued: usize,
    pub borrowed: usize,
    pub groups: Vec<GroupFairshareView>,
    pub tenants: Vec<TenantFairshareView>,
    pub keys: Vec<KeyFairshareView>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FairshareLiveView {
    pub algorithm: String,
    /// Slots this replica can run: its share of each enabled model's pool
    /// size, summed, taken from the database rather than from the snapshot,
    /// so a model that has had no traffic yet still counts. The aggregated
    /// `weight_share` below is normalized against the sum of the caps of the
    /// pools that are *present in the snapshot*, so the two can differ until
    /// every enabled model has been used at least once.
    pub max_in_flight: usize,
    /// The enabled models' pool sizes as configured, summed: the fleet-wide
    /// intent that `max_in_flight` is this replica's share of.
    pub configured_max_in_flight: usize,
    /// Total in-flight ceiling across all pools as this replica enforces it:
    /// its share of `OBLETH_GLOBAL_MAX_IN_FLIGHT`.
    pub hard_ceiling: usize,
    /// `OBLETH_GLOBAL_MAX_IN_FLIGHT` as configured.
    pub configured_hard_ceiling: usize,
    pub default_model_max_in_flight: usize,
    /// Live gateway replicas the configured limits are divided across. Every
    /// count in this view (in flight, queued, caps) is this replica's own.
    pub replicas: usize,
    /// Whether limits are divided across replicas at all
    /// (`OBLETH_FAIRSHARE_REPLICA_AWARE`); when off, `replicas` is 1.
    pub replica_aware: bool,
    pub global_in_flight: usize,
    pub global_queued: i64,
    /// Total occupancy above the apportioned group caps.
    pub global_borrowed: usize,
    /// Aggregated across pools (see `aggregate_pools`).
    pub groups: Vec<GroupFairshareView>,
    pub tenants: Vec<TenantFairshareView>,
    pub keys: Vec<KeyFairshareView>,
    pub pools: Vec<ModelPoolView>,
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
    /// Extra client-facing names this model answers to, so a name can be
    /// cleaned up (`glm-5-3-fp8` -> `glm-5-3`) without breaking pinned
    /// clients. Each must be unused by any other model's name or aliases.
    #[serde(default)]
    pub aliases: Option<Vec<String>>,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    /// Extra headers sent on every request to this model's upstream, after the
    /// client's forwarded headers so these win (e.g. a routing hint an
    /// inference gateway reads, or a tenant or organization header a provider
    /// requires). Names are case-insensitive. Hop-by-hop headers,
    /// `authorization` (use `api_key`), `host`, `content-length`,
    /// `content-type`, and `accept-encoding` are refused. Values are
    /// write-only: responses list only `upstream_header_names`.
    #[serde(default)]
    #[schema(value_type = Option<std::collections::BTreeMap<String, String>>)]
    pub upstream_headers: Option<obleth_config::UpstreamHeadersWrite>,
    #[serde(default)]
    pub model_type: Option<String>,
    /// Serving format from the fixed `QUANTIZATIONS` vocabulary. Omitted means
    /// `unknown` — undeclared, not "full precision".
    #[serde(default)]
    pub quantization: Option<String>,
    pub input_cost_per_token: Option<f64>,
    pub output_cost_per_token: Option<f64>,
    #[serde(default)]
    pub cost_per_image: Option<f64>,
    #[serde(default)]
    pub cost_per_audio_second: Option<f64>,
    #[serde(default)]
    pub cost_per_character: Option<f64>,
    /// Flat USD price of one created job (`video` models).
    #[serde(default)]
    pub cost_per_video: Option<f64>,
    pub context_window: Option<i64>,
    pub admission_weight: Option<i64>,
    pub max_in_flight: Option<i64>,
    /// `static` (default), `tuned` or `discovered`. In `discovered` mode the
    /// pool size follows the live backend and `max_in_flight` is the fallback.
    #[serde(default)]
    pub capacity_mode: Option<String>,
    /// `discovered` mode: `endpoints` (default) or `kubernetes`.
    #[serde(default)]
    pub capacity_source: Option<String>,
    /// `kubernetes` source: namespace of the backend pods. Omitted searches
    /// the gateway's `OBLETH_CAPACITY_DISCOVERY_NAMESPACES`.
    #[serde(default)]
    pub capacity_namespace: Option<String>,
    /// `kubernetes` source: label selector for the serving pods. Omitted uses
    /// the gateway's `OBLETH_CAPACITY_DEFAULT_SELECTOR`.
    #[serde(default)]
    pub capacity_selector: Option<String>,
    /// Requests one serving replica takes. Omitted reads a known concurrency
    /// flag off the serving container (`kubernetes`) or uses each endpoint's
    /// `max_in_flight` (`endpoints`).
    #[serde(default)]
    pub per_replica_max_in_flight: Option<i64>,
    /// Multiplier on the derived pool size (default 1.0).
    #[serde(default)]
    pub capacity_headroom: Option<f64>,
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
    /// Per-model multiplier on the `auto` router's final score. `1.0` is
    /// neutral; the router clamps to `[0.1, 3.0]` before applying it.
    #[serde(default)]
    pub route_bias: Option<f64>,
    /// Whether the `auto` router may select this model. `false` keeps the model
    /// addressable by name but removes it from auto's candidate pool. Omitted
    /// means eligible.
    #[serde(default)]
    pub auto_eligible: Option<bool>,
    /// This model's own speculation drafter. Empty or omitted = fleet default.
    #[serde(default)]
    pub draft_model: Option<String>,
    /// Direct URL of a deployment of this model that scores its drafts
    /// (`prompt_logprobs`). Empty or omitted = the model cannot speculate.
    #[serde(default)]
    pub verify_api_base: Option<String>,
    /// Name that scoring backend serves, if not this model's `upstream_model`.
    #[serde(default)]
    pub verify_upstream_model: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateModel {
    pub description: Option<String>,
    /// Replaces the alias list wholesale; omitted leaves it unchanged. An
    /// empty list removes every alias, which also evicts their resolver keys.
    #[serde(default)]
    pub aliases: Option<Vec<String>>,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    /// Replaces the model's upstream headers; omitted leaves them unchanged
    /// and `{}` removes them all. A `null` value keeps the value stored under
    /// that name, so a client that only knows the names (values are
    /// write-only) can add or drop one header without re-sending the others.
    #[serde(default)]
    #[schema(value_type = Option<std::collections::BTreeMap<String, Option<String>>>)]
    pub upstream_headers: Option<obleth_config::UpstreamHeadersWrite>,
    #[serde(default)]
    pub model_type: Option<String>,
    /// Serving format from the fixed `QUANTIZATIONS` vocabulary; omitted
    /// leaves the current value unchanged.
    #[serde(default)]
    pub quantization: Option<String>,
    pub input_cost_per_token: Option<f64>,
    pub output_cost_per_token: Option<f64>,
    #[serde(default)]
    pub cost_per_image: Option<f64>,
    #[serde(default)]
    pub cost_per_audio_second: Option<f64>,
    #[serde(default)]
    pub cost_per_character: Option<f64>,
    /// Flat USD price of one created job (`video` models).
    #[serde(default)]
    pub cost_per_video: Option<f64>,
    pub context_window: Option<i64>,
    pub admission_weight: Option<i64>,
    pub max_in_flight: Option<i64>,
    /// `static`, `tuned` or `discovered`; omitted leaves it unchanged.
    #[serde(default)]
    pub capacity_mode: Option<String>,
    /// `endpoints` or `kubernetes`; omitted leaves it unchanged.
    #[serde(default)]
    pub capacity_source: Option<String>,
    /// Omitted leaves it unchanged; `null` or `""` clears it.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub capacity_namespace: Option<Option<String>>,
    /// Omitted leaves it unchanged; `null` or `""` clears it.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub capacity_selector: Option<Option<String>>,
    /// Omitted leaves it unchanged; `null` clears it.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub per_replica_max_in_flight: Option<Option<i64>>,
    /// Omitted leaves it unchanged.
    #[serde(default)]
    pub capacity_headroom: Option<f64>,
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
    /// Per-model multiplier on the `auto` router's final score. `1.0` is
    /// neutral; the router clamps to `[0.1, 3.0]` before applying it.
    #[serde(default)]
    pub route_bias: Option<f64>,
    /// Whether the `auto` router may select this model. `false` keeps the model
    /// addressable by name but removes it from auto's candidate pool. Omitted
    /// leaves the current value unchanged.
    #[serde(default)]
    pub auto_eligible: Option<bool>,
    /// This model's own speculation drafter. Empty clears it (fleet default);
    /// omitted leaves the current value unchanged.
    #[serde(default)]
    pub draft_model: Option<String>,
    /// Direct scoring URL for this model's drafts. Empty clears it (the model
    /// stops speculating); omitted leaves the current value unchanged.
    #[serde(default)]
    pub verify_api_base: Option<String>,
    /// Name the scoring backend serves, if not `upstream_model`. Empty clears.
    #[serde(default)]
    pub verify_upstream_model: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelCache {
    pub cache_enabled: bool,
    /// Response-cache lifetime in seconds (default 300). 0 disables the cache:
    /// responses are not written.
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
    /// Requests this endpoint takes at once, for a `discovered` model using
    /// the `endpoints` source. Omitted uses the model's
    /// `per_replica_max_in_flight`.
    #[serde(default)]
    pub max_in_flight: Option<i64>,
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
    /// Omitted keeps the stored value; `null` clears it.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub max_in_flight: Option<Option<i64>>,
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
    /// The `discovered`-mode fields, with the same rules as on
    /// `PUT /api/v1/models/{id}`: omitted leaves a field unchanged.
    #[serde(default)]
    pub capacity_source: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub capacity_namespace: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub capacity_selector: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub per_replica_max_in_flight: Option<Option<i64>>,
    #[serde(default)]
    pub capacity_headroom: Option<f64>,
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
    let mut tenant = state
        .store
        .create_tenant(
            &body.name,
            body.weight.unwrap_or(100),
            body.tokens_per_minute.unwrap_or(0),
            body.max_in_flight,
            body.fairshare_group.as_deref(),
        )
        .await?;
    if body.synthetic == Some(true) {
        state.store.set_tenant_synthetic(tenant.id, true).await?;
        tenant.synthetic = true;
    }
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

// ---- secret-redacted response views ----
//
// The store decrypts upstream secrets on every read, so the config structs
// carry plaintext keys. Every Management API response (and audit detail) for
// models, endpoints, and MCP servers goes through these views instead: secrets
// are write-only and only their presence is reported. The `From` impls
// destructure exhaustively so a new config field fails to compile here rather
// than silently going missing from the API.

/// A registered model route as returned by the Management API. Identical to
/// the stored route except the upstream `api_key` is never returned.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelRouteView {
    pub id: Uuid,
    /// Name clients pass in `model` (e.g. `qwen3-vl-32b-instruct`). Kept free
    /// of deployment detail — the serving format belongs in `quantization`,
    /// not in the name — so re-quantizing does not break every caller.
    pub model_name: String,
    /// Additional client-facing names that resolve to this same route. Exists
    /// so a name can be cleaned up without breaking pinned clients: register
    /// the old `…-fp8` spelling as an alias and it keeps working, while only
    /// `model_name` is advertised by the discovery endpoints.
    pub aliases: Vec<String>,
    /// Human-facing summary for operators and dashboards.
    pub description: String,
    /// Value sent to the upstream in the `model` field.
    pub upstream_model: String,
    /// Base URL including `/v1` suffix when required.
    pub api_base: String,
    /// Whether an upstream API key is stored. The key itself is write-only.
    pub api_key_set: bool,
    /// Names of the headers added to every upstream request. The values are
    /// write-only, like `api_key`, because an operator may put a credential
    /// in one.
    pub upstream_header_names: Vec<String>,
    /// Modality from the fixed `MODEL_TYPES` vocabulary. Determines which
    /// OpenAI endpoint this model serves (`chat`, `embedding`,
    /// `audio_transcription`, `audio_speech`, `image`, `video`). Defaults to
    /// `chat`.
    pub model_type: String,
    /// Weight/activation format this deployment serves, from the fixed
    /// `QUANTIZATIONS` vocabulary. Descriptive only — it never affects
    /// routing; it is reported so clients can tell a `fp8` deployment from a
    /// `bf16` one without reading it out of the model's name.
    pub quantization: String,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    /// Per-generated-image cost in USD (`image` models).
    pub cost_per_image: f64,
    /// Per-second-of-audio cost in USD (`audio_transcription` models).
    pub cost_per_audio_second: f64,
    /// Per-input-character cost in USD (`audio_speech` models).
    pub cost_per_character: f64,
    /// Per-created-job cost in USD (`video` models), charged once when the
    /// create call succeeds.
    pub cost_per_video: f64,
    pub context_window: i64,
    /// Multiplier applied to tenant weight at admission when this model is used.
    pub admission_weight: i64,
    /// Optional per-model in-flight cap. `None` means only the global scheduler
    /// cap and tenant/group fairshare limits apply.
    pub max_in_flight: Option<i64>,
    /// How `max_in_flight` is decided. `static` (default) keeps the
    /// operator-set value; `tuned` means it was found by the auto-tune ramp
    /// probe against the upstream.
    pub capacity_mode: String,
    /// When the tuned `max_in_flight` was last written by auto-tune. `None`
    /// until the model has been tuned.
    pub capacity_tuned_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `discovered` mode: where serving replicas are counted, `endpoints`
    /// (the model's enabled, healthy endpoints) or `kubernetes` (Ready pods).
    pub capacity_source: String,
    /// `kubernetes` source: namespace of the backend pods. `None` searches
    /// the gateway's `OBLETH_CAPACITY_DISCOVERY_NAMESPACES`.
    pub capacity_namespace: Option<String>,
    /// `kubernetes` source: label selector for the serving pods. `None` uses
    /// the gateway's `OBLETH_CAPACITY_DEFAULT_SELECTOR`.
    pub capacity_selector: Option<String>,
    /// Requests one serving replica takes. `None` reads a known concurrency
    /// flag off the serving container (`kubernetes`) or each endpoint's
    /// `max_in_flight` (`endpoints`).
    pub per_replica_max_in_flight: Option<i64>,
    /// Multiplier on the derived pool size; `1.0` is exactly the ready
    /// capacity.
    pub capacity_headroom: f64,
    pub supports_function_calling: bool,
    pub supports_system_messages: bool,
    pub supports_response_schema: bool,
    pub supports_tool_choice: bool,
    /// Native image-input capability. When false, the gateway's vision boon can
    /// relay images to a designated vision model and inject text descriptions
    /// before forwarding the request to this model.
    pub supports_vision: bool,
    pub enabled: bool,
    /// When true, identical requests to this model are served from the response
    /// cache (exact-match on tenant + model + request body) instead of the
    /// upstream. Entries are never shared across tenants.
    pub cache_enabled: bool,
    /// Time-to-live for cached responses, in seconds. `0` disables caching:
    /// nothing is stored (never "store without expiry").
    pub cache_ttl_secs: i64,
    /// Routing tags from the fixed `MODEL_TAGS` vocabulary. The `auto` router
    /// prefers models whose tags match the request's classified intent.
    pub tags: Vec<String>,
    /// Gateway boons enabled for this model from the fixed `MODEL_BOONS`
    /// vocabulary. Boons grant capabilities the model lacks natively (e.g. the
    /// `vision` boon relays images to a describer). Empty by default.
    pub boons: Vec<String>,
    /// Registered MCP servers whose tools this model may use. Distinct from
    /// capabilities: a capability is what the model can do natively (e.g.
    /// function calling); a tool server is something the gateway grants access
    /// to. When non-empty, plain chat requests get the servers' tools injected
    /// and the gateway runs the tool loop itself. Empty by default (off).
    pub tool_servers: Vec<String>,
    /// Per-request upstream timeout in seconds. `None` falls back to the global
    /// `OBLETH_UPSTREAM_TIMEOUT_SECS` default.
    pub request_timeout_secs: Option<i64>,
    /// Extra attempts against the same endpoint on retryable failures (network
    /// errors, timeouts, 408/429/5xx). `0` disables retries.
    pub max_retries: i64,
    /// Base delay in milliseconds for exponential backoff between retries.
    pub retry_backoff_ms: i64,
    /// How obleth chooses among this model's registered endpoints: `failover`
    /// (priority order) or `load_balance` (weighted).
    pub endpoint_selection_mode: String,
    /// When true, a terminal 502/504 against this model triggers read-only
    /// upstream diagnostics (DNS resolve + TCP connect) recorded as a trace
    /// span. Opt-in; off by default. Diagnose-only — never changes routing.
    pub debug_diagnostics: bool,
    /// Declared saturation for energy accounting: how many concurrent
    /// sequences of this model saturate one node (instances per node x
    /// sequences per instance). Each request is charged
    /// `node_watts / energy_slots_per_node` for its serving time.
    /// `0` (default) disables energy accounting for this model.
    pub energy_slots_per_node: i64,
    /// Per-model multiplier on the `auto` router's final score. `1.0` is
    /// neutral; above 1.0 prefers the model, below 1.0 de-prioritizes it.
    pub route_bias: f64,
    /// Whether the `auto` router may select this model. `false` removes it from
    /// auto's candidate pool while leaving it addressable by name — the
    /// distinction from `enabled = false`, which removes it everywhere.
    pub auto_eligible: bool,
    /// Which small model writes this model's speculation drafts. Empty falls
    /// back to the fleet default in the boon settings.
    pub draft_model: String,
    /// Direct (non-gateway) URL of a deployment of THIS model whose backend
    /// supports `prompt_logprobs`; it scores every draft token. Empty means
    /// this model cannot speculate.
    pub verify_api_base: String,
    /// The model name that scoring backend serves, when it differs from this
    /// model's own `upstream_model` (a canary serving its own name). Empty =
    /// same as `upstream_model`.
    pub verify_upstream_model: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// A model's operator-configured upstream headers as a header map, for every
/// outbound call to that model (the proxied request, helper calls, health
/// probes). Entries were validated when they were written; one that still
/// fails to parse is skipped rather than failing the request.
pub fn upstream_header_map(headers: &obleth_config::UpstreamHeaders) -> reqwest::header::HeaderMap {
    let mut out = reqwest::header::HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        if let (Ok(name), Ok(value)) = (
            reqwest::header::HeaderName::from_bytes(name.as_bytes()),
            reqwest::header::HeaderValue::from_str(value),
        ) {
            out.insert(name, value);
        }
    }
    out
}

fn secret_set(s: &Option<String>) -> bool {
    s.as_ref().is_some_and(|v| !v.is_empty())
}

fn views<T, V: From<T>>(items: Vec<T>) -> Vec<V> {
    items.into_iter().map(V::from).collect()
}

impl From<ModelRoute> for ModelRouteView {
    fn from(m: ModelRoute) -> Self {
        let ModelRoute {
            id,
            model_name,
            aliases,
            description,
            upstream_model,
            api_base,
            api_key,
            upstream_headers,
            model_type,
            quantization,
            input_cost_per_token,
            output_cost_per_token,
            cost_per_image,
            cost_per_audio_second,
            cost_per_character,
            cost_per_video,
            context_window,
            admission_weight,
            max_in_flight,
            capacity_mode,
            capacity_tuned_at,
            capacity_source,
            capacity_namespace,
            capacity_selector,
            per_replica_max_in_flight,
            capacity_headroom,
            supports_function_calling,
            supports_system_messages,
            supports_response_schema,
            supports_tool_choice,
            supports_vision,
            enabled,
            cache_enabled,
            cache_ttl_secs,
            tags,
            boons,
            tool_servers,
            request_timeout_secs,
            max_retries,
            retry_backoff_ms,
            endpoint_selection_mode,
            debug_diagnostics,
            energy_slots_per_node,
            route_bias,
            auto_eligible,
            draft_model,
            verify_api_base,
            verify_upstream_model,
            created_at,
            updated_at,
        } = m;
        ModelRouteView {
            id,
            model_name,
            aliases,
            description,
            upstream_model,
            api_base,
            api_key_set: secret_set(&api_key),
            upstream_header_names: obleth_config::upstream_header_names(&upstream_headers),
            model_type,
            quantization,
            input_cost_per_token,
            output_cost_per_token,
            cost_per_image,
            cost_per_audio_second,
            cost_per_character,
            cost_per_video,
            context_window,
            admission_weight,
            max_in_flight,
            capacity_mode,
            capacity_tuned_at,
            capacity_source,
            capacity_namespace,
            capacity_selector,
            per_replica_max_in_flight,
            capacity_headroom,
            supports_function_calling,
            supports_system_messages,
            supports_response_schema,
            supports_tool_choice,
            supports_vision,
            enabled,
            cache_enabled,
            cache_ttl_secs,
            tags,
            boons,
            tool_servers,
            request_timeout_secs,
            max_retries,
            retry_backoff_ms,
            endpoint_selection_mode,
            debug_diagnostics,
            energy_slots_per_node,
            route_bias,
            auto_eligible,
            draft_model,
            verify_api_base,
            verify_upstream_model,
            created_at,
            updated_at,
        }
    }
}

/// A model's upstream endpoint as returned by the Management API. The
/// endpoint `api_key` is never returned.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelEndpointView {
    pub id: Uuid,
    #[schema(value_type = String)]
    pub model_id: Uuid,
    pub name: String,
    pub api_base: String,
    /// Whether an endpoint API key is stored. The key itself is write-only.
    pub api_key_set: bool,
    pub priority: i64,
    pub weight: i64,
    pub enabled: bool,
    /// Requests this endpoint takes at once, counted by a `discovered` model
    /// using the `endpoints` source. `None` uses the model's
    /// `per_replica_max_in_flight`.
    pub max_in_flight: Option<i64>,
    pub health_status: String,
    pub consecutive_failures: i64,
    pub alert_state: String,
    pub last_checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_latency_ms: Option<i64>,
    pub last_http_status: Option<i64>,
    pub last_message: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<ModelEndpoint> for ModelEndpointView {
    fn from(e: ModelEndpoint) -> Self {
        let ModelEndpoint {
            id,
            model_id,
            name,
            api_base,
            api_key,
            priority,
            weight,
            enabled,
            max_in_flight,
            health_status,
            consecutive_failures,
            alert_state,
            last_checked_at,
            last_latency_ms,
            last_http_status,
            last_message,
            created_at,
            updated_at,
        } = e;
        ModelEndpointView {
            id,
            model_id,
            name,
            api_base,
            api_key_set: secret_set(&api_key),
            priority,
            weight,
            enabled,
            max_in_flight,
            health_status,
            consecutive_failures,
            alert_state,
            last_checked_at,
            last_latency_ms,
            last_http_status,
            last_message,
            created_at,
            updated_at,
        }
    }
}

/// A registered MCP server as returned by the Management API. The upstream
/// `auth_header` is never returned.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct McpServerView {
    pub id: Uuid,
    pub name: String,
    pub upstream_url: String,
    /// Whether an upstream Authorization header is stored. The value itself is
    /// write-only.
    pub auth_header_set: bool,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<McpServer> for McpServerView {
    fn from(s: McpServer) -> Self {
        let McpServer {
            id,
            name,
            upstream_url,
            auth_header,
            enabled,
            created_at,
            updated_at,
        } = s;
        McpServerView {
            id,
            name,
            upstream_url,
            auth_header_set: secret_set(&auth_header),
            enabled,
            created_at,
            updated_at,
        }
    }
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
        Some(url) if !url.is_empty() => {
            // Alert dispatch POSTs to this URL from the gateway; hold it to the
            // same destination policy as registered upstreams.
            state.ssrf.validate(url).await?;
            Some(url.to_string())
        }
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

/// Convert a persisted `TierSource` to the lowercase string used on the wire.
fn tier_source_as_str(t: obleth_config::TierSource) -> &'static str {
    match t {
        obleth_config::TierSource::Hybrid => "hybrid",
        obleth_config::TierSource::Derived => "derived",
        obleth_config::TierSource::Declared => "declared",
    }
}

/// Parse a wire string into a `TierSource`. Returns `None` for anything
/// unrecognized so callers can fall back to the existing value instead of
/// erroring the request.
fn parse_tier_source(s: &str) -> Option<obleth_config::TierSource> {
    match s {
        "hybrid" => Some(obleth_config::TierSource::Hybrid),
        "derived" => Some(obleth_config::TierSource::Derived),
        "declared" => Some(obleth_config::TierSource::Declared),
        _ => None,
    }
}

/// View of the persisted `auto` router classifier and scoring settings.
#[derive(Debug, Serialize, ToSchema)]
pub struct AutoRouterSettingsView {
    pub classifier_enabled: bool,
    pub classifier_model: Option<String>,
    pub classifier_timeout_ms: u64,
    /// The fixed tag vocabulary, surfaced so the UI can render tag pickers.
    pub available_tags: Vec<String>,
    pub capacity_weight: f64,
    pub cost_weight: f64,
    pub tag_weight: f64,
    pub default_soft_cap: u32,
    pub temperature: f64,
    pub difficulty_enabled: bool,
    /// `"hybrid" | "derived" | "declared"`.
    pub tier_source: String,
    /// Model or alias served for unknown model names on `/v1/messages`;
    /// `None` means such requests get `not_found_error`.
    pub messages_default_model: Option<String>,
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
            capacity_weight: s.capacity_weight,
            cost_weight: s.cost_weight,
            tag_weight: s.tag_weight,
            default_soft_cap: s.default_soft_cap,
            temperature: s.temperature,
            difficulty_enabled: s.difficulty_enabled,
            tier_source: tier_source_as_str(s.tier_source).to_string(),
            messages_default_model: s.messages_default_model.clone(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema, Default)]
pub struct UpdateAutoRouterSettings {
    #[serde(default)]
    pub classifier_enabled: Option<bool>,
    /// `model_name` of the classifier brain. Empty string clears it.
    #[serde(default)]
    pub classifier_model: Option<String>,
    #[serde(default)]
    pub classifier_timeout_ms: Option<u64>,
    #[serde(default)]
    pub capacity_weight: Option<f64>,
    #[serde(default)]
    pub cost_weight: Option<f64>,
    #[serde(default)]
    pub tag_weight: Option<f64>,
    #[serde(default)]
    pub default_soft_cap: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub difficulty_enabled: Option<bool>,
    /// `"hybrid" | "derived" | "declared"`. Unrecognized or absent values
    /// leave the persisted `tier_source` untouched.
    #[serde(default)]
    pub tier_source: Option<String>,
    /// Model or alias served for unknown model names on `/v1/messages`.
    /// Empty string clears it.
    #[serde(default)]
    pub messages_default_model: Option<String>,
}

/// Merge an update over the persisted settings, clamping out-of-range values.
/// Weights are clamped to [0,1]; temperature to [0,2]; soft cap must be >= 1.
/// Fields absent from the update (or unrecognized, for `tier_source`) keep
/// their existing persisted value rather than resetting to a default.
fn merge_auto_router(
    existing: &AutoRouterSettings,
    body: &UpdateAutoRouterSettings,
) -> AutoRouterSettings {
    let unit = |v: Option<f64>, cur: f64| v.map(|x| x.clamp(0.0, 1.0)).unwrap_or(cur);
    let classifier_model = match body.classifier_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.classifier_model.clone(),
    };
    let messages_default_model = match body.messages_default_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.messages_default_model.clone(),
    };
    AutoRouterSettings {
        classifier_enabled: body
            .classifier_enabled
            .unwrap_or(existing.classifier_enabled),
        classifier_model,
        classifier_timeout_ms: body
            .classifier_timeout_ms
            .filter(|ms| *ms > 0)
            .unwrap_or(existing.classifier_timeout_ms),
        capacity_weight: unit(body.capacity_weight, existing.capacity_weight),
        cost_weight: unit(body.cost_weight, existing.cost_weight),
        tag_weight: unit(body.tag_weight, existing.tag_weight),
        default_soft_cap: body
            .default_soft_cap
            .filter(|c| *c > 0)
            .unwrap_or(existing.default_soft_cap),
        temperature: body
            .temperature
            .map(|t| t.clamp(0.0, 2.0))
            .unwrap_or(existing.temperature),
        difficulty_enabled: body
            .difficulty_enabled
            .unwrap_or(existing.difficulty_enabled),
        tier_source: body
            .tier_source
            .as_deref()
            .and_then(parse_tier_source)
            .unwrap_or(existing.tier_source),
        messages_default_model,
    }
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

    let settings = merge_auto_router(&existing, &body);

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
                "capacity_weight": settings.capacity_weight,
                "cost_weight": settings.cost_weight,
                "tag_weight": settings.tag_weight,
                "default_soft_cap": settings.default_soft_cap,
                "temperature": settings.temperature,
                "difficulty_enabled": settings.difficulty_enabled,
                "tier_source": tier_source_as_str(settings.tier_source),
                "messages_default_model": settings.messages_default_model,
            }),
        )
        .await?;
    Ok(Json(AutoRouterSettingsView::from_settings(&settings)))
}

/// One hypothetical `auto` request for the routing tuner.
///
/// Everything is optional: an empty body simulates a bare, untagged prompt
/// against the saved settings. The weight fields are *overrides* — unset ones
/// fall back to whatever is persisted, exactly as a partial settings write
/// would.
#[derive(Debug, Deserialize, ToSchema, Default)]
pub struct SimulateRouteRequest {
    /// Plain prompt. Ignored when `messages` is present.
    #[serde(default)]
    pub prompt: Option<String>,
    /// An OpenAI-style `messages` array, for replaying a real request shape
    /// (multi-turn, or multimodal parts the vision heuristic keys off). Must be
    /// a JSON array when present.
    #[serde(default)]
    pub messages: Option<serde_json::Value>,
    /// Requested completion budget. Counts toward the context-window filter
    /// exactly as a real request's `max_tokens` does; absent means the request
    /// does not pin one.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Apply this tenant's model allowlist.
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// `low` | `medium` | `high`. Overrides the heuristic difficulty, and is
    /// reported as a `header` source because it is the same override the
    /// `x-obleth-effort` header performs on the data plane.
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub needs_function_calling: bool,
    /// The request pins a specific tool (`tool_choice` naming a function, or
    /// `function_call`), which only models with native tool-choice support can
    /// serve — no boon emulates it.
    #[serde(default)]
    pub needs_tool_choice: bool,
    #[serde(default)]
    pub needs_response_schema: bool,
    /// Weight overrides. Unset fields fall back to the saved settings.
    #[serde(default)]
    pub capacity_weight: Option<f64>,
    #[serde(default)]
    pub cost_weight: Option<f64>,
    #[serde(default)]
    pub tag_weight: Option<f64>,
    #[serde(default)]
    pub default_soft_cap: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub difficulty_enabled: Option<bool>,
    /// Pretend this many requests are in flight per model. **Absent means the
    /// live fleet load**, which is what the gateway itself scores against —
    /// this field is a what-if override, not the default.
    #[serde(default)]
    pub busyness: Option<std::collections::HashMap<String, usize>>,
    /// Pin the softmax draw in `[0,1)`. Absent means a fresh random draw, as on
    /// the request path. Pin it when comparing two simulations that differ only
    /// in their weights: at `temperature > 0` two independent draws make
    /// sampling noise look like an effect of the weight change. The draw that
    /// was used is echoed back as `uniform`.
    #[serde(default)]
    pub uniform: Option<f64>,
    /// Ask the LIVE intent classifier (the same brain, cache and timeout the
    /// data plane uses) instead of the keyword heuristic. Costs one small
    /// model call. Ignored — heuristics as before — when the classifier is
    /// disabled, unconfigured, or this admin runs without a data plane; the
    /// response's `tag_source` says which one actually ran.
    #[serde(default)]
    pub classify: bool,
}

/// Run the whole `auto` routing pipeline against the live fleet and return the
/// decision it *would* make, without dispatching anything.
///
/// Fidelity is the entire point, so this shares code with the data plane rather
/// than reimplementing it: candidates come from [`Store::build_candidates`] (the
/// same call the proxy's boot warm-up and 15s registry refresh make), busyness
/// defaults to the same `fairshare.model_load()` the request path scores
/// against, tokens are counted with the same `obleth-tokenizer` estimator that
/// feeds the real context-window filter, overrides are merged through the same
/// [`merge_auto_router`] that clamps a real settings write, and the verdict comes
/// from `explain_selection`, which is the same `evaluate` the proxy serves from.
/// A simulator that diverged on any of these would confidently show an operator
/// a decision the gateway would never make.
///
/// Every request field is an *override* of a live input, never a replacement
/// for one: omit `busyness` and the real fleet load is used, omit `uniform` and
/// a fresh draw is taken. The draw that was used is always echoed back, so two
/// simulations can be diffed without mistaking sampling noise for a weight
/// effect.
///
/// **This path never calls the classifier brain.** A simulation must not incur
/// model calls or upstream latency, so intent is derived from `heuristic_intent`
/// plus the optional `effort` override only. `tag_source`/`difficulty_source`
/// therefore report `heuristic` (or `header`), never `classifier` — the
/// dashboard must not imply a classification that did not happen. With the
/// classifier enabled, a real request's tags may differ from what is shown here.
///
/// Read-only: no audit entry is recorded, and nothing is written.
#[utoipa::path(
    post, path = "/api/v1/router/simulate", tag = "settings",
    request_body = SimulateRouteRequest,
    responses((status = 200, body = RouteExplain))
)]
async fn simulate_route(
    State(state): State<AdminState>,
    Json(body): Json<SimulateRouteRequest>,
) -> Result<Json<RouteExplain>> {
    // Saved settings with the request's overrides merged over them, clamped by
    // the same merge a `PUT /settings/auto-router` would use.
    let saved = state
        .store
        .get_auto_router_settings()
        .await?
        .unwrap_or_default();
    let settings = merge_auto_router(
        &saved,
        &UpdateAutoRouterSettings {
            capacity_weight: body.capacity_weight,
            cost_weight: body.cost_weight,
            tag_weight: body.tag_weight,
            default_soft_cap: body.default_soft_cap,
            temperature: body.temperature,
            difficulty_enabled: body.difficulty_enabled,
            ..Default::default()
        },
    );
    let weights = RouterWeights::from_settings(&settings);

    // Same candidate assembly, same tier derivation, as the data plane's
    // registry refresh — including health and maintenance state.
    let candidates = state.store.build_candidates(settings.tier_source).await?;

    // Tenant allowlist, when the caller wants to see a specific tenant's view.
    // An empty list is "no allowlist", matching the proxy's treatment.
    let allowed: Option<Vec<String>> = match body.tenant_id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => {
            let uuid = Uuid::parse_str(id)
                .map_err(|_| AdminError::BadRequest("tenant_id is not a uuid".to_string()))?;
            state
                .store
                .get_tenant(uuid)
                .await?
                .allowed_models
                .filter(|m| !m.is_empty())
        }
        _ => None,
    };

    let grants =
        BoonGrants::from_settings(&state.store.get_boon_settings().await?.unwrap_or_default());

    // Rebuild an OpenAI-style body so token counting and feature detection read
    // exactly what the data plane reads. A `messages` that is present but not an
    // array is the caller's mistake: silently simulating a blank prompt instead
    // would answer a question they did not ask, which is the worst thing to hand
    // someone debugging their routing.
    let messages = match body.messages {
        Some(serde_json::Value::Array(m)) => serde_json::Value::Array(m),
        Some(_) => {
            return Err(AdminError::BadRequest(
                "messages must be an array of chat messages".to_string(),
            ))
        }
        None => serde_json::json!([{
            "role": "user",
            "content": body.prompt.unwrap_or_default(),
        }]),
    };
    let max_tokens = body.max_tokens.unwrap_or(0);
    let mut json = serde_json::json!({ "model": "auto", "messages": messages });
    if max_tokens > 0 {
        json["max_tokens"] = serde_json::json!(max_tokens);
    }
    let est = obleth_tokenizer::HeuristicTokenizer::new().estimate_request(&json);
    let est_input_tokens = est.input_tokens as u64;
    let mut features = RequestFeatures::from_request(&json, est_input_tokens, max_tokens);
    // The three capability requirements the synthesized body cannot express on
    // its own, OR-ed on so an explicit flag can only ever tighten the filters.
    features.needs_function_calling |= body.needs_function_calling;
    features.needs_tool_choice |= body.needs_tool_choice;
    features.needs_response_schema |= body.needs_response_schema;

    let mut intent = None;
    if body.classify {
        if let Some(classify) = state.classify.as_ref() {
            let settings = state
                .store
                .get_auto_router_settings()
                .await?
                .unwrap_or_default();
            if settings.classifier_active() {
                // Same menu the data plane offers: only tags a candidate
                // actually carries. The prompt is the synthesized body's text,
                // which for a plain `prompt` is exactly what a client would
                // have sent.
                let mut menu: Vec<String> = Vec::new();
                for c in &candidates {
                    for t in &c.model.tags {
                        if !menu.contains(t) {
                            menu.push(t.clone());
                        }
                    }
                }
                let text = simulate_prompt_text(&json);
                if !menu.is_empty() && !text.trim().is_empty() {
                    let derived = classify(text, menu).await;
                    // Empty tags = the classifier's documented failure shape;
                    // fall back to heuristics exactly as the data plane does.
                    if !derived.tags.is_empty() {
                        intent = Some(derived);
                    }
                }
            }
        }
    }
    let mut intent = intent.unwrap_or_else(|| heuristic_intent(&json, est_input_tokens));
    if let Some(difficulty) = difficulty_from_header(body.effort.as_deref()) {
        intent.difficulty = difficulty;
        intent.source = IntentSource::Header;
    }

    // Spare capacity is one of the three terms an operator comes here to tune,
    // so the default must be the live fleet load the gateway itself scores
    // against — simulating a perfectly idle fleet would make `capacity_weight`
    // look inert. An explicit `busyness` is a what-if override.
    let busyness = body
        .busyness
        .unwrap_or_else(|| state.fairshare.model_load());
    // At the default temperature of 0 the draw is ignored and the pick is the
    // exact argmax. Above it production samples, so an unpinned simulation draws
    // too. A caller comparing two simulations must pin the same draw across
    // both, or the sampling difference reads as a weight effect; the draw used
    // is echoed back in the response either way.
    let uniform = match body.uniform {
        Some(u) if u.is_finite() => u.clamp(0.0, 0.999_999_999),
        Some(_) => {
            return Err(AdminError::BadRequest(
                "uniform must be a finite number in [0,1)".to_string(),
            ))
        }
        None => rand::thread_rng().gen_range(0.0..1.0),
    };

    Ok(Json(explain_selection(
        &candidates,
        &features,
        &busyness,
        &state.output_stats.snapshot(),
        allowed.as_deref(),
        &intent.tags,
        grants,
        &weights,
        uniform,
        &intent,
    )))
}

/// The classifier prompt for a simulated request: the system message (if any)
/// plus the first user message's text — the same compact shape the data
/// plane's classifier reads, so a simulated classification is of the same
/// prompt a live one would see.
fn simulate_prompt_text(json: &serde_json::Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
        let mut have_user = false;
        for msg in messages {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let text = match msg.get("content") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
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
    /// Wall-clock budget for one request's whole tool loop, in seconds.
    pub tool_loop_deadline_secs: u64,
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
    pub image_generation_enabled: bool,
    pub image_generation_model: Option<String>,
    pub image_generation_tool_description: String,
    pub image_generation_allowed_sizes: Vec<String>,
    pub image_generation_max_images_per_request: u32,
    pub image_generation_timeout_ms: u64,
    pub speculation_enabled: bool,
    pub speculation_draft_model: Option<String>,
    pub speculation_verify_model: Option<String>,
    pub speculation_classify_model: Option<String>,
    pub speculation_agree_min: f64,
    pub speculation_lp_min: f64,
    pub speculation_abort_agree: f64,
    pub speculation_abort_lp: f64,
    pub speculation_first_chunk_tokens: u32,
    pub speculation_chunk_tokens: u32,
    pub speculation_decide_by_tokens: u32,
    pub speculation_max_draft_tokens: u32,
    pub speculation_pace_ms: u64,
    pub speculation_timeout_ms: u64,
    /// `chat_template_kwargs` sent with draft calls, or null when unset.
    #[schema(value_type = Object)]
    pub speculation_draft_chat_template_kwargs: Option<serde_json::Value>,
    /// Per-category gates as `[{tag, speculate, agree_min, lp_min}]`.
    #[schema(value_type = Vec<Object>)]
    pub speculation_category_gates: serde_json::Value,
    pub speculation_unlisted_categories_speculate: bool,
    pub speculation_verify_url_template: String,
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
            tool_loop_deadline_secs: s.tool_loop.deadline_secs,
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
            image_generation_enabled: s.image_generation.enabled,
            image_generation_model: s.image_generation.image_model.clone(),
            image_generation_tool_description: s.image_generation.tool_description.clone(),
            image_generation_allowed_sizes: s.image_generation.allowed_sizes.clone(),
            image_generation_max_images_per_request: s.image_generation.max_images_per_request,
            image_generation_timeout_ms: s.image_generation.timeout_ms,
            speculation_enabled: s.speculation.enabled,
            speculation_draft_model: s.speculation.draft_model.clone(),
            speculation_verify_model: s.speculation.verify_model.clone(),
            speculation_classify_model: s.speculation.classify_model.clone(),
            speculation_agree_min: s.speculation.agree_min,
            speculation_lp_min: s.speculation.lp_min,
            speculation_abort_agree: s.speculation.abort_agree,
            speculation_abort_lp: s.speculation.abort_lp,
            speculation_first_chunk_tokens: s.speculation.first_chunk_tokens,
            speculation_chunk_tokens: s.speculation.chunk_tokens,
            speculation_decide_by_tokens: s.speculation.decide_by_tokens,
            speculation_max_draft_tokens: s.speculation.max_draft_tokens,
            speculation_pace_ms: s.speculation.pace_ms,
            speculation_timeout_ms: s.speculation.timeout_ms,
            speculation_draft_chat_template_kwargs: s
                .speculation
                .draft_chat_template_kwargs
                .clone(),
            speculation_category_gates: serde_json::to_value(&s.speculation.category_gates)
                .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
            speculation_unlisted_categories_speculate: s.speculation.unlisted_categories_speculate,
            speculation_verify_url_template: s.speculation.verify_url_template.clone(),
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
    /// Wall-clock budget for one request's whole tool loop, in seconds.
    /// Clamped to `1..=3600` (`TOOL_LOOP_MAX_DEADLINE_SECS`); omit (or send 0)
    /// to leave unchanged.
    #[serde(default)]
    pub tool_loop_deadline_secs: Option<u64>,
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
    /// Enable or disable the image-generation boon globally. Omit to leave unchanged.
    #[serde(default)]
    pub image_generation_enabled: Option<bool>,
    /// `model_name` of the registered `image`-type model that serves
    /// generations. Empty string clears it (which deactivates the boon).
    #[serde(default)]
    pub image_generation_model: Option<String>,
    /// Tool description the model reads. Empty string resets it to the built-in
    /// default; omit the field to leave it unchanged.
    #[serde(default)]
    pub image_generation_tool_description: Option<String>,
    /// Sizes offered in the tool schema. An empty or all-blank list resets it to
    /// the built-in default.
    #[serde(default)]
    pub image_generation_allowed_sizes: Option<Vec<String>>,
    /// Images per tool call, clamped to `IMAGE_GENERATION_MAX_PER_REQUEST`.
    /// A value of `0` is a no-op and leaves the existing setting unchanged.
    #[serde(default)]
    pub image_generation_max_images_per_request: Option<u32>,
    /// Generation timeout (ms). Omit/zero leaves unchanged.
    #[serde(default)]
    pub image_generation_timeout_ms: Option<u64>,
    /// Enable or disable the speculation boon globally. Omit to leave unchanged.
    #[serde(default)]
    pub speculation_enabled: Option<bool>,
    /// `model_name` of the registered chat model that drafts. Empty string
    /// clears it (which deactivates the boon).
    #[serde(default)]
    pub speculation_draft_model: Option<String>,
    /// `model_name` of the registered model that scores drafts via
    /// prompt_logprobs. Empty string clears it (which deactivates the boon).
    #[serde(default)]
    pub speculation_verify_model: Option<String>,
    /// `model_name` of the tiny classify model for per-category gates. Empty
    /// string clears it (the default gate then applies to everything).
    #[serde(default)]
    pub speculation_classify_model: Option<String>,
    /// Ship floor on rank-1 agreement, in `[0,1]`. Omit to leave unchanged.
    #[serde(default)]
    pub speculation_agree_min: Option<f64>,
    /// Ship floor on mean logprob (≤ 0). Omit to leave unchanged.
    #[serde(default)]
    pub speculation_lp_min: Option<f64>,
    /// Abort floor on agreement, in `[0,1]`. Omit to leave unchanged.
    #[serde(default)]
    pub speculation_abort_agree: Option<f64>,
    /// Abort floor on mean logprob (≤ 0). Omit to leave unchanged.
    #[serde(default)]
    pub speculation_abort_lp: Option<f64>,
    /// Draft tokens before the first verification. Omit/zero leaves unchanged.
    #[serde(default)]
    pub speculation_first_chunk_tokens: Option<u32>,
    /// Draft tokens between verifications. Omit/zero leaves unchanged.
    #[serde(default)]
    pub speculation_chunk_tokens: Option<u32>,
    /// Defer patience in draft tokens. Omit/zero leaves unchanged.
    #[serde(default)]
    pub speculation_decide_by_tokens: Option<u32>,
    /// Drafter max_tokens cap. Omit/zero leaves unchanged.
    #[serde(default)]
    pub speculation_max_draft_tokens: Option<u32>,
    /// Milliseconds between released deltas (0 = burst). Omit leaves unchanged.
    #[serde(default)]
    pub speculation_pace_ms: Option<u64>,
    /// Pre-release wall-clock budget (ms). Omit/zero leaves unchanged.
    #[serde(default)]
    pub speculation_timeout_ms: Option<u64>,
    /// `chat_template_kwargs` for draft calls. An empty object `{}` clears it;
    /// omit (or null) to leave unchanged.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub speculation_draft_chat_template_kwargs: Option<serde_json::Value>,
    /// Replaces the per-category gate list. Entries are
    /// `{tag, speculate?, agree_min?, lp_min?}`; an empty list clears all
    /// per-category gates. Omit to leave unchanged.
    #[serde(default)]
    #[schema(value_type = Vec<Object>)]
    pub speculation_category_gates: Option<serde_json::Value>,
    /// Whether unlisted categories use the default gate (true) or abstain
    /// (false). Omit to leave unchanged.
    #[serde(default)]
    pub speculation_unlisted_categories_speculate: Option<bool>,
    /// Fleet rule for locating scoring endpoints ({upstream}/{model}
    /// placeholders). Empty string clears; omitted keeps the current value.
    #[serde(default)]
    pub speculation_verify_url_template: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/settings/boons", tag = "settings",
    responses((status = 200, body = BoonSettingsView))
)]
async fn get_boon_settings(State(state): State<AdminState>) -> Result<Json<BoonSettingsView>> {
    let settings = state.store.get_boon_settings().await?.unwrap_or_default();
    Ok(Json(BoonSettingsView::from_settings(&settings)))
}

/// Trim, drop blanks, de-duplicate order-stably, and fall back to the built-in
/// list when nothing usable remains — an empty `enum` in the tool schema would
/// make the `size` argument unsatisfiable.
fn normalize_image_sizes(sizes: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for size in sizes {
        let s = size.trim().to_string();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    }
    if out.is_empty() {
        obleth_config::ImageGenerationBoonSettings::default().allowed_sizes
    } else {
        out
    }
}

/// Images per call, bounded by the hard ceiling.
fn clamp_image_count(n: u32) -> u32 {
    n.clamp(1, obleth_config::IMAGE_GENERATION_MAX_PER_REQUEST)
}

/// Apply a `tool_loop_deadline_secs` update: `None`/0 keeps `existing`, and any
/// other value is clamped to `1..=TOOL_LOOP_MAX_DEADLINE_SECS`. Unbounded, a huge
/// value overflows the loop's deadline arithmetic on the request path.
fn merge_tool_loop_deadline(update: Option<u64>, existing: u64) -> u64 {
    update
        .filter(|s| *s > 0)
        .map(|s| s.min(TOOL_LOOP_MAX_DEADLINE_SECS))
        .unwrap_or(existing)
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
    let verify_template_supplied = body.speculation_verify_url_template.is_some();

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

    let image_model = match body.image_generation_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.image_generation.image_model.clone(),
    };
    // Reject a misconfiguration here rather than leaving it to surface as a
    // warn line at request time: the boon fails open, so a bad model name is
    // otherwise invisible until someone notices no images are ever produced.
    //
    // Only validate when the caller actually supplied the field: `image_model`
    // above falls back to the *existing* persisted value when it is omitted,
    // and re-validating that carried-forward value here would mean a PUT that
    // only touches an unrelated setting (e.g. toggling vision) starts failing
    // the moment the configured image model is deleted or renamed elsewhere.
    if body.image_generation_model.is_some() {
        if let Some(name) = image_model.as_deref() {
            match state.store.get_model_by_name(name).await {
                Ok(model) if model.model_type == "image" => {}
                Ok(model) => {
                    return Err(AdminError::BadRequest(format!(
                        "image_generation_model `{name}` has model_type `{}`; it must be `image`",
                        model.model_type
                    )))
                }
                // A genuinely missing model is the caller's mistake.
                Err(obleth_store::StoreError::NotFound) => {
                    return Err(AdminError::BadRequest(format!(
                        "image_generation_model `{name}` is not a registered model"
                    )))
                }
                // Any other store error (a transient DB failure, say) is not a
                // bad model name and must not be reported as one — that would
                // invite an operator to "fix" a perfectly good setting.
                Err(e) => return Err(e.into()),
            }
        }
    }

    let image_tool_description = match body
        .image_generation_tool_description
        .as_deref()
        .map(str::trim)
    {
        Some("") => obleth_config::DEFAULT_IMAGE_TOOL_DESCRIPTION.to_string(),
        Some(d) => d.to_string(),
        None => existing.image_generation.tool_description.clone(),
    };

    // Speculation helper models: same merge-and-validate shape as
    // `image_generation_model` above — validate only fields the caller
    // actually supplied, and require registered chat models.
    let spec_draft_model = match body.speculation_draft_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.speculation.draft_model.clone(),
    };
    let spec_verify_model = match body.speculation_verify_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.speculation.verify_model.clone(),
    };
    let spec_classify_model = match body.speculation_classify_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.speculation.classify_model.clone(),
    };
    for (field, supplied, value) in [
        (
            "speculation_draft_model",
            body.speculation_draft_model.is_some(),
            spec_draft_model.as_deref(),
        ),
        (
            "speculation_verify_model",
            body.speculation_verify_model.is_some(),
            spec_verify_model.as_deref(),
        ),
        (
            "speculation_classify_model",
            body.speculation_classify_model.is_some(),
            spec_classify_model.as_deref(),
        ),
    ] {
        if !supplied {
            continue;
        }
        if let Some(name) = value {
            match state.store.get_model_by_name(name).await {
                Ok(model) if model.model_type == "chat" => {}
                Ok(model) => {
                    return Err(AdminError::BadRequest(format!(
                        "{field} `{name}` has model_type `{}`; it must be `chat`",
                        model.model_type
                    )))
                }
                Err(obleth_store::StoreError::NotFound) => {
                    return Err(AdminError::BadRequest(format!(
                        "{field} `{name}` is not a registered model"
                    )))
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    let spec_category_gates = match body.speculation_category_gates {
        Some(v) => serde_json::from_value::<Vec<obleth_config::SpeculationCategoryGate>>(v)
            .map_err(|e| {
                AdminError::BadRequest(format!(
                    "speculation_category_gates must be a list of {{tag, speculate?, agree_min?, lp_min?}}: {e}"
                ))
            })?,
        None => existing.speculation.category_gates.clone(),
    };
    let spec_draft_kwargs = match body.speculation_draft_chat_template_kwargs {
        // An empty object clears; any other object replaces; omit keeps.
        Some(v) if v.as_object().is_some_and(|o| o.is_empty()) => None,
        Some(v) if v.is_object() => Some(v),
        Some(v) if v.is_null() => existing.speculation.draft_chat_template_kwargs.clone(),
        Some(_) => {
            return Err(AdminError::BadRequest(
                "speculation_draft_chat_template_kwargs must be a JSON object".to_string(),
            ))
        }
        None => existing.speculation.draft_chat_template_kwargs.clone(),
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
            deadline_secs: merge_tool_loop_deadline(
                body.tool_loop_deadline_secs,
                existing.tool_loop.deadline_secs,
            ),
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
        // No admin-API fields for the knowledge boon yet (Task 7+); carry the
        // persisted value through unchanged, same as `guardrails` above.
        knowledge: existing.knowledge.clone(),
        image_generation: obleth_config::ImageGenerationBoonSettings {
            enabled: body
                .image_generation_enabled
                .unwrap_or(existing.image_generation.enabled),
            image_model,
            tool_description: image_tool_description,
            allowed_sizes: match body.image_generation_allowed_sizes {
                Some(sizes) => normalize_image_sizes(sizes),
                None => existing.image_generation.allowed_sizes.clone(),
            },
            max_images_per_request: body
                .image_generation_max_images_per_request
                .filter(|n| *n > 0)
                .map(clamp_image_count)
                .unwrap_or(existing.image_generation.max_images_per_request),
            timeout_ms: body
                .image_generation_timeout_ms
                .filter(|ms| *ms > 0)
                .unwrap_or(existing.image_generation.timeout_ms),
        },
        speculation: obleth_config::SpeculationBoonSettings {
            enabled: body
                .speculation_enabled
                .unwrap_or(existing.speculation.enabled),
            draft_model: spec_draft_model,
            verify_model: spec_verify_model,
            classify_model: spec_classify_model,
            agree_min: body
                .speculation_agree_min
                .filter(|v| (0.0..=1.0).contains(v))
                .unwrap_or(existing.speculation.agree_min),
            lp_min: body
                .speculation_lp_min
                .filter(|v| *v <= 0.0)
                .unwrap_or(existing.speculation.lp_min),
            abort_agree: body
                .speculation_abort_agree
                .filter(|v| (0.0..=1.0).contains(v))
                .unwrap_or(existing.speculation.abort_agree),
            abort_lp: body
                .speculation_abort_lp
                .filter(|v| *v <= 0.0)
                .unwrap_or(existing.speculation.abort_lp),
            first_chunk_tokens: body
                .speculation_first_chunk_tokens
                .filter(|n| *n > 0)
                .unwrap_or(existing.speculation.first_chunk_tokens),
            chunk_tokens: body
                .speculation_chunk_tokens
                .filter(|n| *n > 0)
                .unwrap_or(existing.speculation.chunk_tokens),
            decide_by_tokens: body
                .speculation_decide_by_tokens
                .filter(|n| *n > 0)
                .unwrap_or(existing.speculation.decide_by_tokens),
            max_draft_tokens: body
                .speculation_max_draft_tokens
                .filter(|n| *n > 0)
                .unwrap_or(existing.speculation.max_draft_tokens),
            pace_ms: body
                .speculation_pace_ms
                .unwrap_or(existing.speculation.pace_ms),
            draft_chat_template_kwargs: spec_draft_kwargs,
            timeout_ms: body
                .speculation_timeout_ms
                .filter(|ms| *ms > 0)
                .unwrap_or(existing.speculation.timeout_ms),
            category_gates: spec_category_gates,
            unlisted_categories_speculate: body
                .speculation_unlisted_categories_speculate
                .unwrap_or(existing.speculation.unlisted_categories_speculate),
            verify_url_template: body
                .speculation_verify_url_template
                .map(|t| t.trim().to_string())
                .unwrap_or_else(|| existing.speculation.verify_url_template.clone()),
        },
    };

    // The speculation verifier sends each model's upstream key to this URL, so
    // hold it to the upstream destination policy (placeholders are refused in
    // the scheme/credentials; a filled host is re-checked by the proxy). Only
    // a newly supplied value is checked, so an unrelated save isn't blocked by
    // DNS for a stored one.
    let template = settings.speculation.verify_url_template.as_str();
    if verify_template_supplied && !template.is_empty() {
        state
            .ssrf
            .validate_template(template, ssrf::VERIFY_TEMPLATE_PLACEHOLDERS)
            .await
            .map_err(|e| AdminError::BadRequest(format!("speculation_verify_url_template: {e}")))?;
    }
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
                "tool_loop_deadline_secs": settings.tool_loop.deadline_secs,
                "tool_loop_nudge_len": settings.tool_loop.nudge.len(),
                "image_generation_enabled": settings.image_generation.enabled,
                "image_generation_model": settings.image_generation.image_model,
                "image_generation_allowed_sizes": settings.image_generation.allowed_sizes,
                "image_generation_max_images_per_request": settings.image_generation.max_images_per_request,
                "image_generation_timeout_ms": settings.image_generation.timeout_ms,
                "speculation_enabled": settings.speculation.enabled,
                "speculation_draft_model": settings.speculation.draft_model,
                "speculation_verify_model": settings.speculation.verify_model,
                "speculation_classify_model": settings.speculation.classify_model,
                "speculation_agree_min": settings.speculation.agree_min,
                "speculation_lp_min": settings.speculation.lp_min,
                "speculation_category_gates": settings.speculation.category_gates.len(),
                "speculation_unlisted_categories_speculate": settings.speculation.unlisted_categories_speculate,
                "speculation_verify_url_template": settings.speculation.verify_url_template,
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
    // The poller queries this URL from the gateway; apply the same destination
    // policy the test route enforces so a save can't bypass it.
    if let Some(url) = body.prometheus_url.as_deref().map(str::trim) {
        if !url.is_empty() {
            state.ssrf.validate(url).await?;
        }
    }
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
    state.ssrf.validate(&body.prometheus_url).await?;
    let q = body.power_query.trim();
    let base = body.prometheus_url.trim();
    let http = ssrf::upstream_client_builder()
        .build()
        .map_err(|e| AdminError::BadRequest(format!("could not initialize HTTP client: {e}")))?;
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

/// Charo assistant settings surfaced to the dashboard. Mirrors
/// `obleth_config::CharoSettings` exactly.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CharoSettingsView {
    pub enabled: bool,
    pub brain_model: Option<String>,
    #[serde(default)]
    pub tools_enabled: std::collections::BTreeMap<String, bool>,
    pub bench_max_concurrency: u32,
    pub bench_max_duration_s: u32,
    pub bench_max_requests: u32,
}

impl From<obleth_config::CharoSettings> for CharoSettingsView {
    fn from(s: obleth_config::CharoSettings) -> Self {
        CharoSettingsView {
            enabled: s.enabled,
            brain_model: s.brain_model,
            tools_enabled: s.tools_enabled,
            bench_max_concurrency: s.bench_max_concurrency,
            bench_max_duration_s: s.bench_max_duration_s,
            bench_max_requests: s.bench_max_requests,
        }
    }
}

impl From<CharoSettingsView> for obleth_config::CharoSettings {
    fn from(v: CharoSettingsView) -> Self {
        obleth_config::CharoSettings {
            enabled: v.enabled,
            brain_model: v.brain_model,
            tools_enabled: v.tools_enabled,
            bench_max_concurrency: v.bench_max_concurrency,
            bench_max_duration_s: v.bench_max_duration_s,
            bench_max_requests: v.bench_max_requests,
        }
    }
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
    let settings = state.store.get_charo_settings().await?;
    Ok(Json(settings.into()))
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
    let settings: obleth_config::CharoSettings = body.into();
    state.store.set_charo_settings(&settings).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "set_charo_settings",
            "settings",
            "charo",
            serde_json::json!(settings),
        )
        .await?;
    Ok(Json(settings.into()))
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
    let evicted = evict_keys(&state, &hashes, "the tenant and its keys").await;
    // The Postgres delete stands either way, so it is audited before an
    // eviction failure is reported.
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "delete_tenant",
            "tenant",
            &id.to_string(),
            serde_json::json!({ "keys_removed": hashes.len(), "cache_evicted": evicted.is_ok() }),
        )
        .await?;
    evicted?;
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
    if body.weight < 1 {
        return Err(AdminError::BadRequest("weight must be >= 1".into()));
    }
    if matches!(body.max_in_flight, Some(c) if c < 1) {
        return Err(AdminError::BadRequest("max_in_flight must be >= 1".into()));
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
            body.weight,
            body.max_in_flight,
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
                "weight": key.weight,
                "max_in_flight": key.max_in_flight,
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
    if body.weight < 1 {
        return Err(AdminError::BadRequest("weight must be >= 1".into()));
    }
    if matches!(body.max_in_flight, Some(c) if c < 1) {
        return Err(AdminError::BadRequest("max_in_flight must be >= 1".into()));
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
            body.weight,
            body.max_in_flight,
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
                "weight": key.weight,
                "max_in_flight": key.max_in_flight,
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
    let evicted = evict_keys(&state, std::slice::from_ref(&hash), "the key").await;
    // Audited before an eviction failure is reported: the row is gone.
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "delete_key",
            "api_key",
            &id.to_string(),
            serde_json::json!({ "cache_evicted": evicted.is_ok() }),
        )
        .await?;
    evicted?;
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

#[utoipa::path(
    put, path = "/api/v1/keys/{id}/tracing", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    request_body = SetKeyTracing,
    responses((status = 204), (status = 404))
)]
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

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/tracing", tag = "tenants",
    params(("id" = Uuid, Path, description = "Tenant id")),
    request_body = SetKeyTracing,
    responses((status = 204), (status = 404))
)]
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
    put, path = "/api/v1/tenants/{id}/synthetic", tag = "tenants",
    params(("id" = Uuid, Path, description = "Tenant id")),
    request_body = SetTenantSynthetic,
    responses((status = 204))
)]
async fn set_tenant_synthetic_handler(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetTenantSynthetic>,
) -> Result<StatusCode> {
    state.store.set_tenant_synthetic(id, body.synthetic).await?;
    // The flag is denormalized into every resolved key; re-push the tenant's keys.
    sync_tenant_keys(&state, id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "set_tenant_synthetic",
            "tenant",
            &id.to_string(),
            serde_json::json!({ "synthetic": body.synthetic }),
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

    let summary = usage::query_key_usage_summary(
        &state.clickhouse,
        key.tenant_id,
        id,
        q.since_ms,
        q.include_internal,
    )
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
        usage::query_costs(&state.clickhouse, q.since_ms, q.include_internal, &costs).await?,
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

#[utoipa::path(
    get, path = "/api/v1/usage/logs/{request_id}/spans", tag = "usage",
    params(("request_id" = Uuid, Path, description = "Request id")),
    responses((status = 200, body = [usage::SpanEntry]))
)]
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
    let rows = usage::query_usage_breakdown_by_model(
        &state.clickhouse,
        &q.model,
        q.since_ms,
        q.limit,
        q.include_internal,
    )
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
    if body.days > usage_retention::MAX_RETENTION_DAYS {
        return Err(AdminError::BadRequest(format!(
            "retention days must be at most {}",
            usage_retention::MAX_RETENTION_DAYS
        )));
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
    get, path = "/api/v1/overview/summary", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = OverviewSummaryView))
)]
async fn get_overview_summary(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<OverviewSummaryView>> {
    let counts = state.store.overview_counts().await?;
    // ClickHouse being down degrades the usage totals to zero rather than
    // failing the whole summary — the config counts are still useful and this
    // read backs a poll on every dashboard page.
    let totals = usage::query_usage_totals(&state.clickhouse, q.since_ms, q.include_internal)
        .await
        .unwrap_or_default();
    // The ledger keeps rows for since-deleted tenants; clamp so the summary
    // can never report more active tenants than currently exist.
    let active_tenants = totals.active_tenants.min(counts.tenant_count.max(0) as u64);
    Ok(Json(OverviewSummaryView {
        requests: totals.requests,
        tokens: totals.total_tokens,
        cost: totals.cost_usd,
        has_pricing: counts.has_pricing,
        tenant_count: counts.tenant_count,
        active_tenants,
        model_count: counts.model_count,
        enabled_models: counts.enabled_models,
        key_count: counts.key_count,
    }))
}

/// Slots the gateway can actually run: the sum of the enabled models' pool
/// sizes, each model's `max_in_flight` or the gateway default.
pub fn enabled_pool_capacity(models: &[ModelRoute], default_cap: usize) -> usize {
    enabled_pool_share(models, default_cap, 1)
}

/// Slots one of `replicas` live replicas can run: its share of each enabled
/// model's pool size, summed. Each share rounds up on its own, so this can
/// exceed `enabled_pool_capacity / replicas` by up to one slot per model.
pub fn enabled_pool_share(models: &[ModelRoute], default_cap: usize, replicas: usize) -> usize {
    enabled_pool_share_with(models, default_cap, replicas, &Default::default())
}

/// [`enabled_pool_share`] with the pool sizes discovery set for some models
/// (see [`capacity_discovery`]) in place of their `max_in_flight`.
pub fn enabled_pool_share_with(
    models: &[ModelRoute],
    default_cap: usize,
    replicas: usize,
    discovered: &std::collections::HashMap<String, usize>,
) -> usize {
    models
        .iter()
        .filter(|m| m.enabled)
        .map(|m| {
            let configured = discovered.get(&m.model_name).copied().unwrap_or_else(|| {
                m.max_in_flight
                    .and_then(|c| usize::try_from(c).ok())
                    .filter(|c| *c > 0)
                    .unwrap_or(default_cap)
            });
            replica_share(configured, replicas)
        })
        .sum()
}

/// Why the global ceiling would bind before the pools do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CeilingWarning {
    /// The ceiling compared, configured or per replica to match `pool_sum`.
    pub ceiling: usize,
    pub pool_sum: usize,
    pub message: &'static str,
}

/// Start-up check that the global ceiling sits above the pools, comparing like
/// with like. Configured ceiling against configured pool sum first: that is
/// the operator's fleet-wide intent. Then this replica's share of each at the
/// live count, since the shares round up per pool and many small pools can
/// push their sum past the ceiling's share even when the configured numbers
/// fit. With one replica the two checks are the same.
pub fn ceiling_check(
    models: &[ModelRoute],
    default_cap: usize,
    ceiling: usize,
    replicas: usize,
) -> Option<CeilingWarning> {
    let pool_sum = enabled_pool_capacity(models, default_cap);
    if ceiling < pool_sum {
        return Some(CeilingWarning {
            ceiling,
            pool_sum,
            message: "OBLETH_GLOBAL_MAX_IN_FLIGHT is below the sum of the enabled models' pool \
                      sizes; the ceiling will bind first and pools will be served round-robin \
                      — raise it above the pool sum",
        });
    }
    let share_sum = enabled_pool_share(models, default_cap, replicas);
    let ceiling_share = replica_share(ceiling, replicas);
    if ceiling_share < share_sum {
        return Some(CeilingWarning {
            ceiling: ceiling_share,
            pool_sum: share_sum,
            message: "each replica's share of OBLETH_GLOBAL_MAX_IN_FLIGHT is below the sum of \
                      its pool shares, which round up per model; the ceiling will bind first \
                      on every replica — raise it by at least one slot per enabled model per \
                      replica above the pool sum",
        });
    }
    None
}

/// Fold per-pool views into one all-models view. Occupancy, backlog, served
/// tokens and expected slots add; `weight_share` becomes expected slots over
/// total pool capacity so it stays in [0, 1].
pub(crate) fn aggregate_pools(
    pools: &[ModelPoolView],
) -> (
    Vec<GroupFairshareView>,
    Vec<TenantFairshareView>,
    Vec<KeyFairshareView>,
) {
    use std::collections::BTreeMap;
    let total_cap: usize = pools.iter().map(|p| p.cap).sum();
    let share = |expected: f64| {
        if total_cap > 0 {
            expected / total_cap as f64
        } else {
            0.0
        }
    };

    let mut groups: BTreeMap<String, GroupFairshareView> = BTreeMap::new();
    let mut tenants: BTreeMap<Uuid, TenantFairshareView> = BTreeMap::new();
    let mut keys: BTreeMap<Uuid, KeyFairshareView> = BTreeMap::new();
    for pool in pools {
        for g in &pool.groups {
            let e = groups
                .entry(g.name.clone())
                .or_insert_with(|| GroupFairshareView {
                    name: g.name.clone(),
                    weight: g.weight,
                    in_flight: 0,
                    queued: 0,
                    slot_cap: 0,
                    borrowed: 0,
                    served_tokens: 0.0,
                    share_score: 0.0,
                    weight_share: 0.0,
                    expected_slots: 0.0,
                });
            e.in_flight += g.in_flight;
            e.queued += g.queued;
            e.slot_cap += g.slot_cap;
            e.borrowed += g.borrowed;
            e.served_tokens += g.served_tokens;
            e.expected_slots += g.expected_slots;
        }
        for t in &pool.tenants {
            let e = tenants
                .entry(t.tenant_id)
                .or_insert_with(|| TenantFairshareView {
                    tenant_id: t.tenant_id,
                    name: t.name.clone(),
                    fairshare_group: t.fairshare_group.clone(),
                    weight: t.weight,
                    max_in_flight: t.max_in_flight,
                    in_flight: 0,
                    queued: 0,
                    served_tokens: 0.0,
                    share_score: 0.0,
                    weight_share: 0.0,
                    expected_slots: 0.0,
                });
            e.in_flight += t.in_flight;
            e.queued += t.queued;
            e.served_tokens += t.served_tokens;
            e.expected_slots += t.expected_slots;
        }
        for k in &pool.keys {
            let e = keys.entry(k.key_id).or_insert_with(|| KeyFairshareView {
                key_id: k.key_id,
                tenant_id: k.tenant_id,
                name: k.name.clone(),
                weight: k.weight,
                max_in_flight: k.max_in_flight,
                in_flight: 0,
                queued: 0,
                served_tokens: 0.0,
                share_score: 0.0,
                weight_share: 0.0,
                expected_slots: 0.0,
            });
            e.in_flight += k.in_flight;
            e.queued += k.queued;
            e.served_tokens += k.served_tokens;
            e.expected_slots += k.expected_slots;
        }
    }
    let by_score = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
    let mut groups: Vec<_> = groups
        .into_values()
        .map(|mut g| {
            g.share_score = g.served_tokens / g.weight.max(1) as f64;
            g.weight_share = share(g.expected_slots);
            g
        })
        .collect();
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    let mut tenants: Vec<_> = tenants
        .into_values()
        .map(|mut t| {
            t.share_score = t.served_tokens / t.weight.max(1) as f64;
            t.weight_share = share(t.expected_slots);
            t
        })
        .collect();
    tenants.sort_by(|a, b| by_score(a.share_score, b.share_score));
    let mut keys: Vec<_> = keys
        .into_values()
        .map(|mut k| {
            k.share_score = k.served_tokens / k.weight.max(1) as f64;
            k.weight_share = share(k.expected_slots);
            k
        })
        .collect();
    keys.sort_by(|a, b| by_score(a.share_score, b.share_score));
    (groups, tenants, keys)
}

#[utoipa::path(
    get, path = "/api/v1/stats", tag = "usage",
    responses((status = 200, body = LiveStats))
)]
async fn get_stats(State(state): State<AdminState>) -> Json<LiveStats> {
    use obleth_fairshare::CapacityProvider;
    use std::sync::atomic::Ordering;
    // The live counters are the point of this endpoint; a database hiccup
    // should degrade the capacity number, not fail the poll.
    let replicas = state
        .fairshare_stats
        .replicas
        .load(Ordering::Relaxed)
        .max(1);
    let discovered = state.capacity_discovery.effective_caps();
    let capacity = state
        .store
        .list_models()
        .await
        .map(|m| {
            enabled_pool_share_with(&m, state.default_model_max_in_flight, replicas, &discovered)
        })
        .unwrap_or(0);
    Json(LiveStats {
        in_flight: state.fairshare_stats.in_flight.load(Ordering::Relaxed),
        queued: state.fairshare_stats.queued.load(Ordering::Relaxed),
        max_in_flight: if capacity > 0 {
            capacity
        } else {
            replica_share(state.capacity.max_in_flight(), replicas)
        },
        replicas,
    })
}

/// This replica's capacity discovery: its settings and every `discovered`
/// model's state.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CapacityDiscoveryView {
    /// `OBLETH_CAPACITY_DISCOVERY_ENABLED` on this replica.
    pub enabled: bool,
    pub interval_secs: u64,
    /// Namespaces the `kubernetes` source may read. Empty: that source is
    /// unavailable.
    pub namespaces: Vec<String>,
    /// Selector template for `kubernetes` models that set none.
    pub default_selector: String,
    /// Live gateway replicas each pool size is divided across.
    pub replicas: usize,
    pub models: Vec<CapacityDiscoveryModelView>,
}

/// One `discovered` model.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CapacityDiscoveryModelView {
    pub model_id: Uuid,
    pub model_name: String,
    pub enabled: bool,
    /// The model's own `max_in_flight`: the fallback.
    pub static_max_in_flight: Option<i64>,
    /// What this replica enforces: its share of
    /// `status.effective_max_in_flight`.
    pub replica_share: usize,
    pub status: capacity_discovery::ModelCapacityStatus,
}

#[utoipa::path(
    get, path = "/api/v1/capacity/discovery", tag = "models",
    responses((status = 200, body = CapacityDiscoveryView))
)]
async fn get_capacity_discovery(
    State(state): State<AdminState>,
) -> Result<Json<CapacityDiscoveryView>> {
    use std::sync::atomic::Ordering;
    let settings = state.capacity_discovery.settings().clone();
    let replicas = state
        .fairshare_stats
        .replicas
        .load(Ordering::Relaxed)
        .max(1);
    let statuses: std::collections::HashMap<String, capacity_discovery::ModelCapacityStatus> =
        state
            .capacity_discovery
            .statuses()
            .into_iter()
            .map(|s| (s.model_name.clone(), s))
            .collect();
    let models = state
        .store
        .list_models()
        .await?
        .into_iter()
        .filter(|m| m.capacity_mode == obleth_config::DISCOVERED_CAPACITY_MODE)
        .map(|m| {
            let status = statuses.get(&m.model_name).cloned().unwrap_or_else(|| {
                // Not evaluated by this replica (yet): say why, with the value
                // fairshare is using meanwhile.
                let reason = if !settings.enabled {
                    "capacity discovery is off on this gateway \
                     (OBLETH_CAPACITY_DISCOVERY_ENABLED)"
                } else if !m.enabled {
                    "the model is disabled"
                } else {
                    "waiting for the next discovery pass"
                };
                capacity_discovery::ModelCapacityStatus {
                    model_name: m.model_name.clone(),
                    source: m.capacity_source.clone(),
                    namespaces: Vec::new(),
                    selector: m.capacity_selector.clone(),
                    ready_replicas: None,
                    per_replica_max_in_flight: None,
                    per_replica_source: None,
                    headroom: m.capacity_headroom,
                    derived_max_in_flight: None,
                    effective_max_in_flight: m
                        .max_in_flight
                        .and_then(|c| usize::try_from(c).ok())
                        .filter(|c| *c > 0)
                        .unwrap_or(state.default_model_max_in_flight),
                    state: capacity_discovery::STATE_FALLBACK.into(),
                    last_refresh: None,
                    last_success: None,
                    reason: Some(format!("{reason}; using the static max_in_flight")),
                }
            });
            CapacityDiscoveryModelView {
                model_id: m.id,
                model_name: m.model_name,
                enabled: m.enabled,
                static_max_in_flight: m.max_in_flight,
                replica_share: replica_share(status.effective_max_in_flight, replicas),
                status,
            }
        })
        .collect();
    Ok(Json(CapacityDiscoveryView {
        enabled: settings.enabled,
        interval_secs: settings.interval.as_secs(),
        namespaces: settings.namespaces,
        default_selector: settings.default_selector,
        replicas,
        models,
    }))
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
    let tenant_names: std::collections::HashMap<Uuid, String> =
        tenants.into_iter().map(|t| (t.id, t.name)).collect();
    let key_ids: Vec<Uuid> = snap
        .pools
        .iter()
        .flat_map(|p| p.keys.iter().map(|k| k.key_id))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let key_names: std::collections::HashMap<Uuid, String> = state
        .store
        .keys_by_ids(&key_ids)
        .await?
        .into_iter()
        .map(|k| (k.id, k.name))
        .collect();
    let models = state.store.list_models().await?;
    let discovered = state.capacity_discovery.effective_caps();
    let configured_capacity =
        enabled_pool_share_with(&models, state.default_model_max_in_flight, 1, &discovered);
    let capacity = enabled_pool_share_with(
        &models,
        state.default_model_max_in_flight,
        snap.replicas,
        &discovered,
    );

    // Pools outlive their model: a renamed, deleted or disabled model keeps an
    // empty pool in the scheduler until restart. Every pool with visible
    // traffic is kept; the idle ones are dropped unless an enabled model still
    // answers to that name or one of its aliases.
    let live_names: std::collections::HashSet<&str> = models
        .iter()
        .filter(|m| m.enabled)
        .flat_map(|m| {
            std::iter::once(m.model_name.as_str()).chain(m.aliases.iter().map(String::as_str))
        })
        .collect();

    let health_tenant = model_health::health_tenant_id();
    let mut hidden_in_flight = 0usize;
    let mut hidden_queued = 0usize;
    let mut pools: Vec<ModelPoolView> = snap
        .pools
        .iter()
        .map(|p| {
            let cap = p.cap;
            let hidden = p
                .groups
                .iter()
                .find(|g| g.name == model_health::HEALTH_GROUP);
            let h_in = hidden.map(|g| g.in_flight).unwrap_or(0);
            let h_q = hidden.map(|g| g.queued).unwrap_or(0);
            hidden_in_flight += h_in;
            hidden_queued += h_q;
            ModelPoolView {
                model: p.model.clone(),
                cap,
                configured_cap: p.configured_cap,
                in_flight: p.in_flight.saturating_sub(h_in),
                queued: p.queued.saturating_sub(h_q),
                borrowed: p.borrowed,
                groups: p
                    .groups
                    .iter()
                    .filter(|g| g.name != model_health::HEALTH_GROUP)
                    .map(|g| GroupFairshareView {
                        name: g.name.clone(),
                        weight: g.weight,
                        in_flight: g.in_flight,
                        queued: g.queued,
                        slot_cap: g.slot_cap,
                        borrowed: g.borrowed,
                        served_tokens: g.served_tokens,
                        share_score: g.share_score,
                        weight_share: g.weight_share,
                        expected_slots: g.weight_share * cap as f64,
                    })
                    .collect(),
                tenants: p
                    .tenants
                    .iter()
                    .filter(|t| {
                        t.tenant_id != health_tenant
                            && t.fairshare_group != model_health::HEALTH_GROUP
                    })
                    .map(|t| TenantFairshareView {
                        tenant_id: t.tenant_id,
                        name: tenant_names
                            .get(&t.tenant_id)
                            .cloned()
                            .unwrap_or_else(|| t.tenant_id.to_string()),
                        fairshare_group: t.fairshare_group.clone(),
                        weight: t.weight,
                        max_in_flight: t.max_in_flight,
                        in_flight: t.in_flight,
                        queued: t.queued,
                        served_tokens: t.served_tokens,
                        share_score: t.share_score,
                        weight_share: t.weight_share,
                        expected_slots: t.weight_share * cap as f64,
                    })
                    .collect(),
                keys: p
                    .keys
                    .iter()
                    .filter(|k| k.tenant_id != health_tenant)
                    .map(|k| KeyFairshareView {
                        key_id: k.key_id,
                        tenant_id: k.tenant_id,
                        name: key_names
                            .get(&k.key_id)
                            .cloned()
                            .unwrap_or_else(|| k.key_id.to_string()),
                        weight: k.weight,
                        max_in_flight: k.max_in_flight,
                        in_flight: k.in_flight,
                        queued: k.queued,
                        served_tokens: k.served_tokens,
                        share_score: k.share_score,
                        weight_share: k.weight_share,
                        expected_slots: k.weight_share * cap as f64,
                    })
                    .collect(),
            }
        })
        .collect();
    pools.retain(|p| p.in_flight > 0 || p.queued > 0 || live_names.contains(p.model.as_str()));
    let (groups, tenants, keys) = aggregate_pools(&pools);
    Ok(Json(FairshareLiveView {
        algorithm: snap.algorithm,
        max_in_flight: if capacity > 0 {
            capacity
        } else {
            snap.max_in_flight
        },
        configured_max_in_flight: if configured_capacity > 0 {
            configured_capacity
        } else {
            snap.configured_max_in_flight
        },
        hard_ceiling: snap.max_in_flight,
        configured_hard_ceiling: snap.configured_max_in_flight,
        default_model_max_in_flight: snap.default_model_max_in_flight,
        replicas: snap.replicas,
        replica_aware: state.fairshare_replica_aware,
        global_in_flight: snap.global_in_flight.saturating_sub(hidden_in_flight),
        global_queued: snap.global_queued.saturating_sub(hidden_queued) as i64,
        global_borrowed: snap.global_borrowed,
        groups,
        tenants,
        keys,
        pools,
        model_in_flight: snap.model_in_flight,
        model_queued: snap.model_queued,
    }))
}

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct FairshareHistoryQuery {
    /// Return points at or after this Unix time in ms. Default: now minus retention.
    pub since_ms: Option<i64>,
    /// Scope to one model's pool. Absent or empty: aggregate across pools.
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FairshareHistoryPointView {
    pub ts_ms: i64,
    pub in_flight: usize,
    pub queued: usize,
    /// Group name to in-flight slots.
    pub groups: std::collections::BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FairshareHistoryView {
    pub interval_ms: u64,
    pub retention_ms: u64,
    /// Time of the oldest retained sample; null when nothing is retained yet.
    pub oldest_ts_ms: Option<i64>,
    pub points: Vec<FairshareHistoryPointView>,
}

#[utoipa::path(
    get, path = "/api/v1/fairshare/history", tag = "fairshare",
    params(FairshareHistoryQuery),
    responses((status = 200, body = FairshareHistoryView))
)]
async fn get_fairshare_history(
    State(state): State<AdminState>,
    Query(q): Query<FairshareHistoryQuery>,
) -> Json<FairshareHistoryView> {
    let retention_ms = state.fairshare_history_secs.saturating_mul(1000);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let since_ms = q
        .since_ms
        .unwrap_or_else(|| now_ms.saturating_sub(retention_ms as i64));
    let model = q.model.as_deref().filter(|m| !m.is_empty());
    let points = state
        .fairshare_history
        .points(since_ms, model, Some(model_health::HEALTH_GROUP))
        .into_iter()
        .map(|p| FairshareHistoryPointView {
            ts_ms: p.ts_ms,
            in_flight: p.in_flight,
            queued: p.queued,
            groups: p.groups,
        })
        .collect();
    Json(FairshareHistoryView {
        interval_ms: obleth_fairshare::FAIRSHARE_HISTORY_INTERVAL_MS,
        retention_ms,
        oldest_ts_ms: state.fairshare_history.oldest_ts_ms(),
        points,
    })
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
    sync_group_keys(&state, &name).await?;
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

/// Republish every key from Postgres and evict Redis entries with no backing
/// row (e.g. a delete whose eviction failed). Returns (pushed, pruned).
async fn resync_all_keys(state: &AdminState) -> Result<(usize, usize)> {
    let keys = state.store.all_resolved_keys().await?;
    push_keys_bulk(state, &keys, BulkInvalidation::All).await?;
    let known: std::collections::HashSet<String> = keys.iter().map(|(h, _)| h.clone()).collect();
    let pruned: std::collections::HashSet<String> = state
        .redis
        .prune_stale_resolved_keys(&known)
        .await?
        .into_iter()
        .collect();
    // Re-read after the prune: a key inserted after the snapshot but pushed
    // before the SCAN was pruned wrongly and is restored here, and a key deleted
    // after the snapshot (and so re-pushed above) is evicted again.
    let fresh = state.store.all_resolved_keys().await?;
    let fresh_hashes: std::collections::HashSet<&str> =
        fresh.iter().map(|(h, _)| h.as_str()).collect();
    for (hash, resolved) in fresh.iter().filter(|(h, _)| pruned.contains(h)) {
        push_key(state, hash, resolved).await?;
    }
    let gone: Vec<String> = pruned
        .iter()
        .chain(known.iter())
        .filter(|h| !fresh_hashes.contains(h.as_str()))
        .cloned()
        .collect();
    for hash in &gone {
        evict_key(state, hash).await?;
    }
    Ok((fresh.len(), gone.len()))
}

/// Republish every model and MCP server from Postgres and evict resolver
/// entries no enabled row answers to. Returns (models, pruned names, mcp
/// servers, pruned servers).
async fn resync_models_and_mcp(state: &AdminState) -> Result<(usize, usize, usize, usize)> {
    use std::collections::HashSet;
    fn model_names(models: &[ModelRoute]) -> HashSet<String> {
        models
            .iter()
            .filter(|m| m.enabled)
            .flat_map(|m| std::iter::once(&m.model_name).chain(m.aliases.iter()))
            .cloned()
            .collect()
    }
    fn server_names(servers: &[McpServer]) -> HashSet<String> {
        servers
            .iter()
            .filter(|s| s.enabled)
            .map(|s| s.name.clone())
            .collect()
    }

    let models = state.store.list_models().await?;
    for model in &models {
        sync_model(state, model).await?;
    }
    let pruned_models = state
        .redis
        .prune_stale_resolved_models(&model_names(&models))
        .await?;
    // Same re-read as `resync_all_keys`: restore anything created mid-prune.
    let fresh_models = state.store.list_models().await?;
    let fresh_names = model_names(&fresh_models);
    for model in &fresh_models {
        if model.enabled
            && std::iter::once(&model.model_name)
                .chain(model.aliases.iter())
                .any(|n| pruned_models.contains(n))
        {
            sync_model(state, model).await?;
        }
    }
    for name in pruned_models.iter().filter(|n| !fresh_names.contains(*n)) {
        state
            .redis
            .publish_invalidation(&format!("model:{name}"))
            .await?;
    }

    let servers = state.store.list_mcp_servers().await?;
    for server in &servers {
        sync_mcp_server(state, server).await?;
    }
    let pruned_servers = state
        .redis
        .prune_stale_resolved_mcp_servers(&server_names(&servers))
        .await?;
    let fresh_servers = state.store.list_mcp_servers().await?;
    let fresh_server_names = server_names(&fresh_servers);
    for server in fresh_servers
        .iter()
        .filter(|s| pruned_servers.contains(&s.name))
    {
        sync_mcp_server(state, server).await?;
    }
    for name in pruned_servers
        .iter()
        .filter(|n| !fresh_server_names.contains(*n))
    {
        state
            .redis
            .publish_invalidation(&format!("mcp:{name}"))
            .await?;
    }

    Ok((
        fresh_models.len(),
        pruned_models.len(),
        fresh_servers.len(),
        pruned_servers.len(),
    ))
}

/// Outcome of a resolver-cache reconcile.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResyncReport {
    /// Keys republished from Postgres.
    pub keys: usize,
    /// Key entries evicted because no key row backs them.
    pub keys_pruned: usize,
    /// Models republished from Postgres.
    pub models: usize,
    /// Model/alias entries evicted because no enabled model answers to them.
    pub model_names_pruned: usize,
    /// MCP servers republished from Postgres.
    pub mcp_servers: usize,
    /// MCP entries evicted because no enabled server backs them.
    pub mcp_servers_pruned: usize,
}

/// Rebuild the data plane's resolver cache (keys, models, MCP servers) from
/// Postgres. This is the retry for a delete that reported a failed eviction.
#[utoipa::path(
    post, path = "/api/v1/resync", tag = "meta",
    responses((status = 200, body = ResyncReport), (status = 502))
)]
async fn resync_resolver_cache(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<ResyncReport>> {
    let (keys, keys_pruned) = resync_all_keys(&state).await?;
    let (models, model_names_pruned, mcp_servers, mcp_servers_pruned) =
        resync_models_and_mcp(&state).await?;
    let report = ResyncReport {
        keys,
        keys_pruned,
        models,
        model_names_pruned,
        mcp_servers,
        mcp_servers_pruned,
    };
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "resync_resolver_cache",
            "gateway",
            "resolver_cache",
            serde_json::to_value(&report).unwrap_or_default(),
        )
        .await?;
    Ok(Json(report))
}

/// The speculation boon sends the model's upstream key to its scoring endpoint,
/// so a non-empty `verify_api_base` gets the same destination policy as
/// `api_base`.
async fn validate_verify_api_base(state: &AdminState, raw: Option<&str>) -> Result<()> {
    match raw.map(str::trim) {
        Some(url) if !url.is_empty() => state
            .ssrf
            .validate(url)
            .await
            .map_err(|e| AdminError::BadRequest(format!("verify_api_base: {e}"))),
        _ => Ok(()),
    }
}

/// Validate a declared serving format against the fixed vocabulary.
///
/// Unlike most enum-ish fields, this one is *rejected* rather than normalized
/// to the default on the write path: `normalize_quantization` silently folds an
/// unknown value to `unknown`, and an operator who types `fp-8` in the model
/// form deserves to be told, not to have the field quietly emptied. The
/// normalizer still runs in the store, for values arriving from a backend's own
/// spelling (`FP8`, `w4a16-awq`) where folding is the point.
fn validate_quantization(raw: &str) -> Result<String> {
    let q = raw.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Ok(obleth_config::DEFAULT_QUANTIZATION.to_string());
    }
    if !obleth_config::is_valid_quantization(&q) {
        return Err(AdminError::BadRequest(format!(
            "unknown quantization '{raw}' (expected one of: {})",
            obleth_config::QUANTIZATIONS.join(", ")
        )));
    }
    Ok(q)
}

/// Normalize an alias list and reject any name already claimed elsewhere.
///
/// A client sending `model: "x"` cannot tell whether `x` is a canonical name or
/// an alias, so `x` must identify exactly one route: an alias may not collide
/// with any model's `model_name`, with another model's alias, or with the
/// model's own name. `editing` is the row being written, whose own names are
/// not collisions with itself.
async fn validate_aliases(
    state: &AdminState,
    raw: &[String],
    editing: Option<Uuid>,
    model_name: &str,
) -> Result<Vec<String>> {
    let aliases = obleth_config::normalize_aliases(raw);
    for alias in &aliases {
        if alias == model_name {
            return Err(AdminError::BadRequest(format!(
                "alias '{alias}' is the model's own name"
            )));
        }
        if alias == obleth_config::routing::AUTO_MODEL_NAME {
            return Err(AdminError::BadRequest(format!(
                "alias '{alias}' is reserved for automatic model selection"
            )));
        }
        if let Some((owner_id, owner_name)) = state.store.model_name_owner(alias).await? {
            if Some(owner_id) != editing {
                return Err(AdminError::BadRequest(format!(
                    "alias '{alias}' is already taken by model '{owner_name}'"
                )));
            }
        }
    }
    Ok(aliases)
}

/// Validate a capacity mode against the fixed vocabulary, returning its
/// canonical form. Rejected rather than normalized: a typo must not quietly
/// turn a model back to `static`.
fn validate_capacity_mode(raw: &str) -> Result<String> {
    let m = raw.trim().to_ascii_lowercase();
    if !obleth_config::is_valid_capacity_mode(&m) {
        return Err(AdminError::BadRequest(format!(
            "invalid capacity_mode `{raw}` (expected one of: {})",
            obleth_config::CAPACITY_MODES.join(", ")
        )));
    }
    Ok(m)
}

/// The stored discovery fields with a patch applied: an omitted field keeps
/// its value, a `null` (or blank text) clears it.
fn patch_discovery_fields(
    existing: &ModelRoute,
    source: Option<&str>,
    namespace: Option<&Option<String>>,
    selector: Option<&Option<String>>,
    per_replica_max_in_flight: Option<Option<i64>>,
    headroom: Option<f64>,
) -> obleth_config::capacity::DiscoveryFields {
    let current = obleth_config::capacity::DiscoveryFields::of(existing);
    obleth_config::capacity::DiscoveryFields {
        source: source.map(str::to_string).unwrap_or(current.source),
        namespace: namespace.cloned().unwrap_or(current.namespace),
        selector: selector.cloned().unwrap_or(current.selector),
        per_replica_max_in_flight: per_replica_max_in_flight
            .unwrap_or(current.per_replica_max_in_flight),
        headroom: headroom.unwrap_or(current.headroom),
    }
    .normalized()
}

/// Check a model's capacity fields, and for a `discovered` model on the
/// `kubernetes` source that this gateway can read it (see
/// [`obleth_config::capacity::validate_discovery_fields`]).
fn validate_capacity_fields(
    state: &AdminState,
    capacity_mode: &str,
    model_name: &str,
    upstream_model: &str,
    fields: &obleth_config::capacity::DiscoveryFields,
) -> Result<()> {
    obleth_config::capacity::validate_discovery_fields(
        model_name,
        upstream_model,
        fields,
        capacity_mode == obleth_config::DISCOVERED_CAPACITY_MODE,
        state.capacity_discovery.policy(),
    )
    .map_err(AdminError::BadRequest)
}

#[utoipa::path(
    post, path = "/api/v1/models", tag = "models",
    request_body = CreateModel,
    responses((status = 200, body = ModelRouteView))
)]
async fn create_model(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateModel>,
) -> Result<Json<ModelRouteView>> {
    // A blank api_base is allowed: Slurm-provisioned models have no static
    // upstream until a replica is promoted into the endpoint rotation. Only
    // validate a non-empty URL.
    if !body.api_base.trim().is_empty() {
        state.ssrf.validate(&body.api_base).await?;
    }
    validate_verify_api_base(&state, body.verify_api_base.as_deref()).await?;
    let quantization = validate_quantization(body.quantization.as_deref().unwrap_or_default())?;
    let aliases = validate_aliases(
        &state,
        body.aliases.as_deref().unwrap_or_default(),
        None,
        body.model_name.trim(),
    )
    .await?;
    let upstream_headers = match &body.upstream_headers {
        Some(write) => obleth_config::merge_upstream_headers(&Default::default(), write)
            .map_err(AdminError::BadRequest)?,
        None => Default::default(),
    };
    let capacity_mode = validate_capacity_mode(
        body.capacity_mode
            .as_deref()
            .unwrap_or(obleth_config::DEFAULT_CAPACITY_MODE),
    )?;
    let discovery = obleth_config::capacity::DiscoveryFields {
        source: body
            .capacity_source
            .clone()
            .unwrap_or_else(|| obleth_config::DEFAULT_CAPACITY_SOURCE.to_string()),
        namespace: body.capacity_namespace.clone(),
        selector: body.capacity_selector.clone(),
        per_replica_max_in_flight: body.per_replica_max_in_flight,
        headroom: body.capacity_headroom.unwrap_or(1.0),
    }
    .normalized();
    validate_capacity_fields(
        &state,
        &capacity_mode,
        body.model_name.trim(),
        &body.upstream_model,
        &discovery,
    )?;
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
            body.route_bias.unwrap_or(1.0),
            body.auto_eligible.unwrap_or(true),
            body.draft_model.as_deref().unwrap_or(""),
            body.verify_api_base.as_deref().unwrap_or(""),
            body.verify_upstream_model.as_deref().unwrap_or(""),
            &aliases,
            &quantization,
            &upstream_headers,
            body.cost_per_video.unwrap_or(0.0),
            &capacity_mode,
            &discovery,
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
            serde_json::to_value(ModelRouteView::from(model.clone())).unwrap_or_default(),
        )
        .await?;
    Ok(Json(model.into()))
}

#[utoipa::path(
    get, path = "/api/v1/models", tag = "models",
    responses((status = 200, body = [ModelRouteView]))
)]
async fn list_models(State(state): State<AdminState>) -> Result<Json<Vec<ModelRouteView>>> {
    Ok(Json(views(state.store.list_models().await?)))
}

#[utoipa::path(
    get, path = "/api/v1/models/{id}", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 200, body = ModelRouteView), (status = 404))
)]
async fn get_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ModelRouteView>> {
    Ok(Json(state.store.get_model(id).await?.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = UpdateModel,
    responses((status = 200, body = ModelRouteView))
)]
async fn update_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateModel>,
) -> Result<Json<ModelRouteView>> {
    // Blank api_base allowed for provisioned-only (Slurm) models — see create_model.
    if !body.api_base.trim().is_empty() {
        state.ssrf.validate(&body.api_base).await?;
    }
    validate_verify_api_base(&state, body.verify_api_base.as_deref()).await?;
    let existing = state.store.get_model(id).await?;
    let api_key = body.api_key.as_deref().or(existing.api_key.as_deref());
    let quantization = match body.quantization.as_deref() {
        Some(q) => validate_quantization(q)?,
        None => existing.quantization.clone(),
    };
    let aliases = match body.aliases.as_deref() {
        Some(list) => validate_aliases(&state, list, Some(id), &existing.model_name).await?,
        None => existing.aliases.clone(),
    };
    let upstream_headers = match &body.upstream_headers {
        Some(write) => obleth_config::merge_upstream_headers(&existing.upstream_headers, write)
            .map_err(AdminError::BadRequest)?,
        None => existing.upstream_headers.clone(),
    };
    let capacity_mode = match body.capacity_mode.as_deref() {
        Some(m) => validate_capacity_mode(m)?,
        None => existing.capacity_mode.clone(),
    };
    let discovery = patch_discovery_fields(
        &existing,
        body.capacity_source.as_deref(),
        body.capacity_namespace.as_ref(),
        body.capacity_selector.as_ref(),
        body.per_replica_max_in_flight,
        body.capacity_headroom,
    );
    validate_capacity_fields(
        &state,
        &capacity_mode,
        &existing.model_name,
        &body.upstream_model,
        &discovery,
    )?;
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
            body.route_bias.unwrap_or(existing.route_bias),
            body.auto_eligible.unwrap_or(existing.auto_eligible),
            body.draft_model.as_deref().unwrap_or(&existing.draft_model),
            body.verify_api_base
                .as_deref()
                .unwrap_or(&existing.verify_api_base),
            body.verify_upstream_model
                .as_deref()
                .unwrap_or(&existing.verify_upstream_model),
            &aliases,
            &quantization,
            &upstream_headers,
            body.cost_per_video.unwrap_or(existing.cost_per_video),
            &capacity_mode,
            &discovery,
        )
        .await?;
    if model_health::probe_config_changed(&existing, &model) {
        // The old failure streak / alert state describe a configuration that
        // no longer exists; reset so the scheduler re-verifies immediately.
        state.store.reset_model_health(id).await?;
    }
    sync_model_from(&state, &model, Some(&existing)).await?;
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
    Ok(Json(model.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/capacity", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelCapacity,
    responses((status = 200, body = ModelRouteView))
)]
async fn set_model_capacity(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelCapacity>,
) -> Result<Json<ModelRouteView>> {
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
    Ok(Json(model.into()))
}

#[utoipa::path(
    put,
    path = "/api/v1/models/{id}/capacity-mode",
    tag = "models",
    request_body = SetModelCapacityMode,
    responses((status = 200, body = ModelRouteView))
)]
async fn set_model_capacity_mode(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelCapacityMode>,
) -> Result<Json<ModelRouteView>> {
    let capacity_mode = validate_capacity_mode(&body.capacity_mode)?;
    let existing = state.store.get_model(id).await?;
    let discovery = patch_discovery_fields(
        &existing,
        body.capacity_source.as_deref(),
        body.capacity_namespace.as_ref(),
        body.capacity_selector.as_ref(),
        body.per_replica_max_in_flight,
        body.capacity_headroom,
    );
    validate_capacity_fields(
        &state,
        &capacity_mode,
        &existing.model_name,
        &existing.upstream_model,
        &discovery,
    )?;
    let model = state
        .store
        .update_model_capacity_mode(id, &capacity_mode, Some(&discovery))
        .await?;
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "set_model_capacity_mode",
            "model",
            &id.to_string(),
            serde_json::json!({
                "capacity_mode": model.capacity_mode,
                "capacity_source": model.capacity_source,
                "capacity_namespace": model.capacity_namespace,
                "capacity_selector": model.capacity_selector,
                "per_replica_max_in_flight": model.per_replica_max_in_flight,
                "capacity_headroom": model.capacity_headroom,
            }),
        )
        .await?;
    Ok(Json(model.into()))
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
    responses((status = 200, body = ModelRouteView))
)]
async fn apply_autotune_capacity(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ApplyAutotuneCapacity>,
) -> Result<Json<ModelRouteView>> {
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
    Ok(Json(model.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/weight", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelWeight,
    responses((status = 200, body = ModelRouteView))
)]
async fn set_model_weight(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelWeight>,
) -> Result<Json<ModelRouteView>> {
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
    Ok(Json(model.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/cache", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelCache,
    responses((status = 200, body = ModelRouteView))
)]
async fn set_model_cache(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelCache>,
) -> Result<Json<ModelRouteView>> {
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
    Ok(Json(model.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/reliability", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelReliability,
    responses((status = 200, body = ModelRouteView))
)]
async fn set_model_reliability(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelReliability>,
) -> Result<Json<ModelRouteView>> {
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
    Ok(Json(model.into()))
}

// ---- model endpoints -----------------------------------------------------

#[utoipa::path(
    get, path = "/api/v1/models/{id}/endpoints", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 200, body = [ModelEndpointView]))
)]
async fn list_model_endpoints(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ModelEndpointView>>> {
    // Confirm the model exists so callers get 404 (not an empty list) for a
    // bad id.
    state.store.get_model(id).await?;
    Ok(Json(views(state.store.list_model_endpoints(id).await?)))
}

/// An endpoint's own concurrency, when given, has the same bounds as a
/// model's `per_replica_max_in_flight`.
fn validate_endpoint_max_in_flight(value: Option<i64>) -> Result<()> {
    obleth_config::capacity::validate_per_replica_max_in_flight(value).map_err(|_| {
        AdminError::BadRequest(format!(
            "max_in_flight must be between 1 and {}",
            obleth_config::capacity::MAX_PER_REPLICA_MAX_IN_FLIGHT
        ))
    })
}

#[utoipa::path(
    post, path = "/api/v1/models/{id}/endpoints", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = CreateModelEndpoint,
    responses((status = 200, body = ModelEndpointView))
)]
async fn create_model_endpoint(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<CreateModelEndpoint>,
) -> Result<Json<ModelEndpointView>> {
    state.ssrf.validate(&body.api_base).await?;
    validate_endpoint_max_in_flight(body.max_in_flight)?;
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
            body.max_in_flight,
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
    Ok(Json(endpoint.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/endpoints/{endpoint_id}", tag = "models",
    params(
        ("id" = Uuid, Path, description = "Model id"),
        ("endpoint_id" = Uuid, Path, description = "Endpoint id")
    ),
    request_body = UpdateModelEndpoint,
    responses((status = 200, body = ModelEndpointView), (status = 404))
)]
async fn update_model_endpoint(
    State(state): State<AdminState>,
    Path((id, endpoint_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(body): Json<UpdateModelEndpoint>,
) -> Result<Json<ModelEndpointView>> {
    state.ssrf.validate(&body.api_base).await?;
    let model = state.store.get_model(id).await?;
    let max_in_flight = match body.max_in_flight {
        Some(v) => v,
        // Omitted keeps the stored value. An id outside this model finds
        // nothing here and is refused by the scoped update below.
        None => state
            .store
            .list_model_endpoints(model.id)
            .await?
            .into_iter()
            .find(|e| e.id == endpoint_id)
            .and_then(|e| e.max_in_flight),
    };
    validate_endpoint_max_in_flight(max_in_flight)?;
    let endpoint = state
        .store
        // Scoped to the path model: another model's endpoint id is a 404.
        .update_model_endpoint(
            model.id,
            endpoint_id,
            &body.name,
            &body.api_base,
            body.api_key.as_deref(),
            body.priority,
            body.weight,
            body.enabled,
            max_in_flight,
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
    Ok(Json(endpoint.into()))
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
    // Scoped to the path model: another model's endpoint id is a 404.
    state
        .store
        .delete_model_endpoint(model.id, endpoint_id)
        .await?;
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

#[derive(serde::Deserialize, ToSchema)]
pub struct ProvisionErrorBody {
    #[serde(default)]
    pub error: Option<String>,
}

#[utoipa::path(patch, path = "/api/v1/models/{id}/managed/provision-error",
    request_body = ProvisionErrorBody,
    responses((status = 200)))]
async fn set_provision_error(
    State(state): State<AdminState>,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(body): Json<ProvisionErrorBody>,
) -> Result<Json<serde_json::Value>> {
    // Only a clear is an operator-visible change worth auditing: the
    // provisioner records an error on every failed submit and clears on every
    // successful one, so the audit row is written only when a clear actually
    // removed a recorded error.
    let cleared = match body.error {
        None => state
            .store
            .get_managed_model(id)
            .await?
            .and_then(|m| m.last_provision_error),
        Some(_) => None,
    };
    state
        .store
        .set_provision_error(id, body.error.as_deref())
        .await?;
    if let Some(previous) = cleared {
        state
            .store
            .record_audit(
                &audit_actor(&headers),
                "clear_provision_error",
                "managed_model",
                &id.to_string(),
                serde_json::json!({ "cleared_error": previous }),
            )
            .await?;
    }
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
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let n = state.store.delete_lost_replicas(id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "clear_lost_replicas",
            "model",
            &id.to_string(),
            serde_json::json!({ "deleted": n }),
        )
        .await?;
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
    // Aliases are resolver keys of their own, so a delete has to clear all of
    // them or the model stays reachable under its old names.
    let mut evict_err = None;
    for name in
        std::iter::once(model.model_name.as_str()).chain(model.aliases.iter().map(String::as_str))
    {
        let evicted = async {
            state.redis.delete_resolved_model(name).await?;
            state
                .redis
                .publish_invalidation(&format!("model:{name}"))
                .await
        }
        .await;
        if let Err(e) = evicted {
            evict_err.get_or_insert(e);
        }
    }
    // Audited before an eviction failure is reported: the row is gone.
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "delete_model",
            "model",
            &id.to_string(),
            serde_json::json!({
                "model_name": model.model_name,
                "cache_evicted": evict_err.is_none(),
            }),
        )
        .await?;
    if let Some(e) = evict_err {
        return Err(eviction_failed("the model", e));
    }
    Ok(StatusCode::NO_CONTENT)
}

// ---- mcp servers ---------------------------------------------------------

#[utoipa::path(
    post, path = "/api/v1/mcp-servers", tag = "mcp",
    request_body = CreateMcpServer,
    responses((status = 200, body = McpServerView))
)]
async fn create_mcp_server(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateMcpServer>,
) -> Result<Json<McpServerView>> {
    state.ssrf.validate(&body.upstream_url).await?;
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
    Ok(Json(server.into()))
}

#[utoipa::path(
    get, path = "/api/v1/mcp-servers", tag = "mcp",
    responses((status = 200, body = [McpServerView]))
)]
async fn list_mcp_servers(State(state): State<AdminState>) -> Result<Json<Vec<McpServerView>>> {
    Ok(Json(views(state.store.list_mcp_servers().await?)))
}

#[utoipa::path(
    get, path = "/api/v1/mcp-servers/{id}", tag = "mcp",
    params(("id" = Uuid, Path, description = "MCP server id")),
    responses((status = 200, body = McpServerView), (status = 404))
)]
async fn get_mcp_server(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServerView>> {
    Ok(Json(state.store.get_mcp_server(id).await?.into()))
}

#[utoipa::path(
    put, path = "/api/v1/mcp-servers/{id}", tag = "mcp",
    params(("id" = Uuid, Path, description = "MCP server id")),
    request_body = UpdateMcpServer,
    responses((status = 200, body = McpServerView))
)]
async fn update_mcp_server(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateMcpServer>,
) -> Result<Json<McpServerView>> {
    state.ssrf.validate(&body.upstream_url).await?;
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
    Ok(Json(server.into()))
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
    let evicted = async {
        state.redis.delete_resolved_mcp_server(&server.name).await?;
        state
            .redis
            .publish_invalidation(&format!("mcp:{}", server.name))
            .await
    }
    .await
    .map_err(|e| eviction_failed("the MCP server", e));
    // Cascade: drop the deleted server from every model's tool grants. A stale
    // grant fails tool discovery on every request, and the dashboard can no
    // longer display or clear it once the server's checkbox is gone. The
    // Postgres cascade runs even when the eviction above failed.
    let stripped = state.store.strip_tool_server_grants(&server.name).await?;
    let mut synced = Ok(());
    for model in &stripped {
        if let Err(e) = sync_model(&state, model).await {
            if synced.is_ok() {
                synced = Err(AdminError::CacheSync(format!(
                    "the MCP server was deleted, but republishing model '{}' without its \
                     grant failed ({e}); reconcile the data-plane cache with {RESYNC_ROUTE}",
                    model.model_name
                )));
            }
        }
    }
    // Audited before a cache failure is reported: the rows are gone.
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "delete_mcp_server",
            "mcp_server",
            &id.to_string(),
            serde_json::json!({
                "name": server.name,
                "models_ungranted": stripped
                    .iter()
                    .map(|m| m.model_name.as_str())
                    .collect::<Vec<_>>(),
                "cache_evicted": evicted.is_ok() && synced.is_ok(),
            }),
        )
        .await?;
    evicted?;
    synced?;
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

/// Route that rebuilds the resolver cache from Postgres; named in every
/// eviction-failure error so the operator has a concrete retry.
const RESYNC_ROUTE: &str = "POST /api/v1/resync";

/// Resolver entries have no TTL, so a missed eviction would keep a deleted key,
/// model or MCP server resolving indefinitely. Surface it as a 502 naming the
/// reconcile route instead of reporting success.
fn eviction_failed(what: &str, e: obleth_redis::RedisError) -> AdminError {
    AdminError::CacheSync(format!(
        "{what} was deleted, but evicting it from the data-plane cache failed ({e}); \
         it may keep resolving until the cache is reconciled with {RESYNC_ROUTE}"
    ))
}

/// Like [`eviction_failed`] for a saved change (disable, alias removal) whose
/// stale resolver entry could not be removed.
fn cache_removal_failed(what: &str, e: obleth_redis::RedisError) -> AdminError {
    AdminError::CacheSync(format!(
        "the change was saved, but removing {what} from the data-plane cache failed ({e}); \
         it may keep resolving until the cache is reconciled with {RESYNC_ROUTE}"
    ))
}

/// Evict a deleted key from Redis and every gateway's in-process cache.
async fn evict_key(
    state: &AdminState,
    hash: &str,
) -> std::result::Result<(), obleth_redis::RedisError> {
    let result = async {
        state.redis.delete_resolved_key(hash).await?;
        state.redis.publish_invalidation(hash).await
    }
    .await;
    // Always drop the local copy: even if the publish failed, this replica
    // must not keep serving the key from moka for the TTL backstop.
    if let Some(tx) = &state.local_cache_tx {
        let _ = tx.send(hash.to_string());
    }
    result
}

/// Evict every hash, attempting all of them before reporting the first failure.
async fn evict_keys(state: &AdminState, hashes: &[String], what: &str) -> Result<()> {
    let mut first_err = None;
    for hash in hashes {
        if let Err(e) = evict_key(state, hash).await {
            first_err.get_or_insert(e);
        }
    }
    match first_err {
        Some(e) => Err(eviction_failed(what, e)),
        None => Ok(()),
    }
}

async fn push_key(state: &AdminState, hash: &str, resolved: &ResolvedKey) -> Result<()> {
    state.redis.put_resolved_key(hash, resolved).await?;
    state.redis.publish_invalidation(hash).await?;
    if let Some(tx) = &state.local_cache_tx {
        let _ = tx.send(hash.to_string());
    }
    Ok(())
}

async fn sync_model(state: &AdminState, model: &ModelRoute) -> Result<()> {
    sync_model_from(state, model, None).await
}

/// Republish a model into the resolver cache, evicting the keys it no longer
/// owns.
///
/// `previous` is the row as it was before this write, and is only needed when
/// aliases may have changed: an alias that was just dropped still has a live
/// `obleth:model:<alias>` key pointing at this model, and nothing else in the
/// system would ever clear it. Passing `None` (create, capacity toggle, any
/// write that cannot touch aliases) publishes without an eviction pass.
async fn sync_model_from(
    state: &AdminState,
    model: &ModelRoute,
    previous: Option<&ModelRoute>,
) -> Result<()> {
    // Endpoints carry the per-cluster wire targets and health; the data plane
    // prefers them over the legacy single api_base/api_key when present.
    let endpoints = state
        .store
        .resolved_endpoints_for(model.id)
        .await
        .unwrap_or_default();
    let knowledge_collections = state
        .store
        .model_collection_ids(model.id)
        .await
        .unwrap_or_default();
    let resolved = ResolvedModel {
        model_name: model.model_name.clone(),
        aliases: model.aliases.clone(),
        upstream_model: model.upstream_model.clone(),
        api_base: model.api_base.clone(),
        api_key: model.api_key.clone(),
        upstream_headers: model.upstream_headers.clone(),
        model_type: model.model_type.clone(),
        quantization: model.quantization.clone(),
        admission_weight: model.admission_weight,
        max_in_flight: model.max_in_flight.and_then(|n| usize::try_from(n).ok()),
        capacity_mode: model.capacity_mode.clone(),
        capacity_source: model.capacity_source.clone(),
        capacity_namespace: model.capacity_namespace.clone(),
        capacity_selector: model.capacity_selector.clone(),
        per_replica_max_in_flight: model
            .per_replica_max_in_flight
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0),
        capacity_headroom: model.capacity_headroom,
        enabled: model.enabled,
        cache_enabled: model.cache_enabled,
        cache_ttl_secs: model.cache_ttl_secs,
        input_cost_per_token: model.input_cost_per_token,
        output_cost_per_token: model.output_cost_per_token,
        cost_per_image: model.cost_per_image,
        cost_per_audio_second: model.cost_per_audio_second,
        cost_per_character: model.cost_per_character,
        cost_per_video: model.cost_per_video,
        context_window: model.context_window,
        supports_function_calling: model.supports_function_calling,
        supports_system_messages: model.supports_system_messages,
        supports_response_schema: model.supports_response_schema,
        supports_tool_choice: model.supports_tool_choice,
        supports_vision: model.supports_vision,
        // `model.tags` is the raw suffixed storage form (round-tripped as-is
        // through create/update so a declared `tag:level` survives an edit
        // that doesn't touch tags). The hot-path cache needs the bare
        // vocabulary for the router's overlap match, plus the parsed ladder.
        tags: obleth_config::normalize_tags(&model.tags),
        declared_levels: obleth_config::declared_tag_levels(&model.tags),
        boons: model.boons.clone(),
        tool_servers: model.tool_servers.clone(),
        knowledge_collections,
        request_timeout_secs: model.request_timeout_secs,
        max_retries: model.max_retries,
        retry_backoff_ms: model.retry_backoff_ms,
        endpoint_selection_mode: model.endpoint_selection_mode.clone(),
        debug_diagnostics: model.debug_diagnostics,
        energy_slots_per_node: model.energy_slots_per_node,
        route_bias: model.route_bias,
        auto_eligible: model.auto_eligible,
        draft_model: model.draft_model.clone(),
        verify_api_base: model.verify_api_base.clone(),
        verify_upstream_model: model.verify_upstream_model.clone(),
        endpoints,
    };
    // Aliases the write removed: their keys would otherwise keep resolving to
    // this model forever. Done before the publish so a name moved from alias to
    // canonical (or between the two lists) is re-added, not left evicted.
    if let Some(previous) = previous {
        for stale in previous
            .aliases
            .iter()
            .filter(|a| !model.aliases.contains(a))
        {
            state
                .redis
                .delete_resolved_model(stale)
                .await
                .map_err(|e| cache_removal_failed("a removed alias", e))?;
            state
                .redis
                .publish_invalidation(&format!("model:{stale}"))
                .await
                .map_err(|e| cache_removal_failed("a removed alias", e))?;
        }
    }
    // Every name the model answers to gets its own resolver key, so the data
    // plane keeps resolving an alias in exactly one lookup — aliases cost
    // nothing on the request path.
    for name in resolved.addressable_names() {
        if model.enabled {
            state.redis.put_resolved_model(name, &resolved).await?;
        } else {
            state
                .redis
                .delete_resolved_model(name)
                .await
                .map_err(|e| cache_removal_failed("the disabled model", e))?;
        }
        state
            .redis
            .publish_invalidation(&format!("model:{name}"))
            .await?;
    }
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
        state
            .redis
            .delete_resolved_mcp_server(&server.name)
            .await
            .map_err(|e| cache_removal_failed("the disabled MCP server", e))?;
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

/// In-flight Redis writes for a bulk key push. The connection is multiplexed,
/// so concurrent SETs pipeline instead of paying one round trip each.
const BULK_PUSH_CONCURRENCY: usize = 32;

/// How a bulk key push tells the gateways to drop their in-process copies.
#[derive(Clone, Copy)]
enum BulkInvalidation {
    /// One message per pushed hash. Used for scoped pushes (a group weight
    /// change) so gateways keep every unrelated key, model, and MCP entry.
    PerKey,
    /// A single `*`, which clears every gateway's key, model, and MCP caches.
    /// Used only by the full reconcile, which rewrites all of them anyway.
    All,
}

/// Re-push the keys of every tenant in a fairshare group after its weight
/// changed. Keys outside the group carry a different `group_weight` and are
/// untouched; no SCAN or prune runs here (that is `POST /api/v1/resync`).
async fn sync_group_keys(state: &AdminState, group: &str) -> Result<usize> {
    let keys = group_keys(state.store.all_resolved_keys().await?, group);
    push_keys_bulk(state, &keys, BulkInvalidation::PerKey).await?;
    Ok(keys.len())
}

fn group_keys(keys: Vec<(String, ResolvedKey)>, group: &str) -> Vec<(String, ResolvedKey)> {
    keys.into_iter()
        .filter(|(_, k)| k.fairshare_group == group)
        .collect()
}

/// Write many resolved keys to Redis with bounded concurrency, then invalidate
/// the gateways' in-process copies. Every SET completes before any invalidation
/// is published, so a gateway that refetches on the message reads the new row.
async fn push_keys_bulk(
    state: &AdminState,
    keys: &[(String, ResolvedKey)],
    invalidation: BulkInvalidation,
) -> Result<()> {
    use futures::stream::{self, StreamExt, TryStreamExt};
    if keys.is_empty() {
        return Ok(());
    }
    // Indexing (rather than mapping over `keys.iter()`) keeps the closure's
    // argument free of a borrowed lifetime; otherwise the stream is not provably
    // `Send` for every lifetime and the axum handlers calling this fail to build.
    let redis = &state.redis;
    stream::iter(0..keys.len())
        .map(move |i| {
            let (hash, resolved) = &keys[i];
            redis.put_resolved_key(hash, resolved)
        })
        .buffer_unordered(BULK_PUSH_CONCURRENCY)
        .try_for_each(|()| async { Ok(()) })
        .await?;
    match invalidation {
        BulkInvalidation::All => state.redis.publish_invalidation("*").await?,
        BulkInvalidation::PerKey => {
            stream::iter(0..keys.len())
                .map(move |i| redis.publish_invalidation(&keys[i].0))
                .buffer_unordered(BULK_PUSH_CONCURRENCY)
                .try_for_each(|()| async { Ok(()) })
                .await?
        }
    }
    if let Some(tx) = &state.local_cache_tx {
        for (hash, _) in keys {
            let _ = tx.send(hash.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Live-service plumbing for the handful of admin tests that need a real
    /// router. Everything here self-skips when the integration datastores are
    /// not configured, so a plain `cargo test` stays hermetic.
    mod harness {
        use super::*;
        use tower::ServiceExt;

        pub(super) const TEST_ADMIN_TOKEN: &str = "test-admin-token";

        /// Serialises the DB-backed tests in this crate, mirroring
        /// `obleth-store`'s `serial()`. These tests share one Postgres
        /// database, and some of what a handler reads is *global*, not scoped
        /// to a test's own fixtures: `build_candidates` derives cost-rank tier
        /// levels across every model in the database, so a sibling test
        /// creating or deleting a fixture model mid-test shifts the ladder and
        /// changes another test's `level` values between two calls (exactly
        /// how `simulate_pins_the_draw_so_two_runs_are_comparable` flaked in
        /// CI). Held via `TestApp` so every DB-backed test carries it for its
        /// full duration and a new test cannot forget to take it.
        static SERIAL: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
        fn serial() -> &'static tokio::sync::Mutex<()> {
            SERIAL.get_or_init(tokio::sync::Mutex::default)
        }

        /// Reads `OBLETH_TEST_DATABASE_URL`, mirroring the guard in
        /// `obleth-store`'s harness: refuses any database whose name does not
        /// contain "test", so a misconfigured env can never point these
        /// fixtures at a real or dev database.
        pub(super) fn test_db_url() -> Option<String> {
            let url = std::env::var("OBLETH_TEST_DATABASE_URL").ok()?;
            let db = url
                .rsplit('/')
                .next()
                .unwrap_or("")
                .split('?')
                .next()
                .unwrap_or("");
            assert!(
                db.contains("test"),
                "OBLETH_TEST_DATABASE_URL database name {db:?} is not a dedicated test DB \
                 (name must contain \"test\", e.g. obleth_test). Refusing to run integration \
                 tests against a possibly-real database."
            );
            Some(url)
        }

        /// A fixture model with sane defaults. Caller owns the returned row and
        /// must delete it.
        pub(super) async fn fixture_model(
            store: &Store,
            name: &str,
            input_cost_per_token: f64,
        ) -> ModelRoute {
            store
                .create_model(
                    name,
                    "",
                    name,
                    "http://upstream.invalid",
                    None,
                    obleth_config::DEFAULT_MODEL_TYPE,
                    input_cost_per_token,
                    0.0,
                    0.0,
                    0.0,
                    0.0,
                    128_000,
                    100,
                    Some(4),
                    true,
                    true,
                    true,
                    true,
                    false,
                    &["coding".to_string()],
                    &[],
                    &[],
                    0,
                    1.0,
                    true,
                    "",
                    "",
                    "",
                    &[],
                    "",
                    &Default::default(),
                    0.0,
                    "static",
                    &Default::default(),
                )
                .await
                .expect("create fixture model")
        }

        /// The real `/api/v1` router plus the handles a test may need to set up
        /// live state the handlers read.
        pub(super) struct TestApp {
            pub(super) app: Router,
            pub(super) store: Store,
            /// Same instance the router's `AdminState` holds, so admitting a
            /// request here is visible to a handler as real fleet load.
            pub(super) fairshare: FairShare,
            /// Same ring the router's `AdminState` holds, so a test can seed
            /// samples and read them back through the handler.
            pub(super) fairshare_history: Arc<FairshareHistory>,
            /// Keeps this test's exclusive claim on the shared test database
            /// alive for the test's full duration (see `serial()`).
            _serial: tokio::sync::MutexGuard<'static, ()>,
        }

        /// The real `/api/v1` router, wired to the integration datastores.
        /// Returns `None` (test skips) when either is unconfigured.
        pub(super) async fn test_admin_app() -> Option<TestApp> {
            let redis_url = std::env::var("OBLETH_TEST_REDIS_URL").ok()?;
            test_admin_app_on(&redis_url).await
        }

        /// [`test_admin_app`] against an explicit Redis URL (e.g. a
        /// restricted ACL user, to inject cache failures).
        pub(super) async fn test_admin_app_on(redis_url: &str) -> Option<TestApp> {
            test_admin_app_with(redis_url, capacity_discovery::CapacityDiscovery::disabled()).await
        }

        /// [`test_admin_app`] with the gateway's capacity discovery settings.
        pub(super) async fn test_admin_app_discovering(
            discovery: obleth_config::CapacityDiscoveryConfig,
        ) -> Option<TestApp> {
            let redis_url = std::env::var("OBLETH_TEST_REDIS_URL").ok()?;
            test_admin_app_with(
                &redis_url,
                capacity_discovery::CapacityDiscovery::new(discovery),
            )
            .await
        }

        async fn test_admin_app_with(
            redis_url: &str,
            capacity_discovery: capacity_discovery::CapacityDiscovery,
        ) -> Option<TestApp> {
            let db_url = test_db_url()?;

            // Taken before the first database touch (`migrate()` runs DDL) and
            // held until the test drops its `TestApp`.
            let guard = serial().lock().await;
            let store = Store::connect(&db_url).await.expect("connect postgres");
            store.migrate().await.expect("migrate");
            let redis = RedisStore::connect(redis_url).await.expect("connect redis");

            let capacity = Arc::new(StaticCapacity::new(64));
            let fairshare = FairShare::start(
                capacity.clone(),
                obleth_config::FairshareAlgorithm::default(),
                32,
            );
            let fairshare_history = Arc::new(FairshareHistory::new(1800));
            let http = reqwest::Client::new();
            let alerts = AlertDispatcher::new(http.clone(), AlertSettings::default());
            let state = AdminState {
                store: store.clone(),
                redis,
                capacity,
                fairshare: fairshare.clone(),
                fairshare_stats: fairshare.stats(),
                default_model_max_in_flight: 32,
                fairshare_history: fairshare_history.clone(),
                fairshare_history_secs: 3600,
                fairshare_replica_aware: true,
                // Never dialled: no route under test reads ClickHouse.
                clickhouse: clickhouse::Client::default(),
                admin_token: TEST_ADMIN_TOKEN.to_string(),
                output_stats: Default::default(),
                classify: None,
                capacity_discovery,
                health: ModelHealthRuntime {
                    scheduled_enabled: false,
                    default_interval_secs: 60,
                    timeout_secs: 5,
                    retention_days: 1,
                    http,
                    alerts: None,
                    telemetry: None,
                    catalogs: Default::default(),
                },
                usage_retention_default_days: 30,
                ssrf: ssrf::SsrfPolicy::from_env(),
                alerts,
                local_cache_tx: None,
            };
            Some(TestApp {
                app: router(state),
                store,
                fairshare,
                fairshare_history,
                _serial: guard,
            })
        }

        pub(super) async fn send(
            app: &Router,
            req: axum::http::Request<axum::body::Body>,
        ) -> (StatusCode, serde_json::Value) {
            let res = app.clone().oneshot(req).await.expect("handler ran");
            let status = res.status();
            let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .expect("read body");
            let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            (status, json)
        }

        pub(super) fn simulate_request(
            token: Option<&str>,
            body: serde_json::Value,
        ) -> axum::http::Request<axum::body::Body> {
            let mut req = axum::http::Request::post("/api/v1/router/simulate")
                .header("content-type", "application/json");
            if let Some(token) = token {
                req = req.header("authorization", format!("Bearer {token}"));
            }
            req.body(axum::body::Body::from(body.to_string()))
                .expect("build request")
        }
    }

    use harness::{
        fixture_model, send, simulate_request, test_admin_app, test_admin_app_discovering,
        test_admin_app_on, TEST_ADMIN_TOKEN,
    };

    fn json_request(
        method: &str,
        path: &str,
        body: serde_json::Value,
    ) -> axum::http::Request<axum::body::Body> {
        axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request")
    }

    /// The discovered mode's fields go through create, update and the
    /// capacity-mode endpoint with their patch rules, and a `kubernetes`
    /// source this gateway cannot read is refused when the model is written.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn discovered_capacity_fields_are_validated_and_patched() {
        let Some(t) = test_admin_app_discovering(obleth_config::CapacityDiscoveryConfig {
            enabled: true,
            interval: std::time::Duration::from_secs(15),
            namespaces: vec!["inference".into()],
            default_selector: "app.example.com/model={upstream_model}".into(),
        })
        .await
        else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let name = format!("disc-{}", Uuid::new_v4());
        let base = serde_json::json!({
            "model_name": name,
            "upstream_model": "served-model",
            "api_base": "http://127.0.0.1:9/v1",
        });
        let with = |extra: serde_json::Value| {
            let mut b = base.clone();
            for (k, v) in extra.as_object().unwrap() {
                b[k] = v.clone();
            }
            b
        };

        // Refused: a namespace outside the allowlist, a bad selector, an
        // unknown source or mode, a zero per-replica value.
        for (bad, why) in [
            (
                serde_json::json!({"capacity_mode": "discovered", "capacity_source": "kubernetes",
                                   "capacity_namespace": "kube-system"}),
                "OBLETH_CAPACITY_DISCOVERY_NAMESPACES",
            ),
            (
                serde_json::json!({"capacity_mode": "discovered", "capacity_source": "kubernetes",
                                   "capacity_selector": "app in (a"}),
                "capacity_selector",
            ),
            (
                serde_json::json!({"capacity_mode": "discovered", "capacity_source": "prometheus"}),
                "capacity_source",
            ),
            (
                serde_json::json!({"capacity_mode": "automatic"}),
                "capacity_mode",
            ),
            (
                serde_json::json!({"per_replica_max_in_flight": 0}),
                "per_replica_max_in_flight",
            ),
            (
                serde_json::json!({"capacity_headroom": 50.0}),
                "capacity_headroom",
            ),
        ] {
            let (status, body) =
                send(&t.app, json_request("POST", "/api/v1/models", with(bad))).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(body.to_string().contains(why), "{why}: {body}");
        }

        // Accepted: the default selector template renders for this model.
        let (status, created) = send(
            &t.app,
            json_request(
                "POST",
                "/api/v1/models",
                with(serde_json::json!({
                    "capacity_mode": "discovered",
                    "capacity_source": "kubernetes",
                    "capacity_namespace": " inference ",
                    "per_replica_max_in_flight": 8,
                    "capacity_headroom": 1.25,
                })),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{created}");
        assert_eq!(created["capacity_mode"], "discovered");
        assert_eq!(created["capacity_source"], "kubernetes");
        assert_eq!(created["capacity_namespace"], "inference");
        assert_eq!(created["capacity_selector"], serde_json::Value::Null);
        assert_eq!(created["per_replica_max_in_flight"], 8);
        assert_eq!(created["capacity_headroom"], 1.25);
        let id = created["id"].as_str().unwrap().to_string();

        // Update: omitted fields are kept, null clears, "" clears text.
        let (status, updated) = send(
            &t.app,
            json_request(
                "PUT",
                &format!("/api/v1/models/{id}"),
                serde_json::json!({
                    "upstream_model": "served-model",
                    "api_base": "http://127.0.0.1:9/v1",
                    "capacity_selector": "app=served-model,role!=worker",
                    "per_replica_max_in_flight": null,
                    "capacity_namespace": "",
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{updated}");
        assert_eq!(updated["capacity_mode"], "discovered", "kept");
        assert_eq!(updated["capacity_headroom"], 1.25, "kept");
        assert_eq!(
            updated["capacity_selector"],
            "app=served-model,role!=worker"
        );
        assert_eq!(
            updated["per_replica_max_in_flight"],
            serde_json::Value::Null
        );
        assert_eq!(updated["capacity_namespace"], serde_json::Value::Null);

        // An upstream name the template cannot use needs its own selector.
        let (status, body) = send(
            &t.app,
            json_request(
                "PUT",
                &format!("/api/v1/models/{id}"),
                serde_json::json!({
                    "upstream_model": "org/served-model",
                    "api_base": "http://127.0.0.1:9/v1",
                    "capacity_selector": null,
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

        // The capacity-mode endpoint switches mode and source together.
        let (status, switched) = send(
            &t.app,
            json_request(
                "PUT",
                &format!("/api/v1/models/{id}/capacity-mode"),
                serde_json::json!({
                    "capacity_mode": "discovered",
                    "capacity_source": "endpoints",
                    "per_replica_max_in_flight": 4,
                }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{switched}");
        assert_eq!(switched["capacity_source"], "endpoints");
        assert_eq!(switched["per_replica_max_in_flight"], 4);
        assert_eq!(
            switched["capacity_selector"], "app=served-model,role!=worker",
            "omitted fields are kept"
        );

        // An endpoint carries its own concurrency; omitted on update keeps it.
        let (status, ep) = send(
            &t.app,
            json_request(
                "POST",
                &format!("/api/v1/models/{id}/endpoints"),
                serde_json::json!({"name": "a", "api_base": "http://127.0.0.1:9/v1",
                                   "max_in_flight": 16}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ep}");
        assert_eq!(ep["max_in_flight"], 16);
        let ep_id = ep["id"].as_str().unwrap().to_string();
        let (status, ep) = send(
            &t.app,
            json_request(
                "PUT",
                &format!("/api/v1/models/{id}/endpoints/{ep_id}"),
                serde_json::json!({"name": "a", "api_base": "http://127.0.0.1:9/v1",
                                   "weight": 50}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{ep}");
        assert_eq!(ep["max_in_flight"], 16, "kept");
        let (status, ep) = send(
            &t.app,
            json_request(
                "PUT",
                &format!("/api/v1/models/{id}/endpoints/{ep_id}"),
                serde_json::json!({"name": "a", "api_base": "http://127.0.0.1:9/v1",
                                   "max_in_flight": 0}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{ep}");

        // The discovery view lists the model with the value in force: no loop
        // runs in this test, so the static fallback, and says why.
        let req = axum::http::Request::get("/api/v1/capacity/discovery")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::empty())
            .expect("build request");
        let (status, view) = send(&t.app, req).await;
        assert_eq!(status, StatusCode::OK, "{view}");
        assert_eq!(view["enabled"], true);
        assert_eq!(view["namespaces"], serde_json::json!(["inference"]));
        let entry = view["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["model_id"] == serde_json::json!(id))
            .expect("listed");
        assert_eq!(entry["status"]["state"], "fallback");
        assert_eq!(entry["status"]["effective_max_in_flight"], 32);
        assert!(entry["status"]["reason"]
            .as_str()
            .unwrap()
            .contains("waiting for the next discovery pass"));

        let _ = t.store.delete_model(Uuid::parse_str(&id).unwrap()).await;
    }

    /// The simulator sits behind the same bearer gate as every other write-side
    /// route: it reads the whole model fleet and every tenant's allowlist.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn simulate_requires_admin() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let (status, _) = send(
            &t.app,
            simulate_request(None, serde_json::json!({ "prompt": "hello" })),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    /// The whole point of the endpoint: a full routing verdict with no upstream
    /// call. The fixture model is unique per run and deleted before the
    /// assertions so a failure cannot leave a row behind.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn simulate_returns_an_explanation_without_dispatching() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let name = format!("m-{}", Uuid::new_v4());
        let model = fixture_model(&t.store, &name, 0.0).await;

        let (status, body) = send(
            &t.app,
            simulate_request(
                Some(TEST_ADMIN_TOKEN),
                serde_json::json!({ "prompt": "write a python function" }),
            ),
        )
        .await;

        let _ = t.store.delete_model(model.id).await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert!(body.get("scored").is_some());
        assert!(body.get("rejected").is_some());
        // The fixture is enabled, healthy and roomy, so it must survive every
        // hard filter and appear in the ranking.
        let scored = body["scored"].as_array().expect("scored is an array");
        assert!(
            scored.iter().any(|s| s["model"] == serde_json::json!(name)),
            "the fixture model should be a scored candidate: {scored:?}"
        );
        // A "python function" prompt is tagged by the heuristics, never by the
        // classifier: the simulate path must not make a model call.
        assert_eq!(
            body["tag_source"],
            serde_json::json!("heuristic"),
            "simulate must never report a classification it did not perform"
        );
        assert_eq!(body["difficulty_source"], serde_json::json!("heuristic"));
        assert_eq!(
            body["classifier_ms"],
            serde_json::json!(0),
            "no classifier ran, so there is no timing to report"
        );
        assert!(body["tags"]
            .as_array()
            .expect("tags is an array")
            .contains(&serde_json::json!("coding")));
    }

    /// Weight overrides must be clamped exactly as a settings write would be,
    /// and unset fields must fall through to the persisted values — otherwise
    /// the tuner shows a ranking no saved configuration could produce.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn simulate_clamps_weight_overrides_like_a_settings_write() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let saved = t
            .store
            .get_auto_router_settings()
            .await
            .expect("read settings")
            .unwrap_or_default();

        let (status, body) = send(
            &t.app,
            simulate_request(
                Some(TEST_ADMIN_TOKEN),
                serde_json::json!({
                    "prompt": "hello",
                    "cost_weight": 7.5,      // out of range, clamps to 1.0
                    "temperature": -3.0,     // out of range, clamps to 0.0
                    "default_soft_cap": 0,   // rejected, keeps the saved value
                }),
            ),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["weights"]["cost"], serde_json::json!(1.0));
        assert_eq!(body["temperature"], serde_json::json!(0.0));
        assert_eq!(
            body["weights"]["soft_cap"],
            serde_json::json!(saved.default_soft_cap as f64),
            "a rejected soft cap falls back to the persisted setting"
        );
        assert_eq!(
            body["weights"]["capacity"],
            serde_json::json!(saved.capacity_weight),
            "an unset override keeps the persisted weight"
        );
    }

    /// `effort` is the simulator's stand-in for `x-obleth-effort`, so it must
    /// report the same `header` provenance the data plane records.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn simulate_reports_the_effort_override_as_a_header_source() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let (status, body) = send(
            &t.app,
            simulate_request(
                Some(TEST_ADMIN_TOKEN),
                serde_json::json!({ "prompt": "hello", "effort": "high" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["difficulty"], serde_json::json!(3));
        assert_eq!(body["difficulty_source"], serde_json::json!("header"));
    }

    /// A malformed tenant id is the caller's mistake, not a 500.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn simulate_rejects_a_malformed_tenant_id() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let (status, _) = send(
            &t.app,
            simulate_request(
                Some(TEST_ADMIN_TOKEN),
                serde_json::json!({ "prompt": "hello", "tenant_id": "not-a-uuid" }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// Answering a malformed request with a simulation of a blank prompt is the
    /// worst thing to hand someone who is debugging their routing.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn simulate_rejects_non_array_messages() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let (status, _) = send(
            &t.app,
            simulate_request(
                Some(TEST_ADMIN_TOKEN),
                serde_json::json!({ "messages": { "role": "user", "content": "hi" } }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    /// Omitting `busyness` must score against the *live* fleet load, the same
    /// input the request path passes. Simulating a perfectly idle fleet would
    /// make `capacity_weight` look inert in the tuner: the term it scales would
    /// be a constant 1.0 for every candidate.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn simulate_defaults_busyness_to_the_live_fleet_load() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let name = format!("m-{}", Uuid::new_v4());
        let model = fixture_model(&t.store, &name, 0.0).await;

        // Occupy 2 of the fixture's 4 in-flight slots, and hold the permits for
        // the duration of the call so the scheduler still reports them.
        let tenant = Uuid::new_v4();
        let mut permits = Vec::new();
        for _ in 0..2 {
            let admitted = t
                .fairshare
                .admit(
                    obleth_fairshare::AdmitRequest::new(tenant, name.clone(), 1)
                        .weight(100)
                        .group("default", 100)
                        .model_cap(4),
                )
                .await
                .expect("admitted");
            permits.push(admitted);
        }
        let live = t.fairshare.model_load();
        assert_eq!(
            live.get(&name).copied(),
            Some(2),
            "the scheduler must be reporting the load this test set up"
        );

        // Same request twice: once letting the default apply, once passing the
        // live map explicitly. They must agree — i.e. the default *is* the live
        // load, not an empty map.
        let defaulted = send(
            &t.app,
            simulate_request(
                Some(TEST_ADMIN_TOKEN),
                serde_json::json!({ "prompt": "hello", "uniform": 0.0 }),
            ),
        )
        .await;
        let explicit = send(
            &t.app,
            simulate_request(
                Some(TEST_ADMIN_TOKEN),
                serde_json::json!({ "prompt": "hello", "uniform": 0.0, "busyness": live }),
            ),
        )
        .await;
        // And an explicitly idle fleet, which must differ — otherwise the two
        // assertions above would pass even if busyness were ignored entirely.
        let idle = send(
            &t.app,
            simulate_request(
                Some(TEST_ADMIN_TOKEN),
                serde_json::json!({
                    "prompt": "hello",
                    "uniform": 0.0,
                    "busyness": serde_json::json!({}),
                }),
            ),
        )
        .await;

        drop(permits);
        let _ = t.store.delete_model(model.id).await;

        assert_eq!(defaulted.0, StatusCode::OK, "body: {}", defaulted.1);
        let spare_of = |body: &serde_json::Value| -> f64 {
            body["scored"]
                .as_array()
                .expect("scored is an array")
                .iter()
                .find(|s| s["model"] == serde_json::json!(name))
                .unwrap_or_else(|| panic!("fixture missing from the ranking: {body}"))["spare"]
                .as_f64()
                .expect("spare is a number")
        };
        assert_eq!(
            spare_of(&defaulted.1),
            spare_of(&explicit.1),
            "omitting busyness must score against the live fleet load"
        );
        assert_eq!(
            spare_of(&defaulted.1),
            0.5,
            "2 of 4 slots taken is half spare capacity"
        );
        assert_eq!(
            spare_of(&idle.1),
            1.0,
            "an explicitly idle override must still be honoured"
        );
    }

    /// The tuner calls this endpoint twice — saved weights vs. edited weights —
    /// and shows the difference. With independent draws above temperature 0,
    /// sampling noise would be attributed to the operator's edit, so the draw
    /// must be pinnable and must be echoed back.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn simulate_pins_the_draw_so_two_runs_are_comparable() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        // Two models with clearly different costs, isolated from whatever else
        // lives in the shared test database by a tenant allowlist.
        let cheap = format!("m-cheap-{}", Uuid::new_v4());
        let dear = format!("m-dear-{}", Uuid::new_v4());
        let cheap_row = fixture_model(&t.store, &cheap, 0.000_001).await;
        let dear_row = fixture_model(&t.store, &dear, 0.001).await;
        let tenant = t
            .store
            .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
            .await
            .expect("create tenant");
        t.store
            .update_tenant_allowlist(tenant.id, Some(vec![cheap.clone(), dear.clone()]))
            .await
            .expect("set allowlist");

        let simulate = |uniform: f64| {
            let app = t.app.clone();
            let tenant_id = tenant.id.to_string();
            async move {
                send(
                    &app,
                    simulate_request(
                        Some(TEST_ADMIN_TOKEN),
                        serde_json::json!({
                            "prompt": "hello",
                            "tenant_id": tenant_id,
                            "temperature": 2.0,
                            "uniform": uniform,
                        }),
                    ),
                )
                .await
            }
        };
        let first = simulate(0.0).await;
        let again = simulate(0.0).await;
        let other = simulate(0.999).await;

        let _ = t.store.delete_model(cheap_row.id).await;
        let _ = t.store.delete_model(dear_row.id).await;
        let _ = t.store.delete_tenant(tenant.id).await;

        assert_eq!(first.0, StatusCode::OK, "body: {}", first.1);
        assert_eq!(
            first.1["uniform"],
            serde_json::json!(0.0),
            "the draw used must be echoed back"
        );
        assert_eq!(other.1["uniform"], serde_json::json!(0.999));
        assert_eq!(
            first.1["chosen"], again.1["chosen"],
            "the same pinned draw and the same weights must give the same pick"
        );
        assert_eq!(
            first.1["scored"], again.1["scored"],
            "a pinned draw makes the whole ranking reproducible"
        );
        // Only two candidates survive the allowlist, and at temperature 2.0 a
        // draw at the far end of the mass lands on the runner-up — so the
        // pinning above is doing real work, not describing a degenerate case.
        assert_ne!(
            first.1["chosen"], other.1["chosen"],
            "different pinned draws must be able to move the pick"
        );
    }

    /// A PUT that never mentions `image_generation_model` must not be rejected
    /// because the *previously configured* image model was since deleted or
    /// renamed elsewhere. Before the fix, `image_model` fell back to that
    /// stale existing value and got re-validated anyway, so an operator could
    /// not save an unrelated toggle (here, `vision_enabled`) until they
    /// noticed and cleared a field they never touched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn put_boon_settings_does_not_revalidate_an_unsupplied_image_model() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let original = t
            .store
            .get_boon_settings()
            .await
            .expect("read boon settings");
        let restore = original.clone().unwrap_or_default();

        // Seed a stale reference standing in for "the configured image model
        // was deleted after the fact": a name that is not registered at all.
        let mut seeded = restore.clone();
        seeded.image_generation.enabled = true;
        seeded.image_generation.image_model = Some("ghost-model-does-not-exist".to_string());
        t.store
            .put_boon_settings(&seeded)
            .await
            .expect("seed a stale image_generation_model");

        let req = axum::http::Request::put("/api/v1/settings/boons")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(
                serde_json::json!({ "vision_enabled": true }).to_string(),
            ))
            .expect("build request");
        let (status, body) = send(&t.app, req).await;

        // Leave the shared test database as this test found it, regardless of
        // the assertions below.
        let _ = t.store.put_boon_settings(&restore).await;

        assert_eq!(
            status,
            StatusCode::OK,
            "a PUT that never touched image_generation_model must not be rejected \
             over a stale carried-forward value; body: {body}"
        );
        assert_eq!(
            body["image_generation_model"],
            serde_json::json!("ghost-model-does-not-exist"),
            "the unsupplied field must be carried forward unchanged, not cleared"
        );
        assert_eq!(body["vision_enabled"], serde_json::json!(true));
    }

    /// The speculation verifier receives each model's upstream key, so its
    /// URL template is policy-checked on save: per-model Service hosts are
    /// fine, placeholders in the credentials and blocked literal hosts are not.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn put_boon_settings_validates_the_verify_url_template() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let restore = t
            .store
            .get_boon_settings()
            .await
            .expect("read boon settings")
            .unwrap_or_default();
        let put = |template: &str| {
            axum::http::Request::put("/api/v1/settings/boons")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
                .body(axum::body::Body::from(
                    serde_json::json!({ "speculation_verify_url_template": template }).to_string(),
                ))
                .expect("build request")
        };
        let mut results = Vec::new();
        for template in [
            "http://{upstream}.serving.svc.cluster.local:8000/v1",
            "http://{model}@10.0.0.5:8000/v1",
            "http://169.254.169.254/{model}/v1",
        ] {
            results.push((template, send(&t.app, put(template)).await));
        }

        let _ = t.store.put_boon_settings(&restore).await;

        let status = |i: usize| (results[i].1).0;
        assert_eq!(status(0), StatusCode::OK, "{:?}", results[0]);
        assert_eq!(status(1), StatusCode::BAD_REQUEST, "{:?}", results[1]);
        assert_eq!(status(2), StatusCode::BAD_REQUEST, "{:?}", results[2]);
    }

    /// Slurm URL policy: a disabled draft may hold a URL that doesn't resolve
    /// yet, enabling it (or testing it) holds the URL to the SSRF policy.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn slurm_url_is_validated_when_enabled_and_on_test() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let original = t
            .store
            .get_slurm_settings()
            .await
            .expect("read slurm settings")
            .unwrap_or_default();
        let put = |body: serde_json::Value| {
            axum::http::Request::put("/api/v1/settings/slurm")
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
                .body(axum::body::Body::from(body.to_string()))
                .expect("build request")
        };
        // `.invalid` is reserved and never resolves (RFC 6761).
        let unresolvable = "http://slurm.obleth-test.invalid:6820";

        let (disabled, disabled_body) = send(
            &t.app,
            put(serde_json::json!({
                "enabled": false, "slurmrestd_url": unresolvable, "slurm_user": "obleth"
            })),
        )
        .await;
        let (enabled_unresolvable, _) = send(
            &t.app,
            put(serde_json::json!({
                "enabled": true, "slurmrestd_url": unresolvable, "slurm_user": "obleth"
            })),
        )
        .await;
        let (enabled_blocked, _) = send(
            &t.app,
            put(serde_json::json!({
                "enabled": true, "slurmrestd_url": "http://169.254.169.254:6820",
                "slurm_user": "obleth"
            })),
        )
        .await;
        // A blocked URL stored directly (legacy row / restore) must not be pinged.
        let mut blocked = original.clone();
        blocked.slurmrestd_url = "http://169.254.169.254:6820".into();
        t.store
            .put_slurm_settings(&blocked)
            .await
            .expect("seed blocked url");
        let test_req = axum::http::Request::post("/api/v1/settings/slurm/test")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::empty())
            .expect("build request");
        let (tested, _) = send(&t.app, test_req).await;

        let _ = t.store.put_slurm_settings(&original).await;

        assert_eq!(disabled, StatusCode::OK, "{disabled_body}");
        assert_eq!(enabled_unresolvable, StatusCode::BAD_REQUEST);
        assert_eq!(enabled_blocked, StatusCode::BAD_REQUEST);
        assert_eq!(tested, StatusCode::BAD_REQUEST);
    }

    /// A key delete whose Redis eviction fails must not report success: the
    /// resolver entry has no TTL, so a silent failure leaves the revoked key
    /// working. Failure is injected with a Redis ACL user denied `DEL`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn delete_key_is_a_502_when_the_eviction_fails() {
        let Ok(redis_url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        if redis_url.contains('@') || !redis_url.starts_with("redis://") {
            eprintln!("skipping: needs a credential-free redis:// OBLETH_TEST_REDIS_URL");
            return;
        }
        let user = format!("obleth-test-nodel-{}", Uuid::new_v4().simple());
        let client = ::redis::Client::open(redis_url.as_str()).expect("redis client");
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .expect("redis connect");
        let _: () = ::redis::cmd("ACL")
            .arg("SETUSER")
            .arg(&user)
            .arg("on")
            .arg("nopass")
            .arg("~*")
            .arg("&*")
            .arg("+@all")
            .arg("-del")
            .query_async(&mut conn)
            .await
            .expect("create restricted ACL user");
        let restricted = redis_url.replacen("redis://", &format!("redis://{user}:x@"), 1);
        let Some(t) = test_admin_app_on(&restricted).await else {
            let _: ::redis::RedisResult<()> = ::redis::cmd("ACL")
                .arg("DELUSER")
                .arg(&user)
                .query_async(&mut conn)
                .await;
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };

        let tenant = t
            .store
            .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
            .await
            .expect("create tenant");
        let create = axum::http::Request::post(format!("/api/v1/tenants/{}/keys", tenant.id))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(
                serde_json::json!({ "name": "k" }).to_string(),
            ))
            .expect("build request");
        let (created_status, created) = send(&t.app, create).await;
        let key_id = created["key"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let delete = axum::http::Request::delete(format!("/api/v1/keys/{key_id}"))
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::empty())
            .expect("build request");
        let (deleted_status, deleted) = send(&t.app, delete).await;

        let _ = t.store.delete_tenant(tenant.id).await;
        // The failed eviction left the entry behind; remove it as the admin.
        if let Some(secret) = created["secret"].as_str() {
            let hash = obleth_config::hash_api_key(secret);
            let _: ::redis::RedisResult<()> = ::redis::cmd("DEL")
                .arg(format!("obleth:key:{hash}"))
                .query_async(&mut conn)
                .await;
        }
        let _: ::redis::RedisResult<()> = ::redis::cmd("ACL")
            .arg("DELUSER")
            .arg(&user)
            .query_async(&mut conn)
            .await;

        assert_eq!(created_status, StatusCode::OK, "{created}");
        assert_eq!(deleted_status, StatusCode::BAD_GATEWAY, "{deleted}");
        assert!(
            deleted["error"]
                .as_str()
                .is_some_and(|e| e.contains(RESYNC_ROUTE)),
            "{deleted}"
        );
    }

    /// Removes the group test's tenants and fairshare group on drop, including
    /// on a failed assertion. The store has no group delete, so the group row
    /// goes through the pool; tenants go first because they reference it.
    struct GroupFixture {
        store: Store,
        group: String,
        tenants: Vec<Uuid>,
    }

    impl Drop for GroupFixture {
        fn drop(&mut self) {
            let store = self.store.clone();
            let group = std::mem::take(&mut self.group);
            let tenants = std::mem::take(&mut self.tenants);
            // block_in_place needs the multi-thread test flavor.
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    for id in tenants {
                        let _ = store.delete_tenant(id).await;
                    }
                    let _ = sqlx::query("delete from fairshare_groups where name = $1")
                        .bind(&group)
                        .execute(store.pool())
                        .await;
                });
            });
        }
    }

    /// A group weight change republishes only that group's keys and never
    /// prunes: a key in another group keeps its (sentinel) cache entry, and an
    /// orphan entry with no backing row is left for `POST /resync`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn group_weight_patch_pushes_only_the_groups_keys() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let redis_url = std::env::var("OBLETH_TEST_REDIS_URL").expect("checked above");
        let redis = RedisStore::connect(&redis_url)
            .await
            .expect("connect redis");
        let group = format!("g-{}", Uuid::new_v4().simple());
        t.store
            .create_fairshare_group(&group, 100)
            .await
            .expect("create group");
        let mut fixture = GroupFixture {
            store: t.store.clone(),
            group: group.clone(),
            tenants: Vec::new(),
        };
        let inside = t
            .store
            .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
            .await
            .expect("create tenant");
        fixture.tenants.push(inside.id);
        t.store
            .update_tenant_fairshare_group(inside.id, &group)
            .await
            .expect("move tenant");
        let outside = t
            .store
            .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
            .await
            .expect("create tenant");
        fixture.tenants.push(outside.id);
        let create_key = |tenant: Uuid| {
            axum::http::Request::post(format!("/api/v1/tenants/{tenant}/keys"))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
                .body(axum::body::Body::from(
                    serde_json::json!({ "name": "k" }).to_string(),
                ))
                .expect("build request")
        };
        let (_, a) = send(&t.app, create_key(inside.id)).await;
        let (_, b) = send(&t.app, create_key(outside.id)).await;
        let hash_of = |v: &serde_json::Value| {
            obleth_config::hash_api_key(v["secret"].as_str().unwrap_or_default())
        };
        let (hash_a, hash_b) = (hash_of(&a), hash_of(&b));

        let mut sentinel = redis
            .get_resolved_key(&hash_b)
            .await
            .expect("read b")
            .expect("b cached on create");
        sentinel.tenant_name = "sentinel".to_string();
        redis
            .put_resolved_key(&hash_b, &sentinel)
            .await
            .expect("write sentinel");
        let orphan = format!("orphan-{}", Uuid::new_v4().simple());
        redis
            .put_resolved_key(&orphan, &sentinel)
            .await
            .expect("write orphan");

        let patch = axum::http::Request::patch(format!("/api/v1/fairshare/groups/{group}/weight"))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(
                serde_json::json!({ "weight": 777 }).to_string(),
            ))
            .expect("build request");
        let mut pubsub = ::redis::Client::open(redis_url.as_str())
            .expect("redis client")
            .get_async_pubsub()
            .await
            .expect("pubsub connect");
        pubsub
            .subscribe("obleth:invalidate")
            .await
            .expect("subscribe");
        let (status, body) = send(&t.app, patch).await;
        let mut published = Vec::new();
        {
            use futures::StreamExt;
            let mut messages = pubsub.on_message();
            while let Ok(Some(msg)) =
                tokio::time::timeout(std::time::Duration::from_millis(300), messages.next()).await
            {
                published.push(msg.get_payload::<String>().unwrap_or_default());
            }
        }
        let cached_a = redis.get_resolved_key(&hash_a).await.expect("read a");
        let cached_b = redis.get_resolved_key(&hash_b).await.expect("read b");
        let cached_orphan = redis.get_resolved_key(&orphan).await.expect("read orphan");

        let _ = redis.delete_resolved_key(&orphan).await;
        for hash in [&hash_a, &hash_b] {
            let _ = redis.delete_resolved_key(hash).await;
        }
        drop(fixture);
        let group_left: Option<(String,)> =
            sqlx::query_as("select name from fairshare_groups where name = $1")
                .bind(&group)
                .fetch_optional(t.store.pool())
                .await
                .expect("look up group");

        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(cached_a.map(|k| k.group_weight), Some(777));
        assert_eq!(
            cached_b.map(|k| k.tenant_name),
            Some("sentinel".to_string()),
            "a key outside the group was re-pushed"
        );
        assert!(cached_orphan.is_some(), "a weight patch must not prune");
        assert_eq!(
            published,
            vec![hash_a.clone()],
            "a weight patch invalidates exactly the group's keys, never `*`"
        );
        assert!(group_left.is_none(), "fixture group was not cleaned up");
    }

    #[test]
    fn group_keys_keeps_only_the_named_group() {
        let key = |group: &str| {
            let k: ResolvedKey = serde_json::from_value(serde_json::json!({
                "key_id": Uuid::nil(),
                "tenant_id": Uuid::nil(),
                "tenant_name": "t",
                "fairshare_group": group,
                "group_weight": 1,
                "weight": 1,
                "tokens_per_minute": 1,
                "disabled": false,
            }))
            .expect("resolved key");
            k
        };
        let keys = vec![
            ("a".to_string(), key("research")),
            ("b".to_string(), key("default")),
            ("c".to_string(), key("research")),
        ];
        let hashes: Vec<String> = group_keys(keys, "research")
            .into_iter()
            .map(|(h, _)| h)
            .collect();
        assert_eq!(hashes, ["a", "c"]);
    }

    /// Fairshare weight and per-model cap are set on a key at creation and
    /// edited afterwards, so both have to survive the round trip through the
    /// store and come back on the response the dashboard renders.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn key_create_and_update_carry_fairshare_weight_and_cap() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let tenant = t
            .store
            .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
            .await
            .expect("create tenant");
        let post = |body: serde_json::Value| {
            axum::http::Request::post(format!("/api/v1/tenants/{}/keys", tenant.id))
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
                .body(axum::body::Body::from(body.to_string()))
                .expect("build request")
        };

        let (status, created) = send(
            &t.app,
            post(serde_json::json!({ "name": "k", "weight": 250, "max_in_flight": 3 })),
        )
        .await;
        let key_id = created["key"]["id"].as_str().map(|s| s.to_string());
        let put = |body: serde_json::Value| {
            axum::http::Request::put(format!(
                "/api/v1/keys/{}",
                key_id.clone().unwrap_or_default()
            ))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request")
        };
        let updated = send(
            &t.app,
            put(serde_json::json!({ "name": "k", "weight": 100, "max_in_flight": null })),
        )
        .await;
        let zero_weight = send(
            &t.app,
            post(serde_json::json!({ "name": "k2", "weight": 0 })),
        )
        .await;
        let zero_cap = send(
            &t.app,
            post(serde_json::json!({ "name": "k3", "max_in_flight": 0 })),
        )
        .await;

        let _ = t.store.delete_tenant(tenant.id).await;

        assert_eq!(status, StatusCode::OK, "body: {created}");
        assert_eq!(created["key"]["weight"], serde_json::json!(250));
        assert_eq!(created["key"]["max_in_flight"], serde_json::json!(3));
        assert_eq!(updated.0, StatusCode::OK, "body: {}", updated.1);
        assert_eq!(updated.1["weight"], serde_json::json!(100));
        assert_eq!(
            updated.1["max_in_flight"],
            serde_json::Value::Null,
            "a null cap clears the per-model ceiling"
        );
        assert_eq!(zero_weight.0, StatusCode::BAD_REQUEST);
        assert_eq!(zero_cap.0, StatusCode::BAD_REQUEST);
    }

    /// The health prober admits through the same scheduler as real traffic, so
    /// its hidden tenant and group have to be filtered out of every level of
    /// the live view -- and the pool it created on its own must not show up as
    /// a model either.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn fairshare_live_hides_the_health_prober() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let health_tenant = model_health::health_tenant_id();
        let probe = t
            .fairshare
            .admit(
                obleth_fairshare::AdmitRequest::new(health_tenant, "probe-model", 1)
                    .group(model_health::HEALTH_GROUP, 100),
            )
            .await
            .expect("probe admitted");
        let tenant = Uuid::new_v4();
        let real = t
            .fairshare
            .admit(obleth_fairshare::AdmitRequest::new(tenant, "m", 1))
            .await
            .expect("tenant admitted");

        let req = axum::http::Request::get("/api/v1/fairshare/live")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::empty())
            .expect("build request");
        let (status, body) = send(&t.app, req).await;

        drop(probe);
        drop(real);

        assert_eq!(status, StatusCode::OK, "body: {body}");
        let pools = body["pools"].as_array().expect("pools is an array");
        assert!(
            pools
                .iter()
                .any(|p| p["model"] == serde_json::json!("m") && p["in_flight"] == 1),
            "the tenant's pool is reported: {body}"
        );
        assert!(
            !pools
                .iter()
                .any(|p| p["model"] == serde_json::json!("probe-model")),
            "the prober's own pool is not a model an operator can act on: {body}"
        );
        let health_tenant = serde_json::json!(health_tenant.to_string());
        let health_group = serde_json::json!(model_health::HEALTH_GROUP);
        let rows = |level: &str| -> Vec<serde_json::Value> {
            pools
                .iter()
                .flat_map(|p| p[level].as_array().cloned().unwrap_or_default())
                .chain(body[level].as_array().cloned().unwrap_or_default())
                .collect()
        };
        assert!(
            rows("groups").iter().all(|g| g["name"] != health_group),
            "the health group is filtered at every level: {body}"
        );
        assert!(
            rows("tenants")
                .iter()
                .all(|t| t["tenant_id"] != health_tenant && t["fairshare_group"] != health_group),
            "the health tenant is filtered at every level: {body}"
        );
        assert!(
            rows("keys").iter().all(|k| k["tenant_id"] != health_tenant),
            "the health tenant's key is filtered at every level: {body}"
        );
        assert_eq!(
            body["global_in_flight"],
            serde_json::json!(1),
            "only the real request counts"
        );
        assert!(
            body["hard_ceiling"].as_u64().unwrap_or(0) > 0,
            "the total ceiling is reported: {body}"
        );
        assert_eq!(body["default_model_max_in_flight"], serde_json::json!(32));
    }

    /// With several replicas live, the view reports this replica's share of
    /// each limit next to the configured value, and the count it divides by.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn fairshare_live_reports_per_replica_shares() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        t.fairshare.set_replicas(2);
        let admitted = t
            .fairshare
            .admit(obleth_fairshare::AdmitRequest::new(Uuid::new_v4(), "split", 1).model_cap(9))
            .await
            .expect("admitted");
        let get = |path: &str| {
            axum::http::Request::get(path)
                .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
                .body(axum::body::Body::empty())
                .expect("build request")
        };
        let (status, body) = send(&t.app, get("/api/v1/fairshare/live")).await;
        let (stats_status, stats) = send(&t.app, get("/api/v1/stats")).await;
        drop(admitted);

        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["replicas"], serde_json::json!(2));
        assert_eq!(body["replica_aware"], serde_json::json!(true));
        assert_eq!(body["hard_ceiling"], serde_json::json!(32), "ceil(64 / 2)");
        assert_eq!(body["configured_hard_ceiling"], serde_json::json!(64));
        assert!(
            body["configured_max_in_flight"].as_u64() >= body["max_in_flight"].as_u64(),
            "the share never exceeds the configured sum: {body}"
        );
        let pool = body["pools"]
            .as_array()
            .expect("pools")
            .iter()
            .find(|p| p["model"] == serde_json::json!("split"))
            .cloned()
            .expect("the split pool is reported");
        assert_eq!(pool["cap"], serde_json::json!(5), "ceil(9 / 2)");
        assert_eq!(pool["configured_cap"], serde_json::json!(9));
        assert_eq!(stats_status, StatusCode::OK, "body: {stats}");
        assert_eq!(stats["replicas"], serde_json::json!(2));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn fairshare_history_scopes_to_a_pool_or_the_aggregate() {
        let Some(t) = test_admin_app().await else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return;
        };
        use obleth_fairshare::{FairshareSample, GroupSample, PoolSample};
        let pool =
            |model: &str, in_flight: usize, queued: usize, group_in_flight: usize| PoolSample {
                model: model.into(),
                cap: 8,
                in_flight,
                queued,
                groups: vec![
                    GroupSample {
                        name: "research".into(),
                        in_flight: group_in_flight,
                        queued,
                    },
                    GroupSample {
                        name: model_health::HEALTH_GROUP.into(),
                        in_flight: 1,
                        queued: 0,
                    },
                ],
            };
        t.fairshare_history.push(FairshareSample {
            ts_ms: 1_000,
            global_in_flight: 4,
            global_queued: 2,
            pools: vec![pool("m", 3, 1, 3), pool("n", 1, 1, 1)],
        });
        t.fairshare_history.push(FairshareSample {
            ts_ms: 3_000,
            global_in_flight: 6,
            global_queued: 0,
            pools: vec![pool("m", 6, 0, 6)],
        });

        let get = |path: &str| {
            axum::http::Request::get(path)
                .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
                .body(axum::body::Body::empty())
                .expect("build request")
        };

        let (status, body) = send(&t.app, get("/api/v1/fairshare/history?since_ms=0")).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        assert_eq!(body["interval_ms"], 2000);
        assert_eq!(body["retention_ms"], 3_600_000);
        assert_eq!(body["oldest_ts_ms"], 1_000);
        let points = body["points"].as_array().expect("points");
        assert_eq!(points.len(), 2);
        assert_eq!(points[0]["in_flight"], 2, "global 4 minus 2 prober: {body}");
        assert_eq!(points[0]["groups"]["research"], 4);
        assert!(
            points[0]["groups"]
                .get(model_health::HEALTH_GROUP)
                .is_none(),
            "prober group hidden: {body}"
        );

        let (status, body) = send(
            &t.app,
            get("/api/v1/fairshare/history?since_ms=2000&model=m"),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        let points = body["points"].as_array().expect("points");
        assert_eq!(points.len(), 1);
        assert_eq!(points[0]["ts_ms"], 3_000);
        assert_eq!(
            points[0]["in_flight"], 5,
            "model m: 6 minus 1 prober: {body}"
        );
        assert_eq!(points[0]["groups"]["research"], 6);

        let (status, body) =
            send(&t.app, get("/api/v1/fairshare/history?since_ms=0&model=n")).await;
        assert_eq!(status, StatusCode::OK, "body: {body}");
        let points = body["points"].as_array().expect("points");
        assert_eq!(
            points.len(),
            2,
            "a sample without the pool still yields a zero point"
        );
        assert_eq!(points[1]["in_flight"], 0);
        assert!(points[1]["groups"].as_object().expect("groups").is_empty());
    }

    #[test]
    fn cache_sync_errors_are_single_line_and_name_the_resync_route() {
        let redis_err =
            || obleth_redis::RedisError::from(serde_json::from_str::<u8>("x").unwrap_err());
        for err in [
            eviction_failed("the key", redis_err()),
            cache_removal_failed("the disabled model", redis_err()),
        ] {
            let AdminError::CacheSync(msg) = err else {
                panic!("expected CacheSync");
            };
            assert!(!msg.contains('\n'), "{msg:?}");
            assert!(!msg.contains("  "), "{msg:?}");
            assert!(msg.contains(RESYNC_ROUTE), "{msg:?}");
        }
    }

    /// Handlers that once carried `#[utoipa::path]` (or none) without being
    /// listed in `paths(...)`, plus the schemas their annotations reference.
    #[test]
    fn openapi_doc_exposes_previously_unregistered_handlers() {
        use utoipa::OpenApi;
        let doc = serde_json::to_value(ApiDoc::openapi()).expect("serialize the openapi doc");
        for (path, method) in [
            ("/api/v1/resync", "post"),
            ("/api/v1/replicas/{id}/restart", "post"),
            ("/api/v1/keys/{id}/tracing", "put"),
            ("/api/v1/tenants/{id}/tracing", "put"),
            ("/api/v1/usage/logs/{request_id}/spans", "get"),
            ("/api/v1/models/{id}/managed/provision-error", "patch"),
        ] {
            assert!(
                doc["paths"][path][method].is_object(),
                "{method} {path} is missing from paths(...)"
            );
        }
        let schemas = &doc["components"]["schemas"];
        for name in [
            "ResyncReport",
            "SetKeyTracing",
            "ProvisionErrorBody",
            "SpanEntry",
        ] {
            assert!(
                schemas.get(name).is_some(),
                "{name} is not registered in components(...)"
            );
        }
    }

    #[test]
    fn failed_eviction_is_a_502_naming_the_resync_route() {
        // Any RedisError will do; a serde one is constructible without a server.
        let cause = serde_json::from_str::<i32>("x").unwrap_err();
        let err = eviction_failed("the key", obleth_redis::RedisError::Serde(cause));
        assert!(err.to_string().contains(RESYNC_ROUTE), "{err}");
        let resp = axum::response::IntoResponse::into_response(err);
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    /// A handler can carry `#[utoipa::path]` and still be missing from the
    /// document, and an unregistered schema leaves a dangling `$ref` rather
    /// than a compile error. Pin both halves.
    #[test]
    fn openapi_doc_exposes_the_simulate_endpoint() {
        use utoipa::OpenApi;
        let doc = serde_json::to_value(ApiDoc::openapi()).expect("serialize the openapi doc");
        assert!(
            doc["paths"]["/api/v1/router/simulate"]["post"].is_object(),
            "the simulate route is missing from paths(...)"
        );
        let schemas = &doc["components"]["schemas"];
        for name in [
            "SimulateRouteRequest",
            "RouteExplain",
            "ScoredCandidate",
            "Rejection",
            "WeightsView",
            "IntentSource",
        ] {
            assert!(
                schemas.get(name).is_some(),
                "{name} is not registered in components(...)"
            );
        }
    }

    #[test]
    fn update_auto_router_clamps_weights_and_preserves_unset() {
        use obleth_config::AutoRouterSettings;
        let existing = AutoRouterSettings {
            capacity_weight: 0.9,
            temperature: 0.3,
            ..Default::default()
        };
        let body = UpdateAutoRouterSettings {
            cost_weight: Some(0.7),
            temperature: Some(-5.0), // out of range, must clamp to 0.0
            ..Default::default()
        };
        let merged = merge_auto_router(&existing, &body);
        assert_eq!(merged.cost_weight, 0.7);
        assert_eq!(
            merged.capacity_weight, 0.9,
            "unset fields keep their existing value"
        );
        assert_eq!(
            merged.temperature, 0.0,
            "negative temperature clamps to argmax"
        );
    }

    #[test]
    fn update_auto_router_rejects_zero_soft_cap() {
        use obleth_config::AutoRouterSettings;
        let existing = AutoRouterSettings::default();
        let body = UpdateAutoRouterSettings {
            default_soft_cap: Some(0),
            ..Default::default()
        };
        assert_eq!(merge_auto_router(&existing, &body).default_soft_cap, 8);
    }

    #[test]
    fn update_auto_router_preserves_tier_source_on_unrecognized_string() {
        use obleth_config::{AutoRouterSettings, TierSource};
        let existing = AutoRouterSettings {
            tier_source: TierSource::Declared,
            ..Default::default()
        };
        let body = UpdateAutoRouterSettings {
            tier_source: Some("bogus".to_string()),
            ..Default::default()
        };
        assert_eq!(
            merge_auto_router(&existing, &body).tier_source,
            TierSource::Declared
        );

        let body_absent = UpdateAutoRouterSettings::default();
        assert_eq!(
            merge_auto_router(&existing, &body_absent).tier_source,
            TierSource::Declared
        );

        let body_valid = UpdateAutoRouterSettings {
            tier_source: Some("derived".to_string()),
            ..Default::default()
        };
        assert_eq!(
            merge_auto_router(&existing, &body_valid).tier_source,
            TierSource::Derived
        );
    }

    #[test]
    fn auto_router_view_round_trips_scoring_fields() {
        use obleth_config::{AutoRouterSettings, TierSource};
        let s = AutoRouterSettings {
            capacity_weight: 0.7,
            cost_weight: 0.3,
            tag_weight: 0.4,
            default_soft_cap: 12,
            temperature: 0.5,
            difficulty_enabled: true,
            tier_source: TierSource::Declared,
            ..Default::default()
        };
        let view = AutoRouterSettingsView::from_settings(&s);
        assert_eq!(view.capacity_weight, 0.7);
        assert_eq!(view.cost_weight, 0.3);
        assert_eq!(view.tag_weight, 0.4);
        assert_eq!(view.default_soft_cap, 12);
        assert_eq!(view.temperature, 0.5);
        assert!(view.difficulty_enabled);
        assert_eq!(view.tier_source, "declared");
    }

    #[test]
    fn merge_auto_router_sets_and_clears_messages_default_model() {
        use obleth_config::AutoRouterSettings;
        let existing = AutoRouterSettings::default();
        let set = merge_auto_router(
            &existing,
            &UpdateAutoRouterSettings {
                messages_default_model: Some("local-llama".into()),
                ..Default::default()
            },
        );
        assert_eq!(set.messages_default_model.as_deref(), Some("local-llama"));
        let cleared = merge_auto_router(
            &set,
            &UpdateAutoRouterSettings {
                messages_default_model: Some("".into()),
                ..Default::default()
            },
        );
        assert_eq!(cleared.messages_default_model, None);
        let kept = merge_auto_router(&set, &UpdateAutoRouterSettings::default());
        assert_eq!(kept.messages_default_model.as_deref(), Some("local-llama"));
    }

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
    fn boon_view_update_clamps_the_tool_loop_deadline() {
        assert_eq!(merge_tool_loop_deadline(None, 300), 300);
        assert_eq!(
            merge_tool_loop_deadline(Some(0), 300),
            300,
            "0 means unchanged"
        );
        assert_eq!(merge_tool_loop_deadline(Some(120), 300), 120);
        assert_eq!(
            merge_tool_loop_deadline(Some(u64::MAX), 300),
            TOOL_LOOP_MAX_DEADLINE_SECS
        );
        assert_eq!(
            merge_tool_loop_deadline(Some(TOOL_LOOP_MAX_DEADLINE_SECS + 1), 300),
            TOOL_LOOP_MAX_DEADLINE_SECS
        );
    }

    #[test]
    fn boon_view_exposes_the_tool_loop_deadline() {
        let mut s = BoonSettings::default();
        assert_eq!(
            BoonSettingsView::from_settings(&s).tool_loop_deadline_secs,
            300
        );
        s.tool_loop.deadline_secs = 90;
        assert_eq!(
            BoonSettingsView::from_settings(&s).tool_loop_deadline_secs,
            90
        );
        let update: UpdateBoonSettings =
            serde_json::from_value(serde_json::json!({ "tool_loop_deadline_secs": 120 })).unwrap();
        assert_eq!(update.tool_loop_deadline_secs, Some(120));
    }

    #[test]
    fn boon_view_exposes_image_generation_settings() {
        let s = BoonSettings {
            image_generation: obleth_config::ImageGenerationBoonSettings {
                enabled: true,
                image_model: Some("sdxl".to_string()),
                allowed_sizes: vec!["768x768".to_string()],
                max_images_per_request: 3,
                timeout_ms: 90_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let view = BoonSettingsView::from_settings(&s);
        assert!(view.image_generation_enabled);
        assert_eq!(view.image_generation_model.as_deref(), Some("sdxl"));
        assert_eq!(
            view.image_generation_allowed_sizes,
            vec!["768x768".to_string()]
        );
        assert_eq!(view.image_generation_max_images_per_request, 3);
        assert_eq!(view.image_generation_timeout_ms, 90_000);
        assert!(!view.image_generation_tool_description.is_empty());
    }

    #[test]
    fn image_generation_sizes_are_normalised() {
        // Blank and duplicate entries are dropped; an all-blank list falls back
        // to the default rather than leaving the tool schema with an empty enum.
        assert_eq!(
            normalize_image_sizes(vec![" 512x512 ".into(), "".into(), "512x512".into()]),
            vec!["512x512".to_string()]
        );
        assert_eq!(
            normalize_image_sizes(vec!["   ".into()]),
            obleth_config::ImageGenerationBoonSettings::default().allowed_sizes
        );
    }

    #[test]
    fn image_generation_count_is_clamped_to_the_ceiling() {
        assert_eq!(
            clamp_image_count(99),
            obleth_config::IMAGE_GENERATION_MAX_PER_REQUEST
        );
        assert_eq!(clamp_image_count(1), 1);
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
            power_query: "node_power_watts".into(),
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

    /// A model route with every field filled with sane defaults, for tests
    /// that only care about a couple of fields (e.g. `enabled`/`max_in_flight`).
    fn fixture_model_route(name: &str) -> ModelRoute {
        let now = chrono::Utc::now();
        ModelRoute {
            id: Uuid::new_v4(),
            model_name: name.to_string(),
            aliases: Vec::new(),
            description: String::new(),
            upstream_model: name.to_string(),
            api_base: "http://upstream.invalid".to_string(),
            api_key: None,
            upstream_headers: Default::default(),
            model_type: obleth_config::DEFAULT_MODEL_TYPE.to_string(),
            quantization: obleth_config::DEFAULT_QUANTIZATION.to_string(),
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            cost_per_video: 0.0,
            context_window: 128_000,
            admission_weight: 100,
            max_in_flight: None,
            capacity_mode: obleth_config::DEFAULT_CAPACITY_MODE.to_string(),
            capacity_tuned_at: None,
            capacity_source: "endpoints".into(),
            capacity_namespace: None,
            capacity_selector: None,
            per_replica_max_in_flight: None,
            capacity_headroom: 1.0,
            supports_function_calling: false,
            supports_system_messages: false,
            supports_response_schema: false,
            supports_tool_choice: false,
            supports_vision: false,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 0,
            tags: Vec::new(),
            boons: Vec::new(),
            tool_servers: Vec::new(),
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: obleth_config::DEFAULT_RETRY_BACKOFF_MS,
            endpoint_selection_mode: obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string(),
            debug_diagnostics: false,
            energy_slots_per_node: 0,
            route_bias: obleth_config::DEFAULT_ROUTE_BIAS,
            auto_eligible: obleth_config::DEFAULT_AUTO_ELIGIBLE,
            draft_model: String::new(),
            verify_api_base: String::new(),
            verify_upstream_model: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    #[test]
    fn model_route_view_redacts_api_key() {
        let mut route = fixture_model_route("m");
        route.api_key = Some("sk-upstream-secret".into());
        let v = serde_json::to_value(ModelRouteView::from(route.clone())).unwrap();
        assert_eq!(v["api_key_set"], serde_json::json!(true));
        assert!(v.get("api_key").is_none(), "view leaked api_key: {v}");
        assert!(!v.to_string().contains("sk-upstream-secret"));
        assert_eq!(v["model_name"], "m");

        route.api_key = None;
        let v = serde_json::to_value(ModelRouteView::from(route.clone())).unwrap();
        assert_eq!(v["api_key_set"], serde_json::json!(false));
        // An empty stored key is "no key" — the proxy sends no auth for it.
        route.api_key = Some(String::new());
        let v = serde_json::to_value(ModelRouteView::from(route)).unwrap();
        assert_eq!(v["api_key_set"], serde_json::json!(false));
    }

    #[test]
    fn model_route_view_lists_upstream_header_names_but_never_values() {
        let mut route = fixture_model_route("m");
        route.upstream_headers = [
            ("x-routing-hint".to_string(), "sticky".to_string()),
            (
                "x-upstream-token".to_string(),
                "tok-upstream-secret".to_string(),
            ),
        ]
        .into();
        let v = serde_json::to_value(ModelRouteView::from(route)).unwrap();
        assert_eq!(
            v["upstream_header_names"],
            serde_json::json!(["x-routing-hint", "x-upstream-token"])
        );
        assert!(
            v.get("upstream_headers").is_none(),
            "view leaked values: {v}"
        );
        assert!(!v.to_string().contains("tok-upstream-secret"));
        assert!(!v.to_string().contains("sticky"));
    }

    #[test]
    fn model_writes_take_upstream_headers_with_null_meaning_keep() {
        let create: CreateModel = serde_json::from_value(serde_json::json!({
            "model_name": "m", "upstream_model": "m", "api_base": "",
            "upstream_headers": {"X-Routing-Hint": "sticky"}
        }))
        .unwrap();
        let write = create.upstream_headers.unwrap();
        assert_eq!(write["X-Routing-Hint"].as_deref(), Some("sticky"));

        let update: UpdateModel = serde_json::from_value(serde_json::json!({
            "upstream_model": "m", "api_base": "",
            "upstream_headers": {"x-upstream-token": null}
        }))
        .unwrap();
        assert_eq!(update.upstream_headers.unwrap()["x-upstream-token"], None);
        let omitted: UpdateModel =
            serde_json::from_value(serde_json::json!({"upstream_model": "m", "api_base": ""}))
                .unwrap();
        assert!(
            omitted.upstream_headers.is_none(),
            "absent means keep them all"
        );
    }

    #[test]
    fn upstream_header_map_holds_the_validated_headers() {
        let headers: obleth_config::UpstreamHeaders =
            [("x-routing-hint".to_string(), "sticky".to_string())].into();
        let map = upstream_header_map(&headers);
        assert_eq!(map.len(), 1);
        assert_eq!(map["x-routing-hint"], "sticky");
    }

    #[test]
    fn model_endpoint_view_redacts_api_key() {
        let now = chrono::Utc::now();
        let ep = ModelEndpoint {
            id: Uuid::new_v4(),
            model_id: Uuid::new_v4(),
            name: "primary".into(),
            api_base: "http://upstream.invalid/v1".into(),
            api_key: Some("sk-endpoint-secret".into()),
            priority: 0,
            weight: 100,
            enabled: true,
            max_in_flight: None,
            health_status: "unknown".into(),
            consecutive_failures: 0,
            alert_state: "ok".into(),
            last_checked_at: None,
            last_latency_ms: None,
            last_http_status: None,
            last_message: None,
            created_at: now,
            updated_at: now,
        };
        let v = serde_json::to_value(ModelEndpointView::from(ep.clone())).unwrap();
        assert_eq!(v["api_key_set"], serde_json::json!(true));
        assert!(v.get("api_key").is_none(), "view leaked api_key: {v}");
        assert!(!v.to_string().contains("sk-endpoint-secret"));
        assert_eq!(v["name"], "primary");

        let v = serde_json::to_value(ModelEndpointView::from(ModelEndpoint {
            api_key: None,
            ..ep
        }))
        .unwrap();
        assert_eq!(v["api_key_set"], serde_json::json!(false));
    }

    #[test]
    fn mcp_server_view_redacts_auth_header() {
        let now = chrono::Utc::now();
        let server = McpServer {
            id: Uuid::new_v4(),
            name: "files".into(),
            upstream_url: "http://mcp.invalid/mcp".into(),
            auth_header: Some("Bearer mcp-secret".into()),
            enabled: true,
            created_at: now,
            updated_at: now,
        };
        let v = serde_json::to_value(McpServerView::from(server.clone())).unwrap();
        assert_eq!(v["auth_header_set"], serde_json::json!(true));
        assert!(
            v.get("auth_header").is_none(),
            "view leaked auth_header: {v}"
        );
        assert!(!v.to_string().contains("mcp-secret"));
        assert_eq!(v["name"], "files");

        let v = serde_json::to_value(McpServerView::from(McpServer {
            auth_header: None,
            ..server
        }))
        .unwrap();
        assert_eq!(v["auth_header_set"], serde_json::json!(false));
    }

    fn pool(model: &str, cap: usize, tenants: Vec<TenantFairshareView>) -> ModelPoolView {
        let in_flight = tenants.iter().map(|t| t.in_flight).sum();
        ModelPoolView {
            model: model.into(),
            cap,
            configured_cap: cap,
            in_flight,
            queued: 0,
            borrowed: 0,
            groups: vec![GroupFairshareView {
                name: "g".into(),
                weight: 100,
                in_flight,
                queued: 0,
                slot_cap: cap,
                borrowed: 0,
                served_tokens: 0.0,
                share_score: 0.0,
                weight_share: 1.0,
                expected_slots: cap as f64,
            }],
            tenants,
            keys: vec![],
        }
    }
    fn tv(
        id: Uuid,
        in_flight: usize,
        served: f64,
        share: f64,
        expected: f64,
    ) -> TenantFairshareView {
        TenantFairshareView {
            tenant_id: id,
            name: "t".into(),
            fairshare_group: "g".into(),
            weight: 100,
            max_in_flight: None,
            in_flight,
            queued: 0,
            served_tokens: served,
            share_score: served / 100.0,
            weight_share: share,
            expected_slots: expected,
        }
    }

    #[test]
    fn aggregate_pools_sums_occupancy_and_shares_by_pool_capacity() {
        let t = Uuid::new_v4();
        let pools = vec![
            pool("a", 8, vec![tv(t, 2, 100.0, 0.5, 4.0)]),
            pool("b", 2, vec![tv(t, 1, 50.0, 1.0, 2.0)]),
        ];
        let (groups, tenants, _) = aggregate_pools(&pools);
        assert_eq!(tenants.len(), 1);
        assert_eq!(tenants[0].in_flight, 3);
        assert_eq!(tenants[0].served_tokens, 150.0);
        assert_eq!(tenants[0].expected_slots, 6.0);
        assert!((tenants[0].weight_share - 0.6).abs() < 1e-9); // 6 of 10 slots
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].slot_cap, 10);
    }

    #[test]
    fn enabled_pool_capacity_uses_model_cap_or_default() {
        let mut a = fixture_model_route("a");
        a.max_in_flight = Some(4);
        let mut b = fixture_model_route("b");
        b.max_in_flight = None;
        let mut c = fixture_model_route("c");
        c.enabled = false;
        c.max_in_flight = Some(100);
        assert_eq!(enabled_pool_capacity(&[a, b, c], 32), 36);
    }

    fn routes_with_caps(caps: &[Option<i64>]) -> Vec<ModelRoute> {
        caps.iter()
            .enumerate()
            .map(|(i, cap)| {
                let mut m = fixture_model_route(&format!("m{i}"));
                m.max_in_flight = *cap;
                m
            })
            .collect()
    }

    #[test]
    fn enabled_pool_share_rounds_each_pool_up() {
        let models = routes_with_caps(&[Some(4), None, Some(1)]);
        assert_eq!(enabled_pool_share(&models, 32, 1), 37);
        // ceil(4/3) + ceil(32/3) + ceil(1/3) = 2 + 11 + 1
        assert_eq!(enabled_pool_share(&models, 32, 3), 14);
    }

    #[test]
    fn ceiling_check_compares_like_with_like() {
        let models = routes_with_caps(&[Some(8), Some(8)]);
        // Configured ceiling under the configured pool sum.
        let w = ceiling_check(&models, 32, 12, 1).expect("warns");
        assert_eq!((w.ceiling, w.pool_sum), (12, 16));
        assert!(w.message.contains("OBLETH_GLOBAL_MAX_IN_FLIGHT is below"));
        // Covered, and still covered per replica: 3 replicas hold 3 + 3
        // slots of pools against a 6-slot share of the ceiling.
        assert_eq!(ceiling_check(&models, 32, 16, 3), None);
        assert_eq!(ceiling_check(&models, 32, 64, 5), None);

        // 64 one-slot pools under a 64 ceiling fit configured, but over 2
        // replicas every pool rounds up to 1 while the ceiling halves.
        let tiny = routes_with_caps(&[Some(1); 64]);
        assert_eq!(ceiling_check(&tiny, 32, 64, 1), None);
        let w = ceiling_check(&tiny, 32, 64, 2).expect("warns per replica");
        assert_eq!((w.ceiling, w.pool_sum), (32, 64));
        assert!(w.message.contains("round up per model"));
    }
}
