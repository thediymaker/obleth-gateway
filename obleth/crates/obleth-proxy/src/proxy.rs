//! The data-plane request pipeline.
//!
//! resolve key -> estimate cost -> fairshare admit -> reserve
//! budget -> stream to upstream -> reconcile actual cost -> emit telemetry.
//!
//! The fairshare permit is held inside the response stream and released only
//! when the stream finishes, so concurrency accounting matches real upstream
//! occupancy including streaming time.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{header, HeaderMap, Method, Request, Response, StatusCode};
use axum::response::IntoResponse;
use futures_util::StreamExt;
use obleth_config::{Admission, ResolvedEndpoint, ResolvedKey, ResolvedModel, UsageRecord};
use obleth_tokenizer::{CostEstimate, Tokenizer};
use tokio::time::timeout;
use tracing::Instrument;
use uuid::Uuid;

use crate::state::AppState;

pub(crate) const BODY_LIMIT: usize = 64 * 1024 * 1024;
const TAIL_CAP: usize = 16 * 1024;
/// Upper bound on a response we are willing to cache. Larger responses stream
/// through uncached so the cache can't be used to balloon Redis memory.
const CACHE_MAX_BYTES: usize = 512 * 1024;
/// Largest upstream completion a response-transforming boon will rewrite.
/// Bigger bodies pass through verbatim (fail-open).
const BOON_BUFFER_MAX: usize = 4 * 1024 * 1024;
/// Cap on the upstream error body recorded into a request trace. The body is
/// captured to explain *why* a backend rejected a request, but it can echo a
/// slice of the request and traces are retained, so the stored snippet is
/// bounded. The client still receives the full, untruncated body.
const ERROR_BODY_TRACE_CAP: usize = 4 * 1024;
/// Floor for the per-request upstream timeout when a response-transforming
/// boon is active: the upstream call is forced non-streaming, so the timeout
/// bounds the whole generation instead of time-to-first-byte.
const BOON_MIN_TIMEOUT: Duration = Duration::from_secs(120);
/// Tells an intermediary reverse proxy (nginx / ingress-nginx) not to buffer
/// the response, so streamed SSE tokens reach the client as they are produced
/// instead of in proxy-buffer-sized bursts. Harmless when no such proxy is in
/// front of the gateway. (HAProxy honours `option http-no-delay` instead.)
const NO_BUFFER_HEADER: (&str, &str) = ("x-accel-buffering", "no");
/// Fixed, short delay before the one bonus retry granted to a connection-level
/// upstream failure (a stale pooled keep-alive socket). Long enough to let a
/// fresh connection replace the dead one, short enough to stay invisible in TTFT.
const CONN_RETRY_BACKOFF: Duration = Duration::from_millis(50);
/// Default bound on a request's wait in the fairshare queue
/// (`OBLETH_ADMISSION_TIMEOUT_SECS`). Without one, a saturated pool parks
/// callers indefinitely.
const DEFAULT_ADMISSION_TIMEOUT: Duration = Duration::from_secs(60);
/// `Retry-After` sent with an admission timeout. `pub(crate)` so other
/// admission call sites (e.g. `verdicts`) send the same value.
pub(crate) const ADMISSION_RETRY_AFTER_SECS: &str = "5";

/// Parse `OBLETH_ADMISSION_TIMEOUT_SECS`; unset, unparseable, or zero falls
/// back to the default.
fn parse_admission_timeout(raw: Option<&str>) -> Duration {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|s| *s > 0)
        .map(Duration::from_secs)
        .unwrap_or(DEFAULT_ADMISSION_TIMEOUT)
}

pub(crate) fn admission_timeout() -> Duration {
    static TIMEOUT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();
    *TIMEOUT.get_or_init(|| {
        parse_admission_timeout(
            std::env::var("OBLETH_ADMISSION_TIMEOUT_SECS")
                .ok()
                .as_deref(),
        )
    })
}

/// The canonical spelling of a POST path, when it differs from the request's.
///
/// Repeated and trailing slashes are removed, and the bare `/responses`, `/messages`,
/// `/messages/count_tokens`, and `/verdicts` spellings map to their `/v1` forms. Without this,
/// `/v1/responses/` or `/responses` skipped the translation shims and was
/// forwarded to the upstream's own endpoint, so a tenant's guardrails never saw
/// it, and `/v1/chat/completions/` lost its chat classification (and with it
/// output guardrails and every chat-only boon).
fn canonical_post_path(path: &str) -> Option<String> {
    // Runs of `/` collapse to one, so `//v1/responses` cannot skip the shim
    // (the upstream URL builder strips leading slashes and would forward it).
    let mut collapsed = String::with_capacity(path.len());
    for c in path.chars() {
        if !(c == '/' && collapsed.ends_with('/')) {
            collapsed.push(c);
        }
    }
    let trimmed = collapsed.trim_end_matches('/');
    let mapped = match trimmed {
        "/responses" => crate::responses::RESPONSES_PATH,
        "/messages" => crate::messages::MESSAGES_PATH,
        "/messages/count_tokens" => crate::messages::COUNT_TOKENS_PATH,
        "/verdicts" => crate::verdicts::VERDICTS_PATH,
        other => other,
    };
    (!mapped.is_empty() && mapped != path).then(|| mapped.to_string())
}

pub async fn proxy_handler(state: State<AppState>, mut req: Request<Body>) -> Response<Body> {
    let request_id = Uuid::new_v4();
    // The surface marker is the gateway's own annotation (see
    // `crate::responses::SURFACE_HEADER`); stripped here unconditionally so a
    // caller cannot stamp a direct chat call as Responses traffic and pollute
    // the adoption metric. The shim re-inserts it after this strip.
    req.headers_mut().remove(crate::responses::SURFACE_HEADER);
    if req.method() == Method::POST {
        if let Some(canonical) = canonical_post_path(req.uri().path()) {
            let pq = match req.uri().query() {
                Some(q) => format!("{canonical}?{q}"),
                None => canonical,
            };
            if let Ok(uri) = pq.parse() {
                *req.uri_mut() = uri;
            }
        }
    }
    // The router serves the exact `/v1/verdicts` path; its other spellings
    // arrive here and must reach the same handler, never the upstream.
    if req.method() == Method::POST && req.uri().path() == crate::verdicts::VERDICTS_PATH {
        return crate::verdicts::handler(state, req).await;
    }
    // `/v1/responses` is served by translating to chat completions and back,
    // around the unchanged pipeline — see `crate::responses`.
    if req.method() == Method::POST && req.uri().path() == crate::responses::RESPONSES_PATH {
        return responses_shim(state, req, request_id).await;
    }
    // `/v1/messages` (Anthropic Messages API) is served the same way — see
    // `crate::messages`.
    if req.method() == Method::POST {
        match req.uri().path() {
            crate::messages::MESSAGES_PATH => return messages_shim(state, req, request_id).await,
            crate::messages::COUNT_TOKENS_PATH => {
                return count_tokens_shim(state, req, request_id).await
            }
            _ => {}
        }
    }
    let mut resp = proxy_handler_inner(state, req, request_id).await;
    // Ensure every response — including error paths that build their own response —
    // carries the request id so callers (e.g. the Charo model-test console) can always
    // fetch the request's trace. Success/stream/cache paths set this already; this is a
    // no-op for them and only fills it in for the error branches.
    if !resp.headers().contains_key("x-obleth-request-id") {
        if let Ok(value) = header::HeaderValue::from_str(&request_id.to_string()) {
            resp.headers_mut().insert("x-obleth-request-id", value);
        }
    }
    resp
}

/// Largest `/v1/responses` body accepted. The translated body is held in
/// memory, and the pipeline's own limits apply after that.
const RESPONSES_BODY_MAX: usize = 32 * 1024 * 1024;

/// Run a Responses request as a chat request and translate the answer back.
///
/// Everything between the two conversions is the ordinary pipeline: the same
/// routing, admission, boons and accounting a native chat call gets. That is
/// the whole point of translating at the edge rather than teaching the
/// pipeline a second request schema.
async fn responses_shim(
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
    match crate::jwt_auth::authenticate_credential(&state, &secret).await {
        Ok(cred) => {
            if let Err(resp) = gate_resolved_key(&state, &cred.resolved) {
                return resp;
            }
        }
        Err(resp) => return resp,
    }
    let Ok(bytes) = axum::body::to_bytes(body, RESPONSES_BODY_MAX).await else {
        return error_json(
            StatusCode::BAD_REQUEST,
            "request body too large or unreadable",
        );
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
    let chat_bytes = match serde_json::to_vec(&chat_body) {
        Ok(b) => b,
        Err(_) => {
            return error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                "request translation failed",
            )
        }
    };

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
    if let Ok(v) = header::HeaderValue::from_str("responses") {
        parts.headers.insert(crate::responses::SURFACE_HEADER, v);
    }
    let chat_req = Request::from_parts(parts, Body::from(chat_bytes));

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
    if streaming {
        translate_response_stream(resp, request_id, model)
    } else {
        translate_response_body(resp, request_id).await
    }
}

/// Buffer a translated chat reply and hand back the Responses object.
async fn translate_response_body(resp: Response<Body>, request_id: Uuid) -> Response<Body> {
    let (parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, RESPONSES_BODY_MAX).await else {
        return error_json(StatusCode::BAD_GATEWAY, "upstream response too large");
    };
    let Ok(chat) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        // Not JSON: hand it back untouched rather than inventing a shape.
        return Response::from_parts(parts, Body::from(bytes));
    };
    let translated = crate::responses::from_chat_response(&chat, &request_id.to_string());
    let mut builder = Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        if name != header::CONTENT_LENGTH {
            builder = builder.header(name, value);
        }
    }
    builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(translated.to_string()))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

/// Re-emit a chat SSE stream as Responses events, frame by frame.
fn translate_response_stream(
    resp: Response<Body>,
    request_id: Uuid,
    model: String,
) -> Response<Body> {
    let (parts, body) = resp.into_parts();
    let id = request_id.to_string();
    let stream = async_stream::stream! {
        let mut translator = crate::responses::StreamTranslator::new(&id, &model);
        let mut upstream = body.into_data_stream();
        // Bytes in, complete lines out — see `drain_sse_data_lines` for why
        // the split happens on bytes rather than decoded text.
        let mut buffer: Vec<u8> = Vec::new();
        while let Some(item) = upstream.next().await {
            let Ok(chunk) = item else {
                // The pipeline aborts its body when the upstream stream
                // breaks; say so instead of closing with `completed`.
                for frame in translator.fail("upstream stream ended before the response finished") {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
                }
                break;
            };
            buffer.extend_from_slice(&chunk);
            for payload in crate::responses::drain_sse_data_lines(&mut buffer) {
                if payload == "[DONE]" {
                    continue;
                }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else { continue };
                for frame in translator.on_chunk(&value) {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
                }
            }
        }
        for frame in translator.finish() {
            yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
        }
    };
    let mut builder = Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        if name != header::CONTENT_LENGTH && name != header::CONTENT_TYPE {
            builder = builder.header(name, value);
        }
    }
    builder
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

/// Anthropic-shaped error response. Status and `Retry-After` are the
/// pipeline's; only the body shape changes.
fn anthropic_error(status: StatusCode, message: &str, retry_after: Option<&str>) -> Response<Body> {
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
fn looks_like_context_length_error(message: &str) -> bool {
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
async fn messages_shim(
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
    } = front;
    let Ok(chat_bytes) = serde_json::to_vec(&chat_body) else {
        return anthropic_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "request translation failed",
            None,
        );
    };
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
    if let Ok(v) = header::HeaderValue::from_str(crate::messages::SURFACE) {
        parts.headers.insert(crate::responses::SURFACE_HEADER, v);
    }
    let chat_req = Request::from_parts(parts, Body::from(chat_bytes));

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
    match crate::jwt_auth::authenticate_credential(state, &secret).await {
        Ok(cred) => {
            if let Err(resp) = gate_resolved_key(state, &cred.resolved) {
                return Err(redress_error(resp).await);
            }
        }
        Err(resp) => return Err(redress_error(resp).await),
    }
    // `axum::body::to_bytes` fails only past the cap (a malformed transport
    // read surfaces earlier, from `into_parts`/the connection itself), so
    // every failure here is the client's body being too large.
    let Ok(bytes) = axum::body::to_bytes(body, RESPONSES_BODY_MAX).await else {
        return Err(anthropic_error(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body too large",
            None,
        ));
    };
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
        let text_est = state.tokenizer.estimate_request(&chat_body).input_tokens as u64;
        let input_est = messages_input_estimate(&chat_body, text_est);
        let window = context_window_for(state, &served).await;
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
async fn context_window_for(state: &AppState, served: &str) -> Option<u64> {
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

/// `estimate_request` counts message text only. That undercounts badly for a
/// tool-heavy client: Claude Code's own `tools` array runs 10-20k tokens by
/// itself, and an image part's few bytes of URL are nothing like its real
/// cost once decoded. Left uncorrected, the clamp below admits a `max_tokens`
/// the upstream still rejects on the real (text + tools + images) prompt.
/// Not used for `count_tokens`'s reported figure: that endpoint's contract is
/// "what the tokenizer counts", not this surface's own admission math.
fn messages_input_estimate(chat_body: &serde_json::Value, text_estimate: u64) -> u64 {
    // `to_string().len() / 3`, not the tokenizer's own chars-per-token-4
    // heuristic: `tools` is JSON schema (short, punctuation- and
    // digit-heavy — `{`, `"`, `:`, field names), which tokenizes denser than
    // prose does, roughly 3 chars/token rather than 4. No finer estimate
    // exists without a real tokenizer, and this only needs to be in the
    // right order of magnitude to keep the clamp from admitting an
    // upstream-rejecting value.
    let tools_tokens = chat_body
        .get("tools")
        .map(|t| t.to_string().len() as u64 / 3)
        .unwrap_or(0);
    let image_count = chat_body
        .get("messages")
        .and_then(serde_json::Value::as_array)
        .map(|messages| {
            messages
                .iter()
                .filter_map(|m| m.get("content").and_then(serde_json::Value::as_array))
                .flatten()
                .filter(|part| {
                    part.get("type").and_then(serde_json::Value::as_str) == Some("image_url")
                })
                .count() as u64
        })
        .unwrap_or(0);
    // A round number, not a measurement: real per-image cost depends on
    // resolution and the vision encoder, which this gateway has no way to
    // know ahead of the upstream. Large enough that a handful of images still
    // pushes the clamp, small enough not to starve `max_tokens` on a single
    // screenshot.
    text_estimate + tools_tokens + image_count * 1500
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
    let (parts, body) = resp.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, RESPONSES_BODY_MAX).await else {
        return anthropic_error(StatusCode::BAD_GATEWAY, "upstream response too large", None);
    };
    let Ok(chat) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        // Not JSON: hand it back untouched rather than inventing a shape.
        return Response::from_parts(parts, Body::from(bytes));
    };
    let translated = crate::messages::from_chat_response(&chat, &ctx);
    let mut builder = Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        if name != header::CONTENT_LENGTH && name != header::CONTENT_TYPE {
            builder = builder.header(name, value);
        }
    }
    builder
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(translated.to_string()))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

/// Interval on which an idle Messages stream gets a `ping` frame. This loop
/// exists only once `proxy_handler_inner` has already returned a streaming
/// response, so it covers gaps between upstream chunks (e.g. a slow decode)
/// — it starts nothing early and cannot cover an admission queue wait or
/// buffered-boon work, which both happen before this function is even called.
const MESSAGES_PING_INTERVAL: std::time::Duration = std::time::Duration::from_secs(15);

