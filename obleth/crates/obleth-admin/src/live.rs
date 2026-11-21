//! The overview, stats, capacity discovery and live fair-share views.

use super::*;

/// Live counters of the answering gateway replica.
#[derive(Debug, Serialize, ToSchema)]
pub struct LiveStats {
    /// This replica's in-flight requests.
    pub in_flight: usize,
    pub queued: i64,
    /// The enabled models' pool sizes, summed, as this replica enforces
    /// them: the configured (cluster-wide) sizes in `shared` and `local`
    /// mode, this replica's share of each in `split` and `fallback` mode.
    pub max_in_flight: usize,
    /// Live gateway replicas.
    pub replicas: usize,
    /// How this replica enforces the limits: `local`, `split`, `shared` or
    /// `fallback`.
    pub mode: String,
    /// In-flight requests across every replica, from the shared slots.
    /// `null` outside `shared` mode.
    pub cluster_in_flight: Option<usize>,
}

/// At-a-glance dashboard summary: config counts from Postgres plus usage
/// totals over the window from ClickHouse. Backs the overview/footer poll
/// without deserializing the full tenant/key/model lists per tick.
#[derive(Debug, Serialize, ToSchema)]
pub struct OverviewSummaryView {
    pub requests: u64,
    pub tokens: u64,
    pub cost: f64,
    pub has_pricing: bool,
    pub tenant_count: i64,
    pub active_tenants: u64,
    pub model_count: i64,
    pub enabled_models: i64,
    pub key_count: i64,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Requests in the window that ended with an HTTP status of 400 or above.
    pub errors: u64,
    /// The part of `errors` that were the platform's failures, not the
    /// callers' (see `obleth_config::ErrorClass::is_platform`).
    pub platform_errors: u64,
    /// Median time to first token over the window; 0 when no request produced one.
    pub p50_ttft_ms: f64,
    pub avg_ttft_ms: f64,
    pub energy_wh: f64,
    pub energy_cost_usd: f64,
    pub co2_g: f64,
}

/// A fairshare group in one pool, or across pools in the aggregated lists.
/// Counts are in the view's `scope` (see [`FairshareLiveView::scope`]).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct GroupFairshareView {
    pub name: String,
    pub weight: i64,
    pub in_flight: usize,
    pub queued: usize,
    pub slot_cap: usize,
    /// Slots held above this group's cap, borrowed from siblings leaving
    /// capacity idle. Non-zero only while some other group is under-demand.
    pub borrowed: usize,
    pub served_tokens: f64,
    pub share_score: f64,
    pub weight_share: f64,
    pub expected_slots: f64,
}

/// A tenant in one pool, or across pools in the aggregated lists. Counts are
/// in the view's `scope` (see [`FairshareLiveView::scope`]).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct TenantFairshareView {
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub name: String,
    pub fairshare_group: String,
    pub weight: i64,
    pub max_in_flight: Option<usize>,
    pub in_flight: usize,
    pub queued: usize,
    /// Service received. In `cluster` scope, summed over the replicas and
    /// excluding the virtual time a tenant is placed at when it joins.
    pub served_tokens: f64,
    /// `served_tokens` over weight: the lower, the sooner a freed slot goes
    /// to the tenant. Each replica still orders its own queue.
    pub share_score: f64,
    /// Share of the pool while every active tenant is busy (0 to 1), from
    /// the tenants active in the view's scope; 0 for an idle tenant.
    pub weight_share: f64,
    /// Steady-state slot share under sustained contention:
    /// `weight_share × cap`. A tenant with `queued > 0` and `in_flight`
    /// below `floor(expected_slots)` is waiting below its fair share; judge
    /// that per pool, since a tenant can be above its share in one pool and
    /// short in another.
    pub expected_slots: f64,
}

/// A key in one pool, or across pools in the aggregated lists. Counts are in
/// the view's `scope` (see [`FairshareLiveView::scope`]).
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct KeyFairshareView {
    #[schema(value_type = String)]
    pub key_id: Uuid,
    #[schema(value_type = String)]
    pub tenant_id: Uuid,
    pub name: String,
    pub weight: i64,
    pub max_in_flight: Option<usize>,
    pub in_flight: usize,
    pub queued: usize,
    pub served_tokens: f64,
    pub share_score: f64,
    pub weight_share: f64,
    pub expected_slots: f64,
    /// Set when this row is one end user of a key with per-end-user
    /// fairshare: `key_id` is then derived, `name` is the real key's name,
    /// and this is the end-user id the caller sent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub end_user: Option<String>,
    /// The real key an end user's row belongs to.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub parent_key_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ModelPoolView {
    pub model: String,
    /// The model has nowhere to send a request right now (see
    /// [`models_with_servers`]). Its pool keeps its size for when servers
    /// return, but the capacity totals leave it out.
    pub no_servers: bool,
    /// Slots this replica enforces: `configured_cap` in `shared` and `local`
    /// mode, its share of it in `split` and `fallback` mode. Fair shares
    /// (`expected_slots`) are taken from it.
    pub cap: usize,
    /// The pool size as configured: the cluster-wide size.
    pub configured_cap: usize,
    /// In-flight requests in the pool, in the view's `scope`: every
    /// replica's in `cluster` scope, the answering replica's in `replica`
    /// scope.
    pub in_flight: usize,
    /// The pool slots those requests hold, in the same scope: `in_flight`,
    /// or more in a background pool, where a request holds one slot per
    /// `OBLETH_BACKGROUND_SLOT_TOKENS` of prompt. `cap` counts slots.
    pub slots: usize,
    /// In-flight requests in the pool across every replica, read from the
    /// shared slot counts at the time of the request. `null` outside
    /// `shared` mode.
    pub cluster_in_flight: Option<usize>,
    /// Requests waiting for a slot in the pool, in the view's `scope`.
    pub queued: usize,
    pub borrowed: usize,
    /// The answering replica's own in-flight requests in the pool, whatever
    /// the scope.
    pub replica_in_flight: usize,
    /// The answering replica's own queued requests in the pool, whatever
    /// the scope.
    pub replica_queued: usize,
    pub groups: Vec<GroupFairshareView>,
    pub tenants: Vec<TenantFairshareView>,
    pub keys: Vec<KeyFairshareView>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct FairshareLiveView {
    pub algorithm: String,
    /// Slots this replica can run: each enabled model's pool size as this
    /// replica enforces it (see `mode`), summed, taken from the database
    /// rather than from the snapshot, so a model that has had no traffic yet
    /// still counts. The aggregated `weight_share` below is normalized
    /// against the sum of the caps of the pools that are *present in the
    /// snapshot*, so the two can differ until every enabled model has been
    /// used at least once.
    pub max_in_flight: usize,
    /// The enabled models' pool sizes as configured, summed over the models
    /// that have servers: the cluster-wide capacity.
    pub configured_max_in_flight: usize,
    /// How many enabled models the two totals above add up: those with
    /// servers right now (see [`models_with_servers`]).
    pub models_with_servers: usize,
    /// The enabled models left out of the totals because they have no
    /// servers right now, by name.
    pub models_without_servers: Vec<String>,
    /// Total in-flight ceiling across all pools as this replica enforces it:
    /// the global ceiling in `shared` and `local` mode, this replica's share
    /// of it in `split` and `fallback` mode.
    pub hard_ceiling: usize,
    /// The global ceiling as configured: saved with `PUT /api/v1/capacity`,
    /// else `OBLETH_GLOBAL_MAX_IN_FLIGHT` (see `hard_ceiling_source`).
    pub configured_hard_ceiling: usize,
    /// Where `configured_hard_ceiling` comes from: `saved` or `environment`.
    pub hard_ceiling_source: CeilingSource,
    pub default_model_max_in_flight: usize,
    /// Live gateway replicas.
    pub replicas: usize,
    /// How this replica enforces the limits: `shared` (cluster-wide slots
    /// in Redis), `split` (each replica enforces `ceil(configured /
    /// replicas)`, shared slots off), `fallback` (shared slots on but
    /// unavailable, so the split applies) or `local` (this replica alone
    /// enforces the configured values).
    pub mode: String,
    /// Whether shared slots are configured (`OBLETH_FAIRSHARE_SHARED_SLOTS`).
    pub shared_slots: bool,
    /// Whether the split applies when shared slots are off or unavailable
    /// (`OBLETH_FAIRSHARE_REPLICA_AWARE`).
    pub replica_aware: bool,
    /// In-flight requests across every replica, read from the shared slot
    /// counts at the time of the request. `null` outside `shared` mode.
    pub cluster_in_flight: Option<usize>,
    /// Which replicas the counts in this view cover (in-flight, queued and
    /// borrowed totals, every pool, group, tenant and key, and the fair
    /// shares computed from them):
    ///
    /// - `cluster`: every live replica. In `shared` mode each replica
    ///   publishes its scheduler state about twice a second and the
    ///   answering replica merges its own current state with the others'
    ///   (see `replicas_reporting` and `state_age_ms`).
    /// - `replica`: the answering replica alone. Always so outside `shared`
    ///   mode, and in `shared` mode when the other replicas' state could not
    ///   be read (see `scope_reason`).
    ///
    /// `replica_in_flight` and `replica_queued`, here and on each pool, are
    /// always the answering replica's own.
    pub scope: String,
    /// `cluster` scope: how many replicas' state the counts include. Fewer
    /// than `replicas` while a replica has not published yet (it does so
    /// within a couple of seconds of starting). `null` in `replica` scope.
    pub replicas_reporting: Option<usize>,
    /// `cluster` scope: age of the oldest replica state included, in ms.
    /// `null` in `replica` scope.
    pub state_age_ms: Option<u64>,
    /// `shared` mode in `replica` scope: why the other replicas' state is
    /// missing. `null` otherwise.
    pub scope_reason: Option<String>,
    /// In-flight requests in the view's `scope`.
    pub global_in_flight: usize,
    /// Queued requests in the view's `scope`.
    pub global_queued: i64,
    /// Total occupancy above the apportioned group caps.
    pub global_borrowed: usize,
    /// The answering replica's own in-flight requests, whatever the scope.
    pub replica_in_flight: usize,
    /// The answering replica's own queued requests, whatever the scope.
    pub replica_queued: i64,
    /// Aggregated across pools (see `aggregate_pools`).
    pub groups: Vec<GroupFairshareView>,
    pub tenants: Vec<TenantFairshareView>,
    pub keys: Vec<KeyFairshareView>,
    pub pools: Vec<ModelPoolView>,
    /// In-flight request count per model name, in the view's `scope`.
    #[serde(default)]
    pub model_in_flight: std::collections::HashMap<String, usize>,
    /// Queued request count per model name, in the view's `scope`.
    #[serde(default)]
    pub model_queued: std::collections::HashMap<String, usize>,
}

#[utoipa::path(
    get, path = "/api/v1/overview/summary", tag = "usage",
    params(usage::UsageQuery),
    responses((status = 200, body = OverviewSummaryView))
)]
pub(crate) async fn get_overview_summary(
    State(state): State<AdminState>,
    Query(q): Query<usage::UsageQuery>,
) -> Result<Json<OverviewSummaryView>> {
    let counts = state.store.overview_counts().await?;
    // ClickHouse being down degrades the usage totals to zero rather than
    // failing the whole summary — the config counts are still useful and this
    // read backs a poll on every dashboard page. That poll's unfiltered read
    // is shared for a few seconds (see `usage_cache`).
    let read = usage::query_usage_totals(&state.clickhouse, &q);
    let totals =
        match usage_cache::TotalsKey::of(&q, usage::window_start(q.since_ms, q.until_ms)) {
            Some(key) => state.usage_cache.totals.get_or_load(key, || read).await,
            None => read.await,
        }
        .unwrap_or_default();
    Ok(Json(overview_summary(&counts, &totals)))
}

