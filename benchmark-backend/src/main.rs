//! Benchmark vLLM / OpenAI-compatible backend for load-testing obleth without GPUs.
//!
//! Streams SSE chat completions with configurable time-to-first-token and
//! inter-token latency, and reports `usage` in the final chunk so the gateway
//! can reconcile real cost. A global concurrency limit simulates a saturated
//! cluster (extra requests queue, raising latency) so fairshare behavior is
//! observable end-to-end.
//!
//! `GET /stats` returns process-wide request/token counters so callers can
//! measure exactly how many requests reached the backend (e.g. to quantify
//! gateway cache offload) rather than inferring it from container metrics.
//!
//! Env knobs:
//!   BENCHMARK_BACKEND_LISTEN          (default 0.0.0.0:8081)
//!   BENCHMARK_BACKEND_TTFT_MS         time to first token (default 20)
//!   BENCHMARK_BACKEND_TOKEN_MS        per-token delay (default 5)
//!   BENCHMARK_BACKEND_DEFAULT_OUTPUT  output tokens when max_tokens absent (default 64)
//!   BENCHMARK_BACKEND_CONCURRENCY     simulated server-wide slot count (default 10000)
//!
//! The legacy `MOCK_*` names are still accepted for local compatibility.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use tokio::sync::Semaphore;

/// Process-wide request counters. These exist so the true number of requests
/// that actually reached the backend can be compared against gateway/bench
/// counts - making cache offload (and any admission/routing drop) measurable
/// instead of guessed from `docker stats`.
#[derive(Default)]
struct Counters {
    requests: AtomicU64,
    streaming: AtomicU64,
    non_streaming: AtomicU64,
    prompt_tokens: AtomicU64,
    completion_tokens: AtomicU64,
}

#[derive(Clone)]
struct Cfg {
    ttft_ms: u64,
    token_ms: u64,
    default_output: u32,
    slots: Arc<Semaphore>,
    counters: Arc<Counters>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let cfg = Cfg {
        ttft_ms: env_u64("BENCHMARK_BACKEND_TTFT_MS", "MOCK_TTFT_MS", 20),
        token_ms: env_u64("BENCHMARK_BACKEND_TOKEN_MS", "MOCK_TOKEN_MS", 5),
        default_output: env_u64(
            "BENCHMARK_BACKEND_DEFAULT_OUTPUT",
            "MOCK_DEFAULT_OUTPUT",
            64,
        ) as u32,
        slots: Arc::new(Semaphore::new(env_u64(
            "BENCHMARK_BACKEND_CONCURRENCY",
            "MOCK_CONCURRENCY",
            10_000,
        ) as usize)),
        counters: Arc::new(Counters::default()),
    };

    let app = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/stats", get(stats))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/completions", post(chat))
        .with_state(cfg);

    let listen = env_string("BENCHMARK_BACKEND_LISTEN", "MOCK_LISTEN", "0.0.0.0:8081");
    let listener = tokio::net::TcpListener::bind(&listen).await.unwrap();
    tracing::info!("benchmark fixture backend listening on {listen}");
    axum::serve(listener, app).await.unwrap();
}

/// Snapshot of the backend's request counters. The gateway/bench can diff
/// `requests` against the number of requests they issued to quantify cache
/// offload (or any pre-upstream drop) precisely.
async fn stats(State(cfg): State<Cfg>) -> Json<Value> {
    let c = &cfg.counters;
    Json(json!({
        "requests": c.requests.load(Ordering::Relaxed),
        "streaming": c.streaming.load(Ordering::Relaxed),
        "non_streaming": c.non_streaming.load(Ordering::Relaxed),
        "prompt_tokens": c.prompt_tokens.load(Ordering::Relaxed),
        "completion_tokens": c.completion_tokens.load(Ordering::Relaxed),
    }))
}

