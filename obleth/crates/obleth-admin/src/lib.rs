//! Management API: the versioned, authenticated control surface.
//!
//! Mounted on a separate admin port from the data plane. Every config write
//! follows a single path — **Postgres (durable) -> Redis (cache) -> pub/sub
//! invalidate** — and is recorded in the audit log. Usage/cost reads hit
//! ClickHouse. The Next.js dashboard and any CLI/Terraform consume these exact
//! endpoints.

pub mod advice;
pub mod alerts;
mod audit;
pub mod autotune;
mod backup;
mod batches;
pub mod capacity_discovery;
mod endpoints;
pub mod energy_probe;
mod error;
mod keys;
mod keys_page;
pub mod knowledge;
mod live;
pub mod live_catalog;
pub mod live_load;
mod managed;
mod mcp;
pub mod model_health;
mod models;
mod models_io;
mod names;
mod openapi;
pub mod recipes;
pub mod replica_settings;
pub mod rewarm;
pub mod router_readiness;
mod settings;
pub mod slurm_resources;
pub mod slurm_settings;
pub mod ssrf;
mod sync;
mod targets;
mod tenants;
#[cfg(test)]
mod tests;
mod usage;
pub mod usage_cache;
pub mod usage_retention;
mod usage_routes;
mod views;

use std::sync::Arc;

use crate::alerts::ChannelResult;
use crate::autotune::{AutotuneReport, AutotuneRequest};
use crate::capacity_discovery::{ModelCapacityStatus, ServiceSummary};
use crate::replica_settings::CeilingSource;
use crate::usage::{
    CacheStats, CostAgg, KeyRejectionAgg, KeySource, KeyUsageSummary, ModelSpeedPoint,
    ModelUsageTimePoint, OverviewTenantUsage, SpanEntry, TenantUsageTimePoint, UsageAgg,
    UsageDailyRow, UsageFailureGroups, UsageKeyAgg, UsageKeyModelBreakdown, UsageLogFacets,
    UsageLogHistogram, UsageLogRow, UsageModelAgg, UsageTimePoint,
};
use axum::extract::{Path, Query, State};
use axum::http::{header::AUTHORIZATION, HeaderMap, Request, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{delete, get, patch, post, put};
use axum::{Json, Router};
use obleth_config::network::{NamedNetwork, NetworkNames};
use obleth_config::routing::route_explain::RouteExplain;
use obleth_config::routing::{
    classifier_prompt, difficulty_from_header, explain_selection, heuristic_intent, BoonGrants,
    Classified, IntentSource, RequestFeatures, RouterWeights,
};
use obleth_config::{
    hash_api_key, ApiKey, DeploymentLaunch, FairshareGroup, ManagedModelSpec, McpServer,
    ModelEndpoint, ModelReplica, ModelRoute, ResolvedKey, ResolvedMcpServer, ResolvedModel, Tenant,
};
use obleth_config::{
    AlertSettings, AutoRouterSettings, BoonSettings, EmailSettings, StructuredOutputBoonSettings,
    ToolLoopSettings, VisionBoonSettings, STRUCTURED_OUTPUT_MAX_REPAIR_ATTEMPTS,
    TOOL_LOOP_MAX_DEADLINE_SECS, TOOL_LOOP_MAX_TURNS,
};
use obleth_config::{
    CompressionPolicy, FamilyMembers, GuardrailsPolicy, ManagedBackoff, ModelFamily,
    ModelLifecycle, ModelReasoning, ModelSessionAffinity, ModelStatus, ModelVariant,
    ReasoningLevel, WeeklyWindow,
};
use obleth_fairshare::{
    replica_share, FairShare, FairshareHistory, ModelPoolFairshare, PoolKey, SlotMode, Stats,
};
use obleth_redis::{Invalidation, RedisStore, Resolver, SavedSetting};
use obleth_store::{AuditEntry, KeyFlag, Store};
use obleth_tokenizer::Tokenizer;
use rand::Rng;
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;
use utoipa::ToSchema;
use uuid::Uuid;

pub use alerts::AlertDispatcher;
pub use audit::*;
pub use endpoints::*;
pub use error::AdminError;
pub use keys::*;
pub use live::*;
pub use managed::*;
pub use mcp::*;
pub use model_health::{AlertSink, ModelHealthRuntime};
pub use models::*;
pub use openapi::ApiDoc;
pub use settings::*;
pub use sync::*;
pub use targets::*;
pub use tenants::*;
pub use usage::READ_MAX_EXECUTION_SECS;
pub use usage_routes::*;
pub use views::*;

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

/// Update bodies follow one rule: an omitted field keeps its stored value,
/// and `null` clears a field that can be empty. A plain `Option<T>` with
/// `#[serde(default)]` gives the first half; this helper gives both halves
/// on an `Option<Option<T>>` with `#[serde(default, deserialize_with =
/// "nullable")]` (and `#[schema(value_type = Option<T>)]`). An update body
/// never fills an omitted field with a default of its own.
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
    /// The global in-flight ceiling the scheduler reads, saved or from
    /// `OBLETH_GLOBAL_MAX_IN_FLIGHT`.
    pub ceiling: Arc<replica_settings::GlobalCeiling>,
    pub fairshare: FairShare,
    /// Per-model in-flight cap used for models that don't set their own
    /// `max_in_flight`, sourced from `cfg.default_model_max_in_flight`.
    pub default_model_max_in_flight: usize,
    /// Sampled scheduler history for `/api/v1/fairshare/history`.
    pub fairshare_history: Arc<FairshareHistory>,
    /// Configured retention in seconds; `0` means the sampler is off.
    pub fairshare_history_secs: u64,
    /// Whether fairshare divides its limits across the live replicas when
    /// shared slots are off or unavailable (`OBLETH_FAIRSHARE_REPLICA_AWARE`).
    pub fairshare_replica_aware: bool,
    /// Whether fairshare limits are cluster-wide slots in Redis
    /// (`OBLETH_FAIRSHARE_SHARED_SLOTS`). Reported by the live view.
    pub fairshare_shared_slots: bool,
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
    /// Bridge to the data plane's `/v1/verdicts`, used to judge advice
    /// findings. Injected by the binary that owns the data plane; `None`
    /// (a standalone admin process) leaves findings unjudged.
    pub judge: Option<advice::JudgeFn>,
    /// Models and endpoints for `/fairshare/live`, re-read at most every
    /// [`live_catalog::LIVE_CATALOG_TTL`].
    pub live_catalog: Arc<live_catalog::LiveCatalog>,
    /// The footer totals and the Overview's usage, re-read at most every
    /// [`usage_cache::USAGE_CACHE_TTL`].
    pub usage_cache: Arc<usage_cache::UsageCache>,
    /// The client the Slurm connection test and resource reads share.
    pub slurm_http: slurm_settings::SlurmHttp,
    /// How the gateway finds a request's client (`Config::client_address`),
    /// shown on the network settings.
    pub client_address: obleth_config::network::ClientAddressSettings,
    /// The prompt-compressor sidecar (`Config::compressor`), probed by the
    /// compressor status.
    pub compressor: obleth_config::CompressorConfig,
}

