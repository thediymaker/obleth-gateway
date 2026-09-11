//! Knowledge base: collection and document management, chunking, indexing.

pub mod chunk;
pub mod embed;
pub mod indexer;

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::Json;
use base64::Engine;
use obleth_config::{BoonSettings, KnowledgeBoonSettings};
use obleth_store::knowledge::{score_against, KnowledgeCollection, KnowledgeDocument};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{audit_actor, sync_model, AdminError, AdminState, Result};

/// Reject an upload before decoding, then require valid UTF-8. Binary formats
/// are a later addition; today an undecodable file is a clear error rather
/// than a corpus full of mojibake.
pub fn validate_upload(bytes: &[u8], max_bytes: i64) -> std::result::Result<String, String> {
    if bytes.len() as i64 > max_bytes {
        return Err(format!(
            "file too large: {} bytes exceeds the {max_bytes}-byte limit",
            bytes.len()
        ));
    }
    String::from_utf8(bytes.to_vec())
        .map_err(|_| "file is not valid UTF-8 text; only .md, .txt and .csv are supported".into())
}

pub fn content_type_for(filename: &str) -> &'static str {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        _ => "text/plain",
    }
}

// ---- collections ----------------------------------------------------------

/// Dashboard-facing view of a collection. Carries the derived `needs_reindex`
/// flag plus the retrievable chunk count and approximate stored bytes (from
/// `Store::collection_sizes`), which the raw `KnowledgeCollection` row does
/// not know how to compute itself.
#[derive(Debug, Serialize, ToSchema)]
pub struct CollectionView {
    pub id: Uuid,
    pub name: String,
    pub description: String,
    pub embedding_model: String,
    pub indexed_embedding_model: String,
    pub embedding_dim: i32,
    pub chunk_tokens: i32,
    pub chunk_overlap_tokens: i32,
    pub needs_reindex: bool,
    pub chunk_count: i64,
    pub estimated_bytes: i64,
}

impl CollectionView {
    fn from_collection(c: &KnowledgeCollection, chunk_count: i64, estimated_bytes: i64) -> Self {
        CollectionView {
            id: c.id,
            name: c.name.clone(),
            description: c.description.clone(),
            embedding_model: c.embedding_model.clone(),
            indexed_embedding_model: c.indexed_embedding_model.clone(),
            embedding_dim: c.embedding_dim,
            chunk_tokens: c.chunk_tokens,
            chunk_overlap_tokens: c.chunk_overlap_tokens,
            needs_reindex: c.needs_reindex(),
            chunk_count,
            estimated_bytes,
        }
    }
}

/// Look up one collection's size in the bulk `collection_sizes` result,
/// defaulting to zero for a brand-new collection that has not been sized yet.
async fn view_with_size(state: &AdminState, c: KnowledgeCollection) -> Result<CollectionView> {
    let (chunk_count, estimated_bytes) = state
        .store
        .collection_sizes()
        .await?
        .into_iter()
        .find(|(id, _, _)| *id == c.id)
        .map(|(_, chunks, bytes)| (chunks, bytes))
        .unwrap_or((0, 0));
    Ok(CollectionView::from_collection(
        &c,
        chunk_count,
        estimated_bytes,
    ))
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateCollection {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub embedding_model: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateCollection {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub embedding_model: Option<String>,
    #[serde(default)]
    pub chunk_tokens: Option<i32>,
    #[serde(default)]
    pub chunk_overlap_tokens: Option<i32>,
}

#[utoipa::path(
    get, path = "/api/v1/knowledge/collections", tag = "knowledge",
    responses((status = 200, body = [CollectionView]))
)]
pub async fn list_collections(
    State(state): State<AdminState>,
) -> Result<Json<Vec<CollectionView>>> {
    let collections = state.store.list_collections().await?;
    let sizes: HashMap<Uuid, (i64, i64)> = state
        .store
        .collection_sizes()
        .await?
        .into_iter()
        .map(|(id, chunks, bytes)| (id, (chunks, bytes)))
        .collect();
    let views = collections
        .iter()
        .map(|c| {
            let (chunk_count, estimated_bytes) = sizes.get(&c.id).copied().unwrap_or((0, 0));
            CollectionView::from_collection(c, chunk_count, estimated_bytes)
        })
        .collect();
    Ok(Json(views))
}

