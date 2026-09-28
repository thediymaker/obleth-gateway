//! Per-model fairshare pools with hierarchical group → tenant → key admission.
//!
//! # Model
//! Every model is its own scheduling pool sized by the model's `max_in_flight`
//! (or the gateway default). Inside a pool, tenants compete under the
//! **weighted** algorithm by minimizing `served / weight`, or under
//! **hierarchical** by splitting the pool's slots across fairshare groups by
//! group weight and then across the tenants in a group by tenant weight. Once a
//! tenant wins a slot, the key inside it with the lowest `served / key_weight`
//! is served. A single scheduler task owns every pool; nothing is shared. The
//! [`CapacityProvider`] is a total in-flight ceiling across all pools, not a
//! fairness input; when it binds, pools are served round-robin.
//!
//! # Replicas and shared slots
//! Every configured limit that states cluster-wide intent (pool sizes, the
//! global ceiling, per-tenant and per-key `max_in_flight`) is meant for the
//! whole fleet of gateway replicas. How a replica holds to that depends on
//! its [`SlotMode`]:
//!
//! - **shared**: with a [`SharedSlots`] backend set by
//!   [`FairShare::enable_shared_slots`] and more than one live replica, every
//!   admission also takes a cluster-wide slot from the backend, checked
//!   against the configured pool size, global ceiling and tenant and key caps
//!   together (see [`shared`]). Any replica can use a model's whole pool; the
//!   fleet never exceeds it. Local limits are the configured values.
//! - **split** / **fallback**: each replica enforces [`replica_share`] of
//!   every limit for the live replica count set with
//!   [`FairShare::set_replicas`]: the behaviour with shared slots off, and
//!   the fallback while the backend is unavailable.
//! - **local**: one live replica (or no division configured) enforces the
//!   configured values itself, with no backend call on the request path.
//!
//! Weights are ratios and are never divided, and fairness (which waiter goes
//! next) is always decided per replica. A resize never touches in-flight
//! requests: shrinking just stops admitting until occupancy falls under the
//! new size, growing dispatches waiters straight away.
//!
//! # Model caps set from outside
//! A model's pool size normally arrives with each admission (the route's
//! `max_in_flight`). [`FairShare::set_model_caps`] overrides it for named
//! models with a configured (cluster-wide) value derived elsewhere, such as
//! the live backend capacity a `discovered` model follows. An override is
//! still a configured value: it is the cluster-wide pool size in shared mode
//! and is divided across the replicas in split mode like any other, and a
//! change resizes the pool the same way a replica-count change does.
//!
//! Requests with no resolved route share one [`PoolKey::Unrouted`] pool. A
//! pool with no permits and no waiters for [`IDLE_POOL_TTL`] is dropped by the
//! scheduler's housekeeping timer, which also drops queued waiters whose
//! caller has gone.

mod algorithm;
mod capacity;
pub mod history;
mod replicas;
pub mod shared;

pub use algorithm::{group_slot_caps, weighted_caps};
pub use capacity::{CapacityProvider, StaticCapacity};
pub use history::{
    FairshareHistory, FairshareSample, GroupSample, HistoryPoint, PoolSample,
    FAIRSHARE_HISTORY_INTERVAL_MS,
};
pub use replicas::{replica_share, ReplicaTracker};
pub use shared::{
    AcquireOutcome, ClusterCounts, InMemoryCluster, InMemorySlots, PoolHoldings, ReconcileOutcome,
    SharedSlots, SharedSlotsConfig, SlotClaim, SlotDenial, SlotError, SlotFuture, SlotMode,
    SlotRelease,
};

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use obleth_config::{Admission, FairshareAlgorithm};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// Pool size for a model that has no explicit `max_in_flight`.
pub const DEFAULT_MODEL_MAX_IN_FLIGHT: usize = 32;

/// Display name of the shared pool for requests with no resolved route.
pub const UNROUTED_POOL: &str = "(unrouted)";

/// How long a pool with no permits and no waiters is kept before housekeeping
/// drops it.
pub const IDLE_POOL_TTL: Duration = Duration::from_secs(600);

/// Housekeeping period: a sixtieth of the idle TTL, kept between 1 and 10 s.
fn housekeeping_interval(idle_pool_ttl: Duration) -> Duration {
    (idle_pool_ttl / 60).clamp(Duration::from_secs(1), Duration::from_secs(10))
}

/// Which scheduling pool an admission lands in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PoolKey {
    /// The pool of a resolved model route.
    Model(String),
    /// One pool shared by every request that did not resolve to a route, so
    /// arbitrary model strings cannot each mint a pool of their own.
    Unrouted,
}

impl PoolKey {
    fn display_name(&self) -> &str {
        match self {
            PoolKey::Model(name) => name,
            PoolKey::Unrouted => UNROUTED_POOL,
        }
    }

    /// The pool's id in the shared slot backend, the same on every replica:
    /// `m:` and the model name, or `u` for the unrouted pool.
    pub fn slot_id(&self) -> String {
        match self {
            PoolKey::Model(name) => format!("m:{name}"),
            PoolKey::Unrouted => "u".into(),
        }
    }

    /// The inverse of [`PoolKey::slot_id`].
    pub fn from_slot_id(id: &str) -> Option<Self> {
        match id {
            "u" => Some(PoolKey::Unrouted),
            _ => id.strip_prefix("m:").map(|m| PoolKey::Model(m.to_string())),
        }
    }
}

/// Live counters for metrics/dashboards.
#[derive(Debug, Default)]
pub struct Stats {
    /// This replica's in-flight requests.
    pub in_flight: AtomicUsize,
    pub queued: AtomicI64,
    /// Live gateway replicas.
    pub replicas: AtomicUsize,
    mode: AtomicU8,
    /// Cluster-wide in-flight requests as last reported by the shared slot
    /// backend. Meaningful only in [`SlotMode::Shared`].
    pub cluster_in_flight: AtomicUsize,
}

impl Stats {
    /// How this replica is enforcing the configured limits.
    pub fn mode(&self) -> SlotMode {
        SlotMode::from_u8(self.mode.load(Ordering::Relaxed))
    }
}

/// Per-group scheduler view for dashboards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupFairshare {
    pub name: String,
    pub weight: i64,
    pub in_flight: usize,
    pub queued: usize,
    pub slot_cap: usize,
    /// Occupancy above this group's apportioned cap, i.e. slots borrowed from
    /// siblings that were leaving capacity idle.
    pub borrowed: usize,
    /// Service actually received by the group's tenants, excluding the
    /// virtual time each tenant was placed at; the scheduler ranks groups by
    /// this over weight. Not the sum of the tenant rows' `served_tokens`.
    pub served_tokens: f64,
    pub share_score: f64,
    pub weight_share: f64,
}

/// Per-tenant scheduler view for dashboards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantFairshare {
    pub tenant_id: Uuid,
    pub fairshare_group: String,
    pub weight: i64,
    /// Per-model in-flight ceiling for this tenant, when one is set, as this
    /// replica enforces it: the configured value, cluster-wide, in shared and
    /// local mode, its share of it in split mode.
    #[serde(default)]
    pub max_in_flight: Option<usize>,
    pub in_flight: usize,
    pub queued: usize,
    pub served_tokens: f64,
    pub share_score: f64,
    pub weight_share: f64,
}

/// Per-key scheduler view for dashboards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyFairshare {
    pub key_id: Uuid,
    pub tenant_id: Uuid,
    pub weight: i64,
    /// The key's per-model cap as this replica enforces it (see
    /// [`TenantFairshare::max_in_flight`]).
    #[serde(default)]
    pub max_in_flight: Option<usize>,
    pub in_flight: usize,
    pub queued: usize,
    pub served_tokens: f64,
    pub share_score: f64,
    /// Share of the whole pool: the tenant's `weight_share` split across the
    /// tenant's active keys by key weight.
    pub weight_share: f64,
}

/// One model's scheduling pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelPoolFairshare {
    pub model: String,
    /// Slots this replica enforces: `configured_cap` in shared and local
    /// mode, its share of it in split mode.
    pub cap: usize,
    /// The pool size as configured (the model's `max_in_flight`, its
    /// discovered size, or the gateway default): the cluster-wide size.
    #[serde(default)]
    pub configured_cap: usize,
    /// This replica's in-flight requests in the pool.
    pub in_flight: usize,
    /// Cluster-wide in-flight requests in the pool as last reported by the
    /// shared slot backend; `None` outside shared mode or before this
    /// replica has heard.
    #[serde(default)]
    pub cluster_in_flight: Option<usize>,
    pub queued: usize,
    pub borrowed: usize,
    pub groups: Vec<GroupFairshare>,
    pub tenants: Vec<TenantFairshare>,
    pub keys: Vec<KeyFairshare>,
}

/// Point-in-time fairshare state across all pools.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FairshareSnapshot {
    pub algorithm: String,
    /// Total in-flight ceiling across all pools, as this replica enforces it:
    /// `configured_max_in_flight` in shared and local mode, its share of it
    /// in split mode.
    pub max_in_flight: usize,
    /// The ceiling as configured: the cluster-wide value.
    #[serde(default)]
    pub configured_max_in_flight: usize,
    /// Live gateway replicas.
    #[serde(default = "one")]
    pub replicas: usize,
    /// How this replica is enforcing the configured limits: `local`,
    /// `split`, `shared` or `fallback` (see [`SlotMode`]).
    #[serde(default = "local_mode")]
    pub mode: String,
    /// Whether shared slots are configured on this replica, whatever the
    /// current mode.
    #[serde(default)]
    pub shared_slots: bool,
    /// Cluster-wide in-flight requests across every pool as last reported by
    /// the shared slot backend; `None` outside shared mode.
    #[serde(default)]
    pub cluster_in_flight: Option<usize>,
    /// Configured default pool size.
    pub default_model_max_in_flight: usize,
    pub global_in_flight: usize,
    pub global_queued: usize,
    pub global_borrowed: usize,
    pub pools: Vec<ModelPoolFairshare>,
    /// Live in-flight request count per model name (models with at least one
    /// in-flight request). Read by the `auto` router.
    #[serde(default)]
    pub model_in_flight: HashMap<String, usize>,
    #[serde(default)]
    pub model_queued: HashMap<String, usize>,
}

/// Serde default for [`FairshareSnapshot::replicas`].
fn one() -> usize {
    1
}

/// Serde default for [`FairshareSnapshot::mode`].
fn local_mode() -> String {
    SlotMode::Local.as_str().into()
}

/// Context passed to the scheduler for a single admission attempt.
#[derive(Debug, Clone)]
pub struct AdmitRequest {
    pub tenant: Uuid,
    pub key: Uuid,
    pub weight: i64,
    pub key_weight: i64,
    pub group: String,
    pub group_weight: i64,
    pub model: String,
    pub model_max_in_flight: Option<usize>,
    pub tenant_max_in_flight: Option<usize>,
    pub key_max_in_flight: Option<usize>,
    pub cost: u32,
}

impl AdmitRequest {
    /// A request with default weights (100), the `default` group, no caps, and
    /// the key id equal to the tenant id.
    ///
    /// The key defaulting to the tenant id keeps callers that have no key
    /// identity (tests, health probes) on a single implicit key per tenant. The
    /// gateway's only production caller, `admit_request_for` in obleth-proxy,
    /// always supplies the real key id.
    pub fn new(tenant: Uuid, model: impl Into<String>, cost: u32) -> Self {
        Self {
            tenant,
            key: tenant,
            weight: 100,
            key_weight: 100,
            group: "default".into(),
            group_weight: 100,
            model: model.into(),
            model_max_in_flight: None,
            tenant_max_in_flight: None,
            key_max_in_flight: None,
            cost,
        }
    }

