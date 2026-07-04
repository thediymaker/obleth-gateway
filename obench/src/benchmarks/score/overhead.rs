//! Proxy tax (demo only): identical load sent straight at the GPU-free
//! benchmark backend vs through the gateway, reporting the TTFB/throughput
//! delta the proxy adds. `256` may saturate the gateway on purpose — it's
//! reported for visibility but excluded from grading.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::benchmarks::score::{SectionId, SectionResult};
use crate::engine::load::{ChatRequest, LoadClient, ProxyRequest, RunConfig};
use crate::engine::stats::{Stats, Summary};

pub const OVERHEAD_CONCS: &[u32] = &[1, 64, 256];
pub const OVERHEAD_STEP_SECS: u64 = 12;

/// Concurrency above which points are reported but not graded (may saturate
/// the gateway on purpose).
const GRADED_CONC_MAX: u32 = 64;
/// Throughput-tax threshold: proxy req/s below this fraction of direct req/s
/// at the graded concurrency ceiling costs points.
const THROUGHPUT_TAX_RATIO: f64 = 0.85;
const THROUGHPUT_TAX_PENALTY: i32 = 15;

#[derive(Clone, Debug, Serialize)]
pub struct OverheadPoint {
    pub conc: u32,
    pub direct_p50_ttfb_ms: u64,
    pub proxy_p50_ttfb_ms: u64,
    pub direct_p99_ttfb_ms: u64,
    pub proxy_p99_ttfb_ms: u64,
    pub direct_rps: f64,
    pub proxy_rps: f64,
}

fn band(delta_ms: u64) -> u8 {
    if delta_ms < 5 {
        100
    } else if delta_ms < 15 {
        80
    } else if delta_ms < 40 {
        60
    } else if delta_ms < 80 {
        45
    } else {
        20
    }
}

pub fn overhead_section(points: &[OverheadPoint]) -> SectionResult {
    if points.is_empty() {
        return SectionResult::skipped(SectionId::Overhead, "no overhead measurements");
    }

    let delta_ms = points
        .iter()
        .filter(|p| p.conc <= GRADED_CONC_MAX)
        .map(|p| p.proxy_p50_ttfb_ms.saturating_sub(p.direct_p50_ttfb_ms))
        .max()
        .unwrap_or(0);

    let mut score = band(delta_ms) as i32;
    let mut recs = Vec::new();

    if let Some(p64) = points.iter().find(|p| p.conc == GRADED_CONC_MAX) {
        if p64.direct_rps > 0.0 {
            let ratio = p64.proxy_rps / p64.direct_rps;
            if ratio < THROUGHPUT_TAX_RATIO {
                score -= THROUGHPUT_TAX_PENALTY;
                let pct = (ratio * 100.0).round() as i64;
                recs.push(format!(
                    "throughput through the proxy is {pct}% of direct at conc 64"
                ));
            }
        }
    }

    let score = score.clamp(0, 100) as u8;

    if score < 75 {
        recs.push(format!("gateway adds {delta_ms}ms p50 TTFB"));
    }

    SectionResult {
        id: SectionId::Overhead,
        score: Some(score),
        metrics: serde_json::json!({ "points": points, "p50_overhead_ms": delta_ms }),
        recommendations: recs,
        error: None,
    }
}

pub async fn run_overhead(
    backend_base: &str,
    proxy_base: &str,
    key: &str,
    model: &str,
    input_tokens: u32,
) -> anyhow::Result<Vec<OverheadPoint>> {
    let mut points = Vec::new();

    for &conc in OVERHEAD_CONCS {
        let direct = run_leg(backend_base, "obench-direct", model, input_tokens, conc).await?;
        let proxy = run_leg(proxy_base, key, model, input_tokens, conc).await?;

        let point = OverheadPoint {
            conc,
            direct_p50_ttfb_ms: direct.p50_ttfb_ms,
            proxy_p50_ttfb_ms: proxy.p50_ttfb_ms,
            direct_p99_ttfb_ms: direct.p99_ttfb_ms,
            proxy_p99_ttfb_ms: proxy.p99_ttfb_ms,
            direct_rps: direct.req_per_s,
            proxy_rps: proxy.req_per_s,
        };
        println!(
            "    overhead conc={conc}: direct p50 {}ms / proxy p50 {}ms  direct {:.0} req/s / proxy {:.0} req/s",
            point.direct_p50_ttfb_ms, point.proxy_p50_ttfb_ms, point.direct_rps, point.proxy_rps
        );
        points.push(point);
    }

    Ok(points)
}

async fn run_leg(
    proxy_base: &str,
    key: &str,
    model: &str,
    input_tokens: u32,
    conc: u32,
) -> anyhow::Result<Summary> {
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
            output_tokens: 32,
            stream: true,
        })
    };
    let started = std::time::Instant::now();
    crate::engine::load::run_closed_loop(
        client,
        make_req,
        RunConfig {
            conc,
            duration_s: OVERHEAD_STEP_SECS,
            warmup_s: 2,
        },
        stop,
        stats.clone(),
    )
    .await;
    let elapsed = started.elapsed().as_secs_f64().max(1.0);
    let sum = stats.lock().unwrap().summarize(elapsed, 1.0);
    Ok(sum)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pt(conc: u32, d50: u64, p50: u64, drps: f64, prps: f64) -> OverheadPoint {
        OverheadPoint {
            conc,
            direct_p50_ttfb_ms: d50,
            proxy_p50_ttfb_ms: p50,
            direct_p99_ttfb_ms: d50 * 2,
            proxy_p99_ttfb_ms: p50 * 2,
            direct_rps: drps,
            proxy_rps: prps,
        }
    }

    #[test]
    fn tiny_overhead_is_a() {
        let pts = vec![pt(1, 20, 23, 40.0, 39.0), pt(64, 22, 25, 2000.0, 1950.0)];
        let r = overhead_section(&pts);
        assert_eq!(r.score, Some(100));
        assert_eq!(r.metrics["p50_overhead_ms"], 3);
    }

    #[test]
    fn overhead_bands() {
        let mk = |delta: u64| {
            vec![
                pt(1, 20, 20 + delta, 40.0, 40.0),
                pt(64, 20, 20 + delta, 2000.0, 2000.0),
            ]
        };
        assert_eq!(overhead_section(&mk(10)).score, Some(80));
        assert_eq!(overhead_section(&mk(30)).score, Some(60));
        assert_eq!(overhead_section(&mk(60)).score, Some(45));
        assert_eq!(overhead_section(&mk(120)).score, Some(20));
    }

    #[test]
    fn throughput_tax_penalized() {
        // 3ms delta (band 100) but proxy only pushes 70% of direct rps at 64.
        let pts = vec![pt(1, 20, 23, 40.0, 39.0), pt(64, 22, 25, 2000.0, 1400.0)];
        let r = overhead_section(&pts);
        assert_eq!(r.score, Some(85));
        assert!(r.recommendations.iter().any(|s| s.contains("throughput")));
    }

    #[test]
    fn empty_points_skipped() {
        assert_eq!(overhead_section(&[]).score, None);
    }
}