fn overview_summary(
    counts: &obleth_store::OverviewCounts,
    totals: &usage::UsageTotals,
) -> OverviewSummaryView {
    // The ledger keeps rows for since-deleted tenants; clamp so the summary
    // can never report more active tenants than currently exist.
    let active_tenants = totals.active_tenants.min(counts.tenant_count.max(0) as u64);
    OverviewSummaryView {
        requests: totals.requests,
        tokens: totals.total_tokens,
        cost: totals.cost_usd,
        has_pricing: counts.has_pricing,
        tenant_count: counts.tenant_count,
        active_tenants,
        model_count: counts.model_count,
        enabled_models: counts.enabled_models,
        key_count: counts.key_count,
        input_tokens: totals.input_tokens,
        output_tokens: totals.output_tokens,
        errors: totals.errors,
        platform_errors: totals.platform_errors,
        p50_ttft_ms: totals.p50_ttft_ms,
        avg_ttft_ms: totals.avg_ttft_ms,
        energy_wh: totals.energy_wh,
        energy_cost_usd: totals.energy_cost_usd,
        co2_g: totals.co2_g,
    }
}

/// Everything on the dashboard Overview that follows its range control.
#[derive(Debug, Serialize, ToSchema)]
pub struct OverviewView {
    /// The window, `[since_ms, until_ms)`; `until_ms` is the read's "now"
    /// when the query left it open.
    pub since_ms: i64,
    pub until_ms: i64,
    /// Start of the previous window, which ends at `since_ms`.
    pub previous_since_ms: i64,
    /// Width of the series buckets.
    pub bucket_ms: i64,
    /// Totals of the window, with the gateway's counts.
    pub summary: OverviewSummaryView,
    /// Totals of the previous window (counts as in `summary`).
    pub previous: OverviewSummaryView,
    /// Buckets over both windows, oldest first.
    pub series: Vec<UsageTimePoint>,
    /// Per-model usage of the window, most tokens first.
    pub models: Vec<UsageModelAgg>,
    /// Per-tenant usage of the window, most requests first.
    pub tenants: Vec<OverviewTenantUsage>,
    pub cache: CacheStats,
}

/// The Overview in one read: totals of the window and the one before it,
/// the series over both, per-model and per-tenant usage and cache counts,
/// from two ledger scans. The usage part is shared for a few seconds (see
/// `usage_cache`); counts and tenant names are read fresh. With ClickHouse
/// down the usage reads as zero and the counts are still returned.
#[utoipa::path(
    get, path = "/api/v1/overview", tag = "usage",
    params(usage::OverviewQuery),
    responses((status = 200, body = OverviewView))
)]
pub(crate) async fn get_overview(
    State(state): State<AdminState>,
    Query(q): Query<usage::OverviewQuery>,
) -> Result<Json<OverviewView>> {
    let key = usage_cache::OverviewKey {
        since: usage_cache::bucketed(usage::window_start(q.since_ms, q.until_ms)),
        until: q.until_ms.map(usage_cache::bucketed),
        bucket_ms: q.bucket_ms,
        include_internal: q.include_internal.unwrap_or(false),
    };
    let usage = state.usage_cache.overview.get_or_load(key, || async {
        usage::query_overview(&state.clickhouse, &q)
            .await
            .map(Arc::new)
    });
    let (counts, usage) = tokio::join!(state.store.overview_counts(), usage);
    let counts = counts?;
    let usage = usage.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "overview usage read failed; reporting zero usage");
        Arc::new(usage::OverviewUsage::default())
    });
    let names = names::resolve_names(&state, usage.tenants.iter().map(|t| t.0), []).await?;
    Ok(Json(OverviewView {
        since_ms: usage.since_ms,
        until_ms: usage.until_ms,
        previous_since_ms: usage.previous_since_ms,
        bucket_ms: usage.bucket_ms,
        summary: overview_summary(&counts, &usage.current),
        previous: overview_summary(&counts, &usage.previous),
        series: usage.series.clone(),
        models: usage.models.clone(),
        tenants: usage
            .tenants
            .iter()
            .map(
                |&(tenant_id, requests, total_tokens, cost_usd)| OverviewTenantUsage {
                    tenant_id,
                    name: names.tenant_name(&tenant_id).map(str::to_owned),
                    requests,
                    total_tokens,
                    cost_usd,
                },
            )
            .collect(),
        cache: usage.cache.clone(),
    }))
}

/// Slots the gateway can actually run: the sum of the enabled models' pool
/// sizes, each model's `max_in_flight` or the gateway default.
pub fn enabled_pool_capacity(models: &[ModelRoute], default_cap: usize) -> usize {
    enabled_pool_share(models, default_cap, 1)
}

/// Slots one of `replicas` live replicas can run: its share of each enabled
/// model's pool size, summed. Each share rounds up on its own, so this can
/// exceed `enabled_pool_capacity / replicas` by up to one slot per model.
pub fn enabled_pool_share(models: &[ModelRoute], default_cap: usize, replicas: usize) -> usize {
    enabled_pool_share_with(models, default_cap, replicas, &Default::default())
}

