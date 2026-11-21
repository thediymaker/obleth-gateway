//! The Responses and Messages surfaces: each shim translates its request into
//! a chat completion, runs it through the pipeline, and translates the reply
//! (buffered or streamed) and its errors back into its own format.

use super::*;

/// Largest upstream answer a shim translates whole; a larger one passes back
/// untouched (see [`translate_body`]).
const TRANSLATED_RESPONSE_MAX: usize = 32 * 1024 * 1024;

/// Run a Responses request as a chat request and translate the answer back.
///
/// Everything between the two conversions is the ordinary pipeline: the same
/// routing, admission, boons and accounting a native chat call gets. That is
/// the whole point of translating at the edge rather than teaching the
/// pipeline a second request schema.
pub(super) async fn responses_shim(
    state: State<AppState>,
    req: Request<Body>,
    request_id: Uuid,
) -> Response<Body> {
    let (mut parts, body) = req.into_parts();
    // Authenticate before buffering: the body may be up to RESPONSES_BODY_MAX,
    // and an unauthenticated caller must not get to make us hold that. The
    // inner pipeline authenticates again (a moka hit) — this is only the door.
    let Some(secret) = bearer(&parts.headers) else {
        return error_json(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    let credential = match crate::jwt_auth::authenticate_credential(&state, &secret).await {
        Ok(cred) => {
            let client = crate::client::ClientInfo::of(&parts.extensions);
            if let Err(resp) = gate_resolved_key(&state, &cred.resolved, &client) {
                return resp;
            }
            cred
        }
        Err(resp) => return resp,
    };
    let bytes = match read_body(body).await {
        Ok(bytes) => bytes,
        Err(too_large) => return too_large.openai(),
    };
    let Ok(incoming) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return error_json(StatusCode::BAD_REQUEST, "invalid JSON body");
    };
    // Refused rather than ignored: these promise server-side conversation
    // storage, and this gateway keeps usage metadata, never prompt or output
    // text. Silently dropping them would leave a client trusting a history
    // that does not exist.
    if let Some(field) = crate::responses::unsupported_field(&incoming) {
        return error_json(
            StatusCode::BAD_REQUEST,
            &format!(
                "`{field}` needs server-side response storage, which this gateway does not do. \
                 Send the conversation in `input` on each request."
            ),
        );
    }

    let streaming = incoming.get("stream").and_then(serde_json::Value::as_bool) == Some(true);
    let model = incoming
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown")
        .to_string();
    let chat_body = crate::responses::to_chat_request(&incoming);

    // Re-point at the chat path so `is_chat_path` holds and every boon runs,
    // and record the surface the caller actually used so telemetry does not
    // report Responses traffic as chat.
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let Ok(uri) = format!("{}{query}", crate::responses::CHAT_PATH).parse() else {
        return error_json(
            StatusCode::INTERNAL_SERVER_ERROR,
            "request translation failed",
        );
    };
    parts.uri = uri;
    parts.headers.remove(header::CONTENT_LENGTH);
    // `reasoning.effort` has no chat spelling the translation keeps; the
    // pipeline translates it per model.
    parts.extensions.insert(PipelineInput {
        surface: Some(crate::responses::SURFACE),
        effort: crate::reasoning::Requested::of_surface(
            obleth_config::reasoning::responses_effort(&incoming),
        ),
        credential: Some(credential),
        body: Some(chat_body),
        ..PipelineInput::default()
    });
    let chat_req = Request::from_parts(parts, Body::empty());

    let mut resp = proxy_handler_inner(state, chat_req, request_id).await;
    if !resp.headers().contains_key("x-obleth-request-id") {
        if let Ok(value) = header::HeaderValue::from_str(&request_id.to_string()) {
            resp.headers_mut().insert("x-obleth-request-id", value);
        }
    }
    // An error from the pipeline (budget, admission, upstream) is already in
    // the shape a caller can read; re-dressing it as a Responses object would
    // only hide the status.
    if !resp.status().is_success() {
        return resp;
    }
    if should_translate_as_stream(streaming, resp.headers()) {
        translate_response_stream(resp, request_id, model)
    } else {
        translate_response_body(resp, request_id).await
    }
}

/// Buffer a translated chat reply and hand back the Responses object.
async fn translate_response_body(resp: Response<Body>, request_id: Uuid) -> Response<Body> {
    translate_body(
        resp,
        || error_json(StatusCode::BAD_GATEWAY, "upstream response too large"),
        |chat| crate::responses::from_chat_response(chat, &request_id.to_string()),
    )
    .await
}

/// Re-emit a chat SSE stream as Responses events, frame by frame.
fn translate_response_stream(
    resp: Response<Body>,
    request_id: Uuid,
    model: String,
) -> Response<Body> {
    translate_stream(
        resp,
        crate::responses::StreamTranslator::new(&request_id.to_string(), &model),
    )
}

/// A shim's stream translator as the shared driver sees it: chat chunks in,
/// frames of the shim's own event format out.
trait StreamShim: Send + 'static {
    fn on_chunk(&mut self, chunk: &serde_json::Value) -> Vec<String>;
    /// A heartbeat while the upstream is silent.
    fn ping(&mut self) -> Vec<String>;
    /// Close a stream whose upstream broke off; `finish` is a no-op after it.
    fn fail(&mut self, message: &str) -> Vec<String>;
    fn finish(&mut self) -> Vec<String>;
}

