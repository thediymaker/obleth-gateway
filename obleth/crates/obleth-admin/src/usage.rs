//! Usage/cost reads from the ClickHouse ledger.

use clickhouse::Row;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct UsageQuery {
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    #[schema(value_type = Option<String>)]
    pub key_id: Option<Uuid>,
    pub model: Option<String>,
    /// Lower bound, unix epoch millis. Defaults to the last 24h.
    pub since_ms: Option<i64>,
    /// Aggregate dimension: `tenant` (default), `key`, or `model`.
    pub group_by: Option<String>,
    /// Cap the number of returned rows (highest-volume first). Critical for
    /// per-key reads where a tenant fleet can hold 100k+ keys.
    pub limit: Option<u64>,
}

#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct UsageSeriesQuery {
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    /// Restrict the series to a single model. Required by the per-model series
    /// endpoint; ignored by the global/tenant series readers.
    pub model: Option<String>,
    pub since_ms: Option<i64>,
    /// Bucket width in milliseconds. Default 300_000 (5 minutes).
    pub bucket_ms: Option<i64>,
}

/// Filters for the per-model tenant/key breakdown
/// (`GET /api/v1/usage/breakdown`). Scoped to a single model so the expanded
/// model card can show which tenants/keys are driving its load.
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct UsageBreakdownQuery {
    /// Model to break down (required).
    pub model: String,
    /// Lower bound, unix epoch millis. Defaults to the last 24h.
    pub since_ms: Option<i64>,
    /// Cap on rows returned (busiest tenant/key pairs first).
    pub limit: Option<u64>,
}

/// Date-range read against the permanent daily rollup (`usage_daily`).
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct UsageDailyQuery {
    /// Inclusive lower bound, `YYYY-MM-DD`. Defaults to 7 days ago.
    pub start_day: Option<String>,
    /// Inclusive upper bound, `YYYY-MM-DD`. Defaults to today.
    pub end_day: Option<String>,
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    /// One or more key ids. Accepts a single UUID or a comma-separated list
    /// (`key_id=a,b,c`) so a caller can fetch spend across all of a user's
    /// rotated keys in one request.
    pub key_id: Option<String>,
    pub model: Option<String>,
    /// Aggregate dimension: `day` (default), `tenant`, `key`, `model`,
    /// or `key_model` (one row per key+model across the whole range).
    pub group_by: Option<String>,
}

/// Parse the `key_id` query value, which may be a single UUID or a
/// comma-separated list (`a,b,c`). Empty entries are ignored so a trailing
/// comma is harmless. Returns the first parse error so the handler can map it
/// to a 400.
pub fn parse_key_ids(raw: Option<&str>) -> Result<Vec<Uuid>, uuid::Error> {
    match raw {
        Some(s) => s
            .split(',')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(Uuid::parse_str)
            .collect(),
        None => Ok(Vec::new()),
    }
}

/// Filters for the raw per-request log feed (`GET /api/v1/usage/logs`). This is
/// the only read that returns individual `usage` rows rather than an aggregate,
/// and it powers the live request-log view in the control plane.
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct UsageLogQuery {
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    #[schema(value_type = Option<String>)]
    pub key_id: Option<Uuid>,
    pub model: Option<String>,
    /// Coarse request class (`chat`, `embedding`, `audio`, ...).
    pub request_type: Option<String>,
    pub session_id: Option<String>,
    /// Status filter: `success` (2xx/3xx), `error` (>=400), or all (default).
    pub status: Option<String>,
    /// Case-insensitive prefix match on the request id, for the search box.
    pub request_id: Option<String>,
    /// Inclusive lower bound, unix epoch millis. Defaults to the last 24h.
    pub since_ms: Option<i64>,
    /// Inclusive upper bound, unix epoch millis. Open-ended by default.
    pub until_ms: Option<i64>,
    /// Keyset cursor for "older" pages: return rows strictly before this
    /// timestamp. Paired with `before_request_id` to break ties at the same ms.
    pub before_ms: Option<i64>,
    #[schema(value_type = Option<String>)]
    pub before_request_id: Option<Uuid>,
    /// Page size (highest `ts_ms` first). Clamped to a sane ceiling.
    pub limit: Option<u64>,
    /// When `true`, return only requests that have at least one span in
    /// ClickHouse (i.e. were traced).
    pub traced_only: Option<bool>,
    /// When true, include internal traffic (e.g. health probes) that is hidden
    /// from the request log by default.
    pub include_internal: Option<bool>,
}

/// A single request as stored in the `usage` ledger, returned newest-first for
/// the live log view. UUIDs are resolved to tenant/key names by the handler.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageLogRow {
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub request_id: Uuid,
    /// Unix epoch milliseconds at request completion.
    pub ts_ms: i64,
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub key_id: Uuid,
    pub model: String,
    pub request_type: String,
    pub session_id: String,
    pub session_id_source: String,
    pub admission: String,
    pub status_code: u16,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u64,
    pub queue_wait_ms: u32,
    pub ttft_ms: u32,
    pub total_ms: u32,
    pub cache_status: String,
    pub cost_usd: f64,
}

