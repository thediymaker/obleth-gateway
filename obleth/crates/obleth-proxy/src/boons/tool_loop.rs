//! The **gateway tool loop**: gives a model actual tools, not just the tool
//! *calling* capability.
//!
//! Capabilities vs tools: a capability is what the model can do natively
//! (function calling); a tool is something registered at the gateway (an MCP
//! server like SearXNG). When an operator grants a model access to tool
//! servers (`ModelRoute::tool_servers`), the gateway:
//!
//! 1. discovers the servers' tools (cached `tools/list`) and injects them into
//!    plain chat requests (only models with native function calling are eligible);
//! 2. intercepts the response; when the model calls a tool, the gateway
//!    executes it against the MCP server, appends the result, and re-asks the
//!    model — looping (bounded) until the model produces a final answer.
//!
//! The client sends a plain OpenAI chat request and receives a grounded final
//! answer; it never sees tool definitions or tool calls. Clients that bring
//! their own `tools` (agentic clients like an IDE assistant) still get the
//! granted MCP tools *merged* into their set: the gateway executes only its
//! own MCP tools and hands any client-owned tool call straight back to the
//! client, so the client keeps full control of its own tools while the model
//! still gains the tools the operator granted it.
//!
//! This module is the **buffered** loop, used for non-streaming clients;
//! [`super::tool_stream`] is the token-by-token streaming variant. Both share
//! request enrichment ([`inject`]) and per-call execution ([`execute_call`],
//! [`push_message`]); keep behavioral fixes applied to both paths.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use obleth_config::{ResolvedKey, ResolvedModel, ToolLoopSettings};
use obleth_tokenizer::Tokenizer;
use serde_json::{json, Value};

use super::mcp_tools::{self, McpTool};
use super::respond::TransformResult;
use super::ResponsePlan;
use crate::state::AppState;

/// Cap on a single tool result fed back to the model (bounds context growth).
const TOOL_RESULT_MAX_CHARS: usize = 16_000;

/// Name of the gateway-executed tool the compression boon injects so a model can
/// pull back content that was lossily summarized.
pub(super) const RETRIEVE_ORIGINAL_TOOL: &str = "retrieve_original";
/// Synthetic "server" name registered in the tool map for `retrieve_original`,
/// so the loop recognizes the tool as gateway-owned (not a client tool to pass
/// through). The value is never used as a real MCP server; `execute_call`
/// short-circuits the call by name before any server lookup.
pub(super) const COMPRESSION_SYNTHETIC_SERVER: &str = "__compression__";

/// Warning reported when a request armed both this boon and the
/// structured-output boon: the image is generated and billed, but attaching
/// markdown to a schema-validated completion would corrupt it, so the schema
/// wins and the suppression is surfaced on `BOONS_WARNING_HEADER`.
pub(super) const IMAGE_SUPPRESSED_WARNING: &str = "image_suppressed_by_response_format";

/// Warning reported when billed images could not be attached to the final
/// completion because it lacked `/choices/0/message` (an error body, a
/// malformed reply). The generation still happened and was billed; this is
/// the last point in the request where that fact would otherwise vanish with
/// no signal to the client or the logs.
pub(super) const IMAGE_ATTACH_FAILED_WARNING: &str = "image_attach_failed";

/// The OpenAI function-tool definition for `retrieve_original`.
pub(super) fn retrieve_original_tool_def() -> Value {
    json!({
        "type": "function",
        "function": {
            "name": RETRIEVE_ORIGINAL_TOOL,
            "description": "Retrieve the full original text of content that was \
                summarized to save space. Pass the exact hash shown in a \
                [ref:HASH] marker.",
            "parameters": {
                "type": "object",
                "properties": {
                    "ref": {
                        "type": "string",
                        "description": "The hash from a [ref:HASH] marker."
                    }
                },
                "required": ["ref"]
            }
        }
    })
}

/// Format a `retrieve_original` result for the model. `reference` is the ref the
/// model passed (None when it omitted the argument); `content` is the Redis
/// lookup result (None = miss/expired). Fail-soft: a miss or missing ref returns
/// a readable message, never an error that breaks the loop.
fn format_retrieve_result(reference: Option<&str>, content: Option<String>) -> String {
    match (reference, content) {
        (None, _) => "Error: retrieve_original requires a `ref` argument (the hash \
                      from a [ref:HASH] marker)."
            .to_string(),
        (Some(_), Some(original)) => original,
        (Some(r), None) => format!(
            "The original content for ref {r} is no longer available (it may have expired)."
        ),
    }
}

/// Response-side state for the tool loop, armed at request enrichment.
pub struct ToolLoopPlan {
    /// Tool name -> MCP server name, for executing calls.
    pub tool_servers: HashMap<String, String>,
    /// The enriched request body (tools injected, `stream` forced off), used
    /// to re-dispatch follow-up turns with the conversation appended.
    pub request: Value,
    /// Settings snapshot taken at request time.
    pub settings: ToolLoopSettings,
    /// True when the client supplied its own `tools` and the gateway merged
    /// its granted tools in. Any tool call the model makes against a name the
    /// gateway does not own (i.e. a client tool) is returned to the client
    /// untouched instead of being executed or error-recovered.
    pub passthrough_unmapped: bool,
    /// Image-generation boon settings snapshot, present when the boon armed
    /// the loop. `None` leaves `generate_image` unhandled, which is correct:
    /// the tool is only ever injected alongside this field.
    pub image_gen: Option<obleth_config::ImageGenerationBoonSettings>,
    /// URL of the upstream that answered turn 0, when the dispatcher records
    /// it. Follow-up turns go to the same endpoint so they reuse its prefix
    /// cache. `None` falls back to the model's endpoint selection with this
    /// request's session key, which picks the same first endpoint the proxy
    /// tried in `failover` and `session_hash` modes.
    pub served_url: Option<String>,
}