/// [`enabled_pool_share`] with the pool sizes discovery set for some models
/// (see [`capacity_discovery`]) in place of their `max_in_flight`.
pub fn enabled_pool_share_with(
    models: &[ModelRoute],
    default_cap: usize,
    replicas: usize,
    discovered: &std::collections::HashMap<String, usize>,
) -> usize {
    models
        .iter()
        .filter(|m| m.enabled)
        .map(|m| {
            let configured = pool_size(
                &m.model_name,
                m.max_in_flight.and_then(|c| usize::try_from(c).ok()),
                default_cap,
                discovered,
            );
            replica_share(configured, replicas)
        })
        .sum()
}

/// A model's pool size as configured, cluster-wide: the size discovery set
/// for it, else its own `max_in_flight` when positive, else `default_cap`.
pub fn pool_size(
    model_name: &str,
    max_in_flight: Option<usize>,
    default_cap: usize,
    discovered: &std::collections::HashMap<String, usize>,
) -> usize {
    discovered
        .get(model_name)
        .copied()
        .unwrap_or_else(|| max_in_flight.filter(|c| *c > 0).unwrap_or(default_cap))
}

/// Whether an enabled model has somewhere to send a request right now: a
/// fixed `api_base` or an enabled endpoint, and not a backend discovery last
/// saw with no ready replica (see [`models_with_servers`]).
pub fn has_servers(api_base: &str, has_enabled_endpoint: bool, none_ready: bool) -> bool {
    (!api_base.trim().is_empty() || has_enabled_endpoint) && !none_ready
}

/// Names of the models capacity discovery last saw with no ready replica.
pub fn models_without_ready_replicas(
    discovery: &capacity_discovery::CapacityDiscovery,
) -> std::collections::HashSet<String> {
    discovery
        .statuses()
        .into_iter()
        .filter(|s| s.ready_replicas == Some(0))
        .map(|s| s.model_name)
        .collect()
}

/// Names of the enabled models that have somewhere to send a request right
/// now: a fixed `api_base` or at least one enabled endpoint, and not a model
/// whose discovered backend currently has no ready replica. The scheduler
/// keeps a pool for the others, at its last or configured size so a brief
/// outage doesn't shrink it, but adding those slots to a capacity total
/// shows capacity that isn't there.
///
/// `with_endpoint` is the ids of models with an enabled endpoint;
/// `none_ready` the names discovery last saw with no ready replica.
pub fn models_with_servers(
    models: &[ModelRoute],
    with_endpoint: &std::collections::HashSet<Uuid>,
    none_ready: &std::collections::HashSet<String>,
) -> std::collections::HashSet<String> {
    models
        .iter()
        .filter(|m| m.enabled)
        .filter(|m| {
            has_servers(
                &m.api_base,
                with_endpoint.contains(&m.id),
                none_ready.contains(&m.model_name),
            )
        })
        .map(|m| m.model_name.clone())
        .collect()
}

/// The models [`models_with_servers`] counts, for a capacity total.
fn served_models(
    state: &AdminState,
    models: Vec<ModelRoute>,
    endpoints: &[obleth_config::ModelEndpoint],
) -> (Vec<ModelRoute>, std::collections::HashSet<String>) {
    let with_endpoint = endpoints
        .iter()
        .filter(|e| e.enabled)
        .map(|e| e.model_id)
        .collect();
    let none_ready = models_without_ready_replicas(&state.capacity_discovery);
    let serving = models_with_servers(&models, &with_endpoint, &none_ready);
    let counted = models
        .into_iter()
        .filter(|m| serving.contains(&m.model_name))
        .collect();
    (counted, serving)
}

/// How the answering replica enforces the limits right now: its mode, the
/// live replica count, and what configured limits are divided by in that
/// mode (1 unless the split applies). `replica_aware` is
/// `OBLETH_FAIRSHARE_REPLICA_AWARE`.
pub(crate) fn enforcement(stats: &Stats, replica_aware: bool) -> (SlotMode, usize, usize) {
    use std::sync::atomic::Ordering;
    let replicas = stats.replicas.load(Ordering::Relaxed).max(1);
    let mode = stats.mode();
    let divisor = match mode {
        SlotMode::Shared | SlotMode::Local => 1,
        SlotMode::Split => replicas,
        SlotMode::Fallback if replica_aware => replicas,
        SlotMode::Fallback => 1,
    };
    (mode, replicas, divisor)
}

/// Cluster-wide in-flight requests, every pool together and each of `pools`,
/// read from the shared slots. In `shared` mode only; when Redis does not
/// answer, the global count this replica last heard stands in and the
/// per-pool counts are left out.
pub(crate) async fn cluster_in_flight(
    fairshare: &FairShare,
    stats: &Stats,
    mode: SlotMode,
    pools: &[PoolKey],
) -> Option<(usize, Vec<Option<usize>>)> {
    use std::sync::atomic::Ordering;
    if mode != SlotMode::Shared {
        return None;
    }
    match fairshare.cluster_in_flight(pools).await {
        Some((global, per)) => Some((global, per.into_iter().map(Some).collect())),
        None => Some((
            stats.cluster_in_flight.load(Ordering::Relaxed),
            vec![None; pools.len()],
        )),
    }
}

/// Why the global ceiling would bind before the pools do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CeilingWarning {
    /// The ceiling compared, configured or per replica to match `pool_sum`.
    pub ceiling: usize,
    pub pool_sum: usize,
    pub message: &'static str,
}

/// Start-up check that the global ceiling sits above the pools, comparing like
/// with like. Configured ceiling against configured pool sum first: that is
/// the operator's fleet-wide intent. Then this replica's share of each at the
/// live count, since the shares round up per pool and many small pools can
/// push their sum past the ceiling's share even when the configured numbers
/// fit. With one replica the two checks are the same.
pub fn ceiling_check(
    models: &[ModelRoute],
    default_cap: usize,
    ceiling: usize,
    replicas: usize,
) -> Option<CeilingWarning> {
    let pool_sum = enabled_pool_capacity(models, default_cap);
    if ceiling < pool_sum {
        return Some(CeilingWarning {
            ceiling,
            pool_sum,
            message: "OBLETH_GLOBAL_MAX_IN_FLIGHT is below the sum of the enabled models' pool \
                      sizes; the ceiling will bind first and pools will be served round-robin \
                      — raise it above the pool sum",
        });
    }
    let share_sum = enabled_pool_share(models, default_cap, replicas);
    let ceiling_share = replica_share(ceiling, replicas);
    if ceiling_share < share_sum {
        return Some(CeilingWarning {
            ceiling: ceiling_share,
            pool_sum: share_sum,
            message: "each replica's share of OBLETH_GLOBAL_MAX_IN_FLIGHT is below the sum of \
                      its pool shares, which round up per model; the ceiling will bind first \
                      on every replica — raise it by at least one slot per enabled model per \
                      replica above the pool sum",
        });
    }
    None
}

/// Fold per-pool views into one all-models view. Occupancy, backlog, served
/// tokens and expected slots add; `weight_share` becomes expected slots over
/// total pool capacity so it stays in [0, 1].
pub(crate) fn aggregate_pools(
    pools: &[ModelPoolView],
) -> (
    Vec<GroupFairshareView>,
    Vec<TenantFairshareView>,
    Vec<KeyFairshareView>,
) {
    use std::collections::BTreeMap;
    let total_cap: usize = pools.iter().map(|p| p.cap).sum();
    let share = |expected: f64| {
        if total_cap > 0 {
            expected / total_cap as f64
        } else {
            0.0
        }
    };

    let mut groups: BTreeMap<String, GroupFairshareView> = BTreeMap::new();
    let mut tenants: BTreeMap<Uuid, TenantFairshareView> = BTreeMap::new();
    let mut keys: BTreeMap<Uuid, KeyFairshareView> = BTreeMap::new();
    for pool in pools {
        for g in &pool.groups {
            let e = groups
                .entry(g.name.clone())
                .or_insert_with(|| GroupFairshareView {
                    name: g.name.clone(),
                    weight: g.weight,
                    in_flight: 0,
                    queued: 0,
                    slot_cap: 0,
                    borrowed: 0,
                    served_tokens: 0.0,
                    share_score: 0.0,
                    weight_share: 0.0,
                    expected_slots: 0.0,
                });
            e.in_flight += g.in_flight;
            e.queued += g.queued;
            e.slot_cap += g.slot_cap;
            e.borrowed += g.borrowed;
            e.served_tokens += g.served_tokens;
            e.expected_slots += g.expected_slots;
        }
        for t in &pool.tenants {
            let e = tenants
                .entry(t.tenant_id)
                .or_insert_with(|| TenantFairshareView {
                    tenant_id: t.tenant_id,
                    name: t.name.clone(),
                    fairshare_group: t.fairshare_group.clone(),
                    weight: t.weight,
                    max_in_flight: t.max_in_flight,
                    in_flight: 0,
                    queued: 0,
                    served_tokens: 0.0,
                    share_score: 0.0,
                    weight_share: 0.0,
                    expected_slots: 0.0,
                });
            e.in_flight += t.in_flight;
            e.queued += t.queued;
            e.served_tokens += t.served_tokens;
            e.expected_slots += t.expected_slots;
        }
        for k in &pool.keys {
            let e = keys.entry(k.key_id).or_insert_with(|| KeyFairshareView {
                key_id: k.key_id,
                tenant_id: k.tenant_id,
                name: k.name.clone(),
                weight: k.weight,
                max_in_flight: k.max_in_flight,
                in_flight: 0,
                queued: 0,
                served_tokens: 0.0,
                share_score: 0.0,
                weight_share: 0.0,
                expected_slots: 0.0,
                end_user: k.end_user.clone(),
                parent_key_id: k.parent_key_id,
            });
            e.in_flight += k.in_flight;
            e.queued += k.queued;
            e.served_tokens += k.served_tokens;
            e.expected_slots += k.expected_slots;
        }
    }
    let by_score = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
    let mut groups: Vec<_> = groups
        .into_values()
        .map(|mut g| {
            g.share_score = g.served_tokens / g.weight.max(1) as f64;
            g.weight_share = share(g.expected_slots);
            g
        })
        .collect();
    groups.sort_by(|a, b| a.name.cmp(&b.name));
    let mut tenants: Vec<_> = tenants
        .into_values()
        .map(|mut t| {
            t.share_score = t.served_tokens / t.weight.max(1) as f64;
            t.weight_share = share(t.expected_slots);
            t
        })
        .collect();
    tenants.sort_by(|a, b| by_score(a.share_score, b.share_score));
    let mut keys: Vec<_> = keys
        .into_values()
        .map(|mut k| {
            k.share_score = k.served_tokens / k.weight.max(1) as f64;
            k.weight_share = share(k.expected_slots);
            k
        })
        .collect();
    keys.sort_by(|a, b| by_score(a.share_score, b.share_score));
    (groups, tenants, keys)
}

