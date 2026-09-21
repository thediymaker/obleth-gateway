//! Fairness property tests for the admission scheduler.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use obleth_config::{Admission, FairshareAlgorithm};
use obleth_fairshare::{AdmitRequest, FairShare, StaticCapacity};
use uuid::Uuid;

#[tokio::test]
async fn fast_path_when_idle() {
    let cap = Arc::new(StaticCapacity::new(8));
    let fs = FairShare::start(cap, FairshareAlgorithm::Weighted, 8);
    let admitted = fs
        .admit(AdmitRequest::weighted(Uuid::new_v4(), 1, 10))
        .await
        .expect("admit");
    assert_eq!(admitted.admission, Admission::Fast);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saturated_model_does_not_block_other_models() {
    let cap = Arc::new(StaticCapacity::new(4));
    let fs = FairShare::start(cap, FairshareAlgorithm::Weighted, 4);
    let slow_tenant = Uuid::new_v4();
    let fast_tenant = Uuid::new_v4();

    let slow = |tenant| {
        AdmitRequest::new(tenant, "slow", 10)
            .weight(100)
            .group("default", 100)
            .model_cap(1)
    };
    let fast = |tenant| {
        AdmitRequest::new(tenant, "fast", 10)
            .weight(100)
            .group("default", 100)
            .model_cap(4)
    };

    let slow_permit = fs
        .admit(slow(slow_tenant))
        .await
        .expect("first slow admit")
        .permit;

    let fs_slow = fs.clone();
    let slow_waiter = tokio::spawn(async move { fs_slow.admit(slow(slow_tenant)).await });
    tokio::time::sleep(Duration::from_millis(30)).await;

    let fast_admitted = tokio::time::timeout(Duration::from_secs(1), fs.admit(fast(fast_tenant)))
        .await
        .expect("fast model should not wait on slow cap")
        .expect("fast admit");

    assert!(
        matches!(fast_admitted.admission, Admission::Fast | Admission::Queued),
        "fast model should be admitted while slow model is capped"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(50), slow_waiter)
            .await
            .is_err(),
        "second slow request should still be capped"
    );

    drop(fast_admitted.permit);
    drop(slow_permit);
}

/// Under contention (capacity = 1), a tenant with 3x the weight should win the
/// majority of the early grants. We assert the boosted tenant is served at least
/// twice as often as the baseline within the first window of dispatches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn weighted_share_under_contention() {
    let cap = Arc::new(StaticCapacity::new(1));
    let fs = FairShare::start(cap, FairshareAlgorithm::Weighted, 1);

    let low = Uuid::new_v4();
    let high = Uuid::new_v4();
    let order: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    let mut handles = Vec::new();
    for _ in 0..40 {
        for (tag, tenant, weight) in [(0u8, low, 1i64), (1u8, high, 3i64)] {
            let fs = fs.clone();
            let order = order.clone();
            handles.push(tokio::spawn(async move {
                if let Some(adm) = fs.admit(AdmitRequest::weighted(tenant, weight, 10)).await {
                    order.lock().unwrap().push(tag);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    drop(adm.permit);
                }
            }));
        }
    }
    for h in handles {
        let _ = h.await;
    }

    let order = order.lock().unwrap();
    let window = 40.min(order.len());
    let high_count = order[..window].iter().filter(|&&t| t == 1).count();
    let low_count = order[..window].iter().filter(|&&t| t == 0).count();

    assert!(
        high_count >= low_count * 2,
        "expected boosted tenant to dominate early grants: high={high_count} low={low_count}"
    );
}

/// Within a single hierarchical group, a tenant with 3x the weight of a peer
/// should win the majority of grants. This guards the "bump one user inside a
/// crowded group" tuning workflow — weight must matter even when the group, not
/// the global pool, is the unit of capacity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hierarchical_higher_weight_tenant_wins_within_group() {
    let cap = Arc::new(StaticCapacity::new(8));
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical, 8);

    let low = Uuid::new_v4();
    let high = Uuid::new_v4();
    let order: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    let request = |tenant, weight| {
        AdmitRequest::new(tenant, "general-model", 10)
            .weight(weight)
            .group("general", 100)
    };

    let mut handles = Vec::new();
    for _ in 0..60 {
        for (tag, tenant, weight) in [(0u8, low, 1i64), (1u8, high, 3i64)] {
            let fs = fs.clone();
            let order = order.clone();
            handles.push(tokio::spawn(async move {
                if let Some(adm) = fs.admit(request(tenant, weight)).await {
                    order.lock().unwrap().push(tag);
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    drop(adm.permit);
                }
            }));
        }
    }
    for h in handles {
        let _ = h.await;
    }

    let order = order.lock().unwrap();
    let window = 48.min(order.len());
    let high_count = order[..window].iter().filter(|&&t| t == 1).count();
    let low_count = order[..window].iter().filter(|&&t| t == 0).count();

    assert!(
        high_count >= low_count * 2,
        "boosted tenant should dominate inside the group: high={high_count} low={low_count}"
    );
}

