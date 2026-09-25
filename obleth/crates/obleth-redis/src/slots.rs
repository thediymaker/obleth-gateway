//! Shared fairshare slots in Redis: the [`SharedSlots`] backend a gateway
//! replica hands its scheduler, and the key layout the slot scripts use (see
//! `scripts::SLOT_ACQUIRE` and the notes above it).
//!
//! Cost per request, only while more than one replica is live: one script
//! call on admission and one on release, each touching the totals hash, this
//! replica's holdings hash and (for admission) a `ZSCORE` on the replica set.
//! Reconciles run once per heartbeat interval, off the request path.

use obleth_fairshare::{
    AcquireOutcome, ClusterCounts, PoolHoldings, ReconcileOutcome, SharedSlots, SlotClaim,
    SlotDenial, SlotError, SlotFuture, SlotRelease,
};

use crate::{RedisError, RedisStore};

/// Live gateway replicas, as a sorted set of instance ids scored by heartbeat
/// expiry (see `scripts::REPLICA_HEARTBEAT`).
pub const REPLICAS_KEY: &str = "obleth:gateway:replicas";
/// Cluster-wide slot counts and wait markers.
pub const SLOT_TOTALS_KEY: &str = "obleth:fairshare:slots";
/// Prefix of each replica's holdings hash; the instance id is appended.
pub const SLOT_HELD_PREFIX: &str = "obleth:fairshare:held:";
/// Pub/sub channel carrying the id of a pool where a slot was freed while
/// someone was waiting for one.
pub const SLOT_RELEASE_CHANNEL: &str = "obleth:fairshare:released";

/// How long a refused claim marks its pool as waited on. Every refusal
/// renews it, and a waiter retries well inside it, so releases keep
/// publishing for as long as anyone is still waiting.
const WAIT_MARK_MS: u64 = 1_000;

/// Where replica and slot state lives. [`Default`] is the gateway's layout;
/// tests use their own so they cannot disturb, or be disturbed by, anything
/// else on the same Redis.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotKeys {
    pub replicas: String,
    pub totals: String,
    pub held_prefix: String,
    pub channel: String,
}

impl Default for SlotKeys {
    fn default() -> Self {
        SlotKeys {
            replicas: REPLICAS_KEY.into(),
            totals: SLOT_TOTALS_KEY.into(),
            held_prefix: SLOT_HELD_PREFIX.into(),
            channel: SLOT_RELEASE_CHANNEL.into(),
        }
    }
}

impl SlotKeys {
    /// Every key under `prefix`, for tests.
    pub fn with_prefix(prefix: &str) -> Self {
        SlotKeys {
            replicas: format!("{prefix}:replicas"),
            totals: format!("{prefix}:slots"),
            held_prefix: format!("{prefix}:held:"),
            channel: format!("{prefix}:released"),
        }
    }

    pub fn held(&self, instance: &str) -> String {
        format!("{}{instance}", self.held_prefix)
    }
}

/// The totals-hash fields a pool's holdings occupy: every pool together, the
/// pool, and each tenant and key in it. Must match the slot scripts.
fn holding_fields(holdings: &[PoolHoldings]) -> Vec<(String, usize)> {
    let mut fields = Vec::new();
    let global: usize = holdings.iter().map(|h| h.in_flight).sum();
    if global > 0 {
        fields.push(("g".to_string(), global));
    }
    for h in holdings {
        if h.in_flight > 0 {
            fields.push((format!("p|{}", h.pool), h.in_flight));
        }
        for (tenant, n) in &h.tenants {
            fields.push((format!("t|{}|{tenant}", h.pool), *n));
        }
        for (key, n) in &h.keys {
            fields.push((format!("k|{}|{key}", h.pool), *n));
        }
    }
    fields.retain(|(_, n)| *n > 0);
    fields
}

fn slot_error(e: RedisError) -> SlotError {
    SlotError(e.to_string())
}

impl RedisStore {
    /// Take one cluster-wide slot for `claim` on behalf of `instance`.
    pub async fn slot_acquire(
        &self,
        keys: &SlotKeys,
        instance: &str,
        claim: &SlotClaim,
    ) -> crate::Result<(AcquireOutcome, ClusterCounts)> {
        let mut conn = self.conn.clone();
        let (code, pool, global): (i64, i64, i64) = crate::slot_acquire_script()
            .key(&keys.totals)
            .key(keys.held(instance))
            .key(&keys.replicas)
            .arg(instance)
            .arg(&claim.pool)
            .arg(claim.tenant.to_string())
            .arg(claim.key.to_string())
            .arg(claim.pool_cap.max(1) as u64)
            .arg(claim.global_cap.max(1) as u64)
            .arg(claim.tenant_cap.unwrap_or(0) as u64)
            .arg(claim.key_cap.unwrap_or(0) as u64)
            .arg(WAIT_MARK_MS)
            .invoke_async(&mut conn)
            .await?;
        let outcome = match code {
            1 => AcquireOutcome::Granted,
            0 => AcquireOutcome::Denied(SlotDenial::Pool),
            -1 => AcquireOutcome::Denied(SlotDenial::Global),
            -2 => AcquireOutcome::Denied(SlotDenial::Tenant),
            -3 => AcquireOutcome::Denied(SlotDenial::Key),
            _ => AcquireOutcome::NotRegistered,
        };
        Ok((
            outcome,
            ClusterCounts {
                pool: pool.max(0) as usize,
                global: global.max(0) as usize,
            },
        ))
    }

