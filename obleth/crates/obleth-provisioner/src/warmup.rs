//! Post-promotion warmup inference.
//!
//! A replica is promoted to `healthy` the moment its `/health` endpoint returns
//! 200 — but on inference servers (vLLM, SGLang, …) `/health` goes green when
//! the HTTP server is up, *before* the first real forward pass triggers CUDA
//! graph capture, lazy weight paging, and KV-cache warmup. That first request
//! can take many seconds to its first token on a cold replica (e.g. one freshly
//! resubmitted after a preemption), which is long enough to trip the proxy's
//! time-to-first-token timeout and surface to a user as a 502/504.
//!
//! This module pays that cost up front, off the hot path: right after a replica
//! is promoted, fire one throwaway 1-token completion so the cold first-token
//! work happens against the gateway instead of the first real user.
//!
//! Best-effort by design: any failure is logged by the caller and never affects
//! promotion — the replica is already healthy and registered.

use anyhow::Context;
use std::time::Duration;

/// Warm a freshly-promoted replica by firing a single 1-token inference at it.
///
/// `api_base` is the OpenAI root the planner registered for the endpoint
/// (e.g. `http://gpu7:8000/v1`). We first `GET {api_base}/models` to learn the
/// served model id (avoids having to parse it out of the launch command), then
/// `POST {api_base}/chat/completions` with `max_tokens: 1` to force a real
/// forward pass. `budget` bounds each HTTP call — set it generously, since the
/// whole point is to absorb a slow cold first token.
///
/// Returns `Err` on any failure so the caller can log it; never panics.
pub async fn warm_up(
    http: &reqwest::Client,
    api_base: &str,
    budget: Duration,
) -> anyhow::Result<()> {
    let base = api_base.trim_end_matches('/');

    // 1. Discover the served model id. Inference servers expose exactly the
    //    model(s) they are serving here; we warm the first one.
    let models_url = format!("{base}/models");
    let models: serde_json::Value = http
        .get(&models_url)
        .timeout(budget)
        .send()
        .await
        .context("warmup: GET /models failed")?
        .error_for_status()
        .context("warmup: /models returned an error status")?
        .json()
        .await
        .context("warmup: /models body was not JSON")?;
    let model_id = models
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|a| a.first())
        .and_then(|m| m.get("id"))
        .and_then(|x| x.as_str())
        .context("warmup: /models response had no data[0].id")?;

    // 2. Fire a minimal completion. `max_tokens: 1` keeps the generation
    //    trivial; the value is in forcing the first forward pass, not the output.
    let chat_url = format!("{base}/chat/completions");
    let body = serde_json::json!({
        "model": model_id,
        "messages": [{ "role": "user", "content": "warmup" }],
        "max_tokens": 1,
        "temperature": 0,
        "stream": false,
    });
    http.post(&chat_url)
        .timeout(budget)
        .json(&body)
        .send()
        .await
        .context("warmup: POST /chat/completions failed")?
        .error_for_status()
        .context("warmup: /chat/completions returned an error status")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// A throwaway HTTP/1.1 server that answers each request based on its path
    /// and records which paths it was hit on. Each connection is answered with
    /// `Connection: close` so we never have to parse keep-alive framing — reqwest
    /// simply opens a fresh connection for the second call.
    ///
    /// `chat_status` lets a test make `/chat/completions` fail; `models_ok`
    /// lets a test make `/models` fail. Returns the bound base URL (with `/v1`)
    /// and the shared hit log.
    async fn mock_server(models_ok: bool, chat_status: u16) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let hits_task = hits.clone();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(c) => c,
                    Err(_) => break,
                };
                let hits = hits_task.clone();
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let head = String::from_utf8_lossy(&buf[..n]);
                    let path = head
                        .lines()
                        .next()
                        .and_then(|l| l.split_whitespace().nth(1))
                        .unwrap_or("")
                        .to_string();
                    hits.lock().unwrap().push(path.clone());
                    let (status, json): (u16, String) = if path.ends_with("/models") {
                        if models_ok {
                            (200, r#"{"data":[{"id":"served-model"}]}"#.to_string())
                        } else {
                            (500, r#"{"error":"boom"}"#.to_string())
                        }
                    } else {
                        (
                            chat_status,
                            r#"{"choices":[{"message":{"content":"x"}}]}"#.to_string(),
                        )
                    };
                    let resp = format!(
                        "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
                        json.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });
        (format!("http://{addr}/v1"), hits)
    }

    #[tokio::test]
    async fn warm_up_discovers_model_then_fires_chat_completion() {
        let (base, hits) = mock_server(true, 200).await;
        let http = reqwest::Client::new();
        warm_up(&http, &base, Duration::from_secs(5))
            .await
            .expect("warmup should succeed against a healthy server");
        let hits = hits.lock().unwrap();
        // It must both discover the model AND fire a real inference — a plain
        // health ping (the thing this feature exists to go beyond) would only
        // ever hit one of these.
        assert!(
            hits.iter().any(|p| p.ends_with("/models")),
            "expected a /models discovery call; got {hits:?}"
        );
        assert!(
            hits.iter().any(|p| p.ends_with("/chat/completions")),
            "expected a /chat/completions warmup call; got {hits:?}"
        );
    }

    #[tokio::test]
    async fn warm_up_trailing_slash_base_is_normalized() {
        let (base, hits) = mock_server(true, 200).await;
        let http = reqwest::Client::new();
        // A base with a trailing slash must not produce `//models`.
        warm_up(&http, &format!("{base}/"), Duration::from_secs(5))
            .await
            .expect("warmup should tolerate a trailing slash");
        let hits = hits.lock().unwrap();
        assert!(
            hits.iter().any(|p| p.ends_with("/v1/models")),
            "expected a clean /v1/models path; got {hits:?}"
        );
    }

    #[tokio::test]
    async fn warm_up_errors_when_models_discovery_fails_and_skips_chat() {
        let (base, hits) = mock_server(false, 200).await;
        let http = reqwest::Client::new();
        let err = warm_up(&http, &base, Duration::from_secs(5)).await;
        assert!(err.is_err(), "a failing /models must surface an error");
        let hits = hits.lock().unwrap();
        assert!(
            !hits.iter().any(|p| p.ends_with("/chat/completions")),
            "must not fire a completion when discovery failed; got {hits:?}"
        );
    }

    #[tokio::test]
    async fn warm_up_errors_when_completion_returns_error_status() {
        let (base, _hits) = mock_server(true, 500).await;
        let http = reqwest::Client::new();
        let err = warm_up(&http, &base, Duration::from_secs(5)).await;
        assert!(
            err.is_err(),
            "a 5xx from /chat/completions must surface an error"
        );
    }

    #[tokio::test]
    async fn warm_up_errors_on_unreachable_host_within_budget() {
        let http = reqwest::Client::new();
        // Port 1 has no listener: connection refused, fast — must error, not hang.
        let err = warm_up(&http, "http://127.0.0.1:1/v1", Duration::from_millis(500)).await;
        assert!(
            err.is_err(),
            "an unreachable upstream must surface an error"
        );
    }
}
