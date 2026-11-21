//! MCP servers.

use super::*;

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateMcpServer {
    pub name: String,
    pub upstream_url: String,
    pub auth_header: Option<String>,
    /// The tenants whose keys may call the server through `/mcp/{name}`.
    /// Left out, no tenant may: the server serves only models granted its tools.
    #[serde(default)]
    pub allowed_tenants: Vec<Uuid>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateMcpServer {
    pub upstream_url: String,
    /// A new Authorization header; omit to keep the stored one.
    pub auth_header: Option<String>,
    /// Remove the stored Authorization header (ignored when `auth_header` is set).
    #[serde(default)]
    pub clear_auth: bool,
    pub enabled: Option<bool>,
    /// The tenants whose keys may call the server through `/mcp/{name}`; omit
    /// to keep the stored list, send `[]` to close the server to direct calls.
    pub allowed_tenants: Option<Vec<Uuid>>,
    /// A new name; the server's `/mcp/<name>` path and every model grant follow it.
    pub name: Option<String>,
}

/// A server's name is the `/mcp/<name>` path segment and the key models grant
/// it by: letters, digits, `.`, `_` and `-`, up to 64, starting with a letter or digit.
fn validate_mcp_server_name(name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if ok {
        Ok(())
    } else {
        Err(AdminError::BadRequest(
            "server name must be 1-64 letters, digits, '.', '_' or '-', starting with a letter or digit".into(),
        ))
    }
}

#[utoipa::path(
    post, path = "/api/v1/mcp-servers", tag = "mcp",
    request_body = CreateMcpServer,
    responses((status = 200, body = McpServerView))
)]
pub(crate) async fn create_mcp_server(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateMcpServer>,
) -> Result<Json<McpServerView>> {
    validate_mcp_server_name(&body.name)?;
    state.ssrf.validate(&body.upstream_url).await?;
    let allowed = mcp_tenants(&state, body.allowed_tenants).await?;
    let server = state
        .store
        .create_mcp_server(
            &body.name,
            &body.upstream_url,
            body.auth_header.as_deref(),
            &allowed,
        )
        .await?;
    sync_mcp_server(&state, &server).await?;
    state
        .audit(
            &headers,
            "create_mcp_server",
            "mcp_server",
            server.id,
            serde_json::json!({
                "name": server.name,
                "upstream_url": server.upstream_url,
                "allowed_tenants": server.allowed_tenants,
            }),
        )
        .await?;
    Ok(Json(server.into()))
}

/// The tenants an MCP server is opened to, each one checked to exist and
/// listed once. A stale id would be a grant waiting for whoever is given it.
async fn mcp_tenants(state: &AdminState, tenants: Vec<Uuid>) -> Result<Vec<Uuid>> {
    let mut allowed: Vec<Uuid> = Vec::new();
    for id in tenants {
        if allowed.contains(&id) {
            continue;
        }
        match state.store.get_tenant(id).await {
            Ok(_) => allowed.push(id),
            Err(obleth_store::StoreError::NotFound) => {
                return Err(AdminError::BadRequest(format!(
                    "allowed_tenants names a tenant that does not exist: {id}"
                )))
            }
            Err(e) => return Err(e.into()),
        }
    }
    Ok(allowed)
}

#[utoipa::path(
    get, path = "/api/v1/mcp-servers", tag = "mcp",
    responses((status = 200, body = [McpServerView]))
)]
pub(crate) async fn list_mcp_servers(
    State(state): State<AdminState>,
) -> Result<Json<Vec<McpServerView>>> {
    Ok(Json(views(state.store.list_mcp_servers().await?)))
}

