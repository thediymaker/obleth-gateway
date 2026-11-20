//! Neural prose compression: extractive sentence selection plus an HTTP client
//! for the optional compressor scoring sidecar.
//!
//! Pure functions (`split_sentences`, `select_kept`) are used by the lossy
//! compression pass. `CompressorClient` handles the sidecar POST and is held on
//! `AppState` so a single client is shared across requests.

// ── Sidecar client ────────────────────────────────────────────────────────────

/// HTTP client for the optional compressor scoring sidecar.
///
/// Construct with [`CompressorClient::from_env`] or [`parse_config`].
/// The caller supplies the shared [`reqwest::Client`] on each [`score`] call so
/// no internal client is held.
#[derive(Clone)]
pub struct CompressorClient {
    pub base_url: String,
    pub timeout: std::time::Duration,
    breaker: std::sync::Arc<Breaker>,
}

/// Largest `/score` answer read; a sidecar answering more is an error.
const SCORE_RESPONSE_MAX: usize = 1024 * 1024;
/// Consecutive timeouts after which the sidecar is left alone for
/// [`BREAKER_HOLD`]: a stalled sidecar must not add its timeout to every
/// request, and a short hold lets a recovering one back soon.
const BREAKER_AFTER: u32 = 3;
const BREAKER_HOLD: std::time::Duration = std::time::Duration::from_secs(30);
/// How often a failing sidecar is logged; the counter carries the rest.
const WARN_EVERY: std::time::Duration = std::time::Duration::from_secs(60);

/// Carries this gateway's timeout for one `/score` call, in milliseconds.
const TIMEOUT_HEADER: &str = "x-obleth-timeout-ms";

/// Sentences one sidecar scores per second: a 4-core pod on kompress-v2-base
/// (fp32) scored 30-40 a second of mixed-length prose in chunks of 8 (measured
/// 2026-11-20). Sizes [`CompressorClient::sentence_budget`].
const SCORED_SENTENCES_PER_SEC: u64 = 30;

/// The fewest sentences a call may carry, however short the timeout.
const MIN_SENTENCE_BUDGET: usize = 8;

/// How one `/score` call ended; the span and the
/// `obleth_compressor_scores_total` counter carry it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScoreOutcome {
    /// The sidecar scored every batch.
    Neural,
    /// The sidecar did not answer within its timeout.
    Timeout,
    /// The sidecar could not be reached, answered an error, or an unreadable body.
    Error,
    /// The sidecar turned the call away: its queue was full (503) or the
    /// timeout the gateway sent passed before it finished (504). Load
    /// shedding, not a fault.
    Busy,
    /// Not asked: the breaker is open after consecutive timeouts.
    Open,
}

impl ScoreOutcome {
    pub fn name(self) -> &'static str {
        match self {
            ScoreOutcome::Neural => "neural",
            ScoreOutcome::Timeout => "timeout",
            ScoreOutcome::Error => "error",
            ScoreOutcome::Busy => "busy",
            ScoreOutcome::Open => "breaker_open",
        }
    }
}

/// What a `/score` call came to: the scores when the sidecar answered, how
/// it ended, and how long it took.
pub struct Scored {
    pub scores: Option<Vec<Vec<f32>>>,
    pub outcome: ScoreOutcome,
    pub elapsed: std::time::Duration,
}

/// The sidecar's recent health, shared by every request on this replica.
#[derive(Default)]
struct Breaker {
    consecutive_timeouts: std::sync::atomic::AtomicU32,
    open_until: std::sync::Mutex<Option<std::time::Instant>>,
    last_warning: std::sync::Mutex<Option<std::time::Instant>>,
}

