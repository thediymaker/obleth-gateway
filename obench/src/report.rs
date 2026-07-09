use std::fs::{create_dir_all, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;

use crate::engine::stats::{Summary, Verdict};

/// Serializes tests that mutate the process-global BENCH_OUT_DIR env var.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn out_dir() -> PathBuf {
    let dir = std::env::var("BENCH_OUT_DIR").unwrap_or_else(|_| "/tmp/obleth-bench".to_string());
    let p = PathBuf::from(dir);
    let _ = create_dir_all(&p);
    p
}

pub fn write_meta(profile: &str, meta: &serde_json::Value) -> Result<PathBuf> {
    let path = out_dir().join(format!("{profile}-meta.json"));
    let mut f = File::create(&path)?;
    f.write_all(serde_json::to_string_pretty(meta)?.as_bytes())?;
    Ok(path)
}

/// Write a rendered markdown report to `{name}-report.md` in BENCH_OUT_DIR.
/// Reports are artifacts, so they never land in the source tree.
pub fn write_report(name: &str, markdown: &str) -> Result<PathBuf> {
    let path = out_dir().join(format!("{name}-report.md"));
    let mut f = File::create(&path)?;
    f.write_all(markdown.as_bytes())?;
    Ok(path)
}

pub fn append_timeline(profile: &str, row: &serde_json::Value) -> Result<()> {
    let path = out_dir().join(format!("{profile}-timeline.jsonl"));
    let mut f = OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", serde_json::to_string(row)?)?;
    Ok(())
}

pub fn render_summary(summary: &Summary, ui_base: &str) -> String {
    let verdict = match &summary.verdict {
        Verdict::Pass => "PASS — deployment stayed up and served the load".to_string(),
        Verdict::Fail(issues) => format!("FAIL — {}", issues.join("; ")),
    };
    let est = if summary.any_estimated {
        "  (some token counts estimated)"
    } else {
        ""
    };
    let per_stream = if summary.decode_samples > 0 {
        format!(
            " · per-stream p50 {:.1} p10 {:.1} tok/s",
            summary.p50_decode_tps, summary.p10_decode_tps
        )
    } else {
        String::new()
    };
    format!(
        "verdict: {verdict}\n\
         requests: {} ok / {} attempts  ({:.0} req/s)\n\
         errors: {} ({:.2}%)   429: {}\n\
         ttfb ms:  p50={} p90={} p99={}\n\
         total ms: p50={} p99={}\n\
         tokens: in {} out {}{est}\n\
         throughput: {:.0} tok/s{per_stream}\n\
         watch in the control plane:\n\
         \u{20}\u{20}fairshare   {ui_base}/fairshare\n\
         \u{20}\u{20}accounting  {ui_base}/usage",
        summary.completed,
        summary.attempts,
        summary.req_per_s,
        summary.errors,
        summary.error_rate * 100.0,
        summary.rejected,
        summary.p50_ttfb_ms,
        summary.p90_ttfb_ms,
        summary.p99_ttfb_ms,
        summary.p50_total_ms,
        summary.p99_total_ms,
        summary.in_tokens,
        summary.out_tokens,
        summary.agg_out_tok_per_s,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::stats::Stats;

    #[test]
    fn render_includes_verdict_and_pointers() {
        let mut s = Stats::default();
        s.record(&crate::engine::stats::RequestOutcome {
            status: 200,
            ttfb_ms: 10,
            total_ms: 20,
            in_tokens: 5,
            out_tokens: 7,
            usage_estimated: false,
            gaps_ms: Vec::new(),
        });
        let sum = s.summarize(1.0, 0.05);
        let out = render_summary(&sum, "http://localhost:3000");
        assert!(out.contains("PASS"));
        assert!(out.contains("http://localhost:3000/fairshare"));
        assert!(out.contains("http://localhost:3000/usage"));
    }

    #[test]
    fn write_and_append_roundtrip() {
        let _guard = crate::report::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var(
            "BENCH_OUT_DIR",
            std::env::temp_dir().join("obench-test").to_str().unwrap(),
        );
        let p = write_meta("unit", &serde_json::json!({ "ok": true })).unwrap();
        assert!(p.exists());
        append_timeline("unit", &serde_json::json!({ "t": 1 })).unwrap();
        let tl = out_dir().join("unit-timeline.jsonl");
        assert!(tl.exists());
    }

    #[test]
    fn write_report_creates_md_file() {
        let _guard = crate::report::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        std::env::set_var(
            "BENCH_OUT_DIR",
            std::env::temp_dir()
                .join("obench-report-test")
                .to_str()
                .unwrap(),
        );
        let p = write_report("compression", "# hello\n\nbody").unwrap();
        assert!(p.exists());
        assert!(p.to_string_lossy().ends_with("compression-report.md"));
        assert_eq!(std::fs::read_to_string(&p).unwrap(), "# hello\n\nbody");
    }

    #[test]
    fn render_includes_throughput_line() {
        let mut s = Stats::default();
        s.record(&crate::engine::stats::RequestOutcome {
            status: 200,
            ttfb_ms: 100,
            total_ms: 600,
            in_tokens: 5,
            out_tokens: 20,
            usage_estimated: false,
            gaps_ms: Vec::new(),
        });
        let sum = s.summarize(2.0, 0.05);
        let out = render_summary(&sum, "http://localhost:3000");
        assert!(out.contains("throughput: 10 tok/s"));
        assert!(out.contains("per-stream p50 40.0 p10 40.0 tok/s"));
    }

    #[test]
    fn render_omits_per_stream_without_decode_samples() {
        let mut s = Stats::default();
        // Embeddings-shaped outcome: no out tokens, no decode window.
        s.record(&crate::engine::stats::RequestOutcome {
            status: 200,
            ttfb_ms: 10,
            total_ms: 10,
            in_tokens: 5,
            out_tokens: 0,
            usage_estimated: false,
            gaps_ms: Vec::new(),
        });
        let sum = s.summarize(1.0, 0.05);
        let out = render_summary(&sum, "http://localhost:3000");
        assert!(out.contains("throughput: 0 tok/s"));
        assert!(!out.contains("per-stream"));
    }
}
