//! Cluster-wide admission slots shared by every gateway replica.
//!
//! Each replica keeps its own fair queue and decides locally which waiter goes
//! next. With shared slots on, a waiter the local scheduler picks is admitted
//! only once a [`SharedSlots`] backend (Redis in the gateway) has atomically
//! taken a slot for it against the cluster-wide counters: the model's pool,
//! the global ceiling, and the tenant's and key's caps in that pool. The caps
//! passed are the configured values, never divided by the replica count, so
//! one replica can use a model's whole pool while the others are idle and the
//! fleet as a whole still never exceeds it.
//!
//! The backend tracks what each replica holds, so a crashed replica's slots
//! can be reclaimed once its heartbeat expires, and each replica periodically
//! re-asserts its true holdings ([`SharedSlots::reconcile`]) to repair drift
//! from a lost release. When the backend fails, the scheduler falls back to
//! the per-replica split (`ceil(configured / replicas)`) until a reconcile
//! succeeds again.
//!
//! Ordering is fair within a replica. Across replicas a freed slot goes to
//! whichever replica acquires it first; releases wake the others promptly
//! (see [`crate::FairShare::wake`]) and a short jittered retry covers a lost
//! wakeup.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use uuid::Uuid;

use crate::FairShare;

/// A boxed, sendable future returned by [`SharedSlots`] operations.
pub type SlotFuture<T> = Pin<Box<dyn Future<Output = Result<T, SlotError>> + Send + 'static>>;

/// A backend operation that failed or timed out. Any error puts the
/// scheduler in fallback mode until a reconcile succeeds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotError(pub String);

impl std::fmt::Display for SlotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SlotError {}

/// One slot to take, with every cluster-wide limit it must fit under. All
/// caps are the configured values, not a replica's share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotClaim {
    /// The pool's stable id (see [`crate::PoolKey::slot_id`]).
    pub pool: String,
    pub tenant: Uuid,
    pub key: Uuid,
    pub pool_cap: usize,
    pub global_cap: usize,
    pub tenant_cap: Option<usize>,
    pub key_cap: Option<usize>,
}

/// A held slot to give back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRelease {
    pub pool: String,
    pub tenant: Uuid,
    pub key: Uuid,
}

/// Which cluster-wide limit refused a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotDenial {
    Pool,
    Global,
    Tenant,
    Key,
}

/// The answer to a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AcquireOutcome {
    Granted,
    Denied(SlotDenial),
    /// The backend does not count this replica as live (its heartbeat
    /// expired), so it must not take slots until it has re-registered and
    /// reconciled.
    NotRegistered,
}

/// Cluster-wide occupancy after an operation: the claim's pool, and every
/// pool together.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClusterCounts {
    pub pool: usize,
    pub global: usize,
}

/// What this replica holds in one pool, for [`SharedSlots::reconcile`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PoolHoldings {
    pub pool: String,
    pub in_flight: usize,
    pub tenants: Vec<(Uuid, usize)>,
    pub keys: Vec<(Uuid, usize)>,
}

/// The answer to a reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileOutcome {
    /// The backend now records exactly the holdings sent.
    Synced,
    /// The replica is not live in the backend yet; nothing was written.
    NotRegistered,
}

/// Cluster-wide slot accounting, one handle per replica.
///
/// Implementations must make [`SharedSlots::acquire`] atomic across every
/// limit in the claim (check all, then take all or nothing) and must record
/// holdings per replica, so a dead replica's holdings can be reclaimed and a
/// release never frees a slot its replica does not hold.
pub trait SharedSlots: Send + Sync + 'static {
    fn acquire(&self, claim: SlotClaim) -> SlotFuture<(AcquireOutcome, ClusterCounts)>;
    fn release(&self, slot: SlotRelease) -> SlotFuture<ClusterCounts>;
    /// Replace everything this replica is recorded as holding with
    /// `holdings`.
    fn reconcile(&self, holdings: Vec<PoolHoldings>) -> SlotFuture<ReconcileOutcome>;
    /// Cluster-wide occupancy: every pool together, then each of `pools`.
    fn totals(&self, pools: Vec<String>) -> SlotFuture<(usize, Vec<usize>)>;
}

