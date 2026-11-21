//! Background pools: batch work that shares a model's slots with interactive
//! traffic, served only when no interactive request waits and only up to a
//! share of the pool and of the global ceiling.

use std::sync::Arc;
use std::time::Duration;

use obleth_config::FairshareAlgorithm;
use obleth_fairshare::{
    AcquireOutcome, AdmitRequest, Admitted, FairShare, InMemoryCluster, PoolKey, SharedSlots,
    SharedSlotsConfig, SlotClaim, SlotDenial, SlotMode, StaticCapacity,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

const MODEL: &str = "m";

fn interactive() -> PoolKey {
    PoolKey::Model(MODEL.into())
}

fn background() -> PoolKey {
    PoolKey::Background(MODEL.into())
}

fn local(ceiling: usize) -> FairShare {
    FairShare::start(
        Arc::new(StaticCapacity::new(ceiling)),
        FairshareAlgorithm::Hierarchical,
        32,
    )
}

fn req(tenant: Uuid, cap: usize) -> AdmitRequest {
    AdmitRequest::new(tenant, MODEL, 1).model_cap(cap)
}

fn bg_req(tenant: Uuid, cap: usize, share: f64) -> AdmitRequest {
    req(tenant, cap).background_share(share)
}

async fn admit_now(fs: &FairShare, pool: PoolKey, req: AdmitRequest) -> Admitted {
    tokio::time::timeout(Duration::from_secs(2), fs.admit_to(pool, req))
        .await
        .expect("admitted in time")
        .expect("scheduler alive")
}

fn waiter(fs: &FairShare, pool: PoolKey, req: AdmitRequest) -> JoinHandle<Option<Admitted>> {
    let fs = fs.clone();
    tokio::spawn(async move { fs.admit_to(pool, req).await })
}

async fn still_waiting(handle: &JoinHandle<Option<Admitted>>) {
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!handle.is_finished(), "admitted past a limit");
}

async fn admitted(handle: JoinHandle<Option<Admitted>>) -> Admitted {
    tokio::time::timeout(Duration::from_secs(2), handle)
        .await
        .expect("admitted in time")
        .expect("task ran")
        .expect("scheduler alive")
}

#[test]
fn background_pool_ids_round_trip() {
    let key = background();
    assert_eq!(key.slot_id(), "b:m");
    assert_eq!(PoolKey::from_slot_id("b:m"), Some(key.clone()));
    assert_eq!(key.sibling(), Some(interactive()));
    assert_eq!(interactive().sibling(), Some(key));
    assert_eq!(PoolKey::Unrouted.sibling(), None);
    assert_eq!(obleth_fairshare::background_cap(8, 0.75), 6);
    assert_eq!(obleth_fairshare::background_cap(1, 0.75), 0);
    assert_eq!(obleth_fairshare::background_cap(8, f64::NAN), 6);
}

/// The live view names a model's background pool after the model, as
/// background work (batch items and background keys both run there); its id
/// in the shared backend stays `b:` and the model.
#[tokio::test]
async fn the_background_pool_is_shown_as_background() {
    let fs = local(1024);
    let held = admit_now(&fs, background(), bg_req(Uuid::new_v4(), 8, 0.75)).await;
    let snap = fs.snapshot().await.expect("snapshot");
    let names: Vec<&str> = snap.pools.iter().map(|p| p.model.as_str()).collect();
    assert_eq!(names, vec!["m (background)"]);
    assert_eq!(background().slot_id(), "b:m");
    drop(held);
}

