//! In-process knowledge index.
//!
//! The request path may not read Postgres, so the corpus is mirrored into
//! memory and swapped wholesale behind an `ArcSwap` — the same shape as the
//! model registry and the classifier. Readers take an `Arc` of the map, so a
//! rebuild never blocks or invalidates an in-flight retrieval.
//!
//! The index starts empty and fills asynchronously (see `spawn_refresh`), so a
//! large corpus never delays boot.
//!
//! This task builds the index and its refresh loop only; the request-path
//! consumer (the knowledge boon reading `AppState::knowledge` and calling
//! `CollectionSlab::retrieve`) lands in a later task (Task 11). Until then
//! `Hit`, `retrieve`, and several slab/chunk fields are exercised only by this
//! module's own tests, so they carry targeted `#[allow(dead_code)]` rather
//! than a module-wide one — a later dead item added elsewhere in this module
//! should still be caught.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use obleth_store::knowledge::{score_against, KnowledgeCollection};
use obleth_store::Store;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SlabChunk {
    #[allow(dead_code)]
    pub id: Uuid,
    #[allow(dead_code)]
    pub title: String,
    #[allow(dead_code)]
    pub text: String,
    #[allow(dead_code)]
    pub token_count: u32,
    #[allow(dead_code)]
    pub embedding: Vec<f32>,
}

/// One collection's active generation, ready to score against.
#[derive(Debug, Clone)]
pub struct CollectionSlab {
    /// The embedder that built these vectors. A query must be embedded with
    /// this same model, or the scores are meaningless.
    #[allow(dead_code)]
    pub embedding_model: String,
    #[allow(dead_code)]
    pub dim: usize,
    /// The collection's `version` at the time this slab was built. Compared
    /// against the live row on each refresh so an unchanged collection is
    /// never re-read from Postgres.
    pub version: i64,
    #[allow(dead_code)]
    pub chunks: Vec<SlabChunk>,
}

/// One retrieved chunk.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Hit {
    pub id: Uuid,
    pub title: String,
    pub text: String,
    pub token_count: u32,
    pub score: f32,
}

impl CollectionSlab {
    #[allow(dead_code)]
    pub fn retrieve(&self, query: &[f32], top_k: usize, min_score: f32) -> Vec<Hit> {
        // No intermediate `Vec<Vec<f32>>`: at the collection chunk cap
        // (100k chunks x 768 dims) cloning every embedding on each
        // request-path retrieval would allocate and copy ~300MB per call.
        score_against(
            query,
            self.chunks.iter().map(|c| c.embedding.as_slice()),
            top_k,
            min_score,
        )
        .into_iter()
        .map(|s| {
            let c = &self.chunks[s.index];
            Hit {
                id: c.id,
                title: c.title.clone(),
                text: c.text.clone(),
                token_count: c.token_count,
                score: s.score,
            }
        })
        .collect()
    }
}

pub type Slabs = HashMap<Uuid, Arc<CollectionSlab>>;

pub struct KnowledgeIndex {
    slabs: ArcSwap<Slabs>,
}

impl Default for KnowledgeIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl KnowledgeIndex {
    pub fn new() -> Self {
        Self {
            slabs: ArcSwap::from_pointee(HashMap::new()),
        }
    }

    /// Current snapshot. Cheap; clones a single `Arc` so an in-flight
    /// retrieval is never invalidated by a concurrent rebuild.
    pub fn snapshot(&self) -> Arc<Slabs> {
        self.slabs.load_full()
    }

    pub fn swap(&self, next: Arc<Slabs>) {
        self.slabs.store(next);
    }

    /// Rebuild any collection whose `version` changed, using only collections
    /// granted to at least one model (per-replica memory scales with the
    /// grounded corpus, not the whole database) and isolating a single
    /// collection's load failure from the rest of the pass (see
    /// `plan_refresh` and `apply_rebuild`). `list_collections` and
    /// `all_model_collection_ids` are the only calls whose failure aborts the
    /// whole refresh outright — without them there is no way to distinguish
    /// "deleted"/"ungranted" from "failed to load", so the last good index is
    /// left untouched and the caller (`spawn_refresh`) retries next tick.
    pub async fn refresh_once(&self, store: &Store) -> anyhow::Result<()> {
        let collections = store.list_collections().await?;
        let granted: HashSet<Uuid> = store
            .all_model_collection_ids()
            .await?
            .into_values()
            .flatten()
            .collect();
        let current = self.snapshot();
        let plan = plan_refresh(&current, &collections, &granted);

        let mut next: Slabs = HashMap::with_capacity(plan.len());
        for (id, action) in plan {
            match action {
                RefreshAction::Reuse(slab) => {
                    next.insert(id, slab);
                }
                RefreshAction::Rebuild {
                    embedding_model,
                    dim,
                    version,
                } => {
                    let result = build_slab(store, id, embedding_model, dim, version).await;
                    apply_rebuild(&mut next, &current, id, result);
                }
            }
        }
        self.swap(Arc::new(next));
        Ok(())
    }

