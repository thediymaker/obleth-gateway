//! Config backup export and restore endpoints.
//!
//! Export returns one JSON document with every configuration table — enough to
//! recreate an instance, minus usage history. Restore merges such a document
//! back in atomically (upsert by id, never delete), verifies the encryption
//! key up front via the backup's `key_check` sentinel, and re-syncs the Redis
//! hot caches afterwards so the data plane picks the restored config up
//! immediately.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::Json;
use obleth_config::{
    pepper_is_set, BackupData, BackupEncryption, ConfigBackup, RestoreReport, BACKUP_FORMAT,
    BACKUP_VERSION,
};
use obleth_store::CryptoError;

use crate::ssrf::{SsrfPolicy, VERIFY_TEMPLATE_PLACEHOLDERS};
use crate::{
    audit_actor, resync_all_keys, sync_mcp_server, sync_model, AdminError, AdminState, Result,
};

#[utoipa::path(
    get, path = "/api/v1/backup/export", tag = "backup",
    responses((status = 200, body = ConfigBackup))
)]
pub(crate) async fn export_backup(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<ConfigBackup>> {
    let data = state.store.export_backup_data().await?;
    let backup = ConfigBackup {
        format: BACKUP_FORMAT.to_string(),
        version: BACKUP_VERSION,
        exported_at: chrono::Utc::now(),
        gateway_version: env!("CARGO_PKG_VERSION").to_string(),
        encryption: BackupEncryption {
            cipher_enabled: state.store.cipher_enabled(),
            key_check: state.store.backup_key_check(),
            api_key_pepper_set: pepper_is_set(),
        },
        data,
    };
    // Audit counts only: the payload itself carries ciphertext and key hashes.
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "export_backup",
            "gateway",
            "backup",
            entity_counts(&backup.data),
        )
        .await?;
    Ok(Json(backup))
}

#[utoipa::path(
    post, path = "/api/v1/backup/restore", tag = "backup",
    request_body = ConfigBackup,
    responses((status = 200, body = RestoreReport))
)]
pub(crate) async fn restore_backup(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<ConfigBackup>,
) -> Result<Json<RestoreReport>> {
    if body.format != BACKUP_FORMAT {
        return Err(AdminError::BadRequest(format!(
            "not an obleth config backup (format {:?})",
            body.format
        )));
    }
    if body.version != BACKUP_VERSION {
        return Err(AdminError::BadRequest(format!(
            "unsupported backup version {} (this gateway supports version {})",
            body.version, BACKUP_VERSION
        )));
    }

    // Prove the encryption keys match before any database write.
    match &body.encryption.key_check {
        Some(key_check) => state
            .store
            .verify_backup_key_check(key_check)
            .map_err(|e| match e {
                CryptoError::Decrypt => AdminError::BadRequest(
                    "this backup was created with a different OBLETH_ENCRYPTION_KEY; \
                     restoring it requires the same key"
                        .into(),
                ),
                CryptoError::KeyMissing => AdminError::BadRequest(
                    "this backup contains encrypted secrets but OBLETH_ENCRYPTION_KEY \
                     is not set on this instance"
                        .into(),
                ),
                CryptoError::Malformed => {
                    AdminError::BadRequest("the backup's encryption key check is malformed".into())
                }
            })?,
        // No key check (exporter had the cipher disabled) — but reject if the
        // data still carries ciphertext we could never decrypt.
        None => {
            if !state.store.cipher_enabled() && contains_ciphertext(&body.data) {
                return Err(AdminError::BadRequest(
                    "this backup contains encrypted secrets but OBLETH_ENCRYPTION_KEY \
                     is not set on this instance"
                        .into(),
                ));
            }
        }
    }

    // Restored rows are dispatched to by the data plane and boons without any
    // further check, so hold the whole document to the same destination policy
    // the individual forms enforce — before the first write.
    validate_backup_destinations(&state.ssrf, &body.data).await?;

    let mut report = state.store.restore_backup_data(&body.data).await?;

    // A pepper mismatch can't be detected from the opaque hashes; surface it
    // as a warning so the operator knows restored keys may need rotation.
    if !body.data.api_keys.is_empty() && body.encryption.api_key_pepper_set != pepper_is_set() {
        report.warnings.push(
            "the backup and this instance differ on OBLETH_API_KEY_PEPPER; restored API keys \
             will not authenticate until the pepper matches or the keys are rotated"
                .to_string(),
        );
    }

    // Re-sync the Redis hot caches so the data plane sees the restored config
    // without waiting for its periodic refresh.
    resync_all_keys(&state).await?;
    for model in state.store.list_models().await? {
        sync_model(&state, &model).await?;
    }
    for server in state.store.list_mcp_servers().await? {
        sync_mcp_server(&state, &server).await?;
    }
    // Alert settings are pushed (not polled); reload them live. Auto-router,
    // boons and retention are picked up by the proxy's poll loop.
    if let Some(settings) = state.store.get_alert_settings().await? {
        state.alerts.update(settings);
    }

    let mut detail = entity_counts(&body.data);
    if let Some(obj) = detail.as_object_mut() {
        obj.insert(
            "source_exported_at".into(),
            serde_json::json!(body.exported_at),
        );
        obj.insert(
            "source_gateway_version".into(),
            serde_json::json!(body.gateway_version),
        );
        obj.insert("warnings".into(), serde_json::json!(report.warnings));
    }
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "restore_backup",
            "gateway",
            "backup",
            detail,
        )
        .await?;

    Ok(Json(report))
}

