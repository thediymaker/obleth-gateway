//! The **streaming** gateway tool loop: a live, token-by-token version of
//! [`super::tool_loop`].
//!
//! The buffered loop in [`super::tool_loop`] forces the upstream call
//! non-streaming, waits for the whole completion, runs the tool, and only then
//! produces output — so the client sees a single burst with no live streaming.
//! This driver keeps every turn streaming instead:
//!
//! 1. it forwards the model's `content`/`reasoning` deltas straight to the
//!    client as they arrive (turn 1 "let me check…" streams live);
//! 2. when the model calls a *gateway-owned* tool it emits a short visible
//!    marker (so the user sees the search happening), runs the tool, appends
//!    the result, and re-asks the model — streaming the next turn live too;
//! 3. when the model calls a *client-owned* tool (an agentic client that
//!    brought its own tools) it flushes the tool-call deltas back to the client
//!    untouched so the client drives that call itself;
//! 4. when the model just answers, the answer was already streamed — it only
//!    needs the terminating `[DONE]`.
//!
//! Only the tool execution between turns pauses the stream. The first turn
//! reuses the response the proxy already opened (preserving retry/failover on
//! the hot path); follow-up turns are dispatched here directly.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use obleth_config::{ResolvedKey, ResolvedModel, ToolLoopSettings};
use serde_json::{json, Value};

use crate::state::AppState;

/// Visible marker prefix shown inline before a gateway tool runs.
const SEARCH_GLYPH: &str = "\u{1f50e}";

/// Token/latency stats the driver reports back so the proxy can settle the
/// request after the stream drains. `final_set` stays false when no upstream
/// usage was seen, telling the proxy to fall back to its estimate.
#[derive(Default)]
pub struct StreamStats {
    pub ttft_ms: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub final_set: bool,
    ttft_set: bool,
}

/// Everything the streaming tool loop owns for the duration of a request.
pub struct StreamLoop {
    pub state: AppState,
    pub route: ResolvedModel,
    pub key: ResolvedKey,
    pub session_id: String,
    /// The enriched request body (tools injected) used as the base for
    /// follow-up turns. Its `stream` flag is overwritten per dispatch.
    pub base_request: Value,
    /// Tool name -> MCP server name, for executing and ownership checks.
    pub tool_servers: HashMap<String, String>,
    pub settings: ToolLoopSettings,
    /// True when the client brought its own tools: a call against a name the
    /// gateway does not own is handed back to the client untouched.
    pub passthrough_unmapped: bool,
    /// Image-generation boon settings snapshot, present when the boon armed the
    /// loop.
    pub image_gen: Option<obleth_config::ImageGenerationBoonSettings>,
    pub dispatch_timeout: Duration,
    /// Whether the client asked for `stream_options.include_usage`.
    pub client_include_usage: bool,
    pub upstream_start: Instant,
}

/// One tool call assembled from streamed `tool_calls` deltas.
#[derive(Clone, Default)]
struct ToolAccum {
    id: String,
    name: String,
    arguments: String,
}

