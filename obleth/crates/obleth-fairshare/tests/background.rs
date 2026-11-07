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
