//! Anthropic Messages API surface, served by translating to Chat Completions
//! and back around the unchanged pipeline — the same shape as `responses`.
//! Both directions are pure functions on `serde_json::Value`: routing,
//! fairshare, the tool loop and every boon read `messages` and write
//! `choices`, so the translation must stay at the edge.

use serde_json::{json, Map, Value};

pub(crate) const MESSAGES_PATH: &str = "/v1/messages";
pub(crate) const COUNT_TOKENS_PATH: &str = "/v1/messages/count_tokens";
/// Value for `crate::responses::SURFACE_HEADER` marking a translated request.
pub(crate) const SURFACE: &str = "messages";

/// A request the shim refuses before it reaches the pipeline; rendered with
/// the Anthropic error envelope and status 400.
#[derive(Debug, PartialEq)]
pub(crate) struct RequestError {
    pub error_type: &'static str,
    pub message: String,
}

fn invalid(message: impl Into<String>) -> RequestError {
    RequestError {
        error_type: "invalid_request_error",
        message: message.into(),
    }
}

/// Anthropic request → Chat Completions request. Pure; never logs content.
pub(crate) fn to_chat_request(incoming: &Value) -> Result<Value, RequestError> {
    let obj = incoming
        .as_object()
        .ok_or_else(|| invalid("request body must be a JSON object"))?;
    let max_tokens = obj
        .get("max_tokens")
        .and_then(Value::as_u64)
        .filter(|&n| n >= 1)
        .ok_or_else(|| invalid("`max_tokens` is required and must be a positive integer"))?;
    let in_messages = obj
        .get("messages")
        .and_then(Value::as_array)
        .filter(|m| !m.is_empty())
        .ok_or_else(|| invalid("`messages` must be a non-empty array"))?;

    let mut out = Map::new();
    let mut messages: Vec<Value> = Vec::new();
    if let Some(system) = obj.get("system") {
        let text = system_text(system);
        if !text.trim().is_empty() {
            messages.push(json!({"role": "system", "content": text}));
        }
    }
    for m in in_messages {
        convert_message(m, &mut messages)?;
    }
    // Chat templates (vLLM's included) accept a system message only at the
    // start, while Claude Code interleaves system-role reminders through the
    // conversation. All system text is gathered, in order, into one leading
    // system message so the upstream never sees one mid-conversation.
    let (system_parts, rest): (Vec<Value>, Vec<Value>) = messages
        .into_iter()
        .partition(|m| m.get("role").and_then(Value::as_str) == Some("system"));
    let mut messages = rest;
    if !system_parts.is_empty() {
        let text = system_parts
            .iter()
            .filter_map(|m| m.get("content").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n");
        messages.insert(0, json!({"role": "system", "content": text}));
    }
    out.insert("messages".into(), Value::Array(messages));
    out.insert("max_tokens".into(), json!(max_tokens));

    for (k, v) in obj {
        match k.as_str() {
            "messages" | "max_tokens" | "system" | "top_k" | "metadata" | "thinking"
            | "service_tier" | "cache_control" | "container" | "mcp_servers" | "stop_sequences"
            | "tools" | "tool_choice" => {}
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    if let Some(stop) = obj.get("stop_sequences") {
        out.insert("stop".into(), stop.clone());
    }
    if let Some(user) = incoming
        .pointer("/metadata/user_id")
        .and_then(Value::as_str)
    {
        out.insert("user".into(), json!(user));
    }
    if let Some(tools) = obj.get("tools").and_then(Value::as_array) {
        let converted: Vec<Value> = tools.iter().filter_map(convert_tool).collect();
        if !converted.is_empty() {
            out.insert("tools".into(), Value::Array(converted));
        }
    }
    if let Some(choice) = obj.get("tool_choice") {
        if let Some(tc) = convert_tool_choice(choice) {
            out.insert("tool_choice".into(), tc);
        }
        if choice
            .get("disable_parallel_tool_use")
            .and_then(Value::as_bool)
            == Some(true)
        {
            out.insert("parallel_tool_calls".into(), json!(false));
        }
    }
    // `stream` itself already rode through in the passthrough loop above,
    // true, false or absent, exactly as the caller sent it.
    if obj.get("stream").and_then(Value::as_bool) == Some(true) {
        // Usage arrives on the final chunk only when asked for; the translator
        // needs it for `message_delta`.
        out.insert("stream_options".into(), json!({"include_usage": true}));
    }
    Ok(Value::Object(out))
}

/// Anthropic's `system` is a string or a list of cache-controlled text
/// blocks; chat's system message takes one string, so multiple blocks are
/// joined as separate paragraphs would read.
fn system_text(system: &Value) -> String {
    match system {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => String::new(),
    }
}

/// One Anthropic message → one or more chat messages, appended in order.
/// A user turn that carries `tool_result` blocks yields the `tool` messages
/// first, then a `user` message with whatever text remains.
fn convert_message(m: &Value, out: &mut Vec<Value>) -> Result<(), RequestError> {
    let role = m
        .get("role")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("each message needs a `role`"))?;
    let content = m
        .get("content")
        .ok_or_else(|| invalid("each message needs `content`"))?;
    // Claude Code puts system-role entries inside `messages` as well as the
    // top-level `system`; chat completions accepts a system message anywhere,
    // so it is forwarded as one rather than refused.
    if role == "system" {
        let text = system_text(content);
        if !text.trim().is_empty() {
            out.push(json!({"role": "system", "content": text}));
        }
        return Ok(());
    }
    if role != "user" && role != "assistant" {
        return Err(invalid(format!("unsupported message role `{role}`")));
    }
    if let Value::String(s) = content {
        out.push(json!({"role": role, "content": s}));
        return Ok(());
    }
    let blocks = content
        .as_array()
        .ok_or_else(|| invalid("`content` must be a string or an array of blocks"))?;

    let mut parts: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut text_only = true;
    for b in blocks {
        match b.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(t) = b.get("text").and_then(Value::as_str) {
                    parts.push(json!({"type": "text", "text": t}));
                }
            }
            Some("image") => {
                text_only = false;
                if let Some(url) = image_url(b) {
                    parts.push(json!({"type": "image_url", "image_url": {"url": url}}));
                }
            }
            Some("tool_use") if role == "assistant" => {
                let id = b.get("id").and_then(Value::as_str).unwrap_or_default();
                let name = b.get("name").and_then(Value::as_str).unwrap_or_default();
                let input = b.get("input").cloned().unwrap_or_else(|| json!({}));
                tool_calls.push(json!({
                    "id": id, "type": "function",
                    "function": {"name": name, "arguments": input.to_string()}
                }));
            }
            Some("tool_result") if role == "user" => {
                let id = b
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let mut text = tool_result_text(b.get("content"));
                if b.get("is_error").and_then(Value::as_bool) == Some(true) {
                    text = format!("Error: {text}");
                }
                out.push(json!({"role": "tool", "tool_call_id": id, "content": text}));
            }
            // Thinking blocks are the model's, not the client's, and the
            // upstream cannot verify a signature it never issued.
            Some("thinking") | Some("redacted_thinking") => {}
            Some(other) => {
                return Err(invalid(format!("unsupported content block type `{other}`")))
            }
            None => return Err(invalid("content block missing `type`")),
        }
    }
    let has_parts = !parts.is_empty();
    if !has_parts && tool_calls.is_empty() {
        return Ok(());
    }
    let content_value = if text_only {
        Value::String(
            parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n"),
        )
    } else {
        Value::Array(parts)
    };
    let mut msg = json!({"role": role, "content": content_value});
    if !tool_calls.is_empty() {
        msg["tool_calls"] = Value::Array(tool_calls);
        if !has_parts {
            msg["content"] = Value::Null;
        }
    }
    out.push(msg);
    Ok(())
}

