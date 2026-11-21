//! Runtime configuration, loaded from environment variables with sane defaults.
//!
//! Kept dependency-free (plain `std::env`) so a misconfigured deploy fails loudly
//! and predictably rather than depending on a config-file discovery order.
//! Every variable the gateway reads goes through [`Config::from_lookup`]'s
//! one reader: a value that is set but unreadable falls back to its default
//! with a startup warning, and a safety setting that is unreadable stops the
//! start. A new on/off variable is named `OBLETH_<FEATURE>_ENABLED`.

use std::env;
use std::fmt;
use std::time::Duration;

use crate::{FairshareAlgorithm, JobScope};

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
    /// How long a new TCP connection to an upstream may take. Without it a
    /// host that drops packets holds the request, and its fairshare permit,
    /// for the kernel's connect give-up (about two minutes) before the next
    /// endpoint is tried. `0` disables.
    pub upstream_connect_timeout_secs: u64,
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
    /// Size of, and wait for, the Postgres connection pool.
    pub pg_pool: PgPoolSettings,
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
    /// Session affinity in the fairshare queue; `None` while it is off for
    /// models that do not turn it on themselves.
    pub fairshare_session_affinity: Option<SessionAffinityConfig>,
    /// The gateway's session affinity values, whether or not it is on: what
    /// a model that turns affinity on uses for anything it does not set.
    pub fairshare_session_affinity_settings: SessionAffinityConfig,

    /// `OBLETH_SCHEMA_VERIFY_ONLY`: check that Postgres has every schema
    /// file applied and refuse to start if not, without changing anything.
    /// For a gateway whose database role cannot alter the schema, or one
    /// started while migrations are applied out of band.
    pub schema_verify_only: bool,
    /// Fail-open: keep serving from cache + buffer telemetry to WAL when
    /// Redis/ClickHouse are unavailable. Fail-closed rejects instead.
    pub fail_open: bool,
    /// Path to the local write-ahead log used as telemetry fallback.
    pub wal_path: String,
    /// How long each replica buffers usage rows and trace spans before it
    /// inserts them into ClickHouse. Every insert becomes a part that
    /// ClickHouse later rewrites in merges, so fewer, larger inserts cost far
    /// less disk I/O than one small insert a second.
    pub telemetry_flush_interval: Duration,

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

    /// Serve `GET /v1/load`, each model's live load for callers
    /// (`OBLETH_LOAD_ENDPOINT`, default on). Off, the path answers 404.
    pub load_endpoint: bool,

    /// Who may poll, download, delete and list a video job
    /// (`OBLETH_VIDEO_JOB_SCOPE`): the key that created it (`key`, the
    /// default) or any key of its tenant (`tenant`).
    pub job_scope: JobScope,

    /// Derive a session id for requests that send none
    /// (`OBLETH_SESSION_ID_DERIVATION`, default on).
    pub session_id_derivation: bool,

    /// Longest a request waits in the fairshare queue before a 503
    /// (`OBLETH_ADMISSION_TIMEOUT_SECS`, default 60).
    pub admission_timeout: Duration,

    /// How long video and mesh job records are kept
    /// (`OBLETH_VIDEO_JOB_RETENTION_HOURS`, default 24). The backend forgets
    /// a finished job an hour after it completes (two for one that never
    /// finished), so a day is well past the last moment a follow-up could
    /// succeed.
    pub video_job_retention: Duration,

    /// Refuse a request that has been in flight at two engine deaths
    /// (`OBLETH_CRASH_GUARD`, default on).
    pub crash_guard: bool,

    /// Egress checks for operator-registered URLs.
    pub ssrf: SsrfConfig,

    /// How a request's client address and country are found
    /// (`OBLETH_TRUSTED_PROXIES`, `OBLETH_CLIENT_ADDRESS_HEADER`,
    /// `OBLETH_CLIENT_COUNTRY_HEADER`).
    pub client_address: crate::network::ClientAddressSettings,

    /// The prompt-compressor sidecar.
    pub compressor: CompressorConfig,

    /// Batch and background-work settings.
    pub batches: BatchSettings,

    /// The S3 or S3-compatible object store (`OBLETH_S3_*`); `None` when
    /// `OBLETH_S3_BUCKET` is unset.
    pub object_store: Option<ObjectStoreSettings>,

    /// Settings that were present but invalid and fell back to a safe
    /// default, one message each. Logged at startup once tracing is up (this
    /// crate does not log).
    pub warnings: Vec<String>,
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

/// Session affinity in the fairshare queue. An agent sends the steps of a
/// session one after another over a growing prompt, and the upstream keeps
/// that prefix cached only while other work does not push it out. Affinity
/// lets a session that is still warm go ahead of older requests from the same
/// key, and keeps the slot a step frees for the session's next step for a
/// moment. Off unless `OBLETH_FAIRSHARE_SESSION_AFFINITY` is set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionAffinityConfig {
    /// How long after its last step a session counts as warm
    /// (`OBLETH_FAIRSHARE_SESSION_AFFINITY_WARM_SECS`, default 120).
    pub warm_ttl: Duration,
    /// How long a slot is held for a session's next step
    /// (`OBLETH_FAIRSHARE_SESSION_AFFINITY_HOLD_MS`, default 3000; 0 holds
    /// nothing). Also the longest gap between one step finishing and the next
    /// arriving for the session to count as a loop worth holding for. At most
    /// `max_bypass`.
    pub hold: Duration,
    /// How many times a queued request may be passed by later requests of
    /// its key, by warm sessions or held slots, before it goes next; also
    /// the most holds in a row a session gets while others wait for the pool
    /// (`OBLETH_FAIRSHARE_SESSION_AFFINITY_MAX_JUMPS`, default 8, at least 1).
    pub max_jumps: u32,
    /// A request that has waited this long is no longer passed over, and
    /// held slots give way to it
    /// (`OBLETH_FAIRSHARE_SESSION_AFFINITY_MAX_BYPASS_SECS`, default 30).
    pub max_bypass: Duration,
    /// The most of a pool's slots that may be held at once, in percent
    /// (`OBLETH_FAIRSHARE_SESSION_AFFINITY_MAX_HELD_PERCENT`, default 25,
    /// at most 100; 0 holds nothing). Rounded up, so any pool can hold one.
    pub max_held_percent: u8,
}