/// Tuning for shared slots. [`Default`] is what the gateway runs with, apart
/// from `reconcile_interval` and `split_on_fallback`, which follow its
/// configuration.
#[derive(Debug, Clone)]
pub struct SharedSlotsConfig {
    /// How often this replica re-asserts its holdings.
    pub reconcile_interval: Duration,
    /// How often a replica in fallback mode tries to reconcile its way back.
    pub recovery_interval: Duration,
    /// A pool refused a slot retries after a random delay in this range even
    /// without a wakeup, so correctness never depends on one arriving.
    pub retry_min: Duration,
    pub retry_max: Duration,
    /// Upper bound on any one backend call, on top of the backend's own
    /// timeouts. A call past it counts as a failure.
    pub op_timeout: Duration,
    /// Claims one pool may have outstanding at once. After a refusal the pool
    /// sends one at a time until a claim succeeds.
    pub max_pending_per_pool: usize,
    /// In fallback mode, divide the limits across the live replicas (the
    /// split); otherwise each replica enforces them in full.
    pub split_on_fallback: bool,
}

impl Default for SharedSlotsConfig {
    fn default() -> Self {
        SharedSlotsConfig {
            reconcile_interval: Duration::from_secs(5),
            recovery_interval: Duration::from_secs(1),
            retry_min: Duration::from_millis(100),
            retry_max: Duration::from_millis(250),
            op_timeout: Duration::from_secs(1),
            max_pending_per_pool: 8,
            split_on_fallback: true,
        }
    }
}

/// How this replica is enforcing the configured limits right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SlotMode {
    /// Limits enforced in full by this replica alone: one live replica, or
    /// both shared slots and the split turned off.
    Local,
    /// Each replica enforces `ceil(configured / replicas)`: shared slots are
    /// turned off and replica-aware sizing is on.
    Split,
    /// Limits are cluster-wide slots in the shared backend.
    Shared,
    /// Shared slots are on but unavailable; the split (or full per-replica
    /// limits, with the split off) applies until the backend answers again.
    Fallback,
}

