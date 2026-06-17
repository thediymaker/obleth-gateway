use obleth_telemetry::{SpanRecord, TelemetrySink};
use uuid::Uuid;

pub struct SpanRecorder {
    request_id: Uuid,
    request_start_ms: i64,
    spans: Vec<SpanRecord>,
    sink: TelemetrySink,
}

impl SpanRecorder {
    pub fn new(request_id: Uuid, request_start_ms: i64, sink: TelemetrySink) -> Self {
        SpanRecorder {
            request_id,
            request_start_ms,
            spans: Vec::with_capacity(16),
            sink,
        }
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
        self.record(span_name, parent_span, span_start_ms, duration_ms, status, attributes);
    }

    /// Record the root `proxy_request` span with total request duration, then
    /// send all buffered spans to the telemetry sink.
    pub fn finish(mut self, status: &str) {
        let duration_ms = (now_ms() - self.request_start_ms).max(0) as u32;
        self.spans.push(SpanRecord {
            request_id: self.request_id,
            span_name: "proxy_request".to_string(),
            parent_span: String::new(),
            start_ms: self.request_start_ms,
            duration_ms,
            status: status.to_string(),
            attributes: "{}".to_string(),
        });
        let sink = self.sink;
        for span in self.spans {
            sink.record_span(span);
        }
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