/// Drive the streaming tool loop. `first` is the already-opened upstream
/// response for turn 0 (dispatched by the proxy). The returned stream yields
/// SSE bytes for the client; `stats` is filled in as the loop runs.
pub fn run(
    args: StreamLoop,
    first: reqwest::Response,
    stats: Arc<Mutex<StreamStats>>,
) -> impl Stream<Item = Result<Bytes, std::io::Error>> {
    async_stream::stream! {
        let StreamLoop {
            state,
            route,
            key,
            session_id,
            base_request,
            tool_servers,
            settings,
            passthrough_unmapped,
            image_gen,
            dispatch_timeout,
            client_include_usage,
            upstream_start,
        } = args;

        let tool_timeout = Duration::from_millis(settings.tool_timeout_ms.max(1));
        let max_turns = settings.max_turns.clamp(1, obleth_config::TOOL_LOOP_MAX_TURNS);
        let upstream_model = route.upstream_model.clone();
        let created = super::now_ms() / 1000;
        let mut request = base_request;
        let mut sessions = super::tool_loop::Sessions::default();
        let deadline = super::tool_loop::LoopDeadline::new(&settings);
        // Follow-up turns go to the endpoint that served turn 0 (prefix-cache
        // locality); resolved once so every turn agrees.
        let target = super::helper_target(&route, &session_id, Some(first.url().as_str()));
        let idle = stream_idle_timeout();
        let mut current: Option<reqwest::Response> = Some(first);
        let mut image_ctx = image_gen.as_ref().map(|cfg| super::image_gen::ImageCtx {
            cfg,
            key: &key,
            session_id: &session_id,
            images: Vec::new(),
            events: Vec::new(),
        });

        'turns: for _turn in 0..max_turns {
            // Obtain this turn's streaming response: reuse the proxy's first
            // dispatch, otherwise dispatch a follow-up here.
            let resp = match current.take() {
                Some(r) => r,
                None => {
                    if let Some(obj) = request.as_object_mut() {
                        obj.insert("stream".into(), Value::Bool(true));
                        obj.insert(
                            "stream_options".into(),
                            json!({ "include_usage": true }),
                        );
                        obj.insert("model".into(), Value::String(upstream_model.clone()));
                    }
                    match dispatch(&state, &target, &request, deadline.bound(dispatch_timeout)).await {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::warn!(error = %e, "tool stream follow-up dispatch failed");
                            yield Ok(Bytes::from(content_chunk(
                                "chatcmpl-toolstream",
                                &upstream_model,
                                created,
                                &format!("\n\n[search continuation failed: {e}]\n\n"),
                            )));
                            if let Some(ctx) = image_ctx.as_ref() {
                                if let Some(chunk) =
                                    image_chunk("chatcmpl-toolstream", &upstream_model, created, &ctx.images)
                                {
                                    yield Ok(Bytes::from(chunk));
                                }
                            }
                            yield Ok(Bytes::from(finish_chunk(
                                "chatcmpl-toolstream",
                                &upstream_model,
                                created,
                                None,
                            )));
                            yield Ok(Bytes::from(done()));
                            return;
                        }
                    }
                }
            };

            let mut bytes = resp.bytes_stream();
            let mut buf: Vec<u8> = Vec::new();
            let mut content_acc = String::new();
            let mut tool_acc: BTreeMap<u64, ToolAccum> = BTreeMap::new();
            let mut buffered_tool_deltas: Vec<Value> = Vec::new();
            let mut usage: Option<(u32, u32)> = None;
            let mut id = String::from("chatcmpl-toolstream");
            let mut model_name = upstream_model.clone();
            let mut finish_reason: Option<Value> = None;
            let mut turn_done = false;

            'read: while let Some(item) = next_within(&mut bytes, idle).await {
                let chunk = match item {
                    Ok(c) => c,
                    Err(e) => {
                        tracing::warn!(error = %e, "tool stream upstream read error");
                        break 'read;
                    }
                };
                buf.extend_from_slice(&chunk);
                while let Some(event) = split_event(&mut buf) {
                    for data in parse_data_lines(&event) {
                        if data == "[DONE]" {
                            turn_done = true;
                            continue;
                        }
                        let Ok(v) = serde_json::from_str::<Value>(&data) else {
                            continue;
                        };
                        if let Some(x) = v.get("id").and_then(|x| x.as_str()) {
                            id = x.to_string();
                        }
                        if let Some(x) = v.get("model").and_then(|x| x.as_str()) {
                            model_name = x.to_string();
                        }
                        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                            let it = u
                                .get("prompt_tokens")
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0) as u32;
                            let ot = u
                                .get("completion_tokens")
                                .and_then(|x| x.as_u64())
                                .unwrap_or(0) as u32;
                            usage = Some((it, ot));
                        }
                        // Capture the turn's terminal finish reason so the plain
                        // answer can be closed with a proper finish chunk. The
                        // upstream's finish chunk usually carries an empty delta
                        // (so it is not forwarded below); without re-emitting it
                        // strict clients see a stream that never completes.
                        if let Some(fr) = v
                            .pointer("/choices/0/finish_reason")
                            .filter(|fr| !fr.is_null())
                        {
                            finish_reason = Some(fr.clone());
                        }
                        let delta = v.pointer("/choices/0/delta").cloned().unwrap_or(Value::Null);
                        if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                            for tc in tcs {
                                accumulate(&mut tool_acc, tc);
                            }
                            buffered_tool_deltas.push(delta.clone());
                        }
                        let content = delta.get("content").and_then(|c| c.as_str()).unwrap_or("");
                        let reasoning = delta
                            .get("reasoning_content")
                            .or_else(|| delta.get("reasoning"))
                            .and_then(|c| c.as_str())
                            .unwrap_or("");
                        if !content.is_empty() || !reasoning.is_empty() {
                            set_ttft(&stats, upstream_start.elapsed().as_millis() as u32);
                            yield Ok(Bytes::from(format!(
                                "data: {}\n\n",
                                forward_delta(&v)
                            )));
                            content_acc.push_str(content);
                        }
                    }
                }
                if turn_done {
                    break 'read;
                }
            }

            let calls: Vec<ToolAccum> = tool_acc
                .into_values()
                .filter(|c| !c.name.is_empty())
                .collect();

            // Plain answer: the content already streamed. Close it with the
            // terminal finish chunk a normal OpenAI stream always sends (the
            // upstream's was dropped above because it carried no content), then
            // the optional usage chunk and `[DONE]`.
            if calls.is_empty() {
                if let Some(ctx) = image_ctx.as_ref() {
                    if let Some(chunk) = image_chunk(&id, &model_name, created, &ctx.images) {
                        yield Ok(Bytes::from(chunk));
                    }
                }
                yield Ok(Bytes::from(finish_chunk(
                    &id,
                    &model_name,
                    created,
                    finish_reason.clone(),
                )));
                if client_include_usage {
                    if let Some((it, ot)) = usage {
                        yield Ok(Bytes::from(usage_chunk(&id, &model_name, created, it, ot)));
                    }
                }
                yield Ok(Bytes::from(done()));
                finalize_stats(&stats, usage);
                return;
            }

            // Client-owned tool call: hand the tool-call deltas back so the
            // client executes them itself.
            if passthrough_unmapped
                && calls.iter().any(|c| !tool_servers.contains_key(&c.name))
            {
                for d in &buffered_tool_deltas {
                    let chunk = json!({
                        "id": id,
                        "object": "chat.completion.chunk",
                        "created": created,
                        "model": model_name,
                        "choices": [{ "index": 0, "delta": d, "finish_reason": null }],
                    });
                    yield Ok(Bytes::from(format!("data: {chunk}\n\n")));
                }
                if let Some(ctx) = image_ctx.as_ref() {
                    if let Some(chunk) = image_chunk(&id, &model_name, created, &ctx.images) {
                        yield Ok(Bytes::from(chunk));
                    }
                }
                let fin = json!({
                    "id": id,
                    "object": "chat.completion.chunk",
                    "created": created,
                    "model": model_name,
                    "choices": [{ "index": 0, "delta": {}, "finish_reason": "tool_calls" }],
                });
                yield Ok(Bytes::from(format!("data: {fin}\n\n")));
                if client_include_usage {
                    if let Some((it, ot)) = usage {
                        yield Ok(Bytes::from(usage_chunk(&id, &model_name, created, it, ot)));
                    }
                }
                yield Ok(Bytes::from(done()));
                finalize_stats(&stats, usage);
                return;
            }

            // Gateway-owned tools: bill this turn, surface a visible marker per
            // call, execute, append results, and loop for the next turn.
            if let Some((it, ot)) = usage {
                super::bill_helper_call(&state, &route, &key, &session_id, "tool_loop", it, ot);
            }
            super::tool_loop::push_message(
                &mut request,
                assistant_message(&content_acc, &calls),
            );
            for (index, c) in calls.iter().enumerate() {
                // Emit the "searching…" marker on the reasoning channel, not as
                // answer content: a coding/agentic client shows it as visible
                // work (so a 15s search doesn't look like a hang) without it
                // corrupting the content stream it parses as the real answer.
                if index < obleth_config::TOOL_LOOP_MAX_CALLS_PER_TURN {
                    yield Ok(Bytes::from(reasoning_chunk(
                        &id,
                        &model_name,
                        created,
                        &marker_text(&c.name, &c.arguments),
                    )));
                }
                let pending = super::tool_loop::PendingCall {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    arguments: serde_json::from_str(&c.arguments).unwrap_or_else(|_| json!({})),
                };
                let result = super::tool_loop::run_one_call(
                    &state,
                    &key.tenant_id,
                    &tool_servers,
                    &mut sessions,
                    image_ctx.as_mut(),
                    &pending,
                    index,
                    &deadline,
                    tool_timeout,
                )
                .await;
                super::tool_loop::push_message(
                    &mut request,
                    json!({
                        "role": "tool",
                        "tool_call_id": c.id,
                        "content": result,
                    }),
                );
            }
            continue 'turns;
        }

        // Ran out of turns with the model still wanting tools. Mirror the
        // buffered loop: strip the tools, ask the model to answer from what it
        // already gathered, and stream that final turn live. Emitting only a
        // bare "[turn limit]" marker (as before) left the user with no answer
        // at all even though tool results were collected.
        if let Some(obj) = request.as_object_mut() {
            obj.remove("tools");
            obj.remove("tool_choice");
            obj.insert("stream".into(), Value::Bool(true));
            obj.insert("stream_options".into(), json!({ "include_usage": true }));
            obj.insert("model".into(), Value::String(upstream_model.clone()));
        }
        super::tool_loop::push_message(
            &mut request,
            json!({
                "role": "user",
                "content": "Please answer the original question now using the information \
                            already gathered above. Do not call any more tools.",
            }),
        );

        let final_resp = match dispatch(&state, &target, &request, deadline.bound(dispatch_timeout)).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "tool stream finalization dispatch failed");
                yield Ok(Bytes::from(content_chunk(
                    "chatcmpl-toolstream",
                    &upstream_model,
                    created,
                    "\n\n[reached the tool-call turn limit and could not produce a final answer]\n\n",
                )));
                if let Some(ctx) = image_ctx.as_ref() {
                    if let Some(chunk) =
                        image_chunk("chatcmpl-toolstream", &upstream_model, created, &ctx.images)
                    {
                        yield Ok(Bytes::from(chunk));
                    }
                }
                yield Ok(Bytes::from(finish_chunk(
                    "chatcmpl-toolstream",
                    &upstream_model,
                    created,
                    None,
                )));
                yield Ok(Bytes::from(done()));
                finalize_stats(&stats, None);
                return;
            }
        };

        let mut bytes = final_resp.bytes_stream();
        let mut buf: Vec<u8> = Vec::new();
        let mut usage: Option<(u32, u32)> = None;
        let mut id = String::from("chatcmpl-toolstream");
        let mut model_name = upstream_model.clone();
        let mut finish_reason: Option<Value> = None;
        let mut turn_done = false;

        'final_read: while let Some(item) = next_within(&mut bytes, idle).await {
            let chunk = match item {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(error = %e, "tool stream final read error");
                    break 'final_read;
                }
            };
            buf.extend_from_slice(&chunk);
            while let Some(event) = split_event(&mut buf) {
                for data in parse_data_lines(&event) {
                    if data == "[DONE]" {
                        turn_done = true;
                        continue;
                    }
                    let Ok(v) = serde_json::from_str::<Value>(&data) else {
                        continue;
                    };
                    if let Some(x) = v.get("id").and_then(|x| x.as_str()) {
                        id = x.to_string();
                    }
                    if let Some(x) = v.get("model").and_then(|x| x.as_str()) {
                        model_name = x.to_string();
                    }
                    if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
                        let it = u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                        let ot = u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
                        usage = Some((it, ot));
                    }
                    if let Some(fr) = v
                        .pointer("/choices/0/finish_reason")
                        .filter(|fr| !fr.is_null())
                    {
                        finish_reason = Some(fr.clone());
                    }
                    let delta = v.pointer("/choices/0/delta").cloned().unwrap_or(Value::Null);
                    let content = delta.get("content").and_then(|c| c.as_str()).unwrap_or("");
                    let reasoning = delta
                        .get("reasoning_content")
                        .or_else(|| delta.get("reasoning"))
                        .and_then(|c| c.as_str())
                        .unwrap_or("");
                    if !content.is_empty() || !reasoning.is_empty() {
                        set_ttft(&stats, upstream_start.elapsed().as_millis() as u32);
                        yield Ok(Bytes::from(format!("data: {}\n\n", forward_delta(&v))));
                    }
                }
            }
            if turn_done {
                break 'final_read;
            }
        }

        // The finalization turn is the client-facing final answer, so it is
        // settled as the main request via `finalize_stats` below — not billed
        // as a separate helper call (that would double-charge this turn).
        if let Some(ctx) = image_ctx.as_ref() {
            if let Some(chunk) = image_chunk(&id, &model_name, created, &ctx.images) {
                yield Ok(Bytes::from(chunk));
            }
        }
        yield Ok(Bytes::from(finish_chunk(&id, &model_name, created, finish_reason)));
        if client_include_usage {
            if let Some((it, ot)) = usage {
                yield Ok(Bytes::from(usage_chunk(&id, &model_name, created, it, ot)));
            }
        }
        yield Ok(Bytes::from(done()));
        finalize_stats(&stats, usage);
    }
}

