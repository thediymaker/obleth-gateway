//! Background indexing: pending document -> chunks -> vectors -> new generation.
//!
//! Work is claimed one document at a time with `for update skip locked`, so no
//! two claims ever return the same row in the same instant. That is not the
//! same as "two workers can never hold a document at once": a worker that
//! dies mid-document leaves the row `indexing`, and `claim_pending_document`'s
//! staleness window (see `KnowledgeBoonSettings::index_stale_after_secs`)
//! deliberately lets a second worker reclaim it after a timeout, even if the
//! first worker turns out to still be alive and embedding it -- that overlap
//! is how stranded work is recovered at all, not a bug. What makes it safe is
//! `knowledge_chunks_document_generation_ordinal_uq` (`0020`): only one of the
//! two workers' commits can actually persist chunks for the document's
//! generation, and the loser fails with `23505` rather than retrying (see
//! `knowledge::commit_document_generation`). Recovery is timeout-based rather
//! than a reset at startup because a startup reset would let a booting
//! replica steal a document another replica is still actively embedding.
//!
//! Generations are scoped per-document (not per-collection): each document
//! tracks its own `active_generation`, and `commit_document_generation` only ever
//! touches that one document's chunks, promoting the generation and marking the
//! document `ready` in a single transaction. See
//! `.superpowers/sdd/2026-09-10-knowledge-base/CORRECTION-per-document-generations.md`
//! for why a collection-scoped generation counter was wrong (it deleted every
//! other document's chunks on each index), and
//! `.superpowers/sdd/2026-09-10-knowledge-base/CORRECTION-indexer-atomicity.md`
//! for why writing chunks and promoting the generation must be one transaction
//! rather than two separate calls (a crash between them left a document `ready`
//! with no live chunks, and no retry path could ever reclaim it).

use std::time::Duration;

use anyhow::{anyhow, Result};
use obleth_config::{BoonSettings, KnowledgeBoonSettings};
use obleth_store::knowledge::{ChunkInsert, KnowledgeDocument};
use obleth_store::Store;
use obleth_tokenizer::HeuristicTokenizer;

use super::chunk::{chunk_text, Chunk};
use super::embed::{embed_batch, EmbedTarget};

/// Group chunk texts into upstream-sized batches, preserving order.
pub fn plan_batches(chunks: &[Chunk], batch_size: usize) -> Vec<Vec<String>> {
    let size = batch_size.max(1);
    chunks
        .chunks(size)
        .map(|g| g.iter().map(|c| c.text.clone()).collect())
        .collect()
}

/// Refuse a document that would push the collection past its chunk cap.
pub fn check_chunk_cap(existing: i64, incoming: usize, cap: i64) -> Result<()> {
    if existing + incoming as i64 > cap {
        return Err(anyhow!(
            "collection chunk limit reached ({cap}); delete documents or raise \
             max_chunks_per_collection"
        ));
    }
    Ok(())
}

/// Chunk, embed, and store one document as its own next generation.
pub async fn index_document(
    store: &Store,
    client: &reqwest::Client,
    doc: &KnowledgeDocument,
    settings: &KnowledgeBoonSettings,
) -> Result<()> {
    let collection = store.get_collection(doc.collection_id).await?;
    // Distinguish "model isn't registered" (an administrator misconfiguration)
    // from a transient store failure: collapsing both into the same message
    // would mislabel a database hiccup as a configuration mistake, and this
    // string is exactly what an administrator reads in the document's `error`
    // column.
    let model = match store.get_model_by_name(&collection.embedding_model).await {
        Ok(m) => m,
        Err(obleth_store::StoreError::NotFound) => {
            return Err(anyhow!(
                "embedding model `{}` is not registered",
                collection.embedding_model
            ))
        }
        Err(e) => {
            return Err(anyhow!(
                "could not load embedding model `{}`: {e}",
                collection.embedding_model
            ))
        }
    };
    if model.model_type != "embedding" {
        return Err(anyhow!(
            "model `{}` is type `{}`, not `embedding`",
            model.model_name,
            model.model_type
        ));
    }

    let tk = HeuristicTokenizer::new();
    let chunks = chunk_text(
        &doc.content,
        &doc.content_type,
        collection.chunk_tokens as u32,
        collection.chunk_overlap_tokens as u32,
        &tk,
    );
    if chunks.is_empty() {
        return Err(anyhow!("document produced no chunks"));
    }
    // Excludes `doc`'s own currently-active chunks: they belong to the
    // generation this index run is about to replace, so counting them as
    // "existing" alongside the incoming chunks double-charges this document
    // against the cap (see `collection_chunk_count_excluding`).
    let existing = store
        .collection_chunk_count_excluding(collection.id, doc.id)
        .await?;
    check_chunk_cap(existing, chunks.len(), settings.max_chunks_per_collection)?;

    let target = EmbedTarget {
        api_base: model.api_base.clone(),
        api_key: model.api_key.clone(),
        headers: crate::upstream_header_map(&model.upstream_headers),
        upstream_model: model.upstream_model.clone(),
    };
    let timeout = Duration::from_millis(settings.index_timeout_ms.max(1_000));

    let mut vectors: Vec<Vec<f32>> = Vec::with_capacity(chunks.len());
    for batch in plan_batches(&chunks, settings.index_batch_size.max(1) as usize) {
        vectors.extend(embed_batch(client, &target, &batch, timeout).await?);
    }
    if vectors.len() != chunks.len() {
        return Err(anyhow!(
            "embedder returned {} vectors for {} chunks",
            vectors.len(),
            chunks.len()
        ));
    }
    let dim = vectors[0].len() as i32;

    // Commit into the next generation and promote it atomically: retrieval
    // keeps serving the current generation until this single transaction
    // commits, so a failure anywhere up to this point is invisible to the data
    // plane, and a crash during the commit itself cannot land the document
    // `ready` without live chunks (see `commit_document_generation`).
    let generation = doc.active_generation + 1;
    let inserts: Vec<ChunkInsert> = chunks
        .into_iter()
        .zip(vectors)
        .enumerate()
        .map(|(i, (c, embedding))| ChunkInsert {
            ordinal: i as i32,
            text: c.text,
            token_count: c.token_count as i32,
            embedding,
        })
        .collect();
    store
        .commit_document_generation(
            collection.id,
            doc.id,
            generation,
            &collection.embedding_model,
            dim,
            &inserts,
        )
        .await?;
    Ok(())
}

