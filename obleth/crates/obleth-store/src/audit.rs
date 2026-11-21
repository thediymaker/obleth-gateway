//! The admin audit log.

use super::*;

/// Most audit rows one page returns. `list_audit_filtered` reads one more on
/// request, so a caller can tell whether more follow.
pub const AUDIT_PAGE_MAX: i64 = 1000;

/// What `list_audit_filtered` narrows to; every field is optional but `limit`.
#[derive(Debug, Clone, Default)]
pub struct AuditFilter {
    pub actor: Option<String>,
    /// Any of these entity types.
    pub entity_types: Option<Vec<String>>,
    pub entity_id: Option<String>,
    /// Any of these actions.
    pub actions: Option<Vec<String>>,
    pub since: Option<chrono::DateTime<chrono::Utc>>,
    pub until: Option<chrono::DateTime<chrono::Utc>>,
    pub before_id: Option<i64>,
    pub q: Option<String>,
    pub limit: i64,
}

/// One row of the admin audit log (`GET /api/v1/audit`).
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: DateTime<Utc>,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub detail: serde_json::Value,
}

fn audit_from_row(row: &PgRow) -> Result<AuditEntry> {
    Ok(AuditEntry {
        id: row.try_get("id")?,
        ts: row.try_get("ts")?,
        actor: row.try_get("actor")?,
        action: row.try_get("action")?,
        entity_type: row.try_get("entity_type")?,
        entity_id: row.try_get("entity_id")?,
        detail: row.try_get("detail")?,
    })
}

impl Store {
    pub async fn record_audit(
        &self,
        actor: &str,
        action: &str,
        entity_type: &str,
        entity_id: &str,
        detail: serde_json::Value,
    ) -> Result<()> {
        sqlx::query(
            "insert into audit_log (actor, action, entity_type, entity_id, detail)
             values ($1, $2, $3, $4, $5)",
        )
        .bind(actor)
        .bind(action)
        .bind(entity_type)
        .bind(entity_id)
        .bind(detail)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Audit rows matching `f`, newest first. `before_id` pages back: pass the
    /// last id of one page to get the next. `q` matches the actor, action,
    /// entity id and the detail's text, case-insensitively.
    pub async fn list_audit_filtered(&self, f: &AuditFilter) -> Result<Vec<AuditEntry>> {
        let q =
            f.q.as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(contains_pattern);
        let rows = sqlx::query(
            "select id, ts, actor, action, entity_type, entity_id, detail
             from audit_log
             where ($1::text is null or actor = $1)
               and ($2::text[] is null or entity_type = any($2))
               and ($3::text is null or entity_id = $3)
               and ($4::text[] is null or action = any($4))
               and ($5::timestamptz is null or ts >= $5)
               and ($6::timestamptz is null or ts < $6)
               and ($7::bigint is null or id < $7)
               and ($8::text is null or actor ilike $8 or action ilike $8 or entity_id ilike $8 or detail::text ilike $8)
             order by id desc
             limit $9",
        )
        .bind(f.actor.as_deref())
        .bind(f.entity_types.as_deref())
        .bind(f.entity_id.as_deref())
        .bind(f.actions.as_deref())
        .bind(f.since)
        .bind(f.until)
        .bind(f.before_id)
        .bind(q)
        .bind(f.limit.clamp(1, AUDIT_PAGE_MAX + 1))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(audit_from_row).collect()
    }

    pub async fn list_audit(&self, limit: i64) -> Result<Vec<AuditEntry>> {
        let rows = sqlx::query(
            "select id, ts, actor, action, entity_type, entity_id, detail
             from audit_log order by ts desc limit $1",
        )
        .bind(limit.clamp(1, 1000))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(audit_from_row).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::serial;

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` points at a
    /// throwaway Postgres. Skips silently otherwise so unit runs stay hermetic.
    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// The audit log narrows by actor, entity, action, time and text, and pages back by id.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn audit_filters_and_pages() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let actor = format!("audit-{}@example.edu", Uuid::new_v4().simple());
        let thing = Uuid::new_v4().to_string();
        for (action, detail) in [
            (
                "create_model",
                serde_json::json!({"model_name": "needle-model"}),
            ),
            (
                "update_model",
                serde_json::json!({"model_name": "needle-model", "note": "50%_off"}),
            ),
            ("delete_model", serde_json::json!({})),
        ] {
            store
                .record_audit(&actor, action, "model", &thing, detail)
                .await
                .expect("audit");
        }
        let base = AuditFilter {
            actor: Some(actor.clone()),
            limit: 100,
            ..Default::default()
        };

        let all = store.list_audit_filtered(&base).await.expect("all");
        assert_eq!(
            all.iter().map(|e| e.action.as_str()).collect::<Vec<_>>(),
            ["delete_model", "update_model", "create_model"]
        );

        let two = AuditFilter {
            actions: Some(vec!["create_model".into(), "delete_model".into()]),
            ..base.clone()
        };
        assert_eq!(store.list_audit_filtered(&two).await.unwrap().len(), 2);

        let page = AuditFilter {
            before_id: Some(all[0].id),
            limit: 1,
            ..base.clone()
        };
        assert_eq!(
            store.list_audit_filtered(&page).await.unwrap()[0].action,
            "update_model"
        );

        // % and _ in the text are literal, not wildcards.
        let text = AuditFilter {
            q: Some("50%_off".into()),
            ..base.clone()
        };
        assert_eq!(store.list_audit_filtered(&text).await.unwrap().len(), 1);
        let wild = AuditFilter {
            q: Some("50%%off".into()),
            ..base.clone()
        };
        assert!(store.list_audit_filtered(&wild).await.unwrap().is_empty());

        let other = AuditFilter {
            entity_types: Some(vec!["tenant".into()]),
            ..base.clone()
        };
        assert!(store.list_audit_filtered(&other).await.unwrap().is_empty());
        // Any of several types, as the Audit page's Kind filter sends them.
        let kinds = AuditFilter {
            entity_types: Some(vec!["tenant".into(), "model".into()]),
            ..base.clone()
        };
        assert_eq!(store.list_audit_filtered(&kinds).await.unwrap().len(), 3);
        let later = AuditFilter {
            since: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            ..base.clone()
        };
        assert!(store.list_audit_filtered(&later).await.unwrap().is_empty());
        let byid = AuditFilter {
            entity_id: Some(thing.clone()),
            actor: None,
            ..base
        };
        assert_eq!(store.list_audit_filtered(&byid).await.unwrap().len(), 3);
    }
}
