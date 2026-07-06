//! Overload behavior: grade what happens PAST the knee. A good gateway sheds
//! excess load as fast clean 429s with bounded latency; a bad one hangs
//! sockets, 500s, and lets p99 run away. Zero extra runtime — this section
//! reuses the over-knee step the capacity ramp already paid for.

use crate::benchmarks::score::capacity::CapacityCard;
use crate::benchmarks::score::{SectionId, SectionResult};

const LATENCY_BLOWUP_FACTOR: f64 = 4.0;
const LATENCY_BLOWUP_PENALTY: i32 = 25;

pub fn overload_section(cards: &[CapacityCard]) -> SectionResult {
    let mut model_scores: Vec<(String, i32, serde_json::Value)> = Vec::new();
    let mut recs = Vec::new();

    for c in cards {
        let Some(over) = &c.over_knee else { continue };
        let clean: u64 = *over.statuses.get(&429).unwrap_or(&0);
        let dirty: u64 = over
            .statuses
            .iter()
            .filter(|(s, _)| **s >= 500 || **s == 0)
            .map(|(_, n)| n)
            .sum();
        if clean + dirty == 0 {
            continue;
        }
        let mut score = (100.0 * clean as f64 / (clean + dirty) as f64).round() as i32;
        let knee_p99 = c.p99_ttfb_at_knee_ms.max(1) as f64;
        let blowup = over.p99_ttfb_ms as f64 > knee_p99 * LATENCY_BLOWUP_FACTOR;
        if blowup {
            score -= LATENCY_BLOWUP_PENALTY;
        }
        let score = score.clamp(0, 100);
        if score < 75 {
            recs.push(format!(
                "model {}: overload produced {} dirty failures (5xx/hangs) vs {} clean 429s{} — check admission control / queue limits",
                c.model, dirty, clean,
                if blowup { ", and p99 blew past 4x the knee" } else { "" }
            ));
        }
        model_scores.push((
            c.model.clone(),
            score,
            serde_json::json!({
                "model": c.model, "clean_429": clean, "dirty": dirty,
                "over_knee_p99_ms": over.p99_ttfb_ms, "knee_p99_ms": c.p99_ttfb_at_knee_ms,
                "score": score,
            }),
        ));
    }

    if model_scores.is_empty() {
        return SectionResult::skipped(
            SectionId::Overload,
            "no model was pushed past its knee (all clean within the ramp cap)",
        );
    }
    let mean =
        (model_scores.iter().map(|(_, s, _)| *s).sum::<i32>() / model_scores.len() as i32) as u8;
    SectionResult {
        id: SectionId::Overload,
        score: Some(mean),
        metrics: serde_json::json!({
            "models": model_scores.iter().map(|(_, _, m)| m.clone()).collect::<Vec<_>>()
        }),
        recommendations: recs,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::benchmarks::score::capacity::{card_from_steps, StepData, STEP_SECS};
    use std::collections::BTreeMap;

    fn sd(conc: u32, p99: u64, statuses: &[(u16, u64)]) -> StepData {
        let map: BTreeMap<u16, u64> = statuses.iter().copied().collect();
        let rejected = *map.get(&429).unwrap_or(&0);
        let errors: u64 = map
            .iter()
            .filter(|(s, _)| **s >= 500 || **s == 0)
            .map(|(_, n)| n)
            .sum();
        let completed = *map.get(&200).unwrap_or(&0);
        StepData {
            conc,
            req_per_s: completed as f64 / STEP_SECS as f64,
            error_rate: if completed + rejected + errors > 0 {
                errors as f64 / (completed + rejected + errors) as f64
            } else {
                0.0
            },
            p50_ttfb_ms: p99 / 2,
            p99_ttfb_ms: p99,
            completed,
            rejected,
            errors,
            out_tokens: 0,
            elapsed_s: STEP_SECS as f64,
            statuses: map,
        }
    }

    #[test]
    fn graceful_overload_scores_100() {
        // Past the knee: all excess load cleanly 429'd, latency bounded.
        let clean = vec![sd(32, 40, &[(200, 3000)])];
        let over = sd(64, 60, &[(200, 3000), (429, 500)]);
        let card = card_from_steps("m", clean, Some((over, "errors".into())));
        let r = overload_section(&[card]);
        assert_eq!(r.score, Some(100));
    }

    #[test]
    fn catastrophic_overload_scores_low() {
        // Past the knee: 500s and hangs, p99 exploded (40 -> 900 > 4x).
        let clean = vec![sd(32, 40, &[(200, 3000)])];
        let over = sd(64, 900, &[(200, 100), (500, 400), (0, 100)]);
        let card = card_from_steps("m", clean, Some((over, "errors".into())));
        let r = overload_section(&[card]);
        assert_eq!(r.score, Some(0)); // 0% clean, minus latency penalty, floored
        assert!(!r.recommendations.is_empty());
    }

    #[test]
    fn mixed_overload_is_proportional() {
        // 300 clean 429s vs 100 dirty 500s = 75%, latency bounded.
        let clean = vec![sd(32, 40, &[(200, 3000)])];
        let over = sd(64, 100, &[(429, 300), (500, 100)]);
        let card = card_from_steps("m", clean, Some((over, "errors".into())));
        assert_eq!(overload_section(&[card]).score, Some(75));
    }

    #[test]
    fn no_over_knee_data_is_skipped() {
        let card = card_from_steps("m", vec![sd(32, 40, &[(200, 100)])], None);
        let r = overload_section(&[card]);
        assert_eq!(r.score, None);
        assert!(r.error.is_none());
    }
}