/// Inject the granted servers' tools into a chat request. Returns the
/// tool-name -> server-name map when at least one tool was injected. Fail-open:
/// discovery errors leave the request unchanged (`None`).
///
/// `nudge` is the system instruction that tells the model it has tools and when
/// to use them; pass `None` (or an empty string) to skip it — e.g. for agentic
/// clients that brought their own `tools` and manage their own tool-use policy.
pub(super) async fn inject(
    state: &AppState,
    route: &ResolvedModel,
    nudge: Option<&str>,
    json: &mut Value,
) -> Option<HashMap<String, String>> {
    // Client-supplied tool names win on a collision and are never executed by
    // the gateway: the model already knows them as the client's tools and the
    // client expects to handle the call itself.
    let client_tool_names: std::collections::HashSet<String> = json
        .get("tools")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.pointer("/function/name").and_then(|n| n.as_str()))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();

    let mut tool_defs: Vec<Value> = Vec::new();
    let mut map: HashMap<String, String> = HashMap::new();
    for server_name in &route.tool_servers {
        let Some(tools) = cached_tools(state, server_name).await else {
            continue;
        };
        for tool in tools.iter() {
            if client_tool_names.contains(&tool.name) {
                tracing::warn!(
                    tool = %tool.name,
                    server = %server_name,
                    "granted MCP tool name collides with a client-supplied tool; client wins"
                );
                continue;
            }
            if map.contains_key(&tool.name) {
                tracing::warn!(
                    tool = %tool.name,
                    server = %server_name,
                    "duplicate tool name across granted MCP servers; first one wins"
                );
                continue;
            }
            map.insert(tool.name.clone(), server_name.clone());
            tool_defs.push(json!({
                "type": "function",
                "function": {
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.input_schema,
                },
            }));
        }
    }
    if tool_defs.is_empty() {
        return None;
    }
    if let Some(obj) = json.as_object_mut() {
        // Merge into any client-supplied tools rather than replacing them.
        match obj.get_mut("tools").and_then(|v| v.as_array_mut()) {
            Some(existing) => existing.extend(tool_defs),
            None => {
                obj.insert("tools".into(), Value::Array(tool_defs));
            }
        }
    }
    if let Some(nudge) = nudge.map(str::trim).filter(|n| !n.is_empty()) {
        super::structured::inject_prompt_section(json, nudge, route.supports_system_messages);
    }
    Some(map)
}

/// How long a failed `tools/list` is remembered. Without it every request to
/// a tool-granted model re-pays the full discovery timeout while the server is
/// down; long enough to shed that load, short enough that a recovered server
/// is picked up quickly.
const MCP_DISCOVERY_NEGATIVE_TTL: Duration = Duration::from_secs(45);

type ToolCache = moka::future::Cache<String, Arc<Vec<McpTool>>>;

/// Failed discoveries, cached as an empty tool list. Separate from
/// `AppState::tool_cache` because that cache's 10-minute TTL is right for a
/// success and far too long for a failure.
fn discovery_failures() -> &'static ToolCache {
    static CACHE: std::sync::OnceLock<ToolCache> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| {
        moka::future::Cache::builder()
            .time_to_live(MCP_DISCOVERY_NEGATIVE_TTL)
            .max_capacity(1_000)
            .build()
    })
}

/// Discovered tools for one server, via the short-TTL cache.
async fn cached_tools(state: &AppState, server_name: &str) -> Option<Arc<Vec<McpTool>>> {
    if let Some(tools) = state.tool_cache.get(server_name).await {
        return Some(tools);
    }
    if let Some(empty) = discovery_failures().get(server_name).await {
        return Some(empty);
    }
    let Some(server) = crate::mcp::resolve_mcp(state, server_name).await else {
        tracing::warn!(server = %server_name, "granted MCP server is not registered");
        return None;
    };
    if !server.enabled {
        return None;
    }
    discover_coalesced(
        &state.tool_cache,
        discovery_failures(),
        server_name,
        mcp_tools::list_tools(state, &server, Duration::from_secs(10)),
    )
    .await
}

/// Run one discovery for `server_name`, shared by every concurrent miss
/// (moka `try_get_with` runs a single init per key and hands its result to all
/// waiters), so a burst of requests after expiry sends one `tools/list`, not
/// one per request. A failure is recorded in `failures` as an empty list.
async fn discover_coalesced<F>(
    cache: &ToolCache,
    failures: &ToolCache,
    server_name: &str,
    fetch: F,
) -> Option<Arc<Vec<McpTool>>>
where
    F: std::future::Future<Output = anyhow::Result<Vec<McpTool>>>,
{
    match cache
        .try_get_with(server_name.to_string(), async { fetch.await.map(Arc::new) })
        .await
    {
        Ok(tools) => Some(tools),
        Err(e) => {
            tracing::warn!(error = %e, server = %server_name, "mcp tool discovery failed");
            let empty = Arc::new(Vec::new());
            failures
                .insert(server_name.to_string(), empty.clone())
                .await;
            Some(empty)
        }
    }
}

/// Per-request MCP session state, shared by both loop variants. Generic only
/// so tests can stand in for a real session.
pub(super) struct Sessions<S = mcp_tools::Session> {
    open: HashMap<String, S>,
    /// Servers whose `initialize` already failed on this request, with the
    /// error. Retrying a dead server on every call would cost a full tool
    /// timeout per call.
    failed: HashMap<String, String>,
}

impl<S> Default for Sessions<S> {
    fn default() -> Self {
        Sessions {
            open: HashMap::new(),
            failed: HashMap::new(),
        }
    }
}

impl<S> Sessions<S> {
    /// The open session for `server`, opening it on first use. A server whose
    /// open already failed on this request fails again immediately with the
    /// original error.
    async fn get_or_open<F, Fut>(&mut self, server: &str, open: F) -> Result<&S, String>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = anyhow::Result<S>>,
    {
        if let Some(err) = self.failed.get(server) {
            return Err(err.clone());
        }
        if !self.open.contains_key(server) {
            match open().await {
                Ok(session) => {
                    self.open.insert(server.to_string(), session);
                }
                Err(e) => {
                    tracing::warn!(error = %e, server = %server, "mcp session open failed");
                    let msg = e.to_string();
                    self.failed.insert(server.to_string(), msg.clone());
                    return Err(msg);
                }
            }
        }
        self.open
            .get(server)
            .ok_or_else(|| "session missing after open".to_string())
    }
}