/// Re-emit a chat SSE stream as Anthropic Messages events, frame by frame,
/// with a `ping` heartbeat while the upstream is silent.
fn translate_messages_stream(
    resp: Response<Body>,
    ctx: crate::messages::ResponseContext,
) -> Response<Body> {
    let (parts, body) = resp.into_parts();
    let stream = async_stream::stream! {
        let mut translator = crate::messages::AnthropicStreamTranslator::new(ctx);
        let mut upstream = body.into_data_stream();
        let mut buffer: Vec<u8> = Vec::new();
        loop {
            let item = match tokio::time::timeout(MESSAGES_PING_INTERVAL, upstream.next()).await {
                Ok(item) => item,
                Err(_) => {
                    // Silence, not failure: a gap between upstream chunks (a
                    // slow decode step) is not itself an error. Sending a
                    // `ping` is what keeps the client's own idle timeout from
                    // firing while the upstream is still working.
                    for frame in translator.ping() {
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
                    }
                    continue;
                }
            };
            let Some(item) = item else { break };
            let Ok(chunk) = item else {
                for frame in translator.fail("api_error", "upstream stream ended before the response finished") {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
                }
                break;
            };
            buffer.extend_from_slice(&chunk);
            for payload in crate::responses::drain_sse_data_lines(&mut buffer) {
                if payload == "[DONE]" { continue; }
                let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else { continue };
                for frame in translator.on_chunk(&value) {
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
                }
            }
        }
        // `fail` followed by `finish` is safe on both paths: `finish` is a
        // no-op once `fail` has set the translator's `done` flag.
        for frame in translator.finish() {
            yield Ok::<Bytes, std::io::Error>(Bytes::from(frame));
        }
    };
    // Same header handling as `translate_response_stream`.
    let mut builder = Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        if name != header::CONTENT_LENGTH && name != header::CONTENT_TYPE {
            builder = builder.header(name, value);
        }
    }
    builder
        .header(header::CONTENT_TYPE, "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

/// `POST /v1/messages/count_tokens`: the gateway's own estimate of the prompt
/// as it would be sent. Not admitted, not budgeted, not recorded.
async fn count_tokens_shim(
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

#[tracing::instrument(
    skip_all,
    name = "proxy_request",
    fields(session.id = tracing::field::Empty, session.id.source = tracing::field::Empty)
)]
async fn proxy_handler_inner(
    State(state): State<AppState>,
    req: Request<Body>,
    request_id: Uuid,
) -> Response<Body> {
    let request_start = Instant::now();
    let proxy_start_ms = crate::tracer::now_ms();
    let (parts, body) = req.into_parts();
    let method = parts.method;
    let path = parts.uri.path().to_string();
    let query = parts
        .uri
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let headers = parts.headers;

    // ---- reject path traversal before any upstream work ----
    if has_path_traversal(&path) {
        return error_json(StatusCode::BAD_REQUEST, "invalid request path");
    }

    // ---- auth ----
    let Some(secret) = bearer(&headers) else {
        return error_json(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    let auth_start = crate::tracer::now_ms();
    // Shared with the MCP handler (`crate::mcp::mcp_handler`) so both surfaces
    // authenticate identically: JWT path when the feature is on and the
    // credential looks like a JWT, else the existing secret-key path.
    let (resolved, device_id, auth_kind) =
        match crate::jwt_auth::authenticate_credential(&state, &secret).await {
            Ok(cred) => (cred.resolved, cred.device_id, cred.auth_kind),
            Err(resp) => return resp,
        };
    let auth_duration = (crate::tracer::now_ms() - auth_start) as u32;
    if let Err(resp) = gate_resolved_key(&state, &resolved) {
        return resp;
    }

    // ---- Videos API follow-ups (poll, download, delete, list) ----
    // They carry a job id, never a model, so they are served from the job
    // record written at create time: routed to the model and endpoint that
    // made the job, and "not found" for anyone who does not own it (another
    // tenant, or by default another key of this one). Reads of work the
    // create already paid for, so no admission, budget, or ledger row (see
    // `crate::videos`).
    if let Some(call) = crate::videos::follow_up(&method, &path) {
        let inbound = crate::videos::Inbound {
            method: &method,
            query: &query,
            headers: &headers,
        };
        return crate::videos::handle_follow_up(&state, &resolved, call, inbound, request_id).await;
    }
    let video_create = crate::videos::is_create(&method, &path);

    // ---- request flight-recorder tracer ----
    let mut tracer: Option<crate::tracer::SpanRecorder> = if resolved.tracing_enabled {
        tracing::debug!(request_id = %request_id, "tracing enabled — recording spans");
        Some(crate::tracer::SpanRecorder::new(
            request_id,
            proxy_start_ms,
            state.telemetry.clone(),
        ))
    } else {
        tracing::debug!(request_id = %request_id, "tracing disabled for this key");
        None
    };
    if let Some(ref mut t) = tracer {
        t.record(
            "auth_resolve",
            "proxy_request",
            auth_start,
            auth_duration,
            "ok",
            serde_json::json!({
                "tenant": resolved.tenant_name,
                "tenant_id": resolved.tenant_id.to_string(),
                "auth": auth_kind,
            }),
        );
    }

    // ---- read + parse body ----
    let mut body_bytes = match axum::body::to_bytes(body, BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => return error_json(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
    };
    let mut json: serde_json::Value =
        serde_json::from_slice(&body_bytes).unwrap_or(serde_json::Value::Null);
    // File-upload endpoints (audio transcription/translation, image edits and
    // variations) send the model as a `multipart/form-data` field alongside
    // the uploaded file, not as JSON. Parse the fields once so we can resolve
    // the model and later rebuild the upstream form with the model name
    // swapped. The text fields stand in for the JSON body from here on, so
    // the input guardrails scan the prompt and image cost reads `n`.
    let content_type_in = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let mut multipart_fields =
        if is_multipart_endpoint(&path) && content_type_in.starts_with("multipart/form-data") {
            match multer::parse_boundary(&content_type_in) {
                Ok(boundary) => match parse_multipart(&body_bytes, &boundary).await {
                    Ok(fields) => Some(fields),
                    Err(_) => return error_json(StatusCode::BAD_REQUEST, "invalid multipart body"),
                },
                Err(_) => return error_json(StatusCode::BAD_REQUEST, "invalid multipart boundary"),
            }
        } else {
            None
        };
    if let Some(fields) = &multipart_fields {
        json = multipart_text_view(fields);
    }

    let mut model = if let Some(fields) = &multipart_fields {
        fields
            .iter()
            .find(|f| f.name == "model")
            .and_then(|f| std::str::from_utf8(&f.data).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    } else {
        json.get("model")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string()
    };

    // ---- model listing (OpenAI `GET /v1/models`) ----
    // Answer the plain list call from the gateway's own registry so every
    // registered model — including Slurm-hosted ones on their own endpoints — is
    // listed, not just whatever a single upstream reports.
    //
    // `/v1/models/` is the same collection: the trailing slash carries no id.
    // Without it here the request fell into the detail branch below with an
    // empty id, missed the registry, and was forwarded upstream, so a client
    // that added a slash got one backend's raw catalog instead of this
    // gateway's listing.
    //
    // The listing is returned whatever the body says. A GET to a collection
    // carries no body in the OpenAI API, but clients send one anyway — a REST
    // client reuses the tab it made a chat call in, and the stale `{"model":
    // …}` rides along. Treating that as a per-model probe made a plain "list
    // your models" request depend on a field it should not have: a name this
    // gateway knew returned one entry instead of the list, and a name it did
    // not know was forwarded to an upstream and came back as that service's
    // 404, so the listing appeared broken and the fix looked like adding a
    // trailing slash. Detail lookups have their own path, `/v1/models/{id}`,
    // which still falls through for an unknown id so a wildcard passthrough
    // keeps working.
    if method == Method::GET && is_models_collection(&path) {
        return models_list_response(&state, &resolved);
    }
    // ---- model detail (`GET /v1/models/{id}`) ----
    // Answered from the registry for any name the gateway has a route for,
    // because the listing above advertises the gateway's clean `model_name`:
    // forwarding that name to a backend that only knows itself by its
    // quantized one would 404 on an id the gateway had just published. An id
    // no route claims still falls through to the upstream, so a wildcard
    // passthrough keeps working.
    if method == Method::GET && !is_models_collection(&path) && path.starts_with("/v1/models/") {
        let id = path.trim_start_matches("/v1/models/");
        match registered_model_entry(&state, id, allowed_models_for(&resolved)) {
            Some(Ok(entry)) => return (StatusCode::OK, axum::Json(entry)).into_response(),
            // A registered model outside the tenant's allowlist answers like a
            // name the gateway does not have. Falling through would forward the
            // id to an upstream, which knows nothing of the allowlist.
            Some(Err(())) => {
                return error_json(StatusCode::NOT_FOUND, &format!("model '{id}' not found"));
            }
            None => {}
        }
    }
    // ---- model detail (`GET /model/info`) ----
    // Answered from the registry, not from the upstreams: this endpoint reports
    // what the gateway was configured with, which is the only place facts like
    // `quantization` and the routing tags exist at all.
    if method == Method::GET && is_model_info_endpoint(&path) {
        return model_info_response(&state, &resolved);
    }

    // Request-log metadata, captured once so every `finalize` path (cache hit,
    // rejection, upstream error, streamed success) records the same session and
    // request class.
    let conversation = resolve_conversation(
        &headers,
        &json,
        resolved.tenant_id,
        state.session_id_derivation,
    );
    let req_meta = RequestMeta {
        session_id: conversation.value,
        session_id_source: conversation.source.as_str(),
        request_type: surfaced_request_type(&resolved, &path, &headers),
        device_id,
    };
    // Surface the conversation id on the OTLP/Jaeger root span for cross-request
    // grouping (the field is declared Empty on the #[instrument] below).
    tracing::Span::current().record("session.id", req_meta.session_id.as_str());
    tracing::Span::current().record("session.id.source", req_meta.session_id_source);
    if let Some(t) = tracer.as_mut() {
        t.set_conversation(&req_meta.session_id, req_meta.session_id_source);
    }

    // Cost estimate, computed once per request body. Re-estimated below only
    // when a boon actually rewrites the body; the later upstream model-name
    // swap does not affect it (the tokenizer ignores the `model` field).
    let mut est = state.tokenizer.estimate_request(&json);

    // ---- auto model selection ----
    // `model: "auto"` is resolved to a concrete registered model from request
    // shape (estimated context size, required capabilities) and live load. From
    // here on, everything downstream — admission, budgets, caching, telemetry,
    // upstream dispatch — sees the concrete model as if the client named it.
    let route = if model == crate::router::AUTO_MODEL_NAME {
        // Span bookkeeping for the routing decision. Only the traced branch
        // below reads it, so an untraced request does not pay for the clock
        // read — nor does any non-auto request, which never enters this block.
        let traced = tracer.is_some();
        let auto_start = if traced { crate::tracer::now_ms() } else { 0 };
        let max_tokens = json.get("max_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let features = crate::router::RequestFeatures::from_request(
            &json,
            est.input_tokens as u64,
            max_tokens,
        );
        let candidates = state.model_registry.load();
        // Cheap shared-map read; the full scheduler snapshot is dashboard-only.
        let busyness = state.fairshare.model_load();
        // Observed completion lengths, for the per-request cost estimate.
        let expected_output = state.output_stats.snapshot();
        let allowed = if resolved.internal {
            None
        } else {
            resolved.allowed_models.as_deref()
        };

        // Single snapshot load for this request: reused for both tag
        // derivation and scoring so `auto` costs exactly one settings read.
        let router_settings = state.classifier.settings();

        // Derive intent (tags + difficulty): explicit header first, then the
        // classifier (when enabled + resolvable), then cheap heuristics, then a
        // neutral default.
        let available_tags = union_candidate_tags(&candidates, allowed);
        let effort_header = headers.get("x-obleth-effort").and_then(|v| v.to_str().ok());
        let classifier_start = if traced { crate::tracer::now_ms() } else { 0 };
        let intent = derive_intent(
            &state,
            &json,
            est.input_tokens as u64,
            &available_tags,
            &router_settings,
            effort_header,
            // The classifier model reads the prompt, and on this path intent is
            // derived before the tenant input guardrails run: under an input
            // policy, route on the header and heuristics instead.
            has_input_guardrails(&resolved),
        )
        .await;
        // Wall-clock around the whole of intent derivation (header parse,
        // classifier round-trip when active, heuristic fallback otherwise), not
        // an isolated classifier timer — `derive_intent` is where that round-trip
        // happens, and it is nearly all of this span's time when the classifier
        // is active and near-zero otherwise. Not claiming more precision than that.
        // Measured only when something will read it; untraced, it was discarded.
        let classifier_ms = if traced {
            (crate::tracer::now_ms() - classifier_start) as u32
        } else {
            0
        };

        // Boon-granted capabilities count as native in the hard filters: a
        // model carrying the structured_output boon can serve requests that
        // need it, because the boon engine emulates the capability.
        let grants = crate::router::BoonGrants::from_settings(&state.boons.settings());
        let weights = crate::router::RouterWeights::from_settings(&router_settings);
        // Exactly one draw per request, whichever branch runs below. The router
        // only reads it when it samples, so at the default temperature of 0 —
        // where the pick is the exact argmax — the clock read is skipped
        // entirely. `samples()` is the router's own predicate, not a copy of its
        // threshold.
        let uniform = if weights.samples() {
            crate::router::splitmix_uniform()
        } else {
            0.0
        };
        // Traced requests get the full explanation from a single `route()` call
        // (one `evaluate`, one `uniform` draw) so the span describes exactly the
        // sample that was served. Untraced requests call `select_model` instead,
        // which asks for `Narration::Off` and builds no rejection/score data —
        // that is the whole cost story for this feature: zero by default.
        let picked = if let Some(ref mut t) = tracer {
            let (picked, mut explain) = crate::router::route(
                &candidates,
                &features,
                &busyness,
                &expected_output,
                allowed,
                &intent.tags,
                grants,
                &weights,
                uniform,
                &intent,
            );
            explain.classifier_ms = classifier_ms;
            t.record(
                "auto_route",
                "proxy_request",
                auto_start,
                (crate::tracer::now_ms() - auto_start) as u32,
                "ok",
                serde_json::to_value(&explain).unwrap_or_else(|_| {
                    serde_json::json!({
                        "chosen": explain.chosen,
                        "error": "explanation failed to serialize",
                    })
                }),
            );
            picked
        } else {
            crate::router::select_model(
                &candidates,
                &features,
                &busyness,
                &expected_output,
                allowed,
                &intent.tags,
                grants,
                &weights,
                uniform,
                intent.difficulty,
            )
        };
        match picked {
            Some(chosen) => {
                tracing::debug!(chosen = %chosen.model_name, "auto-routed request");
                model = chosen.model_name.clone();
                Some(Arc::new(chosen))
            }
            None => {
                return error_json(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no model is available to satisfy the auto request",
                );
            }
        }
    } else {
        let route = resolve_model(&state, &model).await;
        // An alias resolves to the same route as the canonical name, so adopt
        // the canonical name for everything downstream: admission, budgets, the
        // response cache, the per-tenant allowlist, and the usage ledger. This
        // is the same move `auto` makes after it picks — without it, one
        // model's traffic would split across every spelling clients happen to
        // have pinned, and a tenant allowed `glm-5-3` would be refused for
        // asking the same model by its old name.
        if let Some(r) = route.as_ref() {
            if r.model_name != model {
                tracing::debug!(alias = %model, model = %r.model_name, "resolved model alias");
                model = r.model_name.clone();
            }
        }
        route
    };

    if requires_registered_model(&path) {
        if model == "unknown" {
            return error_json(StatusCode::BAD_REQUEST, "model is required");
        }
        let Some(route) = route.as_ref() else {
            return error_json(
                StatusCode::NOT_FOUND,
                &format!("model '{model}' is not registered"),
            );
        };
        if !route.enabled {
            return error_json(StatusCode::FORBIDDEN, "model is disabled");
        }
    }
    // ---- reject unmapped passthrough (noise / info-leak guard) ----
    // A request that resolved to no registered model (`route` is None) and whose
    // path is not a recognized OpenAI endpoint would otherwise be forwarded
    // opaquely to the default upstream (`state.upstream_base`). That turns stray
    // probes and scans (`/props`, `/health`, favicons, vulnerability scanners)
    // into real upstream round-trips and noisy `unknown`/`other` request-log rows
    // that also leak the internal upstream URL. Reject them here — before any
    // dispatch or telemetry — so they never reach the ledger. The OpenAI model
    // discovery endpoints are the only model-less passthroughs the gateway
    // serves, so they stay allowed. A request that DID name a registered model on
    // an unusual path (`route` is Some) is still forwarded untouched.
    if route.is_none() && request_type_for_path(&path) == "other" && !is_models_endpoint(&path) {
        return error_json(StatusCode::NOT_FOUND, "unknown endpoint");
    }
    // ---- per-tenant model allowlist (Phase 4) ----
    if !resolved.internal {
        if let Some(allowed) = &resolved.allowed_models {
            if !allowed.iter().any(|m| m == &model) {
                return error_json(StatusCode::FORBIDDEN, "model not permitted for tenant");
            }
        }
    }

    // ---- model boons (gateway-granted capabilities) ----
    // e.g. the vision boon rewrites image content into text descriptions for
    // models that lack native vision. Runs before estimation/caching/dispatch so
    // every downstream stage sees the rewritten body. Fail-open: on any error
    // the body is left unchanged. `x-obleth-boons: off` skips boons for one
    // request; the structured-output boon and the gateway tool loop may arm a
    // response plan that intercepts and rewrites the completion below (streaming
    // clients of the tool loop are driven live; see `stream_tap`).
    // `x-obleth-boons` is a comma-separated control list: `off` disables all
    // boons for the request; `lossy` forces the compression boon's lossy pass on
    // (for back-to-back A/B testing). `off` wins if both are present.
    let (boons_opt_out, boons_force_lossy) = headers
        .get(crate::boons::BOONS_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            let mut opt_out = false;
            let mut force_lossy = false;
            for tok in v.split(',') {
                match tok.trim().to_ascii_lowercase().as_str() {
                    "off" => opt_out = true,
                    "lossy" => force_lossy = true,
                    _ => {}
                }
            }
            (opt_out, force_lossy)
        })
        .unwrap_or((false, false));
    // Refused before the input scan so a request that cannot be served never
    // spends a guard-model call first.
    if method == Method::POST && output_guardrails_unenforceable(&resolved, &path) {
        if let Some(t) = tracer.take() {
            t.finish("error");
        }
        return error_json(
            StatusCode::BAD_REQUEST,
            "this tenant's output guardrails read chat responses only; \
             use /v1/chat/completions, /v1/responses, or /v1/messages",
        );
    }
    if method == Method::POST && input_guardrails_unscannable(&resolved, &path) {
        if let Some(t) = tracer.take() {
            t.finish("error");
        }
        return error_json(
            StatusCode::BAD_REQUEST,
            "this tenant's input guardrails cannot scan requests to this path; \n             use /v1/chat/completions, /v1/responses, or /v1/messages",
        );
    }
    let boon_outcome = state
        .boons
        .enrich_request(
            &state,
            route.as_deref(),
            &resolved,
            &req_meta.session_id,
            boons_opt_out,
            boons_force_lossy,
            is_chat_path(&path),
            &mut json,
            tracer.as_mut(),
        )
        .await;
    if let Some(block) = boon_outcome.blocked {
        if let Some(t) = tracer.take() {
            t.finish("error");
        }
        return error_json(block.status, block.reason);
    }
    if boon_outcome.rewritten {
        if let Some(fields) = multipart_fields.as_mut() {
            // The form is rebuilt from its fields at dispatch, not from the body.
            apply_multipart_text_view(fields, &json);
        } else {
            match serde_json::to_vec(&json) {
                Ok(bytes) => body_bytes = Bytes::from(bytes),
                Err(e) => tracing::warn!(error = %e, "failed to re-serialize boon-rewritten body"),
            }
        }
        // The body changed (e.g. images became text descriptions); the
        // admission estimate must reflect what is actually sent upstream.
        est = state.tokenizer.estimate_request(&json);
    }
    let boons_applied = boon_outcome.applied;
    // Compact compression summary for the `x-obleth-compression` response header,
    // so a back-to-back A/B client can diff savings without reading traces.
    let compression_header = boon_outcome.compression_tokens.map(|(before, after)| {
        format!(
            "before={};after={};saved={}",
            before,
            after,
            before.saturating_sub(after)
        )
    });
    let response_plan = boon_outcome.response_plan;
    let speculation_plan = boon_outcome.speculation;

    // Live streaming tool loop: when the *only* response transform is the
    // gateway tool loop and the client asked to stream, keep the upstream call
    // streaming and drive it through `tool_stream`. Content/reasoning deltas
    // stream straight through, a visible marker shows when a gateway tool runs,
    // and only the tool execution between turns pauses the stream. The
    // structured-output transform still needs a fully buffered completion, so it
    // keeps forcing the upstream non-streaming.
    let stream_tap = response_plan.as_ref().is_some_and(|p| {
        p.tool_loop.is_some()
            && p.structured.is_none()
            && p.client_stream
            && p.guardrails
                .as_ref()
                .map(|g| matches!(g.policy.action, obleth_config::GuardrailsAction::LogOnly))
                .unwrap_or(true)
    }) && route.is_some();

    let effective_weight = effective_admission_weight(resolved.weight, route.as_deref());

    // Cost rates and per-request modality surcharge captured up front so every
    // `finalize` path (cache hit, rejection, upstream error, streamed success)
    // can freeze an identical USD cost, and so the post-stream term-usage commit
    // can reuse them after `route` is moved out of the request scope.
    let (in_cost_rate, out_cost_rate) = route
        .as_ref()
        .map(|r| (r.input_cost_per_token, r.output_cost_per_token))
        .unwrap_or((0.0, 0.0));
    let modality_cost = compute_modality_cost(route.as_deref(), &json);
    let energy_slots = route.as_ref().map(|r| r.energy_slots_per_node).unwrap_or(0);

    // ---- response cache (exact-match, before admission so hits cost nothing) ----
    // Tool-loop answers depend on live tool results (e.g. a web search), so a
    // cached answer would be wrong by definition: skip the cache entirely.
    let tool_loop_armed = response_plan
        .as_ref()
        .is_some_and(|p| p.tool_loop.is_some());
    // The response cache is keyed on (tenant, model, body), so an entry is only
    // ever replayed to the tenant that populated it. That alone does not make
    // it safe under output guardrails: an entry stored before the tenant's
    // block/redact policy was armed (or while it scanned nothing) would replay
    // an un-scanned response and bypass the scan. Disable the cache for the
    // request whenever output guardrails are armed.
    let output_guardrails_armed = response_plan
        .as_ref()
        .is_some_and(|p| p.guardrails.is_some());
    // A video create is never replayed from cache either: an identical body
    // must start a new job, not hand back an id some earlier call recorded.
    let cache_enabled = route.as_ref().map(|r| r.cache_enabled).unwrap_or(false)
        && !tool_loop_armed
        && !output_guardrails_armed
        && !video_create;
    let cache_ttl = route.as_ref().map(|r| r.cache_ttl_secs).unwrap_or(0);
    // TTL <= 0 means "don't cache": nothing is ever written, so a lookup could
    // never hit and would only cost a Redis round-trip.
    let cache_key = (cache_enabled && cache_ttl > 0)
        .then(|| obleth_config::cache_key(&resolved.tenant_id.to_string(), &model, &body_bytes));
    if let Some(ck) = &cache_key {
        let cache_start = crate::tracer::now_ms();
        let cache_result = state
            .redis
            .cache_get(ck)
            .instrument(tracing::info_span!("cache_lookup"))
            .await;
        let cache_ms = (crate::tracer::now_ms() - cache_start) as u32;
        match cache_result {
            Ok(Some(cached)) => {
                if let Some(mut t) = tracer.take() {
                    t.record(
                        "cache_lookup",
                        "proxy_request",
                        cache_start,
                        cache_ms,
                        "hit",
                        serde_json::json!({
                            "result": "hit",
                            "tokens_saved": cached.input_tokens.saturating_add(cached.output_tokens),
                        }),
                    );
                    t.finish("ok");
                }
                state.metrics.record_cache(
                    true,
                    cached.input_tokens.saturating_add(cached.output_tokens),
                );
                finalize(
                    &state,
                    request_id,
                    &resolved,
                    &req_meta,
                    &model,
                    Admission::Fast,
                    est,
                    cached.input_tokens,
                    cached.output_tokens,
                    0,
                    0,
                    0,
                    cached.status,
                    "hit",
                    (cached.input_tokens as f64) * in_cost_rate
                        + (cached.output_tokens as f64) * out_cost_rate
                        + modality_cost,
                    crate::energy::EnergyFigures::default(),
                );
                return cached_response(cached, request_id);
            }
            Ok(None) => {
                if let Some(ref mut t) = tracer {
                    t.record(
                        "cache_lookup",
                        "proxy_request",
                        cache_start,
                        cache_ms,
                        "miss",
                        serde_json::json!({ "result": "miss" }),
                    );
                }
                state.metrics.record_cache(false, 0);
            }
            Err(e) => {
                if let Some(ref mut t) = tracer {
                    t.record(
                        "cache_lookup",
                        "proxy_request",
                        cache_start,
                        cache_ms,
                        "error",
                        serde_json::json!({}),
                    );
                }
                tracing::warn!(error = %e, "cache lookup failed; treating as miss");
                state.alerts.issue(
                    "redis_cache_lookup_failed",
                    "Redis response-cache lookup failed",
                    format!("model `{model}` tenant `{}`: {e}", resolved.tenant_name),
                );
            }
        }
    }
    // Telemetry label for everything that isn't a cache hit.
    let cache_status_label = if cache_enabled { "miss" } else { "off" };

    // ---- fairshare admission (per-model pool; group → tenant → key) ----
    // Admission deliberately precedes the budget reservation below: a request
    // parked in the queue holds no budget, so a waiter that times out (or
    // whose client leaves) has nothing to refund, and budgets still fail shut
    // because nothing is dispatched until the reservation has succeeded.
    // Requests with no registered route share one pool, so arbitrary model
    // strings cannot each mint scheduler state.
    let admission_start = crate::tracer::now_ms();
    let pool = if route.is_some() {
        obleth_fairshare::PoolKey::Model(model.clone())
    } else {
        obleth_fairshare::PoolKey::Unrouted
    };
    let admit_wait = admission_timeout();
    let admit = state.fairshare.admit_to(
        pool,
        admit_request_for(
            &resolved,
            &model,
            route.as_deref(),
            effective_weight,
            est.total(),
        ),
    );
    let admitted = match timeout(admit_wait, admit).await {
        Ok(Some(a)) => a,
        Err(_) => {
            if let Some(t) = tracer.take() {
                t.finish("error");
            }
            let queued_ms = (crate::tracer::now_ms() - admission_start) as u32;
            finalize(
                &state,
                request_id,
                &resolved,
                &req_meta,
                &model,
                Admission::Rejected,
                est,
                0,
                0,
                queued_ms,
                0,
                request_start.elapsed().as_millis() as u32,
                503,
                cache_status_label,
                0.0,
                crate::energy::EnergyFigures::default(),
            );
            let mut resp = error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "timed out waiting for model capacity",
            );
            resp.headers_mut().insert(
                header::RETRY_AFTER,
                header::HeaderValue::from_static(ADMISSION_RETRY_AFTER_SECS),
            );
            return resp;
        }
        Ok(None) => {
            if let Some(t) = tracer.take() {
                t.finish("error");
            }
            state.alerts.issue(
                "scheduler_unavailable",
                "Fairshare scheduler unavailable",
                format!(
                    "tenant `{}` model `{model}` path `{path}`",
                    resolved.tenant_name
                ),
            );
            return error_json(StatusCode::SERVICE_UNAVAILABLE, "scheduler unavailable");
        }
    };
    let admission = admitted.admission;
    let permit = admitted.permit;
    let queue_wait_ms = admitted.waited.as_millis() as u32;
    let admission_ms = (crate::tracer::now_ms() - admission_start) as u32;
    if let Some(ref mut t) = tracer {
        t.record(
            "admission",
            "proxy_request",
            admission_start,
            admission_ms,
            "ok",
            serde_json::json!({
                "decision": admission.as_str(),
                "queue_wait_ms": queue_wait_ms,
            }),
        );
    }

    let send_bytes = body_bytes;

    // ---- token budget reserve + cumulative term gate (atomic, cross-pod) ----
    // One Redis round trip per scope covers both checks. The term gate
    // (Phase 3: caps on lifetime/monthly/term usage) runs first inside the
    // script, so a term-exhausted request never reserves per-minute tokens it
    // has no completion path to refund. An admitted request also reserves its
    // estimated tokens and cost against the term cap, so concurrent requests
    // cannot all pass the gate on the same headroom; each such reservation is
    // settled exactly once, through `term_reconcile` at settlement or
    // `release_term_hold` on a rejection between the two scopes.
    let capacity = resolved.tokens_per_minute.max(0);
    let now = chrono::Utc::now();
    let est_cost = estimated_cost(est, in_cost_rate, out_cost_rate, modality_cost);
    let term_period = term_period_key(&resolved, now);
    let term_gate = term_period
        .as_deref()
        .map(|period_key| obleth_redis::TermGate {
            period_key,
            budget_tokens: resolved.budget_tokens,
            budget_cost_usd: resolved.budget_cost_usd,
        });
    let key_term_period = key_term_period_key(&resolved, now);
    let key_term_gate = key_term_period
        .as_deref()
        .map(|period_key| obleth_redis::TermGate {
            period_key,
            budget_tokens: resolved.key_budget_tokens,
            budget_cost_usd: resolved.key_budget_cost_usd,
        });
    // Whether each scope holds a term reservation that settlement must
    // reconcile. False on the fail-open path: nothing was reserved, so the
    // settle commits usage with a plain add instead.
    let mut key_term_held = false;
    // Releases the key reservation if the request ends (client drop included)
    // before settlement takes ownership of it.
    let mut key_hold: Option<PendingTermHold> = None;
    let mut tenant_term_held = false;
    if let Some(gate) = key_term_gate {
        match state
            .redis
            .reserve_with_term(&resolved.key_id, 0, 0, est.total(), Some(gate), est_cost)
            .instrument(tracing::info_span!("reserve_key_budget"))
            .await
        {
            Ok(obleth_redis::ReserveOutcome::Reserved { .. }) => {
                key_term_held = true;
                key_hold = key_term_period.clone().map(|period| {
                    PendingTermHold::new(&state, resolved.key_id, period, est, est_cost)
                });
            }
            // Capacity 0 has no per-minute bucket; nothing was reserved.
            Ok(obleth_redis::ReserveOutcome::RateLimited { .. }) => {}
            Ok(obleth_redis::ReserveOutcome::TermExhausted {
                used_tokens,
                used_cost,
            }) => {
                drop(permit);
                state.alerts.issue(
                    format!("key_term_budget_exhausted:{}", resolved.key_id),
                    "API key term budget exhausted",
                    format!(
                        "tenant `{}` key `{}` blocked: used {used_tokens} tokens / ${used_cost:.4} against caps tokens={:?} cost={:?}",
                        resolved.tenant_name,
                        resolved.key_id,
                        resolved.key_budget_tokens,
                        resolved.key_budget_cost_usd,
                    ),
                );
                finalize(
                    &state,
                    request_id,
                    &resolved,
                    &req_meta,
                    &model,
                    Admission::Rejected,
                    est,
                    0,
                    0,
                    queue_wait_ms,
                    0,
                    0,
                    403,
                    cache_status_label,
                    0.0,
                    crate::energy::EnergyFigures::default(),
                );
                if let Some(t) = tracer.take() {
                    t.finish("error");
                }
                return error_json(StatusCode::FORBIDDEN, "api key term budget exhausted");
            }
            Err(e) => {
                if !state.fail_open {
                    drop(permit);
                    state.alerts.issue(
                        "redis_key_budget_reserve_failed_closed",
                        "Redis key-budget reserve failed",
                        format!(
                            "fail-open is disabled; rejecting tenant `{}` key `{}` model `{model}`: {e}",
                            resolved.tenant_name,
                            resolved.key_id
                        ),
                    );
                    if let Some(t) = tracer.take() {
                        t.finish("error");
                    }
                    return error_json(StatusCode::SERVICE_UNAVAILABLE, "key budget check failed");
                }
                tracing::warn!(error = %e, "key budget reserve failed; failing open");
                state.alerts.issue(
                    "redis_key_budget_reserve_failed_open",
                    "Redis key-budget reserve failed",
                    format!(
                        "fail-open is enabled; admitting tenant `{}` key `{}` model `{model}` without key budget enforcement: {e}",
                        resolved.tenant_name,
                        resolved.key_id
                    ),
                );
            }
        }
    }
    let tenant_gate_armed = term_gate.is_some();
    let should_check_budget = capacity > 0 || tenant_gate_armed;
    if should_check_budget {
        match state
            .redis
            .reserve_with_term(
                &resolved.tenant_id,
                capacity,
                resolved.tokens_per_minute,
                est.total(),
                term_gate,
                est_cost,
            )
            .instrument(tracing::info_span!("reserve_budget"))
            .await
        {
            Ok(obleth_redis::ReserveOutcome::Reserved { .. }) => {
                tenant_term_held = tenant_gate_armed;
            }
            Ok(obleth_redis::ReserveOutcome::RateLimited { .. }) => {
                drop(permit);
                if let Some(hold) = key_hold.take() {
                    hold.release().await;
                }
                finalize(
                    &state,
                    request_id,
                    &resolved,
                    &req_meta,
                    &model,
                    Admission::Rejected,
                    est,
                    0,
                    0,
                    queue_wait_ms,
                    0,
                    0,
                    429,
                    cache_status_label,
                    0.0,
                    crate::energy::EnergyFigures::default(),
                );
                if let Some(t) = tracer.take() {
                    t.finish("error");
                }
                return error_json(StatusCode::TOO_MANY_REQUESTS, "token budget exceeded");
            }
            Ok(obleth_redis::ReserveOutcome::TermExhausted {
                used_tokens,
                used_cost,
            }) => {
                drop(permit);
                if let Some(hold) = key_hold.take() {
                    hold.release().await;
                }
                state.alerts.issue(
                    format!("term_budget_exhausted:{}", resolved.tenant_id),
                    "Tenant term budget exhausted",
                    format!(
                        "tenant `{}` blocked: used {used_tokens} tokens / ${used_cost:.4} against caps tokens={:?} cost={:?}",
                        resolved.tenant_name,
                        resolved.budget_tokens,
                        resolved.budget_cost_usd,
                    ),
                );
                finalize(
                    &state,
                    request_id,
                    &resolved,
                    &req_meta,
                    &model,
                    Admission::Rejected,
                    est,
                    0,
                    0,
                    queue_wait_ms,
                    0,
                    0,
                    403,
                    cache_status_label,
                    0.0,
                    crate::energy::EnergyFigures::default(),
                );
                if let Some(t) = tracer.take() {
                    t.finish("error");
                }
                return error_json(StatusCode::FORBIDDEN, "tenant term budget exhausted");
            }
            Err(e) => {
                if !state.fail_open {
                    drop(permit);
                    if let Some(hold) = key_hold.take() {
                        hold.release().await;
                    }
                    state.alerts.issue(
                        "redis_budget_reserve_failed_closed",
                        "Redis budget reserve failed",
                        format!(
                            "fail-open is disabled; rejecting tenant `{}` model `{model}`: {e}",
                            resolved.tenant_name
                        ),
                    );
                    if let Some(t) = tracer.take() {
                        t.finish("error");
                    }
                    return error_json(StatusCode::SERVICE_UNAVAILABLE, "budget check failed");
                }
                tracing::warn!(error = %e, "budget reserve failed; failing open");
                state.alerts.issue(
                    "redis_budget_reserve_failed_open",
                    "Redis budget reserve failed",
                    format!(
                        "fail-open is enabled; admitting tenant `{}` model `{model}` without budget enforcement: {e}",
                        resolved.tenant_name
                    ),
                );
            }
        }
    }

    if let Some(hold) = key_hold.take() {
        hold.hand_off();
    }

    // From here on every exit settles through `settle_guard`, so the
    // per-minute bucket and any term reservation are reconciled exactly once
    // on every path — including a client that disconnects before upstream
    // headers arrive, which settles as nothing generated (zero tokens, 499).
    // Once an upstream answer starts, `arm_estimate` switches that fallback
    // to the admission estimate.
    let accounting = StreamAccounting {
        state: state.clone(),
        request_id,
        resolved: resolved.clone(),
        meta: req_meta.clone(),
        model: model.clone(),
        admission,
        est,
        queue_wait_ms,
        request_start,
        cache_status: cache_status_label.to_string(),
        capacity,
        term_period: term_period.clone(),
        key_term_period: key_term_period.clone(),
        in_cost_rate,
        out_cost_rate,
        modality_cost,
        energy_slots,
        holds: TermHolds {
            tenant: tenant_term_held,
            key: key_term_held,
            est_cost,
        },
    };
    let mut settle_guard = accounting.unbilled_guard();

    // ---- proxy upstream ----
    // Resolve the per-request timeout and retry policy. Both default to the
    // model-level config, falling back to the global gateway settings.
    let mut req_timeout = route
        .as_ref()
        .and_then(|r| r.request_timeout_secs)
        .filter(|s| *s >= 1)
        .map(|s| Duration::from_secs(s as u64))
        .unwrap_or(state.upstream_timeout);
    // With a response-transforming boon the upstream call is non-streaming, so
    // the send timeout covers the entire generation, not just the headers.
    if response_plan.is_some() {
        req_timeout = req_timeout.max(BOON_MIN_TIMEOUT);
    }
    let max_retries = route.as_ref().map(|r| r.max_retries.max(0)).unwrap_or(0);
    let backoff = Duration::from_millis(
        route
            .as_ref()
            .map(|r| r.retry_backoff_ms.max(0) as u64)
            .unwrap_or(0),
    );
    let selection_mode = route
        .as_ref()
        .map(|r| r.endpoint_selection_mode.as_str())
        .unwrap_or(obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE);

    // ---- speculation boon (draft-verify cascade, pre-dispatch) ----
    // Runs after admission and budget reserve — the request is fully admitted
    // either way — but before the target dispatch. A committed cascade returns
    // the response here (verified draft, or draft + mid-stream continuation);
    // an abstain falls through so the normal dispatch below runs untouched.
    if let (Some(spec_plan), Some(spec_route)) = (speculation_plan, route.clone()) {
        if multipart_fields.is_none() {
            let spec_stats = std::sync::Arc::new(std::sync::Mutex::new(
                crate::boons::speculation::SpecStats::default(),
            ));
            let spec_req = crate::boons::speculation::SpecRequest {
                state: &state,
                route: spec_route,
                key: &resolved,
                session_id: &req_meta.session_id,
                dispatch_timeout: req_timeout,
                started: request_start,
            };
            match crate::boons::speculation::run(spec_req, spec_plan, spec_stats.clone()).await {
                crate::boons::speculation::Outcome::Abstain(_) => {}
                crate::boons::speculation::Outcome::ShippedJson {
                    body,
                    input_tokens,
                    output_tokens,
                } => {
                    drop(permit);
                    let total_ms = request_start.elapsed().as_millis() as u32;
                    let _ = settle_guard
                        .complete(accounting.settle(
                            (input_tokens, output_tokens),
                            total_ms,
                            total_ms,
                            200,
                            None,
                        ))
                        .await;
                    if let Some(t) = tracer.take() {
                        t.finish("ok");
                    }
                    let mut builder = Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "application/json")
                        .header("x-obleth-request-id", request_id.to_string())
                        .header(NO_BUFFER_HEADER.0, NO_BUFFER_HEADER.1);
                    if !boons_applied.is_empty() {
                        builder =
                            builder.header(crate::boons::BOONS_HEADER, boons_applied.join(","));
                    }
                    return builder
                        .body(Body::from(body.to_string()))
                        .unwrap_or_else(|_| {
                            error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
                        });
                }
                crate::boons::speculation::Outcome::Stream(driver) => {
                    accounting.arm_estimate(&mut settle_guard);
                    let body_stream = async_stream::stream! {
                        futures_util::pin_mut!(driver);
                        while let Some(item) = driver.next().await {
                            yield item;
                        }
                        drop(permit);
                        let (ttft_ms, input_tokens, output_tokens) = {
                            let s = spec_stats.lock().unwrap_or_else(|e| e.into_inner());
                            let toks = if s.final_set {
                                (s.input_tokens, s.output_tokens)
                            } else {
                                (est.input_tokens, est.estimated_output_tokens)
                            };
                            (s.ttft_ms, toks.0, toks.1)
                        };
                        let total_ms = request_start.elapsed().as_millis() as u32;
                        let _ = settle_guard.complete(accounting.settle(
                            (input_tokens, output_tokens), ttft_ms, total_ms, 200, None,
                        )).await;
                    };
                    if let Some(t) = tracer.take() {
                        t.finish("ok");
                    }
                    let mut builder = Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "text/event-stream")
                        .header("x-obleth-request-id", request_id.to_string())
                        .header(NO_BUFFER_HEADER.0, NO_BUFFER_HEADER.1);
                    if !boons_applied.is_empty() {
                        builder =
                            builder.header(crate::boons::BOONS_HEADER, boons_applied.join(","));
                    }
                    return builder
                        .body(Body::from_stream(body_stream))
                        .unwrap_or_else(|_| {
                            error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
                        });
                }
            }
        }
    }

    // Build the ordered list of upstream targets. When a model defines explicit
    // endpoints we route across the healthy/enabled ones (priority order for
    // failover, weighted order for load_balance); otherwise we fall back to the
    // legacy single api_base/api_key on the model (or the global default).
    let targets = build_targets(
        route.as_deref(),
        &state.upstream_base,
        selection_mode,
        &req_meta.session_id,
    );

    // The JSON body is rebuilt once and replayed on each attempt/endpoint.
    // Multipart bodies cannot be replayed, so they get a single attempt against
    // the first target only.
    let replayable = multipart_fields.is_none();
    // Streaming chat/completions always ask the upstream for a final usage
    // chunk, so billing never depends on whether the client happened to set
    // `stream_options.include_usage`. A client that did not ask for it never
    // sees it: the pass-through below strips it again.
    let force_non_streaming = response_plan.is_some() && !stream_tap;
    let client_include_usage = json
        .pointer("/stream_options/include_usage")
        .and_then(serde_json::Value::as_bool)
        == Some(true);
    let inject_usage = replayable
        && !force_non_streaming
        && route.is_some()
        && json.is_object()
        && json.get("stream").and_then(serde_json::Value::as_bool) == Some(true)
        && is_stream_usage_path(&path);
    let strip_usage_chunk = inject_usage && !stream_tap && !client_include_usage;
    let prepared_body: Option<Bytes> = replayable.then(|| {
        prepare_upstream_body(
            route.as_deref(),
            &mut json,
            send_bytes,
            force_non_streaming,
            stream_tap || inject_usage,
        )
    });

    // TTFT is measured from the moment we dispatch the *successful* upstream
    // request, *after* fairshare admission. Time spent waiting in the queue is
    // reported separately as `queue_wait_ms`; folding it into TTFT would
    // double-count the wait and make a fast model look slow under contention.
    let dispatch_start = crate::tracer::now_ms();
    let mut upstream_start = Instant::now();
    let mut upstream_resp: Option<reqwest::Response> = None;
    let mut last_url = String::new();
    let mut last_err: Option<String> = None;
    let mut timed_out = false;
    let total_targets = targets.len();
    // Which target answered: a video job lives on the backend that accepted it.
    let mut served_target = 0usize;

    'targets: for (ti, target) in targets.iter().enumerate() {
        let url = build_upstream_url(&target.base, &path, &query);
        last_url = url.clone();
        // Base budget = configured retries + 1. Plus one extra slot reserved for
        // connection-level failures: a reused keep-alive socket the upstream
        // already closed surfaces as a send error with no response, so the request
        // never executed and is safe to retry on a fresh connection even when
        // max_retries == 0. The bonus slot is only ever consumed by a connection
        // error — status/timeout failures break out to failover instead.
        let base_attempts: u32 = if replayable {
            max_retries as u32 + 1
        } else {
            1
        };
        let attempts: u32 = base_attempts + if replayable { 1 } else { 0 };
        for attempt in 0..attempts {
            let configured_left = attempt + 1 < base_attempts;
            let bonus_left = attempt + 1 < attempts;
            let more_targets = replayable && ti + 1 < total_targets;

            let mut fwd_headers = forward_headers(&headers);
            // The model's own upstream headers go on after the client's, so
            // the operator's value wins a name both set.
            fwd_headers.extend(target.headers.clone());
            if let Some(key) = &target.api_key {
                if let Ok(v) = header::HeaderValue::from_str(&format!("Bearer {key}")) {
                    fwd_headers.insert(header::AUTHORIZATION, v);
                }
            }
            let req_builder = if let Some(body) = &prepared_body {
                state
                    .http
                    .request(method.clone(), &url)
                    .headers(fwd_headers)
                    .body(body.clone())
            } else {
                // Rebuild the multipart form, swapping the client model name for
                // the upstream one. reqwest regenerates the Content-Type (with
                // boundary), so the inbound multipart content-type must be dropped.
                fwd_headers.remove(header::CONTENT_TYPE);
                let upstream_model = route
                    .as_ref()
                    .map(|r| r.upstream_model.clone())
                    .unwrap_or_else(|| model.clone());
                // Multipart bodies are not replayable, so this branch only runs on
                // the single attempt against the first target (`replayable` is false
                // ⇒ one attempt, no failover). The `take` therefore yields `Some`
                // exactly once; we still handle `None` gracefully instead of
                // panicking should that invariant ever change.
                let Some(fields) = multipart_fields.take() else {
                    let total_ms = request_start.elapsed().as_millis() as u32;
                    let _ = settle_guard
                        .complete(accounting.settle_unbilled(0, total_ms, 500))
                        .await;
                    return error_json(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "multipart request body was already consumed",
                    );
                };
                let form = build_multipart_form(fields, &upstream_model);
                state
                    .http
                    .request(method.clone(), &url)
                    .headers(fwd_headers)
                    .multipart(form)
            };

            upstream_start = Instant::now();
            let send_fut = req_builder
                .send()
                .instrument(tracing::info_span!("upstream_request"));
            match timeout(req_timeout, send_fut).await {
                Ok(Ok(resp)) => {
                    let status = resp.status().as_u16();
                    if is_retryable_status(status) && (configured_left || more_targets) {
                        last_err = Some(format!("upstream status {status}"));
                        if configured_left {
                            state.metrics.record_upstream_attempt("retry");
                            tokio::time::sleep(backoff_for(backoff, attempt)).await;
                            continue;
                        }
                        state.metrics.record_upstream_attempt("failover");
                        continue 'targets;
                    }
                    state.metrics.record_upstream_attempt("success");
                    upstream_resp = Some(resp);
                    served_target = ti;
                    break 'targets;
                }
                Ok(Err(e)) => {
                    let connection_error = is_connection_error(&e);
                    last_err = Some(e.to_string());
                    if configured_left {
                        state.metrics.record_upstream_attempt("retry");
                        tokio::time::sleep(backoff_for(backoff, attempt)).await;
                        continue;
                    }
                    // Configured retries are spent, but a connection-level send
                    // failure never reached the upstream (e.g. a stale pooled
                    // keep-alive socket): spend the reserved bonus slot on one
                    // fresh-connection retry before failing over.
                    if connection_error && bonus_left {
                        state.metrics.record_upstream_attempt("conn_retry");
                        tokio::time::sleep(CONN_RETRY_BACKOFF).await;
                        continue;
                    }
                    break;
                }
                Err(_) => {
                    timed_out = true;
                    last_err = Some(format!("timed out after {req_timeout:?}"));
                    if configured_left {
                        state.metrics.record_upstream_attempt("timeout");
                        tokio::time::sleep(backoff_for(backoff, attempt)).await;
                        continue;
                    }
                    break;
                }
            }
        }
        // Attempts on this target are exhausted. Fail over to the next target
        // when one is available and the request is replayable.
        if replayable && ti + 1 < total_targets {
            state.metrics.record_upstream_attempt("failover");
            continue 'targets;
        }
        break;
    }

    let url = last_url;
    let dispatch_ms = (crate::tracer::now_ms() - dispatch_start) as u32;
    let upstream = match upstream_resp {
        Some(r) => r,
        None => {
            if let Some(mut t) = tracer.take() {
                t.record(
                    "upstream",
                    "proxy_request",
                    dispatch_start,
                    dispatch_ms,
                    "error",
                    serde_json::json!({
                        "model": model,
                        "url": url.as_str(),
                        "targets": total_targets,
                        "error": last_err.clone().unwrap_or_default(),
                    }),
                );
                // Opt-in upstream diagnostics: only when this model has the flag
                // on (and we're already tracing). Bounded, read-only DNS + TCP
                // probe recorded as its own span before the trace is finished —
                // it never alters the 502/504 returned below.
                if route.as_ref().map(|r| r.debug_diagnostics).unwrap_or(false) {
                    let diag_start = crate::tracer::now_ms();
                    let diag = crate::diagnostics::probe_upstream(
                        &url,
                        &last_err.clone().unwrap_or_default(),
                        std::time::Duration::from_millis(1500),
                    )
                    .await;
                    let diag_ms = (crate::tracer::now_ms() - diag_start) as u32;
                    let status = if diag.dns.ok && diag.tcp.ok {
                        "ok"
                    } else {
                        "error"
                    };
                    t.record(
                        "upstream_diagnostics",
                        "proxy_request",
                        diag_start,
                        diag_ms,
                        status,
                        serde_json::to_value(&diag).unwrap_or_default(),
                    );
                }
                t.finish("error");
            }
            state.metrics.record_upstream_attempt("exhausted");
            let msg = last_err.unwrap_or_else(|| "upstream request failed".to_string());
            tracing::warn!(error = %msg, "upstream request failed after all attempts");
            state.alerts.issue(
                "upstream_request_failed",
                "Upstream request failed",
                format!(
                    "tenant `{}` model `{model}` path `{path}` upstream `{url}`: {msg}",
                    resolved.tenant_name
                ),
            );
            drop(permit);
            let (code, http_status) = if timed_out {
                (504u16, StatusCode::GATEWAY_TIMEOUT)
            } else {
                (502u16, StatusCode::BAD_GATEWAY)
            };
            // Nothing was generated: refund the per-minute reservation and
            // release any term reservation rather than keeping the estimate.
            let total_ms = request_start.elapsed().as_millis() as u32;
            let _ = settle_guard
                .complete(accounting.settle_unbilled(0, total_ms, code))
                .await;
            let detail = if timed_out {
                "upstream request timed out"
            } else {
                "upstream request failed"
            };
            return error_json(http_status, detail);
        }
    };
    let status_code = upstream.status().as_u16();
    if status_code >= 500 {
        state.alerts.issue(
            "upstream_5xx_response",
            "Upstream returned a server error",
            format!(
                "tenant `{}` model `{model}` path `{path}` upstream `{url}` status `{status_code}`",
                resolved.tenant_name
            ),
        );
    }
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| header::HeaderValue::from_static("application/json"));
    let content_type_str = content_type
        .to_str()
        .unwrap_or("application/json")
        .to_string();

    // An upstream error (4xx/5xx) is a small, non-streaming JSON body, so buffer
    // it and fold it into the trace's `upstream` span — otherwise the tracer only
    // ever sees a bare status code and operators can't tell *why* a backend
    // rejected the request (e.g. vLLM's "model `x` does not exist" or a
    // context-length 400). The body is then replayed to the client verbatim.
    // (4xx never reaches the streaming/boon paths below — they short-circuit
    // here — so the only behaviour change is the captured body + buffered reply.)
    if status_code >= 400 {
        let mut buf: Vec<u8> = Vec::new();
        let mut ttft_ms = 0u32;
        let mut byte_stream = upstream.bytes_stream();
        while let Some(item) = byte_stream.next().await {
            match item {
                Ok(chunk) => {
                    if buf.is_empty() && !chunk.is_empty() {
                        ttft_ms = upstream_start.elapsed().as_millis() as u32;
                    }
                    let room = BODY_LIMIT.saturating_sub(buf.len());
                    if chunk.len() > room {
                        buf.extend_from_slice(&chunk[..room]);
                        break;
                    }
                    buf.extend_from_slice(&chunk);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "upstream read failed reading error body");
                    break;
                }
            }
        }
        drop(permit);

        if let Some(ref mut t) = tracer {
            let body = String::from_utf8_lossy(&buf);
            let snippet: String = body.chars().take(ERROR_BODY_TRACE_CAP).collect();
            t.record(
                "upstream",
                "proxy_request",
                dispatch_start,
                dispatch_ms,
                "error",
                serde_json::json!({
                    "model": model,
                    "url": url.as_str(),
                    "status": status_code,
                    "targets": total_targets,
                    "body": snippet,
                }),
            );
        }

        // A failed upstream call is billed only for the usage its body reports.
        // With none reported it is billed nothing: the row keeps the status,
        // the per-minute reservation is refunded, and any term reservation is
        // released. Errors are never cached.
        let total_ms = request_start.elapsed().as_millis() as u32;
        let (tokens, billed) = match extract_usage(&String::from_utf8_lossy(&buf)) {
            Some(tokens) => (tokens, true),
            None => ((0, 0), false),
        };
        let _ = settle_guard
            .complete(accounting.settle_with(tokens, ttft_ms, total_ms, status_code, None, billed))
            .await;
        if let Some(t) = tracer.take() {
            t.finish("error");
        }

        let mut builder = Response::builder()
            .status(status_code)
            .header(header::CONTENT_TYPE, content_type)
            .header("x-obleth-request-id", request_id.to_string())
            .header(NO_BUFFER_HEADER.0, NO_BUFFER_HEADER.1);
        if !boons_applied.is_empty() {
            builder = builder.header(crate::boons::BOONS_HEADER, boons_applied.join(","));
            if let Some(h) = &compression_header {
                builder = builder.header(crate::boons::COMPRESSION_HEADER, h);
            }
        }
        return builder.body(Body::from(buf)).unwrap_or_else(|_| {
            error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
        });
    }

    // Success: record the `upstream` span here (the error path above records its
    // own, enriched with the body).
    if let Some(ref mut t) = tracer {
        t.record(
            "upstream",
            "proxy_request",
            dispatch_start,
            dispatch_ms,
            "ok",
            serde_json::json!({
                "model": model,
                "url": url.as_str(),
                "status": status_code,
                "targets": total_targets,
            }),
        );
    }

    // The upstream has answered and is generating: from here a client that
    // disconnects is billed the estimate, as usage will never be delivered.
    accounting.arm_estimate(&mut settle_guard);

    // Cache only successful responses.
    let store_in_cache = cache_key.clone();

    // ---- video job create: record the job before its id leaves ----
    // The response is a small JSON video object. It is read whole, its id is
    // recorded against this tenant, this key and the target that accepted
    // it, and only then is it returned — an unrecorded id could never be
    // followed up. The create is billed its flat `cost_per_video` (no
    // tokens) once recorded, and nothing when it is not. The whole step runs
    // as its own task so a client that leaves mid-way cannot strand a job it
    // was charged for.
    if video_create {
        if let Some(t) = tracer.take() {
            t.finish("ok");
        }
        let job = crate::videos::CreatedJob {
            jobs: state.video_jobs.clone(),
            http: state.http.clone(),
            model: model.clone(),
            tenant_id: resolved.tenant_id,
            key_id: resolved.key_id,
            target: Target {
                base: targets[served_target].base.clone(),
                api_key: targets[served_target].api_key.clone(),
                headers: targets[served_target].headers.clone(),
            },
            started: upstream_start,
        };
        let recorded = tokio::spawn(async move {
            let outcome = crate::videos::record_create(job, upstream).await;
            drop(permit);
            let total_ms = request_start.elapsed().as_millis() as u32;
            let settle = accounting.settle_with(
                (0, 0),
                outcome.ttft_ms(),
                total_ms,
                outcome.status().as_u16(),
                None,
                outcome.billed(),
            );
            let _ = settle_guard.complete(settle).await;
            outcome
        });
        return match recorded.await {
            Ok(outcome) => outcome.into_response(request_id),
            Err(_) => error_json(StatusCode::INTERNAL_SERVER_ERROR, "video create failed"),
        };
    }

    // Extract guardrails policy for log_only output scanning (evaluated after stream drains).
    let scan_policy = resolved
        .guardrails_policy
        .as_ref()
        .filter(|p| {
            status_code == 200
                && !p.output_scanners.is_empty()
                && matches!(p.action, obleth_config::GuardrailsAction::LogOnly)
        })
        .cloned();
    let mut output_monitor = scan_policy.as_ref().map(|_| {
        crate::output_monitor::OutputMonitor::new(content_type_str.contains("text/event-stream"))
    });

    // ---- streaming gateway tool loop ----
    // When the only response transform is the tool loop and the client asked to
    // stream, drive the loop live (see `stream_tap`): the model's content and
    // reasoning stream straight through, a visible marker is shown when a
    // gateway tool runs, and only the tool execution between turns pauses the
    // stream. The first turn reuses the upstream response already opened above.
    if stream_tap && status_code == 200 {
        if let (Some(plan), Some(route_owned)) = (response_plan.as_ref(), route.clone()) {
            if let Some(loop_plan) = &plan.tool_loop {
                let stats = std::sync::Arc::new(std::sync::Mutex::new(
                    crate::boons::tool_stream::StreamStats::default(),
                ));
                let driver = crate::boons::tool_stream::run(
                    crate::boons::tool_stream::StreamLoop {
                        state: state.clone(),
                        route: (*route_owned).clone(),
                        key: (*resolved).clone(),
                        session_id: req_meta.session_id.clone(),
                        base_request: loop_plan.request.clone(),
                        tool_servers: loop_plan.tool_servers.clone(),
                        settings: loop_plan.settings.clone(),
                        passthrough_unmapped: loop_plan.passthrough_unmapped,
                        image_gen: loop_plan.image_gen.clone(),
                        dispatch_timeout: req_timeout,
                        client_include_usage: plan.include_usage,
                        upstream_start,
                    },
                    upstream,
                    stats.clone(),
                );

                let body_stream = async_stream::stream! {
                    futures_util::pin_mut!(driver);
                    while let Some(item) = driver.next().await {
                        if let (Some(monitor), Ok(chunk)) = (output_monitor.as_mut(), &item) {
                            monitor.push(chunk);
                        }
                        yield item;
                    }
                    drop(permit);
                    let (ttft_ms, input_tokens, output_tokens) = {
                        let s = stats.lock().unwrap_or_else(|e| e.into_inner());
                        let toks = if s.final_set {
                            (s.input_tokens, s.output_tokens)
                        } else {
                            (est.input_tokens, est.estimated_output_tokens)
                        };
                        (s.ttft_ms, toks.0, toks.1)
                    };
                    let total_ms = request_start.elapsed().as_millis() as u32;
                    accounting.monitor(scan_policy.as_ref(), output_monitor);
                    let _ = settle_guard.complete(accounting.settle(
                        (input_tokens, output_tokens), ttft_ms, total_ms, status_code, None,
                    )).await;
                };
                if let Some(t) = tracer.take() {
                    t.finish(if status_code < 400 { "ok" } else { "error" });
                }
                let mut builder = Response::builder()
                    .status(status_code)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .header("x-obleth-request-id", request_id.to_string())
                    .header(NO_BUFFER_HEADER.0, NO_BUFFER_HEADER.1);
                if !boons_applied.is_empty() {
                    builder = builder.header(crate::boons::BOONS_HEADER, boons_applied.join(","));
                    if let Some(h) = &compression_header {
                        builder = builder.header(crate::boons::COMPRESSION_HEADER, h);
                    }
                }
                return builder
                    .body(Body::from_stream(body_stream))
                    .unwrap_or_else(|_| {
                        error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
                    });
            }
        }
    }

    // ---- boon response interception (structured output / buffered tool loop) ----
    // The upstream call was forced non-streaming; buffer the completion,
    // transform it, and reply — synthesizing SSE when the client asked for a
    // stream. Fail-open: a body that can't be buffered or parsed passes
    // through verbatim. Non-200 responses skip transformation entirely and
    // fall through to the normal pass-through path below.
    if let Some(mut plan) = response_plan.filter(|_| status_code == 200) {
        // Follow-up tool turns pin to the endpoint that served turn 0.
        if let Some(tool_loop) = plan.tool_loop.as_mut() {
            tool_loop.served_url = Some(upstream.url().to_string());
        }
        // Buffer the upstream body, recording TTFT at the first byte for
        // metric continuity with the streaming path.
        let mut buf: Vec<u8> = Vec::new();
        let mut ttft_ms = 0u32;
        let mut truncated = false;
        let mut byte_stream = upstream.bytes_stream();
        while let Some(item) = byte_stream.next().await {
            match item {
                Ok(chunk) => {
                    if buf.is_empty() && !chunk.is_empty() {
                        ttft_ms = upstream_start.elapsed().as_millis() as u32;
                    }
                    if buf.len().saturating_add(chunk.len()) > BODY_LIMIT {
                        truncated = true;
                        break;
                    }
                    buf.extend_from_slice(&chunk);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "upstream read failed during boon interception");
                    truncated = true;
                    break;
                }
            }
        }

        // Parse + transform. Oversized, truncated, or unparseable bodies pass
        // through unchanged (fail-open); only well-formed completions are
        // rewritten.
        let mut warning: Option<&'static str> = None;
        // Usage the main row settles with when the buffered tool loop replaced
        // the body (the follow-up turns are billed as helper rows).
        let mut turn0_usage: Option<(u32, u32)> = None;
        let mut completion: Option<serde_json::Value> = (!truncated
            && buf.len() <= BOON_BUFFER_MAX)
            .then(|| serde_json::from_slice::<serde_json::Value>(&buf).ok())
            .flatten();
        let (final_body, final_content_type): (Bytes, String) = match completion.as_mut() {
            Some(body_json) => {
                // Tool-loop and repair calls run while the fairshare permit is
                // still held — they occupy upstream capacity just like the
                // original call. `tool_loop::run` falls through to the plain
                // boon transform when no tool loop is armed.
                let outcome = crate::boons::tool_loop::run(
                    &state,
                    &plan,
                    route.as_deref(),
                    &resolved,
                    &req_meta.session_id,
                    req_timeout,
                    body_json,
                    tracer.as_mut(),
                )
                .await;
                warning = outcome.warning;
                turn0_usage = outcome.turn0_usage;
                // guardrails output scan (block/redact action)
                if let Some(guard_plan) = &plan.guardrails {
                    match crate::boons::guardrails::apply_output(
                        &state,
                        &guard_plan.settings,
                        &guard_plan.policy,
                        &resolved,
                        &req_meta.session_id,
                        body_json,
                        tracer.as_mut(),
                    )
                    .await
                    {
                        crate::boons::guardrails::ApplyOutputResult::Block(block) => {
                            drop(permit);
                            // The upstream did generate the blocked answer, so
                            // the request settles with its real usage even
                            // though the client only sees the block.
                            let tokens = turn0_usage
                                .or_else(|| completion_body_usage(body_json))
                                .unwrap_or((est.input_tokens, est.estimated_output_tokens));
                            let total_ms = request_start.elapsed().as_millis() as u32;
                            let _ = settle_guard
                                .complete(accounting.settle(
                                    tokens,
                                    ttft_ms,
                                    total_ms,
                                    block.status.as_u16(),
                                    None,
                                ))
                                .await;
                            if let Some(t) = tracer.take() {
                                t.finish("error");
                            }
                            return error_json(block.status, block.reason);
                        }
                        crate::boons::guardrails::ApplyOutputResult::Pass => {}
                    }
                }
                if plan.client_stream {
                    (
                        Bytes::from(crate::boons::respond::synthesize_sse(
                            body_json,
                            plan.include_usage,
                        )),
                        "text/event-stream".to_string(),
                    )
                } else {
                    (
                        serde_json::to_vec(body_json)
                            .map(Bytes::from)
                            .unwrap_or_else(|_| Bytes::from(std::mem::take(&mut buf))),
                        content_type_str.clone(),
                    )
                }
            }
            None => (
                Bytes::from(std::mem::take(&mut buf)),
                content_type_str.clone(),
            ),
        };
        drop(permit);

        let (input_tokens, output_tokens) = turn0_usage
            .or_else(|| completion.as_ref().and_then(completion_body_usage))
            .unwrap_or((est.input_tokens, est.estimated_output_tokens));
        let total_ms = request_start.elapsed().as_millis() as u32;

        // Cache the *transformed* body so cache hits replay exactly what the
        // client received. The cache key was computed from the client body
        // (original `stream` flag included), so JSON and SSE representations
        // never collide.
        let cache_body = (!truncated && final_body.len() <= CACHE_MAX_BYTES)
            .then(|| String::from_utf8_lossy(&final_body).into_owned());
        let cache_put = match (&store_in_cache, cache_body) {
            (Some(ck), Some(body)) => {
                Some((ck.clone(), cache_ttl, final_content_type.clone(), body))
            }
            _ => None,
        };
        let _ = settle_guard
            .complete(accounting.settle(
                (input_tokens, output_tokens),
                ttft_ms,
                total_ms,
                status_code,
                cache_put,
            ))
            .await;

        let mut builder = Response::builder()
            .status(status_code)
            .header(header::CONTENT_TYPE, final_content_type)
            .header("x-obleth-request-id", request_id.to_string())
            .header(NO_BUFFER_HEADER.0, NO_BUFFER_HEADER.1);
        if !boons_applied.is_empty() {
            builder = builder.header(crate::boons::BOONS_HEADER, boons_applied.join(","));
            if let Some(h) = &compression_header {
                builder = builder.header(crate::boons::COMPRESSION_HEADER, h);
            }
        }
        if let Some(w) = warning {
            builder = builder.header(crate::boons::BOONS_WARNING_HEADER, w);
        }
        if let Some(t) = tracer.take() {
            t.finish(if status_code < 400 { "ok" } else { "error" });
        }
        return builder.body(Body::from(final_body)).unwrap_or_else(|_| {
            error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
        });
    }

    // Finish the tracer before entering the async stream body — the stream macro
    // cannot capture a non-Clone, non-Send value.
    if let Some(t) = tracer.take() {
        t.finish(if status_code < 400 { "ok" } else { "error" });
    }

    // ---- stream back, inspecting for actual usage, then reconcile ----
    let body_stream = async_stream::stream! {
        let mut byte_stream = upstream.bytes_stream();
        let mut first = true;
        let mut ttft_ms = 0u32;
        let mut tail: Vec<u8> = Vec::with_capacity(TAIL_CAP.min(4 * 1024));
        let mut full: Vec<u8> = Vec::new();
        let mut cacheable = store_in_cache.is_some();
        let mut usage_filter = strip_usage_chunk.then(UsageChunkFilter::default);
        let mut upstream_error: Option<String> = None;
        // Characters of generated text delivered so far, for billing a stream
        // the upstream breaks off before reporting usage.
        let mut streamed_chars: usize = 0;

        while let Some(item) = byte_stream.next().await {
            match item {
                Ok(chunk) => {
                    if first {
                        ttft_ms = upstream_start.elapsed().as_millis() as u32;
                        first = false;
                    }
                    // Usage is read from the raw upstream bytes, before the
                    // gateway-requested usage chunk is filtered out.
                    append_tail(&mut tail, &chunk);
                    streamed_chars += delta_text_chars(&chunk);
                    let chunk = match usage_filter.as_mut() {
                        Some(filter) => filter.push(chunk),
                        None => chunk,
                    };
                    if chunk.is_empty() {
                        continue;
                    }
                    if let Some(monitor) = output_monitor.as_mut() { monitor.push(&chunk); }
                    if cacheable {
                        if full.len() + chunk.len() <= CACHE_MAX_BYTES {
                            full.extend_from_slice(&chunk);
                        } else {
                            cacheable = false;
                            full = Vec::new();
                        }
                    }
                    yield Ok::<Bytes, std::io::Error>(chunk);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "upstream stream error");
                    accounting.state.alerts.issue(
                        "upstream_stream_error",
                        "Upstream stream failed",
                        format!(
                            "tenant `{}` model `{model}` status `{status_code}`: {e}",
                            accounting.resolved.tenant_name
                        ),
                    );
                    cacheable = false;
                    upstream_error = Some(e.to_string());
                    break;
                }
            }
        }
        if upstream_error.is_none() {
            // A partial trailing event is passed through as received.
            if let Some(rest) = usage_filter.take().map(UsageChunkFilter::finish) {
                if !rest.is_empty() {
                    if let Some(monitor) = output_monitor.as_mut() { monitor.push(&rest); }
                    if cacheable {
                        if full.len() + rest.len() <= CACHE_MAX_BYTES {
                            full.extend_from_slice(&rest);
                        } else {
                            cacheable = false;
                            full = Vec::new();
                        }
                    }
                    yield Ok::<Bytes, std::io::Error>(rest);
                }
            }
        }

        // The upstream stream has fully drained: the request no longer occupies
        // upstream capacity, so release the fairshare slot *before* the Redis
        // bookkeeping below. Cache stores and budget reconciliation are
        // accounting, not occupancy; holding the permit through them would
        // shrink effective concurrency whenever Redis is slow.
        drop(permit);

        let usage = extract_usage(&String::from_utf8_lossy(&tail));
        let total_ms = request_start.elapsed().as_millis() as u32;
        accounting.monitor(scan_policy.as_ref(), output_monitor);

        if let Some(err) = upstream_error {
            // A stream the upstream broke off is a failed call (502), billed
            // for what reached the client: reported usage, else the streamed
            // text's estimated tokens; nothing only when nothing streamed.
            // Settlement is handed off before the error is yielded: the error
            // makes hyper drop this body, and the guard would otherwise settle
            // it as a client disconnect.
            let (tokens, billed) = truncated_stream_billing(usage, streamed_chars, est);
            let _ = settle_guard.complete(accounting.settle_with(
                tokens, ttft_ms, total_ms, 502, None, billed,
            )).await;
            // An error item aborts the response instead of ending it cleanly,
            // so the client cannot mistake the truncated body for a whole one.
            yield Err(std::io::Error::other(format!("upstream stream failed: {err}")));
        } else {
            let tokens = usage.unwrap_or((est.input_tokens, est.estimated_output_tokens));
            // store the full response for identical future requests
            let cache_put = if cacheable && status_code == 200 {
                store_in_cache.as_deref().map(|ck| {
                    // Take ownership of the buffer instead of copying it; the
                    // lossy re-encode only runs for invalid UTF-8 (never for the
                    // JSON/SSE bodies this cache is meant for).
                    let body = String::from_utf8(std::mem::take(&mut full))
                        .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
                    (ck.to_string(), cache_ttl, content_type_str.clone(), body)
                })
            } else {
                None
            };
            let _ = settle_guard.complete(accounting.settle(
                tokens, ttft_ms, total_ms, status_code, cache_put,
            )).await;
        }
    };

    let mut builder = Response::builder().status(status_code);
    builder = builder
        .header(header::CONTENT_TYPE, content_type)
        .header("x-obleth-request-id", request_id.to_string())
        .header(NO_BUFFER_HEADER.0, NO_BUFFER_HEADER.1);
    if !boons_applied.is_empty() {
        builder = builder.header(crate::boons::BOONS_HEADER, boons_applied.join(","));
        if let Some(h) = &compression_header {
            builder = builder.header(crate::boons::COMPRESSION_HEADER, h);
        }
    }
    builder
        .body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

