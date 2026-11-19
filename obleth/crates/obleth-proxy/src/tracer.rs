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
            true,
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
        let _ = std::fs::remove_dir_all(&wal);
    }
}
