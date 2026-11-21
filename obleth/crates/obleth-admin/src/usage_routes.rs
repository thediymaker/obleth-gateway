//! Usage, cost, log and daily-stats handlers.

use super::*;

#[utoipa::path(
    get, path = "/api/v1/usage", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = [UsageAgg]))
)]
pub(crate) async fn get_usage(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<Vec<UsageAgg>>> {
    Ok(Json(usage::query_usage(&state.clickhouse, q).await?))
}

#[utoipa::path(
    get, path = "/api/v1/usage/keys", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = [UsageKeyAgg]))
)]
pub(crate) async fn get_usage_keys(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<Vec<UsageKeyAgg>>> {
    Ok(Json(usage::query_usage_by_key(&state.clickhouse, q).await?))
}

/// Activity summary for a single API key: last-used timestamp, last model and
/// status, and rolling request/token/cost totals. 404s when the key id is
/// unknown; a known key with no traffic yet returns a zeroed summary
/// (`last_used_ms = 0`) rather than 404.
#[utoipa::path(
    get, path = "/api/v1/keys/{id}/usage", tag = "keys",
    params(
        ("id" = Uuid, Path, description = "API key id"),
        usage::KeyUsageSummaryQuery
    ),
    responses((status = 200, body = KeyUsageSummary), (status = 404))
)]
pub(crate) async fn get_key_usage(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Query(q): Query<usage::KeyUsageSummaryQuery>,
) -> Result<Json<KeyUsageSummary>> {
    // Validate the key exists (and recover its tenant for the never-used case)
    // before touching ClickHouse, so unknown ids are a clean 404.
    let key = state
        .store
        .keys_by_ids(&[id])
        .await?
        .into_iter()
        .next()
        .ok_or(AdminError::NotFound)?;

    let summary = usage::query_key_usage_summary(
        &state.clickhouse,
        key.tenant_id,
        id,
        q.since_ms,
        q.include_internal,
    )
    .await?
    .unwrap_or(KeyUsageSummary {
        key_id: id,
        tenant_id: key.tenant_id,
        last_used_ms: 0,
        last_model: String::new(),
        last_status_code: 0,
        requests: 0,
        input_tokens: 0,
        output_tokens: 0,
        total_tokens: 0,
        cost_usd: 0.0,
        energy_wh: 0.0,
        energy_cost_usd: 0.0,
        co2_g: 0.0,
    });
    Ok(Json(summary))
}

/// Where each key was used from in the window: one row per key, network
/// (IPv4 /24, IPv6 /48) and country, with request and address counts, when
/// the key was first seen from that country (ever) and that network (as far
/// back as client addresses are kept), and the network's name when one of
/// the named networks holds it. For an account system that
/// watches its keys for use from somewhere new. Default window: the last 24
/// hours; at most 31 days. Requests recorded before client addresses were
/// captured are left out. At most 10,000 rows; when rows were left out the
/// response carries `x-obleth-truncated: true`. The read gives up after 10
/// seconds.
#[utoipa::path(
    get, path = "/api/v1/usage/key-sources", tag = "usage",
    params(usage::KeyWatchQuery),
    responses(
        (status = 200, body = [KeySource], headers(
            ("x-obleth-truncated" = String, description = "`true` when more rows matched than the 10,000-row cap let through")
        )),
        (status = 400)
    )
)]
pub(crate) async fn get_usage_key_sources(
    State(state): State<AdminState>,
    Query(q): Query<usage::KeyWatchQuery>,
) -> Result<(HeaderMap, Json<Vec<KeySource>>)> {
    let (since, until) = q
        .window(chrono::Utc::now().timestamp_millis())
        .map_err(AdminError::BadRequest)?;
    let usage::KeySources {
        mut rows,
        truncated,
    } = usage::query_key_sources(&state.clickhouse, &q, since, until).await?;
    let names = NetworkNames::new(&state.store.named_networks().await?);
    if !names.is_empty() {
        for r in &mut rows {
            r.network_name = names.name_of(&r.network).map(str::to_string);
        }
    }
    Ok((truncated_header(truncated), Json(rows)))
}

/// `x-obleth-truncated: true` when a capped read left rows out.
pub(crate) fn truncated_header(truncated: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    if truncated {
        headers.insert(
            "x-obleth-truncated",
            axum::http::HeaderValue::from_static("true"),
        );
    }
    headers
}

