//! The Keys list a page at a time: `GET /keys` filtered and sorted on the
//! server by any of the dashboard's orders, how many keys a filter matches,
//! and the Keys page's tiles.
//!
//! The created and name orders are Postgres keyset pages. The use orders
//! (last used, requests, tokens, spend in a window) rank keys in ClickHouse a
//! page at a time and read those keys from Postgres; the keys with no use in
//! the window follow, by name. The budget order ranks the capped keys by the
//! share of their cap used this period (from the enforcement counters, as
//! `GET /budgets/usage` reads them); the uncapped keys follow, by name.

use std::collections::{HashMap, HashSet};

use axum::extract::{Query, State};
use axum::Json;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use obleth_config::ApiKey;
use obleth_store::{KeyListFilter, KeyOrder, KeySeek, Store};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

use crate::error::AdminError;
use crate::usage::{self, KeyRankMetric};
use crate::{key_budget_reads, read_budgets, AdminState, ListKeysQuery};

type Result<T> = std::result::Result<T, AdminError>;

/// Share of a cap at which a key counts as near its budget.
pub(crate) const NEAR_BUDGET: f64 = 0.8;

/// The use window when none is given: 30 days back from its end.
const DEFAULT_WINDOW_MS: i64 = 30 * 86_400_000;

/// Most keys a filter may match for a use ranking to name them to
/// ClickHouse (`key_id in (…)`, about 100 KB of query text at most). A
/// broader filter ranks every key and drops the ones it leaves out.
const RANK_PUSHDOWN_MAX: usize = 2_000;

/// [`RANK_PUSHDOWN_MAX`], which a test lowers to rank without naming keys.
#[cfg(not(test))]
fn rank_pushdown_max() -> usize {
    RANK_PUSHDOWN_MAX
}

#[cfg(test)]
pub(crate) static TEST_RANK_PUSHDOWN_MAX: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(RANK_PUSHDOWN_MAX);

#[cfg(test)]
fn rank_pushdown_max() -> usize {
    TEST_RANK_PUSHDOWN_MAX.load(std::sync::atomic::Ordering::Relaxed)
}

/// Most rows one ranking round asks ClickHouse for.
const RANK_ROUND_MAX: u64 = 10_000;

/// Each ranking round after the first asks for this many times more rows,
/// when the filter left out most of the last round.
const RANK_ROUND_GROWTH: u64 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sort {
    Created,
    Name,
    Use(KeyRankMetric),
    Budget,
}

/// Where a page ended, in its order. Encoded into the opaque `cursor`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "at", rename_all = "snake_case")]
enum Position {
    /// Created order: `created_at` in microseconds, and id.
    Created { t: i64, id: Uuid },
    /// Name order: the name as stored, and id.
    Name { n: String, id: Uuid },
    /// A use order's ranked keys: the score ClickHouse ranked by, and id.
    Rank { v: i64, id: Uuid },
    /// The budget order's capped keys: the share used, and id.
    Share { v: f64, id: Uuid },
    /// The keys after a use or budget order's ranked ones, by name.
    Tail { n: String, id: Uuid },
}

#[derive(Debug, Serialize, Deserialize)]
struct Cursor {
    /// The order the cursor was made for (`requests`, `name:desc`, …).
    s: String,
    #[serde(flatten)]
    p: Position,
}

fn encode_cursor(order: &str, p: Position) -> String {
    let json = serde_json::to_vec(&Cursor {
        s: order.to_string(),
        p,
    })
    .expect("a cursor serialises");
    URL_SAFE_NO_PAD.encode(json)
}

fn decode_cursor(order: &str, raw: &str) -> Result<Position> {
    let bad = || AdminError::BadRequest("cursor is not one this list gave".into());
    let bytes = URL_SAFE_NO_PAD.decode(raw.trim()).map_err(|_| bad())?;
    let c: Cursor = serde_json::from_slice(&bytes).map_err(|_| bad())?;
    if c.s != order {
        return Err(AdminError::BadRequest(format!(
            "cursor is for sort {:?}, not {order:?}",
            c.s
        )));
    }
    Ok(c.p)
}

/// A `GET /keys` request made sense of: what to match and in what order.
struct Plan {
    sort: Sort,
    desc: bool,
    /// The order's name in cursors.
    order: String,
    filter: KeyListFilter,
    near: bool,
    unused: bool,
    since: i64,
    until: i64,
    position: Option<Position>,
}

