//! Usage/cost reads from the ClickHouse ledger.

use clickhouse::Row;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

#[derive(Debug, Deserialize)]
pub struct UsageQuery {
    pub tenant_id: Option<Uuid>,
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

#[derive(Debug, Deserialize)]
pub struct UsageSeriesQuery {
    pub tenant_id: Option<Uuid>,
    pub since_ms: Option<i64>,
    /// Bucket width in milliseconds. Default 300_000 (5 minutes).
    pub bucket_ms: Option<i64>,
}

/// Date-range read against the permanent daily rollup (`usage_daily`).
#[derive(Debug, Deserialize)]
pub struct UsageDailyQuery {
    /// Inclusive lower bound, `YYYY-MM-DD`. Defaults to 7 days ago.
    pub start_day: Option<String>,
    /// Inclusive upper bound, `YYYY-MM-DD`. Defaults to today.
    pub end_day: Option<String>,
    pub tenant_id: Option<Uuid>,
    pub key_id: Option<Uuid>,
    pub model: Option<String>,
    /// Aggregate dimension: `day` (default), `tenant`, `key`, `model`,
    /// or `key_model` (one row per key+model across the whole range).
    pub group_by: Option<String>,
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
    let usage = query_usage_by_model(
        client,
        UsageQuery {
            tenant_id: None,
            key_id: None,
            model: None,
            since_ms,
            group_by: Some("model".into()),
            limit: None,
        },
    )
    .await?;
    let cost_map: std::collections::HashMap<String, (f64, f64)> = model_costs
        .iter()
        .map(|(m, i, o)| (m.clone(), (*i, *o)))
        .collect();
    Ok(usage
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
                total_cost: input_cost + output_cost,
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
    if q.key_id.is_some() {
        inner_where.push_str(" and key_id = toUUID(?)");
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
         round(sum(total_ms_sum) / greatest(sum(success_requests), 1), 1) as avg_total_ms \
         from (select * from usage_daily {inner_where}) "
    );
    sql.push_str(group_clause);

    let start = q.start_day.clone().unwrap_or_else(default_start_day);
    let end = q.end_day.clone().unwrap_or_else(today_day);
    let mut query = client.query(&sql).bind(start).bind(end);
    if let Some(tid) = q.tenant_id {
        query = query.bind(tid.to_string());
    }
    if let Some(kid) = q.key_id {
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