/// With cap=8 and groups 500:50, the low-priority group keeps a reserved slot
/// even when the high-priority group saturates global capacity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hierarchical_group_gets_reserved_slot() {
    let cap = Arc::new(StaticCapacity::new(8));
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical, 8);

    let chatbot = Uuid::new_v4();
    let api = Uuid::new_v4();

    let mut permits = Vec::new();
    for _ in 0..8 {
        let adm = fs
            .admit(
                AdmitRequest::new(chatbot, "chat-model", 10)
                    .weight(500)
                    .group("chatbot", 500),
            )
            .await
            .expect("chatbot admit");
        permits.push(adm.permit);
    }

    let fs2 = fs.clone();
    let api_handle = tokio::spawn(async move {
        fs2.admit(
            AdmitRequest::new(api, "api-model", 10)
                .weight(50)
                .group("api", 50),
        )
        .await
    });

    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(permits.pop());

    let api_admitted = tokio::time::timeout(Duration::from_secs(1), api_handle)
        .await
        .expect("timeout")
        .expect("join")
        .expect("api should be admitted once a slot frees");

    assert!(
        matches!(api_admitted.admission, Admission::Fast | Admission::Queued),
        "api group should receive a slot under hierarchical fairshare"
    );
}

/// Once multiple tenants in the same hierarchical group are contending, the
/// group's slot pool should not be handed entirely to whichever tenant happens
/// to have the lowest historical served score. Already-running requests are not
/// preempted, but newly freed slots should respect the group's tenant split.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hierarchical_group_slots_split_across_tenants() {
    let cap = Arc::new(StaticCapacity::new(8));
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical, 8);

    let chatbot = Uuid::new_v4();
    let chatbot2 = Uuid::new_v4();

    let request = |tenant| {
        AdmitRequest::new(tenant, "chat-model", 10)
            .weight(500)
            .group("chatbot", 500)
    };

    let mut chatbot_permits = Vec::new();
    for _ in 0..8 {
        chatbot_permits.push(fs.admit(request(chatbot)).await.expect("admit").permit);
    }

    let mut chatbot2_handles = Vec::new();
    for _ in 0..4 {
        let fs2 = fs.clone();
        chatbot2_handles.push(tokio::spawn(
            async move { fs2.admit(request(chatbot2)).await },
        ));
    }

    tokio::time::sleep(Duration::from_millis(30)).await;
    for _ in 0..4 {
        drop(chatbot_permits.pop());
    }

    let mut chatbot2_permits = Vec::new();
    for handle in chatbot2_handles {
        chatbot2_permits.push(
            tokio::time::timeout(Duration::from_secs(1), handle)
                .await
                .expect("chatbot-2 timeout")
                .expect("join")
                .expect("chatbot-2 admit")
                .permit,
        );
    }

    let fs_chatbot = fs.clone();
    let chatbot_handle = tokio::spawn(async move { fs_chatbot.admit(request(chatbot)).await });
    let fs_chatbot2 = fs.clone();
    let chatbot2_handle = tokio::spawn(async move { fs_chatbot2.admit(request(chatbot2)).await });

    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(chatbot_permits.pop());

    let admitted = tokio::time::timeout(Duration::from_secs(1), chatbot_handle)
        .await
        .expect("chatbot timeout")
        .expect("join")
        .expect("chatbot should receive the next slot");

    assert!(
        tokio::time::timeout(Duration::from_millis(50), chatbot2_handle)
            .await
            .is_err(),
        "chatbot-2 was already at its half of the group pool"
    );

    drop(admitted.permit);
    drop(chatbot2_permits);
    drop(chatbot_permits);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn snapshot_reports_per_model_queued() {
    let cap = Arc::new(StaticCapacity::new(1));
    let fs = FairShare::start(cap, FairshareAlgorithm::Weighted, 1);
    let tenant = Uuid::new_v4();

    let req = move |model: &str| {
        AdmitRequest::new(tenant, model, 10)
            .weight(100)
            .group("default", 100)
            .model_cap(1)
    };

    // Fill the single global slot with model "alpha".
    let held = fs.admit(req("alpha")).await.expect("first admit").permit;

    // Two more requests must queue (global capacity is full): one more "alpha"
    // and one "beta".
    let fs2 = fs.clone();
    let w1 = tokio::spawn(async move { fs2.admit(req("alpha")).await });
    let fs3 = fs.clone();
    let w2 = tokio::spawn(async move { fs3.admit(req("beta")).await });
    tokio::time::sleep(Duration::from_millis(40)).await;

    let snap = fs.snapshot().await.expect("snapshot");
    assert_eq!(snap.model_in_flight.get("alpha").copied(), Some(1));
    assert_eq!(snap.model_queued.get("alpha").copied(), Some(1));
    assert_eq!(snap.model_queued.get("beta").copied(), Some(1));

    drop(held);
    let _ = tokio::time::timeout(Duration::from_secs(1), w1).await;
    let _ = tokio::time::timeout(Duration::from_secs(1), w2).await;
}

