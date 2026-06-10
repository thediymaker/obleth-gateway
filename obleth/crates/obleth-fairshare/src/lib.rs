//! Weighted and hierarchical fairshare admission control.
//!
//! # Model
//! A single scheduler task owns all admission state. Under the **weighted**
//! algorithm, tenants compete globally by minimizing `served / weight`. Under
//! **hierarchical** (Option B), global capacity is split between fairshare
//! groups by group weight, then split among tenants within each group in
//! proportion to each tenant's weight (equal weights give an even split).

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
    pub in_flight: usize,
    pub queued: usize,
    pub served_tokens: f64,
    pub share_score: f64,
    pub weight_share: f64,
}

/// Point-in-time fairshare state across all active tenants.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FairshareSnapshot {
    pub algorithm: String,
    pub max_in_flight: usize,
    pub global_in_flight: usize,
    pub global_queued: usize,
    pub groups: Vec<GroupFairshare>,
    pub tenants: Vec<TenantFairshare>,
    /// Live in-flight request count per model name. Used by the `auto` router
    /// to prefer models with spare capacity. Only includes models that
    /// currently have at least one in-flight request.
    #[serde(default)]
    pub model_in_flight: HashMap<String, usize>,
}

/// Context passed to the scheduler for a single admission attempt.
#[derive(Debug, Clone)]
pub struct AdmitRequest {
    pub tenant: Uuid,
    pub weight: i64,
    pub group: String,
    pub group_weight: i64,
    pub model: String,
    pub model_max_in_flight: Option<usize>,
    pub cost: u32,
}

impl AdmitRequest {
    pub fn weighted(tenant: Uuid, weight: i64, cost: u32) -> Self {
        Self {
            tenant,
            weight,
            group: "default".into(),
            group_weight: 100,
            model: "default".into(),
            model_max_in_flight: None,
            cost,
        }
    }
}

/// A held admission slot. Dropping it returns capacity to the scheduler.
pub struct Permit {
    release: Option<mpsc::UnboundedSender<Ctl>>,
    tenant: Uuid,
    model: String,
}

