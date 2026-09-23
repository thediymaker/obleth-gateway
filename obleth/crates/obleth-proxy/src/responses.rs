//! The **Responses API** shim: `/v1/responses` served by translating to and
//! from Chat Completions at the edge of the proxy.
//!
//! Clients have started defaulting to this API — OpenWebUI picks it per
//! connection — and nothing in this fleet serves it, so the request used to be
//! forwarded to an upstream that answered 404 and the caller saw another
//! service's error for a model that was working fine.
//!
//! Translating at the edge rather than teaching the pipeline a second schema is
//! deliberate: routing, fairshare, the tool loop and every boon read `messages`
//! and write `choices`, and they keep doing exactly that. The conversion is a
//! pure function on each side, so the whole pipeline is exercised unchanged.
//!
//! **Stateless only.** `store`, `previous_response_id` and `background` are
//! refused rather than ignored: they promise the caller that the gateway is
//! keeping their conversation, and obleth persists usage metadata, never
//! prompt or output text. Quietly dropping them would make a client believe in
//! a history that does not exist. See [`unsupported_field`].

use serde_json::{json, Map, Value};

/// The path this shim owns.
pub(crate) const RESPONSES_PATH: &str = "/v1/responses";

/// The chat path every translated request is run as, so `is_chat_path` is true
/// and the boons apply exactly as they would to a native chat call.
pub(crate) const CHAT_PATH: &str = "/v1/chat/completions";

/// Header stamped on a translated request so the handler can still record the
/// surface the caller actually used, rather than logging every Responses call
/// as chat.
pub(crate) const SURFACE_HEADER: &str = "x-obleth-api-surface";

/// Fields that only mean something with server-side conversation storage.
/// Returning the offending name lets the caller be told which one to drop.
pub(crate) fn unsupported_field(body: &Value) -> Option<&'static str> {
    if body.get("store").and_then(Value::as_bool) == Some(true) {
        return Some("store");
    }
    if body
        .get("previous_response_id")
        .is_some_and(|v| !v.is_null())
    {
        return Some("previous_response_id");
    }
    if body.get("background").and_then(Value::as_bool) == Some(true) {
        return Some("background");
    }
    None
}