impl Default for SessionAffinityConfig {
    /// The default values, as with no variables set.
    fn default() -> Self {
        Self::settings_from_values(None, None, None, None, None)
    }
}

impl SessionAffinityConfig {
    pub const DEFAULT_WARM_SECS: u64 = 120;
    pub const DEFAULT_HOLD_MS: u64 = 3000;
    pub const DEFAULT_MAX_JUMPS: u32 = 8;
    pub const DEFAULT_MAX_BYPASS_SECS: u64 = 30;
    pub const DEFAULT_MAX_HELD_PERCENT: u8 = 25;

    /// Build from raw env values; `None` unless `enabled` reads as true.
    /// Missing or unparseable values take their defaults, and zero where
    /// zero makes no sense (the warm window, the bypass limit, the jump
    /// count) does too.
    pub fn from_values(
        enabled: Option<&str>,
        warm_secs: Option<&str>,
        hold_ms: Option<&str>,
        max_jumps: Option<&str>,
        max_bypass_secs: Option<&str>,
        max_held_percent: Option<&str>,
    ) -> Option<Self> {
        lenient_bool(enabled, false).then(|| {
            Self::settings_from_values(
                warm_secs,
                hold_ms,
                max_jumps,
                max_bypass_secs,
                max_held_percent,
            )
        })
    }

    /// [`SessionAffinityConfig::from_values`] without the switch.
    pub fn settings_from_values(
        warm_secs: Option<&str>,
        hold_ms: Option<&str>,
        max_jumps: Option<&str>,
        max_bypass_secs: Option<&str>,
        max_held_percent: Option<&str>,
    ) -> Self {
        fn num<T: std::str::FromStr>(v: Option<&str>) -> Option<T> {
            v.and_then(|s| s.trim().parse().ok())
        }
        let warm = num(warm_secs)
            .filter(|n| *n > 0)
            .unwrap_or(Self::DEFAULT_WARM_SECS);
        let max_bypass = Duration::from_secs(
            num(max_bypass_secs)
                .filter(|n| *n > 0)
                .unwrap_or(Self::DEFAULT_MAX_BYPASS_SECS),
        );
        let hold = Duration::from_millis(num(hold_ms).unwrap_or(Self::DEFAULT_HOLD_MS));
        SessionAffinityConfig {
            warm_ttl: Duration::from_secs(warm),
            hold: hold.min(max_bypass),
            max_jumps: num(max_jumps)
                .filter(|n| *n > 0)
                .unwrap_or(Self::DEFAULT_MAX_JUMPS),
            max_bypass,
            max_held_percent: num::<u8>(max_held_percent)
                .unwrap_or(Self::DEFAULT_MAX_HELD_PERCENT)
                .min(100),
        }
    }

    /// How many of a pool's `cap` slots may be held at once.
    pub fn max_held(&self, cap: usize) -> usize {
        if self.hold.is_zero() {
            return 0;
        }
        (cap * self.max_held_percent as usize).div_ceil(100)
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

/// The Postgres connection pool: how many connections one gateway process
/// may hold, and how long a request waits for a free one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PgPoolSettings {
    /// `OBLETH_PG_MAX_CONNECTIONS`, default 10. Every replica holds up to
    /// this many, so Postgres `max_connections` must cover replicas times
    /// this plus the control plane and the migrations.
    pub max_connections: u32,
    /// `OBLETH_PG_ACQUIRE_TIMEOUT_SECS`, default 5: how long a request waits
    /// for a connection when all are busy before it fails instead of
    /// queueing behind the pool.
    pub acquire_timeout: Duration,
}

impl PgPoolSettings {
    pub const DEFAULT_MAX_CONNECTIONS: u32 = 10;
    pub const DEFAULT_ACQUIRE_TIMEOUT_SECS: u64 = 5;

    /// Build from raw env values; missing, unparseable, or zero values fall
    /// back to the defaults (an empty pool or a zero wait would fail every
    /// query).
    pub fn from_values(max_connections: Option<&str>, acquire_timeout_secs: Option<&str>) -> Self {
        fn positive(v: Option<&str>, default: u64) -> u64 {
            v.and_then(|s| s.trim().parse::<u64>().ok())
                .filter(|n| *n > 0)
                .unwrap_or(default)
        }
        PgPoolSettings {
            max_connections: positive(max_connections, Self::DEFAULT_MAX_CONNECTIONS as u64)
                .min(u32::MAX as u64) as u32,
            acquire_timeout: Duration::from_secs(positive(
                acquire_timeout_secs,
                Self::DEFAULT_ACQUIRE_TIMEOUT_SECS,
            )),
        }
    }
}

impl Default for PgPoolSettings {
    fn default() -> Self {
        Self::from_values(None, None)
    }
}

impl Config {
    /// The upstream read timeout with nothing set: the default upstream
    /// request timeout.
    pub const DEFAULT_UPSTREAM_READ_TIMEOUT_SECS: u64 = DEFAULT_UPSTREAM_TIMEOUT_SECS;

    /// Read the configuration from the process environment. See
    /// [`Config::from_lookup`].
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(&|key| env::var(key).ok())
    }

