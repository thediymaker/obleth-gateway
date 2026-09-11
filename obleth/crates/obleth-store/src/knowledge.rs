//! Knowledge-base storage: collections, documents, chunks, and chunk vectors.
//!
//! Vectors are stored as little-endian f32 `bytea` rather than pgvector. The
//! request hot path may not read Postgres, so vectors are only ever *stored*
//! here and searched in the proxy's in-memory slab — which means the extension
//! would buy nothing while costing every self-hoster a deployment requirement.

use std::collections::HashMap;

use sqlx::Row;
use uuid::Uuid;

use crate::{Result, Store, StoreError};

#[derive(Debug, Clone)]
pub struct KnowledgeCollection {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub embedding_model: String,
    pub indexed_embedding_model: String,
    pub embedding_dim: i32,
    pub chunk_tokens: i32,
    pub chunk_overlap_tokens: i32,
    pub version: i64,
}

impl KnowledgeCollection {
    /// True when the operator changed the embedder but the active generation
    /// was built with a different one. Retrieval keeps working on the old
    /// vectors until a re-index actually runs.
    pub fn needs_reindex(&self) -> bool {
        !self.indexed_embedding_model.is_empty()
            && self.indexed_embedding_model != self.embedding_model
    }
}

fn row_to_collection(row: &sqlx::postgres::PgRow) -> KnowledgeCollection {
    KnowledgeCollection {
        id: row.get("id"),
        name: row.get("name"),
        description: row.get("description"),
        embedding_model: row.get("embedding_model"),
        indexed_embedding_model: row.get("indexed_embedding_model"),
        embedding_dim: row.get("embedding_dim"),
        chunk_tokens: row.get("chunk_tokens"),
        chunk_overlap_tokens: row.get("chunk_overlap_tokens"),
        version: row.get("version"),
    }
}

const COLLECTION_COLS: &str = "id, name, description, embedding_model, \
    indexed_embedding_model, embedding_dim, chunk_tokens, chunk_overlap_tokens, \
    version";

impl Store {
    pub async fn create_collection(
        &self,
        name: &str,
        description: &str,
        embedding_model: &str,
    ) -> Result<KnowledgeCollection> {
        let row = sqlx::query(&format!(
            "insert into knowledge_collections (id, name, description, embedding_model)
             values ($1, $2, $3, $4) returning {COLLECTION_COLS}"
        ))
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(description)
        .bind(embedding_model)
        .fetch_one(self.pool())
        .await?;
        Ok(row_to_collection(&row))
    }

    pub async fn list_collections(&self) -> Result<Vec<KnowledgeCollection>> {
        let rows = sqlx::query(&format!(
            "select {COLLECTION_COLS} from knowledge_collections order by name"
        ))
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(row_to_collection).collect())
    }

    pub async fn get_collection(&self, id: Uuid) -> Result<KnowledgeCollection> {
        let row = sqlx::query(&format!(
            "select {COLLECTION_COLS} from knowledge_collections where id = $1"
        ))
        .bind(id)
        .fetch_one(self.pool())
        .await?;
        Ok(row_to_collection(&row))
    }

    pub async fn update_collection(
        &self,
        id: Uuid,
        name: &str,
        description: &str,
        embedding_model: &str,
        chunk_tokens: i32,
        chunk_overlap_tokens: i32,
    ) -> Result<KnowledgeCollection> {
        let row = sqlx::query(&format!(
            "update knowledge_collections
                set name = $2, description = $3, embedding_model = $4,
                    chunk_tokens = $5, chunk_overlap_tokens = $6, updated_at = now()
              where id = $1 returning {COLLECTION_COLS}"
        ))
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(embedding_model)
        .bind(chunk_tokens)
        .bind(chunk_overlap_tokens)
        .fetch_one(self.pool())
        .await?;
        Ok(row_to_collection(&row))
    }

    /// Record which embedder built the active generation. Called only at
    /// generation flip, never when the operator edits the desired embedder.
    pub async fn set_indexed_embedder(
        &self,
        id: Uuid,
        model: &str,
        dim: i32,
    ) -> Result<KnowledgeCollection> {
        let row = sqlx::query(&format!(
            "update knowledge_collections
                set indexed_embedding_model = $2, embedding_dim = $3, updated_at = now()
              where id = $1 returning {COLLECTION_COLS}"
        ))
        .bind(id)
        .bind(model)
        .bind(dim)
        .fetch_one(self.pool())
        .await?;
        Ok(row_to_collection(&row))
    }

    pub async fn delete_collection(&self, id: Uuid) -> Result<()> {
        sqlx::query("delete from knowledge_collections where id = $1")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}