/// Dispatch a follow-up streaming turn to the loop's pinned target. `timeout`
/// is `None` once the loop's time budget is spent.
async fn dispatch(
    state: &AppState,
    target: &anyhow::Result<crate::proxy::Target>,
    body: &Value,
    timeout: Option<Duration>,
) -> anyhow::Result<reqwest::Response> {
    let Some(timeout) = timeout else {
        anyhow::bail!("tool loop time limit reached");
    };
    let target = match target {
        Ok(t) => t,
        Err(e) => anyhow::bail!("{e}"),
    };
    let mut req = state
        .http
        .post(super::build_chat_url(&target.base))
        .headers(target.headers.clone())
        .json(body);
    if let Some(api_key) = &target.api_key {
        req = req.bearer_auth(api_key);
    }
    let resp = tokio::time::timeout(timeout, req.send())
        .await
        .map_err(|_| anyhow::anyhow!("tool stream dispatch timed out"))??;
    if !resp.status().is_success() {
        anyhow::bail!("upstream returned {}", resp.status());
    }
    Ok(resp)
}

/// Longest silence tolerated between two chunks of a boon-driven upstream
/// stream — the same effective read timeout the shared client applies to the
/// main upstream path (`0` disables). Mirrors
/// `obleth_config::Config::upstream_read_timeout_secs`'s own fallback: an
/// explicit `OBLETH_UPSTREAM_READ_TIMEOUT_SECS` wins, otherwise the upstream
/// request timeout floored at 120s. Computed from the env directly, rather
/// than threaded through `AppState`, because `main.rs` builds `Config` before
/// `AppState` exists. A zombie backend that stops sending mid-stream would
/// otherwise hold the request, and its fairshare permit, until the client
/// gives up.
pub(crate) fn stream_idle_timeout() -> Option<Duration> {
    static IDLE: std::sync::OnceLock<Option<Duration>> = std::sync::OnceLock::new();
    *IDLE.get_or_init(|| {
        let secs = idle_timeout_secs(
            std::env::var("OBLETH_UPSTREAM_READ_TIMEOUT_SECS")
                .ok()
                .as_deref(),
            std::env::var("OBLETH_UPSTREAM_TIMEOUT_SECS")
                .ok()
                .as_deref(),
        );
        (secs > 0).then(|| Duration::from_secs(secs))
    })
}

