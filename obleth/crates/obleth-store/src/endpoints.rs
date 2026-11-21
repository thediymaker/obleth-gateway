//! Model endpoints.

use super::*;

pub(crate) fn resolved_endpoint_from_row(row: &PgRow) -> Result<ResolvedEndpoint> {
    let status: String = row.try_get("health_status")?;
    Ok(ResolvedEndpoint {
        id: row.try_get::<Uuid, _>("id")?.to_string(),
        api_base: row.try_get("api_base")?,
        api_key: cipher().decrypt_opt(row.try_get("api_key")?)?,
        priority: row.try_get("priority")?,
        weight: row.try_get("weight")?,
        enabled: row.try_get("enabled")?,
        // Treat unknown/degraded as eligible (soft-pass); only an
        // explicit unhealthy/disabled state removes an endpoint.
        healthy: !matches!(status.as_str(), "unhealthy" | "disabled"),
        // Tolerant read: statements that don't select it read as unset.
        max_in_flight: row
            .try_get::<Option<i64>, _>("max_in_flight")
            .unwrap_or(None)
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0),
    })
}

pub(crate) fn endpoint_from_row(row: &PgRow) -> Result<ModelEndpoint> {
    Ok(ModelEndpoint {
        id: row.try_get("id")?,
        model_id: row.try_get("model_id")?,
        name: row.try_get("name")?,
        api_base: row.try_get("api_base")?,
        api_key: cipher().decrypt_opt(row.try_get("api_key")?)?,
        priority: row.try_get("priority")?,
        weight: row.try_get("weight")?,
        enabled: row.try_get("enabled")?,
        max_in_flight: row.try_get("max_in_flight").unwrap_or(None),
        health_status: row.try_get("health_status")?,
        consecutive_failures: row.try_get("consecutive_failures")?,
        alert_state: row.try_get("alert_state")?,
        last_checked_at: row.try_get("last_checked_at")?,
        last_latency_ms: row.try_get("last_latency_ms")?,
        last_http_status: row.try_get("last_http_status")?,
        last_message: row.try_get("last_message")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

impl Store {
    /// Hot-path endpoint views for one model: enabled endpoints with their
    /// decrypted upstream keys and current health, ordered by priority.
    pub async fn resolved_endpoints_for(&self, model_id: Uuid) -> Result<Vec<ResolvedEndpoint>> {
        let rows = sqlx::query(
            "select id, api_base, api_key, priority, weight, enabled, health_status, max_in_flight
             from model_endpoints
             where model_id = $1
             order by priority asc, created_at asc",
        )
        .bind(model_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(resolved_endpoint_from_row).collect()
    }

    pub async fn list_model_endpoints(&self, model_id: Uuid) -> Result<Vec<ModelEndpoint>> {
        let rows = sqlx::query(
            "select id, model_id, name, api_base, api_key, priority, weight, enabled, max_in_flight,
                    health_status, consecutive_failures, alert_state,
                    last_checked_at, last_latency_ms, last_http_status, last_message,
                    created_at, updated_at
             from model_endpoints where model_id = $1
             order by priority asc, created_at asc",
        )
        .bind(model_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(endpoint_from_row).collect()
    }

    /// List every endpoint across all models (used for cache warming and the
    /// per-endpoint health scheduler).
    pub async fn all_model_endpoints(&self) -> Result<Vec<ModelEndpoint>> {
        let rows = sqlx::query(
            "select id, model_id, name, api_base, api_key, priority, weight, enabled, max_in_flight,
                    health_status, consecutive_failures, alert_state,
                    last_checked_at, last_latency_ms, last_http_status, last_message,
                    created_at, updated_at
             from model_endpoints order by model_id, priority asc",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(endpoint_from_row).collect()
    }

    pub async fn get_model_endpoint(&self, id: Uuid) -> Result<ModelEndpoint> {
        let row = sqlx::query(
            "select id, model_id, name, api_base, api_key, priority, weight, enabled, max_in_flight,
                    health_status, consecutive_failures, alert_state,
                    last_checked_at, last_latency_ms, last_http_status, last_message,
                    created_at, updated_at
             from model_endpoints where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        endpoint_from_row(&row)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_model_endpoint(
        &self,
        model_id: Uuid,
        name: &str,
        api_base: &str,
        api_key: Option<&str>,
        priority: i64,
        weight: i64,
        enabled: bool,
        max_in_flight: Option<i64>,
    ) -> Result<ModelEndpoint> {
        let api_key = cipher().encrypt_opt(api_key);
        let row = sqlx::query(
            "insert into model_endpoints
                (id, model_id, name, api_base, api_key, priority, weight, enabled, max_in_flight)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             returning id, model_id, name, api_base, api_key, priority, weight, enabled, max_in_flight,
                       health_status, consecutive_failures, alert_state,
                       last_checked_at, last_latency_ms, last_http_status, last_message,
                       created_at, updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(model_id)
        .bind(name)
        .bind(api_base)
        .bind(api_key)
        .bind(priority.max(0))
        .bind(weight.max(1))
        .bind(enabled)
        .bind(max_in_flight.map(|n| n.max(1)))
        .fetch_one(&self.pool)
        .await?;
        // A new endpoint starts as `unknown` and is only ever health-checked as a
        // side effect of the model-level probe, which is gated by the model's
        // `health_next_check_at` (up to a full interval away). Make the model due
        // now so the health worker re-syncs the routing projection within a tick
        // instead of leaving a freshly-promoted replica mis-rated for minutes.
        self.mark_model_health_due(model_id).await;
        endpoint_from_row(&row)
    }

    /// Best-effort: bring a model's next health check forward to now so the
    /// worker re-probes its endpoints promptly after a topology change. Failures
    /// are logged, not propagated — the caller's mutation already succeeded and
    /// the worst case is waiting the normal interval.
    async fn mark_model_health_due(&self, model_id: Uuid) {
        if let Err(error) =
            sqlx::query("update models set health_next_check_at = now() where id = $1")
                .bind(model_id)
                .execute(&self.pool)
                .await
        {
            tracing::warn!(%error, %model_id, "failed to mark model health due after endpoint change");
        }
    }

    /// Update one endpoint of `model_id`. The model scope is part of the match:
    /// an endpoint id belonging to another model is `NotFound`, never edited.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_model_endpoint(
        &self,
        model_id: Uuid,
        id: Uuid,
        name: &str,
        api_base: &str,
        api_key: Option<&str>,
        priority: i64,
        weight: i64,
        enabled: bool,
        max_in_flight: Option<i64>,
    ) -> Result<ModelEndpoint> {
        let max_in_flight = max_in_flight.map(|n| n.max(1));
        // A `None` api_key leaves the stored secret untouched (so the UI can
        // edit other fields without re-entering the key); an empty string
        // clears it.
        let row = match api_key {
            Some(secret) => {
                let enc = cipher().encrypt_opt(Some(secret));
                sqlx::query(
                    "update model_endpoints set name = $2, api_base = $3, api_key = $4,
                            priority = $5, weight = $6, enabled = $7, max_in_flight = $9,
                            updated_at = now()
                     where id = $1 and model_id = $8
                     returning id, model_id, name, api_base, api_key, priority, weight, enabled, max_in_flight,
                               health_status, consecutive_failures, alert_state,
                               last_checked_at, last_latency_ms, last_http_status, last_message,
                               created_at, updated_at",
                )
                .bind(id)
                .bind(name)
                .bind(api_base)
                .bind(enc)
                .bind(priority.max(0))
                .bind(weight.max(1))
                .bind(enabled)
                .bind(model_id)
                .bind(max_in_flight)
                .fetch_optional(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "update model_endpoints set name = $2, api_base = $3,
                            priority = $4, weight = $5, enabled = $6, max_in_flight = $8,
                            updated_at = now()
                     where id = $1 and model_id = $7
                     returning id, model_id, name, api_base, api_key, priority, weight, enabled, max_in_flight,
                               health_status, consecutive_failures, alert_state,
                               last_checked_at, last_latency_ms, last_http_status, last_message,
                               created_at, updated_at",
                )
                .bind(id)
                .bind(name)
                .bind(api_base)
                .bind(priority.max(0))
                .bind(weight.max(1))
                .bind(enabled)
                .bind(model_id)
                .bind(max_in_flight)
                .fetch_optional(&self.pool)
                .await?
            }
        }
        .ok_or(StoreError::NotFound)?;
        endpoint_from_row(&row)
    }

    /// Delete one endpoint of `model_id`; an endpoint id belonging to another
    /// model is `NotFound`, never deleted.
    pub async fn delete_model_endpoint(&self, model_id: Uuid, id: Uuid) -> Result<Uuid> {
        let row = sqlx::query(
            "delete from model_endpoints where id = $1 and model_id = $2 returning model_id",
        )
        .bind(id)
        .bind(model_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        let model_id: Uuid = row.try_get("model_id")?;
        // Removing an endpoint changes the model's live pool; re-probe promptly so
        // the model-level aggregate doesn't lag a full interval behind.
        self.mark_model_health_due(model_id).await;
        Ok(model_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{default_test_fields, serial, FixtureGuard};

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// An endpoint is only reachable through its own model: addressing model
    /// B's endpoint via model A's id must neither edit nor delete it.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn endpoint_writes_are_scoped_to_their_model() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let mut ids = Vec::new();
        for _ in 0..2 {
            let model = store
                .create_model(&NewModel::new(
                    format!("m-{}", Uuid::new_v4()),
                    ModelFields {
                        description: "endpoint scoping test model".into(),
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
            ids.push(model.id);
        }
        let (model_a, model_b) = (ids[0], ids[1]);
        let ep_b = store
            .create_model_endpoint(
                model_b,
                "b",
                "http://127.0.0.1:8082",
                None,
                0,
                1,
                true,
                None,
            )
            .await
            .expect("create endpoint");

        let err = store
            .update_model_endpoint(
                model_a,
                ep_b.id,
                "hijacked",
                "http://127.0.0.1:9999",
                None,
                0,
                1,
                true,
                None,
            )
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::NotFound), "{err:?}");
        let err = store
            .delete_model_endpoint(model_a, ep_b.id)
            .await
            .unwrap_err();
        assert!(matches!(err, StoreError::NotFound), "{err:?}");

        let untouched = store.list_model_endpoints(model_b).await.expect("list");
        assert_eq!(untouched.len(), 1);
        assert_eq!(untouched[0].name, "b");
        assert_eq!(untouched[0].api_base, "http://127.0.0.1:8082");

        // Through its own model the endpoint is still writable.
        store
            .update_model_endpoint(
                model_b,
                ep_b.id,
                "b2",
                "http://127.0.0.1:8083",
                None,
                0,
                1,
                true,
                None,
            )
            .await
            .expect("owner update");
        store
            .delete_model_endpoint(model_b, ep_b.id)
            .await
            .expect("owner delete");
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// A `degraded` probe (reachable, model id unconfirmed) must lift a stale
    /// `unhealthy` so a recovered endpoint returns to rotation — but it must not
    /// downgrade a confirmed `healthy`. Regression for endpoints stuck unhealthy
    /// after a Slurm replica restart until a manual check.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn endpoint_degraded_clears_unhealthy_but_not_healthy() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        // Same positional create_model call as the other health tests.
        let model = store
            .create_model(&NewModel::new(
                format!("m-{}", Uuid::new_v4()),
                ModelFields {
                    description: "endpoint health test model".into(),
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

        let ep = store
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

        // Stale unhealthy from a startup-window probe.
        let down = store
            .record_endpoint_health(ep.id, "unhealthy", None, Some(503), Some("loading"))
            .await
            .expect("record unhealthy");
        assert_eq!(down.health_status, "unhealthy");

        // A later degraded probe (reachable, id mismatch) must lift it to routable.
        let recovered = store
            .record_endpoint_health(ep.id, "degraded", Some(5), Some(200), Some("id mismatch"))
            .await
            .expect("record degraded");
        assert_eq!(
            recovered.health_status, "degraded",
            "degraded must clear a stale unhealthy"
        );

        // From healthy, a degraded probe must NOT downgrade the badge.
        store
            .record_endpoint_health(ep.id, "healthy", Some(4), Some(200), Some("ok"))
            .await
            .expect("record healthy");
        let still_healthy = store
            .record_endpoint_health(ep.id, "degraded", Some(6), Some(200), Some("id mismatch"))
            .await
            .expect("record degraded after healthy");
        assert_eq!(
            still_healthy.health_status, "healthy",
            "degraded must not downgrade a confirmed healthy"
        );
    }
}
