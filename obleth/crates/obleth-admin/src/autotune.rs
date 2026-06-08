//! Capacity auto-tuning: an active ramp probe that finds a model's real
//! in-flight knee.
//!
//! The probe drives closed-loop load **directly at the upstream**
//! (`model.api_base`), deliberately bypassing obleth's own admission so we
//! measure the backend's true capacity rather than the gateway's current cap.
//! It steps concurrency up geometrically (1, 2, 4, 8, …), measuring sustained
//! throughput and tail latency at each level, and **keeps ramping until it
//! finds the knee** — the level past which adding more concurrency no longer
//! buys throughput, or where tail latency degrades past the operator's
//! tolerance. The highest concurrency that still cleared both bars becomes the
//! recommended `max_in_flight`.
//!
//! ## Why latency is measured *relative to a baseline*
//!
//! A single request to a large model can already take seconds — that's the
//! model's inherent per-request latency, and it has nothing to do with how
//! many requests it can serve at once. So the probe first measures the
//! single-request p99 at concurrency 1 (the **baseline**), then ramps and
//! flags the knee when p99 climbs past `baseline × headroom` (e.g. 4×). The
//! operator picks how much slowdown they'll tolerate under load, not an
//! absolute millisecond number they'd have to guess. This avoids the trap
//! where an absolute target below the model's own per-request latency collapses
//! the recommendation to 1 slot.
//!
//! The shape of each probe request is set by the [`WorkloadProfile`]: a `chat`
//! turn is a small prompt with a short reply, while a `coding` turn carries a
//! large context and a longer reply. Latency (and therefore the knee) depends
//! heavily on this shape, so tuning against a representative workload matters —
//! a trivial `ping` would over-estimate capacity for a real coding workload.
//!
//! It is **recommend-only**: this module never writes config. The caller
//! decides whether to apply the suggestion (see `apply_tuned_model_capacity`
//! in the store). Local/self-hosted models are the intended target — cloud
//! models should stay on a `static` cap to bound spend.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use obleth_config::ModelRoute;
use serde::{Deserialize, Serialize};
use tokio::task::JoinSet;
use utoipa::ToSchema;

/// How much slower than the single-request baseline p99 a step may run before
/// it counts as the latency knee, when the caller does not supply a value.
const DEFAULT_LATENCY_HEADROOM: f64 = 4.0;
/// Bounds on the headroom multiplier the caller may request.
const MIN_LATENCY_HEADROOM: f64 = 1.5;
const MAX_LATENCY_HEADROOM: f64 = 20.0;
/// Absolute floor added to the latency ceiling so a tiny baseline (e.g. 10 ms)
/// isn't tripped by ordinary jitter on the next step.
const LATENCY_CEILING_FLOOR_MS: u64 = 100;
/// Absolute safety ceiling on probe concurrency, regardless of replica count.
/// The ramp never climbs past this even if the operator says they have many
/// replicas — it bounds spend and protects the backend.
const HARD_MAX_CONCURRENCY: usize = 512;
/// How many concurrent requests to probe per replica when deriving the ramp
/// ceiling from a replica count. LLM servers batch many requests per replica,
/// so this is generous headroom above the likely knee, not a target.
const PER_REPLICA_CONCURRENCY: usize = 32;
/// Ramp ceiling used when the caller gives neither a replica count nor an
/// explicit override.
const DEFAULT_MAX_CONCURRENCY: usize = 64;
/// How long each concurrency step sustains load before we read its numbers.
const STEP_DURATION: Duration = Duration::from_millis(2_500);
/// Total wall-clock budget for the whole probe (all steps combined).
const TOTAL_DEADLINE: Duration = Duration::from_secs(60);
/// Cap on total requests issued across the whole probe (spend/abuse guard).
const MAX_TOTAL_REQUESTS: usize = 20_000;
/// Minimum relative throughput gain a step must add over the previous one to be
/// considered "still climbing". Below this we treat the curve as plateaued.
const PLATEAU_GAIN: f64 = 0.07;