#[utoipa::path(
    get, path = "/api/v1/mcp-servers/{id}", tag = "mcp",
    params(("id" = Uuid, Path, description = "MCP server id")),
    responses((status = 200, body = McpServerView), (status = 404))
)]
pub(crate) async fn get_mcp_server(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<McpServerView>> {
    Ok(Json(state.store.get_mcp_server(id).await?.into()))
}

#[utoipa::path(
    put, path = "/api/v1/mcp-servers/{id}", tag = "mcp",
    params(("id" = Uuid, Path, description = "MCP server id")),
    request_body = UpdateMcpServer,
    responses((status = 200, body = McpServerView))
)]
pub(crate) async fn update_mcp_server(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateMcpServer>,
) -> Result<Json<McpServerView>> {
    state.ssrf.validate(&body.upstream_url).await?;
    let existing = state.store.get_mcp_server(id).await?;
    let rename = body
        .name
        .as_deref()
        .map(str::trim)
        .filter(|n| *n != existing.name)
        .map(str::to_string);
    if let Some(name) = &rename {
        validate_mcp_server_name(name)?;
    }
    let auth = match body
        .auth_header
        .as_deref()
        .map(str::trim)
        .filter(|a| !a.is_empty())
    {
        Some(new) => Some(new),
        None if body.clear_auth => None,
        None => existing.auth_header.as_deref(),
    };
    let allowed = match body.allowed_tenants.clone() {
        Some(tenants) => mcp_tenants(&state, tenants).await?,
        None => existing.allowed_tenants.clone(),
    };
    let mut server = state
        .store
        .update_mcp_server(
            id,
            &body.upstream_url,
            auth,
            body.enabled.unwrap_or(existing.enabled),
            &allowed,
        )
        .await?;
    let mut regranted: Vec<String> = Vec::new();
    if let Some(name) = &rename {
        let (renamed, models) = state.store.rename_mcp_server(id, name).await?;
        server = renamed;
        // The old name leaves the data-plane cache; the new one is published below.
        state
            .redis
            .delete_resolved_mcp_server(&existing.name)
            .await
            .map_err(|e| eviction_failed("the MCP server's old name", e))?;
        state
            .redis
            .publish_invalidation(&Invalidation::Mcp(existing.name.clone()))
            .await
            .map_err(|e| eviction_failed("the MCP server's old name", e))?;
        for model in &models {
            sync_model(&state, model).await?;
        }
        regranted = models.into_iter().map(|m| m.model_name).collect();
    }
    sync_mcp_server(&state, &server).await?;
    state.audit(&headers, "update_mcp_server", "mcp_server", id, serde_json::json!({
                "name": server.name,
                "renamed_from": rename.as_ref().map(|_| existing.name.clone()),
                "upstream_url": server.upstream_url,
                "enabled": server.enabled,
                "allowed_tenants": server.allowed_tenants,
                "auth_header_set": server.auth_header.is_some(),
                "auth_changed": body.auth_header.as_deref().is_some_and(|a| !a.trim().is_empty()) || (body.clear_auth && existing.auth_header.is_some()),
                "models_regranted": regranted,
            }))
        .await?;
    Ok(Json(server.into()))
}

#[utoipa::path(
    delete, path = "/api/v1/mcp-servers/{id}", tag = "mcp",
    params(("id" = Uuid, Path, description = "MCP server id")),
    responses((status = 204), (status = 404))
)]
pub(crate) async fn delete_mcp_server(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let server = state.store.get_mcp_server(id).await?;
    state.store.delete_mcp_server(id).await?;
    let evicted = async {
        state.redis.delete_resolved_mcp_server(&server.name).await?;
        state
            .redis
            .publish_invalidation(&Invalidation::Mcp(server.name.clone()))
            .await
    }
    .await
    .map_err(|e| eviction_failed("the MCP server", e));
    // Cascade: drop the deleted server from every model's tool grants. A stale
    // grant fails tool discovery on every request, and the dashboard can no
    // longer display or clear it once the server's checkbox is gone. The
    // Postgres cascade runs even when the eviction above failed.
    let stripped = state.store.strip_tool_server_grants(&server.name).await?;
    let mut synced = Ok(());
    for model in &stripped {
        if let Err(e) = sync_model(&state, model).await {
            if synced.is_ok() {
                synced = Err(AdminError::CacheSync(format!(
                    "the MCP server was deleted, but republishing model '{}' without its \
                     grant failed ({e}); reconcile the data-plane cache with {RESYNC_ROUTE}",
                    model.model_name
                )));
            }
        }
    }
    // Audited before a cache failure is reported: the rows are gone.
    state
        .audit(
            &headers,
            "delete_mcp_server",
            "mcp_server",
            id,
            serde_json::json!({
                "name": server.name,
                "models_ungranted": stripped
                    .iter()
                    .map(|m| m.model_name.as_str())
                    .collect::<Vec<_>>(),
                "cache_evicted": evicted.is_ok() && synced.is_ok(),
            }),
        )
        .await?;
    evicted?;
    synced?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mcp_server_names_are_path_safe() {
        for ok in ["github", "rc-docs", "jira.v2", "a_b", "x1"] {
            assert!(
                validate_mcp_server_name(ok).is_ok(),
                "{ok} should be allowed"
            );
        }
        for bad in [
            "",
            "-lead",
            "has space",
            "a/b",
            "../up",
            "ünï",
            &"x".repeat(65),
        ] {
            assert!(
                validate_mcp_server_name(bad).is_err(),
                "{bad:?} should be refused"
            );
        }
    }
}