    pub fn weighted(tenant: Uuid, weight: i64, cost: u32) -> Self {
        Self::new(tenant, "default", cost).weight(weight)
    }

    pub fn weight(mut self, weight: i64) -> Self {
        self.weight = weight;
        self
    }

    pub fn key(mut self, key: Uuid, weight: i64) -> Self {
        self.key = key;
        self.key_weight = weight;
        self
    }

    pub fn group(mut self, name: impl Into<String>, weight: i64) -> Self {
        self.group = name.into();
        self.group_weight = weight;
        self
    }

    pub fn model_cap(mut self, cap: usize) -> Self {
        self.model_max_in_flight = Some(cap);
        self
    }

    pub fn tenant_cap(mut self, cap: usize) -> Self {
        self.tenant_max_in_flight = Some(cap);
        self
    }

    pub fn key_cap(mut self, cap: usize) -> Self {
        self.key_max_in_flight = Some(cap);
        self
    }
}

/// A held admission slot. Dropping it returns capacity to the scheduler.
pub struct Permit {
    release: Option<mpsc::UnboundedSender<Ctl>>,
    tenant: Uuid,
    key: Uuid,
    pool: PoolKey,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(tx) = self.release.take() {
            let _ = tx.send(Ctl::Release {
                tenant: self.tenant,
                key: self.key,
                pool: std::mem::replace(&mut self.pool, PoolKey::Unrouted),
            });
        }
    }
}

/// Result of a successful admission.
pub struct Admitted {
    pub permit: Permit,
    pub admission: Admission,
    pub waited: Duration,
}

enum Ctl {
    Admit {
        pool: PoolKey,
        req: AdmitRequest,
        respond: oneshot::Sender<Admitted>,
        enqueued: Instant,
    },
    Release {
        tenant: Uuid,
        key: Uuid,
        pool: PoolKey,
    },
    Snapshot {
        respond: oneshot::Sender<FairshareSnapshot>,
    },
    Sample {
        respond: oneshot::Sender<FairshareSample>,
    },
    SetReplicas(usize),
    SetModelCaps(HashMap<String, usize>),
    EnableShared {
        backend: Arc<dyn SharedSlots>,
        config: SharedSlotsConfig,
    },
    /// A slot may have been freed in this pool (`None`: anywhere).
    Wake(Option<PoolKey>),
    AcquireDone {
        pool: PoolKey,
        id: u64,
        result: Result<(AcquireOutcome, ClusterCounts), SlotError>,
    },
    ReleaseDone {
        pool: PoolKey,
        result: Result<ClusterCounts, SlotError>,
    },
    ReconcileDone {
        result: Result<ReconcileOutcome, SlotError>,
    },
}

/// Handle to the fairshare scheduler. Cheap to clone.
#[derive(Clone)]
pub struct FairShare {
    ctl: mpsc::UnboundedSender<Ctl>,
    stats: Arc<Stats>,
    model_load: Arc<RwLock<HashMap<String, usize>>>,
    shared: Arc<OnceLock<Arc<dyn SharedSlots>>>,
}

impl FairShare {
    pub fn start(
        capacity: Arc<dyn CapacityProvider>,
        algorithm: FairshareAlgorithm,
        default_model_max_in_flight: usize,
    ) -> Self {
        Self::start_with_idle_pool_ttl(
            capacity,
            algorithm,
            default_model_max_in_flight,
            IDLE_POOL_TTL,
        )
    }

    /// [`FairShare::start`] with a custom [`IDLE_POOL_TTL`].
    pub fn start_with_idle_pool_ttl(
        capacity: Arc<dyn CapacityProvider>,
        algorithm: FairshareAlgorithm,
        default_model_max_in_flight: usize,
        idle_pool_ttl: Duration,
    ) -> Self {
        let (ctl, rx) = mpsc::unbounded_channel();
        let stats = Arc::new(Stats::default());
        stats.replicas.store(1, Ordering::Relaxed);
        let model_load = Arc::new(RwLock::new(HashMap::new()));
        let scheduler = Scheduler {
            algorithm,
            capacity,
            default_model_cap: default_model_max_in_flight.max(1),
            replicas: 1,
            divisor: 1,
            mode: SlotMode::Local,
            shared: None,
            next_op_id: 0,
            model_caps: HashMap::new(),
            pools: HashMap::new(),
            pool_order: Vec::new(),
            cursor: 0,
            in_flight: 0,
            queued_total: 0,
            idle_pool_ttl,
            ctl_tx: ctl.clone(),
            stats: stats.clone(),
            model_load: model_load.clone(),
        };
        tokio::spawn(scheduler.run(rx));
        FairShare {
            ctl,
            stats,
            model_load,
            shared: Arc::new(OnceLock::new()),
        }
    }

    pub fn stats(&self) -> Arc<Stats> {
        self.stats.clone()
    }

    /// Live in-flight request count per model. Cheap; used by the `auto`
    /// router on every request.
    pub fn model_load(&self) -> HashMap<String, usize> {
        self.model_load
            .read()
            .map(|m| m.clone())
            .unwrap_or_default()
    }

    /// Tell the scheduler how many gateway replicas are live (see the crate
    /// docs): with shared slots on, more than one means admissions go
    /// through the shared backend; otherwise, and in fallback, the limits
    /// are divided by it. Takes effect for the next admission decision;
    /// in-flight requests are never cut short. 0 is read as 1.
    pub fn set_replicas(&self, replicas: usize) {
        let _ = self.ctl.send(Ctl::SetReplicas(replicas.max(1)));
    }

    /// Enforce the configured limits cluster-wide through `backend` whenever
    /// more than one replica is live (see [`shared`]). Call once, before
    /// traffic; later calls are ignored. The scheduler reconciles its
    /// holdings with the backend before it first admits through it.
    pub fn enable_shared_slots(&self, backend: Arc<dyn SharedSlots>, config: SharedSlotsConfig) {
        if self.shared.set(backend.clone()).is_err() {
            return;
        }
        let _ = self.ctl.send(Ctl::EnableShared { backend, config });
    }

    /// A slot may have been freed in the pool with this
    /// [`PoolKey::slot_id`], on any replica: if this replica has waiters
    /// refused a slot there, it retries once now. Unknown ids are ignored.
    pub fn wake(&self, slot_id: &str) {
        if let Some(pool) = PoolKey::from_slot_id(slot_id) {
            let _ = self.ctl.send(Ctl::Wake(Some(pool)));
        }
    }

    /// Like [`FairShare::wake`] for every pool, e.g. after wakeups may have
    /// been missed.
    pub fn wake_all(&self) {
        let _ = self.ctl.send(Ctl::Wake(None));
    }

    /// Cluster-wide in-flight requests read from the shared backend: every
    /// pool together, then each of `pools`. `None` unless this replica is in
    /// shared mode and the backend answers.
    pub async fn cluster_in_flight(&self, pools: &[PoolKey]) -> Option<(usize, Vec<usize>)> {
        if self.stats.mode() != SlotMode::Shared {
            return None;
        }
        let backend = self.shared.get()?;
        let ids = pools.iter().map(PoolKey::slot_id).collect();
        match tokio::time::timeout(Duration::from_secs(1), backend.totals(ids)).await {
            Ok(Ok(totals)) => Some(totals),
            _ => None,
        }
    }

    /// Replace the per-model pool-size overrides: each named model's pool is
    /// sized by its value (a configured, cluster-wide number) instead of the
    /// cap its admissions carry. A model left out goes back to its
    /// admissions' cap from its next admission on.
    /// Resizes like [`FairShare::set_replicas`]: in-flight requests are never
    /// cut short, and growth dispatches waiters at once. 0 is read as 1.
    pub fn set_model_caps(&self, caps: HashMap<String, usize>) {
        let caps = caps.into_iter().map(|(m, c)| (m, c.max(1))).collect();
        let _ = self.ctl.send(Ctl::SetModelCaps(caps));
    }

    pub async fn snapshot(&self) -> Option<FairshareSnapshot> {
        let (respond, rx) = oneshot::channel();
        self.ctl.send(Ctl::Snapshot { respond }).ok()?;
        rx.await.ok()
    }

    /// Admit into the pool named by `req.model`. Callers holding a request
    /// that did not resolve to a route should use [`FairShare::admit_to`]
    /// with [`PoolKey::Unrouted`] instead.
    pub async fn admit(&self, req: AdmitRequest) -> Option<Admitted> {
        let pool = PoolKey::Model(req.model.clone());
        self.admit_to(pool, req).await
    }

    /// Admit into an explicit pool. `req.model` is not consulted for pool
    /// selection.
    pub async fn admit_to(&self, pool: PoolKey, req: AdmitRequest) -> Option<Admitted> {
        let (respond, rx) = oneshot::channel();
        self.ctl
            .send(Ctl::Admit {
                pool,
                req,
                respond,
                enqueued: Instant::now(),
            })
            .ok()?;
        rx.await.ok()
    }

    /// A scheduler sample built directly from pool state, for the history
    /// ring. Cheaper than [`FairShare::snapshot`]: it skips the tenant and
    /// key views the dashboard's live console needs but the history sampler
    /// does not.
    pub async fn sample(&self) -> Option<FairshareSample> {
        let (respond, rx) = oneshot::channel();
        self.ctl.send(Ctl::Sample { respond }).ok()?;
        rx.await.ok()
    }
}

struct Waiter {
    weight: i64,
    key_weight: i64,
    group: String,
    group_weight: i64,
    tenant_max_in_flight: Option<usize>,
    key_max_in_flight: Option<usize>,
    cost: u32,
    enqueued: Instant,
    /// The local fast path would have admitted it on arrival; in shared mode
    /// it is still reported as fast if its first claim succeeds.
    fast: bool,
    respond: oneshot::Sender<Admitted>,
}

/// A waiter whose cluster-wide slot claim is outstanding. It already holds
/// its local slot, so local limits count it.
struct PendingClaim {
    tenant: Uuid,
    key: Uuid,
    waiter: Waiter,
}

/// A tenant's backlog, split per key so the key level can pick fairly.
#[derive(Default)]
struct TenantQueue {
    keys: HashMap<Uuid, VecDeque<Waiter>>,
    len: usize,
}

impl TenantQueue {
    fn is_empty(&self) -> bool {
        self.len == 0
    }

    fn push(&mut self, key: Uuid, waiter: Waiter) {
        self.keys.entry(key).or_default().push_back(waiter);
        self.len += 1;
    }

    /// Put a waiter back in arrival order, e.g. after its claim was refused.
    fn reinsert(&mut self, key: Uuid, waiter: Waiter) {
        let queue = self.keys.entry(key).or_default();
        let at = queue
            .iter()
            .position(|w| w.enqueued > waiter.enqueued)
            .unwrap_or(queue.len());
        queue.insert(at, waiter);
        self.len += 1;
    }

    fn pop(&mut self, key: &Uuid) -> Option<Waiter> {
        let queue = self.keys.get_mut(key)?;
        let waiter = queue.pop_front()?;
        if queue.is_empty() {
            self.keys.remove(key);
        }
        self.len -= 1;
        Some(waiter)
    }