/// Encode a vector as little-endian f32 bytes for the `bytea` column.
pub fn encode_vector(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Decode a `bytea` vector. A length that is not a multiple of 4 means the row
/// is corrupt; return empty rather than a misaligned vector, because cosine
/// against a shifted vector produces a plausible-looking wrong answer instead
/// of an error.
pub fn decode_vector(bytes: &[u8]) -> Vec<f32> {
    if !bytes.len().is_multiple_of(4) {
        return Vec::new();
    }
    bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

#[derive(Debug, Clone)]
pub struct KnowledgeDocument {
    pub id: Uuid,
    pub collection_id: Uuid,
    pub title: String,
    pub filename: String,
    pub content_type: String,
    pub content: String,
    pub byte_size: i64,
    pub status: String,
    pub error: Option<String>,
    pub chunk_count: i32,
    pub active_generation: i32,
    pub indexed_embedding_model: String,
    pub indexing_started_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// One chunk awaiting insert. `embedding` is already normalized by the indexer.
#[derive(Debug, Clone)]
pub struct ChunkInsert {
    pub ordinal: i32,
    pub text: String,
    pub token_count: i32,
    pub embedding: Vec<f32>,
}

/// A chunk loaded for the proxy slab or the admin preview.
#[derive(Debug, Clone)]
pub struct KnowledgeChunk {
    pub id: Uuid,
    pub document_id: Uuid,
    pub text: String,
    pub token_count: i32,
    pub embedding: Vec<f32>,
}

const DOC_COLS: &str = "id, collection_id, title, filename, content_type, content, \
    byte_size, status, error, chunk_count, active_generation, indexed_embedding_model, \
    indexing_started_at";

fn row_to_document(row: &sqlx::postgres::PgRow) -> KnowledgeDocument {
    KnowledgeDocument {
        id: row.get("id"),
        collection_id: row.get("collection_id"),
        title: row.get("title"),
        filename: row.get("filename"),
        content_type: row.get("content_type"),
        content: row.get("content"),
        byte_size: row.get("byte_size"),
        status: row.get("status"),
        error: row.get("error"),
        chunk_count: row.get("chunk_count"),
        active_generation: row.get("active_generation"),
        indexed_embedding_model: row.get("indexed_embedding_model"),
        indexing_started_at: row.get("indexing_started_at"),
    }
}

fn content_hash(content: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(content.as_bytes());
    format!("{:x}", h.finalize())
}

impl Store {
    /// Insert a document, or return the existing row when the identical content
    /// already lives in this collection. Re-uploading the same file is a no-op
    /// rather than a duplicate and a wasted re-embed.
    pub async fn upsert_document(
        &self,
        collection_id: Uuid,
        title: &str,
        filename: &str,
        content_type: &str,
        content: &str,
    ) -> Result<KnowledgeDocument> {
        let hash = content_hash(content);
        let row = sqlx::query(&format!(
            "insert into knowledge_documents
                 (id, collection_id, title, filename, content_type, content,
                  content_hash, byte_size, status)
             values ($1, $2, $3, $4, $5, $6, $7, $8, 'pending')
             on conflict (collection_id, content_hash) do update
                 set title = knowledge_documents.title
             returning {DOC_COLS}"
        ))
        .bind(Uuid::new_v4())
        .bind(collection_id)
        .bind(title)
        .bind(filename)
        .bind(content_type)
        .bind(content)
        .bind(&hash)
        .bind(content.len() as i64)
        .fetch_one(self.pool())
        .await?;
        Ok(row_to_document(&row))
    }

    pub async fn list_documents(&self, collection_id: Uuid) -> Result<Vec<KnowledgeDocument>> {
        let rows = sqlx::query(&format!(
            "select {DOC_COLS} from knowledge_documents
              where collection_id = $1 order by created_at desc"
        ))
        .bind(collection_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(row_to_document).collect())
    }

    /// Atomically take one document for indexing.
    ///
    /// Matches `pending` rows, and also `indexing` rows whose claim is older than
    /// `stale_after_secs` — a document stranded by a crash or deploy is otherwise
    /// unreachable, because claiming already moved it off `pending`.
    ///
    /// Recovery is timeout-based rather than a reset at startup: with more than
    /// one gateway replica, a booting replica would otherwise yank a document
    /// another replica is actively embedding. Double-claiming a genuinely slow
    /// document is wasteful but not corrupting — both workers derive the same
    /// generation from `doc.active_generation + 1`, and
    /// `commit_document_generation` replaces that generation's rows wholesale.
    pub async fn claim_pending_document(
        &self,
        stale_after_secs: i64,
    ) -> Result<Option<KnowledgeDocument>> {
        let row = sqlx::query(&format!(
            "update knowledge_documents
                set status = 'indexing', indexing_started_at = now()
              where id = (
                  select id from knowledge_documents
                   where status = 'pending'
                      or (
                          status = 'indexing'
                          and indexing_started_at is not null
                          and indexing_started_at
                              < now() - make_interval(secs => $1::double precision)
                      )
                   order by created_at
                   for update skip locked
                   limit 1
              ) returning {DOC_COLS}"
        ))
        .bind(stale_after_secs)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(row_to_document))
    }

    pub async fn mark_document_failed(&self, id: Uuid, error: &str) -> Result<()> {
        sqlx::query("update knowledge_documents set status = 'failed', error = $2 where id = $1")
            .bind(id)
            .bind(error)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn delete_document(&self, id: Uuid) -> Result<Uuid> {
        let row =
            sqlx::query("delete from knowledge_documents where id = $1 returning collection_id")
                .bind(id)
                .fetch_one(self.pool())
                .await?;
        Ok(row.get("collection_id"))
    }

    /// Commit one document's freshly embedded chunks and make them live, in a
    /// single transaction.
    ///
    /// This is deliberately ONE method rather than a write step followed by a
    /// flip step. When those were separate (`write_generation` then
    /// `flip_document_generation`), a crash in between left the document
    /// `ready` with its generation un-promoted — neither `pending` nor
    /// `indexing`, so no retry path could ever reclaim it, while retrieval saw
    /// either nothing (first index) or permanently stale content (re-index).
    ///
    /// Because everything commits together, a crash anywhere leaves the
    /// document still `indexing`, which the staleness reclaim in
    /// `claim_pending_document` picks up.
    ///
    /// The delete below is scoped to `document_id`, not `collection_id`:
    /// generation numbers are per-document, so a collection-wide delete on
    /// `generation <> $2` would drop every other document's chunks too — that
    /// was the Task 2 round-1 defect. `<>` rather than `<` is intentional and
    /// safe here: it also garbage-collects a higher generation orphaned by a
    /// crashed indexer, and two indexers can never hold the same document
    /// concurrently because `claim_pending_document` flips it to `indexing`
    /// under `for update skip locked` first.
    ///
    /// The collection-level `indexed_embedding_model` advances only once EVERY
    /// document in the collection has been built with the collection's desired
    /// `embedding_model`. That gate is what keeps a collection-wide re-embed
    /// from ever exposing two embedding spaces at once: documents already moved
    /// to the new embedder are excluded from the slab (see
    /// `load_active_chunks`) until the last one lands, so retrieval serves the
    /// complete old set throughout and switches atomically.
    pub async fn commit_document_generation(
        &self,
        collection_id: Uuid,
        document_id: Uuid,
        generation: i32,
        embedding_model: &str,
        embedding_dim: i32,
        chunks: &[ChunkInsert],
    ) -> Result<()> {
        if chunks.is_empty() {
            return Err(StoreError::Conflict("document produced no chunks".into()));
        }
        let mut tx = self.pool().begin().await?;

        // Replace anything already written for this generation, so a retry
        // after a partial run is idempotent.
        sqlx::query("delete from knowledge_chunks where document_id = $1 and generation = $2")
            .bind(document_id)
            .bind(generation)
            .execute(&mut *tx)
            .await?;
        for c in chunks {
            sqlx::query(
                "insert into knowledge_chunks
                     (id, document_id, collection_id, generation, ordinal, text,
                      token_count, embedding, embedding_dim)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(Uuid::new_v4())
            .bind(document_id)
            .bind(collection_id)
            .bind(generation)
            .bind(c.ordinal)
            .bind(&c.text)
            .bind(c.token_count)
            .bind(encode_vector(&c.embedding))
            .bind(c.embedding.len() as i32)
            .execute(&mut *tx)
            .await?;
        }

        // Drop this document's other generations. Scoped to `document_id`:
        // other documents' chunks are untouched.
        sqlx::query("delete from knowledge_chunks where document_id = $1 and generation <> $2")
            .bind(document_id)
            .bind(generation)
            .execute(&mut *tx)
            .await?;

        // Promote the generation and mark the document ready together.
        sqlx::query(
            "update knowledge_documents
                set active_generation = $2, indexed_embedding_model = $3,
                    status = 'ready', error = null, chunk_count = $4,
                    indexed_at = now()
              where id = $1",
        )
        .bind(document_id)
        .bind(generation)
        .bind(embedding_model)
        .bind(chunks.len() as i32)
        .execute(&mut *tx)
        .await?;

        // Advance the collection's embedder only when no document lags behind.
        // Deliberately does NOT touch `version` — see the unconditional bump below.
        sqlx::query(
            "update knowledge_collections c
                set indexed_embedding_model = $2, embedding_dim = $3, updated_at = now()
              where c.id = $1
                and not exists (
                    select 1 from knowledge_documents d
                     where d.collection_id = c.id
                       and d.status = 'ready'
                       and d.indexed_embedding_model <> c.embedding_model
                )",
        )
        .bind(collection_id)
        .bind(embedding_model)
        .bind(embedding_dim)
        .execute(&mut *tx)
        .await?;

        // Bump `version` unconditionally and exactly once. This document's chunks
        // changed, so every proxy must rebuild its slab regardless of whether the
        // collection's embedder advanced. (An earlier two-conditional-statement
        // form left a reachable gap where neither statement fired and version
        // never moved — see the correction doc for the live-Postgres proof.)
        sqlx::query(
            "update knowledge_collections set version = version + 1, updated_at = now()
              where id = $1",
        )
        .bind(collection_id)
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(())
    }

    /// Bump `version` without a generation change, for deletes.
    pub async fn bump_collection_version(&self, collection_id: Uuid) -> Result<()> {
        sqlx::query(
            "update knowledge_collections set version = version + 1, updated_at = now()
              where id = $1",
        )
        .bind(collection_id)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Chunks retrievable right now: each document's own active generation,
    /// gated to documents whose embedder matches the collection's — a document
    /// mid-reindex to a new embedder is excluded until the whole collection has
    /// caught up (see `commit_document_generation`), so the count never includes
    /// a mixed embedding space.
    pub async fn collection_chunk_count(&self, collection_id: Uuid) -> Result<i64> {
        let row = sqlx::query(
            "select count(*) as n
               from knowledge_chunks k
               join knowledge_documents d on d.id = k.document_id
               join knowledge_collections c on c.id = k.collection_id
              where k.collection_id = $1
                and k.generation = d.active_generation
                and d.indexed_embedding_model = c.indexed_embedding_model",
        )
        .bind(collection_id)
        .fetch_one(self.pool())
        .await?;
        Ok(row.get("n"))
    }

    /// Chunk count and approximate stored bytes per collection, for the
    /// dashboard's size display. Same per-document-generation, embedder-matched
    /// scope as `collection_chunk_count`, so the reported size matches what is
    /// actually retrievable.
    pub async fn collection_sizes(&self) -> Result<Vec<(Uuid, i64, i64)>> {
        let rows = sqlx::query(
            "select c.id as id,
                    count(k.id) as chunks,
                    coalesce(sum(length(k.text) + octet_length(k.embedding)), 0) as bytes
               from knowledge_collections c
               left join knowledge_documents d on d.collection_id = c.id
               left join knowledge_chunks k
                 on k.document_id = d.id
                and k.generation = d.active_generation
                and d.indexed_embedding_model = c.indexed_embedding_model
              group by c.id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get("id"), r.get("chunks"), r.get("bytes")))
            .collect())
    }

    /// Load a collection's retrievable chunks for the proxy slab: each
    /// document's own active generation, gated to documents whose embedder
    /// matches the collection's (the mixed-embedding-space guard — see
    /// `commit_document_generation`). Ordered by document then ordinal so chunk
    /// order is stable within a document.
    /// Background-loop use only — never call this from the request path.
    pub async fn load_active_chunks(&self, collection_id: Uuid) -> Result<Vec<KnowledgeChunk>> {
        let rows = sqlx::query(
            "select k.id as id, k.document_id as document_id, k.text as text,
                    k.token_count as token_count, k.embedding as embedding
               from knowledge_chunks k
               join knowledge_documents d on d.id = k.document_id
               join knowledge_collections c on c.id = k.collection_id
              where k.collection_id = $1
                and k.generation = d.active_generation
                and d.indexed_embedding_model = c.indexed_embedding_model
              order by k.document_id, k.ordinal",
        )
        .bind(collection_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                let bytes: Vec<u8> = r.get("embedding");
                KnowledgeChunk {
                    id: r.get("id"),
                    document_id: r.get("document_id"),
                    text: r.get("text"),
                    token_count: r.get("token_count"),
                    embedding: decode_vector(&bytes),
                }
            })
            .collect())
    }

    /// Collections a model grounds on. Used by the admin API to render the
    /// current attachment set for one model.
    pub async fn model_collection_ids(&self, model_id: Uuid) -> Result<Vec<Uuid>> {
        let rows = sqlx::query(
            "select collection_id from model_knowledge_collections where model_id = $1",
        )
        .bind(model_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(|r| r.get("collection_id")).collect())
    }

    /// Every model's collection attachments in one query, grouped by model.
    ///
    /// Used by `Store::all_resolved_models` (the proxy boot warm path, and the
    /// 15s registry refresh) so cost stays flat regardless of model count —
    /// one query over the whole join table rather than one per model.
    pub async fn all_model_collection_ids(&self) -> Result<HashMap<Uuid, Vec<Uuid>>> {
        let rows = sqlx::query("select model_id, collection_id from model_knowledge_collections")
            .fetch_all(self.pool())
            .await?;
        let mut out: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
        for row in &rows {
            out.entry(row.get("model_id"))
                .or_default()
                .push(row.get("collection_id"));
        }
        Ok(out)
    }

    /// Replace a model's full set of knowledge-collection attachments.
    pub async fn set_model_collections(&self, model_id: Uuid, ids: &[Uuid]) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("delete from model_knowledge_collections where model_id = $1")
            .bind(model_id)
            .execute(&mut *tx)
            .await?;
        for id in ids {
            sqlx::query(
                "insert into model_knowledge_collections (model_id, collection_id)
                 values ($1, $2) on conflict do nothing",
            )
            .bind(model_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    /// Reset every document in a collection back to `pending`, clearing any
    /// prior error, so a manual re-index requeues the whole collection for the
    /// background indexer. Returns the number of documents requeued.
    pub async fn reindex_collection_documents(&self, collection_id: Uuid) -> Result<u64> {
        let result = sqlx::query(
            "update knowledge_documents set status = 'pending', error = null
              where collection_id = $1",
        )
        .bind(collection_id)
        .execute(self.pool())
        .await?;
        Ok(result.rows_affected())
    }

    /// Reset a single document back to `pending`, clearing any prior error.
    pub async fn reindex_document(&self, id: Uuid) -> Result<KnowledgeDocument> {
        let row = sqlx::query(&format!(
            "update knowledge_documents set status = 'pending', error = null
              where id = $1 returning {DOC_COLS}"
        ))
        .bind(id)
        .fetch_one(self.pool())
        .await?;
        Ok(row_to_document(&row))
    }
}

#[cfg(test)]
mod tests {
    /// Requires a live Postgres; skipped when TEST_DATABASE_URL is unset so the
    /// unit suite stays runnable without infrastructure.
    async fn test_store() -> Option<crate::Store> {
        let url = std::env::var("TEST_DATABASE_URL").ok()?;
        let store = crate::Store::connect(&url).await.ok()?;
        store.migrate().await.ok()?;
        Some(store)
    }

    #[tokio::test]
    async fn migrate_is_rerunnable() {
        let Some(store) = test_store().await else {
            return;
        };
        // Every migration runs on every boot; running twice must not error.
        store.migrate().await.expect("second migrate");
    }

    #[tokio::test]
    async fn create_and_get_collection() {
        let Some(store) = test_store().await else {
            return;
        };
        let name = format!("policies-{}", uuid::Uuid::new_v4());
        let created = store
            .create_collection(&name, "HR policies", "qwen4-embedding")
            .await
            .expect("create");
        assert_eq!(created.name, name);
        assert_eq!(created.embedding_model, "qwen4-embedding");
        assert_eq!(created.version, 0);
        // Not yet indexed, so there is no embedder on record and no dimension.
        assert_eq!(created.indexed_embedding_model, "");
        assert_eq!(created.embedding_dim, 0);

        let fetched = store.get_collection(created.id).await.expect("get");
        assert_eq!(fetched.id, created.id);

        store.delete_collection(created.id).await.expect("delete");
        assert!(store.get_collection(created.id).await.is_err());
    }

    #[tokio::test]
    async fn needs_reindex_when_embedder_differs() {
        let Some(store) = test_store().await else {
            return;
        };
        let name = format!("catalog-{}", uuid::Uuid::new_v4());
        let c = store
            .create_collection(&name, "", "embed-a")
            .await
            .expect("create");
        // Nothing indexed yet, so this is not a re-index prompt.
        assert!(!c.needs_reindex());

        let c = store
            .set_indexed_embedder(c.id, "embed-a", 768)
            .await
            .expect("flip");
        assert!(!c.needs_reindex());

        let c = store
            .update_collection(c.id, &c.name, &c.description, "embed-b", 400, 50)
            .await
            .expect("update");
        assert!(
            c.needs_reindex(),
            "changing the embedder must flag re-index"
        );

        store.delete_collection(c.id).await.ok();
    }

    #[test]
    fn vector_roundtrips_through_bytea() {
        let v = vec![0.5f32, -0.25, 0.0, 1.0];
        let bytes = super::encode_vector(&v);
        assert_eq!(bytes.len(), v.len() * 4);
        assert_eq!(super::decode_vector(&bytes), v);
    }

    #[test]
    fn decode_rejects_truncated_vector() {
        // A short read must yield nothing rather than a silently wrong vector:
        // cosine against a misaligned vector is wrong, not an error.
        assert!(super::decode_vector(&[0u8; 7]).is_empty());
    }

    #[tokio::test]
    async fn upsert_document_dedupes_by_content_hash() {
        let Some(store) = test_store().await else {
            return;
        };
        let c = store
            .create_collection(&format!("dedupe-{}", uuid::Uuid::new_v4()), "", "e")
            .await
            .expect("collection");

        let a = store
            .upsert_document(c.id, "Policy", "policy.md", "text/markdown", "same body")
            .await
            .expect("first");
        let b = store
            .upsert_document(
                c.id,
                "Policy renamed",
                "policy.md",
                "text/markdown",
                "same body",
            )
            .await
            .expect("second");
        assert_eq!(a.id, b.id, "identical content must not create a second row");

        let docs = store.list_documents(c.id).await.expect("list");
        assert_eq!(docs.len(), 1);
        store.delete_collection(c.id).await.ok();
    }

    #[tokio::test]
    async fn indexing_a_second_document_preserves_the_first() {
        let Some(store) = test_store().await else {
            return;
        };
        let c = store
            .create_collection(&format!("multi-{}", uuid::Uuid::new_v4()), "", "embed-a")
            .await
            .expect("collection");
        let a = store
            .upsert_document(c.id, "Doc A", "a.md", "text/markdown", "body a")
            .await
            .expect("doc a");
        let b = store
            .upsert_document(c.id, "Doc B", "b.md", "text/markdown", "body b")
            .await
            .expect("doc b");

        store
            .commit_document_generation(
                c.id,
                a.id,
                1,
                "embed-a",
                2,
                &[super::ChunkInsert {
                    ordinal: 0,
                    text: "chunk a".into(),
                    token_count: 2,
                    embedding: vec![1.0, 0.0],
                }],
            )
            .await
            .expect("commit a");

        store
            .commit_document_generation(
                c.id,
                b.id,
                1,
                "embed-a",
                2,
                &[super::ChunkInsert {
                    ordinal: 0,
                    text: "chunk b".into(),
                    token_count: 2,
                    embedding: vec![0.0, 1.0],
                }],
            )
            .await
            .expect("commit b");

        // The original defect: indexing Doc B deleted Doc A's chunks.
        let chunks = store.load_active_chunks(c.id).await.expect("load");
        assert_eq!(chunks.len(), 2, "both documents must remain retrievable");
        let texts: Vec<&str> = chunks.iter().map(|k| k.text.as_str()).collect();
        assert!(texts.contains(&"chunk a"));
        assert!(texts.contains(&"chunk b"));

        store.delete_collection(c.id).await.ok();
    }

    #[tokio::test]
    async fn a_documents_own_reindex_replaces_only_its_chunks() {
        let Some(store) = test_store().await else {
            return;
        };
        let c = store
            .create_collection(&format!("reidx-{}", uuid::Uuid::new_v4()), "", "embed-a")
            .await
            .expect("collection");
        let a = store
            .upsert_document(c.id, "Doc A", "a.md", "text/markdown", "body a")
            .await
            .expect("doc a");

        store
            .commit_document_generation(
                c.id,
                a.id,
                1,
                "embed-a",
                2,
                &[super::ChunkInsert {
                    ordinal: 0,
                    text: "old".into(),
                    token_count: 1,
                    embedding: vec![1.0, 0.0],
                }],
            )
            .await
            .expect("gen 1");

        store
            .commit_document_generation(
                c.id,
                a.id,
                2,
                "embed-a",
                2,
                &[super::ChunkInsert {
                    ordinal: 0,
                    text: "new".into(),
                    token_count: 1,
                    embedding: vec![0.0, 1.0],
                }],
            )
            .await
            .expect("gen 2");

        let chunks = store.load_active_chunks(c.id).await.expect("load");
        assert_eq!(chunks.len(), 1, "the old generation must be gone");
        assert_eq!(chunks[0].text, "new");

        store.delete_collection(c.id).await.ok();
    }

    #[tokio::test]
    async fn flip_bumps_version_even_when_the_embedder_does_not_advance() {
        let Some(store) = test_store().await else {
            return;
        };
        let c = store
            .create_collection(&format!("bump-{}", uuid::Uuid::new_v4()), "", "embed-a")
            .await
            .expect("collection");
        let a = store
            .upsert_document(c.id, "Doc A", "a.md", "text/markdown", "body a")
            .await
            .expect("doc a");
        store
            .commit_document_generation(
                c.id,
                a.id,
                1,
                "embed-a",
                2,
                &[super::ChunkInsert {
                    ordinal: 0,
                    text: "a".into(),
                    token_count: 1,
                    embedding: vec![1.0, 0.0],
                }],
            )
            .await
            .expect("commit 1");
        let after_first = store.get_collection(c.id).await.expect("get").version;

        // Second commit with identical embedder and dimension: the collection's
        // embedder cannot advance, but version MUST still move or the proxy
        // never rebuilds its slab.
        store
            .commit_document_generation(
                c.id,
                a.id,
                2,
                "embed-a",
                2,
                &[super::ChunkInsert {
                    ordinal: 0,
                    text: "a2".into(),
                    token_count: 1,
                    embedding: vec![0.0, 1.0],
                }],
            )
            .await
            .expect("commit 2");
        let after_second = store.get_collection(c.id).await.expect("get").version;

        assert!(
            after_second > after_first,
            "version must bump on every commit"
        );

        store.delete_collection(c.id).await.ok();
    }

    #[tokio::test]
    async fn a_stranded_indexing_document_is_reclaimed_after_the_window() {
        let Some(store) = test_store().await else {
            return;
        };
        let c = store
            .create_collection(&format!("stranded-{}", uuid::Uuid::new_v4()), "", "embed-a")
            .await
            .expect("collection");
        let doc = store
            .upsert_document(c.id, "Doc", "d.md", "text/markdown", "body")
            .await
            .expect("doc");

        // Claim it, then simulate the worker dying: the row stays `indexing`.
        let claimed = store.claim_pending_document(1_800).await.expect("claim");
        assert_eq!(claimed.expect("claimed").id, doc.id);

        // With a long window it is NOT reclaimable — no double-processing of a
        // document that is merely slow.
        assert!(
            store
                .claim_pending_document(1_800)
                .await
                .expect("claim")
                .is_none(),
            "a fresh claim must not be stealable"
        );

        // With a zero window the stranded row is reclaimed rather than lost.
        let reclaimed = store.claim_pending_document(0).await.expect("reclaim");
        assert_eq!(
            reclaimed.expect("reclaimed").id,
            doc.id,
            "a stranded indexing row must be reclaimable on timeout"
        );

        store.delete_collection(c.id).await.ok();
    }

    #[tokio::test]
    async fn commit_is_atomic_so_a_document_is_never_ready_without_live_chunks() {
        let Some(store) = test_store().await else {
            return;
        };
        let c = store
            .create_collection(&format!("atomic-{}", uuid::Uuid::new_v4()), "", "embed-a")
            .await
            .expect("collection");
        let doc = store
            .upsert_document(c.id, "Doc", "d.md", "text/markdown", "body")
            .await
            .expect("doc");

        store
            .commit_document_generation(
                c.id,
                doc.id,
                1,
                "embed-a",
                2,
                &[super::ChunkInsert {
                    ordinal: 0,
                    text: "live".into(),
                    token_count: 1,
                    embedding: vec![1.0, 0.0],
                }],
            )
            .await
            .expect("commit");

        // `ready` and retrievable must become true together, never separately.
        let docs = store.list_documents(c.id).await.expect("list");
        assert_eq!(docs[0].status, "ready");
        assert_eq!(docs[0].chunk_count, 1);
        let chunks = store.load_active_chunks(c.id).await.expect("load");
        assert_eq!(chunks.len(), 1, "a ready document must have live chunks");
        assert_eq!(chunks[0].text, "live");

        store.delete_collection(c.id).await.ok();
    }

    /// Insert a minimal `models` row directly (bypassing `create_model`'s long
    /// argument list, which is irrelevant to these attachment tests).
    async fn make_model(store: &crate::Store, name: &str) -> uuid::Uuid {
        let id = uuid::Uuid::new_v4();
        sqlx::query(
            "insert into models (id, model_name, upstream_model, api_base)
             values ($1, $2, 'upstream', 'http://localhost')",
        )
        .bind(id)
        .bind(name)
        .execute(store.pool())
        .await
        .expect("insert model");
        id
    }

    #[tokio::test]
    async fn model_collections_roundtrip_and_replace() {
        let Some(store) = test_store().await else {
            return;
        };
        let model_id = make_model(&store, &format!("model-{}", uuid::Uuid::new_v4())).await;
        let c1 = store
            .create_collection(&format!("kc-a-{}", uuid::Uuid::new_v4()), "", "embed-a")
            .await
            .expect("c1");
        let c2 = store
            .create_collection(&format!("kc-b-{}", uuid::Uuid::new_v4()), "", "embed-a")
            .await
            .expect("c2");

        store
            .set_model_collections(model_id, &[c1.id, c2.id])
            .await
            .expect("set");
        let mut ids = store.model_collection_ids(model_id).await.expect("get");
        ids.sort();
        let mut expected = vec![c1.id, c2.id];
        expected.sort();
        assert_eq!(ids, expected);

        // Replacing must clear the old set, not append to it.
        store
            .set_model_collections(model_id, &[c1.id])
            .await
            .expect("replace");
        assert_eq!(
            store.model_collection_ids(model_id).await.expect("get2"),
            vec![c1.id]
        );

        store.delete_collection(c1.id).await.ok();
        store.delete_collection(c2.id).await.ok();
        store.delete_model(model_id).await.ok();
    }

    #[tokio::test]
    async fn all_model_collection_ids_groups_by_model_in_one_query() {
        let Some(store) = test_store().await else {
            return;
        };
        let m1 = make_model(&store, &format!("model-a-{}", uuid::Uuid::new_v4())).await;
        let m2 = make_model(&store, &format!("model-b-{}", uuid::Uuid::new_v4())).await;
        let c = store
            .create_collection(&format!("kc-{}", uuid::Uuid::new_v4()), "", "embed-a")
            .await
            .expect("c");

        store
            .set_model_collections(m1, &[c.id])
            .await
            .expect("set m1");
        // m2 deliberately gets nothing attached.

        let all = store.all_model_collection_ids().await.expect("bulk");
        assert_eq!(all.get(&m1), Some(&vec![c.id]));
        assert!(
            !all.contains_key(&m2),
            "a model with no attachments must be absent from the map, not an empty vec"
        );

        store.delete_collection(c.id).await.ok();
        store.delete_model(m1).await.ok();
        store.delete_model(m2).await.ok();
    }

    #[tokio::test]
    async fn reindex_collection_documents_requeues_only_that_collection() {
        let Some(store) = test_store().await else {
            return;
        };
        let a = store
            .create_collection(&format!("reidx-a-{}", uuid::Uuid::new_v4()), "", "embed-a")
            .await
            .expect("a");
        let b = store
            .create_collection(&format!("reidx-b-{}", uuid::Uuid::new_v4()), "", "embed-a")
            .await
            .expect("b");
        let doc_a = store
            .upsert_document(a.id, "Doc A", "a.md", "text/markdown", "body a")
            .await
            .expect("doc a");
        let doc_b = store
            .upsert_document(b.id, "Doc B", "b.md", "text/markdown", "body b")
            .await
            .expect("doc b");
        store
            .mark_document_failed(doc_a.id, "boom")
            .await
            .expect("fail a");
        store
            .mark_document_failed(doc_b.id, "boom")
            .await
            .expect("fail b");

        let n = store
            .reindex_collection_documents(a.id)
            .await
            .expect("reindex a");
        assert_eq!(n, 1);

        let docs_a = store.list_documents(a.id).await.expect("list a");
        assert_eq!(docs_a[0].status, "pending");
        assert!(docs_a[0].error.is_none());

        let docs_b = store.list_documents(b.id).await.expect("list b");
        assert_eq!(
            docs_b[0].status, "failed",
            "a different collection's document must be untouched"
        );

        store.delete_collection(a.id).await.ok();
        store.delete_collection(b.id).await.ok();
    }

    #[tokio::test]
    async fn reindex_document_resets_status_and_clears_error() {
        let Some(store) = test_store().await else {
            return;
        };
        let c = store
            .create_collection(
                &format!("reidx-doc-{}", uuid::Uuid::new_v4()),
                "",
                "embed-a",
            )
            .await
            .expect("c");
        let doc = store
            .upsert_document(c.id, "Doc", "d.md", "text/markdown", "body")
            .await
            .expect("doc");
        store
            .mark_document_failed(doc.id, "boom")
            .await
            .expect("fail");

        let reindexed = store.reindex_document(doc.id).await.expect("reindex");
        assert_eq!(reindexed.status, "pending");
        assert!(reindexed.error.is_none());

        store.delete_collection(c.id).await.ok();
    }
}
