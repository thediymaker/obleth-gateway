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
//!
//! A turn the upstream stops short of (its connection breaks or goes silent,
//! it closes before `[DONE]` and a finish reason, or a follow-up turn cannot
//! be dispatched) ends the driver's stream with an error item and no finish
//! chunk or `[DONE]`. The proxy settles the request, then tells the client
//! with an error event and aborts the response, as on the pass-through path.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use obleth_config::{ResolvedKey, ResolvedModel, ToolLoopSettings};
use serde_json::{json, Value};

use crate::proxy::UpstreamUsage;
use crate::state::AppState;
use crate::upstream_failure::{payload_failure, stream_cut_error, DEFAULT_FAILURE_STATUS};

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
    /// The part of `input_tokens` the upstream served from its prefix cache.
    pub cached_input_tokens: u32,
    pub final_set: bool,
    ttft_set: bool,
    /// The status to record when the client-facing turn failed: the upstream
    /// broke off, reported an error in its stream, or could not be reached
    /// for a follow-up turn. The request then settles as a failure, billed
    /// for what the turn delivered, never with the estimate.
    pub failure: Option<u16>,
    /// Characters of generated text the failed turn delivered, for billing
    /// it when the upstream reported no usage.
    pub failed_turn_chars: usize,
}

/// Everything the streaming tool loop owns for the duration of a request.
pub struct StreamLoop {
    pub state: AppState,
    pub route: ResolvedModel,
    pub key: ResolvedKey,
    pub session_id: String,
    /// The client request this loop answers; every follow-up turn is billed
    /// against it.
    pub request_id: uuid::Uuid,
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
    /// Web-search boon settings snapshot, present when the boon armed the loop.
    pub web_search: Option<obleth_config::WebSearchBoonSettings>,
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
            request_id,
            base_request,
            tool_servers,
            settings,
            passthrough_unmapped,
            image_gen,
            web_search,
            dispatch_timeout,
            client_include_usage,
            upstream_start,
        } = args;

        let tool_timeout = Duration::from_millis(settings.tool_timeout_ms.max(1));
        let max_turns = settings.max_turns.clamp(1, obleth_config::TOOL_LOOP_MAX_TURNS);
        let upstream_model = route.upstream_model.clone();
        let created = crate::tracer::now_ms() / 1000;
        let mut request = base_request;
        // The upstream's role chunk carries no content, so the content filter
        // below drops it; the driver opens the client stream with its own role
        // chunk instead, once, ahead of the first forwarded chunk.
        let mut role_sent = false;
        // Time to first token is recorded once, on the first text forwarded.
        let mut ttft_set = false;
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
            request_id,
            images: Vec::new(),
            events: Vec::new(),
        });
        let mut search_ctx = web_search.as_ref().map(|cfg| super::web_search::SearchCtx {
            cfg,
            key: &key,
            session_id: &session_id,
            request_id,
            searches: 0,
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
                            // The answer cannot go on past the tool call: it
                            // ends the way a broken-off stream does.
                            tracing::warn!(error = %e, "tool stream follow-up dispatch failed");
                            fail_stats(&stats, DEFAULT_FAILURE_STATUS, None, 0);
                            yield Err(stream_cut_error());
                            return;
                        }
                    }
                }
            };

            let mut reader = TurnReader::new(resp, idle);
            let mut turn = Turn::new(&upstream_model, true);
            loop {
                match reader.next().await {
                    TurnEvent::Chunk { value, failure } => {
                        if let Some(status) = failure {
                            turn.failure.get_or_insert(status);
                        }
                        let (frames, text) = turn.take(value, &mut role_sent, created);
                        if text && !ttft_set {
                            ttft_set = true;
                            set_ttft(&stats, upstream_start.elapsed().as_millis() as u32);
                        }
                        for frame in frames {
                            yield Ok(Bytes::from(frame));
                        }
                    }
                    TurnEvent::Error { data, status, message } => {
                        // An error event ends the turn: the client gets it as
                        // the upstream sent it.
                        tracing::warn!(status, %message, "tool stream upstream reported an error");
                        turn.failure.get_or_insert(status);
                        yield Ok(Bytes::from(format!("data: {data}\n\n")));
                        turn.error_forwarded = true;
                        break;
                    }
                    TurnEvent::Done => {
                        turn.done = true;
                        break;
                    }
                    TurnEvent::Cut(e) => {
                        tracing::warn!(error = %e, "tool stream upstream read error");
                        turn.broke_off = true;
                        break;
                    }
                    TurnEvent::End => break,
                }
            }
            let Turn {
                id,
                model: model_name,
                content: content_acc,
                tools,
                usage,
                finish_reason,
                done: turn_done,
                failure,
                error_forwarded,
                chars: turn_chars,
                broke_off,
            } = turn;
            let (tool_acc, buffered_tool_deltas) = tools.unwrap_or_default();

            // A turn the upstream stopped short of, or that closed with
            // neither `[DONE]` nor a finish reason, ends with an error item
            // and no finish chunk or `[DONE]` (see the module doc). Its tool
            // calls, possibly cut off, are not run.
            if !error_forwarded && (broke_off || (!turn_done && finish_reason.is_none())) {
                if !broke_off {
                    tracing::warn!("tool stream upstream ended before its last event");
                }
                fail_stats(&stats, failure.unwrap_or(DEFAULT_FAILURE_STATUS), usage, turn_chars);
                yield Err(stream_cut_error());
                return;
            }

            // A turn the upstream failed in its own words (an error event, or
            // a generation it aborted) ends the loop too, with what it said.
            if let Some(status) = failure {
                if !error_forwarded {
                    for frame in closing(
                        &id,
                        &model_name,
                        created,
                        image_ctx.as_ref().map(|ctx| &ctx.images[..]),
                        finish_reason.clone(),
                        usage.filter(|_| client_include_usage),
                    ) {
                        yield Ok(Bytes::from(frame));
                    }
                }
                yield Ok(Bytes::from(done()));
                fail_stats(&stats, status, usage, turn_chars);
                return;
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
                for frame in closing(
                    &id,
                    &model_name,
                    created,
                    image_ctx.as_ref().map(|ctx| &ctx.images[..]),
                    finish_reason.clone(),
                    usage.filter(|_| client_include_usage),
                ) {
                    yield Ok(Bytes::from(frame));
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
                for frame in closing(
                    &id,
                    &model_name,
                    created,
                    image_ctx.as_ref().map(|ctx| &ctx.images[..]),
                    Some(Value::String("tool_calls".into())),
                    usage.filter(|_| client_include_usage),
                ) {
                    yield Ok(Bytes::from(frame));
                }
                yield Ok(Bytes::from(done()));
                finalize_stats(&stats, usage);
                return;
            }

            // Gateway-owned tools: bill this turn, surface a visible marker per
            // call, execute, append results, and loop for the next turn.
            if let Some(u) = usage {
                super::bill_helper_call(&state, &route, &key, &session_id, request_id, "tool_loop", u);
            }
            super::tool_loop::push_message(
                &mut request,
                assistant_message(&content_acc, &calls),
            );
            // Emit the "searching…" markers on the reasoning channel, not as
            // answer content: a coding/agentic client shows them as visible
            // work (so a 15s search doesn't look like a hang) without them
            // corrupting the content stream it parses as the real answer. The
            // calls then run together.
            for c in calls.iter().take(obleth_config::TOOL_LOOP_MAX_CALLS_PER_TURN) {
                yield Ok(Bytes::from(reasoning_chunk(
                    &id,
                    &model_name,
                    created,
                    &marker_text(&c.name, &c.arguments),
                )));
            }
            let pending: Vec<super::tool_loop::PendingCall> = calls
                .iter()
                .map(|c| super::tool_loop::PendingCall {
                    id: c.id.clone(),
                    name: c.name.clone(),
                    arguments: serde_json::from_str(&c.arguments).unwrap_or_else(|_| json!({})),
                })
                .collect();
            let results = super::tool_loop::run_turn(
                &state,
                &key.tenant_id,
                &tool_servers,
                &mut sessions,
                image_ctx.as_mut(),
                search_ctx.as_mut(),
                &pending,
                &deadline,
                tool_timeout,
            )
            .await;
            for (c, result) in calls.iter().zip(results) {
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
                fail_stats(&stats, DEFAULT_FAILURE_STATUS, None, 0);
                yield Err(stream_cut_error());
                return;
            }
        };

        let mut reader = TurnReader::new(final_resp, idle);
        let mut turn = Turn::new(&upstream_model, false);
        loop {
            match reader.next().await {
                TurnEvent::Chunk { value, failure } => {
                    if let Some(status) = failure {
                        turn.failure.get_or_insert(status);
                    }
                    let (frames, text) = turn.take(value, &mut role_sent, created);
                    if text && !ttft_set {
                        ttft_set = true;
                        set_ttft(&stats, upstream_start.elapsed().as_millis() as u32);
                    }
                    for frame in frames {
                        yield Ok(Bytes::from(frame));
                    }
                }
                TurnEvent::Error { data, status, message } => {
                    tracing::warn!(status, %message, "tool stream upstream reported an error");
                    turn.failure.get_or_insert(status);
                    yield Ok(Bytes::from(format!("data: {data}\n\n")));
                    turn.error_forwarded = true;
                    break;
                }
                TurnEvent::Done => {
                    turn.done = true;
                    break;
                }
                TurnEvent::Cut(e) => {
                    tracing::warn!(error = %e, "tool stream final read error");
                    turn.broke_off = true;
                    break;
                }
                TurnEvent::End => break,
            }
        }
        let Turn {
            id,
            model: model_name,
            usage,
            finish_reason,
            done: turn_done,
            failure,
            error_forwarded,
            chars: turn_chars,
            broke_off,
            ..
        } = turn;

        // Stopped short, as a tool turn can be: the same error item.
        if !error_forwarded && (broke_off || (!turn_done && finish_reason.is_none())) {
            if !broke_off {
                tracing::warn!("tool stream final turn ended before its last event");
            }
            fail_stats(&stats, failure.unwrap_or(DEFAULT_FAILURE_STATUS), usage, turn_chars);
            yield Err(stream_cut_error());
            return;
        }

        // The finalization turn is the client-facing final answer, so it is
        // settled as the main request via `finalize_stats` below — not billed
        // as a separate helper call (that would double-charge this turn).
        if !error_forwarded {
            for frame in closing(
                &id,
                &model_name,
                created,
                image_ctx.as_ref().map(|ctx| &ctx.images[..]),
                finish_reason,
                usage.filter(|_| client_include_usage),
            ) {
                yield Ok(Bytes::from(frame));
            }
        }
        yield Ok(Bytes::from(done()));
        if let Some(status) = failure {
            fail_stats(&stats, status, usage, turn_chars);
            return;
        }
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
    let resp = super::with_timeout("tool stream dispatch", timeout, async {
        Ok(req.send().await?)
    })
    .await?;
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
/// What a [`TurnReader`] reads next from an upstream chat stream.
pub(crate) enum TurnEvent {
    /// One chunk. `failure` is the status of a failure it reports alongside
    /// its choices (a generation the upstream aborted).
    Chunk { value: Value, failure: Option<u16> },
    /// The upstream's own error event (no choices): its data as sent, and
    /// what it reports.
    Error {
        data: String,
        status: u16,
        message: String,
    },
    /// `[DONE]`.
    Done,
    /// The connection broke, or went silent past the idle limit.
    Cut(anyhow::Error),
    /// The connection closed.
    End,
}

