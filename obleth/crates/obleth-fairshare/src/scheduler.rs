//! The scheduler task: the pools, the control channel, and the shared-slot
//! state machine that claims and releases cluster-wide slots.

use super::*;

pub(crate) enum Ctl {
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
        slots: usize,
    },
    Snapshot {
        /// Include each pool's tenants and keys.
        members: bool,
        respond: oneshot::Sender<FairshareSnapshot>,
    },
    BackgroundQueued {
        respond: oneshot::Sender<HashMap<(String, Uuid), usize>>,
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
pub(crate) struct LocalView {
    pub(crate) view: ReplicaView,
    /// Every pool this replica has, idle ones included: (model, configured
    /// size).
    pub(crate) pools: Vec<(String, usize)>,
    pub(crate) algorithm: FairshareAlgorithm,
    /// The configured global ceiling.
    pub(crate) ceiling: usize,
}

/// Shared-slot state of the scheduler (see [`shared`]).
pub(crate) struct SharedCtl {
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
pub(crate) struct Reconciling {
    /// Taken with no claim or release in flight and nothing admitted while
    /// it runs, so its holdings are exact when it lands.
    quiesced: bool,
    unrecorded_at: u64,
}

pub(crate) struct Scheduler {
    pub(crate) algorithm: FairshareAlgorithm,
    pub(crate) capacity: Arc<dyn CapacityProvider>,
    pub(crate) default_model_cap: usize,
    /// Live gateway replicas.
    pub(crate) replicas: usize,
    /// What configured limits are divided by in the current mode.
    pub(crate) divisor: usize,
    pub(crate) mode: SlotMode,
    pub(crate) shared: Option<SharedCtl>,
    pub(crate) next_op_id: u64,
    /// Configured pool sizes set with [`FairShare::set_model_caps`], which win
    /// over the cap an admission carries.
    pub(crate) model_caps: HashMap<String, usize>,
    /// Set with [`FairShare::set_session_affinity`]; applies to every pool.
    pub(crate) session_affinity: Option<SessionAffinityConfig>,
    pub(crate) pools: HashMap<PoolKey, Pool>,
    /// Insertion order of pools, for round-robin when the ceiling binds.
    pub(crate) pool_order: Vec<PoolKey>,
    pub(crate) cursor: usize,
    pub(crate) in_flight: usize,
    pub(crate) queued_total: usize,
    pub(crate) idle_pool_ttl: Duration,
    pub(crate) ctl_tx: mpsc::UnboundedSender<Ctl>,
    pub(crate) stats: Arc<Stats>,
    pub(crate) model_load: Arc<RwLock<HashMap<String, usize>>>,
}

impl Scheduler {
    pub(crate) async fn run(mut self, mut rx: mpsc::UnboundedReceiver<Ctl>) {
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
            .map(|p| (p.slots, p.queued_total))
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
                req.slots = if key.is_background() {
                    req.slots.max(1)
                } else {
                    1
                };
                if req.slots > 1 {
                    // A slot held for a session's next step is one slot: a
                    // request asking for several keeps no session.
                    req.session = None;
                }
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
                slots,
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
                    Some(pool) => pool.finish_step(tenant, key, session.as_deref(), slots, hold_in),
                    None => StepEnd::Release,
                };
                match end {
                    StepEnd::Held => {}
                    StepEnd::Release => self.release_slot(&pool_key, tenant, key, slots),
                    StepEnd::HoldInBackend { session, arrived } => {
                        self.hold_in_backend(&pool_key, tenant, key, &session, arrived)
                    }
                }
                self.dispatch_all();
                self.publish_stats();
            }
            Ctl::Snapshot { members, respond } => {
                let _ = respond.send(self.build_snapshot(members));
            }
            Ctl::BackgroundQueued { respond } => {
                let _ = respond.send(self.background_queued());
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
                        .map(|p| (p.model.clone(), p.reported_caps().1))
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
                    self.release_slot(&key, hold.tenant, hold.key, 1);
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

    /// Give back one request's `slots` of `tenant` and `key` in `pool_key`:
    /// locally, and to the shared backend when it records them.
    fn release_slot(&mut self, pool_key: &PoolKey, tenant: Uuid, key: Uuid, slots: usize) {
        self.in_flight = self.in_flight.saturating_sub(1);
        if let Some(pool) = self.pools.get_mut(pool_key) {
            pool.release(tenant, key, slots);
            pool.settle_idle();
        }
        if self.mode == SlotMode::Shared || self.quiesced_reconcile_running() {
            self.release_shared(SlotRelease::of(pool_key, tenant, key, slots));
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
            self.release_slot(pool_key, tenant, key, 1);
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
        pool.release(tenant, key, 1);
        pool.settle_idle();
        self.in_flight = self.in_flight.saturating_sub(1);
        self.publish_model_load(pool_key);
        let Some(s) = self.shared.as_mut() else {
            return;
        };
        s.ops += 1;
        let slot = SlotRelease::of(pool_key, tenant, key, 1);
        let fut = s.backend.hold(slot, hold);
        let pool = pool_key.clone();
        spawn_op(&self.ctl_tx, s.config.op_timeout, fut, move |result| {
            Ctl::HoldDone { pool, result }
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
            self.release_slot(&key, hold.tenant, hold.key, 1);
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
            self.release_slot(&key, hold.tenant, hold.key, 1);
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
        spawn_op(&self.ctl_tx, s.config.op_timeout, fut, move |result| {
            Ctl::ViewPublished { result }
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
                let key = &self.pool_order[idx];
                // Most pools have nothing to send: skip them before working
                // out what they may fill.
                let ready = self.pools.get(key).is_some_and(|pool| {
                    let limit = if pool.probing { 1 } else { max_pending };
                    pool.queued_total > 0 && !pool.blocked && pool.pending.len() < limit
                });
                if !ready {
                    continue;
                }
                let headroom = ceiling.saturating_sub(self.in_flight);
                let Some(effective) = self.effective_of(key, headroom, ceiling) else {
                    continue;
                };
                let key = key.clone();
                let Some(pool) = self.pools.get_mut(&key) else {
                    continue;
                };
                let (next, dropped) = pool.take_claim(effective);
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
        spawn_op(&self.ctl_tx, s.config.op_timeout, fut, move |result| {
            Ctl::AcquireDone { pool, id, result }
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
        spawn_op(&self.ctl_tx, s.config.op_timeout, fut, move |result| {
            Ctl::ReleaseDone { pool, result }
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
                let grant =
                    Grant::new(tenant, key_id, waiter.session, waiter.jumped).holding(waiter.slots);
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
        spawn_op(&self.ctl_tx, s.config.op_timeout, fut, move |result| {
            Ctl::ReconcileDone { result }
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
            self.release_slot(key, hold.tenant, hold.key, 1);
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
            pool.forget_quiet_tenants();
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
                // Most pools have nothing queued: skip them before working
                // out what they may fill.
                if self
                    .pools
                    .get(&self.pool_order[idx])
                    .is_none_or(|p| p.queued_total == 0)
                {
                    continue;
                }
                let headroom = ceiling.saturating_sub(self.in_flight);
                let effective = self.effective_of(&self.pool_order[idx], headroom, ceiling);
                let (granted, dropped) =
                    match (self.pools.get_mut(&self.pool_order[idx]), effective) {
                        (Some(pool), Some(effective)) => {
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
                cap: pool.reported_caps().0,
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

    fn background_queued(&self) -> HashMap<(String, Uuid), usize> {
        let mut out = HashMap::new();
        for (key, pool) in &self.pools {
            let PoolKey::Background(model) = key else {
                continue;
            };
            for queue in pool.queues.values() {
                for key_id in queue.keys.keys() {
                    let (_, queued) = pool.key_state(key_id);
                    if queued > 0 {
                        out.insert((model.clone(), *key_id), queued);
                    }
                }
            }
        }
        out
    }

    fn build_snapshot(&self, members: bool) -> FairshareSnapshot {
        let ceiling = self.ceiling();
        let headroom = ceiling.saturating_sub(self.in_flight);
        let mut pools: Vec<ModelPoolFairshare> = self
            .pools
            .values()
            .map(|p| p.snapshot(headroom, members))
            .collect();
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

/// Bound on each cluster-wide read ([`FairShare::cluster_snapshot`],
/// [`FairShare::cluster_sample`], [`FairShare::cluster_in_flight`]): a
/// dashboard poll or the history sampler gives up on a stalled backend and
/// shows this replica's own state. Fixed, unlike the scheduler's
/// configurable `op_timeout`, which bounds the calls admission waits on.
pub const CLUSTER_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Far enough ahead to stand for "never" in a `sleep_until`.
fn far_future() -> Instant {
    Instant::now() + Duration::from_secs(86_400)
}

/// Run a backend call bounded by `timeout` and send its result to the
/// scheduler as the message `done` makes of it.
fn spawn_op<T: Send + 'static>(
    tx: &mpsc::UnboundedSender<Ctl>,
    timeout: Duration,
    fut: SlotFuture<T>,
    done: impl FnOnce(Result<T, SlotError>) -> Ctl + Send + 'static,
) {
    let tx = tx.clone();
    tokio::spawn(async move {
        let _ = tx.send(done(bounded(timeout, fut).await));
    });
}

impl SlotRelease {
    /// `slots` of `tenant` and `key` in `pool_key` (and its sibling pool's
    /// shared model cap, if it has one).
    fn of(pool_key: &PoolKey, tenant: Uuid, key: Uuid, slots: usize) -> Self {
        SlotRelease {
            pool: pool_key.slot_id(),
            tenant,
            key,
            slots,
            sibling: pool_key.sibling().map(|k| k.slot_id()),
        }
    }
}

/// A backend call bounded by `timeout`; running past it is a failure.
pub(crate) async fn bounded<T>(timeout: Duration, fut: SlotFuture<T>) -> Result<T, SlotError> {
    match tokio::time::timeout(timeout, fut).await {
        Ok(result) => result,
        Err(_) => Err(SlotError(format!(
            "shared slot backend did not answer within {timeout:?}"
        ))),
    }
}
