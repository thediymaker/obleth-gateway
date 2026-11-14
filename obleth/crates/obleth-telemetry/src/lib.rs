//! Async usage/telemetry ledger writer (ClickHouse) with a local WAL fallback.
//!
//! The request hot path calls [`TelemetrySink::record`], which is a non-blocking
//! channel send — it never awaits ClickHouse. A background task batches rows and
//! inserts them. If ClickHouse is unavailable, batches always spill (regardless of
//! `OBLETH_FAIL_OPEN`) to a local write-ahead log and are replayed once it recovers,
//! so the user's request is never blocked by the ledger.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clickhouse::{Client, Row};
use obleth_config::{KeyRejection, KeyRejectionReason, UsageRecord};
use serde::{Deserialize, Serialize};
mod wal;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

const BATCH_MAX: usize = 500;
/// WAL replay keeps its own one-second cadence, independent of the flush
/// interval, so a long flush interval doesn't slow recovery after an outage.
/// Replay batches are already large (up to 1 MiB of rows each).
const REPLAY_INTERVAL: Duration = Duration::from_secs(1);
/// Bound on the reachability probe that precedes schema setup: the ClickHouse
/// client has no connect timeout, and a blackholed host would otherwise hang
/// boot (or the flusher) for the OS TCP timeout.
const SCHEMA_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// `usage_meta` key recording that `usage_daily` has been seeded from history.
const BACKFILL_MARKER: &str = "backfilled_v1";
/// `usage_meta` key under which booting replicas elect one backfiller.
const BACKFILL_CLAIM: &str = "backfill_claim_v1";
/// `usage_meta` key recording that `usage_daily` rows written before it
/// counted platform failures have been given their count.
const PLATFORM_ERRORS_MARKER: &str = "platform_errors_v1";
/// `usage_meta` key under which booting replicas elect who adds that count.
const PLATFORM_ERRORS_CLAIM: &str = "platform_errors_claim_v1";
/// How long a claimant waits for concurrent claims to become visible.
const BACKFILL_CLAIM_SETTLE: Duration = Duration::from_secs(2);
/// Upper bound on `TelemetrySink::shutdown` draining both flushers.
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(20);
/// A failing schema setup retries on every flush tick (~1s); cap how often
/// that repeats as a `warn!` so a down/misconfigured ClickHouse doesn't spam
/// the log, while still keeping it visible at the default log level (before
/// this it was `debug!`, which production never shows).
const SCHEMA_WARN_INTERVAL: Duration = Duration::from_secs(60);
/// Consecutive schema-setup failures after which we escalate once to
/// `error!`. Failing this persistently is much more likely a configuration
/// problem (bad grants, DDL the user can't run) than a transient outage.
const SCHEMA_FAILURE_ALERT_THRESHOLD: u64 = 10;
/// Days a client address and User-Agent are kept on a ledger row, unless
/// `OBLETH_CLIENT_IP_RETENTION_DAYS` says otherwise. The rest of the row,
/// country included, stays as long as the row does.
pub const DEFAULT_CLIENT_IP_RETENTION_DAYS: u32 = 90;
/// `usage_meta` key recording the client address retention the column TTLs
/// were last set to, so a changed setting is applied once, not on every boot.
const CLIENT_IP_TTL_META: &str = "client_ip_ttl_days";
/// Refused uses of one key from one address for one reason are folded into
/// one `key_rejections` row per window of this length.
pub const REJECTION_WINDOW: Duration = Duration::from_secs(10);
/// Distinct (key, address, reason) rows held per window. A flood from more
/// addresses than this within one window is counted, not stored.
const REJECTION_WINDOW_MAX_ROWS: usize = 10_000;

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("clickhouse: {0}")]
    Click(#[from] clickhouse::error::Error),
    #[error("invalid clickhouse database name: {0:?}")]
    InvalidDatabase(String),
    #[error("clickhouse insert timed out")]
    InsertTimeout,
    #[error("clickhouse did not answer within {0:?}")]
    Unreachable(Duration),
}

/// Borrowed ClickHouse row mirror of [`UsageRecord`], so a batch insert
/// serializes straight from the buffered records instead of cloning every
/// string field per row.
#[derive(Debug, Row, Serialize)]
struct UsageRow<'a> {
    #[serde(with = "clickhouse::serde::uuid")]
    request_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    tenant_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    key_id: Uuid,
    model: &'a str,
    admission: &'a str,
    weight: i64,
    input_tokens: u32,
    output_tokens: u32,
    cached_input_tokens: u32,
    estimated_tokens: u32,
    queue_wait_ms: u32,
    ttft_ms: u32,
    total_ms: u32,
    status_code: u16,
    cache_status: &'a str,
    cost_usd: f64,
    energy_wh: f64,
    energy_cost_usd: f64,
    co2_g: f64,
    ts_ms: i64,
    session_id: &'a str,
    session_id_source: &'a str,
    request_type: &'a str,
    device_id: &'a str,
    end_user: &'a str,
    #[serde(with = "clickhouse::serde::uuid")]
    parent_request_id: Uuid,
    model_variant: &'a str,
    reasoning_effort: &'a str,
    reasoning_effort_requested: &'a str,
    model_family: &'a str,
    key_name: &'a str,
    tenant_name: &'a str,
    streamed: bool,
    error_class: &'a str,
    error_reason: &'a str,
    client_ip: &'a str,
    client_network: &'a str,
    client_country: &'a str,
    user_agent: &'a str,
}

impl<'a> From<&'a UsageRecord> for UsageRow<'a> {
    fn from(r: &'a UsageRecord) -> Self {
        UsageRow {
            request_id: r.request_id,
            tenant_id: r.tenant_id,
            key_id: r.key_id,
            model: &r.model,
            admission: &r.admission,
            weight: r.weight,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            cached_input_tokens: r.cached_input_tokens,
            estimated_tokens: r.estimated_tokens,
            queue_wait_ms: r.queue_wait_ms,
            ttft_ms: r.ttft_ms,
            total_ms: r.total_ms,
            status_code: r.status_code,
            cache_status: &r.cache_status,
            cost_usd: finite_or_zero(r.cost_usd),
            energy_wh: finite_or_zero(r.energy_wh),
            energy_cost_usd: finite_or_zero(r.energy_cost_usd),
            co2_g: finite_or_zero(r.co2_g),
            ts_ms: r.ts_ms,
            session_id: &r.session_id,
            session_id_source: &r.session_id_source,
            request_type: &r.request_type,
            device_id: &r.device_id,
            end_user: &r.end_user,
            parent_request_id: r.parent_request_id,
            model_variant: &r.model_variant,
            reasoning_effort: &r.reasoning_effort,
            reasoning_effort_requested: &r.reasoning_effort_requested,
            model_family: &r.model_family,
            key_name: &r.key_name,
            tenant_name: &r.tenant_name,
            streamed: r.streamed,
            error_class: &r.error_class,
            error_reason: &r.error_reason,
            client_ip: &r.client_ip,
            client_network: &r.client_network,
            client_country: &r.client_country,
            user_agent: &r.user_agent,
        }
    }
}

/// One NaN in `usage` turns every `sum()` over it, including the permanent
/// `usage_daily` rollup, into NaN.
fn finite_or_zero(value: f64) -> f64 {
    if value.is_finite() {
        value
    } else {
        0.0
    }
}

/// Stable across processes so a batch replayed from the WAL carries the same
/// `insert_deduplication_token` as the insert that timed out, whether or not
/// ClickHouse committed it.
fn dedup_token(batch: &[UsageRecord]) -> String {
    // FNV-1a: std's hashers are not guaranteed stable across Rust releases.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for record in batch {
        for byte in record.request_id.as_bytes() {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    format!("usage-{}-{hash:016x}", batch.len())
}

/// Public record type that the proxy constructs for each span.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanRecord {
    pub request_id: uuid::Uuid,
    pub span_name: String,
    pub parent_span: String,
    pub start_ms: i64,
    pub duration_ms: u32,
    pub status: String,
    pub attributes: String,
    pub session_id: String,
    pub session_id_source: String,
}

/// Borrowed ClickHouse row mirror of [`SpanRecord`] for batch insert.
#[derive(Debug, Row, Serialize)]
struct SpanRow<'a> {
    #[serde(with = "clickhouse::serde::uuid")]
    request_id: uuid::Uuid,
    span_name: &'a str,
    parent_span: &'a str,
    start_ms: i64,
    duration_ms: u32,
    status: &'a str,
    attributes: &'a str,
    session_id: &'a str,
    session_id_source: &'a str,
}

impl<'a> From<&'a SpanRecord> for SpanRow<'a> {
    fn from(r: &'a SpanRecord) -> Self {
        SpanRow {
            request_id: r.request_id,
            span_name: &r.span_name,
            parent_span: &r.parent_span,
            start_ms: r.start_ms,
            duration_ms: r.duration_ms,
            status: &r.status,
            attributes: &r.attributes,
            session_id: &r.session_id,
            session_id_source: &r.session_id_source,
        }
    }
}

#[derive(Debug, Default)]
pub struct TelemetryStats {
    pub recorded: AtomicU64,
    pub dropped: AtomicU64,
    pub waled: AtomicU64,
}

/// Cloneable handle used by the data plane to emit usage records.
#[derive(Clone)]
pub struct TelemetrySink {
    tx: mpsc::Sender<UsageRecord>,
    tx_spans: mpsc::Sender<SpanRecord>,
    /// Drain requests: each flusher empties its queue, flushes, then acks.
    drain: mpsc::Sender<oneshot::Sender<()>>,
    drain_spans: mpsc::Sender<oneshot::Sender<()>>,
    stats: Arc<TelemetryStats>,
    /// Mirrors the flusher's view of schema readiness so the caller can tell
    /// "connected, schema applied" from "up in spill mode" right after boot,
    /// without reaching into the flusher task.
    schema_ready: Arc<AtomicBool>,
    /// Refused uses of real keys, folded per window (see [`RejectionWindow`]).
    rejections: Arc<RejectionFlusher>,
}