#[utoipa::path(
    post, path = "/api/v1/knowledge/collections", tag = "knowledge",
    request_body = CreateCollection,
    responses((status = 200, body = CollectionView))
)]
pub async fn create_collection(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateCollection>,
) -> Result<Json<CollectionView>> {
    let name = body.name.trim();
    if name.is_empty() {
        return Err(AdminError::BadRequest("name is required".into()));
    }
    let embedding_model = body.embedding_model.trim();
    if embedding_model.is_empty() {
        return Err(AdminError::BadRequest("embedding_model is required".into()));
    }
    let collection = state
        .store
        .create_collection(name, body.description.trim(), embedding_model)
        .await?;
    let view = CollectionView::from_collection(&collection, 0, 0);
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "create_knowledge_collection",
            "knowledge_collection",
            &collection.id.to_string(),
            serde_json::to_value(&view).unwrap_or_default(),
        )
        .await?;
    Ok(Json(view))
}

#[utoipa::path(
    get, path = "/api/v1/knowledge/collections/{id}", tag = "knowledge",
    params(("id" = Uuid, Path, description = "Collection id")),
    responses((status = 200, body = CollectionView), (status = 404))
)]
pub async fn get_collection(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<CollectionView>> {
    let collection = state.store.get_collection(id).await?;
    Ok(Json(view_with_size(&state, collection).await?))
}

#[utoipa::path(
    put, path = "/api/v1/knowledge/collections/{id}", tag = "knowledge",
    params(("id" = Uuid, Path, description = "Collection id")),
    request_body = UpdateCollection,
    responses((status = 200, body = CollectionView))
)]
pub async fn update_collection(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateCollection>,
) -> Result<Json<CollectionView>> {
    let existing = state.store.get_collection(id).await?;
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing.name);
    let description = body.description.as_deref().unwrap_or(&existing.description);
    let embedding_model = body
        .embedding_model
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(&existing.embedding_model);
    let chunk_tokens = body
        .chunk_tokens
        .filter(|n| *n > 0)
        .unwrap_or(existing.chunk_tokens);
    let chunk_overlap_tokens = body
        .chunk_overlap_tokens
        .filter(|n| *n >= 0)
        .unwrap_or(existing.chunk_overlap_tokens);
    if chunk_overlap_tokens >= chunk_tokens {
        return Err(AdminError::BadRequest(
            "chunk_overlap_tokens must be less than chunk_tokens".into(),
        ));
    }
    let collection = state
        .store
        .update_collection(
            id,
            name,
            description,
            embedding_model,
            chunk_tokens,
            chunk_overlap_tokens,
        )
        .await?;
    let view = view_with_size(&state, collection).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "update_knowledge_collection",
            "knowledge_collection",
            &id.to_string(),
            serde_json::to_value(&view).unwrap_or_default(),
        )
        .await?;
    Ok(Json(view))
}