/// Reads one upstream chat stream as [`TurnEvent`]s: the byte stream, its
/// event splitter and the idle limit in one place, for the tool-stream turns,
/// the speculation drafter and the continuation relay. Events with no data,
/// and data that is not JSON, are skipped.
pub(crate) struct TurnReader {
    bytes: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
    events: crate::sse::Splitter,
    idle: Option<Duration>,
}

impl TurnReader {
    pub(crate) fn new(resp: reqwest::Response, idle: Option<Duration>) -> Self {
        Self::from_stream(Box::pin(resp.bytes_stream()), idle)
    }

    pub(crate) fn from_stream(
        bytes: std::pin::Pin<Box<dyn Stream<Item = Result<Bytes, reqwest::Error>> + Send>>,
        idle: Option<Duration>,
    ) -> Self {
        Self {
            bytes,
            events: crate::sse::Splitter::unbounded(),
            idle,
        }
    }

    pub(crate) async fn next(&mut self) -> TurnEvent {
        loop {
            while let Some(event) = self.events.next_event() {
                let Some(data) = crate::sse::data_text(event) else {
                    continue;
                };
                if data == "[DONE]" {
                    return TurnEvent::Done;
                }
                let Ok(value) = serde_json::from_str::<Value>(&data) else {
                    continue;
                };
                let failure = payload_failure(&value);
                if let Some(reported) = failure.as_ref().filter(|_| value.get("choices").is_none())
                {
                    return TurnEvent::Error {
                        status: reported.status,
                        message: reported.message.clone(),
                        data: data.into_owned(),
                    };
                }
                return TurnEvent::Chunk {
                    failure: failure.map(|f| f.status),
                    value,
                };
            }
            match next_within(&mut self.bytes, self.idle).await {
                Some(Ok(chunk)) => self.events.push(&chunk),
                Some(Err(e)) => return TurnEvent::Cut(e),
                None => return TurnEvent::End,
            }
        }
    }
}

