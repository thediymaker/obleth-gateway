//! The assistant, compressor, key prefix, client addresses, named
//! networks, usage retention and global capacity settings.

use super::*;

/// Total in-flight ceiling across all model pools, not a fairness budget.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetCapacity {
    pub max_in_flight: usize,
}

/// Total in-flight ceiling across all model pools, not a fairness budget.
#[derive(Debug, Serialize, ToSchema)]
pub struct CapacityView {
    /// The ceiling every replica enforces.
    pub max_in_flight: usize,
    /// `saved` (with `PUT`; it wins over the environment until `DELETE`) or
    /// `environment` (`OBLETH_GLOBAL_MAX_IN_FLIGHT`).
    pub source: CeilingSource,
    /// `OBLETH_GLOBAL_MAX_IN_FLIGHT` on this replica, which applies while
    /// nothing is saved.
    pub environment_max_in_flight: usize,
}

impl CapacityView {
    fn of(ceiling: &replica_settings::GlobalCeiling) -> Self {
        CapacityView {
            max_in_flight: ceiling.max_in_flight(),
            source: ceiling.source(),
            environment_max_in_flight: ceiling.environment(),
        }
    }
}

/// Charo assistant settings surfaced to the dashboard. Mirrors
/// `obleth_config::CharoSettings` exactly.
#[derive(Debug, Serialize, Deserialize, ToSchema)]
pub struct CharoSettingsView {
    pub enabled: bool,
    pub brain_model: Option<String>,
    #[serde(default)]
    pub tools_enabled: std::collections::BTreeMap<String, bool>,
    pub bench_max_concurrency: u32,
    pub bench_max_duration_s: u32,
    pub bench_max_requests: u32,
}

impl From<obleth_config::CharoSettings> for CharoSettingsView {
    fn from(s: obleth_config::CharoSettings) -> Self {
        CharoSettingsView {
            enabled: s.enabled,
            brain_model: s.brain_model,
            tools_enabled: s.tools_enabled,
            bench_max_concurrency: s.bench_max_concurrency,
            bench_max_duration_s: s.bench_max_duration_s,
            bench_max_requests: s.bench_max_requests,
        }
    }
}

impl From<CharoSettingsView> for obleth_config::CharoSettings {
    fn from(v: CharoSettingsView) -> Self {
        obleth_config::CharoSettings {
            enabled: v.enabled,
            brain_model: v.brain_model,
            tools_enabled: v.tools_enabled,
            bench_max_concurrency: v.bench_max_concurrency,
            bench_max_duration_s: v.bench_max_duration_s,
            bench_max_requests: v.bench_max_requests,
        }
    }
}

