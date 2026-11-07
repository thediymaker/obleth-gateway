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
//!
//! With session affinity, a finished step's slot can be held for the
//! session's next step ([`SharedSlots::hold`]). A hold leaves its replica's
//! holdings but still counts against every limit, and the next claim naming
//! it, from whichever replica the step reached, takes it over. A hold nobody
//! takes is freed when it runs out, like the holdings of a dead replica.
//!
//! The backend also carries each replica's [`ReplicaView`], a compact copy
//! of its scheduler state published every [`SharedSlotsConfig::view_interval`]
//! while more than one replica is live, so dashboards can show the queues and
//! shares of the whole fleet (see [`crate::view`]). That is off the request
//! path and never affects admission.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use uuid::Uuid;

use crate::view::{PeerView, PeerViews, ReplicaView};
use crate::FairShare;

/// How long a refused claim marks its pool (or the ceiling) as waited on,
/// as the Redis backend does.
const WAIT_MARK: Duration = Duration::from_secs(1);

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
    /// The same model's other pool (see [`crate::PoolKey::sibling`]): its
    /// count is added to this pool's when checking `pool_cap`, since both
    /// draw on the same upstream capacity.
    pub sibling: Option<String>,
    /// A background claim: a refusal marks only its own pool as waited on,
    /// never the global ceiling, so a freed slot wakes background waiters
    /// without changing anything for interactive callers (their pool's
    /// session-affinity holds look at their own pool's mark).
    pub background: bool,
    pub tenant_cap: Option<usize>,
    pub key_cap: Option<usize>,
    /// The id of a slot held for this request's session ([`SlotHold::id`]):
    /// if it is still held, the claim takes it over instead of a new slot.
    pub hold: Option<String>,
    /// Take only the held slot: with none held under `hold`, take nothing
    /// and mark nothing as waited on.
    pub redeem_only: bool,
    /// A request for the pool has waited too long to keep yielding to held
    /// slots: the pool's other held slots are freed first, and none is held
    /// there while such claims keep coming.
    pub starving: bool,
}

/// A finished request's slot to keep for its session's next step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotHold {
    /// The same on every replica for one session in one pool.
    pub id: String,
    pub ttl: Duration,
    /// The most slots the pool may have held at once, cluster-wide.
    pub max_held: usize,
    /// How long ago the finished request arrived. It chained onto the
    /// session's previous step, and so is worth holding for, if it arrived
    /// within `ttl` of that step finishing, on whichever replica.
    pub arrived_ago: Duration,
    /// How long the backend remembers when the session's step finished, for
    /// the next step's check.
    pub remember: Duration,
    /// The most holds in a row a session may have while some replica waits
    /// for the pool. Then its slot is released, so waiters on other replicas
    /// get their turn; a release, or a hold while nobody waits, starts the
    /// count again.
    pub max_run: u32,
}

/// A held slot to give back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlotRelease {
    pub pool: String,
    pub tenant: Uuid,
    pub key: Uuid,
    /// The same model's other pool: a freed slot is room for its waiters
    /// too, so they are woken as well.
    pub sibling: Option<String>,
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
    /// The claim took over the slot held under its `hold` id.
    Redeemed,
    /// A `redeem_only` claim found no slot held for it.
    NotHeld,
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
    /// A session's step finished in a slot this replica holds: note when, and
    /// if the step chained onto the session's previous one (see
    /// [`SlotHold::arrived_ago`]) keep the slot for `hold.ttl` under
    /// `hold.id` instead of giving it back. A held slot leaves this replica's
    /// holdings but still counts against the pool, the ceiling, the tenant
    /// and the key; the first claim naming the id takes it over
    /// ([`AcquireOutcome::Redeemed`]), and one nobody takes is freed when it
    /// runs out. A step that did not chain, or a pool with `max_held` slots
    /// held already, or an id held already, has the slot released as by
    /// [`SharedSlots::release`]. Returns whether it is held.
    fn hold(&self, slot: SlotRelease, hold: SlotHold) -> SlotFuture<(bool, ClusterCounts)>;
    /// Replace everything this replica is recorded as holding with
    /// `holdings`.
    fn reconcile(&self, holdings: Vec<PoolHoldings>) -> SlotFuture<ReconcileOutcome>;
    /// Cluster-wide occupancy: every pool together, then each of `pools`.
    fn totals(&self, pools: Vec<String>) -> SlotFuture<(usize, Vec<usize>)>;
    /// Record `view` as this replica's scheduler state, replacing the last
    /// one; it expires after `ttl` unless published again.
    fn publish_view(&self, view: ReplicaView, ttl: Duration) -> SlotFuture<()>;
    /// The views every other live replica last published, and how many
    /// replicas are live. A replica that is no longer live is left out even
    /// while its view has not expired.
    fn peer_views(&self) -> SlotFuture<PeerViews>;
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
    /// How often this replica publishes its [`ReplicaView`] while more than
    /// one replica is live. Each view lives for [`Self::view_ttl`].
    pub view_interval: Duration,
}

