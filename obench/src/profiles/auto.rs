use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use anyhow::Result;

use crate::cli::{Cli, Scope, Target};
use crate::engine::calibrate::{evaluate, Decision, KneeConfig, StepResult};
use crate::engine::load::{ChatRequest, LoadClient, ProxyRequest, RunConfig};
use crate::engine::stats::Stats;
use crate::report;
use crate::seed::SeededRun;

const STEPS: &[u32] = &[32, 64, 128, 256, 512, 1024, 2048];
const STEP_SECS: u64 = 12;

pub async fn run(
    cli: &Cli,
    tgt: Target,
    scope: Scope,
    seeded: &SeededRun,
    proxy_base: &str,
) -> Result<i32> {
    let cfg = KneeConfig::default();
    let mut history: Vec<StepResult> = Vec::new();
    let mut sustainable = STEPS[0];

    for &conc in STEPS {
        let client = Arc::new(LoadClient::new((conc as usize) * 2));
        let stats = Arc::new(Mutex::new(Stats::default()));
        let stop = Arc::new(AtomicBool::new(false));

        // Extract owned values before the move closure so we avoid borrowing
        // seeded inside the closure body (seeded is moved as seeded2).
        let tenant_key = seeded.tenants[0].key.clone();
        let model = seeded.models[0].clone();
        let proxy = proxy_base.to_string();
        let input_tokens = cli.input_tokens;
        let make_req = move || {
            ProxyRequest::Chat(ChatRequest {
                proxy_base: proxy.clone(),
                key: tenant_key.clone(),
                model: model.clone(),
                input_tokens,
                output_tokens: 4,
                stream: false,
            })
        };

        let started = std::time::Instant::now();
        crate::engine::load::run_closed_loop(
            client,
            make_req,
            RunConfig {
                conc,
                duration_s: STEP_SECS,
                warmup_s: 2,
            },
            stop,
            stats.clone(),
        )
        .await;

        let elapsed = started.elapsed().as_secs_f64().max(1.0);
        let sum = stats.lock().unwrap().summarize(elapsed, 1.0); // verdict unused here
        let latest = StepResult {
            conc,
            req_per_s: sum.req_per_s,
            error_rate: sum.error_rate,
            p99_ttfb_ms: sum.p99_ttfb_ms,
            max_queued: 0,
        };
        println!(
            "  step conc={conc}: {:.0} req/s  err {:.2}%  p99 {}ms",
            latest.req_per_s,
            latest.error_rate * 100.0,
            latest.p99_ttfb_ms
        );

        match evaluate(&history, &latest, &cfg) {
            Decision::Continue => {
                sustainable = conc;
                history.push(latest);
            }
            Decision::Stop {
                last_clean_conc,
                reason,
            } => {
                sustainable = last_clean_conc;
                println!("  knee at conc={conc}: {reason}");
                break;
            }
        }
    }

    if sustainable == 0 {
        println!(
            "\nno sustainable level found — even the lowest step ({}) exceeded the error ceiling",
            STEPS[0]
        );
    } else {
        println!("\nsustainable ceiling for this deployment: conc={sustainable}");
    }
    report::write_meta(
        "auto",
        &serde_json::json!({
            "target": format!("{tgt:?}"), "scope": format!("{scope:?}"),
            "sustainable_conc": sustainable,
            "sustainable_found": sustainable > 0,
            "replay": if sustainable > 0 {
                serde_json::json!({ "profile": "manual", "conc": sustainable, "output_tokens": 4, "stream": false })
            } else {
                serde_json::json!(null)
            },
            "steps": history.iter().map(|s| serde_json::json!({
                "conc": s.conc, "req_per_s": s.req_per_s, "error_rate": s.error_rate, "p99_ttfb_ms": s.p99_ttfb_ms,
            })).collect::<Vec<_>>(),
        }),
    )?;
    Ok(0)
}
