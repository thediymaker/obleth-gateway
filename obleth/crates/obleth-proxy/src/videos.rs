//! OpenAI Videos API: job-based video generation.
//!
//! Every other modality is one request and one response. A video is a job:
//!
//! ```text
//! POST   /v1/videos               prompt [+ reference image] -> job id
//! GET    /v1/videos/{id}          status + progress
//! GET    /v1/videos/{id}/content  the finished video
//! DELETE /v1/videos/{id}          drop the job and its file
//! GET    /v1/videos               list jobs
//! ```
//!
//! The create runs down the ordinary pipeline: model resolution (JSON or
//! multipart), the tenant allowlist, input guardrails on `prompt`, fairshare
//! admission, budgets, the upstream model-name swap and headers. It is billed
//! the model's flat `cost_per_video` when it succeeds. What is specific to
//! video happens after the upstream answers: the job id is recorded against
//! the model, the tenant, and the base URL that accepted it (see
//! [`record_create`]) before the id reaches the client.
//!
//! The follow-ups carry the job id and nothing else, so they cannot resolve a
//! model the usual way. [`handle_follow_up`] reads the record instead: it
//! routes the call back to the model and base that created the job, answers
//! "not found" for an id this tenant does not own, and passes the upstream
//! answer through untouched (its status included, for example a not-ready
//! answer while the video renders), streaming the file rather than buffering
//! it.
//! Follow-ups are authenticated and tenant-checked but take no fairshare slot,
//! reserve no budget, and are not billed or written to the usage ledger: they
//! are reads of work already paid for.
//!
//! Not handled here yet: admission that counts outstanding renders rather than
//! in-flight requests, billing on completion, and progress webhooks.

use std::collections::{BTreeMap, HashSet};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use futures_util::StreamExt;
use obleth_config::{ResolvedKey, ResolvedModel};
use tokio::time::timeout;
use uuid::Uuid;

use crate::proxy::{build_upstream_url, error_json, forward_headers, resolve_model, Target};
use crate::state::AppState;

/// The collection path: `POST` creates a job, `GET` lists them.
pub(crate) const VIDEOS_PATH: &str = "/v1/videos";

/// Largest create response read to find the job id. A video object is a few
/// hundred bytes; anything near this is not one.
const CREATE_BODY_MAX: usize = 1024 * 1024;

/// Most jobs a list call considers. Older rows are still followable by id
/// until they are pruned; they just drop off the listing.
const LIST_LIMIT: i64 = 1000;

/// Largest upstream listing read. The backend lists every live job on the
/// deployment, not only the caller's.
const LIST_BODY_MAX: usize = 8 * 1024 * 1024;

/// How long a job record is kept. The backend forgets a finished job an hour
/// after it completes (two for one that never finished), so a day is well past
/// the last moment a follow-up could succeed. `OBLETH_VIDEO_JOB_RETENTION_HOURS`
/// overrides it.
const DEFAULT_JOB_RETENTION: Duration = Duration::from_secs(24 * 3600);

/// How often each replica prunes expired job records.
const PRUNE_INTERVAL: Duration = Duration::from_secs(3600);

/// Upstream response headers a follow-up passes through: the body's framing
/// and type (a download needs its length and filename), caching validators,
/// range support, and `Retry-After` on a 429/503.
const PASSTHROUGH_HEADERS: &[header::HeaderName] = &[
    header::CONTENT_TYPE,
    header::CONTENT_LENGTH,
    header::CONTENT_DISPOSITION,
    header::CONTENT_RANGE,
    header::ACCEPT_RANGES,
    header::ETAG,
    header::LAST_MODIFIED,
    header::CACHE_CONTROL,
    header::RETRY_AFTER,
];

/// The video job table, and only that: the data plane reads no other
/// Postgres state on the request path.
#[derive(Clone)]
pub struct VideoJobStore(obleth_store::Store);

impl VideoJobStore {
    pub fn new(store: obleth_store::Store) -> Self {
        VideoJobStore(store)
    }
}

/// A follow-up call on the Videos API: anything but the create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FollowUp<'a> {
    List,
    Retrieve(&'a str),
    Content(&'a str),
    Delete(&'a str),
}

impl FollowUp<'_> {
    fn job_id(&self) -> Option<&str> {
        match self {
            FollowUp::List => None,
            FollowUp::Retrieve(id) | FollowUp::Content(id) | FollowUp::Delete(id) => Some(id),
        }
    }
}

/// Classify a request as a Videos API follow-up. `None` for the create
/// (`POST /v1/videos`) and for every other path, including Videos API calls
/// this gateway does not serve (which then fall through to the unknown
/// endpoint guard).
pub(crate) fn follow_up<'a>(method: &Method, path: &'a str) -> Option<FollowUp<'a>> {
    let path = path.trim_end_matches('/');
    if path == VIDEOS_PATH {
        return (method == Method::GET).then_some(FollowUp::List);
    }
    let rest = path.strip_prefix(VIDEOS_PATH)?.strip_prefix('/')?;
    match rest.split_once('/') {
        None if method == Method::GET => Some(FollowUp::Retrieve(rest)),
        None if method == Method::DELETE => Some(FollowUp::Delete(rest)),
        Some((id, "content")) if method == Method::GET => Some(FollowUp::Content(id)),
        _ => None,
    }
}

/// True for the create call.
pub(crate) fn is_create(method: &Method, path: &str) -> bool {
    method == Method::POST && path == VIDEOS_PATH
}

/// A job id this gateway will look up and forward. Ids are the upstream's to
/// mint (for example OpenAI's `video_<hex>`), so nothing is assumed beyond
/// URL safety: 1 to 256 printable, non-whitespace ASCII characters, none of
/// the ones that would change how a URL is split or decoded (`/`, `\`, `?`,
/// `#`, `%`), and not a dot segment. The id is percent-encoded as a single
/// path segment when it is forwarded (see [`follow_up_path`]).
fn valid_job_id(id: &str) -> bool {
    (1..=256).contains(&id.len())
        && id != "."
        && id != ".."
        && id
            .bytes()
            .all(|b| b.is_ascii_graphic() && !matches!(b, b'/' | b'\\' | b'?' | b'#' | b'%'))
}

/// `s` percent-encoded as one URL path segment: everything but RFC 3986's
/// unreserved characters is escaped, so no id can add a segment or a query.
fn encode_path_segment(s: &str) -> String {
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

/// The upstream path of a follow-up: the Videos API path, rebuilt with the
/// job id encoded as a single segment rather than copied from the client.
fn follow_up_path(call: FollowUp<'_>) -> String {
    match call {
        FollowUp::List => VIDEOS_PATH.to_string(),
        FollowUp::Retrieve(id) | FollowUp::Delete(id) => {
            format!("{VIDEOS_PATH}/{}", encode_path_segment(id))
        }
        FollowUp::Content(id) => format!("{VIDEOS_PATH}/{}/content", encode_path_segment(id)),
    }
}

/// The not-found answer for an id that is unknown, malformed, or owned by
/// another tenant. One message for all three, so a caller learns nothing
/// about ids it does not hold.
fn no_such_job() -> Response<Body> {
    error_json(StatusCode::NOT_FOUND, "no such video job")
}

/// Record retention, from `OBLETH_VIDEO_JOB_RETENTION_HOURS`.
fn parse_retention(raw: Option<&str>) -> Duration {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|h| *h > 0)
        .map(|h| Duration::from_secs(h.saturating_mul(3600)))
        .unwrap_or(DEFAULT_JOB_RETENTION)
}

