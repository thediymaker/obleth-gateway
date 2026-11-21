//! Tenants and fair-share groups.

use super::*;

/// A tenant's display name and fair-share group, for labelling rows that
/// carry only its id (see `tenant_names`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TenantName {
    pub id: Uuid,
    pub name: String,
    pub fairshare_group: String,
}

pub(crate) const TENANT_COLUMNS: &str = "id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, compression_policy, tracing_enabled, synthetic, created_at, updated_at";

pub(crate) fn tenant_from_row(row: &PgRow) -> Result<Tenant> {
    Ok(Tenant {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        fairshare_group: row.try_get("fairshare_group")?,
        weight: row.try_get("weight")?,
        tokens_per_minute: row.try_get("tokens_per_minute")?,
        max_in_flight: row.try_get("max_in_flight")?,
        description: row.try_get("description")?,
        organization: row.try_get("organization")?,
        contact_email: row.try_get("contact_email")?,
        status: row.try_get("status")?,
        timezone: row.try_get("timezone")?,
        active_from: row.try_get("active_from")?,
        active_until: row.try_get("active_until")?,
        weekly_windows: weekly_windows_from_row(row)?,
        budget_tokens: row.try_get("budget_tokens")?,
        budget_cost_usd: row.try_get("budget_cost_usd")?,
        budget_period: row.try_get("budget_period")?,
        budget_started_at: row.try_get("budget_started_at")?,
        allowed_models: allowed_models_from_row(row)?,
        tracing_enabled: row.try_get("tracing_enabled")?,
        guardrails_policy: guardrails_policy_from_row(row)?,
        compression_policy: compression_policy_from_row(row)?,
        synthetic: row.try_get("synthetic")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Decode the optional `weekly_windows` jsonb column. A SQL `null` or an empty
/// array both collapse to `None` (meaning "any time of week").
pub(crate) fn weekly_windows_from_row(row: &PgRow) -> Result<Option<Vec<WeeklyWindow>>> {
    let json: Option<sqlx::types::Json<Vec<WeeklyWindow>>> = row.try_get("weekly_windows")?;
    Ok(json.map(|j| j.0).filter(|v| !v.is_empty()))
}

/// Decode the optional `allowed_models` jsonb column. A SQL `null` or empty
/// array both collapse to `None` (meaning "all models permitted").
pub(crate) fn allowed_models_from_row(row: &PgRow) -> Result<Option<Vec<String>>> {
    let json: Option<sqlx::types::Json<Vec<String>>> = row.try_get("allowed_models")?;
    Ok(json.map(|j| j.0).filter(|v| !v.is_empty()))
}

/// Decode the optional `guardrails_policy` jsonb column. A decode failure (e.g.
/// a malformed policy written out of band) is logged and treated as no policy
/// rather than failing the whole row — consistent with the boon's fail-open
/// posture, and surfaced via the warning so it can be investigated.
pub(crate) fn guardrails_policy_from_row(
    row: &PgRow,
) -> Result<Option<obleth_config::GuardrailsPolicy>> {
    match row.try_get::<Option<sqlx::types::Json<obleth_config::GuardrailsPolicy>>, _>(
        "guardrails_policy",
    ) {
        Ok(json) => Ok(json.map(|j| j.0)),
        Err(e) => {
            tracing::warn!(error = %e, "failed to decode guardrails_policy; treating as none");
            Ok(None)
        }
    }
}

/// Decode the optional `compression_policy` jsonb column. A decode failure is
/// logged and treated as no policy rather than failing the whole row —
/// consistent with the boon's fail-open posture.
pub(crate) fn compression_policy_from_row(
    row: &PgRow,
) -> Result<Option<obleth_config::CompressionPolicy>> {
    match row.try_get::<Option<sqlx::types::Json<obleth_config::CompressionPolicy>>, _>(
        "compression_policy",
    ) {
        Ok(json) => Ok(json.map(|j| j.0)),
        Err(e) => {
            tracing::warn!(error = %e, "failed to decode compression_policy; treating as none");
            Ok(None)
        }
    }
}

fn fairshare_group_from_row(row: &PgRow) -> Result<FairshareGroup> {
    Ok(FairshareGroup {
        name: row.try_get("name")?,
        weight: row.try_get("weight")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

impl Store {
    pub async fn list_fairshare_groups(&self) -> Result<Vec<FairshareGroup>> {
        let rows = sqlx::query(
            "select name, weight, created_at, updated_at from fairshare_groups order by name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(fairshare_group_from_row).collect()
    }

    pub async fn create_fairshare_group(&self, name: &str, weight: i64) -> Result<FairshareGroup> {
        let row = sqlx::query(
            "insert into fairshare_groups (name, weight)
             values ($1, $2)
             returning name, weight, created_at, updated_at",
        )
        .bind(name)
        .bind(weight)
        .fetch_one(&self.pool)
        .await?;
        fairshare_group_from_row(&row)
    }

    pub async fn update_fairshare_group_weight(
        &self,
        name: &str,
        weight: i64,
    ) -> Result<FairshareGroup> {
        let row = sqlx::query(
            "update fairshare_groups set weight = $2, updated_at = now() where name = $1
             returning name, weight, created_at, updated_at",
        )
        .bind(name)
        .bind(weight)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        fairshare_group_from_row(&row)
    }

    pub async fn update_tenant_fairshare_group(
        &self,
        id: Uuid,
        fairshare_group: &str,
    ) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(&format!(
            "update tenants set fairshare_group = $2, updated_at = now() where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(fairshare_group)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    // ---- tenants ---------------------------------------------------------

    pub async fn create_tenant(
        &self,
        name: &str,
        weight: i64,
        tokens_per_minute: i64,
        max_in_flight: Option<i64>,
        fairshare_group: Option<&str>,
    ) -> Result<Tenant> {
        let group = fairshare_group.unwrap_or("default");
        let row = sqlx::query(&format!(
            "insert into tenants (id, name, fairshare_group, weight, tokens_per_minute, max_in_flight)
             values ($1, $2, $3, $4, $5, $6)
             returning {TENANT_COLUMNS}"))
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(group)
        .bind(weight)
        .bind(tokens_per_minute.max(0))
        .bind(max_in_flight)
        .fetch_one(&self.pool)
        .await?;
        tenant_from_row(&row)
    }

    pub async fn list_tenants(&self) -> Result<Vec<Tenant>> {
        let rows = sqlx::query(&format!(
            "select {TENANT_COLUMNS}
             from tenants order by created_at"
        ))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(tenant_from_row).collect()
    }

    /// Cheap dashboard counts: one aggregate row instead of deserializing the
    /// full tenant/key/model lists (the key fleet alone can be 100k+ rows and
    /// this backs a poll that fires on every dashboard page).
    pub async fn overview_counts(&self) -> Result<OverviewCounts> {
        let row = sqlx::query(
            "select
               (select count(*) from tenants) as tenant_count,
               (select count(*) from api_keys) as key_count,
               (select count(*) from models) as model_count,
               (select count(*) from models where enabled) as enabled_models,
               (select coalesce(bool_or(input_cost_per_token > 0 or output_cost_per_token > 0), false)
                  from models) as has_pricing",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok(OverviewCounts {
            tenant_count: row.try_get("tenant_count")?,
            key_count: row.try_get("key_count")?,
            model_count: row.try_get("model_count")?,
            enabled_models: row.try_get("enabled_models")?,
            has_pricing: row.try_get("has_pricing")?,
        })
    }

    pub async fn get_tenant(&self, id: Uuid) -> Result<Tenant> {
        let row = sqlx::query(&format!(
            "select {TENANT_COLUMNS}
             from tenants where id = $1"
        ))
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    pub async fn tenant_by_name(&self, name: &str) -> Result<Option<Tenant>> {
        let row = sqlx::query(&format!(
            "select {TENANT_COLUMNS}
             from tenants where name = $1"
        ))
        .bind(name)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(tenant_from_row).transpose()
    }

    pub async fn update_tenant_weight(&self, id: Uuid, weight: i64) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(&format!(
            "update tenants set weight = $2, updated_at = now() where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(weight)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    pub async fn update_tenant_quota(
        &self,
        id: Uuid,
        tokens_per_minute: i64,
        max_in_flight: Option<i64>,
    ) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(&format!(
            "update tenants set tokens_per_minute = $2, max_in_flight = $3, updated_at = now()
             where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(tokens_per_minute.max(0))
        .bind(max_in_flight)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    /// Update the editable directory fields (name + metadata) of a tenant.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_tenant_details(
        &self,
        id: Uuid,
        name: &str,
        description: &str,
        organization: &str,
        contact_email: &str,
    ) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(&format!(
            "update tenants set name = $2, description = $3, organization = $4,
                    contact_email = $5, updated_at = now()
             where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(organization)
        .bind(contact_email)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    /// Set a tenant's lifecycle status (`active`, `suspended`, `archived`).
    pub async fn set_tenant_status(&self, id: Uuid, status: &str) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(&format!(
            "update tenants set status = $2, updated_at = now() where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(status)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    /// Update a tenant's schedule: timezone, optional activation/expiry instants,
    /// and optional recurring weekly windows. Passing an empty window list clears
    /// the recurring schedule (stored as SQL null).
    pub async fn update_tenant_schedule(
        &self,
        id: Uuid,
        timezone: &str,
        active_from: Option<DateTime<Utc>>,
        active_until: Option<DateTime<Utc>>,
        weekly_windows: Option<Vec<WeeklyWindow>>,
    ) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let windows = weekly_windows
            .filter(|w| !w.is_empty())
            .map(sqlx::types::Json);
        let row = sqlx::query(&format!(
            "update tenants set timezone = $2, active_from = $3, active_until = $4,
                    weekly_windows = $5, updated_at = now()
             where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(timezone)
        .bind(active_from)
        .bind(active_until)
        .bind(windows)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    /// Set a tenant's term budget: optional token and/or USD-cost caps, the reset
    /// period (`lifetime`/`monthly`/`term`), and the term start instant. Passing
    /// `None` caps clears the corresponding ceiling.
    pub async fn update_tenant_budget(
        &self,
        id: Uuid,
        budget_tokens: Option<i64>,
        budget_cost_usd: Option<f64>,
        budget_period: Option<&str>,
        budget_started_at: Option<DateTime<Utc>>,
    ) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(&format!(
            "update tenants set budget_tokens = $2, budget_cost_usd = $3, budget_period = $4,
                    budget_started_at = $5, updated_at = now()
             where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(budget_tokens)
        .bind(budget_cost_usd)
        .bind(budget_period)
        .bind(budget_started_at)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    /// Set a tenant's model allowlist. An empty list clears the allowlist (stored
    /// as SQL null), meaning every registered model is permitted.
    pub async fn update_tenant_allowlist(
        &self,
        id: Uuid,
        allowed_models: Option<Vec<String>>,
    ) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let allowed = allowed_models
            .filter(|m| !m.is_empty())
            .map(sqlx::types::Json);
        let row = sqlx::query(&format!(
            "update tenants set allowed_models = $2, updated_at = now()
             where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(allowed)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    /// Set or clear a tenant's guardrails policy. `None` removes the policy.
    pub async fn update_tenant_guardrails_policy(
        &self,
        id: Uuid,
        policy: Option<obleth_config::GuardrailsPolicy>,
    ) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let encoded = policy.map(sqlx::types::Json);
        let row = sqlx::query(&format!(
            "update tenants set guardrails_policy = $2, updated_at = now()
             where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(encoded)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    /// Set or clear a tenant's compression policy. `None` removes the policy.
    pub async fn update_tenant_compression_policy(
        &self,
        id: Uuid,
        policy: Option<obleth_config::CompressionPolicy>,
    ) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let encoded = policy.map(sqlx::types::Json);
        let row = sqlx::query(&format!(
            "update tenants set compression_policy = $2, updated_at = now()
             where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(encoded)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    /// Hard-delete a tenant. Cascades to its API keys (FK `on delete cascade`).
    /// Returns the key hashes that were removed so callers can evict caches.
    pub async fn delete_tenant(&self, id: Uuid) -> Result<Vec<String>> {
        Self::guard_reserved_tenant(id)?;
        let mut tx = self.pool.begin().await?;
        // Lock the tenant row before snapshotting key hashes: a concurrent key
        // insert must take a KEY SHARE lock on this row for its FK check, so
        // holding FOR UPDATE guarantees no key can appear between the hash
        // snapshot and the cascade delete and dodge cache eviction.
        sqlx::query("select 1 from tenants where id = $1 for update")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::NotFound)?;
        let hashes: Vec<String> = sqlx::query("select key_hash from api_keys where tenant_id = $1")
            .bind(id)
            .fetch_all(&mut *tx)
            .await?
            .iter()
            .map(|r| r.try_get::<String, _>("key_hash"))
            .collect::<std::result::Result<_, _>>()?;
        sqlx::query("delete from tenants where id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(hashes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{default_test_fields, serial, FixtureGuard};
    use obleth_config::hash_api_key;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn tenant_key_audit_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("t-{}", Uuid::new_v4());
        let tenant = store
            .create_tenant(&name, 250, 1000, Some(4), None)
            .await
            .expect("create tenant");
        fixtures.track_tenant(tenant.id);
        assert_eq!(tenant.weight, 250);

        let (key, secret) = store
            .create_api_key(
                tenant.id,
                "k",
                "",
                None,
                None,
                None,
                None,
                100,
                None,
                &[],
                &[],
            )
            .await
            .expect("create key");
        let hash = hash_api_key(&secret);
        let resolved = store
            .resolved_key_by_hash(&hash)
            .await
            .expect("resolve")
            .expect("present");
        assert_eq!(resolved.tenant_id, tenant.id);
        assert_eq!(resolved.weight, 250);
        assert!(!resolved.disabled);

        // weight change must propagate to the resolved (hot-cache) view
        store
            .update_tenant_weight(tenant.id, 999)
            .await
            .expect("update weight");
        let resolved = store.resolved_key_by_hash(&hash).await.unwrap().unwrap();
        assert_eq!(resolved.weight, 999);

        store
            .record_audit(
                "test",
                "noop",
                "api_key",
                &key.id.to_string(),
                serde_json::json!({}),
            )
            .await
            .expect("audit");
        let audit = store.list_audit(10).await.expect("list audit");
        assert!(!audit.is_empty());

        let model = store
            .create_model(&NewModel::new(
                format!("m-{}", Uuid::new_v4()),
                ModelFields {
                    description: "integration test model".into(),
                    upstream_model: "upstream-model".into(),
                    api_base: "http://127.0.0.1:8081".into(),
                    // A non-chat type: the scheduled claim query must round-trip
                    // `model_type`, or the worker probes every model against
                    // /chat/completions (regression 2026-07-06 — embedding/TTS
                    // models were pinned `degraded` by a chat probe they don't serve).
                    model_type: "embedding".into(),
                    context_window: 8192,
                    admission_weight: 100,
                    supports_system_messages: true,
                    route_bias: 1.0,
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);
        let health = store
            .get_model_health_summary(model.id)
            .await
            .expect("health summary");
        assert_eq!(health.status, "unknown");
        assert!(health.checks_enabled);

        store
            .update_model_health_config(
                model.id,
                ModelHealthConfigUpdate {
                    checks_enabled: true,
                    alerts_enabled: true,
                    check_interval_secs: 60,
                    failure_threshold: 1,
                    maintenance_until: None,
                    maintenance_note: None,
                },
            )
            .await
            .expect("update health config");

        let next = Utc::now() + chrono::Duration::seconds(60);
        let failed = store
            .record_model_health_check(
                model.id,
                "manual",
                "unhealthy",
                Some(12),
                Some(500),
                Some("failed"),
                Some("body"),
                next,
            )
            .await
            .expect("record failed health");
        assert_eq!(failed.alert_event, Some(ModelHealthAlertEvent::Down));
        assert_eq!(failed.summary.consecutive_failures, 1);

        let recovered = store
            .record_model_health_check(
                model.id,
                "manual",
                "healthy",
                Some(8),
                Some(200),
                Some("ok"),
                None,
                next,
            )
            .await
            .expect("record recovery");
        assert_eq!(recovered.alert_event, Some(ModelHealthAlertEvent::Recovery));
        assert_eq!(recovered.summary.consecutive_failures, 0);

        let checks = store
            .list_model_health_checks(model.id, 10)
            .await
            .expect("health checks");
        assert!(checks.len() >= 2);

        // A probe-relevant config change resets health state: the failure
        // streak and alert state are cleared, the badge returns to `unknown`,
        // and the model is immediately due for a re-check.
        let failed_again = store
            .record_model_health_check(
                model.id,
                "manual",
                "unhealthy",
                Some(12),
                Some(404),
                Some("failed"),
                None,
                Utc::now() + chrono::Duration::seconds(3600),
            )
            .await
            .expect("record failed health again");
        assert_eq!(failed_again.summary.consecutive_failures, 1);
        assert_eq!(failed_again.summary.alert_state, "firing");
        store
            .reset_model_health(model.id)
            .await
            .expect("reset health");
        let reset = store
            .get_model_health_summary(model.id)
            .await
            .expect("summary after reset");
        assert_eq!(reset.status, "unknown");
        assert_eq!(reset.consecutive_failures, 0);
        assert_eq!(reset.alert_state, "ok");
        assert!(reset.next_check_at <= Utc::now());

        store
            .update_model_health_config(
                model.id,
                ModelHealthConfigUpdate {
                    checks_enabled: true,
                    alerts_enabled: true,
                    check_interval_secs: 60,
                    failure_threshold: 1,
                    maintenance_until: None,
                    maintenance_note: None,
                },
            )
            .await
            .expect("make due");
        // The test database is shared: other tests' models can be due too, and
        // the claim takes the longest-overdue first. Put this one first.
        sqlx::query(
            "update models set health_next_check_at = now() - interval '1 day' where id = $1",
        )
        .bind(model.id)
        .execute(&store.pool)
        .await
        .expect("make most overdue");
        let claims = store
            .claim_due_model_health_checks(10)
            .await
            .expect("claim due");
        let claimed = claims
            .iter()
            .find(|claim| claim.model.id == model.id)
            .expect("model is due and claimed");
        // The claimed model must carry its real type, not the `chat` default of
        // a tolerant read — otherwise the scheduled worker probes the wrong
        // modality endpoint.
        assert_eq!(claimed.model.model_type, "embedding");

        let deleted = store
            .delete_model_health_checks_before(Utc::now() + chrono::Duration::days(1))
            .await
            .expect("delete old checks");
        assert!(deleted >= 2);

        // the batched resolve must attach this model's endpoints
        store
            .create_model_endpoint(
                model.id,
                "primary",
                "http://127.0.0.1:8082",
                None,
                0,
                1,
                true,
                None,
            )
            .await
            .expect("create endpoint");
        let resolved_models = store.all_resolved_models().await.expect("resolved models");
        let (_, resolved_model) = resolved_models
            .iter()
            .find(|(n, _)| n == &model.model_name)
            .expect("model resolved");
        assert_eq!(resolved_model.endpoints.len(), 1);
        assert_eq!(
            resolved_model.endpoints[0].api_base,
            "http://127.0.0.1:8082"
        );

        // Reliability settings must survive the admin read paths: the write
        // persists them, but `list_models`/`get_model` historically omitted these
        // columns from their SELECT, so the control plane always re-displayed
        // defaults. Guard every admin read that feeds the UI.
        store
            .update_model_reliability(model.id, Some(600), 3, 300, "load_balance", false)
            .await
            .expect("update reliability");
        let fetched = store.get_model(model.id).await.expect("get_model");
        assert_eq!(fetched.request_timeout_secs, Some(600));
        assert_eq!(fetched.max_retries, 3);
        assert_eq!(fetched.retry_backoff_ms, 300);
        assert_eq!(fetched.endpoint_selection_mode, "load_balance");
        let listed = store.list_models().await.expect("list_models");
        let listed_model = listed
            .iter()
            .find(|m| m.id == model.id)
            .expect("model listed");
        assert_eq!(listed_model.request_timeout_secs, Some(600));
        assert_eq!(listed_model.max_retries, 3);
        assert_eq!(listed_model.retry_backoff_ms, 300);
        assert_eq!(listed_model.endpoint_selection_mode, "load_balance");

        // tenant delete must report the cascade-deleted key hashes
        let hashes = store.delete_tenant(tenant.id).await.expect("delete tenant");
        assert!(hashes.contains(&hash));
        assert!(store.resolved_key_by_hash(&hash).await.unwrap().is_none());
        assert!(matches!(
            store.delete_tenant(tenant.id).await,
            Err(StoreError::NotFound)
        ));
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Every tenant write answers with the tenant's `tracing_enabled` and
    /// `synthetic` flags as stored, so a response (and the audit row made from
    /// it) can no longer report them as false.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn tenant_writes_keep_the_flags() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("t-{}", Uuid::new_v4());
        let id = store
            .create_tenant(&name, 100, 0, None, None)
            .await
            .expect("create tenant")
            .id;
        fixtures.track_tenant(id);
        store
            .set_tenant_tracing(id, true)
            .await
            .expect("set tracing");
        let flagged = store
            .set_tenant_synthetic(id, true)
            .await
            .expect("set synthetic");
        assert!(flagged.tracing_enabled && flagged.synthetic);

        let written: Vec<(&str, Tenant)> = vec![
            ("weight", store.update_tenant_weight(id, 7).await.unwrap()),
            (
                "quota",
                store.update_tenant_quota(id, 10, Some(2)).await.unwrap(),
            ),
            (
                "details",
                store
                    .update_tenant_details(id, &name, "d", "o", "e")
                    .await
                    .unwrap(),
            ),
            (
                "status",
                store.set_tenant_status(id, "active").await.unwrap(),
            ),
            (
                "schedule",
                store
                    .update_tenant_schedule(id, "UTC", None, None, None)
                    .await
                    .unwrap(),
            ),
            (
                "budget",
                store
                    .update_tenant_budget(id, Some(1), None, None, None)
                    .await
                    .unwrap(),
            ),
            (
                "allowlist",
                store.update_tenant_allowlist(id, None).await.unwrap(),
            ),
            (
                "guardrails",
                store
                    .update_tenant_guardrails_policy(id, None)
                    .await
                    .unwrap(),
            ),
            (
                "compression",
                store
                    .update_tenant_compression_policy(id, None)
                    .await
                    .unwrap(),
            ),
            (
                "fairshare_group",
                store
                    .update_tenant_fairshare_group(id, "default")
                    .await
                    .unwrap(),
            ),
        ];
        for (write, tenant) in written {
            assert!(tenant.tracing_enabled, "{write} dropped tracing_enabled");
            assert!(tenant.synthetic, "{write} dropped synthetic");
        }
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// `synthetic` must default to false, flip via `set_tenant_synthetic`, and
    /// flow through into the resolved hot-path key view.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn synthetic_flag_round_trips_to_resolved_key() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("t-{}", Uuid::new_v4());
        let tenant = store
            .create_tenant(&name, 100, 0, None, None)
            .await
            .expect("create tenant");
        fixtures.track_tenant(tenant.id);
        assert!(!tenant.synthetic, "tenants default to non-synthetic");

        store
            .set_tenant_synthetic(tenant.id, true)
            .await
            .expect("set synthetic");
        assert!(
            store
                .get_tenant(tenant.id)
                .await
                .expect("get tenant")
                .synthetic
        );

        let (_key, secret) = store
            .create_api_key(
                tenant.id,
                "k",
                "",
                None,
                None,
                None,
                None,
                100,
                None,
                &[],
                &[],
            )
            .await
            .expect("create key");
        let hash = hash_api_key(&secret);
        let resolved = store
            .resolved_key_by_hash(&hash)
            .await
            .expect("resolve")
            .expect("present");
        assert!(resolved.synthetic);
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test]
    async fn control_plane_identity_is_idempotent() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");

        store
            .ensure_control_plane_identity()
            .await
            .expect("provision");
        let first = store.control_plane_key_secret().await.expect("read secret");
        assert!(first.is_some(), "secret stored on first provision");

        // Second call must not rotate the secret or create a duplicate key.
        store
            .ensure_control_plane_identity()
            .await
            .expect("re-provision");
        let second = store
            .control_plane_key_secret()
            .await
            .expect("read secret again");
        assert_eq!(first, second, "secret stable across repeat provisioning");

        // The stored secret resolves to a key owned by the reserved tenant.
        let hash = hash_api_key(first.as_deref().unwrap());
        let resolved = store
            .resolved_key_by_hash(&hash)
            .await
            .expect("resolve")
            .expect("present");
        assert_eq!(resolved.tenant_id, Store::CONTROL_PLANE_TENANT_ID);

        // The reserved tenant is synthetic, so its traffic is tagged
        // `benchmark` and excluded from usage stats by default. This must hold
        // on the resolved hot-path view the proxy actually reads.
        assert!(
            resolved.synthetic,
            "control-plane traffic must resolve as synthetic"
        );

        // Exactly one key under the reserved tenant (idempotent, no duplicates).
        let keys = store
            .list_keys(Some(Store::CONTROL_PLANE_TENANT_ID))
            .await
            .expect("list keys");
        assert_eq!(keys.len(), 1, "exactly one reserved key");
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    #[tokio::test]
    async fn reserved_control_plane_identity_is_protected() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        store
            .ensure_control_plane_identity()
            .await
            .expect("provision");

        let id = Store::CONTROL_PLANE_TENANT_ID;
        // Tenant mutations on the reserved id are rejected with Protected.
        assert!(matches!(
            store.set_tenant_status(id, "suspended").await,
            Err(StoreError::Protected(_))
        ));
        assert!(matches!(
            store.update_tenant_weight(id, 5).await,
            Err(StoreError::Protected(_))
        ));
        assert!(matches!(
            store.delete_tenant(id).await,
            Err(StoreError::Protected(_))
        ));

        // The reserved key cannot be disabled or deleted either.
        let secret = store.control_plane_key_secret().await.unwrap().unwrap();
        let hash = hash_api_key(&secret);
        let resolved = store.resolved_key_by_hash(&hash).await.unwrap().unwrap();
        assert!(matches!(
            store
                .set_key_flag(resolved.key_id, KeyFlag::Disabled, true)
                .await,
            Err(StoreError::Protected(_))
        ));
        assert!(matches!(
            store.delete_key(resolved.key_id).await,
            Err(StoreError::Protected(_))
        ));
    }
}
