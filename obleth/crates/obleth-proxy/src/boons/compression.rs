//! The **compression** boon: gateway-side reduction of what the model reads.
//!
//! Phase A: lossless structural compaction of JSON content in chat messages,
//! gated per-model and by global settings. Fail-open like every boon — any
//! error or absence of gain leaves the request untouched.

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