/// Request body for `POST /api/v1/models/:id/autotune`.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct AutotuneRequest {
    /// Shape of the probe request — what kind of traffic to tune for. Latency
    /// depends heavily on context size, so this should match real usage.
    /// Defaults to [`WorkloadProfile::Chat`].
    pub workload: Option<WorkloadProfile>,
    /// How much slower than a single request the model may get under load
    /// before the ramp calls it the knee, as a multiple of the concurrency-1
    /// p99 baseline (e.g. `4.0` = tolerate 4× the single-request latency).
    /// Clamped to `[MIN_LATENCY_HEADROOM, MAX_LATENCY_HEADROOM]`. Defaults to
    /// [`DEFAULT_LATENCY_HEADROOM`].
    pub latency_headroom: Option<f64>,
    /// How many replicas of the model are running upstream. The ramp ceiling is
    /// sized from this (`replicas × [`PER_REPLICA_CONCURRENCY`]`) so a 1–2
    /// replica backend isn't pounded all the way to 512. Ignored when
    /// `max_concurrency` is set explicitly. When neither is given, the ceiling
    /// defaults to [`DEFAULT_MAX_CONCURRENCY`].
    pub replicas: Option<usize>,
    /// Explicit override for the highest concurrency the ramp climbs to. Takes
    /// precedence over `replicas`. The probe runs the **full** geometric ladder
    /// up to the ceiling every run (1, 2, 4, … up to the ceiling) so the curve
    /// is reproducible, then recommends the knee from the complete curve.
    /// Clamped to [`HARD_MAX_CONCURRENCY`].
    pub max_concurrency: Option<usize>,
}

/// The shape of traffic the probe sends, which sets the prompt context size and
/// reply length. Latency \u2014 and therefore the in-flight knee \u2014 is very different
/// for a short chat turn versus a large-context coding turn, so the operator
/// tunes against the workload that matches real usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadProfile {
    /// Short interactive turn: small prompt, short reply.
    Chat,
    /// Long-context coding turn: large prompt, longer reply.
    Coding,
}

impl WorkloadProfile {
    /// Approximate `(prompt_tokens, reply_tokens)` the probe request targets.
    fn shape(self) -> (usize, i64) {
        match self {
            WorkloadProfile::Chat => (512, 128),
            WorkloadProfile::Coding => (6_000, 512),
        }
    }
}

/// Why the ramp stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum KneeReason {
    /// A step's p99 climbed past the latency ceiling (`baseline × headroom`).
    LatencyDegraded,
    /// Throughput stopped climbing meaningfully (saturation).
    Plateau,
    /// Reached `max_concurrency` without finding a knee — the real knee may be
    /// higher; consider raising the ceiling.
    MaxConcurrency,
    /// Every step failed to produce usable samples (upstream unreachable).
    NoData,
}

/// One concurrency level's measurements.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutotuneStep {
    pub concurrency: usize,
    /// Sustained throughput at this level (successful requests per second).
    pub throughput_rps: f64,
    /// p99 of total request time at this level (ms).
    pub p99_ms: u64,
    /// p50 of total request time at this level (ms).
    pub p50_ms: u64,
    pub requests: usize,
    pub errors: usize,
}

/// Result of an auto-tune probe. Recommend-only — nothing is written.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct AutotuneReport {
    pub model_id: String,
    pub model_name: String,
    pub modality: String,
    /// Workload shape the probe ran against.
    pub workload: WorkloadProfile,
    /// Suggested `max_in_flight`. The operator applies this explicitly.
    pub recommended_max_in_flight: usize,
    pub knee_reason: KneeReason,
    /// Single-request p99 measured at concurrency 1 (ms). The latency ceiling
    /// is derived from this; `0` if concurrency 1 produced no usable samples.
    pub baseline_p99_ms: u64,
    /// The p99 ceiling the ramp held to: `baseline_p99_ms × latency_headroom`
    /// (with a small absolute floor). `0` if no baseline was measured.
    pub latency_ceiling_ms: u64,
    /// The headroom multiplier actually applied.
    pub latency_headroom: f64,
    pub max_concurrency: usize,
    /// Throughput at the recommended level (rps), for the UI summary.
    pub recommended_throughput_rps: f64,
    pub steps: Vec<AutotuneStep>,
    pub duration_ms: u64,
}

/// Errors specific to running a probe.
#[derive(Debug)]
pub enum AutotuneError {
    /// The model's modality isn't probeable (only `chat` and `embedding` are).
    UnsupportedModality(String),
}

impl std::fmt::Display for AutotuneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AutotuneError::UnsupportedModality(m) => write!(
                f,
                "auto-tune supports `chat` and `embedding` models only (got `{m}`)"
            ),
        }
    }
}