impl TelemetrySink {
    /// Connect, ensure schema exists, and spawn the background flusher.
    ///
    /// An unreachable ClickHouse does not fail start: the sink comes up in
    /// spill mode (usage goes to the WAL, spans are dropped) and the flusher
    /// retries schema setup with backoff before its first insert. Only an
    /// invalid database name is an error.
    ///
    /// Rows are inserted every `flush_interval`, or sooner once `BATCH_MAX`
    /// are buffered.
    ///
    /// `_fail_open` is ignored: telemetry always spills to the WAL on failure.
    /// `OBLETH_FAIL_OPEN` governs budget admission only, and a ledger outage
    /// must never cost accounting data. Kept so callers compile unchanged.
    pub async fn start(
        url: &str,
        database: &str,
        user: &str,
        password: &str,
        wal_path: &str,
        flush_interval: Duration,
        _fail_open: bool,
    ) -> Result<Self, TelemetryError> {
        if !is_valid_identifier(database) {
            return Err(TelemetryError::InvalidDatabase(database.to_string()));
        }
        let mut schema_client = Client::default().with_url(url).with_user(user);
        if !password.is_empty() {
            schema_client = schema_client.with_password(password);
        }
        let schema_ready = match apply_schema(&schema_client, database).await {
            Ok(()) => true,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    "clickhouse unavailable at boot; spilling usage to the WAL and retrying schema setup"
                );
                false
            }
        };
        let schema_ready = Arc::new(AtomicBool::new(schema_ready));
        // The schema client stays database-less: a `database` URL parameter
        // naming a database that does not exist yet fails every statement,
        // including the `CREATE DATABASE` that would fix it.
        let client = schema_client.clone().with_database(database);

        let (tx, rx) = mpsc::channel(10_000);
        let (tx_spans, rx_spans) = mpsc::channel::<SpanRecord>(10_000);
        let stats = Arc::new(TelemetryStats::default());
        let flusher = Flusher {
            client: client.clone(),
            schema_client,
            database: database.to_string(),
            schema_ready: schema_ready.clone(),
            wal: wal::Wal::new(wal_path),
            wal_path: wal_path.to_string(),
            flush_interval,
            backoff: Backoff::default(),
            replay_backoff: Backoff::default(),
            stats: stats.clone(),
            schema_failures: 0,
            last_schema_warn: None,
        };
        let spans_flusher = SpansFlusher {
            client,
            schema_ready: schema_ready.clone(),
            flush_interval,
            stats: stats.clone(),
        };
        let rejections = Arc::new(RejectionFlusher {
            client: spans_flusher.client.clone(),
            schema_ready: schema_ready.clone(),
            window: std::sync::Mutex::new(RejectionWindow::default()),
            stats: stats.clone(),
        });
        let (drain, drain_rx) = mpsc::channel(4);
        let (drain_spans, drain_spans_rx) = mpsc::channel(4);
        tokio::spawn(flusher.run(rx, drain_rx));
        tokio::spawn(spans_flusher.run(rx_spans, drain_spans_rx));
        tokio::spawn(rejections.clone().run());
        Ok(TelemetrySink {
            tx,
            tx_spans,
            drain,
            drain_spans,
            stats,
            schema_ready,
            rejections,
        })
    }

    pub fn stats(&self) -> Arc<TelemetryStats> {
        self.stats.clone()
    }

    /// Whether the ClickHouse schema is applied right now — `false` means
    /// usage rows are spilling to the WAL, either because setup hasn't
    /// succeeded since boot or because it is currently down.
    pub fn schema_ready(&self) -> bool {
        self.schema_ready.load(Ordering::Acquire)
    }

    /// Non-blocking emit. Drops (and counts) the record if the buffer is full so
    /// the hot path is never stalled by the ledger.
    pub fn record(&self, record: UsageRecord) {
        match self.tx.try_send(record) {
            Ok(()) => {
                self.stats.recorded.fetch_add(1, Ordering::Relaxed);
            }
            Err(_) => {
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Non-blocking span emit. Silently drops if the buffer is full so the hot
    /// path is never stalled by the tracer.
    pub fn record_span(&self, span: SpanRecord) {
        let _ = self.tx_spans.try_send(span);
    }

    /// Record a refused use of a real key. Never waits on ClickHouse: the
    /// refusal is folded into the current window in memory (one row per key,
    /// address and reason, with a count) and written when the window closes.
    pub fn record_rejection(&self, rejection: KeyRejection) {
        self.rejections.add(rejection);
    }

    /// Write everything recorded so far before the process exits, bounded so a
    /// wedged ledger can't hold exit. Each flusher drains its queue and runs a
    /// final flush (ClickHouse, or the WAL if the insert fails) before acking.
    /// `record` keeps working afterwards; the flushers keep running.
    pub async fn shutdown(&self) {
        let drained = tokio::time::timeout(SHUTDOWN_TIMEOUT, async {
            let mut acks = Vec::new();
            for drain in [&self.drain, &self.drain_spans] {
                let (ack, done) = oneshot::channel();
                if drain.send(ack).await.is_ok() {
                    acks.push(done);
                }
            }
            for done in acks {
                let _ = done.await;
            }
            self.rejections.flush().await;
        })
        .await;
        if drained.is_err() {
            tracing::warn!(
                timeout = ?SHUTDOWN_TIMEOUT,
                "telemetry did not finish flushing before shutdown; queued records may be lost"
            );
        }
    }
}

struct Flusher {
    client: Client,
    schema_client: Client,
    database: String,
    schema_ready: Arc<AtomicBool>,
    wal: wal::Wal,
    /// Kept alongside `wal` (which doesn't expose its path) so a persistent
    /// schema failure can name where usage is spilling to.
    wal_path: String,
    flush_interval: Duration,
    backoff: Backoff,
    replay_backoff: Backoff,
    stats: Arc<TelemetryStats>,
    /// Consecutive schema-setup failures since the last success; resets on
    /// the next success. Drives the warn rate limit and the one-time escalation
    /// to `error!`.
    schema_failures: u64,
    last_schema_warn: Option<tokio::time::Instant>,
}

impl Flusher {
    async fn run(
        mut self,
        mut rx: mpsc::Receiver<UsageRecord>,
        mut drain: mpsc::Receiver<oneshot::Sender<()>>,
    ) {
        let mut buf: Vec<UsageRecord> = Vec::with_capacity(BATCH_MAX);
        // The first flush is one interval after boot; replay starts at once so
        // spill left by a previous run goes back to ClickHouse right away.
        let mut ticker = tokio::time::interval_at(
            tokio::time::Instant::now() + self.flush_interval,
            self.flush_interval,
        );
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut replay = tokio::time::interval(REPLAY_INTERVAL);
        replay.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                Some(ack) = drain.recv() => {
                    // Everything sent before the drain request is already queued.
                    // Shutdown has a fixed budget: no untimed schema setup
                    // (claim sleep, backfill) here; spill instead.
                    while let Ok(rec) = rx.try_recv() {
                        buf.push(rec);
                        if buf.len() >= BATCH_MAX {
                            self.flush(&mut buf, false).await;
                        }
                    }
                    self.flush(&mut buf, false).await;
                    let _ = ack.send(());
                }
                maybe = rx.recv() => {
                    match maybe {
                        Some(rec) => {
                            buf.push(rec);
                            if buf.len() >= BATCH_MAX {
                                self.flush(&mut buf, true).await;
                            }
                        }
                        None => {
                            self.flush(&mut buf, false).await;
                            break;
                        }
                    }
                }
                _ = ticker.tick() => {
                    self.flush(&mut buf, true).await;
                }
                _ = replay.tick() => {
                    self.replay_wal().await;
                }
            }
        }
    }

    /// Insert, or spill to the WAL on any failure. Records are dropped only
    /// when the WAL itself refuses them (disk/segment cap).
    async fn flush(&mut self, buf: &mut Vec<UsageRecord>, setup_schema: bool) {
        if buf.is_empty() {
            return;
        }
        let mut batch = std::mem::take(buf);
        if !self.clickhouse_ready(setup_schema).await {
            self.write_wal(&batch).await;
            batch.clear();
            *buf = batch;
            return;
        }
        match self.insert(&batch).await {
            Ok(()) => self.backoff.reset(),
            Err(e) => {
                self.backoff.fail();
                tracing::warn!(error = %e, count = batch.len(), "clickhouse insert failed");
                self.write_wal(&batch).await;
            }
        }
        batch.clear();
        *buf = batch;
    }

    /// False while backing off, or until schema setup (skipped at boot because
    /// ClickHouse was down) has succeeded. With `setup_schema` false a missing
    /// schema is reported as not ready rather than retried.
    async fn clickhouse_ready(&mut self, setup_schema: bool) -> bool {
        if !self.backoff.ready() {
            return false;
        }
        if self.schema_ready.load(Ordering::Acquire) {
            return true;
        }
        if !setup_schema {
            return false;
        }
        match apply_schema(&self.schema_client, &self.database).await {
            Ok(()) => {
                self.schema_ready.store(true, Ordering::Release);
                self.schema_failures = 0;
                self.last_schema_warn = None;
                tracing::info!("clickhouse schema applied; leaving telemetry spill mode");
                true
            }
            Err(e) => {
                self.backoff.fail();
                self.schema_failures += 1;
                let now = tokio::time::Instant::now();
                let should_warn = match self.last_schema_warn {
                    Some(last) => now.saturating_duration_since(last) >= SCHEMA_WARN_INTERVAL,
                    None => true,
                };
                if should_warn {
                    self.last_schema_warn = Some(now);
                    tracing::warn!(
                        error = %e,
                        attempts = self.schema_failures,
                        "clickhouse schema setup failing"
                    );
                } else {
                    tracing::debug!(error = %e, attempts = self.schema_failures, "clickhouse schema setup still failing");
                }
                if self.schema_failures == SCHEMA_FAILURE_ALERT_THRESHOLD {
                    tracing::error!(
                        attempts = self.schema_failures,
                        wal_path = %self.wal_path,
                        "clickhouse schema setup failing for {} attempts; usage rows are spilling to {}",
                        self.schema_failures,
                        self.wal_path
                    );
                }
                false
            }
        }
    }

    async fn insert(&self, batch: &[UsageRecord]) -> Result<(), TelemetryError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut insert = self
                .client
                .insert("usage")?
                .with_option("insert_deduplication_token", dedup_token(batch))
                // Without this the rollup view still fires for a block that
                // `usage` discarded as a duplicate, double-counting usage_daily.
                .with_option("deduplicate_blocks_in_dependent_materialized_views", "1");
            for rec in batch {
                insert.write(&UsageRow::from(rec)).await?;
            }
            insert.end().await?;
            Ok(())
        })
        .await
        .map_err(|_| TelemetryError::InsertTimeout)?
    }

    async fn write_wal(&self, batch: &[UsageRecord]) {
        match self.wal.append(batch).await {
            Ok(()) => {
                self.stats
                    .waled
                    .fetch_add(batch.len() as u64, Ordering::Relaxed);
            }
            Err(e) => {
                self.stats
                    .dropped
                    .fetch_add(batch.len() as u64, Ordering::Relaxed);
                tracing::error!(error = %e, count = batch.len(), "telemetry WAL spill failed; records dropped");
            }
        }
    }

    async fn replay_wal(&mut self) {
        if !self.replay_backoff.ready() || !self.clickhouse_ready(true).await {
            return;
        }
        let batch = match self.wal.next_batch().await {
            Ok(Some(batch)) => batch,
            Ok(None) => return,
            Err(e) => {
                self.replay_backoff.fail();
                tracing::error!(error = %e, "telemetry WAL read failed; retaining spill");
                return;
            }
        };
        if !batch.records.is_empty() {
            if let Err(e) = self.insert(&batch.records).await {
                self.backoff.fail();
                tracing::warn!(error = %e, "telemetry WAL replay failed");
                return;
            }
        }
        match self.wal.commit(&batch).await {
            Ok(()) => {
                self.backoff.reset();
                self.replay_backoff.reset();
                tracing::info!(count = batch.records.len(), "replayed telemetry WAL batch");
            }
            Err(e) => {
                self.replay_backoff.fail();
                tracing::error!(error = %e, "telemetry WAL checkpoint failed; batch may replay again");
            }
        }
    }
}

