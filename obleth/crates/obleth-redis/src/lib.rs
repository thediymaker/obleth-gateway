//! Redis layer: hot read-cache of resolved keys + live token-bucket counters.
//!
//! The data plane reads *only* Redis on the hot path (Postgres is the durable
//! source of truth written by the Management API, then synced here). Budget
//! checks run as atomic Lua so they are correct across many gateway pods.

mod prune;
pub mod scripts;
mod slots;

pub use slots::{
    RedisSlots, SlotKeys, REPLICAS_KEY, SLOT_HELD_PREFIX, SLOT_RELEASE_CHANNEL, SLOT_TOTALS_KEY,
};

use std::sync::OnceLock;

use futures_util::StreamExt;
pub use obleth_config::config::RedisTimeouts;
use obleth_config::{CachedResponse, ResolvedKey, ResolvedMcpServer, ResolvedModel};
use redis::aio::{ConnectionManager, ConnectionManagerConfig};
use redis::AsyncCommands;
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

cached_script!(reserve_script, scripts::RESERVE);
cached_script!(reserve_with_term_script, scripts::RESERVE_WITH_TERM);
cached_script!(reconcile_script, scripts::RECONCILE);
cached_script!(term_usage_read_script, scripts::TERM_USAGE_READ);
cached_script!(term_usage_add_script, scripts::TERM_USAGE_ADD);
cached_script!(term_reconcile_script, scripts::TERM_RECONCILE);
cached_script!(replica_heartbeat_script, scripts::REPLICA_HEARTBEAT);
cached_script!(replica_deregister_script, scripts::REPLICA_DEREGISTER);
cached_script!(slot_acquire_script, scripts::SLOT_ACQUIRE);
cached_script!(slot_release_script, scripts::SLOT_RELEASE);
cached_script!(slot_reconcile_script, scripts::SLOT_RECONCILE);

