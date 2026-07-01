//! Neural prose compression: extractive sentence selection plus an HTTP client
//! for the optional kompress scoring sidecar.
//!
//! Pure functions (`split_sentences`, `select_kept`) are used by the lossy
//! compression pass. `KompressClient` handles the sidecar POST and is held on
//! `AppState` so a single client is shared across requests.

// ── Sidecar client ────────────────────────────────────────────────────────────

/// HTTP client for the optional kompress scoring sidecar.
///
/// Construct with [`KompressClient::from_env`] or [`parse_config`].
/// The caller supplies the shared [`reqwest::Client`] on each [`score`] call so
/// no internal client is held.
#[derive(Clone)]
pub struct KompressClient {
    pub base_url: String,
    pub timeout: std::time::Duration,
}

/// Build a `KompressClient` from explicit values.
///
/// Returns `Some` only when `url` is `Some` and non-empty after trimming.
/// Strips a single trailing `/` from the URL. `timeout_ms` is parsed as `u64`
/// milliseconds; unparseable or absent values default to 800 ms.
fn parse_config(url: Option<&str>, timeout_ms: Option<&str>) -> Option<KompressClient> {
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
    Some(KompressClient { base_url, timeout })
}

impl KompressClient {
    /// Construct from environment variables.
    ///
    /// Reads `OBLETH_KOMPRESS_URL` and `OBLETH_KOMPRESS_TIMEOUT_MS`.
    /// Returns `None` when the URL variable is unset or empty.
    pub fn from_env() -> Option<KompressClient> {
        parse_config(
            std::env::var("OBLETH_KOMPRESS_URL").ok().as_deref(),
            std::env::var("OBLETH_KOMPRESS_TIMEOUT_MS").ok().as_deref(),
        )
    }

    /// POST `batches` to the sidecar's `/score` endpoint and return the
    /// per-sentence scores.
    ///
    /// Fails open on any error (send failure, non-2xx status, parse error) by
    /// returning `None`.  The caller supplies the shared `http` client.
    pub(super) async fn score(
        &self,
        http: &reqwest::Client,
        batches: &[Vec<String>],
    ) -> Option<Vec<Vec<f32>>> {
        let url = format!("{}/score", self.base_url);
        let body = build_score_body(batches);
        let resp = http
            .post(&url)
            .json(&body)
            .timeout(self.timeout)
            .send()
            .await
            .ok()?;
        if !resp.status().is_success() {
            return None;
        }
        let json: serde_json::Value = resp.json().await.ok()?;
        parse_score_response(&json, batches.len())
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
    let mut current = String::new();

    let chars: Vec<char> = text.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];

        if ch == '\n' {
            // Hard newline always ends the current sentence.
            let trimmed = current.trim().to_string();
            if !trimmed.is_empty() {
                sentences.push(trimmed);
            }
            current.clear();
            i += 1;
            continue;
        }

        current.push(ch);

        if matches!(ch, '.' | '!' | '?') {
            // Break when the next non-consumed character is whitespace (or end).
            if i + 1 >= len || chars[i + 1].is_whitespace() {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    sentences.push(trimmed);
                }
                current.clear();
            }
        }

        i += 1;
    }

    // Flush any trailing text that did not end with punctuation.
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        sentences.push(trimmed);
    }

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

    // Force-keep any sentence whose lowercased text contains a query term.
    for (idx, sent) in sentences.iter().enumerate() {
        let lower = sent.to_lowercase();
        if query_terms.iter().any(|term| lower.contains(term.as_str())) {
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