    fn key_len(&self, key: &Uuid) -> usize {
        self.keys.get(key).map(VecDeque::len).unwrap_or(0)
    }
}

/// One model's scheduling pool: the group → tenant → key share tree.
///
/// The key-level maps (`key_in_flight`, `key_served`, `key_weight`, `key_cap`,
/// `key_tenant`) are keyed by key id alone, not by (tenant, key): that relies on
/// `api_keys.id` being globally unique, which it is.
struct Pool {
    key: PoolKey,
    model: String,
    algorithm: FairshareAlgorithm,
    /// Slots enforced here: `replica_share(configured_cap, divisor)`.
    cap: usize,
    configured_cap: usize,
    /// What the configured limits are divided by: the live replica count in
    /// split mode, 1 otherwise.
    divisor: usize,
    in_flight: usize,
    queued_total: usize,
    tenant_in_flight: HashMap<Uuid, usize>,
    tenant_group: HashMap<Uuid, String>,
    group_weight: HashMap<String, i64>,
    tenant_weight: HashMap<Uuid, i64>,
    /// Configured per-tenant caps; enforced as this replica's share.
    tenant_cap: HashMap<Uuid, usize>,
    queues: HashMap<Uuid, TenantQueue>,
    served: HashMap<Uuid, f64>,
    /// The part of each tenant's `served` that is virtual time it was placed
    /// at (on joining, or when snapped forward after going quiet) rather than
    /// service it received. `served - served_base` is its actual service.
    served_base: HashMap<Uuid, f64>,
    key_in_flight: HashMap<Uuid, usize>,
    key_served: HashMap<Uuid, f64>,
    key_weight: HashMap<Uuid, i64>,
    /// Configured per-key caps; enforced as this replica's share.
    key_cap: HashMap<Uuid, usize>,
    key_tenant: HashMap<Uuid, Uuid>,
    virtual_time: f64,
    /// When the pool last became idle (no permits, no waiters); `None` while
    /// it is in use.
    idle_since: Option<Instant>,
    /// Shared mode: claims outstanding with the backend, by id.
    pending: HashMap<u64, PendingClaim>,
    /// Shared mode: a claim was refused, so send one at a time until one
    /// succeeds.
    probing: bool,
    /// Shared mode: the pool is full cluster-wide; wait for a wakeup.
    blocked: bool,
    /// Shared mode: tenants and keys at their cluster-wide cap here.
    blocked_tenants: HashSet<Uuid>,
    blocked_keys: HashSet<Uuid>,
    /// Shared mode: the pool's cluster-wide occupancy as last reported.
    cluster_in_flight: Option<usize>,
    ctl_tx: mpsc::UnboundedSender<Ctl>,
}

impl Pool {
    fn new(
        key: PoolKey,
        algorithm: FairshareAlgorithm,
        cap: usize,
        ctl_tx: mpsc::UnboundedSender<Ctl>,
    ) -> Self {
        Pool {
            model: key.display_name().to_string(),
            key,
            algorithm,
            cap: cap.max(1),
            configured_cap: cap.max(1),
            divisor: 1,
            in_flight: 0,
            queued_total: 0,
            tenant_in_flight: HashMap::new(),
            tenant_group: HashMap::new(),
            group_weight: HashMap::new(),
            tenant_weight: HashMap::new(),
            tenant_cap: HashMap::new(),
            queues: HashMap::new(),
            served: HashMap::new(),
            served_base: HashMap::new(),
            key_in_flight: HashMap::new(),
            key_served: HashMap::new(),
            key_weight: HashMap::new(),
            key_cap: HashMap::new(),
            key_tenant: HashMap::new(),
            virtual_time: 0.0,
            idle_since: None,
            pending: HashMap::new(),
            probing: false,
            blocked: false,
            blocked_tenants: HashSet::new(),
            blocked_keys: HashSet::new(),
            cluster_in_flight: None,
            ctl_tx,
        }
    }

    /// Set the configured pool size; the enforced cap follows from it.
    fn set_configured_cap(&mut self, configured: usize) {
        self.configured_cap = configured.max(1);
        self.cap = replica_share(self.configured_cap, self.divisor);
    }

    fn set_divisor(&mut self, divisor: usize) {
        self.divisor = divisor.max(1);
        self.cap = replica_share(self.configured_cap, self.divisor);
    }

    /// The tenant's enforced cap in this pool, if it has one.
    fn tenant_limit(&self, tenant: &Uuid) -> Option<usize> {
        self.tenant_cap
            .get(tenant)
            .map(|cap| replica_share(*cap, self.divisor))
    }

    /// The key's enforced cap in this pool, if it has one.
    fn key_limit(&self, key: &Uuid) -> Option<usize> {
        self.key_cap
            .get(key)
            .map(|cap| replica_share(*cap, self.divisor))
    }

    /// Forget every cluster-wide refusal: something may have been freed.
    fn unblock(&mut self) {
        self.blocked = false;
        self.blocked_tenants.clear();
        self.blocked_keys.clear();
    }

    fn track_meta(&mut self, req: &AdmitRequest) {
        self.tenant_weight.insert(req.tenant, req.weight.max(1));
        self.tenant_group.insert(req.tenant, req.group.clone());
        self.group_weight
            .insert(req.group.clone(), req.group_weight.max(1));
        self.key_weight.insert(req.key, req.key_weight.max(1));
        self.key_tenant.insert(req.key, req.tenant);
        match req.tenant_max_in_flight.filter(|c| *c > 0) {
            Some(cap) => {
                self.tenant_cap.insert(req.tenant, cap);
            }
            None => {
                self.tenant_cap.remove(&req.tenant);
            }
        }
        match req.key_max_in_flight.filter(|c| *c > 0) {
            Some(cap) => {
                self.key_cap.insert(req.key, cap);
            }
            None => {
                self.key_cap.remove(&req.key);
            }
        }
    }

    fn track_waiter_meta(&mut self, tenant: Uuid, key: Uuid, w: &Waiter) {
        self.tenant_weight.insert(tenant, w.weight.max(1));
        self.tenant_group.insert(tenant, w.group.clone());
        self.group_weight
            .insert(w.group.clone(), w.group_weight.max(1));
        self.key_weight.insert(key, w.key_weight.max(1));
        self.key_tenant.insert(key, tenant);
        match w.tenant_max_in_flight.filter(|c| *c > 0) {
            Some(cap) => {
                self.tenant_cap.insert(tenant, cap);
            }
            None => {
                self.tenant_cap.remove(&tenant);
            }
        }
        match w.key_max_in_flight.filter(|c| *c > 0) {
            Some(cap) => {
                self.key_cap.insert(key, cap);
            }
            None => {
                self.key_cap.remove(&key);
            }
        }
    }

    fn tenant_has_slot(&self, tenant: &Uuid) -> bool {
        match self.tenant_limit(tenant) {
            Some(cap) => self.tenant_in_flight.get(tenant).copied().unwrap_or(0) < cap,
            None => true,
        }
    }

    fn key_has_slot(&self, key: &Uuid) -> bool {
        match self.key_limit(key) {
            Some(cap) => self.key_in_flight.get(key).copied().unwrap_or(0) < cap,
            None => true,
        }
    }

    /// A key can be picked if it is under its cap here and, in shared mode,
    /// not at its cluster-wide cap.
    fn key_open(&self, key: &Uuid) -> bool {
        self.key_has_slot(key) && !self.blocked_keys.contains(key)
    }

    /// A tenant can be picked only if it is under its own cap (here and, in
    /// shared mode, cluster-wide) and at least one of its queued keys is open.
    fn tenant_is_eligible(&self, tenant: &Uuid) -> bool {
        if !self.tenant_has_slot(tenant) || self.blocked_tenants.contains(tenant) {
            return false;
        }
        self.queues
            .get(tenant)
            .map(|q| {
                q.keys
                    .iter()
                    .any(|(k, d)| !d.is_empty() && self.key_open(k))
            })
            .unwrap_or(false)
    }

    fn group_in_flight(&self, group: &str) -> usize {
        self.tenant_in_flight
            .iter()
            .filter(|(tenant, n)| {
                **n > 0 && self.tenant_group.get(*tenant).map(|g| g.as_str()) == Some(group)
            })
            .map(|(_, n)| n)
            .sum()
    }

    fn tenant_is_active(&self, tenant: &Uuid) -> bool {
        self.queues.get(tenant).is_some_and(|q| !q.is_empty())
            || self.tenant_in_flight.get(tenant).copied().unwrap_or(0) > 0
    }

    fn active_tenants_in_group(&self, group: &str) -> Vec<Uuid> {
        let mut tenants: HashSet<Uuid> = HashSet::new();
        for tenant in self.queues.keys().chain(self.tenant_in_flight.keys()) {
            if self.tenant_group.get(tenant).map(|g| g.as_str()) != Some(group) {
                continue;
            }
            if self.tenant_is_active(tenant) {
                tenants.insert(*tenant);
            }
        }
        tenants.into_iter().collect()
    }

    fn tenant_slot_caps(&self, group: &str, group_cap: usize) -> HashMap<Uuid, usize> {
        let tenants = self.active_tenants_in_group(group);
        if group_cap == 0 || tenants.is_empty() {
            return HashMap::new();
        }
        let weighted: Vec<(Uuid, i64)> = tenants
            .into_iter()
            .map(|t| (t, self.tenant_weight.get(&t).copied().unwrap_or(1).max(1)))
            .collect();
        weighted_caps(group_cap, &weighted)
    }

    fn active_groups(&self) -> Vec<(String, i64)> {
        let mut names: HashSet<String> = HashSet::new();
        for tenant in self.queues.keys().chain(self.tenant_in_flight.keys()) {
            if let Some(g) = self.tenant_group.get(tenant) {
                if self.tenant_is_active(tenant) {
                    names.insert(g.clone());
                }
            }
        }
        names
            .into_iter()
            .map(|name| {
                let weight = self.group_weight.get(&name).copied().unwrap_or(100).max(1);
                (name, weight)
            })
            .collect()
    }

    /// The number of slots this pool can actually fill right now: its own cap,
    /// or fewer when the global ceiling leaves less headroom than that. Group
    /// and tenant splits are taken from this figure, not from the pool cap,
    /// so a ceiling that binds below the pool still reserves every group's
    /// share of what is really available. Splitting the nominal cap instead
    /// hands the largest group a cap it can never reach, and it then never
    /// has to yield a freed slot to a smaller group.
    fn effective_cap(&self, headroom: usize) -> usize {
        self.cap.min(self.in_flight.saturating_add(headroom)).max(1)
    }

    fn compute_group_caps(&self, effective: usize) -> HashMap<String, usize> {
        group_slot_caps(effective, &self.active_groups())
    }

    fn can_grant_immediately(&self, req: &AdmitRequest, effective: usize) -> bool {
        if self.in_flight >= effective || self.queued_total > 0 {
            return false;
        }
        if !self.tenant_has_slot(&req.tenant) || !self.key_has_slot(&req.key) {
            return false;
        }
        match self.algorithm {
            FairshareAlgorithm::Weighted => true,
            FairshareAlgorithm::Hierarchical => {
                let caps = self.compute_group_caps(effective);
                let cap = caps.get(&req.group).copied().unwrap_or(effective);
                self.group_in_flight(&req.group) < cap
            }
        }
    }

