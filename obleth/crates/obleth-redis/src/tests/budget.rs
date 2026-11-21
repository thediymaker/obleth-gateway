//! Per-minute buckets, term budgets and daily counters.

use crate::test_util::*;
use crate::*;

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn budget_reserve_and_reconcile() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let store = RedisStore::connect(&url).await.expect("connect");
    let tenant = Uuid::new_v4();
    let capacity = 100i64;
    let tpm = 0i64; // no refill, so accounting is deterministic

    // first reserve of 60 succeeds, leaving 40
    let out = store
        .reserve_with_term(&tenant, capacity, tpm, 60, None, 0.0)
        .await
        .unwrap();
    assert_eq!(out, ReserveOutcome::Reserved { remaining: 40 });

    // The bucket is stamped with the Redis clock, not this process's.
    let mut conn = store.conn.clone();
    let ts: i64 = conn
        .hget(RedisStore::budget_key(&tenant), "ts")
        .await
        .unwrap();
    let (secs, micros): (i64, i64) = redis::cmd("TIME").query_async(&mut conn).await.unwrap();
    let redis_now = secs * 1000 + micros / 1000;
    assert!(
        (redis_now - ts).abs() < 5_000,
        "ts {ts} vs Redis {redis_now}"
    );

    // 50 more exceeds the remaining 40 -> denied
    let out = store
        .reserve_with_term(&tenant, capacity, tpm, 50, None, 0.0)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::RateLimited { .. }));

    // reconcile: estimated 60 but actual 10 -> refund 50, back to 90
    let after = store
        .reconcile_budget(&tenant, capacity, 60, 10)
        .await
        .unwrap();
    assert_eq!(after, 90);
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn combined_reserve_gates_on_term_budget_first() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let store = RedisStore::connect(&url).await.expect("connect");
    let tenant = Uuid::new_v4();
    let capacity = 100i64;
    let tpm = 0i64; // no refill, so accounting is deterministic

    // No term gate: behaves like a plain reserve.
    let out = store
        .reserve_with_term(&tenant, capacity, tpm, 60, None, 0.0)
        .await
        .unwrap();
    assert_eq!(out, ReserveOutcome::Reserved { remaining: 40 });

    // Bucket exhausted -> RateLimited, nothing reserved.
    let out = store
        .reserve_with_term(&tenant, capacity, tpm, 50, None, 0.0)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::RateLimited { .. }));

    // Accumulate term usage, then a gate below that usage must reject
    // WITHOUT touching the bucket.
    store
        .term_usage_add(&tenant, "l:0", 500, 1.25)
        .await
        .unwrap();
    let gate = TermGate {
        period_key: "l:0",
        budget_tokens: Some(500),
        budget_cost_usd: None,
    };
    let out = store
        .reserve_with_term(&tenant, capacity, tpm, 10, Some(gate), 0.0)
        .await
        .unwrap();
    assert_eq!(
        out,
        ReserveOutcome::TermExhausted {
            used_tokens: 500,
            used_cost: 1.25
        }
    );
    // Bucket untouched by the term rejection: 40 tokens still reservable.
    let out = store
        .reserve_with_term(&tenant, capacity, tpm, 40, None, 0.0)
        .await
        .unwrap();
    assert_eq!(out, ReserveOutcome::Reserved { remaining: 0 });

    // A roomy term gate passes through to the bucket (now empty).
    let gate = TermGate {
        period_key: "l:0",
        budget_tokens: Some(1_000_000),
        budget_cost_usd: Some(100.0),
    };
    let out = store
        .reserve_with_term(&tenant, capacity, tpm, 10, Some(gate), 0.0)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::RateLimited { .. }));

    // A different period key rolls the counters: gate opens again.
    let gate = TermGate {
        period_key: "m:2026-06",
        budget_tokens: Some(500),
        budget_cost_usd: None,
    };
    let out = store
        .reserve_with_term(&Uuid::new_v4(), capacity, tpm, 10, Some(gate), 0.0)
        .await
        .unwrap();
    assert_eq!(out, ReserveOutcome::Reserved { remaining: 90 });
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn zero_capacity_means_no_per_minute_token_cap() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let store = RedisStore::connect(&url).await.expect("connect");
    let tenant = Uuid::new_v4();

    let out = store
        .reserve_with_term(&tenant, 0, 0, 1_000_000, None, 0.0)
        .await
        .unwrap();
    assert_eq!(out, ReserveOutcome::Reserved { remaining: 0 });

    store
        .term_usage_add(&tenant, "l:0", 500, 1.25)
        .await
        .unwrap();
    let gate = TermGate {
        period_key: "l:0",
        budget_tokens: Some(500),
        budget_cost_usd: None,
    };
    let out = store
        .reserve_with_term(&tenant, 0, 0, 10, Some(gate), 0.0)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::TermExhausted { .. }));
}

