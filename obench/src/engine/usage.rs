use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

#[derive(Deserialize)]
struct UsageWire {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

#[derive(Deserialize)]
struct Envelope {
    usage: Option<UsageWire>,
}

fn parse_envelope(json: &str) -> Option<Usage> {
    let env: Envelope = serde_json::from_str(json).ok()?;
    let u = env.usage?;
    Some(Usage { prompt_tokens: u.prompt_tokens, completion_tokens: u.completion_tokens })
}

pub fn from_json(body: &str) -> Option<Usage> {
    parse_envelope(body)
}

/// Scan an SSE stream for the last `data:` chunk that carries a usage object.
/// `stream_options.include_usage` puts it in the final pre-[DONE] chunk.
pub fn from_sse(body: &str) -> Option<Usage> {
    let mut found = None;
    for line in body.lines() {
        let line = line.trim_start();
        let Some(payload) = line.strip_prefix("data:") else { continue };
        let payload = payload.trim();
        if payload == "[DONE]" || payload.is_empty() {
            continue;
        }
        if let Some(u) = parse_envelope(payload) {
            if u.prompt_tokens != 0 || u.completion_tokens != 0 {
                found = Some(u);
            }
        }
    }
    found
}

/// Rough fallback when upstream omits usage. ~4 chars per token.
pub fn estimate(input_tokens: u32, out_chars: usize) -> Usage {
    Usage {
        prompt_tokens: input_tokens as u64,
        completion_tokens: (out_chars as u64).div_ceil(4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buffered_usage() {
        let body = r#"{"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":34}}"#;
        assert_eq!(from_json(body), Some(Usage { prompt_tokens: 12, completion_tokens: 34 }));
    }

    #[test]
    fn sse_last_usage_chunk() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                    data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":7}}\n\n\
                    data: [DONE]\n\n";
        assert_eq!(from_sse(body), Some(Usage { prompt_tokens: 5, completion_tokens: 7 }));
    }

    #[test]
    fn sse_without_usage_returns_none() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\ndata: [DONE]\n\n";
        assert_eq!(from_sse(body), None);
    }

    #[test]
    fn estimate_rounds_up() {
        assert_eq!(estimate(256, 9), Usage { prompt_tokens: 256, completion_tokens: 3 });
    }
}
