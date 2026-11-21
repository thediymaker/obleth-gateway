//! Redis layer: hot read-cache of resolved keys + live token-bucket counters.
//!
//! The data plane reads *only* Redis on the hot path (Postgres is the durable
//! source of truth written by the Management API, then synced here). Budget
//! checks run as atomic Lua so they are correct across many gateway pods.

mod prune;
mod resolver;
pub mod scripts;
mod slots;

pub use slots::{
    RedisSlots, SlotKeys, REPLICAS_KEY, SLOT_CHAIN_KEY, SLOT_HELD_PREFIX, SLOT_HOLDS_KEY,
    SLOT_HOLD_PREFIX, SLOT_RECENT_KEY, SLOT_RELEASE_CHANNEL, SLOT_TOTALS_KEY, SLOT_VIEW_PREFIX,
};

use std::sync::OnceLock;

use futures_util::StreamExt;
pub use obleth_config::config::RedisTimeouts;
use obleth_config::{CachedResponse, ResolvedKey, ResolvedMcpServer, ResolvedModel};
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::AsyncCommands;
pub use resolver::{Invalidation, Resolver, SavedSetting};
use uuid::Uuid;

/// Lua scripts are wrapped in `redis::Script` once (constructing one SHA-1
/// hashes the source) instead of per call on the request hot path.
macro_rules! cached_script {
    ($name:ident, $src:expr) => {
        fn $name() -> &'static redis::Script {
            static SCRIPT: OnceLock<redis::Script> = OnceLock::new();
            SCRIPT.get_or_init(|| redis::Script::new($src))
        }
    };
}

cached_script!(reserve_with_term_script, scripts::RESERVE_WITH_TERM);
cached_script!(reconcile_script, scripts::RECONCILE);
cached_script!(term_usage_add_script, scripts::TERM_USAGE_ADD);
cached_script!(term_reconcile_script, scripts::TERM_RECONCILE);
cached_script!(replica_heartbeat_script, scripts::REPLICA_HEARTBEAT);
cached_script!(replica_deregister_script, scripts::REPLICA_DEREGISTER);
cached_script!(peer_views_script, scripts::PEER_VIEWS);
cached_script!(slot_acquire_script, scripts::SLOT_ACQUIRE);
cached_script!(slot_release_script, scripts::SLOT_RELEASE);
cached_script!(slot_hold_script, scripts::SLOT_HOLD);
cached_script!(slot_reconcile_script, scripts::SLOT_RECONCILE);
cached_script!(crash_strike_script, scripts::CRASH_STRIKE);

/// Resolved keys, by key hash. Versioned: an entry is only ever written by a
/// build that knows every security field of `ResolvedKey`. When a field like
/// that is added, the namespace moves on, so an older replica still running
/// in a rollout (which keeps re-warming its own namespace) can never write an
/// entry a newer one reads with that field missing and defaulted to "off".
/// `v2` added `allowed_networks`, `v3` added `allowed_countries`. Each build re-warms its namespace from
/// Postgres at boot, before it serves, so a rollback reads its own entries
/// fresh too. Entries left in an older namespace are never read; they can be
/// removed with `redis-cli --scan --pattern 'obleth:key:*' | xargs redis-cli
/// del` (and the same for `obleth:key2:*`) once no older replica runs.
const KEY_PREFIX: &str = "obleth:key3:";
/// The namespaces before `KEY_PREFIX`. Only deletes touch them, so a key
/// revoked while older replicas still run is gone for them too.
const LEGACY_KEY_PREFIXES: [&str; 2] = ["obleth:key:", "obleth:key2:"];
const MODEL_PREFIX: &str = "obleth:model:";
const MCP_PREFIX: &str = "obleth:mcp:";
const BUDGET_PREFIX: &str = "obleth:budget:";
const TERM_USAGE_PREFIX: &str = "obleth:term_usage:";
/// In-flight term-budget reservations, kept apart from committed usage so a
/// never-settled request can only leak for the key's short TTL (see
/// `scripts::RESERVE_WITH_TERM`).
const TERM_RESERVED_PREFIX: &str = "obleth:term_reserved:";
const CACHE_PREFIX: &str = "obleth:cache:";
/// Namespace for compression-boon originals stashed for reversibility. Distinct
/// from the response cache so the two never collide.
const COMPRESS_PREFIX: &str = "obleth:compress:";
/// Compression-boon compacted forms, per tenant, so a segment compacts the
/// same way on every turn and the prompt prefix stays the same.
const COMPACTED_PREFIX: &str = "obleth:compacted:";
/// Namespace for cached query vectors (knowledge boon). The key itself is
/// built here from the hash the proxy supplies (see `query_vector_key`),
/// as with the tenant-scoped keys of `tenant_key`.
const KNOWLEDGE_QUERY_PREFIX: &str = "obleth:knowledge:q:";
/// Vision-boon image descriptions, per tenant, keyed by a hash of the image,
/// the describer and its prompt.
const VISION_PREFIX: &str = "obleth:vision:";
/// `auto` conversation records and answered-request markers, per tenant.
const AUTO_TURN_PREFIX: &str = "obleth:auto:";
/// Crash-guard strikes: how many model-server engine deaths one request (by
/// fingerprint) was being answered at, per model. A hash (`n`, `last`).
const CRASH_PREFIX: &str = "obleth:crashguard:";
/// Models with a crash-guard strike record still alive: a sorted set of
/// model names scored by when their newest record expires (Unix ms). Every
/// read drops the expired members, so the set stays as small as the list of
/// models struck recently.
const CRASH_RECENT_KEY: &str = "obleth:crashrecent";
const INVALIDATE_CHANNEL: &str = "obleth:invalidate";
/// Per-day request counters (see `daily_stats_key`).
const DAILY_STATS_PREFIX: &str = "obleth:daily:";
/// Fixed-window call counters (see `count_call`).
const CALL_COUNT_PREFIX: &str = "obleth:calls:";
/// Single shared key holding the provisioner's last-seen epoch seconds. Shared
/// via Redis (not per-pod memory) so the dashboard reads a consistent value no
/// matter which gateway pod served the provisioner's poll vs. the settings read.
const PROVISIONER_HEARTBEAT_KEY: &str = "obleth:provisioner:heartbeat";
/// Companion to the heartbeat holding the provisioner's reported build identity
/// (JSON: version/git_sha/built_at). Separate key so the bare-int heartbeat — and
/// the "running" derivation built on it — is untouched; both share a TTL so they
/// expire together when the provisioner stops.
const PROVISIONER_VERSION_KEY: &str = "obleth:provisioner:version";
/// Companion to the heartbeat holding the provisioner's last reconcile-tick
/// outcome (JSON: status/detail/at/last_ok_at/since). The heartbeat proves the
/// *process* is alive; this proves reconciliation is actually *succeeding* —
/// a provisioner can poll green for days while every tick fails against
/// slurmrestd and holds all replica state frozen.
const PROVISIONER_TICK_KEY: &str = "obleth:provisioner:tick";

