//! One pool's fair queueing: its tenants' and keys' queues, standing and
//! clocks, and the waiters and grants it hands out.

use super::*;

pub(crate) struct Waiter {
    weight: i64,
    key_weight: i64,
    group: String,
    group_weight: i64,
    tenant_max_in_flight: Option<usize>,
    key_max_in_flight: Option<usize>,
    cost: u32,
    /// The pool slots it asks for; once taken, the slots it holds (capped
    /// by [`Pool::slots_for`]).
    pub(crate) slots: usize,
    end_user: Option<EndUser>,
    pub(crate) session: Option<String>,
    /// Session affinity put it ahead of an older request when it was taken.
    pub(crate) jumped: bool,
    /// Shared mode: a claim for only its session's held slot has been tried.
    probed: bool,
    /// Times a later request of its key was served first, through session
    /// affinity.
    passed: u32,
    pub(crate) enqueued: Instant,
    /// The local fast path would have admitted it on arrival; in shared mode
    /// it is still reported as fast if its first claim succeeds.
    pub(crate) fast: bool,
    /// Shared mode: its key's move count (`Pool::key_moves`) when its local
    /// slot was charged. A refused claim undoes the key's charge only if the
    /// key has not moved since, because a move starts its standing again.
    charged_moves: Option<u64>,
    /// Shared mode: its tenant's rebase count (`Pool::tenant_rebases`) when
    /// its local slot was charged. A rebase starts the tenant's standing
    /// again, so a refused claim then undoes only the service it was billed.
    charged_rebases: Option<u64>,
    pub(crate) respond: oneshot::Sender<Admitted>,
}

/// Where a slot held for a session's next step is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldIn {
    /// In the pool: it stays taken here.
    Pool,
    /// In the shared backend, where any replica's claim can take it.
    Backend,
    /// Nowhere: the slot is given back.
    Nowhere,
}

/// What becomes of a finished step's slot.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StepEnd {
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
pub(crate) fn hold_id(pool: &PoolKey, session: &str) -> String {
    format!("{}|{session}", pool.slot_id())
}

/// Who a grant goes to, for [`Pool::send`].
pub(crate) struct Grant {
    tenant: Uuid,
    key: Uuid,
    session: Option<String>,
    /// It went ahead of an older request.
    jumped: bool,
    /// The pool slots it holds.
    slots: usize,
}

impl Grant {
    pub(crate) fn new(tenant: Uuid, key: Uuid, session: Option<String>, jumped: bool) -> Self {
        Grant {
            tenant,
            key,
            session,
            jumped,
            slots: 1,
        }
    }

    pub(crate) fn holding(mut self, slots: usize) -> Self {
        self.slots = slots.max(1);
        self
    }
}

/// A waiter whose cluster-wide slot claim is outstanding. It already holds
/// its local slot, so local limits count it.
pub(crate) struct PendingClaim {
    pub(crate) tenant: Uuid,
    pub(crate) key: Uuid,
    pub(crate) waiter: Waiter,
}