/// Flatten one Responses content array into plain text, keeping the parts a
/// chat model can read. Both the input and output spellings are accepted
/// because a client replays its own previous turns back as input.
fn text_of(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(Value::as_str) {
                Some("input_text" | "output_text" | "text" | "summary_text") => {
                    p.get("text").and_then(Value::as_str)
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// Image parts of one content array, as chat `image_url` parts.
fn image_parts(content: &Value) -> Vec<Value> {
    let Value::Array(parts) = content else {
        return Vec::new();
    };
    parts
        .iter()
        .filter(|p| p.get("type").and_then(Value::as_str) == Some("input_image"))
        .filter_map(|p| {
            // `image_url` is a bare string here, unlike chat's nested object.
            let url = p.get("image_url").and_then(|u| {
                u.as_str()
                    .map(str::to_string)
                    .or_else(|| u.get("url").and_then(Value::as_str).map(str::to_string))
            })?;
            Some(json!({ "type": "image_url", "image_url": { "url": url } }))
        })
        .collect()
}

/// One input item as the chat message(s) it stands for. Returns nothing for an
/// item this shim does not model, so an unknown item is skipped rather than
/// corrupting the conversation.
fn item_to_messages(item: &Value) -> Vec<Value> {
    let kind = item
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("message");
    match kind {
        "message" => {
            let role = item.get("role").and_then(Value::as_str).unwrap_or("user");
            let content = item.get("content").unwrap_or(&Value::Null);
            let text = text_of(content);
            let images = image_parts(content);
            if images.is_empty() {
                return vec![json!({ "role": role, "content": text })];
            }
            // A vision turn keeps the multimodal shape chat expects.
            let mut parts = vec![json!({ "type": "text", "text": text })];
            parts.extend(images);
            vec![json!({ "role": role, "content": parts })]
        }
        // The model asked for a tool: an assistant turn carrying the call.
        "function_call" => vec![json!({
            "role": "assistant",
            "content": Value::Null,
            "tool_calls": [{
                "id": item.get("call_id").and_then(Value::as_str).unwrap_or(""),
                "type": "function",
                "function": {
                    "name": item.get("name").and_then(Value::as_str).unwrap_or(""),
                    "arguments": item.get("arguments").and_then(Value::as_str).unwrap_or("{}"),
                },
            }],
        })],
        // Its result, which chat carries as a `tool` message.
        "function_call_output" => vec![json!({
            "role": "tool",
            "tool_call_id": item.get("call_id").and_then(Value::as_str).unwrap_or(""),
            "content": item.get("output").and_then(Value::as_str).unwrap_or(""),
        })],
        // Reasoning items are the model's own scratchpad replayed back; chat
        // has no slot for them and a model does not need its previous thinking
        // re-fed, so they are dropped rather than flattened into content.
        "reasoning" => Vec::new(),
        _ => Vec::new(),
    }
}

/// Convert a Responses request body into the Chat Completions body the
/// pipeline runs. Unknown top-level fields are carried through untouched so a
/// sampling knob this shim has not heard of still reaches the upstream.
pub(crate) fn to_chat_request(body: &Value) -> Value {
    let mut out = Map::new();
    if let Some(obj) = body.as_object() {
        for (k, v) in obj {
            match k.as_str() {
                // Translated below, or meaningless downstream.
                "input"
                | "instructions"
                | "max_output_tokens"
                | "tools"
                | "text"
                | "store"
                | "previous_response_id"
                | "background"
                | "include"
                | "reasoning"
                | "conversation" => {}
                _ => {
                    out.insert(k.clone(), v.clone());
                }
            }
        }
    }

    let mut messages: Vec<Value> = Vec::new();
    // `instructions` is the Responses spelling of a system prompt, and it
    // applies to this turn only, so it leads the conversation.
    if let Some(text) = body.get("instructions").and_then(Value::as_str) {
        if !text.trim().is_empty() {
            messages.push(json!({ "role": "system", "content": text }));
        }
    }
    match body.get("input") {
        Some(Value::String(s)) => messages.push(json!({ "role": "user", "content": s })),
        Some(Value::Array(items)) => {
            for item in items {
                messages.extend(item_to_messages(item));
            }
        }
        _ => {}
    }
    out.insert("messages".into(), Value::Array(messages));

    if let Some(max) = body.get("max_output_tokens").and_then(Value::as_i64) {
        out.insert("max_tokens".into(), json!(max));
    }
    if let Some(tools) = body.get("tools").and_then(Value::as_array) {
        let converted: Vec<Value> = tools.iter().filter_map(tool_to_chat).collect();
        if !converted.is_empty() {
            out.insert("tools".into(), Value::Array(converted));
        }
    }
    // `text.format` is the Responses spelling of `response_format`.
    if let Some(format) = body.pointer("/text/format") {
        out.insert("response_format".into(), format.clone());
    }
    // A streamed Responses reply always ends with usage on
    // `response.completed`, so the chat stream it is built from must carry it.
    if body.get("stream").and_then(Value::as_bool) == Some(true) {
        let options = out.entry("stream_options").or_insert_with(|| json!({}));
        if !options.is_object() {
            *options = json!({});
        }
        options["include_usage"] = json!(true);
    }
    Value::Object(out)
}

/// A Responses tool as a chat tool. Responses flattens the function onto the
/// tool; chat nests it. Hosted tool types (`web_search` and friends) have no
/// self-hosted equivalent and are dropped.
fn tool_to_chat(tool: &Value) -> Option<Value> {
    if tool.get("type").and_then(Value::as_str) != Some("function") {
        return None;
    }
    // Already chat-shaped (a client mixing the two) — pass it through.
    if tool.get("function").is_some() {
        return Some(tool.clone());
    }
    let name = tool.get("name").and_then(Value::as_str)?;
    let mut function = Map::new();
    function.insert("name".into(), json!(name));
    if let Some(d) = tool.get("description") {
        function.insert("description".into(), d.clone());
    }
    if let Some(p) = tool.get("parameters") {
        function.insert("parameters".into(), p.clone());
    }
    Some(json!({ "type": "function", "function": Value::Object(function) }))
}

/// Convert a Chat Completions reply into a Responses object.
///
/// `id` is the gateway's request id rather than an invented one, so a caller
/// that logs `response.id` can hand it straight back for a trace lookup.
pub(crate) fn from_chat_response(chat: &Value, id: &str) -> Value {
    let message = chat.pointer("/choices/0/message");
    let finish = chat
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .unwrap_or("stop");

    let mut output: Vec<Value> = Vec::new();
    // A reasoning model puts its scratchpad beside the answer, under either
    // spelling; it is a distinct item here, not part of the reply text.
    if let Some(text) = message
        .and_then(|m| m.get("reasoning_content").or_else(|| m.get("reasoning")))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        output.push(json!({
            "type": "reasoning",
            "id": format!("rs_{id}"),
            "status": "completed",
            "summary": [],
            "content": [{ "type": "reasoning_text", "text": text }],
        }));
    }
    if let Some(text) = message
        .and_then(|m| m.get("content"))
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    {
        output.push(json!({
            "type": "message",
            "id": format!("msg_{id}"),
            "status": "completed",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text, "annotations": [] }],
        }));
    }
    if let Some(calls) = message
        .and_then(|m| m.get("tool_calls"))
        .and_then(Value::as_array)
    {
        for (i, call) in calls.iter().enumerate() {
            output.push(json!({
                "type": "function_call",
                "id": format!("fc_{id}_{i}"),
                "status": "completed",
                "call_id": call.get("id").and_then(Value::as_str).unwrap_or(""),
                "name": call.pointer("/function/name").and_then(Value::as_str).unwrap_or(""),
                "arguments": call.pointer("/function/arguments").and_then(Value::as_str).unwrap_or("{}"),
            }));
        }
    }

    let mut out = json!({
        "id": format!("resp_{id}"),
        "object": "response",
        "created_at": chat.get("created").and_then(Value::as_i64).unwrap_or(0),
        "status": status_for(finish),
        "model": chat.get("model").cloned().unwrap_or(Value::Null),
        "output": output,
        "parallel_tool_calls": true,
        "tool_choice": "auto",
        "tools": [],
    });
    // `usage` is renamed, not reshaped: Responses counts the same tokens under
    // `input_tokens`/`output_tokens`.
    if let Some(u) = chat.get("usage") {
        out["usage"] = json!({
            "input_tokens": u.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0),
            "output_tokens": u.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0),
            "total_tokens": u.get("total_tokens").and_then(Value::as_i64).unwrap_or(0),
        });
    }
    if finish == "length" {
        out["incomplete_details"] = json!({ "reason": "max_output_tokens" });
    }
    out
}

