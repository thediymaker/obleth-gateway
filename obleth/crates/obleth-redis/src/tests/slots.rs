//! Shared fairshare slots across gateways, and the replica views they
//! publish and merge.

use crate::test_util::*;
use crate::*;

use obleth_fairshare::{
    AcquireOutcome, FairShare, PoolHoldings, ReconcileOutcome, SharedSlotsConfig, SlotClaim,
    SlotDenial, SlotHold, SlotMode, SlotRelease, StaticCapacity,
};

fn claim(pool: &str, pool_cap: usize, global_cap: usize) -> SlotClaim {
    SlotClaim {
        pool: pool.into(),
        tenant: Uuid::new_v4(),
        key: Uuid::new_v4(),
        slots: 1,
        pool_cap,
        global_cap,
        tenant_cap: None,
        key_cap: None,
        hold: None,
        redeem_only: false,
        starving: false,
        sibling: None,
        background: false,
    }
}

async fn live(store: &RedisStore, keys: &SlotKeys, instances: &[&str]) {
    for i in instances {
        store
            .replica_heartbeat_in(keys, i, std::time::Duration::from_secs(60))
            .await
            .unwrap();
    }
}

async fn totals_field(store: &RedisStore, keys: &SlotKeys, field: &str) -> i64 {
    let mut conn = store.conn.clone();
    let v: Option<i64> = conn.hget(&keys.totals, field).await.unwrap();
    v.unwrap_or(0)
}

async fn held_field(store: &RedisStore, keys: &SlotKeys, instance: &str, field: &str) -> i64 {
    let mut conn = store.conn.clone();
    let v: Option<i64> = conn.hget(keys.held(instance), field).await.unwrap();
    v.unwrap_or(0)
}

/// Fire `claims` concurrently, spread over `instances`; return how many
/// were granted.
async fn race(
    store: &RedisStore,
    keys: &SlotKeys,
    instances: &[&str],
    claims: Vec<SlotClaim>,
) -> Vec<SlotClaim> {
    let tasks: Vec<_> = claims
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let (store, keys) = (store.clone(), keys.clone());
            let instance = instances[i % instances.len()].to_string();
            tokio::spawn(async move {
                let (outcome, _) = store.slot_acquire(&keys, &instance, &c).await.unwrap();
                (outcome == AcquireOutcome::Granted).then_some(c)
            })
        })
        .collect();
    let mut granted = Vec::new();
    for t in tasks {
        if let Some(c) = t.await.unwrap() {
            granted.push(c);
        }
    }
    granted
}

/// The totals always equal the sum of the replicas' holdings.
async fn assert_books_balance(store: &RedisStore, keys: &SlotKeys, instances: &[&str]) {
    let mut conn = store.conn.clone();
    let totals: std::collections::HashMap<String, i64> = conn.hgetall(&keys.totals).await.unwrap();
    let mut summed: std::collections::HashMap<String, i64> = Default::default();
    for i in instances {
        let held: std::collections::HashMap<String, i64> =
            conn.hgetall(keys.held(i)).await.unwrap();
        for (f, n) in held {
            *summed.entry(f).or_default() += n;
        }
    }
    let counts: std::collections::HashMap<String, i64> = totals
        .into_iter()
        .filter(|(f, _)| !f.starts_with('w'))
        .collect();
    assert_eq!(counts, summed, "totals are the sum of the holdings");
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// Concurrent claims from several replicas never take a slot past any
/// limit, each limit binding on its own and all of them together.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn slot_claims_are_atomic_across_every_limit() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    let gws = ["a", "b", "c", "d"];
    live(&store, &keys, &gws).await;

    // The pool binds.
    let granted = race(
        &store,
        &keys,
        &gws,
        (0..50).map(|_| claim("m:p", 10, 1000)).collect(),
    )
    .await;
    assert_eq!(granted.len(), 10);
    assert_eq!(totals_field(&store, &keys, "p|m:p").await, 10);
    assert_eq!(totals_field(&store, &keys, "g").await, 10);

    // The tenant binds, in a pool with room.
    let tenant = Uuid::new_v4();
    let granted = race(
        &store,
        &keys,
        &gws,
        (0..30)
            .map(|_| SlotClaim {
                tenant,
                tenant_cap: Some(3),
                ..claim("m:t", 100, 1000)
            })
            .collect(),
    )
    .await;
    assert_eq!(granted.len(), 3);

    // The key binds.
    let key = Uuid::new_v4();
    let granted = race(
        &store,
        &keys,
        &gws,
        (0..30)
            .map(|_| SlotClaim {
                key,
                key_cap: Some(2),
                ..claim("m:k", 100, 1000)
            })
            .collect(),
    )
    .await;
    assert_eq!(granted.len(), 2);

    // The global count (15 so far) binds across pools.
    let granted = race(
        &store,
        &keys,
        &gws,
        (0..30).map(|_| claim("m:g", 100, 19)).collect(),
    )
    .await;
    assert_eq!(granted.len(), 4);
    assert_eq!(totals_field(&store, &keys, "g").await, 19);

    // All at once: a 6-slot pool, one tenant capped at 3 sending most of
    // the load and one of its keys capped at 2.
    let heavy = Uuid::new_v4();
    let heavy_key = Uuid::new_v4();
    let mixed: Vec<SlotClaim> = (0..60)
        .map(|i| match i % 3 {
            0 => SlotClaim {
                tenant: heavy,
                key: heavy_key,
                tenant_cap: Some(3),
                key_cap: Some(2),
                ..claim("m:x", 6, 1000)
            },
            1 => SlotClaim {
                tenant: heavy,
                tenant_cap: Some(3),
                ..claim("m:x", 6, 1000)
            },
            _ => claim("m:x", 6, 1000),
        })
        .collect();
    let granted = race(&store, &keys, &gws, mixed).await;
    assert_eq!(granted.len(), 6);
    assert!(granted.iter().filter(|c| c.tenant == heavy).count() <= 3);
    assert!(granted.iter().filter(|c| c.key == heavy_key).count() <= 2);
    assert_eq!(totals_field(&store, &keys, "p|m:x").await, 6);
    assert_books_balance(&store, &keys, &gws).await;
    drop_test_keys(&store, &keys, &gws).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn slot_releases_free_only_what_the_replica_holds() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    live(&store, &keys, &["a", "b"]).await;
    let c = claim("m:p", 4, 100);
    let (outcome, counts) = store.slot_acquire(&keys, "a", &c).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Granted);
    assert_eq!((counts.pool, counts.global), (1, 1));
    let slot = SlotRelease {
        pool: c.pool.clone(),
        tenant: c.tenant,
        key: c.key,
        slots: 1,
        sibling: None,
    };
    // b holds nothing, so its release frees nothing.
    let (freed, counts) = store.slot_release(&keys, "b", &slot).await.unwrap();
    assert!(!freed);
    assert_eq!(counts.pool, 1);
    let (freed, counts) = store.slot_release(&keys, "a", &slot).await.unwrap();
    assert!(freed);
    assert_eq!((counts.pool, counts.global), (0, 0));
    // A second release of the same slot is a no-op, not a negative count.
    let (freed, _) = store.slot_release(&keys, "a", &slot).await.unwrap();
    assert!(!freed);
    let mut conn = store.conn.clone();
    let fields: std::collections::HashMap<String, i64> = conn.hgetall(&keys.totals).await.unwrap();
    assert!(fields.is_empty(), "counts at zero are deleted: {fields:?}");
    drop_test_keys(&store, &keys, &["a", "b"]).await;
}

