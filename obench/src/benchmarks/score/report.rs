//! Deployment scorecard: renders section results into a persisted report,
//! stores baselines under `BENCH_OUT_DIR/scorecards/`, and diffs a run
//! against its most recent baseline to flag regressions.

use std::fs::{create_dir_all, File};
use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{grade_from_score, SectionResult};

/// Fraction below which a drop in sustainable concurrency counts as a
/// capacity regression.
const CAPACITY_DROP_RATIO: f64 = 0.85;
/// Multiplier above which a rise in knee-latency or proxy overhead counts as
/// a regression.
const RISE_RATIO: f64 = 1.25;
/// Minimum absolute increase (ms) required before a proxy-overhead rise is
/// flagged, so sub-millisecond noise near the ratio threshold doesn't count.
const OVERHEAD_MIN_DELTA_MS: f64 = 3.0;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SectionRecord {
    pub id: String,
    pub score: Option<u8>,
    pub grade: String,
    pub metrics: serde_json::Value,
    pub recommendations: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Scorecard {
    pub target: String,
    pub created_unix: u64,
    pub gateway_version: String,
    pub system_score: Option<u8>,
    pub sections: Vec<SectionRecord>,
}

#[derive(Debug, Clone)]
pub struct Regression {
    pub section: String,
    pub message: String,
}

pub fn record_sections(results: &[SectionResult]) -> Vec<SectionRecord> {
    results
        .iter()
        .map(|r| SectionRecord {
            id: r.id.name().to_string(),
            score: r.score,
            grade: r.grade().letter().to_string(),
            metrics: r.metrics.clone(),
            recommendations: r.recommendations.clone(),
            error: r.error.clone(),
        })
        .collect()
}

fn find_section<'a>(card: &'a Scorecard, id: &str) -> Option<&'a SectionRecord> {
    card.sections.iter().find(|s| s.id == id)
}

fn num(v: &serde_json::Value, key: &str) -> Option<f64> {
    v.get(key).and_then(|x| x.as_f64())
}