/// Live status of the optional neural compression sidecar. Its URL is env-only
/// operator config (`OBLETH_COMPRESSOR_URL`), not stored in the DB, so this is a
/// live on-demand probe of the sidecar's `/health` — mirroring how the Slurm tab
/// surfaces provisioner health. Fail-soft: any error yields `reachable=false`
/// with the reason, never an error response.
#[derive(Debug, Serialize, ToSchema)]
pub struct CompressorStatusView {
    /// True when `OBLETH_COMPRESSOR_URL` is set (the feature is wired at all).
    pub configured: bool,
    /// The configured sidecar base URL (empty when unconfigured).
    pub url: String,
    /// True when the sidecar answered `/health` with status `"ok"`.
    pub reachable: bool,
    /// Model name the sidecar reports (e.g. `"kompress-v2-base"`).
    pub model: Option<String>,
    /// Model commit revision the sidecar reports.
    pub revision: Option<String>,
    /// Human-readable reason when the sidecar is configured but not reachable.
    pub error: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/settings/compressor", tag = "settings",
    responses((status = 200, body = CompressorStatusView))
)]
pub(crate) async fn get_compressor_status(
    State(state): State<AdminState>,
) -> Result<Json<CompressorStatusView>> {
    let Some(url) = state.compressor.url.clone() else {
        return Ok(Json(CompressorStatusView {
            configured: false,
            url: String::new(),
            reachable: false,
            model: None,
            revision: None,
            error: None,
        }));
    };

    // Give the settings probe more room than the request path's timeout: a
    // cold sidecar can be slow to answer its first /health.
    let timeout = state
        .compressor
        .timeout
        .max(std::time::Duration::from_secs(2));

    let unreachable = |url: String, error: String| {
        Json(CompressorStatusView {
            configured: true,
            url,
            reachable: false,
            model: None,
            revision: None,
            error: Some(error),
        })
    };

    let client = match reqwest::Client::builder().timeout(timeout).build() {
        Ok(c) => c,
        Err(e) => return Ok(unreachable(url, e.to_string())),
    };

    match client.get(format!("{url}/health")).send().await {
        Ok(resp) if resp.status().is_success() => match resp.json::<serde_json::Value>().await {
            Ok(v) => {
                let ok = v.get("status").and_then(|s| s.as_str()) == Some("ok");
                Ok(Json(CompressorStatusView {
                    configured: true,
                    url,
                    reachable: ok,
                    model: v.get("model").and_then(|s| s.as_str()).map(str::to_string),
                    revision: v
                        .get("revision")
                        .and_then(|s| s.as_str())
                        .map(str::to_string),
                    error: if ok {
                        None
                    } else {
                        Some("sidecar reported a non-ok status".to_string())
                    },
                }))
            }
            Err(e) => Ok(unreachable(url, format!("invalid /health response: {e}"))),
        },
        Ok(resp) => Ok(unreachable(
            url,
            format!("sidecar returned HTTP {}", resp.status()),
        )),
        Err(e) => Ok(unreachable(url, e.to_string())),
    }
}

#[utoipa::path(
    get, path = "/api/v1/settings/charo", tag = "settings",
    responses((status = 200, body = CharoSettingsView))
)]
pub(crate) async fn get_charo_settings(
    State(state): State<AdminState>,
) -> Result<Json<CharoSettingsView>> {
    let settings = state.store.get_charo_settings().await?;
    Ok(Json(settings.into()))
}

#[utoipa::path(
    put, path = "/api/v1/settings/charo", tag = "settings",
    request_body = CharoSettingsView,
    responses((status = 200, body = CharoSettingsView))
)]
pub(crate) async fn put_charo_settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<CharoSettingsView>,
) -> Result<Json<CharoSettingsView>> {
    let settings: obleth_config::CharoSettings = body.into();
    state.store.set_charo_settings(&settings).await?;
    state
        .audit(
            &headers,
            "set_charo_settings",
            "settings",
            "charo",
            serde_json::json!(settings),
        )
        .await?;
    Ok(Json(settings.into()))
}

/// The prefix new API keys start with.
#[derive(Debug, Serialize, ToSchema)]
pub struct KeyPrefixView {
    /// What new keys start with, before the `_`.
    pub prefix: String,
    /// True when this is a saved setting rather than the default.
    pub configured: bool,
    /// The prefix used when none is saved: `OBLETH_KEY_PREFIX`, else `sk`.
    pub default_prefix: String,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateKeyPrefix {
    /// 1 to 3 lowercase letters or digits, the first a letter.
    pub prefix: String,
}

#[utoipa::path(
    get, path = "/api/v1/settings/key-prefix", tag = "settings",
    responses((status = 200, body = KeyPrefixView))
)]
pub(crate) async fn get_key_prefix(State(state): State<AdminState>) -> Result<Json<KeyPrefixView>> {
    let saved = state.store.get_key_prefix_setting().await?;
    let default_prefix = obleth_config::default_key_prefix().to_string();
    Ok(Json(KeyPrefixView {
        configured: saved.is_some(),
        prefix: saved.unwrap_or_else(|| default_prefix.clone()),
        default_prefix,
    }))
}

