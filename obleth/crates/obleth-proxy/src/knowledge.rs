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
//! The request-path consumer is the knowledge boon (`boons::knowledge`),
//! which reads `AppState::knowledge` and calls `CollectionSlab::retrieve`.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use obleth_store::knowledge::{score_against, ActiveDocument, KnowledgeChunk, KnowledgeCollection};
use obleth_store::Store;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SlabChunk {
    pub id: Uuid,
    pub document_id: Uuid,
    pub title: String,
    pub text: String,
    pub token_count: u32,
    pub embedding: Vec<f32>,
}

/// One collection's active generation, ready to score against.
#[derive(Debug, Clone)]
pub struct CollectionSlab {
    /// The embedder that built these vectors. A query must be embedded with
    /// this same model, or the scores are meaningless.
    pub embedding_model: String,
    /// Not read yet on the request path (the boon does not currently validate
    /// query/collection dimension parity before scoring); still genuinely
    /// unused, so it keeps its own allow rather than being swept up with the
    /// other fields Task 11 put to use.
    #[allow(dead_code)]
    pub dim: usize,
    /// The collection's `version` at the time this slab was built. Compared
    /// against the live row on each refresh so an unchanged collection is
    /// never re-read from Postgres.
    pub version: i64,
    /// Shared with the slab this one was built from wherever a document did
    /// not change, so an incremental rebuild copies no chunk.
    pub chunks: Vec<Arc<SlabChunk>>,
    /// The documents the chunks came from, as they were when loaded: a
    /// rebuild reloads only the documents that differ from these.
    pub documents: HashMap<Uuid, ActiveDocument>,
    /// When every chunk was last read from Postgres. Incremental rebuilds
    /// keep it; past [`FULL_RELOAD_EVERY`] the next rebuild reads everything.
    pub loaded_at: Instant,
}

/// How long incremental rebuilds may go on before a rebuild reloads the
/// whole collection, so a bookkeeping slip cannot serve stale chunks for
/// long.
pub const FULL_RELOAD_EVERY: Duration = Duration::from_secs(30 * 60);

/// One retrieved chunk.
#[derive(Debug, Clone)]
pub struct Hit {
    pub id: Uuid,
    pub title: String,
    pub text: String,
    pub token_count: u32,
    pub score: f32,
}