/// Poll for pending documents and index them one at a time.
///
/// Settings are read from the store on each iteration rather than accepted as
/// a shared `Arc<ArcSwap<BoonSettings>>`: the proxy's `BoonEngine` already owns
/// its settings inside its own `ArcSwap`, and there is no way for a caller to
/// share that specific instance without introducing a second, independently
/// refreshed settings source that could drift from the one the boon actually
/// reads. Reading Postgres here is fine — this loop already polls every 5s and
/// is explicitly allowed to read Postgres (unlike the request hot path).
///
/// Deliberately ignores `knowledge.enabled`: that flag gates request-path
/// injection (the boon), not corpus preparation. An administrator must be
/// able to upload and index documents before switching the boon on, or
/// enabling it would appear to do nothing while a backlog of documents
/// drained through — so this loop always indexes pending documents
/// regardless of `enabled`.
pub fn spawn_indexer(store: Store, client: reqwest::Client) {
    tokio::spawn(async move {
        loop {
            let knowledge = match store.get_boon_settings().await {
                Ok(settings) => settings.unwrap_or_default().knowledge,
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "knowledge indexer: boon settings read failed; using defaults for this iteration"
                    );
                    BoonSettings::default().knowledge
                }
            };
            match store
                .claim_pending_document(knowledge.index_stale_after_secs)
                .await
            {
                Ok(Some(doc)) => {
                    let id = doc.id;
                    if let Err(e) = index_document(&store, &client, &doc, &knowledge).await {
                        tracing::warn!(document = %id, error = %e, "knowledge indexing failed");
                        let _ = store.mark_document_failed(id, &e.to_string()).await;
                    }
                    // Loop straight back: there may be more work queued.
                    continue;
                }
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "knowledge indexer poll failed"),
            }
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::knowledge::chunk::Chunk;

    fn chunks(n: usize) -> Vec<Chunk> {
        (0..n)
            .map(|i| Chunk {
                text: format!("chunk {i}"),
                token_count: 2,
            })
            .collect()
    }

    #[test]
    fn batches_respect_the_batch_size() {
        let batches = plan_batches(&chunks(10), 4);
        assert_eq!(batches.len(), 3);
        assert_eq!(batches[0].len(), 4);
        assert_eq!(batches[2].len(), 2);
    }

    #[test]
    fn batching_preserves_order_across_batches() {
        // Vectors are matched back to chunks positionally, so any reordering
        // attaches the wrong vector to the wrong text.
        let flat: Vec<String> = plan_batches(&chunks(7), 3).into_iter().flatten().collect();
        assert_eq!(flat[0], "chunk 0");
        assert_eq!(flat[6], "chunk 6");
    }

    #[test]
    fn empty_input_yields_no_batches() {
        assert!(plan_batches(&[], 4).is_empty());
    }

    #[test]
    fn cap_rejects_a_document_that_would_exceed_the_limit() {
        // Refusing loudly is the point: silently truncating a document would
        // make the model confidently answer from half a policy.
        let err = check_chunk_cap(9_990, 20, 10_000).expect_err("must refuse");
        assert!(err.to_string().contains("chunk limit"));
    }

    #[test]
    fn cap_allows_a_document_that_fits() {
        assert!(check_chunk_cap(9_000, 20, 10_000).is_ok());
    }
}
