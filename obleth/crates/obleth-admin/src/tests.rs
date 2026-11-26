//! Router-level tests of the admin API against live datastores, and the
//! helpers other test modules share.

use super::*;
use harness::{
    fixture_model, send, simulate_request, test_admin_app, test_admin_app_discovering,
    test_admin_app_on, test_admin_app_on_clickhouse, test_admin_app_with_discovery,
    TEST_ADMIN_TOKEN,
};

/// Live-service plumbing for the handful of admin tests that need a real
/// router. Everything here self-skips when the integration datastores are
/// not configured, so a plain `cargo test` stays hermetic.
mod harness {
    use super::*;
    use tower::ServiceExt;

    pub(super) const TEST_ADMIN_TOKEN: &str = "test-admin-token";

    /// Serialises the DB-backed tests in this crate, mirroring
    /// `obleth-store`'s `serial()`. These tests share one Postgres
    /// database, and some of what a handler reads is *global*, not scoped
    /// to a test's own fixtures: `build_candidates` derives cost-rank tier
    /// levels across every model in the database, so a sibling test
    /// creating or deleting a fixture model mid-test shifts the ladder and
    /// changes another test's `level` values between two calls (exactly
    /// how `simulate_pins_the_draw_so_two_runs_are_comparable` flaked in
    /// CI). Held via `TestApp` so every DB-backed test carries it for its
    /// full duration and a new test cannot forget to take it.
    static SERIAL: std::sync::OnceLock<tokio::sync::Mutex<()>> = std::sync::OnceLock::new();
    fn serial() -> &'static tokio::sync::Mutex<()> {
        SERIAL.get_or_init(tokio::sync::Mutex::default)
    }

    /// Reads `OBLETH_TEST_DATABASE_URL`, mirroring the guard in
    /// `obleth-store`'s harness: refuses any database whose name does not
    /// contain "test", so a misconfigured env can never point these
    /// fixtures at a real or dev database.
    pub(super) fn test_db_url() -> Option<String> {
        let url = std::env::var("OBLETH_TEST_DATABASE_URL").ok()?;
        let db = url
            .rsplit('/')
            .next()
            .unwrap_or("")
            .split('?')
            .next()
            .unwrap_or("");
        assert!(
            db.contains("test"),
            "OBLETH_TEST_DATABASE_URL database name {db:?} is not a dedicated test DB \
             (name must contain \"test\", e.g. obleth_test). Refusing to run integration \
             tests against a possibly-real database."
        );
        Some(url)
    }

    /// A fixture model with sane defaults. Caller owns the returned row and
    /// must delete it.
    pub(super) async fn fixture_model(
        store: &Store,
        name: &str,
        input_cost_per_token: f64,
    ) -> ModelRoute {
        store
            .create_model(&obleth_store::NewModel::new(
                name,
                obleth_store::ModelFields {
                    upstream_model: name.into(),
                    api_base: "http://upstream.invalid".into(),
                    model_type: obleth_config::DEFAULT_MODEL_TYPE.into(),
                    input_cost_per_token,
                    context_window: 128_000,
                    admission_weight: 100,
                    max_in_flight: Some(4),
                    supports_function_calling: true,
                    supports_system_messages: true,
                    supports_response_schema: true,
                    supports_tool_choice: true,
                    tags: vec!["coding".into()],
                    route_bias: 1.0,
                    auto_eligible: true,
                    capacity_mode: "static".into(),
                    ..Default::default()
                },
            ))
            .await
            .expect("create fixture model")
    }

    /// The real `/api/v1` router plus the handles a test may need to set up
    /// live state the handlers read.
    pub(super) struct TestApp {
        pub(super) app: Router,
        pub(super) store: Store,
        /// Same instance the router's `AdminState` holds, so admitting a
        /// request here is visible to a handler as real fleet load.
        pub(super) fairshare: FairShare,
        /// Same ring the router's `AdminState` holds, so a test can seed
        /// samples and read them back through the handler.
        pub(super) fairshare_history: Arc<FairshareHistory>,
        pub(super) redis: RedisStore,
        /// Keeps this test's exclusive claim on the shared test database
        /// alive for the test's full duration (see `serial()`).
        _serial: tokio::sync::MutexGuard<'static, ()>,
    }

    /// The real `/api/v1` router, wired to the integration datastores.
    /// Returns `None` (test skips) when either is unconfigured.
    pub(super) async fn test_admin_app() -> Option<TestApp> {
        let redis_url = std::env::var("OBLETH_TEST_REDIS_URL").ok()?;
        test_admin_app_on(&redis_url).await
    }

    /// [`test_admin_app`] against an explicit Redis URL (e.g. a
    /// restricted ACL user, to inject cache failures).
    pub(super) async fn test_admin_app_on(redis_url: &str) -> Option<TestApp> {
        test_admin_app_with(redis_url, capacity_discovery::CapacityDiscovery::disabled()).await
    }

    /// [`test_admin_app`] with the gateway's capacity discovery settings.
    pub(super) async fn test_admin_app_discovering(
        discovery: obleth_config::CapacityDiscoveryConfig,
    ) -> Option<TestApp> {
        let redis_url = std::env::var("OBLETH_TEST_REDIS_URL").ok()?;
        test_admin_app_with(
            &redis_url,
            capacity_discovery::CapacityDiscovery::new(discovery),
        )
        .await
    }

    /// [`test_admin_app`] with a given capacity discovery handle.
    pub(super) async fn test_admin_app_with_discovery(
        discovery: capacity_discovery::CapacityDiscovery,
    ) -> Option<TestApp> {
        let redis_url = std::env::var("OBLETH_TEST_REDIS_URL").ok()?;
        test_admin_app_with(&redis_url, discovery).await
    }

    async fn test_admin_app_with(
        redis_url: &str,
        capacity_discovery: capacity_discovery::CapacityDiscovery,
    ) -> Option<TestApp> {
        // Never dialled: no route under test reads ClickHouse.
        test_admin_app_full(redis_url, capacity_discovery, clickhouse::Client::default()).await
    }

    /// [`test_admin_app`] reading usage from this ClickHouse.
    pub(super) async fn test_admin_app_on_clickhouse(
        clickhouse: clickhouse::Client,
    ) -> Option<TestApp> {
        let redis_url = std::env::var("OBLETH_TEST_REDIS_URL").ok()?;
        test_admin_app_full(
            &redis_url,
            capacity_discovery::CapacityDiscovery::disabled(),
            clickhouse,
        )
        .await
    }

    async fn test_admin_app_full(
        redis_url: &str,
        capacity_discovery: capacity_discovery::CapacityDiscovery,
        clickhouse: clickhouse::Client,
    ) -> Option<TestApp> {
        let db_url = test_db_url()?;

        // Taken before the first database touch (`migrate()` runs DDL) and
        // held until the test drops its `TestApp`.
        let guard = serial().lock().await;
        let store = Store::connect(&db_url).await.expect("connect postgres");
        store.migrate().await.expect("migrate");
        let redis = RedisStore::connect(redis_url).await.expect("connect redis");

        let state = test_state(store.clone(), redis.clone(), capacity_discovery, clickhouse);
        Some(TestApp {
            fairshare: state.fairshare.clone(),
            fairshare_history: state.fairshare_history.clone(),
            app: router(state),
            store,
            redis,
            _serial: guard,
        })
    }

    impl TestApp {
        /// A second gateway replica: its own in-process state (capacity,
        /// fairshare, alert targets) over the same Postgres and Redis.
        pub(super) fn replica(&self) -> AdminState {
            test_state(
                self.store.clone(),
                self.redis.clone(),
                capacity_discovery::CapacityDiscovery::disabled(),
                clickhouse::Client::default(),
            )
        }
    }

    /// One replica's `AdminState` over the given datastores.
    fn test_state(
        store: Store,
        redis: RedisStore,
        capacity_discovery: capacity_discovery::CapacityDiscovery,
        clickhouse: clickhouse::Client,
    ) -> AdminState {
        let capacity = Arc::new(obleth_fairshare::StaticCapacity::new(64));
        let fairshare = FairShare::start(
            capacity.clone(),
            obleth_config::FairshareAlgorithm::default(),
            32,
        );
        let http = reqwest::Client::new();
        let alerts = AlertDispatcher::new(http.clone(), AlertSettings::default());
        AdminState {
            store,
            redis,
            ceiling: Arc::new(replica_settings::GlobalCeiling::new(capacity, 64)),
            fairshare: fairshare.clone(),
            fairshare_stats: fairshare.stats(),
            default_model_max_in_flight: 32,
            fairshare_history: Arc::new(FairshareHistory::new(1800)),
            fairshare_history_secs: 3600,
            fairshare_replica_aware: true,
            fairshare_shared_slots: false,
            clickhouse,
            admin_token: TEST_ADMIN_TOKEN.to_string(),
            output_stats: Default::default(),
            classify: None,
            capacity_discovery,
            judge: None,
            live_catalog: Default::default(),
            usage_cache: Default::default(),
            slurm_http: Default::default(),
            client_address: Default::default(),
            compressor: Default::default(),
            health: ModelHealthRuntime {
                scheduled_enabled: false,
                default_interval_secs: 60,
                timeout_secs: 5,
                retention_days: 1,
                http,
                alerts: None,
                telemetry: None,
                catalogs: Default::default(),
            },
            usage_retention_default_days: 30,
            ssrf: ssrf::SsrfPolicy::default(),
            alerts,
            local_cache_tx: None,
        }
    }

    pub(super) async fn send(
        app: &Router,
        req: axum::http::Request<axum::body::Body>,
    ) -> (StatusCode, serde_json::Value) {
        let res = app.clone().oneshot(req).await.expect("handler ran");
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .expect("read body");
        let json = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    pub(super) fn simulate_request(
        token: Option<&str>,
        body: serde_json::Value,
    ) -> axum::http::Request<axum::body::Body> {
        let mut req = axum::http::Request::post("/api/v1/router/simulate")
            .header("content-type", "application/json");
        if let Some(token) = token {
            req = req.header("authorization", format!("Bearer {token}"));
        }
        req.body(axum::body::Body::from(body.to_string()))
            .expect("build request")
    }
}

fn json_request(
    method: &str,
    path: &str,
    body: serde_json::Value,
) -> axum::http::Request<axum::body::Body> {
    axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::from(body.to_string()))
        .expect("build request")
}