/// Above this much held, unterminated buffer, [`UsageChunkFilter`] gives up
/// waiting for an SSE blank-line terminator and flushes what it has. An
/// upstream that never emits one (non-compliant, or a non-SSE error body)
/// would otherwise hold the whole stream indefinitely.
const USAGE_FILTER_MAX_PENDING: usize = 64 * 1024;

/// Drops the usage-only SSE event the gateway asked the upstream for on a
/// client's behalf (see `prepare_upstream_body`), passing every other event
/// through byte-for-byte. Complete events are released as soon as their
/// terminating blank line arrives, so it adds no latency to a live stream.
#[derive(Default)]
struct UsageChunkFilter {
    pending: Vec<u8>,
    /// Set once the cap has been hit, so the fallback is logged only once
    /// per stream rather than on every subsequent chunk.
    overflowed: bool,
}

impl UsageChunkFilter {
    fn push(&mut self, chunk: Bytes) -> Bytes {
        // Common case: a chunk of whole events with no usage in it.
        if self.pending.is_empty()
            && (chunk.ends_with(b"\n\n") || chunk.ends_with(b"\r\n\r\n"))
            && find_bytes(&chunk, b"prompt_tokens").is_none()
        {
            return chunk;
        }
        self.pending.extend_from_slice(&chunk);
        let mut out = Vec::with_capacity(self.pending.len());
        while let Some(end) = sse_event_end(&self.pending) {
            let event: Vec<u8> = self.pending.drain(..end).collect();
            if !is_usage_only_event(&event) {
                out.extend_from_slice(&event);
            }
        }
        // No event boundary showed up and the buffer has grown past the cap:
        // stop holding it hostage. The client may then see the injected usage
        // chunk verbatim, which is acceptable next to holding the stream.
        if self.pending.len() > USAGE_FILTER_MAX_PENDING {
            if !self.overflowed {
                self.overflowed = true;
                tracing::debug!(
                    pending_bytes = self.pending.len(),
                    "usage-chunk filter buffer exceeded cap; flushing unfiltered"
                );
            }
            out.extend_from_slice(&self.pending);
            self.pending.clear();
        }
        Bytes::from(out)
    }