/// Chat's `image_url` is always a URL string; Anthropic's base64 source
/// becomes a `data:` URI so nothing has to fetch or re-host the image.
fn image_url(block: &Value) -> Option<String> {
    let source = block.get("source")?;
    match source.get("type").and_then(Value::as_str)? {
        "base64" => {
            let media = source.get("media_type").and_then(Value::as_str)?;
            let data = source.get("data").and_then(Value::as_str)?;
            Some(format!("data:{media};base64,{data}"))
        }
        "url" => source
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string),
        _ => None,
    }
}

/// Text of a `tool_result`: a string, or its text blocks joined. Image
/// blocks are dropped — the chat `tool` role carries text only.
fn tool_result_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(blocks)) => {
            let dropped = blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("image"))
                .count();
            if dropped > 0 {
                tracing::warn!(
                    dropped,
                    "tool_result image blocks dropped: chat tool messages carry text only"
                );
            }
            blocks
                .iter()
                .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|b| b.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n")
        }
        _ => String::new(),
    }
}

/// Anthropic flattens a tool's name, description and schema onto the tool
/// itself; chat nests the same three fields under `function`.
fn convert_tool(tool: &Value) -> Option<Value> {
    let name = tool.get("name").and_then(Value::as_str)?;
    let mut function = json!({"name": name});
    if let Some(d) = tool.get("description") {
        function["description"] = d.clone();
    }
    if let Some(schema) = tool.get("input_schema") {
        function["parameters"] = schema.clone();
    }
    Some(json!({"type": "function", "function": function}))
}

/// Anthropic's `tool_choice` is a typed object; chat's is the bare strings
/// `"auto"`/`"required"`/`"none"`, or an object only to force one named tool.
fn convert_tool_choice(choice: &Value) -> Option<Value> {
    match choice.get("type").and_then(Value::as_str)? {
        "auto" => Some(json!("auto")),
        "any" => Some(json!("required")),
        "none" => Some(json!("none")),
        "tool" => choice
            .get("name")
            .and_then(Value::as_str)
            .map(|n| json!({"type": "function", "function": {"name": n}})),
        _ => None,
    }
}

/// Identifies the reply being translated: which request it answers and which
/// model name to echo, since the pipeline may have served an alias or the
/// configured default rather than the exact name the caller sent.
pub(crate) struct ResponseContext {
    pub request_id: String,
    /// The name the client sent, echoed back; the pipeline may have served an
    /// alias or the configured default.
    pub model: String,
}

/// `length` wins over an in-flight tool call: a reply cut off by
/// `max_tokens` mid-arguments must not be reported as `tool_use`, or a
/// client tries to run a call whose JSON was truncated. The real Anthropic
/// API reports `max_tokens` for exactly this case.
pub(crate) fn stop_reason(finish_reason: Option<&str>, has_tool_calls: bool) -> &'static str {
    if finish_reason == Some("length") {
        return "max_tokens";
    }
    if has_tool_calls {
        return "tool_use";
    }
    match finish_reason {
        Some("tool_calls") => "tool_use",
        _ => "end_turn",
    }
}

/// Chat Completions reply → Anthropic `message` object. Never logs content.
pub(crate) fn from_chat_response(chat: &Value, ctx: &ResponseContext) -> Value {
    let choice = chat.pointer("/choices/0").cloned().unwrap_or(Value::Null);
    let message = choice.get("message").cloned().unwrap_or(Value::Null);
    let mut content: Vec<Value> = Vec::new();
    // Both spellings: LiteLLM normalises to `reasoning_content`, a vLLM
    // server straight behind the gateway sends `reasoning`.
    if let Some(r) = message
        .get("reasoning_content")
        .or_else(|| message.get("reasoning"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        content.push(json!({"type": "thinking", "thinking": r, "signature": ""}));
    }
    if let Some(t) = message
        .get("content")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
        content.push(json!({"type": "text", "text": t}));
    }
    let tool_calls = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for call in &tool_calls {
        content.push(tool_use_block(call));
    }
    let usage = chat.get("usage").cloned().unwrap_or(Value::Null);
    json!({
        "id": format!("msg_{}", ctx.request_id),
        "type": "message",
        "role": "assistant",
        "model": ctx.model,
        "content": content,
        "stop_reason": stop_reason(choice.get("finish_reason").and_then(Value::as_str), !tool_calls.is_empty()),
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": usage.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
            "output_tokens": usage.get("completion_tokens").and_then(Value::as_u64).unwrap_or(0),
        }
    })
}

/// `tool_calls[i]` → `tool_use` block. Arguments that are not valid JSON
/// become an empty object: a client can still see the call was made.
pub(crate) fn tool_use_block(call: &Value) -> Value {
    let id = call.get("id").and_then(Value::as_str).unwrap_or_default();
    let name = call
        .pointer("/function/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let input = call
        .pointer("/function/arguments")
        .and_then(Value::as_str)
        .and_then(|a| serde_json::from_str::<Value>(a).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| {
            tracing::warn!(
                tool = name,
                "tool call arguments were not a JSON object; sending empty input"
            );
            json!({})
        });
    json!({"type": "tool_use", "id": id, "name": name, "input": input})
}

/// Anthropic error `type` for a status this shim is about to answer with.
pub(crate) fn error_type_for(status: http::StatusCode) -> &'static str {
    use http::StatusCode as S;
    match status {
        S::BAD_REQUEST => "invalid_request_error",
        S::UNAUTHORIZED => "authentication_error",
        S::FORBIDDEN => "permission_error",
        S::NOT_FOUND => "not_found_error",
        S::PAYLOAD_TOO_LARGE => "request_too_large",
        S::TOO_MANY_REQUESTS => "rate_limit_error",
        S::BAD_GATEWAY | S::SERVICE_UNAVAILABLE | S::GATEWAY_TIMEOUT => "overloaded_error",
        _ => "api_error",
    }
}

pub(crate) fn error_envelope(error_type: &str, message: &str) -> Value {
    json!({"type": "error", "error": {"type": error_type, "message": message}})
}