impl std::error::Error for AutotuneError {}

/// The two modalities the probe knows how to drive.
#[derive(Clone, Copy)]
enum Modality {
    Chat,
    Embedding,
}

fn modality_for(model_type: &str) -> Result<Modality, AutotuneError> {
    match model_type {
        "chat" => Ok(Modality::Chat),
        "embedding" => Ok(Modality::Embedding),
        other => Err(AutotuneError::UnsupportedModality(other.to_string())),
    }
}

fn probe_url(api_base: &str, modality: Modality) -> String {
    let base = api_base.trim_end_matches('/');
    let path = match modality {
        Modality::Chat => "chat/completions",
        Modality::Embedding => "embeddings",
    };
    if base.ends_with(path) {
        base.to_string()
    } else {
        format!("{base}/{path}")
    }
}

/// Build a chunk of filler text of roughly `approx_tokens` tokens, so a probe
/// request carries a realistic prompt size. English text averages ~0.75 words
/// per token, so we emit that many words from a small repeating vocabulary.
fn filler_text(approx_tokens: usize) -> String {
    const WORDS: [&str; 16] = [
        "the", "quick", "brown", "fox", "jumps", "over", "lazy", "dog", "lorem", "ipsum", "dolor",
        "sit", "amet", "consectetur", "adipiscing", "elit",
    ];
    let n_words = ((approx_tokens as f64) * 0.75).ceil() as usize;
    let mut s = String::with_capacity(n_words * 7);
    for i in 0..n_words.max(1) {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(WORDS[i % WORDS.len()]);
    }
    s
}

fn probe_body(
    upstream_model: &str,
    modality: Modality,
    reply_tokens: i64,
    prompt: &str,
) -> serde_json::Value {
    match modality {
        Modality::Chat => serde_json::json!({
            "model": upstream_model,
            "messages": [{ "role": "user", "content": prompt }],
            "max_tokens": reply_tokens,
            // Disable backend prompt/response caching so every request actually
            // exercises the model rather than replaying a cached answer.
            "temperature": 1.0,
            "stream": false,
        }),
        Modality::Embedding => serde_json::json!({
            "model": upstream_model,
            "input": prompt,
        }),
    }
}

/// Monotonic counter used to make every probe request unique (cache-busting).
static PROBE_NONCE: AtomicU64 = AtomicU64::new(0);

/// Build a unique prompt for one probe request by prefixing a fresh nonce to
/// the workload's base prompt. The nonce goes at the **front** so it also
/// defeats prefix/KV caches on the backend (which key on a shared leading
/// substring), not just exact-match response caches.
fn unique_prompt(salt: u64, base: &str) -> String {
    let n = PROBE_NONCE.fetch_add(1, Ordering::Relaxed);
    format!("[probe {salt:08x}-{n:08x}] {base}")
}

/// The geometric concurrency ladder, capped at `max_concurrency`.
fn concurrency_ladder(max_concurrency: usize) -> Vec<usize> {
    let mut ladder = Vec::new();
    let mut c = 1usize;
    while c < max_concurrency {
        ladder.push(c);
        c *= 2;
    }
    ladder.push(max_concurrency);
    ladder
}

/// p-quantile (0..=1) of a sorted slice of millisecond samples.
fn percentile(sorted: &[u64], q: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = (q * (sorted.len() as f64 - 1.0)).round() as usize;
    sorted[rank.min(sorted.len() - 1)]
}