struct Backoff {
    next: tokio::time::Instant,
    delay: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            next: tokio::time::Instant::now(),
            delay: Duration::from_secs(1),
        }
    }
}

impl Backoff {
    fn ready(&self) -> bool {
        tokio::time::Instant::now() >= self.next
    }
    fn reset(&mut self) {
        *self = Self::default();
    }
    fn fail(&mut self) {
        self.next = tokio::time::Instant::now() + self.delay;
        self.delay = (self.delay * 2).min(Duration::from_secs(60));
    }
}

/// Refusals of real keys gathered over one [`REJECTION_WINDOW`]: at most
/// one row per (key, client address, reason), whose `count` grows with each
/// repeat. A client hammering a disabled key therefore costs one map update
/// per request and one ledger row per window, never a row per request.
#[derive(Default)]
struct RejectionWindow {
    rows: std::collections::HashMap<(Uuid, String, KeyRejectionReason), KeyRejection>,
    /// Refusals not stored because the window was already full.
    overflow: u64,
}

impl RejectionWindow {
    fn add(&mut self, rejection: KeyRejection) {
        let id = (
            rejection.key_id,
            rejection.client_ip.clone(),
            rejection.reason,
        );
        if let Some(row) = self.rows.get_mut(&id) {
            row.count = row.count.saturating_add(rejection.count.max(1));
            return;
        }
        if self.rows.len() >= REJECTION_WINDOW_MAX_ROWS {
            self.overflow = self.overflow.saturating_add(rejection.count.max(1));
            return;
        }
        let mut rejection = rejection;
        rejection.count = rejection.count.max(1);
        self.rows.insert(id, rejection);
    }

    /// The window's rows, oldest first, leaving it empty.
    fn take(&mut self) -> (Vec<KeyRejection>, u64) {
        let mut rows: Vec<KeyRejection> = self.rows.drain().map(|(_, r)| r).collect();
        rows.sort_by_key(|r| r.ts_ms);
        (rows, std::mem::take(&mut self.overflow))
    }
}

/// Borrowed ClickHouse row mirror of [`KeyRejection`].
#[derive(Debug, Row, Serialize)]
struct KeyRejectionRow<'a> {
    ts_ms: i64,
    #[serde(with = "clickhouse::serde::uuid")]
    key_id: Uuid,
    key_name: &'a str,
    #[serde(with = "clickhouse::serde::uuid")]
    tenant_id: Uuid,
    tenant_name: &'a str,
    reason: &'a str,
    client_ip: &'a str,
    client_country: &'a str,
    user_agent: &'a str,
    path: &'a str,
    count: u64,
}

impl<'a> From<&'a KeyRejection> for KeyRejectionRow<'a> {
    fn from(r: &'a KeyRejection) -> Self {
        KeyRejectionRow {
            ts_ms: r.ts_ms,
            key_id: r.key_id,
            key_name: &r.key_name,
            tenant_id: r.tenant_id,
            tenant_name: &r.tenant_name,
            reason: r.reason.as_str(),
            client_ip: &r.client_ip,
            client_country: &r.client_country,
            user_agent: &r.user_agent,
            path: &r.path,
            count: r.count,
        }
    }
}

/// Writes each closed [`RejectionWindow`] to `key_rejections`. Like spans,
/// these rows are best effort: a failed insert is logged and dropped, never
/// retried in the request path.
struct RejectionFlusher {
    client: Client,
    schema_ready: Arc<AtomicBool>,
    window: std::sync::Mutex<RejectionWindow>,
    stats: Arc<TelemetryStats>,
}

impl RejectionFlusher {
    fn add(&self, rejection: KeyRejection) {
        // Never held across an await; a poisoned lock still holds a usable map.
        let mut window = self
            .window
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        window.add(rejection);
    }

    async fn run(self: Arc<Self>) {
        let mut ticker = tokio::time::interval_at(
            tokio::time::Instant::now() + REJECTION_WINDOW,
            REJECTION_WINDOW,
        );
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            self.flush().await;
        }
    }

    async fn flush(&self) {
        let (rows, overflow) = {
            let mut window = self
                .window
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            window.take()
        };
        if overflow > 0 {
            tracing::warn!(
                overflow,
                "refused key uses from too many addresses in one window; some were counted but not recorded"
            );
        }
        if rows.is_empty() {
            return;
        }
        if !self.schema_ready.load(Ordering::Acquire) {
            self.stats
                .dropped
                .fetch_add(rows.len() as u64, Ordering::Relaxed);
            tracing::debug!(
                count = rows.len(),
                "key rejections dropped; clickhouse schema not ready"
            );
            return;
        }
        let insert = tokio::time::timeout(Duration::from_secs(5), async {
            let mut ins = self.client.insert("key_rejections")?;
            for row in &rows {
                ins.write(&KeyRejectionRow::from(row)).await?;
            }
            ins.end().await?;
            Ok::<(), clickhouse::error::Error>(())
        })
        .await;
        match insert {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                tracing::warn!(error = %e, count = rows.len(), "key rejections insert failed")
            }
            Err(_) => tracing::warn!(count = rows.len(), "key rejections insert timed out"),
        }
    }
}

/// Days client addresses and User-Agents are kept on ledger rows:
/// `OBLETH_CLIENT_IP_RETENTION_DAYS`, else [`DEFAULT_CLIENT_IP_RETENTION_DAYS`].
/// Clamped to 1..=3650 days.
pub fn client_ip_retention_days() -> u32 {
    parse_retention_days(
        std::env::var("OBLETH_CLIENT_IP_RETENTION_DAYS")
            .ok()
            .as_deref(),
    )
}

fn parse_retention_days(raw: Option<&str>) -> u32 {
    raw.and_then(|v| v.trim().parse::<u32>().ok())
        .map(|d| d.clamp(1, 3650))
        .unwrap_or(DEFAULT_CLIENT_IP_RETENTION_DAYS)
}

/// The column TTL clause that expires a client-identifying column `days`
/// after its row was written. Only the column is cleared (to its default,
/// `''`); the row stays.
fn client_ip_ttl(days: u32) -> String {
    format!("TTL toDate(fromUnixTimestamp64Milli(ts_ms)) + INTERVAL {days} DAY")
}

struct SpansFlusher {
    client: Client,
    schema_ready: Arc<AtomicBool>,
    flush_interval: Duration,
    #[allow(dead_code)]
    stats: Arc<TelemetryStats>,
}

impl SpansFlusher {
    async fn run(
        self,
        mut rx: mpsc::Receiver<SpanRecord>,
        mut drain: mpsc::Receiver<oneshot::Sender<()>>,
    ) {
        let mut buf: Vec<SpanRecord> = Vec::with_capacity(BATCH_MAX);
        let mut ticker = tokio::time::interval(self.flush_interval);
        loop {
            tokio::select! {
                Some(ack) = drain.recv() => {
                    while let Ok(rec) = rx.try_recv() {
                        buf.push(rec);
                        if buf.len() >= BATCH_MAX {
                            self.flush(&mut buf).await;
                        }
                    }
                    self.flush(&mut buf).await;
                    let _ = ack.send(());
                }
                maybe = rx.recv() => {
                    match maybe {
                        Some(rec) => {
                            buf.push(rec);
                            if buf.len() >= BATCH_MAX {
                                self.flush(&mut buf).await;
                            }
                        }
                        None => {
                            self.flush(&mut buf).await;
                            break;
                        }
                    }
                }
                _ = ticker.tick() => {
                    self.flush(&mut buf).await;
                }
            }
        }
    }

    async fn flush(&self, buf: &mut Vec<SpanRecord>) {
        if buf.is_empty() {
            return;
        }
        let batch = std::mem::take(buf);
        let count = batch.len();
        // Spans are disposable; without a schema the insert can only fail.
        if !self.schema_ready.load(Ordering::Acquire) {
            tracing::debug!(count, "spans dropped; clickhouse schema not ready");
            return;
        }
        if let Err(e) = self.insert(&batch).await {
            tracing::warn!(error = %e, count, "spans insert failed");
        } else {
            tracing::debug!(count, "spans flushed to ClickHouse");
        }
    }

    async fn insert(&self, batch: &[SpanRecord]) -> Result<(), TelemetryError> {
        tokio::time::timeout(Duration::from_secs(5), async {
            let mut ins = self.client.insert("spans")?;
            for rec in batch {
                ins.write(&SpanRow::from(rec)).await?;
            }
            ins.end().await?;
            Ok(())
        })
        .await
        .map_err(|_| TelemetryError::InsertTimeout)?
    }
}

/// True when `name` is a safe bare SQL identifier (letters, digits, underscore,
/// not starting with a digit). Used to guard identifiers that must be string-
/// interpolated into ClickHouse DDL.
fn is_valid_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Probe reachability under a timeout, then run the (untimed) schema setup:
/// cancelling it midway could interrupt the one-time rollup backfill.
async fn apply_schema(client: &Client, database: &str) -> Result<(), TelemetryError> {
    tokio::time::timeout(SCHEMA_PROBE_TIMEOUT, client.query("SELECT 1").execute())
        .await
        .map_err(|_| TelemetryError::Unreachable(SCHEMA_PROBE_TIMEOUT))??;
    ensure_schema(client, database).await
}

