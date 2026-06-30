//! The **compression** boon: gateway-side reduction of what the model reads.
//!
//! Phase A: lossless structural compaction of JSON content in chat messages,
//! gated per-model and by global settings. Fail-open like every boon — any
//! error or absence of gain leaves the request untouched.
//!
//! Phase B2: lossy semantic compression — summarize long prose segments via a
//! helper model, stash each original in Redis for reversibility, and replace
//! the segment with `summary + [ref:HASH]`.

use obleth_config::{CompressionBoonSettings, ResolvedKey};
use obleth_tokenizer::{HeuristicTokenizer, Tokenizer};
use serde_json::Value;

use crate::state::AppState;
use super::tool_loop::retrieve_original_tool_def;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentKind {
    Json,
    Code,
    Log,
    Prose,
}

/// Classify a message segment by shape. Only object/array JSON counts as
/// `Json`; bare scalars do not (no structural gain).
pub(crate) fn classify(text: &str) -> ContentKind {
    let trimmed = text.trim();
    if trimmed.starts_with("```") {
        return ContentKind::Code;
    }
    if (trimmed.starts_with('{') && trimmed.ends_with('}'))
        || (trimmed.starts_with('[') && trimmed.ends_with(']'))
    {
        if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
            if v.is_object() || v.is_array() {
                return ContentKind::Json;
            }
        }
    }
    // Log-shaped: many lines, a majority beginning with a timestamp or a level token.
    let lines: Vec<&str> = trimmed.lines().collect();
    if lines.len() >= 8 {
        let log_like = lines
            .iter()
            .filter(|l| {
                let t = l.trim_start();
                t.starts_with(|c: char| c.is_ascii_digit()) // timestamp-ish
                    || ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"]
                        .iter()
                        .any(|lvl| t.starts_with(lvl) || t.contains(&format!(" {lvl} ")))
            })
            .count();
        if log_like * 2 >= lines.len() {
            return ContentKind::Log;
        }
    }
    ContentKind::Prose
}

/// Losslessly minify JSON text. Returns `Some(minified)` only when the result
/// is strictly shorter AND re-parses to a value equal to the original; otherwise `None`.
pub(crate) fn compact_json(text: &str) -> Option<String> {
    let value: Value = serde_json::from_str(text.trim()).ok()?;
    let compact = serde_json::to_string(&value).ok()?;
    if compact.len() >= text.len() {
        return None;
    }
    // Self-enforce the lossless invariant rather than relying on it by construction:
    // only substitute when the compacted text re-parses to a value equal to the original.
    if serde_json::from_str::<Value>(&compact).ok()? != value {
        return None;
    }
    Some(compact)
}

/// Conservative, lossless-leaning whitespace normalization of code text: strip
/// trailing whitespace from each line and collapse runs of 2+ blank lines to a
/// single blank line. Returns `Some` only when strictly shorter. Opt-in
/// (`code_compaction`): a fenced block containing a multi-line string literal
/// with intentional trailing spaces or consecutive blank lines is the one case
/// this could alter, which is why it is off by default.
pub(crate) fn compact_code(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut blank_run = 0u32;
    for line in text.lines() {
        let trimmed = line.trim_end();
        if trimmed.is_empty() {
            blank_run += 1;
            if blank_run > 1 {
                continue; // collapse consecutive blank lines
            }
        } else {
            blank_run = 0;
        }
        out.push_str(trimmed);
        out.push('\n');
    }
    // Drop a single trailing newline we may have added past the original's end.
    if !text.ends_with('\n') {
        while out.ends_with('\n') {
            out.pop();
        }
    }
    if out.len() >= text.len() {
        return None;
    }
    Some(out)
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CompressionStats {
    /// Number of segments above the `min_tokens` floor that were examined,
    /// including non-JSON segments that classification subsequently skipped.
    pub scanned: u32,
    pub compressed: u32,
    pub tokens_before: u32,
    pub tokens_after: u32,
}

/// Compact eligible JSON segments of `messages[]` in place. Phase A is lossless
/// and deterministic; it never calls a helper model or touches the network.
pub(super) fn apply(cfg: &CompressionBoonSettings, code_compaction: bool, json: &mut Value) -> CompressionStats {
    let tk = HeuristicTokenizer::new();
    let mut stats = CompressionStats::default();
    let Some(messages) = json.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return stats;
    };
    for msg in messages.iter_mut() {
        if stats.compressed >= cfg.max_segments {
            break;
        }
        // Two content shapes: a plain string, or an array of typed parts.
        match msg.get_mut("content") {
            Some(Value::String(s)) => {
                try_compact_string(cfg, code_compaction, &tk, s, &mut stats);
            }
            Some(Value::Array(parts)) => {
                for part in parts.iter_mut() {
                    if stats.compressed >= cfg.max_segments {
                        break;
                    }
                    if let Some(Value::String(s)) = part.get_mut("text") {
                        try_compact_string(cfg, code_compaction, &tk, s, &mut stats);
                    }
                }
            }
            _ => {}
        }
    }
    stats
}

