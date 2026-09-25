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

    /// Upstream base URL (an inference server or gateway, or the benchmark fixture backend in dev).
    pub upstream_base_url: String,
    /// Upstream request timeout.
    pub upstream_timeout: Duration,
    /// How long an idle upstream keep-alive connection may sit in the pool before
    /// reqwest drops it. Must be shorter than the upstream's own idle-close
    /// timeout, or the pool will hand out sockets the server already closed
    /// (surfacing as random "error sending request" / connection-closed 502s).
    /// `0` disables idle connection reuse entirely.
    pub upstream_pool_idle_secs: u64,
    /// TCP keep-alive probe interval for upstream connections. Keeps NAT/LB state
    /// warm and surfaces dead peers faster. `0` disables.
    pub upstream_tcp_keepalive_secs: u64,
    /// Longest silence on an upstream connection (header wait or gap between
    /// body chunks). reqwest applies it to the header wait too, so it defaults
    /// to the upstream request timeout to never undercut it. `0` disables.
    pub upstream_read_timeout_secs: u64,
    /// How long shutdown waits for in-flight requests before exiting.
    pub shutdown_grace: Duration,
    /// Interval of the Postgres-to-Redis re-push of keys/models/MCP servers.
    /// `0` disables the timer (the re-push after a Redis reconnect still runs).
    pub redis_rewarm_secs: u64,

    pub redis_url: String,
    /// Per-command and per-connect timeouts for the shared Redis connection.
    pub redis_timeouts: RedisTimeouts,
    pub database_url: String,
    pub clickhouse_url: String,
    pub clickhouse_db: String,
    pub clickhouse_user: String,
    pub clickhouse_password: String,

    /// Bootstrap admin token for the Management API.
    pub admin_token: String,

    /// Total in-flight ceiling across all model pools (memory and
    /// upstream-connection guard). Not a fairness input; set it above the sum
    /// of the pool sizes you expect. Fleet-wide, like the pools, with
    /// `fairshare_shared_slots` or `fairshare_replica_aware` on.
    pub global_max_in_flight: usize,
    /// Pool size for a model that has no explicit `max_in_flight`.
    pub default_model_max_in_flight: usize,
    /// Seconds of scheduler history the gateway keeps in memory for the
    /// dashboard's activity chart, sampled every 2 seconds. `0` disables it.
    pub fairshare_history_secs: u64,
    /// Fairshare scheduling algorithm (`weighted` or `hierarchical`).
    pub fairshare_algorithm: FairshareAlgorithm,
    /// Enforce the fairshare limits (pool sizes, the global ceiling, tenant
    /// and key caps) as cluster-wide slots in Redis whenever more than one
    /// gateway replica is live, so any replica can use a model's whole pool
    /// and the fleet never exceeds it. With one replica there is no Redis
    /// call on the request path.
    pub fairshare_shared_slots: bool,
    /// Divide the fairshare limits across the live gateway replicas, counted
    /// from Redis heartbeats, whenever shared slots are off or unavailable:
    /// each replica then enforces `ceil(configured / replicas)`. Off, each
    /// replica enforces the full limits in that case. With one replica it
    /// changes nothing.
    pub fairshare_replica_aware: bool,
    /// How often each replica refreshes its Redis heartbeat, and re-asserts
    /// its shared-slot holdings.
    pub fairshare_replica_heartbeat: Duration,
    /// How long a heartbeat counts a replica as live. A crashed replica stops
    /// being counted, and its shared slots (or its share) return to the
    /// survivors, within this.
    pub fairshare_replica_ttl: Duration,
    /// Settings for models in the `discovered` capacity mode.
    pub capacity_discovery: CapacityDiscoveryConfig,

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

    /// Trusted JWT issuers for data-plane bearer auth. Empty = JWT path off.
    pub jwt_issuers: Vec<crate::jwt::JwtIssuerConfig>,

    /// Default days of raw per-request `usage` history to keep before pruning.
    /// A runtime setting saved from the control plane overrides this.
    pub usage_retention_days: i64,

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

