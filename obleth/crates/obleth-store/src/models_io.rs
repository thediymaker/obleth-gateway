//! Transactional writer for the `obleth-models` manifest.
//!
//! All of the merge and validation logic lives in `obleth_config::manifest`,
//! where it is unit-testable without a database. By the time a write reaches
//! this module every field has already been resolved against what is stored, so
//! the SQL here is a plain full-row upsert keyed on `model_name` — no
//! partial-column logic, no "leave this one alone" branches.
//!
//! The whole manifest applies in one transaction. A file that fails validation
//! never reaches here; a file that fails mid-write leaves nothing behind.

use obleth_config::manifest::{EndpointConfig, ModelConfig};
use obleth_config::{ModelEndpoint, ModelRoute};
use sqlx::Row;
use uuid::Uuid;

use crate::{cipher, endpoint_from_row, model_from_row, serialize_tag_levels, Result, Store};

/// One model's resolved target state, ready to write.
#[derive(Debug, Clone)]
pub struct ModelImportWrite {
    pub model_name: String,
    pub config: ModelConfig,
    /// `None` leaves the model's endpoints untouched. `Some` upserts the listed
    /// endpoints by name; endpoints absent from the list are never deleted.
    pub endpoints: Option<Vec<EndpointConfig>>,
}

/// What the write actually did to one model.
#[derive(Debug, Clone)]
pub struct ModelImportOutcome {
    /// Post-write state, so the caller can push it to Redis via `sync_model`.
    pub model: ModelRoute,
    /// True when the row was inserted rather than updated.
    pub created: bool,
    /// Post-write endpoints, present only when the manifest listed any.
    pub endpoints: Option<Vec<ModelEndpoint>>,
}

/// Every column of `models` that `model_from_row` reads. Kept as one const so
/// the insert's RETURNING clause cannot drift from the mapper.
const MODEL_COLUMNS: &str = "id, model_name, aliases, description, upstream_model, api_base, api_key,
     upstream_headers, model_type, quantization, input_cost_per_token, output_cost_per_token, cost_per_image,
     cost_per_audio_second, cost_per_character, cost_per_video, context_window, admission_weight,
     max_in_flight, capacity_mode, capacity_tuned_at, capacity_source, capacity_namespace,
     capacity_selector, per_replica_max_in_flight, capacity_headroom, supports_function_calling,
     supports_system_messages, supports_response_schema, supports_tool_choice,
     supports_vision, enabled, cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
     request_timeout_secs, max_retries, retry_backoff_ms, endpoint_selection_mode,
     debug_diagnostics, energy_slots_per_node, route_bias, auto_eligible, draft_model, verify_api_base, verify_upstream_model,
     created_at, updated_at";

const ENDPOINT_COLUMNS: &str = "id, model_id, name, api_base, api_key, priority, weight, enabled,
     max_in_flight,
     health_status, consecutive_failures, alert_state,
     last_checked_at, last_latency_ms, last_http_status, last_message,
     created_at, updated_at";

