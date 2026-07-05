//! Deployment scorecard: orchestrates the check sections, rolls their scores
//! into one graded system score, and tracks regressions across runs.

pub mod capacity;
pub mod fairshare;
pub mod overhead;
pub mod overload;
pub mod report;
pub mod resilience;
pub mod streaming;

use serde::Serialize;

#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SectionId {
    Overhead,
    Capacity,
    Overload,
    Streaming,
    Resilience,
    Fairshare,
    Compression,
}

impl SectionId {
    pub fn name(self) -> &'static str {
        match self {
            SectionId::Overhead => "overhead",
            SectionId::Capacity => "capacity",
            SectionId::Overload => "overload",
            SectionId::Streaming => "streaming",
            SectionId::Resilience => "resilience",
            SectionId::Fairshare => "fairshare",
            SectionId::Compression => "compression",
        }
    }

    pub fn from_name(s: &str) -> Option<SectionId> {
        match s {
            "overhead" => Some(SectionId::Overhead),
            "capacity" => Some(SectionId::Capacity),
            "overload" => Some(SectionId::Overload),
            "streaming" => Some(SectionId::Streaming),
            "resilience" => Some(SectionId::Resilience),
            "fairshare" => Some(SectionId::Fairshare),
            "compression" => Some(SectionId::Compression),
            _ => None,
        }
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Grade {
    A,
    B,
    C,
    D,
    F,
    Skipped,
    Errored,
}

impl Grade {
    pub fn letter(self) -> &'static str {
        match self {
            Grade::A => "A",
            Grade::B => "B",
            Grade::C => "C",
            Grade::D => "D",
            Grade::F => "F",
            Grade::Skipped => "—",
            Grade::Errored => "ERR",
        }
    }
}

pub fn grade_from_score(score: u8) -> Grade {
    match score {
        90..=u8::MAX => Grade::A,
        75..=89 => Grade::B,
        60..=74 => Grade::C,
        45..=59 => Grade::D,
        _ => Grade::F,
    }
}

#[derive(Debug, Serialize)]
pub struct SectionResult {
    pub id: SectionId,
    pub score: Option<u8>,
    pub metrics: serde_json::Value,
    pub recommendations: Vec<String>,
    pub error: Option<String>,
}

impl SectionResult {
    pub fn grade(&self) -> Grade {
        if self.error.is_some() {
            return Grade::Errored;
        }
        match self.score {
            Some(s) => grade_from_score(s),
            None => Grade::Skipped,
        }
    }

    pub fn skipped(id: SectionId, why: &str) -> SectionResult {
        SectionResult {
            id,
            score: None,
            metrics: serde_json::json!({ "skipped": why }),
            recommendations: vec![],
            error: None,
        }
    }

    pub fn errored(id: SectionId, err: &str) -> SectionResult {
        SectionResult {
            id,
            score: None,
            metrics: serde_json::json!({}),
            recommendations: vec![],
            error: Some(err.to_string()),
        }
    }
}

/// Section weights per target. Sections missing from a target's list are not
/// run there at all; sections that run but end Skipped/Errored redistribute
/// their weight via `system_score`.
pub fn weights(target: crate::cli::Target) -> Vec<(SectionId, u32)> {
    use crate::cli::Target::*;
    match target {
        Demo => vec![
            (SectionId::Overhead, 15),
            (SectionId::Capacity, 20),
            (SectionId::Overload, 15),
            (SectionId::Streaming, 10),
            (SectionId::Resilience, 20),
            (SectionId::Fairshare, 15),
            (SectionId::Compression, 5),
        ],
        Live => vec![
            (SectionId::Capacity, 45),
            (SectionId::Overload, 25),
            (SectionId::Streaming, 20),
            (SectionId::Compression, 10),
        ],
    }
}

/// Weighted mean over sections that produced a score; weights of unscored
/// sections are redistributed by normalizing over the scored subset.
pub fn system_score(results: &[SectionResult], weights: &[(SectionId, u32)]) -> Option<u8> {
    let mut num = 0f64;
    let mut den = 0f64;
    for (id, w) in weights {
        if let Some(r) = results.iter().find(|r| r.id == *id) {
            if let Some(s) = r.score {
                num += s as f64 * *w as f64;
                den += *w as f64;
            }
        }
    }
    if den == 0.0 {
        None
    } else {
        Some((num / den).round() as u8)
    }
}

