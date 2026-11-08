//! Batches across every tenant, for the dashboard: the list with progress,
//! one batch, and cancelling one. Callers manage their own batches through
//! the OpenAI-shaped `/v1/batches` on the proxy port; this is the operator's
//! view of all of them.

use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::Json;
use chrono::{DateTime, Utc};
use obleth_store::batches::{AdminBatchFilter, AdminBatchRow, BatchRecord};
use obleth_store::JobOwner;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::error::AdminError;

type Result<T> = std::result::Result<T, AdminError>;
use crate::{audit_actor, AdminState};

const STATUSES: &[&str] = &[
    "validating",
    "failed",
    "in_progress",
    "finalizing",
    "completed",
    "expired",
    "cancelling",
    "cancelled",
];

#[derive(Debug, Deserialize, IntoParams)]
pub struct BatchListQuery {
    /// One status, or `active` for every unfinished one.
    pub status: Option<String>,
    pub tenant_id: Option<Uuid>,
    pub model: Option<String>,
    /// The last batch id of the previous page.
    pub after: Option<String>,
    /// Up to 200; default 50.
    pub limit: Option<i64>,
}

/// A batch as the dashboard shows it.
#[derive(Debug, Serialize, ToSchema)]
pub struct AdminBatchView {
    pub id: String,
    pub tenant_id: Uuid,
    pub tenant_name: Option<String>,
    pub key_id: Uuid,
    pub key_name: Option<String>,
    pub model: String,
    pub endpoint: String,
    pub status: String,
    /// Requests in the batch.
    pub total: i32,
    /// Requests that succeeded.
    pub completed: i32,
    /// Requests that failed or expired.
    pub failed: i32,
    pub completion_window: String,
    pub created_at: DateTime<Utc>,
    pub in_progress_at: Option<DateTime<Utc>>,
    pub deadline_at: DateTime<Utc>,
    /// When a request of the batch last finished.
    pub last_progress_at: Option<DateTime<Utc>>,
    /// When the batch reached its final status.
    pub finished_at: Option<DateTime<Utc>>,
    /// In progress with requests left and none finished for two minutes:
    /// usually the model is busy with interactive work.
    pub waiting: bool,
    #[schema(value_type = Option<Object>)]
    pub metadata: Option<serde_json::Value>,
    /// Validation errors of a failed batch.
    #[schema(value_type = Option<Object>)]
    pub errors: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AdminBatchList {
    pub data: Vec<AdminBatchView>,
    pub has_more: bool,
    /// Batches by status across every tenant, regardless of the filters.
    pub counts: BTreeMap<String, i64>,
}

fn view(b: BatchRecord, tenant_name: Option<String>, key_name: Option<String>) -> AdminBatchView {
    let now = Utc::now();
    let json = |t: &Option<String>| {
        t.as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
    };
    AdminBatchView {
        waiting: b.waiting_for_capacity(now),
        finished_at: b.finished_at(),
        metadata: json(&b.metadata),
        errors: json(&b.errors),
        id: b.id,
        tenant_id: b.tenant_id,
        tenant_name,
        key_id: b.key_id,
        key_name,
        model: b.model,
        endpoint: b.endpoint,
        status: b.status,
        total: b.total,
        completed: b.completed,
        failed: b.failed,
        completion_window: b.completion_window,
        created_at: b.created_at,
        in_progress_at: b.in_progress_at,
        deadline_at: b.deadline_at,
        last_progress_at: b.last_progress_at,
    }
}

fn row_view(r: AdminBatchRow) -> AdminBatchView {
    view(r.batch, r.tenant_name, r.key_name)
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

#[utoipa::path(
    get, path = "/api/v1/batches", tag = "batches",
    params(BatchListQuery),
    responses((status = 200, body = AdminBatchList))
)]
pub async fn list_batches(
    State(state): State<AdminState>,
    Query(q): Query<BatchListQuery>,
) -> Result<Json<AdminBatchList>> {
    let status = q.status.as_deref().map(str::trim).filter(|s| !s.is_empty());
    if let Some(s) = status {
        if s != "active" && !STATUSES.contains(&s) {
            return Err(AdminError::BadRequest(format!(
                "status must be active or one of {}",
                STATUSES.join(", ")
            )));
        }
    }
    if let Some(a) = q.after.as_deref() {
        if !valid_id(a) {
            return Err(AdminError::BadRequest("after must be a batch id".into()));
        }
    }
    let model = q.model.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let filter = AdminBatchFilter {
        status,
        tenant_id: q.tenant_id,
        model,
    };
    let mut rows = state
        .store
        .list_all_batches(&filter, q.after.as_deref(), limit + 1)
        .await?;
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let counts = state
        .store
        .batch_status_counts()
        .await?
        .into_iter()
        .collect();
    Ok(Json(AdminBatchList {
        data: rows.into_iter().map(row_view).collect(),
        has_more,
        counts,
    }))
}

async fn named(state: &AdminState, b: BatchRecord) -> AdminBatchView {
    match state.store.resolved_key_by_id(b.key_id).await {
        Ok(Some(k)) => view(b, Some(k.tenant_name), Some(k.key_name)),
        _ => {
            let tenant = state
                .store
                .get_tenant(b.tenant_id)
                .await
                .ok()
                .map(|t| t.name);
            view(b, tenant, None)
        }
    }
}

#[utoipa::path(
    get, path = "/api/v1/batches/{id}", tag = "batches",
    params(("id" = String, Path, description = "Batch id")),
    responses((status = 200, body = AdminBatchView), (status = 404))
)]
pub async fn get_batch(
    State(state): State<AdminState>,
    Path(id): Path<String>,
) -> Result<Json<AdminBatchView>> {
    if !valid_id(&id) {
        return Err(AdminError::NotFound);
    }
    let b = state
        .store
        .get_batch_any(&id)
        .await?
        .ok_or(AdminError::NotFound)?;
    Ok(Json(named(&state, b).await))
}

#[utoipa::path(
    post, path = "/api/v1/batches/{id}/cancel", tag = "batches",
    params(("id" = String, Path, description = "Batch id")),
    responses((status = 200, body = AdminBatchView), (status = 404))
)]
pub async fn cancel_batch(
    State(state): State<AdminState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<AdminBatchView>> {
    if !valid_id(&id) {
        return Err(AdminError::NotFound);
    }
    let b = state
        .store
        .get_batch_any(&id)
        .await?
        .ok_or(AdminError::NotFound)?;
    let before = b.status.clone();
    let b = state
        .store
        .cancel_batch(&id, JobOwner::key(b.tenant_id, b.key_id))
        .await?
        .ok_or(AdminError::NotFound)?;
    if b.status != before {
        state
            .store
            .record_audit(
                &audit_actor(&headers),
                "cancel_batch",
                "batch",
                &b.id,
                serde_json::json!({
                    "tenant_id": b.tenant_id, "key_id": b.key_id, "model": b.model,
                    "total": b.total, "completed": b.completed, "failed": b.failed,
                }),
            )
            .await?;
    }
    Ok(Json(named(&state, b).await))
}