    /// Read the configuration through `lookup` (a variable's value, `None`
    /// when unset). A value that is set but cannot be read falls back to its
    /// default with a message in `warnings`; a setting the gateway must not
    /// guess at (the admin token, a safety switch, a JWT issuer, an SSRF
    /// range) fails the whole read, naming every such problem.
    pub fn from_lookup(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let mut r = EnvReader::new(lookup);
        let job_scope = job_scope(r.get("OBLETH_VIDEO_JOB_SCOPE").as_deref(), &mut r.warnings);
        let (fairshare_replica_heartbeat, fairshare_replica_ttl) = replica_heartbeat_timing(
            r.parse("OBLETH_FAIRSHARE_REPLICA_HEARTBEAT_SECS", 5),
            r.parse("OBLETH_FAIRSHARE_REPLICA_TTL_SECS", 15),
        );
        let upstream_timeout_secs = r.parse(
            "OBLETH_UPSTREAM_TIMEOUT_SECS",
            DEFAULT_UPSTREAM_TIMEOUT_SECS,
        );
        let jwt_issuers =
            match crate::jwt::parse_jwt_issuers(&r.get("OBLETH_JWT_ISSUERS").unwrap_or_default()) {
                Ok(issuers) => issuers,
                Err(e) => {
                    // A trust anchor that fails to parse must not silently
                    // disable itself.
                    r.fail(format!("OBLETH_JWT_ISSUERS: {e}"));
                    Vec::new()
                }
            };
        let affinity = [
            "OBLETH_FAIRSHARE_SESSION_AFFINITY_WARM_SECS",
            "OBLETH_FAIRSHARE_SESSION_AFFINITY_HOLD_MS",
            "OBLETH_FAIRSHARE_SESSION_AFFINITY_MAX_JUMPS",
            "OBLETH_FAIRSHARE_SESSION_AFFINITY_MAX_BYPASS_SECS",
        ]
        .map(|key| r.number::<u64>(key));
        let held_percent = r.number::<u8>("OBLETH_FAIRSHARE_SESSION_AFFINITY_MAX_HELD_PERCENT");
        let fairshare_session_affinity_settings = SessionAffinityConfig::settings_from_values(
            affinity[0].as_deref(),
            affinity[1].as_deref(),
            affinity[2].as_deref(),
            affinity[3].as_deref(),
            held_percent.as_deref(),
        );
        let config = Config {
            proxy_listen: r.string("OBLETH_PROXY_LISTEN", "0.0.0.0:8080"),
            admin_listen: r.string("OBLETH_ADMIN_LISTEN", "0.0.0.0:9180"),
            metrics_listen: r.string("OBLETH_METRICS_LISTEN", "0.0.0.0:9091"),
            upstream_base_url: r.string("OBLETH_UPSTREAM_BASE_URL", "http://127.0.0.1:8081"),
            upstream_timeout: Duration::from_secs(upstream_timeout_secs),
            upstream_pool_idle_secs: r.parse("OBLETH_UPSTREAM_POOL_IDLE_SECS", 15),
            upstream_tcp_keepalive_secs: r.parse("OBLETH_UPSTREAM_TCP_KEEPALIVE_SECS", 30),
            upstream_connect_timeout_secs: r.parse("OBLETH_UPSTREAM_CONNECT_TIMEOUT_SECS", 5),
            upstream_read_timeout_secs: upstream_read_timeout_secs(
                r.number::<u64>("OBLETH_UPSTREAM_READ_TIMEOUT_SECS")
                    .as_deref(),
                upstream_timeout_secs,
            ),
            shutdown_grace: Duration::from_secs(r.parse("OBLETH_SHUTDOWN_GRACE_SECS", 300)),
            redis_rewarm_secs: r.parse("OBLETH_REDIS_REWARM_SECS", 300),
            redis_url: r.string("OBLETH_REDIS_URL", "redis://127.0.0.1:6379"),
            redis_timeouts: RedisTimeouts::from_values(
                r.number::<u64>("OBLETH_REDIS_RESPONSE_TIMEOUT_MS")
                    .as_deref(),
                r.number::<u64>("OBLETH_REDIS_CONNECT_TIMEOUT_MS")
                    .as_deref(),
            ),
            database_url: r.string(
                "OBLETH_DATABASE_URL",
                "postgres://obleth:obleth@127.0.0.1:5432/obleth",
            ),
            pg_pool: PgPoolSettings::from_values(
                r.number::<u64>("OBLETH_PG_MAX_CONNECTIONS").as_deref(),
                r.number::<u64>("OBLETH_PG_ACQUIRE_TIMEOUT_SECS").as_deref(),
            ),
            clickhouse_url: r.string("OBLETH_CLICKHOUSE_URL", "http://127.0.0.1:8123"),
            clickhouse_db: r.string("OBLETH_CLICKHOUSE_DB", "obleth"),
            clickhouse_user: r.string("OBLETH_CLICKHOUSE_USER", "default"),
            clickhouse_password: r.string("OBLETH_CLICKHOUSE_PASSWORD", ""),
            admin_token: r.secret("OBLETH_ADMIN_TOKEN"),
            global_max_in_flight: r
                .parse("OBLETH_GLOBAL_MAX_IN_FLIGHT", DEFAULT_GLOBAL_MAX_IN_FLIGHT),
            default_model_max_in_flight: r.parse("OBLETH_DEFAULT_MODEL_MAX_IN_FLIGHT", 32),
            fairshare_history_secs: r.parse("OBLETH_FAIRSHARE_HISTORY_SECS", 3600),
            fairshare_algorithm: FairshareAlgorithm::parse(
                &r.string("OBLETH_FAIRSHARE_ALGORITHM", "hierarchical"),
            ),
            fairshare_shared_slots: r.flag("OBLETH_FAIRSHARE_SHARED_SLOTS", true),
            fairshare_replica_aware: r.flag("OBLETH_FAIRSHARE_REPLICA_AWARE", true),
            fairshare_replica_heartbeat,
            fairshare_replica_ttl,
            capacity_discovery: CapacityDiscoveryConfig::from_values(
                Some(bool_str(r.flag("OBLETH_CAPACITY_DISCOVERY_ENABLED", false))),
                r.number::<u64>("OBLETH_CAPACITY_DISCOVERY_INTERVAL_SECS")
                    .as_deref(),
                r.get("OBLETH_CAPACITY_DISCOVERY_NAMESPACES").as_deref(),
                r.get("OBLETH_CAPACITY_DEFAULT_SERVICE").as_deref(),
            ),
            fairshare_session_affinity: r
                .flag("OBLETH_FAIRSHARE_SESSION_AFFINITY", false)
                .then_some(fairshare_session_affinity_settings),
            fairshare_session_affinity_settings,
            schema_verify_only: r.strict_flag("OBLETH_SCHEMA_VERIFY_ONLY", false),
            fail_open: r.strict_flag("OBLETH_FAIL_OPEN", true),
            wal_path: r.string("OBLETH_WAL_PATH", "./obleth-telemetry.wal"),
            telemetry_flush_interval: telemetry_flush_interval(
                r.number::<u64>("OBLETH_TELEMETRY_FLUSH_INTERVAL_MS")
                    .as_deref(),
            ),
            model_health_enabled: r.flag("OBLETH_MODEL_HEALTH_ENABLED", true),
            model_health_interval_secs: r.parse("OBLETH_MODEL_HEALTH_INTERVAL_SECS", 900),
            model_health_timeout_secs: r.parse("OBLETH_MODEL_HEALTH_TIMEOUT_SECS", 30),
            model_health_retention_days: r.parse("OBLETH_MODEL_HEALTH_RETENTION_DAYS", 30),
            jwt_issuers,
            usage_retention_days: r.parse("OBLETH_USAGE_RETENTION_DAYS", 180),
            otel_endpoint: r.get("OBLETH_OTEL_ENDPOINT"),
            slack_alerts: SlackAlertConfig {
                webhook_url: r.get("OBLETH_SLACK_WEBHOOK_URL"),
                min_interval: Duration::from_secs(
                    r.parse("OBLETH_SLACK_ALERT_MIN_INTERVAL_SECS", 300),
                ),
            },
            auto_classifier_enabled: r.flag("OBLETH_AUTO_CLASSIFIER_ENABLED", false),
            auto_classifier_model: r.get("OBLETH_AUTO_CLASSIFIER_MODEL"),
            auto_classifier_timeout_ms: r.parse("OBLETH_AUTO_CLASSIFIER_TIMEOUT_MS", 250),
            load_endpoint: r.flag("OBLETH_LOAD_ENDPOINT", true),
            session_id_derivation: r.flag("OBLETH_SESSION_ID_DERIVATION", true),
            admission_timeout: Duration::from_secs(r.positive("OBLETH_ADMISSION_TIMEOUT_SECS", 60)),
            video_job_retention: Duration::from_secs(
                r.positive("OBLETH_VIDEO_JOB_RETENTION_HOURS", 24)
                    .saturating_mul(3600),
            ),
            crash_guard: r.flag("OBLETH_CRASH_GUARD", true),
            job_scope,
            ssrf: SsrfConfig::read(&mut r),
            client_address: client_address(&mut r),
            compressor: CompressorConfig::read(&mut r),
            batches: BatchSettings::read(&mut r),
            object_store: ObjectStoreSettings::read(&r),
            warnings: Vec::new(),
        };
        if let Some(prefix) = r.get("OBLETH_KEY_PREFIX") {
            if !crate::keys::is_valid_key_prefix(&prefix) {
                r.warn(format!(
                    "OBLETH_KEY_PREFIX={prefix:?} is not a key prefix (1 to {} lowercase letters \
                     or digits, starting with a letter); new keys start with `{}`",
                    crate::keys::KEY_PREFIX_MAX_CHARS,
                    crate::keys::DEFAULT_KEY_PREFIX,
                ));
            }
        }
        r.finish(config)
    }
}