fn entity_counts(data: &BackupData) -> serde_json::Value {
    serde_json::json!({
        "fairshare_groups": data.fairshare_groups.len(),
        "tenants": data.tenants.len(),
        "api_keys": data.api_keys.len(),
        "models": data.models.len(),
        "model_endpoints": data.model_endpoints.len(),
        "mcp_servers": data.mcp_servers.len(),
        "app_settings": data.app_settings.len(),
    })
}

/// True when any secret column in the backup carries `enc:v1:` ciphertext.
fn contains_ciphertext(data: &BackupData) -> bool {
    let enc = |v: &Option<String>| v.as_deref().is_some_and(|s| s.starts_with("enc:v1:"));
    data.models.iter().any(|m| enc(&m.api_key))
        || data.model_endpoints.iter().any(|e| enc(&e.api_key))
        || data.mcp_servers.iter().any(|s| enc(&s.auth_header))
}

/// One outbound URL a restore would register.
struct Destination {
    label: String,
    url: String,
    /// A per-model URL template rather than a literal URL.
    template: bool,
}

/// Every outbound destination a restore would register — model, endpoint and
/// MCP upstreams plus the URLs inside `app_settings` — checked against the
/// SSRF policy the create/update forms use. The first blocked entry fails the
/// whole restore so nothing is written.
async fn validate_backup_destinations(policy: &SsrfPolicy, data: &BackupData) -> Result<()> {
    validate_destinations(policy, backup_destinations(data)).await
}

fn backup_destinations(data: &BackupData) -> Vec<Destination> {
    let literal = |label: String, url: &str| Destination {
        label,
        url: url.to_string(),
        template: false,
    };
    let mut out = Vec::new();
    for m in &data.models {
        out.push(literal(
            format!("model '{}' api_base", m.model_name),
            &m.api_base,
        ));
        out.push(literal(
            format!("model '{}' verify_api_base", m.model_name),
            &m.verify_api_base,
        ));
    }
    for e in &data.model_endpoints {
        out.push(literal(format!("endpoint '{}'", e.name), &e.api_base));
    }
    for s in &data.mcp_servers {
        out.push(literal(format!("MCP server '{}'", s.name), &s.upstream_url));
    }
    // Settings documents are opaque JSON here; pick out the fields the gateway
    // sends credentials to (webhook path, Slurm JWT, model upstream keys).
    for setting in &data.app_settings {
        // Same rule as the Slurm settings form: a disabled configuration may
        // carry a URL this host can't resolve; it is checked when enabled and
        // before every test ping.
        let slurm_enabled = setting.value.get("enabled").and_then(|v| v.as_bool()) == Some(true);
        let fields: &[(&str, bool)] = match setting.key.as_str() {
            "alerts" => &[("/slack_webhook_url", false)],
            "energy" => &[("/prometheus_url", false)],
            "slurm" if slurm_enabled => &[("/slurmrestd_url", false)],
            "boons" => &[("/speculation/verify_url_template", true)],
            _ => &[],
        };
        for (pointer, template) in fields {
            if let Some(url) = setting.value.pointer(pointer).and_then(|v| v.as_str()) {
                out.push(Destination {
                    label: format!("setting '{}' {}", setting.key, &pointer[1..]),
                    url: url.to_string(),
                    template: *template,
                });
            }
        }
    }
    out
}