/// Read individual `usage` rows newest-first, honoring the supplied filters and
/// keyset cursor. Bind order below must track the `?` placeholders exactly,
/// since the ClickHouse client binds positionally.
pub async fn query_usage_logs(
    client: &clickhouse::Client,
    q: UsageLogQuery,
) -> Result<Vec<UsageLogRow>, clickhouse::error::Error> {
    let since = q.since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);

    let mut sql = String::from(
        "select request_id, ts_ms, tenant_id, key_id, model, request_type, session_id, session_id_source, \
         admission, status_code, input_tokens, output_tokens, \
         toUInt64(input_tokens) + toUInt64(output_tokens) as total_tokens, \
         queue_wait_ms, ttft_ms, total_ms, cache_status, cost_usd \
         from usage where ts_ms >= ?",
    );
    if q.until_ms.is_some() {
        sql.push_str(" and ts_ms <= ?");
    }
    if q.tenant_id.is_some() {
        sql.push_str(" and tenant_id = toUUID(?)");
    }
    if q.key_id.is_some() {
        sql.push_str(" and key_id = toUUID(?)");
    }
    if q.model.is_some() {
        sql.push_str(" and model = ?");
    }
    if q.request_type.is_some() {
        sql.push_str(" and request_type = ?");
    }
    if q.session_id.is_some() {
        sql.push_str(" and session_id = ?");
    }
    if q.include_internal != Some(true) {
        // `health_probe` is a literal constant, no bind needed / no injection surface.
        sql.push_str(&format!(
            " and request_type != '{}'",
            obleth_admin_health_probe_label()
        ));
    }
    match q.status.as_deref() {
        Some("success") => sql.push_str(" and status_code >= 200 and status_code < 400"),
        Some("error") => sql.push_str(" and status_code >= 400"),
        _ => {}
    }
    if q.request_id.is_some() {
        sql.push_str(" and startsWith(lower(toString(request_id)), lower(?))");
    }
    if q.traced_only == Some(true) {
        let since = q.since_ms.unwrap_or(0);
        let until = q.until_ms.unwrap_or(i64::MAX);
        // Safety: `since` and `until` are `i64` — `Display` emits only ASCII
        // digits (and an optional leading `-`), so there is no SQL-injection
        // surface. The clickhouse crate (v0.13) scopes bind parameters to the
        // top-level query string and does not propagate them into subqueries,
        // making `format!` the correct approach for subquery literals.
        sql.push_str(&format!(
            " AND request_id IN (SELECT DISTINCT request_id FROM spans \
              WHERE start_ms >= {since} AND start_ms <= {until})"
        ));
    }
    // Keyset cursor: (ts_ms, request_id) tuple strictly less than the cursor.
    // Tuple comparison matches the `order by` below for stable paging.
    if q.before_ms.is_some() && q.before_request_id.is_some() {
        sql.push_str(" and (ts_ms, toString(request_id)) < (?, ?)");
    } else if q.before_ms.is_some() {
        sql.push_str(" and ts_ms < ?");
    }
    // Order by the same expression the keyset cursor compares against
    // (`toString(request_id)`), so paging is stable across millisecond ties:
    // ClickHouse's native UUID ordering differs from its string ordering.
    sql.push_str(&format!(
        " order by ts_ms desc, toString(request_id) desc limit {limit}"
    ));

    let mut query = client.query(&sql).bind(since);
    if let Some(until) = q.until_ms {
        query = query.bind(until);
    }
    if let Some(tid) = q.tenant_id {
        query = query.bind(tid.to_string());
    }
    if let Some(kid) = q.key_id {
        query = query.bind(kid.to_string());
    }
    if let Some(model) = &q.model {
        query = query.bind(model.clone());
    }
    if let Some(rt) = &q.request_type {
        query = query.bind(rt.clone());
    }
    if let Some(sid) = &q.session_id {
        query = query.bind(sid.clone());
    }
    if let Some(rid) = &q.request_id {
        query = query.bind(rid.clone());
    }
    if let (Some(before_ms), Some(before_id)) = (q.before_ms, q.before_request_id) {
        query = query.bind(before_ms).bind(before_id.to_string());
    } else if let Some(before_ms) = q.before_ms {
        query = query.bind(before_ms);
    }

    query.fetch_all::<UsageLogRow>().await
}

/// One row of the daily rollup, shaped by the requested `group_by`. Identity
/// columns that aren't part of the grouping come back empty/zero.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageDailyRow {
    /// `YYYY-MM-DD` for day-grouped reads; empty otherwise.
    pub day: String,
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub key_id: Uuid,
    pub model: String,
    pub requests: u64,
    pub success_requests: u64,
    pub error_requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub estimated_tokens: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    /// Average time-to-first-token in ms across the grouped rows.
    pub avg_ttft_ms: f64,
    /// Average end-to-end latency in ms across the grouped rows.
    pub avg_total_ms: f64,
    /// Total USD spend across the grouped rows, summed from the per-request
    /// cost frozen at completion time (never recomputed from current prices).
    pub cost_usd: f64,
}

