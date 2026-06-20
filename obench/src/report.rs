use std::fs::{create_dir_all, File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

use anyhow::Result;

use crate::engine::stats::{Summary, Verdict};

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
    let est = if summary.any_estimated { "  (some token counts estimated)" } else { "" };
    format!(
        "verdict: {verdict}\n\
         requests: {} ok / {} attempts  ({:.0} req/s)\n\
         errors: {} ({:.2}%)   429: {}\n\
         ttfb ms:  p50={} p90={} p99={}\n\
         total ms: p50={} p99={}\n\
         tokens: in {} out {}{est}\n\
         watch in the control plane:\n\
         \u{20}\u{20}fairshare   {ui_base}/fairshare\n\
         \u{20}\u{20}accounting  {ui_base}/usage",
        summary.completed, summary.attempts, summary.req_per_s,
        summary.errors, summary.error_rate * 100.0, summary.rejected,
        summary.p50_ttfb_ms, summary.p90_ttfb_ms, summary.p99_ttfb_ms,
        summary.p50_total_ms, summary.p99_total_ms,
        summary.in_tokens, summary.out_tokens,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::stats::Stats;

    #[test]
    fn render_includes_verdict_and_pointers() {
        let mut s = Stats::default();
        s.record(&crate::engine::stats::RequestOutcome { status: 200, ttfb_ms: 10, total_ms: 20, in_tokens: 5, out_tokens: 7, usage_estimated: false });
        let sum = s.summarize(1.0, 0.05);
        let out = render_summary(&sum, "http://localhost:3000");
        assert!(out.contains("PASS"));
        assert!(out.contains("http://localhost:3000/fairshare"));
        assert!(out.contains("http://localhost:3000/usage"));
    }

    #[test]
    fn write_and_append_roundtrip() {
        std::env::set_var("BENCH_OUT_DIR", std::env::temp_dir().join("obench-test").to_str().unwrap());
        let p = write_meta("unit", &serde_json::json!({ "ok": true })).unwrap();
        assert!(p.exists());
        append_timeline("unit", &serde_json::json!({ "t": 1 })).unwrap();
        let tl = out_dir().join("unit-timeline.jsonl");
        assert!(tl.exists());
    }
}