/// What one streamed turn of the tool loop has read so far.
struct Turn {
    id: String,
    model: String,
    /// Answer text, kept for the assistant message of a tool turn.
    content: String,
    /// The tool-call deltas, merged and as sent, on a turn that may call
    /// tools.
    tools: Option<(BTreeMap<u64, ToolAccum>, Vec<Value>)>,
    usage: Option<UpstreamUsage>,
    finish_reason: Option<Value>,
    /// `[DONE]` arrived.
    done: bool,
    /// The status to record for a failed turn.
    failure: Option<u16>,
    /// The upstream's own error event already went to the client.
    error_forwarded: bool,
    /// Characters of text delivered, for billing a turn cut short.
    chars: usize,
    /// The upstream stopped short: its connection broke or went silent.
    broke_off: bool,
}

impl Turn {
    fn new(upstream_model: &str, tools: bool) -> Self {
        Self {
            id: String::from("chatcmpl-toolstream"),
            model: upstream_model.to_string(),
            content: String::new(),
            tools: tools.then(Default::default),
            usage: None,
            finish_reason: None,
            done: false,
            failure: None,
            error_forwarded: false,
            chars: 0,
            broke_off: false,
        }
    }

    /// Take in one chunk; the frames to send the client (the stream's role
    /// chunk first, once), and whether it carried text.
    fn take(&mut self, value: Value, role_sent: &mut bool, created: i64) -> (Vec<String>, bool) {
        if let Some(x) = value.get("id").and_then(Value::as_str) {
            if self.id != x {
                self.id = x.to_string();
            }
        }
        if let Some(x) = value.get("model").and_then(Value::as_str) {
            if self.model != x {
                self.model = x.to_string();
            }
        }
        let mut frames = Vec::new();
        if !*role_sent {
            *role_sent = true;
            frames.push(role_chunk(&self.id, &self.model, created));
        }
        if let Some(u) = value.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(chunk_usage(u));
        }
        // Capture the turn's terminal finish reason so the plain answer can be
        // closed with a proper finish chunk. The upstream's finish chunk
        // usually carries an empty delta (so it is not forwarded below);
        // without re-emitting it strict clients see a stream that never
        // completes.
        if let Some(fr) = value
            .pointer("/choices/0/finish_reason")
            .filter(|fr| !fr.is_null())
        {
            self.finish_reason = Some(fr.clone());
        }
        let delta = value.pointer("/choices/0/delta").unwrap_or(&Value::Null);
        if let Some((acc, sent)) = self.tools.as_mut() {
            if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
                for tc in tcs {
                    accumulate(acc, tc);
                }
                sent.push(delta.clone());
            }
        }
        let content = delta.get("content").and_then(Value::as_str).unwrap_or("");
        let reasoning = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let text = !content.is_empty() || !reasoning.is_empty();
        if text {
            self.chars += content.chars().count() + reasoning.chars().count();
            if self.tools.is_some() {
                self.content.push_str(content);
            }
            frames.push(format!("data: {}\n\n", forward_delta(value)));
        }
        (frames, text)
    }
}