/// Wall-clock budget for one request's whole tool loop.
pub(super) struct LoopDeadline {
    at: std::time::Instant,
}

impl LoopDeadline {
    pub(super) fn new(settings: &ToolLoopSettings) -> Self {
        // `deadline_secs` is read back from settings JSON, which may hold a
        // value written before the admin-side clamp existed; clamp again here
        // so a stale over-limit value can't outrun `after`'s own fallback.
        let secs = settings
            .deadline_secs
            .clamp(1, obleth_config::TOOL_LOOP_MAX_DEADLINE_SECS);
        Self::after(Duration::from_secs(secs))
    }

    pub(super) fn after(budget: Duration) -> Self {
        // A stored budget too large to add to `Instant::now()` must not panic
        // the request path (boons fail open): fall back to the maximum an
        // operator may configure.
        let now = std::time::Instant::now();
        let at = now.checked_add(budget).unwrap_or_else(|| {
            now + Duration::from_secs(obleth_config::TOOL_LOOP_MAX_DEADLINE_SECS)
        });
        LoopDeadline { at }
    }

    /// `timeout` shortened to the time left, or `None` once the budget is
    /// spent.
    pub(super) fn bound(&self, timeout: Duration) -> Option<Duration> {
        let left = self.at.saturating_duration_since(std::time::Instant::now());
        (!left.is_zero()).then(|| left.min(timeout))
    }
}

/// Warning reported when the loop stopped because its wall-clock budget ran
/// out.
pub(super) const DEADLINE_WARNING: &str = "tool_loop_deadline_exceeded";

/// Tool result for a call past [`obleth_config::TOOL_LOOP_MAX_CALLS_PER_TURN`].
pub(super) fn over_call_cap_result(name: &str) -> String {
    format!(
        "Error: tool `{name}` was not executed: at most {} tool calls run per turn.",
        obleth_config::TOOL_LOOP_MAX_CALLS_PER_TURN
    )
}

/// Tool result for a call reached after the loop's time budget ran out.
pub(super) fn deadline_result(name: &str) -> String {
    format!(
        "Error: tool `{name}` was not executed: the tool time limit for this request was reached."
    )
}

/// `(prompt_tokens, completion_tokens)` from a completion, read the way the
/// proxy's settle path reads it (a missing `prompt_tokens` means "no usage").
fn completion_usage(body: &Value) -> Option<(u32, u32)> {
    let input = body.pointer("/usage/prompt_tokens")?.as_u64()? as u32;
    let output = body
        .pointer("/usage/completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    Some((input, output))
}

/// Attach generated images to the final completion and reconcile the warning.
///
/// `structured_armed` is `plan.structured.is_some()`. When a schema is armed the
/// images are deliberately *not* attached — appending markdown to a
/// schema-validated JSON completion produces output that is neither valid JSON
/// nor a rendered image, in either order — and the suppression is reported
/// instead. The generation is still billed, because it happened.
///
/// `existing` is whatever warning the structured transform already produced; a
/// real validation failure is never overwritten by the suppression note.
fn finish_images(
    images: &[super::image_gen::GeneratedImage],
    structured_armed: bool,
    body: &mut Value,
    existing: Option<&'static str>,
) -> Option<&'static str> {
    if images.is_empty() {
        return existing;
    }
    if structured_armed {
        tracing::warn!(
            images = images.len(),
            "image-generation boon produced images on a request that also asked for a \
             response_format schema; the images are suppressed so the validated JSON \
             stays intact"
        );
        return existing.or(Some(IMAGE_SUPPRESSED_WARNING));
    }
    if !super::image_gen::attach_to_completion(images, body) {
        // The images were already billed the moment upstream generation
        // succeeded; a malformed completion (no `/choices/0/message`) must
        // not let that billing vanish with zero signal to the client or the
        // logs. A pre-existing, more specific warning still wins.
        tracing::warn!(
            images = images.len(),
            "billed images could not be attached: malformed completion"
        );
        return existing.or(Some(IMAGE_ATTACH_FAILED_WARNING));
    }
    existing
}