/// Set the prefix new keys start with. Keys already made keep working: any
/// prefix authenticates, only minting reads this.
#[utoipa::path(
    put, path = "/api/v1/settings/key-prefix", tag = "settings",
    request_body = UpdateKeyPrefix,
    responses((status = 200, body = KeyPrefixView), (status = 400, description = "Not a valid prefix"))
)]
pub(crate) async fn put_key_prefix(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<UpdateKeyPrefix>,
) -> Result<Json<KeyPrefixView>> {
    let prefix = body.prefix.trim();
    if !obleth_config::is_valid_key_prefix(prefix) {
        return Err(AdminError::BadRequest(format!(
            "a key prefix is 1 to {} lowercase letters or digits, starting with a letter",
            obleth_config::KEY_PREFIX_MAX_CHARS
        )));
    }
    let previous = state.store.key_prefix().await?;
    state.store.put_key_prefix_setting(prefix).await?;
    state
        .audit(
            &headers,
            "set_key_prefix",
            "settings",
            "key_prefix",
            serde_json::json!({ "prefix": prefix, "previous": previous }),
        )
        .await?;
    Ok(Json(KeyPrefixView {
        prefix: prefix.to_string(),
        configured: true,
        default_prefix: obleth_config::default_key_prefix().to_string(),
    }))
}

/// How the gateway works out a request's client, from its environment.
/// Read-only: these are set where the gateway is deployed.
#[derive(Debug, Serialize, ToSchema)]
pub struct ClientAddressView {
    /// The proxies whose `X-Forwarded-For` and headers are believed
    /// (`OBLETH_TRUSTED_PROXIES`).
    pub trusted_proxies: Vec<String>,
    /// True when `OBLETH_TRUSTED_PROXIES` is unset, so every private and
    /// loopback range is trusted.
    pub trusted_proxies_default: bool,
    /// True when `X-Forwarded-For` is read from the right past the trusted
    /// proxies in it, which the gateway does only when
    /// `OBLETH_TRUSTED_PROXIES` is set. Otherwise its rightmost entry is the
    /// client.
    pub walks_past_proxies: bool,
    /// Entries of `OBLETH_TRUSTED_PROXIES` that are not ranges, ignored.
    pub invalid_trusted_proxies: Vec<String>,
    /// `OBLETH_CLIENT_ADDRESS_HEADER`, or `null` when the client is found
    /// in `X-Forwarded-For`.
    pub address_header: Option<String>,
    /// `OBLETH_CLIENT_COUNTRY_HEADER`, or `null` when countries are off.
    pub country_header: Option<String>,
    /// True when a country can let a request use a key limited to certain
    /// countries: a country header is set and so is
    /// `OBLETH_TRUSTED_PROXIES`. Otherwise countries are only recorded, and
    /// a key limited to countries alone is refused everywhere.
    pub countries_admit: bool,
}

/// Named networks, and how the client address is found.
#[derive(Debug, Serialize, ToSchema)]
pub struct NetworkSettingsView {
    pub networks: Vec<NamedNetwork>,
    pub client_address: ClientAddressView,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateNamedNetworks {
    /// Every named network; this replaces the list. The older one-range
    /// rows (`{"network", "name"}`) are still accepted, and pieces that
    /// share a name are merged into one network.
    pub networks: Vec<NamedNetwork>,
}

fn network_settings_view(
    networks: Vec<NamedNetwork>,
    env: &obleth_config::network::ClientAddressSettings,
) -> NetworkSettingsView {
    NetworkSettingsView {
        networks,
        client_address: ClientAddressView {
            trusted_proxies: env.trusted.iter().map(|n| n.to_string()).collect(),
            trusted_proxies_default: !env.explicit,
            walks_past_proxies: env.explicit,
            invalid_trusted_proxies: env.invalid.clone(),
            countries_admit: env.explicit && env.country_header.is_some(),
            address_header: env.address_header.clone(),
            country_header: env.country_header.clone(),
        },
    }
}

#[utoipa::path(
    get, path = "/api/v1/settings/networks", tag = "settings",
    responses((status = 200, body = NetworkSettingsView))
)]
pub(crate) async fn get_network_settings(
    State(state): State<AdminState>,
) -> Result<Json<NetworkSettingsView>> {
    Ok(Json(network_settings_view(
        state.store.named_networks().await?,
        &state.client_address,
    )))
}

