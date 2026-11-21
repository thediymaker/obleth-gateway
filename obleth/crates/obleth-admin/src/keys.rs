//! API keys: request bodies, listing, writes and budget reads.

use super::*;

#[derive(Debug, Deserialize, ToSchema)]
pub struct MoveKey {
    /// The tenant the key moves to.
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateKey {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Cumulative token ceiling for the current key term. `null` clears the cap.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Cumulative USD-cost ceiling for the current key term. `null` clears the cap.
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    /// Reset period: `lifetime`, `monthly`, or `term`. `null` = lifetime.
    #[serde(default)]
    pub budget_period: Option<String>,
    /// When the current key term began.
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Fairshare weight among the tenant's keys. Default 100, minimum 1.
    #[serde(default = "obleth_config::default_key_weight")]
    pub weight: i64,
    /// Per-model in-flight ceiling for this key. `null` clears the cap.
    #[serde(default)]
    pub max_in_flight: Option<i64>,
    /// Networks the key may be used from: CIDR ranges or single addresses
    /// (IPv4 or IPv6), at most 64. Empty or absent means any network.
    /// Entries are stored in canonical form (`203.0.113.7` becomes
    /// `203.0.113.7/32`).
    #[serde(default)]
    pub allowed_networks: Vec<String>,
    /// Countries the key may be used from: two-letter ISO 3166-1 codes such
    /// as `US`, at most 50, stored upper-case. A request whose client country
    /// is listed is allowed even from outside `allowed_networks`. The country
    /// comes from the edge, and only for public client addresses, so a
    /// request from a private network has none. When both lists are empty
    /// the key works from anywhere.
    #[serde(default)]
    pub allowed_countries: Vec<String>,
}

/// Body of `PUT /api/v1/keys/{id}`. An omitted field keeps its stored value;
/// `null` clears a cap, a budget or an allowlist.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateKey {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    /// Cumulative token ceiling for the current key term. `null` clears the cap.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub budget_tokens: Option<Option<i64>>,
    /// Cumulative USD-cost ceiling for the current key term. `null` clears the cap.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<f64>)]
    pub budget_cost_usd: Option<Option<f64>>,
    /// Reset period: `lifetime`, `monthly`, or `term`. `null` = lifetime.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub budget_period: Option<Option<String>>,
    /// When the current key term began.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<chrono::DateTime<chrono::Utc>>)]
    pub budget_started_at: Option<Option<chrono::DateTime<chrono::Utc>>>,
    /// Fairshare weight among the tenant's keys, at least 1.
    #[serde(default)]
    pub weight: Option<i64>,
    /// Per-model in-flight ceiling for this key. `null` clears the cap.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub max_in_flight: Option<Option<i64>>,
    /// Networks the key may be used from (see `CreateKey::allowed_networks`).
    /// Left out, the key keeps its list, so an edit that does not mention it
    /// never opens a limited key to every network; `[]` or `null` allows any
    /// network again.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Vec<String>>)]
    pub allowed_networks: Option<Option<Vec<String>>>,
    /// Countries the key may be used from (see
    /// `CreateKey::allowed_countries`). Left out, the key keeps its list;
    /// `[]` or `null` clears it.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<Vec<String>>)]
    pub allowed_countries: Option<Option<Vec<String>>>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct CreatedKey {
    pub key: ApiKey,
    /// The raw secret, shown exactly once. Store it now; it cannot be retrieved.
    pub secret: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetDisabled {
    pub disabled: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetKeyTracing {
    pub tracing_enabled: bool,
}

/// Body of `PUT /api/v1/keys/{id}/end-user-fairshare`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetKeyEndUserFairshare {
    /// Whether each end user the key names (`x-obleth-end-user`, else the
    /// body's `user`) queues on their own. Only for a trusted caller: see
    /// `ApiKey::end_user_fairshare`.
    pub end_user_fairshare: bool,
}

/// Body of `PUT /api/v1/keys/{id}/background`.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetKeyBackground {
    /// Whether the key's requests run in each model's background pool, taking
    /// only spare capacity. See `ApiKey::background`.
    pub background: bool,
}

pub(crate) fn normalize_budget_fields(
    budget_tokens: Option<i64>,
    budget_cost_usd: Option<f64>,
    budget_period: Option<&str>,
    budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    token_field: &str,
    cost_field: &str,
    period_field: &str,
) -> Result<(Option<String>, Option<chrono::DateTime<chrono::Utc>>)> {
    let period = match budget_period.map(str::trim) {
        None | Some("") => None,
        Some(p) => {
            let p = p.to_lowercase();
            if !matches!(p.as_str(), "lifetime" | "monthly" | "term") {
                return Err(AdminError::BadRequest(format!(
                    "{period_field} must be one of: lifetime, monthly, term",
                )));
            }
            Some(p)
        }
    };
    if let Some(tokens) = budget_tokens {
        if tokens < 0 {
            return Err(AdminError::BadRequest(format!(
                "{token_field} must be non-negative",
            )));
        }
    }
    if let Some(cost) = budget_cost_usd {
        if cost < 0.0 || !cost.is_finite() {
            return Err(AdminError::BadRequest(format!(
                "{cost_field} must be a non-negative number",
            )));
        }
    }
    let started_at = budget_started_at
        .or_else(|| (budget_tokens.is_some() || budget_cost_usd.is_some()).then(chrono::Utc::now));
    Ok((period, started_at))
}

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct ListKeysQuery {
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    /// Only these keys: one id or up to 200 joined by commas.
    #[serde(default, deserialize_with = "usage::key_id_list")]
    #[param(value_type = Option<String>)]
    #[schema(value_type = Option<String>)]
    pub ids: Vec<Uuid>,
    /// Page size, 1 to 1,000. Without it every matching key comes back.
    pub limit: Option<i64>,
    /// Continue after this key (the last id of the previous page), in the
    /// default oldest-first order. Other orders page with `cursor`.
    #[schema(value_type = Option<String>)]
    pub after: Option<Uuid>,
    /// Continue where the previous page ended: its `x-obleth-next-cursor`
    /// header, sent back as is with the same `sort`, `dir`, filters and
    /// window.
    pub cursor: Option<String>,
    /// Only keys whose name, prefix, description, identity subject or
    /// issuer, or tenant's name, contains this text, ignoring case.
    pub q: Option<String>,
    /// Only these fields of each key, comma-separated (`id,name,key_prefix,tenant_id`
    /// for a picker). Without it, every field.
    pub fields: Option<String>,
    /// Only keys turned off (`true`) or on (`false`).
    pub disabled: Option<bool>,
    /// Only `secret` or `identity` keys.
    pub kind: Option<String>,
    /// `capped`: only keys with a token or cost cap; `none`: only keys
    /// without; `near`: only keys that have used 80% or more of a cap this
    /// period.
    pub budget: Option<String>,
    /// Only keys with no use in the window (`since_ms` to `until_ms`).
    pub unused: Option<bool>,
    /// The order: `created` (default), `name` (ignoring case), or, highest
    /// first, `last_used`, `requests`, `tokens` or `cost` in the window, or
    /// `budget` (the share of its cap used this period). The use and budget
    /// orders list the keys without use (or without a cap) after the rest,
    /// by name. Ties go by id.
    pub sort: Option<String>,
    /// `asc` or `desc`, for `created` and `name` (default `asc`). The other
    /// orders are highest first only.
    pub dir: Option<String>,
    /// Start of the window the use orders and `unused` read, unix epoch
    /// millis. Defaults to 30 days before `until_ms`. Send the same window
    /// with every page so the order holds still.
    pub since_ms: Option<i64>,
    /// End of that window (exclusive), unix epoch millis. Defaults to now.
    pub until_ms: Option<i64>,
}

/// Most keys one page of `GET /keys` returns.
const KEYS_PAGE_MAX: i64 = 1_000;

/// The fields of an `ApiKey` a `GET /keys?fields=` projection may name.
pub(crate) const KEY_FIELDS: &[&str] = &[
    "id",
    "tenant_id",
    "name",
    "description",
    "key_prefix",
    "kind",
    "identity_issuer",
    "identity_subject",
    "identity_claims",
    "budget_tokens",
    "budget_cost_usd",
    "budget_period",
    "budget_started_at",
    "weight",
    "max_in_flight",
    "disabled",
    "tracing_enabled",
    "end_user_fairshare",
    "background",
    "allowed_networks",
    "allowed_countries",
    "created_at",
    "updated_at",
];

/// The fields a `fields=` list names, or a 400 naming the first unknown one.
fn key_projection(fields: &str) -> Result<Vec<&str>> {
    let names: Vec<&str> = fields
        .split(',')
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .collect();
    if names.is_empty() {
        return Err(AdminError::BadRequest("fields names no field".into()));
    }
    if let Some(bad) = names.iter().find(|f| !KEY_FIELDS.contains(f)) {
        return Err(AdminError::BadRequest(format!(
            "fields: a key has no field {bad:?}"
        )));
    }
    Ok(names)
}

#[utoipa::path(
    post, path = "/api/v1/tenants/{id}/keys", tag = "keys",
    request_body = CreateKey,
    responses((status = 200, body = CreatedKey))
)]
pub(crate) async fn create_key(
    State(state): State<AdminState>,
    Path(tenant_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<CreateKey>,
) -> Result<Json<CreatedKey>> {
    // The reserved control-plane tenant is system-owned and not user-manageable.
    if tenant_id == Store::CONTROL_PLANE_TENANT_ID {
        return Err(AdminError::Store(obleth_store::StoreError::Protected(
            "the reserved control-plane tenant cannot be modified".into(),
        )));
    }
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AdminError::BadRequest("key name is required".into()));
    }
    if body.weight < 1 {
        return Err(AdminError::BadRequest("weight must be >= 1".into()));
    }
    if matches!(body.max_in_flight, Some(c) if c < 1) {
        return Err(AdminError::BadRequest("max_in_flight must be >= 1".into()));
    }
    let description = body.description.trim().to_string();
    let (period, started_at) = normalize_budget_fields(
        body.budget_tokens,
        body.budget_cost_usd,
        body.budget_period.as_deref(),
        body.budget_started_at,
        "budget_tokens",
        "budget_cost_usd",
        "budget_period",
    )?;
    let allowed_networks =
        obleth_config::network::normalize_allowed_networks(&body.allowed_networks)
            .map_err(AdminError::BadRequest)?;
    let allowed_countries =
        obleth_config::network::normalize_allowed_countries(&body.allowed_countries)
            .map_err(AdminError::BadRequest)?;
    let (key, secret) = state
        .store
        .create_api_key(
            tenant_id,
            &name,
            &description,
            body.budget_tokens,
            body.budget_cost_usd,
            period.as_deref(),
            started_at,
            body.weight,
            body.max_in_flight,
            &allowed_networks,
            &allowed_countries,
        )
        .await?;
    let hash = hash_api_key(&secret);
    if let Some(resolved) = state.store.resolved_key_by_hash(&hash).await? {
        push_key(&state, &hash, &resolved).await?;
    }
    state
        .audit(
            &headers,
            "create_key",
            "api_key",
            key.id,
            serde_json::json!({
                "tenant_id": tenant_id,
                "prefix": key.key_prefix,
                "budget_tokens": key.budget_tokens,
                "budget_cost_usd": key.budget_cost_usd,
                "budget_period": key.budget_period,
                "budget_started_at": key.budget_started_at,
                "weight": key.weight,
                "max_in_flight": key.max_in_flight,
                "allowed_networks": key.allowed_networks,
                "allowed_countries": key.allowed_countries,
            }),
        )
        .await?;
    Ok(Json(CreatedKey { key, secret }))
}