impl Plan {
    fn new(q: &ListKeysQuery, now_ms: i64) -> Result<Self> {
        let sort = match q.sort.as_deref().map(str::trim).unwrap_or("created") {
            "" | "created" => Sort::Created,
            "name" => Sort::Name,
            "last_used" => Sort::Use(KeyRankMetric::LastUsed),
            "requests" => Sort::Use(KeyRankMetric::Requests),
            "tokens" => Sort::Use(KeyRankMetric::Tokens),
            "cost" => Sort::Use(KeyRankMetric::Cost),
            "budget" => Sort::Budget,
            other => {
                return Err(AdminError::BadRequest(format!(
                    "sort {other:?} is not one of created, name, last_used, requests, tokens, cost, budget"
                )))
            }
        };
        let desc = match q.dir.as_deref().map(str::trim) {
            None | Some("") => !matches!(sort, Sort::Created | Sort::Name),
            Some("asc") if matches!(sort, Sort::Created | Sort::Name) => false,
            Some("desc") => true,
            Some("asc") => {
                return Err(AdminError::BadRequest(
                    "the use and budget orders are highest first only (dir=desc)".into(),
                ))
            }
            Some(other) => {
                return Err(AdminError::BadRequest(format!(
                    "dir {other:?} is not asc or desc"
                )))
            }
        };
        let order = match sort {
            Sort::Created => "created",
            Sort::Name => "name",
            Sort::Use(KeyRankMetric::LastUsed) => "last_used",
            Sort::Use(KeyRankMetric::Requests) => "requests",
            Sort::Use(KeyRankMetric::Tokens) => "tokens",
            Sort::Use(KeyRankMetric::Cost) => "cost",
            Sort::Budget => "budget",
        };
        let order = if desc && matches!(sort, Sort::Created | Sort::Name) {
            format!("{order}:desc")
        } else {
            order.to_string()
        };
        if q.after.is_some() && (q.cursor.is_some() || sort != Sort::Created || desc) {
            return Err(AdminError::BadRequest(
                "after pages the default oldest-first order; other orders page with cursor".into(),
            ));
        }
        let position = q
            .cursor
            .as_deref()
            .filter(|c| !c.trim().is_empty())
            .map(|c| decode_cursor(&order, c))
            .transpose()?;
        let kind = match q.kind.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(k @ ("secret" | "identity")) => Some(k.to_string()),
            Some(other) => {
                return Err(AdminError::BadRequest(format!(
                    "kind {other:?} is not secret or identity"
                )))
            }
        };
        let (capped, near) = match q.budget.as_deref().map(str::trim) {
            None | Some("") => (None, false),
            Some("capped") => (Some(true), false),
            Some("none") => (Some(false), false),
            Some("near") => (Some(true), true),
            Some(other) => {
                return Err(AdminError::BadRequest(format!(
                    "budget {other:?} is not capped, none or near"
                )))
            }
        };
        let until = q.until_ms.unwrap_or(now_ms);
        let since = q.since_ms.unwrap_or(until - DEFAULT_WINDOW_MS);
        if since >= until {
            return Err(AdminError::BadRequest(
                "since_ms must be before until_ms".into(),
            ));
        }
        Ok(Plan {
            sort,
            desc,
            order,
            filter: KeyListFilter {
                tenant_id: q.tenant_id,
                ids: (!q.ids.is_empty()).then(|| q.ids.clone()),
                // The reserved control-plane key (Charo's) stays off the
                // management surface.
                exclude_tenant: Some(Store::CONTROL_PLANE_TENANT_ID),
                q: q.q.clone(),
                disabled: q.disabled,
                kind,
                capped,
                ..Default::default()
            },
            near,
            unused: q.unused == Some(true),
            since,
            until,
            position,
        })
    }
}

/// What a plan reads beyond Postgres, read once per request.
struct Reads<'a> {
    state: &'a AdminState,
    since: i64,
    until: i64,
    used: Option<Vec<Uuid>>,
}

