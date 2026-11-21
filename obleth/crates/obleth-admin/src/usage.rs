//! Usage/cost reads from the ClickHouse ledger.

use clickhouse::Row;
use obleth_config::{DECODE_MS_SQL, TTFT_OK_SQL};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// `request_type` labels for gateway-internal traffic: health probes and
/// benchmark runs from synthetic tenants. Default usage/cost reads exclude
/// them; pass `include_internal=true` to opt back in.
pub const INTERNAL_REQUEST_TYPES: &[&str] = &[
    crate::model_health::HEALTH_PROBE_REQUEST_TYPE,
    obleth_config::BENCHMARK_REQUEST_TYPE,
];

/// SQL fragment excluding internal traffic unless explicitly included. The
/// labels are compile-time constants, so inlining them is injection-safe.
pub(crate) fn internal_filter(include_internal: Option<bool>) -> String {
    if include_internal == Some(true) {
        return String::new();
    }
    let list = INTERNAL_REQUEST_TYPES
        .iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(" and request_type not in ({list})")
}

/// Mean first-token time of the [`TTFT_OK_SQL`] requests, 0 when there are
/// none.
fn avg_ttft_sql() -> String {
    format!("round(if(countIf({TTFT_OK_SQL}) > 0, avgIf(ttft_ms, {TTFT_OK_SQL}), 0), 1)")
}

/// Median first-token time of the [`TTFT_OK_SQL`] requests, 0 when there are
/// none.
fn p50_ttft_sql() -> String {
    format!("round(if(countIf({TTFT_OK_SQL}) > 0, quantileIf(0.5)(ttft_ms, {TTFT_OK_SQL}), 0), 1)")
}

/// The requests a prefill rate is read from: a real first token (see
/// [`TTFT_OK_SQL`]) late enough to time, and a prompt.
fn prefill_rated_sql() -> String {
    format!("{TTFT_OK_SQL} and ttft_ms >= 20 and input_tokens >= 1")
}

/// Filters of the aggregate usage reads (`/usage`, `/usage/keys`,
/// `/usage/models`, `/usage/cache`, `/costs`, `/overview/summary`). Every
/// read honours every field; an unknown parameter is refused.
#[derive(Debug, Default, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageQuery {
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    #[schema(value_type = Option<String>)]
    pub key_id: Option<Uuid>,
    pub model: Option<String>,
    /// Inclusive lower bound, unix epoch millis. Defaults to a day before
    /// `until_ms` (or now).
    pub since_ms: Option<i64>,
    /// Exclusive upper bound, unix epoch millis. Open-ended by default, so a
    /// previous period is one bounded read.
    pub until_ms: Option<i64>,
    /// Most rows a grouped read returns, busiest first: default 1,000, at
    /// most 10,000. Single-row reads return their one row.
    pub limit: Option<u64>,
    /// When true, include internal traffic (health probes, benchmark runs)
    /// that is excluded from this read by default.
    pub include_internal: Option<bool>,
}

#[derive(Debug, Default, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageSeriesQuery {
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    /// Restrict the series to one model. Required by the per-model series.
    pub model: Option<String>,
    /// Inclusive lower bound, unix epoch millis. Defaults to a day before
    /// `until_ms` (or now).
    pub since_ms: Option<i64>,
    /// Exclusive upper bound, unix epoch millis. Open-ended by default.
    pub until_ms: Option<i64>,
    /// Bucket width in milliseconds. Default 300_000 (5 minutes).
    pub bucket_ms: Option<i64>,
    /// When true, include internal traffic (health probes, benchmark runs)
    /// that is excluded from this read by default.
    pub include_internal: Option<bool>,
}

/// Filters for the per-model tenant/key breakdown
/// (`GET /api/v1/usage/breakdown`). Scoped to a single model so the expanded
/// model card can show which tenants/keys are driving its load.
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageBreakdownQuery {
    /// Model to break down (required).
    pub model: String,
    /// Inclusive lower bound, unix epoch millis. Defaults to a day before
    /// `until_ms` (or now).
    pub since_ms: Option<i64>,
    /// Exclusive upper bound, unix epoch millis. Open-ended by default.
    pub until_ms: Option<i64>,
    /// Most rows, busiest tenant/key pairs first: default 100, at most 10,000.
    pub limit: Option<u64>,
    /// When true, include internal traffic (health probes, benchmark runs)
    /// that is excluded from this read by default.
    pub include_internal: Option<bool>,
}

/// Filters for `GET /api/v1/usage/speed`: how fast each model answered over a
/// window, as one row per model or one per model per hour or day.
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageSpeedQuery {
    /// Inclusive lower bound, unix epoch millis. Defaults to the last 24h.
    pub since_ms: Option<i64>,
    /// Exclusive upper bound, unix epoch millis. Defaults to now.
    pub until_ms: Option<i64>,
    /// `none` (default) for one row per model over the whole window, or
    /// `hour` / `day` for one row per model per hour or day.
    pub bucket: Option<String>,
    /// IANA time zone the hours and days are cut in. Defaults to UTC.
    pub tz: Option<String>,
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    #[schema(value_type = Option<String>)]
    pub key_id: Option<Uuid>,
    pub model: Option<String>,
    /// When true, include internal traffic (health probes, benchmark runs)
    /// that is excluded from this read by default.
    pub include_internal: Option<bool>,
}

/// Date-range read against the permanent daily rollup (`usage_daily`).
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageDailyQuery {
    /// Inclusive lower bound, `YYYY-MM-DD`. Defaults to 7 days ago.
    pub start_day: Option<String>,
    /// Inclusive upper bound, `YYYY-MM-DD`. Defaults to today.
    pub end_day: Option<String>,
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    /// One key id, or up to 200 joined by commas (`key_id=a,b,c`), so a
    /// caller can fetch spend across all of a user's rotated keys in one
    /// request.
    #[serde(default, deserialize_with = "comma_list")]
    #[schema(value_type = Option<String>)]
    #[param(value_type = Option<String>)]
    pub key_id: Vec<Uuid>,
    pub model: Option<String>,
    /// Aggregate dimension: `day` (default), `tenant`, `key`, `model`,
    /// `key_model` (one row per key+model across the whole range), or one of
    /// the per-day splits `day_tenant`, `day_model` and `day_key_model`.
    pub group_by: Option<String>,
}

/// Most values one request-log filter takes as a comma-separated list.
pub const MAX_LOG_FILTER_VALUES: usize = 200;

/// `key_id` of the key-watching reads: like [`comma_list`], except a value
/// that is present but names no key (`key_id=`, `key_id=,`) is an error. An
/// empty list would otherwise drop the key filter and return every key's rows.
pub(crate) fn key_id_list<'de, D>(de: D) -> Result<Vec<Uuid>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let Some(raw) = Option::<String>::deserialize(de)? else {
        return Ok(Vec::new());
    };
    let keys: Vec<Uuid> = parse_comma_list(&raw)?;
    if keys.is_empty() {
        return Err(serde::de::Error::custom(
            "key_id names no key; leave it out to read every key",
        ));
    }
    Ok(keys)
}

/// A request-log filter given as one value or several joined by commas
/// (`a,b,c`); a row matches any of them. Empty entries are ignored, so a
/// trailing comma is harmless. A bad entry, or more than
/// [`MAX_LOG_FILTER_VALUES`], fails the whole query.
fn comma_list<'de, D, T>(de: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    match Option::<String>::deserialize(de)? {
        Some(raw) => parse_comma_list(&raw),
        None => Ok(Vec::new()),
    }
}

/// Split `raw` on commas, skip empty items and parse the rest, at most
/// [`MAX_LOG_FILTER_VALUES`] of them.
fn parse_comma_list<T, E>(raw: &str) -> Result<Vec<T>, E>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
    E: serde::de::Error,
{
    let values = raw
        .split(',')
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.parse::<T>().map_err(E::custom))
        .collect::<Result<Vec<T>, _>>()?;
    if values.len() > MAX_LOG_FILTER_VALUES {
        return Err(E::custom(format!(
            "at most {MAX_LOG_FILTER_VALUES} values per filter"
        )));
    }
    Ok(values)
}

/// Filters for the raw per-request log feed (`GET /api/v1/usage/logs`). This is
/// the only read that returns individual `usage` rows rather than an aggregate,
/// and it powers the live request-log view in the control plane.
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UsageLogQuery {
    /// One tenant id, or several joined by commas.
    #[serde(default, deserialize_with = "comma_list")]
    #[schema(value_type = Option<String>)]
    #[param(value_type = Option<String>)]
    pub tenant_id: Vec<Uuid>,
    /// One key id, or several joined by commas (`a,b,c`), so a search can
    /// match every key whose name fits a pattern.
    #[serde(default, deserialize_with = "comma_list")]
    #[schema(value_type = Option<String>)]
    #[param(value_type = Option<String>)]
    pub key_id: Vec<Uuid>,
    /// One model, or several joined by commas.
    #[serde(default, deserialize_with = "comma_list")]
    #[schema(value_type = Option<String>)]
    #[param(value_type = Option<String>)]
    pub model: Vec<String>,
    /// Coarse request class (`chat`, `embedding`, `audio`, ...).
    pub request_type: Option<String>,
    pub session_id: Option<String>,
    /// Exact match on the token device id (identity-key traffic).
    pub device_id: Option<String>,
    /// Status filter: `success` (2xx/3xx), `error` (>=400), or all (default).
    pub status: Option<String>,
    /// One exact HTTP status, e.g. `502`. Combines with `status`.
    pub status_code: Option<u16>,
    /// Whose failures: `platform` (the gateway or a model server could not
    /// serve the request) or `client` (the request could not be served as
    /// sent). Implies failures only.
    pub fault: Option<String>,
    /// One failure class, or several joined by commas (`capacity,network`);
    /// see `obleth_config::ErrorClass`.
    #[serde(default, deserialize_with = "comma_list")]
    #[schema(value_type = Option<String>)]
    #[param(value_type = Option<String>)]
    pub error_class: Vec<String>,
    /// Case-insensitive prefix match on the request id, for the search box.
    pub request_id: Option<String>,
    /// Where the request came from: one address (`203.0.113.7`) or a CIDR
    /// range (`10.0.0.0/8`). Something that is neither matches no rows.
    /// Requests whose address has passed its retention never match.
    pub client_ip: Option<String>,
    /// Only the helper calls a boon made for this client request (a
    /// speculation draft, an image description, ...): the rows whose
    /// `parent_request_id` is this id. Combine with `since_ms`/`until_ms`
    /// around the request's own time; the default window is the last 24h.
    #[schema(value_type = Option<String>)]
    pub parent_request_id: Option<Uuid>,
    /// Inclusive lower bound, unix epoch millis. Defaults to the last 24h.
    pub since_ms: Option<i64>,
    /// Exclusive upper bound, unix epoch millis. Open-ended by default.
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
    /// When true, include internal traffic (health probes, benchmark runs)
    /// that is hidden from the request log by default.
    pub include_internal: Option<bool>,
    /// Histogram only (`/usage/logs/histogram`): bucket width in
    /// milliseconds. Default 60_000; widened so the window fits in 400
    /// buckets. The cursor and `limit` do not apply there.
    pub bucket_ms: Option<i64>,
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
    pub device_id: String,
    pub admission: String,
    pub status_code: u16,
    /// Every prompt token, cached ones included.
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub total_tokens: u64,
    /// The part of `input_tokens` the upstream served from its prefix cache.
    pub cached_input_tokens: u32,
    pub queue_wait_ms: u32,
    pub ttft_ms: u32,
    pub total_ms: u32,
    pub cache_status: String,
    pub cost_usd: f64,
    pub energy_wh: f64,
    pub energy_cost_usd: f64,
    pub co2_g: f64,
    /// On a helper-call row (a boon's call to another model, `admission` =
    /// `boon`), the client request it served; the nil UUID on every other row.
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub parent_request_id: Uuid,
    /// The variant name the client called the model by, or empty. `model` is
    /// the parent model's name either way.
    pub model_variant: String,
    /// The reasoning effort the request was served at, `unsupported` when the
    /// model has no effort control, or empty when it went through as sent.
    pub reasoning_effort: String,
    /// The reasoning effort the caller asked for, or empty.
    pub reasoning_effort_requested: String,
    /// The model family the caller named, or empty; `model` is the member
    /// that served the request.
    pub model_family: String,
    /// The key's and tenant's names as recorded on the row when it was
    /// served; empty on rows written before they were. Not serialized: the
    /// handler folds them into the entry's `key_name` / `tenant_name`, which
    /// still name a deleted key or tenant.
    #[serde(skip_serializing)]
    pub recorded_key_name: String,
    #[serde(skip_serializing)]
    pub recorded_tenant_name: String,
    /// Whether the reply was relayed to the client as an event stream. Only
    /// a streamed reply has a first token: `ttft_ms` is 0 on the others
    /// (rows written before this column existed read false here and may
    /// still carry a first-token time equal to the whole reply).
    pub streamed: bool,
    /// Why it failed, as recorded; empty on a success and on older rows.
    pub error_reason: String,
    /// What kind of failure the request met (`obleth_config::ErrorClass`), or
    /// empty for a success. Rows written before failures were classified are
    /// filed by their status code.
    pub error_class: String,
    /// The address the request came from; empty when unknown, on older rows,
    /// and once the client address retention has passed.
    pub client_ip: String,
    /// Two-letter country the edge named for the client, or empty.
    pub client_country: String,
    /// The client's User-Agent; empty when unknown or past retention.
    pub user_agent: String,
}

/// A value bound to a placeholder. The ClickHouse client binds positionally,
/// so a [`Filter`] keeps its values in placeholder order.
#[derive(Clone, Debug, PartialEq)]
enum Bind {
    Int(i64),
    Text(String),
}

/// The `where` clause of a ledger read and the values it binds, built
/// together so a placeholder and its value cannot drift apart. Every usage
/// read builds its filters here.
#[derive(Clone, Debug, Default)]
pub(crate) struct Filter {
    sql: String,
    binds: Vec<Bind>,
}

impl Filter {
    /// `where` the `usage` (or `key_rejections`) window `[since, until)`. The
    /// bounds are stated on `ts` too, the partition key, so ClickHouse skips
    /// whole days outside the window instead of checking each part's index.
    pub(crate) fn window(since: i64, until: Option<i64>) -> Self {
        let mut f = Filter {
            sql: " where ts >= fromUnixTimestamp64Milli(toInt64(?)) and ts_ms >= ?".into(),
            binds: vec![Bind::Int(since), Bind::Int(since)],
        };
        if let Some(until) = until {
            f.sql
                .push_str(" and ts < fromUnixTimestamp64Milli(toInt64(?)) and ts_ms < ?");
            f.binds.extend([Bind::Int(until), Bind::Int(until)]);
        }
        f
    }

    /// `where` an opening predicate of a table without a `ts` window.
    fn from(predicate: &str, binds: Vec<Bind>) -> Self {
        Filter {
            sql: format!(" where {predicate}"),
            binds,
        }
    }

    /// A constant predicate (`and ...`), with nothing to bind.
    fn and(&mut self, predicate: &str) -> &mut Self {
        self.sql.push_str(predicate);
        self
    }

    /// `and {predicate}` with its one value.
    fn and_bound(&mut self, predicate: &str, value: Bind) -> &mut Self {
        self.sql.push_str(" and ");
        self.sql.push_str(predicate);
        self.binds.push(value);
        self
    }

    /// `and {column} = toUUID(?)` when there is an id.
    fn uuid(&mut self, column: &str, id: Option<Uuid>) -> &mut Self {
        if let Some(id) = id {
            self.and_bound(&format!("{column} = toUUID(?)"), Bind::Text(id.to_string()));
        }
        self
    }

    /// `and {column} = ?` when there is a value.
    fn text(&mut self, column: &str, value: Option<&str>) -> &mut Self {
        if let Some(v) = value {
            self.and_bound(&format!("{column} = ?"), Bind::Text(v.to_string()));
        }
        self
    }

    /// `and {column} = …` for one id, `in (…)` for several, nothing for none.
    fn uuids(&mut self, column: &str, ids: &[Uuid]) -> &mut Self {
        let ids: Vec<String> = ids.iter().map(Uuid::to_string).collect();
        self.any_of(column, "toUUID(?)", &ids)
    }

    /// `and {column} = …` for one value, `and {column} in (…)` for several,
    /// nothing for none; `placeholder` is how each bound value is read.
    fn any_of(&mut self, column: &str, placeholder: &str, values: &[String]) -> &mut Self {
        match values {
            [] => {}
            [one] => {
                self.and_bound(
                    &format!("{column} = {placeholder}"),
                    Bind::Text(one.clone()),
                );
            }
            many => {
                let list = vec![placeholder; many.len()].join(", ");
                self.sql.push_str(&format!(" and {column} in ({list})"));
                self.binds.extend(many.iter().cloned().map(Bind::Text));
            }
        }
        self
    }

    /// Leave out internal traffic unless it is asked for.
    pub(crate) fn internal(&mut self, include_internal: Option<bool>) -> &mut Self {
        self.and(&internal_filter(include_internal))
    }

    pub(crate) fn sql(&self) -> &str {
        &self.sql
    }

    /// Bind this filter's values onto `query`, after any bound before it.
    pub(crate) fn bind(&self, mut query: clickhouse::query::Query) -> clickhouse::query::Query {
        for b in &self.binds {
            query = match b {
                Bind::Int(v) => query.bind(*v),
                Bind::Text(v) => query.bind(v.as_str()),
            };
        }
        query
    }
}

#[cfg(test)]
impl Filter {
    /// The text values bound, in order; the window's numbers left out.
    fn texts(&self) -> Vec<String> {
        self.binds
            .iter()
            .filter_map(|b| match b {
                Bind::Text(t) => Some(t.clone()),
                Bind::Int(_) => None,
            })
            .collect()
    }
}

/// A read's window `[since, until)`: `since` defaults to a day before
/// `until`, or before now when the window is open-ended.
pub(crate) fn window_start(since_ms: Option<i64>, until_ms: Option<i64>) -> i64 {
    since_ms.unwrap_or_else(|| until_ms.unwrap_or_else(now_ms) - 86_400_000)
}

/// Default rows of a grouped read, busiest first, when no `limit` is given.
const GROUPED_ROWS_DEFAULT: u64 = 1_000;

/// Most rows any grouped read returns.
pub const GROUPED_ROWS_MAX: u64 = 10_000;

/// A grouped read's row limit: `limit`, or `default`, within
/// 1..=[`GROUPED_ROWS_MAX`].
fn row_limit(limit: Option<u64>, default: u64) -> u64 {
    limit.unwrap_or(default).clamp(1, GROUPED_ROWS_MAX)
}

/// Seconds ClickHouse may spend on one Management API read before it gives
/// up. Set once on the admin read client, so a wide window or a slow plan
/// cannot hold a server thread; a read may ask for less.
pub const READ_MAX_EXECUTION_SECS: u64 = 30;

