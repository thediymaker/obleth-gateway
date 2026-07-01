//! Pure extractive sentence selection for the neural prose compression pass.
//!
//! No I/O, no async, no state. These functions are called by the lossy
//! compression pass to reduce prose blocks before sending to an upstream model.

/// Split `text` into sentence substrings.
///
/// Breaks after `.`, `!`, or `?` when followed by whitespace, and also on hard
/// newlines. Each piece is trimmed; empty pieces are dropped. The sentence text
/// is preserved as-is otherwise (no other normalisation) so reassembly is
/// faithful.
#[allow(dead_code)] // called by the lossy compression pass in a later task
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
#[allow(dead_code)] // called by the lossy compression pass in a later task
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

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
}