/// The chunks that close a turn's answer, before `[DONE]`: the generated
/// images, the terminal finish chunk a normal OpenAI stream always sends (the
/// upstream's was not forwarded, as it carried no text), and the usage chunk
/// when the client asked for it.
fn closing(
    id: &str,
    model: &str,
    created: i64,
    images: Option<&[super::image_gen::GeneratedImage]>,
    finish_reason: Option<Value>,
    usage: Option<UpstreamUsage>,
) -> Vec<String> {
    let mut frames = Vec::with_capacity(3);
    if let Some(chunk) = images.and_then(|images| image_chunk(id, model, created, images)) {
        frames.push(chunk);
    }
    frames.push(finish_chunk(id, model, created, finish_reason));
    if let Some(u) = usage {
        frames.push(usage_chunk(id, model, created, u));
    }
    frames
}

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

/// One stream chunk's `usage` object: each count independently, zero when
/// absent, with the cached part of the prompt when it reports one.
pub(crate) fn chunk_usage(u: &Value) -> UpstreamUsage {
    let read = |field: &str| u.get(field).and_then(Value::as_u64).unwrap_or(0) as u32;
    UpstreamUsage::new(
        read("prompt_tokens"),
        read("completion_tokens"),
        crate::proxy::cached_prompt_tokens(u) as u32,
    )
}

