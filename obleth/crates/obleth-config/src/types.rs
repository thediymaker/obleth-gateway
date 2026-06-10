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
    /// Free-text operator note shown in dashboards.
    #[serde(default)]
    pub description: String,
    /// Optional grouping label (team, project, org unit, customer).
    #[serde(default)]
    pub organization: String,
    /// Contact for budget/expiry notifications.
    #[serde(default)]
    pub contact_email: String,
    /// Lifecycle: `active`, `suspended`, or `archived`. Only `active` admits traffic.
    #[serde(default = "default_tenant_status")]
    pub status: String,
    /// IANA timezone (e.g. `America/Phoenix`) the schedule below is evaluated in.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Optional activation start. Before this instant the tenant is not active.
    #[serde(default)]
    pub active_from: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional expiry cutoff. After this instant the tenant is not active.
    #[serde(default)]
    pub active_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional recurring weekly windows. Empty/`None` means any time of week.
    #[serde(default)]
    pub weekly_windows: Option<Vec<WeeklyWindow>>,
    /// Optional cumulative token budget for the current term. `None` = no cap.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Optional cumulative USD cost budget for the current term. `None` = no cap.
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    /// Term budget reset period: `lifetime`, `monthly`, or `term`. `None` = lifetime.
    #[serde(default)]
    pub budget_period: Option<String>,
    /// When the current term began. Changing it resets cumulative term usage.
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional per-tenant model allowlist. Empty/`None` = all models permitted.
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
pub fn default_tenant_status() -> String {
    "active".to_string()
}

/// Default tenant timezone used when deserializing legacy records.
pub fn default_timezone() -> String {
    "UTC".to_string()
}

/// A recurring weekly availability window. Times are minutes from local midnight
/// in the tenant's `timezone`; `day` is 0=Sunday .. 6=Saturday (matches
/// JavaScript `Date.getDay()`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct WeeklyWindow {
    pub day: u8,
    pub start_min: u16,
    pub end_min: u16,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
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
    /// Tenant lifecycle status. Anything other than `active` blocks the request.
    #[serde(default = "default_tenant_status")]
    pub status: String,
    /// IANA timezone the scheduling fields below are evaluated in.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Optional activation start; requests before this instant are blocked.
    #[serde(default)]
    pub active_from: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional expiry cutoff; requests after this instant are blocked.
    #[serde(default)]
    pub active_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional recurring weekly windows. Empty/`None` means any time of week.
    #[serde(default)]
    pub weekly_windows: Option<Vec<WeeklyWindow>>,
    /// Optional cumulative token budget for the current term. `None` = no cap.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Optional cumulative USD cost budget for the current term. `None` = no cap.
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    /// Term budget reset period: `lifetime`, `monthly`, or `term`. `None` = lifetime.
    #[serde(default)]
    pub budget_period: Option<String>,
    /// When the current term began. Changing it resets cumulative term usage.
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional per-tenant model allowlist. Empty/`None` = all models permitted.
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
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
    /// Rejected (budget exhausted or fail-closed).
    Rejected,
}