    fn occupy(&mut self, tenant: Uuid, key: Uuid, cost: u32) {
        self.in_flight += 1;
        *self.tenant_in_flight.entry(tenant).or_insert(0) += 1;
        *self.key_in_flight.entry(key).or_insert(0) += 1;
        let vt = self.virtual_time;
        *self.served.entry(tenant).or_insert_with(|| {
            self.served_base.insert(tenant, vt);
            vt
        }) += cost as f64;
        *self.key_served.entry(key).or_insert(self.virtual_time) += cost as f64;
    }

    fn make_admitted(
        &self,
        tenant: Uuid,
        key: Uuid,
        admission: Admission,
        waited: Duration,
    ) -> Admitted {
        Admitted {
            permit: Permit {
                release: Some(self.ctl_tx.clone()),
                tenant,
                key,
                pool: self.key.clone(),
            },
            admission,
            waited,
        }
    }

    fn send(
        &self,
        tenant: Uuid,
        key: Uuid,
        respond: oneshot::Sender<Admitted>,
        admission: Admission,
        waited: Duration,
    ) {
        // If the caller is gone, `send` hands the `Admitted` back as `Err` and
        // dropping it here drops its `Permit`, which already releases the
        // slot via `Permit::drop`. An explicit release here would double up:
        // this grant would be released twice for one occupy.
        let _ = respond.send(self.make_admitted(tenant, key, admission, waited));
    }

    fn grant_fast(&mut self, req: AdmitRequest, respond: oneshot::Sender<Admitted>) {
        self.occupy(req.tenant, req.key, req.cost);
        self.send(
            req.tenant,
            req.key,
            respond,
            Admission::Fast,
            Duration::ZERO,
        );
    }

    fn enqueue(
        &mut self,
        req: AdmitRequest,
        respond: oneshot::Sender<Admitted>,
        enqueued: Instant,
        fast: bool,
    ) {
        let tenant_queue_empty = self
            .queues
            .get(&req.tenant)
            .is_none_or(TenantQueue::is_empty);
        if tenant_queue_empty {
            let vt = self.virtual_time;
            match self.served.get_mut(&req.tenant) {
                None => {
                    self.served.insert(req.tenant, vt);
                    self.served_base.insert(req.tenant, vt);
                }
                Some(served) if *served < vt => {
                    // The snap forward is placement, not service: move the
                    // base with it so group scores count only real service.
                    *self.served_base.entry(req.tenant).or_insert(0.0) += vt - *served;
                    *served = vt;
                }
                Some(_) => {}
            }
        }
        let key_queue_empty = self
            .queues
            .get(&req.tenant)
            .map(|q| q.key_len(&req.key) == 0)
            .unwrap_or(true);
        if key_queue_empty {
            let entry = self.key_served.entry(req.key).or_insert(self.virtual_time);
            if *entry < self.virtual_time {
                *entry = self.virtual_time;
            }
        }
        self.queues.entry(req.tenant).or_default().push(
            req.key,
            Waiter {
                weight: req.weight,
                key_weight: req.key_weight,
                group: req.group,
                group_weight: req.group_weight,
                tenant_max_in_flight: req.tenant_max_in_flight,
                key_max_in_flight: req.key_max_in_flight,
                cost: req.cost,
                enqueued,
                fast,
                respond,
            },
        );
        self.queued_total += 1;
    }

    /// Admit one queued waiter if the pool has a free slot. Waiters whose
    /// caller has gone are dropped on the way without being charged. Returns
    /// whether a grant happened, so the scheduler can account the global
    /// ceiling, and how many dead waiters were dropped.
    fn dispatch_one(&mut self, effective: usize) -> (bool, usize) {
        let (next, dropped) = self.take_next(effective);
        let Some((tenant, key, waiter)) = next else {
            return (false, dropped);
        };
        let waited = waiter.enqueued.elapsed();
        self.send(tenant, key, waiter.respond, Admission::Queued, waited);
        (true, dropped)
    }

    /// Pick the next waiter the local limits allow and give it its local
    /// slot: dequeued, occupied and charged. Waiters whose caller has gone
    /// are dropped on the way; the count comes back with the pick.
    fn take_next(&mut self, effective: usize) -> (Option<(Uuid, Uuid, Waiter)>, usize) {
        let mut dropped = 0;
        loop {
            if self.in_flight >= effective || self.queued_total == 0 {
                return (None, dropped);
            }
            let Some(tenant) = self.pick_tenant(effective) else {
                return (None, dropped);
            };
            let Some(key) = self.pick_key(&tenant) else {
                return (None, dropped);
            };
            let waiter = {
                let queue = self.queues.get_mut(&tenant).expect("picked tenant exists");
                let waiter = queue.pop(&key).expect("picked key non-empty");
                if queue.is_empty() {
                    self.queues.remove(&tenant);
                }
                waiter
            };
            self.queued_total -= 1;
            if waiter.respond.is_closed() {
                dropped += 1;
                self.forget_if_quiet(tenant, key);
                continue;
            }
            self.track_waiter_meta(tenant, key, &waiter);
            self.occupy(tenant, key, waiter.cost);
            self.advance_virtual_time();
            return (Some((tenant, key, waiter)), dropped);
        }
    }

