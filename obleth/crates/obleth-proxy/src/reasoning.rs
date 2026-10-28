//! Reasoning effort at the request edge (see [`obleth_config::reasoning`]).
//!
//! The caller's effort is read once, before the model is known: from the
//! chat body, or — for `/v1/responses` and `/v1/messages`, whose own effort
//! fields do not survive translation to chat — from [`EFFORT_HEADER`], which
//! the shims set. Once the route is known, the route's map turns it into the
//! fields its server understands ([`obleth_config::ModelReasoning::plan`]),
//! and the response says what was served.
//!
//! A family name resolves here too: the level picks the member, and the
//! member serves the request as if the caller had named it.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::http::{HeaderMap, HeaderValue, Response};
use obleth_config::{ModelFamily, ReasoningLevel, ReasoningPlan};

/// The effort a Responses or Messages caller asked for, carried from the
/// shim to the chat pipeline. The gateway's own annotation: stripped from
/// every incoming request before the shims set it.
pub(crate) const EFFORT_HEADER: &str = "x-obleth-internal-reasoning-effort";
/// On a response: the level the request was served at, or `unsupported`
/// when the model has no effort control and the caller's was removed.
pub(crate) const SERVED_HEADER: &str = "x-obleth-reasoning-effort";
/// On a response whose served level differs from the one asked for (a model
/// without that level serves it as another).
pub(crate) const REQUESTED_HEADER: &str = "x-obleth-reasoning-effort-requested";

/// Lock-free, hot-swappable map of model families by name, refreshed with
/// the model registry.
#[derive(Clone)]
pub(crate) struct FamilyRegistry {
    inner: Arc<ArcSwap<HashMap<String, Arc<ModelFamily>>>>,
}

impl Default for FamilyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FamilyRegistry {
    pub(crate) fn new() -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(HashMap::new())),
        }
    }

    pub(crate) fn get(&self, name: &str) -> Option<Arc<ModelFamily>> {
        self.inner.load().get(name).cloned()
    }

    /// Every family, by name.
    pub(crate) fn all(&self) -> Vec<Arc<ModelFamily>> {
        let mut out: Vec<Arc<ModelFamily>> = self.inner.load().values().cloned().collect();
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub(crate) fn store(&self, families: Vec<ModelFamily>) {
        self.inner.store(Arc::new(
            families
                .into_iter()
                .map(|f| (f.name.clone(), Arc::new(f)))
                .collect(),
        ));
    }
}

/// The effort a request asked for, read before its model is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Requested {
    /// No effort named.
    Nothing,
    Level(ReasoningLevel),
    /// A value obleth does not know. Refused for a model that translates
    /// effort (it could not be translated); left alone for one that passes
    /// requests through.
    Unknown(String),
}

impl Requested {
    /// Read the request: the shim's header first, else the chat body.
    pub(crate) fn read(headers: &HeaderMap, body: &serde_json::Value) -> Self {
        if let Some(raw) = headers.get(EFFORT_HEADER).and_then(|v| v.to_str().ok()) {
            return match ReasoningLevel::parse(raw) {
                Some(level) => Requested::Level(level),
                None => Requested::Unknown(raw.to_string()),
            };
        }
        match obleth_config::reasoning::chat_effort(body) {
            Ok(Some(level)) => Requested::Level(level),
            Ok(None) => Requested::Nothing,
            Err(raw) => Requested::Unknown(raw),
        }
    }

    /// The level, or the refusal message for an unknown value.
    pub(crate) fn level(&self) -> Result<Option<ReasoningLevel>, String> {
        match self {
            Requested::Nothing => Ok(None),
            Requested::Level(l) => Ok(Some(*l)),
            Requested::Unknown(raw) => Err(unknown_message(raw)),
        }
    }
}

/// The refusal for an effort value obleth cannot translate.
pub(crate) fn unknown_message(raw: &str) -> String {
    format!(
        "reasoning effort `{}` is not one of none, low, medium or high",
        raw.trim_matches('"')
    )
}

/// The header value a shim forwards for a level it read: the canonical
/// spelling, or the raw value when it is not one obleth knows (so the
/// pipeline can refuse it with the caller's own word).
pub(crate) fn header_value(read: Result<Option<ReasoningLevel>, String>) -> Option<HeaderValue> {
    let raw = match read {
        Ok(Some(level)) => level.as_str().to_string(),
        Ok(None) => return None,
        Err(raw) => raw,
    };
    HeaderValue::from_str(&raw).ok()
}

