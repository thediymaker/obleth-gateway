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
