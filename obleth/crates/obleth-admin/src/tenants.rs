//! Tenants and fair-share groups: their request bodies and handlers.

use super::*;

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateTenant {
    pub name: String,
    pub weight: Option<i64>,
    pub tokens_per_minute: Option<i64>,
    pub max_in_flight: Option<i64>,
    pub fairshare_group: Option<String>,
    /// Mark the tenant synthetic at creation (benchmark/test traffic).
    #[serde(default)]
    pub synthetic: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTenant {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default)]
    pub contact_email: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantStatus {
    pub status: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantSynthetic {
    pub synthetic: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantSchedule {
    /// IANA timezone the windows/cutoffs are evaluated in (e.g. `America/Phoenix`).
    #[serde(default = "obleth_config::default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub active_from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub active_until: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub weekly_windows: Option<Vec<WeeklyWindow>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantBudget {
    /// Cumulative token ceiling for the current term. `null` clears the cap.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Cumulative USD-cost ceiling for the current term. `null` clears the cap.
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    /// Reset period: `lifetime`, `monthly`, or `term`. `null` = lifetime.
    #[serde(default)]
    pub budget_period: Option<String>,
    /// When the current term began (used by `term`/`lifetime` reset semantics).
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantAllowlist {
    /// Permitted model names. An empty list clears the allowlist (all permitted).
    #[serde(default)]
    pub allowed_models: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantGuardrails {
    /// `null` clears the guardrails policy (no scanning for this tenant).
    pub policy: Option<GuardrailsPolicy>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetTenantCompression {
    /// `null` clears the compression policy (tenant follows the global default).
    pub policy: Option<CompressionPolicy>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateFairshareGroup {
    pub name: String,
    pub weight: Option<i64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateGroupWeight {
    pub weight: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateTenantGroup {
    pub fairshare_group: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateWeight {
    pub weight: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateQuota {
    pub tokens_per_minute: i64,
    pub max_in_flight: Option<i64>,
}

#[utoipa::path(
    post, path = "/api/v1/tenants", tag = "tenants",
    request_body = CreateTenant,
    responses((status = 200, body = Tenant))
)]
pub(crate) async fn create_tenant(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateTenant>,
) -> Result<Json<Tenant>> {
    let mut tenant = state
        .store
        .create_tenant(
            &body.name,
            validate_weight(body.weight.unwrap_or(100))?,
            body.tokens_per_minute.unwrap_or(0),
            body.max_in_flight,
            body.fairshare_group.as_deref(),
        )
        .await?;
    if body.synthetic == Some(true) {
        tenant = state.store.set_tenant_synthetic(tenant.id, true).await?;
    }
    state
        .audit(
            &headers,
            "create_tenant",
            "tenant",
            tenant.id,
            serde_json::to_value(&tenant).unwrap_or_default(),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(get, path = "/api/v1/tenants", tag = "tenants", responses((status = 200, body = [Tenant])))]
pub(crate) async fn list_tenants(State(state): State<AdminState>) -> Result<Json<Vec<Tenant>>> {
    let mut tenants = state.store.list_tenants().await?;
    // The reserved control-plane identity (Charo) is system-owned: hide it from
    // the management surface so it can't be edited/deleted by mistake.
    tenants.retain(|t| t.id != Store::CONTROL_PLANE_TENANT_ID);
    Ok(Json(tenants))
}

#[utoipa::path(
    get, path = "/api/v1/tenants/{id}", tag = "tenants",
    params(("id" = Uuid, Path, description = "Tenant id")),
    responses((status = 200, body = Tenant), (status = 404))
)]
pub(crate) async fn get_tenant(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Tenant>> {
    Ok(Json(state.store.get_tenant(id).await?))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}", tag = "tenants",
    request_body = UpdateTenant,
    responses((status = 200, body = Tenant))
)]
pub(crate) async fn update_tenant(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateTenant>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .update_tenant_details(
            id,
            &body.name,
            &body.description,
            &body.organization,
            &body.contact_email,
        )
        .await?;
    // Name is denormalized into every resolved key; re-push the tenant's keys.
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            "update_tenant",
            "tenant",
            id,
            serde_json::to_value(&tenant).unwrap_or_default(),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/status", tag = "tenants",
    request_body = SetTenantStatus,
    responses((status = 200, body = Tenant))
)]
pub(crate) async fn set_tenant_status(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetTenantStatus>,
) -> Result<Json<Tenant>> {
    let status = body.status.trim().to_lowercase();
    if !matches!(status.as_str(), "active" | "suspended" | "archived") {
        return Err(AdminError::BadRequest(
            "status must be one of: active, suspended, archived".into(),
        ));
    }
    let tenant = state.store.set_tenant_status(id, &status).await?;
    // Status gates admission in the data plane; refresh the cached keys.
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            "set_tenant_status",
            "tenant",
            id,
            serde_json::json!({ "status": status }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/schedule", tag = "tenants",
    request_body = SetTenantSchedule,
    responses((status = 200, body = Tenant))
)]
pub(crate) async fn set_tenant_schedule(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetTenantSchedule>,
) -> Result<Json<Tenant>> {
    let timezone = body.timezone.trim();
    if timezone.parse::<chrono_tz::Tz>().is_err() {
        return Err(AdminError::BadRequest(format!(
            "unknown timezone '{timezone}'; use an IANA name like 'America/Phoenix' or 'UTC'"
        )));
    }
    if let (Some(from), Some(until)) = (body.active_from, body.active_until) {
        if until <= from {
            return Err(AdminError::BadRequest(
                "active_until must be after active_from".into(),
            ));
        }
    }
    if let Some(windows) = &body.weekly_windows {
        for w in windows {
            if w.day > 6 {
                return Err(AdminError::BadRequest(
                    "weekly window day must be 0 (Sunday) through 6 (Saturday)".into(),
                ));
            }
            if w.start_min > 1440 || w.end_min > 1440 || w.start_min >= w.end_min {
                return Err(AdminError::BadRequest(
                    "weekly window minutes must satisfy 0 <= start_min < end_min <= 1440".into(),
                ));
            }
        }
    }
    let tenant = state
        .store
        .update_tenant_schedule(
            id,
            timezone,
            body.active_from,
            body.active_until,
            body.weekly_windows,
        )
        .await?;
    // Schedule gates admission in the data plane; refresh the cached keys.
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            "set_tenant_schedule",
            "tenant",
            id,
            serde_json::json!({
                "timezone": tenant.timezone,
                "active_from": tenant.active_from,
                "active_until": tenant.active_until,
                "weekly_windows": tenant.weekly_windows,
            }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/budget", tag = "tenants",
    request_body = SetTenantBudget,
    responses((status = 200, body = Tenant))
)]
pub(crate) async fn set_tenant_budget(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetTenantBudget>,
) -> Result<Json<Tenant>> {
    let (period, started_at) = normalize_budget_fields(
        body.budget_tokens,
        body.budget_cost_usd,
        body.budget_period.as_deref(),
        body.budget_started_at,
        "budget_tokens",
        "budget_cost_usd",
        "budget_period",
    )?;
    let tenant = state
        .store
        .update_tenant_budget(
            id,
            body.budget_tokens,
            body.budget_cost_usd,
            period.as_deref(),
            started_at,
        )
        .await?;
    // Budget gates admission in the data plane; refresh the cached keys.
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            "set_tenant_budget",
            "tenant",
            id,
            serde_json::json!({
                "budget_tokens": tenant.budget_tokens,
                "budget_cost_usd": tenant.budget_cost_usd,
                "budget_period": tenant.budget_period,
                "budget_started_at": tenant.budget_started_at,
            }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/allowlist", tag = "tenants",
    request_body = SetTenantAllowlist,
    responses((status = 200, body = Tenant))
)]
pub(crate) async fn set_tenant_allowlist(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetTenantAllowlist>,
) -> Result<Json<Tenant>> {
    // Normalize: trim, drop blanks, de-duplicate while preserving order.
    let mut seen = std::collections::HashSet::new();
    let models: Vec<String> = body
        .allowed_models
        .into_iter()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty() && seen.insert(m.clone()))
        .collect();
    let allowed = (!models.is_empty()).then_some(models);
    let tenant = state.store.update_tenant_allowlist(id, allowed).await?;
    // Allowlist gates admission in the data plane; refresh the cached keys.
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            "set_tenant_allowlist",
            "tenant",
            id,
            serde_json::json!({ "allowed_models": tenant.allowed_models }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/guardrails", tag = "tenants",
    request_body = SetTenantGuardrails,
    responses((status = 200, body = Tenant))
)]
pub(crate) async fn set_tenant_guardrails(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetTenantGuardrails>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .update_tenant_guardrails_policy(id, body.policy)
        .await?;
    // Guardrails gate data-plane behaviour; refresh the cached keys.
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            "set_tenant_guardrails",
            "tenant",
            id,
            serde_json::json!({ "guardrails_policy": tenant.guardrails_policy }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/compression", tag = "tenants",
    request_body = SetTenantCompression,
    responses((status = 200, body = Tenant))
)]
pub(crate) async fn set_tenant_compression(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetTenantCompression>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .update_tenant_compression_policy(id, body.policy)
        .await?;
    // Compression gating reads the cached key; refresh it.
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            "set_tenant_compression",
            "tenant",
            id,
            serde_json::json!({ "compression_policy": tenant.compression_policy }),
        )
        .await?;
    Ok(Json(tenant))
}

//
// The store decrypts upstream secrets on every read, so the config structs
// carry plaintext keys. Every Management API response (and audit detail) for
// models, endpoints, and MCP servers goes through these views instead: secrets
// are write-only and only their presence is reported. The `From` impls
// destructure exhaustively so a new config field fails to compile here rather
// than silently going missing from the API.

#[utoipa::path(
    delete, path = "/api/v1/tenants/{id}", tag = "tenants",
    responses((status = 204))
)]
pub(crate) async fn delete_tenant(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let hashes = state.store.delete_tenant(id).await?;
    // Evict every cascaded key from the data-plane cache.
    let evicted = evict_keys(&state, &hashes, "the tenant and its keys").await;
    // The Postgres delete stands either way, so it is audited before an
    // eviction failure is reported.
    state
        .audit(
            &headers,
            "delete_tenant",
            "tenant",
            id,
            serde_json::json!({ "keys_removed": hashes.len(), "cache_evicted": evicted.is_ok() }),
        )
        .await?;
    evicted?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/weight", tag = "tenants",
    request_body = UpdateWeight,
    responses((status = 200, body = Tenant))
)]
pub(crate) async fn set_tenant_weight(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateWeight>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .update_tenant_weight(id, validate_weight(body.weight)?)
        .await?;
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            "update_weight",
            "tenant",
            id,
            serde_json::json!({ "weight": body.weight }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/quota", tag = "tenants",
    params(("id" = Uuid, Path, description = "Tenant id")),
    request_body = UpdateQuota,
    responses((status = 200, body = Tenant))
)]
pub(crate) async fn set_tenant_quota(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateQuota>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .update_tenant_quota(id, body.tokens_per_minute, body.max_in_flight)
        .await?;
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            "update_quota",
            "tenant",
            id,
            serde_json::json!({
                "tokens_per_minute": body.tokens_per_minute,
                "max_in_flight": body.max_in_flight
            }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/tracing", tag = "tenants",
    params(("id" = Uuid, Path, description = "Tenant id")),
    request_body = SetKeyTracing,
    responses((status = 200, body = Tenant), (status = 404))
)]
pub(crate) async fn set_tenant_tracing_handler(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetKeyTracing>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .set_tenant_tracing(id, body.tracing_enabled)
        .await?;
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            if body.tracing_enabled {
                "enable_tenant_tracing"
            } else {
                "disable_tenant_tracing"
            },
            "tenant",
            id,
            serde_json::json!({ "tracing_enabled": body.tracing_enabled }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/synthetic", tag = "tenants",
    params(("id" = Uuid, Path, description = "Tenant id")),
    request_body = SetTenantSynthetic,
    responses((status = 200, body = Tenant), (status = 404))
)]
pub(crate) async fn set_tenant_synthetic_handler(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetTenantSynthetic>,
) -> Result<Json<Tenant>> {
    let tenant = state.store.set_tenant_synthetic(id, body.synthetic).await?;
    // The flag is denormalized into every resolved key; re-push the tenant's keys.
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            "set_tenant_synthetic",
            "tenant",
            id,
            serde_json::json!({ "synthetic": body.synthetic }),
        )
        .await?;
    Ok(Json(tenant))
}