/// The pipeline's `error.message` when the body is the OpenAI envelope;
/// otherwise the raw body, cut to 512 bytes on a char boundary. Never logs
/// the body itself.
pub(crate) fn upstream_error_message(body: &[u8]) -> String {
    if let Ok(v) = serde_json::from_slice::<Value>(body) {
        if let Some(m) = v.pointer("/error/message").and_then(Value::as_str) {
            return m.to_string();
        }
    }
    let s = String::from_utf8_lossy(body);
    let mut end = s.len().min(512);
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s[..end].to_string()
}

/// Translates a Chat Completions SSE stream into Anthropic Messages events.
///
/// Tool-call argument fragments are forwarded as `input_json_delta` as they
/// arrive rather than assembled at the end: Anthropic clients render the
/// call incrementally and time out on a silent stream. Every block gets a
/// `content_block_stop` before the next opens or the message ends, and at
/// most one tool-use block is ever open at a time — upstreams send tool
/// calls sequentially, and Anthropic's own clients key their per-block UI
/// state to that assumption.
pub(crate) struct AnthropicStreamTranslator {
    ctx: ResponseContext,
    started: bool,
    done: bool,
    next_index: usize,
    /// The text or thinking block currently open, if any.
    open: Option<OpenBlock>,
    /// Tool calls in first-seen order. A slot's `content_index` is `None`
    /// until its `name` is known, so fragments that arrive before the name
    /// does are buffered in `pending_args` rather than opening a block with
    /// an empty name Anthropic has no way to amend later.
    tools: Vec<ToolSlot>,
    /// Maps an upstream `tool_calls[].index` to its slot, for upstreams that
    /// send one.
    tool_index_slots: std::collections::HashMap<u64, usize>,
    /// The slot currently open (started, not yet stopped), if any. At most
    /// one at a time: opening a new tool key closes it first.
    open_tool_slot: Option<usize>,
    input_tokens: Option<u64>,
    output_tokens: Option<u64>,
    finish_reason: Option<String>,
    saw_tool_call: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum BlockKind {
    Text,
    Thinking,
}

struct OpenBlock {
    kind: BlockKind,
    index: usize,
}

#[derive(Default)]
struct ToolSlot {
    id: Option<String>,
    name: Option<String>,
    /// Argument text seen before `content_index` was assigned, flushed as
    /// one delta the moment the block opens.
    pending_args: String,
    /// Set once `content_block_start` has been emitted for this slot.
    content_index: Option<usize>,
    closed: bool,
}

impl AnthropicStreamTranslator {
    pub(crate) fn new(ctx: ResponseContext) -> Self {
        Self {
            ctx,
            started: false,
            done: false,
            next_index: 0,
            open: None,
            tools: Vec::new(),
            tool_index_slots: Default::default(),
            open_tool_slot: None,
            input_tokens: None,
            output_tokens: None,
            finish_reason: None,
            saw_tool_call: false,
        }
    }

    fn frame(kind: &str, data: Value) -> String {
        format!("event: {kind}\ndata: {data}\n\n")
    }

    fn start(&mut self, out: &mut Vec<String>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(Self::frame(
            "message_start",
            json!({
                "type": "message_start",
                "message": {
                    "id": format!("msg_{}", self.ctx.request_id),
                    "type": "message", "role": "assistant", "model": self.ctx.model,
                    "content": [], "stop_reason": Value::Null, "stop_sequence": Value::Null,
                    "usage": {"input_tokens": self.input_tokens.unwrap_or(0), "output_tokens": 0}
                }
            }),
        ));
    }

    fn close_open(&mut self, out: &mut Vec<String>) {
        if let Some(b) = self.open.take() {
            out.push(Self::frame(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": b.index}),
            ));
        }
    }

    /// Close whichever tool block is currently open (started, not yet
    /// stopped). A slot still waiting on its `name` is left alone — it is
    /// only resolved at `finish`/`fail` (see `finalize_tools`), since there
    /// is nothing to open yet.
    fn close_current_tool(&mut self, out: &mut Vec<String>) {
        if let Some(slot) = self.open_tool_slot.take() {
            if !self.tools[slot].closed {
                self.tools[slot].closed = true;
                let index = self.tools[slot]
                    .content_index
                    .expect("an open tool slot always has a content index");
                out.push(Self::frame(
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": index}),
                ));
            }
        }
    }

    /// Whatever else is open — the text/thinking block, or the one open
    /// tool-use block — is done: a new block is about to take its place.
    fn close_other_blocks(&mut self, out: &mut Vec<String>) {
        self.close_open(out);
        self.close_current_tool(out);
    }

    fn ensure_open(&mut self, kind: BlockKind, out: &mut Vec<String>) -> usize {
        if let Some(b) = &self.open {
            if b.kind == kind {
                return b.index;
            }
        }
        self.close_other_blocks(out);
        let index = self.next_index;
        self.next_index += 1;
        let block = match kind {
            BlockKind::Text => json!({"type": "text", "text": ""}),
            BlockKind::Thinking => json!({"type": "thinking", "thinking": "", "signature": ""}),
        };
        out.push(Self::frame(
            "content_block_start",
            json!({"type": "content_block_start", "index": index, "content_block": block}),
        ));
        self.open = Some(OpenBlock { kind, index });
        index
    }

    /// Match a `tool_calls[]` fragment to its slot, returning the slot and
    /// whether it was just created. Most upstreams send `index`; ones that
    /// omit it (older Ollama, some llama.cpp builds) send each call whole
    /// with only `id`, so a fragment with a new, non-empty `id` starts a new
    /// slot, and one with neither `index` nor `id` continues whatever was
    /// most recently opened.
    fn resolve_tool_slot(&mut self, call: &Value) -> (usize, bool) {
        if let Some(idx) = call.get("index").and_then(Value::as_u64) {
            if let Some(&slot) = self.tool_index_slots.get(&idx) {
                return (slot, false);
            }
            let slot = self.tools.len();
            self.tools.push(ToolSlot::default());
            self.tool_index_slots.insert(idx, slot);
            return (slot, true);
        }
        if let Some(id) = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            if self
                .tools
                .last()
                .is_some_and(|t| t.id.as_deref() == Some(id))
            {
                return (self.tools.len() - 1, false);
            }
            let slot = self.tools.len();
            self.tools.push(ToolSlot::default());
            return (slot, true);
        }
        if self.tools.is_empty() {
            self.tools.push(ToolSlot::default());
            return (0, true);
        }
        (self.tools.len() - 1, false)
    }