async fn ensure_schema(client: &Client, database: &str) -> Result<(), TelemetryError> {
    // The database name is interpolated directly into DDL (ClickHouse has no
    // bind-parameter support for identifiers), so reject anything that isn't a
    // plain SQL identifier as defense-in-depth even though it comes from trusted
    // config rather than user input.
    if !is_valid_identifier(database) {
        return Err(TelemetryError::InvalidDatabase(database.to_string()));
    }
    client
        .query(&format!("CREATE DATABASE IF NOT EXISTS {database}"))
        .execute()
        .await?;
    let ip_days = client_ip_retention_days();
    let ip_ttl = client_ip_ttl(ip_days);
    let ddl = format!(
        "CREATE TABLE IF NOT EXISTS {database}.usage (
            request_id UUID,
            tenant_id UUID,
            key_id UUID,
            model String,
            admission LowCardinality(String),
            weight Int64,
            input_tokens UInt32,
            output_tokens UInt32,
            cached_input_tokens UInt32 DEFAULT 0,
            estimated_tokens UInt32,
            queue_wait_ms UInt32,
            ttft_ms UInt32,
            total_ms UInt32,
            status_code UInt16,
            cache_status LowCardinality(String) DEFAULT 'off',
            cost_usd Float64 DEFAULT 0,
            energy_wh Float64 DEFAULT 0,
            energy_cost_usd Float64 DEFAULT 0,
            co2_g Float64 DEFAULT 0,
            ts_ms Int64,
            session_id String DEFAULT '',
            session_id_source LowCardinality(String) DEFAULT '',
            request_type LowCardinality(String) DEFAULT '',
            device_id String DEFAULT '',
            end_user String DEFAULT '',
            parent_request_id UUID DEFAULT toUUID('00000000-0000-0000-0000-000000000000'),
            model_variant String DEFAULT '',
            reasoning_effort LowCardinality(String) DEFAULT '',
            reasoning_effort_requested LowCardinality(String) DEFAULT '',
            model_family String DEFAULT '',
            key_name String DEFAULT '',
            tenant_name String DEFAULT '',
            streamed Bool DEFAULT false,
            error_class LowCardinality(String) DEFAULT '',
            error_reason String DEFAULT '',
            client_ip String DEFAULT '' {ip_ttl},
            client_network String DEFAULT '' {ip_ttl},
            client_country LowCardinality(String) DEFAULT '',
            user_agent String DEFAULT '' {ip_ttl},
            ts DateTime64(3) MATERIALIZED fromUnixTimestamp64Milli(ts_ms),
            INDEX idx_ts_ms ts_ms TYPE minmax GRANULARITY 4
        ) ENGINE = MergeTree()
        PARTITION BY toYYYYMMDD(ts)
        ORDER BY (tenant_id, ts_ms)"
    );
    client.query(&ddl).execute().await?;
    // The sort key leads with tenant_id, so cross-tenant time-range reads (the
    // live request log, the usage series) cannot use the primary index. A
    // minmax skip index on ts_ms lets those queries skip granules outside the
    // requested window. Idempotent add for tables created before the index
    // existed; it applies to newly written parts (old parts age out via the
    // retention worker, so we deliberately skip an expensive MATERIALIZE).
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD INDEX IF NOT EXISTS idx_ts_ms ts_ms TYPE minmax GRANULARITY 4"
        ))
        .execute()
        .await?;
    // Idempotent add for databases created before the cache column existed.
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS cache_status LowCardinality(String) DEFAULT 'off'"
        ))
        .execute()
        .await?;
    // Idempotent add for databases created before per-request cost was frozen.
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS cost_usd Float64 DEFAULT 0"
        ))
        .execute()
        .await?;
    // Idempotent adds for databases created before per-request energy was frozen.
    for col in [
        "energy_wh Float64 DEFAULT 0",
        "energy_cost_usd Float64 DEFAULT 0",
        "co2_g Float64 DEFAULT 0",
    ] {
        client
            .query(&format!(
                "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS {col}"
            ))
            .execute()
            .await?;
    }
    // Idempotent adds for databases created before the per-request log surfaced
    // session grouping and request class.
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS session_id String DEFAULT ''"
        ))
        .execute()
        .await?;
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS session_id_source LowCardinality(String) DEFAULT ''"
        ))
        .execute()
        .await?;
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS request_type LowCardinality(String) DEFAULT ''"
        ))
        .execute()
        .await?;
    // Idempotent add for databases created before per-device attribution.
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS device_id String DEFAULT ''"
        ))
        .execute()
        .await?;
    // Idempotent add for databases created before per-end-user attribution.
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS end_user String DEFAULT ''"
        ))
        .execute()
        .await?;
    // Idempotent add for databases created before upstream prefix-cache hits
    // were recorded.
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS cached_input_tokens UInt32 DEFAULT 0"
        ))
        .execute()
        .await?;
    // Idempotent adds for databases created before helper-call rows named the
    // request they served, and before requests recorded the variant they used.
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS parent_request_id UUID DEFAULT toUUID('00000000-0000-0000-0000-000000000000')"
        ))
        .execute()
        .await?;
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS model_variant String DEFAULT ''"
        ))
        .execute()
        .await?;
    for column in ["reasoning_effort", "reasoning_effort_requested"] {
        client
            .query(&format!(
                "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS {column} LowCardinality(String) DEFAULT ''"
            ))
            .execute()
            .await?;
    }
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS model_family String DEFAULT ''"
        ))
        .execute()
        .await?;
    for column in ["key_name", "tenant_name"] {
        client
            .query(&format!(
                "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS {column} String DEFAULT ''"
            ))
            .execute()
            .await?;
    }
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS streamed Bool DEFAULT false"
        ))
        .execute()
        .await?;
    // Why a request failed (see `obleth_config::failure`); empty on older rows,
    // which readers file by status code instead.
    for column in [
        "error_class LowCardinality(String) DEFAULT ''",
        "error_reason String DEFAULT ''",
    ] {
        client
            .query(&format!(
                "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS {column}"
            ))
            .execute()
            .await?;
    }
    // Where each request came from. The address, its network and the
    // User-Agent identify a person more than the rest of the row does, so
    // they expire after the client address retention (column TTL: cleared
    // to '', the row stays); the country is kept as long as the row.
    for column in [
        format!("client_ip String DEFAULT '' {ip_ttl}"),
        format!("client_network String DEFAULT '' {ip_ttl}"),
        "client_country LowCardinality(String) DEFAULT ''".to_string(),
        format!("user_agent String DEFAULT '' {ip_ttl}"),
    ] {
        client
            .query(&format!(
                "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS {column}"
            ))
            .execute()
            .await?;
    }
    // Required for `insert_deduplication_token` on a non-replicated table: an
    // insert that timed out after committing is replayed from the WAL with the
    // same token, and ClickHouse discards the duplicate.
    client
        .query(&format!(
            "ALTER TABLE {database}.usage MODIFY SETTING non_replicated_deduplication_window = 10000"
        ))
        .execute()
        .await?;
    // Small key/value ledger for one-time schema actions shared by replicas.
    client
        .query(&format!(
            "CREATE TABLE IF NOT EXISTS {database}.usage_meta (
                key String,
                value String,
                updated_at DateTime64(3) DEFAULT now64(3)
            ) ENGINE = ReplacingMergeTree(updated_at)
            ORDER BY (key, value)"
        ))
        .execute()
        .await?;
    ensure_daily_rollup(client, database).await?;
    ensure_key_watch(client, database, ip_days).await?;
    client
        .query(&format!(
            "CREATE TABLE IF NOT EXISTS {database}.spans (
                request_id        UUID,
                span_name         LowCardinality(String),
                parent_span       String DEFAULT '',
                start_ms          Int64,
                duration_ms       UInt32,
                status            LowCardinality(String) DEFAULT 'ok',
                attributes        String DEFAULT '',
                session_id        String DEFAULT '',
                session_id_source LowCardinality(String) DEFAULT ''
            ) ENGINE = MergeTree()
            PARTITION BY toYYYYMMDD(fromUnixTimestamp64Milli(start_ms))
            ORDER BY (request_id, start_ms)
            TTL toDate(fromUnixTimestamp64Milli(start_ms)) + INTERVAL 14 DAY DELETE"
        ))
        .execute()
        .await?;
    // Idempotent adds for spans tables created before conversation id was tracked.
    client
        .query(&format!(
            "ALTER TABLE {database}.spans ADD COLUMN IF NOT EXISTS session_id String DEFAULT ''"
        ))
        .execute()
        .await?;
    client
        .query(&format!(
            "ALTER TABLE {database}.spans ADD COLUMN IF NOT EXISTS session_id_source LowCardinality(String) DEFAULT ''"
        ))
        .execute()
        .await?;
    Ok(())
}

