//! Knowledge-base storage: collections, documents, chunks, and chunk vectors.
//!
//! Vectors are stored as little-endian f32 `bytea` rather than pgvector. The
//! request hot path may not read Postgres, so vectors are only ever *stored*
//! here and searched in the proxy's in-memory slab — which means the extension
//! would buy nothing while costing every self-hoster a deployment requirement.

use sqlx::Row;
use uuid::Uuid;

use crate::{Result, Store};

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
    pub active_generation: i32,
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
        active_generation: row.get("active_generation"),
    }
}

const COLLECTION_COLS: &str = "id, name, description, embedding_model, \
    indexed_embedding_model, embedding_dim, chunk_tokens, chunk_overlap_tokens, \
    version, active_generation";

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
    byte_size, status, error, chunk_count";

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

    /// Atomically take one pending document for indexing. `for update skip
    /// locked` means a second indexer (or a replica) never picks the same row.
    pub async fn claim_pending_document(&self) -> Result<Option<KnowledgeDocument>> {
        let row = sqlx::query(&format!(
            "update knowledge_documents set status = 'indexing'
              where id = (
                  select id from knowledge_documents
                   where status = 'pending'
                   order by created_at
                   for update skip locked
                   limit 1
              ) returning {DOC_COLS}"
        ))
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

    /// Write one document's chunks into `generation`, replacing anything
    /// already written there for that document. Retrieval is unaffected until
    /// `flip_generation` runs.
    pub async fn write_generation(
        &self,
        collection_id: Uuid,
        document_id: Uuid,
        generation: i32,
        chunks: &[ChunkInsert],
    ) -> Result<()> {
        let mut tx = self.pool().begin().await?;
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
        sqlx::query(
            "update knowledge_documents
                set status = 'ready', error = null, chunk_count = $2, indexed_at = now()
              where id = $1",
        )
        .bind(document_id)
        .bind(chunks.len() as i32)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Promote `generation` to active, record the embedder that built it, bump
    /// `version` to trigger a slab rebuild, and drop every older generation —
    /// all in one transaction, so no reader ever observes mixed vectors.
    pub async fn flip_generation(
        &self,
        collection_id: Uuid,
        generation: i32,
        embedding_model: &str,
        embedding_dim: i32,
    ) -> Result<KnowledgeCollection> {
        let mut tx = self.pool().begin().await?;
        let row = sqlx::query(&format!(
            "update knowledge_collections
                set active_generation = $2, indexed_embedding_model = $3,
                    embedding_dim = $4, version = version + 1, updated_at = now()
              where id = $1 returning {COLLECTION_COLS}"
        ))
        .bind(collection_id)
        .bind(generation)
        .bind(embedding_model)
        .bind(embedding_dim)
        .fetch_one(&mut *tx)
        .await?;
        sqlx::query("delete from knowledge_chunks where collection_id = $1 and generation <> $2")
            .bind(collection_id)
            .bind(generation)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(row_to_collection(&row))
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

    pub async fn collection_chunk_count(&self, collection_id: Uuid) -> Result<i64> {
        let row = sqlx::query(
            "select count(*) as n from knowledge_chunks
              where collection_id = $1
                and generation = (select active_generation from knowledge_collections where id = $1)",
        )
        .bind(collection_id)
        .fetch_one(self.pool())
        .await?;
        Ok(row.get("n"))
    }

    /// Chunk count and approximate stored bytes per collection, for the
    /// dashboard's size display.
    pub async fn collection_sizes(&self) -> Result<Vec<(Uuid, i64, i64)>> {
        let rows = sqlx::query(
            "select c.id as id,
                    count(k.id) as chunks,
                    coalesce(sum(length(k.text) + octet_length(k.embedding)), 0) as bytes
               from knowledge_collections c
               left join knowledge_chunks k
                 on k.collection_id = c.id and k.generation = c.active_generation
              group by c.id",
        )
        .fetch_all(self.pool())
        .await?;
        Ok(rows
            .iter()
            .map(|r| (r.get("id"), r.get("chunks"), r.get("bytes")))
            .collect())
    }

    /// Load a collection's active-generation chunks for the proxy slab.
    /// Background-loop use only — never call this from the request path.
    pub async fn load_active_chunks(&self, collection_id: Uuid) -> Result<Vec<KnowledgeChunk>> {
        let rows = sqlx::query(
            "select k.id as id, k.document_id as document_id, k.text as text,
                    k.token_count as token_count, k.embedding as embedding
               from knowledge_chunks k
               join knowledge_collections c on c.id = k.collection_id
              where k.collection_id = $1 and k.generation = c.active_generation
              order by k.ordinal",
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
        assert_eq!(created.active_generation, 0);
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
    async fn flip_generation_swaps_atomically_and_drops_the_old() {
        let Some(store) = test_store().await else {
            return;
        };
        let c = store
            .create_collection(&format!("gen-{}", uuid::Uuid::new_v4()), "", "e")
            .await
            .expect("collection");
        let doc = store
            .upsert_document(c.id, "Doc", "d.md", "text/markdown", "body")
            .await
            .expect("doc");

        store
            .write_generation(
                c.id,
                doc.id,
                1,
                &[super::ChunkInsert {
                    ordinal: 0,
                    text: "generation one".into(),
                    token_count: 3,
                    embedding: vec![1.0, 0.0],
                }],
            )
            .await
            .expect("write gen 1");

        // Before the flip the active generation is still 0, so the slab loader
        // sees nothing — this is what keeps a crashed re-index invisible.
        assert!(store
            .load_active_chunks(c.id)
            .await
            .expect("load")
            .is_empty());

        let c = store.flip_generation(c.id, 1, "e", 2).await.expect("flip");
        assert_eq!(c.active_generation, 1);
        assert_eq!(c.embedding_dim, 2);
        assert!(
            c.version > 0,
            "flip must bump version to trigger a slab rebuild"
        );

        let chunks = store.load_active_chunks(c.id).await.expect("load");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "generation one");
        assert_eq!(chunks[0].embedding, vec![1.0, 0.0]);

        store.delete_collection(c.id).await.ok();
    }
}
