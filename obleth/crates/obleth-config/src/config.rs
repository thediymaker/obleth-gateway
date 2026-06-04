//! Runtime configuration, loaded from environment variables with sane defaults.
//!
//! Kept dependency-free (plain `std::env`) so a misconfigured deploy fails loudly
//! and predictably rather than depending on a config-file discovery order.

use std::env;
use std::fmt;
use std::time::Duration;

use crate::FairshareAlgorithm;

/// Top-level gateway configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Data-plane listener (client traffic -> upstream).
    pub proxy_listen: String,
    /// Management API listener (config writes, usage reads). Isolated from the hot path.
    pub admin_listen: String,
    /// Prometheus metrics listener.
    pub metrics_listen: String,

    /// Upstream base URL (Aibrix gateway, or the benchmark fixture backend in dev).
    pub upstream_base_url: String,
    /// Upstream request timeout.
    pub upstream_timeout: Duration,

    pub redis_url: String,
    pub database_url: String,
    pub clickhouse_url: String,
    pub clickhouse_db: String,
    pub clickhouse_user: String,
    pub clickhouse_password: String,

    /// Bootstrap admin token for the Management API.
    pub admin_token: String,

    /// Static global in-flight concurrency budget (the v1 CapacityProvider).
    pub global_max_in_flight: usize,
    /// Fairshare scheduling algorithm (`weighted` or `hierarchical`).
    pub fairshare_algorithm: FairshareAlgorithm,
    /// Queue-wait threshold (ms) past which brownout degradation kicks in.
    pub brownout_wait_ms: u64,

    /// Fail-open: keep serving from cache + buffer telemetry to WAL when
    /// Redis/ClickHouse are unavailable. Fail-closed rejects instead.
    pub fail_open: bool,
    /// Path to the local write-ahead log used as telemetry fallback.
    pub wal_path: String,

    /// Enable the scheduled model-health worker. Manual health checks remain
    /// available through the Management API even when this is false.
    pub model_health_enabled: bool,
    pub model_health_interval_secs: u64,
    pub model_health_timeout_secs: u64,
    pub model_health_retention_days: i64,
    pub internal_proxy_base_url: String,

    /// OTLP/HTTP trace collector base URL (e.g. `http://jaeger:4318`). `None`
    /// disables distributed tracing entirely.
    pub otel_endpoint: Option<String>,

    /// Optional Slack incoming-webhook alert delivery.
    pub slack_alerts: SlackAlertConfig,

    /// Boot-time defaults for the `auto` router's intent classifier. Persisted
    /// settings in Postgres override these once an operator saves them.
    pub auto_classifier_enabled: bool,
    pub auto_classifier_model: Option<String>,
    pub auto_classifier_timeout_ms: u64,
}

/// Slack alerting configuration. The webhook URL is intentionally redacted from
/// Debug output because Slack treats it as a secret.
#[derive(Clone)]
pub struct SlackAlertConfig {
    pub webhook_url: Option<String>,
    pub min_interval: Duration,
}

impl fmt::Debug for SlackAlertConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SlackAlertConfig")
            .field(
                "webhook_url",
                &self.webhook_url.as_ref().map(|_| "<redacted>"),
            )
            .field("min_interval", &self.min_interval)
            .finish()
    }
}

impl Config {
    pub fn from_env() -> Self {
        Config {
            proxy_listen: env_or("OBLETH_PROXY_LISTEN", "0.0.0.0:8080"),
            admin_listen: env_or("OBLETH_ADMIN_LISTEN", "0.0.0.0:9090"),
            metrics_listen: env_or("OBLETH_METRICS_LISTEN", "0.0.0.0:9091"),
            upstream_base_url: env_or("OBLETH_UPSTREAM_BASE_URL", "http://127.0.0.1:8081"),
            upstream_timeout: Duration::from_secs(parse_or("OBLETH_UPSTREAM_TIMEOUT_SECS", 300)),
            redis_url: env_or("OBLETH_REDIS_URL", "redis://127.0.0.1:6379"),
            database_url: env_or(
                "OBLETH_DATABASE_URL",
                "postgres://obleth:obleth@127.0.0.1:5432/obleth",
            ),
            clickhouse_url: env_or("OBLETH_CLICKHOUSE_URL", "http://127.0.0.1:8123"),
            clickhouse_db: env_or("OBLETH_CLICKHOUSE_DB", "obleth"),
            clickhouse_user: env_or("OBLETH_CLICKHOUSE_USER", "default"),
            clickhouse_password: env_or("OBLETH_CLICKHOUSE_PASSWORD", ""),
            admin_token: require_secret("OBLETH_ADMIN_TOKEN"),
            global_max_in_flight: parse_or("OBLETH_GLOBAL_MAX_IN_FLIGHT", 256),
            fairshare_algorithm: FairshareAlgorithm::parse(&env_or(
                "OBLETH_FAIRSHARE_ALGORITHM",
                "hierarchical",
            )),
            brownout_wait_ms: parse_or("OBLETH_BROWNOUT_WAIT_MS", 750),
            fail_open: parse_or("OBLETH_FAIL_OPEN", true),
            wal_path: env_or("OBLETH_WAL_PATH", "./obleth-telemetry.wal"),
            model_health_enabled: parse_or("OBLETH_MODEL_HEALTH_ENABLED", true),
            model_health_interval_secs: parse_or("OBLETH_MODEL_HEALTH_INTERVAL_SECS", 900),
            model_health_timeout_secs: parse_or("OBLETH_MODEL_HEALTH_TIMEOUT_SECS", 30),
            model_health_retention_days: parse_or("OBLETH_MODEL_HEALTH_RETENTION_DAYS", 30),
            internal_proxy_base_url: env_or(
                "OBLETH_INTERNAL_PROXY_BASE_URL",
                "http://127.0.0.1:8080",
            ),
            otel_endpoint: env::var("OBLETH_OTEL_ENDPOINT")
                .ok()
                .filter(|s| !s.is_empty()),
            slack_alerts: SlackAlertConfig {
                webhook_url: env::var("OBLETH_SLACK_WEBHOOK_URL")
                    .ok()
                    .filter(|s| !s.trim().is_empty()),
                min_interval: Duration::from_secs(parse_or(
                    "OBLETH_SLACK_ALERT_MIN_INTERVAL_SECS",
                    300,
                )),
            },
            auto_classifier_enabled: parse_or("OBLETH_AUTO_CLASSIFIER_ENABLED", false),
            auto_classifier_model: env::var("OBLETH_AUTO_CLASSIFIER_MODEL")
                .ok()
                .filter(|s| !s.trim().is_empty()),
            auto_classifier_timeout_ms: parse_or("OBLETH_AUTO_CLASSIFIER_TIMEOUT_MS", 250),
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self::from_env()
    }
}

fn env_or(key: &str, default: &str) -> String {
    env::var(key).unwrap_or_else(|_| default.to_string())
}

/// Read a required secret from the environment, aborting startup if it is
/// missing or blank. Used for credentials that must never fall back to a
/// hardcoded development default.
fn require_secret(key: &str) -> String {
    match env::var(key) {
        Ok(v) if !v.trim().is_empty() => v,
        _ => panic!(
            "{key} must be set to a non-empty value. Refusing to start with a default/blank secret."
        ),
    }
}

fn parse_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}
