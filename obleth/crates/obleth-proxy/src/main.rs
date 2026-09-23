//! obleth gateway data-plane binary.
//!
//! Boots three listeners: the data-plane proxy, the Management API (admin), and
//! a Prometheus metrics endpoint. Wires Postgres (config SoT), Redis (hot cache
//! + budgets), ClickHouse (usage ledger) and the fairshare scheduler together.

mod completion;
mod energy;
mod jwt_auth;
mod knowledge;
mod mcp;
mod metrics;
mod output_monitor;
mod proxy;
mod responses;
mod router;
mod state;
mod verdicts;

mod boons;
mod classifier;
mod diagnostics;
pub mod tracer;

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State as AxumState;
use axum::routing::get;
use axum::Router;
use moka::future::Cache;
use obleth_config::Config;
use obleth_fairshare::{
    FairShare, FairshareHistory, StaticCapacity, FAIRSHARE_HISTORY_INTERVAL_MS,
};
use obleth_redis::RedisStore;
use obleth_store::Store;
use obleth_telemetry::{TelemetrySink, TelemetryStats};
use obleth_tokenizer::HeuristicTokenizer;

use crate::metrics::Metrics;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::from_env();
    let otel_provider = init_telemetry(&cfg);
    tracing::info!(?cfg.proxy_listen, ?cfg.admin_listen, "starting obleth gateway");

    // ---- connect dependencies (with simple boot-time retries) ----
    let store = retry("postgres", || Store::connect(&cfg.database_url)).await?;
    store.migrate().await?;
    // Provision the reserved control-plane identity (Charo) — idempotent.
    store.ensure_control_plane_identity().await?;
    tracing::info!("postgres connected + schema applied");

    let redis = retry("redis", || {
        RedisStore::connect_with(&cfg.redis_url, cfg.redis_timeouts)
    })
    .await?;
    tracing::info!("redis connected");

    // Not wrapped in `retry`: `start` only errs on a config problem (an
    // invalid database name) that retrying can't fix, and an unreachable
    // ClickHouse is not an error here — the sink comes up in spill mode and
    // the flusher retries schema setup on its own (see obleth-telemetry).
    let telemetry = TelemetrySink::start(
        &cfg.clickhouse_url,
        &cfg.clickhouse_db,
        &cfg.clickhouse_user,
        &cfg.clickhouse_password,
        &cfg.wal_path,
        cfg.fail_open,
    )
    .await?;
    if telemetry.schema_ready() {
        tracing::info!("clickhouse connected + schema applied");
    } else {
        tracing::warn!(
            "telemetry started in spill mode; ClickHouse schema deferred until reachable"
        );
    }

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
    let fairshare = FairShare::start(
        capacity.clone(),
        cfg.fairshare_algorithm,
        cfg.default_model_max_in_flight,
    );
    // ---- fairshare history (dashboard activity chart) ----
    let history_len = if cfg.fairshare_history_secs == 0 {
        0
    } else {
        (cfg.fairshare_history_secs.saturating_mul(1000) / FAIRSHARE_HISTORY_INTERVAL_MS).max(1)
            as usize
    };
    let fairshare_history = Arc::new(FairshareHistory::new(history_len));
    if history_len > 0 {
        spawn_fairshare_history_sampler(fairshare.clone(), fairshare_history.clone());
    }
    // The ceiling is a safety guard, not a fairness budget: set below the sum
    // of the pools it silently caps the fleet and hides the pool sizes the
    // operator configured.
    match store.list_models().await {
        Ok(models) => {
            let pool_sum =
                obleth_admin::enabled_pool_capacity(&models, cfg.default_model_max_in_flight);
            if cfg.global_max_in_flight < pool_sum {
                tracing::warn!(
                    ceiling = cfg.global_max_in_flight,
                    pool_sum,
                    "OBLETH_GLOBAL_MAX_IN_FLIGHT is below the sum of the enabled models' pool \
                     sizes; the ceiling will bind first and pools will be served round-robin \
                     — raise it above the pool sum"
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "failed to load models for the capacity check"),
    }

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
    // Discovered MCP tool lists for the gateway tool loop. Discovery runs on the
    // request's critical path (the tools must be injected before dispatch), so a
    // cache miss adds a full `tools/list` round trip to TTFT. Tool sets change
    // rarely, so cache them for 10 minutes: this keeps the discovery cost off all
    // but the first request to a tool-granted model (and the occasional refresh),
    // instead of re-paying it whenever requests arrive more than a minute apart.
    let tool_cache: Cache<String, Arc<Vec<boons::mcp_tools::McpTool>>> = Cache::builder()
        .time_to_live(Duration::from_secs(600))
        .max_capacity(1_000)
        .build();

    let model_registry = router::ModelRegistry::new();
    // Rolling completion-length averages, shared with the admin state below so
    // simulate scores with the live numbers.
    let output_stats = router::OutputStats::default();

    let (local_cache_tx, mut local_cache_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let key_cache_direct = key_cache.clone();
    tokio::spawn(async move {
        while let Some(hash) = local_cache_rx.recv().await {
            key_cache_direct.invalidate(&hash).await;
        }
    });

    // Upstream client. The idle-pool timeout is deliberately short (and
    // configurable): inference servers (llama.cpp, vLLM, SGLang) and any proxy in
    // front of them reap idle HTTP connections aggressively, so a long pool TTL
    // makes reqwest hand out sockets the server has already closed — the classic
    // "error sending request … connection closed" 502 race. tcp_keepalive keeps
    // live connections from being dropped by NAT/LBs. The data-plane dispatch
    // loop additionally retries a connection-level send error once on a fresh
    // connection (see proxy.rs), which absorbs the residual race.
    let mut http_builder =
        obleth_admin::ssrf::upstream_client_builder().pool_max_idle_per_host(256);
    if cfg.upstream_pool_idle_secs > 0 {
        http_builder =
            http_builder.pool_idle_timeout(Duration::from_secs(cfg.upstream_pool_idle_secs));
    } else {
        // 0 => never reuse an idle connection.
        http_builder = http_builder.pool_idle_timeout(Duration::ZERO);
    }
    if cfg.upstream_tcp_keepalive_secs > 0 {
        http_builder =
            http_builder.tcp_keepalive(Duration::from_secs(cfg.upstream_tcp_keepalive_secs));
    }
    // A backend that sends headers and then stalls would otherwise hold its
    // fairshare permit forever. reqwest's read timeout also bounds the wait
    // for response headers, so a non-streaming generation slower than this
    // fails even when the model's request timeout is longer.
    if cfg.upstream_read_timeout_secs > 0 {
        http_builder =
            http_builder.read_timeout(Duration::from_secs(cfg.upstream_read_timeout_secs));
    }
    let http = http_builder.build()?;

    // Auto-router classifier settings live in Postgres (app_settings,
    // key='auto_router') and are hot-reloadable. On boot, prefer saved settings;
    // otherwise seed from env so existing deployments work until configured.
    let initial_router_settings = match store.get_auto_router_settings().await {
        Ok(Some(settings)) => settings,
        Ok(None) => obleth_config::AutoRouterSettings {
            classifier_enabled: cfg.auto_classifier_enabled,
            classifier_model: cfg.auto_classifier_model.clone(),
            classifier_timeout_ms: cfg.auto_classifier_timeout_ms,
            ..Default::default()
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

    // Energy & carbon accounting settings live in Postgres (app_settings,
    // key='energy') and are hot-reloadable, mirroring the boon settings above.
    let initial_energy_settings = match store.get_energy_settings().await {
        Ok(Some(s)) => s,
        Ok(None) => obleth_config::EnergySettings::default(),
        Err(e) => {
            tracing::warn!(error = %e, "energy settings load failed; starting with defaults");
            obleth_config::EnergySettings::default()
        }
    };
    let energy = energy::EnergyEngine::new(initial_energy_settings);

    // In-process knowledge-base index: the request path may not read Postgres,
    // so the corpus is mirrored into memory and swapped wholesale, mirroring
    // the model registry/classifier/boons pattern above. Starts empty and
    // fills on the first refresh tick, so a large corpus never delays boot.
    let knowledge = Arc::new(knowledge::KnowledgeIndex::new());

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

    // JWT bearer auth: trusted issuers from OBLETH_JWT_ISSUERS. JWKS URLs pass
    // the same destination policy as any operator-supplied upstream, discovery
    // resolves `jwks_uri` for issuers configured without one, and the first
    // fetch happens here so the path is live before the listener opens. A
    // fetch failure at boot is logged (and alerted) but does not abort: the
    // background refresh keeps retrying and the key path is unaffected.
    let jwt = if cfg.jwt_issuers.is_empty() {
        None
    } else {
        let ssrf = obleth_admin::ssrf::SsrfPolicy::from_env();
        let mut issuers = cfg.jwt_issuers.clone();
        for i in issuers.iter_mut() {
            if i.jwks_url.is_none() {
                let url = format!(
                    "{}/.well-known/openid-configuration",
                    i.issuer.trim_end_matches('/')
                );
                if let Err(e) = ssrf.validate(&url).await {
                    anyhow::bail!(
                        "OBLETH_JWT_ISSUERS: issuer {} rejected by destination policy: {e}",
                        i.issuer
                    );
                }
                let doc = http
                    .get(&url)
                    .timeout(Duration::from_secs(10))
                    .send()
                    .await
                    .and_then(|r| r.error_for_status());
                match doc {
                    Ok(r) => match r.json::<serde_json::Value>().await {
                        Ok(v) => match v.get("jwks_uri").and_then(|u| u.as_str()) {
                            Some(u) => i.jwks_url = Some(u.to_string()),
                            None => anyhow::bail!("OBLETH_JWT_ISSUERS: discovery document for {} has no jwks_uri; set jwks_url explicitly", i.issuer),
                        },
                        Err(e) => anyhow::bail!("OBLETH_JWT_ISSUERS: discovery document for {} unreadable ({e}); set jwks_url explicitly", i.issuer),
                    },
                    Err(e) => anyhow::bail!("OBLETH_JWT_ISSUERS: discovery fetch for {} failed ({e}); set jwks_url explicitly", i.issuer),
                }
            }
            let jwks_url = i.jwks_url.as_deref().expect("resolved above");
            if let Err(e) = ssrf.validate(jwks_url).await {
                anyhow::bail!(
                    "OBLETH_JWT_ISSUERS: jwks_url {jwks_url} rejected by destination policy: {e}"
                );
            }
        }
        let verifier =
            jwt_auth::JwksVerifier::new(issuers, http.clone(), metrics.clone(), alerts.clone());
        for idx in 0..cfg.jwt_issuers.len() {
            if verifier.refresh_issuer(idx).await {
                tracing::info!(issuer = %cfg.jwt_issuers[idx].issuer, "jwks loaded");
            } else {
                tracing::warn!(issuer = %cfg.jwt_issuers[idx].issuer, "jwks not loaded at boot; tokens from this issuer are rejected until the next successful refresh");
            }
        }
        verifier.spawn_refresh();
        tracing::info!(issuers = cfg.jwt_issuers.len(), "jwt bearer auth enabled");
        Some(jwt_auth::JwtAuth::new(verifier, store.clone()))
    };

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
        tool_cache,
        model_registry: model_registry.clone(),
        classifier: classifier.clone(),
        output_stats: output_stats.clone(),
        boons: boons.clone(),
        metrics: metrics.clone(),
        fail_open: cfg.fail_open,
        alerts: alerts.clone(),
        session_id_derivation: std::env::var("OBLETH_SESSION_ID_DERIVATION")
            .map(|v| {
                !matches!(
                    v.trim().to_ascii_lowercase().as_str(),
                    "0" | "false" | "no" | "off"
                )
            })
            .unwrap_or(true),
        compressor: crate::boons::compressor::CompressorClient::from_env(),
        energy: energy.clone(),
        jwt,
        knowledge: knowledge.clone(),
    };

    match store.all_resolved_models().await {
        Ok(models) => {
            for (_, resolved) in &models {
                // One key per addressable name: the canonical `model_name` plus
                // every alias. Resolution stays a single lookup on whatever
                // name the client sent, so an alias costs nothing per request.
                let shared = Arc::new(resolved.clone());
                for name in resolved.addressable_names() {
                    if let Err(e) = redis.put_resolved_model(name, resolved).await {
                        tracing::warn!(error = %e, "failed to warm model into redis");
                    }
                    model_cache.insert(name.to_string(), shared.clone()).await;
                }
            }
            tracing::info!(count = models.len(), "warmed model cache");
            let tier_source = classifier.settings().tier_source;
            install_candidates(&model_registry, store.build_candidates(tier_source).await);
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
        energy.clone(),
        cfg.upstream_read_timeout_secs,
    );

    energy::spawn_energy_poller(energy.clone(), http.clone(), alerts.clone());

    // Keep the in-process knowledge index fresh: unchanged collections reuse
    // their existing `Arc` (see `refresh_once`), so this is cheap once the
    // corpus is stable. A Postgres error here leaves the last good index
    // serving rather than clearing it — see `KnowledgeIndex::refresh_once`.
    knowledge::KnowledgeIndex::spawn_refresh(
        knowledge.clone(),
        store.clone(),
        Duration::from_secs(15),
    );

    // The background document indexer (chunk -> embed -> commit generation)
    // has no other caller anywhere in the workspace: without this, an
    // uploaded document sits at `status = 'pending'` forever. Every replica
    // may run one — `for update skip locked` claiming, per-document
    // generations, and the staleness reclaim together make concurrent
    // indexers across replicas safe, so no singleton/leader election is
    // needed here.
    obleth_admin::knowledge::indexer::spawn_indexer(store.clone(), http.clone());

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

    // Keep discovered MCP tool lists warm in `tool_cache` so the request path
    // never pays a synchronous `tools/list` round trip (which would stall TTFT
    // and, on a slow/cold server, look like a hang). Refreshes every 5 min,
    // under the cache's 10-min TTL, so a granted server's tools never expire on
    // the hot path. Runs an immediate first pass at startup.
    spawn_tool_cache_prewarm(app_state.clone(), store.clone());

    // ---- pub/sub cache invalidation listener + Redis re-warm ----
    let redis_reconnected = Arc::new(tokio::sync::Notify::new());
    spawn_invalidation_listener(
        redis.clone(),
        InvalidationCaches {
            keys: key_cache.clone(),
            models: model_cache.clone(),
            mcp: mcp_cache.clone(),
        },
        redis_reconnected.clone(),
    );
    spawn_redis_rewarm(
        store.clone(),
        redis.clone(),
        (cfg.redis_rewarm_secs > 0)
            .then(|| Duration::from_secs(clamped_redis_rewarm_secs(cfg.redis_rewarm_secs))),
        redis_reconnected,
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
        telemetry: Some(telemetry.clone()),
        catalogs: Default::default(),
    };
    let admin_state = obleth_admin::AdminState {
        store: store.clone(),
        redis: redis.clone(),
        capacity: capacity.clone(),
        fairshare: fairshare.clone(),
        fairshare_stats: fairshare.stats(),
        default_model_max_in_flight: cfg.default_model_max_in_flight,
        fairshare_history: fairshare_history.clone(),
        fairshare_history_secs: cfg.fairshare_history_secs,
        clickhouse: clickhouse_read,
        admin_token: cfg.admin_token.clone(),
        health: health_runtime,
        usage_retention_default_days: cfg.usage_retention_days,
        ssrf: obleth_admin::ssrf::SsrfPolicy::from_env(),
        alerts: alerts.clone(),
        local_cache_tx: Some(local_cache_tx),
        output_stats: output_stats.clone(),
        // Simulate's opt-in real classification: same classifier instance,
        // same cache, same timeout as the data plane.
        classify: Some(std::sync::Arc::new({
            let st = app_state.clone();
            move |prompt: String, tags: Vec<String>| {
                let st = st.clone();
                Box::pin(async move { proxy::classify_for_simulate(&st, prompt, tags).await })
                    as std::pin::Pin<Box<dyn std::future::Future<Output = router::Intent> + Send>>
            }
        })),
    };
    obleth_admin::model_health::spawn_worker(admin_state.clone());
    obleth_admin::usage_retention::spawn_worker(admin_state.clone());
    let admin_app = obleth_admin::router(admin_state);

    // ---- routers ----
    let proxy_app = Router::new()
        .route("/health", get(|| async { "ok" }))
        // Native typed-verdict endpoint. Registered explicitly (not via the
        // fallback) so it is never mistaken for a passthrough path and proxied
        // verbatim to `{api_base}/verdicts`.
        .route(
            verdicts::VERDICTS_PATH,
            axum::routing::post(verdicts::handler),
        )
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

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    spawn_signal_listener(shutdown_tx);
    serve_until_shutdown(
        vec![
            (proxy_listener, proxy_app),
            (admin_listener, admin_app),
            (metrics_listener, metrics_app),
        ],
        shutdown_rx,
        cfg.shutdown_grace,
    )
    .await?;

    telemetry.shutdown().await;
    if let Some(provider) = otel_provider {
        // The exporter flush blocks; bound it so a dead collector can't hold exit.
        let flush = tokio::task::spawn_blocking(move || provider.shutdown());
        if tokio::time::timeout(Duration::from_secs(10), flush)
            .await
            .is_err()
        {
            // Dropping the runtime waits for blocking tasks, so a stuck flush
            // would hang here; everything else is already drained.
            tracing::warn!("trace export flush timed out; exiting");
            std::process::exit(0);
        }
    }
    tracing::info!("obleth gateway stopped");
    Ok(())
}

async fn shutdown_requested(mut rx: tokio::sync::watch::Receiver<bool>) {
    // A dropped sender also ends the wait, so a lost signal task can never
    // leave the servers unstoppable.
    let _ = rx.wait_for(|stop| *stop).await;
}

async fn wait_for_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "ctrl-c handler unavailable");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "SIGTERM handler unavailable");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }
}

