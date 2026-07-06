use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;

use crate::engine::stats::{RequestOutcome, Stats};
use crate::engine::usage;

pub struct LoadClient {
    http: reqwest::Client,
}

#[derive(Clone)]
pub struct ChatRequest {
    pub proxy_base: String,
    pub key: String,
    pub model: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub stream: bool,
}

/// A standalone embeddings request routed to `/v1/embeddings`.
/// `output_tokens` is intentionally absent: embeddings return no completion tokens.
#[derive(Clone)]
pub struct EmbedRequest {
    pub proxy_base: String,
    pub key: String,
    pub model: String,
    pub input_tokens: u32,
}

/// Unified request descriptor. Engine dispatches on the variant; profile/target
/// concepts never leak into the engine layer.
#[derive(Clone)]
pub enum ProxyRequest {
    Chat(ChatRequest),
    Embed(EmbedRequest),
}

#[derive(Clone, Copy)]
pub struct RunConfig {
    pub conc: u32,
    pub duration_s: u64,
    pub warmup_s: u64,
}

impl LoadClient {
    pub fn new(pool_max: usize) -> Self {
        let http = reqwest::Client::builder()
            .pool_max_idle_per_host(pool_max)
            .tcp_nodelay(true)
            .build()
            .expect("reqwest client");
        Self { http }
    }

    /// Dispatch a `ProxyRequest` to the appropriate endpoint.
    pub async fn dispatch(&self, req: &ProxyRequest) -> RequestOutcome {
        match req {
            ProxyRequest::Chat(r) => self.chat(r).await,
            ProxyRequest::Embed(r) => self.embeddings(r).await,
        }
    }

    /// POST `/v1/embeddings`. Records `prompt_tokens`; `out_tokens` is always 0.
    pub async fn embeddings(&self, req: &EmbedRequest) -> RequestOutcome {
        // Pad input to ~input_tokens (~4 chars/token).
        let input = "obench ".repeat((req.input_tokens as usize * 4 / 7).max(1));
        let body = serde_json::json!({
            "model": req.model,
            "input": input,
        });

        let start = Instant::now();
        let send = self
            .http
            .post(format!("{}/v1/embeddings", req.proxy_base))
            .bearer_auth(&req.key)
            .json(&body)
            .send()
            .await;

        let resp = match send {
            Ok(r) => r,
            Err(e) => {
                return RequestOutcome {
                    status: 0,
                    ttfb_ms: 0,
                    total_ms: start.elapsed().as_millis() as u64,
                    in_tokens: 0,
                    out_tokens: 0,
                    usage_estimated: false,
                    gaps_ms: Vec::new(),
                }
                .with_error(&e.to_string())
            }
        };

        let status = resp.status().as_u16();
        let ttfb_ms = start.elapsed().as_millis() as u64;
        let text = resp.text().await.unwrap_or_default();
        let total_ms = start.elapsed().as_millis() as u64;

        if status != 200 {
            return RequestOutcome {
                status,
                ttfb_ms,
                total_ms,
                in_tokens: 0,
                out_tokens: 0,
                usage_estimated: false,
                gaps_ms: Vec::new(),
            };
        }

        // Parse `usage.prompt_tokens` from the embeddings response.
        let (prompt_tokens, estimated) = parse_embed_usage(&text, req.input_tokens);
        RequestOutcome {
            status,
            ttfb_ms,
            total_ms,
            in_tokens: prompt_tokens,
            out_tokens: 0,
            usage_estimated: estimated,
            gaps_ms: Vec::new(),
        }
    }

