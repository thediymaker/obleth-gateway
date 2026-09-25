//! Discovery passes against a fake Kubernetes API (a local HTTP server that
//! answers pod lists) and against a model's own endpoints, feeding a real
//! fairshare scheduler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use obleth_config::{CapacityDiscoveryConfig, FairshareAlgorithm, ResolvedEndpoint};
use obleth_fairshare::{AdmitRequest, FairShare, StaticCapacity};
use serde_json::{json, Value};
use uuid::Uuid;

use super::*;

/// What the fake API server answers, and what it was asked.
#[derive(Default)]
struct FakeApi {
    /// `(namespace, selector)` -> pod list JSON, or an HTTP error status.
    answers: HashMap<(String, String), Result<Value, u16>>,
    /// Every request: namespace, selector, bearer token.
    seen: Vec<(String, String, Option<String>)>,
}

type Shared = Arc<Mutex<FakeApi>>;

async fn list_pods(
    State(api): State<Shared>,
    Path(ns): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> axum::response::Response {
    let selector = q.get("labelSelector").cloned().unwrap_or_default();
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let mut api = api.lock().unwrap();
    api.seen.push((ns.clone(), selector.clone(), token));
    match api.answers.get(&(ns, selector)) {
        Some(Ok(list)) => Json(list.clone()).into_response(),
        Some(Err(code)) => (
            StatusCode::from_u16(*code).unwrap(),
            Json(json!({"kind": "Status", "message": "pods is forbidden: nope"})),
        )
            .into_response(),
        None => Json(json!({"kind": "PodList", "items": []})).into_response(),
    }
}

async fn fake_api() -> (KubeClient, Shared) {
    let api: Shared = Arc::default();
    let app = Router::new()
        .route("/api/v1/namespaces/:ns/pods", get(list_pods))
        .with_state(api.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        KubeClient::with_base(format!("http://{addr}"), Some("sa-token".into())),
        api,
    )
}

fn answer(api: &Shared, ns: &str, selector: &str, pods: Vec<Value>) {
    api.lock().unwrap().answers.insert(
        (ns.into(), selector.into()),
        Ok(json!({"kind": "PodList", "metadata": {}, "items": pods})),
    );
}

fn fail(api: &Shared, ns: &str, selector: &str, code: u16) {
    api.lock()
        .unwrap()
        .answers
        .insert((ns.into(), selector.into()), Err(code));
}

/// A pod whose serving container runs with `args`.
fn pod(name: &str, ready: bool, args: &[&str]) -> Value {
    json!({
        "metadata": {"name": name, "namespace": "inference"},
        "spec": {"containers": [
            {"name": "proxy", "image": "sidecar", "args": ["--port", "9000"]},
            {"name": "server", "command": ["/bin/sh", "-c"],
             "args": [format!("exec serve-model {}", args.join(" "))],
             "env": [{"name": "SECRET", "valueFrom": {"secretKeyRef": {"name": "s", "key": "k"}}}]}
        ]},
        "status": {
            "phase": "Running",
            "conditions": [{"type": "Ready", "status": if ready { "True" } else { "False" }}]
        }
    })
}

fn terminating(mut p: Value) -> Value {
    p["metadata"]["deletionTimestamp"] = json!("2026-09-24T00:00:00Z");
    p
}

fn pending(mut p: Value) -> Value {
    p["status"]["phase"] = json!("Pending");
    p
}

fn settings(namespaces: &[&str], default_selector: &str) -> CapacityDiscovery {
    CapacityDiscovery::new(CapacityDiscoveryConfig {
        enabled: true,
        interval: Duration::from_secs(15),
        namespaces: namespaces.iter().map(|s| s.to_string()).collect(),
        default_selector: default_selector.into(),
    })
}

fn kube_target(name: &str) -> DiscoveryTarget {
    DiscoveryTarget {
        model_name: name.into(),
        upstream_model: format!("{name}-served"),
        static_max_in_flight: Some(6),
        source: "kubernetes".into(),
        namespace: Some("inference".into()),
        selector: None,
        per_replica_max_in_flight: None,
        headroom: 1.0,
        healthy: true,
        api_base: "http://backend/v1".into(),
        endpoint_selection_mode: "failover".into(),
        endpoints: Vec::new(),
    }
}

fn fairshare() -> FairShare {
    FairShare::start(
        Arc::new(StaticCapacity::new(4096)),
        FairshareAlgorithm::Hierarchical,
        32,
    )
}

fn status(handle: &CapacityDiscovery, model: &str) -> ModelCapacityStatus {
    handle
        .statuses()
        .into_iter()
        .find(|s| s.model_name == model)
        .unwrap_or_else(|| panic!("no status for {model}"))
}

/// The configured pool size fairshare holds for `model`, read by admitting
/// one request with a route cap the override must beat.
async fn pool_size(fs: &FairShare, model: &str) -> usize {
    let admitted = fs
        .admit(AdmitRequest::new(Uuid::new_v4(), model, 1).model_cap(1))
        .await
        .expect("admitted");
    let snap = fs.snapshot().await.expect("snapshot");
    drop(admitted);
    snap.pools
        .iter()
        .find(|p| p.model == model)
        .expect("pool")
        .configured_cap
}

const TEMPLATE: &str = "app.example.com/model={upstream_model}";

#[tokio::test]
async fn ready_pods_times_the_container_flag_sizes_the_pool() {
    let (client, api) = fake_api().await;
    let selector = "app.example.com/model=m-served";
    answer(
        &api,
        "inference",
        selector,
        vec![
            pod("a", true, &["--max-num-seqs", "8"]),
            pod("b", true, &["--max-num-seqs=8"]),
            // Not Ready, terminating, or not Running: none of them serve.
            pod("c", false, &["--max-num-seqs", "8"]),
            terminating(pod("d", true, &["--max-num-seqs", "8"])),
            pending(pod("e", true, &["--max-num-seqs", "8"])),
        ],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let fs = fairshare();
    let mut d = Discoverer::new(Some(Ok(client)), fs.clone(), 32);

    d.pass(&handle, vec![kube_target("m")]).await;

    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_DISCOVERED, "{s:?}");
    assert_eq!(s.ready_replicas, Some(2));
    assert_eq!(s.per_replica_max_in_flight, Some(8));
    assert_eq!(s.per_replica_source.as_deref(), Some("vLLM --max-num-seqs"));
    assert_eq!(s.derived_max_in_flight, Some(16));
    assert_eq!(s.effective_max_in_flight, 16);
    assert_eq!(s.selector.as_deref(), Some(selector));
    assert_eq!(s.namespaces, vec!["inference"]);
    assert!(s.reason.is_none());
    assert_eq!(pool_size(&fs, "m").await, 16);
    assert_eq!(handle.effective_caps().get("m"), Some(&16));

    let seen = api.lock().unwrap().seen.clone();
    assert_eq!(
        seen,
        vec![(
            "inference".to_string(),
            selector.to_string(),
            Some("sa-token".to_string())
        )]
    );
}

#[tokio::test]
async fn set_based_and_inequality_selectors_reach_the_api_server_intact() {
    let (client, api) = fake_api().await;
    // Heads only, the way a multi-node deployment excludes worker pods.
    let selector = "app=m,ray.io/node-type!=worker,tier in (gpu, accel)";
    answer(
        &api,
        "inference",
        selector,
        vec![pod("head", true, &["--max-num-seqs", "4"])],
    );
    let handle = settings(&["inference"], "");
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    let mut t = kube_target("m");
    t.selector = Some(selector.into());

    d.pass(&handle, vec![t]).await;

    assert_eq!(status(&handle, "m").effective_max_in_flight, 4);
    assert_eq!(api.lock().unwrap().seen[0].1, selector);
}

#[tokio::test]
async fn models_sharing_a_selector_share_one_list_call() {
    let (client, api) = fake_api().await;
    answer(
        &api,
        "inference",
        "app=shared",
        vec![pod("a", true, &["--max-num-seqs", "2"])],
    );
    let handle = settings(&["inference"], "");
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    let targets: Vec<DiscoveryTarget> = ["one", "two", "three"]
        .into_iter()
        .map(|n| {
            let mut t = kube_target(n);
            t.selector = Some("app=shared".into());
            t
        })
        .collect();

    d.pass(&handle, targets).await;

    assert_eq!(
        api.lock().unwrap().seen.len(),
        1,
        "one list for three models"
    );
    for n in ["one", "two", "three"] {
        assert_eq!(status(&handle, n).effective_max_in_flight, 2);
    }
}

#[tokio::test]
async fn no_namespace_searches_every_allowed_one() {
    let (client, api) = fake_api().await;
    answer(
        &api,
        "llm",
        "app.example.com/model=m-served",
        vec![pod("a", true, &["--max-num-seqs", "3"])],
    );
    answer(
        &api,
        "batch",
        "app.example.com/model=m-served",
        vec![pod("b", true, &["--max-num-seqs", "3"])],
    );
    let handle = settings(&["llm", "batch"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    let mut t = kube_target("m");
    t.namespace = None;

    d.pass(&handle, vec![t]).await;

    let s = status(&handle, "m");
    assert_eq!(s.ready_replicas, Some(2));
    assert_eq!(s.effective_max_in_flight, 6);
    assert_eq!(s.namespaces, vec!["llm", "batch"]);
}

#[tokio::test]
async fn the_models_own_value_wins_and_headroom_scales_it() {
    let (client, api) = fake_api().await;
    answer(
        &api,
        "inference",
        "app.example.com/model=m-served",
        vec![
            pod("a", true, &["--max-num-seqs", "64"]),
            pod("b", true, &["--max-num-seqs", "64"]),
        ],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    let mut t = kube_target("m");
    t.per_replica_max_in_flight = Some(8);
    t.headroom = 1.25;

    d.pass(&handle, vec![t]).await;

    let s = status(&handle, "m");
    assert_eq!(s.per_replica_max_in_flight, Some(8));
    assert_eq!(
        s.per_replica_source.as_deref(),
        Some("per_replica_max_in_flight")
    );
    assert_eq!(s.effective_max_in_flight, 20, "ceil(2 x 8 x 1.25)");
}

#[tokio::test]
async fn pods_that_disagree_use_the_lowest_value() {
    let (client, api) = fake_api().await;
    answer(
        &api,
        "inference",
        "app.example.com/model=m-served",
        vec![
            pod("old", true, &["--max-num-seqs", "4"]),
            pod("new", true, &["--max-running-requests", "16"]),
            pod("plain", true, &["--port", "8000"]),
        ],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);

    d.pass(&handle, vec![kube_target("m")]).await;

    let s = status(&handle, "m");
    assert_eq!(s.ready_replicas, Some(3));
    assert_eq!(s.per_replica_max_in_flight, Some(4));
    assert_eq!(s.effective_max_in_flight, 12);
}

#[tokio::test]
async fn no_ready_pod_keeps_the_last_value_then_recovers() {
    let (client, api) = fake_api().await;
    let sel = "app.example.com/model=m-served";
    answer(
        &api,
        "inference",
        sel,
        vec![pod("a", true, &["--max-num-seqs", "8"])],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let fs = fairshare();
    let mut d = Discoverer::new(Some(Ok(client)), fs.clone(), 32);
    d.pass(&handle, vec![kube_target("m")]).await;
    assert_eq!(status(&handle, "m").effective_max_in_flight, 8);

    // Scaled to zero, or every pod restarting: the last value holds.
    answer(
        &api,
        "inference",
        sel,
        vec![pod("a", false, &["--max-num-seqs", "8"])],
    );
    d.pass(&handle, vec![kube_target("m")]).await;
    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_STALE);
    assert_eq!(s.ready_replicas, Some(0));
    assert_eq!(s.effective_max_in_flight, 8);
    assert_eq!(s.derived_max_in_flight, Some(8));
    assert!(s.reason.unwrap().contains("no Ready pod"));
    assert_eq!(pool_size(&fs, "m").await, 8);

    // Back with three replicas: the pool grows.
    answer(
        &api,
        "inference",
        sel,
        (0..3)
            .map(|i| pod(&format!("p{i}"), true, &["--max-num-seqs", "8"]))
            .collect(),
    );
    d.pass(&handle, vec![kube_target("m")]).await;
    assert_eq!(status(&handle, "m").state, STATE_DISCOVERED);
    assert_eq!(pool_size(&fs, "m").await, 24);
}

#[tokio::test]
async fn with_nothing_discovered_yet_the_static_value_applies() {
    let (client, api) = fake_api().await;
    answer(&api, "inference", "app.example.com/model=m-served", vec![]);
    let handle = settings(&["inference"], TEMPLATE);
    let fs = fairshare();
    let mut d = Discoverer::new(Some(Ok(client)), fs.clone(), 32);
    let mut unset = kube_target("u");
    unset.upstream_model = "m-served".into();
    unset.static_max_in_flight = None;

    d.pass(&handle, vec![kube_target("m"), unset]).await;

    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_FALLBACK);
    assert_eq!(s.effective_max_in_flight, 6, "the model's max_in_flight");
    assert_eq!(s.derived_max_in_flight, None);
    assert_eq!(
        status(&handle, "u").effective_max_in_flight,
        32,
        "the gateway default"
    );
    assert_eq!(pool_size(&fs, "m").await, 6);
}

#[tokio::test]
async fn an_api_failure_keeps_the_last_value_and_says_why() {
    let (client, api) = fake_api().await;
    let sel = "app.example.com/model=m-served";
    answer(
        &api,
        "inference",
        sel,
        vec![pod("a", true, &["--max-num-seqs", "5"])],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    d.pass(&handle, vec![kube_target("m")]).await;

    fail(&api, "inference", sel, 403);
    d.pass(&handle, vec![kube_target("m")]).await;
    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_STALE);
    assert_eq!(s.effective_max_in_flight, 5);
    assert_eq!(s.ready_replicas, None);
    let reason = s.reason.unwrap();
    assert!(reason.contains("HTTP 403"), "{reason}");
    assert!(reason.contains("get/list on pods"), "{reason}");

    // A model that never had a value falls back to its static one.
    let mut other = kube_target("o");
    other.selector = Some("app=o".into());
    fail(&api, "inference", "app=o", 500);
    d.pass(&handle, vec![other]).await;
    assert_eq!(status(&handle, "o").state, STATE_FALLBACK);
    assert_eq!(status(&handle, "o").effective_max_in_flight, 6);
}

#[tokio::test]
async fn an_unreachable_api_server_is_a_failure_not_a_crash() {
    // Nothing listens here.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let client = KubeClient::with_base(format!("http://{addr}"), None);
    let handle = settings(&["inference"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    d.pass(&handle, vec![kube_target("m")]).await;
    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_FALLBACK);
    assert!(s.reason.unwrap().contains("unreachable"));
}

#[tokio::test]
async fn a_server_with_no_known_flag_cannot_be_discovered() {
    let (client, api) = fake_api().await;
    answer(
        &api,
        "inference",
        "app.example.com/model=m-served",
        vec![pod("a", true, &["--port", "8000"])],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);

    d.pass(&handle, vec![kube_target("m")]).await;

    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_FALLBACK);
    assert_eq!(s.ready_replicas, Some(1));
    assert_eq!(s.effective_max_in_flight, 6);
    assert!(s.reason.unwrap().contains("per_replica_max_in_flight"));
}

#[tokio::test]
async fn gateway_policy_problems_are_reported_without_calling_the_api() {
    let (client, api) = fake_api().await;
    let handle = settings(&["inference"], "");
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    let mut outside = kube_target("outside");
    outside.namespace = Some("kube-system".into());
    outside.selector = Some("app=x".into());
    let no_selector = kube_target("no-selector");

    d.pass(&handle, vec![outside, no_selector]).await;

    let r = status(&handle, "outside").reason.unwrap();
    assert!(r.contains("OBLETH_CAPACITY_DISCOVERY_NAMESPACES"), "{r}");
    let r = status(&handle, "no-selector").reason.unwrap();
    assert!(r.contains("OBLETH_CAPACITY_DEFAULT_SELECTOR"), "{r}");
    assert!(api.lock().unwrap().seen.is_empty());

    // No namespaces at all: the kubernetes source is unavailable.
    let handle = settings(&[], "");
    let mut d = Discoverer::new(None, fairshare(), 32);
    d.pass(&handle, vec![kube_target("m")]).await;
    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_FALLBACK);
    assert!(s.reason.unwrap().contains("not available"));
}

fn endpoint(priority: i64, enabled: bool, healthy: bool, max: Option<usize>) -> ResolvedEndpoint {
    ResolvedEndpoint {
        id: Uuid::new_v4().to_string(),
        api_base: format!("http://backend-{priority}/v1"),
        api_key: None,
        priority,
        weight: 100,
        enabled,
        healthy,
        max_in_flight: max,
    }
}

fn endpoints_target(mode: &str, endpoints: Vec<ResolvedEndpoint>) -> DiscoveryTarget {
    DiscoveryTarget {
        source: "endpoints".into(),
        namespace: None,
        endpoint_selection_mode: mode.into(),
        endpoints,
        ..kube_target("e")
    }
}

#[tokio::test]
async fn the_endpoints_source_counts_enabled_healthy_endpoints() {
    let handle = settings(&[], "");
    let fs = fairshare();
    let mut d = Discoverer::new(None, fs.clone(), 32);
    let mut t = endpoints_target(
        "load_balance",
        vec![
            endpoint(0, true, true, Some(16)),
            endpoint(1, true, true, None),
            endpoint(2, false, true, Some(99)),
            endpoint(3, true, false, Some(99)),
        ],
    );
    t.per_replica_max_in_flight = Some(8);

    d.pass(&handle, vec![t.clone()]).await;

    let s = status(&handle, "e");
    assert_eq!(s.state, STATE_DISCOVERED);
    assert_eq!(s.ready_replicas, Some(2));
    assert_eq!(s.effective_max_in_flight, 24, "16 + 8");
    assert_eq!(s.per_replica_max_in_flight, Some(12));
    assert_eq!(pool_size(&fs, "e").await, 24);

    // Under failover only the endpoint in use takes traffic.
    t.endpoint_selection_mode = "failover".into();
    d.pass(&handle, vec![t.clone()]).await;
    let s = status(&handle, "e");
    assert_eq!(s.ready_replicas, Some(1));
    assert_eq!(s.effective_max_in_flight, 16);
    assert_eq!(
        s.per_replica_source.as_deref(),
        Some("endpoint max_in_flight")
    );

    // Every endpoint down: the last value holds.
    t.endpoints.iter_mut().for_each(|e| e.healthy = false);
    d.pass(&handle, vec![t.clone()]).await;
    let s = status(&handle, "e");
    assert_eq!(s.state, STATE_STALE);
    assert_eq!(s.effective_max_in_flight, 16);
}

#[tokio::test]
async fn an_endpoint_with_no_value_anywhere_cannot_be_counted() {
    let handle = settings(&[], "");
    let mut d = Discoverer::new(None, fairshare(), 32);
    let t = endpoints_target(
        "load_balance",
        vec![
            endpoint(0, true, true, Some(4)),
            endpoint(1, true, true, None),
        ],
    );
    d.pass(&handle, vec![t]).await;
    let s = status(&handle, "e");
    assert_eq!(s.state, STATE_FALLBACK);
    assert_eq!(s.effective_max_in_flight, 6);
    assert!(s.reason.unwrap().contains("per_replica_max_in_flight"));
}

#[tokio::test]
async fn a_model_with_no_endpoint_rows_counts_its_api_base_while_healthy() {
    let handle = settings(&[], "");
    let mut d = Discoverer::new(None, fairshare(), 32);
    let mut t = endpoints_target("failover", Vec::new());
    t.per_replica_max_in_flight = Some(10);
    d.pass(&handle, vec![t.clone()]).await;
    assert_eq!(status(&handle, "e").effective_max_in_flight, 10);

    t.healthy = false;
    d.pass(&handle, vec![t]).await;
    assert_eq!(status(&handle, "e").state, STATE_STALE);
}

#[tokio::test]
async fn a_model_leaving_the_mode_leaves_the_view_and_the_overrides() {
    let handle = settings(&[], "");
    let fs = fairshare();
    let mut d = Discoverer::new(None, fs.clone(), 32);
    let mut t = endpoints_target("failover", Vec::new());
    t.per_replica_max_in_flight = Some(10);
    d.pass(&handle, vec![t]).await;
    assert_eq!(pool_size(&fs, "e").await, 10);

    d.pass(&handle, Vec::new()).await;
    assert!(handle.statuses().is_empty());
    // Back to the cap its admissions carry (1 in `pool_size`).
    assert_eq!(pool_size(&fs, "e").await, 1);
}

#[test]
fn the_derivation_rounds_up_and_never_reaches_zero() {
    assert_eq!(derive(2, 8, 1.0), 16);
    assert_eq!(derive(3, 5, 1.1), 17, "16.5 rounds up");
    assert_eq!(derive(1, 1, 0.1), 1, "never below 1");
    assert_eq!(derive(usize::MAX, 2, 10.0), MAX_DERIVED);
}

#[test]
fn only_discovered_models_become_targets() {
    let mut m = resolved("m");
    assert!(DiscoveryTarget::from_resolved(&m, true).is_none());
    m.capacity_mode = "discovered".into();
    m.max_in_flight = Some(0);
    m.per_replica_max_in_flight = Some(4);
    let t = DiscoveryTarget::from_resolved(&m, false).expect("target");
    assert_eq!(t.static_max_in_flight, None, "0 is no cap");
    assert_eq!(t.per_replica_max_in_flight, Some(4));
    assert!(!t.healthy);
}

fn resolved(name: &str) -> ResolvedModel {
    ResolvedModel {
        model_name: name.to_string(),
        aliases: Vec::new(),
        quantization: "unknown".into(),
        upstream_model: name.to_string(),
        api_base: "http://upstream".to_string(),
        api_key: None,
        upstream_headers: Default::default(),
        model_type: obleth_config::DEFAULT_MODEL_TYPE.to_string(),
        admission_weight: 100,
        max_in_flight: None,
        capacity_mode: "static".into(),
        capacity_source: "endpoints".into(),
        capacity_namespace: None,
        capacity_selector: None,
        per_replica_max_in_flight: None,
        capacity_headroom: 1.0,
        enabled: true,
        cache_enabled: false,
        cache_ttl_secs: 0,
        input_cost_per_token: 0.0,
        output_cost_per_token: 0.0,
        cost_per_image: 0.0,
        cost_per_audio_second: 0.0,
        cost_per_character: 0.0,
        cost_per_video: 0.0,
        context_window: 8192,
        supports_function_calling: false,
        supports_system_messages: true,
        supports_response_schema: false,
        supports_tool_choice: false,
        supports_vision: false,
        tags: Vec::new(),
        declared_levels: Vec::new(),
        boons: Vec::new(),
        tool_servers: Vec::new(),
        knowledge_collections: Vec::new(),
        request_timeout_secs: None,
        max_retries: 0,
        retry_backoff_ms: obleth_config::DEFAULT_RETRY_BACKOFF_MS,
        endpoint_selection_mode: obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string(),
        debug_diagnostics: false,
        energy_slots_per_node: 0,
        route_bias: 1.0,
        auto_eligible: true,
        draft_model: String::new(),
        verify_api_base: String::new(),
        verify_upstream_model: String::new(),
        endpoints: Vec::new(),
    }
}
