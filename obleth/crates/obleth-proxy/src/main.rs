//! obleth gateway data-plane binary.
//!
//! Boots three listeners: the data-plane proxy, the Management API (admin), and
//! a Prometheus metrics endpoint. Wires Postgres (config SoT), Redis (hot cache
//! + budgets), ClickHouse (usage ledger) and the fairshare scheduler together.

mod batches;
mod completion;
mod crash_guard;
mod energy;
mod jobs;
mod jwt_auth;
mod key_miss;
mod knowledge;
mod lifecycle;
mod load;
mod mcp;
mod messages;
mod metrics;
mod object_store;
mod output_monitor;
mod proxy;
mod reasoning;
mod replicas;
mod responses;
mod router;
mod search;
mod speech;
mod sse;
mod state;
mod sticky;
mod tool_slots;
mod upstream_failure;
mod verdicts;

mod boons;
mod classifier;
mod client;
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
use obleth_redis::{RedisStore, Resolver};
use obleth_store::Store;
use obleth_telemetry::{TelemetrySink, TelemetryStats};
use obleth_tokenizer::HeuristicTokenizer;

use crate::metrics::Metrics;
use crate::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cfg = Config::from_env()?;
    let otel_provider = init_telemetry(&cfg);
    tracing::info!(?cfg.proxy_listen, ?cfg.admin_listen, "starting obleth gateway");
    for warning in &cfg.warnings {
        tracing::warn!("{warning}");
    }
    // Settings read deep in the request path, from the one configuration.
    client::install(cfg.client_address.clone());
    batches::install(cfg.batches.clone());
    boons::tool_stream::install_stream_idle_timeout(cfg.upstream_read_timeout_secs);
    proxy::install_admission_timeout(cfg.admission_timeout);
    // One destination policy for every operator-supplied URL.
    let ssrf = obleth_admin::ssrf::SsrfPolicy::new(&cfg.ssrf);

    // ---- connect dependencies (with simple boot-time retries) ----
    let store = retry("postgres", || {
        Store::connect_with(&cfg.database_url, cfg.pg_pool)
    })
    .await?;
    if cfg.schema_verify_only {
        let pending = store.pending_migrations().await?;
        if !pending.is_empty() {
            anyhow::bail!(
                "OBLETH_SCHEMA_VERIFY_ONLY is set and Postgres is missing {} schema file(s): {}",
                pending.len(),
                pending.join(", ")
            );
        }
    } else {
        store.migrate().await?;
    }
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
        cfg.telemetry_flush_interval,
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
        Ok(keys) => match redis.put_resolved_bulk(Resolver::Keys, &keys).await {
            Ok(()) => tracing::info!(count = keys.len(), "warmed key cache"),
            Err(e) => tracing::warn!(error = %e, "failed to warm keys into redis"),
        },
        Err(e) => tracing::warn!(error = %e, "failed to load keys for warming"),
    }

    // ---- fairshare scheduler + capacity ----
    // The ceiling saved from the API wins; OBLETH_GLOBAL_MAX_IN_FLIGHT
    // applies while none is saved.
    let capacity = Arc::new(StaticCapacity::new(cfg.global_max_in_flight));
    let ceiling = Arc::new(obleth_admin::replica_settings::GlobalCeiling::new(
        capacity.clone(),
        cfg.global_max_in_flight,
    ));
    match store.get_global_ceiling().await {
        Ok(saved) => ceiling.apply(saved),
        Err(e) => {
            tracing::warn!(error = %e, "failed to load the saved global ceiling; using OBLETH_GLOBAL_MAX_IN_FLIGHT")
        }
    }
    let fairshare = FairShare::start(
        capacity.clone(),
        cfg.fairshare_algorithm,
        cfg.default_model_max_in_flight,
    );
    if let Some(affinity) = cfg.fairshare_session_affinity {
        tracing::info!(
            warm_secs = affinity.warm_ttl.as_secs(),
            hold_ms = affinity.hold.as_millis() as u64,
            max_jumps = affinity.max_jumps,
            max_bypass_secs = affinity.max_bypass.as_secs(),
            max_held_percent = affinity.max_held_percent,
            "fairshare session affinity on"
        );
        fairshare.set_session_affinity(Some(affinity));
    }
    // Cluster-wide limits: with shared slots on and more than one replica
    // live, every admission takes a slot in Redis against the configured
    // (undivided) limits; alone, admission stays local. When shared slots are
    // off or Redis is unavailable, replica-aware sizing divides the limits
    // across the live replicas instead. Both need the heartbeat, which runs
    // before the listeners open so the first request sees the fleet.
    let instance = replicas::instance_id();
    if cfg.fairshare_shared_slots {
        fairshare.enable_shared_slots(
            Arc::new(obleth_redis::RedisSlots::new(
                redis.clone(),
                instance.clone(),
            )),
            obleth_fairshare::SharedSlotsConfig {
                reconcile_interval: cfg.fairshare_replica_heartbeat,
                split_on_fallback: cfg.fairshare_replica_aware,
                // The cluster-wide counts add up views published at
                // different moments; requests that finish or start between
                // them blur the sum, so publish often enough that the
                // blur stays a slot or two.
                view_interval: cfg
                    .fairshare_replica_heartbeat
                    .min(Duration::from_millis(500)),
                ..Default::default()
            },
        );
        spawn_slot_release_listener(redis.clone(), fairshare.clone());
    }
    let (replica_heartbeat, replicas) = if cfg.fairshare_shared_slots || cfg.fairshare_replica_aware
    {
        let (heartbeat, n) = replicas::ReplicaHeartbeat::start(
            redis.clone(),
            fairshare.clone(),
            instance.clone(),
            cfg.fairshare_replica_heartbeat,
            cfg.fairshare_replica_ttl,
        )
        .await;
        (Some(heartbeat), n)
    } else {
        tracing::info!(
            "OBLETH_FAIRSHARE_SHARED_SLOTS and OBLETH_FAIRSHARE_REPLICA_AWARE are off; each \
                 replica enforces the full configured fairshare limits"
        );
        (None, 1)
    };
    tracing::info!(
        instance = %instance,
        replicas,
        shared_slots = cfg.fairshare_shared_slots,
        replica_aware = cfg.fairshare_replica_aware,
        "fairshare limits are cluster-wide: {}",
        match (cfg.fairshare_shared_slots, cfg.fairshare_replica_aware) {
            (true, true) => "shared slots in Redis, divided across replicas if Redis is unavailable",
            (true, false) => "shared slots in Redis, enforced in full per replica if Redis is unavailable",
            (false, true) => "divided across the live replicas",
            (false, false) => "no; each replica enforces them in full",
        }
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
            // Shared slots enforce the configured values as they stand; the
            // per-replica check only applies where the limits are divided.
            let divided_by = if cfg.fairshare_shared_slots || !cfg.fairshare_replica_aware {
                1
            } else {
                replicas
            };
            if let Some(warning) = obleth_admin::ceiling_check(
                &models,
                cfg.default_model_max_in_flight,
                ceiling.max_in_flight(),
                divided_by,
            ) {
                tracing::warn!(
                    ceiling = warning.ceiling,
                    ceiling_source = ?ceiling.source(),
                    pool_sum = warning.pool_sum,
                    replicas,
                    "{}",
                    warning.message
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
    // Hashes Redis did not know, so a flood of one made-up key costs one
    // Redis lookup a minute on this pod (see key_miss.rs).
    let key_misses = key_miss::KeyMissCache::default();
    let model_misses = key_miss::model_misses();
    let model_cache: Cache<String, Arc<obleth_config::ResolvedModel>> = Cache::builder()
        .time_to_live(Duration::from_secs(300))
        .max_capacity(10_000)
        .build();
    let variant_cache: Cache<String, Arc<obleth_config::ResolvedModel>> = Cache::builder()
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
    let families = reasoning::FamilyRegistry::new();
    // Rolling completion-length averages, shared with the admin state below so
    // simulate scores with the live numbers.
    let output_stats = router::OutputStats::default();

    let (local_cache_tx, mut local_cache_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let key_cache_direct = key_cache.clone();
    let key_misses_direct = key_misses.clone();
    tokio::spawn(async move {
        while let Some(hash) = local_cache_rx.recv().await {
            key_cache_direct.invalidate(&hash).await;
            key_misses_direct.forget(&hash).await;
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
    // A host that drops packets would otherwise hold the request, and its
    // fairshare permit, for the kernel's connect give-up before the next
    // endpoint is tried.
    if cfg.upstream_connect_timeout_secs > 0 {
        http_builder =
            http_builder.connect_timeout(Duration::from_secs(cfg.upstream_connect_timeout_secs));
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
    // re-read by the 15 s refresh loop, so a save on any replica reaches all of
    // them. On boot, prefer the saved settings; otherwise
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
    let replica_settings = obleth_admin::replica_settings::ReplicaSettings::new(
        store.clone(),
        alerts.clone(),
        ceiling.clone(),
    );

    // JWT bearer auth: trusted issuers from OBLETH_JWT_ISSUERS. JWKS URLs pass
    // the same destination policy as any operator-supplied upstream, discovery
    // resolves `jwks_uri` for issuers configured without one, and the first
    // fetch happens here so the path is live before the listener opens. A
    // fetch failure at boot is logged (and alerted) but does not abort: the
    // background refresh keeps retrying and the key path is unaffected.
    let jwt = if cfg.jwt_issuers.is_empty() {
        None
    } else {
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

    // Discovered models: each replica derives their pool sizes from the live
    // backend and hands them to fairshare as configured values. Started with
    // the Management API below; `GET /v1/load` reads the sizes it sets.
    let capacity_discovery =
        obleth_admin::capacity_discovery::CapacityDiscovery::new(cfg.capacity_discovery.clone());
    let load = cfg.load_endpoint.then(|| {
        load::LoadReporter::new(
            fairshare.clone(),
            model_registry.clone(),
            families.clone(),
            capacity_discovery.clone(),
            obleth_admin::live_load::LoadSettings {
                replica_aware: cfg.fairshare_replica_aware,
                // The replica heartbeat above runs with either one on.
                replicas_known: cfg.fairshare_shared_slots || cfg.fairshare_replica_aware,
                default_cap: cfg.default_model_max_in_flight,
            },
        )
    });
    if load.is_none() {
        tracing::info!("GET /v1/load is off (OBLETH_LOAD_ENDPOINT)");
    }

    // The object store, for bytes that do not belong in Postgres. Features
    // that need it (batches) are off without it.
    let objects = match cfg
        .object_store
        .as_ref()
        .map(object_store::ObjectStore::from_settings)
    {
        Some(Ok(o)) => {
            tracing::info!(bucket = %o.describe(), "object store on");
            o.spawn_check();
            Some(o)
        }
        None => None,
        Some(Err(e)) => {
            tracing::error!(error = %e, "object store off: its settings are not usable");
            None
        }
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
        key_misses: key_misses.clone(),
        model_misses: model_misses.clone(),
        model_cache: model_cache.clone(),
        variant_cache: variant_cache.clone(),
        mcp_cache: mcp_cache.clone(),
        tool_cache,
        model_registry: model_registry.clone(),
        families: families.clone(),
        classifier: classifier.clone(),
        output_stats: output_stats.clone(),
        boons: boons.clone(),
        metrics: metrics.clone(),
        fail_open: cfg.fail_open,
        alerts: alerts.clone(),
        session_id_derivation: cfg.session_id_derivation,
        session_affinity: cfg.fairshare_session_affinity,
        session_affinity_settings: cfg.fairshare_session_affinity_settings,
        compressor: crate::boons::compressor::CompressorClient::new(&cfg.compressor),
        energy: energy.clone(),
        jwt,
        knowledge: knowledge.clone(),
        jobs: jobs::JobStore::new(store.clone(), cfg.job_scope),
        load,
        crash_guard: crate::crash_guard::CrashGuard::new(
            cfg.crash_guard,
            Some(capacity_discovery.clone()),
        ),
        batches: objects.map(|o| batches::Batches::new(store.clone(), o)),
    };
    tracing::info!(
        scope = cfg.job_scope.as_str(),
        "video jobs are private to their {}",
        cfg.job_scope.as_str()
    );
    jobs::spawn_job_pruner(app_state.jobs.clone(), cfg.video_job_retention);
    // Created here so the batch runner can watch it; the signal listener that
    // sends on it starts once the listeners are bound.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let batch_runner = match app_state.batches.clone() {
        Some(b) => {
            tracing::info!(
                max_lines = b.config().max_lines,
                share = b.config().share,
                retention_hours = b.config().retention.as_secs() / 3600,
                "batches on: /v1/files and /v1/batches are served"
            );
            batches::spawn_runner(app_state.clone(), b, shutdown_rx.clone())
        }
        None => {
            tracing::info!(
                "batches off: they need an object store; set OBLETH_S3_BUCKET to serve /v1/files and /v1/batches"
            );
            None
        }
    };

    match store.all_resolved_models().await {
        Ok(models) => {
            // One key per addressable name: the canonical `model_name` plus
            // every alias and variant. Resolution stays a single lookup on
            // whatever name the client sent, so an alias costs nothing per
            // request.
            for (_, resolved) in &models {
                let shared = Arc::new(resolved.clone());
                for name in resolved.addressable_names() {
                    model_cache.insert(name.to_string(), shared.clone()).await;
                }
            }
            let count = models.len();
            let by_name = obleth_admin::rewarm::models_by_name(models);
            if let Err(e) = redis.put_resolved_bulk(Resolver::Models, &by_name).await {
                tracing::warn!(error = %e, "failed to warm models into redis");
            }
            tracing::info!(count, "warmed model cache");
            let tier_source = classifier.settings().tier_source;
            install_candidates(&model_registry, store.build_candidates(tier_source).await);
        }
        Err(e) => tracing::warn!(error = %e, "failed to load models for warming"),
    }
    match store.list_model_families().await {
        Ok(f) => families.store(f),
        Err(e) => tracing::warn!(error = %e, "failed to load model families"),
    }

    // Keep the `auto`-router candidate list fresh: model edits, enable/disable,
    // and health/maintenance transitions are all reflected within one interval.
    spawn_model_registry_refresh(
        store.clone(),
        model_registry.clone(),
        families.clone(),
        SavedSettings {
            classifier: classifier.clone(),
            boons: boons.clone(),
            energy: energy.clone(),
            replica: replica_settings.clone(),
        },
        cfg.upstream_read_timeout_secs,
    );

    energy::spawn_energy_poller(energy.clone(), http.clone(), alerts.clone());
    energy::spawn_energy_in_flight_sync(energy.clone(), fairshare.clone());

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
            if let Err(e) = redis
                .put_resolved_bulk(Resolver::McpServers, &servers)
                .await
            {
                tracing::warn!(error = %e, "failed to warm mcp servers into redis");
            }
            for (name, resolved) in &servers {
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
    // Which models' requests the crash guard looks strikes up for.
    app_state.crash_guard.spawn_refresh(redis.clone());
    spawn_invalidation_listener(
        redis.clone(),
        InvalidationCaches {
            keys: key_cache.clone(),
            key_misses: key_misses.clone(),
            model_misses: model_misses.clone(),
            models: model_cache.clone(),
            variants: variant_cache.clone(),
            mcp: mcp_cache.clone(),
            crash_guard: app_state.crash_guard.clone(),
            settings: Some(replica_settings.clone()),
        },
        redis_reconnected.clone(),
    );
    spawn_redis_rewarm(
        store.clone(),
        redis.clone(),
        (cfg.redis_rewarm_secs > 0)
            .then(|| Duration::from_secs(clamped_redis_rewarm_secs(cfg.redis_rewarm_secs))),
        redis_reconnected,
        [key_misses.clone(), model_misses.clone()],
    );

    // ---- admin (Management API) ----
    // Every Management API read stops at one execution cap; ingest keeps its
    // own client without one.
    let clickhouse_read = build_clickhouse(&cfg).with_option(
        "max_execution_time",
        obleth_admin::READ_MAX_EXECUTION_SECS.to_string(),
    );
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
    // Capacity discovery's loop reads the registry the refresher above keeps
    // current, never Postgres.
    let _capacity_discovery_task =
        capacity_discovery.spawn(fairshare.clone(), cfg.default_model_max_in_flight, {
            let registry = model_registry.clone();
            std::sync::Arc::new(move || {
                registry
                    .load()
                    .iter()
                    .filter_map(|c| {
                        obleth_admin::capacity_discovery::DiscoveryTarget::from_resolved(
                            &c.model, c.healthy,
                        )
                    })
                    .collect()
            })
        });
    let admin_state = obleth_admin::AdminState {
        store: store.clone(),
        redis: redis.clone(),
        ceiling: ceiling.clone(),
        fairshare: fairshare.clone(),
        fairshare_stats: fairshare.stats(),
        default_model_max_in_flight: cfg.default_model_max_in_flight,
        fairshare_history: fairshare_history.clone(),
        fairshare_history_secs: cfg.fairshare_history_secs,
        fairshare_replica_aware: cfg.fairshare_replica_aware,
        fairshare_shared_slots: cfg.fairshare_shared_slots,
        clickhouse: clickhouse_read,
        admin_token: cfg.admin_token.clone(),
        health: health_runtime,
        usage_retention_default_days: cfg.usage_retention_days,
        ssrf: ssrf.clone(),
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
        capacity_discovery: capacity_discovery.clone(),
        // Advice findings are judged through the real /v1/verdicts pipeline
        // (auth, fairshare, ledger) as the reserved control-plane key, so the
        // judging shows up in usage like any other caller.
        judge: Some(std::sync::Arc::new({
            let st = app_state.clone();
            let store = store.clone();
            move |body: serde_json::Value| {
                let st = st.clone();
                let store = store.clone();
                Box::pin(async move { judge_with_verdicts(st, store, body).await })
                    as std::pin::Pin<
                        Box<
                            dyn std::future::Future<
                                    Output = std::result::Result<serde_json::Value, String>,
                                > + Send,
                        >,
                    >
            }
        })),
        live_catalog: Default::default(),
        usage_cache: Default::default(),
        slurm_http: Default::default(),
        client_address: cfg.client_address.clone(),
        compressor: cfg.compressor.clone(),
    };
    obleth_admin::advice::spawn_output_stats_refresher(
        admin_state.clickhouse.clone(),
        output_stats.clone(),
    );
    obleth_admin::model_health::spawn_worker(admin_state.clone());
    obleth_admin::usage_retention::spawn_worker(admin_state.clone());
    let admin_app = obleth_admin::router(admin_state);

    // ---- routers ----
    let proxy_app = proxy_router(app_state);

    let metrics_state = MetricsState {
        metrics: metrics.clone(),
        fs: fairshare.stats(),
        tele: telemetry.stats(),
        capacity_discovery,
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

    // Before the heartbeat stops: the runner's last requests still hold slots.
    if let Some(runner) = batch_runner {
        runner.finish().await;
    }
    // Only once drained: a draining replica still holds its in-flight slots.
    if let Some(heartbeat) = replica_heartbeat {
        heartbeat.stop().await;
    }
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
/// One `/v1/verdicts` call made in-process as the control-plane key: the
/// same handler a client reaches, without a network hop.
async fn judge_with_verdicts(
    state: AppState,
    store: Store,
    body: serde_json::Value,
) -> std::result::Result<serde_json::Value, String> {
    let secret = store
        .control_plane_key_secret()
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "the control-plane key does not exist yet".to_string())?;
    let req = axum::http::Request::builder()
        .method("POST")
        .uri(verdicts::VERDICTS_PATH)
        .header(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {secret}"),
        )
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(body.to_string()))
        .map_err(|e| e.to_string())?;
    let resp = verdicts::handler(AxumState(state), req).await;
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
        .await
        .map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "verdicts answered {status}: {}",
            String::from_utf8_lossy(&bytes)
                .chars()
                .take(300)
                .collect::<String>()
        ));
    }
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

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
            // TCP_NODELAY: a response goes out as headers then body; with
            // Nagle on, the body waits for the client's delayed ACK of the
            // headers (40 ms on Linux) on every reused keep-alive connection.
            // With connect info, so the client-address middleware can see
            // the TCP peer.
            axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .tcp_nodelay(true)
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

/// The data-plane router: the gateway's native endpoints, with everything
/// else through the proxy pipeline.
fn proxy_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(|| async { "ok" }))
        // Native typed-verdict endpoint. Registered explicitly (not via the
        // fallback) so it is never mistaken for a passthrough path and proxied
        // verbatim to `{api_base}/verdicts`.
        .route(
            verdicts::VERDICTS_PATH,
            axum::routing::post(verdicts::handler),
        )
        // Native web search (Perplexity shape), in both spellings LiteLLM
        // serves. `GET /v1/search/tools` shares the `:tool` pattern.
        .route(search::SEARCH_PATH, axum::routing::post(search::search))
        .route(
            "/v1/search/:tool",
            axum::routing::post(search::search_named).get(search::tools),
        )
        .route("/search", axum::routing::post(search::search))
        .route(
            "/search/:tool",
            axum::routing::post(search::search_named).get(search::tools),
        )
        // The voices a text-to-speech model's server offers (not in OpenAI's
        // API; vLLM-Omni's shape). Speech itself streams through the fallback.
        .route(speech::VOICES_PATH, get(speech::voices))
        // Each model's live load, for callers. Registered explicitly so it is
        // never forwarded upstream; off, it answers 404 itself.
        .route(load::LOAD_PATH, get(load::load))
        .route("/mcp/:server", axum::routing::any(mcp::mcp_handler))
        .route("/mcp/:server/*rest", axum::routing::any(mcp::mcp_handler))
        .fallback(proxy::proxy_handler)
        // Records each request's client address, country and User-Agent for
        // key network limits and the usage ledger (see `client`).
        .layer(axum::middleware::from_fn(client::capture))
        .with_state(state)
}

#[derive(Clone)]
struct MetricsState {
    metrics: Arc<Metrics>,
    fs: Arc<obleth_fairshare::Stats>,
    tele: Arc<TelemetryStats>,
    capacity_discovery: obleth_admin::capacity_discovery::CapacityDiscovery,
}

async fn metrics_handler(
    AxumState(state): AxumState<MetricsState>,
) -> impl axum::response::IntoResponse {
    use std::sync::atomic::Ordering;
    state.metrics.set_gauges(
        state.fs.in_flight.load(Ordering::Relaxed) as i64,
        state.fs.queued.load(Ordering::Relaxed),
    );
    state.metrics.record_telemetry_losses(&state.tele);
    state
        .metrics
        .set_fairshare_replicas(state.fs.replicas.load(Ordering::Relaxed) as i64);
    state.metrics.set_fairshare_slot_mode(
        state.fs.mode(),
        state.fs.cluster_in_flight.load(Ordering::Relaxed) as i64,
    );
    state.metrics.set_session_affinity(
        state.fs.held.load(Ordering::Relaxed) as i64,
        &state.fs.affinity,
    );
    for (discovery_state, count) in state.capacity_discovery.state_counts() {
        state
            .metrics
            .set_capacity_discovery_models(discovery_state, count as i64);
    }
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

/// The engines whose settings are saved from the control plane and re-read
/// by the refresh loop, so a save on any replica reaches all of them.
struct SavedSettings {
    classifier: classifier::Classifier,
    boons: boons::BoonEngine,
    energy: energy::EnergyEngine,
    /// Alert targets and the global ceiling; also re-read when another
    /// replica says they changed (see [`InvalidationCaches::apply`]).
    replica: obleth_admin::replica_settings::ReplicaSettings,
}

/// Periodically rebuild the `auto`-router candidate list so enable/disable,
/// metadata edits, and health/maintenance transitions take effect without a
/// restart. Also refreshes the settings saved from the control plane
/// (classifier, boons, energy, alerts, the global ceiling) so they propagate
/// to every replica within one interval. Runs every 15s; failures are logged and retried next
/// tick.
fn spawn_model_registry_refresh(
    store: Store,
    registry: router::ModelRegistry,
    families: reasoning::FamilyRegistry,
    saved: SavedSettings,
    upstream_read_timeout_secs: u64,
) {
    let SavedSettings {
        classifier,
        boons,
        energy,
        replica,
    } = saved;
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
            match store.list_model_families().await {
                Ok(f) => families.store(f),
                Err(e) => tracing::warn!(error = %e, "model family refresh failed"),
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
            replica.reload_all().await;
        }
    });
}

/// Sample the scheduler every `FAIRSHARE_HISTORY_INTERVAL_MS` into the
/// in-memory history ring. With shared slots each sample counts every live
/// replica, so whichever replica answers `/fairshare/history` returns the
/// same series; when the shared backend cannot be read the tick records this
/// replica's own state. A failed sample (scheduler gone) skips the tick.
fn spawn_fairshare_history_sampler(fairshare: FairShare, history: Arc<FairshareHistory>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(FAIRSHARE_HISTORY_INTERVAL_MS));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let sample = match fairshare.cluster_sample().await {
                Some(sample) => Some(sample),
                None => fairshare.sample().await,
            };
            if let Some(sample) = sample {
                history.push(sample);
            }
        }
    });
}

