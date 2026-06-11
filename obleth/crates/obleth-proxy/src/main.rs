//! obleth gateway data-plane binary.
//!
//! Boots three listeners: the data-plane proxy, the Management API (admin), and
//! a Prometheus metrics endpoint. Wires Postgres (config SoT), Redis (hot cache
//! + budgets), ClickHouse (usage ledger) and the fairshare scheduler together.

mod mcp;
mod metrics;
mod proxy;
mod router;
mod state;

mod boons;
mod classifier;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State as AxumState;
use axum::routing::get;
use axum::Router;
use moka::future::Cache;
use obleth_config::Config;
use obleth_fairshare::{FairShare, StaticCapacity};
use obleth_redis::RedisStore;
use obleth_store::Store;
use obleth_telemetry::{TelemetrySink, TelemetryStats};
use obleth_tokenizer::HeuristicTokenizer;

use crate::metrics::Metrics;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::from_env();
    let _otel_provider = init_telemetry(&cfg);
    tracing::info!(?cfg.proxy_listen, ?cfg.admin_listen, "starting obleth gateway");

    // ---- connect dependencies (with simple boot-time retries) ----
    let store = retry("postgres", || Store::connect(&cfg.database_url)).await?;
    store.migrate().await?;
    tracing::info!("postgres connected + schema applied");

    let redis = retry("redis", || RedisStore::connect(&cfg.redis_url)).await?;
    tracing::info!("redis connected");

    let telemetry = retry("clickhouse", || {
        TelemetrySink::start(
            &cfg.clickhouse_url,
            &cfg.clickhouse_db,
            &cfg.clickhouse_user,
            &cfg.clickhouse_password,
            &cfg.wal_path,
            cfg.fail_open,
        )
    })
    .await?;
    tracing::info!("clickhouse connected + schema applied");

    // ---- warm the hot cache from the source of truth ----
    match store.all_resolved_keys().await {
        Ok(keys) => {
            for (hash, resolved) in &keys {
                if let Err(e) = redis.put_resolved_key(hash, resolved).await {
                    tracing::warn!(error = %e, "failed to warm key into redis");
                }
            }
            tracing::info!(count = keys.len(), "warmed key cache");
        }
        Err(e) => tracing::warn!(error = %e, "failed to load keys for warming"),
    }

    // ---- fairshare scheduler + capacity ----
    let capacity = Arc::new(StaticCapacity::new(cfg.global_max_in_flight));
    let fairshare = FairShare::start(capacity.clone(), cfg.fairshare_algorithm);

    let metrics = Arc::new(Metrics::new());
    let key_cache: Cache<String, Arc<obleth_config::ResolvedKey>> = Cache::builder()
        .time_to_live(Duration::from_secs(300))
        .max_capacity(100_000)
        .build();
    let model_cache: Cache<String, Arc<obleth_config::ResolvedModel>> = Cache::builder()
        .time_to_live(Duration::from_secs(300))
        .max_capacity(10_000)
        .build();
    let mcp_cache: Cache<String, Arc<obleth_config::ResolvedMcpServer>> = Cache::builder()
        .time_to_live(Duration::from_secs(300))
        .max_capacity(10_000)
        .build();

    let model_registry = router::ModelRegistry::new();

    let http = reqwest::Client::builder()
        .pool_max_idle_per_host(256)
        .build()?;

    // Auto-router classifier settings live in Postgres (app_settings,
    // key='auto_router') and are hot-reloadable. On boot, prefer saved settings;
    // otherwise seed from env so existing deployments work until configured.
    let initial_router_settings = match store.get_auto_router_settings().await {
        Ok(Some(settings)) => settings,
        Ok(None) => obleth_config::AutoRouterSettings {
            classifier_enabled: cfg.auto_classifier_enabled,
            classifier_model: cfg.auto_classifier_model.clone(),
            classifier_timeout_ms: cfg.auto_classifier_timeout_ms,
        },
        Err(e) => {
            tracing::warn!(error = %e, "failed to load auto-router settings; using defaults");
            obleth_config::AutoRouterSettings::default()
        }
    };
    let classifier = classifier::Classifier::new(initial_router_settings);

    // Model-boon settings live in Postgres (app_settings, key='boons') and are
    // hot-reloadable, mirroring the auto-router classifier above.
    let initial_boon_settings = match store.get_boon_settings().await {
        Ok(Some(settings)) => settings,
        Ok(None) => obleth_config::BoonSettings::default(),
        Err(e) => {
            tracing::warn!(error = %e, "failed to load boon settings; using defaults");
            obleth_config::BoonSettings::default()
        }
    };
    let boons = boons::BoonEngine::new(initial_boon_settings);

    // Alert settings are persisted in Postgres (app_settings, key='alerts') and
    // hot-reloadable at runtime. On boot, prefer the saved settings; otherwise
    // seed from the legacy env-configured Slack webhook so existing deployments
    // keep working until an operator saves settings from the control plane.
    let initial_alert_settings = match store.get_alert_settings().await {
        Ok(Some(settings)) => settings,
        Ok(None) => obleth_config::AlertSettings {
            slack_webhook_url: cfg.slack_alerts.webhook_url.clone(),
            email: None,
            min_interval_secs: cfg.slack_alerts.min_interval.as_secs(),
        },
        Err(e) => {
            tracing::warn!(error = %e, "failed to load alert settings; starting with defaults");
            obleth_config::AlertSettings::default()
        }
    };
    let alerts = obleth_admin::AlertDispatcher::new(http.clone(), initial_alert_settings);
    if alerts.enabled() {
        tracing::info!("alerting enabled");
    }

    let app_state = AppState {
        redis: redis.clone(),
        fairshare: fairshare.clone(),
        tokenizer: Arc::new(HeuristicTokenizer::new()),
        telemetry: telemetry.clone(),
        http: http.clone(),
        upstream_base: cfg.upstream_base_url.clone(),
        upstream_timeout: cfg.upstream_timeout,
        key_cache: key_cache.clone(),
        model_cache: model_cache.clone(),
        mcp_cache: mcp_cache.clone(),
        model_registry: model_registry.clone(),
        classifier: classifier.clone(),
        boons: boons.clone(),
        metrics: metrics.clone(),
        fail_open: cfg.fail_open,
        alerts: alerts.clone(),
    };

    match store.all_resolved_models().await {
        Ok(models) => {
            for (name, resolved) in &models {
                if let Err(e) = redis.put_resolved_model(name, resolved).await {
                    tracing::warn!(error = %e, "failed to warm model into redis");
                }
                model_cache
                    .insert(name.clone(), Arc::new(resolved.clone()))
                    .await;
            }
            tracing::info!(count = models.len(), "warmed model cache");
            model_registry.store(build_candidates(&store, models).await);
        }
        Err(e) => tracing::warn!(error = %e, "failed to load models for warming"),
    }

    // Keep the `auto`-router candidate list fresh: model edits, enable/disable,
    // and health/maintenance transitions are all reflected within one interval.
    spawn_model_registry_refresh(
        store.clone(),
        model_registry.clone(),
        classifier.clone(),
        boons.clone(),
    );

    match store.all_resolved_mcp_servers().await {
        Ok(servers) => {
            for (name, resolved) in &servers {
                if let Err(e) = redis.put_resolved_mcp_server(name, resolved).await {
                    tracing::warn!(error = %e, "failed to warm mcp server into redis");
                }
                mcp_cache
                    .insert(name.clone(), Arc::new(resolved.clone()))
                    .await;
            }
            tracing::info!(count = servers.len(), "warmed mcp server cache");
        }
        Err(e) => tracing::warn!(error = %e, "failed to load mcp servers for warming"),
    }

    // ---- pub/sub cache invalidation listener ----
    spawn_invalidation_listener(
        redis.clone(),
        key_cache.clone(),
        model_cache.clone(),
        mcp_cache.clone(),
    );

    // ---- admin (Management API) ----
    let clickhouse_read = build_clickhouse(&cfg);
    let health_runtime = obleth_admin::ModelHealthRuntime {
        scheduled_enabled: cfg.model_health_enabled,
        default_interval_secs: cfg.model_health_interval_secs as i64,
        timeout_secs: cfg.model_health_timeout_secs,
        retention_days: cfg.model_health_retention_days,
        http: http.clone(),
        alerts: Some(Arc::new(alerts.clone()) as Arc<dyn obleth_admin::AlertSink>),
    };
    let admin_state = obleth_admin::AdminState {
        store: store.clone(),
        redis: redis.clone(),
        capacity: capacity.clone(),
        fairshare: fairshare.clone(),
        fairshare_stats: fairshare.stats(),
        clickhouse: clickhouse_read,
        admin_token: cfg.admin_token.clone(),
        health: health_runtime,
        usage_retention_default_days: cfg.usage_retention_days,
        ssrf: obleth_admin::ssrf::SsrfPolicy::from_env(),
        alerts: alerts.clone(),
    };
    obleth_admin::model_health::spawn_worker(admin_state.clone());
    obleth_admin::usage_retention::spawn_worker(admin_state.clone());
    let admin_app = obleth_admin::router(admin_state);

    // ---- routers ----
    let proxy_app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/mcp/:server", axum::routing::any(mcp::mcp_handler))
        .route("/mcp/:server/*rest", axum::routing::any(mcp::mcp_handler))
        .fallback(proxy::proxy_handler)
        .with_state(app_state);

    let metrics_state = MetricsState {
        metrics: metrics.clone(),
        fs: fairshare.stats(),
        tele: telemetry.stats(),
    };
    let metrics_app = Router::new()
        .route("/metrics", get(metrics_handler))
        .with_state(metrics_state);

    // ---- serve all three ----
    let proxy_listener = tokio::net::TcpListener::bind(&cfg.proxy_listen).await?;
    let admin_listener = tokio::net::TcpListener::bind(&cfg.admin_listen).await?;
    let metrics_listener = tokio::net::TcpListener::bind(&cfg.metrics_listen).await?;
    tracing::info!(
        "listening: proxy={}, admin={}, metrics={}",
        cfg.proxy_listen,
        cfg.admin_listen,
        cfg.metrics_listen
    );

    tokio::try_join!(
        async { axum::serve(proxy_listener, proxy_app).await },
        async { axum::serve(admin_listener, admin_app).await },
        async { axum::serve(metrics_listener, metrics_app).await },
    )?;

    Ok(())
}