/// The `where` clause the request log, its histogram, facets and failure
/// groups share: every filter the log offers. The keyset cursor is the log's
/// alone.
fn log_filter_sql(q: &UsageLogQuery, since: i64) -> Filter {
    let mut f = Filter::window(since, q.until_ms);
    f.uuids("tenant_id", &q.tenant_id)
        .uuids("key_id", &q.key_id)
        .any_of("model", "?", &q.model)
        .text("request_type", q.request_type.as_deref())
        .text("session_id", q.session_id.as_deref())
        .text("device_id", q.device_id.as_deref())
        .internal(q.include_internal);
    match q.status.as_deref() {
        Some("success") => {
            f.and(" and status_code >= 200 and status_code < 400");
        }
        Some("error") => {
            f.and(" and status_code >= 400");
        }
        _ => {}
    }
    if let Some(code) = q.status_code {
        // A u16 prints as digits only, so it is safe to inline.
        f.and(&format!(" and status_code = {code}"));
    }
    let class = obleth_config::EFFECTIVE_CLASS_SQL;
    let platform = obleth_config::PLATFORM_CLASSES_SQL;
    match q.fault.as_deref() {
        Some("platform") => {
            f.and(&format!(" and {class} in {platform}"));
        }
        Some("client") => {
            f.and(&format!(
                " and status_code >= 400 and {class} not in {platform}"
            ));
        }
        _ => {}
    }
    f.any_of(class, "?", &q.error_class);
    if let Some(rid) = &q.request_id {
        f.and_bound(
            "startsWith(lower(toString(request_id)), lower(?))",
            Bind::Text(rid.clone()),
        );
    }
    if let Some(raw) = q.client_ip.as_deref() {
        client_ip_filter(&mut f, raw);
    }
    f.uuid("parent_request_id", q.parent_request_id);
    if q.traced_only == Some(true) {
        f.and(&traced_only_filter(since, q.until_ms));
    }
    f
}

/// `client_ip` matching one address or a range. Addresses are stored in
/// canonical text, so one address is an exact match; a range compares the
/// parsed address against the range's ends with the `OrNull` parsers, which
/// never throw on a row whose address is empty.
fn client_ip_filter(f: &mut Filter, raw: &str) {
    use ipnet::IpNet;
    let Some(net) = obleth_config::network::parse_network(raw) else {
        f.and(" and 0");
        return;
    };
    if net.prefix_len() == net.max_prefix_len() {
        f.and_bound("client_ip = ?", Bind::Text(net.addr().to_string()));
        return;
    }
    let parse = match net {
        IpNet::V4(_) => "toIPv4OrNull",
        IpNet::V6(_) => "toIPv6OrNull",
    };
    f.and_bound(
        &format!("{parse}(client_ip) between {parse}(?) and {parse}(?)"),
        Bind::Text(net.network().to_string()),
    );
    f.binds.push(Bind::Text(net.broadcast().to_string()));
}

/// The request log's columns, in [`UsageLogRow`] field order: rows are read
/// by position, so the two must move together. The class is read through
/// [`obleth_config::EFFECTIVE_CLASS_SQL`] under another name: an alias
/// `error_class` would shadow the column the expression reads.
fn usage_log_select() -> String {
    format!(
        "select request_id, ts_ms, tenant_id, key_id, model, request_type, session_id, session_id_source, device_id, \
         admission, status_code, input_tokens, output_tokens, \
         toUInt64(input_tokens) + toUInt64(output_tokens) as total_tokens, \
         cached_input_tokens, queue_wait_ms, ttft_ms, total_ms, cache_status, cost_usd, \
         energy_wh, energy_cost_usd, co2_g, parent_request_id, model_variant, \
         reasoning_effort, reasoning_effort_requested, model_family, \
         key_name as recorded_key_name, tenant_name as recorded_tenant_name, streamed, \
         error_reason, {} as filed_class, client_ip, client_country, user_agent \
         from usage",
        obleth_config::EFFECTIVE_CLASS_SQL
    )
}

/// Read individual `usage` rows newest-first, honoring the supplied filters and
/// keyset cursor. Two reads: the page's `(ts_ms, request_id)` pairs, which
/// reads only those columns (and the filters') across the window, then every
/// column of just those rows, within the time span they cover.
pub async fn query_usage_logs(
    client: &clickhouse::Client,
    q: UsageLogQuery,
) -> Result<LogPage, clickhouse::error::Error> {
    let since = window_start(q.since_ms, q.until_ms);
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let filter = log_filter_sql(&q, since);

    let mut page_sql = format!("select ts_ms, request_id from usage{}", filter.sql());
    // Keyset cursor: (ts_ms, request_id) tuple strictly less than the cursor.
    // Tuple comparison matches the `order by` below for stable paging.
    if q.before_ms.is_some() && q.before_request_id.is_some() {
        page_sql.push_str(" and (ts_ms, toString(request_id)) < (?, ?)");
    } else if q.before_ms.is_some() {
        page_sql.push_str(" and ts_ms < ?");
    }
    // One past the page, to tell whether more follow.
    page_sql.push_str(&format!(" {LOG_ORDER} limit {}", limit + 1));
    let mut page = filter.bind(client.query(&page_sql));
    if let (Some(before_ms), Some(before_id)) = (q.before_ms, q.before_request_id) {
        page = page.bind(before_ms).bind(before_id.to_string());
    } else if let Some(before_ms) = q.before_ms {
        page = page.bind(before_ms);
    }
    let mut page = page.fetch_all::<LogPageEntry>().await?;
    let more = page.len() > limit as usize;
    page.truncate(limit as usize);
    let Some(rows_sql) = usage_log_rows_sql(filter.sql(), &page) else {
        return Ok(LogPage::default());
    };
    let rows = filter
        .bind(client.query(&rows_sql))
        .fetch_all::<UsageLogRow>()
        .await?;
    Ok(LogPage { rows, more })
}

/// A page of the request log, and whether rows past it match too.
#[derive(Debug, Default)]
pub struct LogPage {
    pub rows: Vec<UsageLogRow>,
    pub more: bool,
}

/// Newest first. Ties on the millisecond order by the expression the keyset
/// cursor compares (`toString(request_id)`): ClickHouse's native UUID
/// ordering differs from its string ordering.
const LOG_ORDER: &str = "order by ts_ms desc, toString(request_id) desc";

#[derive(Debug, Row, Deserialize)]
struct LogPageEntry {
    ts_ms: i64,
    #[serde(with = "clickhouse::serde::uuid")]
    request_id: Uuid,
}

/// Every column of the rows on `page`: the log's filters again (so a row is
/// returned only if it matched), the page's time span, which bounds the
/// partitions and granules read, and its ids. `None` for an empty page.
/// Values are written into the SQL: an `i64` and a UUID print only digits,
/// hex and dashes.
fn usage_log_rows_sql(filter: &str, page: &[LogPageEntry]) -> Option<String> {
    let oldest = page.iter().map(|e| e.ts_ms).min()?;
    let newest = page.iter().map(|e| e.ts_ms).max()?;
    let ids = page
        .iter()
        .map(|e| format!("toUUID('{}')", e.request_id))
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!(
        "{}{filter} \
         and ts between fromUnixTimestamp64Milli(toInt64({oldest})) and fromUnixTimestamp64Milli(toInt64({newest})) \
         and ts_ms between {oldest} and {newest} \
         and request_id in ({ids}) \
         {LOG_ORDER} limit {}",
        usage_log_select(),
        page.len()
    ))
}

/// The request log's histogram: the bucket width used, and each non-empty bucket.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UsageLogHistogram {
    pub bucket_ms: i64,
    pub buckets: Vec<UsageLogBucket>,
}

/// One bucket of the request log's histogram.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageLogBucket {
    /// Bucket start, unix epoch millis (epoch-aligned).
    pub bucket_ms: i64,
    pub requests: u64,
    /// Requests in the bucket that ended with an HTTP status of 400 or above.
    pub errors: u64,
    /// The part of `errors` that were the platform's failures, not the
    /// caller's (see `obleth_config::ErrorClass::is_platform`).
    pub platform_errors: u64,
}

/// Most buckets a histogram returns: a strip a few hundred pixels wide needs
/// no more.
const MAX_LOG_HISTOGRAM_BUCKETS: i64 = 400;

/// The requested width (default a minute, at least a second), widened until
/// `[since, until]` fits in [`MAX_LOG_HISTOGRAM_BUCKETS`].
fn log_histogram_bucket_ms(since: i64, until: i64, requested: Option<i64>) -> i64 {
    let span = until.saturating_sub(since).max(0);
    let widths = MAX_LOG_HISTOGRAM_BUCKETS - 1;
    let min_for_cap = span / widths + i64::from(span % widths != 0);
    requested.unwrap_or(60_000).max(1_000).max(min_for_cap)
}

fn usage_log_histogram_sql(bucket: i64, filter: &str) -> String {
    format!(
        "select intDiv(ts_ms, {bucket}) * {bucket} as bucket_ms, \
         count() as requests, \
         countIf(status_code >= 400) as errors, \
         countIf({class} in {platform}) as platform_errors \
         from usage{filter} group by bucket_ms order by bucket_ms",
        class = obleth_config::EFFECTIVE_CLASS_SQL,
        platform = obleth_config::PLATFORM_CLASSES_SQL,
    )
}

/// Requests and failures per bucket over the log's window, with the log's
/// filters. Empty buckets are left out; the caller fills the gaps.
pub async fn query_usage_log_histogram(
    client: &clickhouse::Client,
    q: UsageLogQuery,
) -> Result<(i64, Vec<UsageLogBucket>), clickhouse::error::Error> {
    let since = window_start(q.since_ms, q.until_ms);
    let until = q.until_ms.unwrap_or_else(now_ms);
    let bucket = log_histogram_bucket_ms(since, until, q.bucket_ms);
    let filter = log_filter_sql(&q, since);
    let sql = usage_log_histogram_sql(bucket, filter.sql());
    let rows = filter
        .bind(client.query(&sql))
        .fetch_all::<UsageLogBucket>()
        .await?;
    Ok((bucket, rows))
}

/// Most values a facet lists.
const LOG_FACET_LIMIT: u64 = 12;

/// One value of a facet. `label` is the tenant's or key's name, resolved by
/// the handler; the other facets leave it empty.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UsageLogFacet {
    pub value: String,
    pub label: String,
    pub requests: u64,
    pub errors: u64,
}

/// A status code and model that failed together, the most common first.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageLogFailure {
    pub status_code: u16,
    pub model: String,
    pub requests: u64,
}

/// `GET /usage/logs/facets`: what the requests matching the log's filters
/// are made of, each dimension's busiest values first.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UsageLogFacets {
    pub status_codes: Vec<UsageLogFacet>,
    pub models: Vec<UsageLogFacet>,
    pub tenants: Vec<UsageLogFacet>,
    pub keys: Vec<UsageLogFacet>,
    pub request_types: Vec<UsageLogFacet>,
    /// Failure classes (`obleth_config::ErrorClass`); successes come back
    /// under the empty value.
    pub error_classes: Vec<UsageLogFacet>,
    pub failures: Vec<UsageLogFailure>,
}

/// The dimensions a facet can count, and the expression that reads each.
pub const LOG_FACET_DIMENSIONS: &[(&str, &str)] = &[
    ("status_codes", "toString(status_code)"),
    ("models", "model"),
    ("tenants", "toString(tenant_id)"),
    ("keys", "toString(key_id)"),
    ("request_types", "request_type"),
    ("error_classes", obleth_config::EFFECTIVE_CLASS_SQL),
];

/// The `grouping(...)` mask of a facet row: one bit per grouping column,
/// the first column highest, set when the row is NOT grouped by it.
const FACET_GROUPING_COLUMNS: u32 = LOG_FACET_DIMENSIONS.len() as u32 + 2;

fn facet_mask(grouped: &[u32]) -> u64 {
    let all = (1u64 << FACET_GROUPING_COLUMNS) - 1;
    grouped
        .iter()
        .fold(all, |m, i| m & !(1 << (FACET_GROUPING_COLUMNS - 1 - i)))
}

/// Every facet and the most common failures in one scan: a grouping set per
/// dimension (`d0`..), and one of failed requests by status and model
/// (`fail_status`, `fail_model`; zero and empty on successes, which are
/// dropped). Each set keeps its [`LOG_FACET_LIMIT`] busiest rows.
fn usage_log_facets_sql(filter: &str) -> String {
    let dims = LOG_FACET_DIMENSIONS.len() as u32;
    let columns: Vec<String> = LOG_FACET_DIMENSIONS
        .iter()
        .enumerate()
        .map(|(i, (_, expr))| format!("{expr} as d{i}"))
        .collect();
    let names: Vec<String> = (0..dims).map(|i| format!("d{i}")).collect();
    let sets: Vec<String> = names.iter().map(|d| format!("({d})")).collect();
    format!(
        "select grouping({names}, fail_status, fail_model) as g, {columns}, \
         if(status_code >= 400, status_code, 0) as fail_status, \
         if(status_code >= 400, model, '') as fail_model, \
         count() as requests, countIf(status_code >= 400) as errors \
         from usage{filter} \
         group by grouping sets ({sets}, (fail_status, fail_model)) \
         having g != {failures} or fail_status != 0 \
         order by g, requests desc, {names}, fail_model, fail_status \
         limit {LOG_FACET_LIMIT} by g",
        names = names.join(", "),
        columns = columns.join(", "),
        sets = sets.join(", "),
        failures = facet_mask(&[dims, dims + 1]),
    )
}

/// A row of [`usage_log_facets_sql`]. Read positionally: `d0`..`d5` follow
/// [`LOG_FACET_DIMENSIONS`].
#[derive(Debug, Row, Deserialize)]
struct UsageLogFacetsRow {
    g: u64,
    d: [String; 6],
    fail_status: u16,
    fail_model: String,
    requests: u64,
    errors: u64,
}

/// Each facet of the requests matching the log's filters, and the most common
/// failures, from one scan. Labels are left to the handler.
pub async fn query_usage_log_facets(
    client: &clickhouse::Client,
    q: UsageLogQuery,
) -> Result<UsageLogFacets, clickhouse::error::Error> {
    let since = window_start(q.since_ms, q.until_ms);
    let filter = log_filter_sql(&q, since);
    let rows = filter
        .bind(client.query(&usage_log_facets_sql(filter.sql())))
        .fetch_all::<UsageLogFacetsRow>()
        .await?;
    Ok(facets_from_rows(rows))
}

fn facets_from_rows(rows: Vec<UsageLogFacetsRow>) -> UsageLogFacets {
    let dims = LOG_FACET_DIMENSIONS.len() as u32;
    let mut facets: [Vec<UsageLogFacet>; 6] = Default::default();
    let mut failures = Vec::new();
    let failures_mask = facet_mask(&[dims, dims + 1]);
    for mut row in rows {
        if row.g == failures_mask {
            failures.push(UsageLogFailure {
                status_code: row.fail_status,
                model: row.fail_model,
                requests: row.requests,
            });
        } else if let Some(i) = (0..dims).find(|&i| row.g == facet_mask(&[i])) {
            facets[i as usize].push(UsageLogFacet {
                value: std::mem::take(&mut row.d[i as usize]),
                label: String::new(),
                requests: row.requests,
                errors: row.errors,
            });
        }
    }
    let [status_codes, models, tenants, keys, request_types, error_classes] = facets;
    UsageLogFacets {
        status_codes,
        models,
        tenants,
        keys,
        request_types,
        error_classes,
        failures,
    }
}

/// Most groups [`query_usage_failure_groups`] returns.
const FAILURE_GROUP_LIMIT: u64 = 50;

/// Failures that are the same failure over and over: one caller, one model,
/// one status, one class, and one reason once its numbers are set aside (so
/// "prompt contains at least 196609 input tokens" and "... 204864 ..." group
/// together).
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageFailureGroup {
    pub error_class: String,
    /// Whether the class is the platform's failure rather than the caller's.
    pub platform: bool,
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub tenant_name: String,
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub key_id: Uuid,
    pub key_name: String,
    pub model: String,
    pub status_code: u16,
    /// One of the group's recorded reasons, numbers and all; empty for rows
    /// written before reasons were recorded.
    pub reason: String,
    pub requests: u64,
    /// The group's first and last failure, unix epoch millis.
    pub first_ms: i64,
    pub last_ms: i64,
}

/// `GET /usage/failures`: the failures among the requests matching the
/// log's filters, split by whose they were, and grouped into repeats.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct UsageFailureGroups {
    /// Every request matching the filters, failed or not.
    pub requests: u64,
    pub failures: u64,
    /// The part of `failures` that were the platform's.
    pub platform_failures: u64,
    /// The largest groups first, at most 50.
    pub groups: Vec<UsageFailureGroup>,
}

#[derive(Debug, Clone, Row, Deserialize)]
struct FailureCounts {
    requests: u64,
    failures: u64,
    platform_failures: u64,
}

fn failure_counts_sql(filter: &str) -> String {
    format!(
        "select count() as requests, \
         countIf(status_code >= 400) as failures, \
         countIf({class} in {platform}) as platform_failures \
         from usage{filter}",
        class = obleth_config::EFFECTIVE_CLASS_SQL,
        platform = obleth_config::PLATFORM_CLASSES_SQL,
    )
}

/// The failures matching `filter` (a `where` clause) grouped into repeats of
/// at least `min_failures`, largest first, at most `limit` groups. Read by the
/// request log and by Advice. Aliases stay clear of the columns the query
/// reads; see [`usage_totals_sql`].
pub(crate) fn failure_groups_sql(filter: &str, min_failures: u64, limit: u64) -> String {
    format!(
        "select {class} as filed_class, filed_class in {platform} as is_platform, \
         tenant_id, any(tenant_name) as recorded_tenant, key_id, any(key_name) as recorded_key, \
         model, status_code, any(error_reason) as sample_reason, \
         count() as failed, min(ts_ms) as first_at, max(ts_ms) as last_at \
         from usage{filter} and status_code >= 400 \
         group by filed_class, tenant_id, key_id, model, status_code, \
         replaceRegexpAll(error_reason, '[0-9]+', 'N') \
         having failed >= {min_failures} \
         order by failed desc, last_at desc limit {limit}",
        class = obleth_config::EFFECTIVE_CLASS_SQL,
        platform = obleth_config::PLATFORM_CLASSES_SQL,
    )
}

/// The failures among the requests matching the log's filters: how many
/// there were, how many were the platform's, and the largest repeats.
pub async fn query_usage_failure_groups(
    client: &clickhouse::Client,
    q: UsageLogQuery,
) -> Result<UsageFailureGroups, clickhouse::error::Error> {
    let since = window_start(q.since_ms, q.until_ms);
    let filter = log_filter_sql(&q, since);
    let counts_sql = failure_counts_sql(filter.sql());
    let groups_sql = failure_groups_sql(filter.sql(), 1, FAILURE_GROUP_LIMIT);
    let (counts, groups) = tokio::try_join!(
        filter
            .bind(client.query(&counts_sql))
            .fetch_one::<FailureCounts>(),
        filter
            .bind(client.query(&groups_sql))
            .fetch_all::<UsageFailureGroup>(),
    )?;
    Ok(UsageFailureGroups {
        requests: counts.requests,
        failures: counts.failures,
        platform_failures: counts.platform_failures,
        groups,
    })
}