/// Take a slot for `c` on `instance`; what releases it.
async fn take_slot(
    store: &RedisStore,
    keys: &SlotKeys,
    instance: &str,
    c: &SlotClaim,
) -> SlotRelease {
    let (outcome, _) = store.slot_acquire(keys, instance, c).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Granted);
    SlotRelease {
        pool: c.pool.clone(),
        tenant: c.tenant,
        key: c.key,
        slots: 1,
        sibling: None,
    }
}

fn slot_hold(id: &str, ttl_ms: u64, max_held: usize) -> SlotHold {
    SlotHold {
        id: id.into(),
        ttl: std::time::Duration::from_millis(ttl_ms),
        max_held,
        arrived_ago: std::time::Duration::ZERO,
        remember: std::time::Duration::from_secs(120),
        max_run: 8,
    }
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// A slot is held only for a step that followed straight on from the
/// session's previous one; a held slot keeps counting, moves to
/// whichever replica claims it for its session, and is freed when nobody
/// does.
#[tokio::test]
async fn slot_holds_keep_counting_move_to_the_claimant_and_run_out() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    live(&store, &keys, &["a", "b"]).await;
    let c = claim("m:h", 1, 100);
    let hold = slot_hold("m:h|k/s", 60_000, 1);

    // The session's first step has nothing to chain to: released.
    let slot = take_slot(&store, &keys, "a", &c).await;
    let (held, counts) = store.slot_hold(&keys, "a", &slot, &hold).await.unwrap();
    assert!(!held);
    assert_eq!((counts.pool, counts.global), (0, 0));

    // Its second, straight after, on any replica: held.
    let slot = take_slot(&store, &keys, "a", &c).await;
    let (held, counts) = store.slot_hold(&keys, "a", &slot, &hold).await.unwrap();
    assert!(held);
    assert_eq!(
        (counts.pool, counts.global),
        (1, 1),
        "a held slot still counts"
    );
    assert_eq!(held_field(&store, &keys, "a", "p|m:h").await, 0);
    assert_eq!(totals_field(&store, &keys, "h|m:h").await, 1);

    // Nobody else gets it, and a claim for another session's hold alone
    // takes nothing.
    let (outcome, _) = store
        .slot_acquire(&keys, "b", &claim("m:h", 1, 100))
        .await
        .unwrap();
    assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Pool));
    let probe = SlotClaim {
        hold: Some("m:h|k/other".into()),
        redeem_only: true,
        ..claim("m:h", 1, 100)
    };
    let (outcome, _) = store.slot_acquire(&keys, "b", &probe).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::NotHeld);

    // The session's next step, on b, takes it over.
    let next = SlotClaim {
        hold: Some(hold.id.clone()),
        redeem_only: true,
        ..c.clone()
    };
    let (outcome, counts) = store.slot_acquire(&keys, "b", &next).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Redeemed);
    assert_eq!((counts.pool, counts.global), (1, 1));
    assert_eq!(held_field(&store, &keys, "b", "p|m:h").await, 1);
    assert_eq!(
        held_field(&store, &keys, "b", &format!("k|m:h|{}", c.key)).await,
        1
    );
    assert_eq!(totals_field(&store, &keys, "h|m:h").await, 0);
    let (outcome, _) = store.slot_acquire(&keys, "b", &next).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::NotHeld, "taken once only");

    // b gives it back like any slot it holds.
    let (freed, counts) = store.slot_release(&keys, "b", &slot).await.unwrap();
    assert!(freed);
    assert_eq!((counts.pool, counts.global), (0, 0));

    // A step that came later than the hold after the one before is not
    // held for.
    let late = slot_hold("m:h|k/late", 200, 1);
    let c = claim("m:h", 4, 100);
    let slot = take_slot(&store, &keys, "a", &c).await;
    store.slot_hold(&keys, "a", &slot, &late).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    let slot = take_slot(&store, &keys, "a", &c).await;
    let (held, _) = store.slot_hold(&keys, "a", &slot, &late).await.unwrap();
    assert!(!held);

    // At most max_held per pool: a second hold is a plain release.
    let mut slots = Vec::new();
    for id in ["m:h|k/1", "m:h|k/2"] {
        let c = claim("m:h", 4, 100);
        let first = take_slot(&store, &keys, "a", &c).await;
        store
            .slot_hold(&keys, "a", &first, &slot_hold(id, 200, 1))
            .await
            .unwrap();
        slots.push((id, take_slot(&store, &keys, "a", &c).await));
    }
    let (held, _) = store
        .slot_hold(&keys, "a", &slots[0].1, &slot_hold(slots[0].0, 200, 1))
        .await
        .unwrap();
    assert!(held);
    let (held, counts) = store
        .slot_hold(&keys, "a", &slots[1].1, &slot_hold(slots[1].0, 200, 1))
        .await
        .unwrap();
    assert!(!held);
    assert_eq!(counts.pool, 1, "released, not held");

    // Nobody takes the first: the next heartbeat after it runs out frees it.
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    live(&store, &keys, &["a"]).await;
    let mut conn = store.conn.clone();
    let mut fields: std::collections::HashMap<String, i64> =
        conn.hgetall(&keys.totals).await.unwrap();
    // The refused claim above left its wait marker.
    fields.retain(|f, _| !f.starts_with('w'));
    assert!(fields.is_empty(), "everything given back: {fields:?}");
    let ids: Vec<String> = conn.zrange(&keys.holds, 0, -1).await.unwrap();
    assert!(ids.is_empty());
    let left: bool = conn
        .exists(format!("{}m:h|k/1", keys.hold_prefix))
        .await
        .unwrap();
    assert!(!left);
    drop_test_keys(&store, &keys, &["a", "b"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// While another replica waits for the pool, a session is held for at
/// most `max_run` steps in a row; then its slot goes to the waiter.
#[tokio::test]
async fn a_session_holds_only_so_many_times_in_a_row_while_others_wait() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    live(&store, &keys, &["a", "b"]).await;
    let c = claim("m:r", 1, 100);
    let hold = SlotHold {
        max_run: 2,
        ..slot_hold("m:r|k/s", 60_000, 1)
    };
    let next = SlotClaim {
        hold: Some(hold.id.clone()),
        redeem_only: true,
        ..c.clone()
    };
    let slot = take_slot(&store, &keys, "a", &c).await;
    store.slot_hold(&keys, "a", &slot, &hold).await.unwrap();

    // Nobody waits: held as often as the session loops.
    take_slot(&store, &keys, "a", &c).await;
    for _ in 0..3 {
        let (held, _) = store.slot_hold(&keys, "a", &slot, &hold).await.unwrap();
        assert!(held);
        let (outcome, _) = store.slot_acquire(&keys, "a", &next).await.unwrap();
        assert_eq!(outcome, AcquireOutcome::Redeemed);
    }

    // b waits for the pool: two more holds, then the slot is released.
    let (outcome, _) = store
        .slot_acquire(&keys, "b", &claim("m:r", 1, 100))
        .await
        .unwrap();
    assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Pool));
    for _ in 0..2 {
        let (held, _) = store.slot_hold(&keys, "a", &slot, &hold).await.unwrap();
        assert!(held);
        let (outcome, _) = store.slot_acquire(&keys, "a", &next).await.unwrap();
        assert_eq!(outcome, AcquireOutcome::Redeemed);
    }
    let (held, counts) = store.slot_hold(&keys, "a", &slot, &hold).await.unwrap();
    assert!(!held, "the waiter's turn");
    assert_eq!(counts.pool, 0);
    let (outcome, _) = store
        .slot_acquire(&keys, "b", &claim("m:r", 1, 100))
        .await
        .unwrap();
    assert_eq!(outcome, AcquireOutcome::Granted);
    let mut conn = store.conn.clone();
    let run: Option<i64> = conn.hget(&keys.chain, &hold.id).await.unwrap();
    assert_eq!(run, None, "giving way starts the count again");
    drop_test_keys(&store, &keys, &["a", "b"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// A claim for a request that waited too long frees the pool's held
/// slots, and none is held there while such claims keep coming.
#[tokio::test]
async fn a_starving_claim_frees_held_slots_and_stops_new_ones() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    live(&store, &keys, &["a", "b"]).await;
    let c = claim("m:s", 1, 100);
    let hold = slot_hold("m:s|k/s", 60_000, 1);
    let slot = take_slot(&store, &keys, "a", &c).await;
    store.slot_hold(&keys, "a", &slot, &hold).await.unwrap();
    take_slot(&store, &keys, "a", &c).await;
    let (held, _) = store.slot_hold(&keys, "a", &slot, &hold).await.unwrap();
    assert!(held);

    let starving = SlotClaim {
        starving: true,
        ..claim("m:s", 1, 100)
    };
    let (outcome, counts) = store.slot_acquire(&keys, "b", &starving).await.unwrap();
    assert_eq!(
        outcome,
        AcquireOutcome::Granted,
        "the held slot was freed for it"
    );
    assert_eq!(counts.pool, 1);
    assert_eq!(totals_field(&store, &keys, "h|m:s").await, 0);
    let mut conn = store.conn.clone();
    let ids: Vec<String> = conn.zrange(&keys.holds, 0, -1).await.unwrap();
    assert!(ids.is_empty());

    // The session's next steps are not held for meanwhile.
    let next = SlotRelease {
        pool: starving.pool.clone(),
        tenant: starving.tenant,
        key: starving.key,
        slots: 1,
        sibling: None,
    };
    let (held, counts) = store.slot_hold(&keys, "b", &next, &hold).await.unwrap();
    assert!(!held);
    assert_eq!(counts.pool, 0);
    drop_test_keys(&store, &keys, &["a", "b"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// A replica's slots are reclaimed once its heartbeat expires, by the
/// next live replica's heartbeat, and not a moment before.
#[tokio::test]
async fn slot_holdings_are_reclaimed_when_the_heartbeat_expires_and_not_before() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    let ttl = std::time::Duration::from_millis(400);
    store.replica_heartbeat_in(&keys, "a", ttl).await.unwrap();
    store.replica_heartbeat_in(&keys, "b", ttl).await.unwrap();
    let mut held = Vec::new();
    for _ in 0..3 {
        let c = claim("m:p", 10, 100);
        let (outcome, _) = store.slot_acquire(&keys, "b", &c).await.unwrap();
        assert_eq!(outcome, AcquireOutcome::Granted);
        held.push(c);
    }
    // Inside b's TTL: a's heartbeat leaves b's slots alone.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert_eq!(
        store.replica_heartbeat_in(&keys, "a", ttl).await.unwrap(),
        2
    );
    assert_eq!(totals_field(&store, &keys, "p|m:p").await, 3);
    assert_eq!(held_field(&store, &keys, "b", "p|m:p").await, 3);

    // Past it: the next heartbeat reclaims them.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert_eq!(
        store.replica_heartbeat_in(&keys, "a", ttl).await.unwrap(),
        1
    );
    assert_eq!(totals_field(&store, &keys, "p|m:p").await, 0);
    assert_eq!(totals_field(&store, &keys, "g").await, 0);
    assert_eq!(held_field(&store, &keys, "b", "p|m:p").await, 0);

    // A late release from b frees nothing, and b takes nothing new until
    // it heartbeats again.
    let slot = SlotRelease {
        pool: held[0].pool.clone(),
        tenant: held[0].tenant,
        key: held[0].key,
        slots: 1,
        sibling: None,
    };
    let (freed, _) = store.slot_release(&keys, "b", &slot).await.unwrap();
    assert!(!freed);
    let (outcome, _) = store
        .slot_acquire(&keys, "b", &claim("m:p", 10, 100))
        .await
        .unwrap();
    assert_eq!(outcome, AcquireOutcome::NotRegistered);
    drop_test_keys(&store, &keys, &["a", "b"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn slot_reconcile_replaces_the_holdings_and_repairs_drift() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    live(&store, &keys, &["a", "b"]).await;
    let tenant = Uuid::new_v4();
    let key = Uuid::new_v4();
    for _ in 0..2 {
        store
            .slot_acquire(
                &keys,
                "a",
                &SlotClaim {
                    tenant,
                    key,
                    ..claim("m:p", 10, 100)
                },
            )
            .await
            .unwrap();
    }
    store
        .slot_acquire(&keys, "b", &claim("m:p", 10, 100))
        .await
        .unwrap();
    // Drift: three slots a no longer really holds (lost releases).
    let mut conn = store.conn.clone();
    for f in ["g", "p|m:p"] {
        let _: i64 = conn.hincr(keys.held("a"), f, 3).await.unwrap();
        let _: i64 = conn.hincr(&keys.totals, f, 3).await.unwrap();
    }
    assert_eq!(totals_field(&store, &keys, "p|m:p").await, 6);

    let truth = vec![PoolHoldings {
        pool: "m:p".into(),
        in_flight: 1,
        slots: 1,
        tenants: vec![(tenant, 1)],
        keys: vec![(key, 1)],
    }];
    assert_eq!(
        store.slot_reconcile(&keys, "a", &truth).await.unwrap(),
        ReconcileOutcome::Synced
    );
    assert_eq!(
        totals_field(&store, &keys, "p|m:p").await,
        2,
        "a's 1 + b's 1"
    );
    assert_eq!(totals_field(&store, &keys, "g").await, 2);
    assert_eq!(
        totals_field(&store, &keys, &format!("t|m:p|{tenant}")).await,
        1
    );
    assert_eq!(held_field(&store, &keys, "a", "p|m:p").await, 1);
    assert_books_balance(&store, &keys, &["a", "b"]).await;

    // Reconciling to nothing clears a's books; b's are untouched.
    store.slot_reconcile(&keys, "a", &[]).await.unwrap();
    assert_eq!(totals_field(&store, &keys, "p|m:p").await, 1);

    // A replica that is not live writes nothing.
    assert_eq!(
        store.slot_reconcile(&keys, "ghost", &truth).await.unwrap(),
        ReconcileOutcome::NotRegistered
    );
    assert_eq!(totals_field(&store, &keys, "p|m:p").await, 1);
    drop_test_keys(&store, &keys, &["a", "b", "ghost"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// A model's interactive and background pools share its cap: each claim
/// counts the other pool's slots, a background refusal marks nothing as
/// waited on, and freeing a background slot wakes interactive waiters.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sibling_pools_share_the_model_cap() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    live(&store, &keys, &["a", "b"]).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (subscribed_tx, mut subscribed) = tokio::sync::mpsc::unbounded_channel::<()>();
    let listener = {
        let (store, keys) = (store.clone(), keys.clone());
        tokio::spawn(async move {
            store
                .run_slot_release_listener(
                    &keys,
                    move |pool| {
                        let _ = tx.send(pool);
                    },
                    move |_| {
                        let _ = subscribed_tx.send(());
                    },
                )
                .await
        })
    };
    subscribed.recv().await.expect("subscribed");

    let bg = |cap| SlotClaim {
        sibling: Some("m:s".into()),
        background: true,
        ..claim("b:s", cap, 100)
    };
    let fg = || SlotClaim {
        sibling: Some("b:s".into()),
        ..claim("m:s", 4, 100)
    };
    // Background may fill 3 of 4.
    let mut bg_slots = Vec::new();
    for _ in 0..3 {
        let c = bg(3);
        let (outcome, _) = store.slot_acquire(&keys, "a", &c).await.unwrap();
        assert_eq!(outcome, AcquireOutcome::Granted);
        bg_slots.push(c);
    }
    let (outcome, _) = store.slot_acquire(&keys, "a", &bg(3)).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Pool));
    // Marked on its own pool only: interactive holds read `w|m:s`.
    assert!(
        totals_field(&store, &keys, "w|b:s").await > 0,
        "own pool marked"
    );
    assert_eq!(totals_field(&store, &keys, "w|m:s").await, 0);
    assert_eq!(totals_field(&store, &keys, "w").await, 0);
    // A background refusal at its share of the ceiling marks its pool,
    // not the ceiling.
    let (outcome, _) = store
        .slot_acquire(
            &keys,
            "a",
            &SlotClaim {
                global_cap: 1,
                ..bg(9)
            },
        )
        .await
        .unwrap();
    assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Global));
    assert_eq!(totals_field(&store, &keys, "w").await, 0);

    // Interactive takes the last slot, then counts background's three.
    let (outcome, _) = store.slot_acquire(&keys, "b", &fg()).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Granted);
    let (outcome, _) = store.slot_acquire(&keys, "b", &fg()).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Pool));

    // Freeing a background slot wakes both pools: each was waited on.
    let c = bg_slots.pop().unwrap();
    let slot = SlotRelease {
        pool: c.pool.clone(),
        tenant: c.tenant,
        key: c.key,
        slots: 1,
        sibling: Some("m:s".into()),
    };
    let (freed, _) = store.slot_release(&keys, "a", &slot).await.unwrap();
    assert!(freed);
    let mut woken = Vec::new();
    for _ in 0..2 {
        woken.push(
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("published")
                .unwrap(),
        );
    }
    woken.sort();
    assert_eq!(woken, vec!["b:s".to_string(), "m:s".to_string()]);
    let last = fg();
    let (outcome, _) = store.slot_acquire(&keys, "b", &last).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Granted);

    // Slots free while the interactive pool is still marked as waited on
    // (by gw-b's refusal above): background on gw-a is refused although
    // there is room under its share, since an interactive request may be
    // waiting for that slot.
    let (freed, _) = store
        .slot_release(
            &keys,
            "b",
            &SlotRelease {
                pool: last.pool.clone(),
                tenant: last.tenant,
                key: last.key,
                slots: 1,
                sibling: Some("b:s".into()),
            },
        )
        .await
        .unwrap();
    assert!(freed);
    // And a background slot: the pool is at 2 of 4, under background's
    // share of 3, so only the mark stands in the way.
    let c = bg_slots.pop().unwrap();
    let (freed, _) = store
        .slot_release(
            &keys,
            "a",
            &SlotRelease {
                pool: c.pool.clone(),
                tenant: c.tenant,
                key: c.key,
                slots: 1,
                sibling: Some("m:s".into()),
            },
        )
        .await
        .unwrap();
    assert!(freed);
    assert!(
        totals_field(&store, &keys, "w|m:s").await > 0,
        "still marked"
    );
    let (outcome, _) = store.slot_acquire(&keys, "a", &bg(3)).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Pool));
    // Once nobody has waited for a while, background may take it.
    tokio::time::sleep(std::time::Duration::from_millis(
        crate::slots::WAIT_MARK_MS + 100,
    ))
    .await;
    let (outcome, _) = store.slot_acquire(&keys, "a", &bg(3)).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Granted);
    listener.abort();
    drop_test_keys(&store, &keys, &["a", "b"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// A release publishes the pool only while someone waits for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn slot_releases_publish_only_while_someone_waits() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    live(&store, &keys, &["a", "b"]).await;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let (subscribed_tx, mut subscribed) = tokio::sync::mpsc::unbounded_channel::<()>();
    let listener = {
        let (store, keys) = (store.clone(), keys.clone());
        tokio::spawn(async move {
            store
                .run_slot_release_listener(
                    &keys,
                    move |pool| {
                        let _ = tx.send(pool);
                    },
                    move |_| {
                        let _ = subscribed_tx.send(());
                    },
                )
                .await
        })
    };
    subscribed.recv().await.expect("subscribed");

    let c = claim("m:p", 1, 100);
    store.slot_acquire(&keys, "a", &c).await.unwrap();
    let slot = SlotRelease {
        pool: c.pool.clone(),
        tenant: c.tenant,
        key: c.key,
        slots: 1,
        sibling: None,
    };
    store.slot_release(&keys, "a", &slot).await.unwrap();
    let quiet = tokio::time::timeout(std::time::Duration::from_millis(200), rx.recv()).await;
    assert!(quiet.is_err(), "nobody waited, so nothing was published");

    store.slot_acquire(&keys, "a", &c).await.unwrap();
    let (outcome, _) = store
        .slot_acquire(&keys, "b", &claim("m:p", 1, 100))
        .await
        .unwrap();
    assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Pool));
    store.slot_release(&keys, "a", &slot).await.unwrap();
    let woken = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
        .await
        .expect("published")
        .unwrap();
    assert_eq!(woken, "m:p");
    listener.abort();
    drop_test_keys(&store, &keys, &["a", "b"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn replica_deregister_frees_what_the_replica_still_holds() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    live(&store, &keys, &["a"]).await;
    store
        .slot_acquire(&keys, "a", &claim("m:p", 10, 100))
        .await
        .unwrap();
    store.replica_deregister_in(&keys, "a").await.unwrap();
    assert_eq!(totals_field(&store, &keys, "p|m:p").await, 0);
    assert_eq!(held_field(&store, &keys, "a", "p|m:p").await, 0);
    drop_test_keys(&store, &keys, &["a"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// Background claims take slots by prompt length: each replica's
/// multi-slot holds add up in the pool count (one request each in the
/// global, tenant and key counts), a claim bigger than the room left
/// runs only alone, a release frees exactly what it took, and a replica
/// whose heartbeat runs out has exactly its slots reclaimed.
#[tokio::test]
async fn multi_slot_background_claims_add_up_and_release_cleanly() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    let ttl = std::time::Duration::from_millis(400);
    store.replica_heartbeat_in(&keys, "a", ttl).await.unwrap();
    store.replica_heartbeat_in(&keys, "b", ttl).await.unwrap();
    let bg = |slots| SlotClaim {
        slots,
        sibling: Some("m:x".into()),
        background: true,
        ..claim("b:x", 8, 100)
    };
    let release = |c: &SlotClaim| SlotRelease {
        pool: c.pool.clone(),
        tenant: c.tenant,
        key: c.key,
        slots: c.slots,
        sibling: c.sibling.clone(),
    };
    // Interactive work on the model holds 2 through a.
    for _ in 0..2 {
        take_slot(
            &store,
            &keys,
            "a",
            &SlotClaim {
                sibling: Some("b:x".into()),
                ..claim("m:x", 16, 100)
            },
        )
        .await;
    }
    let on_a = bg(4);
    let (outcome, counts) = store.slot_acquire(&keys, "a", &on_a).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Granted);
    assert_eq!((counts.pool, counts.global), (4, 3));
    // 4 + 2 interactive + 3 > 8: refused; 2 fits exactly.
    let (outcome, _) = store.slot_acquire(&keys, "b", &bg(3)).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Pool));
    let on_b = bg(2);
    let (outcome, counts) = store.slot_acquire(&keys, "b", &on_b).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Granted);
    assert_eq!((counts.pool, counts.global), (6, 4));
    assert_eq!(held_field(&store, &keys, "a", "p|b:x").await, 4);
    assert_eq!(held_field(&store, &keys, "b", "p|b:x").await, 2);
    assert_eq!(held_field(&store, &keys, "b", "g").await, 1);
    assert_eq!(
        totals_field(&store, &keys, &format!("t|b:x|{}", on_b.tenant)).await,
        1
    );
    assert_books_balance(&store, &keys, &["a", "b"]).await;

    // a's release frees its 4 and its one request.
    let (freed, counts) = store
        .slot_release(&keys, "a", &release(&on_a))
        .await
        .unwrap();
    assert!(freed);
    assert_eq!((counts.pool, counts.global), (2, 3));
    assert_eq!(held_field(&store, &keys, "a", "p|b:x").await, 0);
    // The whole share does not fit while b holds 2, nor run alone.
    let (outcome, _) = store.slot_acquire(&keys, "a", &bg(8)).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Denied(SlotDenial::Pool));

    // b's heartbeat runs out: exactly its slots come back.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    store.replica_heartbeat_in(&keys, "a", ttl).await.unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert_eq!(
        store.replica_heartbeat_in(&keys, "a", ttl).await.unwrap(),
        1
    );
    assert_eq!(totals_field(&store, &keys, "p|b:x").await, 0);
    assert_eq!(totals_field(&store, &keys, "g").await, 2);
    // Alone now, the whole share runs although interactive holds 2.
    let whole = bg(8);
    let (outcome, counts) = store.slot_acquire(&keys, "a", &whole).await.unwrap();
    assert_eq!(outcome, AcquireOutcome::Granted);
    assert_eq!(counts.pool, 8);
    assert_books_balance(&store, &keys, &["a", "b"]).await;
    store
        .slot_release(&keys, "a", &release(&whole))
        .await
        .unwrap();
    assert_eq!(totals_field(&store, &keys, "p|b:x").await, 0);

    // A reconcile writes slots to the pool and requests to the rest.
    let tenant = Uuid::new_v4();
    let truth = vec![PoolHoldings {
        pool: "b:x".into(),
        in_flight: 2,
        slots: 7,
        tenants: vec![(tenant, 2)],
        keys: vec![(tenant, 2)],
    }];
    store.slot_reconcile(&keys, "a", &truth).await.unwrap();
    assert_eq!(totals_field(&store, &keys, "p|b:x").await, 7);
    assert_eq!(totals_field(&store, &keys, "g").await, 2);
    assert_books_balance(&store, &keys, &["a", "b"]).await;
    drop_test_keys(&store, &keys, &["a", "b"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// Two gateways' schedulers on one Redis: background requests holding
/// several slots each add up against the share cluster-wide, a waiter
/// gets in once enough slots are freed, and every slot comes back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gateways_add_up_multi_slot_background_holds_through_redis() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    let (a, la) = slot_gateway(&store, &keys, "a", 2, slot_config()).await;
    let (b, lb) = slot_gateway(&store, &keys, "b", 2, slot_config()).await;
    for fs in [&a, &b] {
        wait_for_slot_mode(fs, SlotMode::Shared).await;
    }
    let pool = obleth_fairshare::PoolKey::Background("shared-model".into());
    let field = format!("p|{}", pool.slot_id());
    let long = |slots| {
        obleth_fairshare::AdmitRequest::new(Uuid::new_v4(), "shared-model", 1)
            .model_cap(16)
            .background_share(0.5)
            .slots(slots)
    };
    let admit = |fs: &FairShare, req| {
        let (fs, pool) = (fs.clone(), pool.clone());
        tokio::spawn(async move { fs.admit_to(pool, req).await })
    };
    let in_time = |h: tokio::task::JoinHandle<Option<obleth_fairshare::Admitted>>| async {
        tokio::time::timeout(std::time::Duration::from_secs(3), h)
            .await
            .expect("admitted in time")
            .unwrap()
            .expect("scheduler alive")
    };
    let on_a = in_time(admit(&a, long(4))).await;
    let on_b = in_time(admit(&b, long(3))).await;
    assert_eq!((on_a.permit.slots(), on_b.permit.slots()), (4, 3));
    assert_eq!(totals_field(&store, &keys, &field).await, 7);
    assert_eq!(totals_field(&store, &keys, "g").await, 2);
    // b alone holds 3, but the share is 8 cluster-wide: 7 + 2 waits.
    let more = admit(&b, long(2));
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
    assert!(!more.is_finished(), "admitted past the share");
    drop(on_a);
    let more = in_time(more).await;
    assert_eq!(totals_field(&store, &keys, &field).await, 5);
    assert_eq!(held_field(&store, &keys, "b", &field).await, 5);
    drop((on_b, more));
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while totals_field(&store, &keys, &field).await != 0
        || totals_field(&store, &keys, "g").await != 0
    {
        assert!(tokio::time::Instant::now() < deadline, "released cleanly");
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_books_balance(&store, &keys, &["a", "b"]).await;
    la.abort();
    lb.abort();
    drop_test_keys(&store, &keys, &["a", "b"]).await;
}

fn slot_config() -> SharedSlotsConfig {
    SharedSlotsConfig {
        reconcile_interval: std::time::Duration::from_secs(60),
        recovery_interval: std::time::Duration::from_millis(100),
        // Far out, so a waiter admitted promptly proves the wakeup.
        retry_min: std::time::Duration::from_secs(30),
        retry_max: std::time::Duration::from_secs(30),
        ..SharedSlotsConfig::default()
    }
}

/// A gateway's scheduler on `store`, live under `instance` and listening
/// for release wakeups.
async fn slot_gateway(
    store: &RedisStore,
    keys: &SlotKeys,
    instance: &str,
    replicas: usize,
    config: SharedSlotsConfig,
) -> (FairShare, tokio::task::JoinHandle<()>) {
    live(store, keys, &[instance]).await;
    let fs = FairShare::start(
        std::sync::Arc::new(StaticCapacity::new(4096)),
        obleth_config::FairshareAlgorithm::Hierarchical,
        32,
    );
    fs.enable_shared_slots(
        std::sync::Arc::new(RedisSlots::with_keys(store.clone(), instance, keys.clone())),
        config,
    );
    fs.set_replicas(replicas);
    let (subscribed_tx, mut subscribed) = tokio::sync::mpsc::unbounded_channel::<()>();
    let listener = {
        let (store, keys, fs) = (store.clone(), keys.clone(), fs.clone());
        tokio::spawn(async move {
            let woken = fs.clone();
            store
                .run_slot_release_listener(
                    &keys,
                    move |pool| woken.wake(&pool),
                    move |reconnected| {
                        if reconnected {
                            fs.wake_all();
                        }
                        let _ = subscribed_tx.send(());
                    },
                )
                .await
        })
    };
    subscribed.recv().await.expect("subscribed");
    (fs, listener)
}

async fn wait_for_slot_mode(fs: &FairShare, mode: SlotMode) {
    let stats = fs.stats();
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while stats.mode() != mode {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("timed out waiting for {}", mode.as_str()));
}

fn model_req(cap: usize) -> obleth_fairshare::AdmitRequest {
    obleth_fairshare::AdmitRequest::new(Uuid::new_v4(), "shared-model", 1).model_cap(cap)
}

async fn admit_in_time(fs: &FairShare, cap: usize) -> obleth_fairshare::Admitted {
    tokio::time::timeout(std::time::Duration::from_secs(3), fs.admit(model_req(cap)))
        .await
        .expect("admitted in time")
        .expect("scheduler alive")
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// Two gateways on one Redis with session affinity: a looping session's
/// slot is held when its step ends on one gateway, its next step takes
/// it on the other, and another request gets the slot only after.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_held_slot_follows_its_session_across_gateways_through_redis() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    let config = SharedSlotsConfig {
        reconcile_interval: std::time::Duration::from_secs(60),
        ..SharedSlotsConfig::default()
    };
    let (a, la) = slot_gateway(&store, &keys, "a", 2, config.clone()).await;
    let (b, lb) = slot_gateway(&store, &keys, "b", 2, config).await;
    for fs in [&a, &b] {
        fs.set_session_affinity(Some(obleth_config::SessionAffinityConfig {
            warm_ttl: std::time::Duration::from_secs(120),
            hold: std::time::Duration::from_secs(3),
            max_jumps: 8,
            max_bypass: std::time::Duration::from_secs(30),
            max_held_percent: 100,
        }));
        wait_for_slot_mode(fs, SlotMode::Shared).await;
    }
    let agent = Uuid::new_v4();
    let step = || {
        obleth_fairshare::AdmitRequest::new(agent, "shared-model", 1)
            .model_cap(1)
            .session("loop")
    };
    let admit = |fs: &FairShare, req| {
        let fs = fs.clone();
        async move {
            tokio::time::timeout(std::time::Duration::from_secs(3), fs.admit(req))
                .await
                .expect("admitted in time")
                .expect("scheduler alive")
        }
    };
    drop(admit(&a, step()).await.permit);
    let second = admit(&a, step()).await;
    let other = {
        let b = b.clone();
        tokio::spawn(async move { b.admit(model_req(1)).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    drop(second.permit);
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(totals_field(&store, &keys, "h|m:shared-model").await, 1);
    assert!(!other.is_finished(), "the held slot is the session's");

    let third = admit(&b, step()).await;
    assert_eq!(totals_field(&store, &keys, "h|m:shared-model").await, 0);
    assert_eq!(held_field(&store, &keys, "b", "p|m:shared-model").await, 1);
    assert_eq!(
        b.stats()
            .affinity
            .redeemed
            .load(std::sync::atomic::Ordering::Relaxed),
        1
    );
    assert!(!other.is_finished());
    drop(third.permit);
    let granted = tokio::time::timeout(std::time::Duration::from_secs(5), other)
        .await
        .expect("granted once the session is done")
        .unwrap();
    drop(granted);
    la.abort();
    lb.abort();
    drop_test_keys(&store, &keys, &["a", "b"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// Three gateways on one Redis: all 20 requests for a 20-slot model land
/// on one gateway and are all admitted; a waiter on another gateway gets
/// the next slot freed, through the release wakeup.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gateways_share_a_pool_through_redis() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    let mut gws = Vec::new();
    for name in ["a", "b", "c"] {
        gws.push(slot_gateway(&store, &keys, name, 3, slot_config()).await);
    }
    for (fs, _) in &gws {
        wait_for_slot_mode(fs, SlotMode::Shared).await;
    }
    let pool = obleth_fairshare::PoolKey::Model("shared-model".into());
    let mut permits = Vec::new();
    for _ in 0..20 {
        permits.push(admit_in_time(&gws[0].0, 20).await);
    }
    assert_eq!(
        totals_field(&store, &keys, &format!("p|{}", pool.slot_id())).await,
        20
    );
    assert_eq!(
        gws[1]
            .0
            .cluster_in_flight(std::slice::from_ref(&pool))
            .await,
        Some((20, vec![20]))
    );

    let b = gws[1].0.clone();
    let waiter = tokio::spawn(async move { b.admit(model_req(20)).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!waiter.is_finished(), "the pool is full cluster-wide");
    let freed = std::time::Instant::now();
    permits.pop();
    let admitted = tokio::time::timeout(std::time::Duration::from_secs(3), waiter)
        .await
        .expect("woken by the release")
        .unwrap()
        .expect("admitted");
    assert!(
        freed.elapsed() < std::time::Duration::from_secs(3),
        "admitted by the wakeup, not the 30 s retry"
    );
    assert_eq!(
        held_field(&store, &keys, "b", &format!("p|{}", pool.slot_id())).await,
        1
    );
    drop(admitted);
    drop(permits);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while totals_field(&store, &keys, "g").await != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("every slot released");
    for (_, listener) in gws {
        listener.abort();
    }
    drop_test_keys(&store, &keys, &["a", "b", "c"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// Redis going away mid-traffic puts a gateway on the split; when it
/// comes back the gateway reconciles what it admitted meanwhile and goes
/// back to shared slots.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_redis_outage_falls_back_to_the_split_and_recovers() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let info = redis::Client::open(url.as_str())
        .unwrap()
        .get_connection_info()
        .clone();
    let redis::ConnectionAddr::Tcp(host, port) = info.addr.clone() else {
        eprintln!("skipping: the relay needs a TCP Redis URL");
        return;
    };
    let relay = Relay::start(format!("{host}:{port}")).await;
    let mut relayed_info = info.clone();
    relayed_info.addr = redis::ConnectionAddr::Tcp("127.0.0.1".into(), relay.port);
    let client = redis::Client::open(relayed_info).unwrap();
    let config = ConnectionManagerConfig::new()
        .set_response_timeout(std::time::Duration::from_millis(250))
        .set_connection_timeout(std::time::Duration::from_millis(500))
        .set_number_of_retries(0);
    let relayed = RedisStore {
        conn: relayed_connection(&client, config).await,
        client,
    };
    let direct = test_store().await.unwrap();
    let keys = replica_test_key();
    // b is another live gateway; a reaches Redis through the relay.
    live(&direct, &keys, &["b"]).await;
    let (fs, listener) = slot_gateway(&relayed, &keys, "a", 2, slot_config()).await;
    wait_for_slot_mode(&fs, SlotMode::Shared).await;
    let pool = format!(
        "p|{}",
        obleth_fairshare::PoolKey::Model("shared-model".into()).slot_id()
    );
    let mut permits = vec![admit_in_time(&fs, 10).await];
    assert_eq!(held_field(&direct, &keys, "a", &pool).await, 1);

    relay.down();
    let started = std::time::Instant::now();
    permits.push(admit_in_time(&fs, 10).await);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "a dead Redis cannot stall admission: {:?}",
        started.elapsed()
    );
    assert_eq!(fs.stats().mode(), SlotMode::Fallback);
    let snap = fs.snapshot().await.unwrap();
    assert_eq!(snap.mode, "fallback");
    assert_eq!(snap.pools[0].cap, 5, "ceil(10 / 2)");
    while permits.len() < 5 {
        permits.push(admit_in_time(&fs, 10).await);
    }
    let blocked = {
        let fs = fs.clone();
        tokio::spawn(async move { fs.admit(model_req(10)).await })
    };
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!blocked.is_finished(), "the split still bounds admission");

    relay.up().await;
    wait_for_slot_mode(&fs, SlotMode::Shared).await;
    let admitted = tokio::time::timeout(std::time::Duration::from_secs(3), blocked)
        .await
        .expect("admitted once shared again")
        .unwrap()
        .expect("admitted");
    assert_eq!(
        held_field(&direct, &keys, "a", &pool).await,
        6,
        "what the split admitted is on the books"
    );
    drop(admitted);
    drop(permits);
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while held_field(&direct, &keys, "a", &pool).await != 0 {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("releases recorded after recovery");
    listener.abort();
    relay.down();
    drop_test_keys(&direct, &keys, &["a", "b"]).await;
}

// ---- cluster view ------------------------------------------------------

fn one_pool_view(model: &str, in_flight: usize, queued: usize) -> obleth_fairshare::ReplicaView {
    obleth_fairshare::ReplicaView {
        version: obleth_fairshare::view::VIEW_VERSION,
        pools: vec![obleth_fairshare::ReplicaPoolView {
            pool: obleth_fairshare::PoolKey::Model(model.into()).slot_id(),
            configured_cap: 16,
            in_flight,
            queued,
            ..Default::default()
        }],
    }
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// A replica reads the views of the other live replicas only: not its
/// own, not a dead replica's while its view lingers, and not one that
/// deregistered.
#[tokio::test]
async fn peer_views_are_the_other_live_replicas() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    let ttl = std::time::Duration::from_millis(400);
    let view_ttl = std::time::Duration::from_secs(30);
    for i in ["a", "b", "c"] {
        store.replica_heartbeat_in(&keys, i, ttl).await.unwrap();
    }
    for (i, n) in [("a", 1), ("b", 2), ("c", 3)] {
        store
            .slot_publish_view(&keys, i, &one_pool_view("m", n, n * 10), view_ttl)
            .await
            .unwrap();
    }

    let peers = store.slot_peer_views(&keys, "a").await.unwrap().unwrap();
    assert_eq!(peers.live, 3);
    let seen: Vec<(&str, usize)> = peers
        .views
        .iter()
        .map(|p| (p.instance.as_str(), p.view.pools[0].in_flight))
        .collect();
    assert_eq!(seen, vec![("b", 2), ("c", 3)]);
    assert!(peers.views.iter().all(|p| p.age_ms < 5_000));

    // c deregisters: its view goes with it.
    store.replica_deregister_in(&keys, "c").await.unwrap();
    let mut conn = store.conn.clone();
    let gone: bool = conn.exists(keys.view("c")).await.unwrap();
    assert!(!gone, "deregistering drops the view");

    // b stops heartbeating: once it is no longer live its view is left
    // out, though the key has not expired.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    store.replica_heartbeat_in(&keys, "a", ttl).await.unwrap();
    let peers = store.slot_peer_views(&keys, "a").await.unwrap().unwrap();
    assert_eq!(peers.live, 1);
    assert!(peers.views.is_empty(), "{:?}", peers.views);
    drop_test_keys(&store, &keys, &["a", "b", "c"]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
/// Two schedulers on one Redis publish their views on their own, and
/// either one's cluster snapshot counts both.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn gateways_publish_views_and_merge_them_through_redis() {
    let Some(store) = test_store().await else {
        return;
    };
    let keys = replica_test_key();
    let config = SharedSlotsConfig {
        view_interval: std::time::Duration::from_millis(50),
        ..slot_config()
    };
    let (a, la) = slot_gateway(&store, &keys, "a", 2, config.clone()).await;
    let (b, lb) = slot_gateway(&store, &keys, "b", 2, config).await;
    wait_for_slot_mode(&a, SlotMode::Shared).await;
    wait_for_slot_mode(&b, SlotMode::Shared).await;
    let mut permits = vec![admit_in_time(&a, 10).await];
    for _ in 0..3 {
        permits.push(admit_in_time(&b, 10).await);
    }
    let merged = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if let Ok(c) = a.cluster_snapshot().await {
                let total: usize = c.pools.iter().map(|p| p.in_flight).sum();
                if c.replicas_reporting == 2 && total == 4 {
                    return c;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("both gateways' state merged");
    let pool = merged
        .pools
        .iter()
        .find(|p| p.model == "shared-model")
        .expect("the pool");
    assert_eq!((pool.in_flight, pool.cap), (4, 10));
    assert_eq!(pool.tenants.len(), 4);
    drop(permits);
    la.abort();
    lb.abort();
    drop_test_keys(&store, &keys, &["a", "b"]).await;
}