/// Deliver shared-slot release wakeups from other replicas to the scheduler,
/// so a waiter here retries as soon as a slot frees anywhere. After a
/// reconnect every pool is retried, since wakeups sent meanwhile are lost;
/// the scheduler's own retry timer covers the gap either way.
fn spawn_slot_release_listener(redis: RedisStore, fairshare: FairShare) {
    tokio::spawn(async move {
        let woken = fairshare.clone();
        redis
            .run_slot_release_listener(
                &obleth_redis::SlotKeys::default(),
                move |pool| woken.wake(&pool),
                move |reconnected| {
                    if reconnected {
                        fairshare.wake_all();
                    }
                },
            )
            .await;
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
    key_misses: key_miss::KeyMissCache,
    model_misses: key_miss::KeyMissCache,
    models: Cache<String, Arc<obleth_config::ResolvedModel>>,
    variants: Cache<String, Arc<obleth_config::ResolvedModel>>,
    mcp: Cache<String, Arc<obleth_config::ResolvedMcpServer>>,
    crash_guard: crash_guard::CrashGuard,
    /// Alert targets and the global ceiling, re-read when another replica
    /// saves them. `None` in tests that exercise the caches alone.
    settings: Option<obleth_admin::replica_settings::ReplicaSettings>,
}

impl InvalidationCaches {
    fn invalidate_all(&self) {
        self.keys.invalidate_all();
        self.key_misses.clear();
        self.model_misses.clear();
        self.models.invalidate_all();
        self.variants.invalidate_all();
        self.mcp.invalidate_all();
    }

    async fn apply(&self, target: obleth_redis::Invalidation) {
        use obleth_redis::Invalidation;
        match target {
            Invalidation::All => self.invalidate_all(),
            Invalidation::Model(name) => {
                self.models.invalidate(&name).await;
                self.variants.invalidate(&name).await;
                // A model by this name may have just been created.
                self.model_misses.forget(&name).await;
            }
            Invalidation::Mcp(name) => self.mcp.invalidate(&name).await,
            Invalidation::CrashStrike(model) => self
                .crash_guard
                .note_strike(&model, crash_guard::STRIKE_TTL),
            Invalidation::Settings(which) => {
                if let Some(settings) = &self.settings {
                    settings.reload(which).await;
                }
            }
            Invalidation::Key(hash) => {
                self.keys.invalidate(&hash).await;
                // A key with this hash may have just been created or restored.
                self.key_misses.forget(&hash).await;
            }
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
                tokio::spawn(async move {
                    caches
                        .apply(obleth_redis::Invalidation::parse(&target))
                        .await
                });
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
    misses: [key_miss::KeyMissCache; 2],
) {
    tokio::spawn(async move {
        let mut tick = every.map(|period| {
            // Replicas started together would otherwise re-warm together,
            // each pushing everything at once; up to a tenth of the period,
            // per replica, spreads them out.
            let jitter = period.mul_f64(rand::random::<f64>() / 10.0);
            let first = tokio::time::Instant::now() + period + jitter;
            let mut t = tokio::time::interval_at(first, period);
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
            rewarm_redis(&store, &redis, &misses).await;
        }
    });
}

/// One re-warm pass (see [`obleth_admin::rewarm`]). A real key looked up
/// while Redis was missing it (after a restart or flush, before this pass
/// pushed it back) was remembered as unknown; the push publishes no per-key
/// invalidation, so every miss is forgotten once Redis holds the keys again.
async fn rewarm_redis(store: &Store, redis: &RedisStore, misses: &[key_miss::KeyMissCache; 2]) {
    let pass = obleth_admin::rewarm::push_and_prune(
        store,
        redis,
        obleth_admin::rewarm::Reread::WhenPruned,
    )
    .await;
    // Keys and model names, which a pass also pushes without a message.
    let [key_misses, model_misses] = misses;
    if pass.keys.is_ok() {
        key_misses.clear();
    }
    if pass.models.is_ok() {
        model_misses.clear();
    }
    let count = |o: &obleth_admin::rewarm::Outcome| o.as_ref().ok().map(|p| p.ids.len());
    tracing::debug!(
        keys = ?count(&pass.keys),
        models = ?count(&pass.models),
        mcp = ?count(&pass.mcp),
        "redis re-warm pass complete"
    );
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

    /// A model name Redis did not know is remembered, so a family's name
    /// costs one lookup, not one per request; a model published under the
    /// name forgets it on every pod.
    #[tokio::test]
    async fn a_published_model_forgets_its_name_miss() {
        let caches = InvalidationCaches {
            keys: Cache::builder().build(),
            key_misses: Default::default(),
            model_misses: key_miss::model_misses(),
            models: Cache::builder().build(),
            variants: Cache::builder().build(),
            mcp: Cache::builder().build(),
            crash_guard: crash_guard::CrashGuard::disabled(),
            settings: None,
        };
        let lookups = std::sync::atomic::AtomicUsize::new(0);
        let lookup = || async {
            lookups.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok::<Option<()>, ()>(None)
        };
        for _ in 0..3 {
            caches
                .model_misses
                .lookup("family-x", true, lookup)
                .await
                .unwrap();
        }
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 1);
        caches
            .apply(obleth_redis::Invalidation::Model("family-x".into()))
            .await;
        caches
            .model_misses
            .lookup("family-x", true, lookup)
            .await
            .unwrap();
        assert_eq!(lookups.load(std::sync::atomic::Ordering::SeqCst), 2);
    }

    #[test]
    fn failed_health_query_retains_snapshot_and_success_replaces_it() {
        let registry = router::ModelRegistry::new();
        let before = registry.load();
        install_candidates(&registry, Err(obleth_store::StoreError::NotFound));
        assert!(Arc::ptr_eq(&before, &registry.load()));
        install_candidates(&registry, Ok(Vec::new()));
        assert!(!Arc::ptr_eq(&before, &registry.load()));
    }

    #[test]
    fn every_variant_gets_a_resolver_key_holding_its_parent_route() {
        let mut parent = model("glm-5-3", None);
        parent.aliases = vec!["glm-5-3-fp8".into()];
        parent.variants = vec![obleth_config::ModelVariant {
            name: "glm-5-3-spec".into(),
            description: String::new(),
            boons: vec!["speculation".into()],
        }];
        let keyed = obleth_admin::rewarm::models_by_name(vec![("glm-5-3".into(), parent.clone())]);
        let names: Vec<&str> = keyed.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["glm-5-3", "glm-5-3-fp8", "glm-5-3-spec"]);
        // The variant's key holds the parent unchanged; its boons are added per
        // request, so the cached route never differs between the names.
        assert!(keyed.iter().all(|(_, m)| m == &parent));
    }

    fn model(name: &str, request_timeout_secs: Option<i64>) -> obleth_config::ResolvedModel {
        obleth_config::ResolvedModel {
            description: String::new(),
            model_name: name.to_string(),
            aliases: Vec::new(),
            lifecycle: Default::default(),
            reasoning: Default::default(),
            listed: true,
            rejected_request_fields: Vec::new(),
            session_affinity: Default::default(),
            variants: Vec::new(),
            upstream_model: name.to_string(),
            api_base: "http://upstream".to_string(),
            api_key: None,
            upstream_headers: Default::default(),
            model_type: obleth_config::DEFAULT_MODEL_TYPE.to_string(),
            quantization: obleth_config::DEFAULT_QUANTIZATION.to_string(),
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
            model: Arc::new(model(name, request_timeout_secs)),
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

    #[test]
    fn redis_rewarm_secs_is_clamped_to_the_maximum() {
        assert_eq!(clamped_redis_rewarm_secs(300), 300);
        assert_eq!(clamped_redis_rewarm_secs(u64::MAX), MAX_REDIS_REWARM_SECS);
    }
}