#[utoipa::path(
    get, path = "/api/v1/fairshare/groups", tag = "fairshare",
    responses((status = 200, body = [FairshareGroup]))
)]
pub(crate) async fn list_fairshare_groups(
    State(state): State<AdminState>,
) -> Result<Json<Vec<FairshareGroup>>> {
    Ok(Json(state.store.list_fairshare_groups().await?))
}

#[utoipa::path(
    post, path = "/api/v1/fairshare/groups", tag = "fairshare",
    request_body = CreateFairshareGroup,
    responses((status = 200, body = FairshareGroup))
)]
pub(crate) async fn create_fairshare_group(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateFairshareGroup>,
) -> Result<Json<FairshareGroup>> {
    let group = state
        .store
        .create_fairshare_group(&body.name, validate_weight(body.weight.unwrap_or(100))?)
        .await?;
    state
        .audit(
            &headers,
            "create_fairshare_group",
            "fairshare_group",
            &group.name,
            serde_json::to_value(&group).unwrap_or_default(),
        )
        .await?;
    Ok(Json(group))
}

#[utoipa::path(
    put, path = "/api/v1/fairshare/groups/{name}/weight", tag = "fairshare",
    params(("name" = String, Path, description = "Fairshare group name")),
    request_body = UpdateGroupWeight,
    responses((status = 200, body = FairshareGroup))
)]
pub(crate) async fn set_fairshare_group_weight(
    State(state): State<AdminState>,
    Path(name): Path<String>,
    headers: HeaderMap,
    Json(body): Json<UpdateGroupWeight>,
) -> Result<Json<FairshareGroup>> {
    let group = state
        .store
        .update_fairshare_group_weight(&name, validate_weight(body.weight)?)
        .await?;
    sync_group_keys(&state, &name).await?;
    state
        .audit(
            &headers,
            "update_fairshare_group_weight",
            "fairshare_group",
            &name,
            serde_json::json!({ "weight": body.weight }),
        )
        .await?;
    Ok(Json(group))
}