impl SharedSlotsConfig {
    /// How long a published view lives: three intervals, the heartbeat's
    /// own ratio, so one slow publish does not drop a replica from the view.
    pub fn view_ttl(&self) -> Duration {
        self.view_interval.max(Duration::from_millis(100)) * 3
    }
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
            view_interval: Duration::from_millis(500),
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
fn held_field(pool: &str) -> String {
    format!("h|{pool}")
}
const GLOBAL_FIELD: &str = "g";

/// The holdings owner a held slot is recorded under, beside the replicas.
fn hold_owner(id: &str) -> String {
    format!("hold:{id}")
}

/// A published view, with when it was published and how long it lives.
struct MemView {
    view: ReplicaView,
    at: Instant,
    ttl: Duration,
}

#[derive(Default)]
struct MemState {
    totals: HashMap<String, usize>,
    held: HashMap<String, HashMap<String, usize>>,
    live: HashSet<String>,
    /// Held slots by id, with when each runs out.
    holds: HashMap<String, Instant>,
    /// When each session's step last finished, by hold id.
    recent: HashMap<String, Instant>,
    /// Holds in a row while the pool was waited on, by hold id.
    chain: HashMap<String, u32>,
    /// Until when someone waits for each pool, by pool id ("" for the
    /// global ceiling), as the Redis wait markers.
    waiting: HashMap<String, Instant>,
    /// Until when a request for each pool has waited too long for slots to
    /// be held there, by pool id.
    starving: HashMap<String, Instant>,
    views: HashMap<String, MemView>,
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

    /// Free the holds that ran out; returns the pools freed in.
    fn purge_holds(&mut self) -> Vec<String> {
        let now = Instant::now();
        let expired: Vec<String> = self
            .holds
            .iter()
            .filter(|(_, until)| **until <= now)
            .map(|(id, _)| id.clone())
            .collect();
        let mut freed = Vec::new();
        for id in expired {
            self.holds.remove(&id);
            freed.extend(self.reclaim(&hold_owner(&id)));
        }
        freed
    }

    /// Free every slot held in `pool` but `keep`; returns the pools freed in.
    fn free_pool_holds(&mut self, pool: &str, keep: Option<&str>) -> Vec<String> {
        let lead = format!("{pool}|");
        let ids: Vec<String> = self
            .holds
            .keys()
            .filter(|id| id.starts_with(&lead) && Some(id.as_str()) != keep)
            .cloned()
            .collect();
        let mut freed = Vec::new();
        for id in ids {
            self.holds.remove(&id);
            freed.extend(self.reclaim(&hold_owner(&id)));
        }
        freed
    }