/// A chat `finish_reason` as a Responses `status`. A truncated reply is
/// `incomplete` there, not `completed`, and a caller checking status alone
/// would otherwise treat a cut-off answer as a whole one.
fn status_for(finish_reason: &str) -> &'static str {
    match finish_reason {
        "length" => "incomplete",
        "content_filter" => "incomplete",
        _ => "completed",
    }
}

/// Drain the `data:` payloads of every complete line in an SSE byte buffer,
/// leaving any partial trailing line in place.
///
/// The buffer is split on the newline BYTE and only complete lines are
/// decoded: a UTF-8 continuation byte can never be 0x0A, so the split cannot
/// land inside a character. Decoding each network chunk independently
/// corrupted CJK and emoji whenever a codepoint straddled a chunk boundary —
/// each half became U+FFFD.
pub(crate) fn drain_sse_data_lines(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(idx) = buffer.iter().position(|b| *b == b'\n') {
        let line_bytes: Vec<u8> = buffer.drain(..=idx).collect();
        let line = String::from_utf8_lossy(&line_bytes);
        let Some(payload) = line.trim().strip_prefix("data:") else {
            continue;
        };
        let payload = payload.trim();
        if !payload.is_empty() {
            out.push(payload.to_string());
        }
    }
    out
}

/// Translates a Chat Completions SSE stream into Responses events.
///
/// The event vocabulary is the one a Responses client actually consumes:
/// `response.created`, `response.in_progress`, `response.output_item.added`,
/// `response.content_part.added`, a run of deltas, then the matching `.done`
/// events and `response.completed`. Indices and `item_id` ride on every event
/// because clients address the item they are updating.
///
/// Two kinds of item are produced. Reasoning models here stream their
/// scratchpad as `delta.reasoning` (or `delta.reasoning_content`) with no
/// `content` at all until they finish thinking — glm-5-3 does exactly this —
/// so that becomes a `reasoning` item emitting `response.reasoning_text.delta`,
/// and the answer that follows becomes the `message` item.
///
/// Tool calls are emitted as whole `function_call` items at the end rather
/// than argument deltas: the gateway's own tool loop resolves a call before
/// the reply streams, so a partial call is never in flight for a client to
/// render.
pub(crate) struct StreamTranslator {
    response_id: String,
    request_id: String,
    model: String,
    seq: u64,
    started: bool,
    completed: bool,
    /// The item currently streaming, if any. Each opening is a NEW item with
    /// its own id: a model that interleaves reasoning and content produces one
    /// item per segment, in order, rather than one reused id appearing at two
    /// output indices — clients key UI state by `item_id`.
    current: Option<CurrentItem>,
    /// Completed items in stream order, echoed on `response.completed`.
    done_items: Vec<Value>,
    next_index: usize,
    tool_calls: Vec<Value>,
    usage: Option<Value>,
    finish_reason: String,
}

#[derive(Clone, Copy, PartialEq)]
enum ItemKind {
    Reasoning,
    Message,
}

struct CurrentItem {
    kind: ItemKind,
    id: String,
    index: usize,
    text: String,
}

impl CurrentItem {
    /// The item as JSON, `completed` with its text or `in_progress` and empty.
    fn json(&self, status: &str, with_text: bool) -> Value {
        let content = |part_type: &str| {
            if with_text {
                json!([{ "type": part_type, "text": self.text }])
            } else {
                json!([])
            }
        };
        match self.kind {
            ItemKind::Reasoning => json!({
                "id": self.id,
                "type": "reasoning",
                "status": status,
                "summary": [],
                "content": content("reasoning_text"),
            }),
            ItemKind::Message => {
                let mut v = json!({
                    "id": self.id,
                    "type": "message",
                    "status": status,
                    "role": "assistant",
                    "content": content("output_text"),
                });
                if with_text {
                    v["content"][0]["annotations"] = json!([]);
                }
                v
            }
        }
    }
}

impl StreamTranslator {
    pub(crate) fn new(request_id: &str, model: &str) -> Self {
        Self {
            response_id: format!("resp_{request_id}"),
            request_id: request_id.to_string(),
            model: model.to_string(),
            seq: 0,
            started: false,
            completed: false,
            current: None,
            done_items: Vec::new(),
            next_index: 0,
            tool_calls: Vec::new(),
            usage: None,
            finish_reason: "stop".to_string(),
        }
    }

    /// `event:` line plus `data:` payload, with the running sequence number
    /// every Responses event carries.
    fn frame(&mut self, kind: &str, mut data: Value) -> String {
        data["type"] = json!(kind);
        data["sequence_number"] = json!(self.seq);
        self.seq += 1;
        format!("event: {kind}\ndata: {data}\n\n")
    }

    /// The `response` object echoed on the lifecycle events.
    fn envelope(&self, status: &str, output: Value) -> Value {
        let mut v = json!({
            "id": self.response_id,
            "object": "response",
            "status": status,
            "model": self.model,
            "output": output,
            "parallel_tool_calls": true,
            "tool_choice": "auto",
            "tools": [],
        });
        if let Some(u) = &self.usage {
            v["usage"] = u.clone();
        }
        v
    }