#[utoipa::path(
    put, path = "/api/v1/tenants/{id}/group", tag = "tenants",
    params(("id" = Uuid, Path, description = "Tenant id")),
    request_body = UpdateTenantGroup,
    responses((status = 200, body = Tenant))
)]
pub(crate) async fn set_tenant_group(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateTenantGroup>,
) -> Result<Json<Tenant>> {
    let tenant = state
        .store
        .update_tenant_fairshare_group(id, &body.fairshare_group)
        .await?;
    sync_tenant_keys(&state, id).await?;
    state
        .audit(
            &headers,
            "update_tenant_group",
            "tenant",
            id,
            serde_json::json!({ "fairshare_group": body.fairshare_group }),
        )
        .await?;
    Ok(Json(tenant))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_tenant_compression_deserializes_null_and_policy() {
        // null clears the policy.
        let cleared: SetTenantCompression = serde_json::from_str(r#"{"policy": null}"#).unwrap();
        assert!(cleared.policy.is_none());

        // A present policy round-trips.
        let set: SetTenantCompression =
            serde_json::from_str(r#"{"policy": {"enabled": true, "allow_lossy": false}}"#).unwrap();
        let p = set.policy.expect("policy present");
        assert!(p.enabled);
        assert!(!p.allow_lossy);
    }

    #[test]
    fn set_tenant_compression_carries_per_piece_flags() {
        let set: SetTenantCompression = serde_json::from_str(
            r#"{"policy":{"enabled":true,"code_compaction":true,"dedup":true,"allow_lossy":true}}"#,
        )
        .unwrap();
        let p = set.policy.expect("policy present");
        assert!(p.enabled && p.code_compaction && p.dedup && p.allow_lossy);
    }
}
