//! Batches: the OpenAI Files and Batches APIs, and the runner that works
//! through them.
//!
//! ```text
//! POST   /v1/files                  upload a JSONL input file (purpose=batch)
//! GET    /v1/files                  list files
//! GET    /v1/files/{id}             file object
//! GET    /v1/files/{id}/content     input bytes, or a batch's output or error lines
//! DELETE /v1/files/{id}             delete a file
//! POST   /v1/batches                create a batch from an input file
//! GET    /v1/batches                list batches
//! GET    /v1/batches/{id}           batch object
//! POST   /v1/batches/{id}/cancel    cancel a batch
//! ```
//!
//! A batch is checked line by line when it is created; each line becomes an
//! item in Postgres. The runner, in every gateway replica, claims items with
//! a lease and sends each one through the ordinary pipeline in process, as
//! the batch's owner, admitted to the model's background pool: it runs only
//! while no interactive request waits for the model, and fills at most
//! `OBLETH_BATCH_MAX_SHARE` of its slots (see `obleth_fairshare::PoolKey`).
//! Routing, guardrails, boons, budgets and the usage ledger apply as for any
//! request; ledger rows carry `request_type = "batch"`.
//!
//! Extensions to the OpenAI shape are optional: `completion_window` accepts
//! any `<n>h` or `<n>d` within the configured bounds, and batch objects carry
//! an `obleth` object a standard client ignores.

// Handlers answer `Result<Response, Response>`: the error is the response.
#![allow(clippy::result_large_err)]

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, Method, Request, Response, StatusCode};
use axum::response::IntoResponse;
use chrono::{DateTime, Utc};
use obleth_config::{JobScope, ResolvedKey};
use obleth_store::batches::{
    BatchFile, BatchOutput, BatchRecord, ClaimedItem, NewBatch, NewBatchFile, NewBatchItem,
    OutputRow, PURPOSE_BATCH, PURPOSE_BATCH_ERROR, PURPOSE_BATCH_OUTPUT,
};
use obleth_store::JobOwner;
use uuid::Uuid;

use crate::proxy::error_json;
use crate::state::AppState;

/// Endpoints a batch may target. Each runs down the pipeline unchanged.
pub(crate) const BATCH_ENDPOINTS: &[&str] =
    &["/v1/chat/completions", "/v1/completions", "/v1/embeddings"];

/// Errors reported for a batch that fails validation; the rest are counted.
const MAX_REPORTED_ERRORS: usize = 100;

/// Largest answer the runner keeps for one item.
const ITEM_RESPONSE_MAX: usize = 32 * 1024 * 1024;

/// Lines per page when streaming an output or error file.
const OUTPUT_PAGE: i64 = 500;

/// How long the runner's claim on an item lasts without renewal.
const LEASE_SECS: i64 = 180;

/// How often the runner renews its leases and sweeps lapsed ones.
const HOUSEKEEPING: Duration = Duration::from_secs(15);

/// How often finished batches are given their final status.
const ADVANCE: Duration = Duration::from_secs(3);

/// Items per model the runner keeps waiting for a background slot on this
/// replica, beyond those running. It claims no more than that, so items it
/// cannot start soon stay pending: another replica may take them, and a
/// cancel still reaches them.
const QUEUE_BUFFER: usize = 8;

/// How long a resolved batch owner is reused before it is read again.
const OWNER_TTL: Duration = Duration::from_secs(30);

/// How long an item waits before it is tried again while its owner may not
/// send requests (disabled key, inactive tenant, outside its hours).
const OWNER_PAUSE_SECS: f64 = 60.0;

/// Most an item may wait for a background slot before its turn is given
/// back; it is retried at once, so this only bounds a single wait.
const MAX_ADMISSION_WAIT: Duration = Duration::from_secs(3600);

/// Batch settings, from the environment.
#[derive(Debug, Clone)]
pub struct BatchConfig {
    pub max_file_bytes: usize,
    pub max_lines: usize,
    pub max_active_per_key: i64,
    pub window_min: Duration,
    pub window_max: Duration,
    pub max_attempts: i32,
    pub runner: bool,
    pub concurrency: usize,
    pub retention_days: i64,
    pub scope: JobScope,
    pub share: f64,
    /// This replica's name in item leases.
    pub instance: String,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

fn env_num<T: std::str::FromStr>(name: &str, default: T) -> T {
    env(name)
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(default)
}

fn truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

impl BatchConfig {
    /// `None` unless `OBLETH_BATCHES` is on.
    pub fn from_env() -> Option<Self> {
        if !env("OBLETH_BATCHES").is_some_and(|v| truthy(&v)) {
            return None;
        }
        let window = |name: &str, default: Duration| {
            env(name).and_then(|v| parse_window(&v)).unwrap_or(default)
        };
        let scope = match JobScope::parse(env("OBLETH_BATCH_SCOPE").as_deref()) {
            Ok(s) => s,
            Err(bad) => {
                tracing::warn!(value = %bad, "OBLETH_BATCH_SCOPE is not key or tenant; using key");
                JobScope::Key
            }
        };
        let instance = env("POD_NAME")
            .or_else(|| env("HOSTNAME"))
            .unwrap_or_else(|| format!("gw-{}", Uuid::new_v4().simple()));
        Some(BatchConfig {
            max_file_bytes: env_num("OBLETH_BATCH_MAX_FILE_BYTES", 200 * 1024 * 1024),
            max_lines: env_num("OBLETH_BATCH_MAX_LINES", 50_000),
            max_active_per_key: env_num("OBLETH_BATCH_MAX_ACTIVE_PER_KEY", 10),
            window_min: window("OBLETH_BATCH_WINDOW_MIN", Duration::from_secs(3600)),
            window_max: window("OBLETH_BATCH_WINDOW_MAX", Duration::from_secs(7 * 86400)),
            max_attempts: env_num("OBLETH_BATCH_MAX_ATTEMPTS", 5).max(1),
            runner: env("OBLETH_BATCH_RUNNER").is_none_or(|v| truthy(&v)),
            concurrency: env_num("OBLETH_BATCH_RUNNER_CONCURRENCY", 64).max(1),
            retention_days: env_num("OBLETH_BATCH_RETENTION_DAYS", 30).max(1),
            scope,
            share: env_num(
                "OBLETH_BATCH_MAX_SHARE",
                obleth_fairshare::DEFAULT_BACKGROUND_SHARE,
            )
            .clamp(0.0, 1.0),
            instance,
        })
    }
}

/// `<n>h` or `<n>d` as a duration.
pub(crate) fn parse_window(raw: &str) -> Option<Duration> {
    let raw = raw.trim().to_ascii_lowercase();
    let (n, unit) = raw.split_at(raw.len().checked_sub(1)?);
    let n: u64 = n.parse().ok().filter(|n| *n > 0)?;
    match unit {
        "h" => Some(Duration::from_secs(n * 3600)),
        "d" => Some(Duration::from_secs(n * 86400)),
        _ => None,
    }
}

/// The batch store and settings the data plane carries when batches are on.
#[derive(Clone)]
pub struct Batches {
    store: obleth_store::Store,
    cfg: Arc<BatchConfig>,
}

impl Batches {
    pub fn new(store: obleth_store::Store, cfg: BatchConfig) -> Self {
        Batches {
            store,
            cfg: Arc::new(cfg),
        }
    }

