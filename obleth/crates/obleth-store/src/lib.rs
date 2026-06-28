//! Postgres config source-of-truth + audit log.
//!
//! This is the durable, relational backbone behind the Management API: tenants,
//! keys, quotas and a full change history. It is deliberately *off* the request
//! hot path — the data plane reads Redis, which this layer keeps in sync.

use chrono::{DateTime, Utc};
use obleth_config::{
    generate_api_key, ApiKey, FairshareGroup, ManagedModelSpec, McpServer, ModelEndpoint,
    ModelHealthCheck, ModelHealthDetail, ModelHealthSummary, ModelReplica, ModelRoute,
    ResolvedEndpoint, ResolvedKey, ResolvedMcpServer, ResolvedModel, Tenant, WeeklyWindow,
};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::Row;
use std::collections::HashMap;
use std::sync::OnceLock;
use uuid::Uuid;

mod backup;
mod crypto;
pub use backup::BACKUP_KEY_SENTINEL;
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
    #[error("{0}")]
    Conflict(String),
    /// A protected, system-owned resource (e.g. the reserved control-plane
    /// identity) cannot be mutated through the management API.
    #[error("{0}")]
    Protected(String),
}

type Result<T> = std::result::Result<T, StoreError>;

/// Embedded idempotent schema, applied on boot. A versioned copy also lives in
/// `schema/postgres/` for operators who manage migrations out of band.
const SCHEMA: &str = include_str!("../../../../schema/postgres/0001_init.sql");
const SCHEMA_V2: &str = include_str!("../../../../schema/postgres/0002_tracing_flag.sql");
const SCHEMA_V3: &str = include_str!("../../../../schema/postgres/0003_guardrails_policy.sql");
const SCHEMA_V4: &str = include_str!("../../../../schema/postgres/0004_saved_recipes.sql");
const SCHEMA_V5: &str = include_str!("../../../../schema/postgres/0005_managed_launcher_spec.sql");
const SCHEMA_V6: &str = include_str!("../../../../schema/postgres/0006_recipes.sql");
const SCHEMA_V7: &str =
    include_str!("../../../../schema/postgres/0007_replica_port_and_min_replicas.sql");
const SCHEMA_V8: &str =
    include_str!("../../../../schema/postgres/0008_managed_provision_error.sql");
const SCHEMA_V9: &str =
    include_str!("../../../../schema/postgres/0009_replica_cancel_requested.sql");
const SCHEMA_V10: &str =
    include_str!("../../../../schema/postgres/0010_drop_replica_model_cascade.sql");
const SCHEMA_V11: &str =
    include_str!("../../../../schema/postgres/0011_endpoint_selection_session_hash.sql");
const SCHEMA_V12: &str =
    include_str!("../../../../schema/postgres/0012_model_debug_diagnostics.sql");

/// Arbitrary, fixed key for the advisory lock that serializes `migrate()`
/// across connections, replicas and parallel test binaries.
const MIGRATE_LOCK_KEY: i64 = 0x0B1E_7480_0001;
/// Advisory-lock key serializing reserved control-plane identity provisioning
/// across racing replicas (distinct from `MIGRATE_LOCK_KEY`).
const CONTROL_PLANE_LOCK_KEY: i64 = 0x0B1E_7480_0002;