impl SlotMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SlotMode::Local => "local",
            SlotMode::Split => "split",
            SlotMode::Shared => "shared",
            SlotMode::Fallback => "fallback",
        }
    }

    pub const ALL: [SlotMode; 4] = [
        SlotMode::Local,
        SlotMode::Split,
        SlotMode::Shared,
        SlotMode::Fallback,
    ];

    pub(crate) fn to_u8(self) -> u8 {
        match self {
            SlotMode::Local => 0,
            SlotMode::Split => 1,
            SlotMode::Shared => 2,
            SlotMode::Fallback => 3,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Self {
        match v {
            1 => SlotMode::Split,
            2 => SlotMode::Shared,
            3 => SlotMode::Fallback,
            _ => SlotMode::Local,
        }
    }
}

// ---------------------------------------------------------------------------
// In-process backend
// ---------------------------------------------------------------------------

fn pool_field(pool: &str) -> String {
    format!("p|{pool}")
}
fn tenant_field(pool: &str, tenant: &Uuid) -> String {
    format!("t|{pool}|{tenant}")
}
fn key_field(pool: &str, key: &Uuid) -> String {
    format!("k|{pool}|{key}")
}
const GLOBAL_FIELD: &str = "g";

#[derive(Default)]
struct MemState {
    totals: HashMap<String, usize>,
    held: HashMap<String, HashMap<String, usize>>,
    live: HashSet<String>,
    subscribers: Vec<FairShare>,
    failing: bool,
}

impl MemState {
    fn add(&mut self, instance: &str, field: &str, n: isize) {
        let held = self.held.entry(instance.to_string()).or_default();
        let h = held.entry(field.to_string()).or_insert(0);
        *h = (*h as isize + n).max(0) as usize;
        if *h == 0 {
            held.remove(field);
        }
        let t = self.totals.entry(field.to_string()).or_insert(0);
        *t = (*t as isize + n).max(0) as usize;
        if *t == 0 {
            self.totals.remove(field);
        }
    }

    fn count(&self, field: &str) -> usize {
        self.totals.get(field).copied().unwrap_or(0)
    }

    fn reclaim(&mut self, instance: &str) -> Vec<String> {
        let mut freed = Vec::new();
        if let Some(held) = self.held.remove(instance) {
            for (field, n) in held {
                if let Some(pool) = field.strip_prefix("p|") {
                    freed.push(pool.to_string());
                }
                let t = self.totals.entry(field.clone()).or_insert(0);
                *t = t.saturating_sub(n);
                if *t == 0 {
                    self.totals.remove(&field);
                }
            }
        }
        freed
    }
}

/// Call counts of an [`InMemorySlots`] cluster, for tests.
#[derive(Debug, Default)]
pub struct SlotCalls {
    pub acquire: AtomicUsize,
    pub release: AtomicUsize,
    pub reconcile: AtomicUsize,
}

/// An in-process cluster of [`InMemorySlots`] handles with the same
/// semantics as the Redis backend: atomic multi-limit claims, per-replica
/// holdings, reclaim of a replica taken down with [`InMemoryCluster::kill`],
/// and wakeups on release. For tests, and for running several schedulers in
/// one process.
#[derive(Clone, Default)]
pub struct InMemoryCluster {
    state: Arc<Mutex<MemState>>,
    calls: Arc<SlotCalls>,
}

impl InMemoryCluster {
    pub fn new() -> Self {
        Self::default()
    }

    /// A live replica's handle.
    pub fn gateway(&self, instance: &str) -> InMemorySlots {
        self.state.lock().unwrap().live.insert(instance.to_string());
        InMemorySlots {
            cluster: self.clone(),
            instance: instance.to_string(),
        }
    }

    /// Deliver release wakeups to `fairshare`.
    pub fn subscribe(&self, fairshare: &FairShare) {
        self.state
            .lock()
            .unwrap()
            .subscribers
            .push(fairshare.clone());
    }

    /// Make every call fail (a backend outage) or succeed again.
    pub fn set_failing(&self, failing: bool) {
        self.state.lock().unwrap().failing = failing;
    }

    /// A replica's heartbeat expiring: it stops being live and everything it
    /// held is reclaimed.
    pub fn kill(&self, instance: &str) {
        let (freed, subs) = {
            let mut s = self.state.lock().unwrap();
            s.live.remove(instance);
            (s.reclaim(instance), s.subscribers.clone())
        };
        for pool in freed {
            for fs in &subs {
                fs.wake(&pool);
            }
        }
    }

    /// Mark a replica live again (a heartbeat after an expiry).
    pub fn revive(&self, instance: &str) {
        self.state.lock().unwrap().live.insert(instance.to_string());
    }

    /// Cluster-wide occupancy of a pool.
    pub fn pool_in_flight(&self, pool: &str) -> usize {
        self.state.lock().unwrap().count(&pool_field(pool))
    }

    /// Cluster-wide occupancy of every pool together.
    pub fn global_in_flight(&self) -> usize {
        self.state.lock().unwrap().count(GLOBAL_FIELD)
    }

    /// What `instance` is recorded as holding in a pool.
    pub fn held(&self, instance: &str, pool: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .held
            .get(instance)
            .and_then(|h| h.get(&pool_field(pool)).copied())
            .unwrap_or(0)
    }

    /// Record `n` extra slots for `instance` in a pool without a request
    /// behind them: the drift a lost release leaves.
    pub fn inject_drift(&self, instance: &str, pool: &str, n: usize) {
        let mut s = self.state.lock().unwrap();
        s.add(instance, &pool_field(pool), n as isize);
        s.add(instance, GLOBAL_FIELD, n as isize);
    }

    pub fn calls(&self) -> &SlotCalls {
        &self.calls
    }
}

/// One replica's handle on an [`InMemoryCluster`].
#[derive(Clone)]
pub struct InMemorySlots {
    cluster: InMemoryCluster,
    instance: String,
}

impl InMemorySlots {
    fn fail<T: Send + 'static>(&self) -> Option<SlotFuture<T>> {
        if self.cluster.state.lock().unwrap().failing {
            Some(Box::pin(async {
                Err(SlotError("backend unavailable".into()))
            }))
        } else {
            None
        }
    }
}

