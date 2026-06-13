//! Minimal MCP client used by the gateway tool loop.
//!
//! Speaks JSON-RPC over MCP streamable HTTP directly against a registered MCP
//! server's `upstream_url` (the same target the `/mcp/{server}` reverse proxy
//! forwards to): `initialize` → `notifications/initialized` → `tools/list` /
//! `tools/call`, carrying the `mcp-session-id` header across the sequence.
//! Responses may arrive as plain JSON or SSE-framed JSON-RPC; both are parsed.
//!
//! A fresh session is opened per operation; the discovered tool list is cached
//! (see `AppState::tool_cache`) so the per-request cost on the hot path is one
//! cache read.

use std::time::Duration;

use obleth_config::ResolvedMcpServer;
use serde_json::{json, Value};

use crate::state::AppState;

/// MCP protocol version the gateway speaks.
const PROTOCOL_VERSION: &str = "2025-03-26";

/// One tool advertised by an MCP server, in gateway-neutral form.
#[derive(Debug, Clone)]
pub struct McpTool {
    pub name: String,
    pub description: String,
    /// JSON Schema for the tool's arguments (OpenAI `parameters`-compatible).
    pub input_schema: Value,
}

/// An open MCP session: the server URL, auth, and the session id (when the
/// server issued one on initialize). Reused across tool calls within one
/// request so rate-limited servers see one initialization, not one per call.
pub(super) struct Session {
    url: String,
    auth_header: Option<String>,
    session_id: Option<String>,
}

/// Open a session against `server` for repeated tool calls.
pub(super) async fn open_session(
    state: &AppState,
    server: &ResolvedMcpServer,
    timeout: Duration,
) -> anyhow::Result<Session> {
    match tokio::time::timeout(timeout, initialize(state, server)).await {
        Ok(r) => r,
        Err(_) => anyhow::bail!("mcp initialize timed out after {timeout:?}"),
    }
}

/// Execute one tool call on an existing session and return the flattened text
/// content of the result.
pub(super) async fn call_tool_in(
    state: &AppState,
    session: &Session,
    name: &str,
    arguments: Value,
    timeout: Duration,
) -> anyhow::Result<String> {
    let run = async {
        let result = rpc(
            state,
            session,
            3,
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        )
        .await?;
        let text = flatten_content(&result);
        if result
            .get("isError")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            anyhow::bail!("tool reported an error: {}", truncate(&text, 500));
        }
        Ok(text)
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(r) => r,
        Err(_) => anyhow::bail!("mcp tools/call `{name}` timed out after {timeout:?}"),
    }
}

/// List the tools advertised by `server` (uncached; see `cached_tools`).
pub async fn list_tools(
    state: &AppState,
    server: &ResolvedMcpServer,
    timeout: Duration,
) -> anyhow::Result<Vec<McpTool>> {
    let run = async {
        let session = initialize(state, server).await?;
        let result = rpc(state, &session, 2, "tools/list", json!({})).await?;
        let tools = result
            .get("tools")
            .and_then(|t| t.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(tools
            .iter()
            .filter_map(|t| {
                Some(McpTool {
                    name: t.get("name")?.as_str()?.to_string(),
                    description: t
                        .get("description")
                        .and_then(|d| d.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    input_schema: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({"type": "object", "properties": {}})),
                })
            })
            .collect())
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(r) => r,
        Err(_) => anyhow::bail!("mcp tools/list timed out after {timeout:?}"),
    }
}

/// Open a session: `initialize`, capture `mcp-session-id`, then send the
/// `notifications/initialized` notification.
async fn initialize(state: &AppState, server: &ResolvedMcpServer) -> anyhow::Result<Session> {
    let mut session = Session {
        url: server.upstream_url.clone(),
        auth_header: server.auth_header.clone(),
        session_id: None,
    };
    let body = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": PROTOCOL_VERSION,
            "capabilities": {},
            "clientInfo": { "name": "obleth-gateway", "version": env!("CARGO_PKG_VERSION") },
        },
    });
    let resp = post(state, &session, &body).await?;
    if let Some(id) = resp
        .headers()
        .get("mcp-session-id")
        .and_then(|v| v.to_str().ok())
    {
        session.session_id = Some(id.to_string());
    }
    let status = resp.status();
    let text = resp.text().await?;
    parse_rpc_result(&text, 1)
        .map_err(|e| anyhow::anyhow!("mcp initialize failed (status {status}): {e}"))?;

    // Fire-and-forget per the spec; some servers require it before requests.
    let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let _ = post(state, &session, &note).await;
    Ok(session)
}