/// `traced_only` subquery. It takes the outer query's resolved `since` so the
/// span scan is bounded by the same window as the log read rather than the
/// whole retained `spans` table.
fn traced_only_filter(since: i64, until_ms: Option<i64>) -> String {
    let until = until_ms.unwrap_or(i64::MAX);
    // Safety: `since` and `until` are `i64` — `Display` emits only ASCII
    // digits (and an optional leading `-`), so there is no SQL-injection
    // surface. The clickhouse crate (v0.13) scopes bind parameters to the
    // top-level query string and does not propagate them into subqueries,
    // making `format!` the correct approach for subquery literals.
    format!(
        " AND request_id IN (SELECT DISTINCT request_id FROM spans \
          WHERE start_ms >= {since} AND start_ms < {until})"
    )
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
    /// Average time to first token in ms across the grouped rows, over
    /// answered requests that streamed a first token, cache hits left out.
    pub avg_ttft_ms: f64,
    /// Average end-to-end latency in ms across the grouped rows.
    pub avg_total_ms: f64,
    /// Total USD spend across the grouped rows, summed from the per-request
    /// cost frozen at completion time (never recomputed from current prices).
    pub cost_usd: f64,
    /// Total energy consumed across the grouped rows in watt-hours.
    pub energy_wh: f64,
    /// Estimated energy cost in USD for the grouped rows.
    pub energy_cost_usd: f64,
    /// Total CO₂ equivalent in grams for the grouped rows.
    pub co2_g: f64,
    /// The part of `error_requests` that were the platform's failures, not
    /// the caller's.
    pub platform_error_requests: u64,
    /// The requests `avg_ttft_ms` averages over, so averages of several rows
    /// can be weighted by it. Days rolled up before v2.13 did not count them
    /// and report their answered requests instead.
    pub ttft_requests: u64,
}

/// Whole-gateway usage totals for the overview strip: one aggregate row over
/// the window instead of per-tenant/per-model groupings.
#[derive(Debug, Clone, Default, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageTotals {
    pub requests: u64,
    pub total_tokens: u64,
    /// Sum of each request's frozen `cost_usd` — never recomputed from
    /// current prices.
    pub cost_usd: f64,
    /// Distinct tenants with at least one request in the window.
    pub active_tenants: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Requests that ended with an HTTP status of 400 or above.
    pub errors: u64,
    /// Median time to first token over answered requests that streamed a first token, cache hits left out; 0 when there were none.
    pub p50_ttft_ms: f64,
    pub avg_ttft_ms: f64,
    pub energy_wh: f64,
    pub energy_cost_usd: f64,
    pub co2_g: f64,
    /// The part of `errors` that were the platform's failures, not the
    /// caller's (see `obleth_config::ErrorClass::is_platform`).
    pub platform_errors: u64,
}

/// Columns are read positionally into [`UsageTotals`], so aliases need not
/// match field names, and must not: ClickHouse resolves an alias anywhere in
/// the query, so `sum(input_tokens) as input_tokens` would turn the
/// `total_tokens` expression into an aggregate of an aggregate and fail the
/// whole read (which the summary then reports as zeros).
fn usage_totals_sql(filter: &str) -> String {
    format!("select {} from usage{filter}", usage_totals_columns())
}

/// The columns of [`UsageTotals`], in field order.
fn usage_totals_columns() -> String {
    let (avg_ttft, p50_ttft) = (avg_ttft_sql(), p50_ttft_sql());
    format!(
        "count() as requests, \
         sum(input_tokens) + sum(output_tokens) as total_tokens, \
         sum(cost_usd) as cost_usd, \
         uniqExact(tenant_id) as active_tenants, \
         sum(input_tokens) as in_tok, sum(output_tokens) as out_tok, \
         countIf(status_code >= 400) as errors, \
         {p50_ttft} as p50_ttft_ms, \
         {avg_ttft} as avg_ttft_ms, \
         sum(energy_wh) as energy_wh, sum(energy_cost_usd) as energy_cost_usd, sum(co2_g) as co2_g, \
         countIf({class} in {platform}) as platform_errors",
        class = obleth_config::EFFECTIVE_CLASS_SQL,
        platform = obleth_config::PLATFORM_CLASSES_SQL,
    )
}

pub async fn query_usage_totals(
    client: &clickhouse::Client,
    q: &UsageQuery,
) -> Result<UsageTotals, clickhouse::error::Error> {
    let filter = usage_filter(q);
    filter
        .bind(client.query(&usage_totals_sql(filter.sql())))
        .fetch_one::<UsageTotals>()
        .await
}

/// Query of `GET /overview`: one window and the one before it.
#[derive(Debug, Default, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct OverviewQuery {
    /// Inclusive start of the window, unix epoch millis. Defaults to a day
    /// before `until_ms` (or now).
    pub since_ms: Option<i64>,
    /// Exclusive end of the window, unix epoch millis. Open-ended (now) by
    /// default.
    pub until_ms: Option<i64>,
    /// Width of the series buckets, which span both windows: default 5
    /// minutes, at least 1 minute, widened so the series fits in
    /// 5,000 buckets.
    pub bucket_ms: Option<i64>,
    /// When true, include internal traffic (health probes, benchmark runs).
    pub include_internal: Option<bool>,
}

/// Usage of one tenant in the Overview window.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct OverviewTenantUsage {
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    /// The tenant's name; `null` once the tenant is deleted.
    pub name: Option<String>,
    pub requests: u64,
    pub total_tokens: u64,
    /// Sum of each request's frozen `cost_usd`.
    pub cost_usd: f64,
}

/// What the Overview reads from the usage ledger, from two scans: totals of
/// the window and of the one before it, cache counts and the series over
/// both; then per-model and per-tenant rows of the window.
#[derive(Debug, Clone, Default)]
pub struct OverviewUsage {
    pub since_ms: i64,
    pub until_ms: i64,
    pub previous_since_ms: i64,
    pub bucket_ms: i64,
    pub current: UsageTotals,
    pub previous: UsageTotals,
    pub cache: CacheStats,
    pub series: Vec<UsageTimePoint>,
    /// Busiest (most tokens) first.
    pub models: Vec<UsageModelAgg>,
    /// Busiest (most requests) first, without names.
    pub tenants: Vec<(Uuid, u64, u64, f64)>,
}

/// A row of the first Overview scan: a period's totals (`by_bucket` 0,
/// `period` 1 for the previous window and 2 for this one) or a series bucket
/// (`by_bucket` 1). Read positionally; see [`overview_periods_sql`].
#[derive(Debug, Row, Deserialize)]
struct OverviewPeriodRow {
    by_bucket: u64,
    period: u8,
    bucket_ms: i64,
    requests: u64,
    total_tokens: u64,
    cost_usd: f64,
    active_tenants: u64,
    input_tokens: u64,
    output_tokens: u64,
    errors: u64,
    p50_ttft_ms: f64,
    avg_ttft_ms: f64,
    energy_wh: f64,
    energy_cost_usd: f64,
    co2_g: f64,
    platform_errors: u64,
    hits: u64,
    misses: u64,
    tokens_saved: u64,
}

impl OverviewPeriodRow {
    fn totals(&self) -> UsageTotals {
        UsageTotals {
            requests: self.requests,
            total_tokens: self.total_tokens,
            cost_usd: self.cost_usd,
            active_tenants: self.active_tenants,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            errors: self.errors,
            p50_ttft_ms: self.p50_ttft_ms,
            avg_ttft_ms: self.avg_ttft_ms,
            energy_wh: self.energy_wh,
            energy_cost_usd: self.energy_cost_usd,
            co2_g: self.co2_g,
            platform_errors: self.platform_errors,
        }
    }

    fn point(&self) -> UsageTimePoint {
        UsageTimePoint {
            bucket_ms: self.bucket_ms,
            requests: self.requests,
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            total_tokens: self.total_tokens,
            energy_wh: self.energy_wh,
            errors: self.errors,
            cost_usd: self.cost_usd,
            p50_ttft_ms: self.p50_ttft_ms,
            avg_ttft_ms: self.avg_ttft_ms,
            platform_errors: self.platform_errors,
        }
    }
}

/// A row of the second Overview scan: a model's aggregate (`by_tenant` 0)
/// or a tenant's (`by_tenant` 1, where only the counts and cost are read).
#[derive(Debug, Row, Deserialize)]
struct OverviewBreakdownRow {
    by_tenant: u64,
    model: String,
    #[serde(with = "clickhouse::serde::uuid")]
    tenant_id: Uuid,
    requests: u64,
    input_tokens: u64,
    output_tokens: u64,
    total_tokens: u64,
    gen_tokens_per_sec: f64,
    agg_tokens_per_sec: f64,
    avg_ttft_ms: f64,
    avg_total_ms: f64,
    p50_ttft_ms: f64,
    p50_total_ms: f64,
    avg_prompt_tokens: f64,
    avg_gen_tokens: f64,
    users: u64,
    energy_wh: f64,
    energy_cost_usd: f64,
    co2_g: f64,
    cost_usd: f64,
}

/// Totals of each window, cache counts and the series, in one scan of
/// `[previous_since, until)`: `period` splits the windows at `since`, and
/// grouping sets give one row per period and one per bucket.
fn overview_periods_sql(since: i64, bucket: i64, filter: &str) -> String {
    format!(
        "select grouping(period) as by_bucket, \
         if(ts_ms >= {since}, 2, 1) as period, \
         intDiv(ts_ms, {bucket}) * {bucket} as bucket_ms, \
         {}, \
         countIf(cache_status = 'hit') as hits, \
         countIf(cache_status = 'miss') as misses, \
         sumIf(input_tokens + output_tokens, cache_status = 'hit') as tokens_saved \
         from usage{filter} \
         group by grouping sets ((period), (bucket_ms)) \
         order by by_bucket, period, bucket_ms",
        usage_totals_columns(),
    )
}

/// Per-model and per-tenant rows of the window in one scan.
fn overview_breakdown_sql(filter: &str) -> String {
    format!(
        "select grouping(model) as by_tenant, model, tenant_id, {}, \
         sum(cost_usd) as cost \
         from usage{filter} \
         group by grouping sets ((model), (tenant_id))",
        usage_model_columns(),
    )
}

/// Everything the Overview reads from the ledger for `q`, in two scans run
/// together. The previous window is as long as this one and ends where it
/// starts.
pub async fn query_overview(
    client: &clickhouse::Client,
    q: &OverviewQuery,
) -> Result<OverviewUsage, clickhouse::error::Error> {
    let since = window_start(q.since_ms, q.until_ms);
    let until = q.until_ms.unwrap_or_else(now_ms);
    let previous_since = since - until.saturating_sub(since).max(0);
    let bucket = series_bucket_ms(previous_since, q.bucket_ms, 60_000, until);
    let both = UsageQuery {
        since_ms: Some(previous_since),
        until_ms: q.until_ms,
        include_internal: q.include_internal,
        ..Default::default()
    };
    let window = UsageQuery {
        since_ms: Some(since),
        until_ms: q.until_ms,
        include_internal: q.include_internal,
        ..Default::default()
    };
    let (both, window) = (usage_filter(&both), usage_filter(&window));
    let (periods, breakdown) = tokio::try_join!(
        both.bind(client.query(&overview_periods_sql(since, bucket, both.sql())))
            .fetch_all::<OverviewPeriodRow>(),
        window
            .bind(client.query(&overview_breakdown_sql(window.sql())))
            .fetch_all::<OverviewBreakdownRow>(),
    )?;
    Ok(assemble_overview(
        since,
        until,
        previous_since,
        bucket,
        periods,
        breakdown,
    ))
}

fn assemble_overview(
    since: i64,
    until: i64,
    previous_since: i64,
    bucket: i64,
    periods: Vec<OverviewPeriodRow>,
    breakdown: Vec<OverviewBreakdownRow>,
) -> OverviewUsage {
    let mut out = OverviewUsage {
        since_ms: since,
        until_ms: until,
        previous_since_ms: previous_since,
        bucket_ms: bucket,
        ..Default::default()
    };
    for row in periods {
        match (row.by_bucket, row.period) {
            (0, 2) => {
                out.current = row.totals();
                out.cache = CacheStats {
                    hits: row.hits,
                    misses: row.misses,
                    tokens_saved: row.tokens_saved,
                };
            }
            (0, _) => out.previous = row.totals(),
            _ => out.series.push(row.point()),
        }
    }
    for row in breakdown {
        if row.by_tenant == 1 {
            out.tenants
                .push((row.tenant_id, row.requests, row.total_tokens, row.cost_usd));
        } else {
            out.models.push(UsageModelAgg {
                model: row.model,
                requests: row.requests,
                input_tokens: row.input_tokens,
                output_tokens: row.output_tokens,
                total_tokens: row.total_tokens,
                gen_tokens_per_sec: row.gen_tokens_per_sec,
                agg_tokens_per_sec: row.agg_tokens_per_sec,
                avg_ttft_ms: row.avg_ttft_ms,
                avg_total_ms: row.avg_total_ms,
                p50_ttft_ms: row.p50_ttft_ms,
                p50_total_ms: row.p50_total_ms,
                avg_prompt_tokens: row.avg_prompt_tokens,
                avg_gen_tokens: row.avg_gen_tokens,
                users: row.users,
                energy_wh: row.energy_wh,
                energy_cost_usd: row.energy_cost_usd,
                co2_g: row.co2_g,
            });
        }
    }
    out.models.sort_by(|a, b| {
        b.total_tokens
            .cmp(&a.total_tokens)
            .then_with(|| a.model.cmp(&b.model))
    });
    out.models.truncate(GROUPED_ROWS_DEFAULT as usize);
    out.tenants
        .sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out.tenants.truncate(GROUPED_ROWS_MAX as usize);
    out
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
    pub energy_wh: f64,
    pub energy_cost_usd: f64,
    pub co2_g: f64,
    /// Sum of each request's frozen `cost_usd`.
    pub cost_usd: f64,
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
    pub energy_wh: f64,
    pub energy_cost_usd: f64,
    pub co2_g: f64,
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
    /// Average time to first token in milliseconds, over answered requests that streamed a first token, cache hits left out.
    pub avg_ttft_ms: f64,
    /// Average end-to-end latency in milliseconds.
    pub avg_total_ms: f64,
    /// Median (p50) time to first token in milliseconds, over answered requests that streamed a first token, cache hits left out.
    pub p50_ttft_ms: f64,
    /// Median (p50) end-to-end latency in milliseconds.
    pub p50_total_ms: f64,
    /// Average prompt (input) tokens per request.
    pub avg_prompt_tokens: f64,
    /// Average generated (output) tokens per request.
    pub avg_gen_tokens: f64,
    /// Distinct API keys (users) that hit this model in the window.
    pub users: u64,
    pub energy_wh: f64,
    pub energy_cost_usd: f64,
    pub co2_g: f64,
}

/// Time-bucketed usage for charts.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct UsageTimePoint {
    pub bucket_ms: i64,
    pub requests: u64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub total_tokens: u64,
    pub energy_wh: f64,
    /// Requests in the bucket that ended with an HTTP status of 400 or above.
    pub errors: u64,
    /// Sum of each request's frozen `cost_usd`.
    pub cost_usd: f64,
    /// Median time to first token in the bucket, over answered requests that streamed a first token, cache hits left out; 0 when there
    /// were none.
    pub p50_ttft_ms: f64,
    pub avg_ttft_ms: f64,
    /// The part of `errors` that were the platform's failures, not the
    /// caller's.
    pub platform_errors: u64,
}

#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct TenantUsageTimePoint {
    #[serde(with = "clickhouse::serde::uuid")]
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub bucket_ms: i64,
    pub requests: u64,
    pub total_tokens: u64,
    pub energy_wh: f64,
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
    /// Average time to first token in milliseconds over the bucket, over
    /// answered requests that streamed a first token, cache hits left out.
    pub avg_ttft_ms: f64,
    /// Median (p50) time to first token in milliseconds over the bucket, over
    /// answered requests that streamed a first token, cache hits left out.
    pub p50_ttft_ms: f64,
    /// Average end-to-end latency in milliseconds over the bucket.
    pub avg_total_ms: f64,
    /// Median (p50) end-to-end latency in milliseconds over the bucket.
    pub p50_total_ms: f64,
    pub energy_wh: f64,
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
    pub energy_wh: f64,
    pub energy_cost_usd: f64,
    pub co2_g: f64,
}

/// Response-cache effectiveness over the window.
#[derive(Debug, Clone, Default, Row, Serialize, Deserialize, ToSchema)]
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

/// Usage per tenant, busiest first.
pub async fn query_usage(
    client: &clickhouse::Client,
    q: UsageQuery,
) -> Result<Vec<UsageAgg>, clickhouse::error::Error> {
    let filter = usage_filter(&q);
    let sql = format!(
        "select tenant_id, count() as requests, \
         sum(input_tokens) as in_tok, sum(output_tokens) as out_tok, \
         sum(input_tokens) + sum(output_tokens) as total_tok, \
         sum(energy_wh) as energy_wh, sum(energy_cost_usd) as energy_cost_usd, sum(co2_g) as co2_g, \
         sum(cost_usd) as cost_usd \
         from usage{} group by tenant_id order by total_tok desc limit {}",
        filter.sql(),
        row_limit(q.limit, GROUPED_ROWS_DEFAULT),
    );
    filter
        .bind(client.query(&sql))
        .fetch_all::<UsageAgg>()
        .await
}

pub async fn query_usage_by_key(
    client: &clickhouse::Client,
    q: UsageQuery,
) -> Result<Vec<UsageKeyAgg>, clickhouse::error::Error> {
    let filter = usage_filter(&q);
    filter
        .bind(client.query(&usage_by_key_sql(&q, filter.sql())))
        .fetch_all::<UsageKeyAgg>()
        .await
}

fn usage_by_key_sql(q: &UsageQuery, filter: &str) -> String {
    format!(
        "select key_id, tenant_id, count() as requests, \
         sum(input_tokens) as in_tok, sum(output_tokens) as out_tok, \
         sum(input_tokens) + sum(output_tokens) as total_tok, \
         sum(energy_wh) as energy_wh, sum(energy_cost_usd) as energy_cost_usd, sum(co2_g) as co2_g \
         from usage{filter} group by key_id, tenant_id order by total_tok desc limit {}",
        row_limit(q.limit, GROUPED_ROWS_DEFAULT),
    )
}

/// Filters for the per-key usage summary feeds (`/keys/{id}/usage` and
/// `/usage/keys/summary`).
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
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
    /// Exclusive upper bound of the bulk feed's window, unix epoch millis.
    /// Open-ended by default; the single-key endpoint ignores it like
    /// `tenant_id`.
    pub until_ms: Option<i64>,
    /// Most rows of the bulk feed, busiest keys first: default 1,000, at
    /// most 10,000.
    pub limit: Option<u64>,
    /// Only these keys in the bulk feed: one UUID or up to 200 joined by
    /// commas (a page of the Keys list). Ignored by the single-key endpoint.
    #[serde(default, deserialize_with = "key_id_list")]
    #[schema(value_type = Option<String>)]
    #[param(value_type = Option<String>)]
    pub key_id: Vec<Uuid>,
    /// When true, include internal traffic (health probes, benchmark runs)
    /// that is excluded from this read by default.
    pub include_internal: Option<bool>,
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
    pub e_wh: f64,
    pub e_cost_usd: f64,
    pub e_co2_g: f64,
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
            energy_wh: r.e_wh,
            energy_cost_usd: r.e_cost_usd,
            co2_g: r.e_co2_g,
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
    /// Energy consumed in watt-hours in the window.
    pub energy_wh: f64,
    /// Estimated energy cost in USD in the window.
    pub energy_cost_usd: f64,
    /// CO₂ equivalent in grams in the window.
    pub co2_g: f64,
}

