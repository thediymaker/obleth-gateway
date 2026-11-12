//! Advice: what in the running gateway an operator should change, found from
//! the request ledger and the registry, each finding with its evidence.
//!
//! Checks are deterministic: plain ledger queries over the last day plus the
//! registry, so every number on a finding is a measured one. When a judge is
//! wired in (the binary passes a [`JudgeFn`] that calls the gateway's own
//! `/v1/verdicts` as the control-plane key) and a verdicts model is set, each
//! finding is also asked a few yes/no questions about its evidence; the
//! answers order the list and move findings nobody needs to act on down to
//! notes. The judge never adds findings or changes their numbers.
//!
//! Snoozes and dismissals are kept in `app_settings` (`advice`), keyed by a
//! finding's stable `key`, so they hold across replicas and restarts.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::Json;
use clickhouse::Row;
use obleth_config::routing::{OutputStats, WindowedOutput};
use obleth_config::ModelRoute;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::{audit_actor, AdminError, AdminState, Result};

/// The window every check reads.
pub const WINDOW_HOURS: i64 = 24;
const WINDOW_MS: i64 = WINDOW_HOURS * 3_600_000;
/// How long one computed report is served before the checks run again.
const REPORT_TTL: Duration = Duration::from_secs(60);
/// How long one finding's judgment is reused while its evidence stays in the
/// same order of magnitude.
const JUDGMENT_TTL: Duration = Duration::from_secs(3_600);
/// Most findings judged per report; the rest keep their check's order.
const MAX_JUDGED: usize = 12;
/// A background judging pass gives up after this long.
const JUDGE_BUDGET: Duration = Duration::from_secs(90);

/// Failure groups smaller than this are noise, not a pattern.
const MIN_GROUP_FAILURES: u64 = 20;
/// A caller repeating one failure at least this often, over at least an hour,
/// is reported on its own.
const REPEAT_MIN_FAILURES: u64 = 500;
const REPEAT_MIN_SPAN_MS: i64 = 3_600_000;
/// A model whose own failures pass both of these is reported.
const PLATFORM_MIN_FAILURES: u64 = 50;
const PLATFORM_MIN_RATE: f64 = 0.01;
/// Waiting for room: this many requests gave up, or the slowest tenth waited
/// this long, over at least [`WAIT_MIN_REQUESTS`].
const WAIT_MIN_GAVE_UP: u64 = 20;
const WAIT_P90_MS: f64 = 10_000.0;
const WAIT_MIN_REQUESTS: u64 = 100;
/// An Auto model averaging this many output tokens per answer is reported.
const LONG_ANSWER_TOKENS: f64 = 4_000.0;

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// One measured fact behind a finding.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdviceEvidence {
    pub label: String,
    pub value: String,
}

/// A change the dashboard can make for the operator in one step.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AdviceFix {
    /// Add `boon` to each listed model's boons, keeping the rest.
    GrantBoon { boon: String, models: Vec<String> },
}

/// The requests a finding is about, as request-log filters.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct AdviceLogs {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(value_type = Option<String>)]
    pub key_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_code: Option<u16>,
    /// Error classes, comma-separated (`client_input`, `capacity`, ...).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
}

/// What the judge model made of a finding: probabilities of "yes".
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdviceJudgment {
    pub model: String,
    /// Are users' requests failing or slowed because of this?
    pub hurting_users: f64,
    /// Should an operator act on it now?
    pub worth_acting: f64,
    /// Is the offered fix low-risk and easy to undo? Only with a fix.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_is_safe: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdviceFinding {
    /// Stable across reports while it describes the same thing; snoozes key on it.
    pub key: String,
    /// The check that found it.
    pub check: String,
    /// `act` (failing now, fix it), `watch` (worth a look) or `note`.
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub evidence: Vec<AdviceEvidence>,
    pub models: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<AdviceFix>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub logs: Option<AdviceLogs>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub judgment: Option<AdviceJudgment>,
    /// The headline count the finding is about; orders findings of one
    /// severity and decides when a cached judgment is stale.
    pub weight: u64,
}

/// A finding the operator set aside.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SnoozedAdvice {
    pub key: String,
    pub title: String,
    /// When it may come back, unix millis; absent means dismissed for good.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<i64>,
    #[serde(default)]
    pub by: String,
    pub at_ms: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdviceJudge {
    /// The verdicts model asked, when judging is possible at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Why the last judging pass judged nothing, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Some findings are still being judged; a later read has them.
    #[serde(default)]
    pub pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AdviceReport {
    pub generated_at_ms: i64,
    pub window_hours: i64,
    /// Most urgent first. Snoozed findings are left out.
    pub findings: Vec<AdviceFinding>,
    pub snoozed: Vec<SnoozedAdvice>,
    pub judge: AdviceJudge,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SnoozeAdvice {
    /// The finding's title, kept so the snoozed list can name it.
    pub title: String,
    /// When it may come back, unix millis; absent dismisses it for good.
    #[serde(default)]
    pub until_ms: Option<i64>,
}

/// A call into the gateway's own `/v1/verdicts`: request body in, response
/// body out. Injected by the binary that owns the data plane, so this crate
/// never depends on the proxy; `None` means findings go unjudged.
pub type JudgeFn = Arc<
    dyn Fn(
            serde_json::Value,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = std::result::Result<serde_json::Value, String>>
                    + Send,
            >,
        > + Send
        + Sync,
>;

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

#[utoipa::path(
    get, path = "/api/v1/advice", tag = "settings",
    responses((status = 200, body = AdviceReport))
)]
pub(crate) async fn get_advice(State(state): State<AdminState>) -> Result<Json<AdviceReport>> {
    let mut report = cached_report(&state).await?;
    let snoozed = load_snoozed(&state).await?;
    let now = now_ms();
    let active: Vec<&SnoozedAdvice> = snoozed
        .iter()
        .filter(|s| s.until_ms.is_none_or(|until| until > now))
        .collect();
    report
        .findings
        .retain(|f| !active.iter().any(|s| s.key == f.key));
    report.snoozed = active.into_iter().cloned().collect();
    Ok(Json(report))
}

#[utoipa::path(
    put, path = "/api/v1/advice/snoozed/{key}", tag = "settings",
    params(("key" = String, Path, description = "The finding's key")),
    request_body = SnoozeAdvice,
    responses((status = 200, body = [SnoozedAdvice]))
)]
pub(crate) async fn put_snoozed(
    State(state): State<AdminState>,
    Path(key): Path<String>,
    headers: HeaderMap,
    Json(body): Json<SnoozeAdvice>,
) -> Result<Json<Vec<SnoozedAdvice>>> {
    if key.trim().is_empty() || key.len() > 300 {
        return Err(AdminError::BadRequest("a finding key is required".into()));
    }
    let now = now_ms();
    if body.until_ms.is_some_and(|until| until <= now) {
        return Err(AdminError::BadRequest(
            "until_ms must be in the future".into(),
        ));
    }
    let mut all = load_snoozed(&state).await?;
    // Expired entries go when the list is next written.
    all.retain(|s| s.key != key && s.until_ms.is_none_or(|until| until > now));
    all.push(SnoozedAdvice {
        key: key.clone(),
        title: body.title.chars().take(300).collect(),
        until_ms: body.until_ms,
        by: audit_actor(&headers),
        at_ms: now,
    });
    save_snoozed(&state, &all).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            if body.until_ms.is_some() {
                "snooze_advice"
            } else {
                "dismiss_advice"
            },
            "advice",
            &key,
            serde_json::json!({ "until_ms": body.until_ms }),
        )
        .await?;
    Ok(Json(all))
}