impl AdminState {
    /// What re-reads this replica's alert settings and global ceiling.
    pub fn replica_settings(&self) -> replica_settings::ReplicaSettings {
        replica_settings::ReplicaSettings::new(
            self.store.clone(),
            self.alerts.clone(),
            self.ceiling.clone(),
        )
    }

    /// Record an audit row for a change made by the request's caller (the
    /// actor comes from its headers, see [`audit_actor`]). Action names are
    /// `<verb>_<entity>` or, for one field, `set_<entity>_<field>`; stored
    /// rows keep the names they were written with.
    pub(crate) async fn audit(
        &self,
        headers: &HeaderMap,
        action: &str,
        entity_type: &str,
        entity_id: impl std::fmt::Display,
        detail: serde_json::Value,
    ) -> Result<()> {
        self.store
            .record_audit(
                &audit_actor(headers),
                action,
                entity_type,
                &entity_id.to_string(),
                detail,
            )
            .await?;
        Ok(())
    }
}

/// A boxed call into the data plane's classifier: `(prompt, available_tags)`
/// to what it said ([`obleth_config::routing::Classified`]). Timeout and
/// caching live behind the closure, in the classifier itself; an unanswered
/// question comes back as no answer, for the heuristics to settle.
pub type ClassifyFn = std::sync::Arc<
    dyn Fn(
            String,
            Vec<String>,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = obleth_config::routing::Classified> + Send>,
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
        .route(
            "/api/v1/tenants/:id/status",
            put(set_tenant_status).patch(set_tenant_status),
        )
        .route(
            "/api/v1/tenants/:id/schedule",
            put(set_tenant_schedule).patch(set_tenant_schedule),
        )
        .route(
            "/api/v1/tenants/:id/budget",
            put(set_tenant_budget).patch(set_tenant_budget),
        )
        .route(
            "/api/v1/tenants/:id/allowlist",
            put(set_tenant_allowlist).patch(set_tenant_allowlist),
        )
        .route(
            "/api/v1/tenants/:id/guardrails",
            put(set_tenant_guardrails).patch(set_tenant_guardrails),
        )
        .route(
            "/api/v1/tenants/:id/compression",
            put(set_tenant_compression).patch(set_tenant_compression),
        )
        .route(
            "/api/v1/tenants/:id/weight",
            put(set_tenant_weight).patch(set_tenant_weight),
        )
        .route("/api/v1/tenants/:id/quota", put(set_tenant_quota))
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
        .route("/api/v1/keys/count", get(keys_page::count_keys))
        .route("/api/v1/keys/stats", get(keys_page::key_stats))
        .route("/api/v1/budgets/usage", get(get_budget_usage))
        .route("/api/v1/resync", post(resync_resolver_cache))
        .route(
            "/api/v1/keys/:id",
            get(get_key).put(update_key).delete(delete_key),
        )
        .route("/api/v1/keys/:id/disabled", put(set_key_disabled))
        .route("/api/v1/keys/:id/tenant", put(move_key))
        .route("/api/v1/keys/:id/tracing", put(set_key_tracing_handler))
        .route(
            "/api/v1/keys/:id/end-user-fairshare",
            put(set_key_end_user_fairshare_handler),
        )
        .route(
            "/api/v1/keys/:id/background",
            put(set_key_background_handler),
        )
        .route("/api/v1/keys/:id/usage", get(get_key_usage))
        .route("/api/v1/usage", get(get_usage))
        .route("/api/v1/usage/keys", get(get_usage_keys))
        .route("/api/v1/usage/keys/summary", get(get_usage_keys_summary))
        .route("/api/v1/usage/key-sources", get(get_usage_key_sources))
        .route(
            "/api/v1/usage/key-rejections",
            get(get_usage_key_rejections),
        )
        .route("/api/v1/usage/models", get(get_usage_models))
        .route("/api/v1/usage/series", get(get_usage_series))
        .route(
            "/api/v1/usage/series/tenants",
            get(get_usage_series_tenants),
        )
        .route("/api/v1/usage/series/models", get(get_usage_series_models))
        .route("/api/v1/usage/speed", get(get_usage_speed))
        .route("/api/v1/usage/breakdown", get(get_usage_breakdown))
        .route("/api/v1/usage/cache", get(get_cache_stats))
        .route("/api/v1/usage/logs", get(get_usage_logs))
        .route("/api/v1/usage/logs/histogram", get(get_usage_log_histogram))
        .route("/api/v1/usage/logs/facets", get(get_usage_log_facets))
        .route("/api/v1/usage/failures", get(get_usage_failure_groups))
        .route(
            "/api/v1/usage/logs/:request_id/spans",
            get(get_request_spans),
        )
        .route("/api/v1/usage/daily", get(get_usage_daily))
        .route("/api/v1/usage/compact", post(compact_usage))
        .route("/api/v1/costs", get(get_costs))
        .route("/api/v1/stats", get(get_stats))
        .route("/api/v1/overview", get(get_overview))
        .route("/api/v1/overview/summary", get(get_overview_summary))
        .route("/api/v1/fairshare/live", get(get_fairshare_live))
        .route("/api/v1/capacity/discovery", get(get_capacity_discovery))
        .route("/api/v1/capacity/services", get(get_capacity_services))
        .route("/api/v1/fairshare/history", get(get_fairshare_history))
        .route(
            "/api/v1/fairshare/groups",
            post(create_fairshare_group).get(list_fairshare_groups),
        )
        .route(
            "/api/v1/fairshare/groups/:name/weight",
            put(set_fairshare_group_weight).patch(set_fairshare_group_weight),
        )
        .route(
            "/api/v1/tenants/:id/group",
            put(set_tenant_group).patch(set_tenant_group),
        )
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
            "/api/v1/models/:id/health/activate",
            post(model_health::activate),
        )
        .route(
            "/api/v1/models/:id/health/config",
            put(model_health::update_config),
        )
        .route("/api/v1/models/:id/weight", put(set_model_weight))
        .route("/api/v1/models/:id/capacity", put(set_model_capacity))
        .route("/api/v1/models/:id/status", put(set_model_status))
        .route("/api/v1/models/:id/reasoning", put(set_model_reasoning))
        .route("/api/v1/models/:id/listing", put(set_model_listing))
        .route(
            "/api/v1/models/:id/rejected-request-fields",
            put(set_model_rejected_request_fields),
        )
        .route(
            "/api/v1/models/:id/session-affinity",
            put(set_model_session_affinity),
        )
        .route(
            "/api/v1/model-families",
            get(list_model_families).post(create_model_family),
        )
        .route(
            "/api/v1/model-families/:id",
            put(update_model_family).delete(delete_model_family),
        )
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
        .route(
            "/api/v1/models/:id/managed/retry",
            post(retry_managed_model),
        )
        .route("/api/v1/models/:id/managed/roll", post(roll_managed_model))
        .route(
            "/api/v1/models/:id/managed/backoff",
            put(set_managed_backoff),
        )
        .route("/api/v1/replicas", get(list_all_replicas))
        .route("/api/v1/endpoints", get(list_all_endpoints))
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
            "/api/v1/deployments/launches",
            get(list_deployment_launches),
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
        .route("/api/v1/batches", get(batches::list_batches))
        .route("/api/v1/batches/:id", get(batches::get_batch))
        .route("/api/v1/batches/:id/cancel", post(batches::cancel_batch))
        .route("/api/v1/stats/daily", get(get_daily_stats))
        .route(
            "/api/v1/capacity",
            get(get_capacity).put(set_capacity).delete(clear_capacity),
        )
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
        .route("/api/v1/advice", get(advice::get_advice))
        .route(
            "/api/v1/advice/snoozed/:key",
            put(advice::put_snoozed).delete(advice::delete_snoozed),
        )
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
            "/api/v1/settings/key-prefix",
            get(get_key_prefix).put(put_key_prefix),
        )
        .route(
            "/api/v1/settings/networks",
            get(get_network_settings).put(put_network_settings),
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

fn default_true() -> bool {
    true
}

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