impl Reads<'_> {
    /// Every key with use in the window.
    async fn used(&mut self) -> Result<Vec<Uuid>> {
        if self.used.is_none() {
            let rows =
                usage::query_used_keys(&self.state.clickhouse, self.since, self.until).await?;
            self.used = Some(rows.into_iter().map(|r| r.key_id).collect());
        }
        Ok(self.used.clone().unwrap_or_default())
    }
}

/// The capped keys matching `filter` with the share of their cap used this
/// period, highest first, ties by id (descending). A key whose budget has no
/// current period has share 0.
pub(crate) async fn budget_shares(
    state: &AdminState,
    filter: &KeyListFilter,
) -> Result<Vec<(f64, ApiKey)>> {
    if filter.capped == Some(false) {
        return Ok(Vec::new());
    }
    let keys = state
        .store
        .list_keys_where(&KeyListFilter {
            capped: Some(true),
            seek: None,
            limit: None,
            order: KeyOrder::Created,
            ..filter.clone()
        })
        .await?;
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let zones: HashMap<Uuid, String> = state
        .store
        .list_tenants()
        .await?
        .into_iter()
        .map(|t| (t.id, t.timezone))
        .collect();
    let now = chrono::Utc::now();
    let shares: HashMap<Uuid, f64> =
        read_budgets(state, &key_budget_reads(&keys, &zones, now), now)
            .await?
            .into_iter()
            .map(|b| (b.id, b.share()))
            .collect();
    let mut out: Vec<(f64, ApiKey)> = keys
        .into_iter()
        .map(|k| (shares.get(&k.id).copied().unwrap_or(0.0), k))
        .collect();
    out.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.id.cmp(&a.1.id)));
    Ok(out)
}

/// Narrow the plan's filter by what Postgres alone cannot tell: keys near
/// their budget, keys unused in the window. `None` when nothing can match.
async fn resolve_filter(plan: &Plan, reads: &mut Reads<'_>) -> Result<Option<KeyListFilter>> {
    let mut filter = plan.filter.clone();
    if plan.near {
        let near: Vec<Uuid> = budget_shares(reads.state, &filter)
            .await?
            .into_iter()
            .filter(|(share, _)| *share >= NEAR_BUDGET)
            .map(|(_, k)| k.id)
            .collect();
        if near.is_empty() {
            return Ok(None);
        }
        filter.ids = Some(near);
    }
    if plan.unused {
        filter.exclude_ids = Some(reads.used().await?);
    }
    Ok(Some(filter))
}

/// One page of `GET /keys`.
pub(crate) struct KeyPage {
    pub keys: Vec<ApiKey>,
    /// Whether keys past this page match (only when a limit was given).
    pub more: bool,
    /// Where the next page starts, when `more`.
    pub next: Option<String>,
}

/// The page of keys a `GET /keys` request asks for.
pub(crate) async fn list_page(
    state: &AdminState,
    q: &ListKeysQuery,
    limit: Option<i64>,
) -> Result<KeyPage> {
    let plan = Plan::new(q, usage::now_ms())?;
    let mut reads = Reads {
        state,
        since: plan.since,
        until: plan.until,
        used: None,
    };
    let Some(filter) = resolve_filter(&plan, &mut reads).await? else {
        return Ok(KeyPage {
            keys: Vec::new(),
            more: false,
            next: None,
        });
    };
    // One past the page, to tell whether more follow.
    let want = limit.map(|n| n as usize + 1);
    let mut rows: Vec<(ApiKey, Position)> = match plan.sort {
        Sort::Created | Sort::Name => metadata_rows(state, &plan, filter, q.after, want).await?,
        Sort::Use(metric) => use_rows(&plan, &mut reads, metric, filter, want).await?,
        Sort::Budget => budget_rows(state, &plan, filter, want).await?,
    };
    let more = want.is_some_and(|w| rows.len() >= w);
    if let Some(n) = limit {
        rows.truncate(n as usize);
    }
    let next = more
        .then(|| {
            rows.last()
                .map(|(_, p)| encode_cursor(&plan.order, p.clone()))
        })
        .flatten();
    Ok(KeyPage {
        keys: rows.into_iter().map(|(k, _)| k).collect(),
        more,
        next,
    })
}