#[derive(Clone)]
struct MetricsState {
    metrics: Arc<Metrics>,
    fs: Arc<obleth_fairshare::Stats>,
    tele: Arc<TelemetryStats>,
}

async fn metrics_handler(
    AxumState(state): AxumState<MetricsState>,
) -> impl axum::response::IntoResponse {
    use std::sync::atomic::Ordering;
    state.metrics.set_gauges(
        state.fs.in_flight.load(Ordering::Relaxed) as i64,
        state.fs.queued.load(Ordering::Relaxed),
        state.tele.dropped.load(Ordering::Relaxed),
    );
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4",
        )],
        state.metrics.encode(),
    )
}

/// Build the `auto`-router candidate list from the registered models plus the
/// latest health/maintenance state. A model is a candidate when enabled; it is
/// marked unhealthy when its health check reports `unhealthy` or it is inside a
/// maintenance window. Models without a health summary are treated as healthy.
async fn build_candidates(
    store: &Store,
    models: Vec<(String, obleth_config::ResolvedModel)>,
) -> Vec<router::Candidate> {
    let now = chrono::Utc::now();
    let health = store
        .list_model_health_summaries()
        .await
        .unwrap_or_else(|e| {
            tracing::warn!(error = %e, "failed to load model health for auto router");
            Vec::new()
        });
    let health_by_name: std::collections::HashMap<String, &obleth_config::ModelHealthSummary> =
        health.iter().map(|h| (h.model_name.clone(), h)).collect();

    models
        .into_iter()
        .map(|(name, model)| {
            let healthy = match health_by_name.get(&name) {
                Some(h) => {
                    h.status != "unhealthy" && h.maintenance_until.map(|m| m <= now).unwrap_or(true)
                }
                None => true,
            };
            router::Candidate { model, healthy }
        })
        .collect()
}

