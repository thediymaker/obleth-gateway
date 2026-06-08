//! Prometheus metrics for the data plane.
//!
//! Labels are deliberately low-cardinality (admission class + status class only).
//! Per-tenant breakdowns live in ClickHouse, not Prometheus, to avoid a label
//! explosion across thousands of tenants.

use prometheus::{
    Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts, Registry, TextEncoder,
};

pub struct Metrics {
    registry: Registry,
    requests: IntCounterVec,
    tokens_in: IntCounter,
    tokens_out: IntCounter,
    pub ttft_ms: Histogram,
    pub total_ms: Histogram,
    in_flight: IntGauge,
    queue_depth: IntGauge,
    telemetry_dropped: IntGauge,
    cache_lookups: IntCounterVec,
    tokens_saved: IntCounter,
    mcp_requests: IntCounterVec,
    upstream_attempts: IntCounterVec,
}

impl Metrics {
    pub fn new() -> Self {
        let registry = Registry::new();

        let requests = IntCounterVec::new(
            Opts::new(
                "obleth_requests_total",
                "Requests by admission class and status class",
            ),
            &["admission", "status"],
        )
        .unwrap();
        let tokens_in =
            IntCounter::with_opts(Opts::new("obleth_input_tokens_total", "Total input tokens"))
                .unwrap();
        let tokens_out = IntCounter::with_opts(Opts::new(
            "obleth_output_tokens_total",
            "Total output tokens",
        ))
        .unwrap();
        let ttft_ms = Histogram::with_opts(
            HistogramOpts::new("obleth_ttft_ms", "Time to first token (ms)").buckets(vec![
                5.0, 10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0,
            ]),
        )
        .unwrap();
        let total_ms = Histogram::with_opts(
            HistogramOpts::new("obleth_total_ms", "Total request duration (ms)").buckets(vec![
                10.0, 25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0, 30000.0,
            ]),
        )
        .unwrap();
        let in_flight = IntGauge::with_opts(Opts::new(
            "obleth_in_flight",
            "Requests currently in flight",
        ))
        .unwrap();
        let queue_depth = IntGauge::with_opts(Opts::new(
            "obleth_queue_depth",
            "Requests waiting for admission",
        ))
        .unwrap();
        let telemetry_dropped = IntGauge::with_opts(Opts::new(
            "obleth_telemetry_dropped",
            "Telemetry records dropped due to buffer pressure",
        ))
        .unwrap();
        let cache_lookups = IntCounterVec::new(
            Opts::new(
                "obleth_cache_lookups_total",
                "Response cache lookups by result",
            ),
            &["result"],
        )
        .unwrap();
        let tokens_saved = IntCounter::with_opts(Opts::new(
            "obleth_cache_tokens_saved_total",
            "Tokens served from cache instead of the upstream",
        ))
        .unwrap();
        let mcp_requests = IntCounterVec::new(
            Opts::new(
                "obleth_mcp_requests_total",
                "MCP gateway requests by server and status class",
            ),
            &["server", "status"],
        )
        .unwrap();
        let upstream_attempts = IntCounterVec::new(
            Opts::new(
                "obleth_upstream_attempts_total",
                "Upstream dispatch attempts by outcome (success/retry/timeout/failover/exhausted)",
            ),
            &["outcome"],
        )
        .unwrap();

        registry.register(Box::new(requests.clone())).unwrap();
        registry.register(Box::new(tokens_in.clone())).unwrap();
        registry.register(Box::new(tokens_out.clone())).unwrap();
        registry.register(Box::new(ttft_ms.clone())).unwrap();
        registry.register(Box::new(total_ms.clone())).unwrap();
        registry.register(Box::new(in_flight.clone())).unwrap();
        registry.register(Box::new(queue_depth.clone())).unwrap();
        registry
            .register(Box::new(telemetry_dropped.clone()))
            .unwrap();
        registry.register(Box::new(cache_lookups.clone())).unwrap();
        registry.register(Box::new(tokens_saved.clone())).unwrap();
        registry.register(Box::new(mcp_requests.clone())).unwrap();
        registry
            .register(Box::new(upstream_attempts.clone()))
            .unwrap();

        Metrics {
            registry,
            requests,
            tokens_in,
            tokens_out,
            ttft_ms,
            total_ms,
            in_flight,
            queue_depth,
            telemetry_dropped,
            cache_lookups,
            tokens_saved,
            mcp_requests,
            upstream_attempts,
        }
    }

    /// Record one upstream dispatch attempt outcome. `outcome` is one of
    /// `success`, `retry`, `timeout`, `failover`, or `exhausted`.
    pub fn record_upstream_attempt(&self, outcome: &str) {
        self.upstream_attempts.with_label_values(&[outcome]).inc();
    }

    /// Record an MCP gateway request by server name and HTTP status class.
    pub fn record_mcp(&self, server: &str, status: u16) {
        let status_class = format!("{}xx", status / 100);
        self.mcp_requests
            .with_label_values(&[server, &status_class])
            .inc();
    }

    pub fn record_request(&self, admission: &str, status: u16, input: u32, output: u32) {
        let status_class = format!("{}xx", status / 100);
        self.requests
            .with_label_values(&[admission, &status_class])
            .inc();
        self.tokens_in.inc_by(input as u64);
        self.tokens_out.inc_by(output as u64);
    }

    /// Record a cache lookup result (`hit` or `miss`). On a hit, also credit the
    /// tokens that did not have to be generated by the upstream.
    pub fn record_cache(&self, hit: bool, tokens_saved: u32) {
        self.cache_lookups
            .with_label_values(&[if hit { "hit" } else { "miss" }])
            .inc();
        if hit {
            self.tokens_saved.inc_by(tokens_saved as u64);
        }
    }

    pub fn set_gauges(&self, in_flight: i64, queue_depth: i64, telemetry_dropped: u64) {
        self.in_flight.set(in_flight);
        self.queue_depth.set(queue_depth);
        self.telemetry_dropped.set(telemetry_dropped as i64);
    }

    pub fn encode(&self) -> String {
        let encoder = TextEncoder::new();
        encoder
            .encode_to_string(&self.registry.gather())
            .unwrap_or_default()
    }
}

impl Default for Metrics {
    fn default() -> Self {
        Self::new()
    }
}