    /// Poll for corpus changes. Errors are logged and retried: a Postgres
    /// outage must leave the last good index serving, never clear it.
    pub fn spawn_refresh(index: Arc<Self>, store: Store, interval: Duration) {
        tokio::spawn(async move {
            loop {
                if let Err(e) = index.refresh_once(&store).await {
                    tracing::warn!(error = %e, "knowledge index refresh failed");
                }
                tokio::time::sleep(interval).await;
            }
        });
    }
}

/// What to do with one collection on a refresh pass. Building the actual
/// `CollectionSlab` requires Postgres, so `Rebuild` carries only the plain
/// data needed to do that later — this type itself, and `plan_refresh` below,
/// touch no I/O and are exhaustively unit-tested without a database.
#[derive(Debug, Clone)]
pub enum RefreshAction {
    /// Version unchanged: keep serving the existing `Arc`, no Postgres reads.
    Reuse(Arc<CollectionSlab>),
    /// New collection, or an existing one whose version changed: (re)load its
    /// chunks and titles from Postgres.
    Rebuild {
        embedding_model: String,
        dim: usize,
        version: i64,
    },
}

/// Decide what to do with each collection in `collections`, given the
/// currently-held `current` snapshot and the set of collection ids actually
/// granted to at least one model (`granted`). Per-replica memory scales with
/// the grounded corpus, not the whole database, so a collection absent from
/// `granted` is simply omitted from the result — the caller drops it from the
/// next snapshot. A collection absent from `collections` entirely (deleted)
/// is likewise omitted, for the same reason.
pub fn plan_refresh(
    current: &Slabs,
    collections: &[KnowledgeCollection],
    granted: &HashSet<Uuid>,
) -> Vec<(Uuid, RefreshAction)> {
    collections
        .iter()
        .filter(|c| granted.contains(&c.id))
        .map(|c| {
            let action = match current.get(&c.id) {
                Some(existing) if existing.version == c.version => {
                    RefreshAction::Reuse(existing.clone())
                }
                _ => RefreshAction::Rebuild {
                    embedding_model: c.indexed_embedding_model.clone(),
                    dim: c.embedding_dim.max(0) as usize,
                    version: c.version,
                },
            };
            (c.id, action)
        })
        .collect()
}

/// Load one collection's chunks and titles and assemble its slab. A titles
/// failure is treated exactly like a chunk-load failure (propagated via `?`,
/// not swallowed): because a slab is only rebuilt on the next version change,
/// a swallowed titles failure would produce "Untitled" labels that are sticky
/// rather than transient, silently degrading citations until the collection
/// is written again.
async fn build_slab(
    store: &Store,
    collection_id: Uuid,
    embedding_model: String,
    dim: usize,
    version: i64,
) -> anyhow::Result<CollectionSlab> {
    let chunks = store.load_active_chunks(collection_id).await?;
    let titles = store.document_titles(collection_id).await?;
    Ok(CollectionSlab {
        embedding_model,
        dim,
        version,
        chunks: chunks
            .into_iter()
            .map(|k| SlabChunk {
                id: k.id,
                title: titles
                    .get(&k.document_id)
                    .cloned()
                    .unwrap_or_else(|| "Untitled".into()),
                text: k.text,
                token_count: k.token_count.max(0) as u32,
                embedding: k.embedding,
            })
            .collect(),
    })
}

/// Cache key (hash only, no prefix — `RedisStore` owns the namespace) for a
/// query vector. Scoped to the embedding model because the vector depends
/// only on the model, not the collection: re-indexing content must not
/// cold-start the cache, and two embedders must never share an entry. A
/// SHA-256 digest keeps user query text (which may carry sensitive content)
/// out of the key itself.
pub fn query_cache_key(embedding_model: &str, query: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(embedding_model.as_bytes());
    h.update([0u8]);
    h.update(query.as_bytes());
    format!("{:x}", h.finalize())
}

