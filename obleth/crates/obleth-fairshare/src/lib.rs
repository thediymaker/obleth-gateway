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
//! next) is always decided per replica. [`FairShare::snapshot`] is this
//! replica's own state; in shared mode [`FairShare::cluster_snapshot`] merges
//! every live replica's (see [`view`]). A resize never touches in-flight
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
//! # Session affinity
//! With [`FairShare::set_session_affinity`], a pool favours agent sessions
//! whose context is still cached upstream: a warm session goes ahead of older
//! requests from the same key, and the slot a step frees can be held briefly
//! for the session's next step (see [`affinity`]). A held slot counts like a
//! taken one, against the pool, the global ceiling, the tenant and the key.
//! In shared mode it is held in the backend, so the step can take it on any
//! replica: a claim names the session's hold, and a request queued behind a
//! full pool makes one claim for that held slot alone.
//!
//! Requests with no resolved route share one [`PoolKey::Unrouted`] pool. A
//! pool with no permits and no waiters for [`IDLE_POOL_TTL`] is dropped by the
//! scheduler's housekeeping timer, which also drops queued waiters whose
//! caller has gone.

mod affinity;
mod algorithm;
mod capacity;
pub mod history;
mod replicas;
pub mod shared;
pub mod view;

pub use affinity::AffinityStats;
pub use algorithm::{
    group_slot_caps, pool_shares, weighted_caps, PoolShares, ShareKey, ShareTenant,
};
pub use capacity::{CapacityProvider, StaticCapacity};
pub use history::{
    FairshareHistory, FairshareSample, GroupSample, HistoryPoint, PoolSample,
    FAIRSHARE_HISTORY_INTERVAL_MS,
};
pub use replicas::{replica_share, ReplicaTracker};
pub use shared::{
    AcquireOutcome, ClusterCounts, InMemoryCluster, InMemorySlots, PoolHoldings, ReconcileOutcome,
    SharedSlots, SharedSlotsConfig, SlotClaim, SlotDenial, SlotError, SlotFuture, SlotHold,
    SlotMode, SlotRelease,
};
pub use view::{
    merge_views, ClusterSnapshot, PeerView, PeerViews, ReplicaGroupView, ReplicaKeyView,
    ReplicaPoolView, ReplicaTenantView, ReplicaView,
};

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use obleth_config::{Admission, FairshareAlgorithm, SessionAffinityConfig};
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

/// Most idle keys a pool keeps per tenant for the lead they have over the
/// tenant's key clock. Past it, the key with the smallest lead is forgotten,
/// which forgives that lead.
const MAX_IDLE_KEYS_PER_TENANT: usize = 512;

/// Housekeeping period: a sixtieth of the idle TTL, kept between 1 and 10 s.
fn housekeeping_interval(idle_pool_ttl: Duration) -> Duration {
    (idle_pool_ttl / 60).clamp(Duration::from_secs(1), Duration::from_secs(10))
}

/// Which scheduling pool an admission lands in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PoolKey {
    /// The pool of a resolved model route.
    Model(String),
    /// Background work (batches) for a model. It has its own queues and
    /// fairness, and shares the model's slots with [`PoolKey::Model`]: it is
    /// served only while the model has no interactive request waiting, and
    /// only up to a share of the model's pool (see
    /// [`AdmitRequest::background_share`]).
    Background(String),
    /// One pool shared by every request that did not resolve to a route, so
    /// arbitrary model strings cannot each mint a pool of their own.
    Unrouted,
}

impl PoolKey {
    fn display_name(&self) -> String {
        match self {
            PoolKey::Model(name) => name.clone(),
            PoolKey::Background(name) => format!("{name} (batch)"),
            PoolKey::Unrouted => UNROUTED_POOL.to_string(),
        }
    }

    /// The pool's id in the shared slot backend, the same on every replica:
    /// `m:` and the model name, `b:` and the model name for its background
    /// pool, or `u` for the unrouted pool.
    pub fn slot_id(&self) -> String {
        match self {
            PoolKey::Model(name) => format!("m:{name}"),
            PoolKey::Background(name) => format!("b:{name}"),
            PoolKey::Unrouted => "u".into(),
        }
    }

    /// The inverse of [`PoolKey::slot_id`].
    pub fn from_slot_id(id: &str) -> Option<Self> {
        if id == "u" {
            return Some(PoolKey::Unrouted);
        }
        if let Some(m) = id.strip_prefix("m:") {
            return Some(PoolKey::Model(m.to_string()));
        }
        id.strip_prefix("b:")
            .map(|m| PoolKey::Background(m.to_string()))
    }

    /// Whether this is a model's background pool.
    pub fn is_background(&self) -> bool {
        matches!(self, PoolKey::Background(_))
    }

    /// The model's other pool: background for interactive and the reverse.
    /// Both draw on the same upstream capacity.
    pub fn sibling(&self) -> Option<PoolKey> {
        match self {
            PoolKey::Model(name) => Some(PoolKey::Background(name.clone())),
            PoolKey::Background(name) => Some(PoolKey::Model(name.clone())),
            PoolKey::Unrouted => None,
        }
    }
}

/// The share of a model's pool, and of the global ceiling, that background
/// work may fill when the request does not say.
pub const DEFAULT_BACKGROUND_SHARE: f64 = 0.75;

/// `share` of `cap`, rounded down, for a background limit.
pub fn background_cap(cap: usize, share: f64) -> usize {
    let share = if share.is_finite() {
        share.clamp(0.0, 1.0)
    } else {
        DEFAULT_BACKGROUND_SHARE
    };
    (cap as f64 * share).floor() as usize
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
    /// Slots this replica holds for sessions' next steps, across every pool.
    pub held: AtomicUsize,
    /// What session affinity did, as running totals.
    pub affinity: Arc<AffinityStats>,
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
    /// Service actually received by the group's tenants, excluding where
    /// each tenant was placed on joining; the scheduler ranks groups by
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
    /// Set when this entry is one end user of a key with per-end-user
    /// fairshare, so `key_id` is derived rather than a real key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_user: Option<EndUser>,
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
    /// Slots held for sessions' next steps (counted in `in_flight`).
    #[serde(default)]
    pub held: usize,
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

/// The end user a scheduler key stands for. When a key with per-end-user
/// fairshare admits on behalf of one of its users, the scheduler key is
/// derived from the real key and the end user, so each user queues on their
/// own. The scheduler treats that derived key like any other; this only lets
/// dashboards show whose it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndUser {
    /// The real API key the end user came through.
    pub parent_key: Uuid,
    /// The end-user id the caller sent.
    pub name: String,
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
    /// Set when `key` is an end user's derived key (see [`EndUser`]).
    pub end_user: Option<EndUser>,
    /// The conversation this request is a step of, for session affinity.
    pub session: Option<String>,
    /// Session affinity for the request's pool: the scheduler's setting, or
    /// the model's own.
    pub affinity: PoolAffinity,
    /// For a background pool: the share of the model's pool and of the
    /// global ceiling its work may fill. `None` is
    /// [`DEFAULT_BACKGROUND_SHARE`]. Ignored for other pools.
    pub background_share: Option<f64>,
}

/// Where a pool's session affinity setting comes from.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PoolAffinity {
    /// The scheduler's, set with [`FairShare::set_session_affinity`].
    #[default]
    Scheduler,
    /// The model's own, already resolved against the gateway's values;
    /// `None` is off.
    Model(Option<SessionAffinityConfig>),
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
            end_user: None,
            session: None,
            affinity: PoolAffinity::Scheduler,
            background_share: None,
        }
    }

    /// The share of the model's pool a background request may fill.
    pub fn background_share(mut self, share: f64) -> Self {
        self.background_share = Some(share);
        self
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

    pub fn end_user(mut self, parent_key: Uuid, name: impl Into<String>) -> Self {
        self.end_user = Some(EndUser {
            parent_key,
            name: name.into(),
        });
        self
    }

    /// The conversation id; empty means none.
    pub fn session(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        self.session = (!id.is_empty()).then_some(id);
        self
    }

    /// The model's own session affinity setting, resolved: `None` turns it
    /// off for the pool whatever the scheduler's setting.
    pub fn session_affinity(mut self, config: Option<SessionAffinityConfig>) -> Self {
        self.affinity = PoolAffinity::Model(config);
        self
    }
}