    pub fn config(&self) -> &BatchConfig {
        &self.cfg
    }

    fn owner_of(&self, resolved: &ResolvedKey) -> JobOwner {
        match self.cfg.scope {
            JobScope::Key => JobOwner::key(resolved.tenant_id, resolved.key_id),
            JobScope::Tenant => JobOwner::tenant(resolved.tenant_id),
        }
    }
}

/// Set on a request the runner sends in process: the pipeline takes the
/// caller from here instead of a bearer token, and admits the request to the
/// model's background pool. Only the runner can set it; it never arrives
/// over HTTP.
#[derive(Clone)]
pub(crate) struct BatchCaller {
    pub resolved: Arc<ResolvedKey>,
    pub share: f64,
    /// Longest the request may wait for a background slot.
    pub admission_wait: Duration,
}

// ---- routing --------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route<'a> {
    FileUpload,
    FileList,
    FileGet(&'a str),
    FileContent(&'a str),
    FileDelete(&'a str),
    BatchCreate,
    BatchList,
    BatchGet(&'a str),
    BatchCancel(&'a str),
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The batch endpoint `method` and `path` name, if any.
pub(crate) fn route<'a>(method: &Method, path: &'a str) -> Option<Route<'a>> {
    let path = path.trim_end_matches('/');
    if let Some(rest) = path.strip_prefix("/v1/files") {
        let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
        return match (method.as_str(), parts.as_slice()) {
            ("POST", []) => Some(Route::FileUpload),
            ("GET", []) => Some(Route::FileList),
            ("GET", [id]) if valid_id(id) => Some(Route::FileGet(id)),
            ("GET", [id, "content"]) if valid_id(id) => Some(Route::FileContent(id)),
            ("DELETE", [id]) if valid_id(id) => Some(Route::FileDelete(id)),
            _ => None,
        };
    }
    if let Some(rest) = path.strip_prefix("/v1/batches") {
        let parts: Vec<&str> = rest.split('/').filter(|p| !p.is_empty()).collect();
        return match (method.as_str(), parts.as_slice()) {
            ("POST", []) => Some(Route::BatchCreate),
            ("GET", []) => Some(Route::BatchList),
            ("GET", [id]) if valid_id(id) => Some(Route::BatchGet(id)),
            ("POST", [id, "cancel"]) if valid_id(id) => Some(Route::BatchCancel(id)),
            _ => None,
        };
    }
    None
}

/// Serve a batch endpoint for an authenticated caller.
pub(crate) async fn handle(
    state: &AppState,
    batches: &Batches,
    resolved: &ResolvedKey,
    route: Route<'_>,
    query: &str,
    headers: &HeaderMap,
    body: Body,
) -> Response<Body> {
    let owner = batches.owner_of(resolved);
    let q = parse_query(query);
    let result = match route {
        Route::FileUpload => upload(batches, resolved, headers, body).await,
        Route::FileList => list_files(batches, owner, &q).await,
        Route::FileGet(id) => get_file(batches, owner, id).await,
        Route::FileContent(id) => file_content(batches, owner, id).await,
        Route::FileDelete(id) => delete_file(batches, owner, id).await,
        Route::BatchCreate => create(state, batches, resolved, body).await,
        Route::BatchList => list_batches(batches, owner, &q).await,
        Route::BatchGet(id) => get_batch(batches, owner, id).await,
        Route::BatchCancel(id) => cancel(batches, owner, id).await,
    };
    result.unwrap_or_else(|e| e)
}

type Handled = Result<Response<Body>, Response<Body>>;

fn store_error(e: obleth_store::StoreError) -> Response<Body> {
    tracing::warn!(error = %e, "batch store call failed");
    error_json(StatusCode::SERVICE_UNAVAILABLE, "batch storage unavailable")
}

fn invalid(msg: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": {
        "message": msg, "type": "invalid_request_error", "param": null, "code": null
    }});
    (StatusCode::BAD_REQUEST, axum::Json(body)).into_response()
}

fn not_found(what: &str, id: &str) -> Response<Body> {
    let body = serde_json::json!({ "error": {
        "message": format!("No such {what}: '{id}'"),
        "type": "invalid_request_error", "param": "id", "code": null
    }});
    (StatusCode::NOT_FOUND, axum::Json(body)).into_response()
}

fn json(value: serde_json::Value) -> Response<Body> {
    (StatusCode::OK, axum::Json(value)).into_response()
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let query = query.strip_prefix('?').unwrap_or(query);
    form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect()
}

fn limit_from(q: &HashMap<String, String>, default: i64, max: i64) -> i64 {
    q.get("limit")
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(default)
        .clamp(1, max)
}

fn new_file_id() -> String {
    format!("file-{}", Uuid::new_v4().simple())
}

fn new_batch_id() -> String {
    format!("batch_{}", Uuid::new_v4().simple())
}

// ---- files ----------------------------------------------------------------

fn file_object(f: &BatchFile) -> serde_json::Value {
    serde_json::json!({
        "id": f.id,
        "object": "file",
        "bytes": f.bytes,
        "created_at": f.created_at.timestamp(),
        "filename": f.filename,
        "purpose": f.purpose,
        "status": "processed",
        "expires_at": null,
        "status_details": null,
    })
}

async fn upload(
    batches: &Batches,
    resolved: &ResolvedKey,
    headers: &HeaderMap,
    body: Body,
) -> Handled {
    let ct = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let boundary = multer::parse_boundary(ct).map_err(|_| {
        invalid("upload a file as multipart/form-data with fields `file` and `purpose`")
    })?;
    let max = batches.cfg.max_file_bytes;
    let constraints = multer::Constraints::new().size_limit(
        multer::SizeLimit::new()
            .whole_stream(max as u64 + 64 * 1024)
            .for_field("file", max as u64)
            .for_field("purpose", 1024),
    );
    let mut form =
        multer::Multipart::with_constraints(body.into_data_stream(), boundary, constraints);
    let mut purpose: Option<String> = None;
    let mut file: Option<(String, Bytes)> = None;
    loop {
        let field = match form.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(
                multer::Error::FieldSizeExceeded { .. } | multer::Error::StreamSizeExceeded { .. },
            ) => {
                return Err(error_json(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    &format!("the file is larger than the {max} bytes allowed"),
                ))
            }
            Err(_) => return Err(invalid("the upload is not valid multipart/form-data")),
        };
        match field.name() {
            Some("purpose") => {
                purpose = Some(
                    field
                        .text()
                        .await
                        .map_err(|_| invalid("unreadable `purpose`"))?,
                );
            }
            Some("file") => {
                let name = field.file_name().unwrap_or("input.jsonl").to_string();
                let data = match field.bytes().await {
                    Ok(d) => d,
                    Err(multer::Error::FieldSizeExceeded { .. }) => {
                        return Err(error_json(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            &format!("the file is larger than the {max} bytes allowed"),
                        ))
                    }
                    Err(_) => return Err(invalid("unreadable `file`")),
                };
                file = Some((name, data));
            }
            _ => {}
        }
    }
    match purpose.as_deref().map(str::trim) {
        Some(PURPOSE_BATCH) => {}
        Some(other) => {
            return Err(invalid(&format!(
                "purpose `{other}` is not supported here; this gateway accepts files with purpose `batch`"
            )))
        }
        None => return Err(invalid("missing `purpose`")),
    }
    let Some((filename, data)) = file else {
        return Err(invalid("missing `file`"));
    };
    if data.is_empty() {
        return Err(invalid("the file is empty"));
    }
    let lines = data
        .split(|b| *b == b'\n')
        .filter(|l| !l.trim_ascii().is_empty())
        .count();
    if lines > batches.cfg.max_lines {
        return Err(invalid(&format!(
            "the file has {lines} requests; a batch may have at most {}",
            batches.cfg.max_lines
        )));
    }
    let id = new_file_id();
    let filename: String = filename.chars().take(255).collect();
    let stored = batches
        .store
        .insert_batch_file(&NewBatchFile {
            id: &id,
            tenant_id: resolved.tenant_id,
            key_id: resolved.key_id,
            purpose: PURPOSE_BATCH,
            filename: &filename,
            bytes: data.len() as i64,
            content: Some(&data),
            batch_id: None,
        })
        .await
        .map_err(store_error)?;
    Ok(json(file_object(&stored)))
}