/// First SIGTERM/ctrl-c starts the drain; a second one exits immediately.
fn spawn_signal_listener(tx: tokio::sync::watch::Sender<bool>) {
    tokio::spawn(async move {
        wait_for_signal().await;
        tracing::info!("shutdown signal received; waiting for in-flight connections");
        let _ = tx.send(true);
        wait_for_signal().await;
        tracing::warn!("second shutdown signal; exiting without draining");
        std::process::exit(130);
    });
}

/// Serve every listener until shutdown is requested, then stop accepting and
/// give in-flight requests (including open streams) up to `grace` to finish.
async fn serve_until_shutdown(
    servers: Vec<(tokio::net::TcpListener, Router)>,
    shutdown: tokio::sync::watch::Receiver<bool>,
    grace: Duration,
) -> anyhow::Result<()> {
    let running = futures::future::try_join_all(servers.into_iter().map(|(listener, app)| {
        let stop = shutdown_requested(shutdown.clone());
        async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(stop)
                .await
        }
    }));
    tokio::pin!(running);
    tokio::select! {
        res = &mut running => {
            res?;
            return Ok(());
        }
        _ = shutdown_requested(shutdown.clone()) => {}
    }
    match tokio::time::timeout(grace, &mut running).await {
        Ok(res) => {
            res?;
            tracing::info!("in-flight requests drained");
        }
        Err(_) => tracing::warn!(
            grace_secs = grace.as_secs(),
            "shutdown grace elapsed; remaining connections end at process exit"
        ),
    }
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

