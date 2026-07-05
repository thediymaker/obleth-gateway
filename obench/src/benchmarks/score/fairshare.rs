//! Fairshare dynamics: under forced contention (capacity clamped well below
//! aggregate demand), grade how well the gateway arbitrates between tenant
//! groups. Three signals: steady-state Jain fairness index (are groups
//! getting their weighted share once things settle?), convergence time after
//! a mid-run tenant injection (how fast does the split recover?), and
//! starvation (does any group go dark for an extended stretch?).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::benchmarks::score::{SectionId, SectionResult};
use crate::engine::fleet::{FIXTURE_GROUPS, FIXTURE_TENANTS};
use crate::engine::load::{ChatRequest, LoadClient, ProxyRequest, RunConfig};
use crate::engine::stats::Stats;

const CONTENTION_CAPACITY: u32 = 16;
const PER_TENANT_CONC: u32 = 12;
const INJECT_AT_S: u64 = 20;
const TOTAL_S: u64 = 80;
const SAMPLE_EVERY_S: u64 = 2;
const MODEL: &str = "obench-base";

const CONVERGENCE_TOL: f64 = 0.10;
const STEADY_STATE_FROM_S: f64 = 50.0;
const STARVATION_STREAK: u32 = 5;

/// Jain's fairness index over a set of observed/expected ratios.
/// `1.0` = perfectly fair, `1/n` = maximally unfair (all to one party).
/// Empty input, or all-zero input (Σx² == 0), is defined as `0.0`.
pub fn jain_index(ratios: &[f64]) -> f64 {
    if ratios.is_empty() {
        return 0.0;
    }
    let sum: f64 = ratios.iter().sum();
    let sum_sq: f64 = ratios.iter().map(|x| x * x).sum();
    if sum_sq == 0.0 {
        return 0.0;
    }
    (sum * sum) / (ratios.len() as f64 * sum_sq)
}

/// One sampled window's per-group observed share of completions (sums to 1.0).
#[derive(Clone, Debug, Serialize)]
pub struct SharePoint {
    pub t_s: f64,
    pub shares: Vec<f64>,
}

fn within_tol(shares: &[f64], expected: &[f64], tol: f64) -> bool {
    shares.len() == expected.len()
        && shares
            .iter()
            .zip(expected.iter())
            .all(|(s, e)| (s - e).abs() <= tol)
}

/// First point at `t_s >= from_t` that starts a run of 3 consecutive points
/// all within `tol` (absolute) of `expected` on every group. Returns the
/// elapsed time from `from_t` to that first point, or `None` if the series
/// never stabilizes.
pub fn convergence_time_s(
    series: &[SharePoint],
    expected: &[f64],
    tol: f64,
    from_t: f64,
) -> Option<f64> {
    let filtered: Vec<&SharePoint> = series.iter().filter(|p| p.t_s >= from_t).collect();
    filtered
        .windows(3)
        .find(|w| w.iter().all(|p| within_tol(&p.shares, expected, tol)))
        .map(|w| w[0].t_s - from_t)
}

/// Pure scoring: base is the Jain index rescaled so 0.5 -> 0 and 1.0 -> 100
/// (clamped before penalties), then convergence and starvation penalties are
/// applied and the result re-clamped to 0..100.
pub fn fairshare_score(jain: f64, convergence_s: Option<f64>, starved: bool) -> (u8, Vec<String>) {
    let mut recs = Vec::new();
    let base = (100.0 * (jain - 0.5) / 0.5).clamp(0.0, 100.0);
    let mut score = base;

    match convergence_s {
        None => {
            score -= 30.0;
            recs.push(
                "fairshare never converged to the expected group split within the run — check weight propagation / admission fairness under contention"
                    .to_string(),
            );
        }
        Some(t) if t > 30.0 => {
            score -= 20.0;
            recs.push(format!(
                "fairshare took {t:.0}s to converge after the tenant injection — investigate slow rebalancing"
            ));
        }
        Some(t) if t > 15.0 => {
            score -= 10.0;
            recs.push(format!(
                "fairshare took {t:.0}s to converge after the tenant injection — investigate slow rebalancing"
            ));
        }
        Some(_) => {}
    }

    if starved {
        score -= 40.0;
        recs.push(
            "a tenant group saw zero completions for an extended stretch after contention began — investigate starvation under load"
                .to_string(),
        );
    }

    (score.clamp(0.0, 100.0).round() as u8, recs)
}

