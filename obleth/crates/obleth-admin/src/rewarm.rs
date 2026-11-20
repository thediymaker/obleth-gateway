//! Push the data plane's resolver namespaces (keys, models, MCP servers)
//! from Postgres into Redis, prune what Postgres no longer lists, and settle
//! what the prune caught mid-change. One routine for the gateway's periodic
//! re-warm and the admin resync (`POST /api/v1/resync`).
//!
//! Each namespace is written pipelined; all three are pruned in one SCAN; a
//! namespace whose load or push failed is left out of the prune, so a failed
//! push never prunes entries it did not write.

use std::collections::{HashMap, HashSet};

use obleth_config::{ResolvedKey, ResolvedMcpServer, ResolvedModel};
use obleth_redis::{Invalidation, RedisStore, Resolver};
use obleth_store::{Store, StoreError};

/// How one namespace ended up.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pushed {
    /// Ids (key hashes, model names, server names) in Redis after the pass.
    pub ids: Vec<String>,
    /// For models, how many models those names belong to; otherwise the
    /// number of ids.
    pub rows: usize,
    /// Ids evicted: pruned and confirmed gone, or deleted mid-pass.
    pub evicted: Vec<String>,
}

/// A namespace's pass: `Err` (already logged) when it did not run, in which
/// case the next pass starts it clean.
pub type Outcome = Result<Pushed, String>;

#[derive(Debug)]
pub struct Pass {
    pub keys: Outcome,
    pub models: Outcome,
    pub mcp: Outcome,
}

/// When a pass reads Postgres again after its prune.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reread {
    /// Only when the prune removed something: then an entry created
    /// mid-pass may have been pruned. The periodic re-warm, which runs again
    /// soon anyway.
    WhenPruned,
    /// Always, so an entry deleted after the snapshot (and so pushed again
    /// from its old row) is evicted now. The admin resync.
    Always,
}

/// One pass over all three namespaces.
pub async fn push_and_prune(store: &Store, redis: &RedisStore, reread: Reread) -> Pass {
    let keys = push(redis, Resolver::Keys, store.all_resolved_keys().await).await;
    let models = store.all_resolved_models().await;
    let model_rows = models.as_ref().map_or(0, Vec::len);
    let models = push(redis, Resolver::Models, models.map(models_by_name)).await;
    let mcp = push(
        redis,
        Resolver::McpServers,
        store.all_resolved_mcp_servers().await,
    )
    .await;
    let [keys, mut models, mcp] = prune_and_settle(
        redis,
        reread,
        (keys, || store.all_resolved_keys()),
        (models, || async {
            store.all_resolved_models().await.map(models_by_name)
        }),
        (mcp, || store.all_resolved_mcp_servers()),
    )
    .await;
    if let Ok(m) = &mut models {
        m.rows = model_rows;
    }
    Pass { keys, models, mcp }
}

/// A namespace's entries as Postgres has them (or why they are not), and
/// how to read them again.
type Step<T, F> = (Result<Vec<(String, T)>, String>, F);

type Reloaded<T> = Result<Vec<(String, T)>, StoreError>;

/// Write `loaded` into `resolver`'s namespace, pipelined.
async fn push<T: serde::Serialize>(
    redis: &RedisStore,
    resolver: Resolver,
    loaded: Reloaded<T>,
) -> Result<Vec<(String, T)>, String> {
    let entries = loaded.map_err(|e| {
        tracing::warn!(error = %e, ?resolver, "redis re-warm: load failed");
        format!("reading {resolver:?} from Postgres failed: {e}")
    })?;
    redis
        .put_resolved_bulk(resolver, &entries)
        .await
        .map_err(|e| {
            tracing::warn!(error = %e, ?resolver, "redis re-warm: push failed; retrying next pass");
            format!("writing {resolver:?} to Redis failed: {e}")
        })?;
    Ok(entries)
}

