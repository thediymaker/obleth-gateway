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
            Err(e) => return RequestOutcome {
                status: 0, ttfb_ms: 0, total_ms: start.elapsed().as_millis() as u64,
                in_tokens: 0, out_tokens: 0, usage_estimated: false,
            }.with_error(&e.to_string()),
        };

        let status = resp.status().as_u16();
        let mut ttfb_ms = 0u64;
        let mut text = String::new();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(bytes) => {
                    if ttfb_ms == 0 { ttfb_ms = start.elapsed().as_millis() as u64; }
                    text.push_str(&String::from_utf8_lossy(&bytes));
                }
                Err(_) => break,
            }
        }
        let total_ms = start.elapsed().as_millis() as u64;
        if ttfb_ms == 0 { ttfb_ms = total_ms; }

        if status != 200 {
            return RequestOutcome { status, ttfb_ms, total_ms, in_tokens: 0, out_tokens: 0, usage_estimated: false };
        }

        let parsed = if req.stream { usage::from_sse(&text) } else { usage::from_json(&text) };
        let (u, estimated) = match parsed {
            Some(u) => (u, false),
            None => (usage::estimate(req.input_tokens, text.len()), true),
        };
        RequestOutcome {
            status, ttfb_ms, total_ms,
            in_tokens: u.prompt_tokens, out_tokens: u.completion_tokens,
            usage_estimated: estimated,
        }
    }
}

impl RequestOutcome {
    fn with_error(self, _msg: &str) -> RequestOutcome {
        // status 0 already marks a transport error; msg is for future logging.
        self
    }
}

pub async fn run_closed_loop<F>(
    client: Arc<LoadClient>,
    make_req: F,
    cfg: RunConfig,
    stop: Arc<AtomicBool>,
    stats: Arc<Mutex<Stats>>,
) where
    F: Fn() -> ChatRequest + Send + Sync + 'static,
{
    let make_req = Arc::new(make_req);
    let started = Instant::now();
    let warmup = Duration::from_secs(cfg.warmup_s);
    let deadline = if cfg.duration_s == 0 { None } else { Some(Duration::from_secs(cfg.duration_s + cfg.warmup_s)) };

    let mut handles = Vec::new();
    for _ in 0..cfg.conc {
        let client = client.clone();
        let make_req = make_req.clone();
        let stop = stop.clone();
        let stats = stats.clone();
        handles.push(tokio::spawn(async move {
            loop {
                if stop.load(Ordering::Relaxed) { break; }
                if let Some(d) = deadline { if started.elapsed() >= d { break; } }
                let req = make_req();
                let outcome = client.chat(&req).await;
                if started.elapsed() >= warmup {
                    stats.lock().unwrap().record(&outcome);
                }
            }
        }));
    }
    for h in handles { let _ = h.await; }
}