/// Prune job records past their retention, once an hour, on every replica.
/// Concurrent prunes are harmless: each deletes by age.
pub(crate) fn spawn_job_pruner(jobs: VideoJobStore) {
    let retention = parse_retention(
        std::env::var("OBLETH_VIDEO_JOB_RETENTION_HOURS")
            .ok()
            .as_deref(),
    );
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(PRUNE_INTERVAL);
        loop {
            tick.tick().await;
            let Ok(age) = chrono::Duration::from_std(retention) else {
                return;
            };
            match jobs.0.prune_video_jobs(chrono::Utc::now() - age).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(pruned = n, "pruned expired video job records"),
                Err(e) => tracing::warn!(error = %e, "video job prune failed"),
            }
        }
    });
}

/// The upstream a job lives on: the configured endpoint (or the model's own
/// `api_base`) whose base URL accepted the create. Health and the enabled
/// flag are deliberately ignored — the job is on that backend whatever its
/// probe says, and no other endpoint can answer for it. `None` when the base
/// is no longer configured on the model.
fn affinity_target(route: &ResolvedModel, base: &str) -> Option<Target> {
    let headers = obleth_admin::upstream_header_map(&route.upstream_headers);
    if let Some(e) = route.endpoints.iter().find(|e| e.api_base == base) {
        return Some(Target {
            base: e.api_base.clone(),
            api_key: e.api_key.clone().or_else(|| route.api_key.clone()),
            headers,
        });
    }
    (route.api_base == base).then(|| Target {
        base: route.api_base.clone(),
        api_key: route.api_key.clone(),
        headers,
    })
}

/// Headers for a call to `target`: the client's forwarded headers, then the
/// model's own, then its key — the same order the pipeline applies.
fn upstream_headers(client: &HeaderMap, target: &Target) -> HeaderMap {
    let mut out = forward_headers(client);
    out.extend(target.headers.clone());
    if let Some(key) = &target.api_key {
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {key}")) {
            out.insert(header::AUTHORIZATION, v);
        }
    }
    out
}

/// The per-request timeout the pipeline would use for this model. It bounds
/// the wait for response headers only; a download then streams under the
/// client's read timeout, however long the file.
fn request_timeout(state: &AppState, route: &ResolvedModel) -> Duration {
    route
        .request_timeout_secs
        .filter(|s| *s >= 1)
        .map(|s| Duration::from_secs(s as u64))
        .unwrap_or(state.upstream_timeout)
}

/// Where a job's route leads, or the status and message explaining why it
/// cannot be followed.
async fn job_target(
    state: &AppState,
    model_name: &str,
    base: &str,
) -> Result<(std::sync::Arc<ResolvedModel>, Target), (StatusCode, &'static str)> {
    let Some(route) = resolve_model(state, model_name).await.filter(|r| r.enabled) else {
        return Err((
            StatusCode::SERVICE_UNAVAILABLE,
            "the model that created this video job is not available",
        ));
    };
    let Some(target) = affinity_target(&route, base) else {
        return Err((
            StatusCode::BAD_GATEWAY,
            "the endpoint that created this video job is no longer configured",
        ));
    };
    Ok((route, target))
}

/// The parts of the client's request a follow-up forwards.
pub(crate) struct Inbound<'a> {
    pub(crate) method: &'a Method,
    /// Empty, or `?` and the query string.
    pub(crate) query: &'a str,
    pub(crate) headers: &'a HeaderMap,
}

/// Serve a follow-up for an authenticated caller.
pub(crate) async fn handle_follow_up(
    state: &AppState,
    resolved: &ResolvedKey,
    call: FollowUp<'_>,
    inbound: Inbound<'_>,
    request_id: Uuid,
) -> Response<Body> {
    let Some(id) = call.job_id() else {
        return list(state, resolved, inbound.headers, request_id).await;
    };
    if !valid_job_id(id) {
        return no_such_job();
    }
    let job = match state
        .video_jobs
        .0
        .get_video_job(id, resolved.tenant_id)
        .await
    {
        Ok(Some(job)) => job,
        Ok(None) => return no_such_job(),
        Err(e) => {
            tracing::warn!(error = %e, "video job lookup failed");
            state.alerts.issue(
                "video_job_lookup_failed",
                "Video job lookup failed",
                format!("tenant `{}` job `{id}`: {e}", resolved.tenant_name),
            );
            return error_json(StatusCode::SERVICE_UNAVAILABLE, "video job lookup failed");
        }
    };
    let (route, target) = match job_target(state, &job.model_name, &job.upstream_base).await {
        Ok(found) => found,
        Err((status, message)) => return error_json(status, message),
    };

    let url = build_upstream_url(&target.base, &follow_up_path(call), inbound.query);
    let send = state
        .http
        .request(inbound.method.clone(), &url)
        .headers(upstream_headers(inbound.headers, &target))
        .send();
    let upstream = match timeout(request_timeout(state, &route), send).await {
        Ok(Ok(resp)) => resp,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, job = id, "video follow-up failed");
            return error_json(StatusCode::BAD_GATEWAY, "upstream request failed");
        }
        Err(_) => return error_json(StatusCode::GATEWAY_TIMEOUT, "upstream request timed out"),
    };
    let status = upstream.status();

    // The upstream has confirmed the job is gone — deleted now, or already
    // expired — so the record goes too. Any other answer keeps it: a failed
    // delete must stay retryable.
    if matches!(call, FollowUp::Delete(_))
        && (status.is_success() || status == StatusCode::NOT_FOUND)
    {
        if let Err(e) = state
            .video_jobs
            .0
            .delete_video_job(id, resolved.tenant_id)
            .await
        {
            tracing::warn!(error = %e, job = id, "video job record delete failed");
        }
    }

    passthrough(upstream, request_id)
}