fn install_candidates(
    registry: &router::ModelRegistry,
    candidates: Result<Vec<router::Candidate>, obleth_store::StoreError>,
) {
    match candidates {
        Ok(candidates) => registry.store(candidates),
        Err(e) => {
            tracing::warn!(error = %e, "auto-router candidate refresh failed; retaining previous snapshot")
        }
    }
}

/// Model names whose configured `request_timeout_secs` exceeds the upstream
/// read timeout, with the configured value. Such a request is cut by the read
/// timeout — a plain connection error — before the model's own timeout would
/// ever fire, so the operator's per-model override is silently dead.
fn overlong_request_timeouts(
    candidates: &[router::Candidate],
    upstream_read_timeout_secs: u64,
) -> Vec<(String, i64)> {
    let mut offenders: Vec<(String, i64)> = candidates
        .iter()
        .filter_map(|c| {
            let configured = c.model.request_timeout_secs?;
            (configured > upstream_read_timeout_secs as i64)
                .then(|| (c.model.model_name.clone(), configured))
        })
        .collect();
    offenders.sort();
    offenders
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
    energy: energy::EnergyEngine,
    upstream_read_timeout_secs: u64,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(15));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Names last warned about, so an unchanged offending set doesn't
        // repeat the warning every 15s tick; only a change (new offender,
        // one fixed) logs again.
        let mut warned: std::collections::HashSet<String> = std::collections::HashSet::new();
        loop {
            tick.tick().await;
            // Refresh auto-router settings (including `tier_source`) before
            // rebuilding candidates, so a tier-source change takes effect on
            // this tick's `derive_levels` call rather than one tick late.
            match store.get_auto_router_settings().await {
                Ok(Some(settings)) => classifier.update(settings),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "auto-router settings refresh failed"),
            }
            let tier_source = classifier.settings().tier_source;
            install_candidates(&registry, store.build_candidates(tier_source).await);
            let offenders =
                overlong_request_timeouts(registry.load().as_slice(), upstream_read_timeout_secs);
            let current: std::collections::HashSet<String> =
                offenders.iter().map(|(name, _)| name.clone()).collect();
            if current != warned {
                for (name, configured) in &offenders {
                    tracing::warn!(
                        model = %name,
                        request_timeout_secs = configured,
                        read_timeout_secs = upstream_read_timeout_secs,
                        "requests to {name} will be cut at {upstream_read_timeout_secs}s by the read timeout"
                    );
                }
                warned = current;
            }
            match store.get_boon_settings().await {
                Ok(Some(settings)) => boons.update(settings),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "boon settings refresh failed"),
            }
            match store.get_energy_settings().await {
                Ok(Some(settings)) => energy.update(settings),
                Ok(None) => {}
                Err(e) => tracing::warn!(error = %e, "energy settings refresh failed"),
            }
        }
    });
}