impl Drop for Permit {
    fn drop(&mut self) {
        if let Some(tx) = self.release.take() {
            let _ = tx.send(Ctl::Release {
                tenant: self.tenant,
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
    /// Mirror of the scheduler's per-model in-flight counts, maintained on
    /// grant/release so readers never round-trip through the scheduler task.
    model_load: Arc<RwLock<HashMap<String, usize>>>,
}

impl FairShare {
    pub fn start(
        capacity: Arc<dyn CapacityProvider>,
        algorithm: FairshareAlgorithm,
    ) -> Self {
        let (ctl, rx) = mpsc::unbounded_channel();
        let stats = Arc::new(Stats::default());
        let model_load = Arc::new(RwLock::new(HashMap::new()));
        let scheduler = Scheduler {
            algorithm,
            capacity,
            in_flight: 0,
            queued_total: 0,
            tenant_in_flight: HashMap::new(),
            model_in_flight: HashMap::new(),
            tenant_group: HashMap::new(),
            group_weight: HashMap::new(),
            model_cap: HashMap::new(),
            tenant_weight: HashMap::new(),
            queues: HashMap::new(),
            served: HashMap::new(),
            virtual_time: 0.0,
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

    /// Live in-flight request count per model. Cheap (one read-lock + clone of
    /// a small map); used by the `auto` router on every request, so it must not
    /// serialize through the scheduler task the way [`Self::snapshot`] does.
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
    group: String,
    group_weight: i64,
    model: String,
    model_max_in_flight: Option<usize>,
    cost: u32,
    enqueued: Instant,
    respond: oneshot::Sender<Admitted>,
}

struct Scheduler {
    algorithm: FairshareAlgorithm,
    capacity: Arc<dyn CapacityProvider>,
    in_flight: usize,
    /// Total queued waiters across all tenants, maintained incrementally so the
    /// hot admit/dispatch paths never re-sum every queue.
    queued_total: usize,
    tenant_in_flight: HashMap<Uuid, usize>,
    model_in_flight: HashMap<String, usize>,
    tenant_group: HashMap<Uuid, String>,
    group_weight: HashMap<String, i64>,
    model_cap: HashMap<String, usize>,
    tenant_weight: HashMap<Uuid, i64>,
    queues: HashMap<Uuid, VecDeque<Waiter>>,
    served: HashMap<Uuid, f64>,
    virtual_time: f64,
    ctl_tx: mpsc::UnboundedSender<Ctl>,
    stats: Arc<Stats>,
    /// Shared mirror of `model_in_flight` read by [`FairShare::model_load`].
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
                    self.track_tenant_meta(&req);
                    self.track_model_meta(&req);
                    let max = self.capacity.max_in_flight();
                    if self.can_grant_immediately(&req, max) {
                        self.grant(
                            req.tenant,
                            req.weight,
                            req.model,
                            req.cost,
                            respond,
                            enqueued,
                            Admission::Fast,
                            Duration::ZERO,
                        );
                    } else {
                        if self.queues.get(&req.tenant).is_none_or(VecDeque::is_empty) {
                            let entry = self.served.entry(req.tenant).or_insert(self.virtual_time);
                            if *entry < self.virtual_time {
                                *entry = self.virtual_time;
                            }
                        }
                        self.queues
                            .entry(req.tenant)
                            .or_default()
                            .push_back(Waiter {
                                weight: req.weight,
                                group: req.group,
                                group_weight: req.group_weight,
                                model: req.model,
                                model_max_in_flight: req.model_max_in_flight,
                                cost: req.cost,
                                enqueued,
                                respond,
                            });
                        self.queued_total += 1;
                        self.stats
                            .queued
                            .store(self.queued_total as i64, Ordering::Relaxed);
                        self.dispatch();
                    }
                }
                Ctl::Release { tenant, model } => {
                    self.in_flight = self.in_flight.saturating_sub(1);
                    self.stats
                        .in_flight
                        .store(self.in_flight, Ordering::Relaxed);
                    if let Some(n) = self.tenant_in_flight.get_mut(&tenant) {
                        *n = n.saturating_sub(1);
                        if *n == 0 {
                            self.tenant_in_flight.remove(&tenant);
                        }
                    }
                    self.dec_model_in_flight(&model);
                    self.dispatch();
                }
                Ctl::Snapshot { respond } => {
                    let _ = respond.send(self.build_snapshot());
                }
            }
        }
    }

    fn track_tenant_meta(&mut self, req: &AdmitRequest) {
        self.tenant_weight.insert(req.tenant, req.weight.max(1));
        self.tenant_group.insert(req.tenant, req.group.clone());
        self.group_weight
            .insert(req.group.clone(), req.group_weight.max(1));
    }

    fn track_model_meta(&mut self, req: &AdmitRequest) {
        if let Some(cap) = req.model_max_in_flight.filter(|cap| *cap > 0) {
            self.model_cap.insert(req.model.clone(), cap);
        } else {
            self.model_cap.remove(&req.model);
        }
    }

    /// Bump a model's in-flight count and propagate the change to the shared
    /// mirror read by [`FairShare::model_load`].
    fn inc_model_in_flight(&mut self, model: &str) {
        *self
            .model_in_flight
            .entry(model.to_string())
            .or_insert(0) += 1;
        self.publish_model_load(model);
    }

    /// Decrement a model's in-flight count (dropping the entry at zero) and
    /// propagate the change to the shared mirror.
    fn dec_model_in_flight(&mut self, model: &str) {
        if let Some(n) = self.model_in_flight.get_mut(model) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.model_in_flight.remove(model);
            }
        }
        self.publish_model_load(model);
    }

