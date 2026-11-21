//! The helper-model client: one chat call to a helper model (vision
//! describer, structured-output fixer, guardrails judge, tool-loop turn),
//! bounded in time and in the size of what it reads, sent to the endpoint the
//! proxy's own dispatch would pick.

use std::time::Duration;

use obleth_config::ResolvedModel;
use serde_json::Value;

use super::billing::reported_usage;
use crate::proxy::UpstreamUsage;
use crate::state::AppState;

/// Result of one helper-model chat call: the reply text plus the token usage
/// the helper reported (zero when it did not return a `usage` object).
#[derive(Debug)]
pub(crate) struct ChatCallResult {
    pub text: String,
    /// What the helper reported, cached prompt tokens included, for its ledger row.
    pub usage: UpstreamUsage,
}

/// A helper call that did not answer within its bound. Typed, so a caller
/// classes it with `downcast_ref` instead of matching the message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HelperTimeout {
    pub what: String,
    pub after: Duration,
}

impl std::fmt::Display for HelperTimeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} timed out after {:?}", self.what, self.after)
    }
}

impl std::error::Error for HelperTimeout {}

impl HelperTimeout {
    /// Whether `error` is a helper call's timeout, anywhere in its chain.
    pub(crate) fn is(error: &anyhow::Error) -> bool {
        error
            .chain()
            .any(|e| e.downcast_ref::<HelperTimeout>().is_some())
    }
}

/// Run a helper call under `after`; a call that outlasts it fails with a
/// [`HelperTimeout`] naming `what`.
pub(crate) async fn with_timeout<T>(
    what: impl Into<String>,
    after: Duration,
    fut: impl std::future::Future<Output = anyhow::Result<T>>,
) -> anyhow::Result<T> {
    match tokio::time::timeout(after, fut).await {
        Ok(result) => result,
        Err(_) => Err(HelperTimeout {
            what: what.into(),
            after,
        }
        .into()),
    }
}

/// POST `body` as JSON to `url`, with `headers` and the bearer `api_key` when
/// there is one, and read a success reply of at most `max` bytes. An error
/// status, or a reply over the cap, is an error; nothing is truncated.
pub(crate) async fn post_json_capped(
    http: &reqwest::Client,
    url: String,
    headers: &reqwest::header::HeaderMap,
    api_key: Option<&str>,
    body: &Value,
    max: usize,
) -> anyhow::Result<bytes::Bytes> {
    let mut req = http.post(url).headers(headers.clone()).json(body);
    if let Some(api_key) = api_key {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("upstream returned {}", resp.status());
    }
    read_body_capped(resp, max).await
}

/// Largest helper reply body read into memory. A helper reply is one chat
/// completion (or one MCP JSON-RPC message); anything bigger is a misbehaving
/// upstream, and reading it unbounded would let it exhaust gateway memory.
pub(crate) const HELPER_BODY_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Where a helper call is sent: the same endpoint selection the proxy's own
/// dispatch uses ([`crate::proxy::build_targets`]), so a model that lives only
/// in `endpoints` (every Slurm-provisioned model has a blank `api_base`) is
/// reachable, unhealthy endpoints are skipped, and a per-endpoint key wins
/// over the model's.
///
/// `pinned` is a URL the model already answered on this request (the tool
/// loop's turn 0); the target whose base prefixes it is chosen so follow-up
/// turns land on the replica holding the conversation's prefix cache. Falls
/// back to the first target when nothing matches.
pub(crate) fn helper_target(
    model: &ResolvedModel,
    session_key: &str,
    pinned: Option<&str>,
) -> anyhow::Result<crate::proxy::Target> {
    let mut targets =
        crate::proxy::build_targets(Some(model), "", &model.endpoint_selection_mode, session_key);
    let pick = pinned
        .and_then(|url| {
            targets.iter().position(|t| {
                // Whole path segments only: `http://h:8000/v1` must not
                // claim a URL served by `http://h:8000/v10`.
                let base = t.base.trim_end_matches('/');
                !base.is_empty()
                    && url.strip_prefix(base).is_some_and(|rest| {
                        rest.is_empty() || rest.starts_with('/') || rest.starts_with('?')
                    })
            })
        })
        .unwrap_or(0);
    if pick >= targets.len() {
        anyhow::bail!("model `{}` has no upstream target", model.model_name);
    }
    let target = targets.swap_remove(pick);
    if target.base.trim().is_empty() {
        anyhow::bail!(
            "model `{}` has no usable upstream endpoint",
            model.model_name
        );
    }
    Ok(target)
}

