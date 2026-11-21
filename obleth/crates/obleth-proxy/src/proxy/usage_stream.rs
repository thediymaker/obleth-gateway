//! Reading usage out of a relayed stream: the bounded tail kept for the final
//! usage chunk, and the visible-text counts that stand in when no usage comes.

use super::*;

pub(super) const TAIL_CAP: usize = 16 * 1024;

/// Characters of generated text in one raw response chunk: the string values
/// of `content`, `reasoning_content`, `reasoning` and `text` fields. A byte
/// scan rather than a JSON parse so it can run on every streamed chunk; a
/// string split across two chunks is only partly counted, which errs low.
///
/// Some upstreams mirror the same reasoning text under both
/// `reasoning_content` and `reasoning` in one delta; counting both would
/// double the estimate, so `reasoning` is only counted when the chunk has no
/// `reasoning_content` text of its own.
pub(super) fn delta_text_chars(chunk: &[u8]) -> usize {
    let content = field_text_chars(chunk, b"\"content\"");
    let reasoning_content = field_text_chars(chunk, b"\"reasoning_content\"");
    let reasoning = if reasoning_content > 0 {
        0
    } else {
        field_text_chars(chunk, b"\"reasoning\"")
    };
    let text = field_text_chars(chunk, b"\"text\"");
    content + reasoning_content + reasoning + text
}

/// Characters in every string value that follows `key` in the chunk.
fn field_text_chars(chunk: &[u8], key: &[u8]) -> usize {
    let skip_spaces = |mut i: usize| {
        while i < chunk.len() && chunk[i] == b' ' {
            i += 1;
        }
        i
    };
    let mut total = 0;
    let mut from = 0;
    while let Some(at) = crate::sse::find_bytes(&chunk[from..], key) {
        let mut i = skip_spaces(from + at + key.len());
        if i >= chunk.len() || chunk[i] != b':' {
            from = i;
            continue;
        }
        i = skip_spaces(i + 1);
        if i >= chunk.len() || chunk[i] != b'"' {
            // `null` or a non-string value.
            from = i;
            continue;
        }
        i += 1;
        while i < chunk.len() {
            match chunk[i] {
                b'"' => {
                    i += 1;
                    break;
                }
                b'\\' => {
                    total += 1;
                    i += if chunk.get(i + 1) == Some(&b'u') {
                        6
                    } else {
                        2
                    };
                }
                // UTF-8 continuation bytes belong to the character before.
                b if b & 0xC0 == 0x80 => i += 1,
                _ => {
                    total += 1;
                    i += 1;
                }
            }
        }
        from = i.min(chunk.len());
    }
    total
}

/// Append a chunk to the rolling response tail, keeping only the last
/// `TAIL_CAP` bytes. Works on raw bytes with `copy_within` so a long stream
/// never re-allocates per chunk; the one UTF-8 conversion happens at stream
/// end (a partial char at the buffer start is replaced lossily there, which is
/// harmless — `extract_usage` only scans for ASCII JSON keys).
pub(super) fn append_tail(tail: &mut Vec<u8>, chunk: &[u8]) {
    if chunk.len() >= TAIL_CAP {
        tail.clear();
        tail.extend_from_slice(&chunk[chunk.len() - TAIL_CAP..]);
        return;
    }
    let overflow = (tail.len() + chunk.len()).saturating_sub(TAIL_CAP);
    if overflow > 0 {
        tail.copy_within(overflow.., 0);
        tail.truncate(tail.len() - overflow);
    }
    tail.extend_from_slice(chunk);
}

