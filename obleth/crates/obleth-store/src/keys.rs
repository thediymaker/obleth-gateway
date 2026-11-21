//! API keys: listing, writes, identity keys and the resolved-key reads.

use super::*;

/// A key's display name and prefix, for labelling rows that carry only its
/// id (see `key_names`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyName {
    pub id: Uuid,
    pub name: String,
    pub key_prefix: String,
}

/// What `list_keys_where` narrows to; every field is optional.
#[derive(Debug, Clone, Default)]
pub struct KeyListFilter {
    pub tenant_id: Option<Uuid>,
    /// Only these keys.
    pub ids: Option<Vec<Uuid>>,
    /// Leave out these keys.
    pub exclude_ids: Option<Vec<Uuid>>,
    /// Leave out this tenant's keys (the control plane's own).
    pub exclude_tenant: Option<Uuid>,
    /// Start after this position in `order`.
    pub seek: Option<KeySeek>,
    /// At most this many; all when `None`.
    pub limit: Option<i64>,
    /// Only keys whose name, prefix, description, identity subject or
    /// issuer, or tenant's name, contains this text, ignoring case.
    pub q: Option<String>,
    /// Only keys turned off (`true`) or on (`false`).
    pub disabled: Option<bool>,
    /// Only `secret` or `identity` keys.
    pub kind: Option<String>,
    /// Only keys with a token or cost cap (`true`) or with neither (`false`).
    pub capped: Option<bool>,
    pub order: KeyOrder,
}

/// The order `list_keys_where` returns keys in; ties always go by id, in the
/// same direction.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum KeyOrder {
    /// Oldest first.
    #[default]
    Created,
    /// Newest first.
    CreatedDesc,
    /// By name, ignoring case.
    Name,
    /// By name, ignoring case, Z first.
    NameDesc,
}

impl KeyOrder {
    pub fn descending(self) -> bool {
        matches!(self, KeyOrder::CreatedDesc | KeyOrder::NameDesc)
    }
}

/// Where a page of `list_keys_where` starts: just after the key with this
/// sort value and id, whether or not that key still exists.
#[derive(Debug, Clone, PartialEq)]
pub enum KeySeek {
    /// For the created orders.
    Created(chrono::DateTime<chrono::Utc>, Uuid),
    /// For the name orders: the key's name as stored (compared lowercased).
    Name(String, Uuid),
    /// After this key, oldest first, looked up by id (the original `after`
    /// of `GET /keys`; the key must still exist).
    After(Uuid),
}

/// The Keys page's tiles, from [`Store::key_stats`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyStats {
    pub keys: i64,
    pub disabled: i64,
    pub identity: i64,
    pub tenants: i64,
    /// Keys with use in the window.
    pub used: i64,
    /// Keys with use in the recent span.
    pub used_recent: i64,
    /// Keys turned on with no use in the window.
    pub unused_on: i64,
    /// Requests in the window by keys that still exist.
    pub requests: i64,
}

/// The `and ...` predicates of `f`, onto a query over `api_keys k`.
fn push_key_filter(qb: &mut sqlx::QueryBuilder<'_, sqlx::Postgres>, f: &KeyListFilter) {
    if let Some(t) = f.tenant_id {
        qb.push(" and k.tenant_id = ").push_bind(t);
    }
    if let Some(ids) = &f.ids {
        qb.push(" and k.id = any(").push_bind(ids.clone()).push(")");
    }
    if let Some(ids) = &f.exclude_ids {
        // An anti-join, so a long list (every key used in a window) costs
        // one hash, not a scan of the list per key.
        qb.push(" and not exists (select 1 from unnest(")
            .push_bind(ids.clone())
            .push("::uuid[]) as x(id) where x.id = k.id)");
    }
    if let Some(t) = f.exclude_tenant {
        qb.push(" and k.tenant_id <> ").push_bind(t);
    }
    if let Some(pattern) =
        f.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(contains_pattern)
    {
        qb.push(" and (k.name ilike ")
            .push_bind(pattern.clone())
            .push(" or k.key_prefix ilike ")
            .push_bind(pattern.clone())
            .push(" or k.description ilike ")
            .push_bind(pattern.clone())
            .push(" or k.identity_subject ilike ")
            .push_bind(pattern.clone())
            .push(" or k.identity_issuer ilike ")
            .push_bind(pattern.clone())
            .push(" or exists (select 1 from tenants t where t.id = k.tenant_id and t.name ilike ")
            .push_bind(pattern)
            .push("))");
    }
    if let Some(d) = f.disabled {
        qb.push(" and k.disabled = ").push_bind(d);
    }
    if let Some(kind) = &f.kind {
        qb.push(" and k.kind = ").push_bind(kind.clone());
    }
    match f.capped {
        Some(true) => {
            qb.push(" and (k.budget_tokens is not null or k.budget_cost_usd is not null)");
        }
        Some(false) => {
            qb.push(" and k.budget_tokens is null and k.budget_cost_usd is null");
        }
        None => {}
    }
}

/// An `ilike` pattern matching text that contains `q`, its own `%`, `_` and
/// `\` taken literally (backslash is `ilike`'s default escape).
pub(crate) fn contains_pattern(q: &str) -> String {
    let mut out = String::with_capacity(q.len() + 2);
    out.push('%');
    for c in q.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('%');
    out
}

/// A key's on/off settings that each have their own route, so a whole-key
/// `PUT` can never switch one by leaving it out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFlag {
    /// `ApiKey::disabled`: the key is refused.
    Disabled,
    /// `ApiKey::tracing_enabled`.
    Tracing,
    /// `ApiKey::end_user_fairshare`.
    EndUserFairshare,
    /// `ApiKey::background`.
    Background,
}