/// Permanent daily rollup of the per-request `usage` ledger.
///
/// `usage` rows are pruned on a retention window (default 180 days) to bound
/// storage, but the aggregates here are kept forever: one row per
/// `day x tenant x key x model`. A ClickHouse materialized view keeps the
/// rollup current as new requests land, and a one-time guarded backfill seeds
/// it from any history that predates the view. `SummingMergeTree` collapses
/// rows sharing the sort key on merge, so summed columns stay correct.
async fn ensure_daily_rollup(client: &Client, database: &str) -> Result<(), TelemetryError> {
    let table_ddl = format!(
        "CREATE TABLE IF NOT EXISTS {database}.usage_daily (
            day Date,
            tenant_id UUID,
            key_id UUID,
            model String,
            requests UInt64,
            success_requests UInt64,
            error_requests UInt64,
            input_tokens UInt64,
            output_tokens UInt64,
            estimated_tokens UInt64,
            cache_hits UInt64,
            cache_misses UInt64,
            ttft_ms_sum UInt64,
            total_ms_sum UInt64,
            cost_usd_sum Float64,
            energy_wh_sum Float64,
            energy_cost_usd_sum Float64,
            co2_g_sum Float64,
            platform_error_requests UInt64
        ) ENGINE = SummingMergeTree()
        PARTITION BY toYYYYMM(day)
        ORDER BY (day, tenant_id, key_id, model)"
    );
    client.query(&table_ddl).execute().await?;
    // Idempotent add for rollups created before per-request cost was frozen.
    client
        .query(&format!(
            "ALTER TABLE {database}.usage_daily ADD COLUMN IF NOT EXISTS cost_usd_sum Float64 DEFAULT 0"
        ))
        .execute()
        .await?;
    // Idempotent adds for rollups created before per-request energy was frozen.
    for col in [
        "energy_wh_sum Float64 DEFAULT 0",
        "energy_cost_usd_sum Float64 DEFAULT 0",
        "co2_g_sum Float64 DEFAULT 0",
        // Added after the others, so it stays last, in step with the
        // positional backfill below.
        "platform_error_requests UInt64 DEFAULT 0",
    ] {
        client
            .query(&format!(
                "ALTER TABLE {database}.usage_daily ADD COLUMN IF NOT EXISTS {col}"
            ))
            .execute()
            .await?;
    }
    // Dependent-view deduplication (see `Flusher::insert`) needs the window on
    // the view's target too. View blocks are identified by their source block,
    // not their content, so identical aggregates from different batches stay.
    client
        .query(&format!(
            "ALTER TABLE {database}.usage_daily MODIFY SETTING non_replicated_deduplication_window = 10000"
        ))
        .execute()
        .await?;

    // The aggregation projection shared by the materialized view and the
    // backfill, so both compute identical columns from the raw ledger.
    // Latency sums (`ttft_ms_sum`/`total_ms_sum`) are accumulated over
    // successful (2xx/3xx) requests only, so timeouts and upstream errors don't
    // distort the average TTFT / total-time reported per day. The read side
    // divides these by `success_requests` to match. `platform_error_requests`
    // counts the failures that were the platform's, not the caller's (see
    // `obleth_config::failure`).
    let rollup_select = format!(
        "
        toDate(ts) AS day,
        tenant_id,
        key_id,
        model,
        count() AS requests,
        countIf(status_code >= 200 AND status_code < 400) AS success_requests,
        countIf(status_code >= 400) AS error_requests,
        sum(input_tokens) AS input_tokens,
        sum(output_tokens) AS output_tokens,
        sum(estimated_tokens) AS estimated_tokens,
        countIf(cache_status = 'hit') AS cache_hits,
        countIf(cache_status = 'miss') AS cache_misses,
        sumIf(ttft_ms, status_code >= 200 AND status_code < 400) AS ttft_ms_sum,
        sumIf(total_ms, status_code >= 200 AND status_code < 400) AS total_ms_sum,
        sum(cost_usd) AS cost_usd_sum,
        sum(energy_wh) AS energy_wh_sum,
        sum(energy_cost_usd) AS energy_cost_usd_sum,
        sum(co2_g) AS co2_g_sum,
        countIf({class} IN {platform}) AS platform_error_requests",
        class = obleth_config::EFFECTIVE_CLASS_SQL,
        platform = obleth_config::PLATFORM_CLASSES_SQL,
    );

    // Benchmark traffic (synthetic tenants) never enters the permanent rollup:
    // usage_daily has no request_type dimension and an immutable sort key, so
    // it cannot be filtered at read time. Health probes DO roll up — their
    // tokens are deliberately accounted under the nil tenant (model_health.rs).
    let bench = obleth_config::BENCHMARK_REQUEST_TYPE;

    // One-time backfill BEFORE the view exists, and only when the rollup is
    // empty, so restarts never double-count (SummingMergeTree would otherwise
    // re-add existing history) and the view below cannot also capture the same
    // historical rows. A failed count is an error, never "empty".
    if meta_count(client, database, BACKFILL_MARKER).await? == 0 {
        let rolled_up = client
            .query(&format!("SELECT count() FROM {database}.usage_daily"))
            .fetch_one::<u64>()
            .await?;
        let history = client
            .query(&format!(
                "SELECT count() FROM {database}.usage WHERE request_type != ?"
            ))
            .bind(bench)
            .fetch_one::<u64>()
            .await?;
        if rolled_up > 0 || history == 0 {
            meta_put(client, database, BACKFILL_MARKER, "1").await?;
        } else if won_backfill_claim(client, database).await? {
            let backfill = format!(
                "INSERT INTO {database}.usage_daily
                 SELECT {rollup_select}
                 FROM {database}.usage
                 WHERE request_type != '{bench}'
                 GROUP BY day, tenant_id, key_id, model"
            );
            // Content-hash dedup must not drop backfill blocks: the marker and
            // claim above are what prevent a double backfill.
            match client
                .query(&backfill)
                .with_option("insert_deduplicate", "0")
                .execute()
                .await
            {
                Ok(()) => {
                    meta_put(client, database, BACKFILL_MARKER, "1").await?;
                    // The backfill already counted them.
                    meta_put(client, database, PLATFORM_ERRORS_MARKER, "1").await?;
                }
                Err(e) => tracing::warn!(
                    error = %e,
                    "usage_daily backfill failed; rollup will fill going forward"
                ),
            }
        }
    }

    let mv_select = format!(
        "SELECT {rollup_select}
         FROM {database}.usage
         WHERE request_type != '{bench}'
         GROUP BY day, tenant_id, key_id, model"
    );
    // Recreate only when the stored definition differs (e.g. the success-only
    // latency sums). Every drop opens a window in which rows written by other
    // replicas never reach `usage_daily`; target rows themselves survive it.
    let unchanged = match view_matches(client, database, &mv_select).await {
        Ok(unchanged) => unchanged,
        Err(e) => {
            tracing::warn!(error = %e, "could not compare usage_daily_mv definition; recreating it");
            false
        }
    };
    if !unchanged {
        client
            .query(&format!("DROP VIEW IF EXISTS {database}.usage_daily_mv"))
            .execute()
            .await?;
        client
            .query(&format!(
                "CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.usage_daily_mv
                 TO {database}.usage_daily AS {mv_select}"
            ))
            .execute()
            .await?;
    }
    count_earlier_platform_errors(client, database).await?;
    Ok(())
}

/// Give rollup rows written before `platform_error_requests` existed their
/// count, once per deployment.
///
/// The view counts every row inserted since it was created with the column,
/// so the history is the raw ledger before that moment. `SummingMergeTree`
/// adds rows that share a key, so the count goes in as rows of its own with
/// every other column 0. Rows already pruned from the raw ledger stay at 0.
/// A request finished just before the view was created but flushed just
/// after is counted twice; a flush lasts seconds, and that is accepted.
async fn count_earlier_platform_errors(
    client: &Client,
    database: &str,
) -> Result<(), TelemetryError> {
    if meta_count(client, database, PLATFORM_ERRORS_MARKER).await? > 0 {
        return Ok(());
    }
    if !won_claim(client, database, PLATFORM_ERRORS_CLAIM).await? {
        return Ok(());
    }
    let view_created_ms = client
        .query(
            "SELECT toUnixTimestamp(metadata_modification_time) * 1000 FROM system.tables
             WHERE database = ? AND name = 'usage_daily_mv'",
        )
        .bind(database)
        .fetch_one::<i64>()
        .await?;
    let insert = format!(
        "INSERT INTO {database}.usage_daily (day, tenant_id, key_id, model, platform_error_requests)
         SELECT toDate(ts) AS day, tenant_id, key_id, model,
                countIf({class} IN {platform}) AS platform_error_requests
         FROM {database}.usage
         WHERE request_type != '{bench}' AND ts_ms < {view_created_ms}
         GROUP BY day, tenant_id, key_id, model
         HAVING platform_error_requests > 0",
        class = obleth_config::EFFECTIVE_CLASS_SQL,
        platform = obleth_config::PLATFORM_CLASSES_SQL,
        bench = obleth_config::BENCHMARK_REQUEST_TYPE,
    );
    match client
        .query(&insert)
        .with_option("insert_deduplicate", "0")
        .execute()
        .await
    {
        Ok(()) => meta_put(client, database, PLATFORM_ERRORS_MARKER, "1").await?,
        Err(e) => tracing::warn!(
            error = %e,
            "usage_daily platform failure count for earlier days failed; it counts going forward"
        ),
    }
    Ok(())
}

/// True when `usage_daily_mv` exists with exactly `select` as its query.
/// ClickHouse stores views re-formatted (`create_table_query` gains a column
/// list and parenthesised predicates), so the intended SELECT is formatted by
/// the server itself and compared with the stored `as_select`.
async fn view_matches(
    client: &Client,
    database: &str,
    select: &str,
) -> Result<bool, TelemetryError> {
    let matches = client
        .query(
            "SELECT formatQuerySingleLine(?) = as_select FROM system.tables
             WHERE database = ? AND name = 'usage_daily_mv'",
        )
        .bind(select)
        .bind(database)
        .fetch_optional::<u8>()
        .await?;
    Ok(matches == Some(1))
}

/// Tables that let an account system watch where its keys are used from.
///
/// - `key_rejections`: refused uses of real keys (disabled, or outside the
///   key's allowed networks), one row per key, address and reason per
///   [`REJECTION_WINDOW`], with a count.
/// - `key_country_seen`: the first and last time each key was seen from each
///   country, kept forever (the raw ledger is pruned, this is not), fed by a
///   materialized view on `usage`. Rows written before countries were
///   recorded have none, so it starts empty.
///
/// Then, when the client address retention changed since the column TTLs
/// were last set, moves every client-identifying column to the new TTL once.
async fn ensure_key_watch(
    client: &Client,
    database: &str,
    ip_days: u32,
) -> Result<(), TelemetryError> {
    let ip_ttl = client_ip_ttl(ip_days);
    client
        .query(&format!(
            "CREATE TABLE IF NOT EXISTS {database}.key_rejections (
                ts_ms Int64,
                key_id UUID,
                key_name String DEFAULT '',
                tenant_id UUID,
                tenant_name String DEFAULT '',
                reason LowCardinality(String),
                client_ip String DEFAULT '' {ip_ttl},
                client_country LowCardinality(String) DEFAULT '',
                user_agent String DEFAULT '' {ip_ttl},
                path String DEFAULT '',
                count UInt64 DEFAULT 1,
                ts DateTime64(3) MATERIALIZED fromUnixTimestamp64Milli(ts_ms)
            ) ENGINE = MergeTree()
            PARTITION BY toYYYYMM(ts)
            ORDER BY (key_id, ts_ms)"
        ))
        .execute()
        .await?;
    client
        .query(&format!(
            "CREATE TABLE IF NOT EXISTS {database}.key_country_seen (
                key_id UUID,
                client_country LowCardinality(String),
                first_seen_ms SimpleAggregateFunction(min, Int64),
                last_seen_ms SimpleAggregateFunction(max, Int64)
            ) ENGINE = AggregatingMergeTree()
            ORDER BY (key_id, client_country)"
        ))
        .execute()
        .await?;
    client
        .query(&format!(
            "CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.key_country_seen_mv
            TO {database}.key_country_seen AS
            SELECT key_id, client_country,
                   min(ts_ms) AS first_seen_ms, max(ts_ms) AS last_seen_ms
            FROM {database}.usage
            WHERE client_country != ''
            GROUP BY key_id, client_country"
        ))
        .execute()
        .await?;

    // The TTL a column was created with is the retention in force when it
    // was added, recorded here the first time. A later change is applied with
    // one MODIFY per column; ClickHouse then re-evaluates the TTL on existing
    // parts in the background.
    let applied = meta_latest(client, database, CLIENT_IP_TTL_META).await?;
    let wanted = ip_days.to_string();
    match applied {
        Some(days) if days == wanted => {}
        None => meta_put(client, database, CLIENT_IP_TTL_META, &wanted).await?,
        Some(previous) => {
            for (table, column) in [
                ("usage", "client_ip"),
                ("usage", "client_network"),
                ("usage", "user_agent"),
                ("key_rejections", "client_ip"),
                ("key_rejections", "user_agent"),
            ] {
                client
                    .query(&format!(
                        "ALTER TABLE {database}.{table} MODIFY COLUMN {column} String DEFAULT '' {ip_ttl}"
                    ))
                    .execute()
                    .await?;
            }
            meta_put(client, database, CLIENT_IP_TTL_META, &wanted).await?;
            tracing::info!(
                from_days = %previous,
                to_days = ip_days,
                "client address retention changed; column TTLs updated"
            );
        }
    }
    Ok(())
}