#[utoipa::path(
    delete, path = "/api/v1/advice/snoozed/{key}", tag = "settings",
    params(("key" = String, Path, description = "The finding's key")),
    responses((status = 200, body = [SnoozedAdvice]))
)]
pub(crate) async fn delete_snoozed(
    State(state): State<AdminState>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Vec<SnoozedAdvice>>> {
    let mut all = load_snoozed(&state).await?;
    let before = all.len();
    all.retain(|s| s.key != key);
    if all.len() == before {
        return Err(AdminError::NotFound);
    }
    save_snoozed(&state, &all).await?;
    state
        .store
        .record_audit(
            &audit_actor(&headers),
            "restore_advice",
            "advice",
            &key,
            serde_json::json!({}),
        )
        .await?;
    Ok(Json(all))
}

async fn load_snoozed(state: &AdminState) -> Result<Vec<SnoozedAdvice>> {
    let raw = state.store.get_advice_state().await?;
    Ok(raw
        .and_then(|v| v.get("snoozed").cloned())
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default())
}

async fn save_snoozed(state: &AdminState, all: &[SnoozedAdvice]) -> Result<()> {
    let mut raw = state
        .store
        .get_advice_state()
        .await?
        .filter(|v| v.is_object())
        .unwrap_or_else(|| serde_json::json!({}));
    raw["snoozed"] = serde_json::to_value(all).map_err(|e| AdminError::Internal(e.to_string()))?;
    state.store.put_advice_state(&raw).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Report cache + judging
// ---------------------------------------------------------------------------

struct Cache {
    report: Option<(Instant, AdviceReport)>,
    /// fingerprint -> (when judged, judgment)
    judgments: HashMap<String, (Instant, AdviceJudgment)>,
    /// A background judging pass is running.
    judging: bool,
    /// Why the last judging pass judged nothing, if it did.
    judge_error: Option<String>,
}

static CACHE: std::sync::LazyLock<Mutex<Cache>> = std::sync::LazyLock::new(|| {
    Mutex::new(Cache {
        report: None,
        judgments: HashMap::new(),
        judging: false,
        judge_error: None,
    })
});
/// One report computation at a time per process; concurrent page loads wait
/// for it instead of each running every check.
static COMPUTE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Drop this process's cached report, so the next read runs the checks
/// again. Called after a change that can settle a finding (a model update).
pub fn invalidate() {
    CACHE.lock().unwrap_or_else(|p| p.into_inner()).report = None;
}

fn cached(max_age: Duration) -> Option<AdviceReport> {
    let cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    cache
        .report
        .as_ref()
        .filter(|(at, _)| at.elapsed() < max_age)
        .map(|(_, r)| r.clone())
}

async fn cached_report(state: &AdminState) -> Result<AdviceReport> {
    if let Some(r) = cached(REPORT_TTL) {
        return Ok(r);
    }
    let _guard = COMPUTE.lock().await;
    if let Some(r) = cached(REPORT_TTL) {
        return Ok(r);
    }
    let report = compute(state).await?;
    CACHE.lock().unwrap_or_else(|p| p.into_inner()).report = Some((Instant::now(), report.clone()));
    Ok(report)
}

async fn compute(state: &AdminState) -> Result<AdviceReport> {
    let now = now_ms();
    let since = now - WINDOW_MS;
    let (groups, totals, models, boons) = tokio::try_join!(
        async { Ok::<_, AdminError>(query_failure_groups(&state.clickhouse, since).await?) },
        async { Ok::<_, AdminError>(query_model_totals(&state.clickhouse, since).await?) },
        async { Ok::<_, AdminError>(state.store.list_models().await?) },
        async { Ok::<_, AdminError>(state.store.get_boon_settings().await?) },
    )?;
    let vision_describer = boons
        .as_ref()
        .filter(|b| b.vision.enabled)
        .and_then(|b| b.vision.fallback_model.clone());
    let mut findings = run_checks(&Inputs {
        groups: &groups,
        totals: &totals,
        models: &models,
        vision_describer: vision_describer.as_deref(),
        now_ms: now,
    });
    let slurm = state.store.get_slurm_settings().await?.unwrap_or_default();
    if slurm.enabled {
        let managed = state.store.list_managed_models().await?;
        findings.extend(slurm_findings(&slurm, &managed, &models, now));
    }
    rank(&mut findings);

    let router = state
        .store
        .get_auto_router_settings()
        .await?
        .unwrap_or_default();
    let judge_model = router
        .verdicts_default_model
        .clone()
        .filter(|m| !m.trim().is_empty());
    let judge = match (&state.judge, judge_model) {
        (None, _) => AdviceJudge {
            model: None,
            error: Some("judging is not available in this process".into()),
            pending: false,
        },
        (Some(_), None) => AdviceJudge {
            model: None,
            error: Some("no verdicts model is set (Routing > Verdicts default)".into()),
            pending: false,
        },
        (Some(judge_fn), Some(model)) => {
            // Judgments already made are applied now; the rest are asked for
            // in the background so a page load never waits on the judge.
            let pending = apply_cached_judgments(&model, &mut findings);
            rank(&mut findings);
            let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
            if !pending.is_empty() && !cache.judging {
                cache.judging = true;
                let judge_fn = judge_fn.clone();
                let model = model.clone();
                tokio::spawn(async move {
                    let error = judge_all(&judge_fn, &model, pending).await.err();
                    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
                    cache.judging = false;
                    cache.judge_error = error;
                    // The next read picks the new judgments up.
                    cache.report = None;
                });
            }
            AdviceJudge {
                model: Some(model),
                error: cache.judge_error.clone(),
                pending: cache.judging,
            }
        }
    };

    Ok(AdviceReport {
        generated_at_ms: now,
        window_hours: WINDOW_HOURS,
        findings,
        snoozed: Vec::new(),
        judge,
    })
}

/// The cache key for a finding's judgment: same finding, same severity, same
/// order of magnitude of its headline count.
fn fingerprint(f: &AdviceFinding) -> String {
    let magnitude = if f.weight == 0 { 0 } else { f.weight.ilog10() };
    format!("{}|{}|{}", f.key, f.severity, magnitude)
}

/// Apply judgments made earlier to the findings they still fit, and return
/// the findings (up to [`MAX_JUDGED`], most urgent first) that need one.
fn apply_cached_judgments(model: &str, findings: &mut [AdviceFinding]) -> Vec<AdviceFinding> {
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    cache
        .judgments
        .retain(|_, (at, _)| at.elapsed() < JUDGMENT_TTL);
    let mut pending = Vec::new();
    for f in findings.iter_mut().take(MAX_JUDGED) {
        match cache.judgments.get(&fingerprint(f)) {
            Some((_, j)) if j.model == model => f.judgment = Some(j.clone()),
            _ => pending.push(f.clone()),
        }
    }
    pending
}

/// Ask the judge about each finding and keep the answers. Fails only when no
/// finding could be judged.
async fn judge_all(
    judge_fn: &JudgeFn,
    model: &str,
    findings: Vec<AdviceFinding>,
) -> std::result::Result<(), String> {
    let calls = findings.iter().map(|f| {
        let judge_fn = judge_fn.clone();
        let body = verdicts_body(model, f);
        async move { judge_fn(body).await }
    });
    let results = tokio::time::timeout(JUDGE_BUDGET, futures::future::join_all(calls))
        .await
        .map_err(|_| format!("the judge took longer than {} s", JUDGE_BUDGET.as_secs()))?;
    let mut first_error = None;
    let mut judged = 0;
    let mut cache = CACHE.lock().unwrap_or_else(|p| p.into_inner());
    for (f, result) in findings.iter().zip(results) {
        match result.and_then(|resp| parse_judgment(model, &resp)) {
            Ok(j) => {
                judged += 1;
                cache.judgments.insert(fingerprint(f), (Instant::now(), j));
            }
            Err(e) => {
                tracing::warn!(finding = %f.key, error = %e, "advice judge failed");
                first_error.get_or_insert(e);
            }
        }
    }
    match first_error {
        Some(e) if judged == 0 => Err(e),
        _ => Ok(()),
    }
}

/// The `/v1/verdicts` request for one finding: the finding as the state, and
/// two or three yes/no questions about it.
pub fn verdicts_body(model: &str, f: &AdviceFinding) -> serde_json::Value {
    let mut questions = serde_json::json!({
        "hurting_users": {
            "type": "boolean",
            "instructions": "Going only by the evidence, are people's requests failing, \
                being refused, or being made noticeably slower right now because of this?",
            "criteria": {
                "true": "The evidence shows real requests failing or delayed.",
                "false": "Nothing in the evidence shows requests failing or delayed; it is \
                    tidying, cost, or a risk that has not happened."
            }
        },
        "worth_acting": {
            "type": "boolean",
            "instructions": "Should the person who runs this AI gateway act on this today, \
                rather than leave it? Weigh how many requests are involved, whether it is \
                still happening, and whether they can do anything about it.",
            "criteria": {
                "true": "Acting today clearly helps the people using the gateway.",
                "false": "It can wait, it is too small to matter, or only the caller can fix it."
            }
        }
    });
    if let Some(fix) = &f.fix {
        let described = match fix {
            AdviceFix::GrantBoon { boon, models } => format!(
                "Turn on the gateway's `{boon}` boon for {}.",
                models.join(", ")
            ),
        };
        questions["fix_is_safe"] = serde_json::json!({
            "type": "boolean",
            "instructions": format!(
                "The proposed fix is: {described} Is this fix low-risk and easy to undo, \
                 such that it will not break requests that work today?"
            )
        });
    }
    serde_json::json!({
        "model": model,
        "on_no_answer": "low_confidence",
        "state": {
            "finding": f.title,
            "explanation": f.detail,
            "evidence": f.evidence.iter().map(|e| format!("{}: {}", e.label, e.value)).collect::<Vec<_>>(),
            "window": format!("the last {WINDOW_HOURS} hours"),
        },
        "questions": questions,
    })
}

fn yes_probability(resp: &serde_json::Value, q: &str) -> Option<f64> {
    resp.pointer(&format!("/verdicts/{q}/probabilities/true"))
        .and_then(|v| v.as_f64())
}

pub fn parse_judgment(
    model: &str,
    resp: &serde_json::Value,
) -> std::result::Result<AdviceJudgment, String> {
    let hurting = yes_probability(resp, "hurting_users");
    let worth = yes_probability(resp, "worth_acting");
    match (hurting, worth) {
        (Some(hurting_users), Some(worth_acting)) => Ok(AdviceJudgment {
            model: resp
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or(model)
                .to_string(),
            hurting_users,
            worth_acting,
            fix_is_safe: yes_probability(resp, "fix_is_safe"),
        }),
        _ => Err("the judge's answer had no verdicts".into()),
    }
}

fn severity_rank(s: &str) -> u8 {
    match s {
        "act" => 3,
        "watch" => 2,
        _ => 1,
    }
}

/// Order findings and let judgments move them: a finding the judge thinks is
/// not worth acting on drops to a note; a `watch` the judge says is hurting
/// users and worth acting on rises to `act`. Then most urgent first.
pub fn rank(findings: &mut [AdviceFinding]) {
    for f in findings.iter_mut() {
        if let Some(j) = &f.judgment {
            if j.worth_acting < 0.25 {
                f.severity = "note".into();
            } else if f.severity == "watch" && j.worth_acting >= 0.75 && j.hurting_users >= 0.75 {
                f.severity = "act".into();
            }
        }
    }
    findings.sort_by(|a, b| {
        let score = |f: &AdviceFinding| {
            f.judgment
                .as_ref()
                .map(|j| j.worth_acting + 0.5 * j.hurting_users)
                .unwrap_or(0.0)
        };
        severity_rank(&b.severity)
            .cmp(&severity_rank(&a.severity))
            .then(score(b).total_cmp(&score(a)))
            .then(b.weight.cmp(&a.weight))
            .then(a.key.cmp(&b.key))
    });
}

// ---------------------------------------------------------------------------
// Ledger reads
// ---------------------------------------------------------------------------

/// The same failure over and over: one caller, one model, one status, one
/// class, one reason once its numbers are set aside.
#[derive(Debug, Clone, Row, Deserialize)]
pub struct FailureGroup {
    pub class: String,
    pub platform: bool,
    #[serde(with = "clickhouse::serde::uuid")]
    pub tenant_id: Uuid,
    pub tenant_name: String,
    #[serde(with = "clickhouse::serde::uuid")]
    pub key_id: Uuid,
    pub key_name: String,
    pub model: String,
    pub status_code: u16,
    pub reason: String,
    pub failures: u64,
    pub first_ms: i64,
    pub last_ms: i64,
}

/// One model's day in the ledger.
#[derive(Debug, Clone, Row, Deserialize)]
pub struct ModelTotals {
    pub model: String,
    pub requests: u64,
    pub platform_failures: u64,
    pub gave_up_waiting: u64,
    pub p90_wait_ms: f64,
    pub answers: u64,
    pub mean_output: f64,
}

fn failure_groups_sql() -> String {
    // Aliases stay clear of the column names the query reads.
    format!(
        "select {class} as filed_class, filed_class in {platform} as is_platform, \
         tenant_id, any(tenant_name) as tn, key_id, any(key_name) as kn, model, status_code, \
         any(error_reason) as sample_reason, count() as failed, \
         min(ts_ms) as first_at, max(ts_ms) as last_at \
         from usage where ts_ms >= ? and status_code >= 400 \
         group by filed_class, tenant_id, key_id, model, status_code, \
         replaceRegexpAll(error_reason, '[0-9]+', 'N') \
         having failed >= {MIN_GROUP_FAILURES} \
         order by failed desc limit 200",
        class = obleth_config::EFFECTIVE_CLASS_SQL,
        platform = obleth_config::PLATFORM_CLASSES_SQL,
    )
}

fn model_totals_sql() -> String {
    format!(
        "select model, count() as n, \
         countIf(status_code >= 400 and {class} in {platform} and {class} != 'capacity') as pf, \
         countIf({class} = 'capacity') as gave_up, \
         ifNotFinite(toFloat64(quantileIf(0.9)(queue_wait_ms, status_code < 400)), 0) as p90w, \
         countIf(status_code = 200 and output_tokens > 0 and request_type = 'chat') as answered, \
         ifNotFinite(avgIf(toFloat64(output_tokens), \
             status_code = 200 and output_tokens > 0 and request_type = 'chat'), 0) as mean_out \
         from usage where ts_ms >= ? group by model",
        class = obleth_config::EFFECTIVE_CLASS_SQL,
        platform = obleth_config::PLATFORM_CLASSES_SQL,
    )
}

async fn query_failure_groups(
    client: &clickhouse::Client,
    since_ms: i64,
) -> std::result::Result<Vec<FailureGroup>, clickhouse::error::Error> {
    client
        .query(&failure_groups_sql())
        .bind(since_ms)
        .fetch_all::<FailureGroup>()
        .await
}

async fn query_model_totals(
    client: &clickhouse::Client,
    since_ms: i64,
) -> std::result::Result<Vec<ModelTotals>, clickhouse::error::Error> {
    client
        .query(&model_totals_sql())
        .bind(since_ms)
        .fetch_all::<ModelTotals>()
        .await
}

// ---------------------------------------------------------------------------
// Checks
// ---------------------------------------------------------------------------

pub struct Inputs<'a> {
    pub groups: &'a [FailureGroup],
    pub totals: &'a [ModelTotals],
    pub models: &'a [ModelRoute],
    /// The vision boon's describer, when the boon is on and has one.
    pub vision_describer: Option<&'a str>,
    pub now_ms: i64,
}

/// Every check over one set of inputs. Pure, so it is tested against
/// synthetic ledgers.
pub fn run_checks(inp: &Inputs<'_>) -> Vec<AdviceFinding> {
    let mut out = Vec::new();
    let mut covered: Vec<usize> = Vec::new();
    images_refused(inp, &mut out, &mut covered);
    repeating_failures(inp, &covered, &mut out);
    platform_failures(inp, &mut out);
    waiting_for_room(inp, &mut out);
    long_answers(inp, &mut out);
    out
}

/// Slurm findings: a token that expires within three days or has expired,
/// and a managed model whose submits are paused. Pure, for tests.
pub fn slurm_findings(
    settings: &obleth_config::SlurmSettings,
    managed: &[obleth_config::ManagedModelSpec],
    models: &[ModelRoute],
    now_ms: i64,
) -> Vec<AdviceFinding> {
    let mut out = Vec::new();
    let now = now_ms / 1000;
    let exp = obleth_config::slurm::slurm_jwt_claims(&settings.slurm_jwt).and_then(|c| c.exp);
    if let Some(exp) = exp {
        let left = exp - now;
        if left <= obleth_provisioner::slurm::TOKEN_WARN_SECS {
            let at = chrono::DateTime::from_timestamp(exp, 0)
                .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default();
            let (title, detail) = if left <= 0 {
                (
                    "The Slurm token has expired".to_string(),
                    "slurmrestd refuses every request, so no Slurm deployment can start, and \
                     running ones are not watched. Mint a new token with `scontrol token \
                     lifespan=2592000` and save it in the Slurm settings."
                        .to_string(),
                )
            } else {
                (
                    format!(
                        "The Slurm token expires in {}",
                        obleth_provisioner::slurm::plain_duration(left)
                    ),
                    "When it expires slurmrestd refuses every request and Slurm deployments \
                     stop being managed. Mint a new token with `scontrol token \
                     lifespan=2592000` and save it in the Slurm settings."
                        .to_string(),
                )
            };
            out.push(AdviceFinding {
                key: format!("slurm_token_expiry:{exp}"),
                check: "slurm_token".into(),
                severity: "act".into(),
                title,
                detail,
                evidence: vec![AdviceEvidence {
                    label: "Expires".into(),
                    value: at,
                }],
                models: vec![],
                fix: None,
                logs: None,
                judgment: None,
                weight: 1,
            });
        }
    }
    for m in managed.iter().filter(|m| m.enabled) {
        let Some(reason) = m.paused_reason.as_deref() else {
            continue;
        };
        if m.paused_until
            .is_some_and(|u| u.timestamp_millis() <= now_ms)
        {
            continue;
        }
        let name = models
            .iter()
            .find(|r| r.id == m.model_id)
            .map(|r| r.model_name.clone())
            .unwrap_or_else(|| m.model_id.to_string());
        let mut evidence = vec![AdviceEvidence {
            label: "Failed launches in a row".into(),
            value: m.failed_launches.to_string(),
        }];
        if let Some(until) = m.paused_until {
            evidence.push(AdviceEvidence {
                label: "Next try".into(),
                value: until.format("%Y-%m-%d %H:%M UTC").to_string(),
            });
        }
        out.push(AdviceFinding {
            key: format!("slurm_paused:{}", m.model_id),
            check: "slurm_paused".into(),
            severity: "act".into(),
            title: format!("{name} has stopped starting jobs on Slurm"),
            detail: format!(
                "{reason} Fix the cause (the job's log usually says it), then press Try again \
                 on the deployment to start a job now."
            ),
            evidence,
            models: vec![name],
            fix: None,
            logs: None,
            judgment: None,
            weight: u64::try_from(m.failed_launches.max(1)).unwrap_or(1),
        });
    }
    out
}

fn model<'a>(inp: &Inputs<'a>, name: &str) -> Option<&'a ModelRoute> {
    inp.models.iter().find(|m| m.model_name == name)
}

