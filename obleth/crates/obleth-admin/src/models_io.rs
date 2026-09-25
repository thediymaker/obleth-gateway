//! Model manifest export and import.
//!
//! The config backup (`backup.rs`) answers "recreate this instance". This pair
//! answers the other question operators actually ask day to day: "let me read
//! and bulk-edit the model registry as a file". See
//! `obleth_config::manifest` for the format and why it differs from the backup.
//!
//! Import is a merge, never a sync: models listed in the file are created or
//! updated, and models absent from it are left alone. Nothing here deletes.

use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::Json;
use obleth_config::manifest::{
    endpoint_to_manifest_entry, model_to_manifest_entry, resolve_endpoint, resolve_model,
    EndpointConfig, ModelImportEntry, ModelImportReport, ModelManifest, MODEL_MANIFEST_FORMAT,
    MODEL_MANIFEST_VERSION,
};
use obleth_config::{ModelEndpoint, ModelRoute};
use obleth_store::ModelImportWrite;
use serde::Deserialize;
use std::collections::{HashMap, HashSet};

use crate::{audit_actor, sync_model_from, AdminError, AdminState, Result};

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(crate) struct ImportQuery {
    /// Validate and report what would change without writing anything.
    #[serde(default)]
    dry_run: bool,
}

#[utoipa::path(
    get, path = "/api/v1/models/export", tag = "models",
    responses((status = 200, body = ModelManifest))
)]
pub(crate) async fn export_models(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<ModelManifest>> {
    let mut models = state.store.list_models().await?;
    // Stable order so two exports of the same registry diff cleanly in git.
    models.sort_by(|a, b| a.model_name.cmp(&b.model_name));

    let mut entries = Vec::with_capacity(models.len());
    for model in &models {
        let mut entry = model_to_manifest_entry(model);
        let mut endpoints = state.store.list_model_endpoints(model.id).await?;
        endpoints.sort_by(|a, b| a.name.cmp(&b.name));
        // Omit the key entirely for models with no endpoints, so re-importing
        // reads as "leave endpoints alone" rather than "set them to empty".
        if !endpoints.is_empty() {
            entry.endpoints = Some(endpoints.iter().map(endpoint_to_manifest_entry).collect());
        }
        entries.push(entry);
    }

    let manifest = ModelManifest {
        format: MODEL_MANIFEST_FORMAT.to_string(),
        version: MODEL_MANIFEST_VERSION,
        exported_at: Some(chrono::Utc::now()),
        gateway_version: Some(env!("CARGO_PKG_VERSION").to_string()),
        models: entries,
    };

    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "export_models",
            "gateway",
            "model_manifest",
            serde_json::json!({ "models": manifest.models.len() }),
        )
        .await?;
    Ok(Json(manifest))
}