/// Per-tenant usage aggregate.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageAgg {
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Per-key usage aggregate.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageKeyAgg {
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub key_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

/// Per-model usage aggregate.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageModelAgg {
    pub model: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    /// Median per-request generation throughput (output tokens/sec). Measured
    /// over service time \u2014 decode window (total - ttft) for streamed responses,
    /// or upstream time (total - queue_wait) for non-streamed ones \u2014 so queue
    /// delay under saturation doesn't deflate the rate a user actually sees.
    pub gen_tokens_per_sec: f64,
    /// Aggregate generation throughput: sum of per-request rates across the
    /// window (combined model output rate across all connections).
    pub agg_tokens_per_sec: f64,
    /// Average time-to-first-token in milliseconds.
    pub avg_ttft_ms: f64,
    /// Average end-to-end latency in milliseconds.
    pub avg_total_ms: f64,
    /// Median (p50) time-to-first-token in milliseconds.
    pub p50_ttft_ms: f64,
    /// Median (p50) end-to-end latency in milliseconds.
    pub p50_total_ms: f64,
    /// Average prompt (input) tokens per request.
    pub avg_prompt_tokens: f64,
    /// Average generated (output) tokens per request.
    pub avg_gen_tokens: f64,
    /// Distinct API keys (users) that hit this model in the window.
    pub users: u64,
}

/// Time-bucketed usage for charts.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageTimePoint {
    pub bucket_ms: i64,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
}

#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct TenantUsageTimePoint {
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub bucket_ms: i64,
    pub requests: u64,
    pub total_tokens: u64,
}

/// Time-bucketed per-model series powering the three charts in the expanded
/// model card (throughput, end-to-end latency, time-to-first-token). Throughput
/// is reported as per-stream median engine rates (prefill / decode), not
/// volume-over-wall-clock, so it stays comparable to the upstream engine even on
/// sparse traffic. Latency carries avg + p50 so the charts match the rest of the
/// pipeline.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct ModelUsageTimePoint {
    pub bucket_ms: i64,
    pub requests: u64,
    /// Median per-stream decode rate: output tokens / decode window (total - ttft).
    pub gen_tokens_per_sec: f64,
    /// Median per-stream prefill rate: input tokens / ttft.
    pub prompt_tokens_per_sec: f64,
    /// Average time-to-first-token in milliseconds over the bucket.
    pub avg_ttft_ms: f64,
    /// Median (p50) time-to-first-token in milliseconds over the bucket.
    pub p50_ttft_ms: f64,
    /// Average end-to-end latency in milliseconds over the bucket.
    pub avg_total_ms: f64,
    /// Median (p50) end-to-end latency in milliseconds over the bucket.
    pub p50_total_ms: f64,
}

/// One tenant/key pair's usage of a single model over the window.
/// `gen_tokens_per_sec` reuses the same median per-request decode-rate
/// definition as `/usage/models`.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageKeyModelBreakdown {
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub key_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub requests: u64,
    pub total_tokens: u64,
    pub gen_tokens_per_sec: f64,
}

/// Response-cache effectiveness over the window.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct CacheStats {
    pub hits: u64,
    pub misses: u64,
    /// Tokens served from cache instead of generated upstream.
    pub tokens_saved: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CostAgg {
    pub model: String,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub input_cost: f64,
    pub output_cost: f64,
    pub total_cost: f64,
}

pub async fn query_usage(
    client: &clickhouse::Client,
    q: UsageQuery,
) -> Result<Vec<UsageAgg>, clickhouse::error::Error> {
    let since = q.since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let group = q.group_by.as_deref().unwrap_or("tenant");
    match group {
        "key" | "model" => Ok(vec![]),
        _ => {
            let mut sql = String::from(
                "select tenant_id, count() as requests, \
                 sum(input_tokens) as in_tok, sum(output_tokens) as out_tok, \
                 sum(input_tokens) + sum(output_tokens) as total_tok \
                 from usage where ts_ms >= ?",
            );
            if q.tenant_id.is_some() {
                sql.push_str(" and tenant_id = toUUID(?)");
            }
            if q.key_id.is_some() {
                sql.push_str(" and key_id = toUUID(?)");
            }
            if q.model.is_some() {
                sql.push_str(" and model = ?");
            }
            sql.push_str(" group by tenant_id");
            bind_usage_filters(client.query(&sql).bind(since), &q)
                .fetch_all::<UsageAgg>()
                .await
        }
    }
}

pub async fn query_usage_by_key(
    client: &clickhouse::Client,
    q: UsageQuery,
) -> Result<Vec<UsageKeyAgg>, clickhouse::error::Error> {
    let since = q.since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let mut sql = String::from(
        "select key_id, tenant_id, count() as requests, \
         sum(input_tokens) as in_tok, sum(output_tokens) as out_tok, \
         sum(input_tokens) + sum(output_tokens) as total_tok \
         from usage where ts_ms >= ?",
    );
    if q.tenant_id.is_some() {
        sql.push_str(" and tenant_id = toUUID(?)");
    }
    if q.key_id.is_some() {
        sql.push_str(" and key_id = toUUID(?)");
    }
    sql.push_str(" group by key_id, tenant_id order by total_tok desc");
    if let Some(limit) = q.limit {
        sql.push_str(&format!(" limit {}", limit.min(10_000)));
    }
    bind_usage_filters(client.query(&sql).bind(since), &q)
        .fetch_all::<UsageKeyAgg>()
        .await
}

