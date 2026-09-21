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

mod algorithm;
mod capacity;

pub use algorithm::{group_slot_caps, weighted_caps};
pub use capacity::{CapacityProvider, StaticCapacity};

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use obleth_config::{Admission, FairshareAlgorithm};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

/// Pool size for a model that has no explicit `max_in_flight`.
pub const DEFAULT_MODEL_MAX_IN_FLIGHT: usize = 32;

/// Live counters for metrics/dashboards.
#[derive(Debug, Default)]
pub struct Stats {
    pub in_flight: AtomicUsize,
    pub queued: AtomicI64,
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
    /// Per-model in-flight ceiling for this tenant, when one is set.
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
    pub cap: usize,
    pub in_flight: usize,
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
    /// Total in-flight ceiling across all pools.
    pub max_in_flight: usize,
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
    model: String,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(tx) = self.release.take() {
            let _ = tx.send(Ctl::Release {
                tenant: self.tenant,
                key: self.key,
                model: self.model.clone(),
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
        req: AdmitRequest,
        respond: oneshot::Sender<Admitted>,
        enqueued: Instant,
    },
    Release {
        tenant: Uuid,
        key: Uuid,
        model: String,
    },
    Snapshot {
        respond: oneshot::Sender<FairshareSnapshot>,
    },
}

/// Handle to the fairshare scheduler. Cheap to clone.
#[derive(Clone)]
pub struct FairShare {
    ctl: mpsc::UnboundedSender<Ctl>,
    stats: Arc<Stats>,
    model_load: Arc<RwLock<HashMap<String, usize>>>,
}

impl FairShare {
    pub fn start(
        capacity: Arc<dyn CapacityProvider>,
        algorithm: FairshareAlgorithm,
        default_model_max_in_flight: usize,
    ) -> Self {
        let (ctl, rx) = mpsc::unbounded_channel();
        let stats = Arc::new(Stats::default());
        let model_load = Arc::new(RwLock::new(HashMap::new()));
        let scheduler = Scheduler {
            algorithm,
            capacity,
            default_model_cap: default_model_max_in_flight.max(1),
            pools: HashMap::new(),
            pool_order: Vec::new(),
            cursor: 0,
            in_flight: 0,
            queued_total: 0,
            ctl_tx: ctl.clone(),
            stats: stats.clone(),
            model_load: model_load.clone(),
        };
        tokio::spawn(scheduler.run(rx));
        FairShare {
            ctl,
            stats,
            model_load,
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

    pub async fn snapshot(&self) -> Option<FairshareSnapshot> {
        let (respond, rx) = oneshot::channel();
        self.ctl.send(Ctl::Snapshot { respond }).ok()?;
        rx.await.ok()
    }

    pub async fn admit(&self, req: AdmitRequest) -> Option<Admitted> {
        let (respond, rx) = oneshot::channel();
        self.ctl
            .send(Ctl::Admit {
                req,
                respond,
                enqueued: Instant::now(),
            })
            .ok()?;
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
    respond: oneshot::Sender<Admitted>,
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
    model: String,
    algorithm: FairshareAlgorithm,
    cap: usize,
    in_flight: usize,
    queued_total: usize,
    tenant_in_flight: HashMap<Uuid, usize>,
    tenant_group: HashMap<Uuid, String>,
    group_weight: HashMap<String, i64>,
    tenant_weight: HashMap<Uuid, i64>,
    tenant_cap: HashMap<Uuid, usize>,
    queues: HashMap<Uuid, TenantQueue>,
    served: HashMap<Uuid, f64>,
    key_in_flight: HashMap<Uuid, usize>,
    key_served: HashMap<Uuid, f64>,
    key_weight: HashMap<Uuid, i64>,
    key_cap: HashMap<Uuid, usize>,
    key_tenant: HashMap<Uuid, Uuid>,
    virtual_time: f64,
    ctl_tx: mpsc::UnboundedSender<Ctl>,
}

impl Pool {
    fn new(
        model: String,
        algorithm: FairshareAlgorithm,
        cap: usize,
        ctl_tx: mpsc::UnboundedSender<Ctl>,
    ) -> Self {
        Pool {
            model,
            algorithm,
            cap: cap.max(1),
            in_flight: 0,
            queued_total: 0,
            tenant_in_flight: HashMap::new(),
            tenant_group: HashMap::new(),
            group_weight: HashMap::new(),
            tenant_weight: HashMap::new(),
            tenant_cap: HashMap::new(),
            queues: HashMap::new(),
            served: HashMap::new(),
            key_in_flight: HashMap::new(),
            key_served: HashMap::new(),
            key_weight: HashMap::new(),
            key_cap: HashMap::new(),
            key_tenant: HashMap::new(),
            virtual_time: 0.0,
            ctl_tx,
        }
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
        match self.tenant_cap.get(tenant) {
            Some(cap) => self.tenant_in_flight.get(tenant).copied().unwrap_or(0) < *cap,
            None => true,
        }
    }

    fn key_has_slot(&self, key: &Uuid) -> bool {
        match self.key_cap.get(key) {
            Some(cap) => self.key_in_flight.get(key).copied().unwrap_or(0) < *cap,
            None => true,
        }
    }

    /// A tenant can be picked only if it is under its own cap and at least one
    /// of its queued keys is under its key cap.
    fn tenant_is_eligible(&self, tenant: &Uuid) -> bool {
        if !self.tenant_has_slot(tenant) {
            return false;
        }
        self.queues
            .get(tenant)
            .map(|q| {
                q.keys
                    .iter()
                    .any(|(k, d)| !d.is_empty() && self.key_has_slot(k))
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

    fn compute_group_caps(&self) -> HashMap<String, usize> {
        group_slot_caps(self.cap, &self.active_groups())
    }

    fn can_grant_immediately(&self, req: &AdmitRequest) -> bool {
        if self.in_flight >= self.cap || self.queued_total > 0 {
            return false;
        }
        if !self.tenant_has_slot(&req.tenant) || !self.key_has_slot(&req.key) {
            return false;
        }
        match self.algorithm {
            FairshareAlgorithm::Weighted => true,
            FairshareAlgorithm::Hierarchical => {
                let caps = self.compute_group_caps();
                let cap = caps.get(&req.group).copied().unwrap_or(self.cap);
                self.group_in_flight(&req.group) < cap
            }
        }
    }

    fn occupy(&mut self, tenant: Uuid, key: Uuid, cost: u32) {
        self.in_flight += 1;
        *self.tenant_in_flight.entry(tenant).or_insert(0) += 1;
        *self.key_in_flight.entry(key).or_insert(0) += 1;
        *self.served.entry(tenant).or_insert(self.virtual_time) += cost as f64;
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
                model: self.model.clone(),
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
    ) {
        let tenant_queue_empty = self
            .queues
            .get(&req.tenant)
            .is_none_or(TenantQueue::is_empty);
        if tenant_queue_empty {
            let entry = self.served.entry(req.tenant).or_insert(self.virtual_time);
            if *entry < self.virtual_time {
                *entry = self.virtual_time;
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
                respond,
            },
        );
        self.queued_total += 1;
    }

    /// Admit one queued waiter if the pool has a free slot. Returns whether a
    /// grant happened so the scheduler can account the global ceiling.
    fn dispatch_one(&mut self) -> bool {
        if self.in_flight >= self.cap || self.queued_total == 0 {
            return false;
        }
        let Some(tenant) = self.pick_tenant() else {
            return false;
        };
        let Some(key) = self.pick_key(&tenant) else {
            return false;
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
        self.track_waiter_meta(tenant, key, &waiter);
        self.occupy(tenant, key, waiter.cost);
        self.advance_virtual_time();
        let waited = waiter.enqueued.elapsed();
        self.send(tenant, key, waiter.respond, Admission::Queued, waited);
        true
    }

    fn pick_tenant(&self) -> Option<Uuid> {
        match self.algorithm {
            FairshareAlgorithm::Weighted => self.pick_tenant_weighted(),
            FairshareAlgorithm::Hierarchical => self.pick_tenant_hierarchical(),
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

    fn pick_tenant_hierarchical(&self) -> Option<Uuid> {
        let caps = self.compute_group_caps();
        let mut in_flight_by_group: HashMap<&str, usize> = HashMap::new();
        for (tenant, n) in &self.tenant_in_flight {
            if *n > 0 {
                if let Some(g) = self.tenant_group.get(tenant) {
                    *in_flight_by_group.entry(g.as_str()).or_insert(0) += *n;
                }
            }
        }
        let mut served_by_group: HashMap<&str, f64> = HashMap::new();
        for (tenant, s) in &self.served {
            if let Some(g) = self.tenant_group.get(tenant) {
                *served_by_group.entry(g.as_str()).or_insert(0.0) += *s;
            }
        }
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

            let cap = caps.get(&group).copied().unwrap_or(self.cap);
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

    /// Within the winning tenant, the key with the lowest weight-adjusted
    /// service debt goes first; ties break on the older head-of-line request.
    fn pick_key(&self, tenant: &Uuid) -> Option<Uuid> {
        let queue = self.queues.get(tenant)?;
        let mut best: Option<(Uuid, f64, Instant)> = None;
        for (key, deque) in &queue.keys {
            let Some(head) = deque.front() else { continue };
            if !self.key_has_slot(key) {
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
        self.tenant_weight.remove(tenant);
        self.tenant_group.remove(tenant);
        self.tenant_cap.remove(tenant);
        self.queues.remove(tenant);
    }

    fn is_idle(&self) -> bool {
        self.in_flight == 0 && self.queued_total == 0
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
        self.key_in_flight.clear();
        self.key_served.clear();
        self.key_weight.clear();
        self.key_cap.clear();
        self.key_tenant.clear();
        self.virtual_time = 0.0;
    }

    fn snapshot(&self) -> ModelPoolFairshare {
        let ids: HashSet<Uuid> = self
            .queues
            .keys()
            .copied()
            .chain(self.tenant_in_flight.keys().copied())
            .chain(self.served.keys().copied())
            .collect();
        let active = self.active_groups();
        let group_caps = self.compute_group_caps();
        let total_group_weight: i64 = active.iter().map(|(_, w)| (*w).max(1)).sum();

        let mut names: HashSet<String> = active.iter().map(|(n, _)| n.clone()).collect();
        for tenant in &ids {
            if let Some(g) = self.tenant_group.get(tenant) {
                names.insert(g.clone());
            }
        }

        // One pass over the tenants with state, bucketed by group: in-flight,
        // queued and served per group, so the loop below does not re-scan every
        // tenant for every group.
        let mut group_totals: HashMap<&str, (usize, usize, f64)> = HashMap::new();
        for tenant in &ids {
            let Some(group) = self.tenant_group.get(tenant) else {
                continue;
            };
            let entry = group_totals.entry(group.as_str()).or_insert((0, 0, 0.0));
            entry.0 += self.tenant_in_flight.get(tenant).copied().unwrap_or(0);
            entry.1 += self.queues.get(tenant).map(|q| q.len).unwrap_or(0);
            entry.2 += self.served.get(tenant).copied().unwrap_or(0.0);
        }
        let mut groups: Vec<GroupFairshare> = names
            .into_iter()
            .map(|name| {
                let weight = self.group_weight.get(&name).copied().unwrap_or(100).max(1);
                let (in_flight, queued, served_tokens) = group_totals
                    .get(name.as_str())
                    .copied()
                    .unwrap_or((0, 0, 0.0));
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
                    max_in_flight: self.tenant_cap.get(&tenant_id).copied(),
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
                    max_in_flight: self.key_cap.get(&key_id).copied(),
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
            in_flight: self.in_flight,
            queued: self.queued_total,
            borrowed: groups.iter().map(|g| g.borrowed).sum(),
            groups,
            tenants,
            keys,
        }
    }
}

struct Scheduler {
    algorithm: FairshareAlgorithm,
    capacity: Arc<dyn CapacityProvider>,
    default_model_cap: usize,
    pools: HashMap<String, Pool>,
    /// Insertion order of pools, for round-robin when the ceiling binds.
    pool_order: Vec<String>,
    cursor: usize,
    in_flight: usize,
    queued_total: usize,
    ctl_tx: mpsc::UnboundedSender<Ctl>,
    stats: Arc<Stats>,
    model_load: Arc<RwLock<HashMap<String, usize>>>,
}

impl Scheduler {
    async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Ctl>) {
        while let Some(msg) = rx.recv().await {
            match msg {
                Ctl::Admit {
                    req,
                    respond,
                    enqueued,
                } => {
                    let ceiling = self.capacity.max_in_flight();
                    let headroom = self.in_flight < ceiling;
                    let default_cap = self.default_model_cap;
                    let model = req.model.clone();
                    let pool = self.pool_mut(&model);
                    pool.cap = req
                        .model_max_in_flight
                        .filter(|c| *c > 0)
                        .unwrap_or(default_cap);
                    pool.track_meta(&req);
                    if headroom && pool.can_grant_immediately(&req) {
                        pool.grant_fast(req, respond);
                        self.in_flight += 1;
                    } else {
                        pool.enqueue(req, respond, enqueued);
                        self.queued_total += 1;
                        self.dispatch_all();
                    }
                    self.publish_model_load(&model);
                    self.publish_stats();
                }
                Ctl::Release { tenant, key, model } => {
                    self.in_flight = self.in_flight.saturating_sub(1);
                    if let Some(pool) = self.pools.get_mut(&model) {
                        pool.release(tenant, key);
                        if pool.is_idle() {
                            pool.reset();
                        }
                    }
                    self.publish_model_load(&model);
                    self.dispatch_all();
                    self.publish_stats();
                }
                Ctl::Snapshot { respond } => {
                    let _ = respond.send(self.build_snapshot());
                }
            }
        }
    }

    fn pool_mut(&mut self, model: &str) -> &mut Pool {
        if !self.pools.contains_key(model) {
            self.pools.insert(
                model.to_string(),
                Pool::new(
                    model.to_string(),
                    self.algorithm,
                    self.default_model_cap,
                    self.ctl_tx.clone(),
                ),
            );
            self.pool_order.push(model.to_string());
        }
        self.pools.get_mut(model).expect("pool just inserted")
    }

    /// Serve queued waiters across pools. Each pass grants at most one slot per
    /// pool starting from a rotating cursor, so a binding ceiling is shared
    /// round-robin rather than by map order.
    fn dispatch_all(&mut self) {
        let n = self.pool_order.len();
        if n == 0 {
            return;
        }
        let ceiling = self.capacity.max_in_flight();
        let mut progressed = true;
        while progressed && self.in_flight < ceiling && self.queued_total > 0 {
            progressed = false;
            for i in 0..n {
                if self.in_flight >= ceiling {
                    break;
                }
                let idx = (self.cursor + i) % n;
                let granted = match self.pools.get_mut(self.pool_order[idx].as_str()) {
                    Some(pool) if pool.queued_total > 0 => pool.dispatch_one(),
                    _ => false,
                };
                if granted {
                    self.in_flight += 1;
                    self.queued_total = self.queued_total.saturating_sub(1);
                    progressed = true;
                    let name = self.pool_order[idx].clone();
                    self.publish_model_load(&name);
                }
            }
            self.cursor = (self.cursor + 1) % n;
        }
    }

    fn publish_model_load(&self, model: &str) {
        if let Ok(mut shared) = self.model_load.write() {
            match self.pools.get(model).map(|p| p.in_flight) {
                Some(n) if n > 0 => {
                    shared.insert(model.to_string(), n);
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
    }

    fn build_snapshot(&self) -> FairshareSnapshot {
        let mut pools: Vec<ModelPoolFairshare> = self.pools.values().map(Pool::snapshot).collect();
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
            max_in_flight: self.capacity.max_in_flight(),
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