    /// Give back one slot `instance` holds. Returns whether one was freed
    /// (not when its holdings no longer record it) and the counts after.
    pub async fn slot_release(
        &self,
        keys: &SlotKeys,
        instance: &str,
        slot: &SlotRelease,
    ) -> crate::Result<(bool, ClusterCounts)> {
        let mut conn = self.conn.clone();
        let (freed, pool, global): (i64, i64, i64) = crate::slot_release_script()
            .key(&keys.totals)
            .key(keys.held(instance))
            .arg(&slot.pool)
            .arg(slot.tenant.to_string())
            .arg(slot.key.to_string())
            .arg(&keys.channel)
            .invoke_async(&mut conn)
            .await?;
        Ok((
            freed == 1,
            ClusterCounts {
                pool: pool.max(0) as usize,
                global: global.max(0) as usize,
            },
        ))
    }

    /// Record exactly `holdings` for `instance`, replacing what it was
    /// recorded as holding.
    pub async fn slot_reconcile(
        &self,
        keys: &SlotKeys,
        instance: &str,
        holdings: &[PoolHoldings],
    ) -> crate::Result<ReconcileOutcome> {
        let mut conn = self.conn.clone();
        let mut invocation = crate::slot_reconcile_script().prepare_invoke();
        invocation
            .key(&keys.totals)
            .key(keys.held(instance))
            .key(&keys.replicas)
            .arg(instance)
            .arg(&keys.channel);
        for (field, n) in holding_fields(holdings) {
            invocation.arg(field).arg(n as u64);
        }
        let code: i64 = invocation.invoke_async(&mut conn).await?;
        Ok(if code == 1 {
            ReconcileOutcome::Synced
        } else {
            ReconcileOutcome::NotRegistered
        })
    }

    /// Cluster-wide occupancy: every pool together, then each of `pools`.
    pub async fn slot_totals(
        &self,
        keys: &SlotKeys,
        pools: &[String],
    ) -> crate::Result<(usize, Vec<usize>)> {
        let mut conn = self.conn.clone();
        let mut cmd = redis::cmd("HMGET");
        cmd.arg(&keys.totals).arg("g");
        for pool in pools {
            cmd.arg(format!("p|{pool}"));
        }
        let values: Vec<Option<i64>> = cmd.query_async(&mut conn).await?;
        let count = |v: Option<&Option<i64>>| v.copied().flatten().unwrap_or(0).max(0) as usize;
        let global = count(values.first());
        let per = (0..pools.len()).map(|i| count(values.get(i + 1))).collect();
        Ok((global, per))
    }
}

/// One replica's [`SharedSlots`] backend on Redis.
#[derive(Clone)]
pub struct RedisSlots {
    store: RedisStore,
    instance: String,
    keys: SlotKeys,
}

impl RedisSlots {
    /// The backend for `instance`, the same id this replica heartbeats
    /// under: a replica whose heartbeat has expired may not take slots.
    pub fn new(store: RedisStore, instance: impl Into<String>) -> Self {
        Self::with_keys(store, instance, SlotKeys::default())
    }

    pub fn with_keys(store: RedisStore, instance: impl Into<String>, keys: SlotKeys) -> Self {
        RedisSlots {
            store,
            instance: instance.into(),
            keys,
        }
    }
}

impl SharedSlots for RedisSlots {
    fn acquire(&self, claim: SlotClaim) -> SlotFuture<(AcquireOutcome, ClusterCounts)> {
        let this = self.clone();
        Box::pin(async move {
            this.store
                .slot_acquire(&this.keys, &this.instance, &claim)
                .await
                .map_err(slot_error)
        })
    }

    fn release(&self, slot: SlotRelease) -> SlotFuture<ClusterCounts> {
        let this = self.clone();
        Box::pin(async move {
            this.store
                .slot_release(&this.keys, &this.instance, &slot)
                .await
                .map(|(_, counts)| counts)
                .map_err(slot_error)
        })
    }

    fn reconcile(&self, holdings: Vec<PoolHoldings>) -> SlotFuture<ReconcileOutcome> {
        let this = self.clone();
        Box::pin(async move {
            this.store
                .slot_reconcile(&this.keys, &this.instance, &holdings)
                .await
                .map_err(slot_error)
        })
    }

    fn totals(&self, pools: Vec<String>) -> SlotFuture<(usize, Vec<usize>)> {
        let this = self.clone();
        Box::pin(async move {
            this.store
                .slot_totals(&this.keys, &pools)
                .await
                .map_err(slot_error)
        })
    }
}
