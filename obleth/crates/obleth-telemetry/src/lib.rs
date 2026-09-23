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
use obleth_config::UsageRecord;
use serde::{Deserialize, Serialize};
mod wal;
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

const BATCH_MAX: usize = 500;
const FLUSH_INTERVAL: Duration = Duration::from_millis(1000);
/// Bound on the reachability probe that precedes schema setup: the ClickHouse
/// client has no connect timeout, and a blackholed host would otherwise hang
/// boot (or the flusher) for the OS TCP timeout.
const SCHEMA_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// `usage_meta` key recording that `usage_daily` has been seeded from history.
const BACKFILL_MARKER: &str = "backfilled_v1";
/// `usage_meta` key under which booting replicas elect one backfiller.
const BACKFILL_CLAIM: &str = "backfill_claim_v1";
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
}

impl TelemetrySink {
    /// Connect, ensure schema exists, and spawn the background flusher.
    ///
    /// An unreachable ClickHouse does not fail start: the sink comes up in
    /// spill mode (usage goes to the WAL, spans are dropped) and the flusher
    /// retries schema setup with backoff before its first insert. Only an
    /// invalid database name is an error.
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
            backoff: Backoff::default(),
            replay_backoff: Backoff::default(),
            stats: stats.clone(),
            schema_failures: 0,
            last_schema_warn: None,
        };
        let spans_flusher = SpansFlusher {
            client,
            schema_ready: schema_ready.clone(),
            stats: stats.clone(),
        };
        let (drain, drain_rx) = mpsc::channel(4);
        let (drain_spans, drain_spans_rx) = mpsc::channel(4);
        tokio::spawn(flusher.run(rx, drain_rx));
        tokio::spawn(spans_flusher.run(rx_spans, drain_spans_rx));
        Ok(TelemetrySink {
            tx,
            tx_spans,
            drain,
            drain_spans,
            stats,
            schema_ready,
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
        let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
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

struct SpansFlusher {
    client: Client,
    schema_ready: Arc<AtomicBool>,
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
        let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
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
            co2_g_sum Float64
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
    // divides these by `success_requests` to match.
    let rollup_select = "
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
        sum(co2_g) AS co2_g_sum";

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
                Ok(()) => meta_put(client, database, BACKFILL_MARKER, "1").await?,
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
    let claim = Uuid::new_v4().to_string();
    meta_put(client, database, BACKFILL_CLAIM, &claim).await?;
    tokio::time::sleep(BACKFILL_CLAIM_SETTLE).await;
    let winner = client
        .query(&format!(
            "SELECT min(value) FROM {database}.usage_meta
             WHERE key = ? AND updated_at > now64(3) - INTERVAL 5 MINUTE"
        ))
        .bind(BACKFILL_CLAIM)
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
        };
        rec.energy_wh = 1.5;
        rec.energy_cost_usd = 0.0002;
        rec.co2_g = 0.6;
        let row = UsageRow::from(&rec);
        assert_eq!(row.energy_wh, 1.5);
        assert_eq!(row.energy_cost_usd, 0.0002);
        assert_eq!(row.co2_g, 0.6);
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