impl Admission {
    pub fn as_str(self) -> &'static str {
        match self {
            Admission::Fast => "fast",
            Admission::Queued => "queued",
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
    /// Modality from the fixed [`MODEL_TYPES`] vocabulary. Determines which
    /// OpenAI endpoint this model serves (`chat`, `embedding`,
    /// `audio_transcription`, `audio_speech`, `image`). Defaults to `chat`.
    #[serde(default = "default_model_type")]
    pub model_type: String,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    /// Per-generated-image cost in USD (`image` models).
    #[serde(default)]
    pub cost_per_image: f64,
    /// Per-second-of-audio cost in USD (`audio_transcription` models).
    #[serde(default)]
    pub cost_per_audio_second: f64,
    /// Per-input-character cost in USD (`audio_speech` models).
    #[serde(default)]
    pub cost_per_character: f64,
    pub context_window: i64,
    /// Multiplier applied to tenant weight at admission when this model is used.
    pub admission_weight: i64,
    /// Optional per-model in-flight cap. `None` means only the global scheduler
    /// cap and tenant/group fairshare limits apply.
    pub max_in_flight: Option<i64>,
    /// How `max_in_flight` is decided. `static` (default) keeps the
    /// operator-set value; `tuned` means it was found by the auto-tune ramp
    /// probe against the upstream. Stored as text for forward compatibility.
    #[serde(default = "default_capacity_mode")]
    pub capacity_mode: String,
    /// When the tuned `max_in_flight` was last written by auto-tune. `None`
    /// until the model has been tuned.
    #[serde(default)]
    pub capacity_tuned_at: Option<chrono::DateTime<chrono::Utc>>,
    pub supports_function_calling: bool,
    pub supports_system_messages: bool,
    pub supports_response_schema: bool,
    pub supports_tool_choice: bool,
    /// Native image-input capability. When false, the gateway's vision boon can
    /// relay images to a designated vision model and inject text descriptions
    /// before forwarding the request to this model.
    #[serde(default)]
    pub supports_vision: bool,
    pub enabled: bool,
    /// When true, identical requests to this model are served from the response
    /// cache (exact-match on model + request body) instead of the upstream.
    pub cache_enabled: bool,
    /// Time-to-live for cached responses, in seconds.
    pub cache_ttl_secs: i64,
    /// Routing tags from the fixed [`MODEL_TAGS`] vocabulary. The `auto` router
    /// prefers models whose tags match the request's classified intent.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Gateway boons enabled for this model from the fixed [`MODEL_BOONS`]
    /// vocabulary. Boons grant capabilities the model lacks natively (e.g. the
    /// `vision` boon relays images to a describer). Empty by default.
    #[serde(default)]
    pub boons: Vec<String>,
    /// Per-request upstream timeout in seconds. `None` falls back to the global
    /// `OBLETH_UPSTREAM_TIMEOUT_SECS` default.
    #[serde(default)]
    pub request_timeout_secs: Option<i64>,
    /// Extra attempts against the same endpoint on retryable failures (network
    /// errors, timeouts, 408/429/5xx). `0` disables retries.
    #[serde(default)]
    pub max_retries: i64,
    /// Base delay in milliseconds for exponential backoff between retries.
    #[serde(default = "default_retry_backoff_ms")]
    pub retry_backoff_ms: i64,
    /// How obleth chooses among this model's registered endpoints: `failover`
    /// (priority order) or `load_balance` (weighted).
    #[serde(default = "default_endpoint_selection_mode")]
    pub endpoint_selection_mode: String,
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedModel {
    pub model_name: String,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    /// Modality from the fixed [`MODEL_TYPES`] vocabulary. `#[serde(default)]`
    /// keeps older cached payloads (without this field) deserializable as
    /// `chat`.
    #[serde(default = "default_model_type")]
    pub model_type: String,
    pub admission_weight: i64,
    pub max_in_flight: Option<usize>,
    pub enabled: bool,
    pub cache_enabled: bool,
    pub cache_ttl_secs: i64,
    /// Cost per input/output token, used for cumulative USD budget accounting.
    #[serde(default)]
    pub input_cost_per_token: f64,
    #[serde(default)]
    pub output_cost_per_token: f64,
    /// Per-unit costs for non-chat modalities (USD).
    #[serde(default)]
    pub cost_per_image: f64,
    #[serde(default)]
    pub cost_per_audio_second: f64,
    #[serde(default)]
    pub cost_per_character: f64,
    /// Maximum context window in tokens. Used by the `auto` router to filter
    /// out models that cannot fit the request. `#[serde(default)]` keeps older
    /// cached payloads (without this field) deserializable.
    #[serde(default)]
    pub context_window: i64,
    #[serde(default)]
    pub supports_function_calling: bool,
    #[serde(default)]
    pub supports_system_messages: bool,
    #[serde(default)]
    pub supports_response_schema: bool,
    #[serde(default)]
    pub supports_tool_choice: bool,
    /// Native image-input capability. When false, the gateway's vision boon can
    /// relay images to a designated vision model. `#[serde(default)]` keeps
    /// older cached payloads (without this field) deserializable as `false`.
    #[serde(default)]
    pub supports_vision: bool,
    /// Routing tags from the fixed [`MODEL_TAGS`] vocabulary.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Gateway boons enabled for this model from the fixed [`MODEL_BOONS`]
    /// vocabulary. `#[serde(default)]` keeps older cached payloads readable.
    #[serde(default)]
    pub boons: Vec<String>,
    /// Per-request upstream timeout in seconds. `None` falls back to the global
    /// default. `#[serde(default)]` keeps older cached payloads readable.
    #[serde(default)]
    pub request_timeout_secs: Option<i64>,
    /// Extra attempts against the same endpoint on retryable failures.
    #[serde(default)]
    pub max_retries: i64,
    /// Base delay (ms) for exponential backoff between retries.
    #[serde(default = "default_retry_backoff_ms")]
    pub retry_backoff_ms: i64,
    /// How to choose among `endpoints`: `failover` or `load_balance`.
    #[serde(default = "default_endpoint_selection_mode")]
    pub endpoint_selection_mode: String,
    /// Upstream endpoints for this model. When empty, the data plane falls back
    /// to the legacy single `api_base`/`api_key` pair above (older cached
    /// payloads and un-migrated rows).
    #[serde(default)]
    pub endpoints: Vec<ResolvedEndpoint>,
}

/// Hot-path view of one upstream endpoint of a model. Several endpoints of the
/// same model are interchangeable (identical pricing and capabilities); only
/// the wire target (`api_base`/`api_key`) differs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedEndpoint {
    #[serde(default)]
    pub id: String,
    pub api_base: String,
    #[serde(default)]
    pub api_key: Option<String>,
    /// Lower wins in `failover` mode.
    #[serde(default)]
    pub priority: i64,
    /// Relative share in `load_balance` mode.
    #[serde(default)]
    pub weight: i64,
    /// Whether this endpoint is eligible for traffic at all.
    #[serde(default)]
    pub enabled: bool,
    /// Last observed health. Unhealthy endpoints are skipped during selection.
    #[serde(default)]
    pub healthy: bool,
}

/// Persisted upstream endpoint of a model (control-plane/API view). Several
/// endpoints of the same model are interchangeable; obleth fails over or
/// load-balances across them and tracks health per endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelEndpoint {
    pub id: Uuid,
    #[schema(value_type = String)]
    pub model_id: Uuid,
    /// Operator-facing label, unique within the model.
    pub name: String,
    pub api_base: String,
    /// Optional bearer/api key for this endpoint (stored encrypted-at-rest).
    pub api_key: Option<String>,
    /// Lower wins in `failover` mode.
    pub priority: i64,
    /// Relative share in `load_balance` mode.
    pub weight: i64,
    pub enabled: bool,
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
    /// USD cost of this request, frozen at completion using the model's
    /// per-token (and per-modality) rates in effect at that moment. Stored so
    /// spend reporting never has to recompute from tokens × current price, and
    /// so editing a model's price later can't rewrite historical spend.
    /// `#[serde(default)]` keeps WAL records written before this field existed
    /// replayable.
    #[serde(default)]
    pub cost_usd: f64,
    /// Unix epoch milliseconds at request completion.
    pub ts_ms: i64,
    /// Client-supplied session/conversation id used to group related requests
    /// in the live request log (e.g. a multi-turn chat). Empty when the caller
    /// did not provide one. `#[serde(default)]` keeps older WAL records
    /// replayable.
    #[serde(default)]
    pub session_id: String,
    /// Coarse request class derived from the request path (e.g. `chat`,
    /// `embedding`, `audio`, `image`, `completion`, `responses`, `rerank`,
    /// `other`). Surfaced as the "Type" column in the request log.
    /// `#[serde(default)]` keeps older WAL records replayable.
    #[serde(default)]
    pub request_type: String,
}