impl CollectionSlab {
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
                    let previous = current.get(&id).map(|s| s.as_ref());
                    let result =
                        build_slab(store, id, embedding_model, dim, version, previous).await;
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

/// Load one collection's slab. With the `previous` slab for the same
/// embedder, only documents that are new or changed (another generation or
/// title) are read from Postgres, and every other document's chunks are
/// shared with it; removed documents drop out. Without one, or once the
/// previous full load is [`FULL_RELOAD_EVERY`] old, everything is read.
async fn build_slab(
    store: &Store,
    collection_id: Uuid,
    embedding_model: String,
    dim: usize,
    version: i64,
    previous: Option<&CollectionSlab>,
) -> anyhow::Result<CollectionSlab> {
    let documents = store.active_documents(collection_id).await?;
    let previous = previous.filter(|p| {
        p.embedding_model == embedding_model && p.loaded_at.elapsed() < FULL_RELOAD_EVERY
    });
    let unchanged =
        |d: &ActiveDocument| previous.is_some_and(|p| p.documents.get(&d.id) == Some(d));
    let changed: Vec<Uuid> = documents
        .iter()
        .filter(|d| !unchanged(d))
        .map(|d| d.id)
        .collect();
    let loaded = match previous {
        None => store.load_active_chunks(collection_id).await?,
        Some(_) if changed.is_empty() => Vec::new(),
        Some(_) => store.load_active_chunks_of(collection_id, &changed).await?,
    };
    Ok(assemble_slab(
        embedding_model,
        dim,
        version,
        documents,
        previous,
        loaded,
    ))
}

/// Put a slab together from `documents` (in id order): each unchanged
/// document's chunks from `previous`, every other one's from `loaded`
/// (ordered by document, then ordinal). Pure, for testing without Postgres.
fn assemble_slab(
    embedding_model: String,
    dim: usize,
    version: i64,
    documents: Vec<ActiveDocument>,
    previous: Option<&CollectionSlab>,
    loaded: Vec<KnowledgeChunk>,
) -> CollectionSlab {
    let by_id: HashMap<Uuid, &ActiveDocument> = documents.iter().map(|d| (d.id, d)).collect();
    let mut kept: HashMap<Uuid, Vec<Arc<SlabChunk>>> = HashMap::new();
    if let Some(p) = previous {
        for chunk in &p.chunks {
            let doc = chunk.document_id;
            if by_id
                .get(&doc)
                .is_some_and(|d| p.documents.get(&doc) == Some(*d))
            {
                kept.entry(doc).or_default().push(chunk.clone());
            }
        }
    }
    let mut fresh: HashMap<Uuid, Vec<Arc<SlabChunk>>> = HashMap::new();
    for k in loaded {
        let title = by_id
            .get(&k.document_id)
            .map_or_else(|| "Untitled".to_string(), |d| d.title.clone());
        fresh
            .entry(k.document_id)
            .or_default()
            .push(Arc::new(SlabChunk {
                id: k.id,
                document_id: k.document_id,
                title,
                text: k.text,
                token_count: k.token_count.max(0) as u32,
                embedding: k.embedding,
            }));
    }
    let chunks = documents
        .iter()
        .flat_map(|d| {
            kept.remove(&d.id)
                .or_else(|| fresh.remove(&d.id))
                .unwrap_or_default()
        })
        .collect();
    CollectionSlab {
        embedding_model,
        dim,
        version,
        chunks,
        documents: documents.into_iter().map(|d| (d.id, d)).collect(),
        loaded_at: previous.map_or_else(Instant::now, |p| p.loaded_at),
    }
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
/// Called from the knowledge boon (`boons::knowledge::apply`) on the request
/// path.
pub async fn embed_query(
    state: &crate::state::AppState,
    embedding_model: &str,
    query: &str,
    settings: &obleth_config::KnowledgeBoonSettings,
) -> Option<Vec<f32>> {
    let key = query_cache_key(embedding_model, query);
    // The cache only saves an embedding call: a Redis error is a miss.
    match state.redis.get_query_vector(&key).await {
        Ok(Some(v)) => return Some(v),
        Ok(None) => {}
        Err(e) => tracing::debug!(error = %e, "knowledge query vector read failed; embedding"),
    }
    let route = crate::proxy::resolve_model(state, embedding_model).await?;
    let target = match embed_target(&route) {
        Ok(target) => target,
        Err(e) => {
            tracing::debug!(error = %e, model = %embedding_model, "knowledge query embed skipped");
            return None;
        }
    };
    let vector = obleth_admin::knowledge::embed::embed_batch(
        &state.http,
        &target,
        &[query],
        Duration::from_millis(settings.embed_timeout_ms),
    )
    .await
    .ok()?
    .into_iter()
    .next()?;
    if let Err(e) = state
        .redis
        .put_query_vector(&key, &vector, settings.query_cache_ttl_s)
        .await
    {
        tracing::debug!(error = %e, "knowledge query vector store failed");
    }
    Some(vector)
}

/// Where the query embedding goes: the embedding model's endpoint-aware target
/// (healthy endpoints first, endpoint key over model key), so a Slurm-hosted
/// embedding model with a blank `api_base` is reachable.
fn embed_target(
    route: &obleth_config::ResolvedModel,
) -> anyhow::Result<obleth_admin::knowledge::embed::EmbedTarget> {
    let target = crate::boons::helper_target(route, "", None)?;
    Ok(obleth_admin::knowledge::embed::EmbedTarget {
        api_base: target.base,
        api_key: target.api_key,
        headers: target.headers,
        upstream_model: route.upstream_model.clone(),
    })
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
                .map(|i| {
                    Arc::new(SlabChunk {
                        id: Uuid::new_v4(),
                        document_id: Uuid::nil(),
                        title: format!("Doc {i}"),
                        text: format!("chunk {i}"),
                        token_count: 10,
                        embedding: vec![1.0; dim],
                    })
                })
                .collect(),
            documents: HashMap::new(),
            loaded_at: Instant::now(),
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
            Arc::new(SlabChunk {
                id: Uuid::new_v4(),
                document_id: Uuid::nil(),
                title: "A".into(),
                text: "a".into(),
                token_count: 5,
                embedding: vec![1.0, 0.0],
            }),
            Arc::new(SlabChunk {
                id: Uuid::new_v4(),
                document_id: Uuid::nil(),
                title: "B".into(),
                text: "b".into(),
                token_count: 5,
                embedding: vec![0.0, 1.0],
            }),
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

    /// A Slurm-hosted embedding model has a blank `api_base` and one healthy
    /// endpoint; the query embedding must reach it with the endpoint's key.
    #[tokio::test]
    async fn query_embedding_reaches_an_endpoint_only_model() {
        let seen = Arc::new(std::sync::Mutex::new(String::new()));
        let seen_clone = seen.clone();
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(move |headers: axum::http::HeaderMap| {
                let seen = seen_clone.clone();
                async move {
                    *seen.lock().unwrap() = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    axum::Json(serde_json::json!({
                        "data": [{ "index": 0, "embedding": [3.0, 4.0] }]
                    }))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let route = crate::boons::test_support::endpoint_only_route(&format!("http://{addr}/v1"));
        let target = embed_target(&route).expect("endpoint target");
        let vectors = obleth_admin::knowledge::embed::embed_batch(
            &reqwest::Client::new(),
            &target,
            &["query"],
            Duration::from_secs(5),
        )
        .await
        .expect("embedding through the endpoint");
        assert_eq!(vectors.len(), 1);
        assert_eq!(seen.lock().unwrap().as_str(), "Bearer endpoint-key");

        let mut unreachable = route.clone();
        unreachable.endpoints.clear();
        assert!(embed_target(&unreachable).is_err());
    }

    fn doc(id: Uuid, generation: i32) -> ActiveDocument {
        ActiveDocument {
            id,
            generation,
            title: format!("T{generation}"),
        }
    }

    fn loaded(document_id: Uuid, n: usize) -> Vec<KnowledgeChunk> {
        (0..n)
            .map(|i| KnowledgeChunk {
                id: Uuid::new_v4(),
                document_id,
                text: format!("chunk {i}"),
                token_count: 1,
                embedding: vec![1.0, 0.0],
            })
            .collect()
    }

    #[test]
    fn an_incremental_build_shares_unchanged_documents_and_drops_removed_ones() {
        let mut ids = [
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        ];
        ids.sort();
        let [a, b, c, gone] = ids;
        let mut first_loaded = loaded(a, 2);
        first_loaded.extend(loaded(b, 3));
        first_loaded.extend(loaded(gone, 1));
        let first = assemble_slab(
            "embed-a".into(),
            2,
            1,
            vec![doc(a, 1), doc(b, 1), doc(gone, 1)],
            None,
            first_loaded,
        );
        assert_eq!(first.chunks.len(), 6);

        // A re-indexed, B unchanged, C new, `gone` deleted: only A and C load.
        let mut changed = loaded(a, 4);
        changed.extend(loaded(c, 1));
        let second = assemble_slab(
            "embed-a".into(),
            2,
            2,
            vec![doc(a, 2), doc(b, 1), doc(c, 1)],
            Some(&first),
            changed,
        );
        let of = |slab: &CollectionSlab, d: Uuid| -> Vec<Arc<SlabChunk>> {
            slab.chunks
                .iter()
                .filter(|k| k.document_id == d)
                .cloned()
                .collect()
        };
        let (b_before, b_after) = (of(&first, b), of(&second, b));
        assert_eq!(b_after.len(), 3);
        assert!(
            b_before
                .iter()
                .zip(&b_after)
                .all(|(x, y)| Arc::ptr_eq(x, y)),
            "B is shared, not copied"
        );
        assert_eq!(of(&second, a).len(), 4);
        assert_eq!(of(&second, c).len(), 1);
        assert!(of(&second, gone).is_empty());
        let order: Vec<Uuid> = second.chunks.iter().map(|k| k.document_id).collect();
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted, "chunks stay in document order");
        assert_eq!(
            second.loaded_at, first.loaded_at,
            "an incremental build is not a full load"
        );
        assert_eq!(second.documents.len(), 3);
    }

    /// Against Postgres: re-indexing one document reloads its chunks and
    /// shares the other's; the result serves the same chunks as a full
    /// reload. Runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_refresh_reloads_only_the_changed_document() {
        let Ok(url) = std::env::var("OBLETH_TEST_DATABASE_URL") else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let coll = store
            .create_collection(&format!("slab-{}", Uuid::new_v4()), "", "embed-a")
            .await
            .expect("collection");
        let commit = |doc_id: Uuid, generation: i32, n: i32| {
            let store = store.clone();
            async move {
                let chunks: Vec<obleth_store::knowledge::ChunkInsert> = (0..n)
                    .map(|i| obleth_store::knowledge::ChunkInsert {
                        ordinal: i,
                        text: format!("g{generation}-{i}"),
                        token_count: 1,
                        embedding: vec![1.0, 0.0],
                    })
                    .collect();
                store
                    .commit_document_generation(coll.id, doc_id, generation, "embed-a", 2, &chunks)
                    .await
                    .expect("commit");
            }
        };
        let a = store
            .upsert_document(coll.id, "A", "a.md", "text/markdown", "a")
            .await
            .expect("a");
        let b = store
            .upsert_document(coll.id, "B", "b.md", "text/markdown", "b")
            .await
            .expect("b");
        commit(a.id, 1, 2).await;
        commit(b.id, 1, 3).await;
        let first = build_slab(&store, coll.id, "embed-a".into(), 2, 1, None)
            .await
            .expect("first");
        assert_eq!(first.chunks.len(), 5);

        commit(a.id, 2, 4).await;
        let second = build_slab(&store, coll.id, "embed-a".into(), 2, 2, Some(&first))
            .await
            .expect("second");
        let full = build_slab(&store, coll.id, "embed-a".into(), 2, 2, None)
            .await
            .expect("full");
        let ids = |s: &CollectionSlab| s.chunks.iter().map(|k| k.id).collect::<Vec<_>>();
        assert_eq!(ids(&second), ids(&full), "incremental == full reload");
        let shared = second
            .chunks
            .iter()
            .filter(|k| first.chunks.iter().any(|f| Arc::ptr_eq(f, k)))
            .count();
        assert_eq!(shared, 3, "B's three chunks are shared; A's four are new");

        store.delete_document(b.id).await.expect("delete b");
        let third = build_slab(&store, coll.id, "embed-a".into(), 2, 3, Some(&second))
            .await
            .expect("third");
        assert_eq!(third.chunks.len(), 4);
        assert!(third.chunks.iter().all(|k| k.document_id == a.id));
        store.delete_collection(coll.id).await.ok();
    }
}
