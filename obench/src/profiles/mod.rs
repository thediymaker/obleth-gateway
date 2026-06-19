pub mod auto;
pub mod plan;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use rand::Rng;

use crate::admin::AdminClient;
use crate::cli::{Cli, Profile, Scope, Target};
use crate::engine::fleet::{self, TrafficKind};
use crate::engine::load::{ChatRequest, LoadClient, RunConfig};
use crate::engine::stats::{Stats, Verdict};
use crate::seed::{self, SeededRun};
use crate::{report, target};

const FIXTURE_API_BASE_DEFAULT: &str = "http://benchmark-backend:8081";

pub struct RunHandles {
    pub stats: Arc<Mutex<Stats>>,
    pub stop: Arc<AtomicBool>,
    pub plan: plan::ProfilePlan,
    pub ui_base: String,
    pub profile_name: String,
}

// ── shared seed → guard → capacity → make_req helper ──────────────────────────

struct SeededSetup {
    #[allow(dead_code)]
    seeded: SeededRun,
    make_req: Box<dyn Fn() -> ChatRequest + Send + Sync + 'static>,
    plan: plan::ProfilePlan,
    profile_name: String,
}

async fn build_setup(cli: &Cli, tgt: Target, profile: Profile, scope: Scope) -> Result<SeededSetup> {
    let admin = AdminClient::new(cli.admin_base.clone(), cli.admin_token.clone());
    let plan = plan::resolve(profile, cli);

    // Seed.
    let seeded: SeededRun = match tgt {
        Target::Fixture => {
            let api_base = std::env::var("BENCHMARK_API_BASE")
                .unwrap_or_else(|_| FIXTURE_API_BASE_DEFAULT.to_string());
            seed::seed_fixture(&admin, &api_base, &scope).await?
        }
        Target::Live => {
            let raw = std::fs::read_to_string(&cli.config)
                .map_err(|e| anyhow::anyhow!("reading {}: {e}", cli.config))?;
            let cfg = crate::config::load_live_config(&raw, &|k| std::env::var(k).ok())
                .map_err(|e| anyhow::anyhow!(e))?;
            crate::config::validate_live(&cfg, &scope).map_err(|e| anyhow::anyhow!(e))?;
            seed::seed_live(&admin, &cfg, &scope).await?
        }
    };

    if seeded.models.is_empty() {
        anyhow::bail!("no models were seeded — check the target/config (fixture backend reachable? live config has matching models?)");
    }
    if seeded.tenants.is_empty() {
        anyhow::bail!("no tenants were seeded — check the target/config");
    }

    // Set capacity.
    let cap = admin.set_capacity(plan.capacity).await?;
    println!(
        "seeded {} models, {} tenants, capacity max_in_flight={cap}",
        seeded.models.len(),
        seeded.tenants.len()
    );

    // Build a make_req closure over the seeded fleet.
    let proxy_base = cli.proxy_base.clone();
    let input_tokens = cli.input_tokens;
    let seeded_arc = Arc::new(seeded.clone());
    let plan_stream = plan.stream;
    let plan_out = plan.output_tokens;

    let make_req = {
        let seeded_arc = seeded_arc.clone();
        move || -> ChatRequest {
            let mut rng = rand::thread_rng();
            // Pick a tenant by traffic share.
            let tweights: Vec<u32> = seeded_arc
                .tenants
                .iter()
                .map(|t| t.traffic_share.max(1))
                .collect();
            let ti = fleet::weighted_index(&tweights, rng.gen::<f64>());
            let tenant = &seeded_arc.tenants[ti];
            // Pick a model + shape. Fixture uses the traffic catalog; live uses the seeded models.
            let (model, out_tokens, stream) =
                if seeded_arc.models.iter().all(|m| m.starts_with("obench-")) {
                    let cands: Vec<&fleet::TrafficType> = fleet::FIXTURE_TRAFFIC
                        .iter()
                        .filter(|t| seeded_arc.models.iter().any(|m| m == t.model))
                        .collect();
                    if cands.is_empty() {
                        (seeded_arc.models[0].clone(), plan_out, plan_stream)
                    } else {
                        let w: Vec<u32> = cands.iter().map(|t| t.weight).collect();
                        let tt = cands[fleet::weighted_index(&w, rng.gen::<f64>())];
                        (
                            tt.model.to_string(),
                            if tt.output_tokens > 0 { tt.output_tokens } else { plan_out },
                            tt.kind == TrafficKind::ChatStream,
                        )
                    }
                } else {
                    let mi = rng.gen_range(0..seeded_arc.models.len());
                    (seeded_arc.models[mi].clone(), plan_out, plan_stream)
                };
            ChatRequest {
                proxy_base: proxy_base.clone(),
                key: tenant.key.clone(),
                model,
                input_tokens,
                output_tokens: out_tokens,
                stream,
            }
        }
    };

    let profile_name = format!("{profile:?}").to_lowercase();

    Ok(SeededSetup {
        seeded,
        make_req: Box::new(make_req),
        plan,
        profile_name,
    })
}