/// Summary for one key. `last_used_ms` / `last_model` / `last_status_code` are
/// computed over the full retained ledger for that key, while the
/// request/token/cost columns are limited to the rolling window. Returns `None`
/// when the key has never appeared in the ledger.
pub async fn query_key_usage_summary(
    client: &clickhouse::Client,
    tenant_id: Uuid,
    key_id: Uuid,
    since_ms: Option<i64>,
    include_internal: Option<bool>,
) -> Result<Option<KeyUsageSummary>, clickhouse::error::Error> {
    let since = since_ms.unwrap_or_else(|| now_ms() - 86_400_000);
    let sql = key_usage_summary_sql(include_internal);
    let rows = client
        .query(&sql)
        .bind(since)
        .bind(since)
        .bind(since)
        .bind(since)
        .bind(since)
        .bind(since)
        .bind(since)
        .bind(since)
        .bind(tenant_id.to_string())
        .bind(key_id.to_string())
        .fetch_all::<KeyUsageSummaryRow>()
        .await?;
    Ok(rows.into_iter().next().map(KeyUsageSummary::from))
}

/// The ledger's sort key is `(tenant_id, ts_ms)`, so the tenant predicate is
/// what lets ClickHouse skip granules; `key_id` alone scans every retained row.
/// The raw column is qualified so the `tenant_id` SELECT alias cannot shadow it.
fn key_usage_summary_sql(include_internal: Option<bool>) -> String {
    format!(
        "select \
         key_id, \
         any(tenant_id) as tenant_id, \
         max(ts_ms) as last_used_ms, \
         argMax(model, ts_ms) as last_model, \
         argMax(status_code, ts_ms) as last_status_code, \
         countIf(ts_ms >= ?) as requests, \
         sumIf(input_tokens, ts_ms >= ?) as in_tok, \
         sumIf(output_tokens, ts_ms >= ?) as out_tok, \
         sumIf(toUInt64(input_tokens) + toUInt64(output_tokens), ts_ms >= ?) as total_tok, \
         sumIf(cost_usd, ts_ms >= ?) as cost_sum, \
         sumIf(energy_wh, ts_ms >= ?) as e_wh, \
         sumIf(energy_cost_usd, ts_ms >= ?) as e_cost_usd, \
         sumIf(co2_g, ts_ms >= ?) as e_co2_g \
         from usage where usage.tenant_id = toUUID(?) and key_id = toUUID(?){} \
         group by key_id",
        internal_filter(include_internal)
    )
}

/// Bulk per-key summary for the dashboard. The window (`since_ms`) bounds the
/// whole scan, so `last_used_ms` here is the last use *within the window* and
/// only keys with activity in the window are returned. Ordered by token volume.
pub async fn query_keys_usage_summary(
    client: &clickhouse::Client,
    q: KeyUsageSummaryQuery,
) -> Result<Vec<KeyUsageSummary>, clickhouse::error::Error> {
    let limit = row_limit(q.limit, GROUPED_ROWS_DEFAULT);
    let mut filter = Filter::window(window_start(q.since_ms, q.until_ms), q.until_ms);
    // Qualified so the SELECT alias `tenant_id` cannot shadow the column.
    filter
        .uuid("usage.tenant_id", q.tenant_id)
        .uuids("usage.key_id", &q.key_id)
        .internal(q.include_internal);
    let sql = format!(
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
         sum(cost_usd) as cost_sum, \
         sum(energy_wh) as e_wh, \
         sum(energy_cost_usd) as e_cost_usd, \
         sum(co2_g) as e_co2_g \
         from usage{} group by key_id order by total_tok desc limit {limit}",
        filter.sql()
    );
    let rows = filter
        .bind(client.query(&sql))
        .fetch_all::<KeyUsageSummaryRow>()
        .await?;
    Ok(rows.into_iter().map(KeyUsageSummary::from).collect())
}

/// What `GET /keys` can rank keys by: their use in a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyRankMetric {
    /// The last request's time.
    LastUsed,
    Requests,
    /// Input and output tokens.
    Tokens,
    /// Spend, in billionths of a dollar so sums are exact and the same on
    /// every read (a float sum's last bits depend on the order ClickHouse
    /// adds in, and a page cursor compares them exactly).
    Cost,
}

impl KeyRankMetric {
    fn sql(self) -> &'static str {
        match self {
            KeyRankMetric::LastUsed => "max(ts_ms)",
            KeyRankMetric::Requests => "toInt64(count())",
            KeyRankMetric::Tokens => {
                "toInt64(sum(toUInt64(input_tokens) + toUInt64(output_tokens)))"
            }
            KeyRankMetric::Cost => "sum(toInt64(round(cost_usd * 1000000000)))",
        }
    }
}

/// One key's place in a ranking by [`KeyRankMetric`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Row, Deserialize)]
pub struct KeyRank {
    #[serde(with = "clickhouse::serde::uuid")]
    pub key_id: Uuid,
    pub score: i64,
}

/// Seconds ClickHouse may spend ranking keys for one page of `GET /keys`.
const KEY_RANK_MAX_EXECUTION_SECS: u32 = 10;

/// A page of keys ranked by their use in `[since, until)`: highest first,
/// ties by key id (also descending, compared as text: ClickHouse's own UUID
/// order differs from the byte order Postgres and the cursor use), starting after `after` (a previous
/// page's last score and id), at most `limit` rows. With `key_ids`, only
/// those keys. Internal traffic is left out, as in the other usage reads.
/// Keys with no use in the window have no row.
pub async fn query_key_ranks(
    client: &clickhouse::Client,
    metric: KeyRankMetric,
    since: i64,
    until: i64,
    key_ids: &[Uuid],
    after: Option<(i64, Uuid)>,
    limit: Option<u64>,
) -> Result<Vec<KeyRank>, clickhouse::error::Error> {
    let mut filter = Filter::window(since, Some(until));
    filter.uuids("usage.key_id", key_ids).internal(None);
    let sql = key_ranks_sql(metric, filter.sql(), after.is_some(), limit);
    let mut query = filter.bind(client.query(&sql));
    if let Some((score, id)) = after {
        query = query.bind(score).bind(id.to_string());
    }
    query.fetch_all::<KeyRank>().await
}

fn key_ranks_sql(metric: KeyRankMetric, filter: &str, after: bool, limit: Option<u64>) -> String {
    let having = if after {
        " having (score, toString(key_id)) < (?, ?)"
    } else {
        ""
    };
    let limit = limit.map(|n| format!(" limit {n}")).unwrap_or_default();
    format!(
        "select key_id, {} as score from usage{filter} group by key_id{having} \
         order by score desc, toString(key_id) desc{limit} \
         settings max_execution_time = {KEY_RANK_MAX_EXECUTION_SECS}",
        metric.sql()
    )
}

/// One key's use in a window, from [`query_used_keys`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Row, Deserialize)]
pub struct KeyUse {
    #[serde(with = "clickhouse::serde::uuid")]
    pub key_id: Uuid,
    pub last_used_ms: i64,
    pub requests: u64,
}

/// Every key with use in `[since, until)` (internal traffic left out), with
/// its last request and request count: one small row per key, for telling
/// used keys from unused ones.
pub async fn query_used_keys(
    client: &clickhouse::Client,
    since: i64,
    until: i64,
) -> Result<Vec<KeyUse>, clickhouse::error::Error> {
    let mut filter = Filter::window(since, Some(until));
    filter.internal(None);
    let sql = format!(
        "select key_id, max(ts_ms) as last_used_ms, count() as requests \
         from usage{} group by key_id \
         settings max_execution_time = {KEY_RANK_MAX_EXECUTION_SECS}",
        filter.sql()
    );
    filter.bind(client.query(&sql)).fetch_all::<KeyUse>().await
}

/// Longest window `key-sources` and `key-rejections` read, in milliseconds.
pub const KEY_WATCH_MAX_WINDOW_MS: i64 = 31 * 86_400_000;

/// Most rows `key-sources` returns.
const KEY_WATCH_MAX_ROWS: u64 = 10_000;

/// Most rows `key-rejections` returns, and its default when no `limit` is
/// given. A caller that names neither `limit` nor `per_key` gets the same
/// rows it always did; when rows are cut the response says so (see
/// [`KeyRejections::truncated`]).
const KEY_REJECTIONS_MAX_ROWS: u32 = 10_000;

/// Seconds ClickHouse may spend on one `key-sources` or `key-rejections`
/// read before it gives up, so a wide window cannot tie up the server.
const KEY_WATCH_MAX_EXECUTION_SECS: u32 = 10;

/// Filters for the endpoints an account system polls to watch where its keys
/// are used from (`/usage/key-sources`, `/usage/key-rejections`).
#[derive(Debug, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyWatchQuery {
    /// Window start, unix epoch millis. Defaults to 24 hours before
    /// `until_ms`.
    pub since_ms: Option<i64>,
    /// Window end (exclusive), unix epoch millis. Defaults to now. The window
    /// may be at most 31 days long.
    pub until_ms: Option<i64>,
    /// Only this tenant's keys.
    #[schema(value_type = Option<String>)]
    pub tenant_id: Option<Uuid>,
    /// Only this key, or these keys: one UUID or up to 200 joined by commas
    /// (`a,b,c`). Present but empty (`key_id=`) is refused rather than read
    /// as "every key".
    #[serde(default, deserialize_with = "key_id_list")]
    #[schema(value_type = Option<String>)]
    #[param(value_type = Option<String>)]
    pub key_id: Vec<Uuid>,
    /// `key-rejections` only: most rows to return, newest `last_seen_ms`
    /// first. Default and most: 10,000.
    pub limit: Option<u32>,
    /// `key-rejections` only: most rows to return for any one key, so one
    /// noisy key cannot crowd out the rest. Default: no per-key cap.
    pub per_key: Option<u32>,
}

impl KeyWatchQuery {
    /// The window as `(since, until)`, or why it is not one.
    pub fn window(&self, now: i64) -> Result<(i64, i64), String> {
        let until = self.until_ms.unwrap_or(now);
        let since = self.since_ms.unwrap_or(until - 86_400_000);
        if since >= until {
            return Err("since_ms must be before until_ms".into());
        }
        if until - since > KEY_WATCH_MAX_WINDOW_MS {
            return Err("the window can be at most 31 days (since_ms to until_ms)".into());
        }
        Ok((since, until))
    }

    /// The tenant and key filters on `table`, added to `f`. Columns are
    /// qualified so a SELECT alias can never shadow them. `key_id` leads the
    /// sort key of `key_rejections`, so there an `in` list reads only those
    /// keys' granules; the ledger is sorted by tenant, so on `usage` only the
    /// tenant filter (and the window) narrows what is read.
    fn key_filters<'f>(&self, f: &'f mut Filter, table: &str) -> &'f mut Filter {
        f.uuid(&format!("{table}.tenant_id"), self.tenant_id)
            .uuids(&format!("{table}.key_id"), &self.key_id)
    }

    /// Rows `key-rejections` returns: `limit` clamped to 1..=10,000,
    /// default 10,000.
    fn rejection_limit(&self) -> u32 {
        self.limit
            .unwrap_or(KEY_REJECTIONS_MAX_ROWS)
            .clamp(1, KEY_REJECTIONS_MAX_ROWS)
    }

    /// Per-key row cap for `key-rejections`, clamped to 1..=10,000.
    fn rejection_per_key(&self) -> Option<u32> {
        self.per_key.map(|n| n.clamp(1, KEY_REJECTIONS_MAX_ROWS))
    }
}

/// One key's traffic from one network and country in the window.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct KeySource {
    #[schema(value_type = String)]
    pub key_id: Uuid,
    pub key_name: String,
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub tenant_name: String,
    /// The client addresses' network: an IPv4 /24 or IPv6 /48, as CIDR text.
    pub network: String,
    /// Two-letter country the edge named, or empty when none was given.
    pub client_country: String,
    /// Requests from this network and country in the window.
    pub requests: u64,
    /// Distinct client addresses among them.
    pub distinct_ips: u64,
    /// The most recent of them, unix epoch millis.
    pub last_seen_ms: i64,
    /// The first time this key was ever seen from this country, across all
    /// history the gateway kept since it began recording countries. `null`
    /// when the country is empty.
    pub first_seen_ms: Option<i64>,
    /// The first time this key was seen from this network, as far back as
    /// client addresses are kept (`OBLETH_CLIENT_IP_RETENTION_DAYS`).
    pub network_first_seen_ms: Option<i64>,
    /// The name of the narrowest named network (Settings › Networks) that
    /// holds this network, or `null`.
    #[serde(default)]
    pub network_name: Option<String>,
}

#[derive(Debug, Row, Deserialize)]
struct KeySourceRow {
    #[serde(with = "clickhouse::serde::uuid")]
    key_id: Uuid,
    key_name: String,
    #[serde(with = "clickhouse::serde::uuid")]
    tenant_id: Uuid,
    tenant_name: String,
    network: String,
    client_country: String,
    requests: u64,
    distinct_ips: u64,
    last_seen_ms: i64,
    country_first_ms: i64,
    network_first_ms: i64,
}

impl From<KeySourceRow> for KeySource {
    fn from(r: KeySourceRow) -> Self {
        let seen = |ms: i64| (ms > 0).then_some(ms);
        KeySource {
            first_seen_ms: if r.client_country.is_empty() {
                None
            } else {
                seen(r.country_first_ms)
            },
            network_first_seen_ms: seen(r.network_first_ms),
            key_id: r.key_id,
            key_name: r.key_name,
            tenant_id: r.tenant_id,
            tenant_name: r.tenant_name,
            network: r.network,
            client_country: r.client_country,
            requests: r.requests,
            distinct_ips: r.distinct_ips,
            last_seen_ms: r.last_seen_ms,
            network_name: None,
        }
    }
}

/// The `key-sources` query and its filters, in bind order: the window's,
/// then the first-seen tables' (country, then network).
fn key_sources_sql(q: &KeyWatchQuery, since: i64, until: i64) -> (String, [Filter; 3]) {
    let mut window = Filter::window(since, Some(until));
    window.and(" and client_ip != '' and client_network != ''");
    q.key_filters(&mut window, "usage");
    // The first-seen tables have no tenant column; their rows only join to
    // the window's keys, which are already filtered by tenant.
    let mut country = Filter::from("client_country != ''", Vec::new());
    country.uuids("key_country_seen.key_id", &q.key_id);
    let mut network = Filter::from("client_network != ''", Vec::new());
    network.uuids("key_network_seen.key_id", &q.key_id);
    // One row past the cap, so the caller can tell a cut list.
    let fetch = KEY_WATCH_MAX_ROWS + 1;
    let sql = format!(
        "select w.key_id as key_id, w.kname as key_name, w.tid as tenant_id, \
         w.tname as tenant_name, w.net as network, w.country as client_country, \
         w.reqs as requests, w.ips as distinct_ips, w.last_ms as last_seen_ms, \
         c.first_ms as country_first_ms, n.first_ms as network_first_ms \
         from ( \
           select key_id, any(usage.tenant_id) as tid, \
                  argMax(key_name, ts_ms) as kname, argMax(tenant_name, ts_ms) as tname, \
                  client_network as net, client_country as country, \
                  count() as reqs, uniqExact(client_ip) as ips, max(ts_ms) as last_ms \
           from usage{} \
           group by key_id, net, country \
         ) as w \
         left join ( \
           select key_id, client_country as country, min(first_seen_ms) as first_ms \
           from key_country_seen{} \
           group by key_id, country \
         ) as c on w.key_id = c.key_id and w.country = c.country \
         left join ( \
           select key_id, client_network as net, min(first_seen_ms) as first_ms \
           from key_network_seen{} \
           group by key_id, net \
         ) as n on w.key_id = n.key_id and w.net = n.net \
         order by key_id, requests desc \
         limit {fetch} \
         settings max_execution_time = {KEY_WATCH_MAX_EXECUTION_SECS}",
        window.sql(),
        country.sql(),
        network.sql(),
    );
    (sql, [window, country, network])
}

/// What `key-sources` read: the rows, and whether more matched than the
/// 10,000-row cap let through.
pub struct KeySources {
    pub rows: Vec<KeySource>,
    pub truncated: bool,
}

/// Each key with traffic in the window, one row per network and country it
/// came from, with when the key was first seen from each. Rows written
/// before client addresses were recorded (or whose address has expired) are
/// left out.
pub async fn query_key_sources(
    client: &clickhouse::Client,
    q: &KeyWatchQuery,
    since: i64,
    until: i64,
) -> Result<KeySources, clickhouse::error::Error> {
    let (sql, filters) = key_sources_sql(q, since, until);
    let query = filters
        .iter()
        .fold(client.query(&sql), |query, f| f.bind(query));
    let mut rows = query.fetch_all::<KeySourceRow>().await?;
    let truncated = rows.len() > KEY_WATCH_MAX_ROWS as usize;
    rows.truncate(KEY_WATCH_MAX_ROWS as usize);
    Ok(KeySources {
        rows: rows.into_iter().map(KeySource::from).collect(),
        truncated,
    })
}

/// Refused uses of one key for one reason from one address and country in
/// the window.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct KeyRejectionAgg {
    #[schema(value_type = String)]
    pub key_id: Uuid,
    pub key_name: String,
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub tenant_name: String,
    /// `disabled` or `ip_not_allowed`.
    pub reason: String,
    /// Empty once the address's retention has passed.
    pub client_ip: String,
    pub client_country: String,
    /// How many requests were refused.
    pub count: u64,
    pub first_seen_ms: i64,
    pub last_seen_ms: i64,
    /// The User-Agent of the most recent of them.
    pub sample_user_agent: String,
    /// The name of the narrowest named network (Settings › Networks) that
    /// holds the address, or `null`.
    #[serde(default)]
    pub network_name: Option<String>,
}

#[derive(Debug, Row, Deserialize)]
struct KeyRejectionAggRow {
    #[serde(with = "clickhouse::serde::uuid")]
    key_id: Uuid,
    key_name: String,
    #[serde(with = "clickhouse::serde::uuid")]
    tenant_id: Uuid,
    tenant_name: String,
    reason: String,
    client_ip: String,
    client_country: String,
    total: u64,
    first_ms: i64,
    last_ms: i64,
    sample_ua: String,
}

