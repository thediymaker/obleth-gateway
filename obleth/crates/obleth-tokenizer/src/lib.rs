//! Token counting and output estimation.
//!
//! Fairshare is measured in tokens, so admission needs a cost estimate *before*
//! the upstream runs. We expose a trait so a precise BPE tokenizer (tiktoken /
//! HF `tokenizers`) can replace the heuristic without touching the data plane.
//!
//! The default [`HeuristicTokenizer`] needs no model files and is dependency
//! light: ~4 chars/token, which is close enough for budget *reservation*. The
//! true cost is always reconciled from the upstream's reported usage afterward,
//! so estimation error only affects admission ordering, never billing accuracy.

use serde_json::Value;

/// Estimated cost of a request, in tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CostEstimate {
    pub input_tokens: u32,
    pub estimated_output_tokens: u32,
}

impl CostEstimate {
    /// Total tokens to reserve up front (input is certain, output is a ceiling).
    pub fn total(&self) -> u32 {
        self.input_tokens
            .saturating_add(self.estimated_output_tokens)
    }
}

/// Strategy for counting input tokens and estimating output tokens.
pub trait Tokenizer: Send + Sync {
    fn count_text(&self, text: &str) -> u32;

    /// Estimate cost from a parsed OpenAI-style chat/completions body.
    fn estimate_request(&self, body: &Value) -> CostEstimate {
        let input_tokens = count_prompt_tokens(self, body);
        let estimated_output_tokens = estimate_output_tokens(body, input_tokens);
        CostEstimate {
            input_tokens,
            estimated_output_tokens,
        }
    }
}

/// Cheap, model-free estimator. Good enough for admission; exact cost is
/// reconciled later from upstream usage.
#[derive(Debug, Clone, Default)]
pub struct HeuristicTokenizer;

impl HeuristicTokenizer {
    pub fn new() -> Self {
        HeuristicTokenizer
    }
}

impl Tokenizer for HeuristicTokenizer {
    fn count_text(&self, text: &str) -> u32 {
        // ~4 chars per token, with a small per-call floor.
        let chars = text.chars().count() as u32;
        (chars / 4).max(if text.is_empty() { 0 } else { 1 })
    }
}

/// Default output ceiling when the caller does not pin `max_tokens`.
const DEFAULT_OUTPUT_CEILING: u32 = 512;
/// Hard cap so a single unbounded request can't reserve the whole budget.
const MAX_OUTPUT_CEILING: u32 = 8192;

fn count_prompt_tokens<T: Tokenizer + ?Sized>(tk: &T, body: &Value) -> u32 {
    // `messages` (chat) or `prompt` (completions).
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        let mut total = 0u32;
        for msg in messages {
            if let Some(content) = msg.get("content") {
                total = total.saturating_add(count_content(tk, content));
            }
            // small per-message overhead for role/formatting tokens
            total = total.saturating_add(4);
        }
        return total;
    }
    if let Some(prompt) = body.get("prompt") {
        return count_content(tk, prompt);
    }
    0
}

fn count_content<T: Tokenizer + ?Sized>(tk: &T, content: &Value) -> u32 {
    match content {
        Value::String(s) => tk.count_text(s),
        Value::Array(parts) => parts
            .iter()
            .map(|p| {
                p.get("text")
                    .and_then(Value::as_str)
                    .map(|s| tk.count_text(s))
                    .unwrap_or(0)
            })
            .sum(),
        _ => 0,
    }
}

fn estimate_output_tokens(body: &Value, input_tokens: u32) -> u32 {
    if let Some(max) = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .and_then(Value::as_u64)
    {
        return (max as u32).min(MAX_OUTPUT_CEILING);
    }
    // No explicit cap: assume output proportional to input, bounded.
    input_tokens.clamp(DEFAULT_OUTPUT_CEILING, MAX_OUTPUT_CEILING)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn counts_chat_messages() {
        let tk = HeuristicTokenizer::new();
        let body = json!({
            "model": "test",
            "messages": [{"role": "user", "content": "hello world this is a test"}]
        });
        let est = tk.estimate_request(&body);
        assert!(est.input_tokens > 0);
        // no max_tokens -> default ceiling
        assert_eq!(est.estimated_output_tokens, DEFAULT_OUTPUT_CEILING);
    }

    #[test]
    fn respects_max_tokens() {
        let tk = HeuristicTokenizer::new();
        let body = json!({"prompt": "abc", "max_tokens": 16});
        let est = tk.estimate_request(&body);
        assert_eq!(est.estimated_output_tokens, 16);
    }

    #[test]
    fn caps_unbounded_output() {
        let tk = HeuristicTokenizer::new();
        let body = json!({"prompt": "x", "max_tokens": 999999});
        let est = tk.estimate_request(&body);
        assert_eq!(est.estimated_output_tokens, MAX_OUTPUT_CEILING);
    }
}
