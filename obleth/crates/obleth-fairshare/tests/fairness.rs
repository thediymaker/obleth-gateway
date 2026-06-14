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