    fn finish(self) -> Bytes {
        Bytes::from(self.pending)
    }
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Index just past the first SSE event terminator (a blank line).
fn sse_event_end(buf: &[u8]) -> Option<usize> {
    let lf = find_bytes(buf, b"\n\n").map(|i| i + 2);
    let crlf = find_bytes(buf, b"\r\n\r\n").map(|i| i + 4);
    match (lf, crlf) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, b) => a.or(b),
    }
}

/// An event carrying only usage: a `usage` object and no choices. Content
/// chunks that also carry usage (continuous usage stats) are not dropped.
fn is_usage_only_event(event: &[u8]) -> bool {
    if find_bytes(event, b"prompt_tokens").is_none() {
        return false;
    }
    let text = String::from_utf8_lossy(event);
    let payload = text
        .lines()
        .filter_map(|l| l.trim_start().strip_prefix("data:"))
        .map(str::trim)
        .collect::<Vec<_>>()
        .join("\n");
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&payload) else {
        return false;
    };
    let has_usage = value.get("usage").is_some_and(serde_json::Value::is_object);
    let no_choices = match value.get("choices") {
        None => true,
        Some(c) => c.as_array().is_some_and(|a| a.is_empty()),
    };
    has_usage && no_choices
}

/// Which term reservations a request holds, and the cost estimate they were
/// made with; settlement must release exactly these.
#[derive(Clone, Copy, Default)]
pub(crate) struct TermHolds {
    pub(crate) tenant: bool,
    pub(crate) key: bool,
    pub(crate) est_cost: f64,
}

/// Admission-time cost estimate, priced the same way settlement prices
/// actual usage, so a term reservation and its reconcile agree.
pub(crate) fn estimated_cost(
    est: CostEstimate,
    in_rate: f64,
    out_rate: f64,
    modality_cost: f64,
) -> f64 {
    (est.input_tokens as f64) * in_rate
        + (est.estimated_output_tokens as f64) * out_rate
        + modality_cost
}

/// Frozen `(cost_usd, energy)` for a settled request. Energy is wall-time
/// slot-share, so a request that held a slot is charged it even when it
/// produced nothing billable; only the token-priced cost is waived then.
fn settled_figures(
    energy: &crate::energy::EnergyEngine,
    billed: bool,
    tokens: (u32, u32),
    rates: (f64, f64, f64),
    energy_slots: i64,
    total_ms: u32,
    queue_wait_ms: u32,
) -> (f64, crate::energy::EnergyFigures) {
    let (in_rate, out_rate, modality_cost) = rates;
    let cost_usd = if billed {
        (tokens.0 as f64) * in_rate + (tokens.1 as f64) * out_rate + modality_cost
    } else {
        0.0
    };
    (
        cost_usd,
        energy.compute(energy_slots, total_ms, queue_wait_ms),
    )
}

/// A key-scope term reservation made before the tenant step, not yet owned by
/// the settlement guard. If the request ends while it is armed (a tenant
/// rejection that forgot it, or the client leaving mid-await), `Drop` releases
/// it in the background so it cannot block headroom until the TTL lapses.
pub(crate) struct PendingTermHold {
    state: AppState,
    scope: Uuid,
    period: String,
    est: CostEstimate,
    est_cost: f64,
    armed: bool,
}

impl PendingTermHold {
    pub(crate) fn new(
        state: &AppState,
        scope: Uuid,
        period: String,
        est: CostEstimate,
        est_cost: f64,
    ) -> Self {
        Self {
            state: state.clone(),
            scope,
            period,
            est,
            est_cost,
            armed: true,
        }
    }

    /// Release now. The release runs as its own task, so cancelling this
    /// await neither loses it nor lets `Drop` run it a second time.
    pub(crate) async fn release(mut self) {
        self.armed = false;
        let _ = tokio::spawn(self.release_task()).await;
    }

    /// Settlement now owns the reservation.
    pub(crate) fn hand_off(mut self) {
        self.armed = false;
    }

    fn release_task(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let (state, scope, period, est, est_cost) = (
            self.state.clone(),
            self.scope,
            self.period.clone(),
            self.est,
            self.est_cost,
        );
        async move { release_term_hold(&state, &scope, Some(&period), est, est_cost).await }
    }
}

impl Drop for PendingTermHold {
    fn drop(&mut self) {
        if self.armed {
            tokio::spawn(self.release_task());
        }
    }
}

/// Release a term reservation for a request that will never settle (it was
/// rejected by a later admission step). Best effort: a failure only leaves
/// the reservation to lapse with its key's TTL.
async fn release_term_hold(
    state: &AppState,
    scope: &Uuid,
    period: Option<&str>,
    est: CostEstimate,
    est_cost: f64,
) {
    let Some(period) = period else {
        return;
    };
    if let Err(e) = state
        .redis
        .term_reconcile(
            &scope.to_string(),
            period,
            est.total() as i64,
            est_cost,
            0,
            0.0,
        )
        .await
    {
        tracing::warn!(error = %e, "term reservation release failed");
    }
}

/// Tokens and billing for a stream the upstream broke off. Reported usage
/// wins; otherwise the text already delivered is estimated the way the
/// admission estimator counts text (`HeuristicTokenizer::count_text`,
/// ~4 characters per token) on top of the prompt estimate. Nothing streamed
/// means nothing generated, so it is unbilled.
fn truncated_stream_billing(
    usage: Option<(u32, u32)>,
    streamed_chars: usize,
    est: CostEstimate,
) -> ((u32, u32), bool) {
    if let Some(tokens) = usage {
        return (tokens, true);
    }
    if streamed_chars == 0 {
        return ((0, 0), false);
    }
    let output = u32::try_from(streamed_chars / 4).unwrap_or(u32::MAX).max(1);
    ((est.input_tokens, output), true)
}

/// Characters of generated text in one raw response chunk: the string values
/// of `content`, `reasoning_content`, `reasoning` and `text` fields. A byte
/// scan rather than a JSON parse so it can run on every streamed chunk; a
/// string split across two chunks is only partly counted, which errs low.
///
/// Some upstreams mirror the same reasoning text under both
/// `reasoning_content` and `reasoning` in one delta; counting both would
/// double the estimate, so `reasoning` is only counted when the chunk has no
/// `reasoning_content` text of its own.
fn delta_text_chars(chunk: &[u8]) -> usize {
    let content = field_text_chars(chunk, b"\"content\"");
    let reasoning_content = field_text_chars(chunk, b"\"reasoning_content\"");
    let reasoning = if reasoning_content > 0 {
        0
    } else {
        field_text_chars(chunk, b"\"reasoning\"")
    };
    let text = field_text_chars(chunk, b"\"text\"");
    content + reasoning_content + reasoning + text
}

/// Characters in every string value that follows `key` in the chunk.
fn field_text_chars(chunk: &[u8], key: &[u8]) -> usize {
    let skip_spaces = |mut i: usize| {
        while i < chunk.len() && chunk[i] == b' ' {
            i += 1;
        }
        i
    };
    let mut total = 0;
    let mut from = 0;
    while let Some(at) = find_bytes(&chunk[from..], key) {
        let mut i = skip_spaces(from + at + key.len());
        if i >= chunk.len() || chunk[i] != b':' {
            from = i;
            continue;
        }
        i = skip_spaces(i + 1);
        if i >= chunk.len() || chunk[i] != b'"' {
            // `null` or a non-string value.
            from = i;
            continue;
        }
        i += 1;
        while i < chunk.len() {
            match chunk[i] {
                b'"' => {
                    i += 1;
                    break;
                }
                b'\\' => {
                    total += 1;
                    i += if chunk.get(i + 1) == Some(&b'u') {
                        6
                    } else {
                        2
                    };
                }
                // UTF-8 continuation bytes belong to the character before.
                b if b & 0xC0 == 0x80 => i += 1,
                _ => {
                    total += 1;
                    i += 1;
                }
            }
        }
        from = i.min(chunk.len());
    }
    total
}

/// Usage a buffered chat completion reports, if any.
fn completion_body_usage(body: &serde_json::Value) -> Option<(u32, u32)> {
    let input = body.pointer("/usage/prompt_tokens")?.as_u64()? as u32;
    let output = body
        .pointer("/usage/completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    Some((input, output))
}

/// Owned admission snapshot: cancellation bookkeeping must not borrow the body
/// that is being dropped. Prices and periods are the same as normal settlement.
#[derive(Clone)]
pub(crate) struct StreamAccounting {
    pub(crate) state: AppState,
    pub(crate) request_id: Uuid,
    pub(crate) resolved: Arc<ResolvedKey>,
    pub(crate) meta: RequestMeta,
    pub(crate) model: String,
    pub(crate) admission: Admission,
    pub(crate) est: CostEstimate,
    pub(crate) queue_wait_ms: u32,
    pub(crate) request_start: Instant,
    pub(crate) cache_status: String,
    pub(crate) capacity: i64,
    pub(crate) term_period: Option<String>,
    pub(crate) key_term_period: Option<String>,
    pub(crate) in_cost_rate: f64,
    pub(crate) out_cost_rate: f64,
    pub(crate) modality_cost: f64,
    pub(crate) energy_slots: i64,
    pub(crate) holds: TermHolds,
}

impl StreamAccounting {
    fn monitor(
        &self,
        policy: Option<&obleth_config::GuardrailsPolicy>,
        monitor: Option<crate::output_monitor::OutputMonitor>,
    ) {
        if let (Some(policy), Some(monitor)) = (policy, monitor) {
            match monitor.finish() {
                Ok(completion) => crate::boons::guardrails::monitor_output(
                    &self.state,
                    &self.state.boons.settings().guardrails,
                    policy,
                    &self.resolved,
                    &self.meta.session_id,
                    self.request_id,
                    &completion,
                ),
                Err(reason) => {
                    tracing::warn!(request_id = %self.request_id, reason, "output monitoring skipped")
                }
            }
        }
    }

    /// Cancellation fallback settling as status 499. `tokens` is `None` before
    /// the upstream has answered (nothing generated, billed nothing) and the
    /// estimate afterwards. Runtime shutdown remains best-effort, just like
    /// the asynchronous telemetry sink; a partial answer is never cached.
    fn on_cancel(&self, tokens: Option<(u32, u32)>) -> impl FnOnce() + Send + 'static {
        let accounting = self.clone();
        move || {
            let elapsed = accounting.request_start.elapsed().as_millis() as u32;
            let (tokens, billed) = match tokens {
                Some(tokens) => (tokens, true),
                None => ((0, 0), false),
            };
            tokio::spawn(accounting.settle_with(tokens, 0, elapsed, 499, None, billed));
        }
    }

    pub(crate) fn unbilled_guard(&self) -> crate::completion::CompletionGuard {
        crate::completion::CompletionGuard::new(self.on_cancel(None))
    }

    fn arm_estimate(&self, guard: &mut crate::completion::CompletionGuard) {
        guard.rearm(self.on_cancel(Some((
            self.est.input_tokens,
            self.est.estimated_output_tokens,
        ))));
    }

    pub(crate) async fn settle(
        self,
        tokens: (u32, u32),
        ttft_ms: u32,
        total_ms: u32,
        status_code: u16,
        cache_put: Option<(String, i64, String, String)>,
    ) {
        self.settle_with(tokens, ttft_ms, total_ms, status_code, cache_put, true)
            .await;
    }

    /// Settle a request that produced nothing billable: zero tokens and
    /// cost, full refund of every reservation (energy is still charged).
    pub(crate) async fn settle_unbilled(self, ttft_ms: u32, total_ms: u32, status_code: u16) {
        self.settle_with((0, 0), ttft_ms, total_ms, status_code, None, false)
            .await;
    }

    async fn settle_with(
        self,
        tokens: (u32, u32),
        ttft_ms: u32,
        total_ms: u32,
        status_code: u16,
        cache_put: Option<(String, i64, String, String)>,
        billed: bool,
    ) {
        let cache_enabled = cache_put.is_some();
        let (key, ttl, content_type, body) = cache_put.unwrap_or_default();
        settle_inner(
            &self.state,
            self.request_id,
            &self.resolved,
            &self.meta,
            &self.model,
            self.admission,
            self.est,
            tokens.0,
            tokens.1,
            self.queue_wait_ms,
            ttft_ms,
            total_ms,
            status_code,
            &self.cache_status,
            self.capacity,
            self.term_period.as_deref(),
            self.key_term_period.as_deref(),
            self.in_cost_rate,
            self.out_cost_rate,
            self.modality_cost,
            self.energy_slots,
            cache_enabled.then_some((key.as_str(), ttl, content_type.as_str(), body)),
            self.holds,
            billed,
        )
        .await;
    }
}

/// End-of-request bookkeeping shared by the streaming pass-through path and
/// the boon interception path: cache store, per-minute budget reconciliation,
/// term-usage commit + budget alerts, and the usage-ledger record.
///
/// `cache_put` carries `(key, ttl, content_type, body)` when the response
/// should be stored for identical future requests; callers gate it on a
/// successful (200) response.
///
/// `holds` names the term reservations made at admission, which are
/// reconciled (released, actual committed) rather than added to. `billed =
/// false` settles a request that produced nothing billable: tokens and cost
/// are frozen at zero (energy is still charged) and the reservations are
/// fully refunded.
#[allow(clippy::too_many_arguments)]
async fn settle_inner(
    state: &AppState,
    request_id: Uuid,
    resolved: &ResolvedKey,
    meta: &RequestMeta,
    model: &str,
    admission: Admission,
    est: CostEstimate,
    input_tokens: u32,
    output_tokens: u32,
    queue_wait_ms: u32,
    ttft_ms: u32,
    total_ms: u32,
    status_code: u16,
    cache_status: &str,
    capacity: i64,
    term_period: Option<&str>,
    key_term_period: Option<&str>,
    in_cost_rate: f64,
    out_cost_rate: f64,
    modality_cost: f64,
    energy_slots: i64,
    cache_put: Option<(&str, i64, &str, String)>,
    holds: TermHolds,
    billed: bool,
) {
    // Feed the router's per-request cost estimate: one EWMA sample of how
    // long this model's answers actually run. Successful requests only — an
    // error body's usage says nothing about the model's answering behavior.
    if status_code == 200 && output_tokens > 0 {
        state.output_stats.observe(model, output_tokens as u64);
    }

    // store the full response for identical future requests
    if let Some((ck, ttl, content_type, body)) = cache_put {
        let cached = obleth_config::CachedResponse {
            status: status_code,
            content_type: content_type.to_string(),
            body,
            input_tokens,
            output_tokens,
        };
        if let Err(e) = state.redis.cache_put(ck, &cached, ttl).await {
            tracing::warn!(error = %e, "cache store failed");
            state.alerts.issue(
                "redis_cache_store_failed",
                "Redis response-cache store failed",
                format!(
                    "tenant `{}` model `{model}` cache ttl `{ttl}`: {e}",
                    resolved.tenant_name
                ),
            );
        }
    }

    // Reconcile estimate vs actual against the per-minute budget bucket.
    // A zero token rate means the tenant has no per-minute limiter.
    if capacity > 0 {
        if let Err(e) = state
            .redis
            .reconcile_budget(
                &resolved.tenant_id,
                capacity,
                est.total(),
                input_tokens.saturating_add(output_tokens),
            )
            .await
        {
            tracing::warn!(error = %e, "budget reconcile failed");
            state.alerts.issue(
                "redis_budget_reconcile_failed",
                "Redis budget reconcile failed",
                format!(
                    "tenant `{}` model `{model}` estimated `{}` actual `{}`: {e}",
                    resolved.tenant_name,
                    est.total(),
                    input_tokens.saturating_add(output_tokens),
                ),
            );
        }
    }

    // ---- term-usage commit + budget alerts (Phase 3 + Phase 5) ----
    // Frozen request cost: per-token rates (captured at admission) plus any
    // per-request modality surcharge. Computed once and used for both the
    // term-budget commit and the persisted usage ledger so they agree.
    // Frozen energy figures: slot-share of live cluster power over serving
    // time (queue wait excluded). Zeros when accounting is off. Frozen like
    // `cost_usd` so later settings edits never rewrite history.
    let (cost_usd, energy) = settled_figures(
        &state.energy,
        billed,
        (input_tokens, output_tokens),
        (in_cost_rate, out_cost_rate, modality_cost),
        energy_slots,
        total_ms,
        queue_wait_ms,
    );
    let added = input_tokens.saturating_add(output_tokens) as i64;
    let est_tokens = est.total() as i64;
    if let Some(period) = term_period {
        let committed = if holds.tenant {
            state
                .redis
                .term_reconcile(
                    &resolved.tenant_id.to_string(),
                    period,
                    est_tokens,
                    holds.est_cost,
                    added,
                    cost_usd,
                )
                .await
        } else {
            state
                .redis
                .term_usage_add(&resolved.tenant_id, period, added, cost_usd)
                .await
        };
        match committed {
            Ok((total_tokens, total_cost)) => {
                maybe_alert_budget(state, resolved, total_tokens, total_cost);
            }
            Err(e) => {
                tracing::warn!(error = %e, "term usage commit failed");
                state.alerts.issue(
                    "redis_term_usage_failed",
                    "Redis term-usage commit failed",
                    format!("tenant `{}` model `{model}`: {e}", resolved.tenant_name),
                );
            }
        }
    }
    if let Some(period) = key_term_period {
        let committed = if holds.key {
            state
                .redis
                .term_reconcile(
                    &resolved.key_id.to_string(),
                    period,
                    est_tokens,
                    holds.est_cost,
                    added,
                    cost_usd,
                )
                .await
        } else {
            state
                .redis
                .term_usage_add(&resolved.key_id, period, added, cost_usd)
                .await
        };
        match committed {
            Ok((total_tokens, total_cost)) => {
                maybe_alert_key_budget(state, resolved, total_tokens, total_cost);
            }
            Err(e) => {
                tracing::warn!(error = %e, "key term usage commit failed");
                state.alerts.issue(
                    "redis_key_term_usage_failed",
                    "Redis key term-usage commit failed",
                    format!(
                        "tenant `{}` key `{}` model `{model}`: {e}",
                        resolved.tenant_name, resolved.key_id
                    ),
                );
            }
        }
    }

    finalize(
        state,
        request_id,
        resolved,
        meta,
        model,
        admission,
        est,
        input_tokens,
        output_tokens,
        queue_wait_ms,
        ttft_ms,
        total_ms,
        status_code,
        cache_status,
        cost_usd,
        energy,
    );
    state.metrics.total_ms.observe(total_ms as f64);
}

/// Resolve a key via moka, falling back to Redis and caching the result.
/// Distinguishes a Redis error from a miss so callers can fail closed on the
/// former (503, backend unavailable) rather than reading it as an unknown
/// credential (401) -- see `jwt_auth::authenticate_credential`.
#[tracing::instrument(skip_all, name = "auth_resolve")]
pub(crate) async fn try_resolve_key(
    state: &AppState,
    hash: &str,
) -> Result<Option<Arc<ResolvedKey>>, ()> {
    if let Some(r) = state.key_cache.get(hash).await {
        return Ok(Some(r));
    }
    match state.redis.get_resolved_key(hash).await {
        Ok(Some(r)) => {
            let r = Arc::new(r);
            state.key_cache.insert(hash.to_string(), r.clone()).await;
            Ok(Some(r))
        }
        Ok(None) => Ok(None),
        Err(e) => {
            tracing::warn!(error = %e, "redis key lookup failed");
            state.alerts.issue(
                "redis_key_lookup_failed",
                "Redis key lookup failed",
                format!("API key resolution failed against Redis: {e}"),
            );
            Err(())
        }
    }
}

/// Union of routing tags across the candidates the request may actually use.
/// Restricting the classifier to achievable tags keeps it honest and cheap.
pub(crate) fn union_candidate_tags(
    candidates: &[crate::router::Candidate],
    allowed_models: Option<&[String]>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in candidates {
        if let Some(allowed) = allowed_models {
            if !allowed.iter().any(|m| m == &c.model.model_name) {
                continue;
            }
        }
        for t in &c.model.tags {
            if !out.contains(t) {
                out.push(t.clone());
            }
        }
    }
    out
}

/// Routing intent for an `auto` request. Precedence: explicit header, then the
/// classifier brain (when enabled, configured, resolvable, and not itself
/// `auto`), then cheap heuristics, then a neutral default. Every fallback
/// lowers difficulty rather than raising it, so a slow or broken brain makes
/// routing cheaper and never silently more expensive.
pub(crate) async fn derive_intent(
    state: &AppState,
    json: &serde_json::Value,
    est_input_tokens: u64,
    available_tags: &[String],
    settings: &obleth_config::AutoRouterSettings,
    effort_header: Option<&str>,
    skip_classifier: bool,
) -> crate::router::Intent {
    let forced = crate::router::difficulty_from_header(effort_header);

    if !skip_classifier && settings.classifier_active() && !available_tags.is_empty() {
        if let Some(name) = settings.classifier_model.as_deref() {
            if name != crate::router::AUTO_MODEL_NAME {
                if let Some(brain) = resolve_model(state, name).await {
                    let prompt = classifier_prompt(json);
                    if !prompt.trim().is_empty() {
                        let mut intent = state
                            .classifier
                            .classify(&state.http, &brain, &prompt, available_tags)
                            .await;
                        if !intent.tags.is_empty() {
                            if let Some(d) = forced {
                                intent.difficulty = d;
                                intent.source = crate::router::IntentSource::Header;
                            }
                            return intent;
                        }
                    }
                }
            }
        }
    }

    // Heuristic fallback (also used when the classifier is off or returns empty).
    let mut intent = crate::router::heuristic_intent(json, est_input_tokens);
    if let Some(d) = forced {
        intent.difficulty = d;
        intent.source = crate::router::IntentSource::Header;
    }
    intent
}

/// Build a compact prompt for the classifier: the system message (if any) plus
/// the first user message's text.
fn classifier_prompt(json: &serde_json::Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
        let mut have_user = false;
        for msg in messages {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let text = message_text(msg.get("content"));
            if role == "system" && !text.is_empty() {
                parts.push(text);
            } else if role == "user" && !have_user && !text.is_empty() {
                parts.push(text);
                have_user = true;
            }
            if have_user {
                break;
            }
        }
    }
    parts.join("\n")
}

fn message_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => {
            let mut out = String::new();
            for part in parts {
                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                    out.push_str(t);
                    out.push('\n');
                }
            }
            out
        }
        _ => String::new(),
    }
}

/// Bridge for the admin simulate endpoint: run the live intent classifier
/// exactly as `derive_intent` would — same brain resolution, same shared
/// cache, same timeout — on an already-built prompt and tag menu. Returns
/// `Intent::default()` (empty tags) when the classifier is off, unconfigured,
/// or unresolvable, which is the same "no signal, fall back to heuristics"
/// shape the data plane reads.
pub async fn classify_for_simulate(
    state: &AppState,
    prompt: String,
    available_tags: Vec<String>,
) -> crate::router::Intent {
    let settings = state.classifier.settings();
    if !settings.classifier_active() || available_tags.is_empty() {
        return crate::router::Intent::default();
    }
    let Some(name) = settings.classifier_model.as_deref() else {
        return crate::router::Intent::default();
    };
    if name == crate::router::AUTO_MODEL_NAME {
        return crate::router::Intent::default();
    }
    let Some(brain) = resolve_model(state, name).await else {
        return crate::router::Intent::default();
    };
    if prompt.trim().is_empty() {
        return crate::router::Intent::default();
    }
    state
        .classifier
        .classify(&state.http, &brain, &prompt, &available_tags)
        .await
}

pub(crate) async fn resolve_model(state: &AppState, name: &str) -> Option<Arc<ResolvedModel>> {
    if let Some(r) = state.model_cache.get(name).await {
        return Some(r);
    }
    match state.redis.get_resolved_model(name).await {
        Ok(Some(r)) => {
            let r = Arc::new(r);
            state.model_cache.insert(name.to_string(), r.clone()).await;
            Some(r)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "redis model lookup failed");
            state.alerts.issue(
                "redis_model_lookup_failed",
                "Redis model lookup failed",
                format!("model `{name}` lookup failed against Redis: {e}"),
            );
            None
        }
    }
}

pub(crate) fn effective_admission_weight(tenant_weight: i64, route: Option<&ResolvedModel>) -> i64 {
    let Some(route) = route else {
        return tenant_weight.max(1);
    };
    ((tenant_weight as f64 * route.admission_weight as f64) / 100.0)
        .round()
        .max(1.0) as i64
}

/// Build the scheduler request for one admission. Every admission carries the
/// caps from the resolved key on every request; the scheduler treats an
/// omitted cap as "clear the cap for this pool", so callers must never pass
/// `None` for a tenant or key that has a cap configured. Caps that are unset,
/// zero, or negative mean "no cap".
///
/// The tenant cap is read from the *per-key* cached `ResolvedKey`, so for the
/// short window after a tenant's quota changes, two keys of the same tenant can
/// carry different tenant caps and the last admit wins for that pool. It
/// self-corrects once the cached keys refresh.
pub(crate) fn admit_request_for(
    resolved: &ResolvedKey,
    model: &str,
    route: Option<&ResolvedModel>,
    weight: i64,
    cost: u32,
) -> obleth_fairshare::AdmitRequest {
    let positive = |c: Option<i64>| c.and_then(|c| usize::try_from(c).ok()).filter(|c| *c > 0);
    obleth_fairshare::AdmitRequest {
        tenant: resolved.tenant_id,
        key: resolved.key_id,
        weight,
        key_weight: resolved.key_weight.max(1),
        group: resolved.fairshare_group.clone(),
        group_weight: resolved.group_weight,
        model: model.to_string(),
        model_max_in_flight: route.and_then(|r| r.max_in_flight).filter(|c| *c > 0),
        tenant_max_in_flight: positive(resolved.max_in_flight),
        key_max_in_flight: positive(resolved.key_max_in_flight),
        cost,
    }
}

/// OpenAI-style endpoints that must resolve to a registered model route.
/// Unregistered models must not fall through to the default benchmark fixture upstream.
/// Evaluate a tenant's schedule against the current instant. Returns `Ok(())`
/// when traffic is permitted, or `Err(reason)` with a client-facing message when
/// the tenant is outside its activation window, expired, or outside its
/// recurring weekly windows.
pub(crate) fn tenant_active_now(
    resolved: &ResolvedKey,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<(), &'static str> {
    if let Some(from) = resolved.active_from {
        if now < from {
            return Err("tenant is not active yet");
        }
    }
    if let Some(until) = resolved.active_until {
        if now >= until {
            return Err("tenant access has expired");
        }
    }
    if let Some(windows) = resolved.weekly_windows.as_ref().filter(|w| !w.is_empty()) {
        // Evaluate the recurring windows in the tenant's local timezone. An
        // unparseable timezone falls back to UTC rather than blocking traffic.
        let tz: chrono_tz::Tz = resolved.timezone.parse().unwrap_or(chrono_tz::UTC);
        let local = now.with_timezone(&tz);
        use chrono::{Datelike, Timelike};
        let day = local.weekday().num_days_from_sunday() as u8; // 0=Sunday
        let minute_of_day = (local.hour() * 60 + local.minute()) as u16;
        let in_window = windows
            .iter()
            .any(|w| w.day == day && minute_of_day >= w.start_min && minute_of_day < w.end_min);
        if !in_window {
            return Err("tenant is outside its scheduled access window");
        }
    }
    Ok(())
}

/// Compute the term-usage period key for a tenant, or `None` when no cumulative
/// budget cap is configured. The key namespaces the Redis counters so that
/// `monthly` budgets roll over at each calendar month (in the tenant timezone),
/// `term` budgets reset whenever `budget_started_at` changes, and `lifetime`
/// budgets never reset.
pub(crate) fn term_period_key(
    resolved: &ResolvedKey,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    budget_period_key(
        resolved.budget_tokens,
        resolved.budget_cost_usd,
        resolved.budget_period.as_deref(),
        resolved.budget_started_at,
        &resolved.timezone,
        now,
    )
}

pub(crate) fn key_term_period_key(
    resolved: &ResolvedKey,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    budget_period_key(
        resolved.key_budget_tokens,
        resolved.key_budget_cost_usd,
        resolved.key_budget_period.as_deref(),
        resolved.key_budget_started_at,
        &resolved.timezone,
        now,
    )
}

