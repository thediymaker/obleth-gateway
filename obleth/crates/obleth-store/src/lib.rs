//! Postgres config source-of-truth + audit log.
//!
//! This is the durable, relational backbone behind the Management API: tenants,
//! keys, quotas and a full change history. It is deliberately *off* the request
//! hot path — the data plane reads Redis, which this layer keeps in sync.

use chrono::{DateTime, Utc};
use obleth_config::{
    generate_api_key, ApiKey, FairshareGroup, McpServer, ModelEndpoint, ModelHealthCheck,
    ModelHealthDetail, ModelHealthSummary, ModelRoute, ResolvedEndpoint, ResolvedKey,
    ResolvedMcpServer, ResolvedModel, Tenant, WeeklyWindow,
};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;
use std::sync::OnceLock;
use uuid::Uuid;

mod crypto;
pub use crypto::{Cipher, CryptoError};

/// Process-wide cipher for upstream secret columns, initialized once from the
/// environment. Kept global so the row-mapping helpers can transparently
/// decrypt without threading the key through every call site.
static CIPHER: OnceLock<Cipher> = OnceLock::new();

fn cipher() -> &'static Cipher {
    CIPHER.get_or_init(Cipher::from_env)
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("db: {0}")]
    Db(#[from] sqlx::Error),
    #[error("not found")]
    NotFound,
    #[error("crypto: {0}")]
    Crypto(#[from] CryptoError),
}

type Result<T> = std::result::Result<T, StoreError>;

/// Embedded idempotent schema, applied on boot. A versioned copy also lives in
/// `schema/postgres/` for operators who manage migrations out of band.
const SCHEMA: &str = include_str!("../../../../schema/postgres/0001_init.sql");

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

impl Store {
    pub async fn connect(database_url: &str) -> Result<Self> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .connect(database_url)
            .await?;
        // Eagerly initialize the cipher so a misconfigured key fails at boot.
        let _ = cipher();
        Ok(Store { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Apply the embedded schema (CREATE ... IF NOT EXISTS).
    pub async fn migrate(&self) -> Result<()> {
        sqlx::raw_sql(SCHEMA).execute(&self.pool).await?;
        Ok(())
    }

    // ---- fairshare groups -----------------------------------------------

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
        .bind(weight.max(1))
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
        .bind(weight.max(1))
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
        let row = sqlx::query(
            "update tenants set fairshare_group = $2, updated_at = now() where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, created_at, updated_at",
        )
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
        let row = sqlx::query(
            "insert into tenants (id, name, fairshare_group, weight, tokens_per_minute, max_in_flight)
             values ($1, $2, $3, $4, $5, $6)
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, created_at, updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(group)
        .bind(weight.max(1))
        .bind(tokens_per_minute.max(0))
        .bind(max_in_flight)
        .fetch_one(&self.pool)
        .await?;
        tenant_from_row(&row)
    }

    pub async fn list_tenants(&self) -> Result<Vec<Tenant>> {
        let rows = sqlx::query(
            "select id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, created_at, updated_at
             from tenants order by created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(tenant_from_row).collect()
    }

    pub async fn get_tenant(&self, id: Uuid) -> Result<Tenant> {
        let row = sqlx::query(
            "select id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, created_at, updated_at
             from tenants where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    pub async fn update_tenant_weight(&self, id: Uuid, weight: i64) -> Result<Tenant> {
        let row = sqlx::query(
            "update tenants set weight = $2, updated_at = now() where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, created_at, updated_at",
        )
        .bind(id)
        .bind(weight.max(1))
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
        let row = sqlx::query(
            "update tenants set tokens_per_minute = $2, max_in_flight = $3, updated_at = now()
             where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, created_at, updated_at",
        )
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
        let row = sqlx::query(
            "update tenants set name = $2, description = $3, organization = $4,
                    contact_email = $5, updated_at = now()
             where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, created_at, updated_at",
        )
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
        let row = sqlx::query(
            "update tenants set status = $2, updated_at = now() where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, created_at, updated_at",
        )
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
        let windows = weekly_windows
            .filter(|w| !w.is_empty())
            .map(sqlx::types::Json);
        let row = sqlx::query(
            "update tenants set timezone = $2, active_from = $3, active_until = $4,
                    weekly_windows = $5, updated_at = now()
             where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, created_at, updated_at",
        )
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
        let row = sqlx::query(
            "update tenants set budget_tokens = $2, budget_cost_usd = $3, budget_period = $4,
                    budget_started_at = $5, updated_at = now()
             where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, created_at, updated_at",
        )
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
        let allowed = allowed_models
            .filter(|m| !m.is_empty())
            .map(sqlx::types::Json);
        let row = sqlx::query(
            "update tenants set allowed_models = $2, updated_at = now()
             where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, created_at, updated_at",
        )
        .bind(id)
        .bind(allowed)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    /// Hard-delete a tenant. Cascades to its API keys (FK `on delete cascade`).
    /// Returns the key hashes that were removed so callers can evict caches.
    pub async fn delete_tenant(&self, id: Uuid) -> Result<Vec<String>> {
        let hashes: Vec<String> = sqlx::query("select key_hash from api_keys where tenant_id = $1")
            .bind(id)
            .fetch_all(&self.pool)
            .await?
            .iter()
            .map(|r| r.try_get::<String, _>("key_hash"))
            .collect::<std::result::Result<_, _>>()?;
        let res = sqlx::query("delete from tenants where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if res.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(hashes)
    }

    // ---- api keys --------------------------------------------------------

    /// Create a key. Returns the stored metadata plus the one-time raw secret.
    pub async fn create_api_key(&self, tenant_id: Uuid, name: &str) -> Result<(ApiKey, String)> {
        let gen = generate_api_key();
        let row = sqlx::query(
            "insert into api_keys (id, tenant_id, name, key_prefix, key_hash)
             values ($1, $2, $3, $4, $5)
             returning id, tenant_id, name, key_prefix, disabled, created_at",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(name)
        .bind(&gen.prefix)
        .bind(&gen.hash)
        .fetch_one(&self.pool)
        .await?;
        Ok((api_key_from_row(&row)?, gen.secret))
    }

    pub async fn list_keys(&self, tenant_id: Option<Uuid>) -> Result<Vec<ApiKey>> {
        let rows = match tenant_id {
            Some(t) => {
                sqlx::query(
                    "select id, tenant_id, name, key_prefix, disabled, created_at
                     from api_keys where tenant_id = $1 order by created_at",
                )
                .bind(t)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "select id, tenant_id, name, key_prefix, disabled, created_at
                     from api_keys order by created_at",
                )
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter().map(api_key_from_row).collect()
    }

    /// Fetch a bounded set of keys by id in one query. Used to resolve the
    /// key ids on a page of request-log rows to display names without loading
    /// the full key fleet (which can be 100k+).
    pub async fn keys_by_ids(&self, ids: &[Uuid]) -> Result<Vec<ApiKey>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(
            "select id, tenant_id, name, key_prefix, disabled, created_at
             from api_keys where id = any($1)",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(api_key_from_row).collect()
    }

    pub async fn set_key_disabled(
        &self,
        id: Uuid,
        disabled: bool,
    ) -> Result<(String, ResolvedKey)> {
        let row = sqlx::query("update api_keys set disabled = $2 where id = $1 returning key_hash")
            .bind(id)
            .bind(disabled)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        let hash: String = row.try_get("key_hash")?;
        let resolved = self
            .resolved_key_by_hash(&hash)
            .await?
            .ok_or(StoreError::NotFound)?;
        Ok((hash, resolved))
    }

    pub async fn delete_key(&self, id: Uuid) -> Result<String> {
        let row = sqlx::query("delete from api_keys where id = $1 returning key_hash")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        Ok(row.try_get("key_hash")?)
    }

    /// Resolve a single key (joined with its tenant) by hash.
    pub async fn resolved_key_by_hash(&self, hash: &str) -> Result<Option<ResolvedKey>> {
        let row = sqlx::query(
            "select k.id as key_id, k.tenant_id, k.disabled,
                    t.name as tenant_name, t.fairshare_group, g.weight as group_weight,
                    t.weight, t.tokens_per_minute, t.max_in_flight, t.status,
                    t.timezone, t.active_from, t.active_until, t.weekly_windows,
                    t.budget_tokens, t.budget_cost_usd, t.budget_period, t.budget_started_at, t.allowed_models
             from api_keys k
             join tenants t on t.id = k.tenant_id
             join fairshare_groups g on g.name = t.fairshare_group
             where k.key_hash = $1",
        )
        .bind(hash)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(resolved_from_row).transpose()
    }

    /// All (hash, resolved-key) pairs, for warming/refreshing the Redis cache.
    pub async fn all_resolved_keys(&self) -> Result<Vec<(String, ResolvedKey)>> {
        let rows = sqlx::query(
            "select k.key_hash, k.id as key_id, k.tenant_id, k.disabled,
                    t.name as tenant_name, t.fairshare_group, g.weight as group_weight,
                    t.weight, t.tokens_per_minute, t.max_in_flight, t.status,
                    t.timezone, t.active_from, t.active_until, t.weekly_windows,
                    t.budget_tokens, t.budget_cost_usd, t.budget_period, t.budget_started_at, t.allowed_models
             from api_keys k
             join tenants t on t.id = k.tenant_id
             join fairshare_groups g on g.name = t.fairshare_group",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get::<String, _>("key_hash")?, resolved_from_row(r)?)))
            .collect()
    }

    /// All keys belonging to a tenant, as (hash, resolved). Used to re-sync the
    /// cache after a weight/quota change touches every key of a tenant.
    pub async fn resolved_keys_for_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<(String, ResolvedKey)>> {
        let rows = sqlx::query(
            "select k.key_hash, k.id as key_id, k.tenant_id, k.disabled,
                    t.name as tenant_name, t.fairshare_group, g.weight as group_weight,
                    t.weight, t.tokens_per_minute, t.max_in_flight, t.status,
                    t.timezone, t.active_from, t.active_until, t.weekly_windows,
                    t.budget_tokens, t.budget_cost_usd, t.budget_period, t.budget_started_at, t.allowed_models
             from api_keys k
             join tenants t on t.id = k.tenant_id
             join fairshare_groups g on g.name = t.fairshare_group
             where k.tenant_id = $1",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| Ok((r.try_get::<String, _>("key_hash")?, resolved_from_row(r)?)))
            .collect()
    }

    // ---- audit -----------------------------------------------------------

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

    // ---- models ----------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn create_model(
        &self,
        model_name: &str,
        description: &str,
        upstream_model: &str,
        api_base: &str,
        api_key: Option<&str>,
        model_type: &str,
        input_cost_per_token: f64,
        output_cost_per_token: f64,
        cost_per_image: f64,
        cost_per_audio_second: f64,
        cost_per_character: f64,
        context_window: i64,
        admission_weight: i64,
        max_in_flight: Option<i64>,
        supports_function_calling: bool,
        supports_system_messages: bool,
        supports_response_schema: bool,
        supports_tool_choice: bool,
        tags: &[String],
    ) -> Result<ModelRoute> {
        let api_key = cipher().encrypt_opt(api_key);
        let row = sqlx::query(
            "insert into models (
                id, model_name, description, upstream_model, api_base, api_key, model_type,
                input_cost_per_token, output_cost_per_token,
                cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                supports_response_schema, supports_tool_choice, tags
             ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20)
             returning id, model_name, description, upstream_model, api_base, api_key, model_type,
                       input_cost_per_token, output_cost_per_token,
                       cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                       admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                       supports_response_schema, supports_tool_choice, enabled,
                       cache_enabled, cache_ttl_secs, tags,
                       capacity_mode, capacity_tuned_at,
                       created_at, updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(model_name)
        .bind(description)
        .bind(upstream_model)
        .bind(api_base)
        .bind(api_key)
        .bind(obleth_config::normalize_model_type(model_type))
        .bind(input_cost_per_token)
        .bind(output_cost_per_token)
        .bind(cost_per_image.max(0.0))
        .bind(cost_per_audio_second.max(0.0))
        .bind(cost_per_character.max(0.0))
        .bind(context_window.max(0))
        .bind(admission_weight.max(1))
        .bind(max_in_flight.map(|n| n.max(1)))
        .bind(supports_function_calling)
        .bind(supports_system_messages)
        .bind(supports_response_schema)
        .bind(supports_tool_choice)
        .bind(sqlx::types::Json(obleth_config::normalize_tags(tags)))
        .fetch_one(&self.pool)
        .await?;
        model_from_row(&row)
    }

    pub async fn list_models(&self) -> Result<Vec<ModelRoute>> {
        let rows = sqlx::query(
            "select id, model_name, description, upstream_model, api_base, api_key, model_type,
                    input_cost_per_token, output_cost_per_token,
                    cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                    admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                    supports_response_schema, supports_tool_choice, enabled,
                    cache_enabled, cache_ttl_secs, tags,
                    capacity_mode, capacity_tuned_at,
                    created_at, updated_at
             from models order by model_name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(model_from_row).collect()
    }

    pub async fn get_model(&self, id: Uuid) -> Result<ModelRoute> {
        let row = sqlx::query(
            "select id, model_name, description, upstream_model, api_base, api_key, model_type,
                    input_cost_per_token, output_cost_per_token,
                    cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                    admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                    supports_response_schema, supports_tool_choice, enabled,
                    cache_enabled, cache_ttl_secs, tags,
                    capacity_mode, capacity_tuned_at,
                    created_at, updated_at
             from models where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    pub async fn get_model_by_name(&self, model_name: &str) -> Result<ModelRoute> {
        let row = sqlx::query(
            "select id, model_name, description, upstream_model, api_base, api_key, model_type,
                    input_cost_per_token, output_cost_per_token,
                    cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                    admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                    supports_response_schema, supports_tool_choice, enabled,
                    cache_enabled, cache_ttl_secs, tags,
                    capacity_mode, capacity_tuned_at,
                    created_at, updated_at
             from models where model_name = $1",
        )
        .bind(model_name)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_model(
        &self,
        id: Uuid,
        description: &str,
        upstream_model: &str,
        api_base: &str,
        api_key: Option<&str>,
        model_type: &str,
        input_cost_per_token: f64,
        output_cost_per_token: f64,
        cost_per_image: f64,
        cost_per_audio_second: f64,
        cost_per_character: f64,
        context_window: i64,
        admission_weight: i64,
        max_in_flight: Option<i64>,
        supports_function_calling: bool,
        supports_system_messages: bool,
        supports_response_schema: bool,
        supports_tool_choice: bool,
        enabled: bool,
        tags: &[String],
    ) -> Result<ModelRoute> {
        let api_key = cipher().encrypt_opt(api_key);
        let row = sqlx::query(
            "update models set
                description = $2, upstream_model = $3, api_base = $4, api_key = $5,
                input_cost_per_token = $6, output_cost_per_token = $7,
                context_window = $8, admission_weight = $9,
                max_in_flight = $10,
                supports_function_calling = $11, supports_system_messages = $12,
                supports_response_schema = $13, supports_tool_choice = $14,
                enabled = $15, tags = $16, model_type = $17,
                cost_per_image = $18, cost_per_audio_second = $19, cost_per_character = $20,
                updated_at = now()
             where id = $1
             returning id, model_name, description, upstream_model, api_base, api_key, model_type,
                       input_cost_per_token, output_cost_per_token,
                       cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                       admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                       supports_response_schema, supports_tool_choice, enabled,
                       cache_enabled, cache_ttl_secs, tags,
                       capacity_mode, capacity_tuned_at,
                       created_at, updated_at",
        )
        .bind(id)
        .bind(description)
        .bind(upstream_model)
        .bind(api_base)
        .bind(api_key)
        .bind(input_cost_per_token)
        .bind(output_cost_per_token)
        .bind(context_window.max(0))
        .bind(admission_weight.max(1))
        .bind(max_in_flight.map(|n| n.max(1)))
        .bind(supports_function_calling)
        .bind(supports_system_messages)
        .bind(supports_response_schema)
        .bind(supports_tool_choice)
        .bind(enabled)
        .bind(sqlx::types::Json(obleth_config::normalize_tags(tags)))
        .bind(obleth_config::normalize_model_type(model_type))
        .bind(cost_per_image.max(0.0))
        .bind(cost_per_audio_second.max(0.0))
        .bind(cost_per_character.max(0.0))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    pub async fn delete_model(&self, id: Uuid) -> Result<()> {
        let r = sqlx::query("delete from models where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if r.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub async fn update_model_capacity(
        &self,
        id: Uuid,
        max_in_flight: Option<i64>,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(
            "update models set max_in_flight = $2, updated_at = now()
             where id = $1
             returning id, model_name, description, upstream_model, api_base, api_key, model_type,
                       input_cost_per_token, output_cost_per_token,
                       cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                       admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                       supports_response_schema, supports_tool_choice, enabled,
                       cache_enabled, cache_ttl_secs, tags,
                       capacity_mode, capacity_tuned_at,
                       created_at, updated_at",
        )
        .bind(id)
        .bind(max_in_flight.map(|n| n.max(1)))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Set the capacity-tuning mode (`static` or `tuned`) for a model. Switching
    /// to `static` leaves `max_in_flight` and `capacity_tuned_at` untouched so
    /// operators can keep or edit the value the tuner found.
    pub async fn update_model_capacity_mode(
        &self,
        id: Uuid,
        capacity_mode: &str,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(
            "update models set capacity_mode = $2, updated_at = now()
             where id = $1
             returning id, model_name, description, upstream_model, api_base, api_key, model_type,
                       input_cost_per_token, output_cost_per_token,
                       cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                       admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                       supports_response_schema, supports_tool_choice, enabled,
                       cache_enabled, cache_ttl_secs, tags,
                       capacity_mode, capacity_tuned_at,
                       created_at, updated_at",
        )
        .bind(id)
        .bind(obleth_config::normalize_capacity_mode(capacity_mode))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Apply a value found by auto-tune: set `max_in_flight`, flip the model to
    /// `tuned` mode, and stamp `capacity_tuned_at` so the dashboard can show
    /// when the recommendation was last applied.
    pub async fn apply_tuned_model_capacity(
        &self,
        id: Uuid,
        max_in_flight: i64,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(
            "update models set max_in_flight = $2, capacity_mode = 'tuned',
                    capacity_tuned_at = now(), updated_at = now()
             where id = $1
             returning id, model_name, description, upstream_model, api_base, api_key, model_type,
                       input_cost_per_token, output_cost_per_token,
                       cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                       admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                       supports_response_schema, supports_tool_choice, enabled,
                       cache_enabled, cache_ttl_secs, tags,
                       capacity_mode, capacity_tuned_at,
                       created_at, updated_at",
        )
        .bind(id)
        .bind(max_in_flight.max(1))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    pub async fn update_model_admission_weight(
        &self,
        id: Uuid,
        admission_weight: i64,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(
            "update models set admission_weight = $2, updated_at = now()
             where id = $1
             returning id, model_name, description, upstream_model, api_base, api_key, model_type,
                       input_cost_per_token, output_cost_per_token,
                       cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                       admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                       supports_response_schema, supports_tool_choice, enabled,
                       cache_enabled, cache_ttl_secs, tags,
                       capacity_mode, capacity_tuned_at,
                       created_at, updated_at",
        )
        .bind(id)
        .bind(admission_weight.max(1))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    pub async fn all_resolved_models(&self) -> Result<Vec<(String, ResolvedModel)>> {
        let rows = sqlx::query(
            "select id, model_name, upstream_model, api_base, api_key, model_type, admission_weight, max_in_flight, enabled,
                    cache_enabled, cache_ttl_secs, input_cost_per_token, output_cost_per_token,
                    cost_per_image, cost_per_audio_second, cost_per_character,
                    context_window, supports_function_calling, supports_system_messages,
                    supports_response_schema, supports_tool_choice, tags,
                    request_timeout_secs, max_retries, retry_backoff_ms, endpoint_selection_mode
             from models where enabled = true",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let name: String = row.try_get("model_name")?;
            let model_id: Uuid = row.try_get("id")?;
            let endpoints = self.resolved_endpoints_for(model_id).await?;
            out.push((
                name.clone(),
                ResolvedModel {
                    model_name: name,
                    upstream_model: row.try_get("upstream_model")?,
                    api_base: row.try_get("api_base")?,
                    api_key: cipher().decrypt_opt(row.try_get("api_key")?)?,
                    model_type: row.try_get("model_type")?,
                    admission_weight: row.try_get("admission_weight")?,
                    max_in_flight: row
                        .try_get::<Option<i64>, _>("max_in_flight")?
                        .and_then(|n| usize::try_from(n).ok()),
                    enabled: row.try_get("enabled")?,
                    cache_enabled: row.try_get("cache_enabled")?,
                    cache_ttl_secs: row.try_get("cache_ttl_secs")?,
                    input_cost_per_token: row.try_get("input_cost_per_token")?,
                    output_cost_per_token: row.try_get("output_cost_per_token")?,
                    cost_per_image: row.try_get("cost_per_image")?,
                    cost_per_audio_second: row.try_get("cost_per_audio_second")?,
                    cost_per_character: row.try_get("cost_per_character")?,
                    context_window: row.try_get("context_window")?,
                    supports_function_calling: row.try_get("supports_function_calling")?,
                    supports_system_messages: row.try_get("supports_system_messages")?,
                    supports_response_schema: row.try_get("supports_response_schema")?,
                    supports_tool_choice: row.try_get("supports_tool_choice")?,
                    tags: row
                        .try_get::<sqlx::types::Json<Vec<String>>, _>("tags")
                        .map(|j| j.0)
                        .unwrap_or_default(),
                    request_timeout_secs: row.try_get("request_timeout_secs")?,
                    max_retries: row.try_get("max_retries")?,
                    retry_backoff_ms: row.try_get("retry_backoff_ms")?,
                    endpoint_selection_mode: row.try_get("endpoint_selection_mode")?,
                    endpoints,
                },
            ));
        }
        Ok(out)
    }

    /// Toggle (and set the TTL of) the response cache for a model.
    pub async fn update_model_cache(
        &self,
        id: Uuid,
        cache_enabled: bool,
        cache_ttl_secs: i64,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(
            "update models set cache_enabled = $2, cache_ttl_secs = $3, updated_at = now()
             where id = $1
             returning id, model_name, description, upstream_model, api_base, api_key, model_type,
                       input_cost_per_token, output_cost_per_token,
                       cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                       admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                       supports_response_schema, supports_tool_choice, enabled,
                       cache_enabled, cache_ttl_secs, tags,
                       capacity_mode, capacity_tuned_at,
                       created_at, updated_at",
        )
        .bind(id)
        .bind(cache_enabled)
        .bind(cache_ttl_secs.max(0))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    /// Set the per-model reliability controls (upstream timeout, retries,
    /// retry backoff, endpoint-selection mode). A `None` timeout means the
    /// model defers to the global default.
    pub async fn update_model_reliability(
        &self,
        id: Uuid,
        request_timeout_secs: Option<i64>,
        max_retries: i64,
        retry_backoff_ms: i64,
        endpoint_selection_mode: &str,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(
            "update models set request_timeout_secs = $2, max_retries = $3,
                    retry_backoff_ms = $4, endpoint_selection_mode = $5, updated_at = now()
             where id = $1
             returning id, model_name, description, upstream_model, api_base, api_key, model_type,
                       input_cost_per_token, output_cost_per_token,
                       cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                       admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                       supports_response_schema, supports_tool_choice, enabled,
                       cache_enabled, cache_ttl_secs, tags,
                       capacity_mode, capacity_tuned_at,
                       request_timeout_secs, max_retries, retry_backoff_ms, endpoint_selection_mode,
                       created_at, updated_at",
        )
        .bind(id)
        .bind(request_timeout_secs.filter(|n| *n >= 1))
        .bind(max_retries.max(0))
        .bind(retry_backoff_ms.max(0))
        .bind(obleth_config::normalize_endpoint_selection_mode(
            endpoint_selection_mode,
        ))
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    // ---- model endpoints -------------------------------------------------

    /// Hot-path endpoint views for one model: enabled endpoints with their
    /// decrypted upstream keys and current health, ordered by priority.
    pub async fn resolved_endpoints_for(
        &self,
        model_id: Uuid,
    ) -> Result<Vec<ResolvedEndpoint>> {
        let rows = sqlx::query(
            "select id, api_base, api_key, priority, weight, enabled, health_status
             from model_endpoints
             where model_id = $1
             order by priority asc, created_at asc",
        )
        .bind(model_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                let status: String = r.try_get("health_status")?;
                Ok(ResolvedEndpoint {
                    id: r.try_get::<Uuid, _>("id")?.to_string(),
                    api_base: r.try_get("api_base")?,
                    api_key: cipher().decrypt_opt(r.try_get("api_key")?)?,
                    priority: r.try_get("priority")?,
                    weight: r.try_get("weight")?,
                    enabled: r.try_get("enabled")?,
                    // Treat unknown/degraded as eligible (soft-pass); only an
                    // explicit unhealthy/disabled state removes an endpoint.
                    healthy: !matches!(status.as_str(), "unhealthy" | "disabled"),
                })
            })
            .collect()
    }

    pub async fn list_model_endpoints(&self, model_id: Uuid) -> Result<Vec<ModelEndpoint>> {
        let rows = sqlx::query(
            "select id, model_id, name, api_base, api_key, priority, weight, enabled,
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
            "select id, model_id, name, api_base, api_key, priority, weight, enabled,
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
            "select id, model_id, name, api_base, api_key, priority, weight, enabled,
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
    ) -> Result<ModelEndpoint> {
        let api_key = cipher().encrypt_opt(api_key);
        let row = sqlx::query(
            "insert into model_endpoints (id, model_id, name, api_base, api_key, priority, weight, enabled)
             values ($1, $2, $3, $4, $5, $6, $7, $8)
             returning id, model_id, name, api_base, api_key, priority, weight, enabled,
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
        .fetch_one(&self.pool)
        .await?;
        endpoint_from_row(&row)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_model_endpoint(
        &self,
        id: Uuid,
        name: &str,
        api_base: &str,
        api_key: Option<&str>,
        priority: i64,
        weight: i64,
        enabled: bool,
    ) -> Result<ModelEndpoint> {
        // A `None` api_key leaves the stored secret untouched (so the UI can
        // edit other fields without re-entering the key); an empty string
        // clears it.
        let row = match api_key {
            Some(secret) => {
                let enc = cipher().encrypt_opt(Some(secret));
                sqlx::query(
                    "update model_endpoints set name = $2, api_base = $3, api_key = $4,
                            priority = $5, weight = $6, enabled = $7, updated_at = now()
                     where id = $1
                     returning id, model_id, name, api_base, api_key, priority, weight, enabled,
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
                .fetch_optional(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "update model_endpoints set name = $2, api_base = $3,
                            priority = $4, weight = $5, enabled = $6, updated_at = now()
                     where id = $1
                     returning id, model_id, name, api_base, api_key, priority, weight, enabled,
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
                .fetch_optional(&self.pool)
                .await?
            }
        }
        .ok_or(StoreError::NotFound)?;
        endpoint_from_row(&row)
    }

    pub async fn delete_model_endpoint(&self, id: Uuid) -> Result<Uuid> {
        let row = sqlx::query("delete from model_endpoints where id = $1 returning model_id")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        Ok(row.try_get("model_id")?)
    }

    /// Record the outcome of a per-endpoint health check. Mirrors
    /// `record_model_health_check`: `healthy`/`disabled` reset the failure
    /// counter; `degraded`/`skipped` are transient (left untouched); anything
    /// else increments `consecutive_failures`.
    pub async fn record_endpoint_health(
        &self,
        id: Uuid,
        status: &str,
        latency_ms: Option<i64>,
        http_status: Option<i64>,
        message: Option<&str>,
    ) -> Result<ModelEndpoint> {
        let failure_delta: i64 = match status {
            "healthy" | "disabled" => 0,
            "degraded" | "skipped" => -1, // sentinel: leave counter as-is
            _ => 1,
        };
        let row = sqlx::query(
            "update model_endpoints set
                health_status = case when $2 in ('degraded','skipped') then health_status else $2 end,
                consecutive_failures = case
                    when $3 = 0 then 0
                    when $3 < 0 then consecutive_failures
                    else consecutive_failures + 1 end,
                last_checked_at = now(),
                last_latency_ms = $4,
                last_http_status = $5,
                last_message = $6,
                updated_at = now()
             where id = $1
             returning id, model_id, name, api_base, api_key, priority, weight, enabled,
                       health_status, consecutive_failures, alert_state,
                       last_checked_at, last_latency_ms, last_http_status, last_message,
                       created_at, updated_at",
        )
        .bind(id)
        .bind(status)
        .bind(failure_delta)
        .bind(latency_ms)
        .bind(http_status)
        .bind(message)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        endpoint_from_row(&row)
    }

    // ---- model health ----------------------------------------------------

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
                    health_alert_state
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
        let transient = status == "degraded" || status == "skipped";
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
        .bind(status)
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

    // ---- mcp servers -----------------------------------------------------

    pub async fn create_mcp_server(
        &self,
        name: &str,
        upstream_url: &str,
        auth_header: Option<&str>,
    ) -> Result<McpServer> {
        let auth_header = cipher().encrypt_opt(auth_header);
        let row = sqlx::query(
            "insert into mcp_servers (id, name, upstream_url, auth_header)
             values ($1, $2, $3, $4)
             returning id, name, upstream_url, auth_header, enabled, created_at, updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(name)
        .bind(upstream_url)
        .bind(auth_header)
        .fetch_one(&self.pool)
        .await?;
        mcp_server_from_row(&row)
    }

    pub async fn list_mcp_servers(&self) -> Result<Vec<McpServer>> {
        let rows = sqlx::query(
            "select id, name, upstream_url, auth_header, enabled, created_at, updated_at
             from mcp_servers order by name",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(mcp_server_from_row).collect()
    }

    pub async fn get_mcp_server(&self, id: Uuid) -> Result<McpServer> {
        let row = sqlx::query(
            "select id, name, upstream_url, auth_header, enabled, created_at, updated_at
             from mcp_servers where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        mcp_server_from_row(&row)
    }

    pub async fn update_mcp_server(
        &self,
        id: Uuid,
        upstream_url: &str,
        auth_header: Option<&str>,
        enabled: bool,
    ) -> Result<McpServer> {
        let auth_header = cipher().encrypt_opt(auth_header);
        let row = sqlx::query(
            "update mcp_servers set upstream_url = $2, auth_header = $3, enabled = $4, updated_at = now()
             where id = $1
             returning id, name, upstream_url, auth_header, enabled, created_at, updated_at",
        )
        .bind(id)
        .bind(upstream_url)
        .bind(auth_header)
        .bind(enabled)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        mcp_server_from_row(&row)
    }

    /// Delete an MCP server, returning its name so the cache can be invalidated.
    pub async fn delete_mcp_server(&self, id: Uuid) -> Result<String> {
        let row = sqlx::query("delete from mcp_servers where id = $1 returning name")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or(StoreError::NotFound)?;
        Ok(row.try_get("name")?)
    }

    /// All enabled MCP servers as (name, resolved), for warming the cache.
    pub async fn all_resolved_mcp_servers(&self) -> Result<Vec<(String, ResolvedMcpServer)>> {
        let rows = sqlx::query(
            "select name, upstream_url, auth_header, enabled
             from mcp_servers where enabled = true",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let name: String = row.try_get("name")?;
                Ok((
                    name.clone(),
                    ResolvedMcpServer {
                        name,
                        upstream_url: row.try_get("upstream_url")?,
                        auth_header: cipher().decrypt_opt(row.try_get("auth_header")?)?,
                        enabled: row.try_get("enabled")?,
                    },
                ))
            })
            .collect()
    }

    /// Load the persisted alert settings, or `None` if none have been saved.
    pub async fn get_alert_settings(&self) -> Result<Option<obleth_config::AlertSettings>> {
        let row = sqlx::query("select value from app_settings where key = 'alerts'")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => {
                let value: sqlx::types::Json<obleth_config::AlertSettings> =
                    row.try_get("value")?;
                Ok(Some(value.0))
            }
            None => Ok(None),
        }
    }

    /// Persist the alert settings (upsert on the single `alerts` key).
    pub async fn put_alert_settings(&self, settings: &obleth_config::AlertSettings) -> Result<()> {
        sqlx::query(
            "insert into app_settings (key, value, updated_at)
             values ('alerts', $1, now())
             on conflict (key) do update set value = excluded.value, updated_at = now()",
        )
        .bind(sqlx::types::Json(settings))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load the persisted `auto` router settings, or `None` if unset.
    pub async fn get_auto_router_settings(
        &self,
    ) -> Result<Option<obleth_config::AutoRouterSettings>> {
        let row = sqlx::query("select value from app_settings where key = 'auto_router'")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => {
                let value: sqlx::types::Json<obleth_config::AutoRouterSettings> =
                    row.try_get("value")?;
                Ok(Some(value.0))
            }
            None => Ok(None),
        }
    }

    /// Persist the `auto` router settings (upsert on the single `auto_router` key).
    pub async fn put_auto_router_settings(
        &self,
        settings: &obleth_config::AutoRouterSettings,
    ) -> Result<()> {
        sqlx::query(
            "insert into app_settings (key, value, updated_at)
             values ('auto_router', $1, now())
             on conflict (key) do update set value = excluded.value, updated_at = now()",
        )
        .bind(sqlx::types::Json(settings))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Load the persisted raw-usage retention setting, or `None` if unset.
    pub async fn get_usage_retention_settings(
        &self,
    ) -> Result<Option<obleth_config::UsageRetentionSettings>> {
        let row = sqlx::query("select value from app_settings where key = 'usage_retention'")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => {
                let value: sqlx::types::Json<obleth_config::UsageRetentionSettings> =
                    row.try_get("value")?;
                Ok(Some(value.0))
            }
            None => Ok(None),
        }
    }

    /// Persist the raw-usage retention setting (upsert on `usage_retention`).
    pub async fn put_usage_retention_settings(
        &self,
        settings: &obleth_config::UsageRetentionSettings,
    ) -> Result<()> {
        sqlx::query(
            "insert into app_settings (key, value, updated_at)
             values ('usage_retention', $1, now())
             on conflict (key) do update set value = excluded.value, updated_at = now()",
        )
        .bind(sqlx::types::Json(settings))
        .execute(&self.pool)
        .await?;
        Ok(())
    }
}

/// A single audit-log entry.
#[derive(Debug, Clone, serde::Serialize)]
pub struct AuditEntry {
    pub id: i64,
    pub ts: DateTime<Utc>,
    pub actor: String,
    pub action: String,
    pub entity_type: String,
    pub entity_id: String,
    pub detail: serde_json::Value,
}

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

fn tenant_from_row(row: &PgRow) -> Result<Tenant> {
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
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

/// Decode the optional `weekly_windows` jsonb column. A SQL `null` or an empty
/// array both collapse to `None` (meaning "any time of week").
fn weekly_windows_from_row(row: &PgRow) -> Result<Option<Vec<WeeklyWindow>>> {
    let json: Option<sqlx::types::Json<Vec<WeeklyWindow>>> = row.try_get("weekly_windows")?;
    Ok(json.map(|j| j.0).filter(|v| !v.is_empty()))
}

/// Decode the optional `allowed_models` jsonb column. A SQL `null` or empty
/// array both collapse to `None` (meaning "all models permitted").
fn allowed_models_from_row(row: &PgRow) -> Result<Option<Vec<String>>> {
    let json: Option<sqlx::types::Json<Vec<String>>> = row.try_get("allowed_models")?;
    Ok(json.map(|j| j.0).filter(|v| !v.is_empty()))
}

fn fairshare_group_from_row(row: &PgRow) -> Result<FairshareGroup> {
    Ok(FairshareGroup {
        name: row.try_get("name")?,
        weight: row.try_get("weight")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn api_key_from_row(row: &PgRow) -> Result<ApiKey> {
    Ok(ApiKey {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        name: row.try_get("name")?,
        key_prefix: row.try_get("key_prefix")?,
        disabled: row.try_get("disabled")?,
        created_at: row.try_get("created_at")?,
    })
}

fn resolved_from_row(row: &PgRow) -> Result<ResolvedKey> {
    Ok(ResolvedKey {
        key_id: row.try_get("key_id")?,
        tenant_id: row.try_get("tenant_id")?,
        tenant_name: row.try_get("tenant_name")?,
        fairshare_group: row.try_get("fairshare_group")?,
        group_weight: row.try_get("group_weight")?,
        weight: row.try_get("weight")?,
        tokens_per_minute: row.try_get("tokens_per_minute")?,
        max_in_flight: row.try_get("max_in_flight")?,
        disabled: row.try_get("disabled")?,
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
        internal: false,
    })
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

fn mcp_server_from_row(row: &PgRow) -> Result<McpServer> {
    Ok(McpServer {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        upstream_url: row.try_get("upstream_url")?,
        auth_header: cipher().decrypt_opt(row.try_get("auth_header")?)?,
        enabled: row.try_get("enabled")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn model_from_row(row: &PgRow) -> Result<ModelRoute> {
    Ok(ModelRoute {
        id: row.try_get("id")?,
        model_name: row.try_get("model_name")?,
        description: row.try_get("description")?,
        upstream_model: row.try_get("upstream_model")?,
        api_base: row.try_get("api_base")?,
        api_key: cipher().decrypt_opt(row.try_get("api_key")?)?,
        // Tolerant reads: SQL statements that don't select these newer columns
        // (e.g. capacity/weight toggles) degrade to defaults rather than
        // erroring. The full SELECT/RETURNING clauses do include them so the
        // resolved-cache refresh in `sync_model` sees the real values.
        model_type: row
            .try_get::<String, _>("model_type")
            .unwrap_or_else(|_| obleth_config::DEFAULT_MODEL_TYPE.to_string()),
        input_cost_per_token: row.try_get("input_cost_per_token")?,
        output_cost_per_token: row.try_get("output_cost_per_token")?,
        cost_per_image: row.try_get("cost_per_image").unwrap_or(0.0),
        cost_per_audio_second: row.try_get("cost_per_audio_second").unwrap_or(0.0),
        cost_per_character: row.try_get("cost_per_character").unwrap_or(0.0),
        context_window: row.try_get("context_window")?,
        admission_weight: row.try_get("admission_weight")?,
        max_in_flight: row.try_get("max_in_flight")?,
        // Tolerant reads for the capacity-mode columns: statements that don't
        // select them degrade to the static default rather than erroring.
        capacity_mode: row
            .try_get::<String, _>("capacity_mode")
            .unwrap_or_else(|_| obleth_config::DEFAULT_CAPACITY_MODE.to_string()),
        capacity_tuned_at: row.try_get("capacity_tuned_at").unwrap_or(None),
        supports_function_calling: row.try_get("supports_function_calling")?,
        supports_system_messages: row.try_get("supports_system_messages")?,
        supports_response_schema: row.try_get("supports_response_schema")?,
        supports_tool_choice: row.try_get("supports_tool_choice")?,
        enabled: row.try_get("enabled")?,
        cache_enabled: row.try_get("cache_enabled")?,
        cache_ttl_secs: row.try_get("cache_ttl_secs")?,
        // Tolerant read: SQL statements that don't select `tags` (e.g. capacity
        // toggles) degrade to an empty list rather than erroring.
        tags: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("tags")
            .map(|j| j.0)
            .unwrap_or_default(),
        // Tolerant reads for the reliability columns: statements that don't
        // select them degrade to defaults rather than erroring.
        request_timeout_secs: row.try_get("request_timeout_secs").unwrap_or(None),
        max_retries: row.try_get("max_retries").unwrap_or(0),
        retry_backoff_ms: row
            .try_get("retry_backoff_ms")
            .unwrap_or(obleth_config::DEFAULT_RETRY_BACKOFF_MS),
        endpoint_selection_mode: row
            .try_get::<String, _>("endpoint_selection_mode")
            .unwrap_or_else(|_| obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string()),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn endpoint_from_row(row: &PgRow) -> Result<ModelEndpoint> {
    Ok(ModelEndpoint {
        id: row.try_get("id")?,
        model_id: row.try_get("model_id")?,
        name: row.try_get("name")?,
        api_base: row.try_get("api_base")?,
        api_key: cipher().decrypt_opt(row.try_get("api_key")?)?,
        priority: row.try_get("priority")?,
        weight: row.try_get("weight")?,
        enabled: row.try_get("enabled")?,
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

#[cfg(test)]
mod tests {
    use super::*;
    use obleth_config::hash_api_key;

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` points at a
    /// throwaway Postgres. Skips silently otherwise so unit runs stay hermetic.
    #[tokio::test]
    async fn tenant_key_audit_roundtrip() {
        let Ok(url) = std::env::var("OBLETH_TEST_DATABASE_URL") else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");

        let name = format!("t-{}", Uuid::new_v4());
        let tenant = store
            .create_tenant(&name, 250, 1000, Some(4), None)
            .await
            .expect("create tenant");
        assert_eq!(tenant.weight, 250);

        let (key, secret) = store
            .create_api_key(tenant.id, "k")
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
            .create_model(
                &format!("m-{}", Uuid::new_v4()),
                "integration test model",
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
                &[],
            )
            .await
            .expect("create model");
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
        let claims = store
            .claim_due_model_health_checks(10)
            .await
            .expect("claim due");
        assert!(claims.iter().any(|claim| claim.model.id == model.id));

        let deleted = store
            .delete_model_health_checks_before(Utc::now() + chrono::Duration::days(1))
            .await
            .expect("delete old checks");
        assert!(deleted >= 2);
    }
}