pub async fn run_fairshare(
    admin: &crate::admin::AdminClient,
    proxy_base: &str,
    seeded: &crate::seed::SeededRun,
) -> anyhow::Result<SectionResult> {
    // Force queueing so fairshare actually arbitrates.
    let prev_capacity = admin.get_capacity().await?;
    admin.set_capacity(CONTENTION_CAPACITY).await?;

    let result = scenario(proxy_base, seeded).await;

    // Restore on every path.
    let _ = admin.set_capacity(prev_capacity).await;
    result
}

struct TenantHandle {
    group_idx: usize,
    stats: Arc<Mutex<Stats>>,
}

async fn scenario(
    proxy_base: &str,
    seeded: &crate::seed::SeededRun,
) -> anyhow::Result<SectionResult> {
    let group_names: Vec<&str> = FIXTURE_GROUPS.iter().map(|g| g.0).collect();
    let total_weight: u32 = FIXTURE_GROUPS.iter().map(|g| g.1).sum();
    let expected: Vec<f64> = FIXTURE_GROUPS
        .iter()
        .map(|g| g.1 as f64 / total_weight as f64)
        .collect();

    let stop = Arc::new(AtomicBool::new(false));
    let client = Arc::new(LoadClient::new((PER_TENANT_CONC as usize) * 2));

    let mut tenant_handles: Vec<TenantHandle> = Vec::new();
    let mut join_handles = Vec::new();

    // t=0 for the whole scenario: as close as possible to when the immediate
    // (chatbot-group) loops are spawned.
    let started = std::time::Instant::now();

    for tenant in &seeded.tenants {
        let Some(entry) = FIXTURE_TENANTS.iter().find(|t| t.0 == tenant.name.as_str()) else {
            continue;
        };
        let group = entry.1;
        let Some(group_idx) = group_names.iter().position(|g| *g == group) else {
            continue;
        };
        let is_chatbot = group == "obench-chatbot";

        let stats = Arc::new(Mutex::new(Stats::default()));

        let proxy = proxy_base.to_string();
        let key = tenant.key.clone();
        let model = MODEL.to_string();
        let make_req = move || {
            ProxyRequest::Chat(ChatRequest {
                proxy_base: proxy.clone(),
                key: key.clone(),
                model: model.clone(),
                input_tokens: 64,
                output_tokens: 16,
                stream: false,
            })
        };
        let run_cfg = RunConfig {
            conc: PER_TENANT_CONC,
            duration_s: 0,
            warmup_s: 0,
        };

        let task_client = client.clone();
        let task_stop = stop.clone();
        let task_stats = stats.clone();

        let jh = if is_chatbot {
            tokio::spawn(async move {
                crate::engine::load::run_closed_loop(
                    task_client,
                    make_req,
                    run_cfg,
                    task_stop,
                    task_stats,
                )
                .await;
            })
        } else {
            // The other three groups' tenants join mid-run, sharing the same
            // stop flag so the final `stop.store` + join drains them too.
            tokio::spawn(async move {
                tokio::time::sleep(std::time::Duration::from_secs(INJECT_AT_S)).await;
                crate::engine::load::run_closed_loop(
                    task_client,
                    make_req,
                    run_cfg,
                    task_stop,
                    task_stats,
                )
                .await;
            })
        };

        join_handles.push(jh);
        tenant_handles.push(TenantHandle { group_idx, stats });
    }

    // Sampler: windowed per-group completions every SAMPLE_EVERY_S.
    let mut last_ok = vec![0u64; group_names.len()];
    let mut zero_streak = vec![0u32; group_names.len()];
    let mut starved = false;
    let mut series: Vec<SharePoint> = Vec::new();
    let starvation_from_s = (INJECT_AT_S + 10) as f64;

    while started.elapsed().as_secs() < TOTAL_S {
        tokio::time::sleep(std::time::Duration::from_secs(SAMPLE_EVERY_S)).await;
        let t_s = started.elapsed().as_secs_f64();

        let mut cur = vec![0u64; group_names.len()];
        for th in &tenant_handles {
            cur[th.group_idx] += th.stats.lock().unwrap().ok;
        }
        let deltas: Vec<i64> = cur
            .iter()
            .zip(&last_ok)
            .map(|(c, l)| *c as i64 - *l as i64)
            .collect();
        let total_delta: i64 = deltas.iter().sum();

        if total_delta > 0 {
            let shares: Vec<f64> = deltas
                .iter()
                .map(|d| *d as f64 / total_delta as f64)
                .collect();
            series.push(SharePoint { t_s, shares });
        }

        if t_s >= starvation_from_s {
            for (i, d) in deltas.iter().enumerate() {
                if expected[i] > 0.0 {
                    if *d == 0 {
                        zero_streak[i] += 1;
                        if zero_streak[i] >= STARVATION_STREAK {
                            starved = true;
                        }
                    } else {
                        zero_streak[i] = 0;
                    }
                }
            }
        }

        last_ok = cur;
    }

    stop.store(true, Ordering::Relaxed);
    for jh in join_handles {
        let _ = jh.await;
    }

    // Steady-state Jain: average each group's observed share over the tail of
    // the run, compare to its expected weight.
    let steady: Vec<&SharePoint> = series
        .iter()
        .filter(|p| p.t_s >= STEADY_STATE_FROM_S)
        .collect();
    let mut avg_share = vec![0.0f64; group_names.len()];
    if !steady.is_empty() {
        for p in &steady {
            for (i, s) in p.shares.iter().enumerate() {
                avg_share[i] += s;
            }
        }
        for v in avg_share.iter_mut() {
            *v /= steady.len() as f64;
        }
    }
    let ratios: Vec<f64> = avg_share
        .iter()
        .zip(&expected)
        .map(|(a, e)| if *e > 0.0 { a / e } else { 0.0 })
        .collect();
    let jain = jain_index(&ratios);

    let convergence_s = convergence_time_s(&series, &expected, CONVERGENCE_TOL, INJECT_AT_S as f64);

    let (score, recommendations) = fairshare_score(jain, convergence_s, starved);

    Ok(SectionResult {
        id: SectionId::Fairshare,
        score: Some(score),
        metrics: serde_json::json!({
            "jain": jain,
            "convergence_s": convergence_s,
            "starved": starved,
            "expected_shares": expected,
            "steady_shares": avg_share,
            "series": series,
        }),
        recommendations,
        error: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jain_perfect_fairness_is_1() {
        assert!((jain_index(&[1.0, 1.0, 1.0]) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn jain_total_unfairness_approaches_1_over_n() {
        let j = jain_index(&[1.0, 0.0, 0.0]);
        assert!((j - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(jain_index(&[]), 0.0);
    }

    fn pt(t: f64, shares: &[f64]) -> SharePoint {
        SharePoint {
            t_s: t,
            shares: shares.to_vec(),
        }
    }

    #[test]
    fn convergence_needs_three_consecutive_points() {
        let expected = [0.7, 0.3];
        let series = vec![
            pt(20.0, &[0.95, 0.05]), // not yet
            pt(22.0, &[0.75, 0.25]), // within tol (1)
            pt(24.0, &[0.72, 0.28]), // within tol (2)
            pt(26.0, &[0.71, 0.29]), // within tol (3) -> converged at t=22
        ];
        assert_eq!(
            convergence_time_s(&series, &expected, 0.10, 20.0),
            Some(2.0)
        );
    }

    #[test]
    fn convergence_none_when_never_stable() {
        let expected = [0.7, 0.3];
        let series = vec![pt(20.0, &[0.95, 0.05]), pt(22.0, &[0.9, 0.1])];
        assert_eq!(convergence_time_s(&series, &expected, 0.10, 20.0), None);
    }

    #[test]
    fn score_perfect() {
        let (s, _) = fairshare_score(1.0, Some(5.0), false);
        assert_eq!(s, 100);
    }

    #[test]
    fn score_penalties_stack() {
        let (s, recs) = fairshare_score(1.0, None, true); // -30 no convergence, -40 starved
        assert_eq!(s, 30);
        assert_eq!(recs.len(), 2);
        let (s2, _) = fairshare_score(0.75, Some(20.0), false); // 50 - 10
        assert_eq!(s2, 40);
    }
}