fn key_rejections_sql(q: &KeyWatchQuery, since: i64, until: i64) -> (String, Filter) {
    let mut filter = Filter::window(since, Some(until));
    q.key_filters(&mut filter, "key_rejections");
    let per_key = q
        .rejection_per_key()
        .map(|n| format!(" limit {n} by key_id"))
        .unwrap_or_default();
    // One row past the limit, so the caller can tell a cut list from a
    // complete one.
    let fetch = q.rejection_limit() + 1;
    let sql = format!(
        "select key_id as kid, argMax(key_name, ts_ms) as kname, \
         any(key_rejections.tenant_id) as tid, argMax(tenant_name, ts_ms) as tname, \
         reason as why, client_ip as ip, client_country as cc, \
         sum(count) as total, min(ts_ms) as first_ms, max(ts_ms) as last_ms, \
         argMax(user_agent, ts_ms) as sample_ua \
         from key_rejections{} \
         group by key_id, reason, client_ip, client_country \
         order by last_ms desc{per_key} \
         limit {fetch} \
         settings max_execution_time = {KEY_WATCH_MAX_EXECUTION_SECS}",
        filter.sql()
    );
    (sql, filter)
}

/// What `key-rejections` read: the rows, and whether more matched than
/// the limit let through.
pub struct KeyRejections {
    pub rows: Vec<KeyRejectionAgg>,
    /// True when rows past the limit were left out, so a caller summing
    /// counts knows it saw only part of the window.
    pub truncated: bool,
}

/// Refused uses of real keys in the window (disabled, or used from outside
/// their allowed networks), per key, reason, address and country.
pub async fn query_key_rejections(
    client: &clickhouse::Client,
    q: &KeyWatchQuery,
    since: i64,
    until: i64,
) -> Result<KeyRejections, clickhouse::error::Error> {
    let (sql, filter) = key_rejections_sql(q, since, until);
    let mut rows = filter
        .bind(client.query(&sql))
        .fetch_all::<KeyRejectionAggRow>()
        .await?;
    let limit = q.rejection_limit() as usize;
    let truncated = rows.len() > limit;
    rows.truncate(limit);
    let rows = rows
        .into_iter()
        .map(|r| KeyRejectionAgg {
            key_id: r.key_id,
            key_name: r.key_name,
            tenant_id: r.tenant_id,
            tenant_name: r.tenant_name,
            reason: r.reason,
            client_ip: r.client_ip,
            client_country: r.client_country,
            count: r.total,
            first_seen_ms: r.first_ms,
            last_seen_ms: r.last_ms,
            sample_user_agent: r.sample_ua,
            network_name: None,
        })
        .collect();
    Ok(KeyRejections { rows, truncated })
}

pub async fn query_usage_by_model(
    client: &clickhouse::Client,
    q: UsageQuery,
) -> Result<Vec<UsageModelAgg>, clickhouse::error::Error> {
    let filter = usage_filter(&q);
    filter
        .bind(client.query(&usage_by_model_sql(&q, filter.sql())))
        .fetch_all::<UsageModelAgg>()
        .await
}

fn usage_by_model_sql(q: &UsageQuery, filter: &str) -> String {
    format!(
        "select model, {} from usage{filter} group by model order by total_tok desc limit {}",
        usage_model_columns(),
        row_limit(q.limit, GROUPED_ROWS_DEFAULT),
    )
}

/// The columns of [`UsageModelAgg`] after `model`, in field order.
fn usage_model_columns() -> String {
    let (avg_ttft, p50_ttft) = (avg_ttft_sql(), p50_ttft_sql());
    format!(
        "count() as requests, \
         sum(input_tokens) as in_tok, sum(output_tokens) as out_tok, \
         sum(input_tokens) + sum(output_tokens) as total_tok, \
         round(if(countIf(total_ms >= 20 and output_tokens >= 1) > 0, \
         quantileIf(0.5)(output_tokens / (greatest({DECODE_MS_SQL}, 1) / 1000.), \
         total_ms >= 20 and output_tokens >= 1), 0), 1) as gen_tps, \
         round(sumIf(output_tokens / (greatest({DECODE_MS_SQL}, 1) / 1000.), \
         total_ms >= 20 and output_tokens >= 1), 1) as agg_tps, \
         {avg_ttft} as avg_ttft, \
         round(if(countIf(total_ms > 0) > 0, avgIf(total_ms, total_ms > 0), 0), 1) as avg_total, \
         {p50_ttft} as p50_ttft, \
         round(if(countIf(total_ms > 0) > 0, quantileIf(0.5)(total_ms, total_ms > 0), 0), 1) as p50_total, \
         round(avg(input_tokens), 1) as avg_prompt, \
         round(avg(output_tokens), 1) as avg_gen, \
         uniq(key_id) as users, \
         sum(energy_wh) as energy_wh, sum(energy_cost_usd) as energy_cost_usd, sum(co2_g) as co2_g"
    )
}

/// Aliases stay clear of the column names the query reads; see [`usage_totals_sql`].
fn usage_series_sql(bucket: i64, filter: &str) -> String {
    let (avg_ttft, p50_ttft) = (avg_ttft_sql(), p50_ttft_sql());
    format!(
        "select intDiv(ts_ms, {bucket}) * {bucket} as bucket_ms, \
         count() as requests, \
         sum(input_tokens) as in_tok, sum(output_tokens) as out_tok, \
         sum(input_tokens) + sum(output_tokens) as total_tok, \
         sum(energy_wh) as energy_wh, \
         countIf(status_code >= 400) as errors, \
         sum(cost_usd) as cost_usd, \
         {p50_ttft} as p50_ttft_ms, \
         {avg_ttft} as avg_ttft_ms, \
         countIf({class} in {platform}) as platform_errors \
         from usage{filter} group by bucket_ms order by bucket_ms",
        class = obleth_config::EFFECTIVE_CLASS_SQL,
        platform = obleth_config::PLATFORM_CLASSES_SQL,
    )
}

pub async fn query_usage_series(
    client: &clickhouse::Client,
    q: UsageSeriesQuery,
) -> Result<Vec<UsageTimePoint>, clickhouse::error::Error> {
    let since = window_start(q.since_ms, q.until_ms);
    let until = q.until_ms.unwrap_or_else(now_ms);
    let bucket = series_bucket_ms(since, q.bucket_ms, 60_000, until);
    let filter = series_filter(&q, since);
    filter
        .bind(client.query(&usage_series_sql(bucket, filter.sql())))
        .fetch_all::<UsageTimePoint>()
        .await
}

pub async fn query_usage_series_by_tenant(
    client: &clickhouse::Client,
    q: UsageSeriesQuery,
) -> Result<Vec<TenantUsageTimePoint>, clickhouse::error::Error> {
    let since = window_start(q.since_ms, q.until_ms);
    let until = q.until_ms.unwrap_or_else(now_ms);
    let bucket = series_bucket_ms(since, q.bucket_ms, 10_000, until);
    let filter = series_filter(&q, since);
    let sql = format!(
        "select tenant_id, intDiv(ts_ms, {bucket}) * {bucket} as bucket_ms, \
         count() as requests, \
         sum(input_tokens) + sum(output_tokens) as total_tokens, \
         sum(energy_wh) as energy_wh \
         from usage{} \
         group by tenant_id, bucket_ms order by bucket_ms, tenant_id",
        filter.sql()
    );
    filter
        .bind(client.query(&sql))
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
    if q.model.is_none() {
        return Ok(Vec::new());
    }
    let since = window_start(q.since_ms, q.until_ms);
    let until = q.until_ms.unwrap_or_else(now_ms);
    let bucket = series_bucket_ms(since, q.bucket_ms, 10_000, until);
    let filter = series_filter(&q, since);
    // Per-stream engine rates, NOT volume-over-wall-clock. `gen_tps` is the
    // median decode rate (output tokens over the decode window = total - ttft,
    // falling back to total - queue when the response wasn't streamed), the same
    // definition `/usage/models` uses. `prompt_tps` is the median prefill rate
    // (prompt tokens over ttft, since prefill completes by the first byte). This
    // makes the chart comparable to the upstream engine's reported throughput
    // instead of collapsing toward zero on sparse, bursty traffic.
    let (avg_ttft, p50_ttft) = (avg_ttft_sql(), p50_ttft_sql());
    let prefill = prefill_rated_sql();
    let sql = format!(
        "select intDiv(ts_ms, {bucket}) * {bucket} as bucket_ms, \
         count() as requests, \
         round(if(countIf(total_ms >= 20 and output_tokens >= 1) > 0, \
         quantileIf(0.5)(output_tokens / (greatest({DECODE_MS_SQL}, 1) / 1000.), \
         total_ms >= 20 and output_tokens >= 1), 0), 1) as gen_tps, \
         round(if(countIf({prefill}) > 0, \
         quantileIf(0.5)(input_tokens / (greatest(ttft_ms, 1) / 1000.), {prefill}), 0), 1) as prompt_tps, \
         {avg_ttft} as avg_ttft, \
         {p50_ttft} as p50_ttft, \
         round(if(countIf(total_ms > 0) > 0, avgIf(total_ms, total_ms > 0), 0), 1) as avg_total, \
         round(if(countIf(total_ms > 0) > 0, quantileIf(0.5)(total_ms, total_ms > 0), 0), 1) as p50_total, \
         sum(energy_wh) as energy_wh \
         from usage{} \
         group by bucket_ms order by bucket_ms",
        filter.sql()
    );
    filter
        .bind(client.query(&sql))
        .fetch_all::<ModelUsageTimePoint>()
        .await
}

/// Per tenant/key breakdown for one model. Mirrors the gen-rate math in
/// `query_usage_by_model` but groups by `(key_id, tenant_id)` and is scoped to
/// a single model, so the expanded model card can show who is driving load.
pub async fn query_usage_breakdown_by_model(
    client: &clickhouse::Client,
    q: &UsageBreakdownQuery,
) -> Result<Vec<UsageKeyModelBreakdown>, clickhouse::error::Error> {
    let limit = row_limit(q.limit, 100);
    let mut filter = Filter::window(window_start(q.since_ms, q.until_ms), q.until_ms);
    filter
        .text("model", Some(&q.model))
        .internal(q.include_internal);
    let sql = format!(
        "select key_id, tenant_id, count() as requests, \
         sum(input_tokens) + sum(output_tokens) as total_tokens, \
         round(if(countIf(total_ms >= 20 and output_tokens >= 1) > 0, \
         quantileIf(0.5)(output_tokens / (greatest({DECODE_MS_SQL}, 1) / 1000.), \
         total_ms >= 20 and output_tokens >= 1), 0), 1) as gen_tps, \
         sum(energy_wh) as energy_wh, sum(energy_cost_usd) as energy_cost_usd, sum(co2_g) as co2_g \
         from usage{} \
         group by key_id, tenant_id order by total_tokens desc limit {limit}",
        filter.sql()
    );
    filter
        .bind(client.query(&sql))
        .fetch_all::<UsageKeyModelBreakdown>()
        .await
}

/// One model's speed over a window, or over one hour or day of it. Rates are
/// per request, so they show what a caller saw whatever the traffic volume.
/// Only successful, uncached requests that wrote at least 16 tokens are rated:
/// a one-word answer has no speed worth the name.
#[derive(Debug, Clone, Row, Serialize, Deserialize, ToSchema)]
pub struct ModelSpeedPoint {
    /// Start of the hour or day, unix epoch millis; 0 for a whole-window row.
    pub bucket_ms: i64,
    pub model: String,
    /// Every request to the model in the bucket, failed and cached included.
    pub requests: u64,
    /// The requests a speed was measured on.
    pub rated: u64,
    /// Median generation rate: output tokens over the decode window, the
    /// same definition as `/usage/models`.
    pub gen_tokens_per_sec: f64,
    /// The rate the slowest tenth of requests got (10th percentile).
    pub slow_tokens_per_sec: f64,
    /// Median prefill rate: prompt tokens over time to first token.
    pub prompt_tokens_per_sec: f64,
    /// Median time to first token in milliseconds, over answered requests that streamed a first token, cache hits left out.
    pub p50_ttft_ms: f64,
}

/// Longest window `/usage/speed` cuts into hours; past it, rows are per day.
const SPEED_MAX_HOURLY_MS: i64 = 31 * 86_400_000;

/// The bucket column for `/usage/speed`. The time zone is a bound value, so
/// it comes first in the bind order when there is one.
fn speed_bucket_sql(bucket: &str, span_ms: i64) -> (&'static str, bool) {
    match bucket {
        "hour" if span_ms <= SPEED_MAX_HOURLY_MS => (
            "toInt64(toUnixTimestamp(toStartOfHour(ts, ?))) * 1000",
            true,
        ),
        "hour" | "day" => ("toInt64(toUnixTimestamp(toStartOfDay(ts, ?))) * 1000", true),
        _ => ("toInt64(0)", false),
    }
}

/// Aliases stay clear of the column names the query reads; see [`usage_totals_sql`].
fn usage_speed_sql(bucket_expr: &str, filter: &str) -> String {
    let rated =
        "status_code < 400 and cache_status != 'hit' and total_ms >= 20 and output_tokens >= 16";
    let rate = format!("output_tokens / (greatest({DECODE_MS_SQL}, 1) / 1000.)");
    let prefill = prefill_rated_sql();
    let p50_ttft = p50_ttft_sql();
    format!(
        "select {bucket_expr} as bucket_ms, model as model_name, \
         count() as reqs, \
         countIf({rated}) as rated_reqs, \
         round(if(countIf({rated}) > 0, quantileIf(0.5)({rate}, {rated}), 0), 1) as gen_tps, \
         round(if(countIf({rated}) > 0, quantileIf(0.1)({rate}, {rated}), 0), 1) as slow_tps, \
         round(if(countIf({prefill}) > 0, quantileIf(0.5)(input_tokens / (greatest(ttft_ms, 1) / 1000.), {prefill}), 0), 1) as prompt_tps, \
         {p50_ttft} as p50_ttft \
         from usage{filter} \
         group by bucket_ms, model_name order by bucket_ms, rated_reqs desc"
    )
}

/// The `where` clause of the speed read: [`UsageSpeedQuery`]'s filters over
/// `[since, until)`.
fn speed_filter(q: &UsageSpeedQuery, since: i64, until: i64) -> Filter {
    let mut f = Filter::window(since, Some(until));
    f.uuid("tenant_id", q.tenant_id)
        .uuid("key_id", q.key_id)
        .text("model", q.model.as_deref())
        .internal(q.include_internal);
    f
}

/// Per-model speed for the Reports page. `tz` must already be a valid IANA
/// name; the handler checks it.
pub async fn query_usage_speed(
    client: &clickhouse::Client,
    q: UsageSpeedQuery,
) -> Result<Vec<ModelSpeedPoint>, clickhouse::error::Error> {
    let until = q.until_ms.unwrap_or_else(now_ms);
    let since = q.since_ms.unwrap_or(until - 86_400_000);
    let (bucket_expr, binds_tz) =
        speed_bucket_sql(q.bucket.as_deref().unwrap_or("none"), until - since);
    let filter = speed_filter(&q, since, until);
    let sql = usage_speed_sql(bucket_expr, filter.sql());
    let mut query = client.query(&sql);
    // The bucket's time zone is in the select list, ahead of the filter.
    if binds_tz {
        query = query.bind(q.tz.clone().unwrap_or_else(|| "UTC".to_string()));
    }
    filter.bind(query).fetch_all::<ModelSpeedPoint>().await
}

pub async fn query_cache_stats(
    client: &clickhouse::Client,
    q: UsageQuery,
) -> Result<CacheStats, clickhouse::error::Error> {
    let filter = usage_filter(&q);
    let sql = format!(
        "select countIf(cache_status = 'hit') as hits, \
         countIf(cache_status = 'miss') as misses, \
         sumIf(input_tokens + output_tokens, cache_status = 'hit') as tokens_saved \
         from usage{}",
        filter.sql()
    );
    let rows = filter
        .bind(client.query(&sql))
        .fetch_all::<CacheStats>()
        .await?;
    Ok(rows.into_iter().next().unwrap_or(CacheStats {
        hits: 0,
        misses: 0,
        tokens_saved: 0,
    }))
}

