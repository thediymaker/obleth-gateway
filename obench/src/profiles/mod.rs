pub mod auto;
pub mod plan;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use rand::Rng;

use crate::admin::AdminClient;
use crate::cli::{Cli, Profile, Scope, Target};
use crate::engine::fleet::{self, TrafficKind};
use crate::engine::load::{ChatRequest, EmbedRequest, LoadClient, ProxyRequest, RunConfig};
use crate::engine::stats::{is_stall, Stats, Verdict};
use crate::seed::{self, SeededRun};
use crate::{report, target};

const FIXTURE_API_BASE_DEFAULT: &str = "http://benchmark-backend:8081";

pub struct RunHandles {
    pub stats: Arc<Mutex<Stats>>,
    pub stop: Arc<AtomicBool>,
    pub handle: tokio::task::JoinHandle<()>,
    pub plan: plan::ProfilePlan,
    pub ui_base: String,
    pub profile_name: String,
    pub teardown: crate::admin::Teardown,
    /// True only when obench owns a local gateway it can query over the admin
    /// API (the demo path). For a remote `live` run obench has no admin token,
    /// so the dashboard shows client-side metrics only.
    pub gateway_observable: bool,
    /// Display names of the tenant keys driving load, in counter order.
    pub key_labels: Vec<String>,
    /// Per-key dispatched-request counters, parallel to `key_labels`. The load
    /// engine bumps these so the dashboard can show fairshare distribution.
    pub key_counts: Arc<Vec<AtomicU64>>,
}

// ── shared seed → guard → capacity → make_req helper ────────────────────────

struct SeededSetup {
    make_req: Box<dyn Fn() -> ProxyRequest + Send + Sync + 'static>,
    plan: plan::ProfilePlan,
    profile_name: String,
    teardown: crate::admin::Teardown,
    /// Where load is actually sent: the local proxy (demo) or the remote obleth
    /// proxy URL the user supplied (live).
    proxy_base: String,
    /// Root URL for the control-plane watch links — local for demo, the remote
    /// host for live.
    ui_base: String,
    /// See `RunHandles::gateway_observable`.
    gateway_observable: bool,
    /// See `RunHandles::key_labels`.
    key_labels: Vec<String>,
    /// See `RunHandles::key_counts`.
    key_counts: Arc<Vec<AtomicU64>>,
}

/// Resolve the `LiveConfig` for a live run: either the one the TUI built in
/// memory (`live_override`) or the headless config file at `cli.config`.
pub(crate) fn resolve_live_config(
    cli: &Cli,
    live_override: Option<&crate::config::LiveConfig>,
) -> Result<crate::config::LiveConfig> {
    match live_override {
        Some(c) => Ok(c.clone()),
        None => {
            let raw = std::fs::read_to_string(&cli.config)
                .map_err(|e| anyhow::anyhow!("reading {}: {e}", cli.config))?;
            crate::config::load_live_config(&raw, &|k| std::env::var(k).ok())
                .map_err(|e| anyhow::anyhow!(e))
        }
    }
}

/// Seed the target, validate the result is non-empty, then return the `SeededRun`.
///
/// `live_override` lets the interactive TUI pass a `LiveConfig` it built from
/// user input (proxy URL + tenant keys + picked models) instead of reading a
/// config file. The demo path seeds via the admin API; the live path is a pure
/// black-box client and never touches admin.
async fn seed_and_guard(
    cli: &Cli,
    tgt: Target,
    scope: &Scope,
    admin: &AdminClient,
    live_override: Option<&crate::config::LiveConfig>,
) -> Result<SeededRun> {
    let seeded: SeededRun = match tgt {
        Target::Demo => {
            let api_base = std::env::var("BENCHMARK_API_BASE")
                .unwrap_or_else(|_| FIXTURE_API_BASE_DEFAULT.to_string());
            seed::seed_fixture(admin, &api_base, scope).await?
        }
        Target::Live => {
            let cfg = resolve_live_config(cli, live_override)?;
            crate::config::validate_live(&cfg, scope).map_err(|e| anyhow::anyhow!(e))?;
            seed::live_run_from_config(&cfg, scope)?
        }
    };
    if seeded.models.is_empty() {
        anyhow::bail!("no models to drive — check the target/config (fixture backend reachable? live config has models?)");
    }
    if seeded.tenants.is_empty() {
        anyhow::bail!(
            "no tenants to drive — check the target/config (live config needs at least one key)"
        );
    }
    Ok(seeded)
}