impl StreamShim for crate::responses::StreamTranslator {
    fn on_chunk(&mut self, chunk: &serde_json::Value) -> Vec<String> {
        self.on_chunk(chunk)
    }
    fn ping(&mut self) -> Vec<String> {
        self.ping()
    }
    fn fail(&mut self, message: &str) -> Vec<String> {
        self.fail(message)
    }
    fn finish(&mut self) -> Vec<String> {
        self.finish()
    }
}

impl StreamShim for crate::messages::AnthropicStreamTranslator {
    fn on_chunk(&mut self, chunk: &serde_json::Value) -> Vec<String> {
        self.on_chunk(chunk)
    }
    fn ping(&mut self) -> Vec<String> {
        self.ping()
    }
    fn fail(&mut self, message: &str) -> Vec<String> {
        self.fail("api_error", message)
    }
    fn finish(&mut self) -> Vec<String> {
        self.finish()
    }
}

/// Interval on which an idle translated stream gets a heartbeat. The driver
/// below exists only once `proxy_handler_inner` has already returned a
/// streaming response, so it covers gaps between upstream chunks (e.g. a slow
/// decode) — it starts nothing early and cannot cover an admission queue wait
/// or buffered-boon work, which both happen before this function is even called.
const STREAM_PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Re-emit a chat SSE stream through `translator`, frame by frame, with a
/// heartbeat while the upstream is silent. Shared by the Responses and
/// Messages shims so a fix on one surface is a fix on both.
fn translate_stream<T: StreamShim>(resp: Response<Body>, translator: T) -> Response<Body> {
    translate_stream_with(resp, translator, STREAM_PING_INTERVAL)
}

fn translate_stream_with<T: StreamShim>(
    resp: Response<Body>,
    mut translator: T,
    ping_interval: std::time::Duration,
) -> Response<Body> {
    let (parts, body) = resp.into_parts();
    let stream = async_stream::stream! {
        let mut upstream = body.into_data_stream();
        // Bytes in, whole events out: an event is decoded only once it is
        // complete, so a character split across chunks is never cut in two.
        let mut events = crate::sse::Splitter::unbounded();
        let mut broke_off = false;
        loop {
            let item = match tokio::time::timeout(ping_interval, upstream.next()).await {
                Ok(item) => item,
                Err(_) => {
                    // Silence, not failure: a gap between upstream chunks (a
                    // slow decode step) is not itself an error. The heartbeat
                    // keeps the client's own idle timeout from firing while
                    // the upstream is still working.
                    for frame in translator.ping() {
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
                    }
                    continue;
                }
            };
            let Some(item) = item else { break };
            let Ok(chunk) = item else {
                // The pipeline aborts its body when the upstream stream
                // breaks; say so instead of closing as complete.
                for frame in translator.fail("upstream stream ended before the response finished") {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
                }
                broke_off = true;
                break;
            };
            events.push(&chunk);
            while let Some(event) = events.next_event() {
                for frame in translate_event(&mut translator, event) {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
                }
            }
        }
        // A last event the upstream closed without a blank line.
        if !broke_off {
            if let Some(crate::sse::Piece::Event(event)) = events.finish() {
                for frame in translate_event(&mut translator, event) {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
                }
            }
        }
        // `fail` followed by `finish` is safe: `finish` is a no-op once `fail`
        // has closed the translator.
        for frame in translator.finish() {
            yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
        }
    };
    translated_response(parts, "text/event-stream", Body::from_stream(stream))
}

/// The frames one upstream event translates to; none for `[DONE]`, a
/// heartbeat or data that is not JSON.
fn translate_event<T: StreamShim>(translator: &mut T, event: &[u8]) -> Vec<String> {
    let Some(payload) = crate::sse::data_text(event) else {
        return Vec::new();
    };
    if payload == "[DONE]" {
        return Vec::new();
    }
    match serde_json::from_str::<serde_json::Value>(&payload) {
        Ok(value) => translator.on_chunk(&value),
        Err(_) => Vec::new(),
    }
}

/// Buffer a chat reply and hand back `translate` of it. A body that is not JSON
/// goes back untouched rather than dressed in an invented shape.
async fn translate_body(
    resp: Response<Body>,
    too_large: impl FnOnce() -> Response<Body>,
    translate: impl FnOnce(&serde_json::Value) -> serde_json::Value,
) -> Response<Body> {
    let (parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, TRANSLATED_RESPONSE_MAX).await else {
        return too_large();
    };
    let Ok(chat) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let translated = translate(&chat);
    translated_response(
        parts,
        "application/json",
        Body::from(translated.to_string()),
    )
}

/// The pipeline's response with its body replaced: the status and headers
/// stay (request id, rate limits), the length no longer applies, and there
/// is exactly one Content-Type.
fn translated_response(
    parts: http::response::Parts,
    content_type: &'static str,
    body: Body,
) -> Response<Body> {
    let mut resp = Response::from_parts(parts, body);
    let headers = resp.headers_mut();
    headers.remove(header::CONTENT_LENGTH);
    headers.insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static(content_type),
    );
    resp
}