/// How much of one budget cap its current period has used, from the same
/// counters the gateway enforces the cap with.
#[derive(Debug, Serialize, ToSchema)]
pub struct BudgetUsage {
    /// `tenant` or `key`.
    pub scope: String,
    #[schema(value_type = String)]
    pub id: Uuid,
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    /// `monthly`, `term` or `lifetime`.
    pub period: String,
    pub budget_tokens: Option<i64>,
    pub budget_cost_usd: Option<f64>,
    pub used_tokens: i64,
    pub used_cost_usd: f64,
    /// When the current period began (a month's first day in the tenant's
    /// time zone; a term's or lifetime's start, when set).
    pub period_start: Option<chrono::DateTime<chrono::Utc>>,
    /// When a monthly budget next resets; none for term and lifetime.
    pub resets_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// Filters of `GET /budgets/usage`.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BudgetUsageQuery {
    /// One tenant: its own budget and its keys'.
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    /// Only these keys' budgets (no tenant's): one id or up to 200 joined by
    /// commas, for a page of the Keys list.
    #[serde(default, deserialize_with = "usage::key_id_list")]
    #[param(value_type = Option<String>)]
    #[schema(value_type = Option<String>)]
    pub ids: Vec<Uuid>,
}

/// Every tenant and key budget with a cap, or one tenant's, and how much of
/// its current period it has used. Reads the enforcement counters without
/// changing them, so a period that has rolled over reads as zero until its
/// first request.
#[utoipa::path(
    get, path = "/api/v1/budgets/usage", tag = "keys",
    params(BudgetUsageQuery),
    responses((status = 200, body = [BudgetUsage]))
)]
pub(crate) async fn get_budget_usage(
    State(state): State<AdminState>,
    Query(q): Query<BudgetUsageQuery>,
) -> Result<Json<Vec<BudgetUsage>>> {
    let now = chrono::Utc::now();
    let tenants = match q.tenant_id {
        Some(id) => vec![state.store.get_tenant(id).await?],
        None => state.store.list_tenants().await?,
    };
    let zones: std::collections::HashMap<Uuid, String> =
        tenants.iter().map(|t| (t.id, t.timezone.clone())).collect();
    let mut wanted = Vec::new();
    for t in tenants.iter().filter(|_| q.ids.is_empty()) {
        if t.id == Store::CONTROL_PLANE_TENANT_ID {
            continue;
        }
        if let Some(period) = obleth_config::budget::period_key(
            t.budget_tokens,
            t.budget_cost_usd,
            t.budget_period.as_deref(),
            t.budget_started_at,
            &t.timezone,
            now,
        ) {
            wanted.push(BudgetRead {
                scope: "tenant",
                id: t.id,
                tenant_id: t.id,
                period_key: period,
                period: t.budget_period.clone(),
                budget_tokens: t.budget_tokens,
                budget_cost_usd: t.budget_cost_usd,
                started_at: t.budget_started_at,
                tz: t.timezone.clone(),
            });
        }
    }
    let keys = state
        .store
        .list_keys_where(&obleth_store::KeyListFilter {
            tenant_id: q.tenant_id,
            ids: (!q.ids.is_empty()).then_some(q.ids),
            capped: Some(true),
            ..Default::default()
        })
        .await?;
    wanted.extend(key_budget_reads(&keys, &zones, now));
    Ok(Json(read_budgets(&state, &wanted, now).await?))
}

