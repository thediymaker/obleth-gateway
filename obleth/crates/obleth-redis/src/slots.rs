//! Shared fairshare slots in Redis: the [`SharedSlots`] backend a gateway
//! replica hands its scheduler, and the key layout the slot scripts use (see
//! `scripts::SLOT_ACQUIRE` and the notes above it).
//!
//! Cost per request, only while more than one replica is live: one script
//! call on admission and one on release, each touching the totals hash, this
//! replica's holdings hash and (for admission) a `ZSCORE` on the replica set,
//! plus a range read of the held slot set. With session affinity a finished
//! step's slot can be held instead of released (the same one call), and a
//! request queued behind a full pool may make one claim for a slot held for
//! its session.
//! Reconciles run once per heartbeat interval, off the request path.
//!
//! Off the request path too, each replica rewrites its scheduler view (a few
//! KB of JSON, bounded by the view's own limits) every view interval with
//! `SET ... PX`, and a dashboard read fetches the other live replicas' views
//! with one read-only script (`scripts::PEER_VIEWS`).

use std::time::Duration;

use obleth_fairshare::{
    AcquireOutcome, ClusterCounts, PeerView, PeerViews, PoolHoldings, ReconcileOutcome,
    ReplicaView, SharedSlots, SlotClaim, SlotDenial, SlotError, SlotFuture, SlotHold, SlotRelease,
};
use serde::{Deserialize, Serialize};

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
/// Prefix of each replica's scheduler view (a JSON string with a TTL); the
/// instance id is appended.
pub const SLOT_VIEW_PREFIX: &str = "obleth:fairshare:view:";
/// Ids of the slots held for sessions' next steps, scored by expiry.
pub const SLOT_HOLDS_KEY: &str = "obleth:fairshare:holds";
/// Prefix of each held slot's holdings hash; the hold id is appended.
pub const SLOT_HOLD_PREFIX: &str = "obleth:fairshare:hold:";
/// When each session's last step finished, by hold id.
pub const SLOT_RECENT_KEY: &str = "obleth:fairshare:recent";
/// Each session's holds in a row while its pool was waited on, by hold id.
pub const SLOT_CHAIN_KEY: &str = "obleth:fairshare:chain";

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
    pub view_prefix: String,
    pub holds: String,
    pub hold_prefix: String,
    pub recent: String,
    pub chain: String,
}

impl Default for SlotKeys {
    fn default() -> Self {
        SlotKeys {
            replicas: REPLICAS_KEY.into(),
            totals: SLOT_TOTALS_KEY.into(),
            held_prefix: SLOT_HELD_PREFIX.into(),
            channel: SLOT_RELEASE_CHANNEL.into(),
            view_prefix: SLOT_VIEW_PREFIX.into(),
            holds: SLOT_HOLDS_KEY.into(),
            hold_prefix: SLOT_HOLD_PREFIX.into(),
            recent: SLOT_RECENT_KEY.into(),
            chain: SLOT_CHAIN_KEY.into(),
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
            view_prefix: format!("{prefix}:view:"),
            holds: format!("{prefix}:holds"),
            hold_prefix: format!("{prefix}:hold:"),
            recent: format!("{prefix}:recent"),
            chain: format!("{prefix}:chain"),
        }
    }

    pub fn held(&self, instance: &str) -> String {
        format!("{}{instance}", self.held_prefix)
    }

    pub fn view(&self, instance: &str) -> String {
        format!("{}{instance}", self.view_prefix)
    }
}

/// A view as stored: with the TTL it was written with, so a reader can tell
/// its age from the time it has left.
#[derive(Serialize, Deserialize)]
struct StoredView {
    ttl_ms: u64,
    view: ReplicaView,
}