/// Filters for the per-key usage summary feeds (`/keys/{id}/usage` and
/// `/usage/keys/summary`).
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
pub struct KeyUsageSummaryQuery {
    /// Restrict the bulk feed to one tenant (UUID). Ignored by the single-key
    /// endpoint, which is already scoped to one key.
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    /// Rolling window for the request/token/cost aggregates, unix epoch millis.
    /// Defaults to the last 24h. Note this does **not** bound `last_used_ms` for
    /// the single-key endpoint (which reports the true last use within the
    /// ledger retention window); it does for the bulk endpoint.
    pub since_ms: Option<i64>,
    /// Cap on rows returned by the bulk feed (busiest keys first).
    pub limit: Option<u64>,
}

/// ClickHouse row for per-key summary queries. Aggregate aliases intentionally
/// avoid colliding with source column names (`in_tok` not `input_tokens`) so
/// ClickHouse does not resolve inner `sumIf(input_tokens, …)` to the alias.
#[derive(Debug, Clone, Row, Serialize, Deserialize)]
struct KeyUsageSummaryRow {
    #[serde(with = "clickhouse::serde::uuid")]
    pub key_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    pub tenant_id: Uuid,
    pub last_used_ms: i64,
    pub last_model: String,
    pub last_status_code: u16,
    pub requests: u64,
    pub in_tok: u64,
    pub out_tok: u64,
    pub total_tok: u64,
    pub cost_sum: f64,
}

impl From<KeyUsageSummaryRow> for KeyUsageSummary {
    fn from(r: KeyUsageSummaryRow) -> Self {
        KeyUsageSummary {
            key_id: r.key_id,
            tenant_id: r.tenant_id,
            last_used_ms: r.last_used_ms,
            last_model: r.last_model,
            last_status_code: r.last_status_code,
            requests: r.requests,
            input_tokens: r.in_tok,
            output_tokens: r.out_tok,
            total_tokens: r.total_tok,
            cost_usd: r.cost_sum,
        }
    }
}

/// Activity summary for a single API key: when it was last seen, what it last
/// called, and rolling usage totals. `last_used_ms` is `0` when the key has no
/// requests in the queried range (i.e. never used, within ledger retention).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct KeyUsageSummary {
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub key_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    /// Unix epoch millis of the most recent request, or `0` if never used.
    pub last_used_ms: i64,
    /// Model named on the most recent request (empty if never used).
    pub last_model: String,
    /// HTTP status of the most recent request (`0` if never used).
    pub last_status_code: u16,
    /// Requests in the rolling window.
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    /// USD spend in the window, summed from each request's frozen cost.
    pub cost_usd: f64,
}

/// Summary for one key. `last_used_ms` / `last_model` / `last_status_code` are
/// computed over the full retained ledger for that key (cheap — one key is a
/// narrow scan), while the request/token/cost columns are limited to the
/// rolling window. Returns `None` when the key has never appeared in the ledger.
pub async fn query_key_usage_summary(
    client: &clickhouse::Client,
    key_id: Uuid,
    since_ms: Option<i64>,
) -> Result<Option<KeyUsageSummary>, clickhouse::error::Error> {
    let since = since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let sql = "select \
         key_id, \
         any(tenant_id) as tenant_id, \
         max(ts_ms) as last_used_ms, \
         argMax(model, ts_ms) as last_model, \
         argMax(status_code, ts_ms) as last_status_code, \
         countIf(ts_ms >= ?) as requests, \
         sumIf(input_tokens, ts_ms >= ?) as in_tok, \
         sumIf(output_tokens, ts_ms >= ?) as out_tok, \
         sumIf(toUInt64(input_tokens) + toUInt64(output_tokens), ts_ms >= ?) as total_tok, \
         sumIf(cost_usd, ts_ms >= ?) as cost_sum \
         from usage where key_id = toUUID(?) group by key_id";
    let rows = client
        .query(sql)
        .bind(since)
        .bind(since)
        .bind(since)
        .bind(since)
        .bind(since)
        .bind(key_id.to_string())
        .fetch_all::<KeyUsageSummaryRow>()
        .await?;
    Ok(rows.into_iter().next().map(KeyUsageSummary::from))
}