pub async fn query_costs(
    client: &clickhouse::Client,
    q: &UsageQuery,
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
    let filter = usage_filter(q);
    let sql = format!(
        "select model, count() as requests, \
         sum(input_tokens) as input_tokens, sum(output_tokens) as output_tokens, \
         sum(cost_usd) as total_cost \
         from usage{} group by model order by total_cost desc limit {}",
        filter.sql(),
        row_limit(q.limit, GROUPED_ROWS_DEFAULT),
    );
    let rows = filter
        .bind(client.query(&sql))
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
        "day_tenant" => (
            "toString(day)",
            "tenant_id",
            "toUUID('00000000-0000-0000-0000-000000000000') as key_id",
            "'' as model",
            "group by day, tenant_id order by day, total_tokens desc",
        ),
        "day_model" => (
            "toString(day)",
            "toUUID('00000000-0000-0000-0000-000000000000') as tenant_id",
            "toUUID('00000000-0000-0000-0000-000000000000') as key_id",
            "model",
            "group by day, model order by day, total_tokens desc",
        ),
        "day_key_model" => (
            "toString(day)",
            "tenant_id",
            "key_id",
            "model",
            "group by day, tenant_id, key_id, model order by day, total_tokens desc",
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
    let start = q.start_day.clone().unwrap_or_else(default_start_day);
    let end = q.end_day.clone().unwrap_or_else(today_day);
    let mut inner = Filter::from(
        "day >= toDate(?) and day <= toDate(?)",
        vec![Bind::Text(start), Bind::Text(end)],
    );
    inner
        .uuid("tenant_id", q.tenant_id)
        .uuids("key_id", &q.key_id)
        .text("model", q.model.as_deref());

    let sql = format!(
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
         round(sum(ttft_ms_sum) / greatest(sum({DAILY_TTFT_REQUESTS}), 1), 1) as avg_ttft_ms, \
         round(sum(total_ms_sum) / greatest(sum(success_requests), 1), 1) as avg_total_ms, \
         round(sum(cost_usd_sum), 6) as cost_usd, \
         round(sum(energy_wh_sum), 6) as energy_wh, \
         round(sum(energy_cost_usd_sum), 6) as energy_cost_usd, \
         round(sum(co2_g_sum), 6) as co2_g, \
         sum(platform_error_requests), \
         sum({DAILY_TTFT_REQUESTS}) \
         from (select * from usage_daily{}) {group_clause}",
        inner.sql()
    );
    inner
        .bind(client.query(&sql))
        .fetch_all::<UsageDailyRow>()
        .await
}

/// A daily rollup row's first-token request count. Rows rolled up before
/// v2.13 summed first-token times over every answered request and counted
/// none, so they divide by their answered requests, as they were read then.
/// On the upgrade day a merge can join an old and a new row of one key and
/// model; that day's average for them leans high, and the days after are
/// exact.
const DAILY_TTFT_REQUESTS: &str =
    "if(ttft_requests = 0 and ttft_ms_sum > 0, success_requests, ttft_requests)";

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

/// The `where` clause of the aggregate reads: [`UsageQuery`]'s window and
/// filters.
fn usage_filter(q: &UsageQuery) -> Filter {
    let mut f = Filter::window(window_start(q.since_ms, q.until_ms), q.until_ms);
    f.uuid("tenant_id", q.tenant_id)
        .uuid("key_id", q.key_id)
        .text("model", q.model.as_deref())
        .internal(q.include_internal);
    f
}

/// The `where` clause of the series reads: [`UsageSeriesQuery`]'s window and
/// filters.
fn series_filter(q: &UsageSeriesQuery, since: i64) -> Filter {
    let mut f = Filter::window(since, q.until_ms);
    f.uuid("tenant_id", q.tenant_id)
        .text("model", q.model.as_deref())
        .internal(q.include_internal);
    f
}

/// Most buckets a series read may return per series. A wide window with a fine
/// bucket (or `since_ms=0`) would otherwise return millions of rows.
pub const MAX_SERIES_BUCKETS: i64 = 5_000;

/// Bucket width for a series read: the requested width (default 5 minutes),
/// raised to `floor_ms`, then widened until `[since, now]` fits in
/// [`MAX_SERIES_BUCKETS`].
fn series_bucket_ms(since: i64, requested: Option<i64>, floor_ms: i64, now: i64) -> i64 {
    let span = now.saturating_sub(since).max(0);
    // Buckets are epoch-aligned (`intDiv(ts_ms, b) * b`), so a span covers up
    // to `span / b + 1` of them; size `b` for `MAX - 1` whole widths.
    let widths = MAX_SERIES_BUCKETS - 1;
    let min_for_cap = span / widths + i64::from(span % widths != 0);
    requested.unwrap_or(300_000).max(floor_ms).max(min_for_cap)
}

pub(crate) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// One row from the `spans` table, returned by the per-request trace endpoint.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, clickhouse::Row, ToSchema)]
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

#[cfg(test)]
mod internal_filter_tests {
    use super::*;

    #[test]
    fn default_excludes_internal_traffic() {
        assert_eq!(
            internal_filter(None),
            " and request_type not in ('health_probe', 'benchmark')"
        );
        assert_eq!(internal_filter(Some(false)), internal_filter(None));
    }

    #[test]
    fn opt_in_disables_the_filter() {
        assert!(internal_filter(Some(true)).is_empty());
    }

    #[test]
    fn key_usage_summary_filters_on_the_sort_key_prefix() {
        let sql = key_usage_summary_sql(None);
        assert!(
            sql.contains("where usage.tenant_id = toUUID(?) and key_id = toUUID(?)"),
            "{sql}"
        );
        // 8 window binds + tenant + key.
        assert_eq!(sql.matches('?').count(), 10, "{sql}");
    }

    #[test]
    fn traced_only_subquery_uses_the_outer_window() {
        let sql = traced_only_filter(1_700_000_000_000, None);
        assert!(sql.contains("start_ms >= 1700000000000"), "{sql}");
        assert!(!sql.contains("start_ms >= 0 "), "{sql}");
        let bounded = traced_only_filter(5, Some(9));
        assert!(
            bounded.contains("start_ms >= 5 AND start_ms < 9"),
            "{bounded}"
        );
    }

    #[test]
    fn series_bucket_honours_request_and_floor_in_short_windows() {
        let now = 1_700_000_000_000;
        let day = 86_400_000;
        assert_eq!(series_bucket_ms(now - day, None, 60_000, now), 300_000);
        assert_eq!(
            series_bucket_ms(now - day, Some(1_000), 60_000, now),
            60_000
        );
        assert_eq!(
            series_bucket_ms(now - day, Some(3_600_000), 10_000, now),
            3_600_000
        );
        // Future `since` is an empty window, not a negative width.
        assert_eq!(series_bucket_ms(now + day, None, 10_000, now), 300_000);
    }

    #[test]
    fn series_bucket_widens_so_wide_windows_stay_under_the_cap() {
        let now = 1_700_000_000_123;
        for (since, requested, floor) in [
            (0, Some(10_000), 10_000),
            (0, None, 60_000),
            (now - 365 * 86_400_000, Some(10_000), 10_000),
            (now - 30 * 86_400_000, Some(60_000), 60_000),
        ] {
            let b = series_bucket_ms(since, requested, floor, now);
            let first = since.max(0) / b;
            let last = now / b;
            assert!(
                last - first < MAX_SERIES_BUCKETS,
                "since={since} bucket={b}: {} buckets",
                last - first + 1
            );
        }
    }

    #[test]
    fn usage_by_key_and_model_placeholders_match_binds() {
        let query = |tenant: bool, key: bool, model: bool| UsageQuery {
            tenant_id: tenant.then(Uuid::new_v4),
            key_id: key.then(Uuid::new_v4),
            model: model.then(|| "m".to_string()),
            since_ms: None,
            until_ms: model.then_some(1_750_000_000_000),
            limit: Some(10),
            include_internal: None,
        };
        for tenant in [false, true] {
            for key in [false, true] {
                for model in [false, true] {
                    let q = query(tenant, key, model);
                    let f = usage_filter(&q);
                    for sql in [
                        usage_by_key_sql(&q, f.sql()),
                        usage_by_model_sql(&q, f.sql()),
                    ] {
                        assert_eq!(
                            sql.matches('?').count(),
                            f.binds.len(),
                            "tenant={tenant} key={key} model={model}: {sql}"
                        );
                        assert!(sql.ends_with(" limit 10"), "{sql}");
                    }
                }
            }
        }
        // No limit reads the default, and a huge one is capped.
        let mut q = query(false, false, false);
        q.limit = None;
        assert!(usage_by_key_sql(&q, "").ends_with(" limit 1000"));
        q.limit = Some(u64::MAX);
        assert!(usage_by_model_sql(&q, "").ends_with(" limit 10000"));
    }

    #[test]
    fn usage_filter_reads_the_partition_window_then_the_filters() {
        let (tenant, key) = (Uuid::new_v4(), Uuid::new_v4());
        let q = UsageQuery {
            tenant_id: Some(tenant),
            key_id: Some(key),
            model: Some("m".into()),
            since_ms: Some(5),
            until_ms: Some(9),
            limit: None,
            include_internal: Some(true),
        };
        let f = usage_filter(&q);
        assert_eq!(
            f.sql(),
            " where ts >= fromUnixTimestamp64Milli(toInt64(?)) and ts_ms >= ? \
             and ts < fromUnixTimestamp64Milli(toInt64(?)) and ts_ms < ? \
             and tenant_id = toUUID(?) and key_id = toUUID(?) and model = ?"
        );
        assert_eq!(
            f.binds,
            vec![
                Bind::Int(5),
                Bind::Int(5),
                Bind::Int(9),
                Bind::Int(9),
                Bind::Text(tenant.to_string()),
                Bind::Text(key.to_string()),
                Bind::Text("m".into()),
            ]
        );
        // `since` defaults to a day before `until`, not before now.
        let open = usage_filter(&UsageQuery {
            since_ms: None,
            ..q
        });
        assert_eq!(open.binds[0], Bind::Int(9 - 86_400_000));
    }

    #[test]
    fn until_is_exclusive_and_open_ended_by_default() {
        let bounded = Filter::window(5, Some(9));
        assert!(
            bounded.sql().contains(" and ts_ms < ?"),
            "{}",
            bounded.sql()
        );
        assert!(!bounded.sql().contains("<="), "{}", bounded.sql());
        let open = Filter::window(5, None);
        assert_eq!(
            open.sql(),
            " where ts >= fromUnixTimestamp64Milli(toInt64(?)) and ts_ms >= ?"
        );
        assert_eq!(open.binds, vec![Bind::Int(5), Bind::Int(5)]);
    }

    #[test]
    fn usage_reads_refuse_unknown_parameters() {
        let bad = serde_json::json!({ "group_by": "tenant" });
        assert!(serde_json::from_value::<UsageQuery>(bad).is_err());
        let typo = serde_json::json!({ "untill_ms": 5 });
        assert!(serde_json::from_value::<UsageSeriesQuery>(typo).is_err());
        let ok = serde_json::json!({ "since_ms": 1, "until_ms": 2 });
        assert!(serde_json::from_value::<UsageQuery>(ok).is_ok());
    }
}

#[cfg(test)]
mod alias_tests {
    use super::*;

    /// The select list's top-level expressions, each with its alias.
    fn select_items(sql: &str) -> Vec<(String, String)> {
        let body = sql.split_once("select ").unwrap().1;
        let body = body.split(" from ").next().unwrap();
        let (mut items, mut depth, mut cur) = (Vec::new(), 0i32, String::new());
        for c in body.chars() {
            match c {
                '(' => depth += 1,
                ')' => depth -= 1,
                ',' if depth == 0 => {
                    items.push(std::mem::take(&mut cur));
                    continue;
                }
                _ => {}
            }
            cur.push(c);
        }
        items.push(cur);
        items
            .into_iter()
            .map(|item| {
                let (expr, alias) = item.rsplit_once(" as ").expect("every column is aliased");
                (expr.trim().to_string(), alias.trim().to_string())
            })
            .collect()
    }

    fn reads(expr: &str, column: &str) -> bool {
        expr.split(|c: char| !(c.is_alphanumeric() || c == '_'))
            .any(|word| word == column)
    }

    /// ClickHouse resolves an alias anywhere in the query, so an alias that
    /// names a column another expression reads turns that expression into an
    /// aggregate of an aggregate and fails the read.
    fn assert_no_shadowing(sql: &str) {
        let items = select_items(sql);
        for (i, (_, alias)) in items.iter().enumerate() {
            for (j, (expr, other)) in items.iter().enumerate() {
                assert!(
                    i == j || !reads(expr, alias),
                    "alias `{alias}` shadows a column that `{other}` reads: {expr}"
                );
            }
        }
    }

    fn watch_query(tenant: bool, key: bool) -> KeyWatchQuery {
        KeyWatchQuery {
            since_ms: None,
            until_ms: None,
            tenant_id: tenant.then(Uuid::new_v4),
            key_id: key.then(Uuid::new_v4).into_iter().collect(),
            limit: None,
            per_key: None,
        }
    }

    #[test]
    fn key_watch_reads_bind_one_value_per_placeholder() {
        for (tenant, key) in [(false, false), (true, false), (false, true), (true, true)] {
            let q = watch_query(tenant, key);
            let (sql, [window, country, network]) = key_sources_sql(&q, 1, 2);
            let filters = usize::from(tenant) + usize::from(key);
            assert_eq!(window.binds.len(), 4 + filters);
            assert_eq!(country.binds.len(), usize::from(key));
            assert_eq!(network.binds.len(), usize::from(key));
            assert_eq!(
                sql.matches('?').count(),
                window.binds.len() + country.binds.len() + network.binds.len(),
                "{sql}"
            );
            assert!(
                !sql.contains("from usage where ts >= fromUnixTimestamp64Milli(toInt64(?)) and ts_ms >= ? and client_network"),
                "first-seen networks come from key_network_seen, not a ledger scan"
            );
            assert!(
                sql.ends_with("limit 10001 settings max_execution_time = 10"),
                "{sql}"
            );
            assert!(
                sql.contains("client_ip != ''"),
                "old rows without an address are skipped"
            );

            let (sql, filter) = key_rejections_sql(&q, 1, 2);
            assert_eq!(filter.binds.len(), 4 + filters);
            assert_eq!(sql.matches('?').count(), filter.binds.len(), "{sql}");
            assert_no_shadowing(&sql);
            assert_eq!(select_items(&sql).len(), 11);
        }
        let (sql, _) = key_sources_sql(&watch_query(true, true), 1, 2);
        assert_no_shadowing(&sql);
        assert_eq!(select_items(&sql).len(), 11);
        assert!(sql.contains("usage.tenant_id = toUUID(?)"));
        assert!(sql.contains("key_country_seen.key_id = toUUID(?)"));
        assert!(sql.contains("key_network_seen.key_id = toUUID(?)"));
        assert!(!sql.contains("key_country_seen.tenant_id"));
    }

    #[test]
    fn key_rejections_take_a_key_list_and_limits() {
        let keys: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        let raw = keys
            .iter()
            .map(Uuid::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let q: KeyWatchQuery = serde_json::from_value(serde_json::json!({
            "tenant_id": Uuid::new_v4().to_string(),
            "key_id": format!("{raw},"),
            "limit": 50,
            "per_key": 5,
        }))
        .expect("parses");
        assert_eq!(q.key_id, keys);
        let (sql, filter) = key_rejections_sql(&q, 1, 2);
        assert!(
            sql.contains(" and key_rejections.key_id in (toUUID(?), toUUID(?), toUUID(?))"),
            "{sql}"
        );
        assert!(sql.contains(Filter::window(1, Some(2)).sql()), "{sql}");
        assert_eq!(
            filter.texts()[1..],
            keys.iter().map(Uuid::to_string).collect::<Vec<_>>()[..]
        );
        assert_eq!(sql.matches('?').count(), filter.binds.len());
        assert!(
            sql.contains("order by last_ms desc limit 5 by key_id limit 51 settings"),
            "{sql}"
        );
        assert!(sql.ends_with("settings max_execution_time = 10"), "{sql}");
        assert_no_shadowing(&sql);

        // key-sources reads the same list from each of its tables.
        let (sql, [window, country, network]) = key_sources_sql(&q, 1, 2);
        assert_eq!(
            (
                window.texts().len(),
                country.texts().len(),
                network.texts().len()
            ),
            (4, 3, 3)
        );
        assert!(sql.contains("key_country_seen.key_id in (toUUID(?), toUUID(?), toUUID(?))"));
        assert!(sql.contains("key_network_seen.key_id in (toUUID(?), toUUID(?), toUUID(?))"));
        assert_eq!(
            sql.matches('?').count(),
            window.binds.len() + country.binds.len() + network.binds.len()
        );

        // A single key reads exactly as before; limits default and clamp.
        let one = watch_query(false, true);
        let (sql, filter) = key_rejections_sql(&one, 1, 2);
        assert!(sql.contains(" and key_rejections.key_id = toUUID(?)"));
        assert_eq!(filter.texts().len(), 1);
        assert!(
            sql.contains("order by last_ms desc limit 10001 settings"),
            "{sql}"
        );
        let mut big = watch_query(false, false);
        big.limit = Some(50_000);
        big.per_key = Some(0);
        let (sql, _) = key_rejections_sql(&big, 1, 2);
        assert!(sql.contains("limit 1 by key_id limit 10001 "), "{sql}");
        big.limit = Some(0);
        assert!(key_rejections_sql(&big, 1, 2)
            .0
            .contains("limit 1 by key_id limit 2 "));

        // A key_id that is present but names no key is refused, never read
        // as "every key".
        for empty in ["", ",", " , "] {
            let q = serde_json::json!({ "tenant_id": Uuid::new_v4().to_string(), "key_id": empty });
            assert!(
                serde_json::from_value::<KeyWatchQuery>(q).is_err(),
                "{empty:?}"
            );
        }
        let absent: KeyWatchQuery = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(absent.key_id.is_empty());

        // A bad id, or more than 200, is refused.
        let bad = serde_json::json!({ "key_id": format!("{},nope", keys[0]) });
        assert!(serde_json::from_value::<KeyWatchQuery>(bad).is_err());
        let many = vec![keys[0].to_string(); MAX_LOG_FILTER_VALUES + 1].join(",");
        assert!(
            serde_json::from_value::<KeyWatchQuery>(serde_json::json!({ "key_id": many })).is_err()
        );
        let max = vec![keys[0].to_string(); MAX_LOG_FILTER_VALUES].join(",");
        assert!(
            serde_json::from_value::<KeyWatchQuery>(serde_json::json!({ "key_id": max })).is_ok()
        );
    }

    #[test]
    fn key_watch_window_defaults_to_a_day_and_caps_at_31_days() {
        let now = 1_750_000_000_000;
        let mut q = watch_query(false, false);
        assert_eq!(q.window(now), Ok((now - 86_400_000, now)));
        q.until_ms = Some(now - 1000);
        assert_eq!(q.window(now), Ok((now - 1000 - 86_400_000, now - 1000)));
        q.since_ms = Some(now - 1000 - KEY_WATCH_MAX_WINDOW_MS);
        assert!(q.window(now).is_ok());
        q.since_ms = Some(now - 1001 - KEY_WATCH_MAX_WINDOW_MS);
        assert!(q.window(now).unwrap_err().contains("31 days"));
        q.since_ms = Some(now);
        assert!(q.window(now).unwrap_err().contains("before"));
    }

    #[test]
    fn key_sources_report_first_seen_only_when_known() {
        let row = |country: &str, country_first: i64, network_first: i64| KeySourceRow {
            key_id: Uuid::nil(),
            key_name: "k".into(),
            tenant_id: Uuid::nil(),
            tenant_name: "t".into(),
            network: "203.0.113.0/24".into(),
            client_country: country.into(),
            requests: 3,
            distinct_ips: 2,
            last_seen_ms: 9,
            country_first_ms: country_first,
            network_first_ms: network_first,
        };
        let seen = KeySource::from(row("DE", 5, 4));
        assert_eq!(
            (seen.first_seen_ms, seen.network_first_seen_ms),
            (Some(5), Some(4))
        );
        // No country: no country first-seen, whatever the join gave.
        let no_country = KeySource::from(row("", 5, 4));
        assert_eq!(no_country.first_seen_ms, None);
        // A missing join row reads as 0 in ClickHouse: unknown.
        let unknown = KeySource::from(row("DE", 0, 0));
        assert_eq!(
            (unknown.first_seen_ms, unknown.network_first_seen_ms),
            (None, None)
        );
    }

    #[test]
    fn totals_aliases_do_not_shadow_columns() {
        assert_no_shadowing(&usage_totals_sql(""));
        assert_eq!(select_items(&usage_totals_sql("")).len(), 13);
    }

    #[test]
    fn series_aliases_do_not_shadow_columns() {
        assert_no_shadowing(&usage_series_sql(60_000, ""));
        assert_eq!(select_items(&usage_series_sql(60_000, "")).len(), 11);
    }

    fn speed_query(filters: bool) -> UsageSpeedQuery {
        UsageSpeedQuery {
            since_ms: None,
            until_ms: None,
            bucket: None,
            tz: None,
            tenant_id: filters.then(Uuid::new_v4),
            key_id: filters.then(Uuid::new_v4),
            model: filters.then(|| "m".to_string()),
            include_internal: None,
        }
    }

    #[test]
    fn speed_aliases_do_not_shadow_columns() {
        let (expr, _) = speed_bucket_sql("day", 86_400_000);
        let sql = usage_speed_sql(expr, speed_filter(&speed_query(false), 0, 1).sql());
        assert_no_shadowing(&sql);
        assert_eq!(select_items(&sql).len(), 8);
    }

    #[test]
    fn speed_binds_one_value_per_placeholder() {
        for bucket in ["none", "hour", "day", "week"] {
            for filters in [false, true] {
                let (expr, tz) = speed_bucket_sql(bucket, 86_400_000);
                let f = speed_filter(&speed_query(filters), 0, 86_400_000);
                let sql = usage_speed_sql(expr, f.sql());
                // tz, in the select list, then the filter's own.
                let binds = usize::from(tz) + f.binds.len();
                assert_eq!(f.binds.len(), 4 + if filters { 3 } else { 0 });
                assert_eq!(sql.matches('?').count(), binds, "{bucket}: {sql}");
            }
        }
    }

    #[test]
    fn speed_cuts_long_windows_into_days_not_hours() {
        assert!(speed_bucket_sql("hour", 7 * 86_400_000)
            .0
            .contains("toStartOfHour"));
        assert!(speed_bucket_sql("hour", 90 * 86_400_000)
            .0
            .contains("toStartOfDay"));
        assert_eq!(speed_bucket_sql("none", 0), ("toInt64(0)", false));
    }

    #[test]
    fn log_histogram_aliases_do_not_shadow_columns() {
        let sql = usage_log_histogram_sql(60_000, " where ts_ms >= ?");
        assert_no_shadowing(&sql);
        assert_eq!(select_items(&sql).len(), 4);
    }

    pub(super) fn log_query() -> UsageLogQuery {
        UsageLogQuery {
            tenant_id: Vec::new(),
            key_id: Vec::new(),
            model: Vec::new(),
            request_type: None,
            session_id: None,
            device_id: None,
            status: None,
            status_code: None,
            fault: None,
            error_class: Vec::new(),
            request_id: None,
            client_ip: None,
            parent_request_id: None,
            since_ms: None,
            until_ms: None,
            before_ms: None,
            before_request_id: None,
            limit: None,
            traced_only: None,
            include_internal: None,
            bucket_ms: None,
        }
    }

    #[test]
    fn log_filters_bind_one_value_per_placeholder() {
        let Filter { sql, binds } = log_filter_sql(&log_query(), 0);
        assert_eq!(sql.matches('?').count(), binds.len());
        let full = UsageLogQuery {
            tenant_id: vec![Uuid::nil()],
            key_id: vec![Uuid::nil()],
            model: vec!["m".into()],
            request_type: Some("chat".into()),
            session_id: Some("s".into()),
            device_id: Some("d".into()),
            status: Some("error".into()),
            status_code: Some(502),
            request_id: Some("7c1e".into()),
            parent_request_id: Some(Uuid::nil()),
            until_ms: Some(10),
            traced_only: Some(true),
            ..log_query()
        };
        let Filter { sql, binds } = log_filter_sql(&full, 0);
        assert_eq!(sql.matches('?').count(), binds.len());
        // The window's four, then eight filters.
        assert_eq!(binds.len(), 12);
        assert!(sql.contains("status_code >= 400"));
        assert!(sql.contains("status_code = 502"));
        assert!(sql.contains("FROM spans"));
    }

    #[test]
    fn log_filters_match_an_address_or_a_range() {
        let with = |ip: &str| {
            log_filter_sql(
                &UsageLogQuery {
                    client_ip: Some(ip.into()),
                    ..log_query()
                },
                0,
            )
        };
        let bound = |binds: &[Bind]| -> Vec<String> {
            Filter {
                sql: String::new(),
                binds: binds.to_vec(),
            }
            .texts()
        };

        let Filter { sql, binds } = with(" 203.0.113.7 ");
        assert!(sql.contains(" and client_ip = ?"));
        assert_eq!(bound(&binds), vec!["203.0.113.7"]);

        let Filter { sql, binds } = with("10.1.2.3/8");
        assert!(sql.contains("toIPv4OrNull(client_ip) between toIPv4OrNull(?) and toIPv4OrNull(?)"));
        assert_eq!(bound(&binds), vec!["10.0.0.0", "10.255.255.255"]);
        assert_eq!(sql.matches('?').count(), binds.len());

        let Filter { sql, binds } = with("2001:db8::/48");
        assert!(sql.contains("toIPv6OrNull(client_ip) between"));
        assert_eq!(sql.matches('?').count(), binds.len());

        let Filter { sql, binds } = with("not an address");
        assert!(sql.ends_with(" and 0"));
        assert_eq!(sql.matches('?').count(), binds.len());
    }

    #[test]
    fn log_filters_take_comma_lists() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let q: UsageLogQuery = serde_json::from_value(serde_json::json!({
            "key_id": format!("{a}, {b},"),
            "model": "glm-5-3",
        }))
        .expect("parses");
        assert_eq!(q.key_id, vec![a, b]);
        assert!(q.tenant_id.is_empty());
        let Filter { sql, binds } = log_filter_sql(&q, 0);
        assert!(sql.contains(" and key_id in (toUUID(?), toUUID(?))"));
        assert!(sql.contains(" and model = ?"));
        assert_eq!(sql.matches('?').count(), binds.len());

        let bad = serde_json::json!({ "key_id": format!("{a},not-a-uuid") });
        assert!(serde_json::from_value::<UsageLogQuery>(bad).is_err());
        let many = vec![a.to_string(); MAX_LOG_FILTER_VALUES + 1].join(",");
        let too_many = serde_json::json!({ "key_id": many });
        assert!(serde_json::from_value::<UsageLogQuery>(too_many).is_err());
    }

    #[test]
    fn helper_rows_are_fetched_by_their_parent_request() {
        let parent = Uuid::new_v4();
        let Filter { sql, binds } = log_filter_sql(
            &UsageLogQuery {
                parent_request_id: Some(parent),
                ..log_query()
            },
            0,
        );
        assert!(sql.contains(" and parent_request_id = toUUID(?)"));
        assert_eq!(sql.matches('?').count(), binds.len());
        assert!(matches!(binds.last(), Some(Bind::Text(v)) if v == &parent.to_string()));
    }

    #[test]
    fn log_columns_follow_the_row_field_order() {
        // `UsageLogRow` is read by position: a column out of place would land
        // in the wrong field (or fail the read) without any name check.
        let select = usage_log_select().replace(obleth_config::EFFECTIVE_CLASS_SQL, "class_expr");
        let select = select
            .trim_start_matches("select ")
            .split(" from ")
            .next()
            .unwrap();
        let columns: Vec<&str> = select
            .split(',')
            .map(|c| c.rsplit(" as ").next().unwrap().trim())
            .collect();
        assert_eq!(
            columns,
            [
                "request_id",
                "ts_ms",
                "tenant_id",
                "key_id",
                "model",
                "request_type",
                "session_id",
                "session_id_source",
                "device_id",
                "admission",
                "status_code",
                "input_tokens",
                "output_tokens",
                "total_tokens",
                "cached_input_tokens",
                "queue_wait_ms",
                "ttft_ms",
                "total_ms",
                "cache_status",
                "cost_usd",
                "energy_wh",
                "energy_cost_usd",
                "co2_g",
                "parent_request_id",
                "model_variant",
                "reasoning_effort",
                "reasoning_effort_requested",
                "model_family",
                "recorded_key_name",
                "recorded_tenant_name",
                "streamed",
                "error_reason",
                "filed_class",
                "client_ip",
                "client_country",
                "user_agent",
            ]
        );
    }

    #[test]
    fn failure_reads_file_old_rows_by_status_and_split_by_side() {
        let counts = failure_counts_sql(" where ts_ms >= ?");
        assert_no_shadowing(&counts);
        assert_eq!(select_items(&counts).len(), 3);
        let groups = failure_groups_sql(" where ts_ms >= ?", 1, FAILURE_GROUP_LIMIT);
        assert!(groups.contains(" and status_code >= 400 group by"));
        assert!(groups.contains(" having failed >= 1 order by"));
        assert!(groups.ends_with("limit 50"));
        assert!(groups.contains(obleth_config::EFFECTIVE_CLASS_SQL));
        assert!(!groups.contains(" as error_class"));
        assert_eq!(groups.matches('?').count(), 1);

        let q = |fault: Option<&str>, classes: &[&str]| UsageLogQuery {
            fault: fault.map(str::to_string),
            error_class: classes.iter().map(|c| c.to_string()).collect(),
            ..log_query()
        };
        let Filter { sql, binds } = log_filter_sql(&q(Some("platform"), &[]), 0);
        assert!(sql.ends_with(&format!(
            " and {} in {}",
            obleth_config::EFFECTIVE_CLASS_SQL,
            obleth_config::PLATFORM_CLASSES_SQL
        )));
        assert_eq!(sql.matches('?').count(), binds.len());
        let sql = log_filter_sql(&q(Some("client"), &[]), 0).sql;
        assert!(sql.contains(" and status_code >= 400 and multiIf("));
        assert!(sql.contains(" not in ('capacity'"));
        let Filter { sql, binds } = log_filter_sql(&q(None, &["capacity", "network"]), 0);
        assert!(sql.ends_with(" in (?, ?)"));
        assert_eq!(sql.matches('?').count(), binds.len());
    }

    #[test]
    fn log_facets_read_in_one_query_that_keeps_the_filters() {
        let Filter { sql: filter, binds } = log_filter_sql(
            &UsageLogQuery {
                model: vec!["m".into()],
                ..log_query()
            },
            0,
        );
        let sql = usage_log_facets_sql(&filter);
        // g, six dimensions, the failure pair, two counts.
        let items = select_items(&sql);
        assert_eq!(items.len(), 11);
        // `grouping(...)` names the grouping columns by alias, as it must;
        // no other item may read one.
        for (expr, alias) in items.iter().filter(|(_, a)| a != "g") {
            for (_, other) in &items {
                assert!(
                    other == alias || !reads(expr, other),
                    "alias `{other}` shadows a column that `{alias}` reads: {expr}"
                );
            }
        }
        assert!(sql.contains(" and model = ?"));
        assert_eq!(sql.matches('?').count(), binds.len());
        assert!(sql.contains("having g != 252 or fail_status != 0"), "{sql}");
        assert!(sql.ends_with("limit 12 by g"), "{sql}");
        // The first grouping column is the highest bit.
        assert_eq!(facet_mask(&[0]), 0b0111_1111);
        assert_eq!(facet_mask(&[5]), 0b1111_1011);
        assert_eq!(facet_mask(&[6, 7]), 252);
    }

    #[test]
    fn log_histogram_buckets_stay_bounded() {
        // The default minute, raised to a second at least.
        assert_eq!(log_histogram_bucket_ms(0, 3_600_000, None), 60_000);
        assert_eq!(log_histogram_bucket_ms(0, 60_000, Some(10)), 1_000);
        // Ninety days of minutes would be 129,600 buckets: widened to fit 400.
        let wide = log_histogram_bucket_ms(0, 90 * 86_400_000, None);
        assert!((90 * 86_400_000) / wide < MAX_LOG_HISTOGRAM_BUCKETS);
    }

    #[test]
    fn the_check_catches_the_bug_it_exists_for() {
        let bad = "select sum(input_tokens) + sum(output_tokens) as total_tokens, \
                   sum(input_tokens) as input_tokens from usage";
        assert!(std::panic::catch_unwind(|| assert_no_shadowing(bad)).is_err());
    }
}

/// Run against a real ClickHouse by setting `OBLETH_TEST_CLICKHOUSE_URL`
/// (e.g. `http://127.0.0.1:18123`); without it this returns early. Works in
/// its own uniquely named database and drops only that database.
#[cfg(test)]
mod clickhouse_tests {
    use super::*;
    use obleth_config::{KeyRejection, KeyRejectionReason, UsageRecord};

    fn usage(key: Uuid, ts_ms: i64, ip: &str, network: &str, country: &str) -> UsageRecord {
        let mut rec: UsageRecord = serde_json::from_value(serde_json::json!({
            "request_id": Uuid::new_v4(), "tenant_id": Uuid::nil(), "key_id": key,
            "model": "m", "admission": "fast", "weight": 1, "input_tokens": 1,
            "output_tokens": 1, "estimated_tokens": 2, "queue_wait_ms": 0, "ttft_ms": 0,
            "total_ms": 1, "status_code": 200, "cache_status": "off",
            "ts_ms": ts_ms, "request_type": "chat", "key_name": "alice"
        }))
        .unwrap();
        rec.client_ip = ip.into();
        rec.client_network = network.into();
        rec.client_country = country.into();
        rec.user_agent = "curl/8.0".into();
        rec
    }

    #[tokio::test]
    async fn key_sources_and_rejections_read_what_the_gateway_wrote() {
        let Ok(url) = std::env::var("OBLETH_TEST_CLICKHOUSE_URL") else {
            eprintln!("skipping: set OBLETH_TEST_CLICKHOUSE_URL to run");
            return;
        };
        let database = format!("obleth_test_{}", Uuid::new_v4().simple());
        let dir = std::env::temp_dir().join(format!("obleth-keywatch-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let sink = obleth_telemetry::TelemetrySink::start(
            &url,
            &database,
            "default",
            "",
            dir.join("usage.wal").to_str().unwrap(),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("sink");
        let key = Uuid::new_v4();
        let now = now_ms();
        let day = 86_400_000;
        // Seen from DE ten days ago (before the window), and from a new
        // network in NL inside it.
        sink.record(usage(
            key,
            now - 10 * day,
            "203.0.113.9",
            "203.0.113.0/24",
            "DE",
        ));
        sink.record(usage(
            key,
            now - 1000,
            "203.0.113.10",
            "203.0.113.0/24",
            "DE",
        ));
        sink.record(usage(
            key,
            now - 900,
            "198.51.100.4",
            "198.51.100.0/24",
            "NL",
        ));
        sink.record(usage(
            key,
            now - 800,
            "198.51.100.5",
            "198.51.100.0/24",
            "NL",
        ));
        // An old row with no address is skipped.
        sink.record(usage(key, now - 700, "", "", ""));
        for _ in 0..3 {
            sink.record_rejection(KeyRejection {
                ts_ms: now - 500,
                key_id: key,
                key_name: "alice".into(),
                tenant_id: Uuid::nil(),
                tenant_name: "t".into(),
                reason: KeyRejectionReason::IpNotAllowed,
                client_ip: "192.0.2.1".into(),
                client_country: "US".into(),
                user_agent: "bot/1".into(),
                path: "/v1/chat/completions".into(),
                count: 1,
            });
        }
        sink.shutdown().await;

        let client = clickhouse::Client::default()
            .with_url(&url)
            .with_database(&database);
        let q = KeyWatchQuery {
            since_ms: None,
            until_ms: None,
            tenant_id: Some(Uuid::nil()),
            key_id: vec![key],
            limit: None,
            per_key: None,
        };
        let (since, until) = q.window(now + 1000).unwrap();
        let sources = query_key_sources(&client, &q, since, until)
            .await
            .expect("key sources");
        assert!(!sources.truncated);
        let sources = sources.rows;
        assert_eq!(sources.len(), 2, "{sources:?}");
        let de = sources.iter().find(|s| s.client_country == "DE").unwrap();
        assert_eq!((de.requests, de.distinct_ips), (1, 1));
        assert_eq!(de.first_seen_ms, Some(now - 10 * day), "first seen ever");
        assert_eq!(de.network_first_seen_ms, Some(now - 10 * day));
        let nl = sources.iter().find(|s| s.client_country == "NL").unwrap();
        assert_eq!((nl.requests, nl.distinct_ips), (2, 2));
        assert_eq!(nl.network, "198.51.100.0/24");
        assert_eq!(nl.first_seen_ms, Some(now - 900));
        assert_eq!(nl.last_seen_ms, now - 800);
        assert_eq!(nl.key_name, "alice");

        let rejections = query_key_rejections(&client, &q, since, until)
            .await
            .expect("key rejections");
        assert!(!rejections.truncated);
        let rejections = rejections.rows;
        assert_eq!(rejections.len(), 1, "{rejections:?}");
        assert_eq!(rejections[0].count, 3);
        assert_eq!(rejections[0].reason, "ip_not_allowed");
        assert_eq!(rejections[0].sample_user_agent, "bot/1");

        client
            .query(&format!("DROP DATABASE IF EXISTS {database}"))
            .execute()
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Streamed, buffered, failed and cached replies of one model: the
    /// Overview totals, the model reads and the daily rollup all average
    /// first token over the same two requests.
    #[tokio::test]
    async fn every_first_token_average_counts_the_same_requests() {
        let Ok(url) = std::env::var("OBLETH_TEST_CLICKHOUSE_URL") else {
            eprintln!("skipping: set OBLETH_TEST_CLICKHOUSE_URL to run");
            return;
        };
        let database = format!("obleth_test_{}", Uuid::new_v4().simple());
        let dir = std::env::temp_dir().join(format!("obleth-ttft-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let sink = obleth_telemetry::TelemetrySink::start(
            &url,
            &database,
            "default",
            "",
            dir.join("usage.wal").to_str().unwrap(),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("sink");
        let key = Uuid::new_v4();
        let now = now_ms();
        let reply = |ttft_ms: u32, streamed: bool, status_code: u16, cache: &str| {
            let mut rec = usage(key, now - 60_000, "203.0.113.9", "203.0.113.0/24", "DE");
            rec.model = "t".into();
            rec.ttft_ms = ttft_ms;
            rec.total_ms = 2_000;
            rec.streamed = streamed;
            rec.status_code = status_code;
            rec.cache_status = cache.into();
            rec
        };
        // Counted: two streamed answers.
        sink.record(reply(200, true, 200, "miss"));
        sink.record(reply(400, true, 200, "miss"));
        // Not counted: a buffered answer, a failure and a cache hit.
        sink.record(reply(0, false, 200, "miss"));
        sink.record(reply(1_500, true, 502, "miss"));
        sink.record(reply(5, true, 200, "hit"));
        sink.shutdown().await;
        let client = clickhouse::Client::default()
            .with_url(&url)
            .with_database(&database);

        let q = |extra: serde_json::Value| -> UsageQuery {
            let mut v = serde_json::json!({ "since_ms": now - 3_600_000, "model": "t" });
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value(v).unwrap()
        };
        let totals = query_usage_totals(&client, &q(serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(totals.requests, 5);
        assert_eq!(totals.avg_ttft_ms, 300.0);
        assert_eq!(totals.p50_ttft_ms, 300.0);
        let model = query_usage_by_model(&client, q(serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(model[0].avg_ttft_ms, 300.0);
        let series: UsageSeriesQuery = serde_json::from_value(serde_json::json!({
            "since_ms": now - 3_600_000, "model": "t", "bucket_ms": 3_600_000,
        }))
        .unwrap();
        let series = query_usage_series_by_model(&client, series).await.unwrap();
        let avg: Vec<f64> = series
            .iter()
            .filter(|p| p.requests > 0)
            .map(|p| p.avg_ttft_ms)
            .collect();
        assert_eq!(avg, vec![300.0]);
        let today = chrono::Utc::now().date_naive();
        let daily: UsageDailyQuery = serde_json::from_value(serde_json::json!({
            "start_day": (today - chrono::Days::new(1)).to_string(),
            "end_day": today.to_string(),
            "group_by": "model",
        }))
        .unwrap();
        let daily = query_usage_daily(&client, daily).await.unwrap();
        assert_eq!(daily.len(), 1, "{daily:?}");
        assert_eq!((daily[0].avg_ttft_ms, daily[0].ttft_requests), (300.0, 2));

        // A day rolled up before the count existed divides by its answered
        // requests, as it was read then.
        client
            .query(
                "insert into usage_daily (day, tenant_id, key_id, model, requests, \
                 success_requests, ttft_ms_sum) values (toDate('2026-01-01'), \
                 toUUID('00000000-0000-0000-0000-000000000000'), toUUID(?), 'old', 4, 4, 2000)",
            )
            .bind(key.to_string())
            .execute()
            .await
            .unwrap();
        let old: UsageDailyQuery = serde_json::from_value(serde_json::json!({
            "start_day": "2026-01-01", "end_day": "2026-01-01", "group_by": "model",
        }))
        .unwrap();
        let old = query_usage_daily(&client, old).await.unwrap();
        assert_eq!((old[0].avg_ttft_ms, old[0].ttft_requests), (500.0, 4));

        client
            .query(&format!("DROP DATABASE IF EXISTS {database}"))
            .execute()
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every aggregate read runs against the real schema, reads the window
    /// `[since, until)` with `until` exclusive, and honours its filters.
    /// The Overview's two scans report what the six reads it replaces
    /// report: totals of both windows, cache counts, the series, per-model
    /// and per-tenant rows.
    #[tokio::test]
    async fn the_overview_read_matches_the_reads_it_replaces() {
        let Ok(url) = std::env::var("OBLETH_TEST_CLICKHOUSE_URL") else {
            eprintln!("skipping: set OBLETH_TEST_CLICKHOUSE_URL to run");
            return;
        };
        let database = format!("obleth_test_{}", Uuid::new_v4().simple());
        let dir = std::env::temp_dir().join(format!("obleth-overview-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let sink = obleth_telemetry::TelemetrySink::start(
            &url,
            &database,
            "default",
            "",
            dir.join("usage.wal").to_str().unwrap(),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("sink");
        let minute = 60_000;
        let now = now_ms();
        let tenants = [Uuid::new_v4(), Uuid::new_v4(), Uuid::nil()];
        // Five hours of traffic: the window is the last two, the previous
        // window the two before, and the first hour is outside both.
        for i in 0..60i64 {
            let mut rec = usage(
                Uuid::new_v4(),
                now - 300 * minute + i * 5 * minute,
                "",
                "",
                "",
            );
            rec.tenant_id = tenants[(i % 3) as usize];
            rec.model = ["a", "b", "c", "d"][(i % 4) as usize].into();
            rec.input_tokens = 10 + i as u32;
            rec.output_tokens = 5 + (i % 7) as u32;
            rec.ttft_ms = (40 + i * 3) as u32;
            rec.total_ms = (200 + i * 11) as u32;
            // Binary fractions, so sums are exact in any order.
            rec.cost_usd = i as f64 / 4.0;
            rec.energy_wh = i as f64 / 8.0;
            rec.status_code = if i % 9 == 0 {
                502
            } else if i % 11 == 0 {
                400
            } else {
                200
            };
            rec.cache_status = ["hit", "miss", "off"][(i % 3) as usize].into();
            sink.record(rec);
        }
        sink.shutdown().await;
        let client = clickhouse::Client::default()
            .with_url(&url)
            .with_database(&database);

        let (since, until, bucket) = (now - 120 * minute, now, 10 * minute);
        let ov = query_overview(
            &client,
            &OverviewQuery {
                since_ms: Some(since),
                until_ms: Some(until),
                bucket_ms: Some(bucket),
                include_internal: None,
            },
        )
        .await
        .expect("overview");
        assert_eq!(ov.previous_since_ms, now - 240 * minute);
        let window = || UsageQuery {
            since_ms: Some(since),
            until_ms: Some(until),
            ..Default::default()
        };
        let before = UsageQuery {
            since_ms: Some(ov.previous_since_ms),
            until_ms: Some(since),
            ..Default::default()
        };
        fn json<T: Serialize>(v: &T) -> serde_json::Value {
            serde_json::to_value(v).unwrap()
        }
        let current = query_usage_totals(&client, &window()).await.unwrap();
        let previous = query_usage_totals(&client, &before).await.unwrap();
        assert!(current.requests > 0 && previous.requests > 0);
        assert_eq!(json(&ov.current), json(&current));
        assert_eq!(json(&ov.previous), json(&previous));
        let cache = query_cache_stats(&client, window()).await.unwrap();
        assert_eq!(json(&ov.cache), json(&cache));
        let series = query_usage_series(
            &client,
            UsageSeriesQuery {
                since_ms: Some(ov.previous_since_ms),
                until_ms: Some(until),
                bucket_ms: Some(bucket),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(ov.bucket_ms, bucket);
        assert_eq!(json(&ov.series), json(&series));
        let models = query_usage_by_model(&client, window()).await.unwrap();
        assert_eq!(json(&ov.models), json(&models));
        let mut per_tenant: Vec<_> = query_usage(&client, window())
            .await
            .unwrap()
            .into_iter()
            .map(|t| (t.tenant_id, t.requests, t.total_tokens, t.cost_usd))
            .collect();
        per_tenant.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        assert_eq!(ov.tenants, per_tenant);
        assert!(
            ov.tenants.iter().any(|t| t.0 == Uuid::nil()),
            "the nil tenant is a tenant row"
        );

        clickhouse::Client::default()
            .with_url(&url)
            .query(&format!("drop database if exists {database}"))
            .execute()
            .await
            .ok();
    }

    /// A row of the one-facet-per-query read the single scan replaced.
    #[derive(Debug, Row, Deserialize)]
    struct UsageLogFacetRow {
        facet: String,
        requests: u64,
        errors: u64,
    }

    /// The one-scan facets read returns what one query per facet (and one
    /// for failures) returned.
    #[tokio::test]
    async fn log_facets_match_one_query_per_facet() {
        let Ok(url) = std::env::var("OBLETH_TEST_CLICKHOUSE_URL") else {
            eprintln!("skipping: set OBLETH_TEST_CLICKHOUSE_URL to run");
            return;
        };
        let database = format!("obleth_test_{}", Uuid::new_v4().simple());
        let dir = std::env::temp_dir().join(format!("obleth-facets-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let sink = obleth_telemetry::TelemetrySink::start(
            &url,
            &database,
            "default",
            "",
            dir.join("usage.wal").to_str().unwrap(),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("sink");
        let now = now_ms();
        let tenants: Vec<Uuid> = (0..3).map(|_| Uuid::new_v4()).collect();
        // More models and keys than a facet lists, so the limit applies.
        for i in 0..200i64 {
            let mut rec = usage(
                Uuid::from_u128((i % 15) as u128 + 1),
                now - 1000 - i,
                "",
                "",
                "",
            );
            rec.tenant_id = tenants[(i % 3) as usize];
            rec.model = format!("m{}", i % 17);
            rec.request_type = ["chat", "embeddings", "completions"][(i % 3) as usize].into();
            rec.status_code = [200, 200, 200, 429, 500, 502, 400][(i % 7) as usize];
            sink.record(rec);
        }
        sink.shutdown().await;
        let client = clickhouse::Client::default()
            .with_url(&url)
            .with_database(&database);
        let q = || UsageLogQuery {
            since_ms: Some(now - 60_000),
            ..super::alias_tests::log_query()
        };
        let facets = query_usage_log_facets(&client, q()).await.expect("facets");

        let filter = log_filter_sql(&q(), now - 60_000);
        let one = |expr: &str| {
            let sql = format!(
                "select {expr} as facet, count() as requests, countIf(status_code >= 400) as errors \
                 from usage{} group by facet order by requests desc, facet limit {LOG_FACET_LIMIT}",
                filter.sql()
            );
            let query = filter.bind(client.query(&sql));
            async move { query.fetch_all::<UsageLogFacetRow>().await.unwrap() }
        };
        let got = [
            &facets.status_codes,
            &facets.models,
            &facets.tenants,
            &facets.keys,
            &facets.request_types,
            &facets.error_classes,
        ];
        for ((name, expr), got) in LOG_FACET_DIMENSIONS.iter().zip(got) {
            let want: Vec<(String, u64, u64)> = one(expr)
                .await
                .into_iter()
                .map(|r| (r.facet, r.requests, r.errors))
                .collect();
            let got: Vec<(String, u64, u64)> = got
                .iter()
                .map(|f| (f.value.clone(), f.requests, f.errors))
                .collect();
            assert_eq!(got, want, "{name}");
        }
        assert_eq!(facets.models.len(), LOG_FACET_LIMIT as usize);
        let failures_sql = format!(
            "select status_code, model, count() as requests from usage{} and status_code >= 400 \
             group by status_code, model order by requests desc, model, status_code limit {LOG_FACET_LIMIT}",
            filter.sql()
        );
        let want: Vec<(u16, String, u64)> = filter
            .bind(client.query(&failures_sql))
            .fetch_all::<UsageLogFailure>()
            .await
            .unwrap()
            .into_iter()
            .map(|f| (f.status_code, f.model, f.requests))
            .collect();
        let got: Vec<(u16, String, u64)> = facets
            .failures
            .iter()
            .map(|f| (f.status_code, f.model.clone(), f.requests))
            .collect();
        assert_eq!(got, want);
        assert!(got.iter().all(|f| f.0 >= 400));

        clickhouse::Client::default()
            .with_url(&url)
            .query(&format!("drop database if exists {database}"))
            .execute()
            .await
            .ok();
    }

    /// The two-step log read pages through the same rows, in the same order,
    /// as one read of every column did, ties on the millisecond included.
    #[tokio::test]
    async fn log_pages_match_a_single_full_read() {
        let Ok(url) = std::env::var("OBLETH_TEST_CLICKHOUSE_URL") else {
            eprintln!("skipping: set OBLETH_TEST_CLICKHOUSE_URL to run");
            return;
        };
        let database = format!("obleth_test_{}", Uuid::new_v4().simple());
        let dir = std::env::temp_dir().join(format!("obleth-logs-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let sink = obleth_telemetry::TelemetrySink::start(
            &url,
            &database,
            "default",
            "",
            dir.join("usage.wal").to_str().unwrap(),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("sink");
        let now = now_ms();
        for i in 0..40i64 {
            // Three rows per millisecond, and every fourth row another model.
            let mut rec = usage(Uuid::new_v4(), now - 5_000 + i / 3, "", "", "");
            rec.model = if i % 4 == 0 {
                "other".into()
            } else {
                "m".into()
            };
            rec.status_code = if i % 5 == 0 { 500 } else { 200 };
            sink.record(rec);
        }
        sink.shutdown().await;
        let client = clickhouse::Client::default()
            .with_url(&url)
            .with_database(&database);
        let query = |before: Option<(i64, Uuid)>| UsageLogQuery {
            since_ms: Some(now - 60_000),
            model: vec!["m".into()],
            limit: Some(7),
            before_ms: before.map(|b| b.0),
            before_request_id: before.map(|b| b.1),
            ..super::alias_tests::log_query()
        };
        let single = |before: Option<(i64, Uuid)>| {
            let q = query(before);
            let filter = log_filter_sql(&q, now - 60_000);
            let mut sql = format!("{}{}", usage_log_select(), filter.sql());
            if before.is_some() {
                sql.push_str(" and (ts_ms, toString(request_id)) < (?, ?)");
            }
            sql.push_str(&format!(" {LOG_ORDER} limit 7"));
            let mut read = filter.bind(client.query(&sql));
            if let Some((ms, id)) = before {
                read = read.bind(ms).bind(id.to_string());
            }
            async move { read.fetch_all::<UsageLogRow>().await.unwrap() }
        };
        let json = |rows: &[UsageLogRow]| serde_json::to_value(rows).unwrap();
        let mut cursor = None;
        let mut seen = 0;
        loop {
            let page = query_usage_logs(&client, query(cursor)).await.unwrap();
            let got = page.rows;
            let want = single(cursor).await;
            assert_eq!(json(&got), json(&want));
            assert_eq!(
                page.more,
                seen + got.len() < 30,
                "more follow until the last page"
            );
            seen += got.len();
            let Some(last) = got.last() else { break };
            cursor = Some((last.ts_ms, last.request_id));
        }
        assert_eq!(seen, 30, "every row of model m, once");

        clickhouse::Client::default()
            .with_url(&url)
            .query(&format!("drop database if exists {database}"))
            .execute()
            .await
            .ok();
    }

    #[tokio::test]
    async fn usage_reads_run_on_clickhouse_and_honour_the_window() {
        let Ok(url) = std::env::var("OBLETH_TEST_CLICKHOUSE_URL") else {
            eprintln!("skipping: set OBLETH_TEST_CLICKHOUSE_URL to run");
            return;
        };
        let database = format!("obleth_test_{}", Uuid::new_v4().simple());
        let dir = std::env::temp_dir().join(format!("obleth-usage-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let sink = obleth_telemetry::TelemetrySink::start(
            &url,
            &database,
            "default",
            "",
            dir.join("usage.wal").to_str().unwrap(),
            std::time::Duration::from_millis(50),
        )
        .await
        .expect("sink");
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let hour = 3_600_000;
        let now = now_ms();
        for at in [now - 3 * hour, now - 2 * hour, now - hour] {
            sink.record(usage(a, at, "203.0.113.9", "203.0.113.0/24", "DE"));
        }
        let mut failed = usage(b, now - hour / 2, "198.51.100.4", "198.51.100.0/24", "NL");
        failed.model = "n".into();
        failed.status_code = 500;
        sink.record(failed);
        // Ten days back: outside every window below.
        sink.record(usage(
            a,
            now - 240 * hour,
            "203.0.113.9",
            "203.0.113.0/24",
            "DE",
        ));
        sink.shutdown().await;
        let client = clickhouse::Client::default()
            .with_url(&url)
            .with_database(&database);

        let since = now - 4 * hour;
        let until = now - hour;
        let query = |extra: serde_json::Value| -> UsageQuery {
            let mut v = serde_json::json!({ "since_ms": since });
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value(v).expect("usage query")
        };
        let totals = |q: UsageQuery| {
            let client = client.clone();
            async move { query_usage_totals(&client, &q).await.expect("totals") }
        };
        assert_eq!(totals(query(serde_json::json!({}))).await.requests, 4);
        // The row at `until` is out: the bound is exclusive.
        let bounded = query(serde_json::json!({ "until_ms": until }));
        assert_eq!(totals(bounded).await.requests, 2);
        let only_a = query(serde_json::json!({ "key_id": a }));
        assert_eq!(totals(only_a).await.requests, 3);
        // `since` defaults to a day before `until`, so the old row stays out.
        let day_before = query(serde_json::json!({ "until_ms": now }));
        let day_before = UsageQuery {
            since_ms: None,
            ..day_before
        };
        assert_eq!(totals(day_before).await.requests, 4);

        let tenants = query_usage(&client, query(serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(tenants.iter().map(|r| r.requests).sum::<u64>(), 4);
        let keys = query_usage_by_key(&client, query(serde_json::json!({ "limit": 1 })))
            .await
            .unwrap();
        assert_eq!(keys.len(), 1);
        assert_eq!((keys[0].key_id, keys[0].requests), (a, 3));
        let models = query_usage_by_model(&client, query(serde_json::json!({ "until_ms": until })))
            .await
            .unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!((models[0].model.as_str(), models[0].requests), ("m", 2));
        let cache = query_cache_stats(&client, query(serde_json::json!({ "key_id": b })))
            .await
            .unwrap();
        assert_eq!(cache.hits, 0);
        let costs = query_costs(
            &client,
            &query(serde_json::json!({ "model": "m" })),
            &[("m".into(), 1.0, 2.0)],
        )
        .await
        .unwrap();
        assert_eq!(costs.len(), 1);
        assert_eq!(costs[0].requests, 3);

        let series = |extra: serde_json::Value| -> UsageSeriesQuery {
            let mut v = serde_json::json!({ "since_ms": since, "bucket_ms": hour });
            v.as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            serde_json::from_value(v).expect("series query")
        };
        let sum = |points: &[UsageTimePoint]| points.iter().map(|p| p.requests).sum::<u64>();
        let all = query_usage_series(&client, series(serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(sum(&all), 4);
        let cut = query_usage_series(&client, series(serde_json::json!({ "until_ms": until })))
            .await
            .unwrap();
        assert_eq!(sum(&cut), 2);
        let by_tenant = query_usage_series_by_tenant(&client, series(serde_json::json!({})))
            .await
            .unwrap();
        assert_eq!(by_tenant.iter().map(|p| p.requests).sum::<u64>(), 4);
        let by_model =
            query_usage_series_by_model(&client, series(serde_json::json!({ "model": "n" })))
                .await
                .unwrap();
        assert_eq!(by_model.iter().map(|p| p.requests).sum::<u64>(), 1);

        let breakdown: UsageBreakdownQuery = serde_json::from_value(
            serde_json::json!({ "model": "m", "since_ms": since, "until_ms": until }),
        )
        .unwrap();
        let breakdown = query_usage_breakdown_by_model(&client, &breakdown)
            .await
            .unwrap();
        assert_eq!(breakdown.iter().map(|r| r.requests).sum::<u64>(), 2);

        let speed: UsageSpeedQuery = serde_json::from_value(serde_json::json!({
            "since_ms": since, "until_ms": now, "bucket": "hour", "tz": "UTC", "key_id": a,
        }))
        .unwrap();
        let speed = query_usage_speed(&client, speed).await.unwrap();
        assert_eq!(speed.iter().map(|p| p.requests).sum::<u64>(), 3);

        let summary: KeyUsageSummaryQuery =
            serde_json::from_value(serde_json::json!({ "since_ms": since, "until_ms": until }))
                .unwrap();
        let summary = query_keys_usage_summary(&client, summary).await.unwrap();
        assert_eq!(summary.iter().map(|r| r.requests).sum::<u64>(), 2);

        let log = || -> UsageLogQuery {
            serde_json::from_value(
                serde_json::json!({ "since_ms": since, "until_ms": now, "model": "n" }),
            )
            .unwrap()
        };
        let page = query_usage_logs(&client, log()).await.unwrap();
        assert_eq!((page.rows.len(), page.more), (1, false));
        let (_, buckets) = query_usage_log_histogram(&client, log()).await.unwrap();
        assert_eq!(buckets.iter().map(|b| b.errors).sum::<u64>(), 1);
        let facets = query_usage_log_facets(&client, log()).await.unwrap();
        assert_eq!(facets.models.len(), 1);
        let groups = query_usage_failure_groups(&client, log()).await.unwrap();
        assert_eq!((groups.requests, groups.failures), (1, 1));

        // Advice reads the same ledger through the same window.
        let advice = crate::advice::query_model_totals(&client, since)
            .await
            .unwrap();
        assert_eq!(advice.iter().map(|t| t.requests).sum::<u64>(), 4);
        let n = advice.iter().find(|t| t.model == "n").unwrap();
        assert_eq!(n.platform_failures, 1);
        crate::advice::query_failure_groups(&client, since)
            .await
            .expect("advice failure groups");

        let today = chrono::Utc::now().date_naive();
        let daily: UsageDailyQuery = serde_json::from_value(serde_json::json!({
            "start_day": (today - chrono::Days::new(1)).to_string(),
            "end_day": today.to_string(),
            "key_id": format!("{a},{b}"),
        }))
        .unwrap();
        let daily = query_usage_daily(&client, daily).await.unwrap();
        assert_eq!(daily.iter().map(|r| r.requests).sum::<u64>(), 4);

        client
            .query(&format!("DROP DATABASE IF EXISTS {database}"))
            .execute()
            .await
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