    pub async fn chat(&self, req: &ChatRequest) -> RequestOutcome {
        // Pad the prompt to ~input_tokens (~4 chars/token).
        let prompt = "obench ".repeat((req.input_tokens as usize * 4 / 7).max(1));
        let mut body = serde_json::json!({
            "model": req.model,
            "messages": [
                { "role": "system", "content": "obench load. Answer concisely." },
                { "role": "user", "content": prompt }
            ],
            "max_tokens": req.output_tokens,
            "stream": req.stream,
        });
        if req.stream {
            body["stream_options"] = serde_json::json!({ "include_usage": true });
        }

        let start = Instant::now();
        let send = self
            .http
            .post(format!("{}/v1/chat/completions", req.proxy_base))
            .bearer_auth(&req.key)
            .json(&body)
            .send()
            .await;

        let resp = match send {
            Ok(r) => r,
            Err(e) => {
                return RequestOutcome {
                    status: 0,
                    ttfb_ms: 0,
                    total_ms: start.elapsed().as_millis() as u64,
                    in_tokens: 0,
                    out_tokens: 0,
                    usage_estimated: false,
                    gaps_ms: Vec::new(),
                }
                .with_error(&e.to_string())
            }
        };

        let status = resp.status().as_u16();
        let mut ttfb_ms = 0u64;
        let mut text = String::new();
        let mut gaps_ms: Vec<u64> = Vec::new();
        let mut last_chunk_at: Option<Instant> = None;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    let now = Instant::now();
                    if ttfb_ms == 0 {
                        ttfb_ms = start.elapsed().as_millis() as u64;
                    } else if req.stream {
                        if let Some(prev) = last_chunk_at {
                            gaps_ms.push(now.duration_since(prev).as_millis() as u64);
                        }
                    }
                    last_chunk_at = Some(now);
                    text.push_str(&String::from_utf8_lossy(&bytes));
                }
                Err(_) => break,
            }
        }
        let total_ms = start.elapsed().as_millis() as u64;
        if ttfb_ms == 0 {
            ttfb_ms = total_ms;
        }

        if status != 200 {
            return RequestOutcome {
                status,
                ttfb_ms,
                total_ms,
                in_tokens: 0,
                out_tokens: 0,
                usage_estimated: false,
                gaps_ms: Vec::new(),
            };
        }

        let parsed = if req.stream {
            usage::from_sse(&text)
        } else {
            usage::from_json(&text)
        };
        let (u, estimated) = match parsed {
            Some(u) => (u, false),
            None => (usage::estimate(req.input_tokens, text.len()), true),
        };
        RequestOutcome {
            status,
            ttfb_ms,
            total_ms,
            in_tokens: u.prompt_tokens,
            out_tokens: u.completion_tokens,
            usage_estimated: estimated,
            gaps_ms,
        }
    }
}

impl RequestOutcome {
    fn with_error(self, _msg: &str) -> RequestOutcome {
        // status 0 already marks a transport error; msg is for future logging.
        self
    }
}

/// Parse `usage.prompt_tokens` from an embeddings JSON response body.
/// Returns `(prompt_tokens, estimated)`. Falls back to `input_tokens` if parsing fails.
fn parse_embed_usage(body: &str, input_tokens: u32) -> (u64, bool) {
    #[derive(serde::Deserialize)]
    struct EmbedUsage {
        #[serde(default)]
        prompt_tokens: u64,
    }
    #[derive(serde::Deserialize)]
    struct EmbedEnv {
        usage: Option<EmbedUsage>,
    }
    if let Ok(env) = serde_json::from_str::<EmbedEnv>(body) {
        if let Some(u) = env.usage {
            if u.prompt_tokens > 0 {
                return (u.prompt_tokens, false);
            }
        }
    }
    (input_tokens as u64, true)
}

pub async fn run_closed_loop<F>(
    client: Arc<LoadClient>,
    make_req: F,
    cfg: RunConfig,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<Stats>>,
) where
    F: Fn() -> ProxyRequest + Send + Sync + 'static,
{
    let make_req = Arc::new(make_req);
    let started = Instant::now();
    let warmup = Duration::from_secs(cfg.warmup_s);
    let deadline = if cfg.duration_s == 0 {
        None
    } else {
        Some(Duration::from_secs(cfg.duration_s + cfg.warmup_s))
    };

    let mut handles = Vec::new();
    for _ in 0..cfg.conc {
        let client = client.clone();
        let make_req = make_req.clone();
        let stop = stop.clone();
        let stats = stats.clone();
        handles.push(tokio::spawn(async move {
            loop {
                if stop.load(Ordering::Relaxed) {
                    break;
                }
                if let Some(d) = deadline {
                    if started.elapsed() >= d {
                        break;
                    }
                }
                let req = make_req();
                let outcome = client.dispatch(&req).await;
                if started.elapsed() >= warmup {
                    stats.lock().unwrap().record(&outcome);
                }
            }
        }));
    }
    for h in handles {
        let _ = h.await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_usage_parsed_from_response() {
        let body = r#"{"object":"list","data":[],"model":"obench-embed","usage":{"prompt_tokens":42,"total_tokens":42}}"#;
        let (tokens, estimated) = parse_embed_usage(body, 10);
        assert_eq!(tokens, 42);
        assert!(!estimated);
    }

    #[test]
    fn embed_usage_falls_back_to_input_tokens() {
        // No usage field → fall back to input_tokens estimate.
        let body = r#"{"object":"list","data":[]}"#;
        let (tokens, estimated) = parse_embed_usage(body, 17);
        assert_eq!(tokens, 17);
        assert!(estimated);
    }

    #[test]
    fn embed_usage_falls_back_when_zero() {
        // usage present but prompt_tokens is 0 → treat as missing.
        let body = r#"{"usage":{"prompt_tokens":0,"total_tokens":0}}"#;
        let (tokens, estimated) = parse_embed_usage(body, 5);
        assert_eq!(tokens, 5);
        assert!(estimated);
    }
}