/// How many keys a `GET /keys` request's filters match, and in how many
/// tenants.
pub(crate) async fn count(state: &AdminState, q: &ListKeysQuery) -> Result<(i64, i64)> {
    let plan = Plan::new(q, usage::now_ms())?;
    let mut reads = Reads {
        state,
        since: plan.since,
        until: plan.until,
        used: None,
    };
    match resolve_filter(&plan, &mut reads).await? {
        Some(filter) => Ok(state.store.count_keys_where(&filter).await?),
        None => Ok((0, 0)),
    }
}

fn wrong_cursor() -> AdminError {
    AdminError::BadRequest("cursor is not one this list gave".into())
}

/// The created and name orders: one keyset page from Postgres.
async fn metadata_rows(
    state: &AdminState,
    plan: &Plan,
    mut filter: KeyListFilter,
    after: Option<Uuid>,
    want: Option<usize>,
) -> Result<Vec<(ApiKey, Position)>> {
    let by_name = plan.sort == Sort::Name;
    filter.order = match (by_name, plan.desc) {
        (false, false) => KeyOrder::Created,
        (false, true) => KeyOrder::CreatedDesc,
        (true, false) => KeyOrder::Name,
        (true, true) => KeyOrder::NameDesc,
    };
    filter.seek = match (&plan.position, after) {
        (None, None) => None,
        (None, Some(id)) => Some(KeySeek::After(id)),
        (Some(Position::Created { t, id }), _) if !by_name => Some(KeySeek::Created(
            chrono::DateTime::from_timestamp_micros(*t).ok_or_else(wrong_cursor)?,
            *id,
        )),
        (Some(Position::Name { n, id }), _) if by_name => Some(KeySeek::Name(n.clone(), *id)),
        _ => return Err(wrong_cursor()),
    };
    filter.limit = want.map(|n| n as i64);
    let keys = state.store.list_keys_where(&filter).await?;
    Ok(keys
        .into_iter()
        .map(|k| {
            let p = if by_name {
                Position::Name {
                    n: k.name.clone(),
                    id: k.id,
                }
            } else {
                Position::Created {
                    t: k.created_at.timestamp_micros(),
                    id: k.id,
                }
            };
            (k, p)
        })
        .collect())
}

/// The keys after a ranking's ranked ones: those `filter` matches, by name,
/// from `from`, at most `want`.
async fn tail_rows(
    state: &AdminState,
    mut filter: KeyListFilter,
    from: Option<(String, Uuid)>,
    want: Option<usize>,
) -> Result<Vec<(ApiKey, Position)>> {
    filter.order = KeyOrder::Name;
    filter.seek = from.map(|(n, id)| KeySeek::Name(n, id));
    filter.limit = want.map(|n| n as i64);
    Ok(state
        .store
        .list_keys_where(&filter)
        .await?
        .into_iter()
        .map(|k| {
            let p = Position::Tail {
                n: k.name.clone(),
                id: k.id,
            };
            (k, p)
        })
        .collect())
}

