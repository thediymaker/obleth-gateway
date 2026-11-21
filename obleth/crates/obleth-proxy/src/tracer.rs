use obleth_telemetry::{SpanRecord, TelemetrySink};
use uuid::Uuid;

/// The spans of one request, sent to the sink when the trace finishes. A
/// recorder dropped without [`finish`](Self::finish) (an early return, a
/// panic unwinding) finishes as `error`, so no traced request goes missing
/// and no exit has to remember to finish it.
pub struct SpanRecorder {
    request_id: Uuid,
    request_start_ms: i64,
    session_id: String,
    session_id_source: String,
    spans: Vec<SpanRecord>,
    sink: TelemetrySink,
    finished: bool,
}

impl SpanRecorder {
    pub fn new(request_id: Uuid, request_start_ms: i64, sink: TelemetrySink) -> Self {
        SpanRecorder {
            request_id,
            request_start_ms,
            session_id: String::new(),
            session_id_source: "none".to_string(),
            spans: Vec::with_capacity(16),
            sink,
            finished: false,
        }
    }

    /// Stamp the conversation id onto every span this recorder emits. Called once
    /// after the id is resolved; safe to call before any spans are recorded.
    pub fn set_conversation(&mut self, session_id: &str, source: &str) {
        self.session_id = session_id.to_string();
        self.session_id_source = source.to_string();
    }

    pub fn record(
        &mut self,
        span_name: &str,
        parent_span: &str,
        start_ms: i64,
        duration_ms: u32,
        status: &str,
        attributes: serde_json::Value,
    ) {
        self.spans.push(SpanRecord {
            request_id: self.request_id,
            span_name: span_name.to_string(),
            parent_span: parent_span.to_string(),
            start_ms,
            duration_ms,
            status: status.to_string(),
            attributes: attributes.to_string(),
            session_id: self.session_id.clone(),
            session_id_source: self.session_id_source.clone(),
        });
    }

    /// Record spans a boon measured without a recorder at hand.
    pub fn record_pending(&mut self, spans: impl IntoIterator<Item = PendingSpan>) {
        for span in spans {
            self.record(
                &span.name,
                &span.parent,
                span.start_ms,
                span.duration_ms,
                span.status,
                span.attributes,
            );
        }
    }

    /// A handle that records spans for this request after the recorder has
    /// finished: a streamed response's boons (the streaming tool loop, a
    /// committed speculation) finish inside the response body, after the
    /// recorder has sent the trace at header time.
    pub fn late(&self) -> LateSpans {
        LateSpans {
            request_id: self.request_id,
            session_id: self.session_id.clone(),
            session_id_source: self.session_id_source.clone(),
            sink: self.sink.clone(),
        }
    }

    pub fn record_elapsed(
        &mut self,
        span_name: &str,
        parent_span: &str,
        span_start_ms: i64,
        status: &str,
        attributes: serde_json::Value,
    ) {
        let duration_ms = (now_ms() - span_start_ms).max(0) as u32;
        self.record(
            span_name,
            parent_span,
            span_start_ms,
            duration_ms,
            status,
            attributes,
        );
    }

    /// Record the root `proxy_request` span with total request duration, then
    /// send all buffered spans to the telemetry sink.
    pub fn finish(mut self, status: &str) {
        self.close(status);
    }

    fn close(&mut self, status: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        let duration_ms = (now_ms() - self.request_start_ms).max(0) as u32;
        self.spans.push(SpanRecord {
            request_id: self.request_id,
            span_name: "proxy_request".to_string(),
            parent_span: String::new(),
            start_ms: self.request_start_ms,
            duration_ms,
            status: status.to_string(),
            attributes: "{}".to_string(),
            session_id: self.session_id.clone(),
            session_id_source: self.session_id_source.clone(),
        });
        for span in self.spans.drain(..) {
            self.sink.record_span(span);
        }
    }
}

impl Drop for SpanRecorder {
    fn drop(&mut self) {
        self.close("error");
    }
}

/// A span measured where no recorder is at hand, recorded afterwards with
/// [`SpanRecorder::record_pending`] or [`LateSpans::record`].
#[derive(Debug, Clone, PartialEq)]
pub struct PendingSpan {
    pub name: String,
    pub parent: String,
    pub start_ms: i64,
    pub duration_ms: u32,
    pub status: &'static str,
    pub attributes: serde_json::Value,
}

