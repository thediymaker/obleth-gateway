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
    pool.tenant_clock.insert("many".into(), 0.5);
    pool.tenant_clock.insert("one".into(), 0.5);

    for _ in 0..4 {
        let t = Uuid::new_v4();
        pool.track_meta(&AdmitRequest::new(t, "m", 10).group("many", 100));
        pool.occupy(t, t, 10, 1);
    }
    let solo = Uuid::new_v4();
    pool.track_meta(&AdmitRequest::new(solo, "m", 10).group("one", 100));
    for _ in 0..4 {
        pool.occupy(solo, solo, 10, 1);
    }

    let service = pool.group_service();
    assert_eq!(service["many"], 40.0);
    assert_eq!(service["one"], 40.0);

    let snap = pool.snapshot(usize::MAX, true);
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
    pub(crate) fn new(slots: usize) -> Self {
        Self::with(FairshareAlgorithm::Weighted, slots)
    }

    fn with(algorithm: FairshareAlgorithm, slots: usize) -> Self {
        let (tx, _rx) = mpsc::unbounded_channel();
        Sim {
            pool: Pool::new(PoolKey::Model("m".into()), algorithm, slots, tx),
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
            self.pool.release(tenant, key, 1);
        }
        let (next, _) = self.pool.take_next(self.pool.cap);
        let (tenant, key, _) = next.expect("a request can be granted");
        self.running.push_back((tenant, key));
        key
    }

    /// Admit `req` straight into a free slot, as the admit path does
    /// when nothing is queued, and finish it at once.
    fn fast(&mut self, req: AdmitRequest) {
        self.pool.track_meta(&req);
        assert!(self.pool.can_grant_immediately(&req, self.pool.cap));
        let (tenant, key) = (req.tenant, req.key);
        let (respond, _admitted) = oneshot::channel();
        self.pool.grant_fast(req, respond);
        self.pool.release(tenant, key, 1);
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

/// A tenant that went quiet while ahead of its clock used to be kept, with
/// its idle keys, until its own next request or until the whole pool
/// emptied. Housekeeping now reconsiders it: once the tenants still being
/// served have moved the clock past it, one sweep forgets it while the
/// pool stays busy.
#[test]
fn a_quiet_tenant_ahead_of_its_clock_is_forgotten_by_a_sweep_while_the_pool_is_busy() {
    let mut sim = Sim::new(1);
    let (ahead, busy) = (Uuid::new_v4(), Uuid::new_v4());
    let (ahead_key, busy_key) = (Uuid::new_v4(), Uuid::new_v4());
    // Both start level; `ahead` gets a large request, then goes quiet.
    sim.queue(ahead, ahead_key, 1_000);
    sim.queue(busy, busy_key, 1);
    sim.grant();
    sim.grant();
    while let Some((t, k)) = sim.running.pop_front() {
        sim.pool.release(t, k, 1);
    }
    // Quiet, with no slot and nothing queued, but ahead of the clock: kept.
    sim.pool.forget_quiet_tenants();
    assert!(
        sim.pool.served.contains_key(&ahead),
        "ahead of its clock: kept"
    );
    // The busy tenant keeps being served, moving the clock past `ahead`.
    for _ in 0..1_200 {
        sim.queue(busy, busy_key, 1);
        sim.grant();
    }
    assert!(!sim.pool.is_idle(), "the pool stays busy");
    sim.pool.forget_quiet_tenants();
    assert!(
        !sim.pool.served.contains_key(&ahead),
        "caught up: forgotten by the sweep"
    );
    assert!(
        !sim.pool.key_served.contains_key(&ahead_key),
        "its idle key went with it"
    );
    assert!(sim.pool.served.contains_key(&busy));
}

/// `key_quiet` reads one queue and the in-flight counts; on random
/// sequences of queueing, granting and finishing it says what the full
/// scan says, and a tenant's quiet state matches the scan too.
#[test]
fn the_constant_time_quiet_checks_match_the_scan_on_random_states() {
    let mut seed: u64 = 0x5eed_c0de_1234_5678;
    let mut next = move |n: u64| {
        seed = seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (seed >> 33) % n
    };
    let tenants: Vec<Uuid> = (0..4).map(|_| Uuid::new_v4()).collect();
    let keys: Vec<(Uuid, Uuid)> = tenants
        .iter()
        .flat_map(|t| (0..3).map(move |_| (*t, Uuid::new_v4())))
        .collect();
    let mut sim = Sim::new(2);
    for step in 0..600 {
        match next(3) {
            0 | 1 => {
                let (tenant, key) = keys[next(keys.len() as u64) as usize];
                sim.queue(tenant, key, 1 + next(50) as u32);
            }
            _ if sim.pool.queued_total > 0 => {
                sim.grant();
            }
            _ => {}
        }
        if step % 7 == 0 {
            if let Some((tenant, key)) = sim.running.pop_front() {
                sim.pool.release(tenant, key, 1);
            }
        }
        for (_, key) in &keys {
            let scan = !sim.pool.key_in_flight.contains_key(key)
                && sim.pool.queues.values().all(|q| q.key_len(key) == 0);
            assert_eq!(sim.pool.key_quiet(key), scan, "step {step}");
        }
        for tenant in &tenants {
            let scan = sim.pool.queues.get(tenant).is_none_or(|q| q.len == 0)
                && !sim.pool.tenant_in_flight.contains_key(tenant)
                && !sim
                    .pool
                    .key_in_flight
                    .keys()
                    .any(|k| sim.pool.key_tenant.get(k) == Some(tenant));
            assert_eq!(sim.pool.tenant_quiet(tenant), scan, "step {step}");
        }
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
            sim.pool.release(t, k, 1);
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
        sim.pool.release(tenant, key, 1);
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
        sim.pool.release(t, k, 1);
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
            sim.pool.release(t, k, 1);
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
    assert!(sim
        .pool
        .queues
        .get(&old)
        .is_none_or(|q| q.key_len(&moved) == 0));
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
/// requests in with the tenant raised to its clock, as a request arriving
/// there would, so the tenant does not take every grant until its old service
/// catches up.
#[test]
fn moved_key_into_a_quiet_tenant_starts_at_its_clock() {
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
fn refused_claim_moved_into_a_quiet_tenant_starts_at_its_clock() {
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
        sim.pool.release(t, k, 1);
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
        sim.pool.release(tenant, key, 1);
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
    assert!(sim.pool.queues.get(&b).is_none_or(|q| q.is_empty()));
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
    pool.occupy(a, key, 10, 1);
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

/// The tenant clock moves with grants made while nothing is queued, so a
/// tenant that arrives while another has had the pool to itself starts
/// level with it, not credited with all the service given out before it
/// came.
#[test]
fn newcomer_is_not_credited_with_service_before_it_came() {
    for slots in [2, 4] {
        let mut sim = Sim::new(slots);
        let (first, later) = (Uuid::new_v4(), Uuid::new_v4());
        // One long request keeps the first tenant in the pool throughout.
        let long = AdmitRequest::new(first, "m", 100);
        sim.pool.track_meta(&long);
        let (respond, _long) = oneshot::channel();
        sim.pool.grant_fast(long, respond);
        sim.running.push_back((first, first));
        for _ in 0..1_000 {
            sim.fast(AdmitRequest::new(first, "m", 100));
        }
        for _ in 0..2_000 {
            sim.queue(first, first, 100);
            sim.queue(later, later, 100);
        }
        let first_grants = (0..1_000).filter(|_| sim.grant() == first).count();
        assert!(
            first_grants >= 400,
            "with {slots} slots the first tenant had {first_grants} of 1,000 grants"
        );
    }
}

/// A tenant held at its cap with a backlog does not hold the tenant clock
/// back, so a tenant that joins starts level with the uncapped tenants it
/// competes with, not with the capped one.
#[test]
fn capped_tenant_does_not_hold_the_tenant_clock_back() {
    let mut sim = Sim::new(8);
    let (capped, busy, later) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let capped_req = || AdmitRequest::new(capped, "m", 100).tenant_cap(1);
    for _ in 0..3_000 {
        sim.queue_req(capped_req());
        sim.queue(busy, busy, 100);
    }
    for _ in 0..1_000 {
        sim.grant();
    }
    for _ in 0..2_000 {
        sim.queue(later, later, 100);
    }
    let (mut to_busy, mut to_later) = (0, 0);
    for _ in 0..1_000 {
        match sim.grant() {
            k if k == busy => to_busy += 1,
            k if k == later => to_later += 1,
            _ => {}
        }
    }
    assert!(
        to_busy * 10 >= (to_busy + to_later) * 4,
        "the busy tenant had {to_busy} grants to the newcomer's {to_later}"
    );
}

/// Under the hierarchical algorithm a tenant competes only with its own
/// group's tenants, so it joins level with them, whatever the standing of
/// tenants in other groups. Here a group sending one request at a time
/// lends slots, which go by tenant standing.
#[test]
fn tenant_joining_a_group_starts_level_with_its_group() {
    let mut sim = Sim::with(FairshareAlgorithm::Hierarchical, 8);
    let (first, other, single, later) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    let req = |tenant: Uuid, group: &str, weight: i64| {
        AdmitRequest::new(tenant, "m", 100)
            .weight(weight)
            .group(group, 100)
    };
    for _ in 0..8_000 {
        sim.queue_req(req(first, "g1", 10));
        sim.queue_req(req(other, "g2", 200));
    }
    let step = |sim: &mut Sim| {
        let single_waits = sim.pool.queues.get(&single).is_some_and(|q| !q.is_empty());
        if !single_waits && !sim.is_running(single) {
            sim.queue_req(req(single, "g3", 100));
        }
        sim.grant()
    };
    for _ in 0..3_000 {
        step(&mut sim);
    }
    for _ in 0..3_000 {
        sim.queue_req(req(later, "g1", 10));
    }
    let (mut to_first, mut to_later) = (0, 0);
    for _ in 0..1_000 {
        match step(&mut sim) {
            k if k == first => to_first += 1,
            k if k == later => to_later += 1,
            _ => {}
        }
    }
    assert!(
        to_first * 10 >= (to_first + to_later) * 4,
        "the group's first tenant had {to_first} grants to the newcomer's {to_later}"
    );
}

/// Shared mode: a tenant that a key moved to is kept while the key's
/// claim is out under its old tenant, so a refused claim goes back in
/// line under it at its own weight.
#[test]
fn owner_of_a_key_with_a_claim_out_is_kept() {
    let mut sim = Sim::new(4);
    let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (moved, other) = (Uuid::new_v4(), Uuid::new_v4());
    for _ in 0..2_000 {
        sim.queue(c, other, 100);
    }
    for _ in 0..20 {
        sim.grant();
    }
    while let Some((t, k)) = sim.running.pop_front() {
        sim.pool.release(t, k, 1);
    }
    sim.queue(a, moved, 100);
    let take_until = |sim: &mut Sim, tenant: Uuid| loop {
        let (next, _) = sim.pool.take_next(sim.pool.cap);
        let (t, k, w) = next.expect("a request can be taken");
        if t == tenant && k == moved {
            break PendingClaim {
                tenant: t,
                key: k,
                waiter: w,
            };
        }
        sim.pool.release(t, k, 1);
    };
    let claim = take_until(&mut sim, a);
    sim.queue(b, moved, 100);
    take_until(&mut sim, b);
    for _ in 0..50 {
        let (next, _) = sim.pool.take_next(sim.pool.cap);
        let (t, k, _) = next.expect("a request can be taken");
        sim.pool.release(t, k, 1);
    }
    sim.pool.release(b, moved, 1);
    assert!(sim.pool.tenant_weight.contains_key(&b), "b was forgotten");
    sim.pool.requeue(claim);
    sim.queue(b, moved, 100);
    let standing = |t: Uuid| sim.pool.served[&t] / sim.pool.tenant_weight[&t] as f64;
    assert!(
        standing(b) >= standing(c) - 1.0,
        "b went back in line at {}, below c at {}",
        standing(b),
        standing(c)
    );
}

/// Under the hierarchical algorithm, a tenant moved to another group
/// starts at that group's clock: the service it had in its old group is
/// on another group's scale and neither holds it back nor puts it ahead
/// of the new group's tenants.
#[test]
fn tenant_moved_to_another_group_starts_at_its_clock() {
    let mut sim = Sim::with(FairshareAlgorithm::Hierarchical, 4);
    let (moved, stays) = (Uuid::new_v4(), Uuid::new_v4());
    let req = |tenant: Uuid, group: &str, weight: i64| {
        AdmitRequest::new(tenant, "m", 100)
            .weight(weight)
            .group(group, 100)
    };
    for _ in 0..2_000 {
        sim.queue_req(req(moved, "g1", 10));
        sim.queue_req(req(stays, "g2", 100));
    }
    for _ in 0..1_000 {
        sim.grant();
    }
    sim.queue_req(req(moved, "g2", 10));
    let standing = |t: Uuid| sim.pool.tenant_standing(&t).unwrap();
    let clock = sim.pool.tenant_clock["g2"];
    assert!(
        standing(moved) >= clock && standing(moved) <= standing(stays) + 1.0,
        "the moved tenant stands at {} in a group whose clock is {clock} and tenant {}",
        standing(moved),
        standing(stays)
    );
}

/// Shared mode: a claim refused after its tenant's or key's weight
/// changed is refunded at the new weight's scale, so the standing goes
/// back to where it was before the claim was taken.
#[test]
fn refused_claim_after_a_weight_change_restores_the_standing() {
    for (from, to) in [(10, 1_000), (1_000, 10)] {
        let mut sim = Sim::new(2);
        let (tenant, key) = (Uuid::new_v4(), Uuid::new_v4());
        let at = |w: i64| AdmitRequest::new(tenant, "m", 100).weight(w).key(key, w);
        sim.queue_req(at(from));
        sim.grant();
        let (t, k) = sim.running.pop_front().unwrap();
        sim.pool.release(t, k, 1);
        sim.queue_req(at(from));
        sim.queue_req(at(from));
        sim.grant();
        let actual = |pool: &Pool| pool.served[&tenant] - pool.served_base[&tenant];
        let actual_before = actual(&sim.pool);
        let before = (
            sim.pool.tenant_standing(&tenant).unwrap(),
            sim.pool.key_standing(&key),
        );
        let (next, _) = sim.pool.take_next(sim.pool.cap);
        let (t, k, waiter) = next.expect("a request can be taken");
        sim.queue_req(at(to));
        sim.pool.requeue(PendingClaim {
            tenant: t,
            key: k,
            waiter,
        });
        let after = (
            sim.pool.tenant_standing(&tenant).unwrap(),
            sim.pool.key_standing(&key),
        );
        assert!(
            (after.0 - before.0).abs() < 1e-9 && (after.1 - before.1).abs() < 1e-9,
            "weight {from} to {to}: standing {before:?} before the claim, {after:?} after its refund"
        );
        let actual_after = actual(&sim.pool);
        assert!(
            (actual_after - actual_before).abs() < 1e-9,
            "weight {from} to {to}: service {actual_before} before the claim, {actual_after} after"
        );
    }
}

/// Shared mode: a claim refused after its tenant moved to another group
/// does not take the tenant below that group's clock, where the move
/// placed it, and undoes only the service it was billed.
#[test]
fn refused_claim_after_a_group_move_stays_at_the_new_clock() {
    let mut sim = Sim::with(FairshareAlgorithm::Hierarchical, 4);
    let (moved, stays) = (Uuid::new_v4(), Uuid::new_v4());
    let req = |tenant: Uuid, group: &str, weight: i64| {
        AdmitRequest::new(tenant, "m", 100)
            .weight(weight)
            .group(group, 100)
    };
    for _ in 0..2_000 {
        sim.queue_req(req(moved, "g1", 10));
        sim.queue_req(req(stays, "g2", 100));
    }
    for _ in 0..1_000 {
        sim.grant();
    }
    let claim = loop {
        if sim.running.len() >= sim.pool.cap {
            let (t, k) = sim.running.pop_front().unwrap();
            sim.pool.release(t, k, 1);
        }
        let (next, _) = sim.pool.take_next(sim.pool.cap);
        let (tenant, key, waiter) = next.expect("a request can be taken");
        if tenant == moved {
            break PendingClaim {
                tenant,
                key,
                waiter,
            };
        }
        sim.running.push_back((tenant, key));
    };
    let actual = |pool: &Pool| pool.served[&moved] - pool.served_base[&moved];
    let billed = actual(&sim.pool);
    sim.queue_req(req(moved, "g2", 10));
    sim.pool.requeue(claim);
    let clock = sim.pool.tenant_clock["g2"];
    let standing = sim.pool.tenant_standing(&moved).unwrap();
    assert!(
        standing >= clock - 1e-9,
        "the moved tenant went back in line at {standing}, below its group's clock {clock}"
    );
    assert!((actual(&sim.pool) - (billed - 100.0)).abs() < 1e-9);
}

/// A tenant's new weight reaches the requests it already has queued or
/// out as claims, so taking one of them does not record the old weight
/// again.
#[test]
fn new_tenant_weight_reaches_queued_and_refused_requests() {
    let mut sim = Sim::new(2);
    let tenant = Uuid::new_v4();
    let at = |weight: i64| AdmitRequest::new(tenant, "m", 100).weight(weight);
    for _ in 0..5 {
        sim.queue_req(at(100));
    }
    let (next, _) = sim.pool.take_next(sim.pool.cap);
    let (t, key, waiter) = next.expect("a request can be taken");
    sim.queue_req(at(300));
    sim.pool.requeue(PendingClaim {
        tenant: t,
        key,
        waiter,
    });
    for _ in 0..6 {
        let (next, _) = sim.pool.take_next(sim.pool.cap);
        let (t, k, _) = next.expect("a request can be taken");
        assert_eq!(sim.pool.tenant_weight[&tenant], 300);
        sim.pool.release(t, k, 1);
    }
}

/// Longest run of grants in a row that did not go to `key`.
fn longest_wait(grants: &[Uuid], key: Uuid) -> usize {
    let (mut run, mut longest) = (0, 0);
    for k in grants {
        if *k == key {
            run = 0;
        } else {
            run += 1;
            longest = longest.max(run);
        }
    }
    longest
}

/// A tenant's new weight applies to service from then on: its standing
/// is kept, so a raise does not take every grant and a cut does not
/// starve it while its past service is weighed again.
#[test]
fn tenant_weight_change_keeps_its_standing() {
    for (from, to) in [(50, 200), (200, 50)] {
        let mut sim = Sim::new(1);
        let (steady, changed) = (Uuid::new_v4(), Uuid::new_v4());
        for _ in 0..4_000 {
            sim.queue_req(AdmitRequest::new(steady, "m", 100));
            sim.queue_req(AdmitRequest::new(changed, "m", 100).weight(from));
        }
        for _ in 0..1_500 {
            sim.grant();
        }
        sim.queue_req(AdmitRequest::new(changed, "m", 100).weight(to));
        let grants: Vec<Uuid> = (0..1_500).map(|_| sim.grant()).collect();
        let share = grants.iter().filter(|k| **k == changed).count() as f64 / 1_500.0;
        let fair = to as f64 / (to + 100) as f64;
        assert!(
            (share - fair).abs() < 0.05,
            "weight {from} to {to}: share {share:.2}, fair {fair:.2}"
        );
        assert!(longest_wait(&grants, steady) <= 4);
        assert!(longest_wait(&grants, changed) <= 4);
    }
}

/// The same for a key's weight inside its tenant.
#[test]
fn key_weight_change_keeps_its_standing() {
    let mut sim = Sim::new(1);
    let (tenant, steady, changed) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    for _ in 0..3_000 {
        sim.queue(tenant, steady, 100);
        sim.queue(tenant, changed, 100);
    }
    for _ in 0..1_000 {
        sim.grant();
    }
    sim.queue_req(AdmitRequest::new(tenant, "m", 100).key(changed, 300));
    let grants: Vec<Uuid> = (0..1_000).map(|_| sim.grant()).collect();
    let share = grants.iter().filter(|k| **k == changed).count() as f64 / 1_000.0;
    assert!((share - 0.75).abs() < 0.05, "share {share:.2}, fair 0.75");
    assert!(longest_wait(&grants, steady) <= 4);
}

/// A tenant whose queued requests are all held at their key's cap gains
/// no credit while it waits, so when it has an open key again it does
/// not take every slot until its service catches up.
#[test]
fn tenant_held_at_a_cap_gains_no_credit() {
    let mut sim = Sim::new(4);
    let (held, busy) = (Uuid::new_v4(), Uuid::new_v4());
    let (capped, open) = (Uuid::new_v4(), Uuid::new_v4());
    for _ in 0..5_000 {
        sim.queue_req(
            AdmitRequest::new(held, "m", 100)
                .key(capped, 100)
                .key_cap(1),
        );
        sim.queue(busy, busy, 100);
    }
    for _ in 0..2_000 {
        sim.grant();
    }
    for _ in 0..3_000 {
        sim.queue(held, open, 100);
    }
    let grants: Vec<Uuid> = (0..3_000).map(|_| sim.grant()).collect();
    let wait = longest_wait(&grants, busy);
    assert!(wait <= 4, "the busy tenant waited {wait} grants in a row");
}

/// The same for a key held at its cap inside its tenant, once the cap is
/// lifted.
#[test]
fn key_held_at_its_cap_gains_no_credit() {
    let mut sim = Sim::new(4);
    let (tenant, capped, busy) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    for _ in 0..5_000 {
        sim.queue_req(
            AdmitRequest::new(tenant, "m", 100)
                .key(capped, 100)
                .key_cap(1),
        );
        sim.queue(tenant, busy, 100);
    }
    for _ in 0..2_000 {
        sim.grant();
    }
    sim.queue(tenant, capped, 100);
    let grants: Vec<Uuid> = (0..2_000).map(|_| sim.grant()).collect();
    assert!(
        grants.contains(&capped) && !sim.pool.key_cap.contains_key(&capped),
        "the cap was lifted"
    );
    let wait = longest_wait(&grants, busy);
    assert!(wait <= 4, "the busy key waited {wait} grants in a row");
}

/// A tenant's cap lifted by its latest request reaches its queued
/// requests, so taking one does not set the old cap again.
#[test]
fn lifted_tenant_cap_reaches_queued_requests() {
    let mut sim = Sim::new(4);
    let tenant = Uuid::new_v4();
    for _ in 0..20 {
        sim.queue_req(AdmitRequest::new(tenant, "m", 100).tenant_cap(1));
    }
    sim.grant();
    sim.queue_req(AdmitRequest::new(tenant, "m", 100));
    for _ in 0..3 {
        sim.grant();
    }
    assert_eq!(sim.pool.tenant_in_flight.get(&tenant), Some(&4));
}

/// Under the hierarchical algorithm a quiet tenant alone in its group,
/// whose clock only its own grants move, is still forgotten.
#[test]
fn quiet_tenant_alone_in_its_group_is_forgotten() {
    let mut sim = Sim::with(FairshareAlgorithm::Hierarchical, 2);
    let (busy, passing) = (Uuid::new_v4(), Uuid::new_v4());
    for _ in 0..200 {
        sim.queue_req(AdmitRequest::new(busy, "m", 100).group("g1", 100));
    }
    sim.queue_req(AdmitRequest::new(passing, "m", 100).group("g2", 100));
    sim.grants_before(passing);
    for _ in 0..10 {
        sim.grant();
    }
    assert!(!sim.is_running(passing));
    assert!(!sim.pool.served.contains_key(&passing));
}

/// A tenant kept because a key it owns was running under its old tenant
/// is forgotten once that request ends, if it is quiet.
#[test]
fn owner_kept_for_a_running_key_is_forgotten_when_it_ends() {
    let mut sim = Sim::new(4);
    let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (moved, other) = (Uuid::new_v4(), Uuid::new_v4());
    for _ in 0..2_000 {
        sim.queue(c, other, 100);
    }
    for _ in 0..20 {
        sim.grant();
    }
    while let Some((t, k)) = sim.running.pop_front() {
        sim.pool.release(t, k, 1);
    }
    sim.queue(a, moved, 100);
    let take = |sim: &mut Sim, tenant: Uuid| loop {
        let (next, _) = sim.pool.take_next(sim.pool.cap);
        let (t, k, _) = next.expect("a request can be taken");
        if t == tenant && k == moved {
            break;
        }
        sim.pool.release(t, k, 1);
    };
    take(&mut sim, a);
    sim.queue(b, moved, 100);
    take(&mut sim, b);
    for _ in 0..50 {
        let (next, _) = sim.pool.take_next(sim.pool.cap);
        let (t, k, _) = next.expect("a request can be taken");
        sim.pool.release(t, k, 1);
    }
    sim.pool.release(b, moved, 1);
    assert!(sim.pool.served.contains_key(&b), "kept while its key runs");
    sim.pool.release(a, moved, 1);
    assert!(!sim.pool.served.contains_key(&b), "forgotten once it ends");
}

/// Shared mode: a round of claims refused together leaves a tenant or
/// key held at its cap only by its own claim level with the others, not
/// raised by the clock moves the other claims made.
#[test]
fn refused_round_leaves_a_capped_tenant_and_key_level() {
    for capped_key in [false, true] {
        let mut sim = Sim::new(8);
        let (capped, other) = (Uuid::new_v4(), Uuid::new_v4());
        let shared = Uuid::new_v4();
        let req = |id: Uuid| {
            let r = if capped_key {
                AdmitRequest::new(shared, "m", 100).key(id, 100)
            } else {
                AdmitRequest::new(id, "m", 100)
            };
            match (id == capped, capped_key) {
                (true, true) => r.key_cap(1),
                (true, false) => r.tenant_cap(1),
                _ => r,
            }
        };
        for _ in 0..10 {
            sim.queue_req(req(capped));
            sim.queue_req(req(other));
        }
        let standing = |pool: &Pool, id: Uuid| {
            if capped_key {
                pool.key_standing(&id)
            } else {
                pool.tenant_standing(&id).unwrap()
            }
        };
        for id in 0..4 {
            let (next, _) = sim.pool.take_claim(sim.pool.cap);
            let (tenant, key, waiter) = next.expect("a request can be taken");
            sim.pool.pending.insert(
                id,
                PendingClaim {
                    tenant,
                    key,
                    waiter,
                },
            );
        }
        for id in 0..4 {
            let claim = sim.pool.pending.remove(&id).unwrap();
            sim.pool.requeue(claim);
        }
        let (c, o) = (standing(&sim.pool, capped), standing(&sim.pool, other));
        assert!(
            (c - o).abs() < 1e-9,
            "capped {capped_key}: after the refused round {c} against {o}"
        );
    }
}

/// Admit `req` straight into a free slot and leave it running.
fn fast_running(sim: &mut Sim, req: AdmitRequest) {
    sim.pool.track_meta(&req);
    assert!(sim.pool.can_grant_immediately(&req, sim.pool.cap));
    let (tenant, key) = (req.tenant, req.key);
    let (respond, _admitted) = oneshot::channel();
    sim.pool.grant_fast(req, respond);
    sim.running.push_back((tenant, key));
}

/// Take a claim with `take_claim` and refuse it, as the scheduler does.
fn refuse_next_claim(sim: &mut Sim) -> (Uuid, Uuid) {
    let (next, _) = sim.pool.take_claim(sim.pool.cap);
    let (tenant, key, waiter) = next.expect("a request can be taken");
    sim.pool.pending.insert(
        0,
        PendingClaim {
            tenant,
            key,
            waiter,
        },
    );
    let claim = sim.pool.pending.remove(&0).unwrap();
    sim.pool.requeue(claim);
    (tenant, key)
}

/// Shared mode: a single claim that brings its tenant or key to its cap,
/// refused, does not leave it raised by the clock move its own charge
/// made while another was ahead of it.
#[test]
fn refused_claim_does_not_raise_its_own_capped_tenant_or_key() {
    for capped_key in [false, true] {
        let mut sim = Sim::new(8);
        let shared = Uuid::new_v4();
        let (capped, other) = (Uuid::new_v4(), Uuid::new_v4());
        let req = |id: Uuid, cost: u32| {
            let r = if capped_key {
                AdmitRequest::new(shared, "m", cost).key(id, 100)
            } else {
                AdmitRequest::new(id, "m", cost)
            };
            match (id == capped, capped_key) {
                (true, true) => r.key_cap(1),
                (true, false) => r.tenant_cap(1),
                _ => r,
            }
        };
        fast_running(&mut sim, req(other, 100));
        for _ in 0..3 {
            sim.queue_req(req(capped, 10));
            sim.queue_req(req(other, 100));
        }
        let standing = |pool: &Pool, id: Uuid| {
            if capped_key {
                pool.key_standing(&id)
            } else {
                pool.tenant_standing(&id).unwrap()
            }
        };
        let before = standing(&sim.pool, capped);
        assert!(before < standing(&sim.pool, other));
        let (tenant, key) = refuse_next_claim(&mut sim);
        assert_eq!(if capped_key { key } else { tenant }, capped);
        let after = standing(&sim.pool, capped);
        assert!(
            (after - before).abs() < 1e-9,
            "capped key {capped_key}: {before} before the refused claim, {after} after"
        );
    }
}

/// Shared mode: a tenant whose claim took the last queued request of its
/// only open key, its other key being at its cap, still counts as
/// waiting while the claim is out, so a refusal leaves it where it was.
#[test]
fn refused_claim_that_emptied_an_open_key_does_not_raise_its_tenant() {
    let mut sim = Sim::new(8);
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let (capped, open) = (Uuid::new_v4(), Uuid::new_v4());
    let at_cap = || {
        AdmitRequest::new(tenant, "m", 10)
            .key(capped, 100)
            .key_cap(1)
    };
    fast_running(&mut sim, at_cap());
    fast_running(&mut sim, AdmitRequest::new(other, "m", 100));
    for _ in 0..3 {
        sim.queue_req(at_cap());
        sim.queue(other, other, 100);
    }
    sim.queue_req(AdmitRequest::new(tenant, "m", 10).key(open, 100));
    let before = sim.pool.tenant_standing(&tenant).unwrap();
    assert!(before < sim.pool.tenant_standing(&other).unwrap());
    let (t, k) = refuse_next_claim(&mut sim);
    assert_eq!((t, k), (tenant, open));
    let after = sim.pool.tenant_standing(&tenant).unwrap();
    assert!(
        (after - before).abs() < 1e-9,
        "{before} before the refused claim, {after} after"
    );
}

/// Shared mode: a claim whose key moves to another tenant while it is
/// out does not keep counting as a waiting key of the tenant it was
/// charged to, so that tenant, its other key at its cap, is held and
/// raised by the clock as usual.
#[test]
fn claim_of_a_moved_key_does_not_keep_its_old_tenant_waiting() {
    let mut sim = Sim::new(8);
    let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let (capped, moved) = (Uuid::new_v4(), Uuid::new_v4());
    let at_cap = || AdmitRequest::new(a, "m", 10).key(capped, 100).key_cap(1);
    fast_running(&mut sim, at_cap());
    fast_running(&mut sim, AdmitRequest::new(b, "m", 100));
    for _ in 0..3 {
        sim.queue_req(at_cap());
        sim.queue(b, b, 100);
    }
    sim.queue_req(AdmitRequest::new(a, "m", 10).key(moved, 100));
    let (next, _) = sim.pool.take_claim(sim.pool.cap);
    let (tenant, key, waiter) = next.expect("a request can be taken");
    assert_eq!((tenant, key), (a, moved));
    sim.pool.pending.insert(
        0,
        PendingClaim {
            tenant,
            key,
            waiter,
        },
    );
    sim.queue(c, moved, 10);
    assert!(sim.pool.tenant_held(&a));
}
