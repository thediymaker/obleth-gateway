//! Model health checks.

use super::*;

#[derive(Debug, Clone)]
pub struct ModelHealthConfigUpdate {
    pub checks_enabled: bool,
    pub alerts_enabled: bool,
    pub check_interval_secs: i64,
    pub failure_threshold: i64,
    pub maintenance_until: Option<DateTime<Utc>>,
    pub maintenance_note: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ModelHealthClaim {
    pub model: ModelRoute,
    pub summary: ModelHealthSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelHealthAlertEvent {
    Down,
    Recovery,
}

#[derive(Debug, Clone)]
pub struct ModelHealthRecordOutcome {
    pub summary: ModelHealthSummary,
    pub check: ModelHealthCheck,
    pub alert_event: Option<ModelHealthAlertEvent>,
}

fn model_health_summary_from_row(row: &PgRow) -> Result<ModelHealthSummary> {
    Ok(ModelHealthSummary {
        model_id: row.try_get("model_id")?,
        model_name: row.try_get("model_name")?,
        checks_enabled: row.try_get("health_checks_enabled")?,
        alerts_enabled: row.try_get("health_alerts_enabled")?,
        check_interval_secs: row.try_get("health_check_interval_secs")?,
        failure_threshold: row.try_get("health_failure_threshold")?,
        maintenance_until: row.try_get("health_maintenance_until")?,
        maintenance_note: row.try_get("health_maintenance_note")?,
        status: row.try_get("health_status")?,
        consecutive_failures: row.try_get("health_consecutive_failures")?,
        alert_state: row.try_get("health_alert_state")?,
        next_check_at: row.try_get("health_next_check_at")?,
        last_checked_at: row.try_get("health_last_checked_at")?,
        last_latency_ms: row.try_get("health_last_latency_ms")?,
        last_http_status: row.try_get("health_last_http_status")?,
        last_message: row.try_get("health_last_message")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn model_health_check_from_row(row: &PgRow) -> Result<ModelHealthCheck> {
    Ok(ModelHealthCheck {
        id: row.try_get("id")?,
        model_id: row.try_get("model_id")?,
        checked_at: row.try_get("checked_at")?,
        trigger: row.try_get("trigger")?,
        status: row.try_get("status")?,
        latency_ms: row.try_get("latency_ms")?,
        http_status: row.try_get("http_status")?,
        message: row.try_get("message")?,
        response_excerpt: row.try_get("response_excerpt")?,
    })
}

fn model_health_claim_from_row(row: &PgRow) -> Result<ModelHealthClaim> {
    Ok(ModelHealthClaim {
        model: model_from_row(row)?,
        summary: model_health_summary_from_row(row)?,
    })
}

impl Store {
    pub async fn list_model_health_summaries(&self) -> Result<Vec<ModelHealthSummary>> {
        let rows = sqlx::query(
            "select id as model_id, model_name,
                    health_checks_enabled, health_alerts_enabled,
                    health_check_interval_secs, health_failure_threshold,
                    health_maintenance_until, health_maintenance_note,
                    health_status, health_consecutive_failures, health_alert_state,
                    health_next_check_at, health_last_checked_at,
                    health_last_latency_ms, health_last_http_status,
                    health_last_message, updated_at
             from models order by model_name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(model_health_summary_from_row).collect()
    }

    pub async fn get_model_health_detail(&self, id: Uuid, limit: i64) -> Result<ModelHealthDetail> {
        let summary = self.get_model_health_summary(id).await?;
        let checks = self.list_model_health_checks(id, limit).await?;
        Ok(ModelHealthDetail { summary, checks })
    }

    pub async fn get_model_health_summary(&self, id: Uuid) -> Result<ModelHealthSummary> {
        let row = sqlx::query(
            "select id as model_id, model_name,
                    health_checks_enabled, health_alerts_enabled,
                    health_check_interval_secs, health_failure_threshold,
                    health_maintenance_until, health_maintenance_note,
                    health_status, health_consecutive_failures, health_alert_state,
                    health_next_check_at, health_last_checked_at,
                    health_last_latency_ms, health_last_http_status,
                    health_last_message, updated_at
             from models where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_health_summary_from_row(&row)
    }

    pub async fn list_model_health_checks(
        &self,
        model_id: Uuid,
        limit: i64,
    ) -> Result<Vec<ModelHealthCheck>> {
        let rows = sqlx::query(
            "select id, model_id, checked_at, trigger, status, latency_ms,
                    http_status, message, response_excerpt
             from model_health_checks
             where model_id = $1
             order by checked_at desc
             limit $2",
        )
        .bind(model_id)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(model_health_check_from_row).collect()
    }

    pub async fn update_model_health_config(
        &self,
        id: Uuid,
        config: ModelHealthConfigUpdate,
    ) -> Result<ModelHealthSummary> {
        let row = sqlx::query(
            "update models set
                health_checks_enabled = $2,
                health_alerts_enabled = $3,
                health_check_interval_secs = $4,
                health_failure_threshold = $5,
                health_maintenance_until = $6,
                health_maintenance_note = $7,
                health_next_check_at = case when $2 then now() else health_next_check_at end,
                health_alert_state = case
                    when not $2 or not $3 or ($6 is not null and $6 > now()) then 'ok'
                    else health_alert_state
                end,
                updated_at = now()
             where id = $1
             returning id as model_id, model_name,
                       health_checks_enabled, health_alerts_enabled,
                       health_check_interval_secs, health_failure_threshold,
                       health_maintenance_until, health_maintenance_note,
                       health_status, health_consecutive_failures, health_alert_state,
                       health_next_check_at, health_last_checked_at,
                       health_last_latency_ms, health_last_http_status,
                       health_last_message, updated_at",
        )
        .bind(id)
        .bind(config.checks_enabled)
        .bind(config.alerts_enabled)
        .bind(config.check_interval_secs.max(60))
        .bind(config.failure_threshold.max(1))
        .bind(config.maintenance_until)
        .bind(config.maintenance_note)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_health_summary_from_row(&row)
    }

    /// Reset a model's health state after a probe-relevant config change
    /// (`api_base` / `upstream_model` / `model_type`): the old failure streak
    /// and alert state describe a configuration that no longer exists.
    /// `health_next_check_at = now()` makes the scheduler re-verify within one
    /// worker tick, so the fix proves itself immediately. Check history rows
    /// are left untouched.
    pub async fn reset_model_health(&self, id: Uuid) -> Result<()> {
        let r = sqlx::query(
            "update models set
                health_status = 'unknown',
                health_consecutive_failures = 0,
                health_alert_state = 'ok',
                health_next_check_at = now(),
                updated_at = now()
             where id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        if r.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub async fn claim_due_model_health_checks(&self, limit: i64) -> Result<Vec<ModelHealthClaim>> {
        let rows = sqlx::query(
            "with due as (
                select id
                from models
                where enabled = true
                  and health_checks_enabled = true
                  and health_next_check_at <= now()
                  and (health_maintenance_until is null or health_maintenance_until <= now())
                order by health_next_check_at
                for update skip locked
                limit $1
             )
             update models m
             set health_next_check_at =
                    now() + make_interval(secs => greatest(m.health_check_interval_secs, 60)::double precision)
             from due
             where m.id = due.id
             returning m.id, m.model_name, m.description, m.upstream_model, m.api_base, m.api_key,
                       m.upstream_headers, m.model_type, m.lifecycle,
                       m.input_cost_per_token, m.output_cost_per_token, m.context_window,
                       m.admission_weight, m.max_in_flight,
                       m.supports_function_calling, m.supports_system_messages,
                       m.supports_response_schema, m.supports_tool_choice, m.enabled,
                       m.cache_enabled, m.cache_ttl_secs,
                       m.created_at, m.updated_at,
                       m.id as model_id,
                       m.health_checks_enabled, m.health_alerts_enabled,
                       m.health_check_interval_secs, m.health_failure_threshold,
                       m.health_maintenance_until, m.health_maintenance_note,
                       m.health_status, m.health_consecutive_failures, m.health_alert_state,
                       m.health_next_check_at, m.health_last_checked_at,
                       m.health_last_latency_ms, m.health_last_http_status,
                       m.health_last_message",
        )
        .bind(limit.clamp(1, 100))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(model_health_claim_from_row).collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn record_model_health_check(
        &self,
        model_id: Uuid,
        trigger: &str,
        status: &str,
        latency_ms: Option<i64>,
        http_status: Option<i64>,
        message: Option<&str>,
        response_excerpt: Option<&str>,
        next_check_at: DateTime<Utc>,
    ) -> Result<ModelHealthRecordOutcome> {
        let mut tx = self.pool.begin().await?;
        let current = sqlx::query(
            "select health_alerts_enabled, health_failure_threshold,
                    health_maintenance_until, health_consecutive_failures,
                    health_alert_state, health_status
             from models where id = $1 for update",
        )
        .bind(model_id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound)?;

        let checked_at = Utc::now();
        let check_row = sqlx::query(
            "insert into model_health_checks (
                model_id, checked_at, trigger, status, latency_ms,
                http_status, message, response_excerpt
             ) values ($1, $2, $3, $4, $5, $6, $7, $8)
             returning id, model_id, checked_at, trigger, status, latency_ms,
                       http_status, message, response_excerpt",
        )
        .bind(model_id)
        .bind(checked_at)
        .bind(trigger)
        .bind(status)
        .bind(latency_ms)
        .bind(http_status)
        .bind(message)
        .bind(response_excerpt)
        .fetch_one(&mut *tx)
        .await?;
        let check = model_health_check_from_row(&check_row)?;

        let alerts_enabled: bool = current.try_get("health_alerts_enabled")?;
        let failure_threshold: i64 = current.try_get("health_failure_threshold")?;
        let maintenance_until: Option<DateTime<Utc>> =
            current.try_get("health_maintenance_until")?;
        let previous_failures: i64 = current.try_get("health_consecutive_failures")?;
        let previous_alert_state: String = current.try_get("health_alert_state")?;
        let healthy = status == "healthy";
        let disabled = status == "disabled";
        // Transient/inconclusive outcomes (overloaded upstream, an unsupported
        // probe endpoint, a single network blip) must not flap a model to
        // "down": they neither count toward the failure threshold nor reset a
        // streak that is already in progress, and they never fire an alert.
        let transient = matches!(status, "degraded" | "skipped" | "unknown");
        let in_maintenance = maintenance_until
            .map(|until| until > checked_at)
            .unwrap_or(false);
        let new_failures = if healthy || disabled {
            0
        } else if transient {
            previous_failures
        } else {
            previous_failures.saturating_add(1)
        };
        let should_fire = !healthy
            && !disabled
            && !transient
            && alerts_enabled
            && !in_maintenance
            && new_failures >= failure_threshold.max(1);
        let new_alert_state = if transient {
            previous_alert_state.as_str()
        } else if should_fire {
            "firing"
        } else {
            "ok"
        };
        let alert_event = if healthy && previous_alert_state == "firing" && alerts_enabled {
            Some(ModelHealthAlertEvent::Recovery)
        } else if should_fire && previous_alert_state != "firing" {
            Some(ModelHealthAlertEvent::Down)
        } else {
            None
        };

        let previous_status: String = current.try_get("health_status")?;
        // The displayed badge is threshold-gated, matching the alerting logic: a
        // sub-threshold failure streak holds the last stable badge instead of
        // flapping to "down". The badge shows the fresh `status` verbatim when the
        // outcome is healthy/disabled/transient, or when a failure is confirmed
        // down (streak at/over threshold); a sub-threshold failure holds the last
        // stable badge.
        let show_status = status == "healthy"
            || status == "disabled"
            || transient
            || new_failures >= failure_threshold.max(1);
        let displayed_status = if show_status {
            status
        } else {
            previous_status.as_str() // hold last stable badge
        };

        let summary_row = sqlx::query(
            "update models set
                health_status = $2,
                health_consecutive_failures = $3,
                health_alert_state = $4,
                health_next_check_at = $5,
                health_last_checked_at = $6,
                health_last_latency_ms = $7,
                health_last_http_status = $8,
                health_last_message = $9,
                updated_at = now()
             where id = $1
             returning id as model_id, model_name,
                       health_checks_enabled, health_alerts_enabled,
                       health_check_interval_secs, health_failure_threshold,
                       health_maintenance_until, health_maintenance_note,
                       health_status, health_consecutive_failures, health_alert_state,
                       health_next_check_at, health_last_checked_at,
                       health_last_latency_ms, health_last_http_status,
                       health_last_message, updated_at",
        )
        .bind(model_id)
        .bind(displayed_status)
        .bind(new_failures)
        .bind(new_alert_state)
        .bind(next_check_at)
        .bind(checked_at)
        .bind(latency_ms)
        .bind(http_status)
        .bind(message)
        .fetch_one(&mut *tx)
        .await?;
        let summary = model_health_summary_from_row(&summary_row)?;

        tx.commit().await?;
        Ok(ModelHealthRecordOutcome {
            summary,
            check,
            alert_event,
        })
    }

    pub async fn delete_model_health_checks_before(&self, cutoff: DateTime<Utc>) -> Result<u64> {
        let result = sqlx::query("delete from model_health_checks where checked_at < $1")
            .bind(cutoff)
            .execute(&self.pool)
            .await?;
        Ok(result.rows_affected())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{default_test_fields, serial, FixtureGuard};

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// A single sub-threshold failure must NOT flip the displayed badge.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn health_status_badge_holds_below_threshold() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        // Same positional create_model call as `tenant_key_audit_roundtrip`.
        let model = store
            .create_model(&NewModel::new(
                format!("m-{}", Uuid::new_v4()),
                ModelFields {
                    description: "hysteresis test model".into(),
                    upstream_model: "upstream-model".into(),
                    api_base: "http://127.0.0.1:8081".into(),
                    model_type: "chat".into(),
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

        store
            .update_model_health_config(
                model.id,
                ModelHealthConfigUpdate {
                    checks_enabled: true,
                    alerts_enabled: true,
                    check_interval_secs: 60,
                    failure_threshold: 2,
                    maintenance_until: None,
                    maintenance_note: None,
                },
            )
            .await
            .expect("update health config");

        let next = Utc::now() + chrono::Duration::seconds(60);

        // First failure: streak = 1 < threshold 2 -> badge must NOT move to unhealthy.
        let first = store
            .record_model_health_check(
                model.id,
                "manual",
                "unhealthy",
                Some(12),
                Some(500),
                Some("blip"),
                None,
                next,
            )
            .await
            .expect("record first failure");
        assert_eq!(first.summary.consecutive_failures, 1);
        assert_ne!(
            first.summary.status, "unhealthy",
            "single blip must not flip the badge"
        );
        assert_eq!(first.alert_event, None, "no alert below threshold");

        // Second consecutive failure: streak = 2 >= threshold -> badge flips, alert fires.
        let second = store
            .record_model_health_check(
                model.id,
                "manual",
                "unhealthy",
                Some(13),
                Some(500),
                Some("still down"),
                None,
                next,
            )
            .await
            .expect("record second failure");
        assert_eq!(second.summary.consecutive_failures, 2);
        assert_eq!(second.summary.status, "unhealthy");
        assert_eq!(second.alert_event, Some(ModelHealthAlertEvent::Down));

        // Single healthy check: immediate recovery of badge and streak.
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
        assert_eq!(recovered.summary.consecutive_failures, 0);
        assert_eq!(recovered.summary.status, "healthy");

        // The raw audit trail must still contain the unhealthy rows.
        let checks = store
            .list_model_health_checks(model.id, 10)
            .await
            .expect("checks");
        assert!(
            checks.iter().any(|c| c.status == "unhealthy"),
            "raw unhealthy checks must remain in the audit trail"
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// An `unknown` health check (e.g. for image/audio model types that are not
    /// auto-probed) must be non-alerting and must not increment
    /// `consecutive_failures` — it leaves the counter unchanged, the alert_state
    /// at "ok", and fires no alert event.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn unknown_health_check_is_non_alerting() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(&model_name, default_test_fields()))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

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

        // Record a prior unhealthy check so consecutive_failures starts at 1.
        let prior = store
            .record_model_health_check(
                model.id,
                "manual",
                "unhealthy",
                None,
                None,
                Some("prior failure"),
                None,
                next,
            )
            .await
            .expect("record prior failure");
        let prior_failures = prior.summary.consecutive_failures;

        // An `unknown` check must leave consecutive_failures unchanged and fire
        // no alert event, even though alerts are enabled and threshold is 1.
        let outcome = store
            .record_model_health_check(
                model.id,
                "scheduled",
                "unknown",
                None,
                None,
                Some("model type `image` is not auto-probed; status unverified"),
                None,
                next,
            )
            .await
            .expect("record unknown check");

        assert_eq!(
            outcome.alert_event, None,
            "`unknown` must not fire a Down alert"
        );
        assert_eq!(
            outcome.summary.consecutive_failures, prior_failures,
            "`unknown` must not increment consecutive_failures (was {prior_failures})"
        );
        assert_eq!(
            outcome.summary.status, "unknown",
            "displayed status for an `unknown` check must be `unknown`"
        );
    }
}