/// Refused uses of real keys in the window: a disabled key, or a key used
/// from outside its allowed networks. One row per key, reason, client
/// address and country, with the number of refusals, first and last seen,
/// and the most recent User-Agent, newest first. Default window: the last 24
/// hours; at most 31 days. `key_id` takes one key or up to 200 joined by
/// commas, so one call covers every key a person holds (`key_id=` with no
/// key is refused); `limit` (default and most 10,000) caps the rows and
/// `per_key` caps any one key's rows. When rows were left out the response
/// carries `x-obleth-truncated: true`. The read gives up after 10 seconds.
#[utoipa::path(
    get, path = "/api/v1/usage/key-rejections", tag = "usage",
    params(usage::KeyWatchQuery),
    responses(
        (status = 200, body = [KeyRejectionAgg], headers(
            ("x-obleth-truncated" = String, description = "`true` when more rows matched than `limit` or the 10,000-row cap let through")
        )),
        (status = 400)
    )
)]
pub(crate) async fn get_usage_key_rejections(
    State(state): State<AdminState>,
    Query(q): Query<usage::KeyWatchQuery>,
) -> Result<(HeaderMap, Json<Vec<KeyRejectionAgg>>)> {
    let (since, until) = q
        .window(chrono::Utc::now().timestamp_millis())
        .map_err(AdminError::BadRequest)?;
    let usage::KeyRejections {
        mut rows,
        truncated,
    } = usage::query_key_rejections(&state.clickhouse, &q, since, until).await?;
    let headers = truncated_header(truncated);
    let names = NetworkNames::new(&state.store.named_networks().await?);
    if !names.is_empty() {
        for r in &mut rows {
            r.network_name = names.name_of(&r.client_ip).map(str::to_string);
        }
    }
    Ok((headers, Json(rows)))
}

/// Bulk per-key activity summary for the dashboard Keys table. Returns the
/// busiest keys (by token volume) that saw traffic in the window, each with
/// last-used metadata so the UI can show "last used" without N+1 log fetches.
#[utoipa::path(
    get, path = "/api/v1/usage/keys/summary", tag = "usage",
    params(usage::KeyUsageSummaryQuery),
    responses((status = 200, body = [KeyUsageSummary]))
)]
pub(crate) async fn get_usage_keys_summary(
    State(state): State<AdminState>,
    Query(q): Query<usage::KeyUsageSummaryQuery>,
) -> Result<Json<Vec<KeyUsageSummary>>> {
    Ok(Json(
        usage::query_keys_usage_summary(&state.clickhouse, q).await?,
    ))
}

#[utoipa::path(
    get, path = "/api/v1/usage/models", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = [UsageModelAgg]))
)]
pub(crate) async fn get_usage_models(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<Vec<UsageModelAgg>>> {
    Ok(Json(
        usage::query_usage_by_model(&state.clickhouse, q).await?,
    ))
}

#[utoipa::path(
    get, path = "/api/v1/usage/series", tag = "usage",
    params(usage::UsageSeriesQuery),
    responses((status = 200, body = [UsageTimePoint]))
)]
pub(crate) async fn get_usage_series(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageSeriesQuery>,
) -> Result<Json<Vec<UsageTimePoint>>> {
    Ok(Json(usage::query_usage_series(&state.clickhouse, q).await?))
}

#[utoipa::path(
    get, path = "/api/v1/usage/series/tenants", tag = "usage",
    params(usage::UsageSeriesQuery),
    responses((status = 200, body = [TenantUsageTimePoint]))
)]
pub(crate) async fn get_usage_series_tenants(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageSeriesQuery>,
) -> Result<Json<Vec<TenantUsageTimePoint>>> {
    Ok(Json(
        usage::query_usage_series_by_tenant(&state.clickhouse, q).await?,
    ))
}

#[utoipa::path(
    get, path = "/api/v1/usage/series/models", tag = "usage",
    params(usage::UsageSeriesQuery),
    responses((status = 200, body = [ModelUsageTimePoint]))
)]
pub(crate) async fn get_usage_series_models(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageSeriesQuery>,
) -> Result<Json<Vec<ModelUsageTimePoint>>> {
    Ok(Json(
        usage::query_usage_series_by_model(&state.clickhouse, q).await?,
    ))
}