async fn list_files(batches: &Batches, owner: JobOwner, q: &HashMap<String, String>) -> Handled {
    let limit = limit_from(q, 100, 10_000);
    let mut files = batches
        .store
        .list_batch_files(
            owner,
            q.get("purpose").map(String::as_str),
            q.get("after").map(String::as_str),
            limit + 1,
        )
        .await
        .map_err(store_error)?;
    let has_more = files.len() as i64 > limit;
    files.truncate(limit as usize);
    Ok(json(list_object(
        files.iter().map(file_object).collect(),
        has_more,
    )))
}

fn list_object(data: Vec<serde_json::Value>, has_more: bool) -> serde_json::Value {
    let first = data.first().and_then(|d| d.get("id")).cloned();
    let last = data.last().and_then(|d| d.get("id")).cloned();
    serde_json::json!({
        "object": "list",
        "data": data,
        "first_id": first,
        "last_id": last,
        "has_more": has_more,
    })
}

async fn get_file(batches: &Batches, owner: JobOwner, id: &str) -> Handled {
    match batches
        .store
        .get_batch_file(id, owner)
        .await
        .map_err(store_error)?
    {
        Some(f) => Ok(json(file_object(&f))),
        None => Err(not_found("file", id)),
    }
}

async fn delete_file(batches: &Batches, owner: JobOwner, id: &str) -> Handled {
    if batches
        .store
        .delete_batch_file(id, owner)
        .await
        .map_err(store_error)?
    {
        Ok(json(
            serde_json::json!({ "id": id, "object": "file", "deleted": true }),
        ))
    } else {
        Err(not_found("file", id))
    }
}

async fn file_content(batches: &Batches, owner: JobOwner, id: &str) -> Handled {
    let Some(file) = batches
        .store
        .get_batch_file(id, owner)
        .await
        .map_err(store_error)?
    else {
        return Err(not_found("file", id));
    };
    let which = match file.purpose.as_str() {
        PURPOSE_BATCH_OUTPUT => BatchOutput::Output,
        PURPOSE_BATCH_ERROR => BatchOutput::Error,
        _ => {
            let Some(bytes) = batches
                .store
                .batch_file_content(id, owner)
                .await
                .map_err(store_error)?
            else {
                return Err(not_found("file", id));
            };
            return Ok(jsonl_response(Body::from(bytes), &file.filename));
        }
    };
    let Some(batch_id) = file.batch_id.clone() else {
        return Err(not_found("file", id));
    };
    let store = batches.store.clone();
    // Pages of lines, read as the client reads them.
    let stream = futures_util::stream::unfold(Some(-1i32), move |after| {
        let (store, batch_id) = (store.clone(), batch_id.clone());
        async move {
            let after = after?;
            match store
                .batch_output_rows(&batch_id, which, after, OUTPUT_PAGE)
                .await
            {
                Ok(rows) if rows.is_empty() => None,
                Ok(rows) => {
                    let next = (rows.len() as i64 == OUTPUT_PAGE)
                        .then(|| rows.last().map(|r| r.line))
                        .flatten();
                    let mut out = Vec::new();
                    for row in &rows {
                        out.extend_from_slice(output_line(&batch_id, row).to_string().as_bytes());
                        out.push(b'\n');
                    }
                    Some((Ok::<_, std::io::Error>(Bytes::from(out)), next))
                }
                Err(e) => {
                    tracing::warn!(error = %e, batch = %batch_id, "reading a batch file failed");
                    Some((
                        Err(std::io::Error::other("batch storage unavailable")),
                        None,
                    ))
                }
            }
        }
    });
    Ok(jsonl_response(Body::from_stream(stream), &file.filename))
}

