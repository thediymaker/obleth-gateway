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
//! `CollectionSlab::retrieve`) lands in a later task. Until then `Hit`,
//! `retrieve`, and the slab/chunk fields are exercised only by this module's
//! own tests. Remove this once that boon calls `retrieve`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use obleth_store::knowledge::score_against;
use obleth_store::Store;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct SlabChunk {
    pub id: Uuid,
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
    pub dim: usize,
    /// The collection's `version` at the time this slab was built. Compared
    /// against the live row on each refresh so an unchanged collection is
    /// never re-read from Postgres.
    pub version: i64,
    pub chunks: Vec<SlabChunk>,
}

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
        let vectors: Vec<Vec<f32>> = self.chunks.iter().map(|c| c.embedding.clone()).collect();
        score_against(query, &vectors, top_k, min_score)
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

    /// Rebuild any collection whose `version` changed. Unchanged collections
    /// keep their existing `Arc`, so a refresh costs nothing for a stable
    /// corpus and never re-reads vectors it already holds.
    ///
    /// A Postgres error here must leave the last good index serving: return
    /// early on the failing call so `self.slabs` is never replaced with a
    /// partial or empty map. The caller (`spawn_refresh`) logs and retries on
    /// the next tick.
    pub async fn refresh_once(&self, store: &Store) -> anyhow::Result<()> {
        let collections = store.list_collections().await?;
        let current = self.snapshot();
        let mut next: Slabs = HashMap::with_capacity(collections.len());
        for c in collections {
            if let Some(existing) = current.get(&c.id) {
                if existing.version == c.version {
                    next.insert(c.id, existing.clone());
                    continue;
                }
            }
            let chunks = store.load_active_chunks(c.id).await?;
            let titles = store.document_titles(c.id).await.unwrap_or_default();
            let slab = CollectionSlab {
                embedding_model: c.indexed_embedding_model.clone(),
                dim: c.embedding_dim.max(0) as usize,
                version: c.version,
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
            };
            next.insert(c.id, Arc::new(slab));
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
}
