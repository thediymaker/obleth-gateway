//! Reconciling the resolver caches against Postgres.
//!
//! Resolver entries (`obleth:key:*`, `obleth:model:*`, `obleth:mcp:*`) carry no
//! TTL, so an entry whose eviction was missed (a Redis blip during a delete)
//! would keep resolving forever. These helpers SCAN a namespace and delete every
//! entry the caller's authoritative set does not list.

use std::collections::HashSet;

use super::{RedisStore, Result, KEY_PREFIX, MCP_PREFIX, MODEL_PREFIX};

/// Keys fetched per SCAN round-trip and deleted per DEL.
const SCAN_BATCH: usize = 500;

impl RedisStore {
    /// Delete every resolved-key entry whose hash is not in `known_hashes`.
    /// Returns the pruned hashes so the caller can publish invalidations.
    pub async fn prune_stale_resolved_keys(
        &self,
        known_hashes: &HashSet<String>,
    ) -> Result<Vec<String>> {
        self.prune_namespace(KEY_PREFIX, known_hashes).await
    }

    /// Delete every resolved-model entry whose name is not in `known_names`
    /// (canonical names and aliases of the models that should resolve).
    pub async fn prune_stale_resolved_models(
        &self,
        known_names: &HashSet<String>,
    ) -> Result<Vec<String>> {
        self.prune_namespace(MODEL_PREFIX, known_names).await
    }

    /// Delete every resolved-MCP-server entry whose name is not in `known_names`.
    pub async fn prune_stale_resolved_mcp_servers(
        &self,
        known_names: &HashSet<String>,
    ) -> Result<Vec<String>> {
        self.prune_namespace(MCP_PREFIX, known_names).await
    }

    async fn prune_namespace(&self, prefix: &str, known: &HashSet<String>) -> Result<Vec<String>> {
        let mut conn = self.conn.clone();
        let pattern = format!("{prefix}*");
        let mut stale_keys: Vec<String> = Vec::new();
        // Explicit cursor loop rather than `scan_match` so a mid-scan error
        // surfaces instead of silently ending the iteration.
        let mut cursor: u64 = 0;
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(&pattern)
                .arg("COUNT")
                .arg(SCAN_BATCH)
                .query_async(&mut conn)
                .await?;
            stale_keys.extend(keys.into_iter().filter(|k| {
                k.strip_prefix(prefix)
                    .is_some_and(|suffix| !known.contains(suffix))
            }));
            if next == 0 {
                break;
            }
            cursor = next;
        }
        // SCAN may return a key more than once; DEL is idempotent but the
        // returned list should not repeat.
        stale_keys.sort();
        stale_keys.dedup();
        for chunk in stale_keys.chunks(SCAN_BATCH) {
            let _: () = redis::cmd("DEL").arg(chunk).query_async(&mut conn).await?;
        }
        Ok(stale_keys
            .into_iter()
            .filter_map(|k| k.strip_prefix(prefix).map(str::to_string))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use redis::AsyncCommands;

    #[derive(Clone, Copy)]
    enum Namespace {
        Keys,
        Models,
        McpServers,
    }

    impl Namespace {
        fn prefix(self) -> &'static str {
            match self {
                Namespace::Keys => KEY_PREFIX,
                Namespace::Models => MODEL_PREFIX,
                Namespace::McpServers => MCP_PREFIX,
            }
        }

        async fn prune(self, store: &RedisStore, known: &HashSet<String>) -> Vec<String> {
            match self {
                Namespace::Keys => store.prune_stale_resolved_keys(known).await,
                Namespace::Models => store.prune_stale_resolved_models(known).await,
                Namespace::McpServers => store.prune_stale_resolved_mcp_servers(known).await,
            }
            .unwrap()
        }
    }

    /// Seed one kept and one stale entry in `ns`, prune with every other entry
    /// already present treated as known (so the test never deletes data it
    /// did not create), and check only the stale one went.
    async fn prune_removes_only_the_unlisted_entry(store: &RedisStore, ns: Namespace) {
        let prefix = ns.prefix();
        let mut conn = store.conn.clone();
        let keep = format!("prune-keep-{}", uuid::Uuid::new_v4());
        let stale = format!("prune-stale-{}", uuid::Uuid::new_v4());
        for name in [&keep, &stale] {
            let _: () = conn.set(format!("{prefix}{name}"), "{}").await.unwrap();
        }

        let mut known: HashSet<String> = HashSet::new();
        let mut cursor: u64 = 0;
        loop {
            let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                .arg(cursor)
                .arg("MATCH")
                .arg(format!("{prefix}*"))
                .query_async(&mut conn)
                .await
                .unwrap();
            known.extend(
                keys.iter()
                    .filter_map(|k| k.strip_prefix(prefix).map(str::to_string)),
            );
            if next == 0 {
                break;
            }
            cursor = next;
        }
        known.remove(&stale);

        let pruned = ns.prune(store, &known).await;
        assert_eq!(pruned, vec![stale.clone()], "{prefix}");
        let exists = |name: &str| {
            let mut conn = store.conn.clone();
            let key = format!("{prefix}{name}");
            async move { conn.exists::<_, bool>(key).await.unwrap() }
        };
        assert!(!exists(&stale).await, "{prefix}");
        assert!(exists(&keep).await, "{prefix}");

        let _: () = conn.del(format!("{prefix}{keep}")).await.unwrap();
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn prune_removes_only_unlisted_resolved_keys() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        prune_removes_only_the_unlisted_entry(&store, Namespace::Keys).await;
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn prune_removes_only_unlisted_models_and_mcp_servers() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        prune_removes_only_the_unlisted_entry(&store, Namespace::Models).await;
        prune_removes_only_the_unlisted_entry(&store, Namespace::McpServers).await;
    }
}
