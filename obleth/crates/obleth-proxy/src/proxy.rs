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
use obleth_config::{
    Admission, KeyRejection, KeyRejectionReason, ResolvedKey, ResolvedModel, UsageRecord,
};
use obleth_tokenizer::{CostEstimate, Tokenizer};
use tokio::time::timeout;
use tracing::Instrument;
use uuid::Uuid;

use crate::state::AppState;
pub(crate) use obleth_admin::{build_targets, UpstreamTarget as Target};

mod discovery;
mod multipart;
mod paths;
mod settle;
mod surfaces;
mod usage_stream;

pub(crate) use discovery::*;
use multipart::*;
pub(crate) use paths::*;
pub(crate) use settle::*;
use surfaces::*;
use usage_stream::*;

/// Largest request body any surface accepts: `/v1/chat/completions`, the
/// Responses and Messages shims, and the file-free batch and job calls read
/// through [`read_body`], so one limit and one answer hold everywhere.
pub(crate) const BODY_LIMIT: usize = 64 * 1024 * 1024;
/// The message of the 413 every surface gives a body over [`BODY_LIMIT`].
pub(crate) const BODY_TOO_LARGE: &str = "request body too large";
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
/// Message of the 503 for a model lookup Redis could not answer.
const MODEL_REGISTRY_UNAVAILABLE: &str = "model registry unavailable; try again shortly";

/// `Retry-After` sent with a 503 the client should retry: an admission
/// timeout, or a registry lookup Redis could not answer (see [`unavailable`]).
pub(crate) const ADMISSION_RETRY_AFTER_SECS: &str = "5";

static ADMISSION_TIMEOUT: std::sync::OnceLock<Duration> = std::sync::OnceLock::new();

/// Use the gateway's bound on a request's wait in the fairshare queue
/// (`Config::admission_timeout`). Called once at start.
pub(crate) fn install_admission_timeout(timeout: Duration) {
    if ADMISSION_TIMEOUT.set(timeout).is_err() {
        tracing::warn!("the admission timeout was already in use; keeping it");
    }
}

/// The bound on a request's wait in the fairshare queue: without one, a
/// saturated pool parks callers indefinitely. 60 s before
/// [`install_admission_timeout`] (a test).
pub(crate) fn admission_timeout() -> Duration {
    *ADMISSION_TIMEOUT.get_or_init(|| Duration::from_secs(60))
}

pub async fn proxy_handler(state: State<AppState>, mut req: Request<Body>) -> Response<Body> {
    let request_id = Uuid::new_v4();
    if let Some(canonical) = canonical_path(req.method(), req.uri().path()) {
        let pq = match req.uri().query() {
            Some(q) => format!("{canonical}?{q}"),
            None => canonical,
        };
        if let Ok(uri) = pq.parse() {
            *req.uri_mut() = uri;
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

/// A request body over [`BODY_LIMIT`]: the one thing `to_bytes` fails on,
/// since a malformed transport read surfaces earlier, from the connection
/// itself. Each surface renders it in its own error shape, with one status
/// and one message.
pub(crate) struct BodyTooLarge;

impl BodyTooLarge {
    /// The OpenAI-shaped 413 (`/v1/chat/completions`, `/v1/responses`, batches, jobs).
    pub(crate) fn openai(self) -> Response<Body> {
        error_json(StatusCode::PAYLOAD_TOO_LARGE, BODY_TOO_LARGE)
    }
    /// The Anthropic-shaped 413 (`/v1/messages`).
    fn anthropic(self) -> Response<Body> {
        anthropic_error(StatusCode::PAYLOAD_TOO_LARGE, BODY_TOO_LARGE, None)
    }
}

/// The whole request body, or [`BodyTooLarge`] past [`BODY_LIMIT`].
pub(crate) async fn read_body(body: Body) -> Result<Bytes, BodyTooLarge> {
    axum::body::to_bytes(body, BODY_LIMIT)
        .await
        .map_err(|_| BodyTooLarge)
}

/// Background admission for a batch item: its share of the model and the
/// longest it may wait for a slot.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct BackgroundShare {
    pub share: f64,
    pub admission_wait: Duration,
}

/// What an in-process caller (a shim, the batch runner) already knows about
/// the request it hands to the pipeline, as a request extension. The pipeline
/// takes each piece from here instead of doing it again, and none of it rides
/// on a header a client could set or strip. Only in-process code inserts it;
/// it never arrives over HTTP.
#[derive(Clone, Default)]
pub(crate) struct PipelineInput {
    /// The surface the caller used (`responses`, `messages`); chat when none.
    pub surface: Option<&'static str>,
    /// The effort the surface spelled in fields chat has no name for.
    pub effort: Option<crate::reasoning::Requested>,
    /// The credential the caller already authenticated and gated.
    pub credential: Option<crate::jwt_auth::Credential>,
    /// The translated body, so the pipeline parses nothing.
    pub body: Option<serde_json::Value>,
    /// A batch item's background admission.
    pub batch: Option<BackgroundShare>,
}

impl PipelineInput {
    /// The input an in-process caller set, or nothing for a request over HTTP.
    pub(crate) fn of(extensions: &http::Extensions) -> PipelineInput {
        extensions
            .get::<PipelineInput>()
            .cloned()
            .unwrap_or_default()
    }
}

/// How far over its context window a prompt's estimate must be before the
/// gateway refuses it itself. The estimate (~4 characters a token) runs low
/// on code and non-Latin text but high on long words and runs of
/// whitespace (a list of common words came out at 6.6 characters a token),
/// so only a prompt it puts at twice the window is sure not to fit.
const SURE_OVERFLOW_FACTOR: u64 = 2;

/// The model's context window, when the prompt's estimate is far enough over
/// it that the model is sure to refuse; `None` when it may fit or the window
/// is unknown.
fn sure_overflow(context_window: i64, prompt_estimate: u32) -> Option<u64> {
    let window = u64::try_from(context_window).ok().filter(|&w| w > 0)?;
    (u64::from(prompt_estimate) >= window * SURE_OVERFLOW_FACTOR).then_some(window)
}

/// `400` for a prompt over the model's context window, in the OpenAI error
/// shape and with OpenAI's code for it, which SDKs and agents key on.
fn context_overflow_response(message: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": {
        "message": message,
        "type": "invalid_request_error",
        "param": "messages",
        "code": "context_length_exceeded",
    }});
    (StatusCode::BAD_REQUEST, axum::Json(body)).into_response()
}

/// Response header naming the variant a request was served through (see
/// [`obleth_config::ModelVariant`]), on every response after the name resolved.
pub(crate) const MODEL_VARIANT_HEADER: &str = "x-obleth-model-variant";
/// Milliseconds the request waited in the fairshare queue before admission. Sent on every
/// admitted response, so a client can tell time spent queued from time the model took.
pub(crate) const QUEUE_WAIT_HEADER: &str = "x-obleth-queue-wait-ms";

/// What every answer built after admission says about itself: the request
/// id, how long it queued, that proxies must not buffer it, and the boons
/// and compression that shaped it. One builder, so no exit forgets a header.
struct Answer<'a> {
    request_id: Uuid,
    queue_wait_ms: u32,
    boons_applied: &'a [&'static str],
    compression: Option<&'a str>,
}

impl Answer<'_> {
    fn respond(
        &self,
        status: impl Into<u16>,
        content_type: header::HeaderValue,
        body: Body,
    ) -> Response<Body> {
        let mut builder = Response::builder()
            .status(status.into())
            .header(header::CONTENT_TYPE, content_type)
            .header("x-obleth-request-id", self.request_id.to_string())
            .header(QUEUE_WAIT_HEADER, self.queue_wait_ms.to_string())
            .header(NO_BUFFER_HEADER.0, NO_BUFFER_HEADER.1);
        if !self.boons_applied.is_empty() {
            builder = builder.header(crate::boons::BOONS_HEADER, self.boons_applied.join(","));
            if let Some(h) = self.compression {
                builder = builder.header(crate::boons::COMPRESSION_HEADER, h);
            }
        }
        builder.body(body).unwrap_or_else(|_| {
            error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
        })
    }
}

/// `value` as a header, or `application/json` when it is not one.
fn content_type_header(value: &str) -> header::HeaderValue {
    header::HeaderValue::from_str(value)
        .unwrap_or_else(|_| header::HeaderValue::from_static("application/json"))
}

/// The pipeline every proxied request runs ([`run_pipeline`]), plus the
/// response headers that describe how it was served, whichever exit the
/// pipeline took.
async fn proxy_handler_inner(
    state: State<AppState>,
    req: Request<Body>,
    request_id: Uuid,
) -> Response<Body> {
    let mut variant: Option<String> = None;
    let mut lifecycle: Option<crate::lifecycle::Notice> = None;
    let mut reasoning: Option<crate::reasoning::Note> = None;
    let mut auto_note: Option<crate::sticky::Note> = None;
    // Boxed: the pipeline's state is the largest future in the gateway, and
    // on the heap it cannot overflow a 2 MB worker or test thread stack in
    // debug builds.
    let resp = Box::pin(run_pipeline(
        state,
        req,
        request_id,
        &mut variant,
        &mut lifecycle,
        &mut reasoning,
        &mut auto_note,
    ))
    .await;
    let mut resp = with_variant_header(resp, variant.as_deref());
    if let Some(notice) = &lifecycle {
        crate::lifecycle::apply_headers(&mut resp, notice);
    }
    if let Some(note) = &reasoning {
        note.apply_headers(&mut resp);
    }
    if let Some(note) = &auto_note {
        note.apply_headers(&mut resp);
    }
    resp
}

/// Run a request the gateway makes for itself (a batch item) through the
/// pipeline, as [`proxy_handler`] does for one from a client.
pub(crate) async fn run_internal(
    state: AppState,
    req: Request<Body>,
    request_id: Uuid,
) -> Response<Body> {
    proxy_handler_inner(State(state), req, request_id).await
}

/// `resp` with [`MODEL_VARIANT_HEADER`] set when the request named a variant.
fn with_variant_header(mut resp: Response<Body>, variant: Option<&str>) -> Response<Body> {
    if let Some(value) = variant.and_then(|v| header::HeaderValue::from_str(v).ok()) {
        resp.headers_mut().insert(MODEL_VARIANT_HEADER, value);
    }
    resp
}

/// A route as the client named it: the route to serve the request with, and
/// the variant name the client used, if the name was one.
///
/// A variant is served by its parent route with the variant's boons added
/// ([`ResolvedModel::with_variant`]). Every other field, `model_name` first,
/// stays the parent's, so the admission pool, budgets, prices and the usage
/// ledger key on one model however many names it answers to.
pub(crate) fn serve_as(
    route: Arc<ResolvedModel>,
    requested: &str,
) -> (Arc<ResolvedModel>, Option<String>) {
    match route.with_variant(requested) {
        Some(served) => (Arc::new(served), Some(requested.to_string())),
        None => (route, None),
    }
}

/// [`serve_as`], with the variant's route kept in `cache` (the gateway's
/// `variant_cache`) under the variant's name, so it is built once.
pub(crate) async fn serve_variant(
    cache: &moka::future::Cache<String, Arc<ResolvedModel>>,
    route: Arc<ResolvedModel>,
    requested: &str,
) -> (Arc<ResolvedModel>, Option<String>) {
    if route.variant(requested).is_none() {
        return (route, None);
    }
    if let Some(served) = cache.get(requested).await {
        return (served, Some(requested.to_string()));
    }
    let (served, variant) = serve_as(route, requested);
    cache.insert(requested.to_string(), served.clone()).await;
    (served, variant)
}

/// How a request is admitted as background work: the share of the model's
/// slots it may fill and the longest it waits for one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BackgroundAdmission {
    pub share: f64,
    pub wait: Duration,
}

/// Background admission for a batch item (its own share and wait) or for a
/// routed request from a key with background priority (the batch runner's
/// share and longest wait, from `key_share`). `None` for everything else,
/// which is admitted as interactive work.
pub(crate) fn background_admission(
    route: Option<&ResolvedModel>,
    batch: Option<&BackgroundShare>,
    resolved: &ResolvedKey,
    key_share: impl FnOnce() -> f64,
) -> Option<BackgroundAdmission> {
    match batch {
        Some(b) => Some(BackgroundAdmission {
            share: b.share,
            wait: b.admission_wait,
        }),
        None if resolved.background && route.is_some() => Some(BackgroundAdmission {
            share: key_share(),
            wait: crate::batches::MAX_ADMISSION_WAIT,
        }),
        None => None,
    }
}

/// The pool a routed request is admitted to: one per model, keyed by the
/// route's own `model_name`, so a variant queues with its parent.
pub(crate) fn admission_pool(route: Option<&ResolvedModel>) -> obleth_fairshare::PoolKey {
    match route {
        Some(r) => obleth_fairshare::PoolKey::Model(r.model_name.clone()),
        None => obleth_fairshare::PoolKey::Unrouted,
    }
}

