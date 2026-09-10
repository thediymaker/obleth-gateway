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

use crate::ssrf::SsrfPolicy;
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
    validate_backup_destinations(&state.ssrf, &body.data)?;

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

/// Every outbound destination a restore would register, checked against the
/// SSRF policy the create/update forms use. The first blocked entry fails the
/// whole restore so nothing is written.
fn validate_backup_destinations(policy: &SsrfPolicy, data: &BackupData) -> Result<()> {
    let models = data
        .models
        .iter()
        .map(|m| (format!("model '{}'", m.model_name), m.api_base.as_str()));
    let endpoints = data
        .model_endpoints
        .iter()
        .map(|e| (format!("endpoint '{}'", e.name), e.api_base.as_str()));
    let mcp = data
        .mcp_servers
        .iter()
        .map(|s| (format!("MCP server '{}'", s.name), s.upstream_url.as_str()));
    validate_destinations(policy, models.chain(endpoints).chain(mcp))
}

fn validate_destinations<'a>(
    policy: &SsrfPolicy,
    targets: impl Iterator<Item = (String, &'a str)>,
) -> Result<()> {
    for (label, url) in targets {
        // An empty URL is not a destination; older exports may carry one for
        // rows that were never dispatched to.
        if url.trim().is_empty() {
            continue;
        }
        policy
            .validate(url)
            .map_err(|e| AdminError::BadRequest(format!("backup {label}: {e}")))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(url: &str) -> Result<()> {
        validate_destinations(
            &SsrfPolicy::default(),
            std::iter::once(("model 'm'".to_string(), url)),
        )
    }

    #[test]
    fn metadata_destinations_fail_the_restore_with_the_entry_named() {
        let err = check("http://169.254.169.254/latest/meta-data/").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("model 'm'"), "{msg}");
        assert!(matches!(err, AdminError::BadRequest(_)));
    }

    #[test]
    fn local_upstreams_and_empty_urls_pass() {
        assert!(check("http://127.0.0.1:8000/v1").is_ok());
        assert!(check("http://10.1.2.3:8000/v1").is_ok());
        assert!(check("").is_ok());
        assert!(check("   ").is_ok());
    }

    #[test]
    fn stops_at_the_first_blocked_entry() {
        let targets = vec![
            ("model 'ok'".to_string(), "http://127.0.0.1:8000"),
            ("endpoint 'bad'".to_string(), "http://[fe80::1]/"),
            ("MCP server 'later'".to_string(), "http://169.254.169.254/"),
        ];
        let msg = validate_destinations(&SsrfPolicy::default(), targets.into_iter())
            .unwrap_err()
            .to_string();
        assert!(msg.contains("endpoint 'bad'"), "{msg}");
    }
}
