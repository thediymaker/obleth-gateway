//! Bounded, incremental decoding for monitoring only. Failure never affects the
//! response stream; once invalid/oversized, a monitor cannot resume on a suffix.

use serde_json::{json, Value};

use crate::sse::{self, Piece, Splitter};

const MAX_BYTES: usize = 512 * 1024;
/// Streamed tool calls are indexed by the upstream; an index past this is
/// treated as invalid output rather than allocated.
const MAX_TOOL_CALLS: usize = 64;

/// One streamed tool call, assembled from its deltas.
#[derive(Default)]
struct ToolCall {
    id: String,
    name: String,
    arguments: String,
}

pub(crate) struct OutputMonitor {
    sse: bool,
    received: usize,
    /// A JSON body so far.
    pending: Vec<u8>,
    /// An SSE stream, split into events.
    events: Splitter,
    channels: [String; 4],
    tool_calls: Vec<ToolCall>,
    finish_reason: Option<String>,
    failed: bool,
}

impl OutputMonitor {
    pub(crate) fn new(sse: bool) -> Self {
        Self {
            sse,
            received: 0,
            pending: Vec::new(),
            events: Splitter::new(MAX_BYTES),
            channels: Default::default(),
            tool_calls: Vec::new(),
            finish_reason: None,
            failed: false,
        }
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        if self.failed {
            return;
        }
        self.received = self.received.saturating_add(chunk.len());
        if self.received > MAX_BYTES {
            self.fail();
            return;
        }
        if !self.sse {
            self.pending.extend_from_slice(chunk);
            return;
        }
        self.events.push(chunk);
        while !self.failed {
            let value = match self.events.next() {
                None => return,
                Some(piece) => read_event(piece),
            };
            self.dispatch(value);
        }
    }

    fn fail(&mut self) {
        self.failed = true;
        self.pending = Vec::new();
        self.events = Splitter::new(0);
        self.channels = Default::default();
        self.tool_calls = Vec::new();
        self.finish_reason = None;
    }

    /// Take in one event read by [`read_event`].
    fn dispatch(&mut self, value: Result<Option<Value>, ()>) {
        let value = match value {
            Ok(Some(value)) => value,
            Ok(None) => return,
            Err(()) => {
                self.fail();
                return;
            }
        };
        // Keep channels separate until completion: inserting a separator between
        // deltas would hide a keyword split across tokens, while interleaving
        // reasoning and content could manufacture or obscure one.
        for (channel, pointer) in self.channels.iter_mut().zip([
            "/choices/0/delta/content",
            "/choices/0/delta/reasoning_content",
            "/choices/0/delta/reasoning",
            "/choices/0/delta/provider_specific_fields/reasoning",
        ]) {
            if let Some(text) = value.pointer(pointer).and_then(Value::as_str) {
                channel.push_str(text);
            }
        }
        if let Some(calls) = value
            .pointer("/choices/0/delta/tool_calls")
            .and_then(Value::as_array)
        {
            for call in calls {
                let index = call.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
                if index >= MAX_TOOL_CALLS {
                    self.fail();
                    return;
                }
                if self.tool_calls.len() <= index {
                    self.tool_calls.resize_with(index + 1, ToolCall::default);
                }
                let slot = &mut self.tool_calls[index];
                if let Some(id) = call.get("id").and_then(Value::as_str) {
                    slot.id = id.to_string();
                }
                if let Some(name) = call.pointer("/function/name").and_then(Value::as_str) {
                    slot.name.push_str(name);
                }
                if let Some(args) = call.pointer("/function/arguments").and_then(Value::as_str) {
                    slot.arguments.push_str(args);
                }
            }
        }
        if let Some(reason) = value
            .pointer("/choices/0/finish_reason")
            .and_then(Value::as_str)
        {
            self.finish_reason = Some(reason.to_string());
        }
    }