/// A tenant's backlog, split per key so the key level can pick fairly.
#[derive(Default)]
pub(crate) struct TenantQueue {
    pub(crate) keys: HashMap<Uuid, VecDeque<Waiter>>,
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
pub(crate) struct Pool {
    key: PoolKey,
    pub(crate) model: String,
    algorithm: FairshareAlgorithm,
    /// Slots enforced here: `replica_share(configured_cap, divisor)`.
    cap: usize,
    pub(crate) configured_cap: usize,
    /// What the configured limits are divided by: the live replica count in
    /// split mode, 1 otherwise.
    divisor: usize,
    /// Requests holding slots here (claims out with the backend included).
    pub(crate) in_flight: usize,
    /// The slots they hold: `in_flight`, or more in a background pool where
    /// a request with a long prompt holds several. Pool limits count these;
    /// tenant and key limits count requests.
    pub(crate) slots: usize,
    pub(crate) queued_total: usize,
    tenant_in_flight: HashMap<Uuid, usize>,
    tenant_group: HashMap<Uuid, String>,
    group_weight: HashMap<String, i64>,
    tenant_weight: HashMap<Uuid, i64>,
    /// Configured per-tenant caps; enforced as this replica's share.
    tenant_cap: HashMap<Uuid, usize>,
    pub(crate) queues: HashMap<Uuid, TenantQueue>,
    served: HashMap<Uuid, f64>,
    /// The part of each tenant's `served` that is placement (on joining, or
    /// when raised after going quiet) rather than service it received.
    /// `served - served_base` is its actual service.
    served_base: HashMap<Uuid, f64>,
    key_in_flight: HashMap<Uuid, usize>,
    /// For each key with a slot, the tenant that owned it when its first
    /// slot was taken, and per owner how many of its keys hold a slot: a
    /// tenant is quiet only when that count is zero, read in O(1) instead of
    /// scanning every in-flight key.
    in_flight_owner: HashMap<Uuid, Uuid>,
    owner_in_flight: HashMap<Uuid, usize>,
    key_served: HashMap<Uuid, f64>,
    key_weight: HashMap<Uuid, i64>,
    /// Configured per-key caps; enforced as this replica's share.
    key_cap: HashMap<Uuid, usize>,
    key_tenant: HashMap<Uuid, Uuid>,
    /// Which end user a derived key stands for (dashboards only).
    key_end_user: HashMap<Uuid, EndUser>,
    /// The virtual time of the tenants that compete with each other (all of
    /// the pool's under the weighted algorithm, keyed by `""`, and each
    /// group's under the hierarchical one, keyed by group name), in
    /// `served / weight` (a tenant's standing, which is what tenants are
    /// ranked on). A tenant that starts queueing is raised to its clock, so
    /// it rejoins level with the tenants it now competes with. See
    /// [`Pool::advance_tenant_clock`] and [`Pool::join_tenant`].
    tenant_clock: HashMap<String, f64>,
    /// Per tenant, the virtual time of its keys, in `key_served / key_weight`
    /// (a key's standing). A key that comes back with nothing queued is
    /// raised to it, so it rejoins level with the keys it now competes with.
    /// The tenant clock is the wrong place for that: it measures tenants,
    /// the service of every key in a tenant together. See
    /// [`Pool::advance_key_clock`].
    key_clock: HashMap<Uuid, f64>,
    /// Per tenant, its keys with nothing queued or running that are kept
    /// because they stand above the key clock. Each is forgotten once the
    /// clock reaches it, and the set holds at most
    /// [`MAX_IDLE_KEYS_PER_TENANT`].
    key_idle: HashMap<Uuid, HashSet<Uuid>>,
    /// How many times each tracked key has moved to another tenant.
    key_moves: HashMap<Uuid, u64>,
    /// How many times each tracked tenant has been rebased to another
    /// group's clock.
    tenant_rebases: HashMap<Uuid, u64>,
    /// When the pool last became idle (no permits, no waiters); `None` while
    /// it is in use.
    pub(crate) idle_since: Option<Instant>,
    /// Shared mode: claims outstanding with the backend, by id.
    pub(crate) pending: HashMap<u64, PendingClaim>,
    /// Shared mode: the tenant and key of a claim being charged, before it
    /// goes into `pending`.
    charging: Option<(Uuid, Uuid)>,
    /// Shared mode: a claim was refused, so send one at a time until one
    /// succeeds.
    pub(crate) probing: bool,
    /// Shared mode: the pool is full cluster-wide; wait for a wakeup.
    pub(crate) blocked: bool,
    /// Shared mode: tenants and keys at their cluster-wide cap here.
    pub(crate) blocked_tenants: HashSet<Uuid>,
    pub(crate) blocked_keys: HashSet<Uuid>,
    /// Shared mode: the pool's cluster-wide occupancy as last reported.
    pub(crate) cluster_in_flight: Option<usize>,
    /// Session affinity. Not cleared by [`Pool::reset`]: a session stays
    /// cached upstream while the pool is idle between its steps.
    pub(crate) affinity: affinity::Affinity,
    /// Where `affinity`'s setting comes from: the scheduler's, which
    /// [`FairShare::set_session_affinity`] changes, or the model's own.
    pub(crate) affinity_source: PoolAffinity,
    /// Background pools: the share of the model's pool and of the global
    /// ceiling this pool may fill (see [`background_cap`]).
    pub(crate) share: f64,
    ctl_tx: mpsc::UnboundedSender<Ctl>,
}

impl Pool {
    pub(crate) fn new(
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
            slots: 0,
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
            in_flight_owner: HashMap::new(),
            owner_in_flight: HashMap::new(),
            key_served: HashMap::new(),
            key_weight: HashMap::new(),
            key_cap: HashMap::new(),
            key_tenant: HashMap::new(),
            key_end_user: HashMap::new(),
            tenant_clock: HashMap::new(),
            key_clock: HashMap::new(),
            key_idle: HashMap::new(),
            key_moves: HashMap::new(),
            tenant_rebases: HashMap::new(),
            idle_since: None,
            pending: HashMap::new(),
            charging: None,
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
    pub(crate) fn set_configured_cap(&mut self, configured: usize) {
        self.configured_cap = configured.max(1);
        self.cap = replica_share(self.configured_cap, self.divisor);
    }

    pub(crate) fn set_divisor(&mut self, divisor: usize) {
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
    pub(crate) fn unblock(&mut self) {
        self.blocked = false;
        self.blocked_tenants.clear();
        self.blocked_keys.clear();
    }

    pub(crate) fn track_meta(&mut self, req: &AdmitRequest) {
        let reweighted = self.set_tenant_weight(req.tenant, req.weight.max(1));
        let old_group = self.tenant_group.insert(req.tenant, req.group.clone());
        let regrouped = old_group.is_some_and(|g| g != req.group);
        let group_reweighted = self
            .group_weight
            .insert(req.group.clone(), req.group_weight.max(1))
            .is_some_and(|w| w != req.group_weight.max(1));
        let key_reweighted = self.set_key_weight(req.key, req.key_weight.max(1));
        let moved_from = self.set_key_tenant(req.key, req.tenant);
        match &req.end_user {
            Some(end_user) => {
                self.key_end_user.insert(req.key, end_user.clone());
            }
            None => {
                self.key_end_user.remove(&req.key);
            }
        }
        let tenant_cap = req.tenant_max_in_flight.filter(|c| *c > 0);
        let old_tenant_cap = match tenant_cap {
            Some(cap) => self.tenant_cap.insert(req.tenant, cap),
            None => self.tenant_cap.remove(&req.tenant),
        };
        let key_cap = req.key_max_in_flight.filter(|c| *c > 0);
        let old_key_cap = match key_cap {
            Some(cap) => self.key_cap.insert(req.key, cap),
            None => self.key_cap.remove(&req.key),
        };
        if regrouped {
            self.rebase_tenant(req.tenant);
        }
        // Queued requests carry the weights, group and caps they arrived
        // with, and taking one records them again: give them the new ones.
        if group_reweighted {
            let tenants: Vec<Uuid> = self
                .queues
                .keys()
                .filter(|t| self.tenant_group.get(*t) == Some(&req.group))
                .copied()
                .collect();
            for tenant in tenants {
                self.retarget_queued(tenant);
            }
        } else if reweighted
            || regrouped
            || key_reweighted
            || old_tenant_cap != tenant_cap
            || old_key_cap != key_cap
        {
            self.retarget_queued(req.tenant);
        }
        if let Some(old) = moved_from {
            self.move_queued(req.key, old, req.tenant);
        }
    }

    /// Give `tenant`'s queued requests its and their keys' weights, group
    /// and caps as now recorded, so taking one does not record the old ones
    /// again.
    fn retarget_queued(&mut self, tenant: Uuid) {
        let Some(mut queue) = self.queues.remove(&tenant) else {
            return;
        };
        for (key, deque) in queue.keys.iter_mut() {
            for waiter in deque.iter_mut() {
                self.retarget_waiter(waiter, tenant, *key);
            }
        }
        self.queues.insert(tenant, queue);
    }

    /// Record `tenant`'s weight. A change keeps its standing: the service it
    /// has had is not weighed again at the new weight, only what it gets
    /// from now on. Returns whether the weight changed.
    fn set_tenant_weight(&mut self, tenant: Uuid, weight: i64) -> bool {
        let Some(old) = self.tenant_weight.insert(tenant, weight) else {
            return false;
        };
        if old == weight {
            return false;
        }
        if let Some(served) = self.served.get_mut(&tenant) {
            let rescaled = *served * weight as f64 / old as f64;
            // Placement, not service: the base moves with it.
            *self.served_base.entry(tenant).or_insert(0.0) += rescaled - *served;
            *served = rescaled;
        }
        true
    }

    /// Record `key`'s weight, keeping its standing as
    /// [`Pool::set_tenant_weight`] does a tenant's.
    fn set_key_weight(&mut self, key: Uuid, weight: i64) -> bool {
        let Some(old) = self.key_weight.insert(key, weight) else {
            return false;
        };
        if old == weight {
            return false;
        }
        if let Some(served) = self.key_served.get_mut(&key) {
            *served = *served * weight as f64 / old as f64;
        }
        true
    }

    /// A tenant that moved to another group under the hierarchical
    /// algorithm: its standing, measured against the old group's clock,
    /// starts again at the new group's.
    fn rebase_tenant(&mut self, tenant: Uuid) {
        if self.algorithm != FairshareAlgorithm::Hierarchical {
            return;
        }
        let start = self.tenant_start(&tenant);
        if let Some(served) = self.served.get_mut(&tenant) {
            // Placement, not service: the base moves with it.
            *self.served_base.entry(tenant).or_insert(0.0) += start - *served;
            *served = start;
            *self.tenant_rebases.entry(tenant).or_insert(0) += 1;
        }
    }

    fn tenant_rebases_of(&self, tenant: &Uuid) -> u64 {
        self.tenant_rebases.get(tenant).copied().unwrap_or(0)
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
            self.retarget_waiter(&mut waiter, to, key);
            self.queues.entry(to).or_default().reinsert(key, waiter);
        }
    }

    /// Give a waiter `tenant`'s and `key`'s weights, group and caps, as last
    /// recorded.
    fn retarget_waiter(&self, waiter: &mut Waiter, tenant: Uuid, key: Uuid) {
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
        if let Some(weight) = self.key_weight.get(&key) {
            waiter.key_weight = *weight;
        }
        waiter.key_max_in_flight = self.key_cap.get(&key).copied();
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
        // A slot the key holds now counts for its new owner, which is kept
        // while the request runs, as the old owner was.
        if let Some(prev) = self.in_flight_owner.insert(key, tenant) {
            if let Some(n) = self.owner_in_flight.get_mut(&prev) {
                *n = n.saturating_sub(1);
                if *n == 0 {
                    self.owner_in_flight.remove(&prev);
                }
            }
            *self.owner_in_flight.entry(tenant).or_insert(0) += 1;
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
        self.set_tenant_weight(tenant, w.weight.max(1));
        self.tenant_group.insert(tenant, w.group.clone());
        self.group_weight
            .insert(w.group.clone(), w.group_weight.max(1));
        self.set_key_weight(key, w.key_weight.max(1));
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

    /// Shared mode: the tenant and key of every claim still out with the
    /// backend, including one being charged.
    fn outstanding(&self) -> impl Iterator<Item = (Uuid, Uuid)> + '_ {
        self.pending
            .values()
            .map(|c| (c.tenant, c.key))
            .chain(self.charging)
    }

    /// Whether `key` is held at its cap for the key clock: as
    /// [`Pool::key_open`], but not counting slots filled only by its own
    /// claims still out with the shared backend. Those may yet be refused,
    /// and a refusal undoes their charge but cannot undo a raise to the
    /// clock, so a key held only by them is treated as waiting.
    fn key_held(&self, key: &Uuid) -> bool {
        if self.blocked_keys.contains(key) {
            return true;
        }
        self.key_limit(key).is_some_and(|cap| {
            let out = self.outstanding().filter(|(_, k)| k == key).count();
            self.key_in_flight
                .get(key)
                .copied()
                .unwrap_or(0)
                .saturating_sub(out)
                >= cap
        })
    }

    /// Whether `tenant` is held for the tenant clock: at its cap or blocked,
    /// or with no waiting key that is not held, not counting its own claims
    /// still out with the shared backend (see [`Pool::key_held`]). A key it
    /// owns whose last queued request is out as one of those claims still
    /// counts as waiting.
    fn tenant_held(&self, tenant: &Uuid) -> bool {
        if self.blocked_tenants.contains(tenant) {
            return true;
        }
        let at_cap = self.tenant_limit(tenant).is_some_and(|cap| {
            let out = self.outstanding().filter(|(t, _)| t == tenant).count();
            self.tenant_in_flight
                .get(tenant)
                .copied()
                .unwrap_or(0)
                .saturating_sub(out)
                >= cap
        });
        if at_cap {
            return true;
        }
        let queued = self
            .queues
            .get(tenant)
            .into_iter()
            .flat_map(|q| q.keys.iter())
            .filter(|(_, d)| !d.is_empty())
            .map(|(k, _)| *k);
        // A refused claim goes back in line under its key's owner, which is
        // not the tenant it was charged to if the key has moved since.
        let claimed = self
            .outstanding()
            .filter(|(t, k)| self.key_owner(k, *t) == *tenant)
            .map(|(_, k)| k);
        !queued.chain(claimed).any(|k| !self.key_held(&k))
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
                let weight = self
                    .group_weight
                    .get(&name)
                    .copied()
                    .unwrap_or(DEFAULT_GROUP_WEIGHT)
                    .max(1);
                (name, weight)
            })
            .collect()
    }