fn jsonl_response(body: Body, filename: &str) -> Response<Body> {
    let mut resp = Response::new(body);
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/jsonl"),
    );
    let safe: String = filename
        .chars()
        .filter(|c| c.is_ascii_graphic() && *c != '"' && *c != '\\')
        .collect();
    if let Ok(v) = header::HeaderValue::from_str(&format!("attachment; filename=\"{safe}\"")) {
        resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
    }
    resp
}

/// One line of an output or error file, in the OpenAI shape.
pub(crate) fn output_line(batch_id: &str, row: &OutputRow) -> serde_json::Value {
    let id = format!(
        "batch_req_{}_{}",
        batch_id.trim_start_matches("batch_"),
        row.line
    );
    let response = row.response_status.map(|status| {
        let body = row
            .response
            .as_deref()
            .map(|b| {
                serde_json::from_slice::<serde_json::Value>(b).unwrap_or_else(|_| {
                    serde_json::Value::String(String::from_utf8_lossy(b).into())
                })
            })
            .unwrap_or(serde_json::Value::Null);
        serde_json::json!({
            "status_code": status,
            "request_id": row.request_id.map(|r| r.to_string()),
            "body": body,
        })
    });
    let error = if response.is_some() {
        serde_json::Value::Null
    } else {
        row.error
            .as_deref()
            .and_then(|e| serde_json::from_str(e).ok())
            .unwrap_or_else(|| serde_json::json!({ "code": row.status, "message": row.status }))
    };
    serde_json::json!({
        "id": id,
        "custom_id": row.custom_id,
        "response": response,
        "error": error,
    })
}

// ---- batches --------------------------------------------------------------

/// A batch in the OpenAI shape, with obleth's extension fields under
/// `obleth`.
pub(crate) fn batch_object(b: &BatchRecord) -> serde_json::Value {
    let ts = |t: Option<DateTime<Utc>>| t.map(|t| t.timestamp());
    let errors = b
        .errors
        .as_deref()
        .and_then(|e| serde_json::from_str::<serde_json::Value>(e).ok())
        .map(|data| serde_json::json!({ "object": "list", "data": data }));
    let metadata = b
        .metadata
        .as_deref()
        .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok());
    let expires = (b.status == "in_progress").then(|| b.deadline_at.timestamp());
    serde_json::json!({
        "id": b.id,
        "object": "batch",
        "endpoint": b.endpoint,
        "errors": errors,
        "input_file_id": b.input_file_id,
        "completion_window": b.completion_window,
        "status": b.status,
        "output_file_id": b.output_file_id,
        "error_file_id": b.error_file_id,
        "created_at": b.created_at.timestamp(),
        "in_progress_at": ts(b.in_progress_at),
        "expires_at": expires,
        "finalizing_at": ts(b.finalizing_at),
        "completed_at": ts(b.completed_at),
        "failed_at": ts(b.failed_at),
        "expired_at": ts(b.expired_at),
        "cancelling_at": ts(b.cancelling_at),
        "cancelled_at": ts(b.cancelled_at),
        "request_counts": {
            "total": b.total,
            "completed": b.completed,
            "failed": b.failed,
        },
        "metadata": metadata,
        "obleth": {
            "model": b.model,
            "deadline_at": b.deadline_at.timestamp(),
        },
    })
}

/// What a valid input file holds: its model and items.
#[derive(Debug, PartialEq)]
pub(crate) struct ParsedInput {
    pub model: String,
    pub items: Vec<NewBatchItem>,
}

/// Check every line of an input file against the batch's endpoint, as the
/// OpenAI Batch API does. Returns the items, or the errors (at most
/// [`MAX_REPORTED_ERRORS`], with a count of the rest).
pub(crate) fn parse_input(
    content: &[u8],
    endpoint: &str,
    max_lines: usize,
) -> Result<ParsedInput, Vec<serde_json::Value>> {
    let mut errors: Vec<serde_json::Value> = Vec::new();
    let mut more = 0usize;
    macro_rules! err {
        ($line:expr, $code:expr, $message:expr, $param:expr) => {{
            let param: Option<&str> = $param;
            let message: String = $message;
            if errors.len() < MAX_REPORTED_ERRORS {
                errors.push(serde_json::json!({
                    "code": $code, "message": message, "param": param, "line": $line,
                }));
            } else {
                more += 1;
            }
        }};
    }
    let Ok(text) = std::str::from_utf8(content) else {
        return Err(vec![serde_json::json!({
            "code": "invalid_file", "message": "the file is not UTF-8 text", "param": null, "line": null
        })]);
    };
    let mut items = Vec::new();
    let mut seen = HashSet::new();
    let mut model: Option<String> = None;
    for (idx, raw) in text.lines().enumerate() {
        let line = idx + 1;
        if raw.trim().is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(raw) else {
            err!(
                line,
                "invalid_json_line",
                "this line is not valid JSON".into(),
                None
            );
            continue;
        };
        let custom_id = match v.get("custom_id").and_then(|c| c.as_str()) {
            Some(c) if !c.is_empty() && c.len() <= 512 => c.to_string(),
            _ => {
                err!(
                    line,
                    "missing_required_parameter",
                    "missing or invalid `custom_id`".into(),
                    Some("custom_id")
                );
                continue;
            }
        };
        if !seen.insert(custom_id.clone()) {
            err!(
                line,
                "duplicate_custom_id",
                format!("`custom_id` {custom_id:?} appears more than once"),
                Some("custom_id")
            );
            continue;
        }
        if v.get("method").and_then(|m| m.as_str()) != Some("POST") {
            err!(
                line,
                "invalid_method",
                "`method` must be POST".into(),
                Some("method")
            );
            continue;
        }
        if v.get("url").and_then(|u| u.as_str()) != Some(endpoint) {
            err!(
                line,
                "mismatched_endpoint",
                format!("`url` must be the batch's endpoint, {endpoint}"),
                Some("url")
            );
            continue;
        }
        let Some(body) = v.get("body").filter(|b| b.is_object()) else {
            err!(
                line,
                "missing_required_parameter",
                "missing `body` object".into(),
                Some("body")
            );
            continue;
        };
        let Some(m) = body
            .get("model")
            .and_then(|m| m.as_str())
            .filter(|m| !m.is_empty())
        else {
            err!(
                line,
                "missing_required_parameter",
                "`body.model` is required".into(),
                Some("body.model")
            );
            continue;
        };
        match &model {
            None => model = Some(m.to_string()),
            Some(first) if first != m => {
                err!(
                    line,
                    "mismatched_model",
                    format!(
                        "every request in a batch must use the same model; line 1 uses {first:?}"
                    ),
                    Some("body.model")
                );
                continue;
            }
            Some(_) => {}
        }
        items.push(NewBatchItem {
            line: items.len() as i32,
            custom_id,
            body: body.to_string(),
        });
    }
    if items.len() > max_lines {
        err!(
            0,
            "too_many_requests",
            format!("a batch may have at most {max_lines} requests"),
            None
        );
    }
    if errors.is_empty() && items.is_empty() {
        err!(0, "empty_file", "the file has no requests".into(), None);
    }
    if more > 0 {
        errors.push(serde_json::json!({
            "code": "more_errors", "message": format!("{more} more lines have errors"),
            "param": null, "line": null,
        }));
    }
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(ParsedInput {
        model: model.unwrap_or_default(),
        items,
    })
}

