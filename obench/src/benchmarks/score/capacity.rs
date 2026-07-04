//! Per-model capacity: a stepped concurrency ramp with knee detection, scoped
//! to one model at a time. Produces the headline "max sustainable load" card.

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::benchmarks::score::{SectionId, SectionResult};
use crate::engine::calibrate::{evaluate, Decision, KneeConfig, StepResult};
use crate::engine::load::{ChatRequest, LoadClient, ProxyRequest, RunConfig};
use crate::engine::stats::Stats;

pub const RAMP_STEPS: &[u32] = &[8, 32, 64, 128, 256, 512, 1024];
pub const STEP_SECS: u64 = 8;

#[derive(Clone, Debug, Serialize)]
pub struct StepData {
    pub conc: u32,
    pub req_per_s: f64,
    pub error_rate: f64,
    pub p50_ttfb_ms: u64,
    pub p99_ttfb_ms: u64,
    pub completed: u64,
    pub rejected: u64,
    pub errors: u64,
    pub out_tokens: u64,
    pub elapsed_s: f64,
    pub statuses: BTreeMap<u16, u64>,
}

#[derive(Clone, Debug, Serialize)]
pub struct CapacityCard {
    pub model: String,
    pub sustainable_conc: u32,
    pub peak_req_per_s: f64,
    pub out_tok_per_s: f64,
    pub p50_ttfb_at_knee_ms: u64,
    pub p99_ttfb_at_knee_ms: u64,
    pub knee_reason: Option<String>,
    pub clean_steps: Vec<StepData>,
    pub over_knee: Option<StepData>,
}

pub fn card_from_steps(
    model: &str,
    clean: Vec<StepData>,
    stop: Option<(StepData, String)>,
) -> CapacityCard {
    let knee = clean.last();
    let (sustainable_conc, peak_req_per_s, p50, p99, out_tok_per_s) = match knee {
        Some(k) => (
            k.conc,
            k.req_per_s,
            k.p50_ttfb_ms,
            k.p99_ttfb_ms,
            if k.elapsed_s > 0.0 {
                k.out_tokens as f64 / k.elapsed_s
            } else {
                0.0
            },
        ),
        None => (0, 0.0, 0, 0, 0.0),
    };
    let (over_knee, knee_reason) = match stop {
        Some((s, r)) => (Some(s), Some(r)),
        None => (None, None),
    };
    CapacityCard {
        model: model.to_string(),
        sustainable_conc,
        peak_req_per_s,
        out_tok_per_s,
        p50_ttfb_at_knee_ms: p50,
        p99_ttfb_at_knee_ms: p99,
        knee_reason,
        clean_steps: clean,
        over_knee,
    }
}

/// Capacity is hardware-relative, so the score is availability-shaped: every
/// model that sustains at least the first ramp step scores 100; a model that
/// can't even hold the first step scores 0 and drags the mean. Absolute
/// numbers are information (and regression fodder), not grades.
pub fn capacity_section(cards: &[CapacityCard]) -> SectionResult {
    if cards.is_empty() {
        return SectionResult::skipped(SectionId::Capacity, "no models in scope");
    }
    let mut recs = Vec::new();
    let mut total = 0u32;
    for c in cards {
        if c.sustainable_conc == 0 {
            recs.push(format!(
                "model {} could not sustain even conc={} ({}) — investigate before relying on it",
                c.model,
                RAMP_STEPS[0],
                c.knee_reason.as_deref().unwrap_or("no clean step")
            ));
        } else {
            total += 100;
        }
    }
    let score = (total / cards.len() as u32) as u8;
    SectionResult {
        id: SectionId::Capacity,
        score: Some(score),
        metrics: serde_json::json!({ "models": cards }),
        recommendations: recs,
        error: None,
    }
}