#[derive(Debug, thiserror::Error)]
pub enum RedisError {
    #[error("redis: {0}")]
    Redis(#[from] redis::RedisError),
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),
}

type Result<T> = std::result::Result<T, RedisError>;

/// Caps for the cumulative term-budget gate of
/// [`RedisStore::reserve_budget_with_term`].
#[derive(Debug, Clone)]
pub struct TermGate<'a> {
    /// Period key namespacing the counters (see `term_period_key`).
    pub period_key: &'a str,
    /// Cumulative token cap, `None` = uncapped.
    pub budget_tokens: Option<i64>,
    /// Cumulative USD cap, `None` = uncapped.
    pub budget_cost_usd: Option<f64>,
}

/// Result of the combined term-gate + token-bucket admission check.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ReserveOutcome {
    /// Tokens reserved; carries the per-minute bucket remainder.
    Reserved { remaining: i64 },
    /// Per-minute bucket exhausted; nothing reserved.
    RateLimited { remaining: i64 },
    /// Cumulative term budget exhausted; nothing reserved. Carries the term
    /// usage observed by the gate, for alerting.
    TermExhausted { used_tokens: i64, used_cost: f64 },
}

/// Cloneable handle to Redis. Holds a multiplexed connection manager (auto
/// reconnecting) plus the client for creating dedicated pub/sub connections.
///
/// The response timeout (default 250 ms) applies to every command on the
/// shared connection, including admin-side batch work such as the Redis
/// resync and stale-entry prune. That is intended: those run as many small
/// commands, and a stalled Redis should fail them fast rather than hang.
/// Pub/sub listeners use their own connection and are not bounded by it.
#[derive(Clone)]
pub struct RedisStore {
    conn: ConnectionManager,
    client: redis::Client,
}

impl RedisStore {
    /// Connect with the default timeouts (250 ms response / 2 s connect).
    /// Deployments pass `Config.redis_timeouts` via [`Self::connect_with`].
    pub async fn connect(url: &str) -> Result<Self> {
        Self::connect_with(url, RedisTimeouts::default()).await
    }

    pub async fn connect_with(url: &str, timeouts: RedisTimeouts) -> Result<Self> {
        let client = redis::Client::open(url)?;
        // No internal retries: a request that lands while the manager is
        // reconnecting awaits the whole retry chain, so with retries a dead
        // Redis would stall each request for several connect timeouts instead
        // of one. The next command re-triggers a reconnect on its own, and
        // boot-time callers wrap `connect` in their own retry loop.
        let config = ConnectionManagerConfig::new()
            .set_response_timeout(timeouts.response)
            .set_connection_timeout(timeouts.connect)
            .set_number_of_retries(0);
        let conn = ConnectionManager::new_with_config(client.clone(), config).await?;
        Ok(RedisStore { conn, client })
    }

    fn key_cache(hash: &str) -> String {
        format!("{KEY_PREFIX}{hash}")
    }
    fn budget_key(tenant: &Uuid) -> String {
        format!("{BUDGET_PREFIX}{tenant}")
    }
    fn term_usage_key(tenant: &Uuid) -> String {
        format!("{TERM_USAGE_PREFIX}{tenant}")
    }
    fn term_reserved_key(scope: &Uuid) -> String {
        format!("{TERM_RESERVED_PREFIX}{scope}")
    }