/// Pure parse behind [`stream_idle_timeout`], split out so the fallback chain
/// can be unit tested without racing the `OnceLock` across test threads.
fn idle_timeout_secs(read_raw: Option<&str>, request_raw: Option<&str>) -> u64 {
    read_raw
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or_else(|| {
            let request_timeout_secs = request_raw
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(300);
            request_timeout_secs.max(120)
        })
}

/// The next item of `stream`, or an error once it has been silent for `idle`.
pub(crate) async fn next_within<S, T, E>(
    stream: &mut S,
    idle: Option<Duration>,
) -> Option<anyhow::Result<T>>
where
    S: Stream<Item = Result<T, E>> + Unpin,
    E: Into<anyhow::Error>,
{
    let item = match idle {
        Some(limit) => match tokio::time::timeout(limit, stream.next()).await {
            Ok(item) => item,
            Err(_) => return Some(Err(anyhow::anyhow!("upstream stream idle for {limit:?}"))),
        },
        None => stream.next().await,
    };
    item.map(|r| r.map_err(Into::into))
}

fn set_ttft(stats: &Arc<Mutex<StreamStats>>, ms: u32) {
    if let Ok(mut s) = stats.lock() {
        if !s.ttft_set {
            s.ttft_ms = ms;
            s.ttft_set = true;
        }
    }
}

