//! MCP (Model Context Protocol) gateway.
//!
//! obleth fronts registered MCP servers with the same identity layer it applies
//! to LLM traffic: a client authenticates once with an obleth API key and reaches
//! any registered MCP server through `/mcp/{server}`. obleth authenticates,
//! resolves the server from its hot cache, injects the upstream credential, and
//! reverse-proxies the request (JSON-RPC over streamable-HTTP or SSE), streaming
//! the response straight back.
//!
//! This is deliberately transport-transparent: obleth does not parse JSON-RPC, so
//! any MCP-over-HTTP server works. Tool injection into chat completions is a
//! separate, later concern.

use axum::body::Body;
use axum::extract::{Path, State};
use axum::http::{header, Request, Response, StatusCode};
use futures_util::StreamExt;
use obleth_config::{ResolvedKey, ResolvedMcpServer, UsageRecord};
use std::sync::Arc;
use uuid::Uuid;

use crate::proxy::{bearer, error_json, forward_headers, has_path_traversal};
use crate::state::AppState;

const MCP_BODY_LIMIT: usize = 16 * 1024 * 1024;

/// Count one call to an MCP server in its daily stats: `tool` for a model's
/// tool call in the gateway's tool loop, otherwise a client's direct request
/// through `/mcp/<name>`. Fire and forget: stats never slow or fail a request.
pub fn record_call(state: &AppState, server: &str, tool: bool, ok: bool, ms: u64) {
    let redis = state.redis.clone();
    let server = server.to_string();
    let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
    tokio::spawn(async move {
        let fields = [
            (if tool { "tool_calls" } else { "direct" }, 1),
            ("errors", i64::from(!ok)),
            ("ms", ms as i64),
        ];
        if let Err(e) = redis.bump_daily("mcp", &server, &day, &fields).await {
            tracing::debug!(error = %e, "mcp daily stats not recorded");
        }
    });
}

/// The ledger's `request_type` for a client's call through `/mcp/<name>`.
pub const MCP_REQUEST_TYPE: &str = "mcp";

/// One client call through `/mcp/<name>` as a usage row, so it shows in the
/// request log beside model traffic: the server's name in `model`, admission
/// `direct` (MCP calls skip fair-share), the HTTP status, and the time until
/// the server's response headers, since an MCP session can stream long after.
/// No tokens or cost: obleth does not price MCP calls. The client's
/// `mcp-session-id` groups a session's calls.
fn mcp_usage_record(
    key: &ResolvedKey,
    request_id: Uuid,
    server: &str,
    session_id: Option<&str>,
    device_id: &str,
    status_code: u16,
    total_ms: u32,
) -> UsageRecord {
    UsageRecord {
        request_id,
        tenant_id: key.tenant_id,
        key_id: key.key_id,
        model: server.to_string(),
        admission: "direct".to_string(),
        weight: key.weight,
        input_tokens: 0,
        output_tokens: 0,
        cached_input_tokens: 0,
        estimated_tokens: 0,
        queue_wait_ms: 0,
        ttft_ms: 0,
        total_ms,
        status_code,
        cache_status: "off".to_string(),
        cost_usd: 0.0,
        energy_wh: 0.0,
        energy_cost_usd: 0.0,
        co2_g: 0.0,
        ts_ms: chrono::Utc::now().timestamp_millis(),
        session_id: session_id.unwrap_or_default().to_string(),
        session_id_source: if session_id.is_some() {
            "client"
        } else {
            "none"
        }
        .to_string(),
        // Synthetic tenants' traffic is `benchmark`, as on every endpoint.
        request_type: if key.synthetic {
            obleth_config::BENCHMARK_REQUEST_TYPE
        } else {
            MCP_REQUEST_TYPE
        }
        .to_string(),
        device_id: device_id.to_string(),
        end_user: String::new(),
        parent_request_id: Uuid::nil(),
        model_variant: String::new(),
        reasoning_effort: String::new(),
        reasoning_effort_requested: String::new(),
        model_family: String::new(),
        key_name: key.key_name.clone(),
        tenant_name: key.tenant_name.clone(),
        streamed: false,
        error_class: obleth_config::class_for_status(status_code, "direct")
            .map_or("", obleth_config::ErrorClass::as_str)
            .to_string(),
        error_reason: obleth_config::reason_for_status(status_code, "direct").to_string(),
    }
}