/// One budget whose current period `read_budgets` reads.
pub(crate) struct BudgetRead {
    scope: &'static str,
    id: Uuid,
    tenant_id: Uuid,
    period_key: String,
    period: Option<String>,
    budget_tokens: Option<i64>,
    budget_cost_usd: Option<f64>,
    started_at: Option<chrono::DateTime<chrono::Utc>>,
    tz: String,
}

/// The capped keys' budgets to read, in their tenants' time zones (`zones`,
/// by tenant id; UTC for one not there). The control plane's are left out.
pub(crate) fn key_budget_reads(
    keys: &[ApiKey],
    zones: &std::collections::HashMap<Uuid, String>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<BudgetRead> {
    keys.iter()
        .filter(|k| k.tenant_id != Store::CONTROL_PLANE_TENANT_ID)
        .filter_map(|k| {
            let tz = zones
                .get(&k.tenant_id)
                .cloned()
                .unwrap_or_else(|| "UTC".into());
            let period_key = obleth_config::budget::period_key(
                k.budget_tokens,
                k.budget_cost_usd,
                k.budget_period.as_deref(),
                k.budget_started_at,
                &tz,
                now,
            )?;
            Some(BudgetRead {
                scope: "key",
                id: k.id,
                tenant_id: k.tenant_id,
                period_key,
                period: k.budget_period.clone(),
                budget_tokens: k.budget_tokens,
                budget_cost_usd: k.budget_cost_usd,
                started_at: k.budget_started_at,
                tz,
            })
        })
        .collect()
}

/// How much of each budget's current period is used, from the enforcement
/// counters, 32 reads at a time.
pub(crate) async fn read_budgets(
    state: &AdminState,
    wanted: &[BudgetRead],
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Vec<BudgetUsage>> {
    let mut out = Vec::with_capacity(wanted.len());
    for chunk in wanted.chunks(32) {
        let reads = chunk
            .iter()
            .map(|w| state.redis.term_usage_peek(&w.id, &w.period_key));
        let used = futures::future::join_all(reads).await;
        for (w, u) in chunk.iter().zip(used) {
            let (used_tokens, used_cost_usd) = u?;
            let (period_start, resets_at) =
                obleth_config::budget::period_bounds(w.period.as_deref(), w.started_at, &w.tz, now);
            out.push(BudgetUsage {
                scope: w.scope.to_string(),
                id: w.id,
                tenant_id: w.tenant_id,
                period: w.period.clone().unwrap_or_else(|| "lifetime".into()),
                budget_tokens: w.budget_tokens,
                budget_cost_usd: w.budget_cost_usd,
                used_tokens,
                used_cost_usd,
                period_start,
                resets_at,
            });
        }
    }
    Ok(out)
}

impl BudgetUsage {
    /// The larger of its cost and token shares (a cap of 0 counts as no
    /// share), as the Keys page shows it.
    pub fn share(&self) -> f64 {
        let cost = match self.budget_cost_usd {
            Some(cap) if cap != 0.0 => self.used_cost_usd / cap,
            _ => 0.0,
        };
        let tokens = match self.budget_tokens {
            Some(cap) if cap != 0 => self.used_tokens as f64 / cap as f64,
            _ => 0.0,
        };
        cost.max(tokens)
    }
}

/// API keys, oldest first unless `sort` says otherwise. `tenant_id`, `ids`,
/// `q`, `disabled`, `kind`, `budget` and `unused` narrow the list. With
/// `limit` it comes in pages: while more follow, the response carries
/// `x-obleth-truncated: true` and `x-obleth-next-cursor`, which the next
/// request sends back as `cursor` (the oldest-first order also takes the
/// last key's id as `after`). Without `limit`, every matching key in one
/// array. `fields` sends only the named fields of each key, for pickers that
/// need names and prefixes.
#[utoipa::path(
    get, path = "/api/v1/keys", tag = "keys",
    params(ListKeysQuery),
    responses(
        (status = 200, body = [ApiKey], headers(
            ("x-obleth-truncated" = String, description = "`true` when keys past this page match"),
            ("x-obleth-next-cursor" = String, description = "Where the next page starts, sent back as `cursor`; only while more follow")
        )),
        (status = 400)
    )
)]
pub(crate) async fn list_keys(
    State(state): State<AdminState>,
    Query(q): Query<ListKeysQuery>,
) -> Result<Response> {
    let projection = q.fields.as_deref().map(key_projection).transpose()?;
    let limit = q.limit.map(|n| n.clamp(1, KEYS_PAGE_MAX));
    let keys_page::KeyPage { keys, more, next } = keys_page::list_page(&state, &q, limit).await?;
    let mut headers = truncated_header(more);
    if let Some(cursor) = next.and_then(|c| axum::http::HeaderValue::from_str(&c).ok()) {
        headers.insert("x-obleth-next-cursor", cursor);
    }
    let Some(fields) = projection else {
        return Ok(axum::response::IntoResponse::into_response((
            headers,
            Json(keys),
        )));
    };
    let projected = keys
        .into_iter()
        .map(|key| match serde_json::to_value(key) {
            Ok(serde_json::Value::Object(mut all)) => Ok(fields
                .iter()
                .filter_map(|f| all.remove_entry(*f))
                .collect::<serde_json::Map<_, _>>()),
            Ok(_) => Err(AdminError::Internal("a key is not an object".into())),
            Err(e) => Err(AdminError::Internal(e.to_string())),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(axum::response::IntoResponse::into_response((
        headers,
        Json(projected),
    )))
}

/// One API key. The reserved control-plane key reads as missing.
#[utoipa::path(
    get, path = "/api/v1/keys/{id}", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    responses((status = 200, body = ApiKey), (status = 404))
)]
pub(crate) async fn get_key(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ApiKey>> {
    state
        .store
        .keys_by_ids(&[id])
        .await?
        .into_iter()
        .find(|k| k.tenant_id != Store::CONTROL_PLANE_TENANT_ID)
        .map(Json)
        .ok_or(AdminError::NotFound)
}

#[derive(Serialize)]
pub(crate) struct ControlPlaneKeyView {
    secret: String,
}

/// Admin-gated: hand the server-side control-plane (Charo) its reserved API key
/// secret so it can call the data plane on the operator's behalf. The secret is
/// decrypted from `app_settings` and must never reach the browser.
pub(crate) async fn get_control_plane_key(
    State(state): State<AdminState>,
) -> Result<Json<ControlPlaneKeyView>> {
    match state.store.control_plane_key_secret().await? {
        Some(secret) => Ok(Json(ControlPlaneKeyView { secret })),
        None => Err(AdminError::Internal(
            "control-plane identity not provisioned".into(),
        )),
    }
}

#[utoipa::path(
    put, path = "/api/v1/keys/{id}", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    request_body = UpdateKey,
    responses((status = 200, body = ApiKey), (status = 404))
)]
pub(crate) async fn update_key(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateKey>,
) -> Result<Json<ApiKey>> {
    let existing = state
        .store
        .keys_by_ids(&[id])
        .await?
        .into_iter()
        .next()
        .ok_or(obleth_store::StoreError::NotFound)?;
    let name = body
        .name
        .as_deref()
        .map(str::trim)
        .unwrap_or(&existing.name)
        .to_string();
    if name.is_empty() {
        return Err(AdminError::BadRequest("key name is required".into()));
    }
    let weight = body.weight.unwrap_or(existing.weight);
    if weight < 1 {
        return Err(AdminError::BadRequest("weight must be >= 1".into()));
    }
    let max_in_flight = body.max_in_flight.unwrap_or(existing.max_in_flight);
    if matches!(max_in_flight, Some(c) if c < 1) {
        return Err(AdminError::BadRequest("max_in_flight must be >= 1".into()));
    }
    let description = body
        .description
        .as_deref()
        .map(str::trim)
        .unwrap_or(&existing.description)
        .to_string();
    let budget_tokens = body.budget_tokens.unwrap_or(existing.budget_tokens);
    let budget_cost_usd = body.budget_cost_usd.unwrap_or(existing.budget_cost_usd);
    let budget_period = body
        .budget_period
        .unwrap_or_else(|| existing.budget_period.clone());
    let (period, started_at) = normalize_budget_fields(
        budget_tokens,
        budget_cost_usd,
        budget_period.as_deref(),
        body.budget_started_at.unwrap_or(existing.budget_started_at),
        "budget_tokens",
        "budget_cost_usd",
        "budget_period",
    )?;
    let allowed_networks = body
        .allowed_networks
        .map(|list| obleth_config::network::normalize_allowed_networks(&list.unwrap_or_default()))
        .transpose()
        .map_err(AdminError::BadRequest)?;
    let allowed_countries = body
        .allowed_countries
        .map(|list| obleth_config::network::normalize_allowed_countries(&list.unwrap_or_default()))
        .transpose()
        .map_err(AdminError::BadRequest)?;
    let (hash, key, resolved) = state
        .store
        .update_api_key(
            id,
            &name,
            &description,
            budget_tokens,
            budget_cost_usd,
            period.as_deref(),
            started_at,
            weight,
            max_in_flight,
            allowed_networks.as_deref(),
            allowed_countries.as_deref(),
        )
        .await?;
    // Same path as disabling a key: Redis is rewritten and every replica
    // drops its local copy, so a narrowed list applies within moments.
    push_key(&state, &hash, &resolved).await?;
    state
        .audit(
            &headers,
            "update_key",
            "api_key",
            id,
            serde_json::json!({
                "tenant_id": key.tenant_id,
                "prefix": key.key_prefix,
                "budget_tokens": key.budget_tokens,
                "budget_cost_usd": key.budget_cost_usd,
                "budget_period": key.budget_period,
                "budget_started_at": key.budget_started_at,
                "weight": key.weight,
                "max_in_flight": key.max_in_flight,
                "allowed_networks": key.allowed_networks,
                "allowed_countries": key.allowed_countries,
            }),
        )
        .await?;
    Ok(Json(key))
}