async fn create(
    state: &AppState,
    batches: &Batches,
    resolved: &ResolvedKey,
    body: Body,
) -> Handled {
    let bytes = axum::body::to_bytes(body, 1024 * 1024)
        .await
        .map_err(|_| invalid("request body too large"))?;
    let req: serde_json::Value =
        serde_json::from_slice(&bytes).map_err(|_| invalid("the request body is not JSON"))?;
    let input_file_id = req
        .get("input_file_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid("missing `input_file_id`"))?;
    let endpoint = req
        .get("endpoint")
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid("missing `endpoint`"))?;
    if !BATCH_ENDPOINTS.contains(&endpoint) {
        return Err(invalid(&format!(
            "`endpoint` must be one of {}",
            BATCH_ENDPOINTS.join(", ")
        )));
    }
    let window_raw = req
        .get("completion_window")
        .and_then(|v| v.as_str())
        .ok_or_else(|| invalid("missing `completion_window`"))?;
    let cfg = &batches.cfg;
    let window = parse_window(window_raw)
        .filter(|w| *w >= cfg.window_min && *w <= cfg.window_max)
        .ok_or_else(|| {
            invalid(&format!(
                "`completion_window` must be a duration like 24h or 2d, from {}h to {}h",
                cfg.window_min.as_secs() / 3600,
                cfg.window_max.as_secs() / 3600
            ))
        })?;
    let metadata = match req.get("metadata") {
        None | Some(serde_json::Value::Null) => None,
        Some(m @ serde_json::Value::Object(map)) if map.len() <= 16 => Some(m.to_string()),
        Some(_) => return Err(invalid("`metadata` must be an object of at most 16 keys")),
    };
    let active = batches
        .store
        .active_batch_count(resolved.key_id)
        .await
        .map_err(store_error)?;
    if active >= cfg.max_active_per_key {
        return Err(error_json(
            StatusCode::TOO_MANY_REQUESTS,
            &format!(
                "this key already has {active} batches in progress; the most allowed is {}",
                cfg.max_active_per_key
            ),
        ));
    }
    let owner = batches.owner_of(resolved);
    let file = batches
        .store
        .get_batch_file(input_file_id, owner)
        .await
        .map_err(store_error)?
        .ok_or_else(|| not_found("file", input_file_id))?;
    if file.purpose != PURPOSE_BATCH {
        return Err(invalid(
            "`input_file_id` must name a file uploaded with purpose `batch`",
        ));
    }
    let content = batches
        .store
        .batch_file_content(input_file_id, owner)
        .await
        .map_err(store_error)?
        .ok_or_else(|| not_found("file", input_file_id))?;

    let id = new_batch_id();
    let deadline = Utc::now()
        + chrono::Duration::from_std(window).unwrap_or_else(|_| chrono::Duration::hours(24));
    let parsed = parse_input(&content, endpoint, cfg.max_lines);
    let model_name = match &parsed {
        Ok(p) => p.model.clone(),
        Err(_) => String::new(),
    };
    let new = NewBatch {
        id: &id,
        tenant_id: resolved.tenant_id,
        key_id: resolved.key_id,
        model: &model_name,
        endpoint,
        input_file_id,
        completion_window: window_raw,
        deadline_at: deadline,
        metadata: metadata.as_deref(),
    };
    let parsed = match parsed {
        Ok(p) => p,
        Err(errors) => return fail_batch(batches, &new, errors).await,
    };
    // The model must exist and be open to the caller now; each request is
    // checked again by the pipeline when it runs.
    let known = crate::proxy::resolve_model(state, &parsed.model)
        .await
        .is_some();
    let allowed =
        crate::proxy::model_visible(crate::proxy::allowed_models_for(resolved), &parsed.model);
    if !known || !allowed {
        return fail_batch(batches, &new, vec![serde_json::json!({
            "code": "model_not_found",
            "message": format!("model {:?} does not exist or is not available to this key", parsed.model),
            "param": "body.model",
            "line": 1,
        })])
        .await;
    }
    let b = batches
        .store
        .create_batch(&new, &parsed.items)
        .await
        .map_err(store_error)?;
    tracing::info!(batch = %b.id, model = %b.model, total = b.total, tenant = %resolved.tenant_name, "batch created");
    Ok(json(batch_object(&b)))
}

/// Record a batch that failed validation and answer with it.
async fn fail_batch(
    batches: &Batches,
    new: &NewBatch<'_>,
    errors: Vec<serde_json::Value>,
) -> Handled {
    let errors = serde_json::Value::Array(errors).to_string();
    let b = batches
        .store
        .create_failed_batch(new, &errors)
        .await
        .map_err(store_error)?;
    Ok(json(batch_object(&b)))
}

