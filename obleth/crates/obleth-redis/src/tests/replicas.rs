//! Connection timeouts and replica heartbeats.

use crate::test_util::*;
use crate::*;

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set (the
/// target address needs no Redis, but the test needs a routable network).
#[tokio::test]
async fn connect_to_blackholed_address_errors_within_connect_timeout() {
    if std::env::var("OBLETH_TEST_REDIS_URL").is_err() {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return;
    }
    let timeouts = RedisTimeouts {
        response: std::time::Duration::from_millis(250),
        connect: std::time::Duration::from_secs(2),
    };
    let started = std::time::Instant::now();
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        RedisStore::connect_with("redis://10.255.255.1:6379", timeouts),
    )
    .await
    .expect("connect must not hang past the connect timeout");
    let elapsed = started.elapsed();
    assert!(res.is_err(), "blackholed connect must error");
    assert!(
        elapsed < std::time::Duration::from_millis(2500),
        "took {elapsed:?}"
    );
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn response_timeout_is_applied_to_commands() {
    let Some(store) = test_store().await else {
        return;
    };
    // BLPOP on an empty list stalls the reply for 2 s, standing in for a
    // server that stops answering after the connection is established.
    let mut conn = store.conn.clone();
    let started = std::time::Instant::now();
    let res: redis::RedisResult<Option<(String, String)>> = redis::cmd("BLPOP")
        .arg(format!("obleth:test:blpop:{}", Uuid::new_v4()))
        .arg(2)
        .query_async(&mut conn)
        .await;
    let elapsed = started.elapsed();
    assert!(res.is_err(), "stalled command must time out, got {res:?}");
    assert!(
        elapsed < std::time::Duration::from_millis(1500),
        "took {elapsed:?}"
    );
}

#[tokio::test]
async fn replica_heartbeats_count_live_instances_and_expire_the_silent() {
    let Some(store) = test_store().await else {
        return;
    };
    let key = replica_test_key();
    let ttl = std::time::Duration::from_millis(400);

    assert_eq!(store.replica_heartbeat_in(&key, "a", ttl).await.unwrap(), 1);
    assert_eq!(store.replica_heartbeat_in(&key, "b", ttl).await.unwrap(), 2);
    // A refresh is not a second replica.
    assert_eq!(store.replica_heartbeat_in(&key, "a", ttl).await.unwrap(), 2);

    // b stops heartbeating (a crash): once its TTL passes it is no longer
    // counted, while a, still refreshing, is.
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert_eq!(store.replica_heartbeat_in(&key, "a", ttl).await.unwrap(), 2);
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    assert_eq!(
        store.replica_heartbeat_in(&key, "a", ttl).await.unwrap(),
        1,
        "b expired"
    );
    let mut conn = store.conn.clone();
    let members: Vec<String> = conn.zrange(&key.replicas, 0, -1).await.unwrap();
    assert_eq!(members, vec!["a".to_string()], "expired members are pruned");

    // The set itself never expires, so volatile-lru cannot evict it.
    let pttl: i64 = conn.pttl(&key.replicas).await.unwrap();
    assert_eq!(pttl, -1);

    drop_test_keys(&store, &key, &[]).await;
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn replica_deregister_drops_the_instance_at_once() {
    let Some(store) = test_store().await else {
        return;
    };
    let key = replica_test_key();
    let ttl = std::time::Duration::from_secs(30);
    store.replica_heartbeat_in(&key, "a", ttl).await.unwrap();
    assert_eq!(store.replica_heartbeat_in(&key, "b", ttl).await.unwrap(), 2);
    store.replica_deregister_in(&key, "a").await.unwrap();
    assert_eq!(store.replica_heartbeat_in(&key, "b", ttl).await.unwrap(), 1);
    // Deregistering an unknown instance is a no-op, not an error.
    store.replica_deregister_in(&key, "gone").await.unwrap();
    drop_test_keys(&store, &key, &[]).await;
}

/// The first heartbeat on a new connection, retried for the same reason.
async fn first_heartbeat(
    store: &RedisStore,
    keys: &SlotKeys,
    instance: &str,
    ttl: std::time::Duration,
) -> usize {
    for _ in 0..20 {
        if let Ok(n) = store.replica_heartbeat_in(keys, instance, ttl).await {
            return n;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("the first heartbeat never got through");
}

/// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
#[tokio::test]
async fn replica_heartbeat_fails_fast_while_redis_is_down_and_recovers() {
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
    let mut relayed = info.clone();
    relayed.addr = redis::ConnectionAddr::Tcp("127.0.0.1".into(), relay.port);
    let client = redis::Client::open(relayed).unwrap();
    let config = ConnectionManagerConfig::new()
        .set_response_timeout(std::time::Duration::from_millis(250))
        .set_connection_timeout(std::time::Duration::from_millis(500))
        .set_number_of_retries(0);
    let store = RedisStore {
        conn: relayed_connection(&client, config).await,
        client,
    };
    let key = replica_test_key();
    let ttl = std::time::Duration::from_secs(30);
    assert_eq!(first_heartbeat(&store, &key, "a", ttl).await, 1);

    relay.down();
    for _ in 0..3 {
        let started = std::time::Instant::now();
        let res = store.replica_heartbeat_in(&key, "a", ttl).await;
        assert!(res.is_err(), "heartbeat must fail while Redis is down");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(2),
            "a failed heartbeat must not hang: {:?}",
            started.elapsed()
        );
    }

    relay.up().await;
    let recovered = tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            if let Ok(n) = store.replica_heartbeat_in(&key, "a", ttl).await {
                return n;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("the heartbeat reconnects once Redis is back");
    assert_eq!(recovered, 1);

    let direct = test_store().await.unwrap();
    drop_test_keys(&direct, &key, &[]).await;
    relay.down();
}