/// Periodically rebuild the `auto`-router candidate list so enable/disable,
/// metadata edits, and health/maintenance transitions take effect without a
/// restart. Also refreshes the classifier settings (saved from the control
/// plane) so they propagate within one interval. Runs every 15s; failures are
/// logged and retried next tick.
fn spawn_model_registry_refresh(
    store: Store,
    registry: router::ModelRegistry,
    classifier: classifier::Classifier,
    boons: boons::BoonEngine,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match store.all_resolved_models().await {
                Ok(models) => registry.store(build_candidates(&store, models).await),
                Err(e) => tracing::warn!(error = %e, "auto-router model refresh failed"),
            }
            match store.get_auto_router_settings().await {
                Ok(Some(settings)) => classifier.update(settings),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "auto-router settings refresh failed"),
            }
            match store.get_boon_settings().await {
                Ok(Some(settings)) => boons.update(settings),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "boon settings refresh failed"),
            }
        }
    });
}

fn spawn_invalidation_listener(
    redis: RedisStore,
    key_cache: Cache<String, Arc<obleth_config::ResolvedKey>>,
    model_cache: Cache<String, Arc<obleth_config::ResolvedModel>>,
    mcp_cache: Cache<String, Arc<obleth_config::ResolvedMcpServer>>,
) {
    tokio::spawn(async move {
        loop {
            let key_cache = key_cache.clone();
            let model_cache = model_cache.clone();
            let mcp_cache = mcp_cache.clone();
            let result = redis
                .run_invalidation_listener(move |target| {
                    let key_cache = key_cache.clone();
                    let model_cache = model_cache.clone();
                    let mcp_cache = mcp_cache.clone();
                    tokio::spawn(async move {
                        if target == "*" {
                            key_cache.invalidate_all();
                            model_cache.invalidate_all();
                            mcp_cache.invalidate_all();
                            return;
                        }
                        if let Some(name) = target.strip_prefix("model:") {
                            model_cache.invalidate(name).await;
                        } else if let Some(name) = target.strip_prefix("mcp:") {
                            mcp_cache.invalidate(name).await;
                        } else {
                            key_cache.invalidate(&target).await;
                        }
                    });
                })
                .await;
            if let Err(e) = result {
                tracing::warn!(error = %e, "invalidation listener stopped; retrying in 2s");
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

/// Initialize logging + (optionally) OTLP trace export. Returns the tracer
/// provider so it lives for the process lifetime and flushes on shutdown.
/// Tracing is fully gated on `OBLETH_OTEL_ENDPOINT`: unset = logs only, zero cost.
fn init_telemetry(cfg: &Config) -> Option<opentelemetry_sdk::trace::SdkTracerProvider> {
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::util::SubscriberInitExt;

    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,obleth=debug".into());
    let fmt_layer = tracing_subscriber::fmt::layer();

    let provider =
        cfg.otel_endpoint
            .as_deref()
            .and_then(|endpoint| match build_tracer_provider(endpoint) {
                Ok(p) => {
                    tracing::info!(%endpoint, "OTLP trace export enabled");
                    Some(p)
                }
                Err(e) => {
                    eprintln!("otel init failed ({e}); continuing without tracing");
                    None
                }
            });

    // `Option<Layer>` is itself a Layer (no-op when None), so this composes
    // cleanly whether or not tracing is enabled.
    let otel_layer = provider.as_ref().map(|p| {
        use opentelemetry::trace::TracerProvider;
        tracing_opentelemetry::layer().with_tracer(p.tracer("obleth"))
    });

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt_layer)
        .with(otel_layer)
        .init();

    provider
}

fn build_tracer_provider(
    endpoint: &str,
) -> anyhow::Result<opentelemetry_sdk::trace::SdkTracerProvider> {
    use opentelemetry_otlp::{Protocol, WithExportConfig};

    let traces_url = format!("{}/v1/traces", endpoint.trim_end_matches('/'));
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(traces_url)
        .build()?;
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_batch_exporter(exporter)
        .with_resource(
            opentelemetry_sdk::Resource::builder()
                .with_service_name("obleth")
                .build(),
        )
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());
    Ok(provider)
}

fn build_clickhouse(cfg: &Config) -> clickhouse::Client {
    let mut client = clickhouse::Client::default()
        .with_url(&cfg.clickhouse_url)
        .with_user(&cfg.clickhouse_user)
        .with_database(&cfg.clickhouse_db);
    if !cfg.clickhouse_password.is_empty() {
        client = client.with_password(&cfg.clickhouse_password);
    }
    client
}

/// Retry a fallible async connector a handful of times to absorb boot ordering.
async fn retry<T, E, F, Fut>(name: &str, mut f: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, E>>,
    E: std::fmt::Display,
{
    let mut last = String::new();
    for attempt in 1..=10 {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                last = e.to_string();
                tracing::warn!(%name, attempt, error = %last, "connect failed; retrying");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    anyhow::bail!("could not connect to {name} after retries: {last}")
}