    /// `response.created` and `response.in_progress`, emitted once before the
    /// first item of any kind.
    fn start(&mut self) -> Vec<String> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        let envelope = self.envelope("in_progress", json!([]));
        vec![
            self.frame("response.created", json!({ "response": envelope.clone() })),
            self.frame("response.in_progress", json!({ "response": envelope })),
        ]
    }

    /// Switch the streaming item, closing whatever was open first.
    fn switch_to(&mut self, kind: ItemKind) -> Vec<String> {
        if self.current.as_ref().is_some_and(|c| c.kind == kind) {
            return Vec::new();
        }
        let mut out = self.close_open();
        out.extend(self.start());

        let index = self.next_index;
        self.next_index += 1;
        let prefix = match kind {
            ItemKind::Reasoning => "rs",
            ItemKind::Message => "msg",
        };
        let part_type = match kind {
            ItemKind::Reasoning => "reasoning_text",
            ItemKind::Message => "output_text",
        };
        let item = CurrentItem {
            kind,
            id: format!("{prefix}_{}_{index}", self.request_id),
            index,
            text: String::new(),
        };
        let added = item.json("in_progress", false);
        let id = item.id.clone();
        self.current = Some(item);
        out.push(self.frame(
            "response.output_item.added",
            json!({ "output_index": index, "item": added }),
        ));
        out.push(self.frame(
            "response.content_part.added",
            json!({
                "item_id": id,
                "output_index": index,
                "content_index": 0,
                "part": { "type": part_type, "text": "" },
            }),
        ));
        out
    }

    /// Emit the `.done` events for the streaming item and archive it.
    fn close_open(&mut self) -> Vec<String> {
        let Some(item) = self.current.take() else {
            return Vec::new();
        };
        let (done_name, part_type) = match item.kind {
            ItemKind::Reasoning => ("response.reasoning_text.done", "reasoning_text"),
            ItemKind::Message => ("response.output_text.done", "output_text"),
        };
        let mut out = vec![self.frame(
            done_name,
            json!({
                "item_id": item.id,
                "output_index": item.index,
                "content_index": 0,
                "text": item.text,
            }),
        )];
        out.push(self.frame(
            "response.content_part.done",
            json!({
                "item_id": item.id,
                "output_index": item.index,
                "content_index": 0,
                "part": { "type": part_type, "text": item.text },
            }),
        ));
        let done = item.json("completed", true);
        out.push(self.frame(
            "response.output_item.done",
            json!({ "output_index": item.index, "item": done.clone() }),
        ));
        self.done_items.push(done);
        out
    }

    /// The text delta for whichever item is streaming.
    fn delta_frame(&mut self, kind: ItemKind, text: &str) -> Vec<String> {
        let mut out = self.switch_to(kind);
        let current = self.current.as_mut().expect("switch_to opened an item");
        current.text.push_str(text);
        let (id, index) = (current.id.clone(), current.index);
        let delta_name = match kind {
            ItemKind::Reasoning => "response.reasoning_text.delta",
            ItemKind::Message => "response.output_text.delta",
        };
        out.push(self.frame(
            delta_name,
            json!({
                "item_id": id,
                "output_index": index,
                "content_index": 0,
                "delta": text,
            }),
        ));
        out
    }

    /// Feed one parsed `chat.completion.chunk`.
    pub(crate) fn on_chunk(&mut self, chunk: &Value) -> Vec<String> {
        let mut out = Vec::new();
        if let Some(u) = chunk.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(json!({
                "input_tokens": u.get("prompt_tokens").and_then(Value::as_i64).unwrap_or(0),
                "output_tokens": u.get("completion_tokens").and_then(Value::as_i64).unwrap_or(0),
                "total_tokens": u.get("total_tokens").and_then(Value::as_i64).unwrap_or(0),
            }));
        }
        // Both spellings: LiteLLM normalises to `reasoning_content`, a vLLM
        // server straight behind the gateway sends `reasoning`.
        let reasoning = chunk
            .pointer("/choices/0/delta/reasoning_content")
            .or_else(|| chunk.pointer("/choices/0/delta/reasoning"))
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty());
        if let Some(text) = reasoning {
            out.extend(self.delta_frame(ItemKind::Reasoning, text));
        }
        if let Some(text) = chunk
            .pointer("/choices/0/delta/content")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty())
        {
            out.extend(self.delta_frame(ItemKind::Message, text));
        }
        if let Some(calls) = chunk
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
        {
            self.absorb_tool_calls(calls);
        }
        if let Some(reason) = chunk
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            self.finish_reason = reason.to_string();
        }
        out
    }

    /// Merge streamed tool-call fragments by index, the way chat sends them.
    fn absorb_tool_calls(&mut self, calls: &[Value]) {
        for call in calls {
            let idx = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
            while self.tool_calls.len() <= idx {
                self.tool_calls
                    .push(json!({ "call_id": "", "name": "", "arguments": "" }));
            }
            let slot = &mut self.tool_calls[idx];
            if let Some(id) = call.get("id").and_then(Value::as_str) {
                slot["call_id"] = json!(id);
            }
            if let Some(n) = call.pointer("/function/name").and_then(Value::as_str) {
                slot["name"] = json!(n);
            }
            if let Some(a) = call.pointer("/function/arguments").and_then(Value::as_str) {
                let existing = slot["arguments"].as_str().unwrap_or("").to_string();
                slot["arguments"] = json!(format!("{existing}{a}"));
            }
        }
    }

    /// Close the stream. Safe to call twice; the second call emits nothing.
    pub(crate) fn finish(&mut self) -> Vec<String> {
        if self.completed {
            return Vec::new();
        }
        self.completed = true;
        let mut out = self.close_open();
        out.extend(self.start());

        let mut output: Vec<Value> = self.done_items.clone();
        for (i, call) in self.tool_calls.clone().iter().enumerate() {
            let index = self.next_index;
            self.next_index += 1;
            let item = json!({
                "id": format!("fc_{}_{i}", self.request_id),
                "type": "function_call",
                "status": "completed",
                "call_id": call.get("call_id").cloned().unwrap_or(json!("")),
                "name": call.get("name").cloned().unwrap_or(json!("")),
                "arguments": call.get("arguments").cloned().unwrap_or(json!("{}")),
            });
            output.push(item.clone());
            out.push(self.frame(
                "response.output_item.added",
                json!({ "output_index": index, "item": item }),
            ));
            out.push(self.frame(
                "response.output_item.done",
                json!({ "output_index": index, "item": item }),
            ));
        }

        let status = status_for(&self.finish_reason);
        let envelope = self.envelope(status, Value::Array(output));
        out.push(self.frame("response.completed", json!({ "response": envelope })));
        out
    }

    /// Close a stream whose upstream broke off mid-answer. Ends with
    /// `response.failed` rather than `response.completed`: a caller that
    /// checks the terminal event alone must not take a truncated answer for
    /// a whole one. Items already streamed are closed and echoed so the
    /// partial text is not lost. Shares `finish`'s run-once flag.
    pub(crate) fn fail(&mut self, message: &str) -> Vec<String> {
        if self.completed {
            return Vec::new();
        }
        self.completed = true;
        let mut out = self.close_open();
        out.extend(self.start());
        let mut envelope = self.envelope("failed", Value::Array(self.done_items.clone()));
        envelope["error"] = json!({ "code": "server_error", "message": message });
        out.push(self.frame("response.failed", json!({ "response": envelope })));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_string_input_becomes_one_user_message() {
        let chat = to_chat_request(&json!({ "model": "m", "input": "hello" }));
        assert_eq!(chat["model"], "m");
        assert_eq!(
            chat["messages"],
            json!([{ "role": "user", "content": "hello" }])
        );
    }

    #[test]
    fn instructions_lead_as_a_system_message() {
        let chat = to_chat_request(&json!({
            "input": [{ "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] }],
            "instructions": "be terse",
        }));
        assert_eq!(
            chat["messages"],
            json!([
                { "role": "system", "content": "be terse" },
                { "role": "user", "content": "hi" },
            ])
        );
        // `instructions` must not also survive as a top-level chat field.
        assert!(chat.get("instructions").is_none());
    }

    #[test]
    fn a_replayed_turn_round_trips_through_both_content_spellings() {
        // A client sends its own previous answer back as `output_text`.
        let chat = to_chat_request(&json!({
            "input": [
                { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": "hi" }] },
                { "type": "message", "role": "assistant", "content": [{ "type": "output_text", "text": "hello" }] },
                { "type": "message", "role": "user", "content": "again" },
            ],
        }));
        assert_eq!(
            chat["messages"],
            json!([
                { "role": "user", "content": "hi" },
                { "role": "assistant", "content": "hello" },
                { "role": "user", "content": "again" },
            ])
        );
    }

    #[test]
    fn tool_calls_and_their_output_become_chat_turns() {
        let chat = to_chat_request(&json!({
            "input": [
                { "type": "function_call", "call_id": "c1", "name": "lookup", "arguments": "{\"q\":1}" },
                { "type": "function_call_output", "call_id": "c1", "output": "42" },
            ],
        }));
        let msgs = chat["messages"].as_array().expect("messages");
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[0]["tool_calls"][0]["id"], "c1");
        assert_eq!(msgs[0]["tool_calls"][0]["function"]["name"], "lookup");
        assert_eq!(
            msgs[1],
            json!({ "role": "tool", "tool_call_id": "c1", "content": "42" })
        );
    }

    #[test]
    fn an_image_part_keeps_the_multimodal_shape() {
        let chat = to_chat_request(&json!({
            "input": [{
                "type": "message", "role": "user",
                "content": [
                    { "type": "input_text", "text": "what is this?" },
                    { "type": "input_image", "image_url": "data:image/png;base64,AAA" },
                ],
            }],
        }));
        assert_eq!(
            chat["messages"][0]["content"],
            json!([
                { "type": "text", "text": "what is this?" },
                { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAA" } },
            ])
        );
    }

    #[test]
    fn reasoning_items_are_dropped_not_flattened_into_content() {
        let chat = to_chat_request(&json!({
            "input": [
                { "type": "reasoning", "summary": [{ "type": "summary_text", "text": "thinking" }] },
                { "type": "message", "role": "user", "content": "go" },
            ],
        }));
        assert_eq!(
            chat["messages"],
            json!([{ "role": "user", "content": "go" }])
        );
    }

    #[test]
    fn renamed_and_carried_through_fields() {
        let chat = to_chat_request(&json!({
            "model": "m", "input": "x", "max_output_tokens": 64,
            "temperature": 0.3, "stream": true, "top_p": 0.9,
            "text": { "format": { "type": "json_object" } },
        }));
        assert_eq!(chat["max_tokens"], 64);
        assert!(chat.get("max_output_tokens").is_none());
        assert_eq!(chat["response_format"], json!({ "type": "json_object" }));
        // Unknown-to-the-shim knobs still reach the upstream.
        assert_eq!(chat["temperature"], 0.3);
        assert_eq!(chat["top_p"], 0.9);
        assert_eq!(chat["stream"], true);
    }

    #[test]
    fn tools_are_renested_and_hosted_ones_dropped() {
        let chat = to_chat_request(&json!({
            "input": "x",
            "tools": [
                { "type": "function", "name": "f", "description": "d", "parameters": { "type": "object" } },
                { "type": "web_search" },
            ],
        }));
        assert_eq!(chat["tools"].as_array().expect("tools").len(), 1);
        assert_eq!(
            chat["tools"][0],
            json!({
                "type": "function",
                "function": { "name": "f", "description": "d", "parameters": { "type": "object" } },
            })
        );
    }

    #[test]
    fn stateful_fields_are_named_so_the_caller_knows_which_to_drop() {
        assert_eq!(unsupported_field(&json!({ "store": true })), Some("store"));
        assert_eq!(
            unsupported_field(&json!({ "previous_response_id": "resp_1" })),
            Some("previous_response_id")
        );
        assert_eq!(
            unsupported_field(&json!({ "background": true })),
            Some("background")
        );
        // The harmless spellings are not refused.
        assert_eq!(unsupported_field(&json!({ "store": false })), None);
        assert_eq!(
            unsupported_field(&json!({ "previous_response_id": Value::Null })),
            None
        );
        assert_eq!(unsupported_field(&json!({ "input": "hi" })), None);
    }

    fn chat_reply(content: &str, finish: &str) -> Value {
        json!({
            "id": "chatcmpl-1", "created": 1700, "model": "glm-5-3",
            "choices": [{ "index": 0, "finish_reason": finish, "message": {
                "role": "assistant", "content": content } }],
            "usage": { "prompt_tokens": 11, "completion_tokens": 3, "total_tokens": 14 },
        })
    }

    #[test]
    fn a_reply_becomes_one_output_text_item() {
        let out = from_chat_response(&chat_reply("hello", "stop"), "req1");
        assert_eq!(out["id"], "resp_req1");
        assert_eq!(out["object"], "response");
        assert_eq!(out["status"], "completed");
        assert_eq!(out["model"], "glm-5-3");
        assert_eq!(out["output"][0]["type"], "message");
        assert_eq!(out["output"][0]["content"][0]["type"], "output_text");
        assert_eq!(out["output"][0]["content"][0]["text"], "hello");
        // Usage is renamed, and the numbers are the same tokens.
        assert_eq!(
            out["usage"],
            json!({
            "input_tokens": 11, "output_tokens": 3, "total_tokens": 14 })
        );
    }

    #[test]
    fn a_truncated_reply_is_incomplete_not_completed() {
        let out = from_chat_response(&chat_reply("half an ans", "length"), "req1");
        assert_eq!(out["status"], "incomplete");
        assert_eq!(out["incomplete_details"]["reason"], "max_output_tokens");
    }

    #[test]
    fn tool_calls_become_function_call_items() {
        let chat = json!({
            "model": "m", "created": 1,
            "choices": [{ "finish_reason": "tool_calls", "message": {
                "role": "assistant", "content": Value::Null,
                "tool_calls": [{ "id": "c1", "type": "function",
                    "function": { "name": "lookup", "arguments": "{}" } }] } }],
        });
        let out = from_chat_response(&chat, "req1");
        assert_eq!(out["output"][0]["type"], "function_call");
        assert_eq!(out["output"][0]["call_id"], "c1");
        assert_eq!(out["output"][0]["name"], "lookup");
    }

    /// Parse the frames a translator emitted back into (event, data) pairs.
    fn parse(frames: &[String]) -> Vec<(String, Value)> {
        frames
            .iter()
            .map(|f| {
                let mut lines = f.lines();
                let ev = lines
                    .next()
                    .expect("event line")
                    .trim_start_matches("event: ")
                    .to_string();
                let data = lines
                    .next()
                    .expect("data line")
                    .trim_start_matches("data: ");
                (ev, serde_json::from_str(data).expect("valid json"))
            })
            .collect()
    }

    fn delta(content: &str) -> Value {
        json!({ "choices": [{ "index": 0, "delta": { "content": content }, "finish_reason": Value::Null }] })
    }

    #[test]
    fn a_text_stream_emits_the_documented_event_order() {
        let mut t = StreamTranslator::new("req1", "glm-5-3");
        let mut frames = t.on_chunk(&delta("Hel"));
        frames.extend(t.on_chunk(&delta("lo")));
        frames.extend(t.on_chunk(&json!({
            "choices": [{ "index": 0, "delta": {}, "finish_reason": "stop" }] })));
        frames.extend(t.finish());
        let events: Vec<String> = parse(&frames).into_iter().map(|(e, _)| e).collect();
        assert_eq!(
            events,
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added",
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
    }

    #[test]
    fn a_broken_upstream_stream_ends_failed_not_completed() {
        let mut t = StreamTranslator::new("req1", "m");
        let mut frames = t.on_chunk(&delta("partial"));
        frames.extend(t.fail("upstream stream ended early"));
        // A late `finish` (e.g. from a caller that always closes) adds nothing.
        frames.extend(t.finish());
        let parsed = parse(&frames);
        let events: Vec<&str> = parsed.iter().map(|(e, _)| e.as_str()).collect();
        assert!(!events.contains(&"response.completed"));
        assert_eq!(events.last(), Some(&"response.failed"));
        let (_, failed) = parsed.last().unwrap();
        assert_eq!(failed["response"]["status"], "failed");
        assert_eq!(failed["response"]["error"]["code"], "server_error");
        // The text that did stream is closed and echoed, not dropped.
        assert_eq!(
            failed["response"]["output"][0]["content"][0]["text"],
            "partial"
        );
    }

    #[test]
    fn a_streamed_request_asks_the_chat_upstream_for_usage() {
        let chat = to_chat_request(&json!({ "model": "m", "input": "hi", "stream": true }));
        assert_eq!(chat["stream_options"]["include_usage"], true);
        let chat = to_chat_request(&json!({ "model": "m", "input": "hi" }));
        assert!(chat.get("stream_options").is_none());
    }

    #[test]
    fn stream_events_carry_indices_and_a_rising_sequence() {
        let mut t = StreamTranslator::new("req1", "m");
        let mut frames = t.on_chunk(&delta("hi"));
        frames.extend(t.finish());
        let parsed = parse(&frames);
        for (i, (_, d)) in parsed.iter().enumerate() {
            assert_eq!(d["sequence_number"], i as u64, "sequence must not skip");
        }
        let (_, d) = parsed
            .iter()
            .find(|(e, _)| e == "response.output_text.delta")
            .expect("delta");
        assert_eq!(d["delta"], "hi");
        assert_eq!(d["item_id"], "msg_req1_0");
        assert_eq!(d["output_index"], 0);
        assert_eq!(d["content_index"], 0);
    }

    #[test]
    fn the_completed_event_carries_the_whole_text_and_usage() {
        let mut t = StreamTranslator::new("req1", "m");
        let mut frames = t.on_chunk(&delta("Hel"));
        frames.extend(t.on_chunk(&delta("lo")));
        frames.extend(t.on_chunk(&json!({
            "choices": [{ "delta": {}, "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7 } })));
        frames.extend(t.finish());
        let parsed = parse(&frames);
        let (_, done) = parsed
            .iter()
            .find(|(e, _)| e == "response.completed")
            .expect("completed");
        assert_eq!(done["response"]["status"], "completed");
        assert_eq!(done["response"]["output"][0]["content"][0]["text"], "Hello");
        assert_eq!(done["response"]["usage"]["total_tokens"], 7);
    }

    #[test]
    fn a_tool_call_only_reply_opens_no_message_item() {
        let mut t = StreamTranslator::new("req1", "m");
        let mut frames = t.on_chunk(&json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 0, "id": "c1", "function": { "name": "look", "arguments": "{\"a\":" } }] } }] }));
        frames.extend(t.on_chunk(&json!({ "choices": [{ "delta": { "tool_calls": [
            { "index": 0, "function": { "arguments": "1}" } }] }, "finish_reason": "tool_calls" }] })));
        frames.extend(t.finish());
        let parsed = parse(&frames);
        let events: Vec<&str> = parsed.iter().map(|(e, _)| e.as_str()).collect();
        assert!(
            !events.contains(&"response.content_part.added"),
            "no text, so no message item"
        );
        let (_, added) = parsed
            .iter()
            .find(|(e, _)| e == "response.output_item.added")
            .expect("item");
        assert_eq!(added["item"]["type"], "function_call");
        // Argument fragments are joined in order.
        assert_eq!(added["item"]["arguments"], "{\"a\":1}");
    }

    fn reasoning_delta(text: &str) -> Value {
        json!({ "choices": [{ "index": 0, "delta": { "reasoning": text }, "finish_reason": Value::Null }] })
    }

    #[test]
    fn a_reasoning_model_streams_its_scratchpad_as_a_reasoning_item() {
        // glm-5-3 sends `delta.reasoning` with no `content` until it stops
        // thinking; reading only `content` showed the client nothing.
        let mut t = StreamTranslator::new("req1", "glm-5-3");
        let mut frames = t.on_chunk(&reasoning_delta("thinking"));
        frames.extend(t.on_chunk(&delta("answer")));
        frames.extend(t.finish());
        let parsed = parse(&frames);
        let events: Vec<&str> = parsed.iter().map(|(e, _)| e.as_str()).collect();
        assert_eq!(
            events,
            vec![
                "response.created",
                "response.in_progress",
                "response.output_item.added", // reasoning
                "response.content_part.added",
                "response.reasoning_text.delta",
                "response.reasoning_text.done", // closed when content starts
                "response.content_part.done",
                "response.output_item.done",
                "response.output_item.added", // message
                "response.content_part.added",
                "response.output_text.delta",
                "response.output_text.done",
                "response.content_part.done",
                "response.output_item.done",
                "response.completed",
            ]
        );
        let (_, done) = parsed
            .iter()
            .find(|(e, _)| e == "response.completed")
            .expect("completed");
        let output = done["response"]["output"].as_array().expect("output");
        assert_eq!(output[0]["type"], "reasoning");
        assert_eq!(output[0]["content"][0]["text"], "thinking");
        assert_eq!(output[1]["type"], "message");
        assert_eq!(output[1]["content"][0]["text"], "answer");
    }

    #[test]
    fn the_two_reasoning_spellings_are_both_read() {
        // LiteLLM normalises to `reasoning_content`; a vLLM server behind the
        // gateway sends `reasoning`.
        for key in ["reasoning", "reasoning_content"] {
            let mut t = StreamTranslator::new("req1", "m");
            let chunk = json!({ "choices": [{ "delta": { key: "hmm" } }] });
            let frames = t.on_chunk(&chunk);
            let parsed = parse(&frames);
            assert!(
                parsed
                    .iter()
                    .any(|(e, d)| e == "response.reasoning_text.delta" && d["delta"] == "hmm"),
                "{key} should produce a reasoning delta"
            );
        }
    }

    #[test]
    fn reasoning_only_output_still_completes_with_the_item() {
        // A budget spent entirely on thinking: no message item, but the
        // reasoning must not vanish.
        let mut t = StreamTranslator::new("req1", "m");
        let mut frames = t.on_chunk(&reasoning_delta("thought"));
        frames.extend(t.on_chunk(&json!({
            "choices": [{ "delta": {}, "finish_reason": "length" }] })));
        frames.extend(t.finish());
        let parsed = parse(&frames);
        let (_, done) = parsed
            .iter()
            .find(|(e, _)| e == "response.completed")
            .expect("completed");
        assert_eq!(done["response"]["status"], "incomplete");
        assert_eq!(done["response"]["output"][0]["type"], "reasoning");
        assert_eq!(
            done["response"]["output"][0]["content"][0]["text"],
            "thought"
        );
    }

    #[test]
    fn a_non_streaming_reply_carries_reasoning_as_its_own_item() {
        let chat = json!({
            "model": "m", "created": 1,
            "choices": [{ "finish_reason": "stop", "message": {
                "role": "assistant", "reasoning_content": "thought", "content": "answer" } }],
        });
        let out = from_chat_response(&chat, "req1");
        assert_eq!(out["output"][0]["type"], "reasoning");
        assert_eq!(out["output"][0]["content"][0]["text"], "thought");
        assert_eq!(out["output"][1]["type"], "message");
        assert_eq!(out["output"][1]["content"][0]["text"], "answer");
    }

    #[test]
    fn interleaved_reasoning_gets_a_fresh_item_each_segment() {
        // reasoning -> content -> reasoning: each segment is its own item with
        // its own id and index. Reusing one id at two output indices broke
        // clients that key UI state by item_id.
        let mut t = StreamTranslator::new("req1", "m");
        let mut frames = t.on_chunk(&reasoning_delta("first thought"));
        frames.extend(t.on_chunk(&delta("partial answer")));
        frames.extend(t.on_chunk(&reasoning_delta("second thought")));
        frames.extend(t.finish());
        let parsed = parse(&frames);

        let added: Vec<&Value> = parsed
            .iter()
            .filter(|(e, _)| e == "response.output_item.added")
            .map(|(_, d)| &d["item"])
            .collect();
        assert_eq!(added.len(), 3);
        assert_eq!(added[0]["id"], "rs_req1_0");
        assert_eq!(added[1]["id"], "msg_req1_1");
        assert_eq!(added[2]["id"], "rs_req1_2");

        let (_, done) = parsed
            .iter()
            .find(|(e, _)| e == "response.completed")
            .expect("completed");
        let output = done["response"]["output"].as_array().expect("output");
        assert_eq!(output.len(), 3, "three segments, three items");
        assert_eq!(output[0]["content"][0]["text"], "first thought");
        assert_eq!(output[1]["content"][0]["text"], "partial answer");
        assert_eq!(output[2]["content"][0]["text"], "second thought");
        // Every added item has a matching done at the same output_index.
        let dones = parsed
            .iter()
            .filter(|(e, _)| e == "response.output_item.done")
            .count();
        assert_eq!(dones, 3);
    }

    #[test]
    fn sse_lines_survive_a_chunk_split_inside_a_multibyte_character() {
        // "猫" is three bytes; split the stream between its first and second.
        let frame = "data: {\"text\":\"猫\"}\n".as_bytes();
        let (a, b) = frame.split_at(frame.iter().position(|c| *c >= 0x80).unwrap() + 1);

        let mut buffer: Vec<u8> = Vec::new();
        buffer.extend_from_slice(a);
        assert!(
            drain_sse_data_lines(&mut buffer).is_empty(),
            "no complete line yet"
        );
        buffer.extend_from_slice(b);
        let lines = drain_sse_data_lines(&mut buffer);
        assert_eq!(lines, vec!["{\"text\":\"猫\"}".to_string()]);
        assert!(buffer.is_empty());
    }

    #[test]
    fn sse_draining_skips_blank_and_non_data_lines_and_keeps_partials() {
        let mut buffer = b"event: ping\n\ndata: one\ndata: tw".to_vec();
        assert_eq!(drain_sse_data_lines(&mut buffer), vec!["one".to_string()]);
        // The partial "data: tw" stays buffered for the next chunk.
        assert_eq!(buffer, b"data: tw".to_vec());
    }

    #[test]
    fn finish_is_idempotent_so_a_closed_stream_emits_nothing_twice() {
        let mut t = StreamTranslator::new("req1", "m");
        let _ = t.on_chunk(&delta("hi"));
        assert!(!t.finish().is_empty());
        assert!(t.finish().is_empty());
    }
}