/// Turn a `scripts::PEER_VIEWS` reply into [`PeerViews`]. A view that does
/// not parse (another version's, say) is left out, so the replica counts as
/// not reporting rather than failing the read.
fn parse_peer_views(reply: redis::Value) -> Result<PeerViews, SlotError> {
    let bad = || SlotError("unexpected reply to the peer views script".into());
    let redis::Value::Array(items) = reply else {
        return Err(bad());
    };
    let mut items = items.into_iter();
    let live: i64 = items
        .next()
        .and_then(|v| redis::from_redis_value(&v).ok())
        .ok_or_else(bad)?;
    let mut views = Vec::new();
    for item in items {
        let Ok((instance, raw, left)) = redis::from_redis_value::<(String, String, i64)>(&item)
        else {
            continue;
        };
        let Ok(stored) = serde_json::from_str::<StoredView>(&raw) else {
            continue;
        };
        // PTTL is -1 with no expiry and -2 once gone: age unknown, say 0.
        let age_ms = if left >= 0 {
            stored.ttl_ms.saturating_sub(left as u64)
        } else {
            0
        };
        views.push(PeerView {
            instance,
            view: stored.view,
            age_ms,
        });
    }
    views.sort_by(|a, b| a.instance.cmp(&b.instance));
    Ok(PeerViews {
        live: live.max(0) as usize,
        views,
    })
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
            .key(&keys.holds)
            .arg(instance)
            .arg(&claim.pool)
            .arg(claim.tenant.to_string())
            .arg(claim.key.to_string())
            .arg(claim.pool_cap.max(1) as u64)
            .arg(claim.global_cap.max(1) as u64)
            .arg(claim.tenant_cap.unwrap_or(0) as u64)
            .arg(claim.key_cap.unwrap_or(0) as u64)
            .arg(if claim.background { 0 } else { WAIT_MARK_MS })
            .arg(&keys.hold_prefix)
            .arg(&keys.channel)
            .arg(claim.hold.as_deref().unwrap_or(""))
            .arg(if claim.redeem_only { "1" } else { "0" })
            .arg(if claim.starving { "1" } else { "0" })
            .arg(claim.sibling.as_deref().unwrap_or(""))
            .invoke_async(&mut conn)
            .await?;
        let outcome = match code {
            1 => AcquireOutcome::Granted,
            2 => AcquireOutcome::Redeemed,
            3 => AcquireOutcome::NotHeld,
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
            .arg(slot.sibling.as_deref().unwrap_or(""))
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

    /// Keep one slot `instance` holds for a session's next step (see
    /// [`SharedSlots::hold`]). Returns whether it is held and the counts
    /// after.
    pub async fn slot_hold(
        &self,
        keys: &SlotKeys,
        instance: &str,
        slot: &SlotRelease,
        hold: &SlotHold,
    ) -> crate::Result<(bool, ClusterCounts)> {
        let mut conn = self.conn.clone();
        let (code, pool, global): (i64, i64, i64) = crate::slot_hold_script()
            .key(&keys.totals)
            .key(keys.held(instance))
            .key(&keys.holds)
            .key(&keys.recent)
            .key(&keys.chain)
            .arg(&slot.pool)
            .arg(slot.tenant.to_string())
            .arg(slot.key.to_string())
            .arg(&keys.channel)
            .arg(&keys.hold_prefix)
            .arg(&hold.id)
            .arg(hold.ttl.as_millis().max(1) as u64)
            .arg(hold.max_held as u64)
            .arg(hold.arrived_ago.as_millis() as u64)
            .arg(hold.remember.as_millis().max(1) as u64)
            .arg(hold.max_run.max(1))
            .invoke_async(&mut conn)
            .await?;
        Ok((
            code == 2,
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

    /// Record `view` as `instance`'s scheduler view for `ttl`.
    pub async fn slot_publish_view(
        &self,
        keys: &SlotKeys,
        instance: &str,
        view: &ReplicaView,
        ttl: Duration,
    ) -> crate::Result<()> {
        let ttl_ms = ttl.as_millis().max(1) as u64;
        let json = serde_json::to_string(&StoredView {
            ttl_ms,
            view: view.clone(),
        })?;
        let mut conn = self.conn.clone();
        let _: () = redis::cmd("SET")
            .arg(keys.view(instance))
            .arg(json)
            .arg("PX")
            .arg(ttl_ms)
            .query_async(&mut conn)
            .await?;
        Ok(())
    }

    /// The views of every live replica but `instance`, and the live count.
    pub async fn slot_peer_views(
        &self,
        keys: &SlotKeys,
        instance: &str,
    ) -> crate::Result<Result<PeerViews, SlotError>> {
        let mut conn = self.conn.clone();
        let reply: redis::Value = crate::peer_views_script()
            .key(&keys.replicas)
            .arg(&keys.view_prefix)
            .arg(instance)
            .invoke_async(&mut conn)
            .await?;
        Ok(parse_peer_views(reply))
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

    fn hold(&self, slot: SlotRelease, hold: SlotHold) -> SlotFuture<(bool, ClusterCounts)> {
        let this = self.clone();
        Box::pin(async move {
            this.store
                .slot_hold(&this.keys, &this.instance, &slot, &hold)
                .await
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

    fn publish_view(&self, view: ReplicaView, ttl: Duration) -> SlotFuture<()> {
        let this = self.clone();
        Box::pin(async move {
            this.store
                .slot_publish_view(&this.keys, &this.instance, &view, ttl)
                .await
                .map_err(slot_error)
        })
    }

    fn peer_views(&self) -> SlotFuture<PeerViews> {
        let this = self.clone();
        Box::pin(async move {
            this.store
                .slot_peer_views(&this.keys, &this.instance)
                .await
                .map_err(slot_error)?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::Value;

    fn bulk(s: &str) -> Value {
        Value::BulkString(s.as_bytes().to_vec())
    }

    fn stored(ttl_ms: u64) -> String {
        serde_json::to_string(&StoredView {
            ttl_ms,
            view: ReplicaView {
                version: 1,
                pools: Vec::new(),
            },
        })
        .unwrap()
    }

    #[test]
    fn peer_views_reply_parses_with_ages_from_the_ttl_left() {
        let reply = Value::Array(vec![
            Value::Int(3),
            Value::Array(vec![bulk("b"), bulk(&stored(6000)), Value::Int(4500)]),
            Value::Array(vec![bulk("a"), bulk(&stored(6000)), Value::Int(6000)]),
        ]);
        let peers = parse_peer_views(reply).unwrap();
        assert_eq!(peers.live, 3);
        let got: Vec<(&str, u64)> = peers
            .views
            .iter()
            .map(|p| (p.instance.as_str(), p.age_ms))
            .collect();
        assert_eq!(got, vec![("a", 0), ("b", 1500)]);
    }

    /// A view this build cannot read leaves its replica out instead of
    /// failing the whole read.
    #[test]
    fn an_unreadable_view_is_left_out() {
        let reply = Value::Array(vec![
            Value::Int(2),
            Value::Array(vec![
                bulk("x"),
                bulk("{\"not\":\"a view\"}"),
                Value::Int(10),
            ]),
            Value::Array(vec![bulk("y"), bulk(&stored(100)), Value::Int(-1)]),
        ]);
        let peers = parse_peer_views(reply).unwrap();
        assert_eq!(peers.live, 2);
        assert_eq!(peers.views.len(), 1);
        assert_eq!(
            (peers.views[0].instance.as_str(), peers.views[0].age_ms),
            ("y", 0)
        );
    }

    #[test]
    fn a_reply_of_the_wrong_shape_is_an_error() {
        assert!(parse_peer_views(Value::Int(1)).is_err());
        assert!(parse_peer_views(Value::Array(vec![])).is_err());
    }

    #[test]
    fn view_keys_follow_the_prefix() {
        assert_eq!(
            SlotKeys::default().view("gw-1"),
            "obleth:fairshare:view:gw-1"
        );
        assert_eq!(SlotKeys::with_prefix("t").view("gw-1"), "t:view:gw-1");
    }
}