/// What a response says about effort.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Note {
    Served {
        served: ReasoningLevel,
        requested: Option<ReasoningLevel>,
    },
    /// The caller named an effort the model has no control for; it was
    /// removed.
    Unsupported,
}

impl Note {
    /// The note for a plan carried out, if the response has anything to say.
    pub(crate) fn of(plan: &ReasoningPlan) -> Option<Self> {
        match plan {
            ReasoningPlan::Untouched => None,
            ReasoningPlan::Strip { requested: None } => None,
            ReasoningPlan::Strip { requested: Some(_) } => Some(Note::Unsupported),
            ReasoningPlan::Apply {
                requested, served, ..
            } => Some(Note::Served {
                served: *served,
                requested: *requested,
            }),
        }
    }

    /// The note for a request that came through a family at `level`: the
    /// level the member served (after its own map), and the one the caller
    /// asked for when that differs.
    pub(crate) fn for_family(
        level: ReasoningLevel,
        requested: Option<ReasoningLevel>,
        plan: &ReasoningPlan,
    ) -> Self {
        let served = match plan {
            ReasoningPlan::Apply { served, .. } => *served,
            ReasoningPlan::Untouched | ReasoningPlan::Strip { .. } => level,
        };
        Note::Served { served, requested }
    }

    /// The served value, as the response header and the usage ledger spell it.
    pub(crate) fn served_label(&self) -> &'static str {
        match self {
            Note::Unsupported => "unsupported",
            Note::Served { served, .. } => served.as_str(),
        }
    }

    pub(crate) fn apply_headers(&self, resp: &mut Response<Body>) {
        let headers = resp.headers_mut();
        match self {
            Note::Unsupported => {
                headers.insert(SERVED_HEADER, HeaderValue::from_static("unsupported"));
            }
            Note::Served { served, requested } => {
                headers.insert(SERVED_HEADER, HeaderValue::from_static(served.as_str()));
                if let Some(r) = requested.filter(|r| r != served) {
                    headers.insert(REQUESTED_HEADER, HeaderValue::from_static(r.as_str()));
                }
            }
        }
    }
}

