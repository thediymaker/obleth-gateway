//! Pushing the store's state to Redis: resolver sync, key pushes and resync.

use super::*;

/// Rebuild the data plane's resolver cache from Postgres: the gateways'
/// periodic re-warm run now (every key, model and MCP server pushed
/// pipelined, stale entries pruned in one SCAN), then every gateway told to
/// drop what it holds in process.
pub(crate) async fn resync_resolvers(state: &AdminState) -> Result<ResyncReport> {
    let pass = rewarm::push_and_prune(&state.store, &state.redis, rewarm::Reread::Always).await;
    let failed = |o: &rewarm::Outcome| o.as_ref().err().cloned();
    if let Some(why) = failed(&pass.keys)
        .or_else(|| failed(&pass.models))
        .or_else(|| failed(&pass.mcp))
    {
        return Err(AdminError::CacheSync(format!(
            "the resolver cache was not fully rebuilt ({why}); run {RESYNC_ROUTE} again"
        )));
    }
    let (keys, models, mcp) = (
        pass.keys.unwrap_or_default(),
        pass.models.unwrap_or_default(),
        pass.mcp.unwrap_or_default(),
    );
    // Every gateway drops what it holds in process and reads the new rows.
    state.redis.publish_invalidation(&Invalidation::All).await?;
    if let Some(tx) = &state.local_cache_tx {
        for hash in keys.ids.iter().chain(&keys.evicted) {
            let _ = tx.send(hash.clone());
        }
    }
    Ok(ResyncReport {
        keys: keys.ids.len(),
        keys_pruned: keys.evicted.len(),
        models: models.rows,
        model_names_pruned: models.evicted.len(),
        mcp_servers: mcp.ids.len(),
        mcp_servers_pruned: mcp.evicted.len(),
    })
}

/// Outcome of a resolver-cache reconcile.
#[derive(Debug, Serialize, ToSchema)]
pub struct ResyncReport {
    /// Keys republished from Postgres.
    pub keys: usize,
    /// Key entries evicted because no key row backs them.
    pub keys_pruned: usize,
    /// Models republished from Postgres.
    pub models: usize,
    /// Model/alias entries evicted because no enabled model answers to them.
    pub model_names_pruned: usize,
    /// MCP servers republished from Postgres.
    pub mcp_servers: usize,
    /// MCP entries evicted because no enabled server backs them.
    pub mcp_servers_pruned: usize,
}

