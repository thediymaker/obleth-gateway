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
mod pool;
mod replicas;
mod scheduler;
pub mod shared;
mod snapshot;
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
use pool::*;
pub use replicas::{replica_share, ReplicaTracker};
pub use scheduler::CLUSTER_READ_TIMEOUT;
use scheduler::*;
pub use shared::{
    AcquireOutcome, ClusterCounts, InMemoryCluster, InMemorySlots, PoolHoldings, ReconcileOutcome,
    SharedSlots, SharedSlotsConfig, SlotClaim, SlotDenial, SlotError, SlotFuture, SlotHold,
    SlotMode, SlotRelease,
};
pub use snapshot::*;
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

/// The fair-share group of a tenant that names none.
pub const DEFAULT_GROUP: &str = "default";
/// A group's weight when none is known.
pub const DEFAULT_GROUP_WEIGHT: i64 = 100;

/// Display name of the shared pool for requests with no resolved route.
pub const UNROUTED_POOL: &str = "(unrouted)";

/// What a model's background pool is shown as: the model name and this.
pub const BACKGROUND_POOL_SUFFIX: &str = " (background)";

/// How long a pool with no permits and no waiters is kept before housekeeping
/// drops it.
pub const IDLE_POOL_TTL: Duration = Duration::from_secs(600);

/// Most idle keys a pool keeps per tenant for the lead they have over the
/// tenant's key clock. Past it, the key with the smallest lead is forgotten,
/// which forgives that lead.
const MAX_IDLE_KEYS_PER_TENANT: usize = 512;

/// How many of the smallest-lead idle keys go at once when a tenant is over
/// [`MAX_IDLE_KEYS_PER_TENANT`], so the ranking pass runs once per batch.
const IDLE_KEY_EVICT_BATCH: usize = MAX_IDLE_KEYS_PER_TENANT / 8;

/// Housekeeping period: a sixtieth of the idle TTL, kept between 1 and 10 s.
fn housekeeping_interval(idle_pool_ttl: Duration) -> Duration {
    (idle_pool_ttl / 60).clamp(Duration::from_secs(1), Duration::from_secs(10))
}

/// Which scheduling pool an admission lands in.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PoolKey {
    /// The pool of a resolved model route.
    Model(String),
    /// Background work (batch items, and requests from keys with background
    /// priority) for a model. It has its own queues and fairness, and shares
    /// the model's slots with [`PoolKey::Model`]: it is served only while the
    /// model has no interactive request waiting, and only up to a share of
    /// the model's pool (see [`AdmitRequest::background_share`]). A request
    /// here holds slots by its prompt length (see [`AdmitRequest::slots`]).
    Background(String),
    /// One pool shared by every request that did not resolve to a route, so
    /// arbitrary model strings cannot each mint a pool of their own.
    Unrouted,
}