fn finalize_stats(stats: &Arc<Mutex<StreamStats>>, usage: Option<(u32, u32)>) {
    if let Some((it, ot)) = usage {
        if let Ok(mut s) = stats.lock() {
            s.input_tokens = it;
            s.output_tokens = ot;
            s.final_set = true;
        }
    }
}

/// Extract the next complete SSE event (up to and including its blank-line
/// delimiter, `\n\n` or `\r\n\r\n`) from the buffer, draining it. Returns
/// `None` while no full event is buffered yet.
pub(crate) fn split_event(buf: &mut Vec<u8>) -> Option<Vec<u8>> {
    let lf = buf.windows(2).position(|w| w == b"\n\n").map(|pos| pos + 2);
    let crlf = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|pos| pos + 4);
    let end = match (lf, crlf) {
        (Some(a), Some(b)) => a.min(b),
        (a, b) => a.or(b)?,
    };
    Some(buf.drain(..end).collect())
}

/// Pull the `data:` payloads out of one SSE event.
pub(crate) fn parse_data_lines(event: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(event);
    text.lines()
        .filter_map(|line| line.trim_start().strip_prefix("data:"))
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .collect()
}

/// Merge one streamed `tool_calls` delta into the per-turn accumulator.
fn accumulate(map: &mut BTreeMap<u64, ToolAccum>, tc: &Value) {
    let index = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0);
    let entry = map.entry(index).or_default();
    if let Some(id) = tc.get("id").and_then(|i| i.as_str()) {
        if !id.is_empty() {
            entry.id = id.to_string();
        }
    }
    if let Some(name) = tc.pointer("/function/name").and_then(|n| n.as_str()) {
        if !name.is_empty() {
            entry.name = name.to_string();
        }
    }
    if let Some(args) = tc.pointer("/function/arguments").and_then(|a| a.as_str()) {
        entry.arguments.push_str(args);
    }
}