#[utoipa::path(
    get, path = "/api/v1/stats", tag = "usage",
    responses((status = 200, body = LiveStats))
)]
pub(crate) async fn get_stats(State(state): State<AdminState>) -> Json<LiveStats> {
    use std::sync::atomic::Ordering;
    // The live counters are the point of this endpoint; a database hiccup
    // should degrade the capacity number, not fail the poll.
    let (mode, replicas, divisor) =
        enforcement(&state.fairshare_stats, state.fairshare_replica_aware);
    let discovered = state.capacity_discovery.effective_caps();
    let endpoints = state.store.all_model_endpoints().await.unwrap_or_default();
    let capacity = state
        .store
        .list_models()
        .await
        .map(|m| {
            let (counted, _) = served_models(&state, m, &endpoints);
            enabled_pool_share_with(
                &counted,
                state.default_model_max_in_flight,
                divisor,
                &discovered,
            )
        })
        .unwrap_or(0);
    Json(LiveStats {
        in_flight: state.fairshare_stats.in_flight.load(Ordering::Relaxed),
        queued: state.fairshare_stats.queued.load(Ordering::Relaxed),
        max_in_flight: if capacity > 0 {
            capacity
        } else {
            replica_share(state.ceiling.max_in_flight(), divisor)
        },
        replicas,
        mode: mode.as_str().into(),
        cluster_in_flight: cluster_in_flight(&state.fairshare, &state.fairshare_stats, mode, &[])
            .await
            .map(|(g, _)| g),
    })
}

/// This replica's capacity discovery: its settings and every `discovered`
/// model's state.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CapacityDiscoveryView {
    /// `OBLETH_CAPACITY_DISCOVERY_ENABLED` on this replica.
    pub enabled: bool,
    pub interval_secs: u64,
    /// Namespaces the `kubernetes` source may read. Empty: that source is
    /// unavailable.
    pub namespaces: Vec<String>,
    /// Service name template for `kubernetes` models that set no
    /// `capacity_service`.
    pub default_service: String,
    /// Live gateway replicas.
    pub replicas: usize,
    /// How the answering replica enforces pool sizes: `shared`, `split`,
    /// `fallback` or `local` (see `FairshareLiveView::mode`).
    pub mode: String,
    pub models: Vec<CapacityDiscoveryModelView>,
}

/// One `discovered` model.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CapacityDiscoveryModelView {
    pub model_id: Uuid,
    pub model_name: String,
    pub enabled: bool,
    /// The model's own `max_in_flight`: the fallback.
    pub static_max_in_flight: Option<i64>,
    /// The pool size the answering replica enforces:
    /// `status.effective_max_in_flight` in `shared` and `local` mode, its
    /// share of it in `split` and `fallback` mode.
    pub enforced_max_in_flight: usize,
    /// In-flight requests for the model across every replica, from the
    /// shared slots. `null` outside `shared` mode.
    pub cluster_in_flight: Option<usize>,
    /// The answering replica's own in-flight requests for the model.
    pub in_flight: usize,
    pub status: ModelCapacityStatus,
}

#[utoipa::path(
    get, path = "/api/v1/capacity/discovery", tag = "models",
    responses((status = 200, body = CapacityDiscoveryView))
)]
pub(crate) async fn get_capacity_discovery(
    State(state): State<AdminState>,
) -> Result<Json<CapacityDiscoveryView>> {
    let settings = state.capacity_discovery.settings().clone();
    let (mode, replicas, divisor) =
        enforcement(&state.fairshare_stats, state.fairshare_replica_aware);
    let local_in_flight = state.fairshare.model_load();
    let statuses: std::collections::HashMap<String, ModelCapacityStatus> = state
        .capacity_discovery
        .statuses()
        .into_iter()
        .map(|s| (s.model_name.clone(), s))
        .collect();
    let discovered: Vec<obleth_config::ModelRoute> = state
        .store
        .list_models()
        .await?
        .into_iter()
        .filter(|m| m.capacity_mode == obleth_config::DISCOVERED_CAPACITY_MODE)
        .collect();
    let pool_keys: Vec<PoolKey> = discovered
        .iter()
        .map(|m| PoolKey::Model(m.model_name.clone()))
        .collect();
    let cluster = cluster_in_flight(&state.fairshare, &state.fairshare_stats, mode, &pool_keys)
        .await
        .map(|(_, per)| per)
        .unwrap_or_else(|| vec![None; pool_keys.len()]);
    let models = discovered
        .into_iter()
        .zip(cluster)
        .map(|(m, cluster_in_flight)| {
            let status = statuses.get(&m.model_name).cloned().unwrap_or_else(|| {
                // Not evaluated by this replica (yet): say why, with the value
                // fairshare is using meanwhile.
                let reason = if !settings.enabled {
                    "capacity discovery is off on this gateway \
                     (OBLETH_CAPACITY_DISCOVERY_ENABLED)"
                } else if !m.enabled {
                    "the model is disabled"
                } else {
                    "waiting for the next discovery pass"
                };
                ModelCapacityStatus {
                    model_name: m.model_name.clone(),
                    source: m.capacity_source.clone(),
                    namespaces: Vec::new(),
                    namespace: m.capacity_namespace.clone(),
                    service: m.capacity_service.clone(),
                    ready_replicas: None,
                    terminating_replicas: None,
                    per_replica_max_in_flight: None,
                    per_replica_source: None,
                    replica_capacity: None,
                    headroom: m.capacity_headroom,
                    derived_max_in_flight: None,
                    effective_max_in_flight: m
                        .max_in_flight
                        .and_then(|c| usize::try_from(c).ok())
                        .filter(|c| *c > 0)
                        .unwrap_or(state.default_model_max_in_flight),
                    state: capacity_discovery::STATE_FALLBACK.into(),
                    last_refresh: None,
                    last_success: None,
                    reason: Some(format!("{reason}; using the static max_in_flight")),
                }
            });
            CapacityDiscoveryModelView {
                model_id: m.id,
                enabled: m.enabled,
                static_max_in_flight: m.max_in_flight,
                enforced_max_in_flight: replica_share(status.effective_max_in_flight, divisor),
                cluster_in_flight,
                in_flight: local_in_flight.get(&m.model_name).copied().unwrap_or(0),
                model_name: m.model_name,
                status,
            }
        })
        .collect();
    Ok(Json(CapacityDiscoveryView {
        enabled: settings.enabled,
        interval_secs: settings.interval.as_secs(),
        namespaces: settings.namespaces,
        default_service: settings.default_service,
        replicas,
        mode: mode.as_str().into(),
        models,
    }))
}