/// Pull `prompt_tokens` / `completion_tokens` out of the (possibly streamed)
/// response tail. Returns `None` if the upstream didn't report usage.
///
/// Embedding responses report `prompt_tokens` (and `total_tokens`) but no
/// `completion_tokens`; those are treated as input-only usage.
///
/// Cached prompt tokens come from the usage object holding that last
/// `prompt_tokens`, so a stream that reports usage on several chunks never
/// pairs one chunk's prompt count with another chunk's cache detail.
pub(super) fn extract_usage(tail: &str) -> Option<UpstreamUsage> {
    let input = find_int_after(tail, "\"prompt_tokens\"");
    let output = find_int_after(tail, "\"completion_tokens\"");
    let (input, output) = match (input, output) {
        (Some(i), Some(o)) => (i, o),
        // Embeddings and other input-only modalities: count prompt tokens.
        (Some(i), None) => (i, 0),
        _ => return None,
    };
    Some(UpstreamUsage::new(input, output, tail_cached_tokens(tail)))
}

/// Cached prompt tokens of the JSON object around the tail's last
/// `prompt_tokens`; 0 when it reports none, is cut off, or does not parse.
fn tail_cached_tokens(tail: &str) -> u32 {
    let Some(at) = tail.rfind("\"prompt_tokens\"") else {
        return 0;
    };
    enclosing_json_object(tail, at)
        .and_then(|object| serde_json::from_str::<serde_json::Value>(object).ok())
        .map_or(0, |usage| cached_prompt_tokens(&usage) as u32)
}