// ── public API ─────────────────────────────────────────────────────────────────

pub async fn run_headless(cli: &Cli, tgt: Target, profile: Profile, scope: Scope) -> Result<i32> {
    target::validate_combo(tgt, profile).map_err(|e| anyhow::anyhow!(e))?;

    let admin = AdminClient::new(cli.admin_base.clone(), cli.admin_token.clone());
    let plan = plan::resolve(profile, cli);

    if profile == Profile::Auto {
        // Seed independently for the Auto path (it handles its own capacity).
        let seeded: SeededRun = match tgt {
            Target::Fixture => {
                let api_base = std::env::var("BENCHMARK_API_BASE")
                    .unwrap_or_else(|_| FIXTURE_API_BASE_DEFAULT.to_string());
                seed::seed_fixture(&admin, &api_base, &scope).await?
            }
            Target::Live => {
                let raw = std::fs::read_to_string(&cli.config)
                    .map_err(|e| anyhow::anyhow!("reading {}: {e}", cli.config))?;
                let cfg = crate::config::load_live_config(&raw, &|k| std::env::var(k).ok())
                    .map_err(|e| anyhow::anyhow!(e))?;
                crate::config::validate_live(&cfg, &scope).map_err(|e| anyhow::anyhow!(e))?;
                seed::seed_live(&admin, &cfg, &scope).await?
            }
        };
        if seeded.models.is_empty() {
            anyhow::bail!("no models were seeded");
        }
        if seeded.tenants.is_empty() {
            anyhow::bail!("no tenants were seeded");
        }
        let _ = admin.set_capacity(plan.capacity).await?;
        return auto::run(cli, tgt, scope, &seeded, &cli.proxy_base).await;
    }

    let scope_str = format!("{scope:?}");
    let setup = build_setup(cli, tgt, profile, scope).await?;
    let SeededSetup { seeded: _, make_req, plan, profile_name } = setup;

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

    // Sample fairshare into the timeline.
    let sampler = {
        let admin_base = cli.admin_base.clone();
        let admin_token = cli.admin_token.clone();
        let stop = stop.clone();
        let pname = profile_name.clone();
        tokio::spawn(async move {
            let a = AdminClient::new(admin_base, admin_token);
            while !stop.load(Ordering::Relaxed) {
                if let Ok(live) = a.fairshare_live().await {
                    let _ = report::append_timeline(
                        &pname,
                        &serde_json::json!({
                            "in_flight": live.global_in_flight, "queued": live.global_queued,
                        }),
                    );
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        })
    };

    let started = std::time::Instant::now();
    crate::engine::load::run_closed_loop(
        client,
        make_req,
        RunConfig { conc: plan.conc, duration_s: plan.duration_s, warmup_s: plan.warmup_s },
        stop.clone(),
        stats.clone(),
    )
    .await;
    stop.store(true, Ordering::Relaxed);
    let _ = sampler.await;

    // Summarize + report.
    let elapsed = started.elapsed().as_secs_f64().max(1.0);
    let summary = stats.lock().unwrap().summarize(elapsed, plan.max_error_rate);
    println!("\n{}", report::render_summary(&summary, &cli.ui_base));
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
pub async fn start_run(cli: &Cli, tgt: Target, profile: Profile, scope: Scope) -> Result<RunHandles> {
    target::validate_combo(tgt, profile).map_err(|e| anyhow::anyhow!(e))?;

    let setup = build_setup(cli, tgt, profile, scope).await?;
    let SeededSetup { seeded: _, make_req, plan, profile_name } = setup;

    let client = Arc::new(LoadClient::new((plan.conc as usize) * 2));
    let stats = Arc::new(Mutex::new(Stats::default()));
    let stop = Arc::new(AtomicBool::new(false));

    // Spawn the engine — do NOT await it; return handles immediately.
    {
        let client = client.clone();
        let stats = stats.clone();
        let stop = stop.clone();
        let cfg = RunConfig { conc: plan.conc, duration_s: plan.duration_s, warmup_s: plan.warmup_s };
        tokio::spawn(async move {
            crate::engine::load::run_closed_loop(client, make_req, cfg, stop.clone(), stats).await;
        });
    }

    Ok(RunHandles {
        stats,
        stop,
        plan,
        ui_base: cli.ui_base.clone(),
        profile_name,
    })
}