    /// Record the provisioner heartbeat: the given epoch seconds, kept for
    /// `ttl_secs` then expired. Stored in Redis so any gateway pod can read a
    /// consistent value (the provisioner's poll and the dashboard's read may land
    /// on different pods behind a Service).
    pub async fn set_provisioner_heartbeat(&self, epoch_secs: i64, ttl_secs: u64) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = conn
            .set_ex(PROVISIONER_HEARTBEAT_KEY, epoch_secs, ttl_secs)
            .await?;
        Ok(())
    }

    /// The provisioner's last-seen epoch seconds, or `None` when it hasn't polled
    /// within the heartbeat TTL (key expired) or has never polled.
    pub async fn get_provisioner_heartbeat(&self) -> Result<Option<i64>> {
        let mut conn = self.conn.clone();
        let v: Option<i64> = conn.get(PROVISIONER_HEARTBEAT_KEY).await?;
        Ok(v)
    }

    /// Record the provisioner's reported build identity (an opaque JSON blob)
    /// alongside the heartbeat, expiring after `ttl_secs`. Stored separately from
    /// the bare-int heartbeat so the "running" derivation is unaffected.
    pub async fn set_provisioner_version(&self, json: &str, ttl_secs: u64) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = conn.set_ex(PROVISIONER_VERSION_KEY, json, ttl_secs).await?;
        Ok(())
    }

    /// The provisioner's last-reported build identity (raw JSON), or `None` when
    /// it hasn't reported within the TTL or never reported one.
    pub async fn get_provisioner_version(&self) -> Result<Option<String>> {
        let mut conn = self.conn.clone();
        let v: Option<String> = conn.get(PROVISIONER_VERSION_KEY).await?;
        Ok(v)
    }

    /// Record the provisioner's last reconcile-tick outcome (an opaque JSON
    /// blob), expiring after `ttl_secs`. Same rationale as the heartbeat: any
    /// gateway pod may serve the provisioner's poll or the dashboard's read.
    pub async fn set_provisioner_tick_status(&self, json: &str, ttl_secs: u64) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = conn.set_ex(PROVISIONER_TICK_KEY, json, ttl_secs).await?;
        Ok(())
    }

    /// The provisioner's last-reported tick outcome (raw JSON), or `None` when
    /// it hasn't reported within the TTL or never reported one.
    pub async fn get_provisioner_tick_status(&self) -> Result<Option<String>> {
        let mut conn = self.conn.clone();
        let v: Option<String> = conn.get(PROVISIONER_TICK_KEY).await?;
        Ok(v)
    }

    /// Refresh this replica's heartbeat for `ttl`, reclaim the shared slots
    /// of any replica whose heartbeat has expired, and return how many
    /// replicas are live (unexpired), this one included. One small script
    /// call; run it on an interval well under `ttl`, never per request.
    pub async fn replica_heartbeat(
        &self,
        instance: &str,
        ttl: std::time::Duration,
    ) -> Result<usize> {
        self.replica_heartbeat_in(&SlotKeys::default(), instance, ttl)
            .await
    }

    /// Drop this replica's heartbeat and its scheduler view, and free any
    /// shared slot it still holds, so the survivors grow at their next
    /// heartbeat instead of one TTL later. Called on clean shutdown.
    pub async fn replica_deregister(&self, instance: &str) -> Result<()> {
        self.replica_deregister_in(&SlotKeys::default(), instance)
            .await
    }

    /// [`RedisStore::replica_heartbeat`] with an explicit key layout.
    pub async fn replica_heartbeat_in(
        &self,
        keys: &SlotKeys,
        instance: &str,
        ttl: std::time::Duration,
    ) -> Result<usize> {
        let mut conn = self.conn.clone();
        let live: usize = replica_heartbeat_script()
            .key(&keys.replicas)
            .key(&keys.totals)
            .key(&keys.holds)
            .arg(instance)
            .arg(ttl.as_millis().max(1) as u64)
            .arg(&keys.held_prefix)
            .arg(&keys.channel)
            .arg(&keys.hold_prefix)
            .invoke_async(&mut conn)
            .await?;
        Ok(live)
    }

    /// [`RedisStore::replica_deregister`] with an explicit key layout.
    pub async fn replica_deregister_in(&self, keys: &SlotKeys, instance: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: i64 = replica_deregister_script()
            .key(&keys.replicas)
            .key(&keys.totals)
            .key(keys.held(instance))
            .key(keys.view(instance))
            .arg(instance)
            .arg(&keys.channel)
            .invoke_async(&mut conn)
            .await?;
        Ok(())
    }

    /// Hot-path lookup of a resolved key by its hash.
    pub async fn get_resolved_key(&self, hash: &str) -> Result<Option<ResolvedKey>> {
        self.get_json(Self::key_cache(hash)).await
    }

    /// Write/refresh the cached resolved key (called by the Management API sync).
    pub async fn put_resolved_key(&self, hash: &str, key: &ResolvedKey) -> Result<()> {
        self.set_json(Self::key_cache(hash), key).await
    }

    pub async fn delete_resolved_key(&self, hash: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = conn
            .del(&[
                Self::key_cache(hash),
                format!("{}{hash}", LEGACY_KEY_PREFIXES[0]),
                format!("{}{hash}", LEGACY_KEY_PREFIXES[1]),
            ])
            .await?;
        Ok(())
    }

    fn model_cache(name: &str) -> String {
        format!("{MODEL_PREFIX}{name}")
    }

    pub async fn get_resolved_model(&self, name: &str) -> Result<Option<ResolvedModel>> {
        self.get_json(Self::model_cache(name)).await
    }

    pub async fn put_resolved_model(&self, name: &str, model: &ResolvedModel) -> Result<()> {
        self.set_json(Self::model_cache(name), model).await
    }

    pub async fn delete_resolved_model(&self, name: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = conn.del(Self::model_cache(name)).await?;
        Ok(())
    }

    fn mcp_cache(name: &str) -> String {
        format!("{MCP_PREFIX}{name}")
    }

    pub async fn get_resolved_mcp_server(&self, name: &str) -> Result<Option<ResolvedMcpServer>> {
        self.get_json(Self::mcp_cache(name)).await
    }

    pub async fn put_resolved_mcp_server(
        &self,
        name: &str,
        server: &ResolvedMcpServer,
    ) -> Result<()> {
        self.set_json(Self::mcp_cache(name), server).await
    }

    /// `GET key`, parsed as JSON; `None` when the key is absent.
    async fn get_json<T: serde::de::DeserializeOwned>(&self, key: String) -> Result<Option<T>> {
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(key).await?;
        Ok(raw.map(|json| serde_json::from_str(&json)).transpose()?)
    }

    /// `SET key` to `value` as JSON, with no TTL.
    async fn set_json<T: serde::Serialize>(&self, key: String, value: &T) -> Result<()> {
        let mut conn = self.conn.clone();
        let json = serde_json::to_string(value)?;
        let _: () = conn.set(key, json).await?;
        Ok(())
    }

    pub async fn delete_resolved_mcp_server(&self, name: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = conn.del(Self::mcp_cache(name)).await?;
        Ok(())
    }

    fn response_cache_key(key: &str) -> String {
        format!("{CACHE_PREFIX}{key}")
    }

    /// Look up a cached upstream response by its exact-match key.
    pub async fn cache_get(&self, key: &str) -> Result<Option<CachedResponse>> {
        self.get_json(Self::response_cache_key(key)).await
    }

    /// Store a response in the cache, expiring after `ttl_secs`. A `ttl_secs`
    /// of 0 (or less) means caching is disabled and nothing is written: a
    /// never-expiring entry would be an unbounded memory path in the only
    /// hot-path store.
    pub async fn cache_put(&self, key: &str, value: &CachedResponse, ttl_secs: i64) -> Result<()> {
        if ttl_secs <= 0 {
            return Ok(());
        }
        let mut conn = self.conn.clone();
        let json = serde_json::to_string(value)?;
        let _: () = conn
            .set_ex(Self::response_cache_key(key), json, ttl_secs as u64)
            .await?;
        Ok(())
    }

    /// `{prefix}{tenant}:{id}`: the shape of every tenant-scoped cache key, so
    /// an id alone never reaches another tenant's entry.
    fn tenant_key(prefix: &str, tenant: &Uuid, id: &str) -> String {
        format!("{prefix}{tenant}:{id}")
    }

    /// The value stored under a tenant-scoped key, or `None` when it has
    /// expired or was never stored.
    async fn tenant_get(&self, prefix: &str, tenant: &Uuid, id: &str) -> Result<Option<String>> {
        let mut conn = self.conn.clone();
        let v: Option<String> = conn.get(Self::tenant_key(prefix, tenant, id)).await?;
        Ok(v)
    }

    /// `SET ... EX ttl_secs` under a tenant-scoped key. A `ttl_secs` of 0
    /// skips the write (same reasoning as `cache_put`).
    async fn tenant_set_ex(
        &self,
        prefix: &str,
        tenant: &Uuid,
        id: &str,
        value: &str,
        ttl_secs: u64,
    ) -> Result<()> {
        if ttl_secs == 0 {
            return Ok(());
        }
        let mut conn = self.conn.clone();
        let _: () = conn
            .set_ex(Self::tenant_key(prefix, tenant, id), value, ttl_secs)
            .await?;
        Ok(())
    }

    /// Stash an original content segment under the owning tenant and its
    /// content hash for later reversibility, expiring after `ttl_secs`. The
    /// tenant is part of the key so a hash alone never retrieves another
    /// tenant's text. A `ttl_secs` of 0 skips the write (same reasoning as
    /// `cache_put`).
    pub async fn compress_put(
        &self,
        tenant: &Uuid,
        hash: &str,
        content: &str,
        ttl_secs: u64,
    ) -> Result<()> {
        self.tenant_set_ex(COMPRESS_PREFIX, tenant, hash, content, ttl_secs)
            .await
    }

    /// [`compress_put`](Self::compress_put) for several originals in one round
    /// trip: a pipeline of `SET ... EX`. Nothing is written for an empty set or
    /// a `ttl_secs` of 0.
    pub async fn compress_put_many(
        &self,
        tenant: &Uuid,
        originals: &[(String, String)],
        ttl_secs: u64,
    ) -> Result<()> {
        self.set_many(
            originals.iter().map(|(hash, content)| {
                (
                    Self::tenant_key(COMPRESS_PREFIX, tenant, hash),
                    content.as_str(),
                )
            }),
            ttl_secs,
        )
        .await
    }

    /// `SET key value EX ttl_secs` for each pair, pipelined in one round
    /// trip. Nothing is written for no pairs or a `ttl_secs` of 0.
    async fn set_many<'a>(
        &self,
        pairs: impl Iterator<Item = (String, &'a str)>,
        ttl_secs: u64,
    ) -> Result<()> {
        if ttl_secs == 0 {
            return Ok(());
        }
        let mut pipe = redis::pipe();
        let mut any = false;
        for (key, value) in pairs {
            pipe.cmd("SET")
                .arg(key)
                .arg(value)
                .arg("EX")
                .arg(ttl_secs)
                .ignore();
            any = true;
        }
        if !any {
            return Ok(());
        }
        let mut conn = self.conn.clone();
        let _: () = pipe.query_async(&mut conn).await?;
        Ok(())
    }

    /// A tenant's kept compacted forms for `ids`, in order, `None` where none
    /// is kept; one round trip.
    pub async fn compacted_get_many(
        &self,
        tenant: &Uuid,
        ids: &[String],
    ) -> Result<Vec<Option<String>>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut cmd = redis::cmd("MGET");
        for id in ids {
            cmd.arg(Self::tenant_key(COMPACTED_PREFIX, tenant, id));
        }
        let mut conn = self.conn.clone();
        let found: Vec<Option<String>> = cmd.query_async(&mut conn).await?;
        Ok(found)
    }

    /// Keep compacted forms `(id, text)` for a tenant for `ttl_secs`, in one
    /// round trip; writing one again restarts its TTL.
    pub async fn compacted_put_many(
        &self,
        tenant: &Uuid,
        compacted: &[(String, String)],
        ttl_secs: u64,
    ) -> Result<()> {
        self.set_many(
            compacted.iter().map(|(id, text)| {
                (
                    Self::tenant_key(COMPACTED_PREFIX, tenant, id),
                    text.as_str(),
                )
            }),
            ttl_secs,
        )
        .await
    }

    /// Fetch a tenant's stashed original by its content hash, or `None` when
    /// it has expired, was never stored, or belongs to another tenant.
    pub async fn compress_get(&self, tenant: &Uuid, hash: &str) -> Result<Option<String>> {
        self.tenant_get(COMPRESS_PREFIX, tenant, hash).await
    }

    /// Fetch a tenant's stored description of an image by its hash, or `None`
    /// when it has expired, was never stored, or belongs to another tenant.
    pub async fn vision_get(&self, tenant: &Uuid, hash: &str) -> Result<Option<String>> {
        self.tenant_get(VISION_PREFIX, tenant, hash).await
    }

    /// Store a description of an image for a tenant, expiring after
    /// `ttl_secs`. A `ttl_secs` of 0 skips the write, as with `compress_put`.
    pub async fn vision_put(
        &self,
        tenant: &Uuid,
        hash: &str,
        text: &str,
        ttl_secs: u64,
    ) -> Result<()> {
        self.tenant_set_ex(VISION_PREFIX, tenant, hash, text, ttl_secs)
            .await
    }

    /// Fetch an `auto` conversation entry stored under `hash` for `tenant`.
    pub async fn auto_turn_get(&self, tenant: &Uuid, hash: &str) -> Result<Option<String>> {
        self.tenant_get(AUTO_TURN_PREFIX, tenant, hash).await
    }

    /// Store an `auto` conversation entry, expiring after `ttl_secs`. A
    /// `ttl_secs` of 0 skips the write, as with `compress_put`.
    pub async fn auto_turn_put(
        &self,
        tenant: &Uuid,
        hash: &str,
        value: &str,
        ttl_secs: u64,
    ) -> Result<()> {
        self.tenant_set_ex(AUTO_TURN_PREFIX, tenant, hash, value, ttl_secs)
            .await
    }

    fn crash_key(model: &str, fingerprint: &str) -> String {
        format!("{CRASH_PREFIX}{model}:{fingerprint}")
    }

    /// The crash-guard strikes recorded against a request fingerprint for
    /// `model` (0 when none, or expired).
    pub async fn crash_strikes(&self, model: &str, fingerprint: &str) -> Result<u32> {
        let mut conn = self.conn.clone();
        let v: Option<u32> = conn.hget(Self::crash_key(model, fingerprint), "n").await?;
        Ok(v.unwrap_or(0))
    }

    /// Add one crash-guard strike to a request fingerprint for `model`; a
    /// strike more than `window` after the previous one starts the count
    /// again. The record expires `ttl` after its last strike. Returns the
    /// count.
    pub async fn crash_strike(
        &self,
        model: &str,
        fingerprint: &str,
        window: std::time::Duration,
        ttl: std::time::Duration,
    ) -> Result<u32> {
        let mut conn = self.conn.clone();
        let n: u32 = crash_strike_script()
            .key(Self::crash_key(model, fingerprint))
            .arg(window.as_millis() as i64)
            .arg(ttl.as_secs() as i64)
            .invoke_async(&mut conn)
            .await?;
        // The model has a live strike record until `ttl` from now.
        let until = now_ms() + ttl.as_millis() as i64;
        let _: () = redis::pipe()
            .cmd("ZADD")
            .arg(CRASH_RECENT_KEY)
            .arg(until)
            .arg(model)
            .ignore()
            .query_async(&mut conn)
            .await?;
        Ok(n)
    }

    /// Models that may have live strike records, with how long until their
    /// newest one expires; one command. Expired members are dropped.
    pub async fn crash_recent_models(&self) -> Result<Vec<(String, std::time::Duration)>> {
        let now = now_ms();
        let mut conn = self.conn.clone();
        let (found,): (Vec<(String, i64)>,) = redis::pipe()
            .cmd("ZRANGEBYSCORE")
            .arg(CRASH_RECENT_KEY)
            .arg(now)
            .arg("+inf")
            .arg("WITHSCORES")
            .cmd("ZREMRANGEBYSCORE")
            .arg(CRASH_RECENT_KEY)
            .arg("-inf")
            .arg(format!("({now}"))
            .ignore()
            .query_async(&mut conn)
            .await?;
        Ok(found
            .into_iter()
            .map(|(m, until)| {
                (
                    m,
                    std::time::Duration::from_millis((until - now).max(0) as u64),
                )
            })
            .collect())
    }

    fn query_vector_key(hash: &str) -> String {
        format!("{KNOWLEDGE_QUERY_PREFIX}{hash}")
    }

    /// Fetch a cached query vector by its hash. A missing key, or a byte
    /// length that is not a multiple of 4 (a misaligned vector, which would
    /// score plausibly but wrongly), is `None`: the caller re-embeds.
    pub async fn get_query_vector(&self, hash: &str) -> Result<Option<Vec<f32>>> {
        let mut conn = self.conn.clone();
        let bytes: Option<Vec<u8>> = conn.get(Self::query_vector_key(hash)).await?;
        Ok(bytes
            .filter(|b| !b.is_empty() && b.len() % 4 == 0)
            .map(|b| {
                b.as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| f32::from_le_bytes(*c))
                    .collect()
            }))
    }

    /// Store a query vector under its hash, little-endian f32 encoded.
    ///
    /// As with `cache_put` and `compress_put`, `ttl_secs == 0` here means
    /// "caching disabled — skip the write entirely" rather than "store with no
    /// expiry": this key space is a digest of arbitrary user query text, so a
    /// never-expiring entry would be an unbounded memory path in the only
    /// hot-path store, which also serves key resolution and budget enforcement.
    /// A storage-boundary method should not depend on a caller in another
    /// crate validating this for it.
    pub async fn put_query_vector(&self, hash: &str, vector: &[f32], ttl_secs: u64) -> Result<()> {
        if ttl_secs == 0 {
            return Ok(());
        }
        let mut bytes = Vec::with_capacity(vector.len() * 4);
        for f in vector {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        let mut conn = self.conn.clone();
        let _: () = conn
            .set_ex(Self::query_vector_key(hash), bytes, ttl_secs)
            .await?;
        Ok(())
    }

    /// Combined admission check in a single round trip: an optional cumulative
    /// term-budget gate followed by the atomic token-bucket reserve. The term
    /// gate runs first, so a term-exhausted request never reserves per-minute
    /// tokens it has no completion path to refund.
    ///
    /// When admitted with a term gate, `requested` tokens and `est_cost` USD are
    /// reserved for the scope in `obleth:term_reserved:<scope>` (`tokens` /
    /// `cost`, 10-minute TTL set on creation, never refreshed) and count toward the cap until released by
    /// [`Self::term_reconcile`] with the same estimates. `scope` is the tenant
    /// or API-key id; the gate rejects when `committed + reserved >= cap`.
    ///
    /// Caller obligation: every `Reserved` outcome must eventually be settled
    /// with [`Self::term_reconcile`], or the reservation blocks budget headroom
    /// until the period rolls. In particular, after a successful key-scope
    /// reservation, if the tenant step then rejects or errors, or the request
    /// exits early for any reason before settling, release the key
    /// reservation with `term_reconcile(key, period, est, est_cost, 0, 0.0)`.
    pub async fn reserve_with_term(
        &self,
        scope: &Uuid,
        capacity: i64,
        tokens_per_minute: i64,
        requested: u32,
        term: Option<TermGate<'_>>,
        est_cost: f64,
    ) -> Result<ReserveOutcome> {
        let mut conn = self.conn.clone();
        let refill_per_ms = tokens_per_minute as f64 / 60_000.0;
        let (check_term, period_key, cap_tokens, cap_cost) = match &term {
            Some(gate) => (
                1u8,
                gate.period_key,
                gate.budget_tokens
                    .map(|t| t.to_string())
                    .unwrap_or_default(),
                gate.budget_cost_usd
                    .map(|c| c.to_string())
                    .unwrap_or_default(),
            ),
            None => (0u8, "", String::new(), String::new()),
        };
        let (status, remaining, term_tokens, term_cost): (i64, i64, i64, String) =
            reserve_with_term_script()
                .key(Self::budget_key(scope))
                .key(Self::term_usage_key(scope))
                .key(Self::term_reserved_key(scope))
                .arg(capacity)
                .arg(refill_per_ms)
                .arg(requested)
                .arg(check_term)
                .arg(period_key)
                .arg(cap_tokens)
                .arg(cap_cost)
                .arg(est_cost)
                .invoke_async(&mut conn)
                .await?;
        Ok(match status {
            1 => ReserveOutcome::Reserved { remaining },
            -1 => ReserveOutcome::TermExhausted {
                used_tokens: term_tokens,
                used_cost: term_cost.parse().unwrap_or(0.0),
            },
            _ => ReserveOutcome::RateLimited { remaining },
        })
    }

    /// Reconcile the difference between estimated and actual cost.
    pub async fn reconcile_budget(
        &self,
        tenant: &Uuid,
        capacity: i64,
        estimated: u32,
        actual: u32,
    ) -> Result<i64> {
        let mut conn = self.conn.clone();
        let delta = estimated as i64 - actual as i64;
        let remaining: i64 = reconcile_script()
            .key(Self::budget_key(tenant))
            .arg(capacity)
            .arg(delta)
            .invoke_async(&mut conn)
            .await?;
        Ok(remaining)
    }

    /// Settle a request admitted by [`Self::reserve_with_term`]: release the
    /// `est_*` reservation (floored at 0) and commit the `actual_*` usage to
    /// the scope's term counters, atomically. `scope` is the tenant or API-key
    /// id that was passed to `reserve_with_term`, and `period` is the period
    /// key used at admission.
    ///
    /// Returns the committed `(tokens, cost_usd)` after the reconcile (same
    /// shape as [`Self::term_usage_add`]), for budget alerting.
    pub async fn term_reconcile(
        &self,
        scope: &Uuid,
        period: &str,
        est_tokens: i64,
        est_cost: f64,
        actual_tokens: i64,
        actual_cost: f64,
    ) -> Result<(i64, f64)> {
        let mut conn = self.conn.clone();
        let (tokens, cost): (i64, String) = term_reconcile_script()
            .key(Self::term_usage_key(scope))
            .key(Self::term_reserved_key(scope))
            .arg(period)
            .arg(est_tokens)
            .arg(est_cost)
            .arg(actual_tokens)
            .arg(actual_cost)
            .invoke_async(&mut conn)
            .await?;
        Ok((tokens, cost.parse().unwrap_or(0.0)))
    }

    /// How long daily counters live: a little over a month of history.
    const DAILY_STATS_TTL_SECS: i64 = 40 * 86_400;

    fn daily_stats_key(kind: &str, id: &str, day: &str) -> String {
        format!("{DAILY_STATS_PREFIX}{kind}:{id}:{day}")
    }

    /// Count one call by `who` against `what` in the current minute, and say
    /// whether it is within `limit`: `(allowed, seconds until the minute ends)`.
    /// A fixed window: for calls that carry no tokens to meter, such as a call
    /// to an MCP server, this is what keeps one caller from flooding it.
    pub async fn count_call(&self, what: &str, who: &Uuid, limit: u32) -> Result<(bool, u64)> {
        let now = now_ms().max(0) as u64;
        let minute = now / 60_000;
        let key = format!("{CALL_COUNT_PREFIX}{what}:{who}:{minute}");
        let mut pipe = redis::pipe();
        pipe.cmd("INCR").arg(&key);
        pipe.cmd("EXPIRE").arg(&key).arg(120).ignore();
        let mut conn = self.conn.clone();
        let (count,): (u64,) = pipe.query_async(&mut conn).await?;
        let left = 60 - (now / 1000) % 60;
        Ok((count <= u64::from(limit), left))
    }

    /// Add to one thing's counters for `day` (`YYYY-MM-DD`, UTC), e.g. an MCP
    /// server's calls, failures and milliseconds. Counters expire after about
    /// 40 days. Best-effort: callers on the request path fire and forget.
    pub async fn bump_daily(
        &self,
        kind: &str,
        id: &str,
        day: &str,
        fields: &[(&str, i64)],
    ) -> Result<()> {
        if fields.is_empty() {
            return Ok(());
        }
        let key = Self::daily_stats_key(kind, id, day);
        let mut pipe = redis::pipe();
        for (field, by) in fields {
            pipe.cmd("HINCRBY").arg(&key).arg(*field).arg(*by).ignore();
        }
        pipe.cmd("EXPIRE")
            .arg(&key)
            .arg(Self::DAILY_STATS_TTL_SECS)
            .ignore();
        let mut conn = self.conn.clone();
        pipe.query_async::<()>(&mut conn).await?;
        Ok(())
    }

    /// Each id's counters on each of `days`, as `(day, field → value)`; days with
    /// nothing recorded are left out.
    pub async fn read_daily(
        &self,
        kind: &str,
        ids: &[String],
        days: &[String],
    ) -> Result<
        std::collections::HashMap<String, Vec<(String, std::collections::HashMap<String, i64>)>>,
    > {
        let mut out = std::collections::HashMap::new();
        if ids.is_empty() || days.is_empty() {
            return Ok(out);
        }
        let mut pipe = redis::pipe();
        for id in ids {
            for day in days {
                pipe.cmd("HGETALL")
                    .arg(Self::daily_stats_key(kind, id, day));
            }
        }
        let mut conn = self.conn.clone();
        let rows: Vec<std::collections::HashMap<String, i64>> = pipe.query_async(&mut conn).await?;
        let mut rows = rows.into_iter();
        for id in ids {
            let mut per_day = Vec::new();
            for day in days {
                let fields = rows.next().unwrap_or_default();
                if !fields.is_empty() {
                    per_day.push((day.clone(), fields));
                }
            }
            out.insert(id.clone(), per_day);
        }
        Ok(out)
    }

    /// Read a scope's (tenant or key id) committed term usage for `period_key`
    /// without touching it: counters stored for another period read as zero,
    /// since that is what the next request would find after rolling them.
    pub async fn term_usage_peek(&self, scope: &Uuid, period_key: &str) -> Result<(i64, f64)> {
        let mut conn = self.conn.clone();
        let (period, tokens, cost): (Option<String>, Option<i64>, Option<String>) =
            redis::cmd("HMGET")
                .arg(Self::term_usage_key(scope))
                .arg("period")
                .arg("tokens")
                .arg("cost")
                .query_async(&mut conn)
                .await?;
        if period.as_deref() != Some(period_key) {
            return Ok((0, 0.0));
        }
        Ok((
            tokens.unwrap_or(0),
            cost.and_then(|c| c.parse().ok()).unwrap_or(0.0),
        ))
    }

    /// Add observed `(tokens, cost_usd)` to a tenant's term counters (rolling the
    /// period first) and return the new cumulative `(tokens, cost_usd)`.
    pub async fn term_usage_add(
        &self,
        tenant: &Uuid,
        period_key: &str,
        add_tokens: i64,
        add_cost: f64,
    ) -> Result<(i64, f64)> {
        let mut conn = self.conn.clone();
        let (tokens, cost): (i64, String) = term_usage_add_script()
            .key(Self::term_usage_key(tenant))
            .arg(period_key)
            .arg(add_tokens)
            .arg(add_cost)
            .invoke_async(&mut conn)
            .await?;
        Ok((tokens, cost.parse().unwrap_or(0.0)))
    }

    /// Run `on_hash` for every invalidation message, forever. Reconnects with
    /// backoff when the connection drops, and calls `on_subscribed(reconnected)`
    /// after every successful subscribe: `false` for the first, `true` for
    /// each later one. Messages published while disconnected are lost, so a
    /// caller should treat `true` as "local caches may be stale".
    pub async fn run_invalidation_listener<F, S>(&self, on_hash: F, on_subscribed: S)
    where
        F: FnMut(String) + Send,
        S: FnMut(bool) + Send,
    {
        self.run_channel_listener(INVALIDATE_CHANNEL, "invalidation", on_hash, on_subscribed)
            .await
    }

    /// Run `on_pool` with the pool id of every shared-slot release wakeup on
    /// `keys.channel`, forever, with the same reconnect behaviour as
    /// [`RedisStore::run_invalidation_listener`]. Wakeups published while
    /// disconnected are lost, so a caller should treat `on_subscribed(true)`
    /// as "retry every pool".
    pub async fn run_slot_release_listener<F, S>(
        &self,
        keys: &SlotKeys,
        on_pool: F,
        on_subscribed: S,
    ) where
        F: FnMut(String) + Send,
        S: FnMut(bool) + Send,
    {
        self.run_channel_listener(&keys.channel, "slot release", on_pool, on_subscribed)
            .await
    }

    async fn run_channel_listener<F, S>(
        &self,
        channel: &str,
        what: &'static str,
        mut on_message: F,
        mut on_subscribed: S,
    ) where
        F: FnMut(String) + Send,
        S: FnMut(bool) + Send,
    {
        let mut subscribed_before = false;
        let mut backoff = PUBSUB_BACKOFF_MIN;
        loop {
            let mut subscribed = false;
            let result = self
                .listen_once(channel, &mut on_message, &mut || {
                    subscribed = true;
                    on_subscribed(subscribed_before);
                    subscribed_before = true;
                })
                .await;
            // A session that got as far as subscribing was healthy, so the
            // next retry starts from the short delay again.
            if subscribed {
                backoff = PUBSUB_BACKOFF_MIN;
            }
            match result {
                Ok(()) => {
                    tracing::warn!(listener = what, retry_in = ?backoff, "pub/sub listener connection closed")
                }
                Err(e) => {
                    tracing::warn!(listener = what, error = %e, retry_in = ?backoff, "pub/sub listener stopped")
                }
            }
            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(PUBSUB_BACKOFF_MAX);
        }
    }

    /// One pub/sub session: `Ok` when the server closed the connection.
    async fn listen_once<F, S>(
        &self,
        channel: &str,
        on_message: &mut F,
        on_subscribed: &mut S,
    ) -> Result<()>
    where
        F: FnMut(String) + Send,
        S: FnMut() + Send,
    {
        let timed_out = |what: &'static str| {
            RedisError::Redis(redis::RedisError::from((redis::ErrorKind::IoError, what)))
        };
        let pubsub = tokio::time::timeout(PUBSUB_CONNECT_TIMEOUT, self.client.get_async_pubsub())
            .await
            .map_err(|_| timed_out("pub/sub connect timed out"))??;
        let (mut sink, mut stream) = pubsub.split();
        tokio::time::timeout(PUBSUB_REPLY_TIMEOUT, sink.subscribe(channel))
            .await
            .map_err(|_| timed_out("pub/sub subscribe timed out"))??;
        on_subscribed();
        let mut keepalive = tokio::time::interval_at(
            tokio::time::Instant::now() + PUBSUB_KEEPALIVE,
            PUBSUB_KEEPALIVE,
        );
        keepalive.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                msg = stream.next() => {
                    let Some(msg) = msg else { return Ok(()) };
                    if let Ok(payload) = msg.get_payload::<String>() {
                        on_message(payload);
                    }
                }
                _ = keepalive.tick() => {
                    // A half-open socket delivers nothing and reports no
                    // error. Re-subscribing to a channel already joined still
                    // gets a reply, so it serves as the PING that the
                    // subscribed-mode sink doesn't expose.
                    tokio::time::timeout(PUBSUB_REPLY_TIMEOUT, sink.subscribe(channel))
                        .await
                        .map_err(|_| timed_out("pub/sub keepalive timed out"))??;
                }
            }
        }
    }
}

const PUBSUB_KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(30);
const PUBSUB_REPLY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
const PUBSUB_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const PUBSUB_BACKOFF_MIN: std::time::Duration = std::time::Duration::from_secs(1);
const PUBSUB_BACKOFF_MAX: std::time::Duration = std::time::Duration::from_secs(30);

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod test_util;
#[cfg(test)]
mod tests;
