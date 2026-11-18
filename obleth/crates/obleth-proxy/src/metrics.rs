//! Prometheus metrics for the data plane.
//!
//! Labels are deliberately low-cardinality (admission class + status class only).
//! Per-tenant breakdowns live in ClickHouse, not Prometheus, to avoid a label
//! explosion across thousands of tenants.

use prometheus::{
    Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry,
    TextEncoder,
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
    fairshare_replicas: IntGauge,
    fairshare_slot_mode: IntGaugeVec,
    fairshare_cluster_in_flight: IntGauge,
    fairshare_held: IntGauge,
    fairshare_affinity: IntCounterVec,
    capacity_discovery_models: IntGaugeVec,
    telemetry_rows_lost: IntCounterVec,
    cache_lookups: IntCounterVec,
    tokens_saved: IntCounter,
    pub(crate) compression_tokens_saved: IntCounter,
    mcp_requests: IntCounterVec,
    upstream_attempts: IntCounterVec,
    jwt_verify: IntCounterVec,
    jwks_refresh: IntCounterVec,
    knowledge_retrievals: IntCounterVec,
    auto_routes: IntCounterVec,
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
            // Up to two minutes: a reasoning model or a cold backend takes
            // seconds to tens of seconds to the first token, and a quantile
            // past the last finite bucket reads as that bucket's bound.
            HistogramOpts::new("obleth_ttft_ms", "Time to first token (ms)").buckets(vec![
                25.0, 50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0, 30000.0, 60000.0,
                120000.0,
            ]),
        )
        .unwrap();
        let total_ms = Histogram::with_opts(
            // Up to ten minutes, where generations and the request timeout live.
            HistogramOpts::new("obleth_total_ms", "Total request duration (ms)").buckets(vec![
                50.0, 100.0, 250.0, 500.0, 1000.0, 2500.0, 5000.0, 10000.0, 30000.0, 60000.0,
                120000.0, 300000.0, 600000.0,
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
        let fairshare_replicas = IntGauge::with_opts(Opts::new(
            "obleth_fairshare_replicas",
            "Live gateway replicas, as this replica last counted them from Redis heartbeats",
        ))
        .unwrap();
        // Labelled by mode only (a fixed set of four), not by model.
        let fairshare_slot_mode = IntGaugeVec::new(
            Opts::new(
                "obleth_fairshare_shared_slots_mode",
                "How this replica enforces the fairshare limits: 1 for the current mode \
                 (local, split, shared or fallback), 0 for the others",
            ),
            &["mode"],
        )
        .unwrap();
        let fairshare_cluster_in_flight = IntGauge::with_opts(Opts::new(
            "obleth_fairshare_cluster_in_flight",
            "In-flight requests across every gateway replica, as the shared slots last \
             reported them; this replica's own count outside shared mode",
        ))
        .unwrap();
        let fairshare_held = IntGauge::with_opts(Opts::new(
            "obleth_fairshare_held_slots",
            "Slots this replica holds for agent sessions' next steps (session affinity)",
        ))
        .unwrap();
        // Labelled by event only (a fixed set of four).
        let fairshare_affinity = IntCounterVec::new(
            Opts::new(
                "obleth_fairshare_affinity_total",
                "What session affinity did on this replica: jump (a request went ahead of an \
                 older one), hold (a slot was held for a session's next step), redeem (the \
                 next step took it) or expire (it was given back unused)",
            ),
            &["event"],
        )
        .unwrap();
        // Labelled by state only (a fixed set of three), not by model, in
        // keeping with the low-cardinality rule above; per-model detail is in
        // the Management API's capacity discovery view.
        let capacity_discovery_models = IntGaugeVec::new(
            Opts::new(
                "obleth_capacity_discovery_models",
                "Models in the discovered capacity mode, by state: discovered, stale (last value \
                 kept) or fallback (static max_in_flight)",
            ),
            &["state"],
        )
        .unwrap();
        let telemetry_rows_lost = IntCounterVec::new(
            Opts::new(
                "obleth_telemetry_rows_lost_total",
                "Accounting rows that did not reach ClickHouse: dropped (lost) or spilled (written to the WAL, replayed later)",
            ),
            &["stream", "outcome"],
        )
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
        let compression_tokens_saved = IntCounter::with_opts(Opts::new(
            "obleth_compression_tokens_saved_total",
            "Input tokens saved by the compression boon before upstream dispatch",
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
        let jwt_verify = IntCounterVec::new(
            Opts::new(
                "obleth_jwt_verify_total",
                "JWT bearer verification outcomes; not_provisioned/provision_failed are post-verification outcomes counted in addition to ok",
            ),
            &["result"],
        )
        .unwrap();
        let jwks_refresh = IntCounterVec::new(
            Opts::new(
                "obleth_jwks_refresh_total",
                "JWKS fetch outcomes per configured issuer index",
            ),
            &["issuer_index", "result"],
        )
        .unwrap();
        let knowledge_retrievals = IntCounterVec::new(
            Opts::new(
                "obleth_knowledge_retrievals_total",
                "Knowledge retrievals by outcome",
            ),
            &["outcome"],
        )
        .unwrap();
        let auto_routes = IntCounterVec::new(
            Opts::new(
                "obleth_auto_routes_total",
                "auto requests by the model served and why it was chosen",
            ),
            &["model", "reason"],
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
            .register(Box::new(fairshare_replicas.clone()))
            .unwrap();
        registry
            .register(Box::new(fairshare_slot_mode.clone()))
            .unwrap();
        registry
            .register(Box::new(fairshare_cluster_in_flight.clone()))
            .unwrap();
        registry.register(Box::new(fairshare_held.clone())).unwrap();
        registry
            .register(Box::new(fairshare_affinity.clone()))
            .unwrap();
        registry
            .register(Box::new(capacity_discovery_models.clone()))
            .unwrap();
        registry
            .register(Box::new(telemetry_rows_lost.clone()))
            .unwrap();
        registry.register(Box::new(cache_lookups.clone())).unwrap();
        registry.register(Box::new(tokens_saved.clone())).unwrap();
        registry
            .register(Box::new(compression_tokens_saved.clone()))
            .unwrap();
        registry.register(Box::new(mcp_requests.clone())).unwrap();
        registry
            .register(Box::new(upstream_attempts.clone()))
            .unwrap();
        registry.register(Box::new(jwt_verify.clone())).unwrap();
        registry.register(Box::new(jwks_refresh.clone())).unwrap();
        registry
            .register(Box::new(knowledge_retrievals.clone()))
            .unwrap();
        registry.register(Box::new(auto_routes.clone())).unwrap();

        Metrics {
            registry,
            requests,
            tokens_in,
            tokens_out,
            ttft_ms,
            total_ms,
            in_flight,
            queue_depth,
            fairshare_replicas,
            fairshare_slot_mode,
            fairshare_cluster_in_flight,
            fairshare_held,
            fairshare_affinity,
            capacity_discovery_models,
            telemetry_rows_lost,
            cache_lookups,
            tokens_saved,
            compression_tokens_saved,
            mcp_requests,
            upstream_attempts,
            jwt_verify,
            jwks_refresh,
            knowledge_retrievals,
            auto_routes,
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

    pub fn record_compression_saved(&self, saved: u32) {
        self.compression_tokens_saved.inc_by(saved as u64);
    }

    pub fn set_gauges(&self, in_flight: i64, queue_depth: i64) {
        self.in_flight.set(in_flight);
        self.queue_depth.set(queue_depth);
    }

    /// Bring the exported loss counters up to the sink's running totals. The
    /// ledger only grows, so each scrape adds the difference since the last.
    pub fn record_telemetry_losses(&self, stats: &obleth_telemetry::TelemetryStats) {
        for (stream, outcome, total) in stats.losses() {
            let counter = self
                .telemetry_rows_lost
                .with_label_values(&[stream.name(), outcome.name()]);
            let exported = counter.get();
            if total > exported {
                counter.inc_by(total - exported);
            }
        }
    }

    pub fn set_fairshare_replicas(&self, replicas: i64) {
        self.fairshare_replicas.set(replicas);
    }

    pub fn set_fairshare_slot_mode(
        &self,
        mode: obleth_fairshare::SlotMode,
        cluster_in_flight: i64,
    ) {
        for m in obleth_fairshare::SlotMode::ALL {
            self.fairshare_slot_mode
                .with_label_values(&[m.as_str()])
                .set(i64::from(m == mode));
        }
        self.fairshare_cluster_in_flight.set(cluster_in_flight);
    }

    /// Mirror the scheduler's session affinity figures: the held slots now,
    /// and its running totals, which only ever grow.
    pub fn set_session_affinity(&self, held: i64, totals: &obleth_fairshare::AffinityStats) {
        use std::sync::atomic::Ordering;
        self.fairshare_held.set(held);
        for (event, total) in [
            ("jump", &totals.jumps),
            ("hold", &totals.holds),
            ("redeem", &totals.redeemed),
            ("expire", &totals.expired),
        ] {
            let counter = self.fairshare_affinity.with_label_values(&[event]);
            let total = total.load(Ordering::Relaxed);
            counter.inc_by(total.saturating_sub(counter.get()));
        }
    }

    pub fn set_capacity_discovery_models(&self, state: &str, count: i64) {
        self.capacity_discovery_models
            .with_label_values(&[state])
            .set(count);
    }

    pub fn record_jwt_verify(&self, result: &str) {
        self.jwt_verify.with_label_values(&[result]).inc();
    }

    pub fn record_jwks_refresh(&self, issuer_idx: usize, result: &str) {
        self.jwks_refresh
            .with_label_values(&[&issuer_idx.to_string(), result])
            .inc();
    }

    /// Record one knowledge-boon retrieval attempt. `outcome` is one of the
    /// fixed set `hit`/`miss`/`no_query`/`error`/`no_window` — nothing
    /// request-derived (collection id, model, tenant, query text) may ever
    /// become a label
    /// here; per-tenant/per-collection breakdowns live in ClickHouse.
    pub fn record_knowledge_retrieval(&self, outcome: &str) {
        self.knowledge_retrievals
            .with_label_values(&[outcome])
            .inc();
    }

    /// Record an `auto` pick. `model` is a registered model name (operator
    /// configured, so bounded); `reason` is a sticky reason, or `per_turn`
    /// when stickiness is off.
    pub fn record_auto_route(&self, model: &str, reason: &str) {
        self.auto_routes.with_label_values(&[model, reason]).inc();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_compression_savings() {
        let m = Metrics::new();
        m.record_compression_saved(120);
        m.record_compression_saved(30);
        // Exposed via the gather/encode path; assert the counter value directly.
        assert_eq!(m.compression_tokens_saved.get(), 150);
    }

    #[test]
    fn long_requests_land_in_finite_buckets() {
        let m = Metrics::new();
        m.ttft_ms.observe(12_000.0);
        m.total_ms.observe(45_000.0);
        let text = m.encode();
        assert!(
            text.contains("obleth_ttft_ms_bucket{le=\"30000\"} 1"),
            "{text}"
        );
        assert!(
            text.contains("obleth_total_ms_bucket{le=\"60000\"} 1"),
            "{text}"
        );
    }

    #[test]
    fn telemetry_losses_render_as_counters_that_only_grow() {
        use obleth_telemetry::{Outcome, Stream, TelemetryStats};
        let m = Metrics::new();
        let stats = TelemetryStats::default();
        stats.lose(Stream::Usage, Outcome::Spilled, 3);
        stats.lose(Stream::Spans, Outcome::Dropped, 2);
        m.record_telemetry_losses(&stats);
        // A second scrape with unchanged totals adds nothing.
        m.record_telemetry_losses(&stats);
        let text = m.encode();
        assert!(
            text.contains("# TYPE obleth_telemetry_rows_lost_total counter"),
            "{text}"
        );
        let value = |stream: &str, outcome: &str| {
            text.lines()
                .find(|l| {
                    l.starts_with("obleth_telemetry_rows_lost_total{")
                        && l.contains(stream)
                        && l.contains(outcome)
                })
                .map(|l| l.rsplit(' ').next().unwrap().to_string())
        };
        assert_eq!(
            value("stream=\"usage\"", "outcome=\"spilled\"").as_deref(),
            Some("3")
        );
        assert_eq!(
            value("stream=\"spans\"", "outcome=\"dropped\"").as_deref(),
            Some("2")
        );
        assert_eq!(
            value("stream=\"usage\"", "outcome=\"dropped\"").as_deref(),
            Some("0")
        );
    }

    #[test]
    fn jwt_counters_render() {
        let m = Metrics::new();
        m.record_jwt_verify("ok");
        m.record_jwt_verify("expired");
        m.record_jwks_refresh(0, "ok");
        let text = m.encode();
        assert!(text.contains("obleth_jwt_verify_total{result=\"ok\"} 1"));
        assert!(text.contains("obleth_jwt_verify_total{result=\"expired\"} 1"));
        assert!(text.contains("obleth_jwks_refresh_total{issuer_index=\"0\",result=\"ok\"} 1"));
    }

    #[test]
    fn fairshare_replicas_gauge_renders() {
        let m = Metrics::new();
        m.set_fairshare_replicas(3);
        assert!(m.encode().contains("obleth_fairshare_replicas 3"));
    }

    #[test]
    fn fairshare_slot_mode_marks_only_the_current_mode() {
        let m = Metrics::new();
        m.set_fairshare_slot_mode(obleth_fairshare::SlotMode::Shared, 12);
        let text = m.encode();
        assert!(text.contains("obleth_fairshare_shared_slots_mode{mode=\"shared\"} 1"));
        assert!(text.contains("obleth_fairshare_shared_slots_mode{mode=\"fallback\"} 0"));
        assert!(text.contains("obleth_fairshare_cluster_in_flight 12"));
        m.set_fairshare_slot_mode(obleth_fairshare::SlotMode::Fallback, 3);
        let text = m.encode();
        assert!(text.contains("obleth_fairshare_shared_slots_mode{mode=\"shared\"} 0"));
        assert!(text.contains("obleth_fairshare_shared_slots_mode{mode=\"fallback\"} 1"));
    }

    #[test]
    fn session_affinity_figures_render() {
        use std::sync::atomic::Ordering;
        let m = Metrics::new();
        let totals = obleth_fairshare::AffinityStats::default();
        totals.holds.store(5, Ordering::Relaxed);
        totals.redeemed.store(4, Ordering::Relaxed);
        m.set_session_affinity(2, &totals);
        totals.holds.store(7, Ordering::Relaxed);
        m.set_session_affinity(1, &totals);
        let text = m.encode();
        assert!(text.contains("obleth_fairshare_held_slots 1"));
        assert!(text.contains("obleth_fairshare_affinity_total{event=\"hold\"} 7"));
        assert!(text.contains("obleth_fairshare_affinity_total{event=\"redeem\"} 4"));
        assert!(text.contains("obleth_fairshare_affinity_total{event=\"expire\"} 0"));
    }

    #[test]
    fn capacity_discovery_gauge_renders_per_state() {
        let m = Metrics::new();
        m.set_capacity_discovery_models("discovered", 4);
        m.set_capacity_discovery_models("fallback", 1);
        let text = m.encode();
        assert!(text.contains("obleth_capacity_discovery_models{state=\"discovered\"} 4"));
        assert!(text.contains("obleth_capacity_discovery_models{state=\"fallback\"} 1"));
    }
}