/// The JSON object that byte offset `at` sits in: from the nearest `{` before
/// it that is still open there, through its matching `}`. The backward walk
/// counts braces only (a usage object's keys hold none); the forward walk
/// skips string contents. `None` when either end is outside `text`.
fn enclosing_json_object(text: &str, at: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut start = None;
    for i in (0..at).rev() {
        match bytes[i] {
            b'}' => depth += 1,
            b'{' if depth == 0 => {
                start = Some(i);
                break;
            }
            b'{' => depth -= 1,
            _ => {}
        }
    }
    let start = start?;
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    for (offset, &b) in bytes[start..].iter().enumerate() {
        if in_string {
            match b {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match b {
            b'"' => in_string = true,
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..=start + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

fn find_int_after(haystack: &str, key: &str) -> Option<u32> {
    let idx = haystack.rfind(key)? + key.len();
    let rest = &haystack[idx..];
    let digits: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::lifecycle_tests::{content_event, sse};

    /// The tail of a streamed chat completion: one content chunk, the final
    /// usage-only chunk carrying `usage`, then `[DONE]`.
    fn stream_tail(usage: serde_json::Value) -> String {
        let content = serde_json::json!({
            "object": "chat.completion.chunk",
            "choices": [{ "index": 0, "delta": { "content": "hi" } }],
        });
        let last = serde_json::json!({
            "object": "chat.completion.chunk", "choices": [], "usage": usage,
        });
        format!("data: {content}\n\ndata: {last}\n\ndata: [DONE]\n\n")
    }

    #[test]
    fn append_tail_keeps_last_cap_bytes() {
        use super::{append_tail, extract_usage, UpstreamUsage, TAIL_CAP};
        let mut tail = Vec::new();
        // Small chunks accumulate verbatim.
        append_tail(&mut tail, b"hello ");
        append_tail(&mut tail, b"world");
        assert_eq!(tail, b"hello world");
        // Filling past the cap keeps only the newest TAIL_CAP bytes.
        append_tail(&mut tail, &vec![b'x'; TAIL_CAP]);
        assert_eq!(tail.len(), TAIL_CAP);
        assert!(tail.iter().all(|b| *b == b'x'));
        // A usage blob appended at the end survives intact and parses.
        append_tail(
            &mut tail,
            br#"{"usage":{"prompt_tokens":12,"completion_tokens":34}}"#,
        );
        assert_eq!(tail.len(), TAIL_CAP);
        assert_eq!(
            extract_usage(&String::from_utf8_lossy(&tail)),
            Some(UpstreamUsage::uncached(12, 34))
        );
        // A chunk larger than the cap replaces the buffer with its own tail.
        append_tail(&mut tail, &vec![b'y'; TAIL_CAP * 2]);
        assert_eq!(tail.len(), TAIL_CAP);
        assert!(tail.iter().all(|b| *b == b'y'));
    }

    #[test]
    fn delta_text_counting_handles_escapes_multibyte_and_null() {
        let e = sse(serde_json::json!({
            "choices": [{ "delta": { "content": "a\"b\né", "reasoning_content": "hm" } }],
        }));
        // a " b \n é = 5, plus "hm" = 2.
        assert_eq!(delta_text_chars(e.as_bytes()), 7);
        let null = content_event("");
        assert_eq!(delta_text_chars(null.as_bytes()), 0);
        assert_eq!(delta_text_chars(br#"{"content": null}"#), 0);
    }

    #[test]
    fn delta_text_counting_does_not_double_count_mirrored_reasoning() {
        // Some upstreams echo the same reasoning text under both
        // `reasoning_content` and `reasoning` in one delta. Only
        // `reasoning_content` should count.
        let e = sse(serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "hi",
                    "reasoning_content": "thinking",
                    "reasoning": "thinking",
                },
            }],
        }));
        // "hi" = 2, plus "thinking" once = 8.
        assert_eq!(delta_text_chars(e.as_bytes()), 10);

        // Without `reasoning_content`, `reasoning` still counts on its own.
        let fallback = sse(serde_json::json!({
            "choices": [{ "delta": { "reasoning": "thinking" } }],
        }));
        assert_eq!(delta_text_chars(fallback.as_bytes()), 8);
    }

    #[test]
    fn stream_tail_usage_reads_cached_prompt_tokens() {
        let tail = stream_tail(serde_json::json!({
            "prompt_tokens": 2048, "total_tokens": 2098, "completion_tokens": 50,
            "prompt_tokens_details": { "cached_tokens": 1536 },
        }));
        assert_eq!(
            extract_usage(&tail),
            Some(UpstreamUsage::new(2048, 50, 1536))
        );
    }

    #[test]
    fn stream_tail_usage_without_cache_detail_is_unchanged() {
        let null = stream_tail(serde_json::json!({
            "prompt_tokens": 20, "total_tokens": 25, "completion_tokens": 5,
            "prompt_tokens_details": null,
        }));
        assert_eq!(extract_usage(&null), Some(UpstreamUsage::uncached(20, 5)));
        let absent = stream_tail(serde_json::json!({
            "prompt_tokens": 20, "total_tokens": 25, "completion_tokens": 5,
        }));
        assert_eq!(extract_usage(&absent), Some(UpstreamUsage::uncached(20, 5)));
        let no_count = stream_tail(serde_json::json!({
            "prompt_tokens": 20, "completion_tokens": 5,
            "prompt_tokens_details": { "audio_tokens": 0 },
        }));
        assert_eq!(
            extract_usage(&no_count),
            Some(UpstreamUsage::uncached(20, 5))
        );
    }

    #[test]
    fn stream_tail_cache_detail_comes_from_the_final_usage_object() {
        // Usage on every chunk (continuous usage stats): only the last chunk's
        // cache detail may pair with the last chunk's prompt count, even when
        // that last chunk reports none and an earlier one did.
        let early = serde_json::json!({
            "choices": [{ "index": 0, "delta": { "content": "x" } }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 1,
                       "prompt_tokens_details": { "cached_tokens": 8 } },
        });
        let last = serde_json::json!({
            "choices": [],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2, "prompt_tokens_details": null },
        });
        let tail = format!("data: {early}\n\ndata: {last}\n\ndata: [DONE]\n\n");
        assert_eq!(extract_usage(&tail), Some(UpstreamUsage::uncached(10, 2)));

        // Key order does not matter: details before the prompt count still
        // belong to the same object.
        let reordered = r#"data: {"choices":[],"usage":{"prompt_tokens_details":{"cached_tokens":3},"completion_tokens":2,"prompt_tokens":10}}"#;
        assert_eq!(extract_usage(reordered), Some(UpstreamUsage::new(10, 2, 3)));

        // A usage object cut off at the end of the tail reports no cache
        // detail rather than a guess, and keeps the old prompt reading.
        let cut = r#"data: {"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_tokens_details":{"cached_tok"#;
        assert_eq!(extract_usage(cut), Some(UpstreamUsage::uncached(10, 2)));
    }
}