/// Drive the tool loop over a buffered completion: execute the model's tool
/// calls against their MCP servers, append the results, re-dispatch, and
/// repeat until the model answers (or the turn limit is hit). Mutates `body`
/// into the final client-facing completion.
///
/// Every follow-up turn is billed as a `tool_loop` helper row, so once `body`
/// has been replaced the result carries turn 0's usage in
/// [`TransformResult::turn0_usage`] for the main row.
#[allow(clippy::too_many_arguments)]
pub async fn run(
    state: &AppState,
    plan: &ResponsePlan,
    route: Option<&ResolvedModel>,
    key: &ResolvedKey,
    session_id: &str,
    dispatch_timeout: Duration,
    body: &mut Value,
    mut tracer: Option<&mut crate::tracer::SpanRecorder>,
) -> TransformResult {
    let Some(loop_plan) = &plan.tool_loop else {
        return super::respond::transform_completion(state, plan, route, key, session_id, body)
            .await;
    };
    let Some(route) = route else {
        return TransformResult {
            warning: None,
            turn0_usage: None,
        };
    };
    // Captured before the loop can overwrite `body`: the main request row must
    // settle with turn 0, not with whatever turn the loop ends on.
    let turn0 = completion_usage(body);
    let mut replaced = false;
    let mut request = loop_plan.request.clone();
    if let Some(obj) = request.as_object_mut() {
        obj.insert("model".into(), Value::String(route.upstream_model.clone()));
    }
    let tool_timeout = Duration::from_millis(loop_plan.settings.tool_timeout_ms.max(1));
    let max_turns = loop_plan
        .settings
        .max_turns
        .clamp(1, obleth_config::TOOL_LOOP_MAX_TURNS);
    let deadline = LoopDeadline::new(&loop_plan.settings);
    // Resolved once so every follow-up turn lands on the same endpoint.
    let mut target = super::helper_target(route, session_id, loop_plan.served_url.as_deref());
    // One MCP session per server for the whole request: rate-limited servers
    // see a single initialization instead of one per tool call.
    let mut sessions = Sessions::default();
    let tool_loop_start = crate::tracer::now_ms();
    let mut image_ctx = loop_plan
        .image_gen
        .as_ref()
        .map(|cfg| super::image_gen::ImageCtx {
            cfg,
            key,
            session_id,
            images: Vec::new(),
            events: Vec::new(),
        });

    let mut completed_turns: u32 = 0;
    // Deduplicated list of every tool name called across all turns, for the
    // parent span summary.
    let mut all_tools_seen: Vec<String> = Vec::new();

    for turn in 0..max_turns {
        let calls = extract_tool_calls(body);
        // When the client brought its own tools, a call against a name the
        // gateway does not own belongs to the client. Hand the completion back
        // untouched so the client drives that tool turn itself.
        if loop_plan.passthrough_unmapped
            && calls
                .iter()
                .any(|c| !loop_plan.tool_servers.contains_key(&c.name))
        {
            tracing::debug!("tool loop yielding client-owned tool call back to the client");
            if let Some(t) = tracer {
                t.record_elapsed(
                    "boon:tool_loop",
                    "proxy_request",
                    tool_loop_start,
                    "ok",
                    serde_json::json!({ "turns": completed_turns, "tools": all_tools_seen }),
                );
            }
            let warning = finish_images(
                image_ctx
                    .as_ref()
                    .map(|c| c.images.as_slice())
                    .unwrap_or(&[]),
                plan.structured.is_some(),
                body,
                None,
            );
            return TransformResult {
                warning,
                turn0_usage: replaced.then(|| turn0_or_estimate(state, loop_plan, turn0)),
            };
        }
        if calls.is_empty() {
            // Final answer. Apply the structured-output transform when armed.
            let warning = match &plan.structured {
                Some(structured_plan) => {
                    super::respond::apply_structured(
                        state,
                        structured_plan,
                        Some(route),
                        key,
                        session_id,
                        body,
                    )
                    .await
                }
                None => None,
            };
            let warning = finish_images(
                image_ctx
                    .as_ref()
                    .map(|c| c.images.as_slice())
                    .unwrap_or(&[]),
                plan.structured.is_some(),
                body,
                warning,
            );
            if let Some(t) = tracer {
                t.record_elapsed(
                    "boon:tool_loop",
                    "proxy_request",
                    tool_loop_start,
                    "ok",
                    serde_json::json!({ "turns": completed_turns, "tools": all_tools_seen }),
                );
            }
            return TransformResult {
                warning,
                turn0_usage: replaced.then(|| turn0_or_estimate(state, loop_plan, turn0)),
            };
        }

        // Collect the tool names for this iteration and update the all-turns list.
        let iter_tool_names: Vec<String> = calls.iter().map(|c| c.name.clone()).collect();
        for name in &iter_tool_names {
            if !all_tools_seen.contains(name) {
                all_tools_seen.push(name.clone());
            }
        }
        let iter_start = crate::tracer::now_ms();

        // Record the assistant turn, execute each call, and append results.
        if let Some(message) = body.pointer("/choices/0/message") {
            push_message(&mut request, message.clone());
        }
        let tool_exec_start = crate::tracer::now_ms();
        for (index, call) in calls.iter().enumerate() {
            let result_text = run_one_call(
                state,
                &key.tenant_id,
                &loop_plan.tool_servers,
                &mut sessions,
                image_ctx.as_mut(),
                call,
                index,
                &deadline,
                tool_timeout,
            )
            .await;
            tracing::debug!(
                tool = %call.name,
                turn,
                chars = result_text.len(),
                "gateway tool loop executed a tool call"
            );
            push_message(
                &mut request,
                json!({
                    "role": "tool",
                    "tool_call_id": call.id,
                    "content": result_text,
                }),
            );
        }
        if let Some(ctx) = image_ctx.as_mut() {
            for event in ctx.events.drain(..) {
                if let Some(t) = tracer.as_deref_mut() {
                    t.record(
                        "boon:image_generation",
                        "boon:tool_loop",
                        tool_exec_start,
                        event.upstream_ms,
                        if event.ok { "ok" } else { "error" },
                        serde_json::json!({
                            "images": event.images,
                            "size": event.size,
                            "model": event.model,
                        }),
                    );
                }
            }
        }
        let tool_exec_ms = (crate::tracer::now_ms() - tool_exec_start) as u32;

        let model_call_start = crate::tracer::now_ms();
        let (dispatched, failure_warning) = match deadline.bound(dispatch_timeout) {
            None => (
                Err(anyhow::anyhow!("tool loop time budget exhausted")),
                DEADLINE_WARNING,
            ),
            Some(timeout) => (
                dispatch_follow_up(&state.http, &mut target, request.clone(), timeout).await,
                "tool_loop_dispatch_failed",
            ),
        };
        match dispatched {
            Ok(completion) => {
                let model_ms = (crate::tracer::now_ms() - model_call_start) as u32;
                let iter_ms = (crate::tracer::now_ms() - iter_start) as u32;
                let (input_tokens, output_tokens) = reported_tokens(&completion);
                super::bill_helper_call(
                    state,
                    route,
                    key,
                    session_id,
                    "tool_loop",
                    input_tokens,
                    output_tokens,
                );
                *body = completion;
                replaced = true;
                completed_turns += 1;
                if let Some(t) = tracer.as_deref_mut() {
                    t.record(
                        &format!("boon:tool_loop:iter:{turn}"),
                        "boon:tool_loop",
                        iter_start,
                        iter_ms,
                        "ok",
                        serde_json::json!({
                            "tools": iter_tool_names,
                            "tool_ms": tool_exec_ms,
                            "model_ms": model_ms,
                        }),
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    model = %route.model_name,
                    turn,
                    "gateway tool loop dispatch failed; returning last completion"
                );
                if let Some(t) = tracer.as_deref_mut() {
                    t.record(
                        &format!("boon:tool_loop:iter:{turn}"),
                        "boon:tool_loop",
                        iter_start,
                        (crate::tracer::now_ms() - iter_start) as u32,
                        "error",
                        serde_json::json!({
                            "tools": iter_tool_names,
                            "error": e.to_string(),
                        }),
                    );
                }
                if let Some(t) = tracer {
                    t.record_elapsed(
                        "boon:tool_loop",
                        "proxy_request",
                        tool_loop_start,
                        "error",
                        serde_json::json!({ "turns": completed_turns, "tools": all_tools_seen }),
                    );
                }
                let warning = finish_images(
                    image_ctx
                        .as_ref()
                        .map(|c| c.images.as_slice())
                        .unwrap_or(&[]),
                    plan.structured.is_some(),
                    body,
                    Some(failure_warning),
                );
                return TransformResult {
                    warning,
                    turn0_usage: replaced.then(|| turn0_or_estimate(state, loop_plan, turn0)),
                };
            }
        }
    }
    // Out of turns with the model still asking for tools. Force a final
    // answer from what was gathered: strip the tool definitions and ask the
    // model to conclude. A client that sent a plain chat request must never
    // receive a response carrying `tool_calls` it didn't ask for.
    tracing::warn!(
        model = %route.model_name,
        max_turns,
        "gateway tool loop hit its turn limit; forcing a final answer"
    );
    if let Some(obj) = request.as_object_mut() {
        obj.remove("tools");
        obj.remove("tool_choice");
    }
    push_message(
        &mut request,
        json!({
            "role": "user",
            "content": "Please answer the original question now using the information \
                        already gathered above. Do not call any more tools.",
        }),
    );
    let mut limit_warning = "tool_loop_turn_limit";
    let finalized = match deadline.bound(dispatch_timeout) {
        Some(timeout) => dispatch_follow_up(&state.http, &mut target, request, timeout).await,
        None => {
            limit_warning = DEADLINE_WARNING;
            Err(anyhow::anyhow!("tool loop time budget exhausted"))
        }
    };
    match finalized {
        Ok(completion) => {
            let (input_tokens, output_tokens) = reported_tokens(&completion);
            super::bill_helper_call(
                state,
                route,
                key,
                session_id,
                "tool_loop",
                input_tokens,
                output_tokens,
            );
            *body = completion;
            replaced = true;
        }
        Err(e) => {
            tracing::warn!(error = %e, "tool loop finalization dispatch failed");
        }
    }
    if let Some(t) = tracer {
        t.record_elapsed(
            "boon:tool_loop",
            "proxy_request",
            tool_loop_start,
            "ok",
            serde_json::json!({ "turns": max_turns, "tools": all_tools_seen }),
        );
    }
    let warning = finish_images(
        image_ctx
            .as_ref()
            .map(|c| c.images.as_slice())
            .unwrap_or(&[]),
        plan.structured.is_some(),
        body,
        Some(limit_warning),
    );
    TransformResult {
        warning,
        turn0_usage: replaced.then(|| turn0_or_estimate(state, loop_plan, turn0)),
    }
}

/// Helper-billing token counts: each field independently, zero when absent.
fn reported_tokens(completion: &Value) -> (u32, u32) {
    let read = |field: &str| {
        completion
            .pointer(&format!("/usage/{field}"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32
    };
    (read("prompt_tokens"), read("completion_tokens"))
}

/// Turn 0's reported usage, or an estimate of it when the upstream reported
/// none. Only called once the body was replaced, so the tokenizer pass is paid
/// only when it is actually needed.
fn turn0_or_estimate(
    state: &AppState,
    loop_plan: &ToolLoopPlan,
    reported: Option<(u32, u32)>,
) -> (u32, u32) {
    reported.unwrap_or_else(|| {
        let est = state.tokenizer.estimate_request(&loop_plan.request);
        (est.input_tokens, est.estimated_output_tokens)
    })
}

/// One non-streaming follow-up turn against the loop's pinned target. A
/// target that could not be resolved is reported once, with its cause chain.
async fn dispatch_follow_up(
    http: &reqwest::Client,
    target: &mut anyhow::Result<crate::proxy::Target>,
    request: Value,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let target = match target {
        Ok(target) => target,
        Err(e) => {
            let cause = std::mem::replace(e, anyhow::anyhow!("follow-up target unavailable"));
            return Err(cause.context("tool loop follow-up has no upstream target"));
        }
    };
    super::chat_call_completion_on(http, target, request, timeout).await
}

/// Execute call number `index` of one turn, honoring the per-turn call cap and
/// the loop's time budget. Both limits answer with a tool result the model can
/// read instead of failing the loop.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_one_call(
    state: &AppState,
    tenant: &uuid::Uuid,
    tool_servers: &HashMap<String, String>,
    sessions: &mut Sessions,
    image: Option<&mut super::image_gen::ImageCtx<'_>>,
    call: &PendingCall,
    index: usize,
    deadline: &LoopDeadline,
    tool_timeout: Duration,
) -> String {
    match call_timeout(&call.name, index, deadline, tool_timeout) {
        Ok(timeout) => {
            execute_call(
                state,
                tenant,
                tool_servers,
                sessions,
                image,
                call,
                timeout,
                deadline,
            )
            .await
        }
        Err(refusal) => refusal,
    }
}

/// The timeout for call number `index` of a turn, or the tool result to
/// return instead of running it.
fn call_timeout(
    name: &str,
    index: usize,
    deadline: &LoopDeadline,
    tool_timeout: Duration,
) -> Result<Duration, String> {
    if index >= obleth_config::TOOL_LOOP_MAX_CALLS_PER_TURN {
        return Err(over_call_cap_result(name));
    }
    deadline
        .bound(tool_timeout)
        .ok_or_else(|| deadline_result(name))
}

/// One tool call extracted from a completion.
pub(super) struct PendingCall {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) arguments: Value,
}

/// Read `message.tool_calls` from a completion (native or rewritten form).
fn extract_tool_calls(body: &Value) -> Vec<PendingCall> {
    let Some(calls) = body
        .pointer("/choices/0/message/tool_calls")
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    calls
        .iter()
        .filter_map(|call| {
            let name = call.pointer("/function/name")?.as_str()?.to_string();
            let id = call
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            // `arguments` is a JSON-encoded string on the wire; tolerate a
            // bare object too.
            let arguments = match call.pointer("/function/arguments") {
                Some(Value::String(s)) => serde_json::from_str(s).unwrap_or(json!({})),
                Some(Value::Object(o)) => Value::Object(o.clone()),
                _ => json!({}),
            };
            Some(PendingCall {
                id,
                name,
                arguments,
            })
        })
        .collect()
}

/// Execute one call against its MCP server, reusing the per-request session.
/// Errors become a text result the model can read and recover from (fail-open
/// inside the loop). `tenant` scopes `retrieve_original` to the caller's own
/// stashed originals.
#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_call(
    state: &AppState,
    tenant: &uuid::Uuid,
    tool_servers: &HashMap<String, String>,
    sessions: &mut Sessions,
    image: Option<&mut super::image_gen::ImageCtx<'_>>,
    call: &PendingCall,
    timeout: Duration,
    deadline: &LoopDeadline,
) -> String {
    // Gateway-executed compression tool: resolve from Redis, never an MCP server.
    if call.name == RETRIEVE_ORIGINAL_TOOL {
        let reference = call.arguments.get("ref").and_then(|v| v.as_str());
        let content = match reference {
            Some(r) => match state.redis.compress_get(tenant, r).await {
                Ok(found) => found,
                Err(e) => {
                    tracing::warn!(error = %e, "compress_get failed for retrieve_original");
                    None
                }
            },
            None => None,
        };
        return format_retrieve_result(reference, content);
    }

    // Gateway-executed image generation: a POST to the configured image model,
    // never an MCP server. The returned receipt is what the model reads; the
    // image itself lands in `ctx.images`.
    if call.name == super::image_gen::GENERATE_IMAGE_TOOL {
        let Some(ctx) = image else {
            return super::image_gen::failure_receipt("image generation is not configured");
        };
        let Some(image_timeout) = super::image_gen::call_timeout(ctx.cfg, deadline) else {
            return deadline_result(&call.name);
        };
        return super::image_gen::execute(state, ctx, &call.arguments, image_timeout).await;
    }

    let Some(server_name) = tool_servers.get(&call.name) else {
        return format!("Error: tool `{}` is not available.", call.name);
    };
    let Some(server) = crate::mcp::resolve_mcp(state, server_name).await else {
        return format!("Error: tool server `{server_name}` is unavailable.");
    };
    let session = match sessions
        .get_or_open(server_name, || {
            mcp_tools::open_session(state, &server, timeout)
        })
        .await
    {
        Ok(session) => session,
        Err(e) => {
            return format!("Error: could not reach tool server `{server_name}`: {e}");
        }
    };
    match mcp_tools::call_tool_in(state, session, &call.name, call.arguments.clone(), timeout).await
    {
        Ok(text) => {
            let mut text = text;
            if text.len() > TOOL_RESULT_MAX_CHARS {
                let mut end = TOOL_RESULT_MAX_CHARS;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                text.truncate(end);
                text.push_str("\n[truncated]");
            }
            text
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                tool = %call.name,
                server = %server_name,
                "gateway tool execution failed"
            );
            format!("Error executing tool `{}`: {e}", call.name)
        }
    }
}