/// Send one JSON-RPC request on the session and return its `result`.
async fn rpc(
    state: &AppState,
    session: &Session,
    id: u64,
    method: &str,
    params: Value,
) -> anyhow::Result<Value> {
    let body = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
    let resp = post(state, session, &body).await?;
    let status = resp.status();
    if !status.is_success() {
        anyhow::bail!("mcp {method} returned status {status}");
    }
    let text = resp.text().await?;
    parse_rpc_result(&text, id).map_err(|e| anyhow::anyhow!("mcp {method} failed: {e}"))
}

async fn post(
    state: &AppState,
    session: &Session,
    body: &Value,
) -> anyhow::Result<reqwest::Response> {
    let mut req = state
        .http
        .post(&session.url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .json(body);
    if let Some(auth) = &session.auth_header {
        req = req.header("authorization", auth);
    }
    if let Some(sid) = &session.session_id {
        req = req.header("mcp-session-id", sid);
    }
    Ok(req.send().await?)
}

/// Parse a streamable-HTTP response body — plain JSON or SSE-framed JSON-RPC —
/// and return the `result` of the response matching `id`.
fn parse_rpc_result(body: &str, id: u64) -> anyhow::Result<Value> {
    let candidates: Vec<Value> = if body.trim_start().starts_with('{') {
        serde_json::from_str(body.trim()).into_iter().collect()
    } else {
        // SSE frames: every `data: …` line is a candidate JSON-RPC message.
        body.lines()
            .filter_map(|line| line.strip_prefix("data:"))
            .filter_map(|payload| serde_json::from_str(payload.trim()).ok())
            .collect()
    };
    for msg in candidates {
        if msg.get("id").and_then(|v| v.as_u64()) != Some(id) {
            continue;
        }
        if let Some(err) = msg.get("error") {
            anyhow::bail!("json-rpc error: {err}");
        }
        if let Some(result) = msg.get("result") {
            return Ok(result.clone());
        }
    }
    anyhow::bail!("no json-rpc response with id {id} in body");
}

/// Flatten a `tools/call` result's content blocks into plain text.
fn flatten_content(result: &Value) -> String {
    let Some(content) = result.get("content").and_then(|c| c.as_array()) else {
        return String::new();
    };
    content
        .iter()
        .map(|item| match item.get("type").and_then(|t| t.as_str()) {
            Some("text") => item
                .get("text")
                .and_then(|t| t.as_str())
                .unwrap_or_default()
                .to_string(),
            _ => serde_json::to_string(item).unwrap_or_default(),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn truncate(s: &str, max: usize) -> &str {
    match s.char_indices().nth(max) {
        Some((idx, _)) => &s[..idx],
        None => s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_json_response() {
        let body = r#"{"jsonrpc":"2.0","id":2,"result":{"tools":[]}}"#;
        let result = parse_rpc_result(body, 2).unwrap();
        assert_eq!(result, json!({"tools": []}));
    }

    #[test]
    fn parses_sse_framed_response() {
        let body = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":3,\"result\":{\"content\":[{\"type\":\"text\",\"text\":\"hi\"}]}}\n\n";
        let result = parse_rpc_result(body, 3).unwrap();
        assert_eq!(flatten_content(&result), "hi");
    }

    #[test]
    fn surfaces_json_rpc_errors() {
        let body = r#"{"jsonrpc":"2.0","id":2,"error":{"code":-32601,"message":"nope"}}"#;
        let err = parse_rpc_result(body, 2).unwrap_err().to_string();
        assert!(err.contains("nope"));
    }

    #[test]
    fn ignores_messages_with_other_ids() {
        let body = "data: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"ok\":true}}\n\n";
        let result = parse_rpc_result(body, 2).unwrap();
        assert_eq!(result, json!({"ok": true}));
    }

    #[test]
    fn flattens_mixed_content() {
        let result = json!({
            "content": [
                { "type": "text", "text": "line one" },
                { "type": "image", "data": "..." },
            ]
        });
        let text = flatten_content(&result);
        assert!(text.starts_with("line one"));
        assert!(text.contains("image"));
    }
}
