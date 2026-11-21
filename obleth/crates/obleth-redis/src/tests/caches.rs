//! The resolver, response, compression, vision, crash-guard and query-vector
//! caches.

use crate::test_util::*;
use crate::*;
use obleth_config::ResolvedKey;

/// An older replica prunes `obleth:key:*` (or `obleth:key2:*`) against
/// its own snapshot. The current namespace must not match those
/// patterns, or an older replica still running in a rollout would delete
/// every entry a newer one wrote.
#[test]
fn the_key_namespace_is_outside_the_legacy_scan_pattern() {
    for legacy in LEGACY_KEY_PREFIXES {
        assert!(!KEY_PREFIX.starts_with(legacy));
        assert_ne!(KEY_PREFIX, legacy);
    }
    assert!(RedisStore::key_cache("abc").starts_with(KEY_PREFIX));
}

#[tokio::test]
async fn resolved_key_cache_roundtrip() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let _serial = RESOLVER_TEST_LOCK.lock().await;
    let store = RedisStore::connect(&url).await.expect("connect");
    let hash = format!("test-{}", Uuid::new_v4());
    let key = ResolvedKey {
        key_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        tenant_name: "t".into(),
        key_name: "k".into(),
        fairshare_group: "default".into(),
        group_weight: 100,
        weight: 7,
        tokens_per_minute: 1000,
        max_in_flight: None,
        disabled: false,
        status: "active".into(),
        timezone: "UTC".into(),
        active_from: None,
        active_until: None,
        weekly_windows: None,
        budget_tokens: None,
        budget_cost_usd: None,
        budget_period: None,
        budget_started_at: None,
        key_budget_tokens: None,
        key_budget_cost_usd: None,
        key_budget_period: None,
        key_budget_started_at: None,
        key_weight: 100,
        key_max_in_flight: None,
        end_user_fairshare: false,
        background: false,
        allowed_models: None,
        internal: false,
        tracing_enabled: false,
        guardrails_policy: None,
        compression_policy: None,
        synthetic: false,
        allowed_networks: Vec::new(),
        allowed_countries: Vec::new(),
    };
    store.put_resolved_key(&hash, &key).await.unwrap();
    let got = store.get_resolved_key(&hash).await.unwrap().unwrap();
    assert_eq!(got, key);
    store.delete_resolved_key(&hash).await.unwrap();
    assert!(store.get_resolved_key(&hash).await.unwrap().is_none());
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn compress_store_roundtrip_and_miss() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let store = RedisStore::connect(&url).await.expect("connect");
    let tenant = Uuid::new_v4();
    let hash = format!("h-{}", Uuid::new_v4());

    // Miss before write.
    assert!(store.compress_get(&tenant, &hash).await.unwrap().is_none());

    // Round-trip with a TTL.
    store
        .compress_put(&tenant, &hash, "the original content", 60)
        .await
        .unwrap();
    assert_eq!(
        store.compress_get(&tenant, &hash).await.unwrap().as_deref(),
        Some("the original content")
    );

    // Several at once, in one round trip, each under its own hash and
    // with the TTL; nothing for a TTL of 0.
    let many: Vec<(String, String)> = (0..3)
        .map(|n| (format!("{hash}-{n}"), format!("original {n}")))
        .collect();
    store.compress_put_many(&tenant, &many, 60).await.unwrap();
    for (h, content) in &many {
        assert_eq!(
            store.compress_get(&tenant, h).await.unwrap().as_deref(),
            Some(content.as_str())
        );
    }
    let unkept = vec![(format!("{hash}-unkept"), "gone".to_owned())];
    store.compress_put_many(&tenant, &unkept, 0).await.unwrap();
    assert!(store
        .compress_get(&tenant, &unkept[0].0)
        .await
        .unwrap()
        .is_none());
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn a_strike_lists_its_model_until_the_record_expires() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let store = RedisStore::connect(&url).await.expect("connect");
    let (lasting, brief) = (
        format!("m-{}", Uuid::new_v4().simple()),
        format!("m-{}", Uuid::new_v4().simple()),
    );
    let window = std::time::Duration::from_secs(3600);
    store
        .crash_strike(&lasting, "fp", window, std::time::Duration::from_secs(600))
        .await
        .unwrap();
    store
        .crash_strike(&brief, "fp", window, std::time::Duration::from_millis(1))
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    let recent = store.crash_recent_models().await.unwrap();
    let left = recent.iter().find(|(m, _)| *m == lasting).map(|(_, l)| *l);
    assert!(
        left.is_some_and(|l| l > std::time::Duration::from_secs(590)),
        "{recent:?}"
    );
    assert!(
        !recent.iter().any(|(m, _)| *m == brief),
        "an expired record is not listed"
    );
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn compacted_forms_round_trip_per_tenant() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let store = RedisStore::connect(&url).await.expect("connect");
    let (tenant, other) = (Uuid::new_v4(), Uuid::new_v4());
    let ids: Vec<String> = (0..3).map(|n| format!("v1:0.5:h{n}")).collect();
    assert_eq!(
        store.compacted_get_many(&tenant, &ids).await.unwrap(),
        vec![None, None, None]
    );
    let kept = vec![
        (ids[0].clone(), "short 0".to_owned()),
        (ids[2].clone(), "short 2".to_owned()),
    ];
    store.compacted_put_many(&tenant, &kept, 60).await.unwrap();
    assert_eq!(
        store.compacted_get_many(&tenant, &ids).await.unwrap(),
        vec![Some("short 0".to_owned()), None, Some("short 2".to_owned())]
    );
    assert_eq!(
        store.compacted_get_many(&other, &ids[..1]).await.unwrap(),
        vec![None],
        "another tenant's forms are not found"
    );
    assert!(store
        .compacted_get_many(&tenant, &[])
        .await
        .unwrap()
        .is_empty());
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn compress_store_is_scoped_to_the_tenant() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let store = RedisStore::connect(&url).await.expect("connect");
    let (owner, other) = (Uuid::new_v4(), Uuid::new_v4());
    let hash = format!("h-{}", Uuid::new_v4());
    store
        .compress_put(&owner, &hash, "tenant secret", 60)
        .await
        .unwrap();
    assert!(
        store.compress_get(&other, &hash).await.unwrap().is_none(),
        "another tenant must not retrieve an original by its hash"
    );
    let mut conn = store.conn.clone();
    let exists: bool = conn
        .exists(format!("{COMPRESS_PREFIX}{owner}:{hash}"))
        .await
        .unwrap();
    assert!(exists, "stored under obleth:compress:<tenant>:<sha>");
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn an_image_description_is_kept_for_its_tenant_only() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let store = RedisStore::connect(&url).await.expect("connect");
    let (owner, other) = (Uuid::new_v4(), Uuid::new_v4());
    let hash = format!("h-{}", Uuid::new_v4());
    assert!(store.vision_get(&owner, &hash).await.unwrap().is_none());
    store
        .vision_put(&owner, &hash, "a red error toast", 60)
        .await
        .unwrap();
    assert_eq!(
        store.vision_get(&owner, &hash).await.unwrap().as_deref(),
        Some("a red error toast")
    );
    assert!(
        store.vision_get(&other, &hash).await.unwrap().is_none(),
        "another tenant must not read a description by its hash"
    );
    let unkept = format!("h-{}", Uuid::new_v4());
    store.vision_put(&owner, &unkept, "x", 0).await.unwrap();
    assert!(store.vision_get(&owner, &unkept).await.unwrap().is_none());
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn zero_ttl_never_writes() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let store = RedisStore::connect(&url).await.expect("connect");
    let tenant = Uuid::new_v4();
    let hash = format!("h-{}", Uuid::new_v4());
    store
        .compress_put(&tenant, &hash, "content", 0)
        .await
        .unwrap();
    assert!(store.compress_get(&tenant, &hash).await.unwrap().is_none());

    let key = format!("k-{}", Uuid::new_v4());
    let value = CachedResponse {
        status: 200,
        content_type: "application/json".into(),
        body: "{}".into(),
        input_tokens: 1,
        output_tokens: 1,
    };
    store.cache_put(&key, &value, 0).await.unwrap();
    assert!(store.cache_get(&key).await.unwrap().is_none());

    // A positive TTL still stores, and the entry expires.
    store.cache_put(&key, &value, 60).await.unwrap();
    assert!(store.cache_get(&key).await.unwrap().is_some());
    let mut conn = store.conn.clone();
    let ttl: i64 = conn.ttl(format!("{CACHE_PREFIX}{key}")).await.unwrap();
    assert!(ttl > 0 && ttl <= 60, "entry must expire, got ttl {ttl}");
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn query_vector_roundtrip_and_miss() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let store = RedisStore::connect(&url).await.expect("connect");
    let hash = format!("q-{}", Uuid::new_v4());

    // Miss before write.
    assert!(store.get_query_vector(&hash).await.unwrap().is_none());

    let vector = vec![0.5f32, -1.25, 3.0, 0.0];
    store.put_query_vector(&hash, &vector, 60).await.unwrap();
    assert_eq!(store.get_query_vector(&hash).await.unwrap(), Some(vector));
}

/// A misaligned byte length must be treated as a miss, not a decode
/// error: a stray truncated write must never score a request against a
/// silently shifted vector.
#[tokio::test]
async fn query_vector_misaligned_bytes_is_a_miss() {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let store = RedisStore::connect(&url).await.expect("connect");
    let hash = format!("q-bad-{}", Uuid::new_v4());
    let redis_key = RedisStore::query_vector_key(&hash);
    let mut conn = store.conn.clone();
    let _: () = conn.set(redis_key, vec![1u8, 2, 3]).await.unwrap();

    assert!(store.get_query_vector(&hash).await.unwrap().is_none());
}