/// The object store's settings as given. Whether they are usable (a bucket
/// name S3 accepts, a URL, both keys) is decided where the store is built,
/// which turns object storage off with an error when they are not.
#[derive(Clone, PartialEq, Eq)]
pub struct ObjectStoreSettings {
    /// `OBLETH_S3_BUCKET`.
    pub bucket: String,
    /// `OBLETH_S3_ENDPOINT`; `None` for AWS S3 itself.
    pub endpoint: Option<String>,
    /// `OBLETH_S3_REGION`, default `us-east-1`.
    pub region: String,
    /// `OBLETH_S3_ACCESS_KEY_ID`.
    pub access_key: Option<String>,
    /// `OBLETH_S3_SECRET_ACCESS_KEY`.
    pub secret_key: Option<String>,
}

impl ObjectStoreSettings {
    fn read(r: &EnvReader) -> Option<Self> {
        Some(ObjectStoreSettings {
            bucket: r.get("OBLETH_S3_BUCKET")?,
            endpoint: r.get("OBLETH_S3_ENDPOINT"),
            region: r
                .get("OBLETH_S3_REGION")
                .unwrap_or_else(|| "us-east-1".into()),
            access_key: r.get("OBLETH_S3_ACCESS_KEY_ID"),
            secret_key: r.get("OBLETH_S3_SECRET_ACCESS_KEY"),
        })
    }
}

impl std::fmt::Debug for ObjectStoreSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectStoreSettings")
            .field("bucket", &self.bucket)
            .field("endpoint", &self.endpoint)
            .field("region", &self.region)
            .field("access_key", &self.access_key.as_ref().map(|_| "(set)"))
            .field("secret_key", &self.secret_key.as_ref().map(|_| "(set)"))
            .finish()
    }
}

/// The settings the gateway refused to start with, one sentence each.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError(pub Vec<String>);

impl fmt::Display for ConfigError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "refusing to start: {}", self.0.join("; "))
    }
}

impl std::error::Error for ConfigError {}

/// Reads settings for [`Config::from_lookup`]: one parser for every
/// variable, collecting a warning for each value that is set but unreadable
/// (it falls back to its default) and an error for each the gateway must not
/// guess at.
struct EnvReader<'a> {
    lookup: &'a dyn Fn(&str) -> Option<String>,
    warnings: Vec<String>,
    errors: Vec<String>,
}

impl<'a> EnvReader<'a> {
    fn new(lookup: &'a dyn Fn(&str) -> Option<String>) -> Self {
        EnvReader {
            lookup,
            warnings: Vec::new(),
            errors: Vec::new(),
        }
    }

    /// The value, trimmed; `None` when unset or blank, so a deployment
    /// template can pass a variable through empty.
    fn get(&self, key: &str) -> Option<String> {
        (self.lookup)(key)
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
    }

    /// The value as set, untrimmed (a password or path may hold spaces), or
    /// `default` when unset.
    fn string(&self, key: &str, default: &str) -> String {
        (self.lookup)(key).unwrap_or_else(|| default.to_string())
    }

    fn warn(&mut self, message: String) {
        self.warnings.push(message);
    }

    fn fail(&mut self, message: String) {
        self.errors.push(message);
    }

    /// `key` as a `T`, or `default` when unset, or when set but not a `T`
    /// (with a warning).
    fn parse<T: std::str::FromStr + fmt::Display>(&mut self, key: &str, default: T) -> T {
        let Some(raw) = self.get(key) else {
            return default;
        };
        match raw.parse() {
            Ok(v) => v,
            Err(_) => {
                self.warn(format!(
                    "{key}={raw:?} is not a valid value; using {default}"
                ));
                default
            }
        }
    }

    /// `key` as a number above zero, or `default` when unset, or when set to
    /// anything else (with a warning).
    fn positive(&mut self, key: &str, default: u64) -> u64 {
        match self.parse(key, default) {
            0 => {
                self.warn(format!("{key}=0 is not allowed; using {default}"));
                default
            }
            n => n,
        }
    }