/// Handle `/mcp/{server}` and `/mcp/{server}/{*rest}`. Every answer after
/// authentication carries its `x-obleth-request-id`, the id of its row in the
/// request log.
#[tracing::instrument(skip_all, name = "mcp_request", fields(server = %params.server))]
pub async fn mcp_handler(
    State(state): State<AppState>,
    Path(params): Path<McpPath>,
    req: Request<Body>,
) -> Response<Body> {
    let request_id = Uuid::new_v4();
    let (mut resp, logged) = handle(&state, params, req, request_id).await;
    if logged {
        if let Ok(value) = header::HeaderValue::from_str(&request_id.to_string()) {
            resp.headers_mut()
                .entry("x-obleth-request-id")
                .or_insert(value);
        }
    }
    resp
}

/// The call itself, and whether it reached the ledger (it does once the
/// credential is known).
async fn handle(
    state: &AppState,
    params: McpPath,
    req: Request<Body>,
    request_id: Uuid,
) -> (Response<Body>, bool) {
    let (parts, body) = req.into_parts();
    let headers = parts.headers;

    // ---- reject path traversal in the `*rest` segment before any upstream work ----
    if let Some(rest) = params.rest.as_deref() {
        if has_path_traversal(rest) {
            return (
                error_json(StatusCode::BAD_REQUEST, "invalid request path"),
                false,
            );
        }
    }

    // ---- auth: same bearer credential the data plane accepts, JWT or
    // secret key (`crate::jwt_auth::authenticate_credential`) ----
    let Some(secret) = bearer(&headers) else {
        return (
            error_json(StatusCode::UNAUTHORIZED, "missing bearer token"),
            false,
        );
    };
    let (resolved, device_id) = match crate::jwt_auth::authenticate_credential(state, &secret).await
    {
        Ok(cred) => (cred.resolved, cred.device_id),
        Err(resp) => return (resp, false),
    };
    // Same gate as the data plane: disabled key, inactive tenant, schedule.
    if let Err(resp) = crate::proxy::gate_resolved_key(state, &resolved) {
        return (resp, false);
    }

    // From here every answer is a row in the ledger. Internal keys are not
    // recorded, as in `finalize`.
    let session_id = headers
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let started = std::time::Instant::now();
    let ledger = |server: &str, status: StatusCode| {
        if resolved.internal {
            return;
        }
        state.telemetry.record(mcp_usage_record(
            &resolved,
            request_id,
            server,
            session_id.as_deref(),
            &device_id,
            status.as_u16(),
            started.elapsed().as_millis() as u32,
        ));
    };

    // ---- resolve the registered MCP server ----
    let Some(server) = resolve_mcp(state, &params.server).await else {
        ledger(&params.server, StatusCode::NOT_FOUND);
        let msg = format!("mcp server '{}' is not registered", params.server);
        return (error_json(StatusCode::NOT_FOUND, &msg), true);
    };
    if !server.enabled {
        ledger(&server.name, StatusCode::FORBIDDEN);
        return (
            error_json(StatusCode::FORBIDDEN, "mcp server is disabled"),
            true,
        );
    }

    // ---- forward upstream ----
    let body_bytes = match axum::body::to_bytes(body, MCP_BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => {
            ledger(&server.name, StatusCode::PAYLOAD_TOO_LARGE);
            let resp = error_json(StatusCode::PAYLOAD_TOO_LARGE, "request body too large");
            return (resp, true);
        }
    };
    let url = build_mcp_url(
        &server.upstream_url,
        params.rest.as_deref(),
        parts.uri.query(),
    );
    let mut fwd_headers = forward_headers(&headers);
    if let Some(auth) = &server.auth_header {
        if let Ok(v) = header::HeaderValue::from_str(auth) {
            fwd_headers.insert(header::AUTHORIZATION, v);
        }
    }

    let upstream = state
        .http
        .request(parts.method, &url)
        .headers(fwd_headers)
        .body(body_bytes)
        .send()
        .await;
    let upstream = match upstream {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, server = %server.name, "mcp upstream request failed");
            state.alerts.issue(
                "mcp_upstream_request_failed",
                "MCP upstream request failed",
                format!(
                    "server `{}` tenant `{}` upstream `{url}`: {e}",
                    server.name, resolved.tenant_name
                ),
            );
            state.metrics.record_mcp(&server.name, 502);
            record_call(
                state,
                &server.name,
                false,
                false,
                started.elapsed().as_millis() as u64,
            );
            ledger(&server.name, StatusCode::BAD_GATEWAY);
            let resp = error_json(StatusCode::BAD_GATEWAY, "mcp upstream request failed");
            return (resp, true);
        }
    };

    let status = upstream.status();
    if status.is_server_error() {
        state.alerts.issue(
            "mcp_upstream_5xx_response",
            "MCP upstream returned a server error",
            format!(
                "server `{}` tenant `{}` status `{}`",
                server.name, resolved.tenant_name, status
            ),
        );
    }
    state.metrics.record_mcp(&server.name, status.as_u16());
    record_call(
        state,
        &server.name,
        false,
        !status.is_server_error(),
        started.elapsed().as_millis() as u64,
    );
    ledger(&server.name, status);

    // Forward the upstream response headers, dropping only what the re-stream
    // invalidates. MCP streamable-HTTP servers carry protocol state in
    // headers — notably `mcp-session-id` on the initialize response — so
    // stripping them breaks every follow-up request in the session.
    let mut builder = Response::builder().status(status);
    if let Some(headers_out) = builder.headers_mut() {
        for (name, value) in upstream.headers() {
            match name.as_str() {
                "transfer-encoding" | "content-length" | "connection" => continue,
                _ => {
                    headers_out.insert(name.clone(), value.clone());
                }
            }
        }
        if !headers_out.contains_key(header::CONTENT_TYPE) {
            headers_out.insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
        }
    }

    // Stream the response straight through (handles both JSON and SSE).
    let stream = upstream
        .bytes_stream()
        .map(|chunk| chunk.map_err(std::io::Error::other));

    let resp = builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"));
    (resp, true)
}

