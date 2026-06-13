//! Shared data-plane state.

use std::sync::Arc;

use moka::future::Cache;
use obleth_config::ResolvedKey;
use obleth_config::ResolvedMcpServer;
use obleth_config::ResolvedModel;
use obleth_fairshare::FairShare;
use obleth_redis::RedisStore;
use obleth_telemetry::TelemetrySink;
use obleth_tokenizer::HeuristicTokenizer;

use crate::boons::BoonEngine;
use crate::classifier::Classifier;
use crate::metrics::Metrics;
use crate::router::ModelRegistry;

/// All handles needed on the request hot path. Cheap to clone.
#[derive(Clone)]
pub struct AppState {
    pub redis: RedisStore,
    pub fairshare: FairShare,
    pub tokenizer: Arc<HeuristicTokenizer>,
    pub telemetry: TelemetrySink,
    pub http: reqwest::Client,
    pub upstream_base: String,
    /// Default per-request upstream timeout, used when a model does not set its
    /// own `request_timeout_secs`. From `OBLETH_UPSTREAM_TIMEOUT_SECS`.
    pub upstream_timeout: std::time::Duration,
    /// hash -> resolved key, with a short TTL as a backstop to pub/sub invalidation.
    /// Values are `Arc`-wrapped so a per-request lookup clones a pointer, not the
    /// whole struct (these are read on every data-plane and MCP request).
    pub key_cache: Cache<String, Arc<ResolvedKey>>,
    pub model_cache: Cache<String, Arc<ResolvedModel>>,
    pub mcp_cache: Cache<String, Arc<ResolvedMcpServer>>,
    /// Enumerable list of candidate models for `auto` selection. Kept fresh by
    /// a background refresh task; the `model_cache` above is not enumerable.
    pub model_registry: ModelRegistry,
    /// Intent classifier for `auto` routing; its settings are refreshed by the
    /// same background task that refreshes `model_registry`.
    pub classifier: Classifier,
    /// Engine for model "boons" (e.g. the vision boon). Its settings are
    /// refreshed by the same background task that refreshes `model_registry`.
    pub boons: BoonEngine,
    /// Short-TTL cache of discovered MCP tool lists, keyed by server name, so
    /// the gateway tool loop pays one `tools/list` per server per TTL instead
    /// of one per request.
    pub tool_cache: Cache<String, Arc<Vec<crate::boons::mcp_tools::McpTool>>>,
    pub metrics: Arc<Metrics>,
    pub fail_open: bool,
    pub alerts: obleth_admin::AlertDispatcher,
}