/// Format a metric value without a spurious trailing `.0` for whole numbers.
fn fmt_num(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

pub fn diff(current: &Scorecard, baseline: &Scorecard) -> Vec<Regression> {
    let mut regs = Vec::new();

    if let (Some(cur_cap), Some(base_cap)) = (
        find_section(current, "capacity"),
        find_section(baseline, "capacity"),
    ) {
        let empty = Vec::new();
        let cur_models = cur_cap
            .metrics
            .get("models")
            .and_then(|m| m.as_array())
            .unwrap_or(&empty);
        let base_models = base_cap
            .metrics
            .get("models")
            .and_then(|m| m.as_array())
            .unwrap_or(&empty);

        for cm in cur_models {
            let Some(name) = cm.get("model").and_then(|v| v.as_str()) else {
                continue;
            };
            let Some(bm) = base_models
                .iter()
                .find(|b| b.get("model").and_then(|v| v.as_str()) == Some(name))
            else {
                continue;
            };

            if let (Some(cur_sc), Some(base_sc)) =
                (num(cm, "sustainable_conc"), num(bm, "sustainable_conc"))
            {
                if base_sc > 0.0 && cur_sc < base_sc * CAPACITY_DROP_RATIO {
                    let (old, new) = (fmt_num(base_sc), fmt_num(cur_sc));
                    regs.push(Regression {
                        section: "capacity".into(),
                        message: format!("capacity: {name} sustainable concurrency {old} → {new}"),
                    });
                }
            }

            if let (Some(cur_p99), Some(base_p99)) = (
                num(cm, "p99_ttfb_at_knee_ms"),
                num(bm, "p99_ttfb_at_knee_ms"),
            ) {
                if base_p99 > 0.0 && cur_p99 > 0.0 && cur_p99 > base_p99 * RISE_RATIO {
                    let (old, new) = (fmt_num(base_p99), fmt_num(cur_p99));
                    regs.push(Regression {
                        section: "capacity".into(),
                        message: format!("latency: {name} p99 at knee {old}ms → {new}ms"),
                    });
                }
            }
        }
    }

    if let (Some(cur_oh), Some(base_oh)) = (
        find_section(current, "overhead"),
        find_section(baseline, "overhead"),
    ) {
        if let (Some(cur_p50), Some(base_p50)) = (
            num(&cur_oh.metrics, "p50_overhead_ms"),
            num(&base_oh.metrics, "p50_overhead_ms"),
        ) {
            let delta = cur_p50 - base_p50;
            if cur_p50 > base_p50 * RISE_RATIO && delta >= OVERHEAD_MIN_DELTA_MS {
                let (old, new) = (fmt_num(base_p50), fmt_num(cur_p50));
                regs.push(Regression {
                    section: "overhead".into(),
                    message: format!("overhead: proxy tax {old}ms → {new}ms p50"),
                });
            }
        }
    }

    regs
}

/// Caps each affected section's score at 60. Does not recompute the system
/// score — that's the orchestrator's job, since it alone knows the weights.
/// Does recompute the section's own `grade` string, since a capped score can
/// drop it a letter grade (e.g. A/100 -> C/60) and leaving the stale letter
/// behind would show `"score": 60, "grade": "A"` in both the table and JSON.
pub fn apply_regressions(card: &mut Scorecard, regressions: &[Regression]) {
    for reg in regressions {
        if let Some(sec) = card.sections.iter_mut().find(|s| s.id == reg.section) {
            sec.score = sec.score.map(|s| s.min(60));
            if let Some(s) = sec.score {
                sec.grade = grade_from_score(s).letter().to_string();
            }
        }
    }
}

fn headline(sec: &SectionRecord) -> String {
    match sec.id.as_str() {
        "capacity" => {
            let n = sec
                .metrics
                .get("models")
                .and_then(|m| m.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            format!("{n} models")
        }
        "overhead" => match num(&sec.metrics, "p50_overhead_ms") {
            Some(v) => format!("{}ms p50 added", fmt_num(v)),
            None => String::new(),
        },
        "resilience" => match num(&sec.metrics, "mttd_s") {
            Some(v) => format!("MTTD {}s", fmt_num(v)),
            None => String::new(),
        },
        _ => String::new(),
    }
}

pub fn render_markdown(
    card: &Scorecard,
    regressions: &[Regression],
    baseline_ts: Option<u64>,
) -> String {
    let mut out = String::new();
    out.push_str("# obleth deployment scorecard\n\n");

    let sys = match card.system_score {
        Some(s) => format!("{s} ({})", grade_from_score(s).letter()),
        None => "—".to_string(),
    };
    out.push_str(&format!(
        "**target:** {}   **gateway:** {}   **system score: {sys}**\n\n",
        card.target, card.gateway_version
    ));

    out.push_str("| section | grade | score | headline |\n");
    out.push_str("|---|---|---|---|\n");
    for sec in &card.sections {
        let score_str = sec
            .score
            .map(|s| s.to_string())
            .unwrap_or_else(|| "—".to_string());
        out.push_str(&format!(
            "| {} | {} | {score_str} | {} |\n",
            sec.id,
            sec.grade,
            headline(sec)
        ));
    }
    out.push('\n');

    if !regressions.is_empty() {
        let ts = baseline_ts
            .map(|t| t.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        out.push_str(&format!("## regressions vs baseline ({ts})\n\n"));
        for r in regressions {
            out.push_str(&format!("- {}\n", r.message));
        }
        out.push('\n');
    }

    let recs: Vec<&String> = card
        .sections
        .iter()
        .flat_map(|s| s.recommendations.iter())
        .collect();
    if !recs.is_empty() {
        out.push_str("## recommendations\n\n");
        for (i, r) in recs.iter().enumerate() {
            let n = i + 1;
            out.push_str(&format!("{n}. {r}\n"));
        }
    }

    out
}

/// Writes `scorecard.json` and `scorecard.md` into `BENCH_OUT_DIR`, plus a
/// dated copy under `scorecards/` that later runs use as a baseline.
pub fn write_scorecard(card: &Scorecard, markdown: &str) -> Result<(PathBuf, PathBuf)> {
    let dir = crate::report::out_dir();

    let json_path = dir.join("scorecard.json");
    File::create(&json_path)?.write_all(serde_json::to_string_pretty(card)?.as_bytes())?;

    let md_path = dir.join("scorecard.md");
    File::create(&md_path)?.write_all(markdown.as_bytes())?;

    let scorecards_dir = dir.join("scorecards");
    create_dir_all(&scorecards_dir)?;
    let baseline_path = scorecards_dir.join(format!("{}-{}.json", card.target, card.created_unix));
    File::create(&baseline_path)?.write_all(serde_json::to_string_pretty(card)?.as_bytes())?;

    Ok((json_path, md_path))
}

/// Finds the newest `scorecards/{target}-{ts}.json` with `ts < before_unix`
/// (strictly less, so a run never picks itself as its own baseline).
pub fn latest_baseline(target: &str, before_unix: u64) -> Option<Scorecard> {
    let dir = crate::report::out_dir().join("scorecards");
    let entries = std::fs::read_dir(&dir).ok()?;

    let prefix = format!("{target}-");
    let mut best: Option<(u64, PathBuf)> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let Some(rest) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Some(ts_str) = rest.strip_suffix(".json") else {
            continue;
        };
        if ts_str.is_empty() || !ts_str.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let Ok(ts) = ts_str.parse::<u64>() else {
            continue;
        };
        if ts >= before_unix {
            continue;
        }
        if best.as_ref().is_none_or(|(b, _)| ts > *b) {
            best = Some((ts, path));
        }
    }

    let (_, path) = best?;
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card_with_capacity(models: serde_json::Value) -> Scorecard {
        Scorecard {
            target: "demo".into(),
            created_unix: 100,
            gateway_version: "v0.7.2".into(),
            system_score: Some(90),
            sections: vec![SectionRecord {
                id: "capacity".into(),
                score: Some(100),
                grade: "A".into(),
                metrics: serde_json::json!({ "models": models }),
                recommendations: vec![],
                error: None,
            }],
        }
    }

    #[test]
    fn capacity_drop_flags_regression() {
        let base = card_with_capacity(serde_json::json!([
            { "model": "m1", "sustainable_conc": 256, "p99_ttfb_at_knee_ms": 40 }
        ]));
        let mut cur = card_with_capacity(serde_json::json!([
            { "model": "m1", "sustainable_conc": 128, "p99_ttfb_at_knee_ms": 40 }
        ]));
        cur.created_unix = 200;
        let regs = diff(&cur, &base);
        assert_eq!(regs.len(), 1);
        assert!(regs[0].message.contains("m1"));
        assert_eq!(regs[0].section, "capacity");
    }

    #[test]
    fn p99_rise_flags_regression_but_small_wiggle_does_not() {
        let base = card_with_capacity(serde_json::json!([
            { "model": "m1", "sustainable_conc": 256, "p99_ttfb_at_knee_ms": 40 }
        ]));
        let mut worse = base.clone();
        worse.sections[0].metrics = serde_json::json!({ "models": [
            { "model": "m1", "sustainable_conc": 256, "p99_ttfb_at_knee_ms": 60 }
        ]});
        assert_eq!(diff(&worse, &base).len(), 1);
        let mut wiggle = base.clone();
        wiggle.sections[0].metrics = serde_json::json!({ "models": [
            { "model": "m1", "sustainable_conc": 240, "p99_ttfb_at_knee_ms": 44 }
        ]});
        assert!(diff(&wiggle, &base).is_empty());
    }

    #[test]
    fn regression_caps_section_score() {
        let mut cur = card_with_capacity(serde_json::json!([]));
        let regs = vec![Regression {
            section: "capacity".into(),
            message: "x".into(),
        }];
        apply_regressions(&mut cur, &regs);
        assert_eq!(cur.sections[0].score, Some(60));
        assert_eq!(cur.sections[0].grade, "C");
    }

    #[test]
    fn markdown_contains_scores_and_recommendations() {
        let mut card = card_with_capacity(serde_json::json!([]));
        card.sections[0].recommendations = vec!["do the thing".into()];
        let md = render_markdown(&card, &[], None);
        assert!(md.contains("# obleth deployment scorecard"));
        assert!(md.contains("capacity"));
        assert!(md.contains("do the thing"));
        assert!(md.contains("90"));
    }

    #[test]
    fn scorecard_roundtrips_and_latest_baseline_found() {
        let _guard = crate::report::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var(
            "BENCH_OUT_DIR",
            std::env::temp_dir()
                .join("obench-score-test")
                .to_str()
                .unwrap(),
        );
        let _ = std::fs::remove_dir_all(crate::report::out_dir().join("scorecards"));
        let old = card_with_capacity(serde_json::json!([]));
        write_scorecard(&old, "# md").unwrap();
        let mut newer = old.clone();
        newer.created_unix = 150;
        write_scorecard(&newer, "# md").unwrap();
        let found = latest_baseline("demo", 200).unwrap();
        assert_eq!(found.created_unix, 150);
        // a run must not pick ITSELF as baseline
        assert_eq!(latest_baseline("demo", 150).unwrap().created_unix, 100);
        assert!(latest_baseline("live", 200).is_none());
    }
}
