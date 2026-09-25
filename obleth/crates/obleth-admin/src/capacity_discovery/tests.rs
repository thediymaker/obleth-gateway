//! Discovery passes against a fake Kubernetes API (a local HTTP server that
//! answers EndpointSlice lists, and records every path it is asked for) and
//! against a model's own endpoints, feeding a real fairshare scheduler.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::{Json, Router};
use obleth_config::{CapacityDiscoveryConfig, FairshareAlgorithm, ResolvedEndpoint};
use obleth_fairshare::{AdmitRequest, FairShare, StaticCapacity};
use serde_json::{json, Value};
use uuid::Uuid;

use super::kube::{count_endpoints, EndpointCount, EndpointSlice};
use super::*;

/// The only path prefix and suffix the kubernetes source may ask for.
const SLICES_PREFIX: &str = "/apis/discovery.k8s.io/v1/namespaces/";
const SLICES_SUFFIX: &str = "/endpointslices";

/// Pages of EndpointSlices, or an HTTP error status.
type Answer = Result<Vec<Vec<Value>>, u16>;

/// What the fake API server answers, and what it was asked.
#[derive(Default)]
struct FakeApi {
    /// `(namespace, service)` -> what to answer.
    answers: HashMap<(String, String), Answer>,
    /// Every EndpointSlice request: namespace, service, bearer token, query.
    seen: Vec<Seen>,
    /// Every path asked for, whatever it was.
    paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq)]
struct Seen {
    namespace: String,
    service: String,
    token: Option<String>,
    query: HashMap<String, String>,
}

type Shared = Arc<Mutex<FakeApi>>;

async fn list_slices(
    State(api): State<Shared>,
    Path(ns): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    headers: HeaderMap,
    uri: Uri,
) -> axum::response::Response {
    let selector = q.get("labelSelector").cloned().unwrap_or_default();
    let service = selector
        .strip_prefix("kubernetes.io/service-name=")
        .unwrap_or("")
        .to_string();
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    let mut api = api.lock().unwrap();
    api.paths.push(uri.path().to_string());
    api.seen.push(Seen {
        namespace: ns.clone(),
        service: service.clone(),
        token,
        query: q.clone(),
    });
    let page: usize = q
        .get("continue")
        .and_then(|c| c.strip_prefix('p'))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0);
    match api.answers.get(&(ns, service)) {
        Some(Ok(pages)) => {
            let items = pages.get(page).cloned().unwrap_or_default();
            let meta = if page + 1 < pages.len() {
                json!({"continue": format!("p{}", page + 1)})
            } else {
                json!({})
            };
            Json(json!({"kind": "EndpointSliceList", "metadata": meta, "items": items}))
                .into_response()
        }
        Some(Err(code)) => (
            StatusCode::from_u16(*code).unwrap(),
            Json(json!({"kind": "Status", "message": "endpointslices is forbidden: nope"})),
        )
            .into_response(),
        None => {
            Json(json!({"kind": "EndpointSliceList", "metadata": {}, "items": []})).into_response()
        }
    }
}

/// Anything else the client asks for is recorded and refused.
async fn anything_else(State(api): State<Shared>, uri: Uri) -> StatusCode {
    api.lock().unwrap().paths.push(uri.path().to_string());
    StatusCode::NOT_FOUND
}