/// Bulk per-key summary for the dashboard. The window (`since_ms`) bounds the
/// whole scan, so `last_used_ms` here is the last use *within the window* and
/// only keys with activity in the window are returned. Ordered by token volume.
pub async fn query_keys_usage_summary(
    client: &clickhouse::Client,
    q: KeyUsageSummaryQuery,
) -> Result<Vec<KeyUsageSummary>, clickhouse::error::Error> {
    let since = q.since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let limit = q.limit.unwrap_or(1000).min(10_000);
    let mut sql = String::from(
        "select \
         key_id, \
         any(tenant_id) as tenant_id, \
         max(ts_ms) as last_used_ms, \
         argMax(model, ts_ms) as last_model, \
         argMax(status_code, ts_ms) as last_status_code, \
         count() as requests, \
         sum(input_tokens) as in_tok, \
         sum(output_tokens) as out_tok, \
         sum(toUInt64(input_tokens) + toUInt64(output_tokens)) as total_tok, \
         sum(cost_usd) as cost_sum \
         from usage where ts_ms >= ?",
    );
    if q.tenant_id.is_some() {
        // Qualify the raw column so the SELECT alias `tenant_id` cannot shadow it.
        sql.push_str(" and usage.tenant_id = toUUID(?)");
    }
    sql.push_str(&format!(
        " group by key_id order by total_tok desc limit {limit}"
    ));
    let mut query = client.query(&sql).bind(since);
    if let Some(tid) = q.tenant_id {
        query = query.bind(tid.to_string());
    }
    let rows = query.fetch_all::<KeyUsageSummaryRow>().await?;
    Ok(rows.into_iter().map(KeyUsageSummary::from).collect())
}

pub async fn query_usage_by_model(
    client: &clickhouse::Client,
    q: UsageQuery,
) -> Result<Vec<UsageModelAgg>, clickhouse::error::Error> {
    let since = q.since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let mut sql = String::from(
        "select model, count() as requests, \
         sum(input_tokens) as in_tok, sum(output_tokens) as out_tok, \
         sum(input_tokens) + sum(output_tokens) as total_tok, \
         round(if(countIf(total_ms >= 20 and output_tokens >= 1) > 0, \
         quantileIf(0.5)(output_tokens / (greatest(if(total_ms - ttft_ms >= 20, total_ms - ttft_ms, total_ms - queue_wait_ms), 1) / 1000.), \
         total_ms >= 20 and output_tokens >= 1), 0), 1) as gen_tps, \
         round(sumIf(output_tokens / (greatest(if(total_ms - ttft_ms >= 20, total_ms - ttft_ms, total_ms - queue_wait_ms), 1) / 1000.), \
         total_ms >= 20 and output_tokens >= 1), 1) as agg_tps, \
         round(if(countIf(ttft_ms > 0) > 0, avgIf(ttft_ms, ttft_ms > 0), 0), 1) as avg_ttft, \
         round(if(countIf(total_ms > 0) > 0, avgIf(total_ms, total_ms > 0), 0), 1) as avg_total, \
         round(if(countIf(ttft_ms > 0) > 0, quantileIf(0.5)(ttft_ms, ttft_ms > 0), 0), 1) as p50_ttft, \
         round(if(countIf(total_ms > 0) > 0, quantileIf(0.5)(total_ms, total_ms > 0), 0), 1) as p50_total, \
         round(avg(input_tokens), 1) as avg_prompt, \
         round(avg(output_tokens), 1) as avg_gen, \
         uniq(key_id) as users \
         from usage where ts_ms >= ?",
    );
    if q.tenant_id.is_some() {
        sql.push_str(" and tenant_id = toUUID(?)");
    }
    if q.model.is_some() {
        sql.push_str(" and model = ?");
    }
    sql.push_str(" group by model order by total_tok desc");
    bind_usage_filters(client.query(&sql).bind(since), &q)
        .fetch_all::<UsageModelAgg>()
        .await
}

pub async fn query_usage_series(
    client: &clickhouse::Client,
    q: UsageSeriesQuery,
) -> Result<Vec<UsageTimePoint>, clickhouse::error::Error> {
    let since = q.since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let bucket = q.bucket_ms.unwrap_or(300_000).max(60_000);
    let mut sql = format!(
        "select intDiv(ts_ms, {bucket}) * {bucket} as bucket_ms, \
         count() as requests, \
         sum(input_tokens) as in_tok, sum(output_tokens) as out_tok, \
         sum(input_tokens) + sum(output_tokens) as total_tok \
         from usage where ts_ms >= ?"
    );
    if q.tenant_id.is_some() {
        sql.push_str(" and tenant_id = toUUID(?)");
    }
    sql.push_str(" group by bucket_ms order by bucket_ms");
    let mut query = client.query(&sql).bind(since);
    if let Some(tid) = q.tenant_id {
        query = query.bind(tid.to_string());
    }
    query.fetch_all::<UsageTimePoint>().await
}

pub async fn query_usage_series_by_tenant(
    client: &clickhouse::Client,
    q: UsageSeriesQuery,
) -> Result<Vec<TenantUsageTimePoint>, clickhouse::error::Error> {
    let since = q.since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let bucket = q.bucket_ms.unwrap_or(300_000).max(10_000);
    let sql = format!(
        "select tenant_id, intDiv(ts_ms, {bucket}) * {bucket} as bucket_ms, \
         count() as requests, \
         sum(input_tokens) + sum(output_tokens) as total_tokens \
         from usage where ts_ms >= ? \
         group by tenant_id, bucket_ms order by bucket_ms, tenant_id"
    );
    client
        .query(&sql)
        .bind(since)
        .fetch_all::<TenantUsageTimePoint>()
        .await
}