/// Sample the scheduler every `FAIRSHARE_HISTORY_INTERVAL_MS` into the
/// in-memory history ring. A failed sample (scheduler gone) skips the tick.
fn spawn_fairshare_history_sampler(fairshare: FairShare, history: Arc<FairshareHistory>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(FAIRSHARE_HISTORY_INTERVAL_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            if let Some(sample) = fairshare.sample().await {
                history.push(sample);
            }
        }
    });
}

/// Background pre-warm of the gateway tool loop's `tool_cache`. Discovery
/// (`initialize` + `tools/list`) is done here, off the request path, so a user
/// request to a tool-granted model is always a cache hit. A granted server that
/// fails discovery is alerted on (instead of failing open silently) because the
/// model then receives no tools and typically claims it cannot use them.
fn spawn_tool_cache_prewarm(state: AppState, store: Store) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            // Nothing to discover when the loop is off; don't poke MCP servers.
            if !state.boons.settings().tool_loop.active() {
                continue;
            }
            let models = match store.all_resolved_models().await {
                Ok(m) => m,
                Err(e) => {
                    tracing::warn!(error = %e, "tool-cache prewarm: model load failed");
                    continue;
                }
            };
            // Unique set of servers granted to any enabled model.
            let mut servers: std::collections::HashSet<String> = std::collections::HashSet::new();
            for (_, model) in &models {
                if !model.enabled {
                    continue;
                }
                for s in &model.tool_servers {
                    servers.insert(s.clone());
                }
            }
            for name in servers {
                let Some(server) = mcp::resolve_mcp(&state, &name).await else {
                    continue;
                };
                if !server.enabled {
                    continue;
                }
                match boons::mcp_tools::list_tools(&state, server.as_ref(), Duration::from_secs(10))
                    .await
                {
                    Ok(tools) => {
                        tracing::debug!(server = %name, count = tools.len(), "tool-cache prewarmed");
                        state.tool_cache.insert(name.clone(), Arc::new(tools)).await;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, server = %name, "tool-cache prewarm discovery failed");
                        state.alerts.issue(
                            "mcp_tool_discovery_failed",
                            "MCP tool discovery failed",
                            format!(
                                "granted MCP server `{name}` could not be discovered: {e}. \
                                 Models granted this server will receive no tools and may \
                                 report that they cannot use them."
                            ),
                        );
                    }
                }
            }
        }
    });
}

#[derive(Clone)]
struct InvalidationCaches {
    keys: Cache<String, Arc<obleth_config::ResolvedKey>>,
    models: Cache<String, Arc<obleth_config::ResolvedModel>>,
    mcp: Cache<String, Arc<obleth_config::ResolvedMcpServer>>,
}

impl InvalidationCaches {
    fn invalidate_all(&self) {
        self.keys.invalidate_all();
        self.models.invalidate_all();
        self.mcp.invalidate_all();
    }

    async fn apply(&self, target: &str) {
        if target == "*" {
            self.invalidate_all();
        } else if let Some(name) = target.strip_prefix("model:") {
            self.models.invalidate(name).await;
        } else if let Some(name) = target.strip_prefix("mcp:") {
            self.mcp.invalidate(name).await;
        } else {
            self.keys.invalidate(target).await;
        }
    }
}

fn spawn_invalidation_listener(
    redis: RedisStore,
    caches: InvalidationCaches,
    reconnected: Arc<tokio::sync::Notify>,
) {
    tokio::spawn(async move {
        let on_message = {
            let caches = caches.clone();
            move |target: String| {
                let caches = caches.clone();
                tokio::spawn(async move { caches.apply(&target).await });
            }
        };
        let on_subscribed = move |resubscribed: bool| {
            // Anything published while disconnected was missed, and Redis
            // itself may have lost data, so drop the in-process copies and
            // re-push the source of truth.
            if resubscribed {
                caches.invalidate_all();
                reconnected.notify_one();
                tracing::info!("invalidation listener resubscribed");
            }
        };
        redis
            .run_invalidation_listener(on_message, on_subscribed)
            .await;
    });
}