/// The Services the `kubernetes` capacity source can see, for choosing a
/// model's Service. Built from EndpointSlices in the allowed namespaces
/// only, and cached briefly.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct CapacityServicesView {
    /// Every Service with endpoints in the allowed namespaces.
    pub services: Vec<ServiceSummary>,
    /// Why `services` is empty or incomplete, when it is.
    pub reason: Option<String>,
    /// Namespaces that could not be listed, with why.
    pub errors: Vec<String>,
    /// With `model`: the Service name that model would use by default
    /// (`OBLETH_CAPACITY_DEFAULT_SERVICE` rendered for it), whether or not it
    /// exists. `null` when there is no default.
    pub default_service: Option<String>,
    /// With `model`: that default Service, in the first allowed namespace
    /// that has it, as discovery would find it. `null` when none has it.
    pub default_match: Option<ServiceSummary>,
}

#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub struct CapacityServicesQuery {
    /// A model's name or id, to resolve its default Service.
    pub model: Option<String>,
}

#[utoipa::path(
    get, path = "/api/v1/capacity/services", tag = "models",
    params(CapacityServicesQuery),
    responses((status = 200, body = CapacityServicesView))
)]
pub(crate) async fn get_capacity_services(
    State(state): State<AdminState>,
    Query(q): Query<CapacityServicesQuery>,
) -> Result<Json<CapacityServicesView>> {
    let listing = state.capacity_discovery.list_services().await;
    let (default_service, default_match) = match q.model.as_deref().filter(|m| !m.is_empty()) {
        Some(wanted) => {
            let model = state
                .store
                .list_models()
                .await?
                .into_iter()
                .find(|m| m.model_name == wanted || m.id.to_string() == wanted)
                .ok_or(AdminError::NotFound)?;
            state.capacity_discovery.default_match(
                &listing,
                &model.upstream_model,
                &model.model_name,
            )
        }
        None => (None, None),
    };
    Ok(Json(CapacityServicesView {
        services: listing.services,
        reason: listing.reason,
        errors: listing.errors,
        default_service,
        default_match,
    }))
}

/// The health prober's load in a scheduler pool: (in flight, queued). The
/// prober admits like real traffic under its own group, which the live view
/// leaves out.
fn health_load(p: &ModelPoolFairshare) -> (usize, usize) {
    p.groups
        .iter()
        .find(|g| g.name == model_health::HEALTH_GROUP)
        .map(|g| (g.in_flight, g.queued))
        .unwrap_or((0, 0))
}

/// Names the live view resolves scheduler ids to, and what it needs to mark
/// a pool as having no servers.
pub(crate) struct LiveNames<'a> {
    pub tenants: &'a std::collections::HashMap<Uuid, String>,
    pub keys: &'a std::collections::HashMap<Uuid, String>,
    /// Enabled models.
    pub enabled: &'a std::collections::HashSet<&'a str>,
    /// Enabled models with somewhere to send a request.
    pub serving: &'a std::collections::HashSet<String>,
}

/// A pool's request counts as the live view reports them, the health prober
/// left out (see [`live_pool_view`] for the arguments).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PoolCounts {
    /// In flight in the view's scope.
    pub(crate) in_flight: usize,
    /// The pool slots those requests hold in the view's scope.
    pub(crate) slots: usize,
    /// Waiting for a slot in the view's scope.
    pub(crate) queued: usize,
    /// In flight across every replica, from the shared slots.
    pub(crate) cluster_in_flight: Option<usize>,
    /// The answering replica's own counts, whatever the scope.
    pub(crate) replica_in_flight: usize,
    pub(crate) replica_queued: usize,
}

/// One pool's [`PoolCounts`].
pub(crate) fn pool_counts(
    p: &ModelPoolFairshare,
    local: Option<&ModelPoolFairshare>,
    cluster_in_flight: Option<usize>,
) -> PoolCounts {
    let (h_in, h_q) = health_load(p);
    let (replica_in_flight, replica_queued) = local
        .map(|l| {
            let (h_in, h_q) = health_load(l);
            (
                l.in_flight.saturating_sub(h_in),
                l.queued.saturating_sub(h_q),
            )
        })
        .unwrap_or((0, 0));
    PoolCounts {
        in_flight: p.in_flight.saturating_sub(h_in),
        // A probe holds one slot. A snapshot without slot counts reads as
        // one slot per request.
        slots: p.slots.max(p.in_flight).saturating_sub(h_in),
        queued: p.queued.saturating_sub(h_q),
        // Cluster-wide slot counts include every replica's health probes,
        // which are not told apart from traffic there. Without a fresh read,
        // the count the replica last heard stands in.
        cluster_in_flight: cluster_in_flight
            .or(p.cluster_in_flight)
            .or(local.and_then(|l| l.cluster_in_flight)),
        replica_in_flight,
        replica_queued,
    }
}

/// One pool of the live view: `p` is the scheduler's pool in the view's
/// scope (merged across replicas in `cluster` scope), `local` the answering
/// replica's own pool when it has one, `cluster_in_flight` the shared slot
/// count. The health prober is left out of every level and every count.
pub(crate) fn live_pool_view(
    p: &ModelPoolFairshare,
    local: Option<&ModelPoolFairshare>,
    cluster_in_flight: Option<usize>,
    names: &LiveNames<'_>,
) -> ModelPoolView {
    let cap = p.cap;
    let counts = pool_counts(p, local, cluster_in_flight);
    let health_tenant = model_health::health_tenant_id();
    ModelPoolView {
        model: p.model.clone(),
        no_servers: names.enabled.contains(p.model.as_str()) && !names.serving.contains(&p.model),
        cap,
        configured_cap: p.configured_cap,
        in_flight: counts.in_flight,
        slots: counts.slots,
        cluster_in_flight: counts.cluster_in_flight,
        queued: counts.queued,
        borrowed: p.borrowed,
        replica_in_flight: counts.replica_in_flight,
        replica_queued: counts.replica_queued,
        groups: p
            .groups
            .iter()
            .filter(|g| g.name != model_health::HEALTH_GROUP)
            .map(|g| GroupFairshareView {
                name: g.name.clone(),
                weight: g.weight,
                in_flight: g.in_flight,
                queued: g.queued,
                slot_cap: g.slot_cap,
                borrowed: g.borrowed,
                served_tokens: g.served_tokens,
                share_score: g.share_score,
                weight_share: g.weight_share,
                expected_slots: g.weight_share * cap as f64,
            })
            .collect(),
        tenants: p
            .tenants
            .iter()
            .filter(|t| {
                t.tenant_id != health_tenant && t.fairshare_group != model_health::HEALTH_GROUP
            })
            .map(|t| TenantFairshareView {
                tenant_id: t.tenant_id,
                name: names
                    .tenants
                    .get(&t.tenant_id)
                    .cloned()
                    .unwrap_or_else(|| t.tenant_id.to_string()),
                fairshare_group: t.fairshare_group.clone(),
                weight: t.weight,
                max_in_flight: t.max_in_flight,
                in_flight: t.in_flight,
                queued: t.queued,
                served_tokens: t.served_tokens,
                share_score: t.share_score,
                weight_share: t.weight_share,
                expected_slots: t.weight_share * cap as f64,
            })
            .collect(),
        keys: p
            .keys
            .iter()
            .filter(|k| k.tenant_id != health_tenant)
            .map(|k| KeyFairshareView {
                key_id: k.key_id,
                tenant_id: k.tenant_id,
                name: {
                    let real = k.end_user.as_ref().map_or(k.key_id, |e| e.parent_key);
                    names
                        .keys
                        .get(&real)
                        .cloned()
                        .unwrap_or_else(|| real.to_string())
                },
                weight: k.weight,
                max_in_flight: k.max_in_flight,
                in_flight: k.in_flight,
                queued: k.queued,
                served_tokens: k.served_tokens,
                share_score: k.share_score,
                weight_share: k.weight_share,
                expected_slots: k.weight_share * cap as f64,
                end_user: k.end_user.as_ref().map(|e| e.name.clone()),
                parent_key_id: k.end_user.as_ref().map(|e| e.parent_key),
            })
            .collect(),
    }
}

/// Which replicas the live view covers, and the pools to build it from.
pub(crate) struct LiveScope {
    pub(crate) scope: &'static str,
    pub(crate) reason: Option<String>,
    pub(crate) cluster: Option<obleth_fairshare::ClusterSnapshot>,
}