impl KeyFlag {
    /// The `api_keys` column, which is also the `ApiKey` field.
    pub fn column(self) -> &'static str {
        match self {
            KeyFlag::Disabled => "disabled",
            KeyFlag::Tracing => "tracing_enabled",
            KeyFlag::EndUserFairshare => "end_user_fairshare",
            KeyFlag::Background => "background",
        }
    }
}

/// The columns `api_key_from_row` reads, from `api_keys` aliased `k`.
const KEY_COLS: &str = "k.id, k.tenant_id, k.name, k.description, k.key_prefix,
    k.budget_tokens, k.budget_cost_usd, k.budget_period, k.budget_started_at,
    k.disabled, k.tracing_enabled, k.end_user_fairshare, k.background,
    k.allowed_networks, k.allowed_countries, k.created_at, k.updated_at,
    k.kind, k.identity_issuer, k.identity_subject, k.identity_claims,
    k.weight, k.max_in_flight";

/// Every column a `Tenant` is built from, in one place: each tenant select
/// and `returning` list splices this in, so a write can never hand back a row
/// the mapper only half-fills (tracing_enabled and synthetic used to go
/// missing from every write and read as false).
/// Which keys a resolved-key read returns.
enum KeyScope<'a> {
    All,
    Tenant(Uuid),
    Group(&'a str),
}

/// A key as the data plane sees it: the key, its tenant and its group. Every
/// resolved-key read selects this and adds its own `where`.
const RESOLVED_KEY_SELECT: &str = "select k.key_hash, k.id as key_id, k.tenant_id, k.disabled,
                    k.name as key_name, t.name as tenant_name, t.fairshare_group, g.weight as group_weight,
                    t.weight, t.tokens_per_minute, t.max_in_flight, t.status,
                    t.timezone, t.active_from, t.active_until, t.weekly_windows,
                    t.budget_tokens, t.budget_cost_usd, t.budget_period, t.budget_started_at,
                    k.budget_tokens as key_budget_tokens,
                    k.budget_cost_usd as key_budget_cost_usd,
                    k.budget_period as key_budget_period,
                    k.budget_started_at as key_budget_started_at,
                    k.weight as key_weight, k.max_in_flight as key_max_in_flight,
                    k.end_user_fairshare, k.background, k.allowed_networks, k.allowed_countries,
                    t.allowed_models,
                    (k.tracing_enabled OR t.tracing_enabled) AS tracing_enabled,
                    t.guardrails_policy,
                    t.compression_policy,
                    t.synthetic
             from api_keys k
             join tenants t on t.id = k.tenant_id
             join fairshare_groups g on g.name = t.fairshare_group";