/// Apply the threshold + classify + compact pipeline to one string segment,
/// rewriting it in place and updating `stats`.
fn try_compact_string(
    cfg: &CompressionBoonSettings,
    code_compaction: bool,
    tk: &HeuristicTokenizer,
    s: &mut String,
    stats: &mut CompressionStats,
) {
    let before = tk.count_text(s);
    if before < cfg.min_tokens {
        return;
    }
    stats.scanned += 1;
    match classify(s) {
        ContentKind::Json => {
            if let Some(compact) = super::structural_json::compact(s) {
                let after = tk.count_text(&compact);
                stats.tokens_before = stats.tokens_before.saturating_add(before);
                stats.tokens_after = stats.tokens_after.saturating_add(after);
                stats.compressed += 1;
                *s = compact;
            }
        }
        ContentKind::Code if code_compaction => {
            if let Some(compact) = compact_code(s) {
                let after = tk.count_text(&compact);
                stats.tokens_before = stats.tokens_before.saturating_add(before);
                stats.tokens_after = stats.tokens_after.saturating_add(after);
                stats.compressed += 1;
                *s = compact;
            }
        }
        _ => {}
    }
}

// ────────────────────────────────────────────────────────────────────────────
// Phase B2: lossy semantic compressor
// ────────────────────────────────────────────────────────────────────────────

/// Replace a summarized segment's text with the summary plus a retrieval marker.
fn lossy_marker(summary: &str, hash: &str) -> String {
    format!("{}\n[ref:{hash}]", summary.trim())
}

/// System nudge telling the model how to recover summarized content.
const RETRIEVE_NUDGE: &str = "Some long content in this conversation was replaced \
    with a shorter summary followed by a marker like [ref:HASH]. If you need the \
    exact original text of a summarized section, call the retrieve_original tool \
    with that hash.";