async fn fake_api() -> (KubeClient, Shared) {
    let api: Shared = Arc::default();
    let app = Router::new()
        .route(
            "/apis/discovery.k8s.io/v1/namespaces/:ns/endpointslices",
            get(list_slices),
        )
        .fallback(anything_else)
        .with_state(api.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (
        KubeClient::with_base(format!("http://{addr}"), Some("sa-token".into())),
        api,
    )
}

/// Every path the fake API server saw is an EndpointSlice list: no pod, no
/// Service object, no Secret, nothing else.
fn assert_only_endpoint_slices(api: &Shared) {
    for path in &api.lock().unwrap().paths {
        assert!(
            path.starts_with(SLICES_PREFIX) && path.ends_with(SLICES_SUFFIX),
            "the kubernetes source asked for {path}"
        );
        assert!(!path.contains("/pods"), "{path}");
    }
}

fn answer(api: &Shared, ns: &str, service: &str, slices: Vec<Value>) {
    answer_pages(api, ns, service, vec![slices]);
}

fn answer_pages(api: &Shared, ns: &str, service: &str, pages: Vec<Vec<Value>>) {
    api.lock()
        .unwrap()
        .answers
        .insert((ns.into(), service.into()), Ok(pages));
}

fn fail(api: &Shared, ns: &str, service: &str, code: u16) {
    api.lock()
        .unwrap()
        .answers
        .insert((ns.into(), service.into()), Err(code));
}

fn asked(api: &Shared) -> Vec<(String, String)> {
    api.lock()
        .unwrap()
        .seen
        .iter()
        .map(|s| (s.namespace.clone(), s.service.clone()))
        .collect()
}

/// One endpoint: a pod `uid` at `ip`, with explicit conditions.
fn ep(uid: &str, ip: &str, ready: bool, serving: bool, terminating: bool) -> Value {
    json!({
        "addresses": [ip],
        "conditions": {"ready": ready, "serving": serving, "terminating": terminating},
        "targetRef": {"kind": "Pod", "name": format!("pod-{uid}"), "namespace": "inference", "uid": uid},
        "nodeName": "node-a"
    })
}

fn ready(uid: &str, ip: &str) -> Value {
    ep(uid, ip, true, true, false)
}

fn slice(name: &str, endpoints: Vec<Value>) -> Value {
    json!({
        "metadata": {"name": name, "labels": {"kubernetes.io/service-name": "svc"}},
        "addressType": "IPv4",
        "endpoints": endpoints,
        "ports": [{"name": "http", "port": 8000, "protocol": "TCP"}]
    })
}

fn settings(namespaces: &[&str], default_service: &str) -> CapacityDiscovery {
    CapacityDiscovery::new(CapacityDiscoveryConfig {
        enabled: true,
        interval: Duration::from_secs(15),
        namespaces: namespaces.iter().map(|s| s.to_string()).collect(),
        default_service: default_service.into(),
    })
}

fn kube_target(name: &str) -> DiscoveryTarget {
    DiscoveryTarget {
        model_name: name.into(),
        upstream_model: format!("{name}-served"),
        static_max_in_flight: Some(6),
        source: "kubernetes".into(),
        namespace: Some("inference".into()),
        service: None,
        per_replica_max_in_flight: Some(8),
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

/// Services named after the served model.
const TEMPLATE: &str = "{upstream_model}";

#[tokio::test]
async fn ready_endpoints_times_the_per_replica_value_size_the_pool() {
    let (client, api) = fake_api().await;
    answer(
        &api,
        "inference",
        "m-served",
        vec![
            slice(
                "m-served-abc",
                vec![
                    ready("a", "10.0.0.1"),
                    ready("b", "10.0.0.2"),
                    // Not ready, or draining: neither counts.
                    ep("c", "10.0.0.3", false, false, false),
                    ep("d", "10.0.0.4", false, true, true),
                    ep("e", "10.0.0.5", true, true, true),
                ],
            ),
            // A second slice lists `a` again (an IPv6 slice, or the
            // controller moving it) and one more ready pod.
            slice(
                "m-served-def",
                vec![
                    ep("a", "fd00::1", true, true, false),
                    ready("f", "10.0.0.6"),
                ],
            ),
        ],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let fs = fairshare();
    let mut d = Discoverer::new(Some(Ok(client)), fs.clone(), 32);

    d.pass(&handle, vec![kube_target("m")]).await;

    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_DISCOVERED, "{s:?}");
    assert_eq!(s.ready_replicas, Some(3), "a, b and f");
    assert_eq!(s.per_replica_max_in_flight, Some(8));
    assert_eq!(
        s.per_replica_source.as_deref(),
        Some(PER_REPLICA_CONFIGURED)
    );
    assert_eq!(s.derived_max_in_flight, Some(24));
    assert_eq!(s.effective_max_in_flight, 24);
    assert_eq!(s.service.as_deref(), Some("m-served"));
    assert_eq!(s.namespaces, vec!["inference"]);
    assert_eq!(s.namespace.as_deref(), Some("inference"));
    assert!(s.reason.is_none());
    assert_eq!(pool_size(&fs, "m").await, 24);
    assert_eq!(handle.effective_caps().get("m"), Some(&24));

    let seen = api.lock().unwrap().seen.clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].namespace, "inference");
    assert_eq!(seen[0].token.as_deref(), Some("sa-token"));
    assert_eq!(
        seen[0].query.get("labelSelector").map(String::as_str),
        Some("kubernetes.io/service-name=m-served")
    );
    assert_eq!(
        seen[0].query.get("resourceVersion").map(String::as_str),
        Some("0")
    );
    assert_only_endpoint_slices(&api);
}

#[test]
fn endpoints_are_deduplicated_and_judged_by_their_conditions() {
    let slices: Vec<EndpointSlice> = serde_json::from_value(json!([
        {"endpoints": [
            // Missing `serving` defers to `ready`.
            {"addresses": ["10.0.0.1"], "conditions": {"ready": true},
             "targetRef": {"uid": "a"}},
            // Serving false: not counted even if marked ready.
            {"addresses": ["10.0.0.2"], "conditions": {"ready": true, "serving": false},
             "targetRef": {"uid": "b"}},
            // No conditions at all: an unknown state, which the API says to
            // read as ready.
            {"addresses": ["10.0.0.3"], "targetRef": {"uid": "c"}},
            // Missing `ready`: read as ready.
            {"addresses": ["10.0.0.4"], "conditions": {"serving": true},
             "targetRef": {"uid": "d"}},
            // No targetRef: keyed by address.
            {"addresses": ["10.0.0.5"], "conditions": {"ready": true}},
            // Neither uid nor address: skipped.
            {"addresses": [], "conditions": {"ready": true}}
        ]},
        {"endpoints": [
            // `a` again, not ready in this copy: still counted once.
            {"addresses": ["10.0.0.1"], "conditions": {"ready": false},
             "targetRef": {"uid": "a"}},
            // The address-keyed endpoint again.
            {"addresses": ["10.0.0.5"], "conditions": {"ready": true, "terminating": false}}
        ]},
        // A slice of a Service with no endpoints.
        {"endpoints": null},
        {}
    ]))
    .expect("slices");
    assert_eq!(
        count_endpoints(&slices),
        EndpointCount { total: 5, ready: 4 }
    );
    assert_eq!(count_endpoints(&[]), EndpointCount::default());
}

#[tokio::test]
async fn a_paged_list_is_read_to_the_end() {
    let (client, api) = fake_api().await;
    answer_pages(
        &api,
        "inference",
        "m-served",
        vec![
            vec![slice("s1", vec![ready("a", "10.0.0.1")])],
            vec![slice("s2", vec![ready("b", "10.0.0.2")])],
        ],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);

    d.pass(&handle, vec![kube_target("m")]).await;

    assert_eq!(status(&handle, "m").ready_replicas, Some(2));
    let seen = api.lock().unwrap().seen.clone();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        seen[1].query.get("continue").map(String::as_str),
        Some("p1")
    );
    assert!(!seen[1].query.contains_key("resourceVersion"));
}