fn budget_period_key(
    budget_tokens: Option<i64>,
    budget_cost_usd: Option<f64>,
    budget_period: Option<&str>,
    budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    timezone: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<String> {
    if budget_tokens.is_none() && budget_cost_usd.is_none() {
        return None;
    }
    let period = budget_period.unwrap_or("lifetime");
    let key = match period {
        "monthly" => {
            use chrono::Datelike;
            let tz: chrono_tz::Tz = timezone.parse().unwrap_or(chrono_tz::UTC);
            let local = now.with_timezone(&tz);
            format!("m:{}-{:02}", local.year(), local.month())
        }
        "term" => {
            let anchor = budget_started_at.map(|t| t.timestamp()).unwrap_or(0);
            format!("t:{anchor}")
        }
        // "lifetime" and any unknown value: a single non-rolling bucket.
        _ => {
            let anchor = budget_started_at.map(|t| t.timestamp()).unwrap_or(0);
            format!("l:{anchor}")
        }
    };
    Some(key)
}

/// Emit warning/exhaustion alerts when a tenant crosses 80% / 100% of either
/// cumulative budget cap. Cooldown dedup lives in `SlackAlerts`.
fn maybe_alert_budget(state: &AppState, resolved: &ResolvedKey, used_tokens: i64, used_cost: f64) {
    let token_pct = resolved
        .budget_tokens
        .filter(|c| *c > 0)
        .map(|cap| used_tokens as f64 / cap as f64);
    let cost_pct = resolved
        .budget_cost_usd
        .filter(|c| *c > 0.0)
        .map(|cap| used_cost / cap);
    let pct = [token_pct, cost_pct]
        .into_iter()
        .flatten()
        .fold(0.0_f64, f64::max);
    if pct >= 1.0 {
        state.alerts.issue(
            format!("term_budget_exhausted:{}", resolved.tenant_id),
            "Tenant term budget exhausted",
            format!(
                "tenant `{}` reached its budget cap (used {used_tokens} tokens / ${used_cost:.4})",
                resolved.tenant_name
            ),
        );
    } else if pct >= 0.8 {
        state.alerts.issue(
            format!("term_budget_warn:{}", resolved.tenant_id),
            "Tenant term budget at 80%",
            format!(
                "tenant `{}` is at {:.0}% of its budget (used {used_tokens} tokens / ${used_cost:.4})",
                resolved.tenant_name,
                pct * 100.0
            ),
        );
    }
}

fn maybe_alert_key_budget(
    state: &AppState,
    resolved: &ResolvedKey,
    used_tokens: i64,
    used_cost: f64,
) {
    let token_pct = resolved
        .key_budget_tokens
        .filter(|c| *c > 0)
        .map(|cap| used_tokens as f64 / cap as f64);
    let cost_pct = resolved
        .key_budget_cost_usd
        .filter(|c| *c > 0.0)
        .map(|cap| used_cost / cap);
    let pct = [token_pct, cost_pct]
        .into_iter()
        .flatten()
        .fold(0.0_f64, f64::max);
    if pct >= 1.0 {
        state.alerts.issue(
            format!("key_term_budget_exhausted:{}", resolved.key_id),
            "API key term budget exhausted",
            format!(
                "tenant `{}` key `{}` reached its budget cap (used {used_tokens} tokens / ${used_cost:.4})",
                resolved.tenant_name, resolved.key_id
            ),
        );
    } else if pct >= 0.8 {
        state.alerts.issue(
            format!("key_term_budget_warn:{}", resolved.key_id),
            "API key term budget at 80%",
            format!(
                "tenant `{}` key `{}` is at {:.0}% of its budget (used {used_tokens} tokens / ${used_cost:.4})",
                resolved.tenant_name,
                resolved.key_id,
                pct * 100.0
            ),
        );
    }
}

/// Serve `GET /v1/models` from the gateway's own registry: one entry per
/// enabled route, under its client-facing `model_name`, carrying the same
/// fields as the detail lookup.
///
/// The registry is the only honest source for this answer. The listing used to
/// be the union of every upstream's own `/v1/models`, which listed nothing for
/// a route whose backend does not serve a catalog (or was unreachable that
/// second), and advertised whatever else a backend happened to serve, under
/// its backend id, though a request naming a model the gateway has not
/// registered is refused. Provisioned models are registry rows too, so nothing
/// the union found is lost.
///
/// Tenants with a model allowlist see only the models they may call, as on
/// `/model/info`: listing a model the caller would be refused advertises a 403.
fn models_list_response(state: &AppState, resolved: &ResolvedKey) -> Response<Body> {
    let candidates = state.model_registry.load();
    (
        StatusCode::OK,
        axum::Json(registry_models_list(
            &candidates,
            allowed_models_for(resolved),
        )),
    )
        .into_response()
}

/// The model allowlist that bounds what a caller is shown, or `None` when it
/// may see everything (internal callers, and tenants without an allowlist).
/// Entries are canonical `model_name`s, which is also what the discovery
/// endpoints advertise.
fn allowed_models_for(resolved: &ResolvedKey) -> Option<&[String]> {
    if resolved.internal {
        None
    } else {
        resolved.allowed_models.as_deref()
    }
}

/// True when `model_name` is visible under `allowed` (see [`allowed_models_for`]).
fn model_visible(allowed: Option<&[String]>, model_name: &str) -> bool {
    allowed.is_none_or(|list| list.iter().any(|m| m == model_name))
}

/// The OpenAI `{object:"list", data:[…]}` listing for a registry snapshot,
/// sorted by id and limited to `allowed` when set. Pure, so the shape is
/// unit-testable without a registry.
fn registry_models_list(
    candidates: &[obleth_config::routing::Candidate],
    allowed: Option<&[String]>,
) -> serde_json::Value {
    let mut data: Vec<serde_json::Value> = candidates
        .iter()
        .filter(|c| c.model.enabled)
        .filter(|c| model_visible(allowed, &c.model.model_name))
        .map(|c| model_entry(&ModelFacts::of(&c.model)))
        .collect();
    data.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    data.dedup_by(|a, b| a["id"] == b["id"]);
    serde_json::json!({ "object": "list", "data": data })
}

/// One discovery entry (`/v1/models` and `/v1/models/{id}`).
fn model_entry(facts: &ModelFacts) -> serde_json::Value {
    serde_json::json!({
        "id": facts.model_name,
        "object": "model",
        "owned_by": "obleth",
        "model_type": facts.model_type,
        "mode": mode_for_model_type(&facts.model_type),
        "quantization": facts.quantization,
        "tags": facts.tags,
        "aliases": facts.aliases,
    })
}

/// What the gateway knows about one registered route, as the discovery
/// endpoints report it.
#[derive(Debug, Clone, PartialEq)]
struct ModelFacts {
    /// The one name the gateway advertises for this route.
    model_name: String,
    model_type: String,
    quantization: String,
    aliases: Vec<String>,
    tags: Vec<String>,
}

impl ModelFacts {
    fn of(model: &ResolvedModel) -> Self {
        ModelFacts {
            model_name: model.model_name.clone(),
            model_type: model.model_type.clone(),
            quantization: model.quantization.clone(),
            aliases: model.aliases.clone(),
            tags: model.tags.clone(),
        }
    }
}

/// Index every name a registered route can be recognized by — its
/// `model_name`, its aliases, and the `upstream_model` its backend reports —
/// onto the facts the gateway advertises for it.
///
/// The `upstream_model` key lets a detail lookup by a backend's own id (a
/// client that learned `glm-5-3-mxfp4` from the backend) answer with the
/// registered `glm-5-3`. Insertion order sets precedence deliberately —
/// `model_name` is written last and wins, because it is the name request
/// resolution matches on.
fn model_facts_index(
    candidates: &[obleth_config::routing::Candidate],
) -> std::collections::HashMap<String, std::sync::Arc<ModelFacts>> {
    let mut by_name: std::collections::HashMap<String, std::sync::Arc<ModelFacts>> =
        std::collections::HashMap::new();
    let facts: Vec<std::sync::Arc<ModelFacts>> = candidates
        .iter()
        .map(|c| std::sync::Arc::new(ModelFacts::of(&c.model)))
        .collect();
    for (c, f) in candidates.iter().zip(facts.iter()) {
        if !c.model.upstream_model.is_empty() {
            by_name
                .entry(c.model.upstream_model.clone())
                .or_insert_with(|| f.clone());
        }
    }
    for (c, f) in candidates.iter().zip(facts.iter()) {
        for alias in &c.model.aliases {
            by_name.entry(alias.clone()).or_insert_with(|| f.clone());
        }
    }
    for f in facts {
        by_name.insert(f.model_name.clone(), f);
    }
    by_name
}

/// The LiteLLM-convention spelling of a modality, which many OpenAI-compatible
/// clients read instead of obleth's own `model_type`. Identical to the obleth
/// vocabulary except that `image` and `video` are spelled `image_generation`
/// and `video_generation`.
fn mode_for_model_type(model_type: &str) -> &str {
    match model_type {
        "image" => "image_generation",
        "video" => "video_generation",
        other => other,
    }
}

/// True for the model-listing collection itself, in either spelling. The
/// trailing slash carries no id, so `/v1/models/` lists rather than being read
/// as a detail lookup for the empty string.
fn is_models_collection(path: &str) -> bool {
    path == "/v1/models" || path == "/v1/models/"
}

/// One OpenAI model object for a name the gateway has a route for — canonical
/// or alias — identical to that route's entry in the listing. `None` for a
/// name no route claims, which lets the caller fall through to the upstream
/// passthrough.
///
/// `owned_by` is `obleth`: the answer is the gateway's own. `created` is
/// omitted rather than fabricated — the hot-path view of a route does not
/// carry a registration timestamp.
///
/// `Some(Err(()))` when a route claims the name but it is outside `allowed`:
/// the caller answers that like an unknown model, without forwarding the id,
/// so the detail lookup reveals no more than the filtered listing does.
fn registered_model_entry(
    state: &AppState,
    id: &str,
    allowed: Option<&[String]>,
) -> Option<Result<serde_json::Value, ()>> {
    if id.is_empty() {
        return None;
    }
    let candidates = state.model_registry.load();
    let facts = model_facts_index(&candidates).get(id)?.clone();
    if !model_visible(allowed, &facts.model_name) {
        return Some(Err(()));
    }
    Some(Ok(model_entry(&facts)))
}

/// Serve `GET /model/info` from the gateway's own registry.
///
/// Where `/v1/models` answers "which names can I call" in the OpenAI shape,
/// this answers "what has this gateway been told about each model" — every
/// registered, enabled route with its health, and the configured facts a
/// client cannot infer from a name: the serving format, the routing tags, the context
/// window, the per-token prices, the capability flags, the aliases that still
/// resolve here.
///
/// The envelope keeps the *shape* of LiteLLM's `/model/info`, so a client
/// written against that proxy needs no new parsing: `{"data": [{model_name,
/// obleth_params, model_info}]}`, with the conventional `model_info` keys
/// (`mode`, `max_input_tokens`, `supports_*`, `*_cost_per_token`) in place.
///
/// The per-route object is `obleth_params`, not `litellm_params`: the shape is
/// borrowed, the contents are this gateway's, and they have diverged — the
/// serving format, the routing tags and the aliases have no LiteLLM
/// equivalent. Naming it after another proxy implied a compatibility that
/// stopped being true.
///
/// Deliberately absent from `obleth_params`: `api_base` and `api_key`. This
/// is a tenant-facing endpoint, and a backend's internal URL is not a client's
/// business — the upstream *name* is reported, the route to it is not.
///
/// Tenants with a model allowlist see only the models they may call. Listing a
/// model a caller would be refused would be advertising a 403.
fn model_info_response(state: &AppState, resolved: &ResolvedKey) -> Response<Body> {
    let candidates = state.model_registry.load();
    let allowed = allowed_models_for(resolved);
    let data: Vec<serde_json::Value> = candidates
        .iter()
        .filter(|c| c.model.enabled)
        .filter(|c| model_visible(allowed, &c.model.model_name))
        .map(|c| model_info_entry(&c.model, c.healthy))
        .collect();
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "data": data })),
    )
        .into_response()
}

/// One `/model/info` entry. Split out so the payload shape is unit-testable
/// without a registry or a request.
fn model_info_entry(model: &ResolvedModel, healthy: bool) -> serde_json::Value {
    serde_json::json!({
        "model_name": model.model_name,
        "obleth_params": {
            // The name the backend serves, which is where a quantization
            // suffix legitimately lives — the client-facing name above stays
            // clean regardless of how this deployment is built.
            "model": model.upstream_model,
        },
        "model_info": {
            // Conventional keys kept under their usual names, so a client
            // written against the LiteLLM shape reads them as-is.
            "mode": mode_for_model_type(&model.model_type),
            "max_input_tokens": model.context_window,
            "input_cost_per_token": model.input_cost_per_token,
            "output_cost_per_token": model.output_cost_per_token,
            "supports_function_calling": model.supports_function_calling,
            "supports_system_messages": model.supports_system_messages,
            "supports_response_schema": model.supports_response_schema,
            "supports_vision": model.supports_vision,
            // obleth's own additions.
            "model_type": model.model_type,
            "quantization": model.quantization,
            "aliases": model.aliases,
            "tags": model.tags,
            "boons": model.boons,
            "supports_tool_choice": model.supports_tool_choice,
            "context_window": model.context_window,
            "cost_per_image": model.cost_per_image,
            "cost_per_audio_second": model.cost_per_audio_second,
            "cost_per_character": model.cost_per_character,
            "cost_per_video": model.cost_per_video,
            // Health as the gateway last observed it. False means the model is
            // registered and addressable but currently failing its probe or
            // held in a maintenance window.
            "healthy": healthy,
        },
    })
}

/// The OpenAI model-discovery endpoints: `GET /v1/models` (list), the
/// `GET /v1/models/{id}` / `{"model": …}` detail probe, and obleth's
/// registry-backed `/model/info`. These are the only paths the gateway serves
/// without resolving to a registered model, so they are exempt from the
/// unmapped-path rejection.
fn is_models_endpoint(path: &str) -> bool {
    path == "/v1/models" || path.starts_with("/v1/models/") || is_model_info_endpoint(path)
}

/// The `/model/info` detail listing, accepted both bare and under `/v1` —
/// LiteLLM serves it at the bare path, and a client that appends the OpenAI
/// `/v1` prefix to every request should not be the one to find that out.
fn is_model_info_endpoint(path: &str) -> bool {
    path == "/model/info" || path == "/v1/model/info"
}

/// OpenAI endpoints that must name a registered model.
const REGISTERED_MODEL_PATHS: &[&str] = &[
    "/v1/chat/completions",
    "/v1/completions",
    "/v1/embeddings",
    "/v1/responses",
    "/v1/audio/transcriptions",
    "/v1/audio/translations",
    "/v1/audio/speech",
    "/v1/images/generations",
    "/v1/images/edits",
    "/v1/images/variations",
    crate::videos::VIDEOS_PATH,
];

fn requires_registered_model(path: &str) -> bool {
    REGISTERED_MODEL_PATHS.contains(&path)
}

/// True when the endpoint takes a file upload, so the OpenAI spec sends it as
/// `multipart/form-data` with the model as a form field rather than JSON:
/// audio transcription/translation, the two image endpoints that take a
/// source image (edits and variations), and the video create, whose optional
/// reference frame is an upload (it accepts JSON too).
fn is_multipart_endpoint(path: &str) -> bool {
    matches!(
        path,
        "/v1/audio/transcriptions"
            | "/v1/audio/translations"
            | "/v1/images/edits"
            | "/v1/images/variations"
            | crate::videos::VIDEOS_PATH
    )
}

/// A single parsed `multipart/form-data` field, held in memory so it can be
/// rebuilt into an upstream form after model resolution.
struct MultipartField {
    name: String,
    file_name: Option<String>,
    content_type: Option<String>,
    data: Bytes,
}

/// Parse an in-memory multipart body into its fields. The whole body is already
/// buffered (bounded by `BODY_LIMIT`), so this just re-reads it through `multer`.
async fn parse_multipart(
    body: &Bytes,
    boundary: &str,
) -> Result<Vec<MultipartField>, multer::Error> {
    let bytes = body.clone();
    let stream =
        futures_util::stream::once(async move { Ok::<Bytes, std::convert::Infallible>(bytes) });
    let mut multipart = multer::Multipart::new(stream, boundary.to_string());
    let mut fields = Vec::new();
    while let Some(field) = multipart.next_field().await? {
        let name = field.name().unwrap_or("").to_string();
        let file_name = field.file_name().map(|s| s.to_string());
        let content_type = field.content_type().map(|m| m.to_string());
        let data = field.bytes().await?;
        fields.push(MultipartField {
            name,
            file_name,
            content_type,
            data,
        });
    }
    Ok(fields)
}

/// The form's text fields as a JSON object, so the stages that read the
/// request body (the token estimate, the input guardrails, the per-image
/// cost) see a multipart request the way they see a JSON one. File parts are
/// left out. Values stay strings, as the form sent them; a name that repeats
/// becomes an array, so every occurrence is scanned.
fn multipart_text_view(fields: &[MultipartField]) -> serde_json::Value {
    let mut obj = serde_json::Map::new();
    for f in fields.iter().filter(|f| f.file_name.is_none()) {
        let text = serde_json::Value::String(String::from_utf8_lossy(&f.data).into_owned());
        match obj.get_mut(&f.name) {
            None => {
                obj.insert(f.name.clone(), text);
            }
            Some(serde_json::Value::Array(items)) => items.push(text),
            Some(first) => *first = serde_json::Value::Array(vec![first.take(), text]),
        }
    }
    serde_json::Value::Object(obj)
}

/// Copy text fields rewritten in the JSON view (a redacting input policy)
/// back into the form, so what is forwarded is what was scanned. The inverse
/// of [`multipart_text_view`]: a repeated name maps onto its array by position.
fn apply_multipart_text_view(fields: &mut [MultipartField], view: &serde_json::Value) {
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for f in fields.iter_mut().filter(|f| f.file_name.is_none()) {
        let index = seen.entry(f.name.clone()).or_insert(0);
        let value = match view.get(&f.name) {
            Some(serde_json::Value::Array(items)) => items.get(*index),
            other if *index == 0 => other,
            _ => None,
        };
        *index += 1;
        if let Some(text) = value.and_then(|v| v.as_str()) {
            if text.as_bytes() != f.data.as_ref() {
                f.data = Bytes::from(text.to_string());
            }
        }
    }
}

/// Rebuild a reqwest multipart form from parsed fields, replacing the client
/// `model` value with the upstream model name. File parts preserve their
/// filename and content-type.
fn build_multipart_form(
    fields: Vec<MultipartField>,
    upstream_model: &str,
) -> reqwest::multipart::Form {
    let mut form = reqwest::multipart::Form::new();
    let mut had_model = false;
    for f in fields {
        if f.name == "model" {
            had_model = true;
            form = form.text("model", upstream_model.to_string());
            continue;
        }
        if f.file_name.is_some() {
            let file_name = f.file_name.unwrap_or_default();
            let data = f.data.to_vec();
            let part = match f.content_type {
                // content_type came from a parsed Mime, so mime_str won't fail.
                Some(ct) => reqwest::multipart::Part::bytes(data)
                    .file_name(file_name)
                    .mime_str(&ct)
                    .unwrap_or_else(|_| reqwest::multipart::Part::text("")),
                None => reqwest::multipart::Part::bytes(data).file_name(file_name),
            };
            form = form.part(f.name, part);
        } else {
            form = form.text(f.name, String::from_utf8_lossy(&f.data).into_owned());
        }
    }
    if !had_model {
        form = form.text("model", upstream_model.to_string());
    }
    form
}

/// Per-request surcharge for non-token-billed modalities: image generations are
/// billed per image, text-to-speech per input character, and a video job at a
/// flat price per created job. Returns `0.0` for token-billed modalities
/// (chat, embeddings) and audio transcription.
fn compute_modality_cost(route: Option<&ResolvedModel>, json: &serde_json::Value) -> f64 {
    let Some(route) = route else {
        return 0.0;
    };
    match route.model_type.as_str() {
        "image" => {
            // `n` is a number in a JSON body and a string in a multipart form
            // (edits and variations).
            let n = json
                .get("n")
                .and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
                })
                .unwrap_or(1)
                .max(1);
            n as f64 * route.cost_per_image
        }
        "audio_speech" => {
            let chars = json
                .get("input")
                .and_then(|v| v.as_str())
                .map(|s| s.chars().count())
                .unwrap_or(0);
            chars as f64 * route.cost_per_character
        }
        // Frozen at the create, the only video call that reaches settlement:
        // the polls and the download are served outside the pipeline.
        "video" => route.cost_per_video,
        _ => 0.0,
    }
}

/// Swap the body's `model` field for the upstream model name, mutating the
/// already-parsed JSON in place (no deep clone) and re-serializing once.
/// Bodies that are not JSON objects (e.g. multipart audio uploads, handled
/// separately) pass through untouched, as does everything when the model has
/// no registered route. The token estimate is unaffected by this rewrite —
/// the tokenizer never reads the `model` field.
///
/// `force_non_streaming` is set when a response-transforming boon is armed:
/// the upstream call is made non-streaming regardless of what the client
/// asked for. This happens here — after the cache key was computed from the
/// client body — so streaming and non-streaming clients keep distinct cache
/// entries holding the representation each actually receives (JSON vs SSE).
///
/// `stream_with_usage` is set for every streaming chat/completions dispatch
/// (including the streaming tool loop's turn 0): the upstream stays streaming
/// but is asked to include a final usage chunk so tokens are billed exactly
/// instead of estimated. Other `stream_options` the client sent are kept.
/// (`tool_stream` and the pass-through strip that chunk unless the client
/// itself asked for usage.) The two flags are mutually exclusive.
fn prepare_upstream_body(
    route: Option<&ResolvedModel>,
    json: &mut serde_json::Value,
    body: Bytes,
    force_non_streaming: bool,
    stream_with_usage: bool,
) -> Bytes {
    let Some(route) = route else {
        return body;
    };
    let Some(obj) = json.as_object_mut() else {
        return body;
    };
    obj.insert(
        "model".into(),
        serde_json::Value::String(route.upstream_model.clone()),
    );
    if force_non_streaming {
        obj.insert("stream".into(), serde_json::Value::Bool(false));
        obj.remove("stream_options");
    } else if stream_with_usage {
        obj.insert("stream".into(), serde_json::Value::Bool(true));
        let options = obj
            .entry("stream_options")
            .or_insert_with(|| serde_json::json!({}));
        if !options.is_object() {
            *options = serde_json::json!({});
        }
        options["include_usage"] = serde_json::Value::Bool(true);
    }
    serde_json::to_vec(&*json).map(Bytes::from).unwrap_or(body)
}

/// One resolved upstream target: a base URL plus an optional bearer key, and
/// the model's operator-configured upstream headers.
pub(crate) struct Target {
    pub(crate) base: String,
    pub(crate) api_key: Option<String>,
    pub(crate) headers: HeaderMap,
}

/// Build the ordered list of upstream targets for a request.
///
/// When the model defines explicit endpoints we route across the ones that are
/// both `enabled` and `healthy`. `failover` orders them by ascending priority
/// (lowest first); `load_balance` orders them by a weighted random shuffle so
/// traffic spreads across clusters in proportion to their weights; `session_hash`
/// pins a session to one endpoint via rendezvous hashing of the session key,
/// with the rest following for failover. When no usable endpoints exist we fall
/// back to the model's own `api_base`/`api_key` (or the global default base),
/// preserving the legacy single-upstream path.
pub(crate) fn build_targets(
    route: Option<&ResolvedModel>,
    default_base: &str,
    selection_mode: &str,
    session_key: &str,
) -> Vec<Target> {
    let mut targets: Vec<Target> = Vec::new();
    let headers = route
        .map(|r| obleth_admin::upstream_header_map(&r.upstream_headers))
        .unwrap_or_default();
    if let Some(r) = route {
        let mut eligible: Vec<&ResolvedEndpoint> = r
            .endpoints
            .iter()
            .filter(|e| e.enabled && e.healthy)
            .collect();
        if !eligible.is_empty() {
            match selection_mode {
                "load_balance" => eligible = weighted_order(eligible),
                "session_hash" => eligible = session_hash_order(eligible, session_key),
                _ => eligible.sort_by_key(|e| e.priority), // failover (default)
            }
            for e in eligible {
                targets.push(Target {
                    base: e.api_base.clone(),
                    api_key: e.api_key.clone().or_else(|| r.api_key.clone()),
                    headers: headers.clone(),
                });
            }
            return targets;
        }
    }
    targets.push(Target {
        base: route
            .map(|r| r.api_base.clone())
            .unwrap_or_else(|| default_base.to_string()),
        api_key: route.and_then(|r| r.api_key.clone()),
        headers,
    });
    targets
}

/// Order endpoints by weighted random sampling (A-Res): each endpoint gets a
/// key `u^(1/weight)` for a uniform random `u`, and we sort by descending key.
/// Higher-weight endpoints land earlier more often, so the first eligible
/// target is chosen in proportion to weight while the rest stay available for
/// failover.
fn weighted_order(items: Vec<&ResolvedEndpoint>) -> Vec<&ResolvedEndpoint> {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15);
    let mut next = move || {
        seed = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = seed;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    };
    let mut keyed: Vec<(f64, &ResolvedEndpoint)> = items
        .into_iter()
        .map(|e| {
            let w = e.weight.max(1) as f64;
            let u = (next() as f64 / u64::MAX as f64).max(1e-12);
            (u.powf(1.0 / w), e)
        })
        .collect();
    keyed.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    keyed.into_iter().map(|(_, e)| e).collect()
}

/// FNV-1a over `session_key\0endpoint_id` — a small, dependency-free, stable
/// hash. Stability matters: the same key must score an endpoint identically on
/// every request so a session keeps landing on the same replica.
fn rendezvous_score(session_key: &str, endpoint_id: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in session_key
        .bytes()
        .chain(std::iter::once(0u8))
        .chain(endpoint_id.bytes())
    {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Order endpoints so a session sticks to one replica: rendezvous (highest-
/// random-weight) hashing scores each endpoint by `hash(session_key, id)`; the
/// highest score is the session's home and goes first, the rest follow for
/// failover. Deterministic for a given key; adding/removing an endpoint
/// reshuffles only minimally. With no session key, falls back to weighted order.
fn session_hash_order<'a>(
    items: Vec<&'a ResolvedEndpoint>,
    session_key: &str,
) -> Vec<&'a ResolvedEndpoint> {
    if session_key.is_empty() {
        // No session key on the request, so stickiness is impossible: this falls
        // back to weighted_order, i.e. session_hash behaves like load_balance.
        // Logged (debug, not warn — keyless requests are common and expected) so
        // operators can see why a session_hash model isn't actually sticking.
        tracing::debug!(
            "session_hash selected but request has no session key; falling back to weighted order"
        );
        return weighted_order(items);
    }
    let mut scored: Vec<(u64, &ResolvedEndpoint)> = items
        .into_iter()
        .map(|e| (rendezvous_score(session_key, &e.id), e))
        .collect();
    // Highest score first; tie-break on id for a fully deterministic order.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.id.cmp(&b.1.id)));
    scored.into_iter().map(|(_, e)| e).collect()
}

/// Whether an upstream HTTP status is worth retrying or failing over for.
/// Transient transport/overload conditions only — never 4xx client errors
/// (except 408 request-timeout and 429 too-many-requests).
fn is_retryable_status(status: u16) -> bool {
    matches!(status, 408 | 429 | 500 | 502 | 503 | 504)
}

/// Whether a reqwest send error means the request never reached the upstream, so
/// a fresh-connection retry is safe. Covers connection establishment failures and
/// the "reused a keep-alive socket the server already closed" race — hyper
/// surfaces the latter as an incomplete/closed/reset error in the source chain
/// rather than via `is_connect()`, so we also scan the error chain for the
/// well-known signatures.
fn is_connection_error(e: &reqwest::Error) -> bool {
    use std::error::Error;
    if e.is_connect() || e.is_request() {
        return true;
    }
    let mut source: Option<&dyn Error> = Some(e);
    while let Some(err) = source {
        let msg = err.to_string().to_ascii_lowercase();
        if msg.contains("connection closed")
            || msg.contains("connection reset")
            || msg.contains("connection aborted")
            || msg.contains("broken pipe")
            || msg.contains("channel closed")
            || msg.contains("unexpected end of file")
            || msg.contains("incompletemessage")
        {
            return true;
        }
        source = err.source();
    }
    false
}

/// Exponential backoff for retry `attempt` (0-based), capped to avoid overflow.
fn backoff_for(base: Duration, attempt: u32) -> Duration {
    if base.is_zero() {
        return base;
    }
    base.saturating_mul(1u32 << attempt.min(6))
}

/// Reject client request paths that could escape the configured upstream base.
///
/// The upstream host is fixed by the operator-registered `api_base` (and is
/// SSRF-screened at registration), but the client-supplied path is appended to
/// it. A `..` segment — literal or percent-encoded — could walk above the
/// intended API prefix and reach a different path on that host, so we refuse it
/// outright. Legitimate OpenAI-compatible paths never contain `..`, encoded
/// dots, or encoded separators.
pub(crate) fn has_path_traversal(path: &str) -> bool {
    let lowered = path.to_ascii_lowercase();
    // Encoded dot (`%2e`) or separators (`%2f` `/`, `%5c` `\`) can reconstruct a
    // traversal after the upstream decodes them; none are valid here.
    if lowered.contains("%2e") || lowered.contains("%2f") || lowered.contains("%5c") {
        return true;
    }
    path.split(['/', '\\']).any(|seg| seg == "..")
}

pub(crate) fn build_upstream_url(base: &str, path: &str, query: &str) -> String {
    let base = base.trim_end_matches('/');
    let rel_raw = path.trim_start_matches('/');
    // Defensive: operators sometimes paste the full endpoint URL as api_base
    // (e.g. ".../v1/embeddings" or ".../v1/audio/speech") instead of the base
    // (".../v1"). If the configured base already ends with the request path,
    // don't append it again — that would produce ".../v1/embeddings/v1/embeddings".
    if !rel_raw.is_empty() && (base.ends_with(rel_raw) || base.ends_with(&format!("/{rel_raw}"))) {
        return format!("{base}{query}");
    }
    let mut rel = rel_raw.to_string();
    // api_base is typically `…/v1` while clients call `/v1/chat/completions` — avoid `/v1/v1/…`
    if base.ends_with("/v1") && rel.starts_with("v1/") {
        rel = rel.trim_start_matches("v1/").to_string();
    }
    if rel.is_empty() {
        format!("{base}{query}")
    } else {
        format!("{base}/{rel}{query}")
    }
}

pub(crate) fn bearer(headers: &HeaderMap) -> Option<String> {
    if let Some(v) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(rest) = v.strip_prefix("Bearer ") {
            return Some(rest.trim().to_string());
        }
    }
    headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
}