    /// The number of slots this pool can actually fill right now: `cap` (the
    /// pool's cap as [`Pool::reported_caps`] gives it), or fewer when the
    /// global ceiling leaves less headroom than that. Group
    /// and tenant splits are taken from this figure, not from the pool cap,
    /// so a ceiling that binds below the pool still reserves every group's
    /// share of what is really available. Splitting the nominal cap instead
    /// hands the largest group a cap it can never reach, and it then never
    /// has to yield a freed slot to a smaller group.
    fn effective_cap(&self, cap: usize, headroom: usize) -> usize {
        cap.min(self.slots.saturating_add(headroom)).max(1)
    }

    /// The pool's size as views report it, `(cap, configured_cap)`. A
    /// background pool reports the share of the model's pool it may fill,
    /// which is what admission enforces (see [`Pool::claim`]), so its fair
    /// shares and bars are drawn against the slots background work can use
    /// rather than the whole model.
    pub(crate) fn reported_caps(&self) -> (usize, usize) {
        if self.key.is_background() {
            (
                background_cap(self.cap, self.share).max(1),
                background_cap(self.configured_cap, self.share).max(1),
            )
        } else {
            (self.cap, self.configured_cap)
        }
    }

    /// The slots this pool may fill now, given the slots the same model's
    /// other pool holds (`sibling_in_flight`) and what it has waiting
    /// (`sibling_queued`), and the replica's in-flight total against its
    /// ceiling.
    ///
    /// An interactive pool gives up the slots background work holds. A
    /// background pool takes nothing new while an interactive request waits,
    /// and fills at most its share of the pool and of the ceiling, both
    /// counting every request in flight; the rest stays free for
    /// interactive arrivals. See [`Pool::fits`] for a request of several
    /// slots.
    pub(crate) fn effective_with(
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
                .min(self.slots.saturating_add(headroom))
                .max(1);
        }
        if sibling_queued > 0 {
            return self.slots;
        }
        let pool_room = background_cap(self.cap, self.share).saturating_sub(sibling_in_flight);
        let global_room = background_cap(ceiling, self.share).saturating_sub(replica_in_flight);
        pool_room.min(self.slots.saturating_add(global_room))
    }