#[tokio::test]
async fn models_sharing_a_service_share_one_list_call() {
    let (client, api) = fake_api().await;
    answer(
        &api,
        "inference",
        "shared",
        vec![slice("s", vec![ready("a", "10.0.0.1")])],
    );
    let handle = settings(&["inference"], "");
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    let targets: Vec<DiscoveryTarget> = ["one", "two", "three"]
        .into_iter()
        .map(|n| {
            let mut t = kube_target(n);
            t.service = Some("shared".into());
            t.per_replica_max_in_flight = Some(2);
            t
        })
        .collect();

    d.pass(&handle, targets).await;

    assert_eq!(asked(&api).len(), 1, "one list for three models");
    for n in ["one", "two", "three"] {
        assert_eq!(status(&handle, n).effective_max_in_flight, 2);
    }
}

#[tokio::test]
async fn no_namespace_takes_the_first_allowed_one_that_has_the_service() {
    let (client, api) = fake_api().await;
    // Not in `llm`; in both `batch` and `extra`: `batch` comes first.
    answer(
        &api,
        "batch",
        "m-served",
        vec![slice("s", vec![ready("a", "10.0.0.1")])],
    );
    answer(
        &api,
        "extra",
        "m-served",
        vec![slice(
            "s",
            vec![ready("b", "10.0.1.1"), ready("c", "10.0.1.2")],
        )],
    );
    let handle = settings(&["llm", "batch", "extra"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    let mut t = kube_target("m");
    t.namespace = None;
    t.per_replica_max_in_flight = Some(3);

    d.pass(&handle, vec![t.clone()]).await;

    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_DISCOVERED, "{s:?}");
    assert_eq!(s.ready_replicas, Some(1));
    assert_eq!(s.effective_max_in_flight, 3);
    assert_eq!(s.namespaces, vec!["llm", "batch", "extra"]);
    assert_eq!(s.namespace.as_deref(), Some("batch"));
    // `extra` is never read once `batch` has the Service.
    assert_eq!(
        asked(&api),
        vec![
            ("llm".to_string(), "m-served".to_string()),
            ("batch".to_string(), "m-served".to_string()),
        ]
    );

    // A Service with no endpoints still exists: its namespace wins, and the
    // model has no ready replica rather than moving on to `extra`.
    answer(&api, "batch", "m-served", vec![slice("s", vec![])]);
    d.pass(&handle, vec![t]).await;
    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_STALE);
    assert_eq!(s.namespace.as_deref(), Some("batch"));
    assert!(s.reason.unwrap().contains("no ready endpoint"));
    assert!(!asked(&api).iter().any(|(ns, _)| ns == "extra"));
    assert_only_endpoint_slices(&api);
}