async fn validate_destinations(policy: &SsrfPolicy, targets: Vec<Destination>) -> Result<()> {
    for d in targets {
        // An empty URL is not a destination; older exports may carry one for
        // rows that were never dispatched to.
        let url = d.url.trim();
        if url.is_empty() {
            continue;
        }
        let checked = if d.template {
            policy
                .validate_template(url, VERIFY_TEMPLATE_PLACEHOLDERS)
                .await
        } else {
            policy.validate(url).await
        };
        checked.map_err(|e| AdminError::BadRequest(format!("backup {}: {e}", d.label)))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dest(label: &str, url: &str) -> Destination {
        Destination {
            label: label.to_string(),
            url: url.to_string(),
            template: false,
        }
    }

    async fn check(url: &str) -> Result<()> {
        validate_destinations(&SsrfPolicy::default(), vec![dest("model 'm'", url)]).await
    }

    #[tokio::test]
    async fn metadata_destinations_fail_the_restore_with_the_entry_named() {
        let err = check("http://169.254.169.254/latest/meta-data/")
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("model 'm'"), "{msg}");
        assert!(matches!(err, AdminError::BadRequest(_)));
    }

    #[tokio::test]
    async fn local_upstreams_and_empty_urls_pass() {
        assert!(check("http://127.0.0.1:8000/v1").await.is_ok());
        assert!(check("http://10.1.2.3:8000/v1").await.is_ok());
        assert!(check("").await.is_ok());
        assert!(check("   ").await.is_ok());
    }

    #[tokio::test]
    async fn stops_at_the_first_blocked_entry() {
        let targets = vec![
            dest("model 'ok'", "http://127.0.0.1:8000"),
            dest("endpoint 'bad'", "http://[fe80::1]/"),
            dest("MCP server 'later'", "http://169.254.169.254/"),
        ];
        let msg = validate_destinations(&SsrfPolicy::default(), targets)
            .await
            .unwrap_err()
            .to_string();
        assert!(msg.contains("endpoint 'bad'"), "{msg}");
    }

    fn setting(key: &str, value: serde_json::Value) -> obleth_config::AppSettingBackup {
        obleth_config::AppSettingBackup {
            key: key.to_string(),
            value,
        }
    }

    #[tokio::test]
    async fn settings_urls_are_validated_and_named() {
        let cases = [
            (
                setting(
                    "alerts",
                    serde_json::json!({ "slack_webhook_url": "http://169.254.169.254/hook" }),
                ),
                "setting 'alerts' slack_webhook_url",
            ),
            (
                setting(
                    "energy",
                    serde_json::json!({ "prometheus_url": "http://[fe80::1]:9090" }),
                ),
                "setting 'energy' prometheus_url",
            ),
            (
                setting(
                    "slurm",
                    serde_json::json!({
                        "enabled": true, "slurmrestd_url": "http://100.100.100.200:6820"
                    }),
                ),
                "setting 'slurm' slurmrestd_url",
            ),
            (
                setting(
                    "boons",
                    serde_json::json!({
                        "speculation": { "verify_url_template": "http://169.254.169.254/{model}" }
                    }),
                ),
                "setting 'boons' speculation/verify_url_template",
            ),
            (
                setting(
                    "boons",
                    serde_json::json!({
                        "speculation": { "verify_url_template": "http://{model}@10.0.0.5:8000/v1" }
                    }),
                ),
                "setting 'boons' speculation/verify_url_template",
            ),
        ];
        for (s, expected) in cases {
            let data = BackupData {
                app_settings: vec![s],
                ..Default::default()
            };
            let msg = validate_backup_destinations(&SsrfPolicy::default(), &data)
                .await
                .unwrap_err()
                .to_string();
            assert!(msg.contains(expected), "{msg}");
        }
    }

    #[tokio::test]
    async fn disabled_slurm_host_templates_and_unrelated_settings_pass() {
        let data = BackupData {
            app_settings: vec![
                setting(
                    "boons",
                    serde_json::json!({
                        "speculation": {
                            "verify_url_template": "http://{upstream}.serving.svc.cluster.local:8000/{model}/v1"
                        }
                    }),
                ),
                setting("usage_retention", serde_json::json!({ "days": 30 })),
                // Disabled: even a blocked or unresolvable URL is not a
                // destination until the configuration is enabled.
                setting(
                    "slurm",
                    serde_json::json!({
                        "enabled": false, "slurmrestd_url": "http://169.254.169.254:6820"
                    }),
                ),
            ],
            ..Default::default()
        };
        assert!(validate_backup_destinations(&SsrfPolicy::default(), &data)
            .await
            .is_ok());
    }
}
