//! The cluster-wide view: each gateway publishes its scheduler state through
//! the shared backend, and any gateway's cluster snapshot merges them all.

use std::ops::AsyncFnMut;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use obleth_config::FairshareAlgorithm;
use obleth_fairshare::view::{VIEW_MAX_KEYS_PER_POOL, VIEW_VERSION};
use obleth_fairshare::{
    AdmitRequest, Admitted, ClusterSnapshot, FairShare, InMemoryCluster, ModelPoolFairshare,
    PoolKey, ReplicaPoolView, ReplicaTenantView, ReplicaView, SharedSlotsConfig, SlotMode,
    StaticCapacity,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

const MODEL: &str = "m";

fn config() -> SharedSlotsConfig {
    SharedSlotsConfig {
        reconcile_interval: Duration::from_secs(60),
        recovery_interval: Duration::from_millis(50),
        view_interval: Duration::from_millis(20),
        ..SharedSlotsConfig::default()
    }
}

fn gateways(cluster: &InMemoryCluster, n: usize, config: SharedSlotsConfig) -> Vec<FairShare> {
    (0..n)
        .map(|i| {
            let fs = FairShare::start(
                Arc::new(StaticCapacity::new(4096)),
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

async fn wait_until<T>(what: &str, mut probe: impl AsyncFnMut() -> Option<T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(v) = probe().await {
                return v;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

async fn shared(gws: &[FairShare]) {
    for fs in gws {
        let stats = fs.stats();
        wait_until("shared mode", async || {
            (stats.mode() == SlotMode::Shared).then_some(())
        })
        .await;
    }
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

fn pool<'a>(snap: &'a ClusterSnapshot, model: &str) -> &'a ModelPoolFairshare {
    snap.pools
        .iter()
        .find(|p| p.model == model)
        .unwrap_or_else(|| panic!("pool {model} in {:?}", snap.pools))
}

/// The case behind the cluster view: one tenant holds every slot of a pool
/// spread over three gateways and has requests queued on each. Any gateway's
/// own snapshot shows a third of that against a fair share of the whole
/// pool; the cluster snapshot shows all of it, so the tenant is not below
/// its share.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_single_tenant_full_pool_reads_as_full_from_any_gateway() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 3, config());
    shared(&gws).await;
    let tenant = Uuid::new_v4();
    let req = || AdmitRequest::new(tenant, MODEL, 1).model_cap(6);
    let mut permits = Vec::new();
    for fs in &gws {
        for _ in 0..2 {
            permits.push(admit_now(fs, req()).await);
        }
    }
    let mut waiters = Vec::new();
    for (i, fs) in gws.iter().enumerate() {
        for _ in 0..=i {
            waiters.push(waiter(fs, req()));
        }
    }

    // The gateway's own view: 2 of 6 slots against a fair share of 6.
    let own = wait_until("the waiters to queue", async || {
        let s = gws[2].snapshot().await?;
        (s.pools[0].queued == 3).then_some(s)
    })
    .await;
    let mine = &own.pools[0].tenants[0];
    assert_eq!((mine.in_flight, mine.queued), (2, 3));
    assert!((mine.weight_share * own.pools[0].cap as f64 - 6.0).abs() < 1e-9);

    let snap = wait_until("every gateway's view", async || {
        let s = gws[2].cluster_snapshot().await.ok()?;
        let p = s.pools.iter().find(|p| p.model == MODEL)?;
        (s.replicas_reporting == 3 && p.queued == 6).then_some(s)
    })
    .await;
    assert_eq!(snap.replicas_live, 3);
    let p = pool(&snap, MODEL);
    assert_eq!(
        (p.cap, p.configured_cap, p.in_flight, p.queued),
        (6, 6, 6, 6)
    );
    assert_eq!(p.cluster_in_flight, None);
    assert_eq!(p.tenants.len(), 1);
    let t = &p.tenants[0];
    assert_eq!((t.tenant_id, t.in_flight, t.queued), (tenant, 6, 6));
    let expected = t.weight_share * p.cap as f64;
    assert!(
        (expected - 6.0).abs() < 1e-9,
        "fair share is the whole pool"
    );
    assert!(
        t.in_flight as f64 >= expected.floor(),
        "holding its whole share, it is not waiting below it"
    );
    assert_eq!(p.groups.len(), 1);
    assert_eq!((p.groups[0].in_flight, p.groups[0].queued), (6, 6));
    assert_eq!(p.keys.len(), 1);
    assert_eq!((p.keys[0].in_flight, p.keys[0].queued), (6, 6));

    // Every gateway answers the same.
    for fs in &gws[..2] {
        let other = wait_until("the same view elsewhere", async || {
            let s = fs.cluster_snapshot().await.ok()?;
            let ready = s.pools.iter().any(|p| p.model == MODEL && p.queued == 6);
            (s.replicas_reporting == 3 && ready).then_some(s)
        })
        .await;
        let p = pool(&other, MODEL);
        assert_eq!((p.in_flight, p.tenants[0].in_flight), (6, 6));
    }
    for w in &waiters {
        w.abort();
    }
    drop(permits);
}

/// Two tenants on different gateways: each gateway alone sees one tenant
/// holding the whole pool's share; merged, the share is split between them
/// and the one that only waits is below it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shares_are_recomputed_over_every_gateways_tenants() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, config());
    shared(&gws).await;
    let (busy, starved) = (Uuid::new_v4(), Uuid::new_v4());
    let mut permits = Vec::new();
    for _ in 0..4 {
        permits.push(admit_now(&gws[0], AdmitRequest::new(busy, MODEL, 1).model_cap(4)).await);
    }
    let waiting = waiter(&gws[1], AdmitRequest::new(starved, MODEL, 1).model_cap(4));

    // Each gateway publishes its view on a timer, so a snapshot can hold one
    // taken between two of the busy tenant's admits. Wait for the counts the
    // assertions read, not only for both tenants to appear.
    let snap = wait_until("both gateways' tenants", async || {
        let s = gws[1].cluster_snapshot().await.ok()?;
        let ready = s.pools.iter().any(|p| {
            let counts = |id: Uuid| {
                p.tenants
                    .iter()
                    .find(|t| t.tenant_id == id)
                    .map(|t| (t.in_flight, t.queued))
            };
            p.model == MODEL
                && p.tenants.len() == 2
                && counts(busy) == Some((4, 0))
                && counts(starved) == Some((0, 1))
        });
        (s.replicas_reporting == 2 && ready).then_some(s)
    })
    .await;
    let p = pool(&snap, MODEL);
    let row = |id: Uuid| p.tenants.iter().find(|t| t.tenant_id == id).unwrap();
    assert!((row(busy).weight_share - 0.5).abs() < 1e-9);
    assert!((row(starved).weight_share - 0.5).abs() < 1e-9);
    assert_eq!((row(busy).in_flight, row(busy).queued), (4, 0));
    assert_eq!((row(starved).in_flight, row(starved).queued), (0, 1));
    let expected = row(starved).weight_share * p.cap as f64;
    assert!(
        (row(starved).in_flight as f64) < expected.floor(),
        "the waiting tenant is below its share"
    );
    waiting.abort();
    drop(permits);
}

/// A gateway that stops heartbeating drops out of the view at once, even
/// though its last view has not expired.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dead_gateway_drops_out_of_the_view() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, config());
    shared(&gws).await;
    let _held = admit_now(&gws[1], AdmitRequest::new(Uuid::new_v4(), MODEL, 1)).await;
    wait_until("gw-1's view", async || {
        let s = gws[0].cluster_snapshot().await.ok()?;
        (s.replicas_reporting == 2).then_some(())
    })
    .await;
    cluster.kill("gw-1");
    let snap = gws[0].cluster_snapshot().await.expect("backend answers");
    assert_eq!((snap.replicas_live, snap.replicas_reporting), (1, 1));
    assert!(
        snap.pools.iter().all(|p| p.in_flight == 0),
        "{:?}",
        snap.pools
    );
}

/// A live gateway with nothing published yet is counted live but not
/// reporting, so a reader can tell the view is partial.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_gateway_without_a_view_is_live_but_not_reporting() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 1, config());
    gws[0].set_replicas(2);
    shared(&gws).await;
    let _quiet = cluster.gateway("quiet");
    let snap = gws[0].cluster_snapshot().await.expect("backend answers");
    assert_eq!((snap.replicas_live, snap.replicas_reporting), (2, 1));
}

/// The history sampler's cluster sample: any gateway records the whole
/// pool, where its own sample holds only its part.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_gateway_samples_the_whole_pool_for_history() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 3, config());
    shared(&gws).await;
    let tenant = Uuid::new_v4();
    let req = || AdmitRequest::new(tenant, MODEL, 1).model_cap(6);
    let mut permits = Vec::new();
    for fs in &gws {
        for _ in 0..2 {
            permits.push(admit_now(fs, req()).await);
        }
    }
    let mut waiters = Vec::new();
    for (i, fs) in gws.iter().enumerate() {
        for _ in 0..=i {
            waiters.push(waiter(fs, req()));
        }
    }

    for fs in &gws {
        let s = wait_until("every gateway's view", async || {
            let s = fs.cluster_sample().await?;
            (s.global_queued == 6).then_some(s)
        })
        .await;
        assert!(s.cluster);
        assert_eq!((s.global_in_flight, s.global_queued), (6, 6));
        let p = &s.pools[0];
        assert_eq!(
            (p.model.as_str(), p.cap, p.in_flight, p.queued),
            (MODEL, 6, 6, 6)
        );
    }
    let own = gws[0].sample().await.expect("scheduler alive");
    assert!(!own.cluster);
    assert_eq!(own.global_in_flight, 2);

    cluster.set_failing(true);
    assert!(gws[0].cluster_sample().await.is_none());
    drop(permits);
    for w in waiters {
        w.abort();
    }
}

/// With the backend down there is no cluster view: the caller falls back to
/// the gateway's own snapshot.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn no_cluster_view_while_the_backend_is_down() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, config());
    shared(&gws).await;
    cluster.set_failing(true);
    assert!(gws[0].cluster_snapshot().await.is_err());
    // Shared slots off: no cluster view either.
    let alone = FairShare::start(
        Arc::new(StaticCapacity::new(64)),
        FairshareAlgorithm::Hierarchical,
        8,
    );
    assert!(alone.cluster_snapshot().await.is_err());
}