/// The most recently written value under `key` in `usage_meta`, if any.
async fn meta_latest(
    client: &Client,
    database: &str,
    key: &str,
) -> Result<Option<String>, TelemetryError> {
    let rows = client
        .query(&format!(
            "SELECT value FROM {database}.usage_meta WHERE key = ? \
             ORDER BY updated_at DESC LIMIT 1"
        ))
        .bind(key)
        .fetch_all::<String>()
        .await?;
    Ok(rows.into_iter().next())
}

async fn meta_count(client: &Client, database: &str, key: &str) -> Result<u64, TelemetryError> {
    Ok(client
        .query(&format!(
            "SELECT count() FROM {database}.usage_meta WHERE key = ?"
        ))
        .bind(key)
        .fetch_one::<u64>()
        .await?)
}

async fn meta_put(
    client: &Client,
    database: &str,
    key: &str,
    value: &str,
) -> Result<(), TelemetryError> {
    client
        .query(&format!(
            "INSERT INTO {database}.usage_meta (key, value) VALUES (?, ?)"
        ))
        .bind(key)
        .bind(value)
        .execute()
        .await?;
    Ok(())
}

/// ClickHouse has no compare-and-set, so replicas booting together elect the
/// backfiller: each records a random claim, waits for concurrent claims to
/// land, and only the smallest recent claim proceeds. Claims older than a few
/// minutes belong to a replica that died before finishing and are ignored.
///
/// Trade-offs, accepted because the backfill runs once per deployment:
/// - A claim from a replica that crashed less than 5 minutes ago can still
///   win, so nobody backfills on this boot. The rollup then fills going
///   forward only, which is the same loss as the old "backfill failed" path.
/// - A replica whose claim becomes visible only after the settle window (a
///   very slow insert) can also see itself as the smallest claim, so both
///   backfill and history is counted twice. The window makes this unlikely,
///   not impossible.
async fn won_backfill_claim(client: &Client, database: &str) -> Result<bool, TelemetryError> {
    won_claim(client, database, BACKFILL_CLAIM).await
}

/// The election [`won_backfill_claim`] describes, under any claim key.
async fn won_claim(client: &Client, database: &str, key: &str) -> Result<bool, TelemetryError> {
    let claim = Uuid::new_v4().to_string();
    meta_put(client, database, key, &claim).await?;
    tokio::time::sleep(BACKFILL_CLAIM_SETTLE).await;
    let winner = client
        .query(&format!(
            "SELECT min(value) FROM {database}.usage_meta
             WHERE key = ? AND updated_at > now64(3) - INTERVAL 5 MINUTE"
        ))
        .bind(key)
        .fetch_one::<String>()
        .await?;
    Ok(winner == claim)
}

#[cfg(test)]
mod conv_tests {
    use super::*;

    fn record() -> UsageRecord {
        UsageRecord {
            request_id: uuid::Uuid::new_v4(),
            tenant_id: uuid::Uuid::nil(),
            key_id: uuid::Uuid::nil(),
            model: "m".into(),
            admission: "ok".into(),
            weight: 1,
            input_tokens: 1,
            output_tokens: 1,
            cached_input_tokens: 0,
            estimated_tokens: 2,
            queue_wait_ms: 0,
            ttft_ms: 0,
            total_ms: 1,
            status_code: 200,
            cache_status: "off".into(),
            cost_usd: 0.0,
            energy_wh: 0.0,
            energy_cost_usd: 0.0,
            co2_g: 0.0,
            ts_ms: 0,
            session_id: String::new(),
            session_id_source: String::new(),
            request_type: "chat".into(),
            device_id: String::new(),
            end_user: String::new(),
            parent_request_id: Uuid::nil(),
            model_variant: String::new(),
            reasoning_effort: String::new(),
            reasoning_effort_requested: String::new(),
            model_family: String::new(),
            key_name: String::new(),
            tenant_name: String::new(),
            streamed: false,
            error_class: String::new(),
            error_reason: String::new(),
            client_ip: String::new(),
            client_network: String::new(),
            client_country: String::new(),
            user_agent: String::new(),
        }
    }

