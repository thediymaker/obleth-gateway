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
}