/// Views are published only while more than one gateway is live, on the
/// configured interval, and carry the pools with work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn views_are_published_while_more_than_one_gateway_is_live() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 1, config());
    let fs = &gws[0];
    let _held = admit_now(fs, AdmitRequest::new(Uuid::new_v4(), MODEL, 1)).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(cluster.calls().publish_view.load(Ordering::Relaxed), 0);
    assert!(cluster.view("gw-0").is_none());

    fs.set_replicas(2);
    shared(&gws).await;
    let view = wait_until("a published view", async || cluster.view("gw-0")).await;
    assert_eq!(view.version, VIEW_VERSION);
    assert_eq!(view.pools.len(), 1);
    assert_eq!(view.pools[0].pool, PoolKey::Model(MODEL.into()).slot_id());
    assert_eq!(view.pools[0].in_flight, 1);
    let before = cluster.calls().publish_view.load(Ordering::Relaxed);
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after = cluster.calls().publish_view.load(Ordering::Relaxed);
    assert!(
        (3..=20).contains(&(after - before)),
        "about one publish per 20 ms interval: {}",
        after - before
    );
}

/// A pool with more active keys than a view may list keeps the busiest and
/// says how many it left out; the pool's totals stay complete.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn views_are_bounded() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 2, config());
    shared(&gws).await;
    let tenant = Uuid::new_v4();
    let n = VIEW_MAX_KEYS_PER_POOL + 10;
    let mut permits = Vec::new();
    for _ in 0..n {
        let req = AdmitRequest::new(tenant, MODEL, 1)
            .key(Uuid::new_v4(), 100)
            .model_cap(n);
        permits.push(admit_now(&gws[0], req).await);
    }
    let view = wait_until("a view of every key", async || {
        cluster
            .view("gw-0")
            .filter(|v| v.pools.first().is_some_and(|p| p.in_flight == n))
    })
    .await;
    let p = &view.pools[0];
    assert_eq!(p.keys.len(), VIEW_MAX_KEYS_PER_POOL);
    assert_eq!(p.omitted_keys, 10);
    assert_eq!(p.tenants[0].in_flight, n, "tenant totals are complete");
}