async fn build_setup(
    cli: &Cli,
    tgt: Target,
    profile: Profile,
    scope: Scope,
    live_override: Option<&crate::config::LiveConfig>,
) -> Result<SeededSetup> {
    let admin = AdminClient::new(cli.admin_base.clone(), cli.admin_token.clone());
    let plan = plan::resolve(profile, cli);

    // Where load is sent + where to watch. Demo drives the local gateway and is
    // fully observable over admin; live drives the remote proxy the user gave and
    // is a black box (no admin token).
    let (proxy_base, ui_base, gateway_observable) = match tgt {
        Target::Demo => (cli.proxy_base.clone(), cli.ui_base.clone(), true),
        Target::Live => {
            let cfg = resolve_live_config(cli, live_override)?;
            let root = target::endpoint_root(&cfg.proxy_url);
            (cfg.proxy_url.clone(), root, false)
        }
    };

    let seeded = seed_and_guard(cli, tgt, &scope, &admin, live_override).await?;
    let teardown = seeded.teardown.clone();

    // Only the demo path owns a local gateway whose capacity it can set.
    if gateway_observable {
        let cap = admin.set_capacity(plan.capacity).await?;
        println!(
            "seeded {} models, {} tenants, capacity max_in_flight={cap}",
            seeded.models.len(),
            seeded.tenants.len()
        );
    } else {
        println!(
            "driving {} models across {} tenant keys on {proxy_base}",
            seeded.models.len(),
            seeded.tenants.len()
        );
    }

    // Build a make_req closure over the seeded fleet.
    let req_base = proxy_base.clone();
    let input_tokens = cli.input_tokens;
    let seeded_arc = Arc::new(seeded.clone());
    let plan_stream = plan.stream;
    let plan_out = plan.output_tokens;

    // Per-key dispatch counters so the dashboard can show how load actually
    // spread across tenants (the whole point of a fairshare benchmark).
    let key_labels: Vec<String> = seeded
        .tenants
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if t.name.trim().is_empty() {
                format!("tenant-{}", i + 1)
            } else {
                t.name.clone()
            }
        })
        .collect();
    let key_counts: Arc<Vec<AtomicU64>> = Arc::new(
        (0..seeded.tenants.len())
            .map(|_| AtomicU64::new(0))
            .collect(),
    );

    let make_req = {
        let seeded_arc = seeded_arc.clone();
        let proxy_base = req_base.clone();
        let key_counts = key_counts.clone();
        move || -> ProxyRequest {
            let mut rng = rand::thread_rng();
            // Pick a tenant by traffic share.
            let tweights: Vec<u32> = seeded_arc
                .tenants
                .iter()
                .map(|t| t.traffic_share.max(1))
                .collect();
            let ti = fleet::weighted_index(&tweights, rng.gen::<f64>());
            let tenant = &seeded_arc.tenants[ti];
            if let Some(c) = key_counts.get(ti) {
                c.fetch_add(1, Ordering::Relaxed);
            }
            // Pick a model + shape. Fixture uses the traffic catalog; live uses the seeded models.
            if seeded_arc.models.iter().all(|m| m.starts_with("obench-")) {
                let cands: Vec<&fleet::TrafficType> = fleet::FIXTURE_TRAFFIC
                    .iter()
                    .filter(|t| seeded_arc.models.iter().any(|m| m == t.model))
                    .collect();
                if cands.is_empty() {
                    return ProxyRequest::Chat(ChatRequest {
                        proxy_base: proxy_base.clone(),
                        key: tenant.key.clone(),
                        model: seeded_arc.models[0].clone(),
                        input_tokens,
                        output_tokens: plan_out,
                        stream: plan_stream,
                    });
                }
                let w: Vec<u32> = cands.iter().map(|t| t.weight).collect();
                let tt = cands[fleet::weighted_index(&w, rng.gen::<f64>())];
                // Route Embed traffic to the real embeddings endpoint.
                if tt.kind == TrafficKind::Embed {
                    return ProxyRequest::Embed(EmbedRequest {
                        proxy_base: proxy_base.clone(),
                        key: tenant.key.clone(),
                        model: tt.model.to_string(),
                        input_tokens,
                    });
                }
                ProxyRequest::Chat(ChatRequest {
                    proxy_base: proxy_base.clone(),
                    key: tenant.key.clone(),
                    model: tt.model.to_string(),
                    input_tokens,
                    output_tokens: if tt.output_tokens > 0 {
                        tt.output_tokens
                    } else {
                        plan_out
                    },
                    stream: tt.kind == TrafficKind::ChatStream,
                })
            } else {
                let mi = rng.gen_range(0..seeded_arc.models.len());
                ProxyRequest::Chat(ChatRequest {
                    proxy_base: proxy_base.clone(),
                    key: tenant.key.clone(),
                    model: seeded_arc.models[mi].clone(),
                    input_tokens,
                    output_tokens: plan_out,
                    stream: plan_stream,
                })
            }
        }
    };

    let profile_name = format!("{profile:?}").to_lowercase();

    Ok(SeededSetup {
        make_req: Box::new(make_req),
        plan,
        profile_name,
        teardown,
        proxy_base,
        ui_base,
        gateway_observable,
        key_labels,
        key_counts,
    })
}

