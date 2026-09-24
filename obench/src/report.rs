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

/// Append one per-tenant observation to `{profile}-fairshare.csv`, writing the
/// header on first use. This is the plotting series for the convergence figure.
pub fn append_fairshare_row(
    profile: &str,
    t_s: f64,
    sample: &crate::engine::fairshare::TenantSample,
) -> Result<()> {
    const HEADER: &str = "t_s,tenant,group,weight,in_flight,queued,served_tokens,weight_share";
    let path = out_dir().join(format!("{profile}-fairshare.csv"));
    let need_header = std::fs::metadata(&path)
        .map(|m| m.len() == 0)
        .unwrap_or(true);
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    if need_header {
        writeln!(f, "{HEADER}")?;
    }
    writeln!(
        f,
        "{:.3},{},{},{},{},{},{:.1},{:.6}",
        t_s,
        csv_field(&sample.name),
        csv_field(&sample.group),
        sample.weight,
        sample.in_flight,
        sample.queued,
        sample.served_tokens,
        sample.weight_share,
    )?;
    Ok(())
}

/// Append one per-group observation to `{profile}-fairshare-groups.csv`.
pub fn append_fairshare_group_row(
    profile: &str,
    t_s: f64,
    sample: &crate::engine::fairshare::GroupSample,
) -> Result<()> {
    const HEADER: &str =
        "t_s,group,weight,in_flight,queued,slot_cap,borrowed,served_tokens,weight_share";
    let path = out_dir().join(format!("{profile}-fairshare-groups.csv"));
    let need_header = std::fs::metadata(&path)
        .map(|m| m.len() == 0)
        .unwrap_or(true);
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    if need_header {
        writeln!(f, "{HEADER}")?;
    }
    writeln!(
        f,
        "{:.3},{},{},{},{},{},{},{:.1},{:.6}",
        t_s,
        csv_field(&sample.name),
        sample.weight,
        sample.in_flight,
        sample.queued,
        sample.slot_cap,
        sample.borrowed,
        sample.served_tokens,
        sample.weight_share,
    )?;
    Ok(())
}

/// Append one per-model-pool observation to `{profile}-fairshare-pools.csv`.
pub fn append_pool_row(
    profile: &str,
    t_s: f64,
    pool: &crate::engine::fairshare::PoolSample,
) -> Result<()> {
    const HEADER: &str = "t_s,model,cap,in_flight,queued";
    let path = out_dir().join(format!("{profile}-fairshare-pools.csv"));
    let need_header = std::fs::metadata(&path)
        .map(|m| m.len() == 0)
        .unwrap_or(true);
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    if need_header {
        writeln!(f, "{HEADER}")?;
    }
    writeln!(
        f,
        "{:.3},{},{},{},{}",
        t_s,
        csv_field(&pool.model),
        pool.cap,
        pool.in_flight,
        pool.queued,
    )?;
    Ok(())
}

/// Append one per-key observation to `{profile}-fairshare-keys.csv`.
pub fn append_key_row(
    profile: &str,
    t_s: f64,
    model: &str,
    sample: &crate::engine::fairshare::KeySample,
) -> Result<()> {
    const HEADER: &str = "t_s,model,tenant,key,weight,max_in_flight,in_flight,queued,served_tokens";
    let path = out_dir().join(format!("{profile}-fairshare-keys.csv"));
    let need_header = std::fs::metadata(&path)
        .map(|m| m.len() == 0)
        .unwrap_or(true);
    let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
    if need_header {
        writeln!(f, "{HEADER}")?;
    }
    writeln!(
        f,
        "{:.3},{},{},{},{},{},{},{},{:.1}",
        t_s,
        csv_field(model),
        csv_field(&sample.tenant),
        csv_field(&sample.name),
        sample.weight,
        sample
            .max_in_flight
            .map(|c| c.to_string())
            .unwrap_or_default(),
        sample.in_flight,
        sample.queued,
        sample.served_tokens,
    )?;
    Ok(())
}

/// Write the whole-run per-tenant results to `{profile}-fairshare-summary.csv`.
pub fn write_fairshare_summary_csv(
    profile: &str,
    summary: &crate::engine::fairshare::FairshareSummary,
) -> Result<PathBuf> {
    let path = out_dir().join(format!("{profile}-fairshare-summary.csv"));
    let mut f = File::create(&path)?;
    writeln!(
        f,
        "tenant,group,weight,slot_seconds,active_seconds,realized_share,\
         expected_share_raw,expected_share,share_ratio,\
         served_tokens_delta,token_share,peak_queued,backlogged_ticks,starved"
    )?;
    for t in summary.competed() {
        writeln!(
            f,
            "{},{},{},{:.3},{:.3},{:.6},{:.6},{:.6},{:.6},{:.1},{:.6},{},{},{}",
            csv_field(&t.name),
            csv_field(&t.group),
            t.weight,
            t.slot_seconds,
            t.active_seconds,
            t.realized_share,
            t.expected_share,
            t.normalized_expected_share,
            t.share_ratio,
            t.served_tokens_delta,
            t.token_share,
            t.peak_queued,
            t.backlogged_ticks,
            t.starved,
        )?;
    }
    Ok(path)
}