#[tokio::test]
async fn a_namespace_that_does_not_answer_is_not_skipped() {
    let (client, api) = fake_api().await;
    fail(&api, "llm", "m-served", 403);
    answer(
        &api,
        "batch",
        "m-served",
        vec![slice("s", vec![ready("a", "10.0.0.1")])],
    );
    let handle = settings(&["llm", "batch"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    let mut t = kube_target("m");
    t.namespace = None;

    d.pass(&handle, vec![t]).await;

    // Whether `llm` has the Service is unknown, so `batch` cannot be trusted
    // to be the right one.
    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_FALLBACK);
    let reason = s.reason.unwrap();
    assert!(reason.contains("HTTP 403"), "{reason}");
    assert_eq!(asked(&api).len(), 1);
}

#[tokio::test]
async fn a_missing_service_falls_back_then_keeps_the_last_value() {
    let (client, api) = fake_api().await;
    let handle = settings(&["llm", "batch"], TEMPLATE);
    let fs = fairshare();
    let mut d = Discoverer::new(Some(Ok(client)), fs.clone(), 32);
    let mut t = kube_target("m");
    t.namespace = None;

    // Nowhere yet: the static value, and why.
    d.pass(&handle, vec![t.clone()]).await;
    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_FALLBACK);
    assert_eq!(s.effective_max_in_flight, 6);
    assert_eq!(s.namespace, None);
    let reason = s.reason.unwrap();
    assert!(
        reason.contains("no EndpointSlice for Service m-served"),
        "{reason}"
    );
    assert!(reason.contains("llm, batch"), "{reason}");

    // It appears with two ready replicas.
    answer(
        &api,
        "batch",
        "m-served",
        vec![slice(
            "s",
            vec![ready("a", "10.0.0.1"), ready("b", "10.0.0.2")],
        )],
    );
    d.pass(&handle, vec![t.clone()]).await;
    assert_eq!(status(&handle, "m").effective_max_in_flight, 16);
    assert_eq!(pool_size(&fs, "m").await, 16);

    // Deleted, or the API briefly answers nothing: the last value holds.
    api.lock().unwrap().answers.clear();
    d.pass(&handle, vec![t]).await;
    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_STALE);
    assert_eq!(s.ready_replicas, Some(0));
    assert_eq!(s.effective_max_in_flight, 16);
    assert_eq!(pool_size(&fs, "m").await, 16);
}