/// Anthropic-shaped error response. Status and `Retry-After` are the
/// pipeline's; only the body shape changes.
pub(super) fn anthropic_error(
    status: StatusCode,
    message: &str,
    retry_after: Option<&str>,
) -> Response<Body> {
    let body = crate::messages::error_envelope(crate::messages::error_type_for(status), message);
    let mut builder = Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(ra) = retry_after {
        builder = builder.header(header::RETRY_AFTER, ra);
    }
    builder
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

/// Whether an upstream's 400 message is a context-length rejection in its
/// own words. This surface's own admission clamp (`clamp_max_tokens`) is
/// meant to catch this before it ever reaches an upstream, but it can only
/// clamp against a window it actually knows (`context_window_for` returns
/// `None` for an unregistered `context_window`, or a served model this
/// gateway has never learned the window of), so the raw upstream 400 is
/// still a live path. vLLM, TGI and llama.cpp each phrase it differently,
/// and none of them use Anthropic's wording, so Claude Code's automatic
/// context-compaction — keyed on `prompt is too long` — never fires on the
/// unmodified message. Matched case-insensitively against a short, specific
/// list rather than any 400: this must not relabel an unrelated bad request.
pub(super) fn looks_like_context_length_error(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    ["context length", "maximum context", "too many tokens"]
        .iter()
        .any(|needle| lower.contains(needle))
}

/// Re-dress a pipeline error response (OpenAI envelope) as an Anthropic one.
async fn redress_error(resp: Response<Body>) -> Response<Body> {
    let (parts, body) = resp.into_parts();
    let bytes = axum::body::to_bytes(body, 64 * 1024)
        .await
        .unwrap_or_default();
    let message = crate::messages::upstream_error_message(&bytes);
    let message = if message.is_empty() {
        // A body over the cap, or one that failed to read at all, yields an
        // empty message from `upstream_error_message`; the status line still
        // says something, so fall back to it rather than ship `message: ""`.
        parts
            .status
            .canonical_reason()
            .unwrap_or("error")
            .to_string()
    } else if parts.status == StatusCode::BAD_REQUEST && looks_like_context_length_error(&message) {
        // See `looks_like_context_length_error`: this is the fallback path
        // for a context overflow this surface's own clamp did not catch.
        format!("prompt is too long: {message}")
    } else {
        message
    };
    let retry_after = parts
        .headers
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let mut out = anthropic_error(parts.status, &message, retry_after.as_deref());
    if let Some(id) = parts.headers.get("x-obleth-request-id") {
        out.headers_mut().insert("x-obleth-request-id", id.clone());
    }
    out
}

/// Run a Messages request as a chat request and translate the answer back —
/// the same edge-translation shape as [`responses_shim`], see its module doc.
pub(super) async fn messages_shim(
    state: State<AppState>,
    req: Request<Body>,
    request_id: Uuid,
) -> Response<Body> {
    let front = match messages_front(&state, req, false).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    let MessagesFront {
        mut parts,
        chat_body,
        requested_model,
        streaming,
        credential,
        effort,
    } = front;
    let query = strip_beta_query(parts.uri.query())
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let Ok(uri) = format!("{}{query}", crate::responses::CHAT_PATH).parse() else {
        return anthropic_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "request translation failed",
            None,
        );
    };
    parts.uri = uri;
    parts.headers.remove(header::CONTENT_LENGTH);
    // Anthropic-only; the upstream never defined these and forwarding them is
    // pointless at best (`anthropic-beta` also leaks which client library is
    // in use to a backend that has no use for the information).
    parts.headers.remove("anthropic-beta");
    parts.extensions.insert(PipelineInput {
        surface: Some(crate::messages::SURFACE),
        effort,
        credential: Some(credential),
        body: Some(chat_body),
        batch: None,
    });
    let chat_req = Request::from_parts(parts, Body::empty());

    let mut resp = proxy_handler_inner(state, chat_req, request_id).await;
    if !resp.headers().contains_key("x-obleth-request-id") {
        if let Ok(value) = header::HeaderValue::from_str(&request_id.to_string()) {
            resp.headers_mut().insert("x-obleth-request-id", value);
        }
    }
    if !resp.status().is_success() {
        return redress_error(resp).await;
    }
    let ctx = crate::messages::ResponseContext {
        request_id: request_id.to_string(),
        model: requested_model,
    };
    if should_translate_as_stream(streaming, resp.headers()) {
        translate_messages_stream(resp, ctx)
    } else {
        translate_messages_body(resp, ctx).await
    }
}

/// Whether the pipeline's 2xx response should go through the SSE translator.
/// An upstream that ignores `stream` and answers a plain JSON 200 has to fall
/// back to the buffered path instead: handing that body to the SSE
/// translator would find no `data:` lines and emit a well-formed but silently
/// empty message.
fn should_translate_as_stream(client_asked_to_stream: bool, headers: &HeaderMap) -> bool {
    const SSE: &str = "text/event-stream";
    client_asked_to_stream
        && headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            // Case-insensitive: a `Content-Type` is not case-sensitive per RFC
            // 9110, and some upstreams answer `Text/Event-Stream`.
            .and_then(|ct| ct.get(..SSE.len()))
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(SSE))
}

/// Drop the Anthropic-only `beta` query parameter before the request goes
/// upstream — the parallel of the `anthropic-beta` header stripped in
/// `messages_shim`. An unrecognised query parameter on `/v1/chat/completions`
/// is harmless to a real upstream, but there is no reason to forward it.
fn strip_beta_query(query: Option<&str>) -> Option<String> {
    let kept: Vec<&str> = query?
        .split('&')
        .filter(|pair| !pair.is_empty())
        .filter(|pair| pair.split('=').next() != Some("beta"))
        .collect();
    (!kept.is_empty()).then(|| kept.join("&"))
}

/// The chat-shaped body ready for the pipeline, the request parts to forward
/// it in, and the model name the client sent — assembled by `messages_front`.
struct MessagesFront {
    parts: http::request::Parts,
    chat_body: serde_json::Value,
    requested_model: String,
    streaming: bool,
    /// Authenticated and gated here, once.
    credential: crate::jwt_auth::Credential,
    /// `thinking` / `output_config.effort`, which chat has no spelling for.
    effort: Option<crate::reasoning::Requested>,
}