    fn publish_model_load(&self, model: &str) {
        if let Ok(mut shared) = self.model_load.write() {
            match self.model_in_flight.get(model) {
                Some(n) => {
                    shared.insert(model.to_string(), *n);
                }
                None => {
                    shared.remove(model);
                }
            }
        }
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

    fn group_queued(&self, group: &str) -> usize {
        self.queues
            .iter()
            .filter(|(tenant, q)| {
                !q.is_empty() && self.tenant_group.get(*tenant).map(|g| g.as_str()) == Some(group)
            })
            .map(|(_, q)| q.len())
            .sum()
    }

    fn group_served(&self, group: &str) -> f64 {
        self.served
            .iter()
            .filter(|(tenant, _)| self.tenant_group.get(*tenant).map(|g| g.as_str()) == Some(group))
            .map(|(_, s)| s)
            .sum()
    }

    fn active_tenants_in_group(&self, group: &str) -> Vec<Uuid> {
        let mut tenants: HashSet<Uuid> = HashSet::new();
        for tenant in self.queues.keys().chain(self.tenant_in_flight.keys()) {
            if self.tenant_group.get(tenant).map(|g| g.as_str()) != Some(group) {
                continue;
            }
            if self.queues.get(tenant).is_some_and(|q| !q.is_empty())
                || self.tenant_in_flight.get(tenant).copied().unwrap_or(0) > 0
            {
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

        // Split the group's pool across its tenants in proportion to each
        // tenant's weight (largest-remainder), so boosting one user's weight
        // grows their slice even inside a crowded group. Equal weights collapse
        // to an even split.
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
                if self.queues.get(tenant).is_some_and(|q| !q.is_empty())
                    || self.tenant_in_flight.get(tenant).copied().unwrap_or(0) > 0
                {
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

    fn compute_group_caps(&self, max: usize) -> HashMap<String, usize> {
        group_slot_caps(max, &self.active_groups())
    }

    fn can_grant_immediately(&self, req: &AdmitRequest, max: usize) -> bool {
        if self.in_flight >= max || self.queued_total > 0 {
            return false;
        }
        if !self.model_has_slot(&req.model) {
            return false;
        }
        match self.algorithm {
            FairshareAlgorithm::Weighted => true,
            FairshareAlgorithm::Hierarchical => {
                let caps = self.compute_group_caps(max);
                let cap = caps.get(&req.group).copied().unwrap_or(max);
                self.group_in_flight(&req.group) < cap
            }
        }
    }

    fn model_has_slot(&self, model: &str) -> bool {
        match self.model_cap.get(model).copied() {
            Some(cap) => self.model_in_flight.get(model).copied().unwrap_or(0) < cap,
            None => true,
        }
    }

    fn make_admitted(
        &self,
        tenant: Uuid,
        model: String,
        admission: Admission,
        waited: Duration,
    ) -> Admitted {
        Admitted {
            permit: Permit {
                release: Some(self.ctl_tx.clone()),
                tenant,
                model,
            },
            admission,
            waited,
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn grant(
        &mut self,
        tenant: Uuid,
        weight: i64,
        model: String,
        cost: u32,
        respond: oneshot::Sender<Admitted>,
        enqueued: Instant,
        admission: Admission,
        waited: Duration,
    ) {
        self.in_flight += 1;
        *self.tenant_in_flight.entry(tenant).or_insert(0) += 1;
        self.inc_model_in_flight(&model);
        self.stats
            .in_flight
            .store(self.in_flight, Ordering::Relaxed);
        *self.served.entry(tenant).or_insert(self.virtual_time) += cost as f64;
        self.tenant_weight.insert(tenant, weight.max(1));
        if respond
            .send(self.make_admitted(tenant, model.clone(), admission, waited))
            .is_err()
        {
            let _ = self.ctl_tx.send(Ctl::Release { tenant, model });
        }
        let _ = enqueued;
    }

    fn build_snapshot(&self) -> FairshareSnapshot {
        let max = self.capacity.max_in_flight();
        let ids: HashSet<Uuid> = self
            .queues
            .keys()
            .copied()
            .chain(self.tenant_in_flight.keys().copied())
            .chain(self.served.keys().copied())
            .collect();

        let active = self.active_groups();
        let group_caps = self.compute_group_caps(max);
        let total_group_weight: i64 = active.iter().map(|(_, w)| (*w).max(1)).sum();

        let groups: Vec<GroupFairshare> = {
            let mut names: HashSet<String> = active.iter().map(|(n, _)| n.clone()).collect();
            for tenant in &ids {
                if let Some(g) = self.tenant_group.get(tenant) {
                    names.insert(g.clone());
                }
            }
            let mut out: Vec<GroupFairshare> = names
                .into_iter()
                .map(|name| {
                    let weight = self.group_weight.get(&name).copied().unwrap_or(100).max(1);
                    let served_tokens = self.group_served(&name);
                    let share_score = served_tokens / weight as f64;
                    let weight_share =
                        if total_group_weight > 0 && active.iter().any(|(n, _)| n == &name) {
                            weight as f64 / total_group_weight as f64
                        } else {
                            0.0
                        };
                    GroupFairshare {
                        name: name.clone(),
                        weight,
                        in_flight: self.group_in_flight(&name),
                        queued: self.group_queued(&name),
                        slot_cap: group_caps.get(&name).copied().unwrap_or(0),
                        served_tokens,
                        share_score,
                        weight_share,
                    }
                })
                .collect();
            out.sort_by(|a, b| a.name.cmp(&b.name));
            out
        };

        let total_weight: i64 = ids
            .iter()
            .map(|id| self.tenant_weight.get(id).copied().unwrap_or(1).max(1))
            .sum();

        // Sum of tenant weights per group, so a tenant's hierarchical
        // entitlement reflects its share of its group's pool by weight.
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
            .into_iter()
            .map(|tenant_id| {
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
                let share_score = match self.algorithm {
                    FairshareAlgorithm::Weighted => served_tokens / weight as f64,
                    FairshareAlgorithm::Hierarchical => served_tokens,
                };
                let weight_share = match self.algorithm {
                    FairshareAlgorithm::Weighted => {
                        if total_weight > 0 {
                            weight as f64 / total_weight as f64
                        } else {
                            0.0
                        }
                    }
                    FairshareAlgorithm::Hierarchical => {
                        let group = groups.iter().find(|g| g.name == fairshare_group);
                        let group_w = group_weight_sum
                            .get(&fairshare_group)
                            .copied()
                            .unwrap_or(weight)
                            .max(1);
                        group
                            .map(|g| g.weight_share * (weight as f64 / group_w as f64))
                            .unwrap_or(0.0)
                    }
                };
                TenantFairshare {
                    tenant_id,
                    fairshare_group,
                    weight,
                    in_flight: self.tenant_in_flight.get(&tenant_id).copied().unwrap_or(0),
                    queued: self.queues.get(&tenant_id).map(|q| q.len()).unwrap_or(0),
                    served_tokens,
                    share_score,
                    weight_share,
                }
            })
            .collect();
        tenants.sort_by(|a, b| {
            a.share_score
                .partial_cmp(&b.share_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        FairshareSnapshot {
            algorithm: self.algorithm.as_str().into(),
            max_in_flight: max,
            global_in_flight: self.in_flight,
            global_queued: self.queued_total,
            groups,
            tenants,
            model_in_flight: self.model_in_flight.clone(),
        }
    }

    fn dispatch(&mut self) {
        loop {
            let max = self.capacity.max_in_flight();
            if self.in_flight >= max {
                break;
            }
            let Some(tenant) = self.pick_tenant(max) else {
                break;
            };

            let queue = self.queues.get_mut(&tenant).expect("picked tenant exists");
            let waiter = queue.pop_front().expect("picked tenant non-empty");
            self.queued_total = self.queued_total.saturating_sub(1);
            if queue.is_empty() {
                self.queues.remove(&tenant);
            }

            self.tenant_group.insert(tenant, waiter.group.clone());
            self.group_weight
                .insert(waiter.group.clone(), waiter.group_weight.max(1));
            if let Some(cap) = waiter.model_max_in_flight.filter(|cap| *cap > 0) {
                self.model_cap.insert(waiter.model.clone(), cap);
            } else {
                self.model_cap.remove(&waiter.model);
            }
            *self.served.entry(tenant).or_insert(self.virtual_time) += waiter.cost as f64;
            self.advance_virtual_time();

            self.in_flight += 1;
            *self.tenant_in_flight.entry(tenant).or_insert(0) += 1;
            self.inc_model_in_flight(&waiter.model);
            self.stats
                .in_flight
                .store(self.in_flight, Ordering::Relaxed);
            self.stats
                .queued
                .store(self.queued_total as i64, Ordering::Relaxed);
            self.tenant_weight.insert(tenant, waiter.weight.max(1));

            let waited = waiter.enqueued.elapsed();
            if waiter
                .respond
                .send(self.make_admitted(tenant, waiter.model.clone(), Admission::Queued, waited))
                .is_err()
            {
                let _ = self.ctl_tx.send(Ctl::Release {
                    tenant,
                    model: waiter.model,
                });
            }
        }
    }

    fn pick_tenant(&self, max: usize) -> Option<Uuid> {
        match self.algorithm {
            FairshareAlgorithm::Weighted => self.pick_tenant_weighted(),
            FairshareAlgorithm::Hierarchical => self.pick_tenant_hierarchical(max),
        }
    }

    fn pick_tenant_weighted(&self) -> Option<Uuid> {
        let mut best: Option<(Uuid, f64)> = None;
        for (tenant, queue) in &self.queues {
            if queue.is_empty() {
                continue;
            }
            let Some(waiter) = queue.front() else {
                continue;
            };
            if !self.model_has_slot(&waiter.model) {
                continue;
            }
            let weight = waiter.weight.max(1) as f64;
            let key = self.served.get(tenant).copied().unwrap_or(0.0) / weight;
            match best {
                Some((_, best_key)) if key >= best_key => {}
                _ => best = Some((*tenant, key)),
            }
        }
        best.map(|(t, _)| t)
    }

    fn pick_tenant_hierarchical(&self, max: usize) -> Option<Uuid> {
        let caps = self.compute_group_caps(max);

        // Aggregate per-group in-flight and served totals in one pass each,
        // instead of rescanning every tenant for every queued candidate. This
        // keeps each pick O(tenants), not O(queued x tenants).
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
        // The per-tenant slot split is identical for every candidate in the
        // same group, so compute it lazily and at most once per group.
        let mut tenant_caps_by_group: HashMap<String, HashMap<Uuid, usize>> = HashMap::new();

        let mut eligible: Vec<(Uuid, f64)> = Vec::new();

        for (tenant, queue) in &self.queues {
            if queue.is_empty() {
                continue;
            }
            let Some(waiter) = queue.front() else {
                continue;
            };
            if !self.model_has_slot(&waiter.model) {
                continue;
            }
            let group = self
                .tenant_group
                .get(tenant)
                .cloned()
                .unwrap_or_else(|| waiter.group.clone());
            let cap = caps.get(&group).copied().unwrap_or(max);
            if in_flight_by_group
                .get(group.as_str())
                .copied()
                .unwrap_or(0)
                >= cap
            {
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
            let group_weight = self.group_weight.get(&group).copied().unwrap_or(100).max(1) as f64;
            let group_score = served_by_group
                .get(group.as_str())
                .copied()
                .unwrap_or(0.0)
                / group_weight;
            eligible.push((*tenant, group_score));
        }

        if eligible.is_empty() {
            return None;
        }

        let min_group_score = eligible
            .iter()
            .map(|(_, score)| *score)
            .fold(f64::INFINITY, f64::min);

        let mut best: Option<(Uuid, f64)> = None;
        for (tenant, group_score) in eligible {
            if (group_score - min_group_score).abs() > f64::EPSILON && group_score > min_group_score
            {
                continue;
            }
            // Within the winning group, prefer the tenant with the lowest
            // weight-adjusted debt: a higher-weight user burns fair share more
            // slowly and is picked sooner.
            let weight = self.tenant_weight.get(&tenant).copied().unwrap_or(1).max(1) as f64;
            let tenant_score = self.served.get(&tenant).copied().unwrap_or(0.0) / weight;
            match best {
                Some((_, best_score)) if tenant_score >= best_score => {}
                _ => best = Some((tenant, tenant_score)),
            }
        }
        best.map(|(t, _)| t)
    }

    fn advance_virtual_time(&mut self) {
        let min_active = self
            .queues
            .keys()
            .filter_map(|t| self.served.get(t).copied())
            .fold(f64::INFINITY, f64::min);
        if min_active.is_finite() {
            self.virtual_time = min_active;
        }
    }
}