pub struct Recipe {
    pub id: Uuid,
    pub name: String,
    pub body: String,
    pub author: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

pub struct UpsertRecipe {
    pub id: Option<Uuid>,
    pub name: String,
    pub body: String,
    pub author: String,
}

/// Input for `upsert_managed_model` (no timestamps; the DB sets them).
pub struct UpsertManagedModel {
    pub model_id: Uuid,
    pub enabled: bool,
    pub partition: String,
    pub gres: String,
    pub nodes: i64,
    pub constraints: Option<String>,
    pub exclude: Option<String>,
    pub account: Option<String>,
    pub qos: Option<String>,
    pub time_limit: Option<String>,
    pub cpus_per_task: Option<i64>,
    pub mem: Option<String>,
    pub image: String,
    pub preamble: String,
    pub log_output_dir: String,
    pub launch_command: String,
    pub script_body: String,
    pub serving_port: i64,
    pub health_path: String,
    pub target_replicas: i64,
    pub min_replicas: i64,
    pub max_job_failures: i64,
    pub launcher_spec: Option<serde_json::Value>,
}

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
    ///
    /// `CREATE TABLE IF NOT EXISTS` is *not* safe under concurrency: two
    /// backends can both pass the existence check and then collide creating the
    /// table's implicit row-type, failing with a duplicate-key violation on
    /// `pg_type_typname_nsp_index`. Since this runs on every gateway boot (and
    /// thus races across replicas / parallel test binaries), we serialize the
    /// whole schema apply behind a session-level advisory lock taken on a single
    /// dedicated connection.
    pub async fn migrate(&self) -> Result<()> {
        let mut conn = self.pool.acquire().await?;
        sqlx::query("select pg_advisory_lock($1)")
            .bind(MIGRATE_LOCK_KEY)
            .execute(&mut *conn)
            .await?;
        let result: Result<()> = async {
            sqlx::raw_sql(SCHEMA).execute(&mut *conn).await?;
            sqlx::raw_sql(SCHEMA_V2).execute(&mut *conn).await?;
            sqlx::raw_sql(SCHEMA_V3).execute(&mut *conn).await?;
            sqlx::raw_sql(SCHEMA_V4).execute(&mut *conn).await?;
            sqlx::raw_sql(SCHEMA_V5).execute(&mut *conn).await?;
            sqlx::raw_sql(SCHEMA_V6).execute(&mut *conn).await?;
            sqlx::raw_sql(SCHEMA_V7).execute(&mut *conn).await?;
            sqlx::raw_sql(SCHEMA_V8).execute(&mut *conn).await?;
            sqlx::raw_sql(SCHEMA_V9).execute(&mut *conn).await?;
            sqlx::raw_sql(SCHEMA_V10).execute(&mut *conn).await?;
            sqlx::raw_sql(SCHEMA_V11).execute(&mut *conn).await?;
            sqlx::raw_sql(SCHEMA_V12).execute(&mut *conn).await?;
            Ok(())
        }
        .await;
        // Always release the lock, even if the schema apply failed.
        let _ = sqlx::query("select pg_advisory_unlock($1)")
            .bind(MIGRATE_LOCK_KEY)
            .execute(&mut *conn)
            .await;
        result?;
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
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(
            "update tenants set fairshare_group = $2, updated_at = now() where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, created_at, updated_at",
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
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, created_at, updated_at",
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
            "select id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, tracing_enabled, created_at, updated_at
             from tenants order by created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(tenant_from_row).collect()
    }

    pub async fn get_tenant(&self, id: Uuid) -> Result<Tenant> {
        let row = sqlx::query(
            "select id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, tracing_enabled, created_at, updated_at
             from tenants where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    pub async fn update_tenant_weight(&self, id: Uuid, weight: i64) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(
            "update tenants set weight = $2, updated_at = now() where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, created_at, updated_at",
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
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(
            "update tenants set tokens_per_minute = $2, max_in_flight = $3, updated_at = now()
             where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, created_at, updated_at",
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
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(
            "update tenants set name = $2, description = $3, organization = $4,
                    contact_email = $5, updated_at = now()
             where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, created_at, updated_at",
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
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(
            "update tenants set status = $2, updated_at = now() where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, created_at, updated_at",
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
        Self::guard_reserved_tenant(id)?;
        let windows = weekly_windows
            .filter(|w| !w.is_empty())
            .map(sqlx::types::Json);
        let row = sqlx::query(
            "update tenants set timezone = $2, active_from = $3, active_until = $4,
                    weekly_windows = $5, updated_at = now()
             where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, created_at, updated_at",
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
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(
            "update tenants set budget_tokens = $2, budget_cost_usd = $3, budget_period = $4,
                    budget_started_at = $5, updated_at = now()
             where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, created_at, updated_at",
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
        Self::guard_reserved_tenant(id)?;
        let allowed = allowed_models
            .filter(|m| !m.is_empty())
            .map(sqlx::types::Json);
        let row = sqlx::query(
            "update tenants set allowed_models = $2, updated_at = now()
             where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, created_at, updated_at",
        )
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
        let row = sqlx::query(
            "update tenants set guardrails_policy = $2, updated_at = now()
             where id = $1
             returning id, name, fairshare_group, weight, tokens_per_minute, max_in_flight, description, organization, contact_email, status, timezone, active_from, active_until, weekly_windows, budget_tokens, budget_cost_usd, budget_period, budget_started_at, allowed_models, guardrails_policy, created_at, updated_at",
        )
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

    // ---- api keys --------------------------------------------------------

    /// Create a key. Returns the stored metadata plus the one-time raw secret.
    pub async fn create_api_key(
        &self,
        tenant_id: Uuid,
        name: &str,
        description: &str,
        budget_tokens: Option<i64>,
        budget_cost_usd: Option<f64>,
        budget_period: Option<&str>,
        budget_started_at: Option<DateTime<Utc>>,
    ) -> Result<(ApiKey, String)> {
        let gen = generate_api_key();
        let row = sqlx::query(
            "insert into api_keys (id, tenant_id, name, description, key_prefix, key_hash,
                    budget_tokens, budget_cost_usd, budget_period, budget_started_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             returning id, tenant_id, name, description, key_prefix,
                    budget_tokens, budget_cost_usd, budget_period, budget_started_at,
                    disabled, tracing_enabled, created_at, updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(name)
        .bind(description)
        .bind(&gen.prefix)
        .bind(&gen.hash)
        .bind(budget_tokens)
        .bind(budget_cost_usd)
        .bind(budget_period)
        .bind(budget_started_at)
        .fetch_one(&self.pool)
        .await?;
        Ok((api_key_from_row(&row)?, gen.secret))
    }

    /// Well-known id for the reserved control-plane tenant that powers Charo,
    /// the in-app model test console. It is a *normal, visible* tenant — its
    /// test traffic is recorded under this identity — but is hidden from and
    /// protected against the Tenants/Keys management surfaces (see
    /// `StoreError::Protected`). Provisioned once on boot with no rate or term
    /// caps so manual tests are never throttled, and no model allowlist so it
    /// can reach every registered model (and thus every model's MCP servers).
    pub const CONTROL_PLANE_TENANT_ID: Uuid = Uuid::from_u128(0xc0);

    /// Ensure the reserved control-plane tenant + its api key exist. Idempotent
    /// and safe to call on every boot, including across racing replicas: the
    /// check-then-create runs under a Postgres advisory lock (mirroring
    /// `migrate`). On first creation the one-time key secret is stored encrypted
    /// in `app_settings` (`control_plane_key`) because the stored key hash is not
    /// reversible and the control-plane needs the raw secret to call the proxy.
    pub async fn ensure_control_plane_identity(&self) -> Result<()> {
        let mut conn = self.pool.acquire().await?;
        sqlx::query("select pg_advisory_lock($1)")
            .bind(CONTROL_PLANE_LOCK_KEY)
            .execute(&mut *conn)
            .await?;
        let result: Result<()> = async {
            // Reserved tenant: fixed id, default fairshare group, no per-minute
            // rate cap (tokens_per_minute = 0), no model allowlist (null ⇒ every
            // model + MCP). on-conflict makes this a no-op once provisioned.
            sqlx::query(
                "insert into tenants
                     (id, name, fairshare_group, weight, tokens_per_minute, description)
                 values ($1, '__control_plane__', 'default', 100, 0,
                         'Reserved identity for the in-app Charo model test console')
                 on conflict (id) do nothing",
            )
            .bind(Self::CONTROL_PLANE_TENANT_ID)
            .execute(&self.pool)
            .await?;

            // Key secret already stored ⇒ the api key exists; nothing more to do.
            if self.control_plane_key_secret().await?.is_some() {
                return Ok(());
            }

            // First provision: mint the key, capture the one-time secret, store
            // it encrypted so it survives restarts.
            let (_key, secret) = self
                .create_api_key(
                    Self::CONTROL_PLANE_TENANT_ID,
                    "charo",
                    "",
                    None,
                    None,
                    None,
                    None,
                )
                .await?;
            let enc = cipher().encrypt(&secret);
            sqlx::query(
                "insert into app_settings (key, value, updated_at)
                 values ('control_plane_key', $1, now())
                 on conflict (key) do nothing",
            )
            .bind(sqlx::types::Json(enc))
            .execute(&self.pool)
            .await?;
            Ok(())
        }
        .await;
        // Always release the lock, even if provisioning failed.
        let _ = sqlx::query("select pg_advisory_unlock($1)")
            .bind(CONTROL_PLANE_LOCK_KEY)
            .execute(&mut *conn)
            .await;
        result
    }

    /// Return the decrypted control-plane key secret, or `None` if it has not
    /// been provisioned yet.
    pub async fn control_plane_key_secret(&self) -> Result<Option<String>> {
        let row = sqlx::query("select value from app_settings where key = 'control_plane_key'")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => {
                let value: sqlx::types::Json<String> = row.try_get("value")?;
                Ok(Some(cipher().decrypt(&value.0)?))
            }
            None => Ok(None),
        }
    }

    /// Reject a mutation that targets the reserved control-plane tenant so
    /// Charo's identity can't be edited, suspended, or deleted via the admin API.
    fn guard_reserved_tenant(id: Uuid) -> Result<()> {
        if id == Self::CONTROL_PLANE_TENANT_ID {
            return Err(StoreError::Protected(
                "the reserved control-plane tenant cannot be modified".into(),
            ));
        }
        Ok(())
    }

    /// Reject a mutation that targets a key owned by the reserved control-plane
    /// tenant. A missing key is left to the method's own NotFound handling.
    async fn guard_reserved_key(&self, key_id: Uuid) -> Result<()> {
        let row = sqlx::query("select tenant_id from api_keys where id = $1")
            .bind(key_id)
            .fetch_optional(&self.pool)
            .await?;
        if let Some(row) = row {
            let tenant_id: Uuid = row.try_get("tenant_id")?;
            if tenant_id == Self::CONTROL_PLANE_TENANT_ID {
                return Err(StoreError::Protected(
                    "the reserved control-plane key cannot be modified".into(),
                ));
            }
        }
        Ok(())
    }

    pub async fn list_keys(&self, tenant_id: Option<Uuid>) -> Result<Vec<ApiKey>> {
        let rows = match tenant_id {
            Some(t) => {
                sqlx::query(
                    "select id, tenant_id, name, description, key_prefix,
                            budget_tokens, budget_cost_usd, budget_period, budget_started_at,
                            disabled, tracing_enabled, created_at, updated_at
                     from api_keys where tenant_id = $1 order by created_at",
                )
                .bind(t)
                .fetch_all(&self.pool)
                .await?
            }
            None => {
                sqlx::query(
                    "select id, tenant_id, name, description, key_prefix,
                            budget_tokens, budget_cost_usd, budget_period, budget_started_at,
                            disabled, tracing_enabled, created_at, updated_at
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
            "select id, tenant_id, name, description, key_prefix,
                    budget_tokens, budget_cost_usd, budget_period, budget_started_at,
                    disabled, tracing_enabled, created_at, updated_at
             from api_keys where id = any($1)",
        )
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(api_key_from_row).collect()
    }

    pub async fn update_api_key(
        &self,
        id: Uuid,
        name: &str,
        description: &str,
        budget_tokens: Option<i64>,
        budget_cost_usd: Option<f64>,
        budget_period: Option<&str>,
        budget_started_at: Option<DateTime<Utc>>,
    ) -> Result<(String, ApiKey, ResolvedKey)> {
        self.guard_reserved_key(id).await?;
        let row = sqlx::query(
            "update api_keys
             set name = $2,
                 description = $3,
                 budget_tokens = $4,
                 budget_cost_usd = $5,
                 budget_period = $6,
                 budget_started_at = $7,
                 updated_at = now()
             where id = $1
             returning key_hash, id, tenant_id, name, description, key_prefix,
                    budget_tokens, budget_cost_usd, budget_period, budget_started_at,
                    disabled, tracing_enabled, created_at, updated_at",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(budget_tokens)
        .bind(budget_cost_usd)
        .bind(budget_period)
        .bind(budget_started_at)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        let hash: String = row.try_get("key_hash")?;
        let key = api_key_from_row(&row)?;
        let resolved = self
            .resolved_key_by_hash(&hash)
            .await?
            .ok_or(StoreError::NotFound)?;
        Ok((hash, key, resolved))
    }

    pub async fn set_key_disabled(
        &self,
        id: Uuid,
        disabled: bool,
    ) -> Result<(String, ResolvedKey)> {
        self.guard_reserved_key(id).await?;
        let row = sqlx::query(
            "update api_keys set disabled = $2, updated_at = now() where id = $1 returning key_hash",
        )
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

    pub async fn set_key_tracing(
        &self,
        id: Uuid,
        tracing_enabled: bool,
    ) -> Result<(String, ResolvedKey)> {
        self.guard_reserved_key(id).await?;
        let row = sqlx::query(
            "update api_keys set tracing_enabled = $2, updated_at = now() \
             where id = $1 returning key_hash",
        )
        .bind(id)
        .bind(tracing_enabled)
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

    pub async fn set_tenant_tracing(&self, id: Uuid, tracing_enabled: bool) -> Result<()> {
        Self::guard_reserved_tenant(id)?;
        sqlx::query(
            "update tenants set tracing_enabled = $2, updated_at = now() where id = $1 returning id",
        )
        .bind(id)
        .bind(tracing_enabled)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        Ok(())
    }

    pub async fn delete_key(&self, id: Uuid) -> Result<String> {
        self.guard_reserved_key(id).await?;
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
                    t.budget_tokens, t.budget_cost_usd, t.budget_period, t.budget_started_at,
                    k.budget_tokens as key_budget_tokens,
                    k.budget_cost_usd as key_budget_cost_usd,
                    k.budget_period as key_budget_period,
                    k.budget_started_at as key_budget_started_at,
                    t.allowed_models,
                    (k.tracing_enabled OR t.tracing_enabled) AS tracing_enabled,
                    t.guardrails_policy
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
                    t.budget_tokens, t.budget_cost_usd, t.budget_period, t.budget_started_at,
                    k.budget_tokens as key_budget_tokens,
                    k.budget_cost_usd as key_budget_cost_usd,
                    k.budget_period as key_budget_period,
                    k.budget_started_at as key_budget_started_at,
                    t.allowed_models,
                    (k.tracing_enabled OR t.tracing_enabled) AS tracing_enabled,
                    t.guardrails_policy
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
                    t.budget_tokens, t.budget_cost_usd, t.budget_period, t.budget_started_at,
                    k.budget_tokens as key_budget_tokens,
                    k.budget_cost_usd as key_budget_cost_usd,
                    k.budget_period as key_budget_period,
                    k.budget_started_at as key_budget_started_at,
                    t.allowed_models,
                    (k.tracing_enabled OR t.tracing_enabled) AS tracing_enabled,
                    t.guardrails_policy
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
        supports_vision: bool,
        tags: &[String],
        boons: &[String],
        tool_servers: &[String],
    ) -> Result<ModelRoute> {
        let api_key = cipher().encrypt_opt(api_key);
        let row = sqlx::query(
            "insert into models (
                id, model_name, description, upstream_model, api_base, api_key, model_type,
                input_cost_per_token, output_cost_per_token,
                cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                supports_response_schema, supports_tool_choice, supports_vision, tags, boons, tool_servers
             ) values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23)
             returning id, model_name, description, upstream_model, api_base, api_key, model_type,
                       input_cost_per_token, output_cost_per_token,
                       cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                       admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                       supports_response_schema, supports_tool_choice, supports_vision, enabled,
                       cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                       capacity_mode, capacity_tuned_at,
                       debug_diagnostics,
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
        .bind(supports_vision)
        .bind(sqlx::types::Json(obleth_config::normalize_tags(tags)))
        .bind(sqlx::types::Json(obleth_config::normalize_boons(boons)))
        .bind(sqlx::types::Json(obleth_config::normalize_tool_servers(
            tool_servers,
        )))
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
                    supports_response_schema, supports_tool_choice, supports_vision, enabled,
                    cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                    capacity_mode, capacity_tuned_at,
                    request_timeout_secs, max_retries, retry_backoff_ms, endpoint_selection_mode,
                    debug_diagnostics,
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
                    supports_response_schema, supports_tool_choice, supports_vision, enabled,
                    cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                    capacity_mode, capacity_tuned_at,
                    request_timeout_secs, max_retries, retry_backoff_ms, endpoint_selection_mode,
                    debug_diagnostics,
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
                    supports_response_schema, supports_tool_choice, supports_vision, enabled,
                    cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                    capacity_mode, capacity_tuned_at,
                    request_timeout_secs, max_retries, retry_backoff_ms, endpoint_selection_mode,
                    debug_diagnostics,
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
        supports_vision: bool,
        enabled: bool,
        tags: &[String],
        boons: &[String],
        tool_servers: &[String],
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
                supports_vision = $21, boons = $22, tool_servers = $23,
                updated_at = now()
             where id = $1
             returning id, model_name, description, upstream_model, api_base, api_key, model_type,
                       input_cost_per_token, output_cost_per_token,
                       cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                       admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                       supports_response_schema, supports_tool_choice, supports_vision, enabled,
                       cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                       capacity_mode, capacity_tuned_at,
                       debug_diagnostics,
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
        .bind(supports_vision)
        .bind(sqlx::types::Json(obleth_config::normalize_boons(boons)))
        .bind(sqlx::types::Json(obleth_config::normalize_tool_servers(
            tool_servers,
        )))
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
                       supports_response_schema, supports_tool_choice, supports_vision, enabled,
                       cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                       capacity_mode, capacity_tuned_at,
                       debug_diagnostics,
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
                       supports_response_schema, supports_tool_choice, supports_vision, enabled,
                       cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                       capacity_mode, capacity_tuned_at,
                       debug_diagnostics,
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
                       supports_response_schema, supports_tool_choice, supports_vision, enabled,
                       cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                       capacity_mode, capacity_tuned_at,
                       debug_diagnostics,
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
                       supports_response_schema, supports_tool_choice, supports_vision, enabled,
                       cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                       capacity_mode, capacity_tuned_at,
                       debug_diagnostics,
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
                    supports_response_schema, supports_tool_choice, supports_vision, tags, boons, tool_servers,
                    request_timeout_secs, max_retries, retry_backoff_ms, endpoint_selection_mode,
                    debug_diagnostics
             from models where enabled = true",
        )
        .fetch_all(&self.pool)
        .await?;
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        // Fetch every endpoint in one batched query instead of one per model.
        let model_ids: Vec<Uuid> = rows
            .iter()
            .map(|r| r.try_get("id"))
            .collect::<std::result::Result<_, _>>()?;
        let endpoint_rows = sqlx::query(
            "select model_id, id, api_base, api_key, priority, weight, enabled, health_status
             from model_endpoints
             where model_id = any($1)
             order by priority asc, created_at asc",
        )
        .bind(&model_ids)
        .fetch_all(&self.pool)
        .await?;
        let mut endpoints_by_model: HashMap<Uuid, Vec<ResolvedEndpoint>> = HashMap::new();
        for row in &endpoint_rows {
            endpoints_by_model
                .entry(row.try_get("model_id")?)
                .or_default()
                .push(resolved_endpoint_from_row(row)?);
        }
        let mut out = Vec::with_capacity(rows.len());
        for row in &rows {
            let name: String = row.try_get("model_name")?;
            let model_id: Uuid = row.try_get("id")?;
            let endpoints = endpoints_by_model.remove(&model_id).unwrap_or_default();
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
                    supports_vision: row.try_get("supports_vision").unwrap_or(false),
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
                    request_timeout_secs: row.try_get("request_timeout_secs")?,
                    max_retries: row.try_get("max_retries")?,
                    retry_backoff_ms: row.try_get("retry_backoff_ms")?,
                    endpoint_selection_mode: row.try_get("endpoint_selection_mode")?,
                    debug_diagnostics: row.try_get("debug_diagnostics").unwrap_or(false),
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
                       supports_response_schema, supports_tool_choice, supports_vision, enabled,
                       cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                       capacity_mode, capacity_tuned_at,
                       debug_diagnostics,
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
        debug_diagnostics: bool,
    ) -> Result<ModelRoute> {
        let row = sqlx::query(
            "update models set request_timeout_secs = $2, max_retries = $3,
                    retry_backoff_ms = $4, endpoint_selection_mode = $5,
                    debug_diagnostics = $6, updated_at = now()
             where id = $1
             returning id, model_name, description, upstream_model, api_base, api_key, model_type,
                       input_cost_per_token, output_cost_per_token,
                       cost_per_image, cost_per_audio_second, cost_per_character, context_window,
                       admission_weight, max_in_flight, supports_function_calling, supports_system_messages,
                       supports_response_schema, supports_tool_choice, supports_vision, enabled,
                       cache_enabled, cache_ttl_secs, tags, boons, tool_servers,
                       capacity_mode, capacity_tuned_at,
                       request_timeout_secs, max_retries, retry_backoff_ms, endpoint_selection_mode,
                       debug_diagnostics,
                       created_at, updated_at",
        )
        .bind(id)
        .bind(request_timeout_secs.filter(|n| *n >= 1))
        .bind(max_retries.max(0))
        .bind(retry_backoff_ms.max(0))
        .bind(obleth_config::normalize_endpoint_selection_mode(
            endpoint_selection_mode,
        ))
        .bind(debug_diagnostics)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        model_from_row(&row)
    }

    // ---- model endpoints -------------------------------------------------

    /// Hot-path endpoint views for one model: enabled endpoints with their
    /// decrypted upstream keys and current health, ordered by priority.
    pub async fn resolved_endpoints_for(&self, model_id: Uuid) -> Result<Vec<ResolvedEndpoint>> {
        let rows = sqlx::query(
            "select id, api_base, api_key, priority, weight, enabled, health_status
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
        let model_id: Uuid = row.try_get("model_id")?;
        // Removing an endpoint changes the model's live pool; re-probe promptly so
        // the model-level aggregate doesn't lag a full interval behind.
        self.mark_model_health_due(model_id).await;
        Ok(model_id)
    }

    // ---- managed_models (Slurm provisioning specs) -----------------------

    pub async fn get_managed_model(&self, model_id: Uuid) -> Result<Option<ManagedModelSpec>> {
        let row = sqlx::query(
            "select model_id, enabled, partition, gres, nodes, constraints, exclude, account, \
             qos, time_limit, cpus_per_task, mem, image, preamble, log_output_dir, launch_command, \
             script_body, serving_port, \
             health_path, target_replicas, min_replicas, max_job_failures, launcher_spec, \
             last_provision_error, last_provision_error_at, created_at, updated_at \
             from managed_models where model_id = $1",
        )
        .bind(model_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(managed_model_from_row).transpose()
    }

    /// Just the health floor for a managed model. Used on the health-check hot
    /// path so it doesn't fetch + deserialize the full spec (script_body,
    /// launcher_spec JSON, ...) only to read one scalar.
    pub async fn get_managed_min_replicas(&self, model_id: Uuid) -> Result<Option<i64>> {
        let v = sqlx::query_scalar::<_, i64>(
            "select min_replicas from managed_models where model_id = $1",
        )
        .bind(model_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(v)
    }

    pub async fn list_managed_models(&self) -> Result<Vec<ManagedModelSpec>> {
        let rows = sqlx::query(
            "select model_id, enabled, partition, gres, nodes, constraints, exclude, account, \
             qos, time_limit, cpus_per_task, mem, image, preamble, log_output_dir, launch_command, \
             script_body, serving_port, \
             health_path, target_replicas, min_replicas, max_job_failures, launcher_spec, \
             last_provision_error, last_provision_error_at, created_at, updated_at \
             from managed_models order by model_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(managed_model_from_row).collect()
    }

    pub async fn upsert_managed_model(&self, m: UpsertManagedModel) -> Result<ManagedModelSpec> {
        let row = sqlx::query(
            "insert into managed_models
                (model_id, enabled, partition, gres, nodes, constraints, exclude,
                 account, qos, time_limit, cpus_per_task, mem, image, preamble, log_output_dir,
                 launch_command, script_body,
                 serving_port, health_path, target_replicas, min_replicas, max_job_failures, launcher_spec)
             values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23)
             on conflict (model_id) do update set
                enabled = excluded.enabled, partition = excluded.partition,
                gres = excluded.gres, nodes = excluded.nodes,
                constraints = excluded.constraints, exclude = excluded.exclude,
                account = excluded.account, qos = excluded.qos,
                time_limit = excluded.time_limit, cpus_per_task = excluded.cpus_per_task,
                mem = excluded.mem, image = excluded.image,
                preamble = excluded.preamble, log_output_dir = excluded.log_output_dir,
                launch_command = excluded.launch_command, script_body = excluded.script_body,
                serving_port = excluded.serving_port, health_path = excluded.health_path,
                target_replicas = excluded.target_replicas,
                min_replicas = excluded.min_replicas,
                max_job_failures = excluded.max_job_failures,
                launcher_spec = excluded.launcher_spec, updated_at = now()
             returning model_id, enabled, partition, gres, nodes, constraints, exclude, account, \
             qos, time_limit, cpus_per_task, mem, image, preamble, log_output_dir, launch_command, \
             script_body, serving_port, \
             health_path, target_replicas, min_replicas, max_job_failures, launcher_spec, \
             last_provision_error, last_provision_error_at, created_at, updated_at",
        )
        .bind(m.model_id)
        .bind(m.enabled)
        .bind(&m.partition)
        .bind(&m.gres)
        .bind(m.nodes.max(1))
        .bind(&m.constraints)
        .bind(&m.exclude)
        .bind(&m.account)
        .bind(&m.qos)
        .bind(&m.time_limit)
        .bind(m.cpus_per_task)
        .bind(&m.mem)
        .bind(&m.image)
        .bind(&m.preamble)
        .bind(&m.log_output_dir)
        .bind(&m.launch_command)
        .bind(&m.script_body)
        .bind(m.serving_port)
        .bind(&m.health_path)
        .bind(m.target_replicas.max(1))
        // The health floor cannot exceed the count the reconciler submits toward;
        // otherwise the model could never reach `min_replicas` healthy and would
        // be wedged below its floor forever. Clamp to [1, target_replicas].
        .bind(m.min_replicas.clamp(1, m.target_replicas.max(1)))
        .bind(m.max_job_failures.max(0))
        .bind(m.launcher_spec.as_ref().map(sqlx::types::Json))
        .fetch_one(&self.pool)
        .await?;
        managed_model_from_row(&row)
    }

    pub async fn delete_managed_model(&self, model_id: Uuid) -> Result<()> {
        sqlx::query("delete from managed_models where model_id = $1")
            .bind(model_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Record (or clear) the provisioner's last submit error for a model. Sets
    /// the timestamp to now() when storing an error; both columns to NULL when
    /// clearing. No-op if the model has no managed_models row.
    pub async fn set_provision_error(&self, model_id: Uuid, error: Option<&str>) -> Result<()> {
        sqlx::query(
            "update managed_models \
             set last_provision_error = $2, \
                 last_provision_error_at = case when $2 is null then null else now() end, \
                 updated_at = now() \
             where model_id = $1",
        )
        .bind(model_id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- recipes ---------------------------------------------------------

    pub async fn list_recipes(&self) -> Result<Vec<Recipe>> {
        let rows = sqlx::query(
            "select id, name, body, author, created_at, updated_at
             from recipes order by updated_at desc",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(recipe_from_row).collect()
    }

    pub async fn get_recipe(&self, id: Uuid) -> Result<Option<Recipe>> {
        let row = sqlx::query(
            "select id, name, body, author, created_at, updated_at
             from recipes where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(recipe_from_row).transpose()
    }

    pub async fn upsert_recipe(&self, r: UpsertRecipe) -> Result<Recipe> {
        let id = r.id.unwrap_or_else(Uuid::new_v4);
        let row = sqlx::query(
            "insert into recipes (id, name, body, author)
             values ($1,$2,$3,$4)
             on conflict (id) do update set
               name=excluded.name, body=excluded.body,
               author=excluded.author, updated_at=now()
             returning id, name, body, author, created_at, updated_at",
        )
        .bind(id)
        .bind(&r.name)
        .bind(&r.body)
        .bind(&r.author)
        .fetch_one(&self.pool)
        .await?;
        recipe_from_row(&row)
    }

    pub async fn delete_recipe(&self, id: Uuid) -> Result<()> {
        sqlx::query("delete from recipes where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn create_replica(
        &self,
        model_id: Uuid,
        slurm_job_id: &str,
        port_base: Option<i64>,
    ) -> Result<ModelReplica> {
        // Idempotent: a retried insert for the same (model_id, slurm_job_id)
        // returns the existing row instead of erroring on the unique index.
        let row = sqlx::query(
            "insert into model_replicas (id, model_id, slurm_job_id, port_base)
             values ($1, $2, $3, $4)
             on conflict (model_id, slurm_job_id) do update set updated_at = now()
             returning id, model_id, slurm_job_id, nodes, endpoint_id, state,
                       last_message, port_base, cancel_requested, created_at, updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(model_id)
        .bind(slurm_job_id)
        .bind(port_base)
        .fetch_one(&self.pool)
        .await?;
        replica_from_row(&row)
    }

    pub async fn list_replicas(&self, model_id: Uuid) -> Result<Vec<ModelReplica>> {
        let rows = sqlx::query(
            "select id, model_id, slurm_job_id, nodes, endpoint_id, state,
                    last_message, port_base, cancel_requested, created_at, updated_at
             from model_replicas where model_id = $1 order by created_at",
        )
        .bind(model_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(replica_from_row).collect()
    }

    pub async fn all_replicas(&self) -> Result<Vec<ModelReplica>> {
        let rows = sqlx::query(
            "select id, model_id, slurm_job_id, nodes, endpoint_id, state,
                    last_message, port_base, cancel_requested, created_at, updated_at
             from model_replicas order by model_id, created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(replica_from_row).collect()
    }

    pub async fn update_replica_state(
        &self,
        id: Uuid,
        state: &str,
        message: Option<&str>,
    ) -> Result<ModelReplica> {
        let row = sqlx::query(
            "update model_replicas set state = $2, last_message = $3, updated_at = now()
             where id = $1
             returning id, model_id, slurm_job_id, nodes, endpoint_id, state,
                       last_message, port_base, cancel_requested, created_at, updated_at",
        )
        .bind(id)
        .bind(state)
        .bind(message)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        replica_from_row(&row)
    }

    /// Set the allocated nodes and/or linked endpoint for a replica.
    ///
    /// A `None` argument keeps the current column value instead of overwriting
    /// it with NULL (COALESCE semantics). Pass `Some(value)` to update a field,
    /// `None` to leave it unchanged.
    pub async fn set_replica_runtime(
        &self,
        id: Uuid,
        nodes: Option<&str>,
        endpoint_id: Option<Uuid>,
    ) -> Result<ModelReplica> {
        let row = sqlx::query(
            "update model_replicas set nodes = coalesce($2, nodes),
                    endpoint_id = coalesce($3, endpoint_id), updated_at = now()
             where id = $1
             returning id, model_id, slurm_job_id, nodes, endpoint_id, state,
                       last_message, port_base, cancel_requested, created_at, updated_at",
        )
        .bind(id)
        .bind(nodes)
        .bind(endpoint_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        replica_from_row(&row)
    }

    /// Update just a replica's last_message (used by the provisioner's live-status
    /// annotation, which carries no state/runtime change). Returns the updated row.
    pub async fn set_replica_message(&self, id: Uuid, message: &str) -> Result<ModelReplica> {
        let row = sqlx::query(
            "update model_replicas set last_message = $2, updated_at = now()
             where id = $1
             returning id, model_id, slurm_job_id, nodes, endpoint_id, state,
                       last_message, port_base, cancel_requested, created_at, updated_at",
        )
        .bind(id)
        .bind(message)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        replica_from_row(&row)
    }

    /// Flag a replica for restart: the provisioner cancels its Slurm job on the
    /// next tick and resubmit-to-target launches a fresh one. Returns `false`
    /// when no replica with that id exists, so the caller can surface a 404.
    pub async fn request_replica_cancel(&self, id: Uuid) -> Result<bool> {
        let res = sqlx::query(
            "update model_replicas set cancel_requested = true, updated_at = now() where id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Fetch a single replica by id. Returns `None` if no row exists.
    pub async fn get_replica(&self, id: Uuid) -> Result<Option<ModelReplica>> {
        let row = sqlx::query(
            "select id, model_id, slurm_job_id, nodes, endpoint_id, state,
                    last_message, port_base, cancel_requested, created_at, updated_at
             from model_replicas where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(replica_from_row).transpose()
    }

    pub async fn delete_replica(&self, id: Uuid) -> Result<()> {
        sqlx::query("delete from model_replicas where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn delete_lost_replicas(&self, model_id: Uuid) -> Result<u64> {
        let r = sqlx::query("delete from model_replicas where model_id = $1 and state = 'lost'")
            .bind(model_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
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
        // `skipped` is "we didn't really probe" -> never touch the stored status.
        // `degraded` is "reachable, but couldn't confirm the model id" -> it's a
        // real liveness signal, and the router treats degraded as routable. So a
        // degraded probe must lift a stale `unhealthy` (otherwise a steady-state
        // degraded endpoint — e.g. an id-format mismatch in /v1/models — can never
        // recover after one transient unhealthy read, and stays out of rotation
        // until a manual check). It still must not downgrade a confirmed `healthy`.
        let row = sqlx::query(
            "update model_endpoints set
                health_status = case
                    when $2 = 'skipped' then health_status
                    when $2 = 'degraded' then
                        case when health_status = 'unhealthy' then 'degraded' else health_status end
                    else $2 end,
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

        let previous_status: String = current.try_get("health_status")?;
        // The displayed badge is threshold-gated, matching the alerting logic: a
        // sub-threshold failure streak holds the last stable badge instead of
        // flapping to "down". Healthy/disabled/transient outcomes display as-is.
        let displayed_status = if status == "healthy" || status == "disabled" || transient {
            status
        } else if new_failures >= failure_threshold.max(1) {
            status // confirmed down
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

    /// Load the persisted model-"boons" settings, or `None` if unset.
    pub async fn get_boon_settings(&self) -> Result<Option<obleth_config::BoonSettings>> {
        let row = sqlx::query("select value from app_settings where key = 'boons'")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => {
                let value: sqlx::types::Json<obleth_config::BoonSettings> = row.try_get("value")?;
                Ok(Some(value.0))
            }
            None => Ok(None),
        }
    }

    /// Persist the model-"boons" settings (upsert on the single `boons` key).
    pub async fn put_boon_settings(&self, settings: &obleth_config::BoonSettings) -> Result<()> {
        sqlx::query(
            "insert into app_settings (key, value, updated_at)
             values ('boons', $1, now())
             on conflict (key) do update set value = excluded.value, updated_at = now()",
        )
        .bind(sqlx::types::Json(settings))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Whether the Charo control-plane assistant is enabled in the dashboard UI.
    /// `None` means unset (callers should treat this as enabled by default).
    pub async fn get_charo_enabled(&self) -> Result<Option<bool>> {
        let row = sqlx::query("select value from app_settings where key = 'charo_enabled'")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => {
                let value: sqlx::types::Json<bool> = row.try_get("value")?;
                Ok(Some(value.0))
            }
            None => Ok(None),
        }
    }

    /// Persist the Charo assistant UI toggle (upsert on the `charo_enabled` key).
    pub async fn set_charo_enabled(&self, enabled: bool) -> Result<()> {
        sqlx::query(
            "insert into app_settings (key, value, updated_at)
             values ('charo_enabled', $1, now())
             on conflict (key) do update set value = excluded.value, updated_at = now()",
        )
        .bind(sqlx::types::Json(enabled))
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

    /// Load the system-wide Slurm settings, or `None` if never configured. The
    /// stored JWT is decrypted transparently (legacy/empty values pass through).
    pub async fn get_slurm_settings(&self) -> Result<Option<obleth_config::SlurmSettings>> {
        let row = sqlx::query("select value from app_settings where key = 'slurm'")
            .fetch_optional(&self.pool)
            .await?;
        match row {
            Some(row) => {
                let value: sqlx::types::Json<obleth_config::SlurmSettings> =
                    row.try_get("value")?;
                let mut settings = value.0;
                if !settings.slurm_jwt.is_empty() {
                    settings.slurm_jwt = cipher().decrypt(&settings.slurm_jwt)?;
                }
                Ok(Some(settings))
            }
            None => Ok(None),
        }
    }

    /// Persist the system-wide Slurm settings (upsert on the single `slurm`
    /// key). The JWT is encrypted at rest with the same envelope cipher used for
    /// upstream provider keys before it is written.
    pub async fn put_slurm_settings(&self, settings: &obleth_config::SlurmSettings) -> Result<()> {
        let mut to_store = settings.clone();
        if !to_store.slurm_jwt.is_empty() {
            to_store.slurm_jwt = cipher().encrypt(&to_store.slurm_jwt);
        }
        sqlx::query(
            "insert into app_settings (key, value, updated_at)
             values ('slurm', $1, now())
             on conflict (key) do update set value = excluded.value, updated_at = now()",
        )
        .bind(sqlx::types::Json(&to_store))
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
        tracing_enabled: row.try_get("tracing_enabled").unwrap_or(false),
        guardrails_policy: guardrails_policy_from_row(row)?,
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

/// Decode the optional `guardrails_policy` jsonb column. A decode failure (e.g.
/// a malformed policy written out of band) is logged and treated as no policy
/// rather than failing the whole row — consistent with the boon's fail-open
/// posture, and surfaced via the warning so it can be investigated.
fn guardrails_policy_from_row(row: &PgRow) -> Result<Option<obleth_config::GuardrailsPolicy>> {
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
        description: row.try_get("description")?,
        key_prefix: row.try_get("key_prefix")?,
        budget_tokens: row.try_get("budget_tokens")?,
        budget_cost_usd: row.try_get("budget_cost_usd")?,
        budget_period: row.try_get("budget_period")?,
        budget_started_at: row.try_get("budget_started_at")?,
        disabled: row.try_get("disabled")?,
        tracing_enabled: row.try_get("tracing_enabled").unwrap_or(false),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
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
        key_budget_tokens: row.try_get("key_budget_tokens")?,
        key_budget_cost_usd: row.try_get("key_budget_cost_usd")?,
        key_budget_period: row.try_get("key_budget_period")?,
        key_budget_started_at: row.try_get("key_budget_started_at")?,
        allowed_models: allowed_models_from_row(row)?,
        internal: false,
        tracing_enabled: row.try_get::<bool, _>("tracing_enabled").unwrap_or(false),
        guardrails_policy: guardrails_policy_from_row(row)?,
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
        // Tolerant read: statements that don't select `supports_vision` (e.g.
        // older callers) degrade to false rather than erroring.
        supports_vision: row.try_get("supports_vision").unwrap_or(false),
        enabled: row.try_get("enabled")?,
        cache_enabled: row.try_get("cache_enabled")?,
        cache_ttl_secs: row.try_get("cache_ttl_secs")?,
        // Tolerant read: SQL statements that don't select `tags` (e.g. capacity
        // toggles) degrade to an empty list rather than erroring.
        tags: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("tags")
            .map(|j| j.0)
            .unwrap_or_default(),
        // Tolerant read: statements that don't select `boons` degrade to an
        // empty list rather than erroring.
        boons: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("boons")
            .map(|j| j.0)
            .unwrap_or_default(),
        // Tolerant read: statements that don't select `tool_servers` degrade
        // to an empty list rather than erroring.
        tool_servers: row
            .try_get::<sqlx::types::Json<Vec<String>>, _>("tool_servers")
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
        // Tolerant read: statements that don't select `debug_diagnostics`
        // degrade to false rather than erroring.
        debug_diagnostics: row.try_get("debug_diagnostics").unwrap_or(false),
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn resolved_endpoint_from_row(row: &PgRow) -> Result<ResolvedEndpoint> {
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
    })
}

fn recipe_from_row(row: &PgRow) -> Result<Recipe> {
    Ok(Recipe {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        body: row.try_get("body")?,
        author: row.try_get("author")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn managed_model_from_row(row: &PgRow) -> Result<ManagedModelSpec> {
    let launcher_spec: Option<sqlx::types::Json<serde_json::Value>> =
        row.try_get("launcher_spec")?;
    Ok(ManagedModelSpec {
        model_id: row.try_get("model_id")?,
        enabled: row.try_get("enabled")?,
        partition: row.try_get("partition")?,
        gres: row.try_get("gres")?,
        nodes: row.try_get("nodes")?,
        constraints: row.try_get("constraints")?,
        exclude: row.try_get("exclude")?,
        account: row.try_get("account")?,
        qos: row.try_get("qos")?,
        time_limit: row.try_get("time_limit")?,
        cpus_per_task: row.try_get("cpus_per_task")?,
        mem: row.try_get("mem")?,
        image: row.try_get("image")?,
        preamble: row.try_get("preamble")?,
        log_output_dir: row.try_get("log_output_dir")?,
        launch_command: row.try_get("launch_command")?,
        script_body: row.try_get("script_body")?,
        serving_port: row.try_get("serving_port")?,
        health_path: row.try_get("health_path")?,
        target_replicas: row.try_get("target_replicas")?,
        min_replicas: row.try_get("min_replicas")?,
        max_job_failures: row.try_get("max_job_failures")?,
        launcher_spec: launcher_spec.map(|j| j.0),
        last_provision_error: row.try_get("last_provision_error")?,
        last_provision_error_at: row.try_get("last_provision_error_at")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn replica_from_row(row: &PgRow) -> Result<ModelReplica> {
    Ok(ModelReplica {
        id: row.try_get("id")?,
        model_id: row.try_get("model_id")?,
        slurm_job_id: row.try_get("slurm_job_id")?,
        nodes: row.try_get("nodes")?,
        endpoint_id: row.try_get("endpoint_id")?,
        state: row.try_get("state")?,
        last_message: row.try_get("last_message")?,
        port_base: row.try_get("port_base")?,
        cancel_requested: row.try_get("cancel_requested")?,
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

/// Shared integration-test plumbing, available to every test module in the
/// crate (`lib.rs` `tests` and `backup.rs` `tests`).
#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    /// Reads OBLETH_TEST_DATABASE_URL for an integration test. Returns None when
    /// unset (the test skips). Panics if it IS set but the database name doesn't
    /// contain "test" — refuses to run the integration suite against a possibly
    /// real/dev database, which is how fixtures previously leaked.
    pub(crate) fn test_db_url() -> Option<String> {
        let url = std::env::var("OBLETH_TEST_DATABASE_URL").ok()?;
        let db = url
            .rsplit('/')
            .next()
            .unwrap_or("")
            .split('?')
            .next()
            .unwrap_or("");
        assert!(
            db.contains("test"),
            "OBLETH_TEST_DATABASE_URL database name {db:?} is not a dedicated test DB \
             (name must contain \"test\", e.g. obleth_test). Refusing to run integration \
             tests against a possibly-real database."
        );
        Some(url)
    }

    /// Tracks the fixture rows a test creates and deletes them when it drops —
    /// which happens even if the test panics on a failed assertion — so leaked
    /// `m-…`/`t-…` rows (and the replica rows they leave behind) never persist
    /// in the target database. Deleting a model cascades to its health
    /// checks/endpoints/managed row, and deleting a tenant cascades to its
    /// keys, so model + tenant ids are enough for those. Replica rows do NOT
    /// cascade from a model delete (the provisioner needs orphan rows to drain
    /// Slurm jobs), so they are tracked and deleted separately.
    ///
    /// Cleanup runs async DB deletes from `Drop`, which requires the test to use
    /// the multi-thread runtime flavor (so `block_in_place` is permitted).
    pub(crate) struct FixtureGuard {
        store: Store,
        models: Vec<Uuid>,
        tenants: Vec<Uuid>,
        replicas: Vec<Uuid>,
    }

    impl FixtureGuard {
        pub(crate) fn new(store: &Store) -> Self {
            Self {
                store: store.clone(),
                models: Vec::new(),
                tenants: Vec::new(),
                replicas: Vec::new(),
            }
        }
        pub(crate) fn track_model(&mut self, id: Uuid) {
            self.models.push(id);
        }
        pub(crate) fn track_tenant(&mut self, id: Uuid) {
            self.tenants.push(id);
        }
        pub(crate) fn track_replica(&mut self, id: Uuid) {
            self.replicas.push(id);
        }
    }

    impl Drop for FixtureGuard {
        fn drop(&mut self) {
            let store = self.store.clone();
            let models = std::mem::take(&mut self.models);
            let tenants = std::mem::take(&mut self.tenants);
            let replicas = std::mem::take(&mut self.replicas);
            if models.is_empty() && tenants.is_empty() && replicas.is_empty() {
                return;
            }
            // Deletes are best-effort: a row a test already removed returns
            // NotFound, which we ignore. Replica order vs models is irrelevant
            // since the model_replicas FK was dropped. block_in_place needs the
            // multi-thread flavor on the test.
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    for id in models {
                        let _ = store.delete_model(id).await;
                    }
                    for id in tenants {
                        let _ = store.delete_tenant(id).await;
                    }
                    for id in replicas {
                        let _ = store.delete_replica(id).await;
                    }
                });
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obleth_config::hash_api_key;
    use std::sync::OnceLock;

    use super::test_support::FixtureGuard;

    // Serialise integration tests so DDL from migrate() never races with DML
    // from a concurrently running test. Each test must hold this guard for its
    // full duration (including the migrate() call).
    static SERIAL: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();
    fn serial() -> &'static tokio::sync::Mutex<()> {
        SERIAL.get_or_init(tokio::sync::Mutex::default)
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` points at a
    /// throwaway Postgres. Skips silently otherwise so unit runs stay hermetic.
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
            .create_api_key(tenant.id, "k", "", None, None, None, None)
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
                false,
                &[],
                &[],
                &[],
            )
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
            .create_model(
                &format!("m-{}", Uuid::new_v4()),
                "hysteresis test model",
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
                &[],
                &[],
                &[],
            )
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
            .create_model(
                &format!("m-{}", Uuid::new_v4()),
                "endpoint health test model",
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
                &[],
                &[],
                &[],
            )
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

    fn default_test_model(
        name: &str,
    ) -> (
        &str,
        &str,
        &str,
        &str,
        Option<&str>,
        &str,
        f64,
        f64,
        f64,
        f64,
        f64,
        i64,
        i64,
        Option<i64>,
        bool,
        bool,
        bool,
        bool,
        bool,
        Vec<String>,
        Vec<String>,
        Vec<String>,
    ) {
        (
            name,
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
            false,
            vec![],
            vec![],
            vec![],
        )
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
            store.set_key_disabled(resolved.key_id, true).await,
            Err(StoreError::Protected(_))
        ));
        assert!(matches!(
            store.delete_key(resolved.key_id).await,
            Err(StoreError::Protected(_))
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn managed_model_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        // a model is required (FK). Reuse create_model with minimal args.
        let model_name = format!("m-{}", Uuid::new_v4());
        let args = default_test_model(&model_name);
        let model = store
            .create_model(
                args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7, args.8, args.9,
                args.10, args.11, args.12, args.13, args.14, args.15, args.16, args.17, args.18,
                &args.19, &args.20, &args.21,
            )
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        assert!(store
            .get_managed_model(model.id)
            .await
            .expect("get")
            .is_none());

        let spec = store
            .upsert_managed_model(UpsertManagedModel {
                model_id: model.id,
                enabled: true,
                partition: "gpu-preempt".into(),
                gres: "gpu:h100:2".into(),
                nodes: 1,
                constraints: None,
                exclude: None,
                account: None,
                qos: None,
                time_limit: Some("12:00:00".into()),
                cpus_per_task: None,
                mem: None,
                image: "vllm.sif".into(),
                preamble: String::new(),
                log_output_dir: String::new(),
                launch_command: "vllm serve nemotron".into(),
                script_body: String::new(),
                serving_port: 8000,
                health_path: "/health".into(),
                target_replicas: 2,
                min_replicas: 1,
                max_job_failures: 0,
                launcher_spec: Some(serde_json::json!({"backendId":"llamacpp"})),
            })
            .await
            .expect("upsert");
        assert_eq!(spec.target_replicas, 2);
        assert_eq!(spec.min_replicas, 1);
        assert_eq!(spec.partition, "gpu-preempt");
        assert_eq!(
            spec.launcher_spec,
            Some(serde_json::json!({"backendId":"llamacpp"}))
        );

        // Assert membership, not a global count — other tests share this DB and
        // may have their own managed models, so an exact count is flaky in CI.
        let listed = store.list_managed_models().await.expect("list");
        assert!(listed.iter().any(|m| m.model_id == model.id));

        store.delete_managed_model(model.id).await.expect("delete");
        assert!(store
            .get_managed_model(model.id)
            .await
            .expect("get")
            .is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn managed_model_provision_error_set_and_clear() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let args = default_test_model(&model_name);
        let model = store
            .create_model(
                args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7, args.8, args.9,
                args.10, args.11, args.12, args.13, args.14, args.15, args.16, args.17, args.18,
                &args.19, &args.20, &args.21,
            )
            .await
            .expect("model");
        fixtures.track_model(model.id);
        store
            .upsert_managed_model(UpsertManagedModel {
                model_id: model.id,
                enabled: true,
                partition: "gpu-preempt".into(),
                gres: "gpu:h100:1".into(),
                nodes: 1,
                constraints: None,
                exclude: None,
                account: None,
                qos: None,
                time_limit: None,
                cpus_per_task: None,
                mem: None,
                image: "vllm.sif".into(),
                preamble: String::new(),
                log_output_dir: String::new(),
                launch_command: "vllm serve perr-model".into(),
                script_body: String::new(),
                serving_port: 8000,
                health_path: "/health".into(),
                target_replicas: 1,
                min_replicas: 1,
                max_job_failures: 0,
                launcher_spec: None,
            })
            .await
            .expect("managed");

        store
            .set_provision_error(model.id, Some("error 2045"))
            .await
            .expect("set");
        let m = store
            .get_managed_model(model.id)
            .await
            .expect("get")
            .expect("some");
        assert_eq!(m.last_provision_error.as_deref(), Some("error 2045"));
        assert!(m.last_provision_error_at.is_some());

        store
            .set_provision_error(model.id, None)
            .await
            .expect("clear");
        let m = store
            .get_managed_model(model.id)
            .await
            .expect("get")
            .expect("some");
        assert_eq!(m.last_provision_error, None);
        assert!(m.last_provision_error_at.is_none());

        // Clean up so the shared test DB doesn't accumulate managed models.
        store.delete_managed_model(model.id).await.expect("delete");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn model_replica_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let args = default_test_model(&model_name);
        let model = store
            .create_model(
                args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7, args.8, args.9,
                args.10, args.11, args.12, args.13, args.14, args.15, args.16, args.17, args.18,
                &args.19, &args.20, &args.21,
            )
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        let r = store
            .create_replica(model.id, "job-123", None)
            .await
            .expect("create replica");
        fixtures.track_replica(r.id);
        assert_eq!(r.state, "pending");
        assert_eq!(r.slurm_job_id, "job-123");

        // Idempotent: re-creating the same (model_id, slurm_job_id) returns the
        // same row rather than spawning a duplicate.
        let again = store
            .create_replica(model.id, "job-123", None)
            .await
            .expect("create replica again");
        assert_eq!(again.id, r.id, "duplicate create must return the same row");
        assert_eq!(
            store.list_replicas(model.id).await.expect("list").len(),
            1,
            "no duplicate replica row"
        );

        let promoted = store
            .update_replica_state(r.id, "starting", Some("node alloc"))
            .await
            .expect("update state");
        assert_eq!(promoted.state, "starting");

        store
            .set_replica_runtime(r.id, Some("gpu-node-7"), None)
            .await
            .expect("set runtime");

        let mine = store.list_replicas(model.id).await.expect("list");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].nodes.as_deref(), Some("gpu-node-7"));

        let all = store.all_replicas().await.expect("all");
        assert!(all.iter().any(|x| x.id == r.id));

        // keep-semantics: patching only endpoint must NOT wipe nodes (and vice versa)
        store
            .set_replica_runtime(r.id, None, None)
            .await
            .expect("noop runtime");
        let after = store.list_replicas(model.id).await.expect("list2");
        assert_eq!(
            after[0].nodes.as_deref(),
            Some("gpu-node-7"),
            "nodes must be preserved"
        );

        // get_replica returns the row
        let got = store
            .get_replica(r.id)
            .await
            .expect("get_replica")
            .expect("present");
        assert_eq!(got.id, r.id);

        store.delete_replica(r.id).await.expect("delete");
        assert!(store
            .list_replicas(model.id)
            .await
            .expect("list")
            .is_empty());
    }

    #[tokio::test]
    async fn recipe_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");

        let saved = store
            .upsert_recipe(UpsertRecipe {
                id: None,
                name: "GLM".into(),
                body: "---\nname: GLM\n---\nllama-server".into(),
                author: "you".into(),
            })
            .await
            .expect("insert");
        assert!(store
            .list_recipes()
            .await
            .expect("list")
            .iter()
            .any(|r| r.id == saved.id));

        let updated = store
            .upsert_recipe(UpsertRecipe {
                id: Some(saved.id),
                name: "GLM v2".into(),
                body: saved.body.clone(),
                author: "you".into(),
            })
            .await
            .expect("update");
        assert_eq!(updated.name, "GLM v2");

        store.delete_recipe(saved.id).await.expect("delete");
        assert!(!store
            .list_recipes()
            .await
            .expect("list2")
            .iter()
            .any(|r| r.id == saved.id));
    }

    #[tokio::test]
    async fn slurm_settings_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");

        let settings = obleth_config::SlurmSettings {
            enabled: true,
            slurmrestd_url: "http://slurm:6820".into(),
            slurmrestd_api_version: "v0.0.40".into(),
            slurm_user: "obleth".into(),
            slurm_jwt: "header.payload.sig-secret".into(),
        };
        store.put_slurm_settings(&settings).await.expect("put");

        // get round-trips the JWT back to plaintext
        let got = store
            .get_slurm_settings()
            .await
            .expect("get")
            .expect("present");
        assert!(got.enabled);
        assert_eq!(got.slurmrestd_url, settings.slurmrestd_url);
        assert_eq!(got.slurm_user, settings.slurm_user);
        assert_eq!(got.slurm_jwt, settings.slurm_jwt);

        // the JWT must be ciphertext at rest whenever a cipher is configured
        let raw: sqlx::types::Json<serde_json::Value> =
            sqlx::query("select value from app_settings where key = 'slurm'")
                .fetch_one(&store.pool)
                .await
                .expect("raw")
                .try_get("value")
                .expect("col");
        let stored_jwt = raw
            .0
            .get("slurm_jwt")
            .and_then(|v| v.as_str())
            .unwrap_or_default();
        let cipher_on = std::env::var("OBLETH_ENCRYPTION_KEY")
            .map(|v| !v.trim().is_empty())
            .unwrap_or(false);
        if cipher_on {
            assert!(
                stored_jwt.starts_with("enc:v1:"),
                "jwt must be encrypted at rest, got {stored_jwt}"
            );
            assert_ne!(stored_jwt, settings.slurm_jwt);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn delete_lost_replicas_test() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let args = default_test_model(&model_name);
        let model = store
            .create_model(
                args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7, args.8, args.9,
                args.10, args.11, args.12, args.13, args.14, args.15, args.16, args.17, args.18,
                &args.19, &args.20, &args.21,
            )
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        // Insert two replicas.
        let r1 = store
            .create_replica(model.id, "job-loss-1", None)
            .await
            .expect("create replica 1");
        fixtures.track_replica(r1.id);
        let r2 = store
            .create_replica(model.id, "job-loss-2", None)
            .await
            .expect("create replica 2");
        fixtures.track_replica(r2.id);

        // Mark one as lost.
        store
            .update_replica_state(r1.id, "lost", Some("node gone"))
            .await
            .expect("mark lost");

        // delete_lost_replicas must remove only the lost one.
        let deleted = store
            .delete_lost_replicas(model.id)
            .await
            .expect("delete_lost_replicas");
        assert_eq!(deleted, 1, "exactly one lost replica deleted");

        let remaining = store.list_replicas(model.id).await.expect("list");
        assert_eq!(remaining.len(), 1, "one replica remains");
        assert_eq!(remaining[0].id, r2.id, "the non-lost replica survives");

        // Clean up.
        store.delete_replica(r2.id).await.expect("delete r2");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn set_replica_message_test() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let args = default_test_model(&model_name);
        let model = store
            .create_model(
                args.0, args.1, args.2, args.3, args.4, args.5, args.6, args.7, args.8, args.9,
                args.10, args.11, args.12, args.13, args.14, args.15, args.16, args.17, args.18,
                &args.19, &args.20, &args.21,
            )
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        let replica = store
            .create_replica(model.id, "job-msg-1", None)
            .await
            .expect("create replica");
        fixtures.track_replica(replica.id);

        // A message-only update must persist without touching state.
        let updated = store
            .set_replica_message(replica.id, "provisioning: waiting for allocation")
            .await
            .expect("set_replica_message");
        assert_eq!(
            updated.last_message.as_deref(),
            Some("provisioning: waiting for allocation"),
            "last_message must be persisted"
        );
        assert_eq!(updated.state, replica.state, "state must be unchanged");

        // Clean up.
        store
            .delete_replica(replica.id)
            .await
            .expect("delete replica");
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Deleting a model must NOT delete its replica rows — the provisioner needs
    /// them to drain the Slurm jobs they represent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn deleting_model_leaves_replica_rows() {
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
            .create_model(
                &format!("m-{}", Uuid::new_v4()),
                "replica-survival test model",
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
                &[],
                &[],
                &[],
            )
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        let replica = store
            .create_replica(model.id, "slurm-job-12345", Some(8000))
            .await
            .expect("create replica");
        fixtures.track_replica(replica.id);

        // Hard-delete the model. Before the cascade was dropped this also deleted
        // the replica row; now the row must survive.
        store.delete_model(model.id).await.expect("delete model");

        // The model is gone...
        assert!(
            matches!(store.get_model(model.id).await, Err(StoreError::NotFound)),
            "model row should be deleted"
        );
        // ...but its replica row survives for the provisioner to drain.
        let replicas = store.list_replicas(model.id).await.expect("list replicas");
        assert!(
            replicas.iter().any(|r| r.id == replica.id),
            "replica row must outlive the model delete"
        );

        // Clean up the now-orphaned replica row, matching the teardown discipline
        // of the other replica integration tests.
        store.delete_replica(replica.id).await.ok();
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Verifies that `debug_diagnostics` round-trips through the store and is
    /// not silently cleared back to `false` by an unrelated update's RETURNING.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn debug_diagnostics_round_trips_and_survives_unrelated_update() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model = store
            .create_model(
                &format!("diag-{}", Uuid::new_v4()),
                "debug diagnostics test model",
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
                &[],
                &[],
                &[],
            )
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        // Enable via the reliability update.
        let saved = store
            .update_model_reliability(model.id, None, 2, 200, "failover", true)
            .await
            .expect("update reliability");
        assert!(
            saved.debug_diagnostics,
            "reliability update should persist the flag"
        );

        // A read SELECT must report it (not the tolerant default).
        assert!(
            store.get_model(model.id).await.unwrap().debug_diagnostics,
            "get_model must return the persisted flag"
        );
        assert!(
            store
                .list_models()
                .await
                .unwrap()
                .iter()
                .find(|m| m.id == model.id)
                .unwrap()
                .debug_diagnostics,
            "list_models must return the persisted flag"
        );

        // An UNRELATED update's RETURNING must not mask it back to false
        // (this is the sync_model cache-poisoning trap).
        store
            .update_model_cache(model.id, true, 60)
            .await
            .expect("update_model_cache (unrelated update)");
        assert!(
            store.get_model(model.id).await.unwrap().debug_diagnostics,
            "unrelated update must not clear debug_diagnostics"
        );
    }
}