impl Store {
    /// Apply a resolved manifest. Returns one outcome per write, in input
    /// order, so the caller can re-sync Redis for exactly the models touched.
    pub async fn apply_model_manifest(
        &self,
        writes: &[ModelImportWrite],
    ) -> Result<Vec<ModelImportOutcome>> {
        let mut tx = self.pool.begin().await?;
        let mut out: Vec<ModelImportOutcome> = Vec::with_capacity(writes.len());

        for write in writes {
            let c = &write.config;
            let sql = format!(
                "insert into models (
                    id, model_name, description, upstream_model, api_base, api_key, model_type,
                    input_cost_per_token, output_cost_per_token, cost_per_image,
                    cost_per_audio_second, cost_per_character, context_window, admission_weight,
                    max_in_flight, capacity_mode, supports_function_calling,
                    supports_system_messages, supports_response_schema, supports_tool_choice,
                    supports_vision, enabled, cache_enabled, cache_ttl_secs, tags, boons,
                    tool_servers, request_timeout_secs, max_retries, retry_backoff_ms,
                    endpoint_selection_mode, debug_diagnostics, energy_slots_per_node,
                    route_bias, auto_eligible,
                    draft_model, verify_api_base, verify_upstream_model, aliases, quantization,
                    upstream_headers, cost_per_video, capacity_namespace, capacity_selector,
                    per_replica_max_in_flight, capacity_source, capacity_headroom
                 ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                    $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31,
                    $32, $33, $34, $35, $36, $37, $38, $39, $40, $41, $42, $43, $44, $45, $46, $47)
                 on conflict (model_name) do update set
                    description = excluded.description,
                    upstream_model = excluded.upstream_model,
                    api_base = excluded.api_base,
                    api_key = excluded.api_key,
                    model_type = excluded.model_type,
                    input_cost_per_token = excluded.input_cost_per_token,
                    output_cost_per_token = excluded.output_cost_per_token,
                    cost_per_image = excluded.cost_per_image,
                    cost_per_audio_second = excluded.cost_per_audio_second,
                    cost_per_character = excluded.cost_per_character,
                    context_window = excluded.context_window,
                    admission_weight = excluded.admission_weight,
                    max_in_flight = excluded.max_in_flight,
                    capacity_mode = excluded.capacity_mode,
                    supports_function_calling = excluded.supports_function_calling,
                    supports_system_messages = excluded.supports_system_messages,
                    supports_response_schema = excluded.supports_response_schema,
                    supports_tool_choice = excluded.supports_tool_choice,
                    supports_vision = excluded.supports_vision,
                    enabled = excluded.enabled,
                    cache_enabled = excluded.cache_enabled,
                    cache_ttl_secs = excluded.cache_ttl_secs,
                    tags = excluded.tags,
                    boons = excluded.boons,
                    tool_servers = excluded.tool_servers,
                    request_timeout_secs = excluded.request_timeout_secs,
                    max_retries = excluded.max_retries,
                    retry_backoff_ms = excluded.retry_backoff_ms,
                    endpoint_selection_mode = excluded.endpoint_selection_mode,
                    debug_diagnostics = excluded.debug_diagnostics,
                    energy_slots_per_node = excluded.energy_slots_per_node,
                    route_bias = excluded.route_bias,
                    auto_eligible = excluded.auto_eligible,
                    draft_model = excluded.draft_model,
                    verify_api_base = excluded.verify_api_base,
                    verify_upstream_model = excluded.verify_upstream_model,
                    aliases = excluded.aliases,
                    quantization = excluded.quantization,
                    upstream_headers = excluded.upstream_headers,
                    cost_per_video = excluded.cost_per_video,
                    capacity_namespace = excluded.capacity_namespace,
                    capacity_selector = excluded.capacity_selector,
                    per_replica_max_in_flight = excluded.per_replica_max_in_flight,
                    capacity_source = excluded.capacity_source,
                    capacity_headroom = excluded.capacity_headroom,
                    updated_at = now()
                 returning {MODEL_COLUMNS}, (xmax = 0) as inserted"
            );

            let row = sqlx::query(&sql)
                .bind(Uuid::new_v4())
                .bind(&write.model_name)
                .bind(&c.description)
                .bind(&c.upstream_model)
                .bind(&c.api_base)
                .bind(cipher().encrypt_opt(c.api_key.as_deref()))
                .bind(&c.model_type)
                .bind(c.input_cost_per_token)
                .bind(c.output_cost_per_token)
                .bind(c.cost_per_image)
                .bind(c.cost_per_audio_second)
                .bind(c.cost_per_character)
                .bind(c.context_window)
                .bind(c.admission_weight)
                .bind(c.max_in_flight)
                .bind(&c.capacity_mode)
                .bind(c.supports_function_calling)
                .bind(c.supports_system_messages)
                .bind(c.supports_response_schema)
                .bind(c.supports_tool_choice)
                .bind(c.supports_vision)
                .bind(c.enabled)
                .bind(c.cache_enabled)
                .bind(c.cache_ttl_secs)
                .bind(sqlx::types::Json(serialize_tag_levels(&c.tags)))
                .bind(sqlx::types::Json(obleth_config::normalize_boons(&c.boons)))
                .bind(sqlx::types::Json(obleth_config::normalize_tool_servers(
                    &c.tool_servers,
                )))
                .bind(c.request_timeout_secs)
                .bind(c.max_retries)
                .bind(c.retry_backoff_ms)
                .bind(&c.endpoint_selection_mode)
                .bind(c.debug_diagnostics)
                .bind(c.energy_slots_per_node)
                .bind(c.route_bias)
                .bind(c.auto_eligible)
                .bind(c.draft_model.trim())
                .bind(c.verify_api_base.trim())
                .bind(c.verify_upstream_model.trim())
                .bind(sqlx::types::Json(obleth_config::normalize_aliases(
                    &c.aliases,
                )))
                .bind(obleth_config::normalize_quantization(&c.quantization))
                .bind(crate::encrypt_upstream_headers(&c.upstream_headers))
                .bind(c.cost_per_video.max(0.0))
                .bind(obleth_config::capacity::normalize_optional_text(
                    c.capacity_namespace.as_deref(),
                ))
                .bind(obleth_config::capacity::normalize_optional_text(
                    c.capacity_selector.as_deref(),
                ))
                .bind(c.per_replica_max_in_flight.map(|n| n.max(1)))
                .bind(&c.capacity_source)
                .bind(c.capacity_headroom)
                .fetch_one(&mut *tx)
                .await?;

            let model = model_from_row(&row)?;
            let created: bool = row.try_get("inserted")?;

            let endpoints = match &write.endpoints {
                None => None,
                Some(list) => {
                    let mut written = Vec::with_capacity(list.len());
                    for e in list {
                        let sql = format!(
                            "insert into model_endpoints
                                (id, model_id, name, api_base, api_key, priority, weight, enabled,
                                 max_in_flight)
                             values ($1, $2, $3, $4, $5, $6, $7, $8, $9)
                             on conflict (model_id, name) do update set
                                api_base = excluded.api_base,
                                api_key = excluded.api_key,
                                priority = excluded.priority,
                                weight = excluded.weight,
                                enabled = excluded.enabled,
                                max_in_flight = excluded.max_in_flight,
                                updated_at = now()
                             returning {ENDPOINT_COLUMNS}"
                        );
                        let row = sqlx::query(&sql)
                            .bind(Uuid::new_v4())
                            .bind(model.id)
                            .bind(&e.name)
                            .bind(&e.api_base)
                            .bind(cipher().encrypt_opt(e.api_key.as_deref()))
                            .bind(e.priority)
                            .bind(e.weight)
                            .bind(e.enabled)
                            .bind(e.max_in_flight.map(|n| n.max(1)))
                            .fetch_one(&mut *tx)
                            .await?;
                        written.push(endpoint_from_row(&row)?);
                    }
                    Some(written)
                }
            };

            out.push(ModelImportOutcome {
                model,
                created,
                endpoints,
            });
        }

