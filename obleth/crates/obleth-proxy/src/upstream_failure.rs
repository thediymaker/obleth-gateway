//! Failures an upstream reports inside a successful response.
//!
//! Once an OpenAI-compatible server has sent `200` and started a stream, the
//! only way left to report a failure is in the body: an SSE event carrying an
//! `error` object (what vLLM and OpenAI send when generation fails after the
//! stream opened), a typed `error` or `response.failed` event, or a choice
//! that ends with `finish_reason` `error` or `abort` (the server gave up on
//! the request). A buffered JSON response can likewise be a `200` whose body
//! is an error object.
//!
//! Such a response is a failed call. The gateway still passes its bytes
//! through untouched, but records it with an error status, bills only what
//! was produced, never caches it, and keeps it out of the completion-length
//! averages.
//!
//! A stream can also stop short with no word from the upstream at all: its
//! connection breaks or goes silent, or it closes before its terminal event.
//! The gateway then tells the client itself, with [`stream_cut_event`], and
//! aborts the response, so the cut is never mistaken for a whole answer.

use std::time::Duration;

use bytes::Bytes;
use serde_json::Value;

/// Status recorded for a failure that carries no HTTP code of its own.
pub(crate) const DEFAULT_FAILURE_STATUS: u16 = 502;

/// The SSE event that tells a client its stream stopped before the response
/// was complete, `elapsed` after the upstream call began. An error object in
/// OpenAI's shape: OpenAI's SDKs raise an `APIError` for any stream event
/// whose JSON carries an `error` key. It names no upstream detail.
pub(crate) fn stream_cut_event(elapsed: Duration) -> Bytes {
    let event = serde_json::json!({
        "error": {
            "message": format!(
                "upstream stream ended before the response was complete (after {} s)",
                elapsed.as_secs()
            ),
            "type": crate::proxy::GATEWAY_ERROR_TYPE,
            "code": DEFAULT_FAILURE_STATUS,
        }
    });
    Bytes::from(format!("data: {event}\n\n"))
}

/// The error that aborts a response after [`stream_cut_event`]: the body ends
/// without its clean close (no `[DONE]`, no final chunk), so a client reading
/// raw SSE sees a truncated stream too.
pub(crate) fn stream_cut_error() -> std::io::Error {
    std::io::Error::other("upstream stream ended before the response was complete")
}

/// How long the end of a cut stream waits between its event and the abort.
const CUT_FLUSH_PAUSE: Duration = Duration::from_millis(50);

/// The end of a response the upstream stopped short of, for after the request
/// has settled: `event` (the [`stream_cut_event`], unless the upstream's own
/// error event already went out), then the abort.
///
/// The server writes a body's chunks into a buffer and flushes it only once
/// the body has nothing ready; an error that follows a chunk at once aborts
/// the connection with the chunk still in that buffer. The pause before the
/// abort lets everything sent reach the client first.
pub(crate) fn stream_cut_tail(
    event: Option<Bytes>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> {
    async_stream::stream! {
        if let Some(event) = event {
            yield Ok(event);
        }
        tokio::time::sleep(CUT_FLUSH_PAUSE).await;
        yield Err(stream_cut_error());
    }
}

/// Whether bytes sent so far (their last few) end at an SSE event boundary,
/// so an event can follow them without joining the one before. Nothing sent
/// is a boundary.
pub(crate) fn at_event_boundary(sent_tail: &[u8]) -> bool {
    sent_tail.is_empty()
        || sent_tail.ends_with(b"\n\n")
        || sent_tail.ends_with(b"\r\n\r\n")
        || sent_tail.ends_with(b"\r\r")
}

/// Keep the last four bytes sent, for [`at_event_boundary`].
pub(crate) fn remember_sent(sent_tail: &mut Vec<u8>, chunk: &[u8]) {
    sent_tail.extend_from_slice(&chunk[chunk.len().saturating_sub(4)..]);
    let excess = sent_tail.len().saturating_sub(4);
    sent_tail.drain(..excess);
}

/// Error payloads are small; an SSE event or JSON body above this is
/// generated content, and is not inspected.
const MAX_INSPECTED_BYTES: usize = 64 * 1024;

/// Longest upstream message kept for logs and alerts.
const MAX_MESSAGE_CHARS: usize = 300;

/// A failure the upstream reported inside a non-error HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UpstreamFailure {
    /// The status to record: the error's own HTTP code when it carries one
    /// (400 to 599), otherwise [`DEFAULT_FAILURE_STATUS`].
    pub(crate) status: u16,
    /// The upstream's own description, shortened.
    pub(crate) message: String,
}

impl UpstreamFailure {
    fn new(status: Option<u16>, message: impl Into<String>) -> Self {
        Self {
            status: status.unwrap_or(DEFAULT_FAILURE_STATUS),
            message: shorten(message.into()),
        }
    }
}

/// The failure one decoded payload reports, if any: a whole JSON body, or the
/// `data:` of one SSE event.
///
/// - a top-level `error` that is set (an object or a message; `null` and
///   empty values are not errors),
/// - a typed `error` or `response.failed` event,
/// - a choice whose `finish_reason` is `error` or `abort`.
pub(crate) fn payload_failure(value: &Value) -> Option<UpstreamFailure> {
    let object = value.as_object()?;
    if let Some(error) = object.get("error").filter(|e| is_set(e)) {
        let status = error_status(error).or_else(|| error_status(value));
        return Some(UpstreamFailure::new(status, error_message(error)));
    }
    match object.get("type").and_then(Value::as_str) {
        Some("error") => {
            let message = object
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("upstream error event");
            return Some(UpstreamFailure::new(error_status(value), message));
        }
        Some("response.failed") => {
            let error = value.pointer("/response/error").filter(|e| is_set(e));
            return Some(UpstreamFailure::new(
                error.and_then(error_status),
                error.map_or_else(|| "response failed".to_string(), error_message),
            ));
        }
        _ => {}
    }
    object
        .get("choices")?
        .as_array()?
        .iter()
        .find_map(|choice| {
            let reason = choice.get("finish_reason")?.as_str()?;
            matches!(reason, "error" | "abort")
                .then(|| UpstreamFailure::new(None, format!("generation ended with `{reason}`")))
        })
}

fn is_set(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::String(s) => !s.trim().is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
        Value::Number(_) => true,
    }
}

/// An HTTP status the object carries in `code`, `status` or `status_code`,
/// as a number or a numeric string. OpenAI's own string codes
/// (`"server_error"`) are not statuses and are ignored.
fn error_status(value: &Value) -> Option<u16> {
    let object = value.as_object()?;
    ["code", "status", "status_code"].iter().find_map(|field| {
        let status = match object.get(*field)? {
            Value::Number(n) => n.as_u64()?,
            Value::String(s) => s.trim().parse().ok()?,
            _ => return None,
        };
        (400..=599).contains(&status).then_some(status as u16)
    })
}

fn error_message(error: &Value) -> String {
    match error {
        Value::String(s) => s.clone(),
        Value::Object(o) => o
            .get("message")
            .and_then(Value::as_str)
            .or_else(|| o.get("type").and_then(Value::as_str))
            .map_or_else(|| error.to_string(), str::to_string),
        other => other.to_string(),
    }
}

