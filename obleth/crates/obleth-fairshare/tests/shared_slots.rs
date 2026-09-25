//! Cluster-wide shared slots, with several schedulers in one process sharing
//! an in-memory backend that follows the Redis backend's rules.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use obleth_config::{Admission, FairshareAlgorithm};
use obleth_fairshare::{
    AdmitRequest, Admitted, FairShare, InMemoryCluster, PoolKey, SharedSlotsConfig, SlotMode,
    StaticCapacity,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

const MODEL: &str = "m";

fn pool_id() -> String {
    PoolKey::Model(MODEL.into()).slot_id()
}

fn config() -> SharedSlotsConfig {
    SharedSlotsConfig {
        reconcile_interval: Duration::from_secs(60),
        recovery_interval: Duration::from_millis(50),
        ..SharedSlotsConfig::default()
    }
}

/// `n` schedulers sharing `cluster`, each told `n` replicas are live and
/// subscribed to release wakeups.
fn gateways(
    cluster: &InMemoryCluster,
    n: usize,
    ceiling: usize,
    config: SharedSlotsConfig,
) -> Vec<FairShare> {
    (0..n)
        .map(|i| {
            let fs = FairShare::start(
                Arc::new(StaticCapacity::new(ceiling)),
                FairshareAlgorithm::Hierarchical,
                32,
            );
            fs.enable_shared_slots(
                Arc::new(cluster.gateway(&format!("gw-{i}"))),
                config.clone(),
            );
            fs.set_replicas(n);
            cluster.subscribe(&fs);
            fs
        })
        .collect()
}

async fn wait_until(what: &str, mut ok: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !ok() {
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"));
}

async fn wait_for_mode(fs: &FairShare, mode: SlotMode) {
    let stats = fs.stats();
    wait_until(mode.as_str(), || stats.mode() == mode).await;
}

async fn admit_now(fs: &FairShare, req: AdmitRequest) -> Admitted {
    tokio::time::timeout(Duration::from_secs(2), fs.admit(req))
        .await
        .expect("admitted in time")
        .expect("scheduler alive")
}

fn waiter(fs: &FairShare, req: AdmitRequest) -> JoinHandle<Option<Admitted>> {
    let fs = fs.clone();
    tokio::spawn(async move { fs.admit(req).await })
}

async fn still_waiting(handle: &JoinHandle<Option<Admitted>>) {
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(!handle.is_finished(), "admitted past a cluster-wide limit");
}

fn req(cap: usize) -> AdmitRequest {
    AdmitRequest::new(Uuid::new_v4(), MODEL, 1).model_cap(cap)
}

/// The case that motivated shared slots: a 20-slot model behind three
/// gateways, with all 20 requests landing on one of them. Every one is
/// admitted there, and the cluster-wide limit still holds for the others.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_gateway_uses_the_whole_pool_while_the_others_are_idle() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 3, 4096, config());
    for fs in &gws {
        wait_for_mode(fs, SlotMode::Shared).await;
    }
    let mut permits = Vec::new();
    for _ in 0..20 {
        permits.push(admit_now(&gws[0], req(20)).await);
    }
    assert_eq!(cluster.pool_in_flight(&pool_id()), 20);
    assert_eq!(cluster.held("gw-0", &pool_id()), 20);

    let snap = gws[0].snapshot().await.unwrap();
    assert_eq!(snap.mode, "shared");
    let pool = &snap.pools[0];
    assert_eq!(
        (pool.cap, pool.configured_cap, pool.in_flight),
        (20, 20, 20)
    );
    assert_eq!(pool.cluster_in_flight, Some(20));

    // Full cluster-wide: the 21st waits, on this gateway and on the others.
    let here = waiter(&gws[0], req(20));
    let there = waiter(&gws[1], req(20));
    still_waiting(&here).await;
    still_waiting(&there).await;

    permits.pop();
    permits.pop();
    let a = tokio::time::timeout(Duration::from_secs(2), here).await;
    let b = tokio::time::timeout(Duration::from_secs(2), there).await;
    let a = a.expect("here admitted").unwrap().expect("admitted");
    let b = b.expect("there admitted").unwrap().expect("admitted");
    assert_eq!(cluster.pool_in_flight(&pool_id()), 20);
    assert_eq!(cluster.held("gw-1", &pool_id()), 1);
    drop((a, b, permits));
    wait_until("every slot back", || {
        cluster.pool_in_flight(&pool_id()) == 0
    })
    .await;
}

/// Tenant and key caps are cluster-wide too: a tenant capped at 4 gets 4
/// across the fleet, not 4 per gateway, and a refusal for one tenant or key
/// does not hold up the others.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenant_and_key_caps_hold_across_gateways() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, 4096, config());
    for fs in &gws {
        wait_for_mode(fs, SlotMode::Shared).await;
    }
    let tenant = Uuid::new_v4();
    let capped = || {
        AdmitRequest::new(tenant, MODEL, 1)
            .model_cap(50)
            .tenant_cap(4)
    };
    let mut held = Vec::new();
    for i in 0..4 {
        held.push(admit_now(&gws[i % 2], capped()).await);
    }
    let over = waiter(&gws[1], capped());
    still_waiting(&over).await;
    // Another tenant still gets in: the refusal is the tenant's, not the pool's.
    let other_tenant = admit_now(&gws[1], req(50)).await;

    let other = Uuid::new_v4();
    let key = Uuid::new_v4();
    let keyed = || {
        AdmitRequest::new(other, MODEL, 1)
            .key(key, 100)
            .model_cap(50)
            .key_cap(1)
    };
    let key_held = admit_now(&gws[0], keyed()).await;
    let key_over = waiter(&gws[1], keyed());
    still_waiting(&key_over).await;

    // Freeing one of the tenant's slots admits its waiter; the key's waiter
    // still waits for its key.
    held.remove(0);
    let admitted = tokio::time::timeout(Duration::from_secs(2), over)
        .await
        .expect("tenant waiter admitted")
        .unwrap()
        .expect("admitted");
    still_waiting(&key_over).await;
    drop(key_held);
    tokio::time::timeout(Duration::from_secs(2), key_over)
        .await
        .expect("key waiter admitted")
        .unwrap()
        .expect("admitted");
    drop((admitted, other_tenant, held));
}