/// Quote a CSV field only when it would otherwise break the row.
fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

/// Truncate to `w` display characters so table columns cannot drift.
fn fit(s: &str, w: usize) -> String {
    if s.chars().count() <= w {
        return s.to_string();
    }
    let mut out: String = s.chars().take(w.saturating_sub(1)).collect();
    out.push('…');
    out
}

/// Render the fairshare convergence block for the terminal report.
pub fn render_fairshare(summary: &crate::engine::fairshare::FairshareSummary) -> String {
    let competed = summary.competed();
    if summary.samples == 0 || competed.is_empty() {
        return "fairshare: no fairshare samples collected".to_string();
    }
    let algorithm = if summary.algorithm.is_empty() {
        "unknown"
    } else {
        &summary.algorithm
    };
    const NAME_W: usize = 20;
    const GROUP_W: usize = 18;
    let mut out = format!(
        "fairshare convergence — {algorithm}, max_in_flight {}, {} samples over {:.1}s\n\
         \u{20}\u{20}{:<NAME_W$}{:<GROUP_W$}{:>7}{:>11}{:>11}{:>8}{:>8}\n",
        summary.max_in_flight,
        summary.samples,
        summary.elapsed_s,
        "tenant",
        "group",
        "weight",
        "expected",
        "realized",
        "ratio",
        "peak q",
    );

    // Largest realized share first: the contended tenants are what a reader
    // checks, and it puts any starved tenant at the bottom where it stands out.
    let mut rows = competed;
    rows.sort_by(|a, b| {
        b.realized_share
            .partial_cmp(&a.realized_share)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.name.cmp(&b.name))
    });
    for t in &rows {
        out.push_str(&format!(
            "\u{20}\u{20}{:<NAME_W$}{:<GROUP_W$}{:>7}{:>10.1}%{:>10.1}%{:>8.2}{:>8}\n",
            fit(&t.name, NAME_W),
            fit(&t.group, GROUP_W),
            t.weight,
            t.normalized_expected_share * 100.0,
            t.realized_share * 100.0,
            t.share_ratio,
            t.peak_queued,
        ));
    }

    let starvation = if summary.starved.is_empty() {
        "none".to_string()
    } else {
        summary.starved.join(", ")
    };
    out.push_str(&format!(
        "\u{20}\u{20}Jain's index (weight-normalized): {:.3} · starvation: {starvation}\n\
         \u{20}\u{20}capacity utilization: {:.1}%{}",
        summary.jain_index,
        summary.utilization * 100.0,
        // Shares only describe the scheduler when the pool is full. Below that,
        // they describe offered load, and saying so beats a silent footnote.
        if summary.utilization < 0.95 {
            "  — under-saturated: shares reflect offered load, not admission"
        } else {
            ""
        }
    ));
    out
}