async fn term_field(store: &RedisStore, scope: &Uuid, field: &str) -> Option<String> {
    let mut conn = store.conn.clone();
    conn.hget(RedisStore::term_usage_key(scope), field)
        .await
        .unwrap()
}

async fn reserved_field(store: &RedisStore, scope: &Uuid, field: &str) -> Option<String> {
    let mut conn = store.conn.clone();
    conn.hget(RedisStore::term_reserved_key(scope), field)
        .await
        .unwrap()
}

/// Stand-in for the 10-minute TTL running out on a never-settled request.
async fn expire_reservations_now(store: &RedisStore, scope: &Uuid) {
    let mut conn = store.conn.clone();
    let _: i64 = redis::cmd("PEXPIRE")
        .arg(RedisStore::term_reserved_key(scope))
        .arg(1)
        .query_async(&mut conn)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn reservation_ttl_is_set_on_creation_and_never_refreshed() {
    let Some(store) = test_store().await else {
        return;
    };
    let scope = Uuid::new_v4();
    let rkey = RedisStore::term_reserved_key(&scope);
    store
        .reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.0)
        .await
        .unwrap();
    let mut conn = store.conn.clone();
    let ttl: i64 = conn.pttl(&rkey).await.unwrap();
    assert!(ttl > 590_000 && ttl <= 600_000, "PTTL={ttl}");

    // Stand-in for ~5 minutes passing (t0 + delta) without sleeping.
    let _: i64 = redis::cmd("PEXPIRE")
        .arg(&rkey)
        .arg(300_000)
        .query_async(&mut conn)
        .await
        .unwrap();
    // A later reserve in the same (busy) scope must not re-arm the TTL,
    // or a leaked reservation would never expire.
    store
        .reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.0)
        .await
        .unwrap();
    let ttl: i64 = conn.pttl(&rkey).await.unwrap();
    assert!(ttl > 0 && ttl <= 300_000, "PTTL={ttl}");
    assert_eq!(
        reserved_field(&store, &scope, "tokens").await.as_deref(),
        Some("20")
    );
    // Committed usage has no TTL at all, independent of reservations: it
    // is budget state, and a TTL would make it evictable under
    // volatile-lru.
    let ttl: i64 = redis::cmd("PTTL")
        .arg(RedisStore::term_usage_key(&scope))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(ttl, -1, "committed term usage must not expire");
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn daily_counters_add_up_and_expire() {
    let Some(store) = test_store().await else {
        return;
    };
    let id = Uuid::new_v4().to_string();
    store
        .bump_daily("test", &id, "2026-09-27", &[("calls", 1), ("ms", 40)])
        .await
        .unwrap();
    store
        .bump_daily(
            "test",
            &id,
            "2026-09-27",
            &[("calls", 1), ("ms", 60), ("errors", 1)],
        )
        .await
        .unwrap();
    store
        .bump_daily("test", &id, "2026-09-28", &[("calls", 3)])
        .await
        .unwrap();
    let days = [
        "2026-09-26".to_string(),
        "2026-09-27".to_string(),
        "2026-09-28".to_string(),
    ];
    let read = store
        .read_daily("test", &[id.clone(), "nobody".into()], &days)
        .await
        .unwrap();
    let mine = &read[&id];
    assert_eq!(mine.len(), 2, "a day with nothing recorded is left out");
    assert_eq!(mine[0].0, "2026-09-27");
    assert_eq!(mine[0].1["calls"], 2);
    assert_eq!(mine[0].1["ms"], 100);
    assert_eq!(mine[0].1["errors"], 1);
    assert_eq!(mine[1].1["calls"], 3);
    assert!(read["nobody"].is_empty());
    let mut conn = store.conn.clone();
    let ttl: i64 = redis::cmd("TTL")
        .arg(RedisStore::daily_stats_key("test", &id, "2026-09-27"))
        .query_async(&mut conn)
        .await
        .unwrap();
    assert!(
        ttl > 0 && ttl <= RedisStore::DAILY_STATS_TTL_SECS,
        "counters must expire, got {ttl}"
    );
}