#[derive(serde::Deserialize)]
pub struct McpPath {
    server: String,
    rest: Option<String>,
}

/// Resolve a registered MCP server via moka, falling back to Redis. Also used
/// by the gateway tool loop to reach granted servers directly.
pub(crate) async fn resolve_mcp(state: &AppState, name: &str) -> Option<Arc<ResolvedMcpServer>> {
    if let Some(s) = state.mcp_cache.get(name).await {
        return Some(s);
    }
    match state.redis.get_resolved_mcp_server(name).await {
        Ok(Some(s)) => {
            let s = Arc::new(s);
            state.mcp_cache.insert(name.to_string(), s.clone()).await;
            Some(s)
        }
        Ok(None) => None,
        Err(e) => {
            tracing::warn!(error = %e, "mcp server lookup failed");
            state.alerts.issue(
                "redis_mcp_lookup_failed",
                "Redis MCP server lookup failed",
                format!("server `{name}` lookup failed against Redis: {e}"),
            );
            None
        }
    }
}

fn build_mcp_url(base: &str, rest: Option<&str>, query: Option<&str>) -> String {
    let mut url = match rest {
        Some(r) if !r.is_empty() => {
            let base = base.trim_end_matches('/');
            format!("{base}/{}", r.trim_start_matches('/'))
        }
        // Preserve a trailing slash on the registered base URL (some MCP servers
        // require `/mcp/` not `/mcp`).
        _ => base.to_string(),
    };
    if let Some(q) = query {
        url.push('?');
        url.push_str(q);
    }
    url
}

