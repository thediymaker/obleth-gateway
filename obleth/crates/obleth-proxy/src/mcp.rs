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
use obleth_config::ResolvedMcpServer;
use std::sync::Arc;

use crate::proxy::{bearer, error_json, forward_headers, has_path_traversal, resolve_key};
use crate::state::AppState;

const MCP_BODY_LIMIT: usize = 16 * 1024 * 1024;

/// Handle `/mcp/{server}` and `/mcp/{server}/{*rest}`.
#[tracing::instrument(skip_all, name = "mcp_request", fields(server = %params.server))]
pub async fn mcp_handler(
    State(state): State<AppState>,
    Path(params): Path<McpPath>,
    req: Request<Body>,
) -> Response<Body> {
    let (parts, body) = req.into_parts();
    let headers = parts.headers;

    // ---- reject path traversal in the `*rest` segment before any upstream work ----
    if let Some(rest) = params.rest.as_deref() {
        if has_path_traversal(rest) {
            return error_json(StatusCode::BAD_REQUEST, "invalid request path");
        }
    }

    // ---- auth (same obleth API key as the data plane) ----
    let Some(secret) = bearer(&headers) else {
        return error_json(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    let hash = obleth_config::hash_api_key(&secret);
    let Some(resolved) = resolve_key(&state, &hash).await else {
        return error_json(StatusCode::UNAUTHORIZED, "invalid api key");
    };
    if resolved.disabled {
        return error_json(StatusCode::FORBIDDEN, "api key disabled");
    }

    // ---- resolve the registered MCP server ----
    let Some(server) = resolve_mcp(&state, &params.server).await else {
        return error_json(
            StatusCode::NOT_FOUND,
            &format!("mcp server '{}' is not registered", params.server),
        );
    };
    if !server.enabled {
        return error_json(StatusCode::FORBIDDEN, "mcp server is disabled");
    }

    // ---- forward upstream ----
    let body_bytes = match axum::body::to_bytes(body, MCP_BODY_LIMIT).await {
        Ok(b) => b,
        Err(_) => return error_json(StatusCode::PAYLOAD_TOO_LARGE, "request body too large"),
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
            return error_json(StatusCode::BAD_GATEWAY, "mcp upstream request failed");
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

    builder
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
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
    use super::build_mcp_url;

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
