//! `GET /v1/load`: how busy each model the caller can use is right now, so a
//! client can say a model is busy or full, and why a request is waiting.
//!
//! One entry per id the caller's `GET /v1/models` lists, with the numbers
//! the dashboard's Fairshare page shows for that model's pool: its slots,
//! the requests running in it and the requests waiting for it. A variant
//! reports its model's pool and a family its default member's. `you` is the
//! caller's own requests in flight and the in-flight cap that applies to
//! them. Nothing about other tenants or keys is included.
//!
//! The pool numbers are read at most once a second per gateway replica,
//! however many clients poll, and shared by every caller; `you` is read from
//! the same reading. The call takes no fairshare slot, reserves no budget and writes
//! no ledger row. `OBLETH_LOAD_ENDPOINT=false` turns it off: the path then
//! answers 404 like an unknown one.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Response, StatusCode};
use axum::response::IntoResponse;
use obleth_admin::capacity_discovery::CapacityDiscovery;
use obleth_admin::live_load::{self, LiveLoad, LoadSettings, PoolLoad};
use obleth_config::ResolvedKey;
use obleth_fairshare::FairShare;
use serde::Serialize;
use uuid::Uuid;

use crate::proxy::{self, error_json, gate_resolved_key};
use crate::reasoning::FamilyRegistry;
use crate::router::ModelRegistry;
use crate::state::AppState;

pub(crate) const LOAD_PATH: &str = "/v1/load";

/// How long one reading of the pools is served before the next is taken.
const READING_TTL: Duration = Duration::from_secs(1);

/// What the load endpoint reads pools and models from, and its last
/// reading. Cheap to clone.
#[derive(Clone)]
pub struct LoadReporter {
    fairshare: FairShare,
    registry: ModelRegistry,
    families: FamilyRegistry,
    discovery: CapacityDiscovery,
    settings: LoadSettings,
    /// The last reading and when it was taken. Held while a reading is
    /// taken, so callers that arrive meanwhile wait for it rather than each
    /// taking their own.
    last: Arc<tokio::sync::Mutex<Option<Reading>>>,
}

/// A reading of every pool, and when it was taken.
type Reading = (Instant, Arc<LiveLoad>);

impl LoadReporter {
    pub(crate) fn new(
        fairshare: FairShare,
        registry: ModelRegistry,
        families: FamilyRegistry,
        discovery: CapacityDiscovery,
        settings: LoadSettings,
    ) -> Self {
        LoadReporter {
            fairshare,
            registry,
            families,
            discovery,
            settings,
            last: Arc::default(),
        }
    }

    /// Every registered model's pool load, at most [`READING_TTL`] old.
    /// `None` when the scheduler does not answer.
    async fn reading(&self) -> Option<Arc<LiveLoad>> {
        let mut last = self.last.lock().await;
        if let Some((at, load)) = last.as_ref() {
            if at.elapsed() < READING_TTL {
                return Some(load.clone());
            }
        }
        let routes = self.registry.load();
        let load = Arc::new(
            live_load::live_load(
                &self.fairshare,
                &self.settings,
                &self.discovery,
                routes.iter().map(|c| &c.model),
            )
            .await?,
        );
        *last = Some((Instant::now(), load.clone()));
        Some(load)
    }

    /// The answer for an authenticated caller: every model it is offered,
    /// or only `model`.
    async fn answer(
        &self,
        resolved: &ResolvedKey,
        model: Option<&str>,
        headers: &HeaderMap,
    ) -> Response<Body> {
        let mut listed = proxy::listed_pools(&self.registry, &self.families, resolved);
        let wanted = model.map(str::trim).filter(|m| !m.is_empty());
        if let Some(name) = wanted {
            // A name the caller is not offered answers like one that does
            // not exist, as `GET /v1/models/{id}` does.
            listed.retain(|(id, _)| id == name);
            if listed.is_empty() {
                return error_json(StatusCode::NOT_FOUND, &format!("model '{name}' not found"));
            }
        }
        let Some(reading) = self.reading().await else {
            return error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "the fairshare scheduler is not answering",
            );
        };
        let key = scheduler_key(resolved, headers);
        let pool = wanted.and(listed.first().map(|(_, pool)| pool.as_str()));
        let you = You {
            in_flight: reading.key_in_flight(key, pool),
            limit: tightest_cap(resolved, reading.divisor),
        };
        let mut resp = (
            StatusCode::OK,
            axum::Json(load_list(&reading, &listed, you)),
        )
            .into_response();
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        resp
    }
}