#[utoipa::path(
    delete, path = "/api/v1/keys/{id}", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    responses((status = 204), (status = 404))
)]
pub(crate) async fn delete_key(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let hash = state.store.delete_key(id).await?;
    let evicted = evict_keys(&state, std::slice::from_ref(&hash), "the key").await;
    // Audited before an eviction failure is reported: the row is gone.
    state
        .audit(
            &headers,
            "delete_key",
            "api_key",
            id,
            serde_json::json!({ "cache_evicted": evicted.is_ok() }),
        )
        .await?;
    evicted?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put, path = "/api/v1/keys/{id}/disabled", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    request_body = SetDisabled,
    responses((status = 200, body = ApiKey), (status = 404))
)]
pub(crate) async fn set_key_disabled(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetDisabled>,
) -> Result<Json<ApiKey>> {
    set_key_flag(&state, &headers, id, KeyFlag::Disabled, body.disabled).await
}

/// Turn one of a key's own on/off settings, push the key to Redis, audit the
/// change, and answer the updated key. Disabling is audited as
/// `disable_key`/`enable_key`; the others as `enable_key_<setting>` and
/// `disable_key_<setting>`.
async fn set_key_flag(
    state: &AdminState,
    headers: &HeaderMap,
    id: Uuid,
    flag: KeyFlag,
    on: bool,
) -> Result<Json<ApiKey>> {
    let (hash, key, resolved) = state.store.set_key_flag(id, flag, on).await?;
    push_key(state, &hash, &resolved).await?;
    let action = match (flag, on) {
        (KeyFlag::Disabled, true) => "disable_key".to_string(),
        (KeyFlag::Disabled, false) => "enable_key".to_string(),
        (_, on) => format!(
            "{}_key_{}",
            if on { "enable" } else { "disable" },
            flag.column().trim_end_matches("_enabled")
        ),
    };
    state
        .audit(
            headers,
            &action,
            "api_key",
            id,
            serde_json::json!({ flag.column(): on }),
        )
        .await?;
    Ok(Json(key))
}

