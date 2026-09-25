//! The `discovered` capacity mode: a model's pool size derived from its live
//! backend instead of typed in.
//!
//! Every gateway replica runs its own loop (nothing is written to Postgres,
//! nothing is audited). Each pass reads every `discovered` model's capacity
//! source, derives
//!
//! ```text
//! configured pool size = max(1, ceil(ready serving replicas x per-replica concurrency x headroom))
//! ```
//!
//! and hands the result to fairshare as the model's configured, cluster-wide
//! pool size ([`FairShare::set_model_caps`]), so the replica-aware division
//! still applies on top: each gateway replica enforces its share of it.
//!
//! Sources:
//! - `endpoints`: the model's enabled, healthy endpoints (all of them under
//!   `load_balance` and `session_hash`, only the one in use under `failover`),
//!   each counted at its own `max_in_flight` or the model's
//!   `per_replica_max_in_flight`. A model with no endpoint rows counts its
//!   single `api_base` while the model is healthy. Needs nothing outside the
//!   gateway.
//! - `kubernetes`: the Ready, non-terminating pods the model's label selector
//!   matches in its namespace (or in every allowed namespace). Which pods
//!   serve is the selector's business: a multi-node deployment whose worker
//!   pods serve nothing excludes them there (for KubeRay,
//!   `ray.io/node-type!=worker`). The per-replica value is the model's own,
//!   else the lowest value a known concurrency flag gives on the serving pods
//!   (see [`concurrency`]).
//!
//! Fallbacks: when a source answers with no serving replica, or does not
//! answer, the last derived value stays (the backend may be restarting or the
//! API server briefly away), or the static `max_in_flight`/default if there
//! is none yet. When a model cannot be discovered as configured (no
//! per-replica value anywhere, no selector, a namespace outside the
//! allowlist), it uses its static value. Each state change is logged once.

pub(crate) mod concurrency;
pub mod kube;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, RwLock};
use std::time::Duration;

use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use obleth_config::capacity::{effective_capacity_selector, DiscoveryPolicy};
use obleth_config::{CapacityDiscoveryConfig, ResolvedEndpoint, ResolvedModel};
use obleth_fairshare::FairShare;
use serde::Serialize;
use utoipa::ToSchema;

pub use kube::KubeClient;

/// Pod lists fetched at once in one pass.
const LIST_CONCURRENCY: usize = 4;

/// Upper bound on a derived pool size, so a runaway flag value or headroom
/// cannot overflow anything downstream.
const MAX_DERIVED: usize = 10_000_000;

/// State of a discovered model: `discovered` (derived this pass), `stale`
/// (the last derived value, kept because the source has no answer right now)
/// or `fallback` (the static `max_in_flight` or the gateway default).
pub const STATE_DISCOVERED: &str = "discovered";
pub const STATE_STALE: &str = "stale";
pub const STATE_FALLBACK: &str = "fallback";

/// Handle to the gateway's capacity discovery. Cheap to clone.
#[derive(Clone)]
pub struct CapacityDiscovery {
    inner: Arc<Inner>,
}

struct Inner {
    settings: CapacityDiscoveryConfig,
    status: RwLock<BTreeMap<String, ModelCapacityStatus>>,
}

/// What discovery knows about one `discovered` model, as this replica sees it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ModelCapacityStatus {
    pub model_name: String,
    /// `endpoints` or `kubernetes`.
    pub source: String,
    /// `kubernetes`: the namespaces searched.
    pub namespaces: Vec<String>,
    /// `kubernetes`: the selector the pods were listed with.
    pub selector: Option<String>,
    /// Serving replicas the source reported on its last answer.
    pub ready_replicas: Option<usize>,
    /// Requests one replica takes, as used in the last derivation.
    pub per_replica_max_in_flight: Option<usize>,
    /// Where that value came from: `per_replica_max_in_flight`, `endpoint
    /// max_in_flight`, or the rule that read it off a container (e.g.
    /// `vLLM --max-num-seqs`).
    pub per_replica_source: Option<String>,
    pub headroom: f64,
    /// The last value derived from the source: the cluster-wide pool size,
    /// before the replica-aware division.
    pub derived_max_in_flight: Option<usize>,
    /// The cluster-wide pool size fairshare uses now: the derived value, the
    /// last one kept while the source has no answer, or the static fallback.
    pub effective_max_in_flight: usize,
    /// `discovered`, `stale` or `fallback`.
    pub state: String,
    /// When this replica last evaluated the model.
    pub last_refresh: Option<DateTime<Utc>>,
    /// When a value was last derived.
    pub last_success: Option<DateTime<Utc>>,
    /// Why the model is not `discovered` right now, or why a value is stale.
    pub reason: Option<String>,
}