impl SharedSlots for InMemorySlots {
    fn acquire(&self, claim: SlotClaim) -> SlotFuture<(AcquireOutcome, ClusterCounts)> {
        self.cluster.calls.acquire.fetch_add(1, Ordering::Relaxed);
        if let Some(f) = self.fail() {
            return f;
        }
        let mut s = self.cluster.state.lock().unwrap();
        let pf = pool_field(&claim.pool);
        let tf = tenant_field(&claim.pool, &claim.tenant);
        let kf = key_field(&claim.pool, &claim.key);
        let counts = |s: &MemState| ClusterCounts {
            pool: s.count(&pf),
            global: s.count(GLOBAL_FIELD),
        };
        let outcome = if !s.live.contains(&self.instance) {
            AcquireOutcome::NotRegistered
        } else if s.count(&pf) >= claim.pool_cap {
            AcquireOutcome::Denied(SlotDenial::Pool)
        } else if s.count(GLOBAL_FIELD) >= claim.global_cap {
            AcquireOutcome::Denied(SlotDenial::Global)
        } else if claim.tenant_cap.is_some_and(|c| s.count(&tf) >= c) {
            AcquireOutcome::Denied(SlotDenial::Tenant)
        } else if claim.key_cap.is_some_and(|c| s.count(&kf) >= c) {
            AcquireOutcome::Denied(SlotDenial::Key)
        } else {
            for f in [GLOBAL_FIELD, &pf, &tf, &kf] {
                s.add(&self.instance, f, 1);
            }
            AcquireOutcome::Granted
        };
        let c = counts(&s);
        Box::pin(async move { Ok((outcome, c)) })
    }

    fn release(&self, slot: SlotRelease) -> SlotFuture<ClusterCounts> {
        self.cluster.calls.release.fetch_add(1, Ordering::Relaxed);
        if let Some(f) = self.fail() {
            return f;
        }
        let pf = pool_field(&slot.pool);
        let (counts, subs) = {
            let mut s = self.cluster.state.lock().unwrap();
            for f in [
                GLOBAL_FIELD.to_string(),
                pf.clone(),
                tenant_field(&slot.pool, &slot.tenant),
                key_field(&slot.pool, &slot.key),
            ] {
                let held = s
                    .held
                    .get(&self.instance)
                    .and_then(|h| h.get(&f).copied())
                    .unwrap_or(0);
                if held > 0 {
                    s.add(&self.instance, &f, -1);
                }
            }
            (
                ClusterCounts {
                    pool: s.count(&pf),
                    global: s.count(GLOBAL_FIELD),
                },
                s.subscribers.clone(),
            )
        };
        for fs in subs {
            fs.wake(&slot.pool);
        }
        Box::pin(async move { Ok(counts) })
    }

    fn reconcile(&self, holdings: Vec<PoolHoldings>) -> SlotFuture<ReconcileOutcome> {
        self.cluster.calls.reconcile.fetch_add(1, Ordering::Relaxed);
        if let Some(f) = self.fail() {
            return f;
        }
        let (freed, subs) = {
            let mut s = self.cluster.state.lock().unwrap();
            if !s.live.contains(&self.instance) {
                return Box::pin(async { Ok(ReconcileOutcome::NotRegistered) });
            }
            let freed = s.reclaim(&self.instance);
            for h in &holdings {
                s.add(&self.instance, &pool_field(&h.pool), h.in_flight as isize);
                s.add(&self.instance, GLOBAL_FIELD, h.in_flight as isize);
                for (t, n) in &h.tenants {
                    s.add(&self.instance, &tenant_field(&h.pool, t), *n as isize);
                }
                for (k, n) in &h.keys {
                    s.add(&self.instance, &key_field(&h.pool, k), *n as isize);
                }
            }
            (freed, s.subscribers.clone())
        };
        for pool in freed {
            for fs in &subs {
                fs.wake(&pool);
            }
        }
        Box::pin(async { Ok(ReconcileOutcome::Synced) })
    }

    fn totals(&self, pools: Vec<String>) -> SlotFuture<(usize, Vec<usize>)> {
        if let Some(f) = self.fail() {
            return f;
        }
        let s = self.cluster.state.lock().unwrap();
        let global = s.count(GLOBAL_FIELD);
        let per = pools.iter().map(|p| s.count(&pool_field(p))).collect();
        Box::pin(async move { Ok((global, per)) })
    }
}
