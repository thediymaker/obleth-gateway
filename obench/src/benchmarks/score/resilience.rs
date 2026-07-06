//! Resilience: fault-injection scenario measuring how quickly the gateway's
//! tiered health prober detects a backend failure (MTTD) and confirms
//! recovery (MTTR) once the fault clears.
//!
//! Grading uses the *ratio* of MTTD to the prober's effective interval, not
//! absolute seconds, because the health scheduler floors re-check scheduling
//! at 60s (`jittered_next_check_at` uses `interval_secs.max(60)` —
//! obleth/crates/obleth-admin/src/model_health.rs:1163). Absolute seconds
//! would just measure that floor; the ratio measures whether detection
//! happened on the first possible probe cycle.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde::Serialize;

use crate::benchmarks::score::{SectionId, SectionResult};
use crate::engine::load::{ChatRequest, LoadClient, ProxyRequest};
use crate::engine::stats::Stats;

const DETECT_BUDGET_S: u64 = 150;
const RECOVER_BUDGET_S: u64 = 150;
const POLL_MS: u64 = 500;
const LOAD_CONC: u32 = 4;
/// Pace of the background load loop during resilience: one request per
/// worker every `PACE_MS`, so `LOAD_CONC` workers produce roughly
/// `LOAD_CONC * (1000 / PACE_MS)` req/s (~16 req/s at the defaults above).
const PACE_MS: u64 = 250;

#[derive(Clone, Debug, Serialize)]
pub struct ResilienceOutcome {
    pub model: String,
    pub mttd_s: Option<f64>,
    pub mttr_s: Option<f64>,
    pub errors_before_detect: u64,
    pub errors_after_detect: u64,
    pub effective_interval_s: f64,
}

/// Pure scoring: no I/O, drives the fault-injection scenario's result through
/// the grading rule described in the module docs.
pub fn resilience_score(o: &ResilienceOutcome) -> (u8, Vec<String>) {
    let mut recs = Vec::new();

    let Some(mttd_s) = o.mttd_s else {
        recs.push(format!(
            "gateway never marked {} unhealthy within the budget",
            o.model
        ));
        return (10, recs);
    };

    let interval = o.effective_interval_s.max(1.0);
    let ratio = mttd_s / interval;
    let mut score: i32 = if ratio <= 1.5 {
        100
    } else if ratio <= 2.5 {
        85
    } else if ratio <= 4.0 {
        65
    } else {
        35
    };

    score -= o.errors_after_detect.min(25) as i32;
    if o.errors_after_detect > 0 {
        recs.push(format!(
            "{} request(s) still failed after the gateway detected the fault — check for shielding/fast-fail once a model is marked unhealthy",
            o.errors_after_detect
        ));
    }

    match o.mttr_s {
        None => {
            score -= 15;
            recs.push(
                "recovery was not observed within the budget after the fault cleared".to_string(),
            );
        }
        Some(mttr_s) if mttr_s / interval > 2.5 => {
            score -= 10;
        }
        Some(_) => {}
    }

    (score.clamp(0, 100) as u8, recs)
}

pub fn resilience_section(o: &ResilienceOutcome) -> SectionResult {
    let (score, recommendations) = resilience_score(o);
    SectionResult {
        id: SectionId::Resilience,
        score: Some(score),
        metrics: serde_json::to_value(o).unwrap_or_else(|_| serde_json::json!({})),
        recommendations,
        error: None,
    }
}