    /// Feed one parsed `chat.completion.chunk`. Never logs its content.
    pub(crate) fn on_chunk(&mut self, chunk: &Value) -> Vec<String> {
        if self.done {
            return Vec::new();
        }
        // The pipeline's mid-stream error frame carries no `choices`; it
        // ends the message the same way a broken upstream connection would.
        if let Some(err) = chunk.get("error").filter(|e| e.is_object()) {
            let message = err
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("upstream error");
            return self.fail("api_error", message);
        }
        let mut out = Vec::new();
        if let Some(u) = chunk.get("usage").filter(|u| u.is_object()) {
            if let Some(p) = u.get("prompt_tokens").and_then(Value::as_u64) {
                self.input_tokens = Some(p);
            }
            if let Some(c) = u.get("completion_tokens").and_then(Value::as_u64) {
                self.output_tokens = Some(c);
            }
        }
        let choice = chunk.pointer("/choices/0");
        // A usage-only chunk carries no choice; it may also be the first thing
        // we see (a request the cache answered), so start only on a choice.
        let Some(choice) = choice else {
            return out;
        };
        self.start(&mut out);
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
        // Both spellings: LiteLLM normalises to `reasoning_content`, a vLLM
        // server straight behind the gateway sends `reasoning`.
        let reasoning = delta
            .get("reasoning_content")
            .or_else(|| delta.get("reasoning"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        if let Some(r) = reasoning {
            let index = self.ensure_open(BlockKind::Thinking, &mut out);
            out.push(Self::frame(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": index, "delta": {"type": "thinking_delta", "thinking": r}}),
            ));
        }
        if let Some(t) = delta
            .get("content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            let index = self.ensure_open(BlockKind::Text, &mut out);
            out.push(Self::frame(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": t}}),
            ));
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                self.saw_tool_call = true;
                self.absorb_tool_call(call, &mut out);
            }
        }
        if let Some(f) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish_reason = Some(f.to_string());
        }
        out
    }