/// Prune every pushed namespace in one SCAN, then settle each.
async fn prune_and_settle<FK, FKF, FM, FMF, FS, FSF>(
    redis: &RedisStore,
    reread: Reread,
    keys: Step<ResolvedKey, FK>,
    models: Step<ResolvedModel, FM>,
    mcp: Step<ResolvedMcpServer, FS>,
) -> [Outcome; 3]
where
    FK: FnOnce() -> FKF,
    FKF: std::future::Future<Output = Reloaded<ResolvedKey>>,
    FM: FnOnce() -> FMF,
    FMF: std::future::Future<Output = Reloaded<ResolvedModel>>,
    FS: FnOnce() -> FSF,
    FSF: std::future::Future<Output = Reloaded<ResolvedMcpServer>>,
{
    let known = [known_ids(&keys.0), known_ids(&models.0), known_ids(&mcp.0)];
    let resolvers = [Resolver::Keys, Resolver::Models, Resolver::McpServers];
    let asked: Vec<(Resolver, &HashSet<String>)> = resolvers
        .iter()
        .zip(&known)
        .filter_map(|(r, k)| k.as_ref().map(|k| (*r, k)))
        .collect();
    let mut pruned = if asked.is_empty() {
        HashMap::new()
    } else {
        match redis.prune_stale_resolved(&asked).await {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "redis re-warm: prune failed; retrying next pass");
                let failed = || Err(format!("pruning Redis failed: {e}"));
                return [failed(), failed(), failed()];
            }
        }
    };
    let mut take = |r: Resolver| pruned.remove(&r).unwrap_or_default();
    let pruned = [
        take(Resolver::Keys),
        take(Resolver::Models),
        take(Resolver::McpServers),
    ];
    let [known_keys, known_models, known_mcp] = known;
    let [pruned_keys, pruned_models, pruned_mcp] = pruned;
    [
        settle(redis, reread, Resolver::Keys, known_keys, keys, pruned_keys).await,
        settle(
            redis,
            reread,
            Resolver::Models,
            known_models,
            models,
            pruned_models,
        )
        .await,
        settle(
            redis,
            reread,
            Resolver::McpServers,
            known_mcp,
            mcp,
            pruned_mcp,
        )
        .await,
    ]
}

/// The ids a pushed namespace lists; `None` when it was not pushed.
fn known_ids<T>(entries: &Result<Vec<(String, T)>, String>) -> Option<HashSet<String>> {
    entries
        .as_ref()
        .ok()
        .map(|e| e.iter().map(|(id, _)| id.clone()).collect())
}

async fn settle<T, F, Fut>(
    redis: &RedisStore,
    reread: Reread,
    resolver: Resolver,
    known: Option<HashSet<String>>,
    (pushed, reload): Step<T, F>,
    pruned: Vec<String>,
) -> Outcome
where
    T: serde::Serialize,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Reloaded<T>>,
{
    let pushed = pushed?;
    let known = known.unwrap_or_default();
    let pushed_ids = pushed.into_iter().map(|(id, _)| id).collect();
    Ok(settle_pruned(redis, reread, resolver, &known, pruned, pushed_ids, reload).await)
}

/// After a prune: an entry created between the snapshot load and the SCAN
/// was not in the snapshot, so the prune deleted it as stale and every
/// replica would miss it until the next pass. When the prune removed
/// anything, read Postgres again; restore what it still has, and evict what
/// it does not (including a `known` entry deleted mid-pass, which the prune
/// left alone because it still matched). A failed re-read is only logged:
/// the push and prune already landed, and the next pass repeats this.
async fn settle_pruned<T, F, Fut>(
    redis: &RedisStore,
    reread: Reread,
    resolver: Resolver,
    known: &HashSet<String>,
    pruned: Vec<String>,
    pushed: Vec<String>,
    reload: F,
) -> Pushed
where
    T: serde::Serialize,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Reloaded<T>>,
{
    let done = |ids: Vec<String>, evicted: Vec<String>| Pushed {
        rows: ids.len(),
        ids,
        evicted,
    };
    if pruned.is_empty() && reread == Reread::WhenPruned {
        return done(pushed, Vec::new());
    }
    let invalidate = |ids: &[String]| -> Vec<Invalidation> {
        ids.iter().map(|id| resolver.invalidation(id)).collect()
    };
    let fresh = match reload().await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, ?resolver, "redis re-warm: re-read after prune failed; a wrongly pruned entry may be missing until the next pass");
            let _ = redis.publish_invalidations(&invalidate(&pruned)).await;
            return done(pushed, pruned);
        }
    };
    let pruned: HashSet<String> = pruned.into_iter().collect();
    let fresh_by_id: HashMap<&str, &T> = fresh.iter().map(|(id, v)| (id.as_str(), v)).collect();
    let fresh_ids: HashSet<&str> = fresh_by_id.keys().copied().collect();
    let (restore, evict) = restore_and_evict(known, &pruned, &fresh_ids);
    let restored: Vec<(String, &T)> = restore
        .into_iter()
        .filter_map(|id| fresh_by_id.get(id.as_str()).map(|v| (id, *v)))
        .collect();
    match redis.put_resolved_bulk(resolver, &restored).await {
        Ok(()) => {
            let ids: Vec<String> = restored.into_iter().map(|(id, _)| id).collect();
            let _ = redis.publish_invalidations(&invalidate(&ids)).await;
        }
        Err(e) => {
            tracing::warn!(error = %e, ?resolver, "redis re-warm: restoring wrongly pruned entries failed")
        }
    }
    let _ = redis.delete_resolved_bulk(resolver, &evict).await;
    let _ = redis.publish_invalidations(&invalidate(&evict)).await;
    done(fresh.into_iter().map(|(id, _)| id).collect(), evict)
}