/// In `shared` mode, every live replica's scheduler state merged; when the
/// shared backend cannot give it, or outside `shared` mode, the answering
/// replica's own, with why when the cluster view was wanted.
/// The answering replica's scheduler state for a live view in `scope`. With
/// the cluster view in hand its tenants and keys come from there, so only the
/// replica's pool and group counts are read.
pub(crate) async fn local_snapshot(
    fairshare: &FairShare,
    scope: &LiveScope,
) -> Option<obleth_fairshare::FairshareSnapshot> {
    if scope.cluster.is_some() {
        fairshare.pool_snapshot().await
    } else {
        fairshare.snapshot().await
    }
}

pub(crate) async fn live_scope(fairshare: &FairShare, mode: SlotMode) -> LiveScope {
    if mode != SlotMode::Shared {
        return LiveScope {
            scope: "replica",
            reason: None,
            cluster: None,
        };
    }
    match fairshare.cluster_snapshot().await {
        Ok(cluster) => LiveScope {
            scope: "cluster",
            reason: None,
            cluster: Some(cluster),
        },
        Err(e) => LiveScope {
            scope: "replica",
            reason: Some(format!(
                "the other replicas' scheduler state could not be read from the shared \
                 backend ({e}); queues, tenants and fair shares are this replica's own"
            )),
            cluster: None,
        },
    }
}