/// The level a request is served at when it came through `family`, and
/// the member that serves it.
pub(crate) fn family_member(
    family: &ModelFamily,
    requested: &Requested,
) -> Result<(ReasoningLevel, String), String> {
    let (level, member) = family.member_for(requested.level()?);
    Ok((level, member.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use obleth_config::FamilyMembers;
    use serde_json::json;

    fn family() -> ModelFamily {
        ModelFamily {
            id: uuid::Uuid::nil(),
            name: "acme-7b".into(),
            description: String::new(),
            default_level: ReasoningLevel::None,
            members: FamilyMembers {
                none: "acme-7b-instruct".into(),
                low: "acme-7b-think".into(),
                medium: "acme-7b-think".into(),
                high: "acme-7b-think".into(),
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn the_shim_header_wins_over_the_body() {
        let mut headers = HeaderMap::new();
        headers.insert(EFFORT_HEADER, HeaderValue::from_static("high"));
        let body = json!({"reasoning_effort": "low"});
        assert_eq!(
            Requested::read(&headers, &body),
            Requested::Level(ReasoningLevel::High)
        );
        assert_eq!(
            Requested::read(&HeaderMap::new(), &body),
            Requested::Level(ReasoningLevel::Low)
        );
        assert_eq!(
            Requested::read(&HeaderMap::new(), &json!({})),
            Requested::Nothing
        );
    }

    #[test]
    fn an_unknown_value_is_kept_for_the_refusal() {
        let r = Requested::read(&HeaderMap::new(), &json!({"reasoning_effort": "turbo"}));
        assert_eq!(r, Requested::Unknown("turbo".into()));
        assert!(r.level().unwrap_err().contains("`turbo`"));
    }

    #[test]
    fn a_family_sends_each_level_to_its_member() {
        let f = family();
        assert_eq!(
            family_member(&f, &Requested::Nothing).unwrap(),
            (ReasoningLevel::None, "acme-7b-instruct".to_string())
        );
        assert_eq!(
            family_member(&f, &Requested::Level(ReasoningLevel::Medium)).unwrap(),
            (ReasoningLevel::Medium, "acme-7b-think".to_string())
        );
        assert!(family_member(&f, &Requested::Unknown("x".into())).is_err());
    }

    #[test]
    fn the_registry_answers_by_name() {
        let reg = FamilyRegistry::new();
        assert!(reg.get("acme-7b").is_none());
        reg.store(vec![family()]);
        assert_eq!(reg.get("acme-7b").unwrap().members.none, "acme-7b-instruct");
        assert_eq!(reg.all().len(), 1);
    }

    #[test]
    fn notes_name_the_served_level_and_a_differing_request() {
        let mut resp = Response::new(Body::empty());
        Note::Served {
            served: ReasoningLevel::Low,
            requested: Some(ReasoningLevel::None),
        }
        .apply_headers(&mut resp);
        assert_eq!(resp.headers()[SERVED_HEADER], "low");
        assert_eq!(resp.headers()[REQUESTED_HEADER], "none");

        let mut same = Response::new(Body::empty());
        Note::Served {
            served: ReasoningLevel::High,
            requested: Some(ReasoningLevel::High),
        }
        .apply_headers(&mut same);
        assert!(same.headers().get(REQUESTED_HEADER).is_none());

        assert_eq!(
            Note::of(&ReasoningPlan::Strip { requested: None }),
            None,
            "nothing to say when the caller named nothing"
        );
        assert_eq!(
            Note::of(&ReasoningPlan::Strip {
                requested: Some(ReasoningLevel::Low)
            }),
            Some(Note::Unsupported)
        );
    }

    #[test]
    fn a_family_reports_the_level_it_served() {
        let strip = ReasoningPlan::Strip {
            requested: Some(ReasoningLevel::High),
        };
        assert_eq!(
            Note::for_family(ReasoningLevel::High, Some(ReasoningLevel::High), &strip),
            Note::Served {
                served: ReasoningLevel::High,
                requested: Some(ReasoningLevel::High)
            }
        );
        // No level named: the family's default is what was served.
        assert_eq!(
            Note::for_family(ReasoningLevel::None, None, &ReasoningPlan::Untouched),
            Note::Served {
                served: ReasoningLevel::None,
                requested: None
            }
        );
        // A member that serves the level as another says so.
        let used = ReasoningPlan::Apply {
            requested: Some(ReasoningLevel::None),
            served: ReasoningLevel::Low,
            send: Default::default(),
        };
        assert_eq!(
            Note::for_family(ReasoningLevel::None, Some(ReasoningLevel::None), &used),
            Note::Served {
                served: ReasoningLevel::Low,
                requested: Some(ReasoningLevel::None)
            }
        );
    }

    #[test]
    fn shims_forward_the_canonical_spelling_or_the_raw_value() {
        assert_eq!(
            header_value(Ok(Some(ReasoningLevel::High))).unwrap(),
            "high"
        );
        assert!(header_value(Ok(None)).is_none());
        assert_eq!(header_value(Err("turbo".into())).unwrap(), "turbo");
    }
}

/// The whole pipeline against a fake chat upstream: what each model's server
/// receives for each level, and what the caller is told. Needs the test
/// datastores (`OBLETH_TEST_DATABASE_URL`, `OBLETH_TEST_REDIS_URL`); skipped
/// without them.
#[cfg(test)]
mod pipeline {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::extract::State;
    use axum::http::{header, Method, Request, StatusCode};
    use axum::routing::post;
    use axum::{Json, Router};
    use moka::future::Cache;
    use obleth_config::{
        FamilyMembers, ModelFamily, ModelReasoning, ReasoningAction, ReasoningLevel, ReasoningMode,
        ResolvedModel,
    };
    use serde_json::{json, Value};
    use uuid::Uuid;

    use super::{REQUESTED_HEADER, SERVED_HEADER};
    use crate::state::AppState;

    type Seen = Arc<Mutex<Vec<Value>>>;

    async fn spawn_upstream() -> (Seen, String) {
        let seen: Seen = Arc::default();
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(|State(seen): State<Seen>, Json(body): Json<Value>| async move {
                    let model = body["model"].clone();
                    seen.lock().unwrap().push(body);
                    Json(json!({
                        "id": "c1",
                        "object": "chat.completion",
                        "model": model,
                        "choices": [{
                            "index": 0,
                            "message": {"role": "assistant", "content": "ok"},
                            "finish_reason": "stop",
                        }],
                        "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4},
                    }))
                }),
            )
            .with_state(seen.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (seen, format!("http://{addr}/v1"))
    }

    struct Gateway {
        state: AppState,
        wal_dir: std::path::PathBuf,
    }

    impl Drop for Gateway {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.wal_dir);
        }
    }

    impl Gateway {
        async fn register(&self, route: ResolvedModel) {
            self.state
                .model_cache
                .insert(route.model_name.clone(), Arc::new(route))
                .await;
        }

        async fn key(&self) -> String {
            let mut key = crate::boons::test_support::test_key();
            key.key_id = Uuid::new_v4();
            key.tenant_id = Uuid::new_v4();
            let secret = format!("sk-effort-{}", Uuid::new_v4());
            self.state
                .key_cache
                .insert(obleth_config::hash_api_key(&secret), Arc::new(key))
                .await;
            secret
        }

        async fn post(&self, uri: &str, secret: &str, body: Value) -> axum::response::Response {
            let req = Request::builder()
                .method(Method::POST)
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap();
            crate::proxy::proxy_handler(State(self.state.clone()), req).await
        }
    }

    async fn gateway() -> Option<Gateway> {
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
        let wal_dir = std::env::temp_dir().join(format!("obleth-effort-wal-{}", Uuid::new_v4()));
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
        let http = reqwest::Client::new();
        let state = AppState {
            redis,
            fairshare: obleth_fairshare::FairShare::start(
                Arc::new(obleth_fairshare::StaticCapacity::new(64)),
                obleth_config::FairshareAlgorithm::default(),
                64,
            ),
            tokenizer: Arc::new(obleth_tokenizer::HeuristicTokenizer::new()),
            telemetry,
            http: http.clone(),
            upstream_base: "http://127.0.0.1:1/v1".into(),
            upstream_timeout: Duration::from_secs(30),
            key_cache: Cache::builder().build(),
            model_cache: Cache::builder().build(),
            mcp_cache: Cache::builder().build(),
            model_registry: crate::router::ModelRegistry::new(),
            families: super::FamilyRegistry::new(),
            classifier: crate::classifier::Classifier::new(Default::default()),
            output_stats: Default::default(),
            boons: crate::boons::BoonEngine::new(Default::default()),
            tool_cache: Cache::builder().build(),
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
            load: None,
        };
        Some(Gateway { state, wal_dir })
    }

    fn route(name: &str, base: &str, reasoning: ModelReasoning) -> ResolvedModel {
        let mut r = crate::boons::test_support::test_route();
        r.model_name = name.into();
        r.upstream_model = format!("{name}-upstream");
        r.api_base = base.into();
        r.reasoning = reasoning;
        r
    }

    fn send(v: Value) -> ReasoningAction {
        ReasoningAction::Send(v.as_object().unwrap().clone())
    }

    /// low/high only, no off switch: `none` serves as low.
    fn two_level_map() -> ModelReasoning {
        ModelReasoning {
            mode: ReasoningMode::Mapped,
            default_level: None,
            levels: [
                (
                    ReasoningLevel::None,
                    ReasoningAction::Use(ReasoningLevel::Low),
                ),
                (
                    ReasoningLevel::Low,
                    send(json!({"reasoning_effort": "low"})),
                ),
                (ReasoningLevel::Medium, send(json!({}))),
                (
                    ReasoningLevel::High,
                    send(json!({"reasoning_effort": "max"})),
                ),
            ]
            .into_iter()
            .collect(),
        }
    }

    fn chat(model: &str) -> Value {
        json!({"model": model, "messages": [{"role": "user", "content": "hi"}]})
    }

    #[tokio::test]
    async fn a_mapped_model_receives_its_own_fields_and_the_caller_is_told() {
        let Some(gw) = gateway().await else { return };
        let (seen, base) = spawn_upstream().await;
        gw.register(route("two-level", &base, two_level_map()))
            .await;
        let secret = gw.key().await;

        let mut body = chat("two-level");
        body["reasoning_effort"] = json!("none");
        let resp = gw.post("/v1/chat/completions", &secret, body).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[SERVED_HEADER], "low");
        assert_eq!(resp.headers()[REQUESTED_HEADER], "none");
        let sent = seen.lock().unwrap().pop().unwrap();
        assert_eq!(sent["reasoning_effort"], "low");

        let mut body = chat("two-level");
        body["reasoning_effort"] = json!("medium");
        let resp = gw.post("/v1/chat/completions", &secret, body).await;
        assert_eq!(resp.headers()[SERVED_HEADER], "medium");
        let sent = seen.lock().unwrap().pop().unwrap();
        assert!(
            sent.get("reasoning_effort").is_none(),
            "medium sends nothing"
        );

        let mut body = chat("two-level");
        body["reasoning_effort"] = json!("turbo");
        let resp = gw.post("/v1/chat/completions", &secret, body).await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn an_unsupported_model_never_sees_the_effort() {
        let Some(gw) = gateway().await else { return };
        let (seen, base) = spawn_upstream().await;
        let unsupported = ModelReasoning {
            mode: ReasoningMode::Unsupported,
            ..Default::default()
        };
        gw.register(route("plain", &base, unsupported)).await;
        let secret = gw.key().await;

        let mut body = chat("plain");
        body["reasoning_effort"] = json!("high");
        let resp = gw.post("/v1/chat/completions", &secret, body).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[SERVED_HEADER], "unsupported");
        assert!(seen
            .lock()
            .unwrap()
            .pop()
            .unwrap()
            .get("reasoning_effort")
            .is_none());
    }

    #[tokio::test]
    async fn a_passthrough_model_gets_the_request_as_sent() {
        let Some(gw) = gateway().await else { return };
        let (seen, base) = spawn_upstream().await;
        gw.register(route("as-is", &base, ModelReasoning::default()))
            .await;
        let secret = gw.key().await;

        let mut body = chat("as-is");
        body["reasoning_effort"] = json!("xhigh");
        let resp = gw.post("/v1/chat/completions", &secret, body).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(SERVED_HEADER).is_none());
        assert_eq!(
            seen.lock().unwrap().pop().unwrap()["reasoning_effort"],
            "xhigh"
        );
    }

    #[tokio::test]
    async fn a_family_name_is_served_by_the_member_its_level_picks() {
        let Some(gw) = gateway().await else { return };
        let (seen, base) = spawn_upstream().await;
        let thinking_on_off = ModelReasoning {
            mode: ReasoningMode::Unsupported,
            ..Default::default()
        };
        gw.register(route("acme-instruct", &base, thinking_on_off.clone()))
            .await;
        gw.register(route("acme-think", &base, thinking_on_off))
            .await;
        gw.state.families.store(vec![ModelFamily {
            id: Uuid::new_v4(),
            name: "acme".into(),
            description: String::new(),
            default_level: ReasoningLevel::None,
            members: FamilyMembers {
                none: "acme-instruct".into(),
                low: "acme-think".into(),
                medium: "acme-think".into(),
                high: "acme-think".into(),
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }]);
        let secret = gw.key().await;

        let resp = gw.post("/v1/chat/completions", &secret, chat("acme")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[crate::proxy::MODEL_VARIANT_HEADER], "acme");
        assert_eq!(resp.headers()[SERVED_HEADER], "none", "the family default");
        assert_eq!(
            seen.lock().unwrap().pop().unwrap()["model"],
            "acme-instruct-upstream"
        );

        let mut body = chat("acme");
        body["reasoning_effort"] = json!("high");
        let resp = gw.post("/v1/chat/completions", &secret, body).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let sent = seen.lock().unwrap().pop().unwrap();
        assert_eq!(sent["model"], "acme-think-upstream");
        assert!(
            sent.get("reasoning_effort").is_none(),
            "the member's map still applies"
        );

        // The Messages surface's `thinking` picks the member too.
        let resp = gw
            .post(
                "/v1/messages",
                &secret,
                json!({
                    "model": "acme",
                    "max_tokens": 64,
                    "messages": [{"role": "user", "content": "hi"}],
                    "thinking": {"type": "enabled", "budget_tokens": 2048},
                }),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(
            seen.lock().unwrap().pop().unwrap()["model"],
            "acme-think-upstream"
        );
    }

    #[tokio::test]
    async fn the_responses_surface_effort_is_translated_per_model() {
        let Some(gw) = gateway().await else { return };
        let (seen, base) = spawn_upstream().await;
        gw.register(route("two-level", &base, two_level_map()))
            .await;
        let secret = gw.key().await;

        let resp = gw
            .post(
                "/v1/responses",
                &secret,
                json!({"model": "two-level", "input": "hi", "reasoning": {"effort": "high"}}),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[SERVED_HEADER], "high");
        assert_eq!(
            seen.lock().unwrap().pop().unwrap()["reasoning_effort"],
            "max"
        );
    }

    #[tokio::test]
    async fn a_caller_cannot_forge_the_shims_header() {
        let Some(gw) = gateway().await else { return };
        let (seen, base) = spawn_upstream().await;
        gw.register(route("two-level", &base, two_level_map()))
            .await;
        let secret = gw.key().await;

        let req = Request::builder()
            .method(Method::POST)
            .uri("/v1/chat/completions")
            .header(header::AUTHORIZATION, format!("Bearer {secret}"))
            .header(header::CONTENT_TYPE, "application/json")
            .header(super::EFFORT_HEADER, "high")
            .body(axum::body::Body::from(chat("two-level").to_string()))
            .unwrap();
        let resp = crate::proxy::proxy_handler(State(gw.state.clone()), req).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert!(resp.headers().get(SERVED_HEADER).is_none());
        assert!(seen
            .lock()
            .unwrap()
            .pop()
            .unwrap()
            .get("reasoning_effort")
            .is_none());
    }
}