/// Runtime-configurable retention for the raw per-request `usage` ledger.
/// Rows older than `days` are pruned (whole day-partitions dropped) by a
/// background worker; the permanent `usage_daily` rollup is never affected.
/// Persisted in Postgres so changes survive restarts and override the env
/// default.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct UsageRetentionSettings {
    /// Days of raw per-request history to keep. Clamped to a sane floor on use.
    pub days: i64,
}

/// Runtime-configurable alerting settings, editable from the control plane and
/// persisted in Postgres so they survive restarts. The gateway loads these at
/// boot (falling back to environment defaults) and applies live updates without
/// a restart.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AlertSettings {
    /// Slack incoming-webhook URL. `None` disables Slack delivery.
    #[serde(default)]
    pub slack_webhook_url: Option<String>,
    /// SMTP email delivery. `None` disables email alerts.
    #[serde(default)]
    pub email: Option<EmailSettings>,
    /// Minimum seconds between repeat alerts for the same dedup key (cooldown).
    #[serde(default = "default_alert_min_interval_secs")]
    pub min_interval_secs: u64,
}

/// SMTP relay configuration for email alerts (universities typically run an
/// internal relay). Credentials are optional for unauthenticated relays.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmailSettings {
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// Optional SMTP auth username. `None` for an open/unauthenticated relay.
    #[serde(default)]
    pub username: Option<String>,
    /// Optional SMTP auth password. `None` for an open/unauthenticated relay.
    #[serde(default)]
    pub password: Option<String>,
    /// Envelope/From address (e.g. `obleth-alerts@university.edu`).
    pub from_address: String,
    /// Where alert emails are delivered.
    #[serde(default)]
    pub recipients: Vec<String>,
    /// Use STARTTLS (port 587) when true; plain SMTP (port 25) when false.
    #[serde(default = "default_true")]
    pub starttls: bool,
}