#[tokio::test]
async fn headroom_scales_the_derived_value() {
    let (client, api) = fake_api().await;
    answer(
        &api,
        "inference",
        "m-served",
        vec![slice(
            "s",
            vec![ready("a", "10.0.0.1"), ready("b", "10.0.0.2")],
        )],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    let mut t = kube_target("m");
    t.headroom = 1.25;

    d.pass(&handle, vec![t]).await;

    assert_eq!(
        status(&handle, "m").effective_max_in_flight,
        20,
        "ceil(2 x 8 x 1.25)"
    );
}

#[tokio::test]
async fn scaled_to_zero_keeps_the_last_value_then_recovers() {
    let (client, api) = fake_api().await;
    answer(
        &api,
        "inference",
        "m-served",
        vec![slice("s", vec![ready("a", "10.0.0.1")])],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let fs = fairshare();
    let mut d = Discoverer::new(Some(Ok(client)), fs.clone(), 32);
    d.pass(&handle, vec![kube_target("m")]).await;
    assert_eq!(status(&handle, "m").effective_max_in_flight, 8);

    // Scaled to zero: the controller keeps one slice with no endpoints.
    answer(
        &api,
        "inference",
        "m-served",
        vec![json!({"metadata": {"name": "s"}, "addressType": "IPv4", "endpoints": null})],
    );
    d.pass(&handle, vec![kube_target("m")]).await;
    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_STALE);
    assert_eq!(s.ready_replicas, Some(0));
    assert_eq!(s.effective_max_in_flight, 8);
    assert_eq!(s.derived_max_in_flight, Some(8));
    assert!(s.reason.unwrap().contains("no ready endpoint (0 listed)"));
    assert_eq!(pool_size(&fs, "m").await, 8);

    // Back with three replicas: the pool grows.
    answer(
        &api,
        "inference",
        "m-served",
        vec![slice(
            "s",
            (0..3)
                .map(|i| ready(&format!("p{i}"), &format!("10.0.0.{i}")))
                .collect(),
        )],
    );
    d.pass(&handle, vec![kube_target("m")]).await;
    assert_eq!(status(&handle, "m").state, STATE_DISCOVERED);
    assert_eq!(pool_size(&fs, "m").await, 24);
}

#[tokio::test]
async fn with_nothing_discovered_yet_the_static_value_applies() {
    let (client, api) = fake_api().await;
    answer(&api, "inference", "m-served", vec![slice("s", vec![])]);
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
    answer(
        &api,
        "inference",
        "m-served",
        vec![slice("s", vec![ready("a", "10.0.0.1")])],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    d.pass(&handle, vec![kube_target("m")]).await;

    fail(&api, "inference", "m-served", 403);
    d.pass(&handle, vec![kube_target("m")]).await;
    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_STALE);
    assert_eq!(s.effective_max_in_flight, 8);
    assert_eq!(s.ready_replicas, None);
    let reason = s.reason.unwrap();
    assert!(reason.contains("HTTP 403"), "{reason}");
    assert!(
        reason.contains("get/list/watch on endpointslices"),
        "{reason}"
    );

    // A model that never had a value falls back to its static one.
    let mut other = kube_target("o");
    other.service = Some("o".into());
    fail(&api, "inference", "o", 500);
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
async fn a_kubernetes_model_without_a_per_replica_value_is_not_counted() {
    let (client, api) = fake_api().await;
    answer(
        &api,
        "inference",
        "m-served",
        vec![slice("s", vec![ready("a", "10.0.0.1")])],
    );
    let handle = settings(&["inference"], TEMPLATE);
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    let mut t = kube_target("m");
    t.per_replica_max_in_flight = None;

    d.pass(&handle, vec![t]).await;

    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_FALLBACK);
    assert_eq!(s.effective_max_in_flight, 6);
    let reason = s.reason.unwrap();
    assert!(
        reason.contains("per_replica_max_in_flight is required"),
        "{reason}"
    );
    assert!(asked(&api).is_empty(), "nothing is read for it");
}

#[tokio::test]
async fn the_default_service_template_names_each_models_service() {
    let (client, api) = fake_api().await;
    answer(
        &api,
        "inference",
        "chat-serve",
        vec![slice("s", vec![ready("a", "10.0.0.1")])],
    );
    let handle = settings(&["inference"], "{model_name}-serve");
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    // A name the template cannot turn into a Service name.
    let mut odd = kube_target("Odd/Name");
    odd.upstream_model = "x".into();

    d.pass(&handle, vec![kube_target("chat"), odd]).await;

    let s = status(&handle, "chat");
    assert_eq!(s.service.as_deref(), Some("chat-serve"));
    assert_eq!(s.state, STATE_DISCOVERED);
    let r = status(&handle, "Odd/Name").reason.unwrap();
    assert!(r.contains("set capacity_service"), "{r}");
    assert_eq!(
        asked(&api),
        vec![("inference".to_string(), "chat-serve".to_string())]
    );
}

#[tokio::test]
async fn gateway_policy_problems_are_reported_without_calling_the_api() {
    let (client, api) = fake_api().await;
    let handle = settings(&["inference"], "");
    let mut d = Discoverer::new(Some(Ok(client)), fairshare(), 32);
    let mut outside = kube_target("outside");
    outside.namespace = Some("kube-system".into());
    outside.service = Some("x".into());
    let no_service = kube_target("no-service");

    d.pass(&handle, vec![outside, no_service]).await;

    let r = status(&handle, "outside").reason.unwrap();
    assert!(r.contains("OBLETH_CAPACITY_DISCOVERY_NAMESPACES"), "{r}");
    let r = status(&handle, "no-service").reason.unwrap();
    assert!(r.contains("OBLETH_CAPACITY_DEFAULT_SERVICE"), "{r}");
    assert!(api.lock().unwrap().paths.is_empty());

    // No namespaces at all: the kubernetes source is unavailable.
    let handle = settings(&[], "");
    let mut d = Discoverer::new(None, fairshare(), 32);
    d.pass(&handle, vec![kube_target("m")]).await;
    let s = status(&handle, "m");
    assert_eq!(s.state, STATE_FALLBACK);
    assert!(s.reason.unwrap().contains("not available"));
}

#[test]
fn the_only_url_the_client_builds_is_an_endpoint_slice_list() {
    let client = KubeClient::with_base("https://api.example:6443/", None);
    let url = client
        .endpoint_slices_url("inference", "my-model", None)
        .unwrap();
    assert_eq!(
        url.path(),
        "/apis/discovery.k8s.io/v1/namespaces/inference/endpointslices"
    );
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    assert_eq!(
        query["labelSelector"],
        "kubernetes.io/service-name=my-model"
    );
    assert_eq!(query["resourceVersion"], "0");
    let next = client
        .endpoint_slices_url("inference", "my-model", Some("tok"))
        .unwrap();
    assert!(next.query().unwrap().contains("continue=tok"));
}

/// A guard on the source itself: the kubernetes source never reads pods, or
/// any core-group resource. Only the EndpointSlice path may appear in the
/// client and the discovery loop.
#[test]
fn no_code_path_in_the_kubernetes_source_targets_pods() {
    for (file, source) in [
        ("kube.rs", include_str!("kube.rs")),
        ("mod.rs", include_str!("mod.rs")),
    ] {
        for forbidden in ["/pods", "/api/v1/", "PodList"] {
            assert!(
                !source.contains(forbidden),
                "{file} mentions `{forbidden}`: the kubernetes source may only read EndpointSlices"
            );
        }
    }
    assert!(include_str!("kube.rs").contains("/apis/discovery.k8s.io/v1/namespaces/"));
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
        service: None,
        per_replica_max_in_flight: None,
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
    assert_eq!(s.per_replica_source.as_deref(), Some(PER_REPLICA_ENDPOINT));

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
        capacity_service: None,
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
