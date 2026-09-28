//! Raw usage-ledger retention.
//!
//! The per-request `usage` table in ClickHouse grows with traffic, so it is
//! pruned to a rolling window (default 180 days, runtime-tunable from the
//! control plane). Pruning drops whole day-partitions — an O(1) metadata op —
//! rather than deleting rows. The permanent `usage_daily` rollup is *never*
//! touched here, so historical totals survive indefinitely.
//!
//! A background worker enforces the window hourly; the same core also backs the
//! manual "Compact now" admin endpoint.

use std::time::Duration;

use chrono::{Duration as ChronoDuration, Utc};

use crate::{usage, AdminState};

/// Floor so a misconfigured/zero value can never wipe the whole ledger.
const MIN_RETENTION_DAYS: i64 = 1;
/// Ceiling (100 years): far beyond any real window, and small enough that
/// `now - days` can never overflow chrono and panic the worker.
pub(crate) const MAX_RETENTION_DAYS: i64 = 36_500;
/// How often the worker re-evaluates the retention window.
const CLEANUP_INTERVAL_SECS: u64 = 3_600;

/// Outcome of a compaction pass.
#[derive(Debug, Clone, Copy)]
pub struct CompactResult {
    /// Effective retention window applied (after clamping).
    pub retention_days: i64,
    /// Number of `usage` day-partitions dropped.
    pub partitions_dropped: usize,
}

/// Effective retention in days: the persisted setting if present, else the
/// environment default carried on [`AdminState`]. Always clamped to
/// `MIN_RETENTION_DAYS..=MAX_RETENTION_DAYS`, so a bad stored value (older
/// writes, a restored backup) cannot wipe the ledger or panic the worker.
async fn effective_retention_days(state: &AdminState) -> i64 {
    let days = match state.store.get_usage_retention_settings().await {
        Ok(Some(settings)) => settings.days,
        Ok(None) => state.usage_retention_default_days,
        Err(error) => {
            tracing::warn!(%error, "failed to read usage retention setting; using default");
            state.usage_retention_default_days
        }
    };
    clamp_retention_days(days)
}

fn clamp_retention_days(days: i64) -> i64 {
    days.clamp(MIN_RETENTION_DAYS, MAX_RETENTION_DAYS)
}

/// Drop every `usage` day-partition older than the retention window. Safe to
/// call repeatedly; only partitions strictly older than the cutoff are removed.
pub async fn compact_usage_now(
    state: &AdminState,
) -> Result<CompactResult, clickhouse::error::Error> {
    let retention_days = effective_retention_days(state).await;
    let cutoff_date = (Utc::now() - ChronoDuration::days(retention_days)).date_naive();
    // Partition ids are `YYYYMMDD` integers; build the same shape to compare.
    let cutoff_yyyymmdd: u32 = cutoff_date
        .format("%Y%m%d")
        .to_string()
        .parse()
        .unwrap_or(0);

    let partitions = usage::usage_partitions_before(&state.clickhouse, cutoff_yyyymmdd).await?;
    let mut dropped = 0usize;
    for partition in &partitions {
        match usage::drop_usage_partition(&state.clickhouse, partition).await {
            Ok(()) => dropped += 1,
            Err(error) => {
                tracing::warn!(%error, partition, "failed to drop usage partition");
            }
        }
    }
    Ok(CompactResult {
        retention_days,
        partitions_dropped: dropped,
    })
}

/// Spawn the hourly retention worker. A no-op clone-and-spawn so it slots in
/// beside the model-health worker at boot.
pub fn spawn_worker(state: AdminState) {
    tokio::spawn(async move {
        // Run once shortly after boot, then on the cleanup cadence.
        loop {
            match compact_usage_now(&state).await {
                Ok(result) if result.partitions_dropped > 0 => {
                    tracing::info!(
                        retention_days = result.retention_days,
                        partitions_dropped = result.partitions_dropped,
                        "pruned old usage partitions"
                    );
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "usage retention compaction failed"),
            }
            tokio::time::sleep(Duration::from_secs(CLEANUP_INTERVAL_SECS)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_is_clamped_to_a_cutoff_chrono_can_compute() {
        assert_eq!(clamp_retention_days(0), MIN_RETENTION_DAYS);
        assert_eq!(clamp_retention_days(-5), MIN_RETENTION_DAYS);
        assert_eq!(clamp_retention_days(180), 180);
        assert_eq!(clamp_retention_days(i64::MAX), MAX_RETENTION_DAYS);
        // The ceiling itself must not panic when turned into a cutoff date.
        let _ = (Utc::now() - ChronoDuration::days(clamp_retention_days(i64::MAX))).date_naive();
    }
}