#[utoipa::path(
    delete, path = "/api/v1/knowledge/collections/{id}", tag = "knowledge",
    params(("id" = Uuid, Path, description = "Collection id")),
    responses((status = 204))
)]
pub async fn delete_collection(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    state.store.delete_collection(id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "delete_knowledge_collection",
            "knowledge_collection",
            &id.to_string(),
            serde_json::json!({}),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- documents --------------------------------------------------------

/// Dashboard-facing view of a document. Omits the stored `content` — the
/// admin API surfaces document metadata and status, not the full text.
#[derive(Debug, Serialize, ToSchema)]
pub struct DocumentView {
    pub id: Uuid,
    pub collection_id: Uuid,
    pub title: String,
    pub filename: String,
    pub content_type: String,
    pub byte_size: i64,
    pub status: String,
    pub error: Option<String>,
    pub chunk_count: i32,
}

impl DocumentView {
    fn from_document(d: &KnowledgeDocument) -> Self {
        DocumentView {
            id: d.id,
            collection_id: d.collection_id,
            title: d.title.clone(),
            filename: d.filename.clone(),
            content_type: d.content_type.clone(),
            byte_size: d.byte_size,
            status: d.status.clone(),
            error: d.error.clone(),
            chunk_count: d.chunk_count,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UploadDocument {
    pub title: String,
    pub filename: String,
    /// base64-encoded file bytes
    pub content_base64: String,
}

#[utoipa::path(
    get, path = "/api/v1/knowledge/collections/{id}/documents", tag = "knowledge",
    params(("id" = Uuid, Path, description = "Collection id")),
    responses((status = 200, body = [DocumentView]))
)]
pub async fn list_documents(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<DocumentView>>> {
    let docs = state.store.list_documents(id).await?;
    Ok(Json(docs.iter().map(DocumentView::from_document).collect()))
}

#[utoipa::path(
    post, path = "/api/v1/knowledge/collections/{id}/documents", tag = "knowledge",
    params(("id" = Uuid, Path, description = "Collection id")),
    request_body = UploadDocument,
    responses((status = 200, body = DocumentView))
)]
pub async fn upload_document(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UploadDocument>,
) -> Result<Json<DocumentView>> {
    let title = body.title.trim();
    if title.is_empty() {
        return Err(AdminError::BadRequest("title is required".into()));
    }
    // Confirm the collection exists before doing any decode work, so a typo'd
    // id fails fast with a clear error rather than surfacing as an FK
    // violation from the insert below.
    state.store.get_collection(id).await?;

    let settings = state.store.get_boon_settings().await?.unwrap_or_default();
    let max_bytes = settings.knowledge.max_upload_bytes;

    // Reject an oversized upload before decoding: base64 decodes to about
    // 3/4 of its encoded length, so this estimate lets a hostile upload fail
    // without ever allocating the full decoded buffer. The post-decode check
    // in `validate_upload` below remains authoritative (it accounts for the
    // ~33% inflation exactly); this is a cheap early-out, not a replacement.
    let estimated_decoded_bytes = (body.content_base64.len() as i64 / 4) * 3;
    if estimated_decoded_bytes > max_bytes {
        return Err(AdminError::BadRequest(format!(
            "file too large: an estimated {estimated_decoded_bytes} bytes exceeds the {max_bytes}-byte limit"
        )));
    }

    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&body.content_base64)
        .map_err(|_| AdminError::BadRequest("content_base64 is not valid base64".into()))?;
    let text = validate_upload(&bytes, max_bytes).map_err(AdminError::BadRequest)?;

    let doc = state
        .store
        .upsert_document(
            id,
            title,
            &body.filename,
            content_type_for(&body.filename),
            &text,
        )
        .await?;
    // The document lands as `pending`; the background indexer picks it up
    // within a few seconds.
    let view = DocumentView::from_document(&doc);
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "upload_knowledge_document",
            "knowledge_document",
            &doc.id.to_string(),
            serde_json::to_value(&view).unwrap_or_default(),
        )
        .await?;
    Ok(Json(view))
}