/// Build the assistant message (content + tool_calls) for the conversation
/// history sent back upstream on the next turn.
fn assistant_message(content: &str, calls: &[ToolAccum]) -> Value {
    let tool_calls: Vec<Value> = calls
        .iter()
        .map(|c| {
            json!({
                "id": c.id,
                "type": "function",
                "function": { "name": c.name, "arguments": c.arguments },
            })
        })
        .collect();
    json!({
        "role": "assistant",
        "content": if content.is_empty() { Value::Null } else { Value::String(content.to_string()) },
        "tool_calls": tool_calls,
    })
}

/// The short, human-visible marker shown inline before a gateway tool runs.
fn marker_text(name: &str, arguments: &str) -> String {
    let query = serde_json::from_str::<Value>(arguments)
        .ok()
        .and_then(|v| {
            v.get("query")
                .or_else(|| v.get("q"))
                .and_then(|q| q.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            let trimmed = arguments.trim();
            if trimmed.is_empty() || trimmed == "{}" {
                String::new()
            } else {
                let mut s = trimmed.replace('\n', " ");
                if s.len() > 120 {
                    while !s.is_char_boundary(120) {
                        s.truncate(s.len() - 1);
                    }
                    s.truncate(120);
                    s.push('…');
                }
                s
            }
        });
    if query.is_empty() {
        format!("\n\n{SEARCH_GLYPH} Running `{name}`…\n\n")
    } else {
        format!("\n\n{SEARCH_GLYPH} Searching `{name}`: {query}\n\n")
    }
}

/// A `chat.completion.chunk` carrying a single content delta.
pub(crate) fn content_chunk(id: &str, model: &str, created: i64, text: &str) -> String {
    let chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{ "index": 0, "delta": { "content": text }, "finish_reason": null }],
    });
    format!("data: {chunk}\n\n")
}

/// A content chunk carrying the markdown for every image generated this
/// request, or `None` when none were. Emitted just before the terminal finish
/// chunk so the image lands after the model's prose, matching the buffered
/// loop's ordering.
fn image_chunk(
    id: &str,
    model: &str,
    created: i64,
    images: &[super::image_gen::GeneratedImage],
) -> Option<String> {
    let markdown = super::image_gen::attachment(images)?;
    Some(content_chunk(id, model, created, &markdown))
}

/// A `chat.completion.chunk` carrying a single reasoning delta. Used for the
/// in-progress tool markers so clients render them as "thinking" work rather
/// than as part of the answer content.
fn reasoning_chunk(id: &str, model: &str, created: i64, text: &str) -> String {
    let chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": { "reasoning_content": text },
            "finish_reason": null,
        }],
    });
    format!("data: {chunk}\n\n")
}

/// A trailing usage-only chunk (emitted when the client asked for usage).
pub(crate) fn usage_chunk(id: &str, model: &str, created: i64, input: u32, output: u32) -> String {
    let chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [],
        "usage": {
            "prompt_tokens": input,
            "completion_tokens": output,
            "total_tokens": input + output,
        },
    });
    format!("data: {chunk}\n\n")
}

/// Forward a chunk's content/reasoning delta without leaking a `tool_calls`
/// field that shared the same delta (rare, but keeps the client's view clean),
/// and force `finish_reason: null`. The driver re-emits a single terminal
/// finish chunk itself, so content chunks must never carry one — otherwise a
/// model that combines the last token with `finish_reason` would produce two.
fn forward_delta(v: &Value) -> Value {
    let mut v = v.clone();
    if let Some(choice) = v.pointer_mut("/choices/0").and_then(|c| c.as_object_mut()) {
        if let Some(delta) = choice.get_mut("delta").and_then(|d| d.as_object_mut()) {
            delta.remove("tool_calls");
        }
        choice.insert("finish_reason".into(), Value::Null);
    }
    v
}

