//! Config backup export and restore.
//!
//! Export reads every configuration table verbatim — secret columns keep their
//! stored ciphertext rather than being decrypted like the normal row mappers
//! do — so a backup file never contains more than the database itself. Restore
//! is a single-transaction merge: rows are upserted by primary key (insert
//! missing, update existing) and nothing is ever deleted.
//!
//! Usage history (audit log, model health checks, the ClickHouse ledger) is
//! deliberately out of scope: a backup recreates an instance's configuration,
//! not its past.

use crate::{cipher, Cipher, CryptoError, Result, Store, StoreError};
use obleth_config::{
    ApiKeyBackup, AppSettingBackup, BackupData, FairshareGroupBackup, McpServerBackup, ModelBackup,
    ModelEndpointBackup, RestoreCounts, RestoreReport, TenantBackup, WeeklyWindow,
};
use sqlx::postgres::PgRow;
use sqlx::Row;

/// Known plaintext encrypted into `BackupEncryption::key_check` at export time.
/// A restoring instance decrypts it to prove its `OBLETH_ENCRYPTION_KEY`
/// matches the exporter's before any database write happens.
pub const BACKUP_KEY_SENTINEL: &str = "obleth-backup-key-check";

/// Stored-ciphertext tag from `crypto.rs`, used to recognize already-encrypted
/// values when restoring.
const ENC_PREFIX: &str = "enc:v1:";

impl Store {
    /// True when `OBLETH_ENCRYPTION_KEY` is configured.
    pub fn cipher_enabled(&self) -> bool {
        matches!(cipher(), Cipher::Enabled(_))
    }

    /// Sentinel ciphertext for a backup's `key_check`, or `None` when the
    /// cipher is disabled (secrets are stored as plaintext).
    pub fn backup_key_check(&self) -> Option<String> {
        match cipher() {
            Cipher::Disabled => None,
            c @ Cipher::Enabled(_) => Some(c.encrypt(BACKUP_KEY_SENTINEL)),
        }
    }

    /// Verify a backup's `key_check` against the local cipher.
    ///
    /// `CryptoError::Decrypt` means the backup was made with a different key;
    /// `CryptoError::KeyMissing` means the backup is encrypted but this
    /// instance has no `OBLETH_ENCRYPTION_KEY`.
    pub fn verify_backup_key_check(&self, key_check: &str) -> std::result::Result<(), CryptoError> {
        let plain = cipher().decrypt(key_check)?;
        if plain != BACKUP_KEY_SENTINEL {
            return Err(CryptoError::Decrypt);
        }
        Ok(())
    }