/// `share_score` is the paper's Equation 1, `served_tokens / weight`, and the
/// Management API documents it as such. It must not vary by scheduler mode:
/// hierarchical admission ranks tenants inside the winning group on exactly
/// that weight-adjusted value, so reporting the raw token count under the same
/// name misrepresents the key the scheduler actually uses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tenant_share_score_is_weight_adjusted_in_both_modes() {
    for algorithm in [
        FairshareAlgorithm::Hierarchical,
        FairshareAlgorithm::Weighted,
    ] {
        let cap = Arc::new(StaticCapacity::new(8));
        let fs = FairShare::start(cap, algorithm, 8);
        let heavy = Uuid::new_v4();
        let light = Uuid::new_v4();

        let req = |tenant: Uuid, weight: i64, cost: u32| {
            AdmitRequest::new(tenant, "m", cost)
                .weight(weight)
                .group("shared", 100)
        };

        // Same 1000 tokens of service, weights 500 and 50.
        let a = fs.admit(req(heavy, 500, 1000)).await.expect("heavy admit");
        let b = fs.admit(req(light, 50, 1000)).await.expect("light admit");

        let snap = fs.snapshot().await.expect("snapshot");
        let score = |id: Uuid| {
            snap.pools[0]
                .tenants
                .iter()
                .find(|t| t.tenant_id == id)
                .map(|t| t.share_score)
                .expect("tenant in snapshot")
        };

        assert!(
            (score(heavy) - 2.0).abs() < 1e-9,
            "{algorithm:?}: heavy tenant 1000/500 should score 2.0, got {}",
            score(heavy)
        );
        assert!(
            (score(light) - 20.0).abs() < 1e-9,
            "{algorithm:?}: light tenant 1000/50 should score 20.0, got {}",
            score(light)
        );

        drop(a);
        drop(b);
    }
}

/// A group apportioned fewer slots than it has active tenants caps the surplus
/// tenants at zero, and the dispatch guard `in_flight >= tenant_cap` excludes
/// them at 0 >= 0. That exclusion must stay transient: caps are recomputed on
/// every dispatch against the currently-active set, so a zero-capped tenant
/// rotates back in rather than being shut out for the run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn group_with_more_tenants_than_slots_still_serves_every_tenant() {
    let cap = Arc::new(StaticCapacity::new(1));
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical, 1);
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();

    let req = |tenant: Uuid| {
        AdmitRequest::new(tenant, "m", 10)
            .weight(100)
            .group("shared", 100)
    };

    // Occupy the only slot so every later request must queue.
    let held = fs.admit(req(a)).await.expect("seed admit").permit;

    let seen: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));
    let mut workers = Vec::new();
    for tenant in [a, b, a, b, a, b, a, b] {
        let fs = fs.clone();
        let seen = seen.clone();
        workers.push(tokio::spawn(async move {
            if let Some(ok) = fs.admit(req(tenant)).await {
                seen.lock().unwrap().push(tenant);
                tokio::time::sleep(Duration::from_millis(15)).await;
                drop(ok.permit);
            }
        }));
    }

    tokio::time::sleep(Duration::from_millis(60)).await;
    drop(held);
    for w in workers {
        let _ = tokio::time::timeout(Duration::from_millis(800), w).await;
    }

    let got = seen.lock().unwrap().clone();
    let a_count = got.iter().filter(|t| **t == a).count();
    let b_count = got.iter().filter(|t| **t == b).count();
    // Service is bursty rather than strictly interleaved (observed BBBAABAA),
    // because the zero-capped tenant only becomes eligible once the active set
    // changes. Both tenants must nonetheless be fully served.
    assert_eq!(a_count, 4, "tenant A short-served: {got:?}");
    assert_eq!(b_count, 4, "tenant B short-served: {got:?}");
}

