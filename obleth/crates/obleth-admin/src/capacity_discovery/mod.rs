//! The `discovered` capacity mode: a model's pool size derived from its live
//! backend instead of typed in.
//!
//! Every gateway replica runs its own loop (nothing is written to Postgres,
//! nothing is audited). Each pass reads every `discovered` model's capacity
//! source, derives
//!
//! ```text
//! configured pool size = max(1, ceil(summed concurrency of the ready replicas x headroom))
//! ```
//!
//! where the summed concurrency is ready replicas x the per-replica value, or,
//! for endpoints that carry their own `max_in_flight`, each ready endpoint's
//! value added up.
//!
//! and hands the result to fairshare as the model's configured, cluster-wide
//! pool size ([`FairShare::set_model_caps`]), enforced across the gateway
//! replicas like any configured size: as shared slots, or divided across
//! them when shared slots are off or unavailable.
//!
//! Sources:
//! - `endpoints`: the model's enabled, healthy endpoints (all of them under
//!   `load_balance` and `session_hash`, only the one in use under `failover`),
//!   each counted at its own `max_in_flight`, or the model's
//!   `per_replica_max_in_flight` when it has none, and summed (8, 2 and an
//!   unset one with a per-replica value of 4 make 14). A model with no
//!   endpoint rows counts its
//!   single `api_base` while the model is healthy. Needs nothing outside the
//!   gateway.
//! - `kubernetes`: the ready, serving, non-terminating endpoints of the
//!   model's Service, read from its EndpointSlices (see [`kube`]), each
//!   counted at the model's `per_replica_max_in_flight`. Only replica counts
//!   are read: no pod, no spec, no environment. Which pods a Service counts is
//!   its own selector's business, so a multi-node deployment where only some
//!   pods take requests is counted right by a Service that selects just
//!   those. A model that names no namespace is looked up in the allowed
//!   namespaces in order, and the first namespace that has the Service wins.
//!
//! The per-replica concurrency is always the operator's (it rarely changes,
//! and reading it off a server's command line would mean reading pod specs).
//!
//! Fallbacks: when a source answers with no serving replica (the Service
//! scaled to zero, every pod restarting, the Service not there), or does not
//! answer, the last derived value stays (the backend may be restarting or the
//! API server briefly away), or the static `max_in_flight`/default if there
//! is none yet. When a model cannot be discovered as configured (no
//! per-replica value, no Service, a namespace outside the allowlist), it uses
//! its static value. Each state change is logged once.

pub mod kube;

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures::stream::{self, StreamExt};
use obleth_config::capacity::{
    effective_capacity_service, validate_capacity_namespace, DiscoveryPolicy,
};
use obleth_config::{CapacityDiscoveryConfig, ResolvedEndpoint, ResolvedModel};
use obleth_fairshare::FairShare;
use serde::Serialize;
use utoipa::ToSchema;

pub use kube::KubeClient;

/// EndpointSlice lists fetched at once in one pass.
const LIST_CONCURRENCY: usize = 4;

/// Upper bound on a derived pool size, so a runaway per-replica value or
/// headroom cannot overflow anything downstream.
const MAX_DERIVED: usize = 10_000_000;

/// State of a discovered model: `discovered` (derived this pass), `stale`
/// (the last derived value, kept because the source has no answer right now)
/// or `fallback` (the static `max_in_flight` or the gateway default).
pub const STATE_DISCOVERED: &str = "discovered";
pub const STATE_STALE: &str = "stale";
pub const STATE_FALLBACK: &str = "fallback";

/// Where a per-replica value came from: the model's
/// `per_replica_max_in_flight`, each endpoint's own `max_in_flight`, or both.
pub const PER_REPLICA_CONFIGURED: &str = "configured";
pub const PER_REPLICA_ENDPOINT: &str = "endpoint";
pub const PER_REPLICA_BOTH: &str = "endpoint and configured";

/// How long a Service listing is served from cache, so a form open on the
/// dashboard cannot hammer the API server.
pub const SERVICES_CACHE_TTL: Duration = Duration::from_secs(15);