/// How busy a model is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum LoadState {
    /// Under 80% of its slots in use.
    Free,
    /// At least 80% of its slots in use, none waiting.
    Busy,
    /// Every slot in use, or requests waiting: a new request waits.
    Full,
    /// No slots: the model has nowhere to send a request right now.
    Offline,
}

impl LoadState {
    fn of(load: PoolLoad) -> Self {
        if load.slots == 0 {
            LoadState::Offline
        } else if load.in_flight >= load.slots || load.waiting > 0 {
            LoadState::Full
        } else if load.in_flight >= (load.slots * 4).div_ceil(5) {
            LoadState::Busy
        } else {
            LoadState::Free
        }
    }
}

#[derive(Debug, Serialize)]
struct LoadList {
    object: &'static str,
    scope: &'static str,
    data: Vec<ModelLoad>,
    you: You,
}

#[derive(Debug, Serialize)]
struct ModelLoad {
    id: String,
    state: LoadState,
    slots: usize,
    in_flight: usize,
    waiting: usize,
}

/// The caller's own requests and cap.
#[derive(Debug, Serialize)]
struct You {
    in_flight: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<usize>,
}

/// `GET /v1/load`.
pub async fn load(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response<Body> {
    let request_id = Uuid::new_v4();
    let mut resp = handle(&state, query.get("model").map(String::as_str), &headers).await;
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        resp.headers_mut()
            .entry("x-obleth-request-id")
            .or_insert(value);
    }
    resp
}