/// A use order: ClickHouse ranks the keys with use in the window a round at
/// a time (the first round one page plus one), Postgres keeps those the
/// filter matches, and when the ranking runs out the unused keys follow by
/// name.
async fn use_rows(
    plan: &Plan,
    reads: &mut Reads<'_>,
    metric: KeyRankMetric,
    filter: KeyListFilter,
    want: Option<usize>,
) -> Result<Vec<(ApiKey, Position)>> {
    let state = reads.state;
    let (mut after, tail_from) = match &plan.position {
        None => (None, None),
        Some(Position::Rank { v, id }) => (Some((*v, *id)), None),
        Some(Position::Tail { n, id }) => (None, Some((n.clone(), *id))),
        Some(_) => return Err(wrong_cursor()),
    };
    let mut out: Vec<(ApiKey, Position)> = Vec::new();
    let full = |out: &Vec<(ApiKey, Position)>| want.is_some_and(|w| out.len() >= w);
    // Unused keys have no rank; a cursor in the tail is past the ranking.
    if !plan.unused && tail_from.is_none() {
        // A narrow filter names its keys to ClickHouse, so one round holds
        // the page; a broad one ranks every key and drops the rest here.
        let named = state
            .store
            .key_ids_where(&KeyListFilter {
                limit: Some(rank_pushdown_max() as i64 + 1),
                ..filter.clone()
            })
            .await?;
        let named = (named.len() <= rank_pushdown_max()).then_some(named);
        let allowed: Option<HashSet<Uuid>> =
            filter.ids.as_ref().map(|ids| ids.iter().copied().collect());
        if named.as_ref().is_some_and(|n| n.is_empty()) {
            return Ok(out);
        }
        let mut round = want.map(|w| w as u64);
        loop {
            let ranks = usage::query_key_ranks(
                &state.clickhouse,
                metric,
                plan.since,
                plan.until,
                named.as_deref().unwrap_or(&[]),
                after,
                round,
            )
            .await?;
            let exhausted = round.is_none_or(|n| (ranks.len() as u64) < n);
            if let Some(last) = ranks.last() {
                after = Some((last.score, last.key_id));
                let ids: Vec<Uuid> = ranks
                    .iter()
                    .map(|r| r.key_id)
                    .filter(|id| allowed.as_ref().is_none_or(|a| a.contains(id)))
                    .collect();
                let mut keys: HashMap<Uuid, ApiKey> = if ids.is_empty() {
                    HashMap::new()
                } else {
                    state
                        .store
                        .list_keys_where(&KeyListFilter {
                            ids: Some(ids),
                            ..filter.clone()
                        })
                        .await?
                        .into_iter()
                        .map(|k| (k.id, k))
                        .collect()
                };
                for r in &ranks {
                    if full(&out) {
                        break;
                    }
                    if let Some(k) = keys.remove(&r.key_id) {
                        out.push((
                            k,
                            Position::Rank {
                                v: r.score,
                                id: r.key_id,
                            },
                        ));
                    }
                }
            }
            if full(&out) || exhausted {
                break;
            }
            round = round.map(|n| (n * RANK_ROUND_GROWTH).min(RANK_ROUND_MAX));
        }
        if full(&out) {
            return Ok(out);
        }
    }
    let mut tail = filter;
    if !plan.unused {
        tail.exclude_ids = Some(reads.used().await?);
    }
    let left = want.map(|w| w - out.len());
    out.extend(tail_rows(state, tail, tail_from, left).await?);
    Ok(out)
}

/// The budget order: the capped keys by the share of their cap used, then
/// the uncapped ones by name.
async fn budget_rows(
    state: &AdminState,
    plan: &Plan,
    filter: KeyListFilter,
    want: Option<usize>,
) -> Result<Vec<(ApiKey, Position)>> {
    let (after, tail_from) = match &plan.position {
        None => (None, None),
        Some(Position::Share { v, id }) => (Some((*v, *id)), None),
        Some(Position::Tail { n, id }) => (None, Some((n.clone(), *id))),
        Some(_) => return Err(wrong_cursor()),
    };
    let mut out: Vec<(ApiKey, Position)> = Vec::new();
    if tail_from.is_none() {
        let ranked = budget_shares(state, &filter).await?;
        for (share, k) in ranked {
            if want.is_some_and(|w| out.len() >= w) {
                return Ok(out);
            }
            let past = after
                .is_none_or(|(v, id): (f64, Uuid)| share.total_cmp(&v).then(k.id.cmp(&id)).is_lt());
            if past {
                let id = k.id;
                out.push((k, Position::Share { v: share, id }));
            }
        }
    }
    if want.is_some_and(|w| out.len() >= w) || filter.capped == Some(true) {
        return Ok(out);
    }
    let tail = KeyListFilter {
        capped: Some(false),
        ..filter
    };
    let left = want.map(|w| w - out.len());
    out.extend(tail_rows(state, tail, tail_from, left).await?);
    Ok(out)
}

/// `GET /keys/count`: how many keys match.
#[derive(Debug, Serialize, ToSchema)]
pub struct KeyCount {
    pub keys: i64,
    /// The tenants they are in.
    pub tenants: i64,
}

/// How many keys the filters of `GET /keys` match (its `sort`, `dir`,
/// `limit`, `after`, `cursor` and `fields` do not matter here), for a
/// "select every matching key" to say how many it will act on.
#[utoipa::path(
    get, path = "/api/v1/keys/count", tag = "keys",
    params(ListKeysQuery),
    responses((status = 200, body = KeyCount), (status = 400))
)]
pub(crate) async fn count_keys(
    State(state): State<AdminState>,
    Query(q): Query<ListKeysQuery>,
) -> Result<Json<KeyCount>> {
    let (keys, tenants) = count(&state, &q).await?;
    Ok(Json(KeyCount { keys, tenants }))
}