/// The upstream response as-is: status, the body's framing and type headers,
/// and the body streamed chunk by chunk so an mp4 is never held in memory.
fn passthrough(upstream: reqwest::Response, request_id: Uuid) -> Response<Body> {
    let mut builder = Response::builder().status(upstream.status().as_u16());
    for name in PASSTHROUGH_HEADERS {
        if let Some(value) = upstream.headers().get(name) {
            builder = builder.header(name, value);
        }
    }
    builder = builder.header("x-obleth-request-id", request_id.to_string());
    let body = upstream
        .bytes_stream()
        .map(|chunk| chunk.map_err(std::io::Error::other));
    builder
        .body(Body::from_stream(body))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

/// `GET /v1/videos`: the caller's jobs, as the upstream currently reports
/// them.
///
/// The backend lists every live job on its deployment, whoever made it, so its
/// listing is filtered down to the ids this tenant owns. That keeps the rich
/// objects (status, progress) and naturally leaves out jobs the backend has
/// already expired. One upstream call per distinct model and endpoint the
/// tenant has jobs on — in practice, one. The listing is never used to prune
/// records: the backend answers an unreadable job store with an empty list.
async fn list(
    state: &AppState,
    resolved: &ResolvedKey,
    headers: &HeaderMap,
    request_id: Uuid,
) -> Response<Body> {
    let jobs = match state
        .video_jobs
        .0
        .list_video_jobs(resolved.tenant_id, LIST_LIMIT)
        .await
    {
        Ok(jobs) => jobs,
        Err(e) => {
            tracing::warn!(error = %e, "video job list failed");
            return error_json(StatusCode::SERVICE_UNAVAILABLE, "video job lookup failed");
        }
    };
    let mut owned: BTreeMap<(String, String), HashSet<String>> = BTreeMap::new();
    for job in jobs {
        owned
            .entry((job.model_name, job.upstream_base))
            .or_default()
            .insert(job.job_id);
    }

    let mut data: Vec<serde_json::Value> = Vec::new();
    for ((model_name, base), ids) in owned {
        let (route, target) = match job_target(state, &model_name, &base).await {
            Ok(found) => found,
            Err((status, message)) => return error_json(status, message),
        };
        let url = build_upstream_url(&target.base, VIDEOS_PATH, "");
        let send = state
            .http
            .get(&url)
            .headers(upstream_headers(headers, &target))
            .send();
        let listing = match timeout(request_timeout(state, &route), send).await {
            Ok(Ok(resp)) if resp.status().is_success() => read_capped(resp, LIST_BODY_MAX)
                .await
                .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()),
            Ok(Ok(resp)) => {
                tracing::warn!(status = %resp.status(), model = %model_name, "video list upstream error");
                None
            }
            Ok(Err(e)) => {
                tracing::warn!(error = %e, model = %model_name, "video list upstream failed");
                None
            }
            Err(_) => None,
        };
        let Some(listing) = listing else {
            return error_json(
                StatusCode::BAD_GATEWAY,
                "could not list video jobs upstream",
            );
        };
        if let Some(items) = listing.get("data").and_then(|d| d.as_array()) {
            data.extend(
                items
                    .iter()
                    .filter(|v| {
                        v.get("id")
                            .and_then(|i| i.as_str())
                            .is_some_and(|i| ids.contains(i))
                    })
                    .cloned(),
            );
        }
    }
    data.sort_by(|a, b| {
        let at = |v: &serde_json::Value| v.get("created_at").and_then(|c| c.as_i64()).unwrap_or(0);
        at(b).cmp(&at(a))
    });

    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "object": "list", "data": data })),
    )
        .into_response_with_id(request_id)
}

/// Read a response body up to `max` bytes; `None` past the cap or on a read
/// error.
async fn read_capped(resp: reqwest::Response, max: usize) -> Option<Bytes> {
    let mut buf: Vec<u8> = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.ok()?;
        if buf.len() + chunk.len() > max {
            return None;
        }
        buf.extend_from_slice(&chunk);
    }
    Some(Bytes::from(buf))
}

/// Attach the request id to a response built with axum's `IntoResponse`.
trait WithRequestId {
    fn into_response_with_id(self, request_id: Uuid) -> Response<Body>;
}

impl<T: axum::response::IntoResponse> WithRequestId for T {
    fn into_response_with_id(self, request_id: Uuid) -> Response<Body> {
        let mut resp = self.into_response();
        if let Ok(v) = HeaderValue::from_str(&request_id.to_string()) {
            resp.headers_mut().insert("x-obleth-request-id", v);
        }
        resp
    }
}

/// What the pipeline knows about a successful create, for recording the job.
pub(crate) struct CreatedJob {
    pub(crate) jobs: VideoJobStore,
    pub(crate) http: reqwest::Client,
    pub(crate) model: String,
    pub(crate) tenant_id: Uuid,
    pub(crate) key_id: Uuid,
    /// The target that accepted the create; the job lives there.
    pub(crate) target: Target,
    pub(crate) started: Instant,
}

/// How a successful upstream create ended at the gateway.
pub(crate) enum CreateOutcome {
    /// The job is recorded; the upstream's answer goes to the client and the
    /// create is billed.
    Recorded {
        status: StatusCode,
        content_type: HeaderValue,
        body: Bytes,
        ttft_ms: u32,
    },
    /// The job could not be recorded (or there was no job). Unbilled.
    Failed {
        status: StatusCode,
        message: &'static str,
        ttft_ms: u32,
    },
}

impl CreateOutcome {
    pub(crate) fn status(&self) -> StatusCode {
        match self {
            CreateOutcome::Recorded { status, .. } | CreateOutcome::Failed { status, .. } => {
                *status
            }
        }
    }

    pub(crate) fn ttft_ms(&self) -> u32 {
        match self {
            CreateOutcome::Recorded { ttft_ms, .. } | CreateOutcome::Failed { ttft_ms, .. } => {
                *ttft_ms
            }
        }
    }

    pub(crate) fn billed(&self) -> bool {
        matches!(self, CreateOutcome::Recorded { .. })
    }

    pub(crate) fn into_response(self, request_id: Uuid) -> Response<Body> {
        match self {
            CreateOutcome::Recorded {
                status,
                content_type,
                body,
                ..
            } => Response::builder()
                .status(status)
                .header(header::CONTENT_TYPE, content_type)
                .header("x-obleth-request-id", request_id.to_string())
                .body(Body::from(body))
                .unwrap_or_else(|_| {
                    error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed")
                }),
            CreateOutcome::Failed {
                status, message, ..
            } => error_json(status, message).into_response_with_id(request_id),
        }
    }
}