/// Replace the named networks: groups of address ranges with a name (and
/// whether the network is the organisation's own), so reports can say whose
/// network a client is in. An address takes the name of the narrowest range
/// that holds it. They change no check.
#[utoipa::path(
    put, path = "/api/v1/settings/networks", tag = "settings",
    request_body = UpdateNamedNetworks,
    responses((status = 200, body = NetworkSettingsView), (status = 400, description = "A range or name is not valid"))
)]
pub(crate) async fn put_network_settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<UpdateNamedNetworks>,
) -> Result<Json<NetworkSettingsView>> {
    let networks = obleth_config::network::normalize_named_networks(&body.networks)
        .map_err(AdminError::BadRequest)?;
    let previous = state.store.named_networks().await?;
    state.store.put_named_networks(&networks).await?;
    state
        .audit(
            &headers,
            "set_named_networks",
            "settings",
            "named_networks",
            serde_json::json!({ "networks": networks, "previous": previous }),
        )
        .await?;
    Ok(Json(network_settings_view(networks, &state.client_address)))
}

/// View of the persisted raw-usage retention window.
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageRetentionView {
    /// Days of raw per-request history retained before pruning.
    pub days: i64,
    /// True when this reflects a saved setting rather than the env default.
    pub configured: bool,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateUsageRetention {
    pub days: i64,
}

#[utoipa::path(
    get, path = "/api/v1/settings/usage-retention", tag = "settings",
    responses((status = 200, body = UsageRetentionView))
)]
pub(crate) async fn get_usage_retention(
    State(state): State<AdminState>,
) -> Result<Json<UsageRetentionView>> {
    let saved = state.store.get_usage_retention_settings().await?;
    let view = match saved {
        Some(s) => UsageRetentionView {
            days: s.days,
            configured: true,
        },
        None => UsageRetentionView {
            days: state.usage_retention_default_days,
            configured: false,
        },
    };
    Ok(Json(view))
}

#[utoipa::path(
    put, path = "/api/v1/settings/usage-retention", tag = "settings",
    request_body = UpdateUsageRetention,
    responses((status = 200, body = UsageRetentionView))
)]
pub(crate) async fn put_usage_retention(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<UpdateUsageRetention>,
) -> Result<Json<UsageRetentionView>> {
    if body.days < 1 {
        return Err(AdminError::BadRequest(
            "retention days must be at least 1".into(),
        ));
    }
    if body.days > usage_retention::MAX_RETENTION_DAYS {
        return Err(AdminError::BadRequest(format!(
            "retention days must be at most {}",
            usage_retention::MAX_RETENTION_DAYS
        )));
    }
    let settings = obleth_config::UsageRetentionSettings { days: body.days };
    state.store.put_usage_retention_settings(&settings).await?;
    state
        .audit(
            &headers,
            "set_usage_retention",
            "settings",
            "usage_retention",
            serde_json::json!({ "days": settings.days }),
        )
        .await?;
    Ok(Json(UsageRetentionView {
        days: settings.days,
        configured: true,
    }))
}

/// Result of a manual compaction run.
#[derive(Debug, Serialize, ToSchema)]
pub struct CompactUsageResult {
    pub retention_days: i64,
    pub partitions_dropped: usize,
}