/// Group caps are ceilings on *contended* demand, not reservations: a
/// backlogged group borrows slots a sibling group is leaving idle, so the
/// scheduler is work-conserving.
///
/// Here the high-weight group is apportioned 7 of 8 slots but holds only 1.
/// The low-weight group, capped at 1, must be lent the rest rather than
/// queueing behind six idle slots.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn hierarchical_backlogged_group_borrows_idle_sibling_slots() {
    let cap = Arc::new(StaticCapacity::new(8));
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical, 8);
    let big = Uuid::new_v4();
    let small = Uuid::new_v4();

    let req = |tenant: Uuid, group: &'static str, gw: i64| {
        AdmitRequest::new(tenant, "m", 10)
            .weight(100)
            .group(group, gw)
    };

    // High-weight group: active, but holding only 1 of the ~7 slots it is due.
    let _big = fs
        .admit(req(big, "big", 500))
        .await
        .expect("big admit")
        .permit;

    // Low-weight group floods with 10 requests.
    let mut waiters = Vec::new();
    for _ in 0..10 {
        let fs = fs.clone();
        waiters.push(tokio::spawn(async move {
            if let Some(a) = fs.admit(req(small, "small", 50)).await {
                tokio::time::sleep(Duration::from_secs(5)).await;
                drop(a.permit);
            }
        }));
    }
    tokio::time::sleep(Duration::from_millis(120)).await;

    let snap = fs.snapshot().await.expect("snapshot");
    let big_g = snap.pools[0]
        .groups
        .iter()
        .find(|g| g.name == "big")
        .expect("big");
    let small_g = snap.pools[0]
        .groups
        .iter()
        .find(|g| g.name == "small")
        .expect("small");

    assert_eq!(big_g.slot_cap, 7, "high-weight group is due 7 of 8 slots");
    assert_eq!(big_g.in_flight, 1, "but is only using one of them");
    assert_eq!(
        snap.global_in_flight, 8,
        "every slot must be working: {} still queued",
        snap.global_queued
    );
    assert_eq!(
        small_g.in_flight, 7,
        "low-weight group borrows the six idle slots on top of its cap of 1"
    );
    // Borrowed occupancy is reported separately so a run can show that lending
    // fired, rather than leaving it to be inferred from cap vs in_flight.
    assert_eq!(small_g.borrowed, 6, "6 of small's 7 slots are borrowed");
    assert_eq!(big_g.borrowed, 0, "the lender borrows nothing");
    assert_eq!(snap.global_borrowed, 6);

    for w in waiters {
        w.abort();
    }
}

/// A group that has borrowed heavily must yield as its lender's demand
/// returns. Borrowed slots are not preemptible — permits are held for the whole
/// stream — so the lender reclaims as borrowed streams complete, and crucially
/// the borrower must not keep winning fresh slots ahead of the waiting lender.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lender_reclaims_borrowed_slots_as_they_drain() {
    let cap = Arc::new(StaticCapacity::new(8));
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical, 8);
    let big = Uuid::new_v4();
    let small = Uuid::new_v4();

    let req = |tenant: Uuid, group: &'static str, gw: i64| {
        AdmitRequest::new(tenant, "m", 10)
            .weight(100)
            .group(group, gw)
    };

    // The low-weight group arrives first and borrows the whole pool.
    let mut borrowed = Vec::new();
    for _ in 0..8 {
        borrowed.push(
            fs.admit(req(small, "small", 50))
                .await
                .expect("small admit")
                .permit,
        );
    }
    let snap = fs.snapshot().await.expect("snapshot");
    assert_eq!(snap.global_in_flight, 8, "small should hold the whole pool");

    // The lender's demand returns: queue the high-weight group, plus more
    // low-weight work that must NOT jump ahead of it.
    let fs_big = fs.clone();
    let big_wait =
        tokio::spawn(async move { fs_big.admit(req(big, "big", 500)).await.map(|a| a.permit) });
    let mut small_more = Vec::new();
    for _ in 0..4 {
        let fs = fs.clone();
        small_more.push(tokio::spawn(async move {
            fs.admit(req(small, "small", 50)).await.map(|a| a.permit)
        }));
    }
    tokio::time::sleep(Duration::from_millis(40)).await;

    // Free exactly one borrowed slot. It must go to the lender, not the
    // borrower that is already far above its cap.
    drop(borrowed.pop());

    let reclaimed = tokio::time::timeout(Duration::from_secs(2), big_wait)
        .await
        .expect("lender must not wait indefinitely")
        .expect("join")
        .expect("lender admitted");

    let snap = fs.snapshot().await.expect("snapshot");
    let big_g = snap.pools[0]
        .groups
        .iter()
        .find(|g| g.name == "big")
        .expect("big");
    assert_eq!(big_g.in_flight, 1, "the freed slot went to the lender");

    drop(reclaimed);
    for w in small_more {
        w.abort();
    }
    drop(borrowed);
}