pub(crate) fn forward_headers(headers: &HeaderMap) -> HeaderMap {
    let mut out = HeaderMap::new();
    for (name, value) in headers {
        match name.as_str() {
            // strip hop-by-hop / auth / encoding so the body stays inspectable;
            // x-obleth-boons is a gateway directive, not an upstream header
            "host" | "content-length" | "authorization" | "x-api-key" | "accept-encoding"
            | "connection" | "x-obleth-boons" => continue,
            _ => {
                out.insert(name.clone(), value.clone());
            }
        }
    }
    out
}

/// Append a chunk to the rolling response tail, keeping only the last
/// `TAIL_CAP` bytes. Works on raw bytes with `copy_within` so a long stream
/// never re-allocates per chunk; the one UTF-8 conversion happens at stream
/// end (a partial char at the buffer start is replaced lossily there, which is
/// harmless — `extract_usage` only scans for ASCII JSON keys).
fn append_tail(tail: &mut Vec<u8>, chunk: &[u8]) {
    if chunk.len() >= TAIL_CAP {
        tail.clear();
        tail.extend_from_slice(&chunk[chunk.len() - TAIL_CAP..]);
        return;
    }
    let overflow = (tail.len() + chunk.len()).saturating_sub(TAIL_CAP);
    if overflow > 0 {
        tail.copy_within(overflow.., 0);
        tail.truncate(tail.len() - overflow);
    }
    tail.extend_from_slice(chunk);
}

/// Pull `prompt_tokens` / `completion_tokens` out of the (possibly streamed)
/// response tail. Returns `None` if the upstream didn't report usage.
///
/// Embedding responses report `prompt_tokens` (and `total_tokens`) but no
/// `completion_tokens`; those are treated as input-only usage.
fn extract_usage(tail: &str) -> Option<(u32, u32)> {
    let input = find_int_after(tail, "\"prompt_tokens\"");
    let output = find_int_after(tail, "\"completion_tokens\"");
    match (input, output) {
        (Some(i), Some(o)) => Some((i, o)),
        // Embeddings and other input-only modalities: count prompt tokens.
        (Some(i), None) => Some((i, 0)),
        _ => None,
    }
}

fn find_int_after(haystack: &str, key: &str) -> Option<u32> {
    let idx = haystack.rfind(key)? + key.len();
    let rest = &haystack[idx..];
    let digits: String = rest
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok()
}

/// Per-request metadata captured once at the top of the handler and shared by
/// every `finalize` path, so the request-log columns are identical regardless
/// of where the request terminates (cache hit, rejection, upstream error, or
/// streamed success). Cheap to clone for the response-stream closure.
#[derive(Clone)]
pub(crate) struct RequestMeta {
    /// Conversation grouping id (client-supplied or derived), or empty.
    pub(crate) session_id: String,
    /// How `session_id` was obtained: "client" | "derived" | "none".
    pub(crate) session_id_source: &'static str,
    /// Coarse request class derived from the request path, except synthetic
    /// tenants' requests are stamped `benchmark` instead (see
    /// [`effective_request_type`]).
    pub(crate) request_type: &'static str,
    /// Device id from the bearer token (identity-key requests), else empty.
    pub(crate) device_id: String,
}

/// Classify a request by its OpenAI-style path suffix. Matching the suffix (not
/// the full path) keeps this robust to version prefixes (`/v1/...`) or any
/// future routing prefix.
fn request_type_for_path(path: &str) -> &'static str {
    if path.ends_with("/chat/completions") {
        "chat"
    } else if path.ends_with("/responses") {
        "responses"
    } else if path.ends_with("/verdicts") {
        "verdict"
    } else if path.ends_with("/completions") {
        "completion"
    } else if path.ends_with("/embeddings") {
        "embedding"
    } else if path.contains("/audio/") {
        "audio"
    } else if path.contains("/images/") {
        "image"
    } else if path.ends_with("/videos") {
        "video"
    } else if path.ends_with("/rerank") || path.ends_with("/reranking") {
        "rerank"
    } else if path.ends_with("/moderations") {
        "moderation"
    } else {
        "other"
    }
}

/// The request class recorded in the ledger: synthetic tenants' traffic is
/// tagged `benchmark` (replacing the path-derived class, mirroring how health
/// probes record `health_probe`), everything else classifies by path.
///
/// This is an ACCOUNTING label. It must never gate behavior — see
/// [`is_chat_path`].
fn effective_request_type(resolved: &ResolvedKey, path: &str) -> &'static str {
    if resolved.synthetic {
        obleth_config::BENCHMARK_REQUEST_TYPE
    } else {
        request_type_for_path(path)
    }
}

/// [`effective_request_type`], except a request either shim translated is
/// recorded by its own surface (`responses`, `messages`). Both run down the
/// chat path by design, so the path alone would report the caller's surface
/// as chat and make adoption of the new APIs invisible in the ledger.
pub(crate) fn surfaced_request_type(
    resolved: &ResolvedKey,
    path: &str,
    headers: &HeaderMap,
) -> &'static str {
    if !resolved.synthetic {
        match surface(headers) {
            Some("responses") => return "responses",
            Some(crate::messages::SURFACE) => return "messages",
            _ => {}
        }
    }
    effective_request_type(resolved, path)
}

/// The API surface a shim translated this request from, if any. The header
/// is set by the shims only: `proxy_handler` strips it from every incoming
/// request before dispatch.
fn surface(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(crate::responses::SURFACE_HEADER)
        .and_then(|v| v.to_str().ok())
}

/// Whether chat-only boons (knowledge, compression, tools, structured output,
/// the MCP tool loop) apply to this request.
///
/// Derived from the path, never from the ledger's `request_type`: that label is
/// overwritten with `benchmark` for every synthetic tenant, so reading it here
/// disabled every chat-only boon for precisely the two callers that exist to
/// exercise them — the Charo console/playground and the benchmark suite, both of
/// which call the data plane as the reserved (synthetic) control-plane tenant.
fn is_chat_path(path: &str) -> bool {
    request_type_for_path(path) == "chat"
}

/// A block/redact output policy scans chat-completion message content only
/// (which `/v1/responses` and `/v1/messages` are translated into). Other
/// endpoints that return model-generated text (legacy completions,
/// transcription and translation, and unrecognized paths forwarded as-is)
/// would reach the client unscanned, so under such a policy they are refused.
/// Endpoints that return no text (embeddings, images, speech, rerank scores,
/// moderation flags) are unaffected.
fn output_guardrails_unenforceable(key: &ResolvedKey, path: &str) -> bool {
    let text_output = match request_type_for_path(path) {
        "completion" | "other" | "responses" => true,
        "audio" => path.ends_with("/transcriptions") || path.ends_with("/translations"),
        _ => false,
    };
    text_output
        && !key.internal
        && key.guardrails_policy.as_ref().is_some_and(|p| {
            !p.output_scanners.is_empty()
                && !matches!(p.action, obleth_config::GuardrailsAction::LogOnly)
        })
}

/// Paths whose request bodies the input scanner cannot read: a Responses-shaped
/// path that did not reach the translation shim (after canonicalization only
/// an odd spelling or prefix can), and unrecognized paths forwarded as-is.
/// Under an enforcing (block/redact) input policy these are refused rather
/// than forwarded unscanned. `log_only` policies observe and never refuse.
fn input_guardrails_unscannable(key: &ResolvedKey, path: &str) -> bool {
    matches!(request_type_for_path(path), "responses" | "other")
        && !key.internal
        && key.guardrails_policy.as_ref().is_some_and(|p| {
            !p.input_scanners.is_empty()
                && !matches!(p.action, obleth_config::GuardrailsAction::LogOnly)
        })
}

/// A non-internal key whose tenant has input scanners configured.
fn has_input_guardrails(key: &ResolvedKey) -> bool {
    !key.internal
        && key
            .guardrails_policy
            .as_ref()
            .is_some_and(|p| !p.input_scanners.is_empty())
}

/// Endpoints whose streams honour `stream_options.include_usage`.
fn is_stream_usage_path(path: &str) -> bool {
    matches!(request_type_for_path(path), "chat" | "completion")
}

/// Provenance of a resolved conversation id.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SessionSource {
    Client,
    Derived,
    None,
}

impl SessionSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            SessionSource::Client => "client",
            SessionSource::Derived => "derived",
            SessionSource::None => "none",
        }
    }
}

/// A resolved conversation grouping key plus how it was obtained.
pub(crate) struct Conversation {
    pub(crate) value: String,
    pub(crate) source: SessionSource,
}

/// Resolve a conversation id. Precedence: explicit client signal (header or
/// body) > deterministic hash of the conversation seed > none. Total: never
/// errors. The OpenAI `user` field is intentionally NOT a session source (it
/// identifies an end-user, not a conversation).
pub(crate) fn resolve_conversation(
    headers: &HeaderMap,
    json: &serde_json::Value,
    tenant_id: Uuid,
    derivation_enabled: bool,
) -> Conversation {
    const MAX: usize = 200;
    let capped = |s: &str| -> String { s.trim().chars().take(MAX).collect() };

    // 1. Explicit client id: header wins, then body conventions.
    if let Some(s) = headers
        .get("x-obleth-session-id")
        .or_else(|| headers.get("x-session-id"))
        .and_then(|v| v.to_str().ok())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Conversation {
            value: capped(s),
            source: SessionSource::Client,
        };
    }
    let body_ids = [
        json.get("session_id").and_then(|v| v.as_str()),
        json.get("metadata")
            .and_then(|m| m.get("session_id"))
            .and_then(|v| v.as_str()),
    ];
    for c in body_ids.into_iter().flatten() {
        if !c.trim().is_empty() {
            return Conversation {
                value: capped(c),
                source: SessionSource::Client,
            };
        }
    }

    // 2. Derived: hash the conversation seed (tenant + leading system/developer
    //    + first user message). Stable across turns.
    if derivation_enabled {
        if let Some(seed) = conversation_seed(json) {
            let h = fnv1a_continue(fnv1a(tenant_id.as_bytes()), seed.as_bytes());
            return Conversation {
                value: format!("{h:016x}"),
                source: SessionSource::Derived,
            };
        }
    }

    // 3. Nothing to go on.
    Conversation {
        value: String::new(),
        source: SessionSource::None,
    }
}

/// Maximum seed text hashed; bounds work on huge system prompts. The same
/// leading bytes are replayed every turn, so capping stays stable.
const SEED_CAP: usize = 8 * 1024;

/// Build the stable conversation seed: leading system/developer message text up
/// to and including the first user message. `None` when there are no messages
/// or no text to hash (e.g. embeddings, image-only first turn with no system).
fn conversation_seed(json: &serde_json::Value) -> Option<String> {
    let messages = json.get("messages")?.as_array()?;
    let mut seed = String::new();
    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        match role {
            "system" | "developer" => push_capped(&mut seed, &message_text(msg.get("content"))),
            "user" => {
                push_capped(&mut seed, &message_text(msg.get("content")));
                break; // first user turn closes the seed
            }
            _ => {}
        }
    }
    if seed.trim().is_empty() {
        None
    } else {
        Some(seed)
    }
}

/// Append up to the seed cap, with a separator so adjacent messages can't merge
/// into a colliding blob.
fn push_capped(seed: &mut String, text: &str) {
    if seed.len() >= SEED_CAP || text.is_empty() {
        return;
    }
    if !seed.is_empty() {
        seed.push('\u{1f}'); // unit separator
    }
    let room = SEED_CAP - seed.len();
    if text.len() <= room {
        seed.push_str(text);
    } else {
        let mut end = room;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        seed.push_str(&text[..end]);
    }
}

/// FNV-1a 64-bit (same family as `rendezvous_score`): fixed-seed, deterministic
/// across processes. Not for cryptographic use.
fn fnv1a(bytes: &[u8]) -> u64 {
    fnv1a_continue(0xcbf2_9ce4_8422_2325, bytes)
}