#[utoipa::path(
    post, path = "/api/v1/usage/compact", tag = "usage",
    responses((status = 200, body = CompactUsageResult))
)]
pub(crate) async fn compact_usage(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<CompactUsageResult>> {
    let result = usage_retention::compact_usage_now(&state).await?;
    state
        .audit(
            &headers,
            "compact_usage",
            "usage",
            "usage",
            serde_json::json!({
                "retention_days": result.retention_days,
                "partitions_dropped": result.partitions_dropped,
            }),
        )
        .await?;
    Ok(Json(CompactUsageResult {
        retention_days: result.retention_days,
        partitions_dropped: result.partitions_dropped,
    }))
}

#[utoipa::path(
    get, path = "/api/v1/capacity", tag = "capacity",
    responses((status = 200, body = CapacityView))
)]
pub(crate) async fn get_capacity(State(state): State<AdminState>) -> Json<CapacityView> {
    Json(CapacityView::of(&state.ceiling))
}

#[utoipa::path(
    put, path = "/api/v1/capacity", tag = "capacity",
    request_body = SetCapacity,
    responses((status = 200, body = CapacityView))
)]
pub(crate) async fn set_capacity(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<SetCapacity>,
) -> Result<Json<CapacityView>> {
    if body.max_in_flight == 0 {
        return Err(AdminError::BadRequest(
            "max_in_flight must be at least 1".into(),
        ));
    }
    state.store.put_global_ceiling(body.max_in_flight).await?;
    state.ceiling.apply(Some(body.max_in_flight));
    publish_setting(&state, SavedSetting::Capacity).await;
    state
        .audit(
            &headers,
            "set_capacity",
            "gateway",
            "global",
            serde_json::json!({ "max_in_flight": body.max_in_flight }),
        )
        .await?;
    Ok(Json(CapacityView::of(&state.ceiling)))
}

#[utoipa::path(
    delete, path = "/api/v1/capacity", tag = "capacity",
    responses((status = 200, description = "The saved ceiling is forgotten; \
        OBLETH_GLOBAL_MAX_IN_FLIGHT applies on every replica", body = CapacityView))
)]
pub(crate) async fn clear_capacity(
    State(state): State<AdminState>,
    headers: HeaderMap,
) -> Result<Json<CapacityView>> {
    state.store.delete_global_ceiling().await?;
    state.ceiling.apply(None);
    publish_setting(&state, SavedSetting::Capacity).await;
    state
        .audit(
            &headers,
            "clear_capacity",
            "gateway",
            "global",
            serde_json::json!({ "max_in_flight": state.ceiling.environment() }),
        )
        .await?;
    Ok(Json(CapacityView::of(&state.ceiling)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn network_settings_take_legacy_rows_and_answer_in_groups() {
        use utoipa::OpenApi;
        let body: UpdateNamedNetworks = serde_json::from_value(serde_json::json!({
            "networks": [
                { "network": "198.51.100.0/24", "name": "HPC cluster" },
                { "name": "hpc cluster", "ranges": ["198.51.101.0/24"], "ours": true },
                { "name": "Campus", "ranges": ["203.0.113.0/24"] }
            ]
        }))
        .expect("legacy and group rows parse");
        let networks =
            obleth_config::network::normalize_named_networks(&body.networks).expect("valid");
        let view = serde_json::to_value(network_settings_view(networks, &Default::default()))
            .expect("serialize");
        assert_eq!(
            view["networks"],
            serde_json::json!([
                { "name": "HPC cluster", "ranges": ["198.51.100.0/24", "198.51.101.0/24"], "ours": true },
                { "name": "Campus", "ranges": ["203.0.113.0/24"], "ours": false }
            ])
        );
        assert!(view["client_address"].is_object());
        let doc = serde_json::to_value(ApiDoc::openapi()).expect("serialize the openapi doc");
        let schema = &doc["components"]["schemas"]["NamedNetwork"]["properties"];
        assert!(schema["ranges"].is_object(), "{schema}");
        assert!(schema["ours"].is_object(), "{schema}");
    }
}
