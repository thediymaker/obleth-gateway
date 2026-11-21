//! The audit log read.

use super::*;

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct AuditQuery {
    pub limit: Option<i64>,
    /// Exact actor (an email, `admin`, …).
    pub actor: Option<String>,
    /// One entity type or several, comma-separated.
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    /// One action or several, comma-separated.
    pub action: Option<String>,
    /// RFC 3339; `since` inclusive, `until` exclusive.
    pub since: Option<String>,
    pub until: Option<String>,
    /// Page back from this row id (exclusive).
    pub before_id: Option<i64>,
    /// Free text over actor, action, entity id and detail.
    pub q: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/audit", tag = "audit",
    params(AuditQuery),
    responses((status = 200, body = [AuditEntry]))
)]
pub(crate) async fn get_audit(
    State(state): State<AdminState>,
    Query(q): Query<AuditQuery>,
) -> Result<(HeaderMap, Json<Vec<AuditEntry>>)> {
    let limit = q
        .limit
        .unwrap_or(100)
        .clamp(1, obleth_store::AUDIT_PAGE_MAX);
    let time = |v: &Option<String>, name: &str| -> Result<Option<chrono::DateTime<chrono::Utc>>> {
        v.as_deref()
            .filter(|s| !s.trim().is_empty())
            .map(|s| {
                chrono::DateTime::parse_from_rfc3339(s.trim())
                    .map(|t| t.with_timezone(&chrono::Utc))
                    .map_err(|_| AdminError::BadRequest(format!("{name} must be an RFC 3339 time")))
            })
            .transpose()
    };
    let text = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let list = |v: Option<String>| {
        text(v).map(|a| {
            a.split(',')
                .map(|x| x.trim().to_string())
                .filter(|x| !x.is_empty())
                .collect::<Vec<_>>()
        })
    };
    let filter = obleth_store::AuditFilter {
        since: time(&q.since, "since")?,
        until: time(&q.until, "until")?,
        actor: text(q.actor),
        entity_types: list(q.entity_type),
        entity_id: text(q.entity_id),
        actions: list(q.action),
        before_id: q.before_id,
        q: text(q.q),
        // One past the page, to tell whether more follow.
        limit: limit + 1,
    };
    let mut rows = state.store.list_audit_filtered(&filter).await?;
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    Ok((truncated_header(more), Json(rows)))
}
