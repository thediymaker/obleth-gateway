//! Streaming quality: inter-chunk jitter and mid-stream stalls as seen by a
//! client. On the demo target the backend emits tokens on a fixed cadence, so
//! any jitter measured here is gateway-introduced and graded; on live we only
//! grade stalls (real model cadence is legitimately uneven).

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use serde::Serialize;

use crate::benchmarks::score::{SectionId, SectionResult};
use crate::engine::load::{ChatRequest, LoadClient, ProxyRequest, RunConfig};
use crate::engine::stats::{Stats, Summary};

const JITTER_RATIO_FLOOR: f64 = 3.0;

#[derive(Clone, Debug, Serialize)]
pub struct StreamQuality {
    pub model: String,
    pub conc: u32,
    pub p50_gap_ms: u64,
    pub p99_gap_ms: u64,
    pub jitter_ratio: f64,
    pub stall_events: u64,
    pub completed: u64,
    pub gap_samples: u64,
}

pub fn quality_from_summary(model: &str, conc: u32, s: &Summary) -> Option<StreamQuality> {
    if s.gap_samples == 0 {
        return None;
    }
    Some(StreamQuality {
        model: model.to_string(),
        conc,
        p50_gap_ms: s.p50_gap_ms,
        p99_gap_ms: s.p99_gap_ms,
        jitter_ratio: s.p99_gap_ms as f64 / s.p50_gap_ms.max(1) as f64,
        stall_events: s.stall_events,
        completed: s.completed,
        gap_samples: s.gap_samples,
    })
}

pub fn streaming_section(quals: &[StreamQuality], oracle: bool) -> SectionResult {
    if quals.is_empty() {
        return SectionResult::skipped(SectionId::Streaming, "no streaming samples collected");
    }
    let mut recs = Vec::new();
    let mut sum = 0i32;
    for q in quals {
        let mut score = 100i32;
        score -= (q.stall_events.min(5) as i32) * 10;
        if oracle && q.jitter_ratio > JITTER_RATIO_FLOOR {
            score -= (((q.jitter_ratio - JITTER_RATIO_FLOOR) * 10.0).round() as i32).min(40);
        }
        let score = score.clamp(0, 100);
        if q.stall_events > 0 {
            recs.push(format!(
                "model {}: {} mid-stream stalls (gap >= 1s) at conc {} — check proxy buffering / upstream timeouts",
                q.model, q.stall_events, q.conc
            ));
        }
        sum += score;
    }
    SectionResult {
        id: SectionId::Streaming,
        score: Some((sum / quals.len() as i32) as u8),
        metrics: serde_json::json!({ "models": quals }),
        recommendations: recs,
        error: None,
    }
}

pub async fn run_streaming(
    model: &str,
    key: &str,
    proxy_base: &str,
    input_tokens: u32,
    conc: u32,
    stop: Arc<AtomicBool>,
) -> anyhow::Result<Summary> {
    let client = Arc::new(LoadClient::new((conc as usize) * 2));
    let stats = Arc::new(Mutex::new(Stats::default()));
    let (proxy, k, m) = (proxy_base.to_string(), key.to_string(), model.to_string());
    let make_req = move || {
        ProxyRequest::Chat(ChatRequest {
            proxy_base: proxy.clone(),
            key: k.clone(),
            model: m.clone(),
            input_tokens,
            output_tokens: 64,
            stream: true,
        })
    };
    let started = std::time::Instant::now();
    crate::engine::load::run_closed_loop(
        client,
        make_req,
        RunConfig {
            conc,
            duration_s: 20,
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

    fn q(p50: u64, p99: u64, stalls: u64) -> StreamQuality {
        StreamQuality {
            model: "m".into(),
            conc: 8,
            p50_gap_ms: p50,
            p99_gap_ms: p99,
            jitter_ratio: p99 as f64 / p50.max(1) as f64,
            stall_events: stalls,
            completed: 100,
            gap_samples: 6400,
        }
    }

    #[test]
    fn clean_stream_scores_100() {
        let r = streaming_section(&[q(5, 8, 0)], true);
        assert_eq!(r.score, Some(100));
    }

    #[test]
    fn stalls_penalize_both_modes() {
        assert_eq!(streaming_section(&[q(5, 8, 2)], true).score, Some(80));
        assert_eq!(streaming_section(&[q(5, 8, 2)], false).score, Some(80));
        assert_eq!(streaming_section(&[q(5, 8, 9)], false).score, Some(50)); // capped at -50
    }

    #[test]
    fn jitter_penalized_only_with_oracle() {
        let jittery = q(5, 40, 0); // ratio 8.0 -> -(8-3)*10 = -40 (capped)
        assert_eq!(
            streaming_section(std::slice::from_ref(&jittery), true).score,
            Some(60)
        );
        assert_eq!(streaming_section(&[jittery], false).score, Some(100));
    }

    #[test]
    fn empty_input_is_skipped() {
        assert_eq!(streaming_section(&[], true).score, None);
    }

    #[test]
    fn quality_from_summary_none_without_gaps() {
        let s = crate::engine::stats::Stats::default().summarize(1.0, 1.0);
        assert!(quality_from_summary("m", 8, &s).is_none());
    }
}