/// Per-model time series for the expanded model card. Mirrors the tenant
/// series' bucketing (`intDiv(ts_ms, {bucket}) * {bucket}`) but is scoped to a
/// single model and computes throughput + latency per bucket. Throughput is
/// aggregate (sum of tokens over the bucket / bucket seconds); latency reuses
/// the same guarded avg/p50 expressions as `query_usage_by_model`.
pub async fn query_usage_series_by_model(
    client: &clickhouse::Client,
    q: UsageSeriesQuery,
) -> Result<Vec<ModelUsageTimePoint>, clickhouse::error::Error> {
    let since = q.since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let bucket = q.bucket_ms.unwrap_or(300_000).max(10_000);
    // Per-stream engine rates, NOT volume-over-wall-clock. `gen_tps` is the
    // median decode rate (output tokens over the decode window = total - ttft,
    // falling back to total - queue when the response wasn't streamed), the same
    // definition `/usage/models` uses. `prompt_tps` is the median prefill rate
    // (prompt tokens over ttft, since prefill completes by the first byte). This
    // makes the chart comparable to the upstream engine's reported throughput
    // instead of collapsing toward zero on sparse, bursty traffic.
    let sql = format!(
        "select intDiv(ts_ms, {bucket}) * {bucket} as bucket_ms, \
         count() as requests, \
         round(if(countIf(total_ms >= 20 and output_tokens >= 1) > 0, \
         quantileIf(0.5)(output_tokens / (greatest(if(total_ms - ttft_ms >= 20, total_ms - ttft_ms, total_ms - queue_wait_ms), 1) / 1000.), \
         total_ms >= 20 and output_tokens >= 1), 0), 1) as gen_tps, \
         round(if(countIf(ttft_ms >= 20 and input_tokens >= 1) > 0, \
         quantileIf(0.5)(input_tokens / (greatest(ttft_ms, 1) / 1000.), \
         ttft_ms >= 20 and input_tokens >= 1), 0), 1) as prompt_tps, \
         round(if(countIf(ttft_ms > 0) > 0, avgIf(ttft_ms, ttft_ms > 0), 0), 1) as avg_ttft, \
         round(if(countIf(ttft_ms > 0) > 0, quantileIf(0.5)(ttft_ms, ttft_ms > 0), 0), 1) as p50_ttft, \
         round(if(countIf(total_ms > 0) > 0, avgIf(total_ms, total_ms > 0), 0), 1) as avg_total, \
         round(if(countIf(total_ms > 0) > 0, quantileIf(0.5)(total_ms, total_ms > 0), 0), 1) as p50_total \
         from usage where ts_ms >= ? and model = ? \
         group by bucket_ms order by bucket_ms"
    );
    let model = q.model.unwrap_or_default();
    client
        .query(&sql)
        .bind(since)
        .bind(model)
        .fetch_all::<ModelUsageTimePoint>()
        .await
}

/// Per tenant/key breakdown for one model. Mirrors the gen-rate math in
/// `query_usage_by_model` but groups by `(key_id, tenant_id)` and is scoped to
/// a single model, so the expanded model card can show who is driving load.
pub async fn query_usage_breakdown_by_model(
    client: &clickhouse::Client,
    model: &str,
    since_ms: Option<i64>,
    limit: Option<u64>,
) -> Result<Vec<UsageKeyModelBreakdown>, clickhouse::error::Error> {
    let since = since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let limit = limit.unwrap_or(100).min(1000);
    let sql = format!(
        "select key_id, tenant_id, count() as requests, \
         sum(input_tokens) + sum(output_tokens) as total_tokens, \
         round(if(countIf(total_ms >= 20 and output_tokens >= 1) > 0, \
         quantileIf(0.5)(output_tokens / (greatest(if(total_ms - ttft_ms >= 20, total_ms - ttft_ms, total_ms - queue_wait_ms), 1) / 1000.), \
         total_ms >= 20 and output_tokens >= 1), 0), 1) as gen_tps \
         from usage where ts_ms >= ? and model = ? \
         group by key_id, tenant_id order by total_tokens desc limit {limit}"
    );
    client
        .query(&sql)
        .bind(since)
        .bind(model.to_string())
        .fetch_all::<UsageKeyModelBreakdown>()
        .await
}

pub async fn query_cache_stats(
    client: &clickhouse::Client,
    q: UsageQuery,
) -> Result<CacheStats, clickhouse::error::Error> {
    let since = q.since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let mut sql = String::from(
        "select countIf(cache_status = 'hit') as hits, \
         countIf(cache_status = 'miss') as misses, \
         sumIf(input_tokens + output_tokens, cache_status = 'hit') as tokens_saved \
         from usage where ts_ms >= ?",
    );
    if q.tenant_id.is_some() {
        sql.push_str(" and tenant_id = toUUID(?)");
    }
    if q.model.is_some() {
        sql.push_str(" and model = ?");
    }
    let mut query = client.query(&sql).bind(since);
    if let Some(tid) = q.tenant_id {
        query = query.bind(tid.to_string());
    }
    if let Some(model) = &q.model {
        query = query.bind(model.clone());
    }
    let rows = query.fetch_all::<CacheStats>().await?;
    Ok(rows.into_iter().next().unwrap_or(CacheStats {
        hits: 0,
        misses: 0,
        tokens_saved: 0,
    }))
}