pub async fn run_ramp(
    model: &str,
    key: &str,
    proxy_base: &str,
    input_tokens: u32,
    max_conc: u32,
) -> anyhow::Result<CapacityCard> {
    let cfg = KneeConfig::default();
    let mut history: Vec<StepResult> = Vec::new();
    let mut clean: Vec<StepData> = Vec::new();
    let mut stop_step: Option<(StepData, String)> = None;

    for &conc in RAMP_STEPS.iter().filter(|&&c| c <= max_conc) {
        let client = Arc::new(LoadClient::new((conc as usize) * 2));
        let stats = Arc::new(Mutex::new(Stats::default()));
        let stop = Arc::new(AtomicBool::new(false));
        let (proxy, k, m) = (proxy_base.to_string(), key.to_string(), model.to_string());
        let make_req = move || {
            ProxyRequest::Chat(ChatRequest {
                proxy_base: proxy.clone(),
                key: k.clone(),
                model: m.clone(),
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
        let step = {
            let s = stats.lock().unwrap();
            let sum = s.summarize(elapsed, 1.0);
            StepData {
                conc,
                req_per_s: sum.req_per_s,
                error_rate: sum.error_rate,
                p50_ttfb_ms: sum.p50_ttfb_ms,
                p99_ttfb_ms: sum.p99_ttfb_ms,
                completed: sum.completed,
                rejected: sum.rejected,
                errors: sum.errors,
                out_tokens: sum.out_tokens,
                elapsed_s: elapsed,
                statuses: s.statuses.clone(),
            }
        };
        println!(
            "    {model} conc={conc}: {:.0} req/s  err {:.2}%  p99 {}ms",
            step.req_per_s,
            step.error_rate * 100.0,
            step.p99_ttfb_ms
        );
        let latest = StepResult {
            conc,
            req_per_s: step.req_per_s,
            error_rate: step.error_rate,
            p99_ttfb_ms: step.p99_ttfb_ms,
            max_queued: 0,
        };
        match evaluate(&history, &latest, &cfg) {
            Decision::Continue => {
                history.push(latest);
                clean.push(step);
            }
            Decision::Stop { reason, .. } => {
                stop_step = Some((step, reason));
                break;
            }
        }
    }
    Ok(card_from_steps(model, clean, stop_step))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sd(conc: u32, rps: f64, err: f64, p99: u64) -> StepData {
        StepData {
            conc,
            req_per_s: rps,
            error_rate: err,
            p50_ttfb_ms: p99 / 2,
            p99_ttfb_ms: p99,
            completed: (rps * STEP_SECS as f64) as u64,
            rejected: 0,
            errors: 0,
            out_tokens: 4000,
            elapsed_s: STEP_SECS as f64,
            statuses: Default::default(),
        }
    }

    #[test]
    fn card_reports_knee_from_last_clean_step() {
        let clean = vec![sd(8, 100.0, 0.0, 30), sd(32, 380.0, 0.0, 35)];
        let stop = Some((sd(64, 390.0, 0.08, 200), "error rate crossed".to_string()));
        let card = card_from_steps("m1", clean, stop);
        assert_eq!(card.sustainable_conc, 32);
        assert_eq!(card.p99_ttfb_at_knee_ms, 35);
        assert!((card.peak_req_per_s - 380.0).abs() < 0.01);
        assert!(card.over_knee.is_some());
        assert_eq!(card.knee_reason.as_deref(), Some("error rate crossed"));
        assert!(card.out_tok_per_s > 0.0);
    }

    #[test]
    fn card_with_no_knee_uses_last_step() {
        let clean = vec![sd(8, 100.0, 0.0, 30), sd(32, 380.0, 0.0, 35)];
        let card = card_from_steps("m1", clean, None);
        assert_eq!(card.sustainable_conc, 32);
        assert!(card.over_knee.is_none());
        assert!(card.knee_reason.is_none());
    }

    #[test]
    fn card_with_immediate_failure_is_zero() {
        let card = card_from_steps("m1", vec![], Some((sd(8, 10.0, 0.5, 900), "errors".into())));
        assert_eq!(card.sustainable_conc, 0);
        assert_eq!(card.peak_req_per_s, 0.0);
    }

    #[test]
    fn section_scores_healthy_fleet_100() {
        let cards = vec![
            card_from_steps("a", vec![sd(8, 100.0, 0.0, 30)], None),
            card_from_steps("b", vec![sd(8, 90.0, 0.0, 40)], None),
        ];
        let r = capacity_section(&cards);
        assert_eq!(r.score, Some(100));
        assert_eq!(r.metrics["models"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn section_penalizes_dead_model_and_recommends() {
        let cards = vec![
            card_from_steps("a", vec![sd(8, 100.0, 0.0, 30)], None),
            card_from_steps("b", vec![], Some((sd(8, 1.0, 0.9, 900), "errors".into()))),
        ];
        let r = capacity_section(&cards);
        assert_eq!(r.score, Some(50));
        assert!(r.recommendations.iter().any(|s| s.contains("b")));
    }
}
