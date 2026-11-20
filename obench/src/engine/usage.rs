use serde::Deserialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// What a chat completion said about its own size: the usage object when the
/// upstream sent one, and the length of the answer text for the estimate when
/// it did not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Reply {
    pub usage: Option<Usage>,
    pub content_chars: usize,
}

#[derive(Deserialize)]
struct UsageWire {
    #[serde(default)]
    prompt_tokens: u64,
    #[serde(default)]
    completion_tokens: u64,
}

/// One chat completion body or one streamed chunk; the fields obench reads.
#[derive(Deserialize)]
struct Envelope {
    #[serde(default)]
    choices: Vec<Choice>,
    usage: Option<UsageWire>,
}

#[derive(Deserialize)]
struct Choice {
    delta: Option<Text>,
    message: Option<Text>,
}

#[derive(Deserialize)]
struct Text {
    content: Option<String>,
}

impl Envelope {
    fn usage(&self) -> Option<Usage> {
        let u = self.usage.as_ref()?;
        (u.prompt_tokens != 0 || u.completion_tokens != 0).then_some(Usage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
        })
    }

    fn content_chars(&self) -> usize {
        self.choices
            .iter()
            .flat_map(|c| [c.delta.as_ref(), c.message.as_ref()])
            .flatten()
            .filter_map(|t| t.content.as_deref())
            .map(|s| s.chars().count())
            .sum()
    }
}

/// Read a buffered chat completion response body.
pub fn from_json(body: &str) -> Reply {
    match serde_json::from_str::<Envelope>(body) {
        Ok(env) => Reply {
            usage: env.usage(),
            content_chars: env.content_chars(),
        },
        Err(_) => Reply::default(),
    }
}

/// Reads an SSE chat completion stream as it arrives, one `data:` line at a
/// time, so only the current line is held in memory. The usage object is in
/// the last chunk before `[DONE]` when `stream_options.include_usage` is set.
#[derive(Default)]
pub struct SseReader {
    line: Vec<u8>,
    reply: Reply,
}

impl SseReader {
    pub fn feed(&mut self, bytes: &[u8]) {
        for b in bytes {
            if *b == b'\n' {
                let line = std::mem::take(&mut self.line);
                self.line_done(&String::from_utf8_lossy(&line));
            } else {
                self.line.push(*b);
            }
        }
    }

    pub fn finish(mut self) -> Reply {
        let line = std::mem::take(&mut self.line);
        self.line_done(&String::from_utf8_lossy(&line));
        self.reply
    }

    fn line_done(&mut self, line: &str) {
        let Some(payload) = line.trim_start().strip_prefix("data:") else {
            return;
        };
        let payload = payload.trim();
        if payload == "[DONE]" || payload.is_empty() {
            return;
        }
        let Ok(env) = serde_json::from_str::<Envelope>(payload) else {
            return;
        };
        if let Some(u) = env.usage() {
            self.reply.usage = Some(u);
        }
        self.reply.content_chars += env.content_chars();
    }
}

/// Rough fallback when upstream omits usage. ~4 chars of answer text per token.
pub fn estimate(input_tokens: u32, content_chars: usize) -> Usage {
    Usage {
        prompt_tokens: input_tokens as u64,
        completion_tokens: (content_chars as u64).div_ceil(4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(body: &str) -> Reply {
        let mut r = SseReader::default();
        r.feed(body.as_bytes());
        r.finish()
    }

    #[test]
    fn buffered_usage_and_answer_length() {
        let body = r#"{"choices":[{"message":{"role":"assistant","content":"héllo"}}],"usage":{"prompt_tokens":12,"completion_tokens":34}}"#;
        assert_eq!(
            from_json(body),
            Reply {
                usage: Some(Usage {
                    prompt_tokens: 12,
                    completion_tokens: 34
                }),
                content_chars: 5,
            }
        );
        assert_eq!(from_json("not json"), Reply::default());
    }

    #[test]
    fn sse_last_usage_chunk() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"hi\"}}]}\n\n\
                    data: {\"choices\":[],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":7}}\n\n\
                    data: [DONE]\n\n";
        assert_eq!(
            read(body),
            Reply {
                usage: Some(Usage {
                    prompt_tokens: 5,
                    completion_tokens: 7
                }),
                content_chars: 2,
            }
        );
    }

    #[test]
    fn sse_without_usage_counts_only_the_answer_text() {
        let chunk = "data: {\"id\":\"chatcmpl-bench\",\"object\":\"chat.completion.chunk\",\"model\":\"m\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lorem \"},\"finish_reason\":null}]}\n\n";
        let body = format!(
            "data: {{\"choices\":[{{\"delta\":{{\"role\":\"assistant\"}}}}]}}\n\n{}data: [DONE]\n\n",
            chunk.repeat(100)
        );
        let reply = read(&body);
        assert_eq!(reply.usage, None);
        assert_eq!(reply.content_chars, 600);
        assert_eq!(estimate(8, reply.content_chars).completion_tokens, 150);
        // The framing is over 20x the answer text; it must not be counted.
        assert!(body.len() > 20 * reply.content_chars);
    }

    #[test]
    fn sse_lines_split_across_network_chunks_are_read_whole() {
        let body = "data: {\"choices\":[{\"delta\":{\"content\":\"abcdef\"}}]}\ndata: {\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":2}}\ndata: [DONE]";
        let mut r = SseReader::default();
        for piece in body.as_bytes().chunks(7) {
            r.feed(piece);
        }
        assert_eq!(
            r.finish(),
            Reply {
                usage: Some(Usage {
                    prompt_tokens: 1,
                    completion_tokens: 2
                }),
                content_chars: 6,
            }
        );
    }

    #[test]
    fn estimate_rounds_up() {
        assert_eq!(
            estimate(256, 9),
            Usage {
                prompt_tokens: 256,
                completion_tokens: 3
            }
        );
    }
}