async fn handle(state: &AppState, model: Option<&str>, headers: &HeaderMap) -> Response<Body> {
    let Some(reporter) = &state.load else {
        return error_json(StatusCode::NOT_FOUND, "unknown endpoint");
    };
    let Some(secret) = proxy::bearer(headers) else {
        return error_json(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    let resolved = match crate::jwt_auth::authenticate_credential(state, &secret).await {
        Ok(cred) => cred.resolved,
        Err(resp) => return resp,
    };
    if let Err(resp) = gate_resolved_key(state, &resolved) {
        return resp;
    }
    reporter.answer(&resolved, model, headers).await
}

/// The response for `listed` (id, pool) pairs. A pool with no reading (a
/// model registered since it was taken) reports no slots until the next.
fn load_list(reading: &LiveLoad, listed: &[(String, String)], you: You) -> LoadList {
    LoadList {
        object: "list",
        scope: reading.scope,
        data: listed
            .iter()
            .map(|(id, pool)| {
                let load = reading.pools.get(pool).copied().unwrap_or_default();
                ModelLoad {
                    id: id.clone(),
                    state: LoadState::of(load),
                    slots: load.slots,
                    in_flight: load.in_flight,
                    waiting: load.waiting,
                }
            })
            .collect(),
        you,
    }
}

/// The scheduler key the caller's next request would queue under: its own
/// key, or for a key with per-end-user fairshare, the end user named in
/// `x-obleth-end-user`.
fn scheduler_key(resolved: &ResolvedKey, headers: &HeaderMap) -> Uuid {
    proxy::end_user_for(resolved, headers, &serde_json::Value::Null)
        .map_or(resolved.key_id, |user| {
            obleth_config::end_user_key_id(resolved.key_id, &user)
        })
}

/// The tighter of the key's and the tenant's in-flight caps, as the
/// answering replica enforces it (its share when the caps are split across
/// replicas). `None` when neither is set. Each holds in every model's pool
/// separately.
fn tightest_cap(resolved: &ResolvedKey, divisor: usize) -> Option<usize> {
    let positive = |c: Option<i64>| c.and_then(|c| usize::try_from(c).ok()).filter(|c| *c > 0);
    [
        positive(resolved.key_max_in_flight),
        positive(resolved.max_in_flight),
    ]
    .into_iter()
    .flatten()
    .min()
    .map(|cap| obleth_fairshare::replica_share(cap, divisor))
}

#[cfg(test)]
mod tests {
    use obleth_config::routing::Candidate;
    use obleth_config::{FamilyMembers, ModelFamily, ReasoningLevel, ResolvedModel};
    use obleth_fairshare::{AdmitRequest, Admitted};

    use super::*;

    fn load(slots: usize, in_flight: usize, waiting: usize) -> PoolLoad {
        PoolLoad {
            slots,
            in_flight,
            waiting,
        }
    }

    #[test]
    fn the_state_follows_the_slots_in_use_and_the_queue() {
        assert_eq!(LoadState::of(load(0, 0, 0)), LoadState::Offline);
        assert_eq!(LoadState::of(load(0, 3, 2)), LoadState::Offline);
        assert_eq!(LoadState::of(load(40, 0, 0)), LoadState::Free);
        assert_eq!(LoadState::of(load(40, 31, 0)), LoadState::Free);
        // Busy from ceil(0.8 x slots).
        assert_eq!(LoadState::of(load(40, 32, 0)), LoadState::Busy);
        assert_eq!(LoadState::of(load(40, 39, 0)), LoadState::Busy);
        assert_eq!(LoadState::of(load(10, 7, 0)), LoadState::Free);
        assert_eq!(LoadState::of(load(10, 8, 0)), LoadState::Busy);
        assert_eq!(LoadState::of(load(3, 2, 0)), LoadState::Free);
        assert_eq!(LoadState::of(load(3, 3, 0)), LoadState::Full);
        assert_eq!(LoadState::of(load(1, 0, 0)), LoadState::Free);
        assert_eq!(LoadState::of(load(1, 1, 0)), LoadState::Full);
        // Full when every slot is taken (shared counts can run over), or
        // when anyone waits.
        assert_eq!(LoadState::of(load(40, 40, 0)), LoadState::Full);
        assert_eq!(LoadState::of(load(40, 41, 0)), LoadState::Full);
        assert_eq!(LoadState::of(load(40, 2, 1)), LoadState::Full);
    }

    #[test]
    fn the_response_has_one_entry_per_listed_id_in_the_contract_shape() {
        let reading = LiveLoad {
            scope: "cluster",
            divisor: 1,
            pools: HashMap::from([
                ("glm-5-3".to_string(), load(40, 34, 0)),
                ("small".to_string(), load(8, 8, 2)),
                ("down".to_string(), load(0, 0, 0)),
            ]),
            keys: HashMap::new(),
        };
        let listed = vec![
            ("down".to_string(), "down".to_string()),
            ("glm-5-3".to_string(), "glm-5-3".to_string()),
            ("glm-5-3-search".to_string(), "glm-5-3".to_string()),
            ("new".to_string(), "new".to_string()),
            ("small".to_string(), "small".to_string()),
        ];
        let you = You {
            in_flight: 2,
            limit: Some(4),
        };
        let body = serde_json::to_value(load_list(&reading, &listed, you)).unwrap();
        assert_eq!(
            body,
            serde_json::json!({
                "object": "list",
                "scope": "cluster",
                "data": [
                    {"id": "down", "state": "offline", "slots": 0, "in_flight": 0, "waiting": 0},
                    {"id": "glm-5-3", "state": "busy", "slots": 40, "in_flight": 34, "waiting": 0},
                    {"id": "glm-5-3-search", "state": "busy", "slots": 40, "in_flight": 34, "waiting": 0},
                    {"id": "new", "state": "offline", "slots": 0, "in_flight": 0, "waiting": 0},
                    {"id": "small", "state": "full", "slots": 8, "in_flight": 8, "waiting": 2},
                ],
                "you": {"in_flight": 2, "limit": 4},
            })
        );

        // No cap: `limit` is left out.
        let you = You {
            in_flight: 0,
            limit: None,
        };
        let body = serde_json::to_value(load_list(&reading, &listed[..1], you)).unwrap();
        assert_eq!(body["you"], serde_json::json!({"in_flight": 0}));
    }

    #[test]
    fn the_tightest_cap_applies_as_the_replica_enforces_it() {
        let mut key = crate::boons::test_support::test_key();
        key.max_in_flight = None;
        key.key_max_in_flight = None;
        assert_eq!(tightest_cap(&key, 1), None);
        key.max_in_flight = Some(10);
        assert_eq!(tightest_cap(&key, 1), Some(10));
        key.key_max_in_flight = Some(4);
        assert_eq!(tightest_cap(&key, 1), Some(4));
        key.key_max_in_flight = Some(0);
        assert_eq!(tightest_cap(&key, 1), Some(10), "0 is no cap");
        key.key_max_in_flight = Some(-1);
        key.max_in_flight = Some(5);
        assert_eq!(tightest_cap(&key, 2), Some(3), "split across two replicas");
    }

    #[test]
    fn an_end_user_of_a_shared_key_is_its_own_scheduler_key() {
        let mut key = crate::boons::test_support::test_key();
        let mut headers = HeaderMap::new();
        headers.insert(proxy::END_USER_HEADER, HeaderValue::from_static("alice"));
        key.end_user_fairshare = false;
        assert_eq!(scheduler_key(&key, &headers), key.key_id);
        key.end_user_fairshare = true;
        assert_eq!(
            scheduler_key(&key, &headers),
            obleth_config::end_user_key_id(key.key_id, "alice")
        );
        assert_eq!(scheduler_key(&key, &HeaderMap::new()), key.key_id);
    }

    fn route(name: &str, max_in_flight: usize) -> ResolvedModel {
        let mut route = crate::boons::test_support::test_route();
        route.model_name = name.into();
        route.upstream_model = name.into();
        route.max_in_flight = Some(max_in_flight);
        route
    }

    fn candidate(model: ResolvedModel) -> Candidate {
        Candidate {
            model,
            healthy: true,
            levels: Vec::new(),
        }
    }

    /// A registry of four models: `busy` (10 slots, with a variant), `quiet`
    /// (8), `hidden` (unlisted) and `other`, and a family `fam` whose every
    /// level is `quiet`.
    fn sources() -> (FairShare, ModelRegistry, FamilyRegistry) {
        let fairshare = FairShare::start(
            Arc::new(obleth_fairshare::StaticCapacity::new(64)),
            obleth_config::FairshareAlgorithm::default(),
            32,
        );
        let mut busy = route("busy", 10);
        busy.variants = vec![obleth_config::ModelVariant {
            name: "busy-search".into(),
            description: "busy with web search".into(),
            boons: vec![],
        }];
        let mut hidden = route("hidden", 8);
        hidden.listed = false;
        let registry = ModelRegistry::new();
        registry.store(
            [busy, route("quiet", 8), hidden, route("other", 8)]
                .into_iter()
                .map(candidate)
                .collect(),
        );
        let families = FamilyRegistry::new();
        families.store(vec![ModelFamily {
            id: Uuid::new_v4(),
            name: "fam".into(),
            description: String::new(),
            default_level: ReasoningLevel::Medium,
            members: FamilyMembers {
                none: "quiet".into(),
                low: "quiet".into(),
                medium: "quiet".into(),
                high: "quiet".into(),
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }]);
        (fairshare, registry, families)
    }

    fn reporter(sources: &(FairShare, ModelRegistry, FamilyRegistry)) -> LoadReporter {
        LoadReporter::new(
            sources.0.clone(),
            sources.1.clone(),
            sources.2.clone(),
            CapacityDiscovery::new(Default::default()),
            LoadSettings {
                replica_aware: true,
                replicas_known: true,
                default_cap: 32,
            },
        )
    }

    /// A caller allowed `busy`, `quiet` and `hidden`, capped at 4 in flight
    /// itself and at 6 for its tenant.
    fn caller() -> ResolvedKey {
        let mut key = crate::boons::test_support::test_key();
        key.key_id = Uuid::new_v4();
        key.tenant_id = Uuid::new_v4();
        key.allowed_models = Some(vec!["busy".into(), "quiet".into(), "hidden".into()]);
        key.key_max_in_flight = Some(4);
        key.max_in_flight = Some(6);
        key
    }

    async fn run(fairshare: &FairShare, key: Uuid, model: &str, n: usize) -> Vec<Admitted> {
        let mut held = Vec::new();
        for _ in 0..n {
            let req = AdmitRequest::new(Uuid::new_v4(), model, 1)
                .key(key, 100)
                .model_cap(10);
            held.push(fairshare.admit(req).await.expect("admitted"));
        }
        held
    }

    async fn json_of(resp: Response<Body>) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).expect("json")
    }

    #[tokio::test]
    async fn the_answer_covers_the_models_the_caller_is_offered() {
        let sources = sources();
        let reporter = reporter(&sources);
        let key = caller();
        let _mine = run(&sources.0, key.key_id, "busy", 3).await;
        let _theirs = run(&sources.0, Uuid::new_v4(), "busy", 5).await;

        let resp = reporter.answer(&key, None, &HeaderMap::new()).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CACHE_CONTROL], "no-store");
        // `other` is outside the allowlist and `hidden` is unlisted; the
        // variant and the family report their models' pools. Nobody else's
        // requests are named, only counted in the pool.
        assert_eq!(
            json_of(resp).await,
            serde_json::json!({
                "object": "list",
                "scope": "cluster",
                "data": [
                    {"id": "busy", "state": "busy", "slots": 10, "in_flight": 8, "waiting": 0},
                    {"id": "busy-search", "state": "busy", "slots": 10, "in_flight": 8, "waiting": 0},
                    {"id": "fam", "state": "free", "slots": 8, "in_flight": 0, "waiting": 0},
                    {"id": "quiet", "state": "free", "slots": 8, "in_flight": 0, "waiting": 0},
                ],
                "you": {"in_flight": 3, "limit": 4},
            })
        );

        // One model: its entry, and the caller's requests in its pool only.
        let body = json_of(
            reporter
                .answer(&key, Some("quiet"), &HeaderMap::new())
                .await,
        )
        .await;
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
        assert_eq!(body["data"][0]["id"], "quiet");
        assert_eq!(body["you"], serde_json::json!({"in_flight": 0, "limit": 4}));
        let body = json_of(
            reporter
                .answer(&key, Some("busy-search"), &HeaderMap::new())
                .await,
        )
        .await;
        assert_eq!(body["you"]["in_flight"], 3);

        // A model the caller is not offered is not found.
        for name in ["other", "hidden", "nope"] {
            let resp = reporter.answer(&key, Some(name), &HeaderMap::new()).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{name}");
        }
    }

    #[tokio::test]
    async fn a_reading_is_reused_for_a_second() {
        let sources = sources();
        let reporter = reporter(&sources);
        let key = caller();
        let _first = run(&sources.0, key.key_id, "busy", 1).await;
        let body = json_of(reporter.answer(&key, Some("busy"), &HeaderMap::new()).await).await;
        assert_eq!(body["data"][0]["in_flight"], 1);

        let _second = run(&sources.0, key.key_id, "busy", 1).await;
        let body = json_of(reporter.answer(&key, Some("busy"), &HeaderMap::new()).await).await;
        assert_eq!(
            body["data"][0]["in_flight"], 1,
            "served from the last reading"
        );
        assert_eq!(body["you"]["in_flight"], 1);

        *reporter.last.lock().await = None;
        let body = json_of(reporter.answer(&key, Some("busy"), &HeaderMap::new()).await).await;
        assert_eq!(body["data"][0]["in_flight"], 2);
        assert_eq!(body["you"]["in_flight"], 2);
    }

    // ---- through the gateway's router (needs Postgres and Redis) ----

    struct Gateway {
        base: String,
        state: AppState,
        wal_dir: std::path::PathBuf,
    }

    impl Drop for Gateway {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.wal_dir);
        }
    }

    impl Gateway {
        async fn key(&self, key: ResolvedKey) -> String {
            let secret = format!("sk-load-{}", Uuid::new_v4());
            self.state
                .key_cache
                .insert(obleth_config::hash_api_key(&secret), Arc::new(key))
                .await;
            secret
        }

        async fn get(&self, path: &str, secret: Option<&str>) -> reqwest::Response {
            let mut req = reqwest::Client::new().get(format!("{}{path}", self.base));
            if let Some(secret) = secret {
                req = req.bearer_auth(secret);
            }
            req.send().await.expect("gateway answers")
        }
    }

    /// The gateway's real proxy router on a local port, with the load
    /// endpoint on or off, over [`sources`].
    async fn gateway(on: bool) -> Option<Gateway> {
        let (Ok(db), Ok(redis_url)) = (
            std::env::var("OBLETH_TEST_DATABASE_URL"),
            std::env::var("OBLETH_TEST_REDIS_URL"),
        ) else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return None;
        };
        let store = obleth_store::Store::connect(&db).await.expect("postgres");
        let redis = obleth_redis::RedisStore::connect(&redis_url)
            .await
            .expect("redis");
        let wal_dir = std::env::temp_dir().join(format!("obleth-load-wal-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&wal_dir).unwrap();
        let telemetry = obleth_telemetry::TelemetrySink::start(
            "http://127.0.0.1:1",
            "obleth_test",
            "default",
            "",
            wal_dir.join("usage.wal").to_str().unwrap(),
            Duration::from_millis(100),
            true,
        )
        .await
        .expect("telemetry sink");
        let sources = sources();
        let http = reqwest::Client::new();
        let state = AppState {
            redis,
            fairshare: sources.0.clone(),
            tokenizer: Arc::new(obleth_tokenizer::HeuristicTokenizer::new()),
            telemetry,
            http: http.clone(),
            upstream_base: "http://127.0.0.1:1/v1".into(),
            upstream_timeout: Duration::from_secs(30),
            key_cache: moka::future::Cache::builder().build(),
            model_cache: moka::future::Cache::builder().build(),
            mcp_cache: moka::future::Cache::builder().build(),
            model_registry: sources.1.clone(),
            families: sources.2.clone(),
            classifier: crate::classifier::Classifier::new(Default::default()),
            output_stats: Default::default(),
            boons: crate::boons::BoonEngine::new(Default::default()),
            tool_cache: moka::future::Cache::builder().build(),
            metrics: Arc::new(crate::metrics::Metrics::new()),
            fail_open: false,
            alerts: obleth_admin::AlertDispatcher::new(http, Default::default()),
            session_id_derivation: false,
            session_affinity: None,
            session_affinity_settings: Default::default(),
            compressor: None,
            energy: crate::energy::EnergyEngine::new(Default::default()),
            jwt: None,
            knowledge: Arc::new(crate::knowledge::KnowledgeIndex::new()),
            jobs: crate::jobs::JobStore::new(store, Default::default()),
            load: on.then(|| reporter(&sources)),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let app = crate::proxy_router(state.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Some(Gateway {
            base,
            state,
            wal_dir,
        })
    }

    #[tokio::test]
    async fn the_route_answers_a_caller_with_a_key() {
        let Some(gw) = gateway(true).await else {
            return;
        };
        let key = caller();
        let _mine = run(&gw.state.fairshare, key.key_id, "busy", 2).await;
        let secret = gw.key(key).await;

        let resp = gw.get(LOAD_PATH, None).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
        let resp = gw.get(LOAD_PATH, Some("sk-load-unknown")).await;
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);

        let resp = gw.get(LOAD_PATH, Some(&secret)).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().contains_key("x-obleth-request-id"));
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["object"], "list");
        let ids: Vec<&str> = body["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["busy", "busy-search", "fam", "quiet"]);
        assert_eq!(
            body["data"][0],
            serde_json::json!({"id": "busy", "state": "free", "slots": 10, "in_flight": 2, "waiting": 0})
        );
        assert_eq!(body["you"], serde_json::json!({"in_flight": 2, "limit": 4}));

        let resp = gw
            .get(&format!("{LOAD_PATH}?model=quiet"), Some(&secret))
            .await;
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["data"].as_array().unwrap().len(), 1);
        let resp = gw
            .get(&format!("{LOAD_PATH}?model=other"), Some(&secret))
            .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn the_route_is_not_found_when_turned_off() {
        let Some(gw) = gateway(false).await else {
            return;
        };
        let secret = gw.key(caller()).await;
        for secret in [None, Some(secret.as_str())] {
            let resp = gw.get(LOAD_PATH, secret).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND);
            let body: serde_json::Value = resp.json().await.unwrap();
            assert_eq!(body["error"]["message"], "unknown endpoint");
        }
    }
}