impl PendingSpan {
    /// A span from `start_ms` to `end_ms`.
    pub fn between(
        name: impl Into<String>,
        parent: impl Into<String>,
        start_ms: i64,
        end_ms: i64,
        status: &'static str,
        attributes: serde_json::Value,
    ) -> Self {
        PendingSpan {
            name: name.into(),
            parent: parent.into(),
            start_ms,
            duration_ms: u32::try_from((end_ms - start_ms).max(0)).unwrap_or(u32::MAX),
            status,
            attributes,
        }
    }
}

/// Records spans for a request whose recorder already finished (see
/// [`SpanRecorder::late`]). Each span goes to the sink as it is recorded.
pub struct LateSpans {
    request_id: Uuid,
    session_id: String,
    session_id_source: String,
    sink: TelemetrySink,
}

impl LateSpans {
    pub fn record(&self, spans: impl IntoIterator<Item = PendingSpan>) {
        for span in spans {
            self.sink.record_span(SpanRecord {
                request_id: self.request_id,
                span_name: span.name,
                parent_span: span.parent,
                start_ms: span.start_ms,
                duration_ms: span.duration_ms,
                status: span.status.to_string(),
                attributes: span.attributes.to_string(),
                session_id: self.session_id.clone(),
                session_id_source: self.session_id_source.clone(),
            });
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use obleth_telemetry::{Outcome, Stream};
    use std::time::Duration;

    /// A recorder that is dropped without `finish` (an early return) still
    /// emits its trace, finished as an error, and only once; the sink here has
    /// no ClickHouse, so every span it is handed counts as dropped there.
    #[tokio::test]
    async fn a_dropped_recorder_finishes_its_trace_once_as_an_error() {
        let wal = std::env::temp_dir().join(format!("obleth-tracer-test-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&wal).unwrap();
        let sink = TelemetrySink::start(
            "http://127.0.0.1:1",
            "obleth_test",
            "default",
            "",
            wal.join("usage.wal").to_str().unwrap(),
            Duration::from_millis(50),
        )
        .await
        .expect("sink");
        let stats = sink.stats();
        let before = stats.lost(Stream::Spans, Outcome::Dropped);
        {
            let mut recorder = SpanRecorder::new(Uuid::new_v4(), now_ms(), sink.clone());
            recorder.record(
                "auth_resolve",
                "proxy_request",
                now_ms(),
                1,
                "ok",
                serde_json::json!({}),
            );
            // dropped here, unfinished
        }
        let mut finished = SpanRecorder::new(Uuid::new_v4(), now_ms(), sink.clone());
        finished.record(
            "auth_resolve",
            "proxy_request",
            now_ms(),
            1,
            "ok",
            serde_json::json!({}),
        );
        finished.finish("ok");
        let mut handed = 0;
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            handed = stats.lost(Stream::Spans, Outcome::Dropped) - before;
            if handed >= 4 {
                break;
            }
        }
        // Two spans per recorder: the one recorded and the root, for each of
        // the two recorders, and nothing more from the one that finished twice.
        assert_eq!(handed, 4);

        // Spans a stream measures after the recorder finished still reach
        // the sink, once each.
        let recorder = SpanRecorder::new(Uuid::new_v4(), now_ms(), sink.clone());
        let late = recorder.late();
        recorder.finish("ok");
        let start = now_ms();
        late.record([
            PendingSpan::between(
                "boon:tool_loop",
                "proxy_request",
                start,
                start + 5,
                "ok",
                serde_json::json!({}),
            ),
            PendingSpan::between(
                "boon:tool_loop:iter:0",
                "boon:tool_loop",
                start,
                start + 3,
                "ok",
                serde_json::json!({}),
            ),
        ]);
        for _ in 0..40 {
            tokio::time::sleep(Duration::from_millis(50)).await;
            handed = stats.lost(Stream::Spans, Outcome::Dropped) - before;
            if handed >= 7 {
                break;
            }
        }
        assert_eq!(handed, 7);
        let _ = std::fs::remove_dir_all(&wal);
    }
}
