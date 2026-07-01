//! Async usage/telemetry ledger writer (ClickHouse) with a local WAL fallback.
//!
//! The request hot path calls [`TelemetrySink::record`], which is a non-blocking
//! channel send — it never awaits ClickHouse. A background task batches rows and
//! inserts them. If ClickHouse is unavailable and fail-open is set, batches spill
//! to a local write-ahead log and are replayed once it recovers, so the user's
//! request is never blocked by the ledger.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use clickhouse::{Client, Row};
use obleth_config::UsageRecord;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tokio::sync::mpsc;
use uuid::Uuid;

const BATCH_MAX: usize = 500;
const FLUSH_INTERVAL: Duration = Duration::from_millis(1000);

#[derive(Debug, thiserror::Error)]
pub enum TelemetryError {
    #[error("clickhouse: {0}")]
    Click(#[from] clickhouse::error::Error),
    #[error("invalid clickhouse database name: {0:?}")]
    InvalidDatabase(String),
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
    ts_ms: i64,
    session_id: &'a str,
    session_id_source: &'a str,
    request_type: &'a str,
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
            cost_usd: r.cost_usd,
            ts_ms: r.ts_ms,
            session_id: &r.session_id,
            session_id_source: &r.session_id_source,
            request_type: &r.request_type,
        }
    }
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
    stats: Arc<TelemetryStats>,
}

impl TelemetrySink {
    /// Connect, ensure schema exists, and spawn the background flusher.
    pub async fn start(
        url: &str,
        database: &str,
        user: &str,
        password: &str,
        wal_path: &str,
        fail_open: bool,
    ) -> Result<Self, TelemetryError> {
        let mut client = Client::default().with_url(url).with_user(user);
        if !password.is_empty() {
            client = client.with_password(password);
        }
        ensure_schema(&client, database).await?;
        let client = client.with_database(database);

        let (tx, rx) = mpsc::channel(10_000);
        let (tx_spans, rx_spans) = mpsc::channel::<SpanRecord>(10_000);
        let stats = Arc::new(TelemetryStats::default());
        let flusher = Flusher {
            client: client.clone(),
            wal_path: wal_path.to_string(),
            fail_open,
            stats: stats.clone(),
        };
        let spans_flusher = SpansFlusher {
            client,
            stats: stats.clone(),
        };
        tokio::spawn(flusher.run(rx));
        tokio::spawn(spans_flusher.run(rx_spans));
        Ok(TelemetrySink {
            tx,
            tx_spans,
            stats,
        })
    }

    pub fn stats(&self) -> Arc<TelemetryStats> {
        self.stats.clone()
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
}

struct Flusher {
    client: Client,
    wal_path: String,
    fail_open: bool,
    stats: Arc<TelemetryStats>,
}

impl Flusher {
    async fn run(self, mut rx: mpsc::Receiver<UsageRecord>) {
        let mut buf: Vec<UsageRecord> = Vec::with_capacity(BATCH_MAX);
        let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
        loop {
            tokio::select! {
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
                    self.replay_wal().await;
                }
            }
        }
    }

    async fn flush(&self, buf: &mut Vec<UsageRecord>) {
        if buf.is_empty() {
            return;
        }
        let batch = std::mem::take(buf);
        match self.insert(&batch).await {
            Ok(()) => {}
            Err(e) => {
                tracing::warn!(error = %e, count = batch.len(), "clickhouse insert failed");
                if self.fail_open {
                    self.write_wal(&batch).await;
                }
            }
        }
    }

    async fn insert(&self, batch: &[UsageRecord]) -> Result<(), TelemetryError> {
        let mut insert = self.client.insert("usage")?;
        for rec in batch {
            insert.write(&UsageRow::from(rec)).await?;
        }
        insert.end().await?;
        Ok(())
    }