impl CapacityDiscovery {
    pub fn new(settings: CapacityDiscoveryConfig) -> Self {
        CapacityDiscovery {
            inner: Arc::new(Inner {
                settings,
                status: RwLock::new(BTreeMap::new()),
            }),
        }
    }

    /// Discovery off, no namespaces, no default selector.
    pub fn disabled() -> Self {
        Self::new(CapacityDiscoveryConfig::default())
    }

    pub fn settings(&self) -> &CapacityDiscoveryConfig {
        &self.inner.settings
    }

    /// What a model write is checked against.
    pub fn policy(&self) -> DiscoveryPolicy<'_> {
        DiscoveryPolicy {
            namespaces: &self.inner.settings.namespaces,
            default_selector: &self.inner.settings.default_selector,
        }
    }

    /// Every model the last pass evaluated.
    pub fn statuses(&self) -> Vec<ModelCapacityStatus> {
        self.inner
            .status
            .read()
            .map(|m| m.values().cloned().collect())
            .unwrap_or_default()
    }

    /// The cluster-wide pool size in force for each discovered model.
    pub fn effective_caps(&self) -> HashMap<String, usize> {
        self.inner
            .status
            .read()
            .map(|m| {
                m.values()
                    .map(|s| (s.model_name.clone(), s.effective_max_in_flight))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Discovered models per state, for metrics.
    pub fn state_counts(&self) -> [(&'static str, usize); 3] {
        let statuses = self.statuses();
        let count = |state: &str| statuses.iter().filter(|s| s.state == state).count();
        [
            (STATE_DISCOVERED, count(STATE_DISCOVERED)),
            (STATE_STALE, count(STATE_STALE)),
            (STATE_FALLBACK, count(STATE_FALLBACK)),
        ]
    }

    fn publish(&self, statuses: BTreeMap<String, ModelCapacityStatus>) {
        if let Ok(mut m) = self.inner.status.write() {
            *m = statuses;
        }
    }

    /// Start the loop when discovery is enabled. `targets` returns the
    /// models to read each pass (only those in the `discovered` mode are
    /// kept), `default_cap` is the gateway's default pool size, the static
    /// fallback of a model with no `max_in_flight`.
    pub fn spawn(
        &self,
        fairshare: FairShare,
        default_cap: usize,
        targets: TargetsFn,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let settings = self.settings().clone();
        if !settings.enabled {
            tracing::info!(
                "capacity discovery is off (OBLETH_CAPACITY_DISCOVERY_ENABLED); discovered \
                 models use their static max_in_flight"
            );
            return None;
        }
        let kube = if settings.namespaces.is_empty() {
            None
        } else {
            let client = KubeClient::in_cluster();
            if let Err(e) = &client {
                tracing::warn!(
                    error = %e,
                    "the kubernetes capacity source is unavailable; its models use their \
                     static max_in_flight"
                );
            }
            Some(client)
        };
        tracing::info!(
            interval_secs = settings.interval.as_secs(),
            namespaces = %settings.namespaces.join(","),
            kubernetes = kube.as_ref().is_some_and(|k| k.is_ok()),
            "capacity discovery is on"
        );
        let handle = self.clone();
        let mut discoverer = Discoverer::new(kube, fairshare, default_cap);
        Some(tokio::spawn(async move {
            let mut tick = tokio::time::interval(settings.interval.max(Duration::from_secs(1)));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                discoverer.pass(&handle, targets()).await;
            }
        }))
    }
}

/// Returns the models a pass reads. Supplied by the binary that owns the model
/// registry, so this crate needs no Postgres read per pass.
pub type TargetsFn = Arc<dyn Fn() -> Vec<DiscoveryTarget> + Send + Sync>;

/// One `discovered` model, with what its source needs.
#[derive(Debug, Clone)]
pub struct DiscoveryTarget {
    pub model_name: String,
    pub upstream_model: String,
    /// The model's own `max_in_flight`, the static fallback.
    pub static_max_in_flight: Option<usize>,
    pub source: String,
    pub namespace: Option<String>,
    pub selector: Option<String>,
    pub per_replica_max_in_flight: Option<usize>,
    pub headroom: f64,
    /// False while the model is reported down or in maintenance; decides
    /// whether a model with no endpoint rows counts its `api_base`.
    pub healthy: bool,
    pub api_base: String,
    pub endpoint_selection_mode: String,
    pub endpoints: Vec<ResolvedEndpoint>,
}

impl DiscoveryTarget {
    /// The target for a resolved model, or `None` unless it is `discovered`.
    pub fn from_resolved(m: &ResolvedModel, healthy: bool) -> Option<Self> {
        (m.capacity_mode == obleth_config::DISCOVERED_CAPACITY_MODE).then(|| DiscoveryTarget {
            model_name: m.model_name.clone(),
            upstream_model: m.upstream_model.clone(),
            static_max_in_flight: m.max_in_flight.filter(|c| *c > 0),
            source: m.capacity_source.clone(),
            namespace: m.capacity_namespace.clone(),
            selector: m.capacity_selector.clone(),
            per_replica_max_in_flight: m.per_replica_max_in_flight.filter(|c| *c > 0),
            headroom: m.capacity_headroom,
            healthy,
            api_base: m.api_base.clone(),
            endpoint_selection_mode: m.endpoint_selection_mode.clone(),
            endpoints: m.endpoints.clone(),
        })
    }
}

/// What one pass found for one model.
#[derive(Debug, Clone, PartialEq)]
enum Outcome {
    /// Serving replicas and a per-replica value: a pool size.
    Observed {
        ready: usize,
        per_replica: usize,
        per_source: String,
    },
    /// The source answered, but nothing is serving.
    NoReplicas { reason: String },
    /// The source did not answer.
    Failed { reason: String },
    /// The model cannot be discovered as configured.
    Unusable {
        reason: String,
        ready: Option<usize>,
    },
}

/// Per-model memory across passes.
#[derive(Debug, Default)]
struct Memory {
    derived: Option<usize>,
    last_success: Option<DateTime<Utc>>,
    /// `(state, reason)` last logged, so a steady state is logged once.
    logged: Option<(String, Option<String>)>,
}

/// The loop's state: one per gateway replica.
pub struct Discoverer {
    kube: Option<Result<KubeClient, String>>,
    fairshare: FairShare,
    default_cap: usize,
    memory: HashMap<String, Memory>,
    sent: Option<HashMap<String, usize>>,
}

impl Discoverer {
    /// `kube` is `None` when no namespace is allowed (the `kubernetes` source
    /// is unavailable), or the error building the in-cluster client.
    pub fn new(
        kube: Option<Result<KubeClient, String>>,
        fairshare: FairShare,
        default_cap: usize,
    ) -> Self {
        Discoverer {
            kube,
            fairshare,
            default_cap: default_cap.max(1),
            memory: HashMap::new(),
            sent: None,
        }
    }

    /// One pass: read every source, derive, publish the statuses, and resize
    /// fairshare when any pool size changed.
    pub async fn pass(&mut self, handle: &CapacityDiscovery, targets: Vec<DiscoveryTarget>) {
        let now = Utc::now();
        let settings = handle.settings();
        let targets: Vec<DiscoveryTarget> = targets
            .into_iter()
            .filter(|t| !t.model_name.is_empty())
            .collect();

        // Plan the kubernetes reads: one list per distinct namespace and
        // selector, however many models share them.
        let mut plans: HashMap<String, Result<KubePlan, String>> = HashMap::new();
        let mut queries: BTreeSet<(String, String)> = BTreeSet::new();
        for t in targets.iter().filter(|t| t.source == "kubernetes") {
            let plan = self.plan(t, settings);
            if let Ok(p) = &plan {
                for ns in &p.namespaces {
                    queries.insert((ns.clone(), p.selector.clone()));
                }
            }
            plans.insert(t.model_name.clone(), plan);
        }
        let lists = match &self.kube {
            Some(Ok(client)) if !queries.is_empty() => {
                stream::iter(queries.into_iter().map(|(ns, sel)| {
                    let client = client.clone();
                    async move {
                        let res = client.list_pods(&ns, &sel).await;
                        ((ns, sel), res)
                    }
                }))
                .buffer_unordered(LIST_CONCURRENCY)
                .collect::<HashMap<_, _>>()
                .await
            }
            _ => HashMap::new(),
        };

        let mut statuses = BTreeMap::new();
        let mut caps = HashMap::new();
        for t in &targets {
            let (outcome, namespaces, selector) = match t.source.as_str() {
                "endpoints" => (endpoints_outcome(t), Vec::new(), None),
                "kubernetes" => match plans.remove(&t.model_name) {
                    Some(Ok(plan)) => {
                        let outcome = kubernetes_outcome(t, &plan, &lists);
                        (outcome, plan.namespaces, Some(plan.selector))
                    }
                    Some(Err(reason)) => (
                        Outcome::Unusable {
                            reason,
                            ready: None,
                        },
                        Vec::new(),
                        None,
                    ),
                    None => unreachable!("every kubernetes target was planned"),
                },
                other => (
                    Outcome::Unusable {
                        reason: format!("unknown capacity_source `{other}`"),
                        ready: None,
                    },
                    Vec::new(),
                    None,
                ),
            };
            let status = self.settle(t, outcome, namespaces, selector, now);
            caps.insert(t.model_name.clone(), status.effective_max_in_flight);
            statuses.insert(t.model_name.clone(), status);
        }
        self.memory.retain(|name, _| statuses.contains_key(name));
        handle.publish(statuses);

        if self.sent.as_ref() != Some(&caps) {
            self.fairshare.set_model_caps(caps.clone());
            self.sent = Some(caps);
        }
    }

    /// Where and how to list a kubernetes-source model's pods.
    fn plan(
        &self,
        t: &DiscoveryTarget,
        settings: &CapacityDiscoveryConfig,
    ) -> Result<KubePlan, String> {
        match &self.kube {
            None => {
                return Err(
                    "the kubernetes capacity source is not available on this gateway: \
                            OBLETH_CAPACITY_DISCOVERY_NAMESPACES is empty"
                        .into(),
                )
            }
            Some(Err(e)) => {
                return Err(format!(
                    "the kubernetes capacity source is unavailable: {e}"
                ))
            }
            Some(Ok(_)) => {}
        }
        let namespaces = match &t.namespace {
            Some(ns) if settings.namespaces.iter().any(|a| a == ns) => vec![ns.clone()],
            Some(ns) => {
                return Err(format!(
                    "capacity_namespace `{ns}` is not in OBLETH_CAPACITY_DISCOVERY_NAMESPACES ({})",
                    settings.namespaces.join(", ")
                ))
            }
            None => settings.namespaces.clone(),
        };
        let selector = effective_capacity_selector(
            t.selector.as_deref(),
            &settings.default_selector,
            &t.upstream_model,
            &t.model_name,
        )?;
        Ok(KubePlan {
            namespaces,
            selector,
        })
    }

    /// Turn an outcome into the model's status, updating its memory and
    /// logging a state change once.
    fn settle(
        &mut self,
        t: &DiscoveryTarget,
        outcome: Outcome,
        namespaces: Vec<String>,
        selector: Option<String>,
        now: DateTime<Utc>,
    ) -> ModelCapacityStatus {
        let static_cap = t.static_max_in_flight.unwrap_or(self.default_cap);
        let no_replicas = matches!(outcome, Outcome::NoReplicas { .. });
        let mem = self.memory.entry(t.model_name.clone()).or_default();
        let previous = mem.derived;
        let mut status = ModelCapacityStatus {
            model_name: t.model_name.clone(),
            source: t.source.clone(),
            namespaces,
            selector,
            ready_replicas: None,
            per_replica_max_in_flight: None,
            per_replica_source: None,
            headroom: t.headroom,
            derived_max_in_flight: None,
            effective_max_in_flight: static_cap,
            state: STATE_FALLBACK.into(),
            last_refresh: Some(now),
            last_success: None,
            reason: None,
        };
        match outcome {
            Outcome::Observed {
                ready,
                per_replica,
                per_source,
            } => {
                let derived = derive(ready, per_replica, t.headroom);
                mem.derived = Some(derived);
                mem.last_success = Some(now);
                status.ready_replicas = Some(ready);
                status.per_replica_max_in_flight = Some(per_replica);
                status.per_replica_source = Some(per_source);
                status.effective_max_in_flight = derived;
                status.state = STATE_DISCOVERED.into();
                if previous.is_some_and(|p| p != derived) {
                    tracing::info!(
                        model = %t.model_name,
                        ready_replicas = ready,
                        per_replica,
                        from = previous.unwrap_or_default(),
                        to = derived,
                        "discovered capacity changed; resizing the model's pool"
                    );
                }
            }
            Outcome::NoReplicas { reason } | Outcome::Failed { reason } => {
                status.ready_replicas = no_replicas.then_some(0);
                if let Some(kept) = mem.derived {
                    status.effective_max_in_flight = kept;
                    status.state = STATE_STALE.into();
                    status.reason = Some(format!("{reason}; keeping the last discovered value"));
                } else {
                    status.reason = Some(format!("{reason}; using the static max_in_flight"));
                }
            }
            Outcome::Unusable { reason, ready } => {
                mem.derived = None;
                status.ready_replicas = ready;
                status.reason = Some(format!("{reason}; using the static max_in_flight"));
            }
        }
        status.derived_max_in_flight = mem.derived;
        status.last_success = mem.last_success;

        let signature = (status.state.clone(), status.reason.clone());
        if mem.logged.as_ref() != Some(&signature) {
            match status.state.as_str() {
                STATE_DISCOVERED => tracing::info!(
                    model = %t.model_name,
                    source = %t.source,
                    ready_replicas = status.ready_replicas.unwrap_or_default(),
                    per_replica = status.per_replica_max_in_flight.unwrap_or_default(),
                    per_replica_source = status.per_replica_source.as_deref().unwrap_or(""),
                    max_in_flight = status.effective_max_in_flight,
                    "capacity discovered"
                ),
                _ => tracing::warn!(
                    model = %t.model_name,
                    source = %t.source,
                    state = %status.state,
                    max_in_flight = status.effective_max_in_flight,
                    reason = status.reason.as_deref().unwrap_or(""),
                    "capacity not discovered"
                ),
            }
            mem.logged = Some(signature);
        }
        status
    }
}

struct KubePlan {
    namespaces: Vec<String>,
    selector: String,
}

/// `max(1, ceil(ready x per_replica x headroom))`, bounded.
fn derive(ready: usize, per_replica: usize, headroom: f64) -> usize {
    let base = ready.saturating_mul(per_replica) as f64;
    let scaled = (base * headroom).ceil();
    if !scaled.is_finite() || scaled >= MAX_DERIVED as f64 {
        MAX_DERIVED
    } else {
        (scaled as usize).max(1)
    }
}

/// The `endpoints` source.
fn endpoints_outcome(t: &DiscoveryTarget) -> Outcome {
    let legacy;
    let all: &[ResolvedEndpoint] = if t.endpoints.is_empty() {
        if t.api_base.trim().is_empty() {
            return Outcome::NoReplicas {
                reason: "the model has no endpoint and no api_base".into(),
            };
        }
        // A model with no endpoint rows serves from its single api_base.
        legacy = [ResolvedEndpoint {
            id: String::new(),
            api_base: t.api_base.clone(),
            api_key: None,
            priority: 0,
            weight: 1,
            enabled: true,
            healthy: t.healthy,
            max_in_flight: None,
        }];
        &legacy
    } else {
        &t.endpoints
    };
    let mut serving: Vec<&ResolvedEndpoint> =
        all.iter().filter(|e| e.enabled && e.healthy).collect();
    if t.endpoint_selection_mode == "failover" {
        // Only the endpoint in use takes traffic; the rest are standbys.
        serving.sort_by_key(|e| e.priority);
        serving.truncate(1);
    }
    if serving.is_empty() {
        return Outcome::NoReplicas {
            reason: "no enabled, healthy endpoint".into(),
        };
    }
    let ready = serving.len();
    let mut values = Vec::with_capacity(ready);
    let mut from_endpoint = false;
    let mut from_model = false;
    for e in &serving {
        match (
            e.max_in_flight.filter(|v| *v > 0),
            t.per_replica_max_in_flight,
        ) {
            (Some(v), _) => {
                from_endpoint = true;
                values.push(v);
            }
            (None, Some(v)) => {
                from_model = true;
                values.push(v);
            }
            (None, None) => {
                return Outcome::Unusable {
                    reason: "an endpoint in use has no max_in_flight and the model has no \
                             per_replica_max_in_flight"
                        .into(),
                    ready: Some(ready),
                }
            }
        }
    }
    let total: usize = values.iter().sum();
    let uniform = values.iter().all(|v| *v == values[0]);
    let per_source = match (from_endpoint, from_model) {
        (true, false) => "endpoint max_in_flight",
        (false, true) => "per_replica_max_in_flight",
        _ => "endpoint max_in_flight, else per_replica_max_in_flight",
    };
    Outcome::Observed {
        ready,
        // Endpoints that differ are summed; the per-replica figure shown is
        // then the average, rounded up, so ready x per-replica still reads
        // right.
        per_replica: if uniform {
            values[0]
        } else {
            total.div_ceil(ready)
        },
        per_source: per_source.into(),
    }
}

/// The `kubernetes` source, from the pod lists fetched this pass.
fn kubernetes_outcome(
    t: &DiscoveryTarget,
    plan: &KubePlan,
    lists: &HashMap<(String, String), Result<Vec<kube::Pod>, String>>,
) -> Outcome {
    let mut pods: Vec<&kube::Pod> = Vec::new();
    for ns in &plan.namespaces {
        match lists.get(&(ns.clone(), plan.selector.clone())) {
            Some(Ok(list)) => pods.extend(list.iter()),
            Some(Err(e)) => return Outcome::Failed { reason: e.clone() },
            None => {
                return Outcome::Failed {
                    reason: "the Kubernetes API was not asked".into(),
                }
            }
        }
    }
    let serving: Vec<&kube::Pod> = pods.iter().copied().filter(|p| p.is_serving()).collect();
    if serving.is_empty() {
        return Outcome::NoReplicas {
            reason: format!(
                "no Ready pod matches `{}` in {} ({} matched)",
                plan.selector,
                plan.namespaces.join(", "),
                pods.len()
            ),
        };
    }
    let ready = serving.len();
    if let Some(v) = t.per_replica_max_in_flight {
        return Outcome::Observed {
            ready,
            per_replica: v,
            per_source: "per_replica_max_in_flight".into(),
        };
    }
    // The lowest value across the serving pods: during a rollout that changes
    // it, the old pods still take only what they were started with.
    let found = serving
        .iter()
        .filter_map(|p| concurrency::detect(&p.spec.containers))
        .min_by_key(|d| d.value);
    match found {
        Some(d) => Outcome::Observed {
            ready,
            per_replica: d.value,
            per_source: d.rule,
        },
        None => Outcome::Unusable {
            reason: format!(
                "no known concurrency setting on the serving containers ({}); set \
                 per_replica_max_in_flight",
                concurrency::RULES
                    .iter()
                    .map(|r| r.server)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            ready: Some(ready),
        },
    }
}

#[cfg(test)]
mod tests;