impl PoolKey {
    fn display_name(&self) -> String {
        match self {
            PoolKey::Model(name) => name.clone(),
            PoolKey::Background(name) => format!("{name}{BACKGROUND_POOL_SUFFIX}"),
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
/// work may fill when the request does not say; and the prompt tokens per
/// slot a background request holds when the gateway does not say. Defined
/// with the settings that override them.
pub use obleth_config::{DEFAULT_BACKGROUND_SHARE, DEFAULT_BACKGROUND_SLOT_TOKENS};

/// The slots a background request with a prompt of `prompt_tokens` asks to
/// hold in its model's pool: one per `slot_tokens` of prompt, rounded up, and
/// at least one. A long prompt is prefilled in pieces that hold up every
/// stream on the replica, so it takes room in proportion. `slot_tokens` 0
/// is one slot whatever the prompt. The scheduler caps it at the
/// background share of the pool (see [`AdmitRequest::slots`]).
pub fn background_slots(prompt_tokens: u32, slot_tokens: u32) -> usize {
    if slot_tokens == 0 {
        return 1;
    }
    (prompt_tokens.div_ceil(slot_tokens) as usize).max(1)
}

/// `share` of `cap`, rounded down, for a background limit.
pub fn background_cap(cap: usize, share: f64) -> usize {
    let share = if share.is_finite() {
        share.clamp(0.0, 1.0)
    } else {
        DEFAULT_BACKGROUND_SHARE
    };
    (cap as f64 * share).floor() as usize
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
    /// For a background pool: the slots the request asks to hold, from its
    /// prompt length (see [`background_slots`]). Capped at the background
    /// share of the pool when it is admitted, so a prompt bigger than that
    /// still runs, alone. Every other pool takes one slot per request.
    pub slots: usize,
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
            group: DEFAULT_GROUP.into(),
            group_weight: DEFAULT_GROUP_WEIGHT,
            model: model.into(),
            model_max_in_flight: None,
            tenant_max_in_flight: None,
            key_max_in_flight: None,
            cost,
            end_user: None,
            session: None,
            affinity: PoolAffinity::Scheduler,
            background_share: None,
            slots: 1,
        }
    }

    /// The share of the model's pool a background request may fill.
    pub fn background_share(mut self, share: f64) -> Self {
        self.background_share = Some(share);
        self
    }

    /// The slots a background request asks to hold (see
    /// [`AdmitRequest::slots`]).
    pub fn slots(mut self, slots: usize) -> Self {
        self.slots = slots.max(1);
        self
    }

    pub fn weighted(tenant: Uuid, weight: i64, cost: u32) -> Self {
        Self::new(tenant, DEFAULT_GROUP, cost).weight(weight)
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
    /// The pool slots it holds; all of them go back together.
    slots: usize,
}

impl Permit {
    /// The pool slots this admission holds: one, or more for background
    /// work with a long prompt (see [`AdmitRequest::slots`]).
    pub fn slots(&self) -> usize {
        self.slots
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(tx) = self.release.take() {
            let _ = tx.send(Ctl::Release {
                tenant: self.tenant,
                key: self.key,
                pool: std::mem::replace(&mut self.pool, PoolKey::Unrouted),
                session: self.session.take(),
                slots: self.slots,
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
    /// pool together, then each of `pools` in slots (a background pool counts
    /// them by prompt length; see [`background_slots`]). `None` unless this
    /// replica is in shared mode and the backend answers.
    pub async fn cluster_in_flight(&self, pools: &[PoolKey]) -> Option<(usize, Vec<usize>)> {
        let backend = self.shared_backend().ok()?;
        let ids = pools.iter().map(PoolKey::slot_id).collect();
        bounded(CLUSTER_READ_TIMEOUT, backend.totals(ids))
            .await
            .ok()
    }

    /// The shared backend, while this replica is in shared mode: the one
    /// gate for every cluster-wide read.
    fn shared_backend(&self) -> Result<Arc<dyn SharedSlots>, SlotError> {
        if self.stats.mode() != SlotMode::Shared {
            return Err(SlotError("this replica is not in shared slot mode".into()));
        }
        self.shared
            .get()
            .cloned()
            .ok_or_else(|| SlotError("shared slots are not enabled".into()))
    }

    /// This replica's part of a cluster-wide read, asked of the scheduler
    /// with `ask`, and the views every other live replica last published.
    async fn with_peer_views<T>(
        &self,
        ask: impl FnOnce(oneshot::Sender<T>) -> Ctl,
    ) -> Result<(T, PeerViews), SlotError> {
        let backend = self.shared_backend()?;
        let (respond, rx) = oneshot::channel();
        let gone = || SlotError("the fairshare scheduler is not running".into());
        self.ctl.send(ask(respond)).map_err(|_| gone())?;
        let local = rx.await.map_err(|_| gone())?;
        let peers = bounded(CLUSTER_READ_TIMEOUT, backend.peer_views()).await?;
        Ok((local, peers))
    }

    /// Shared mode: this replica's scheduler state merged with the state
    /// every other live replica last published, so queues, tenants, groups
    /// and keys are counted across the fleet (see [`view`]). Each pool's
    /// `cap` is its configured size and its `cluster_in_flight` is `None`
    /// (read the slot counts with [`FairShare::cluster_in_flight`]).
    ///
    /// Fails outside shared mode or when the backend does not answer within
    /// [`CLUSTER_READ_TIMEOUT`]; a caller then has only
    /// [`FairShare::snapshot`], this replica's own.
    pub async fn cluster_snapshot(&self) -> Result<ClusterSnapshot, SlotError> {
        let (local, peers) = self
            .with_peer_views(|respond| Ctl::View { respond })
            .await?;
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
                    slots: 0,
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
        self.snapshot_with(true).await
    }

    /// [`FairShare::snapshot`] without each pool's tenants and keys (left
    /// empty): pool and group counts only, for a caller that reads members
    /// from elsewhere (the cluster view in `shared` mode).
    pub async fn pool_snapshot(&self) -> Option<FairshareSnapshot> {
        self.snapshot_with(false).await
    }

    async fn snapshot_with(&self, members: bool) -> Option<FairshareSnapshot> {
        let (respond, rx) = oneshot::channel();
        self.ctl.send(Ctl::Snapshot { members, respond }).ok()?;
        rx.await.ok()
    }

    /// Requests waiting in each model's background pool, per key: (model,
    /// key) to count, keys with nothing waiting left out.
    pub async fn background_queued(&self) -> Option<HashMap<(String, Uuid), usize>> {
        let (respond, rx) = oneshot::channel();
        self.ctl.send(Ctl::BackgroundQueued { respond }).ok()?;
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
    /// from this replica's own sample and the views its peers last published
    /// (the same state [`FairShare::cluster_snapshot`] reads). `None` outside
    /// shared mode or when the backend does not answer within
    /// [`CLUSTER_READ_TIMEOUT`]; the sampler then records
    /// [`FairShare::sample`], this replica's own.
    pub async fn cluster_sample(&self) -> Option<FairshareSample> {
        let (local, peers) = self
            .with_peer_views(|respond| Ctl::Sample { respond })
            .await
            .ok()?;
        let peers: Vec<&ReplicaView> = peers.views.iter().map(|p| &p.view).collect();
        Some(local.with_peers(&peers))
    }
}