/// Borrowed service is still service: a tenant that borrows idle capacity
/// accrues `served_tokens` for it, so timing luck does not earn free share.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn borrowed_slots_accrue_served_tokens() {
    let cap = Arc::new(StaticCapacity::new(8));
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical, 8);
    let big = Uuid::new_v4();
    let small = Uuid::new_v4();

    let req = |tenant: Uuid, group: &'static str, gw: i64, cost: u32| {
        AdmitRequest::new(tenant, "m", cost)
            .weight(100)
            .group(group, gw)
    };

    let _big = fs
        .admit(req(big, "big", 500, 10))
        .await
        .expect("big admit")
        .permit;

    // Spawned, not awaited in sequence: before borrowing exists these block
    // forever at small's cap of 1, and the test must fail rather than hang.
    let mut waiters = Vec::new();
    for _ in 0..7 {
        let fs = fs.clone();
        waiters.push(tokio::spawn(async move {
            if let Some(a) = fs.admit(req(small, "small", 50, 100)).await {
                tokio::time::sleep(Duration::from_secs(5)).await;
                drop(a.permit);
            }
        }));
    }
    tokio::time::sleep(Duration::from_millis(150)).await;

    let snap = fs.snapshot().await.expect("snapshot");
    let small_t = snap.pools[0]
        .tenants
        .iter()
        .find(|t| t.tenant_id == small)
        .expect("small tenant");
    assert_eq!(
        small_t.served_tokens, 700.0,
        "7 borrowed admissions at cost 100 must all be charged"
    );

    for w in waiters {
        w.abort();
    }
}

