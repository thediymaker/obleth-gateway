//! Helpers shared by the crate's Redis integration tests.

use crate::*;

/// Held by every test that writes resolver entries (`obleth:key3:*`,
/// `obleth:model:*`, `obleth:mcp:*`) in the shared test Redis: the prune tests
/// delete whatever those namespaces hold that they did not list, so a write
/// from a test running alongside would be pruned, or counted as stale.
pub(crate) static RESOLVER_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub(crate) async fn test_store() -> Option<RedisStore> {
    let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
        return None;
    };
    Some(RedisStore::connect(&url).await.expect("connect"))
}

pub(crate) fn replica_test_key() -> SlotKeys {
    SlotKeys::with_prefix(&format!("obleth:test:{}", Uuid::new_v4()))
}

pub(crate) async fn drop_test_keys(store: &RedisStore, keys: &SlotKeys, instances: &[&str]) {
    let mut conn = store.conn.clone();
    let mut all = vec![
        keys.replicas.clone(),
        keys.totals.clone(),
        keys.holds.clone(),
        keys.recent.clone(),
        keys.chain.clone(),
    ];
    all.extend(instances.iter().map(|i| keys.held(i)));
    all.extend(instances.iter().map(|i| keys.view(i)));
    let _: () = conn.del(all).await.unwrap();
}

/// A TCP relay to the test Redis that can be taken down and brought back
/// on the same port, standing in for a Redis outage after boot.
pub(crate) struct Relay {
    pub(crate) port: u16,
    target: String,
    tasks: std::sync::Arc<std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl Relay {
    pub(crate) async fn start(target: String) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let relay = Relay {
            port,
            target,
            tasks: Default::default(),
        };
        relay.serve(listener);
        relay
    }

    fn serve(&self, listener: tokio::net::TcpListener) {
        let target = self.target.clone();
        let tasks = self.tasks.clone();
        let accept = tokio::spawn(async move {
            while let Ok((mut inbound, _)) = listener.accept().await {
                let target = target.clone();
                let conn = tokio::spawn(async move {
                    if let Ok(mut outbound) = tokio::net::TcpStream::connect(target).await {
                        let _ = tokio::io::copy_bidirectional(&mut inbound, &mut outbound).await;
                    }
                });
                tasks.lock().unwrap().push(conn);
            }
        });
        self.tasks.lock().unwrap().push(accept);
    }

    /// Close the listener and every relayed connection.
    pub(crate) fn down(&self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }

    pub(crate) async fn up(&self) {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", self.port))
            .await
            .unwrap();
        self.serve(listener);
    }
}

/// The first connection through a fresh relay, retried: with the whole
/// suite running in parallel the first round trip through a relay can
/// miss a sub-second timeout. What these tests check starts after it.
pub(crate) async fn relayed_connection(
    client: &redis::Client,
    config: ConnectionManagerConfig,
) -> ConnectionManager {
    for _ in 0..20 {
        if let Ok(conn) = ConnectionManager::new_with_config(client.clone(), config.clone()).await {
            return conn;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    panic!("could not connect through the relay");
}