fn finalize_stats(stats: &Arc<Mutex<StreamStats>>, usage: Option<UpstreamUsage>) {
    if let Some(u) = usage {
        if let Ok(mut s) = stats.lock() {
            s.input_tokens = u.input;
            s.output_tokens = u.output;
            s.cached_input_tokens = u.cached;
            s.final_set = true;
        }
    }
}

/// Record that the client-facing turn failed, with whatever usage it
/// reported and the text it delivered.
fn fail_stats(
    stats: &Arc<Mutex<StreamStats>>,
    status: u16,
    usage: Option<UpstreamUsage>,
    delivered_chars: usize,
) {
    finalize_stats(stats, usage);
    if let Ok(mut s) = stats.lock() {
        s.failure = Some(status);
        s.failed_turn_chars = delivered_chars;
    }
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
                    // The cut falls on a character's edge at or before 120 bytes.
                    let mut end = 120;
                    while !s.is_char_boundary(end) {
                        end -= 1;
                    }
                    s.truncate(end);
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

/// The opening `chat.completion.chunk` of an OpenAI stream: the assistant role
/// with empty content.
fn role_chunk(id: &str, model: &str, created: i64) -> String {
    let chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "delta": { "role": "assistant", "content": "" },
            "finish_reason": null,
        }],
    });
    format!("data: {chunk}\n\n")
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
pub(crate) fn usage_chunk(id: &str, model: &str, created: i64, usage: UpstreamUsage) -> String {
    let mut counts = json!({
        "prompt_tokens": usage.input,
        "completion_tokens": usage.output,
        "total_tokens": usage.input + usage.output,
    });
    if usage.cached > 0 {
        counts["prompt_tokens_details"] = json!({ "cached_tokens": usage.cached });
    }
    let chunk = json!({
        "id": id,
        "object": "chat.completion.chunk",
        "created": created,
        "model": model,
        "choices": [],
        "usage": counts,
    });
    format!("data: {chunk}\n\n")
}