#[utoipa::path(
    delete, path = "/api/v1/knowledge/documents/{id}", tag = "knowledge",
    params(("id" = Uuid, Path, description = "Document id")),
    responses((status = 204))
)]
pub async fn delete_document(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let collection_id = state.store.delete_document(id).await?;
    // The proxy compares `version` to decide whether to rebuild its in-memory
    // slab; without this bump a deleted document's chunks keep serving until
    // some other write happens to touch the collection.
    state.store.bump_collection_version(collection_id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "delete_knowledge_document",
            "knowledge_document",
            &id.to_string(),
            serde_json::json!({ "collection_id": collection_id }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

// ---- re-index -----------------------------------------------------------

#[utoipa::path(
    post, path = "/api/v1/knowledge/collections/{id}/reindex", tag = "knowledge",
    params(("id" = Uuid, Path, description = "Collection id")),
    responses((status = 200, body = ReindexResult))
)]
pub async fn reindex_collection(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<ReindexResult>> {
    let documents_requeued = state.store.reindex_collection_documents(id).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "reindex_knowledge_collection",
            "knowledge_collection",
            &id.to_string(),
            serde_json::json!({ "documents_requeued": documents_requeued }),
        )
        .await?;
    Ok(Json(ReindexResult { documents_requeued }))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ReindexResult {
    pub documents_requeued: u64,
}

#[utoipa::path(
    post, path = "/api/v1/knowledge/documents/{id}/reindex", tag = "knowledge",
    params(("id" = Uuid, Path, description = "Document id")),
    responses((status = 200, body = DocumentView))
)]
pub async fn reindex_document(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<DocumentView>> {
    let doc = state.store.reindex_document(id).await?;
    let view = DocumentView::from_document(&doc);
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "reindex_knowledge_document",
            "knowledge_document",
            &id.to_string(),
            serde_json::to_value(&view).unwrap_or_default(),
        )
        .await?;
    Ok(Json(view))
}

// ---- search ---------------------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct SearchQuery {
    pub query: String,
    #[serde(default)]
    pub top_k: Option<u32>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SearchHit {
    pub chunk_id: Uuid,
    pub document_id: Uuid,
    pub score: f32,
    pub token_count: i32,
    pub text: String,
    /// Whether this hit would actually be injected under the collection's
    /// current `min_score` threshold and `max_context_tokens` budget --
    /// the whole reason this preview exists is to let an administrator tune
    /// those two knobs by eye.
    pub would_inject: bool,
}

/// Administrator-facing retrieval preview: embeds `query` with the model the
/// collection's active generation was actually built with, cosine-scores
/// every retrievable chunk, and reports which hits would actually be injected
/// under the current knowledge-boon settings -- mirroring the boon's own
/// packing rather than just thresholding on score. Reads Postgres directly,
/// which is fine here: this is the Management API, not the request path.
#[utoipa::path(
    post, path = "/api/v1/knowledge/collections/{id}/search", tag = "knowledge",
    params(("id" = Uuid, Path, description = "Collection id")),
    request_body = SearchQuery,
    responses((status = 200, body = [SearchHit]), (status = 400), (status = 404))
)]
pub async fn search_collection(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Json(body): Json<SearchQuery>,
) -> Result<Json<Vec<SearchHit>>> {
    let query = body.query.trim();
    if query.is_empty() {
        return Err(AdminError::BadRequest("query is required".into()));
    }

    let collection = state.store.get_collection(id).await?;
    // `indexed_embedding_model` -- the embedder the *active* generation was
    // actually built with -- not `embedding_model`, the operator's currently
    // desired embedder. Those two differ exactly when an administrator has
    // changed the embedder but a re-index has not finished yet
    // (`collection.needs_reindex()`); embedding the query with the wrong
    // model produces a plausible-looking but meaningless similarity score
    // rather than an error, which is the worst failure mode available here.
    if collection.indexed_embedding_model.is_empty() {
        return Err(AdminError::BadRequest(
            "collection has not been indexed yet; upload a document and wait for indexing to \
             finish"
                .into(),
        ));
    }

    let settings = state
        .store
        .get_boon_settings()
        .await?
        .unwrap_or_default()
        .knowledge;

    let model = state
        .store
        .get_model_by_name(&collection.indexed_embedding_model)
        .await?;
    let target = embed::EmbedTarget {
        api_base: model.api_base.clone(),
        api_key: model.api_key.clone(),
        upstream_model: model.upstream_model.clone(),
    };
    // `embed_timeout_ms` -- the hard bound documented for request-path query
    // embedding -- not `index_timeout_ms`, which is the (looser) background
    // indexing timeout. This preview issues the same kind of call the
    // proxy's retrieval path will make later, not an indexing call.
    let query_owned = query.to_string();
    let query_vec = embed::embed_batch(
        &state.health.http,
        &target,
        std::slice::from_ref(&query_owned),
        std::time::Duration::from_millis(settings.embed_timeout_ms),
    )
    .await
    .map_err(|e| AdminError::Internal(e.to_string()))?
    .into_iter()
    .next()
    .unwrap_or_default();

    let chunks = state.store.load_active_chunks(id).await?;
    let vectors: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
    let top_k = body.top_k.unwrap_or(settings.top_k) as usize;
    let scored = score_against(
        &query_vec,
        vectors.iter().map(|v| v.as_slice()),
        top_k,
        settings.min_score,
    );

    // Mirror the boon's packing so `would_inject` tells the truth about what
    // would actually be injected, not merely what scored well: walk hits in
    // descending score order (guaranteed by `score_against`), subtracting
    // `token_count` from a budget seeded at `max_context_tokens`, and mark a
    // hit `would_inject: true` only while it still fits. A hit that scores
    // well but blows the budget must show `false`.
    let mut budget = settings.max_context_tokens as i64;
    let hits = scored
        .into_iter()
        .map(|s| {
            let c = &chunks[s.index];
            let fits = budget >= c.token_count as i64;
            if fits {
                budget -= c.token_count as i64;
            }
            SearchHit {
                chunk_id: c.id,
                document_id: c.document_id,
                score: s.score,
                token_count: c.token_count,
                text: c.text.clone(),
                would_inject: fits,
            }
        })
        .collect();
    Ok(Json(hits))
}