#[cfg(test)]
mod tests {
    use super::{build_mcp_url, mcp_usage_record, MCP_REQUEST_TYPE};
    use uuid::Uuid;

    #[test]
    fn an_mcp_call_is_a_request_log_row() {
        let mut key = crate::boons::test_support::test_key();
        key.key_name = "jlee379-cli".into();
        let id = Uuid::new_v4();
        let row = mcp_usage_record(&key, id, "github", Some("sess-1"), "", 200, 85);
        assert_eq!(row.request_id, id);
        assert_eq!(row.request_type, MCP_REQUEST_TYPE);
        assert_eq!(
            (row.model.as_str(), row.admission.as_str()),
            ("github", "direct")
        );
        assert_eq!((row.status_code, row.total_ms, row.ttft_ms), (200, 85, 0));
        assert_eq!(
            (row.session_id.as_str(), row.session_id_source.as_str()),
            ("sess-1", "client")
        );
        assert_eq!(
            (row.input_tokens, row.output_tokens, row.cost_usd),
            (0, 0, 0.0)
        );
        assert_eq!(row.key_name, "jlee379-cli");
        assert!(!row.streamed);

        let row = mcp_usage_record(&key, id, "github", None, "", 502, 3);
        assert_eq!(
            (row.session_id.as_str(), row.session_id_source.as_str()),
            ("", "none")
        );
        key.synthetic = true;
        let row = mcp_usage_record(&key, id, "github", None, "", 200, 3);
        assert_eq!(row.request_type, obleth_config::BENCHMARK_REQUEST_TYPE);
    }

    #[test]
    fn every_mcp_answer_after_the_key_gate_reaches_the_ledger() {
        // Pinned at the source level for the same reason as the key gate
        // test below. Past the gate, each of the four early answers (404, 403,
        // 413, 502) and the server's own answer records its row, and none is
        // handed back as unlogged.
        let src = include_str!("mcp.rs");
        let handler = &src[..src.find("\nmod tests {").expect("the test module")];
        let logged = &handler[handler
            .find("From here every answer is a row in the ledger")
            .expect("the marker")..];
        assert_eq!(logged.matches("return (").count(), 4);
        assert_eq!(logged.matches("ledger(&").count(), 5);
        assert!(!logged.contains(", false)"));
    }

    #[test]
    fn mcp_handler_applies_the_full_key_gate() {
        // `/mcp/{server}` must refuse exactly what the data plane refuses: a
        // disabled key, a suspended/pending tenant, or a tenant outside its
        // schedule. A bare `disabled` check let suspended tenants through.
        // There is no AppState harness in this crate (it needs live Redis and
        // ClickHouse), so the wiring is pinned at the source level.
        let src = include_str!("mcp.rs");
        let handler = &src[..src.find("\nmod tests {").expect("the test module")];
        let auth = handler
            .find("authenticate_credential(")
            .expect("the auth call");
        let gate = handler
            .find("crate::proxy::gate_resolved_key(state, &resolved)")
            .expect("the MCP handler must call gate_resolved_key");
        let upstream = handler
            .find("resolve_mcp(state")
            .expect("the server lookup");
        assert!(
            auth < gate && gate < upstream,
            "gate after auth, before any upstream work"
        );
        assert!(
            !handler.contains("if resolved.disabled"),
            "the partial disabled-only check must not come back"
        );
    }

    #[test]
    fn appends_rest_and_query() {
        assert_eq!(
            build_mcp_url(
                "https://mcp.example.com/",
                Some("messages"),
                Some("session=1")
            ),
            "https://mcp.example.com/messages?session=1"
        );
    }

    #[test]
    fn bare_base_unchanged() {
        assert_eq!(
            build_mcp_url("https://mcp.example.com", None, None),
            "https://mcp.example.com"
        );
    }

    #[test]
    fn preserves_trailing_slash_on_base() {
        assert_eq!(
            build_mcp_url("http://searxng:8888/mcp/", None, None),
            "http://searxng:8888/mcp/"
        );
    }
}