/// Filters of `GET /keys/stats`.
#[derive(Debug, Default, Deserialize, IntoParams, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KeyStatsQuery {
    /// Start of the use window, unix epoch millis. Defaults to 30 days
    /// before `until_ms`.
    pub since_ms: Option<i64>,
    /// End of the use window (exclusive), unix epoch millis. Defaults to now.
    pub until_ms: Option<i64>,
    /// Start of the recent span inside the window (`used_recent`). Defaults
    /// to a day before `until_ms`.
    pub recent_ms: Option<i64>,
}

/// The key that sent the most requests in the window.
#[derive(Debug, Serialize, ToSchema)]
pub struct BusiestKey {
    #[schema(value_type = String)]
    pub id: Uuid,
    pub name: String,
    pub requests: i64,
}

/// A key near its budget.
#[derive(Debug, Serialize, ToSchema)]
pub struct NearBudgetKey {
    #[schema(value_type = String)]
    pub id: Uuid,
    pub name: String,
    /// Share of its cap used this period.
    pub share: f64,
}

/// Every key's counts for the Keys page's tiles.
#[derive(Debug, Serialize, ToSchema)]
pub struct KeyStatsView {
    pub keys: i64,
    pub disabled: i64,
    pub identity: i64,
    pub tenants: i64,
    /// Keys with use in the window.
    pub used: i64,
    /// Keys with use since `recent_ms`.
    pub used_recent: i64,
    /// Keys with no use in the window.
    pub unused: i64,
    /// Of those, the ones turned on.
    pub unused_on: i64,
    /// Requests in the window by keys that still exist.
    pub requests: i64,
    pub busiest: Option<BusiestKey>,
    /// Keys that have used 80% or more of a cap this period.
    pub near_budget: i64,
    /// The five nearest, highest share first.
    pub near_budget_keys: Vec<NearBudgetKey>,
}