/// Two pools never share slots: a saturated model must not delay a model with
/// free capacity, and tokens served on one model must not change a tenant's
/// standing on another.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn pools_are_isolated_per_model() {
    let fs = FairShare::start(
        Arc::new(StaticCapacity::new(64)),
        FairshareAlgorithm::Weighted,
        32,
    );
    let heavy_user = Uuid::new_v4();
    let light_user = Uuid::new_v4();

    // Saturate "heavy" (cap 1) with heavy_user and queue a second heavy request.
    let held = fs
        .admit(AdmitRequest::new(heavy_user, "heavy", 1_000).model_cap(1))
        .await
        .unwrap()
        .permit;
    let fs2 = fs.clone();
    let waiter = tokio::spawn(async move {
        fs2.admit(AdmitRequest::new(heavy_user, "heavy", 1_000).model_cap(1))
            .await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;

    // "fast" has its own pool: admitted immediately, Fast, despite the heavy backlog.
    let fast = tokio::time::timeout(
        Duration::from_secs(1),
        fs.admit(AdmitRequest::new(light_user, "fast", 10).model_cap(4)),
    )
    .await
    .expect("fast pool must not wait on heavy")
    .unwrap();
    assert_eq!(fast.admission, Admission::Fast);

    // heavy_user's 1,000 served tokens on "heavy" do not count against it on "fast".
    let snap = fs.snapshot().await.unwrap();
    let fast_pool = snap
        .pools
        .iter()
        .find(|p| p.model == "fast")
        .expect("fast pool in snapshot");
    let heavy_on_fast = fast_pool.tenants.iter().find(|t| t.tenant_id == heavy_user);
    assert!(
        heavy_on_fast.is_none(),
        "heavy_user has no state in the fast pool"
    );
    assert_eq!(fast_pool.cap, 4);

    drop(fast.permit);
    drop(held);
    let _ = tokio::time::timeout(Duration::from_secs(1), waiter).await;
}

#[tokio::test]
async fn model_without_cap_gets_the_default_pool_size() {
    let fs = FairShare::start(
        Arc::new(StaticCapacity::new(1024)),
        FairshareAlgorithm::Weighted,
        7,
    );
    let _p = fs
        .admit(AdmitRequest::new(Uuid::new_v4(), "uncapped", 1))
        .await
        .unwrap()
        .permit;
    let snap = fs.snapshot().await.unwrap();
    assert_eq!(snap.default_model_max_in_flight, 7);
    assert_eq!(
        snap.pools
            .iter()
            .find(|p| p.model == "uncapped")
            .unwrap()
            .cap,
        7
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn global_ceiling_still_binds_across_pools() {
    // Two pools of 4 each, but only 2 slots in total.
    let fs = FairShare::start(
        Arc::new(StaticCapacity::new(2)),
        FairshareAlgorithm::Weighted,
        4,
    );
    let t = Uuid::new_v4();
    let a = fs.admit(AdmitRequest::new(t, "a", 1)).await.unwrap();
    let b = fs.admit(AdmitRequest::new(t, "b", 1)).await.unwrap();
    assert_eq!(a.admission, Admission::Fast);
    assert_eq!(b.admission, Admission::Fast);
    let fs2 = fs.clone();
    let third = tokio::spawn(async move { fs2.admit(AdmitRequest::new(t, "a", 1)).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let snap = fs.snapshot().await.unwrap();
    assert_eq!(snap.global_in_flight, 2);
    assert_eq!(snap.global_queued, 1);
    drop(b.permit);
    let admitted = tokio::time::timeout(Duration::from_secs(1), third)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(admitted.admission, Admission::Queued);
    drop(admitted.permit);
    drop(a.permit);
}

/// Inside one tenant, a key with double weight is served at least twice as
/// often as its sibling while both are backlogged.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn keys_inside_a_tenant_share_by_key_weight() {
    let fs = FairShare::start(
        Arc::new(StaticCapacity::new(1)),
        FairshareAlgorithm::Weighted,
        1,
    );
    let tenant = Uuid::new_v4();
    let alice = Uuid::new_v4(); // weight 100
    let bob = Uuid::new_v4(); // weight 200
    let grants: Arc<Mutex<Vec<Uuid>>> = Arc::new(Mutex::new(Vec::new()));

    // Hold the single slot so everything below queues.
    let held = fs
        .admit(AdmitRequest::new(tenant, "m", 10))
        .await
        .unwrap()
        .permit;

    let mut handles = Vec::new();
    for _ in 0..6 {
        for (key, w) in [(alice, 100), (bob, 200)] {
            let fs = fs.clone();
            let grants = grants.clone();
            handles.push(tokio::spawn(async move {
                let a = fs
                    .admit(AdmitRequest::new(tenant, "m", 10).key(key, w))
                    .await
                    .unwrap();
                grants.lock().unwrap().push(key);
                tokio::time::sleep(Duration::from_millis(5)).await;
                drop(a.permit);
            }));
        }
    }
    // Give all twelve spawned admits time to enqueue before the held permit
    // drops, so the window below reflects real WFQ ordering rather than
    // scheduling luck on which admits made it into the queue first.
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(held);
    for h in handles {
        let _ = tokio::time::timeout(Duration::from_secs(5), h).await;
    }
    let order = grants.lock().unwrap().clone();
    let first_nine = &order[..9];
    let bob_first = first_nine.iter().filter(|k| **k == bob).count();
    let alice_first = first_nine.iter().filter(|k| **k == alice).count();
    assert!(
        bob_first >= 2 * alice_first,
        "bob (w200) {bob_first} vs alice (w100) {alice_first} in first nine grants"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn key_cap_and_tenant_cap_are_enforced_without_blocking_siblings() {
    let fs = FairShare::start(
        Arc::new(StaticCapacity::new(8)),
        FairshareAlgorithm::Weighted,
        8,
    );
    let capped_tenant = Uuid::new_v4();
    let capped_key = Uuid::new_v4();
    let other_tenant = Uuid::new_v4();

    // Key cap 1: second request from the same key must queue even with 7 free slots.
    let k1 = fs
        .admit(
            AdmitRequest::new(capped_tenant, "m", 1)
                .key(capped_key, 100)
                .key_cap(1),
        )
        .await
        .unwrap();
    assert_eq!(k1.admission, Admission::Fast);
    let fs2 = fs.clone();
    let k2 = tokio::spawn(async move {
        fs2.admit(
            AdmitRequest::new(capped_tenant, "m", 1)
                .key(capped_key, 100)
                .key_cap(1),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(fs.snapshot().await.unwrap().global_queued, 1);

    // A sibling tenant is not blocked by the capped key's backlog.
    let other = tokio::time::timeout(
        Duration::from_secs(1),
        fs.admit(AdmitRequest::new(other_tenant, "m", 1)),
    )
    .await
    .expect("sibling must not wait")
    .unwrap();
    assert_eq!(other.admission, Admission::Queued);

    // Tenant cap 2 (per model): a third in-flight request for the tenant queues.
    let t1 = fs
        .admit(
            AdmitRequest::new(capped_tenant, "m", 1)
                .key(Uuid::new_v4(), 100)
                .tenant_cap(2),
        )
        .await
        .unwrap();
    let fs3 = fs.clone();
    let t3 = tokio::spawn(async move {
        fs3.admit(
            AdmitRequest::new(capped_tenant, "m", 1)
                .key(Uuid::new_v4(), 100)
                .tenant_cap(2),
        )
        .await
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let snap = fs.snapshot().await.unwrap();
    let pool = &snap.pools[0];
    let ct = pool
        .tenants
        .iter()
        .find(|t| t.tenant_id == capped_tenant)
        .unwrap();
    assert_eq!(ct.in_flight, 2);
    assert_eq!(ct.max_in_flight, Some(2));
    assert!(pool
        .keys
        .iter()
        .any(|k| k.key_id == capped_key && k.max_in_flight == Some(1) && k.queued == 1));

    // Free both the capped key and one tenant slot so k2 and t3 can both land
    // regardless of which the scheduler picks first.
    drop(k1.permit);
    drop(t1.permit);
    let k2 = tokio::time::timeout(Duration::from_secs(1), k2)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let t3 = tokio::time::timeout(Duration::from_secs(1), t3)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    drop(k2.permit);
    drop(t3.permit);
    drop(other.permit);
}

#[tokio::test]
async fn idle_pool_forgets_its_history() {
    let fs = FairShare::start(
        Arc::new(StaticCapacity::new(4)),
        FairshareAlgorithm::Weighted,
        4,
    );
    let t = Uuid::new_v4();
    let p = fs
        .admit(AdmitRequest::new(t, "m", 500))
        .await
        .unwrap()
        .permit;
    assert_eq!(fs.snapshot().await.unwrap().pools[0].tenants.len(), 1);
    drop(p);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let snap = fs.snapshot().await.unwrap();
    assert!(
        snap.pools[0].tenants.is_empty(),
        "idle pool keeps no tenant state"
    );
    assert!(snap.pools[0].keys.is_empty());
    assert_eq!(snap.pools[0].cap, 4);
}

/// A tenant that goes idle carrying no debt (served at or below the pool's
/// virtual time) is dropped from the pool's share tree on release, so a tenant
/// that has gone quiet -- or has been deleted -- stops diluting the live
/// weight shares of the tenants still running.
#[tokio::test]
async fn released_idle_tenant_is_pruned_from_the_pool() {
    let fs = FairShare::start(
        Arc::new(StaticCapacity::new(4)),
        FairshareAlgorithm::Weighted,
        4,
    );
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    // Cost 0 leaves both tenants exactly at the pool's virtual time, which is
    // the condition under which a released tenant carries nothing worth
    // remembering.
    let held_a = fs.admit(AdmitRequest::new(a, "m", 0)).await.unwrap().permit;
    let held_b = fs.admit(AdmitRequest::new(b, "m", 0)).await.unwrap().permit;
    assert_eq!(fs.snapshot().await.unwrap().pools[0].tenants.len(), 2);

    drop(held_a);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let snap = fs.snapshot().await.unwrap();
    let tenants = &snap.pools[0].tenants;
    assert_eq!(tenants.len(), 1, "the released tenant is forgotten");
    assert_eq!(tenants[0].tenant_id, b);
    assert!(
        (tenants[0].weight_share - 1.0).abs() < 1e-9,
        "the tenant still running holds the whole pool share, not half of it"
    );
    assert!(snap.pools[0].keys.iter().all(|k| k.tenant_id == b));

    drop(held_b);
    tokio::time::sleep(Duration::from_millis(20)).await;
    let snap = fs.snapshot().await.unwrap();
    assert!(snap.pools[0].tenants.is_empty());
    assert!(snap.pools[0].keys.is_empty());
    assert_eq!(snap.pools[0].cap, 4);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn snapshot_reports_keys_with_tenant_relative_share() {
    let fs = FairShare::start(
        Arc::new(StaticCapacity::new(1)),
        FairshareAlgorithm::Weighted,
        1,
    );
    let t = Uuid::new_v4();
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let held = fs
        .admit(AdmitRequest::new(t, "m", 10).key(a, 100))
        .await
        .unwrap()
        .permit;
    let fs2 = fs.clone();
    let w = tokio::spawn(async move { fs2.admit(AdmitRequest::new(t, "m", 10).key(b, 300)).await });
    tokio::time::sleep(Duration::from_millis(30)).await;
    let snap = fs.snapshot().await.unwrap();
    let keys = &snap.pools[0].keys;
    let ka = keys.iter().find(|k| k.key_id == a).unwrap();
    let kb = keys.iter().find(|k| k.key_id == b).unwrap();
    assert_eq!((ka.in_flight, ka.queued), (1, 0));
    assert_eq!((kb.in_flight, kb.queued), (0, 1));
    // Only one tenant, so its weight_share is 1.0; keys split it 100:300.
    assert!((ka.weight_share - 0.25).abs() < 1e-9);
    assert!((kb.weight_share - 0.75).abs() < 1e-9);
    assert_eq!(snap.model_queued.get("m").copied(), Some(1));
    drop(held);
    let _ = tokio::time::timeout(Duration::from_secs(1), w).await;
}

/// A queued caller that gives up must still release its granted slot exactly
/// once: `Pool::send`'s `Err(Admitted)` return already drops that `Admitted`'s
/// `Permit`, which releases via `Permit::drop`. An extra explicit release on
/// top of that doesn't just under-count the abandoned grant -- because both
/// counters floor at zero via `saturating_sub`, the *next* release erroneously
/// zeroes out a different, still-genuinely-held permit's occupancy, and the
/// pool then over-admits past its cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn dropped_queued_caller_releases_exactly_once() {
    let fs = FairShare::start(
        Arc::new(StaticCapacity::new(2)),
        FairshareAlgorithm::Weighted,
        2,
    );
    let tenant = Uuid::new_v4();

    // Fill both slots.
    let a = fs
        .admit(AdmitRequest::new(tenant, "m", 10))
        .await
        .unwrap()
        .permit;
    let b = fs
        .admit(AdmitRequest::new(tenant, "m", 10))
        .await
        .unwrap()
        .permit;

    // A third request queues, then its caller gives up: the timeout fires
    // and drops the `admit` future (and its oneshot receiver) while the
    // request is still waiting in the pool's queue.
    let fs2 = fs.clone();
    let _ = tokio::time::timeout(
        Duration::from_millis(20),
        fs2.admit(AdmitRequest::new(tenant, "m", 10)),
    )
    .await;

    // Freeing `a`'s slot dispatches the abandoned request onto it; its
    // `respond` send fails (the receiver is gone), which must release that
    // one slot -- not `a`'s slot again *and* silently zero out `b`'s, which
    // is still genuinely held.
    drop(a);
    tokio::time::sleep(Duration::from_millis(30)).await;

    let snap = fs.snapshot().await.unwrap();
    assert_eq!(
        snap.global_in_flight, 1,
        "only b's permit is still held; a double release would undercount it"
    );

    let c = fs.admit(AdmitRequest::new(tenant, "m", 10)).await.unwrap();
    assert_eq!(
        c.admission,
        Admission::Fast,
        "the one freed slot should be available"
    );

    // b and c now genuinely hold both slots. A third concurrent admit must
    // queue, not be handed out on top of an undercount.
    let over_admitted = tokio::time::timeout(
        Duration::from_millis(50),
        fs.admit(AdmitRequest::new(tenant, "m", 10)),
    )
    .await
    .is_ok();
    assert!(
        !over_admitted,
        "a third admit must not be granted while b and c both still hold real slots at cap=2"
    );

    drop(c.permit);
    drop(b);
}

/// When the global ceiling binds across multiple pools, `dispatch_all`'s
/// rotating cursor must share the ceiling round-robin rather than draining
/// one pool's whole backlog before the other pool gets a slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn ceiling_dispatch_round_robins_across_pools() {
    let fs = FairShare::start(
        Arc::new(StaticCapacity::new(1)),
        FairshareAlgorithm::Weighted,
        4,
    );
    let tenant = Uuid::new_v4();

    // Hold the single global slot on pool "a".
    let held = fs
        .admit(AdmitRequest::new(tenant, "a", 1))
        .await
        .unwrap()
        .permit;

    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for model in ["a", "a", "a", "b", "b", "b"] {
        let fs = fs.clone();
        let order = order.clone();
        handles.push(tokio::spawn(async move {
            let admitted = fs.admit(AdmitRequest::new(tenant, model, 1)).await.unwrap();
            order.lock().unwrap().push(model);
            tokio::time::sleep(Duration::from_millis(5)).await;
            drop(admitted.permit);
        }));
    }
    tokio::time::sleep(Duration::from_millis(30)).await;
    drop(held);
    for h in handles {
        let _ = tokio::time::timeout(Duration::from_secs(5), h).await;
    }

    let order = order.lock().unwrap().clone();
    assert_eq!(
        order.len(),
        6,
        "all six queued requests must eventually be granted"
    );

    let first_four = &order[..4];
    assert!(
        first_four.contains(&"a"),
        "pool a must appear in the first four grants: {order:?}"
    );
    assert!(
        first_four.contains(&"b"),
        "pool b must appear in the first four grants: {order:?}"
    );
    assert!(
        !order[..3].iter().all(|m| *m == "a") && !order[..3].iter().all(|m| *m == "b"),
        "one pool must not drain fully before the other gets a slot: {order:?}"
    );
    for w in order.windows(3) {
        assert!(
            w.contains(&"a") && w.contains(&"b"),
            "round-robin window {w:?} should include both pools: {order:?}"
        );
    }
}