impl Breaker {
    fn is_open(&self, now: std::time::Instant) -> bool {
        self.open_until
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|until| now < until)
    }

    /// Note how a call ended; opens the breaker on the `BREAKER_AFTER`th
    /// consecutive timeout and closes it on any answer.
    fn note(&self, outcome: ScoreOutcome, now: std::time::Instant) {
        use std::sync::atomic::Ordering;
        match outcome {
            ScoreOutcome::Timeout => {
                let run = self.consecutive_timeouts.fetch_add(1, Ordering::Relaxed) + 1;
                if run >= BREAKER_AFTER {
                    *self.open_until.lock().unwrap_or_else(|e| e.into_inner()) =
                        Some(now + BREAKER_HOLD);
                    self.consecutive_timeouts.store(0, Ordering::Relaxed);
                }
            }
            ScoreOutcome::Neural | ScoreOutcome::Error | ScoreOutcome::Busy => {
                self.consecutive_timeouts.store(0, Ordering::Relaxed);
            }
            ScoreOutcome::Open => {}
        }
    }

    /// Whether a failure may be logged now: once per `WARN_EVERY`.
    fn may_warn(&self, now: std::time::Instant) -> bool {
        let mut last = self.last_warning.lock().unwrap_or_else(|e| e.into_inner());
        if last.is_some_and(|at| now.duration_since(at) < WARN_EVERY) {
            return false;
        }
        *last = Some(now);
        true
    }
}