    /// Move everything `from` holds to `to`, apart from the held-slot count,
    /// which is dropped.
    fn take_over(&mut self, from: &str, to: &str) {
        let Some(held) = self.held.get(from).cloned() else {
            return;
        };
        for (field, n) in held {
            self.add(from, &field, -(n as isize));
            if !field.starts_with("h|") {
                self.add(to, &field, n as isize);
            }
        }
        self.held.remove(from);
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
    pub publish_view: AtomicUsize,
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

    /// Slots held for sessions' next steps in a pool.
    pub fn held_slots(&self, pool: &str) -> usize {
        self.state.lock().unwrap().count(&held_field(pool))
    }

    /// Free the holds that ran out now, as any backend call would.
    pub fn purge_holds(&self) {
        let (freed, subs) = {
            let mut s = self.state.lock().unwrap();
            (s.purge_holds(), s.subscribers.clone())
        };
        wake_all(&subs, freed);
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

    /// The view `instance` last published, if it has not expired.
    pub fn view(&self, instance: &str) -> Option<ReplicaView> {
        let s = self.state.lock().unwrap();
        s.views
            .get(instance)
            .filter(|v| v.at.elapsed() < v.ttl)
            .map(|v| v.view.clone())
    }

    /// Record a view for `instance` as if it had published it, e.g. for a
    /// replica that is not running a scheduler in the test.
    pub fn put_view(&self, instance: &str, view: ReplicaView, ttl: Duration) {
        self.state.lock().unwrap().views.insert(
            instance.to_string(),
            MemView {
                view,
                at: Instant::now(),
                ttl,
            },
        );
    }
}

fn wake_all(subscribers: &[FairShare], pools: Vec<String>) {
    for pool in pools {
        for fs in subscribers {
            fs.wake(&pool);
        }
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
        let live = s.live.contains(&self.instance);
        let mut freed = if live { s.purge_holds() } else { Vec::new() };
        if live && claim.starving {
            s.starving
                .insert(claim.pool.clone(), Instant::now() + WAIT_MARK);
            freed.extend(s.free_pool_holds(&claim.pool, claim.hold.as_deref()));
        }
        let held = claim.hold.as_ref().filter(|id| s.holds.contains_key(*id));
        // Background work steps aside while an interactive request waits on
        // its model or on the ceiling anywhere, as the Redis backend does.
        let now = Instant::now();
        let marked = |s: &MemState, mark: &str| s.waiting.get(mark).is_some_and(|t| *t > now);
        let yield_to_interactive = claim.background
            && (claim.sibling.as_deref().is_some_and(|sib| marked(&s, sib)) || marked(&s, ""));
        let outcome = if !live {
            AcquireOutcome::NotRegistered
        } else if let Some(id) = held {
            s.holds.remove(id);
            s.take_over(&hold_owner(id), &self.instance);
            AcquireOutcome::Redeemed
        } else if claim.redeem_only {
            AcquireOutcome::NotHeld
        } else if yield_to_interactive
            || s.count(&pf)
                + claim
                    .sibling
                    .as_deref()
                    .map_or(0, |sib| s.count(&pool_field(sib)))
                >= claim.pool_cap
        {
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
        if let AcquireOutcome::Denied(denial) = outcome {
            let mark = match denial {
                SlotDenial::Global if !claim.background => String::new(),
                _ => claim.pool.clone(),
            };
            s.waiting.insert(mark, Instant::now() + WAIT_MARK);
        }
        let c = counts(&s);
        let subs = s.subscribers.clone();
        drop(s);
        wake_all(&subs, freed);
        Box::pin(async move { Ok((outcome, c)) })
    }

    fn hold(&self, slot: SlotRelease, hold: SlotHold) -> SlotFuture<(bool, ClusterCounts)> {
        let mut s = self.cluster.state.lock().unwrap();
        let freed = s.purge_holds();
        let now = Instant::now();
        s.recent
            .retain(|_, done| now.saturating_duration_since(*done) < hold.remember);
        let MemState { recent, chain, .. } = &mut *s;
        chain.retain(|id, _| recent.contains_key(id));
        let arrived = now.checked_sub(hold.arrived_ago).unwrap_or(now);
        let chained = s
            .recent
            .insert(hold.id.clone(), now)
            .is_some_and(|done| arrived.saturating_duration_since(done) <= hold.ttl);
        let pf = pool_field(&slot.pool);
        let hf = held_field(&slot.pool);
        let mine = |s: &MemState, f: &str| {
            s.held
                .get(&self.instance)
                .and_then(|h| h.get(f).copied())
                .unwrap_or(0)
        };
        let waited = [slot.pool.as_str(), ""]
            .iter()
            .any(|p| s.waiting.get(*p).is_some_and(|until| *until > now));
        let run = s.chain.get(&hold.id).copied().unwrap_or(0);
        let starving = s.starving.get(&slot.pool).is_some_and(|until| *until > now);
        let can = chained
            && !starving
            && (!waited || run < hold.max_run)
            && s.count(&hf) < hold.max_held
            && !s.holds.contains_key(&hold.id)
            && mine(&s, &pf) > 0;
        if !can {
            s.chain.remove(&hold.id);
            drop(s);
            wake_all(
                &self.cluster.state.lock().unwrap().subscribers.clone(),
                freed,
            );
            let release = self.release(slot);
            return Box::pin(async move { release.await.map(|c| (false, c)) });
        }
        let owner = hold_owner(&hold.id);
        for f in [
            GLOBAL_FIELD.to_string(),
            pf.clone(),
            tenant_field(&slot.pool, &slot.tenant),
            key_field(&slot.pool, &slot.key),
        ] {
            if mine(&s, &f) > 0 {
                s.add(&self.instance, &f, -1);
                s.add(&owner, &f, 1);
            }
        }
        s.add(&owner, &hf, 1);
        if waited {
            s.chain.insert(hold.id.clone(), run + 1);
        } else {
            s.chain.remove(&hold.id);
        }
        s.holds.insert(hold.id, Instant::now() + hold.ttl);
        let counts = ClusterCounts {
            pool: s.count(&pf),
            global: s.count(GLOBAL_FIELD),
        };
        let subs = s.subscribers.clone();
        drop(s);
        wake_all(&subs, freed);
        Box::pin(async move { Ok((true, counts)) })
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
            if let Some(sibling) = &slot.sibling {
                fs.wake(sibling);
            }
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

    fn publish_view(&self, view: ReplicaView, ttl: Duration) -> SlotFuture<()> {
        self.cluster
            .calls
            .publish_view
            .fetch_add(1, Ordering::Relaxed);
        if let Some(f) = self.fail() {
            return f;
        }
        self.cluster.put_view(&self.instance, view, ttl);
        Box::pin(async { Ok(()) })
    }

    fn peer_views(&self) -> SlotFuture<PeerViews> {
        if let Some(f) = self.fail() {
            return f;
        }
        let s = self.cluster.state.lock().unwrap();
        let mut views: Vec<PeerView> = s
            .live
            .iter()
            .filter(|instance| **instance != self.instance)
            .filter_map(|instance| {
                let v = s.views.get(instance)?;
                let age = v.at.elapsed();
                (age < v.ttl).then(|| PeerView {
                    instance: instance.clone(),
                    view: v.view.clone(),
                    age_ms: age.as_millis() as u64,
                })
            })
            .collect();
        views.sort_by(|a, b| a.instance.cmp(&b.instance));
        let peers = PeerViews {
            live: s.live.len(),
            views,
        };
        Box::pin(async move { Ok(peers) })
    }
}