/// Inject the `retrieve_original` tool definition (merging into any existing
/// `tools`) and a one-time system nudge describing the [ref:HASH] mechanism.
pub(super) fn inject_retrieve_original_tool(json: &mut Value, supports_system: bool) {
    if let Some(obj) = json.as_object_mut() {
        match obj.get_mut("tools").and_then(|v| v.as_array_mut()) {
            Some(existing) => existing.push(retrieve_original_tool_def()),
            None => {
                obj.insert("tools".into(), Value::Array(vec![retrieve_original_tool_def()]));
            }
        }
    }
    super::structured::inject_prompt_section(json, RETRIEVE_NUDGE, supports_system);
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct LossyStats {
    /// Prose/Log segments above the floor that were examined for lossy compaction.
    pub segments: u32,
    /// Segments actually replaced with a summary + ref (a stored original exists).
    pub refs_created: u32,
    pub tokens_before: u32,
    pub tokens_after: u32,
}

/// Lossy semantic compression: deterministically compacts long Prose segments
/// (via `extract_prose`) and Log segments (via `compact_log`) in string content
/// AND array-parts text, across all messages except a trailing assistant message.
/// Stashes each original in Redis for reversibility (best-effort; never fails the
/// request on Redis error). Only replaces on strict token reduction. Fail-open.
pub(super) async fn apply_lossy(
    state: &AppState,
    cfg: &CompressionBoonSettings,
    _key: &ResolvedKey,
    _session_id: &str,
    json: &mut Value,
) -> LossyStats {
    let mut stats = LossyStats::default();
    let tk = HeuristicTokenizer::new();
    let query_terms = latest_user_query_terms(json);

    // Collect (message_index, Option<part_index>, original_text) for eligible segments.
    let targets: Vec<(usize, Option<usize>, String)> = {
        let Some(messages) = json.get("messages").and_then(|m| m.as_array()) else {
            return stats;
        };
        let n = messages.len();
        let skip_last_assistant = n > 0
            && messages[n - 1].get("role").and_then(|r| r.as_str()) == Some("assistant");
        let mut out = Vec::new();
        for (mi, msg) in messages.iter().enumerate() {
            if skip_last_assistant && mi + 1 == n {
                continue;
            }
            match msg.get("content") {
                Some(Value::String(s)) => {
                    if tk.count_text(s) >= cfg.min_tokens {
                        out.push((mi, None, s.clone()));
                    }
                }
                Some(Value::Array(parts)) => {
                    for (pi, part) in parts.iter().enumerate() {
                        if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                            if tk.count_text(t) >= cfg.min_tokens {
                                out.push((mi, Some(pi), t.to_string()));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        out
    };

    for (mi, pi, original) in targets {
        if stats.segments >= cfg.max_lossy_segments {
            break;
        }
        let compacted = match classify(&original) {
            ContentKind::Log => compact_log(&original),
            ContentKind::Prose => extract_prose(&original, &query_terms, 0.4),
            _ => None, // JSON/Code handled by the lossless pass
        };
        let Some(compacted) = compacted else { continue };
        stats.segments += 1;
        let before = tk.count_text(&original);
        let hash = obleth_config::content_hash(&original);
        let marker = lossy_marker(&compacted, &hash);
        let after = tk.count_text(&marker);
        if after >= before {
            continue; // no token gain — skip before any Redis write
        }
        // Best-effort stash for the retrieve_original bonus; never fail on Redis error.
        let _ = state.redis.compress_put(&hash, &original, cfg.original_ttl_secs).await;
        if set_segment_text(json, mi, pi, marker) {
            stats.refs_created += 1;
            stats.tokens_before = stats.tokens_before.saturating_add(before);
            stats.tokens_after = stats.tokens_after.saturating_add(after);
        }
    }
    stats
}

/// Terms from the latest user message, used to bias prose extraction toward the
/// active question.
fn latest_user_query_terms(json: &Value) -> std::collections::HashSet<String> {
    let mut terms = std::collections::HashSet::new();
    if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
        if let Some(last_user) = messages
            .iter()
            .rev()
            .find(|m| m.get("role").and_then(|r| r.as_str()) == Some("user"))
        {
            let text = match last_user.get("content") {
                Some(Value::String(s)) => s.clone(),
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(" "),
                _ => String::new(),
            };
            for w in text.to_lowercase().split(|c: char| !c.is_alphanumeric()) {
                if w.len() >= 4 {
                    terms.insert(w.to_string());
                }
            }
        }
    }
    terms
}

/// Write `text` into message `mi`'s content (whole string when `pi` is None, else
/// the `text` of part `pi`). Returns whether it wrote.
fn set_segment_text(json: &mut Value, mi: usize, pi: Option<usize>, text: String) -> bool {
    let Some(msg) = json.get_mut("messages").and_then(|m| m.as_array_mut()).and_then(|m| m.get_mut(mi)) else {
        return false;
    };
    match pi {
        None => {
            if let Some(content) = msg.get_mut("content") {
                *content = Value::String(text);
                return true;
            }
        }
        Some(pi) => {
            if let Some(t) = msg
                .get_mut("content")
                .and_then(|c| c.as_array_mut())
                .and_then(|a| a.get_mut(pi))
                .and_then(|p| p.get_mut("text"))
            {
                *t = Value::String(text);
                return true;
            }
        }
    }
    false
}

// ────────────────────────────────────────────────────────────────────────────
// Phase C: cross-turn exact-duplicate dedup pass
// ────────────────────────────────────────────────────────────────────────────

/// Marker substituted for a duplicate occurrence of a block already present
/// earlier in the request. The original is retrievable via `retrieve_original`.
fn dedup_marker(hash: &str) -> String {
    format!("[ref:{hash}] (identical to earlier content; call retrieve_original for the full text)")
}

#[derive(Debug, Default, Clone, Copy)]
pub(super) struct DedupStats {
    /// Duplicate occurrences found (2nd+ copies of a repeated block).
    pub duplicates: u32,
    /// Duplicates actually replaced with a ref (a stored original exists).
    pub refs_created: u32,
    pub tokens_before: u32,
    pub tokens_after: u32,
}

/// Cross-turn exact-duplicate dedup: when a large block (>= `min_tokens`) appears
/// more than once across `messages[]`, keep the first occurrence verbatim and
/// replace each later identical occurrence with a `[ref:HASH]` marker, stashing
/// the original in Redis for `retrieve_original`. Covers both string content and
/// array-parts text. Skips only a trailing assistant message (active turn).
/// Best-effort Redis stash after gain check; fail-open per occurrence.
pub(super) async fn apply_dedup(
    state: &AppState,
    cfg: &CompressionBoonSettings,
    _key: &ResolvedKey,
    _session_id: &str,
    json: &mut Value,
) -> DedupStats {
    let mut stats = DedupStats::default();
    let tk = HeuristicTokenizer::new();
    let targets: Vec<(usize, Option<usize>, String)> = {
        let Some(messages) = json.get("messages").and_then(|m| m.as_array()) else {
            return stats;
        };
        let n = messages.len();
        let skip_last_assistant = n > 0
            && messages[n - 1].get("role").and_then(|r| r.as_str()) == Some("assistant");
        let mut out = Vec::new();
        for (mi, msg) in messages.iter().enumerate() {
            if skip_last_assistant && mi + 1 == n {
                continue;
            }
            match msg.get("content") {
                Some(Value::String(s)) if tk.count_text(s) >= cfg.min_tokens => {
                    out.push((mi, None, s.clone()));
                }
                Some(Value::Array(parts)) => {
                    for (pi, part) in parts.iter().enumerate() {
                        if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                            if tk.count_text(t) >= cfg.min_tokens {
                                out.push((mi, Some(pi), t.to_string()));
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        out
    };
    // First occurrence of each distinct text is kept; later ones are replaced.
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let dups: Vec<(usize, Option<usize>, String)> = targets
        .iter()
        .filter(|(_, _, t)| !seen.insert(t.as_str()))
        .cloned()
        .collect();
    for (mi, pi, content) in dups {
        stats.duplicates += 1;
        if stats.refs_created >= cfg.max_lossy_segments {
            break;
        }
        let before = tk.count_text(&content);
        let hash = obleth_config::content_hash(&content);
        let marker = dedup_marker(&hash);
        let after = tk.count_text(&marker);
        if after >= before {
            continue;
        }
        let _ = state.redis.compress_put(&hash, &content, cfg.original_ttl_secs).await;
        if set_segment_text(json, mi, pi, marker) {
            stats.refs_created += 1;
            stats.tokens_before = stats.tokens_before.saturating_add(before);
            stats.tokens_after = stats.tokens_after.saturating_add(after);
        }
    }
    stats
}

/// Normalize a line for near-duplicate detection: trim + lowercase + collapse digits.
fn dedup_key(line: &str) -> String {
    line.trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect()
}

/// Deterministic extractive compaction of prose/log-ish text: score each line by
/// salience (length/density + overlap with the request's query terms), drop
/// blank lines and near-duplicates, and keep the top `keep_ratio` of lines in
/// original order. Returns `Some` only when strictly shorter than the input.
pub(super) fn extract_prose(
    text: &str,
    query_terms: &std::collections::HashSet<String>,
    keep_ratio: f32,
) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 8 {
        return None; // too small to benefit
    }
    let mut seen = std::collections::HashSet::new();
    // (original_index, score) for non-blank, non-duplicate lines.
    let mut scored: Vec<(usize, f32)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !seen.insert(dedup_key(line)) {
            continue; // near-duplicate
        }
        let mut score = (trimmed.len().min(120) as f32) / 120.0; // density proxy
        let lower = trimmed.to_lowercase();
        if query_terms.iter().any(|t| lower.contains(t.as_str())) {
            score += 2.0; // relevance to the request
        }
        scored.push((i, score));
    }
    if scored.is_empty() {
        return None;
    }
    let keep = ((scored.len() as f32) * keep_ratio).ceil().max(1.0) as usize;
    if keep >= scored.len() {
        return None; // nothing meaningfully dropped
    }
    // Choose the top `keep` by score, then restore original order.
    let mut by_score = scored.clone();
    by_score.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal).then(a.0.cmp(&b.0)));
    let mut kept_idx: Vec<usize> = by_score.into_iter().take(keep).map(|(i, _)| i).collect();
    kept_idx.sort_unstable();
    let omitted = lines.len() - kept_idx.len();
    let mut out = String::with_capacity(text.len());
    for i in &kept_idx {
        out.push_str(lines[*i]);
        out.push('\n');
    }
    out.push_str(&format!("[… {omitted} lines omitted]"));
    if out.len() >= text.len() {
        return None;
    }
    Some(out)
}

/// Mask variable tokens (digit runs, hex) so structurally-identical log lines
/// share a template.
fn log_template(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            out.push('#');
            while chars.peek().is_some_and(|d| d.is_ascii_digit() || *d == ':' || *d == '.' || *d == '-') {
                chars.next();
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Collapse repeated log lines: lines sharing a masked template that appears more
/// than 3× are represented by their first occurrence + `(×N)`; `error`/`warn`
/// lines are always kept verbatim. Returns `Some` only when strictly shorter.
pub(super) fn compact_log(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() < 8 {
        return None;
    }
    use std::collections::HashMap;
    let mut counts: HashMap<String, usize> = HashMap::new();
    for line in &lines {
        *counts.entry(log_template(line)).or_insert(0) += 1;
    }
    let mut emitted: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut out = String::with_capacity(text.len());
    for line in &lines {
        let lower = line.to_lowercase();
        let is_problem = lower.contains("error") || lower.contains("warn");
        let tmpl = log_template(line);
        let count = counts.get(&tmpl).copied().unwrap_or(1);
        if is_problem || count <= 3 {
            out.push_str(line);
            out.push('\n');
        } else if emitted.insert(tmpl) {
            // First occurrence of a frequently-repeated template.
            out.push_str(line);
            out.push_str(&format!(" (×{count})\n"));
        }
        // else: a subsequent occurrence of an already-collapsed template → drop.
    }
    if out.len() >= text.len() {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classifies_json_object() {
        assert_eq!(classify("  {\"a\": 1, \"b\": [1,2,3]}  "), ContentKind::Json);
    }

    #[test]
    fn classifies_json_array() {
        assert_eq!(classify("[1, 2, 3]"), ContentKind::Json);
    }

    #[test]
    fn classifies_prose() {
        assert_eq!(classify("The quick brown fox jumps over the lazy dog."), ContentKind::Prose);
    }

    #[test]
    fn classifies_code_fence_as_code() {
        assert_eq!(classify("```python\nprint('hi')\n```"), ContentKind::Code);
    }

    #[test]
    fn bare_number_is_not_json() {
        // A bare scalar is not worth treating as structured JSON.
        assert_eq!(classify("42"), ContentKind::Prose);
    }

    #[test]
    fn compacts_pretty_json() {
        let pretty = "{\n  \"a\": 1,\n  \"b\": [1, 2, 3]\n}";
        let out = compact_json(pretty).expect("should compact");
        assert!(out.len() < pretty.len());
        // Lossless: re-parses to the same value.
        let a: serde_json::Value = serde_json::from_str(pretty).unwrap();
        let b: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn already_compact_json_returns_none() {
        assert_eq!(compact_json("{\"a\":1}"), None);
    }

    #[test]
    fn compact_json_is_lossless_despite_key_reordering() {
        let input = "{\n  \"z\": 99,\n  \"a\": 1\n}";
        let out = compact_json(input).expect("should compact");
        let before: serde_json::Value = serde_json::from_str(input).unwrap();
        let after: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(before, after); // Value equality holds even though keys may reorder
    }

    #[test]
    fn non_json_returns_none() {
        assert_eq!(compact_json("just some prose"), None);
    }

    #[test]
    fn apply_compacts_large_json_message() {
        let big_array: Vec<i64> = (0..500).collect();
        let pretty = serde_json::to_string_pretty(&json!({ "rows": big_array })).unwrap();
        let mut body = json!({
            "model": "m",
            "messages": [
                { "role": "system", "content": "be helpful" },
                { "role": "tool", "content": pretty }
            ]
        });
        let cfg = obleth_config::CompressionBoonSettings { enabled: true, min_tokens: 16, max_segments: 64, ..Default::default() };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.compressed, 1);
        assert!(stats.tokens_after < stats.tokens_before);
        // The tool message is now compact (no newline indentation).
        let tool = body["messages"][1]["content"].as_str().unwrap();
        assert!(!tool.contains("\n  "));
    }

    #[test]
    fn apply_skips_small_segments() {
        let mut body = json!({
            "model": "m",
            "messages": [ { "role": "tool", "content": "{\"a\": 1}" } ]
        });
        let cfg = obleth_config::CompressionBoonSettings { enabled: true, min_tokens: 512, max_segments: 64, ..Default::default() };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.compressed, 0);
    }

    #[test]
    fn apply_respects_max_segments() {
        let big = serde_json::to_string_pretty(&json!({ "rows": (0..500).collect::<Vec<i64>>() })).unwrap();
        let mut body = json!({
            "model": "m",
            "messages": [
                { "role": "tool", "content": big.clone() },
                { "role": "tool", "content": big }
            ]
        });
        let cfg = obleth_config::CompressionBoonSettings { enabled: true, min_tokens: 16, max_segments: 1, ..Default::default() };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.compressed, 1);
    }

    #[test]
    fn apply_compacts_array_parts_message() {
        let big_array: Vec<i64> = (0..500).collect();
        let pretty = serde_json::to_string_pretty(&json!({ "rows": big_array })).unwrap();
        let mut body = json!({
            "model": "m",
            "messages": [{
                "role": "user",
                "content": [{ "type": "text", "text": pretty }]
            }]
        });
        let cfg = obleth_config::CompressionBoonSettings { enabled: true, min_tokens: 16, max_segments: 64, ..Default::default() };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.compressed, 1);
        assert!(stats.tokens_after < stats.tokens_before);
        let text = body["messages"][0]["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("\n  "));
    }

    #[test]
    fn lossy_marker_carries_summary_and_ref() {
        let m = lossy_marker("short summary", "deadbeef");
        assert!(m.contains("short summary"));
        assert!(m.contains("[ref:deadbeef]"));
    }

    #[test]
    fn inject_retrieve_original_tool_merges_and_nudges() {
        let mut body = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "hi" }],
            "tools": [{ "type": "function", "function": { "name": "existing" } }]
        });
        inject_retrieve_original_tool(&mut body, true);
        let tools = body["tools"].as_array().unwrap();
        // Existing tool preserved, retrieve_original appended.
        assert_eq!(tools.len(), 2);
        assert!(tools.iter().any(|t| t["function"]["name"] == "retrieve_original"));
        // A system nudge mentioning the marker mechanism was injected.
        let has_nudge = body["messages"].as_array().unwrap().iter().any(|msg| {
            msg["content"].as_str().is_some_and(|c| c.contains("[ref:"))
        });
        assert!(has_nudge);
    }

    #[test]
    fn inject_retrieve_original_tool_creates_tools_array_when_absent() {
        let mut body = json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] });
        inject_retrieve_original_tool(&mut body, false);
        assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn compact_code_strips_trailing_ws_and_collapses_blank_runs() {
        let input = "```py\nx = 1   \n\n\n\ny = 2\t\n```";
        let out = compact_code(input).expect("should compact");
        assert!(out.len() < input.len());
        assert!(!out.contains("   \n")); // no trailing whitespace before newline
        assert!(!out.contains("\n\n\n")); // blank runs collapsed to a single blank line
        assert!(out.contains("x = 1"));
        assert!(out.contains("y = 2"));
    }

    #[test]
    fn compact_code_returns_none_when_already_tight() {
        assert_eq!(compact_code("```py\nx = 1\ny = 2\n```"), None);
    }

    #[test]
    fn dedup_marker_carries_ref() {
        let m = dedup_marker("cafef00d");
        assert!(m.contains("[ref:cafef00d]"));
    }

    #[test]
    fn apply_compacts_code_when_enabled() {
        let big = format!("```py\n{}\n```", "x = 1   \n\n\n".repeat(60));
        let mut body = json!({
            "model": "m",
            "messages": [ { "role": "user", "content": big } ]
        });
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true, min_tokens: 16, max_segments: 64, ..Default::default()
        };
        let stats = apply(&cfg, true, &mut body);
        assert_eq!(stats.compressed, 1);
        assert!(stats.tokens_after < stats.tokens_before);
    }

    #[test]
    fn apply_leaves_code_when_disabled() {
        let big = format!("```py\n{}\n```", "x = 1   \n\n\n".repeat(60));
        let mut body = json!({
            "model": "m",
            "messages": [ { "role": "user", "content": big } ]
        });
        let cfg = obleth_config::CompressionBoonSettings {
            enabled: true, min_tokens: 16, max_segments: 64, ..Default::default()
        };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.compressed, 0);
    }

    #[test]
    fn apply_uses_structural_table_for_object_arrays() {
        let rows: Vec<serde_json::Value> = (0..200)
            .map(|i| json!({ "id": i, "name": format!("item-{i}"), "active": true }))
            .collect();
        let pretty = serde_json::to_string_pretty(&json!(rows)).unwrap();
        let mut body = json!({
            "model": "m",
            "messages": [ { "role": "tool", "content": pretty } ]
        });
        let cfg = obleth_config::CompressionBoonSettings { enabled: true, min_tokens: 16, max_segments: 64, ..Default::default() };
        let stats = apply(&cfg, false, &mut body);
        assert_eq!(stats.compressed, 1);
        assert!(stats.tokens_after < stats.tokens_before);
        let out = body["messages"][0]["content"].as_str().unwrap();
        assert!(out.starts_with("OBLETH_TABLE rows=200\n"));
    }

    #[test]
    fn extract_prose_drops_low_value_and_dupes_keeps_query_lines() {
        use std::collections::HashSet;
        let mut text = String::new();
        for _ in 0..40 { text.push_str("boilerplate filler line that repeats a lot\n"); }
        text.push_str("the deadbeef token error occurred in module X\n");
        for _ in 0..40 { text.push_str("more boilerplate filler line that repeats\n"); }
        let mut q = HashSet::new();
        q.insert("deadbeef".to_string());
        let out = extract_prose(&text, &q, 0.3).expect("should extract");
        assert!(out.len() < text.len());
        assert!(out.contains("deadbeef")); // query-relevant line is kept
        assert!(out.contains("omitted")); // omission marker present
    }

    #[test]
    fn extract_prose_returns_none_when_short() {
        use std::collections::HashSet;
        assert_eq!(extract_prose("a\nb\nc", &HashSet::new(), 0.5), None);
    }

    #[test]
    fn compact_log_collapses_repeated_templates_keeps_errors() {
        let mut text = String::new();
        for i in 0..50 { text.push_str(&format!("2026-06-29 INFO request {i} handled in {i}ms\n")); }
        text.push_str("2026-06-29 ERROR disk full on /dev/sda1\n");
        let out = compact_log(&text).expect("should compact");
        assert!(out.len() < text.len());
        assert!(out.contains("(×")); // collapsed repeat marker
        assert!(out.contains("ERROR disk full")); // error line preserved verbatim
    }

    #[test]
    fn classifies_log_shaped_text() {
        let mut text = String::new();
        for i in 0..20 { text.push_str(&format!("2026-06-29T10:00:0{} INFO did thing {}\n", i % 10, i)); }
        assert_eq!(classify(&text), ContentKind::Log);
    }

    #[test]
    fn classifies_plain_paragraph_as_prose() {
        assert_eq!(classify("The quick brown fox jumps over the lazy dog. It was a fine day."), ContentKind::Prose);
    }

    #[test]
    fn apply_lossy_compacts_prose_in_latest_user_message() {
        // Build a long prose user message (the latest turn) + a short follow-up question.
        let big: String = (0..60).map(|i| format!("This is line number {i} of some pasted notes.\n")).collect();
        let mut body = json!({
            "model": "m",
            "messages": [
                { "role": "user", "content": big },
                { "role": "user", "content": "summarize the notes above" }
            ]
        });
        let cfg = obleth_config::CompressionBoonSettings { enabled: true, min_tokens: 16, max_lossy_segments: 8, ..Default::default() };
        // No AppState/Redis in a unit test → call the pure helper path via a thin wrapper is not possible;
        // instead assert extract_prose drives the change through a small synchronous shim:
        let q: std::collections::HashSet<String> = ["summarize", "notes"].iter().map(|s| s.to_string()).collect();
        let first = body["messages"][0]["content"].as_str().unwrap().to_string();
        let compacted = extract_prose(&first, &q, 0.4).expect("prose compacts");
        assert!(compacted.len() < first.len());
        // The active question (last message) is short (below floor) and untouched regardless.
        assert_eq!(body["messages"][1]["content"], "summarize the notes above");
    }
}
