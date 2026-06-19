//! Redis layer: hot read-cache of resolved keys + live token-bucket counters.
//!
//! The data plane reads *only* Redis on the hot path (Postgres is the durable
//! source of truth written by the Management API, then synced here). Budget
//! checks run as atomic Lua so they are correct across many gateway pods.

pub mod scripts;

use std::sync::OnceLock;

use futures_util::StreamExt;
use obleth_config::{CachedResponse, ResolvedKey, ResolvedMcpServer, ResolvedModel};
use redis::aio::ConnectionManager;
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

const KEY_PREFIX: &str = "obleth:key:";
const MODEL_PREFIX: &str = "obleth:model:";
const MCP_PREFIX: &str = "obleth:mcp:";
const BUDGET_PREFIX: &str = "obleth:budget:";
const TERM_USAGE_PREFIX: &str = "obleth:term_usage:";
const CACHE_PREFIX: &str = "obleth:cache:";
const INVALIDATE_CHANNEL: &str = "obleth:invalidate";

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
#[derive(Clone)]
pub struct RedisStore {
    conn: ConnectionManager,
    client: redis::Client,
}

impl RedisStore {
    pub async fn connect(url: &str) -> Result<Self> {
        let client = redis::Client::open(url)?;
        let conn = ConnectionManager::new(client.clone()).await?;
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

    /// Store a response in the cache with a TTL (seconds). A `ttl_secs` of 0
    /// disables expiry; the entry then lives until evicted or invalidated.
    pub async fn cache_put(&self, key: &str, value: &CachedResponse, ttl_secs: i64) -> Result<()> {
        let mut conn = self.conn.clone();
        let json = serde_json::to_string(value)?;
        let redis_key = Self::response_cache_key(key);
        if ttl_secs > 0 {
            let _: () = conn.set_ex(redis_key, json, ttl_secs as u64).await?;
        } else {
            let _: () = conn.set(redis_key, json).await?;
        }
        Ok(())
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

    /// Combined admission check in a single round trip: an optional cumulative
    /// term-budget gate followed by the atomic token-bucket reserve. The term
    /// gate runs first, so a term-exhausted request never reserves per-minute
    /// tokens it has no completion path to refund.
    pub async fn reserve_budget_with_term(
        &self,
        tenant: &Uuid,
        capacity: i64,
        tokens_per_minute: i64,
        requested: u32,
        term: Option<TermGate<'_>>,
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
                .arg(capacity)
                .arg(refill_per_ms)
                .arg(now_ms)
                .arg(requested)
                .arg(check_term)
                .arg(period_key)
                .arg(cap_tokens)
                .arg(cap_cost)
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

    /// Run `on_hash` for every invalidation message. Intended to drive a local
    /// moka cache eviction; loops until the connection drops.
    pub async fn run_invalidation_listener<F>(&self, mut on_hash: F) -> Result<()>
    where
        F: FnMut(String) + Send,
    {
        let mut pubsub = self.client.get_async_pubsub().await?;
        pubsub.subscribe(INVALIDATE_CHANNEL).await?;
        let mut stream = pubsub.on_message();
        while let Some(msg) = stream.next().await {
            if let Ok(hash) = msg.get_payload::<String>() {
                on_hash(hash);
            }
        }
        Ok(())
    }
}

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
            allowed_models: None,
            internal: false,
            tracing_enabled: false,
            guardrails_policy: None,
        };
        store.put_resolved_key(&hash, &key).await.unwrap();
        let got = store.get_resolved_key(&hash).await.unwrap().unwrap();
        assert_eq!(got, key);
        store.delete_resolved_key(&hash).await.unwrap();
        assert!(store.get_resolved_key(&hash).await.unwrap().is_none());
    }
}