/// Embed the retrieval query, preferring the cache. Returns `None` on any
/// failure — cache miss, cache error, unknown/unresolved embedding model,
/// upstream timeout, or a malformed response — so the boon then passes the
/// request through ungrounded rather than failing it. Only Redis and the
/// moka model cache are touched: `resolve_model` never reads Postgres, which
/// matters because this runs on the request path.
///
/// Unused until the knowledge boon (Task 11) calls it on the request path —
/// see the module-level note on targeted `#[allow(dead_code)]` rather than a
/// module-wide one.
#[allow(dead_code)]
pub async fn embed_query(
    state: &crate::state::AppState,
    embedding_model: &str,
    query: &str,
    settings: &obleth_config::KnowledgeBoonSettings,
) -> Option<Vec<f32>> {
    let key = query_cache_key(embedding_model, query);
    if let Some(v) = state.redis.get_query_vector(&key).await {
        return Some(v);
    }
    let route = crate::proxy::resolve_model(state, embedding_model).await?;
    let target = obleth_admin::knowledge::embed::EmbedTarget {
        api_base: route.api_base.clone(),
        api_key: route.api_key.clone(),
        upstream_model: route.upstream_model.clone(),
    };
    let inputs = [query.to_string()];
    let vector = obleth_admin::knowledge::embed::embed_batch(
        &state.http,
        &target,
        &inputs,
        Duration::from_millis(settings.embed_timeout_ms),
    )
    .await
    .ok()?
    .into_iter()
    .next()?;
    state
        .redis
        .put_query_vector(&key, &vector, settings.query_cache_ttl_s)
        .await;
    Some(vector)
}