    pub(crate) fn finish(mut self) -> Result<Value, &'static str> {
        if self.failed {
            return Err("invalid or oversized output");
        }
        if !self.sse {
            return serde_json::from_slice(&self.pending).map_err(|_| "invalid JSON output");
        }
        if let Some(piece) = self.events.finish() {
            let value = read_event(piece);
            self.dispatch(value);
        }
        if self.failed {
            return Err("invalid SSE output");
        }
        let mut message = json!({
            "content": self.channels[0], "reasoning_content": self.channels[1],
            "reasoning": self.channels[2],
            "provider_specific_fields": {"reasoning": self.channels[3]}
        });
        if !self.tool_calls.is_empty() {
            message["tool_calls"] = self
                .tool_calls
                .iter()
                .map(|c| {
                    json!({"id": c.id, "type": "function",
                        "function": {"name": c.name, "arguments": c.arguments}})
                })
                .collect();
        }
        Ok(json!({"choices": [{"message": message, "finish_reason": self.finish_reason}]}))
    }
}

/// One event's chunk: `None` for an event with no data or `[DONE]`, and an
/// error for an oversized event or data that is not JSON.
fn read_event(piece: Piece<'_>) -> Result<Option<Value>, ()> {
    let Piece::Event(event) = piece else {
        return Err(());
    };
    let data = sse::data_payload(event);
    if data.is_empty() || &data[..] == b"[DONE]" {
        return Ok(None);
    }
    serde_json::from_slice(&data).map(Some).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_survives_every_byte_boundary_and_interleaved_reasoning() {
        let wire = concat!(
            ": heartbeat\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"sec\",\"reasoning_content\":\"ré\"}}]}\r\n\r\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"ret\",\"reasoning_content\":\"flexion\"}}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mut monitor = OutputMonitor::new(true);
        for byte in wire.as_bytes() {
            monitor.push(&[*byte]);
        }
        let result = monitor.finish().unwrap();
        assert_eq!(
            result.pointer("/choices/0/message/content").unwrap(),
            "secret"
        );
        assert_eq!(
            result
                .pointer("/choices/0/message/reasoning_content")
                .unwrap(),
            "réflexion"
        );
    }

    #[test]
    fn multiline_event_and_final_unterminated_event() {
        let mut monitor = OutputMonitor::new(true);
        monitor.push(b"data: {\"choices\":\ndata: [{\"delta\":{\"content\":\"hello\"}}]}");
        assert_eq!(
            monitor.finish().unwrap()["choices"][0]["message"]["content"],
            "hello"
        );
    }

    #[test]
    fn overflow_and_malformed_events_never_scan_a_suffix() {
        for wire in [vec![b'x'; MAX_BYTES + 1], b"data: invalid\n\n".to_vec()] {
            let mut monitor = OutputMonitor::new(true);
            monitor.push(&wire);
            monitor.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"clean\"}}]}\n\n");
            assert!(monitor.finish().is_err());
        }
    }

    #[test]
    fn streamed_tool_calls_and_the_finish_reason_are_assembled() {
        let wire = concat!(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"bash\",\"arguments\":\"{\\\"cmd\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\":\\\"ls\\\"}\"}}]}}]}\n\n",
            "data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n",
            "data: [DONE]\n\n"
        );
        let mut monitor = OutputMonitor::new(true);
        monitor.push(wire.as_bytes());
        let done = monitor.finish().unwrap();
        let call = &done["choices"][0]["message"]["tool_calls"][0];
        assert_eq!(call["id"], "call_1");
        assert_eq!(call["function"]["name"], "bash");
        assert_eq!(call["function"]["arguments"], "{\"cmd\":\"ls\"}");
        assert_eq!(done["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn json_completion_remains_unchanged() {
        let body = json!({"choices":[{"message":{"content":"hello","reasoning":"why"}}]});
        let mut monitor = OutputMonitor::new(false);
        for byte in serde_json::to_vec(&body).unwrap() {
            monitor.push(&[byte]);
        }
        assert_eq!(monitor.finish().unwrap(), body);
    }
}