/// Capacity discovery: how the gateway derives the pool size of models in the
/// `discovered` capacity mode from their live backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapacityDiscoveryConfig {
    /// Run the discovery loop (`OBLETH_CAPACITY_DISCOVERY_ENABLED`, default
    /// off). Off, a `discovered` model keeps its static `max_in_flight`.
    pub enabled: bool,
    /// How often each replica re-reads the sources
    /// (`OBLETH_CAPACITY_DISCOVERY_INTERVAL_SECS`, default 15, at least 1).
    pub interval: Duration,
    /// Namespaces the `kubernetes` source may read Service endpoints in
    /// (`OBLETH_CAPACITY_DISCOVERY_NAMESPACES`, comma-separated). A model
    /// that names no namespace is looked up in them in this order, and the
    /// first one that has its Service wins. Empty leaves the `kubernetes`
    /// source unavailable and no Kubernetes client is built.
    pub namespaces: Vec<String>,
    /// Service name template for `kubernetes`-source models that set no
    /// `capacity_service` (`OBLETH_CAPACITY_DEFAULT_SERVICE`), with
    /// `{upstream_model}` and `{model_name}` placeholders. Empty: such a model
    /// is refused.
    pub default_service: String,
}

impl CapacityDiscoveryConfig {
    pub const DEFAULT_INTERVAL_SECS: u64 = 15;

    pub fn from_env() -> Self {
        Self::from_values(
            env::var("OBLETH_CAPACITY_DISCOVERY_ENABLED")
                .ok()
                .as_deref(),
            env::var("OBLETH_CAPACITY_DISCOVERY_INTERVAL_SECS")
                .ok()
                .as_deref(),
            env::var("OBLETH_CAPACITY_DISCOVERY_NAMESPACES")
                .ok()
                .as_deref(),
            env::var("OBLETH_CAPACITY_DEFAULT_SERVICE").ok().as_deref(),
        )
    }

    /// Build from raw env values. An unparseable or zero interval falls back
    /// to the default.
    pub fn from_values(
        enabled: Option<&str>,
        interval_secs: Option<&str>,
        namespaces: Option<&str>,
        default_service: Option<&str>,
    ) -> Self {
        let secs = interval_secs
            .and_then(|v| v.trim().parse::<u64>().ok())
            .filter(|n| *n > 0)
            .unwrap_or(Self::DEFAULT_INTERVAL_SECS);
        CapacityDiscoveryConfig {
            enabled: lenient_bool(enabled, false),
            interval: Duration::from_secs(secs),
            namespaces: crate::capacity::parse_namespace_list(namespaces.unwrap_or_default()),
            default_service: default_service.unwrap_or_default().trim().to_string(),
        }
    }
}

impl Default for CapacityDiscoveryConfig {
    fn default() -> Self {
        Self::from_values(None, None, None, None)
    }
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

/// Timeouts applied to the shared Redis connection manager.
///
/// Without them a blackholed Redis (packets dropped, no RST) hangs every
/// request on the hot path instead of surfacing an error that the fail-open /
/// fail-closed budget logic can act on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RedisTimeouts {
    /// Maximum wait for a reply to a single command
    /// (`OBLETH_REDIS_RESPONSE_TIMEOUT_MS`, default 250).
    pub response: Duration,
    /// Maximum wait for one TCP connect + handshake attempt
    /// (`OBLETH_REDIS_CONNECT_TIMEOUT_MS`, default 2000).
    pub connect: Duration,
}

impl RedisTimeouts {
    pub const DEFAULT_RESPONSE_MS: u64 = 250;
    pub const DEFAULT_CONNECT_MS: u64 = 2000;

    pub fn from_env() -> Self {
        Self::from_values(
            env::var("OBLETH_REDIS_RESPONSE_TIMEOUT_MS").ok().as_deref(),
            env::var("OBLETH_REDIS_CONNECT_TIMEOUT_MS").ok().as_deref(),
        )
    }