/// Build a `CompressorClient` from explicit values.
///
/// Returns `Some` only when `url` is `Some` and non-empty after trimming.
/// Strips a single trailing `/` from the URL. `timeout_ms` is parsed as `u64`
/// milliseconds; unparseable or absent values default to 800 ms.
fn parse_config(url: Option<&str>, timeout_ms: Option<&str>) -> Option<CompressorClient> {
    let raw_url = url?;
    let trimmed = raw_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let base_url = trimmed.trim_end_matches('/').to_string();
    let timeout = timeout_ms
        .and_then(|s| s.parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or_else(|| std::time::Duration::from_millis(800));
    Some(CompressorClient {
        base_url,
        timeout,
        breaker: std::sync::Arc::default(),
    })
}

impl CompressorClient {
    /// Construct from environment variables.
    ///
    /// Reads `OBLETH_COMPRESSOR_URL` and `OBLETH_COMPRESSOR_TIMEOUT_MS`.
    /// Returns `None` when the URL variable is unset or empty.
    pub fn from_env() -> Option<CompressorClient> {
        parse_config(
            std::env::var("OBLETH_COMPRESSOR_URL").ok().as_deref(),
            std::env::var("OBLETH_COMPRESSOR_TIMEOUT_MS")
                .ok()
                .as_deref(),
        )
    }

    /// Most sentences one `/score` call may carry: what the sidecar can score
    /// within this client's timeout. More would time out (and be stopped by the
    /// sidecar), so a segment that would cross it compacts by the heuristic
    /// path instead.
    pub(super) fn sentence_budget(&self) -> usize {
        let ms = self.timeout.as_millis() as u64;
        ((ms * SCORED_SENTENCES_PER_SEC / 1000) as usize).max(MIN_SENTENCE_BUDGET)
    }

    /// POST `batches` to the sidecar's `/score` endpoint and return the
    /// per-sentence scores.
    ///
    /// Fails open: on a timeout, an unreachable sidecar, a non-2xx answer or an
    /// unreadable body the scores are `None` and the outcome says why, so the
    /// request goes on with the heuristic path. After `BREAKER_AFTER`
    /// consecutive timeouts the sidecar is not asked for `BREAKER_HOLD`.
    /// The caller supplies the shared `http` client.
    pub(super) async fn score(&self, http: &reqwest::Client, batches: &[Vec<String>]) -> Scored {
        let started = std::time::Instant::now();
        if self.breaker.is_open(started) {
            return Scored {
                scores: None,
                outcome: ScoreOutcome::Open,
                elapsed: started.elapsed(),
            };
        }
        let url = format!("{}/score", self.base_url);
        let body = build_score_body(batches);
        let answered = http
            .post(&url)
            .json(&body)
            // The sidecar stops work this call has given up on.
            .header(TIMEOUT_HEADER, self.timeout.as_millis().to_string())
            .timeout(self.timeout)
            .send()
            .await;
        let (scores, outcome, why) = match answered {
            Err(e) if e.is_timeout() => (
                None,
                ScoreOutcome::Timeout,
                "did not answer in time".to_owned(),
            ),
            Err(e) => (None, ScoreOutcome::Error, e.without_url().to_string()),
            Ok(resp)
                if matches!(
                    resp.status(),
                    reqwest::StatusCode::SERVICE_UNAVAILABLE | reqwest::StatusCode::GATEWAY_TIMEOUT
                ) =>
            {
                (
                    None,
                    ScoreOutcome::Busy,
                    format!("answered {}", resp.status()),
                )
            }
            Ok(resp) if !resp.status().is_success() => (
                None,
                ScoreOutcome::Error,
                format!("answered {}", resp.status()),
            ),
            Ok(resp) => match super::read_body_capped(resp, SCORE_RESPONSE_MAX).await {
                Ok(bytes) => match serde_json::from_slice::<serde_json::Value>(&bytes) {
                    Ok(json) => (
                        parse_score_response(&json, batches.len()),
                        ScoreOutcome::Neural,
                        String::new(),
                    ),
                    Err(e) => (
                        None,
                        ScoreOutcome::Error,
                        format!("answered something other than JSON: {e}"),
                    ),
                },
                Err(e) => (None, ScoreOutcome::Error, e.to_string()),
            },
        };
        let now = std::time::Instant::now();
        self.breaker.note(outcome, now);
        let fault = !matches!(outcome, ScoreOutcome::Neural | ScoreOutcome::Busy);
        if fault && self.breaker.may_warn(now) {
            tracing::warn!(
                outcome = outcome.name(),
                elapsed_ms = now.duration_since(started).as_millis() as u64,
                "compressor sidecar {why}; prose is compacted by the heuristic path meanwhile"
            );
        }
        Scored {
            scores,
            outcome,
            elapsed: now.duration_since(started),
        }
    }
}

/// Serialize `batches` into the sidecar request shape.
///
/// ```json
/// {"segments": [{"sentences": ["s1", "s2"]}, ...]}
/// ```
fn build_score_body(batches: &[Vec<String>]) -> serde_json::Value {
    let segments: Vec<serde_json::Value> = batches
        .iter()
        .map(|batch| serde_json::json!({"sentences": batch}))
        .collect();
    serde_json::json!({"segments": segments})
}

/// Parse the sidecar's response body.
///
/// Expects `{"results": [{"scores": [f32, ...]}, ...]}` with exactly
/// `expected_len` elements. Returns `None` on any structural mismatch.
fn parse_score_response(v: &serde_json::Value, expected_len: usize) -> Option<Vec<Vec<f32>>> {
    let results = v["results"].as_array()?;
    if results.len() != expected_len {
        return None;
    }
    results
        .iter()
        .map(|elem| {
            let scores_arr = elem["scores"].as_array()?;
            scores_arr
                .iter()
                .map(|n| n.as_f64().map(|f| f as f32))
                .collect::<Option<Vec<f32>>>()
        })
        .collect()
}

// ── Pure extractive helpers ───────────────────────────────────────────────────

/// Split `text` into sentence substrings.
///
/// Breaks after `.`, `!`, or `?` when followed by whitespace, and also on hard
/// newlines. Each piece is trimmed; empty pieces are dropped. The sentence text
/// is preserved as-is otherwise (no other normalisation) so reassembly is
/// faithful.
pub(super) fn split_sentences(text: &str) -> Vec<String> {
    let mut sentences: Vec<String> = Vec::new();
    let mut push = |piece: &str| {
        let piece = piece.trim();
        if !piece.is_empty() {
            sentences.push(piece.to_owned());
        }
    };
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, ch)) = chars.next() {
        if ch == '\n' {
            // A hard newline always ends the sentence, and is not part of it.
            push(&text[start..i]);
            start = i + 1;
        } else if matches!(ch, '.' | '!' | '?')
            && chars.peek().is_none_or(|(_, next)| next.is_whitespace())
        {
            // Punctuation ends it when whitespace (or the end) follows.
            push(&text[start..i + 1]);
            start = i + 1;
        }
    }
    // Trailing text that did not end with punctuation.
    push(&text[start..]);
    sentences
}