fn default_alert_min_interval_secs() -> u64 {
    300
}

fn default_smtp_port() -> u16 {
    587
}

fn default_true() -> bool {
    true
}

impl Default for AlertSettings {
    fn default() -> Self {
        AlertSettings {
            slack_webhook_url: None,
            email: None,
            min_interval_secs: default_alert_min_interval_secs(),
        }
    }
}

impl AlertSettings {
    /// True when at least one delivery channel is configured.
    pub fn any_channel_enabled(&self) -> bool {
        self.slack_enabled() || self.email_enabled()
    }

    pub fn slack_enabled(&self) -> bool {
        self.slack_webhook_url
            .as_ref()
            .is_some_and(|u| !u.trim().is_empty())
    }

    pub fn email_enabled(&self) -> bool {
        self.email
            .as_ref()
            .is_some_and(|e| !e.smtp_host.trim().is_empty() && !e.recipients.is_empty())
    }
}

/// Fixed vocabulary of model routing tags. Operators tag each model with a
/// subset of these; the `auto` router's classifier maps a request to the
/// best-matching tags and selection prefers models carrying them.
pub const MODEL_TAGS: &[&str] = &[
    "coding",
    "general",
    "reasoning",
    "math",
    "vision",
    "long-context",
    "fast",
    "creative",
];

/// True when `tag` is part of the fixed [`MODEL_TAGS`] vocabulary.
pub fn is_valid_tag(tag: &str) -> bool {
    MODEL_TAGS.contains(&tag)
}

/// Fixed vocabulary of gateway boons. A boon grants a capability a model lacks
/// natively; the data plane applies a model's enabled boons before dispatch.
/// `vision` relays image parts to a configured describer model. Operators opt
/// each model into a subset of these; nothing is granted by default.
pub const MODEL_BOONS: &[&str] = &["vision"];

/// True when `boon` is part of the fixed [`MODEL_BOONS`] vocabulary.
pub fn is_valid_boon(boon: &str) -> bool {
    MODEL_BOONS.contains(&boon)
}