#[utoipa::path(
    get, path = "/api/v1/usage/speed", tag = "usage",
    params(usage::UsageSpeedQuery),
    responses((status = 200, body = [ModelSpeedPoint]))
)]
pub(crate) async fn get_usage_speed(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageSpeedQuery>,
) -> Result<Json<Vec<ModelSpeedPoint>>> {
    if let Some(tz) = &q.tz {
        tz.parse::<chrono_tz::Tz>()
            .map_err(|_| AdminError::BadRequest(format!("unknown time zone: {tz}")))?;
    }
    Ok(Json(usage::query_usage_speed(&state.clickhouse, q).await?))
}

#[utoipa::path(
    get, path = "/api/v1/usage/cache", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = CacheStats))
)]
pub(crate) async fn get_cache_stats(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<CacheStats>> {
    Ok(Json(usage::query_cache_stats(&state.clickhouse, q).await?))
}

#[utoipa::path(
    get, path = "/api/v1/costs", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = [CostAgg]))
)]
pub(crate) async fn get_costs(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<Vec<CostAgg>>> {
    let models = state.store.list_models().await?;
    let costs: Vec<(String, f64, f64)> = models
        .iter()
        .map(|m| {
            (
                m.model_name.clone(),
                m.input_cost_per_token,
                m.output_cost_per_token,
            )
        })
        .collect();
    Ok(Json(
        usage::query_costs(&state.clickhouse, &q, &costs).await?,
    ))
}

/// A request-log row enriched with the human-readable tenant and key names
/// resolved from Postgres, so the live log view does not have to display bare
/// UUIDs.
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageLogEntry {
    #[serde(flatten)]
    pub row: UsageLogRow,
    pub tenant_name: String,
    pub key_name: String,
    pub key_prefix: String,
    /// `true` when at least one span for this request exists in ClickHouse.
    #[serde(default)]
    pub has_trace: bool,
    /// The narrowest named network (Settings › Networks) that holds the
    /// client address, when one does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_name: Option<String>,
}

/// Newest-first feed of individual requests for the live log view. ClickHouse
/// stores only UUIDs, so the page's tenant/key ids are resolved to names in a
/// pair of bounded Postgres lookups (tenants are few; keys are fetched by the
/// exact ids on the page rather than the full fleet). When rows past the page
/// match, the response carries `x-obleth-truncated: true`; the next page
/// starts at the last row's `before_ms` and `before_request_id`.
#[utoipa::path(
    get, path = "/api/v1/usage/logs", tag = "usage",
    params(usage::UsageLogQuery),
    responses((status = 200, body = [UsageLogEntry],
        headers(("x-obleth-truncated" = String, description = "`true` when rows past this page match"))))
)]
pub(crate) async fn get_usage_logs(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageLogQuery>,
) -> Result<(HeaderMap, Json<Vec<UsageLogEntry>>)> {
    let usage::LogPage { rows, more } = usage::query_usage_logs(&state.clickhouse, q).await?;
    let known = names::resolve_names(
        &state,
        rows.iter().map(|r| r.tenant_id),
        rows.iter().map(|r| r.key_id),
    )
    .await?;

    let request_ids: Vec<Uuid> = rows.iter().map(|r| r.request_id).collect();
    let traced = usage::batch_has_trace(&state.clickhouse, &request_ids).await;
    let names = NetworkNames::new(&state.store.named_networks().await?);

    let entries = rows
        .into_iter()
        .map(|row| {
            // The live name when the key or tenant still exists, else the one
            // recorded on the row, so a deleted key's requests keep their name.
            let tenant_name = known
                .tenant_name(&row.tenant_id)
                .unwrap_or(&row.recorded_tenant_name)
                .to_string();
            let (key_name, key_prefix) = match known.keys.get(&row.key_id) {
                Some(k) => (k.name.clone(), k.key_prefix.clone()),
                None => (row.recorded_key_name.clone(), String::new()),
            };
            let has_trace = traced.contains(&row.request_id);
            let network_name = (!row.client_ip.is_empty())
                .then(|| names.name_of(&row.client_ip).map(str::to_string))
                .flatten();
            UsageLogEntry {
                row,
                tenant_name,
                key_name,
                key_prefix,
                has_trace,
                network_name,
            }
        })
        .collect();

    Ok((truncated_header(more), Json(entries)))
}