/// Anthropic's real `count_tokens` endpoint takes no `max_tokens` field, and
/// no SDK sends one, but `to_chat_request` treats it as mandatory for an
/// actual chat request. The tokenizer's `input_tokens` estimate never reads
/// `max_tokens`, so backfilling a placeholder here — only for `count_tokens`,
/// only when the field is absent — is invisible to the caller and never
/// reaches an upstream.
fn backfill_max_tokens_for_count_tokens(incoming: &mut serde_json::Value) {
    if let Some(obj) = incoming.as_object_mut() {
        obj.entry("max_tokens").or_insert(serde_json::json!(1));
    }
}

/// Auth, gate, body read and translation shared by `messages_shim` and
/// `count_tokens_shim`. `count_tokens` is true only for the latter — see
/// `backfill_max_tokens_for_count_tokens`.
// The error is a finished response, like `gate_resolved_key`'s.
#[allow(clippy::result_large_err)]
async fn messages_front(
    state: &AppState,
    req: Request<Body>,
    count_tokens: bool,
) -> Result<MessagesFront, Response<Body>> {
    let (parts, body) = req.into_parts();
    // Authenticate before buffering (same reasoning as `responses_shim`).
    let Some(secret) = bearer(&parts.headers) else {
        return Err(anthropic_error(
            StatusCode::UNAUTHORIZED,
            "missing api key",
            None,
        ));
    };
    let credential = match crate::jwt_auth::authenticate_credential(state, &secret).await {
        Ok(cred) => {
            let client = crate::client::ClientInfo::of(&parts.extensions);
            if let Err(resp) = gate_resolved_key(state, &cred.resolved, &client) {
                return Err(redress_error(resp).await);
            }
            cred
        }
        Err(resp) => return Err(redress_error(resp).await),
    };
    let bytes = read_body(body).await.map_err(BodyTooLarge::anthropic)?;
    let Ok(mut incoming) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return Err(anthropic_error(
            StatusCode::BAD_REQUEST,
            "invalid JSON body",
            None,
        ));
    };
    if count_tokens {
        backfill_max_tokens_for_count_tokens(&mut incoming);
    }
    let requested_model = incoming
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string();
    if requested_model.is_empty() {
        return Err(anthropic_error(
            StatusCode::BAD_REQUEST,
            "`model` is required",
            None,
        ));
    }
    let streaming = incoming.get("stream").and_then(serde_json::Value::as_bool) == Some(true);
    let mut chat_body = match crate::messages::to_chat_request(&incoming) {
        Ok(v) => v,
        Err(e) => return Err(anthropic_error(StatusCode::BAD_REQUEST, &e.message, None)),
    };
    let effort = obleth_config::reasoning::messages_effort(&incoming);
    let served = match resolve_messages_model(state, &requested_model).await {
        Some(name) => name,
        None => {
            return Err(anthropic_error(
                StatusCode::NOT_FOUND,
                &format!("model: {requested_model}"),
                None,
            ));
        }
    };
    // Anthropic clients — Claude Code above all — size `max_tokens` for the
    // model family the client believes it is talking to, not for whatever
    // this gateway actually routes an alias (or `auto`) to underneath.
    // Forwarded unclamped, a value sized for a 200k-context model 400s at a
    // smaller upstream (`prompt + max_tokens > context`) or, on `auto`, gets
    // every smaller candidate hard-filtered out by the router (503). Skipped
    // for `count_tokens`: its `max_tokens` is a backfilled placeholder, never
    // forwarded, and never dispatched to a pipeline that could reject it.
    if !count_tokens {
        let input_est = u64::from(state.tokenizer.estimate_request(&chat_body).input_tokens);
        let window = context_window_for(state, &served, effort.clone().ok().flatten()).await;
        let requested_max_tokens = chat_body
            .get("max_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(1);
        match clamp_max_tokens(requested_max_tokens, window, input_est) {
            Ok(clamped) => {
                if clamped != requested_max_tokens {
                    tracing::debug!(
                        requested = requested_max_tokens,
                        clamped,
                        input_est,
                        window,
                        "messages surface: clamped max_tokens to the served model's context window"
                    );
                    chat_body["max_tokens"] = serde_json::Value::from(clamped);
                }
            }
            Err(too_long) => {
                return Err(anthropic_error(
                    StatusCode::BAD_REQUEST,
                    &too_long.message(),
                    None,
                ))
            }
        }
    }
    chat_body["model"] = serde_json::Value::String(served);
    Ok(MessagesFront {
        parts,
        chat_body,
        requested_model,
        streaming,
        credential,
        effort: crate::reasoning::Requested::of_surface(effort),
    })
}