/// Counts over every key for the Keys page's tiles: how many, on and off,
/// identity keys, tenants, used in the window and recently, unused (and
/// still on), the busiest, and those near a budget. One ClickHouse read
/// (each key's last use and requests in the window) and the budget counters
/// of the capped keys.
#[utoipa::path(
    get, path = "/api/v1/keys/stats", tag = "keys",
    params(KeyStatsQuery),
    responses((status = 200, body = KeyStatsView), (status = 400))
)]
pub(crate) async fn key_stats(
    State(state): State<AdminState>,
    Query(q): Query<KeyStatsQuery>,
) -> Result<Json<KeyStatsView>> {
    let until = q.until_ms.unwrap_or_else(usage::now_ms);
    let since = q.since_ms.unwrap_or(until - DEFAULT_WINDOW_MS);
    let recent = q.recent_ms.unwrap_or(until - 86_400_000);
    if since >= until {
        return Err(AdminError::BadRequest(
            "since_ms must be before until_ms".into(),
        ));
    }
    let base = KeyListFilter {
        exclude_tenant: Some(Store::CONTROL_PLANE_TENANT_ID),
        ..Default::default()
    };
    let (mut used, shares) = tokio::try_join!(
        async {
            usage::query_used_keys(&state.clickhouse, since, until)
                .await
                .map_err(AdminError::from)
        },
        budget_shares(&state, &base),
    )?;
    let recent_ids: Vec<Uuid> = used
        .iter()
        .filter(|u| u.last_used_ms >= recent)
        .map(|u| u.key_id)
        .collect();
    let pairs: Vec<(Uuid, i64)> = used
        .iter()
        .map(|u| (u.key_id, i64::try_from(u.requests).unwrap_or(i64::MAX)))
        .collect();
    let stats = state
        .store
        .key_stats(Some(Store::CONTROL_PLANE_TENANT_ID), &pairs, &recent_ids)
        .await?;
    // The busiest key that still exists: the ledger keeps deleted keys' rows.
    used.sort_by(|a, b| b.requests.cmp(&a.requests).then(b.key_id.cmp(&a.key_id)));
    let mut busiest = None;
    for chunk in used.chunks(50).take(4) {
        let keys = state
            .store
            .list_keys_where(&KeyListFilter {
                ids: Some(chunk.iter().map(|u| u.key_id).collect()),
                ..base.clone()
            })
            .await?;
        let names: HashMap<Uuid, String> = keys.into_iter().map(|k| (k.id, k.name)).collect();
        if let Some(u) = chunk.iter().find(|u| names.contains_key(&u.key_id)) {
            busiest = Some(BusiestKey {
                id: u.key_id,
                name: names[&u.key_id].clone(),
                requests: i64::try_from(u.requests).unwrap_or(i64::MAX),
            });
            break;
        }
    }
    let near: Vec<NearBudgetKey> = shares
        .into_iter()
        .filter(|(share, _)| *share >= NEAR_BUDGET)
        .map(|(share, k)| NearBudgetKey {
            id: k.id,
            name: k.name,
            share,
        })
        .collect();
    Ok(Json(KeyStatsView {
        keys: stats.keys,
        disabled: stats.disabled,
        identity: stats.identity,
        tenants: stats.tenants,
        used: stats.used,
        used_recent: stats.used_recent,
        unused: stats.keys - stats.used,
        unused_on: stats.unused_on,
        requests: stats.requests,
        busiest,
        near_budget: near.len() as i64,
        near_budget_keys: near.into_iter().take(5).collect(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_belong_to_their_order() {
        let id = Uuid::new_v4();
        for p in [
            Position::Created {
                t: 1_700_000_000_123_456,
                id,
            },
            Position::Name {
                n: "Lab bot ✓".into(),
                id,
            },
            Position::Rank { v: -5, id },
            Position::Share {
                v: 0.812_345_678_9,
                id,
            },
            Position::Tail {
                n: String::new(),
                id,
            },
        ] {
            let c = encode_cursor("requests", p.clone());
            assert!(
                c.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
                "{c}"
            );
            assert_eq!(decode_cursor("requests", &c).unwrap(), p);
            assert!(matches!(
                decode_cursor("name", &c),
                Err(AdminError::BadRequest(_))
            ));
        }
        assert!(decode_cursor("requests", "not a cursor").is_err());
        assert!(decode_cursor("requests", &URL_SAFE_NO_PAD.encode(b"{}")).is_err());
    }

    fn query(v: serde_json::Value) -> ListKeysQuery {
        serde_json::from_value(v).expect("query")
    }

    #[test]
    fn plans_check_sort_dir_and_filters() {
        let plan = |v| Plan::new(&query(v), 10 * DEFAULT_WINDOW_MS);
        let p = plan(serde_json::json!({})).unwrap();
        assert_eq!(
            (p.sort, p.desc, p.order.as_str()),
            (Sort::Created, false, "created")
        );
        assert_eq!(
            (p.since, p.until),
            (9 * DEFAULT_WINDOW_MS, 10 * DEFAULT_WINDOW_MS)
        );
        let p = plan(serde_json::json!({ "sort": "name", "dir": "desc" })).unwrap();
        assert_eq!(
            (p.sort, p.desc, p.order.as_str()),
            (Sort::Name, true, "name:desc")
        );
        let p = plan(serde_json::json!({ "sort": "cost" })).unwrap();
        assert_eq!((p.sort, p.desc), (Sort::Use(KeyRankMetric::Cost), true));
        let p = plan(serde_json::json!({ "budget": "near", "kind": "identity", "unused": true }))
            .unwrap();
        assert!(p.near && p.unused);
        assert_eq!(p.filter.capped, Some(true));
        assert_eq!(p.filter.kind.as_deref(), Some("identity"));
        assert_eq!(
            p.filter.exclude_tenant,
            Some(Store::CONTROL_PLANE_TENANT_ID)
        );
        for bad in [
            serde_json::json!({ "sort": "weight" }),
            serde_json::json!({ "sort": "requests", "dir": "asc" }),
            serde_json::json!({ "dir": "up" }),
            serde_json::json!({ "kind": "magic" }),
            serde_json::json!({ "budget": "most" }),
            serde_json::json!({ "since_ms": 5, "until_ms": 5 }),
            serde_json::json!({ "sort": "name", "after": Uuid::nil() }),
            serde_json::json!({ "cursor": encode_cursor("name", Position::Name { n: "a".into(), id: Uuid::nil() }) }),
        ] {
            assert!(
                matches!(plan(bad.clone()), Err(AdminError::BadRequest(_))),
                "{bad}"
            );
        }
    }
}