/// Pools that only another gateway has are listed, and so are this
/// gateway's idle pools.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pools_from_other_gateways_and_idle_ones_are_listed() {
    let cluster = InMemoryCluster::new();
    let gws = gateways(&cluster, 1, config());
    // Another live gateway, standing in with a view put by hand.
    let _peer = cluster.gateway("peer");
    gws[0].set_replicas(2);
    shared(&gws).await;
    drop(
        admit_now(
            &gws[0],
            AdmitRequest::new(Uuid::new_v4(), "idle-here", 1).model_cap(3),
        )
        .await,
    );
    let other = Uuid::new_v4();
    cluster.put_view(
        "peer",
        ReplicaView {
            version: VIEW_VERSION,
            pools: vec![ReplicaPoolView {
                pool: PoolKey::Model("elsewhere".into()).slot_id(),
                configured_cap: 12,
                in_flight: 5,
                queued: 2,
                tenants: vec![ReplicaTenantView {
                    id: other,
                    group: "default".into(),
                    weight: 100,
                    cap: None,
                    in_flight: 5,
                    queued: 2,
                    served: 5.0,
                }],
                ..Default::default()
            }],
        },
        Duration::from_secs(60),
    );
    let snap = gws[0].cluster_snapshot().await.expect("backend answers");
    assert_eq!((snap.replicas_live, snap.replicas_reporting), (2, 2));
    let elsewhere = pool(&snap, "elsewhere");
    assert_eq!(
        (elsewhere.cap, elsewhere.in_flight, elsewhere.queued),
        (12, 5, 2)
    );
    assert_eq!(elsewhere.tenants[0].tenant_id, other);
    assert!((elsewhere.tenants[0].weight_share - 1.0).abs() < 1e-9);
    let idle = pool(&snap, "idle-here");
    assert_eq!((idle.cap, idle.in_flight, idle.queued), (3, 0, 0));
}