        tx.commit().await?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{serial, test_db_url, FixtureGuard};
    use crate::StoreError;
    use obleth_config::manifest::{resolve_model, ManifestModel};

    fn write(name: &str, config: ModelConfig) -> ModelImportWrite {
        ModelImportWrite {
            model_name: name.to_string(),
            config,
            endpoints: None,
        }
    }

    fn base_config() -> ModelConfig {
        ModelConfig {
            upstream_model: "vendor/model".into(),
            api_base: "http://127.0.0.1:8000/v1".into(),
            tags: vec!["coding".into()],
            energy_slots_per_node: 8,
            ..Default::default()
        }
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` points at a
    /// throwaway Postgres. Skips silently otherwise.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn manifest_creates_then_updates_by_name() {
        let Some(url) = test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("manifest-{}", Uuid::new_v4());

        // First apply creates.
        let out = store
            .apply_model_manifest(&[write(&name, base_config())])
            .await
            .expect("create");
        assert_eq!(out.len(), 1);
        assert!(out[0].created, "first apply must insert");
        let id = out[0].model.id;
        fixtures.track_model(id);
        assert_eq!(out[0].model.tags, vec!["coding".to_string()]);
        assert_eq!(out[0].model.energy_slots_per_node, 8);

        // Second apply with a changed field updates the same row.
        let mut changed = base_config();
        changed.tags = vec!["general".into()];
        let out = store
            .apply_model_manifest(&[write(&name, changed)])
            .await
            .expect("update");
        assert!(!out[0].created, "second apply must update, not insert");
        assert_eq!(out[0].model.id, id, "must match the same row by name");
        assert_eq!(out[0].model.tags, vec!["general".to_string()]);
    }

    /// The round trip the feature exists for: export a model, resolve the
    /// exported entry back against it, and confirm nothing is reported as
    /// changed — through real storage, not just the in-memory merge.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_stored_model_round_trips_through_the_manifest_unchanged() {
        let Some(url) = test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("manifest-{}", Uuid::new_v4());
        let mut config = base_config();
        config.tags = vec!["coding:3".into(), "general".into()];
        config.boons = vec!["vision".into()];
        config.api_key = Some("sk-secret".into());
        config.route_bias = 1.25;
        config.max_retries = 2;

        let out = store
            .apply_model_manifest(&[write(&name, config)])
            .await
            .expect("create");
        fixtures.track_model(out[0].model.id);

        // Re-read from Postgres so we compare against what was really stored.
        let stored = store.get_model_by_name(&name).await.expect("read back");
        assert_eq!(
            stored.tags,
            vec!["coding:3".to_string(), "general".to_string()],
            "tag strength suffixes must survive storage"
        );
        assert_eq!(
            stored.api_key.as_deref(),
            Some("sk-secret"),
            "api_key must decrypt back to the plaintext we wrote"
        );

        let entry = obleth_config::model_to_manifest_entry(&stored);
        let resolved = resolve_model(&entry, Some(&stored), &[]).expect("resolve");
        assert!(
            resolved.changed_fields.is_empty(),
            "export -> import must be a no-op, changed: {:?}",
            resolved.changed_fields
        );
    }

    /// A manifest is all-or-nothing: a failure partway through must leave the
    /// earlier models in the file untouched.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_failed_write_rolls_the_whole_manifest_back() {
        let Some(url) = test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let good = format!("manifest-good-{}", Uuid::new_v4());
        let out = store
            .apply_model_manifest(&[write(&good, base_config())])
            .await
            .expect("seed");
        fixtures.track_model(out[0].model.id);

        // Second entry carries an endpoint weight of 0, which the
        // `model_endpoints_weight_check` constraint (`>= 1`) rejects. The
        // admin layer clamps this before it ever gets here; feeding it in
        // directly is how we exercise the store's own atomicity guarantee.
        let doomed = format!("manifest-doomed-{}", Uuid::new_v4());
        let mut edit = base_config();
        edit.description = "edited".into();

        let err = store
            .apply_model_manifest(&[
                ModelImportWrite {
                    model_name: good.clone(),
                    config: edit,
                    endpoints: None,
                },
                ModelImportWrite {
                    model_name: doomed.clone(),
                    config: base_config(),
                    endpoints: Some(vec![EndpointConfig {
                        name: "primary".into(),
                        api_base: "http://127.0.0.1:9000/v1".into(),
                        weight: 0,
                        ..Default::default()
                    }]),
                },
            ])
            .await
            .expect_err("an out-of-range endpoint weight must fail the apply");
        assert!(
            matches!(err, StoreError::Db(_)),
            "expected a database error, got: {err}"
        );

        // The edit to the first model must have rolled back with it.
        let after = store.get_model_by_name(&good).await.expect("read back");
        assert_eq!(
            after.description, "",
            "the first model's edit must roll back with the failed transaction"
        );
        assert!(
            store.get_model_by_name(&doomed).await.is_err(),
            "the doomed model must not exist"
        );
    }

    /// Endpoints match by name within the model: re-applying edits the same
    /// row rather than accumulating duplicates.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn endpoints_upsert_by_name_within_the_model() {
        let Some(url) = test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("manifest-{}", Uuid::new_v4());
        let endpoint = EndpointConfig {
            name: "primary".into(),
            api_base: "http://127.0.0.1:9000/v1".into(),
            priority: 10,
            ..Default::default()
        };
        let out = store
            .apply_model_manifest(&[ModelImportWrite {
                model_name: name.clone(),
                config: base_config(),
                endpoints: Some(vec![endpoint.clone()]),
            }])
            .await
            .expect("create");
        let id = out[0].model.id;
        fixtures.track_model(id);
        assert_eq!(out[0].endpoints.as_ref().unwrap().len(), 1);

        let mut edited = endpoint.clone();
        edited.priority = 50;
        store
            .apply_model_manifest(&[ModelImportWrite {
                model_name: name.clone(),
                config: base_config(),
                endpoints: Some(vec![edited]),
            }])
            .await
            .expect("update");

        let stored = store.list_model_endpoints(id).await.expect("list");
        assert_eq!(stored.len(), 1, "re-apply must not duplicate the endpoint");
        assert_eq!(stored[0].priority, 50);
    }

    /// `endpoints: None` means "leave them alone" — a models-only manifest
    /// must never disturb an existing endpoint rotation.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn omitting_endpoints_leaves_them_untouched() {
        let Some(url) = test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("manifest-{}", Uuid::new_v4());
        let out = store
            .apply_model_manifest(&[ModelImportWrite {
                model_name: name.clone(),
                config: base_config(),
                endpoints: Some(vec![EndpointConfig {
                    name: "primary".into(),
                    api_base: "http://127.0.0.1:9000/v1".into(),
                    ..Default::default()
                }]),
            }])
            .await
            .expect("create");
        let id = out[0].model.id;
        fixtures.track_model(id);

        store
            .apply_model_manifest(&[write(&name, base_config())])
            .await
            .expect("models-only re-apply");

        let stored = store.list_model_endpoints(id).await.expect("list");
        assert_eq!(
            stored.len(),
            1,
            "endpoints must survive a models-only apply"
        );
    }

    /// A manifest entry that only sets a name still produces a usable model.
    #[test]
    fn a_name_only_entry_resolves_to_the_create_defaults() {
        let entry = ManifestModel {
            model_name: "x".into(),
            ..Default::default()
        };
        let r = resolve_model(&entry, None, &[]).expect("resolve");
        assert_eq!(r.config, ModelConfig::default());
    }
}