/// Move a key to another tenant, keeping its secret and settings. From now
/// on its requests count against the new tenant's limits, budget and share;
/// past usage stays recorded under the tenant it was made in.
#[utoipa::path(
    put, path = "/api/v1/keys/{id}/tenant", tag = "keys",
    params(("id" = Uuid, Path, description = "Key id")),
    request_body = MoveKey,
    responses((status = 200, body = ApiKey), (status = 404, description = "No such key or tenant"))
)]
pub(crate) async fn move_key(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<MoveKey>,
) -> Result<Json<ApiKey>> {
    let (hash, key, resolved, from) = state.store.move_api_key(id, body.tenant_id).await?;
    push_key(&state, &hash, &resolved).await?;
    state.audit(&headers, "move_key", "api_key", id, serde_json::json!({ "prefix": key.key_prefix, "from": from, "tenant_id": key.tenant_id }))
        .await?;
    Ok(Json(key))
}

#[utoipa::path(
    put, path = "/api/v1/keys/{id}/tracing", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    request_body = SetKeyTracing,
    responses((status = 200, body = ApiKey), (status = 404))
)]
pub(crate) async fn set_key_tracing_handler(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetKeyTracing>,
) -> Result<Json<ApiKey>> {
    set_key_flag(&state, &headers, id, KeyFlag::Tracing, body.tracing_enabled).await
}