/// The context window to clamp `max_tokens` against for the model this
/// request will be served by: the resolved route's declared window for a
/// concrete model, or the largest window among the `auto` router's healthy
/// chat candidates when `served` is `AUTO_MODEL_NAME` (the pipeline still
/// picks the concrete model itself; this is only an upper bound so the
/// client's `max_tokens` cannot doom every candidate before routing runs).
/// `None` when no window is known — the caller leaves `max_tokens` alone
/// rather than clamp on a guess.
async fn context_window_for(
    state: &AppState,
    served: &str,
    effort: Option<obleth_config::ReasoningLevel>,
) -> Option<u64> {
    // A family is served by the member its level picks.
    if let Some(family) = state.families.get(served) {
        let (_, member) = family.member_for(effort);
        return Box::pin(context_window_for(state, member, effort)).await;
    }
    if served == crate::router::AUTO_MODEL_NAME {
        return state
            .model_registry
            .load()
            .iter()
            .filter(|c| c.healthy && c.model.model_type == obleth_config::DEFAULT_MODEL_TYPE)
            .map(|c| c.model.context_window)
            .filter(|&w| w > 0)
            .max()
            .map(|w| w as u64);
    }
    resolve_model(state, served)
        .await
        .map(|m| m.context_window)
        .filter(|&w| w > 0)
        .map(|w| w as u64)
}

/// The prompt alone already meets or exceeds the model's context window, so
/// no `max_tokens` value — clamped or not — would leave room for a reply.
#[derive(Debug, PartialEq)]
struct TooLong {
    input_tokens: u64,
    window: u64,
}

impl TooLong {
    /// Anthropic's own wording (Claude Code's automatic context-compaction
    /// keys its detection on this exact phrase, so it must not drift).
    fn message(&self) -> String {
        format!(
            "prompt is too long: {} tokens > {} maximum",
            self.input_tokens, self.window
        )
    }
}

/// Clamp a client-requested `max_tokens` to what the resolved model's context
/// window can actually hold. The margin scales with the estimate rather than
/// staying flat: `estimate_request`'s ~4-chars/token heuristic runs further
/// behind the true count the longer (and more tool/JSON-heavy) the prompt is,
/// so a request sized like Claude Code's needs far more slack than a short
/// chat message does. A window with no room left for even that margin is
/// treated the same as the prompt alone exceeding it — no `max_tokens` value
/// would leave real room for a reply, so this is the `too long` error, not a
/// clamp down to 1.
fn clamp_max_tokens(
    requested: u64,
    window: Option<u64>,
    input_tokens: u64,
) -> Result<u64, TooLong> {
    let Some(window) = window else {
        // No window known: nothing to clamp against, so leave the client's
        // value alone rather than guess.
        return Ok(requested);
    };
    if input_tokens >= window {
        return Err(TooLong {
            input_tokens,
            window,
        });
    }
    let margin = (input_tokens / 8).max(256);
    let remaining = window - input_tokens;
    if remaining <= margin {
        return Err(TooLong {
            input_tokens,
            window,
        });
    }
    Ok(requested.min(remaining - margin).max(1))
}

/// The alias the client named if the gateway knows it, else the configured
/// default for Anthropic clients, else nothing.
///
/// `AUTO_MODEL_NAME` ("auto") is never registered in Redis — it is the
/// router's reserved name, special-cased in `proxy_handler_inner` before any
/// `resolve_model` lookup — so it is passed through unchanged rather than
/// looked up, both for a client that asks for it directly and for an operator
/// who configured it as the surface's default.
async fn resolve_messages_model(state: &AppState, requested: &str) -> Option<String> {
    if requested == crate::router::AUTO_MODEL_NAME
        || resolve_model(state, requested).await.is_some()
        || state.families.get(requested).is_some()
    {
        return Some(requested.to_string());
    }
    let fallback = state.classifier.settings().messages_default_model.clone()?;
    if fallback == crate::router::AUTO_MODEL_NAME || resolve_model(state, &fallback).await.is_some()
    {
        tracing::debug!(requested, fallback = %fallback, "messages surface: unknown model served by the configured default");
        return Some(fallback);
    }
    None
}

/// Buffer a translated chat reply and hand back the Anthropic `message` object.
async fn translate_messages_body(
    resp: Response<Body>,
    ctx: crate::messages::ResponseContext,
) -> Response<Body> {
    translate_body(
        resp,
        || anthropic_error(StatusCode::BAD_GATEWAY, "upstream response too large", None),
        |chat| crate::messages::from_chat_response(chat, &ctx),
    )
    .await
}

/// Re-emit a chat SSE stream as Anthropic Messages events, frame by frame,
/// with a `ping` heartbeat while the upstream is silent.
fn translate_messages_stream(
    resp: Response<Body>,
    ctx: crate::messages::ResponseContext,
) -> Response<Body> {
    translate_stream(resp, crate::messages::AnthropicStreamTranslator::new(ctx))
}