/// Normalize an arbitrary list of boon strings to the canonical form used in
/// storage: trimmed, lowercased, restricted to the known vocabulary,
/// de-duplicated, and order-stable by first appearance.
pub fn normalize_boons<I, S>(boons: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for boon in boons {
        let b = boon.as_ref().trim().to_ascii_lowercase();
        if is_valid_boon(&b) && !out.contains(&b) {
            out.push(b);
        }
    }
    out
}

/// Fixed vocabulary of model modalities. Each value maps to a family of
/// OpenAI-compatible endpoints the model serves (see
/// `endpoint_matches_model_type` in the proxy). `chat` is the default.
pub const MODEL_TYPES: &[&str] = &[
    "chat",
    "embedding",
    "audio_transcription",
    "audio_speech",
    "image",
];

/// The default modality assigned to a model when none is specified.
pub const DEFAULT_MODEL_TYPE: &str = "chat";

fn default_model_type() -> String {
    DEFAULT_MODEL_TYPE.to_string()
}

/// Fixed vocabulary of capacity-tuning modes. `static` keeps the operator-set
/// `max_in_flight`; `tuned` lets auto-tune set it from a ramp probe.
pub const CAPACITY_MODES: &[&str] = &["static", "tuned"];

/// The default capacity mode assigned to a model when none is specified.
pub const DEFAULT_CAPACITY_MODE: &str = "static";

fn default_capacity_mode() -> String {
    DEFAULT_CAPACITY_MODE.to_string()
}

/// True when `mode` is part of the fixed [`CAPACITY_MODES`] vocabulary.
pub fn is_valid_capacity_mode(mode: &str) -> bool {
    CAPACITY_MODES.contains(&mode)
}

/// Normalize a capacity-mode string to canonical storage form: trimmed and
/// lowercased, falling back to [`DEFAULT_CAPACITY_MODE`] when empty or unknown.
pub fn normalize_capacity_mode(mode: &str) -> String {
    let m = mode.trim().to_ascii_lowercase();
    if is_valid_capacity_mode(&m) {
        m
    } else {
        DEFAULT_CAPACITY_MODE.to_string()
    }
}

/// Fixed vocabulary of endpoint-selection modes. `failover` tries a model's
/// endpoints in priority order; `load_balance` spreads requests across them by
/// weight. Skipping unhealthy/disabled endpoints applies in both modes.
pub const ENDPOINT_SELECTION_MODES: &[&str] = &["failover", "load_balance"];

/// The default endpoint-selection mode assigned to a model when none is set.
pub const DEFAULT_ENDPOINT_SELECTION_MODE: &str = "failover";

fn default_endpoint_selection_mode() -> String {
    DEFAULT_ENDPOINT_SELECTION_MODE.to_string()
}

/// Default base delay (ms) for retry exponential backoff.
pub const DEFAULT_RETRY_BACKOFF_MS: i64 = 200;

fn default_retry_backoff_ms() -> i64 {
    DEFAULT_RETRY_BACKOFF_MS
}

/// True when `mode` is part of the fixed [`ENDPOINT_SELECTION_MODES`] vocabulary.
pub fn is_valid_endpoint_selection_mode(mode: &str) -> bool {
    ENDPOINT_SELECTION_MODES.contains(&mode)
}

/// Normalize an endpoint-selection-mode string to canonical storage form:
/// trimmed and lowercased, falling back to [`DEFAULT_ENDPOINT_SELECTION_MODE`]
/// when empty or unknown.
pub fn normalize_endpoint_selection_mode(mode: &str) -> String {
    let m = mode.trim().to_ascii_lowercase();
    if is_valid_endpoint_selection_mode(&m) {
        m
    } else {
        DEFAULT_ENDPOINT_SELECTION_MODE.to_string()
    }
}

/// True when `model_type` is part of the fixed [`MODEL_TYPES`] vocabulary.
pub fn is_valid_model_type(model_type: &str) -> bool {
    MODEL_TYPES.contains(&model_type)
}