/// Continue an FNV-1a 64-bit hash over additional bytes (lets us chain tenant + seed).
fn fnv1a_continue(mut hash: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn finalize(
    state: &AppState,
    request_id: Uuid,
    resolved: &ResolvedKey,
    meta: &RequestMeta,
    model: &str,
    admission: Admission,
    est: CostEstimate,
    input_tokens: u32,
    output_tokens: u32,
    queue_wait_ms: u32,
    ttft_ms: u32,
    total_ms: u32,
    status_code: u16,
    cache_status: &str,
    cost_usd: f64,
    energy: crate::energy::EnergyFigures,
) {
    state
        .metrics
        .record_request(admission.as_str(), status_code, input_tokens, output_tokens);
    if ttft_ms > 0 {
        state.metrics.ttft_ms.observe(ttft_ms as f64);
    }
    if resolved.internal {
        return;
    }
    state.telemetry.record(UsageRecord {
        request_id,
        tenant_id: resolved.tenant_id,
        key_id: resolved.key_id,
        model: model.to_string(),
        admission: admission.as_str().to_string(),
        weight: resolved.weight,
        input_tokens,
        output_tokens,
        estimated_tokens: est.total(),
        queue_wait_ms,
        ttft_ms,
        total_ms,
        status_code,
        cache_status: cache_status.to_string(),
        cost_usd,
        energy_wh: energy.energy_wh,
        energy_cost_usd: energy.energy_cost_usd,
        co2_g: energy.co2_g,
        ts_ms: now_ms(),
        session_id: meta.session_id.clone(),
        session_id_source: meta.session_id_source.to_string(),
        request_type: meta.request_type.to_string(),
        device_id: meta.device_id.clone(),
    });
}

/// Build an HTTP response from a cached entry, replaying the stored body and
/// content-type (works for both JSON and buffered SSE).
fn cached_response(cached: obleth_config::CachedResponse, request_id: Uuid) -> Response<Body> {
    let content_type = header::HeaderValue::from_str(&cached.content_type)
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/json"));
    let status = StatusCode::from_u16(cached.status).unwrap_or(StatusCode::OK);
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .header("x-obleth-cache", "hit")
        .header("x-obleth-request-id", request_id.to_string())
        .body(Body::from(cached.body))
        .unwrap_or_else(|_| {
            error_json(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cache response build failed",
            )
        })
}

/// Post-auth key/tenant gates shared by the passthrough pipeline and native
/// endpoints (e.g. `/v1/verdicts`): key disabled, tenant lifecycle status,
/// the activation/expiry/weekly schedule window, and the near-expiry operator
/// alert. Internal probe keys bypass tenant lifecycle gating.
// The error is the finished response every caller returns as-is
// (`return resp`); boxing it would push a deref into each call site for a
// rejection path that is cold anyway.
#[allow(clippy::result_large_err)]
pub(crate) fn gate_resolved_key(
    state: &AppState,
    resolved: &ResolvedKey,
) -> Result<(), Response<Body>> {
    if resolved.disabled {
        return Err(error_json(StatusCode::FORBIDDEN, "api key disabled"));
    }
    if !resolved.internal && resolved.status != "active" {
        return Err(error_json(StatusCode::FORBIDDEN, "tenant is not active"));
    }
    // Schedule gate: activation start, expiry cutoff, and recurring weekly windows.
    if !resolved.internal {
        let now = chrono::Utc::now();
        if let Err(reason) = tenant_active_now(resolved, now) {
            return Err(error_json(StatusCode::FORBIDDEN, reason));
        }
        // Phase 5: warn operators when a tenant is within 72h of expiry. This
        // runs on every request in that window, so the alert text is only
        // formatted when a channel could deliver it; repeats are then
        // deduplicated by the dispatcher's per-key cooldown.
        if let Some(until) = resolved.active_until.filter(|_| state.alerts.enabled()) {
            let remaining = until - now;
            if remaining > chrono::Duration::zero() && remaining <= chrono::Duration::hours(72) {
                state.alerts.issue(
                    format!("tenant_expiry:{}", resolved.tenant_id),
                    "Tenant access expiring soon",
                    format!(
                        "tenant `{}` expires at {} (~{}h remaining)",
                        resolved.tenant_name,
                        until.to_rfc3339(),
                        remaining.num_hours()
                    ),
                );
            }
        }
    }
    Ok(())
}

pub(crate) fn error_json(status: StatusCode, msg: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": { "message": msg, "type": "obleth_gateway_error" } });
    (status, axum::Json(body)).into_response()
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        admit_request_for, anthropic_error, apply_multipart_text_view,
        backfill_max_tokens_for_count_tokens, backoff_for, build_targets, build_upstream_url,
        canonical_post_path, clamp_max_tokens, compute_modality_cost, effective_request_type,
        forward_headers, has_input_guardrails, has_path_traversal, input_guardrails_unscannable,
        is_chat_path, is_models_collection, is_models_endpoint, is_multipart_endpoint,
        is_retryable_status, looks_like_context_length_error, messages_input_estimate,
        multipart_text_view, output_guardrails_unenforceable, parse_multipart,
        prepare_upstream_body, redress_error, request_type_for_path, requires_registered_model,
        resolve_conversation, session_hash_order, should_translate_as_stream, strip_beta_query,
        surface, tenant_active_now, weighted_order, MultipartField, RequestMeta, TooLong,
        REGISTERED_MODEL_PATHS,
    };
    use crate::router::{BoonGrants, Candidate, Intent, RequestFeatures, RouterWeights};
    use axum::body::{Body, Bytes};
    use axum::http::{header, HeaderMap, Response, StatusCode};
    use chrono::{DateTime, TimeZone, Utc};
    use obleth_config::{ResolvedEndpoint, ResolvedKey, ResolvedModel, WeeklyWindow};
    use std::collections::HashMap;
    use std::time::Duration;
    use uuid::Uuid;

    fn endpoint(
        id: &str,
        base: &str,
        priority: i64,
        weight: i64,
        enabled: bool,
        healthy: bool,
    ) -> ResolvedEndpoint {
        ResolvedEndpoint {
            id: id.into(),
            api_base: base.into(),
            api_key: None,
            priority,
            weight,
            enabled,
            healthy,
            max_in_flight: None,
        }
    }

    fn key_with_schedule(
        timezone: &str,
        active_from: Option<DateTime<Utc>>,
        active_until: Option<DateTime<Utc>>,
        weekly_windows: Option<Vec<WeeklyWindow>>,
    ) -> ResolvedKey {
        ResolvedKey {
            key_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            tenant_name: "t".into(),
            fairshare_group: "default".into(),
            group_weight: 100,
            weight: 100,
            tokens_per_minute: 1000,
            max_in_flight: None,
            disabled: false,
            status: "active".into(),
            timezone: timezone.into(),
            active_from,
            active_until,
            weekly_windows,
            budget_tokens: None,
            budget_cost_usd: None,
            budget_period: None,
            budget_started_at: None,
            key_budget_tokens: None,
            key_budget_cost_usd: None,
            key_budget_period: None,
            key_budget_started_at: None,
            key_weight: 100,
            key_max_in_flight: None,
            allowed_models: None,
            internal: false,
            tracing_enabled: false,
            guardrails_policy: None,
            compression_policy: None,
            synthetic: false,
        }
    }

    #[test]
    fn legacy_completions_are_refused_only_under_an_enforcing_output_policy() {
        let policy = |action, output: &[&str]| obleth_config::GuardrailsPolicy {
            action,
            input_scanners: vec![],
            output_scanners: output.iter().map(|s| s.to_string()).collect(),
            guard_model: None,
            ban_keywords: vec![],
            fail_open: true,
        };
        let mut key = key_with_schedule("UTC", None, None, None);
        assert!(!output_guardrails_unenforceable(&key, "/v1/completions"));

        key.guardrails_policy = Some(policy(obleth_config::GuardrailsAction::Block, &["pii"]));
        assert!(output_guardrails_unenforceable(&key, "/v1/completions"));
        assert!(!output_guardrails_unenforceable(
            &key,
            "/v1/chat/completions"
        ));

        key.guardrails_policy = Some(policy(obleth_config::GuardrailsAction::Redact, &["pii"]));
        assert!(output_guardrails_unenforceable(&key, "/v1/completions"));

        // log_only is observed after the stream, never enforced: not refused.
        key.guardrails_policy = Some(policy(obleth_config::GuardrailsAction::LogOnly, &["pii"]));
        assert!(!output_guardrails_unenforceable(&key, "/v1/completions"));

        // No output scanners: nothing to enforce on the response.
        key.guardrails_policy = Some(policy(obleth_config::GuardrailsAction::Block, &[]));
        assert!(!output_guardrails_unenforceable(&key, "/v1/completions"));

        // Other endpoints that return generated text are refused too; those
        // that return no text are not.
        key.guardrails_policy = Some(policy(obleth_config::GuardrailsAction::Block, &["pii"]));
        for path in [
            "/v1/audio/transcriptions",
            "/v1/audio/translations",
            "/v1/some/unknown/path",
        ] {
            assert!(output_guardrails_unenforceable(&key, path), "{path}");
        }
        for path in [
            "/v1/embeddings",
            "/v1/audio/speech",
            "/v1/images/generations",
            "/v1/rerank",
            "/v1/moderations",
        ] {
            assert!(!output_guardrails_unenforceable(&key, path), "{path}");
        }

        // Internal probe keys are exempt from guardrails.
        key.internal = true;
        assert!(!output_guardrails_unenforceable(&key, "/v1/completions"));
    }

    #[test]
    fn output_guardrails_refusal_runs_before_the_input_scan() {
        let src = include_str!("proxy.rs");
        let refuse = src
            .find("if method == Method::POST && output_guardrails_unenforceable(&resolved, &path)")
            .expect("output guardrails refusal");
        let enrich = src.find(".enrich_request(").expect("enrich_request call");
        assert!(refuse < enrich);
    }

    #[test]
    fn post_paths_are_canonicalized_before_surface_dispatch() {
        assert_eq!(
            canonical_post_path("/responses").as_deref(),
            Some("/v1/responses")
        );
        assert_eq!(
            canonical_post_path("/v1/responses/").as_deref(),
            Some("/v1/responses")
        );
        assert_eq!(
            canonical_post_path("/messages").as_deref(),
            Some("/v1/messages")
        );
        assert_eq!(
            canonical_post_path("/messages/count_tokens/").as_deref(),
            Some("/v1/messages/count_tokens")
        );
        assert_eq!(
            canonical_post_path("/verdicts").as_deref(),
            Some("/v1/verdicts")
        );
        assert_eq!(
            canonical_post_path("/v1/chat/completions//").as_deref(),
            Some("/v1/chat/completions")
        );
        // Repeated slashes collapse, so a doubled prefix cannot skip a shim.
        assert_eq!(
            canonical_post_path("//v1/responses").as_deref(),
            Some("/v1/responses")
        );
        assert_eq!(
            canonical_post_path("/v1//messages").as_deref(),
            Some("/v1/messages")
        );
        // Already canonical, or nothing to trim: untouched.
        assert_eq!(canonical_post_path("/v1/chat/completions"), None);
        assert_eq!(canonical_post_path("/"), None);
        // Normalization runs before the shim dispatch in `proxy_handler`.
        let src = include_str!("proxy.rs");
        let handler = src
            .find("pub async fn proxy_handler(")
            .expect("proxy_handler");
        let canon = handler
            + src[handler..]
                .find("canonical_post_path(req.uri().path())")
                .expect("normalization");
        let shim = handler
            + src[handler..]
                .find("return responses_shim(")
                .expect("responses dispatch");
        assert!(canon < shim);
    }

    #[test]
    fn unscannable_paths_are_refused_only_under_an_enforcing_input_policy() {
        let policy = |action| obleth_config::GuardrailsPolicy {
            action,
            input_scanners: vec!["pii".into()],
            output_scanners: vec![],
            guard_model: None,
            ban_keywords: vec![],
            fail_open: true,
        };
        let mut key = key_with_schedule("UTC", None, None, None);
        assert!(!input_guardrails_unscannable(&key, "/v2/responses"));
        key.guardrails_policy = Some(policy(obleth_config::GuardrailsAction::Block));
        for path in [
            "/v2/responses",
            "/openai/v1/responses",
            "/v1/some/unknown/path",
        ] {
            assert!(input_guardrails_unscannable(&key, path), "{path}");
        }
        for path in ["/v1/chat/completions", "/v1/completions", "/v1/embeddings"] {
            assert!(!input_guardrails_unscannable(&key, path), "{path}");
        }
        key.guardrails_policy = Some(policy(obleth_config::GuardrailsAction::LogOnly));
        assert!(!input_guardrails_unscannable(&key, "/v2/responses"));
        key.guardrails_policy = Some(policy(obleth_config::GuardrailsAction::Redact));
        key.internal = true;
        assert!(!input_guardrails_unscannable(&key, "/v2/responses"));
    }

    #[test]
    fn the_classifier_is_skipped_under_an_input_policy() {
        let mut key = key_with_schedule("UTC", None, None, None);
        assert!(!has_input_guardrails(&key));
        key.guardrails_policy = Some(obleth_config::GuardrailsPolicy {
            action: obleth_config::GuardrailsAction::LogOnly,
            input_scanners: vec!["pii".into()],
            output_scanners: vec![],
            guard_model: None,
            ban_keywords: vec![],
            fail_open: true,
        });
        assert!(has_input_guardrails(&key));
        key.internal = true;
        assert!(!has_input_guardrails(&key));
        let src = include_str!("proxy.rs");
        assert!(src
            .contains("            has_input_guardrails(&resolved),\n        )\n        .await;"));
    }

    #[test]
    fn synthetic_tenant_requests_are_tagged_benchmark() {
        let mut resolved = key_with_schedule("UTC", None, None, None);
        assert_eq!(
            effective_request_type(&resolved, "/v1/chat/completions"),
            "chat"
        );
        resolved.synthetic = true;
        assert_eq!(
            effective_request_type(&resolved, "/v1/chat/completions"),
            obleth_config::BENCHMARK_REQUEST_TYPE
        );
    }

    #[test]
    fn a_synthetic_tenants_chat_request_still_counts_as_chat_for_boons() {
        // The ledger label and the boon gate must not be the same value. A
        // synthetic tenant's chat completion is stamped `benchmark` for
        // accounting, but it is still a chat request: every chat-only boon
        // (knowledge, compression, tools, structured output, the MCP tool loop)
        // has to fire for it, or the console and the benchmark suite measure a
        // gateway with those boons silently switched off.
        let mut resolved = key_with_schedule("UTC", None, None, None);
        resolved.synthetic = true;
        assert_eq!(
            effective_request_type(&resolved, "/v1/chat/completions"),
            obleth_config::BENCHMARK_REQUEST_TYPE,
            "accounting still tags it benchmark"
        );
        assert!(
            is_chat_path("/v1/chat/completions"),
            "but the boon gate reads the path, not that label"
        );
        assert!(!is_chat_path("/v1/embeddings"));
        assert!(!is_chat_path("/v1/images/generations"));
    }

    #[test]
    fn boons_are_gated_on_the_path_not_the_ledger_label() {
        // Both helpers above are individually correct; the defect was purely in
        // the wiring — `enrich_request` was handed `req_meta.request_type ==
        // "chat"`, which is never true for a synthetic tenant. No test over the
        // pure functions can catch that, so this pins the call site itself
        // (same approach as `knowledge_boon_phases_stay_in_source_order`).
        let src = include_str!("proxy.rs");
        let call = src
            .find(".enrich_request(")
            .expect("the enrich_request call site");
        let args = &src[call..(call + 600).min(src.len())];
        assert!(
            args.contains("is_chat_path(&path)"),
            "the chat flag passed to the boon engine must be path-derived"
        );
        assert!(
            !args.contains("request_type == \"chat\""),
            "the ledger's request_type is an accounting label and must not gate boons"
        );
    }

    #[test]
    fn avoids_duplicate_v1_prefix() {
        assert_eq!(
            build_upstream_url(
                "https://inference.example.com/v1",
                "/v1/chat/completions",
                ""
            ),
            "https://inference.example.com/v1/chat/completions"
        );
    }

    #[test]
    fn a_translated_responses_call_is_distinguished_from_a_native_chat_one() {
        // The shim runs the request down the chat path on purpose, so the path
        // alone cannot tell the two surfaces apart — the header does.
        let mut headers = HeaderMap::new();
        assert_eq!(surface(&headers), None);
        assert_eq!(request_type_for_path("/v1/chat/completions"), "chat");

        headers.insert(
            crate::responses::SURFACE_HEADER,
            "responses".parse().unwrap(),
        );
        assert_eq!(surface(&headers), Some("responses"));

        let mut other = HeaderMap::new();
        other.insert(
            crate::responses::SURFACE_HEADER,
            "something-else".parse().unwrap(),
        );
        assert_eq!(surface(&other), Some("something-else"));
    }

    #[test]
    fn verdict_requests_are_classified_by_path() {
        // `/v1/verdicts` is served by its own route (see main.rs), but the
        // ledger label still derives from the path like every other class.
        assert_eq!(request_type_for_path("/v1/verdicts"), "verdict");
        // Not confused with the legacy completions suffix match.
        assert_eq!(request_type_for_path("/v1/completions"), "completion");
    }

    #[test]
    fn models_collection_covers_both_spellings_but_not_a_detail_path() {
        assert!(is_models_collection("/v1/models"));
        // The slash carries no id: this lists, it does not look up "".
        assert!(is_models_collection("/v1/models/"));

        // A real id is a detail lookup, not the collection.
        assert!(!is_models_collection("/v1/models/glm-5-3"));
        assert!(!is_models_collection("/v1/models/glm-5-3/"));
        assert!(!is_models_collection("/v1/model/info"));
        assert!(!is_models_collection("/v1/chat/completions"));
    }

    #[test]
    fn both_collection_spellings_are_model_discovery_endpoints() {
        // Both must stay reachable without resolving a model, or the listing
        // is rejected as an unmapped path before it can be served.
        assert!(is_models_endpoint("/v1/models"));
        assert!(is_models_endpoint("/v1/models/"));
    }

    #[test]
    fn unmapped_paths_are_rejected_not_forwarded() {
        // The handler rejects a request when it resolved to no registered model
        // (route None), the path is not a recognized OpenAI endpoint, and it is
        // not a model-discovery endpoint. These predicates encode that rule.
        let is_unmapped =
            |path: &str| request_type_for_path(path) == "other" && !is_models_endpoint(path);

        // Stray probes / scans -> unmapped -> rejected instead of forwarded.
        assert!(is_unmapped("/props"));
        assert!(is_unmapped("/health"));
        assert!(is_unmapped("/favicon.ico"));
        assert!(is_unmapped("/"));

        // Model-discovery endpoints stay allowed (served without a model).
        assert!(!is_unmapped("/v1/models"));
        assert!(!is_unmapped("/v1/models/gpt-4o"));

        // Recognized OpenAI endpoints are never treated as unmapped.
        assert!(!is_unmapped("/v1/chat/completions"));
        assert!(!is_unmapped("/v1/embeddings"));
        assert!(!is_unmapped("/v1/rerank"));
        assert!(!is_unmapped("/v1/moderations"));
    }

    #[test]
    fn benchmark_backend_path_unchanged() {
        assert_eq!(
            build_upstream_url("http://benchmark-backend:8081", "/v1/chat/completions", ""),
            "http://benchmark-backend:8081/v1/chat/completions"
        );
    }

    #[test]
    fn api_base_with_full_endpoint_is_not_doubled() {
        // Operator pasted the full endpoint URL as api_base instead of the base.
        assert_eq!(
            build_upstream_url(
                "https://inference.example.com/v1/embeddings",
                "/v1/embeddings",
                ""
            ),
            "https://inference.example.com/v1/embeddings"
        );
        assert_eq!(
            build_upstream_url(
                "https://inference.example.com/v1/audio/speech",
                "/v1/audio/speech",
                ""
            ),
            "https://inference.example.com/v1/audio/speech"
        );
    }

    #[test]
    fn path_traversal_is_detected() {
        // Literal `..` segments anywhere in the path.
        assert!(has_path_traversal("/v1/../admin"));
        assert!(has_path_traversal("/v1/chat/../../secret"));
        assert!(has_path_traversal("/.."));
        // Percent-encoded dots and separators that could decode to a traversal.
        assert!(has_path_traversal("/v1/%2e%2e/admin"));
        assert!(has_path_traversal("/v1%2fadmin"));
        assert!(has_path_traversal("/v1%5c..%5cadmin"));
        // Backslash separators.
        assert!(has_path_traversal("\\..\\admin"));
    }

    #[test]
    fn legitimate_paths_are_allowed() {
        assert!(!has_path_traversal("/v1/chat/completions"));
        assert!(!has_path_traversal("/v1/audio/transcriptions"));
        assert!(!has_path_traversal("/health"));
        // A literal `..` only inside a longer segment is not a traversal segment.
        assert!(!has_path_traversal("/v1/models/gpt..4"));
    }

    #[test]
    fn no_schedule_is_always_active() {
        let key = key_with_schedule("UTC", None, None, None);
        assert!(tenant_active_now(&key, Utc::now()).is_ok());
    }

    #[test]
    fn before_active_from_is_blocked() {
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let from = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let key = key_with_schedule("UTC", Some(from), None, None);
        assert!(tenant_active_now(&key, now).is_err());
    }

    #[test]
    fn after_active_until_is_blocked() {
        let now = Utc.with_ymd_and_hms(2026, 1, 3, 12, 0, 0).unwrap();
        let until = Utc.with_ymd_and_hms(2026, 1, 2, 0, 0, 0).unwrap();
        let key = key_with_schedule("UTC", None, Some(until), None);
        assert!(tenant_active_now(&key, now).is_err());
    }

    #[test]
    fn inside_weekly_window_is_allowed() {
        // 2026-01-01 is a Thursday (weekday 4). 12:00 UTC = 720 minutes.
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 12, 0, 0).unwrap();
        let key = key_with_schedule(
            "UTC",
            None,
            None,
            Some(vec![WeeklyWindow {
                day: 4,
                start_min: 9 * 60,
                end_min: 17 * 60,
            }]),
        );
        assert!(tenant_active_now(&key, now).is_ok());
    }

    #[test]
    fn outside_weekly_window_is_blocked() {
        // Thursday 20:00 UTC, window only covers 09:00-17:00.
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 20, 0, 0).unwrap();
        let key = key_with_schedule(
            "UTC",
            None,
            None,
            Some(vec![WeeklyWindow {
                day: 4,
                start_min: 9 * 60,
                end_min: 17 * 60,
            }]),
        );
        assert!(tenant_active_now(&key, now).is_err());
    }

    #[test]
    fn timezone_shifts_the_local_weekday() {
        // 2026-01-01 02:00 UTC is still Wednesday (day 3) in New York (UTC-5).
        let now = Utc.with_ymd_and_hms(2026, 1, 1, 2, 0, 0).unwrap();
        let key = key_with_schedule(
            "America/New_York",
            None,
            None,
            Some(vec![WeeklyWindow {
                day: 3,
                start_min: 0,
                end_min: 24 * 60,
            }]),
        );
        assert!(tenant_active_now(&key, now).is_ok());
    }

    /// A registered route as the discovery endpoints see it, with only the
    /// fields those endpoints read set to anything interesting.
    fn candidate(
        model_name: &str,
        upstream_model: &str,
        model_type: &str,
        quantization: &str,
        aliases: &[&str],
        tags: &[&str],
    ) -> obleth_config::routing::Candidate {
        let mut model = model_with(Vec::new());
        model.model_name = model_name.to_string();
        model.upstream_model = upstream_model.to_string();
        model.model_type = model_type.to_string();
        model.quantization = quantization.to_string();
        model.aliases = aliases.iter().map(|a| a.to_string()).collect();
        model.tags = tags.iter().map(|t| t.to_string()).collect();
        obleth_config::routing::Candidate {
            model,
            healthy: true,
            levels: Vec::new(),
        }
    }

    #[test]
    fn models_listing_is_the_registry_under_client_facing_names() {
        use super::registry_models_list;
        let mut disabled = candidate("retired", "retired", "chat", "none", &[], &[]);
        disabled.model.enabled = false;
        let candidates = vec![
            // A backend that knows itself only by its quantized name, with an
            // old spelling kept as an alias. Whether or not that backend
            // serves a catalog, the route is listed once, as `glm-5-3`.
            candidate(
                "glm-5-3",
                "glm-5-3-mxfp4",
                "chat",
                "mxfp4",
                &["glm-5-3-fp8"],
                &["coding"],
            ),
            candidate(
                "image-model",
                "vendor/image-model",
                "image",
                "bf16",
                &[],
                &["creative"],
            ),
            disabled,
        ];
        let list = registry_models_list(&candidates, None);
        assert_eq!(list["object"], "list");
        let data = list["data"].as_array().unwrap();
        let ids: Vec<&str> = data.iter().map(|m| m["id"].as_str().unwrap()).collect();
        // Sorted, enabled routes only, and never a backend id.
        assert_eq!(ids, vec!["glm-5-3", "image-model"]);

        let glm = &data[0];
        assert_eq!(glm["object"], "model");
        assert_eq!(glm["owned_by"], "obleth");
        assert_eq!(glm["quantization"], "mxfp4");
        assert_eq!(glm["tags"], serde_json::json!(["coding"]));
        // The old name is advertised as an alias, so a client that had pinned
        // it can see where it went instead of finding it simply gone.
        assert_eq!(glm["aliases"], serde_json::json!(["glm-5-3-fp8"]));
        // Both the obleth vocabulary and the LiteLLM-convention alias.
        assert_eq!(data[1]["model_type"], "image");
        assert_eq!(data[1]["mode"], "image_generation");
        assert_eq!(glm["mode"], "chat");
        assert!(!list.to_string().contains("glm-5-3-mxfp4"));
    }

    #[test]
    fn a_listing_entry_and_the_detail_lookup_agree() {
        use super::{model_entry, model_facts_index, registry_models_list};
        let candidates = vec![candidate(
            "glm-5-3",
            "glm-5-3-mxfp4",
            "chat",
            "mxfp4",
            &["glm-5-3-fp8"],
            &[],
        )];
        let listed = registry_models_list(&candidates, None)["data"][0].clone();
        let detail = model_entry(&model_facts_index(&candidates)["glm-5-3-fp8"]);
        assert_eq!(listed, detail);
    }

    #[test]
    fn the_models_listing_shows_a_tenant_only_its_allowed_models() {
        use super::{model_visible, registry_models_list};
        let candidates = vec![
            candidate(
                "glm-5-3",
                "glm-5-3-mxfp4",
                "chat",
                "mxfp4",
                &["glm-5-3-fp8"],
                &[],
            ),
            candidate(
                "image-model",
                "vendor/image-model",
                "image",
                "bf16",
                &[],
                &[],
            ),
        ];
        let allowed = vec!["glm-5-3".to_string()];
        let list = registry_models_list(&candidates, Some(&allowed));
        let ids: Vec<&str> = list["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["glm-5-3"]);
        // No allowlist, or an internal caller, sees every enabled route.
        assert_eq!(
            registry_models_list(&candidates, None)["data"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        // Visibility is decided on the canonical name an alias resolves to.
        assert!(model_visible(Some(&allowed), "glm-5-3"));
        assert!(!model_visible(Some(&allowed), "image-model"));
        assert!(!model_visible(Some(&[]), "glm-5-3"));
    }

    #[test]
    fn the_models_listing_makes_no_upstream_calls() {
        // Served from the registry snapshot: no per-upstream fan-out, so an
        // upstream without a catalog (or one that is down) cannot shrink it.
        let src = include_str!("proxy.rs");
        let src = &src[..src.find("\nmod tests {").expect("the test module")];
        let start = src.find("fn models_list_response(").unwrap();
        let end = start + src[start..].find("\n}\n").unwrap();
        let body = &src[start..end];
        assert!(!body.contains("http"), "{body}");
        assert!(!body.contains(".await"), "{body}");
    }

    #[test]
    fn model_facts_index_prefers_the_client_facing_name() {
        use super::model_facts_index;
        // A pathological-but-legal fleet: one model's `upstream_model` is
        // another model's client-facing `model_name`. Resolution matches on
        // `model_name`, so the index has to agree with it.
        let candidates = vec![
            candidate("shared-name", "shared-name-fp8", "chat", "fp8", &[], &[]),
            candidate("other", "shared-name", "chat", "none", &[], &[]),
        ];
        let index = model_facts_index(&candidates);
        assert_eq!(index["shared-name"].model_name, "shared-name");
        assert_eq!(index["shared-name-fp8"].model_name, "shared-name");
        assert_eq!(index["other"].model_name, "other");
    }

    #[test]
    fn model_info_entry_uses_obleth_params_and_keeps_the_conventional_keys() {
        use super::model_info_entry;
        let mut model = model_with(Vec::new());
        model.model_name = "glm-5-3".into();
        model.upstream_model = "glm-5-3-mxfp4".into();
        model.api_base = "http://glm-5-3.internal.svc:8000/v1".into();
        model.api_key = Some("sk-secret".into());
        model.quantization = "mxfp4".into();
        model.aliases = vec!["glm-5-3-mxfp4".into()];
        model.tags = vec!["coding".into()];
        model.context_window = 200_000;

        let entry = model_info_entry(&model, false);
        assert_eq!(entry["model_name"], "glm-5-3");
        // The per-route object is ours; the upstream name lives in it.
        assert_eq!(entry["obleth_params"]["model"], "glm-5-3-mxfp4");
        // The old LiteLLM-branded key is gone, not duplicated.
        assert!(entry.get("litellm_params").is_none());
        assert_eq!(entry["model_info"]["mode"], "chat");
        assert_eq!(entry["model_info"]["max_input_tokens"], 200_000);
        // obleth's additions, including the format that used to live in the name.
        assert_eq!(entry["model_info"]["quantization"], "mxfp4");
        assert_eq!(
            entry["model_info"]["aliases"],
            serde_json::json!(["glm-5-3-mxfp4"])
        );
        assert_eq!(entry["model_info"]["tags"], serde_json::json!(["coding"]));
        // Registered but failing its probe: addressable, and honestly labelled.
        assert_eq!(entry["model_info"]["healthy"], false);
        // A tenant-facing endpoint never reports how to reach the backend.
        let rendered = entry.to_string();
        assert!(!rendered.contains("sk-secret"), "{rendered}");
        assert!(!rendered.contains("internal.svc"), "{rendered}");
    }

    #[test]
    fn model_detail_answers_for_the_clean_name_and_its_aliases() {
        use super::model_facts_index;
        // The lookup `registered_model_entry` performs, without an AppState:
        // the detail probe has to accept the clean name the listing publishes
        // and the aliases that still resolve, or a client following the
        // listing would ask for an id the gateway itself advertised and 404.
        let candidates = vec![candidate(
            "glm-5-3",
            "glm-5-3-mxfp4",
            "chat",
            "mxfp4",
            &["glm-5-3-fp8"],
            &[],
        )];
        let index = model_facts_index(&candidates);
        for id in ["glm-5-3", "glm-5-3-fp8", "glm-5-3-mxfp4"] {
            assert_eq!(
                index.get(id).map(|f| f.model_name.as_str()),
                Some("glm-5-3"),
                "{id}"
            );
        }
        // An unclaimed id still has no entry, so the request falls through to
        // the upstream passthrough rather than being answered with a guess.
        assert!(!index.contains_key("wildcard-passthrough"));
    }

    #[test]
    fn model_info_paths_are_recognized_bare_and_under_v1() {
        use super::{is_model_info_endpoint, is_models_endpoint};
        for path in ["/model/info", "/v1/model/info"] {
            assert!(is_model_info_endpoint(path), "{path}");
            // Also exempt from the unmapped-path rejection, or the route would
            // 404 before its handler ran.
            assert!(is_models_endpoint(path), "{path}");
        }
        assert!(!is_model_info_endpoint("/model/info/extra"));
        assert!(!is_model_info_endpoint("/v1/models"));
    }

    fn model_with(endpoints: Vec<ResolvedEndpoint>) -> obleth_config::ResolvedModel {
        obleth_config::ResolvedModel {
            model_name: "m".into(),
            aliases: Vec::new(),
            quantization: "unknown".into(),
            upstream_model: "m".into(),
            api_base: "http://primary/v1".into(),
            api_key: Some("model-key".into()),
            upstream_headers: Default::default(),
            model_type: obleth_config::DEFAULT_MODEL_TYPE.to_string(),
            admission_weight: 100,
            max_in_flight: None,
            capacity_mode: "static".into(),
            capacity_source: "endpoints".into(),
            capacity_namespace: None,
            capacity_service: None,
            per_replica_max_in_flight: None,
            capacity_headroom: 1.0,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 0,
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            cost_per_video: 0.0,
            context_window: 128_000,
            supports_function_calling: true,
            supports_system_messages: true,
            supports_response_schema: true,
            supports_tool_choice: true,
            supports_vision: false,
            tags: Vec::new(),
            declared_levels: Vec::new(),
            boons: Vec::new(),
            tool_servers: Vec::new(),
            knowledge_collections: Vec::new(),
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: obleth_config::DEFAULT_RETRY_BACKOFF_MS,
            endpoint_selection_mode: obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string(),
            debug_diagnostics: false,
            energy_slots_per_node: 0,
            route_bias: 1.0,
            auto_eligible: true,
            draft_model: String::new(),
            verify_api_base: String::new(),
            verify_upstream_model: String::new(),
            endpoints,
        }
    }

    #[test]
    fn append_tail_keeps_last_cap_bytes() {
        use super::{append_tail, extract_usage, TAIL_CAP};
        let mut tail = Vec::new();
        // Small chunks accumulate verbatim.
        append_tail(&mut tail, b"hello ");
        append_tail(&mut tail, b"world");
        assert_eq!(tail, b"hello world");
        // Filling past the cap keeps only the newest TAIL_CAP bytes.
        append_tail(&mut tail, &vec![b'x'; TAIL_CAP]);
        assert_eq!(tail.len(), TAIL_CAP);
        assert!(tail.iter().all(|b| *b == b'x'));
        // A usage blob appended at the end survives intact and parses.
        append_tail(
            &mut tail,
            br#"{"usage":{"prompt_tokens":12,"completion_tokens":34}}"#,
        );
        assert_eq!(tail.len(), TAIL_CAP);
        assert_eq!(
            extract_usage(&String::from_utf8_lossy(&tail)),
            Some((12, 34))
        );
        // A chunk larger than the cap replaces the buffer with its own tail.
        append_tail(&mut tail, &vec![b'y'; TAIL_CAP * 2]);
        assert_eq!(tail.len(), TAIL_CAP);
        assert!(tail.iter().all(|b| *b == b'y'));
    }

    #[test]
    fn retryable_status_classification() {
        for s in [408, 429, 500, 502, 503, 504] {
            assert!(is_retryable_status(s), "{s} should be retryable");
        }
        for s in [200, 201, 400, 401, 403, 404, 422] {
            assert!(!is_retryable_status(s), "{s} should be fatal");
        }
    }

    #[test]
    fn backoff_grows_exponentially_and_caps() {
        let base = Duration::from_millis(100);
        assert_eq!(backoff_for(base, 0), Duration::from_millis(100));
        assert_eq!(backoff_for(base, 1), Duration::from_millis(200));
        assert_eq!(backoff_for(base, 2), Duration::from_millis(400));
        // Cap kicks in at attempt 6 (×64); higher attempts stay flat.
        assert_eq!(backoff_for(base, 10), backoff_for(base, 6));
        // Zero base stays zero (no backoff configured).
        assert_eq!(backoff_for(Duration::ZERO, 3), Duration::ZERO);
    }

    #[test]
    fn no_endpoints_falls_back_to_model_base() {
        let model = model_with(Vec::new());
        let targets = build_targets(Some(&model), "http://global/v1", "failover", "");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].base, "http://primary/v1");
        assert_eq!(targets[0].api_key.as_deref(), Some("model-key"));
    }

    #[test]
    fn none_route_uses_global_default() {
        let targets = build_targets(None, "http://global/v1", "failover", "");
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].base, "http://global/v1");
        assert!(targets[0].api_key.is_none());
    }

    #[test]
    fn failover_orders_by_priority_and_skips_unusable() {
        let model = model_with(vec![
            endpoint("c", "http://c", 30, 100, true, true),
            endpoint("a", "http://a", 10, 100, true, true),
            endpoint("disabled", "http://x", 5, 100, false, true),
            endpoint("unhealthy", "http://y", 1, 100, true, false),
            endpoint("b", "http://b", 20, 100, true, true),
        ]);
        let targets = build_targets(Some(&model), "http://global/v1", "failover", "");
        let bases: Vec<&str> = targets.iter().map(|t| t.base.as_str()).collect();
        assert_eq!(bases, vec!["http://a", "http://b", "http://c"]);
    }

    #[test]
    fn endpoint_key_falls_back_to_model_key() {
        let mut ep = endpoint("a", "http://a", 10, 100, true, true);
        ep.api_key = None;
        let model = model_with(vec![ep]);
        let targets = build_targets(Some(&model), "http://global/v1", "failover", "");
        assert_eq!(targets[0].api_key.as_deref(), Some("model-key"));
    }

    #[test]
    fn every_target_carries_the_models_upstream_headers_over_the_clients() {
        let mut model = model_with(vec![
            endpoint("a", "http://a", 10, 100, true, true),
            endpoint("b", "http://b", 20, 100, true, true),
        ]);
        model.upstream_headers = [
            ("x-routing-hint".to_string(), "sticky".to_string()),
            ("x-team".to_string(), "ops".to_string()),
        ]
        .into();
        let targets = build_targets(Some(&model), "http://global/v1", "failover", "");
        assert_eq!(targets.len(), 2);
        for t in &targets {
            assert_eq!(t.headers["x-routing-hint"], "sticky");
        }
        // The legacy single-upstream fallback carries them too.
        model.endpoints.clear();
        let fallback = build_targets(Some(&model), "http://global/v1", "failover", "");
        assert_eq!(fallback[0].headers["x-team"], "ops");
        // An unrouted request has no model headers to add.
        assert!(build_targets(None, "http://global/v1", "failover", "")[0]
            .headers
            .is_empty());

        // Applied the way dispatch applies them: after the client's
        // forwarded headers, so the operator's value wins a shared name.
        let mut client = HeaderMap::new();
        client.insert("x-team", "client".parse().unwrap());
        client.insert("x-trace", "t1".parse().unwrap());
        let mut fwd = forward_headers(&client);
        fwd.extend(fallback[0].headers.clone());
        assert_eq!(fwd["x-team"], "ops");
        assert_eq!(fwd.get_all("x-team").iter().count(), 1);
        assert_eq!(fwd["x-trace"], "t1");
    }

    #[test]
    fn prepare_upstream_body_forces_non_streaming() {
        let model = model_with(Vec::new());
        let mut json = serde_json::json!({
            "model": "client-name",
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": []
        });
        let body = prepare_upstream_body(
            Some(&model),
            &mut json,
            axum::body::Bytes::new(),
            true,
            false,
        );
        let sent: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sent["stream"], false);
        assert!(sent.get("stream_options").is_none());
        // The model-name swap still happens.
        assert_eq!(sent["model"], "m");
    }

    #[test]
    fn prepare_upstream_body_adds_usage_for_stream_tap() {
        // Turn-0 of the streaming tool loop: stay streaming but ask for a final
        // usage chunk so turn-0 billing is exact.
        let model = model_with(Vec::new());
        let mut json = serde_json::json!({
            "model": "client-name",
            "stream": true,
            "messages": []
        });
        let body = prepare_upstream_body(
            Some(&model),
            &mut json,
            axum::body::Bytes::new(),
            false,
            true,
        );
        let sent: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["stream_options"]["include_usage"], true);
    }

    #[test]
    fn prepare_upstream_body_requests_usage_and_keeps_client_stream_options() {
        // Every streaming chat call asks for usage, so billing never depends
        // on the client's `include_usage`; the client's other options survive.
        let model = model_with(Vec::new());
        let mut json = serde_json::json!({
            "model": "client-name",
            "stream": true,
            "stream_options": { "continuous_usage_stats": false },
            "messages": []
        });
        let body = prepare_upstream_body(
            Some(&model),
            &mut json,
            axum::body::Bytes::new(),
            false,
            true,
        );
        let sent: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sent["stream"], true);
        assert_eq!(sent["stream_options"]["include_usage"], true);
        assert_eq!(sent["stream_options"]["continuous_usage_stats"], false);
    }

    #[test]
    fn prepare_upstream_body_preserves_stream_without_force() {
        let model = model_with(Vec::new());
        let mut json = serde_json::json!({
            "model": "client-name",
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": []
        });
        let body = prepare_upstream_body(
            Some(&model),
            &mut json,
            axum::body::Bytes::new(),
            false,
            false,
        );
        let sent: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(sent["stream"], true);
        assert!(sent.get("stream_options").is_some());
    }

    #[test]
    fn cache_key_diverges_on_client_stream_flag() {
        // The cache key is computed from the client body *before* the boon
        // interception forces `stream: false` upstream, so streaming and
        // non-streaming clients must land on different cache entries.
        let streaming = serde_json::to_vec(
            &serde_json::json!({ "model": "m", "stream": true, "messages": [] }),
        )
        .unwrap();
        let plain = serde_json::to_vec(
            &serde_json::json!({ "model": "m", "stream": false, "messages": [] }),
        )
        .unwrap();
        assert_ne!(
            obleth_config::cache_key("t", "m", &streaming),
            obleth_config::cache_key("t", "m", &plain)
        );
    }

    /// Non-test source of this file, so pins below never match their own text.
    fn handler_source() -> &'static str {
        let src = include_str!("proxy.rs");
        let end = src.find("\nmod tests {").expect("the test module");
        &src[..end]
    }

    #[test]
    fn responses_shim_authenticates_before_reading_the_body() {
        // The shim buffers up to RESPONSES_BODY_MAX before the pipeline runs.
        // An unauthenticated caller must be turned away before that buffer is
        // filled, so the bearer check and key gate have to precede `to_bytes`.
        // There is no AppState harness in this crate (it needs live Redis and
        // ClickHouse), so the order is pinned at the source level.
        let src = handler_source();
        let start = src
            .find("async fn responses_shim(")
            .expect("responses_shim");
        let end = start
            + src[start..]
                .find("async fn translate_response_body(")
                .expect("end of responses_shim");
        let shim = &src[start..end];
        let read = shim.find("to_bytes(").expect("body read");
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
        let src = include_str!("proxy.rs");
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
        let read = pos("to_bytes(");
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
        let src = include_str!("proxy.rs");

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
                .find("\n#[tracing::instrument(")
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
    fn surface_recognises_both_shims() {
        let mut h = HeaderMap::new();
        assert_eq!(surface(&h), None);
        h.insert(
            crate::responses::SURFACE_HEADER,
            header::HeaderValue::from_static("responses"),
        );
        assert_eq!(surface(&h), Some("responses"));
        h.insert(
            crate::responses::SURFACE_HEADER,
            header::HeaderValue::from_static("messages"),
        );
        assert_eq!(surface(&h), Some("messages"));
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
        let src = include_str!("proxy.rs");
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
    fn messages_input_estimate_adds_serialized_tools_length_over_three() {
        let chat_body = serde_json::json!({
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "f", "parameters": {"type": "object"}}}]
        });
        let tools_len = chat_body["tools"].to_string().len() as u64;
        assert_eq!(messages_input_estimate(&chat_body, 10), 10 + tools_len / 3);

        // No `tools` at all: the estimate is the text estimate alone.
        let no_tools = serde_json::json!({"messages": [{"role": "user", "content": "hi"}]});
        assert_eq!(messages_input_estimate(&no_tools, 10), 10);
    }

    #[test]
    fn messages_input_estimate_adds_a_flat_cost_per_image_part() {
        let chat_body = serde_json::json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "what is this"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}}
                ]},
                {"role": "assistant", "content": "it's a cat"},
                {"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": "https://example.com/x.png"}}
                ]}
            ]
        });
        assert_eq!(messages_input_estimate(&chat_body, 100), 100 + 1500 * 2);
    }

    #[test]
    fn messages_input_estimate_causes_clamping_that_the_text_only_estimate_would_miss() {
        // Reproduces the failure mode this fix targets: a 40,960-token
        // window, a small text prompt, and a ~60 KiB `tools` array (Claude
        // Code sends its full tool list on every turn). The text-only
        // tokenizer estimate alone leaves `max_tokens` unclamped; folding in
        // the tools payload clamps it to what the window can actually hold.
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

        // With the tools payload folded in: clamped to less than requested.
        let combined = messages_input_estimate(&chat_body, text_estimate);
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

    #[test]
    fn response_cache_key_includes_the_tenant() {
        let src = handler_source();
        let call = src
            .find("obleth_config::cache_key(")
            .expect("the cache_key call site");
        assert_eq!(
            src.matches("obleth_config::cache_key(").count(),
            1,
            "a single response-cache call site"
        );
        let args = &src[call..(call + 200).min(src.len())];
        assert!(
            args.contains("resolved.tenant_id"),
            "the response cache key must be scoped to the caller's tenant"
        );
    }

    #[test]
    fn response_cache_lookup_is_skipped_when_ttl_disables_caching() {
        let src = handler_source();
        let call = src
            .find("obleth_config::cache_key(")
            .expect("the cache_key call site");
        let gate = &src[call.saturating_sub(120)..call];
        assert!(
            gate.contains("cache_enabled && cache_ttl > 0"),
            "no cache key (and so no cache_get) when TTL <= 0"
        );
    }

    #[test]
    fn weighted_order_preserves_membership() {
        let eps = [
            endpoint("a", "http://a", 10, 100, true, true),
            endpoint("b", "http://b", 20, 50, true, true),
            endpoint("c", "http://c", 30, 1, true, true),
        ];
        let refs: Vec<&ResolvedEndpoint> = eps.iter().collect();
        let ordered = weighted_order(refs);
        assert_eq!(ordered.len(), 3);
        let mut ids: Vec<&str> = ordered.iter().map(|e| e.id.as_str()).collect();
        ids.sort();
        assert_eq!(ids, vec!["a", "b", "c"]);
    }

    #[test]
    fn session_hash_is_deterministic_for_a_key() {
        let eps = [
            endpoint("a", "http://a", 10, 100, true, true),
            endpoint("b", "http://b", 20, 100, true, true),
            endpoint("c", "http://c", 30, 100, true, true),
        ];
        let refs: Vec<&ResolvedEndpoint> = eps.iter().collect();
        let first = session_hash_order(refs.clone(), "session-xyz");
        let second = session_hash_order(refs.clone(), "session-xyz");
        let firsts: Vec<&str> = first.iter().map(|e| e.id.as_str()).collect();
        let seconds: Vec<&str> = second.iter().map(|e| e.id.as_str()).collect();
        // Same key ⇒ identical ordering (so the session sticks to one replica).
        assert_eq!(firsts, seconds);
        // All members retained for failover.
        assert_eq!(first.len(), 3);
    }

    #[test]
    fn session_hash_distributes_across_keys() {
        let eps = [
            endpoint("a", "http://a", 10, 100, true, true),
            endpoint("b", "http://b", 20, 100, true, true),
            endpoint("c", "http://c", 30, 100, true, true),
        ];
        let refs: Vec<&ResolvedEndpoint> = eps.iter().collect();
        // Different keys should not all land on the same home endpoint.
        let homes: std::collections::HashSet<String> = (0..40)
            .map(|i| {
                session_hash_order(refs.clone(), &format!("k{i}"))[0]
                    .id
                    .clone()
            })
            .collect();
        assert!(
            homes.len() > 1,
            "session_hash should spread homes across keys"
        );
    }

    #[test]
    fn session_hash_empty_key_keeps_membership() {
        let eps = [
            endpoint("a", "http://a", 10, 100, true, true),
            endpoint("b", "http://b", 20, 100, true, true),
        ];
        let refs: Vec<&ResolvedEndpoint> = eps.iter().collect();
        let ordered = session_hash_order(refs, "");
        assert_eq!(ordered.len(), 2);
    }

    // --- conversation resolver tests ---

    #[test]
    fn resolve_prefers_explicit_header() {
        let mut headers = HeaderMap::new();
        headers.insert("x-session-id", "sess-abc".parse().unwrap());
        let json = serde_json::json!({"messages":[{"role":"user","content":"hi"}]});
        let c = resolve_conversation(&headers, &json, Uuid::nil(), true);
        assert_eq!(c.value, "sess-abc");
        assert_eq!(c.source.as_str(), "client");
    }

    #[test]
    fn resolve_prefers_body_session_id_over_derivation() {
        let json = serde_json::json!({
            "session_id": "body-1",
            "messages":[{"role":"user","content":"hi"}]
        });
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), true);
        assert_eq!(c.value, "body-1");
        assert_eq!(c.source.as_str(), "client");
    }

    #[test]
    fn resolve_reads_metadata_session_id() {
        let json = serde_json::json!({
            "metadata": {"session_id": "meta-9"},
            "messages":[{"role":"user","content":"hi"}]
        });
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), true);
        assert_eq!(c.value, "meta-9");
        assert_eq!(c.source.as_str(), "client");
    }

    #[test]
    fn resolve_ignores_user_field() {
        // The OpenAI `user` field must NOT be treated as a session source.
        let json = serde_json::json!({
            "user": "user-123",
            "messages":[{"role":"user","content":"hi"}]
        });
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), true);
        assert_eq!(c.source.as_str(), "derived");
        assert_ne!(c.value, "user-123");
    }

    #[test]
    fn derived_is_stable_across_turns() {
        let tid = Uuid::nil();
        let turn1 = serde_json::json!({"messages":[
            {"role":"system","content":"You are helpful."},
            {"role":"user","content":"What is Rust?"}
        ]});
        let turn3 = serde_json::json!({"messages":[
            {"role":"system","content":"You are helpful."},
            {"role":"user","content":"What is Rust?"},
            {"role":"assistant","content":"A language."},
            {"role":"user","content":"And Go?"}
        ]});
        let a = resolve_conversation(&HeaderMap::new(), &turn1, tid, true);
        let b = resolve_conversation(&HeaderMap::new(), &turn3, tid, true);
        assert_eq!(a.source.as_str(), "derived");
        assert_eq!(a.value, b.value, "same seed across turns must hash equal");
    }

    #[test]
    fn derived_differs_by_opening_and_tenant() {
        let t1 = Uuid::from_u128(1);
        let t2 = Uuid::from_u128(2);
        let q1 = serde_json::json!({"messages":[{"role":"user","content":"alpha"}]});
        let q2 = serde_json::json!({"messages":[{"role":"user","content":"beta"}]});
        let a = resolve_conversation(&HeaderMap::new(), &q1, t1, true);
        let b = resolve_conversation(&HeaderMap::new(), &q2, t1, true);
        let c = resolve_conversation(&HeaderMap::new(), &q1, t2, true);
        assert_ne!(a.value, b.value, "different opener -> different id");
        assert_ne!(a.value, c.value, "different tenant -> different id");
        assert_eq!(a.value.len(), 16, "16 hex chars");
    }

    #[test]
    fn derivation_disabled_yields_none() {
        let json = serde_json::json!({"messages":[{"role":"user","content":"hi"}]});
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), false);
        assert_eq!(c.source.as_str(), "none");
        assert!(c.value.is_empty());
    }

    #[test]
    fn no_messages_yields_none() {
        let json = serde_json::json!({"input": "embed me"});
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), true);
        assert_eq!(c.source.as_str(), "none");
        assert!(c.value.is_empty());
    }

    #[test]
    fn multimodal_first_user_uses_text_parts() {
        let json = serde_json::json!({"messages":[
            {"role":"user","content":[
                {"type":"text","text":"describe"},
                {"type":"image_url","image_url":{"url":"data:..."}}
            ]}
        ]});
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), true);
        assert_eq!(c.source.as_str(), "derived");
        assert_eq!(c.value.len(), 16);
    }

    #[test]
    fn session_hash_sticks_and_repins_when_endpoint_removed() {
        // Two endpoints; a fixed key picks one deterministically.
        let ep_a = "http://a/v1";
        let ep_b = "http://b/v1";
        let m_all = model_with(vec![
            endpoint("a", ep_a, 100, 100, true, true),
            endpoint("b", ep_b, 100, 100, true, true),
        ]);
        let key = "deadbeefdeadbeef";
        let first = build_targets(Some(&m_all), "http://global/v1", "session_hash", key);
        let again = build_targets(Some(&m_all), "http://global/v1", "session_hash", key);
        assert_eq!(first[0].base, again[0].base, "same key -> same primary");

        // Remove whichever endpoint was primary; the survivor must take over.
        let primary = first[0].base.clone();
        let survivor = if primary == ep_a { ep_b } else { ep_a };
        let survivors: Vec<_> = vec![
            endpoint("a", ep_a, 100, 100, true, true),
            endpoint("b", ep_b, 100, 100, true, true),
        ]
        .into_iter()
        .filter(|e| e.api_base != primary)
        .collect();
        let m_one = model_with(survivors);
        let after = build_targets(Some(&m_one), "http://global/v1", "session_hash", key);
        assert_eq!(after.len(), 1);
        assert_eq!(
            after[0].base, survivor,
            "re-pinned to the survivor, not the global fallback"
        );
    }

    #[test]
    fn credential_routing_prefers_key_path_for_secret_keys() {
        // A minted secret must never be mistaken for a JWT even if it were to
        // contain dots; and a JWT must never be hashed as a secret.
        let secret = obleth_config::generate_api_key().secret;
        assert!(!crate::jwt_auth::looks_like_jwt(&secret));
        assert!(crate::jwt_auth::looks_like_jwt(
            "eyJhbGciOiJFUzI1NiJ9.eyJpc3MiOiJ4In0.c2ln"
        ));
    }

    #[test]
    fn request_meta_carries_device_id_into_usage_record() {
        let meta = RequestMeta {
            session_id: String::new(),
            session_id_source: "none",
            request_type: "chat",
            device_id: "dev-1".into(),
        };
        assert_eq!(meta.device_id, "dev-1");
    }

    fn minimal_model(name: &str) -> ResolvedModel {
        ResolvedModel {
            model_name: name.to_string(),
            aliases: Vec::new(),
            quantization: "unknown".into(),
            upstream_model: name.to_string(),
            api_base: "http://upstream".to_string(),
            api_key: None,
            upstream_headers: Default::default(),
            model_type: obleth_config::DEFAULT_MODEL_TYPE.to_string(),
            admission_weight: 100,
            max_in_flight: None,
            capacity_mode: "static".into(),
            capacity_source: "endpoints".into(),
            capacity_namespace: None,
            capacity_service: None,
            per_replica_max_in_flight: None,
            capacity_headroom: 1.0,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 0,
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            cost_per_video: 0.0,
            context_window: 128_000,
            supports_function_calling: true,
            supports_system_messages: true,
            supports_response_schema: true,
            supports_tool_choice: true,
            supports_vision: false,
            tags: Vec::new(),
            declared_levels: Vec::new(),
            boons: Vec::new(),
            tool_servers: Vec::new(),
            knowledge_collections: Vec::new(),
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: obleth_config::DEFAULT_RETRY_BACKOFF_MS,
            endpoint_selection_mode: obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string(),
            debug_diagnostics: false,
            energy_slots_per_node: 0,
            route_bias: 1.0,
            auto_eligible: true,
            draft_model: String::new(),
            verify_api_base: String::new(),
            verify_upstream_model: String::new(),
            endpoints: Vec::new(),
        }
    }

    // Regression guard for the `auto_route` span payload: this is the exact
    // shape `route()` hands the tracer in `proxy_handler_inner`, and the
    // dashboard renderer (Task 14) keys off `chosen`/`scored`/`rejected` by
    // name. A silent field rename here must fail this test rather than show up
    // as a blank panel later.
    #[test]
    fn auto_route_explanation_serializes_with_dashboard_keys() {
        let candidates = vec![Candidate {
            model: minimal_model("solo"),
            healthy: true,
            levels: Vec::new(),
        }];
        let (picked, mut explain) = crate::router::route(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            &Intent::default(),
        );
        assert_eq!(picked.map(|m| m.model_name), Some("solo".to_string()));
        // Mirrors the overwrite the data plane performs before recording the
        // span (see the `auto` branch of `proxy_handler_inner`).
        explain.classifier_ms = 7;

        let value = serde_json::to_value(&explain).expect("route explanation must serialize");
        let obj = value
            .as_object()
            .expect("explanation must serialize to a JSON object");
        assert!(obj.contains_key("chosen"), "missing `chosen`: {obj:?}");
        assert!(obj.contains_key("scored"), "missing `scored`: {obj:?}");
        assert!(obj.contains_key("rejected"), "missing `rejected`: {obj:?}");
        assert_eq!(value["classifier_ms"], serde_json::json!(7));
    }

    #[test]
    fn admit_request_carries_key_identity_and_per_model_caps() {
        let mut resolved = key_with_schedule("UTC", None, None, None);
        resolved.key_id = Uuid::new_v4();
        resolved.tenant_id = Uuid::new_v4();
        resolved.fairshare_group = "grp".into();
        resolved.group_weight = 7;
        resolved.key_weight = 250;
        resolved.key_max_in_flight = Some(2);
        resolved.max_in_flight = Some(5);
        let mut route = minimal_model("m");
        route.max_in_flight = Some(9);
        let req = admit_request_for(&resolved, "m", Some(&route), 77, 123);
        assert_eq!(req.tenant, resolved.tenant_id);
        assert_eq!(req.key, resolved.key_id);
        assert_eq!(req.weight, 77);
        assert_eq!(req.key_weight, 250);
        assert_eq!(req.group, resolved.fairshare_group);
        assert_eq!(req.group_weight, resolved.group_weight);
        assert_eq!(req.model, "m");
        assert_eq!(req.model_max_in_flight, Some(9));
        assert_eq!(req.tenant_max_in_flight, Some(5));
        assert_eq!(req.key_max_in_flight, Some(2));
        assert_eq!(req.cost, 123);

        // Zero and negative caps mean "no cap".
        resolved.max_in_flight = Some(0);
        resolved.key_max_in_flight = Some(-1);
        let req = admit_request_for(&resolved, "m", None, 1, 1);
        assert_eq!(req.tenant_max_in_flight, None);
        assert_eq!(req.key_max_in_flight, None);
        assert_eq!(req.model_max_in_flight, None);
    }
    /// Endpoints whose OpenAI spec sends `multipart/form-data` (they take a
    /// file upload). The video create's upload is its optional
    /// `input_reference` frame; it also accepts JSON.
    const SPEC_MULTIPART_PATHS: &[&str] = &[
        "/v1/audio/transcriptions",
        "/v1/audio/translations",
        "/v1/images/edits",
        "/v1/images/variations",
        "/v1/videos",
    ];

    #[test]
    fn every_registered_multipart_endpoint_parses_its_form() {
        // The two lists drifted once: the image upload endpoints required a
        // model but their form was never parsed, so the model field was never
        // seen and every request failed "model is required".
        for path in REGISTERED_MODEL_PATHS {
            assert_eq!(
                is_multipart_endpoint(path),
                SPEC_MULTIPART_PATHS.contains(path),
                "{path}"
            );
        }
        for path in SPEC_MULTIPART_PATHS {
            assert!(requires_registered_model(path), "{path}");
        }
    }

    fn image_edit_form() -> (String, Bytes) {
        let boundary = "XBOUNDARYX".to_string();
        let body = format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nimage-model\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\nadd a hat\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"n\"\r\n\r\n3\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"image\"; filename=\"cat.png\"\r\n\
             Content-Type: image/png\r\n\r\nPNGDATA\r\n\
             --{b}--\r\n",
            b = boundary
        );
        (boundary, Bytes::from(body))
    }

    #[tokio::test]
    async fn an_image_edit_form_reads_like_a_json_body() {
        let (boundary, body) = image_edit_form();
        let fields = parse_multipart(&body, &boundary)
            .await
            .expect("form parses");
        let view = multipart_text_view(&fields);
        // The file is not text the scanners or the estimate should read.
        assert_eq!(
            view,
            serde_json::json!({"model": "image-model", "prompt": "add a hat", "n": "3"})
        );

        // Priced per image from the form's `n`, like a JSON generation.
        let mut route = minimal_model("image-model");
        route.model_type = "image".into();
        route.cost_per_image = 0.04;
        assert!((compute_modality_cost(Some(&route), &view) - 0.12).abs() < 1e-9);
        let json_body = serde_json::json!({"prompt": "x", "n": 2});
        assert!((compute_modality_cost(Some(&route), &json_body) - 0.08).abs() < 1e-9);
        let unset = serde_json::json!({"prompt": "x"});
        assert!((compute_modality_cost(Some(&route), &unset) - 0.04).abs() < 1e-9);
    }

    #[test]
    fn a_video_model_is_its_own_modality() {
        use super::{
            input_guardrails_unscannable, mode_for_model_type, model_info_entry,
            output_guardrails_unenforceable,
        };
        let mut route = minimal_model("video-model");
        route.model_type = "video".into();
        route.cost_per_video = 0.3;
        // One flat price per created job, whatever the body asks for.
        let body = serde_json::json!({"prompt": "a fox", "n": 4, "seconds": "5"});
        assert!((compute_modality_cost(Some(&route), &body) - 0.3).abs() < 1e-9);
        assert_eq!(mode_for_model_type("video"), "video_generation");
        let info = model_info_entry(&route, true);
        assert_eq!(info["model_info"]["mode"], "video_generation");
        assert_eq!(info["model_info"]["cost_per_video"], 0.3);

        // The create takes JSON or a form, must name a registered model, and
        // is its own request class, which the input scanner reads and an
        // output policy does not refuse (a video object is not model text).
        assert!(is_multipart_endpoint("/v1/videos"));
        assert!(requires_registered_model("/v1/videos"));
        assert_eq!(request_type_for_path("/v1/videos"), "video");
        let mut key = crate::boons::test_support::test_key();
        key.guardrails_policy = Some(obleth_config::GuardrailsPolicy {
            action: obleth_config::GuardrailsAction::Block,
            input_scanners: vec!["pii".into()],
            output_scanners: vec!["pii".into()],
            guard_model: None,
            ban_keywords: vec![],
            fail_open: true,
        });
        assert!(!input_guardrails_unscannable(&key, "/v1/videos"));
        assert!(!output_guardrails_unenforceable(&key, "/v1/videos"));
        // Follow-ups never reach the pipeline; were one to, it is unmapped.
        assert_eq!(request_type_for_path("/v1/videos/video_1/content"), "other");
    }

    #[tokio::test]
    async fn a_redacted_prompt_is_what_the_form_forwards() {
        let (boundary, body) = image_edit_form();
        let mut fields = parse_multipart(&body, &boundary)
            .await
            .expect("form parses");
        let mut view = multipart_text_view(&fields);
        view["prompt"] = serde_json::json!("add a [REDACTED]");
        apply_multipart_text_view(&mut fields, &view);

        let prompt = fields.iter().find(|f| f.name == "prompt").unwrap();
        assert_eq!(prompt.data.as_ref(), b"add a [REDACTED]");
        // The file part and the untouched fields keep their bytes.
        let image = fields.iter().find(|f| f.name == "image").unwrap();
        assert_eq!(image.data.as_ref(), b"PNGDATA");
        assert_eq!(image.file_name.as_deref(), Some("cat.png"));
        let n = fields.iter().find(|f| f.name == "n").unwrap();
        assert_eq!(n.data.as_ref(), b"3");
    }

    #[test]
    fn a_repeated_text_field_is_scanned_and_redacted_in_every_occurrence() {
        let text = |name: &str, data: &str| MultipartField {
            name: name.into(),
            file_name: None,
            content_type: None,
            data: Bytes::from(data.to_string()),
        };
        let mut fields = vec![text("prompt", "first"), text("prompt", "second")];
        let mut view = multipart_text_view(&fields);
        assert_eq!(view["prompt"], serde_json::json!(["first", "second"]));
        view["prompt"][1] = serde_json::json!("[REDACTED]");
        apply_multipart_text_view(&mut fields, &view);
        assert_eq!(fields[0].data.as_ref(), b"first");
        assert_eq!(fields[1].data.as_ref(), b"[REDACTED]");
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    /// Source with all whitespace removed, so pins survive reformatting.
    fn squash(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// `proxy_handler_inner`'s source, for ordering pins (there is no
    /// AppState harness in this crate: it needs live Redis and ClickHouse).
    fn handler() -> &'static str {
        let src = include_str!("proxy.rs");
        let src = &src[..src.find("\nmod tests {").expect("the test module")];
        let start = src
            .find("async fn proxy_handler_inner(")
            .expect("proxy_handler_inner");
        let end = start
            + src[start..]
                .find("\n/// Drops the usage-only SSE event")
                .expect("end of the handler");
        &src[start..end]
    }

    #[test]
    fn admission_precedes_the_budget_reservation() {
        let h = handler();
        let admit = h.find(".admit_to(").expect("admission");
        let reserve = h.find(".reserve_with_term(").expect("reservation");
        assert!(admit < reserve, "a queued request must hold no budget");
        assert!(h[admit..reserve].contains("timeout(admit_wait"));
    }

    #[test]
    fn every_exit_after_the_reservation_settles_through_the_guard() {
        let h = handler();
        let guard = h
            .find("let mut settle_guard = accounting.unbilled_guard();")
            .expect("guard");
        let dispatch = h.find("// ---- proxy upstream ----").expect("dispatch");
        assert!(guard < dispatch, "the guard must exist before dispatch");
        let after = &h[guard..];
        for bypass in ["finalize(", "settle_request(", "cancellation_guard("] {
            assert!(
                !after.contains(bypass),
                "`{bypass}` after the reservation skips the guarded settlement"
            );
        }
        // The fallback switches to the estimate only once the upstream answered
        // successfully, i.e. after the error branch returned.
        let flat = squash(after);
        let error_branch = flat.find("ifstatus_code>=400{").expect("error branch");
        let armed = flat.find("accounting.arm_estimate(&mutsettle_guard);//Cacheonly");
        assert!(armed.is_some_and(|a| a > error_branch));
    }

    #[test]
    fn a_rejection_between_the_two_scopes_releases_the_key_hold() {
        let flat = squash(handler());
        let key = flat
            .find(".reserve_with_term(&resolved.key_id,")
            .expect("key reserve");
        let tenant = flat
            .find(".reserve_with_term(&resolved.tenant_id,")
            .expect("tenant reserve");
        let proceed = flat
            .find("letaccounting=StreamAccounting{")
            .expect("accounting");
        assert!(key < tenant);
        let releases = flat[tenant..proceed]
            .matches("hold.release().await")
            .count();
        // Rate limited, term exhausted, and fail-closed error.
        assert_eq!(releases, 3);
        // Success hands the hold to settlement before the guard exists.
        assert!(flat[tenant..proceed].contains("hold.hand_off()"));
    }

    #[test]
    fn a_dropped_pending_hold_is_released_but_a_handed_off_one_is_not() {
        let full = include_str!("proxy.rs");
        let src = squash(&full[..full.find("\nmod tests {").expect("the test module")]);
        let drop_impl = src
            .find("implDropforPendingTermHold{fndrop(&mutself){ifself.armed{tokio::spawn(self.release_task());")
            .is_some();
        assert!(drop_impl, "an armed hold must release itself on drop");
        assert!(src.contains("fnhand_off(mutself){self.armed=false;}"));
    }

    #[test]
    fn admission_timeout_defaults_and_parses() {
        assert_eq!(parse_admission_timeout(None), DEFAULT_ADMISSION_TIMEOUT);
        assert_eq!(
            parse_admission_timeout(Some("abc")),
            DEFAULT_ADMISSION_TIMEOUT
        );
        assert_eq!(
            parse_admission_timeout(Some("0")),
            DEFAULT_ADMISSION_TIMEOUT
        );
        assert_eq!(
            parse_admission_timeout(Some(" 12 ")),
            Duration::from_secs(12)
        );
    }

    #[test]
    fn estimated_cost_prices_the_estimate_like_settlement_prices_usage() {
        let est = CostEstimate {
            input_tokens: 100,
            estimated_output_tokens: 50,
        };
        let cost = estimated_cost(est, 0.01, 0.02, 0.5);
        assert!((cost - (1.0 + 1.0 + 0.5)).abs() < 1e-9);
    }

    #[test]
    fn stream_usage_is_requested_only_where_upstreams_honour_it() {
        assert!(is_stream_usage_path("/v1/chat/completions"));
        assert!(is_stream_usage_path("/v1/completions"));
        assert!(!is_stream_usage_path("/v1/embeddings"));
        assert!(!is_stream_usage_path("/v1/audio/transcriptions"));
    }

    fn sse(v: serde_json::Value) -> String {
        format!("data: {v}\n\n")
    }

    fn content_event(text: &str) -> String {
        sse(serde_json::json!({
            "choices": [{ "index": 0, "delta": { "content": text } }],
            "usage": null,
        }))
    }

    fn usage_event() -> String {
        sse(serde_json::json!({
            "choices": [],
            "usage": { "prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10 },
        }))
    }

    fn run_filter(chunks: &[&[u8]]) -> String {
        let mut filter = UsageChunkFilter::default();
        let mut out = Vec::new();
        for c in chunks {
            out.extend_from_slice(&filter.push(Bytes::copy_from_slice(c)));
        }
        out.extend_from_slice(&filter.finish());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn the_injected_usage_chunk_is_stripped_and_content_passes_verbatim() {
        let a = content_event("Hel");
        let b = content_event("lo");
        let u = usage_event();
        let done = "data: [DONE]\n\n";
        let out = run_filter(&[a.as_bytes(), b.as_bytes(), u.as_bytes(), done.as_bytes()]);
        assert_eq!(out, format!("{a}{b}{done}"));
    }

    #[test]
    fn usage_split_across_chunks_is_still_stripped() {
        let a = content_event("hi");
        let u = usage_event();
        let done = "data: [DONE]\n\n";
        let joined = format!("{a}{u}{done}");
        let bytes = joined.as_bytes();
        // Split inside the usage event and inside the blank-line terminator.
        let cut1 = a.len() + 20;
        let cut2 = a.len() + u.len() - 1;
        let out = run_filter(&[&bytes[..cut1], &bytes[cut1..cut2], &bytes[cut2..]]);
        assert_eq!(out, format!("{a}{done}"));
        // CRLF-framed streams too.
        let crlf = joined.replace("\n\n", "\r\n\r\n");
        assert_eq!(
            run_filter(&[crlf.as_bytes()]),
            format!("{a}{done}").replace("\n\n", "\r\n\r\n")
        );
    }

    #[test]
    fn a_body_past_the_cap_with_no_event_boundary_is_flushed_unfiltered() {
        // A non-SSE (or non-compliant) body that never sends a blank-line
        // terminator must not be held for the life of the stream once it
        // exceeds the cap.
        let body = vec![b'x'; USAGE_FILTER_MAX_PENDING + 1];
        let mut filter = UsageChunkFilter::default();
        let out = filter.push(Bytes::copy_from_slice(&body));
        assert_eq!(out.as_ref(), body.as_slice());
        assert!(filter.pending.is_empty());
        // The filter keeps working on whatever follows the flush (it does not
        // latch into a permanently-disabled state); a later complete event
        // is still filtered normally.
        let done = "data: [DONE]\n\n";
        let mut tail = Vec::new();
        tail.extend_from_slice(&filter.push(Bytes::from_static(done.as_bytes())));
        tail.extend_from_slice(&filter.finish());
        assert_eq!(tail, done.as_bytes());
    }

    #[test]
    fn usage_riding_on_a_content_chunk_is_not_dropped() {
        let e = sse(serde_json::json!({
            "choices": [{ "index": 0, "delta": { "content": "x" } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
        }));
        assert_eq!(run_filter(&[e.as_bytes()]), e);
    }

    #[test]
    fn a_stream_cut_off_after_two_deltas_bills_the_delivered_text() {
        let est = CostEstimate {
            input_tokens: 30,
            estimated_output_tokens: 500,
        };
        // Two content deltas reach the client, then the upstream errors with
        // no usage chunk ever sent.
        let chunks = [content_event("Hello, wor"), content_event("ld and more")];
        let streamed: usize = chunks.iter().map(|c| delta_text_chars(c.as_bytes())).sum();
        assert_eq!(streamed, "Hello, world and more".chars().count());
        let usage = extract_usage(&chunks.concat());
        assert_eq!(usage, None);
        let (tokens, billed) = truncated_stream_billing(usage, streamed, est);
        assert!(billed, "delivered text must be billed");
        assert_eq!(tokens, (30, 5), "prompt estimate + ~4 chars per token");

        // Reported usage wins; nothing streamed is nothing billed.
        assert_eq!(
            truncated_stream_billing(Some((7, 3)), streamed, est),
            ((7, 3), true)
        );
        assert_eq!(truncated_stream_billing(None, 0, est), ((0, 0), false));
    }

    #[test]
    fn delta_text_counting_handles_escapes_multibyte_and_null() {
        let e = sse(serde_json::json!({
            "choices": [{ "delta": { "content": "a\"b\né", "reasoning_content": "hm" } }],
        }));
        // a " b \n é = 5, plus "hm" = 2.
        assert_eq!(delta_text_chars(e.as_bytes()), 7);
        let null = content_event("");
        assert_eq!(delta_text_chars(null.as_bytes()), 0);
        assert_eq!(delta_text_chars(br#"{"content": null}"#), 0);
    }

    #[test]
    fn delta_text_counting_does_not_double_count_mirrored_reasoning() {
        // Some upstreams echo the same reasoning text under both
        // `reasoning_content` and `reasoning` in one delta. Only
        // `reasoning_content` should count.
        let e = sse(serde_json::json!({
            "choices": [{
                "delta": {
                    "content": "hi",
                    "reasoning_content": "thinking",
                    "reasoning": "thinking",
                },
            }],
        }));
        // "hi" = 2, plus "thinking" once = 8.
        assert_eq!(delta_text_chars(e.as_bytes()), 10);

        // Without `reasoning_content`, `reasoning` still counts on its own.
        let fallback = sse(serde_json::json!({
            "choices": [{ "delta": { "reasoning": "thinking" } }],
        }));
        assert_eq!(delta_text_chars(fallback.as_bytes()), 8);
    }

    #[test]
    fn unbilled_settlement_still_charges_slot_energy() {
        let energy = crate::energy::EnergyEngine::new(obleth_config::EnergySettings {
            enabled: true,
            prometheus_url: "http://prom".into(),
            power_query: "watts".into(),
            poll_interval_secs: 60,
            energy_cost_per_kwh: 0.10,
            carbon_g_per_kwh: 400.0,
            pue: 1.0,
        });
        energy.store_reading(crate::energy::PowerReading {
            cluster_watts: 409_000.0,
            node_count: 178,
            at_ms: 0,
        });
        // A 503 that produced nothing but held a slot for 2 s.
        let (cost, figures) =
            settled_figures(&energy, false, (0, 0), (0.01, 0.02, 0.5), 8, 2_000, 0);
        assert_eq!(cost, 0.0, "no billable output, no cost");
        assert!(figures.energy_wh > 0.0, "slot time is still energy");
        let (billed_cost, _) =
            settled_figures(&energy, true, (100, 50), (0.01, 0.02, 0.5), 8, 2_000, 0);
        assert!((billed_cost - 2.5).abs() < 1e-9);
    }

    #[test]
    fn completion_body_usage_reads_openai_usage() {
        let body = serde_json::json!({ "usage": { "prompt_tokens": 4, "completion_tokens": 9 } });
        assert_eq!(completion_body_usage(&body), Some((4, 9)));
        assert_eq!(completion_body_usage(&serde_json::json!({})), None);
    }

    #[test]
    fn upstream_headers_go_on_after_the_forwarded_ones_and_before_auth() {
        let src = squash(handler());
        let fwd = src
            .find("letmutfwd_headers=forward_headers(&headers);")
            .unwrap();
        let ext = src
            .find("fwd_headers.extend(target.headers.clone());")
            .unwrap();
        let auth = src
            .find("fwd_headers.insert(header::AUTHORIZATION,v);")
            .unwrap();
        assert!(fwd < ext && ext < auth);
    }

    #[test]
    fn the_form_stands_in_for_the_body_before_guardrails_and_pricing() {
        let src = squash(handler());
        let view = src
            .find("json=multipart_text_view(fields);")
            .expect("the form's text fields replace the JSON body");
        let scan = src
            .find(".enrich_request(")
            .expect("the boon/guardrail pass");
        let price = src
            .find("compute_modality_cost(route.as_deref(),&json)")
            .expect("the modality price");
        assert!(view < scan && view < price);
        let write_back = src
            .find("apply_multipart_text_view(fields,&json);")
            .expect("a redaction reaches the forwarded form");
        assert!(scan < write_back);
    }
}