/// Pick the recommended concurrency from the **whole** measured curve. We never
/// stop the ramp early, so `steps` always covers the full ladder; choosing from
/// the complete curve (rather than the first level that happened to wobble)
/// makes the recommendation reproducible across runs.
///
/// Algorithm: keep the levels that produced usable samples and stayed under the
/// latency ceiling, take their peak sustained throughput, then recommend the
/// *smallest* concurrency that reaches the plateau (within [`PLATEAU_GAIN`] of
/// the peak). That is the knee — the elbow of the throughput curve — which is
/// stable because it is a threshold crossing, not a noisy arg-max, and it
/// avoids over-provisioning when throughput flattens.
fn recommend(steps: &[AutotuneStep], latency_ceiling_ms: u64) -> (usize, KneeReason, f64) {
    let usable: Vec<&AutotuneStep> = steps.iter().filter(|s| s.requests > s.errors).collect();
    if usable.is_empty() {
        return (1, KneeReason::NoData, 0.0);
    }

    // Levels that also held the latency ceiling, in ladder order.
    let allowed: Vec<&AutotuneStep> = usable
        .iter()
        .copied()
        .filter(|s| s.p99_ms <= latency_ceiling_ms)
        .collect();
    if allowed.is_empty() {
        // Even a single request degraded past the ceiling — recommend the
        // lowest level and flag latency as the limit.
        let first = usable[0];
        return (first.concurrency, KneeReason::LatencyDegraded, first.throughput_rps);
    }

    let peak = allowed
        .iter()
        .map(|s| s.throughput_rps)
        .fold(0.0_f64, f64::max);
    let threshold = peak * (1.0 - PLATEAU_GAIN);
    // Smallest concurrency that reaches the plateau.
    let knee = allowed
        .iter()
        .copied()
        .find(|s| s.throughput_rps >= threshold)
        .unwrap_or_else(|| allowed[allowed.len() - 1]);

    // Explain the knee by what happened at higher concurrency levels.
    let higher_breached = usable
        .iter()
        .any(|s| s.concurrency > knee.concurrency && s.p99_ms > latency_ceiling_ms);
    let has_higher = usable.iter().any(|s| s.concurrency > knee.concurrency);
    let reason = if higher_breached {
        KneeReason::LatencyDegraded
    } else if has_higher {
        KneeReason::Plateau
    } else {
        KneeReason::MaxConcurrency
    };

    (knee.concurrency, reason, knee.throughput_rps)
}

/// Run one concurrency level for [`STEP_DURATION`] (or until `deadline`),
/// returning its measurements. Closed-loop: `concurrency` workers each loop
/// firing requests back-to-back and recording total latency per request. Every
/// request carries a freshly randomized prompt so backend caches can't inflate
/// the numbers.
#[allow(clippy::too_many_arguments)]
async fn run_step(
    http: reqwest::Client,
    url: String,
    upstream_model: String,
    modality: Modality,
    reply_tokens: i64,
    base_prompt: Arc<str>,
    salt: u64,
    api_key: Option<String>,
    concurrency: usize,
    deadline: Instant,
    budget: Arc<AtomicUsize>,
) -> AutotuneStep {
    let step_end = (Instant::now() + STEP_DURATION).min(deadline);
    let started = Instant::now();
    let mut set: JoinSet<(Vec<u64>, usize)> = JoinSet::new();

    for _ in 0..concurrency {
        let http = http.clone();
        let url = url.clone();
        let upstream_model = upstream_model.clone();
        let base_prompt = base_prompt.clone();
        let api_key = api_key.clone();
        let budget = budget.clone();
        set.spawn(async move {
            let mut latencies: Vec<u64> = Vec::new();
            let mut errors = 0usize;
            while Instant::now() < step_end {
                // Global request-count guard, shared across all workers/steps.
                if budget.fetch_sub(1, Ordering::Relaxed) == 0 {
                    budget.fetch_add(1, Ordering::Relaxed);
                    break;
                }
                // Fresh body per request so the backend can't serve a cached
                // response and make this level look faster than it is.
                let prompt = unique_prompt(salt, &base_prompt);
                let body = probe_body(&upstream_model, modality, reply_tokens, &prompt);
                let mut req = http.post(&url).json(&body);
                if let Some(key) = &api_key {
                    req = req.bearer_auth(key);
                }
                let t0 = Instant::now();
                match req.send().await {
                    Ok(res) => {
                        let ok = res.status().is_success();
                        // Drain the body so the connection can be reused.
                        let _ = res.bytes().await;
                        if ok {
                            latencies.push(t0.elapsed().as_millis() as u64);
                        } else {
                            errors += 1;
                        }
                    }
                    Err(_) => errors += 1,
                }
            }
            (latencies, errors)
        });
    }

    let mut all_latencies: Vec<u64> = Vec::new();
    let mut errors = 0usize;
    while let Some(joined) = set.join_next().await {
        if let Ok((latencies, errs)) = joined {
            all_latencies.extend(latencies);
            errors += errs;
        }
    }

    let elapsed = started.elapsed().as_secs_f64().max(1e-3);
    let requests = all_latencies.len() + errors;
    all_latencies.sort_unstable();
    let throughput_rps = all_latencies.len() as f64 / elapsed;

    AutotuneStep {
        concurrency,
        throughput_rps,
        p99_ms: percentile(&all_latencies, 0.99),
        p50_ms: percentile(&all_latencies, 0.50),
        requests,
        errors,
    }
}