/// What the requests matching the log's filters are made of: the busiest
/// status codes, models, tenants, keys and request types, each with its
/// failures, and the status code and model pairs that fail most. Tenant and
/// key ids come back with their names.
#[utoipa::path(
    get, path = "/api/v1/usage/logs/facets", tag = "usage",
    params(usage::UsageLogQuery),
    responses((status = 200, body = UsageLogFacets))
)]
pub(crate) async fn get_usage_log_facets(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageLogQuery>,
) -> Result<Json<UsageLogFacets>> {
    let mut facets = usage::query_usage_log_facets(&state.clickhouse, q).await?;
    let ids = |values: &[usage::UsageLogFacet]| -> Vec<Uuid> {
        values
            .iter()
            .filter_map(|v| Uuid::parse_str(&v.value).ok())
            .collect()
    };
    let known = names::resolve_names(&state, ids(&facets.tenants), ids(&facets.keys)).await?;
    for t in &mut facets.tenants {
        t.label = Uuid::parse_str(&t.value)
            .ok()
            .and_then(|id| known.tenant_name(&id))
            .unwrap_or_default()
            .to_string();
    }
    for k in &mut facets.keys {
        k.label = Uuid::parse_str(&k.value)
            .ok()
            .and_then(|id| known.keys.get(&id))
            .map(|k| {
                if k.name.is_empty() {
                    k.key_prefix.clone()
                } else {
                    format!("{} · {}", k.name, k.key_prefix)
                }
            })
            .unwrap_or_default();
    }
    Ok(Json(facets))
}

/// The failures among the requests matching the log's filters: how many
/// were the platform's and how many the callers', and the same failure
/// repeated (one key, model, status, class and reason) as one group with a
/// count. Tenant and key names are the live ones where the tenant or key
/// still exists.
#[utoipa::path(
    get, path = "/api/v1/usage/failures", tag = "usage",
    params(usage::UsageLogQuery),
    responses((status = 200, body = UsageFailureGroups))
)]
pub(crate) async fn get_usage_failure_groups(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageLogQuery>,
) -> Result<Json<UsageFailureGroups>> {
    let mut failures = usage::query_usage_failure_groups(&state.clickhouse, q).await?;
    let known = names::resolve_names(
        &state,
        failures.groups.iter().map(|g| g.tenant_id),
        failures.groups.iter().map(|g| g.key_id),
    )
    .await?;
    for g in &mut failures.groups {
        if let Some(name) = known.tenant_name(&g.tenant_id) {
            g.tenant_name = name.to_string();
        }
        if let Some(name) = known.key_name(&g.key_id).filter(|n| !n.is_empty()) {
            g.key_name = name.to_string();
        }
    }
    Ok(Json(failures))
}

/// Requests and failures over time for the request log's window, counted with
/// the log's own filters so the chart and the list always agree. Only
/// non-empty buckets come back.
#[utoipa::path(
    get, path = "/api/v1/usage/logs/histogram", tag = "usage",
    params(usage::UsageLogQuery),
    responses((status = 200, body = UsageLogHistogram))
)]
pub(crate) async fn get_usage_log_histogram(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageLogQuery>,
) -> Result<Json<UsageLogHistogram>> {
    let (bucket_ms, buckets) = usage::query_usage_log_histogram(&state.clickhouse, q).await?;
    Ok(Json(UsageLogHistogram { bucket_ms, buckets }))
}

#[utoipa::path(
    get, path = "/api/v1/usage/logs/{request_id}/spans", tag = "usage",
    params(("request_id" = Uuid, Path, description = "Request id")),
    responses((status = 200, body = [SpanEntry]))
)]
pub(crate) async fn get_request_spans(
    State(state): State<AdminState>,
    Path(request_id): Path<Uuid>,
) -> Result<Json<Vec<SpanEntry>>> {
    Ok(Json(
        usage::query_request_spans(&state.clickhouse, request_id).await?,
    ))
}

/// A per-model breakdown row enriched with human-readable tenant/key names and
/// the tenant's fairshare group, resolved from Postgres so the model card's
/// breakdown table does not have to display bare UUIDs.
#[derive(Debug, Serialize, ToSchema)]
pub struct UsageBreakdownEntry {
    #[serde(flatten)]
    pub row: UsageKeyModelBreakdown,
    pub tenant_name: String,
    pub fairshare_group: String,
    pub key_name: String,
    pub key_prefix: String,
}