/// `Instant::now() + period` panics on overflow, which would silently kill
/// the re-warm task (and the reconnect re-warm with it, since both run in
/// the same spawned task) for a misconfigured `OBLETH_REDIS_REWARM_SECS`. The
/// re-warm is a self-heal safety net, not a latency-sensitive schedule, so a
/// day between passes is still a safe ceiling.
const MAX_REDIS_REWARM_SECS: u64 = 86_400;

fn clamped_redis_rewarm_secs(configured: u64) -> u64 {
    if configured > MAX_REDIS_REWARM_SECS {
        tracing::warn!(
            configured,
            clamped = MAX_REDIS_REWARM_SECS,
            "OBLETH_REDIS_REWARM_SECS exceeds the maximum; clamping"
        );
        MAX_REDIS_REWARM_SECS
    } else {
        configured
    }
}

/// Re-push keys, models and MCP servers from Postgres into Redis on a timer
/// and after every pub/sub reconnect, so a Redis restart or flush heals
/// without restarting the gateway. Background only; the request path never
/// waits on it.
fn spawn_redis_rewarm(
    store: Store,
    redis: RedisStore,
    every: Option<Duration>,
    reconnected: Arc<tokio::sync::Notify>,
) {
    tokio::spawn(async move {
        let mut tick = every.map(|period| {
            let mut t = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
            t.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            t
        });
        loop {
            let periodic = async {
                match tick.as_mut() {
                    Some(t) => {
                        t.tick().await;
                    }
                    None => std::future::pending::<()>().await,
                }
            };
            tokio::select! {
                _ = periodic => {}
                _ = reconnected.notified() => {}
            }
            rewarm_redis(&store, &redis).await;
        }
    });
}

async fn rewarm_redis(store: &Store, redis: &RedisStore) {
    let keys = rewarm_keys(store, redis).await;
    let models = rewarm_models(store, redis).await;
    let mcp = rewarm_mcp_servers(store, redis).await;
    tracing::debug!(?keys, ?models, ?mcp, "redis re-warm pass complete");
}

/// From a snapshot's `known` identifiers (hashes or names) and what the
/// SCAN-based prune actually deleted (`pruned`), decide what a fresh
/// post-prune reload should restore vs. evict. An identifier still present in
/// `fresh` is restored — the prune deleted a live entry, most likely one
/// created between the snapshot load and the SCAN (item N1). Anything still
/// absent is evicted, which covers both a `pruned` identifier confirmed gone
/// for good and a `known` identifier deleted mid-pass: the prune itself
/// leaves the latter alone (it still matched `known` at SCAN time), so this
/// is the only place that catches it. Pure and type-agnostic (keys, models
/// and MCP servers all key their resolver cache by plain strings), so it's
/// unit-tested directly instead of only through a live Redis.
fn restore_and_evict(
    known: &std::collections::HashSet<String>,
    pruned: &std::collections::HashSet<String>,
    fresh: &std::collections::HashSet<&str>,
) -> (Vec<String>, Vec<String>) {
    let restore = pruned
        .iter()
        .filter(|id| fresh.contains(id.as_str()))
        .cloned()
        .collect();
    let evict = pruned
        .iter()
        .chain(known.iter())
        .filter(|id| !fresh.contains(id.as_str()))
        .cloned()
        .collect();
    (restore, evict)
}

/// Push a Postgres snapshot into Redis, then delete any `obleth:key:*` entry
/// Redis holds that the snapshot doesn't list — the same SCAN-based prune
/// `POST /api/v1/resync` uses (`obleth-redis/src/prune.rs`). A key revoked
/// before this snapshot was taken has no entry here, so the prune removes it
/// on this pass; a key revoked mid-pass survives until the next pass, which
/// repeats the full push + prune and removes it then. Any failure (a push or
/// the prune itself) ends the pass with a warn and nothing is written further
/// — the next pass 300s later starts clean rather than patching over a
/// partial one, so a revoked key is never left resurrected for good.
///
/// A key an admin *creates* between the snapshot load above and the SCAN is
/// the opposite problem: it isn't in `snapshot`, so the prune deletes it as
/// if it were stale and every replica 401s it until the next pass (up to
/// 300s). When the prune actually removed something, re-read Postgres once
/// more (same fix-up `POST /api/v1/resync`'s `resync_all_keys` applies) and
/// restore anything that reload shows still exists; anything that's still
/// gone — including a `known` entry deleted mid-pass, which the prune above
/// left alone because it still matched `known` — is evicted so Redis matches
/// the source of truth either way. A failure here is only logged: the pushed
/// entries and the prune already landed, so the pass has already done useful
/// work, and the next pass repeats this fix-up too.
///
/// `reload` is `Store::all_resolved_keys` in production; taking it as a
/// closure (rather than `&Store` directly) keeps the restore/evict logic
/// testable against Redis alone, with no Postgres in the loop.
async fn push_and_prune_keys<F, Fut>(
    redis: &RedisStore,
    snapshot: &[(String, obleth_config::ResolvedKey)],
    reload: F,
) -> Option<usize>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<
        Output = std::result::Result<
            Vec<(String, obleth_config::ResolvedKey)>,
            obleth_store::StoreError,
        >,
    >,
{
    for (hash, key) in snapshot {
        if let Err(e) = redis.put_resolved_key(hash, key).await {
            tracing::warn!(error = %e, "redis re-warm: key push failed; retrying next pass");
            return None;
        }
    }
    let known: std::collections::HashSet<String> =
        snapshot.iter().map(|(hash, _)| hash.clone()).collect();
    let pruned: std::collections::HashSet<String> =
        match redis.prune_stale_resolved_keys(&known).await {
            Ok(p) => p.into_iter().collect(),
            Err(e) => {
                tracing::warn!(error = %e, "redis re-warm: key prune failed; retrying next pass");
                return None;
            }
        };
    if pruned.is_empty() {
        return Some(snapshot.len());
    }
    let fresh = match reload().await {
        Ok(f) => f,
        Err(e) => {
            tracing::warn!(error = %e, "redis re-warm: key re-read after prune failed; a wrongly pruned key may 401 until the next pass");
            for hash in &pruned {
                let _ = redis.publish_invalidation(hash).await;
            }
            return Some(snapshot.len());
        }
    };
    let fresh_hashes: std::collections::HashSet<&str> =
        fresh.iter().map(|(h, _)| h.as_str()).collect();
    let fresh_by_hash: std::collections::HashMap<&str, &obleth_config::ResolvedKey> =
        fresh.iter().map(|(h, k)| (h.as_str(), k)).collect();
    let (restore, evict) = restore_and_evict(&known, &pruned, &fresh_hashes);
    for hash in &restore {
        let Some(key) = fresh_by_hash.get(hash.as_str()) else {
            continue;
        };
        if let Err(e) = redis.put_resolved_key(hash, key).await {
            tracing::warn!(error = %e, id = %hash, "redis re-warm: restoring a wrongly pruned key failed");
            continue;
        }
        let _ = redis.publish_invalidation(hash).await;
    }
    for hash in &evict {
        let _ = redis.delete_resolved_key(hash).await;
        let _ = redis.publish_invalidation(hash).await;
    }
    Some(fresh.len())
}