/// Out-of-range or unknown values on the admin write path are 400 with
/// the field named, never coerced to a default or kept silently as the
/// old value.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn bad_admin_input_is_400_not_coerced() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let name = format!("m-{}", Uuid::new_v4());
    let (status, body) = send(
        &t.app,
        json_request("POST", "/api/v1/models", serde_json::json!({
            "model_name": name, "upstream_model": "u", "api_base": "http://127.0.0.1:1/v1", "model_type": "embeddings"
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("model_type"),
        "{body}"
    );
    let (status, body) = send(
        &t.app,
        json_request(
            "POST",
            "/api/v1/tenants",
            serde_json::json!({ "name": format!("t-{}", Uuid::new_v4()), "weight": 0 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = send(
        &t.app,
        json_request(
            "POST",
            "/api/v1/fairshare/groups",
            serde_json::json!({ "name": format!("g-{}", Uuid::new_v4()), "weight": -3 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    // A bad threshold is refused and the stored value stays as it was.
    let (_, before) = send(
        &t.app,
        json_request("GET", "/api/v1/settings/boons", serde_json::Value::Null),
    )
    .await;
    let (status, body) = send(
        &t.app,
        json_request(
            "PUT",
            "/api/v1/settings/boons",
            serde_json::json!({ "speculation_agree_min": 1.5 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body["error"]
            .as_str()
            .unwrap_or_default()
            .contains("speculation_agree_min"),
        "{body}"
    );
    let (_, after) = send(
        &t.app,
        json_request("GET", "/api/v1/settings/boons", serde_json::Value::Null),
    )
    .await;
    assert_eq!(
        after["speculation_agree_min"],
        before["speculation_agree_min"]
    );
    let (status, body) = send(
        &t.app,
        json_request(
            "PUT",
            "/api/v1/settings/energy",
            serde_json::json!({ "pue": 0.5 }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
}

/// An `update_model` audit row lists the fields the update changed, with
/// their values before and after, and nothing it left alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_model_audit_lists_exactly_the_changed_fields() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let model = fixture_model(&t.store, &format!("audit-diff-{}", Uuid::new_v4()), 0.0).await;
    let path = format!("/api/v1/models/{}", model.id);
    let base = serde_json::json!({
        "upstream_model": model.upstream_model,
        "api_base": "http://127.0.0.1:1/v1",
    });
    let (status, body) = send(&t.app, json_request("PUT", &path, base.clone())).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let mut change = base.clone();
    change.as_object_mut().unwrap().extend([
        (
            "description".to_string(),
            serde_json::json!("now described"),
        ),
        ("max_in_flight".to_string(), serde_json::json!(9)),
        ("tags".to_string(), serde_json::json!(["coding"])),
    ]);
    let (status, body) = send(&t.app, json_request("PUT", &path, change)).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let rows = t
        .store
        .list_audit_filtered(&obleth_store::AuditFilter {
            entity_id: Some(model.id.to_string()),
            limit: 10,
            ..Default::default()
        })
        .await
        .unwrap();
    let latest = rows
        .iter()
        .find(|r| r.action == "update_model")
        .expect("an update_model row");
    assert_eq!(
        latest.detail["changes"],
        serde_json::json!({
            "description": { "before": "", "after": "now described" },
            "max_in_flight": { "before": 4, "after": 9 },
        }),
    );
    assert_eq!(latest.detail["model_name"], model.model_name.as_str());
    let _ = t.store.delete_model(model.id).await;
}

/// Recipes follow the admin API's conventions: a delete answers 204 and
/// 404 when nothing was deleted; a PUT changes an existing recipe and
/// 404s for a missing id instead of creating one.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn recipes_delete_with_204_and_404_and_put_never_creates() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let missing = Uuid::new_v4();
    let body = serde_json::json!({"name": "greeter", "body": "spec", "author": "ana"});
    let (status, _) = send(
        &t.app,
        json_request("PUT", &format!("/api/v1/recipes/{missing}"), body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &t.app,
        json_request(
            "DELETE",
            &format!("/api/v1/recipes/{missing}"),
            serde_json::Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, made) = send(
        &t.app,
        json_request("POST", "/api/v1/recipes", body.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = made["id"].as_str().expect("id").to_string();
    let (status, changed) = send(
        &t.app,
        json_request(
            "PUT",
            &format!("/api/v1/recipes/{id}"),
            serde_json::json!({"name": "greeter-2"}),
        ),
    )
    .await;
    assert_eq!(
        (status, changed["name"].as_str()),
        (StatusCode::OK, Some("greeter-2"))
    );
    let (status, _) = send(
        &t.app,
        json_request(
            "DELETE",
            &format!("/api/v1/recipes/{id}"),
            serde_json::Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _) = send(
        &t.app,
        json_request(
            "DELETE",
            &format!("/api/v1/recipes/{id}"),
            serde_json::Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

/// A key's and a tenant's on/off settings answer 200 with the updated row,
/// a missing id is 404, and the old `PATCH` verb on a tenant setter is
/// still served.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn toggles_answer_the_updated_row_and_a_missing_id_is_404() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let name = format!("toggles-{}", Uuid::new_v4());
    let (_, tenant) = send(
        &t.app,
        json_request("POST", "/api/v1/tenants", serde_json::json!({"name": name})),
    )
    .await;
    let tid = tenant["id"].as_str().expect("tenant id").to_string();
    let (_, created) = send(
        &t.app,
        json_request(
            "POST",
            &format!("/api/v1/tenants/{tid}/keys"),
            serde_json::json!({"name": "k"}),
        ),
    )
    .await;
    let kid = created["key"]["id"].as_str().expect("key id").to_string();
    let missing = Uuid::new_v4();
    for (route, field) in [
        ("disabled", "disabled"),
        ("tracing", "tracing_enabled"),
        ("end-user-fairshare", "end_user_fairshare"),
        ("background", "background"),
    ] {
        for on in [true, false] {
            let (status, key) = send(
                &t.app,
                json_request(
                    "PUT",
                    &format!("/api/v1/keys/{kid}/{route}"),
                    serde_json::json!({ field: on }),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::OK, "{route}");
            assert_eq!(
                (key["id"].as_str(), &key[field]),
                (Some(kid.as_str()), &serde_json::json!(on)),
                "{route}"
            );
        }
        let (status, _) = send(
            &t.app,
            json_request(
                "PUT",
                &format!("/api/v1/keys/{missing}/{route}"),
                serde_json::json!({ field: true }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}");
    }
    for (route, field) in [("tracing", "tracing_enabled"), ("synthetic", "synthetic")] {
        let (status, row) = send(
            &t.app,
            json_request(
                "PUT",
                &format!("/api/v1/tenants/{tid}/{route}"),
                serde_json::json!({ field: true }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{route}");
        assert_eq!(
            (row["id"].as_str(), &row[field]),
            (Some(tid.as_str()), &serde_json::json!(true)),
            "{route}"
        );
        let (status, _) = send(
            &t.app,
            json_request(
                "PUT",
                &format!("/api/v1/tenants/{missing}/{route}"),
                serde_json::json!({ field: true }),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{route}");
    }
    for method in ["PUT", "PATCH"] {
        let (status, row) = send(
            &t.app,
            json_request(
                method,
                &format!("/api/v1/tenants/{tid}/weight"),
                serde_json::json!({"weight": 250}),
            ),
        )
        .await;
        assert_eq!(
            (status, row["weight"].as_i64()),
            (StatusCode::OK, Some(250)),
            "{method}"
        );
    }
    let (status, _) = send(
        &t.app,
        json_request(
            "DELETE",
            &format!("/api/v1/tenants/{tid}"),
            serde_json::Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
}

/// Deleting something that does not exist is 404 on every delete route.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn deleting_a_missing_id_is_404() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let m = Uuid::new_v4();
    let mut wrong = Vec::new();
    for path in [
        format!("/api/v1/tenants/{m}"),
        format!("/api/v1/keys/{m}"),
        format!("/api/v1/models/{m}"),
        format!("/api/v1/model-families/{m}"),
        format!("/api/v1/models/{m}/endpoints/{m}"),
        format!("/api/v1/models/{m}/managed"),
        format!("/api/v1/replicas/{m}"),
        format!("/api/v1/mcp-servers/{m}"),
        format!("/api/v1/recipes/{m}"),
        format!("/api/v1/knowledge/collections/{m}"),
        format!("/api/v1/knowledge/documents/{m}"),
    ] {
        let (status, _) = send(
            &t.app,
            json_request("DELETE", &path, serde_json::Value::Null),
        )
        .await;
        if status != StatusCode::NOT_FOUND {
            wrong.push(format!("DELETE {path}: {status}"));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Every model's endpoints come back in one read, each naming its model,
/// with the key kept write-only.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn all_endpoints_are_read_at_once() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let a = harness::fixture_model(&t.store, &format!("all-eps-a-{}", Uuid::new_v4()), 0.0).await;
    let b = harness::fixture_model(&t.store, &format!("all-eps-b-{}", Uuid::new_v4()), 0.0).await;
    for (model, name, key) in [
        (a.id, "a1", Some("sk-secret")),
        (a.id, "a2", None),
        (b.id, "b1", None),
    ] {
        t.store
            .create_model_endpoint(
                model,
                name,
                "http://127.0.0.1:9/v1",
                key,
                100,
                100,
                true,
                None,
            )
            .await
            .expect("endpoint");
    }
    let req = axum::http::Request::get("/api/v1/endpoints")
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let (status, body) = send(&t.app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let names_of = |m: Uuid| {
        let mut names: Vec<String> = body
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["model_id"] == m.to_string())
            .map(|e| e["name"].as_str().unwrap().to_string())
            .collect();
        names.sort();
        names
    };
    assert_eq!(names_of(a.id), ["a1", "a2"]);
    assert_eq!(names_of(b.id), ["b1"]);
    let a1 = body
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "a1" && e["model_id"] == a.id.to_string())
        .unwrap();
    assert_eq!(a1["api_key_set"], true);
    assert!(
        !body.to_string().contains("sk-secret"),
        "the key is write-only"
    );
}

/// A model's endpoints are probed together: four endpoints that each take
/// 400 ms answer in about that long, not 1.6 s, and every one has its
/// result recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_models_endpoints_are_probed_concurrently() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let upstream = axum::Router::new().route(
        "/v1/embeddings",
        axum::routing::post(|| async {
            tokio::time::sleep(std::time::Duration::from_millis(400)).await;
            Json(serde_json::json!({"data": [{"index": 0, "embedding": [1.0, 0.0]}]}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let name = format!("probe-pool-{}", Uuid::new_v4());
    let (status, created) = send(
        &t.app,
        json_request(
            "POST",
            "/api/v1/models",
            serde_json::json!({
                "model_name": name,
                "upstream_model": "bge-m3",
                "api_base": "",
                "model_type": "embedding",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let model_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    for i in 0..4 {
        t.store
            .create_model_endpoint(
                model_id,
                &format!("e{i}"),
                &format!("http://{addr}/v1"),
                None,
                i,
                100,
                true,
                None,
            )
            .await
            .expect("endpoint");
    }

    let started = std::time::Instant::now();
    let (status, body) = send(
        &t.app,
        json_request(
            "POST",
            &format!("/api/v1/models/{model_id}/health/check"),
            serde_json::json!({}),
        ),
    )
    .await;
    let elapsed = started.elapsed();
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        elapsed < std::time::Duration::from_millis(1200),
        "four 400 ms probes took {elapsed:?}; serial would be 1.6 s"
    );
    let endpoints = t.store.list_model_endpoints(model_id).await.expect("list");
    assert_eq!(endpoints.len(), 4);
    assert!(
        endpoints.iter().all(|e| e.last_checked_at.is_some()),
        "every endpoint has its probe recorded: {endpoints:?}"
    );
    t.store.delete_model(model_id).await.ok();
}

/// Export takes a names filter and reads every endpoint in one go; the
/// exported file imports back as unchanged.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn export_filters_by_name_and_round_trips_unchanged() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let wanted = fixture_model(&t.store, &format!("export-a-{}", Uuid::new_v4()), 0.0).await;
    let other = fixture_model(&t.store, &format!("export-b-{}", Uuid::new_v4()), 0.0).await;
    // The import re-checks destinations, and the fixture's host does not
    // resolve; a private address passes the test gateway's policy.
    sqlx::query("update models set api_base = 'http://10.0.0.1/v1' where id = $1")
        .bind(wanted.id)
        .execute(t.store.pool())
        .await
        .expect("point at a resolvable address");
    t.store
        .create_model_endpoint(
            wanted.id,
            "east",
            "http://10.0.0.2/v1",
            None,
            0,
            100,
            true,
            Some(8),
        )
        .await
        .expect("endpoint");
    let get = |path: String| {
        axum::http::Request::get(path)
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::empty())
            .expect("build request")
    };

    let (status, manifest) = send(
        &t.app,
        get(format!(
            "/api/v1/models/export?names={},no-such-model",
            wanted.model_name
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{manifest}");
    let models = manifest["models"].as_array().expect("models");
    assert_eq!(models.len(), 1, "{manifest}");
    assert_eq!(
        models[0]["model_name"],
        serde_json::json!(wanted.model_name)
    );
    assert_eq!(models[0]["endpoints"][0]["name"], serde_json::json!("east"));
    assert_eq!(
        models[0]["endpoints"][0]["max_in_flight"],
        serde_json::json!(8)
    );

    let (status, all) = send(&t.app, get("/api/v1/models/export".into())).await;
    assert_eq!(status, StatusCode::OK, "{all}");
    assert!(all["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["model_name"] == serde_json::json!(other.model_name)));

    let (status, report) = send(
        &t.app,
        json_request("POST", "/api/v1/models/import?dry_run=true", manifest),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{report}");
    assert_eq!(report["unchanged"], serde_json::json!(1), "{report}");
    assert_eq!(report["updated"], serde_json::json!(0), "{report}");
    assert_eq!(report["created"], serde_json::json!(0), "{report}");

    t.store.delete_model(wanted.id).await.ok();
    t.store.delete_model(other.id).await.ok();
}

/// A Slurm-managed embedder has a blank `api_base` and lives only in its
/// endpoints. Indexing and the search preview must reach the healthy one
/// with its key, like the data plane does, not fail on the blank base.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn indexing_and_search_reach_an_embedder_through_its_healthy_endpoint() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let auth = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen = auth.clone();
    let upstream = axum::Router::new().route(
        "/v1/embeddings",
        axum::routing::post(
            move |headers: HeaderMap, Json(body): Json<serde_json::Value>| {
                let seen = seen.clone();
                async move {
                    seen.lock()
                        .unwrap()
                        .push(headers["authorization"].to_str().unwrap().to_string());
                    let data: Vec<_> = (0..body["input"].as_array().unwrap().len())
                        .map(|index| serde_json::json!({"index": index, "embedding": [1.0, 0.0]}))
                        .collect();
                    Json(serde_json::json!({"data": data}))
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, upstream).await.unwrap() });

    let name = format!("slurm-embedder-{}", Uuid::new_v4());
    let (status, created) = send(
        &t.app,
        json_request(
            "POST",
            "/api/v1/models",
            serde_json::json!({
                "model_name": name,
                "upstream_model": "bge-m3",
                "api_base": "",
                "model_type": "embedding",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let model_id: Uuid = created["id"].as_str().unwrap().parse().unwrap();
    // Lower priority wins, so only the health check keeps this one out.
    let down = t
        .store
        .create_model_endpoint(
            model_id,
            "down",
            "http://upstream.invalid/v1",
            Some("sk-down"),
            0,
            100,
            true,
            None,
        )
        .await
        .expect("endpoint");
    t.store
        .record_endpoint_health(down.id, "unhealthy", None, None, None)
        .await
        .expect("health");
    t.store
        .create_model_endpoint(
            model_id,
            "up",
            &format!("http://{addr}/v1"),
            Some("sk-up"),
            1,
            100,
            true,
            None,
        )
        .await
        .expect("endpoint");

    let collection = t
        .store
        .create_collection(&format!("slurm-{}", Uuid::new_v4()), "", &name)
        .await
        .expect("collection");
    let doc = t
        .store
        .upsert_document(collection.id, "t", "t.txt", "text/plain", "hello world")
        .await
        .expect("document");
    // Claimed by hand: `claim_pending_document` could pick up a sibling
    // test's leftover document, and only a claimed one can turn `ready`.
    sqlx::query("update knowledge_documents set status = 'indexing' where id = $1")
        .bind(doc.id)
        .execute(t.store.pool())
        .await
        .expect("claim");

    knowledge::indexer::index_document(
        &t.store,
        &reqwest::Client::new(),
        &doc,
        "hello world",
        &obleth_config::BoonSettings::default().knowledge,
    )
    .await
    .expect("index through the endpoint");
    let docs = t.store.list_documents(collection.id).await.expect("list");
    assert_eq!(docs[0].status, "ready");

    let (status, hits) = send(
        &t.app,
        json_request(
            "POST",
            &format!("/api/v1/knowledge/collections/{}/search", collection.id),
            serde_json::json!({"query": "hello"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{hits}");
    assert_eq!(hits.as_array().unwrap().len(), 1, "{hits}");
    assert_eq!(hits[0]["text"], serde_json::json!("hello world"), "{hits}");

    // One call each from indexing and search, with the endpoint's key.
    assert_eq!(*auth.lock().unwrap(), vec!["Bearer sk-up", "Bearer sk-up"]);

    t.store.delete_collection(collection.id).await.ok();
    t.store.delete_model(model_id).await.ok();
}

/// Attaching collections grants the boon `eligible()` also requires, in the
/// row and in what the data plane reads; without it requests silently go
/// without context. Detaching leaves the boon alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn attaching_collections_grants_the_knowledge_boon() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let redis = RedisStore::connect(&std::env::var("OBLETH_TEST_REDIS_URL").unwrap())
        .await
        .expect("connect redis");
    let name = format!("kb-boon-{}", Uuid::new_v4());
    let model = fixture_model(&t.store, &name, 0.0).await;
    assert!(model.boons.is_empty());
    let collection = t
        .store
        .create_collection(&format!("kb-boon-{}", Uuid::new_v4()), "", "embed-a")
        .await
        .expect("collection");
    let path = format!("/api/v1/models/{}/knowledge", model.id);
    let knowledge = vec!["knowledge".to_string()];

    // An empty list on a model without the boon grants nothing.
    let (status, body) = send(
        &t.app,
        json_request("PUT", &path, serde_json::json!({"collection_ids": []})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(t.store.get_model(model.id).await.unwrap().boons.is_empty());

    // Attaching twice grants once.
    for _ in 0..2 {
        let (status, body) = send(
            &t.app,
            json_request(
                "PUT",
                &path,
                serde_json::json!({"collection_ids": [collection.id]}),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(t.store.get_model(model.id).await.unwrap().boons, knowledge);
    }
    let published = redis
        .get_resolved_model(&name)
        .await
        .expect("read redis")
        .expect("published");
    assert_eq!(published.boons, knowledge);
    assert_eq!(published.knowledge_collections, vec![collection.id]);

    // Detaching keeps the boon, and the model has no collections to use it on.
    let (status, body) = send(
        &t.app,
        json_request("PUT", &path, serde_json::json!({"collection_ids": []})),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(t.store.get_model(model.id).await.unwrap().boons, knowledge);
    let published = redis
        .get_resolved_model(&name)
        .await
        .expect("read redis")
        .expect("published");
    assert_eq!(published.boons, knowledge);
    assert!(published.knowledge_collections.is_empty());

    t.store.delete_collection(collection.id).await.ok();
    t.store.delete_model(model.id).await.ok();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn replica_restart_is_surge_first_and_delete_cancels_live_rows() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let name = format!("slurm-{}", Uuid::new_v4());
    let model = fixture_model(&t.store, &name, 0.0).await;
    let put = |body: serde_json::Value| {
        json_request("PUT", &format!("/api/v1/models/{}/managed", model.id), body)
    };
    let (status, body) = send(
        &t.app,
        put(serde_json::json!({
            "serving_port": 8000, "launch_command": "serve", "health_path": "health",
            "time_limit": "4h",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let msg = body["error"].as_str().unwrap();
    assert!(
        msg.contains("health path") && msg.contains("\"4h\""),
        "{msg}"
    );
    let (status, body) = send(
        &t.app,
        put(serde_json::json!({
            "serving_port": 8000, "launch_command": "serve", "target_replicas": 0,
            "min_replicas": 0, "time_limit": "UNLIMITED",
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["target_replicas"], 0);
    assert_eq!(body["startup_timeout_mins"], 60);

    let live = t
        .store
        .create_replica(model.id, "71", Some(8000))
        .await
        .unwrap();
    let (status, _) = send(
        &t.app,
        json_request(
            "POST",
            &format!("/api/v1/replicas/{}/restart", live.id),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let row = t.store.get_replica(live.id).await.unwrap().unwrap();
    assert!(row.replace_requested && !row.cancel_requested);

    let (status, _) = send(
        &t.app,
        json_request(
            "DELETE",
            &format!("/api/v1/replicas/{}", live.id),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let row = t
        .store
        .get_replica(live.id)
        .await
        .unwrap()
        .expect("row kept");
    assert!(row.cancel_requested);
    let (status, _) = send(
        &t.app,
        json_request(
            "POST",
            &format!("/api/v1/replicas/{}/restart", live.id),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "a cancelling replica is not restarted"
    );
    let (status, _) = send(
        &t.app,
        json_request(
            "DELETE",
            &format!("/api/v1/replicas/{}?force=true", live.id),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(t.store.get_replica(live.id).await.unwrap().is_none());

    t.store
        .set_managed_backoff(
            model.id,
            &obleth_config::ManagedBackoff {
                failed_launches: 3,
                backoff_step: 1,
                paused_reason: Some("Paused.".into()),
                paused_until: Some(chrono::Utc::now() + chrono::Duration::minutes(5)),
            },
        )
        .await
        .unwrap();
    let (status, _) = send(
        &t.app,
        json_request(
            "POST",
            &format!("/api/v1/models/{}/managed/retry", model.id),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let m = t.store.get_managed_model(model.id).await.unwrap().unwrap();
    assert!(m.paused_reason.is_none() && m.failed_launches == 0);
    let (status, body) = send(
        &t.app,
        json_request(
            "POST",
            &format!("/api/v1/models/{}/managed/roll", model.id),
            serde_json::json!({}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["replicas"], 0);
    t.store.delete_model(model.id).await.ok();
}

/// A deployment can give each replica its own API key. The switch is off
/// unless set, and a replica's key is returned only on the provisioner's
/// list, never on a replica.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn a_replicas_api_key_is_read_back_only_on_the_resolved_list() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let name = format!("slurm-{}", Uuid::new_v4());
    let model = fixture_model(&t.store, &name, 0.0).await;
    let put = |body: serde_json::Value| {
        json_request("PUT", &format!("/api/v1/models/{}/managed", model.id), body)
    };
    let (status, body) = send(
        &t.app,
        put(serde_json::json!({ "serving_port": 8000, "launch_command": "serve" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["replica_api_key"], false, "off unless asked for");
    let (status, body) = send(
        &t.app,
        put(serde_json::json!({
            "serving_port": 8000, "launch_command": "serve", "replica_api_key": true,
        })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["replica_api_key"], true);

    let key = format!("obl-replica-{}", Uuid::new_v4().simple());
    let create = |body: serde_json::Value| {
        json_request(
            "POST",
            &format!("/api/v1/models/{}/replicas", model.id),
            body,
        )
    };
    let (status, keyed) = send(
        &t.app,
        create(serde_json::json!({ "slurm_job_id": "81", "port_base": 8000, "api_key": key })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{keyed}");
    assert!(!keyed.to_string().contains(&key), "{keyed}");
    let (status, plain) = send(
        &t.app,
        create(serde_json::json!({ "slurm_job_id": "82", "port_base": 8008 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{plain}");

    // The lists the dashboard reads carry no key.
    for path in [
        "/api/v1/replicas".to_string(),
        format!("/api/v1/models/{}/replicas", model.id),
    ] {
        let (status, body) =
            send(&t.app, json_request("GET", &path, serde_json::Value::Null)).await;
        assert_eq!(status, StatusCode::OK);
        assert!(!body.to_string().contains(&key), "{path}");
    }
    // The provisioner's list carries each replica with its key.
    let (status, body) = send(
        &t.app,
        json_request("GET", "/api/v1/replicas/resolved", serde_json::Value::Null),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let row = |id: &serde_json::Value| {
        body.as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == *id)
            .cloned()
            .expect("replica listed")
    };
    assert_eq!(row(&keyed["id"])["api_key"], key);
    assert_eq!(row(&keyed["id"])["slurm_job_id"], "81");
    assert!(row(&plain["id"])["api_key"].is_null());
    t.store.delete_model(model.id).await.ok();
}

/// Rejected request fields go through create, update, their own endpoint
/// and the read views, checked and normalized on every write.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn rejected_request_fields_round_trip_through_the_api() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let name = format!("rrf-{}", Uuid::new_v4());
    let base = serde_json::json!({
        "model_name": name,
        "upstream_model": "served-model",
        "api_base": "http://127.0.0.1:9/v1",
    });

    let mut bad = base.clone();
    bad["rejected_request_fields"] = serde_json::json!(["model"]);
    let (status, body) = send(&t.app, json_request("POST", "/api/v1/models", bad)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let mut create = base.clone();
    create["rejected_request_fields"] = serde_json::json!([" bad_words ", "bad_words"]);
    let (status, created) = send(&t.app, json_request("POST", "/api/v1/models", create)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(
        created["rejected_request_fields"],
        serde_json::json!(["bad_words"])
    );
    let id = created["id"].as_str().unwrap().to_string();

    // Omitted on update leaves the list alone.
    let (status, updated) = send(
        &t.app,
        json_request("PUT", &format!("/api/v1/models/{id}"), base.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(
        updated["rejected_request_fields"],
        serde_json::json!(["bad_words"])
    );

    let mut replace = base.clone();
    replace["rejected_request_fields"] = serde_json::json!(["logit_bias", "bad_words"]);
    let (status, updated) = send(
        &t.app,
        json_request("PUT", &format!("/api/v1/models/{id}"), replace),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(
        updated["rejected_request_fields"],
        serde_json::json!(["logit_bias", "bad_words"])
    );

    let path = format!("/api/v1/models/{id}/rejected-request-fields");
    let (status, body) = send(
        &t.app,
        json_request(
            "PUT",
            &path,
            serde_json::json!({"rejected_request_fields": ["model"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, set) = send(
        &t.app,
        json_request(
            "PUT",
            &path,
            serde_json::json!({"rejected_request_fields": ["bad_words"]}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{set}");
    assert_eq!(
        set["rejected_request_fields"],
        serde_json::json!(["bad_words"])
    );
    let (status, read) = send(
        &t.app,
        json_request(
            "GET",
            &format!("/api/v1/models/{id}"),
            serde_json::Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(
        read["rejected_request_fields"],
        serde_json::json!(["bad_words"])
    );
    let (status, cleared) = send(
        &t.app,
        json_request(
            "PUT",
            &path,
            serde_json::json!({"rejected_request_fields": []}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["rejected_request_fields"], serde_json::json!([]));

    t.store
        .delete_model(created["id"].as_str().unwrap().parse().unwrap())
        .await
        .ok();
}

/// A model's session affinity setting goes through create, update, its
/// own endpoint and the read views, checked on every write.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn session_affinity_round_trips_through_the_api() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let name = format!("msa-{}", Uuid::new_v4());
    let base = serde_json::json!({
        "model_name": name,
        "upstream_model": "served-model",
        "api_base": "http://127.0.0.1:9/v1",
    });

    let mut bad = base.clone();
    bad["session_affinity"] = serde_json::json!({"max_held_percent": 101});
    let (status, body) = send(&t.app, json_request("POST", "/api/v1/models", bad)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, plain) = send(&t.app, json_request("POST", "/api/v1/models", base.clone())).await;
    assert_eq!(status, StatusCode::OK, "{plain}");
    assert_eq!(plain["session_affinity"], serde_json::json!({}));
    t.store
        .delete_model(plain["id"].as_str().unwrap().parse().unwrap())
        .await
        .ok();

    let mut create = base.clone();
    create["session_affinity"] = serde_json::json!({"enabled": true, "max_held_percent": 50});
    let (status, created) = send(&t.app, json_request("POST", "/api/v1/models", create)).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(
        created["session_affinity"],
        serde_json::json!({"enabled": true, "max_held_percent": 50})
    );
    let id = created["id"].as_str().unwrap().to_string();

    // Omitted on update leaves the setting alone.
    let (status, updated) = send(
        &t.app,
        json_request("PUT", &format!("/api/v1/models/{id}"), base.clone()),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["session_affinity"]["max_held_percent"], 50);

    let mut replace = base.clone();
    replace["session_affinity"] = serde_json::json!({"enabled": false});
    let (status, updated) = send(
        &t.app,
        json_request("PUT", &format!("/api/v1/models/{id}"), replace),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(
        updated["session_affinity"],
        serde_json::json!({"enabled": false})
    );

    let path = format!("/api/v1/models/{id}/session-affinity");
    let (status, body) = send(
        &t.app,
        json_request("PUT", &path, serde_json::json!({"max_bypass_secs": 0})),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let own = serde_json::json!({
        "enabled": true,
        "hold_ms": 5000,
        "max_held_percent": 50,
        "max_bypass_secs": 120,
    });
    let (status, set) = send(&t.app, json_request("PUT", &path, own.clone())).await;
    assert_eq!(status, StatusCode::OK, "{set}");
    assert_eq!(set["session_affinity"], own);
    let (status, read) = send(
        &t.app,
        json_request(
            "GET",
            &format!("/api/v1/models/{id}"),
            serde_json::Value::Null,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{read}");
    assert_eq!(read["session_affinity"], own);
    let (status, cleared) = send(&t.app, json_request("PUT", &path, serde_json::json!({}))).await;
    assert_eq!(status, StatusCode::OK, "{cleared}");
    assert_eq!(cleared["session_affinity"], serde_json::json!({}));

    t.store.delete_model(id.parse().unwrap()).await.ok();
}

/// The discovered mode's fields go through create, update and the
/// capacity-mode endpoint with their patch rules, and a `kubernetes`
/// source this gateway cannot read is refused when the model is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn discovered_capacity_fields_are_validated_and_patched() {
    let Some(t) = test_admin_app_discovering(obleth_config::CapacityDiscoveryConfig {
        enabled: true,
        interval: std::time::Duration::from_secs(15),
        namespaces: vec!["inference".into()],
        default_service: "{upstream_model}".into(),
    })
    .await
    else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let name = format!("disc-{}", Uuid::new_v4());
    let base = serde_json::json!({
        "model_name": name,
        "upstream_model": "served-model",
        "api_base": "http://127.0.0.1:9/v1",
    });
    let with = |extra: serde_json::Value| {
        let mut b = base.clone();
        for (k, v) in extra.as_object().unwrap() {
            b[k] = v.clone();
        }
        b
    };

    // Refused: a namespace outside the allowlist, a bad Service name, no
    // per-replica value where the mode needs one, an unknown source or
    // mode, a zero per-replica value.
    for (bad, why) in [
        (
            serde_json::json!({"capacity_mode": "discovered", "capacity_source": "kubernetes",
                               "capacity_namespace": "kube-system",
                               "per_replica_max_in_flight": 8}),
            "OBLETH_CAPACITY_DISCOVERY_NAMESPACES",
        ),
        (
            serde_json::json!({"capacity_mode": "discovered", "capacity_source": "kubernetes",
                               "capacity_service": "app=served-model",
                               "per_replica_max_in_flight": 8}),
            "capacity_service",
        ),
        (
            serde_json::json!({"capacity_mode": "discovered", "capacity_source": "kubernetes"}),
            "per_replica_max_in_flight is required",
        ),
        (
            // A new model has no endpoint rows: its api_base is counted
            // at the model's value.
            serde_json::json!({"capacity_mode": "discovered"}),
            "per_replica_max_in_flight is required",
        ),
        (
            serde_json::json!({"capacity_mode": "discovered", "capacity_source": "prometheus"}),
            "capacity_source",
        ),
        (
            serde_json::json!({"capacity_mode": "automatic"}),
            "capacity_mode",
        ),
        (
            serde_json::json!({"per_replica_max_in_flight": 0}),
            "per_replica_max_in_flight",
        ),
        (
            serde_json::json!({"capacity_headroom": 50.0}),
            "capacity_headroom",
        ),
    ] {
        let (status, body) = send(&t.app, json_request("POST", "/api/v1/models", with(bad))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert!(body.to_string().contains(why), "{why}: {body}");
    }

    // Accepted: the default Service template renders for this model.
    let (status, created) = send(
        &t.app,
        json_request(
            "POST",
            "/api/v1/models",
            with(serde_json::json!({
                "capacity_mode": "discovered",
                "capacity_source": "kubernetes",
                "capacity_namespace": " inference ",
                "per_replica_max_in_flight": 8,
                "capacity_headroom": 1.25,
            })),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    assert_eq!(created["capacity_mode"], "discovered");
    assert_eq!(created["capacity_source"], "kubernetes");
    assert_eq!(created["capacity_namespace"], "inference");
    assert_eq!(created["capacity_service"], serde_json::Value::Null);
    assert_eq!(created["per_replica_max_in_flight"], 8);
    assert_eq!(created["capacity_headroom"], 1.25);
    let id = created["id"].as_str().unwrap().to_string();

    // Update: omitted fields are kept, "" clears text.
    let (status, updated) = send(
        &t.app,
        json_request(
            "PUT",
            &format!("/api/v1/models/{id}"),
            serde_json::json!({
                "upstream_model": "served-model",
                "api_base": "http://127.0.0.1:9/v1",
                "capacity_service": "served-model-head",
                "capacity_namespace": "",
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{updated}");
    assert_eq!(updated["capacity_mode"], "discovered", "kept");
    assert_eq!(updated["capacity_headroom"], 1.25, "kept");
    assert_eq!(updated["per_replica_max_in_flight"], 8, "kept");
    assert_eq!(updated["capacity_service"], "served-model-head");
    assert_eq!(updated["capacity_namespace"], serde_json::Value::Null);

    // Clearing the per-replica value of a kubernetes model is refused.
    let (status, body) = send(
        &t.app,
        json_request(
            "PUT",
            &format!("/api/v1/models/{id}"),
            serde_json::json!({
                "upstream_model": "served-model",
                "api_base": "http://127.0.0.1:9/v1",
                "per_replica_max_in_flight": null,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string().contains("--max-num-seqs"),
        "the error says what to set: {body}"
    );

    // An upstream name the template cannot use needs its own Service.
    let (status, body) = send(
        &t.app,
        json_request(
            "PUT",
            &format!("/api/v1/models/{id}"),
            serde_json::json!({
                "upstream_model": "org/served-model",
                "api_base": "http://127.0.0.1:9/v1",
                "capacity_service": null,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.to_string().contains("set capacity_service"), "{body}");

    // The capacity-mode endpoint switches mode and source together.
    let (status, switched) = send(
        &t.app,
        json_request(
            "PUT",
            &format!("/api/v1/models/{id}/capacity-mode"),
            serde_json::json!({
                "capacity_mode": "discovered",
                "capacity_source": "endpoints",
                "per_replica_max_in_flight": 4,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{switched}");
    assert_eq!(switched["capacity_source"], "endpoints");
    assert_eq!(switched["per_replica_max_in_flight"], 4);
    assert_eq!(
        switched["capacity_service"], "served-model-head",
        "omitted fields are kept"
    );

    // An endpoint carries its own concurrency; omitted on update keeps it.
    let (status, ep) = send(
        &t.app,
        json_request(
            "POST",
            &format!("/api/v1/models/{id}/endpoints"),
            serde_json::json!({"name": "a", "api_base": "http://127.0.0.1:9/v1",
                               "max_in_flight": 16}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ep}");
    assert_eq!(ep["max_in_flight"], 16);
    let ep_id = ep["id"].as_str().unwrap().to_string();
    let (status, ep) = send(
        &t.app,
        json_request(
            "PUT",
            &format!("/api/v1/models/{id}/endpoints/{ep_id}"),
            serde_json::json!({"name": "a", "api_base": "http://127.0.0.1:9/v1",
                               "weight": 50}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{ep}");
    assert_eq!(ep["max_in_flight"], 16, "kept");
    let (status, ep) = send(
        &t.app,
        json_request(
            "PUT",
            &format!("/api/v1/models/{id}/endpoints/{ep_id}"),
            serde_json::json!({"name": "a", "api_base": "http://127.0.0.1:9/v1",
                               "max_in_flight": 0}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{ep}");

    // Every endpoint has its own value, so the model's may be cleared...
    let (status, switched) = send(
        &t.app,
        json_request(
            "PUT",
            &format!("/api/v1/models/{id}/capacity-mode"),
            serde_json::json!({
                "capacity_mode": "discovered",
                "per_replica_max_in_flight": null,
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{switched}");
    assert_eq!(
        switched["per_replica_max_in_flight"],
        serde_json::Value::Null
    );
    // ...and then an endpoint without one, new or cleared, is refused.
    let (status, body) = send(
        &t.app,
        json_request(
            "POST",
            &format!("/api/v1/models/{id}/endpoints"),
            serde_json::json!({"name": "b", "api_base": "http://127.0.0.1:9/v1"}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.to_string()
            .contains("this endpoint needs its own max_in_flight"),
        "{body}"
    );
    let (status, body) = send(
        &t.app,
        json_request(
            "PUT",
            &format!("/api/v1/models/{id}/endpoints/{ep_id}"),
            serde_json::json!({"name": "a", "api_base": "http://127.0.0.1:9/v1",
                               "max_in_flight": null}),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    // The discovery view lists the model with the value in force: no loop
    // runs in this test, so the static fallback, and says why.
    let req = axum::http::Request::get("/api/v1/capacity/discovery")
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::empty())
        .expect("build request");
    let (status, view) = send(&t.app, req).await;
    assert_eq!(status, StatusCode::OK, "{view}");
    assert_eq!(view["enabled"], true);
    assert_eq!(view["namespaces"], serde_json::json!(["inference"]));
    assert_eq!(view["default_service"], "{upstream_model}");
    let entry = view["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["model_id"] == serde_json::json!(id))
        .expect("listed");
    assert_eq!(entry["status"]["state"], "fallback");
    assert_eq!(entry["status"]["effective_max_in_flight"], 32);
    assert_eq!(view["mode"], "local");
    assert_eq!(entry["enforced_max_in_flight"], 32);
    assert_eq!(entry["in_flight"], 0);
    assert_eq!(entry["cluster_in_flight"], serde_json::Value::Null);
    assert!(entry["status"]["reason"]
        .as_str()
        .unwrap()
        .contains("waiting for the next discovery pass"));

    let _ = t.store.delete_model(Uuid::parse_str(&id).unwrap()).await;
}

/// The simulator sits behind the same bearer gate as every other write-side
/// route: it reads the whole model fleet and every tenant's allowlist.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn simulate_requires_admin() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let (status, _) = send(
        &t.app,
        simulate_request(None, serde_json::json!({ "prompt": "hello" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

/// The whole point of the endpoint: a full routing verdict with no upstream
/// call. The fixture model is unique per run and deleted before the
/// assertions so a failure cannot leave a row behind.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn simulate_returns_an_explanation_without_dispatching() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let name = format!("m-{}", Uuid::new_v4());
    let model = fixture_model(&t.store, &name, 0.0).await;

    let (status, body) = send(
        &t.app,
        simulate_request(
            Some(TEST_ADMIN_TOKEN),
            serde_json::json!({ "prompt": "write a python function" }),
        ),
    )
    .await;

    let _ = t.store.delete_model(model.id).await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert!(body.get("scored").is_some());
    assert!(body.get("rejected").is_some());
    // The fixture is enabled, healthy and roomy, so it must survive every
    // hard filter and appear in the ranking.
    let scored = body["scored"].as_array().expect("scored is an array");
    assert!(
        scored.iter().any(|s| s["model"] == serde_json::json!(name)),
        "the fixture model should be a scored candidate: {scored:?}"
    );
    // A "python function" prompt is tagged by the heuristics, never by the
    // classifier: the simulate path must not make a model call.
    assert_eq!(
        body["tag_source"],
        serde_json::json!("heuristic"),
        "simulate must never report a classification it did not perform"
    );
    assert_eq!(body["difficulty_source"], serde_json::json!("heuristic"));
    assert_eq!(
        body["classifier_ms"],
        serde_json::json!(0),
        "no classifier ran, so there is no timing to report"
    );
    assert!(body["tags"]
        .as_array()
        .expect("tags is an array")
        .contains(&serde_json::json!("coding")));
}

/// Weight overrides must be clamped exactly as a settings write would be,
/// and unset fields must fall through to the persisted values — otherwise
/// the tuner shows a ranking no saved configuration could produce.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn simulate_clamps_weight_overrides_like_a_settings_write() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let saved = t
        .store
        .get_auto_router_settings()
        .await
        .expect("read settings")
        .unwrap_or_default();

    let (status, body) = send(
        &t.app,
        simulate_request(
            Some(TEST_ADMIN_TOKEN),
            serde_json::json!({
                "prompt": "hello",
                "cost_weight": 7.5,      // out of range, clamps to 1.0
                "temperature": -3.0,     // out of range, clamps to 0.0
                "default_soft_cap": 0,   // rejected, keeps the saved value
            }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["weights"]["cost"], serde_json::json!(1.0));
    assert_eq!(body["temperature"], serde_json::json!(0.0));
    assert_eq!(
        body["weights"]["soft_cap"],
        serde_json::json!(saved.default_soft_cap as f64),
        "a rejected soft cap falls back to the persisted setting"
    );
    assert_eq!(
        body["weights"]["capacity"],
        serde_json::json!(saved.capacity_weight),
        "an unset override keeps the persisted weight"
    );
}

/// `effort` is the simulator's stand-in for `x-obleth-effort`, so it must
/// report the same `header` provenance the data plane records.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn simulate_reports_the_effort_override_as_a_header_source() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let (status, body) = send(
        &t.app,
        simulate_request(
            Some(TEST_ADMIN_TOKEN),
            serde_json::json!({ "prompt": "hello", "effort": "high" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["difficulty"], serde_json::json!(3));
    assert_eq!(body["difficulty_source"], serde_json::json!("header"));
}

/// A malformed tenant id is the caller's mistake, not a 500.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn simulate_rejects_a_malformed_tenant_id() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let (status, _) = send(
        &t.app,
        simulate_request(
            Some(TEST_ADMIN_TOKEN),
            serde_json::json!({ "prompt": "hello", "tenant_id": "not-a-uuid" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Answering a malformed request with a simulation of a blank prompt is the
/// worst thing to hand someone who is debugging their routing.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn simulate_rejects_non_array_messages() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let (status, _) = send(
        &t.app,
        simulate_request(
            Some(TEST_ADMIN_TOKEN),
            serde_json::json!({ "messages": { "role": "user", "content": "hi" } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Omitting `busyness` must score against the *live* fleet load, the same
/// input the request path passes. Simulating a perfectly idle fleet would
/// make `capacity_weight` look inert in the tuner: the term it scales would
/// be a constant 1.0 for every candidate.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn simulate_defaults_busyness_to_the_live_fleet_load() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let name = format!("m-{}", Uuid::new_v4());
    let model = fixture_model(&t.store, &name, 0.0).await;

    // Occupy 2 of the fixture's 4 in-flight slots, and hold the permits for
    // the duration of the call so the scheduler still reports them.
    let tenant = Uuid::new_v4();
    let mut permits = Vec::new();
    for _ in 0..2 {
        let admitted = t
            .fairshare
            .admit(
                obleth_fairshare::AdmitRequest::new(tenant, name.clone(), 1)
                    .weight(100)
                    .group("default", 100)
                    .model_cap(4),
            )
            .await
            .expect("admitted");
        permits.push(admitted);
    }
    // The scheduler answers an admission before it publishes the load map,
    // so the second permit can arrive a moment before the map counts it.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut live = t.fairshare.model_load();
    while live.get(&name).copied() != Some(2) && std::time::Instant::now() < deadline {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        live = t.fairshare.model_load();
    }
    assert_eq!(
        live.get(&name).copied(),
        Some(2),
        "the scheduler must be reporting the load this test set up"
    );

    // Same request twice: once letting the default apply, once passing the
    // live map explicitly. They must agree — i.e. the default *is* the live
    // load, not an empty map.
    let defaulted = send(
        &t.app,
        simulate_request(
            Some(TEST_ADMIN_TOKEN),
            serde_json::json!({ "prompt": "hello", "uniform": 0.0 }),
        ),
    )
    .await;
    let explicit = send(
        &t.app,
        simulate_request(
            Some(TEST_ADMIN_TOKEN),
            serde_json::json!({ "prompt": "hello", "uniform": 0.0, "busyness": live }),
        ),
    )
    .await;
    // And an explicitly idle fleet, which must differ — otherwise the two
    // assertions above would pass even if busyness were ignored entirely.
    let idle = send(
        &t.app,
        simulate_request(
            Some(TEST_ADMIN_TOKEN),
            serde_json::json!({
                "prompt": "hello",
                "uniform": 0.0,
                "busyness": serde_json::json!({}),
            }),
        ),
    )
    .await;

    drop(permits);
    let _ = t.store.delete_model(model.id).await;

    assert_eq!(defaulted.0, StatusCode::OK, "body: {}", defaulted.1);
    let spare_of = |body: &serde_json::Value| -> f64 {
        body["scored"]
            .as_array()
            .expect("scored is an array")
            .iter()
            .find(|s| s["model"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("fixture missing from the ranking: {body}"))["spare"]
            .as_f64()
            .expect("spare is a number")
    };
    assert_eq!(
        spare_of(&defaulted.1),
        spare_of(&explicit.1),
        "omitting busyness must score against the live fleet load"
    );
    assert_eq!(
        spare_of(&defaulted.1),
        0.5,
        "2 of 4 slots taken is half spare capacity"
    );
    assert_eq!(
        spare_of(&idle.1),
        1.0,
        "an explicitly idle override must still be honoured"
    );
}

/// The tuner calls this endpoint twice — saved weights vs. edited weights —
/// and shows the difference. With independent draws above temperature 0,
/// sampling noise would be attributed to the operator's edit, so the draw
/// must be pinnable and must be echoed back.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn simulate_pins_the_draw_so_two_runs_are_comparable() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    // Two models with clearly different costs, isolated from whatever else
    // lives in the shared test database by a tenant allowlist.
    let cheap = format!("m-cheap-{}", Uuid::new_v4());
    let dear = format!("m-dear-{}", Uuid::new_v4());
    let cheap_row = fixture_model(&t.store, &cheap, 0.000_001).await;
    let dear_row = fixture_model(&t.store, &dear, 0.001).await;
    let tenant = t
        .store
        .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("create tenant");
    t.store
        .update_tenant_allowlist(tenant.id, Some(vec![cheap.clone(), dear.clone()]))
        .await
        .expect("set allowlist");

    let simulate = |uniform: f64| {
        let app = t.app.clone();
        let tenant_id = tenant.id.to_string();
        async move {
            send(
                &app,
                simulate_request(
                    Some(TEST_ADMIN_TOKEN),
                    serde_json::json!({
                        "prompt": "hello",
                        "tenant_id": tenant_id,
                        "temperature": 2.0,
                        "uniform": uniform,
                    }),
                ),
            )
            .await
        }
    };
    let first = simulate(0.0).await;
    let again = simulate(0.0).await;
    let other = simulate(0.999).await;

    let _ = t.store.delete_model(cheap_row.id).await;
    let _ = t.store.delete_model(dear_row.id).await;
    let _ = t.store.delete_tenant(tenant.id).await;

    assert_eq!(first.0, StatusCode::OK, "body: {}", first.1);
    assert_eq!(
        first.1["uniform"],
        serde_json::json!(0.0),
        "the draw used must be echoed back"
    );
    assert_eq!(other.1["uniform"], serde_json::json!(0.999));
    assert_eq!(
        first.1["chosen"], again.1["chosen"],
        "the same pinned draw and the same weights must give the same pick"
    );
    assert_eq!(
        first.1["scored"], again.1["scored"],
        "a pinned draw makes the whole ranking reproducible"
    );
    // Only two candidates survive the allowlist, and at temperature 2.0 a
    // draw at the far end of the mass lands on the runner-up — so the
    // pinning above is doing real work, not describing a degenerate case.
    assert_ne!(
        first.1["chosen"], other.1["chosen"],
        "different pinned draws must be able to move the pick"
    );
}

/// A PUT that never mentions `image_generation_model` must not be rejected
/// because the *previously configured* image model was since deleted or
/// renamed elsewhere. Before the fix, `image_model` fell back to that
/// stale existing value and got re-validated anyway, so an operator could
/// not save an unrelated toggle (here, `vision_enabled`) until they
/// noticed and cleared a field they never touched.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn put_boon_settings_does_not_revalidate_an_unsupplied_image_model() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let original = t
        .store
        .get_boon_settings()
        .await
        .expect("read boon settings");
    let restore = original.clone().unwrap_or_default();

    // Seed a stale reference standing in for "the configured image model
    // was deleted after the fact": a name that is not registered at all.
    let mut seeded = restore.clone();
    seeded.image_generation.enabled = true;
    seeded.image_generation.image_model = Some("ghost-model-does-not-exist".to_string());
    t.store
        .put_boon_settings(&seeded)
        .await
        .expect("seed a stale image_generation_model");

    let req = axum::http::Request::put("/api/v1/settings/boons")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::from(
            serde_json::json!({ "vision_enabled": true }).to_string(),
        ))
        .expect("build request");
    let (status, body) = send(&t.app, req).await;

    // Leave the shared test database as this test found it, regardless of
    // the assertions below.
    let _ = t.store.put_boon_settings(&restore).await;

    assert_eq!(
        status,
        StatusCode::OK,
        "a PUT that never touched image_generation_model must not be rejected \
         over a stale carried-forward value; body: {body}"
    );
    assert_eq!(
        body["image_generation_model"],
        serde_json::json!("ghost-model-does-not-exist"),
        "the unsupplied field must be carried forward unchanged, not cleared"
    );
    assert_eq!(body["vision_enabled"], serde_json::json!(true));
}

/// The speculation verifier receives each model's upstream key, so its
/// URL template is policy-checked on save: per-model Service hosts are
/// fine, placeholders in the credentials and blocked literal hosts are not.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn put_boon_settings_validates_the_verify_url_template() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let restore = t
        .store
        .get_boon_settings()
        .await
        .expect("read boon settings")
        .unwrap_or_default();
    let put = |template: &str| {
        axum::http::Request::put("/api/v1/settings/boons")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(
                serde_json::json!({ "speculation_verify_url_template": template }).to_string(),
            ))
            .expect("build request")
    };
    let mut results = Vec::new();
    for template in [
        "http://{upstream}.serving.svc.cluster.local:8000/v1",
        "http://{model}@10.0.0.5:8000/v1",
        "http://169.254.169.254/{model}/v1",
    ] {
        results.push((template, send(&t.app, put(template)).await));
    }

    let _ = t.store.put_boon_settings(&restore).await;

    let status = |i: usize| (results[i].1).0;
    assert_eq!(status(0), StatusCode::OK, "{:?}", results[0]);
    assert_eq!(status(1), StatusCode::BAD_REQUEST, "{:?}", results[1]);
    assert_eq!(status(2), StatusCode::BAD_REQUEST, "{:?}", results[2]);
}

/// Slurm URL policy: a disabled draft may hold a URL that doesn't resolve
/// yet, enabling it (or testing it) holds the URL to the SSRF policy.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn slurm_url_is_validated_when_enabled_and_on_test() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let original = t
        .store
        .get_slurm_settings()
        .await
        .expect("read slurm settings")
        .unwrap_or_default();
    let put = |body: serde_json::Value| {
        axum::http::Request::put("/api/v1/settings/slurm")
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request")
    };
    // `.invalid` is reserved and never resolves (RFC 6761).
    let unresolvable = "http://slurm.obleth-test.invalid:6820";

    let (disabled, disabled_body) = send(
        &t.app,
        put(serde_json::json!({
            "enabled": false, "slurmrestd_url": unresolvable, "slurm_user": "obleth"
        })),
    )
    .await;
    let (enabled_unresolvable, _) = send(
        &t.app,
        put(serde_json::json!({
            "enabled": true, "slurmrestd_url": unresolvable, "slurm_user": "obleth"
        })),
    )
    .await;
    let (enabled_blocked, _) = send(
        &t.app,
        put(serde_json::json!({
            "enabled": true, "slurmrestd_url": "http://169.254.169.254:6820",
            "slurm_user": "obleth"
        })),
    )
    .await;
    // A blocked URL stored directly (legacy row / restore) must not be pinged.
    let mut blocked = original.clone();
    blocked.slurmrestd_url = "http://169.254.169.254:6820".into();
    t.store
        .put_slurm_settings(&blocked)
        .await
        .expect("seed blocked url");
    let test_req = axum::http::Request::post("/api/v1/settings/slurm/test")
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::empty())
        .expect("build request");
    let (tested, _) = send(&t.app, test_req).await;

    let _ = t.store.put_slurm_settings(&original).await;

    assert_eq!(disabled, StatusCode::OK, "{disabled_body}");
    assert_eq!(enabled_unresolvable, StatusCode::BAD_REQUEST);
    assert_eq!(enabled_blocked, StatusCode::BAD_REQUEST);
    assert_eq!(tested, StatusCode::BAD_REQUEST);
}

/// Alert settings and the global ceiling saved on one replica reach
/// another as soon as it hears of it, and the ceiling outlives a restart
/// until it is cleared.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn alerts_and_the_ceiling_saved_on_one_replica_apply_on_every_replica() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let alerts_before = t.store.get_alert_settings().await.unwrap();
    let ceiling_before = t.store.get_global_ceiling().await.unwrap();
    // Another replica, listening on the invalidation channel as a gateway
    // does.
    let other = t.replica();
    let (subscribed, ready) = tokio::sync::oneshot::channel();
    let listener = tokio::spawn({
        let redis = other.redis.clone();
        let settings = other.replica_settings();
        let mut subscribed = Some(subscribed);
        async move {
            redis
                .run_invalidation_listener(
                    move |message| {
                        if let Invalidation::Settings(which) = Invalidation::parse(&message) {
                            let settings = settings.clone();
                            tokio::spawn(async move { settings.reload(which).await });
                        }
                    },
                    move |_| {
                        if let Some(tx) = subscribed.take() {
                            let _ = tx.send(());
                        }
                    },
                )
                .await
        }
    });
    ready.await.expect("listener subscribed");
    let call = |method: &str, path: &str, body: serde_json::Value| {
        axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(body.to_string()))
            .unwrap()
    };
    let reached = |what: &'static str, done: Box<dyn Fn() -> bool + Send>| async move {
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while !done() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what} did not reach the other replica"));
    };

    let (alerts_status, _) = send(
        &t.app,
        call(
            "PUT",
            "/api/v1/settings/alerts",
            serde_json::json!({ "min_interval_secs": 4321 }),
        ),
    )
    .await;
    let (put_status, put) = send(
        &t.app,
        call(
            "PUT",
            "/api/v1/capacity",
            serde_json::json!({ "max_in_flight": 777 }),
        ),
    )
    .await;
    let alerts = other.alerts.clone();
    reached(
        "the alert settings",
        Box::new(move || alerts.current().min_interval_secs == 4321),
    )
    .await;
    let ceiling = other.ceiling.clone();
    reached(
        "the ceiling",
        Box::new(move || ceiling.max_in_flight() == 777),
    )
    .await;
    // A replica started now reads the saved ceiling over its environment.
    let restarted = t.replica();
    restarted.replica_settings().reload_all().await;
    let restarted_ceiling = (
        restarted.ceiling.max_in_flight(),
        restarted.ceiling.source(),
    );
    // Clearing it returns every replica to its environment value.
    let (cleared_status, cleared) = send(
        &t.app,
        call("DELETE", "/api/v1/capacity", serde_json::json!({})),
    )
    .await;
    let ceiling = other.ceiling.clone();
    reached(
        "the cleared ceiling",
        Box::new(move || ceiling.max_in_flight() == 64),
    )
    .await;
    let (zero, _) = send(
        &t.app,
        call(
            "PUT",
            "/api/v1/capacity",
            serde_json::json!({ "max_in_flight": 0 }),
        ),
    )
    .await;

    listener.abort();
    if let Some(a) = alerts_before {
        let _ = t.store.put_alert_settings(&a).await;
    }
    let _ = match ceiling_before {
        Some(n) => t.store.put_global_ceiling(n).await,
        None => t.store.delete_global_ceiling().await,
    };

    assert_eq!(alerts_status, StatusCode::OK);
    assert_eq!(put_status, StatusCode::OK);
    assert_eq!(put["max_in_flight"], 777);
    assert_eq!(put["source"], "saved");
    assert_eq!(put["environment_max_in_flight"], 64);
    assert_eq!(
        restarted_ceiling,
        (777, replica_settings::CeilingSource::Saved)
    );
    assert_eq!(cleared_status, StatusCode::OK);
    assert_eq!(cleared["max_in_flight"], 64);
    assert_eq!(cleared["source"], "environment");
    assert_eq!(
        other.ceiling.source(),
        replica_settings::CeilingSource::Environment
    );
    assert_eq!(zero, StatusCode::BAD_REQUEST);
}

/// What the provisioner reports about slurmrestd with its tick is kept in
/// Redis, so every replica shows it, and only for the server it was about.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn slurm_server_facts_from_the_provisioner_reach_every_replica() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let original = t
        .store
        .get_slurm_settings()
        .await
        .expect("read slurm settings")
        .unwrap_or_default();
    let mut settings = original.clone();
    settings.slurmrestd_url = "http://slurm.obleth-test.invalid:6820".into();
    settings.slurm_user = "obleth".into();
    settings.slurmrestd_api_version = "auto".into();
    t.store
        .put_slurm_settings(&settings)
        .await
        .expect("seed slurm settings");
    let server = obleth_provisioner::slurm::SlurmServer {
        url: settings.slurmrestd_url.clone(),
        user: "obleth".into(),
        api_version: "v0.0.42".into(),
        available_api_versions: vec!["v0.0.41".into(), "v0.0.42".into()],
        slurm_release: Some("25.05.1".into()),
    };
    let poll = |server: Option<&obleth_provisioner::slurm::SlurmServer>| {
        let mut req = axum::http::Request::get("/api/v1/settings/slurm/resolved")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .header("x-obleth-provisioner-tick-status", "ok");
        if let Some(server) = server {
            req = req.header(
                obleth_provisioner::slurm::SLURM_SERVER_HEADER,
                serde_json::to_string(server).unwrap(),
            );
        }
        req.body(axum::body::Body::empty()).unwrap()
    };
    let read = |app: Router| async move {
        let req = axum::http::Request::get("/api/v1/settings/slurm")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::empty())
            .unwrap();
        send(&app, req).await.1
    };
    let (status, _) = send(&t.app, poll(Some(&server))).await;
    assert_eq!(status, StatusCode::OK);
    // Another replica, which took no part in the poll, shows the facts.
    let view = read(router(t.replica())).await;
    // A tick that does not carry them (an error tick) keeps the last ones.
    send(&t.app, poll(None)).await;
    let kept = read(router(t.replica())).await;
    // Settings for another server show none of them.
    settings.slurmrestd_url = "http://other.obleth-test.invalid:6820".into();
    t.store.put_slurm_settings(&settings).await.unwrap();
    let other = read(router(t.replica())).await;

    let _ = t.store.put_slurm_settings(&original).await;
    let _ = t.redis.set_provisioner_tick_status("{}", 1).await;

    assert_eq!(view["resolved_api_version"], "v0.0.42");
    assert_eq!(
        view["available_api_versions"],
        serde_json::json!(["v0.0.41", "v0.0.42"])
    );
    assert_eq!(view["slurm_release"], "25.05.1");
    assert_eq!(kept["slurm_release"], "25.05.1");
    assert!(other["resolved_api_version"].is_null(), "{other}");
    assert!(other["slurm_release"].is_null(), "{other}");
}

/// A key delete whose Redis eviction fails must not report success: the
/// resolver entry has no TTL, so a silent failure leaves the revoked key
/// working. Failure is injected with a Redis ACL user denied `DEL`.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn delete_key_is_a_502_when_the_eviction_fails() {
    let Ok(redis_url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    if redis_url.contains('@') || !redis_url.starts_with("redis://") {
        eprintln!("skipping: needs a credential-free redis:// OBLETH_TEST_REDIS_URL");
        return;
    }
    let user = format!("obleth-test-nodel-{}", Uuid::new_v4().simple());
    let client = ::redis::Client::open(redis_url.as_str()).expect("redis client");
    let mut conn = client
        .get_multiplexed_async_connection()
        .await
        .expect("redis connect");
    let _: () = ::redis::cmd("ACL")
        .arg("SETUSER")
        .arg(&user)
        .arg("on")
        .arg("nopass")
        .arg("~*")
        .arg("&*")
        .arg("+@all")
        .arg("-del")
        .query_async(&mut conn)
        .await
        .expect("create restricted ACL user");
    let restricted = redis_url.replacen("redis://", &format!("redis://{user}:x@"), 1);
    let Some(t) = test_admin_app_on(&restricted).await else {
        let _: ::redis::RedisResult<()> = ::redis::cmd("ACL")
            .arg("DELUSER")
            .arg(&user)
            .query_async(&mut conn)
            .await;
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
        return;
    };

    let tenant = t
        .store
        .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("create tenant");
    let create = axum::http::Request::post(format!("/api/v1/tenants/{}/keys", tenant.id))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::from(
            serde_json::json!({ "name": "k" }).to_string(),
        ))
        .expect("build request");
    let (created_status, created) = send(&t.app, create).await;
    let key_id = created["key"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let delete = axum::http::Request::delete(format!("/api/v1/keys/{key_id}"))
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::empty())
        .expect("build request");
    let (deleted_status, deleted) = send(&t.app, delete).await;

    let _ = t.store.delete_tenant(tenant.id).await;
    // The failed eviction left the entry behind; remove it as the admin.
    if let Some(secret) = created["secret"].as_str() {
        let hash = obleth_config::hash_api_key(secret);
        let _: ::redis::RedisResult<()> = ::redis::cmd("DEL")
            .arg(format!("obleth:key3:{hash}"))
            .query_async(&mut conn)
            .await;
    }
    let _: ::redis::RedisResult<()> = ::redis::cmd("ACL")
        .arg("DELUSER")
        .arg(&user)
        .query_async(&mut conn)
        .await;

    assert_eq!(created_status, StatusCode::OK, "{created}");
    assert_eq!(deleted_status, StatusCode::BAD_GATEWAY, "{deleted}");
    assert!(
        deleted["error"]
            .as_str()
            .is_some_and(|e| e.contains(RESYNC_ROUTE)),
        "{deleted}"
    );
}

/// Removes the group test's tenants and fairshare group on drop, including
/// on a failed assertion. The store has no group delete, so the group row
/// goes through the pool; tenants go first because they reference it.
struct GroupFixture {
    store: Store,
    group: String,
    tenants: Vec<Uuid>,
}

impl Drop for GroupFixture {
    fn drop(&mut self) {
        let store = self.store.clone();
        let group = std::mem::take(&mut self.group);
        let tenants = std::mem::take(&mut self.tenants);
        // block_in_place needs the multi-thread test flavor.
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                for id in tenants {
                    let _ = store.delete_tenant(id).await;
                }
                let _ = sqlx::query("delete from fairshare_groups where name = $1")
                    .bind(&group)
                    .execute(store.pool())
                    .await;
            });
        });
    }
}

/// A group weight change republishes only that group's keys and never
/// prunes: a key in another group keeps its (sentinel) cache entry, and an
/// orphan entry with no backing row is left for `POST /resync`.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn group_weight_patch_pushes_only_the_groups_keys() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let redis_url = std::env::var("OBLETH_TEST_REDIS_URL").expect("checked above");
    let redis = RedisStore::connect(&redis_url)
        .await
        .expect("connect redis");
    let group = format!("g-{}", Uuid::new_v4().simple());
    t.store
        .create_fairshare_group(&group, 100)
        .await
        .expect("create group");
    let mut fixture = GroupFixture {
        store: t.store.clone(),
        group: group.clone(),
        tenants: Vec::new(),
    };
    let inside = t
        .store
        .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("create tenant");
    fixture.tenants.push(inside.id);
    t.store
        .update_tenant_fairshare_group(inside.id, &group)
        .await
        .expect("move tenant");
    let outside = t
        .store
        .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("create tenant");
    fixture.tenants.push(outside.id);
    let create_key = |tenant: Uuid| {
        axum::http::Request::post(format!("/api/v1/tenants/{tenant}/keys"))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(
                serde_json::json!({ "name": "k" }).to_string(),
            ))
            .expect("build request")
    };
    let (_, a) = send(&t.app, create_key(inside.id)).await;
    let (_, b) = send(&t.app, create_key(outside.id)).await;
    let hash_of = |v: &serde_json::Value| {
        obleth_config::hash_api_key(v["secret"].as_str().unwrap_or_default())
    };
    let (hash_a, hash_b) = (hash_of(&a), hash_of(&b));

    let mut sentinel = redis
        .get_resolved_key(&hash_b)
        .await
        .expect("read b")
        .expect("b cached on create");
    sentinel.tenant_name = "sentinel".to_string();
    redis
        .put_resolved_key(&hash_b, &sentinel)
        .await
        .expect("write sentinel");
    let orphan = format!("orphan-{}", Uuid::new_v4().simple());
    redis
        .put_resolved_key(&orphan, &sentinel)
        .await
        .expect("write orphan");

    let patch = axum::http::Request::patch(format!("/api/v1/fairshare/groups/{group}/weight"))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::from(
            serde_json::json!({ "weight": 777 }).to_string(),
        ))
        .expect("build request");
    let mut pubsub = ::redis::Client::open(redis_url.as_str())
        .expect("redis client")
        .get_async_pubsub()
        .await
        .expect("pubsub connect");
    pubsub
        .subscribe("obleth:invalidate")
        .await
        .expect("subscribe");
    let (status, body) = send(&t.app, patch).await;
    let mut published = Vec::new();
    {
        use futures::StreamExt;
        let mut messages = pubsub.on_message();
        while let Ok(Some(msg)) =
            tokio::time::timeout(std::time::Duration::from_millis(300), messages.next()).await
        {
            published.push(msg.get_payload::<String>().unwrap_or_default());
        }
    }
    let cached_a = redis.get_resolved_key(&hash_a).await.expect("read a");
    let cached_b = redis.get_resolved_key(&hash_b).await.expect("read b");
    let cached_orphan = redis.get_resolved_key(&orphan).await.expect("read orphan");

    let _ = redis.delete_resolved_key(&orphan).await;
    for hash in [&hash_a, &hash_b] {
        let _ = redis.delete_resolved_key(hash).await;
    }
    drop(fixture);
    let group_left: Option<(String,)> =
        sqlx::query_as("select name from fairshare_groups where name = $1")
            .bind(&group)
            .fetch_optional(t.store.pool())
            .await
            .expect("look up group");

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(cached_a.map(|k| k.group_weight), Some(777));
    assert_eq!(
        cached_b.map(|k| k.tenant_name),
        Some("sentinel".to_string()),
        "a key outside the group was re-pushed"
    );
    assert!(cached_orphan.is_some(), "a weight patch must not prune");
    assert_eq!(
        published,
        vec![hash_a.clone()],
        "a weight patch invalidates exactly the group's keys, never `*`"
    );
    assert!(group_left.is_none(), "fixture group was not cleaned up");
}

/// Per-end-user fairshare is set through its own route, reaches the key
/// the gateway resolves, and survives a whole-key `PUT` that doesn't
/// mention it -- the reason it is not one of that `PUT`'s fields.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn end_user_fairshare_has_its_own_route_and_survives_a_key_put() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let tenant = t
        .store
        .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("create tenant");
    let request = |method: &str, path: String, body: serde_json::Value| {
        axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request")
    };

    let (_, created) = send(
        &t.app,
        request(
            "POST",
            format!("/api/v1/tenants/{}/keys", tenant.id),
            serde_json::json!({ "name": "chatbot" }),
        ),
    )
    .await;
    let id = created["key"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let flag = |on: bool| {
        request(
            "PUT",
            format!("/api/v1/keys/{id}/end-user-fairshare"),
            serde_json::json!({ "end_user_fairshare": on }),
        )
    };
    let (on_status, _) = send(&t.app, flag(true)).await;
    let (_, after_put) = send(
        &t.app,
        request(
            "PUT",
            format!("/api/v1/keys/{id}"),
            serde_json::json!({ "name": "chatbot", "weight": 300 }),
        ),
    )
    .await;
    let resolved_on = t
        .store
        .all_resolved_keys()
        .await
        .expect("resolved keys")
        .into_iter()
        .find(|(_, k)| k.key_id.to_string() == id)
        .map(|(_, k)| k.end_user_fairshare);
    let (off_status, _) = send(&t.app, flag(false)).await;
    let listed_off = t
        .store
        .keys_by_ids(&[id.parse().expect("key id")])
        .await
        .expect("keys by id")
        .first()
        .map(|k| k.end_user_fairshare);

    let _ = t.store.delete_tenant(tenant.id).await;

    assert_eq!(
        created["key"]["end_user_fairshare"],
        serde_json::json!(false),
        "off by default"
    );
    assert_eq!(on_status, StatusCode::OK);
    assert_eq!(after_put["weight"], serde_json::json!(300));
    assert_eq!(
        after_put["end_user_fairshare"],
        serde_json::json!(true),
        "a whole-key PUT without the field must not turn it off"
    );
    assert_eq!(
        resolved_on,
        Some(true),
        "the gateway's resolved key carries it"
    );
    assert_eq!(off_status, StatusCode::OK);
    assert_eq!(listed_off, Some(false));
}

/// Allowed networks are validated and normalised on create and update,
/// reach the gateway's resolved key, and a whole-key PUT that does not
/// mention them leaves them alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn allowed_networks_are_normalised_and_survive_a_key_put() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let tenant = t
        .store
        .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("create tenant");
    let request = |method: &str, path: String, body: serde_json::Value| {
        axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request")
    };
    let resolved_networks = |id: String| {
        let store = t.store.clone();
        async move {
            store
                .all_resolved_keys()
                .await
                .expect("resolved keys")
                .into_iter()
                .find(|(_, k)| k.key_id.to_string() == id)
                .map(|(_, k)| k.allowed_networks)
        }
    };

    let (bad_status, bad) = send(
        &t.app,
        request(
            "POST",
            format!("/api/v1/tenants/{}/keys", tenant.id),
            serde_json::json!({ "name": "lab", "allowed_networks": ["10.0.0.0/33"] }),
        ),
    )
    .await;
    let (_, created) = send(
        &t.app,
        request(
            "POST",
            format!("/api/v1/tenants/{}/keys", tenant.id),
            serde_json::json!({
                "name": "lab",
                "allowed_networks": [" 203.0.113.7 ", "10.1.2.3/8", "", "2001:DB8::/32"]
            }),
        ),
    )
    .await;
    let id = created["key"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let secret = created["secret"].as_str().unwrap_or_default().to_string();
    let resolved_after_create = resolved_networks(id.clone()).await;
    let (_, after_put) = send(
        &t.app,
        request(
            "PUT",
            format!("/api/v1/keys/{id}"),
            serde_json::json!({ "name": "lab", "weight": 300 }),
        ),
    )
    .await;
    let (narrow_status, narrowed) = send(
        &t.app,
        request(
            "PUT",
            format!("/api/v1/keys/{id}"),
            serde_json::json!({ "name": "lab", "allowed_networks": ["192.0.2.0/24"] }),
        ),
    )
    .await;
    let redis = RedisStore::connect(
        &std::env::var("OBLETH_TEST_REDIS_URL").expect("checked by test_admin_app"),
    )
    .await
    .expect("connect redis");
    let cached = redis
        .get_resolved_key(&obleth_config::hash_api_key(&secret))
        .await
        .expect("redis read")
        .map(|k| k.allowed_networks);
    let (bad_put_status, _) = send(
        &t.app,
        request(
            "PUT",
            format!("/api/v1/keys/{id}"),
            serde_json::json!({ "name": "lab", "allowed_networks": ["nope"] }),
        ),
    )
    .await;
    let (_, cleared) = send(
        &t.app,
        request(
            "PUT",
            format!("/api/v1/keys/{id}"),
            serde_json::json!({ "name": "lab", "allowed_networks": [] }),
        ),
    )
    .await;

    let _ = t.store.delete_tenant(tenant.id).await;

    assert_eq!(bad_status, StatusCode::BAD_REQUEST, "{bad}");
    assert!(
        bad.to_string().contains("10.0.0.0/33"),
        "the error names the entry: {bad}"
    );
    let normalised = serde_json::json!(["203.0.113.7/32", "10.0.0.0/8", "2001:db8::/32"]);
    assert_eq!(created["key"]["allowed_networks"], normalised);
    assert_eq!(
        resolved_after_create,
        Some(vec![
            "203.0.113.7/32".to_string(),
            "10.0.0.0/8".to_string(),
            "2001:db8::/32".to_string()
        ])
    );
    assert_eq!(after_put["weight"], serde_json::json!(300));
    assert_eq!(
        after_put["allowed_networks"], normalised,
        "a whole-key PUT without the field must not open the key to every network"
    );
    assert_eq!(narrow_status, StatusCode::OK);
    assert_eq!(
        narrowed["allowed_networks"],
        serde_json::json!(["192.0.2.0/24"])
    );
    assert_eq!(
        cached,
        Some(vec!["192.0.2.0/24".to_string()]),
        "the change reaches the gateway's cache at once"
    );
    assert_eq!(bad_put_status, StatusCode::BAD_REQUEST);
    assert_eq!(cleared["allowed_networks"], serde_json::json!([]));
}

/// One rule for update bodies: an omitted field keeps the stored value,
/// `null` clears it, a value sets it. Checked on the bodies that used to
/// reset what they were not sent: keys, reliability, endpoints and the
/// model-wide cap.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn update_bodies_keep_omitted_fields_and_clear_null_ones() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let put =
        |path: String, body: serde_json::Value| send(&t.app, json_request("PUT", &path, body));

    // -- keys ----------------------------------------------------------
    let tenant = t
        .store
        .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("create tenant");
    let (status, created) = send(
        &t.app,
        json_request(
            "POST",
            &format!("/api/v1/tenants/{}/keys", tenant.id),
            serde_json::json!({
                "name": "lab", "weight": 300, "max_in_flight": 4,
                "budget_tokens": 1000, "allowed_networks": ["192.0.2.0/24"]
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let key_id = created["key"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let key_path = format!("/api/v1/keys/{key_id}");
    let (status, kept) = put(key_path.clone(), serde_json::json!({})).await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(
        (
            kept["name"].as_str(),
            kept["weight"].as_i64(),
            kept["max_in_flight"].as_i64(),
            kept["budget_tokens"].as_i64()
        ),
        (Some("lab"), Some(300), Some(4), Some(1000)),
        "an empty update keeps every field: {kept}"
    );
    assert_eq!(
        kept["allowed_networks"],
        serde_json::json!(["192.0.2.0/24"])
    );
    let (_, cleared) = put(
        key_path.clone(),
        serde_json::json!({ "max_in_flight": null, "budget_tokens": null, "description": "edited" }),
    )
    .await;
    assert!(
        cleared["max_in_flight"].is_null() && cleared["budget_tokens"].is_null(),
        "{cleared}"
    );
    assert_eq!(
        (cleared["weight"].as_i64(), cleared["description"].as_str()),
        (Some(300), Some("edited"))
    );
    assert_eq!(
        cleared["allowed_networks"],
        serde_json::json!(["192.0.2.0/24"])
    );
    let (status, _) = put(key_path.clone(), serde_json::json!({ "weight": 0 })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = put(
        format!("/api/v1/keys/{}", Uuid::new_v4()),
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = t.store.delete_tenant(tenant.id).await;

    // -- reliability ---------------------------------------------------
    let model = fixture_model(&t.store, &format!("m-{}", Uuid::new_v4()), 0.0).await;
    let reliability = format!("/api/v1/models/{}/reliability", model.id);
    let (status, set) = put(
        reliability.clone(),
        serde_json::json!({ "max_retries": 3, "request_timeout_secs": 30 }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{set}");
    let (_, kept) = put(
        reliability.clone(),
        serde_json::json!({ "debug_diagnostics": true }),
    )
    .await;
    assert_eq!(
        (
            kept["max_retries"].as_i64(),
            kept["request_timeout_secs"].as_i64(),
            kept["debug_diagnostics"].as_bool()
        ),
        (Some(3), Some(30), Some(true)),
        "{kept}"
    );
    let (_, cleared) = put(
        reliability.clone(),
        serde_json::json!({ "request_timeout_secs": null }),
    )
    .await;
    assert!(cleared["request_timeout_secs"].is_null(), "{cleared}");
    assert_eq!(cleared["max_retries"].as_i64(), Some(3));

    // -- endpoints -----------------------------------------------------
    let endpoints = format!("/api/v1/models/{}/endpoints", model.id);
    let (status, endpoint) = send(
        &t.app,
        json_request(
            "POST",
            &endpoints,
            serde_json::json!({
                "name": "a", "api_base": "http://127.0.0.1:1/v1", "api_key": "k",
                "priority": 5, "weight": 7, "enabled": false, "max_in_flight": 3
            }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{endpoint}");
    let endpoint_path = format!(
        "{endpoints}/{}",
        endpoint["id"].as_str().unwrap_or_default()
    );
    let (status, kept) = put(endpoint_path.clone(), serde_json::json!({ "name": "b" })).await;
    assert_eq!(status, StatusCode::OK, "{kept}");
    assert_eq!(
        (
            kept["name"].as_str(),
            kept["priority"].as_i64(),
            kept["weight"].as_i64(),
            kept["enabled"].as_bool(),
            kept["max_in_flight"].as_i64(),
            kept["api_key_set"].as_bool()
        ),
        (
            Some("b"),
            Some(5),
            Some(7),
            Some(false),
            Some(3),
            Some(true)
        ),
        "{kept}"
    );
    let (_, cleared) = put(
        endpoint_path.clone(),
        serde_json::json!({ "max_in_flight": null, "api_key": null, "enabled": true }),
    )
    .await;
    assert!(cleared["max_in_flight"].is_null(), "{cleared}");
    assert_eq!(
        (
            cleared["enabled"].as_bool(),
            cleared["api_key_set"].as_bool(),
            cleared["weight"].as_i64()
        ),
        (Some(true), Some(false), Some(7))
    );
    let (status, _) = put(
        format!("{endpoints}/{}", Uuid::new_v4()),
        serde_json::json!({ "name": "c" }),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // -- the model-wide cap --------------------------------------------
    let model_path = format!("/api/v1/models/{}", model.id);
    let base = serde_json::json!({ "upstream_model": model.upstream_model, "api_base": "http://127.0.0.1:1/v1" });
    let with = |extra: serde_json::Value| {
        let mut body = base.clone();
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        body
    };
    let (status, set) = put(
        model_path.clone(),
        with(serde_json::json!({ "max_in_flight": 9 })),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{set}");
    let (_, kept) = put(model_path.clone(), base.clone()).await;
    assert_eq!(kept["max_in_flight"].as_i64(), Some(9), "{kept}");
    let (_, lifted) = put(
        model_path.clone(),
        with(serde_json::json!({ "max_in_flight": null })),
    )
    .await;
    assert!(lifted["max_in_flight"].is_null(), "{lifted}");
    let _ = t.store.delete_model(model.id).await;
}

/// Omitting `allowed_countries` from an update keeps the key's list;
/// `[]` or `null` clears it. On create it defaults to none.
#[test]
fn allowed_countries_are_optional_on_create_and_update() {
    let create: CreateKey = serde_json::from_value(serde_json::json!({ "name": "lab" })).unwrap();
    assert!(create.allowed_countries.is_empty());
    let keep: UpdateKey = serde_json::from_value(serde_json::json!({ "name": "lab" })).unwrap();
    assert_eq!(keep.allowed_countries, None);
    let clear: UpdateKey =
        serde_json::from_value(serde_json::json!({ "name": "lab", "allowed_countries": [] }))
            .unwrap();
    assert_eq!(clear.allowed_countries, Some(Some(Vec::new())));
    let null: UpdateKey =
        serde_json::from_value(serde_json::json!({ "allowed_countries": null })).unwrap();
    assert_eq!(null.allowed_countries, Some(None));
}

/// Allowed countries are validated and normalised on create and update,
/// reach the gateway's resolved key, survive a whole-key PUT that does
/// not mention them, and a bad code is a 400 naming it.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn allowed_countries_are_normalised_and_survive_a_key_put() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let tenant = t
        .store
        .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("create tenant");
    let request = |method: &str, path: String, body: serde_json::Value| {
        axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request")
    };
    let resolved_countries = |id: String| {
        let store = t.store.clone();
        async move {
            store
                .all_resolved_keys()
                .await
                .expect("resolved keys")
                .into_iter()
                .find(|(_, k)| k.key_id.to_string() == id)
                .map(|(_, k)| k.allowed_countries)
        }
    };

    let (bad_status, bad) = send(
        &t.app,
        request(
            "POST",
            format!("/api/v1/tenants/{}/keys", tenant.id),
            serde_json::json!({ "name": "lab", "allowed_countries": ["US", "XX"] }),
        ),
    )
    .await;
    let (_, created) = send(
        &t.app,
        request(
            "POST",
            format!("/api/v1/tenants/{}/keys", tenant.id),
            serde_json::json!({
                "name": "lab",
                "allowed_networks": ["192.0.2.0/24"],
                "allowed_countries": [" us ", "ca", "", "US"]
            }),
        ),
    )
    .await;
    let id = created["key"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let resolved_after_create = resolved_countries(id.clone()).await;
    let (_, after_put) = send(
        &t.app,
        request(
            "PUT",
            format!("/api/v1/keys/{id}"),
            serde_json::json!({ "name": "lab", "weight": 300 }),
        ),
    )
    .await;
    let (bad_put_status, bad_put) = send(
        &t.app,
        request(
            "PUT",
            format!("/api/v1/keys/{id}"),
            serde_json::json!({ "name": "lab", "allowed_countries": ["U1"] }),
        ),
    )
    .await;
    let (_, narrowed) = send(
        &t.app,
        request(
            "PUT",
            format!("/api/v1/keys/{id}"),
            serde_json::json!({ "name": "lab", "allowed_countries": ["us"] }),
        ),
    )
    .await;
    let (_, cleared) = send(
        &t.app,
        request(
            "PUT",
            format!("/api/v1/keys/{id}"),
            serde_json::json!({ "name": "lab", "allowed_countries": [] }),
        ),
    )
    .await;

    let _ = t.store.delete_tenant(tenant.id).await;

    assert_eq!(bad_status, StatusCode::BAD_REQUEST, "{bad}");
    assert!(
        bad.to_string().contains("XX"),
        "the error names the entry: {bad}"
    );
    assert_eq!(
        created["key"]["allowed_countries"],
        serde_json::json!(["US", "CA"])
    );
    assert_eq!(
        resolved_after_create,
        Some(vec!["US".to_string(), "CA".to_string()])
    );
    assert_eq!(
        after_put["allowed_countries"],
        serde_json::json!(["US", "CA"]),
        "a whole-key PUT without the field keeps the countries"
    );
    assert_eq!(bad_put_status, StatusCode::BAD_REQUEST);
    assert!(bad_put.to_string().contains("U1"), "{bad_put}");
    assert_eq!(narrowed["allowed_countries"], serde_json::json!(["US"]));
    assert_eq!(
        narrowed["allowed_networks"],
        serde_json::json!(["192.0.2.0/24"])
    );
    assert_eq!(cleared["allowed_countries"], serde_json::json!([]));
    assert_eq!(
        cleared["allowed_networks"],
        serde_json::json!(["192.0.2.0/24"])
    );
}

/// Background priority is operator-only, has its own route, and a
/// whole-key PUT that does not mention it leaves it alone.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn background_has_its_own_route_and_survives_a_key_put() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let tenant = t
        .store
        .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("create tenant");
    let request = |method: &str, path: String, body: serde_json::Value| {
        axum::http::Request::builder()
            .method(method)
            .uri(path)
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request")
    };

    let (_, created) = send(
        &t.app,
        request(
            "POST",
            format!("/api/v1/tenants/{}/keys", tenant.id),
            serde_json::json!({ "name": "runner" }),
        ),
    )
    .await;
    let id = created["key"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let flag = |on: bool| {
        request(
            "PUT",
            format!("/api/v1/keys/{id}/background"),
            serde_json::json!({ "background": on }),
        )
    };
    let (anon_status, _) = send(
        &t.app,
        axum::http::Request::builder()
            .method("PUT")
            .uri(format!("/api/v1/keys/{id}/background"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                serde_json::json!({ "background": true }).to_string(),
            ))
            .expect("build request"),
    )
    .await;
    let after_anon = t
        .store
        .keys_by_ids(&[id.parse().expect("key id")])
        .await
        .expect("keys by id")
        .first()
        .map(|k| k.background);
    let (on_status, _) = send(&t.app, flag(true)).await;
    let (_, after_put) = send(
        &t.app,
        request(
            "PUT",
            format!("/api/v1/keys/{id}"),
            serde_json::json!({ "name": "runner", "weight": 300 }),
        ),
    )
    .await;
    let resolved_on = t
        .store
        .all_resolved_keys()
        .await
        .expect("resolved keys")
        .into_iter()
        .find(|(_, k)| k.key_id.to_string() == id)
        .map(|(_, k)| k.background);
    let (_, listed) = send(
        &t.app,
        request("GET", "/api/v1/keys".to_string(), serde_json::Value::Null),
    )
    .await;
    let listed_on = listed
        .as_array()
        .and_then(|keys| keys.iter().find(|k| k["id"] == serde_json::json!(id)))
        .map(|k| k["background"].clone());
    let (off_status, _) = send(&t.app, flag(false)).await;
    let listed_off = t
        .store
        .keys_by_ids(&[id.parse().expect("key id")])
        .await
        .expect("keys by id")
        .first()
        .map(|k| k.background);
    let (missing_status, _) = send(
        &t.app,
        request(
            "PUT",
            format!("/api/v1/keys/{}/background", Uuid::new_v4()),
            serde_json::json!({ "background": true }),
        ),
    )
    .await;

    let _ = t.store.delete_tenant(tenant.id).await;

    assert_eq!(
        created["key"]["background"],
        serde_json::json!(false),
        "off by default"
    );
    assert_eq!(anon_status, StatusCode::UNAUTHORIZED, "operator-only");
    assert_eq!(after_anon, Some(false), "a refused call changes nothing");
    assert_eq!(on_status, StatusCode::OK);
    assert_eq!(after_put["weight"], serde_json::json!(300));
    assert_eq!(
        after_put["background"],
        serde_json::json!(true),
        "a whole-key PUT without the field must not turn it off"
    );
    assert_eq!(
        resolved_on,
        Some(true),
        "the gateway's resolved key carries it"
    );
    assert_eq!(
        listed_on,
        Some(serde_json::json!(true)),
        "the key list shows it"
    );
    assert_eq!(off_status, StatusCode::OK);
    assert_eq!(listed_off, Some(false));
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
}

/// Fairshare weight and per-model cap are set on a key at creation and
/// edited afterwards, so both have to survive the round trip through the
/// store and come back on the response the dashboard renders.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn key_create_and_update_carry_fairshare_weight_and_cap() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let tenant = t
        .store
        .create_tenant(&format!("t-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("create tenant");
    let post = |body: serde_json::Value| {
        axum::http::Request::post(format!("/api/v1/tenants/{}/keys", tenant.id))
            .header("content-type", "application/json")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::from(body.to_string()))
            .expect("build request")
    };

    let (status, created) = send(
        &t.app,
        post(serde_json::json!({ "name": "k", "weight": 250, "max_in_flight": 3 })),
    )
    .await;
    let key_id = created["key"]["id"].as_str().map(|s| s.to_string());
    let put = |body: serde_json::Value| {
        axum::http::Request::put(format!(
            "/api/v1/keys/{}",
            key_id.clone().unwrap_or_default()
        ))
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::from(body.to_string()))
        .expect("build request")
    };
    let updated = send(
        &t.app,
        put(serde_json::json!({ "name": "k", "weight": 100, "max_in_flight": null })),
    )
    .await;
    let zero_weight = send(
        &t.app,
        post(serde_json::json!({ "name": "k2", "weight": 0 })),
    )
    .await;
    let zero_cap = send(
        &t.app,
        post(serde_json::json!({ "name": "k3", "max_in_flight": 0 })),
    )
    .await;

    let _ = t.store.delete_tenant(tenant.id).await;

    assert_eq!(status, StatusCode::OK, "body: {created}");
    assert_eq!(created["key"]["weight"], serde_json::json!(250));
    assert_eq!(created["key"]["max_in_flight"], serde_json::json!(3));
    assert_eq!(updated.0, StatusCode::OK, "body: {}", updated.1);
    assert_eq!(updated.1["weight"], serde_json::json!(100));
    assert_eq!(
        updated.1["max_in_flight"],
        serde_json::Value::Null,
        "a null cap clears the per-model ceiling"
    );
    assert_eq!(zero_weight.0, StatusCode::BAD_REQUEST);
    assert_eq!(zero_cap.0, StatusCode::BAD_REQUEST);
}

/// The health prober admits through the same scheduler as real traffic, so
/// its hidden tenant and group have to be filtered out of every level of
/// the live view -- and the pool it created on its own must not show up as
/// a model either.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn fairshare_live_hides_the_health_prober() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let health_tenant = model_health::health_tenant_id();
    let probe = t
        .fairshare
        .admit(
            obleth_fairshare::AdmitRequest::new(health_tenant, "probe-model", 1)
                .group(model_health::HEALTH_GROUP, 100),
        )
        .await
        .expect("probe admitted");
    let tenant = Uuid::new_v4();
    let real = t
        .fairshare
        .admit(obleth_fairshare::AdmitRequest::new(tenant, "m", 1))
        .await
        .expect("tenant admitted");

    let req = axum::http::Request::get("/api/v1/fairshare/live")
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::empty())
        .expect("build request");
    let (status, body) = send(&t.app, req).await;

    drop(probe);
    drop(real);

    assert_eq!(status, StatusCode::OK, "body: {body}");
    let pools = body["pools"].as_array().expect("pools is an array");
    assert!(
        pools
            .iter()
            .any(|p| p["model"] == serde_json::json!("m") && p["in_flight"] == 1),
        "the tenant's pool is reported: {body}"
    );
    assert!(
        !pools
            .iter()
            .any(|p| p["model"] == serde_json::json!("probe-model")),
        "the prober's own pool is not a model an operator can act on: {body}"
    );
    let health_tenant = serde_json::json!(health_tenant.to_string());
    let health_group = serde_json::json!(model_health::HEALTH_GROUP);
    let rows = |level: &str| -> Vec<serde_json::Value> {
        pools
            .iter()
            .flat_map(|p| p[level].as_array().cloned().unwrap_or_default())
            .chain(body[level].as_array().cloned().unwrap_or_default())
            .collect()
    };
    assert!(
        rows("groups").iter().all(|g| g["name"] != health_group),
        "the health group is filtered at every level: {body}"
    );
    assert!(
        rows("tenants")
            .iter()
            .all(|t| t["tenant_id"] != health_tenant && t["fairshare_group"] != health_group),
        "the health tenant is filtered at every level: {body}"
    );
    assert!(
        rows("keys").iter().all(|k| k["tenant_id"] != health_tenant),
        "the health tenant's key is filtered at every level: {body}"
    );
    assert_eq!(
        body["global_in_flight"],
        serde_json::json!(1),
        "only the real request counts"
    );
    assert!(
        body["hard_ceiling"].as_u64().unwrap_or(0) > 0,
        "the total ceiling is reported: {body}"
    );
    assert_eq!(body["default_model_max_in_flight"], serde_json::json!(32));
}

/// `GET /keys?q=` matches name, prefix, description and identity subject
/// ignoring case (its `%` and `_` literally) and pages like the full
/// list; `fields=` sends only the named fields, and refuses unknown ones.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn keys_search_and_project_fields() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let tenant = t
        .store
        .create_tenant(&format!("search-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("tenant");
    let mut ids = Vec::new();
    for (name, description) in [
        ("Lab Chatbot", ""),
        ("batch-jobs", "nightly LAB runs"),
        ("100%_done", ""),
        ("other", ""),
        ("lab-two", ""),
    ] {
        let (key, _) = t
            .store
            .create_api_key(
                tenant.id,
                name,
                description,
                None,
                None,
                None,
                None,
                100,
                None,
                &[],
                &[],
            )
            .await
            .expect("key");
        ids.push(key.id.to_string());
    }
    let get = |path: String| {
        let app = t.app.clone();
        async move {
            let req = axum::http::Request::get(&path)
                .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
                .body(axum::body::Body::empty())
                .unwrap();
            let res = tower::ServiceExt::oneshot(app, req)
                .await
                .expect("handler ran");
            let status = res.status();
            let more = res.headers().get("x-obleth-truncated").is_some();
            let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value =
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            (status, more, body)
        }
    };
    let id_list = |body: &serde_json::Value| -> Vec<String> {
        body.as_array()
            .unwrap()
            .iter()
            .map(|k| k["id"].as_str().unwrap().to_string())
            .collect()
    };
    let scope = format!("tenant_id={}", tenant.id);

    // Name or description, any case, in creation order.
    let (status, _, body) = get(format!("/api/v1/keys?{scope}&q=lab")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        id_list(&body),
        [ids[0].clone(), ids[1].clone(), ids[4].clone()]
    );
    // `%` and `_` are text, not wildcards (every prefix has a `_`).
    let (_, _, body) = get(format!("/api/v1/keys?{scope}&q=%25_")).await;
    assert_eq!(id_list(&body), [ids[2].clone()]);
    let (_, _, body) = get(format!("/api/v1/keys?{scope}&q=%25")).await;
    assert_eq!(id_list(&body), [ids[2].clone()]);
    // A blank search is no search.
    let (_, _, body) = get(format!("/api/v1/keys?{scope}&q=%20")).await;
    assert_eq!(id_list(&body).len(), 5);
    // Searches page like the full list.
    let (_, more, body) = get(format!("/api/v1/keys?{scope}&q=LAB&limit=2")).await;
    assert_eq!(
        (id_list(&body), more),
        (vec![ids[0].clone(), ids[1].clone()], true)
    );
    let (_, more, body) = get(format!(
        "/api/v1/keys?{scope}&q=LAB&limit=2&after={}",
        ids[1]
    ))
    .await;
    assert_eq!((id_list(&body), more), (vec![ids[4].clone()], false));

    // Only the fields asked for, in every key.
    let (status, _, body) = get(format!(
        "/api/v1/keys?{scope}&fields=id,name,key_prefix,tenant_id"
    ))
    .await;
    assert_eq!(status, StatusCode::OK);
    for key in body.as_array().unwrap() {
        let mut names: Vec<&str> = key
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        names.sort_unstable();
        assert_eq!(names, ["id", "key_prefix", "name", "tenant_id"]);
    }
    assert_eq!(id_list(&body), ids);
    let (status, _, body) = get(format!("/api/v1/keys?{scope}&fields=id,secret_hash")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, _, _) = get(format!("/api/v1/keys?{scope}&fields=")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Every field a key has can be asked for.
    let (_, _, body) = get(format!("/api/v1/keys?ids={}", ids[0])).await;
    let mut all: Vec<&str> = body[0]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let mut known = KEY_FIELDS.to_vec();
    all.sort_unstable();
    known.sort_unstable();
    assert_eq!(all, known);
}

/// One `GET` of the keys API: status, whether more follow, the next
/// cursor, and the body.
async fn keys_get(
    app: &Router,
    path: &str,
) -> (StatusCode, bool, Option<String>, serde_json::Value) {
    let req = axum::http::Request::get(path)
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::empty())
        .unwrap();
    let res = tower::ServiceExt::oneshot(app.clone(), req)
        .await
        .expect("handler ran");
    let status = res.status();
    let more = res.headers().get("x-obleth-truncated").is_some();
    let next = res
        .headers()
        .get("x-obleth-next-cursor")
        .map(|v| v.to_str().unwrap().to_string());
    let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
        .await
        .unwrap();
    let body = serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, more, next, body)
}

fn key_ids_of(body: &serde_json::Value) -> Vec<Uuid> {
    body.as_array()
        .unwrap_or_else(|| panic!("not a list: {body}"))
        .iter()
        .map(|k| k["id"].as_str().unwrap().parse().unwrap())
        .collect()
}

/// Every key `query` lists, `limit` at a time, following the cursor;
/// each page but the last says more follow.
async fn keys_all_pages(app: &Router, query: &str, limit: usize) -> Vec<Uuid> {
    let mut out = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..1000 {
        let path = match &cursor {
            Some(c) => format!("/api/v1/keys?{query}&limit={limit}&cursor={c}"),
            None => format!("/api/v1/keys?{query}&limit={limit}"),
        };
        let (status, more, next, body) = keys_get(app, &path).await;
        assert_eq!(status, StatusCode::OK, "{path}: {body}");
        let ids = key_ids_of(&body);
        assert!(ids.len() <= limit);
        out.extend(ids);
        assert_eq!(more, next.is_some(), "{path}");
        match next {
            Some(c) => cursor = Some(c),
            None => return out,
        }
    }
    panic!("{query} never ended");
}

/// The created and name orders page by keyset in both directions with
/// no key twice or missed, ties going by id; a page continues after its
/// last key even when that key is gone. Every filter the Keys page
/// offers is applied by the gateway, the count matches, and the budget
/// order ranks capped keys by the share used before the rest by name.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn keys_pages_sort_and_filter_on_the_server() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let tenant_name = format!("pager-{}", Uuid::new_v4().simple());
    let tenant = t
        .store
        .create_tenant(&tenant_name, 100, 1000, None, None)
        .await
        .expect("tenant");
    let mut keys = Vec::new();
    for (name, cap) in [
        ("beta", Some(100)),
        ("Alpha", None),
        ("alpha", None),
        ("gamma", Some(100)),
        ("Delta", Some(1000)),
        ("alpha", None),
    ] {
        let (key, _) = t
            .store
            .create_api_key(
                tenant.id,
                name,
                "",
                cap,
                None,
                None,
                None,
                100,
                None,
                &[],
                &[],
            )
            .await
            .expect("key");
        keys.push(key);
    }
    t.store
        .set_key_flag(keys[1].id, KeyFlag::Disabled, true)
        .await
        .unwrap();
    t.store
        .set_key_flag(keys[4].id, KeyFlag::Disabled, true)
        .await
        .unwrap();
    let scope = format!("tenant_id={}", tenant.id);
    let created: Vec<Uuid> = keys.iter().map(|k| k.id).collect();
    let mut by_name = keys.clone();
    by_name.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then(a.id.cmp(&b.id))
    });
    let by_name: Vec<Uuid> = by_name.iter().map(|k| k.id).collect();
    let reversed = |v: &[Uuid]| v.iter().rev().copied().collect::<Vec<_>>();

    for limit in [1, 2, 4] {
        assert_eq!(keys_all_pages(&t.app, &scope, limit).await, created);
        assert_eq!(
            keys_all_pages(&t.app, &format!("{scope}&sort=created&dir=desc"), limit).await,
            reversed(&created)
        );
        assert_eq!(
            keys_all_pages(&t.app, &format!("{scope}&sort=name"), limit).await,
            by_name
        );
        assert_eq!(
            keys_all_pages(&t.app, &format!("{scope}&sort=name&dir=desc"), limit).await,
            reversed(&by_name)
        );
    }
    // `after` still pages the default order.
    let (_, more, _, body) = keys_get(
        &t.app,
        &format!("/api/v1/keys?{scope}&limit=2&after={}", created[1]),
    )
    .await;
    assert_eq!((key_ids_of(&body), more), (created[2..4].to_vec(), true));

    // Filters, and their counts.
    let filtered = |f: &str| {
        let app = t.app.clone();
        let path = format!("/api/v1/keys?{scope}&{f}");
        async move { key_ids_of(&keys_get(&app, &path).await.3) }
    };
    let count = |f: &str| {
        let app = t.app.clone();
        let path = format!("/api/v1/keys/count?{scope}&{f}");
        async move { keys_get(&app, &path).await.3 }
    };
    assert_eq!(filtered("disabled=true").await, [created[1], created[4]]);
    assert_eq!(filtered("disabled=false").await.len(), 4);
    assert_eq!(filtered("kind=secret").await, created);
    assert!(filtered("kind=identity").await.is_empty());
    assert_eq!(
        filtered("budget=capped").await,
        [created[0], created[3], created[4]]
    );
    assert_eq!(
        filtered("budget=none").await,
        [created[1], created[2], created[5]]
    );
    // The tenant's name matches too.
    let q = &tenant_name[6..14];
    assert_eq!(filtered(&format!("q={q}")).await, created);
    assert_eq!(
        filtered("q=ALPHA&disabled=false").await,
        [created[2], created[5]]
    );
    assert_eq!(
        count("disabled=false").await,
        serde_json::json!({ "keys": 4, "tenants": 1 })
    );
    assert_eq!(count("q=zzz-none").await["keys"], 0);
    // An unknown order or a cursor from another order is refused.
    assert_eq!(
        keys_get(&t.app, "/api/v1/keys?sort=weight").await.0,
        StatusCode::BAD_REQUEST
    );
    let (_, _, name_cursor, _) =
        keys_get(&t.app, &format!("/api/v1/keys?{scope}&sort=name&limit=1")).await;
    let (status, ..) = keys_get(
        &t.app,
        &format!(
            "/api/v1/keys?{scope}&sort=created&limit=1&cursor={}",
            name_cursor.unwrap()
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Budget order: gamma has used 90% of its cap, beta 50%, Delta none;
    // the uncapped keys follow by name.
    let now = chrono::Utc::now();
    let redis = obleth_redis::RedisStore::connect(&std::env::var("OBLETH_TEST_REDIS_URL").unwrap())
        .await
        .unwrap();
    for (key, used) in [(&keys[3], 90), (&keys[0], 50)] {
        let period = obleth_config::budget::period_key(
            key.budget_tokens,
            key.budget_cost_usd,
            key.budget_period.as_deref(),
            key.budget_started_at,
            "UTC",
            now,
        )
        .unwrap();
        redis
            .term_usage_add(&key.id, &period, used, 0.0)
            .await
            .unwrap();
    }
    let uncapped = by_name
        .iter()
        .filter(|id| ![created[0], created[3], created[4]].contains(id));
    let mut budget_order = vec![created[3], created[0], created[4]];
    budget_order.extend(uncapped);
    for limit in [1, 2, 5] {
        assert_eq!(
            keys_all_pages(&t.app, &format!("{scope}&sort=budget"), limit).await,
            budget_order
        );
    }
    assert_eq!(filtered("budget=near").await, [created[3]]);
    assert_eq!(count("budget=near").await["keys"], 1);
    assert_eq!(
        filtered("budget=near&disabled=true").await,
        Vec::<Uuid>::new()
    );

    // A page continues after its last key even when that key is deleted.
    let (_, _, next, body) =
        keys_get(&t.app, &format!("/api/v1/keys?{scope}&sort=name&limit=2")).await;
    let first = key_ids_of(&body);
    t.store.delete_key(first[1]).await.unwrap();
    let (_, _, _, body) = keys_get(
        &t.app,
        &format!(
            "/api/v1/keys?{scope}&sort=name&limit=2&cursor={}",
            next.unwrap()
        ),
    )
    .await;
    let rest = key_ids_of(
        &keys_get(&t.app, &format!("/api/v1/keys?{scope}&sort=name"))
            .await
            .3,
    );
    let at = rest.iter().position(|id| *id == first[0]).unwrap();
    assert_eq!(key_ids_of(&body), rest[at + 1..at + 3].to_vec());
}

/// The use orders rank keys in ClickHouse over the window and page there:
/// highest first, ties by id, keys used outside the window or not at all
/// after them by name, a deleted key's usage left out. `unused` and the
/// tiles read the same window. Runs with a real ClickHouse only.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn keys_use_orders_rank_in_clickhouse() {
    let Ok(url) = std::env::var("OBLETH_TEST_CLICKHOUSE_URL") else {
        eprintln!("skipping: set OBLETH_TEST_CLICKHOUSE_URL to run");
        return;
    };
    let database = format!("obleth_test_{}", Uuid::new_v4().simple());
    let client = clickhouse::Client::default()
        .with_url(&url)
        .with_database(&database);
    let Some(t) = test_admin_app_on_clickhouse(client).await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let tenant = t
        .store
        .create_tenant(&format!("ranks-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("tenant");
    let mut keys = Vec::new();
    for name in ["k0", "k1", "k2", "k3", "k4", "k5"] {
        let (key, _) = t
            .store
            .create_api_key(
                tenant.id,
                name,
                "",
                None,
                None,
                None,
                None,
                100,
                None,
                &[],
                &[],
            )
            .await
            .expect("key");
        keys.push(key.id);
    }
    let dir = std::env::temp_dir().join(format!("obleth-ranks-{}", Uuid::new_v4()));
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
    let hour = 3_600_000;
    let now = usage::now_ms();
    let record = |key: Uuid, ts_ms: i64, cost: f64| {
        let mut rec: obleth_config::UsageRecord = serde_json::from_value(serde_json::json!({
            "request_id": Uuid::new_v4(), "tenant_id": tenant.id, "key_id": key,
            "model": "m", "admission": "fast", "weight": 1, "input_tokens": 10,
            "output_tokens": 5, "estimated_tokens": 15, "queue_wait_ms": 0, "ttft_ms": 0,
            "total_ms": 1, "status_code": 200, "cache_status": "off",
            "ts_ms": ts_ms, "request_type": "chat", "key_name": "k"
        }))
        .unwrap();
        rec.cost_usd = cost;
        rec
    };
    // k1: 3 requests; k0: 2, the most spend; k3: 1, the latest; k5 two
    // requests like k0 (a tie, broken by id); k4 only before the
    // window; k2 never. A deleted key has the most of all.
    for at in [5, 4, 3] {
        sink.record(record(keys[1], now - at * hour, 0.1));
    }
    for at in [6, 5] {
        sink.record(record(keys[0], now - at * hour, 1.5));
        sink.record(record(keys[5], now - at * hour, 0.01));
    }
    sink.record(record(keys[3], now - hour, 0.2));
    sink.record(record(keys[4], now - 40 * 24 * hour, 9.0));
    let gone = Uuid::new_v4();
    for at in 1..=10 {
        sink.record(record(gone, now - at * 60_000, 5.0));
    }
    // Health probes are not use.
    let mut probe = record(keys[2], now - hour, 0.0);
    probe.request_type = crate::model_health::HEALTH_PROBE_REQUEST_TYPE.into();
    sink.record(probe);
    sink.shutdown().await;

    let window = format!("since_ms={}&until_ms={}", now - 30 * 24 * hour, now);
    let scope = format!("tenant_id={}&{window}", tenant.id);
    let (tie_first, tie_second) = if keys[0] > keys[5] {
        (keys[0], keys[5])
    } else {
        (keys[5], keys[0])
    };
    // Unused keys (k2, k4) by name after the ranked ones.
    let expected = [
        (
            "requests",
            vec![keys[1], tie_first, tie_second, keys[3], keys[2], keys[4]],
        ),
        (
            "tokens",
            vec![keys[1], tie_first, tie_second, keys[3], keys[2], keys[4]],
        ),
        (
            "cost",
            vec![keys[0], keys[1], keys[3], keys[5], keys[2], keys[4]],
        ),
        (
            "last_used",
            vec![keys[3], keys[1], tie_first, tie_second, keys[2], keys[4]],
        ),
    ];
    // Once naming the tenant's keys to ClickHouse, once ranking every
    // key and keeping the tenant's over growing rounds.
    for pushdown in [2_000, 0] {
        keys_page::TEST_RANK_PUSHDOWN_MAX.store(pushdown, std::sync::atomic::Ordering::Relaxed);
        for (sort, order) in &expected {
            for limit in [1, 2, 3, 6, 10] {
                assert_eq!(
                    &keys_all_pages(&t.app, &format!("{scope}&sort={sort}"), limit).await,
                    order,
                    "sort={sort} limit={limit} pushdown={pushdown}"
                );
            }
        }
    }
    // Filters apply to both parts; a broad filter (every key in the
    // database) drops the deleted key's rows.
    assert_eq!(
        keys_all_pages(&t.app, &format!("{scope}&sort=requests&q=k1"), 1).await,
        [keys[1]]
    );
    let (_, more, _, body) = keys_get(
        &t.app,
        &format!("/api/v1/keys?{window}&sort=requests&limit=4"),
    )
    .await;
    assert!(more);
    assert_eq!(key_ids_of(&body), [keys[1], tie_first, tie_second, keys[3]]);
    keys_page::TEST_RANK_PUSHDOWN_MAX.store(2_000, std::sync::atomic::Ordering::Relaxed);
    // Unused in the window, by any order.
    assert_eq!(
        keys_all_pages(&t.app, &format!("{scope}&unused=true"), 1).await,
        [keys[2], keys[4]]
    );
    assert_eq!(
        keys_all_pages(&t.app, &format!("{scope}&unused=true&sort=requests"), 1).await,
        [keys[2], keys[4]]
    );
    let (_, _, _, body) =
        keys_get(&t.app, &format!("/api/v1/keys/count?{scope}&unused=true")).await;
    assert_eq!(body["keys"], 2);

    // Tiles: the busiest key that still exists, and the use spans.
    let (status, _, _, stats) = keys_get(
        &t.app,
        &format!("/api/v1/keys/stats?{window}&recent_ms={}", now - 2 * hour),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{stats}");
    assert_eq!(stats["busiest"]["id"], keys[1].to_string());
    assert_eq!(stats["busiest"]["requests"], 3);
    assert!(stats["used"].as_i64().unwrap() >= 4);
    assert!(stats["used_recent"].as_i64().unwrap() >= 1);
    assert_eq!(
        stats["keys"].as_i64().unwrap() - stats["used"].as_i64().unwrap(),
        stats["unused"].as_i64().unwrap()
    );
}

/// `GET /keys` narrows by tenant and ids and pages with `limit` and
/// `after`, flagging every page but the last; `GET /keys/{id}` reads one
/// key and hides the control plane's; `/budgets/usage` reads one tenant.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn keys_read_one_by_id_by_ids_and_in_pages() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let tenant = t
        .store
        .create_tenant(&format!("paging-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("tenant");
    let mut ids = Vec::new();
    for i in 0..5 {
        let budget = (i == 0).then_some(1_000);
        let (key, _) = t
            .store
            .create_api_key(
                tenant.id,
                &format!("k{i}"),
                "",
                budget,
                None,
                None,
                None,
                100,
                None,
                &[],
                &[],
            )
            .await
            .expect("key");
        ids.push(key.id.to_string());
    }
    let get = |path: String| {
        let app = t.app.clone();
        async move {
            let req = axum::http::Request::get(&path)
                .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
                .body(axum::body::Body::empty())
                .unwrap();
            let res = tower::ServiceExt::oneshot(app, req)
                .await
                .expect("handler ran");
            let status = res.status();
            let more = res.headers().get("x-obleth-truncated").is_some();
            let bytes = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            let body: serde_json::Value =
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            (status, more, body)
        }
    };
    let id_list = |body: &serde_json::Value| -> Vec<String> {
        body.as_array()
            .unwrap()
            .iter()
            .map(|k| k["id"].as_str().unwrap().to_string())
            .collect()
    };

    // Two pages of two and one of one, in creation order.
    let mut paged = Vec::new();
    let mut after = String::new();
    let mut flags = Vec::new();
    loop {
        let (status, more, body) = get(format!(
            "/api/v1/keys?tenant_id={}&limit=2{after}",
            tenant.id
        ))
        .await;
        assert_eq!(status, StatusCode::OK);
        let page = id_list(&body);
        flags.push(more);
        let last = page.last().cloned();
        paged.extend(page);
        match last {
            Some(id) if more => after = format!("&after={id}"),
            _ => break,
        }
    }
    assert_eq!(paged, ids);
    assert_eq!(flags, [true, true, false]);

    // Without `limit`, every key and no flag.
    let (_, more, body) = get(format!("/api/v1/keys?tenant_id={}", tenant.id)).await;
    assert_eq!((id_list(&body), more), (ids.clone(), false));

    let (status, _, body) = get(format!("/api/v1/keys?ids={},{}", ids[3], ids[1])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(id_list(&body), [ids[1].clone(), ids[3].clone()]);
    let (status, _, _) = get("/api/v1/keys?ids=".into()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, _, body) = get(format!("/api/v1/keys/{}", ids[2])).await;
    assert_eq!(
        (status, body["name"].as_str()),
        (StatusCode::OK, Some("k2"))
    );
    let (status, _, _) = get(format!("/api/v1/keys/{}", Uuid::new_v4())).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let control = t
        .store
        .list_keys(Some(Store::CONTROL_PLANE_TENANT_ID))
        .await
        .unwrap();
    if let Some(control) = control.first() {
        let (status, _, _) = get(format!("/api/v1/keys/{}", control.id)).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    let (status, _, body) = get(format!("/api/v1/budgets/usage?tenant_id={}", tenant.id)).await;
    assert_eq!(status, StatusCode::OK);
    let budgets = body.as_array().unwrap();
    assert_eq!(budgets.len(), 1, "only the capped key: {body}");
    assert_eq!(budgets[0]["id"].as_str(), Some(ids[0].as_str()));

    // Audit pages say whether more follow too.
    for id in ids.iter().take(3) {
        t.store
            .record_audit(
                "paging-test",
                "update_key",
                "api_key",
                id,
                serde_json::json!({}),
            )
            .await
            .unwrap();
    }
    let (_, more, body) = get(format!(
        "/api/v1/audit?actor=paging-test&limit=2&entity_id={}",
        ids[0]
    ))
    .await;
    assert_eq!((body.as_array().unwrap().len(), more), (1, false));
    let (_, more, body) = get("/api/v1/audit?actor=paging-test&limit=2".into()).await;
    assert_eq!((body.as_array().unwrap().len(), more), (2, true));
}

/// The live view names a pool's tenant and key from the narrow name
/// reads, and keeps its model list for the cache's lifetime: a model
/// written straight to Postgres is not seen until the copy is dropped.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn fairshare_live_names_from_narrow_reads_and_caches_models() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let tenant = t
        .store
        .create_tenant(&format!("live-{}", Uuid::new_v4()), 100, 1000, None, None)
        .await
        .expect("tenant");
    let (key, _) = t
        .store
        .create_api_key(
            tenant.id,
            "live key",
            "",
            None,
            None,
            None,
            None,
            100,
            None,
            &[],
            &[],
        )
        .await
        .expect("key");
    let admitted = t
        .fairshare
        .admit(
            obleth_fairshare::AdmitRequest::new(tenant.id, "named-pool", 1)
                .key(key.id, 100)
                .model_cap(4),
        )
        .await
        .expect("admitted");
    let get = || {
        axum::http::Request::get("/api/v1/fairshare/live")
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::empty())
            .expect("build request")
    };
    let (status, body) = send(&t.app, get()).await;
    drop(admitted);
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let pool = body["pools"]
        .as_array()
        .expect("pools")
        .iter()
        .find(|p| p["model"] == serde_json::json!("named-pool"))
        .cloned()
        .expect("the pool is reported");
    assert_eq!(pool["tenants"][0]["name"], serde_json::json!(tenant.name));
    assert_eq!(pool["keys"][0]["name"], serde_json::json!("live key"));

    let catalog = live_catalog::LiveCatalog::default();
    let first = catalog.get(&t.store).await.expect("first read");
    let name = format!("live-cat-{}", Uuid::new_v4());
    let model = fixture_model(&t.store, &name, 0.0).await;
    let cached = catalog.get(&t.store).await.expect("cached read");
    assert!(
        Arc::ptr_eq(&first, &cached),
        "a read inside the TTL is the cached copy"
    );
    assert!(!cached.models.iter().any(|m| m.model_name == name));
    catalog.invalidate().await;
    let fresh = catalog.get(&t.store).await.expect("fresh read");
    assert!(fresh.models.iter().any(|m| m.model_name == name));

    t.store.delete_model(model.id).await.ok();
    t.store.delete_tenant(tenant.id).await.ok();
}

/// With several replicas live, the view reports this replica's share of
/// each limit next to the configured value, and the count it divides by.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn fairshare_live_reports_per_replica_shares() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    t.fairshare.set_replicas(2);
    let admitted = t
        .fairshare
        .admit(obleth_fairshare::AdmitRequest::new(Uuid::new_v4(), "split", 1).model_cap(9))
        .await
        .expect("admitted");
    let get = |path: &str| {
        axum::http::Request::get(path)
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::empty())
            .expect("build request")
    };
    let (status, body) = send(&t.app, get("/api/v1/fairshare/live")).await;
    let (stats_status, stats) = send(&t.app, get("/api/v1/stats")).await;
    drop(admitted);

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["replicas"], serde_json::json!(2));
    assert_eq!(body["mode"], "split");
    assert_eq!(body["replica_aware"], serde_json::json!(true));
    assert_eq!(body["cluster_in_flight"], serde_json::Value::Null);
    assert_eq!(body["hard_ceiling"], serde_json::json!(32), "ceil(64 / 2)");
    assert_eq!(body["configured_hard_ceiling"], serde_json::json!(64));
    assert!(
        body["configured_max_in_flight"].as_u64() >= body["max_in_flight"].as_u64(),
        "the share never exceeds the configured sum: {body}"
    );
    let pool = body["pools"]
        .as_array()
        .expect("pools")
        .iter()
        .find(|p| p["model"] == serde_json::json!("split"))
        .cloned()
        .expect("the split pool is reported");
    assert_eq!(pool["cap"], serde_json::json!(5), "ceil(9 / 2)");
    assert_eq!(pool["configured_cap"], serde_json::json!(9));
    assert_eq!(stats_status, StatusCode::OK, "body: {stats}");
    assert_eq!(stats["replicas"], serde_json::json!(2));
    assert_eq!(stats["mode"], "split");
}

/// The Service picker's listing: Services from the allowed namespaces'
/// EndpointSlices, and the model's default Service when it exists.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capacity_services_lists_services_and_the_models_default() {
    use axum::routing::get as get_route;
    let app = axum::Router::new().route(
        "/apis/discovery.k8s.io/v1/namespaces/:ns/endpointslices",
        get_route(|Path(ns): Path<String>| async move {
            let items = if ns == "inference" {
                serde_json::json!([
                    {"metadata": {"labels": {"kubernetes.io/service-name": "svc-picker-a"}},
                     "endpoints": [
                        {"addresses": ["10.0.0.1"], "conditions": {"ready": true}, "targetRef": {"uid": "a"}},
                        {"addresses": ["10.0.0.2"], "conditions": {"ready": true}, "targetRef": {"uid": "b"}},
                        {"addresses": ["10.0.0.3"], "conditions": {"ready": false}, "targetRef": {"uid": "c"}}
                     ]},
                    {"metadata": {"labels": {"kubernetes.io/service-name": "svc-picker-other"}},
                     "endpoints": null}
                ])
            } else {
                serde_json::json!([])
            };
            Json(serde_json::json!({"kind": "EndpointSliceList", "metadata": {}, "items": items}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let discovery = capacity_discovery::CapacityDiscovery::with_kube(
        obleth_config::CapacityDiscoveryConfig {
            enabled: true,
            interval: std::time::Duration::from_secs(15),
            namespaces: vec!["batch".into(), "inference".into()],
            default_service: "{upstream_model}".into(),
        },
        capacity_discovery::KubeClient::with_base(format!("http://{addr}"), None),
    );
    let Some(t) = test_admin_app_with_discovery(discovery).await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let model = fixture_model(&t.store, "svc-picker-a", 0.0).await;
    let get = |path: String| {
        axum::http::Request::get(path)
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::empty())
            .expect("build request")
    };
    let (status, body) = send(&t.app, get("/api/v1/capacity/services".into())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["services"],
        serde_json::json!([
            {"service": "svc-picker-a", "namespace": "inference", "ready": 2},
            {"service": "svc-picker-other", "namespace": "inference", "ready": 0}
        ])
    );
    assert_eq!(body["reason"], serde_json::Value::Null);
    assert_eq!(body["default_match"], serde_json::Value::Null);

    let (status, body) = send(
        &t.app,
        get(format!(
            "/api/v1/capacity/services?model={}",
            model.model_name
        )),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["default_service"], "svc-picker-a");
    assert_eq!(
        body["default_match"],
        serde_json::json!({"service": "svc-picker-a", "namespace": "inference", "ready": 2})
    );
    let (status, _) = send(
        &t.app,
        get("/api/v1/capacity/services?model=no-such-model".into()),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let _ = t.store.delete_model(model.id).await;
}

/// Discovery off: an empty listing that says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn capacity_services_says_why_when_discovery_is_off() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let req = axum::http::Request::get("/api/v1/capacity/services")
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::empty())
        .expect("build request");
    let (status, body) = send(&t.app, req).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["services"], serde_json::json!([]));
    assert!(body["reason"]
        .as_str()
        .unwrap()
        .contains("OBLETH_CAPACITY_DISCOVERY_ENABLED"));
}

/// With shared slots, the view reports the configured sizes as this
/// replica's caps and, in `cluster` scope, the other replicas' queues
/// and tenants merged with its own, which stay alongside.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fairshare_live_reports_cluster_wide_counts_in_shared_mode() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let cluster = obleth_fairshare::InMemoryCluster::new();
    t.fairshare.enable_shared_slots(
        Arc::new(cluster.gateway("this")),
        obleth_fairshare::SharedSlotsConfig::default(),
    );
    t.fairshare.set_replicas(3);
    let stats = t.fairshare.stats();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while stats.mode() != SlotMode::Shared {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("shared mode");
    // Another gateway holds two of the pool's slots.
    let other = cluster.gateway("other");
    for _ in 0..2 {
        let claim = obleth_fairshare::SlotClaim {
            pool: PoolKey::Model("shared".into()).slot_id(),
            tenant: Uuid::new_v4(),
            key: Uuid::new_v4(),
            slots: 1,
            pool_cap: 9,
            global_cap: 64,
            tenant_cap: None,
            key_cap: None,
            hold: None,
            redeem_only: false,
            starving: false,
            sibling: None,
            background: false,
        };
        use obleth_fairshare::SharedSlots;
        other.acquire(claim).await.expect("claimed");
    }
    // ...and has published what is running and waiting there.
    let (theirs, ours) = (Uuid::new_v4(), Uuid::new_v4());
    cluster.put_view(
        "other",
        obleth_fairshare::ReplicaView {
            version: obleth_fairshare::view::VIEW_VERSION,
            pools: vec![obleth_fairshare::ReplicaPoolView {
                pool: PoolKey::Model("shared".into()).slot_id(),
                configured_cap: 9,
                in_flight: 2,
                queued: 5,
                groups: vec![obleth_fairshare::ReplicaGroupView {
                    name: "default".into(),
                    weight: 100,
                    in_flight: 2,
                    queued: 5,
                    served: 2.0,
                }],
                tenants: vec![obleth_fairshare::ReplicaTenantView {
                    id: theirs,
                    group: "default".into(),
                    weight: 100,
                    cap: None,
                    in_flight: 2,
                    queued: 5,
                    served: 2.0,
                }],
                ..Default::default()
            }],
        },
        std::time::Duration::from_secs(60),
    );
    let admitted = t
        .fairshare
        .admit(obleth_fairshare::AdmitRequest::new(ours, "shared", 1).model_cap(9))
        .await
        .expect("admitted");
    let get = |path: &str| {
        axum::http::Request::get(path)
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::empty())
            .expect("build request")
    };
    let (status, body) = send(&t.app, get("/api/v1/fairshare/live")).await;
    let (_, stats) = send(&t.app, get("/api/v1/stats")).await;
    drop(admitted);

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["mode"], "shared");
    assert_eq!(body["replicas"], 3);
    assert_eq!(body["hard_ceiling"], 64, "the ceiling is not divided");
    assert_eq!(body["cluster_in_flight"], 3);
    let pool = body["pools"]
        .as_array()
        .expect("pools")
        .iter()
        .find(|p| p["model"] == serde_json::json!("shared"))
        .cloned()
        .expect("the pool is reported");
    assert_eq!(pool["cap"], 9, "the whole pool is usable here");
    assert_eq!(pool["configured_cap"], 9);
    assert_eq!(pool["cluster_in_flight"], 3);
    assert_eq!(body["scope"], "cluster");
    assert_eq!(body["replicas_reporting"], 2);
    assert!(body["scope_reason"].is_null());
    assert_eq!(
        (pool["in_flight"].clone(), pool["queued"].clone()),
        (3.into(), 5.into())
    );
    assert_eq!(
        (
            pool["replica_in_flight"].clone(),
            pool["replica_queued"].clone()
        ),
        (1.into(), 0.into())
    );
    assert_eq!(
        (
            body["global_in_flight"].clone(),
            body["global_queued"].clone()
        ),
        (3.into(), 5.into())
    );
    assert_eq!(
        (
            body["replica_in_flight"].clone(),
            body["replica_queued"].clone()
        ),
        (1.into(), 0.into())
    );
    assert_eq!(body["model_queued"]["shared"], 5);
    let tenant = |id: Uuid| {
        pool["tenants"]
            .as_array()
            .expect("tenants")
            .iter()
            .find(|t| t["tenant_id"] == serde_json::json!(id.to_string()))
            .cloned()
            .unwrap_or_else(|| panic!("tenant {id} in {pool}"))
    };
    assert_eq!(
        (
            tenant(theirs)["in_flight"].clone(),
            tenant(theirs)["queued"].clone()
        ),
        (2.into(), 5.into())
    );
    assert_eq!(tenant(ours)["in_flight"], 1);
    // Two active tenants split the 9 slots evenly, cluster-wide.
    assert_eq!(tenant(theirs)["expected_slots"], 4.5);
    assert_eq!(tenant(ours)["expected_slots"], 4.5);
    assert_eq!(stats["mode"], "shared");
    assert_eq!(stats["cluster_in_flight"], 3);
    assert_eq!(stats["in_flight"], 1);
}

/// Shared mode with the backend not answering: the view falls back to
/// the answering replica's own counts and says why.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fairshare_live_falls_back_to_the_replica_when_the_backend_is_down() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    let cluster = obleth_fairshare::InMemoryCluster::new();
    t.fairshare.enable_shared_slots(
        Arc::new(cluster.gateway("this")),
        obleth_fairshare::SharedSlotsConfig::default(),
    );
    t.fairshare.set_replicas(2);
    let stats = t.fairshare.stats();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while stats.mode() != SlotMode::Shared {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("shared mode");
    let admitted = t
        .fairshare
        .admit(obleth_fairshare::AdmitRequest::new(Uuid::new_v4(), "down", 1).model_cap(4))
        .await
        .expect("admitted");
    // Down after the admission: the next reconcile is seconds away, so
    // the replica is still in shared mode when the view is read.
    cluster.set_failing(true);
    let req = axum::http::Request::get("/api/v1/fairshare/live")
        .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
        .body(axum::body::Body::empty())
        .expect("build request");
    let (status, body) = send(&t.app, req).await;
    drop(admitted);

    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["mode"], "shared");
    assert_eq!(body["scope"], "replica");
    assert!(body["replicas_reporting"].is_null());
    assert!(
        body["scope_reason"]
            .as_str()
            .is_some_and(|r| r.contains("this replica's own")),
        "{body}"
    );
    let pool = body["pools"]
        .as_array()
        .expect("pools")
        .iter()
        .find(|p| p["model"] == serde_json::json!("down"))
        .cloned()
        .expect("the pool is reported");
    assert_eq!(pool["in_flight"], 1);
    assert_eq!(pool["replica_in_flight"], 1);
    assert_eq!(body["global_in_flight"], body["replica_in_flight"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn fairshare_history_scopes_to_a_pool_or_the_aggregate() {
    let Some(t) = test_admin_app().await else {
        eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
        return;
    };
    use obleth_fairshare::{FairshareSample, GroupSample, PoolSample};
    let pool = |model: &str, in_flight: usize, queued: usize, group_in_flight: usize| PoolSample {
        model: model.into(),
        cap: 8,
        in_flight,
        queued,
        groups: vec![
            GroupSample {
                name: "research".into(),
                in_flight: group_in_flight,
                queued,
            },
            GroupSample {
                name: model_health::HEALTH_GROUP.into(),
                in_flight: 1,
                queued: 0,
            },
        ],
    };
    t.fairshare_history.push(FairshareSample {
        ts_ms: 1_000,
        global_in_flight: 4,
        global_queued: 2,
        pools: vec![pool("m", 3, 1, 3), pool("n", 1, 1, 1)],
        cluster: false,
    });
    t.fairshare_history.push(FairshareSample {
        ts_ms: 3_000,
        global_in_flight: 6,
        global_queued: 0,
        pools: vec![pool("m", 6, 0, 6)],
        cluster: true,
    });

    let get = |path: &str| {
        axum::http::Request::get(path)
            .header("authorization", format!("Bearer {TEST_ADMIN_TOKEN}"))
            .body(axum::body::Body::empty())
            .expect("build request")
    };

    let (status, body) = send(&t.app, get("/api/v1/fairshare/history?since_ms=0")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    assert_eq!(body["interval_ms"], 2000);
    assert_eq!(body["retention_ms"], 3_600_000);
    assert_eq!(body["oldest_ts_ms"], 1_000);
    let points = body["points"].as_array().expect("points");
    assert_eq!(points.len(), 2);
    assert_eq!(points[0]["in_flight"], 2, "global 4 minus 2 prober: {body}");
    assert_eq!(points[0]["groups"]["research"], 4);
    assert_eq!(points[0]["cluster"], false);
    assert_eq!(points[1]["cluster"], true);
    assert!(
        points[0]["groups"]
            .get(model_health::HEALTH_GROUP)
            .is_none(),
        "prober group hidden: {body}"
    );

    let (status, body) = send(
        &t.app,
        get("/api/v1/fairshare/history?since_ms=2000&model=m"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let points = body["points"].as_array().expect("points");
    assert_eq!(points.len(), 1);
    assert_eq!(points[0]["ts_ms"], 3_000);
    assert_eq!(
        points[0]["in_flight"], 5,
        "model m: 6 minus 1 prober: {body}"
    );
    assert_eq!(points[0]["groups"]["research"], 6);

    let (status, body) = send(&t.app, get("/api/v1/fairshare/history?since_ms=0&model=n")).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let points = body["points"].as_array().expect("points");
    assert_eq!(
        points.len(),
        2,
        "a sample without the pool still yields a zero point"
    );
    assert_eq!(points[1]["in_flight"], 0);
    assert!(points[1]["groups"].as_object().expect("groups").is_empty());
}

/// A handler can carry `#[utoipa::path]` and still be missing from the
/// document, and an unregistered schema leaves a dangling `$ref` rather
/// than a compile error. Pin both halves.
#[test]
fn openapi_doc_exposes_the_simulate_endpoint() {
    use utoipa::OpenApi;
    let doc = serde_json::to_value(ApiDoc::openapi()).expect("serialize the openapi doc");
    assert!(
        doc["paths"]["/api/v1/router/simulate"]["post"].is_object(),
        "the simulate route is missing from paths(...)"
    );
    let schemas = &doc["components"]["schemas"];
    for name in [
        "SimulateRouteRequest",
        "RouteExplain",
        "ScoredCandidate",
        "Rejection",
        "WeightsView",
        "IntentSource",
    ] {
        assert!(
            schemas.get(name).is_some(),
            "{name} is not registered in components(...)"
        );
    }
}

/// A model route with every field filled with sane defaults, for tests
/// that only care about a couple of fields (e.g. `enabled`/`max_in_flight`).
fn status_body(status: obleth_config::ModelStatus) -> SetModelStatus {
    SetModelStatus {
        status,
        replacement: None,
        retire_at: None,
        note: None,
        redirect: None,
    }
}

#[test]
fn deprecating_records_when_and_names_the_canonical_replacement() {
    let now = chrono::Utc::now();
    let old = fixture_model_route("glm-4-5v");
    let new = fixture_model_route("gemma4-31b-it");
    let mut body = status_body(obleth_config::ModelStatus::Deprecated);
    body.replacement = Some("gemma-alias".into());
    body.retire_at = Some(now + chrono::Duration::days(14));
    body.note = Some("  Ask rc@asu.edu.  ".into());
    let l = lifecycle_from_request(&old, body, Some(&new), now).unwrap();
    assert_eq!(l.status, obleth_config::ModelStatus::Deprecated);
    assert_eq!(l.replacement, "gemma4-31b-it", "canonical, not the alias");
    assert_eq!(l.changed_at, Some(now));
    assert_eq!(l.note, "Ask rc@asu.edu.");
    assert!(!l.redirect);
}

#[test]
fn changed_at_moves_only_when_the_status_does() {
    let t0 = chrono::Utc::now();
    let t1 = t0 + chrono::Duration::days(2);
    let mut old = fixture_model_route("glm-4-5v");
    old.lifecycle.status = obleth_config::ModelStatus::Deprecated;
    old.lifecycle.changed_at = Some(t0);
    let same = lifecycle_from_request(
        &old,
        status_body(obleth_config::ModelStatus::Deprecated),
        None,
        t1,
    )
    .unwrap();
    assert_eq!(same.changed_at, Some(t0), "editing the note keeps the date");
    let retired = lifecycle_from_request(
        &old,
        status_body(obleth_config::ModelStatus::Retired),
        None,
        t1,
    )
    .unwrap();
    assert_eq!(retired.changed_at, Some(t1));
}

#[test]
fn active_clears_everything_else() {
    let now = chrono::Utc::now();
    let old = fixture_model_route("glm-4-5v");
    let mut body = status_body(obleth_config::ModelStatus::Active);
    body.replacement = Some("anything".into());
    body.note = Some("ignored".into());
    body.redirect = Some(true);
    let l = lifecycle_from_request(&old, body, None, now).unwrap();
    assert_eq!(l.status, obleth_config::ModelStatus::Active);
    assert!(l.replacement.is_empty() && l.note.is_empty() && !l.redirect);
}

#[test]
fn staging_clears_everything_else_and_promoting_dates_the_change() {
    let t0 = chrono::Utc::now();
    let t1 = t0 + chrono::Duration::days(3);
    let mut old = fixture_model_route("qwen4-72b");
    let mut body = status_body(obleth_config::ModelStatus::Staged);
    body.note = Some("ignored".into());
    body.retire_at = Some(t1);
    let staged = lifecycle_from_request(&old, body, None, t0).unwrap();
    assert_eq!(staged.status, obleth_config::ModelStatus::Staged);
    assert_eq!(staged.changed_at, Some(t0));
    assert!(staged.note.is_empty() && staged.retire_at.is_none());

    old.lifecycle = staged;
    let live = lifecycle_from_request(
        &old,
        status_body(obleth_config::ModelStatus::Active),
        None,
        t1,
    )
    .unwrap();
    assert_eq!(live.status, obleth_config::ModelStatus::Active);
    assert_eq!(live.changed_at, Some(t1));
}

#[test]
fn a_new_model_starts_active_or_staged() {
    use obleth_config::ModelStatus;
    assert_eq!(initial_model_status(None).unwrap(), ModelStatus::Active);
    assert_eq!(
        initial_model_status(Some(ModelStatus::Staged)).unwrap(),
        ModelStatus::Staged
    );
    for status in [ModelStatus::Deprecated, ModelStatus::Retired] {
        assert!(matches!(
            initial_model_status(Some(status)),
            Err(AdminError::BadRequest(_))
        ));
    }
}

#[test]
fn bad_replacements_and_redirects_are_refused() {
    let now = chrono::Utc::now();
    let old = fixture_model_route("glm-4-5v");
    let refused = |body: SetModelStatus, r: Option<&ModelRoute>| {
        matches!(
            lifecycle_from_request(&old, body, r, now),
            Err(AdminError::BadRequest(_))
        )
    };
    let named = |name: &str| {
        let mut b = status_body(obleth_config::ModelStatus::Deprecated);
        b.replacement = Some(name.into());
        b
    };
    // Not registered.
    assert!(refused(named("ghost"), None));
    // Itself.
    assert!(refused(named("glm-4-5v"), Some(&old)));
    // Another type.
    let mut emb = fixture_model_route("embedder");
    emb.model_type = "embedding".into();
    assert!(refused(named("embedder"), Some(&emb)));
    // Already retired.
    let mut gone = fixture_model_route("gone");
    gone.lifecycle.status = obleth_config::ModelStatus::Retired;
    assert!(refused(named("gone"), Some(&gone)));
    // Staged, so callers could not find it.
    let mut staged = fixture_model_route("staged");
    staged.lifecycle.status = obleth_config::ModelStatus::Staged;
    assert!(refused(named("staged"), Some(&staged)));
    // A redirect with nowhere to go.
    let mut b = status_body(obleth_config::ModelStatus::Retired);
    b.redirect = Some(true);
    assert!(refused(b, None));
    // A note past the limit.
    let mut b = status_body(obleth_config::ModelStatus::Deprecated);
    b.note = Some("x".repeat(obleth_config::MAX_LIFECYCLE_NOTE_CHARS + 1));
    assert!(refused(b, None));
}

#[test]
fn the_published_model_carries_its_reasoning_map() {
    // The request path reads the published copy: a map left at its
    // default here would pass every caller's effort through untouched.
    let mut m = fixture_model_route("two-level");
    m.reasoning = obleth_config::ModelReasoning {
        mode: obleth_config::ReasoningMode::Unsupported,
        ..Default::default()
    };
    let resolved = resolved_model_of(&m, Vec::new(), Vec::new());
    assert_eq!(resolved.reasoning, m.reasoning);
    let view = ModelRouteView::from(m.clone());
    assert_eq!(view.reasoning, m.reasoning);
}

#[test]
fn reasoning_maps_are_checked_and_kept_to_chat_models() {
    let chat = fixture_model_route("chat-model");
    let mut embed = fixture_model_route("embed-model");
    embed.model_type = "embedding".into();
    let unsupported = obleth_config::ModelReasoning {
        mode: obleth_config::ReasoningMode::Unsupported,
        ..Default::default()
    };
    assert!(check_reasoning(&chat, &unsupported).is_ok());
    assert!(check_reasoning(&embed, &unsupported).is_err());
    // Pass-through is fine anywhere: it changes nothing.
    assert!(check_reasoning(&embed, &Default::default()).is_ok());
    let incomplete = obleth_config::ModelReasoning {
        mode: obleth_config::ReasoningMode::Mapped,
        ..Default::default()
    };
    let err = check_reasoning(&chat, &incomplete).unwrap_err();
    assert!(err.to_string().contains("missing"));
}

#[test]
fn the_published_model_carries_its_lifecycle() {
    // The request path reads the published copy, not Postgres: a
    // lifecycle left at its default here means a retired model is served.
    let mut m = fixture_model_route("glm-4-5v");
    m.lifecycle = obleth_config::ModelLifecycle {
        status: obleth_config::ModelStatus::Retired,
        replacement: "gemma4-31b-it".into(),
        note: "Ask rc.".into(),
        redirect: true,
        ..Default::default()
    };
    m.variants = vec![obleth_config::ModelVariant {
        name: "glm-4-5v-spec".into(),
        ..Default::default()
    }];
    let resolved = resolved_model_of(&m, Vec::new(), Vec::new());
    assert_eq!(resolved.lifecycle, m.lifecycle);
    assert_eq!(resolved.variants, m.variants);
    assert_eq!(resolved.model_name, "glm-4-5v");
}

#[test]
fn the_model_view_reports_the_status_in_force() {
    let mut m = fixture_model_route("glm-4-5v");
    m.lifecycle.status = obleth_config::ModelStatus::Deprecated;
    m.lifecycle.retire_at = Some(chrono::Utc::now() - chrono::Duration::minutes(1));
    let view: ModelRouteView = m.into();
    assert_eq!(
        view.lifecycle.status,
        obleth_config::ModelStatus::Deprecated
    );
    assert_eq!(view.effective_status, obleth_config::ModelStatus::Retired);
}

pub(crate) fn fixture_model_route(name: &str) -> ModelRoute {
    let now = chrono::Utc::now();
    ModelRoute {
        id: Uuid::new_v4(),
        model_name: name.to_string(),
        aliases: Vec::new(),
        lifecycle: Default::default(),
        reasoning: Default::default(),
        listed: true,
        rejected_request_fields: Vec::new(),
        session_affinity: Default::default(),
        variants: Vec::new(),
        description: String::new(),
        upstream_model: name.to_string(),
        api_base: "http://upstream.invalid".to_string(),
        api_key: None,
        upstream_headers: Default::default(),
        model_type: obleth_config::DEFAULT_MODEL_TYPE.to_string(),
        quantization: obleth_config::DEFAULT_QUANTIZATION.to_string(),
        input_cost_per_token: 0.0,
        output_cost_per_token: 0.0,
        cost_per_image: 0.0,
        cost_per_audio_second: 0.0,
        cost_per_character: 0.0,
        cost_per_video: 0.0,
        context_window: 128_000,
        admission_weight: 100,
        max_in_flight: None,
        capacity_mode: obleth_config::DEFAULT_CAPACITY_MODE.to_string(),
        capacity_tuned_at: None,
        capacity_source: "endpoints".into(),
        capacity_namespace: None,
        capacity_service: None,
        per_replica_max_in_flight: None,
        capacity_headroom: 1.0,
        supports_function_calling: false,
        supports_system_messages: false,
        supports_response_schema: false,
        supports_tool_choice: false,
        supports_vision: false,
        enabled: true,
        cache_enabled: false,
        cache_ttl_secs: 0,
        tags: Vec::new(),
        boons: Vec::new(),
        tool_servers: Vec::new(),
        request_timeout_secs: None,
        max_retries: 0,
        retry_backoff_ms: obleth_config::DEFAULT_RETRY_BACKOFF_MS,
        endpoint_selection_mode: obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string(),
        debug_diagnostics: false,
        energy_slots_per_node: 0,
        route_bias: obleth_config::DEFAULT_ROUTE_BIAS,
        auto_eligible: obleth_config::DEFAULT_AUTO_ELIGIBLE,
        draft_model: String::new(),
        verify_api_base: String::new(),
        verify_upstream_model: String::new(),
        created_at: now,
        updated_at: now,
    }
}

fn variant_write(name: &str, boons: &[&str]) -> ModelVariantWrite {
    ModelVariantWrite {
        name: name.into(),
        description: None,
        boons: boons.iter().map(|b| b.to_string()).collect(),
    }
}

#[test]
fn variants_are_trimmed_and_their_boons_normalized() {
    let raw = [ModelVariantWrite {
        name: "  glm-5-3-spec ".into(),
        description: Some(" drafted, then verified ".into()),
        boons: vec!["Speculation".into(), "speculation".into()],
    }];
    let out = check_variants(&raw, "glm-5-3", &[]).expect("valid");
    assert_eq!(
        out,
        vec![obleth_config::ModelVariant {
            name: "glm-5-3-spec".into(),
            description: "drafted, then verified".into(),
            boons: vec!["speculation".into()],
        }]
    );
    // A variant that adds nothing beyond the model's own boons is pointless
    // but harmless, and accepted.
    assert!(check_variants(&[variant_write("glm-5-3-plain", &[])], "glm-5-3", &[]).is_ok());
    // Names are case-sensitive, like every other model name.
    assert!(check_variants(&[variant_write("GLM-5-3", &[])], "glm-5-3", &[]).is_ok());
}

#[test]
fn variants_that_would_make_a_name_ambiguous_are_refused() {
    let aliases = vec!["glm-5-3-fp8".to_string()];
    for (raw, why) in [
        (vec![variant_write("  ", &[])], "needs a name"),
        (vec![variant_write("glm-5-3", &[])], "model's own name"),
        (
            vec![variant_write(" glm-5-3-fp8 ", &[])],
            "one of the model's aliases",
        ),
        (vec![variant_write("auto", &[])], "reserved"),
        (
            vec![variant_write("v", &[]), variant_write(" v", &["vision"])],
            "listed twice",
        ),
        (
            vec![variant_write("v", &["speculation", "teleportation"])],
            "unknown boon 'teleportation'",
        ),
    ] {
        match check_variants(&raw, "glm-5-3", &aliases) {
            Err(AdminError::BadRequest(msg)) => assert!(msg.contains(why), "{msg}"),
            other => panic!("expected a 400 containing {why:?}, got {other:?}"),
        }
    }
}

#[test]
fn variants_are_capped() {
    let at_cap: Vec<ModelVariantWrite> = (0..obleth_config::MAX_MODEL_VARIANTS)
        .map(|i| variant_write(&format!("v{i}"), &[]))
        .collect();
    assert!(check_variants(&at_cap, "m", &[]).is_ok());
    let over: Vec<ModelVariantWrite> = (0..=obleth_config::MAX_MODEL_VARIANTS)
        .map(|i| variant_write(&format!("v{i}"), &[]))
        .collect();
    match check_variants(&over, "m", &[]) {
        Err(AdminError::BadRequest(msg)) => assert!(msg.contains("at most"), "{msg}"),
        other => panic!("expected a 400, got {other:?}"),
    }
}

#[test]
fn model_writes_without_variants_still_parse() {
    // A client written before variants existed sends neither field.
    let body: UpdateModel =
        serde_json::from_str(r#"{"upstream_model":"u","api_base":"http://u/v1"}"#).unwrap();
    assert!(body.variants.is_none(), "omitted must mean unchanged");
    let body: UpdateModel =
        serde_json::from_str(r#"{"upstream_model":"u","api_base":"http://u/v1","variants":[]}"#)
            .unwrap();
    assert_eq!(
        body.variants.map(|v| v.len()),
        Some(0),
        "[] must mean remove all"
    );
    let body: CreateModel = serde_json::from_str(
        r#"{"model_name":"m","upstream_model":"u","api_base":"http://u/v1",
            "variants":[{"name":"m-spec","boons":["speculation"]}]}"#,
    )
    .unwrap();
    let v = &body.variants.expect("variants")[0];
    assert_eq!(
        (v.name.as_str(), v.description.as_deref()),
        ("m-spec", None)
    );
}

/// An imported lifecycle's replacement must stand in once the import is
/// applied: registered or created by the file, of the same type, neither
/// staged nor retired.
#[test]
fn import_checks_a_lifecycle_replacement_against_the_post_import_registry() {
    use obleth_config::{ModelLifecycle, ModelStatus};
    use obleth_store::ModelImportWrite;
    let now = chrono::Utc::now();
    let mut retired = fixture_model_route("old");
    retired.lifecycle.status = ModelStatus::Retired;
    let mut embedder = fixture_model_route("embed");
    embedder.model_type = "embedding".into();
    let existing: std::collections::HashMap<String, ModelRoute> = [
        ("old".to_string(), retired),
        ("embed".to_string(), embedder),
        ("live".to_string(), fixture_model_route("live")),
    ]
    .into();
    let write = |name: &str, status: ModelStatus, replacement: &str| ModelImportWrite {
        model_name: name.into(),
        config: obleth_config::ModelConfig {
            lifecycle: ModelLifecycle {
                status,
                replacement: replacement.into(),
                ..Default::default()
            },
            ..Default::default()
        },
        endpoints: None,
    };
    let deprecated = ModelStatus::Deprecated;
    let errors = crate::models_io::replacement_problems(
        &existing,
        &[
            write("a", deprecated, "live"),
            write("b", deprecated, "new"),
            write("new", ModelStatus::Active, ""),
            write("c", deprecated, "old"),
            write("d", deprecated, "embed"),
            write("e", deprecated, "missing"),
            write("f", deprecated, "staged"),
            write("staged", ModelStatus::Staged, ""),
        ],
        now,
    );
    assert_eq!(
        errors,
        vec![
            "model 'c': replacement 'old' is itself retired",
            "model 'd': replacement 'embed' is a 'embedding' model; it must be 'chat' like 'd'",
            "model 'e': replacement 'missing' is not a registered model",
            "model 'f': replacement 'staged' is staged, so callers cannot find it",
        ]
    );
}

#[test]
fn import_refuses_variant_names_another_route_answers_to() {
    use obleth_store::ModelImportWrite;
    let mut owner = fixture_model_route("glm-5-3");
    owner.aliases = vec!["glm-5-3-fp8".into()];
    owner.variants = vec![obleth_config::ModelVariant {
        name: "glm-5-3-spec".into(),
        ..Default::default()
    }];
    let existing: std::collections::HashMap<String, ModelRoute> =
        [("glm-5-3".to_string(), owner)].into();
    let write = |name: &str, aliases: &[&str], variants: &[&str]| ModelImportWrite {
        model_name: name.into(),
        config: obleth_config::ModelConfig {
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            variants: variants
                .iter()
                .map(|v| obleth_config::ModelVariant {
                    name: v.to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
        endpoints: None,
    };

    let errors = crate::models_io::name_conflicts(
        &existing,
        &[write(
            "qwen3",
            &["qwen3-old"],
            &[
                "glm-5-3",
                "glm-5-3-fp8",
                "glm-5-3-spec",
                "qwen3",
                "qwen3-old",
                "auto",
            ],
        )],
    );
    assert_eq!(
        errors,
        vec![
            "model 'qwen3': variant 'glm-5-3' is already taken by model 'glm-5-3'",
            "model 'qwen3': variant 'glm-5-3-fp8' is already taken by model 'glm-5-3'",
            "model 'qwen3': variant 'glm-5-3-spec' is already taken by model 'glm-5-3'",
            "model 'qwen3': variant 'qwen3' is the model's own name",
            "model 'qwen3': variant 'qwen3-old' is already one of the model's aliases",
            "model 'qwen3': variant 'auto' is reserved for automatic model selection",
        ]
    );

    // A file that rewrites the owner may move its variant to another
    // model: the check runs on the state the file would produce.
    let errors = crate::models_io::name_conflicts(
        &existing,
        &[
            write("glm-5-3", &["glm-5-3-fp8"], &[]),
            write("qwen3", &[], &["glm-5-3-spec"]),
        ],
    );
    assert!(errors.is_empty(), "{errors:?}");
}

#[test]
fn an_import_cannot_take_a_family_name() {
    let write = |name: &str, aliases: &[&str], variants: &[&str]| obleth_store::ModelImportWrite {
        model_name: name.into(),
        config: obleth_config::ModelConfig {
            aliases: aliases.iter().map(|a| a.to_string()).collect(),
            variants: variants
                .iter()
                .map(|v| obleth_config::ModelVariant {
                    name: v.to_string(),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        },
        endpoints: None,
    };
    let families: std::collections::HashSet<String> = ["acme".to_string()].into();
    let errors = crate::models_io::family_conflicts(
        &families,
        &[
            write("acme", &[], &[]),
            write("acme-think", &["acme"], &["acme"]),
            write("other", &["other-old"], &[]),
        ],
    );
    assert_eq!(
        errors,
        vec![
            "model 'acme': 'acme' is already the name of a model family",
            "model 'acme-think': 'acme' is already the name of a model family",
            "model 'acme-think': 'acme' is already the name of a model family",
        ]
    );
}

#[test]
fn model_writes_take_upstream_headers_with_null_meaning_keep() {
    let create: CreateModel = serde_json::from_value(serde_json::json!({
        "model_name": "m", "upstream_model": "m", "api_base": "",
        "upstream_headers": {"X-Routing-Hint": "sticky"}
    }))
    .unwrap();
    let write = create.upstream_headers.unwrap();
    assert_eq!(write["X-Routing-Hint"].as_deref(), Some("sticky"));

    let update: UpdateModel = serde_json::from_value(serde_json::json!({
        "upstream_model": "m", "api_base": "",
        "upstream_headers": {"x-upstream-token": null}
    }))
    .unwrap();
    assert_eq!(update.upstream_headers.unwrap()["x-upstream-token"], None);
    let omitted: UpdateModel =
        serde_json::from_value(serde_json::json!({"upstream_model": "m", "api_base": ""})).unwrap();
    assert!(
        omitted.upstream_headers.is_none(),
        "absent means keep them all"
    );
}