/// Mirrors `system_score`, but over the persisted `SectionRecord` shape (`id`
/// is a plain string) rather than the live `SectionResult`. Used to recompute
/// the system score after `report::apply_regressions` has capped section
/// scores in place — that function only knows about records, not results.
pub fn system_score_from_records(
    records: &[report::SectionRecord],
    weights: &[(SectionId, u32)],
) -> Option<u8> {
    let mut num = 0f64;
    let mut den = 0f64;
    for (id, w) in weights {
        if let Some(r) = records
            .iter()
            .find(|r| SectionId::from_name(&r.id) == Some(*id))
        {
            if let Some(s) = r.score {
                num += s as f64 * *w as f64;
                den += *w as f64;
            }
        }
    }
    if den == 0.0 {
        None
    } else {
        Some((num / den).round() as u8)
    }
}

/// `only` takes precedence over `skip`: if non-empty, `id` must be in it;
/// otherwise `id` runs unless it's in `skip`. Both lists match by
/// `SectionId::name()`.
pub fn section_enabled(id: SectionId, skip: &[String], only: &[String]) -> bool {
    if !only.is_empty() {
        only.iter().any(|s| SectionId::from_name(s) == Some(id))
    } else {
        !skip.iter().any(|s| SectionId::from_name(s) == Some(id))
    }
}

const ALL_SECTIONS: &[SectionId] = &[
    SectionId::Overhead,
    SectionId::Capacity,
    SectionId::Overload,
    SectionId::Streaming,
    SectionId::Resilience,
    SectionId::Fairshare,
    SectionId::Compression,
];

/// Validate that every name in `names` (as given to `--skip`/`--only`) names a
/// real section. Returns `Err` listing the bad names alongside the full set
/// of valid ones, so a typo doesn't silently run (or skip) nothing.
pub fn validate_section_names(names: &[String]) -> Result<(), String> {
    let bad: Vec<&str> = names
        .iter()
        .filter(|n| SectionId::from_name(n).is_none())
        .map(|s| s.as_str())
        .collect();
    if bad.is_empty() {
        return Ok(());
    }
    let valid: Vec<&str> = ALL_SECTIONS.iter().map(|s| s.name()).collect();
    Err(format!(
        "unknown section name(s): {} — valid sections are: {}",
        bad.join(", "),
        valid.join(", ")
    ))
}

/// Concurrency to drive the streaming check at: half the capacity ramp's
/// sustainable concurrency (clamped to a sane 1..64 band), or a fixed default
/// of 8 when there's no capacity card to derive it from (capacity was
/// skipped/quick, or the model never sustained even the first ramp step).
/// `quick` doesn't change the formula — quick mode always hits the "no card"
/// path anyway — it's threaded through so call sites don't need a separate
/// branch.
pub fn streaming_conc(sustainable: Option<u32>, _quick: bool) -> u32 {
    sustainable.map(|s| (s / 2).clamp(1, 64)).unwrap_or(8)
}