/// Normalize an arbitrary model-type string to the canonical form used in
/// storage: trimmed and lowercased, falling back to [`DEFAULT_MODEL_TYPE`]
/// when empty or outside the known vocabulary.
pub fn normalize_model_type(model_type: &str) -> String {
    let t = model_type.trim().to_ascii_lowercase();
    if is_valid_model_type(&t) {
        t
    } else {
        DEFAULT_MODEL_TYPE.to_string()
    }
}

/// Normalize an arbitrary list of tag strings to the canonical form used in
/// storage and matching: trimmed, lowercased, restricted to the known
/// vocabulary, de-duplicated, and order-stable by first appearance.
pub fn normalize_tags<I, S>(tags: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut out: Vec<String> = Vec::new();
    for tag in tags {
        let t = tag.as_ref().trim().to_ascii_lowercase();
        if is_valid_tag(&t) && !out.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// Runtime-editable configuration for the `auto` router's intent classifier.
/// Persisted in `app_settings` under the `auto_router` key so it is editable
/// from the control plane without a restart; seeded from environment on boot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutoRouterSettings {
    /// When true, an `auto` request is first sent to the classifier model to
    /// derive intent tags. When false (or unavailable) the router falls back to
    /// cheap heuristics, then capacity/cost scoring.
    #[serde(default)]
    pub classifier_enabled: bool,
    /// `model_name` of the registered model used as the classifier "brain"
    /// (e.g. a sub-1B model). `None` disables the model call regardless of
    /// `classifier_enabled`.
    #[serde(default)]
    pub classifier_model: Option<String>,
    /// Hard timeout for the classifier call. On timeout the router falls back
    /// to heuristics so an `auto` request is never blocked on the brain.
    #[serde(default = "default_classifier_timeout_ms")]
    pub classifier_timeout_ms: u64,
}

fn default_classifier_timeout_ms() -> u64 {
    250
}

impl Default for AutoRouterSettings {
    fn default() -> Self {
        AutoRouterSettings {
            classifier_enabled: false,
            classifier_model: None,
            classifier_timeout_ms: default_classifier_timeout_ms(),
        }
    }
}

impl AutoRouterSettings {
    /// True when the classifier is both enabled and points at a model.
    pub fn classifier_active(&self) -> bool {
        self.classifier_enabled
            && self
                .classifier_model
                .as_ref()
                .is_some_and(|m| !m.trim().is_empty())
    }
}

/// Default prompt sent to the vision model when describing an image for a
/// non-vision model. Operators can override it from the control plane.
pub const DEFAULT_VISION_DESCRIBE_PROMPT: &str = "You are assisting a text-only \
model that cannot see images. Describe this image in thorough, faithful detail: \
all visible text (verbatim), UI elements, code, diagrams, charts, layout, and \
any information needed to reason about it. Be concise but complete.";

fn default_vision_describe_prompt() -> String {
    DEFAULT_VISION_DESCRIBE_PROMPT.to_string()
}

fn default_vision_max_images() -> u32 {
    6
}

fn default_vision_timeout_ms() -> u64 {
    30_000
}

/// Runtime-editable configuration for the gateway's model "boons" — gateway-side
/// capabilities granted to models that lack them natively. Persisted in
/// `app_settings` under the `boons` key so it is editable from the control plane
/// without a restart.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BoonSettings {
    /// The vision boon: relay images to a designated vision model for models
    /// that do not natively accept image input.
    #[serde(default)]
    pub vision: VisionBoonSettings,
}