/// Apply one collection's rebuild outcome to the next snapshot: on success,
/// insert the new slab; on failure, retain whatever `current` held for this
/// collection (or omit it if `current` had nothing), so one bad collection
/// can neither wipe out its own last-known-good data nor stall the rest of
/// the corpus — every other collection in the same pass still gets a fresh
/// slab. Logs the failure with the collection id.
fn apply_rebuild(
    next: &mut Slabs,
    current: &Slabs,
    id: Uuid,
    result: anyhow::Result<CollectionSlab>,
) {
    match result {
        Ok(slab) => {
            next.insert(id, Arc::new(slab));
        }
        Err(e) => {
            tracing::warn!(
                collection = %id,
                error = %e,
                "knowledge slab rebuild failed; retaining previous slab if any"
            );
            if let Some(existing) = current.get(&id) {
                next.insert(id, existing.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slab(dim: usize, n: usize) -> CollectionSlab {
        CollectionSlab {
            embedding_model: "embed-a".into(),
            dim,
            version: 1,
            chunks: (0..n)
                .map(|i| SlabChunk {
                    id: Uuid::new_v4(),
                    title: format!("Doc {i}"),
                    text: format!("chunk {i}"),
                    token_count: 10,
                    embedding: vec![1.0; dim],
                })
                .collect(),
        }
    }

    #[test]
    fn empty_index_returns_no_slab() {
        let index = KnowledgeIndex::new();
        assert!(index.snapshot().get(&Uuid::new_v4()).is_none());
    }

    #[test]
    fn swapped_snapshot_is_visible_to_later_readers() {
        let index = KnowledgeIndex::new();
        let id = Uuid::new_v4();
        let mut map = HashMap::new();
        map.insert(id, Arc::new(slab(4, 3)));
        index.swap(Arc::new(map));
        let snap = index.snapshot();
        assert_eq!(snap.get(&id).expect("slab").chunks.len(), 3);
    }

    #[test]
    fn a_snapshot_taken_before_a_swap_stays_readable() {
        // Readers hold an Arc, so a rebuild never invalidates an in-flight
        // retrieval — this is why the whole map is swapped rather than mutated.
        let index = KnowledgeIndex::new();
        let id = Uuid::new_v4();
        let mut first = HashMap::new();
        first.insert(id, Arc::new(slab(4, 2)));
        index.swap(Arc::new(first));
        let held = index.snapshot();

        index.swap(Arc::new(HashMap::new()));
        assert_eq!(held.get(&id).expect("still readable").chunks.len(), 2);
        assert!(index.snapshot().get(&id).is_none());
    }

    #[test]
    fn retrieve_ranks_and_thresholds_within_a_slab() {
        let mut s = slab(2, 0);
        s.chunks = vec![
            SlabChunk {
                id: Uuid::new_v4(),
                title: "A".into(),
                text: "a".into(),
                token_count: 5,
                embedding: vec![1.0, 0.0],
            },
            SlabChunk {
                id: Uuid::new_v4(),
                title: "B".into(),
                text: "b".into(),
                token_count: 5,
                embedding: vec![0.0, 1.0],
            },
        ];
        let hits = s.retrieve(&[1.0, 0.0], 5, 0.5);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].title, "A");
    }

    fn collection(id: Uuid, version: i64) -> KnowledgeCollection {
        KnowledgeCollection {
            id,
            name: "c".into(),
            description: String::new(),
            embedding_model: "embed-a".into(),
            indexed_embedding_model: "embed-a".into(),
            embedding_dim: 4,
            chunk_tokens: 100,
            chunk_overlap_tokens: 0,
            version,
        }
    }

    #[test]
    fn plan_reuses_the_same_arc_when_version_is_unchanged() {
        let id = Uuid::new_v4();
        let mut current = HashMap::new();
        current.insert(id, Arc::new(slab(4, 1)));
        let granted = HashSet::from([id]);

        let plan = plan_refresh(&current, &[collection(id, 1)], &granted);
        assert_eq!(plan.len(), 1);
        match &plan[0].1 {
            RefreshAction::Reuse(slab) => {
                assert!(Arc::ptr_eq(slab, current.get(&id).unwrap()));
            }
            other => panic!("expected Reuse, got {other:?}"),
        }
    }

    #[test]
    fn plan_rebuilds_when_version_changed() {
        let id = Uuid::new_v4();
        let mut current = HashMap::new();
        current.insert(id, Arc::new(slab(4, 1))); // version 1
        let granted = HashSet::from([id]);

        let plan = plan_refresh(&current, &[collection(id, 2)], &granted);
        assert_eq!(plan.len(), 1);
        match &plan[0].1 {
            RefreshAction::Rebuild { version, .. } => assert_eq!(*version, 2),
            other => panic!("expected Rebuild, got {other:?}"),
        }
    }

    #[test]
    fn plan_drops_a_collection_absent_from_the_live_list() {
        let id = Uuid::new_v4();
        let mut current = HashMap::new();
        current.insert(id, Arc::new(slab(4, 1)));
        let granted = HashSet::from([id]);

        // Deleted collections simply are not in `collections` any more.
        let plan = plan_refresh(&current, &[], &granted);
        assert!(plan.is_empty(), "a deleted collection must not reappear");
    }

    #[test]
    fn plan_skips_a_collection_not_granted_to_any_model() {
        let id = Uuid::new_v4();
        let current: Slabs = HashMap::new();
        let granted: HashSet<Uuid> = HashSet::new(); // nothing granted

        let plan = plan_refresh(&current, &[collection(id, 1)], &granted);
        assert!(
            plan.is_empty(),
            "an ungranted collection must not be planned at all"
        );
    }

    #[test]
    fn plan_rebuilds_a_brand_new_granted_collection() {
        let id = Uuid::new_v4();
        let current: Slabs = HashMap::new(); // nothing held yet
        let granted = HashSet::from([id]);

        let plan = plan_refresh(&current, &[collection(id, 1)], &granted);
        assert_eq!(plan.len(), 1);
        assert!(matches!(plan[0].1, RefreshAction::Rebuild { .. }));
    }

    #[test]
    fn a_failed_rebuild_retains_the_previous_slab_instead_of_dropping_it() {
        let id = Uuid::new_v4();
        let mut current = HashMap::new();
        current.insert(id, Arc::new(slab(4, 3)));
        let mut next: Slabs = HashMap::new();

        apply_rebuild(&mut next, &current, id, Err(anyhow::anyhow!("boom")));

        assert_eq!(
            next.get(&id).expect("previous slab retained").chunks.len(),
            3,
            "a failed rebuild must keep serving the last good slab for this collection"
        );
    }

    #[test]
    fn cache_key_is_scoped_to_the_embedder_not_the_collection() {
        // The vector depends only on the model, so re-indexing content must not
        // cold-start the cache — and two embedders must never share an entry.
        // `query_cache_key` itself returns only the hash portion: the
        // `obleth:knowledge:q:` namespace prefix is owned by `RedisStore`
        // (mirroring `compress_key`), since the proxy has no access to the
        // private const that names it.
        let a = query_cache_key("embed-a", "refund policy");
        let b = query_cache_key("embed-b", "refund policy");
        assert_ne!(a, b);
        assert_eq!(a, query_cache_key("embed-a", "refund policy"));
        assert_eq!(a.len(), 64, "sha256 hex digest");
    }

    #[test]
    fn cache_key_does_not_embed_raw_query_text() {
        // Queries can contain user content; the key is a digest, not the text.
        let key = query_cache_key("embed-a", "my social security number is 123");
        assert!(!key.contains("social"));
    }

    #[test]
    fn a_failed_rebuild_with_no_previous_slab_omits_the_collection() {
        let id = Uuid::new_v4();
        let current: Slabs = HashMap::new(); // nothing to fall back to
        let mut next: Slabs = HashMap::new();

        apply_rebuild(&mut next, &current, id, Err(anyhow::anyhow!("boom")));

        assert!(
            !next.contains_key(&id),
            "a failed rebuild with no prior data must not fabricate a slab"
        );
    }
}