/// Run a full auto-tune probe against `model`'s upstream and return a
/// recommendation. Never writes config.
pub async fn run_probe(
    http: &reqwest::Client,
    model: &ModelRoute,
    request: &AutotuneRequest,
) -> Result<AutotuneReport, AutotuneError> {
    let modality = modality_for(&model.model_type)?;
    let workload = request.workload.unwrap_or(WorkloadProfile::Chat);
    let headroom = request
        .latency_headroom
        .unwrap_or(DEFAULT_LATENCY_HEADROOM)
        .clamp(MIN_LATENCY_HEADROOM, MAX_LATENCY_HEADROOM);
    // The probe runs the full ladder up to this ceiling every time. Size it
    // from the explicit override if given, else from the replica count, else
    // a conservative default — so small backends aren't ramped to 512.
    let ceiling = request
        .max_concurrency
        .or_else(|| {
            request
                .replicas
                .map(|r| r.max(1).saturating_mul(PER_REPLICA_CONCURRENCY))
        })
        .unwrap_or(DEFAULT_MAX_CONCURRENCY)
        .clamp(1, HARD_MAX_CONCURRENCY);

    let url = probe_url(&model.api_base, modality);
    let (prompt_tokens, reply_tokens) = workload.shape();
    let base_prompt: Arc<str> = Arc::from(filler_text(prompt_tokens).as_str());
    let budget = Arc::new(AtomicUsize::new(MAX_TOTAL_REQUESTS));
    let overall_start = Instant::now();
    let deadline = overall_start + TOTAL_DEADLINE;
    // Per-run salt so requests can't collide with a previous run's cache.
    let salt = (overall_start.elapsed().as_nanos() as u64)
        ^ (std::process::id() as u64).rotate_left(32)
        ^ PROBE_NONCE.load(Ordering::Relaxed);

    // Run the complete geometric ladder. We deliberately do not stop early on
    // plateau or latency: probing the whole curve every time is what makes the
    // recommendation reproducible. Only hard safety limits cut it short.
    let mut steps: Vec<AutotuneStep> = Vec::new();
    for concurrency in concurrency_ladder(ceiling) {
        if Instant::now() >= deadline || budget.load(Ordering::Relaxed) == 0 {
            break;
        }
        let step = run_step(
            http.clone(),
            url.clone(),
            model.upstream_model.clone(),
            modality,
            reply_tokens,
            base_prompt.clone(),
            salt,
            model.api_key.clone(),
            concurrency,
            deadline,
            budget.clone(),
        )
        .await;

        let dead = step.requests > 0 && step.errors == step.requests;
        steps.push(step);
        if dead {
            break;
        }
    }

    // Baseline = single-request p99 (concurrency 1). The latency ceiling is
    // relative to it, so a slow-but-healthy model isn't penalised for its
    // inherent per-request latency.
    let baseline_p99_ms = steps
        .iter()
        .find(|s| s.concurrency == 1 && s.requests > s.errors)
        .map(|s| s.p99_ms)
        .unwrap_or(0);
    let latency_ceiling_ms = if baseline_p99_ms == 0 {
        // No usable baseline: don't gate on latency, rely on the throughput
        // knee alone.
        u64::MAX
    } else {
        ((baseline_p99_ms as f64 * headroom).round() as u64)
            .max(baseline_p99_ms + LATENCY_CEILING_FLOOR_MS)
    };

    let (recommended, knee_reason, recommended_rps) = recommend(&steps, latency_ceiling_ms);

    Ok(AutotuneReport {
        model_id: model.id.to_string(),
        model_name: model.model_name.clone(),
        modality: model.model_type.clone(),
        workload,
        recommended_max_in_flight: recommended,
        knee_reason,
        baseline_p99_ms,
        latency_ceiling_ms: if latency_ceiling_ms == u64::MAX {
            0
        } else {
            latency_ceiling_ms
        },
        latency_headroom: headroom,
        max_concurrency: ceiling,
        recommended_throughput_rps: recommended_rps,
        steps,
        duration_ms: overall_start.elapsed().as_millis() as u64,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(concurrency: usize, rps: f64, p99: u64) -> AutotuneStep {
        AutotuneStep {
            concurrency,
            throughput_rps: rps,
            p99_ms: p99,
            p50_ms: p99 / 2,
            requests: 100,
            errors: 0,
        }
    }

    #[test]
    fn ladder_is_geometric_and_capped() {
        assert_eq!(concurrency_ladder(16), vec![1, 2, 4, 8, 16]);
        assert_eq!(concurrency_ladder(10), vec![1, 2, 4, 8, 10]);
        assert_eq!(concurrency_ladder(1), vec![1]);
    }

    #[test]
    fn percentile_basic() {
        let s = vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10];
        assert_eq!(percentile(&s, 0.5), 6);
        assert_eq!(percentile(&s, 0.99), 10);
        assert_eq!(percentile(&[], 0.5), 0);
    }

    #[test]
    fn recommends_knee_on_latency_degraded() {
        // Throughput keeps climbing, but p99 blows past the ceiling at c=16.
        let steps = vec![
            step(1, 10.0, 100),
            step(2, 19.0, 150),
            step(4, 36.0, 220),
            step(8, 60.0, 400),
            step(16, 75.0, 1200), // breaches ceiling=1000
        ];
        let (rec, reason, _) = recommend(&steps, 1000);
        assert_eq!(rec, 8);
        assert_eq!(reason, KneeReason::LatencyDegraded);
    }

    #[test]
    fn recommends_knee_on_plateau() {
        // p99 stays fine but throughput flattens after c=8.
        let steps = vec![
            step(1, 10.0, 100),
            step(2, 20.0, 120),
            step(4, 38.0, 150),
            step(8, 60.0, 200),
            step(16, 61.0, 250), // +1.6% only -> plateau
        ];
        let (rec, reason, rps) = recommend(&steps, 5000);
        assert_eq!(rec, 8);
        assert_eq!(reason, KneeReason::Plateau);
        assert_eq!(rps, 60.0);
    }

    #[test]
    fn recommends_max_when_still_climbing() {
        let steps = vec![
            step(1, 10.0, 100),
            step(2, 20.0, 120),
            step(4, 40.0, 150),
        ];
        let (rec, reason, _) = recommend(&steps, 5000);
        assert_eq!(rec, 4);
        assert_eq!(reason, KneeReason::MaxConcurrency);
    }

    #[test]
    fn recommends_elbow_not_peak_when_throughput_plateaus_late() {
        // Throughput keeps creeping up in tiny increments after c=8; the knee
        // is the elbow (c=8), not the noisy arg-max at the top.
        let steps = vec![
            step(1, 10.0, 100),
            step(2, 20.0, 120),
            step(4, 40.0, 150),
            step(8, 78.0, 200),
            step(16, 80.0, 260),
            step(32, 81.0, 320),
        ];
        let (rec, reason, _) = recommend(&steps, 5000);
        assert_eq!(rec, 8);
        assert_eq!(reason, KneeReason::Plateau);
    }

    #[test]
    fn recommendation_is_stable_regardless_of_trailing_noise() {
        // The same curve with extra wobbling tail levels still recommends the
        // same knee — reproducibility is the whole point of the full ramp.
        let base = vec![
            step(1, 10.0, 100),
            step(2, 20.0, 120),
            step(4, 40.0, 150),
            step(8, 78.0, 200),
        ];
        let mut noisy = base.clone();
        noisy.push(step(16, 76.0, 260)); // dips
        noisy.push(step(32, 79.0, 340)); // recovers slightly
        let (a, _, _) = recommend(&base, 5000);
        let (b, _, _) = recommend(&noisy, 5000);
        assert_eq!(a, 8);
        assert_eq!(b, 8);
    }

    #[test]
    fn no_usable_data_recommends_one() {
        let mut s = step(4, 0.0, 0);
        s.requests = 50;
        s.errors = 50;
        let (rec, reason, _) = recommend(&[s], 1000);
        assert_eq!(rec, 1);
        assert_eq!(reason, KneeReason::NoData);
    }

    #[test]
    fn unsupported_modality_rejected() {
        assert!(modality_for("image").is_err());
        assert!(modality_for("chat").is_ok());
        assert!(modality_for("embedding").is_ok());
    }
}
