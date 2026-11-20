//! The resolver caches (`obleth:key3:*`, `obleth:model:*`, `obleth:mcp:*`) in
//! bulk, and the invalidation messages that tell every gateway to drop its
//! in-process copy of an entry.

use serde::Serialize;

use super::{
    RedisStore, Result, INVALIDATE_CHANNEL, KEY_PREFIX, LEGACY_KEY_PREFIXES, MCP_PREFIX,
    MODEL_PREFIX,
};

/// Commands per pipelined round trip in a bulk write. The connection is
/// shared with the request path, so a chunk is kept small enough not to hold
/// request commands behind it for long.
const PIPELINE_CHUNK: usize = 200;

/// One resolver namespace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Resolver {
    /// API keys by hash.
    Keys,
    /// Models by every name they answer to.
    Models,
    /// MCP servers by name.
    McpServers,
}

impl Resolver {
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            Resolver::Keys => KEY_PREFIX,
            Resolver::Models => MODEL_PREFIX,
            Resolver::McpServers => MCP_PREFIX,
        }
    }

    /// The message that drops entry `id` of this namespace.
    pub fn invalidation(self, id: &str) -> Invalidation {
        match self {
            Resolver::Keys => Invalidation::Key(id.to_string()),
            Resolver::Models => Invalidation::Model(id.to_string()),
            Resolver::McpServers => Invalidation::Mcp(id.to_string()),
        }
    }

    /// Every Redis key entry `id` may live under (an API key also under the
    /// namespaces of older releases, so a revoke reaches them too).
    fn redis_keys(self, id: &str) -> Vec<String> {
        let mut keys = vec![format!("{}{id}", self.prefix())];
        if self == Resolver::Keys {
            keys.extend(LEGACY_KEY_PREFIXES.iter().map(|p| format!("{p}{id}")));
        }
        keys
    }
}

/// What an invalidation message tells gateways to drop. The wire format is
/// fixed, since replicas of different versions share the channel: a key's
/// bare hash, `model:<name>`, `mcp:<name>`, or `*` for everything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Invalidation {
    Key(String),
    Model(String),
    Mcp(String),
    All,
}

impl Invalidation {
    pub fn encode(&self) -> String {
        match self {
            Invalidation::Key(hash) => hash.clone(),
            Invalidation::Model(name) => format!("model:{name}"),
            Invalidation::Mcp(name) => format!("mcp:{name}"),
            Invalidation::All => "*".to_string(),
        }
    }

    /// Read a message off the channel. Anything that is not `*` and has
    /// neither prefix is a key hash, as older releases send it.
    pub fn parse(message: &str) -> Self {
        if message == "*" {
            Invalidation::All
        } else if let Some(name) = message.strip_prefix("model:") {
            Invalidation::Model(name.to_string())
        } else if let Some(name) = message.strip_prefix("mcp:") {
            Invalidation::Mcp(name.to_string())
        } else {
            Invalidation::Key(message.to_string())
        }
    }
}

impl RedisStore {
    /// Publish one invalidation.
    pub async fn publish_invalidation(&self, target: &Invalidation) -> Result<()> {
        self.publish_invalidations(std::slice::from_ref(target))
            .await
    }

    /// Publish many invalidations, pipelined.
    pub async fn publish_invalidations(&self, targets: &[Invalidation]) -> Result<()> {
        let mut conn = self.conn.clone();
        for chunk in targets.chunks(PIPELINE_CHUNK) {
            let mut pipe = redis::pipe();
            for target in chunk {
                pipe.cmd("PUBLISH")
                    .arg(INVALIDATE_CHANNEL)
                    .arg(target.encode())
                    .ignore();
            }
            let _: () = pipe.query_async(&mut conn).await?;
        }
        Ok(())
    }

    /// Write many entries of one namespace, pipelined: a few round trips for
    /// thousands of keys instead of one each.
    pub async fn put_resolved_bulk<T: Serialize>(
        &self,
        resolver: Resolver,
        entries: &[(String, T)],
    ) -> Result<()> {
        let mut conn = self.conn.clone();
        for chunk in entries.chunks(PIPELINE_CHUNK) {
            let mut pipe = redis::pipe();
            for (id, value) in chunk {
                pipe.set(
                    format!("{}{id}", resolver.prefix()),
                    serde_json::to_string(value)?,
                )
                .ignore();
            }
            let _: () = pipe.query_async(&mut conn).await?;
        }
        Ok(())
    }

    /// Delete many entries of one namespace, pipelined.
    pub async fn delete_resolved_bulk(&self, resolver: Resolver, ids: &[String]) -> Result<()> {
        let mut conn = self.conn.clone();
        for chunk in ids.chunks(PIPELINE_CHUNK) {
            let keys: Vec<String> = chunk
                .iter()
                .flat_map(|id| resolver.redis_keys(id))
                .collect();
            let _: () = redis::cmd("DEL").arg(keys).query_async(&mut conn).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalidations_keep_their_wire_format() {
        let cases = [
            (Invalidation::Key("ab12".into()), "ab12"),
            (Invalidation::Model("qwen3".into()), "model:qwen3"),
            (Invalidation::Model("a:b".into()), "model:a:b"),
            (Invalidation::Mcp("search".into()), "mcp:search"),
            (Invalidation::All, "*"),
        ];
        for (target, wire) in cases {
            assert_eq!(target.encode(), wire);
            assert_eq!(Invalidation::parse(wire), target);
        }
    }

    #[test]
    fn a_key_delete_reaches_the_older_namespaces() {
        assert_eq!(
            Resolver::Keys.redis_keys("h"),
            vec!["obleth:key3:h", "obleth:key:h", "obleth:key2:h"]
        );
        assert_eq!(Resolver::Models.redis_keys("m"), vec!["obleth:model:m"]);
    }

    /// 10,000 keys go in and out in chunks of [`PIPELINE_CHUNK`] commands,
    /// 50 round trips each way rather than 10,000. Integration test; runs
    /// only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn ten_thousand_keys_push_and_delete_in_bulk() {
        use redis::AsyncCommands;
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let store = RedisStore::connect(&url).await.expect("connect");
        let tag = uuid::Uuid::new_v4();
        let entries: Vec<(String, serde_json::Value)> = (0..10_000)
            .map(|i| (format!("bulk-{tag}-{i}"), serde_json::json!({ "n": i })))
            .collect();
        assert_eq!(entries.len().div_ceil(PIPELINE_CHUNK), 50);

        store
            .put_resolved_bulk(Resolver::Keys, &entries)
            .await
            .unwrap();
        let mut conn = store.conn.clone();
        let last: Option<String> = conn
            .get(format!("{KEY_PREFIX}bulk-{tag}-9999"))
            .await
            .unwrap();
        assert_eq!(last.as_deref(), Some(r#"{"n":9999}"#));

        let ids: Vec<String> = entries.into_iter().map(|(id, _)| id).collect();
        store
            .delete_resolved_bulk(Resolver::Keys, &ids)
            .await
            .unwrap();
        let left: usize = conn
            .exists(
                ids.iter()
                    .map(|id| format!("{KEY_PREFIX}{id}"))
                    .collect::<Vec<_>>(),
            )
            .await
            .unwrap();
        assert_eq!(left, 0);
    }
}