async fn list_batches(batches: &Batches, owner: JobOwner, q: &HashMap<String, String>) -> Handled {
    let limit = limit_from(q, 20, 100);
    let mut rows = batches
        .store
        .list_batches(owner, q.get("after").map(String::as_str), limit + 1)
        .await
        .map_err(store_error)?;
    let has_more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    Ok(json(list_object(
        rows.iter().map(batch_object).collect(),
        has_more,
    )))
}

async fn get_batch(batches: &Batches, owner: JobOwner, id: &str) -> Handled {
    match batches
        .store
        .get_batch(id, owner)
        .await
        .map_err(store_error)?
    {
        Some(b) => Ok(json(batch_object(&b))),
        None => Err(not_found("batch", id)),
    }
}

async fn cancel(batches: &Batches, owner: JobOwner, id: &str) -> Handled {
    match batches
        .store
        .cancel_batch(id, owner)
        .await
        .map_err(store_error)?
    {
        Some(b) => Ok(json(batch_object(&b))),
        None => Err(not_found("batch", id)),
    }
}

// ---- runner ---------------------------------------------------------------

/// What one replica's runner has in flight, per batch.
#[derive(Default)]
struct InFlight {
    total: usize,
    per_batch: HashMap<String, usize>,
}

impl InFlight {
    fn add(&mut self, batch: &str, n: usize) {
        self.total += n;
        *self.per_batch.entry(batch.to_string()).or_default() += n;
    }

    fn done(&mut self, batch: &str) {
        self.total = self.total.saturating_sub(1);
        if let Some(n) = self.per_batch.get_mut(batch) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.per_batch.remove(batch);
            }
        }
    }
}

/// How an item's attempt ended.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    /// Keep the answer.
    Success,
    /// Try again after a delay (unless out of attempts).
    Retry,
    /// Final: the request itself is at fault.
    Fail,
}

/// What to do with an upstream status.
pub(crate) fn classify(status: StatusCode) -> Outcome {
    if status.is_success() {
        Outcome::Success
    } else if status == StatusCode::TOO_MANY_REQUESTS
        || status == StatusCode::REQUEST_TIMEOUT
        || status.is_server_error()
    {
        Outcome::Retry
    } else {
        Outcome::Fail
    }
}

/// Delay before attempt `attempts + 1`: doubling from 5 s, at most 5 min,
/// with up to a fifth added at random.
pub(crate) fn backoff(attempts: i32) -> f64 {
    let base = (5.0 * 2f64.powi(attempts.saturating_sub(1).clamp(0, 10))).min(300.0);
    base + base * 0.2 * rand::random::<f64>()
}

/// Start the runner on this replica, when batches and the runner are on.
pub(crate) fn spawn_runner(state: AppState, batches: Batches) {
    if !batches.cfg.runner {
        tracing::info!("batch runner off on this replica (OBLETH_BATCH_RUNNER)");
        return;
    }
    tracing::info!(
        instance = %batches.cfg.instance,
        concurrency = batches.cfg.concurrency,
        share = batches.cfg.share,
        "batch runner started"
    );
    let owners: moka::future::Cache<Uuid, Option<Arc<ResolvedKey>>> =
        moka::future::Cache::builder()
            .time_to_live(OWNER_TTL)
            .max_capacity(10_000)
            .build();
    let in_flight = Arc::new(Mutex::new(InFlight::default()));
    // An item finishing is room for the next: fill again without waiting for
    // the tick.
    let finished = Arc::new(tokio::sync::Notify::new());
    {
        let batches = batches.clone();
        tokio::spawn(async move { housekeeping(batches).await });
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(1));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = finished.notified() => {}
            }
            if let Err(e) = fill(&state, &batches, &owners, &in_flight, &finished).await {
                tracing::warn!(error = %e, "batch runner could not claim work");
            }
        }
    });
}

/// Renew this runner's leases, sweep lapsed ones, finish batches and prune
/// old ones. Every replica does this; each step is safe to race.
async fn housekeeping(batches: Batches) {
    let mut tick = tokio::time::interval(ADVANCE);
    let mut last_leases = std::time::Instant::now() - HOUSEKEEPING;
    let mut last_prune = std::time::Instant::now() - Duration::from_secs(3600);
    loop {
        tick.tick().await;
        let store = &batches.store;
        let cfg = &batches.cfg;
        if last_leases.elapsed() >= HOUSEKEEPING {
            last_leases = std::time::Instant::now();
            if let Err(e) = store.renew_batch_leases(&cfg.instance, LEASE_SECS).await {
                tracing::warn!(error = %e, "batch lease renewal failed");
            }
            match store.sweep_batch_leases(cfg.max_attempts).await {
                Ok(0) => {}
                Ok(n) => tracing::info!(items = n, "batch items whose lease lapsed were swept"),
                Err(e) => tracing::warn!(error = %e, "batch lease sweep failed"),
            }
        }
        match store.advance_batches(&new_file_id).await {
            Ok(done) => {
                for b in done {
                    tracing::info!(batch = %b.id, status = %b.status, "batch finished");
                }
            }
            Err(e) => tracing::warn!(error = %e, "batch status update failed"),
        }
        if last_prune.elapsed() >= Duration::from_secs(3600) {
            last_prune = std::time::Instant::now();
            match store.prune_batches(cfg.retention_days).await {
                Ok((0, 0)) => {}
                Ok((b, f)) => tracing::info!(batches = b, files = f, "pruned old batches"),
                Err(e) => tracing::warn!(error = %e, "batch prune failed"),
            }
        }
    }
}