/// Rebuild the data plane's resolver cache (keys, models, MCP servers) from
/// Postgres. This is the retry for a delete that reported a failed eviction.
#[utoipa::path(
    post, path = "/api/v1/resync", tag = "meta",
    responses((status = 200, body = ResyncReport), (status = 502))
)]
pub(crate) async fn resync_resolver_cache(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<ResyncReport>> {
    let report = resync_resolvers(&state).await?;
    state
        .audit(
            &headers,
            "resync_resolver_cache",
            "gateway",
            "resolver_cache",
            serde_json::to_value(&report).unwrap_or_default(),
        )
        .await?;
    Ok(Json(report))
}

/// Tell the other replicas a replica-wide setting changed, so they re-read
/// it now. Best effort: the save stands, and they re-read it on their next
/// registry tick (15 s) anyway.
pub(crate) async fn publish_setting(state: &AdminState, which: SavedSetting) {
    if let Err(e) = state
        .redis
        .publish_invalidation(&Invalidation::Settings(which))
        .await
    {
        tracing::warn!(error = %e, ?which, "could not tell the other replicas a setting changed; they re-read it within 15 s");
    }
}

/// Route that rebuilds the resolver cache from Postgres; named in every
/// eviction-failure error so the operator has a concrete retry.
pub(crate) const RESYNC_ROUTE: &str = "POST /api/v1/resync";

/// Resolver entries have no TTL, so a missed eviction would keep a deleted key,
/// model or MCP server resolving indefinitely. Surface it as a 502 naming the
/// reconcile route instead of reporting success.
pub(crate) fn eviction_failed(what: &str, e: obleth_redis::RedisError) -> AdminError {
    AdminError::CacheSync(format!(
        "{what} was deleted, but evicting it from the data-plane cache failed ({e}); \
         it may keep resolving until the cache is reconciled with {RESYNC_ROUTE}"
    ))
}

/// Like [`eviction_failed`] for a saved change (disable, alias removal) whose
/// stale resolver entry could not be removed.
fn cache_removal_failed(what: &str, e: obleth_redis::RedisError) -> AdminError {
    AdminError::CacheSync(format!(
        "the change was saved, but removing {what} from the data-plane cache failed ({e}); \
         it may keep resolving until the cache is reconciled with {RESYNC_ROUTE}"
    ))
}

/// Evict deleted keys from Redis and every gateway's in-process cache,
/// attempting all of them before reporting the first failure: the deletes
/// and the invalidations each go out pipelined, and every hash leaves this
/// replica's in-process cache even if the publish failed, so it never keeps
/// serving a deleted key from moka for the TTL backstop.
pub(crate) async fn evict_keys(state: &AdminState, hashes: &[String], what: &str) -> Result<()> {
    let deleted = state
        .redis
        .delete_resolved_bulk(Resolver::Keys, hashes)
        .await;
    let targets: Vec<Invalidation> = hashes
        .iter()
        .map(|h| Invalidation::Key(h.clone()))
        .collect();
    let published = state.redis.publish_invalidations(&targets).await;
    if let Some(tx) = &state.local_cache_tx {
        for hash in hashes {
            let _ = tx.send(hash.clone());
        }
    }
    deleted.and(published).map_err(|e| eviction_failed(what, e))
}

pub(crate) async fn push_key(state: &AdminState, hash: &str, resolved: &ResolvedKey) -> Result<()> {
    state.redis.put_resolved_key(hash, resolved).await?;
    state
        .redis
        .publish_invalidation(&Invalidation::Key(hash.to_string()))
        .await?;
    if let Some(tx) = &state.local_cache_tx {
        let _ = tx.send(hash.to_string());
    }
    Ok(())
}

pub(crate) async fn sync_model(state: &AdminState, model: &ModelRoute) -> Result<()> {
    sync_model_from(state, model, None).await
}

/// The data plane's view of `model`, as `sync_model_from` publishes it under
/// every name the model answers to. Every field the request path reads has to
/// come from the row here: one left at its default is silently wrong on the
/// hot path while the registry, read straight from Postgres, looks right.
pub(crate) fn resolved_model_of(
    model: &ModelRoute,
    endpoints: Vec<obleth_config::ResolvedEndpoint>,
    knowledge_collections: Vec<Uuid>,
) -> ResolvedModel {
    ResolvedModel {
        model_name: model.model_name.clone(),
        aliases: model.aliases.clone(),
        lifecycle: model.lifecycle.clone(),
        reasoning: model.reasoning.clone(),
        listed: model.listed,
        rejected_request_fields: model.rejected_request_fields.clone(),
        session_affinity: model.session_affinity,
        description: model.description.clone(),
        variants: model.variants.clone(),
        upstream_model: model.upstream_model.clone(),
        api_base: model.api_base.clone(),
        api_key: model.api_key.clone(),
        upstream_headers: model.upstream_headers.clone(),
        model_type: model.model_type.clone(),
        quantization: model.quantization.clone(),
        admission_weight: model.admission_weight,
        max_in_flight: model.max_in_flight.and_then(|n| usize::try_from(n).ok()),
        capacity_mode: model.capacity_mode.clone(),
        capacity_source: model.capacity_source.clone(),
        capacity_namespace: model.capacity_namespace.clone(),
        capacity_service: model.capacity_service.clone(),
        per_replica_max_in_flight: model
            .per_replica_max_in_flight
            .and_then(|n| usize::try_from(n).ok())
            .filter(|n| *n > 0),
        capacity_headroom: model.capacity_headroom,
        enabled: model.enabled,
        cache_enabled: model.cache_enabled,
        cache_ttl_secs: model.cache_ttl_secs,
        input_cost_per_token: model.input_cost_per_token,
        output_cost_per_token: model.output_cost_per_token,
        cost_per_image: model.cost_per_image,
        cost_per_audio_second: model.cost_per_audio_second,
        cost_per_character: model.cost_per_character,
        cost_per_video: model.cost_per_video,
        context_window: model.context_window,
        supports_function_calling: model.supports_function_calling,
        supports_system_messages: model.supports_system_messages,
        supports_response_schema: model.supports_response_schema,
        supports_tool_choice: model.supports_tool_choice,
        supports_vision: model.supports_vision,
        // `model.tags` is the raw suffixed storage form (round-tripped as-is
        // through create/update so a declared `tag:level` survives an edit
        // that doesn't touch tags). The hot-path cache needs the bare
        // vocabulary for the router's overlap match, plus the parsed ladder.
        tags: obleth_config::normalize_tags(&model.tags),
        declared_levels: obleth_config::declared_tag_levels(&model.tags),
        boons: model.boons.clone(),
        tool_servers: model.tool_servers.clone(),
        knowledge_collections,
        request_timeout_secs: model.request_timeout_secs,
        max_retries: model.max_retries,
        retry_backoff_ms: model.retry_backoff_ms,
        endpoint_selection_mode: model.endpoint_selection_mode.clone(),
        debug_diagnostics: model.debug_diagnostics,
        energy_slots_per_node: model.energy_slots_per_node,
        route_bias: model.route_bias,
        auto_eligible: model.auto_eligible,
        draft_model: model.draft_model.clone(),
        verify_api_base: model.verify_api_base.clone(),
        verify_upstream_model: model.verify_upstream_model.clone(),
        endpoints,
    }
}

/// Republish a model into the resolver cache, evicting the keys it no longer
/// owns.
///
/// `previous` is the row as it was before this write, and is only needed when
/// aliases may have changed: an alias that was just dropped still has a live
/// `obleth:model:<alias>` key pointing at this model, and nothing else in the
/// system would ever clear it. Passing `None` (create, capacity toggle, any
/// write that cannot touch aliases) publishes without an eviction pass.
pub(crate) async fn sync_model_from(
    state: &AdminState,
    model: &ModelRoute,
    previous: Option<&ModelRoute>,
) -> Result<()> {
    // Every model or endpoint write lands here; this replica's live view sees
    // it on its next poll rather than after the cache ages out.
    state.live_catalog.invalidate().await;
    // Endpoints carry the per-cluster wire targets and health; the data plane
    // prefers them over the legacy single api_base/api_key when present. A
    // lookup that fails fails the write: publishing the model with no
    // endpoints or collections would route it wrongly, with no TTL and no
    // error, until the next re-warm.
    let endpoints = state.store.resolved_endpoints_for(model.id).await?;
    let knowledge_collections = state.store.model_collection_ids(model.id).await?;
    let resolved = resolved_model_of(model, endpoints, knowledge_collections);
    // Aliases and variants the write removed: their keys would otherwise keep
    // resolving to this model forever. Done before the publish so a name moved
    // between the lists (alias to variant, or to canonical) is re-added, not
    // left evicted.
    if let Some(previous) = previous {
        let current: std::collections::HashSet<&str> = model.addressable_names().collect();
        for stale in previous
            .addressable_names()
            .filter(|n| !current.contains(n))
        {
            state
                .redis
                .delete_resolved_model(stale)
                .await
                .map_err(|e| cache_removal_failed("a removed alias or variant", e))?;
            state
                .redis
                .publish_invalidation(&Invalidation::Model(stale.to_string()))
                .await
                .map_err(|e| cache_removal_failed("a removed alias or variant", e))?;
        }
    }
    // Every name the model answers to gets its own resolver key, so the data
    // plane keeps resolving an alias in exactly one lookup — aliases cost
    // nothing on the request path.
    for name in resolved.addressable_names() {
        if model.enabled {
            state.redis.put_resolved_model(name, &resolved).await?;
        } else {
            state
                .redis
                .delete_resolved_model(name)
                .await
                .map_err(|e| cache_removal_failed("the disabled model", e))?;
        }
        state
            .redis
            .publish_invalidation(&Invalidation::Model(name.to_string()))
            .await?;
    }
    Ok(())
}

pub(crate) async fn sync_mcp_server(state: &AdminState, server: &McpServer) -> Result<()> {
    let resolved = ResolvedMcpServer {
        name: server.name.clone(),
        upstream_url: server.upstream_url.clone(),
        auth_header: server.auth_header.clone(),
        enabled: server.enabled,
        allowed_tenants: server.allowed_tenants.clone(),
    };
    if server.enabled {
        state
            .redis
            .put_resolved_mcp_server(&server.name, &resolved)
            .await?;
    } else {
        state
            .redis
            .delete_resolved_mcp_server(&server.name)
            .await
            .map_err(|e| cache_removal_failed("the disabled MCP server", e))?;
    }
    state
        .redis
        .publish_invalidation(&Invalidation::Mcp(server.name.clone()))
        .await?;
    Ok(())
}

/// Re-push every key of a tenant after a weight/quota change.
pub(crate) async fn sync_tenant_keys(state: &AdminState, tenant_id: Uuid) -> Result<()> {
    let keys = state.store.resolved_keys_for_tenant(tenant_id).await?;
    push_keys_bulk(state, &keys).await
}

/// Re-push the keys of every tenant in a fairshare group after its weight
/// changed. Keys outside the group carry a different `group_weight` and are
/// untouched; no SCAN or prune runs here (that is `POST /api/v1/resync`).
pub(crate) async fn sync_group_keys(state: &AdminState, group: &str) -> Result<usize> {
    let keys = state.store.resolved_keys_for_group(group).await?;
    push_keys_bulk(state, &keys).await?;
    Ok(keys.len())
}

/// Write many resolved keys to Redis, pipelined, then invalidate the
/// gateways' in-process copies. Every SET completes before any invalidation
/// is published, so a gateway that refetches on the message reads the new row.
async fn push_keys_bulk(state: &AdminState, keys: &[(String, ResolvedKey)]) -> Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    state.redis.put_resolved_bulk(Resolver::Keys, keys).await?;
    // One message per pushed hash, so gateways keep every unrelated key,
    // model and MCP entry.
    let targets: Vec<Invalidation> = keys
        .iter()
        .map(|(hash, _)| Invalidation::Key(hash.clone()))
        .collect();
    state.redis.publish_invalidations(&targets).await?;
    if let Some(tx) = &state.local_cache_tx {
        for (hash, _) in keys {
            let _ = tx.send(hash.clone());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_sync_errors_are_single_line_and_name_the_resync_route() {
        let redis_err =
            || obleth_redis::RedisError::from(serde_json::from_str::<u8>("x").unwrap_err());
        for err in [
            eviction_failed("the key", redis_err()),
            cache_removal_failed("the disabled model", redis_err()),
        ] {
            let AdminError::CacheSync(msg) = err else {
                panic!("expected CacheSync");
            };
            assert!(!msg.contains('\n'), "{msg:?}");
            assert!(!msg.contains("  "), "{msg:?}");
            assert!(msg.contains(RESYNC_ROUTE), "{msg:?}");
        }
    }

    #[test]
    fn failed_eviction_is_a_502_naming_the_resync_route() {
        // Any RedisError will do; a serde one is constructible without a server.
        let cause = serde_json::from_str::<i32>("x").unwrap_err();
        let err = eviction_failed("the key", obleth_redis::RedisError::Serde(cause));
        assert!(err.to_string().contains(RESYNC_ROUTE), "{err}");
        let resp = axum::response::IntoResponse::into_response(err);
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}