/// The global ceiling is cluster-wide across every pool.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_global_ceiling_holds_across_gateways_and_pools() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, 3, config());
    for fs in &gws {
        wait_for_mode(fs, SlotMode::Shared).await;
    }
    let mut held = vec![
        admit_now(&gws[0], AdmitRequest::new(Uuid::new_v4(), "a", 1)).await,
        admit_now(&gws[1], AdmitRequest::new(Uuid::new_v4(), "b", 1)).await,
        admit_now(&gws[1], AdmitRequest::new(Uuid::new_v4(), "c", 1)).await,
    ];
    assert_eq!(cluster.global_in_flight(), 3);
    let over_a = waiter(&gws[0], AdmitRequest::new(Uuid::new_v4(), "d", 1));
    let over_b = waiter(&gws[1], AdmitRequest::new(Uuid::new_v4(), "e", 1));
    still_waiting(&over_a).await;
    still_waiting(&over_b).await;
    held.pop();
    // Exactly one of the two gets the freed slot, whichever claims first.
    wait_until("one waiter admitted", || {
        over_a.is_finished() || over_b.is_finished()
    })
    .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        over_a.is_finished() ^ over_b.is_finished(),
        "one freed slot admits one request"
    );
    assert_eq!(cluster.global_in_flight(), 3);
    drop(held);
    for w in [over_a, over_b] {
        let admitted = tokio::time::timeout(Duration::from_secs(2), w)
            .await
            .expect("admitted")
            .unwrap()
            .expect("admitted");
        drop(admitted);
    }
}

/// Many gateways racing for a small pool never admit past it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn racing_gateways_never_exceed_the_pool() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 4, 4096, config());
    for fs in &gws {
        wait_for_mode(fs, SlotMode::Shared).await;
    }
    let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let live = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut tasks = Vec::new();
    for i in 0..80 {
        let fs = gws[i % gws.len()].clone();
        let (peak, live) = (peak.clone(), live.clone());
        tasks.push(tokio::spawn(async move {
            let admitted = fs.admit(req(5)).await.expect("admitted");
            let now = live.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(3)).await;
            live.fetch_sub(1, Ordering::SeqCst);
            drop(admitted);
        }));
    }
    for t in tasks {
        tokio::time::timeout(Duration::from_secs(20), t)
            .await
            .expect("every request served")
            .unwrap();
    }
    assert!(peak.load(Ordering::SeqCst) <= 5, "peak {peak:?}");
    assert_eq!(peak.load(Ordering::SeqCst), 5, "the pool was used in full");
    wait_until("every slot back", || {
        cluster.pool_in_flight(&pool_id()) == 0
    })
    .await;
}

