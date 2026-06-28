//! The **compression** boon: gateway-side reduction of what the model reads.
//!
//! Phase A: lossless structural compaction of JSON content in chat messages,
//! gated per-model and by global settings. Fail-open like every boon — any
//! error or absence of gain leaves the request untouched.

use obleth_config::CompressionBoonSettings;
use obleth_tokenizer::{HeuristicTokenizer, Tokenizer};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ContentKind {
    Json,
    Code,
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
pub(super) fn apply(cfg: &CompressionBoonSettings, json: &mut Value) -> CompressionStats {
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
                try_compact_string(cfg, &tk, s, &mut stats);
            }
            Some(Value::Array(parts)) => {
                for part in parts.iter_mut() {
                    if stats.compressed >= cfg.max_segments {
                        break;
                    }
                    if let Some(Value::String(s)) = part.get_mut("text") {
                        try_compact_string(cfg, &tk, s, &mut stats);
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
    tk: &HeuristicTokenizer,
    s: &mut String,
    stats: &mut CompressionStats,
) {
    let before = tk.count_text(s);
    if before < cfg.min_tokens {
        return;
    }
    stats.scanned += 1;
    if classify(s) != ContentKind::Json {
        return;
    }
    if let Some(compact) = compact_json(s) {
        let after = tk.count_text(&compact);
        stats.tokens_before = stats.tokens_before.saturating_add(before);
        stats.tokens_after = stats.tokens_after.saturating_add(after);
        stats.compressed += 1;
        *s = compact;
    }
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
        let cfg = obleth_config::CompressionBoonSettings { enabled: true, min_tokens: 16, max_segments: 64 };
        let stats = apply(&cfg, &mut body);
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
        let cfg = obleth_config::CompressionBoonSettings { enabled: true, min_tokens: 512, max_segments: 64 };
        let stats = apply(&cfg, &mut body);
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
        let cfg = obleth_config::CompressionBoonSettings { enabled: true, min_tokens: 16, max_segments: 1 };
        let stats = apply(&cfg, &mut body);
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
        let cfg = obleth_config::CompressionBoonSettings { enabled: true, min_tokens: 16, max_segments: 64 };
        let stats = apply(&cfg, &mut body);
        assert_eq!(stats.compressed, 1);
        assert!(stats.tokens_after < stats.tokens_before);
        let text = body["messages"][0]["content"][0]["text"].as_str().unwrap();
        assert!(!text.contains("\n  "));
    }
}