    /// Route one `tool_calls[]` fragment to its slot: open a new slot's
    /// block once its `name` is known (flushing anything buffered first),
    /// buffer its arguments while the name is still unknown, or forward the
    /// delta directly once the block is open.
    fn absorb_tool_call(&mut self, call: &Value, out: &mut Vec<String>) {
        let (slot, is_new) = self.resolve_tool_slot(call);
        if is_new {
            // A new tool call begins: text/thinking and any prior tool
            // block are done.
            self.close_other_blocks(out);
        }
        // Merged from whichever fragment carries them, like
        // `responses.rs::absorb_tool_calls` — a name that arrives after the
        // first fragment still reaches the block, since it has not opened
        // until the name is known.
        if let Some(id) = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            self.tools[slot].id = Some(id.to_string());
        }
        if let Some(name) = call
            .pointer("/function/name")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            self.tools[slot].name = Some(name.to_string());
        }
        let args = call
            .pointer("/function/arguments")
            .and_then(Value::as_str)
            .unwrap_or("");

        if let Some(index) = self.tools[slot].content_index {
            if self.tools[slot].closed && !args.is_empty() {
                // An interleaving upstream (not the ordinary sequential
                // case): the block already got its `content_block_stop`.
                // Forward the fragment anyway rather than drop it — never
                // logging the argument text itself.
                tracing::debug!(
                    content_index = index,
                    "tool argument fragment arrived after its block closed; forwarding anyway"
                );
            }
            if !args.is_empty() {
                out.push(Self::frame(
                    "content_block_delta",
                    json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": args}}),
                ));
            }
            return;
        }

        let Some(name) = self.tools[slot].name.clone() else {
            self.tools[slot].pending_args.push_str(args);
            return;
        };
        let index = self.next_index;
        self.next_index += 1;
        let id = self.tools[slot]
            .id
            .clone()
            .unwrap_or_else(|| format!("toolu_{}_{}", self.ctx.request_id, index));
        self.tools[slot].id = Some(id.clone());
        let block = json!({"type": "tool_use", "id": id, "name": name, "input": {}});
        out.push(Self::frame(
            "content_block_start",
            json!({"type": "content_block_start", "index": index, "content_block": block}),
        ));
        self.tools[slot].content_index = Some(index);
        self.open_tool_slot = Some(slot);
        let mut combined = std::mem::take(&mut self.tools[slot].pending_args);
        combined.push_str(args);
        if !combined.is_empty() {
            out.push(Self::frame(
                "content_block_delta",
                json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": combined}}),
            ));
        }
    }

    /// Open (with whatever is known) and immediately close any tool slot
    /// that never got its `name` in time, and close whichever slot is still
    /// open, so `finish`/`fail` never silently drops a buffered fragment.
    fn finalize_tools(&mut self, out: &mut Vec<String>) {
        for slot in 0..self.tools.len() {
            if self.tools[slot].closed {
                continue;
            }
            if self.tools[slot].content_index.is_none() {
                let index = self.next_index;
                self.next_index += 1;
                let name = self.tools[slot].name.clone().unwrap_or_default();
                let id = self.tools[slot]
                    .id
                    .clone()
                    .unwrap_or_else(|| format!("toolu_{}_{}", self.ctx.request_id, index));
                self.tools[slot].id = Some(id.clone());
                let block = json!({"type": "tool_use", "id": id, "name": name, "input": {}});
                out.push(Self::frame(
                    "content_block_start",
                    json!({"type": "content_block_start", "index": index, "content_block": block}),
                ));
                self.tools[slot].content_index = Some(index);
                let pending = std::mem::take(&mut self.tools[slot].pending_args);
                if !pending.is_empty() {
                    out.push(Self::frame(
                        "content_block_delta",
                        json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": pending}}),
                    ));
                }
            }
            let index = self.tools[slot]
                .content_index
                .expect("opened above if it was missing");
            self.tools[slot].closed = true;
            out.push(Self::frame(
                "content_block_stop",
                json!({"type": "content_block_stop", "index": index}),
            ));
        }
        self.open_tool_slot = None;
    }

    /// Keeps the connection alive while nothing else is streaming: opens the
    /// message on its first call if no chunk has arrived yet, so the client
    /// sees something before its own idle timeout fires. This translator, and
    /// so this method, exists only once the pipeline has already returned a
    /// streaming response — it covers gaps between upstream chunks (a slow
    /// decode step), not an admission queue wait or buffered-boon work, both
    /// of which happen earlier, before a caller ever gets here.
    pub(crate) fn ping(&mut self) -> Vec<String> {
        if self.done {
            return Vec::new();
        }
        let mut out = Vec::new();
        self.start(&mut out);
        out.push(Self::frame("ping", json!({"type": "ping"})));
        out
    }

    pub(crate) fn finish(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        if self.done {
            return out;
        }
        self.done = true;
        self.start(&mut out);
        self.close_open(&mut out);
        self.finalize_tools(&mut out);
        let mut usage = json!({"output_tokens": self.output_tokens.unwrap_or(0)});
        if let Some(p) = self.input_tokens {
            usage["input_tokens"] = json!(p);
        }
        out.push(Self::frame(
            "message_delta",
            json!({
                "type": "message_delta",
                "delta": {"stop_reason": stop_reason(self.finish_reason.as_deref(), self.saw_tool_call), "stop_sequence": Value::Null},
                "usage": usage
            }),
        ));
        out.push(Self::frame("message_stop", json!({"type": "message_stop"})));
        out
    }

    /// Close a stream whose upstream broke off before it finished. Any open
    /// block is closed first so the client's UI is left consistent, then a
    /// terminal `error` frame is emitted instead of `message_delta`/`message_stop`.
    pub(crate) fn fail(&mut self, error_type: &str, message: &str) -> Vec<String> {
        let mut out = Vec::new();
        if self.done {
            return out;
        }
        self.done = true;
        self.close_open(&mut out);
        self.finalize_tools(&mut out);
        out.push(Self::frame("error", error_envelope(error_type, message)));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(extra: Value) -> Value {
        let mut base = json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 256,
            "messages": [{"role": "user", "content": "hi"}]
        });
        if let (Some(b), Some(e)) = (base.as_object_mut(), extra.as_object()) {
            for (k, v) in e {
                b.insert(k.clone(), v.clone());
            }
        }
        base
    }

    #[test]
    fn system_role_inside_messages_is_forwarded_as_a_system_message() {
        let out = to_chat_request(&req(json!({"messages": [
            {"role": "system", "content": [{"type": "text", "text": "be terse", "cache_control": {"type": "ephemeral"}}]},
            {"role": "user", "content": "hi"}
        ]})))
        .unwrap();
        assert_eq!(
            out["messages"][0],
            json!({"role": "system", "content": "be terse"})
        );
        assert_eq!(out["messages"][1]["role"], "user");
        let other = to_chat_request(&req(
            json!({"messages": [{"role": "tool", "content": "x"}]}),
        ));
        assert!(other.is_err());
    }

    #[test]
    fn mid_conversation_system_entries_are_hoisted_into_one_leading_system_message() {
        let out = to_chat_request(&req(json!({
            "system": "top",
            "messages": [
                {"role": "user", "content": "one"},
                {"role": "system", "content": "reminder"},
                {"role": "assistant", "content": "two"},
                {"role": "user", "content": "three"}
            ]
        })))
        .unwrap();
        let m = out["messages"].as_array().unwrap();
        assert_eq!(m.len(), 4);
        assert_eq!(
            m[0],
            json!({"role": "system", "content": "top\n\nreminder"})
        );
        assert_eq!(m[1]["content"], "one");
        assert_eq!(m[2]["content"], "two");
        assert_eq!(m[3]["content"], "three");
        assert!(m[1..].iter().all(|x| x["role"] != "system"));
    }

    #[test]
    fn string_system_becomes_leading_system_message() {
        let out = to_chat_request(&req(json!({"system": "be terse"}))).unwrap();
        assert_eq!(
            out["messages"][0],
            json!({"role": "system", "content": "be terse"})
        );
        assert_eq!(out["messages"][1]["content"], "hi");
        assert_eq!(out["max_tokens"], 256);
    }

    #[test]
    fn block_list_system_is_joined() {
        let out = to_chat_request(&req(json!({"system": [
            {"type": "text", "text": "a", "cache_control": {"type": "ephemeral"}},
            {"type": "text", "text": "b"}
        ]})))
        .unwrap();
        assert_eq!(out["messages"][0]["content"], "a\n\nb");
    }

    #[test]
    fn max_tokens_is_required() {
        let mut r = req(json!({}));
        r.as_object_mut().unwrap().remove("max_tokens");
        let err = to_chat_request(&r).unwrap_err();
        assert_eq!(err.error_type, "invalid_request_error");
        assert!(err.message.contains("max_tokens"));

        let err = to_chat_request(&req(json!({"max_tokens": 0}))).unwrap_err();
        assert_eq!(err.error_type, "invalid_request_error");
        assert!(err.message.contains("max_tokens"));
    }

    #[test]
    fn messages_must_be_non_empty() {
        let err = to_chat_request(&req(json!({"messages": []}))).unwrap_err();
        assert!(err.message.contains("messages"));
    }

    #[test]
    fn images_become_image_url_parts() {
        let out = to_chat_request(&req(json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "what is this"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}},
            {"type": "image", "source": {"type": "url", "url": "https://example.com/x.png"}}
        ]}]}))).unwrap();
        let parts = out["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0], json!({"type": "text", "text": "what is this"}));
        assert_eq!(
            parts[1],
            json!({"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}})
        );
        assert_eq!(
            parts[2],
            json!({"type": "image_url", "image_url": {"url": "https://example.com/x.png"}})
        );
    }

    #[test]
    fn tool_use_becomes_tool_calls_and_tool_result_becomes_tool_message() {
        let out = to_chat_request(&req(json!({"messages": [
            {"role": "user", "content": "read it"},
            {"role": "assistant", "content": [
                {"type": "text", "text": "reading"},
                {"type": "tool_use", "id": "toolu_1", "name": "read_file", "input": {"path": "a.rs"}}
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_1", "content": "fn main() {}"},
                {"type": "text", "text": "now explain"}
            ]}
        ]}))).unwrap();
        let m = out["messages"].as_array().unwrap();
        assert_eq!(m[1]["role"], "assistant");
        assert_eq!(m[1]["content"], "reading");
        assert_eq!(m[1]["tool_calls"][0]["id"], "toolu_1");
        assert_eq!(m[1]["tool_calls"][0]["type"], "function");
        assert_eq!(m[1]["tool_calls"][0]["function"]["name"], "read_file");
        assert_eq!(
            m[1]["tool_calls"][0]["function"]["arguments"],
            "{\"path\":\"a.rs\"}"
        );
        assert_eq!(
            m[2],
            json!({"role": "tool", "tool_call_id": "toolu_1", "content": "fn main() {}"})
        );
        assert_eq!(m[3], json!({"role": "user", "content": "now explain"}));
    }

    #[test]
    fn tool_result_block_list_flattens_and_error_prefixes() {
        let out = to_chat_request(&req(json!({"messages": [
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t", "is_error": true, "content": [
                    {"type": "text", "text": "boom"},
                    {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "x"}}
                ]}
            ]}
        ]}))).unwrap();
        assert_eq!(out["messages"][0]["content"], "Error: boom");
    }

    #[test]
    fn thinking_blocks_in_history_are_dropped() {
        let out = to_chat_request(&req(json!({"messages": [
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hmm", "signature": "s"},
                {"type": "redacted_thinking", "data": "x"},
                {"type": "text", "text": "answer"}
            ]}
        ]})))
        .unwrap();
        assert_eq!(out["messages"][0]["content"], "answer");
        assert!(out["messages"][0].get("tool_calls").is_none());
    }

    #[test]
    fn tools_and_tool_choice_translate() {
        let out = to_chat_request(&req(json!({
            "tools": [{"name": "f", "description": "d", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "tool", "name": "f", "disable_parallel_tool_use": true}
        })))
        .unwrap();
        assert_eq!(
            out["tools"][0],
            json!({"type": "function", "function": {"name": "f", "description": "d", "parameters": {"type": "object"}}})
        );
        assert_eq!(
            out["tool_choice"],
            json!({"type": "function", "function": {"name": "f"}})
        );
        assert_eq!(out["parallel_tool_calls"], false);
        let auto = to_chat_request(&req(json!({"tool_choice": {"type": "auto"}}))).unwrap();
        assert_eq!(auto["tool_choice"], "auto");
        let any = to_chat_request(&req(json!({"tool_choice": {"type": "any"}}))).unwrap();
        assert_eq!(any["tool_choice"], "required");
    }

    #[test]
    fn scalar_fields_map_and_unsupported_ones_are_stripped() {
        let out = to_chat_request(&req(json!({
            "stop_sequences": ["END"], "temperature": 0.2, "top_p": 0.9, "top_k": 40,
            "metadata": {"user_id": "u1"}, "thinking": {"type": "enabled", "budget_tokens": 1024},
            "service_tier": "auto", "cache_control": {"type": "ephemeral"}, "container": "c", "mcp_servers": [], "custom_passthrough": 1
        })))
        .unwrap();
        assert_eq!(out["stop"], json!(["END"]));
        assert_eq!(out["temperature"], 0.2);
        assert_eq!(out["top_p"], 0.9);
        assert_eq!(out["user"], "u1");
        assert_eq!(out["custom_passthrough"], 1);
        for k in [
            "top_k",
            "metadata",
            "thinking",
            "service_tier",
            "cache_control",
            "container",
            "mcp_servers",
            "stop_sequences",
            "system",
        ] {
            assert!(out.get(k).is_none(), "{k} should not be forwarded");
        }
    }

    #[test]
    fn stream_forces_include_usage() {
        let out = to_chat_request(&req(json!({"stream": true}))).unwrap();
        assert_eq!(out["stream"], true);
        assert_eq!(out["stream_options"]["include_usage"], true);
        let off = to_chat_request(&req(json!({}))).unwrap();
        assert!(off.get("stream_options").is_none());
        // An explicit `false` is forwarded unchanged, not dropped like the
        // absent case.
        let explicit_off = to_chat_request(&req(json!({"stream": false}))).unwrap();
        assert_eq!(explicit_off["stream"], false);
        assert!(explicit_off.get("stream_options").is_none());
    }

    fn ctx() -> ResponseContext {
        ResponseContext {
            request_id: "req-1".into(),
            model: "claude-sonnet-4-5".into(),
        }
    }

    #[test]
    fn text_reply_maps_to_message_object() {
        let chat = json!({"choices": [{"message": {"role": "assistant", "content": "hello"}, "finish_reason": "stop"}],
                          "usage": {"prompt_tokens": 12, "completion_tokens": 3}});
        let out = from_chat_response(&chat, &ctx());
        assert_eq!(out["id"], "msg_req-1");
        assert_eq!(out["type"], "message");
        assert_eq!(out["role"], "assistant");
        assert_eq!(out["model"], "claude-sonnet-4-5");
        assert_eq!(out["content"], json!([{"type": "text", "text": "hello"}]));
        assert_eq!(out["stop_reason"], "end_turn");
        assert_eq!(out["stop_sequence"], Value::Null);
        assert_eq!(
            out["usage"],
            json!({"input_tokens": 12, "output_tokens": 3})
        );
    }

    #[test]
    fn tool_calls_become_tool_use_blocks_with_parsed_input() {
        let chat = json!({"choices": [{"message": {"content": null, "tool_calls": [
            {"id": "call_1", "type": "function", "function": {"name": "f", "arguments": "{\"a\":1}"}},
            {"id": "call_2", "type": "function", "function": {"name": "g", "arguments": "not json"}}
        ]}, "finish_reason": "tool_calls"}], "usage": {"prompt_tokens": 1, "completion_tokens": 2}});
        let out = from_chat_response(&chat, &ctx());
        assert_eq!(
            out["content"][0],
            json!({"type": "tool_use", "id": "call_1", "name": "f", "input": {"a": 1}})
        );
        assert_eq!(
            out["content"][1],
            json!({"type": "tool_use", "id": "call_2", "name": "g", "input": {}})
        );
        assert_eq!(out["stop_reason"], "tool_use");
    }

    #[test]
    fn reasoning_becomes_thinking_block_before_text() {
        let chat = json!({"choices": [{"message": {"content": "x", "reasoning_content": "why"}, "finish_reason": "length"}]});
        let out = from_chat_response(&chat, &ctx());
        assert_eq!(
            out["content"][0],
            json!({"type": "thinking", "thinking": "why", "signature": ""})
        );
        assert_eq!(out["content"][1]["type"], "text");
        assert_eq!(out["stop_reason"], "max_tokens");
        assert_eq!(out["usage"], json!({"input_tokens": 0, "output_tokens": 0}));
    }

    #[test]
    fn reasoning_spelling_fallback_is_read_non_streaming() {
        // LiteLLM normalises to `reasoning_content`; a vLLM server straight
        // behind the gateway sends `reasoning`.
        let chat = json!({"choices": [{"message": {"content": "x", "reasoning": "why"}, "finish_reason": "stop"}]});
        let out = from_chat_response(&chat, &ctx());
        assert_eq!(
            out["content"][0],
            json!({"type": "thinking", "thinking": "why", "signature": ""})
        );
    }

    #[test]
    fn stop_reason_table() {
        assert_eq!(stop_reason(Some("stop"), false), "end_turn");
        assert_eq!(stop_reason(Some("length"), false), "max_tokens");
        assert_eq!(stop_reason(Some("tool_calls"), false), "tool_use");
        assert_eq!(stop_reason(Some("stop"), true), "tool_use");
        assert_eq!(stop_reason(Some("content_filter"), false), "end_turn");
        assert_eq!(stop_reason(None, false), "end_turn");
        // `length` wins even mid-tool-call: a reply truncated by max_tokens
        // must not be reported as a runnable `tool_use`.
        assert_eq!(stop_reason(Some("length"), true), "max_tokens");
    }

    #[test]
    fn error_type_table() {
        use http::StatusCode as S;
        assert_eq!(error_type_for(S::BAD_REQUEST), "invalid_request_error");
        assert_eq!(error_type_for(S::UNAUTHORIZED), "authentication_error");
        assert_eq!(error_type_for(S::FORBIDDEN), "permission_error");
        assert_eq!(error_type_for(S::NOT_FOUND), "not_found_error");
        assert_eq!(error_type_for(S::PAYLOAD_TOO_LARGE), "request_too_large");
        assert_eq!(error_type_for(S::TOO_MANY_REQUESTS), "rate_limit_error");
        assert_eq!(error_type_for(S::BAD_GATEWAY), "overloaded_error");
        assert_eq!(error_type_for(S::SERVICE_UNAVAILABLE), "overloaded_error");
        assert_eq!(error_type_for(S::GATEWAY_TIMEOUT), "overloaded_error");
        assert_eq!(error_type_for(S::INTERNAL_SERVER_ERROR), "api_error");
    }

    #[test]
    fn upstream_message_reads_openai_envelope_or_truncates_raw() {
        assert_eq!(
            upstream_error_message(br#"{"error":{"message":"budget exhausted"}}"#),
            "budget exhausted"
        );
        let raw = vec![b'x'; 600];
        assert_eq!(upstream_error_message(&raw).len(), 512);
        assert_eq!(
            error_envelope("api_error", "m"),
            json!({"type": "error", "error": {"type": "api_error", "message": "m"}})
        );
    }

    fn parse(frames: &[String]) -> Vec<(String, Value)> {
        frames
            .iter()
            .map(|f| {
                let mut ev = String::new();
                let mut data = String::new();
                for line in f.lines() {
                    if let Some(v) = line.strip_prefix("event: ") {
                        ev = v.to_string();
                    }
                    if let Some(v) = line.strip_prefix("data: ") {
                        data = v.to_string();
                    }
                }
                (ev, serde_json::from_str(&data).unwrap())
            })
            .collect()
    }
    fn chunk(delta: Value, finish: Option<&str>) -> Value {
        let mut c = json!({"object": "chat.completion.chunk", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]});
        if finish.is_some() {
            c["choices"][0]["finish_reason"] = json!(finish);
        }
        c
    }
    fn usage_chunk(p: u64, c: u64) -> Value {
        json!({"object": "chat.completion.chunk", "choices": [], "usage": {"prompt_tokens": p, "completion_tokens": c}})
    }

    #[test]
    fn text_stream_event_sequence() {
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut frames = t.on_chunk(&chunk(json!({"role": "assistant", "content": ""}), None));
        frames.extend(t.on_chunk(&chunk(json!({"content": "Hel"}), None)));
        frames.extend(t.on_chunk(&chunk(json!({"content": "lo"}), None)));
        frames.extend(t.on_chunk(&chunk(json!({}), Some("stop"))));
        frames.extend(t.on_chunk(&usage_chunk(10, 2)));
        frames.extend(t.finish());
        let ev = parse(&frames);
        let names: Vec<&str> = ev.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ]
        );
        assert_eq!(ev[0].1["message"]["id"], "msg_req-1");
        assert_eq!(ev[0].1["message"]["usage"]["input_tokens"], 0);
        assert_eq!(
            ev[1].1["content_block"],
            json!({"type": "text", "text": ""})
        );
        assert_eq!(
            ev[2].1["delta"],
            json!({"type": "text_delta", "text": "Hel"})
        );
        assert_eq!(
            ev[5].1["delta"],
            json!({"stop_reason": "end_turn", "stop_sequence": null})
        );
        assert_eq!(
            ev[5].1["usage"],
            json!({"input_tokens": 10, "output_tokens": 2})
        );
        assert!(t.finish().is_empty(), "finish is idempotent");
    }

    #[test]
    fn message_start_takes_input_tokens_from_first_chunk_when_present() {
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut first = chunk(json!({"content": "a"}), None);
        first["usage"] = json!({"prompt_tokens": 7, "completion_tokens": 0});
        let ev = parse(&t.on_chunk(&first));
        assert_eq!(ev[0].1["message"]["usage"]["input_tokens"], 7);
    }

    #[test]
    fn tool_call_fragments_stream_as_input_json_delta() {
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut frames = t.on_chunk(&chunk(json!({"content": "Let me "}), None));
        frames.extend(t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "id": "call_1", "type": "function", "function": {"name": "f", "arguments": ""}}]}),
            None,
        )));
        frames.extend(t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "{\"a\":"}}]}),
            None,
        )));
        frames.extend(t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "1}"}}]}),
            None,
        )));
        frames.extend(t.on_chunk(&chunk(json!({}), Some("tool_calls"))));
        frames.extend(t.finish());
        let ev = parse(&frames);
        let names: Vec<&str> = ev.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop"
            ]
        );
        assert_eq!(ev[4].1["index"], 1);
        assert_eq!(
            ev[4].1["content_block"],
            json!({"type": "tool_use", "id": "call_1", "name": "f", "input": {}})
        );
        assert_eq!(
            ev[5].1["delta"],
            json!({"type": "input_json_delta", "partial_json": "{\"a\":"})
        );
        assert_eq!(
            ev[6].1["delta"],
            json!({"type": "input_json_delta", "partial_json": "1}"})
        );
        assert_eq!(ev[8].1["delta"]["stop_reason"], "tool_use");
    }

    #[test]
    fn parallel_tool_calls_get_their_own_blocks() {
        // The ordinary sequential case every real upstream (OpenAI, vLLM,
        // LiteLLM) produces: one call's fragments finish before the next
        // call's `index` is seen. Tool blocks are never open at once — the
        // second is not started until the first is stopped.
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut frames = t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "id": "c0", "function": {"name": "f", "arguments": ""}}]}),
            None,
        ));
        frames.extend(t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "{}"}}]}),
            None,
        )));
        frames.extend(t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 1, "id": "c1", "function": {"name": "g", "arguments": ""}}]}),
            None,
        )));
        frames.extend(t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 1, "function": {"arguments": "{}"}}]}),
            None,
        )));
        frames.extend(t.finish());
        let ev = parse(&frames);
        let names: Vec<&str> = ev.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "message_start",
                "content_block_start", // tool 0
                "content_block_delta", // tool 0
                "content_block_stop",  // tool 0, closed before tool 1 opens
                "content_block_start", // tool 1
                "content_block_delta", // tool 1
                "content_block_stop",  // tool 1
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(ev[1].1["index"], 0);
        assert_eq!(ev[1].1["content_block"]["id"], "c0");
        assert_eq!(ev[2].1["delta"]["partial_json"], "{}");
        assert_eq!(ev[4].1["index"], 1);
        assert_eq!(ev[4].1["content_block"]["id"], "c1");
        assert_eq!(ev[5].1["delta"]["partial_json"], "{}");
    }

    #[test]
    fn a_fragment_for_an_already_closed_tool_block_is_forwarded_not_dropped() {
        // A truly interleaving upstream: content arrives between two
        // fragments of the same tool call, so its block is closed (because
        // the content block took over) before the last fragment lands.
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut frames = t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "id": "c0", "function": {"name": "f", "arguments": "{\"a\":"}}]}),
            None,
        ));
        frames.extend(t.on_chunk(&chunk(json!({"content": "x"}), None)));
        frames.extend(t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": "1}"}}]}),
            None,
        )));
        frames.extend(t.finish());
        let ev = parse(&frames);
        let names: Vec<&str> = ev.iter().map(|(n, _)| n.as_str()).collect();
        // No new content_block_start for index 0: the late fragment is
        // forwarded to the existing (closed) block rather than reopening it.
        let starts = names
            .iter()
            .filter(|n| **n == "content_block_start")
            .count();
        assert_eq!(starts, 2, "tool block 0 and text block 1, no duplicate");
        let late_delta = ev
            .iter()
            .find(|(n, v)| n == "content_block_delta" && v["delta"]["partial_json"] == "1}")
            .expect("the late fragment is forwarded");
        assert_eq!(late_delta.1["index"], 0);
    }

    #[test]
    fn reasoning_then_text_are_separate_blocks() {
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut frames = t.on_chunk(&chunk(json!({"reasoning_content": "hmm"}), None));
        frames.extend(t.on_chunk(&chunk(json!({"content": "ok"}), None)));
        frames.extend(t.finish());
        let ev = parse(&frames);
        assert_eq!(ev[1].1["content_block"]["type"], "thinking");
        assert_eq!(
            ev[2].1["delta"],
            json!({"type": "thinking_delta", "thinking": "hmm"})
        );
        assert_eq!(ev[3].0, "content_block_stop");
        assert_eq!(ev[4].1["content_block"]["type"], "text");
        assert_eq!(ev[4].1["index"], 1);
    }

    #[test]
    fn reasoning_spelling_fallback_is_read_in_stream() {
        // LiteLLM normalises to `reasoning_content`; a vLLM server straight
        // behind the gateway sends `reasoning`.
        let mut t = AnthropicStreamTranslator::new(ctx());
        let frames = t.on_chunk(&chunk(json!({"reasoning": "hmm"}), None));
        let ev = parse(&frames);
        let (_, delta) = ev
            .iter()
            .find(|(n, _)| n == "content_block_delta")
            .expect("a thinking delta");
        assert_eq!(
            delta["delta"],
            json!({"type": "thinking_delta", "thinking": "hmm"})
        );
    }

    #[test]
    fn missing_index_keys_by_id_and_two_id_only_calls_get_two_blocks() {
        // Some OpenAI-compatible servers (older Ollama, some llama.cpp
        // builds) omit `tool_calls[].index` and send each call whole.
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut frames = t.on_chunk(&chunk(
            json!({"tool_calls": [{"id": "c0", "function": {"name": "f", "arguments": "{\"p\":1}"}}]}),
            None,
        ));
        frames.extend(t.on_chunk(&chunk(
            json!({"tool_calls": [{"id": "c1", "function": {"name": "g", "arguments": "{\"p\":2}"}}]}),
            None,
        )));
        frames.extend(t.finish());
        let ev = parse(&frames);
        let starts: Vec<&Value> = ev
            .iter()
            .filter(|(n, _)| n == "content_block_start")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(starts.len(), 2, "two distinct ids, two blocks");
        assert_eq!(starts[0]["content_block"]["id"], "c0");
        assert_eq!(starts[0]["content_block"]["name"], "f");
        assert_eq!(starts[1]["content_block"]["id"], "c1");
        assert_eq!(starts[1]["content_block"]["name"], "g");
        let deltas: Vec<&Value> = ev
            .iter()
            .filter(|(n, _)| n == "content_block_delta")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(deltas[0]["delta"]["partial_json"], "{\"p\":1}");
        assert_eq!(deltas[1]["delta"]["partial_json"], "{\"p\":2}");
    }

    #[test]
    fn tool_name_arriving_on_a_later_fragment_is_not_lost() {
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut frames = t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "id": "c0", "type": "function", "function": {"arguments": ""}}]}),
            None,
        ));
        frames.extend(t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "function": {"name": "f", "arguments": "{}"}}]}),
            None,
        )));
        frames.extend(t.finish());
        let ev = parse(&frames);
        let (_, start) = ev
            .iter()
            .find(|(n, _)| n == "content_block_start")
            .expect("a tool_use block");
        // Exactly one content_block_start: the block waited for the name.
        assert_eq!(
            ev.iter()
                .filter(|(n, _)| n == "content_block_start")
                .count(),
            1
        );
        assert_eq!(
            start["content_block"],
            json!({"type": "tool_use", "id": "c0", "name": "f", "input": {}})
        );
        let (_, delta) = ev
            .iter()
            .find(|(n, _)| n == "content_block_delta")
            .expect("the buffered fragment flushed once the block opened");
        assert_eq!(delta["delta"]["partial_json"], "{}");
    }

    #[test]
    fn a_tool_call_with_no_id_gets_a_synthesized_one() {
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut frames = t.on_chunk(&chunk(
            json!({"tool_calls": [{"index": 0, "function": {"name": "f", "arguments": "{}"}}]}),
            None,
        ));
        frames.extend(t.finish());
        let ev = parse(&frames);
        let (_, start) = ev
            .iter()
            .find(|(n, _)| n == "content_block_start")
            .expect("a tool_use block");
        assert_eq!(start["content_block"]["id"], "toolu_req-1_0");
    }

    #[test]
    fn on_chunk_returns_fail_frames_for_a_mid_stream_error_payload() {
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut frames = t.on_chunk(&chunk(json!({"content": "partial"}), None));
        frames.extend(t.on_chunk(&json!({"error": {"message": "budget exhausted"}})));
        let ev = parse(&frames);
        let names: Vec<&str> = ev.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "error"
            ]
        );
        assert_eq!(ev[4].1["error"]["type"], "api_error");
        assert_eq!(ev[4].1["error"]["message"], "budget exhausted");
        // The error terminates the stream like `fail` does.
        assert!(t.finish().is_empty());
        assert!(t
            .on_chunk(&chunk(json!({"content": "more"}), None))
            .is_empty());
    }

    #[test]
    fn fail_closes_open_blocks_and_emits_error() {
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut frames = t.on_chunk(&chunk(json!({"content": "partial"}), None));
        frames.extend(t.fail(
            "api_error",
            "upstream stream ended before the response finished",
        ));
        let ev = parse(&frames);
        let names: Vec<&str> = ev.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_stop",
                "error"
            ]
        );
        assert_eq!(ev[4].1["error"]["type"], "api_error");
        assert!(t.finish().is_empty());
    }

    #[test]
    fn ping_opens_the_message_if_nothing_has_streamed_yet() {
        // A slow prefill or queue wait must not leave the client in total
        // silence past its own timeout before a single byte arrives.
        let mut t = AnthropicStreamTranslator::new(ctx());
        let ev = parse(&t.ping());
        let names: Vec<&str> = ev.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["message_start", "ping"]);
        assert_eq!(ev[0].1["message"]["usage"]["input_tokens"], 0);

        // Once started, a later ping is just the one frame.
        let ev2 = parse(&t.ping());
        assert_eq!(ev2.len(), 1);
        assert_eq!(ev2[0].0, "ping");
        assert_eq!(ev2[0].1, json!({"type": "ping"}));

        // And after the stream ends, none at all.
        t.finish();
        assert!(t.ping().is_empty());
    }

    #[test]
    fn finish_without_terminal_chunk_still_closes_the_message() {
        let mut t = AnthropicStreamTranslator::new(ctx());
        let mut frames = t.on_chunk(&chunk(json!({"content": "a"}), None));
        frames.extend(t.finish());
        let names: Vec<String> = parse(&frames).into_iter().map(|(n, _)| n).collect();
        assert_eq!(names.last().unwrap(), "message_stop");
        assert!(names.contains(&"message_delta".to_string()));
    }
}