/// A slot freed on gateway A reaches a waiter on gateway B through the
/// release wakeup, well before B's own retry timer would fire.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_release_on_one_gateway_wakes_a_waiter_on_another() {
    let cluster = InMemoryCluster::new();
    let slow_retry = SharedSlotsConfig {
        retry_min: Duration::from_secs(30),
        retry_max: Duration::from_secs(30),
        ..config()
    };
    let gws = gateways(&cluster, 2, 4096, slow_retry);
    for fs in &gws {
        wait_for_mode(fs, SlotMode::Shared).await;
    }
    let held = admit_now(&gws[0], req(1)).await;
    let b = waiter(&gws[1], req(1));
    still_waiting(&b).await;
    let freed = std::time::Instant::now();
    drop(held);
    let admitted = tokio::time::timeout(Duration::from_secs(2), b)
        .await
        .expect("woken")
        .unwrap()
        .expect("admitted");
    assert!(freed.elapsed() < Duration::from_secs(2));
    assert_eq!(admitted.admission, Admission::Queued);
    assert_eq!(cluster.held("gw-1", &pool_id()), 1);
}

/// Without any wakeup at all the jittered retry still admits the waiter.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_retry_timer_covers_a_lost_wakeup() {
    let cluster = InMemoryCluster::new();
    let gws: Vec<FairShare> = (0..2)
        .map(|i| {
            let fs = FairShare::start(
                Arc::new(StaticCapacity::new(4096)),
                FairshareAlgorithm::Weighted,
                32,
            );
            fs.enable_shared_slots(Arc::new(cluster.gateway(&format!("gw-{i}"))), config());
            fs.set_replicas(2);
            // Not subscribed: no wakeup is ever delivered.
            fs
        })
        .collect();
    for fs in &gws {
        wait_for_mode(fs, SlotMode::Shared).await;
    }
    let held = admit_now(&gws[0], req(1)).await;
    let b = waiter(&gws[1], req(1));
    still_waiting(&b).await;
    drop(held);
    tokio::time::timeout(Duration::from_secs(2), b)
        .await
        .expect("retried")
        .unwrap()
        .expect("admitted");
}

/// A crashed gateway's slots come back once the backend reclaims them, and
/// not before.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_crashed_gateways_slots_are_reclaimed() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, 4096, config());
    for fs in &gws {
        wait_for_mode(fs, SlotMode::Shared).await;
    }
    let mut on_b = Vec::new();
    for _ in 0..3 {
        on_b.push(admit_now(&gws[1], req(3)).await);
    }
    let a = waiter(&gws[0], req(3));
    still_waiting(&a).await;
    // B's process dies: its permits are never released, only reclaimed.
    std::mem::forget(on_b);
    cluster.kill("gw-1");
    tokio::time::timeout(Duration::from_secs(2), a)
        .await
        .expect("reclaimed slot admitted")
        .unwrap()
        .expect("admitted");
    assert_eq!(cluster.pool_in_flight(&pool_id()), 1);
}

/// Each gateway's periodic reconcile re-asserts what it really holds, which
/// repairs drift such as a release that never reached the backend.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconcile_repairs_drift() {
    let cluster = InMemoryCluster::new();
    let fast_reconcile = SharedSlotsConfig {
        reconcile_interval: Duration::from_millis(40),
        ..config()
    };
    let gws = gateways(&cluster, 2, 4096, fast_reconcile);
    for fs in &gws {
        wait_for_mode(fs, SlotMode::Shared).await;
    }
    let held = admit_now(&gws[0], req(4)).await;
    cluster.inject_drift("gw-0", &pool_id(), 3);
    assert_eq!(cluster.pool_in_flight(&pool_id()), 4);
    wait_until("drift repaired", || {
        cluster.pool_in_flight(&pool_id()) == 1 && cluster.held("gw-0", &pool_id()) == 1
    })
    .await;
    // The pool is usable again in full.
    let mut more = Vec::new();
    for _ in 0..3 {
        more.push(admit_now(&gws[1], req(4)).await);
    }
    drop((held, more));
}