    /// Read every configuration table for export. Secret columns (`api_key`,
    /// `auth_header`) are returned exactly as stored — ciphertext on encrypted
    /// instances — and `api_keys.key_hash` is included so client keys keep
    /// authenticating after a restore.
    pub async fn export_backup_data(&self) -> Result<BackupData> {
        let fairshare_groups =
            sqlx::query("select name, weight, created_at from fairshare_groups order by name")
                .fetch_all(&self.pool)
                .await?
                .iter()
                .map(|row| {
                    Ok(FairshareGroupBackup {
                        name: row.try_get("name")?,
                        weight: row.try_get("weight")?,
                        created_at: row.try_get("created_at")?,
                    })
                })
                .collect::<Result<Vec<_>>>()?;

        let tenants = sqlx::query(
            "select id, name, fairshare_group, weight, tokens_per_minute, max_in_flight,
                    description, organization, contact_email, status, timezone, active_from,
                    active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period,
                    budget_started_at, allowed_models, created_at
             from tenants order by created_at",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(tenant_backup_from_row)
        .collect::<Result<Vec<_>>>()?;

        let api_keys = sqlx::query(
            "select id, tenant_id, name, description, key_prefix, key_hash,
                    budget_tokens, budget_cost_usd, budget_period, budget_started_at,
                    disabled, created_at,
                    kind, identity_issuer, identity_subject, identity_claims,
                    weight, max_in_flight
             from api_keys order by created_at",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|row| {
            let claims: Option<sqlx::types::Json<serde_json::Value>> =
                row.try_get("identity_claims").unwrap_or(None);
            Ok(ApiKeyBackup {
                id: row.try_get("id")?,
                tenant_id: row.try_get("tenant_id")?,
                name: row.try_get("name")?,
                description: row.try_get("description")?,
                key_prefix: row.try_get("key_prefix")?,
                key_hash: row.try_get("key_hash")?,
                kind: row
                    .try_get("kind")
                    .unwrap_or_else(|_| obleth_config::API_KEY_KIND_SECRET.to_string()),
                identity_issuer: row.try_get("identity_issuer").unwrap_or(None),
                identity_subject: row.try_get("identity_subject").unwrap_or(None),
                identity_claims: claims.map(|j| j.0),
                budget_tokens: row.try_get("budget_tokens")?,
                budget_cost_usd: row.try_get("budget_cost_usd")?,
                budget_period: row.try_get("budget_period")?,
                budget_started_at: row.try_get("budget_started_at")?,
                weight: row.try_get("weight").unwrap_or(100),
                max_in_flight: row.try_get("max_in_flight").unwrap_or(None),
                disabled: row.try_get("disabled")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

        let models = sqlx::query(
            "select id, model_name, aliases, description, upstream_model, api_base, api_key,
                    upstream_headers, model_type, quantization,
                    input_cost_per_token, output_cost_per_token, cost_per_image,
                    cost_per_audio_second, cost_per_character, cost_per_video, context_window,
                    admission_weight, max_in_flight, capacity_mode, capacity_tuned_at,
                    capacity_source, capacity_namespace, capacity_service,
                    per_replica_max_in_flight, capacity_headroom, supports_function_calling,
                    supports_system_messages, supports_response_schema, supports_tool_choice,
                    supports_vision, enabled, cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                    route_bias, auto_eligible, draft_model, verify_api_base, verify_upstream_model, request_timeout_secs, max_retries, retry_backoff_ms,
                    endpoint_selection_mode, debug_diagnostics, energy_slots_per_node,
                    health_checks_enabled, health_alerts_enabled, health_check_interval_secs,
                    health_failure_threshold, health_maintenance_until, health_maintenance_note,
                    created_at
             from models order by created_at",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(model_backup_from_row)
        .collect::<Result<Vec<_>>>()?;

        let model_endpoints = sqlx::query(
            "select id, model_id, name, api_base, api_key, priority, weight, enabled,
                    max_in_flight, created_at
             from model_endpoints order by created_at",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|row| {
            Ok(ModelEndpointBackup {
                id: row.try_get("id")?,
                model_id: row.try_get("model_id")?,
                name: row.try_get("name")?,
                api_base: row.try_get("api_base")?,
                api_key: row.try_get("api_key")?,
                priority: row.try_get("priority")?,
                weight: row.try_get("weight")?,
                enabled: row.try_get("enabled")?,
                max_in_flight: row.try_get("max_in_flight").unwrap_or(None),
                created_at: row.try_get("created_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

        let mcp_servers = sqlx::query(
            "select id, name, upstream_url, auth_header, enabled, created_at
             from mcp_servers order by created_at",
        )
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|row| {
            Ok(McpServerBackup {
                id: row.try_get("id")?,
                name: row.try_get("name")?,
                upstream_url: row.try_get("upstream_url")?,
                auth_header: row.try_get("auth_header")?,
                enabled: row.try_get("enabled")?,
                created_at: row.try_get("created_at")?,
            })
        })
        .collect::<Result<Vec<_>>>()?;

        let app_settings = sqlx::query("select key, value from app_settings order by key")
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|row| {
                Ok(AppSettingBackup {
                    key: row.try_get("key")?,
                    value: row.try_get("value")?,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(BackupData {
            fairshare_groups,
            tenants,
            api_keys,
            models,
            model_endpoints,
            mcp_servers,
            app_settings,
        })
    }

    /// Merge a backup into the database: one transaction, upsert by primary
    /// key in foreign-key order, never delete. Plaintext secrets from a
    /// cipher-disabled exporter are encrypted with the local key on write;
    /// already-encrypted values are written verbatim (the caller has verified
    /// the key matches via [`Store::verify_backup_key_check`]).
    pub async fn restore_backup_data(&self, data: &BackupData) -> Result<RestoreReport> {
        let mut tx = self.pool.begin().await?;
        let mut report = RestoreReport::default();

        for g in &data.fairshare_groups {
            let row = sqlx::query(
                "insert into fairshare_groups (name, weight, created_at)
                 values ($1, $2, $3)
                 on conflict (name) do update set weight = excluded.weight, updated_at = now()
                 returning (xmax = 0) as inserted",
            )
            .bind(&g.name)
            .bind(g.weight)
            .bind(g.created_at)
            .fetch_one(&mut *tx)
            .await
            .map_err(restore_db_error)?;
            tally(&mut report.fairshare_groups, &row)?;
        }

        for t in &data.tenants {
            let windows = t
                .weekly_windows
                .clone()
                .filter(|w: &Vec<WeeklyWindow>| !w.is_empty())
                .map(sqlx::types::Json);
            let allowed = t
                .allowed_models
                .clone()
                .filter(|m| !m.is_empty())
                .map(sqlx::types::Json);
            let row = sqlx::query(
                "insert into tenants (id, name, fairshare_group, weight, tokens_per_minute,
                        max_in_flight, description, organization, contact_email, status, timezone,
                        active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd,
                        budget_period, budget_started_at, allowed_models, created_at)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                        $17, $18, $19, $20)
                 on conflict (id) do update set
                        name = excluded.name,
                        fairshare_group = excluded.fairshare_group,
                        weight = excluded.weight,
                        tokens_per_minute = excluded.tokens_per_minute,
                        max_in_flight = excluded.max_in_flight,
                        description = excluded.description,
                        organization = excluded.organization,
                        contact_email = excluded.contact_email,
                        status = excluded.status,
                        timezone = excluded.timezone,
                        active_from = excluded.active_from,
                        active_until = excluded.active_until,
                        weekly_windows = excluded.weekly_windows,
                        budget_tokens = excluded.budget_tokens,
                        budget_cost_usd = excluded.budget_cost_usd,
                        budget_period = excluded.budget_period,
                        budget_started_at = excluded.budget_started_at,
                        allowed_models = excluded.allowed_models,
                        updated_at = now()
                 returning (xmax = 0) as inserted",
            )
            .bind(t.id)
            .bind(&t.name)
            .bind(&t.fairshare_group)
            .bind(t.weight)
            .bind(t.tokens_per_minute)
            .bind(t.max_in_flight)
            .bind(&t.description)
            .bind(&t.organization)
            .bind(&t.contact_email)
            .bind(&t.status)
            .bind(&t.timezone)
            .bind(t.active_from)
            .bind(t.active_until)
            .bind(windows)
            .bind(t.budget_tokens)
            .bind(t.budget_cost_usd)
            .bind(&t.budget_period)
            .bind(t.budget_started_at)
            .bind(allowed)
            .bind(t.created_at)
            .fetch_one(&mut *tx)
            .await
            .map_err(restore_db_error)?;
            tally(&mut report.tenants, &row)?;
        }

        for k in &data.api_keys {
            // A restore upserts by id, but a row that collides on `key_hash`
            // (unique) or on the partial identity index `(identity_issuer,
            // identity_subject) where kind = 'identity'` under a *different*
            // id is not a primary-key conflict -- it's a leftover from
            // before the backup was taken (e.g. the identity was deleted and
            // re-provisioned locally, minting a fresh id for the same
            // issuer/subject). Left in place, the insert below would abort
            // the whole restore on a unique-constraint violation instead of
            // restoring the backup's row. NULL identity columns on a secret
            // key never match the identity half of this predicate, so this
            // is a no-op for ordinary keys.
            sqlx::query(
                "delete from api_keys
                 where (key_hash = $2
                        or (kind = 'identity' and identity_issuer = $3 and identity_subject = $4))
                   and id <> $1",
            )
            .bind(k.id)
            .bind(&k.key_hash)
            .bind(&k.identity_issuer)
            .bind(&k.identity_subject)
            .execute(&mut *tx)
            .await
            .map_err(restore_db_error)?;

            let row = sqlx::query(
                "insert into api_keys (id, tenant_id, name, description, key_prefix, key_hash,
                        budget_tokens, budget_cost_usd, budget_period, budget_started_at,
                        disabled, created_at, kind, identity_issuer, identity_subject,
                        identity_claims, weight, max_in_flight)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                         $17, $18)
                 on conflict (id) do update set
                        tenant_id = excluded.tenant_id,
                        name = excluded.name,
                        description = excluded.description,
                        key_prefix = excluded.key_prefix,
                        key_hash = excluded.key_hash,
                        budget_tokens = excluded.budget_tokens,
                        budget_cost_usd = excluded.budget_cost_usd,
                        budget_period = excluded.budget_period,
                        budget_started_at = excluded.budget_started_at,
                        disabled = excluded.disabled,
                        kind = excluded.kind,
                        identity_issuer = excluded.identity_issuer,
                        identity_subject = excluded.identity_subject,
                        identity_claims = excluded.identity_claims,
                        weight = excluded.weight,
                        max_in_flight = excluded.max_in_flight,
                        updated_at = now()
                 returning (xmax = 0) as inserted",
            )
            .bind(k.id)
            .bind(k.tenant_id)
            .bind(&k.name)
            .bind(&k.description)
            .bind(&k.key_prefix)
            .bind(&k.key_hash)
            .bind(k.budget_tokens)
            .bind(k.budget_cost_usd)
            .bind(&k.budget_period)
            .bind(k.budget_started_at)
            .bind(k.disabled)
            .bind(k.created_at)
            .bind(&k.kind)
            .bind(&k.identity_issuer)
            .bind(&k.identity_subject)
            .bind(k.identity_claims.clone().map(sqlx::types::Json))
            .bind(k.weight)
            .bind(k.max_in_flight)
            .fetch_one(&mut *tx)
            .await
            .map_err(restore_db_error)?;
            tally(&mut report.api_keys, &row)?;
        }

        for m in &data.models {
            let row = sqlx::query(
                "insert into models (id, model_name, description, upstream_model, api_base,
                        api_key, model_type, input_cost_per_token, output_cost_per_token,
                        cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                        admission_weight, max_in_flight, capacity_mode, capacity_tuned_at,
                        supports_function_calling, supports_system_messages,
                        supports_response_schema, supports_tool_choice, supports_vision, enabled,
                        cache_enabled, cache_ttl_secs, tags, boons, tool_servers, route_bias,
                        auto_eligible, draft_model, verify_api_base, verify_upstream_model,
                        request_timeout_secs,
                        max_retries, retry_backoff_ms, endpoint_selection_mode,
                        health_checks_enabled, health_alerts_enabled, health_check_interval_secs,
                        health_failure_threshold, health_maintenance_until,
                        health_maintenance_note, created_at,
                        debug_diagnostics, energy_slots_per_node, aliases, quantization,
                        upstream_headers, cost_per_video, capacity_namespace, capacity_service,
                        per_replica_max_in_flight, capacity_source, capacity_headroom)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16,
                        $17, $18, $19, $20, $21, $22, $23, $24, $25, $26, $27, $28, $29, $30, $31,
                        $32, $33, $34, $35, $36, $37, $38, $39, $40, $41, $42, $43, $44, $45, $46,
                        $47, $48, $49, $50, $51, $52, $53, $54, $55)
                 on conflict (id) do update set
                        model_name = excluded.model_name,
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
                        capacity_tuned_at = excluded.capacity_tuned_at,
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
                        route_bias = excluded.route_bias,
                        auto_eligible = excluded.auto_eligible,
                        draft_model = excluded.draft_model,
                        verify_api_base = excluded.verify_api_base,
                        verify_upstream_model = excluded.verify_upstream_model,
                        request_timeout_secs = excluded.request_timeout_secs,
                        max_retries = excluded.max_retries,
                        retry_backoff_ms = excluded.retry_backoff_ms,
                        endpoint_selection_mode = excluded.endpoint_selection_mode,
                        health_checks_enabled = excluded.health_checks_enabled,
                        health_alerts_enabled = excluded.health_alerts_enabled,
                        health_check_interval_secs = excluded.health_check_interval_secs,
                        health_failure_threshold = excluded.health_failure_threshold,
                        health_maintenance_until = excluded.health_maintenance_until,
                        health_maintenance_note = excluded.health_maintenance_note,
                        debug_diagnostics = excluded.debug_diagnostics,
                        energy_slots_per_node = excluded.energy_slots_per_node,
                        aliases = excluded.aliases,
                        quantization = excluded.quantization,
                        upstream_headers = excluded.upstream_headers,
                        cost_per_video = excluded.cost_per_video,
                        capacity_namespace = excluded.capacity_namespace,
                        capacity_service = excluded.capacity_service,
                        per_replica_max_in_flight = excluded.per_replica_max_in_flight,
                        capacity_source = excluded.capacity_source,
                        capacity_headroom = excluded.capacity_headroom,
                        updated_at = now()
                 returning (xmax = 0) as inserted",
            )
            .bind(m.id)
            .bind(&m.model_name)
            .bind(&m.description)
            .bind(&m.upstream_model)
            .bind(&m.api_base)
            .bind(normalize_secret(m.api_key.as_deref()))
            .bind(&m.model_type)
            .bind(m.input_cost_per_token)
            .bind(m.output_cost_per_token)
            .bind(m.cost_per_image)
            .bind(m.cost_per_audio_second)
            .bind(m.cost_per_character)
            .bind(m.context_window)
            .bind(m.admission_weight)
            .bind(m.max_in_flight)
            .bind(&m.capacity_mode)
            .bind(m.capacity_tuned_at)
            .bind(m.supports_function_calling)
            .bind(m.supports_system_messages)
            .bind(m.supports_response_schema)
            .bind(m.supports_tool_choice)
            .bind(m.supports_vision)
            .bind(m.enabled)
            .bind(m.cache_enabled)
            .bind(m.cache_ttl_secs)
            .bind(sqlx::types::Json(&m.tags))
            .bind(sqlx::types::Json(&m.boons))
            .bind(sqlx::types::Json(&m.tool_servers))
            .bind(m.route_bias)
            .bind(m.auto_eligible)
            .bind(m.draft_model.trim())
            .bind(m.verify_api_base.trim())
            .bind(m.verify_upstream_model.trim())
            .bind(m.request_timeout_secs)
            .bind(m.max_retries)
            .bind(m.retry_backoff_ms)
            .bind(&m.endpoint_selection_mode)
            .bind(m.health_checks_enabled)
            .bind(m.health_alerts_enabled)
            .bind(m.health_check_interval_secs)
            .bind(m.health_failure_threshold)
            .bind(m.health_maintenance_until)
            .bind(&m.health_maintenance_note)
            .bind(m.created_at)
            .bind(m.debug_diagnostics)
            .bind(m.energy_slots_per_node)
            .bind(sqlx::types::Json(obleth_config::normalize_aliases(
                &m.aliases,
            )))
            .bind(obleth_config::normalize_quantization(&m.quantization))
            .bind(sqlx::types::Json(
                m.upstream_headers
                    .iter()
                    .map(|(name, value)| {
                        (
                            name.clone(),
                            normalize_secret(Some(value)).unwrap_or_default(),
                        )
                    })
                    .collect::<std::collections::BTreeMap<_, _>>(),
            ))
            .bind(m.cost_per_video.max(0.0))
            .bind(obleth_config::capacity::normalize_optional_text(
                m.capacity_namespace.as_deref(),
            ))
            .bind(obleth_config::capacity::normalize_optional_text(
                m.capacity_service.as_deref(),
            ))
            .bind(m.per_replica_max_in_flight.filter(|n| *n >= 1))
            .bind(
                if obleth_config::is_valid_capacity_source(&m.capacity_source) {
                    m.capacity_source.as_str()
                } else {
                    obleth_config::DEFAULT_CAPACITY_SOURCE
                },
            )
            .bind(
                if obleth_config::capacity::validate_capacity_headroom(m.capacity_headroom).is_ok()
                {
                    m.capacity_headroom
                } else {
                    1.0
                },
            )
            .fetch_one(&mut *tx)
            .await
            .map_err(restore_db_error)?;
            tally(&mut report.models, &row)?;
        }

        for e in &data.model_endpoints {
            let row = sqlx::query(
                "insert into model_endpoints (id, model_id, name, api_base, api_key, priority,
                        weight, enabled, created_at, max_in_flight)
                 values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
                 on conflict (id) do update set
                        model_id = excluded.model_id,
                        name = excluded.name,
                        api_base = excluded.api_base,
                        api_key = excluded.api_key,
                        priority = excluded.priority,
                        weight = excluded.weight,
                        enabled = excluded.enabled,
                        max_in_flight = excluded.max_in_flight,
                        updated_at = now()
                 returning (xmax = 0) as inserted",
            )
            .bind(e.id)
            .bind(e.model_id)
            .bind(&e.name)
            .bind(&e.api_base)
            .bind(normalize_secret(e.api_key.as_deref()))
            .bind(e.priority)
            .bind(e.weight)
            .bind(e.enabled)
            .bind(e.created_at)
            .bind(e.max_in_flight.filter(|n| *n >= 1))
            .fetch_one(&mut *tx)
            .await
            .map_err(restore_db_error)?;
            tally(&mut report.model_endpoints, &row)?;
        }

        for s in &data.mcp_servers {
            let row = sqlx::query(
                "insert into mcp_servers (id, name, upstream_url, auth_header, enabled,
                        created_at)
                 values ($1, $2, $3, $4, $5, $6)
                 on conflict (id) do update set
                        name = excluded.name,
                        upstream_url = excluded.upstream_url,
                        auth_header = excluded.auth_header,
                        enabled = excluded.enabled,
                        updated_at = now()
                 returning (xmax = 0) as inserted",
            )
            .bind(s.id)
            .bind(&s.name)
            .bind(&s.upstream_url)
            .bind(normalize_secret(s.auth_header.as_deref()))
            .bind(s.enabled)
            .bind(s.created_at)
            .fetch_one(&mut *tx)
            .await
            .map_err(restore_db_error)?;
            tally(&mut report.mcp_servers, &row)?;
        }

        for s in &data.app_settings {
            let row = sqlx::query(
                "insert into app_settings (key, value, updated_at)
                 values ($1, $2, now())
                 on conflict (key) do update set value = excluded.value, updated_at = now()
                 returning (xmax = 0) as inserted",
            )
            .bind(&s.key)
            .bind(sqlx::types::Json(&s.value))
            .fetch_one(&mut *tx)
            .await
            .map_err(restore_db_error)?;
            tally(&mut report.app_settings, &row)?;
        }

        tx.commit().await?;
        Ok(report)
    }
}

/// Bump the insert/update tally from an upsert's `(xmax = 0) as inserted` flag.
fn tally(counts: &mut RestoreCounts, row: &PgRow) -> Result<()> {
    if row.try_get::<bool, _>("inserted")? {
        counts.inserted += 1;
    } else {
        counts.updated += 1;
    }
    Ok(())
}

/// Re-encrypt secrets coming from a cipher-disabled exporter. Already-tagged
/// ciphertext is written verbatim; with the local cipher disabled this is a
/// no-op passthrough.
fn normalize_secret(value: Option<&str>) -> Option<String> {
    value.map(|v| {
        if v.starts_with(ENC_PREFIX) {
            v.to_string()
        } else {
            cipher().encrypt(v)
        }
    })
}

/// Translate restore-time constraint violations into operator-readable
/// conflicts (e.g. the backup carries a tenant name that an existing,
/// different-id tenant already uses).
fn restore_db_error(e: sqlx::Error) -> StoreError {
    if let sqlx::Error::Database(db) = &e {
        let constraint = db.constraint().unwrap_or_default();
        if db.is_unique_violation() {
            let what = match constraint {
                "tenants_name_key" => "a tenant with the same name but a different id",
                "models_model_name_key" => "a model with the same name but a different id",
                "api_keys_key_hash_key" => "an API key with the same hash but a different id",
                "mcp_servers_name_key" => "an MCP server with the same name but a different id",
                "model_endpoints_model_id_name_key" => {
                    "a model endpoint with the same name but a different id"
                }
                _ => {
                    return StoreError::Conflict(format!(
                        "restore conflicts with existing data (unique constraint {constraint})"
                    ))
                }
            };
            return StoreError::Conflict(format!(
                "restore aborted: {what} already exists on this instance"
            ));
        }
        if db.is_foreign_key_violation() {
            return StoreError::Conflict(format!(
                "restore aborted: the backup references missing data (foreign key {constraint})"
            ));
        }
    }
    StoreError::Db(e)
}

fn tenant_backup_from_row(row: &PgRow) -> Result<TenantBackup> {
    Ok(TenantBackup {
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
        weekly_windows: crate::weekly_windows_from_row(row)?,
        budget_tokens: row.try_get("budget_tokens")?,
        budget_cost_usd: row.try_get("budget_cost_usd")?,
        budget_period: row.try_get("budget_period")?,
        budget_started_at: row.try_get("budget_started_at")?,
        allowed_models: crate::allowed_models_from_row(row)?,
        created_at: row.try_get("created_at")?,
    })
}

fn model_backup_from_row(row: &PgRow) -> Result<ModelBackup> {
    Ok(ModelBackup {
        id: row.try_get("id")?,
        model_name: row.try_get("model_name")?,
        aliases: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("aliases")
            .map(|j| j.0)
            .unwrap_or_default(),
        description: row.try_get("description")?,
        upstream_model: row.try_get("upstream_model")?,
        api_base: row.try_get("api_base")?,
        api_key: row.try_get("api_key")?,
        upstream_headers: row
            .try_get::<sqlx::types::Json<obleth_config::UpstreamHeaders>, _>("upstream_headers")
            .map(|j| j.0)
            .unwrap_or_default(),
        model_type: row.try_get("model_type")?,
        quantization: row
            .try_get::<String, _>("quantization")
            .unwrap_or_else(|_| obleth_config::DEFAULT_QUANTIZATION.to_string()),
        input_cost_per_token: row.try_get("input_cost_per_token")?,
        output_cost_per_token: row.try_get("output_cost_per_token")?,
        cost_per_image: row.try_get("cost_per_image")?,
        cost_per_audio_second: row.try_get("cost_per_audio_second")?,
        cost_per_character: row.try_get("cost_per_character")?,
        cost_per_video: row.try_get("cost_per_video").unwrap_or(0.0),
        context_window: row.try_get("context_window")?,
        admission_weight: row.try_get("admission_weight")?,
        max_in_flight: row.try_get("max_in_flight")?,
        capacity_mode: row.try_get("capacity_mode")?,
        capacity_tuned_at: row.try_get("capacity_tuned_at")?,
        capacity_source: row
            .try_get::<String, _>("capacity_source")
            .unwrap_or_else(|_| obleth_config::DEFAULT_CAPACITY_SOURCE.to_string()),
        capacity_namespace: row.try_get("capacity_namespace").unwrap_or(None),
        capacity_service: row.try_get("capacity_service").unwrap_or(None),
        per_replica_max_in_flight: row.try_get("per_replica_max_in_flight").unwrap_or(None),
        capacity_headroom: row.try_get("capacity_headroom").unwrap_or(1.0),
        supports_function_calling: row.try_get("supports_function_calling")?,
        supports_system_messages: row.try_get("supports_system_messages")?,
        supports_response_schema: row.try_get("supports_response_schema")?,
        supports_tool_choice: row.try_get("supports_tool_choice")?,
        supports_vision: row.try_get("supports_vision")?,
        enabled: row.try_get("enabled")?,
        cache_enabled: row.try_get("cache_enabled")?,
        cache_ttl_secs: row.try_get("cache_ttl_secs")?,
        tags: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("tags")
            .map(|j| j.0)
            .unwrap_or_default(),
        boons: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("boons")
            .map(|j| j.0)
            .unwrap_or_default(),
        tool_servers: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("tool_servers")
            .map(|j| j.0)
            .unwrap_or_default(),
        route_bias: row.try_get("route_bias")?,
        auto_eligible: row.try_get("auto_eligible")?,
        draft_model: row.try_get("draft_model").unwrap_or_default(),
        verify_api_base: row.try_get("verify_api_base").unwrap_or_default(),
        verify_upstream_model: row.try_get("verify_upstream_model").unwrap_or_default(),
        request_timeout_secs: row.try_get("request_timeout_secs")?,
        max_retries: row.try_get("max_retries")?,
        retry_backoff_ms: row.try_get("retry_backoff_ms")?,
        endpoint_selection_mode: row.try_get("endpoint_selection_mode")?,
        debug_diagnostics: row.try_get("debug_diagnostics")?,
        energy_slots_per_node: row.try_get("energy_slots_per_node")?,
        health_checks_enabled: row.try_get("health_checks_enabled")?,
        health_alerts_enabled: row.try_get("health_alerts_enabled")?,
        health_check_interval_secs: row.try_get("health_check_interval_secs")?,
        health_failure_threshold: row.try_get("health_failure_threshold")?,
        health_maintenance_until: row.try_get("health_maintenance_until")?,
        health_maintenance_note: row.try_get("health_maintenance_note")?,
        created_at: row.try_get("created_at")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes_gcm::aead::KeyInit;
    use aes_gcm::{Aes256Gcm, Key};

    fn enabled_cipher(byte: u8) -> Cipher {
        Cipher::Enabled(Box::new(Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(
            &[byte; 32],
        ))))
    }

    /// The sentinel round-trips under the same key, fails closed under a
    /// different key, and reports a missing key when the cipher is disabled.
    /// (Exercises the same logic as `verify_backup_key_check`, which needs the
    /// process-global cipher; the env-independent path is tested here.)
    #[test]
    fn key_check_sentinel_semantics() {
        let exporter = enabled_cipher(7);
        let key_check = exporter.encrypt(BACKUP_KEY_SENTINEL);

        assert_eq!(exporter.decrypt(&key_check).unwrap(), BACKUP_KEY_SENTINEL);
        assert!(matches!(
            enabled_cipher(9).decrypt(&key_check),
            Err(CryptoError::Decrypt)
        ));
        assert!(matches!(
            Cipher::Disabled.decrypt(&key_check),
            Err(CryptoError::KeyMissing)
        ));
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` points at a
    /// throwaway Postgres. Skips silently otherwise so unit runs stay hermetic.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn backup_export_restore_roundtrip() {
        use crate::test_support::{serial, test_db_url, FixtureGuard};

        let Some(url) = test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        // `export_backup_data`/`restore_backup_data` operate on every row in
        // the instance, not just this test's own fixtures, so this must not
        // run concurrently with any other DB-backed test in the crate --
        // shared with every `serial()` caller in `lib.rs`'s test module too.
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let name = format!("t-{}", uuid::Uuid::new_v4());
        let tenant = store
            .create_tenant(&name, 250, 1000, Some(4), None)
            .await
            .expect("create tenant");
        fixtures.track_tenant(tenant.id);
        let (key, secret) = store
            .create_api_key(
                tenant.id,
                "backup-test",
                "",
                None,
                None,
                None,
                None,
                100,
                None,
            )
            .await
            .expect("create key");
        let hash = obleth_config::hash_api_key(&secret);

        // A model with a non-neutral routing bias and a levelled intent tag.
        // Both are `auto`-router configuration an operator set deliberately, so
        // both must survive a backup cycle rather than silently reverting to
        // the neutral 1.0.
        let model = store
            .create_model(
                &format!("m-{}", uuid::Uuid::new_v4()),
                "backup test model",
                "upstream-model",
                "http://127.0.0.1:8081",
                None,
                "chat",
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                8192,
                100,
                None,
                false,
                true,
                false,
                false,
                false,
                &["coding:3".to_string()],
                &[],
                &[],
                0,
                2.5,
                // Excluded from auto, so the export/restore assertions below
                // distinguish a carried value from the eligible default.
                false,
                "",
                "",
                "",
                &[],
                "",
                &Default::default(),
                0.0,
                "discovered",
                &obleth_config::capacity::DiscoveryFields {
                    source: "kubernetes".into(),
                    namespace: Some("inference".into()),
                    service: Some("upstream-model".into()),
                    per_replica_max_in_flight: Some(8),
                    headroom: 1.5,
                },
            )
            .await
            .expect("create model");
        fixtures.track_model(model.id);
        assert_eq!(model.route_bias, 2.5);
        let endpoint = store
            .create_model_endpoint(
                model.id,
                "a",
                "http://127.0.0.1:8082",
                None,
                0,
                100,
                true,
                Some(12),
            )
            .await
            .expect("create endpoint");

        // Export carries the stored hash and the tenant config.
        let data = store.export_backup_data().await.expect("export");
        let exported_key = data
            .api_keys
            .iter()
            .find(|k| k.id == key.id)
            .expect("key in export");
        assert_eq!(exported_key.key_hash, hash);
        let exported_tenant = data
            .tenants
            .iter()
            .find(|t| t.id == tenant.id)
            .expect("tenant in export");
        assert_eq!(exported_tenant.weight, 250);
        let exported_model = data
            .models
            .iter()
            .find(|m| m.id == model.id)
            .expect("model in export");
        assert_eq!(exported_model.route_bias, 2.5, "export carries route_bias");
        assert!(
            !exported_model.auto_eligible,
            "export carries auto_eligible"
        );
        assert_eq!(exported_model.tags, vec!["coding:3".to_string()]);

        // Drift the tenant, then restore: the backup wins and counts as an
        // update (everything in the export already exists, so zero inserts).
        store
            .update_tenant_weight(tenant.id, 999)
            .await
            .expect("update weight");
        // Drift the model's bias the same way, through the same restore.
        sqlx::query("update models set route_bias = 1.0, auto_eligible = true where id = $1")
            .bind(model.id)
            .execute(&store.pool)
            .await
            .expect("drift route_bias and auto_eligible");
        // And its capacity discovery fields, and the endpoint's concurrency.
        sqlx::query(
            "update models set capacity_mode = 'static', capacity_source = 'endpoints',
                    capacity_namespace = null, capacity_service = null,
                    per_replica_max_in_flight = null, capacity_headroom = 1
             where id = $1",
        )
        .bind(model.id)
        .execute(&store.pool)
        .await
        .expect("drift capacity fields");
        sqlx::query("update model_endpoints set max_in_flight = null where id = $1")
            .bind(endpoint.id)
            .execute(&store.pool)
            .await
            .expect("drift endpoint concurrency");
        let report = store.restore_backup_data(&data).await.expect("restore");
        assert!(report.tenants.updated >= 1);
        let restored_model = store.get_model(model.id).await.expect("get model");
        assert_eq!(
            restored_model.route_bias, 2.5,
            "restore must not reset route_bias to the neutral default"
        );
        assert!(
            !restored_model.auto_eligible,
            "restore must not re-include a model the backup had excluded"
        );
        assert_eq!(restored_model.tags, vec!["coding:3".to_string()]);
        assert_eq!(restored_model.capacity_mode, "discovered");
        assert_eq!(restored_model.capacity_source, "kubernetes");
        assert_eq!(
            restored_model.capacity_namespace.as_deref(),
            Some("inference")
        );
        assert_eq!(
            restored_model.capacity_service.as_deref(),
            Some("upstream-model")
        );
        assert_eq!(restored_model.per_replica_max_in_flight, Some(8));
        assert_eq!(restored_model.capacity_headroom, 1.5);
        let restored_endpoint = store
            .get_model_endpoint(endpoint.id)
            .await
            .expect("get endpoint");
        assert_eq!(restored_endpoint.max_in_flight, Some(12));
        assert_eq!(report.tenants.inserted, 0);
        let restored = store.get_tenant(tenant.id).await.expect("get tenant");
        assert_eq!(restored.weight, 250);

        // Delete the key, restore again: it comes back as an insert and the
        // original secret's hash resolves like before the deletion.
        store.delete_key(key.id).await.expect("delete key");
        let report = store.restore_backup_data(&data).await.expect("restore 2");
        assert!(report.api_keys.inserted >= 1);
        let resolved = store
            .resolved_key_by_hash(&hash)
            .await
            .expect("resolve")
            .expect("present");
        assert_eq!(resolved.tenant_id, tenant.id);

        // A restore must not abort when a *different* row already occupies
        // the backed-up identity key's `key_hash` / `(issuer, subject)` slot
        // -- the exact situation after an identity key is deleted and the
        // same IdP subject signs in again before the restore runs, minting a
        // fresh id for the same issuer/subject (see the `delete from
        // api_keys ... and id <> $1` step added ahead of the upsert in
        // `restore_backup_data`). Reuses this test's own store/fixtures
        // rather than a second, concurrently-running whole-database
        // export/restore test, since `export_backup_data` and
        // `restore_backup_data` operate on every row in the instance, not
        // just this test's own -- two such tests running in parallel against
        // the same database can otherwise race each other's snapshots.
        use obleth_config::IdentityProvision;
        let issuer = "https://idp.example.com";
        let subject = format!("alice-{}", uuid::Uuid::new_v4());

        let provisioned = store
            .provision_identity_key(IdentityProvision {
                issuer,
                subject: &subject,
                tenant_name: &tenant.name,
                claims: serde_json::Value::Null,
            })
            .await
            .expect("provision identity");
        let original_key_id = provisioned.resolved.key_id;

        // Export carries the identity key under its original id.
        let data = store.export_backup_data().await.expect("export 3");
        let exported = data
            .api_keys
            .iter()
            .find(|k| k.id == original_key_id)
            .expect("identity key in export");
        assert_eq!(exported.kind, obleth_config::API_KEY_KIND_IDENTITY);
        assert_eq!(exported.identity_subject.as_deref(), Some(subject.as_str()));

        // Simulate "fresh state": delete the row, then re-provision the same
        // identity. The partial unique index has no competing row anymore, so
        // this mints a brand new id for the same (issuer, subject) -- and
        // since the key_hash is `identity_key_hash(issuer, subject)`, the new
        // row carries the *same* key_hash as the one in the backup.
        store.delete_key(original_key_id).await.expect("delete key");
        let reprovisioned = store
            .provision_identity_key(IdentityProvision {
                issuer,
                subject: &subject,
                tenant_name: &tenant.name,
                claims: serde_json::Value::Null,
            })
            .await
            .expect("re-provision identity");
        assert_ne!(reprovisioned.resolved.key_id, original_key_id);
        assert_eq!(reprovisioned.hash, exported.key_hash);

        // Restoring the export must succeed (not abort on the key_hash /
        // identity unique-index collision) and leave exactly one row for
        // this identity, matching the backup's original id.
        let report = store.restore_backup_data(&data).await.expect("restore 3");
        assert!(report.api_keys.inserted >= 1);

        let keys = store.list_keys(Some(tenant.id)).await.expect("list keys");
        let matching: Vec<_> = keys
            .iter()
            .filter(|k| k.identity_subject.as_deref() == Some(subject.as_str()))
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "expected exactly one row for the identity"
        );
        assert_eq!(matching[0].id, original_key_id);
    }
}