/// Configuration for the vision boon (image-to-text relay).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VisionBoonSettings {
    /// Master switch for the vision boon. When false, requests pass through
    /// unchanged (today's behavior).
    #[serde(default)]
    pub enabled: bool,
    /// `model_name` of the registered vision model used to describe images.
    /// `None` disables the relay regardless of `enabled`.
    #[serde(default)]
    pub fallback_model: Option<String>,
    /// Instruction sent to the vision model when asking it to describe an image.
    #[serde(default = "default_vision_describe_prompt")]
    pub describe_prompt: String,
    /// Maximum number of images relayed per request (cost/latency guard).
    /// Images beyond this cap are left untouched.
    #[serde(default = "default_vision_max_images")]
    pub max_images: u32,
    /// Hard timeout for each describe call, in milliseconds. On timeout the
    /// relay is abandoned and the original request passes through unchanged.
    #[serde(default = "default_vision_timeout_ms")]
    pub timeout_ms: u64,
}

impl Default for VisionBoonSettings {
    fn default() -> Self {
        VisionBoonSettings {
            enabled: false,
            fallback_model: None,
            describe_prompt: default_vision_describe_prompt(),
            max_images: default_vision_max_images(),
            timeout_ms: default_vision_timeout_ms(),
        }
    }
}

impl VisionBoonSettings {
    /// True when the vision boon is enabled and points at a fallback model.
    pub fn active(&self) -> bool {
        self.enabled
            && self
                .fallback_model
                .as_ref()
                .is_some_and(|m| !m.trim().is_empty())
    }
}

// ---- config backup / restore ------------------------------------------------

/// File-format discriminator for config backups.
pub const BACKUP_FORMAT: &str = "obleth-config-backup";
/// Current backup schema version. Bump when the file shape changes.
pub const BACKUP_VERSION: u32 = 1;

/// A portable snapshot of all gateway configuration: everything needed to
/// recreate an instance except usage history (audit log, health-check history,
/// ClickHouse ledger). Provider secrets are carried exactly as stored — i.e.
/// AES-GCM ciphertext when `OBLETH_ENCRYPTION_KEY` is set — so restoring an
/// encrypted backup requires the same key.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConfigBackup {
    /// Always [`BACKUP_FORMAT`].
    pub format: String,
    /// Backup schema version ([`BACKUP_VERSION`]).
    pub version: u32,
    pub exported_at: chrono::DateTime<chrono::Utc>,
    /// Gateway version that produced the backup (informational).
    pub gateway_version: String,
    pub encryption: BackupEncryption,
    pub data: BackupData,
}

/// Describes how secrets in the backup are protected, and carries a sentinel
/// that lets a restoring instance prove its encryption key matches before
/// touching the database.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BackupEncryption {
    /// Whether the exporting instance had `OBLETH_ENCRYPTION_KEY` configured.
    pub cipher_enabled: bool,
    /// A known plaintext encrypted with the exporter's key. `None` when the
    /// cipher was disabled. Restore decrypts this first; a failure means the
    /// keys differ and the restore is rejected before any write.
    pub key_check: Option<String>,
    /// Whether the exporting instance hashed client keys with an
    /// `OBLETH_API_KEY_PEPPER`. A pepper mismatch can't be detected from the
    /// opaque hashes; this flag lets restore at least warn about it.
    #[serde(default)]
    pub api_key_pepper_set: bool,
}

/// The configuration rows themselves, in foreign-key order (groups before
/// tenants, tenants before keys, models before endpoints).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BackupData {
    #[serde(default)]
    pub fairshare_groups: Vec<FairshareGroupBackup>,
    #[serde(default)]
    pub tenants: Vec<TenantBackup>,
    #[serde(default)]
    pub api_keys: Vec<ApiKeyBackup>,
    #[serde(default)]
    pub models: Vec<ModelBackup>,
    #[serde(default)]
    pub model_endpoints: Vec<ModelEndpointBackup>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerBackup>,
    #[serde(default)]
    pub app_settings: Vec<AppSettingBackup>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FairshareGroupBackup {
    pub name: String,
    pub weight: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// [`Tenant`] minus server-managed `updated_at`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TenantBackup {
    pub id: Uuid,
    pub name: String,
    pub fairshare_group: String,
    pub weight: i64,
    pub tokens_per_minute: i64,
    pub max_in_flight: Option<i64>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default)]
    pub contact_email: String,
    #[serde(default = "default_tenant_status")]
    pub status: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub active_from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub active_until: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub weekly_windows: Option<Vec<WeeklyWindow>>,
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    #[serde(default)]
    pub budget_period: Option<String>,
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// [`ApiKey`] plus the stored `key_hash`, so client keys issued before the
/// backup keep authenticating after a restore. The hash is a one-way SHA-256
/// digest — it cannot be reversed into a usable secret.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyBackup {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    pub key_prefix: String,
    pub key_hash: String,
    pub disabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// [`ModelRoute`] configuration columns plus health *configuration* (intervals,