pub async fn query_costs(
    client: &clickhouse::Client,
    since_ms: Option<i64>,
    model_costs: &[(String, f64, f64)],
) -> Result<Vec<CostAgg>, clickhouse::error::Error> {
    // One row per model from the raw ledger, carrying the authoritative spend
    // (`total_cost`) summed from each request's frozen cost — never recomputed
    // from current prices. The per-token rates are only used to split that
    // total into an informational input/output breakdown.
    #[derive(Row, Deserialize)]
    struct CostRawRow {
        model: String,
        requests: u64,
        input_tokens: u64,
        output_tokens: u64,
        total_cost: f64,
    }
    let since = since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let rows = client
        .query(
            "select model, count() as requests, \
             sum(input_tokens) as input_tokens, sum(output_tokens) as output_tokens, \
             sum(cost_usd) as total_cost \
             from usage where ts_ms >= ? group by model order by total_cost desc",
        )
        .bind(since)
        .fetch_all::<CostRawRow>()
        .await?;
    let cost_map: std::collections::HashMap<String, (f64, f64)> = model_costs
        .iter()
        .map(|(m, i, o)| (m.clone(), (*i, *o)))
        .collect();
    Ok(rows
        .into_iter()
        .map(|u| {
            let (in_rate, out_rate) = cost_map.get(&u.model).copied().unwrap_or((0.0, 0.0));
            let input_cost = u.input_tokens as f64 * in_rate;
            let output_cost = u.output_tokens as f64 * out_rate;
            CostAgg {
                model: u.model,
                requests: u.requests,
                input_tokens: u.input_tokens,
                output_tokens: u.output_tokens,
                input_cost,
                output_cost,
                total_cost: u.total_cost,
            }
        })
        .collect())
}

/// Read the permanent daily rollup over an inclusive `[start_day, end_day]`
/// range. Averages are reconstructed from the stored latency sums and request
/// counts (the rollup keeps sums, not per-request rows).
pub async fn query_usage_daily(
    client: &clickhouse::Client,
    q: UsageDailyQuery,
    key_ids: &[Uuid],
) -> Result<Vec<UsageDailyRow>, clickhouse::error::Error> {
    let group = q.group_by.as_deref().unwrap_or("day");
    // Identity columns selected per grouping; the rest are zeroed so the row
    // shape stays uniform for the typed `Row` decode.
    let (day_col, tenant_col, key_col, model_col, group_clause) = match group {
        "tenant" => (
            "'' as day",
            "tenant_id",
            "toUUID('00000000-0000-0000-0000-000000000000') as key_id",
            "'' as model",
            "group by tenant_id order by total_tokens desc",
        ),
        "key" => (
            "'' as day",
            "tenant_id",
            "key_id",
            "'' as model",
            "group by tenant_id, key_id order by total_tokens desc",
        ),
        "model" => (
            "'' as day",
            "toUUID('00000000-0000-0000-0000-000000000000') as tenant_id",
            "toUUID('00000000-0000-0000-0000-000000000000') as key_id",
            "model",
            "group by model order by total_tokens desc",
        ),
        "key_model" => (
            "'' as day",
            "tenant_id",
            "key_id",
            "model",
            "group by tenant_id, key_id, model order by total_tokens desc",
        ),
        _ => (
            "toString(day)",
            "toUUID('00000000-0000-0000-0000-000000000000') as tenant_id",
            "toUUID('00000000-0000-0000-0000-000000000000') as key_id",
            "'' as model",
            "group by day order by day",
        ),
    };

    // Filters run against the physical table inside a subquery. This keeps the
    // real `day`/`tenant_id`/`model` columns unambiguous: the outer SELECT
    // replaces some of them with literal placeholders (e.g. `'' as day`), and
    // ClickHouse's analyzer makes SELECT aliases visible in WHERE — so applying
    // the predicates here would otherwise try to parse `''` as a Date.
    let mut inner_where = String::from("where day >= toDate(?) and day <= toDate(?)");
    if q.tenant_id.is_some() {
        inner_where.push_str(" and tenant_id = toUUID(?)");
    }
    if !key_ids.is_empty() {
        let placeholders = std::iter::repeat("toUUID(?)")
            .take(key_ids.len())
            .collect::<Vec<_>>()
            .join(", ");
        inner_where.push_str(&format!(" and key_id in ({placeholders})"));
    }
    if q.model.is_some() {
        inner_where.push_str(" and model = ?");
    }

    let mut sql = format!(
        "select {day_col}, {tenant_col}, {key_col}, {model_col}, \
         sum(requests), \
         sum(success_requests), \
         sum(error_requests), \
         sum(input_tokens), \
         sum(output_tokens), \
         sum(input_tokens) + sum(output_tokens) as total_tokens, \
         sum(estimated_tokens), \
         sum(cache_hits), \
         sum(cache_misses), \
         round(sum(ttft_ms_sum) / greatest(sum(success_requests), 1), 1) as avg_ttft_ms, \
         round(sum(total_ms_sum) / greatest(sum(success_requests), 1), 1) as avg_total_ms, \
         round(sum(cost_usd_sum), 6) as cost_usd \
         from (select * from usage_daily {inner_where}) "
    );
    sql.push_str(group_clause);

    let start = q.start_day.clone().unwrap_or_else(default_start_day);
    let end = q.end_day.clone().unwrap_or_else(today_day);
    let mut query = client.query(&sql).bind(start).bind(end);
    if let Some(tid) = q.tenant_id {
        query = query.bind(tid.to_string());
    }
    for kid in key_ids {
        query = query.bind(kid.to_string());
    }
    if let Some(model) = &q.model {
        query = query.bind(model.clone());
    }
    query.fetch_all::<UsageDailyRow>().await
}