// ── public API ─────────────────────────────────────────────────────────────────

pub async fn run_headless(cli: &Cli, tgt: Target, profile: Profile, scope: Scope) -> Result<i32> {
    target::validate_combo(tgt, profile).map_err(|e| anyhow::anyhow!(e))?;
    target::validate_target_locality(tgt, &cli.admin_base, &cli.proxy_base)
        .map_err(|e| anyhow::anyhow!(e))?;

    let admin = AdminClient::new(cli.admin_base.clone(), cli.admin_token.clone());
    let plan = plan::resolve(profile, cli);

    if profile == Profile::Auto {
        // Seed independently for the Auto path (it handles its own capacity).
        let seeded = seed_and_guard(cli, tgt, &scope, &admin, None).await?;
        let teardown = seeded.teardown.clone();
        // Demo drives the local gateway (admin sets capacity); live drives the
        // remote proxy as a black box with no admin access.
        let (proxy_base, observable) = match tgt {
            Target::Demo => (cli.proxy_base.clone(), true),
            Target::Live => (resolve_live_config(cli, None)?.proxy_url, false),
        };
        if observable {
            let _ = admin.set_capacity(plan.capacity).await?;
        }
        let result = auto::run(cli, tgt, scope, &seeded, &proxy_base).await;
        // Clean up anything obench created (no-op for the black-box live path).
        admin.teardown(&teardown).await;
        return result;
    }

    let scope_str = format!("{scope:?}");
    let setup = build_setup(cli, tgt, profile, scope, None).await?;
    let SeededSetup {
        make_req,
        plan,
        profile_name,
        teardown,
        proxy_base: _proxy_base,
        ui_base,
        gateway_observable,
        key_labels: _key_labels,
        key_counts: _key_counts,
    } = setup;

    // Run.
    let client = Arc::new(LoadClient::new((plan.conc as usize) * 2));
    let stats = Arc::new(Mutex::new(Stats::default()));
    let stop = Arc::new(AtomicBool::new(false));

    // Ctrl-C drains cleanly.
    {
        let stop = stop.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            stop.store(true, Ordering::Relaxed);
        });
    }

    // Sample fairshare into the timeline + run the stall watchdog.
    // Watchdog: after warmup, if ≥2 consecutive 10-second ticks see zero new
    // completions while concurrency is active and the run is not winding down,
    // mark the stats as stalled and trigger stop so summarize reports FAIL.
    // Two ticks = 20 s of silence — enough to rule out a single legitimately
    // slow request but short enough to catch a real hang quickly.
    const STALL_THRESHOLD: u32 = 2;
    let sampler = {
        let admin_base = cli.admin_base.clone();
        let admin_token = cli.admin_token.clone();
        let stop = stop.clone();
        let stats_w = stats.clone();
        let pname = profile_name.clone();
        let warmup_s = plan.warmup_s;
        let conc = plan.conc;
        tokio::spawn(async move {
            let a = AdminClient::new(admin_base, admin_token);
            let mut last_completed: u64 = 0;
            let mut consecutive_zeros: u32 = 0;
            let started = std::time::Instant::now();
            while !stop.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_secs(10)).await;
                if stop.load(Ordering::Relaxed) {
                    break;
                }

                // Fairshare timeline sample — only when we own the gateway (demo).
                // A remote live gateway has no admin token, so we record client
                // metrics only and skip the in_flight/queued timeline.
                if gateway_observable {
                    if let Ok(live) = a.fairshare_live().await {
                        let _ = report::append_timeline(
                            &pname,
                            &serde_json::json!({
                                "in_flight": live.global_in_flight, "queued": live.global_queued,
                            }),
                        );
                    }
                }

                // Stall watchdog — only active after the warmup window.
                if started.elapsed().as_secs() <= warmup_s {
                    continue;
                }
                let current_completed = stats_w.lock().unwrap().ok;
                if current_completed > last_completed {
                    consecutive_zeros = 0;
                } else {
                    consecutive_zeros += 1;
                }
                last_completed = current_completed;

                if is_stall(
                    consecutive_zeros,
                    STALL_THRESHOLD,
                    conc,
                    stop.load(Ordering::Relaxed),
                ) {
                    stats_w.lock().unwrap().stalled = true;
                    stop.store(true, Ordering::Relaxed);
                    break;
                }
            }
        })
    };

    let started = std::time::Instant::now();
    crate::engine::load::run_closed_loop(
        client,
        make_req,
        RunConfig {
            conc: plan.conc,
            duration_s: plan.duration_s,
            warmup_s: plan.warmup_s,
        },
        stop.clone(),
        stats.clone(),
    )
    .await;
    stop.store(true, Ordering::Relaxed);
    let _ = sampler.await;

    // Tear down the synthetic models/tenants/keys we created. Runs on every exit
    // path (normal completion, stall, Ctrl-C drain) so no credentials linger.
    admin.teardown(&teardown).await;

    // Summarize + report.
    let elapsed = started.elapsed().as_secs_f64().max(1.0);
    let summary = stats
        .lock()
        .unwrap()
        .summarize(elapsed, plan.max_error_rate);
    println!("\n{}", report::render_summary(&summary, &ui_base));
    report::write_meta(
        &profile_name,
        &serde_json::json!({
            "target": format!("{tgt:?}"), "profile": profile_name, "scope": scope_str,
            "completed": summary.completed, "attempts": summary.attempts,
            "error_rate": summary.error_rate, "req_per_s": summary.req_per_s,
            "p50_ttfb_ms": summary.p50_ttfb_ms, "p99_ttfb_ms": summary.p99_ttfb_ms,
            "in_tokens": summary.in_tokens, "out_tokens": summary.out_tokens,
            "verdict": match &summary.verdict { Verdict::Pass => "PASS".to_string(), Verdict::Fail(v) => format!("FAIL: {}", v.join("; ")) },
        }),
    )?;

    Ok(match summary.verdict {
        Verdict::Pass => 0,
        Verdict::Fail(_) => 1,
    })
}

