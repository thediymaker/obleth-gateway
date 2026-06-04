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
    /// hash -> resolved key, with a short TTL as a backstop to pub/sub invalidation.
    pub key_cache: Cache<String, ResolvedKey>,
    pub model_cache: Cache<String, ResolvedModel>,
    pub mcp_cache: Cache<String, ResolvedMcpServer>,
    /// Enumerable list of candidate models for `auto` selection. Kept fresh by
    /// a background refresh task; the `model_cache` above is not enumerable.
    pub model_registry: ModelRegistry,
    /// Intent classifier for `auto` routing; its settings are refreshed by the
    /// same background task that refreshes `model_registry`.
    pub classifier: Classifier,
    pub metrics: Arc<Metrics>,
    pub fail_open: bool,
    pub alerts: obleth_admin::AlertDispatcher,
}