/// A held admission slot. Dropping it returns capacity to the scheduler.
pub struct Permit {
    release: Option<mpsc::UnboundedSender<Ctl>>,
    tenant: Uuid,
    key: Uuid,
    pool: PoolKey,
    session: Option<String>,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(tx) = self.release.take() {
            let _ = tx.send(Ctl::Release {
                tenant: self.tenant,
                key: self.key,
                pool: std::mem::replace(&mut self.pool, PoolKey::Unrouted),
                session: self.session.take(),
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
        req: Box<AdmitRequest>,
        respond: oneshot::Sender<Admitted>,
        enqueued: Instant,
    },
    Release {
        tenant: Uuid,
        key: Uuid,
        pool: PoolKey,
        session: Option<String>,
    },
    Snapshot {
        respond: oneshot::Sender<FairshareSnapshot>,
    },
    Sample {
        respond: oneshot::Sender<FairshareSample>,
    },
    View {
        respond: oneshot::Sender<LocalView>,
    },
    SetReplicas(usize),
    SetModelCaps(HashMap<String, usize>),
    SetSessionAffinity(Option<SessionAffinityConfig>),
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
    HoldDone {
        pool: PoolKey,
        result: Result<(bool, ClusterCounts), SlotError>,
    },
    ReconcileDone {
        result: Result<ReconcileOutcome, SlotError>,
    },
    ViewPublished {
        result: Result<(), SlotError>,
    },
}

/// This replica's view, with what a merge needs to know about it.
struct LocalView {
    view: ReplicaView,
    /// Every pool this replica has, idle ones included: (model, configured
    /// size).
    pools: Vec<(String, usize)>,
    algorithm: FairshareAlgorithm,
    /// The configured global ceiling.
    ceiling: usize,
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
            session_affinity: None,
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

    /// Shared mode: this replica's scheduler state merged with the state
    /// every other live replica last published, so queues, tenants, groups
    /// and keys are counted across the fleet (see [`view`]). Each pool's
    /// `cap` is its configured size and its `cluster_in_flight` is `None`
    /// (read the slot counts with [`FairShare::cluster_in_flight`]).
    ///
    /// Fails when shared slots are not enabled or the backend does not
    /// answer within a second; a caller then has only
    /// [`FairShare::snapshot`], this replica's own.
    pub async fn cluster_snapshot(&self) -> Result<ClusterSnapshot, SlotError> {
        let backend = self
            .shared
            .get()
            .cloned()
            .ok_or_else(|| SlotError("shared slots are not enabled".into()))?;
        let (respond, rx) = oneshot::channel();
        let gone = || SlotError("the fairshare scheduler is not running".into());
        self.ctl.send(Ctl::View { respond }).map_err(|_| gone())?;
        let local = rx.await.map_err(|_| gone())?;
        let peers = bounded(Duration::from_secs(1), backend.peer_views()).await?;
        let mut views: Vec<&ReplicaView> = vec![&local.view];
        views.extend(peers.views.iter().map(|p| &p.view));
        let reporting = views.len();
        let mut pools = merge_views(local.algorithm, &views, local.ceiling);
        // Pools idle everywhere are in no view; keep this replica's, so the
        // pools it knows of are listed whichever way the view was built.
        for (model, cap) in local.pools {
            if !pools.iter().any(|p| p.model == model) {
                pools.push(ModelPoolFairshare {
                    model,
                    cap,
                    configured_cap: cap,
                    in_flight: 0,
                    cluster_in_flight: None,
                    queued: 0,
                    borrowed: 0,
                    held: 0,
                    groups: Vec::new(),
                    tenants: Vec::new(),
                    keys: Vec::new(),
                });
            }
        }
        pools.sort_by(|a, b| a.model.cmp(&b.model));
        Ok(ClusterSnapshot {
            pools,
            replicas_live: peers.live.max(reporting),
            replicas_reporting: reporting,
            max_age_ms: peers.views.iter().map(|p| p.age_ms).max().unwrap_or(0),
        })
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

    /// Turn session affinity on (`Some`) or off for every pool; see
    /// [`affinity`]. Turning it off releases every held slot.
    pub fn set_session_affinity(&self, config: Option<SessionAffinityConfig>) {
        let _ = self.ctl.send(Ctl::SetSessionAffinity(config));
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
                req: Box::new(req),
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

    /// Shared mode: a history sample counted across every live replica,
    /// from this replica's view and the views its peers last published (the
    /// same state [`FairShare::cluster_snapshot`] reads). `None` outside
    /// shared mode or when the backend does not answer within a second; the
    /// sampler then records [`FairShare::sample`], this replica's own.
    pub async fn cluster_sample(&self) -> Option<FairshareSample> {
        if self.stats.mode() != SlotMode::Shared {
            return None;
        }
        let backend = self.shared.get().cloned()?;
        let (respond, rx) = oneshot::channel();
        self.ctl.send(Ctl::View { respond }).ok()?;
        let local = rx.await.ok()?;
        let peers = bounded(Duration::from_secs(1), backend.peer_views())
            .await
            .ok()?;
        let mut views: Vec<&ReplicaView> = vec![&local.view];
        views.extend(peers.views.iter().map(|p| &p.view));
        let ts_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        Some(FairshareSample::from_views(ts_ms, &views))
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
    end_user: Option<EndUser>,
    session: Option<String>,
    /// Session affinity put it ahead of an older request when it was taken.
    jumped: bool,
    /// Shared mode: a claim for only its session's held slot has been tried.
    probed: bool,
    /// Times a later request of its key was served first, through session
    /// affinity.
    passed: u32,
    enqueued: Instant,
    /// The local fast path would have admitted it on arrival; in shared mode
    /// it is still reported as fast if its first claim succeeds.
    fast: bool,
    /// Shared mode: its key's move count (`Pool::key_moves`) when its local
    /// slot was charged. A refused claim undoes the key's charge only if the
    /// key has not moved since, because a move starts its standing again.
    charged_moves: Option<u64>,
    respond: oneshot::Sender<Admitted>,
}

/// Where a slot held for a session's next step is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HoldIn {
    /// In the pool: it stays taken here.
    Pool,
    /// In the shared backend, where any replica's claim can take it.
    Backend,
    /// Nowhere: the slot is given back.
    Nowhere,
}

/// What becomes of a finished step's slot.
#[derive(Debug, PartialEq, Eq)]
enum StepEnd {
    Release,
    /// Held in the pool for the session's next step.
    Held,
    /// To be held in the shared backend for the session's next step, if
    /// the step chained onto the one before.
    HoldInBackend {
        session: String,
        arrived: Instant,
    },
}

/// The id a slot held for `session` in `pool` goes by in the shared
/// backend, the same on every replica.
fn hold_id(pool: &PoolKey, session: &str) -> String {
    format!("{}|{session}", pool.slot_id())
}

/// Who a grant goes to, for [`Pool::send`].
struct Grant {
    tenant: Uuid,
    key: Uuid,
    session: Option<String>,
    /// It went ahead of an older request.
    jumped: bool,
}

impl Grant {
    fn new(tenant: Uuid, key: Uuid, session: Option<String>, jumped: bool) -> Self {
        Grant {
            tenant,
            key,
            session,
            jumped,
        }
    }
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

    fn pop_at(&mut self, key: &Uuid, at: usize) -> Option<Waiter> {
        let queue = self.keys.get_mut(key)?;
        let waiter = queue.remove(at)?;
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
    /// The part of each tenant's `served` that is placement (on joining, or
    /// when raised after going quiet) rather than service it received.
    /// `served - served_base` is its actual service.
    served_base: HashMap<Uuid, f64>,
    key_in_flight: HashMap<Uuid, usize>,
    key_served: HashMap<Uuid, f64>,
    key_weight: HashMap<Uuid, i64>,
    /// Configured per-key caps; enforced as this replica's share.
    key_cap: HashMap<Uuid, usize>,
    key_tenant: HashMap<Uuid, Uuid>,
    /// Which end user a derived key stands for (dashboards only).
    key_end_user: HashMap<Uuid, EndUser>,
    /// The pool's virtual time among tenants, in `served / weight` (a
    /// tenant's standing, which is what tenants are ranked on): the lowest
    /// standing among tenants with requests queued. A tenant that starts
    /// queueing is placed no lower than it. See [`Pool::join_tenant`].
    virtual_time: f64,
    /// Per tenant, the virtual time of its keys, in `key_served / key_weight`
    /// (a key's standing). A key that comes back with nothing queued is
    /// raised to it, so it rejoins level with the keys it now competes with.
    /// The pool's `virtual_time` is the wrong place for that: it measures
    /// tenants, the service of every key in a tenant together. See
    /// [`Pool::advance_key_clock`].
    key_clock: HashMap<Uuid, f64>,
    /// Per tenant, its keys with nothing queued or running that are kept
    /// because they stand above the key clock. Each is forgotten once the
    /// clock reaches it, and the set holds at most
    /// [`MAX_IDLE_KEYS_PER_TENANT`].
    key_idle: HashMap<Uuid, HashSet<Uuid>>,
    /// How many times each tracked key has moved to another tenant.
    key_moves: HashMap<Uuid, u64>,
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
    /// Session affinity. Not cleared by [`Pool::reset`]: a session stays
    /// cached upstream while the pool is idle between its steps.
    affinity: affinity::Affinity,
    /// Where `affinity`'s setting comes from: the scheduler's, which
    /// [`FairShare::set_session_affinity`] changes, or the model's own.
    affinity_source: PoolAffinity,
    /// Background pools: the share of the model's pool and of the global
    /// ceiling this pool may fill (see [`background_cap`]).
    share: f64,
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
            key_end_user: HashMap::new(),
            virtual_time: 0.0,
            key_clock: HashMap::new(),
            key_idle: HashMap::new(),
            key_moves: HashMap::new(),
            idle_since: None,
            pending: HashMap::new(),
            probing: false,
            blocked: false,
            blocked_tenants: HashSet::new(),
            blocked_keys: HashSet::new(),
            cluster_in_flight: None,
            affinity: affinity::Affinity::default(),
            affinity_source: PoolAffinity::Scheduler,
            share: DEFAULT_BACKGROUND_SHARE,
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
        let moved_from = self.set_key_tenant(req.key, req.tenant);
        match &req.end_user {
            Some(end_user) => {
                self.key_end_user.insert(req.key, end_user.clone());
            }
            None => {
                self.key_end_user.remove(&req.key);
            }
        }
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
        if let Some(old) = moved_from {
            self.move_queued(req.key, old, req.tenant);
        }
    }

    /// A key moved from `from` to `to`: its requests still queued under
    /// `from` move to `to`'s queue, in arrival order, and take `to`'s
    /// weight, group and cap. So a key is only ever queued under the tenant
    /// that owns it.
    fn move_queued(&mut self, key: Uuid, from: Uuid, to: Uuid) {
        let Some(queue) = self.queues.get_mut(&from) else {
            return;
        };
        let Some(waiters) = queue.keys.remove(&key) else {
            return;
        };
        queue.len -= waiters.len();
        if queue.is_empty() {
            self.queues.remove(&from);
        }
        self.join_tenant(to);
        for mut waiter in waiters {
            self.retarget_waiter(&mut waiter, to);
            self.queues.entry(to).or_default().reinsert(key, waiter);
        }
    }

    /// Give a waiter `tenant`'s weight, group and cap, as last recorded.
    fn retarget_waiter(&self, waiter: &mut Waiter, tenant: Uuid) {
        if let Some(weight) = self.tenant_weight.get(&tenant) {
            waiter.weight = *weight;
        }
        if let Some(group) = self.tenant_group.get(&tenant) {
            waiter.group = group.clone();
            if let Some(weight) = self.group_weight.get(group) {
                waiter.group_weight = *weight;
            }
        }
        waiter.tenant_max_in_flight = self.tenant_cap.get(&tenant).copied();
    }

    /// Record `key`'s tenant, as of its latest admission. A key moved to
    /// another tenant leaves the old one's idle set, and its standing,
    /// measured against the old tenant's key clock, starts again at the new
    /// one's. Returns the tenant it moved from.
    fn set_key_tenant(&mut self, key: Uuid, tenant: Uuid) -> Option<Uuid> {
        let old = self.key_tenant.insert(key, tenant)?;
        if old == tenant {
            return None;
        }
        if let Some(idle) = self.key_idle.get_mut(&old) {
            idle.remove(&key);
        }
        let start = self.key_start(tenant, key);
        self.key_served.insert(key, start);
        *self.key_moves.entry(key).or_insert(0) += 1;
        Some(old)
    }

    fn key_moves_of(&self, key: &Uuid) -> u64 {
        self.key_moves.get(key).copied().unwrap_or(0)
    }

    /// The tenant `key` belongs to now: the one its latest admission named.
    /// A request already running, or a session slot held, under the old
    /// tenant when the key moved is still billed there, but the key's
    /// standing, clock and idle state are kept with its owner.
    fn key_owner(&self, key: &Uuid, fallback: Uuid) -> Uuid {
        self.key_tenant.get(key).copied().unwrap_or(fallback)
    }

    fn track_waiter_meta(&mut self, tenant: Uuid, key: Uuid, w: &Waiter) {
        self.tenant_weight.insert(tenant, w.weight.max(1));
        self.tenant_group.insert(tenant, w.group.clone());
        self.group_weight
            .insert(w.group.clone(), w.group_weight.max(1));
        self.key_weight.insert(key, w.key_weight.max(1));
        // A request queued before the key moved does not move it back.
        self.key_tenant.entry(key).or_insert(tenant);
        match &w.end_user {
            Some(end_user) => {
                self.key_end_user.insert(key, end_user.clone());
            }
            None => {
                self.key_end_user.remove(&key);
            }
        }
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

    /// The slots this pool may fill now, given what the same model's other
    /// pool holds (`sibling_in_flight`) and has waiting (`sibling_queued`),
    /// and the replica's in-flight total against its ceiling.
    ///
    /// An interactive pool gives up the slots background work holds. A
    /// background pool takes nothing new while an interactive request waits,
    /// and fills at most its share of the pool and of the ceiling, both
    /// counting every request in flight; the rest stays free for
    /// interactive arrivals.
    fn effective_with(
        &self,
        headroom: usize,
        sibling_in_flight: usize,
        sibling_queued: usize,
        replica_in_flight: usize,
        ceiling: usize,
    ) -> usize {
        if !self.key.is_background() {
            return self
                .cap
                .saturating_sub(sibling_in_flight)
                .min(self.in_flight.saturating_add(headroom))
                .max(1);
        }
        if sibling_queued > 0 {
            return self.in_flight;
        }
        let pool_room = background_cap(self.cap, self.share).saturating_sub(sibling_in_flight);
        let global_room = background_cap(ceiling, self.share).saturating_sub(replica_in_flight);
        pool_room.min(self.in_flight.saturating_add(global_room))
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
        self.charge(tenant, key, cost);
    }

    /// Bill a grant's cost to its tenant and key. [`Pool::occupy`] does it
    /// for a new slot; a held slot taken by its session is billed alone,
    /// since it was never given back.
    fn charge(&mut self, tenant: Uuid, key: Uuid, cost: u32) {
        let start = self.tenant_start(&tenant);
        *self.served.entry(tenant).or_insert_with(|| {
            self.served_base.insert(tenant, start);
            start
        }) += cost as f64;
        let owner = self.key_owner(&key, tenant);
        let start = self.key_start(owner, key);
        let served = self.key_served.entry(key).or_insert(start);
        let began = *served;
        *served += cost as f64;
        let began = began / self.key_weight_of(&key);
        if let Some(idle) = self.key_idle.get_mut(&owner) {
            idle.remove(&key);
        }
        self.advance_key_clock(owner, began);
    }

    /// Start-time fair queueing among one tenant's keys, after a grant to
    /// one of them that started from standing `began`. The tenant's key
    /// clock moves up to the lowest standing among its keys that are queued
    /// and could be picked, or to `began` when none is. It never moves back,
    /// so a key held at its cap cannot pull it down, and a key that sends
    /// one request at a time moves it as much as a backlogged one does.
    /// Quiet keys the clock has reached are forgotten.
    ///
    /// The clock is not held back by requests still running. That keeps a
    /// key that arrives from being credited with time before it came, at a
    /// cost to a key that sends one request at a time next to a backlogged
    /// sibling: the clock can pass it while its request runs, so it comes
    /// back level with the sibling and gets about one request per sibling
    /// request, whatever its weight or request size.
    ///
    /// Keys with nothing behind them start at the clock and so go first: new
    /// keys arriving more slowly than the tenant is served take turns with a
    /// backlogged sibling, and a stream of them as fast as the tenant is
    /// served takes all of it, as fair queueing does.
    fn advance_key_clock(&mut self, tenant: Uuid, began: f64) {
        let waiting = self
            .queues
            .get(&tenant)
            .into_iter()
            .flat_map(|q| q.keys.iter())
            .filter(|(k, d)| !d.is_empty() && self.key_open(k))
            .map(|(k, _)| self.key_standing(k))
            .fold(f64::INFINITY, f64::min);
        let target = if waiting.is_finite() { waiting } else { began };
        let clock = self.key_clock.entry(tenant).or_insert(0.0);
        if target <= *clock {
            return;
        }
        *clock = target;
        self.forget_caught_up_keys(tenant);
    }

    /// Forget `tenant`'s kept idle keys that its key clock has reached: on
    /// return they would be raised to it anyway. Keys that have become
    /// active again leave the idle set.
    fn forget_caught_up_keys(&mut self, tenant: Uuid) {
        let Some(idle) = self.key_idle.get(&tenant) else {
            return;
        };
        let clock = self.key_clock.get(&tenant).copied().unwrap_or(0.0);
        let mut active = Vec::new();
        let mut caught_up = Vec::new();
        for key in idle {
            if !self.key_idle_here(&tenant, key) {
                active.push(*key);
            } else if self.key_standing(key) <= clock {
                caught_up.push(*key);
            }
        }
        if let Some(idle) = self.key_idle.get_mut(&tenant) {
            for key in &active {
                idle.remove(key);
            }
        }
        for key in &caught_up {
            self.forget_key(key);
        }
        if self.key_idle.get(&tenant).is_some_and(HashSet::is_empty) {
            self.key_idle.remove(&tenant);
        }
    }

    /// A key's standing: its service over its weight.
    fn key_standing(&self, key: &Uuid) -> f64 {
        self.key_served.get(key).copied().unwrap_or(0.0) / self.key_weight_of(key)
    }

    /// Nothing of `key` is queued, under any tenant, or running.
    fn key_quiet(&self, key: &Uuid) -> bool {
        !self.key_in_flight.contains_key(key) && self.queues.values().all(|q| q.key_len(key) == 0)
    }

    /// `key` is quiet and belongs to `tenant`, so it may be forgotten from
    /// `tenant`'s idle set.
    fn key_idle_here(&self, tenant: &Uuid, key: &Uuid) -> bool {
        self.key_tenant.get(key) == Some(tenant) && self.key_quiet(key)
    }

    /// Where a key of `tenant` starts, in `key_served`: its tenant's key
    /// clock at the key's weight.
    fn key_start(&self, tenant: Uuid, key: Uuid) -> f64 {
        self.key_clock.get(&tenant).copied().unwrap_or(0.0) * self.key_weight_of(&key)
    }

    fn key_weight_of(&self, key: &Uuid) -> f64 {
        self.key_weight.get(key).copied().unwrap_or(1).max(1) as f64
    }

    fn make_admitted(&self, grant: Grant, admission: Admission, waited: Duration) -> Admitted {
        Admitted {
            permit: Permit {
                release: Some(self.ctl_tx.clone()),
                tenant: grant.tenant,
                key: grant.key,
                pool: self.key.clone(),
                session: grant.session,
            },
            admission,
            waited,
        }
    }

    fn send(
        &mut self,
        grant: Grant,
        respond: oneshot::Sender<Admitted>,
        admission: Admission,
        waited: Duration,
    ) {
        let arrived = Instant::now()
            .checked_sub(waited)
            .unwrap_or_else(Instant::now);
        self.affinity
            .granted(grant.session.as_deref(), grant.jumped, arrived);
        // Only affinity's own reordering counts as passing. In shared mode
        // concurrent claims for one key can land out of order too; that is
        // not a request being passed over.
        if grant.jumped {
            self.pass_earlier(grant.tenant, grant.key, arrived);
        }
        // If the caller is gone, `send` hands the `Admitted` back as `Err` and
        // dropping it here drops its `Permit`, which already releases the
        // slot via `Permit::drop`. An explicit release here would double up:
        // this grant would be released twice for one occupy.
        let _ = respond.send(self.make_admitted(grant, admission, waited));
    }

    fn grant_fast(&mut self, req: AdmitRequest, respond: oneshot::Sender<Admitted>) {
        self.occupy(req.tenant, req.key, req.cost);
        let grant = Grant::new(req.tenant, req.key, req.session, false);
        self.send(grant, respond, Admission::Fast, Duration::ZERO);
    }

    /// Give `req` the slot held for its session: it was never given back,
    /// so only the cost is billed. It goes ahead of anyone queued.
    fn grant_held(
        &mut self,
        hold: affinity::Hold,
        req: AdmitRequest,
        respond: oneshot::Sender<Admitted>,
    ) {
        self.charge(hold.tenant, hold.key, req.cost);
        let jumped = self.queued_total > 0;
        let grant = Grant::new(hold.tenant, hold.key, req.session, jumped);
        self.send(grant, respond, Admission::Fast, Duration::ZERO);
    }

    fn enqueue(
        &mut self,
        req: AdmitRequest,
        respond: oneshot::Sender<Admitted>,
        enqueued: Instant,
        fast: bool,
    ) {
        self.join_tenant(req.tenant);
        let key_queue_empty = self
            .queues
            .get(&req.tenant)
            .map(|q| q.key_len(&req.key) == 0)
            .unwrap_or(true);
        if key_queue_empty {
            let start = self.key_start(req.tenant, req.key);
            let entry = self.key_served.entry(req.key).or_insert(start);
            if *entry < start {
                *entry = start;
            }
            if let Some(idle) = self.key_idle.get_mut(&req.tenant) {
                idle.remove(&req.key);
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
                end_user: req.end_user,
                session: req.session,
                jumped: false,
                probed: false,
                passed: 0,
                enqueued,
                fast,
                charged_moves: None,
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
        let grant = Grant::new(tenant, key, waiter.session, waiter.jumped);
        self.send(grant, waiter.respond, Admission::Queued, waited);
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
            let at = self.affinity.pick(
                self.queues[&tenant].keys[&key]
                    .iter()
                    .map(|w| (w.session.as_deref(), w.enqueued, w.passed)),
                Instant::now(),
            );
            let mut waiter = {
                let queue = self.queues.get_mut(&tenant).expect("picked tenant exists");
                let waiter = queue.pop_at(&key, at).expect("picked waiter exists");
                if queue.is_empty() {
                    self.queues.remove(&tenant);
                }
                waiter
            };
            waiter.jumped = at > 0;
            self.queued_total -= 1;
            if waiter.respond.is_closed() {
                dropped += 1;
                self.forget_if_quiet(tenant, key);
                continue;
            }
            self.track_waiter_meta(tenant, key, &waiter);
            self.occupy(tenant, key, waiter.cost);
            waiter.charged_moves = Some(self.key_moves_of(&key));
            self.advance_virtual_time();
            return (Some((tenant, key, waiter)), dropped);
        }
    }

    /// Shared mode: the claim for a waiter [`Pool::take_next`] or
    /// [`Pool::take_probe`] gave its local slot. With `holds`, it names the
    /// slot held for the waiter's session, if any; `redeem_only` claims that
    /// slot alone.
    fn claim(
        &self,
        tenant: Uuid,
        key: Uuid,
        waiter: &Waiter,
        global_cap: usize,
        holds: bool,
        redeem_only: bool,
    ) -> SlotClaim {
        let background = self.key.is_background();
        let (pool_cap, global_cap) = if background {
            (
                background_cap(self.configured_cap, self.share),
                background_cap(global_cap, self.share),
            )
        } else {
            (self.configured_cap, global_cap)
        };
        SlotClaim {
            pool: self.key.slot_id(),
            tenant,
            key,
            pool_cap,
            global_cap,
            sibling: self.key.sibling().map(|k| k.slot_id()),
            background,
            tenant_cap: self.tenant_cap.get(&tenant).copied(),
            key_cap: self.key_cap.get(&key).copied(),
            hold: waiter
                .session
                .as_deref()
                .filter(|_| holds)
                .map(|session| hold_id(&self.key, session)),
            redeem_only,
            starving: holds && self.starving(waiter),
        }
    }

    /// Shared mode: whether the pool has a request, `waiter` included, that
    /// has waited `max_bypass`. Its claims then make every replica give up
    /// the pool's held slots.
    fn starving(&self, waiter: &Waiter) -> bool {
        let now = Instant::now();
        self.affinity.aged(waiter.enqueued, now)
            || self
                .oldest_eligible()
                .is_some_and(|t| self.affinity.aged(t, now))
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
        if waiter
            .charged_moves
            .is_none_or(|m| m == self.key_moves_of(&key))
        {
            if let Some(served) = self.key_served.get_mut(&key) {
                *served -= waiter.cost as f64;
            }
        }
        waiter.charged_moves = None;
        waiter.fast = false;
        waiter.jumped = false;
        // A key that moved while its claim was out goes back in line under
        // its new tenant.
        let owner = self.key_owner(&key, tenant);
        if owner != tenant {
            self.retarget_waiter(&mut waiter, owner);
            self.join_tenant(owner);
        }
        self.queues.entry(owner).or_default().reinsert(key, waiter);
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
    /// the placement each started from. Summing raw `served` would count
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
            let score = self.key_standing(key);
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
            .filter_map(|(t, _)| self.tenant_standing(t))
            .fold(f64::INFINITY, f64::min);
        if min_active.is_finite() {
            self.virtual_time = min_active;
        }
    }

    fn tenant_weight_of(&self, tenant: &Uuid) -> f64 {
        self.tenant_weight.get(tenant).copied().unwrap_or(1).max(1) as f64
    }

    /// A tenant's service per unit of weight, the value tenants are ranked
    /// on; `None` for a tenant the pool does not track.
    fn tenant_standing(&self, tenant: &Uuid) -> Option<f64> {
        self.served
            .get(tenant)
            .map(|served| served / self.tenant_weight_of(tenant))
    }

    /// Where `tenant` starts: the pool's virtual time, at its weight.
    fn tenant_start(&self, tenant: &Uuid) -> f64 {
        self.virtual_time * self.tenant_weight_of(tenant)
    }

    /// Place `tenant` as it starts queueing with nothing queued: a new
    /// tenant starts at the pool's virtual time, and one below it, from
    /// going quiet, is raised to it, so it carries no credit for time it
    /// was away. Called before a request goes into the tenant's queue,
    /// whether it arrives there or moves there with its key.
    fn join_tenant(&mut self, tenant: Uuid) {
        if self.queues.get(&tenant).is_some_and(|q| !q.is_empty()) {
            return;
        }
        let start = self.tenant_start(&tenant);
        match self.served.get_mut(&tenant) {
            None => {
                self.served.insert(tenant, start);
                self.served_base.insert(tenant, start);
            }
            Some(served) if *served < start => {
                // The raise is placement, not service: move the base with it
                // so group scores count only real service.
                *self.served_base.entry(tenant).or_insert(0.0) += start - *served;
                *served = start;
            }
            Some(_) => {}
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

    /// A step of `session` finished in a slot of `tenant` and `key`: what
    /// becomes of the slot, given where a hold for the session's next step
    /// would be kept.
    fn finish_step(
        &mut self,
        tenant: Uuid,
        key: Uuid,
        session: Option<&str>,
        hold_in: HoldIn,
    ) -> StepEnd {
        let now = Instant::now();
        let (Some(step), Some(session)) = (self.affinity.finished(session, now), session) else {
            return StepEnd::Release;
        };
        let aged = self
            .oldest_eligible()
            .is_some_and(|t| self.affinity.aged(t, now));
        match hold_in {
            _ if aged || self.key_passed_out(tenant, key) => StepEnd::Release,
            HoldIn::Pool if step.chained && self.affinity.can_hold(self.cap) => {
                self.affinity.hold(session, tenant, key, now);
                StepEnd::Held
            }
            // The step before may have run on another replica: the backend,
            // which saw it finish, decides whether this one chained onto it.
            HoldIn::Backend => StepEnd::HoldInBackend {
                session: session.to_string(),
                arrived: step.arrived,
            },
            _ => StepEnd::Release,
        }
    }

    /// Shared mode: take the first queued request of a session that has not
    /// yet tried for a slot held for it, and give it its local slot, for a
    /// claim on that held slot alone. Undone by [`Pool::requeue`].
    fn take_probe(&mut self) -> Option<(Uuid, Uuid, Waiter)> {
        let oldest = self.oldest_eligible();
        let (tenant, key, at) = self.queues.iter().find_map(|(tenant, queue)| {
            queue.keys.iter().find_map(|(key, deque)| {
                deque
                    .iter()
                    .position(|w| w.session.is_some() && !w.probed && !w.respond.is_closed())
                    .filter(|_| !self.key_passed_out(*tenant, *key))
                    .map(|at| (*tenant, *key, at))
            })
        })?;
        let queue = self.queues.get_mut(&tenant).expect("tenant just found");
        let mut waiter = queue.pop_at(&key, at).expect("waiter just found");
        if queue.is_empty() {
            self.queues.remove(&tenant);
        }
        self.queued_total -= 1;
        waiter.probed = true;
        waiter.jumped = oldest.is_some_and(|t| t < waiter.enqueued);
        self.track_waiter_meta(tenant, key, &waiter);
        self.occupy(tenant, key, waiter.cost);
        waiter.charged_moves = Some(self.key_moves_of(&key));
        self.advance_virtual_time();
        Some((tenant, key, waiter))
    }

    /// Session affinity: a request of `key` that arrived at `arrived` was
    /// served, passing every request of the key queued before it. They are
    /// queued under the key's owner, which may not be the `tenant` whose
    /// slot served it.
    fn pass_earlier(&mut self, tenant: Uuid, key: Uuid, arrived: Instant) {
        if !self.affinity.on() {
            return;
        }
        let tenant = self.key_owner(&key, tenant);
        let Some(deque) = self
            .queues
            .get_mut(&tenant)
            .and_then(|q| q.keys.get_mut(&key))
        else {
            return;
        };
        for w in deque.iter_mut().take_while(|w| w.enqueued < arrived) {
            w.passed = w.passed.saturating_add(1);
        }
    }

    /// Session affinity: whether a queued request of `key` whose caller
    /// still waits has been passed as often as it may be. Then nothing more
    /// may pass it: no slot is held for the key, nor a held one taken.
    fn key_passed_out(&self, tenant: Uuid, key: Uuid) -> bool {
        self.queues
            .get(&self.key_owner(&key, tenant))
            .and_then(|q| q.keys.get(&key))
            .is_some_and(|d| {
                d.iter()
                    .any(|w| self.affinity.passed_out(w.passed) && !w.respond.is_closed())
            })
    }

    /// Arrival of the oldest request queued in the pool that a free slot
    /// would go to: its caller still waits and neither its tenant nor its
    /// key is at a cap. Holds only give way to those.
    fn oldest_eligible(&self) -> Option<Instant> {
        self.queues
            .iter()
            .filter(|(tenant, _)| {
                self.tenant_has_slot(tenant) && !self.blocked_tenants.contains(tenant)
            })
            .flat_map(|(_, q)| q.keys.iter())
            .filter(|(key, _)| self.key_open(key))
            .filter_map(|(_, d)| {
                d.iter()
                    .find(|w| !w.respond.is_closed())
                    .map(|w| w.enqueued)
            })
            .min()
    }

    /// The held slots to give back now: all of them once a request here has
    /// waited too long to keep yielding to them, and those of a key with a
    /// request that may be passed no more.
    fn holds_to_give_up(&mut self, now: Instant) -> Vec<affinity::Hold> {
        let aged = self
            .oldest_eligible()
            .is_some_and(|t| self.affinity.aged(t, now));
        if aged {
            return self.affinity.give_up();
        }
        let passed_out: Vec<Uuid> = self
            .affinity
            .holds()
            .filter(|h| self.key_passed_out(h.tenant, h.key))
            .map(|h| h.key)
            .collect();
        passed_out
            .into_iter()
            .flat_map(|key| self.affinity.give_up_key(key))
            .collect()
    }

    /// Hand held slots to queued requests of their sessions. Each keeps the
    /// slot it was held in, and its tenant, and is billed its cost there.
    /// The requests are looked for under the key's owner, where a key that
    /// moved since the slot was held is queued. Returns how many were
    /// granted and how many waiters whose caller had gone were dropped.
    fn redeem_queued(&mut self) -> (usize, usize) {
        let (mut granted, mut dropped) = (0, 0);
        let held: Vec<(String, Uuid, Uuid)> = self
            .affinity
            .holds()
            .map(|h| (h.session.clone(), h.tenant, h.key))
            .collect();
        for (session, tenant, key) in held {
            if self.key_passed_out(tenant, key) {
                continue;
            }
            let queued = self.key_owner(&key, tenant);
            let find = |pool: &Pool| {
                pool.queues
                    .get(&queued)
                    .and_then(|q| q.keys.get(&key))
                    .and_then(|d| {
                        d.iter()
                            .position(|w| w.session.as_deref() == Some(session.as_str()))
                    })
            };
            while let Some(at) = find(self) {
                let queue = self.queues.get_mut(&queued).expect("waiter just found");
                let waiter = queue.pop_at(&key, at).expect("waiter just found");
                if queue.is_empty() {
                    self.queues.remove(&queued);
                }
                self.queued_total -= 1;
                if waiter.respond.is_closed() {
                    dropped += 1;
                    continue;
                }
                let hold = self
                    .affinity
                    .redeem(Some(&session))
                    .expect("hold just listed");
                let jumped = at > 0 || self.oldest_eligible().is_some_and(|t| t < waiter.enqueued);
                self.track_waiter_meta(queued, key, &waiter);
                self.charge(tenant, key, waiter.cost);
                self.advance_virtual_time();
                let waited = waiter.enqueued.elapsed();
                let grant = Grant::new(hold.tenant, hold.key, waiter.session, jumped);
                self.send(grant, waiter.respond, Admission::Queued, waited);
                granted += 1;
                break;
            }
        }
        (granted, dropped)
    }

    fn forget_if_quiet(&mut self, tenant: Uuid, key: Uuid) {
        // A key that is idle and at or below its owner's key clock would be
        // raised to it on return anyway, so its entry carries no information
        // worth keeping. One above the clock is kept until the clock reaches
        // it.
        if self.key_quiet(&key) {
            let owner = self.key_owner(&key, tenant);
            if self.key_served.get(&key).copied().unwrap_or(0.0) <= self.key_start(owner, key) {
                self.forget_key(&key);
            } else {
                self.keep_idle_key(owner, key);
            }
        }
        // The same rule for the tenant, so tenants that have gone quiet (or
        // have been deleted) stop showing up in the pool's share tree.
        let tenant_queued = self.queues.get(&tenant).map(|q| q.len).unwrap_or(0);
        if tenant_queued == 0
            && !self.tenant_in_flight.contains_key(&tenant)
            && self.tenant_standing(&tenant).unwrap_or(0.0) <= self.virtual_time
        {
            self.forget_tenant(&tenant);
        }
    }

    /// Keep an idle `key` for its lead over `tenant`'s key clock, forgetting
    /// the tenant's idle key with the smallest lead if that makes too many.
    fn keep_idle_key(&mut self, tenant: Uuid, key: Uuid) {
        let idle = self.key_idle.entry(tenant).or_default();
        idle.insert(key);
        if idle.len() <= MAX_IDLE_KEYS_PER_TENANT {
            return;
        }
        let (here, gone): (Vec<Uuid>, Vec<Uuid>) = self.key_idle[&tenant]
            .iter()
            .partition(|k| self.key_idle_here(&tenant, k));
        let smallest = here
            .into_iter()
            .min_by(|a, b| self.key_standing(a).total_cmp(&self.key_standing(b)));
        if let Some(idle) = self.key_idle.get_mut(&tenant) {
            for key in gone.iter().chain(smallest.iter()) {
                idle.remove(key);
            }
        }
        if let Some(smallest) = smallest {
            self.forget_key(&smallest);
        }
    }

    fn forget_key(&mut self, key: &Uuid) {
        if let Some(tenant) = self.key_tenant.get(key).copied() {
            if let Some(idle) = self.key_idle.get_mut(&tenant) {
                idle.remove(key);
            }
        }
        self.key_served.remove(key);
        self.key_moves.remove(key);
        self.key_weight.remove(key);
        self.key_cap.remove(key);
        self.key_tenant.remove(key);
        self.key_end_user.remove(key);
    }

    /// Only called for a tenant with no in-flight slot and an empty queue,
    /// so none of its keys has one either. The idle keys kept for their lead
    /// go with it. Its key clock stays until the pool is reset, so any key
    /// still tracked stays measured against it.
    fn forget_tenant(&mut self, tenant: &Uuid) {
        if let Some(idle) = self.key_idle.remove(tenant) {
            for key in &idle {
                if self.key_idle_here(tenant, key) {
                    self.forget_key(key);
                }
            }
        }
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
        self.key_end_user.clear();
        self.virtual_time = 0.0;
        self.key_clock.clear();
        self.key_idle.clear();
        self.key_moves.clear();
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

    /// The share split of this pool (see [`pool_shares`]), over `ids` (every
    /// tenant with state) and `key_ids`.
    fn shares(&self, ids: &HashSet<Uuid>, key_ids: &HashSet<Uuid>) -> PoolShares {
        let default_group = "default".to_string();
        let tenants: Vec<ShareTenant<'_>> = ids
            .iter()
            .map(|id| ShareTenant {
                id: *id,
                group: self.tenant_group.get(id).unwrap_or(&default_group),
                weight: self.tenant_weight.get(id).copied().unwrap_or(1),
                active: self.tenant_is_active(id),
            })
            .collect();
        let keys: Vec<ShareKey> = key_ids
            .iter()
            .filter_map(|id| {
                let (in_flight, queued) = self.key_state(id);
                Some(ShareKey {
                    id: *id,
                    tenant: self.key_tenant.get(id).copied()?,
                    weight: self.key_weight.get(id).copied().unwrap_or(1),
                    active: in_flight > 0 || queued > 0,
                })
            })
            .collect();
        pool_shares(self.algorithm, &self.group_weight, &tenants, &keys)
    }

    /// A key's in-flight and queued requests here.
    fn key_state(&self, key: &Uuid) -> (usize, usize) {
        let queued = self
            .key_tenant
            .get(key)
            .and_then(|t| self.queues.get(t))
            .map(|q| q.key_len(key))
            .unwrap_or(0);
        (self.key_in_flight.get(key).copied().unwrap_or(0), queued)
    }

    /// This pool in a [`ReplicaView`]: its totals, and the groups, tenants
    /// and keys with a request running or waiting, the busiest first within
    /// the pool's bounds and what is left of `budget`.
    fn view(&self, budget: &mut view::ViewBudget) -> ReplicaPoolView {
        let active: HashSet<Uuid> = self
            .tenant_in_flight
            .keys()
            .chain(self.queues.keys())
            .filter(|t| self.tenant_is_active(t))
            .copied()
            .collect();
        let mut tenants: Vec<ReplicaTenantView> = active
            .iter()
            .map(|id| {
                let served = self.served.get(id).copied().unwrap_or(0.0);
                let base = self.served_base.get(id).copied().unwrap_or(0.0);
                ReplicaTenantView {
                    id: *id,
                    group: self
                        .tenant_group
                        .get(id)
                        .cloned()
                        .unwrap_or_else(|| "default".into()),
                    weight: self.tenant_weight.get(id).copied().unwrap_or(1).max(1),
                    cap: self.tenant_cap.get(id).copied(),
                    in_flight: self.tenant_in_flight.get(id).copied().unwrap_or(0),
                    queued: self.queues.get(id).map(|q| q.len).unwrap_or(0),
                    served: (served - base).max(0.0),
                }
            })
            .collect();
        let limit = view::VIEW_MAX_TENANTS_PER_POOL.min(budget.tenants);
        let omitted_tenants =
            view::keep_busiest(&mut tenants, limit, |t| (t.in_flight + t.queued, t.id));
        budget.tenants -= tenants.len();

        let active_keys: HashSet<Uuid> = self
            .key_in_flight
            .keys()
            .copied()
            .chain(self.queues.values().flat_map(|q| q.keys.keys().copied()))
            .collect();
        let mut keys: Vec<ReplicaKeyView> = active_keys
            .iter()
            .filter_map(|id| {
                let (in_flight, queued) = self.key_state(id);
                if in_flight == 0 && queued == 0 {
                    return None;
                }
                Some(ReplicaKeyView {
                    id: *id,
                    tenant: self.key_tenant.get(id).copied()?,
                    weight: self.key_weight.get(id).copied().unwrap_or(1).max(1),
                    cap: self.key_cap.get(id).copied(),
                    in_flight,
                    queued,
                    served: self.key_served.get(id).copied().unwrap_or(0.0),
                    end_user: self.key_end_user.get(id).cloned(),
                })
            })
            .collect();
        let limit = view::VIEW_MAX_KEYS_PER_POOL.min(budget.keys);
        let omitted_keys = view::keep_busiest(&mut keys, limit, |k| (k.in_flight + k.queued, k.id));
        budget.keys -= keys.len();

        let service = self.group_service();
        let groups = self
            .group_totals()
            .into_iter()
            .map(|g| ReplicaGroupView {
                weight: self
                    .group_weight
                    .get(&g.name)
                    .copied()
                    .unwrap_or(100)
                    .max(1),
                served: service.get(g.name.as_str()).copied().unwrap_or(0.0),
                name: g.name,
                in_flight: g.in_flight,
                queued: g.queued,
            })
            .collect();

        ReplicaPoolView {
            pool: self.key.slot_id(),
            configured_cap: self.configured_cap,
            in_flight: self.in_flight,
            queued: self.queued_total,
            groups,
            tenants,
            keys,
            omitted_tenants,
            omitted_keys,
        }
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
        // Keys: every key with state.
        let key_ids: HashSet<Uuid> = self
            .key_tenant
            .keys()
            .copied()
            .chain(self.key_in_flight.keys().copied())
            .chain(self.queues.values().flat_map(|q| q.keys.keys().copied()))
            .collect();
        let shares = self.shares(&ids, &key_ids);
        let group_caps = self.compute_group_caps(self.effective_cap(headroom));
        let group_service = self.group_service();

        let mut names: HashSet<String> = shares.groups.keys().cloned().collect();
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
                let slot_cap = group_caps.get(&name).copied().unwrap_or(0);
                GroupFairshare {
                    weight_share: shares.group(&name),
                    name,
                    weight,
                    in_flight,
                    queued,
                    slot_cap,
                    borrowed: in_flight.saturating_sub(slot_cap),
                    served_tokens,
                    share_score: served_tokens / weight as f64,
                }
            })
            .collect();
        groups.sort_by(|a, b| a.name.cmp(&b.name));

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
                TenantFairshare {
                    tenant_id,
                    fairshare_group,
                    weight,
                    max_in_flight: self.tenant_limit(&tenant_id),
                    in_flight: self.tenant_in_flight.get(&tenant_id).copied().unwrap_or(0),
                    queued: self.queues.get(&tenant_id).map(|q| q.len).unwrap_or(0),
                    served_tokens,
                    share_score: served_tokens / weight as f64,
                    weight_share: shares.tenant(&tenant_id),
                }
            })
            .collect();
        tenants.sort_by(|a, b| {
            a.share_score
                .partial_cmp(&b.share_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut keys: Vec<KeyFairshare> = key_ids
            .iter()
            .filter_map(|&key_id| {
                let tenant_id = self.key_tenant.get(&key_id).copied()?;
                let weight = self.key_weight.get(&key_id).copied().unwrap_or(1).max(1);
                let (in_flight, queued) = self.key_state(&key_id);
                let served_tokens = self.key_served.get(&key_id).copied().unwrap_or(0.0);
                Some(KeyFairshare {
                    key_id,
                    tenant_id,
                    weight,
                    max_in_flight: self.key_limit(&key_id),
                    in_flight,
                    queued,
                    served_tokens,
                    share_score: served_tokens / weight as f64,
                    weight_share: shares.key(&key_id),
                    end_user: self.key_end_user.get(&key_id).cloned(),
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
            held: self.affinity.held(),
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
    /// When this replica next publishes its view, while more than one
    /// replica is live.
    next_view: Instant,
    /// A view publish is outstanding.
    publishing_view: bool,
    /// The last view publish failed; logged once until one works.
    view_failing: bool,
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
    /// Set with [`FairShare::set_session_affinity`]; applies to every pool.
    session_affinity: Option<SessionAffinityConfig>,
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
            let holds_due = self.next_hold_expiry();
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
                _ = tokio::time::sleep_until(holds_due.unwrap_or_else(far_future).into()),
                    if holds_due.is_some() => {
                    self.expire_holds();
                    self.dispatch_all();
                    self.publish_stats();
                }
            }
        }
    }

    /// The global ceiling as this replica enforces it.
    fn ceiling(&self) -> usize {
        replica_share(self.capacity.max_in_flight(), self.divisor)
    }

    /// What the same model's other pool holds and has waiting here.
    fn sibling_load(&self, key: &PoolKey) -> (usize, usize) {
        key.sibling()
            .and_then(|s| self.pools.get(&s))
            .map(|p| (p.in_flight, p.queued_total))
            .unwrap_or((0, 0))
    }

    /// The slots `key`'s pool may fill now (see [`Pool::effective_with`]).
    fn effective_of(&self, key: &PoolKey, headroom: usize, ceiling: usize) -> Option<usize> {
        let pool = self.pools.get(key)?;
        let (sibling_in_flight, sibling_queued) = self.sibling_load(key);
        Some(pool.effective_with(
            headroom,
            sibling_in_flight,
            sibling_queued,
            self.in_flight,
            ceiling,
        ))
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
                let mut req = *req;
                let affinity = match req.affinity {
                    PoolAffinity::Scheduler => self.session_affinity,
                    PoolAffinity::Model(config) => config,
                };
                req.session = match affinity {
                    Some(_) => req.session.take().map(|s| affinity::scoped(req.key, &s)),
                    None => None,
                };
                self.set_pool_affinity(&key, req.affinity, affinity);
                let ceiling = self.ceiling();
                let headroom = ceiling.saturating_sub(self.in_flight);
                let default_cap = self.default_model_cap;
                let override_cap = match &key {
                    PoolKey::Model(name) | PoolKey::Background(name) => {
                        self.model_caps.get(name).copied()
                    }
                    PoolKey::Unrouted => None,
                };
                let (sibling_in_flight, sibling_queued) = self.sibling_load(&key);
                let replica_in_flight = self.in_flight;
                let shared_mode = self.mode == SlotMode::Shared;
                let quiescing = self.quiescing();
                let pool = self.pool_mut(&key);
                if key.is_background() {
                    pool.share = req.background_share.unwrap_or(DEFAULT_BACKGROUND_SHARE);
                }
                pool.set_configured_cap(match key {
                    // No route means no configured cap to honour.
                    PoolKey::Unrouted => default_cap,
                    PoolKey::Model(_) | PoolKey::Background(_) => {
                        override_cap.unwrap_or_else(|| {
                            req.model_max_in_flight
                                .filter(|c| *c > 0)
                                .unwrap_or(default_cap)
                        })
                    }
                });
                pool.idle_since = None;
                pool.track_meta(&req);
                // A slot held for this session's next step is this request's,
                // unless the key has a request that may be passed no more.
                let hold = (!pool.key_passed_out(req.tenant, req.key))
                    .then(|| pool.affinity.redeem(req.session.as_deref()))
                    .flatten();
                if let Some(hold) = hold {
                    pool.grant_held(hold, req, respond);
                    self.publish_stats();
                    return;
                }
                let effective = pool.effective_with(
                    headroom,
                    sibling_in_flight,
                    sibling_queued,
                    replica_in_flight,
                    ceiling,
                );
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
                session,
            } => {
                // In shared mode the slot is the backend's, so a hold is kept
                // there, where the session's next step can take it from any
                // replica. Not while a quiesced reconcile holds releases back.
                let hold_in = match self.mode {
                    SlotMode::Shared if self.quiesced_reconcile_running() => HoldIn::Nowhere,
                    SlotMode::Shared => HoldIn::Backend,
                    _ => HoldIn::Pool,
                };
                let end = match self.pools.get_mut(&pool_key) {
                    Some(pool) => pool.finish_step(tenant, key, session.as_deref(), hold_in),
                    None => StepEnd::Release,
                };
                match end {
                    StepEnd::Held => {}
                    StepEnd::Release => self.release_slot(&pool_key, tenant, key),
                    StepEnd::HoldInBackend { session, arrived } => {
                        self.hold_in_backend(&pool_key, tenant, key, &session, arrived)
                    }
                }
                self.dispatch_all();
                self.publish_stats();
            }
            Ctl::Snapshot { respond } => {
                let _ = respond.send(self.build_snapshot());
            }
            Ctl::Sample { respond } => {
                let _ = respond.send(self.build_sample());
            }
            Ctl::View { respond } => {
                let _ = respond.send(LocalView {
                    view: self.build_view(),
                    pools: self
                        .pools
                        .values()
                        .map(|p| (p.model.clone(), p.configured_cap))
                        .collect(),
                    algorithm: self.algorithm,
                    ceiling: self.capacity.max_in_flight(),
                });
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
            Ctl::SetSessionAffinity(config) => {
                self.session_affinity = config;
                let stats = self.stats.affinity.clone();
                let mut given_up = Vec::new();
                // Pools whose model has a setting of its own keep it.
                for (key, pool) in self
                    .pools
                    .iter_mut()
                    .filter(|(_, p)| p.affinity_source == PoolAffinity::Scheduler)
                {
                    for hold in pool.affinity.configure(config, stats.clone()) {
                        given_up.push((key.clone(), hold));
                    }
                }
                for (key, hold) in given_up {
                    self.release_slot(&key, hold.tenant, hold.key);
                }
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
                    next_view: Instant::now(),
                    publishing_view: false,
                    view_failing: false,
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
            Ctl::HoldDone { pool, result } => {
                match result {
                    Ok((true, counts)) => {
                        self.stats.affinity.holds.fetch_add(1, Ordering::Relaxed);
                        self.hold_done(pool, counts);
                    }
                    result => self.release_done(pool, result.map(|(_, counts)| counts)),
                }
                self.publish_stats();
            }
            Ctl::ReconcileDone { result } => {
                self.reconcile_done(result);
                self.publish_stats();
            }
            Ctl::ViewPublished { result } => self.view_published(result),
        }
    }

    /// Give back one slot of `tenant` and `key` in `pool_key`: locally, and
    /// to the shared backend when it records the slot.
    fn release_slot(&mut self, pool_key: &PoolKey, tenant: Uuid, key: Uuid) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if let Some(pool) = self.pools.get_mut(pool_key) {
            pool.release(tenant, key);
            pool.settle_idle();
        }
        if self.mode == SlotMode::Shared || self.quiesced_reconcile_running() {
            self.release_shared(SlotRelease {
                pool: pool_key.slot_id(),
                tenant,
                key,
                sibling: pool_key.sibling().map(|k| k.slot_id()),
            });
        } else {
            self.note_unrecorded();
        }
        self.publish_model_load(pool_key);
    }

    // ---- session affinity ---------------------------------------------------

    /// Shared mode: give the slot back here, and to the backend to keep for
    /// `session`'s next step if the step that arrived at `arrived` chained
    /// onto the session's previous one.
    fn hold_in_backend(
        &mut self,
        pool_key: &PoolKey,
        tenant: Uuid,
        key: Uuid,
        session: &str,
        arrived: Instant,
    ) {
        let Some((cfg, pool)) = self
            .pools
            .get_mut(pool_key)
            .and_then(|p| Some((p.affinity.config()?, p)))
        else {
            self.release_slot(pool_key, tenant, key);
            return;
        };
        let hold = SlotHold {
            id: hold_id(pool_key, session),
            ttl: cfg.hold,
            max_held: cfg.max_held(pool.configured_cap),
            arrived_ago: arrived.elapsed(),
            remember: cfg.warm_ttl,
            max_run: cfg.max_jumps,
        };
        pool.release(tenant, key);
        pool.settle_idle();
        self.in_flight = self.in_flight.saturating_sub(1);
        self.publish_model_load(pool_key);
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        s.ops += 1;
        let slot = SlotRelease {
            pool: pool_key.slot_id(),
            tenant,
            key,
            sibling: pool_key.sibling().map(|k| k.slot_id()),
        };
        let fut = s.backend.hold(slot, hold);
        let timeout = s.config.op_timeout;
        let tx = self.ctl_tx.clone();
        let pool = pool_key.clone();
        tokio::spawn(async move {
            let result = bounded(timeout, fut).await;
            let _ = tx.send(Ctl::HoldDone { pool, result });
        });
    }

    /// When the next held slot runs out, or a request in a pool holding one
    /// has waited long enough for the holds to give way to it.
    fn next_hold_expiry(&self) -> Option<Instant> {
        self.pools
            .values()
            .filter(|p| p.affinity.held() > 0)
            .flat_map(|p| {
                let bypass = p.affinity.config().map(|c| c.max_bypass);
                [
                    p.affinity.next_expiry(),
                    p.oldest_eligible().zip(bypass).map(|(t, b)| t + b),
                ]
            })
            .flatten()
            .min()
    }

    /// Give back the held slots that ran out.
    fn expire_holds(&mut self) {
        let now = Instant::now();
        let mut expired = Vec::new();
        for (key, pool) in self.pools.iter_mut() {
            for hold in pool.affinity.expire(now) {
                expired.push((key.clone(), hold));
            }
        }
        for (key, hold) in expired {
            self.release_slot(&key, hold.tenant, hold.key);
        }
    }

    /// Hand held slots to queued requests of their sessions, after giving
    /// back those of pools where a request has waited too long.
    /// Nothing is granted while a reconcile is quiesced; giving back is not
    /// held up.
    fn serve_holds(&mut self) {
        if !self.pools.values().any(|p| p.affinity.held() > 0) {
            return;
        }
        let grant = !self.quiescing();
        let now = Instant::now();
        let mut given_up = Vec::new();
        for (key, pool) in self.pools.iter_mut() {
            if pool.affinity.held() == 0 {
                continue;
            }
            for hold in pool.holds_to_give_up(now) {
                given_up.push((key.clone(), hold));
            }
            if grant {
                let (granted, dropped) = pool.redeem_queued();
                self.queued_total = self.queued_total.saturating_sub(granted + dropped);
            }
        }
        for (key, hold) in given_up {
            self.release_slot(&key, hold.tenant, hold.key);
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

    /// When the shared-slot timer next needs to run: a refused pool's retry,
    /// the next reconcile or the next view publish.
    fn shared_deadline(&self) -> Option<Instant> {
        let s = self.shared.as_ref()?;
        let reconcile = (s.reconciling.is_none() && !s.want_reconcile).then_some(s.next_reconcile);
        let view = (self.replicas > 1 && !s.publishing_view).then_some(s.next_view);
        [s.retry_at, reconcile, view].into_iter().flatten().min()
    }

    fn shared_timer(&mut self) {
        let now = Instant::now();
        let multi = self.replicas > 1;
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
        let view_due = multi && !s.publishing_view && s.next_view <= now;
        if retry {
            // Correctness never depends on a wakeup arriving: retry anyway.
            self.unblock_all();
        }
        self.maybe_start_reconcile();
        self.dispatch_all();
        if view_due {
            self.publish_view();
        }
        self.publish_stats();
    }

    /// Publish this replica's view for the cluster-wide dashboard. Off the
    /// request path: it never holds up admission, and a failure only leaves
    /// this replica out of the cluster view until the next one works.
    fn publish_view(&mut self) {
        let view = self.build_view();
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        s.publishing_view = true;
        s.next_view = Instant::now() + s.config.view_interval;
        let fut = s.backend.publish_view(view, s.config.view_ttl());
        let timeout = s.config.op_timeout;
        let tx = self.ctl_tx.clone();
        tokio::spawn(async move {
            let result = bounded(timeout, fut).await;
            let _ = tx.send(Ctl::ViewPublished { result });
        });
    }

    fn view_published(&mut self, result: Result<(), SlotError>) {
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        s.publishing_view = false;
        match result {
            Ok(()) if s.view_failing => {
                s.view_failing = false;
                tracing::info!("fairshare cluster view published again");
            }
            Ok(()) => {}
            Err(e) if !s.view_failing => {
                s.view_failing = true;
                tracing::warn!(
                    error = %e,
                    "could not publish this replica's fairshare state; dashboards leave it out \
                     of the cluster-wide view until a publish works"
                );
            }
            Err(_) => {}
        }
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
        // Wait out a full ceiling; only claims on held slots go meanwhile.
        let global_blocked = s.global_blocked;
        let max_pending = s.config.max_pending_per_pool.max(1);
        let global_cap = self.capacity.max_in_flight();
        let n = self.pool_order.len();
        if n == 0 {
            return;
        }
        let ceiling = self.ceiling();
        let mut claims: Vec<(PoolKey, u64, SlotClaim)> = Vec::new();
        let mut progressed = !global_blocked;
        while progressed && self.in_flight < ceiling && self.queued_total > 0 {
            progressed = false;
            for i in 0..n {
                if self.in_flight >= ceiling {
                    break;
                }
                let idx = (self.cursor + i) % n;
                let headroom = ceiling.saturating_sub(self.in_flight);
                let key = self.pool_order[idx].clone();
                let Some(effective) = self.effective_of(&key, headroom, ceiling) else {
                    continue;
                };
                let Some(pool) = self.pools.get_mut(&key) else {
                    continue;
                };
                let limit = if pool.probing { 1 } else { max_pending };
                if pool.queued_total == 0 || pool.blocked || pool.pending.len() >= limit {
                    continue;
                }
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
                let holds = pool.affinity.holds_on();
                let claim = pool.claim(tenant, key_id, &waiter, global_cap, holds, false);
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
        // A slot may be held for a session whose next step came to this
        // replica while the step before it ran on another: each such queued
        // request tries once for the held slot alone, out of queue order.
        for key in self.pool_order.iter() {
            let Some(pool) = self.pools.get_mut(key).filter(|p| p.affinity.holds_on()) else {
                continue;
            };
            let limit = if pool.probing { 1 } else { max_pending };
            while pool.queued_total > 0 && pool.pending.len() < limit {
                let Some((tenant, key_id, waiter)) = pool.take_probe() else {
                    break;
                };
                let id = self.next_op_id;
                self.next_op_id += 1;
                let claim = pool.claim(tenant, key_id, &waiter, global_cap, true, true);
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
                claims.push((key.clone(), id, claim));
            }
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
            Ok((outcome @ (AcquireOutcome::Granted | AcquireOutcome::Redeemed), counts)) => {
                if outcome == AcquireOutcome::Redeemed {
                    // Taking a held slot says nothing about room for others.
                    self.stats.affinity.redeemed.fetch_add(1, Ordering::Relaxed);
                } else {
                    pool.probing = false;
                }
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
                let grant = Grant::new(tenant, key_id, waiter.session, waiter.jumped);
                pool.send(grant, waiter.respond, admission, waited);
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
                // A background claim's ceiling is its own share of the global
                // one, so reaching it blocks only its pool, never the others.
                let background = key.is_background();
                match denial {
                    SlotDenial::Pool => pool.blocked = true,
                    SlotDenial::Global if background => pool.blocked = true,
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
                    if denial == SlotDenial::Global && !background {
                        s.global_blocked = true;
                    }
                }
                self.schedule_retry();
            }
            Ok((AcquireOutcome::NotHeld, counts)) => {
                // Nothing held for it; it waits its turn, already probed.
                pool.requeue(claim);
                pool.cluster_in_flight = Some(counts.pool);
                self.in_flight = self.in_flight.saturating_sub(1);
                self.queued_total += 1;
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

    /// The backend kept a slot for a session's next step. Nothing was freed
    /// in the cluster, so claims refused for a full pool or ceiling stay
    /// waiting for a release.
    fn hold_done(&mut self, key: PoolKey, counts: ClusterCounts) {
        if let Some(s) = self.shared.as_mut() {
            s.ops = s.ops.saturating_sub(1);
            if self.mode == SlotMode::Shared {
                s.cluster_in_flight = Some(counts.global);
            }
        }
        if self.mode == SlotMode::Shared {
            if let Some(pool) = self.pools.get_mut(&key) {
                pool.cluster_in_flight = Some(counts.pool);
            }
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

    /// Give `key`'s pool the session affinity setting a request brought,
    /// handing back the slots it held if that turns affinity off there.
    fn set_pool_affinity(
        &mut self,
        key: &PoolKey,
        source: PoolAffinity,
        config: Option<SessionAffinityConfig>,
    ) {
        let stats = self.stats.affinity.clone();
        let pool = self.pool_mut(key);
        pool.affinity_source = source;
        if pool.affinity.config() == config {
            return;
        }
        let given_up = pool.affinity.configure(config, stats);
        for hold in given_up {
            self.release_slot(key, hold.tenant, hold.key);
        }
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
            pool.affinity
                .configure(self.session_affinity, self.stats.affinity.clone());
            self.pools.insert(key.clone(), pool);
            self.pool_order.push(key.clone());
        }
        self.pools.get_mut(key).expect("pool just inserted")
    }

    /// Periodic housekeeping, run on its own timer: drop waiters whose
    /// caller has gone, then drop pools that have been idle for the TTL.
    fn sweep(&mut self) {
        let mut dropped = 0;
        let now = Instant::now();
        for pool in self.pools.values_mut() {
            dropped += pool.prune_dead_waiters();
            pool.affinity.prune(now);
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
        self.serve_holds();
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
                let effective = self.effective_of(&self.pool_order[idx], headroom, ceiling);
                let (granted, dropped) =
                    match (self.pools.get_mut(&self.pool_order[idx]), effective) {
                        (Some(pool), Some(effective)) if pool.queued_total > 0 => {
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
        // The unrouted pool is not a model the router could pick. Background
        // work runs on the same upstreams, so it counts toward the model.
        let (PoolKey::Model(model) | PoolKey::Background(model)) = key else {
            return;
        };
        let in_flight = [
            PoolKey::Model(model.clone()),
            PoolKey::Background(model.clone()),
        ]
        .iter()
        .filter_map(|k| self.pools.get(k).map(|p| p.in_flight))
        .reduce(|a, b| a + b);
        if let Ok(mut shared) = self.model_load.write() {
            match in_flight {
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
        let held = self.pools.values().map(|p| p.affinity.held()).sum();
        self.stats.held.store(held, Ordering::Relaxed);
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
            cluster: false,
        }
    }

    /// This replica's [`ReplicaView`]: every pool with a request running or
    /// waiting, in pool order.
    fn build_view(&self) -> ReplicaView {
        let mut budget = view::ViewBudget::default();
        let pools = self
            .pool_order
            .iter()
            .filter_map(|key| self.pools.get(key))
            .filter(|pool| !pool.is_idle())
            .map(|pool| pool.view(&mut budget))
            .collect();
        ReplicaView {
            version: view::VIEW_VERSION,
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

    /// A pool of `slots` driven by hand: requests are queued, and each grant
    /// first frees the oldest running slot when the pool is full.
    struct Sim {
        pool: Pool,
        running: VecDeque<(Uuid, Uuid)>,
        callers: Vec<oneshot::Receiver<Admitted>>,
    }

    impl Sim {
        fn new(slots: usize) -> Self {
            let (tx, _rx) = mpsc::unbounded_channel();
            Sim {
                pool: Pool::new(
                    PoolKey::Model("m".into()),
                    FairshareAlgorithm::Weighted,
                    slots,
                    tx,
                ),
                running: VecDeque::new(),
                callers: Vec::new(),
            }
        }

        fn queue(&mut self, tenant: Uuid, key: Uuid, cost: u32) {
            self.queue_req(AdmitRequest::new(tenant, "m", cost).key(key, 100));
        }

        fn queue_req(&mut self, req: AdmitRequest) {
            self.pool.track_meta(&req);
            let (respond, admitted) = oneshot::channel();
            self.pool.enqueue(req, respond, Instant::now(), false);
            self.callers.push(admitted);
        }

        /// Free the oldest running slot if the pool is full, then grant the
        /// next request. Returns its key.
        fn grant(&mut self) -> Uuid {
            if self.running.len() >= self.pool.cap {
                let (tenant, key) = self.running.pop_front().expect("pool is full");
                self.pool.release(tenant, key);
            }
            let (next, _) = self.pool.take_next(self.pool.cap);
            let (tenant, key, _) = next.expect("a request can be granted");
            self.running.push_back((tenant, key));
            key
        }

        fn is_running(&self, key: Uuid) -> bool {
            self.running.iter().any(|(_, k)| *k == key)
        }

        /// Grant until `key` gets a slot: how many grants went first.
        fn grants_before(&mut self, key: Uuid) -> usize {
            let mut before = 0;
            while self.grant() != key {
                before += 1;
            }
            before
        }
    }

    /// A key that comes back with nothing queued starts level with its
    /// tenant's other keys, not at the service the whole tenant has had. With
    /// one key always backlogged, a key that sends one request at a time
    /// waits for at most one of the backlogged key's requests, however many
    /// other keys of the tenant were served before it.
    #[test]
    fn returning_key_starts_level_with_its_siblings() {
        let mut sim = Sim::new(1);
        let (tenant, busy) = (Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..200 {
            sim.queue(tenant, busy, 100);
        }
        sim.grant();
        for i in 0..5 {
            let other = Uuid::new_v4();
            sim.queue(tenant, other, 2_000);
            let ahead = sim.grants_before(other);
            assert!(
                ahead <= 1,
                "key {i} waited for {ahead} of the backlogged key's requests"
            );
        }
    }

    /// Starting level is a floor, not a reset: a key that has had more than
    /// its sibling and comes straight back waits until the sibling has had
    /// as much.
    #[test]
    fn returning_key_keeps_the_service_it_is_ahead_by() {
        let mut sim = Sim::new(1);
        let (tenant, busy, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..200 {
            sim.queue(tenant, busy, 100);
        }
        sim.grant();
        sim.queue(tenant, other, 2_000);
        assert!(sim.grants_before(other) <= 1);
        sim.queue(tenant, other, 2_000);
        assert!(
            !sim.pool
                .key_idle
                .get(&tenant)
                .is_some_and(|i| i.contains(&other)),
            "a key that comes back is no longer idle"
        );
        let ahead = sim.grants_before(other);
        assert!(
            (19..=21).contains(&ahead),
            "2,000 tokens ahead is 20 requests of 100, not {ahead}"
        );
    }

    /// No saved-up credit: a key that went quiet while its sibling was served
    /// is raised to the clock, at its own weight, and does not take a run of
    /// grants when it comes back with a backlog.
    #[test]
    fn idle_key_is_raised_to_the_clock_at_its_weight() {
        for weight in [100, 50, 300] {
            let mut sim = Sim::new(1);
            let (tenant, busy, other) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
            let other_req = || AdmitRequest::new(tenant, "m", 100).key(other, weight);
            for _ in 0..300 {
                sim.queue(tenant, busy, 100);
            }
            sim.grant();
            sim.queue_req(other_req());
            sim.grants_before(other);
            for _ in 0..50 {
                assert_eq!(sim.grant(), busy);
            }
            for _ in 0..10 {
                sim.queue_req(other_req());
            }
            // Over the next grants the two share by weight; without the raise
            // the returning key would take all ten in a row.
            let (mut run, mut longest) = (0, 0);
            for _ in 0..20 {
                if sim.grant() == other {
                    run += 1;
                    longest = longest.max(run);
                } else {
                    run = 0;
                }
            }
            let allowed = (weight as usize).div_ceil(100) + 1;
            assert!(
                longest <= allowed,
                "weight {weight}: {longest} grants in a row after coming back"
            );
        }
    }

    /// The clock moves with every grant to the tenant, not only while two of
    /// its keys are queued together. A key that sends one request at a time,
    /// in a tenant with nothing else queued, does not build a lead that a new
    /// sibling with a backlog could take.
    #[test]
    fn one_at_a_time_key_has_no_lead_for_a_new_sibling_to_take() {
        let mut sim = Sim::new(1);
        let (tenant, other_tenant) = (Uuid::new_v4(), Uuid::new_v4());
        let (one_at_a_time, newcomer, flood) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..500 {
            sim.queue(other_tenant, flood, 100);
        }
        sim.grant();
        for _ in 0..50 {
            sim.queue(tenant, one_at_a_time, 100);
            sim.grants_before(one_at_a_time);
        }
        for _ in 0..80 {
            sim.queue(tenant, newcomer, 100);
        }
        sim.queue(tenant, one_at_a_time, 100);
        let mut newcomer_first = 0;
        loop {
            let key = sim.grant();
            if key == one_at_a_time {
                break;
            }
            if key == newcomer {
                newcomer_first += 1;
            }
        }
        assert!(
            newcomer_first <= 2,
            "the new key got {newcomer_first} grants before the one-at-a-time key"
        );
    }

    /// A queued key held at its own cap cannot be picked, so it does not hold
    /// the clock down: a new key starts level with the open backlogged key,
    /// not with the capped one far behind it.
    #[test]
    fn capped_key_does_not_hold_the_clock_back() {
        let mut sim = Sim::new(3);
        let tenant = Uuid::new_v4();
        let (capped, open, newcomer) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..1_000 {
            sim.queue_req(
                AdmitRequest::new(tenant, "m", 100)
                    .key(capped, 100)
                    .key_cap(1),
            );
            sim.queue(tenant, open, 100);
        }
        for _ in 0..300 {
            sim.grant();
        }
        for _ in 0..200 {
            sim.queue(tenant, newcomer, 100);
        }
        let mut newcomer_first = 0;
        loop {
            let key = sim.grant();
            if key == open {
                break;
            }
            if key == newcomer {
                newcomer_first += 1;
            }
        }
        assert!(
            newcomer_first <= 2,
            "the new key got {newcomer_first} grants before the open backlogged key"
        );
    }

    /// Keys that come and go are forgotten once the clock reaches them, even
    /// in a tenant that is never quiet, and new keys arriving more slowly
    /// than the tenant is served take turns with the backlogged key.
    #[test]
    fn passing_keys_are_forgotten_and_do_not_starve_the_busy_key() {
        let mut sim = Sim::new(1);
        let (tenant, busy) = (Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..1_000 {
            sim.queue(tenant, busy, 100);
        }
        sim.grant();
        let mut busy_grants = 0;
        for _ in 0..200 {
            let passing = Uuid::new_v4();
            sim.queue(tenant, passing, 300);
            busy_grants += sim.grants_before(passing);
            assert!(
                sim.pool.key_tenant.len() <= 6,
                "{} keys tracked",
                sim.pool.key_tenant.len()
            );
        }
        assert!(
            busy_grants >= 190,
            "busy key got {busy_grants} grants in 200 turns"
        );
        for _ in 0..10 {
            assert_eq!(sim.grant(), busy);
        }
        assert_eq!(sim.pool.key_tenant.len(), 1, "only the busy key is left");
        assert!(sim.pool.key_idle.is_empty());
    }

    /// New keys arriving at half the rate the tenant is served leave a
    /// backlogged sibling about half the grants.
    #[test]
    fn slower_stream_of_new_keys_leaves_the_busy_key_its_turns() {
        let mut sim = Sim::new(1);
        let (tenant, busy) = (Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..1_000 {
            sim.queue(tenant, busy, 100);
        }
        let mut busy_grants = 0;
        for round in 0..400 {
            if round % 2 == 0 {
                sim.queue(tenant, Uuid::new_v4(), 100);
            }
            if sim.grant() == busy {
                busy_grants += 1;
            }
        }
        assert!(
            (180..=220).contains(&busy_grants),
            "busy key got {busy_grants} of 400 grants"
        );
        assert!(sim.pool.key_tenant.len() <= 4);
    }

    /// In a tenant that is never quiet and is served only by new keys, the
    /// key clock does not move, so served keys stay above it. The pool still
    /// keeps no more than its bound of them.
    #[test]
    fn idle_keys_kept_for_a_never_quiet_tenant_are_bounded() {
        let mut sim = Sim::new(4);
        let tenant = Uuid::new_v4();
        for _ in 0..3 {
            sim.queue(tenant, Uuid::new_v4(), 100);
        }
        for _ in 0..(MAX_IDLE_KEYS_PER_TENANT * 3) {
            sim.queue(tenant, Uuid::new_v4(), 100);
            sim.grant();
            let tracked = sim.pool.key_tenant.len();
            assert!(
                tracked <= MAX_IDLE_KEYS_PER_TENANT + 8,
                "{tracked} keys tracked"
            );
        }
        assert!(sim.pool.key_idle[&tenant].len() <= MAX_IDLE_KEYS_PER_TENANT);
    }

    /// A key sending one small request at a time, its next as each one
    /// finishes, next to a sibling with a backlog of long requests, gets at
    /// least one request per sibling request. (Before the key clock it got
    /// far fewer: it came back behind everything the tenant had been served.)
    #[test]
    fn one_at_a_time_key_keeps_pace_with_a_busy_sibling() {
        let mut sim = Sim::new(2);
        let (tenant, busy, light) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..500 {
            sim.queue(tenant, busy, 2_000);
        }
        sim.grant();
        sim.grant();
        sim.queue(tenant, light, 100);
        let mut key = sim.grant();
        let (mut light_grants, mut busy_grants) = (0, 0);
        for _ in 0..300 {
            if key == light {
                light_grants += 1;
            } else {
                busy_grants += 1;
            }
            key = if sim.is_running(light) {
                // The light request finishes and the key sends its next one
                // before the freed slot is handed on.
                let at = sim.running.iter().position(|(_, k)| *k == light).unwrap();
                let (t, k) = sim.running.remove(at).unwrap();
                sim.pool.release(t, k);
                sim.queue(tenant, light, 100);
                let (next, _) = sim.pool.take_next(sim.pool.cap);
                let (t, k, _) = next.unwrap();
                sim.running.push_back((t, k));
                k
            } else {
                // Long busy requests: the oldest one finishes.
                sim.grant()
            };
        }
        assert!(
            light_grants >= busy_grants,
            "light key got {light_grants} grants and the busy key {busy_grants}"
        );
    }

    /// A key moved to another tenant leaves the old tenant's idle set, and
    /// forgetting the old tenant leaves the key, now running and capped under
    /// the new tenant, alone.
    #[test]
    fn moved_key_is_left_alone_by_its_old_tenant() {
        let mut sim = Sim::new(1);
        let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
        let (busy, moved) = (Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..3 {
            sim.queue(old, busy, 100);
        }
        sim.grant();
        sim.queue(old, moved, 2_000);
        sim.grants_before(moved);
        sim.grant();
        assert!(sim.pool.key_idle[&old].contains(&moved));

        let capped = || AdmitRequest::new(new, "m", 100).key(moved, 100).key_cap(1);
        sim.queue_req(capped());
        assert!(!sim
            .pool
            .key_idle
            .get(&old)
            .is_some_and(|i| i.contains(&moved)));
        while sim.pool.queues.contains_key(&old) {
            sim.grant();
        }
        sim.grants_before(moved);
        sim.queue_req(capped());
        sim.pool.forget_tenant(&old);
        assert_eq!(sim.pool.key_tenant.get(&moved), Some(&new));
        assert_eq!(sim.pool.key_cap.get(&moved), Some(&1));
        assert!(sim.pool.key_served.contains_key(&moved));
        assert!(!sim.pool.key_open(&moved), "the key is at its cap of 1");
    }

    /// A key moved while it still had a request running under its old tenant
    /// is forgotten, or kept idle under its new tenant, once the last of its
    /// requests ends, whichever tenant that request ran under.
    #[test]
    fn moved_key_ending_under_its_old_tenant_is_still_tracked_by_its_owner() {
        let mut sim = Sim::new(4);
        let (old, new, moved) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        sim.queue(old, moved, 100);
        sim.grant();
        sim.queue(new, moved, 100);
        sim.grant();
        for (tenant, key) in [(new, moved), (old, moved)] {
            let at = sim
                .running
                .iter()
                .position(|r| *r == (tenant, key))
                .unwrap();
            sim.running.remove(at);
            sim.pool.release(tenant, key);
        }
        let tracked = sim.pool.key_tenant.contains_key(&moved);
        let kept = sim
            .pool
            .key_idle
            .get(&new)
            .is_some_and(|i| i.contains(&moved));
        assert!(
            !tracked || kept,
            "a quiet key is either forgotten or kept idle by its owner"
        );
        assert!(!sim
            .pool
            .key_idle
            .get(&old)
            .is_some_and(|i| i.contains(&moved)));
    }

    /// Shared mode: a claim refused after the key moved does not undo a
    /// charge made before the move, so the key's standing stays at or above
    /// its new tenant's clock.
    #[test]
    fn refused_claim_after_a_move_keeps_the_new_standing() {
        let mut sim = Sim::new(2);
        let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
        let (busy, moved) = (Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..100 {
            sim.queue(new, busy, 100);
        }
        for _ in 0..50 {
            sim.grant();
        }
        sim.queue(old, moved, 500);
        while sim.running.len() >= sim.pool.cap {
            let (t, k) = sim.running.pop_front().unwrap();
            sim.pool.release(t, k);
        }
        let mut taken = None;
        while taken.is_none() {
            let (next, _) = sim.pool.take_next(sim.pool.cap);
            let (t, k, w) = next.expect("a request can be taken");
            if k == moved {
                taken = Some((t, k, w));
            } else {
                sim.running.push_back((t, k));
                let (t, k) = sim.running.pop_front().unwrap();
                sim.pool.release(t, k);
            }
        }
        let (tenant, key, waiter) = taken.unwrap();
        sim.queue(new, moved, 100);
        let start = sim.pool.key_start(new, moved);
        sim.pool.requeue(PendingClaim {
            tenant,
            key,
            waiter,
        });
        let served = sim.pool.key_served[&moved];
        assert!(
            served >= start,
            "standing {served} fell below the new tenant's start {start}"
        );
        assert_eq!(
            sim.pool.queues[&new].key_len(&moved),
            2,
            "it went back in line under its new tenant"
        );
        assert!(!sim
            .pool
            .queues
            .get(&old)
            .is_some_and(|q| q.key_len(&moved) > 0));
    }

    /// Shared mode: a key moved away and back while its claims were out does
    /// not have charges the moves already discarded undone again.
    #[test]
    fn refused_claims_after_moving_away_and_back_stay_at_the_clock() {
        let mut sim = Sim::new(8);
        let (a, b, key) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        sim.queue(a, key, 1_000);
        sim.queue(a, key, 1_000);
        let mut claims = Vec::new();
        for _ in 0..2 {
            let (next, _) = sim.pool.take_next(sim.pool.cap);
            let (tenant, key, waiter) = next.expect("a request can be taken");
            claims.push(PendingClaim {
                tenant,
                key,
                waiter,
            });
        }
        sim.queue(b, key, 100);
        sim.queue(a, key, 100);
        let start = sim.pool.key_start(a, key);
        for claim in claims {
            sim.pool.requeue(claim);
        }
        let served = sim.pool.key_served[&key];
        assert!(
            served >= start,
            "standing {served} fell below the clock's {start}"
        );
    }

    /// A key's requests still queued under its old tenant move with it, in
    /// order, and take the new tenant's group; the old tenant's own keys keep
    /// its slots.
    #[test]
    fn moved_keys_queued_requests_move_with_it() {
        let mut sim = Sim::new(1);
        let (old, new) = (Uuid::new_v4(), Uuid::new_v4());
        let (sibling, moved) = (Uuid::new_v4(), Uuid::new_v4());
        let under = |tenant: Uuid, group: &str, key: Uuid| {
            AdmitRequest::new(tenant, "m", 100)
                .key(key, 100)
                .group(group, 100)
        };
        for _ in 0..300 {
            sim.queue_req(under(old, "g-old", sibling));
        }
        for _ in 0..40 {
            sim.queue_req(under(old, "g-old", moved));
        }
        for _ in 0..10 {
            sim.grant();
        }
        let left = sim.pool.queues[&old].key_len(&moved);
        assert!(left > 0);
        sim.queue_req(under(new, "g-new", moved));
        assert_eq!(sim.pool.queues[&old].key_len(&moved), 0);
        let queue = &sim.pool.queues[&new].keys[&moved];
        assert_eq!(queue.len(), left + 1);
        assert!(queue.iter().all(|w| w.group == "g-new"));
        assert!(queue
            .iter()
            .zip(queue.iter().skip(1))
            .all(|(x, y)| x.enqueued <= y.enqueued));
        assert_eq!(sim.pool.key_tenant[&moved], new);
    }

    /// A queued request counts as passed only when session affinity served
    /// a later one first, not when a later one's grant simply landed first.
    #[test]
    fn only_affinity_reordering_passes_a_queued_request() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut pool = Pool::new(
            PoolKey::Model("m".into()),
            FairshareAlgorithm::Weighted,
            8,
            tx,
        );
        pool.affinity.configure(
            Some(SessionAffinityConfig {
                warm_ttl: Duration::from_secs(120),
                hold: Duration::from_secs(3),
                max_jumps: 8,
                max_bypass: Duration::from_secs(30),
                max_held_percent: 25,
            }),
            Arc::default(),
        );
        let t = Uuid::new_v4();
        let (respond, _admitted) = oneshot::channel();
        pool.enqueue(
            AdmitRequest::new(t, "m", 10).session("old"),
            respond,
            Instant::now(),
            false,
        );
        let passed = |pool: &Pool| pool.queues[&t].keys[&t].front().unwrap().passed;
        let grant = |pool: &mut Pool, jumped: bool| {
            let (respond, _admitted) = oneshot::channel();
            let grant = Grant::new(t, t, Some("new".into()), jumped);
            pool.send(grant, respond, Admission::Queued, Duration::ZERO);
        };
        grant(&mut pool, false);
        assert_eq!(passed(&pool), 0);
        grant(&mut pool, true);
        assert_eq!(passed(&pool), 1);
    }

    /// A tenant that starts queueing again is placed at the pool's virtual
    /// time at its own weight, the scale tenants are ranked on. With a
    /// weight-300 tenant always backlogged, a weight-100 tenant sending one
    /// request at a time gets about one grant in four, not one each time the
    /// busy tenant's service has tripled.
    #[test]
    fn rejoining_tenant_is_placed_at_its_weight() {
        let mut sim = Sim::new(1);
        let (heavy, light) = (Uuid::new_v4(), Uuid::new_v4());
        let req = |tenant: Uuid, weight: i64| {
            AdmitRequest::new(tenant, "m", 100)
                .weight(weight)
                .key(tenant, 100)
        };
        for _ in 0..3_000 {
            sim.queue_req(req(heavy, 300));
        }
        sim.queue_req(req(light, 100));
        let mut light_grants = 0;
        for _ in 0..2_000 {
            let light_queued = sim.pool.queues.get(&light).is_some_and(|q| !q.is_empty());
            if sim.grant() == light {
                light_grants += 1;
            }
            if !light_queued && !sim.is_running(light) {
                sim.queue_req(req(light, 100));
            }
        }
        assert!(
            light_grants >= 400,
            "the weight-100 tenant had {light_grants} of 2,000 grants"
        );
    }

    /// A key moved into a tenant that has been quiet brings its queued
    /// requests in at the pool's virtual time, as a request arriving there
    /// would, so the tenant does not take every grant until its old service
    /// catches up.
    #[test]
    fn moved_key_into_a_quiet_tenant_starts_at_virtual_time() {
        let mut sim = Sim::new(1);
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (moved, other) = (Uuid::new_v4(), Uuid::new_v4());
        sim.queue(b, b, 8_000);
        sim.grant();
        for _ in 0..1_500 {
            sim.queue(a, moved, 100);
            sim.queue(c, other, 100);
        }
        for _ in 0..600 {
            sim.grant();
        }
        assert!(
            sim.pool.served.contains_key(&b),
            "b is kept above the clock"
        );
        sim.queue(b, moved, 100);
        let (mut run, mut longest) = (0, 0);
        for _ in 0..400 {
            if sim.grant() == other {
                run = 0;
            } else {
                run += 1;
                longest = longest.max(run);
            }
        }
        assert!(longest <= 3, "c waited {longest} grants in a row");
    }

    /// Shared mode: a refused claim that goes back in line under the tenant
    /// its key moved to places that tenant as an arrival there would.
    #[test]
    fn refused_claim_moved_into_a_quiet_tenant_starts_at_virtual_time() {
        let mut sim = Sim::new(2);
        let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let (moved, other) = (Uuid::new_v4(), Uuid::new_v4());
        sim.queue(b, b, 8_000);
        sim.grant();
        for _ in 0..600 {
            sim.queue(a, moved, 100);
            sim.queue(c, other, 100);
        }
        for _ in 0..500 {
            sim.grant();
        }
        while let Some((t, k)) = sim.running.pop_front() {
            sim.pool.release(t, k);
        }
        let claim = loop {
            let (next, _) = sim.pool.take_next(sim.pool.cap);
            let (tenant, key, waiter) = next.expect("a request can be taken");
            if key == moved {
                break PendingClaim {
                    tenant,
                    key,
                    waiter,
                };
            }
            sim.pool.release(tenant, key);
        };
        // The key's queued requests drain, then it moves to b with a request
        // that is granted straight away, so nothing is queued under b.
        if let Some(q) = sim.pool.queues.get_mut(&a) {
            if let Some(d) = q.keys.remove(&moved) {
                q.len -= d.len();
                sim.pool.queued_total -= d.len();
            }
        }
        sim.pool
            .track_meta(&AdmitRequest::new(b, "m", 100).key(moved, 100));
        assert!(!sim.pool.queues.get(&b).is_some_and(|q| !q.is_empty()));
        sim.pool.requeue(claim);
        let standing = |t: Uuid| sim.pool.served[&t] / sim.pool.tenant_weight[&t] as f64;
        assert!(
            standing(b) >= standing(c) - 1.0,
            "b went back in line at {}, below c at {}",
            standing(b),
            standing(c)
        );
    }

    fn affinity_pool(max_jumps: u32) -> Pool {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut pool = Pool::new(
            PoolKey::Model("m".into()),
            FairshareAlgorithm::Weighted,
            8,
            tx,
        );
        pool.affinity.configure(
            Some(SessionAffinityConfig {
                warm_ttl: Duration::from_secs(120),
                hold: Duration::from_secs(3),
                max_jumps,
                max_bypass: Duration::from_secs(30),
                max_held_percent: 25,
            }),
            Arc::default(),
        );
        pool
    }

    /// Session affinity finds a moved key's queued requests under its new
    /// tenant: steps served in a slot of the old tenant still count as
    /// passing them, so the pass limit still holds.
    #[test]
    fn pass_limit_counts_a_moved_keys_queued_requests() {
        let mut pool = affinity_pool(2);
        let (a, b, key) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let cold = AdmitRequest::new(a, "m", 10).key(key, 100);
        pool.track_meta(&cold);
        let (respond, _cold) = oneshot::channel();
        pool.enqueue(cold, respond, Instant::now(), false);
        pool.track_meta(&AdmitRequest::new(b, "m", 10).key(key, 100));
        assert_eq!(pool.queues[&b].key_len(&key), 1);
        for _ in 0..2 {
            let (respond, _admitted) = oneshot::channel();
            let grant = Grant::new(a, key, Some("loop".into()), true);
            pool.send(grant, respond, Admission::Fast, Duration::ZERO);
        }
        assert_eq!(pool.queues[&b].keys[&key].front().unwrap().passed, 2);
        assert!(pool.key_passed_out(a, key));
    }

    /// A slot held for a session under the old tenant goes to that
    /// session's request queued under the tenant the key moved to.
    #[test]
    fn held_slot_reaches_its_sessions_request_after_a_move() {
        let mut pool = affinity_pool(8);
        let (a, b, key) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let session = affinity::scoped(key, "loop");
        pool.track_meta(&AdmitRequest::new(a, "m", 10).key(key, 100));
        pool.occupy(a, key, 10);
        pool.affinity.hold(&session, a, key, Instant::now());
        let mut step = AdmitRequest::new(a, "m", 10).key(key, 100);
        step.session = Some(session.clone());
        pool.track_meta(&step);
        let (respond, mut admitted) = oneshot::channel();
        pool.enqueue(step, respond, Instant::now(), false);
        pool.track_meta(&AdmitRequest::new(b, "m", 10).key(key, 100));
        assert_eq!(pool.redeem_queued(), (1, 0));
        assert!(admitted.try_recv().is_ok());
        assert_eq!(pool.affinity.held(), 0);
    }
}