async fn rewarm_keys(store: &Store, redis: &RedisStore) -> Option<usize> {
    let snapshot = match store.all_resolved_keys().await {
        Ok(k) => k,
        Err(e) => {
            tracing::warn!(error = %e, "redis re-warm: key load failed");
            return None;
        }
    };
    push_and_prune_keys(redis, &snapshot, || store.all_resolved_keys()).await
}

/// Resolved models keyed by every addressable name (canonical name + aliases).
fn models_by_name(
    models: Vec<(String, obleth_config::ResolvedModel)>,
) -> Vec<(String, obleth_config::ResolvedModel)> {
    models
        .into_iter()
        .flat_map(|(_, m)| {
            m.addressable_names()
                .map(|n| (n.to_string(), m.clone()))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Same push-then-prune contract as [`push_and_prune_keys`], for the
/// `obleth:model:*` namespace, including the same post-prune restore/evict
/// fix-up for a model created (or renamed to a new alias) between the
/// snapshot load and the SCAN. `snapshot` is keyed by every addressable name
/// ([`models_by_name`]), which is also what the prune treats as known. See
/// [`push_and_prune_keys`] for why `reload` is a closure rather than `&Store`.
async fn push_and_prune_models<F, Fut>(
    redis: &RedisStore,
    snapshot: &[(String, obleth_config::ResolvedModel)],
    reload: F,
) -> Option<usize>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<
        Output = std::result::Result<
            Vec<(String, obleth_config::ResolvedModel)>,
            obleth_store::StoreError,
        >,
    >,
{
    for (name, model) in snapshot {
        if let Err(e) = redis.put_resolved_model(name, model).await {
            tracing::warn!(error = %e, "redis re-warm: model push failed; retrying next pass");
            return None;
        }
    }
    let known: std::collections::HashSet<String> =
        snapshot.iter().map(|(name, _)| name.clone()).collect();
    let pruned: std::collections::HashSet<String> =
        match redis.prune_stale_resolved_models(&known).await {
            Ok(p) => p.into_iter().collect(),
            Err(e) => {
                tracing::warn!(error = %e, "redis re-warm: model prune failed; retrying next pass");
                return None;
            }
        };
    if pruned.is_empty() {
        return Some(snapshot.len());
    }
    let fresh = match reload().await {
        Ok(m) => m,
        Err(e) => {
            tracing::warn!(error = %e, "redis re-warm: model re-read after prune failed; a wrongly pruned model may 404 until the next pass");
            for name in &pruned {
                let _ = redis.publish_invalidation(&format!("model:{name}")).await;
            }
            return Some(snapshot.len());
        }
    };
    let fresh_names: std::collections::HashSet<&str> =
        fresh.iter().map(|(n, _)| n.as_str()).collect();
    let fresh_by_name: std::collections::HashMap<&str, &obleth_config::ResolvedModel> =
        fresh.iter().map(|(n, m)| (n.as_str(), m)).collect();
    let (restore, evict) = restore_and_evict(&known, &pruned, &fresh_names);
    for name in &restore {
        let Some(model) = fresh_by_name.get(name.as_str()) else {
            continue;
        };
        if let Err(e) = redis.put_resolved_model(name, model).await {
            tracing::warn!(error = %e, id = %name, "redis re-warm: restoring a wrongly pruned model failed");
            continue;
        }
        let _ = redis.publish_invalidation(&format!("model:{name}")).await;
    }
    for name in &evict {
        let _ = redis.delete_resolved_model(name).await;
        let _ = redis.publish_invalidation(&format!("model:{name}")).await;
    }
    Some(fresh.len())
}

async fn rewarm_models(store: &Store, redis: &RedisStore) -> Option<usize> {
    let snapshot = match store.all_resolved_models().await {
        Ok(m) => models_by_name(m),
        Err(e) => {
            tracing::warn!(error = %e, "redis re-warm: model load failed");
            return None;
        }
    };
    push_and_prune_models(redis, &snapshot, || async {
        store.all_resolved_models().await.map(models_by_name)
    })
    .await
}

/// Same push-then-prune contract as [`push_and_prune_keys`], for the
/// `obleth:mcp:*` namespace, including the same post-prune restore/evict
/// fix-up for an MCP server registered between the snapshot load and the SCAN.
/// See [`push_and_prune_keys`] for why `reload` is a closure rather than `&Store`.
async fn push_and_prune_mcp_servers<F, Fut>(
    redis: &RedisStore,
    snapshot: &[(String, obleth_config::ResolvedMcpServer)],
    reload: F,
) -> Option<usize>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<
        Output = std::result::Result<
            Vec<(String, obleth_config::ResolvedMcpServer)>,
            obleth_store::StoreError,
        >,
    >,
{
    for (name, server) in snapshot {
        if let Err(e) = redis.put_resolved_mcp_server(name, server).await {
            tracing::warn!(error = %e, "redis re-warm: mcp server push failed; retrying next pass");
            return None;
        }
    }
    let known: std::collections::HashSet<String> =
        snapshot.iter().map(|(name, _)| name.clone()).collect();
    let pruned: std::collections::HashSet<String> = match redis
        .prune_stale_resolved_mcp_servers(&known)
        .await
    {
        Ok(p) => p.into_iter().collect(),
        Err(e) => {
            tracing::warn!(error = %e, "redis re-warm: mcp server prune failed; retrying next pass");
            return None;
        }
    };
    if pruned.is_empty() {
        return Some(snapshot.len());
    }
    let fresh = match reload().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "redis re-warm: mcp server re-read after prune failed; a wrongly pruned server may be unusable until the next pass");
            for name in &pruned {
                let _ = redis.publish_invalidation(&format!("mcp:{name}")).await;
            }
            return Some(snapshot.len());
        }
    };
    let fresh_names: std::collections::HashSet<&str> =
        fresh.iter().map(|(n, _)| n.as_str()).collect();
    let fresh_by_name: std::collections::HashMap<&str, &obleth_config::ResolvedMcpServer> =
        fresh.iter().map(|(n, s)| (n.as_str(), s)).collect();
    let (restore, evict) = restore_and_evict(&known, &pruned, &fresh_names);
    for name in &restore {
        let Some(server) = fresh_by_name.get(name.as_str()) else {
            continue;
        };
        if let Err(e) = redis.put_resolved_mcp_server(name, server).await {
            tracing::warn!(error = %e, id = %name, "redis re-warm: restoring a wrongly pruned mcp server failed");
            continue;
        }
        let _ = redis.publish_invalidation(&format!("mcp:{name}")).await;
    }
    for name in &evict {
        let _ = redis.delete_resolved_mcp_server(name).await;
        let _ = redis.publish_invalidation(&format!("mcp:{name}")).await;
    }
    Some(fresh.len())
}