/// Background work fills at most its share of the pool; the rest stays free
/// for interactive arrivals, which are admitted at once.
#[tokio::test]
async fn background_fills_only_its_share_of_the_pool() {
    let fs = local(1024);
    let t = Uuid::new_v4();
    let mut held = Vec::new();
    for _ in 0..6 {
        held.push(admit_now(&fs, background(), bg_req(t, 8, 0.75)).await);
    }
    let seventh = waiter(&fs, background(), bg_req(t, 8, 0.75));
    still_waiting(&seventh).await;

    // The two slots background left are interactive's, straight away.
    held.push(admit_now(&fs, interactive(), req(Uuid::new_v4(), 8)).await);
    held.push(admit_now(&fs, interactive(), req(Uuid::new_v4(), 8)).await);
    // And the pool is full: interactive does not overrun what background holds.
    let ninth = waiter(&fs, interactive(), req(Uuid::new_v4(), 8));
    still_waiting(&ninth).await;

    // A background slot frees: the interactive waiter takes it, background
    // keeps waiting.
    held.remove(0);
    let ninth = admitted(ninth).await;
    still_waiting(&seventh).await;
    drop(ninth);
    drop(held);
    admitted(seventh).await;
}

/// Background takes nothing new while an interactive request waits for the
/// model, even when the background share has room.
#[tokio::test]
async fn background_yields_to_waiting_interactive_requests() {
    let fs = local(1024);
    let mut busy = Vec::new();
    for _ in 0..4 {
        busy.push(admit_now(&fs, interactive(), req(Uuid::new_v4(), 4)).await);
    }
    let person = waiter(&fs, interactive(), req(Uuid::new_v4(), 4));
    still_waiting(&person).await;
    let batch = waiter(&fs, background(), bg_req(Uuid::new_v4(), 4, 0.75));
    still_waiting(&batch).await;

    // One slot frees: the person gets it.
    busy.pop();
    let person = admitted(person).await;
    still_waiting(&batch).await;

    // Occupancy falls under the background share (3 of 4): batch goes.
    busy.pop();
    busy.pop();
    admitted(batch).await;
    drop((person, busy));
}

/// The background share applies to the gateway's ceiling as well, counting
/// every request in flight on any model.
#[tokio::test]
async fn background_fills_only_its_share_of_the_ceiling() {
    let fs = local(8);
    let mut held = Vec::new();
    // Another model holds 4 of the 8.
    for _ in 0..4 {
        held.push(
            admit_now(
                &fs,
                PoolKey::Model("other".into()),
                AdmitRequest::new(Uuid::new_v4(), "other", 1).model_cap(8),
            )
            .await,
        );
    }
    // Background may fill the gateway to 6 of 8: two more.
    let t = Uuid::new_v4();
    held.push(admit_now(&fs, background(), bg_req(t, 32, 0.75)).await);
    held.push(admit_now(&fs, background(), bg_req(t, 32, 0.75)).await);
    let third = waiter(&fs, background(), bg_req(t, 32, 0.75));
    still_waiting(&third).await;
    drop(held);
    admitted(third).await;
}

/// Background work keeps its own fairness and caps: a tenant's batch does not
/// use up its interactive cap on the model.
#[tokio::test]
async fn background_slots_do_not_count_against_interactive_tenant_caps() {
    let fs = local(1024);
    let t = Uuid::new_v4();
    let a = admit_now(&fs, background(), bg_req(t, 16, 0.75).tenant_cap(2)).await;
    let b = admit_now(&fs, background(), bg_req(t, 16, 0.75).tenant_cap(2)).await;
    let c = admit_now(&fs, interactive(), req(t, 16).tenant_cap(2)).await;
    let d = admit_now(&fs, interactive(), req(t, 16).tenant_cap(2)).await;
    drop((a, b, c, d));
}

/// A share of zero turns background admission off for the model.
#[tokio::test]
async fn a_zero_share_admits_no_background_work() {
    let fs = local(1024);
    let batch = waiter(&fs, background(), bg_req(Uuid::new_v4(), 8, 0.0));
    still_waiting(&batch).await;
    batch.abort();
}

fn config() -> SharedSlotsConfig {
    SharedSlotsConfig {
        reconcile_interval: Duration::from_secs(60),
        recovery_interval: Duration::from_millis(50),
        ..SharedSlotsConfig::default()
    }
}

