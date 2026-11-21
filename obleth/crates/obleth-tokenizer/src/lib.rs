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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
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

/// Flat input-token estimate for one image part. Images aren't tokenized
/// here, and their real cost depends on the model, the resolution and the
/// vision encoder, none of which the gateway knows ahead of the upstream. A
/// round number: large enough that a handful of images still counts, small
/// enough not to starve a single screenshot's request.
pub const IMAGE_PART_TOKENS: u32 = 1500;

/// The prompt's input tokens: message text with a small per-message overhead
/// (or a completions `prompt`), plus the `tools` array and a flat cost per
/// image part. Tools and images matter: an agent client's tool list alone can
/// run 10-20k tokens, and an image part's few bytes of URL are nothing like
/// its real cost once decoded.
fn count_prompt_tokens<T: Tokenizer + ?Sized>(tk: &T, body: &Value) -> u32 {
    // `messages` (chat) or `prompt` (completions).
    let text = if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        let mut total = 0u32;
        for msg in messages {
            if let Some(content) = msg.get("content") {
                total = total.saturating_add(count_content(tk, content));
            }
            // small per-message overhead for role/formatting tokens
            total = total.saturating_add(4);
        }
        total
    } else if let Some(prompt) = body.get("prompt") {
        count_content(tk, prompt)
    } else {
        0
    };
    text.saturating_add(tools_tokens(body))
        .saturating_add(image_parts(body).saturating_mul(IMAGE_PART_TOKENS))
}

/// The `tools` array at about 3 characters a token, not the 4 of prose: it is
/// JSON schema (short, punctuation- and digit-heavy field names), which
/// tokenizes denser. It only needs the right order of magnitude.
fn tools_tokens(body: &Value) -> u32 {
    body.get("tools")
        .map(|t| u32::try_from(serialized_len(t) / 3).unwrap_or(u32::MAX))
        .unwrap_or(0)
}

/// `image_url` parts across the chat messages.
fn image_parts(body: &Value) -> u32 {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return 0;
    };
    let count = messages
        .iter()
        .filter_map(|m| m.get("content").and_then(Value::as_array))
        .flatten()
        .filter(|part| part.get("type").and_then(Value::as_str) == Some("image_url"))
        .count();
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// The length `value` serializes to, counted without building the string.
fn serialized_len(value: &Value) -> usize {
    struct Counting(usize);
    impl std::io::Write for Counting {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0 += buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counting = Counting(0);
    // A `Value` always serializes; the writer never fails.
    let _ = serde_json::to_writer(&mut counting, value);
    counting.0
}

/// Output tokens for `chars` characters of generated text that the upstream
/// reported no usage for, counted the way [`HeuristicTokenizer`] counts text
/// (~4 characters a token, at least 1 for any text). None is 0.
pub fn output_tokens_from_chars(chars: usize) -> u32 {
    if chars == 0 {
        return 0;
    }
    u32::try_from(chars / 4).unwrap_or(u32::MAX).max(1)
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
    // A request may carry both names for the cap. Which one a model server
    // honours is its own choice, so the reservation covers the larger.
    if let Some(max) = ["max_tokens", "max_completion_tokens"]
        .iter()
        .filter_map(|name| body.get(*name).and_then(Value::as_u64))
        .max()
    {
        // Clamp in u64 first: casting a value above u32::MAX truncates it
        // (u32::MAX + 1 would become a zero-token reservation).
        return max.min(MAX_OUTPUT_CEILING as u64) as u32;
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

    /// A request can name its cap twice; the server chooses which it honours,
    /// so the estimate covers the larger.
    #[test]
    fn the_larger_of_two_caps_is_estimated() {
        let body = json!({"prompt": "abc", "max_tokens": 1, "max_completion_tokens": 4000});
        assert_eq!(estimate_output_tokens(&body, 1), 4000);
        let body = json!({"prompt": "abc", "max_tokens": 64, "max_completion_tokens": 8});
        assert_eq!(estimate_output_tokens(&body, 1), 64);
        let body = json!({"prompt": "abc", "max_completion_tokens": 12});
        assert_eq!(estimate_output_tokens(&body, 1), 12);
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

    #[test]
    fn tools_count_at_three_characters_a_token() {
        let tk = HeuristicTokenizer::new();
        let plain = json!({"messages": [{"role": "user", "content": "hi"}]});
        let mut with_tools = plain.clone();
        with_tools["tools"] = json!([{"type": "function", "function": {"name": "f", "parameters": {"type": "object"}}}]);
        let tools_len = with_tools["tools"].to_string().len() as u32;
        assert_eq!(
            tk.estimate_request(&with_tools).input_tokens,
            tk.estimate_request(&plain).input_tokens + tools_len / 3
        );
    }

    #[test]
    fn each_image_part_counts_a_flat_amount() {
        let tk = HeuristicTokenizer::new();
        let body = json!({"messages": [
            {"role": "user", "content": [
                {"type": "text", "text": "what is this"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
            ]},
            {"role": "assistant", "content": "it's a cat"},
            {"role": "user", "content": [
                {"type": "image_url", "image_url": {"url": "https://example.com/x.png"}}
            ]}
        ]});
        let text_only = tk.count_text("what is this") + tk.count_text("it's a cat") + 3 * 4;
        assert_eq!(
            tk.estimate_request(&body).input_tokens,
            text_only + 2 * IMAGE_PART_TOKENS
        );
    }

    #[test]
    fn the_tools_length_is_counted_not_built() {
        let tools = json!([{"type": "function", "function": {"name": "f", "parameters": {"type": "object"}}}]);
        assert_eq!(serialized_len(&tools), tools.to_string().len());
    }

    #[test]
    fn output_from_characters_matches_the_text_count() {
        assert_eq!(output_tokens_from_chars(0), 0);
        assert_eq!(output_tokens_from_chars(3), 1);
        assert_eq!(output_tokens_from_chars(400), 100);
        let text = "x".repeat(400);
        assert_eq!(
            output_tokens_from_chars(400),
            HeuristicTokenizer::new().count_text(&text)
        );
    }

    #[test]
    fn max_tokens_above_u32_clamps_instead_of_wrapping() {
        let tk = HeuristicTokenizer::new();
        // u32::MAX + 1 truncates to 0 if cast before clamping.
        let body = json!({"prompt": "x", "max_tokens": u32::MAX as u64 + 1});
        let est = tk.estimate_request(&body);
        assert_eq!(est.estimated_output_tokens, MAX_OUTPUT_CEILING);
    }
}