/// Live fairshare state
///
/// Every model pool with its groups, tenants and keys, and the same
/// aggregated across pools. In `shared` mode the counts cover every live
/// gateway replica (`scope: cluster`); otherwise, or when the other
/// replicas' state cannot be read, the answering replica's own
/// (`scope: replica`, with `scope_reason` in `shared` mode).
/// `replica_in_flight` and `replica_queued` are always the answering
/// replica's.
#[utoipa::path(
    get, path = "/api/v1/fairshare/live", tag = "fairshare",
    responses((status = 200, body = FairshareLiveView))
)]
pub(crate) async fn get_fairshare_live(
    State(state): State<AdminState>,
) -> Result<Json<FairshareLiveView>> {
    let (mode, _, divisor) = enforcement(&state.fairshare_stats, state.fairshare_replica_aware);
    let scope = live_scope(&state.fairshare, mode).await;
    let snap = local_snapshot(&state.fairshare, &scope)
        .await
        .ok_or(AdminError::Internal("fairshare unavailable".into()))?;
    let source: &[ModelPoolFairshare] = match &scope.cluster {
        Some(c) => &c.pools,
        None => &snap.pools,
    };
    let tenant_ids: Vec<Uuid> = source
        .iter()
        .flat_map(|p| p.tenants.iter().map(|t| t.tenant_id))
        .collect();
    let key_ids: Vec<Uuid> = source
        .iter()
        .flat_map(|p| {
            p.keys
                .iter()
                .map(|k| k.end_user.as_ref().map_or(k.key_id, |e| e.parent_key))
        })
        .collect();
    let known = names::resolve_names(&state, tenant_ids, key_ids).await?;
    let tenant_names: std::collections::HashMap<Uuid, String> = known
        .tenants
        .into_values()
        .map(|t| (t.id, t.name))
        .collect();
    let key_names: std::collections::HashMap<Uuid, String> =
        known.keys.into_values().map(|k| (k.id, k.name)).collect();
    let catalog = state.live_catalog.get(&state.store).await?;
    let (models, endpoints) = (&catalog.models, &catalog.endpoints);
    let discovered = state.capacity_discovery.effective_caps();
    let (counted, serving) = served_models(&state, models.clone(), endpoints);
    let configured_capacity =
        enabled_pool_share_with(&counted, state.default_model_max_in_flight, 1, &discovered);
    let capacity = enabled_pool_share_with(
        &counted,
        state.default_model_max_in_flight,
        divisor,
        &discovered,
    );
    let enabled_names: std::collections::HashSet<&str> = models
        .iter()
        .filter(|m| m.enabled)
        .map(|m| m.model_name.as_str())
        .collect();
    let pool_keys: Vec<PoolKey> = source
        .iter()
        .map(|p| {
            if p.model == obleth_fairshare::UNROUTED_POOL {
                PoolKey::Unrouted
            } else {
                PoolKey::Model(p.model.clone())
            }
        })
        .collect();
    let (cluster_global, cluster_pools) =
        match cluster_in_flight(&state.fairshare, &state.fairshare_stats, mode, &pool_keys).await {
            Some((global, per)) => (Some(global), per),
            None => (None, vec![None; pool_keys.len()]),
        };

    // Pools outlive their model: a renamed, deleted or disabled model keeps an
    // empty pool in the scheduler until restart. Every pool with visible
    // traffic is kept; the idle ones are dropped unless an enabled model still
    // answers to that name or one of its aliases.
    let live_names: std::collections::HashSet<&str> = models
        .iter()
        .filter(|m| m.enabled)
        .flat_map(|m| m.addressable_names())
        .collect();

    let names = LiveNames {
        tenants: &tenant_names,
        keys: &key_names,
        enabled: &enabled_names,
        serving: &serving,
    };
    let (hidden_in_flight, hidden_queued) = source
        .iter()
        .map(health_load)
        .fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
    let (local_hidden_in_flight, local_hidden_queued) = snap
        .pools
        .iter()
        .map(health_load)
        .fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
    let mut pools: Vec<ModelPoolView> = source
        .iter()
        .zip(cluster_pools)
        .map(|(p, cluster_pool)| {
            let local = snap.pools.iter().find(|l| l.model == p.model);
            live_pool_view(p, local, cluster_pool, &names)
        })
        .collect();
    pools.retain(|p| p.in_flight > 0 || p.queued > 0 || live_names.contains(p.model.as_str()));
    let (groups, tenants, keys) = aggregate_pools(&pools);
    let replica_in_flight = snap.global_in_flight.saturating_sub(local_hidden_in_flight);
    let replica_queued = snap.global_queued.saturating_sub(local_hidden_queued) as i64;
    let (global_in_flight, global_queued, global_borrowed, model_in_flight, model_queued) =
        match &scope.cluster {
            Some(c) => (
                c.pools
                    .iter()
                    .map(|p| p.in_flight)
                    .sum::<usize>()
                    .saturating_sub(hidden_in_flight),
                c.pools
                    .iter()
                    .map(|p| p.queued)
                    .sum::<usize>()
                    .saturating_sub(hidden_queued) as i64,
                c.pools.iter().map(|p| p.borrowed).sum(),
                c.pools
                    .iter()
                    .filter(|p| p.in_flight > 0)
                    .map(|p| (p.model.clone(), p.in_flight))
                    .collect(),
                c.pools
                    .iter()
                    .filter(|p| p.queued > 0)
                    .map(|p| (p.model.clone(), p.queued))
                    .collect(),
            ),
            None => (
                replica_in_flight,
                replica_queued,
                snap.global_borrowed,
                snap.model_in_flight.clone(),
                snap.model_queued.clone(),
            ),
        };
    Ok(Json(FairshareLiveView {
        algorithm: snap.algorithm,
        max_in_flight: if capacity > 0 {
            capacity
        } else {
            snap.max_in_flight
        },
        configured_max_in_flight: if configured_capacity > 0 {
            configured_capacity
        } else {
            snap.configured_max_in_flight
        },
        models_with_servers: serving.len(),
        models_without_servers: {
            let mut names: Vec<String> = enabled_names
                .iter()
                .filter(|n| !serving.contains(**n))
                .map(|n| n.to_string())
                .collect();
            names.sort_unstable();
            names
        },
        hard_ceiling: snap.max_in_flight,
        configured_hard_ceiling: snap.configured_max_in_flight,
        hard_ceiling_source: state.ceiling.source(),
        default_model_max_in_flight: snap.default_model_max_in_flight,
        replicas: snap.replicas,
        mode: mode.as_str().into(),
        shared_slots: state.fairshare_shared_slots,
        replica_aware: state.fairshare_replica_aware,
        cluster_in_flight: cluster_global,
        scope: scope.scope.into(),
        replicas_reporting: scope.cluster.as_ref().map(|c| c.replicas_reporting),
        state_age_ms: scope.cluster.as_ref().map(|c| c.max_age_ms),
        scope_reason: scope.reason,
        global_in_flight,
        global_queued,
        global_borrowed,
        replica_in_flight,
        replica_queued,
        groups,
        tenants,
        keys,
        pools,
        model_in_flight,
        model_queued,
    }))
}

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct FairshareHistoryQuery {
    /// Return points at or after this Unix time in ms. Default: now minus retention.
    pub since_ms: Option<i64>,
    /// Scope to one model's pool. Absent or empty: aggregate across pools.
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FairshareHistoryPointView {
    pub ts_ms: i64,
    pub in_flight: usize,
    pub queued: usize,
    /// Group name to in-flight slots.
    pub groups: std::collections::BTreeMap<String, usize>,
    /// Counted across every live gateway replica (shared slots). False for a
    /// point the answering replica sampled alone: shared slots off, or the
    /// shared backend unreadable at sample time.
    pub cluster: bool,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct FairshareHistoryView {
    pub interval_ms: u64,
    pub retention_ms: u64,
    /// Time of the oldest retained sample; null when nothing is retained yet.
    pub oldest_ts_ms: Option<i64>,
    pub points: Vec<FairshareHistoryPointView>,
}

#[utoipa::path(
    get, path = "/api/v1/fairshare/history", tag = "fairshare",
    params(FairshareHistoryQuery),
    responses((status = 200, body = FairshareHistoryView))
)]
pub(crate) async fn get_fairshare_history(
    State(state): State<AdminState>,
    Query(q): Query<FairshareHistoryQuery>,
) -> Json<FairshareHistoryView> {
    let retention_ms = state.fairshare_history_secs.saturating_mul(1000);
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let since_ms = q
        .since_ms
        .unwrap_or_else(|| now_ms.saturating_sub(retention_ms as i64));
    let model = q.model.as_deref().filter(|m| !m.is_empty());
    let points = state
        .fairshare_history
        .points(since_ms, model, Some(model_health::HEALTH_GROUP))
        .into_iter()
        .map(|p| FairshareHistoryPointView {
            ts_ms: p.ts_ms,
            in_flight: p.in_flight,
            queued: p.queued,
            groups: p.groups,
            cluster: p.cluster,
        })
        .collect();
    Json(FairshareHistoryView {
        interval_ms: obleth_fairshare::FAIRSHARE_HISTORY_INTERVAL_MS,
        retention_ms,
        oldest_ts_ms: state.fairshare_history.oldest_ts_ms(),
        points,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::fixture_model_route;

    fn pool(model: &str, cap: usize, tenants: Vec<TenantFairshareView>) -> ModelPoolView {
        let in_flight = tenants.iter().map(|t| t.in_flight).sum();
        ModelPoolView {
            model: model.into(),
            no_servers: false,
            cap,
            configured_cap: cap,
            in_flight,
            slots: in_flight,
            cluster_in_flight: None,
            queued: 0,
            borrowed: 0,
            replica_in_flight: in_flight,
            replica_queued: 0,
            groups: vec![GroupFairshareView {
                name: "g".into(),
                weight: 100,
                in_flight,
                queued: 0,
                slot_cap: cap,
                borrowed: 0,
                served_tokens: 0.0,
                share_score: 0.0,
                weight_share: 1.0,
                expected_slots: cap as f64,
            }],
            tenants,
            keys: vec![],
        }
    }
    fn tv(
        id: Uuid,
        in_flight: usize,
        served: f64,
        share: f64,
        expected: f64,
    ) -> TenantFairshareView {
        TenantFairshareView {
            tenant_id: id,
            name: "t".into(),
            fairshare_group: "g".into(),
            weight: 100,
            max_in_flight: None,
            in_flight,
            queued: 0,
            served_tokens: served,
            share_score: served / 100.0,
            weight_share: share,
            expected_slots: expected,
        }
    }

    #[test]
    fn aggregate_pools_sums_occupancy_and_shares_by_pool_capacity() {
        let t = Uuid::new_v4();
        let pools = vec![
            pool("a", 8, vec![tv(t, 2, 100.0, 0.5, 4.0)]),
            pool("b", 2, vec![tv(t, 1, 50.0, 1.0, 2.0)]),
        ];
        let (groups, tenants, _) = aggregate_pools(&pools);
        assert_eq!(tenants.len(), 1);
        assert_eq!(tenants[0].in_flight, 3);
        assert_eq!(tenants[0].served_tokens, 150.0);
        assert_eq!(tenants[0].expected_slots, 6.0);
        assert!((tenants[0].weight_share - 0.6).abs() < 1e-9); // 6 of 10 slots
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].slot_cap, 10);
    }

    /// One replica's published state for a pool held by one tenant on one
    /// key.
    fn replica_view(
        tenant: Uuid,
        key: Uuid,
        in_flight: usize,
        queued: usize,
    ) -> obleth_fairshare::ReplicaView {
        use obleth_fairshare::{
            ReplicaGroupView, ReplicaKeyView, ReplicaPoolView, ReplicaTenantView,
        };
        obleth_fairshare::ReplicaView {
            version: obleth_fairshare::view::VIEW_VERSION,
            pools: vec![ReplicaPoolView {
                pool: PoolKey::Model("big".into()).slot_id(),
                configured_cap: 256,
                in_flight,
                queued,
                groups: vec![ReplicaGroupView {
                    name: "default".into(),
                    weight: 100,
                    in_flight,
                    queued,
                    served: in_flight as f64,
                }],
                tenants: vec![ReplicaTenantView {
                    id: tenant,
                    group: "default".into(),
                    weight: 100,
                    cap: None,
                    in_flight,
                    queued,
                    served: in_flight as f64,
                }],
                keys: vec![ReplicaKeyView {
                    id: key,
                    tenant,
                    weight: 100,
                    cap: None,
                    in_flight,
                    queued,
                    served: in_flight as f64,
                    end_user: None,
                }],
                ..Default::default()
            }],
        }
    }

    /// The case behind the cluster view: one tenant holds all 256 slots of a
    /// pool across three replicas (42 + 96 + 118) with 767 requests queued
    /// (350 + 209 + 208). The answering replica's own state alone read as
    /// "waiting below its fair share, holding 96 against 256"; merged, the
    /// pool, its tenant and its key are counted across the fleet, the fair
    /// share is the whole pool, and the replica's own counts stay alongside.
    #[test]
    fn live_pool_view_counts_a_merged_pool_across_the_fleet() {
        let (tenant, key) = (Uuid::new_v4(), Uuid::new_v4());
        let views = [
            replica_view(tenant, key, 96, 209),
            replica_view(tenant, key, 42, 350),
            replica_view(tenant, key, 118, 208),
        ];
        let algorithm = obleth_config::FairshareAlgorithm::Hierarchical;
        let merged =
            obleth_fairshare::merge_views(algorithm, &views.iter().collect::<Vec<_>>(), 4096);
        let local = obleth_fairshare::merge_views(algorithm, &[&views[0]], 4096);
        let tenant_names = std::collections::HashMap::from([(tenant, "acme".to_string())]);
        let key_names = std::collections::HashMap::from([(key, "app".to_string())]);
        let enabled = std::collections::HashSet::from(["big"]);
        let serving = std::collections::HashSet::from(["big".to_string()]);
        let names = LiveNames {
            tenants: &tenant_names,
            keys: &key_names,
            enabled: &enabled,
            serving: &serving,
        };
        let view = live_pool_view(&merged[0], local.first(), Some(256), &names);

        assert_eq!((view.cap, view.configured_cap), (256, 256));
        assert_eq!((view.in_flight, view.queued), (256, 767));
        assert_eq!((view.replica_in_flight, view.replica_queued), (96, 209));
        assert_eq!(view.cluster_in_flight, Some(256));
        assert!(!view.no_servers);
        assert_eq!(view.tenants.len(), 1);
        let t = &view.tenants[0];
        assert_eq!((t.name.as_str(), t.in_flight, t.queued), ("acme", 256, 767));
        assert!(
            (t.expected_slots - 256.0).abs() < 1e-9,
            "{}",
            t.expected_slots
        );
        assert!(
            !(t.queued > 0 && (t.in_flight as f64) < t.expected_slots.floor()),
            "holding every slot it can, the tenant is not waiting below its share"
        );
        assert_eq!(view.groups.len(), 1);
        assert_eq!(
            (view.groups[0].in_flight, view.groups[0].queued),
            (256, 767)
        );
        assert_eq!(view.keys.len(), 1);
        assert_eq!(
            (view.keys[0].name.as_str(), view.keys[0].in_flight),
            ("app", 256)
        );

        // The answering replica's own pool, as `replica` scope reports it.
        let own = live_pool_view(&local[0], local.first(), None, &names);
        assert_eq!((own.in_flight, own.queued), (96, 209));
        assert_eq!((own.replica_in_flight, own.replica_queued), (96, 209));
    }

    /// A background pool's requests hold a slot per
    /// `OBLETH_BACKGROUND_SLOT_TOKENS` of prompt: the live view sends the
    /// slots they hold beside the request count, merged across replicas, and
    /// an ordinary pool's slots are its requests.
    #[test]
    fn live_pool_view_sends_the_slots_a_background_pool_holds() {
        let (tenant, key) = (Uuid::new_v4(), Uuid::new_v4());
        let background = |in_flight: usize, slots: usize| {
            let mut v = replica_view(tenant, key, in_flight, 0);
            v.pools[0].pool = PoolKey::Model(format!(
                "glm-5-2{}",
                obleth_fairshare::BACKGROUND_POOL_SUFFIX
            ))
            .slot_id();
            v.pools[0].configured_cap = 12;
            v.pools[0].slots = slots;
            v
        };
        // Two replicas: 3 requests on 9 slots, 1 request on 3.
        let views = [background(3, 9), background(1, 3)];
        let algorithm = obleth_config::FairshareAlgorithm::Hierarchical;
        let merged =
            obleth_fairshare::merge_views(algorithm, &views.iter().collect::<Vec<_>>(), 4096);
        let local = obleth_fairshare::merge_views(algorithm, &[&views[0]], 4096);
        let (tenants, keys) = (Default::default(), Default::default());
        let (enabled, serving) = (Default::default(), Default::default());
        let names = LiveNames {
            tenants: &tenants,
            keys: &keys,
            enabled: &enabled,
            serving: &serving,
        };
        let view = live_pool_view(&merged[0], local.first(), None, &names);
        assert!(view
            .model
            .ends_with(obleth_fairshare::BACKGROUND_POOL_SUFFIX));
        assert_eq!((view.in_flight, view.slots, view.cap), (4, 12, 12));
        let json = serde_json::to_value(&view).unwrap();
        assert_eq!(
            (json["in_flight"].clone(), json["slots"].clone()),
            (4.into(), 12.into())
        );
        let own = live_pool_view(&local[0], local.first(), None, &names);
        assert_eq!((own.in_flight, own.slots), (3, 9));

        // An ordinary pool, or a replica that sends no slot count: a slot per
        // request.
        let plain =
            obleth_fairshare::merge_views(algorithm, &[&replica_view(tenant, key, 5, 0)], 4096);
        assert_eq!(live_pool_view(&plain[0], None, None, &names).slots, 5);
    }

    /// The health prober is left out of every level of a merged pool and of
    /// its counts, on the merged side and on the replica's own.
    #[test]
    fn live_pool_view_hides_the_prober_in_a_merged_pool() {
        use obleth_fairshare::{ReplicaGroupView, ReplicaTenantView};
        let (tenant, key) = (Uuid::new_v4(), Uuid::new_v4());
        let mut probed = replica_view(tenant, key, 4, 2);
        let pool = &mut probed.pools[0];
        pool.in_flight += 1;
        pool.groups.push(ReplicaGroupView {
            name: model_health::HEALTH_GROUP.into(),
            weight: 100,
            in_flight: 1,
            queued: 0,
            served: 1.0,
        });
        pool.tenants.push(ReplicaTenantView {
            id: model_health::health_tenant_id(),
            group: model_health::HEALTH_GROUP.into(),
            weight: 100,
            cap: None,
            in_flight: 1,
            queued: 0,
            served: 1.0,
        });
        let other = replica_view(tenant, key, 3, 0);
        let algorithm = obleth_config::FairshareAlgorithm::Hierarchical;
        let merged = obleth_fairshare::merge_views(algorithm, &[&probed, &other], 4096);
        let local = obleth_fairshare::merge_views(algorithm, &[&probed], 4096);
        let empty = std::collections::HashMap::new();
        let none = std::collections::HashSet::new();
        let names = LiveNames {
            tenants: &empty,
            keys: &empty,
            enabled: &none,
            serving: &std::collections::HashSet::new(),
        };
        let view = live_pool_view(&merged[0], local.first(), None, &names);
        assert_eq!((view.in_flight, view.queued), (7, 2));
        assert_eq!((view.replica_in_flight, view.replica_queued), (4, 2));
        assert!(view
            .groups
            .iter()
            .all(|g| g.name != model_health::HEALTH_GROUP));
        assert!(view
            .tenants
            .iter()
            .all(|t| t.tenant_id != model_health::health_tenant_id()));
    }

    /// A pool the answering replica does not have reports zero for its own
    /// counts, and one with no servers says so.
    #[test]
    fn live_pool_view_without_a_local_pool() {
        let (tenant, key) = (Uuid::new_v4(), Uuid::new_v4());
        let merged = obleth_fairshare::merge_views(
            obleth_config::FairshareAlgorithm::Weighted,
            &[&replica_view(tenant, key, 3, 1)],
            4096,
        );
        let empty = std::collections::HashMap::new();
        let enabled = std::collections::HashSet::from(["big"]);
        let serving = std::collections::HashSet::new();
        let names = LiveNames {
            tenants: &empty,
            keys: &empty,
            enabled: &enabled,
            serving: &serving,
        };
        let view = live_pool_view(&merged[0], None, None, &names);
        assert_eq!((view.in_flight, view.queued), (3, 1));
        assert_eq!((view.replica_in_flight, view.replica_queued), (0, 0));
        assert!(view.no_servers);
        assert_eq!(
            view.tenants[0].name,
            tenant.to_string(),
            "unknown names fall back to ids"
        );
    }

    #[test]
    fn enabled_pool_capacity_uses_model_cap_or_default() {
        let mut a = fixture_model_route("a");
        a.max_in_flight = Some(4);
        let mut b = fixture_model_route("b");
        b.max_in_flight = None;
        let mut c = fixture_model_route("c");
        c.enabled = false;
        c.max_in_flight = Some(100);
        assert_eq!(enabled_pool_capacity(&[a, b, c], 32), 36);
    }

    fn routes_with_caps(caps: &[Option<i64>]) -> Vec<ModelRoute> {
        caps.iter()
            .enumerate()
            .map(|(i, cap)| {
                let mut m = fixture_model_route(&format!("m{i}"));
                m.max_in_flight = *cap;
                m
            })
            .collect()
    }

    #[test]
    fn capacity_totals_count_only_models_with_servers() {
        use std::collections::HashSet;
        let fixed = fixture_model_route("fixed"); // has an api_base
        let mut managed = fixture_model_route("managed"); // endpoints only
        managed.api_base = String::new();
        let mut unplaced = fixture_model_route("unplaced"); // neither: nowhere to go
        unplaced.api_base = "  ".into();
        let scaled_down = fixture_model_route("scaled-down"); // discovered, 0 ready
        let mut off = fixture_model_route("off");
        off.enabled = false;
        let models = vec![fixed, managed.clone(), unplaced, scaled_down, off];
        let with_endpoint: HashSet<Uuid> = [managed.id].into_iter().collect();
        let none_ready: HashSet<String> = ["scaled-down".to_string()].into_iter().collect();

        let serving = models_with_servers(&models, &with_endpoint, &none_ready);
        let mut names: Vec<&str> = serving.iter().map(String::as_str).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["fixed", "managed"]);

        // Without its endpoint the managed model has nowhere to go either.
        let serving = models_with_servers(&models, &HashSet::new(), &HashSet::new());
        assert!(!serving.contains("managed"));
        assert!(
            serving.contains("scaled-down"),
            "no discovery verdict: counted by its api_base"
        );
    }

    #[test]
    fn enabled_pool_share_rounds_each_pool_up() {
        let models = routes_with_caps(&[Some(4), None, Some(1)]);
        assert_eq!(enabled_pool_share(&models, 32, 1), 37);
        // ceil(4/3) + ceil(32/3) + ceil(1/3) = 2 + 11 + 1
        assert_eq!(enabled_pool_share(&models, 32, 3), 14);
    }

    #[test]
    fn ceiling_check_compares_like_with_like() {
        let models = routes_with_caps(&[Some(8), Some(8)]);
        // Configured ceiling under the configured pool sum.
        let w = ceiling_check(&models, 32, 12, 1).expect("warns");
        assert_eq!((w.ceiling, w.pool_sum), (12, 16));
        assert!(w.message.contains("OBLETH_GLOBAL_MAX_IN_FLIGHT is below"));
        // Covered, and still covered per replica: 3 replicas hold 3 + 3
        // slots of pools against a 6-slot share of the ceiling.
        assert_eq!(ceiling_check(&models, 32, 16, 3), None);
        assert_eq!(ceiling_check(&models, 32, 64, 5), None);

        // 64 one-slot pools under a 64 ceiling fit configured, but over 2
        // replicas every pool rounds up to 1 while the ceiling halves.
        let tiny = routes_with_caps(&[Some(1); 64]);
        assert_eq!(ceiling_check(&tiny, 32, 64, 1), None);
        let w = ceiling_check(&tiny, 32, 64, 2).expect("warns per replica");
        assert_eq!((w.ceiling, w.pool_sum), (32, 64));
        assert!(w.message.contains("round up per model"));
    }
}