/// `usage` day-partition ids (`YYYYMMDD`) strictly older than
/// `cutoff_yyyymmdd`. Used by the retention worker and manual compact to drop
/// whole day-partitions (an O(1) metadata op). Never touches `usage_daily`,
/// which is permanent.
pub async fn usage_partitions_before(
    client: &clickhouse::Client,
    cutoff_yyyymmdd: u32,
) -> Result<Vec<String>, clickhouse::error::Error> {
    let sql = "select distinct partition from system.parts \
               where database = currentDatabase() and table = 'usage' and active \
               and toUInt32(partition) < ? order by partition";
    client
        .query(sql)
        .bind(cutoff_yyyymmdd)
        .fetch_all::<String>()
        .await
}

/// Drop a single `usage` day-partition. `partition` is a `YYYYMMDD` id as
/// returned by [`usage_partitions_before`].
pub async fn drop_usage_partition(
    client: &clickhouse::Client,
    partition: &str,
) -> Result<(), clickhouse::error::Error> {
    // Partition ids come straight from ClickHouse's own `system.parts` (numeric
    // YYYYMMDD), never user input, and DDL cannot take bound parameters \u2014 hence
    // the format. Guard anyway so a non-numeric value can never be interpolated.
    if !partition.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(());
    }
    let sql = format!("alter table usage drop partition id '{partition}'");
    client.query(&sql).execute().await
}

fn obleth_admin_health_probe_label() -> &'static str {
    crate::model_health::HEALTH_PROBE_REQUEST_TYPE
}

fn default_start_day() -> String {
    use chrono::{Duration, Utc};
    (Utc::now() - Duration::days(7))
        .format("%Y-%m-%d")
        .to_string()
}

fn today_day() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

fn bind_usage_filters(
    mut query: clickhouse::query::Query,
    q: &UsageQuery,
) -> clickhouse::query::Query {
    if let Some(tid) = q.tenant_id {
        query = query.bind(tid.to_string());
    }
    if let Some(kid) = q.key_id {
        query = query.bind(kid.to_string());
    }
    if let Some(model) = &q.model {
        query = query.bind(model.clone());
    }
    query
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One row from the `spans` table, returned by the per-request trace endpoint.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, clickhouse::Row)]
pub struct SpanEntry {
    #[serde(with = "clickhouse::serde::uuid")]
    pub request_id: Uuid,
    pub span_name: String,
    pub parent_span: String,
    pub start_ms: i64,
    pub duration_ms: u32,
    pub status: String,
    pub attributes: String,
}

/// Fetch all spans for a single request, ordered by `start_ms`.
pub async fn query_request_spans(
    client: &clickhouse::Client,
    request_id: Uuid,
) -> Result<Vec<SpanEntry>, clickhouse::error::Error> {
    client
        .query(
            "SELECT request_id, span_name, parent_span, start_ms, duration_ms, status, attributes \
             FROM spans WHERE request_id = ? ORDER BY start_ms",
        )
        .bind(request_id.to_string())
        .fetch_all::<SpanEntry>()
        .await
}

/// Returns the subset of `request_ids` that have at least one row in `spans`.
/// Fails-open: on ClickHouse error returns an empty set (callers treat all rows
/// as un-traced rather than surfacing a 500 to the log-list endpoint).
pub async fn batch_has_trace(
    client: &clickhouse::Client,
    request_ids: &[Uuid],
) -> std::collections::HashSet<Uuid> {
    if request_ids.is_empty() {
        return std::collections::HashSet::new();
    }
    let in_list = request_ids
        .iter()
        .map(|id| format!("toUUID('{id}')"))
        .collect::<Vec<_>>()
        .join(", ");
    #[derive(clickhouse::Row, serde::Deserialize)]
    struct TracedId {
        #[serde(with = "clickhouse::serde::uuid")]
        request_id: Uuid,
    }
    let sql = format!("SELECT DISTINCT request_id FROM spans WHERE request_id IN ({in_list})");
    match client.query(&sql).fetch_all::<TracedId>().await {
        Ok(rows) => rows.into_iter().map(|r| r.request_id).collect(),
        Err(e) => {
            tracing::warn!(error = %e, "batch_has_trace failed");
            std::collections::HashSet::new()
        }
    }
}