/// Select the sentences to keep for the extractive compression result.
///
/// `sentences` and `scores` must be index-aligned. Returns `None` when:
/// - their lengths differ,
/// - `sentences.len() < 4`, or
/// - the result would not be shorter than the original joined text.
///
/// Otherwise returns `Some(result)` where `result` is the kept sentences joined
/// by `" "` followed by `" [… N sentences omitted]"`.
pub(super) fn select_kept(
    sentences: &[String],
    scores: &[f32],
    keep_ratio: f32,
    query_terms: &std::collections::HashSet<String>,
) -> Option<String> {
    if sentences.len() != scores.len() {
        return None;
    }
    if sentences.len() < 4 {
        return None;
    }

    let n = sentences.len();
    let keep = (n as f32 * keep_ratio).ceil().max(1.0) as usize;

    // Build the set of top-`keep` indices by score (ties broken by lower index).
    // Sort indices by score descending, then by index ascending for ties.
    let mut indexed: Vec<(usize, f32)> = scores.iter().copied().enumerate().collect();
    indexed.sort_by(|a, b| {
        // Higher score first; on tie, lower index first.
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let mut kept_set: std::collections::HashSet<usize> =
        indexed[..keep.min(n)].iter().map(|(i, _)| *i).collect();

    // Force-keep any sentence that has a query term.
    for (idx, sent) in sentences.iter().enumerate() {
        if super::compression::mentions_any(sent, query_terms) {
            kept_set.insert(idx);
        }
    }

    // Restore original order.
    let mut kept_indices: Vec<usize> = kept_set.into_iter().collect();
    kept_indices.sort_unstable();

    let kept_count = kept_indices.len();
    let omitted = n.saturating_sub(kept_count);

    let kept_joined: String = kept_indices
        .iter()
        .map(|&i| sentences[i].as_str())
        .collect::<Vec<_>>()
        .join(" ");

    let original_joined = sentences.join(" ");

    // Only return a compressed result when the kept portion is strictly shorter
    // than the full original text (i.e., at least one sentence was omitted and
    // the kept slice is meaningfully smaller). This also returns None when
    // keep_ratio = 1.0 and every sentence is retained.
    if kept_joined.len() < original_joined.len() {
        Some(format!(
            "{kept_joined} [\u{2026} {omitted} sentences omitted]"
        ))
    } else {
        None
    }
}

/// Choose the prose compaction for one segment: prefer the neural extractive
/// selection when sidecar `scores` are available and productive, otherwise fall
/// back to the deterministic `extract_prose` heuristic. Pure; no I/O.
pub(super) fn compact_prose_segment(
    original: &str,
    neural_scores: Option<&[f32]>,
    keep_ratio: f32,
    query_terms: &std::collections::HashSet<String>,
) -> Option<String> {
    if let Some(scores) = neural_scores {
        let sentences = split_sentences(original);
        if let Some(out) = select_kept(&sentences, scores, keep_ratio, query_terms) {
            return Some(out);
        }
    }
    super::compression::extract_prose(original, query_terms, keep_ratio)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn the_breaker_opens_after_consecutive_timeouts_and_closes_on_an_answer() {
        let breaker = Breaker::default();
        let t0 = Instant::now();
        for _ in 0..BREAKER_AFTER - 1 {
            breaker.note(ScoreOutcome::Timeout, t0);
        }
        assert!(
            !breaker.is_open(t0),
            "one short of the threshold stays closed"
        );
        breaker.note(ScoreOutcome::Neural, t0);
        breaker.note(ScoreOutcome::Timeout, t0);
        assert!(!breaker.is_open(t0), "an answer resets the run");
        for _ in 0..BREAKER_AFTER {
            breaker.note(ScoreOutcome::Timeout, t0);
        }
        assert!(breaker.is_open(t0 + Duration::from_secs(1)));
        assert!(
            !breaker.is_open(t0 + BREAKER_HOLD + Duration::from_secs(1)),
            "the hold is short"
        );
        assert!(breaker.may_warn(t0));
        assert!(
            !breaker.may_warn(t0 + Duration::from_secs(1)),
            "one warning per window"
        );
        assert!(breaker.may_warn(t0 + WARN_EVERY + Duration::from_secs(1)));
    }

    /// A sidecar that accepts and never answers ends as a timeout within the
    /// client's own bound, not reqwest's; a run of them opens the breaker, and
    /// the next call is not sent at all.
    #[tokio::test]
    async fn a_silent_sidecar_times_out_and_then_trips_the_breaker() {
        use axum::{routing::post, Router};
        use std::sync::atomic::{AtomicUsize, Ordering};
        let hits = std::sync::Arc::new(AtomicUsize::new(0));
        let seen = hits.clone();
        let app = Router::new().route(
            "/score",
            post(move || {
                let seen = seen.clone();
                async move {
                    seen.fetch_add(1, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    "{}"
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = parse_config(Some(&url), Some("50")).unwrap();
        let http = reqwest::Client::new();
        let batches = vec![vec!["one".to_owned(), "two".to_owned()]];
        for _ in 0..BREAKER_AFTER {
            let scored = client.score(&http, &batches).await;
            assert_eq!(scored.outcome, ScoreOutcome::Timeout);
            assert!(scored.scores.is_none());
            assert!(
                scored.elapsed < Duration::from_secs(2),
                "{:?}",
                scored.elapsed
            );
        }
        let scored = client.score(&http, &batches).await;
        assert_eq!(scored.outcome, ScoreOutcome::Open);
        // Under load a 50 ms call can time out before the stub counts it, so
        // let any in flight land, then check the open breaker added none.
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            hits.load(Ordering::SeqCst) <= BREAKER_AFTER as usize,
            "the open breaker sends nothing"
        );
    }
    /// The call carries the gateway's timeout, and a sidecar that sheds it
    /// (503 when its queue is full, 504 when the timeout passed) reads as
    /// busy: no scores, no warning, and no step toward the breaker.
    #[tokio::test]
    async fn a_shedding_sidecar_is_busy_and_gets_the_timeout() {
        use axum::{http::HeaderMap, http::StatusCode, routing::post, Router};
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen = sent.clone();
        let app = Router::new().route(
            "/score",
            post(move |headers: HeaderMap| {
                let seen = seen.clone();
                async move {
                    let timeout = headers
                        .get(TIMEOUT_HEADER)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_owned();
                    let mut seen = seen.lock().unwrap();
                    seen.push(timeout);
                    if seen.len() % 2 == 1 {
                        StatusCode::SERVICE_UNAVAILABLE
                    } else {
                        StatusCode::GATEWAY_TIMEOUT
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let client = parse_config(Some(&url), Some("750")).unwrap();
        let http = reqwest::Client::new();
        let batches = vec![vec!["one".to_owned()]];
        for _ in 0..(BREAKER_AFTER + 1) {
            let scored = client.score(&http, &batches).await;
            assert_eq!(scored.outcome, ScoreOutcome::Busy);
            assert_eq!(scored.outcome.name(), "busy");
            assert!(scored.scores.is_none());
        }
        let sent = sent.lock().unwrap();
        assert_eq!(
            sent.len(),
            BREAKER_AFTER as usize + 1,
            "never short-circuited"
        );
        assert!(sent.iter().all(|t| t == "750"), "{sent:?}");
    }

    #[test]
    fn the_sentence_budget_follows_the_timeout() {
        let at = |ms: &str| {
            parse_config(Some("http://c"), Some(ms))
                .unwrap()
                .sentence_budget()
        };
        assert_eq!(at("800"), 24);
        assert_eq!(at("3000"), 90);
        assert_eq!(at("50"), MIN_SENTENCE_BUDGET);
    }

    use std::collections::HashSet;

    #[test]
    fn parse_config_none_without_url() {
        assert!(parse_config(None, None).is_none());
        assert!(parse_config(Some("   "), Some("1000")).is_none());
    }

    #[test]
    fn parse_config_trims_and_defaults_timeout() {
        let c = parse_config(Some("http://k:8080/"), None).unwrap();
        assert_eq!(c.base_url, "http://k:8080");
        assert_eq!(c.timeout, std::time::Duration::from_millis(800));
    }

    #[test]
    fn parse_config_reads_timeout_ms() {
        let c = parse_config(Some("http://k:8080"), Some("1200")).unwrap();
        assert_eq!(c.timeout, std::time::Duration::from_millis(1200));
    }

    #[test]
    fn build_score_body_shapes_segments() {
        let body = build_score_body(&[vec!["a".into(), "b".into()], vec!["c".into()]]);
        assert_eq!(body["segments"][0]["sentences"][1], "b");
        assert_eq!(body["segments"][1]["sentences"][0], "c");
    }

    #[test]
    fn parse_score_response_reads_aligned_scores() {
        let v = serde_json::json!({"results":[{"scores":[0.1,0.2]},{"scores":[0.9]}]});
        let got = parse_score_response(&v, 2).unwrap();
        assert_eq!(got, vec![vec![0.1_f32, 0.2], vec![0.9]]);
    }

    #[test]
    fn parse_score_response_none_on_count_mismatch() {
        let v = serde_json::json!({"results":[{"scores":[0.1]}]});
        assert!(parse_score_response(&v, 2).is_none());
    }

    fn hs(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn split_sentences_breaks_on_punctuation_and_newlines() {
        let s = split_sentences("First one. Second two! Third three?\nFourth four");
        assert_eq!(
            s,
            vec!["First one.", "Second two!", "Third three?", "Fourth four"]
        );
    }

    #[test]
    fn split_sentences_drops_empties_and_trims() {
        let s = split_sentences("  A.   \n\n  B.  ");
        assert_eq!(s, vec!["A.", "B."]);
    }

    #[test]
    fn select_kept_keeps_top_scored_in_original_order() {
        let sents: Vec<String> = ["one", "two", "three", "four", "five", "six"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let scores = [0.1_f32, 0.9, 0.2, 0.8, 0.05, 0.7];
        let out = select_kept(&sents, &scores, 0.5, &HashSet::new()).unwrap();
        assert_eq!(out, "two four six [… 3 sentences omitted]");
    }

    #[test]
    fn select_kept_force_keeps_query_matches() {
        let sents: Vec<String> = ["alpha topic", "beta", "gamma", "delta", "epsilon", "zeta"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let scores = [0.01_f32, 0.9, 0.8, 0.7, 0.6, 0.5];
        let out = select_kept(&sents, &scores, 0.5, &hs(&["topic"])).unwrap();
        assert!(out.starts_with("alpha topic beta gamma delta"));
        assert!(out.contains("omitted]"));
    }

    #[test]
    fn select_kept_none_when_lengths_mismatch() {
        let sents = vec!["a".to_string(), "b".to_string()];
        assert!(select_kept(&sents, &[0.5], 0.5, &HashSet::new()).is_none());
    }

    #[test]
    fn select_kept_none_when_too_few_sentences() {
        let sents: Vec<String> = ["a", "b", "c"].iter().map(|s| s.to_string()).collect();
        assert!(select_kept(&sents, &[0.1, 0.9, 0.5], 0.5, &HashSet::new()).is_none());
    }

    #[test]
    fn select_kept_none_when_not_shorter() {
        let sents: Vec<String> = (0..8).map(|i| format!("s{i}")).collect();
        let scores: Vec<f32> = (0..8).map(|i| i as f32).collect();
        assert!(select_kept(&sents, &scores, 1.0, &HashSet::new()).is_none());
    }

    #[test]
    fn compact_prose_segment_uses_neural_when_scores_help() {
        let text = "Alpha one. Beta two. Gamma three. Delta four. Epsilon five. Zeta six.";
        let sents = split_sentences(text);
        // High score to first three, low to the rest.
        let scores: Vec<f32> = sents
            .iter()
            .enumerate()
            .map(|(i, _)| if i < 3 { 0.9 } else { 0.1 })
            .collect();
        let out = compact_prose_segment(text, Some(&scores), 0.5, &HashSet::new()).unwrap();
        assert!(out.contains("sentences omitted]"));
        assert!(out.len() < text.len());
    }

    #[test]
    fn compact_prose_segment_falls_back_to_heuristic_without_scores() {
        // 12 distinct non-trivial lines so extract_prose (line-based, needs >=8 lines) fires.
        let text: String = (0..12)
            .map(|i| format!("This is a reasonably long note line number {i} with content.\n"))
            .collect();
        let out = compact_prose_segment(&text, None, 0.4, &HashSet::new());
        assert!(out.is_some(), "heuristic should compact 12 lines");
        assert!(out.unwrap().contains("lines omitted]"));
    }

    #[test]
    fn compact_prose_segment_falls_back_when_scores_mismatch() {
        let text: String = (0..12)
            .map(|i| format!("This is a reasonably long note line number {i} with content.\n"))
            .collect();
        // Wrong-length scores → select_kept returns None → heuristic path used.
        let out = compact_prose_segment(&text, Some(&[0.9, 0.1]), 0.4, &HashSet::new());
        assert!(out.is_some());
        assert!(out.unwrap().contains("lines omitted]"));
    }
}