fn shorten(message: String) -> String {
    match message.char_indices().nth(MAX_MESSAGE_CHARS) {
        Some((at, _)) => format!("{}...", &message[..at]),
        None => message,
    }
}

/// Whether a payload could report a failure at all. Cheap enough to run on
/// every event of a long stream, so only the rare candidate is parsed.
fn may_report_failure(payload: &[u8]) -> bool {
    [&b"\"error\""[..], b"\"abort\"", b"response.failed"]
        .iter()
        .any(|needle| contains(payload, needle))
}

/// Whether a choice in this event reached its finish reason. Every chunk
/// names `finish_reason`, mostly as `null`; only one that sets it to a string
/// is parsed. (Quotes inside generated text are escaped, so text cannot pass
/// for the field.)
fn reports_finish(payload: &[u8]) -> bool {
    if !contains(payload, b"\"finish_reason\":\"") && !contains(payload, b"\"finish_reason\": \"") {
        return false;
    }
    serde_json::from_slice::<Value>(payload)
        .ok()
        .and_then(|v| {
            v.get("choices")?.as_array().map(|choices| {
                choices
                    .iter()
                    .any(|c| c.get("finish_reason").is_some_and(Value::is_string))
            })
        })
        .unwrap_or(false)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// Watches the raw bytes of a response for a reported failure, as they pass
/// through to the client. Never alters or holds the stream.
///
/// SSE bodies are read event by event; the first failure wins. A JSON body is
/// judged whole at [`FailureWatch::finish`]. Events and bodies past
/// [`MAX_INSPECTED_BYTES`] are generated content and are skipped.
///
/// A chat or text completion stream ([`FailureWatch::expecting_end`]) is
/// also watched for its end: `[DONE]`, or at least a finish reason for
/// upstreams that send no `[DONE]`.
pub(crate) struct FailureWatch {
    sse: bool,
    /// SSE: the line being read. JSON: the body so far.
    pending: Vec<u8>,
    /// SSE: `data:` of the event being read.
    data: Vec<u8>,
    /// SSE: the event was named `error` (`event: error`).
    error_event: bool,
    /// The current line or JSON body outgrew the cap.
    pending_oversized: bool,
    /// SSE: the current event outgrew the cap.
    event_oversized: bool,
    failure: Option<UpstreamFailure>,
    /// SSE: the stream is a chat or text completion, which has an end.
    expect_end: bool,
    /// SSE: `[DONE]` arrived.
    done: bool,
    /// SSE: a choice reached its finish reason.
    finished: bool,
}

impl FailureWatch {
    /// A watch for a response of this content type, or `None` for bodies that
    /// cannot carry a JSON error (audio, images, plain text).
    pub(crate) fn for_content_type(content_type: &str) -> Option<Self> {
        let content_type = content_type.to_ascii_lowercase();
        let sse = content_type.contains("text/event-stream");
        (sse || content_type.contains("json")).then(|| Self {
            sse,
            pending: Vec::new(),
            data: Vec::new(),
            error_event: false,
            pending_oversized: false,
            event_oversized: false,
            failure: None,
            expect_end: false,
            done: false,
            finished: false,
        })
    }

    /// Watch, too, that the stream reaches its end: it is a chat or text
    /// completion, which closes with `[DONE]`.
    pub(crate) fn expecting_end(mut self) -> Self {
        self.expect_end = true;
        self
    }

    /// An SSE stream rather than a JSON body.
    pub(crate) fn is_sse(&self) -> bool {
        self.sse
    }

    /// `[DONE]` has arrived: the response is whole, and nothing the
    /// connection does after it can cut it short.
    pub(crate) fn saw_done(&self) -> bool {
        self.done
    }

    /// After [`FailureWatch::finish`]: a stream expected to end that closed
    /// with neither `[DONE]` nor a finish reason, and no failure of the
    /// upstream's own to say why.
    pub(crate) fn ended_early(&self) -> bool {
        self.sse && self.expect_end && self.failure.is_none() && !self.done && !self.finished
    }

    pub(crate) fn push(&mut self, chunk: &[u8]) {
        if self.failure.is_some() {
            return;
        }
        if !self.sse {
            self.extend_pending(chunk);
            return;
        }
        let mut rest = chunk;
        while let Some(newline) = rest.iter().position(|&b| b == b'\n') {
            self.extend_pending(&rest[..newline]);
            self.end_line();
            if self.failure.is_some() {
                return;
            }
            rest = &rest[newline + 1..];
        }
        self.extend_pending(rest);
    }

    /// The failure the response reported, if any. Called once, when the body
    /// has ended.
    pub(crate) fn finish(&mut self) -> Option<UpstreamFailure> {
        if self.failure.is_some() {
            return self.failure.clone();
        }
        if self.sse {
            // A last event the upstream never terminated with a blank line.
            if !self.pending.is_empty() || self.pending_oversized {
                self.end_line();
            }
            self.dispatch();
            return self.failure.clone();
        }
        if self.pending_oversized || !may_report_failure(&self.pending) {
            return None;
        }
        self.failure = serde_json::from_slice::<Value>(&self.pending)
            .ok()
            .and_then(|body| payload_failure(&body));
        self.failure.clone()
    }

    fn extend_pending(&mut self, bytes: &[u8]) {
        if self.pending_oversized || bytes.is_empty() {
            return;
        }
        if self.pending.len() + bytes.len() > MAX_INSPECTED_BYTES {
            self.pending_oversized = true;
            self.pending = Vec::new();
            return;
        }
        self.pending.extend_from_slice(bytes);
    }

    fn end_line(&mut self) {
        if std::mem::take(&mut self.pending_oversized) {
            // Only generated content runs this long; the event it belongs to
            // is not an error report.
            self.pending.clear();
            self.event_oversized = true;
            return;
        }
        let line = match self.pending.split_last() {
            Some((b'\r', rest)) => rest,
            _ => &self.pending[..],
        };
        if line.is_empty() {
            self.pending.clear();
            self.dispatch();
            return;
        }
        if let Some(data) = line.strip_prefix(b"data:") {
            let data = data.strip_prefix(b" ").unwrap_or(data);
            if !self.event_oversized {
                if self.data.len() + data.len() + 1 > MAX_INSPECTED_BYTES {
                    self.event_oversized = true;
                    self.data.clear();
                } else {
                    if !self.data.is_empty() {
                        self.data.push(b'\n');
                    }
                    self.data.extend_from_slice(data);
                }
            }
        } else if let Some(name) = line.strip_prefix(b"event:") {
            if name.trim_ascii() == b"error" {
                self.error_event = true;
            }
        }
        // Comments (`:`), `id:` and `retry:` lines carry nothing to judge.
        self.pending.clear();
    }

    /// Judge the event read so far, then start the next one. The buffers are
    /// cleared rather than replaced, so a long stream reuses them.
    fn dispatch(&mut self) {
        let error_event = std::mem::take(&mut self.error_event);
        let oversized = std::mem::take(&mut self.event_oversized);
        let data = &self.data[..];
        if self.expect_end && !oversized {
            if data == b"[DONE]" {
                self.done = true;
            } else if !self.finished && reports_finish(data) {
                self.finished = true;
            }
        }
        let failure = if error_event {
            let reported = serde_json::from_slice::<Value>(data)
                .ok()
                .and_then(|v| payload_failure(&v));
            Some(reported.unwrap_or_else(|| {
                UpstreamFailure::new(None, String::from_utf8_lossy(data).trim().to_string())
            }))
        } else if oversized || data.is_empty() || data == b"[DONE]" || !may_report_failure(data) {
            None
        } else {
            serde_json::from_slice::<Value>(data)
                .ok()
                .and_then(|value| payload_failure(&value))
        };
        self.data.clear();
        if failure.is_some() {
            self.failure = failure;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn watch_sse(wire: &[u8]) -> Option<UpstreamFailure> {
        let mut watch = FailureWatch::for_content_type("text/event-stream; charset=utf-8")
            .expect("an SSE watch");
        watch.push(wire);
        watch.finish()
    }

    /// Every split of `wire` into two pushes reports the same thing.
    fn watch_sse_split_everywhere(wire: &str) -> Option<UpstreamFailure> {
        let whole = watch_sse(wire.as_bytes());
        for cut in 0..=wire.len() {
            let mut watch = FailureWatch::for_content_type("text/event-stream").unwrap();
            watch.push(&wire.as_bytes()[..cut]);
            watch.push(&wire.as_bytes()[cut..]);
            assert_eq!(watch.finish(), whole, "split at byte {cut}");
        }
        whole
    }

    fn content(text: &str) -> String {
        format!(
            "data: {}\n\n",
            json!({"object": "chat.completion.chunk",
                   "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]})
        )
    }

    #[test]
    fn an_openai_compatible_error_event_after_output_is_a_failure_with_its_code() {
        // The shape vLLM's OpenAI server sends when generation fails after the
        // stream opened (Python's `json.dumps`, so spaces after separators).
        let wire = format!(
            "{}{}data: {}\n\ndata: [DONE]\n\n",
            content("Hello"),
            content(", wor"),
            r#"{"error": {"message": "Internal server error", "type": "InternalServerError", "param": null, "code": 500}}"#
        );
        let failure = watch_sse_split_everywhere(&wire).expect("a failure");
        assert_eq!(failure.status, 500);
        assert_eq!(failure.message, "Internal server error");
    }

    #[test]
    fn an_error_event_before_any_output_keeps_a_client_error_code() {
        let wire = concat!(
            "data: {\"error\": {\"object\": \"error\", \"message\": \"bad tool schema\", ",
            "\"type\": \"BadRequestError\", \"param\": null, \"code\": 400}}\n\n",
            "data: [DONE]\n\n"
        );
        let failure = watch_sse_split_everywhere(wire).unwrap();
        assert_eq!(failure.status, 400);
        assert_eq!(failure.message, "bad tool schema");
    }

    #[test]
    fn errors_without_an_http_code_record_bad_gateway() {
        for payload in [
            // OpenAI-style string code.
            json!({"error": {"message": "The server had an error", "type": "server_error", "code": "server_error"}}),
            // A bare message.
            json!({"error": "Request failed during generation: out of memory", "error_type": "generation"}),
            // A typed error event (Responses-style, and Anthropic-style).
            json!({"type": "error", "code": "server_error", "message": "boom", "sequence_number": 4}),
            json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
        ] {
            let failure = watch_sse(format!("data: {payload}\n\n").as_bytes())
                .unwrap_or_else(|| panic!("{payload} is a failure"));
            assert_eq!(failure.status, DEFAULT_FAILURE_STATUS, "{payload}");
            assert!(!failure.message.is_empty());
        }
    }

    #[test]
    fn a_numeric_string_code_and_a_top_level_status_are_honoured() {
        let failure =
            payload_failure(&json!({"error": {"message": "slow down", "code": "429"}})).unwrap();
        assert_eq!(failure.status, 429);
        let failure = payload_failure(&json!({"error": "unavailable", "status": 503})).unwrap();
        assert_eq!(failure.status, 503);
        // Out-of-range numbers are not statuses.
        let failure = payload_failure(&json!({"error": {"message": "x", "code": 7}})).unwrap();
        assert_eq!(failure.status, DEFAULT_FAILURE_STATUS);
    }

    #[test]
    fn an_abort_or_error_finish_reason_is_a_failure_and_ordinary_ones_are_not() {
        for reason in ["abort", "error"] {
            let wire = format!(
                "{}data: {}\n\ndata: [DONE]\n\n",
                content("partial"),
                json!({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
            );
            let failure = watch_sse_split_everywhere(&wire).expect("a failure");
            assert_eq!(failure.status, DEFAULT_FAILURE_STATUS);
        }
        for reason in ["stop", "length", "tool_calls", "content_filter"] {
            let wire = format!(
                "{}data: {}\n\ndata: [DONE]\n\n",
                content("done"),
                json!({"choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
            );
            assert_eq!(watch_sse(wire.as_bytes()), None, "{reason}");
        }
    }

    #[test]
    fn a_named_error_event_is_a_failure_whatever_its_data() {
        let failure = watch_sse(b"event: error\ndata: upstream went away\n\n").unwrap();
        assert_eq!(failure.status, DEFAULT_FAILURE_STATUS);
        assert_eq!(failure.message, "upstream went away");
        let failure = watch_sse(
            b"event: error\r\ndata: {\"error\": {\"message\": \"nope\", \"code\": 503}}\r\n\r\n",
        )
        .unwrap();
        assert_eq!(failure.status, 503);
    }

    #[test]
    fn a_clean_stream_is_not_a_failure_even_when_its_text_talks_about_errors() {
        let wire = format!(
            "{}{}{}data: {}\n\ndata: [DONE]\n\n",
            // Quotes inside content are escaped, but a candidate can still
            // turn up; parsing settles it.
            content("say \"error"),
            content("\" or \"abort\" and {\"error\": 1}"),
            ": keep-alive comment\n\n",
            json!({"choices": [], "usage": {"prompt_tokens": 3, "completion_tokens": 2}, "error": null})
        );
        assert_eq!(watch_sse_split_everywhere(&wire), None);
    }

    #[test]
    fn the_first_failure_wins() {
        let wire = format!(
            "data: {}\n\ndata: {}\n\n",
            json!({"error": {"message": "first", "code": 500}}),
            json!({"error": {"message": "second", "code": 400}}),
        );
        assert_eq!(watch_sse(wire.as_bytes()).unwrap().message, "first");
    }

    #[test]
    fn an_unterminated_final_error_event_still_counts() {
        let failure =
            watch_sse(br#"data: {"error": {"message": "cut", "code": 500}}"#).expect("a failure");
        assert_eq!(failure.status, 500);
    }

    #[test]
    fn oversized_events_are_skipped_without_losing_the_next_one() {
        let big = "x".repeat(MAX_INSPECTED_BYTES + 10);
        let wire = format!(
            "{}data: {}\n\n",
            content(&big),
            json!({"error": {"message": "after a long event", "code": 500}})
        );
        // Delivered in small pieces, as a real stream would be.
        let mut watch = FailureWatch::for_content_type("text/event-stream").unwrap();
        for piece in wire.as_bytes().chunks(1000) {
            watch.push(piece);
        }
        assert_eq!(watch.finish().unwrap().message, "after a long event");
        // An oversized error-looking event is generated content, not a report.
        let wire = format!("data: {{\"error\": \"{big}\"}}\n\n");
        assert_eq!(watch_sse(wire.as_bytes()), None);
    }

    #[test]
    fn a_json_body_that_is_an_error_object_is_a_failure() {
        let body = json!({"error": {"message": "engine dead", "type": "InternalServerError", "code": 500}});
        let mut watch = FailureWatch::for_content_type("application/json").unwrap();
        for byte in body.to_string().as_bytes() {
            watch.push(&[*byte]);
        }
        let failure = watch.finish().unwrap();
        assert_eq!(
            (failure.status, failure.message.as_str()),
            (500, "engine dead")
        );
    }

    #[test]
    fn ordinary_json_bodies_are_not_failures() {
        for body in [
            json!({"object": "chat.completion",
                   "choices": [{"index": 0, "message": {"role": "assistant", "content": "an \"error\""},
                                "finish_reason": "stop"}],
                   "usage": {"prompt_tokens": 1, "completion_tokens": 2}}),
            json!({"object": "response", "status": "completed", "error": null, "output": []}),
            json!({"object": "list", "data": [{"embedding": [0.1, 0.2]}]}),
        ] {
            let mut watch = FailureWatch::for_content_type("application/json").unwrap();
            watch.push(body.to_string().as_bytes());
            assert_eq!(watch.finish(), None, "{body}");
        }
        // Not JSON at all.
        let mut watch = FailureWatch::for_content_type("application/json").unwrap();
        watch.push(b"{\"error\": ");
        assert_eq!(watch.finish(), None);
    }

    #[test]
    fn an_aborted_json_completion_and_a_failed_response_object_are_failures() {
        let aborted = json!({"object": "chat.completion",
            "choices": [{"index": 0, "message": {"content": "par"}, "finish_reason": "abort"}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 1}});
        assert!(payload_failure(&aborted).is_some());
        let failed = json!({"type": "response.failed",
            "response": {"status": "failed", "error": {"code": "server_error", "message": "no"}}});
        assert_eq!(payload_failure(&failed).unwrap().message, "no");
    }

    #[test]
    fn bodies_that_cannot_carry_a_json_error_are_not_watched() {
        for content_type in ["audio/mpeg", "image/png", "text/plain", ""] {
            assert!(FailureWatch::for_content_type(content_type).is_none());
        }
        assert!(FailureWatch::for_content_type("application/json; charset=utf-8").is_some());
        assert!(FailureWatch::for_content_type("Text/Event-Stream").is_some());
    }

    #[test]
    fn a_json_body_past_the_cap_is_not_judged() {
        let mut watch = FailureWatch::for_content_type("application/json").unwrap();
        watch.push(br#"{"error": {"message": "x", "code": 500}, "pad": ""#);
        watch.push(&vec![b'y'; MAX_INSPECTED_BYTES]);
        watch.push(br#""}"#);
        assert_eq!(watch.finish(), None);
    }

    #[test]
    fn long_messages_are_shortened() {
        let failure = payload_failure(&json!({"error": "e".repeat(1000)})).unwrap();
        assert_eq!(failure.message.chars().count(), MAX_MESSAGE_CHARS + 3);
    }

    #[test]
    fn the_cut_event_is_an_openai_error_event_and_reads_as_a_bad_gateway() {
        let event = stream_cut_event(std::time::Duration::from_millis(3_600_400));
        let text = std::str::from_utf8(&event).unwrap();
        assert!(
            text.starts_with("data: ") && text.ends_with("\n\n"),
            "{text:?}"
        );
        let payload: Value = serde_json::from_str(&text[6..]).unwrap();
        assert_eq!(
            payload,
            json!({"error": {
                "message": "upstream stream ended before the response was complete (after 3600 s)",
                "type": "obleth_gateway_error",
                "code": 502,
            }})
        );
        // What the gateway says is a failure by its own reading too, so a
        // translating surface or a downstream obleth records it as one.
        assert_eq!(watch_sse(text.as_bytes()).unwrap().status, 502);
    }

    fn watch_end(wire: &str) -> FailureWatch {
        let mut watch = FailureWatch::for_content_type("text/event-stream")
            .unwrap()
            .expecting_end();
        watch.push(wire.as_bytes());
        watch.finish();
        watch
    }

    fn finish(reason: &str) -> String {
        format!(
            "data: {}\n\n",
            json!({"object": "chat.completion.chunk",
                   "choices": [{"index": 0, "delta": {}, "finish_reason": reason}]})
        )
    }

    #[test]
    fn a_completion_stream_without_done_or_a_finish_reason_ended_early() {
        // Text and then nothing: the upstream stopped short.
        let cut = format!("{}{}", content("Hello"), content(", wor"));
        assert!(watch_end(&cut).ended_early());
        // Nothing at all is no answer either.
        assert!(watch_end("").ended_early());
        // A comment or a keep-alive does not end it.
        assert!(watch_end(&format!("{cut}: keep-alive\n\n")).ended_early());
        // Generated text that talks about the field is not the field.
        assert!(watch_end(&content(r#"say "finish_reason":"stop" now"#)).ended_early());
    }

    #[test]
    fn done_or_a_finish_reason_ends_a_completion_stream() {
        let whole = format!("{}{}data: [DONE]\n\n", content("Hello"), finish("stop"));
        let watch = watch_end(&whole);
        assert!(!watch.ended_early() && watch.saw_done());
        // An upstream that sends no `[DONE]` still says when it finished, in
        // either JSON spacing.
        let no_done = format!("{}{}", content("Hello"), finish("length"));
        assert!(!watch_end(&no_done).ended_early());
        let spaced = format!(
            "{}data: {{\"choices\": [{{\"index\": 0, \"delta\": {{}}, \"finish_reason\": \"stop\"}}]}}\n\n",
            content("Hello")
        );
        assert!(!watch_end(&spaced).ended_early());
        // `[DONE]` without its blank line, at the very end, still counts.
        let watch = watch_end(&format!("{}data: [DONE]", content("Hello")));
        assert!(!watch.ended_early() && watch.saw_done());
        // Split anywhere, the verdict is the same.
        for cut in 0..=whole.len() {
            let mut watch = FailureWatch::for_content_type("text/event-stream")
                .unwrap()
                .expecting_end();
            watch.push(&whole.as_bytes()[..cut]);
            watch.push(&whole.as_bytes()[cut..]);
            watch.finish();
            assert!(!watch.ended_early(), "split at byte {cut}");
        }
    }

    #[test]
    fn a_stream_that_reported_its_failure_or_has_no_end_is_not_ended_early() {
        // The upstream's own error event says why; no second word is needed.
        let failed = format!(
            "{}data: {}\n\n",
            content("Hello"),
            json!({"error": {"message": "boom", "code": 500}})
        );
        let watch = watch_end(&failed);
        assert!(watch.failure.is_some() && !watch.ended_early());
        // Only completion streams are expected to end; other bodies are not
        // judged.
        let mut other = FailureWatch::for_content_type("text/event-stream").unwrap();
        other.push(content("Hello").as_bytes());
        other.finish();
        assert!(!other.ended_early());
        let mut json_body = FailureWatch::for_content_type("application/json")
            .unwrap()
            .expecting_end();
        json_body.push(b"{\"choices\": [");
        json_body.finish();
        assert!(!json_body.ended_early() && !json_body.is_sse());
    }

    /// What a real client reads from a server whose body is `body`: the
    /// bytes that arrived, and whether the response was aborted.
    async fn served(
        body: impl futures_util::Stream<Item = Result<Bytes, std::io::Error>> + Send + 'static,
    ) -> (String, bool) {
        let body = std::sync::Arc::new(std::sync::Mutex::new(Some(axum::body::Body::from_stream(
            body,
        ))));
        let app = axum::Router::new().route(
            "/",
            axum::routing::get(move || {
                let body = body.lock().unwrap().take().expect("one request");
                async move {
                    axum::http::Response::builder()
                        .header("content-type", "text/event-stream")
                        .body(body)
                        .unwrap()
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut resp = reqwest::get(format!("http://{addr}/")).await.unwrap();
        let mut text = String::new();
        loop {
            match resp.chunk().await {
                Ok(Some(chunk)) => text.push_str(&String::from_utf8_lossy(&chunk)),
                Ok(None) => return (text, false),
                Err(_) => return (text, true),
            }
        }
    }

    #[tokio::test]
    async fn the_cut_event_reaches_a_real_client_before_the_abort() {
        use futures_util::StreamExt;
        const TEXT: &str = "data: {\"x\": 1}\n\n";
        let text = || futures_util::stream::iter([Ok(Bytes::from_static(TEXT.as_bytes()))]);
        // Text, then at once the end of a cut stream: nothing in between
        // gives the server a moment to flush, as when the request has just
        // settled.
        let event = stream_cut_event(std::time::Duration::from_secs(7));
        let (got, aborted) = served(text().chain(stream_cut_tail(Some(event.clone())))).await;
        assert!(aborted, "{got}");
        assert_eq!(
            got,
            format!("{TEXT}{}", std::str::from_utf8(&event).unwrap())
        );
        // Without an event (the upstream's own already went out), only the
        // abort.
        let (got, aborted) = served(text().chain(stream_cut_tail(None))).await;
        assert!(aborted, "{got}");
        assert_eq!(got, TEXT);
    }

    #[test]
    fn the_sent_tail_tells_an_event_boundary() {
        let mut tail = Vec::new();
        assert!(at_event_boundary(&tail));
        remember_sent(&mut tail, b"data: {\"a\": 1}\n\n");
        assert!(at_event_boundary(&tail));
        assert_eq!(tail.len(), 4);
        remember_sent(&mut tail, b"data: {\"a\":");
        assert!(!at_event_boundary(&tail));
        // A blank line split across two sends.
        remember_sent(&mut tail, b" 2}\n");
        assert!(!at_event_boundary(&tail));
        remember_sent(&mut tail, b"\n");
        assert!(at_event_boundary(&tail));
        remember_sent(&mut tail, b"data: x\r\n\r\n");
        assert!(at_event_boundary(&tail));
    }
}

/// Bare HTTP/1.1 upstreams, for tests that need a connection to fail in ways a
/// framework server will not.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    /// How the upstream drops a connection it fails.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum Hangup {
        /// A TCP reset.
        Reset,
        /// An orderly close.
        Close,
        /// No hang-up: the connection stays open and nothing is ever sent.
        Silent,
    }

    /// An upstream at the returned base URL (`.../v1`) whose first `failures`
    /// connections read the whole request and then hang up (or, for
    /// [`Hangup::Silent`], go quiet) before sending a byte of response; every
    /// later one answers `200` with `body` as JSON.
    /// The counter is the number of connections accepted.
    pub(crate) async fn spawn_failing_upstream(
        failures: usize,
        hangup: Hangup,
        body: String,
    ) -> (Arc<AtomicUsize>, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                let body = body.clone();
                tokio::spawn(async move {
                    read_request(&mut socket).await;
                    if n < failures {
                        if matches!(hangup, Hangup::Silent) {
                            std::future::pending::<()>().await;
                        }
                        if matches!(hangup, Hangup::Reset) {
                            let _ = socket.set_zero_linger();
                        }
                        drop(socket);
                        return;
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\n\
                         content-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (accepted, format!("http://{addr}/v1"))
    }

    /// How a streaming upstream ends its response.
    #[derive(Clone, Copy, Debug)]
    pub(crate) enum StreamEnd {
        /// The events, then a clean close of the body.
        Clean,
        /// The connection is reset mid-body.
        Reset,
        /// The connection closes mid-body, before the last chunk.
        Close,
        /// The upstream holds the connection open and sends nothing more.
        Silent,
    }

    /// An upstream at the returned base URL (`.../v1`) that answers every
    /// request `200` with an SSE body: each of `events` as its own chunk,
    /// then `end`. The counter is the number of connections accepted.
    pub(crate) async fn spawn_sse_upstream(
        events: Vec<String>,
        end: StreamEnd,
    ) -> (Arc<AtomicUsize>, String) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let accepted = Arc::new(AtomicUsize::new(0));
        let counter = accepted.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                counter.fetch_add(1, Ordering::SeqCst);
                let events = events.clone();
                tokio::spawn(async move {
                    read_request(&mut socket).await;
                    let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\
                                transfer-encoding: chunked\r\nconnection: close\r\n\r\n";
                    if socket.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    for event in &events {
                        let chunk = format!("{:x}\r\n{event}\r\n", event.len());
                        if socket.write_all(chunk.as_bytes()).await.is_err() {
                            return;
                        }
                        let _ = socket.flush().await;
                        // Each event its own read, as a live stream is.
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    // Let the reader take every event before the end, so a
                    // reset cannot discard one still in its buffer.
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    match end {
                        StreamEnd::Clean => {
                            let _ = socket.write_all(b"0\r\n\r\n").await;
                            let _ = socket.shutdown().await;
                        }
                        StreamEnd::Reset => {
                            let _ = socket.set_zero_linger();
                            drop(socket);
                        }
                        StreamEnd::Close => {
                            let _ = socket.shutdown().await;
                        }
                        StreamEnd::Silent => {
                            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        }
                    }
                });
            }
        });
        (accepted, format!("http://{addr}/v1"))
    }

    /// Read one request: its headers, then `content-length` bytes of body.
    async fn read_request(socket: &mut TcpStream) {
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&buf[..end]).to_ascii_lowercase();
                let length = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:"))
                    .and_then(|v| v.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if buf.len() >= end + 4 + length {
                    return;
                }
            }
            match socket.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    }
}

/// The whole pipeline against fake upstreams: how a failure reported inside a
/// `200` is recorded, billed and kept out of the cache, and that a connection
/// the upstream drops before answering is retried. Needs the test datastores
/// (`OBLETH_TEST_DATABASE_URL`, `OBLETH_TEST_REDIS_URL`); skipped without
/// them.
#[cfg(test)]
mod pipeline {
    use std::collections::HashMap;
    use std::sync::atomic::Ordering;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use axum::body::Body;
    use axum::extract::State;
    use axum::http::{header, Method, Request, Response, StatusCode};
    use axum::routing::post;
    use axum::{Json, Router};
    use moka::future::Cache;
    use obleth_config::{ResolvedKey, ResolvedModel};
    use serde_json::{json, Value};
    use uuid::Uuid;

    use super::test_support::{spawn_failing_upstream, spawn_sse_upstream, Hangup, StreamEnd};
    use crate::state::AppState;

    /// Price of one output token; input is free, so spend counts output only.
    const OUTPUT_RATE: f64 = 0.001;

    #[derive(Default)]
    struct Upstream {
        hits: Mutex<HashMap<String, usize>>,
    }

    impl Upstream {
        fn hits(&self, model: &str) -> usize {
            self.hits.lock().unwrap().get(model).copied().unwrap_or(0)
        }
    }

    fn sse(events: &[Value]) -> Response<Body> {
        let mut text: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
        text.push_str("data: [DONE]\n\n");
        Response::builder()
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from(text))
            .unwrap()
    }

    fn delta(text: &str) -> Value {
        json!({"id": "c1", "object": "chat.completion.chunk",
               "choices": [{"index": 0, "delta": {"content": text}, "finish_reason": null}]})
    }

    /// What the fake upstream answers, by the model it is asked for.
    fn reply(model: &str) -> Response<Body> {
        match model {
            "fails-mid-stream" => sse(&[
                delta("Hello"),
                delta(" world"),
                json!({"error": {"message": "Internal server error",
                                 "type": "InternalServerError", "param": null, "code": 500}}),
            ]),
            "streams-cleanly" => sse(&[
                delta("Hello"),
                delta(" world"),
                json!({"id": "c1", "object": "chat.completion.chunk",
                       "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
                json!({"id": "c1", "object": "chat.completion.chunk", "choices": [],
                       "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}}),
            ]),
            "errors-in-json" => Response::builder()
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"error": {"message": "engine unavailable",
                                     "type": "ServiceUnavailableError", "code": 503}})
                    .to_string(),
                ))
                .unwrap(),
            _ => Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::empty())
                .unwrap(),
        }
    }

    async fn spawn_upstream() -> (Arc<Upstream>, String) {
        let upstream = Arc::new(Upstream::default());
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    |State(up): State<Arc<Upstream>>, Json(body): Json<Value>| async move {
                        let model = body["model"].as_str().unwrap_or_default().to_string();
                        *up.hits.lock().unwrap().entry(model.clone()).or_default() += 1;
                        reply(&model)
                    },
                ),
            )
            .with_state(upstream.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (upstream, format!("http://{addr}/v1"))
    }

    struct Gateway {
        state: AppState,
        wal_dir: std::path::PathBuf,
    }

    /// A streamed response as the client read it.
    struct Streamed {
        status: StatusCode,
        boons: String,
        text: String,
        aborted: bool,
    }

    impl Drop for Gateway {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.wal_dir);
        }
    }

    impl Gateway {
        async fn register(&self, route: ResolvedModel) {
            self.state
                .model_cache
                .insert(route.model_name.clone(), Arc::new(route))
                .await;
        }

        /// A key for a fresh tenant with a lifetime cost cap, so its spend is
        /// readable from the term counters.
        async fn tenant(&self) -> (String, ResolvedKey) {
            let mut key = crate::boons::test_support::test_key();
            key.key_id = Uuid::new_v4();
            key.tenant_id = Uuid::new_v4();
            key.tenant_name = format!("t-{}", key.tenant_id);
            key.budget_cost_usd = Some(1000.0);
            key.budget_period = Some("lifetime".into());
            let secret = format!("sk-failure-{}", Uuid::new_v4());
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

        /// Send a chat request and read the whole response, which is when a
        /// stream settles.
        async fn chat(&self, secret: &str, body: &Value) -> (StatusCode, bool, String) {
            let req = Request::builder()
                .method(Method::POST)
                .uri("/v1/chat/completions")
                .header(header::AUTHORIZATION, format!("Bearer {secret}"))
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            let resp = crate::proxy::proxy_handler(State(self.state.clone()), req).await;
            let status = resp.status();
            let cache_hit = resp
                .headers()
                .get("x-obleth-cache")
                .is_some_and(|v| v == "hit");
            let bytes = axum::body::to_bytes(resp.into_body(), usize::MAX)
                .await
                .expect("body");
            (
                status,
                cache_hit,
                String::from_utf8_lossy(&bytes).into_owned(),
            )
        }

        /// Send a streamed chat request through a real HTTP server, as a
        /// client reaches the gateway, and read its body to the end or to the
        /// error that aborts it: what the server writes before an abort is
        /// what a client sees. Returns the status, the boons header, the
        /// bytes that arrived, and whether the body was aborted.
        async fn chat_stream(&self, secret: &str, body: &Value) -> Streamed {
            let app = Router::new()
                .route("/v1/chat/completions", post(crate::proxy::proxy_handler))
                .with_state(self.state.clone());
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let mut resp = reqwest::Client::new()
                .post(format!("http://{addr}/v1/chat/completions"))
                .bearer_auth(secret)
                .json(body)
                .send()
                .await
                .expect("response headers");
            let status = resp.status();
            let boons = resp
                .headers()
                .get(crate::boons::BOONS_HEADER)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_string();
            let mut text = String::new();
            let aborted = loop {
                match resp.chunk().await {
                    Ok(Some(chunk)) => text.push_str(&String::from_utf8_lossy(&chunk)),
                    Ok(None) => break false,
                    Err(_) => break true,
                }
            };
            Streamed {
                status,
                boons,
                text,
                aborted,
            }
        }

        /// Settled requests by status class, from the gateway's own metrics.
        fn requests_with_status(&self, class: &str) -> u64 {
            let label = format!("status=\"{class}\"");
            self.state
                .metrics
                .encode()
                .lines()
                .filter(|l| l.starts_with("obleth_requests_total{") && l.contains(&label))
                .filter_map(|l| l.rsplit(' ').next()?.parse::<f64>().ok())
                .sum::<f64>() as u64
        }

        fn output_tokens(&self) -> u64 {
            self.state
                .metrics
                .encode()
                .lines()
                .find_map(|l| l.strip_prefix("obleth_output_tokens_total "))
                .and_then(|v| v.trim().parse::<f64>().ok())
                .unwrap_or(0.0) as u64
        }
    }

    async fn gateway() -> Option<Gateway> {
        gateway_with(reqwest::Client::new(), Default::default()).await
    }

    /// A gateway whose upstream client is `http` (so a test can give it a
    /// short read timeout) and whose boons run with `boons`.
    async fn gateway_with(
        http: reqwest::Client,
        boons: obleth_config::BoonSettings,
    ) -> Option<Gateway> {
        let (Ok(db), Ok(redis_url)) = (
            std::env::var("OBLETH_TEST_DATABASE_URL"),
            std::env::var("OBLETH_TEST_REDIS_URL"),
        ) else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL and OBLETH_TEST_REDIS_URL to run");
            return None;
        };
        let store = obleth_store::Store::connect(&db).await.expect("postgres");
        let redis = obleth_redis::RedisStore::connect(&redis_url)
            .await
            .expect("redis");
        let wal_dir = std::env::temp_dir().join(format!("obleth-failure-wal-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&wal_dir).unwrap();
        let telemetry = obleth_telemetry::TelemetrySink::start(
            "http://127.0.0.1:1",
            "obleth_test",
            "default",
            "",
            wal_dir.join("usage.wal").to_str().unwrap(),
            Duration::from_millis(100),
            true,
        )
        .await
        .expect("telemetry sink");
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
            families: crate::reasoning::FamilyRegistry::new(),
            classifier: crate::classifier::Classifier::new(Default::default()),
            output_stats: Default::default(),
            boons: crate::boons::BoonEngine::new(boons),
            tool_cache: Cache::builder().build(),
            metrics: Arc::new(crate::metrics::Metrics::new()),
            fail_open: false,
            alerts: obleth_admin::AlertDispatcher::new(http, Default::default()),
            session_id_derivation: false,
            session_affinity: None,
            session_affinity_settings: Default::default(),
            compressor: None,
            energy: crate::energy::EnergyEngine::new(Default::default()),
            jwt: None,
            knowledge: Arc::new(crate::knowledge::KnowledgeIndex::new()),
            jobs: crate::jobs::JobStore::new(store, Default::default()),
            load: None,
        };
        Some(Gateway { state, wal_dir })
    }

    /// A cached, output-priced model served by the fake upstream.
    fn route(name: &str, base: &str) -> ResolvedModel {
        let mut route = crate::boons::test_support::test_route();
        route.model_name = name.into();
        route.upstream_model = name.into();
        route.api_base = base.into();
        route.cache_enabled = true;
        route.cache_ttl_secs = 60;
        route.output_cost_per_token = OUTPUT_RATE;
        route
    }

    fn request(model: &str, stream: bool) -> Value {
        json!({
            "model": model,
            "stream": stream,
            "max_tokens": 8192,
            "messages": [{"role": "user", "content": "hello"}],
        })
    }

    #[tokio::test]
    async fn an_error_event_inside_a_200_stream_is_a_failure_billed_for_its_output() {
        let Some(gw) = gateway().await else { return };
        let (upstream, base) = spawn_upstream().await;
        gw.register(route("fails-mid-stream", &base)).await;
        let (secret, key) = gw.tenant().await;
        let body = request("fails-mid-stream", true);

        let (status, _, text) = gw.chat(&secret, &body).await;
        // The client gets the stream as the upstream sent it, error included.
        assert_eq!(status, StatusCode::OK);
        assert!(text.contains("Internal server error"), "{text}");
        // Recorded with the error's own code, and billed for the text that
        // was delivered ("Hello world": 11 characters, 2 tokens), never for
        // the 8192 `max_tokens` the request allowed.
        assert_eq!(gw.requests_with_status("5xx"), 1);
        assert_eq!(gw.requests_with_status("2xx"), 0);
        assert_eq!(gw.output_tokens(), 2);
        assert!((gw.spent(&key).await - 2.0 * OUTPUT_RATE).abs() < 1e-9);
        // Not a sample of how long this model's answers run.
        assert!(!gw
            .state
            .output_stats
            .snapshot()
            .contains_key("fails-mid-stream"));
        // Never cached: the same request goes to the upstream again.
        let (_, cache_hit, _) = gw.chat(&secret, &body).await;
        assert!(!cache_hit);
        assert_eq!(upstream.hits("fails-mid-stream"), 2);
    }

    #[tokio::test]
    async fn a_clean_stream_settles_its_usage_and_is_cached() {
        let Some(gw) = gateway().await else { return };
        let (upstream, base) = spawn_upstream().await;
        gw.register(route("streams-cleanly", &base)).await;
        let (secret, key) = gw.tenant().await;
        let body = request("streams-cleanly", true);

        let (status, _, _) = gw.chat(&secret, &body).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(gw.requests_with_status("2xx"), 1);
        assert_eq!(gw.output_tokens(), 2);
        assert!((gw.spent(&key).await - 2.0 * OUTPUT_RATE).abs() < 1e-9);
        assert_eq!(
            gw.state.output_stats.snapshot().get("streams-cleanly"),
            Some(&2.0)
        );
        let (_, cache_hit, _) = gw.chat(&secret, &body).await;
        assert!(cache_hit);
        assert_eq!(upstream.hits("streams-cleanly"), 1);
    }

    #[tokio::test]
    async fn a_200_whose_json_body_is_an_error_is_a_failure_billed_nothing() {
        let Some(gw) = gateway().await else { return };
        let (upstream, base) = spawn_upstream().await;
        gw.register(route("errors-in-json", &base)).await;
        let (secret, key) = gw.tenant().await;
        let body = request("errors-in-json", false);

        let (status, _, text) = gw.chat(&secret, &body).await;
        assert_eq!(status, StatusCode::OK, "passed through as sent");
        assert!(text.contains("engine unavailable"), "{text}");
        assert_eq!(gw.requests_with_status("5xx"), 1);
        assert_eq!(gw.output_tokens(), 0);
        assert_eq!(gw.spent(&key).await, 0.0);
        assert!(!gw
            .state
            .output_stats
            .snapshot()
            .contains_key("errors-in-json"));
        let (_, cache_hit, _) = gw.chat(&secret, &body).await;
        assert!(!cache_hit);
        assert_eq!(upstream.hits("errors-in-json"), 2);
    }

    #[tokio::test]
    async fn a_connection_dropped_before_response_headers_is_retried() {
        let Some(gw) = gateway().await else { return };
        let answer = json!({
            "id": "c1", "object": "chat.completion", "model": "flaky",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 1, "total_tokens": 4},
        })
        .to_string();
        // With retries configured, and with none (the one fresh-connection
        // retry a dropped connection always gets).
        for (max_retries, hangup) in [(1, Hangup::Reset), (0, Hangup::Reset), (0, Hangup::Close)] {
            let (accepted, base) = spawn_failing_upstream(1, hangup, answer.clone()).await;
            let name = format!("flaky-{max_retries}-{hangup:?}");
            let mut flaky = route(&name, &base);
            flaky.cache_enabled = false;
            flaky.max_retries = max_retries;
            flaky.retry_backoff_ms = 0;
            gw.register(flaky).await;
            let (secret, _) = gw.tenant().await;

            let (status, _, text) = gw.chat(&secret, &request(&name, false)).await;
            assert_eq!(status, StatusCode::OK, "{name}: {text}");
            assert_eq!(accepted.load(Ordering::SeqCst), 2, "{name}");
        }
    }

    /// The two ways a chat stream runs through the gateway: passed through as
    /// the upstream sends it, or driven by the streaming tool loop (here
    /// armed by the image-generation boon, as on any tool-boon model).
    #[derive(Clone, Copy, Debug)]
    enum Path {
        Plain,
        Boon,
    }

    const PATHS: [Path; 2] = [Path::Plain, Path::Boon];

    fn sse_event(value: &Value) -> String {
        format!("data: {value}\n\n")
    }

    /// A gateway for `path` whose upstream reads give up after half a second
    /// of silence, with a model on `base` named after the case.
    async fn gateway_for(path: Path, name: &str, base: &str) -> Option<Gateway> {
        let http = reqwest::Client::builder()
            .read_timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        let boons = obleth_config::BoonSettings {
            image_generation: obleth_config::ImageGenerationBoonSettings {
                enabled: true,
                image_model: Some("image-model".into()),
                ..Default::default()
            },
            ..Default::default()
        };
        let gw = gateway_with(http, boons).await?;
        let mut model = route(name, base);
        model.cache_enabled = false;
        if let Path::Boon = path {
            model.boons = vec!["image_generation".into()];
            model.supports_function_calling = true;
        }
        gw.register(model).await;
        Some(gw)
    }

    fn assert_path(path: Path, streamed: &Streamed, case: &str) {
        match path {
            Path::Boon => assert!(streamed.boons.contains("image_generation"), "{case}"),
            Path::Plain => assert!(streamed.boons.is_empty(), "{case}: {}", streamed.boons),
        }
    }

    /// The `data:` payloads of every event the client received.
    fn payloads(text: &str) -> Vec<String> {
        text.split("\n\n")
            .filter_map(|event| event.trim().strip_prefix("data: "))
            .map(str::to_string)
            .collect()
    }

    #[tokio::test]
    async fn a_stream_the_upstream_stops_short_of_ends_with_an_error_event_on_every_path() {
        let text = [delta("Hello"), delta(" world")].map(|v| sse_event(&v));
        for path in PATHS {
            for end in [
                StreamEnd::Reset,
                StreamEnd::Close,
                StreamEnd::Silent,
                StreamEnd::Clean,
            ] {
                let case = format!("{path:?}/{end:?}");
                let name = format!("cut-{path:?}-{end:?}").to_lowercase();
                let (_, base) = spawn_sse_upstream(text.to_vec(), end).await;
                let Some(gw) = gateway_for(path, &name, &base).await else {
                    return;
                };
                let (secret, key) = gw.tenant().await;
                let mut body = request(&name, true);
                body["stream_options"] = json!({"include_usage": true});

                let streamed = gw.chat_stream(&secret, &body).await;
                assert_path(path, &streamed, &case);
                assert_eq!(streamed.status, StatusCode::OK, "{case}");
                // The text that streamed reached the client, then the
                // gateway's error event, then the response was aborted: no
                // finish chunk, no usage chunk, no `[DONE]`.
                assert!(streamed.aborted, "{case}: {}", streamed.text);
                let events = payloads(&streamed.text);
                assert!(streamed.text.contains("Hello") && streamed.text.contains(" world"));
                assert!(
                    !streamed.text.contains("[DONE]"),
                    "{case}: {}",
                    streamed.text
                );
                assert!(!streamed.text.contains("finish_reason\":\"stop"), "{case}");
                assert!(!streamed.text.contains("completion_tokens"), "{case}");
                let last: Value = serde_json::from_str(events.last().unwrap()).unwrap();
                assert_eq!(last["error"]["type"], "obleth_gateway_error", "{case}");
                assert_eq!(last["error"]["code"], 502, "{case}");
                assert!(
                    last["error"]["message"].as_str().unwrap().starts_with(
                        "upstream stream ended before the response was complete (after "
                    ),
                    "{case}: {last}"
                );
                assert_eq!(
                    events.iter().filter(|e| e.contains("\"error\"")).count(),
                    1,
                    "{case}"
                );
                // Recorded as a failed call (502) billed for the text that
                // reached the client ("Hello world": 11 characters, 2
                // tokens), never for the 8192 `max_tokens` it allowed.
                assert_eq!(gw.requests_with_status("5xx"), 1, "{case}");
                assert_eq!(gw.requests_with_status("2xx"), 0, "{case}");
                assert_eq!(gw.output_tokens(), 2, "{case}");
                assert!(
                    (gw.spent(&key).await - 2.0 * OUTPUT_RATE).abs() < 1e-9,
                    "{case}"
                );
                assert!(
                    !gw.state.output_stats.snapshot().contains_key(&name),
                    "{case}"
                );
            }
        }
    }

    #[tokio::test]
    async fn the_upstreams_own_error_event_is_forwarded_once_on_every_path() {
        let events = [
            delta("Hello"),
            json!({"error": {"message": "Internal server error",
                             "type": "InternalServerError", "param": null, "code": 500}}),
        ]
        .map(|v| sse_event(&v));
        for path in PATHS {
            let name = format!("own-error-{path:?}").to_lowercase();
            let (_, base) = spawn_sse_upstream(events.to_vec(), StreamEnd::Reset).await;
            let Some(gw) = gateway_for(path, &name, &base).await else {
                return;
            };
            let (secret, _) = gw.tenant().await;

            let streamed = gw.chat_stream(&secret, &request(&name, true)).await;
            assert_path(path, &streamed, &format!("{path:?}"));
            // The upstream said why; the gateway adds no word of its own.
            assert!(streamed.text.contains("Internal server error"), "{path:?}");
            assert!(!streamed.text.contains("obleth_gateway_error"), "{path:?}");
            assert!(
                !streamed.text.contains("finish_reason\":\"stop"),
                "{path:?}"
            );
            // Recorded with the error's own status.
            assert_eq!(gw.requests_with_status("5xx"), 1, "{path:?}");
            assert_eq!(gw.requests_with_status("2xx"), 0, "{path:?}");
            assert_eq!(gw.output_tokens(), 1, "{path:?}");
        }
    }

    #[tokio::test]
    async fn a_whole_stream_ends_as_it_always_has_on_every_path() {
        let events = [
            delta("Hello"),
            delta(" world"),
            json!({"id": "c1", "object": "chat.completion.chunk",
                   "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}),
            json!({"id": "c1", "object": "chat.completion.chunk", "choices": [],
                   "usage": {"prompt_tokens": 5, "completion_tokens": 2, "total_tokens": 7}}),
        ]
        .map(|v| sse_event(&v))
        .into_iter()
        .chain(["data: [DONE]\n\n".to_string()])
        .collect::<Vec<_>>();
        for path in PATHS {
            let name = format!("whole-{path:?}").to_lowercase();
            let (_, base) = spawn_sse_upstream(events.clone(), StreamEnd::Clean).await;
            let Some(gw) = gateway_for(path, &name, &base).await else {
                return;
            };
            let (secret, key) = gw.tenant().await;
            let mut body = request(&name, true);
            body["stream_options"] = json!({"include_usage": true});

            let streamed = gw.chat_stream(&secret, &body).await;
            assert_path(path, &streamed, &format!("{path:?}"));
            assert!(!streamed.aborted, "{path:?}: {}", streamed.text);
            assert!(
                !streamed.text.contains("\"error\""),
                "{path:?}: {}",
                streamed.text
            );
            // A finish chunk, the usage chunk the client asked for, `[DONE]`.
            let events = payloads(&streamed.text);
            assert_eq!(
                events.last().map(String::as_str),
                Some("[DONE]"),
                "{path:?}"
            );
            let usage: Value = serde_json::from_str(&events[events.len() - 2]).unwrap();
            assert_eq!(usage["usage"]["completion_tokens"], 2, "{path:?}");
            assert!(events
                .iter()
                .any(|e| e.contains("\"finish_reason\":\"stop\"")));
            assert_eq!(gw.requests_with_status("2xx"), 1, "{path:?}");
            assert_eq!(gw.output_tokens(), 2, "{path:?}");
            assert!((gw.spent(&key).await - 2.0 * OUTPUT_RATE).abs() < 1e-9);
        }
    }
}