/// Read a response body, refusing anything larger than `max` bytes. The
/// declared `content-length` is checked first so an honest oversized reply is
/// rejected without reading it; chunked bodies are cut off as they cross.
pub(crate) async fn read_body_capped(
    mut resp: reqwest::Response,
    max: usize,
) -> anyhow::Result<bytes::Bytes> {
    if resp.content_length().is_some_and(|n| n > max as u64) {
        anyhow::bail!("upstream body exceeds {max} bytes");
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len().saturating_add(chunk.len()) > max {
            anyhow::bail!("upstream body exceeds {max} bytes");
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(bytes::Bytes::from(buf))
}

/// POST one non-streaming chat completion to `target` and return the parsed
/// JSON body (size-capped). No timeout of its own; callers bound it.
async fn post_chat_completion(
    http: &reqwest::Client,
    target: &crate::proxy::Target,
    body: &Value,
) -> anyhow::Result<Value> {
    let bytes = post_json_capped(
        http,
        build_chat_url(&target.base),
        &target.headers,
        target.api_key.as_deref(),
        body,
        HELPER_BODY_MAX_BYTES,
    )
    .await?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Send a chat-completions request to a helper model (describer, fixer) and
/// return its reply text, bounded by `timeout`.
pub(crate) async fn chat_call(
    state: &AppState,
    helper: &ResolvedModel,
    body: Value,
    timeout: Duration,
) -> anyhow::Result<ChatCallResult> {
    chat_call_with(&state.http, helper, body, timeout).await
}

async fn chat_call_with(
    http: &reqwest::Client,
    helper: &ResolvedModel,
    body: Value,
    timeout: Duration,
) -> anyhow::Result<ChatCallResult> {
    let fut = async {
        let target = helper_target(helper, "", None)?;
        let body = post_chat_completion(http, &target, &body).await?;
        let text = body
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if text.trim().is_empty() {
            anyhow::bail!("helper returned an empty reply");
        }
        Ok(ChatCallResult {
            text,
            usage: reported_usage(&body),
        })
    };
    with_timeout("helper call", timeout, fut).await
}

/// Send a chat-completions request to a model and return the **complete**
/// completion JSON, bounded by `timeout`. Used by the gateway tool loop, which
/// needs the full message (including `tool_calls`) and usage, not just text.
pub(crate) async fn chat_call_completion(
    state: &AppState,
    model: &ResolvedModel,
    body: &Value,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let target = helper_target(model, "", None)?;
    chat_call_completion_on(&state.http, &target, body, timeout).await
}

/// [`chat_call_completion`] against an already-resolved target, for callers
/// that must keep every call on one endpoint (the tool loop).
pub(crate) async fn chat_call_completion_on(
    http: &reqwest::Client,
    target: &crate::proxy::Target,
    body: &Value,
    timeout: Duration,
) -> anyhow::Result<Value> {
    with_timeout(
        "chat call",
        timeout,
        post_chat_completion(http, target, body),
    )
    .await
}

/// The chat completions URL under an OpenAI-style base.
pub(crate) fn build_chat_url(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    format!("{base}/chat/completions")
}

#[cfg(test)]
mod tests {
    use super::super::tests::{chat_server, endpoint, endpoint_only_route};
    use super::*;

    #[test]
    fn chat_url_is_normalised() {
        assert_eq!(
            build_chat_url("http://host:8080/v1/"),
            "http://host:8080/v1/chat/completions"
        );
        assert_eq!(
            build_chat_url("http://host:8080/v1"),
            "http://host:8080/v1/chat/completions"
        );
    }

    #[test]
    fn helper_target_uses_the_healthy_endpoint_of_an_endpoint_only_model() {
        let route = endpoint_only_route("http://replica-1:8000/v1");
        let target = helper_target(&route, "", None).expect("endpoint target");
        assert_eq!(target.base, "http://replica-1:8000/v1");
        assert_eq!(
            target.api_key.as_deref(),
            Some("endpoint-key"),
            "the endpoint's own key wins over the model's"
        );
    }

    #[test]
    fn helper_target_refuses_a_model_with_nowhere_to_go() {
        let mut route = endpoint_only_route("http://replica-1:8000/v1");
        route.endpoints[0].healthy = false;
        assert!(
            helper_target(&route, "", None).is_err(),
            "no healthy endpoint and a blank api_base must fail the helper, not POST to /chat/completions"
        );
        route.endpoints.clear();
        assert!(helper_target(&route, "", None).is_err());
    }

    #[test]
    fn helper_target_pins_the_endpoint_that_served_the_request() {
        let mut route = endpoint_only_route("http://replica-1:8000/v1");
        route
            .endpoints
            .push(endpoint("e2", "http://replica-2:8000/v1/", None, true));
        route.endpoints[1].priority = 5;
        // Failover order would pick replica-1 first.
        let pinned = helper_target(
            &route,
            "",
            Some("http://replica-2:8000/v1/chat/completions"),
        )
        .unwrap();
        assert_eq!(pinned.base, "http://replica-2:8000/v1/");
        assert_eq!(
            pinned.api_key.as_deref(),
            Some("model-key"),
            "an endpoint without its own key falls back to the model's"
        );
        // A pin matches whole path segments only.
        let mut prefixed = route.clone();
        prefixed.endpoints[1].api_base = "http://replica-1:8000/v10".into();
        prefixed.endpoints[1].priority = 0;
        prefixed.endpoints[0].priority = 5;
        let pin = helper_target(
            &prefixed,
            "",
            Some("http://replica-1:8000/v10/chat/completions"),
        )
        .unwrap();
        assert_eq!(pin.base, "http://replica-1:8000/v10");
        prefixed.endpoints[0].priority = 0;
        prefixed.endpoints[1].priority = 5;
        let pin = helper_target(
            &prefixed,
            "",
            Some("http://replica-1:8000/v10/chat/completions"),
        )
        .unwrap();
        assert_eq!(
            pin.base, "http://replica-1:8000/v10",
            "`/v1` must not claim a URL served by `/v10`"
        );
        // A pin that matches nothing falls back to normal selection.
        let fallback = helper_target(&route, "", Some("http://elsewhere/v1")).unwrap();
        assert_eq!(fallback.base, "http://replica-1:8000/v1");
    }

    #[tokio::test]
    async fn chat_call_reaches_an_endpoint_only_model() {
        let (base, seen) = chat_server(serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "described" } }],
            "usage": { "prompt_tokens": 7, "completion_tokens": 3, "prompt_tokens_details": { "cached_tokens": 5 } }
        }))
        .await;
        let route = endpoint_only_route(&base);
        let reply = chat_call_with(
            &reqwest::Client::new(),
            &route,
            serde_json::json!({ "messages": [] }),
            Duration::from_secs(5),
        )
        .await
        .expect("helper call through the endpoint");
        assert_eq!(reply.text, "described");
        // The cached part of the prompt reaches the helper's ledger row too.
        assert_eq!(reply.usage, UpstreamUsage::new(7, 3, 5));
        assert_eq!(seen.lock().unwrap().as_slice(), ["Bearer endpoint-key"]);
    }

    /// A helper that accepts and never answers ends as a typed timeout the
    /// caller classes by type, not by matching the message.
    #[tokio::test]
    async fn a_silent_helper_is_a_typed_timeout() {
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(|| async {
                tokio::time::sleep(Duration::from_secs(5)).await;
                "{}"
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let route = endpoint_only_route(&base);
        let err = chat_call_with(
            &reqwest::Client::new(),
            &route,
            serde_json::json!({ "messages": [] }),
            Duration::from_millis(50),
        )
        .await
        .expect_err("a silent helper must not be waited for");
        assert!(HelperTimeout::is(&err), "{err}");
        assert_eq!(
            err.downcast_ref::<HelperTimeout>().map(|t| t.what.as_str()),
            Some("helper call")
        );
        assert!(!HelperTimeout::is(&anyhow::anyhow!(
            "a message that says timed out"
        )));
    }

    /// A reply over the cap is an error, never a truncated body.
    #[tokio::test]
    async fn an_oversize_helper_reply_is_refused_not_cut() {
        let big = serde_json::json!({ "pad": "x".repeat(10_000) });
        let (base, _) = chat_server(big).await;
        let route = endpoint_only_route(&base);
        let target = helper_target(&route, "", None).unwrap();
        let err = post_json_capped(
            &reqwest::Client::new(),
            build_chat_url(&target.base),
            &target.headers,
            target.api_key.as_deref(),
            &serde_json::json!({}),
            1024,
        )
        .await
        .expect_err("over the cap");
        assert!(err.to_string().contains("exceeds 1024 bytes"), "{err}");
        let whole = post_json_capped(
            &reqwest::Client::new(),
            build_chat_url(&target.base),
            &target.headers,
            target.api_key.as_deref(),
            &serde_json::json!({}),
            64 * 1024,
        )
        .await
        .expect("under the cap");
        assert!(whole.len() > 10_000);
    }

    #[tokio::test]
    async fn chat_call_completion_on_rejects_an_oversized_reply() {
        let big = "x".repeat(HELPER_BODY_MAX_BYTES + 1);
        let (base, _) = chat_server(serde_json::json!({ "pad": big })).await;
        let target = helper_target(&endpoint_only_route(&base), "", None).unwrap();
        let err = chat_call_completion_on(
            &reqwest::Client::new(),
            &target,
            &serde_json::json!({}),
            Duration::from_secs(10),
        )
        .await
        .expect_err("a reply over the cap is refused");
        assert!(err.to_string().contains("exceeds"), "{err}");
    }
}