    async fn write_wal(&self, batch: &[UsageRecord]) {
        let mut lines = String::new();
        for rec in batch {
            if let Ok(json) = serde_json::to_string(rec) {
                lines.push_str(&json);
                lines.push('\n');
            }
        }
        let res = async {
            let mut f = tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.wal_path)
                .await?;
            f.write_all(lines.as_bytes()).await?;
            f.flush().await
        }
        .await;
        match res {
            Ok(()) => {
                self.stats
                    .waled
                    .fetch_add(batch.len() as u64, Ordering::Relaxed);
            }
            Err(e) => tracing::error!(error = %e, "failed to write telemetry WAL"),
        }
    }

    /// Best-effort replay of any WAL'd records, then truncate on success.
    async fn replay_wal(&self) {
        let content = match tokio::fs::read_to_string(&self.wal_path).await {
            Ok(c) if !c.trim().is_empty() => c,
            _ => return,
        };
        let batch: Vec<UsageRecord> = content
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect();
        if batch.is_empty() {
            let _ = tokio::fs::remove_file(&self.wal_path).await;
            return;
        }
        if self.insert(&batch).await.is_ok() {
            let _ = tokio::fs::remove_file(&self.wal_path).await;
            tracing::info!(count = batch.len(), "replayed telemetry WAL");
        }
    }
}

struct SpansFlusher {
    client: Client,
    #[allow(dead_code)]
    stats: Arc<TelemetryStats>,
}

impl SpansFlusher {
    async fn run(self, mut rx: mpsc::Receiver<SpanRecord>) {
        let mut buf: Vec<SpanRecord> = Vec::with_capacity(BATCH_MAX);
        let mut ticker = tokio::time::interval(FLUSH_INTERVAL);
        loop {
            tokio::select! {
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
        if let Err(e) = self.insert(&batch).await {
            tracing::warn!(error = %e, count, "spans insert failed");
        } else {
            tracing::debug!(count, "spans flushed to ClickHouse");
        }
    }

    async fn insert(&self, batch: &[SpanRecord]) -> Result<(), clickhouse::error::Error> {
        let mut ins = self.client.insert("spans")?;
        for rec in batch {
            ins.write(&SpanRow::from(rec)).await?;
        }
        ins.end().await?;
        Ok(())
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
            ts_ms Int64,
            session_id String DEFAULT '',
            session_id_source LowCardinality(String) DEFAULT '',
            request_type LowCardinality(String) DEFAULT '',
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
            cost_usd_sum Float64
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
        sum(cost_usd) AS cost_usd_sum";

    // One-time backfill BEFORE the view exists, and only when the rollup is
    // empty, so restarts never double-count (SummingMergeTree would otherwise
    // re-add existing history) and the view below cannot also capture the same
    // historical rows.
    let existing = client
        .query(&format!("SELECT count() FROM {database}.usage_daily"))
        .fetch_one::<u64>()
        .await
        .unwrap_or(0);
    if existing == 0 {
        let backfill = format!(
            "INSERT INTO {database}.usage_daily
             SELECT {rollup_select}
             FROM {database}.usage
             GROUP BY day, tenant_id, key_id, model"
        );
        if let Err(e) = client.query(&backfill).execute().await {
            tracing::warn!(error = %e, "usage_daily backfill failed; rollup will fill going forward");
        }
    }

    let mv_ddl = format!(
        "CREATE MATERIALIZED VIEW IF NOT EXISTS {database}.usage_daily_mv
         TO {database}.usage_daily AS
         SELECT {rollup_select}
         FROM {database}.usage
         GROUP BY day, tenant_id, key_id, model"
    );
    // Drop-and-recreate so latency-sum semantics (success-only) take effect on
    // deployments whose view predates this change. Dropping the view leaves the
    // target `usage_daily` rows untouched — historical aggregates are kept as-is
    // and only new inserts use the updated projection.
    client
        .query(&format!("DROP VIEW IF EXISTS {database}.usage_daily_mv"))
        .execute()
        .await?;
    client.query(&mv_ddl).execute().await?;
    Ok(())
}

#[cfg(test)]
mod conv_tests {
    use super::*;
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
            ts_ms: 0,
            session_id: "abc".into(),
            session_id_source: "derived".into(),
            request_type: "chat".into(),
        };
        let row = UsageRow::from(&rec);
        assert_eq!(row.session_id_source, "derived");
    }
}