/// Per tenant/key breakdown of one model's traffic over the window, powering
/// the breakdown table in the expanded model card. UUIDs are resolved to
/// tenant/key names in two bounded Postgres lookups, mirroring `/usage/logs`.
#[utoipa::path(
    get, path = "/api/v1/usage/breakdown", tag = "usage",
    params(usage::UsageBreakdownQuery),
    responses((status = 200, body = [UsageBreakdownEntry]))
)]
pub(crate) async fn get_usage_breakdown(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageBreakdownQuery>,
) -> Result<Json<Vec<UsageBreakdownEntry>>> {
    let rows = usage::query_usage_breakdown_by_model(&state.clickhouse, &q).await?;
    let known = names::resolve_names(
        &state,
        rows.iter().map(|r| r.tenant_id),
        rows.iter().map(|r| r.key_id),
    )
    .await?;

    let entries = rows
        .into_iter()
        .map(|row| {
            let (tenant_name, fairshare_group) = known
                .tenants
                .get(&row.tenant_id)
                .map(|t| (t.name.clone(), t.fairshare_group.clone()))
                .unwrap_or_default();
            let (key_name, key_prefix) = known
                .keys
                .get(&row.key_id)
                .map(|k| (k.name.clone(), k.key_prefix.clone()))
                .unwrap_or_default();
            UsageBreakdownEntry {
                row,
                tenant_name,
                fairshare_group,
                key_name,
                key_prefix,
            }
        })
        .collect();

    Ok(Json(entries))
}

#[utoipa::path(
    get, path = "/api/v1/usage/daily", tag = "usage",
    params(usage::UsageDailyQuery),
    responses((status = 200, body = [UsageDailyRow]))
)]
pub(crate) async fn get_usage_daily(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageDailyQuery>,
) -> Result<Json<Vec<UsageDailyRow>>> {
    Ok(Json(usage::query_usage_daily(&state.clickhouse, q).await?))
}

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct DailyStatsQuery {
    /// `mcp` (per server: `direct`, `tool_calls`, `errors`, `ms`) or
    /// `knowledge` (per collection: `searches`, `hits`, `chunks`).
    pub kind: String,
    /// Days back from today, UTC; 1 to 40, default 7.
    pub days: Option<u32>,
}

/// One thing's counters on one day.
#[derive(Debug, Serialize, ToSchema)]
pub struct DailyStat {
    pub day: String,
    pub counts: std::collections::HashMap<String, i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DailyStatsItem {
    /// The MCP server's name, or the collection's id.
    pub id: String,
    pub days: Vec<DailyStat>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DailyStatsView {
    pub kind: String,
    /// The days covered, oldest first.
    pub days: Vec<String>,
    pub items: Vec<DailyStatsItem>,
}

/// Daily usage counters the proxy keeps in Redis: MCP calls per server and
/// knowledge searches per collection. Best-effort and about 40 days deep: the
/// counters carry a TTL, so Redis can evict past days under memory pressure
/// and the chart then shows gaps.
#[utoipa::path(
    get, path = "/api/v1/stats/daily", tag = "usage",
    params(DailyStatsQuery),
    responses((status = 200, body = DailyStatsView))
)]
pub(crate) async fn get_daily_stats(
    State(state): State<AdminState>,
    Query(q): Query<DailyStatsQuery>,
) -> Result<Json<DailyStatsView>> {
    let ids: Vec<String> = match q.kind.as_str() {
        "mcp" => state
            .store
            .list_mcp_servers()
            .await?
            .into_iter()
            .map(|s| s.name)
            .collect(),
        "knowledge" => state
            .store
            .list_collections()
            .await?
            .into_iter()
            .map(|c| c.id.to_string())
            .collect(),
        _ => {
            return Err(AdminError::BadRequest(
                "kind must be mcp or knowledge".into(),
            ))
        }
    };
    let n = q.days.unwrap_or(7).clamp(1, 40);
    let today = chrono::Utc::now().date_naive();
    let days: Vec<String> = (0..n)
        .rev()
        .map(|back| {
            (today - chrono::Duration::days(i64::from(back)))
                .format("%Y-%m-%d")
                .to_string()
        })
        .collect();
    let read = state
        .redis
        .read_daily(&q.kind, &ids, &days)
        .await
        .map_err(|e| AdminError::Internal(format!("daily stats unavailable: {e}")))?;
    let items = ids
        .into_iter()
        .map(|id| DailyStatsItem {
            days: read
                .get(&id)
                .map(|rows| {
                    rows.iter()
                        .map(|(day, counts)| DailyStat {
                            day: day.clone(),
                            counts: counts.clone(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            id,
        })
        .collect();
    Ok(Json(DailyStatsView {
        kind: q.kind,
        days,
        items,
    }))
}