/// Run the deployment scorecard: seed the target, run every applicable
/// section, roll the results into a system score, diff against the last
/// baseline, persist, and return the process exit code (0 or 1).
pub async fn run(
    cli: &crate::cli::Cli,
    args: &crate::cli::ScoreArgs,
    live_override: Option<&crate::config::LiveConfig>,
) -> anyhow::Result<i32> {
    use crate::cli::{Scope, Target};

    let Some(target) = cli.target else {
        anyhow::bail!("obench score needs --target demo or --target live");
    };

    // Validate --skip/--only before anything else runs (seeding included) so
    // a typo'd section name fails fast instead of silently running/skipping
    // nothing.
    let mut section_names = args.skip.clone();
    section_names.extend(args.only.iter().cloned());
    if let Err(e) = validate_section_names(&section_names) {
        anyhow::bail!(e);
    }

    let scope = crate::cli::scope_from(cli.model.clone(), cli.all);
    let admin = crate::admin::AdminClient::new(cli.admin_base.clone(), cli.admin_token.clone());

    let (seeded, proxy_base) = match target {
        Target::Demo => {
            crate::target::validate_target_locality(target, &cli.admin_base, &cli.proxy_base)
                .map_err(|e| anyhow::anyhow!(e))?;
            let api_base = std::env::var("BENCHMARK_API_BASE")
                .unwrap_or_else(|_| "http://benchmark-backend:8081".to_string());
            let seeded = crate::seed::seed_fixture(&admin, &api_base, &scope).await?;
            (seeded, cli.proxy_base.clone())
        }
        Target::Live => {
            let cfg = crate::profiles::resolve_live_config(cli, live_override)?;
            crate::config::validate_live(&cfg, &scope).map_err(|e| anyhow::anyhow!(e))?;
            let seeded = crate::seed::live_run_from_config(&cfg, &scope)?;
            let proxy_base = cfg.proxy_url.clone();
            (seeded, proxy_base)
        }
    };

    // Chat ramps only — the embedding endpoint doesn't fit the capacity/
    // streaming/overhead/resilience request shapes.
    let models: Vec<String> = seeded
        .models
        .iter()
        .filter(|m| !m.to_lowercase().contains("embed"))
        .cloned()
        .collect();
    if models.is_empty() {
        // Nothing to score: tear down whatever seeding just created (demo
        // creates real gateway objects; live is a no-op teardown) before
        // bailing, so a degenerate scope never leaks seeded state.
        admin.teardown(&seeded.teardown).await;
        anyhow::bail!(
            "only embedding models in scope — score needs at least one chat model; run without --model or pick a chat model"
        );
    }
    // Capacity/streaming/overhead/resilience drive a single tenant; fairshare
    // (below) uses the whole seeded fleet.
    let key = seeded
        .tenants
        .first()
        .map(|t| t.key.clone())
        .unwrap_or_default();

    let gateway_version = match target {
        Target::Demo => admin
            .get_version()
            .await
            .unwrap_or_else(|_| "unknown".to_string()),
        Target::Live => "unknown".to_string(),
    };

    // Ctrl-C: checked between sections, drains via the same teardown path a
    // normal run takes.
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let stop = stop.clone();
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            stop.store(true, std::sync::atomic::Ordering::Relaxed);
        });
    }

    let ctl = crate::backend_ctl::BackendControl::new(args.backend_base.clone());

    let w = weights(target);

    // Resilience must be the FIRST section to touch a model, not merely
    // scheduled early. The gateway's active/passive health prober consults a
    // passive tier first, with a look-back window of `interval.max(300s)`;
    // ANY success in that window reports the model healthy regardless of
    // concurrent error volume. If overhead/capacity/streaming/fairshare (or a
    // previous resilience run against the same demo fleet) has already driven
    // traffic to the model, that traffic's successes sit inside the window
    // for the entire resilience detect budget (150s < 300s) and the fault
    // injected by resilience can never be observed — a deterministic F,
    // not flakiness. Running resilience before any other section executes is
    // the only way its MTTD is measurable. This reorders EXECUTION only —
    // `weights()` (and therefore the scoring order and the printed/rendered
    // section table, restored via the sort below) is untouched.
    let mut exec_order = w.clone();
    if let Some(pos) = exec_order
        .iter()
        .position(|(id, _)| *id == SectionId::Resilience)
    {
        let resilience = exec_order.remove(pos);
        exec_order.insert(0, resilience);
    }

    let mut results: Vec<SectionResult> = Vec::new();
    let mut cards: Vec<capacity::CapacityCard> = Vec::new();

    for &(id, _weight) in &exec_order {
        if stop.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        if !section_enabled(id, &args.skip, &args.only) {
            results.push(SectionResult::skipped(id, "skipped by flag"));
            continue;
        }

        println!("== {} ==", id.name());

        match id {
            SectionId::Overhead => {
                if !ctl.healthy().await {
                    let msg = format!(
                        "benchmark-backend not reachable at {} — pass --backend-base",
                        args.backend_base
                    );
                    let mut r = SectionResult::skipped(id, &msg);
                    r.recommendations.push(msg);
                    results.push(r);
                } else {
                    // First in-scope chat model — `models` is guaranteed
                    // non-empty here (the degenerate-scope guard above bails
                    // before any section runs), and always the same model a
                    // `--model <name>` scope resolved to.
                    let model = models.first().cloned().unwrap_or_default();
                    match overhead::run_overhead(
                        &args.backend_base,
                        &proxy_base,
                        &key,
                        &model,
                        cli.input_tokens,
                        stop.clone(),
                    )
                    .await
                    {
                        Ok(points) => results.push(overhead::overhead_section(&points)),
                        Err(e) => results.push(SectionResult::errored(id, &e.to_string())),
                    }
                }
            }
            SectionId::Capacity => {
                if args.quick {
                    results.push(SectionResult::skipped(id, "quick mode"));
                } else {
                    let mut failed = None;
                    for model in &models {
                        if stop.load(std::sync::atomic::Ordering::Relaxed) {
                            break;
                        }
                        match capacity::run_ramp(
                            model,
                            &key,
                            &proxy_base,
                            cli.input_tokens,
                            args.max_conc,
                            stop.clone(),
                        )
                        .await
                        {
                            Ok(card) => cards.push(card),
                            Err(e) => {
                                failed = Some(e.to_string());
                                break;
                            }
                        }
                    }
                    match failed {
                        Some(e) => results.push(SectionResult::errored(id, &e)),
                        None => results.push(capacity::capacity_section(&cards)),
                    }
                }
            }
            SectionId::Overload => {
                if args.quick {
                    results.push(SectionResult::skipped(id, "quick mode: no ramp data"));
                } else {
                    results.push(overload::overload_section(&cards));
                }
            }
            SectionId::Streaming => {
                let mut quals = Vec::new();
                let mut failed = None;
                for model in &models {
                    if stop.load(std::sync::atomic::Ordering::Relaxed) {
                        break;
                    }
                    let sustainable = cards
                        .iter()
                        .find(|c| &c.model == model)
                        .map(|c| c.sustainable_conc)
                        .filter(|c| *c > 0);
                    let conc = streaming_conc(sustainable, args.quick);
                    match streaming::run_streaming(
                        model,
                        &key,
                        &proxy_base,
                        cli.input_tokens,
                        conc,
                        stop.clone(),
                    )
                    .await
                    {
                        Ok(summary) => {
                            if let Some(q) = streaming::quality_from_summary(model, conc, &summary)
                            {
                                quals.push(q);
                            }
                        }
                        Err(e) => {
                            failed = Some(e.to_string());
                            break;
                        }
                    }
                }
                match failed {
                    Some(e) => results.push(SectionResult::errored(id, &e)),
                    None => {
                        results.push(streaming::streaming_section(&quals, target == Target::Demo))
                    }
                }
            }
            SectionId::Resilience => {
                let model = if models.iter().any(|m| m == "obench-turbo") {
                    "obench-turbo".to_string()
                } else {
                    models.first().cloned().unwrap_or_default()
                };
                println!("resilience: injecting fault on {model}");
                match resilience::run_resilience(
                    &admin,
                    &ctl,
                    &proxy_base,
                    &key,
                    &model,
                    stop.clone(),
                )
                .await
                {
                    Ok(outcome) => results.push(resilience::resilience_section(&outcome)),
                    Err(e) => results.push(SectionResult::errored(id, &e.to_string())),
                }
            }
            SectionId::Fairshare => {
                if scope != Scope::All {
                    results.push(SectionResult::skipped(id, "needs --all"));
                } else {
                    match fairshare::run_fairshare(&admin, &proxy_base, &seeded, stop.clone()).await
                    {
                        Ok(r) => results.push(r),
                        Err(e) => results.push(SectionResult::errored(id, &e.to_string())),
                    }
                }
            }
            SectionId::Compression => match (&args.compression_model, &args.compression_key) {
                (Some(model), Some(comp_key)) => {
                    let cfg = crate::benchmarks::compression::CompressionConfig {
                        proxy_base: proxy_base.clone(),
                        admin_base: cli.admin_base.clone(),
                        admin_token: cli.admin_token.clone(),
                        api_key: comp_key.clone(),
                        model: model.clone(),
                        price_in_per_mtok:
                            crate::benchmarks::compression::DEFAULT_PRICE_IN_PER_MTOK,
                        prefill_tps: crate::benchmarks::compression::default_prefill_tps(),
                        max_tokens: crate::benchmarks::compression::DEFAULT_MAX_TOKENS,
                        reps: crate::benchmarks::compression::DEFAULT_REPS,
                        min_tokens: crate::benchmarks::compression::DEFAULT_MIN_TOKENS,
                    };
                    match crate::benchmarks::compression::run(&cfg).await {
                        Ok(0) => results.push(SectionResult {
                            id,
                            score: Some(100),
                            metrics: serde_json::json!({ "ran": true }),
                            recommendations: vec![],
                            error: None,
                        }),
                        Ok(code) => results.push(SectionResult::errored(
                            id,
                            &format!("compression benchmark exited {code}"),
                        )),
                        Err(e) => results.push(SectionResult::errored(id, &e.to_string())),
                    }
                }
                _ => {
                    let mut r = SectionResult::skipped(
                        id,
                        "pass --compression-model/--compression-key to include the compression A/B",
                    );
                    r.recommendations.push(
                        "compression not measured — grant the boon to a model and pass \
                         --compression-model to include token/cost savings in the score"
                            .to_string(),
                    );
                    results.push(r);
                }
            },
        }
    }

    // Teardown runs on every path (normal completion or Ctrl-C drain) so no
    // seeded credentials linger; no-op for a live run (empty Teardown).
    admin.teardown(&seeded.teardown).await;

    if stop.load(std::sync::atomic::Ordering::Relaxed) {
        if let Err(e) = ctl.set_fault("*", "ok").await {
            eprintln!(
                "warning: Ctrl-C cleanup failed to clear injected faults on benchmark-backend: {e} — a fault from a resilience run may still be active; clear it manually via POST {}/control {{\"model\":\"*\",\"mode\":\"ok\"}}",
                args.backend_base
            );
        }
        std::process::exit(130);
    }

    // Restore canonical (weights()) order for scoring/display now that
    // execution has finished — resilience ran first above, but the printed
    // table and scorecard.json should still read overhead/capacity/.../
    // resilience/... in the order operators expect.
    results.sort_by_key(|r| {
        w.iter()
            .position(|(id, _)| *id == r.id)
            .unwrap_or(usize::MAX)
    });

    let sys = system_score(&results, &w);
    let created_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let target_name = format!("{target:?}").to_lowercase();
    let mut card = report::Scorecard {
        target: target_name.clone(),
        created_unix,
        gateway_version,
        system_score: sys,
        sections: report::record_sections(&results),
    };

    let baseline = match &args.baseline {
        Some(path) => {
            let raw = std::fs::read_to_string(path)
                .map_err(|e| anyhow::anyhow!("reading baseline {path}: {e}"))?;
            Some(
                serde_json::from_str::<report::Scorecard>(&raw)
                    .map_err(|e| anyhow::anyhow!("parsing baseline {path}: {e}"))?,
            )
        }
        None => report::latest_baseline(&target_name, created_unix),
    };

    let (regs, baseline_ts) = match &baseline {
        Some(b) => (report::diff(&card, b), Some(b.created_unix)),
        None => (Vec::new(), None),
    };

    if !regs.is_empty() {
        report::apply_regressions(&mut card, &regs);
        card.system_score = system_score_from_records(&card.sections, &w);
    }

    let md = report::render_markdown(&card, &regs, baseline_ts);
    println!("{md}");
    let (json_path, _md_path) = report::write_scorecard(&card, &md)?;
    println!("scorecard written to {}", json_path.display());

    if let Some(min) = args.fail_under {
        if card.system_score.unwrap_or(0) < min || !regs.is_empty() {
            return Ok(1);
        }
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::Target;

    fn scored(id: SectionId, score: u8) -> SectionResult {
        SectionResult {
            id,
            score: Some(score),
            metrics: serde_json::json!({}),
            recommendations: vec![],
            error: None,
        }
    }

    #[test]
    fn grade_thresholds() {
        assert_eq!(grade_from_score(90), Grade::A);
        assert_eq!(grade_from_score(75), Grade::B);
        assert_eq!(grade_from_score(60), Grade::C);
        assert_eq!(grade_from_score(45), Grade::D);
        assert_eq!(grade_from_score(44), Grade::F);
    }

    #[test]
    fn weights_sum_to_100_for_both_targets() {
        for t in [Target::Demo, Target::Live] {
            let sum: u32 = weights(t).iter().map(|(_, w)| w).sum();
            assert_eq!(sum, 100, "{t:?}");
        }
    }

    #[test]
    fn live_has_no_demo_only_sections() {
        let ids: Vec<SectionId> = weights(Target::Live).iter().map(|(i, _)| *i).collect();
        assert!(!ids.contains(&SectionId::Overhead));
        assert!(!ids.contains(&SectionId::Resilience));
        assert!(!ids.contains(&SectionId::Fairshare));
    }

    #[test]
    fn system_score_is_weighted_mean() {
        let w = vec![(SectionId::Capacity, 60), (SectionId::Streaming, 40)];
        let r = vec![
            scored(SectionId::Capacity, 100),
            scored(SectionId::Streaming, 50),
        ];
        assert_eq!(system_score(&r, &w), Some(80)); // 100*0.6 + 50*0.4
    }

    #[test]
    fn skipped_sections_redistribute_weight() {
        let w = vec![(SectionId::Capacity, 60), (SectionId::Streaming, 40)];
        let r = vec![
            scored(SectionId::Capacity, 80),
            SectionResult::skipped(SectionId::Streaming, "quick mode"),
        ];
        assert_eq!(system_score(&r, &w), Some(80)); // streaming weight redistributed
    }

    #[test]
    fn all_skipped_gives_none() {
        let w = vec![(SectionId::Capacity, 100)];
        let r = vec![SectionResult::skipped(SectionId::Capacity, "x")];
        assert_eq!(system_score(&r, &w), None);
    }

    #[test]
    fn errored_grade_and_skipped_grade() {
        assert_eq!(
            SectionResult::errored(SectionId::Overhead, "boom").grade(),
            Grade::Errored
        );
        assert_eq!(
            SectionResult::skipped(SectionId::Overhead, "n/a").grade(),
            Grade::Skipped
        );
        assert_eq!(scored(SectionId::Overhead, 91).grade(), Grade::A);
    }

    #[test]
    fn section_enabled_respects_only_and_skip() {
        // only takes precedence; both match by SectionId::name()
        assert!(section_enabled(SectionId::Capacity, &[], &[]));
        assert!(!section_enabled(
            SectionId::Capacity,
            &["capacity".into()],
            &[]
        ));
        assert!(section_enabled(
            SectionId::Capacity,
            &[],
            &["capacity".into()]
        ));
        assert!(!section_enabled(
            SectionId::Overhead,
            &[],
            &["capacity".into()]
        ));
    }

    #[test]
    fn validate_section_names_accepts_valid() {
        assert!(validate_section_names(&["capacity".into(), "streaming".into()]).is_ok());
    }

    #[test]
    fn validate_section_names_accepts_empty() {
        assert!(validate_section_names(&[]).is_ok());
    }

    #[test]
    fn validate_section_names_rejects_bogus() {
        let err = validate_section_names(&["capacity".into(), "bogus".into()]).unwrap_err();
        assert!(err.contains("bogus"));
        assert!(err.contains("capacity"));
        assert!(err.contains("overhead")); // full valid list is listed
    }

    #[test]
    fn streaming_conc_derivation() {
        assert_eq!(streaming_conc(Some(256), false), 64); // half, capped at 64
        assert_eq!(streaming_conc(Some(64), false), 32);
        assert_eq!(streaming_conc(Some(1), false), 1); // floor 1
        assert_eq!(streaming_conc(None, true), 8); // quick mode fixed
        assert_eq!(streaming_conc(None, false), 8); // no card -> default 8
    }

    #[test]
    fn section_id_names_roundtrip() {
        for id in [
            SectionId::Overhead,
            SectionId::Capacity,
            SectionId::Overload,
            SectionId::Streaming,
            SectionId::Resilience,
            SectionId::Fairshare,
            SectionId::Compression,
        ] {
            assert_eq!(SectionId::from_name(id.name()), Some(id));
        }
        assert_eq!(SectionId::from_name("bogus"), None);
    }
}