/// Forward a chunk's content/reasoning delta without leaking a `tool_calls`
/// field that shared the same delta (rare, but keeps the client's view clean),
/// and force `finish_reason: null`. The driver re-emits a single terminal
/// finish chunk itself, so content chunks must never carry one — otherwise a
/// model that combines the last token with `finish_reason` would produce two.
fn forward_delta(mut v: Value) -> Value {
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

    fn reader(chunks: Vec<&'static str>, idle: Option<Duration>) -> TurnReader {
        let items: Vec<Result<Bytes, reqwest::Error>> = chunks
            .into_iter()
            .map(|c| Ok(Bytes::from_static(c.as_bytes())))
            .collect();
        TurnReader::from_stream(Box::pin(futures_util::stream::iter(items)), idle)
    }

    async fn read_all(mut r: TurnReader) -> Vec<String> {
        let mut seen = Vec::new();
        loop {
            let (label, last) = match r.next().await {
                TurnEvent::Chunk { value, failure } => (
                    format!("chunk {failure:?} {}", value["choices"][0]["delta"]),
                    false,
                ),
                TurnEvent::Error { data, status, .. } => (format!("error {status} {data}"), true),
                TurnEvent::Done => ("done".into(), true),
                TurnEvent::Cut(_) => ("cut".into(), true),
                TurnEvent::End => ("end".into(), true),
            };
            seen.push(label);
            if last {
                return seen;
            }
        }
    }

    #[tokio::test]
    async fn the_turn_reader_reads_chunks_errors_and_ends_across_splits() {
        // A vLLM-shaped stream split mid-event, with a keep-alive comment and
        // a chunk that is not JSON.
        let seen = read_all(reader(
            vec![
                ": ping\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n",
                "\ndata: not json\n\ndata: {\"choices\":[{\"delta\":{\"content\":\"lo\"},",
                "\"finish_reason\":\"abort\"}]}\r\n\r\ndata: [DONE]\n\ndata: {\"choices\":[]}\n\n",
            ],
            None,
        ))
        .await;
        assert_eq!(
            seen,
            vec![
                r#"chunk None {"content":"Hel"}"#,
                r#"chunk Some(502) {"content":"lo"}"#,
                "done",
            ]
        );
        // The upstream's own error event, as it sent it.
        let seen = read_all(reader(
            vec!["data: {\"error\": {\"message\": \"x\", \"code\": 503}}\n\n"],
            None,
        ))
        .await;
        assert_eq!(
            seen,
            vec![r#"error 503 {"error": {"message": "x", "code": 503}}"#]
        );
        // A stream that closes without [DONE] ends; one that goes silent is cut.
        assert_eq!(
            read_all(reader(vec!["data: {}\n\n"], None)).await,
            vec!["chunk None null", "end"]
        );
        let silent = TurnReader::from_stream(
            Box::pin(futures_util::stream::pending()),
            Some(Duration::from_millis(20)),
        );
        assert_eq!(read_all(silent).await, vec!["cut"]);
    }

    #[test]
    fn a_turn_forwards_text_chunks_as_before_and_keeps_tool_calls() {
        let chunks = [
            json!({"id": "c1", "model": "m", "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": null}]}),
            json!({"id": "c1", "model": "m", "choices": [{"index": 0, "delta": {"content": "Hi"}, "finish_reason": null}]}),
            json!({"id": "c1", "model": "m", "choices": [{"index": 0, "delta": {"reasoning": "hm"}, "finish_reason": null}]}),
            json!({"id": "c1", "model": "m", "choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "t", "function": {"name": "search", "arguments": "{}"}}]}, "finish_reason": null}]}),
            json!({"id": "c1", "model": "m", "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 5, "completion_tokens": 2}}),
        ];
        let mut turn = Turn::new("up", true);
        let mut role_sent = false;
        let mut frames = Vec::new();
        for c in &chunks {
            frames.extend(turn.take(c.clone(), &mut role_sent, 7).0);
        }
        // The role chunk once, then each text chunk without tool calls or a
        // finish reason, byte for byte as the loop sent them before.
        let forwarded = |v: &Value| {
            let mut v = v.clone();
            v["choices"][0]["delta"]
                .as_object_mut()
                .unwrap()
                .remove("tool_calls");
            v["choices"][0]["finish_reason"] = Value::Null;
            format!("data: {v}\n\n")
        };
        assert_eq!(
            frames,
            vec![
                role_chunk("c1", "m", 7),
                forwarded(&chunks[1]),
                forwarded(&chunks[2])
            ]
        );
        assert_eq!(turn.content, "Hi");
        assert_eq!(turn.chars, 4);
        assert_eq!(turn.finish_reason, Some(json!("tool_calls")));
        assert_eq!(turn.usage, Some(UpstreamUsage::uncached(5, 2)));
        let (acc, sent) = turn.tools.unwrap();
        assert_eq!(acc[&0].name, "search");
        assert_eq!(sent.len(), 1);
    }

    #[test]
    fn forward_delta_strips_tool_calls_and_nulls_finish_reason() {
        let v = json!({
            "choices": [{
                "delta": { "content": "hi", "tool_calls": [{ "index": 0 }] },
                "finish_reason": "stop"
            }]
        });
        let forwarded = forward_delta(v);
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
    fn role_chunk_opens_the_stream_as_the_assistant() {
        let c = role_chunk("id1", "m", 1);
        let v: Value = serde_json::from_str(c.trim_start_matches("data: ").trim_end()).unwrap();
        assert_eq!(v["choices"][0]["delta"]["role"], "assistant");
        assert_eq!(v["choices"][0]["delta"]["content"], "");
        assert!(v["choices"][0]["finish_reason"].is_null());
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

    #[test]
    fn chunk_usage_keeps_the_cached_prompt_tokens() {
        let u = json!({
            "prompt_tokens": 126008, "completion_tokens": 84,
            "prompt_tokens_details": { "cached_tokens": 125952 },
        });
        assert_eq!(chunk_usage(&u), UpstreamUsage::new(126008, 84, 125952));
        assert_eq!(
            chunk_usage(&json!({ "completion_tokens": 4, "prompt_tokens_details": null })),
            UpstreamUsage::uncached(0, 4)
        );
    }

    #[test]
    fn usage_chunk_reports_cached_tokens_only_when_there_are_some() {
        let parse = |sse: String| -> Value {
            serde_json::from_str(sse.trim_start_matches("data: ").trim_end()).unwrap()
        };
        let v = parse(usage_chunk("id1", "m", 1, UpstreamUsage::new(100, 9, 64)));
        assert_eq!(v["usage"]["prompt_tokens"], 100);
        assert_eq!(v["usage"]["total_tokens"], 109);
        assert_eq!(v["usage"]["prompt_tokens_details"]["cached_tokens"], 64);
        let v = parse(usage_chunk("id1", "m", 1, UpstreamUsage::uncached(100, 9)));
        assert!(v["usage"].get("prompt_tokens_details").is_none());
    }

    #[test]
    fn finalize_stats_carries_the_cached_prompt_tokens() {
        let stats = Arc::new(Mutex::new(StreamStats::default()));
        finalize_stats(&stats, Some(UpstreamUsage::new(500, 20, 448)));
        let s = stats.lock().unwrap();
        assert!(s.final_set);
        assert_eq!(
            (s.input_tokens, s.output_tokens, s.cached_input_tokens),
            (500, 20, 448)
        );
    }
}