/// Turn per-end-user fairshare on or off for one key. Its own route, like
/// tracing, because `PUT /api/v1/keys/{id}` resets any field it is not sent,
/// and a bulk budget edit must never switch this off by leaving it out.
#[utoipa::path(
    put, path = "/api/v1/keys/{id}/end-user-fairshare", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    request_body = SetKeyEndUserFairshare,
    responses((status = 200, body = ApiKey), (status = 404))
)]
pub(crate) async fn set_key_end_user_fairshare_handler(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetKeyEndUserFairshare>,
) -> Result<Json<ApiKey>> {
    set_key_flag(
        &state,
        &headers,
        id,
        KeyFlag::EndUserFairshare,
        body.end_user_fairshare,
    )
    .await
}

/// Turn background priority on or off for one key. Its own route, like
/// per-end-user fairshare, because `PUT /api/v1/keys/{id}` resets any field
/// it is not sent, and a bulk budget edit must never switch this off by
/// leaving it out.
#[utoipa::path(
    put, path = "/api/v1/keys/{id}/background", tag = "keys",
    params(("id" = Uuid, Path, description = "API key id")),
    request_body = SetKeyBackground,
    responses((status = 200, body = ApiKey), (status = 404))
)]
pub(crate) async fn set_key_background_handler(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetKeyBackground>,
) -> Result<Json<ApiKey>> {
    set_key_flag(&state, &headers, id, KeyFlag::Background, body.background).await
}