/// thresholds, maintenance window). Runtime health state (status, failure
/// counters, last-check telemetry) is deliberately absent: a restored model is
/// re-probed from scratch. `api_key` carries the stored value verbatim —
/// ciphertext on encrypted instances.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelBackup {
    pub id: Uuid,
    pub model_name: String,
    #[serde(default)]
    pub description: String,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    #[serde(default = "default_model_type")]
    pub model_type: String,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    #[serde(default)]
    pub cost_per_image: f64,
    #[serde(default)]
    pub cost_per_audio_second: f64,
    #[serde(default)]
    pub cost_per_character: f64,
    pub context_window: i64,
    pub admission_weight: i64,
    pub max_in_flight: Option<i64>,
    #[serde(default = "default_capacity_mode")]
    pub capacity_mode: String,
    #[serde(default)]
    pub capacity_tuned_at: Option<chrono::DateTime<chrono::Utc>>,
    pub supports_function_calling: bool,
    pub supports_system_messages: bool,
    pub supports_response_schema: bool,
    pub supports_tool_choice: bool,
    #[serde(default)]
    pub supports_vision: bool,
    pub enabled: bool,
    pub cache_enabled: bool,
    pub cache_ttl_secs: i64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub boons: Vec<String>,
    #[serde(default)]
    pub request_timeout_secs: Option<i64>,
    #[serde(default)]
    pub max_retries: i64,
    #[serde(default = "default_retry_backoff_ms")]
    pub retry_backoff_ms: i64,
    #[serde(default = "default_endpoint_selection_mode")]
    pub endpoint_selection_mode: String,
    pub health_checks_enabled: bool,
    pub health_alerts_enabled: bool,
    pub health_check_interval_secs: i64,
    pub health_failure_threshold: i64,
    #[serde(default)]
    pub health_maintenance_until: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub health_maintenance_note: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// [`ModelEndpoint`] configuration columns, without runtime health state.
/// `api_key` carries the stored value verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelEndpointBackup {
    pub id: Uuid,
    #[schema(value_type = String)]
    pub model_id: Uuid,
    pub name: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub priority: i64,
    pub weight: i64,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// [`McpServer`] with `auth_header` carried verbatim as stored.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct McpServerBackup {
    pub id: Uuid,
    pub name: String,
    pub upstream_url: String,
    pub auth_header: Option<String>,
    pub enabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// One `app_settings` row, value carried verbatim. Exported generically so
/// settings keys added after this format shipped are still covered.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppSettingBackup {
    pub key: String,
    #[schema(value_type = Object)]
    pub value: serde_json::Value,
}

/// Per-entity insert/update tallies from a restore.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct RestoreCounts {
    pub inserted: u64,
    pub updated: u64,
}

/// Outcome of a successful restore: what was inserted vs updated, plus any
/// non-fatal warnings (e.g. a possible API-key pepper mismatch).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct RestoreReport {
    pub fairshare_groups: RestoreCounts,
    pub tenants: RestoreCounts,
    pub api_keys: RestoreCounts,
    pub models: RestoreCounts,
    pub model_endpoints: RestoreCounts,
    pub mcp_servers: RestoreCounts,
    pub app_settings: RestoreCounts,
    #[serde(default)]
    pub warnings: Vec<String>,
}
