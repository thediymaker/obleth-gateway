//! Shared domain types used across the data plane, store, cache and admin API.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

/// How global concurrency is divided among tenants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FairshareAlgorithm {
    /// Per-tenant weighted fair queuing (`served / tenant_weight`).
    #[default]
    Weighted,
    /// Group-weighted capacity pools; weight-proportional split among tenants in each group.
    Hierarchical,
}

impl FairshareAlgorithm {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "hierarchical" | "group" | "groups" => Self::Hierarchical,
            _ => Self::Weighted,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Weighted => "weighted",
            Self::Hierarchical => "hierarchical",
        }
    }
}

/// Fairshare group — capacity is partitioned by group weight under the hierarchical algorithm.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FairshareGroup {
    pub name: String,
    pub weight: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// A tenant is the unit of fairshare. Every API key belongs to exactly one tenant.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    /// Fairshare group for hierarchical admission. Weights between groups; equal split within.
    pub fairshare_group: String,
    /// Fairshare weight (used by the weighted algorithm; token-budget priority hint). Must be >= 1.
    pub weight: i64,
    /// Sustained token budget refilled per minute (token-bucket rate).
    pub tokens_per_minute: i64,
    /// Optional per-tenant in-flight cap. `None` means only the global limit applies.
    pub max_in_flight: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// An API key. The raw secret is never stored; only its hash + a display prefix.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKey {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    /// Display-only prefix, e.g. `sk_a1b2...` for dashboards.
    pub key_prefix: String,
    pub disabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// The compact, hot-path view resolved from an API key. This is what the data
/// plane caches in moka and reads from Redis; it carries everything the
/// fairshare admission step needs without a relational lookup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedKey {
    pub key_id: Uuid,
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub fairshare_group: String,
    pub group_weight: i64,
    pub weight: i64,
    pub tokens_per_minute: i64,
    pub max_in_flight: Option<i64>,
    pub disabled: bool,
    /// Internal keys are allowed through the proxy path but are omitted from
    /// the normal usage ledger. This is used for gateway-owned health probes.
    #[serde(default)]
    pub internal: bool,
}

/// Outcome of admission for a single request, recorded in telemetry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Admission {
    /// Admitted immediately (capacity available).
    Fast,
    /// Admitted after waiting in the weighted queue.
    Queued,
    /// Admitted but degraded (capped max_tokens / downgraded) under saturation.
    Brownout,
    /// Rejected (budget exhausted or fail-closed).
    Rejected,
}

impl Admission {
    pub fn as_str(self) -> &'static str {
        match self {
            Admission::Fast => "fast",
            Admission::Queued => "queued",
            Admission::Brownout => "brownout",
            Admission::Rejected => "rejected",
        }
    }
}

/// Registered model route. Client-facing `model_name` maps to an upstream
/// OpenAI-compatible endpoint (Aibrix envoy, vLLM service, or external API).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelRoute {
    pub id: Uuid,
    /// Name clients pass in `model` (e.g. `qwen3-vl-32b-instruct`).
    pub model_name: String,
    /// Human-facing summary for operators and dashboards.
    pub description: String,
    /// Value sent to the upstream in the `model` field.
    pub upstream_model: String,
    /// Base URL including `/v1` suffix when required.
    pub api_base: String,
    /// Optional bearer/api key for the upstream (stored encrypted-at-rest in prod).
    pub api_key: Option<String>,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub context_window: i64,
    /// Multiplier applied to tenant weight at admission when this model is used.
    pub admission_weight: i64,
    /// Optional per-model in-flight cap. `None` means only the global scheduler
    /// cap and tenant/group fairshare limits apply.
    pub max_in_flight: Option<i64>,
    pub supports_function_calling: bool,
    pub supports_system_messages: bool,
    pub supports_response_schema: bool,
    pub supports_tool_choice: bool,
    pub enabled: bool,
    /// When true, identical requests to this model are served from the response
    /// cache (exact-match on model + request body) instead of the upstream.
    pub cache_enabled: bool,
    /// Time-to-live for cached responses, in seconds.
    pub cache_ttl_secs: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Persisted model-health status. Stored as snake_case text in Postgres so new
/// UI/API readers can remain forward-compatible with older rows.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelHealthSummary {
    #[schema(value_type = String)]
    pub model_id: Uuid,
    pub model_name: String,
    pub checks_enabled: bool,
    pub alerts_enabled: bool,
    pub check_interval_secs: i64,
    pub failure_threshold: i64,
    pub maintenance_until: Option<chrono::DateTime<chrono::Utc>>,
    pub maintenance_note: Option<String>,
    pub status: String,
    pub consecutive_failures: i64,
    pub alert_state: String,
    pub next_check_at: chrono::DateTime<chrono::Utc>,
    pub last_checked_at: Option<chrono::DateTime<chrono::Utc>>,
    pub last_latency_ms: Option<i64>,
    pub last_http_status: Option<i64>,
    pub last_message: Option<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelHealthCheck {
    pub id: i64,
    #[schema(value_type = String)]
    pub model_id: Uuid,
    pub checked_at: chrono::DateTime<chrono::Utc>,
    pub trigger: String,
    pub status: String,
    pub latency_ms: Option<i64>,
    pub http_status: Option<i64>,
    pub message: Option<String>,
    pub response_excerpt: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelHealthDetail {
    pub summary: ModelHealthSummary,
    pub checks: Vec<ModelHealthCheck>,
}

/// Hot-path view of a model route for the data plane cache.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedModel {
    pub model_name: String,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub admission_weight: i64,
    pub max_in_flight: Option<usize>,
    pub enabled: bool,
    pub cache_enabled: bool,
    pub cache_ttl_secs: i64,
}

/// A registered MCP (Model Context Protocol) server. obleth fronts it with the
/// same identity, audit, and reliability layer it applies to LLM traffic:
/// clients reach many MCP servers through one authenticated obleth endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct McpServer {
    pub id: Uuid,
    /// Path segment clients use: `/mcp/{name}`.
    pub name: String,
    /// Upstream MCP base URL (streamable-HTTP / SSE endpoint).
    pub upstream_url: String,
    /// Full Authorization header value forwarded upstream (e.g. `Bearer …`).
    /// Stored encrypted-at-rest in production.
    pub auth_header: Option<String>,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Hot-path view of an MCP server for the data plane cache.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedMcpServer {
    pub name: String,
    pub upstream_url: String,
    pub auth_header: Option<String>,
    pub enabled: bool,
}

/// A response stored in the exact-match cache. Bodies are UTF-8 (JSON or SSE
/// text), so they round-trip losslessly as a `String`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedResponse {
    pub status: u16,
    pub content_type: String,
    pub body: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// A single completed request's telemetry record (the ClickHouse ledger row).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageRecord {
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub key_id: Uuid,
    pub model: String,
    pub admission: String,
    pub weight: i64,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub estimated_tokens: u32,
    pub queue_wait_ms: u32,
    pub ttft_ms: u32,
    pub total_ms: u32,
    pub status_code: u16,
    /// Response cache outcome: `hit`, `miss`, or `off` (caching not enabled).
    pub cache_status: String,
    /// Unix epoch milliseconds at request completion.
    pub ts_ms: i64,
}