/// With one live gateway there is no backend call on the request path:
/// admissions and releases stay local, as before shared slots.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_single_gateway_makes_no_claims() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 1, 4096, config());
    let fs = &gws[0];
    wait_for_mode(fs, SlotMode::Local).await;
    let mut permits = Vec::new();
    for _ in 0..8 {
        let admitted = admit_now(fs, req(8)).await;
        assert_eq!(admitted.admission, Admission::Fast);
        permits.push(admitted);
    }
    let over = waiter(fs, req(8));
    still_waiting(&over).await;
    permits.clear();
    tokio::time::timeout(Duration::from_secs(1), over)
        .await
        .expect("admitted locally")
        .unwrap()
        .expect("admitted");
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert_eq!(cluster.calls().acquire.load(Ordering::Relaxed), 0);
    assert_eq!(cluster.calls().release.load(Ordering::Relaxed), 0);
    assert_eq!(fs.snapshot().await.unwrap().mode, "local");
}

/// A backend outage falls back to the per-replica split, which never admits
/// past `ceil(configured / replicas)` locally; once the backend answers the
/// gateway reconciles what it admitted meanwhile and goes back to shared.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outage_falls_back_to_the_split_and_recovers() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 3, 4096, config());
    for fs in &gws {
        wait_for_mode(fs, SlotMode::Shared).await;
    }
    let fs = &gws[0];
    let first = admit_now(fs, req(20)).await;

    cluster.set_failing(true);
    // The claim fails, the gateway falls back, and the request is admitted
    // under the split instead.
    let mut permits = vec![first, admit_now(fs, req(20)).await];
    assert_eq!(fs.stats().mode(), SlotMode::Fallback);
    let snap = fs.snapshot().await.unwrap();
    assert_eq!(snap.mode, "fallback");
    assert_eq!(snap.pools[0].cap, 7, "ceil(20 / 3)");
    assert_eq!(snap.pools[0].cluster_in_flight, None);
    while permits.len() < 7 {
        permits.push(admit_now(fs, req(20)).await);
    }
    let over = waiter(fs, req(20));
    still_waiting(&over).await;

    cluster.set_failing(false);
    wait_for_mode(fs, SlotMode::Shared).await;
    // Everything admitted during the outage is now on the books, and the
    // waiter gets a cluster-wide slot.
    let _last = tokio::time::timeout(Duration::from_secs(2), over)
        .await
        .expect("admitted after recovery")
        .unwrap()
        .expect("admitted");
    assert_eq!(cluster.held("gw-0", &pool_id()), 8);
    drop(permits);
    wait_until("releases recorded", || {
        cluster.held("gw-0", &pool_id()) == 1
    })
    .await;
}

/// A replica the backend no longer counts as live (its heartbeat expired
/// while it was cut off) falls back instead of taking slots, and returns to
/// shared mode once it is live again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_expired_gateway_falls_back_until_it_is_live_again() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, 4096, config());
    for fs in &gws {
        wait_for_mode(fs, SlotMode::Shared).await;
    }
    cluster.kill("gw-0");
    let admitted = admit_now(&gws[0], req(10)).await;
    assert_eq!(gws[0].stats().mode(), SlotMode::Fallback);
    assert_eq!(cluster.held("gw-0", &pool_id()), 0);
    cluster.revive("gw-0");
    wait_for_mode(&gws[0], SlotMode::Shared).await;
    assert_eq!(cluster.held("gw-0", &pool_id()), 1);
    drop(admitted);
}

/// Replicas coming and going move a gateway between local and shared mode,
/// and its local holdings are on the books before it admits through the
/// backend.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn scaling_out_resyncs_before_sharing() {
    let cluster = InMemoryCluster::new();
    let fs = FairShare::start(
        Arc::new(StaticCapacity::new(4096)),
        FairshareAlgorithm::Weighted,
        32,
    );
    fs.enable_shared_slots(Arc::new(cluster.gateway("solo")), config());
    cluster.subscribe(&fs);
    wait_for_mode(&fs, SlotMode::Local).await;
    let mut permits = Vec::new();
    for _ in 0..5 {
        permits.push(admit_now(&fs, req(6)).await);
    }
    assert_eq!(cluster.calls().acquire.load(Ordering::Relaxed), 0);

    fs.set_replicas(2);
    wait_for_mode(&fs, SlotMode::Shared).await;
    assert_eq!(cluster.held("solo", &pool_id()), 5);
    permits.push(admit_now(&fs, req(6)).await);
    let over = waiter(&fs, req(6));
    still_waiting(&over).await;

    fs.set_replicas(1);
    wait_for_mode(&fs, SlotMode::Local).await;
    drop(permits);
    tokio::time::timeout(Duration::from_secs(1), over)
        .await
        .expect("admitted locally")
        .unwrap()
        .expect("admitted");
}