/// A terminal `chat.completion.chunk` carrying the finish reason and an empty
/// delta — the chunk a normal OpenAI stream sends right before the usage chunk
/// and `[DONE]`. `finish_reason` defaults to `"stop"` when the upstream did not
/// report one.
pub(crate) fn finish_chunk(
    id: &str,
    model: &str,
    created: i64,
    finish_reason: Option<Value>,
) -> String {
    let chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": {},
            "finish_reason": finish_reason.unwrap_or_else(|| Value::String("stop".into())),
        }],
    });
    format!("data: {chunk}\n\n")
}

pub(crate) fn done() -> &'static str {
    "data: [DONE]\n\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_event_extracts_one_event_at_a_time() {
        let mut buf = b"data: a\n\ndata: b\n\ndata: c".to_vec();
        let e1 = split_event(&mut buf).unwrap();
        assert_eq!(e1, b"data: a\n\n");
        let e2 = split_event(&mut buf).unwrap();
        assert_eq!(e2, b"data: b\n\n");
        // "data: c" has no terminator yet.
        assert!(split_event(&mut buf).is_none());
        assert_eq!(buf, b"data: c");
    }

    #[test]
    fn split_event_accepts_crlf_delimiters() {
        let mut buf = b"data: a\r\n\r\ndata: b\n\ndata: c\r\n".to_vec();
        let e1 = split_event(&mut buf).unwrap();
        assert_eq!(e1, b"data: a\r\n\r\n");
        assert_eq!(parse_data_lines(&e1), vec!["a".to_string()]);
        let e2 = split_event(&mut buf).unwrap();
        assert_eq!(e2, b"data: b\n\n");
        assert!(split_event(&mut buf).is_none());
        assert_eq!(buf, b"data: c\r\n");
    }

    #[test]
    fn idle_timeout_secs_matches_the_upstream_read_timeout_fallback() {
        // Same fallback chain as `obleth_config::Config::upstream_read_timeout_secs`:
        // an explicit read timeout wins; otherwise the request timeout,
        // floored at 120s; with the same defaults when neither is set.
        assert_eq!(idle_timeout_secs(None, None), 300);
        assert_eq!(idle_timeout_secs(None, Some("30")), 120);
        assert_eq!(idle_timeout_secs(Some("45"), None), 45);
        assert_eq!(idle_timeout_secs(Some("0"), None), 0);
        assert_eq!(idle_timeout_secs(Some("x"), Some("600")), 600);
    }

    #[tokio::test]
    async fn next_within_gives_up_on_a_silent_stream() {
        let mut silent = futures_util::stream::pending::<Result<Bytes, std::io::Error>>();
        let item = next_within(&mut silent, Some(Duration::from_millis(20))).await;
        assert!(
            matches!(item, Some(Err(_))),
            "a stalled upstream must end the read, not hang it"
        );

        let mut live =
            futures_util::stream::iter(vec![Ok::<_, std::io::Error>(Bytes::from_static(b"x"))]);
        let first = next_within(&mut live, Some(Duration::from_secs(1))).await;
        assert_eq!(first.unwrap().unwrap(), Bytes::from_static(b"x"));
        assert!(next_within(&mut live, Some(Duration::from_secs(1)))
            .await
            .is_none());
    }

    #[test]
    fn parse_data_lines_reads_payloads() {
        let lines = parse_data_lines(b"data: {\"a\":1}\n\n");
        assert_eq!(lines, vec!["{\"a\":1}".to_string()]);
        let done = parse_data_lines(b"data: [DONE]\n\n");
        assert_eq!(done, vec!["[DONE]".to_string()]);
    }

    #[test]
    fn accumulate_merges_split_tool_call_deltas() {
        let mut map = BTreeMap::new();
        accumulate(
            &mut map,
            &json!({ "index": 0, "id": "call_1", "function": { "name": "search", "arguments": "{\"qu" } }),
        );
        accumulate(
            &mut map,
            &json!({ "index": 0, "function": { "arguments": "ery\":\"x\"}" } }),
        );
        let acc = map.get(&0).unwrap();
        assert_eq!(acc.id, "call_1");
        assert_eq!(acc.name, "search");
        assert_eq!(acc.arguments, "{\"query\":\"x\"}");
    }

    #[test]
    fn assistant_message_carries_content_and_calls() {
        let calls = vec![ToolAccum {
            id: "call_1".into(),
            name: "search".into(),
            arguments: "{\"query\":\"x\"}".into(),
        }];
        let msg = assistant_message("let me check", &calls);
        assert_eq!(msg["role"], "assistant");
        assert_eq!(msg["content"], "let me check");
        assert_eq!(msg["tool_calls"][0]["function"]["name"], "search");
        assert_eq!(
            msg["tool_calls"][0]["function"]["arguments"],
            "{\"query\":\"x\"}"
        );
    }

    #[test]
    fn assistant_message_null_content_when_empty() {
        let calls = vec![ToolAccum {
            id: "c".into(),
            name: "search".into(),
            arguments: "{}".into(),
        }];
        let msg = assistant_message("", &calls);
        assert!(msg["content"].is_null());
    }

    #[test]
    fn marker_uses_query_when_present() {
        let m = marker_text("searxng_web_search", "{\"query\":\"latest NVDA price\"}");
        assert!(m.contains("searxng_web_search"));
        assert!(m.contains("latest NVDA price"));
    }

    #[test]
    fn marker_without_query_falls_back() {
        let m = marker_text("do_thing", "{}");
        assert!(m.contains("Running"));
        assert!(m.contains("do_thing"));
    }

    #[test]
    fn forward_delta_strips_tool_calls_and_nulls_finish_reason() {
        let v = json!({
            "choices": [{
                "delta": { "content": "hi", "tool_calls": [{ "index": 0 }] },
                "finish_reason": "stop"
            }]
        });
        let forwarded = forward_delta(&v);
        assert_eq!(forwarded["choices"][0]["delta"]["content"], "hi");
        assert!(forwarded["choices"][0]["delta"].get("tool_calls").is_none());
        // Content chunks must not carry a finish reason; the driver emits a
        // single terminal finish chunk separately.
        assert!(forwarded["choices"][0]["finish_reason"].is_null());
    }

    #[test]
    fn finish_chunk_defaults_to_stop() {
        let c = finish_chunk("id1", "m", 1, None);
        let json_part = c.trim_start_matches("data: ").trim_end();
        let v: Value = serde_json::from_str(json_part).unwrap();
        assert_eq!(v["choices"][0]["finish_reason"], "stop");
        assert!(v["choices"][0]["delta"].as_object().unwrap().is_empty());
    }

    #[test]
    fn reasoning_chunk_uses_reasoning_channel() {
        let c = reasoning_chunk("id1", "m", 1, "\n\n🔎 Searching\n\n");
        let json_part = c.trim_start_matches("data: ").trim_end();
        let v: Value = serde_json::from_str(json_part).unwrap();
        // Marker lands on reasoning_content, never on content.
        assert_eq!(
            v["choices"][0]["delta"]["reasoning_content"],
            "\n\n🔎 Searching\n\n"
        );
        assert!(v["choices"][0]["delta"].get("content").is_none());
        assert!(v["choices"][0]["finish_reason"].is_null());
    }

    #[test]
    fn content_chunk_is_valid_sse() {
        let c = content_chunk("id1", "m", 1, "hello");
        assert!(c.starts_with("data: "));
        assert!(c.ends_with("\n\n"));
        let json_part = c.trim_start_matches("data: ").trim_end();
        let v: Value = serde_json::from_str(json_part).unwrap();
        assert_eq!(v["choices"][0]["delta"]["content"], "hello");
    }

    #[test]
    fn image_chunk_carries_the_markdown_on_the_content_channel() {
        let images = vec![crate::boons::image_gen::GeneratedImage {
            url: "http://i/x.png".to_string(),
            size: "512x512".to_string(),
            prompt: "a cat".to_string(),
        }];
        let sse = image_chunk("id1", "m", 1, &images).expect("one image produces a chunk");
        assert!(sse.starts_with("data: "));
        assert!(sse.ends_with("\n\n"));
        let v: Value = serde_json::from_str(sse.trim_start_matches("data: ").trim_end()).unwrap();
        let content = v["choices"][0]["delta"]["content"].as_str().unwrap();
        assert!(content.contains("![a cat](http://i/x.png)"));
        // The image is the answer, not thinking: it must not ride the
        // reasoning channel the tool markers use.
        assert!(v["choices"][0]["delta"].get("reasoning_content").is_none());
        assert!(v["choices"][0]["finish_reason"].is_null());
    }

    #[test]
    fn image_chunk_is_none_without_images() {
        assert!(image_chunk("id1", "m", 1, &[]).is_none());
    }
}