    /// Build from raw env values; missing, unparseable, or zero values fall
    /// back to the defaults (a zero timeout would fail every command).
    pub fn from_values(response_ms: Option<&str>, connect_ms: Option<&str>) -> Self {
        let ms = |v: Option<&str>, default: u64| {
            let n = v
                .and_then(|s| s.trim().parse::<u64>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(default);
            Duration::from_millis(n)
        };
        RedisTimeouts {
            response: ms(response_ms, Self::DEFAULT_RESPONSE_MS),
            connect: ms(connect_ms, Self::DEFAULT_CONNECT_MS),
        }
    }
}

impl Default for RedisTimeouts {
    fn default() -> Self {
        Self::from_values(None, None)
    }
}

impl Config {
    pub fn from_env() -> Self {
        let (fairshare_replica_heartbeat, fairshare_replica_ttl) = replica_heartbeat_timing(
            parse_or("OBLETH_FAIRSHARE_REPLICA_HEARTBEAT_SECS", 5),
            parse_or("OBLETH_FAIRSHARE_REPLICA_TTL_SECS", 15),
        );
        Config {
            proxy_listen: env_or("OBLETH_PROXY_LISTEN", "0.0.0.0:8080"),
            admin_listen: env_or("OBLETH_ADMIN_LISTEN", "0.0.0.0:9180"),
            metrics_listen: env_or("OBLETH_METRICS_LISTEN", "0.0.0.0:9091"),
            upstream_base_url: env_or("OBLETH_UPSTREAM_BASE_URL", "http://127.0.0.1:8081"),
            upstream_timeout: Duration::from_secs(parse_or("OBLETH_UPSTREAM_TIMEOUT_SECS", 300)),
            upstream_pool_idle_secs: parse_or("OBLETH_UPSTREAM_POOL_IDLE_SECS", 15),
            upstream_tcp_keepalive_secs: parse_or("OBLETH_UPSTREAM_TCP_KEEPALIVE_SECS", 30),
            upstream_read_timeout_secs: upstream_read_timeout_secs(
                env::var("OBLETH_UPSTREAM_READ_TIMEOUT_SECS")
                    .ok()
                    .as_deref(),
                parse_or("OBLETH_UPSTREAM_TIMEOUT_SECS", 300),
            ),
            shutdown_grace: Duration::from_secs(parse_or("OBLETH_SHUTDOWN_GRACE_SECS", 300)),
            redis_rewarm_secs: parse_or("OBLETH_REDIS_REWARM_SECS", 300),
            redis_url: env_or("OBLETH_REDIS_URL", "redis://127.0.0.1:6379"),
            redis_timeouts: RedisTimeouts::from_env(),
            database_url: env_or(
                "OBLETH_DATABASE_URL",
                "postgres://obleth:obleth@127.0.0.1:5432/obleth",
            ),
            clickhouse_url: env_or("OBLETH_CLICKHOUSE_URL", "http://127.0.0.1:8123"),
            clickhouse_db: env_or("OBLETH_CLICKHOUSE_DB", "obleth"),
            clickhouse_user: env_or("OBLETH_CLICKHOUSE_USER", "default"),
            clickhouse_password: env_or("OBLETH_CLICKHOUSE_PASSWORD", ""),
            admin_token: require_secret("OBLETH_ADMIN_TOKEN"),
            global_max_in_flight: parse_or(
                "OBLETH_GLOBAL_MAX_IN_FLIGHT",
                DEFAULT_GLOBAL_MAX_IN_FLIGHT,
            ),
            default_model_max_in_flight: parse_or("OBLETH_DEFAULT_MODEL_MAX_IN_FLIGHT", 32),
            fairshare_history_secs: parse_or("OBLETH_FAIRSHARE_HISTORY_SECS", 3600),
            fairshare_algorithm: FairshareAlgorithm::parse(&env_or(
                "OBLETH_FAIRSHARE_ALGORITHM",
                "hierarchical",
            )),
            fairshare_shared_slots: bool_or("OBLETH_FAIRSHARE_SHARED_SLOTS", true),
            fairshare_replica_aware: bool_or("OBLETH_FAIRSHARE_REPLICA_AWARE", true),
            fairshare_replica_heartbeat,
            fairshare_replica_ttl,
            capacity_discovery: CapacityDiscoveryConfig::from_env(),
            fail_open: require_bool("OBLETH_FAIL_OPEN", true),
            wal_path: env_or("OBLETH_WAL_PATH", "./obleth-telemetry.wal"),
            model_health_enabled: bool_or("OBLETH_MODEL_HEALTH_ENABLED", true),
            model_health_interval_secs: parse_or("OBLETH_MODEL_HEALTH_INTERVAL_SECS", 900),
            model_health_timeout_secs: parse_or("OBLETH_MODEL_HEALTH_TIMEOUT_SECS", 30),
            model_health_retention_days: parse_or("OBLETH_MODEL_HEALTH_RETENTION_DAYS", 30),
            jwt_issuers: crate::jwt::jwt_issuers_from_env(),
            usage_retention_days: parse_or("OBLETH_USAGE_RETENTION_DAYS", 180),
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
            auto_classifier_enabled: bool_or("OBLETH_AUTO_CLASSIFIER_ENABLED", false),
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

/// Default `OBLETH_GLOBAL_MAX_IN_FLIGHT`. The per-model pools are what limit
/// admission; this is a safety cap above them (cluster-wide like the pools),
/// so it sits well over the sum of a realistic fleet's pools (128 models at
/// the default pool size of 32) rather than binding first.
pub const DEFAULT_GLOBAL_MAX_IN_FLIGHT: usize = 4096;

/// `(heartbeat interval, heartbeat TTL)` from their raw seconds. A zero
/// interval falls back to 5 s, and the TTL is raised to at least twice the
/// interval: a TTL at or under the interval would let one late heartbeat drop
/// a live replica from the count and resize the whole fleet for nothing.
fn replica_heartbeat_timing(interval_secs: u64, ttl_secs: u64) -> (Duration, Duration) {
    let interval = if interval_secs == 0 { 5 } else { interval_secs };
    let ttl = ttl_secs.max(interval.saturating_mul(2));
    (Duration::from_secs(interval), Duration::from_secs(ttl))
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

/// An explicit value wins; otherwise the upstream request timeout, floored at
/// 120s. reqwest's read timeout also bounds the wait for response headers,
/// so a default below the request timeout would cut slow non-streaming calls.
fn upstream_read_timeout_secs(raw: Option<&str>, upstream_timeout_secs: u64) -> u64 {
    raw.and_then(|v| v.trim().parse().ok())
        .unwrap_or(upstream_timeout_secs.max(120))
}

fn parse_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Parse a boolean env value: `1/0`, `true/false`, `yes/no`, `on/off`,
/// case-insensitive, surrounding whitespace ignored. `str::parse::<bool>`
/// only accepts `true`/`false`, so `0` used to fall through to the default.
pub fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Unset/blank → `default`; unparseable → `default`.
fn lenient_bool(value: Option<&str>, default: bool) -> bool {
    value.and_then(parse_bool).unwrap_or(default)
}

/// Unset/blank → `default`; unparseable → error naming the variable.
fn strict_bool(key: &str, value: Option<&str>, default: bool) -> Result<bool, String> {
    match value {
        None => Ok(default),
        Some(v) if v.trim().is_empty() => Ok(default),
        Some(v) => parse_bool(v).ok_or_else(|| {
            format!("{key}={v:?} is not a boolean; use one of 1/0, true/false, yes/no, on/off")
        }),
    }
}

fn bool_or(key: &str, default: bool) -> bool {
    lenient_bool(env::var(key).ok().as_deref(), default)
}

/// Boolean flag whose misreading is unsafe (e.g. `OBLETH_FAIL_OPEN`: a typo
/// must not silently turn budget enforcement fail-open). Aborts startup like
/// [`require_secret`].
fn require_bool(key: &str, default: bool) -> bool {
    match strict_bool(key, env::var(key).ok().as_deref(), default) {
        Ok(b) => b,
        Err(msg) => panic!("{msg}. Refusing to start with an ambiguous setting."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bool_flags_accept_common_spellings_case_insensitively() {
        for v in ["1", "true", "TRUE", "yes", "Yes", "on", "ON", " true "] {
            assert_eq!(parse_bool(v), Some(true), "{v:?}");
        }
        for v in ["0", "false", "False", "no", "NO", "off", "Off"] {
            assert_eq!(parse_bool(v), Some(false), "{v:?}");
        }
        for v in ["", "2", "enabled", "nope", "tru"] {
            assert_eq!(parse_bool(v), None, "{v:?}");
        }
    }

    #[test]
    fn fail_open_zero_means_fail_closed() {
        // Regression: `"0".parse::<bool>()` fails, which used to silently fall
        // back to the fail-open default.
        assert_eq!(strict_bool("OBLETH_FAIL_OPEN", Some("0"), true), Ok(false));
        assert_eq!(
            strict_bool("OBLETH_FAIL_OPEN", Some("off"), true),
            Ok(false)
        );
        assert_eq!(strict_bool("OBLETH_FAIL_OPEN", None, true), Ok(true));
        assert_eq!(strict_bool("OBLETH_FAIL_OPEN", Some("  "), true), Ok(true));
    }

    #[test]
    fn unparseable_strict_bool_is_an_error_not_a_default() {
        let err = strict_bool("OBLETH_FAIL_OPEN", Some("maybe"), true).unwrap_err();
        assert!(err.contains("OBLETH_FAIL_OPEN"), "{err}");
        assert!(err.contains("maybe"), "{err}");
    }

    #[test]
    fn lenient_bool_falls_back_on_garbage() {
        assert!(!lenient_bool(Some("0"), true));
        assert!(lenient_bool(Some("yes"), false));
        assert!(lenient_bool(Some("garbage"), true));
        assert!(!lenient_bool(None, false));
    }

    #[test]
    fn redis_timeouts_default_and_parse() {
        let d = RedisTimeouts::default();
        assert_eq!(d.response, Duration::from_millis(250));
        assert_eq!(d.connect, Duration::from_millis(2000));
        let t = RedisTimeouts::from_values(Some("100"), Some("500"));
        assert_eq!(t.response, Duration::from_millis(100));
        assert_eq!(t.connect, Duration::from_millis(500));
        let t = RedisTimeouts::from_values(Some("nope"), None);
        assert_eq!(t, RedisTimeouts::default());
    }

    #[test]
    fn capacity_discovery_is_off_by_default_and_parses_its_settings() {
        let d = CapacityDiscoveryConfig::default();
        assert!(!d.enabled);
        assert_eq!(d.interval, Duration::from_secs(15));
        assert!(d.namespaces.is_empty());
        assert!(d.default_service.is_empty());

        let c = CapacityDiscoveryConfig::from_values(
            Some("true"),
            Some("30"),
            Some(" llm, image ,llm"),
            Some(" {upstream_model} "),
        );
        assert!(c.enabled);
        assert_eq!(c.interval, Duration::from_secs(30));
        assert_eq!(c.namespaces, vec!["llm", "image"]);
        assert_eq!(c.default_service, "{upstream_model}");

        let c = CapacityDiscoveryConfig::from_values(Some("off"), Some("0"), None, None);
        assert!(!c.enabled);
        assert_eq!(c.interval, Duration::from_secs(15), "zero falls back");
    }

    #[test]
    fn replica_heartbeat_ttl_stays_well_above_the_interval() {
        let secs = |(i, t): (Duration, Duration)| (i.as_secs(), t.as_secs());
        assert_eq!(secs(replica_heartbeat_timing(5, 15)), (5, 15));
        assert_eq!(secs(replica_heartbeat_timing(5, 5)), (5, 10));
        assert_eq!(secs(replica_heartbeat_timing(10, 3)), (10, 20));
        assert_eq!(secs(replica_heartbeat_timing(0, 0)), (5, 10));
        assert_eq!(secs(replica_heartbeat_timing(2, 60)), (2, 60));
    }

    #[test]
    fn upstream_read_timeout_never_undercuts_the_request_timeout() {
        assert_eq!(upstream_read_timeout_secs(None, 300), 300);
        assert_eq!(upstream_read_timeout_secs(None, 30), 120);
        assert_eq!(upstream_read_timeout_secs(Some("45"), 300), 45);
        assert_eq!(upstream_read_timeout_secs(Some("0"), 300), 0);
        assert_eq!(upstream_read_timeout_secs(Some("x"), 600), 600);
    }
}