/// Seed + start the engine in a background task; return handles the TUI polls.
/// Note: Profile::Auto is not routed through start_run — it is excluded from
/// the TUI dashboard path because it self-calibrates capacity internally and
/// doesn't emit a fixed-load stats stream compatible with the dashboard widget.
pub async fn start_run(
    cli: &Cli,
    tgt: Target,
    profile: Profile,
    scope: Scope,
    live_override: Option<&crate::config::LiveConfig>,
) -> Result<RunHandles> {
    target::validate_combo(tgt, profile).map_err(|e| anyhow::anyhow!(e))?;
    target::validate_target_locality(tgt, &cli.admin_base, &cli.proxy_base)
        .map_err(|e| anyhow::anyhow!(e))?;

    let setup = build_setup(cli, tgt, profile, scope, live_override).await?;
    let SeededSetup {
        make_req,
        plan,
        profile_name,
        teardown,
        proxy_base: _proxy_base,
        ui_base,
        gateway_observable,
        key_labels,
        key_counts,
    } = setup;

    let client = Arc::new(LoadClient::new((plan.conc as usize) * 2));
    let stats = Arc::new(Mutex::new(Stats::default()));
    let stop = Arc::new(AtomicBool::new(false));

    // Spawn the engine — do NOT await it; return handles immediately.
    let handle = {
        let client = client.clone();
        let stats = stats.clone();
        let stop = stop.clone();
        let cfg = RunConfig {
            conc: plan.conc,
            duration_s: plan.duration_s,
            warmup_s: plan.warmup_s,
        };
        tokio::spawn(async move {
            crate::engine::load::run_closed_loop(client, make_req, cfg, stop.clone(), stats).await;
            // Signal natural completion (duration elapsed) so watchers — like the
            // TUI dashboard — see the run is done and stop ticking.
            stop.store(true, Ordering::Relaxed);
        })
    };

    Ok(RunHandles {
        stats,
        stop,
        handle,
        plan,
        ui_base,
        profile_name,
        teardown,
        gateway_observable,
        key_labels,
        key_counts,
    })
}