pub(super) fn push_message(request: &mut Value, message: Value) {
    if let Some(messages) = request.get_mut("messages").and_then(|m| m.as_array_mut()) {
        messages.push(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};

    /// In `load_balance` mode endpoint selection is random, so only the
    /// recorded `served_url` keeps follow-up turns on the replica that served
    /// turn 0 (and holds its prefix cache).
    #[tokio::test]
    async fn buffered_follow_ups_pin_to_the_endpoint_that_served_turn_0() {
        let reply = json!({
            "choices": [{ "message": { "role": "assistant", "content": "ok" } }],
            "usage": { "prompt_tokens": 1, "completion_tokens": 1 }
        });
        let (base_a, hits_a) = crate::boons::tests::chat_server(reply.clone()).await;
        let (base_b, hits_b) = crate::boons::tests::chat_server(reply).await;
        let mut route = crate::boons::tests::endpoint_only_route(&base_a);
        route.endpoint_selection_mode = "load_balance".into();
        route.endpoints.push(obleth_config::ResolvedEndpoint {
            id: "e2".into(),
            api_base: base_b.clone(),
            api_key: None,
            priority: 0,
            weight: 1,
            enabled: true,
            healthy: true,
            max_in_flight: None,
        });
        let plan = ToolLoopPlan {
            tool_servers: HashMap::new(),
            request: json!({}),
            settings: ToolLoopSettings::default(),
            passthrough_unmapped: false,
            image_gen: None,
            served_url: Some(format!("{base_b}/chat/completions")),
        };
        let http = reqwest::Client::new();
        for _ in 0..20 {
            let mut target =
                crate::boons::helper_target(&route, "session", plan.served_url.as_deref());
            dispatch_follow_up(&http, &mut target, json!({}), Duration::from_secs(5))
                .await
                .unwrap();
        }
        assert_eq!(hits_b.lock().unwrap().len(), 20);
        assert!(hits_a.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_unresolvable_follow_up_target_keeps_its_cause() {
        let mut route = crate::boons::tests::endpoint_only_route("http://unused/v1");
        route.endpoints.clear();
        let mut target = crate::boons::helper_target(&route, "", None);
        let err = dispatch_follow_up(
            &reqwest::Client::new(),
            &mut target,
            json!({}),
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();
        let chain = format!("{err:#}");
        assert!(chain.contains("no upstream target"), "{chain}");
        assert!(chain.contains("no usable upstream endpoint"), "{chain}");
    }

    #[test]
    fn calls_past_the_per_turn_cap_get_an_error_result() {
        let deadline = LoopDeadline::after(Duration::from_secs(60));
        let cap = obleth_config::TOOL_LOOP_MAX_CALLS_PER_TURN;
        assert_eq!(cap, 16);
        assert_eq!(
            call_timeout("search", cap - 1, &deadline, Duration::from_secs(5)),
            Ok(Duration::from_secs(5))
        );
        let refused = call_timeout("search", cap, &deadline, Duration::from_secs(5))
            .expect_err("the 17th call is not executed");
        assert!(refused.starts_with("Error:") && refused.contains("search"));
    }

    #[test]
    fn the_loop_deadline_shortens_then_refuses_calls() {
        let deadline = LoopDeadline::after(Duration::from_millis(200));
        let bounded = deadline.bound(Duration::from_secs(30)).unwrap();
        assert!(bounded <= Duration::from_millis(200), "{bounded:?}");

        let spent = LoopDeadline::after(Duration::ZERO);
        assert_eq!(spent.bound(Duration::from_secs(30)), None);
        let refused = call_timeout("search", 0, &spent, Duration::from_secs(5)).unwrap_err();
        assert!(refused.contains("time limit"));
    }

    #[test]
    fn an_unrepresentable_deadline_does_not_panic() {
        let deadline = LoopDeadline::after(Duration::from_secs(u64::MAX));
        let bounded = deadline.bound(Duration::from_secs(u64::MAX)).unwrap();
        assert!(
            bounded <= Duration::from_secs(obleth_config::TOOL_LOOP_MAX_DEADLINE_SECS),
            "{bounded:?}"
        );
        let settings = ToolLoopSettings {
            deadline_secs: u64::MAX,
            ..ToolLoopSettings::default()
        };
        assert!(LoopDeadline::new(&settings)
            .bound(Duration::from_secs(1))
            .is_some());
    }

    #[test]
    fn new_clamps_a_persisted_over_limit_deadline() {
        // Settings JSON written before the admin-side clamp existed may still
        // hold something like 10^9; `new` must not hand that straight to
        // `after` unclamped.
        let settings = ToolLoopSettings {
            deadline_secs: 1_000_000_000,
            ..ToolLoopSettings::default()
        };
        let max = obleth_config::TOOL_LOOP_MAX_DEADLINE_SECS;
        let bounded = LoopDeadline::new(&settings)
            .bound(Duration::from_secs(max + 1_000_000_000))
            .unwrap();
        assert!(bounded <= Duration::from_secs(max), "{bounded:?}");

        let zero = ToolLoopSettings {
            deadline_secs: 0,
            ..ToolLoopSettings::default()
        };
        assert!(LoopDeadline::new(&zero)
            .bound(Duration::from_secs(1))
            .is_some());
    }

    #[test]
    fn the_deadline_setting_defaults_to_five_minutes() {
        let settings: ToolLoopSettings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(settings.deadline_secs, 300);
    }

    #[tokio::test]
    async fn a_failed_session_open_is_not_retried_within_the_request() {
        let opens = AtomicUsize::new(0);
        let mut sessions: Sessions<()> = Sessions::default();
        for _ in 0..3 {
            let err = sessions
                .get_or_open("dead", || async {
                    opens.fetch_add(1, Ordering::SeqCst);
                    anyhow::bail!("connection refused")
                })
                .await
                .expect_err("dead server");
            assert!(err.contains("connection refused"));
        }
        assert_eq!(opens.load(Ordering::SeqCst), 1);

        // A healthy server opens once and is reused.
        for _ in 0..3 {
            sessions
                .get_or_open("live", || async {
                    opens.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                })
                .await
                .unwrap();
        }
        assert_eq!(opens.load(Ordering::SeqCst), 2);
    }

    fn tool_cache(ttl: Duration) -> ToolCache {
        moka::future::Cache::builder().time_to_live(ttl).build()
    }

    fn one_tool() -> Vec<McpTool> {
        vec![McpTool {
            name: "search".into(),
            description: String::new(),
            input_schema: json!({}),
        }]
    }

    #[tokio::test]
    async fn concurrent_discovery_misses_share_one_fetch() {
        let cache = tool_cache(Duration::from_secs(600));
        let failures = tool_cache(MCP_DISCOVERY_NEGATIVE_TTL);
        let fetches = AtomicUsize::new(0);
        let lookups = (0..8).map(|_| {
            discover_coalesced(&cache, &failures, "srv", async {
                fetches.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                Ok(one_tool())
            })
        });
        let results = futures_util::future::join_all(lookups).await;
        assert!(results.iter().all(|r| r.as_ref().unwrap().len() == 1));
        assert_eq!(fetches.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_discovery_is_cached_as_an_empty_list() {
        let cache = tool_cache(Duration::from_secs(600));
        let failures = tool_cache(MCP_DISCOVERY_NEGATIVE_TTL);
        let fetches = AtomicUsize::new(0);
        let lookups = (0..4).map(|_| {
            discover_coalesced(&cache, &failures, "srv", async {
                fetches.fetch_add(1, Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(50)).await;
                anyhow::bail!("tools/list timed out")
            })
        });
        let results = futures_util::future::join_all(lookups).await;
        assert!(results.iter().all(|r| r.as_ref().unwrap().is_empty()));
        assert_eq!(
            fetches.load(Ordering::SeqCst),
            1,
            "misses coalesce on failure too"
        );
        assert!(
            failures.get("srv").await.is_some_and(|t| t.is_empty()),
            "the failure is remembered so the next request skips discovery"
        );
        assert!(
            cache.get("srv").await.is_none(),
            "a failure never enters the success cache"
        );
    }

    #[test]
    fn completion_usage_reads_like_the_settle_path() {
        assert_eq!(
            completion_usage(&json!({"usage": {"prompt_tokens": 9, "completion_tokens": 4}})),
            Some((9, 4))
        );
        assert_eq!(
            completion_usage(&json!({"usage": {"completion_tokens": 4}})),
            None
        );
        assert_eq!(
            reported_tokens(&json!({"usage": {"completion_tokens": 4}})),
            (0, 4)
        );
    }

    #[test]
    fn extracts_native_tool_calls() {
        let body = json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "searxng_web_search", "arguments": "{\"query\": \"x\"}" }
                    }]
                }
            }]
        });
        let calls = extract_tool_calls(&body);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "searxng_web_search");
        assert_eq!(calls[0].arguments, json!({"query": "x"}));
    }

    #[test]
    fn no_calls_for_plain_answer() {
        let body = json!({
            "choices": [{ "message": { "role": "assistant", "content": "hi" } }]
        });
        assert!(extract_tool_calls(&body).is_empty());
    }

    #[test]
    fn malformed_arguments_default_to_empty_object() {
        let body = json!({
            "choices": [{
                "message": {
                    "tool_calls": [{
                        "id": "c", "type": "function",
                        "function": { "name": "t", "arguments": "{broken" }
                    }]
                }
            }]
        });
        let calls = extract_tool_calls(&body);
        assert_eq!(calls[0].arguments, json!({}));
    }

    #[test]
    fn retrieve_original_tool_def_shape() {
        let def = retrieve_original_tool_def();
        assert_eq!(def["type"], "function");
        assert_eq!(def["function"]["name"], RETRIEVE_ORIGINAL_TOOL);
        assert_eq!(
            def["function"]["parameters"]["properties"]["ref"]["type"],
            "string"
        );
        assert_eq!(def["function"]["parameters"]["required"][0], "ref");
    }

    #[test]
    fn retrieve_original_missing_ref_is_clear_error() {
        // The pure formatter rejects a missing/blank ref without panicking.
        assert!(format_retrieve_result(None, None).contains("ref"));
    }

    #[test]
    fn retrieve_original_miss_is_not_an_error() {
        // A miss (ref known, content gone) reads as unavailable, not an error.
        let out = format_retrieve_result(Some("abc123"), None);
        assert!(out.to_lowercase().contains("no longer available"));
        assert!(out.contains("abc123"));
    }

    #[test]
    fn retrieve_original_hit_returns_content() {
        assert_eq!(
            format_retrieve_result(Some("abc123"), Some("the original".to_string())),
            "the original"
        );
    }

    use crate::boons::image_gen::GeneratedImage;

    fn one_image() -> Vec<GeneratedImage> {
        vec![GeneratedImage {
            url: "http://i/x.png".to_string(),
            size: "512x512".to_string(),
            prompt: "a cat".to_string(),
        }]
    }

    #[test]
    fn final_answer_gets_the_images_when_no_schema_is_armed() {
        let mut body = json!({
            "choices": [{ "message": { "role": "assistant", "content": "here you go" } }]
        });
        let warning = finish_images(&one_image(), false, &mut body, None);
        assert_eq!(warning, None);
        let content = body["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(content.contains("![a cat](http://i/x.png)"));
    }

    #[test]
    fn an_armed_schema_suppresses_attachment_and_warns() {
        // Appending markdown to schema-validated JSON would produce output that
        // is neither valid JSON nor a rendered image. The schema wins; the
        // suppression is reported.
        let mut body = json!({
            "choices": [{ "message": { "role": "assistant", "content": "{\"ok\":true}" } }]
        });
        let warning = finish_images(&one_image(), true, &mut body, None);
        assert_eq!(warning, Some(IMAGE_SUPPRESSED_WARNING));
        assert_eq!(
            body["choices"][0]["message"]["content"], "{\"ok\":true}",
            "the validated JSON must be left byte-identical"
        );
    }

    #[test]
    fn a_structured_validation_warning_is_not_overwritten() {
        let mut body = json!({
            "choices": [{ "message": { "role": "assistant", "content": "not json" } }]
        });
        let warning = finish_images(&one_image(), true, &mut body, Some("structured_invalid"));
        assert_eq!(
            warning,
            Some("structured_invalid"),
            "a real validation failure is more important than the suppression note"
        );
    }

    #[test]
    fn no_images_means_the_completion_is_untouched() {
        let mut body = json!({
            "choices": [{ "message": { "role": "assistant", "content": "plain answer" } }]
        });
        let before = body.clone();
        assert_eq!(finish_images(&[], false, &mut body, None), None);
        assert_eq!(
            body, before,
            "a request that never called the tool is byte-identical"
        );
        // ...and an existing warning survives.
        assert_eq!(
            finish_images(&[], false, &mut body, Some("tool_loop_turn_limit")),
            Some("tool_loop_turn_limit")
        );
    }

    #[test]
    fn a_dispatch_failure_still_attaches_already_billed_images() {
        // An image generated in a prior turn was already billed the moment the
        // upstream call succeeded; if the *next* turn's model dispatch then
        // fails, the image must still reach the client instead of being
        // silently dropped while the tenant is charged for it.
        let mut body = json!({
            "choices": [{ "message": { "role": "assistant", "content": "here you go" } }]
        });
        let warning = finish_images(
            &one_image(),
            false,
            &mut body,
            Some("tool_loop_dispatch_failed"),
        );
        assert_eq!(
            warning,
            Some("tool_loop_dispatch_failed"),
            "the dispatch-failure warning must survive unchanged"
        );
        let content = body["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(content.contains("![a cat](http://i/x.png)"));
    }

    #[test]
    fn a_malformed_completion_warns_instead_of_silently_dropping_billed_images() {
        // No `/choices/0/message` pointer: `attach_to_completion` is a no-op,
        // but the images were already billed the moment generation succeeded.
        // That must never vanish with zero signal.
        let mut body = json!({ "error": "upstream exploded" });
        let before = body.clone();
        let warning = finish_images(&one_image(), false, &mut body, None);
        assert_eq!(warning, Some(IMAGE_ATTACH_FAILED_WARNING));
        assert_eq!(
            body, before,
            "a completion attach failed must leave the body untouched, not corrupt it further"
        );
    }

    #[test]
    fn a_malformed_completion_does_not_overwrite_a_more_specific_warning() {
        let mut body = json!({ "error": "upstream exploded" });
        let warning = finish_images(
            &one_image(),
            false,
            &mut body,
            Some("tool_loop_dispatch_failed"),
        );
        assert_eq!(
            warning,
            Some("tool_loop_dispatch_failed"),
            "a pre-existing, more specific warning must win over the attach-failed note"
        );
    }
}