fn api_key_from_row(row: &PgRow) -> Result<ApiKey> {
    let claims: Option<sqlx::types::Json<serde_json::Value>> =
        row.try_get("identity_claims").unwrap_or(None);
    Ok(ApiKey {
        id: row.try_get("id")?,
        tenant_id: row.try_get("tenant_id")?,
        name: row.try_get("name")?,
        description: row.try_get("description")?,
        key_prefix: row.try_get("key_prefix")?,
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
        tracing_enabled: row.try_get("tracing_enabled")?,
        end_user_fairshare: row.try_get("end_user_fairshare").unwrap_or(false),
        background: row.try_get("background").unwrap_or(false),
        // Strict, unlike the older columns above: a read that loses this
        // must fail, never resolve as "any network".
        allowed_networks: row.try_get::<Vec<String>, _>("allowed_networks")?,
        allowed_countries: row.try_get::<Vec<String>, _>("allowed_countries")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn resolved_from_row(row: &PgRow) -> Result<ResolvedKey> {
    Ok(ResolvedKey {
        key_id: row.try_get("key_id")?,
        tenant_id: row.try_get("tenant_id")?,
        tenant_name: row.try_get("tenant_name")?,
        key_name: row.try_get("key_name")?,
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
        key_weight: row.try_get("key_weight").unwrap_or(100),
        key_max_in_flight: row.try_get("key_max_in_flight").unwrap_or(None),
        end_user_fairshare: row.try_get("end_user_fairshare").unwrap_or(false),
        background: row.try_get("background").unwrap_or(false),
        // Strict, unlike the older columns above: a read that loses this
        // must fail, never resolve as "any network".
        allowed_networks: row.try_get::<Vec<String>, _>("allowed_networks")?,
        allowed_countries: row.try_get::<Vec<String>, _>("allowed_countries")?,
        allowed_models: allowed_models_from_row(row)?,
        internal: false,
        tracing_enabled: row.try_get::<bool, _>("tracing_enabled").unwrap_or(false),
        guardrails_policy: guardrails_policy_from_row(row)?,
        compression_policy: compression_policy_from_row(row)?,
        synthetic: row.try_get("synthetic")?,
    })
}

impl Store {
    /// Create a key. Returns the stored metadata plus the one-time raw secret.
    #[allow(clippy::too_many_arguments)]
    pub async fn create_api_key(
        &self,
        tenant_id: Uuid,
        name: &str,
        description: &str,
        budget_tokens: Option<i64>,
        budget_cost_usd: Option<f64>,
        budget_period: Option<&str>,
        budget_started_at: Option<DateTime<Utc>>,
        weight: i64,
        max_in_flight: Option<i64>,
        allowed_networks: &[String],
        allowed_countries: &[String],
    ) -> Result<(ApiKey, String)> {
        let gen = generate_api_key_with_prefix(&self.key_prefix().await?);
        let row = sqlx::query(
            "insert into api_keys (id, tenant_id, name, description, key_prefix, key_hash,
                    budget_tokens, budget_cost_usd, budget_period, budget_started_at,
                    weight, max_in_flight, allowed_networks, allowed_countries)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)
             returning id, tenant_id, name, description, key_prefix,
                    budget_tokens, budget_cost_usd, budget_period, budget_started_at,
                    disabled, tracing_enabled, end_user_fairshare, background, allowed_networks, allowed_countries,
                    created_at, updated_at,
                    kind, identity_issuer, identity_subject, identity_claims,
                    weight, max_in_flight",
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
        .bind(weight)
        .bind(max_in_flight)
        .bind(allowed_networks)
        .bind(allowed_countries)
        .fetch_one(&self.pool)
        .await?;
        Ok((api_key_from_row(&row)?, gen.secret))
    }

    /// Ensure the identity key for (issuer, subject) exists under `tenant_name`
    /// and return its hot-path view. Idempotent and safe under concurrency:
    /// the tenant lookup-or-create runs under an advisory lock keyed by the
    /// tenant name, and the key insert conflicts on the partial unique index
    /// `(identity_issuer, identity_subject) where kind = 'identity'`.
    pub async fn provision_identity_key(
        &self,
        p: IdentityProvision<'_>,
    ) -> Result<ProvisionedIdentity> {
        // `tenant_name` comes straight from `OBLETH_JWT_ISSUERS` config, not
        // from anything validated at boot against reserved names. Without
        // this check, a config value equal to the reserved control-plane
        // tenant's name would try to fold externally authenticated CLI/IdP
        // traffic into Charo's reserved identity (`ensure_control_plane_identity`),
        // whose tenant is hidden from and protected against the normal
        // management surfaces -- exactly the kind of silent misrouting this
        // whole feature's tenant-mismatch alerting exists to catch.
        if let Some(existing) = self.tenant_by_name(p.tenant_name).await? {
            if existing.id == Self::CONTROL_PLANE_TENANT_ID {
                return Err(StoreError::Protected(
                    "identity keys cannot be provisioned into the reserved control-plane tenant"
                        .into(),
                ));
            }
        }

        let hash = obleth_config::identity_key_hash(p.issuer, p.subject);

        // Fast path: already provisioned.
        if let Some(resolved) = self.resolved_key_by_hash(&hash).await? {
            return Ok(ProvisionedIdentity {
                hash,
                resolved,
                tenant_created: false,
                key_created: false,
            });
        }

        let mut tx = self.pool.begin().await?;
        // Serialise tenant lookup-or-create per name so racing first requests
        // cannot both insert (tenants.name is unique; this avoids surfacing
        // that as an error to one of them).
        sqlx::query("select pg_advisory_xact_lock(hashtext($1))")
            .bind(p.tenant_name)
            .execute(&mut *tx)
            .await?;
        let existing: Option<Uuid> = sqlx::query("select id from tenants where name = $1")
            .bind(p.tenant_name)
            .fetch_optional(&mut *tx)
            .await?
            .map(|r| r.try_get("id"))
            .transpose()?;
        let (tenant_id, tenant_created) = match existing {
            Some(id) => {
                // Re-check inside the transaction: the pre-transaction lookup
                // above is not atomic with this one, so a rename racing this
                // call could otherwise slip an identity key into the reserved
                // tenant between the two reads.
                if id == Self::CONTROL_PLANE_TENANT_ID {
                    return Err(StoreError::Protected(
                        "identity keys cannot be provisioned into the reserved control-plane tenant"
                            .into(),
                    ));
                }
                (id, false)
            }
            None => {
                let id = Uuid::new_v4();
                sqlx::query(
                    "insert into tenants (id, name, fairshare_group, weight, tokens_per_minute)
                     values ($1, $2, 'default', 100, 0)",
                )
                .bind(id)
                .bind(p.tenant_name)
                .execute(&mut *tx)
                .await?;
                Self::record_audit_tx(
                    &mut tx,
                    "identity-provisioning",
                    "create_tenant",
                    "tenant",
                    &id.to_string(),
                    serde_json::json!({
                        "name": p.tenant_name,
                        "issuer": p.issuer,
                        "source": "jwt_jit",
                    }),
                )
                .await?;
                (id, true)
            }
        };
        let prefix = truncate_chars(&format!("jwt:{}", p.subject), 64);
        let name = format!("{}-cli", p.subject);
        let claims = match p.claims {
            serde_json::Value::Null => None,
            v => Some(sqlx::types::Json(v)),
        };
        let key_row = sqlx::query(
            "insert into api_keys (id, tenant_id, name, description, key_prefix, key_hash,
                    kind, identity_issuer, identity_subject, identity_claims)
             values ($1, $2, $3, '', $4, $5, 'identity', $6, $7, $8)
             on conflict (identity_issuer, identity_subject) where kind = 'identity'
             do update set key_hash = excluded.key_hash, updated_at = now()
             returning id, (xmax = 0) as inserted",
        )
        .bind(Uuid::new_v4())
        .bind(tenant_id)
        .bind(&name)
        .bind(&prefix)
        .bind(&hash)
        .bind(p.issuer)
        .bind(p.subject)
        .bind(claims)
        .fetch_one(&mut *tx)
        .await?;
        let key_id: Uuid = key_row.try_get("id")?;
        let inserted: bool = key_row.try_get("inserted")?;
        if inserted {
            Self::record_audit_tx(
                &mut tx,
                "identity-provisioning",
                "create_key",
                "api_key",
                &key_id.to_string(),
                serde_json::json!({
                    "tenant_id": tenant_id,
                    "kind": "identity",
                    "issuer": p.issuer,
                    "subject": p.subject,
                }),
            )
            .await?;
        }
        tx.commit().await?;

        let resolved = self
            .resolved_key_by_hash(&hash)
            .await?
            .ok_or(StoreError::NotFound)?;
        Ok(ProvisionedIdentity {
            hash,
            resolved,
            tenant_created,
            key_created: inserted,
        })
    }

    /// Same insert as [`Store::record_audit`], but runs inside the caller's
    /// transaction so the audit row commits atomically with the mutation it
    /// describes (or rolls back with it).
    async fn record_audit_tx(
        tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
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
        .execute(&mut **tx)
        .await?;
        Ok(())
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
            // model + MCP). Marked synthetic so all of Charo's traffic — the
            // model-test console and dashboard benchmarks — is tagged
            // `benchmark` in the ledger and stays out of real usage stats by
            // default. on-conflict makes this a no-op once provisioned.
            sqlx::query(
                "insert into tenants
                     (id, name, fairshare_group, weight, tokens_per_minute, description, synthetic)
                 values ($1, '__control_plane__', 'default', 100, 0,
                         'Reserved identity for the in-app Charo model test console', true)
                 on conflict (id) do nothing",
            )
            .bind(Self::CONTROL_PLANE_TENANT_ID)
            .execute(&self.pool)
            .await?;

            // Existing deployments provisioned before the synthetic flag: flip
            // it on idempotently so their Charo/benchmark traffic stops
            // clobbering usage stats. Uses raw sql (not set_tenant_synthetic,
            // which guards the reserved tenant against external mutation).
            sqlx::query("update tenants set synthetic = true where id = $1 and synthetic = false")
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
                    100,
                    None,
                    &[],
                    &[],
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
    pub(crate) fn guard_reserved_tenant(id: Uuid) -> Result<()> {
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

    /// Every key, or one tenant's, oldest first.
    pub async fn list_keys(&self, tenant_id: Option<Uuid>) -> Result<Vec<ApiKey>> {
        self.list_keys_where(&KeyListFilter {
            tenant_id,
            ..Default::default()
        })
        .await
    }

    /// Keys matching `f` in `f.order` (oldest first by default), at most
    /// `f.limit`, starting after `f.seek`. Every order ends in `id`, so a page
    /// continues exactly after its last key.
    pub async fn list_keys_where(&self, f: &KeyListFilter) -> Result<Vec<ApiKey>> {
        let mut qb =
            sqlx::QueryBuilder::new(format!("select {KEY_COLS} from api_keys k where true"));
        push_key_filter(&mut qb, f);
        let desc = f.order.descending();
        let cmp = if desc { "<" } else { ">" };
        let (column, dir) = match f.order {
            KeyOrder::Created => ("k.created_at", ""),
            KeyOrder::CreatedDesc => ("k.created_at", " desc"),
            KeyOrder::Name => ("lower(k.name)", ""),
            KeyOrder::NameDesc => ("lower(k.name)", " desc"),
        };
        match &f.seek {
            None => {}
            Some(KeySeek::Created(at, id)) => {
                qb.push(format!(" and (k.created_at, k.id) {cmp} ("))
                    .push_bind(*at)
                    .push(", ")
                    .push_bind(*id)
                    .push(")");
            }
            Some(KeySeek::Name(name, id)) => {
                qb.push(format!(" and (lower(k.name), k.id) {cmp} (lower("))
                    .push_bind(name.clone())
                    .push("), ")
                    .push_bind(*id)
                    .push(")");
            }
            Some(KeySeek::After(id)) => {
                qb.push(" and (k.created_at, k.id) > (select a.created_at, a.id from api_keys a where a.id = ")
                    .push_bind(*id)
                    .push(")");
            }
        }
        qb.push(format!(" order by {column}{dir}, k.id{dir}"));
        if let Some(n) = f.limit {
            qb.push(" limit ").push_bind(n);
        }
        let rows = qb.build().fetch_all(&self.pool).await?;
        rows.iter().map(api_key_from_row).collect()
    }

    /// How many keys match `f` (its order, seek and limit aside), and how
    /// many tenants they are in.
    pub async fn count_keys_where(&self, f: &KeyListFilter) -> Result<(i64, i64)> {
        let mut qb = sqlx::QueryBuilder::new(
            "select count(*) as keys, count(distinct k.tenant_id) as tenants from api_keys k where true",
        );
        push_key_filter(&mut qb, f);
        let row = qb.build().fetch_one(&self.pool).await?;
        Ok((row.try_get("keys")?, row.try_get("tenants")?))
    }

    /// The ids of the keys matching `f` (its order and seek aside), at most
    /// `f.limit`.
    pub async fn key_ids_where(&self, f: &KeyListFilter) -> Result<Vec<Uuid>> {
        let mut qb = sqlx::QueryBuilder::new("select k.id from api_keys k where true");
        push_key_filter(&mut qb, f);
        qb.push(" order by k.id");
        if let Some(n) = f.limit {
            qb.push(" limit ").push_bind(n);
        }
        let rows = qb.build().fetch_all(&self.pool).await?;
        rows.iter()
            .map(|r| r.try_get("id").map_err(Into::into))
            .collect()
    }

    /// Counts over every key but `exclude_tenant`'s for the Keys page's
    /// tiles. `used` are keys with use in a window, each with its requests;
    /// `recent` those used in a shorter span. Ids of deleted keys count for
    /// nothing.
    pub async fn key_stats(
        &self,
        exclude_tenant: Option<Uuid>,
        used: &[(Uuid, i64)],
        recent: &[Uuid],
    ) -> Result<KeyStats> {
        let (ids, requests): (Vec<Uuid>, Vec<i64>) = used.iter().copied().unzip();
        let row = sqlx::query(
            "with used as (select * from unnest($1::uuid[], $2::bigint[]) as u(id, requests)),
                  recent as (select unnest($3::uuid[]) as id)
             select count(*) as keys,
                    count(*) filter (where k.disabled) as disabled,
                    count(*) filter (where k.kind = 'identity') as identity,
                    count(distinct k.tenant_id) as tenants,
                    count(u.id) as used,
                    count(r.id) as used_recent,
                    count(*) filter (where u.id is null and not k.disabled) as unused_on,
                    coalesce(sum(u.requests), 0)::bigint as requests
             from api_keys k
             left join used u on u.id = k.id
             left join recent r on r.id = k.id
             where ($4::uuid is null or k.tenant_id <> $4)",
        )
        .bind(&ids)
        .bind(&requests)
        .bind(recent)
        .bind(exclude_tenant)
        .fetch_one(&self.pool)
        .await?;
        Ok(KeyStats {
            keys: row.try_get("keys")?,
            disabled: row.try_get("disabled")?,
            identity: row.try_get("identity")?,
            tenants: row.try_get("tenants")?,
            used: row.try_get("used")?,
            used_recent: row.try_get("used_recent")?,
            unused_on: row.try_get("unused_on")?,
            requests: row.try_get("requests")?,
        })
    }

    /// Names and fair-share groups of the given tenants, without reading or
    /// decoding the rest of each row. Unknown ids are left out.
    pub async fn tenant_names(&self, ids: &[Uuid]) -> Result<Vec<TenantName>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query("select id, name, fairshare_group from tenants where id = any($1)")
            .bind(ids)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(TenantName {
                    id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    fairshare_group: r.try_get("fairshare_group")?,
                })
            })
            .collect()
    }

    /// Names and prefixes of the given keys, without reading the rest of each
    /// row. Unknown ids are left out.
    pub async fn key_names(&self, ids: &[Uuid]) -> Result<Vec<KeyName>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query("select id, name, key_prefix from api_keys where id = any($1)")
            .bind(ids)
            .fetch_all(&self.pool)
            .await?;
        rows.iter()
            .map(|r| {
                Ok(KeyName {
                    id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    key_prefix: r.try_get("key_prefix")?,
                })
            })
            .collect()
    }

    /// Fetch a bounded set of keys by id in one query. Used to resolve the
    /// key ids on a page of request-log rows to display names without loading
    /// the full key fleet (which can be 100k+).
    pub async fn keys_by_ids(&self, ids: &[Uuid]) -> Result<Vec<ApiKey>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let rows = sqlx::query(&format!(
            "select {KEY_COLS} from api_keys k where k.id = any($1)"
        ))
        .bind(ids)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(api_key_from_row).collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_api_key(
        &self,
        id: Uuid,
        name: &str,
        description: &str,
        budget_tokens: Option<i64>,
        budget_cost_usd: Option<f64>,
        budget_period: Option<&str>,
        budget_started_at: Option<DateTime<Utc>>,
        weight: i64,
        max_in_flight: Option<i64>,
        allowed_networks: Option<&[String]>,
        allowed_countries: Option<&[String]>,
    ) -> Result<(String, ApiKey, ResolvedKey)> {
        self.guard_reserved_key(id).await?;
        // `allowed_networks` and `allowed_countries` are the fields a
        // whole-key update leaves as they are when not sent (`None`): an edit that never mentions it,
        // such as a bulk budget change, must not open a limited key to every
        // network.
        let row = sqlx::query(
            "update api_keys
             set name = $2,
                 description = $3,
                 budget_tokens = $4,
                 budget_cost_usd = $5,
                 budget_period = $6,
                 budget_started_at = $7,
                 weight = $8,
                 max_in_flight = $9,
                 allowed_networks = coalesce($10, allowed_networks),
                 allowed_countries = coalesce($11, allowed_countries),
                 updated_at = now()
             where id = $1
             returning key_hash, id, tenant_id, name, description, key_prefix,
                    budget_tokens, budget_cost_usd, budget_period, budget_started_at,
                    disabled, tracing_enabled, end_user_fairshare, background, allowed_networks, allowed_countries,
                    created_at, updated_at,
                    kind, identity_issuer, identity_subject, identity_claims,
                    weight, max_in_flight",
        )
        .bind(id)
        .bind(name)
        .bind(description)
        .bind(budget_tokens)
        .bind(budget_cost_usd)
        .bind(budget_period)
        .bind(budget_started_at)
        .bind(weight)
        .bind(max_in_flight)
        .bind(allowed_networks)
        .bind(allowed_countries)
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

    /// Move a key to another tenant. Its secret, settings and budget stay as
    /// they are; from now on its requests count against the new tenant (past
    /// usage keeps the tenant it was recorded under). Returns the key's hash,
    /// the key, and its hot-path view under the new tenant, plus the tenant it
    /// left.
    pub async fn move_api_key(
        &self,
        id: Uuid,
        tenant_id: Uuid,
    ) -> Result<(String, ApiKey, ResolvedKey, Uuid)> {
        self.guard_reserved_key(id).await?;
        Self::guard_reserved_tenant(tenant_id)?;
        let mut tx = self.pool.begin().await?;
        let from: Uuid = sqlx::query("select tenant_id from api_keys where id = $1 for update")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::NotFound)?
            .try_get("tenant_id")?;
        let exists: bool = sqlx::query("select exists(select 1 from tenants where id = $1) as e")
            .bind(tenant_id)
            .fetch_one(&mut *tx)
            .await?
            .try_get("e")?;
        if !exists {
            return Err(StoreError::NotFound);
        }
        let row = sqlx::query(
            "update api_keys set tenant_id = $2, updated_at = now()
             where id = $1
             returning key_hash, id, tenant_id, name, description, key_prefix,
                    budget_tokens, budget_cost_usd, budget_period, budget_started_at,
                    disabled, tracing_enabled, end_user_fairshare, background, allowed_networks, allowed_countries,
                    created_at, updated_at,
                    kind, identity_issuer, identity_subject, identity_claims,
                    weight, max_in_flight",
        )
        .bind(id)
        .bind(tenant_id)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let hash: String = row.try_get("key_hash")?;
        let key = api_key_from_row(&row)?;
        let resolved = self
            .resolved_key_by_hash(&hash)
            .await?
            .ok_or(StoreError::NotFound)?;
        Ok((hash, key, resolved, from))
    }

    /// Turn one of a key's own on/off settings on or off (see [`KeyFlag`]).
    /// Returns the key's hash, the updated key and its hot-path view.
    pub async fn set_key_flag(
        &self,
        id: Uuid,
        flag: KeyFlag,
        on: bool,
    ) -> Result<(String, ApiKey, ResolvedKey)> {
        self.guard_reserved_key(id).await?;
        let row = sqlx::query(&format!(
            "update api_keys k set {} = $2, updated_at = now() where k.id = $1
             returning k.key_hash, {KEY_COLS}",
            flag.column()
        ))
        .bind(id)
        .bind(on)
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

    pub async fn set_tenant_tracing(&self, id: Uuid, tracing_enabled: bool) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(&format!(
            "update tenants set tracing_enabled = $2, updated_at = now() where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(tracing_enabled)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
    }

    /// Mark a tenant as synthetic (benchmark/test traffic) or real.
    pub async fn set_tenant_synthetic(&self, id: Uuid, synthetic: bool) -> Result<Tenant> {
        Self::guard_reserved_tenant(id)?;
        let row = sqlx::query(&format!(
            "update tenants set synthetic = $2, updated_at = now() where id = $1
             returning {TENANT_COLUMNS}"
        ))
        .bind(id)
        .bind(synthetic)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        tenant_from_row(&row)
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
        let row = sqlx::query(&format!("{RESOLVED_KEY_SELECT} where k.key_hash = $1"))
            .bind(hash)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(resolved_from_row).transpose()
    }

    /// The resolved key with this id, for callers that act for a key later
    /// without its secret (the batch runner).
    pub async fn resolved_key_by_id(&self, id: Uuid) -> Result<Option<ResolvedKey>> {
        let row = sqlx::query(&format!("{RESOLVED_KEY_SELECT} where k.id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        row.as_ref().map(resolved_from_row).transpose()
    }

    /// All (hash, resolved-key) pairs, for warming/refreshing the Redis cache.
    pub async fn all_resolved_keys(&self) -> Result<Vec<(String, ResolvedKey)>> {
        self.resolved_keys_where(KeyScope::All).await
    }

    /// The resolved keys of one tenant, for re-pushing after a tenant edit.
    pub async fn resolved_keys_for_tenant(
        &self,
        tenant_id: Uuid,
    ) -> Result<Vec<(String, ResolvedKey)>> {
        self.resolved_keys_where(KeyScope::Tenant(tenant_id)).await
    }

    /// The resolved keys of every tenant in fairshare `group`, for re-pushing
    /// after the group's weight changed.
    pub async fn resolved_keys_for_group(&self, group: &str) -> Result<Vec<(String, ResolvedKey)>> {
        self.resolved_keys_where(KeyScope::Group(group)).await
    }

    async fn resolved_keys_where(&self, scope: KeyScope<'_>) -> Result<Vec<(String, ResolvedKey)>> {
        let rows = match scope {
            KeyScope::All => {
                sqlx::query(RESOLVED_KEY_SELECT)
                    .fetch_all(&self.pool)
                    .await?
            }
            KeyScope::Tenant(id) => {
                sqlx::query(&format!("{RESOLVED_KEY_SELECT} where k.tenant_id = $1"))
                    .bind(id)
                    .fetch_all(&self.pool)
                    .await?
            }
            KeyScope::Group(group) => {
                sqlx::query(&format!(
                    "{RESOLVED_KEY_SELECT} where t.fairshare_group = $1"
                ))
                .bind(group)
                .fetch_all(&self.pool)
                .await?
            }
        };
        rows.iter()
            .map(|r| Ok((r.try_get::<String, _>("key_hash")?, resolved_from_row(r)?)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{serial, FixtureGuard};
    use obleth_config::hash_api_key;
    use obleth_config::API_KEY_KIND_IDENTITY;

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn new_keys_start_with_the_saved_prefix() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);
        let before = sqlx::query("select value from app_settings where key = 'key_prefix'")
            .fetch_optional(&store.pool)
            .await
            .expect("read setting")
            .map(|r| r.get::<sqlx::types::Json<serde_json::Value>, _>("value").0);

        let tenant = store
            .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
            .await
            .expect("create tenant");
        fixtures.track_tenant(tenant.id);

        store
            .put_key_prefix_setting("rc")
            .await
            .expect("save prefix");
        assert_eq!(store.key_prefix().await.unwrap(), "rc");
        let (key, secret) = store
            .create_api_key(
                tenant.id,
                "k",
                "",
                None,
                None,
                None,
                None,
                100,
                None,
                &[],
                &[],
            )
            .await
            .expect("create key");
        assert!(secret.starts_with("rc_"), "{secret}");
        assert!(obleth_config::is_well_formed_key(&secret));
        assert!(key.key_prefix.starts_with("rc_"));

        // An invalid saved value reads as none, so minting falls back.
        sqlx::query(
            "update app_settings set value = '{\"prefix\": \"Bad!\"}' where key = 'key_prefix'",
        )
        .execute(&store.pool)
        .await
        .expect("corrupt");
        assert_eq!(store.get_key_prefix_setting().await.unwrap(), None);

        // An instance that already made `obl_` keys keeps `obl` on upgrade,
        // and a prefix saved later is never overwritten.
        store.put_key_prefix_setting("obl").await.expect("save obl");
        let (_, obl) = store
            .create_api_key(
                tenant.id,
                "k2",
                "",
                None,
                None,
                None,
                None,
                100,
                None,
                &[],
                &[],
            )
            .await
            .expect("create obl key");
        assert!(obl.starts_with("obl_"), "{obl}");
        // Upgrading: 0044 runs once on an instance that has not recorded it.
        sqlx::query("delete from app_settings where key = 'key_prefix'")
            .execute(&store.pool)
            .await
            .expect("clear setting");
        sqlx::query("delete from schema_migrations where version = '0044_key_prefix_keeps_obl'")
            .execute(&store.pool)
            .await
            .expect("forget 0044");
        store.migrate().await.expect("migrate again");
        assert_eq!(
            store.get_key_prefix_setting().await.unwrap().as_deref(),
            Some("obl")
        );
        store.put_key_prefix_setting("rc").await.expect("save rc");
        sqlx::query("delete from schema_migrations where version = '0044_key_prefix_keeps_obl'")
            .execute(&store.pool)
            .await
            .expect("forget 0044 again");
        store.migrate().await.expect("migrate again");
        assert_eq!(store.key_prefix().await.unwrap(), "rc");

        match before {
            Some(v) => {
                sqlx::query("update app_settings set value = $1 where key = 'key_prefix'")
                    .bind(sqlx::types::Json(v))
                    .execute(&store.pool)
                    .await
                    .expect("restore");
            }
            None => {
                sqlx::query("delete from app_settings where key = 'key_prefix'")
                    .execute(&store.pool)
                    .await
                    .expect("restore");
            }
        }
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Per-key fairshare weight and max_in_flight must round-trip through
    /// create/update and flow through into the resolved hot-path key view.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn key_weight_and_cap_round_trip_to_resolved_key() {
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
            .create_tenant(&name, 100, 0, None, None)
            .await
            .expect("create tenant");
        fixtures.track_tenant(tenant.id);

        let (key, secret) = store
            .create_api_key(
                tenant.id,
                "alice",
                "",
                None,
                None,
                None,
                None,
                250,
                Some(3),
                &[],
                &[],
            )
            .await
            .unwrap();
        assert_eq!(key.weight, 250);
        assert_eq!(key.max_in_flight, Some(3));
        let hash = obleth_config::hash_api_key(&secret);
        let resolved = store.resolved_key_by_hash(&hash).await.unwrap().unwrap();
        assert_eq!(resolved.key_weight, 250);
        assert_eq!(resolved.key_max_in_flight, Some(3));

        let (_, updated, resolved) = store
            .update_api_key(
                key.id, "alice", "", None, None, None, None, 100, None, None, None,
            )
            .await
            .unwrap();
        assert_eq!(updated.weight, 100);
        assert_eq!(updated.max_in_flight, None);
        assert_eq!(resolved.key_weight, 100);
        assert_eq!(resolved.key_max_in_flight, None);
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Moving a key keeps its secret and settings, and its hot-path view picks
    /// up the new tenant's limits; a missing or reserved tenant is refused.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn move_api_key_carries_the_key_to_the_new_tenant() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let from = store
            .create_tenant(&format!("from-{}", Uuid::new_v4()), 100, 0, None, None)
            .await
            .expect("from");
        fixtures.track_tenant(from.id);
        // A tenant's group must exist first (tenants_fairshare_group_fkey).
        let group = format!("heavy-{}", Uuid::new_v4().simple());
        store
            .create_fairshare_group(&group, 100)
            .await
            .expect("group");
        let to = store
            .create_tenant(
                &format!("to-{}", Uuid::new_v4()),
                300,
                5000,
                Some(2),
                Some(&group),
            )
            .await
            .expect("to");
        fixtures.track_tenant(to.id);
        let (key, secret) = store
            .create_api_key(
                from.id,
                "heavy-user",
                "",
                None,
                Some(25.0),
                Some("monthly"),
                None,
                150,
                Some(3),
                &[],
                &[],
            )
            .await
            .expect("create key");
        let hash = hash_api_key(&secret);

        let (moved_hash, moved, resolved, left) =
            store.move_api_key(key.id, to.id).await.expect("move");
        assert_eq!(moved_hash, hash, "the secret is unchanged");
        assert_eq!(left, from.id);
        assert_eq!(moved.tenant_id, to.id);
        assert_eq!(moved.weight, 150);
        assert_eq!(moved.budget_cost_usd, Some(25.0));
        assert_eq!(resolved.tenant_id, to.id);
        assert_eq!(resolved.fairshare_group, group);
        assert_eq!(resolved.tokens_per_minute, 5000);

        assert!(matches!(
            store.move_api_key(key.id, Uuid::new_v4()).await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store
                .move_api_key(key.id, Store::CONTROL_PLANE_TENANT_ID)
                .await,
            Err(StoreError::Protected(_))
        ));
        assert!(matches!(
            store.move_api_key(Uuid::new_v4(), to.id).await,
            Err(StoreError::NotFound)
        ));
        assert_eq!(
            store
                .resolved_key_by_hash(&hash)
                .await
                .unwrap()
                .unwrap()
                .tenant_id,
            to.id
        );

        // The group outlives the fixture guard's tenant cleanup otherwise.
        let _ = store.delete_tenant(to.id).await;
        let _ = sqlx::query("delete from fairshare_groups where name = $1")
            .bind(&group)
            .execute(&store.pool)
            .await;
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Verifies `provision_identity_key` is idempotent (same identity ⇒ same
    /// key, no duplicate tenant) and safe under concurrent first-sight
    /// provisioning of the same identity.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provision_identity_key_is_idempotent_and_concurrent_safe() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.unwrap();
        store.migrate().await.unwrap();
        let mut guard = FixtureGuard::new(&store);
        let tenant_name = format!("t-idp-{}", Uuid::new_v4());
        let issuer = "https://idp.example.com";
        let subject = format!("alice-{}", Uuid::new_v4());

        let first = store
            .provision_identity_key(IdentityProvision {
                issuer,
                subject: &subject,
                tenant_name: &tenant_name,
                claims: serde_json::json!({ "uid": "u-1" }),
            })
            .await
            .unwrap();
        guard.track_tenant(first.resolved.tenant_id);
        assert!(first.tenant_created);
        assert!(first.key_created);
        assert_eq!(
            first.hash,
            obleth_config::identity_key_hash(issuer, &subject)
        );
        assert_eq!(first.resolved.tenant_name, tenant_name);

        // Same identity again: no new tenant, no new key, same key id.
        let second = store
            .provision_identity_key(IdentityProvision {
                issuer,
                subject: &subject,
                tenant_name: &tenant_name,
                claims: serde_json::json!({ "uid": "u-1" }),
            })
            .await
            .unwrap();
        assert!(!second.tenant_created);
        assert!(!second.key_created);
        assert_eq!(second.resolved.key_id, first.resolved.key_id);

        // Concurrent first-sight of a *different* subject yields exactly one row.
        let subject2 = format!("bob-{}", Uuid::new_v4());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let store = store.clone();
            let tenant_name = tenant_name.clone();
            let subject2 = subject2.clone();
            handles.push(tokio::spawn(async move {
                store
                    .provision_identity_key(IdentityProvision {
                        issuer,
                        subject: &subject2,
                        tenant_name: &tenant_name,
                        claims: serde_json::Value::Null,
                    })
                    .await
                    .unwrap()
                    .resolved
                    .key_id
            }));
        }
        let mut ids = std::collections::HashSet::new();
        for h in handles {
            ids.insert(h.await.unwrap());
        }
        assert_eq!(ids.len(), 1, "concurrent upserts must converge on one key");

        // The row is a real key: list_keys shows kind = identity with the subject.
        let keys = store
            .list_keys(Some(first.resolved.tenant_id))
            .await
            .unwrap();
        let k = keys.iter().find(|k| k.id == first.resolved.key_id).unwrap();
        assert_eq!(k.kind, API_KEY_KIND_IDENTITY);
        assert_eq!(k.identity_issuer.as_deref(), Some(issuer));
        assert_eq!(k.identity_subject.as_deref(), Some(subject.as_str()));
        assert_eq!(k.key_prefix, format!("jwt:{subject}"));
        assert_eq!(k.identity_claims, Some(serde_json::json!({ "uid": "u-1" })));

        // JIT provisioning audits both the tenant creation and the key
        // creation, scoped to the right entity, and only on first sight --
        // the idempotent `second` call above must not add more rows.
        let audit = store.list_audit(500).await.unwrap();
        let tenant_audits: Vec<_> = audit
            .iter()
            .filter(|a| {
                a.entity_type == "tenant" && a.entity_id == first.resolved.tenant_id.to_string()
            })
            .collect();
        assert_eq!(
            tenant_audits.len(),
            1,
            "tenant creation must be audited exactly once, not duplicated on repeat provisioning"
        );
        assert_eq!(tenant_audits[0].actor, "identity-provisioning");
        assert_eq!(tenant_audits[0].action, "create_tenant");
        assert_eq!(tenant_audits[0].detail["issuer"], issuer);

        let key_audits: Vec<_> = audit
            .iter()
            .filter(|a| {
                a.entity_type == "api_key" && a.entity_id == first.resolved.key_id.to_string()
            })
            .collect();
        assert_eq!(
            key_audits.len(),
            1,
            "key creation must be audited exactly once, not duplicated on repeat provisioning"
        );
        assert_eq!(key_audits[0].actor, "identity-provisioning");
        assert_eq!(key_audits[0].action, "create_key");
        assert_eq!(key_audits[0].detail["kind"], "identity");
        assert_eq!(key_audits[0].detail["subject"], subject);
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Identity keys are JIT-provisioned into whatever tenant name
    /// `OBLETH_JWT_ISSUERS` configures; a config value that names the
    /// reserved control-plane tenant must be rejected rather than folding
    /// externally authenticated traffic into Charo's reserved identity.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn provision_identity_key_rejects_reserved_tenant() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.unwrap();
        store.migrate().await.unwrap();
        store
            .ensure_control_plane_identity()
            .await
            .expect("ensure control plane identity");

        let err = store
            .provision_identity_key(IdentityProvision {
                issuer: "https://idp.example.com",
                subject: &format!("carol-{}", Uuid::new_v4()),
                tenant_name: "__control_plane__",
                claims: serde_json::Value::Null,
            })
            .await
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Protected(_)),
            "expected Protected, got {err:?}"
        );
    }
}