// ---- model attachment -----------------------------------------------------

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelCollections {
    pub collection_ids: Vec<Uuid>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ModelCollectionsView {
    pub collection_ids: Vec<Uuid>,
}

/// The first requested id that is not in `existing`, or `None` when every
/// requested id is real. Pure (no I/O) so the guard logic in
/// `set_model_collections` is unit-testable without a database — the actual
/// existence check (`Store::existing_collection_ids`) still needs one.
fn first_unknown_collection_id(
    requested: &[Uuid],
    existing: &std::collections::HashSet<Uuid>,
) -> Option<Uuid> {
    requested.iter().find(|c| !existing.contains(c)).copied()
}

#[utoipa::path(
    get, path = "/api/v1/models/{id}/knowledge", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 200, body = ModelCollectionsView))
)]
pub async fn get_model_collections(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ModelCollectionsView>> {
    // Load the model first: a missing model must be a 404, not an empty list
    // that looks like "attached to nothing" rather than "does not exist".
    let _model = state.store.get_model(id).await?;
    let collection_ids = state.store.model_collection_ids(id).await?;
    Ok(Json(ModelCollectionsView { collection_ids }))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/knowledge", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelCollections,
    responses((status = 200, body = ModelCollectionsView))
)]
pub async fn set_model_collections(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelCollections>,
) -> Result<Json<ModelCollectionsView>> {
    // Load the model first: a missing model must be a 404, not a raw FK
    // violation surfaced as a 500 from the write below.
    let model = state.store.get_model(id).await?;

    // Every supplied collection id must be real, checked in one query rather
    // than one per id (and rather than relying on the FK to reject a bad id,
    // which would otherwise surface as an opaque 500 too). This runs, and can
    // fail, before `Store::set_model_collections` is ever called, so a bad id
    // leaves the model's existing attachments untouched.
    let existing: std::collections::HashSet<Uuid> = state
        .store
        .existing_collection_ids(&body.collection_ids)
        .await?
        .into_iter()
        .collect();
    if let Some(bad) = first_unknown_collection_id(&body.collection_ids, &existing) {
        return Err(AdminError::BadRequest(format!(
            "unknown collection id: {bad}"
        )));
    }

    state
        .store
        .set_model_collections(id, &body.collection_ids)
        .await?;
    // Refresh Redis immediately so the data plane reflects the new attachment
    // set on the model's next request, instead of waiting on the 15s refresh.
    sync_model(&state, &model).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "set_model_knowledge_collections",
            "model",
            &id.to_string(),
            serde_json::json!({ "collection_ids": body.collection_ids }),
        )
        .await?;
    Ok(Json(ModelCollectionsView {
        collection_ids: body.collection_ids,
    }))
}

