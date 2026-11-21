//! The serde snapshot types a fairshare view is reported in.

use super::*;

/// Live counters for metrics/dashboards.
#[derive(Debug, Default)]
pub struct Stats {
    /// This replica's in-flight requests.
    pub in_flight: AtomicUsize,
    pub queued: AtomicI64,
    /// Live gateway replicas.
    pub replicas: AtomicUsize,
    pub(crate) mode: AtomicU8,
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
    /// The score the scheduler ranks the tenant by: service in this busy
    /// period plus the virtual time it was placed at on joining. Summed over
    /// replicas in a cluster view.
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
    /// The pool slots those requests hold here: `in_flight` for an
    /// interactive pool; in a background pool a request with a long prompt
    /// holds several (see [`background_slots`]), and the pool's cap counts
    /// slots.
    #[serde(default)]
    pub slots: usize,
    /// Cluster-wide slots held in the pool as last reported by the shared
    /// slot backend (requests, apart from background pools; see `slots`);
    /// `None` outside shared mode or before this replica has heard.
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
