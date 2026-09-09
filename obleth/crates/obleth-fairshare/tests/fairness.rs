//! Fairness property tests for the admission scheduler.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use obleth_config::{Admission, FairshareAlgorithm};
use obleth_fairshare::{AdmitRequest, FairShare, StaticCapacity};
use uuid::Uuid;

#[tokio::test]
async fn fast_path_when_idle() {
    let cap = Arc::new(StaticCapacity::new(8));
    let fs = FairShare::start(cap, FairshareAlgorithm::Weighted);
    let admitted = fs
        .admit(AdmitRequest::weighted(Uuid::new_v4(), 1, 10))
        .await
        .expect("admit");
    assert_eq!(admitted.admission, Admission::Fast);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn saturated_model_does_not_block_other_models() {
    let cap = Arc::new(StaticCapacity::new(4));
    let fs = FairShare::start(cap, FairshareAlgorithm::Weighted);
    let slow_tenant = Uuid::new_v4();
    let fast_tenant = Uuid::new_v4();

    let slow = |tenant| AdmitRequest {
        tenant,
        weight: 100,
        group: "default".into(),
        group_weight: 100,
        model: "slow".into(),
        model_max_in_flight: Some(1),
        cost: 10,
    };
    let fast = |tenant| AdmitRequest {
        tenant,
        weight: 100,
        group: "default".into(),
        group_weight: 100,
        model: "fast".into(),
        model_max_in_flight: Some(4),
        cost: 10,
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
    let fs = FairShare::start(cap, FairshareAlgorithm::Weighted);

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
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical);

    let low = Uuid::new_v4();
    let high = Uuid::new_v4();
    let order: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));

    let request = |tenant, weight| AdmitRequest {
        tenant,
        weight,
        group: "general".into(),
        group_weight: 100,
        model: "general-model".into(),
        model_max_in_flight: None,
        cost: 10,
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
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical);

    let chatbot = Uuid::new_v4();
    let api = Uuid::new_v4();

    let mut permits = Vec::new();
    for _ in 0..8 {
        let adm = fs
            .admit(AdmitRequest {
                tenant: chatbot,
                weight: 500,
                group: "chatbot".into(),
                group_weight: 500,
                model: "chat-model".into(),
                model_max_in_flight: None,
                cost: 10,
            })
            .await
            .expect("chatbot admit");
        permits.push(adm.permit);
    }

    let fs2 = fs.clone();
    let api_handle = tokio::spawn(async move {
        fs2.admit(AdmitRequest {
            tenant: api,
            weight: 50,
            group: "api".into(),
            group_weight: 50,
            model: "api-model".into(),
            model_max_in_flight: None,
            cost: 10,
        })
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
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical);

    let chatbot = Uuid::new_v4();
    let chatbot2 = Uuid::new_v4();

    let request = |tenant| AdmitRequest {
        tenant,
        weight: 500,
        group: "chatbot".into(),
        group_weight: 500,
        model: "chat-model".into(),
        model_max_in_flight: None,
        cost: 10,
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
    let fs = FairShare::start(cap, FairshareAlgorithm::Weighted);
    let tenant = Uuid::new_v4();

    let req = move |model: &str| AdmitRequest {
        tenant,
        weight: 100,
        group: "default".into(),
        group_weight: 100,
        model: model.into(),
        model_max_in_flight: Some(1),
        cost: 10,
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
        let fs = FairShare::start(cap, algorithm);
        let heavy = Uuid::new_v4();
        let light = Uuid::new_v4();

        let req = |tenant: Uuid, weight: i64, cost: u32| AdmitRequest {
            tenant,
            weight,
            group: "shared".into(),
            group_weight: 100,
            model: "m".into(),
            model_max_in_flight: None,
            cost,
        };

        // Same 1000 tokens of service, weights 500 and 50.
        let a = fs.admit(req(heavy, 500, 1000)).await.expect("heavy admit");
        let b = fs.admit(req(light, 50, 1000)).await.expect("light admit");

        let snap = fs.snapshot().await.expect("snapshot");
        let score = |id: Uuid| {
            snap.tenants
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
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical);
    let a = Uuid::new_v4();
    let b = Uuid::new_v4();

    let req = |tenant: Uuid| AdmitRequest {
        tenant,
        weight: 100,
        group: "shared".into(),
        group_weight: 100,
        model: "m".into(),
        model_max_in_flight: None,
        cost: 10,
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
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical);
    let big = Uuid::new_v4();
    let small = Uuid::new_v4();

    let req = |tenant: Uuid, group: &'static str, gw: i64| AdmitRequest {
        tenant,
        weight: 100,
        group: group.into(),
        group_weight: gw,
        model: "m".into(),
        model_max_in_flight: None,
        cost: 10,
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
    let big_g = snap.groups.iter().find(|g| g.name == "big").expect("big");
    let small_g = snap
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
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical);
    let big = Uuid::new_v4();
    let small = Uuid::new_v4();

    let req = |tenant: Uuid, group: &'static str, gw: i64| AdmitRequest {
        tenant,
        weight: 100,
        group: group.into(),
        group_weight: gw,
        model: "m".into(),
        model_max_in_flight: None,
        cost: 10,
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
    let big_g = snap.groups.iter().find(|g| g.name == "big").expect("big");
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
    let fs = FairShare::start(cap, FairshareAlgorithm::Hierarchical);
    let big = Uuid::new_v4();
    let small = Uuid::new_v4();

    let req = |tenant: Uuid, group: &'static str, gw: i64, cost: u32| AdmitRequest {
        tenant,
        weight: 100,
        group: group.into(),
        group_weight: gw,
        model: "m".into(),
        model_max_in_flight: None,
        cost,
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
    let small_t = snap
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