    /// Shared mode: undo [`Pool::take_next`] for a waiter whose claim was
    /// refused or failed, putting it back in its place in the queue.
    fn requeue(&mut self, claim: PendingClaim) {
        let PendingClaim {
            tenant,
            key,
            mut waiter,
        } = claim;
        self.in_flight = self.in_flight.saturating_sub(1);
        for (map, id) in [
            (&mut self.tenant_in_flight, tenant),
            (&mut self.key_in_flight, key),
        ] {
            if let Some(n) = map.get_mut(&id) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    map.remove(&id);
                }
            }
        }
        if let Some(served) = self.served.get_mut(&tenant) {
            *served -= waiter.cost as f64;
        }
        if let Some(served) = self.key_served.get_mut(&key) {
            *served -= waiter.cost as f64;
        }
        waiter.fast = false;
        self.queues.entry(tenant).or_default().reinsert(key, waiter);
        self.queued_total += 1;
    }

    /// Shared mode: what this pool holds, for a reconcile. Only called with
    /// no claim outstanding, so every local slot is a granted permit.
    fn holdings(&self) -> Option<PoolHoldings> {
        if self.in_flight == 0 {
            return None;
        }
        Some(PoolHoldings {
            pool: self.key.slot_id(),
            in_flight: self.in_flight,
            tenants: self
                .tenant_in_flight
                .iter()
                .filter(|(_, n)| **n > 0)
                .map(|(t, n)| (*t, *n))
                .collect(),
            keys: self
                .key_in_flight
                .iter()
                .filter(|(_, n)| **n > 0)
                .map(|(k, n)| (*k, *n))
                .collect(),
        })
    }

    /// Drop every queued waiter whose caller has gone. Returns how many.
    fn prune_dead_waiters(&mut self) -> usize {
        let mut dropped: Vec<(Uuid, Uuid)> = Vec::new();
        for (tenant, queue) in &mut self.queues {
            for (key, deque) in &mut queue.keys {
                let before = deque.len();
                deque.retain(|w| !w.respond.is_closed());
                for _ in deque.len()..before {
                    dropped.push((*tenant, *key));
                }
            }
            queue.keys.retain(|_, d| !d.is_empty());
            queue.len = queue.keys.values().map(VecDeque::len).sum();
        }
        if dropped.is_empty() {
            return 0;
        }
        self.queues.retain(|_, q| !q.is_empty());
        self.queued_total = self.queued_total.saturating_sub(dropped.len());
        for (tenant, key) in &dropped {
            self.forget_if_quiet(*tenant, *key);
        }
        dropped.len()
    }

    fn pick_tenant(&self, effective: usize) -> Option<Uuid> {
        match self.algorithm {
            FairshareAlgorithm::Weighted => self.pick_tenant_weighted(),
            FairshareAlgorithm::Hierarchical => self.pick_tenant_hierarchical(effective),
        }
    }

    fn pick_tenant_weighted(&self) -> Option<Uuid> {
        let mut best: Option<(Uuid, f64)> = None;
        for (tenant, queue) in &self.queues {
            if queue.is_empty() || !self.tenant_is_eligible(tenant) {
                continue;
            }
            let weight = self.tenant_weight.get(tenant).copied().unwrap_or(1).max(1) as f64;
            let key = self.served.get(tenant).copied().unwrap_or(0.0) / weight;
            match best {
                Some((_, best_key)) if key >= best_key => {}
                _ => best = Some((*tenant, key)),
            }
        }
        best.map(|(t, _)| t)
    }

    fn pick_tenant_hierarchical(&self, effective: usize) -> Option<Uuid> {
        let caps = self.compute_group_caps(effective);
        let mut in_flight_by_group: HashMap<&str, usize> = HashMap::new();
        for (tenant, n) in &self.tenant_in_flight {
            if *n > 0 {
                if let Some(g) = self.tenant_group.get(tenant) {
                    *in_flight_by_group.entry(g.as_str()).or_insert(0) += *n;
                }
            }
        }
        let served_by_group = self.group_service();
        let mut tenant_caps_by_group: HashMap<String, HashMap<Uuid, usize>> = HashMap::new();
        let mut eligible: Vec<(Uuid, f64)> = Vec::new();
        let mut borrowable: Vec<(Uuid, f64)> = Vec::new();

        for (tenant, queue) in &self.queues {
            if queue.is_empty() || !self.tenant_is_eligible(tenant) {
                continue;
            }
            let group = self
                .tenant_group
                .get(tenant)
                .cloned()
                .unwrap_or_else(|| "default".into());
            let group_weight = self.group_weight.get(&group).copied().unwrap_or(100).max(1) as f64;
            let group_score =
                served_by_group.get(group.as_str()).copied().unwrap_or(0.0) / group_weight;
            borrowable.push((*tenant, group_score));

            let cap = caps.get(&group).copied().unwrap_or(effective);
            if in_flight_by_group.get(group.as_str()).copied().unwrap_or(0) >= cap {
                continue;
            }
            let tenant_cap = tenant_caps_by_group
                .entry(group.clone())
                .or_insert_with(|| self.tenant_slot_caps(&group, cap))
                .get(tenant)
                .copied()
                .unwrap_or(cap);
            if self.tenant_in_flight.get(tenant).copied().unwrap_or(0) >= tenant_cap {
                continue;
            }
            eligible.push((*tenant, group_score));
        }

        // Group caps are ceilings on contended demand, not reservations: when
        // nobody is within their cap but the pool still has a free slot, lend it.
        let pool = if eligible.is_empty() {
            borrowable
        } else {
            eligible
        };
        if pool.is_empty() {
            return None;
        }
        let min_group_score = pool.iter().map(|(_, s)| *s).fold(f64::INFINITY, f64::min);
        let mut best: Option<(Uuid, f64)> = None;
        for (tenant, group_score) in pool {
            if (group_score - min_group_score).abs() > f64::EPSILON && group_score > min_group_score
            {
                continue;
            }
            let weight = self.tenant_weight.get(&tenant).copied().unwrap_or(1).max(1) as f64;
            let tenant_score = self.served.get(&tenant).copied().unwrap_or(0.0) / weight;
            match best {
                Some((_, best_score)) if tenant_score >= best_score => {}
                _ => best = Some((tenant, tenant_score)),
            }
        }
        best.map(|(t, _)| t)
    }

    /// Actual service per group: the sum over its tenants of `served` minus
    /// the virtual time each was placed at. Summing raw `served` would count
    /// that placement once per tenant, so a group with many tenants would
    /// look over-served and lose every tie-break.
    fn group_service(&self) -> HashMap<&str, f64> {
        let mut by_group: HashMap<&str, f64> = HashMap::new();
        for (tenant, s) in &self.served {
            if let Some(g) = self.tenant_group.get(tenant) {
                let base = self.served_base.get(tenant).copied().unwrap_or(0.0);
                *by_group.entry(g.as_str()).or_insert(0.0) += (*s - base).max(0.0);
            }
        }
        by_group
    }

    /// Within the winning tenant, the key with the lowest weight-adjusted
    /// service debt goes first; ties break on the older head-of-line request.
    fn pick_key(&self, tenant: &Uuid) -> Option<Uuid> {
        let queue = self.queues.get(tenant)?;
        let mut best: Option<(Uuid, f64, Instant)> = None;
        for (key, deque) in &queue.keys {
            let Some(head) = deque.front() else { continue };
            if !self.key_open(key) {
                continue;
            }
            let weight = self.key_weight.get(key).copied().unwrap_or(1).max(1) as f64;
            let score = self.key_served.get(key).copied().unwrap_or(0.0) / weight;
            let better = match best {
                None => true,
                Some((_, best_score, best_enqueued)) => {
                    score < best_score || (score == best_score && head.enqueued < best_enqueued)
                }
            };
            if better {
                best = Some((*key, score, head.enqueued));
            }
        }
        best.map(|(k, _, _)| k)
    }

    fn advance_virtual_time(&mut self) {
        let min_active = self
            .queues
            .iter()
            .filter(|(_, q)| !q.is_empty())
            .filter_map(|(t, _)| self.served.get(t).copied())
            .fold(f64::INFINITY, f64::min);
        if min_active.is_finite() {
            self.virtual_time = min_active;
        }
    }

    fn release(&mut self, tenant: Uuid, key: Uuid) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if let Some(n) = self.tenant_in_flight.get_mut(&tenant) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.tenant_in_flight.remove(&tenant);
            }
        }
        if let Some(n) = self.key_in_flight.get_mut(&key) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.key_in_flight.remove(&key);
            }
        }
        self.forget_if_quiet(tenant, key);
    }

    fn forget_if_quiet(&mut self, tenant: Uuid, key: Uuid) {
        // A key that is idle and at or below the pool's virtual time would be
        // snapped to virtual time on return anyway, so its entry carries no
        // information worth keeping.
        let key_queued = self
            .queues
            .get(&tenant)
            .map(|q| q.key_len(&key))
            .unwrap_or(0);
        if key_queued == 0
            && !self.key_in_flight.contains_key(&key)
            && self.key_served.get(&key).copied().unwrap_or(0.0) <= self.virtual_time
        {
            self.forget_key(&key);
        }
        // The same rule for the tenant, so tenants that have gone quiet (or
        // have been deleted) stop showing up in the pool's share tree.
        let tenant_queued = self.queues.get(&tenant).map(|q| q.len).unwrap_or(0);
        if tenant_queued == 0
            && !self.tenant_in_flight.contains_key(&tenant)
            && self.served.get(&tenant).copied().unwrap_or(0.0) <= self.virtual_time
        {
            self.forget_tenant(&tenant);
        }
    }

    fn forget_key(&mut self, key: &Uuid) {
        self.key_served.remove(key);
        self.key_weight.remove(key);
        self.key_cap.remove(key);
        self.key_tenant.remove(key);
    }

    /// Only called for a tenant with no in-flight slot and an empty queue.
    fn forget_tenant(&mut self, tenant: &Uuid) {
        self.served.remove(tenant);
        self.served_base.remove(tenant);
        self.tenant_weight.remove(tenant);
        self.tenant_group.remove(tenant);
        self.tenant_cap.remove(tenant);
        self.queues.remove(tenant);
    }

    fn is_idle(&self) -> bool {
        self.in_flight == 0 && self.queued_total == 0
    }

    /// Record whether the pool is in use, resetting its share history the
    /// first time it is found idle.
    fn settle_idle(&mut self) {
        if !self.is_idle() {
            self.idle_since = None;
        } else if self.idle_since.is_none() {
            self.reset();
            self.idle_since = Some(Instant::now());
        }
    }

    /// With nobody holding or waiting for a slot, no tenant has relative debt,
    /// so the history can go. Keeps the pool size.
    fn reset(&mut self) {
        self.tenant_in_flight.clear();
        self.tenant_group.clear();
        self.group_weight.clear();
        self.tenant_weight.clear();
        self.tenant_cap.clear();
        self.queues.clear();
        self.served.clear();
        self.served_base.clear();
        self.key_in_flight.clear();
        self.key_served.clear();
        self.key_weight.clear();
        self.key_cap.clear();
        self.key_tenant.clear();
        self.virtual_time = 0.0;
    }

    /// Per-group in-flight/queued totals, for the history sampler. Cheaper
    /// than [`Pool::snapshot`]: one pass over `tenant_in_flight` and the
    /// queues, no tenant/key views. Only groups with in-flight or queued
    /// work are included.
    fn group_totals(&self) -> Vec<GroupSample> {
        let mut totals: HashMap<&str, (usize, usize)> = HashMap::new();
        for (tenant, n) in &self.tenant_in_flight {
            if *n == 0 {
                continue;
            }
            if let Some(group) = self.tenant_group.get(tenant) {
                totals.entry(group.as_str()).or_insert((0, 0)).0 += *n;
            }
        }
        for (tenant, queue) in &self.queues {
            if queue.len == 0 {
                continue;
            }
            if let Some(group) = self.tenant_group.get(tenant) {
                totals.entry(group.as_str()).or_insert((0, 0)).1 += queue.len;
            }
        }
        let mut groups: Vec<GroupSample> = totals
            .into_iter()
            .map(|(name, (in_flight, queued))| GroupSample {
                name: name.to_string(),
                in_flight,
                queued,
            })
            .collect();
        groups.sort_by(|a, b| a.name.cmp(&b.name));
        groups
    }

    /// `headroom` is the global ceiling's free slots, so group caps and
    /// borrowing are reported against the capacity actually enforced.
    fn snapshot(&self, headroom: usize) -> ModelPoolFairshare {
        let ids: HashSet<Uuid> = self
            .queues
            .keys()
            .copied()
            .chain(self.tenant_in_flight.keys().copied())
            .chain(self.served.keys().copied())
            .collect();
        let active = self.active_groups();
        let group_caps = self.compute_group_caps(self.effective_cap(headroom));
        let group_service = self.group_service();
        let total_group_weight: i64 = active.iter().map(|(_, w)| (*w).max(1)).sum();

        let mut names: HashSet<String> = active.iter().map(|(n, _)| n.clone()).collect();
        for tenant in &ids {
            if let Some(g) = self.tenant_group.get(tenant) {
                names.insert(g.clone());
            }
        }

        // One pass over the tenants with state, bucketed by group: in-flight
        // and queued per group, so the loop below does not re-scan every
        // tenant for every group.
        let mut group_totals: HashMap<&str, (usize, usize)> = HashMap::new();
        for tenant in &ids {
            let Some(group) = self.tenant_group.get(tenant) else {
                continue;
            };
            let entry = group_totals.entry(group.as_str()).or_insert((0, 0));
            entry.0 += self.tenant_in_flight.get(tenant).copied().unwrap_or(0);
            entry.1 += self.queues.get(tenant).map(|q| q.len).unwrap_or(0);
        }
        let mut groups: Vec<GroupFairshare> = names
            .into_iter()
            .map(|name| {
                let weight = self.group_weight.get(&name).copied().unwrap_or(100).max(1);
                let (in_flight, queued) =
                    group_totals.get(name.as_str()).copied().unwrap_or((0, 0));
                let served_tokens = group_service.get(name.as_str()).copied().unwrap_or(0.0);
                let weight_share =
                    if total_group_weight > 0 && active.iter().any(|(n, _)| n == &name) {
                        weight as f64 / total_group_weight as f64
                    } else {
                        0.0
                    };
                let slot_cap = group_caps.get(&name).copied().unwrap_or(0);
                GroupFairshare {
                    name: name.clone(),
                    weight,
                    in_flight,
                    queued,
                    slot_cap,
                    borrowed: in_flight.saturating_sub(slot_cap),
                    served_tokens,
                    share_score: served_tokens / weight as f64,
                    weight_share,
                }
            })
            .collect();
        groups.sort_by(|a, b| a.name.cmp(&b.name));
        let group_weight_share: HashMap<String, f64> = groups
            .iter()
            .map(|g| (g.name.clone(), g.weight_share))
            .collect();

        let total_weight: i64 = ids
            .iter()
            .map(|id| self.tenant_weight.get(id).copied().unwrap_or(1).max(1))
            .sum();
        let group_weight_sum: HashMap<String, i64> =
            ids.iter().fold(HashMap::new(), |mut acc, id| {
                let g = self
                    .tenant_group
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| "default".into());
                let w = self.tenant_weight.get(id).copied().unwrap_or(1).max(1);
                *acc.entry(g).or_insert(0) += w;
                acc
            });

        let mut tenants: Vec<TenantFairshare> = ids
            .iter()
            .map(|&tenant_id| {
                let weight = self
                    .tenant_weight
                    .get(&tenant_id)
                    .copied()
                    .unwrap_or(1)
                    .max(1);
                let fairshare_group = self
                    .tenant_group
                    .get(&tenant_id)
                    .cloned()
                    .unwrap_or_else(|| "default".into());
                let served_tokens = self.served.get(&tenant_id).copied().unwrap_or(0.0);
                let weight_share = match self.algorithm {
                    FairshareAlgorithm::Weighted => {
                        if total_weight > 0 {
                            weight as f64 / total_weight as f64
                        } else {
                            0.0
                        }
                    }
                    FairshareAlgorithm::Hierarchical => {
                        let group_w = group_weight_sum
                            .get(&fairshare_group)
                            .copied()
                            .unwrap_or(weight)
                            .max(1);
                        group_weight_share
                            .get(&fairshare_group)
                            .map(|share| share * (weight as f64 / group_w as f64))
                            .unwrap_or(0.0)
                    }
                };
                TenantFairshare {
                    tenant_id,
                    fairshare_group,
                    weight,
                    max_in_flight: self.tenant_limit(&tenant_id),
                    in_flight: self.tenant_in_flight.get(&tenant_id).copied().unwrap_or(0),
                    queued: self.queues.get(&tenant_id).map(|q| q.len).unwrap_or(0),
                    served_tokens,
                    share_score: served_tokens / weight as f64,
                    weight_share,
                }
            })
            .collect();
        tenants.sort_by(|a, b| {
            a.share_score
                .partial_cmp(&b.share_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let tenant_weight_share: HashMap<Uuid, f64> = tenants
            .iter()
            .map(|t| (t.tenant_id, t.weight_share))
            .collect();

        // Keys: every key with state, entitled to a slice of its tenant's share
        // in proportion to key weight among the tenant's *active* keys.
        let key_ids: HashSet<Uuid> = self
            .key_tenant
            .keys()
            .copied()
            .chain(self.key_in_flight.keys().copied())
            .chain(self.queues.values().flat_map(|q| q.keys.keys().copied()))
            .collect();
        let key_state = |k: &Uuid| -> (usize, usize) {
            let tenant = self.key_tenant.get(k).copied();
            let queued = tenant
                .and_then(|t| self.queues.get(&t))
                .map(|q| q.key_len(k))
                .unwrap_or(0);
            (self.key_in_flight.get(k).copied().unwrap_or(0), queued)
        };
        let mut active_key_weight_by_tenant: HashMap<Uuid, i64> = HashMap::new();
        for k in &key_ids {
            let (inf, q) = key_state(k);
            if inf > 0 || q > 0 {
                if let Some(t) = self.key_tenant.get(k) {
                    *active_key_weight_by_tenant.entry(*t).or_insert(0) +=
                        self.key_weight.get(k).copied().unwrap_or(1).max(1);
                }
            }
        }
        let mut keys: Vec<KeyFairshare> = key_ids
            .iter()
            .filter_map(|&key_id| {
                let tenant_id = self.key_tenant.get(&key_id).copied()?;
                let weight = self.key_weight.get(&key_id).copied().unwrap_or(1).max(1);
                let (in_flight, queued) = key_state(&key_id);
                let served_tokens = self.key_served.get(&key_id).copied().unwrap_or(0.0);
                let tenant_share = tenant_weight_share.get(&tenant_id).copied().unwrap_or(0.0);
                let weight_share = match active_key_weight_by_tenant.get(&tenant_id) {
                    Some(total) if *total > 0 && (in_flight > 0 || queued > 0) => {
                        tenant_share * weight as f64 / *total as f64
                    }
                    _ => 0.0,
                };
                Some(KeyFairshare {
                    key_id,
                    tenant_id,
                    weight,
                    max_in_flight: self.key_limit(&key_id),
                    in_flight,
                    queued,
                    served_tokens,
                    share_score: served_tokens / weight as f64,
                    weight_share,
                })
            })
            .collect();
        keys.sort_by(|a, b| {
            a.share_score
                .partial_cmp(&b.share_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        ModelPoolFairshare {
            model: self.model.clone(),
            cap: self.cap,
            configured_cap: self.configured_cap,
            in_flight: self.in_flight,
            cluster_in_flight: self.cluster_in_flight,
            queued: self.queued_total,
            borrowed: groups.iter().map(|g| g.borrowed).sum(),
            groups,
            tenants,
            keys,
        }
    }
}

/// Shared-slot state of the scheduler (see [`shared`]).
struct SharedCtl {
    backend: Arc<dyn SharedSlots>,
    config: SharedSlotsConfig,
    /// A reconcile covering every local permit succeeded, and every grant
    /// and release since went through the backend. Shared mode needs it.
    synced: bool,
    /// The last backend call failed; logged once until a reconcile works.
    failing: bool,
    /// Acquires and releases outstanding with the backend.
    ops: usize,
    /// A reconcile is due; in shared mode no claim is sent until it is done.
    want_reconcile: bool,
    reconciling: Option<Reconciling>,
    /// Releases held back while a quiesced reconcile is outstanding: the
    /// holdings it sent still count them.
    deferred: Vec<SlotRelease>,
    /// Bumped on every local grant or release the backend does not see, so
    /// a reconcile that was not quiesced can tell whether its holdings were
    /// still current when it landed.
    unrecorded: u64,
    /// The global ceiling is full cluster-wide; wait for a wakeup.
    global_blocked: bool,
    retry_at: Option<Instant>,
    next_reconcile: Instant,
    cluster_in_flight: Option<usize>,
}

#[derive(Debug, Clone, Copy)]
struct Reconciling {
    /// Taken with no claim or release in flight and nothing admitted while
    /// it runs, so its holdings are exact when it lands.
    quiesced: bool,
    unrecorded_at: u64,
}

struct Scheduler {
    algorithm: FairshareAlgorithm,
    capacity: Arc<dyn CapacityProvider>,
    default_model_cap: usize,
    /// Live gateway replicas.
    replicas: usize,
    /// What configured limits are divided by in the current mode.
    divisor: usize,
    mode: SlotMode,
    shared: Option<SharedCtl>,
    next_op_id: u64,
    /// Configured pool sizes set with [`FairShare::set_model_caps`], which win
    /// over the cap an admission carries.
    model_caps: HashMap<String, usize>,
    pools: HashMap<PoolKey, Pool>,
    /// Insertion order of pools, for round-robin when the ceiling binds.
    pool_order: Vec<PoolKey>,
    cursor: usize,
    in_flight: usize,
    queued_total: usize,
    idle_pool_ttl: Duration,
    ctl_tx: mpsc::UnboundedSender<Ctl>,
    stats: Arc<Stats>,
    model_load: Arc<RwLock<HashMap<String, usize>>>,
}

impl Scheduler {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Ctl>) {
        // Housekeeping runs on its own timer, not on `Ctl::Sample`: the
        // history sampler is optional, and pruning must not depend on it.
        let mut housekeeping = tokio::time::interval(housekeeping_interval(self.idle_pool_ttl));
        housekeeping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            let deadline = self.shared_deadline();
            tokio::select! {
                msg = rx.recv() => {
                    let Some(msg) = msg else { break };
                    self.handle(msg);
                }
                _ = housekeeping.tick() => {
                    self.sweep();
                    self.publish_stats();
                }
                _ = tokio::time::sleep_until(deadline.unwrap_or_else(far_future).into()),
                    if deadline.is_some() => {
                    self.shared_timer();
                }
            }
        }
    }

    /// The global ceiling as this replica enforces it.
    fn ceiling(&self) -> usize {
        replica_share(self.capacity.max_in_flight(), self.divisor)
    }

    /// The mode the current state calls for (see [`SlotMode`]).
    fn wanted_mode(&self) -> SlotMode {
        match &self.shared {
            None if self.replicas > 1 => SlotMode::Split,
            None => SlotMode::Local,
            Some(_) if self.replicas <= 1 => SlotMode::Local,
            Some(s) if s.synced => SlotMode::Shared,
            Some(_) => SlotMode::Fallback,
        }
    }

    /// Move to the mode the state calls for, resizing pools when what the
    /// limits are divided by changes. Held permits are never touched.
    fn apply_mode(&mut self) {
        let mode = self.wanted_mode();
        let split = self
            .shared
            .as_ref()
            .map(|s| s.config.split_on_fallback)
            .unwrap_or(true);
        let divisor = match mode {
            SlotMode::Shared | SlotMode::Local => 1,
            SlotMode::Split => self.replicas,
            SlotMode::Fallback if split => self.replicas,
            SlotMode::Fallback => 1,
        };
        if divisor != self.divisor {
            self.divisor = divisor;
            for pool in self.pools.values_mut() {
                pool.set_divisor(divisor);
            }
        }
        if mode != self.mode {
            if self.mode == SlotMode::Shared {
                // Refusals and cluster counts mean nothing outside shared mode.
                for pool in self.pools.values_mut() {
                    pool.unblock();
                    pool.probing = false;
                    pool.cluster_in_flight = None;
                }
                if let Some(s) = self.shared.as_mut() {
                    s.global_blocked = false;
                    s.retry_at = None;
                    s.cluster_in_flight = None;
                }
            }
            if self.shared.is_some() {
                tracing::info!(
                    from = self.mode.as_str(),
                    to = mode.as_str(),
                    replicas = self.replicas,
                    "fairshare slot mode changed"
                );
            }
            self.mode = mode;
            // Published now rather than with the rest of the stats at the end
            // of the loop turn, so a caller admitted under the new mode never
            // reads the old one.
            self.stats.mode.store(mode.to_u8(), Ordering::Relaxed);
        }
    }

    fn handle(&mut self, msg: Ctl) {
        match msg {
            Ctl::Admit {
                pool: key,
                req,
                respond,
                enqueued,
            } => {
                let ceiling = self.ceiling();
                let headroom = ceiling.saturating_sub(self.in_flight);
                let default_cap = self.default_model_cap;
                let override_cap = match &key {
                    PoolKey::Model(name) => self.model_caps.get(name).copied(),
                    PoolKey::Unrouted => None,
                };
                let shared_mode = self.mode == SlotMode::Shared;
                let quiescing = self.quiescing();
                let pool = self.pool_mut(&key);
                pool.set_configured_cap(match key {
                    // No route means no configured cap to honour.
                    PoolKey::Unrouted => default_cap,
                    PoolKey::Model(_) => override_cap.unwrap_or_else(|| {
                        req.model_max_in_flight
                            .filter(|c| *c > 0)
                            .unwrap_or(default_cap)
                    }),
                });
                pool.idle_since = None;
                pool.track_meta(&req);
                let effective = pool.effective_cap(headroom);
                let fast = headroom > 0 && pool.can_grant_immediately(&req, effective);
                if shared_mode {
                    // Every admission takes a cluster-wide slot first.
                    pool.enqueue(req, respond, enqueued, fast);
                    self.queued_total += 1;
                    self.dispatch_all();
                } else if fast && !quiescing {
                    pool.grant_fast(req, respond);
                    self.in_flight += 1;
                    self.note_unrecorded();
                } else {
                    pool.enqueue(req, respond, enqueued, false);
                    self.queued_total += 1;
                    self.dispatch_all();
                }
                self.publish_model_load(&key);
                self.publish_stats();
            }
            Ctl::Release {
                tenant,
                key,
                pool: pool_key,
            } => {
                self.in_flight = self.in_flight.saturating_sub(1);
                if let Some(pool) = self.pools.get_mut(&pool_key) {
                    pool.release(tenant, key);
                    pool.settle_idle();
                }
                if self.mode == SlotMode::Shared || self.quiesced_reconcile_running() {
                    self.release_shared(SlotRelease {
                        pool: pool_key.slot_id(),
                        tenant,
                        key,
                    });
                } else {
                    self.note_unrecorded();
                }
                self.publish_model_load(&pool_key);
                self.dispatch_all();
                self.publish_stats();
            }
            Ctl::Snapshot { respond } => {
                let _ = respond.send(self.build_snapshot());
            }
            Ctl::Sample { respond } => {
                let _ = respond.send(self.build_sample());
            }
            Ctl::SetReplicas(replicas) => {
                if replicas == self.replicas {
                    return;
                }
                let was_multi = self.replicas > 1;
                self.replicas = replicas;
                if let Some(s) = self.shared.as_mut() {
                    if was_multi != (replicas > 1) {
                        // Grants made alone were never recorded, and grants
                        // from here on alone will not be: resync on the way
                        // into shared mode.
                        s.synced = false;
                    }
                    if replicas > 1 && !s.synced {
                        s.want_reconcile = true;
                    }
                }
                self.apply_mode();
                self.maybe_start_reconcile();
                // Growing frees slots for whoever is queued; shrinking finds
                // nothing to dispatch and leaves every held permit alone.
                self.dispatch_all();
                self.publish_stats();
            }
            Ctl::SetModelCaps(caps) => {
                if caps == self.model_caps {
                    return;
                }
                for (key, pool) in self.pools.iter_mut() {
                    if let PoolKey::Model(name) = key {
                        // A pool whose override was dropped keeps its size
                        // until its next admission brings the route's cap.
                        if let Some(cap) = caps.get(name) {
                            pool.set_configured_cap(*cap);
                        }
                    }
                }
                self.model_caps = caps;
                // Same as a replica-count change: growth dispatches waiters,
                // a shrink leaves every held permit alone.
                self.dispatch_all();
                self.publish_stats();
            }
            Ctl::EnableShared { backend, config } => {
                if self.shared.is_some() {
                    return;
                }
                self.shared = Some(SharedCtl {
                    backend,
                    config,
                    synced: false,
                    failing: false,
                    ops: 0,
                    want_reconcile: true,
                    reconciling: None,
                    deferred: Vec::new(),
                    unrecorded: 0,
                    global_blocked: false,
                    retry_at: None,
                    next_reconcile: Instant::now(),
                    cluster_in_flight: None,
                });
                self.apply_mode();
                self.maybe_start_reconcile();
                self.dispatch_all();
                self.publish_stats();
            }
            Ctl::Wake(pool) => {
                if self.mode != SlotMode::Shared {
                    return;
                }
                match pool {
                    Some(key) => {
                        if let Some(pool) = self.pools.get_mut(&key) {
                            pool.unblock();
                        }
                    }
                    None => self.unblock_all(),
                }
                if let Some(s) = self.shared.as_mut() {
                    s.global_blocked = false;
                }
                self.dispatch_all();
                self.publish_stats();
            }
            Ctl::AcquireDone { pool, id, result } => {
                self.acquire_done(pool, id, result);
                self.publish_stats();
            }
            Ctl::ReleaseDone { pool, result } => {
                self.release_done(pool, result);
                self.publish_stats();
            }
            Ctl::ReconcileDone { result } => {
                self.reconcile_done(result);
                self.publish_stats();
            }
        }
    }

    // ---- shared slots -----------------------------------------------------

    /// Whether admissions are held for a quiesced reconcile: one is due or
    /// running with more than one replica live and the backend answering.
    fn quiescing(&self) -> bool {
        let multi = self.replicas > 1;
        self.shared.as_ref().is_some_and(|s| {
            (s.want_reconcile && multi && !s.failing) || s.reconciling.is_some_and(|r| r.quiesced)
        })
    }

    fn quiesced_reconcile_running(&self) -> bool {
        self.shared
            .as_ref()
            .is_some_and(|s| s.reconciling.is_some_and(|r| r.quiesced))
    }

    /// A local grant or release the backend does not see.
    fn note_unrecorded(&mut self) {
        if let Some(s) = self.shared.as_mut() {
            s.unrecorded = s.unrecorded.wrapping_add(1);
        }
    }

    fn unblock_all(&mut self) {
        for pool in self.pools.values_mut() {
            pool.unblock();
        }
        if let Some(s) = self.shared.as_mut() {
            s.global_blocked = false;
        }
    }

    /// When the shared-slot timer next needs to run: a refused pool's retry
    /// or the next reconcile.
    fn shared_deadline(&self) -> Option<Instant> {
        let s = self.shared.as_ref()?;
        let reconcile = (s.reconciling.is_none() && !s.want_reconcile).then_some(s.next_reconcile);
        match (s.retry_at, reconcile) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn shared_timer(&mut self) {
        let now = Instant::now();
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        let mut retry = false;
        if s.retry_at.is_some_and(|t| t <= now) {
            s.retry_at = None;
            retry = true;
        }
        if s.reconciling.is_none() && !s.want_reconcile && s.next_reconcile <= now {
            s.want_reconcile = true;
            s.next_reconcile = now + s.config.reconcile_interval;
        }
        if retry {
            // Correctness never depends on a wakeup arriving: retry anyway.
            self.unblock_all();
        }
        self.maybe_start_reconcile();
        self.dispatch_all();
        self.publish_stats();
    }

    /// Arm the jittered retry for refused pools, if it is not armed yet.
    fn schedule_retry(&mut self) {
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        if s.retry_at.is_some() {
            return;
        }
        let (lo, hi) = (
            s.config.retry_min,
            s.config.retry_max.max(s.config.retry_min),
        );
        let span = (hi - lo).as_millis() as u64;
        let jitter = if span == 0 {
            0
        } else {
            rand::random::<u64>() % (span + 1)
        };
        s.retry_at = Some(Instant::now() + lo + Duration::from_millis(jitter));
    }

    /// Shared mode: send claims for the waiters the local order picks, round
    /// robin across pools like [`Scheduler::dispatch_all`]. Each pool keeps at
    /// most `max_pending_per_pool` claims outstanding, or one after a refusal.
    fn dispatch_shared(&mut self) {
        if self.quiescing() {
            return;
        }
        let Some(s) = self.shared.as_ref() else {
            return;
        };
        // Wait out a full ceiling.
        if s.global_blocked {
            return;
        }
        let max_pending = s.config.max_pending_per_pool.max(1);
        let global_cap = self.capacity.max_in_flight();
        let n = self.pool_order.len();
        if n == 0 {
            return;
        }
        let ceiling = self.ceiling();
        let mut claims: Vec<(PoolKey, u64, SlotClaim)> = Vec::new();
        let mut progressed = true;
        while progressed && self.in_flight < ceiling && self.queued_total > 0 {
            progressed = false;
            for i in 0..n {
                if self.in_flight >= ceiling {
                    break;
                }
                let idx = (self.cursor + i) % n;
                let headroom = ceiling.saturating_sub(self.in_flight);
                let key = self.pool_order[idx].clone();
                let Some(pool) = self.pools.get_mut(&key) else {
                    continue;
                };
                let limit = if pool.probing { 1 } else { max_pending };
                if pool.queued_total == 0 || pool.blocked || pool.pending.len() >= limit {
                    continue;
                }
                let effective = pool.effective_cap(headroom);
                let (next, dropped) = pool.take_next(effective);
                if dropped > 0 {
                    pool.settle_idle();
                }
                self.queued_total = self.queued_total.saturating_sub(dropped);
                let Some((tenant, key_id, waiter)) = next else {
                    continue;
                };
                let id = self.next_op_id;
                self.next_op_id += 1;
                let claim = SlotClaim {
                    pool: key.slot_id(),
                    tenant,
                    key: key_id,
                    pool_cap: pool.configured_cap,
                    global_cap,
                    tenant_cap: pool.tenant_cap.get(&tenant).copied(),
                    key_cap: pool.key_cap.get(&key_id).copied(),
                };
                pool.pending.insert(
                    id,
                    PendingClaim {
                        tenant,
                        key: key_id,
                        waiter,
                    },
                );
                self.in_flight += 1;
                self.queued_total = self.queued_total.saturating_sub(1);
                claims.push((key, id, claim));
                progressed = true;
            }
            self.cursor = (self.cursor + 1) % n;
        }
        for (pool, id, claim) in claims {
            self.publish_model_load(&pool);
            self.spawn_acquire(pool, id, claim);
        }
    }

    fn spawn_acquire(&mut self, pool: PoolKey, id: u64, claim: SlotClaim) {
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        s.ops += 1;
        let fut = s.backend.acquire(claim);
        let timeout = s.config.op_timeout;
        let tx = self.ctl_tx.clone();
        tokio::spawn(async move {
            let result = bounded(timeout, fut).await;
            let _ = tx.send(Ctl::AcquireDone { pool, id, result });
        });
    }

    fn release_shared(&mut self, slot: SlotRelease) {
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        if s.reconciling.is_some_and(|r| r.quiesced) {
            s.deferred.push(slot);
            return;
        }
        self.spawn_release(slot);
    }

    fn spawn_release(&mut self, slot: SlotRelease) {
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        s.ops += 1;
        let pool = PoolKey::from_slot_id(&slot.pool).unwrap_or(PoolKey::Unrouted);
        let fut = s.backend.release(slot);
        let timeout = s.config.op_timeout;
        let tx = self.ctl_tx.clone();
        tokio::spawn(async move {
            let result = bounded(timeout, fut).await;
            let _ = tx.send(Ctl::ReleaseDone { pool, result });
        });
    }

    fn acquire_done(
        &mut self,
        key: PoolKey,
        id: u64,
        result: Result<(AcquireOutcome, ClusterCounts), SlotError>,
    ) {
        if let Some(s) = self.shared.as_mut() {
            s.ops = s.ops.saturating_sub(1);
        }
        let Some(pool) = self.pools.get_mut(&key) else {
            return;
        };
        let Some(claim) = pool.pending.remove(&id) else {
            return;
        };
        match result {
            Ok((AcquireOutcome::Granted, counts)) => {
                pool.probing = false;
                pool.cluster_in_flight = Some(counts.pool);
                let PendingClaim {
                    tenant,
                    key: key_id,
                    waiter,
                } = claim;
                let admission = if waiter.fast {
                    Admission::Fast
                } else {
                    Admission::Queued
                };
                let waited = waiter.enqueued.elapsed();
                pool.send(tenant, key_id, waiter.respond, admission, waited);
                if let Some(s) = self.shared.as_mut() {
                    s.cluster_in_flight = Some(counts.global);
                }
                if self.mode != SlotMode::Shared {
                    // Granted by the backend after this replica left shared
                    // mode: the next reconcile settles it.
                    self.note_unrecorded();
                }
            }
            Ok((AcquireOutcome::Denied(denial), counts)) => {
                let (tenant, key_id) = (claim.tenant, claim.key);
                pool.requeue(claim);
                pool.probing = true;
                pool.cluster_in_flight = Some(counts.pool);
                match denial {
                    SlotDenial::Pool => pool.blocked = true,
                    SlotDenial::Tenant => {
                        pool.blocked_tenants.insert(tenant);
                    }
                    SlotDenial::Key => {
                        pool.blocked_keys.insert(key_id);
                    }
                    SlotDenial::Global => {}
                }
                self.in_flight = self.in_flight.saturating_sub(1);
                self.queued_total += 1;
                if let Some(s) = self.shared.as_mut() {
                    s.cluster_in_flight = Some(counts.global);
                    if denial == SlotDenial::Global {
                        s.global_blocked = true;
                    }
                }
                self.schedule_retry();
            }
            Ok((AcquireOutcome::NotRegistered, _)) => {
                pool.requeue(claim);
                self.in_flight = self.in_flight.saturating_sub(1);
                self.queued_total += 1;
                self.shared_failed("this gateway is not registered as live in the shared backend");
            }
            Err(e) => {
                pool.requeue(claim);
                self.in_flight = self.in_flight.saturating_sub(1);
                self.queued_total += 1;
                self.shared_failed(&e.to_string());
            }
        }
        self.publish_model_load(&key);
        self.maybe_start_reconcile();
        self.dispatch_all();
    }

    fn release_done(&mut self, key: PoolKey, result: Result<ClusterCounts, SlotError>) {
        if let Some(s) = self.shared.as_mut() {
            s.ops = s.ops.saturating_sub(1);
        }
        match result {
            Ok(counts) => {
                if self.mode == SlotMode::Shared {
                    if let Some(pool) = self.pools.get_mut(&key) {
                        pool.cluster_in_flight = Some(counts.pool);
                        // A claim sent before this release landed may have
                        // been refused for the slot it frees.
                        pool.unblock();
                    }
                    if let Some(s) = self.shared.as_mut() {
                        s.cluster_in_flight = Some(counts.global);
                        s.global_blocked = false;
                    }
                }
            }
            Err(e) => self.shared_failed(&e.to_string()),
        }
        self.maybe_start_reconcile();
        self.dispatch_all();
    }

    /// A backend call failed: fall back to the split until a reconcile gets
    /// through. Logged once per outage.
    fn shared_failed(&mut self, error: &str) {
        let replicas = self.replicas;
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        if !s.failing {
            s.failing = true;
            tracing::warn!(
                error,
                replicas,
                split = s.config.split_on_fallback,
                "shared fairshare slots unavailable; enforcing each limit per replica until \
                 the backend answers and holdings are reconciled"
            );
        }
        s.synced = false;
        for _ in s.deferred.drain(..) {
            s.unrecorded = s.unrecorded.wrapping_add(1);
        }
        s.next_reconcile = Instant::now() + s.config.recovery_interval;
        self.apply_mode();
    }

    /// Start a due reconcile once nothing else is in flight. In shared mode
    /// (and on the way into it with the backend answering) it is quiesced:
    /// no claim is sent until it lands and releases wait behind it, so the
    /// holdings it records are exact. Alone or while failing, it runs
    /// alongside local admissions and only counts as a resync if nothing
    /// changed locally meanwhile.
    fn maybe_start_reconcile(&mut self) {
        let multi = self.replicas > 1;
        let Some(s) = self.shared.as_ref() else {
            return;
        };
        if !s.want_reconcile || s.reconciling.is_some() || s.ops > 0 {
            return;
        }
        let holdings: Vec<PoolHoldings> = self.pools.values().filter_map(Pool::holdings).collect();
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        s.want_reconcile = false;
        s.reconciling = Some(Reconciling {
            quiesced: multi && !s.failing,
            unrecorded_at: s.unrecorded,
        });
        let fut = s.backend.reconcile(holdings);
        let timeout = s.config.op_timeout;
        let tx = self.ctl_tx.clone();
        tokio::spawn(async move {
            let result = bounded(timeout, fut).await;
            let _ = tx.send(Ctl::ReconcileDone { result });
        });
    }

    fn reconcile_done(&mut self, result: Result<ReconcileOutcome, SlotError>) {
        let multi = self.replicas > 1;
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        let Some(run) = s.reconciling.take() else {
            return;
        };
        match result {
            Ok(ReconcileOutcome::Synced) => {
                if s.failing {
                    s.failing = false;
                    tracing::info!("shared fairshare slots reachable again; holdings reconciled");
                }
                if multi {
                    if run.quiesced || run.unrecorded_at == s.unrecorded {
                        s.synced = true;
                    } else {
                        // Local admissions moved on while it ran; now that
                        // the backend answers, a quiesced one settles it.
                        s.want_reconcile = true;
                    }
                }
                s.next_reconcile = Instant::now() + s.config.reconcile_interval;
                let deferred = std::mem::take(&mut s.deferred);
                let send = s.synced && multi;
                self.apply_mode();
                for slot in deferred {
                    if send {
                        self.spawn_release(slot);
                    } else {
                        self.note_unrecorded();
                    }
                }
                // The reconcile may have freed slots this replica was refused.
                self.unblock_all();
            }
            Ok(ReconcileOutcome::NotRegistered) => {
                self.shared_failed("this gateway is not registered as live in the shared backend");
            }
            Err(e) => self.shared_failed(&e.to_string()),
        }
        self.maybe_start_reconcile();
        self.dispatch_all();
    }

    fn pool_mut(&mut self, key: &PoolKey) -> &mut Pool {
        if !self.pools.contains_key(key) {
            let mut pool = Pool::new(
                key.clone(),
                self.algorithm,
                self.default_model_cap,
                self.ctl_tx.clone(),
            );
            pool.set_divisor(self.divisor);
            self.pools.insert(key.clone(), pool);
            self.pool_order.push(key.clone());
        }
        self.pools.get_mut(key).expect("pool just inserted")
    }

    /// Periodic housekeeping, run on its own timer: drop waiters whose
    /// caller has gone, then drop pools that have been idle for the TTL.
    fn sweep(&mut self) {
        let mut dropped = 0;
        for pool in self.pools.values_mut() {
            dropped += pool.prune_dead_waiters();
            pool.settle_idle();
        }
        self.queued_total = self.queued_total.saturating_sub(dropped);
        if dropped > 0 {
            self.dispatch_all();
        }

        let ttl = self.idle_pool_ttl;
        // Only a pool with no permit and no waiter is removed: nothing can
        // still send a `Release` for it or be waiting on a grant from it.
        let expired: Vec<PoolKey> = self
            .pools
            .iter()
            .filter(|(_, p)| p.is_idle() && p.idle_since.is_some_and(|t| t.elapsed() >= ttl))
            .map(|(k, _)| k.clone())
            .collect();
        if expired.is_empty() {
            return;
        }
        for key in &expired {
            self.pools.remove(key);
            self.publish_model_load(key);
        }
        self.pool_order.retain(|k| self.pools.contains_key(k));
        self.cursor = if self.pool_order.is_empty() {
            0
        } else {
            self.cursor % self.pool_order.len()
        };
    }

    /// Serve queued waiters across pools. Each pass grants at most one slot per
    /// pool starting from a rotating cursor, so a binding ceiling is shared
    /// round-robin rather than by map order.
    fn dispatch_all(&mut self) {
        if self.mode == SlotMode::Shared {
            self.dispatch_shared();
            return;
        }
        if self.quiescing() {
            return;
        }
        let n = self.pool_order.len();
        if n == 0 {
            return;
        }
        let ceiling = self.ceiling();
        let mut progressed = true;
        while progressed && self.in_flight < ceiling && self.queued_total > 0 {
            progressed = false;
            for i in 0..n {
                if self.in_flight >= ceiling {
                    break;
                }
                let idx = (self.cursor + i) % n;
                let headroom = ceiling.saturating_sub(self.in_flight);
                let (granted, dropped) = match self.pools.get_mut(&self.pool_order[idx]) {
                    Some(pool) if pool.queued_total > 0 => {
                        let effective = pool.effective_cap(headroom);
                        let outcome = pool.dispatch_one(effective);
                        if outcome.1 > 0 {
                            pool.settle_idle();
                        }
                        outcome
                    }
                    _ => (false, 0),
                };
                self.queued_total = self.queued_total.saturating_sub(dropped);
                if granted {
                    self.in_flight += 1;
                    self.queued_total = self.queued_total.saturating_sub(1);
                    self.note_unrecorded();
                    progressed = true;
                    let key = self.pool_order[idx].clone();
                    self.publish_model_load(&key);
                }
            }
            self.cursor = (self.cursor + 1) % n;
        }
    }

    fn publish_model_load(&self, key: &PoolKey) {
        // The unrouted pool is not a model the router could pick.
        let PoolKey::Model(model) = key else {
            return;
        };
        if let Ok(mut shared) = self.model_load.write() {
            match self.pools.get(key).map(|p| p.in_flight) {
                Some(n) if n > 0 => {
                    shared.insert(model.clone(), n);
                }
                _ => {
                    shared.remove(model);
                }
            }
        }
    }

    fn publish_stats(&self) {
        self.stats
            .in_flight
            .store(self.in_flight, Ordering::Relaxed);
        self.stats
            .queued
            .store(self.queued_total as i64, Ordering::Relaxed);
        self.stats.replicas.store(self.replicas, Ordering::Relaxed);
        self.stats.mode.store(self.mode.to_u8(), Ordering::Relaxed);
        let cluster = self
            .shared
            .as_ref()
            .and_then(|s| s.cluster_in_flight)
            .filter(|_| self.mode == SlotMode::Shared)
            .unwrap_or(self.in_flight);
        self.stats
            .cluster_in_flight
            .store(cluster, Ordering::Relaxed);
    }

    /// A [`FairshareSample`] built directly from pool state, without the
    /// tenant/key views [`Scheduler::build_snapshot`] computes. Keeps the
    /// history sampler from forcing that full walk every tick. Idle pools
    /// carry nothing to chart and are left out.
    fn build_sample(&self) -> FairshareSample {
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let pools = self
            .pool_order
            .iter()
            .filter_map(|key| self.pools.get(key))
            .filter(|pool| !pool.is_idle())
            .map(|pool| PoolSample {
                model: pool.model.clone(),
                cap: pool.cap,
                in_flight: pool.in_flight,
                queued: pool.queued_total,
                groups: pool.group_totals(),
            })
            .collect();
        FairshareSample {
            ts_ms,
            global_in_flight: self.in_flight,
            global_queued: self.queued_total,
            pools,
        }
    }

    fn build_snapshot(&self) -> FairshareSnapshot {
        let ceiling = self.ceiling();
        let headroom = ceiling.saturating_sub(self.in_flight);
        let mut pools: Vec<ModelPoolFairshare> =
            self.pools.values().map(|p| p.snapshot(headroom)).collect();
        pools.sort_by(|a, b| a.model.cmp(&b.model));
        let model_in_flight = pools
            .iter()
            .filter(|p| p.in_flight > 0)
            .map(|p| (p.model.clone(), p.in_flight))
            .collect();
        let model_queued = pools
            .iter()
            .filter(|p| p.queued > 0)
            .map(|p| (p.model.clone(), p.queued))
            .collect();
        FairshareSnapshot {
            algorithm: self.algorithm.as_str().into(),
            max_in_flight: ceiling,
            configured_max_in_flight: self.capacity.max_in_flight(),
            replicas: self.replicas,
            mode: self.mode.as_str().into(),
            shared_slots: self.shared.is_some(),
            cluster_in_flight: self
                .shared
                .as_ref()
                .and_then(|s| s.cluster_in_flight)
                .filter(|_| self.mode == SlotMode::Shared),
            default_model_max_in_flight: self.default_model_cap,
            global_in_flight: self.in_flight,
            global_queued: self.queued_total,
            global_borrowed: pools.iter().map(|p| p.borrowed).sum(),
            pools,
            model_in_flight,
            model_queued,
        }
    }
}

/// Far enough ahead to stand for "never" in a `sleep_until`.
fn far_future() -> Instant {
    Instant::now() + Duration::from_secs(86_400)
}

/// A backend call bounded by `timeout`; running past it is a failure.
async fn bounded<T>(timeout: Duration, fut: SlotFuture<T>) -> Result<T, SlotError> {
    match tokio::time::timeout(timeout, fut).await {
        Ok(result) => result,
        Err(_) => Err(SlotError(format!(
            "shared slot backend did not answer within {timeout:?}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Group standing is actual service, not tenant count: tenants that join
    /// after virtual time has moved each start at that time, and summing raw
    /// `served` would charge a many-tenant group for it once per tenant.
    #[test]
    fn many_tenant_group_ties_one_tenant_group_on_equal_service() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut pool = Pool::new(
            PoolKey::Model("m".into()),
            FairshareAlgorithm::Hierarchical,
            8,
            tx,
        );
        pool.virtual_time = 50.0;

        for _ in 0..4 {
            let t = Uuid::new_v4();
            pool.track_meta(&AdmitRequest::new(t, "m", 10).group("many", 100));
            pool.occupy(t, t, 10);
        }
        let solo = Uuid::new_v4();
        pool.track_meta(&AdmitRequest::new(solo, "m", 10).group("one", 100));
        for _ in 0..4 {
            pool.occupy(solo, solo, 10);
        }

        let service = pool.group_service();
        assert_eq!(service["many"], 40.0);
        assert_eq!(service["one"], 40.0);

        let snap = pool.snapshot(usize::MAX);
        let score = |name: &str| {
            snap.groups
                .iter()
                .find(|g| g.name == name)
                .map(|g| g.share_score)
                .unwrap()
        };
        assert_eq!(score("many"), score("one"));
    }
}