async fn rewarm_mcp_servers(store: &Store, redis: &RedisStore) -> Option<usize> {
    let snapshot = match store.all_resolved_mcp_servers().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "redis re-warm: mcp server load failed");
            return None;
        }
    };
    push_and_prune_mcp_servers(redis, &snapshot, || store.all_resolved_mcp_servers()).await
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

#[cfg(test)]
mod registry_refresh_tests {
    use super::*;

    #[test]
    fn failed_health_query_retains_snapshot_and_success_replaces_it() {
        let registry = router::ModelRegistry::new();
        let before = registry.load();
        install_candidates(&registry, Err(obleth_store::StoreError::NotFound));
        assert!(Arc::ptr_eq(&before, &registry.load()));
        install_candidates(&registry, Ok(Vec::new()));
        assert!(!Arc::ptr_eq(&before, &registry.load()));
    }

    fn model(name: &str, request_timeout_secs: Option<i64>) -> obleth_config::ResolvedModel {
        obleth_config::ResolvedModel {
            model_name: name.to_string(),
            aliases: Vec::new(),
            upstream_model: name.to_string(),
            api_base: "http://upstream".to_string(),
            api_key: None,
            model_type: obleth_config::DEFAULT_MODEL_TYPE.to_string(),
            quantization: obleth_config::DEFAULT_QUANTIZATION.to_string(),
            admission_weight: 100,
            max_in_flight: None,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 0,
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            context_window: 128_000,
            supports_function_calling: true,
            supports_system_messages: true,
            supports_response_schema: true,
            supports_tool_choice: true,
            supports_vision: false,
            tags: Vec::new(),
            declared_levels: Vec::new(),
            boons: Vec::new(),
            tool_servers: Vec::new(),
            knowledge_collections: Vec::new(),
            request_timeout_secs,
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

    fn candidate(name: &str, request_timeout_secs: Option<i64>) -> router::Candidate {
        router::Candidate {
            model: model(name, request_timeout_secs),
            healthy: true,
            levels: Vec::new(),
        }
    }

    #[test]
    fn overlong_request_timeouts_flags_only_models_exceeding_the_read_timeout() {
        let candidates = vec![
            candidate("short", Some(60)),
            candidate("no-override", None),
            candidate("long", Some(600)),
            candidate("exactly-at-limit", Some(300)),
        ];
        let offenders = overlong_request_timeouts(&candidates, 300);
        assert_eq!(offenders, vec![("long".to_string(), 600)]);
    }
}

#[cfg(test)]
mod shutdown_tests {
    use super::*;

    async fn bind() -> (tokio::net::TcpListener, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        (listener, url)
    }

    /// `/slow` signals `started` once its handler runs, so a test can trigger
    /// shutdown while the request is definitely in flight.
    fn slow_app(delay: Duration, started: Arc<tokio::sync::Notify>) -> Router {
        Router::new()
            .route("/health", get(|| async { "ok" }))
            .route(
                "/slow",
                get(move || {
                    let started = started.clone();
                    async move {
                        started.notify_one();
                        tokio::time::sleep(delay).await;
                        "done"
                    }
                }),
            )
    }

    #[tokio::test]
    async fn servers_exit_cleanly_when_shutdown_is_triggered() {
        let (a, a_url) = bind().await;
        let (b, b_url) = bind().await;
        let (c, c_url) = bind().await;
        let (tx, rx) = tokio::sync::watch::channel(false);
        let server = tokio::spawn(serve_until_shutdown(
            vec![
                (a, slow_app(Duration::ZERO, Default::default())),
                (b, slow_app(Duration::ZERO, Default::default())),
                (c, slow_app(Duration::ZERO, Default::default())),
            ],
            rx,
            Duration::from_secs(5),
        ));
        let http = reqwest::Client::new();
        for url in [&a_url, &b_url, &c_url] {
            let body = http.get(format!("{url}/health")).send().await.unwrap();
            assert_eq!(body.text().await.unwrap(), "ok");
        }
        tx.send(true).unwrap();
        let res = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("servers stop promptly")
            .unwrap();
        assert!(res.is_ok());
        assert!(
            http.get(format!("{a_url}/health")).send().await.is_err(),
            "listener no longer accepts"
        );
    }

    #[tokio::test]
    async fn in_flight_request_finishes_before_exit() {
        let (a, a_url) = bind().await;
        let (tx, rx) = tokio::sync::watch::channel(false);
        let started = Arc::new(tokio::sync::Notify::new());
        let server = tokio::spawn(serve_until_shutdown(
            vec![(a, slow_app(Duration::from_millis(500), started.clone()))],
            rx,
            Duration::from_secs(5),
        ));
        let req =
            tokio::spawn(async move { reqwest::get(format!("{a_url}/slow")).await?.text().await });
        started.notified().await;
        tx.send(true).unwrap();
        assert_eq!(req.await.unwrap().unwrap(), "done");
        assert!(server.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn grace_bounds_a_request_that_never_finishes() {
        let (a, a_url) = bind().await;
        let (tx, rx) = tokio::sync::watch::channel(false);
        let started = Arc::new(tokio::sync::Notify::new());
        let server = tokio::spawn(serve_until_shutdown(
            vec![(a, slow_app(Duration::from_secs(600), started.clone()))],
            rx,
            Duration::from_millis(200),
        ));
        let _req = tokio::spawn(reqwest::get(format!("{a_url}/slow")));
        started.notified().await;
        tx.send(true).unwrap();
        let res = tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("grace elapsed")
            .unwrap();
        assert!(res.is_ok());
    }

    /// Pure, always-on coverage of the restore/evict decision itself — the
    /// actual logic behind both I1 (a revoked key must not come back) and N1
    /// (a key created mid-pass must not be wrongly evicted). This is the
    /// primary proof: it needs no Redis and can't leave any shared state
    /// behind, unlike the SCAN-based prune the integration test below drives
    /// for real (see that test's doc comment for why it's careful about it).
    #[test]
    fn restore_and_evict_keeps_a_mid_pass_create_and_drops_a_genuine_revocation() {
        use std::collections::HashSet;
        let known: HashSet<String> = ["kept", "deleted-mid-pass"]
            .into_iter()
            .map(String::from)
            .collect();
        // `pruned` is what the SCAN removed: `created-mid-pass` (wrongly —
        // it's not in `known` only because the snapshot predates it) and
        // `revoked` (correctly — Postgres dropped it before the snapshot).
        let pruned: HashSet<String> = ["created-mid-pass", "revoked"]
            .into_iter()
            .map(String::from)
            .collect();
        // The post-prune reload: `kept` still there, `created-mid-pass` now
        // visible, `deleted-mid-pass` gone (an admin delete that landed after
        // the snapshot but that the prune couldn't see, since it still
        // matched `known`), `revoked` still gone.
        let fresh: HashSet<&str> = ["kept", "created-mid-pass"].into_iter().collect();

        let (mut restore, mut evict) = restore_and_evict(&known, &pruned, &fresh);
        restore.sort();
        evict.sort();
        assert_eq!(restore, vec!["created-mid-pass".to_string()]);
        assert_eq!(
            evict,
            vec!["deleted-mid-pass".to_string(), "revoked".to_string()]
        );
    }

    #[test]
    fn restore_and_evict_is_empty_when_the_prune_removed_nothing() {
        use std::collections::HashSet;
        let known: HashSet<String> = ["kept"].into_iter().map(String::from).collect();
        let pruned: HashSet<String> = HashSet::new();
        let fresh: HashSet<&str> = ["kept"].into_iter().collect();
        assert_eq!(
            restore_and_evict(&known, &pruned, &fresh),
            (Vec::new(), Vec::new())
        );
    }

    /// Integration smoke test for the real Redis plumbing behind both
    /// scenarios `restore_and_evict_keeps_a_mid_pass_create_and_drops_a_genuine_revocation`
    /// covers in isolation above.
    ///
    /// `push_and_prune_keys` calls `RedisStore::prune_stale_resolved_keys`,
    /// which SCANs and deletes across the *entire* `obleth:key:*` namespace —
    /// not just the hashes this test creates. Point `OBLETH_TEST_REDIS_URL`
    /// at an isolated database (e.g. Redis db 15, the project's documented
    /// test database — never db 0 / a shared or live instance): running this
    /// against a real deployment's default database deletes its live
    /// resolved-key cache. Both scenarios are folded into one `#[tokio::test]`
    /// (rather than two, as originally written) because two such tests
    /// running in parallel — the default for `cargo test` — race the same
    /// SCAN and can prune each other's freshly-written keys.
    #[tokio::test]
    async fn push_and_prune_keys_integration() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let redis = obleth_redis::RedisStore::connect(&url)
            .await
            .expect("connect");

        // Scenario 1 (I1): revoked before the snapshot was loaded, absent
        // from both the snapshot and the post-prune reload — stays evicted.
        let revoked_hash = format!("test-rewarm-{}", uuid::Uuid::new_v4());
        redis
            .put_resolved_key(&revoked_hash, &test_resolved_key())
            .await
            .unwrap();

        // Scenario 2 (N1): created between the snapshot load and the SCAN —
        // already in Redis (the admin create path pushes synchronously) but
        // missing from the snapshot; the post-prune reload shows it, so it
        // must be restored.
        let created_hash = format!("test-rewarm-{}", uuid::Uuid::new_v4());
        let created_key = test_resolved_key();
        redis
            .put_resolved_key(&created_hash, &created_key)
            .await
            .unwrap();

        let kept_hash = format!("test-rewarm-{}", uuid::Uuid::new_v4());
        let kept_key = test_resolved_key();
        redis.put_resolved_key(&kept_hash, &kept_key).await.unwrap();

        // The pass's own snapshot predates the create: only `kept` and
        // (already-gone-from-Postgres) nothing for `revoked_hash`.
        let snapshot = vec![(kept_hash.clone(), kept_key.clone())];
        // The post-prune reload runs later: Postgres now also has `created`,
        // and still doesn't have `revoked`.
        let reread = vec![
            (kept_hash.clone(), kept_key),
            (created_hash.clone(), created_key),
        ];

        let pushed =
            push_and_prune_keys(&redis, &snapshot, move || async move { Ok(reread) }).await;
        assert_eq!(pushed, Some(2));
        assert!(
            redis
                .get_resolved_key(&revoked_hash)
                .await
                .unwrap()
                .is_none(),
            "a key revoked before the snapshot must stay evicted"
        );
        assert!(
            redis
                .get_resolved_key(&created_hash)
                .await
                .unwrap()
                .is_some(),
            "a key created during the pass must survive it"
        );
        assert!(redis.get_resolved_key(&kept_hash).await.unwrap().is_some());

        redis.delete_resolved_key(&created_hash).await.unwrap();
        redis.delete_resolved_key(&kept_hash).await.unwrap();
    }

    fn test_resolved_key() -> obleth_config::ResolvedKey {
        obleth_config::ResolvedKey {
            key_id: uuid::Uuid::new_v4(),
            tenant_id: uuid::Uuid::new_v4(),
            tenant_name: "t".into(),
            fairshare_group: "default".into(),
            group_weight: 100,
            weight: 7,
            tokens_per_minute: 1000,
            max_in_flight: None,
            disabled: false,
            status: "active".into(),
            timezone: "UTC".into(),
            active_from: None,
            active_until: None,
            weekly_windows: None,
            budget_tokens: None,
            budget_cost_usd: None,
            budget_period: None,
            budget_started_at: None,
            key_budget_tokens: None,
            key_budget_cost_usd: None,
            key_budget_period: None,
            key_budget_started_at: None,
            key_weight: 100,
            key_max_in_flight: None,
            allowed_models: None,
            internal: false,
            tracing_enabled: false,
            guardrails_policy: None,
            compression_policy: None,
            synthetic: false,
        }
    }

    #[test]
    fn redis_rewarm_secs_is_clamped_to_the_maximum() {
        assert_eq!(clamped_redis_rewarm_secs(300), 300);
        assert_eq!(clamped_redis_rewarm_secs(u64::MAX), MAX_REDIS_REWARM_SECS);
    }
}