fn is_image_refusal(reason: &str) -> bool {
    let r = reason.to_ascii_lowercase();
    ["image", "multimodal", "multi-modal", "vision"]
        .iter()
        .any(|w| r.contains(w))
}

fn fmt_count(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn fmt_ago(now_ms: i64, at_ms: i64) -> String {
    let mins = ((now_ms - at_ms).max(0) / 60_000) as u64;
    match mins {
        0 => "just now".into(),
        1..=59 => format!("{mins} min ago"),
        _ if mins < 48 * 60 => format!("{} h ago", mins / 60),
        _ => format!("{} days ago", mins / 1440),
    }
}

fn caller(g: &FailureGroup) -> String {
    let key = if g.key_name.is_empty() {
        g.key_id.to_string()[..8].to_string()
    } else {
        g.key_name.clone()
    };
    if g.tenant_name.is_empty() {
        key
    } else {
        format!("{key} ({})", g.tenant_name)
    }
}

/// Images sent to models that cannot read them, which the vision boon would
/// have answered by describing the image first.
fn images_refused(inp: &Inputs<'_>, out: &mut Vec<AdviceFinding>, covered: &mut Vec<usize>) {
    // model -> (failures, last_ms, callers)
    let mut by_model: BTreeMap<String, (u64, i64, BTreeMap<String, u64>)> = BTreeMap::new();
    for (i, g) in inp.groups.iter().enumerate() {
        if g.class != "client_input" || !is_image_refusal(&g.reason) {
            continue;
        }
        let Some(m) = model(inp, &g.model) else {
            continue;
        };
        // Image refusals are this check's, reported or not, so the repeating-
        // failure check never reports them a second time.
        covered.push(i);
        // A native vision model refusing (too many images) is the caller's to
        // fix; a model granted the boon since has its images described from
        // now on, though the window still holds its earlier refusals.
        if m.supports_vision || m.boons.iter().any(|b| b == "vision") {
            continue;
        }
        let e = by_model.entry(g.model.clone()).or_default();
        e.0 += g.failures;
        e.1 = e.1.max(g.last_ms);
        *e.2.entry(caller(g)).or_default() += g.failures;
    }
    if by_model.is_empty() {
        return;
    }
    let total: u64 = by_model.values().map(|v| v.0).sum();
    let last = by_model.values().map(|v| v.1).max().unwrap_or(0);
    let mut callers: BTreeMap<String, u64> = BTreeMap::new();
    for (_, _, c) in by_model.values() {
        for (k, n) in c {
            *callers.entry(k.clone()).or_default() += n;
        }
    }
    let mut callers: Vec<(String, u64)> = callers.into_iter().collect();
    callers.sort_by_key(|c| std::cmp::Reverse(c.1));
    let mut per_model: Vec<(&String, u64)> = by_model.iter().map(|(k, v)| (k, v.0)).collect();
    per_model.sort_by_key(|c| std::cmp::Reverse(c.1));
    let names: Vec<String> = per_model.iter().map(|(m, _)| (*m).clone()).collect();
    let n = names.len();
    let who = if n == 1 {
        names[0].clone()
    } else {
        format!("{n} text-only models")
    };

    let (detail, fix) =
        match inp.vision_describer {
            Some(describer) => {
                (
                    format!(
                "{} read only text, so each request that carried an image failed. With the \
                 vision boon, {describer} describes the image in words first and the model \
                 answers from that description.",
                if n == 1 { "This model can" } else { "These models can" }
            ),
                    Some(AdviceFix::GrantBoon {
                        boon: "vision".into(),
                        models: names.clone(),
                    }),
                )
            }
            None => (
                "These models read only text, so each request that carried an image failed. \
             The vision boon would describe images for them, but it is off or has no \
             describer model: turn it on in Settings > Boons first."
                    .to_string(),
                None,
            ),
        };
    let mut evidence: Vec<AdviceEvidence> = per_model
        .iter()
        .take(5)
        .map(|(m, c)| AdviceEvidence {
            label: (*m).clone(),
            value: format!("{} refused", fmt_count(*c)),
        })
        .collect();
    if let Some((top, c)) = callers.first() {
        evidence.push(AdviceEvidence {
            label: if callers.len() == 1 {
                "Sent by".into()
            } else {
                "Mostly from".into()
            },
            value: format!("{top}, {}", fmt_count(*c)),
        });
    }
    evidence.push(AdviceEvidence {
        label: "Last".into(),
        value: fmt_ago(inp.now_ms, last),
    });
    out.push(AdviceFinding {
        key: "images_refused".into(),
        check: "images_refused".into(),
        severity: "act".into(),
        title: format!(
            "{who} refused {} requests with images in the last day",
            fmt_count(total)
        ),
        detail,
        evidence,
        models: names.clone(),
        fix,
        logs: Some(AdviceLogs {
            model: (n == 1).then(|| names[0].clone()),
            status_code: Some(400),
            error_class: Some("client_input".into()),
            ..Default::default()
        }),
        judgment: None,
        weight: total,
    });
}

/// The prompt length a context-overflow reason names, when it names one.
fn overflow_length(reason: &str) -> Option<i64> {
    let r = reason.to_ascii_lowercase();
    if !(r.contains("context") && (r.contains("length") || r.contains("token"))) {
        return None;
    }
    // The first number is the prompt's length in the common wordings
    // ("Input length (134044) exceeds ...", "prompt contains at least 196609
    // input tokens ...").
    let digits: String = r
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// One caller sending the same failing request again and again.
fn repeating_failures(inp: &Inputs<'_>, covered: &[usize], out: &mut Vec<AdviceFinding>) {
    let mut reported = 0;
    for (i, g) in inp.groups.iter().enumerate() {
        if reported >= 5 {
            break;
        }
        if covered.contains(&i)
            || g.platform
            || !matches!(g.class.as_str(), "client_input" | "limit" | "retired")
            || g.failures < REPEAT_MIN_FAILURES
            || g.last_ms - g.first_ms < REPEAT_MIN_SPAN_MS
        {
            continue;
        }
        reported += 1;
        let who = caller(g);
        let mut evidence = vec![
            AdviceEvidence {
                label: "Failures".into(),
                value: format!("{} ({})", fmt_count(g.failures), g.status_code),
            },
            AdviceEvidence {
                label: "Between".into(),
                value: format!(
                    "{} and {}",
                    fmt_ago(inp.now_ms, g.first_ms),
                    fmt_ago(inp.now_ms, g.last_ms)
                ),
            },
        ];
        if !g.reason.is_empty() {
            evidence.push(AdviceEvidence {
                label: "Error".into(),
                value: g.reason.chars().take(160).collect(),
            });
        }
        let detail = match (g.class.as_str(), overflow_length(&g.reason)) {
            ("client_input", Some(len)) => {
                let mut fits: Vec<&ModelRoute> = inp
                    .models
                    .iter()
                    .filter(|m| {
                        m.enabled
                            && m.model_type == obleth_config::DEFAULT_MODEL_TYPE
                            && m.context_window >= len
                            && m.model_name != g.model
                    })
                    .collect();
                fits.sort_by_key(|m| m.context_window);
                let names: Vec<String> = fits
                    .iter()
                    .take(3)
                    .map(|m| format!("{} ({}K)", m.model_name, m.context_window / 1024))
                    .collect();
                format!(
                    "Its prompts are longer than {} takes, so each one is refused. Only the \
                     caller can fix this: by trimming the prompt or calling a model with a \
                     longer context{}.",
                    g.model,
                    if names.is_empty() {
                        String::new()
                    } else {
                        format!(" such as {}", names.join(", "))
                    }
                )
            }
            ("retired", _) => format!(
                "{} is retired, and this caller still sends to it. Tell its owner which model \
                 replaces it.",
                g.model
            ),
            ("limit", _) => "The caller keeps hitting its rate or spend limit. Either the \
                 limit is too low for its work, or the client retries instead of backing off."
                .to_string(),
            _ => "The same request fails the same way each time, so the client is most \
                 likely retrying without changing it. Its owner is the one who can fix it."
                .to_string(),
        };
        out.push(AdviceFinding {
            key: format!("repeating:{}:{}:{}", g.key_id, g.model, g.status_code),
            check: "repeating_failure".into(),
            severity: "watch".into(),
            title: format!("{who} keeps sending requests {} refuses", g.model),
            detail,
            evidence,
            models: vec![g.model.clone()],
            fix: None,
            logs: Some(AdviceLogs {
                model: Some(g.model.clone()),
                key_id: Some(g.key_id),
                status_code: Some(g.status_code),
                error_class: None,
            }),
            judgment: None,
            weight: g.failures,
        });
    }
}

/// A model failing requests on the platform's side.
fn platform_failures(inp: &Inputs<'_>, out: &mut Vec<AdviceFinding>) {
    for t in inp.totals {
        if t.platform_failures < PLATFORM_MIN_FAILURES || t.requests == 0 {
            continue;
        }
        let rate = t.platform_failures as f64 / t.requests as f64;
        if rate < PLATFORM_MIN_RATE {
            continue;
        }
        let top = inp
            .groups
            .iter()
            .filter(|g| g.model == t.model && g.platform && g.class != "capacity")
            .max_by_key(|g| g.failures);
        let mut evidence = vec![AdviceEvidence {
            label: "Failed".into(),
            value: format!(
                "{} of {} ({:.1}%)",
                fmt_count(t.platform_failures),
                fmt_count(t.requests),
                rate * 100.0
            ),
        }];
        if let Some(g) = top {
            evidence.push(AdviceEvidence {
                label: "Most often".into(),
                value: format!(
                    "{}: {}",
                    g.status_code,
                    g.reason.chars().take(140).collect::<String>()
                ),
            });
            evidence.push(AdviceEvidence {
                label: "Last".into(),
                value: fmt_ago(inp.now_ms, g.last_ms),
            });
        }
        out.push(AdviceFinding {
            key: format!("platform:{}", t.model),
            check: "platform_failures".into(),
            severity: if rate >= 0.05 { "act" } else { "watch" }.into(),
            title: format!(
                "{} failed {} requests on the server side",
                t.model,
                fmt_count(t.platform_failures)
            ),
            detail: "These failures are the platform's, not the callers': the model server \
                     was unreachable, broke off an answer, or did not answer in time. Check \
                     the model's replicas and their logs."
                .into(),
            evidence,
            models: vec![t.model.clone()],
            fix: None,
            logs: Some(AdviceLogs {
                model: Some(t.model.clone()),
                error_class: Some("upstream_error,upstream_timeout,network,gateway".into()),
                ..Default::default()
            }),
            judgment: None,
            weight: t.platform_failures,
        });
    }
}

/// A model whose requests wait a long time for a free slot, or give up.
fn waiting_for_room(inp: &Inputs<'_>, out: &mut Vec<AdviceFinding>) {
    for t in inp.totals {
        if t.requests < WAIT_MIN_REQUESTS {
            continue;
        }
        let slow = t.p90_wait_ms >= WAIT_P90_MS;
        let gave_up = t.gave_up_waiting >= WAIT_MIN_GAVE_UP;
        if !slow && !gave_up {
            continue;
        }
        let mut evidence = vec![AdviceEvidence {
            label: "Slowest tenth waited".into(),
            value: format!("{:.0} s or more", t.p90_wait_ms / 1000.0),
        }];
        if t.gave_up_waiting > 0 {
            evidence.push(AdviceEvidence {
                label: "Gave up waiting".into(),
                value: format!("{} (503)", fmt_count(t.gave_up_waiting)),
            });
        }
        evidence.push(AdviceEvidence {
            label: "Requests".into(),
            value: fmt_count(t.requests),
        });
        let max_in_flight = model(inp, &t.model).and_then(|m| m.max_in_flight);
        if let Some(cap) = max_in_flight {
            evidence.push(AdviceEvidence {
                label: "Gateway limit".into(),
                value: format!("{cap} at once"),
            });
        }
        out.push(AdviceFinding {
            key: format!("waiting:{}", t.model),
            check: "waiting_for_room".into(),
            severity: if gave_up { "act" } else { "watch" }.into(),
            title: format!("Requests to {} wait for room", t.model),
            detail: "More requests arrive than the model's servers take at once, so they \
                     queue in the gateway. Add replicas, or raise how many requests each \
                     replica takes if its servers have headroom."
                .into(),
            evidence,
            models: vec![t.model.clone()],
            fix: None,
            logs: Some(AdviceLogs {
                model: Some(t.model.clone()),
                ..Default::default()
            }),
            judgment: None,
            weight: t.gave_up_waiting.max(t.requests / 10),
        });
    }
}

/// An Auto model whose answers run long enough to make Auto requests slow.
fn long_answers(inp: &Inputs<'_>, out: &mut Vec<AdviceFinding>) {
    for t in inp.totals {
        let Some(m) = model(inp, &t.model) else {
            continue;
        };
        if !(m.enabled && m.auto_eligible && m.model_type == obleth_config::DEFAULT_MODEL_TYPE) {
            continue;
        }
        if t.answers < obleth_config::routing::WINDOWED_OUTPUT_MIN_ANSWERS
            || t.mean_output < LONG_ANSWER_TOKENS
        {
            continue;
        }
        out.push(AdviceFinding {
            key: format!("long_answers:{}", t.model),
            check: "long_answers".into(),
            severity: "note".into(),
            title: format!(
                "{} writes long answers: {} tokens on average",
                t.model,
                fmt_count(t.mean_output.round() as u64)
            ),
            detail: "Auto can send requests here. Its token price is low, but long answers \
                     make each request slow. Auto already counts the length when it compares \
                     costs; if Auto requests should come back quickly, take this model out of \
                     Auto. It still answers when called by name."
                .into(),
            evidence: vec![
                AdviceEvidence {
                    label: "Answers".into(),
                    value: fmt_count(t.answers),
                },
                AdviceEvidence {
                    label: "Average length".into(),
                    value: format!("{} tokens", fmt_count(t.mean_output.round() as u64)),
                },
            ],
            models: vec![t.model.clone()],
            fix: None,
            logs: None,
            judgment: None,
            weight: t.mean_output.round() as u64,
        });
    }
}

// ---------------------------------------------------------------------------
// Router output-length refresher
// ---------------------------------------------------------------------------

#[derive(Debug, Row, Deserialize)]
struct OutputRow {
    model: String,
    mean: f64,
    answers: u64,
}

/// Read each model's mean answer length over the last day from the ledger.
pub async fn read_windowed_output(
    client: &clickhouse::Client,
) -> std::result::Result<HashMap<String, WindowedOutput>, clickhouse::error::Error> {
    let rows = client
        .query(
            "select model, avg(toFloat64(output_tokens)) as mean, count() as answers \
             from usage where ts_ms >= ? and status_code = 200 and output_tokens > 0 \
             and request_type = 'chat' group by model",
        )
        .bind(now_ms() - WINDOW_MS)
        .fetch_all::<OutputRow>()
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                r.model,
                WindowedOutput {
                    mean: r.mean,
                    answers: r.answers,
                },
            )
        })
        .collect())
}