#[utoipa::path(
    post, path = "/api/v1/models/import", tag = "models",
    params(ImportQuery),
    request_body = ModelManifest,
    responses((status = 200, body = ModelImportReport), (status = 400))
)]
pub(crate) async fn import_models(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Query(query): Query<ImportQuery>,
    Json(body): Json<ModelManifest>,
) -> Result<Json<ModelImportReport>> {
    if body.format != MODEL_MANIFEST_FORMAT {
        return Err(AdminError::BadRequest(format!(
            "not an obleth model manifest (format {:?}, expected {:?})",
            body.format, MODEL_MANIFEST_FORMAT
        )));
    }
    if body.version != MODEL_MANIFEST_VERSION {
        return Err(AdminError::BadRequest(format!(
            "unsupported manifest version {} (this gateway supports version {})",
            body.version, MODEL_MANIFEST_VERSION
        )));
    }

    let existing: HashMap<String, ModelRoute> = state
        .store
        .list_models()
        .await?
        .into_iter()
        .map(|m| (m.model_name.clone(), m))
        .collect();
    let registered_servers: Vec<String> = state
        .store
        .list_mcp_servers()
        .await?
        .into_iter()
        .map(|s| s.name)
        .collect();

    // Collect every problem in one pass. Fixing a 40-model file one error per
    // round trip is exactly the experience this feature exists to avoid.
    let mut errors: Vec<String> = Vec::new();
    let mut writes: Vec<ModelImportWrite> = Vec::new();
    let mut entries: Vec<ModelImportEntry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for entry in &body.models {
        let name = entry.model_name.trim().to_string();
        if !name.is_empty() && !seen.insert(name.clone()) {
            errors.push(format!(
                "model '{name}': listed more than once in this manifest"
            ));
            continue;
        }
        let current = existing.get(&name);

        let resolved = match resolve_model(entry, current, &registered_servers) {
            Ok(r) => r,
            Err(e) => {
                errors.push(e.to_string());
                continue;
            }
        };

        // A restored row is dispatched to without further checks, so hold it to
        // the same destination policy the create/update forms enforce. A blank
        // api_base is allowed on a model (Slurm-provisioned models have none
        // until a replica is promoted); only a non-empty URL is validated.
        let api_base = resolved.config.api_base.trim();
        if !api_base.is_empty() {
            if let Err(e) = state.ssrf.validate(api_base).await {
                errors.push(format!("model '{name}': api_base {e}"));
                continue;
            }
        }
        let verify_api_base = resolved.config.verify_api_base.trim();
        if !verify_api_base.is_empty() {
            if let Err(e) = state.ssrf.validate(verify_api_base).await {
                errors.push(format!("model '{name}': verify_api_base {e}"));
                continue;
            }
        }

        // The field syntax was checked by `resolve_model`; whether this gateway
        // can read a `discovered` model's `kubernetes` source is checked here,
        // like the create and update forms do.
        let c = &resolved.config;
        if let Err(e) = obleth_config::capacity::validate_discovery_fields(
            &name,
            &c.upstream_model,
            &obleth_config::capacity::DiscoveryFields {
                source: c.capacity_source.clone(),
                namespace: c.capacity_namespace.clone(),
                selector: c.capacity_selector.clone(),
                per_replica_max_in_flight: c.per_replica_max_in_flight,
                headroom: c.capacity_headroom,
            },
            c.capacity_mode == obleth_config::DISCOVERED_CAPACITY_MODE,
            state.capacity_discovery.policy(),
        ) {
            errors.push(format!("model '{name}': {e}"));
            continue;
        }

        let mut changed_fields = resolved.changed_fields.clone();
        let warnings = resolved.warnings.clone();

        let endpoints = match &entry.endpoints {
            None => None,
            Some(list) => {
                let current_endpoints: Vec<ModelEndpoint> = match current {
                    Some(m) => state.store.list_model_endpoints(m.id).await?,
                    None => Vec::new(),
                };
                let by_name: HashMap<&str, &ModelEndpoint> = current_endpoints
                    .iter()
                    .map(|e| (e.name.as_str(), e))
                    .collect();

                let mut resolved_endpoints: Vec<EndpointConfig> = Vec::with_capacity(list.len());
                let mut seen_endpoints: HashSet<String> = HashSet::new();
                let mut failed = false;
                for e in list {
                    let ep_name = e.name.trim().to_string();
                    if !ep_name.is_empty() && !seen_endpoints.insert(ep_name.clone()) {
                        errors.push(format!(
                            "model '{name}': endpoint '{ep_name}' listed more than once"
                        ));
                        failed = true;
                        continue;
                    }
                    match resolve_endpoint(e, by_name.get(ep_name.as_str()).copied(), &name) {
                        Ok(r) => {
                            if let Err(err) = state.ssrf.validate(&r.config.api_base).await {
                                errors.push(format!(
                                    "model '{name}': endpoint '{ep_name}' api_base {err}"
                                ));
                                failed = true;
                                continue;
                            }
                            if r.is_new {
                                changed_fields.push(format!("endpoints.{ep_name} (new)"));
                            } else {
                                changed_fields.extend(r.changed_fields.clone());
                            }
                            resolved_endpoints.push(r.config);
                        }
                        Err(err) => {
                            errors.push(err.to_string());
                            failed = true;
                        }
                    }
                }
                if failed {
                    continue;
                }
                Some(resolved_endpoints)
            }
        };

        let action = if resolved.is_new {
            obleth_config::IMPORT_ACTION_CREATED
        } else if changed_fields.is_empty() {
            obleth_config::IMPORT_ACTION_UNCHANGED
        } else {
            obleth_config::IMPORT_ACTION_UPDATED
        };

        entries.push(ModelImportEntry {
            model_name: name.clone(),
            action: action.to_string(),
            changed_fields,
            warnings,
        });
        writes.push(ModelImportWrite {
            model_name: name,
            config: resolved.config,
            endpoints,
        });
    }

    // ---- alias uniqueness, judged against the post-import state ----
    //
    // A name a client can send has to identify exactly one route, so an alias
    // may not collide with any `model_name` or with another model's alias. The
    // check runs on the state the file *would* produce, not on today's rows: a
    // manifest may legitimately move an alias from one model to another in a
    // single file, which a check against the current rows alone would read as a
    // collision with the row it is moving off.
    //
    // Names held by models this file does not touch form the fixed backdrop.
    // Conflicts already present among those rows are not this import's fault
    // and are not reported against it.
    {
        let touched: HashSet<&str> = writes.iter().map(|w| w.model_name.as_str()).collect();
        let mut owner: HashMap<String, String> = HashMap::new();
        for m in existing.values() {
            owner.insert(m.model_name.clone(), m.model_name.clone());
            if !touched.contains(m.model_name.as_str()) {
                for alias in &m.aliases {
                    owner.insert(alias.clone(), m.model_name.clone());
                }
            }
        }
        for w in &writes {
            owner.insert(w.model_name.clone(), w.model_name.clone());
        }
        // Sorted so a file with several conflicts reports them in a stable
        // order rather than in hash order.
        let mut incoming: Vec<&ModelImportWrite> = writes.iter().collect();
        incoming.sort_by(|a, b| a.model_name.cmp(&b.model_name));
        for w in incoming {
            let model = &w.model_name;
            for alias in &w.config.aliases {
                if alias == obleth_config::routing::AUTO_MODEL_NAME {
                    errors.push(format!(
                        "model '{model}': alias '{alias}' is reserved for automatic model selection"
                    ));
                    continue;
                }
                match owner.get(alias) {
                    Some(other) if other == model => errors.push(format!(
                        "model '{model}': alias '{alias}' is the model's own name"
                    )),
                    Some(other) => errors.push(format!(
                        "model '{model}': alias '{alias}' is already taken by model '{other}'"
                    )),
                    None => {
                        owner.insert(alias.clone(), model.clone());
                    }
                }
            }
        }
    }

    if !errors.is_empty() {
        return Err(AdminError::BadRequest(format!(
            "manifest rejected, nothing was written ({} problem(s)):\n{}",
            errors.len(),
            errors.join("\n")
        )));
    }

    let mut report = ModelImportReport {
        dry_run: query.dry_run,
        created: entries
            .iter()
            .filter(|e| e.action == obleth_config::IMPORT_ACTION_CREATED)
            .count(),
        updated: entries
            .iter()
            .filter(|e| e.action == obleth_config::IMPORT_ACTION_UPDATED)
            .count(),
        unchanged: entries
            .iter()
            .filter(|e| e.action == obleth_config::IMPORT_ACTION_UNCHANGED)
            .count(),
        models: entries,
    };

    if query.dry_run {
        return Ok(Json(report));
    }

    // Skip rows the manifest would write identically — no pointless
    // `updated_at` churn, and no cache invalidation storm for a no-op import.
    let touched: Vec<ModelImportWrite> = writes
        .into_iter()
        .filter(|w| {
            report
                .models
                .iter()
                .find(|e| e.model_name == w.model_name)
                .is_some_and(|e| e.action != obleth_config::IMPORT_ACTION_UNCHANGED)
        })
        .collect();

    let outcomes = state.store.apply_model_manifest(&touched).await?;

    // Postgres is written; now push the new state to Redis so the data plane
    // picks it up without waiting for its periodic refresh.
    for outcome in &outcomes {
        // The pre-import row, so an alias this file dropped has its resolver
        // key evicted rather than left pointing at the model forever.
        sync_model_from(
            &state,
            &outcome.model,
            existing.get(&outcome.model.model_name),
        )
        .await?;
    }

    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "import_models",
            "gateway",
            "model_manifest",
            serde_json::json!({
                "created": report.created,
                "updated": report.updated,
                "unchanged": report.unchanged,
                "models": report
                    .models
                    .iter()
                    .filter(|e| e.action != obleth_config::IMPORT_ACTION_UNCHANGED)
                    .map(|e| &e.model_name)
                    .collect::<Vec<_>>(),
            }),
        )
        .await?;

    report.dry_run = false;
    Ok(Json(report))
}