pub async fn run_resilience(
    admin: &crate::admin::AdminClient,
    ctl: &crate::backend_ctl::BackendControl,
    proxy_base: &str,
    key: &str,
    model: &str,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<ResilienceOutcome> {
    let id = admin
        .find_model_id(model)
        .await?
        .ok_or_else(|| anyhow::anyhow!("model {model} not found"))?;

    // Snapshot current health config so we can restore it.
    let health = admin.model_health().await?;
    let prev = health
        .as_array()
        .and_then(|a| a.iter().find(|h| h["model_name"] == model))
        .cloned()
        .unwrap_or(serde_json::Value::Null);
    let prev_enabled = prev["checks_enabled"].as_bool().unwrap_or(false);
    let prev_alerts = prev["alerts_enabled"].as_bool().unwrap_or(false);
    let prev_interval = prev["check_interval_secs"].as_i64().unwrap_or(900);
    let prev_threshold = prev["failure_threshold"].as_i64().unwrap_or(3);

    // Fastest legal detection: interval 1 (prober floors at 60s), threshold 1.
    admin
        .set_model_health_config(&id, true, false, 1, 1)
        .await?;
    let effective_interval_s = 60.0f64;

    // Background paced load so the passive tier has a signal — deliberately
    // NOT `run_closed_loop` (that hammers as fast as it can, which is right
    // for capacity/streaming/overhead but wrong here: once the fault goes
    // live every request fails instantly, so a closed loop turns the outage
    // into tens of thousands of req/s of 500s, flooding the telemetry
    // pipeline for no benefit — measured at ~5.7k err/s / ~860k rows per run
    // before this fix). `paced_load_loop` below keeps a steady trickle
    // (~16 req/s) running through detection, the leak window, AND recovery:
    // the post-recovery successes are what let the passive tier flip back to
    // healthy, so MTTR stays measurable.
    let stats = Arc::new(Mutex::new(Stats::default()));
    // Controls the paced background load below; distinct from the
    // orchestrator's `stop` (threaded into `scenario` so its poll loops can
    // cut a Ctrl-C'd run short) so this function's own end-of-scenario
    // cleanup never taints the shared Ctrl-C flag.
    let load_stop = Arc::new(AtomicBool::new(false));
    let client = Arc::new(LoadClient::new(8));
    let loop_handle = {
        let (stats, load_stop, client) = (stats.clone(), load_stop.clone(), client.clone());
        let (proxy, k, m) = (proxy_base.to_string(), key.to_string(), model.to_string());
        tokio::spawn(async move { paced_load_loop(client, proxy, k, m, load_stop, stats).await })
    };

    // The whole scenario is wrapped so cleanup ALWAYS runs.
    let result = scenario(admin, ctl, model, &stats, effective_interval_s, &stop).await;

    load_stop.store(true, Ordering::Relaxed);
    let _ = loop_handle.await;
    if let Err(e) = ctl.set_fault(model, "ok").await {
        eprintln!(
            "warning: resilience failed to clear the injected fault on {model}: {e} — the benchmark backend may still be failing requests for it until cleared manually"
        );
    }
    if let Err(e) = admin
        .set_model_health_config(
            &id,
            prev_enabled,
            prev_alerts,
            prev_interval,
            prev_threshold,
        )
        .await
    {
        eprintln!(
            "warning: resilience failed to restore health-check config for {model}: {e} — it may be left at the tightened interval=1s/threshold=1 probe settings used during the scenario"
        );
    }

    result
}

/// Steady background load for the resilience scenario: `LOAD_CONC` workers,
/// each dispatching one non-streaming 4-token chat request and sleeping
/// `PACE_MS` before the next, until `stop` is set. Deliberately local to this
/// module rather than `run_closed_loop` — see the comment at its call site in
/// `run_resilience` for why a closed loop is the wrong tool once the fault is
/// live.
async fn paced_load_loop(
    client: Arc<LoadClient>,
    proxy_base: String,
    key: String,
    model: String,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<Stats>>,
) {
    let mut handles = Vec::new();
    for _ in 0..LOAD_CONC {
        let client = client.clone();
        let (proxy_base, key, model) = (proxy_base.clone(), key.clone(), model.clone());
        let stop = stop.clone();
        let stats = stats.clone();
        handles.push(tokio::spawn(async move {
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                let req = ProxyRequest::Chat(ChatRequest {
                    proxy_base: proxy_base.clone(),
                    key: key.clone(),
                    model: model.clone(),
                    input_tokens: 32,
                    output_tokens: 4,
                    stream: false,
                });
                let outcome = client.dispatch(&req).await;
                stats.lock().unwrap().record(&outcome);
                tokio::time::sleep(Duration::from_millis(PACE_MS)).await;
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

async fn scenario(
    admin: &crate::admin::AdminClient,
    ctl: &crate::backend_ctl::BackendControl,
    model: &str,
    stats: &Arc<Mutex<Stats>>,
    effective_interval_s: f64,
    stop: &Arc<AtomicBool>,
) -> anyhow::Result<ResilienceOutcome> {
    let status_of = |health: &serde_json::Value| -> String {
        health
            .as_array()
            .and_then(|a| a.iter().find(|h| h["model_name"] == model))
            .and_then(|h| h["status"].as_str())
            .unwrap_or("unknown")
            .to_string()
    };

    // No healthy warm-up phase here (deliberately removed): any success this
    // scenario itself streamed right before injecting sat inside the
    // gateway's passive-signal look-back window (>=300s — see the module
    // docs) and made the fault undetectable for the whole detect budget. The
    // background paced load started in `run_resilience` is enough of a
    // signal on its own once the fault flips it to failures.

    // Inject: model name keys the fault (backend matches by substring; the
    // obench-* names are non-overlapping — "turbo" only matches obench-turbo).
    ctl.set_fault(model, "fail").await?;
    let t0 = std::time::Instant::now();
    println!("    fault injected on {model}; waiting for the gateway to notice…");

    let mut mttd_s = None;
    while t0.elapsed().as_secs() < DETECT_BUDGET_S {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let h = admin
            .model_health()
            .await
            .unwrap_or(serde_json::Value::Null);
        let st = status_of(&h);
        if st == "unhealthy" || st == "degraded" {
            mttd_s = Some(t0.elapsed().as_secs_f64());
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(POLL_MS)).await;
    }
    let errors_before_detect = stats.lock().unwrap().error;

    // Post-detection leak window: 10s of continued load against a known-bad model.
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    let errors_after_detect = if mttd_s.is_some() {
        stats
            .lock()
            .unwrap()
            .error
            .saturating_sub(errors_before_detect)
    } else {
        0
    };

    // Recover.
    ctl.set_fault(model, "ok").await?;
    let t1 = std::time::Instant::now();
    let mut mttr_s = None;
    if mttd_s.is_some() {
        while t1.elapsed().as_secs() < RECOVER_BUDGET_S {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            let h = admin
                .model_health()
                .await
                .unwrap_or(serde_json::Value::Null);
            if status_of(&h) == "healthy" {
                mttr_s = Some(t1.elapsed().as_secs_f64());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(POLL_MS)).await;
        }
    }

    Ok(ResilienceOutcome {
        model: model.to_string(),
        mttd_s,
        mttr_s,
        errors_before_detect,
        errors_after_detect,
        effective_interval_s,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn o(mttd: Option<f64>, mttr: Option<f64>, after: u64) -> ResilienceOutcome {
        ResilienceOutcome {
            model: "obench-turbo".to_string(),
            mttd_s: mttd,
            mttr_s: mttr,
            errors_before_detect: 40,
            errors_after_detect: after,
            effective_interval_s: 60.0,
        }
    }

    #[test]
    fn first_cycle_detection_scores_100() {
        let (s, _) = resilience_score(&o(Some(70.0), Some(65.0), 0)); // ratios ~1.16 / ~1.08
        assert_eq!(s, 100);
    }

    #[test]
    fn never_detected_scores_10() {
        let (s, recs) = resilience_score(&o(None, None, 0));
        assert_eq!(s, 10);
        assert!(!recs.is_empty());
    }

    #[test]
    fn slow_detection_bands() {
        assert_eq!(resilience_score(&o(Some(130.0), Some(65.0), 0)).0, 85); // ratio ~2.2
        assert_eq!(resilience_score(&o(Some(200.0), Some(65.0), 0)).0, 65); // ratio ~3.3
        assert_eq!(resilience_score(&o(Some(400.0), Some(65.0), 0)).0, 35); // ratio ~6.7
    }

    #[test]
    fn leaked_requests_after_detection_penalized() {
        let (s, recs) = resilience_score(&o(Some(70.0), Some(65.0), 30));
        assert_eq!(s, 75); // 100 - min(25, 30)
        assert!(recs.iter().any(|r| r.contains("after")));
    }

    #[test]
    fn missing_recovery_penalized() {
        assert_eq!(resilience_score(&o(Some(70.0), None, 0)).0, 85); // 100 - 15
    }
}
