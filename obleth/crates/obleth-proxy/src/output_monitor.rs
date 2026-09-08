//! Bounded, incremental decoding for monitoring only. Failure never affects the
//! response stream; once invalid/oversized, a monitor cannot resume on a suffix.

use serde_json::{json, Value};

const MAX_BYTES: usize = 512 * 1024;

pub(crate) struct OutputMonitor {
    sse: bool,
    received: usize,
    pending: Vec<u8>,
    event: Vec<u8>,
    channels: [String; 4],
    failed: bool,
}

impl OutputMonitor {
    pub(crate) fn new(sse: bool) -> Self {
        Self {
            sse,
            received: 0,
            pending: Vec::new(),
            event: Vec::new(),
            channels: Default::default(),
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
        for &byte in chunk {
            if byte == b'\n' {
                self.line();
                if self.failed {
                    return;
                }
            } else {
                self.pending.push(byte);
            }
        }
    }

    fn fail(&mut self) {
        self.failed = true;
        self.pending = Vec::new();
        self.event = Vec::new();
        self.channels = Default::default();
    }

    fn line(&mut self) {
        let mut line = std::mem::take(&mut self.pending);
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        if line.is_empty() {
            self.dispatch();
        } else if let Some(data) = line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            if !self.event.is_empty() {
                self.event.push(b'\n');
            }
            self.event.extend_from_slice(data);
        }
    }

    fn dispatch(&mut self) {
        let event = std::mem::take(&mut self.event);
        if event.is_empty() || event == b"[DONE]" {
            return;
        }
        let Ok(value) = serde_json::from_slice::<Value>(&event) else {
            self.fail();
            return;
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
    }

    pub(crate) fn finish(mut self) -> Result<Value, &'static str> {
        if self.failed {
            return Err("invalid or oversized output");
        }
        if !self.sse {
            return serde_json::from_slice(&self.pending).map_err(|_| "invalid JSON output");
        }
        if !self.pending.is_empty() {
            self.line();
        }
        self.dispatch();
        if self.failed {
            return Err("invalid SSE output");
        }
        Ok(json!({"choices": [{"message": {
            "content": self.channels[0], "reasoning_content": self.channels[1],
            "reasoning": self.channels[2],
            "provider_specific_fields": {"reasoning": self.channels[3]}
        }}]}))
    }
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
    fn json_completion_remains_unchanged() {
        let body = json!({"choices":[{"message":{"content":"hello","reasoning":"why"}}]});
        let mut monitor = OutputMonitor::new(false);
        for byte in serde_json::to_vec(&body).unwrap() {
            monitor.push(&[byte]);
        }
        assert_eq!(monitor.finish().unwrap(), body);
    }
}