    #[tokio::test]
    async fn starts_without_clickhouse_and_spills_to_the_wal() {
        let directory =
            std::env::temp_dir().join(format!("obleth-sink-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let wal_path = directory.join("usage.jsonl");
        // Port 1 on loopback refuses connections.
        let sink = TelemetrySink::start(
            "http://127.0.0.1:1",
            "obleth",
            "default",
            "",
            wal_path.to_str().unwrap(),
            Duration::from_millis(100),
            true,
        )
        .await
        .expect("sink must start while ClickHouse is unreachable");
        sink.record(record());
        let stats = sink.stats();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        while stats.waled.load(Ordering::Relaxed) == 0 && tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(stats.waled.load(Ordering::Relaxed), 1);
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 0);
        let _ = tokio::fs::remove_dir_all(&directory).await;
    }

    #[tokio::test]
    async fn shutdown_spills_queued_records_when_clickhouse_is_down() {
        let directory =
            std::env::temp_dir().join(format!("obleth-sink-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let wal_path = directory.join("usage.jsonl");
        let sink = TelemetrySink::start(
            "http://127.0.0.1:1",
            "obleth",
            "default",
            "",
            wal_path.to_str().unwrap(),
            Duration::from_millis(100),
            true,
        )
        .await
        .unwrap();
        for _ in 0..25 {
            sink.record(record());
        }
        sink.shutdown().await;
        // Settled by the time shutdown returns, with no waiting on ticks.
        let stats = sink.stats();
        assert_eq!(stats.waled.load(Ordering::Relaxed), 25);
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 0);
        // Recording after shutdown neither panics nor blocks.
        sink.record(record());
        let _ = tokio::fs::remove_dir_all(&directory).await;
    }

    /// Rows are batched for the flush interval even though WAL replay ticks
    /// every second; a one-row insert per tick is what floods ClickHouse with
    /// parts to merge.
    #[tokio::test]
    async fn rows_wait_for_the_flush_interval() {
        let directory =
            std::env::temp_dir().join(format!("obleth-sink-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let wal_path = directory.join("usage.jsonl");
        let sink = TelemetrySink::start(
            "http://127.0.0.1:1",
            "obleth",
            "default",
            "",
            wal_path.to_str().unwrap(),
            Duration::from_secs(60),
            true,
        )
        .await
        .unwrap();
        for _ in 0..3 {
            sink.record(record());
        }
        tokio::time::sleep(Duration::from_millis(1500)).await;
        let stats = sink.stats();
        assert_eq!(stats.waled.load(Ordering::Relaxed), 0);
        sink.shutdown().await;
        assert_eq!(stats.waled.load(Ordering::Relaxed), 3);
        let _ = tokio::fs::remove_dir_all(&directory).await;
    }

    #[tokio::test]
    async fn spills_even_when_fail_open_is_false() {
        let directory =
            std::env::temp_dir().join(format!("obleth-sink-test-{}", uuid::Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let wal_path = directory.join("usage.jsonl");
        let sink = TelemetrySink::start(
            "http://127.0.0.1:1",
            "obleth",
            "default",
            "",
            wal_path.to_str().unwrap(),
            Duration::from_millis(100),
            false,
        )
        .await
        .unwrap();
        for _ in 0..7 {
            sink.record(record());
        }
        sink.shutdown().await;
        let stats = sink.stats();
        assert_eq!(stats.waled.load(Ordering::Relaxed), 7);
        assert_eq!(stats.dropped.load(Ordering::Relaxed), 0);
        let _ = tokio::fs::remove_dir_all(&directory).await;
    }

    #[tokio::test]
    async fn invalid_database_name_still_fails_start() {
        let result = TelemetrySink::start(
            "http://127.0.0.1:1",
            "bad-name;",
            "default",
            "",
            "unused",
            Duration::from_millis(100),
            true,
        )
        .await;
        assert!(matches!(result, Err(TelemetryError::InvalidDatabase(_))));
    }

    #[tokio::test]
    async fn schema_ready_reports_spill_mode_at_boot_when_unreachable() {
        let sink = TelemetrySink::start(
            "http://127.0.0.1:1",
            "obleth",
            "default",
            "",
            "unused",
            Duration::from_millis(100),
            true,
        )
        .await
        .unwrap();
        assert!(!sink.schema_ready());
    }

    /// A schema setup that keeps failing must count every attempt (drives the
    /// error-level escalation at `SCHEMA_FAILURE_ALERT_THRESHOLD`) rather than
    /// silently retrying at `debug` forever. The backoff is reset before each
    /// call so the test doesn't need to wait it out.
    #[tokio::test]
    async fn schema_setup_failures_are_counted() {
        let client = Client::default().with_url("http://127.0.0.1:1");
        let mut flusher = Flusher {
            client: client.clone(),
            schema_client: client,
            database: "obleth".to_string(),
            schema_ready: Arc::new(AtomicBool::new(false)),
            wal: wal::Wal::new("unused-test-wal"),
            wal_path: "unused-test-wal".to_string(),
            flush_interval: Duration::from_millis(100),
            backoff: Backoff::default(),
            replay_backoff: Backoff::default(),
            stats: Arc::default(),
            schema_failures: 0,
            last_schema_warn: None,
        };
        for expected in 1..=3u64 {
            flusher.backoff = Backoff::default();
            assert!(!flusher.clickhouse_ready(true).await);
            assert_eq!(flusher.schema_failures, expected);
        }
    }

    #[test]
    fn replay_backoff_grows_to_one_minute_and_resets() {
        let mut backoff = Backoff::default();
        assert!(backoff.ready());
        for _ in 0..10 {
            backoff.fail();
        }
        assert!(!backoff.ready());
        assert_eq!(backoff.delay, Duration::from_secs(60));
        backoff.reset();
        assert!(backoff.ready());
        assert_eq!(backoff.delay, Duration::from_secs(1));
    }
    #[test]
    fn usage_row_mirrors_session_source() {
        let rec = UsageRecord {
            request_id: uuid::Uuid::nil(),
            tenant_id: uuid::Uuid::nil(),
            key_id: uuid::Uuid::nil(),
            model: "m".into(),
            admission: "ok".into(),
            weight: 1,
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            estimated_tokens: 0,
            queue_wait_ms: 0,
            ttft_ms: 0,
            total_ms: 0,
            status_code: 200,
            cache_status: "off".into(),
            cost_usd: 0.0,
            energy_wh: 0.0,
            energy_cost_usd: 0.0,
            co2_g: 0.0,
            ts_ms: 0,
            session_id: "abc".into(),
            session_id_source: "derived".into(),
            request_type: "chat".into(),
            device_id: "dev-1".into(),
            end_user: String::new(),
            parent_request_id: Uuid::nil(),
            model_variant: String::new(),
            reasoning_effort: String::new(),
            reasoning_effort_requested: String::new(),
            model_family: String::new(),
            key_name: String::new(),
            tenant_name: String::new(),
            streamed: false,
            error_class: String::new(),
            error_reason: String::new(),
            client_ip: String::new(),
            client_network: String::new(),
            client_country: String::new(),
            user_agent: String::new(),
        };
        let row = UsageRow::from(&rec);
        assert_eq!(row.session_id_source, "derived");
        assert_eq!(row.device_id, "dev-1");
    }

    #[test]
    fn usage_row_mirrors_energy_fields() {
        let mut rec = UsageRecord {
            request_id: uuid::Uuid::nil(),
            tenant_id: uuid::Uuid::nil(),
            key_id: uuid::Uuid::nil(),
            model: "m".into(),
            admission: "ok".into(),
            weight: 1,
            input_tokens: 0,
            output_tokens: 0,
            cached_input_tokens: 0,
            estimated_tokens: 0,
            queue_wait_ms: 0,
            ttft_ms: 0,
            total_ms: 0,
            status_code: 200,
            cache_status: "off".into(),
            cost_usd: 0.0,
            energy_wh: 0.0,
            energy_cost_usd: 0.0,
            co2_g: 0.0,
            ts_ms: 0,
            session_id: "abc".into(),
            session_id_source: "derived".into(),
            request_type: "chat".into(),
            device_id: "dev-1".into(),
            end_user: String::new(),
            parent_request_id: Uuid::nil(),
            model_variant: String::new(),
            reasoning_effort: String::new(),
            reasoning_effort_requested: String::new(),
            model_family: String::new(),
            key_name: String::new(),
            tenant_name: String::new(),
            streamed: false,
            error_class: String::new(),
            error_reason: String::new(),
            client_ip: String::new(),
            client_network: String::new(),
            client_country: String::new(),
            user_agent: String::new(),
        };
        rec.energy_wh = 1.5;
        rec.energy_cost_usd = 0.0002;
        rec.co2_g = 0.6;
        let row = UsageRow::from(&rec);
        assert_eq!(row.energy_wh, 1.5);
        assert_eq!(row.energy_cost_usd, 0.0002);
        assert_eq!(row.co2_g, 0.6);
    }

    #[test]
    fn usage_row_mirrors_cached_input_tokens() {
        let mut rec = record();
        rec.input_tokens = 120;
        rec.cached_input_tokens = 96;
        let row = UsageRow::from(&rec);
        assert_eq!(row.input_tokens, 120);
        assert_eq!(row.cached_input_tokens, 96);
    }

    #[test]
    fn usage_row_mirrors_the_parent_request_and_variant() {
        let mut rec = record();
        let row = UsageRow::from(&rec);
        assert!(row.parent_request_id.is_nil());
        assert_eq!(row.model_variant, "");

        let parent = Uuid::new_v4();
        rec.parent_request_id = parent;
        rec.model_variant = "glm-5-3-spec".into();
        let row = UsageRow::from(&rec);
        assert_eq!(row.parent_request_id, parent);
        assert_eq!(row.model_variant, "glm-5-3-spec");
        rec.reasoning_effort = "low".into();
        rec.reasoning_effort_requested = "none".into();
        rec.model_family = "acme".into();
        let row = UsageRow::from(&rec);
        assert_eq!(
            (
                row.reasoning_effort,
                row.reasoning_effort_requested,
                row.model_family
            ),
            ("low", "none", "acme")
        );
    }

    #[test]
    fn usage_row_carries_the_key_and_tenant_names() {
        let mut rec = record();
        rec.key_name = "alice-notebook-1700000000000".into();
        rec.tenant_name = "research".into();
        let row = UsageRow::from(&rec);
        assert_eq!(
            (row.key_name, row.tenant_name),
            ("alice-notebook-1700000000000", "research")
        );
    }

    #[test]
    fn usage_row_carries_the_client() {
        let mut rec = record();
        rec.client_ip = "203.0.113.9".into();
        rec.client_network = "203.0.113.0/24".into();
        rec.client_country = "DE".into();
        rec.user_agent = "curl/8.0".into();
        let row = UsageRow::from(&rec);
        assert_eq!(
            (
                row.client_ip,
                row.client_network,
                row.client_country,
                row.user_agent
            ),
            ("203.0.113.9", "203.0.113.0/24", "DE", "curl/8.0")
        );
        // A WAL record written before the columns existed replays as unknown.
        let mut old = serde_json::to_value(record()).unwrap();
        for field in [
            "client_ip",
            "client_network",
            "client_country",
            "user_agent",
        ] {
            old.as_object_mut().unwrap().remove(field);
        }
        let old: UsageRecord = serde_json::from_value(old).unwrap();
        assert_eq!(old.client_ip, "");
        assert_eq!(old.client_country, "");
    }

    fn rejection(key: u128, ip: &str, reason: KeyRejectionReason, ts_ms: i64) -> KeyRejection {
        KeyRejection {
            ts_ms,
            key_id: Uuid::from_u128(key),
            key_name: "k".into(),
            tenant_id: Uuid::nil(),
            tenant_name: "t".into(),
            reason,
            client_ip: ip.into(),
            client_country: String::new(),
            user_agent: "ua".into(),
            path: "/v1/chat/completions".into(),
            count: 1,
        }
    }

    #[test]
    fn rejections_fold_to_one_row_per_key_address_and_reason() {
        let mut window = RejectionWindow::default();
        for i in 0..1000 {
            window.add(rejection(
                1,
                "203.0.113.9",
                KeyRejectionReason::Disabled,
                100 + i,
            ));
        }
        window.add(rejection(
            1,
            "203.0.113.9",
            KeyRejectionReason::IpNotAllowed,
            50,
        ));
        window.add(rejection(
            1,
            "198.51.100.1",
            KeyRejectionReason::Disabled,
            60,
        ));
        window.add(rejection(
            2,
            "203.0.113.9",
            KeyRejectionReason::Disabled,
            70,
        ));
        let (rows, overflow) = window.take();
        assert_eq!(overflow, 0);
        assert_eq!(rows.len(), 4);
        // Oldest first; a folded row keeps its first refusal's time.
        assert_eq!(
            rows.iter().map(|r| r.ts_ms).collect::<Vec<_>>(),
            [50, 60, 70, 100]
        );
        let hammered = rows.iter().find(|r| r.ts_ms == 100).unwrap();
        assert_eq!(hammered.count, 1000);
        assert!(rows.iter().filter(|r| r.ts_ms != 100).all(|r| r.count == 1));
        // The window starts over empty.
        assert_eq!(window.take().0.len(), 0);
    }

    #[test]
    fn a_full_rejection_window_counts_instead_of_growing() {
        let mut window = RejectionWindow::default();
        for i in 0..(REJECTION_WINDOW_MAX_ROWS as u32 + 5) {
            let ip = std::net::Ipv4Addr::from(i).to_string();
            window.add(rejection(1, &ip, KeyRejectionReason::IpNotAllowed, 1));
        }
        // An address already in the window still folds.
        window.add(rejection(1, "0.0.0.0", KeyRejectionReason::IpNotAllowed, 1));
        let (rows, overflow) = window.take();
        assert_eq!(rows.len(), REJECTION_WINDOW_MAX_ROWS);
        assert_eq!(overflow, 5);
        assert_eq!(
            rows.iter().map(|r| r.count).sum::<u64>(),
            REJECTION_WINDOW_MAX_ROWS as u64 + 1
        );
    }

    #[test]
    fn rejection_rows_name_the_reason() {
        let r = rejection(1, "203.0.113.9", KeyRejectionReason::IpNotAllowed, 1);
        assert_eq!(KeyRejectionRow::from(&r).reason, "ip_not_allowed");
        let r = rejection(1, "203.0.113.9", KeyRejectionReason::Disabled, 1);
        assert_eq!(KeyRejectionRow::from(&r).reason, "disabled");
    }

    #[test]
    fn client_address_retention_defaults_to_90_days_and_is_clamped() {
        assert_eq!(parse_retention_days(None), 90);
        assert_eq!(parse_retention_days(Some("30")), 30);
        assert_eq!(parse_retention_days(Some(" 7 ")), 7);
        assert_eq!(parse_retention_days(Some("0")), 1);
        assert_eq!(parse_retention_days(Some("999999")), 3650);
        assert_eq!(parse_retention_days(Some("soon")), 90);
        assert_eq!(
            client_ip_ttl(90),
            "TTL toDate(fromUnixTimestamp64Milli(ts_ms)) + INTERVAL 90 DAY"
        );
    }

    #[test]
    fn usage_row_says_whether_the_reply_streamed() {
        let mut rec = record();
        assert!(!UsageRow::from(&rec).streamed);
        rec.streamed = true;
        assert!(UsageRow::from(&rec).streamed);
        // A WAL record written before the field existed replays as buffered.
        let mut old = serde_json::to_value(&rec).expect("serializes");
        old.as_object_mut().expect("object").remove("streamed");
        let replayed: UsageRecord = serde_json::from_value(old).expect("replays");
        assert!(!replayed.streamed);
    }
}

#[cfg(test)]
mod accounting_tests {
    use super::*;

    fn with_id(id: u128) -> UsageRecord {
        let mut rec: UsageRecord = serde_json::from_value(serde_json::json!({
            "request_id": uuid::Uuid::nil(), "tenant_id": uuid::Uuid::nil(),
            "key_id": uuid::Uuid::nil(), "model": "m", "admission": "ok", "weight": 1,
            "input_tokens": 1, "output_tokens": 1, "estimated_tokens": 2,
            "queue_wait_ms": 0, "ttft_ms": 0, "total_ms": 1, "status_code": 200,
            "cache_status": "off", "ts_ms": 0
        }))
        .unwrap();
        rec.request_id = uuid::Uuid::from_u128(id);
        rec
    }

    #[test]
    fn dedup_token_is_stable_and_batch_specific() {
        let batch = [with_id(1), with_id(2)];
        assert_eq!(dedup_token(&batch), dedup_token(&batch.clone()));
        // Pinned: a replay after an upgrade must produce the same token.
        assert_eq!(dedup_token(&batch), "usage-2-bdaaca7fe0b2bbcc");
        assert_ne!(dedup_token(&batch), dedup_token(&[with_id(2), with_id(1)]));
        assert_ne!(dedup_token(&batch), dedup_token(&batch[..1]));
    }

    #[test]
    fn usage_row_zeroes_non_finite_figures() {
        let mut rec = with_id(1);
        rec.cost_usd = f64::NAN;
        rec.energy_wh = f64::INFINITY;
        rec.energy_cost_usd = f64::NEG_INFINITY;
        rec.co2_g = 2.5;
        let row = UsageRow::from(&rec);
        assert_eq!(row.cost_usd, 0.0);
        assert_eq!(row.energy_wh, 0.0);
        assert_eq!(row.energy_cost_usd, 0.0);
        assert_eq!(row.co2_g, 2.5);
    }
}

/// Run against a real ClickHouse by setting `OBLETH_TEST_CLICKHOUSE_URL`
/// (e.g. `http://127.0.0.1:18123`); without it these return early. Each test
/// works in its own uniquely named database and drops only that database.
#[cfg(test)]
mod clickhouse_tests {
    use super::*;

    fn fixture() -> Option<(Client, String)> {
        let url = std::env::var("OBLETH_TEST_CLICKHOUSE_URL").ok()?;
        let database = format!("obleth_test_{}", Uuid::new_v4().simple());
        Some((Client::default().with_url(url), database))
    }

    async fn scalar(client: &Client, sql: &str) -> u64 {
        client.query(sql).fetch_one::<u64>().await.unwrap()
    }

    async fn view_mtime(client: &Client, database: &str) -> String {
        client
            .query(
                "SELECT toString(metadata_modification_time) FROM system.tables
                 WHERE database = ? AND name = 'usage_daily_mv'",
            )
            .bind(database)
            .fetch_one::<String>()
            .await
            .unwrap()
    }

    fn record(id: u128) -> UsageRecord {
        let mut rec: UsageRecord = serde_json::from_value(serde_json::json!({
            "request_id": Uuid::nil(), "tenant_id": Uuid::nil(), "key_id": Uuid::nil(),
            "model": "m", "admission": "ok", "weight": 1, "input_tokens": 1,
            "output_tokens": 1, "estimated_tokens": 2, "queue_wait_ms": 0, "ttft_ms": 0,
            "total_ms": 1, "status_code": 200, "cache_status": "off",
            "ts_ms": 1_750_000_000_000i64, "request_type": "chat"
        }))
        .unwrap();
        rec.request_id = Uuid::from_u128(id);
        rec
    }

    fn flusher(client: &Client, database: &str) -> Flusher {
        Flusher {
            client: client.clone().with_database(database),
            schema_client: client.clone(),
            database: database.to_string(),
            schema_ready: Arc::new(AtomicBool::new(true)),
            wal: wal::Wal::new("unused-test-wal"),
            wal_path: "unused-test-wal".to_string(),
            flush_interval: Duration::from_millis(100),
            backoff: Backoff::default(),
            replay_backoff: Backoff::default(),
            stats: Arc::default(),
            schema_failures: 0,
            last_schema_warn: None,
        }
    }

    async fn drop_database(client: &Client, database: &str) {
        client
            .query(&format!("DROP DATABASE IF EXISTS {database}"))
            .execute()
            .await
            .unwrap();
    }

    /// Client columns, their TTLs, the rejections ledger and the per-country
    /// first-seen table come up on a fresh database and on a second boot,
    /// and a changed retention moves the TTLs once.
    #[tokio::test]
    async fn client_columns_and_key_watch_tables_are_applied_idempotently() {
        let Some((client, database)) = fixture() else {
            return;
        };
        ensure_schema(&client, &database).await.unwrap();
        ensure_schema(&client, &database).await.unwrap();
        let flusher = flusher(&client, &database);
        let mut rec = record(1);
        rec.key_id = Uuid::from_u128(7);
        rec.client_ip = "203.0.113.9".into();
        rec.client_network = "203.0.113.0/24".into();
        rec.client_country = "DE".into();
        rec.user_agent = "curl/8.0".into();
        flusher.insert(&[rec]).await.unwrap();
        let ip: String = client
            .query(&format!("SELECT client_ip FROM {database}.usage"))
            .fetch_one()
            .await
            .unwrap();
        assert_eq!(ip, "203.0.113.9");
        let seen =
            format!("SELECT count() FROM {database}.key_country_seen WHERE client_country = 'DE'");
        assert_eq!(scalar(&client, &seen).await, 1);
        let create: String = client
            .query("SELECT create_table_query FROM system.tables WHERE database = ? AND name = 'usage'")
            .bind(&database)
            .fetch_one()
            .await
            .unwrap();
        assert!(create.contains("INTERVAL 90 DAY"), "{create}");

        let rejections = RejectionFlusher {
            client: client.clone().with_database(&database),
            schema_ready: Arc::new(AtomicBool::new(true)),
            window: std::sync::Mutex::new(RejectionWindow::default()),
            stats: Arc::default(),
        };
        let mut r = KeyRejection {
            ts_ms: 1_750_000_000_000,
            key_id: Uuid::from_u128(7),
            key_name: "k".into(),
            tenant_id: Uuid::nil(),
            tenant_name: "t".into(),
            reason: KeyRejectionReason::IpNotAllowed,
            client_ip: "198.51.100.1".into(),
            client_country: "US".into(),
            user_agent: "ua".into(),
            path: "/v1/models".into(),
            count: 1,
        };
        rejections.add(r.clone());
        r.ts_ms += 5;
        rejections.add(r);
        rejections.flush().await;
        let rows = format!("SELECT count() FROM {database}.key_rejections");
        let total = format!("SELECT sum(count) FROM {database}.key_rejections");
        assert_eq!(scalar(&client, &rows).await, 1);
        assert_eq!(scalar(&client, &total).await, 2);

        // A changed retention is applied to the columns once.
        meta_put(&client, &database, CLIENT_IP_TTL_META, "30")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        ensure_key_watch(&client, &database, 90).await.unwrap();
        assert_eq!(
            meta_latest(&client, &database, CLIENT_IP_TTL_META)
                .await
                .unwrap()
                .as_deref(),
            Some("90")
        );
        drop_database(&client, &database).await;
    }

    #[tokio::test]
    async fn second_boot_leaves_rollup_view_untouched() {
        let Some((client, database)) = fixture() else {
            return;
        };
        ensure_schema(&client, &database).await.unwrap();
        let first = view_mtime(&client, &database).await;
        // metadata_modification_time has one-second resolution.
        tokio::time::sleep(Duration::from_millis(1500)).await;
        ensure_schema(&client, &database).await.unwrap();
        assert_eq!(view_mtime(&client, &database).await, first);
        // A changed definition is still applied.
        client
            .query(&format!("DROP VIEW {database}.usage_daily_mv"))
            .execute()
            .await
            .unwrap();
        client
            .query(&format!(
                "CREATE MATERIALIZED VIEW {database}.usage_daily_mv TO {database}.usage_daily AS
                 SELECT toDate(ts) AS day, tenant_id, key_id, model, count() AS requests
                 FROM {database}.usage GROUP BY day, tenant_id, key_id, model"
            ))
            .execute()
            .await
            .unwrap();
        ensure_schema(&client, &database).await.unwrap();
        let select: String = client
            .query(
                "SELECT as_select FROM system.tables
                 WHERE database = ? AND name = 'usage_daily_mv'",
            )
            .bind(&database)
            .fetch_one()
            .await
            .unwrap();
        assert!(select.contains("success_requests"), "{select}");
        drop_database(&client, &database).await;
    }

    #[tokio::test]
    async fn concurrent_boots_backfill_history_once() {
        let Some((client, database)) = fixture() else {
            return;
        };
        ensure_schema(&client, &database).await.unwrap();
        let flusher = flusher(&client, &database);
        flusher
            .insert(&(1..=10).map(record).collect::<Vec<_>>())
            .await
            .unwrap();
        // Recreate the pre-rollup state: history in `usage`, nothing rolled up.
        for sql in [
            format!("DROP VIEW {database}.usage_daily_mv"),
            format!("TRUNCATE TABLE {database}.usage_daily"),
            format!("TRUNCATE TABLE {database}.usage_meta"),
        ] {
            client.query(&sql).execute().await.unwrap();
        }
        let (a, b) = tokio::join!(
            ensure_schema(&client, &database),
            ensure_schema(&client, &database)
        );
        a.unwrap();
        b.unwrap();
        let sum = format!("SELECT sum(requests) FROM {database}.usage_daily");
        assert_eq!(scalar(&client, &sum).await, 10);
        // Later boots see the marker and never re-aggregate.
        client
            .query(&format!("DROP VIEW {database}.usage_daily_mv"))
            .execute()
            .await
            .unwrap();
        ensure_schema(&client, &database).await.unwrap();
        assert_eq!(scalar(&client, &sum).await, 10);
        drop_database(&client, &database).await;
    }

    #[tokio::test]
    async fn shutdown_inserts_queued_records() {
        let Some((client, database)) = fixture() else {
            return;
        };
        let url = std::env::var("OBLETH_TEST_CLICKHOUSE_URL").unwrap();
        let directory = std::env::temp_dir().join(format!("obleth-sink-test-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&directory).await.unwrap();
        let wal_path = directory.join("usage.jsonl");
        let sink = TelemetrySink::start(
            &url,
            &database,
            "default",
            "",
            wal_path.to_str().unwrap(),
            Duration::from_millis(100),
            true,
        )
        .await
        .unwrap();
        for id in 1..=30 {
            sink.record(record(id));
        }
        sink.shutdown().await;
        let usage = format!("SELECT count() FROM {database}.usage");
        assert_eq!(scalar(&client, &usage).await, 30);
        assert_eq!(sink.stats().waled.load(Ordering::Relaxed), 0);
        drop_database(&client, &database).await;
        let _ = tokio::fs::remove_dir_all(&directory).await;
    }

    #[tokio::test]
    async fn replayed_batch_is_deduplicated() {
        let Some((client, database)) = fixture() else {
            return;
        };
        ensure_schema(&client, &database).await.unwrap();
        let flusher = flusher(&client, &database);
        let batch: Vec<_> = (1..=5).map(record).collect();
        flusher.insert(&batch).await.unwrap();
        // The WAL replays a timed-out insert that ClickHouse had committed.
        flusher.insert(&batch).await.unwrap();
        let usage = format!("SELECT count() FROM {database}.usage");
        assert_eq!(scalar(&client, &usage).await, 5);
        let daily = format!("SELECT sum(requests) FROM {database}.usage_daily");
        assert_eq!(scalar(&client, &daily).await, 5);
        // A different batch is not mistaken for a duplicate.
        flusher.insert(&[record(6)]).await.unwrap();
        assert_eq!(scalar(&client, &usage).await, 6);
        drop_database(&client, &database).await;
    }
}