#[tracing::instrument(
    skip_all,
    name = "proxy_request",
    fields(session.id = tracing::field::Empty, session.id.source = tracing::field::Empty)
)]
async fn run_pipeline(
    State(state): State<AppState>,
    req: Request<Body>,
    request_id: Uuid,
    served_variant: &mut Option<String>,
    lifecycle_notice: &mut Option<crate::lifecycle::Notice>,
    reasoning_note: &mut Option<crate::reasoning::Note>,
    auto_note: &mut Option<crate::sticky::Note>,
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
    // What an in-process caller already did: a shim's credential, surface,
    // effort, body and estimate; a batch item's owner and background share.
    let mut input = PipelineInput::of(&parts.extensions);
    let batch = input.batch;
    let presented = input.credential.is_some();

    // ---- reject path traversal before any upstream work ----
    if has_path_traversal(&path) {
        return error_json(StatusCode::BAD_REQUEST, "invalid request path");
    }

    // ---- auth ----
    let auth_start = crate::tracer::now_ms();
    let (resolved, device_id, auth_kind) = match input.credential.take() {
        Some(c) => (c.resolved, c.device_id, c.auth_kind),
        None => {
            let Some(secret) = bearer(&headers) else {
                return error_json(StatusCode::UNAUTHORIZED, "missing bearer token");
            };
            // Shared with the MCP handler (`crate::mcp::mcp_handler`) so both
            // surfaces authenticate identically: JWT path when the feature is
            // on and the credential looks like a JWT, else the existing
            // secret-key path.
            match crate::jwt_auth::authenticate_credential(&state, &secret).await {
                Ok(cred) => (cred.resolved, cred.device_id, cred.auth_kind),
                Err(resp) => return resp,
            }
        }
    };
    let auth_duration = (crate::tracer::now_ms() - auth_start) as u32;
    // A batch item runs for an owner checked when the batch was submitted.
    let client = if batch.is_some() {
        crate::client::ClientInfo::in_process()
    } else {
        crate::client::ClientInfo::of(&parts.extensions)
    };
    // A shim gated the key before translating; a batch item's owner is gated
    // again here, as in-process work.
    if !presented || batch.is_some() {
        if let Err(resp) = gate_resolved_key(&state, &resolved, &client) {
            return resp;
        }
    }

    // ---- job follow-ups: videos, meshes (poll, download, delete, list) ----
    // They carry a job id, never a model, so they are served from the job
    // record written at create time: routed to the model and endpoint that
    // made the job, and "not found" for anyone who does not own it (another
    // tenant, or by default another key of this one). Reads of work the
    // create already paid for, so no admission, budget, or ledger row (see
    // `crate::jobs`).
    if let Some((kind, call)) = crate::jobs::follow_up(&method, &path) {
        let inbound = crate::jobs::Inbound {
            method: &method,
            query: &query,
            headers: &headers,
        };
        // Boxed: a follow-up streams a whole file, and its state must not sit
        // inline in every request's future.
        return Box::pin(crate::jobs::handle_follow_up(
            &state, &resolved, kind, call, inbound, request_id,
        ))
        .await;
    }
    let job_create = crate::jobs::is_create(&method, &path);

    // ---- files and batches (when batches are on) ----
    if let (Some(batches), None) = (state.batches.as_ref(), batch.as_ref()) {
        if let Some(route) = crate::batches::route(&method, &path) {
            return crate::batches::handle(
                &state, batches, &resolved, route, &query, &headers, body,
            )
            .await;
        }
    }

    // ---- only known endpoints go on to an upstream ----
    // What is left is forwarded to a model's upstream with the operator's key
    // for it, so it must be a call the gateway knows how to admit, guard, and
    // meter: an inference endpoint (POST) or model discovery (GET). Any other
    // method or path used to be forwarded as it came whenever the body named
    // an allowed model, which reached whatever else that upstream serves
    // (stored completions, files, fine-tunes, a model server's management
    // routes). There is no setting that opens other routes: an endpoint worth
    // serving is added to `REGISTERED_MODEL_PATHS`, where it is admitted and
    // metered like the rest.
    if batch.is_none() && !is_forwarded(&method, &path) {
        return error_json(StatusCode::NOT_FOUND, "unknown endpoint");
    }

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
    // An in-process caller hands over the body it built; its bytes are made
    // once, where the cache key and the upstream need them.
    let body_given = input.body.is_some();
    let (mut body_bytes, mut json) = match input.body.take() {
        Some(json) => (Bytes::new(), json),
        None => {
            let bytes = match read_body(body).await {
                Ok(bytes) => bytes,
                Err(too_large) => return too_large.openai(),
            };
            let json: serde_json::Value =
                serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
            (bytes, json)
        }
    };
    // `stream` decides how the answer is read and what it is billed from. A
    // value that is neither true nor false (`1`, `"true"`) reads as "not a
    // stream" here while a model server may still stream for it, and that
    // answer would be billed from the estimate instead of its usage.
    if json
        .get("stream")
        .is_some_and(|v| !v.is_boolean() && !v.is_null())
    {
        return error_json(StatusCode::BAD_REQUEST, "stream must be true or false");
    }
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
    // Re-serialize so paths that forward the raw bytes match the parsed body.
    if is_chat_path(&path) && strip_tool_strict(&mut json) {
        if let Ok(bytes) = serde_json::to_vec(&json) {
            body_bytes = Bytes::from(bytes);
        }
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
    // The effort the caller asked for, read before the name resolves: a
    // family name needs it to pick the model that answers.
    let requested_effort = crate::reasoning::Requested::read(input.effort.take(), &json);
    // Set when the name was a family's: the level the member serves at, and
    // the family, which the ledger records apart from variants.
    let mut family_level: Option<obleth_config::ReasoningLevel> = None;
    let mut family_name: Option<String> = None;

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
    let end_user = end_user_for(&resolved, &headers, &json);
    let mut req_meta = RequestMeta {
        session_id: conversation.value,
        session_id_source: conversation.source.as_str(),
        request_type: if batch.is_some() {
            obleth_config::BATCH_REQUEST_TYPE
        } else {
            surfaced_request_type(&resolved, &path, input.surface)
        },
        device_id,
        end_user: end_user.clone().unwrap_or_default(),
        // Set below once the model name resolves.
        model_variant: String::new(),
        reasoning_effort: String::new(),
        reasoning_effort_requested: String::new(),
        model_family: String::new(),
        // Set once the upstream answers.
        streamed: false,
        // Set where a failure's status is chosen.
        failure: None,
        crash_fp: String::new(),
        client: client.clone(),
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

    // The routed `auto` turn, stored once it is answered when stickiness is on.
    let mut sticky_turn: Option<crate::sticky::Turn> = None;

    // ---- auto model selection ----
    // `model: "auto"` is resolved to a concrete registered model from request
    // shape (estimated context size, required capabilities) and live load. From
    // here on, everything downstream — admission, budgets, caching, telemetry,
    // upstream dispatch — sees the concrete model as if the client named it.
    // Set when the model's registry lookup failed on Redis rather than missed.
    let mut registry_unavailable = false;
    let (mut route, mut variant) = if model == crate::router::AUTO_MODEL_NAME {
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
        let classify_system = crate::router::classify_system_from_header(
            headers
                .get(crate::router::CLASSIFY_SYSTEM_HEADER)
                .and_then(|v| v.to_str().ok()),
        );
        // With stickiness on, find the model that answered this conversation's
        // last turn before deciding anything.
        let conversation = if router_settings.sticky_enabled {
            // Only a client's own conversation id names one conversation; a
            // derived id is shared by every chat that opens the same way.
            let session_id = (req_meta.session_id_source == SessionSource::Client.as_str())
                .then_some(req_meta.session_id.as_str())
                .filter(|s| !s.is_empty());
            Some(crate::sticky::lookup(&state, &resolved.tenant_id, &json, session_id).await)
        } else {
            None
        };
        let classifier_start = if traced { crate::tracer::now_ms() } else { 0 };
        // The classifier model reads the prompt, and on this path intent is
        // derived before the tenant input guardrails run: under an input
        // policy, route on the header and heuristics instead.
        let skip_classifier = has_input_guardrails(&resolved);
        let (intent, complaint) = tokio::join!(
            derive_intent(
                &state,
                &json,
                est.input_tokens as u64,
                &available_tags,
                &router_settings,
                effort_header,
                classify_system,
                skip_classifier,
            ),
            crate::sticky::complaint(
                &state,
                &router_settings,
                conversation.as_ref(),
                &json,
                skip_classifier,
            ),
        );
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
        // A sticky turn routes over a narrowed pool: the incumbent alone, or
        // the models at or above the conversation's rung.
        let choice = conversation.as_ref().map(|c| {
            let mut observed = c.observed.clone();
            observed.complaint = complaint;
            crate::sticky::client_signal(
                &mut observed,
                headers
                    .get(crate::sticky::AUTO_ESCALATE_HEADER)
                    .and_then(|v| v.to_str().ok()),
            );
            crate::sticky::choose(
                &candidates,
                c.record.as_ref(),
                c.session.as_ref(),
                &intent,
                &observed,
                |pool| {
                    crate::router::select_model(
                        pool,
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
                    .is_some()
                },
            )
        });
        let pool: &[crate::router::Candidate] = choice
            .as_ref()
            .and_then(|c| c.pool.as_deref())
            .unwrap_or(&candidates);
        // Traced requests get the full explanation from a single `route()` call
        // (one `evaluate`, one `uniform` draw) so the span describes exactly the
        // sample that was served. Untraced requests call `select_model` instead,
        // which asks for `Narration::Off` and builds no rejection/score data —
        // that is the whole cost story for this feature: zero by default.
        // `among`: how many models the pick was made among (those scored), for
        // `x-obleth-auto-why`, and the best of them that were not chosen, for
        // `x-obleth-auto-next`.
        let (picked, among) = if let Some(ref mut t) = tracer {
            let (picked, mut explain) = crate::router::route(
                pool,
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
            let among = explain.among();
            let mut span = serde_json::to_value(&explain).unwrap_or_else(|_| {
                serde_json::json!({
                    "chosen": explain.chosen,
                    "error": "explanation failed to serialize",
                })
            });
            if let (Some(c), Some(obj)) = (choice.as_ref(), span.as_object_mut()) {
                obj.insert(
                    "sticky".into(),
                    serde_json::json!({
                        "reason": c.reason.as_str(),
                        "incumbent": conversation
                            .as_ref()
                            .and_then(|c| c.record.as_ref())
                            .map(|r| r.model.clone()),
                        "floor": c.floor,
                        "domains": c.domains,
                        "strikes": c.strikes,
                    }),
                );
            }
            t.record(
                "auto_route",
                "proxy_request",
                auto_start,
                (crate::tracer::now_ms() - auto_start) as u32,
                "ok",
                span,
            );
            (picked, among)
        } else {
            crate::router::select_model_among(
                pool,
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
                let reason = choice.as_ref().map(|c| c.reason);
                state.metrics.record_auto_route(
                    &chosen.model_name,
                    reason.map_or("per_turn", |r| r.as_str()),
                );
                if let (Some(conversation), Some(choice)) = (conversation, choice.as_ref()) {
                    if let Some(c) = pool
                        .iter()
                        .find(|c| c.model.model_name == chosen.model_name)
                    {
                        sticky_turn = Some(crate::sticky::Turn::new(
                            resolved.tenant_id,
                            conversation,
                            choice,
                            c,
                        ));
                    }
                }
                *auto_note = Some(crate::sticky::Note {
                    model: chosen.model_name.clone(),
                    reason,
                    why: crate::sticky::why(&intent, &features, among.count),
                    next: among.next,
                });
                model = chosen.model_name.clone();
                // Candidates are real models only, never variants.
                (Some(chosen), None)
            }
            None => {
                return error_json(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "no model is available to satisfy the auto request",
                );
            }
        }
    } else {
        // A variant resolves to its parent's route with the variant's boons
        // added (`serve_as`); an alias to the parent's route as it is.
        let looked_up = try_resolve_model(&state, &model).await;
        registry_unavailable = looked_up.is_err();
        let (route, variant) = match looked_up.ok().flatten() {
            Some(r) => {
                let (r, variant) = serve_variant(&state.variant_cache, r, &model).await;
                (Some(r), variant)
            }
            // A family answers with the member its level picks, recorded
            // under the family's name the way a variant is.
            None => match state.families.get(&model) {
                Some(family) => {
                    let (level, member) =
                        match crate::reasoning::family_member(&family, &requested_effort) {
                            Ok(picked) => picked,
                            Err(msg) => return error_json(StatusCode::BAD_REQUEST, &msg),
                        };
                    let r = match try_resolve_model(&state, &member).await {
                        Ok(Some(r)) => r,
                        Ok(None) => {
                            return error_json(
                                StatusCode::NOT_FOUND,
                                &format!(
                                    "model family '{model}' sends `{level}` to '{member}', which is not registered"
                                ),
                            )
                        }
                        Err(()) => return unavailable(MODEL_REGISTRY_UNAVAILABLE),
                    };
                    family_level = Some(level);
                    family_name = Some(family.name.clone());
                    (Some(r), Some(family.name.clone()))
                }
                None => (None, None),
            },
        };
        // An alias or a variant resolves to the same route as the canonical
        // name, so adopt the canonical name for everything downstream:
        // admission, budgets, the response cache (a variant keeps entries of
        // its own, below), the per-tenant allowlist, and the usage ledger.
        // This is the same move `auto` makes after it picks — without it, one
        // model's traffic would split across every spelling clients happen to
        // have pinned (and a variant would mint a second pool for the same
        // backend), and a tenant allowed `glm-5-3` would be refused for asking
        // the same model by its old name.
        if let Some(r) = route.as_ref() {
            if r.model_name != model {
                tracing::debug!(requested = %model, model = %r.model_name, "resolved model alias or variant");
                model = r.model_name.clone();
            }
        }
        (route, variant)
    };
    // ---- model lifecycle ----
    // A deprecated model is served with headers saying so; a retired one is
    // refused with 410 Gone, or answered by its replacement when the operator
    // asked for that. A redirect drops the variant: it belongs to the retired
    // model, and the replacement is served as itself. `auto` never gets here
    // with either, since it does not pick them.
    if let Some(r) = route.clone() {
        match crate::lifecycle::gate(&state, &r, chrono::Utc::now()).await {
            crate::lifecycle::Gate::Serve(notice) => *lifecycle_notice = notice,
            crate::lifecycle::Gate::Redirect(next, notice) => {
                tracing::debug!(retired = %model, served = %next.model_name, "retired model redirected to its replacement");
                model = next.model_name.clone();
                route = Some(next);
                variant = None;
                *lifecycle_notice = Some(notice);
            }
            crate::lifecycle::Gate::Refuse(resp, reason) => {
                // Recorded, so the operator can see who still calls the model
                // and tell them before the 410s do.
                record_called_name(&mut req_meta, variant.as_deref(), family_name.as_deref());
                req_meta.failure = Some(obleth_config::Failure::new(
                    obleth_config::ErrorClass::Retired,
                    &reason,
                ));
                reject(
                    &state,
                    request_id,
                    &resolved,
                    &req_meta,
                    &model,
                    Rejection {
                        status: StatusCode::GONE,
                        since: request_start,
                        est: CostEstimate::default(),
                        cache_status: "off",
                        queue_wait_ms: 0,
                    },
                );
                return resp;
            }
        }
    }
    // The name the client used, for its usage row and response headers. The
    // ledger's `model` stays the parent's.
    record_called_name(&mut req_meta, variant.as_deref(), family_name.as_deref());
    served_variant.clone_from(&variant);

    if requires_registered_model(&path) {
        if model == "unknown" {
            return error_json(StatusCode::BAD_REQUEST, "model is required");
        }
        let Some(route) = route.as_ref() else {
            // A Redis fault is not a missing model: a 404 would tell the
            // client to stop asking for a model that is still there.
            if registry_unavailable {
                return unavailable(MODEL_REGISTRY_UNAVAILABLE);
            }
            return error_json(
                StatusCode::NOT_FOUND,
                &format!("model '{model}' is not registered"),
            );
        };
        if !route.enabled {
            return error_json(StatusCode::FORBIDDEN, "model is disabled");
        }
    }
    // A search tool answers only on its own endpoint; forwarding a chat (or
    // any other) body to the search upstream would come back as its 404.
    if route
        .as_ref()
        .is_some_and(|r| r.model_type == obleth_config::SEARCH_MODEL_TYPE)
    {
        return error_json(
            StatusCode::BAD_REQUEST,
            &format!("'{model}' is a search tool: call POST /v1/search"),
        );
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
    // The endpoints the gateway knows have answered above when the model is not
    // registered, so nothing should reach this; it stays as the last line
    // before a request could go to the default upstream.
    if route.is_none() && !is_models_endpoint(&path) {
        return error_json(StatusCode::NOT_FOUND, "unknown endpoint");
    }
    // ---- per-tenant model allowlist (Phase 4) ----
    let permitted = match route.as_deref() {
        Some(route) => model_permitted(&resolved, route, variant.as_deref()),
        None => model_visible(allowed_models_for(&resolved), &model),
    };
    if !permitted {
        return error_json(StatusCode::FORBIDDEN, "model not permitted for tenant");
    }

    // ---- request fields the model refuses ----
    // Checked on the caller's body, before reasoning effort and boons add
    // fields of their own, and before anything is sent.
    if let Some(field) = route
        .as_ref()
        .and_then(|r| obleth_config::rejected_request_field(&json, &r.rejected_request_fields))
    {
        reject(
            &state,
            request_id,
            &resolved,
            &req_meta,
            &model,
            Rejection {
                status: StatusCode::BAD_REQUEST,
                since: request_start,
                est: CostEstimate::default(),
                cache_status: "off",
                queue_wait_ms: 0,
            },
        );
        return rejected_field_response(&model, field);
    }

    // ---- crash guard ----
    // A request that was in flight when one of this model's engines died is
    // held back while the model is down to its last server, and refused after
    // a second death; see `crate::crash_guard`. Fingerprinted on the caller's
    // body, before the gateway adds fields of its own.
    // Synthetic tenants (benchmarks, probes) are left out; see the module.
    if route.is_some() && state.crash_guard.enabled() && !resolved.synthetic {
        req_meta.crash_fp = crate::crash_guard::fingerprint(&resolved.key_id, &model, &json);
        let verdict = state
            .crash_guard
            .check(&state.redis, &model, &req_meta.crash_fp)
            .await;
        if verdict != crate::crash_guard::Verdict::Pass {
            let (status, class, msg) = match verdict {
                crate::crash_guard::Verdict::Hold { ready, usual } => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    obleth_config::ErrorClass::Capacity,
                    crate::crash_guard::hold_message(&model, ready, usual),
                ),
                crate::crash_guard::Verdict::Refuse { strikes } => (
                    StatusCode::UNPROCESSABLE_ENTITY,
                    obleth_config::ErrorClass::ClientInput,
                    crate::crash_guard::refuse_message(&model, strikes),
                ),
                crate::crash_guard::Verdict::Pass => unreachable!(),
            };
            tracing::warn!(%request_id, model = %model, fingerprint = %req_meta.crash_fp, status = status.as_u16(), "crash guard: request not sent");
            req_meta.failure = Some(obleth_config::Failure::new(class, &msg));
            reject(
                &state,
                request_id,
                &resolved,
                &req_meta,
                &model,
                Rejection {
                    status,
                    since: request_start,
                    est: CostEstimate::default(),
                    cache_status: "off",
                    queue_wait_ms: 0,
                },
            );
            return crate::crash_guard::guard_response(status, &msg);
        }
    }

    // ---- reasoning effort ----
    // The caller's level becomes the fields the served model's server
    // understands (or is removed, for a model without effort control). Runs
    // before boons and the cache so both see the body the model gets. A
    // family's level was fixed when the name resolved.
    let mut effort_served: Option<obleth_config::ReasoningLevel> = None;
    if is_chat_path(&path) {
        if let Some(r) = route.as_ref() {
            let level = match family_level {
                Some(level) => Some(level),
                None => match requested_effort.level() {
                    Ok(level) => level,
                    Err(_) if r.reasoning.is_passthrough() => None,
                    Err(msg) => return error_json(StatusCode::BAD_REQUEST, &msg),
                },
            };
            let plan = r.reasoning.plan(level);
            plan.apply_to(&mut json);
            if let obleth_config::ReasoningPlan::Apply { served, .. } = &plan {
                effort_served = Some(*served);
            }
            *reasoning_note = match family_level {
                // Through a family the level picked the model, so it was
                // served even when the member has no effort control of its
                // own; the member's map can still serve it as another.
                Some(level) => Some(crate::reasoning::Note::for_family(
                    level,
                    requested_effort.level().ok().flatten(),
                    &plan,
                )),
                None => crate::reasoning::Note::of(&plan),
            };
            req_meta.reasoning_effort = reasoning_note
                .as_ref()
                .map(crate::reasoning::Note::served_label)
                .unwrap_or_default()
                .to_string();
        }
    }
    // What the caller asked for is logged whatever the model does with it,
    // so a pass-through request still shows the level it carried.
    req_meta.reasoning_effort_requested = match &requested_effort {
        crate::reasoning::Requested::Nothing => String::new(),
        crate::reasoning::Requested::Level(level) => level.as_str().to_string(),
        crate::reasoning::Requested::Unknown(raw) => raw.clone(),
    };

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
        return error_json(
            StatusCode::BAD_REQUEST,
            "this tenant's output guardrails read chat responses only; \
             use /v1/chat/completions, /v1/responses, or /v1/messages",
        );
    }
    if method == Method::POST && input_guardrails_unscannable(&resolved, &path) {
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
            request_id,
            boons_opt_out,
            boons_force_lossy,
            is_chat_path(&path),
            &mut json,
            tracer.as_mut(),
        )
        .await;
    if let Some(block) = boon_outcome.blocked {
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
    let mut response_plan = boon_outcome.response_plan;
    let speculation_plan = boon_outcome.speculation;

    // ---- a prompt the model cannot hold ----
    // Refused here, after boons have had their chance to shrink it, rather
    // than queued for a model that will turn it away.
    if method == Method::POST && is_chat_path(&path) {
        if let Some(window) = route
            .as_deref()
            .and_then(|r| sure_overflow(r.context_window, est.input_tokens))
        {
            // vLLM's own opening, which clients that compact a conversation
            // on a context-length error already look for.
            let message = format!(
                "This model's maximum context length is {window} tokens, and the prompt is \
                 about {} tokens. Shorten the prompt and retry.",
                est.input_tokens
            );
            req_meta.failure = Some(obleth_config::Failure::new(
                obleth_config::ErrorClass::ClientInput,
                &message,
            ));
            reject(
                &state,
                request_id,
                &resolved,
                &req_meta,
                &model,
                Rejection {
                    status: StatusCode::BAD_REQUEST,
                    since: request_start,
                    est,
                    cache_status: "off",
                    queue_wait_ms: 0,
                },
            );
            return context_overflow_response(&message);
        }
    }

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
    // A job create (video, mesh) is never replayed from cache either: an
    // identical body must start a new job, not hand back an id some earlier
    // call recorded.
    let cache_enabled = route.as_ref().map(|r| r.cache_enabled).unwrap_or(false)
        && !tool_loop_armed
        && !output_guardrails_armed
        && job_create.is_none();
    let cache_ttl = route.as_ref().map(|r| r.cache_ttl_secs).unwrap_or(0);
    // TTL <= 0 means "don't cache": nothing is ever written, so a lookup could
    // never hit and would only cost a Redis round-trip.
    let cache_name =
        response_cache_name(&model, variant.as_deref(), effort_served.or(family_level));
    if body_given && body_bytes.is_empty() {
        match serde_json::to_vec(&json) {
            Ok(bytes) => body_bytes = Bytes::from(bytes),
            Err(_) => {
                return error_json(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "request translation failed",
                )
            }
        }
    }
    let cache_key = (cache_enabled && cache_ttl > 0).then(|| {
        obleth_config::cache_key(&resolved.tenant_id.to_string(), &cache_name, &body_bytes)
    });
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
                    FinalizeOutcome {
                        input_tokens: cached.input_tokens,
                        output_tokens: cached.output_tokens,
                        total_ms: request_start.elapsed().as_millis() as u32,
                        status_code: cached.status,
                        cache_status: "hit",
                        cost_usd: (cached.input_tokens as f64) * in_cost_rate
                            + (cached.output_tokens as f64) * out_cost_rate
                            + modality_cost,
                        ..FinalizeOutcome::default()
                    },
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
    // Boxed: like the reservation below, the admission's state would otherwise
    // sit inline in this function's already large future.
    let admitted = match Box::pin(admit(
        &state,
        Admit {
            resolved: &resolved,
            model: &model,
            route: route.as_deref(),
            weight: effective_weight,
            cost: est.total(),
            prompt_tokens: est.input_tokens,
            end_user: end_user.as_deref(),
            batch: batch.as_ref(),
            session_id: Some(req_meta.session_id.as_str()),
            path: &path,
        },
        tracer.as_mut(),
        serde_json::json!({}),
        |queued_ms| {
            reject(
                &state,
                request_id,
                &resolved,
                &req_meta,
                &model,
                Rejection {
                    status: StatusCode::SERVICE_UNAVAILABLE,
                    since: request_start,
                    est,
                    cache_status: cache_status_label,
                    queue_wait_ms: queued_ms,
                },
            )
        },
    ))
    .await
    {
        Ok(admitted) => admitted,
        Err(resp) => return resp,
    };
    let admission = admitted.admission;
    let permit = admitted.permit;
    let queue_wait_ms = admitted.queue_wait_ms;
    let answer = Answer {
        request_id,
        queue_wait_ms,
        boons_applied: &boons_applied,
        compression: compression_header.as_deref(),
    };
    // Energy is metered from admission: queue time is never charged.
    let request_energy = state.energy.request(route.as_deref());

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
    let est_cost = estimated_cost(est, in_cost_rate, out_cost_rate, modality_cost);
    // Boxed: the reservation's state would otherwise sit inline in this
    // function's already large future, which a 2 MB worker stack cannot hold
    // in debug builds.
    let reserved = match Box::pin(reserve_budget(&state, &resolved, &model, est, est_cost)).await {
        Ok(reserved) => reserved,
        Err(refusal) => {
            drop(permit);
            if refusal.ledger {
                reject(
                    &state,
                    request_id,
                    &resolved,
                    &req_meta,
                    &model,
                    Rejection {
                        status: refusal.status,
                        since: request_start,
                        est,
                        cache_status: cache_status_label,
                        queue_wait_ms,
                    },
                );
            }
            return error_json(refusal.status, refusal.message);
        }
    };

    // From here on every exit settles through `settle_guard`, so the
    // per-minute bucket and any term reservation are reconciled exactly once
    // on every path — including a client that disconnects before the request
    // is sent upstream, which settles as nothing run (zero tokens, 499).
    // `arm_dispatched` switches that fallback to the prompt once the request
    // goes to a model server, and `arm_estimate` to the admission estimate
    // once an answer starts.
    let mut accounting = StreamAccounting {
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
        capacity: reserved.capacity,
        term_period: reserved.term_period,
        key_term_period: reserved.key_term_period,
        in_cost_rate,
        out_cost_rate,
        modality_cost,
        energy: request_energy,
        holds: reserved.holds,
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
                request_id,
                dispatch_timeout: req_timeout,
                started: request_start,
            };
            let outcome =
                crate::boons::speculation::run(spec_req, spec_plan, spec_stats.clone()).await;
            // An abstain or a shipped answer has its spans now; a committed
            // stream's follow the trace once the stream ends.
            let late_spans = tracer.as_ref().map(crate::tracer::SpanRecorder::late);
            if let Some(t) = tracer.as_mut() {
                let spans =
                    std::mem::take(&mut spec_stats.lock().unwrap_or_else(|e| e.into_inner()).spans);
                t.record_pending(spans);
            }
            match outcome {
                crate::boons::speculation::Outcome::Abstain(_) => {}
                crate::boons::speculation::Outcome::ShippedJson {
                    body,
                    input_tokens,
                    output_tokens,
                    drafted_by,
                } => {
                    drop(permit);
                    // Zero figures (accounting off, nothing measured) leave it as is.
                    let mut body = body;
                    crate::sse::stamp_usage(&mut body, accounting.energy_now());
                    let total_ms = request_start.elapsed().as_millis() as u32;
                    let _ = settle_guard
                        .complete(accounting.settle(
                            UpstreamUsage::uncached(input_tokens, output_tokens),
                            total_ms,
                            total_ms,
                            200,
                            None,
                        ))
                        .await;
                    if let Some(t) = tracer.take() {
                        t.finish("ok");
                    }
                    let mut resp = answer.respond(
                        StatusCode::OK,
                        header::HeaderValue::from_static("application/json"),
                        Body::from(body.to_string()),
                    );
                    resp.headers_mut().insert(
                        crate::boons::speculation::DRAFTED_BY_HEADER,
                        content_type_header(&drafted_by),
                    );
                    return resp;
                }
                crate::boons::speculation::Outcome::Stream { driver, drafted_by } => {
                    // Released to the client as it is produced, so the row is a
                    // stream and keeps its first-token time.
                    accounting.meta.streamed = true;
                    accounting.arm_estimate(&mut settle_guard);
                    let body_stream = async_stream::stream! {
                        futures_util::pin_mut!(driver);
                        // A continuation that stopped short ends with an error
                        // item; the client is told once the request has
                        // settled (see the streaming tool loop below).
                        let mut cut = false;
                        let mut energy_stamp = accounting.energy_stamp("text/event-stream");
                        while let Some(item) = driver.next().await {
                            match item {
                                Ok(chunk) => {
                                    let chunk = match energy_stamp.as_mut() {
                                        Some(stamp) => stamp.push(chunk, || accounting.energy_now()).client,
                                        None => chunk,
                                    };
                                    if !chunk.is_empty() {
                                        yield Ok(chunk);
                                    }
                                }
                                Err(_) => {
                                    cut = true;
                                    break;
                                }
                            }
                        }
                        if let Some(rest) = energy_stamp.take().map(|s| s.finish(|| accounting.energy_now()).client) {
                            if !rest.is_empty() {
                                yield Ok(rest);
                            }
                        }
                        drop(permit);
                        let (ttft_ms, settled, spans) = {
                            let mut s = spec_stats.lock().unwrap_or_else(|e| e.into_inner());
                            let usage = s.final_set.then_some(s.usage);
                            (
                                s.ttft_ms,
                                drained_settlement(200, s.failure, usage, 0, est),
                                std::mem::take(&mut s.spans),
                            )
                        };
                        if let Some(late) = &late_spans {
                            late.record(spans);
                        }
                        let total_ms = request_start.elapsed().as_millis() as u32;
                        let mut accounting = accounting;
                        accounting.fail_stream(settled.status, cut, None);
                        let _ = settle_guard.complete(accounting.settle_with(
                            settled.tokens, ttft_ms, total_ms, settled.status, None, settled.billed,
                        )).await;
                        if cut {
                            let event = crate::upstream_failure::stream_cut_event(
                                request_start.elapsed(),
                            );
                            let tail = crate::upstream_failure::stream_cut_tail(Some(event));
                            futures_util::pin_mut!(tail);
                            while let Some(item) = tail.next().await {
                                yield item;
                            }
                        }
                    };
                    if let Some(t) = tracer.take() {
                        t.finish("ok");
                    }
                    let mut resp = answer.respond(
                        StatusCode::OK,
                        header::HeaderValue::from_static("text/event-stream"),
                        Body::from_stream(body_stream),
                    );
                    resp.headers_mut().insert(
                        crate::boons::speculation::DRAFTED_BY_HEADER,
                        content_type_header(&drafted_by),
                    );
                    return resp;
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
    // Follow-up tool turns start from the request as dispatched; the body is
    // not read past this point, so the plan takes it instead of a copy.
    if let Some(loop_plan) = response_plan.as_mut().and_then(|p| p.tool_loop.as_mut()) {
        loop_plan.request = std::sync::Arc::new(std::mem::take(&mut json));
    }

    // TTFT is measured from the moment we dispatch the *successful* upstream
    // request, *after* fairshare admission. Time spent waiting in the queue is
    // reported separately as `queue_wait_ms`; folding it into TTFT would
    // double-count the wait and make a fast model look slow under contention.
    let dispatch_start = crate::tracer::now_ms();
    let mut upstream_start = Instant::now();
    let mut upstream_resp: Option<reqwest::Response> = None;
    let mut last_url = String::new();
    let mut last_err: Option<String> = None;
    // `last_err` in words fit for the ledger: no internal URLs.
    let mut last_reason: Option<String> = None;
    let mut timed_out = false;
    let total_targets = targets.len();
    // Which target answered: a job lives on the backend that accepted it.
    let mut served_target = 0usize;

    accounting.arm_dispatched(&mut settle_guard);
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
                    accounting.fail(
                        obleth_config::ErrorClass::Gateway,
                        "multipart request body was already consumed",
                    );
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
                        last_reason = Some(format!("model server answered {status}"));
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
                    // The client's read timeout also covers the wait for the
                    // response headers, so a slow non-streaming generation can
                    // end here rather than in the arm below. It is the same
                    // timeout: a 504, and never the fresh-connection retry,
                    // which would run the whole generation again.
                    // A connect timeout is also reported as a timeout, but the
                    // request never left: that one is a connection error.
                    let read_timed_out = e.is_timeout() && !e.is_connect();
                    timed_out |= read_timed_out;
                    let connection_error = is_connection_error(&e);
                    last_err = Some(e.to_string());
                    last_reason = Some(if read_timed_out {
                        format!(
                            "no answer from the model server within {}",
                            secs_label(req_timeout)
                        )
                    } else if e.is_connect() && e.is_timeout() {
                        "could not reach the model server: no connection within the connect timeout"
                            .to_string()
                    } else {
                        format!("could not reach the model server: {}", transport_cause(&e))
                    });
                    if configured_left {
                        state.metrics.record_upstream_attempt(if read_timed_out {
                            "timeout"
                        } else {
                            "retry"
                        });
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
                    last_reason = Some(format!(
                        "no answer from the model server within {}",
                        secs_label(req_timeout)
                    ));
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
            accounting.fail(
                if timed_out {
                    obleth_config::ErrorClass::UpstreamTimeout
                } else {
                    obleth_config::ErrorClass::Network
                },
                last_reason
                    .as_deref()
                    .unwrap_or("could not reach the model server"),
            );
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
        let mut read_error: Option<String> = None;
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
                    tracing::warn!(
                        error = %e,
                        %request_id,
                        model = %model,
                        status = status_code,
                        bytes_read = buf.len(),
                        "upstream read failed reading error body"
                    );
                    read_error = Some(transport_cause(&e));
                    break;
                }
            }
        }
        drop(permit);

        // Why the upstream refused, for the ledger. When its message broke off
        // before it could be read, the client gets an error saying so in the
        // shape its SDK reads, rather than the fragment.
        let mut content_type = content_type;
        let reason = match (obleth_config::reason_from_body(&buf), &read_error) {
            (Some(reason), _) => reason,
            (None, Some(cause)) => {
                let msg = format!(
                    "the model server answered {status_code}, but its error message was cut off ({cause})"
                );
                buf = serde_json::to_vec(&serde_json::json!({
                    "error": { "message": msg, "type": GATEWAY_ERROR_TYPE }
                }))
                .unwrap_or_default();
                content_type = header::HeaderValue::from_static("application/json");
                msg
            }
            (None, None) => format!("model server answered {status_code}"),
        };
        accounting.fail(
            obleth_config::class_for_status(status_code, "fast")
                .unwrap_or(obleth_config::ErrorClass::UpstreamError),
            &reason,
        );

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
            None => (UpstreamUsage::default(), false),
        };
        let _ = settle_guard
            .complete(accounting.settle_with(tokens, ttft_ms, total_ms, status_code, None, billed))
            .await;

        return answer.respond(status_code, content_type, Body::from(buf));
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

    // ---- job create (video, mesh): record the job before its id leaves ----
    // The response is a small JSON job object. It is read whole, its id is
    // recorded against this tenant, this key and the target that accepted
    // it, and only then is it returned — an unrecorded id could never be
    // followed up. The create is billed its flat `cost_per_video` (no
    // tokens) once recorded, and nothing when it is not. The whole step runs
    // as its own task so a client that leaves mid-way cannot strand a job it
    // was charged for.
    if let Some(kind) = job_create {
        if let Some(t) = tracer.take() {
            t.finish("ok");
        }
        let job = crate::jobs::CreatedJob {
            kind,
            jobs: state.jobs.clone(),
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
            let outcome = crate::jobs::record_create(job, upstream).await;
            drop(permit);
            let total_ms = request_start.elapsed().as_millis() as u32;
            let settle = accounting.settle_with(
                UpstreamUsage::default(),
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
            Err(_) => error_json(StatusCode::INTERNAL_SERVER_ERROR, "job create failed"),
        };
    }

    // Only a reply relayed as an event stream has a first token to time; see
    // `finalize`.
    let mut accounting = accounting;
    accounting.meta.streamed = relays_event_stream(&content_type_str);

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
    // A sticky `auto` turn is stored under the answer the client receives,
    // so only a successful one is watched for it.
    if status_code != 200 {
        sticky_turn = None;
    }
    let mut output_monitor = (scan_policy.is_some() || sticky_turn.is_some()).then(|| {
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
                        request_id,
                        base_request: loop_plan.request.clone(),
                        tool_servers: loop_plan.tool_servers.clone(),
                        settings: loop_plan.settings.clone(),
                        passthrough_unmapped: loop_plan.passthrough_unmapped,
                        image_gen: loop_plan.image_gen.clone(),
                        web_search: loop_plan.web_search.clone(),
                        dispatch_timeout: req_timeout,
                        client_include_usage: plan.include_usage,
                        upstream_start,
                        traced: tracer.is_some(),
                    },
                    upstream,
                    stats.clone(),
                );
                // The loop's spans are known only once the stream ends, after
                // the trace was sent; they follow it.
                let late_spans = tracer.as_ref().map(crate::tracer::SpanRecorder::late);

                let body_stream = async_stream::stream! {
                    futures_util::pin_mut!(driver);
                    // The driver ends a stream the upstream stopped short of
                    // with an error item. The client is told, and the
                    // response aborted, only once the request has settled:
                    // the abort makes hyper drop the body, and the guard
                    // would otherwise settle the request as a client
                    // disconnect.
                    let mut cut = false;
                    // The usage the loop reports carries the energy measured
                    // for the whole request, every turn included.
                    let mut energy_stamp = accounting.energy_stamp("text/event-stream");
                    while let Some(item) = driver.next().await {
                        match item {
                            Ok(chunk) => {
                                if let Some(monitor) = output_monitor.as_mut() {
                                    monitor.push(&chunk);
                                }
                                let chunk = match energy_stamp.as_mut() {
                                    Some(stamp) => stamp.push(chunk, || accounting.energy_now()).client,
                                    None => chunk,
                                };
                                if !chunk.is_empty() {
                                    yield Ok(chunk);
                                }
                            }
                            Err(_) => {
                                cut = true;
                                break;
                            }
                        }
                    }
                    if let Some(rest) = energy_stamp.take().map(|s| s.finish(|| accounting.energy_now()).client) {
                        if !rest.is_empty() {
                            yield Ok(rest);
                        }
                    }
                    drop(permit);
                    // A failed client-facing turn settles like a failed
                    // pass-through stream; a clean one with its usage, or the
                    // estimate when the upstream reported none.
                    let (ttft_ms, settled, spans) = {
                        let mut s = stats.lock().unwrap_or_else(|e| e.into_inner());
                        let usage = s.final_set.then(|| {
                            UpstreamUsage::new(s.input_tokens, s.output_tokens, s.cached_input_tokens)
                        });
                        (
                            s.ttft_ms,
                            drained_settlement(status_code, s.failure, usage, s.failed_turn_chars, est),
                            std::mem::take(&mut s.spans),
                        )
                    };
                    if let Some(late) = &late_spans {
                        late.record(spans);
                    }
                    let total_ms = request_start.elapsed().as_millis() as u32;
                    let finished = output_monitor.map(crate::output_monitor::OutputMonitor::finish);
                    accounting.monitor(scan_policy.as_ref(), finished.as_ref());
                    if let (false, 200, Some(turn), Some(Ok(done))) =
                        (cut, settled.status, sticky_turn, finished.as_ref())
                    {
                        turn.record(&accounting.state, done);
                    }
                    accounting.fail_stream(settled.status, cut, None);
                    let _ = settle_guard.complete(accounting.settle_with(
                        settled.tokens, ttft_ms, total_ms, settled.status, None, settled.billed,
                    )).await;
                    if cut {
                        let event =
                            crate::upstream_failure::stream_cut_event(upstream_start.elapsed());
                        let tail = crate::upstream_failure::stream_cut_tail(Some(event));
                        futures_util::pin_mut!(tail);
                        while let Some(item) = tail.next().await {
                            yield item;
                        }
                    }
                };
                if let Some(t) = tracer.take() {
                    t.finish(if status_code < 400 { "ok" } else { "error" });
                }
                return answer.respond(
                    status_code,
                    header::HeaderValue::from_static("text/event-stream"),
                    Body::from_stream(body_stream),
                );
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
        let mut read_error: Option<String> = None;
        let mut read_cause = String::new();
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
                    read_error = Some(e.to_string());
                    read_cause = transport_cause(&e);
                    break;
                }
            }
        }

        // The upstream broke off before its whole answer arrived. Nothing has
        // reached the client, and a cut-off completion reports no usage: a
        // failed call (502), billed nothing.
        if let Some(err) = read_error {
            drop(permit);
            state.alerts.issue(
                "upstream_stream_error",
                "Upstream stream failed",
                format!(
                    "tenant `{}` model `{model}` status `{status_code}`: {err}",
                    resolved.tenant_name
                ),
            );
            let total_ms = request_start.elapsed().as_millis() as u32;
            accounting.fail(
                obleth_config::ErrorClass::UpstreamError,
                &format!("the answer broke off before it arrived ({read_cause})"),
            );
            let _ = settle_guard
                .complete(accounting.settle_unbilled(ttft_ms, total_ms, 502))
                .await;
            return error_json(StatusCode::BAD_GATEWAY, "upstream stream failed");
        }

        // Parse + transform. Oversized, truncated, or unparseable bodies pass
        // through unchanged (fail-open); only well-formed completions are
        // rewritten.
        let mut warning: Option<&'static str> = None;
        // Usage the main row settles with when the buffered tool loop replaced
        // the body (the follow-up turns are billed as helper rows).
        let mut turn0_usage: Option<UpstreamUsage> = None;
        let mut completion: Option<serde_json::Value> = (!truncated
            && buf.len() <= BOON_BUFFER_MAX)
            .then(|| serde_json::from_slice::<serde_json::Value>(&buf).ok())
            .flatten();

        // A 200 whose body is the upstream's own failure (an error object, or
        // a generation the server aborted) has nothing to transform. The
        // client gets that body under the failure's status, as if the
        // upstream had said so in its status line; the request is billed
        // only the usage the body reports and is never cached.
        if let Some(failure) = completion
            .as_ref()
            .and_then(crate::upstream_failure::payload_failure)
        {
            drop(permit);
            tracing::warn!(
                status = failure.status,
                message = %failure.message,
                "upstream reported a failure inside a 200 response"
            );
            if failure.status >= 500 {
                state.alerts.issue(
                    "upstream_error_event",
                    "Upstream reported an error inside a response",
                    format!(
                        "tenant `{}` model `{model}` status `{}`: {}",
                        resolved.tenant_name, failure.status, failure.message
                    ),
                );
            }
            let (tokens, billed) = match completion.as_ref().and_then(completion_body_usage) {
                Some(tokens) => (tokens, true),
                None => (UpstreamUsage::default(), false),
            };
            let total_ms = request_start.elapsed().as_millis() as u32;
            accounting.fail(upstream_failure_class(failure.status), &failure.message);
            let _ = settle_guard
                .complete(accounting.settle_with(
                    tokens,
                    ttft_ms,
                    total_ms,
                    failure.status,
                    None,
                    billed,
                ))
                .await;
            return answer.respond(failure.status, content_type, Body::from(buf));
        }

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
                    request_id,
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
                        request_id,
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
                                .unwrap_or(UpstreamUsage::uncached(
                                    est.input_tokens,
                                    est.estimated_output_tokens,
                                ));
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
            // An answer too large to hold, or not a completion, cannot be given
            // to the scanners. Under a policy that blocks or redacts it is not
            // passed on unread; with no such policy it goes through as it came.
            None if plan.guardrails.is_some() => {
                drop(permit);
                let block = crate::boons::guardrails::GuardrailsBlock::unchecked();
                tracing::warn!(
                    bytes = buf.len(),
                    truncated,
                    "a response under an output content policy could not be read; not passed on"
                );
                let tokens = UpstreamUsage::uncached(est.input_tokens, est.estimated_output_tokens);
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
                return error_json(block.status, block.reason);
            }
            None => (
                Bytes::from(std::mem::take(&mut buf)),
                content_type_str.clone(),
            ),
        };
        drop(permit);

        let tokens = turn0_usage
            .or_else(|| completion.as_ref().and_then(completion_body_usage))
            .unwrap_or(UpstreamUsage::uncached(
                est.input_tokens,
                est.estimated_output_tokens,
            ));
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
        // The client's usage also carries the energy measured for it; the cached
        // copy above stays without it, as a cache hit uses none.
        let final_body = if status_code == 200
            && is_stream_usage_path(&path)
            && accounting.state.energy.settings().enabled
        {
            let figures = accounting.state.energy.figures(
                &accounting.energy,
                total_ms,
                accounting.queue_wait_ms,
            );
            crate::sse::stamp_body(final_body, &final_content_type, figures)
        } else {
            final_body
        };
        let _ = settle_guard
            .complete(accounting.settle(tokens, ttft_ms, total_ms, status_code, cache_put))
            .await;
        if let (Some(turn), Some(done)) = (sticky_turn.take(), completion.as_ref()) {
            turn.record(&state, done);
        }

        let mut resp = answer.respond(
            status_code,
            content_type_header(&final_content_type),
            Body::from(final_body),
        );
        if let Some(w) = warning {
            resp.headers_mut()
                .insert(crate::boons::BOONS_WARNING_HEADER, content_type_header(w));
        }
        if let Some(t) = tracer.take() {
            t.finish(if status_code < 400 { "ok" } else { "error" });
        }
        return resp;
    }

    // Finish the tracer before entering the async stream body — the stream macro
    // cannot capture a non-Clone, non-Send value.
    if let Some(t) = tracer.take() {
        t.finish(if status_code < 400 { "ok" } else { "error" });
    }

    // A generation request's 200 can still carry the upstream's failure in its
    // body (an SSE error event, or a JSON error object); watched so it is
    // recorded as the failure it is. A chat or text completion stream is also
    // watched for its end, so one the upstream stops short of is told apart
    // from a whole answer.
    let expect_end = is_stream_usage_path(&path);
    let mut failure_watch = (method == Method::POST)
        .then(|| crate::upstream_failure::FailureWatch::for_content_type(&content_type_str))
        .flatten()
        .map(|watch| {
            if expect_end {
                watch.expecting_end()
            } else {
                watch
            }
        });

    // ---- stream back, inspecting for actual usage, then reconcile ----
    let body_stream = async_stream::stream! {
        let mut byte_stream = upstream.bytes_stream();
        let mut first = true;
        let mut ttft_ms = 0u32;
        let mut tail: Vec<u8> = Vec::with_capacity(TAIL_CAP.min(4 * 1024));
        let mut full: Vec<u8> = Vec::new();
        let mut cacheable = store_in_cache.is_some();
        // Drops the usage chunk the gateway asked for on the client's behalf;
        // the usage a client asked for also carries the energy measured for it.
        let mut usage = crate::sse::UsageRewriter::new(
            &content_type_str,
            strip_usage_chunk,
            expect_end && status_code == 200 && accounting.energy_on(),
        );
        let mut broke_off = false;
        // Characters of generated text delivered so far, for billing a stream
        // the upstream breaks off before reporting usage.
        let mut streamed_chars: usize = 0;
        // The last bytes sent, so an event the gateway adds starts on an
        // event boundary.
        let mut sent_tail: Vec<u8> = Vec::with_capacity(8);

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
                    if let Some(watch) = failure_watch.as_mut() { watch.push(&chunk); }
                    let crate::sse::Rewritten { client, unstamped } = match usage.as_mut() {
                        Some(usage) => usage.push(chunk, || accounting.energy_now()),
                        None => crate::sse::Rewritten { client: chunk.clone(), unstamped: chunk },
                    };
                    // The monitor and the cache read the answer without the
                    // energy added: a cache hit uses none.
                    if !unstamped.is_empty() {
                        if let Some(monitor) = output_monitor.as_mut() { monitor.push(&unstamped); }
                        if cacheable {
                            if full.len() + unstamped.len() <= CACHE_MAX_BYTES {
                                full.extend_from_slice(&unstamped);
                            } else {
                                cacheable = false;
                                full = Vec::new();
                            }
                        }
                    }
                    if client.is_empty() {
                        continue;
                    }
                    crate::upstream_failure::remember_sent(&mut sent_tail, &client);
                    yield Ok::<Bytes, std::io::Error>(client);
                }
                Err(e) if failure_watch.as_ref().is_some_and(|w| w.saw_done()) => {
                    // The answer is whole once `[DONE]` is in; a connection
                    // the upstream drops after it cuts nothing.
                    tracing::debug!(error = %e, "upstream stream closed uncleanly after [DONE]");
                    break;
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
                    broke_off = true;
                    break;
                }
            }
        }
        if !broke_off {
            // A JSON answer is held until it ends; a partial trailing event
            // is passed through as received.
            if let Some(usage) = usage.take() {
                let crate::sse::Rewritten { client, unstamped } =
                    usage.finish(|| accounting.energy_now());
                if !unstamped.is_empty() {
                    if let Some(monitor) = output_monitor.as_mut() { monitor.push(&unstamped); }
                    if cacheable {
                        if full.len() + unstamped.len() <= CACHE_MAX_BYTES {
                            full.extend_from_slice(&unstamped);
                        } else {
                            cacheable = false;
                            full = Vec::new();
                        }
                    }
                }
                if !client.is_empty() {
                    crate::upstream_failure::remember_sent(&mut sent_tail, &client);
                    yield Ok::<Bytes, std::io::Error>(client);
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
        let finished = output_monitor.map(crate::output_monitor::OutputMonitor::finish);
        accounting.monitor(scan_policy.as_ref(), finished.as_ref());

        // A failure the upstream reported in the bytes it sent.
        let failure = failure_watch.as_mut().and_then(|w| w.finish());
        // A completion stream that closed with neither `[DONE]` nor a finish
        // reason stopped short as surely as one whose connection broke.
        let ended_early = !broke_off
            && failure_watch.as_ref().is_some_and(|w| w.ended_early());
        if ended_early {
            tracing::warn!("upstream stream ended before its last event");
            accounting.state.alerts.issue(
                "upstream_stream_error",
                "Upstream stream failed",
                format!(
                    "tenant `{}` model `{model}` status `{status_code}`: \
                     stream ended before its last event",
                    accounting.resolved.tenant_name
                ),
            );
        }

        if broke_off || ended_early {
            // A stream the upstream broke off is a failed call (502, or the
            // status of an error it reported first), billed for what reached
            // the client: reported usage, else the streamed text's estimated
            // tokens; nothing only when nothing streamed.
            // Settlement is handed off before the error is yielded: the error
            // makes hyper drop this body, and the guard would otherwise settle
            // it as a client disconnect.
            let status = failure
                .as_ref()
                .map_or(crate::upstream_failure::DEFAULT_FAILURE_STATUS, |f| f.status);
            let settled = drained_settlement(status_code, Some(status), usage, streamed_chars, est);
            accounting.fail_stream(
                settled.status,
                true,
                Some(failure.as_ref().map_or(
                    if ended_early {
                        "the answer stream ended before its last event"
                    } else {
                        "the answer stream broke off"
                    },
                    |f| f.message.as_str(),
                )),
            );
            let _ = settle_guard.complete(accounting.settle_with(
                settled.tokens, ttft_ms, total_ms, settled.status, None, settled.billed,
            )).await;
            // The client is told in the stream, in the error shape its SDK
            // raises on, unless the upstream's own error event already said
            // why. A blank line first ends an event the upstream left
            // unfinished, so the two do not run together.
            let event = (failure.is_none()
                && failure_watch.as_ref().is_some_and(|w| w.is_sse()))
            .then(|| {
                let event = crate::upstream_failure::stream_cut_event(upstream_start.elapsed());
                if crate::upstream_failure::at_event_boundary(&sent_tail) {
                    event
                } else {
                    Bytes::from([&b"\n\n"[..], &event[..]].concat())
                }
            });
            // Then an error item aborts the response instead of ending it
            // cleanly, so the client cannot mistake the truncated body for a
            // whole one.
            let tail = crate::upstream_failure::stream_cut_tail(event);
            futures_util::pin_mut!(tail);
            while let Some(item) = tail.next().await {
                yield item;
            }
        } else {
            if let (None, Some(turn), Some(Ok(done))) = (&failure, sticky_turn, finished.as_ref()) {
                turn.record(&accounting.state, done);
            }
            // The bytes already went to the client as the upstream sent them;
            // a failure it reported in them is recorded as one.
            if let Some(failure) = &failure {
                tracing::warn!(
                    status = failure.status,
                    message = %failure.message,
                    "upstream reported a failure inside a {status_code} response"
                );
                if failure.status >= 500 {
                    accounting.state.alerts.issue(
                        "upstream_error_event",
                        "Upstream reported an error inside a response",
                        format!(
                            "tenant `{}` model `{model}` status `{}`: {}",
                            accounting.resolved.tenant_name, failure.status, failure.message
                        ),
                    );
                }
            }
            let settled = drained_settlement(
                status_code,
                failure.as_ref().map(|f| f.status),
                usage,
                streamed_chars,
                est,
            );
            accounting.fail_stream(
                settled.status,
                false,
                failure.as_ref().map(|f| f.message.as_str()),
            );
            // store the full response for identical future requests
            let cache_put = if cacheable && settled.cacheable {
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
            let _ = settle_guard.complete(accounting.settle_with(
                settled.tokens, ttft_ms, total_ms, settled.status, cache_put, settled.billed,
            )).await;
        }
    };

    answer.respond(status_code, content_type, Body::from_stream(body_stream))
}

/// What an admitted request holds against its budgets, for settlement: the
/// per-minute capacity it reserved under, the term periods it reserved in,
/// and which of those reservations settlement must reconcile.
pub(crate) struct Reserved {
    pub(crate) capacity: i64,
    pub(crate) term_period: Option<String>,
    pub(crate) key_term_period: Option<String>,
    pub(crate) holds: TermHolds,
}

/// Why a request could not reserve its budget: what to answer, and whether
/// it is a refusal of the caller's (a cap or a rate limit, recorded as a
/// rejected ledger row) or the gateway's own failure closed (no row).
pub(crate) struct BudgetRefusal {
    pub(crate) status: StatusCode,
    pub(crate) message: &'static str,
    pub(crate) ledger: bool,
}

/// Reserve an admitted request's estimate against the key's term cap, then
/// the tenant's per-minute bucket and term cap, in one Redis round trip per
/// scope. The term gate runs first inside the script, so a term-exhausted
/// request never reserves per-minute tokens it has no completion path to
/// refund; a key reservation is released when the tenant scope refuses, and
/// handed to settlement otherwise. A Redis failure admits the request without
/// enforcement when the gateway fails open, and refuses it otherwise; both
/// raise an alert. Shared by the passthrough pipeline and verdicts, so the
/// two cannot drift.
pub(crate) async fn reserve_budget(
    state: &AppState,
    resolved: &ResolvedKey,
    model: &str,
    est: CostEstimate,
    est_cost: f64,
) -> Result<Reserved, BudgetRefusal> {
    let capacity = resolved.tokens_per_minute.max(0);
    let now = chrono::Utc::now();
    let term_period = term_period_key(resolved, now);
    let term_gate = term_period
        .as_deref()
        .map(|period_key| obleth_redis::TermGate {
            period_key,
            budget_tokens: resolved.budget_tokens,
            budget_cost_usd: resolved.budget_cost_usd,
        });
    let key_term_period = key_term_period_key(resolved, now);
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
    // Releases the key reservation if the request ends before settlement
    // takes ownership of it.
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
                    PendingTermHold::new(state, resolved.key_id, period, est, est_cost)
                });
            }
            // Capacity 0 has no per-minute bucket; nothing was reserved.
            Ok(obleth_redis::ReserveOutcome::RateLimited { .. }) => {}
            Ok(obleth_redis::ReserveOutcome::TermExhausted {
                used_tokens,
                used_cost,
            }) => {
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
                return Err(BudgetRefusal {
                    status: StatusCode::FORBIDDEN,
                    message: "api key term budget exhausted",
                    ledger: true,
                });
            }
            Err(e) => {
                if !state.fail_open {
                    state.alerts.issue(
                        "redis_key_budget_reserve_failed_closed",
                        "Redis key-budget reserve failed",
                        format!(
                            "fail-open is disabled; rejecting tenant `{}` key `{}` model `{model}`: {e}",
                            resolved.tenant_name,
                            resolved.key_id
                        ),
                    );
                    return Err(BudgetRefusal {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        message: "key budget check failed",
                        ledger: false,
                    });
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
    if capacity > 0 || tenant_gate_armed {
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
                if let Some(hold) = key_hold.take() {
                    hold.release().await;
                }
                return Err(BudgetRefusal {
                    status: StatusCode::TOO_MANY_REQUESTS,
                    message: "token budget exceeded",
                    ledger: true,
                });
            }
            Ok(obleth_redis::ReserveOutcome::TermExhausted {
                used_tokens,
                used_cost,
            }) => {
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
                return Err(BudgetRefusal {
                    status: StatusCode::FORBIDDEN,
                    message: "tenant term budget exhausted",
                    ledger: true,
                });
            }
            Err(e) => {
                if !state.fail_open {
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
                    return Err(BudgetRefusal {
                        status: StatusCode::SERVICE_UNAVAILABLE,
                        message: "budget check failed",
                        ledger: false,
                    });
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
    Ok(Reserved {
        capacity,
        term_period,
        key_term_period,
        holds: TermHolds {
            tenant: tenant_term_held,
            key: key_term_held,
            est_cost,
        },
    })
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

/// Resolve a key via moka, falling back to Redis and caching the result.
/// Distinguishes a Redis error from a miss so callers can fail closed on the
/// former (503, backend unavailable) rather than reading it as an unknown
/// credential (401) -- see `jwt_auth::authenticate_credential`.
///
/// With `remember_miss`, a hash Redis does not know is remembered on this pod
/// for a minute and answered as unknown without asking Redis again (see
/// `key_miss`). Callers pass it only for credentials without a checksum,
/// never for identity handles, whose miss means "provision".
#[tracing::instrument(skip_all, name = "auth_resolve")]
pub(crate) async fn try_resolve_key(
    state: &AppState,
    hash: &str,
    remember_miss: bool,
) -> Result<Option<Arc<ResolvedKey>>, ()> {
    if let Some(r) = state.key_cache.get(hash).await {
        return Ok(Some(r));
    }
    let found = state
        .key_misses
        .lookup(hash, remember_miss, || state.redis.get_resolved_key(hash))
        .await;
    match found {
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

/// Routing intent for an `auto` request. An explicit effort header sets the
/// difficulty. Otherwise the classifier brain is asked (when enabled,
/// configured, resolvable, and not itself `auto`), and the heuristics answer
/// whatever it did not: all of it when the brain is off, slow or down, and
/// small talk always. A slow or broken brain therefore costs its timeout and
/// nothing else: the request is read as it would be with no classifier.
// Each argument is one independent input to the routing decision.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn derive_intent(
    state: &AppState,
    json: &serde_json::Value,
    est_input_tokens: u64,
    available_tags: &[String],
    settings: &obleth_config::AutoRouterSettings,
    effort_header: Option<&str>,
    classify_system: Option<usize>,
    skip_classifier: bool,
) -> crate::router::Intent {
    let mut classified = crate::router::Classified::default();
    if !skip_classifier && settings.classifier_active() && !available_tags.is_empty() {
        if let Some(name) = settings.classifier_model.as_deref() {
            if name != crate::router::AUTO_MODEL_NAME {
                if let Some(prompt) = crate::router::classifier_prompt(json, classify_system) {
                    if let Some(brain) = resolve_model(state, name).await {
                        classified = state
                            .classifier
                            .classify(&state.http, &brain, &prompt, available_tags)
                            .await;
                    }
                }
            }
        }
    }

    let mut intent = classified.settle(crate::router::heuristic_intent(json, est_input_tokens));
    if let Some(d) = crate::router::difficulty_from_header(effort_header) {
        intent.difficulty = d;
        intent.source = crate::router::IntentSource::Header;
    }
    intent
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
/// cache, same timeout — on an already-built prompt and tag menu. Returns no
/// answer when the classifier is off, unconfigured, or unresolvable, which is
/// what the data plane settles from the heuristics.
pub async fn classify_for_simulate(
    state: &AppState,
    prompt: String,
    available_tags: Vec<String>,
) -> crate::router::Classified {
    let settings = state.classifier.settings();
    let none = crate::router::Classified::default();
    if !settings.classifier_active() || available_tags.is_empty() {
        return none;
    }
    let Some(name) = settings.classifier_model.as_deref() else {
        return none;
    };
    if name == crate::router::AUTO_MODEL_NAME || prompt.trim().is_empty() {
        return none;
    }
    let Some(brain) = resolve_model(state, name).await else {
        return none;
    };
    state
        .classifier
        .classify(&state.http, &brain, &prompt, &available_tags)
        .await
}

/// A model by name, or `None` when it is not registered or Redis could not
/// be asked. For lookups where a Redis fault and a missing model mean the
/// same thing (an optional helper model); a client's own model goes through
/// [`try_resolve_model`] so a fault is answered as one.
pub(crate) async fn resolve_model(state: &AppState, name: &str) -> Option<Arc<ResolvedModel>> {
    try_resolve_model(state, name).await.ok().flatten()
}

/// A model by name: `Ok(None)` when it is not registered, `Err(())` when the
/// cache missed and Redis failed (logged and alerted here), as
/// [`try_resolve_key`] does for keys.
pub(crate) async fn try_resolve_model(
    state: &AppState,
    name: &str,
) -> Result<Option<Arc<ResolvedModel>>, ()> {
    if let Some(r) = state.model_cache.get(name).await {
        return Ok(Some(r));
    }
    let looked_up = state
        .model_misses
        .lookup(name, true, || state.redis.get_resolved_model(name))
        .await;
    match looked_up {
        Ok(Some(r)) => {
            let r = Arc::new(r);
            state.model_cache.insert(name.to_string(), r.clone()).await;
            Ok(Some(r))
        }
        Ok(None) => Ok(None),
        Err(e) => {
            tracing::warn!(error = %e, "redis model lookup failed");
            state.alerts.issue(
                "redis_model_lookup_failed",
                "Redis model lookup failed",
                format!("model `{name}` lookup failed against Redis: {e}"),
            );
            Err(())
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
///
/// `end_user` is the end user [`end_user_for`] found (always `None` unless the
/// key has `end_user_fairshare` on). With one, the scheduler key is derived
/// from the key and the end user, so that user queues on their own, and the
/// key's weight and cap apply to them alone.
pub(crate) fn admit_request_for(
    resolved: &ResolvedKey,
    model: &str,
    route: Option<&ResolvedModel>,
    weight: i64,
    cost: u32,
    end_user: Option<&str>,
) -> obleth_fairshare::AdmitRequest {
    let positive = |c: Option<i64>| c.and_then(|c| usize::try_from(c).ok()).filter(|c| *c > 0);
    let (key, end_user) = match end_user {
        Some(name) => (
            obleth_config::end_user_key_id(resolved.key_id, name),
            Some(obleth_fairshare::EndUser {
                parent_key: resolved.key_id,
                name: name.to_string(),
            }),
        ),
        None => (resolved.key_id, None),
    };
    obleth_fairshare::AdmitRequest {
        tenant: resolved.tenant_id,
        key,
        weight,
        key_weight: resolved.key_weight.max(1),
        group: resolved.fairshare_group.clone(),
        group_weight: resolved.group_weight,
        model: model.to_string(),
        model_max_in_flight: route.and_then(|r| r.max_in_flight).filter(|c| *c > 0),
        tenant_max_in_flight: positive(resolved.max_in_flight),
        key_max_in_flight: positive(resolved.key_max_in_flight),
        cost,
        end_user,
        session: None,
        affinity: Default::default(),
        background_share: None,
        slots: 1,
    }
}

/// Whether a route sends every request of one conversation to the same
/// upstream, so the conversation's prompt cache can stay warm there: one
/// usable endpoint (or none, the model's own base), or any selection mode
/// but `load_balance`, which spreads requests across endpoints.
fn route_keeps_sessions(route: Option<&ResolvedModel>) -> bool {
    let Some(route) = route else {
        return false;
    };
    let usable = route
        .endpoints
        .iter()
        .filter(|e| e.enabled && e.healthy)
        .count();
    usable <= 1 || route.endpoint_selection_mode != "load_balance"
}

/// Header a shared front end names each request's end user with, for keys
/// with `end_user_fairshare` on. A gateway directive, never sent upstream.
pub(crate) const END_USER_HEADER: &str = "x-obleth-end-user";

/// The end user a request names, for a key with `end_user_fairshare` on:
/// the `x-obleth-end-user` header, else the body's OpenAI `user` field, else
/// Anthropic's `metadata.user_id`. Always `None` for other keys, so an
/// ordinary caller cannot split itself into many places in the queue.
pub(crate) fn end_user_for(
    resolved: &ResolvedKey,
    headers: &HeaderMap,
    json: &serde_json::Value,
) -> Option<String> {
    if !resolved.end_user_fairshare {
        return None;
    }
    headers
        .get(END_USER_HEADER)
        .and_then(|v| v.to_str().ok())
        .and_then(obleth_config::normalize_end_user)
        .or_else(|| {
            json.get("user")
                .or_else(|| json.pointer("/metadata/user_id"))
                .and_then(serde_json::Value::as_str)
                .and_then(obleth_config::normalize_end_user)
        })
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
    obleth_config::budget::period_key(
        budget_tokens,
        budget_cost_usd,
        budget_period,
        budget_started_at,
        timezone,
        now,
    )
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

/// The model allowlist that bounds what a caller is shown, or `None` when it
/// may see everything (internal callers, and tenants without an allowlist).
/// Entries are canonical `model_name`s, which is also what the discovery
/// endpoints advertise.
pub(crate) fn allowed_models_for(resolved: &ResolvedKey) -> Option<&[String]> {
    if resolved.internal {
        None
    } else {
        resolved.allowed_models.as_deref()
    }
}

/// True when `model_name` is visible under `allowed` (see [`allowed_models_for`]).
pub(crate) fn model_visible(allowed: Option<&[String]>, model_name: &str) -> bool {
    allowed.is_none_or(|list| list.iter().any(|m| m == model_name))
}

/// `requested`, when it names one of `route`'s variants rather than the
/// model itself or an alias.
pub(crate) fn variant_called<'a>(route: &ResolvedModel, requested: &'a str) -> Option<&'a str> {
    route
        .variants
        .iter()
        .any(|v| v.name == requested)
        .then_some(requested)
}

/// Whether the caller may use `route`, served as `variant` when one was
/// named. A tenant's allowlist names a model by its own name, which covers
/// its aliases and variants, or a variant by the variant's name, so a tenant
/// can be given the variant alone; an alias on the list grants nothing.
/// Internal callers and tenants without a list may use everything. The one
/// rule for the pipeline, search, speech and batches.
pub(crate) fn model_permitted(
    resolved: &ResolvedKey,
    route: &ResolvedModel,
    variant: Option<&str>,
) -> bool {
    allowed_models_for(resolved).is_none_or(|allowed| {
        allowed
            .iter()
            .any(|m| m == &route.model_name || variant == Some(m.as_str()))
    })
}

/// Per-request surcharge for non-token-billed modalities: image generations are
/// billed per image, text-to-speech per input character, and a video or mesh
/// job at the model's flat per-job price (`cost_per_video`). Returns `0.0` for token-billed modalities
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
        // Frozen at the create, the only job call that reaches settlement:
        // the polls and the download are served outside the pipeline.
        "video" | "mesh" => route.cost_per_video,
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
    // A connect timeout is a timeout too, but the request never left.
    if e.is_connect() {
        return true;
    }
    // reqwest reports a read timeout before the headers as a request error;
    // the upstream got the request and was working on it.
    if e.is_timeout() {
        return false;
    }
    if e.is_request() {
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

/// The innermost cause of a transport error, which names what went wrong
/// (`Connection refused (os error 111)`) without the request URL that
/// reqwest's own message carries.
fn transport_cause(e: &reqwest::Error) -> String {
    use std::error::Error as _;
    let mut cause = String::new();
    let mut source = e.source();
    while let Some(err) = source {
        cause = err.to_string();
        source = err.source();
    }
    if cause.is_empty() {
        let kind = if e.is_connect() {
            "connect error"
        } else if e.is_body() || e.is_decode() {
            "response body error"
        } else {
            "request error"
        };
        return kind.to_string();
    }
    cause
}

/// The class of a failure status the upstream gave, in its status line or
/// inside its answer.
fn upstream_failure_class(status: u16) -> obleth_config::ErrorClass {
    obleth_config::class_for_status(status, "fast")
        .unwrap_or(obleth_config::ErrorClass::UpstreamError)
}

/// A duration in whole seconds, for reasons a person reads.
fn secs_label(d: Duration) -> String {
    format!("{} s", d.as_secs())
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
    // A percent-escape of a letter, digit, `-._~`, or a separator spells a
    // path the routes here would not recognize but an upstream that decodes
    // before routing would (`/v1/%76ideos/{id}` is its `/v1/videos/{id}`), and
    // `%2e`, `%2f`, `%5c` can rebuild a traversal. No client needs to escape
    // those, so any such escape, and a `%` that starts none, is refused.
    let bytes = path.as_bytes();
    let mut i = 0;
    while let Some(at) = bytes[i..].iter().position(|b| *b == b'%') {
        let at = i + at;
        let hex = |b: Option<&u8>| b.and_then(|b| (*b as char).to_digit(16));
        let (Some(hi), Some(lo)) = (hex(bytes.get(at + 1)), hex(bytes.get(at + 2))) else {
            return true;
        };
        let decoded = (hi * 16 + lo) as u8;
        if decoded.is_ascii_alphanumeric() || b"-._~/\\".contains(&decoded) {
            return true;
        }
        i = at + 3;
    }
    // `.` is refused with `..`: the upstream's URL parser drops it, so
    // `/v1/./videos` would be forwarded as `/v1/videos` without matching it here.
    path.split(['/', '\\']).any(|seg| seg == ".." || seg == ".")
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
            // strip hop-by-hop / auth / encoding so the body stays inspectable
            "host" | "content-length" | "authorization" | "x-api-key" | "accept-encoding"
            | "connection" => continue,
            // every x-obleth-* header is between the client and the gateway
            // (directives, the end user, session hints); none is an upstream's
            n if n.starts_with("x-obleth-") => continue,
            _ => {
                out.insert(name.clone(), value.clone());
            }
        }
    }
    out
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
    /// The end user the caller named, on keys with `end_user_fairshare` on
    /// (see [`end_user_for`]), else empty.
    pub(crate) end_user: String,
    /// The variant name the client called the model by, else empty. The
    /// ledger's `model` is the parent's either way.
    pub(crate) model_variant: String,
    /// The reasoning effort served (see `UsageRecord::reasoning_effort`).
    pub(crate) reasoning_effort: String,
    /// The reasoning effort the caller asked for, or empty.
    pub(crate) reasoning_effort_requested: String,
    /// The model family the client called by name, else empty.
    pub(crate) model_family: String,
    /// Whether the reply is relayed to the client as an event stream, known
    /// once the upstream answers; see `UsageRecord::streamed`.
    pub(crate) streamed: bool,
    /// Why the request failed, recorded where its failure status is chosen.
    /// Left `None` where the status says it all; see
    /// `obleth_config::ledger_failure`.
    pub(crate) failure: Option<obleth_config::Failure>,
    /// The crash guard's fingerprint of the request body, or empty when the
    /// guard does not watch it; see [`crate::crash_guard`].
    pub(crate) crash_fp: String,
    /// Where the request came from, for the ledger's client columns. Unknown
    /// (empty) for work the gateway runs itself.
    pub(crate) client: crate::client::ClientInfo,
}

/// The name a response cache entry is keyed by.
///
/// A variant's name when the client used one: its boons can change the
/// answer, so it must not share entries with its parent. (A family's name
/// rides the same slot.) The level is part of the name whenever it shaped
/// the request outside the body: a translated effort, whose Responses or
/// Messages caller sent the level in a field the chat body does not keep,
/// and a family's level, which also picked the member that answers, even a
/// member with no effort control of its own. Two levels must not share
/// answers.
fn response_cache_name(
    model: &str,
    variant: Option<&str>,
    level: Option<obleth_config::ReasoningLevel>,
) -> String {
    let name = variant.unwrap_or(model);
    match level {
        Some(level) => format!("{name}@{level}"),
        None => name.to_string(),
    }
}

/// Record the name the client called the model by: a family's goes in
/// `model_family` (the family name rides the variant slot through the
/// pipeline, for the allowlist, the cache and the response header), a real
/// variant's in `model_variant`.
fn record_called_name(meta: &mut RequestMeta, variant: Option<&str>, family: Option<&str>) {
    match family {
        Some(f) => {
            meta.model_family = f.to_string();
            meta.model_variant.clear();
        }
        None => meta.model_variant = variant.unwrap_or_default().to_string(),
    }
}

/// Remove the per-tool `strict` flag from a chat request's function tools.
/// Returns whether anything was removed.
///
/// vLLM does not enforce `strict` as a schema guarantee, and with some tool
/// parsers it changes decoding so that string arguments (code, file contents)
/// are dropped from the call. Agent frameworks send `strict: true` by default,
/// so the flag is removed before any boon or upstream sees the request. Only
/// `function.strict` is touched: the schema itself, including properties that
/// happen to be named `strict`, and `response_format` are left as sent. The
/// `/v1/responses` and `/v1/messages` translations already omit the flag.
fn strip_tool_strict(json: &mut serde_json::Value) -> bool {
    let Some(tools) = json.get_mut("tools").and_then(|t| t.as_array_mut()) else {
        return false;
    };
    let mut removed = false;
    for tool in tools {
        if let Some(function) = tool.get_mut("function").and_then(|f| f.as_object_mut()) {
            removed |= function.remove("strict").is_some();
        }
    }
    removed
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
        // OpenAI's prompt-cache routing key: the client's own grouping of
        // requests that share a prefix.
        json.get("prompt_cache_key").and_then(|v| v.as_str()),
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

/// Maximum seed text hashed from each part (the leading system/developer
/// text, then the first user message); bounds work on huge prompts. The same
/// leading bytes are replayed every turn, so capping stays stable.
const SEED_CAP: usize = 8 * 1024;

/// Build the stable conversation seed: leading system/developer message text up
/// to and including the first user message. `None` when there are no messages
/// or no text to hash (e.g. embeddings, image-only first turn with no system).
///
/// The first user message has a budget of its own: clients that share one
/// long system prompt across every conversation are told apart only by it.
/// A seed that fits in one cap hashes the same as it did under a shared one.
fn conversation_seed(json: &serde_json::Value) -> Option<String> {
    let messages = json.get("messages")?.as_array()?;
    let mut seed = String::new();
    for msg in messages {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        match role {
            "system" | "developer" => {
                push_capped(&mut seed, &message_text(msg.get("content")), SEED_CAP)
            }
            "user" => {
                // The separator `push_capped` adds is not part of the budget.
                let budget = match seed.len() {
                    0 => SEED_CAP,
                    n => n + 1 + SEED_CAP,
                };
                push_capped(&mut seed, &message_text(msg.get("content")), budget);
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

/// Append until the seed is `limit` bytes long, with a separator so adjacent
/// messages can't merge into a colliding blob.
fn push_capped(seed: &mut String, text: &str, limit: usize) {
    if seed.len() >= limit || text.is_empty() {
        return;
    }
    if !seed.is_empty() {
        seed.push('\u{1f}'); // unit separator
    }
    let room = limit.saturating_sub(seed.len());
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

/// FNV-1a 64-bit (same family as `obleth_admin`'s `rendezvous_score`): fixed-seed, deterministic
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

/// Whether an upstream reply with this content type reaches the client as it
/// is produced: an event stream, rather than one body sent when it is done.
fn relays_event_stream(content_type: &str) -> bool {
    content_type.contains("text/event-stream")
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

/// The request id every answer of an endpoint carries, on a response that
/// did not set one itself.
pub(crate) fn with_request_id(mut resp: Response<Body>, request_id: Uuid) -> Response<Body> {
    if let Ok(value) = header::HeaderValue::from_str(&request_id.to_string()) {
        resp.headers_mut()
            .entry("x-obleth-request-id")
            .or_insert(value);
    }
    resp
}

/// The handler envelope every side endpoint opens with: the bearer token,
/// the credential it names (JWT or secret key), and the post-auth key gates.
/// The error is the finished refusal the caller returns as is, so every
/// endpoint refuses a missing token, a bad credential, a disabled key, an
/// inactive tenant or a closed schedule with the same status and body.
// The error is a finished response, like `gate_resolved_key`'s.
#[allow(clippy::result_large_err)]
pub(crate) async fn authenticate_and_gate(
    state: &AppState,
    headers: &HeaderMap,
    client: &crate::client::ClientInfo,
) -> Result<crate::jwt_auth::Credential, Response<Body>> {
    let Some(secret) = bearer(headers) else {
        return Err(error_json(StatusCode::UNAUTHORIZED, "missing bearer token"));
    };
    let cred = crate::jwt_auth::authenticate_credential(state, &secret).await?;
    gate_resolved_key(state, &cred.resolved, client)?;
    Ok(cred)
}

/// `s` percent-encoded as one URL component: everything but RFC 3986's
/// unreserved characters is escaped, so a value can add neither a path
/// segment nor a query parameter. One encoder for every upstream URL the
/// gateway builds from a caller's or a model's text.
pub(crate) fn encode_url_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Headers for a call to `target`: the client's forwarded headers, then the
/// model's own, then its key, the order the pipeline applies.
pub(crate) fn upstream_headers(client: &HeaderMap, target: &Target) -> HeaderMap {
    let mut out = forward_headers(client);
    out.extend(target.headers.clone());
    if let Some(key) = &target.api_key {
        if let Ok(v) = header::HeaderValue::from_str(&format!("Bearer {key}")) {
            out.insert(header::AUTHORIZATION, v);
        }
    }
    out
}

/// The per-request timeout the pipeline would use for `route`: the model's
/// own when it names one, else the gateway's.
pub(crate) fn request_timeout(state: &AppState, route: &ResolvedModel) -> Duration {
    route
        .request_timeout_secs
        .filter(|s| *s >= 1)
        .map(|s| Duration::from_secs(s as u64))
        .unwrap_or(state.upstream_timeout)
}

/// One admission to a model's pool, for every endpoint that takes a slot in
/// one: the pipeline, `/v1/search` and `/v1/verdicts`.
pub(crate) struct Admit<'a> {
    pub resolved: &'a ResolvedKey,
    pub model: &'a str,
    pub route: Option<&'a ResolvedModel>,
    pub weight: i64,
    pub cost: u32,
    /// The prompt's estimated tokens (the input part of `cost`): background
    /// work holds slots of the model in proportion to it.
    pub prompt_tokens: u32,
    pub end_user: Option<&'a str>,
    /// A batch item's own share and wait; interactive work has none.
    pub batch: Option<&'a BackgroundShare>,
    /// The conversation a session-keeping route pins to an upstream; none
    /// for work that keeps no session.
    pub session_id: Option<&'a str>,
    /// Names the endpoint in the scheduler alert.
    pub path: &'a str,
}

pub(crate) struct Admitted {
    pub admission: obleth_config::Admission,
    pub permit: obleth_fairshare::Permit,
    pub queue_wait_ms: u32,
}

/// Admission, the same on every endpoint: the model's pool (its background
/// pool for a batch item or a background key, without the interactive
/// timeout), the key's weight and caps, session affinity where the route
/// keeps sessions, the `admission` span when traced, and one refusal shape
/// when the wait runs out (`on_timeout` records the rejection) or the
/// scheduler is gone.
// The error is a finished response, like `gate_resolved_key`'s.
#[allow(clippy::result_large_err)]
pub(crate) async fn admit(
    state: &AppState,
    spec: Admit<'_>,
    tracer: Option<&mut crate::tracer::SpanRecorder>,
    span_extra: serde_json::Value,
    on_timeout: impl FnOnce(u32),
) -> Result<Admitted, Response<Body>> {
    let admission_start = crate::tracer::now_ms();
    let pool = admission_pool(spec.route);
    let background = background_admission(spec.route, spec.batch, spec.resolved, || {
        crate::batches::background_share()
    });
    let pool = match (pool, &background) {
        (obleth_fairshare::PoolKey::Model(m), Some(_)) => obleth_fairshare::PoolKey::Background(m),
        (pool, _) => pool,
    };
    let admit_wait = background.map(|b| b.wait).unwrap_or_else(admission_timeout);
    let mut admit_req = admit_request_for(
        spec.resolved,
        spec.model,
        spec.route,
        spec.weight,
        spec.cost,
        spec.end_user,
    );
    // Background work keeps no session slots: a slot held idle for a
    // session's next step would hold back interactive requests. It holds a
    // slot per `OBLETH_BACKGROUND_SLOT_TOKENS` of prompt, since a long
    // prompt's prefill slows every stream on the replica that runs it.
    match (&background, spec.session_id) {
        (Some(b), _) => {
            admit_req =
                admit_req
                    .background_share(b.share)
                    .slots(obleth_fairshare::background_slots(
                        spec.prompt_tokens,
                        crate::batches::background_slot_tokens(),
                    ))
        }
        (None, Some(session)) if route_keeps_sessions(spec.route) => {
            admit_req = admit_req.session(session)
        }
        _ => {}
    }
    if let Some(r) = spec.route.filter(|r| !r.session_affinity.is_default()) {
        // The model's own session affinity setting, over the gateway's.
        admit_req = admit_req.session_affinity(r.session_affinity.resolve(
            state.session_affinity.is_some(),
            &state.session_affinity_settings,
        ));
    }
    let admitted = match timeout(admit_wait, state.fairshare.admit_to(pool, admit_req)).await {
        Ok(Some(a)) => a,
        Err(_) => {
            on_timeout((crate::tracer::now_ms() - admission_start) as u32);
            return Err(unavailable("timed out waiting for model capacity"));
        }
        Ok(None) => {
            state.alerts.issue(
                "scheduler_unavailable",
                "Fairshare scheduler unavailable",
                format!(
                    "tenant `{}` model `{}` path `{}`",
                    spec.resolved.tenant_name, spec.model, spec.path
                ),
            );
            return Err(error_json(
                StatusCode::SERVICE_UNAVAILABLE,
                "scheduler unavailable",
            ));
        }
    };
    let queue_wait_ms = admitted.waited.as_millis() as u32;
    if let Some(t) = tracer {
        let mut attrs = serde_json::json!({
            "decision": admitted.admission.as_str(),
            "queue_wait_ms": queue_wait_ms,
        });
        if let (Some(into), Some(extra)) = (attrs.as_object_mut(), span_extra.as_object()) {
            into.extend(extra.iter().map(|(k, v)| (k.clone(), v.clone())));
        }
        t.record(
            "admission",
            "proxy_request",
            admission_start,
            (crate::tracer::now_ms() - admission_start) as u32,
            "ok",
            attrs,
        );
    }
    Ok(Admitted {
        admission: admitted.admission,
        permit: admitted.permit,
        queue_wait_ms,
    })
}

/// Post-auth key/tenant gates shared by the passthrough pipeline and native
/// endpoints (e.g. `/v1/verdicts`): key disabled, the key's allowed networks,
/// tenant lifecycle status, the activation/expiry/weekly schedule window, and
/// the near-expiry operator alert. Internal probe keys bypass tenant lifecycle
/// gating. Every data-plane surface that authenticates a credential calls
/// this right after `authenticate_credential`, with the client the request
/// came from; a refusal of a real key for being disabled or used from outside
/// its networks is recorded in the `key_rejections` ledger (folded per key,
/// address and reason, so a hammering client costs nothing extra).
// The error is the finished response every caller returns as-is
// (`return resp`); boxing it would push a deref into each call site for a
// rejection path that is cold anyway.
#[allow(clippy::result_large_err)]
pub(crate) fn gate_resolved_key(
    state: &AppState,
    resolved: &ResolvedKey,
    client: &crate::client::ClientInfo,
) -> Result<(), Response<Body>> {
    if resolved.disabled {
        record_key_rejection(state, resolved, client, KeyRejectionReason::Disabled);
        return Err(error_json(StatusCode::FORBIDDEN, "api key disabled"));
    }
    if !client_network_allowed(resolved, client) {
        record_key_rejection(state, resolved, client, KeyRejectionReason::IpNotAllowed);
        return Err(ip_not_allowed_response(resolved, client));
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

/// Whether the key's network and country limits admit this client (see
/// [`obleth_config::network::client_allowed`]). Work the gateway runs for an
/// owner it already checked (a batch item) is not checked again.
pub(crate) fn client_network_allowed(
    resolved: &ResolvedKey,
    client: &crate::client::ClientInfo,
) -> bool {
    client.in_process
        || obleth_config::network::client_allowed(
            &resolved.allowed_networks,
            &resolved.allowed_countries,
            client.ip,
            client.admitting_country(),
        )
}

/// `403 ip_not_allowed`, in OpenAI's error shape, naming the address the
/// request came from (and, for a key limited by country, its country) so its
/// owner can see why.
pub(crate) fn ip_not_allowed_response(
    resolved: &ResolvedKey,
    client: &crate::client::ClientInfo,
) -> Response<Body> {
    let from = client
        .ip
        .map(|ip| ip.to_string())
        .unwrap_or_else(|| "an unknown address".to_string());
    let message = if resolved.allowed_countries.is_empty() {
        format!("This API key can't be used from {from}. Its owner limited it to certain networks.")
    } else {
        let country = if client.country.is_empty() {
            " (country unknown)".to_string()
        } else if client.country_admits {
            format!(" in {}", client.country)
        } else {
            // Found behind the default trusted proxies: recorded, not believed.
            format!(" in {}, which this gateway can't confirm", client.country)
        };
        format!(
            "This API key can't be used from {from}{country}. \
             Its owner limited the network or country it can be used from."
        )
    };
    let body = serde_json::json!({ "error": {
        "message": message,
        "type": GATEWAY_ERROR_TYPE,
        "param": null,
        "code": "ip_not_allowed",
    } });
    (StatusCode::FORBIDDEN, axum::Json(body)).into_response()
}

/// Note a refused use of a real key in the `key_rejections` ledger. Never
/// waits: the sink folds it into the current window in memory. Gateway-owned
/// probe keys and work the gateway runs itself are not client uses.
fn record_key_rejection(
    state: &AppState,
    resolved: &ResolvedKey,
    client: &crate::client::ClientInfo,
    reason: KeyRejectionReason,
) {
    if resolved.internal || client.in_process {
        return;
    }
    state.telemetry.record_rejection(KeyRejection {
        ts_ms: crate::tracer::now_ms(),
        key_id: resolved.key_id,
        key_name: resolved.key_name.clone(),
        tenant_id: resolved.tenant_id,
        tenant_name: resolved.tenant_name.clone(),
        reason,
        client_ip: client.ip_string(),
        client_country: client.country.clone(),
        user_agent: client.user_agent.clone(),
        path: client.path.clone(),
        count: 1,
    });
}

/// The `type` of an error the gateway itself reports, in OpenAI's error shape.
pub(crate) const GATEWAY_ERROR_TYPE: &str = "obleth_gateway_error";

/// An error about a gateway condition (no capacity, storage down, a key's
/// limit) in OpenAI's error shape with the gateway's own `type`. An error
/// about the request's shape on an OpenAI surface goes through
/// [`invalid_request`] instead, so clients can match on `invalid_request_error`.
pub(crate) fn error_json(status: StatusCode, msg: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": { "message": msg, "type": GATEWAY_ERROR_TYPE } });
    (status, axum::Json(body)).into_response()
}

/// A 503 with `Retry-After`: a passing condition (no capacity in time, Redis
/// down), so clients and SDKs retry instead of giving up.
pub(crate) fn unavailable(msg: &str) -> Response<Body> {
    let mut resp = error_json(StatusCode::SERVICE_UNAVAILABLE, msg);
    resp.headers_mut().insert(
        header::RETRY_AFTER,
        header::HeaderValue::from_static(ADMISSION_RETRY_AFTER_SECS),
    );
    resp
}

/// An error about the request itself on an OpenAI surface: a missing or
/// refused field (`param` names it), an unknown id, a body over the limit.
/// `code` is the OpenAI code when one applies (`unsupported_parameter`,
/// `batch_running`), else null.
pub(crate) fn invalid_request(
    status: StatusCode,
    msg: &str,
    param: Option<&str>,
    code: Option<&str>,
) -> Response<Body> {
    let body = serde_json::json!({ "error": {
        "message": msg, "type": "invalid_request_error", "param": param, "code": code,
    } });
    (status, axum::Json(body)).into_response()
}

/// `400` for a request carrying a field the model is set to refuse.
fn rejected_field_response(model: &str, field: &str) -> Response<Body> {
    invalid_request(
        StatusCode::BAD_REQUEST,
        &format!(
            "`{field}` is not supported by model `{model}`. Remove it from the request and retry."
        ),
        Some(field),
        Some("unsupported_parameter"),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        admission_pool, admit_request_for, background_admission, backoff_for, build_targets,
        build_upstream_url, canonical_path, compute_modality_cost, context_overflow_response,
        conversation_seed, end_user_for, forward_headers, has_input_guardrails, has_path_traversal,
        input_guardrails_unscannable, is_multipart_endpoint, is_retryable_status,
        looks_like_context_length_error, output_guardrails_unenforceable, prepare_upstream_body,
        request_type_for_path, requires_registered_model, resolve_conversation,
        response_cache_name, route_keeps_sessions, strip_tool_strict, sure_overflow,
        tenant_active_now, RequestMeta, END_USER_HEADER, SEED_CAP,
    };
    use super::{model_permitted, variant_called};
    use crate::router::{BoonGrants, Candidate, Intent, RequestFeatures, RouterWeights};

    use axum::http::{HeaderMap, Method, StatusCode};
    use chrono::{DateTime, TimeZone, Utc};
    use obleth_admin::{session_hash_order, weighted_order};
    use obleth_config::{ResolvedEndpoint, ResolvedKey, ResolvedModel, WeeklyWindow};
    use std::collections::HashMap;
    use std::sync::Arc;
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

    pub(super) fn key_with_schedule(
        timezone: &str,
        active_from: Option<DateTime<Utc>>,
        active_until: Option<DateTime<Utc>>,
        weekly_windows: Option<Vec<WeeklyWindow>>,
    ) -> ResolvedKey {
        ResolvedKey {
            key_id: Uuid::nil(),
            tenant_id: Uuid::nil(),
            tenant_name: "t".into(),
            key_name: "k".into(),
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
            end_user_fairshare: false,
            background: false,
            allowed_models: None,
            internal: false,
            tracing_enabled: false,
            guardrails_policy: None,
            compression_policy: None,
            synthetic: false,
            allowed_networks: Vec::new(),
            allowed_countries: Vec::new(),
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
    fn odd_spellings_of_a_path_are_refused() {
        for path in [
            "/v1/./videos",
            "/./v1/responses",
            "/v1/videos/.",
            "/v1/%76ideos/vid_1",
            "/v1/%56IDEOS",
            "/v1/chat%2Dcompletions",
            "/v1/x%7e",
            "/v1/x%",
            "/v1/x%2",
            "/v1/x%zz",
        ] {
            assert!(has_path_traversal(path), "{path}");
        }
        for path in [
            "/v1/models/my%20model",
            "/v1/models/org%3Amodel",
            "/v1/models/gpt.4",
            "/v1/files/file-abc.jsonl",
        ] {
            assert!(!has_path_traversal(path), "{path}");
        }
    }

    #[test]
    fn post_paths_are_canonicalized_before_surface_dispatch() {
        assert_eq!(
            canonical_path(&Method::POST, "/responses").as_deref(),
            Some("/v1/responses")
        );
        assert_eq!(
            canonical_path(&Method::POST, "/v1/responses/").as_deref(),
            Some("/v1/responses")
        );
        assert_eq!(
            canonical_path(&Method::POST, "/messages").as_deref(),
            Some("/v1/messages")
        );
        assert_eq!(
            canonical_path(&Method::POST, "/messages/count_tokens/").as_deref(),
            Some("/v1/messages/count_tokens")
        );
        assert_eq!(
            canonical_path(&Method::POST, "/verdicts").as_deref(),
            Some("/v1/verdicts")
        );
        assert_eq!(
            canonical_path(&Method::POST, "/v1/chat/completions//").as_deref(),
            Some("/v1/chat/completions")
        );
        // Repeated slashes collapse, so a doubled prefix cannot skip a shim.
        assert_eq!(
            canonical_path(&Method::POST, "//v1/responses").as_deref(),
            Some("/v1/responses")
        );
        assert_eq!(
            canonical_path(&Method::POST, "/v1//messages").as_deref(),
            Some("/v1/messages")
        );
        // Already canonical, or nothing to trim: untouched.
        assert_eq!(canonical_path(&Method::POST, "/v1/chat/completions"), None);
        assert_eq!(canonical_path(&Method::POST, "/"), None);
        // Normalization runs before the shim dispatch in `proxy_handler`.
        let src = include_str!("proxy.rs");
        let handler = src
            .find("pub async fn proxy_handler(")
            .expect("proxy_handler");
        let canon = handler
            + src[handler..]
                .find("canonical_path(req.method(), req.uri().path())")
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
        // Intent and the sticky complaint question both honor the policy.
        assert!(src.contains("let skip_classifier = has_input_guardrails(&resolved);"));
        assert_eq!(
            src.matches("                skip_classifier,\n            ),")
                .count(),
            2
        );
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
    fn a_speculation_stream_is_recorded_as_streamed() {
        // The normal dispatch sets `meta.streamed` from the upstream content
        // type; the speculation stream returns before that, so its arm must set
        // it, or its row records no first-token time.
        let src = include_str!("proxy.rs");
        let arm = src
            .find("speculation::Outcome::Stream { driver, drafted_by } => {")
            .expect("the speculation stream arm");
        let body = &src[arm..(arm + 400).min(src.len())];
        assert!(body.contains("accounting.meta.streamed = true;"));
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
    fn response_cache_names_keep_levels_and_variants_apart() {
        use obleth_config::ReasoningLevel;
        assert_eq!(response_cache_name("m", None, None), "m");
        assert_eq!(response_cache_name("m", Some("m-spec"), None), "m-spec");
        assert_eq!(
            response_cache_name("m", None, Some(ReasoningLevel::High)),
            "m@high"
        );
        // A family's level picks the member, so every level is its own
        // entry even when no member translates the level itself.
        assert_ne!(
            response_cache_name("acme-think", Some("acme"), Some(ReasoningLevel::High)),
            response_cache_name("acme-instruct", Some("acme"), Some(ReasoningLevel::None)),
        );
        assert_eq!(
            response_cache_name("acme-think", Some("acme"), Some(ReasoningLevel::High)),
            "acme@high"
        );
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
    pub(super) fn candidate(
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
            model: Arc::new(model),
            healthy: true,
            levels: Vec::new(),
        }
    }

    pub(super) fn with_variants(
        mut c: obleth_config::routing::Candidate,
    ) -> obleth_config::routing::Candidate {
        Arc::make_mut(&mut c.model).boons = vec!["vision".into()];
        Arc::make_mut(&mut c.model).variants = vec![obleth_config::ModelVariant {
            name: "glm-5-3-spec".into(),
            description: "Drafted by a small model, verified by this one".into(),
            boons: vec!["speculation".into()],
        }];
        c
    }

    #[test]
    fn the_variant_header_is_set_only_for_a_variant() {
        use super::{error_json, with_variant_header, MODEL_VARIANT_HEADER};
        let resp = with_variant_header(
            error_json(StatusCode::SERVICE_UNAVAILABLE, "busy"),
            Some("glm-5-3-spec"),
        );
        assert_eq!(resp.headers()[MODEL_VARIANT_HEADER], "glm-5-3-spec");
        let resp = with_variant_header(error_json(StatusCode::OK, "ok"), None);
        assert!(resp.headers().get(MODEL_VARIANT_HEADER).is_none());
    }

    #[test]
    fn model_permitted_by_its_name_or_a_variants_own_name() {
        let mut route = crate::boons::test_support::test_route();
        route.model_name = "glm-5-3".into();
        route.aliases = vec!["glm".into()];
        route.variants = vec![obleth_config::ModelVariant {
            name: "glm-5-3-spec".into(),
            description: String::new(),
            boons: vec![],
        }];
        let mut key = crate::boons::test_support::test_key();
        key.internal = false;
        let check = |list: Option<Vec<&str>>, requested: &str| {
            let mut key = key.clone();
            key.allowed_models = list.map(|l| l.into_iter().map(String::from).collect());
            model_permitted(&key, &route, variant_called(&route, requested))
        };
        // No list: everything.
        assert!(check(None, "glm-5-3"));
        // The model's name covers the model, its alias and its variant.
        assert!(check(Some(vec!["glm-5-3"]), "glm-5-3"));
        assert!(check(Some(vec!["glm-5-3"]), "glm"));
        assert!(check(Some(vec!["glm-5-3"]), "glm-5-3-spec"));
        // A variant's name covers only that variant.
        assert!(check(Some(vec!["glm-5-3-spec"]), "glm-5-3-spec"));
        assert!(!check(Some(vec!["glm-5-3-spec"]), "glm-5-3"));
        // An alias, a family name or an unlisted model grant nothing.
        assert!(!check(Some(vec!["glm"]), "glm"));
        assert!(!check(Some(vec!["glm-family"]), "glm-5-3"));
        assert!(!check(Some(vec!["other"]), "glm-5-3"));
        // Internal callers see everything.
        let mut internal = key.clone();
        internal.internal = true;
        internal.allowed_models = Some(vec!["other".into()]);
        assert!(model_permitted(&internal, &route, None));
    }

    /// A variant's route is built once and then shared; the parent and
    /// unknown names pass through untouched.
    #[tokio::test]
    async fn a_variant_route_is_built_once_and_shared() {
        use super::serve_variant;
        let parent = with_variants(candidate(
            "glm-5-3",
            "glm-5-3-mxfp4",
            "chat",
            "mxfp4",
            &[],
            &[],
        ))
        .model;
        let cache = moka::future::Cache::builder().build();
        let (first, variant) = serve_variant(&cache, parent.clone(), "glm-5-3-spec").await;
        assert_eq!(variant.as_deref(), Some("glm-5-3-spec"));
        assert!(!Arc::ptr_eq(&first, &parent), "the variant adds its boons");
        let (again, _) = serve_variant(&cache, parent.clone(), "glm-5-3-spec").await;
        assert!(Arc::ptr_eq(&first, &again), "served from the cache");
        let (own, none) = serve_variant(&cache, parent.clone(), "glm-5-3").await;
        assert!(Arc::ptr_eq(&own, &parent) && none.is_none());
        // Invalidating the variant's name (as the admin does on any change to
        // the model) builds it afresh.
        cache.invalidate("glm-5-3-spec").await;
        let (rebuilt, _) = serve_variant(&cache, parent.clone(), "glm-5-3-spec").await;
        assert!(!Arc::ptr_eq(&first, &rebuilt));
    }

    #[test]
    fn a_variant_is_served_by_its_parent_with_its_boons_added() {
        use super::{admission_pool, serve_as};
        let parent = with_variants(candidate(
            "glm-5-3",
            "glm-5-3-mxfp4",
            "chat",
            "mxfp4",
            &[],
            &[],
        ))
        .model;

        let (served, variant) = serve_as(parent.clone(), "glm-5-3-spec");
        assert_eq!(variant.as_deref(), Some("glm-5-3-spec"));
        assert_eq!(served.boons, vec!["vision", "speculation"]);
        // One backend, one pool: the variant queues with its parent and keeps
        // every capacity, pricing and upstream field.
        assert_eq!(served.model_name, "glm-5-3");
        assert_eq!(admission_pool(Some(&served)), admission_pool(Some(&parent)));
        assert_eq!(
            admission_pool(Some(&served)),
            obleth_fairshare::PoolKey::Model("glm-5-3".into())
        );
        assert_eq!(served.max_in_flight, parent.max_in_flight);
        assert_eq!(served.capacity_mode, parent.capacity_mode);
        assert_eq!(served.upstream_model, parent.upstream_model);
        assert_eq!(served.endpoints, parent.endpoints);
        assert_eq!(served.input_cost_per_token, parent.input_cost_per_token);

        // The model's own name (and an alias) serve it exactly as it is.
        for name in ["glm-5-3", "glm-5-3-fp8"] {
            let (served, variant) = serve_as(parent.clone(), name);
            assert!(variant.is_none());
            assert!(std::sync::Arc::ptr_eq(&served, &parent), "{name}");
        }
    }

    pub(super) fn model_with(endpoints: Vec<ResolvedEndpoint>) -> obleth_config::ResolvedModel {
        obleth_config::ResolvedModel {
            description: String::new(),
            model_name: "m".into(),
            aliases: Vec::new(),
            lifecycle: Default::default(),
            reasoning: Default::default(),
            listed: true,
            rejected_request_fields: Vec::new(),
            session_affinity: Default::default(),
            variants: Vec::new(),
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
    fn strip_tool_strict_removes_only_the_tool_flag() {
        let mut json = serde_json::json!({
            "model": "m",
            "tools": [
                {"type": "function", "function": {
                    "name": "edit",
                    "strict": true,
                    "parameters": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {"strict": {"type": "boolean"}}
                    }
                }},
                {"type": "function", "function": {"name": "plain", "parameters": {}}}
            ],
            "response_format": {"type": "json_schema", "json_schema": {"name": "x", "strict": true}}
        });
        assert!(strip_tool_strict(&mut json));
        let edit = &json["tools"][0]["function"];
        assert!(edit.get("strict").is_none());
        // The schema is untouched, including a property named `strict`.
        assert_eq!(edit["parameters"]["additionalProperties"], false);
        assert!(edit["parameters"]["properties"]["strict"].is_object());
        assert_eq!(json["tools"][1]["function"]["name"], "plain");
        assert_eq!(json["response_format"]["json_schema"]["strict"], true);
    }

    #[test]
    fn strip_tool_strict_reports_no_change() {
        let mut no_tools = serde_json::json!({"model": "m", "messages": []});
        assert!(!strip_tool_strict(&mut no_tools));
        let mut without_flag = serde_json::json!({
            "tools": [{"type": "function", "function": {"name": "f"}}]
        });
        assert!(!strip_tool_strict(&mut without_flag));
        let mut odd = serde_json::json!({"tools": "not-a-list"});
        assert!(!strip_tool_strict(&mut odd));
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
    pub(super) fn handler_source() -> &'static str {
        let src = include_str!("proxy.rs");
        let end = src.find("\nmod tests {").expect("the test module");
        &src[..end]
    }

    #[test]
    fn only_a_prompt_well_over_the_window_is_refused_up_front() {
        assert_eq!(sure_overflow(262_144, 200_000), None);
        assert_eq!(sure_overflow(262_144, 262_144), None);
        assert_eq!(sure_overflow(262_144, 524_287), None);
        assert_eq!(sure_overflow(262_144, 524_288), Some(262_144));
        assert_eq!(sure_overflow(0, 10_000_000), None);
        assert_eq!(sure_overflow(-1, 10_000_000), None);
        let resp = context_overflow_response("too long");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(looks_like_context_length_error(
            "This model's maximum context length is 8 tokens, and the prompt is about 9 tokens."
        ));
    }

    /// A conversation with tools and an image reserves the same whether it
    /// arrives as chat completions or as Anthropic messages, and more than
    /// the same conversation without them.
    #[test]
    fn chat_and_messages_reserve_the_same_for_one_conversation() {
        use obleth_tokenizer::Tokenizer;
        let tk = obleth_tokenizer::HeuristicTokenizer::new();
        let messages = serde_json::json!({
            "model": "m",
            "max_tokens": 256,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is in this picture?"},
                {"type": "image", "source": {"type": "url", "url": "https://example.com/x.png"}}
            ]}],
            "tools": [{"name": "lookup", "description": "Look a word up.",
                       "input_schema": {"type": "object", "properties": {"q": {"type": "string"}}}}]
        });
        let chat = serde_json::json!({
            "model": "m",
            "max_tokens": 256,
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "what is in this picture?"},
                {"type": "image_url", "image_url": {"url": "https://example.com/x.png"}}
            ]}],
            "tools": [{"type": "function", "function": {"name": "lookup", "description": "Look a word up.",
                       "parameters": {"type": "object", "properties": {"q": {"type": "string"}}}}}]
        });
        let translated = crate::messages::to_chat_request(&messages).expect("translates");
        let via_messages = tk.estimate_request(&translated);
        let via_chat = tk.estimate_request(&chat);
        assert_eq!(via_messages, via_chat);

        let plain = serde_json::json!({
            "model": "m",
            "max_tokens": 256,
            "messages": [{"role": "user", "content": "what is in this picture?"}]
        });
        let bare = tk.estimate_request(&plain).input_tokens;
        assert!(via_chat.input_tokens > bare + obleth_tokenizer::IMAGE_PART_TOKENS);
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
    fn resolve_reads_prompt_cache_key() {
        let json = serde_json::json!({
            "prompt_cache_key": "pck-7",
            "messages":[{"role":"user","content":"hi"}]
        });
        let c = resolve_conversation(&HeaderMap::new(), &json, Uuid::nil(), true);
        assert_eq!(c.value, "pck-7");
        assert_eq!(c.source.as_str(), "client");
    }

    #[test]
    fn derived_tells_apart_conversations_behind_a_long_system_prompt() {
        let tid = Uuid::nil();
        let system = "s".repeat(3 * SEED_CAP);
        let opening = |task: &str| {
            serde_json::json!({"messages":[
                {"role":"system","content":system},
                {"role":"user","content":task}
            ]})
        };
        let a = resolve_conversation(&HeaderMap::new(), &opening("fix the parser"), tid, true);
        let b = resolve_conversation(&HeaderMap::new(), &opening("write the docs"), tid, true);
        assert_ne!(
            a.value, b.value,
            "the first user message must reach the hash"
        );
        let later = serde_json::json!({"messages":[
            {"role":"system","content":system},
            {"role":"user","content":"fix the parser"},
            {"role":"assistant","content":"Reading it."},
            {"role":"tool","content":"fn parse() {}"},
            {"role":"user","content":"now run the tests"}
        ]});
        let a2 = resolve_conversation(&HeaderMap::new(), &later, tid, true);
        assert_eq!(
            a.value, a2.value,
            "a conversation keeps its id across turns"
        );
    }

    #[test]
    fn seed_under_one_cap_is_unchanged() {
        // What a single shared cap produced: every part in full, separated.
        let json = serde_json::json!({"messages":[
            {"role":"system","content":"You are helpful."},
            {"role":"developer","content":"Answer briefly."},
            {"role":"user","content":"What is Rust?"}
        ]});
        assert_eq!(
            conversation_seed(&json).as_deref(),
            Some("You are helpful.\u{1f}Answer briefly.\u{1f}What is Rust?")
        );
        let lone = "u".repeat(2 * SEED_CAP);
        let json = serde_json::json!({"messages":[{"role":"user","content":lone}]});
        assert_eq!(conversation_seed(&json).map(|s| s.len()), Some(SEED_CAP));
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
    fn sessions_stay_put_unless_load_balanced_across_endpoints() {
        let two = || {
            vec![
                endpoint("a", "http://a/v1", 100, 100, true, true),
                endpoint("b", "http://b/v1", 100, 100, true, true),
            ]
        };
        let mode = |endpoints, mode: &str| {
            let mut m = model_with(endpoints);
            m.endpoint_selection_mode = mode.to_string();
            m
        };
        assert!(!route_keeps_sessions(None));
        assert!(route_keeps_sessions(Some(&mode(
            Vec::new(),
            "load_balance"
        ))));
        assert!(route_keeps_sessions(Some(&mode(two(), "failover"))));
        assert!(route_keeps_sessions(Some(&mode(two(), "session_hash"))));
        assert!(!route_keeps_sessions(Some(&mode(two(), "load_balance"))));
        let one_up = vec![
            endpoint("a", "http://a/v1", 100, 100, true, true),
            endpoint("b", "http://b/v1", 100, 100, true, false),
        ];
        assert!(route_keeps_sessions(Some(&mode(one_up, "load_balance"))));
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
            end_user: String::new(),
            model_variant: String::new(),
            reasoning_effort: String::new(),
            reasoning_effort_requested: String::new(),
            model_family: String::new(),
            streamed: false,
            failure: None,
            crash_fp: String::new(),
            client: crate::client::ClientInfo::default(),
        };
        assert_eq!(meta.device_id, "dev-1");
    }

    pub(super) fn minimal_model(name: &str) -> ResolvedModel {
        ResolvedModel {
            description: String::new(),
            model_name: name.to_string(),
            aliases: Vec::new(),
            lifecycle: Default::default(),
            reasoning: Default::default(),
            listed: true,
            rejected_request_fields: Vec::new(),
            session_affinity: Default::default(),
            variants: Vec::new(),
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
            model: Arc::new(minimal_model("solo")),
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
        assert_eq!(
            picked.map(|m| m.model_name.clone()),
            Some("solo".to_string())
        );
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
        let req = admit_request_for(&resolved, "m", Some(&route), 77, 123, None);
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
        assert_eq!(req.end_user, None);

        // Zero and negative caps mean "no cap".
        resolved.max_in_flight = Some(0);
        resolved.key_max_in_flight = Some(-1);
        let req = admit_request_for(&resolved, "m", None, 1, 1, None);
        assert_eq!(req.tenant_max_in_flight, None);
        assert_eq!(req.key_max_in_flight, None);
        assert_eq!(req.model_max_in_flight, None);
    }

    #[test]
    fn an_end_user_queues_under_a_key_derived_from_the_real_key() {
        let mut resolved = key_with_schedule("UTC", None, None, None);
        resolved.key_id = Uuid::new_v4();
        resolved.tenant_id = Uuid::new_v4();
        resolved.end_user_fairshare = true;
        resolved.key_weight = 40;
        resolved.key_max_in_flight = Some(3);
        let alice = admit_request_for(&resolved, "m", None, 100, 10, Some("alice"));
        let bob = admit_request_for(&resolved, "m", None, 100, 10, Some("bob"));

        assert_eq!(
            alice.key,
            obleth_config::end_user_key_id(resolved.key_id, "alice")
        );
        assert_ne!(alice.key, bob.key, "each end user gets their own place");
        assert_ne!(alice.key, resolved.key_id);
        assert_eq!(alice.tenant, resolved.tenant_id, "still the key's tenant");
        // The key's weight and cap apply to each end user.
        assert_eq!(alice.key_weight, 40);
        assert_eq!(alice.key_max_in_flight, Some(3));
        assert_eq!(
            alice.end_user,
            Some(obleth_fairshare::EndUser {
                parent_key: resolved.key_id,
                name: "alice".into()
            })
        );
    }

    #[test]
    fn a_background_key_is_admitted_as_background_work() {
        let route = minimal_model("m");
        let mut resolved = key_with_schedule("UTC", None, None, None);

        // An ordinary key is interactive.
        assert_eq!(
            background_admission(Some(&route), None, &resolved, || 0.5),
            None
        );

        resolved.background = true;
        let bg = background_admission(Some(&route), None, &resolved, || 0.5)
            .expect("a background key's routed request is background work");
        assert_eq!(bg.share, 0.5, "the batch runner's share");
        assert_eq!(bg.wait, crate::batches::MAX_ADMISSION_WAIT);
        assert!(bg.wait > super::admission_timeout());

        // No route: today's unrouted admission, untouched.
        assert_eq!(background_admission(None, None, &resolved, || 0.5), None);
        assert_eq!(admission_pool(None), obleth_fairshare::PoolKey::Unrouted);

        // A batch item keeps its own share and wait.
        let batch = super::BackgroundShare {
            share: 0.25,
            admission_wait: Duration::from_secs(7),
        };
        let item = background_admission(Some(&route), Some(&batch), &resolved, || 0.5)
            .expect("batch item");
        assert_eq!(item.share, 0.25);
        assert_eq!(item.wait, Duration::from_secs(7));
    }

    /// A background key's request, admitted the way the pipeline admits it,
    /// fills only its share of the model and yields to interactive callers.
    #[tokio::test]
    async fn a_background_keys_requests_take_only_spare_capacity() {
        use obleth_fairshare::{Admitted, FairShare, PoolKey, StaticCapacity};
        use tokio::task::JoinHandle;

        let fs = FairShare::start(
            std::sync::Arc::new(StaticCapacity::new(1024)),
            obleth_config::FairshareAlgorithm::default(),
            32,
        );
        let mut route = minimal_model("m");
        route.max_in_flight = Some(4);
        let mut runner = key_with_schedule("UTC", None, None, None);
        runner.key_id = Uuid::new_v4();
        runner.tenant_id = Uuid::new_v4();
        runner.background = true;
        let mut person = key_with_schedule("UTC", None, None, None);
        person.key_id = Uuid::new_v4();
        person.tenant_id = Uuid::new_v4();

        // What the pipeline does: the pool, then the share on the request.
        let plan = |key: &ResolvedKey| {
            let bg = background_admission(Some(&route), None, key, || 0.75);
            let pool = match (admission_pool(Some(&route)), &bg) {
                (PoolKey::Model(m), Some(_)) => PoolKey::Background(m),
                (pool, _) => pool,
            };
            let mut req = admit_request_for(key, "m", Some(&route), 100, 1, None);
            if let Some(b) = bg {
                req = req.background_share(b.share);
            }
            (pool, req)
        };
        let admit = |key: &ResolvedKey| -> JoinHandle<Option<Admitted>> {
            let (pool, req) = plan(key);
            let fs = fs.clone();
            tokio::spawn(async move { fs.admit_to(pool, req).await })
        };
        async fn still_waiting(h: &JoinHandle<Option<Admitted>>, why: &str) {
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert!(!h.is_finished(), "{why}");
        }
        async fn granted(h: JoinHandle<Option<Admitted>>) -> Admitted {
            tokio::time::timeout(Duration::from_secs(2), h)
                .await
                .expect("admitted in time")
                .expect("task ran")
                .expect("scheduler alive")
        }

        assert_eq!(plan(&runner).0, PoolKey::Background("m".into()));
        assert_eq!(plan(&person).0, PoolKey::Model("m".into()));

        // The share: 3 of the model's 4 slots, and no more.
        let mut held = Vec::new();
        for _ in 0..3 {
            held.push(granted(admit(&runner)).await);
        }
        let fourth = admit(&runner);
        still_waiting(&fourth, "past the background share").await;

        // The slot left over is the person's, at once.
        let first_person = granted(admit(&person)).await;
        // The model is full; a second person waits, and the runner's
        // waiting request does not go ahead of them when a slot frees.
        let second_person = admit(&person);
        still_waiting(&second_person, "past the model's slots").await;
        held.pop();
        let second_person = granted(second_person).await;
        still_waiting(&fourth, "background went ahead of a person").await;

        // People leave: the runner gets its slot back.
        drop((first_person, second_person));
        granted(fourth).await;
        drop(held);
    }

    #[test]
    fn end_user_is_read_only_for_keys_that_turned_it_on() {
        let mut resolved = key_with_schedule("UTC", None, None, None);
        let mut headers = HeaderMap::new();
        headers.insert(END_USER_HEADER, "header-user".parse().unwrap());
        let body = serde_json::json!({ "user": "body-user" });

        // Off (the default): an ordinary caller can't split itself up.
        assert_eq!(end_user_for(&resolved, &headers, &body), None);

        resolved.end_user_fairshare = true;
        // The header wins over the body.
        assert_eq!(
            end_user_for(&resolved, &headers, &body).as_deref(),
            Some("header-user")
        );
        // Then the body's `user`, then Anthropic's metadata.user_id.
        let none = HeaderMap::new();
        assert_eq!(
            end_user_for(&resolved, &none, &body).as_deref(),
            Some("body-user")
        );
        let anthropic = serde_json::json!({ "metadata": { "user_id": "claude-user" } });
        assert_eq!(
            end_user_for(&resolved, &none, &anthropic).as_deref(),
            Some("claude-user")
        );
        // A blank header falls through to the body; nothing named means None.
        let mut blank = HeaderMap::new();
        blank.insert(END_USER_HEADER, "   ".parse().unwrap());
        assert_eq!(
            end_user_for(&resolved, &blank, &body).as_deref(),
            Some("body-user")
        );
        assert_eq!(end_user_for(&resolved, &none, &serde_json::json!({})), None);
        // Non-string `user` values are ignored rather than stringified.
        assert_eq!(
            end_user_for(&resolved, &none, &serde_json::json!({ "user": 42 })),
            None
        );
    }

    #[test]
    fn the_end_user_header_is_not_sent_upstream() {
        let mut headers = HeaderMap::new();
        headers.insert(END_USER_HEADER, "alice".parse().unwrap());
        headers.insert("x-request-note", "kept".parse().unwrap());
        let fwd = forward_headers(&headers);
        assert!(fwd.get(END_USER_HEADER).is_none());
        assert_eq!(fwd.get("x-request-note").unwrap(), "kept");
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
        let info = model_info_entry(&route, true, &Default::default());
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
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    /// Source with all whitespace removed, so pins survive reformatting.
    pub(super) fn squash(s: &str) -> String {
        s.chars().filter(|c| !c.is_whitespace()).collect()
    }

    /// `run_pipeline`'s source, for ordering pins (there is no
    /// AppState harness in this crate: it needs live Redis and ClickHouse).
    fn handler() -> &'static str {
        let src = include_str!("proxy.rs");
        let src = &src[..src.find("\nmod tests {").expect("the test module")];
        let start = src.find("async fn run_pipeline(").expect("run_pipeline");
        let end = start
            + src[start..]
                .find("\n/// What an admitted request holds against its budgets")
                .expect("end of the handler");
        &src[start..end]
    }

    /// The source of the shared `admit()`, which the pipeline, search and
    /// verdicts all run their admission through.
    fn shared_admission() -> &'static str {
        let src = include_str!("proxy.rs");
        let src = &src[..src.find("\nmod tests {").expect("the test module")];
        let start = src.find("pub(crate) async fn admit(").expect("admit");
        let end = start
            + src[start..]
                .find("\npub(crate) fn gate_resolved_key(")
                .expect("end of admit");
        &src[start..end]
    }

    #[test]
    fn a_family_is_logged_apart_from_variants() {
        let mut meta = super::RequestMeta {
            session_id: String::new(),
            session_id_source: "none",
            request_type: "chat",
            device_id: String::new(),
            end_user: String::new(),
            model_variant: String::new(),
            reasoning_effort: String::new(),
            reasoning_effort_requested: String::new(),
            model_family: String::new(),
            streamed: false,
            failure: None,
            crash_fp: String::new(),
            client: crate::client::ClientInfo::default(),
        };
        super::record_called_name(&mut meta, Some("acme"), Some("acme"));
        assert_eq!(meta.model_family, "acme");
        assert_eq!(meta.model_variant, "");
        super::record_called_name(&mut meta, Some("glm-5-3-spec"), None);
        assert_eq!(meta.model_variant, "glm-5-3-spec");
    }

    #[test]
    fn a_variant_is_admitted_to_its_parents_pool() {
        let h = squash(handler());
        let serve = h
            .find("serve_variant(&state.variant_cache,r,&model)")
            .expect("variant resolution");
        let adopt = h
            .find("model=r.model_name.clone();")
            .expect("parent name adopted");
        let admit = h
            .find("letadmitted=matchBox::pin(admit(&state,Admit{resolved:&resolved,model:&model,route:route.as_deref(),")
            .expect("admission keyed by the served route");
        let pool = admit;
        assert!(serve < adopt && adopt < pool);
        assert!(squash(shared_admission()).contains("letpool=admission_pool(spec.route);"));
        // The ledger and the allowlist see the parent name; the variant rides
        // on the request's metadata for its usage row and response header.
        assert!(h[serve..pool].contains("record_called_name(&mutreq_meta,"));
        assert!(h[serve..pool].contains("served_variant.clone_from(&variant);"));
    }

    #[test]
    fn the_lifecycle_gate_runs_after_resolution_and_before_the_allowlist() {
        let h = squash(handler());
        let serve = h
            .find("serve_variant(&state.variant_cache,r,&model)")
            .expect("variant resolution");
        let gate = h.find("crate::lifecycle::gate(").expect("lifecycle gate");
        let allow = h.find("modelnotpermittedfortenant").expect("allowlist");
        let pool = h
            .find("letadmitted=matchBox::pin(admit(&state,Admit{")
            .expect("admission pool");
        // Resolved first (aliases and variants answer for their model), then
        // gated, so a redirect is allowlisted and admitted as the replacement.
        assert!(serve < gate && gate < allow && allow < pool);
        // A refusal is recorded before it is returned.
        let refuse = h.find("Gate::Refuse(resp,reason)").expect("refusal arm");
        let ret = refuse + h[refuse..].find("returnresp;").expect("refusal returns");
        assert!(h[refuse..ret].contains("reject("));
        assert!(h[refuse..ret].contains("status:StatusCode::GONE"));
    }

    #[test]
    fn rejected_request_fields_are_refused_before_anything_is_sent() {
        let h = squash(handler());
        let allow = h.find("modelnotpermittedfortenant").expect("allowlist");
        let check = h
            .find("obleth_config::rejected_request_field(&json,&r.rejected_request_fields)")
            .expect("rejected field check");
        let effort = h.find("r.reasoning.plan(level)").expect("reasoning effort");
        let boons = h.find(".enrich_request(").expect("boons");
        let pool = h
            .find("letadmitted=matchBox::pin(admit(&state,Admit{")
            .expect("admission pool");
        // On the caller's body as the model resolved, before effort and
        // boons add fields of their own and before admission or dispatch.
        assert!(allow < check && check < effort && effort < boons && boons < pool);
        // Recorded as a rejected 400, then returned.
        let ret = check
            + h[check..]
                .find("returnrejected_field_response(&model,field);")
                .expect("refusal returns");
        assert!(h[check..ret].contains("reject("));
        assert!(h[check..ret].contains("status:StatusCode::BAD_REQUEST"));
    }

    #[test]
    fn background_work_is_admitted_in_the_background_pool_without_sessions() {
        // The pipeline hands its batch share and session to the shared admission.
        let pipeline = squash(handler());
        assert!(pipeline
            .contains("batch:batch.as_ref(),session_id:Some(req_meta.session_id.as_str()),"));
        let h = squash(shared_admission());
        let plan = h
            .find("letbackground=background_admission(spec.route,spec.batch,spec.resolved,")
            .expect("background admission is decided");
        let pool = h
            .find("(obleth_fairshare::PoolKey::Model(m),Some(_))=>obleth_fairshare::PoolKey::Background(m)")
            .expect("background work goes to the background pool");
        let wait = h
            .find("letadmit_wait=background.map(|b|b.wait).unwrap_or_else(admission_timeout);")
            .expect("background work waits longer");
        let share = h
            .find("(Some(b),_)=>{admit_req=admit_req.background_share(b.share).slots(obleth_fairshare::background_slots(spec.prompt_tokens,crate::batches::background_slot_tokens(),))}(None,Some(session))ifroute_keeps_sessions(spec.route)=>")
            .expect("background work keeps no session and holds slots by its prompt");
        let admit = h
            .find("state.fairshare.admit_to(pool,admit_req)")
            .expect("admission");
        assert!(plan < pool && pool < wait && wait < share && share < admit);
        assert!(h.contains("crate::batches::background_share()"));
    }

    #[test]
    fn a_models_session_affinity_setting_goes_with_its_admission() {
        let h = squash(shared_admission());
        let set = h
            .find("admit_req=admit_req.session_affinity(r.session_affinity.resolve(")
            .expect("the model's setting is resolved");
        let admit = h
            .find("state.fairshare.admit_to(pool,admit_req)")
            .expect("admission");
        assert!(set < admit);
        // Only a model with a setting of its own overrides the scheduler's.
        assert!(h[..set].contains(".filter(|r|!r.session_affinity.is_default())"));
    }

    #[tokio::test]
    async fn a_rejected_field_answers_in_the_openai_error_shape() {
        let resp = super::rejected_field_response("m", "bad_words");
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let v: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["type"], "invalid_request_error");
        assert_eq!(v["error"]["param"], "bad_words");
        assert_eq!(v["error"]["code"], "unsupported_parameter");
        let msg = v["error"]["message"].as_str().unwrap();
        assert!(msg.contains("`bad_words`") && msg.contains("`m`"), "{msg}");
    }

    #[test]
    fn every_proxied_response_carries_the_lifecycle_notice() {
        let src = squash(include_str!("proxy.rs"));
        let start = src
            .find("asyncfnproxy_handler_inner(")
            .expect("inner handler");
        let body = &src[start..start + 900];
        assert!(body.contains("crate::lifecycle::apply_headers(&mutresp,notice)"));
    }

    #[test]
    fn a_committed_cascade_says_who_drafted_the_answer() {
        let h = squash(handler());
        let spec = h.find("speculation::run(").expect("speculation");
        let dispatch = h.find("letprepared_body").expect("normal dispatch");
        assert_eq!(
            h[spec..dispatch]
                .matches("crate::boons::speculation::DRAFTED_BY_HEADER,content_type_header(&drafted_by),")
                .count(),
            2,
            "both the buffered and the streamed commit carry the header"
        );
        // Nothing after the cascade abstained claims a drafter.
        assert!(!h[dispatch..].contains("DRAFTED_BY_HEADER"));
    }

    #[test]
    fn every_admitted_response_says_how_long_it_queued() {
        let h = squash(handler());
        let admitted = h
            .find("letqueue_wait_ms=admitted.queue_wait_ms;")
            .expect("admission wait");
        let after = &h[admitted..];
        let answers = after.matches("answer.respond(").count();
        assert!(answers >= 7, "found {answers} answers");
        // No answer after admission is built by hand: `Answer::respond` owns
        // the request id, the queue wait and the rest of the header set.
        assert!(!after.contains(".header(\"x-obleth-request-id\""));
        assert!(after.contains("letanswer=Answer{request_id,queue_wait_ms,"));
    }

    /// Every refusal before the upstream goes through `reject`, which measures
    /// the row's `total_ms` from the request's start; none passes a duration
    /// of its own (three used to record 0).
    #[test]
    fn every_refusal_measures_its_own_duration() {
        let h = squash(handler());
        let refusals = h
            .matches("reject(&state,request_id,&resolved,&req_meta,&model,Rejection{")
            .count();
        // Retired, refused field, crash guard, prompt over the window, admission
        // timeout, and the one refusal `reserve_budget` can answer with.
        assert!(refusals >= 6, "found {refusals} refusals");
        assert_eq!(h.matches("since:request_start,").count(), refusals);
        assert!(!h.contains("total_ms:0"));
    }

    #[test]
    fn admission_precedes_the_budget_reservation() {
        let h = handler();
        let admit = h.find("match Box::pin(admit(").expect("admission");
        let reserve = h.find("reserve_budget(&state").expect("reservation");
        assert!(admit < reserve, "a queued request must hold no budget");
        // The wait itself, bounded, lives in the shared admission.
        assert!(shared_admission()
            .contains("timeout(admit_wait, state.fairshare.admit_to(pool, admit_req))"));
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
        // The reservation lives in `reserve_budget`, shared with verdicts.
        let src = squash(include_str!("proxy.rs"));
        let start = src
            .find("pub(crate)asyncfnreserve_budget(")
            .expect("reserve_budget");
        let flat = &src[start..];
        let end = flat.find("Ok(Reserved{").expect("the reservation returns");
        let flat = &flat[..end];
        let key = flat
            .find(".reserve_with_term(&resolved.key_id,")
            .expect("key reserve");
        let tenant = flat
            .find(".reserve_with_term(&resolved.tenant_id,")
            .expect("tenant reserve");
        assert!(key < tenant);
        let releases = flat[tenant..].matches("hold.release().await").count();
        // Rate limited, term exhausted, and fail-closed error.
        assert_eq!(releases, 3);
        // Success hands the hold to settlement before the guard exists.
        assert!(flat[tenant..].contains("hold.hand_off()"));
        // The pipeline takes the helper's answer and records a refusal once.
        let h = squash(handler());
        let reserve = h
            .find("reserve_budget(&state,&resolved,&model,est,est_cost)")
            .expect("pipeline reserve");
        let guard = h
            .find("letmutsettle_guard=accounting.unbilled_guard();")
            .expect("guard");
        assert!(reserve < guard);
        assert!(h[reserve..guard].contains("ifrefusal.ledger{reject("));
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

    pub(super) fn sse(v: serde_json::Value) -> String {
        format!("data: {v}\n\n")
    }

    pub(super) fn content_event(text: &str) -> String {
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

    /// Run chunks through a usage rewriter; the client's copy and the
    /// unstamped one.
    fn run_usage(
        strip: bool,
        stamp: bool,
        chunks: &[&[u8]],
        figures: crate::energy::EnergyFigures,
    ) -> (String, String) {
        let mut usage = crate::sse::UsageRewriter::new("text/event-stream", strip, stamp).unwrap();
        let (mut client, mut unstamped) = (Vec::new(), Vec::new());
        for c in chunks {
            let out = usage.push(Bytes::copy_from_slice(c), || figures);
            client.extend_from_slice(&out.client);
            unstamped.extend_from_slice(&out.unstamped);
        }
        let out = usage.finish(|| figures);
        client.extend_from_slice(&out.client);
        unstamped.extend_from_slice(&out.unstamped);
        (
            String::from_utf8(client).unwrap(),
            String::from_utf8(unstamped).unwrap(),
        )
    }

    fn run_filter(chunks: &[&[u8]]) -> String {
        let (client, unstamped) = run_usage(true, false, chunks, measured(0.0, 0.0));
        assert_eq!(client, unstamped);
        client
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

    fn run_stamp(chunks: &[&[u8]], figures: crate::energy::EnergyFigures) -> String {
        let (client, unstamped) = run_usage(false, true, chunks, figures);
        // The unstamped copy is the stream as the upstream sent it.
        assert_eq!(
            unstamped,
            chunks
                .iter()
                .map(|c| String::from_utf8_lossy(c))
                .collect::<String>()
        );
        client
    }

    #[test]
    fn one_pass_strips_the_injected_usage_and_stamps_what_the_client_asked_for() {
        let a = content_event("Hel");
        let u = usage_event();
        let done = "data: [DONE]\n\n";
        let joined = format!("{a}{u}{done}");
        // The gateway asked for usage on the client's behalf: dropped, not
        // stamped.
        let (client, unstamped) = run_usage(true, true, &[joined.as_bytes()], measured(1.5, 0.5));
        assert_eq!(client, format!("{a}{done}"));
        assert_eq!(unstamped, client);
        // Usage riding on a content chunk is stamped; the cache's copy keeps
        // the upstream's bytes.
        let riding = sse(serde_json::json!({
            "choices": [{ "index": 0, "delta": { "content": "x" } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 },
        }));
        let wire = format!("{a}{riding}{done}");
        let cut = a.len() + 10;
        let (client, unstamped) = run_usage(
            true,
            true,
            &[&wire.as_bytes()[..cut], &wire.as_bytes()[cut..]],
            measured(1.5, 0.5),
        );
        assert_eq!(unstamped, wire);
        assert!(client.starts_with(&a) && client.ends_with(done), "{client}");
        assert!(client.contains("\"energy_wh\":1.5"), "{client}");
    }

    fn measured(energy_wh: f64, co2_g: f64) -> crate::energy::EnergyFigures {
        crate::energy::EnergyFigures {
            energy_wh,
            energy_cost_usd: 0.0,
            co2_g,
        }
    }

    #[test]
    fn the_usage_event_carries_the_measured_energy_and_content_is_untouched() {
        let a = content_event("Hel");
        let u = usage_event();
        let done = "data: [DONE]\n\n";
        let joined = format!("{a}{u}{done}");
        let bytes = joined.as_bytes();
        // Split inside the usage event, as an upstream may.
        let cut = a.len() + 20;
        let out = run_stamp(
            &[&bytes[..cut], &bytes[cut..]],
            measured(0.0263512345, 8.88),
        )
        .replace("\r\n", "\n");
        assert!(out.starts_with(&a), "{out}");
        assert!(out.ends_with(done), "{out}");
        let usage = &out[a.len()..out.len() - done.len()];
        let value: serde_json::Value =
            serde_json::from_str(usage.trim().strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(value["usage"]["prompt_tokens"], 7);
        assert_eq!(value["usage"]["energy_wh"], 0.026351);
        assert_eq!(value["usage"]["co2_g"], 8.88);
        assert_eq!(value["choices"], serde_json::json!([]));
    }

    #[test]
    fn nothing_measured_leaves_the_stream_byte_for_byte() {
        let a = content_event("hi");
        let u = usage_event();
        let done = "data: [DONE]\n\n";
        let joined = format!("{a}{u}{done}");
        assert_eq!(run_stamp(&[joined.as_bytes()], measured(0.0, 0.0)), joined);
        let crlf = joined.replace("\n\n", "\r\n\r\n");
        assert_eq!(run_stamp(&[crlf.as_bytes()], measured(0.0, 0.0)), crlf);
        // Measured energy keeps the stream's own framing.
        let stamped = run_stamp(&[crlf.as_bytes()], measured(1.5, 0.5));
        assert!(stamped.contains("\"energy_wh\":1.5"), "{stamped}");
        assert!(stamped.ends_with("data: [DONE]\r\n\r\n"));
        assert!(!stamped.replace("\r\n", "").contains('\n'), "{stamped}");
    }

    #[test]
    fn the_tool_loops_own_usage_chunk_carries_the_measured_energy() {
        let a = crate::boons::tool_stream::content_chunk("c1", "m", 1, "Hi");
        let u =
            crate::boons::tool_stream::usage_chunk("c1", "m", 1, UpstreamUsage::uncached(575, 13));
        let done = crate::boons::tool_stream::done();
        let out = run_stamp(
            &[a.as_bytes(), u.as_bytes(), done.as_bytes()],
            measured(0.47, 0.16),
        );
        assert!(out.starts_with(&a), "{out}");
        assert!(out.ends_with(done), "{out}");
        let usage = &out[a.len()..out.len() - done.len()];
        let value: serde_json::Value =
            serde_json::from_str(usage.trim().strip_prefix("data: ").unwrap()).unwrap();
        assert_eq!(value["usage"]["prompt_tokens"], 575);
        assert_eq!(value["usage"]["energy_wh"], 0.47);
        assert_eq!(value["usage"]["co2_g"], 0.16);
    }

    #[test]
    fn a_whole_json_answer_carries_the_measured_energy() {
        let body = serde_json::json!({
            "choices": [{ "index": 0, "message": { "role": "assistant", "content": "hi" } }],
            "usage": { "prompt_tokens": 7, "completion_tokens": 3, "total_tokens": 10 },
        });
        let bytes = Bytes::from(serde_json::to_vec(&body).unwrap());
        let out = crate::sse::stamp_body(bytes.clone(), "application/json", measured(2.0, 0.674));
        let value: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(value["usage"]["energy_wh"], 2.0);
        assert_eq!(value["usage"]["co2_g"], 0.674);
        assert_eq!(value["choices"], body["choices"]);
        // Passed through in pieces, it is held and stamped once it ends.
        let mut stamp = crate::sse::UsageRewriter::new("application/json", false, true).unwrap();
        let (a, b) = bytes.split_at(bytes.len() / 2);
        assert!(stamp
            .push(Bytes::copy_from_slice(a), || measured(2.0, 0.674))
            .client
            .is_empty());
        assert!(stamp
            .push(Bytes::copy_from_slice(b), || measured(2.0, 0.674))
            .client
            .is_empty());
        let end = stamp.finish(|| measured(2.0, 0.674));
        assert_eq!(end.client, out);
        assert_eq!(end.unstamped, bytes);
        // Nothing measured, or no usage, sends the body unchanged.
        assert_eq!(
            crate::sse::stamp_body(bytes.clone(), "application/json", measured(0.0, 0.0)),
            bytes
        );
        let plain = Bytes::from_static(b"{\"data\":[]}");
        assert_eq!(
            crate::sse::stamp_body(plain.clone(), "application/json", measured(2.0, 1.0)),
            plain
        );
    }

    #[test]
    fn a_body_past_the_cap_with_no_event_boundary_is_flushed_unfiltered() {
        // A non-SSE (or non-compliant) body that never sends a blank-line
        // terminator must not be held for the life of the stream once it
        // exceeds the cap.
        let none = || measured(0.0, 0.0);
        let body = vec![b'x'; crate::sse::USAGE_MAX_EVENT + 1];
        let mut filter = crate::sse::UsageRewriter::new("text/event-stream", true, false).unwrap();
        let out = filter.push(Bytes::copy_from_slice(&body), none);
        assert_eq!(out.client.as_ref(), body.as_slice());
        // The filter keeps working past the long event (it does not latch
        // into a permanently-disabled state): its end passes on, and a usage
        // event after it is still dropped.
        let done = "data: [DONE]\n\n";
        let u = usage_event();
        let mut tail = Vec::new();
        tail.extend_from_slice(&filter.push(Bytes::from_static(b"\n\n"), none).client);
        tail.extend_from_slice(&filter.push(Bytes::from(format!("{u}{done}")), none).client);
        tail.extend_from_slice(&filter.finish(none).client);
        assert_eq!(tail, format!("\n\n{done}").as_bytes());
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

    #[test]
    fn the_tool_loop_takes_the_dispatched_body_and_nothing_reads_it_after() {
        let src = handler();
        let prepared = src
            .find("prepare_upstream_body(")
            .expect("the body is prepared");
        let hand_off = "loop_plan.request = std::sync::Arc::new(std::mem::take(&mut json));";
        let taken = src.find(hand_off).expect("the plan takes the body");
        assert!(prepared < taken);
        // Past the hand-off the body is `Null`: nothing may read it.
        let rest = &src[taken + hand_off.len()..];
        let reads: Vec<usize> = rest
            .match_indices("json")
            .map(|(at, _)| at)
            .filter(|&at| {
                let before = rest[..at].chars().next_back().unwrap_or(' ');
                let after = rest[at + 4..].chars().next().unwrap_or(' ');
                let word_before = before.is_alphanumeric() || before == '_' || before == '/';
                let word_after = after.is_alphanumeric() || after == '_' || after == '!';
                !word_before && !word_after
            })
            .collect();
        assert!(
            reads.is_empty(),
            "the body is read after the hand-off: {reads:?}"
        );
    }

    #[test]
    fn the_pass_through_stream_watches_raw_upstream_bytes_and_caches_only_clean_answers() {
        let src = squash(handler());
        let stream = src
            .find("//----streamback,inspectingforactualusage,thenreconcile----")
            .expect("the pass-through stream");
        let rest = &src[stream..];
        // The watch sees the bytes as the upstream sent them, before the
        // gateway's own usage chunk is filtered out of the client's copy.
        let watch = rest.find("watch.push(&chunk);").expect("the failure watch");
        let filter = rest.find("usage.push(chunk,").expect("the usage rewriter");
        assert!(watch < filter);
        // The cache keeps the answer without the energy added.
        let cached = rest
            .find("full.extend_from_slice(&unstamped)")
            .expect("the cache copy");
        assert!(filter < cached);
        assert!(rest.contains("letcache_put=ifcacheable&&settled.cacheable{"));
        assert!(
            !rest.contains("estimated_output_tokens"),
            "settles via drained_settlement"
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set (the
    /// target address needs no server, but the test needs a routable network).
    #[tokio::test]
    async fn a_connect_timeout_is_a_connection_error() {
        if std::env::var("OBLETH_TEST_REDIS_URL").is_err() {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        }
        let err = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(300))
            .build()
            .unwrap()
            .post("http://10.255.255.1:9/chat/completions")
            .body("{}")
            .send()
            .await
            .expect_err("a blackholed address never connects");
        assert!(err.is_connect() && err.is_timeout(), "{err:?}");
        assert!(is_connection_error(&err), "{err:?}");
    }

    #[tokio::test]
    async fn a_connection_dropped_before_response_headers_is_a_connection_error() {
        use crate::upstream_failure::test_support::{spawn_failing_upstream, Hangup};
        use std::sync::atomic::Ordering;
        for hangup in [Hangup::Reset, Hangup::Close] {
            let (accepted, base) = spawn_failing_upstream(usize::MAX, hangup, String::new()).await;
            let err = reqwest::Client::new()
                .post(format!("{base}/chat/completions"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(r#"{"model":"m","messages":[]}"#)
                .send()
                .await
                .expect_err("the upstream never answers");
            // So the attempt loop retries it: on a configured retry, and on
            // the one fresh-connection retry it always allows.
            assert!(is_connection_error(&err), "{hangup:?}: {err:?}");
            assert!(!err.is_timeout(), "{hangup:?}");
            assert_eq!(accepted.load(Ordering::SeqCst), 1, "{hangup:?}");
        }
    }

    #[tokio::test]
    async fn a_read_timeout_before_response_headers_is_a_timeout_not_a_connection_error() {
        use crate::upstream_failure::test_support::{spawn_failing_upstream, Hangup};
        use std::sync::atomic::Ordering;
        let (accepted, base) =
            spawn_failing_upstream(usize::MAX, Hangup::Silent, String::new()).await;
        let err = reqwest::Client::builder()
            .read_timeout(Duration::from_millis(200))
            .build()
            .unwrap()
            .post(format!("{base}/chat/completions"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(r#"{"model":"m","messages":[]}"#)
            .send()
            .await
            .expect_err("the upstream never answers");
        // The upstream has the request and is still working on it: retrying
        // on a fresh connection would run the whole generation again.
        assert!(err.is_timeout(), "{err:?}");
        assert!(!is_connection_error(&err), "{err:?}");
        assert_eq!(accepted.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn a_read_timeout_on_send_is_recorded_as_a_timeout() {
        let src = squash(handler());
        let arm = src.find("Ok(Err(e))=>{").expect("the send-error arm");
        let rest = &src[arm..];
        assert!(rest.contains(
            "letread_timed_out=e.is_timeout()&&!e.is_connect();timed_out|=read_timed_out;"
        ));
    }
}

#[cfg(test)]
mod client_error_tests {
    use super::*;

    async fn error_body(resp: Response<Body>) -> (StatusCode, serde_json::Value) {
        let status = resp.status();
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 16)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    /// One limit on every surface: a body one byte over it is refused with
    /// 413 and the same message in the surface's own error shape; a body at
    /// the limit is read whole.
    #[tokio::test]
    async fn a_body_over_the_limit_is_413_on_every_surface_and_at_the_limit_is_read() {
        let Err(too_large) = read_body(Body::from(vec![b'x'; BODY_LIMIT + 1])).await else {
            panic!("a body over the limit was read");
        };
        let (status, json) = error_body(too_large.openai()).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(json["error"]["message"], BODY_TOO_LARGE);
        assert_eq!(json["error"]["type"], GATEWAY_ERROR_TYPE);
        let (status, json) = error_body(BodyTooLarge.anthropic()).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(json["error"]["message"], BODY_TOO_LARGE);
        assert_eq!(json["error"]["type"], "request_too_large");
        let at = read_body(Body::from(vec![b'x'; BODY_LIMIT])).await;
        assert_eq!(at.map(|b| b.len()).ok(), Some(BODY_LIMIT));
    }

    #[tokio::test]
    async fn a_request_error_carries_type_param_and_code() {
        let (status, json) = error_body(invalid_request(
            StatusCode::NOT_FOUND,
            "No such batch: 'b1'",
            Some("id"),
            None,
        ))
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(json["error"]["type"], "invalid_request_error");
        assert_eq!(json["error"]["param"], "id");
        assert!(json["error"]["code"].is_null());
        let (_, json) = error_body(rejected_field_response("m", "logprobs")).await;
        assert_eq!(json["error"]["param"], "logprobs");
        assert_eq!(json["error"]["code"], "unsupported_parameter");
    }
}

#[cfg(test)]
mod pipeline_input_tests {
    use super::*;

    fn key() -> ResolvedKey {
        super::tests::key_with_schedule("UTC", None, None, None)
    }

    /// The surface is what the shim put in the extension; a header a client
    /// sends is not read at all, so it cannot stamp a chat call as Responses.
    #[test]
    fn a_translated_call_is_distinguished_by_the_extension_not_a_header() {
        let mut extensions = http::Extensions::new();
        assert_eq!(PipelineInput::of(&extensions).surface, None);
        extensions.insert(PipelineInput {
            surface: Some(crate::responses::SURFACE),
            ..PipelineInput::default()
        });
        assert_eq!(PipelineInput::of(&extensions).surface, Some("responses"));
        let resolved = key();
        assert_eq!(
            surfaced_request_type(&resolved, "/v1/chat/completions", None),
            "chat"
        );
        assert_eq!(
            surfaced_request_type(&resolved, "/v1/chat/completions", Some("responses")),
            "responses"
        );
        assert_eq!(
            surfaced_request_type(
                &resolved,
                "/v1/chat/completions",
                Some(crate::messages::SURFACE)
            ),
            "messages"
        );
        assert_eq!(
            surfaced_request_type(&resolved, "/v1/chat/completions", Some("something-else")),
            "chat"
        );
    }

    /// No gateway header reaches a model server: directives, the end user and
    /// any session hint stay between the client and the gateway.
    #[test]
    fn no_gateway_header_is_forwarded_upstream() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("x-obleth-boons", "none"),
            ("x-obleth-api-surface", "responses"),
            ("x-obleth-internal-reasoning-effort", "high"),
            (END_USER_HEADER, "ana"),
            ("x-obleth-session", "s1"),
            ("content-type", "application/json"),
            ("x-request-id", "r1"),
        ] {
            headers.insert(name, header::HeaderValue::from_static(value));
        }
        let out = forward_headers(&headers);
        assert!(
            out.keys().all(|k| !k.as_str().starts_with("x-obleth-")),
            "{out:?}"
        );
        assert_eq!(out.len(), 2);
    }

    /// The pipeline takes the caller's credential before it would read a
    /// bearer token, and parses no body when one was handed over.
    #[test]
    fn the_pipeline_uses_the_input_before_authenticating_or_parsing_again() {
        let src = include_str!("proxy.rs");
        let start = src.find("async fn run_pipeline(").expect("run_pipeline");
        let body = &src[start..];
        let pos = |needle: &str| {
            body.find(needle)
                .unwrap_or_else(|| panic!("{needle} in run_pipeline"))
        };
        assert!(pos("input.credential.take()") < pos("bearer(&headers)"));
        assert!(pos("input.body.take()") < pos("read_body(body)"));
        pos("input.surface");
    }
}

#[cfg(test)]
mod answer_tests {
    use super::*;

    #[test]
    fn every_side_endpoint_opens_with_the_shared_envelope() {
        // Each endpoint authenticates and gates through one helper and stamps
        // its request id through one helper, so no endpoint can drift in what
        // it refuses or in the header it answers with.
        let handlers = |src: &'static str| &src[..src.find("\nmod tests {").unwrap_or(src.len())];
        for (name, source) in [
            ("search", handlers(include_str!("search.rs"))),
            ("speech", handlers(include_str!("speech.rs"))),
            ("load", handlers(include_str!("load.rs"))),
            ("mcp", handlers(include_str!("mcp.rs"))),
            ("verdicts", handlers(include_str!("verdicts/mod.rs"))),
        ] {
            assert!(
                source.contains("authenticate_and_gate("),
                "{name} authenticates on its own"
            );
            assert!(
                source.contains("with_request_id("),
                "{name} stamps its request id on its own"
            );
            assert!(
                !source.contains("authenticate_credential("),
                "{name} bypasses the envelope"
            );
            assert!(
                !source.contains("\"missing bearer token\""),
                "{name} words its own refusal"
            );
        }
        for (name, source) in [
            ("search", include_str!("search.rs")),
            ("verdicts", include_str!("verdicts/mod.rs")),
        ] {
            assert!(
                source.contains("proxy::admit("),
                "{name} runs its own admission"
            );
        }
    }

    #[test]
    fn the_request_id_is_stamped_once_and_kept_when_set() {
        let id = Uuid::new_v4();
        let fresh = with_request_id(error_json(StatusCode::NOT_FOUND, "no"), id);
        assert_eq!(
            fresh.headers()["x-obleth-request-id"],
            id.to_string().as_str()
        );
        let mut own = error_json(StatusCode::OK, "ok");
        own.headers_mut().insert(
            "x-obleth-request-id",
            header::HeaderValue::from_static("mine"),
        );
        assert_eq!(
            with_request_id(own, id).headers()["x-obleth-request-id"],
            "mine"
        );
    }

    #[test]
    fn one_url_encoder_for_path_segments_and_query_values() {
        assert_eq!(
            encode_url_component("job:abc/../x y"),
            "job%3Aabc%2F..%2Fx%20y"
        );
        assert_eq!(encode_url_component("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode_url_component("é&="), "%C3%A9%26%3D");
    }

    /// Every answer after admission carries the same header set, whatever
    /// exit built it: the request id, the queue wait, no proxy buffering,
    /// and the boons and compression when boons applied.
    #[tokio::test]
    async fn an_answer_carries_the_whole_header_set() {
        let request_id = Uuid::new_v4();
        let plain = Answer {
            request_id,
            queue_wait_ms: 7,
            boons_applied: &[],
            compression: Some("1000->500"),
        };
        let resp = plain.respond(
            StatusCode::OK,
            header::HeaderValue::from_static("application/json"),
            Body::from("{}"),
        );
        let h = resp.headers();
        assert_eq!(h["x-obleth-request-id"], request_id.to_string().as_str());
        assert_eq!(h[QUEUE_WAIT_HEADER], "7");
        assert_eq!(h[NO_BUFFER_HEADER.0], NO_BUFFER_HEADER.1);
        assert_eq!(h[header::CONTENT_TYPE], "application/json");
        assert!(h.get(crate::boons::BOONS_HEADER).is_none());
        assert!(
            h.get(crate::boons::COMPRESSION_HEADER).is_none(),
            "no boons, no compression header"
        );

        let with_boons = Answer {
            boons_applied: &["compression", "knowledge"],
            ..plain
        };
        let resp = with_boons.respond(
            StatusCode::BAD_GATEWAY,
            content_type_header("text/event-stream"),
            Body::empty(),
        );
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
        let h = resp.headers();
        assert_eq!(h[crate::boons::BOONS_HEADER], "compression,knowledge");
        assert_eq!(h[crate::boons::COMPRESSION_HEADER], "1000->500");
        assert_eq!(h[header::CONTENT_TYPE], "text/event-stream");
        assert_eq!(content_type_header("not a header\n"), "application/json");
    }
}