    /// The value when it reads as a `T`, for the `from_values` builders;
    /// `None` when unset, or when set but not a `T` (with a warning).
    fn number<T: std::str::FromStr>(&mut self, key: &str) -> Option<String> {
        let raw = self.get(key)?;
        if raw.parse::<T>().is_ok() {
            return Some(raw);
        }
        self.warn(format!(
            "{key}={raw:?} is not a valid number; using the default"
        ));
        None
    }

    /// A switch ([`parse_bool`] spellings), or `default` when unset, or when
    /// set to anything else (with a warning).
    fn flag(&mut self, key: &str, default: bool) -> bool {
        match strict_bool(key, self.get(key).as_deref(), default) {
            Ok(v) => v,
            Err(msg) => {
                self.warn(format!("{msg}; using {default}"));
                default
            }
        }
    }

    /// A switch whose misreading is unsafe (a typo must not turn budget
    /// enforcement fail-open, or egress checks off): anything but a
    /// [`parse_bool`] spelling fails the read.
    fn strict_flag(&mut self, key: &str, default: bool) -> bool {
        strict_bool(key, self.get(key).as_deref(), default).unwrap_or_else(|msg| {
            self.fail(msg);
            default
        })
    }

    /// A credential that must never fall back to a development default.
    fn secret(&mut self, key: &str) -> String {
        match (self.lookup)(key) {
            Some(v) if !v.trim().is_empty() => v,
            _ => {
                self.fail(format!("{key} must be set to a non-empty value"));
                String::new()
            }
        }
    }

    fn finish(self, mut config: Config) -> Result<Config, ConfigError> {
        if !self.errors.is_empty() {
            return Err(ConfigError(self.errors));
        }
        config.warnings = self.warnings;
        Ok(config)
    }
}

fn bool_str(v: bool) -> &'static str {
    if v {
        "true"
    } else {
        "false"
    }
}

/// Egress checks for URLs an operator registers (upstreams, MCP servers,
/// webhooks, JWKS): `OBLETH_BLOCK_PRIVATE_NETWORKS` and
/// `OBLETH_ALLOWED_PRIVATE_CIDRS`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SsrfConfig {
    /// Refuse private and internal targets unless listed in
    /// `allowed_private_cidrs` (default off: private targets are allowed,
    /// link-local and metadata addresses never are).
    pub block_private_networks: bool,
    /// Ranges allowed even when private networks are blocked. Never
    /// reopens link-local or metadata addresses.
    pub allowed_private_cidrs: Vec<ipnet::IpNet>,
}

impl SsrfConfig {
    /// Both settings are fatal when misread: a dropped range or a misread
    /// switch would silently change what the gateway may reach.
    fn read(r: &mut EnvReader) -> Self {
        let block_private_networks = r.strict_flag("OBLETH_BLOCK_PRIVATE_NETWORKS", false);
        let (allowed_private_cidrs, invalid) = crate::network::parse_network_list(
            &r.get("OBLETH_ALLOWED_PRIVATE_CIDRS").unwrap_or_default(),
        );
        if !invalid.is_empty() {
            r.fail(format!(
                "OBLETH_ALLOWED_PRIVATE_CIDRS has entries that are not CIDR ranges or \
                 addresses: {}",
                invalid.join(", ")
            ));
        }
        SsrfConfig {
            block_private_networks,
            allowed_private_cidrs,
        }
    }
}

/// How the gateway works out a request's client. Entries of
/// `OBLETH_TRUSTED_PROXIES` that are not ranges are skipped with a warning:
/// a typo there only means that proxy's `X-Forwarded-For` is not believed.
fn client_address(r: &mut EnvReader) -> crate::network::ClientAddressSettings {
    let settings = crate::network::ClientAddressSettings::from_settings(
        (r.lookup)("OBLETH_TRUSTED_PROXIES").as_deref(),
        (r.lookup)("OBLETH_CLIENT_ADDRESS_HEADER").as_deref(),
        (r.lookup)("OBLETH_CLIENT_COUNTRY_HEADER").as_deref(),
    );
    if !settings.invalid.is_empty() {
        r.warn(format!(
            "OBLETH_TRUSTED_PROXIES has entries that are not CIDR ranges; ignoring them: {}",
            settings.invalid.join(", ")
        ));
    }
    settings
}

/// The prompt-compressor sidecar: `OBLETH_COMPRESSOR_URL` (unset or blank:
/// none) and `OBLETH_COMPRESSOR_TIMEOUT_MS` (default 800) for each scoring
/// call on the request path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompressorConfig {
    /// Base URL without a trailing `/`; `None` when no sidecar is set up.
    pub url: Option<String>,
    pub timeout: Duration,
}

impl CompressorConfig {
    pub const DEFAULT_TIMEOUT_MS: u64 = 800;

    fn read(r: &mut EnvReader) -> Self {
        let url = r
            .get("OBLETH_COMPRESSOR_URL")
            .map(|u| u.trim_end_matches('/').to_string())
            .filter(|u| !u.is_empty());
        let ms = r
            .parse("OBLETH_COMPRESSOR_TIMEOUT_MS", Self::DEFAULT_TIMEOUT_MS)
            .max(1);
        CompressorConfig {
            url,
            timeout: Duration::from_millis(ms),
        }
    }
}

impl Default for CompressorConfig {
    fn default() -> Self {
        CompressorConfig {
            url: None,
            timeout: Duration::from_millis(Self::DEFAULT_TIMEOUT_MS),
        }
    }
}

/// The share of a model's slots, and of the global ceiling, background work
/// may fill when nothing else says (`OBLETH_BATCH_MAX_SHARE`).
pub const DEFAULT_BACKGROUND_SHARE: f64 = 0.75;

/// Prompt tokens per slot a background request holds, by default
/// (`OBLETH_BACKGROUND_SLOT_TOKENS`).
pub const DEFAULT_BACKGROUND_SLOT_TOKENS: u32 = 4096;