/// Handle to the gateway's capacity discovery. Cheap to clone.
#[derive(Clone)]
pub struct CapacityDiscovery {
    inner: Arc<Inner>,
}

struct Inner {
    settings: CapacityDiscoveryConfig,
    status: RwLock<BTreeMap<String, ModelCapacityStatus>>,
    /// The Kubernetes client, built on first use when namespaces are set.
    kube: OnceLock<Result<KubeClient, String>>,
    /// The last Service listing and when it was taken. The lock also makes
    /// concurrent callers wait for one listing instead of each making one.
    services: tokio::sync::Mutex<Option<(Instant, ServiceListing)>>,
    services_ttl: Duration,
}

/// One Service the `kubernetes` source can see.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ServiceSummary {
    pub service: String,
    pub namespace: String,
    /// Ready, serving, non-terminating endpoints, counted as discovery
    /// counts them.
    pub ready: usize,
}

/// The Services visible to the `kubernetes` source, from EndpointSlices in
/// the allowed namespaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct ServiceListing {
    /// Sorted by namespace (in allowlist order), then Service name.
    pub services: Vec<ServiceSummary>,
    /// Why the list is empty or incomplete, when it is: discovery off, no
    /// namespaces configured, no Kubernetes client, or no namespace answering.
    pub reason: Option<String>,
    /// Namespaces that could not be listed, with why.
    pub errors: Vec<String>,
}

impl ServiceListing {
    fn unavailable(reason: impl Into<String>) -> Self {
        ServiceListing {
            services: Vec::new(),
            reason: Some(reason.into()),
            errors: Vec::new(),
        }
    }
}

