//! Reconciling the resolver caches against Postgres.
//!
//! Resolver entries (`obleth:key3:*`, `obleth:model:*`, `obleth:mcp:*`) carry no
//! TTL, so an entry whose eviction was missed (a Redis blip during a delete)
//! would keep resolving forever. These helpers SCAN the keyspace once and delete
//! every entry the caller's authoritative sets do not list.

use std::collections::{HashMap, HashSet};

use super::{RedisStore, Resolver, Result};

const SCAN_BATCH: usize = 500;

impl RedisStore {
    pub async fn prune_stale_resolved_keys(
        &self,
        known_hashes: &HashSet<String>,
    ) -> Result<Vec<String>> {
        self.prune_one(Resolver::Keys, known_hashes).await
    }

    pub async fn prune_stale_resolved_models(
        &self,
        known_names: &HashSet<String>,
    ) -> Result<Vec<String>> {
        self.prune_one(Resolver::Models, known_names).await
    }

    pub async fn prune_stale_resolved_mcp_servers(
        &self,
        known_names: &HashSet<String>,
    ) -> Result<Vec<String>> {
        self.prune_one(Resolver::McpServers, known_names).await
    }

    async fn prune_one(&self, resolver: Resolver, known: &HashSet<String>) -> Result<Vec<String>> {
        let mut pruned = self.prune_stale_resolved(&[(resolver, known)]).await?;
        Ok(pruned.remove(&resolver).unwrap_or_default())
    }

    /// Delete every entry of the given namespaces that its `known` set does
    /// not list, in one SCAN of the keyspace however many namespaces are
    /// asked for (each SCAN walks the whole keyspace; `MATCH` only filters
    /// what it returns). Returns the ids deleted, per namespace.
    pub async fn prune_stale_resolved(
        &self,
        known: &[(Resolver, &HashSet<String>)],
    ) -> Result<HashMap<Resolver, Vec<String>>> {
        let mut conn = self.conn.clone();
        let pattern = format!("{}*", common_prefix(known.iter().map(|(r, _)| r.prefix())));
        let mut stale: HashMap<Resolver, Vec<String>> = HashMap::new();
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
            for key in keys {
                for (resolver, listed) in known {
                    if let Some(id) = key.strip_prefix(resolver.prefix()) {
                        if !listed.contains(id) {
                            stale.entry(*resolver).or_default().push(id.to_string());
                        }
                    }
                }
            }
            if next == 0 {
                break;
            }
            cursor = next;
        }
        for (resolver, ids) in &mut stale {
            ids.sort();
            ids.dedup();
            let keys: Vec<String> = ids
                .iter()
                .map(|id| format!("{}{id}", resolver.prefix()))
                .collect();
            for chunk in keys.chunks(SCAN_BATCH) {
                let _: () = redis::cmd("DEL").arg(chunk).query_async(&mut conn).await?;
            }
        }
        Ok(stale)
    }
}

/// The longest prefix every one of `prefixes` starts with.
fn common_prefix<'a>(mut prefixes: impl Iterator<Item = &'a str>) -> &'a str {
    let Some(first) = prefixes.next() else {
        return "";
    };
    prefixes.fold(first, |common, p| {
        let len = common
            .bytes()
            .zip(p.bytes())
            .take_while(|(a, b)| a == b)
            .count();
        &common[..len]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{KEY_PREFIX, MCP_PREFIX, MODEL_PREFIX};
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
        let _serial = crate::RESOLVER_TEST_LOCK.lock().await;
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
        let _serial = crate::RESOLVER_TEST_LOCK.lock().await;
        let store = RedisStore::connect(&url).await.expect("connect");
        prune_removes_only_the_unlisted_entry(&store, Namespace::Models).await;
        prune_removes_only_the_unlisted_entry(&store, Namespace::McpServers).await;
    }

    #[test]
    fn the_scan_pattern_is_the_namespaces_common_prefix() {
        assert_eq!(common_prefix([KEY_PREFIX].into_iter()), KEY_PREFIX);
        assert_eq!(
            common_prefix([KEY_PREFIX, MODEL_PREFIX, MCP_PREFIX].into_iter()),
            "obleth:"
        );
        assert_eq!(
            common_prefix([MODEL_PREFIX, MCP_PREFIX].into_iter()),
            "obleth:m"
        );
    }

    /// One prune over all three namespaces removes each one's unlisted entry
    /// and keeps the listed ones, and a known set only judges its own
    /// namespace. Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn one_scan_prunes_every_namespace() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let _serial = crate::RESOLVER_TEST_LOCK.lock().await;
        let store = RedisStore::connect(&url).await.expect("connect");
        let mut conn = store.conn.clone();
        let resolvers = [Resolver::Keys, Resolver::Models, Resolver::McpServers];
        let tag = uuid::Uuid::new_v4();
        let mut known: HashMap<Resolver, HashSet<String>> = HashMap::new();
        for r in resolvers {
            for name in ["keep", "stale"] {
                let _: () = conn
                    .set(format!("{}{name}-{tag}", r.prefix()), "{}")
                    .await
                    .unwrap();
            }
        }
        // Everything else already in each namespace is listed too, so only
        // this test's `stale` entries are unlisted.
        for r in resolvers {
            let mut listed = HashSet::new();
            let mut cursor: u64 = 0;
            loop {
                let (next, keys): (u64, Vec<String>) = redis::cmd("SCAN")
                    .arg(cursor)
                    .arg("MATCH")
                    .arg(format!("{}*", r.prefix()))
                    .query_async(&mut conn)
                    .await
                    .unwrap();
                listed.extend(
                    keys.iter()
                        .filter_map(|k| k.strip_prefix(r.prefix()).map(str::to_string)),
                );
                if next == 0 {
                    break;
                }
                cursor = next;
            }
            listed.remove(&format!("stale-{tag}"));
            known.insert(r, listed);
        }
        let asked: Vec<(Resolver, &HashSet<String>)> =
            resolvers.iter().map(|r| (*r, &known[r])).collect();

        let pruned = store.prune_stale_resolved(&asked).await.unwrap();

        for r in resolvers {
            assert_eq!(pruned[&r], vec![format!("stale-{tag}")], "{r:?}");
            let kept: bool = conn
                .exists(format!("{}keep-{tag}", r.prefix()))
                .await
                .unwrap();
            assert!(kept, "{r:?}");
            let _: () = conn.del(format!("{}keep-{tag}", r.prefix())).await.unwrap();
        }
    }
}