/// Batch and background-work settings (`OBLETH_BATCH_*`,
/// `OBLETH_BACKGROUND_SLOT_TOKENS`).
#[derive(Debug, Clone, PartialEq)]
pub struct BatchSettings {
    pub max_file_bytes: usize,
    pub max_lines: usize,
    pub max_active_per_key: i64,
    /// Shortest and longest completion window a batch may ask for.
    pub window_min: Duration,
    pub window_max: Duration,
    pub max_attempts: i32,
    /// Run the batch runner on this replica (`OBLETH_BATCH_RUNNER`, default on).
    pub runner: bool,
    pub concurrency: usize,
    /// How long a batch and its answers are kept after it ends; also how
    /// long an uploaded file waits for a batch to read it.
    pub retention: Duration,
    pub scope: JobScope,
    /// The share of a model's slots background work may fill, 0 to 1.
    pub share: f64,
    /// Prompt tokens per slot a background request holds; 0 holds one slot
    /// per request whatever its prompt.
    pub slot_tokens: u32,
}

impl Default for BatchSettings {
    fn default() -> Self {
        BatchSettings::read(&mut EnvReader::new(&|_| None))
    }
}

impl BatchSettings {
    const DEFAULT_RETENTION: Duration = Duration::from_secs(24 * 3600);

    fn read(r: &mut EnvReader) -> Self {
        let window = |r: &mut EnvReader, key: &str, default: Duration| match r.get(key) {
            None => default,
            Some(raw) => parse_window(&raw).unwrap_or_else(|| {
                r.warn(format!(
                    "{key}={raw:?} is not <n>h or <n>d; using {}h",
                    default.as_secs() / 3600
                ));
                default
            }),
        };
        let retention = match r.get("OBLETH_BATCH_RETENTION") {
            Some(_) => window(r, "OBLETH_BATCH_RETENTION", Self::DEFAULT_RETENTION),
            None => match r.number::<u64>("OBLETH_BATCH_RETENTION_DAYS") {
                Some(days) => {
                    r.warn(
                        "OBLETH_BATCH_RETENTION_DAYS is deprecated; set OBLETH_BATCH_RETENTION \
                         (e.g. 72h) instead"
                            .into(),
                    );
                    let days: u64 = days.parse().unwrap_or(1);
                    Duration::from_secs(days.max(1) * 86400)
                }
                None => Self::DEFAULT_RETENTION,
            },
        };
        let scope = JobScope::parse(r.get("OBLETH_BATCH_SCOPE").as_deref()).unwrap_or_else(|bad| {
            r.warn(format!(
                "OBLETH_BATCH_SCOPE={bad:?} is not `key` or `tenant`; using `key`"
            ));
            JobScope::Key
        });
        BatchSettings {
            max_file_bytes: r.parse("OBLETH_BATCH_MAX_FILE_BYTES", 200 * 1024 * 1024),
            max_lines: r.parse("OBLETH_BATCH_MAX_LINES", 50_000),
            max_active_per_key: r.parse("OBLETH_BATCH_MAX_ACTIVE_PER_KEY", 10),
            window_min: window(r, "OBLETH_BATCH_WINDOW_MIN", Duration::from_secs(3600)),
            window_max: window(r, "OBLETH_BATCH_WINDOW_MAX", Duration::from_secs(7 * 86400)),
            max_attempts: r.parse("OBLETH_BATCH_MAX_ATTEMPTS", 5).max(1),
            runner: r.flag("OBLETH_BATCH_RUNNER", true),
            concurrency: r.parse("OBLETH_BATCH_RUNNER_CONCURRENCY", 64).max(1),
            retention,
            scope,
            share: r
                .parse("OBLETH_BATCH_MAX_SHARE", DEFAULT_BACKGROUND_SHARE)
                .clamp(0.0, 1.0),
            slot_tokens: r.parse(
                "OBLETH_BACKGROUND_SLOT_TOKENS",
                DEFAULT_BACKGROUND_SLOT_TOKENS,
            ),
        }
    }
}

/// `<n>h` or `<n>d` as a duration.
pub fn parse_window(raw: &str) -> Option<Duration> {
    let raw = raw.trim().to_ascii_lowercase();
    let (n, unit) = raw.split_at(raw.len().checked_sub(1)?);
    let n: u64 = n.parse().ok().filter(|n| *n > 0)?;
    match unit {
        "h" => Some(Duration::from_secs(n * 3600)),
        "d" => Some(Duration::from_secs(n * 86400)),
        _ => None,
    }
}

/// Default `OBLETH_UPSTREAM_TIMEOUT_SECS`.
const DEFAULT_UPSTREAM_TIMEOUT_SECS: u64 = 300;

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

/// `OBLETH_VIDEO_JOB_SCOPE`. An unrecognised value falls back to `key`, the
/// narrower scope, with a warning: a typo must never share every user's jobs
/// across the tenant.
fn job_scope(raw: Option<&str>, warnings: &mut Vec<String>) -> JobScope {
    JobScope::parse(raw).unwrap_or_else(|bad| {
        warnings.push(format!(
            "OBLETH_VIDEO_JOB_SCOPE={bad:?} is not `key` or `tenant`; using `key` \
             (video jobs private to the key that created them)"
        ));
        JobScope::Key
    })
}

/// An explicit value wins; otherwise the upstream request timeout, floored at
/// 120s. reqwest's read timeout also bounds the wait for response headers,
/// so a default below the request timeout would cut slow non-streaming calls.
fn upstream_read_timeout_secs(raw: Option<&str>, upstream_timeout_secs: u64) -> u64 {
    raw.and_then(|v| v.trim().parse().ok())
        .unwrap_or(upstream_timeout_secs.max(120))
}

/// Default gap between telemetry inserts. ClickHouse advises about one insert
/// per second per table across all writers; 5s keeps several replicas under it.
pub const DEFAULT_TELEMETRY_FLUSH_INTERVAL_MS: u64 = 5_000;