/// From a snapshot's `known` identifiers (hashes or names) and what the
/// SCAN-based prune actually deleted (`pruned`), decide what a fresh
/// post-prune reload should restore vs. evict. An identifier still present in
/// `fresh` is restored — the prune deleted a live entry, most likely one
/// created between the snapshot load and the SCAN. Anything still absent is
/// evicted, which covers both a `pruned` identifier confirmed gone for good
/// and a `known` identifier deleted mid-pass: the prune itself leaves the
/// latter alone (it still matched `known` at SCAN time), so this is the only
/// place that catches it.
fn restore_and_evict(
    known: &HashSet<String>,
    pruned: &HashSet<String>,
    fresh: &HashSet<&str>,
) -> (Vec<String>, Vec<String>) {
    let restore = pruned
        .iter()
        .filter(|id| fresh.contains(id.as_str()))
        .cloned()
        .collect();
    let evict = pruned
        .iter()
        .chain(known.iter())
        .filter(|id| !fresh.contains(id.as_str()))
        .cloned()
        .collect();
    (restore, evict)
}

/// Resolved models keyed by every addressable name (canonical name, aliases
/// and variants). A variant's key holds its parent's route as is; the proxy
/// adds the variant's boons per request.
pub fn models_by_name(models: Vec<(String, ResolvedModel)>) -> Vec<(String, ResolvedModel)> {
    models
        .into_iter()
        .flat_map(|(_, m)| {
            m.addressable_names()
                .map(|n| (n.to_string(), m.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pure, always-on coverage of the restore/evict decision itself — the
    /// actual logic behind both I1 (a revoked key must not come back) and N1
    /// (a key created mid-pass must not be wrongly evicted). This is the
    /// primary proof: it needs no Redis and can't leave any shared state
    /// behind, unlike the SCAN-based prune the integration test below drives
    /// for real (see that test's doc comment for why it's careful about it).
    #[test]
    fn restore_and_evict_keeps_a_mid_pass_create_and_drops_a_genuine_revocation() {
        use std::collections::HashSet;
        let known: HashSet<String> = ["kept", "deleted-mid-pass"]
            .into_iter()
            .map(String::from)
            .collect();
        // `pruned` is what the SCAN removed: `created-mid-pass` (wrongly —
        // it's not in `known` only because the snapshot predates it) and
        // `revoked` (correctly — Postgres dropped it before the snapshot).
        let pruned: HashSet<String> = ["created-mid-pass", "revoked"]
            .into_iter()
            .map(String::from)
            .collect();
        // The post-prune reload: `kept` still there, `created-mid-pass` now
        // visible, `deleted-mid-pass` gone (an admin delete that landed after
        // the snapshot but that the prune couldn't see, since it still
        // matched `known`), `revoked` still gone.
        let fresh: HashSet<&str> = ["kept", "created-mid-pass"].into_iter().collect();

        let (mut restore, mut evict) = restore_and_evict(&known, &pruned, &fresh);
        restore.sort();
        evict.sort();
        assert_eq!(restore, vec!["created-mid-pass".to_string()]);
        assert_eq!(
            evict,
            vec!["deleted-mid-pass".to_string(), "revoked".to_string()]
        );
    }

    #[test]
    fn restore_and_evict_is_empty_when_the_prune_removed_nothing() {
        use std::collections::HashSet;
        let known: HashSet<String> = ["kept"].into_iter().map(String::from).collect();
        let pruned: HashSet<String> = HashSet::new();
        let fresh: HashSet<&str> = ["kept"].into_iter().collect();
        assert_eq!(
            restore_and_evict(&known, &pruned, &fresh),
            (Vec::new(), Vec::new())
        );
    }

    /// Integration smoke test for the real Redis plumbing behind both
    /// scenarios `restore_and_evict_keeps_a_mid_pass_create_and_drops_a_genuine_revocation`
    /// covers in isolation above.
    ///
    /// `prune_and_settle` calls `RedisStore::prune_stale_resolved`, which
    /// SCANs and deletes across the *entire* `obleth:key3:*` namespace —
    /// not just the hashes this test creates. Point `OBLETH_TEST_REDIS_URL`
    /// at an isolated database (e.g. Redis db 15, the project's documented
    /// test database — never db 0 / a shared or live instance): running this
    /// against a real deployment's default database deletes its live
    /// resolved-key cache. Both scenarios are folded into one `#[tokio::test]`
    /// (rather than two, as originally written) because two such tests
    /// running in parallel — the default for `cargo test` — race the same
    /// SCAN and can prune each other's freshly-written keys.
    #[tokio::test]
    async fn push_prune_and_settle_keys_integration() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let redis = obleth_redis::RedisStore::connect(&url)
            .await
            .expect("connect");

        // Scenario 1 (I1): revoked before the snapshot was loaded, absent
        // from both the snapshot and the post-prune reload — stays evicted.
        let revoked_hash = format!("test-rewarm-{}", uuid::Uuid::new_v4());
        redis
            .put_resolved_key(&revoked_hash, &test_resolved_key())
            .await
            .unwrap();

        // Scenario 2 (N1): created between the snapshot load and the SCAN —
        // already in Redis (the admin create path pushes synchronously) but
        // missing from the snapshot; the post-prune reload shows it, so it
        // must be restored.
        let created_hash = format!("test-rewarm-{}", uuid::Uuid::new_v4());
        let created_key = test_resolved_key();
        redis
            .put_resolved_key(&created_hash, &created_key)
            .await
            .unwrap();

        let kept_hash = format!("test-rewarm-{}", uuid::Uuid::new_v4());
        let kept_key = test_resolved_key();
        redis.put_resolved_key(&kept_hash, &kept_key).await.unwrap();

        // The pass's own snapshot predates the create: only `kept` and
        // (already-gone-from-Postgres) nothing for `revoked_hash`.
        let snapshot = vec![(kept_hash.clone(), kept_key.clone())];
        // The post-prune reload runs later: Postgres now also has `created`,
        // and still doesn't have `revoked`.
        let reread = vec![
            (kept_hash.clone(), kept_key),
            (created_hash.clone(), created_key),
        ];

        let pushed = push(&redis, Resolver::Keys, Ok(snapshot)).await;
        assert!(pushed.is_ok());
        let counts = prune_and_settle(
            &redis,
            Reread::WhenPruned,
            (pushed, move || async move { Ok(reread) }),
            (Err("not this pass".into()), || async { Ok(Vec::new()) }),
            (Err("not this pass".into()), || async { Ok(Vec::new()) }),
        )
        .await;
        let ran: Vec<Option<usize>> = counts
            .iter()
            .map(|o| o.as_ref().ok().map(|p| p.ids.len()))
            .collect();
        assert_eq!(ran, [Some(2), None, None], "only the keys ran");
        let keys = counts[0].as_ref().unwrap();
        assert!(keys.evicted.contains(&revoked_hash));
        assert!(
            redis
                .get_resolved_key(&revoked_hash)
                .await
                .unwrap()
                .is_none(),
            "a key revoked before the snapshot must stay evicted"
        );
        assert!(
            redis
                .get_resolved_key(&created_hash)
                .await
                .unwrap()
                .is_some(),
            "a key created during the pass must survive it"
        );
        assert!(redis.get_resolved_key(&kept_hash).await.unwrap().is_some());

        redis.delete_resolved_key(&created_hash).await.unwrap();
        redis.delete_resolved_key(&kept_hash).await.unwrap();
    }

    /// An admin resync always reads Postgres again, so a key deleted after
    /// its snapshot (and pushed again from the old row) is evicted even when
    /// the prune removed nothing; the periodic re-warm leaves that to its
    /// next pass.
    #[tokio::test]
    async fn a_resync_evicts_what_was_deleted_mid_pass() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let redis = RedisStore::connect(&url).await.expect("connect");
        let gone = format!("test-resync-{}", uuid::Uuid::new_v4());
        let pushed = vec![gone.clone()];
        let known: HashSet<String> = pushed.iter().cloned().collect();
        redis
            .put_resolved_key(&gone, &test_resolved_key())
            .await
            .unwrap();

        let reread = || async { Ok(Vec::<(String, ResolvedKey)>::new()) };
        let periodic = settle_pruned(
            &redis,
            Reread::WhenPruned,
            Resolver::Keys,
            &known,
            Vec::new(),
            pushed.clone(),
            reread,
        )
        .await;
        assert!(periodic.evicted.is_empty());
        assert!(redis.get_resolved_key(&gone).await.unwrap().is_some());

        let resync = settle_pruned(
            &redis,
            Reread::Always,
            Resolver::Keys,
            &known,
            Vec::new(),
            pushed,
            reread,
        )
        .await;
        assert_eq!((resync.ids.len(), resync.evicted), (0, vec![gone.clone()]));
        assert!(redis.get_resolved_key(&gone).await.unwrap().is_none());
    }

    fn test_resolved_key() -> obleth_config::ResolvedKey {
        obleth_config::ResolvedKey {
            key_id: uuid::Uuid::new_v4(),
            tenant_id: uuid::Uuid::new_v4(),
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
        }
    }
}