fn gateways(cluster: &InMemoryCluster, n: usize, ceiling: usize) -> Vec<FairShare> {
    (0..n)
        .map(|i| {
            let fs = local(ceiling);
            fs.enable_shared_slots(Arc::new(cluster.gateway(&format!("gw-{i}"))), config());
            fs.set_replicas(n);
            cluster.subscribe(&fs);
            fs
        })
        .collect()
}

async fn wait_for_shared(fs: &FairShare) {
    let stats = fs.stats();
    tokio::time::timeout(Duration::from_secs(5), async {
        while stats.mode() != SlotMode::Shared {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .expect("shared mode");
}

/// Across gateways the share is cluster-wide: background on one gateway and
/// interactive on another together never exceed the pool, background never
/// exceeds its share, and a freed background slot wakes an interactive
/// waiter elsewhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_background_share_holds_across_gateways() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, 4096);
    for fs in &gws {
        wait_for_shared(fs).await;
    }
    let t = Uuid::new_v4();
    let mut batch = Vec::new();
    for _ in 0..6 {
        batch.push(admit_now(&gws[0], background(), bg_req(t, 8, 0.75)).await);
    }
    assert_eq!(cluster.pool_in_flight(&background().slot_id()), 6);
    // Background on the other gateway is refused: the share is used up.
    let more = waiter(&gws[1], background(), bg_req(t, 8, 0.75));
    still_waiting(&more).await;

    // Interactive gets the remaining two, then the pool is full.
    let p1 = admit_now(&gws[1], interactive(), req(Uuid::new_v4(), 8)).await;
    let p2 = admit_now(&gws[1], interactive(), req(Uuid::new_v4(), 8)).await;
    let p3 = waiter(&gws[1], interactive(), req(Uuid::new_v4(), 8));
    still_waiting(&p3).await;

    // A background slot frees on gw-0: gw-1's interactive waiter gets it.
    batch.pop();
    let p3 = admitted(p3).await;
    still_waiting(&more).await;

    drop((p1, p2, p3));
    batch.clear();
    admitted(more).await;
}

/// A slot freed while an interactive request waits on another gateway goes
/// to that request, even when background work waits on the gateway nearer
/// the release: background never takes a slot an interactive request anywhere
/// is waiting for.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn background_yields_to_interactive_waiters_on_other_gateways() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 3, 4096);
    for fs in &gws {
        wait_for_shared(fs).await;
    }
    // gw-1 fills the pool with interactive work and has two more waiting.
    let mut running = Vec::new();
    for _ in 0..4 {
        running.push(admit_now(&gws[1], interactive(), req(Uuid::new_v4(), 4)).await);
    }
    let w1 = waiter(&gws[1], interactive(), req(Uuid::new_v4(), 4));
    let w2 = waiter(&gws[1], interactive(), req(Uuid::new_v4(), 4));
    still_waiting(&w1).await;
    // Background waits on the other two gateways.
    let t = Uuid::new_v4();
    let b0 = waiter(&gws[0], background(), bg_req(t, 4, 0.75));
    let b2 = waiter(&gws[2], background(), bg_req(t, 4, 0.75));
    still_waiting(&b0).await;
    still_waiting(&b2).await;

    // Each release while gw-1 still has a waiter goes to one of its waiters
    // (whichever fairshare picks), never to background.
    let fg_now = || cluster.pool_in_flight(&interactive().slot_id());
    running.pop();
    let one = tokio::time::timeout(Duration::from_secs(2), async {
        while !(w1.is_finished() || w2.is_finished()) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(one.is_ok(), "an interactive waiter got the slot");
    assert_eq!(fg_now(), 4);
    still_waiting(&b0).await;
    still_waiting(&b2).await;
    running.pop();
    let (a, b) = (admitted(w1).await, admitted(w2).await);
    running.extend([a, b]);
    assert_eq!(fg_now(), 4);
    still_waiting(&b0).await;
    still_waiting(&b2).await;
    assert_eq!(cluster.pool_in_flight(&background().slot_id()), 0);

    // Nobody waits now: freed slots go to background, up to its share.
    running.clear();
    admitted(b0).await;
    admitted(b2).await;
}

/// The backend rule under the scheduler: while an interactive claim on the
/// model was refused within the wait-mark window, a background claim is
/// refused although the pool has room under background's share.
#[tokio::test]
async fn a_waited_on_interactive_pool_refuses_background_claims() {
    let cluster = InMemoryCluster::new();
    let a = cluster.gateway("gw-a");
    let b = cluster.gateway("gw-b");
    let fg = |cap| SlotClaim {
        pool: interactive().slot_id(),
        tenant: Uuid::new_v4(),
        key: Uuid::new_v4(),
        slots: 1,
        pool_cap: cap,
        global_cap: 100,
        sibling: Some(background().slot_id()),
        background: false,
        tenant_cap: None,
        key_cap: None,
        hold: None,
        redeem_only: false,
        starving: false,
    };
    let bg = SlotClaim {
        pool: background().slot_id(),
        sibling: Some(interactive().slot_id()),
        background: true,
        ..fg(3)
    };
    // No one waits: background may claim.
    let (outcome, _) = a.acquire(bg.clone()).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Granted);
    // An interactive claim on gw-b is refused (a cap of 1 stands for a full
    // pool), which marks the interactive pool as waited on.
    let (outcome, _) = b.acquire(fg(1)).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Pool));
    // Room for background (1 of its 3), but an interactive request waits.
    let (outcome, _) = a.acquire(bg.clone()).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Pool));
    // The mark lapses once nobody has been refused for a while.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    let (outcome, _) = a.acquire(bg).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Granted);
}