/// What discovery knows about one `discovered` model, as this replica sees it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct ModelCapacityStatus {
    pub model_name: String,
    /// `endpoints` or `kubernetes`.
    pub source: String,
    /// `kubernetes`: the namespaces the Service is looked up in, in order.
    pub namespaces: Vec<String>,
    /// `kubernetes`: the namespace the Service was found in on the last
    /// answer.
    pub namespace: Option<String>,
    /// `kubernetes`: the Service whose ready endpoints are counted.
    pub service: Option<String>,
    /// Serving replicas the source reported on its last answer.
    pub ready_replicas: Option<usize>,
    /// Requests one replica takes, as used in the last derivation. `None`
    /// when the ready replicas' values differ (endpoints with their own
    /// `max_in_flight`); `replica_capacity` then has their sum.
    pub per_replica_max_in_flight: Option<usize>,
    /// Where that value came from: `configured` (the model's
    /// `per_replica_max_in_flight`), `endpoint` (each endpoint's own
    /// `max_in_flight`), or `endpoint and configured` when both were used.
    pub per_replica_source: Option<String>,
    /// The ready replicas' concurrency summed, as used in the last
    /// derivation: ready replicas x the per-replica value, or each
    /// endpoint's own value added up.
    pub replica_capacity: Option<usize>,
    pub headroom: f64,
    /// The last value derived from the source: the cluster-wide pool size.
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
        Self::build(settings, None, SERVICES_CACHE_TTL)
    }

    /// Discovery that reads Kubernetes through `client` rather than the
    /// in-cluster client, e.g. a fake API server in tests.
    pub fn with_kube(settings: CapacityDiscoveryConfig, client: KubeClient) -> Self {
        Self::build(settings, Some(client), SERVICES_CACHE_TTL)
    }

    fn build(
        settings: CapacityDiscoveryConfig,
        client: Option<KubeClient>,
        services_ttl: Duration,
    ) -> Self {
        let kube = OnceLock::new();
        if let Some(client) = client {
            let _ = kube.set(Ok(client));
        }
        CapacityDiscovery {
            inner: Arc::new(Inner {
                settings,
                status: RwLock::new(BTreeMap::new()),
                kube,
                services: tokio::sync::Mutex::new(None),
                services_ttl,
            }),
        }
    }

    /// The Kubernetes client, or why there is none. Built once.
    fn kube_client(&self) -> Option<Result<KubeClient, String>> {
        if self.inner.settings.namespaces.is_empty() {
            return None;
        }
        Some(self.inner.kube.get_or_init(KubeClient::in_cluster).clone())
    }

    /// Every Service in the allowed namespaces with its ready endpoint
    /// count, built from EndpointSlices only (the one read the gateway's
    /// Role allows), and cached for [`SERVICES_CACHE_TTL`].
    pub async fn list_services(&self) -> ServiceListing {
        let settings = &self.inner.settings;
        if !settings.enabled {
            return ServiceListing::unavailable(
                "capacity discovery is off on this gateway (OBLETH_CAPACITY_DISCOVERY_ENABLED)",
            );
        }
        let client = match self.kube_client() {
            None => {
                return ServiceListing::unavailable(
                    "no namespaces are configured for the kubernetes source \
                     (OBLETH_CAPACITY_DISCOVERY_NAMESPACES)",
                )
            }
            Some(Err(e)) => return ServiceListing::unavailable(e),
            Some(Ok(client)) => client,
        };
        let mut cache = self.inner.services.lock().await;
        if let Some((at, listing)) = cache.as_ref() {
            if at.elapsed() < self.inner.services_ttl {
                return listing.clone();
            }
        }
        let lists = stream::iter(settings.namespaces.iter().cloned())
            .map(|ns| {
                let client = client.clone();
                async move {
                    let result = client.list_service_endpoint_slices(&ns).await;
                    (ns, result)
                }
            })
            .buffered(LIST_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        let mut services = Vec::new();
        let mut errors = Vec::new();
        for (ns, result) in lists {
            match result {
                Ok(slices) => {
                    for (service, ready) in kube::ready_by_service(&slices) {
                        services.push(ServiceSummary {
                            service,
                            namespace: ns.clone(),
                            ready,
                        });
                    }
                }
                Err(e) => errors.push(e),
            }
        }
        let reason = if !errors.is_empty() && errors.len() == settings.namespaces.len() {
            Some("no allowed namespace could be listed".to_string())
        } else if services.is_empty() {
            Some(format!(
                "no Services found in {}",
                settings.namespaces.join(", ")
            ))
        } else {
            None
        };
        let listing = ServiceListing {
            services,
            reason,
            errors,
        };
        *cache = Some((Instant::now(), listing.clone()));
        listing
    }

    /// The Service a model that names none would use: the default template
    /// rendered for it, in the first allowed namespace that has it, as
    /// discovery looks it up. `None` when the template is empty, does not
    /// render to a valid name, or no listed namespace has the Service.
    pub fn default_match(
        &self,
        listing: &ServiceListing,
        upstream_model: &str,
        model_name: &str,
    ) -> (Option<String>, Option<ServiceSummary>) {
        let name = effective_capacity_service(
            None,
            &self.inner.settings.default_service,
            upstream_model,
            model_name,
        )
        .ok();
        let found = name.as_ref().and_then(|name| {
            self.inner.settings.namespaces.iter().find_map(|ns| {
                listing
                    .services
                    .iter()
                    .find(|s| &s.service == name && &s.namespace == ns)
                    .cloned()
            })
        });
        (name, found)
    }

    /// Discovery off, no namespaces, no default Service.
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
            default_service: &self.inner.settings.default_service,
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
        let kube = self.kube_client();
        if let Some(Err(e)) = &kube {
            tracing::warn!(
                error = %e,
                "the kubernetes capacity source is unavailable; its models use their \
                 static max_in_flight"
            );
        }
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
    pub service: Option<String>,
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
            service: m.capacity_service.clone(),
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
    /// Serving replicas and their summed concurrency: a pool size.
    Observed {
        ready: usize,
        /// The value every ready replica shares; `None` when they differ.
        per_replica: Option<usize>,
        /// The ready replicas' concurrency, summed.
        capacity: usize,
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

        let mut plans: HashMap<String, Result<KubePlan, String>> = HashMap::new();
        for t in targets.iter().filter(|t| t.source == "kubernetes") {
            plans.insert(t.model_name.clone(), self.plan(t, settings));
        }
        let lookups = match &self.kube {
            Some(Ok(client)) => lookup_services(client, plans.values().flatten()).await,
            _ => HashMap::new(),
        };

        let mut statuses = BTreeMap::new();
        let mut caps = HashMap::new();
        for t in &targets {
            let mut seen = Seen::default();
            let outcome = match t.source.as_str() {
                "endpoints" => endpoints_outcome(t),
                "kubernetes" => match plans.remove(&t.model_name) {
                    Some(Ok(plan)) => {
                        let (outcome, found) = kubernetes_outcome(t, &plan, &lookups);
                        seen = Seen {
                            namespaces: plan.namespaces,
                            namespace: found,
                            service: Some(plan.service),
                        };
                        outcome
                    }
                    Some(Err(reason)) => Outcome::Unusable {
                        reason,
                        ready: None,
                    },
                    None => unreachable!("every kubernetes target was planned"),
                },
                other => Outcome::Unusable {
                    reason: format!("unknown capacity_source `{other}`"),
                    ready: None,
                },
            };
            let status = self.settle(t, outcome, seen, now);
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

    /// Where to look up a kubernetes-source model's Service, and which one.
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
        if t.per_replica_max_in_flight.is_none() {
            return Err(
                "per_replica_max_in_flight is required on the kubernetes source (the requests \
                 one backend replica serves at once)"
                    .into(),
            );
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
        for ns in &namespaces {
            validate_capacity_namespace(ns)
                .map_err(|e| format!("OBLETH_CAPACITY_DISCOVERY_NAMESPACES: {e}"))?;
        }
        let service = effective_capacity_service(
            t.service.as_deref(),
            &settings.default_service,
            &t.upstream_model,
            &t.model_name,
        )?;
        Ok(KubePlan {
            namespaces,
            service,
        })
    }

    /// Turn an outcome into the model's status, updating its memory and
    /// logging a state change once.
    fn settle(
        &mut self,
        t: &DiscoveryTarget,
        outcome: Outcome,
        seen: Seen,
        now: DateTime<Utc>,
    ) -> ModelCapacityStatus {
        let static_cap = t.static_max_in_flight.unwrap_or(self.default_cap);
        let no_replicas = matches!(outcome, Outcome::NoReplicas { .. });
        let mem = self.memory.entry(t.model_name.clone()).or_default();
        let previous = mem.derived;
        let mut status = ModelCapacityStatus {
            model_name: t.model_name.clone(),
            source: t.source.clone(),
            namespaces: seen.namespaces,
            namespace: seen.namespace,
            service: seen.service,
            ready_replicas: None,
            per_replica_max_in_flight: None,
            per_replica_source: None,
            replica_capacity: None,
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
                capacity,
                per_source,
            } => {
                let derived = derive(capacity, t.headroom);
                mem.derived = Some(derived);
                mem.last_success = Some(now);
                status.ready_replicas = Some(ready);
                status.per_replica_max_in_flight = per_replica;
                status.replica_capacity = Some(capacity);
                status.per_replica_source = Some(per_source);
                status.effective_max_in_flight = derived;
                status.state = STATE_DISCOVERED.into();
                if previous.is_some_and(|p| p != derived) {
                    tracing::info!(
                        model = %t.model_name,
                        ready_replicas = ready,
                        replica_capacity = capacity,
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
                    replica_capacity = status.replica_capacity.unwrap_or_default(),
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

/// Where a kubernetes-source model's Service is looked up.
struct KubePlan {
    /// In order; the first that has the Service wins.
    namespaces: Vec<String>,
    service: String,
}

/// What a kubernetes-source model's status shows about where it looked.
#[derive(Default)]
struct Seen {
    namespaces: Vec<String>,
    namespace: Option<String>,
    service: Option<String>,
}

/// EndpointSlice lists by `(namespace, service)`.
type Lookups = HashMap<(String, String), Result<Vec<kube::EndpointSlice>, String>>;

/// The first namespace in `plan` that has not been read yet and is still
/// needed: every namespace before it answered with no slice for the Service.
/// `None` once the plan is decided (found, failed, or every namespace empty).
fn next_lookup(plan: &KubePlan, lookups: &Lookups) -> Option<(String, String)> {
    for ns in &plan.namespaces {
        match lookups.get(&(ns.clone(), plan.service.clone())) {
            None => return Some((ns.clone(), plan.service.clone())),
            Some(Ok(slices)) if slices.is_empty() => continue,
            Some(_) => return None,
        }
    }
    None
}

/// Read the EndpointSlices every plan needs, one round per namespace depth:
/// each round lists, concurrently and once per distinct namespace and
/// Service, the next namespace of every plan whose Service has not been
/// found yet. A plan pinned to one namespace takes one round, and a later
/// namespace is only read when the earlier ones do not have the Service.
async fn lookup_services<'a>(
    client: &KubeClient,
    plans: impl Iterator<Item = &'a KubePlan> + Clone,
) -> Lookups {
    let mut lookups = Lookups::new();
    loop {
        let wanted: BTreeSet<(String, String)> = plans
            .clone()
            .filter_map(|p| next_lookup(p, &lookups))
            .collect();
        if wanted.is_empty() {
            return lookups;
        }
        let answers: Vec<_> = stream::iter(wanted.into_iter().map(|(ns, svc)| {
            let client = client.clone();
            async move {
                let res = client.list_endpoint_slices(&ns, &svc).await;
                ((ns, svc), res)
            }
        }))
        .buffer_unordered(LIST_CONCURRENCY)
        .collect()
        .await;
        lookups.extend(answers);
    }
}

/// `max(1, ceil(capacity x headroom))`, bounded, where `capacity` is the
/// ready replicas' concurrency summed.
fn derive(capacity: usize, headroom: f64) -> usize {
    let base = capacity as f64;
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
    let per_source = match (from_endpoint, from_model) {
        (true, false) => PER_REPLICA_ENDPOINT,
        (false, true) => PER_REPLICA_CONFIGURED,
        _ => PER_REPLICA_BOTH,
    };
    Outcome::Observed {
        ready,
        per_replica: values.iter().all(|v| *v == values[0]).then_some(values[0]),
        // Each ready endpoint at its own value, added up: an average would
        // round a mix like 8, 2 and 4 up to 3 x 5.
        capacity: values.iter().fold(0usize, |a, v| a.saturating_add(*v)),
        per_source: per_source.into(),
    }
}

/// The `kubernetes` source, from the EndpointSlices read this pass, and the
/// namespace the Service was found in.
fn kubernetes_outcome(
    t: &DiscoveryTarget,
    plan: &KubePlan,
    lookups: &Lookups,
) -> (Outcome, Option<String>) {
    let service = &plan.service;
    let Some(per_replica) = t.per_replica_max_in_flight else {
        // Planning refuses this already; kept so a count never goes out
        // without its per-replica value.
        return (
            Outcome::Unusable {
                reason: "per_replica_max_in_flight is required on the kubernetes source".into(),
                ready: None,
            },
            None,
        );
    };
    for ns in &plan.namespaces {
        match lookups.get(&(ns.clone(), service.clone())) {
            Some(Ok(slices)) if slices.is_empty() => continue,
            Some(Ok(slices)) => {
                let count = kube::count_endpoints(slices);
                let outcome = if count.ready == 0 {
                    Outcome::NoReplicas {
                        reason: format!(
                            "Service {service} in {ns} has no ready endpoint ({} listed)",
                            count.total
                        ),
                    }
                } else {
                    Outcome::Observed {
                        ready: count.ready,
                        per_replica: Some(per_replica),
                        capacity: count.ready.saturating_mul(per_replica),
                        per_source: PER_REPLICA_CONFIGURED.into(),
                    }
                };
                return (outcome, Some(ns.clone()));
            }
            Some(Err(e)) => return (Outcome::Failed { reason: e.clone() }, None),
            None => {
                return (
                    Outcome::Failed {
                        reason: "the Kubernetes API was not asked".into(),
                    },
                    None,
                )
            }
        }
    }
    (
        Outcome::NoReplicas {
            reason: format!(
                "no EndpointSlice for Service {service} in {}; does the Service exist there?",
                plan.namespaces.join(", ")
            ),
        },
        None,
    )
}

#[cfg(test)]
mod tests;