// ---- knowledge boon settings ---------------------------------------------

/// Flattened view of `KnowledgeBoonSettings`, matching the pattern of
/// `BoonSettingsView`. Kept on its own `/settings/knowledge` route (rather
/// than folded into `BoonSettingsView`) because these knobs include ingestion
/// caps that are not boon behaviour, and `BoonSettingsView` is already large.
#[derive(Debug, Serialize, ToSchema)]
pub struct KnowledgeSettingsView {
    pub enabled: bool,
    pub top_k: u32,
    pub min_score: f32,
    pub max_context_tokens: u32,
    pub embed_timeout_ms: u64,
    pub query_cache_ttl_s: u64,
    pub query_turns: u32,
    pub max_upload_bytes: i64,
    pub max_chunks_per_collection: i64,
    pub index_batch_size: u32,
    pub index_timeout_ms: u64,
    pub index_stale_after_secs: i64,
    pub debug_snapshot: bool,
}

impl KnowledgeSettingsView {
    fn from_settings(s: &KnowledgeBoonSettings) -> Self {
        KnowledgeSettingsView {
            enabled: s.enabled,
            top_k: s.top_k,
            min_score: s.min_score,
            max_context_tokens: s.max_context_tokens,
            embed_timeout_ms: s.embed_timeout_ms,
            query_cache_ttl_s: s.query_cache_ttl_s,
            query_turns: s.query_turns,
            max_upload_bytes: s.max_upload_bytes,
            max_chunks_per_collection: s.max_chunks_per_collection,
            index_batch_size: s.index_batch_size,
            index_timeout_ms: s.index_timeout_ms,
            index_stale_after_secs: s.index_stale_after_secs,
            debug_snapshot: s.debug_snapshot,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateKnowledgeSettings {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub top_k: Option<u32>,
    #[serde(default)]
    pub min_score: Option<f32>,
    #[serde(default)]
    pub max_context_tokens: Option<u32>,
    #[serde(default)]
    pub embed_timeout_ms: Option<u64>,
    #[serde(default)]
    pub query_cache_ttl_s: Option<u64>,
    #[serde(default)]
    pub query_turns: Option<u32>,
    /// Per-file upload cap in bytes. Omit/zero leaves it unchanged.
    #[serde(default)]
    pub max_upload_bytes: Option<i64>,
    #[serde(default)]
    pub max_chunks_per_collection: Option<i64>,
    #[serde(default)]
    pub index_batch_size: Option<u32>,
    #[serde(default)]
    pub index_timeout_ms: Option<u64>,
    #[serde(default)]
    pub index_stale_after_secs: Option<i64>,
    #[serde(default)]
    pub debug_snapshot: Option<bool>,
}

#[utoipa::path(
    get, path = "/api/v1/settings/knowledge", tag = "settings",
    responses((status = 200, body = KnowledgeSettingsView))
)]
pub async fn get_knowledge_settings(
    State(state): State<AdminState>,
) -> Result<Json<KnowledgeSettingsView>> {
    let settings = state.store.get_boon_settings().await?.unwrap_or_default();
    Ok(Json(KnowledgeSettingsView::from_settings(
        &settings.knowledge,
    )))
}

#[utoipa::path(
    put, path = "/api/v1/settings/knowledge", tag = "settings",
    request_body = UpdateKnowledgeSettings,
    responses((status = 200, body = KnowledgeSettingsView))
)]
pub async fn put_knowledge_settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<UpdateKnowledgeSettings>,
) -> Result<Json<KnowledgeSettingsView>> {
    // Read the whole settings blob and replace only `knowledge` — every other
    // boon (vision, structured_output, tool_loop, guardrails, compression)
    // must round-trip untouched, the same guarantee `put_boon_settings` gives
    // its own fields.
    let existing = state.store.get_boon_settings().await?.unwrap_or_default();
    let k = &existing.knowledge;
    let knowledge = KnowledgeBoonSettings {
        enabled: body.enabled.unwrap_or(k.enabled),
        top_k: body.top_k.filter(|n| *n > 0).unwrap_or(k.top_k),
        min_score: body
            .min_score
            .filter(|s| (0.0..=1.0).contains(s))
            .unwrap_or(k.min_score),
        max_context_tokens: body
            .max_context_tokens
            .filter(|n| *n > 0)
            .unwrap_or(k.max_context_tokens),
        embed_timeout_ms: body
            .embed_timeout_ms
            .filter(|n| *n > 0)
            .unwrap_or(k.embed_timeout_ms),
        query_cache_ttl_s: body
            .query_cache_ttl_s
            .filter(|n| *n > 0)
            .unwrap_or(k.query_cache_ttl_s),
        query_turns: body.query_turns.filter(|n| *n > 0).unwrap_or(k.query_turns),
        max_upload_bytes: body
            .max_upload_bytes
            .filter(|n| *n > 0)
            .unwrap_or(k.max_upload_bytes),
        max_chunks_per_collection: body
            .max_chunks_per_collection
            .filter(|n| *n > 0)
            .unwrap_or(k.max_chunks_per_collection),
        index_batch_size: body
            .index_batch_size
            .filter(|n| *n > 0)
            .unwrap_or(k.index_batch_size),
        index_timeout_ms: body
            .index_timeout_ms
            .filter(|n| *n > 0)
            .unwrap_or(k.index_timeout_ms),
        index_stale_after_secs: body
            .index_stale_after_secs
            .filter(|n| *n > 0)
            .unwrap_or(k.index_stale_after_secs),
        debug_snapshot: body.debug_snapshot.unwrap_or(k.debug_snapshot),
    };
    let settings = BoonSettings {
        knowledge,
        ..existing
    };
    state.store.put_boon_settings(&settings).await?;
    let view = KnowledgeSettingsView::from_settings(&settings.knowledge);
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "set_knowledge_settings",
            "settings",
            "knowledge",
            serde_json::to_value(&view).unwrap_or_default(),
        )
        .await?;
    Ok(Json(view))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_rejects_oversized_files_before_decoding() {
        // Checking the cap before UTF-8 decoding keeps a hostile upload from
        // costing a full-size allocation.
        let err = validate_upload(&vec![0u8; 2048], 1024).expect_err("too big");
        assert!(err.contains("too large"));
    }

    #[test]
    fn upload_rejects_non_utf8() {
        let bytes = vec![0xff, 0xfe, 0xfd];
        assert!(validate_upload(&bytes, 1024).is_err());
    }

    #[test]
    fn upload_accepts_utf8_within_the_cap() {
        let text = validate_upload("policy text".as_bytes(), 1024).expect("ok");
        assert_eq!(text, "policy text");
    }

    #[test]
    fn content_type_is_inferred_from_the_filename() {
        assert_eq!(content_type_for("catalog.csv"), "text/csv");
        assert_eq!(content_type_for("policy.md"), "text/markdown");
        assert_eq!(content_type_for("notes.txt"), "text/plain");
        assert_eq!(content_type_for("mystery"), "text/plain");
    }

    #[test]
    fn first_unknown_collection_id_flags_the_missing_one() {
        let known = Uuid::new_v4();
        let missing = Uuid::new_v4();
        let existing: std::collections::HashSet<Uuid> = [known].into_iter().collect();
        assert_eq!(
            first_unknown_collection_id(&[known, missing], &existing),
            Some(missing)
        );
    }

    #[test]
    fn first_unknown_collection_id_is_none_when_every_id_is_known() {
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        let existing: std::collections::HashSet<Uuid> = [a, b].into_iter().collect();
        assert_eq!(first_unknown_collection_id(&[a, b], &existing), None);
    }

    #[test]
    fn first_unknown_collection_id_is_none_for_an_empty_request() {
        let existing: std::collections::HashSet<Uuid> = std::collections::HashSet::new();
        assert_eq!(first_unknown_collection_id(&[], &existing), None);
    }
}