/// Claim work up to this replica's concurrency, spread evenly across the
/// runnable batches (earliest deadline first), and start it.
async fn fill(
    state: &AppState,
    batches: &Batches,
    owners: &moka::future::Cache<Uuid, Option<Arc<ResolvedKey>>>,
    in_flight: &Arc<Mutex<InFlight>>,
    finished: &Arc<tokio::sync::Notify>,
) -> Result<(), obleth_store::StoreError> {
    let cfg = &batches.cfg;
    if in_flight.lock().unwrap().total >= cfg.concurrency {
        return Ok(());
    }
    let runnable = batches.store.runnable_batches().await?;
    if runnable.is_empty() {
        return Ok(());
    }
    let per_batch = cfg.concurrency.div_ceil(runnable.len()).max(1);
    // Items already waiting for a background slot here, per model.
    let mut waiting: HashMap<String, usize> = HashMap::new();
    if let Some(snap) = state.fairshare.snapshot().await {
        for pool in snap.pools {
            if let Some(model) = pool.model.strip_suffix(" (batch)") {
                waiting.insert(model.to_string(), pool.queued);
            }
        }
    }
    for batch in runnable {
        let queued = waiting.entry(batch.model.clone()).or_default();
        let want = {
            let f = in_flight.lock().unwrap();
            let room = cfg.concurrency.saturating_sub(f.total);
            let mine = f.per_batch.get(&batch.id).copied().unwrap_or(0);
            per_batch
                .saturating_sub(mine)
                .min(room)
                .min(QUEUE_BUFFER.saturating_sub(*queued))
        };
        *queued += want;
        if want == 0 {
            continue;
        }
        let items = batches
            .store
            .claim_batch_items(&batch.id, &cfg.instance, want as i64, LEASE_SECS)
            .await?;
        if items.is_empty() {
            continue;
        }
        in_flight.lock().unwrap().add(&batch.id, items.len());
        let batch = Arc::new(batch);
        for item in items {
            let (state, batches, owners, in_flight, batch, finished) = (
                state.clone(),
                batches.clone(),
                owners.clone(),
                in_flight.clone(),
                batch.clone(),
                finished.clone(),
            );
            tokio::spawn(async move {
                let id = batch.id.clone();
                run_item(&state, &batches, &owners, &batch, item).await;
                in_flight.lock().unwrap().done(&id);
                finished.notify_one();
            });
        }
    }
    Ok(())
}

fn error_text(code: &str, message: &str) -> String {
    serde_json::json!({ "code": code, "message": message }).to_string()
}