/// `POST /v1/messages/count_tokens`: the gateway's own estimate of the prompt
/// as it would be sent. Not admitted, not budgeted, not recorded.
pub(super) async fn count_tokens_shim(
    state: State<AppState>,
    req: Request<Body>,
    _request_id: Uuid,
) -> Response<Body> {
    let front = match messages_front(&state, req, true).await {
        Ok(f) => f,
        Err(resp) => return resp,
    };
    let est = state.tokenizer.estimate_request(&front.chat_body);
    let body = serde_json::json!({"input_tokens": est.input_tokens});
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chat_reply(content_type: &str) -> Response<Body> {
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, content_type)
            .header(header::CONTENT_LENGTH, "999")
            .header("x-obleth-request-id", "r1")
            .body(Body::from(
                r#"{"id":"c","object":"chat.completion","model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"hi"},"finish_reason":"stop"}]}"#,
            ))
            .unwrap()
    }

    #[test]
    fn responses_shim_authenticates_before_reading_the_body() {
        // The shim buffers up to RESPONSES_BODY_MAX before the pipeline runs.
        // An unauthenticated caller must be turned away before that buffer is
        // filled, so the bearer check and key gate have to precede `to_bytes`.
        // There is no AppState harness in this crate (it needs live Redis and
        // ClickHouse), so the order is pinned at the source level.
        let src = include_str!("surfaces.rs");
        let start = src
            .find("async fn responses_shim(")
            .expect("responses_shim");
        let end = start
            + src[start..]
                .find("async fn translate_response_body(")
                .expect("end of responses_shim");
        let shim = &src[start..end];
        let read = shim.find("read_body(").expect("body read");
        for step in [
            "bearer(",
            "UNAUTHORIZED",
            "authenticate_credential(",
            "gate_resolved_key(",
        ] {
            let at = shim
                .find(step)
                .unwrap_or_else(|| panic!("`{step}` missing from responses_shim"));
            assert!(at < read, "`{step}` must run before the body is read");
        }
    }

    #[test]
    fn messages_front_authenticates_before_reading_the_body() {
        // Pinned on `messages_front` itself rather than on `messages_shim`:
        // both shims' auth ordering runs through the shared front half, and
        // a previous version of this pin only held because of where
        // `messages_front` happened to sit in the file relative to
        // `messages_shim`. See `each_shim_calls_messages_front_before_anything_else`
        // for the part that ties the shims to this function.
        let src = include_str!("surfaces.rs");
        let start = src
            .find("async fn messages_front(")
            .expect("messages_front present");
        let body = &src[start..];
        let end = body
            .find("\nasync fn resolve_messages_model(")
            .unwrap_or(body.len());
        let body = &body[..end];
        let pos = |needle: &str| {
            body.find(needle)
                .unwrap_or_else(|| panic!("{needle} in messages_front"))
        };
        let read = pos("read_body(");
        assert!(pos("bearer(") < read);
        assert!(pos("authenticate_credential(") < read);
        assert!(pos("gate_resolved_key(") < read);
    }

    #[test]
    fn each_shim_calls_messages_front_before_anything_else() {
        // `messages_shim` must authenticate before it ever reaches the
        // pipeline; `count_tokens_shim` must authenticate before it estimates
        // tokens. Both route auth through `messages_front`, so pinning that
        // call ahead of each shim's next meaningful step is what makes
        // `messages_front_authenticates_before_reading_the_body` binding for
        // callers, not just for the function in isolation.
        let src = include_str!("surfaces.rs");

        let shim_start = src
            .find("async fn messages_shim(")
            .expect("messages_shim present");
        let shim_end = shim_start
            + src[shim_start..]
                .find("\nasync fn messages_front(")
                .expect("end of messages_shim");
        let shim = &src[shim_start..shim_end];
        let front = shim
            .find("messages_front(")
            .expect("messages_shim calls messages_front");
        let dispatch = shim
            .find("proxy_handler_inner(")
            .expect("messages_shim dispatches to the pipeline");
        assert!(
            front < dispatch,
            "messages_shim must authenticate via messages_front before running the pipeline"
        );
        assert!(
            !shim[..front].contains("to_bytes("),
            "messages_shim must not read the body itself; only messages_front does"
        );

        let ct_start = src
            .find("async fn count_tokens_shim(")
            .expect("count_tokens_shim present");
        let ct_end = ct_start
            + src[ct_start..]
                .find("\n#[cfg(test)]")
                .expect("end of count_tokens_shim");
        let ct = &src[ct_start..ct_end];
        let front = ct
            .find("messages_front(")
            .expect("count_tokens_shim calls messages_front");
        let estimate = ct
            .find("estimate_request(")
            .expect("count_tokens_shim estimates tokens");
        assert!(
            front < estimate,
            "count_tokens_shim must authenticate via messages_front before estimating tokens"
        );
        assert!(
            !ct[..front].contains("to_bytes("),
            "count_tokens_shim must not read the body itself; only messages_front does"
        );
    }

    #[test]
    fn stream_translation_falls_back_to_buffered_when_upstream_ignored_stream() {
        // A `stream: true` request whose upstream answered plain JSON (no
        // `text/event-stream`) must not be handed to the SSE translator: it
        // would find no `data:` lines and emit a well-formed but empty
        // message. Same for a non-streaming request, trivially.
        let mut sse = HeaderMap::new();
        sse.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("text/event-stream"),
        );
        assert!(should_translate_as_stream(true, &sse));
        assert!(!should_translate_as_stream(false, &sse));

        let mut json = HeaderMap::new();
        json.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        assert!(!should_translate_as_stream(true, &json));

        assert!(!should_translate_as_stream(true, &HeaderMap::new()));
    }

    #[tokio::test]
    async fn a_translated_responses_body_carries_exactly_one_content_type() {
        let out = super::translate_response_body(chat_reply("application/json"), Uuid::nil()).await;
        let types: Vec<_> = out.headers().get_all(header::CONTENT_TYPE).iter().collect();
        assert_eq!(types, vec!["application/json"]);
        assert!(out.headers().get(header::CONTENT_LENGTH).is_none());
        assert_eq!(out.headers().get("x-obleth-request-id").unwrap(), "r1");
        let bytes = axum::body::to_bytes(out.into_body(), 1 << 20)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["object"], "response");
    }

    #[tokio::test]
    async fn a_stream_request_answered_with_plain_json_gets_a_translated_body() {
        // The upstream ignored `stream`: the reply is a JSON object, so it is
        // translated as a body, not fed to the SSE translator as an empty stream.
        let reply = chat_reply("application/json");
        assert!(!should_translate_as_stream(true, reply.headers()));
        let out = super::translate_response_body(reply, Uuid::nil()).await;
        let bytes = axum::body::to_bytes(out.into_body(), 1 << 20)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["output"][0]["content"][0]["text"], "hi");
    }

    #[tokio::test]
    async fn an_idle_upstream_gets_a_heartbeat_on_every_shim() {
        use futures_util::StreamExt;
        let interval = std::time::Duration::from_millis(50);
        for shim in ["responses", "messages"] {
            let silent = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from_stream(futures_util::stream::pending::<
                    Result<Bytes, std::io::Error>,
                >()))
                .unwrap();
            let out = if shim == "responses" {
                super::translate_stream_with(
                    silent,
                    crate::responses::StreamTranslator::new("r", "m"),
                    interval,
                )
            } else {
                super::translate_stream_with(
                    silent,
                    crate::messages::AnthropicStreamTranslator::new(
                        crate::messages::ResponseContext {
                            request_id: "r".into(),
                            model: "m".into(),
                        },
                    ),
                    interval,
                )
            };
            let types: Vec<_> = out.headers().get_all(header::CONTENT_TYPE).iter().collect();
            assert_eq!(types, vec!["text/event-stream"], "{shim}");
            let mut body = out.into_body().into_data_stream();
            // Nothing arrives for 15 s of (paused) time, then the heartbeat.
            let mut seen = String::new();
            while !seen.contains(if shim == "responses" {
                ": keep-alive"
            } else {
                "\"type\":\"ping\""
            }) {
                let chunk = tokio::time::timeout(std::time::Duration::from_secs(16), body.next())
                    .await
                    .expect("a frame within the ping interval")
                    .expect("stream open")
                    .unwrap();
                seen.push_str(std::str::from_utf8(&chunk).unwrap());
            }
        }
    }

    #[test]
    fn should_translate_as_stream_compares_content_type_case_insensitively() {
        let mut mixed_case = HeaderMap::new();
        mixed_case.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("Text/Event-Stream; charset=utf-8"),
        );
        assert!(should_translate_as_stream(true, &mixed_case));
    }

    #[test]
    fn anthropic_error_response_uses_envelope_and_keeps_status() {
        let resp = anthropic_error(StatusCode::TOO_MANY_REQUESTS, "slow down", Some("5"));
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(resp.headers().get(header::RETRY_AFTER).unwrap(), "5");
        assert_eq!(
            resp.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
    }

    #[test]
    fn count_tokens_backfills_max_tokens_so_to_chat_request_accepts_it() {
        // Real `count_tokens` callers (Claude Code, the Python/TS SDKs) never
        // send `max_tokens` — the field only exists for an actual chat
        // request. Without the backfill, `to_chat_request` rejects the body.
        let mut body = serde_json::json!({
            "model": "m",
            "messages": [{"role": "user", "content": "hi"}]
        });
        assert!(crate::messages::to_chat_request(&body).is_err());
        backfill_max_tokens_for_count_tokens(&mut body);
        assert_eq!(body["max_tokens"], 1);
        assert!(crate::messages::to_chat_request(&body).is_ok());
    }

    #[test]
    fn count_tokens_backfill_does_not_override_an_explicit_max_tokens() {
        let mut body = serde_json::json!({
            "model": "m",
            "max_tokens": 42,
            "messages": []
        });
        backfill_max_tokens_for_count_tokens(&mut body);
        assert_eq!(body["max_tokens"], 42);
    }

    #[test]
    fn resolve_messages_model_passes_auto_through_without_a_registry_lookup() {
        // `auto` is the router's reserved name, special-cased in
        // `proxy_handler_inner` before any `resolve_model` call, and it is
        // never registered in Redis, so this surface must recognise it
        // directly instead of 404ing it — for both a client that asks for it
        // and an operator who configured it as the surface's default. Pinned
        // at the source level: there is no AppState harness in this crate.
        let src = include_str!("surfaces.rs");
        let start = src
            .find("async fn resolve_messages_model(")
            .expect("resolve_messages_model present");
        let end = start
            + src[start..]
                .find("\nasync fn translate_messages_body(")
                .expect("end of resolve_messages_model");
        let body = &src[start..end];

        let requested_auto = body
            .find("requested == crate::router::AUTO_MODEL_NAME")
            .expect("the requested model is checked against AUTO_MODEL_NAME");
        let requested_lookup = body
            .find("resolve_model(state, requested)")
            .expect("the requested model is still looked up when it is not auto");
        assert!(requested_auto < requested_lookup);

        let fallback_auto = body
            .find("fallback == crate::router::AUTO_MODEL_NAME")
            .expect("the configured default is checked against AUTO_MODEL_NAME");
        let fallback_lookup = body
            .find("resolve_model(state, &fallback)")
            .expect("the configured default is still looked up when it is not auto");
        assert!(fallback_auto < fallback_lookup);
    }

    #[test]
    fn clamp_max_tokens_cases() {
        // No window known: nothing to clamp against, so the client's value
        // survives untouched, however large.
        assert_eq!(clamp_max_tokens(99_999, None, 500), Ok(99_999));

        // Fits comfortably: margin is `max(256, input/8)` = 256 here, well
        // inside the remaining room, so the request is unchanged.
        assert_eq!(clamp_max_tokens(50, Some(1_000), 100), Ok(50));

        // Requested more than the window leaves room for: clamped to what's
        // left after the prompt and the scaled margin (`max(256, 100/8)` =
        // 256 here, so budget = 1_000 - 100 - 256 = 644).
        assert_eq!(clamp_max_tokens(2_000, Some(1_000), 100), Ok(644));

        // A longer prompt scales the margin up past the 256 floor
        // (`8_000 / 8` = 1_000), so the same nominal headroom clamps harder
        // than a short prompt would.
        assert_eq!(clamp_max_tokens(5_000, Some(10_000), 8_000), Ok(1_000));

        // The prompt alone already meets or exceeds the window: no amount of
        // clamping leaves room for a reply, so this is an error, not a clamp
        // to 1.
        assert_eq!(
            clamp_max_tokens(50, Some(1_000), 1_000),
            Err(TooLong {
                input_tokens: 1_000,
                window: 1_000
            })
        );
        assert_eq!(
            clamp_max_tokens(50, Some(1_000), 1_500),
            Err(TooLong {
                input_tokens: 1_500,
                window: 1_000
            })
        );

        // The prompt fits, but leaves no room even for the margin
        // (input 800 -> margin 256; window 1_056 -> remaining exactly 256):
        // this is also the `too long` error, not a clamp down to 1.
        assert_eq!(
            clamp_max_tokens(10, Some(1_056), 800),
            Err(TooLong {
                input_tokens: 800,
                window: 1_056
            })
        );

        // One token more of remaining room than the margin needs: the budget
        // is exactly 1 and it is not an error.
        assert_eq!(clamp_max_tokens(10, Some(1_057), 800), Ok(1));
    }

    #[test]
    fn the_tools_payload_clamps_max_tokens_that_a_text_only_estimate_would_not() {
        // A 40,960-token window, a small text prompt, and a ~60 KiB `tools`
        // array (Claude Code sends its full tool list on every turn). The
        // text alone leaves `max_tokens` unclamped; the estimate, which
        // counts the tools payload, clamps it to what the window can hold.
        let window = Some(40_960u64);
        let text_estimate = 500u64;
        let requested = 32_000u64;

        let chat_body = serde_json::json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "f",
                    "description": "x".repeat(60 * 1024),
                    "parameters": {"type": "object"}
                }
            }]
        });

        // Text-only: nothing is clamped.
        assert_eq!(
            clamp_max_tokens(requested, window, text_estimate),
            Ok(requested)
        );

        // With the tools payload counted: clamped to less than requested.
        let combined = u64::from(
            obleth_tokenizer::Tokenizer::estimate_request(
                &obleth_tokenizer::HeuristicTokenizer::new(),
                &chat_body,
            )
            .input_tokens,
        );
        let clamped = clamp_max_tokens(requested, window, combined).expect("still fits");
        assert!(clamped < requested, "expected clamping, got {clamped}");
    }

    #[test]
    fn too_long_message_wording_matches_anthropics_exact_phrase() {
        // Claude Code's automatic context-compaction keys its detection on
        // this exact string; any drift here silently breaks that recovery.
        let err = TooLong {
            input_tokens: 205_000,
            window: 200_000,
        };
        assert_eq!(
            err.message(),
            "prompt is too long: 205000 tokens > 200000 maximum"
        );
    }

    #[test]
    fn strip_beta_query_drops_only_the_beta_parameter() {
        assert_eq!(strip_beta_query(None), None);
        assert_eq!(strip_beta_query(Some("beta=true")), None);
        assert_eq!(
            strip_beta_query(Some("beta=true&x=1")),
            Some("x=1".to_string())
        );
        assert_eq!(
            strip_beta_query(Some("x=1&beta=true&y=2")),
            Some("x=1&y=2".to_string())
        );
        assert_eq!(strip_beta_query(Some("x=1")), Some("x=1".to_string()));
    }

    #[tokio::test]
    async fn redress_error_falls_back_to_the_canonical_reason_when_the_body_is_empty() {
        let resp = Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Body::empty())
            .unwrap();
        let out = redress_error(resp).await;
        assert_eq!(out.status(), StatusCode::BAD_GATEWAY);
        let bytes = axum::body::to_bytes(out.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["message"], "Bad Gateway");
    }

    #[test]
    fn looks_like_context_length_error_matches_known_phrasings_case_insensitively() {
        assert!(looks_like_context_length_error(
            "This model's maximum context length is 4096 tokens. However, you requested 5000 tokens."
        ));
        assert!(looks_like_context_length_error("CONTEXT LENGTH exceeded"));
        assert!(looks_like_context_length_error(
            "too many tokens in the messages"
        ));
        assert!(!looks_like_context_length_error("`model` is required"));
        assert!(!looks_like_context_length_error("rate limit exceeded"));
    }

    #[tokio::test]
    async fn redress_error_rewrites_a_context_length_400_to_anthropics_wording() {
        // vLLM's own wording ("maximum context length"), not Anthropic's —
        // Claude Code's auto-compaction only fires on `prompt is too long`.
        let upstream_message =
            "This model's maximum context length is 4096 tokens. However, you requested 5000 tokens.";
        let body = serde_json::json!({"error": {"message": upstream_message}});
        let resp = Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from(body.to_string()))
            .unwrap();
        let out = redress_error(resp).await;
        assert_eq!(out.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(out.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            v["error"]["message"],
            format!("prompt is too long: {upstream_message}")
        );
    }

    #[tokio::test]
    async fn redress_error_leaves_an_unrelated_400_message_untouched() {
        let body = serde_json::json!({"error": {"message": "`model` is required"}});
        let resp = Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(Body::from(body.to_string()))
            .unwrap();
        let out = redress_error(resp).await;
        assert_eq!(out.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(out.into_body(), 1024).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["message"], "`model` is required");
    }
}