async fn chat(State(cfg): State<Cfg>, Json(body): Json<Value>) -> Response {
    let model = body
        .get("model")
        .and_then(Value::as_str)
        .unwrap_or("benchmark-endpoint")
        .to_string();
    let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
    let prompt_tokens = estimate_prompt_tokens(&body);
    let output_tokens = body
        .get("max_tokens")
        .or_else(|| body.get("max_completion_tokens"))
        .and_then(Value::as_u64)
        .map(|m| m as u32)
        .unwrap_or(cfg.default_output)
        .clamp(1, 4096);

    // Count every request that actually reaches this backend, before any slot
    // wait, so the tally reflects true upstream load.
    cfg.counters.requests.fetch_add(1, Ordering::Relaxed);
    if stream {
        cfg.counters.streaming.fetch_add(1, Ordering::Relaxed);
    } else {
        cfg.counters.non_streaming.fetch_add(1, Ordering::Relaxed);
    }
    cfg.counters
        .prompt_tokens
        .fetch_add(prompt_tokens as u64, Ordering::Relaxed);
    cfg.counters
        .completion_tokens
        .fetch_add(output_tokens as u64, Ordering::Relaxed);

    // Acquire a simulated GPU slot; when none free, this awaits -> models saturation.
    let permit = cfg.slots.clone().acquire_owned().await.ok();

    if stream {
        stream_response(cfg, model, prompt_tokens, output_tokens, permit).await
    } else {
        json_response(cfg, model, prompt_tokens, output_tokens, permit).await
    }
}

async fn json_response(
    cfg: Cfg,
    model: String,
    prompt_tokens: u32,
    output_tokens: u32,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Response {
    tokio::time::sleep(Duration::from_millis(
        cfg.ttft_ms + cfg.token_ms * output_tokens as u64,
    ))
    .await;
    drop(permit);
    let text = "lorem ipsum ".repeat(output_tokens as usize / 2);
    let payload = json!({
        "id": "chatcmpl-bench",
        "object": "chat.completion",
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": text},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": prompt_tokens,
            "completion_tokens": output_tokens,
            "total_tokens": prompt_tokens + output_tokens
        }
    });
    (StatusCode::OK, Json(payload)).into_response()
}

async fn stream_response(
    cfg: Cfg,
    model: String,
    prompt_tokens: u32,
    output_tokens: u32,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Response {
    let body = async_stream::stream! {
        // hold the slot for the whole stream
        let _permit = permit;
        tokio::time::sleep(Duration::from_millis(cfg.ttft_ms)).await;

        let role = json!({
            "id": "chatcmpl-bench", "object": "chat.completion.chunk", "model": model,
            "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]
        });
        yield sse(&role);

        for _ in 0..output_tokens {
            tokio::time::sleep(Duration::from_millis(cfg.token_ms)).await;
            let chunk = json!({
                "id": "chatcmpl-bench", "object": "chat.completion.chunk", "model": model,
                "choices": [{"index": 0, "delta": {"content": "lorem "}, "finish_reason": null}]
            });
            yield sse(&chunk);
        }

        let final_chunk = json!({
            "id": "chatcmpl-bench", "object": "chat.completion.chunk", "model": model,
            "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": prompt_tokens,
                "completion_tokens": output_tokens,
                "total_tokens": prompt_tokens + output_tokens
            }
        });
        yield sse(&final_chunk);
        yield Ok::<_, std::io::Error>(axum::body::Bytes::from_static(b"data: [DONE]\n\n"));
    };

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(axum::body::Body::from_stream(body))
        .unwrap()
}

fn sse(value: &Value) -> Result<axum::body::Bytes, std::io::Error> {
    Ok(axum::body::Bytes::from(format!("data: {value}\n\n")))
}

fn estimate_prompt_tokens(body: &Value) -> u32 {
    let mut chars = 0usize;
    if let Some(messages) = body.get("messages").and_then(Value::as_array) {
        for m in messages {
            if let Some(s) = m.get("content").and_then(Value::as_str) {
                chars += s.chars().count();
            }
        }
    } else if let Some(s) = body.get("prompt").and_then(Value::as_str) {
        chars = s.chars().count();
    }
    ((chars / 4) as u32).max(1)
}

fn env_u64(primary: &str, legacy: &str, default: u64) -> u64 {
    std::env::var(primary)
        .or_else(|_| std::env::var(legacy))
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn env_string(primary: &str, legacy: &str, default: &str) -> String {
    std::env::var(primary)
        .or_else(|_| std::env::var(legacy))
        .unwrap_or_else(|_| default.to_string())
}