/// Send one item through the pipeline and record how it ended.
async fn run_item(
    state: &AppState,
    batches: &Batches,
    owners: &moka::future::Cache<Uuid, Option<Arc<ResolvedKey>>>,
    batch: &BatchRecord,
    item: ClaimedItem,
) {
    let store = &batches.store;
    let cfg = &batches.cfg;
    let me = cfg.instance.as_str();
    let finish_err = |e: obleth_store::StoreError| {
        tracing::warn!(error = %e, batch = %batch.id, line = item.line, "could not record a batch item");
    };

    // The owner, as of now: a disabled key or an inactive tenant pauses the
    // batch rather than failing it; a deleted key fails it.
    let owner = owners
        .try_get_with(batch.key_id, async {
            store
                .resolved_key_by_id(batch.key_id)
                .await
                .map(|r| r.map(Arc::new))
        })
        .await;
    let resolved = match owner {
        Ok(Some(r)) => r,
        Ok(None) => {
            let _ = store
                .fail_batch_item(
                    &batch.id,
                    item.line,
                    me,
                    None,
                    None,
                    &error_text(
                        "key_not_found",
                        "the API key that created this batch no longer exists",
                    ),
                    None,
                )
                .await
                .map_err(finish_err);
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not resolve a batch owner");
            let _ = store
                .retry_batch_item(
                    &batch.id,
                    item.line,
                    me,
                    OWNER_PAUSE_SECS,
                    &error_text("owner_unavailable", "the batch owner could not be read"),
                    true,
                )
                .await
                .map_err(finish_err);
            return;
        }
    };
    if crate::proxy::gate_resolved_key(state, &resolved).is_err() {
        let _ = store
            .retry_batch_item(
                &batch.id,
                item.line,
                me,
                OWNER_PAUSE_SECS,
                &error_text(
                    "owner_paused",
                    "the key or its tenant may not send requests right now",
                ),
                true,
            )
            .await
            .map_err(finish_err);
        return;
    }

    let mut body: serde_json::Value = match serde_json::from_str(&item.body) {
        Ok(b) => b,
        Err(_) => {
            let _ = store
                .fail_batch_item(
                    &batch.id,
                    item.line,
                    me,
                    None,
                    None,
                    &error_text("invalid_body", "the stored request is not JSON"),
                    None,
                )
                .await
                .map_err(finish_err);
            return;
        }
    };
    if let Some(obj) = body.as_object_mut() {
        obj.insert("stream".into(), serde_json::Value::Bool(false));
        obj.remove("stream_options");
    }
    let wait = (batch.deadline_at - Utc::now())
        .to_std()
        .unwrap_or(Duration::from_secs(1))
        .min(MAX_ADMISSION_WAIT);
    let caller = BatchCaller {
        resolved: resolved.clone(),
        share: cfg.share,
        admission_wait: wait,
    };
    let request_id = Uuid::new_v4();
    let mut req = Request::builder()
        .method(Method::POST)
        .uri(batch.endpoint.as_str())
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .expect("static request parts");
    req.extensions_mut().insert(caller);
    let resp = crate::proxy::run_internal(state.clone(), req, request_id).await;
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), ITEM_RESPONSE_MAX).await;
    let recorded = match (classify(status), bytes) {
        (Outcome::Success, Ok(bytes)) => {
            store
                .complete_batch_item(
                    &batch.id,
                    item.line,
                    me,
                    status.as_u16() as i32,
                    &bytes,
                    request_id,
                )
                .await
        }
        (Outcome::Success, Err(_)) => {
            store
                .fail_batch_item(
                    &batch.id,
                    item.line,
                    me,
                    None,
                    None,
                    &error_text(
                        "response_too_large",
                        "the answer was larger than a batch keeps",
                    ),
                    Some(request_id),
                )
                .await
        }
        (Outcome::Retry, bytes) if item.attempts < cfg.max_attempts => {
            let detail = bytes
                .ok()
                .map(|b| String::from_utf8_lossy(&b[..b.len().min(500)]).into_owned())
                .unwrap_or_default();
            store
                .retry_batch_item(
                    &batch.id,
                    item.line,
                    me,
                    backoff(item.attempts),
                    &error_text(&status.as_u16().to_string(), &detail),
                    false,
                )
                .await
        }
        (_, bytes) => {
            let bytes = bytes.ok();
            store
                .fail_batch_item(
                    &batch.id,
                    item.line,
                    me,
                    Some(status.as_u16() as i32),
                    bytes.as_deref(),
                    &error_text(&status.as_u16().to_string(), "request failed"),
                    Some(request_id),
                )
                .await
        }
    };
    if let Err(e) = recorded {
        finish_err(e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_match_the_openai_paths() {
        let get = Method::GET;
        let post = Method::POST;
        let delete = Method::DELETE;
        assert_eq!(route(&post, "/v1/files"), Some(Route::FileUpload));
        assert_eq!(route(&get, "/v1/files"), Some(Route::FileList));
        assert_eq!(
            route(&get, "/v1/files/file-abc"),
            Some(Route::FileGet("file-abc"))
        );
        assert_eq!(
            route(&get, "/v1/files/file-abc/content"),
            Some(Route::FileContent("file-abc"))
        );
        assert_eq!(
            route(&delete, "/v1/files/file-abc"),
            Some(Route::FileDelete("file-abc"))
        );
        assert_eq!(route(&post, "/v1/batches"), Some(Route::BatchCreate));
        assert_eq!(route(&get, "/v1/batches/"), Some(Route::BatchList));
        assert_eq!(
            route(&get, "/v1/batches/batch_1"),
            Some(Route::BatchGet("batch_1"))
        );
        assert_eq!(
            route(&post, "/v1/batches/batch_1/cancel"),
            Some(Route::BatchCancel("batch_1"))
        );
        assert_eq!(route(&get, "/v1/files/a.b"), None);
        assert_eq!(route(&post, "/v1/chat/completions"), None);
        assert_eq!(route(&delete, "/v1/batches/batch_1"), None);
    }

    #[test]
    fn windows_parse_hours_and_days() {
        assert_eq!(parse_window("24h"), Some(Duration::from_secs(86400)));
        assert_eq!(parse_window("2d"), Some(Duration::from_secs(172800)));
        assert_eq!(parse_window(" 1H "), Some(Duration::from_secs(3600)));
        assert_eq!(parse_window("0h"), None);
        assert_eq!(parse_window("h"), None);
        assert_eq!(parse_window("10m"), None);
        assert_eq!(parse_window(""), None);
    }

    fn line(id: &str, model: &str) -> String {
        format!(
            r#"{{"custom_id":"{id}","method":"POST","url":"/v1/chat/completions","body":{{"model":"{model}","messages":[]}}}}"#
        )
    }

    #[test]
    fn a_valid_file_becomes_items() {
        let file = format!("{}\n\n{}\n", line("a", "m"), line("b", "m"));
        let parsed = parse_input(file.as_bytes(), "/v1/chat/completions", 10).unwrap();
        assert_eq!(parsed.model, "m");
        assert_eq!(parsed.items.len(), 2);
        assert_eq!(
            (parsed.items[1].line, parsed.items[1].custom_id.as_str()),
            (1, "b")
        );
        assert!(parsed.items[0].body.contains("\"model\":\"m\""));
    }

    #[test]
    fn invalid_lines_are_reported_with_their_line_numbers() {
        let file = [
            line("a", "m"),
            "not json".into(),
            line("a", "m"),
            line("c", "other"),
            line("d", "m").replace("/v1/chat/completions", "/v1/embeddings"),
        ]
        .join("\n");
        let errors = parse_input(file.as_bytes(), "/v1/chat/completions", 10).unwrap_err();
        let codes: Vec<(&str, u64)> = errors
            .iter()
            .map(|e| (e["code"].as_str().unwrap(), e["line"].as_u64().unwrap()))
            .collect();
        assert_eq!(
            codes,
            vec![
                ("invalid_json_line", 2),
                ("duplicate_custom_id", 3),
                ("mismatched_model", 4),
                ("mismatched_endpoint", 5),
            ]
        );
    }

    #[test]
    fn too_many_lines_and_empty_files_are_refused() {
        let file = [line("a", "m"), line("b", "m"), line("c", "m")].join("\n");
        let errors = parse_input(file.as_bytes(), "/v1/chat/completions", 2).unwrap_err();
        assert_eq!(errors[0]["code"], "too_many_requests");
        let errors = parse_input(b"\n\n", "/v1/chat/completions", 2).unwrap_err();
        assert_eq!(errors[0]["code"], "empty_file");
    }

    #[test]
    fn statuses_decide_retry_or_fail() {
        assert_eq!(classify(StatusCode::OK), Outcome::Success);
        assert_eq!(classify(StatusCode::TOO_MANY_REQUESTS), Outcome::Retry);
        assert_eq!(classify(StatusCode::SERVICE_UNAVAILABLE), Outcome::Retry);
        assert_eq!(classify(StatusCode::BAD_GATEWAY), Outcome::Retry);
        assert_eq!(classify(StatusCode::BAD_REQUEST), Outcome::Fail);
        assert_eq!(classify(StatusCode::FORBIDDEN), Outcome::Fail);
        assert!((5.0..=6.0).contains(&backoff(1)));
        assert!((10.0..=12.0).contains(&backoff(2)));
        assert!((300.0..=360.0).contains(&backoff(20)));
    }

    #[test]
    fn output_lines_follow_the_openai_shape() {
        let ok = OutputRow {
            line: 3,
            custom_id: "a".into(),
            status: "succeeded".into(),
            response_status: Some(200),
            response: Some(br#"{"id":"chatcmpl-1"}"#.to_vec()),
            error: None,
            request_id: None,
        };
        let v = output_line("batch_abc", &ok);
        assert_eq!(v["id"], "batch_req_abc_3");
        assert_eq!(v["custom_id"], "a");
        assert_eq!(v["response"]["status_code"], 200);
        assert_eq!(v["response"]["body"]["id"], "chatcmpl-1");
        assert!(v["error"].is_null());

        let expired = OutputRow {
            line: 4,
            custom_id: "b".into(),
            status: "expired".into(),
            response_status: None,
            response: None,
            error: Some(r#"{"code":"batch_expired","message":"late"}"#.into()),
            request_id: None,
        };
        let v = output_line("batch_abc", &expired);
        assert!(v["response"].is_null());
        assert_eq!(v["error"]["code"], "batch_expired");
    }
}