#[tokio::test]
async fn committed_term_usage_never_expires_and_drops_a_legacy_ttl() {
    let Some(store) = test_store().await else {
        return;
    };
    let scope = Uuid::new_v4();
    let key = RedisStore::term_usage_key(&scope);
    let mut conn = store.conn.clone();

    store.term_usage_add(&scope, "l:0", 10, 0.5).await.unwrap();
    let ttl: i64 = redis::cmd("PTTL")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(ttl, -1, "add must leave committed usage without a TTL");

    // A key written by an earlier version carries a 1-year TTL; the next
    // write must clear it, not refresh it.
    let _: i64 = redis::cmd("PEXPIRE")
        .arg(&key)
        .arg(31_536_000_000_i64)
        .query_async(&mut conn)
        .await
        .unwrap();
    store.term_usage_add(&scope, "l:0", 5, 0.0).await.unwrap();
    let ttl: i64 = redis::cmd("PTTL")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(ttl, -1, "a legacy TTL must be cleared on the next add");
    assert_eq!(store.term_usage_peek(&scope, "l:0").await.unwrap().0, 15);

    // A scope that is only ever gated (no new usage) is cleared too.
    let _: i64 = redis::cmd("PEXPIRE")
        .arg(&key)
        .arg(31_536_000_000_i64)
        .query_async(&mut conn)
        .await
        .unwrap();
    store
        .reserve_with_term(&scope, 0, 0, 1, Some(token_gate("l:0", 100)), 0.0)
        .await
        .unwrap();
    let ttl: i64 = redis::cmd("PTTL")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert_eq!(ttl, -1, "a legacy TTL must be cleared by the gate");

    let _: () = conn.del(&key).await.unwrap();
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn leaked_reservation_expires_and_stops_blocking_the_scope() {
    let Some(store) = test_store().await else {
        return;
    };
    let scope = Uuid::new_v4();
    store.term_usage_add(&scope, "l:0", 50, 0.0).await.unwrap();
    // Reserve 50 and never settle (crash / SIGKILL): the scope is at cap.
    store
        .reserve_with_term(&scope, 0, 0, 50, Some(token_gate("l:0", 100)), 0.0)
        .await
        .unwrap();
    let out = store
        .reserve_with_term(&scope, 0, 0, 1, Some(token_gate("l:0", 100)), 0.0)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::TermExhausted { .. }));

    expire_reservations_now(&store, &scope).await;

    // Reserved reads as 0 again; only the committed 50 counts.
    let out = store
        .reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.0)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::Reserved { .. }));
    assert_eq!(
        reserved_field(&store, &scope, "tokens").await.as_deref(),
        Some("10")
    );
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn reconcile_after_reservation_expiry_still_commits_actual() {
    let Some(store) = test_store().await else {
        return;
    };
    let scope = Uuid::new_v4();
    store
        .reserve_with_term(&scope, 0, 0, 40, Some(token_gate("l:0", 100)), 0.4)
        .await
        .unwrap();
    expire_reservations_now(&store, &scope).await;

    let committed = store
        .term_reconcile(&scope, "l:0", 40, 0.4, 25, 0.3)
        .await
        .unwrap();
    assert_eq!(committed, (25, 0.3));
    // The release did not resurrect the expired key.
    assert_eq!(reserved_field(&store, &scope, "tokens").await, None);
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn reservation_from_a_previous_period_is_discarded() {
    let Some(store) = test_store().await else {
        return;
    };
    let scope = Uuid::new_v4();
    store
        .reserve_with_term(&scope, 0, 0, 90, Some(token_gate("m:2026-08", 100)), 0.0)
        .await
        .unwrap();
    let out = store
        .reserve_with_term(&scope, 0, 0, 5, Some(token_gate("m:2026-09", 100)), 0.0)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::Reserved { .. }));
    assert_eq!(
        reserved_field(&store, &scope, "tokens").await.as_deref(),
        Some("5")
    );
}