    /// The slots a request asking for `asked` holds here: one in an
    /// interactive pool; in a background pool what it asks, capped at the
    /// background share of the pool so it can still run.
    fn slots_for(&self, asked: usize) -> usize {
        if !self.key.is_background() {
            return 1;
        }
        asked.clamp(1, background_cap(self.cap, self.share).max(1))
    }

    /// Whether a request holding `slots` fits under `effective`: in the room
    /// left, or alone, with nothing else holding a slot here and some room,
    /// so a request as big as the whole share is not starved by smaller ones.
    fn fits(&self, slots: usize, effective: usize) -> bool {
        self.slots.saturating_add(slots) <= effective || (self.slots == 0 && effective > 0)
    }

    fn compute_group_caps(&self, effective: usize) -> HashMap<String, usize> {
        group_slot_caps(effective, &self.active_groups())
    }

    pub(crate) fn can_grant_immediately(&self, req: &AdmitRequest, effective: usize) -> bool {
        if !self.fits(self.slots_for(req.slots), effective) || self.queued_total > 0 {
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

    fn occupy(&mut self, tenant: Uuid, key: Uuid, cost: u32, slots: usize) {
        self.in_flight += 1;
        self.slots += slots;
        *self.tenant_in_flight.entry(tenant).or_insert(0) += 1;
        let slots = self.key_in_flight.entry(key).or_insert(0);
        *slots += 1;
        if *slots == 1 {
            let owner = self.key_owner(&key, tenant);
            self.in_flight_owner.insert(key, owner);
            *self.owner_in_flight.entry(owner).or_insert(0) += 1;
        }
        self.charge(tenant, key, cost);
    }

    /// Bill a grant's cost to its tenant and key. [`Pool::occupy`] does it
    /// for a new slot; a held slot taken by its session is billed alone,
    /// since it was never given back.
    fn charge(&mut self, tenant: Uuid, key: Uuid, cost: u32) {
        let start = self.tenant_start(&tenant);
        let served = self.served.entry(tenant).or_insert_with(|| {
            self.served_base.insert(tenant, start);
            start
        });
        let began = *served;
        *served += cost as f64;
        let began = began / self.tenant_weight_of(&tenant);
        self.advance_tenant_clock(tenant, began);
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
            .filter(|(k, d)| !d.is_empty() && !self.key_held(k))
            .map(|(k, _)| self.key_standing(k))
            .fold(f64::INFINITY, f64::min);
        let target = if waiting.is_finite() { waiting } else { began };
        let clock = self.key_clock.entry(tenant).or_insert(0.0);
        if target <= *clock {
            return;
        }
        *clock = target;
        // A queued key held at its cap gains no credit while it waits, as at
        // the tenant level (see [`Pool::advance_tenant_clock`]).
        let held: Vec<Uuid> = self
            .queues
            .get(&tenant)
            .into_iter()
            .flat_map(|q| q.keys.iter())
            .filter(|(k, d)| !d.is_empty() && self.key_held(k))
            .map(|(k, _)| *k)
            .collect();
        for key in held {
            let start = self.key_start(tenant, key);
            let served = self.key_served.entry(key).or_insert(start);
            if *served < start {
                *served = start;
            }
        }
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
    /// Whether `key` holds no slot and has nothing queued. A key queues under
    /// its owner, so one queue answers; a key that has moved between tenants
    /// may still have a waiter under its old one, so only then every queue is
    /// read.
    fn key_quiet(&self, key: &Uuid) -> bool {
        if self.key_in_flight.contains_key(key) {
            return false;
        }
        if self.key_moves.contains_key(key) {
            return self.queues.values().all(|q| q.key_len(key) == 0);
        }
        let quiet = self
            .key_tenant
            .get(key)
            .and_then(|t| self.queues.get(t))
            .is_none_or(|q| q.key_len(key) == 0);
        debug_assert!(
            !quiet || self.queues.values().all(|q| q.key_len(key) == 0),
            "an unmoved key is queued under a tenant that does not own it"
        );
        quiet
    }

    /// Whether `tenant` has nothing queued, holds no slot, and owns no key
    /// that holds one. O(1): the counts are kept as slots come and go.
    fn tenant_quiet(&self, tenant: &Uuid) -> bool {
        self.queues.get(tenant).is_none_or(|q| q.len == 0)
            && !self.tenant_in_flight.contains_key(tenant)
            && !self.owner_in_flight.contains_key(tenant)
    }

    /// Housekeeping: every tenant the pool still remembers that has gone
    /// quiet is reconsidered now, not on its own next request. A quiet tenant
    /// ahead of its clock is forgotten once the clock, moved by the tenants
    /// still being served, reaches it; one alone in its scope at once. Its
    /// idle keys go with it.
    pub(crate) fn forget_quiet_tenants(&mut self) {
        let quiet: Vec<Uuid> = self
            .served
            .keys()
            .filter(|t| self.tenant_quiet(t))
            .copied()
            .collect();
        for tenant in quiet {
            self.forget_tenant_if_quiet(tenant);
        }
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
                slots: grant.slots,
            },
            admission,
            waited,
        }
    }

    pub(crate) fn send(
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

    pub(crate) fn grant_fast(&mut self, req: AdmitRequest, respond: oneshot::Sender<Admitted>) {
        let slots = self.slots_for(req.slots);
        self.occupy(req.tenant, req.key, req.cost, slots);
        let grant = Grant::new(req.tenant, req.key, req.session, false).holding(slots);
        self.send(grant, respond, Admission::Fast, Duration::ZERO);
    }

    /// Give `req` the slot held for its session: it was never given back,
    /// so only the cost is billed. It goes ahead of anyone queued.
    pub(crate) fn grant_held(
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

    pub(crate) fn enqueue(
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
                slots: req.slots,
                end_user: req.end_user,
                session: req.session,
                jumped: false,
                probed: false,
                passed: 0,
                enqueued,
                fast,
                charged_moves: None,
                charged_rebases: None,
                respond,
            },
        );
        self.queued_total += 1;
    }

    /// Admit one queued waiter if the pool has a free slot. Waiters whose
    /// caller has gone are dropped on the way without being charged. Returns
    /// whether a grant happened, so the scheduler can account the global
    /// ceiling, and how many dead waiters were dropped.
    pub(crate) fn dispatch_one(&mut self, effective: usize) -> (bool, usize) {
        let (next, dropped) = self.take_next(effective);
        let Some((tenant, key, waiter)) = next else {
            return (false, dropped);
        };
        let waited = waiter.enqueued.elapsed();
        let grant = Grant::new(tenant, key, waiter.session, waiter.jumped).holding(waiter.slots);
        self.send(grant, waiter.respond, Admission::Queued, waited);
        (true, dropped)
    }

    /// Pick the next waiter the local limits allow and give it its local
    /// slot: dequeued, occupied and charged. Waiters whose caller has gone
    /// are dropped on the way; the count comes back with the pick.
    fn take_next(&mut self, effective: usize) -> (Option<(Uuid, Uuid, Waiter)>, usize) {
        self.take(effective, false)
    }

    /// Shared mode: [`Pool::take_next`] for a claim on the backend, which
    /// may yet be refused.
    pub(crate) fn take_claim(&mut self, effective: usize) -> (Option<(Uuid, Uuid, Waiter)>, usize) {
        self.take(effective, true)
    }

    fn take(&mut self, effective: usize, claim: bool) -> (Option<(Uuid, Uuid, Waiter)>, usize) {
        let mut dropped = 0;
        loop {
            if !self.fits(1, effective) || self.queued_total == 0 {
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
            // The pick waits for room for all its slots rather than letting
            // smaller requests past, which would starve a long prompt.
            let picked = &self.queues[&tenant].keys[&key][at];
            let slots = self.slots_for(picked.slots);
            if !picked.respond.is_closed() && !self.fits(slots, effective) {
                return (None, dropped);
            }
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
            waiter.slots = slots;
            self.charging = claim.then_some((tenant, key));
            self.occupy(tenant, key, waiter.cost, slots);
            self.charging = None;
            waiter.charged_moves = Some(self.key_moves_of(&key));
            waiter.charged_rebases = Some(self.tenant_rebases_of(&tenant));
            return (Some((tenant, key, waiter)), dropped);
        }
    }

    /// Shared mode: the claim for a waiter [`Pool::take_next`] or
    /// [`Pool::take_probe`] gave its local slot. With `holds`, it names the
    /// slot held for the waiter's session, if any; `redeem_only` claims that
    /// slot alone.
    pub(crate) fn claim(
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
            slots: waiter.slots,
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
    pub(crate) fn requeue(&mut self, claim: PendingClaim) {
        let PendingClaim {
            tenant,
            key,
            mut waiter,
        } = claim;
        self.in_flight = self.in_flight.saturating_sub(1);
        self.slots = self.slots.saturating_sub(waiter.slots);
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
        // The waiter still carries the weights it was charged at; a weight
        // changed since rescaled what it was charged, so the refund is
        // rescaled the same way. The rescaled part is placement, as in
        // `set_tenant_weight`, so the base moves by it and the tenant's
        // actual service drops by the cost it was billed. A tenant rebased to
        // another group since has had the charge replaced by its placement
        // there, so only its actual service is undone.
        let cost = waiter.cost as f64;
        let rebased = waiter
            .charged_rebases
            .is_some_and(|r| r != self.tenant_rebases_of(&tenant));
        let tenant_refund = if rebased {
            0.0
        } else {
            cost * self.tenant_weight_of(&tenant) / waiter.weight.max(1) as f64
        };
        if let Some(served) = self.served.get_mut(&tenant) {
            *served -= tenant_refund;
            *self.served_base.entry(tenant).or_insert(0.0) -= tenant_refund - cost;
        }
        if waiter
            .charged_moves
            .is_none_or(|m| m == self.key_moves_of(&key))
        {
            let key_refund =
                waiter.cost as f64 * self.key_weight_of(&key) / waiter.key_weight.max(1) as f64;
            if let Some(served) = self.key_served.get_mut(&key) {
                *served -= key_refund;
            }
        }
        waiter.charged_moves = None;
        waiter.charged_rebases = None;
        waiter.fast = false;
        waiter.jumped = false;
        // A key that moved while its claim was out goes back in line under
        // its new tenant.
        let owner = self.key_owner(&key, tenant);
        self.retarget_waiter(&mut waiter, owner, key);
        if owner != tenant {
            self.join_tenant(owner);
        }
        self.queues.entry(owner).or_default().reinsert(key, waiter);
        self.queued_total += 1;
    }

    /// Shared mode: what this pool holds, for a reconcile. Only called with
    /// no claim outstanding, so every local slot is a granted permit.
    pub(crate) fn holdings(&self) -> Option<PoolHoldings> {
        if self.in_flight == 0 {
            return None;
        }
        Some(PoolHoldings {
            pool: self.key.slot_id(),
            in_flight: self.in_flight,
            slots: self.slots,
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
    pub(crate) fn prune_dead_waiters(&mut self) -> usize {
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
                .unwrap_or_else(|| DEFAULT_GROUP.into());
            let group_weight = self
                .group_weight
                .get(&group)
                .copied()
                .unwrap_or(DEFAULT_GROUP_WEIGHT)
                .max(1) as f64;
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

    /// Start-time fair queueing among the tenants that compete with
    /// `tenant`, after a grant to it that started from standing `began`.
    /// Their clock moves up to the lowest standing among those queued that
    /// could be picked, or to `began` when none is. It never moves back, so
    /// a tenant held at its cap cannot pull it down, and grants made with
    /// nothing queued move it too: a tenant that arrives later starts level
    /// with the service given out before it came, not credited with it.
    ///
    /// A queued tenant that cannot be picked, held at its cap, is raised to
    /// the clock as it moves, so it gains no credit while it waits: when its
    /// cap lifts it competes level, rather than taking every slot until the
    /// others' service since then is matched. One held only by its own
    /// claims still out with the shared backend is not (see
    /// [`Pool::tenant_held`]). A refused claim undoes its charge but not the
    /// clock's moves, so a whole round of refused claims can leave the clock
    /// up to that round's service ahead of the tenants it refunded.
    fn advance_tenant_clock(&mut self, tenant: Uuid, began: f64) {
        let scope = self.clock_scope(&tenant).to_string();
        let waiting = self
            .queues
            .iter()
            .filter(|(t, q)| !q.is_empty() && self.clock_scope(t) == scope && !self.tenant_held(t))
            .filter_map(|(t, _)| self.tenant_standing(t))
            .fold(f64::INFINITY, f64::min);
        let target = if waiting.is_finite() { waiting } else { began };
        let clock = self.tenant_clock.entry(scope.clone()).or_insert(0.0);
        if target <= *clock {
            return;
        }
        *clock = target;
        let held: Vec<Uuid> = self
            .queues
            .iter()
            .filter(|(t, q)| !q.is_empty() && self.clock_scope(t) == scope && self.tenant_held(t))
            .map(|(t, _)| *t)
            .collect();
        for t in held {
            self.raise_tenant(t);
        }
    }

    /// Which tenant clock `tenant` is measured against: the pool's under the
    /// weighted algorithm, its group's under the hierarchical one, where a
    /// tenant competes only with its own group's tenants.
    fn clock_scope(&self, tenant: &Uuid) -> &str {
        match self.algorithm {
            FairshareAlgorithm::Weighted => "",
            FairshareAlgorithm::Hierarchical => self
                .tenant_group
                .get(tenant)
                .map(String::as_str)
                .unwrap_or(DEFAULT_GROUP),
        }
    }

    fn tenant_clock_of(&self, tenant: &Uuid) -> f64 {
        self.tenant_clock
            .get(self.clock_scope(tenant))
            .copied()
            .unwrap_or(0.0)
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

    /// Where `tenant` starts: its tenant clock, at its weight.
    fn tenant_start(&self, tenant: &Uuid) -> f64 {
        self.tenant_clock_of(tenant) * self.tenant_weight_of(tenant)
    }

    /// Place `tenant` as it starts queueing with nothing queued: a new
    /// tenant starts at its tenant clock, and one below it is raised to it,
    /// so it carries no credit for time it was away or only had requests
    /// running. One above it keeps its lead. Called before a request goes
    /// into the tenant's queue, whether it arrives there or moves there with
    /// its key.
    fn join_tenant(&mut self, tenant: Uuid) {
        if self.queues.get(&tenant).is_some_and(|q| !q.is_empty()) {
            return;
        }
        self.raise_tenant(tenant);
    }

    /// Raise `tenant` to its start if it is below it, starting a new one
    /// there.
    fn raise_tenant(&mut self, tenant: Uuid) {
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

    /// Give back one request's `slots`.
    pub(crate) fn release(&mut self, tenant: Uuid, key: Uuid, slots: usize) {
        self.in_flight = self.in_flight.saturating_sub(1);
        self.slots = self.slots.saturating_sub(slots);
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
                let owner = self.in_flight_owner.remove(&key).unwrap_or(tenant);
                if let Some(n) = self.owner_in_flight.get_mut(&owner) {
                    *n = n.saturating_sub(1);
                    if *n == 0 {
                        self.owner_in_flight.remove(&owner);
                    }
                }
            }
        }
        self.forget_if_quiet(tenant, key);
    }

    /// A step of `session` finished in a slot of `tenant` and `key`: what
    /// becomes of the slot, given where a hold for the session's next step
    /// would be kept.
    pub(crate) fn finish_step(
        &mut self,
        tenant: Uuid,
        key: Uuid,
        session: Option<&str>,
        slots: usize,
        hold_in: HoldIn,
    ) -> StepEnd {
        // A hold keeps one slot; a request holding more gives them all back.
        if slots > 1 {
            return StepEnd::Release;
        }
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
    pub(crate) fn take_probe(&mut self) -> Option<(Uuid, Uuid, Waiter)> {
        let (tenant, key, at) = self.queues.iter().find_map(|(tenant, queue)| {
            queue.keys.iter().find_map(|(key, deque)| {
                deque
                    .iter()
                    .position(|w| w.session.is_some() && !w.probed && !w.respond.is_closed())
                    .filter(|_| !self.key_passed_out(*tenant, *key))
                    .map(|at| (*tenant, *key, at))
            })
        })?;
        // Read before the waiter leaves the queue, as it counts among those
        // it may have jumped.
        let oldest = self.oldest_eligible();
        let queue = self.queues.get_mut(&tenant).expect("tenant just found");
        let mut waiter = queue.pop_at(&key, at).expect("waiter just found");
        if queue.is_empty() {
            self.queues.remove(&tenant);
        }
        self.queued_total -= 1;
        waiter.probed = true;
        waiter.jumped = oldest.is_some_and(|t| t < waiter.enqueued);
        self.track_waiter_meta(tenant, key, &waiter);
        waiter.slots = self.slots_for(waiter.slots);
        self.charging = Some((tenant, key));
        self.occupy(tenant, key, waiter.cost, waiter.slots);
        self.charging = None;
        waiter.charged_moves = Some(self.key_moves_of(&key));
        waiter.charged_rebases = Some(self.tenant_rebases_of(&tenant));
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
    pub(crate) fn key_passed_out(&self, tenant: Uuid, key: Uuid) -> bool {
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
    pub(crate) fn oldest_eligible(&self) -> Option<Instant> {
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
    pub(crate) fn holds_to_give_up(&mut self, now: Instant) -> Vec<affinity::Hold> {
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
    pub(crate) fn redeem_queued(&mut self) -> (usize, usize) {
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
        let owner = self.key_owner(&key, tenant);
        // A key that is idle and at or below its owner's key clock would be
        // raised to it on return anyway, so its entry carries no information
        // worth keeping. One above the clock is kept until the clock reaches
        // it.
        if self.key_quiet(&key) {
            if self.key_served.get(&key).copied().unwrap_or(0.0) <= self.key_start(owner, key) {
                self.forget_key(&key);
            } else {
                self.keep_idle_key(owner, key);
            }
        }
        self.forget_tenant_if_quiet(tenant);
        // A key that ran in another tenant's slot after moving: its owner,
        // kept while it ran, may be quiet now.
        if owner != tenant {
            self.forget_tenant_if_quiet(owner);
        }
    }

    /// The rule for keys, for a tenant, so tenants that have gone quiet (or
    /// have been deleted) stop showing up in the pool's share tree. One at
    /// or below its clock is forgotten, and so is one with no other tenant
    /// on its clock, which has nobody to keep a lead over. A tenant that
    /// owns a key running in another tenant's slot is kept: a refused claim
    /// of that key goes back in line under it.
    fn forget_tenant_if_quiet(&mut self, tenant: Uuid) {
        if !self.tenant_quiet(&tenant) {
            return;
        }
        let scope = self.clock_scope(&tenant);
        let caught_up =
            self.tenant_standing(&tenant).unwrap_or(0.0) <= self.tenant_clock_of(&tenant);
        let alone = !self
            .served
            .keys()
            .any(|t| *t != tenant && self.clock_scope(t) == scope);
        if caught_up || alone {
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
        // Over the bound: one pass ranks the idle keys by lead and forgets a
        // batch of the smallest, so the pass runs once per batch of arrivals
        // rather than once per arrival.
        let (mut here, gone): (Vec<Uuid>, Vec<Uuid>) = self.key_idle[&tenant]
            .iter()
            .partition(|k| self.key_idle_here(&tenant, k));
        here.sort_by(|a, b| self.key_standing(a).total_cmp(&self.key_standing(b)));
        let over = here.len().saturating_sub(MAX_IDLE_KEYS_PER_TENANT);
        let evict: Vec<Uuid> = here
            .into_iter()
            .take(over.max(IDLE_KEY_EVICT_BATCH.min(MAX_IDLE_KEYS_PER_TENANT)))
            .collect();
        if let Some(idle) = self.key_idle.get_mut(&tenant) {
            for key in gone.iter().chain(evict.iter()) {
                idle.remove(key);
            }
        }
        for key in &evict {
            self.forget_key(key);
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
        self.tenant_rebases.remove(tenant);
        self.tenant_weight.remove(tenant);
        self.tenant_group.remove(tenant);
        self.tenant_cap.remove(tenant);
        self.queues.remove(tenant);
    }

    pub(crate) fn is_idle(&self) -> bool {
        self.in_flight == 0 && self.slots == 0 && self.queued_total == 0
    }

    /// Record whether the pool is in use, resetting its share history the
    /// first time it is found idle.
    pub(crate) fn settle_idle(&mut self) {
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
        self.in_flight_owner.clear();
        self.owner_in_flight.clear();
        self.key_served.clear();
        self.key_weight.clear();
        self.key_cap.clear();
        self.key_tenant.clear();
        self.key_end_user.clear();
        self.tenant_clock.clear();
        self.key_clock.clear();
        self.key_idle.clear();
        self.key_moves.clear();
        self.tenant_rebases.clear();
    }

    /// Per-group in-flight/queued totals, for the history sampler. Cheaper
    /// than [`Pool::snapshot`]: one pass over `tenant_in_flight` and the
    /// queues, no tenant/key views. Only groups with in-flight or queued
    /// work are included.
    pub(crate) fn group_totals(&self) -> Vec<GroupSample> {
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
        let default_group = DEFAULT_GROUP.to_string();
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
    pub(crate) fn key_state(&self, key: &Uuid) -> (usize, usize) {
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
    pub(crate) fn view(&self, budget: &mut view::ViewBudget) -> ReplicaPoolView {
        let active: HashSet<Uuid> = self
            .tenant_in_flight
            .keys()
            .chain(self.queues.keys())
            .filter(|t| self.tenant_is_active(t))
            .copied()
            .collect();
        let mut tenants: Vec<ReplicaTenantView> = active
            .iter()
            .map(|id| ReplicaTenantView {
                id: *id,
                group: self
                    .tenant_group
                    .get(id)
                    .cloned()
                    .unwrap_or_else(|| DEFAULT_GROUP.into()),
                weight: self.tenant_weight.get(id).copied().unwrap_or(1).max(1),
                cap: self.tenant_cap.get(id).copied(),
                in_flight: self.tenant_in_flight.get(id).copied().unwrap_or(0),
                queued: self.queues.get(id).map(|q| q.len).unwrap_or(0),
                served: self.served.get(id).copied().unwrap_or(0.0),
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
                    .unwrap_or(DEFAULT_GROUP_WEIGHT)
                    .max(1),
                served: service.get(g.name.as_str()).copied().unwrap_or(0.0),
                name: g.name,
                in_flight: g.in_flight,
                queued: g.queued,
            })
            .collect();

        ReplicaPoolView {
            pool: self.key.slot_id(),
            configured_cap: self.reported_caps().1,
            in_flight: self.in_flight,
            slots: self.slots,
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
    /// The pool's state; its tenants and keys only when `members`.
    pub(crate) fn snapshot(&self, headroom: usize, members: bool) -> ModelPoolFairshare {
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
        let (cap, configured_cap) = self.reported_caps();
        let group_caps = self.compute_group_caps(self.effective_cap(cap, headroom));
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
                let weight = self
                    .group_weight
                    .get(&name)
                    .copied()
                    .unwrap_or(DEFAULT_GROUP_WEIGHT)
                    .max(1);
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
            .filter(|_| members)
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
                    .unwrap_or_else(|| DEFAULT_GROUP.into());
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
            view::share_order(a.share_score, a.tenant_id, b.share_score, b.tenant_id)
        });

        let mut keys: Vec<KeyFairshare> = key_ids
            .iter()
            .filter(|_| members)
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
        keys.sort_by(|a, b| view::share_order(a.share_score, a.key_id, b.share_score, b.key_id));

        ModelPoolFairshare {
            model: self.model.clone(),
            cap,
            configured_cap,
            in_flight: self.in_flight,
            slots: self.slots,
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

#[cfg(test)]
mod tests;