/// Record the job a successful create returned, before its id reaches the
/// client.
///
/// A job the gateway has not recorded cannot be polled, downloaded or deleted
/// through it, so an id is never handed out unrecorded: when the record cannot
/// be written, the job is deleted upstream (best effort) and the create fails
/// unbilled, leaving the client free to retry. A 2xx without a job id is
/// likewise a failed create.
pub(crate) async fn record_create(job: CreatedJob, upstream: reqwest::Response) -> CreateOutcome {
    let status = StatusCode::from_u16(upstream.status().as_u16()).unwrap_or(StatusCode::OK);
    let content_type = upstream
        .headers()
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("application/json"));
    let body = read_capped(upstream, CREATE_BODY_MAX).await;
    let ttft_ms = job.started.elapsed().as_millis() as u32;
    let job_id = body
        .as_deref()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(b).ok())
        .and_then(|v| v.get("id").and_then(|i| i.as_str()).map(str::to_string))
        .filter(|id| valid_job_id(id));
    let (Some(body), Some(job_id)) = (body, job_id) else {
        tracing::warn!(model = %job.model, "video create returned no usable job id");
        return CreateOutcome::Failed {
            status: StatusCode::BAD_GATEWAY,
            message: "the upstream did not return a video job id",
            ttft_ms,
        };
    };

    match job
        .jobs
        .0
        .insert_video_job(
            &job_id,
            &job.model,
            job.tenant_id,
            job.key_id,
            &job.target.base,
        )
        .await
    {
        Ok(()) => CreateOutcome::Recorded {
            status,
            content_type,
            body,
            ttft_ms,
        },
        Err(e) => {
            tracing::warn!(error = %e, job = %job_id, "video job record failed; cancelling the job");
            let url = build_upstream_url(&job.target.base, &format!("{VIDEOS_PATH}/{job_id}"), "");
            let cancel = job
                .http
                .delete(&url)
                .headers(upstream_headers(&HeaderMap::new(), &job.target))
                .send();
            if let Err(e) = timeout(Duration::from_secs(10), cancel)
                .await
                .map_err(|_| "timed out".to_string())
                .and_then(|r| r.map_err(|e| e.to_string()))
            {
                tracing::warn!(error = %e, job = %job_id, "could not cancel an unrecorded video job");
            }
            CreateOutcome::Failed {
                status: StatusCode::SERVICE_UNAVAILABLE,
                message: "could not record the video job; it was cancelled, retry the request",
                ttft_ms,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follow_ups_are_classified_by_method_and_shape() {
        let get = Method::GET;
        let del = Method::DELETE;
        let post = Method::POST;
        assert_eq!(follow_up(&get, "/v1/videos"), Some(FollowUp::List));
        assert_eq!(follow_up(&get, "/v1/videos/"), Some(FollowUp::List));
        assert_eq!(
            follow_up(&get, "/v1/videos/video_abc"),
            Some(FollowUp::Retrieve("video_abc"))
        );
        assert_eq!(
            follow_up(&get, "/v1/videos/video_abc/content"),
            Some(FollowUp::Content("video_abc"))
        );
        assert_eq!(
            follow_up(&del, "/v1/videos/video_abc"),
            Some(FollowUp::Delete("video_abc"))
        );
        // The create goes down the pipeline, not here.
        assert_eq!(follow_up(&post, "/v1/videos"), None);
        assert!(is_create(&post, "/v1/videos"));
        assert!(!is_create(&get, "/v1/videos"));
        // Calls this gateway does not serve fall through to the unknown
        // endpoint guard.
        assert_eq!(follow_up(&post, "/v1/videos/video_abc/remix"), None);
        assert_eq!(follow_up(&del, "/v1/videos/video_abc/content"), None);
        assert_eq!(follow_up(&get, "/v1/videos/a/b/c"), None);
        assert_eq!(follow_up(&get, "/v1/videosx"), None);
        assert_eq!(follow_up(&get, "/v1/images/generations"), None);
    }

    #[test]
    fn job_ids_are_anything_url_safe() {
        for ok in [
            "video_0123abcdef",
            "video-9",
            "3f2b8c1e-5d4a-4e6f-9a7b-0c1d2e3f4a5b",
            "job.2026.09.24",
            "job:abc:1",
            "a.b",
            "~tilde",
            "...",
        ] {
            assert!(valid_job_id(ok), "{ok}");
        }
        assert!(valid_job_id(&"a".repeat(256)));
        for bad in [
            "",
            ".",
            "..",
            "a/b",
            "../etc",
            "a\\b",
            "video?x=1",
            "video#frag",
            "video%2F..",
            "has space",
            "tab\tid",
            "caf\u{e9}",
        ] {
            assert!(!valid_job_id(bad), "{bad:?}");
        }
        assert!(!valid_job_id(&"a".repeat(257)));
    }

    #[test]
    fn a_job_id_is_forwarded_as_one_encoded_segment() {
        fn decode(s: &str) -> String {
            let bytes = s.as_bytes();
            let mut out = Vec::new();
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'%' {
                    out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            String::from_utf8(out).unwrap()
        }
        for id in [
            "video_0123",
            "job:abc:1",
            "a.b~c_d-e",
            "x+y=z&w",
            "(paren)[br]{cu}!*'@$,;",
            "...",
        ] {
            assert!(valid_job_id(id), "{id}");
            let enc = encode_path_segment(id);
            assert!(
                !enc.contains('/') && !enc.contains('?') && !enc.contains('#'),
                "{enc}"
            );
            assert_eq!(decode(&enc), id, "round trip of {id}");
        }
        assert_eq!(encode_path_segment("job:abc"), "job%3Aabc");
        assert_eq!(
            follow_up_path(FollowUp::Content("job:1")),
            "/v1/videos/job%3A1/content"
        );
        assert_eq!(follow_up_path(FollowUp::Retrieve("v_1")), "/v1/videos/v_1");
        assert_eq!(follow_up_path(FollowUp::Delete("a.b")), "/v1/videos/a.b");
    }

    #[test]
    fn retention_defaults_to_a_day_and_parses_hours() {
        assert_eq!(parse_retention(None), DEFAULT_JOB_RETENTION);
        assert_eq!(parse_retention(Some("0")), DEFAULT_JOB_RETENTION);
        assert_eq!(parse_retention(Some("soon")), DEFAULT_JOB_RETENTION);
        assert_eq!(parse_retention(Some(" 6 ")), Duration::from_secs(6 * 3600));
    }

    #[test]
    fn affinity_picks_the_endpoint_that_took_the_create_whatever_its_health() {
        let mut route = crate::boons::test_support::test_route();
        route.api_base = "http://model-base/v1".into();
        route.api_key = Some("model-key".into());
        route
            .upstream_headers
            .insert("x-route".into(), "video".into());
        route.endpoints = vec![
            obleth_config::ResolvedEndpoint {
                id: "a".into(),
                api_base: "http://a/v1".into(),
                api_key: None,
                priority: 0,
                weight: 1,
                enabled: true,
                healthy: true,
                max_in_flight: None,
            },
            obleth_config::ResolvedEndpoint {
                id: "b".into(),
                api_base: "http://b/v1".into(),
                api_key: Some("b-key".into()),
                priority: 1,
                weight: 1,
                enabled: false,
                healthy: false,
                max_in_flight: None,
            },
        ];
        let b = affinity_target(&route, "http://b/v1").expect("configured endpoint");
        assert_eq!(b.base, "http://b/v1");
        assert_eq!(b.api_key.as_deref(), Some("b-key"));
        assert_eq!(b.headers.get("x-route").unwrap(), "video");
        let a = affinity_target(&route, "http://a/v1").expect("configured endpoint");
        assert_eq!(
            a.api_key.as_deref(),
            Some("model-key"),
            "falls back to the model key"
        );
        let own = affinity_target(&route, "http://model-base/v1").expect("model base");
        assert_eq!(own.base, "http://model-base/v1");
        assert!(affinity_target(&route, "http://gone/v1").is_none());
    }
}

/// The whole data plane against a fake Videos backend. Needs the throwaway
/// datastores: `OBLETH_TEST_DATABASE_URL` (a database whose name contains
/// "test") and `OBLETH_TEST_REDIS_URL`; each test skips when either is unset.
/// ClickHouse is not needed: the usage sink comes up spilling to a temp WAL.
#[cfg(test)]
mod pipeline_tests {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use axum::extract::{Path, State};
    use axum::http::Request;
    use axum::response::IntoResponse;
    use axum::routing::{get, post};
    use axum::Router;
    use moka::future::Cache;

    use super::*;

    /// One create as the backend received it.
    #[derive(Debug, Clone, Default)]
    struct SeenCreate {
        model: String,
        prompt: String,
        file_name: Option<String>,
        authorization: Option<String>,
        route_header: Option<String>,
    }

    #[derive(Default)]
    struct Backend {
        name: &'static str,
        next: AtomicUsize,
        /// id -> (status, created_at)
        jobs: Mutex<BTreeMap<String, (String, i64)>>,
        creates: Mutex<Vec<SeenCreate>>,
        /// Every non-create call: "METHOD /path".
        calls: Mutex<Vec<String>>,
        /// Answer creates with a 400, as the backend does for a bad `size`.
        reject_creates: AtomicBool,
        /// When set, the download sends its first chunk and waits for this
        /// before sending the rest.
        hold: Mutex<Option<Arc<tokio::sync::Notify>>>,
    }

    type Shared = Arc<Backend>;

    const MP4_FIRST: &[u8] = b"\x00\x00\x00\x18ftypmp42first-chunk";
    const MP4_REST: &[u8] = b"...the-rest-of-the-mp4...";

    async fn parse_create(headers: &HeaderMap, body: Bytes) -> SeenCreate {
        let mut seen = SeenCreate {
            authorization: headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
            route_header: headers
                .get("x-route")
                .and_then(|v| v.to_str().ok())
                .map(str::to_string),
            ..Default::default()
        };
        let ct = headers
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        if ct.starts_with("multipart/form-data") {
            let boundary = multer::parse_boundary(ct).expect("boundary");
            let stream =
                futures_util::stream::once(
                    async move { Ok::<Bytes, std::convert::Infallible>(body) },
                );
            let mut form = multer::Multipart::new(stream, boundary);
            while let Some(field) = form.next_field().await.expect("field") {
                let name = field.name().unwrap_or("").to_string();
                let file_name = field.file_name().map(str::to_string);
                let text = String::from_utf8_lossy(&field.bytes().await.unwrap()).into_owned();
                match name.as_str() {
                    "model" => seen.model = text,
                    "prompt" => seen.prompt = text,
                    "input_reference" => seen.file_name = file_name,
                    _ => {}
                }
            }
        } else {
            let v: serde_json::Value = serde_json::from_slice(&body).expect("json create");
            seen.model = v["model"].as_str().unwrap_or("").into();
            seen.prompt = v["prompt"].as_str().unwrap_or("").into();
        }
        seen
    }

    fn backend_app(backend: Shared) -> Router {
        async fn create(
            State(b): State<Shared>,
            headers: HeaderMap,
            body: Bytes,
        ) -> axum::response::Response {
            let seen = parse_create(&headers, body).await;
            b.creates.lock().unwrap().push(seen.clone());
            if b.reject_creates.load(Ordering::SeqCst) {
                return (
                    StatusCode::BAD_REQUEST,
                    axum::Json(
                        serde_json::json!({"error": {"message": "size must be a multiple of 32"}}),
                    ),
                )
                    .into_response();
            }
            let n = b.next.fetch_add(1, Ordering::SeqCst) + 1;
            let id = format!("video_{}{n}", b.name);
            let created = 1_000 + n as i64;
            b.jobs
                .lock()
                .unwrap()
                .insert(id.clone(), ("queued".into(), created));
            (
                StatusCode::CREATED,
                axum::Json(serde_json::json!({
                    "id": id, "object": "video", "model": seen.model,
                    "status": "queued", "progress": 0, "created_at": created,
                })),
            )
                .into_response()
        }
        async fn list(State(b): State<Shared>) -> axum::response::Response {
            b.calls.lock().unwrap().push("GET /v1/videos".into());
            let mut data: Vec<serde_json::Value> = b
                .jobs
                .lock()
                .unwrap()
                .iter()
                .map(|(id, (status, created))| {
                    serde_json::json!({"id": id, "object": "video", "status": status, "created_at": created})
                })
                .collect();
            // A job some other client of the backend made.
            data.push(serde_json::json!({"id": "video_foreign", "object": "video", "status": "completed", "created_at": 5}));
            axum::Json(serde_json::json!({"object": "list", "data": data})).into_response()
        }
        async fn retrieve(
            State(b): State<Shared>,
            Path(id): Path<String>,
        ) -> axum::response::Response {
            b.calls.lock().unwrap().push(format!("GET /v1/videos/{id}"));
            match b.jobs.lock().unwrap().get(&id) {
                Some((status, created)) => axum::Json(serde_json::json!({
                    "id": id, "object": "video", "status": status, "created_at": created,
                }))
                .into_response(),
                None => (StatusCode::NOT_FOUND, "no such video job").into_response(),
            }
        }
        async fn content(
            State(b): State<Shared>,
            Path(id): Path<String>,
        ) -> axum::response::Response {
            b.calls
                .lock()
                .unwrap()
                .push(format!("GET /v1/videos/{id}/content"));
            let status = b.jobs.lock().unwrap().get(&id).map(|j| j.0.clone());
            match status.as_deref() {
                None => (StatusCode::NOT_FOUND, "no such video job").into_response(),
                Some("completed") => {
                    let hold = b.hold.lock().unwrap().clone();
                    let stream = async_stream::stream! {
                        yield Ok::<Bytes, std::io::Error>(Bytes::from_static(MP4_FIRST));
                        if let Some(hold) = hold {
                            hold.notified().await;
                        }
                        yield Ok(Bytes::from_static(MP4_REST));
                    };
                    Response::builder()
                        .status(StatusCode::OK)
                        .header(header::CONTENT_TYPE, "video/mp4")
                        .header(header::CONTENT_LENGTH, MP4_FIRST.len() + MP4_REST.len())
                        .header(
                            header::CONTENT_DISPOSITION,
                            format!("attachment; filename=\"{id}.mp4\""),
                        )
                        .body(Body::from_stream(stream))
                        .unwrap()
                }
                Some(other) => (
                    StatusCode::CONFLICT,
                    axum::Json(serde_json::json!({
                        "error": {"message": format!("video is {other} (40%)"), "type": "not_ready"}
                    })),
                )
                    .into_response(),
            }
        }
        async fn delete(
            State(b): State<Shared>,
            Path(id): Path<String>,
        ) -> axum::response::Response {
            b.calls
                .lock()
                .unwrap()
                .push(format!("DELETE /v1/videos/{id}"));
            match b.jobs.lock().unwrap().remove(&id) {
                Some(_) => axum::Json(
                    serde_json::json!({"id": id, "object": "video.deleted", "deleted": true}),
                )
                .into_response(),
                None => (StatusCode::NOT_FOUND, "no such video job").into_response(),
            }
        }
        Router::new()
            .route("/v1/videos", post(create).get(list))
            .route("/v1/videos/:id", get(retrieve).delete(delete))
            .route("/v1/videos/:id/content", get(content))
            .with_state(backend)
    }

    async fn spawn_backend(name: &'static str) -> (Shared, String) {
        let backend = Arc::new(Backend {
            name,
            ..Default::default()
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = backend_app(backend.clone());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (backend, format!("http://{addr}/v1"))
    }

    struct Gateway {
        state: AppState,
        store: obleth_store::Store,
        wal_dir: std::path::PathBuf,
    }

    impl Drop for Gateway {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.wal_dir);
        }
    }

    /// A real `AppState` on the test datastores, or `None` (skip) without them.
    async fn gateway() -> Option<Gateway> {
        let (Ok(db), Ok(redis_url)) = (
            std::env::var("OBLETH_TEST_DATABASE_URL"),
            std::env::var("OBLETH_TEST_REDIS_URL"),
        ) else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return None;
        };
        let db_name = db
            .rsplit('/')
            .next()
            .unwrap_or("")
            .split('?')
            .next()
            .unwrap_or("");
        assert!(
            db_name.contains("test"),
            "OBLETH_TEST_DATABASE_URL must name a dedicated test database"
        );
        let store = obleth_store::Store::connect(&db).await.expect("postgres");
        store.migrate().await.expect("migrate");
        let redis = obleth_redis::RedisStore::connect(&redis_url)
            .await
            .expect("redis");
        let wal_dir = std::env::temp_dir().join(format!("obleth-video-wal-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&wal_dir).unwrap();
        let telemetry = obleth_telemetry::TelemetrySink::start(
            "http://127.0.0.1:1",
            "obleth_test",
            "default",
            "",
            wal_dir.join("usage.wal").to_str().unwrap(),
            true,
        )
        .await
        .expect("telemetry sink");
        let http = reqwest::Client::new();
        let state = AppState {
            redis,
            fairshare: obleth_fairshare::FairShare::start(
                Arc::new(obleth_fairshare::StaticCapacity::new(64)),
                obleth_config::FairshareAlgorithm::default(),
                64,
            ),
            tokenizer: Arc::new(obleth_tokenizer::HeuristicTokenizer::new()),
            telemetry,
            http: http.clone(),
            upstream_base: "http://127.0.0.1:1/v1".into(),
            upstream_timeout: Duration::from_secs(30),
            key_cache: Cache::builder().build(),
            model_cache: Cache::builder().build(),
            mcp_cache: Cache::builder().build(),
            model_registry: crate::router::ModelRegistry::new(),
            classifier: crate::classifier::Classifier::new(Default::default()),
            output_stats: Default::default(),
            boons: crate::boons::BoonEngine::new(Default::default()),
            tool_cache: Cache::builder().build(),
            metrics: Arc::new(crate::metrics::Metrics::new()),
            fail_open: false,
            alerts: obleth_admin::AlertDispatcher::new(http, Default::default()),
            session_id_derivation: false,
            compressor: None,
            energy: crate::energy::EnergyEngine::new(Default::default()),
            jwt: None,
            knowledge: Arc::new(crate::knowledge::KnowledgeIndex::new()),
            video_jobs: VideoJobStore::new(store.clone()),
        };
        Some(Gateway {
            state,
            store,
            wal_dir,
        })
    }

    const COST: f64 = 0.25;

    fn video_route(name: &str, base: &str) -> ResolvedModel {
        let mut route = crate::boons::test_support::test_route();
        route.model_name = name.into();
        route.upstream_model = "video-upstream".into();
        route.api_base = base.into();
        route.api_key = Some("up-key".into());
        route.model_type = "video".into();
        route.cost_per_video = COST;
        route
            .upstream_headers
            .insert("x-route".into(), "video".into());
        route
    }

    impl Gateway {
        async fn register(&self, route: ResolvedModel) {
            self.state
                .model_cache
                .insert(route.model_name.clone(), Arc::new(route))
                .await;
        }

        /// A key for a fresh tenant with a lifetime cost cap, so its spend is
        /// readable from the term counters. Returns (secret, key).
        async fn tenant(
            &self,
            policy: Option<obleth_config::GuardrailsPolicy>,
        ) -> (String, ResolvedKey) {
            let mut key = crate::boons::test_support::test_key();
            key.key_id = Uuid::new_v4();
            key.tenant_id = Uuid::new_v4();
            key.tenant_name = format!("t-{}", key.tenant_id);
            key.budget_cost_usd = Some(1000.0);
            key.budget_period = Some("lifetime".into());
            key.guardrails_policy = policy;
            let secret = format!("sk-video-{}", Uuid::new_v4());
            self.state
                .key_cache
                .insert(obleth_config::hash_api_key(&secret), Arc::new(key.clone()))
                .await;
            (secret, key)
        }

        async fn spent(&self, key: &ResolvedKey) -> f64 {
            let period = crate::proxy::term_period_key(key, chrono::Utc::now()).expect("capped");
            self.state
                .redis
                .term_usage_read(&key.tenant_id, &period)
                .await
                .expect("term usage")
                .1
        }

        fn ledger_rows(&self) -> u64 {
            self.state.telemetry.stats().recorded.load(Ordering::SeqCst)
        }

        async fn send(
            &self,
            method: Method,
            uri: &str,
            secret: &str,
            content_type: Option<&str>,
            body: impl Into<Body>,
        ) -> Response<Body> {
            let mut req = Request::builder()
                .method(method)
                .uri(uri)
                .header(header::AUTHORIZATION, format!("Bearer {secret}"));
            if let Some(ct) = content_type {
                req = req.header(header::CONTENT_TYPE, ct);
            }
            crate::proxy::proxy_handler(State(self.state.clone()), req.body(body.into()).unwrap())
                .await
        }

        async fn create_json(&self, secret: &str, model: &str, prompt: &str) -> Response<Body> {
            let body = serde_json::json!({"model": model, "prompt": prompt, "seconds": "5"});
            self.send(
                Method::POST,
                "/v1/videos",
                secret,
                Some("application/json"),
                body.to_string(),
            )
            .await
        }

        async fn get(&self, secret: &str, uri: &str) -> Response<Body> {
            self.send(Method::GET, uri, secret, None, Body::empty())
                .await
        }
    }

    async fn json_of(resp: Response<Body>) -> serde_json::Value {
        let bytes = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .expect("body");
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
    }

    const BOUNDARY: &str = "videoboundary";

    fn multipart_create(model: &str, prompt: &str) -> String {
        format!(
            "--{b}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{model}\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\n{prompt}\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"seconds\"\r\n\r\n5\r\n\
             --{b}\r\nContent-Disposition: form-data; name=\"input_reference\"; filename=\"frame.png\"\r\n\
             Content-Type: image/png\r\n\r\n\x7fPNG-bytes\r\n--{b}--\r\n",
            b = BOUNDARY
        )
    }

    fn multipart_type() -> String {
        format!("multipart/form-data; boundary={BOUNDARY}")
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_json_create_is_routed_recorded_and_billed_once() {
        let Some(gw) = gateway().await else { return };
        let (backend, base) = spawn_backend("a").await;
        gw.register(video_route("video-model", &base)).await;
        let (secret, key) = gw.tenant(None).await;
        let rows_before = gw.ledger_rows();

        let resp = gw.create_json(&secret, "video-model", "a red fox").await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let job = json_of(resp).await;
        let id = job["id"].as_str().expect("job id").to_string();
        assert_eq!(job["status"], "queued");

        // The backend saw the upstream model name, the model's key and header.
        let seen = backend.creates.lock().unwrap()[0].clone();
        assert_eq!(seen.model, "video-upstream");
        assert_eq!(seen.prompt, "a red fox");
        assert_eq!(seen.authorization.as_deref(), Some("Bearer up-key"));
        assert_eq!(seen.route_header.as_deref(), Some("video"));

        // Recorded against the tenant, the key and the base that took it.
        let rec = gw
            .store
            .get_video_job(&id, key.tenant_id)
            .await
            .unwrap()
            .expect("recorded");
        assert_eq!(rec.model_name, "video-model");
        assert_eq!(rec.key_id, key.key_id);
        assert_eq!(rec.upstream_base, base);

        // Billed the flat price once, with one ledger row.
        assert!((gw.spent(&key).await - COST).abs() < 1e-9);
        assert_eq!(gw.ledger_rows(), rows_before + 1);

        // Polls cost nothing and write no ledger rows.
        for _ in 0..3 {
            let resp = gw.get(&secret, &format!("/v1/videos/{id}")).await;
            assert_eq!(resp.status(), StatusCode::OK);
            assert_eq!(json_of(resp).await["id"], id.as_str());
        }
        assert!((gw.spent(&key).await - COST).abs() < 1e-9);
        assert_eq!(gw.ledger_rows(), rows_before + 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_multipart_create_resolves_the_model_from_the_form() {
        let Some(gw) = gateway().await else { return };
        let (backend, base) = spawn_backend("m").await;
        gw.register(video_route("video-model", &base)).await;
        let (secret, key) = gw.tenant(None).await;

        let resp = gw
            .send(
                Method::POST,
                "/v1/videos",
                &secret,
                Some(&multipart_type()),
                multipart_create("video-model", "the camera pushes forward"),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::CREATED);
        let id = json_of(resp).await["id"].as_str().unwrap().to_string();

        let seen = backend.creates.lock().unwrap()[0].clone();
        assert_eq!(seen.model, "video-upstream", "the form's model is swapped");
        assert_eq!(seen.prompt, "the camera pushes forward");
        assert_eq!(
            seen.file_name.as_deref(),
            Some("frame.png"),
            "the upload is forwarded"
        );
        assert!(gw
            .store
            .get_video_job(&id, key.tenant_id)
            .await
            .unwrap()
            .is_some());
        assert!((gw.spent(&key).await - COST).abs() < 1e-9);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_create_records_nothing_and_bills_nothing() {
        let Some(gw) = gateway().await else { return };
        let (backend, base) = spawn_backend("f").await;
        backend.reject_creates.store(true, Ordering::SeqCst);
        gw.register(video_route("video-model", &base)).await;
        let (secret, key) = gw.tenant(None).await;

        let resp = gw.create_json(&secret, "video-model", "a red fox").await;
        assert_eq!(
            resp.status(),
            StatusCode::BAD_REQUEST,
            "the upstream error passes through"
        );
        assert!(gw
            .store
            .list_video_jobs(key.tenant_id, 10)
            .await
            .unwrap()
            .is_empty());
        assert_eq!(gw.spent(&key).await, 0.0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_unregistered_or_missing_model_is_refused_before_dispatch() {
        let Some(gw) = gateway().await else { return };
        let (backend, base) = spawn_backend("u").await;
        gw.register(video_route("video-model", &base)).await;
        let (secret, _) = gw.tenant(None).await;

        let resp = gw.create_json(&secret, "no-such-model", "x").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = gw
            .send(
                Method::POST,
                "/v1/videos",
                &secret,
                Some("application/json"),
                "{\"prompt\":\"x\"}",
            )
            .await;
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
        assert!(backend.creates.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn another_tenants_job_and_an_unknown_id_are_both_not_found() {
        let Some(gw) = gateway().await else { return };
        let (backend, base) = spawn_backend("x").await;
        gw.register(video_route("video-model", &base)).await;
        let (owner, owner_key) = gw.tenant(None).await;
        let (intruder, intruder_key) = gw.tenant(None).await;

        let id = json_of(gw.create_json(&owner, "video-model", "a fox").await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let expected = json_of(gw.get(&intruder, "/v1/videos/video_nope").await).await;

        for uri in [
            format!("/v1/videos/{id}"),
            format!("/v1/videos/{id}/content"),
        ] {
            let resp = gw.get(&intruder, &uri).await;
            assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
            assert_eq!(
                json_of(resp).await,
                expected,
                "no difference from an unknown id"
            );
        }
        let resp = gw
            .send(
                Method::DELETE,
                &format!("/v1/videos/{id}"),
                &intruder,
                None,
                Body::empty(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let resp = gw.get(&intruder, "/v1/videos/bad.id").await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        // None of that reached the backend, and the owner's job is intact.
        assert!(backend.calls.lock().unwrap().is_empty());
        assert!(gw
            .store
            .get_video_job(&id, owner_key.tenant_id)
            .await
            .unwrap()
            .is_some());
        assert_eq!(gw.spent(&intruder_key).await, 0.0);
        assert_eq!(
            gw.get(&owner, &format!("/v1/videos/{id}")).await.status(),
            StatusCode::OK
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_download_passes_a_not_ready_status_through_then_streams_with_its_headers() {
        let Some(gw) = gateway().await else { return };
        let (backend, base) = spawn_backend("c").await;
        gw.register(video_route("video-model", &base)).await;
        let (secret, key) = gw.tenant(None).await;
        let id = json_of(gw.create_json(&secret, "video-model", "a fox").await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        let rows = gw.ledger_rows();

        let resp = gw.get(&secret, &format!("/v1/videos/{id}/content")).await;
        assert_eq!(resp.status(), StatusCode::CONFLICT, "not ready is not gone");
        assert_eq!(json_of(resp).await["error"]["type"], "not_ready");

        backend.jobs.lock().unwrap().get_mut(&id).unwrap().0 = "completed".into();
        let hold = Arc::new(tokio::sync::Notify::new());
        *backend.hold.lock().unwrap() = Some(hold.clone());

        let resp = gw.get(&secret, &format!("/v1/videos/{id}/content")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        let h = resp.headers();
        assert_eq!(h[header::CONTENT_TYPE], "video/mp4");
        assert_eq!(
            h[header::CONTENT_LENGTH],
            (MP4_FIRST.len() + MP4_REST.len()).to_string().as_str()
        );
        assert_eq!(
            h[header::CONTENT_DISPOSITION],
            format!("attachment; filename=\"{id}.mp4\"").as_str()
        );
        assert!(h.contains_key("x-obleth-request-id"));

        // The first chunk arrives while the backend is still holding the
        // rest: the gateway streams, it does not buffer the file.
        let mut body = resp.into_body().into_data_stream();
        let first = tokio::time::timeout(Duration::from_secs(5), body.next())
            .await
            .expect("first chunk before the backend finishes")
            .expect("a chunk")
            .expect("ok chunk");
        assert_eq!(&first[..], MP4_FIRST);
        hold.notify_one();
        let mut rest = Vec::new();
        while let Some(chunk) = body.next().await {
            rest.extend_from_slice(&chunk.unwrap());
        }
        assert_eq!(rest, MP4_REST);

        // Downloads are not billed either.
        assert!((gw.spent(&key).await - COST).abs() < 1e-9);
        assert_eq!(gw.ledger_rows(), rows);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn delete_removes_the_job_and_its_record() {
        let Some(gw) = gateway().await else { return };
        let (backend, base) = spawn_backend("d").await;
        gw.register(video_route("video-model", &base)).await;
        let (secret, key) = gw.tenant(None).await;
        let id = json_of(gw.create_json(&secret, "video-model", "a fox").await).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let resp = gw
            .send(
                Method::DELETE,
                &format!("/v1/videos/{id}"),
                &secret,
                None,
                Body::empty(),
            )
            .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(json_of(resp).await["deleted"], true);
        assert!(!backend.jobs.lock().unwrap().contains_key(&id));
        assert!(gw
            .store
            .get_video_job(&id, key.tenant_id)
            .await
            .unwrap()
            .is_none());

        // Now unknown to the gateway: answered without asking the backend.
        let calls = backend.calls.lock().unwrap().len();
        assert_eq!(
            gw.get(&secret, &format!("/v1/videos/{id}")).await.status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(backend.calls.lock().unwrap().len(), calls);
        assert!((gw.spent(&key).await - COST).abs() < 1e-9);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_list_holds_only_the_callers_jobs() {
        let Some(gw) = gateway().await else { return };
        let (backend, base) = spawn_backend("l").await;
        gw.register(video_route("video-model", &base)).await;
        let (alice, _) = gw.tenant(None).await;
        let (bob, _) = gw.tenant(None).await;
        let (carol, _) = gw.tenant(None).await;

        let mut mine = Vec::new();
        for prompt in ["one", "two"] {
            let job = json_of(gw.create_json(&alice, "video-model", prompt).await).await;
            mine.push(job["id"].as_str().unwrap().to_string());
        }
        let theirs = json_of(gw.create_json(&bob, "video-model", "three").await).await["id"]
            .as_str()
            .unwrap()
            .to_string();

        let listing = json_of(gw.get(&alice, "/v1/videos").await).await;
        assert_eq!(listing["object"], "list");
        let ids: Vec<&str> = listing["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect();
        // Newest first; Bob's job and the backend's foreign job are left out.
        assert_eq!(ids, vec![mine[1].as_str(), mine[0].as_str()]);
        assert!(!ids.contains(&theirs.as_str()));

        // A tenant with no jobs gets an empty list without a backend call.
        let calls = backend.calls.lock().unwrap().len();
        let empty = json_of(gw.get(&carol, "/v1/videos").await).await;
        assert_eq!(empty["data"], serde_json::json!([]));
        assert_eq!(backend.calls.lock().unwrap().len(), calls);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn follow_ups_return_to_the_endpoint_that_created_the_job() {
        let Some(gw) = gateway().await else { return };
        let (first, first_base) = spawn_backend("p").await;
        let (second, second_base) = spawn_backend("s").await;
        let endpoint = |id: &str, base: &str, priority: i64| obleth_config::ResolvedEndpoint {
            id: id.into(),
            api_base: base.into(),
            api_key: None,
            priority,
            weight: 1,
            enabled: true,
            healthy: true,
            max_in_flight: None,
        };
        let mut route = video_route("video-model", "");
        route.endpoints = vec![
            endpoint("p", &first_base, 0),
            endpoint("s", &second_base, 1),
        ];
        gw.register(route.clone()).await;
        let (secret, _) = gw.tenant(None).await;
        let id = json_of(gw.create_json(&secret, "video-model", "a fox").await).await["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(first.creates.lock().unwrap().len(), 1);

        // The operator flips the priorities: new creates go to the second
        // endpoint, but the job still lives on the first.
        route.endpoints = vec![
            endpoint("p", &first_base, 1),
            endpoint("s", &second_base, 0),
        ];
        gw.register(route).await;
        let resp = gw.get(&secret, &format!("/v1/videos/{id}")).await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(first.calls.lock().unwrap().len(), 1);
        assert!(second.calls.lock().unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn guardrails_scan_the_prompt_of_a_json_or_multipart_create() {
        let Some(gw) = gateway().await else { return };
        let (backend, base) = spawn_backend("g").await;
        gw.register(video_route("video-model", &base)).await;
        let policy = obleth_config::GuardrailsPolicy {
            action: obleth_config::GuardrailsAction::Block,
            input_scanners: vec!["ban_keywords".into()],
            output_scanners: vec![],
            guard_model: None,
            ban_keywords: vec!["forbidden".into()],
            fail_open: false,
        };
        let (secret, key) = gw.tenant(Some(policy)).await;

        let resp = gw
            .create_json(&secret, "video-model", "a forbidden fox")
            .await;
        assert!(resp.status().is_client_error(), "{}", resp.status());
        let resp = gw
            .send(
                Method::POST,
                "/v1/videos",
                &secret,
                Some(&multipart_type()),
                multipart_create("video-model", "a forbidden fox"),
            )
            .await;
        assert!(resp.status().is_client_error(), "{}", resp.status());
        assert!(backend.creates.lock().unwrap().is_empty());
        assert_eq!(gw.spent(&key).await, 0.0);

        // A clean prompt goes through under the same policy.
        let resp = gw.create_json(&secret, "video-model", "a red fox").await;
        assert_eq!(resp.status(), StatusCode::CREATED);
    }
}