/// Render the per-model pool block: one row per admission pool, so a single
/// unfair or under-served model stands out against the fleet.
pub fn render_pools(pools: &[crate::engine::fairshare::PoolSummary]) -> String {
    if pools.is_empty() {
        return "pools: no per-model samples collected".to_string();
    }
    let mut out = format!(
        "per-model pools\n  {:<24}{:>5}{:>12}{:>10}{:>12}{:>8}{:>8}\n",
        "model", "cap", "tenant jain", "key jain", "idle+queue", "capviol", "starved"
    );
    let mut rows: Vec<_> = pools.iter().collect();
    rows.sort_by(|a, b| a.model.cmp(&b.model));
    for p in rows {
        out.push_str(&format!(
            "  {:<24}{:>5}{:>12.3}{:>10.3}{:>12}{:>8}{:>8}\n",
            fit(&p.model, 24),
            p.cap,
            p.tenants.jain_index,
            p.key_jain_index,
            p.idle_with_backlog_ticks,
            p.cap_violations,
            p.tenants.starved.len()
        ));
    }
    out
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
         \u{20}\u{20}accounting  {ui_base}/reports",
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
        assert!(out.contains("http://localhost:3000/reports"));
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

    // ── fairshare convergence reporting ───────────────────────────────────────

    use crate::engine::fairshare::{FairshareAccumulator, TenantSample};

    fn sample(name: &str, group: &str, in_flight: u64, ws: f64) -> TenantSample {
        TenantSample {
            name: name.into(),
            group: group.into(),
            weight: 100,
            max_in_flight: None,
            in_flight,
            queued: 2,
            served_tokens: 1000.0,
            weight_share: ws,
        }
    }

    fn two_tenant_summary() -> crate::engine::fairshare::FairshareSummary {
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 8);
        for _ in 0..10 {
            acc.observe(
                &[
                    sample("chatbot", "prod", 7, 0.909),
                    sample("api-batch", "dev", 1, 0.0909),
                ],
                1.0,
            );
        }
        acc.summarize()
    }

    #[test]
    fn fairshare_csv_writes_a_header_once_then_one_row_per_call() {
        let _guard = crate::report::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("obench-fs-csv");
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BENCH_OUT_DIR", dir.to_str().unwrap());

        append_fairshare_row("unit", 0.0, &sample("chatbot", "prod", 7, 0.909)).unwrap();
        append_fairshare_row("unit", 1.0, &sample("chatbot", "prod", 6, 0.909)).unwrap();

        let body = std::fs::read_to_string(out_dir().join("unit-fairshare.csv")).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 rows, got {body:?}");
        assert_eq!(
            lines[0],
            "t_s,tenant,group,weight,in_flight,queued,served_tokens,weight_share"
        );
        assert!(lines[1].starts_with("0.000,chatbot,prod,100,7,2,"));
        assert!(lines[2].starts_with("1.000,chatbot,prod,100,6,2,"));
    }

    #[test]
    fn fairshare_group_csv_records_the_slot_cap_series() {
        let _guard = crate::report::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("obench-fs-groups");
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BENCH_OUT_DIR", dir.to_str().unwrap());

        let g = crate::engine::fairshare::GroupSample {
            name: "prod".into(),
            weight: 500,
            in_flight: 7,
            queued: 4,
            slot_cap: 7,
            borrowed: 0,
            served_tokens: 91000.0,
            weight_share: 0.909,
        };
        append_fairshare_group_row("unit", 0.0, &g).unwrap();
        append_fairshare_group_row("unit", 1.0, &g).unwrap();

        let body = std::fs::read_to_string(out_dir().join("unit-fairshare-groups.csv")).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 rows, got {body:?}");
        assert_eq!(
            lines[0],
            "t_s,group,weight,in_flight,queued,slot_cap,borrowed,served_tokens,weight_share"
        );
        assert!(lines[1].starts_with("0.000,prod,500,7,4,7,0,"));
    }

    #[test]
    fn fairshare_summary_csv_has_a_row_per_tenant() {
        let _guard = crate::report::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("obench-fs-sum");
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BENCH_OUT_DIR", dir.to_str().unwrap());

        let p = write_fairshare_summary_csv("unit", &two_tenant_summary()).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        let lines: Vec<&str> = body.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 tenants");
        assert!(lines[0].starts_with("tenant,group,weight,slot_seconds,"));
        assert!(body.contains("api-batch"));
        assert!(body.contains("chatbot"));
    }

    #[test]
    fn render_fairshare_shows_expected_versus_realized_and_jain() {
        let out = render_fairshare(&two_tenant_summary());
        assert!(out.contains("hierarchical"), "{out}");
        assert!(out.contains("chatbot"), "{out}");
        // realized 7/8 against an entitlement of 90.9%
        assert!(out.contains("87.5%"), "{out}");
        assert!(out.contains("90.9%"), "{out}");
        assert!(out.to_lowercase().contains("jain"), "{out}");
    }

    #[test]
    fn render_fairshare_omits_tenants_that_never_competed() {
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 64);
        for _ in 0..5 {
            acc.observe(
                &[
                    sample("worker", "prod", 4, 0.5),
                    TenantSample {
                        in_flight: 0,
                        queued: 0,
                        weight_share: 0.0,
                        ..sample("__control_plane__", "default", 0, 0.0)
                    },
                ],
                1.0,
            );
        }
        let out = render_fairshare(&acc.summarize());
        assert!(out.contains("worker"), "{out}");
        assert!(!out.contains("__control_plane__"), "{out}");
    }

    #[test]
    fn render_fairshare_rows_stay_aligned_with_over_long_names() {
        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 64);
        acc.observe(
            &[
                sample("a", "g", 4, 0.5),
                sample(
                    "a-very-long-tenant-name-that-overflows",
                    "a-very-long-group-name",
                    4,
                    0.5,
                ),
            ],
            1.0,
        );
        let out = render_fairshare(&acc.summarize());
        let rows: Vec<&str> = out
            .lines()
            .filter(|l| l.starts_with("  ") && !l.contains("Jain") && !l.contains("utilization"))
            .collect();
        assert_eq!(rows.len(), 3, "header + 2 tenants: {out}");
        let widths: Vec<usize> = rows.iter().map(|r| r.chars().count()).collect();
        assert!(
            widths.iter().all(|w| *w == widths[0]),
            "columns misaligned: {widths:?}\n{out}"
        );
    }

    #[test]
    fn fairshare_summary_csv_only_includes_competing_tenants() {
        let _guard = crate::report::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("obench-fs-filter");
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BENCH_OUT_DIR", dir.to_str().unwrap());

        let mut acc = FairshareAccumulator::new();
        acc.note_config("hierarchical", 64);
        acc.observe(
            &[
                sample("worker", "prod", 4, 0.5),
                TenantSample {
                    in_flight: 0,
                    queued: 0,
                    weight_share: 0.0,
                    ..sample("__control_plane__", "default", 0, 0.0)
                },
            ],
            1.0,
        );
        let p = write_fairshare_summary_csv("unit", &acc.summarize()).unwrap();
        let body = std::fs::read_to_string(&p).unwrap();
        assert_eq!(body.lines().count(), 2, "header + 1 competing tenant");
        assert!(!body.contains("__control_plane__"));
    }

    // ── per-pool and per-key reporting ────────────────────────────────────────

    #[test]
    fn pool_and_key_csvs_write_their_header_once() {
        let _guard = crate::report::TEST_ENV_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join("obench-fs-pools");
        let _ = std::fs::remove_dir_all(&dir);
        std::env::set_var("BENCH_OUT_DIR", dir.to_str().unwrap());

        let key = crate::engine::fairshare::KeySample {
            tenant: "obench-course-a".into(),
            name: "user-e".into(),
            weight: 100,
            max_in_flight: Some(1),
            in_flight: 1,
            queued: 3,
            served_tokens: 4200.0,
        };
        let pool = crate::engine::fairshare::PoolSample {
            model: "obench-base".into(),
            cap: 8,
            in_flight: 8,
            queued: 11,
            tenants: vec![sample("obench-course-a", "obench-chatbot", 4, 0.5)],
            keys: vec![key.clone()],
        };
        append_pool_row("unit", 0.0, &pool).unwrap();
        append_pool_row("unit", 1.0, &pool).unwrap();
        append_key_row("unit", 0.0, &pool.model, &key).unwrap();
        append_key_row("unit", 1.0, &pool.model, &key).unwrap();

        let pools = std::fs::read_to_string(out_dir().join("unit-fairshare-pools.csv")).unwrap();
        let lines: Vec<&str> = pools.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 rows, got {pools:?}");
        assert_eq!(lines[0], "t_s,model,cap,in_flight,queued");
        assert_eq!(lines[1], "0.000,obench-base,8,8,11");

        let keys = std::fs::read_to_string(out_dir().join("unit-fairshare-keys.csv")).unwrap();
        let lines: Vec<&str> = keys.lines().collect();
        assert_eq!(lines.len(), 3, "header + 2 rows, got {keys:?}");
        assert_eq!(
            lines[0],
            "t_s,model,tenant,key,weight,max_in_flight,in_flight,queued,served_tokens"
        );
        assert!(
            lines[1].starts_with("0.000,obench-base,obench-course-a,user-e,100,1,1,3,"),
            "{keys}"
        );
    }

    #[test]
    fn render_pools_lists_every_pool_and_flags_an_empty_run() {
        let mut a = crate::engine::fairshare::PoolAccumulator::new("obench-base", 8);
        a.observe(&[sample("t", "g", 8, 1.0)], &[], 1.0);
        let mut b = crate::engine::fairshare::PoolAccumulator::new("obench-turbo", 4);
        b.observe(&[sample("t", "g", 4, 1.0)], &[], 1.0);
        let out = render_pools(&[b.summarize(), a.summarize()]);
        let rows: Vec<&str> = out.lines().skip(2).collect();
        assert!(rows[0].contains("obench-base"), "sorted by model: {out}");
        assert!(rows[1].contains("obench-turbo"), "{out}");
        assert!(render_pools(&[]).contains("no per-model samples"));
    }

    #[test]
    fn render_fairshare_reports_no_starvation_when_all_tenants_progress() {
        let out = render_fairshare(&two_tenant_summary());
        assert!(out.contains("starvation: none"), "{out}");
    }

    #[test]
    fn render_fairshare_names_starved_tenants() {
        let mut acc = FairshareAccumulator::new();
        acc.note_config("weighted", 8);
        for _ in 0..10 {
            acc.observe(
                &[
                    TenantSample {
                        served_tokens: 0.0,
                        ..sample("victim", "dev", 0, 0.5)
                    },
                    sample("hog", "prod", 8, 0.5),
                ],
                1.0,
            );
        }
        let out = render_fairshare(&acc.summarize());
        assert!(out.contains("starvation: victim"), "{out}");
    }

    #[test]
    fn render_fairshare_handles_a_run_with_no_samples() {
        let out = render_fairshare(&FairshareAccumulator::new().summarize());
        assert!(out.contains("no fairshare samples"), "{out}");
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