/// Views size a background pool by the share it may fill, which is what
/// admission enforces, so fair shares come out of those slots and not the
/// whole model's.
#[tokio::test]
async fn the_background_pool_reports_the_slots_it_may_fill() {
    let fs = local(1024);
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let held = vec![
        admit_now(&fs, background(), bg_req(a, 256, 0.75)).await,
        admit_now(&fs, background(), bg_req(b, 256, 0.75)).await,
        admit_now(&fs, interactive(), req(Uuid::new_v4(), 256)).await,
    ];
    let snap = fs.snapshot().await.expect("snapshot");
    let pool = |name: &str| {
        snap.pools
            .iter()
            .find(|p| p.model == name)
            .expect("pool listed")
    };
    let bg = pool("m (background)");
    assert_eq!((bg.cap, bg.configured_cap), (192, 192));
    let mut shares: Vec<f64> = bg
        .tenants
        .iter()
        .map(|t| t.weight_share * bg.cap as f64)
        .collect();
    shares.sort_by(f64::total_cmp);
    assert_eq!(shares, vec![96.0, 96.0]);
    // The model's interactive pool keeps its whole size.
    let fg = pool("m");
    assert_eq!((fg.cap, fg.configured_cap), (256, 256));
    drop(held);
}

/// The cluster-wide view, merged from every gateway's, sizes the background
/// pool the same way.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_cluster_view_reports_the_background_pool_by_its_share() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, 4096);
    for fs in &gws {
        wait_for_shared(fs).await;
    }
    let held = vec![
        admit_now(&gws[0], background(), bg_req(Uuid::new_v4(), 8, 0.75)).await,
        admit_now(&gws[1], background(), bg_req(Uuid::new_v4(), 8, 0.75)).await,
    ];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let pool = loop {
        let snap = gws[0].cluster_snapshot().await.expect("backend answers");
        if let Some(p) = snap
            .pools
            .into_iter()
            .find(|p| p.model == "m (background)" && p.tenants.len() == 2)
        {
            break p;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "both gateways' tenants in the view"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!((pool.cap, pool.configured_cap), (6, 6));
    for t in &pool.tenants {
        assert_eq!(t.weight_share * pool.cap as f64, 3.0);
    }
    drop(held);
}

/// The batch runner's narrow read: what waits in background pools, per model
/// and key, the same counts the full snapshot reports; and the pools-only
/// snapshot keeps every count but leaves tenants and keys out.
#[tokio::test]
async fn background_queue_counts_and_the_pools_only_snapshot_match_the_full_one() {
    let fs = local(1024);
    let t = Uuid::new_v4();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let mut held = Vec::new();
    for _ in 0..6 {
        held.push(admit_now(&fs, background(), bg_req(t, 8, 0.75).key(a, 1)).await);
    }
    let waiting = [
        waiter(&fs, background(), bg_req(t, 8, 0.75).key(a, 1)),
        waiter(&fs, background(), bg_req(t, 8, 0.75).key(a, 1)),
        waiter(&fs, background(), bg_req(t, 8, 0.75).key(b, 1)),
    ];
    for w in &waiting {
        still_waiting(w).await;
    }

    let queued = fs.background_queued().await.expect("scheduler alive");
    assert_eq!(queued.len(), 2, "{queued:?}");
    assert_eq!(queued[&("m".to_string(), a)], 2);
    assert_eq!(queued[&("m".to_string(), b)], 1);
    let full = fs.snapshot().await.expect("snapshot");
    let from_full: usize = full.pools[0].keys.iter().map(|k| k.queued).sum();
    assert_eq!(from_full, 3);

    let pools = fs.pool_snapshot().await.expect("snapshot");
    let (p, f) = (&pools.pools[0], &full.pools[0]);
    assert!(p.tenants.is_empty() && p.keys.is_empty());
    assert_eq!(
        (p.model.as_str(), p.in_flight, p.queued, p.cap, p.borrowed),
        (f.model.as_str(), f.in_flight, f.queued, f.cap, f.borrowed)
    );
    assert_eq!(p.groups.len(), f.groups.len());
    assert_eq!((pools.global_in_flight, pools.global_queued), (6, 3));
    drop(held);
    for w in waiting {
        admitted(w).await;
    }
}

// ---- background slots by prompt length ------------------------------------

/// The background pool's requests and the slots they hold, from the live
/// view: `(in_flight, slots)`.
async fn bg_load(fs: &FairShare) -> (usize, usize) {
    let snap = fs.snapshot().await.expect("snapshot");
    snap.pools
        .iter()
        .find(|p| p.model == "m (background)")
        .map_or((0, 0), |p| (p.in_flight, p.slots))
}

fn long(tenant: Uuid, cap: usize, slots: usize) -> AdmitRequest {
    bg_req(tenant, cap, 0.5).slots(slots)
}

#[test]
fn background_slots_follow_the_prompt() {
    use obleth_fairshare::background_slots;
    assert_eq!(background_slots(0, 4096), 1);
    assert_eq!(background_slots(200, 4096), 1);
    assert_eq!(background_slots(4096, 4096), 1);
    assert_eq!(background_slots(4097, 4096), 2);
    assert_eq!(background_slots(13_000, 4096), 4);
    // Off: one slot whatever the prompt.
    assert_eq!(background_slots(13_000, 0), 1);
    assert_eq!(background_slots(u32::MAX, 0), 1);
}

/// A background request holds slots in proportion to its prompt, the
/// background cap counts those slots rather than requests, and the
/// interactive pool gives up exactly the slots background holds.
#[tokio::test]
async fn a_long_prompt_holds_slots_and_the_cap_counts_them() {
    let fs = local(1024);
    let t = Uuid::new_v4();
    // 16 slots, half for background: 8.
    let a = admit_now(&fs, background(), long(t, 16, 3)).await;
    let b = admit_now(&fs, background(), long(t, 16, 3)).await;
    assert_eq!((a.permit.slots(), b.permit.slots()), (3, 3));
    assert_eq!(bg_load(&fs).await, (2, 6));
    // Two requests run, but a third of 3 slots does not fit in the 2 left;
    // a one-slot request behind it waits its turn rather than passing it.
    let third = waiter(&fs, background(), long(t, 16, 3));
    still_waiting(&third).await;
    let small = waiter(&fs, background(), long(t, 16, 1));
    still_waiting(&small).await;

    // Interactive has the 10 slots background does not hold, and no more.
    let mut people = Vec::new();
    for _ in 0..10 {
        people.push(admit_now(&fs, interactive(), req(Uuid::new_v4(), 16)).await);
    }
    assert!(people.iter().all(|p| p.permit.slots() == 1));
    let eleventh = waiter(&fs, interactive(), req(Uuid::new_v4(), 16));
    still_waiting(&eleventh).await;
    // One background request ends: its 3 slots come back, and the waiting
    // interactive request goes first (background yields while it waits).
    drop(a);
    let eleventh = admitted(eleventh).await;
    still_waiting(&third).await;
    drop(eleventh);
    drop(people);
    let third = admitted(third).await;
    let small = admitted(small).await;
    assert_eq!(bg_load(&fs).await, (3, 7));
    drop((b, third, small));
    assert_eq!(bg_load(&fs).await, (0, 0));
}

/// A prompt bigger than the whole background share holds the share and runs
/// alone, once nothing else background runs, even with interactive work on
/// the model; nothing background joins it meanwhile.
#[tokio::test]
async fn a_prompt_bigger_than_the_share_runs_alone() {
    let fs = local(1024);
    let t = Uuid::new_v4();
    let small = admit_now(&fs, background(), long(t, 16, 1)).await;
    let huge = waiter(&fs, background(), long(t, 16, 40));
    still_waiting(&huge).await;
    let person = admit_now(&fs, interactive(), req(Uuid::new_v4(), 16)).await;
    drop(small);
    let huge = admitted(huge).await;
    assert_eq!(huge.permit.slots(), 8, "capped at the background share");
    assert_eq!(bg_load(&fs).await, (1, 8));
    let next = waiter(&fs, background(), long(t, 16, 1));
    still_waiting(&next).await;
    // The interactive pool still has what neither holds: 16 - 8 - 1.
    let mut people = Vec::new();
    for _ in 0..7 {
        people.push(admit_now(&fs, interactive(), req(Uuid::new_v4(), 16)).await);
    }
    let over = waiter(&fs, interactive(), req(Uuid::new_v4(), 16));
    still_waiting(&over).await;
    drop((huge, person, people));
    admitted(over).await;
    admitted(next).await;
}

/// Every way a request ends gives back exactly the slots it held: a finished
/// request, a caller that leaves while holding its slots, and one that leaves
/// while still queued (which held none).
#[tokio::test]
async fn release_returns_exactly_what_was_held() {
    let fs = local(1024);
    let t = Uuid::new_v4();
    // Finished.
    let done = admit_now(&fs, background(), long(t, 16, 5)).await;
    assert_eq!(bg_load(&fs).await, (1, 5));
    drop(done);
    assert_eq!(bg_load(&fs).await, (0, 0));

    // A caller that goes away while holding (an aborted stream).
    let holder = {
        let fs = fs.clone();
        tokio::spawn(async move {
            let held = fs.admit_to(background(), long(t, 16, 4)).await;
            std::future::pending::<()>().await;
            drop(held);
        })
    };
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while bg_load(&fs).await != (1, 4) {
        assert!(tokio::time::Instant::now() < deadline, "admitted");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Queued behind it: 4 + 6 > 8.
    let queued = waiter(&fs, background(), long(t, 16, 6));
    still_waiting(&queued).await;
    queued.abort();
    holder.abort();
    let _ = holder.await;
    assert_eq!(bg_load(&fs).await, (0, 0));
    // Nothing leaked: the whole share is there again.
    let all = admit_now(&fs, background(), long(t, 16, 8)).await;
    assert_eq!(bg_load(&fs).await, (1, 8));
    drop(all);
    assert_eq!(bg_load(&fs).await, (0, 0));
}

/// With slot tokens off every background request asks for one slot, and the
/// share counts requests as before.
#[tokio::test]
async fn one_slot_per_request_when_slot_tokens_are_off() {
    let fs = local(1024);
    let t = Uuid::new_v4();
    let one = obleth_fairshare::background_slots(13_000, 0);
    let mut held = Vec::new();
    for _ in 0..8 {
        held.push(admit_now(&fs, background(), long(t, 16, one)).await);
    }
    assert_eq!(bg_load(&fs).await, (8, 8));
    let ninth = waiter(&fs, background(), long(t, 16, one));
    still_waiting(&ninth).await;
    held.pop();
    admitted(ninth).await;
}

/// Slots are a background matter: an interactive request holds one slot
/// whatever it asks for.
#[tokio::test]
async fn interactive_requests_hold_one_slot_each() {
    let fs = local(1024);
    let mut held = Vec::new();
    for _ in 0..4 {
        held.push(admit_now(&fs, interactive(), req(Uuid::new_v4(), 4).slots(3)).await);
    }
    assert!(held.iter().all(|a| a.permit.slots() == 1));
    let snap = fs.snapshot().await.expect("snapshot");
    let fg = snap.pools.iter().find(|p| p.model == "m").expect("pool");
    assert_eq!((fg.in_flight, fg.slots), (4, 4));
    let fifth = waiter(&fs, interactive(), req(Uuid::new_v4(), 4));
    still_waiting(&fifth).await;
    held.pop();
    admitted(fifth).await;
}

/// Across gateways the backend counts each gateway's multi-slot holds in the
/// pool and one request each in the global count, they add up against the
/// share, and they come back exactly on release and when a gateway dies.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_slot_holds_add_up_across_gateways() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, 4096);
    for fs in &gws {
        wait_for_shared(fs).await;
    }
    let pool = background().slot_id();
    let t = Uuid::new_v4();
    let a = admit_now(&gws[0], background(), long(t, 16, 4)).await;
    let b = admit_now(&gws[1], background(), long(t, 16, 3)).await;
    assert_eq!(cluster.pool_in_flight(&pool), 7);
    assert_eq!(cluster.global_in_flight(), 2);
    assert_eq!(
        (cluster.held("gw-0", &pool), cluster.held("gw-1", &pool)),
        (4, 3)
    );
    // 7 + 2 > 8 cluster-wide, though gw-0 alone holds only 4.
    let more = waiter(&gws[0], background(), long(t, 16, 2));
    still_waiting(&more).await;
    drop(a);
    let more = admitted(more).await;
    assert_eq!(cluster.pool_in_flight(&pool), 5);
    assert_eq!(cluster.global_in_flight(), 2);

    // gw-1's heartbeat runs out: its 3 slots, and only those, come back.
    cluster.kill("gw-1");
    assert_eq!(cluster.pool_in_flight(&pool), 2);
    assert_eq!(cluster.global_in_flight(), 1);
    drop(b);
    drop(more);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while cluster.pool_in_flight(&pool) != 0 || cluster.global_in_flight() != 0 {
        assert!(tokio::time::Instant::now() < deadline, "released cleanly");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(bg_load(&gws[0]).await, (0, 0));
}

/// A claim that fails on a backend outage gives its local slots back before
/// the gateway falls back to its split; the request admitted there is then
/// written to the backend by the reconcile with all its slots, and its
/// release clears them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn multi_slot_holds_survive_a_backend_outage() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, 4096);
    for fs in &gws {
        wait_for_shared(fs).await;
    }
    let pool = background().slot_id();
    let t = Uuid::new_v4();
    cluster.set_failing(true);
    // The split gives this gateway 8 of 16, 4 of them background.
    let held = admit_now(&gws[0], background(), long(t, 16, 4)).await;
    assert_eq!(held.permit.slots(), 4);
    assert_eq!(bg_load(&gws[0]).await, (1, 4));
    assert_eq!(gws[0].stats().mode(), SlotMode::Fallback);
    cluster.set_failing(false);
    wait_for_shared(&gws[0]).await;
    assert_eq!(cluster.held("gw-0", &pool), 4);
    assert_eq!(cluster.global_in_flight(), 1);
    drop(held);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while cluster.pool_in_flight(&pool) != 0 || cluster.global_in_flight() != 0 {
        assert!(tokio::time::Instant::now() < deadline, "released cleanly");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(bg_load(&gws[0]).await, (0, 0));
}