const KEY_PREFIX: &str = "obleth:key:";
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
/// Namespace for cached query vectors (knowledge boon). The key itself is
/// built here from the hash the proxy supplies (see `query_vector_key`),
/// mirroring `compress_key`.
const KNOWLEDGE_QUERY_PREFIX: &str = "obleth:knowledge:q:";
const INVALIDATE_CHANNEL: &str = "obleth:invalidate";
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
    fn term_reserved_key(scope: impl std::fmt::Display) -> String {
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

    /// Drop this replica's heartbeat, and free any shared slot it still
    /// holds, so the survivors grow at their next heartbeat instead of one
    /// TTL later. Called on clean shutdown.
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
            .arg(instance)
            .arg(ttl.as_millis().max(1) as u64)
            .arg(&keys.held_prefix)
            .arg(&keys.channel)
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
            .arg(instance)
            .arg(&keys.channel)
            .invoke_async(&mut conn)
            .await?;
        Ok(())
    }

    /// Hot-path lookup of a resolved key by its hash.
    pub async fn get_resolved_key(&self, hash: &str) -> Result<Option<ResolvedKey>> {
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(Self::key_cache(hash)).await?;
        match raw {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    /// Write/refresh the cached resolved key (called by the Management API sync).
    pub async fn put_resolved_key(&self, hash: &str, key: &ResolvedKey) -> Result<()> {
        let mut conn = self.conn.clone();
        let json = serde_json::to_string(key)?;
        let _: () = conn.set(Self::key_cache(hash), json).await?;
        Ok(())
    }

    pub async fn delete_resolved_key(&self, hash: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = conn.del(Self::key_cache(hash)).await?;
        Ok(())
    }

    fn model_cache(name: &str) -> String {
        format!("{MODEL_PREFIX}{name}")
    }

    pub async fn get_resolved_model(&self, name: &str) -> Result<Option<ResolvedModel>> {
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(Self::model_cache(name)).await?;
        match raw {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    pub async fn put_resolved_model(&self, name: &str, model: &ResolvedModel) -> Result<()> {
        let mut conn = self.conn.clone();
        let json = serde_json::to_string(model)?;
        let _: () = conn.set(Self::model_cache(name), json).await?;
        Ok(())
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
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(Self::mcp_cache(name)).await?;
        match raw {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
    }

    pub async fn put_resolved_mcp_server(
        &self,
        name: &str,
        server: &ResolvedMcpServer,
    ) -> Result<()> {
        let mut conn = self.conn.clone();
        let json = serde_json::to_string(server)?;
        let _: () = conn.set(Self::mcp_cache(name), json).await?;
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
        let mut conn = self.conn.clone();
        let raw: Option<String> = conn.get(Self::response_cache_key(key)).await?;
        match raw {
            Some(json) => Ok(Some(serde_json::from_str(&json)?)),
            None => Ok(None),
        }
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

    fn compress_key(tenant: &Uuid, hash: &str) -> String {
        format!("{COMPRESS_PREFIX}{tenant}:{hash}")
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
        if ttl_secs == 0 {
            return Ok(());
        }
        let mut conn = self.conn.clone();
        let _: () = conn
            .set_ex(Self::compress_key(tenant, hash), content, ttl_secs)
            .await?;
        Ok(())
    }

    /// Fetch a tenant's stashed original by its content hash, or `None` when
    /// it has expired, was never stored, or belongs to another tenant.
    pub async fn compress_get(&self, tenant: &Uuid, hash: &str) -> Result<Option<String>> {
        let mut conn = self.conn.clone();
        let v: Option<String> = conn.get(Self::compress_key(tenant, hash)).await?;
        Ok(v)
    }

    fn query_vector_key(hash: &str) -> String {
        format!("{KNOWLEDGE_QUERY_PREFIX}{hash}")
    }

    /// Fetch a cached query vector by its hash. Any error, a missing key, or a
    /// byte length that is not a multiple of 4 (a misaligned vector, which
    /// would score plausibly but wrongly) is treated as a plain miss: the
    /// caller re-embeds. The cache is an optimization, never a correctness
    /// requirement.
    pub async fn get_query_vector(&self, hash: &str) -> Option<Vec<f32>> {
        let mut conn = self.conn.clone();
        let bytes: Option<Vec<u8>> = conn.get(Self::query_vector_key(hash)).await.ok()?;
        let bytes = bytes?;
        if bytes.is_empty() || bytes.len() % 4 != 0 {
            return None;
        }
        Some(
            bytes
                .chunks_exact(4)
                .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                .collect(),
        )
    }

    /// Store a query vector under its hash, little-endian f32 encoded.
    /// Failures are ignored — the cache is an optimization, never a
    /// correctness requirement.
    ///
    /// As with `cache_put` and `compress_put`, `ttl_secs == 0` here means
    /// "caching disabled — skip the write entirely" rather than "store with no
    /// expiry": this key space is a digest of arbitrary user query text, so a
    /// never-expiring entry would be an unbounded memory path in the only
    /// hot-path store, which also serves key resolution and budget enforcement.
    /// A storage-boundary method should not depend on a caller in another
    /// crate validating this for it.
    pub async fn put_query_vector(&self, hash: &str, vector: &[f32], ttl_secs: u64) {
        if ttl_secs == 0 {
            return;
        }
        let mut bytes = Vec::with_capacity(vector.len() * 4);
        for f in vector {
            bytes.extend_from_slice(&f.to_le_bytes());
        }
        let mut conn = self.conn.clone();
        let redis_key = Self::query_vector_key(hash);
        let _: redis::RedisResult<()> = conn.set_ex(redis_key, bytes, ttl_secs).await;
    }

    /// Publish invalidation for a key hash, model name (`model:<name>`), or `*`.
    pub async fn publish_invalidation(&self, target: &str) -> Result<()> {
        let mut conn = self.conn.clone();
        let _: () = conn.publish(INVALIDATE_CHANNEL, target).await?;
        Ok(())
    }

    /// Atomically reserve `requested` tokens. Returns `(allowed, remaining)`.
    pub async fn reserve_budget(
        &self,
        tenant: &Uuid,
        capacity: i64,
        tokens_per_minute: i64,
        requested: u32,
    ) -> Result<(bool, i64)> {
        let mut conn = self.conn.clone();
        let now_ms = now_ms();
        let refill_per_ms = tokens_per_minute as f64 / 60_000.0;
        let (allowed, remaining): (i64, i64) = reserve_script()
            .key(Self::budget_key(tenant))
            .arg(capacity)
            .arg(refill_per_ms)
            .arg(now_ms)
            .arg(requested)
            .invoke_async(&mut conn)
            .await?;
        Ok((allowed == 1, remaining))
    }

    /// Check-only variant of [`Self::reserve_with_term`]: gates on the term
    /// budget but reserves nothing against it. Kept for callers that settle
    /// with [`Self::term_usage_add`]; a reservation made here would never be
    /// released.
    pub async fn reserve_budget_with_term(
        &self,
        tenant: &Uuid,
        capacity: i64,
        tokens_per_minute: i64,
        requested: u32,
        term: Option<TermGate<'_>>,
    ) -> Result<ReserveOutcome> {
        self.reserve_inner(tenant, capacity, tokens_per_minute, requested, term, None)
            .await
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
        self.reserve_inner(
            scope,
            capacity,
            tokens_per_minute,
            requested,
            term,
            Some(est_cost),
        )
        .await
    }

    async fn reserve_inner(
        &self,
        tenant: &Uuid,
        capacity: i64,
        tokens_per_minute: i64,
        requested: u32,
        term: Option<TermGate<'_>>,
        reserve_cost: Option<f64>,
    ) -> Result<ReserveOutcome> {
        let mut conn = self.conn.clone();
        let now_ms = now_ms();
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
                .key(Self::budget_key(tenant))
                .key(Self::term_usage_key(tenant))
                .key(Self::term_reserved_key(tenant))
                .arg(capacity)
                .arg(refill_per_ms)
                .arg(now_ms)
                .arg(requested)
                .arg(check_term)
                .arg(period_key)
                .arg(cap_tokens)
                .arg(cap_cost)
                .arg(u8::from(reserve_cost.is_some()))
                .arg(reserve_cost.unwrap_or(0.0))
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
            .arg(now_ms())
            .invoke_async(&mut conn)
            .await?;
        Ok(remaining)
    }

    /// Settle a request admitted by [`Self::reserve_with_term`]: release the
    /// `est_*` reservation (floored at 0) and commit the `actual_*` usage to
    /// the scope's term counters, atomically. `scope` is the tenant or API-key
    /// id (as a string) that was passed to `reserve_with_term`, and `period`
    /// is the period key used at admission.
    ///
    /// Returns the committed `(tokens, cost_usd)` after the reconcile (same
    /// shape as [`Self::term_usage_add`]), for budget alerting.
    pub async fn term_reconcile(
        &self,
        scope: &str,
        period: &str,
        est_tokens: i64,
        est_cost: f64,
        actual_tokens: i64,
        actual_cost: f64,
    ) -> Result<(i64, f64)> {
        let mut conn = self.conn.clone();
        let (tokens, cost): (i64, String) = term_reconcile_script()
            .key(format!("{TERM_USAGE_PREFIX}{scope}"))
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

    /// Read a tenant's cumulative term usage `(tokens, cost_usd)`, rolling the
    /// period counters if `period_key` no longer matches the stored term.
    pub async fn term_usage_read(&self, tenant: &Uuid, period_key: &str) -> Result<(i64, f64)> {
        let mut conn = self.conn.clone();
        let (tokens, cost): (i64, String) = term_usage_read_script()
            .key(Self::term_usage_key(tenant))
            .arg(period_key)
            .invoke_async(&mut conn)
            .await?;
        Ok((tokens, cost.parse().unwrap_or(0.0)))
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
mod tests {
    use super::*;
    use obleth_config::ResolvedKey;

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn budget_reserve_and_reconcile() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        let tenant = Uuid::new_v4();
        let capacity = 100i64;
        let tpm = 0i64; // no refill, so accounting is deterministic

        // first reserve of 60 succeeds, leaving 40
        let (ok, remaining) = store
            .reserve_budget(&tenant, capacity, tpm, 60)
            .await
            .unwrap();
        assert!(ok);
        assert_eq!(remaining, 40);

        // 50 more exceeds the remaining 40 -> denied
        let (ok, _) = store
            .reserve_budget(&tenant, capacity, tpm, 50)
            .await
            .unwrap();
        assert!(!ok);

        // reconcile: estimated 60 but actual 10 -> refund 50, back to 90
        let after = store
            .reconcile_budget(&tenant, capacity, 60, 10)
            .await
            .unwrap();
        assert_eq!(after, 90);
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn combined_reserve_gates_on_term_budget_first() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        let tenant = Uuid::new_v4();
        let capacity = 100i64;
        let tpm = 0i64; // no refill, so accounting is deterministic

        // No term gate: behaves like a plain reserve.
        let out = store
            .reserve_budget_with_term(&tenant, capacity, tpm, 60, None)
            .await
            .unwrap();
        assert_eq!(out, ReserveOutcome::Reserved { remaining: 40 });

        // Bucket exhausted -> RateLimited, nothing reserved.
        let out = store
            .reserve_budget_with_term(&tenant, capacity, tpm, 50, None)
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::RateLimited { .. }));

        // Accumulate term usage, then a gate below that usage must reject
        // WITHOUT touching the bucket.
        store
            .term_usage_add(&tenant, "l:0", 500, 1.25)
            .await
            .unwrap();
        let gate = TermGate {
            period_key: "l:0",
            budget_tokens: Some(500),
            budget_cost_usd: None,
        };
        let out = store
            .reserve_budget_with_term(&tenant, capacity, tpm, 10, Some(gate))
            .await
            .unwrap();
        assert_eq!(
            out,
            ReserveOutcome::TermExhausted {
                used_tokens: 500,
                used_cost: 1.25
            }
        );
        // Bucket untouched by the term rejection: 40 tokens still reservable.
        let out = store
            .reserve_budget_with_term(&tenant, capacity, tpm, 40, None)
            .await
            .unwrap();
        assert_eq!(out, ReserveOutcome::Reserved { remaining: 0 });

        // A roomy term gate passes through to the bucket (now empty).
        let gate = TermGate {
            period_key: "l:0",
            budget_tokens: Some(1_000_000),
            budget_cost_usd: Some(100.0),
        };
        let out = store
            .reserve_budget_with_term(&tenant, capacity, tpm, 10, Some(gate))
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::RateLimited { .. }));

        // A different period key rolls the counters: gate opens again.
        let gate = TermGate {
            period_key: "m:2026-06",
            budget_tokens: Some(500),
            budget_cost_usd: None,
        };
        let out = store
            .reserve_budget_with_term(&Uuid::new_v4(), capacity, tpm, 10, Some(gate))
            .await
            .unwrap();
        assert_eq!(out, ReserveOutcome::Reserved { remaining: 90 });
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn zero_capacity_means_no_per_minute_token_cap() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        let tenant = Uuid::new_v4();

        let out = store
            .reserve_budget_with_term(&tenant, 0, 0, 1_000_000, None)
            .await
            .unwrap();
        assert_eq!(out, ReserveOutcome::Reserved { remaining: 0 });

        store
            .term_usage_add(&tenant, "l:0", 500, 1.25)
            .await
            .unwrap();
        let gate = TermGate {
            period_key: "l:0",
            budget_tokens: Some(500),
            budget_cost_usd: None,
        };
        let out = store
            .reserve_budget_with_term(&tenant, 0, 0, 10, Some(gate))
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::TermExhausted { .. }));
    }

    #[tokio::test]
    async fn resolved_key_cache_roundtrip() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        let hash = format!("test-{}", Uuid::new_v4());
        let key = ResolvedKey {
            key_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            tenant_name: "t".into(),
            fairshare_group: "default".into(),
            group_weight: 100,
            weight: 7,
            tokens_per_minute: 1000,
            max_in_flight: None,
            disabled: false,
            status: "active".into(),
            timezone: "UTC".into(),
            active_from: None,
            active_until: None,
            weekly_windows: None,
            budget_tokens: None,
            budget_cost_usd: None,
            budget_period: None,
            budget_started_at: None,
            key_budget_tokens: None,
            key_budget_cost_usd: None,
            key_budget_period: None,
            key_budget_started_at: None,
            key_weight: 100,
            key_max_in_flight: None,
            allowed_models: None,
            internal: false,
            tracing_enabled: false,
            guardrails_policy: None,
            compression_policy: None,
            synthetic: false,
        };
        store.put_resolved_key(&hash, &key).await.unwrap();
        let got = store.get_resolved_key(&hash).await.unwrap().unwrap();
        assert_eq!(got, key);
        store.delete_resolved_key(&hash).await.unwrap();
        assert!(store.get_resolved_key(&hash).await.unwrap().is_none());
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn compress_store_roundtrip_and_miss() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        let tenant = Uuid::new_v4();
        let hash = format!("h-{}", Uuid::new_v4());

        // Miss before write.
        assert!(store.compress_get(&tenant, &hash).await.unwrap().is_none());

        // Round-trip with a TTL.
        store
            .compress_put(&tenant, &hash, "the original content", 60)
            .await
            .unwrap();
        assert_eq!(
            store.compress_get(&tenant, &hash).await.unwrap().as_deref(),
            Some("the original content")
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn compress_store_is_scoped_to_the_tenant() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        let (owner, other) = (Uuid::new_v4(), Uuid::new_v4());
        let hash = format!("h-{}", Uuid::new_v4());
        store
            .compress_put(&owner, &hash, "tenant secret", 60)
            .await
            .unwrap();
        assert!(
            store.compress_get(&other, &hash).await.unwrap().is_none(),
            "another tenant must not retrieve an original by its hash"
        );
        let mut conn = store.conn.clone();
        let exists: bool = conn
            .exists(format!("{COMPRESS_PREFIX}{owner}:{hash}"))
            .await
            .unwrap();
        assert!(exists, "stored under obleth:compress:<tenant>:<sha>");
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn zero_ttl_never_writes() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        let tenant = Uuid::new_v4();
        let hash = format!("h-{}", Uuid::new_v4());
        store
            .compress_put(&tenant, &hash, "content", 0)
            .await
            .unwrap();
        assert!(store.compress_get(&tenant, &hash).await.unwrap().is_none());

        let key = format!("k-{}", Uuid::new_v4());
        let value = CachedResponse {
            status: 200,
            content_type: "application/json".into(),
            body: "{}".into(),
            input_tokens: 1,
            output_tokens: 1,
        };
        store.cache_put(&key, &value, 0).await.unwrap();
        assert!(store.cache_get(&key).await.unwrap().is_none());

        // A positive TTL still stores, and the entry expires.
        store.cache_put(&key, &value, 60).await.unwrap();
        assert!(store.cache_get(&key).await.unwrap().is_some());
        let mut conn = store.conn.clone();
        let ttl: i64 = conn.ttl(format!("{CACHE_PREFIX}{key}")).await.unwrap();
        assert!(ttl > 0 && ttl <= 60, "entry must expire, got ttl {ttl}");
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn query_vector_roundtrip_and_miss() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        let hash = format!("q-{}", Uuid::new_v4());

        // Miss before write.
        assert!(store.get_query_vector(&hash).await.is_none());

        let vector = vec![0.5f32, -1.25, 3.0, 0.0];
        store.put_query_vector(&hash, &vector, 60).await;
        assert_eq!(store.get_query_vector(&hash).await, Some(vector));
    }

    /// A misaligned byte length must be treated as a miss, not a decode
    /// error: a stray truncated write must never score a request against a
    /// silently shifted vector.
    #[tokio::test]
    async fn query_vector_misaligned_bytes_is_a_miss() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        let hash = format!("q-bad-{}", Uuid::new_v4());
        let redis_key = RedisStore::query_vector_key(&hash);
        let mut conn = store.conn.clone();
        let _: () = conn.set(redis_key, vec![1u8, 2, 3]).await.unwrap();

        assert!(store.get_query_vector(&hash).await.is_none());
    }

    async fn test_store() -> Option<RedisStore> {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return None;
        };
        Some(RedisStore::connect(&url).await.expect("connect"))
    }

    async fn term_field(store: &RedisStore, scope: &Uuid, field: &str) -> Option<String> {
        let mut conn = store.conn.clone();
        conn.hget(RedisStore::term_usage_key(scope), field)
            .await
            .unwrap()
    }

    async fn reserved_field(store: &RedisStore, scope: &Uuid, field: &str) -> Option<String> {
        let mut conn = store.conn.clone();
        conn.hget(RedisStore::term_reserved_key(scope), field)
            .await
            .unwrap()
    }

    /// Stand-in for the 10-minute TTL running out on a never-settled request.
    async fn expire_reservations_now(store: &RedisStore, scope: &Uuid) {
        let mut conn = store.conn.clone();
        let _: i64 = redis::cmd("PEXPIRE")
            .arg(RedisStore::term_reserved_key(scope))
            .arg(1)
            .query_async(&mut conn)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn reservation_ttl_is_set_on_creation_and_never_refreshed() {
        let Some(store) = test_store().await else {
            return;
        };
        let scope = Uuid::new_v4();
        let rkey = RedisStore::term_reserved_key(scope);
        store
            .reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.0)
            .await
            .unwrap();
        let mut conn = store.conn.clone();
        let ttl: i64 = conn.pttl(&rkey).await.unwrap();
        assert!(ttl > 590_000 && ttl <= 600_000, "PTTL={ttl}");

        // Stand-in for ~5 minutes passing (t0 + delta) without sleeping.
        let _: i64 = redis::cmd("PEXPIRE")
            .arg(&rkey)
            .arg(300_000)
            .query_async(&mut conn)
            .await
            .unwrap();
        // A later reserve in the same (busy) scope must not re-arm the TTL,
        // or a leaked reservation would never expire.
        store
            .reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.0)
            .await
            .unwrap();
        let ttl: i64 = conn.pttl(&rkey).await.unwrap();
        assert!(ttl > 0 && ttl <= 300_000, "PTTL={ttl}");
        assert_eq!(
            reserved_field(&store, &scope, "tokens").await.as_deref(),
            Some("20")
        );
        // Committed usage has no TTL at all, independent of reservations: it
        // is budget state, and a TTL would make it evictable under
        // volatile-lru.
        let ttl: i64 = redis::cmd("PTTL")
            .arg(RedisStore::term_usage_key(&scope))
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(ttl, -1, "committed term usage must not expire");
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn committed_term_usage_never_expires_and_drops_a_legacy_ttl() {
        let Some(store) = test_store().await else {
            return;
        };
        let scope = Uuid::new_v4();
        let key = RedisStore::term_usage_key(&scope);
        let mut conn = store.conn.clone();

        store.term_usage_add(&scope, "l:0", 10, 0.5).await.unwrap();
        let ttl: i64 = redis::cmd("PTTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(ttl, -1, "add must leave committed usage without a TTL");

        // A key written by an earlier version carries a 1-year TTL; the next
        // write must clear it, not refresh it.
        let _: i64 = redis::cmd("PEXPIRE")
            .arg(&key)
            .arg(31_536_000_000_i64)
            .query_async(&mut conn)
            .await
            .unwrap();
        store.term_usage_add(&scope, "l:0", 5, 0.0).await.unwrap();
        let ttl: i64 = redis::cmd("PTTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(ttl, -1, "a legacy TTL must be cleared on the next add");
        assert_eq!(store.term_usage_read(&scope, "l:0").await.unwrap().0, 15);

        // A scope that is only ever read (no new usage) is cleared too.
        let _: i64 = redis::cmd("PEXPIRE")
            .arg(&key)
            .arg(31_536_000_000_i64)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(store.term_usage_read(&scope, "l:0").await.unwrap().0, 15);
        let ttl: i64 = redis::cmd("PTTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert_eq!(ttl, -1, "a legacy TTL must be cleared on read");

        let _: () = conn.del(&key).await.unwrap();
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn leaked_reservation_expires_and_stops_blocking_the_scope() {
        let Some(store) = test_store().await else {
            return;
        };
        let scope = Uuid::new_v4();
        store.term_usage_add(&scope, "l:0", 50, 0.0).await.unwrap();
        // Reserve 50 and never settle (crash / SIGKILL): the scope is at cap.
        store
            .reserve_with_term(&scope, 0, 0, 50, Some(token_gate("l:0", 100)), 0.0)
            .await
            .unwrap();
        let out = store
            .reserve_with_term(&scope, 0, 0, 1, Some(token_gate("l:0", 100)), 0.0)
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::TermExhausted { .. }));

        expire_reservations_now(&store, &scope).await;

        // Reserved reads as 0 again; only the committed 50 counts.
        let out = store
            .reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.0)
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::Reserved { .. }));
        assert_eq!(
            reserved_field(&store, &scope, "tokens").await.as_deref(),
            Some("10")
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn reconcile_after_reservation_expiry_still_commits_actual() {
        let Some(store) = test_store().await else {
            return;
        };
        let scope = Uuid::new_v4();
        store
            .reserve_with_term(&scope, 0, 0, 40, Some(token_gate("l:0", 100)), 0.4)
            .await
            .unwrap();
        expire_reservations_now(&store, &scope).await;

        let committed = store
            .term_reconcile(&scope.to_string(), "l:0", 40, 0.4, 25, 0.3)
            .await
            .unwrap();
        assert_eq!(committed, (25, 0.3));
        // The release did not resurrect the expired key.
        assert_eq!(reserved_field(&store, &scope, "tokens").await, None);
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn reservation_from_a_previous_period_is_discarded() {
        let Some(store) = test_store().await else {
            return;
        };
        let scope = Uuid::new_v4();
        store
            .reserve_with_term(&scope, 0, 0, 90, Some(token_gate("m:2026-08", 100)), 0.0)
            .await
            .unwrap();
        let out = store
            .reserve_with_term(&scope, 0, 0, 5, Some(token_gate("m:2026-09", 100)), 0.0)
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::Reserved { .. }));
        assert_eq!(
            reserved_field(&store, &scope, "tokens").await.as_deref(),
            Some("5")
        );
    }

    fn token_gate(period_key: &str, cap: i64) -> TermGate<'_> {
        TermGate {
            period_key,
            budget_tokens: Some(cap),
            budget_cost_usd: None,
        }
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set (the
    /// target address needs no Redis, but the test needs a routable network).
    #[tokio::test]
    async fn connect_to_blackholed_address_errors_within_connect_timeout() {
        if std::env::var("OBLETH_TEST_REDIS_URL").is_err() {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        }
        let timeouts = RedisTimeouts {
            response: std::time::Duration::from_millis(250),
            connect: std::time::Duration::from_secs(2),
        };
        let started = std::time::Instant::now();
        let res = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            RedisStore::connect_with("redis://10.255.255.1:6379", timeouts),
        )
        .await
        .expect("connect must not hang past the connect timeout");
        let elapsed = started.elapsed();
        assert!(res.is_err(), "blackholed connect must error");
        assert!(
            elapsed < std::time::Duration::from_millis(2500),
            "took {elapsed:?}"
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn response_timeout_is_applied_to_commands() {
        let Some(store) = test_store().await else {
            return;
        };
        // BLPOP on an empty list stalls the reply for 2 s, standing in for a
        // server that stops answering after the connection is established.
        let mut conn = store.conn.clone();
        let started = std::time::Instant::now();
        let res: redis::RedisResult<Option<(String, String)>> = redis::cmd("BLPOP")
            .arg(format!("obleth:test:blpop:{}", Uuid::new_v4()))
            .arg(2)
            .query_async(&mut conn)
            .await;
        let elapsed = started.elapsed();
        assert!(res.is_err(), "stalled command must time out, got {res:?}");
        assert!(
            elapsed < std::time::Duration::from_millis(1500),
            "took {elapsed:?}"
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn term_reservation_counts_toward_the_cap() {
        let Some(store) = test_store().await else {
            return;
        };
        let scope = Uuid::new_v4();
        store.term_usage_add(&scope, "l:0", 50, 0.0).await.unwrap();

        // 50 committed + 0 reserved < 100: admitted, reserves 40.
        let out = store
            .reserve_with_term(&scope, 0, 0, 40, Some(token_gate("l:0", 100)), 0.4)
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::Reserved { .. }));
        assert_eq!(
            reserved_field(&store, &scope, "tokens").await.as_deref(),
            Some("40")
        );
        // 50 + 40 < 100: admitted, reserves 10 more.
        let out = store
            .reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.1)
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::Reserved { .. }));
        // 50 + 50 >= 100: rejected although only 50 is committed.
        let out = store
            .reserve_with_term(&scope, 0, 0, 1, Some(token_gate("l:0", 100)), 0.0)
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::TermExhausted { .. }));
        let reserved_cost: f64 = reserved_field(&store, &scope, "cost")
            .await
            .unwrap()
            .parse()
            .unwrap();
        assert!((reserved_cost - 0.5).abs() < 1e-9, "{reserved_cost}");

        // Reserved cost counts toward a cost cap the same way.
        let cost_scope = Uuid::new_v4();
        let gate = || TermGate {
            period_key: "l:0",
            budget_tokens: None,
            budget_cost_usd: Some(1.0),
        };
        for expect_ok in [true, true, false] {
            let out = store
                .reserve_with_term(&cost_scope, 0, 0, 1, Some(gate()), 0.75)
                .await
                .unwrap();
            assert_eq!(
                matches!(out, ReserveOutcome::Reserved { .. }),
                expect_ok,
                "{out:?}"
            );
        }
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn rate_limited_requests_reserve_no_term_budget() {
        let Some(store) = test_store().await else {
            return;
        };
        let scope = Uuid::new_v4();
        let out = store
            .reserve_with_term(&scope, 10, 0, 5, Some(token_gate("l:0", 1000)), 0.0)
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::Reserved { .. }));
        // 8 does not fit the 5 left in the bucket.
        let out = store
            .reserve_with_term(&scope, 10, 0, 8, Some(token_gate("l:0", 1000)), 0.0)
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::RateLimited { .. }));
        assert_eq!(
            reserved_field(&store, &scope, "tokens").await.as_deref(),
            Some("5")
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn legacy_check_only_reserve_leaves_no_reservation() {
        let Some(store) = test_store().await else {
            return;
        };
        let scope = Uuid::new_v4();
        let out = store
            .reserve_budget_with_term(&scope, 0, 0, 40, Some(token_gate("l:0", 100)))
            .await
            .unwrap();
        assert!(matches!(out, ReserveOutcome::Reserved { .. }));
        assert_eq!(reserved_field(&store, &scope, "tokens").await, None);
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn term_reconcile_moves_reservation_to_committed() {
        let Some(store) = test_store().await else {
            return;
        };
        let scope = Uuid::new_v4();
        let s = scope.to_string();
        store
            .reserve_with_term(&scope, 0, 0, 40, Some(token_gate("l:0", 100)), 0.4)
            .await
            .unwrap();
        let committed = store
            .term_reconcile(&s, "l:0", 40, 0.4, 25, 0.3)
            .await
            .unwrap();
        assert_eq!(committed, (25, 0.3));
        assert_eq!(
            store.term_usage_read(&scope, "l:0").await.unwrap(),
            committed
        );
        assert_eq!(
            reserved_field(&store, &scope, "tokens").await.as_deref(),
            Some("0")
        );
        let reserved_cost: f64 = reserved_field(&store, &scope, "cost")
            .await
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(reserved_cost, 0.0);

        // Releasing more than is reserved floors at zero instead of going
        // negative (a negative reservation would inflate headroom).
        let (tokens, cost) = store
            .term_reconcile(&s, "l:0", 1_000, 5.0, 5, 0.05)
            .await
            .unwrap();
        assert_eq!(tokens, 30);
        assert!((cost - 0.35).abs() < 1e-9, "{cost}");
        assert_eq!(
            reserved_field(&store, &scope, "tokens").await.as_deref(),
            Some("0")
        );
        let reserved_cost: f64 = reserved_field(&store, &scope, "cost")
            .await
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(reserved_cost, 0.0);
        let (tokens, cost) = store.term_usage_read(&scope, "l:0").await.unwrap();
        assert_eq!(tokens, 30);
        assert!((cost - 0.35).abs() < 1e-9, "{cost}");
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn term_reconcile_after_period_roll_keeps_the_new_period() {
        let Some(store) = test_store().await else {
            return;
        };
        let scope = Uuid::new_v4();
        store
            .reserve_with_term(&scope, 0, 0, 40, Some(token_gate("m:2026-08", 100)), 0.0)
            .await
            .unwrap();
        // Another request rolls the counters to the next period and reserves.
        store
            .reserve_with_term(&scope, 0, 0, 7, Some(token_gate("m:2026-09", 100)), 0.0)
            .await
            .unwrap();
        // The straggler from the old period settles: its usage is counted,
        // but it must neither wipe the new period nor release the new
        // period's reservation.
        let committed = store
            .term_reconcile(&scope.to_string(), "m:2026-08", 40, 0.0, 30, 0.0)
            .await
            .unwrap();
        assert_eq!(committed, (30, 0.0));
        assert_eq!(
            term_field(&store, &scope, "period").await.as_deref(),
            Some("m:2026-09")
        );
        assert_eq!(
            reserved_field(&store, &scope, "tokens").await.as_deref(),
            Some("7")
        );
        assert_eq!(
            term_field(&store, &scope, "tokens").await.as_deref(),
            Some("30")
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn concurrent_reservations_at_cap_minus_one_admit_only_one() {
        let Some(store) = test_store().await else {
            return;
        };
        let scope = Uuid::new_v4();
        store.term_usage_add(&scope, "l:0", 99, 0.0).await.unwrap();
        let (ra, rb) = tokio::join!(
            store.reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.0),
            store.reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.0),
        );
        let outcomes = [ra.unwrap(), rb.unwrap()];
        let admitted = outcomes
            .iter()
            .filter(|o| matches!(o, ReserveOutcome::Reserved { .. }))
            .count();
        let rejected = outcomes
            .iter()
            .filter(|o| matches!(o, ReserveOutcome::TermExhausted { .. }))
            .count();
        assert_eq!((admitted, rejected), (1, 1), "{outcomes:?}");
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn reconcile_that_recreates_the_bucket_sets_ttl_and_ts() {
        let Some(store) = test_store().await else {
            return;
        };
        let tenant = Uuid::new_v4();
        // Bucket expired between reserve and reconcile.
        let remaining = store.reconcile_budget(&tenant, 100, 10, 30).await.unwrap();
        assert_eq!(remaining, 80);
        let mut conn = store.conn.clone();
        let key = RedisStore::budget_key(&tenant);
        let ttl: i64 = redis::cmd("PTTL")
            .arg(&key)
            .query_async(&mut conn)
            .await
            .unwrap();
        assert!(ttl > 0, "recreated bucket must expire, PTTL={ttl}");
        let ts: Option<i64> = conn.hget(&key, "ts").await.unwrap();
        assert!(ts.is_some(), "recreated bucket must be re-seeded with ts");
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn estimate_larger_than_capacity_is_admitted_against_a_full_bucket() {
        let Some(store) = test_store().await else {
            return;
        };
        let (capacity, tpm) = (100i64, 0i64);

        let tenant = Uuid::new_v4();
        let (ok, remaining) = store
            .reserve_budget(&tenant, capacity, tpm, 150)
            .await
            .unwrap();
        assert!(ok, "oversized request against a full bucket must pass");
        assert_eq!(remaining, -50);
        // Bucket now in debt: the next request waits for refill.
        let (ok, _) = store
            .reserve_budget(&tenant, capacity, tpm, 150)
            .await
            .unwrap();
        assert!(!ok);

        // The debt floor (-capacity) still holds.
        let tenant = Uuid::new_v4();
        let (ok, remaining) = store
            .reserve_budget(&tenant, capacity, tpm, 10_000)
            .await
            .unwrap();
        assert!(ok);
        assert_eq!(remaining, -capacity);

        // A partially drained bucket still rejects the oversized request.
        let tenant = Uuid::new_v4();
        store
            .reserve_budget(&tenant, capacity, tpm, 1)
            .await
            .unwrap();
        let (ok, _) = store
            .reserve_budget(&tenant, capacity, tpm, 150)
            .await
            .unwrap();
        assert!(!ok);

        // Same rule through the combined term + bucket script.
        let tenant = Uuid::new_v4();
        let out = store
            .reserve_with_term(&tenant, capacity, tpm, 150, None, 0.0)
            .await
            .unwrap();
        assert_eq!(out, ReserveOutcome::Reserved { remaining: -50 });
    }

    fn replica_test_key() -> SlotKeys {
        SlotKeys::with_prefix(&format!("obleth:test:{}", Uuid::new_v4()))
    }

    async fn drop_test_keys(store: &RedisStore, keys: &SlotKeys, instances: &[&str]) {
        let mut conn = store.conn.clone();
        let mut all = vec![keys.replicas.clone(), keys.totals.clone()];
        all.extend(instances.iter().map(|i| keys.held(i)));
        let _: () = conn.del(all).await.unwrap();
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn replica_heartbeats_count_live_instances_and_expire_the_silent() {
        let Some(store) = test_store().await else {
            return;
        };
        let key = replica_test_key();
        let ttl = std::time::Duration::from_millis(400);

        assert_eq!(store.replica_heartbeat_in(&key, "a", ttl).await.unwrap(), 1);
        assert_eq!(store.replica_heartbeat_in(&key, "b", ttl).await.unwrap(), 2);
        // A refresh is not a second replica.
        assert_eq!(store.replica_heartbeat_in(&key, "a", ttl).await.unwrap(), 2);

        // b stops heartbeating (a crash): once its TTL passes it is no longer
        // counted, while a, still refreshing, is.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert_eq!(store.replica_heartbeat_in(&key, "a", ttl).await.unwrap(), 2);
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert_eq!(
            store.replica_heartbeat_in(&key, "a", ttl).await.unwrap(),
            1,
            "b expired"
        );
        let mut conn = store.conn.clone();
        let members: Vec<String> = conn.zrange(&key.replicas, 0, -1).await.unwrap();
        assert_eq!(members, vec!["a".to_string()], "expired members are pruned");

        // The set itself never expires, so volatile-lru cannot evict it.
        let pttl: i64 = conn.pttl(&key.replicas).await.unwrap();
        assert_eq!(pttl, -1);

        drop_test_keys(&store, &key, &[]).await;
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn replica_deregister_drops_the_instance_at_once() {
        let Some(store) = test_store().await else {
            return;
        };
        let key = replica_test_key();
        let ttl = std::time::Duration::from_secs(30);
        store.replica_heartbeat_in(&key, "a", ttl).await.unwrap();
        assert_eq!(store.replica_heartbeat_in(&key, "b", ttl).await.unwrap(), 2);
        store.replica_deregister_in(&key, "a").await.unwrap();
        assert_eq!(store.replica_heartbeat_in(&key, "b", ttl).await.unwrap(), 1);
        // Deregistering an unknown instance is a no-op, not an error.
        store.replica_deregister_in(&key, "gone").await.unwrap();
        drop_test_keys(&store, &key, &[]).await;
    }

    /// A TCP relay to the test Redis that can be taken down and brought back
    /// on the same port, standing in for a Redis outage after boot.
    struct Relay {
        port: u16,
        target: String,
        tasks: std::sync::Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    }

    impl Relay {
        async fn start(target: String) -> Self {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let port = listener.local_addr().unwrap().port();
            let relay = Relay {
                port,
                target,
                tasks: Default::default(),
            };
            relay.serve(listener);
            relay
        }

        fn serve(&self, listener: tokio::net::TcpListener) {
            let target = self.target.clone();
            let tasks = self.tasks.clone();
            let accept = tokio::spawn(async move {
                while let Ok((mut inbound, _)) = listener.accept().await {
                    let target = target.clone();
                    let conn = tokio::spawn(async move {
                        if let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await {
                            let _ =
                                tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                        }
                    });
                    tasks.lock().unwrap().push(conn);
                }
            });
            self.tasks.lock().unwrap().push(accept);
        }

        /// Close the listener and every relayed connection.
        fn down(&self) {
            for task in self.tasks.lock().unwrap().drain(..) {
                task.abort();
            }
        }

        async fn up(&self) {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", self.port))
                .await
                .unwrap();
            self.serve(listener);
        }
    }

    /// The first connection through a fresh relay, retried: with the whole
    /// suite running in parallel the first round trip through a relay can
    /// miss a sub-second timeout. What these tests check starts after it.
    async fn relayed_connection(
        client: &redis::Client,
        config: ConnectionManagerConfig,
    ) -> ConnectionManager {
        for _ in 0..20 {
            if let Ok(conn) =
                ConnectionManager::new_with_config(client.clone(), config.clone()).await
            {
                return conn;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("could not connect through the relay");
    }

    /// The first heartbeat on a new connection, retried for the same reason.
    async fn first_heartbeat(
        store: &RedisStore,
        keys: &SlotKeys,
        instance: &str,
        ttl: std::time::Duration,
    ) -> usize {
        for _ in 0..20 {
            if let Ok(n) = store.replica_heartbeat_in(keys, instance, ttl).await {
                return n;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        panic!("the first heartbeat never got through");
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn replica_heartbeat_fails_fast_while_redis_is_down_and_recovers() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let info = redis::Client::open(url.as_str())
            .unwrap()
            .get_connection_info()
            .clone();
        let redis::ConnectionAddr::Tcp(host, port) = info.addr.clone() else {
            eprintln!("skipping: the relay needs a TCP Redis URL");
            return;
        };
        let relay = Relay::start(format!("{host}:{port}")).await;
        let mut relayed = info.clone();
        relayed.addr = redis::ConnectionAddr::Tcp("127.0.0.1".into(), relay.port);
        let client = redis::Client::open(relayed).unwrap();
        let config = ConnectionManagerConfig::new()
            .set_response_timeout(std::time::Duration::from_millis(250))
            .set_connection_timeout(std::time::Duration::from_millis(500))
            .set_number_of_retries(0);
        let store = RedisStore {
            conn: relayed_connection(&client, config).await,
            client,
        };
        let key = replica_test_key();
        let ttl = std::time::Duration::from_secs(30);
        assert_eq!(first_heartbeat(&store, &key, "a", ttl).await, 1);

        relay.down();
        for _ in 0..3 {
            let started = std::time::Instant::now();
            let res = store.replica_heartbeat_in(&key, "a", ttl).await;
            assert!(res.is_err(), "heartbeat must fail while Redis is down");
            assert!(
                started.elapsed() < std::time::Duration::from_secs(2),
                "a failed heartbeat must not hang: {:?}",
                started.elapsed()
            );
        }

        relay.up().await;
        let recovered = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                if let Ok(n) = store.replica_heartbeat_in(&key, "a", ttl).await {
                    return n;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        })
        .await
        .expect("the heartbeat reconnects once Redis is back");
        assert_eq!(recovered, 1);

        let direct = test_store().await.unwrap();
        drop_test_keys(&direct, &key, &[]).await;
        relay.down();
    }

    // ---- shared fairshare slots ------------------------------------------

    use obleth_fairshare::{
        AcquireOutcome, FairShare, PoolHoldings, ReconcileOutcome, SharedSlotsConfig, SlotClaim,
        SlotDenial, SlotMode, SlotRelease, StaticCapacity,
    };

    fn claim(pool: &str, pool_cap: usize, global_cap: usize) -> SlotClaim {
        SlotClaim {
            pool: pool.into(),
            tenant: Uuid::new_v4(),
            key: Uuid::new_v4(),
            pool_cap,
            global_cap,
            tenant_cap: None,
            key_cap: None,
        }
    }

    async fn live(store: &RedisStore, keys: &SlotKeys, instances: &[&str]) {
        for i in instances {
            store
                .replica_heartbeat_in(keys, i, std::time::Duration::from_secs(60))
                .await
                .unwrap();
        }
    }

    async fn totals_field(store: &RedisStore, keys: &SlotKeys, field: &str) -> i64 {
        let mut conn = store.conn.clone();
        let v: Option<i64> = conn.hget(&keys.totals, field).await.unwrap();
        v.unwrap_or(0)
    }

    async fn held_field(store: &RedisStore, keys: &SlotKeys, instance: &str, field: &str) -> i64 {
        let mut conn = store.conn.clone();
        let v: Option<i64> = conn.hget(keys.held(instance), field).await.unwrap();
        v.unwrap_or(0)
    }

    /// Fire `claims` concurrently, spread over `instances`; return how many
    /// were granted.
    async fn race(
        store: &RedisStore,
        keys: &SlotKeys,
        instances: &[&str],
        claims: Vec<SlotClaim>,
    ) -> Vec<SlotClaim> {
        let tasks: Vec<_> = claims
            .into_iter()
            .enumerate()
            .map(|(i, c)| {
                let (store, keys) = (store.clone(), keys.clone());
                let instance = instances[i % instances.len()].to_string();
                tokio::spawn(async move {
                    let (outcome, _) = store.slot_acquire(&keys, &instance, &c).await.unwrap();
                    (outcome == AcquireOutcome::Granted).then_some(c)
                })
            })
            .collect();
        let mut granted = Vec::new();
        for t in tasks {
            if let Some(c) = t.await.unwrap() {
                granted.push(c);
            }
        }
        granted
    }

    /// The totals always equal the sum of the replicas' holdings.
    async fn assert_books_balance(store: &RedisStore, keys: &SlotKeys, instances: &[&str]) {
        let mut conn = store.conn.clone();
        let totals: std::collections::HashMap<String, i64> =
            conn.hgetall(&keys.totals).await.unwrap();
        let mut summed: std::collections::HashMap<String, i64> = Default::default();
        for i in instances {
            let held: std::collections::HashMap<String, i64> =
                conn.hgetall(keys.held(i)).await.unwrap();
            for (f, n) in held {
                *summed.entry(f).or_default() += n;
            }
        }
        let counts: std::collections::HashMap<String, i64> = totals
            .into_iter()
            .filter(|(f, _)| !f.starts_with('w'))
            .collect();
        assert_eq!(counts, summed, "totals are the sum of the holdings");
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    /// Concurrent claims from several replicas never take a slot past any
    /// limit, each limit binding on its own and all of them together.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn slot_claims_are_atomic_across_every_limit() {
        let Some(store) = test_store().await else {
            return;
        };
        let keys = replica_test_key();
        let gws = ["a", "b", "c", "d"];
        live(&store, &keys, &gws).await;

        // The pool binds.
        let granted = race(
            &store,
            &keys,
            &gws,
            (0..50).map(|_| claim("m:p", 10, 1000)).collect(),
        )
        .await;
        assert_eq!(granted.len(), 10);
        assert_eq!(totals_field(&store, &keys, "p|m:p").await, 10);
        assert_eq!(totals_field(&store, &keys, "g").await, 10);

        // The tenant binds, in a pool with room.
        let tenant = Uuid::new_v4();
        let granted = race(
            &store,
            &keys,
            &gws,
            (0..30)
                .map(|_| SlotClaim {
                    tenant,
                    tenant_cap: Some(3),
                    ..claim("m:t", 100, 1000)
                })
                .collect(),
        )
        .await;
        assert_eq!(granted.len(), 3);

        // The key binds.
        let key = Uuid::new_v4();
        let granted = race(
            &store,
            &keys,
            &gws,
            (0..30)
                .map(|_| SlotClaim {
                    key,
                    key_cap: Some(2),
                    ..claim("m:k", 100, 1000)
                })
                .collect(),
        )
        .await;
        assert_eq!(granted.len(), 2);

        // The global count (15 so far) binds across pools.
        let granted = race(
            &store,
            &keys,
            &gws,
            (0..30).map(|_| claim("m:g", 100, 19)).collect(),
        )
        .await;
        assert_eq!(granted.len(), 4);
        assert_eq!(totals_field(&store, &keys, "g").await, 19);

        // All at once: a 6-slot pool, one tenant capped at 3 sending most of
        // the load and one of its keys capped at 2.
        let heavy = Uuid::new_v4();
        let heavy_key = Uuid::new_v4();
        let mixed: Vec<SlotClaim> = (0..60)
            .map(|i| match i % 3 {
                0 => SlotClaim {
                    tenant: heavy,
                    key: heavy_key,
                    tenant_cap: Some(3),
                    key_cap: Some(2),
                    ..claim("m:x", 6, 1000)
                },
                1 => SlotClaim {
                    tenant: heavy,
                    tenant_cap: Some(3),
                    ..claim("m:x", 6, 1000)
                },
                _ => claim("m:x", 6, 1000),
            })
            .collect();
        let granted = race(&store, &keys, &gws, mixed).await;
        assert_eq!(granted.len(), 6);
        assert!(granted.iter().filter(|c| c.tenant == heavy).count() <= 3);
        assert!(granted.iter().filter(|c| c.key == heavy_key).count() <= 2);
        assert_eq!(totals_field(&store, &keys, "p|m:x").await, 6);
        assert_books_balance(&store, &keys, &gws).await;
        drop_test_keys(&store, &keys, &gws).await;
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn slot_releases_free_only_what_the_replica_holds() {
        let Some(store) = test_store().await else {
            return;
        };
        let keys = replica_test_key();
        live(&store, &keys, &["a", "b"]).await;
        let c = claim("m:p", 4, 100);
        let (outcome, counts) = store.slot_acquire(&keys, "a", &c).await.unwrap();
        assert_eq!(outcome, AcquireOutcome::Granted);
        assert_eq!((counts.pool, counts.global), (1, 1));
        let slot = SlotRelease {
            pool: c.pool.clone(),
            tenant: c.tenant,
            key: c.key,
        };
        // b holds nothing, so its release frees nothing.
        let (freed, counts) = store.slot_release(&keys, "b", &slot).await.unwrap();
        assert!(!freed);
        assert_eq!(counts.pool, 1);
        let (freed, counts) = store.slot_release(&keys, "a", &slot).await.unwrap();
        assert!(freed);
        assert_eq!((counts.pool, counts.global), (0, 0));
        // A second release of the same slot is a no-op, not a negative count.
        let (freed, _) = store.slot_release(&keys, "a", &slot).await.unwrap();
        assert!(!freed);
        let mut conn = store.conn.clone();
        let fields: std::collections::HashMap<String, i64> =
            conn.hgetall(&keys.totals).await.unwrap();
        assert!(fields.is_empty(), "counts at zero are deleted: {fields:?}");
        drop_test_keys(&store, &keys, &["a", "b"]).await;
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    /// A replica's slots are reclaimed once its heartbeat expires, by the
    /// next live replica's heartbeat, and not a moment before.
    #[tokio::test]
    async fn slot_holdings_are_reclaimed_when_the_heartbeat_expires_and_not_before() {
        let Some(store) = test_store().await else {
            return;
        };
        let keys = replica_test_key();
        let ttl = std::time::Duration::from_millis(400);
        store.replica_heartbeat_in(&keys, "a", ttl).await.unwrap();
        store.replica_heartbeat_in(&keys, "b", ttl).await.unwrap();
        let mut held = Vec::new();
        for _ in 0..3 {
            let c = claim("m:p", 10, 100);
            let (outcome, _) = store.slot_acquire(&keys, "b", &c).await.unwrap();
            assert_eq!(outcome, AcquireOutcome::Granted);
            held.push(c);
        }
        // Inside b's TTL: a's heartbeat leaves b's slots alone.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert_eq!(
            store.replica_heartbeat_in(&keys, "a", ttl).await.unwrap(),
            2
        );
        assert_eq!(totals_field(&store, &keys, "p|m:p").await, 3);
        assert_eq!(held_field(&store, &keys, "b", "p|m:p").await, 3);

        // Past it: the next heartbeat reclaims them.
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        assert_eq!(
            store.replica_heartbeat_in(&keys, "a", ttl).await.unwrap(),
            1
        );
        assert_eq!(totals_field(&store, &keys, "p|m:p").await, 0);
        assert_eq!(totals_field(&store, &keys, "g").await, 0);
        assert_eq!(held_field(&store, &keys, "b", "p|m:p").await, 0);

        // A late release from b frees nothing, and b takes nothing new until
        // it heartbeats again.
        let slot = SlotRelease {
            pool: held[0].pool.clone(),
            tenant: held[0].tenant,
            key: held[0].key,
        };
        let (freed, _) = store.slot_release(&keys, "b", &slot).await.unwrap();
        assert!(!freed);
        let (outcome, _) = store
            .slot_acquire(&keys, "b", &claim("m:p", 10, 100))
            .await
            .unwrap();
        assert_eq!(outcome, AcquireOutcome::NotRegistered);
        drop_test_keys(&store, &keys, &["a", "b"]).await;
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn slot_reconcile_replaces_the_holdings_and_repairs_drift() {
        let Some(store) = test_store().await else {
            return;
        };
        let keys = replica_test_key();
        live(&store, &keys, &["a", "b"]).await;
        let tenant = Uuid::new_v4();
        let key = Uuid::new_v4();
        for _ in 0..2 {
            store
                .slot_acquire(
                    &keys,
                    "a",
                    &SlotClaim {
                        tenant,
                        key,
                        ..claim("m:p", 10, 100)
                    },
                )
                .await
                .unwrap();
        }
        store
            .slot_acquire(&keys, "b", &claim("m:p", 10, 100))
            .await
            .unwrap();
        // Drift: three slots a no longer really holds (lost releases).
        let mut conn = store.conn.clone();
        for f in ["g", "p|m:p"] {
            let _: i64 = conn.hincr(keys.held("a"), f, 3).await.unwrap();
            let _: i64 = conn.hincr(&keys.totals, f, 3).await.unwrap();
        }
        assert_eq!(totals_field(&store, &keys, "p|m:p").await, 6);

        let truth = vec![PoolHoldings {
            pool: "m:p".into(),
            in_flight: 1,
            tenants: vec![(tenant, 1)],
            keys: vec![(key, 1)],
        }];
        assert_eq!(
            store.slot_reconcile(&keys, "a", &truth).await.unwrap(),
            ReconcileOutcome::Synced
        );
        assert_eq!(
            totals_field(&store, &keys, "p|m:p").await,
            2,
            "a's 1 + b's 1"
        );
        assert_eq!(totals_field(&store, &keys, "g").await, 2);
        assert_eq!(
            totals_field(&store, &keys, &format!("t|m:p|{tenant}")).await,
            1
        );
        assert_eq!(held_field(&store, &keys, "a", "p|m:p").await, 1);
        assert_books_balance(&store, &keys, &["a", "b"]).await;

        // Reconciling to nothing clears a's books; b's are untouched.
        store.slot_reconcile(&keys, "a", &[]).await.unwrap();
        assert_eq!(totals_field(&store, &keys, "p|m:p").await, 1);

        // A replica that is not live writes nothing.
        assert_eq!(
            store.slot_reconcile(&keys, "ghost", &truth).await.unwrap(),
            ReconcileOutcome::NotRegistered
        );
        assert_eq!(totals_field(&store, &keys, "p|m:p").await, 1);
        drop_test_keys(&store, &keys, &["a", "b", "ghost"]).await;
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    /// A release publishes the pool only while someone waits for it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn slot_releases_publish_only_while_someone_waits() {
        let Some(store) = test_store().await else {
            return;
        };
        let keys = replica_test_key();
        live(&store, &keys, &["a", "b"]).await;
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let (subscribed_tx, mut subscribed) = tokio::sync::mpsc::unbounded_channel::<()>();
        let listener = {
            let (store, keys) = (store.clone(), keys.clone());
            tokio::spawn(async move {
                store
                    .run_slot_release_listener(
                        &keys,
                        move |pool| {
                            let _ = tx.send(pool);
                        },
                        move |_| {
                            let _ = subscribed_tx.send(());
                        },
                    )
                    .await
            })
        };
        subscribed.recv().await.expect("subscribed");

        let c = claim("m:p", 1, 100);
        store.slot_acquire(&keys, "a", &c).await.unwrap();
        let slot = SlotRelease {
            pool: c.pool.clone(),
            tenant: c.tenant,
            key: c.key,
        };
        store.slot_release(&keys, "a", &slot).await.unwrap();
        let quiet = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await;
        assert!(quiet.is_err(), "nobody waited, so nothing was published");

        store.slot_acquire(&keys, "a", &c).await.unwrap();
        let (outcome, _) = store
            .slot_acquire(&keys, "b", &claim("m:p", 1, 100))
            .await
            .unwrap();
        assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Pool));
        store.slot_release(&keys, "a", &slot).await.unwrap();
        let woken = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
            .await
            .expect("published")
            .unwrap();
        assert_eq!(woken, "m:p");
        listener.abort();
        drop_test_keys(&store, &keys, &["a", "b"]).await;
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn replica_deregister_frees_what_the_replica_still_holds() {
        let Some(store) = test_store().await else {
            return;
        };
        let keys = replica_test_key();
        live(&store, &keys, &["a"]).await;
        store
            .slot_acquire(&keys, "a", &claim("m:p", 10, 100))
            .await
            .unwrap();
        store.replica_deregister_in(&keys, "a").await.unwrap();
        assert_eq!(totals_field(&store, &keys, "p|m:p").await, 0);
        assert_eq!(held_field(&store, &keys, "a", "p|m:p").await, 0);
        drop_test_keys(&store, &keys, &["a"]).await;
    }

    fn slot_config() -> SharedSlotsConfig {
        SharedSlotsConfig {
            reconcile_interval: std::time::Duration::from_secs(60),
            recovery_interval: std::time::Duration::from_millis(100),
            // Far out, so a waiter admitted promptly proves the wakeup.
            retry_min: std::time::Duration::from_secs(30),
            retry_max: std::time::Duration::from_secs(30),
            ..SharedSlotsConfig::default()
        }
    }

    /// A gateway's scheduler on `store`, live under `instance` and listening
    /// for release wakeups.
    async fn slot_gateway(
        store: &RedisStore,
        keys: &SlotKeys,
        instance: &str,
        replicas: usize,
        config: SharedSlotsConfig,
    ) -> (FairShare, tokio::task::JoinHandle<()>) {
        live(store, keys, &[instance]).await;
        let fs = FairShare::start(
            std::sync::Arc::new(StaticCapacity::new(4096)),
            obleth_config::FairshareAlgorithm::Hierarchical,
            32,
        );
        fs.enable_shared_slots(
            std::sync::Arc::new(RedisSlots::with_keys(store.clone(), instance, keys.clone())),
            config,
        );
        fs.set_replicas(replicas);
        let (subscribed_tx, mut subscribed) = tokio::sync::mpsc::unbounded_channel::<()>();
        let listener = {
            let (store, keys, fs) = (store.clone(), keys.clone(), fs.clone());
            tokio::spawn(async move {
                let woken = fs.clone();
                store
                    .run_slot_release_listener(
                        &keys,
                        move |pool| woken.wake(&pool),
                        move |reconnected| {
                            if reconnected {
                                fs.wake_all();
                            }
                            let _ = subscribed_tx.send(());
                        },
                    )
                    .await
            })
        };
        subscribed.recv().await.expect("subscribed");
        (fs, listener)
    }

    async fn wait_for_slot_mode(fs: &FairShare, mode: SlotMode) {
        let stats = fs.stats();
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while stats.mode() != mode {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {}", mode.as_str()));
    }

    fn model_req(cap: usize) -> obleth_fairshare::AdmitRequest {
        obleth_fairshare::AdmitRequest::new(Uuid::new_v4(), "shared-model", 1).model_cap(cap)
    }

    async fn admit_in_time(fs: &FairShare, cap: usize) -> obleth_fairshare::Admitted {
        tokio::time::timeout(std::time::Duration::from_secs(3), fs.admit(model_req(cap)))
            .await
            .expect("admitted in time")
            .expect("scheduler alive")
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    /// Three gateways on one Redis: all 20 requests for a 20-slot model land
    /// on one gateway and are all admitted; a waiter on another gateway gets
    /// the next slot freed, through the release wakeup.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn gateways_share_a_pool_through_redis() {
        let Some(store) = test_store().await else {
            return;
        };
        let keys = replica_test_key();
        let mut gws = Vec::new();
        for name in ["a", "b", "c"] {
            gws.push(slot_gateway(&store, &keys, name, 3, slot_config()).await);
        }
        for (fs, _) in &gws {
            wait_for_slot_mode(fs, SlotMode::Shared).await;
        }
        let pool = obleth_fairshare::PoolKey::Model("shared-model".into());
        let mut permits = Vec::new();
        for _ in 0..20 {
            permits.push(admit_in_time(&gws[0].0, 20).await);
        }
        assert_eq!(
            totals_field(&store, &keys, &format!("p|{}", pool.slot_id())).await,
            20
        );
        assert_eq!(
            gws[1]
                .0
                .cluster_in_flight(std::slice::from_ref(&pool))
                .await,
            Some((20, vec![20]))
        );

        let b = gws[1].0.clone();
        let waiter = tokio::spawn(async move { b.admit(model_req(20)).await });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!waiter.is_finished(), "the pool is full cluster-wide");
        let freed = std::time::Instant::now();
        permits.pop();
        let admitted = tokio::time::timeout(std::time::Duration::from_secs(3), waiter)
            .await
            .expect("woken by the release")
            .unwrap()
            .expect("admitted");
        assert!(
            freed.elapsed() < std::time::Duration::from_secs(3),
            "admitted by the wakeup, not the 30 s retry"
        );
        assert_eq!(
            held_field(&store, &keys, "b", &format!("p|{}", pool.slot_id())).await,
            1
        );
        drop(admitted);
        drop(permits);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while totals_field(&store, &keys, "g").await != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("every slot released");
        for (_, listener) in gws {
            listener.abort();
        }
        drop_test_keys(&store, &keys, &["a", "b", "c"]).await;
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    /// Redis going away mid-traffic puts a gateway on the split; when it
    /// comes back the gateway reconciles what it admitted meanwhile and goes
    /// back to shared slots.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_redis_outage_falls_back_to_the_split_and_recovers() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let info = redis::Client::open(url.as_str())
            .unwrap()
            .get_connection_info()
            .clone();
        let redis::ConnectionAddr::Tcp(host, port) = info.addr.clone() else {
            eprintln!("skipping: the relay needs a TCP Redis URL");
            return;
        };
        let relay = Relay::start(format!("{host}:{port}")).await;
        let mut relayed_info = info.clone();
        relayed_info.addr = redis::ConnectionAddr::Tcp("127.0.0.1".into(), relay.port);
        let client = redis::Client::open(relayed_info).unwrap();
        let config = ConnectionManagerConfig::new()
            .set_response_timeout(std::time::Duration::from_millis(250))
            .set_connection_timeout(std::time::Duration::from_millis(500))
            .set_number_of_retries(0);
        let relayed = RedisStore {
            conn: relayed_connection(&client, config).await,
            client,
        };
        let direct = test_store().await.unwrap();
        let keys = replica_test_key();
        // b is another live gateway; a reaches Redis through the relay.
        live(&direct, &keys, &["b"]).await;
        let (fs, listener) = slot_gateway(&relayed, &keys, "a", 2, slot_config()).await;
        wait_for_slot_mode(&fs, SlotMode::Shared).await;
        let pool = format!(
            "p|{}",
            obleth_fairshare::PoolKey::Model("shared-model".into()).slot_id()
        );
        let mut permits = vec![admit_in_time(&fs, 10).await];
        assert_eq!(held_field(&direct, &keys, "a", &pool).await, 1);

        relay.down();
        let started = std::time::Instant::now();
        permits.push(admit_in_time(&fs, 10).await);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "a dead Redis cannot stall admission: {:?}",
            started.elapsed()
        );
        assert_eq!(fs.stats().mode(), SlotMode::Fallback);
        let snap = fs.snapshot().await.unwrap();
        assert_eq!(snap.mode, "fallback");
        assert_eq!(snap.pools[0].cap, 5, "ceil(10 / 2)");
        while permits.len() < 5 {
            permits.push(admit_in_time(&fs, 10).await);
        }
        let blocked = {
            let fs = fs.clone();
            tokio::spawn(async move { fs.admit(model_req(10)).await })
        };
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!blocked.is_finished(), "the split still bounds admission");

        relay.up().await;
        wait_for_slot_mode(&fs, SlotMode::Shared).await;
        let admitted = tokio::time::timeout(std::time::Duration::from_secs(3), blocked)
            .await
            .expect("admitted once shared again")
            .unwrap()
            .expect("admitted");
        assert_eq!(
            held_field(&direct, &keys, "a", &pool).await,
            6,
            "what the split admitted is on the books"
        );
        drop(admitted);
        drop(permits);
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while held_field(&direct, &keys, "a", &pool).await != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("releases recorded after recovery");
        listener.abort();
        relay.down();
        drop_test_keys(&direct, &keys, &["a", "b"]).await;
    }
}