/// Keep the router's expected answer lengths on the ledger's last day,
/// refreshed every five minutes. A failed read keeps the previous figures.
pub fn spawn_output_stats_refresher(client: clickhouse::Client, stats: OutputStats) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(300));
        loop {
            tick.tick().await;
            match read_windowed_output(&client).await {
                Ok(figures) => stats.set_windowed(figures),
                Err(e) => {
                    tracing::warn!(error = %e, "could not read answer lengths from the ledger")
                }
            }
        }
    });
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(name: &str, vision: bool, boons: &[&str]) -> ModelRoute {
        let mut m: ModelRoute = serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4(),
            "model_name": name, "description": "", "upstream_model": name,
            "api_base": "https://up/v1", "api_key": null, "model_type": "chat",
            "input_cost_per_token": 0.0, "output_cost_per_token": 0.0,
            "context_window": 8192, "admission_weight": 100, "max_in_flight": null,
            "supports_function_calling": false, "supports_system_messages": true,
            "supports_response_schema": false, "supports_tool_choice": false,
            "enabled": true, "cache_enabled": false, "cache_ttl_secs": 300,
            "created_at": "2024-01-01T00:00:00Z",
            "updated_at": "2024-01-01T00:00:00Z",
        }))
        .expect("minimal model route");
        m.supports_vision = vision;
        m.boons = boons.iter().map(|b| b.to_string()).collect();
        m.enabled = true;
        m.auto_eligible = true;
        m.model_type = obleth_config::DEFAULT_MODEL_TYPE.to_string();
        m.context_window = 32_768;
        m
    }

    fn group(
        model: &str,
        class: &str,
        status: u16,
        reason: &str,
        n: u64,
        span_h: i64,
    ) -> FailureGroup {
        FailureGroup {
            class: class.into(),
            platform: matches!(
                class,
                "upstream_error" | "upstream_timeout" | "network" | "capacity"
            ),
            tenant_id: Uuid::nil(),
            tenant_name: "lab".into(),
            key_id: Uuid::from_u128(7),
            key_name: "lab-key".into(),
            model: model.into(),
            status_code: status,
            reason: reason.into(),
            failures: n,
            first_ms: 1_000_000_000 - span_h * 3_600_000,
            last_ms: 1_000_000_000,
        }
    }

    fn totals(model: &str, requests: u64) -> ModelTotals {
        ModelTotals {
            model: model.into(),
            requests,
            platform_failures: 0,
            gave_up_waiting: 0,
            p90_wait_ms: 0.0,
            answers: requests,
            mean_output: 300.0,
        }
    }

    fn inputs<'a>(
        groups: &'a [FailureGroup],
        totals: &'a [ModelTotals],
        models: &'a [ModelRoute],
        describer: Option<&'a str>,
    ) -> Inputs<'a> {
        Inputs {
            groups,
            totals,
            models,
            vision_describer: describer,
            now_ms: 1_000_000_000,
        }
    }

    #[test]
    fn images_to_text_models_offer_the_vision_boon_once() {
        let models = [
            route("coder", false, &[]),
            route("big", false, &[]),
            route("seer", true, &[]),
        ];
        let groups = [
            group(
                "coder",
                "client_input",
                400,
                "coder is not a multimodal model",
                19_000,
                8,
            ),
            group(
                "big",
                "client_input",
                400,
                "At most 0 image(s) may be provided",
                16_000,
                8,
            ),
            // A native vision model refusing is the caller's problem.
            group(
                "seer",
                "client_input",
                400,
                "At most 4 image(s) may be provided",
                30,
                1,
            ),
        ];
        let f = run_checks(&inputs(&groups, &[], &models, Some("seer")));
        let img: Vec<_> = f.iter().filter(|f| f.check == "images_refused").collect();
        assert_eq!(img.len(), 1);
        assert_eq!(img[0].weight, 35_000);
        assert!(img[0]
            .title
            .starts_with("2 text-only models refused 35,000"));
        match &img[0].fix {
            Some(AdviceFix::GrantBoon { boon, models }) => {
                assert_eq!(boon, "vision");
                assert_eq!(models, &vec!["coder".to_string(), "big".to_string()]);
            }
            other => panic!("expected a vision grant, got {other:?}"),
        }
        // The same groups are not reported again as repeating failures.
        assert!(f.iter().all(|f| f.check != "repeating_failure"));
    }

    #[test]
    fn models_granted_the_boon_since_drop_out() {
        let models = [route("coder", false, &["vision"])];
        let groups = [group(
            "coder",
            "client_input",
            400,
            "not a multimodal model",
            9_000,
            8,
        )];
        let f = run_checks(&inputs(&groups, &[], &models, Some("seer")));
        assert!(f.is_empty(), "{f:?}");
    }

    #[test]
    fn images_refused_without_a_describer_offers_no_fix() {
        let models = [route("coder", false, &[])];
        let groups = [group(
            "coder",
            "client_input",
            400,
            "not a multimodal model",
            100,
            1,
        )];
        let f = run_checks(&inputs(&groups, &[], &models, None));
        assert!(f[0].fix.is_none());
        assert!(f[0].detail.contains("turn it on"));
    }

    #[test]
    fn context_overflow_names_models_that_fit() {
        let mut long = route("long", false, &[]);
        long.context_window = 262_144;
        let models = [route("short", false, &[]), long];
        let groups = [group(
            "short",
            "client_input",
            400,
            "Input length (134044) exceeds model's maximum context length (131072).",
            900,
            5,
        )];
        let f = run_checks(&inputs(&groups, &[], &models, None));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].check, "repeating_failure");
        assert!(f[0].detail.contains("long (256K)"), "{}", f[0].detail);
    }

    #[test]
    fn short_or_brief_failure_runs_are_not_reported() {
        let models = [route("m", false, &[])];
        let groups = [
            group("m", "client_input", 400, "bad field", 499, 5),
            group("m", "client_input", 422, "bad field", 5_000, 0),
        ];
        assert!(run_checks(&inputs(&groups, &[], &models, None)).is_empty());
    }

    #[test]
    fn platform_failures_need_count_and_rate() {
        let models = [route("m", false, &[])];
        let mut t = totals("m", 10_000);
        t.platform_failures = 60; // 0.6%: under the rate
        assert!(run_checks(&inputs(&[], &[t.clone()], &models, None)).is_empty());
        t.platform_failures = 600; // 6%
        let f = run_checks(&inputs(&[], &[t], &models, None));
        assert_eq!(f[0].check, "platform_failures");
        assert_eq!(f[0].severity, "act");
    }

    #[test]
    fn long_answers_need_enough_answers_and_an_auto_model() {
        let mut out_of_auto = route("quiet", false, &[]);
        out_of_auto.auto_eligible = false;
        let models = [route("thinker", false, &[]), out_of_auto];
        let mut t = totals("thinker", 150);
        t.mean_output = 9_000.0;
        assert!(run_checks(&inputs(&[], &[t.clone()], &models, None)).is_empty());
        t.answers = 5_000;
        t.requests = 5_000;
        let mut q = t.clone();
        q.model = "quiet".into();
        let f = run_checks(&inputs(&[], &[t, q], &models, None));
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].key, "long_answers:thinker");
    }

    #[test]
    fn judgments_reorder_and_demote() {
        let mk = |key: &str, sev: &str, weight: u64, j: Option<(f64, f64)>| AdviceFinding {
            key: key.into(),
            check: "x".into(),
            severity: sev.into(),
            title: key.into(),
            detail: String::new(),
            evidence: vec![],
            models: vec![],
            fix: None,
            logs: None,
            judgment: j.map(|(h, w)| AdviceJudgment {
                model: "judge".into(),
                hurting_users: h,
                worth_acting: w,
                fix_is_safe: None,
            }),
            weight,
        };
        let mut f = vec![
            mk("big-but-pointless", "act", 9_000, Some((0.1, 0.1))),
            mk("watch-that-hurts", "watch", 10, Some((0.9, 0.9))),
            mk("plain-watch", "watch", 500, None),
        ];
        rank(&mut f);
        assert_eq!(f[0].key, "watch-that-hurts");
        assert_eq!(f[0].severity, "act");
        assert_eq!(f[1].key, "plain-watch");
        assert_eq!(f[2].severity, "note");
    }

    #[test]
    fn judgment_parses_boolean_probabilities() {
        let resp = serde_json::json!({
            "model": "jeffan",
            "verdicts": {
                "hurting_users": {"type": "boolean", "value": true, "probabilities": {"true": 0.9, "false": 0.1}, "confidence": 0.8},
                "worth_acting": {"type": "boolean", "value": true, "probabilities": {"true": 0.7, "false": 0.3}, "confidence": 0.5}
            }
        });
        let j = parse_judgment("x", &resp).unwrap();
        assert_eq!(j.model, "jeffan");
        assert!((j.hurting_users - 0.9).abs() < 1e-9);
        assert!(j.fix_is_safe.is_none());
        assert!(parse_judgment("x", &serde_json::json!({})).is_err());
    }

    #[test]
    fn verdicts_body_asks_about_the_fix_only_when_there_is_one() {
        let models = [route("coder", false, &[])];
        let groups = [group(
            "coder",
            "client_input",
            400,
            "not a multimodal model",
            100,
            1,
        )];
        let f = run_checks(&inputs(&groups, &[], &models, Some("seer")));
        let body = verdicts_body("judge", &f[0]);
        assert!(body["questions"]["fix_is_safe"].is_object());
        assert_eq!(body["model"], "judge");
    }

    #[test]
    fn slurm_findings_cover_token_expiry_and_paused_models() {
        use base64::Engine;
        let now_ms = 1_700_000_000_000;
        let tok = |exp: i64| {
            let enc = |b: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
            format!("{}.{}.s", enc("{}"), enc(&format!(r#"{{"exp":{exp}}}"#)))
        };
        let settings = |jwt: String| obleth_config::SlurmSettings {
            enabled: true,
            slurm_jwt: jwt,
            ..Default::default()
        };
        let none = slurm_findings(
            &settings(tok(1_700_000_000 + 10 * 86_400)),
            &[],
            &[],
            now_ms,
        );
        assert!(none.is_empty());
        let f = slurm_findings(&settings(tok(1_700_000_000 + 2 * 86_400)), &[], &[], now_ms);
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].title, "The Slurm token expires in 2 days");
        assert_eq!(f[0].severity, "act");
        let f = slurm_findings(&settings(tok(1_700_000_000 - 5)), &[], &[], now_ms);
        assert_eq!(f[0].title, "The Slurm token has expired");

        let id = Uuid::new_v4();
        let paused = obleth_config::ManagedModelSpec {
            model_id: id,
            paused_reason: Some("Three launches in a row ended before the server answered.".into()),
            paused_until: chrono::DateTime::from_timestamp(1_700_000_600, 0),
            failed_launches: 3,
            ..Default::default()
        };
        let lifted = obleth_config::ManagedModelSpec {
            model_id: Uuid::new_v4(),
            paused_reason: Some("old".into()),
            paused_until: chrono::DateTime::from_timestamp(1_699_999_000, 0),
            ..Default::default()
        };
        let mut route = route("glm", false, &[]);
        route.id = id;
        let f = slurm_findings(
            &settings(String::new()),
            &[paused, lifted],
            &[route],
            now_ms,
        );
        assert_eq!(f.len(), 1);
        assert_eq!(f[0].key, format!("slurm_paused:{id}"));
        assert_eq!(f[0].title, "glm has stopped starting jobs on Slurm");
        assert!(f[0].detail.starts_with("Three launches"));
        assert_eq!(f[0].models, ["glm"]);
    }
}
