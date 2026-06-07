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
}

/// ClickHouse row mirror of [`UsageRecord`].
#[derive(Debug, Clone, Row, Serialize, Deserialize)]
struct UsageRow {
    #[serde(with = "clickhouse::serde::uuid")]
    request_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    tenant_id: Uuid,
    #[serde(with = "clickhouse::serde::uuid")]
    key_id: Uuid,
    model: String,
    admission: String,
    weight: i64,
    input_tokens: u32,
    output_tokens: u32,
    estimated_tokens: u32,
    queue_wait_ms: u32,
    ttft_ms: u32,
    total_ms: u32,
    status_code: u16,
    cache_status: String,
    ts_ms: i64,
}

impl From<UsageRecord> for UsageRow {
    fn from(r: UsageRecord) -> Self {
        UsageRow {
            request_id: r.request_id,
            tenant_id: r.tenant_id,
            key_id: r.key_id,
            model: r.model,
            admission: r.admission,
            weight: r.weight,
            input_tokens: r.input_tokens,
            output_tokens: r.output_tokens,
            estimated_tokens: r.estimated_tokens,
            queue_wait_ms: r.queue_wait_ms,
            ttft_ms: r.ttft_ms,
            total_ms: r.total_ms,
            status_code: r.status_code,
            cache_status: r.cache_status,
            ts_ms: r.ts_ms,
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
        let stats = Arc::new(TelemetryStats::default());
        let flusher = Flusher {
            client,
            wal_path: wal_path.to_string(),
            fail_open,
            stats: stats.clone(),
        };
        tokio::spawn(flusher.run(rx));
        Ok(TelemetrySink { tx, stats })
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
            let row = UsageRow::from(rec.clone());
            insert.write(&row).await?;
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

async fn ensure_schema(client: &Client, database: &str) -> Result<(), TelemetryError> {
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
            ts_ms Int64,
            ts DateTime64(3) MATERIALIZED fromUnixTimestamp64Milli(ts_ms)
        ) ENGINE = MergeTree()
        PARTITION BY toYYYYMMDD(ts)
        ORDER BY (tenant_id, ts_ms)"
    );
    client.query(&ddl).execute().await?;
    // Idempotent add for databases created before the cache column existed.
    client
        .query(&format!(
            "ALTER TABLE {database}.usage ADD COLUMN IF NOT EXISTS cache_status LowCardinality(String) DEFAULT 'off'"
        ))
        .execute()
        .await?;
    ensure_daily_rollup(client, database).await?;
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
            total_ms_sum UInt64
        ) ENGINE = SummingMergeTree()
        PARTITION BY toYYYYMM(day)
        ORDER BY (day, tenant_id, key_id, model)"
    );
    client.query(&table_ddl).execute().await?;

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
        sumIf(total_ms, status_code >= 200 AND status_code < 400) AS total_ms_sum";

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