/// Missing or unparseable values take the default. Others are clamped to
/// 100ms..=60s: zero would panic the flusher's ticker, and a long gap holds
/// more rows in memory, which a crash (not a graceful shutdown) would lose.
fn telemetry_flush_interval(raw: Option<&str>) -> Duration {
    let ms = raw
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(DEFAULT_TELEMETRY_FLUSH_INTERVAL_MS)
        .clamp(100, 60_000);
    Duration::from_millis(ms)
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
    fn session_affinity_is_off_unless_enabled() {
        let off = |v| SessionAffinityConfig::from_values(v, None, None, None, None, None);
        assert_eq!(off(None), None);
        assert_eq!(off(Some("false")), None);
        assert_eq!(off(Some("garbage")), None);
        let on = off(Some("true")).expect("on");
        assert_eq!(on.warm_ttl, Duration::from_secs(120));
        assert_eq!(on.hold, Duration::from_millis(3000));
        assert_eq!(on.max_jumps, 8);
        assert_eq!(on.max_bypass, Duration::from_secs(30));
        assert_eq!(on.max_held_percent, 25);
    }

    #[test]
    fn session_affinity_parses_and_bounds_its_values() {
        let c = SessionAffinityConfig::from_values(
            Some("on"),
            Some("60"),
            Some("90000"),
            Some("0"),
            Some("20"),
            Some("250"),
        )
        .expect("on");
        assert_eq!(c.warm_ttl, Duration::from_secs(60));
        assert_eq!(c.hold, Duration::from_secs(20), "hold capped at max_bypass");
        assert_eq!(c.max_jumps, 8, "zero jumps falls back to the default");
        assert_eq!(c.max_held_percent, 100);
        let none = SessionAffinityConfig::from_values(Some("1"), None, Some("0"), None, None, None)
            .expect("on");
        assert_eq!(none.max_held(64), 0, "hold 0 holds nothing");
    }

    #[test]
    fn session_affinity_max_held_rounds_up() {
        let c = SessionAffinityConfig::from_values(Some("1"), None, None, None, None, None)
            .expect("on");
        assert_eq!(c.max_held(1), 1);
        assert_eq!(c.max_held(8), 2);
        assert_eq!(c.max_held(10), 3);
        let c = SessionAffinityConfig::from_values(Some("1"), None, None, None, None, Some("0"))
            .expect("on");
        assert_eq!(c.max_held(10), 0);
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
    fn pg_pool_settings_default_and_parse() {
        let d = PgPoolSettings::default();
        assert_eq!(d.max_connections, 10);
        assert_eq!(d.acquire_timeout, Duration::from_secs(5));
        let p = PgPoolSettings::from_values(Some("40"), Some("2"));
        assert_eq!(p.max_connections, 40);
        assert_eq!(p.acquire_timeout, Duration::from_secs(2));
        let p = PgPoolSettings::from_values(Some("0"), Some("nope"));
        assert_eq!(p, PgPoolSettings::default());
    }

    #[test]
    fn telemetry_flush_interval_defaults_and_clamps() {
        assert_eq!(telemetry_flush_interval(None), Duration::from_secs(5));
        assert_eq!(
            telemetry_flush_interval(Some("nope")),
            Duration::from_secs(5)
        );
        assert_eq!(
            telemetry_flush_interval(Some(" 1000 ")),
            Duration::from_secs(1)
        );
        assert_eq!(
            telemetry_flush_interval(Some("0")),
            Duration::from_millis(100)
        );
        assert_eq!(
            telemetry_flush_interval(Some("600000")),
            Duration::from_secs(60)
        );
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
    fn video_job_scope_defaults_to_key_and_falls_back_to_it_with_a_warning() {
        let mut warnings = Vec::new();
        assert_eq!(job_scope(None, &mut warnings), JobScope::Key);
        assert_eq!(job_scope(Some(""), &mut warnings), JobScope::Key);
        assert_eq!(job_scope(Some(" Key "), &mut warnings), JobScope::Key);
        assert_eq!(job_scope(Some("TENANT"), &mut warnings), JobScope::Tenant);
        assert!(warnings.is_empty());
        assert_eq!(job_scope(Some("tenants"), &mut warnings), JobScope::Key);
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("OBLETH_VIDEO_JOB_SCOPE=\"tenants\""));
        assert_eq!(JobScope::Key.as_str(), "key");
        assert_eq!(JobScope::Tenant.as_str(), "tenant");
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

    /// The configuration read from `vars` alone, with the admin token set.
    fn config(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let mut map: std::collections::HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        map.entry("OBLETH_ADMIN_TOKEN".into())
            .or_insert_with(|| "t".into());
        Config::from_lookup(&|k| map.get(k).cloned())
    }

    #[test]
    fn object_storage_is_off_without_a_bucket_and_read_as_given_with_one() {
        assert_eq!(config(&[]).unwrap().object_store, None);
        assert_eq!(
            config(&[("OBLETH_S3_BUCKET", " "), ("OBLETH_S3_REGION", "eu-west-1")])
                .unwrap()
                .object_store,
            None
        );
        let c = config(&[
            ("OBLETH_S3_BUCKET", "obleth"),
            ("OBLETH_S3_ENDPOINT", "https://s3.example.edu"),
            ("OBLETH_S3_ACCESS_KEY_ID", "id"),
        ])
        .unwrap();
        let s = c.object_store.expect("on with a bucket");
        assert_eq!(s.bucket, "obleth");
        assert_eq!(s.endpoint.as_deref(), Some("https://s3.example.edu"));
        assert_eq!(s.region, "us-east-1");
        assert_eq!(s.access_key.as_deref(), Some("id"));
        // A missing key is left for the store to refuse, by name.
        assert_eq!(s.secret_key, None);
        assert!(!format!("{s:?}").contains("\"id\""));
    }

    #[test]
    fn unset_values_take_their_defaults_without_warnings() {
        let c = config(&[]).unwrap();
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!(c.global_max_in_flight, DEFAULT_GLOBAL_MAX_IN_FLIGHT);
        assert_eq!(c.upstream_timeout, Duration::from_secs(300));
        assert_eq!(
            c.upstream_read_timeout_secs,
            Config::DEFAULT_UPSTREAM_READ_TIMEOUT_SECS
        );
        assert_eq!(c.admission_timeout, Duration::from_secs(60));
        assert_eq!(c.video_job_retention, Duration::from_secs(24 * 3600));
        assert!(c.crash_guard && c.session_id_derivation && c.fairshare_shared_slots);
        assert!(c.fail_open && !c.schema_verify_only);
        assert_eq!(c.ssrf, SsrfConfig::default());
        assert_eq!(c.compressor, CompressorConfig::default());
        assert_eq!(c.batches, BatchSettings::default());
        assert_eq!(c.batches.share, DEFAULT_BACKGROUND_SHARE);
        assert!(c.batches.runner);
        assert!(c.jwt_issuers.is_empty());
        // Blank is unset, so a template can pass a variable through empty.
        let blank = config(&[
            ("OBLETH_GLOBAL_MAX_IN_FLIGHT", ""),
            ("OBLETH_FAIL_OPEN", " "),
            ("OBLETH_ALLOWED_PRIVATE_CIDRS", ""),
            ("OBLETH_BATCH_RUNNER", ""),
        ])
        .unwrap();
        assert!(blank.warnings.is_empty(), "{:?}", blank.warnings);
        assert_eq!(blank.global_max_in_flight, DEFAULT_GLOBAL_MAX_IN_FLIGHT);
    }

    #[test]
    fn set_values_are_read_trimmed() {
        let c = config(&[
            ("OBLETH_GLOBAL_MAX_IN_FLIGHT", " 512 "),
            ("OBLETH_UPSTREAM_TIMEOUT_SECS", "600"),
            ("OBLETH_ADMISSION_TIMEOUT_SECS", "12"),
            ("OBLETH_VIDEO_JOB_RETENTION_HOURS", "6"),
            ("OBLETH_CRASH_GUARD", "off"),
            ("OBLETH_SESSION_ID_DERIVATION", "no"),
            ("OBLETH_FAIL_OPEN", "0"),
            ("OBLETH_BLOCK_PRIVATE_NETWORKS", "yes"),
            ("OBLETH_ALLOWED_PRIVATE_CIDRS", "10.0.0.0/8, 192.168.1.5"),
            ("OBLETH_COMPRESSOR_URL", "http://c:8080/"),
            ("OBLETH_COMPRESSOR_TIMEOUT_MS", "1200"),
            ("OBLETH_BATCH_MAX_SHARE", "0.5"),
            ("OBLETH_BACKGROUND_SLOT_TOKENS", "0"),
            ("OBLETH_BATCH_RUNNER", "false"),
            ("OBLETH_BATCH_RETENTION", "72h"),
            ("OBLETH_TRUSTED_PROXIES", "10.0.0.0/8"),
        ])
        .unwrap();
        assert!(c.warnings.is_empty(), "{:?}", c.warnings);
        assert_eq!(c.global_max_in_flight, 512);
        // The read timeout follows the request timeout when not set.
        assert_eq!(c.upstream_read_timeout_secs, 600);
        assert_eq!(c.admission_timeout, Duration::from_secs(12));
        assert_eq!(c.video_job_retention, Duration::from_secs(6 * 3600));
        assert!(!c.crash_guard && !c.session_id_derivation && !c.fail_open);
        assert!(c.ssrf.block_private_networks);
        assert_eq!(
            c.ssrf
                .allowed_private_cidrs
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<_>>(),
            ["10.0.0.0/8", "192.168.1.5/32"]
        );
        assert_eq!(c.compressor.url.as_deref(), Some("http://c:8080"));
        assert_eq!(c.compressor.timeout, Duration::from_millis(1200));
        assert_eq!(c.batches.share, 0.5);
        assert_eq!(c.batches.slot_tokens, 0);
        assert!(!c.batches.runner);
        assert_eq!(c.batches.retention, Duration::from_secs(72 * 3600));
        assert!(c.client_address.explicit);
    }

    #[test]
    fn unreadable_values_warn_and_take_their_defaults() {
        let c = config(&[
            ("OBLETH_GLOBAL_MAX_IN_FLIGHT", "4k"),
            ("OBLETH_ADMISSION_TIMEOUT_SECS", "0"),
            ("OBLETH_FAIRSHARE_SHARED_SLOTS", "maybe"),
            ("OBLETH_REDIS_RESPONSE_TIMEOUT_MS", "fast"),
            ("OBLETH_BATCH_WINDOW_MIN", "10m"),
            ("OBLETH_BATCH_RUNNER", "enabled"),
            ("OBLETH_TRUSTED_PROXIES", "10.0.0.0/8,proxy.local"),
            ("OBLETH_KEY_PREFIX", "Bad!"),
        ])
        .unwrap();
        assert_eq!(c.global_max_in_flight, DEFAULT_GLOBAL_MAX_IN_FLIGHT);
        assert_eq!(c.admission_timeout, Duration::from_secs(60));
        assert!(c.fairshare_shared_slots);
        assert_eq!(c.redis_timeouts, RedisTimeouts::default());
        assert_eq!(c.batches.window_min, Duration::from_secs(3600));
        assert!(c.batches.runner);
        assert_eq!(c.warnings.len(), 8, "{:#?}", c.warnings);
        for name in [
            "OBLETH_GLOBAL_MAX_IN_FLIGHT=\"4k\"",
            "OBLETH_ADMISSION_TIMEOUT_SECS=0",
            "OBLETH_FAIRSHARE_SHARED_SLOTS=\"maybe\"",
            "OBLETH_REDIS_RESPONSE_TIMEOUT_MS=\"fast\"",
            "OBLETH_BATCH_WINDOW_MIN=\"10m\"",
            "OBLETH_BATCH_RUNNER=\"enabled\"",
            "proxy.local",
            "OBLETH_KEY_PREFIX=\"Bad!\"",
        ] {
            assert!(
                c.warnings.iter().any(|w| w.contains(name)),
                "{name}: {:#?}",
                c.warnings
            );
        }
    }

    #[test]
    fn settings_the_gateway_must_not_guess_at_stop_the_start() {
        let err = config(&[
            ("OBLETH_ADMIN_TOKEN", " "),
            ("OBLETH_FAIL_OPEN", "maybe"),
            ("OBLETH_BLOCK_PRIVATE_NETWORKS", "enabled"),
            (
                "OBLETH_ALLOWED_PRIVATE_CIDRS",
                "10.0.0.0/8,10.0.0.0/33,intranet",
            ),
            ("OBLETH_JWT_ISSUERS", "not json"),
        ])
        .unwrap_err();
        let all = err.to_string();
        assert_eq!(err.0.len(), 5, "{all}");
        for name in [
            "OBLETH_ADMIN_TOKEN",
            "OBLETH_FAIL_OPEN",
            "OBLETH_BLOCK_PRIVATE_NETWORKS",
            "10.0.0.0/33, intranet",
            "OBLETH_JWT_ISSUERS",
        ] {
            assert!(all.contains(name), "{name}: {all}");
        }
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