fn token_gate(period_key: &str, cap: i64) -> TermGate<'_> {
    TermGate {
        period_key,
        budget_tokens: Some(cap),
        budget_cost_usd: None,
    }
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn term_reservation_counts_toward_the_cap() {
    let Some(store) = test_store().await else {
        return;
    };
    let scope = Uuid::new_v4();
    store.term_usage_add(&scope, "l:0", 50, 0.0).await.unwrap();

    // 50 committed + 0 reserved < 100: admitted, reserves 40.
    let out = store
        .reserve_with_term(&scope, 0, 0, 40, Some(token_gate("l:0", 100)), 0.4)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::Reserved { .. }));
    assert_eq!(
        reserved_field(&store, &scope, "tokens").await.as_deref(),
        Some("40")
    );
    // 50 + 40 < 100: admitted, reserves 10 more.
    let out = store
        .reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.1)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::Reserved { .. }));
    // 50 + 50 >= 100: rejected although only 50 is committed.
    let out = store
        .reserve_with_term(&scope, 0, 0, 1, Some(token_gate("l:0", 100)), 0.0)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::TermExhausted { .. }));
    let reserved_cost: f64 = reserved_field(&store, &scope, "cost")
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert!((reserved_cost - 0.5).abs() < 1e-9, "{reserved_cost}");

    // Reserved cost counts toward a cost cap the same way.
    let cost_scope = Uuid::new_v4();
    let gate = || TermGate {
        period_key: "l:0",
        budget_tokens: None,
        budget_cost_usd: Some(1.0),
    };
    for expect_ok in [true, true, false] {
        let out = store
            .reserve_with_term(&cost_scope, 0, 0, 1, Some(gate()), 0.75)
            .await
            .unwrap();
        assert_eq!(
            matches!(out, ReserveOutcome::Reserved { .. }),
            expect_ok,
            "{out:?}"
        );
    }
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn rate_limited_requests_reserve_no_term_budget() {
    let Some(store) = test_store().await else {
        return;
    };
    let scope = Uuid::new_v4();
    let out = store
        .reserve_with_term(&scope, 10, 0, 5, Some(token_gate("l:0", 1000)), 0.0)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::Reserved { .. }));
    // 8 does not fit the 5 left in the bucket.
    let out = store
        .reserve_with_term(&scope, 10, 0, 8, Some(token_gate("l:0", 1000)), 0.0)
        .await
        .unwrap();
    assert!(matches!(out, ReserveOutcome::RateLimited { .. }));
    assert_eq!(
        reserved_field(&store, &scope, "tokens").await.as_deref(),
        Some("5")
    );
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn term_reconcile_moves_reservation_to_committed() {
    let Some(store) = test_store().await else {
        return;
    };
    let scope = Uuid::new_v4();
    store
        .reserve_with_term(&scope, 0, 0, 40, Some(token_gate("l:0", 100)), 0.4)
        .await
        .unwrap();
    let committed = store
        .term_reconcile(&scope, "l:0", 40, 0.4, 25, 0.3)
        .await
        .unwrap();
    assert_eq!(committed, (25, 0.3));
    assert_eq!(
        store.term_usage_peek(&scope, "l:0").await.unwrap(),
        committed
    );
    assert_eq!(
        reserved_field(&store, &scope, "tokens").await.as_deref(),
        Some("0")
    );
    let reserved_cost: f64 = reserved_field(&store, &scope, "cost")
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(reserved_cost, 0.0);

    // Releasing more than is reserved floors at zero instead of going
    // negative (a negative reservation would inflate headroom).
    let (tokens, cost) = store
        .term_reconcile(&scope, "l:0", 1_000, 5.0, 5, 0.05)
        .await
        .unwrap();
    assert_eq!(tokens, 30);
    assert!((cost - 0.35).abs() < 1e-9, "{cost}");
    assert_eq!(
        reserved_field(&store, &scope, "tokens").await.as_deref(),
        Some("0")
    );
    let reserved_cost: f64 = reserved_field(&store, &scope, "cost")
        .await
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(reserved_cost, 0.0);
    let (tokens, cost) = store.term_usage_peek(&scope, "l:0").await.unwrap();
    assert_eq!(tokens, 30);
    assert!((cost - 0.35).abs() < 1e-9, "{cost}");
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn term_reconcile_after_period_roll_keeps_the_new_period() {
    let Some(store) = test_store().await else {
        return;
    };
    let scope = Uuid::new_v4();
    store
        .reserve_with_term(&scope, 0, 0, 40, Some(token_gate("m:2026-08", 100)), 0.0)
        .await
        .unwrap();
    // Another request rolls the counters to the next period and reserves.
    store
        .reserve_with_term(&scope, 0, 0, 7, Some(token_gate("m:2026-09", 100)), 0.0)
        .await
        .unwrap();
    // The straggler from the old period settles: its usage is counted,
    // but it must neither wipe the new period nor release the new
    // period's reservation.
    let committed = store
        .term_reconcile(&scope, "m:2026-08", 40, 0.0, 30, 0.0)
        .await
        .unwrap();
    assert_eq!(committed, (30, 0.0));
    assert_eq!(
        term_field(&store, &scope, "period").await.as_deref(),
        Some("m:2026-09")
    );
    assert_eq!(
        reserved_field(&store, &scope, "tokens").await.as_deref(),
        Some("7")
    );
    assert_eq!(
        term_field(&store, &scope, "tokens").await.as_deref(),
        Some("30")
    );
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn concurrent_reservations_at_cap_minus_one_admit_only_one() {
    let Some(store) = test_store().await else {
        return;
    };
    let scope = Uuid::new_v4();
    store.term_usage_add(&scope, "l:0", 99, 0.0).await.unwrap();
    let (ra, rb) = tokio::join!(
        store.reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.0),
        store.reserve_with_term(&scope, 0, 0, 10, Some(token_gate("l:0", 100)), 0.0),
    );
    let outcomes = [ra.unwrap(), rb.unwrap()];
    let admitted = outcomes
        .iter()
        .filter(|o| matches!(o, ReserveOutcome::Reserved { .. }))
        .count();
    let rejected = outcomes
        .iter()
        .filter(|o| matches!(o, ReserveOutcome::TermExhausted { .. }))
        .count();
    assert_eq!((admitted, rejected), (1, 1), "{outcomes:?}");
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn reconcile_that_recreates_the_bucket_sets_ttl_and_ts() {
    let Some(store) = test_store().await else {
        return;
    };
    let tenant = Uuid::new_v4();
    // Bucket expired between reserve and reconcile.
    let remaining = store.reconcile_budget(&tenant, 100, 10, 30).await.unwrap();
    assert_eq!(remaining, 80);
    let mut conn = store.conn.clone();
    let key = RedisStore::budget_key(&tenant);
    let ttl: i64 = redis::cmd("PTTL")
        .arg(&key)
        .query_async(&mut conn)
        .await
        .unwrap();
    assert!(ttl > 0, "recreated bucket must expire, PTTL={ttl}");
    let ts: Option<i64> = conn.hget(&key, "ts").await.unwrap();
    assert!(ts.is_some(), "recreated bucket must be re-seeded with ts");
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn estimate_larger_than_capacity_is_admitted_against_a_full_bucket() {
    let Some(store) = test_store().await else {
        return;
    };
    let (capacity, tpm) = (100i64, 0i64);

    let reserve = |tenant: Uuid, n: u32| {
        let store = store.clone();
        async move {
            store
                .reserve_with_term(&tenant, capacity, tpm, n, None, 0.0)
                .await
                .unwrap()
        }
    };

    let tenant = Uuid::new_v4();
    assert_eq!(
        reserve(tenant, 150).await,
        ReserveOutcome::Reserved { remaining: -50 },
        "oversized request against a full bucket must pass"
    );
    // Bucket now in debt: the next request waits for refill.
    assert!(matches!(
        reserve(tenant, 150).await,
        ReserveOutcome::RateLimited { .. }
    ));

    // The debt floor (-capacity) still holds.
    assert_eq!(
        reserve(Uuid::new_v4(), 10_000).await,
        ReserveOutcome::Reserved {
            remaining: -capacity
        }
    );

    // A partially drained bucket still rejects the oversized request.
    let tenant = Uuid::new_v4();
    reserve(tenant, 1).await;
    assert!(matches!(
        reserve(tenant, 150).await,
        ReserveOutcome::RateLimited { .. }
    ));
}
