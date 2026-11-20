//! Short-lived copies of the dashboard's whole-gateway usage reads. Every
//! open dashboard page polls the footer totals every 30 s, and each Overview
//! refresh scans the same usage window; one replica serves each such read
//! from ClickHouse at most once per [`USAGE_CACHE_TTL`], and callers that
//! arrive while it runs wait for that read instead of starting their own.

use std::collections::HashMap;
use std::future::Future;
use std::hash::Hash;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::usage::{OverviewUsage, UsageTotals};

/// How stale a cached usage read may be.
pub const USAGE_CACHE_TTL: Duration = Duration::from_secs(15);

/// The window bound a cache key holds: millis rounded down to the TTL, so
/// polls a few seconds apart (each asking for "the last 24 hours") share
/// an entry.
pub fn bucketed(ms: i64) -> i64 {
    ms.div_euclid(USAGE_CACHE_TTL.as_millis() as i64)
}

/// A value per key, loaded once per `ttl`. Concurrent callers for one key
/// share a single load; callers for different keys do not wait for each
/// other. A failed load is returned to the caller holding the slot and
/// nothing is cached, so the next caller tries again.
pub struct SingleFlight<K, V> {
    ttl: Duration,
    slots: Mutex<HashMap<K, Slot<V>>>,
}

type Slot<V> = Arc<tokio::sync::Mutex<Option<(Instant, V)>>>;

impl<K: Eq + Hash + Clone, V: Clone> SingleFlight<K, V> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            slots: Mutex::new(HashMap::new()),
        }
    }

    pub async fn get_or_load<E, F, Fut>(&self, key: K, load: F) -> Result<V, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<V, E>>,
    {
        let slot = {
            let mut slots = self.slots.lock().unwrap_or_else(|p| p.into_inner());
            // Forget keys nobody holds whose value has aged out (or was never
            // loaded), so one-off windows do not pile up.
            let ttl = self.ttl;
            slots.retain(|_, s| {
                Arc::strong_count(s) > 1
                    || s.try_lock()
                        .map(|v| v.as_ref().is_some_and(|(at, _)| at.elapsed() < ttl))
                        .unwrap_or(true)
            });
            slots.entry(key).or_default().clone()
        };
        let mut cached = slot.lock().await;
        if let Some((at, value)) = cached.as_ref() {
            if at.elapsed() < self.ttl {
                return Ok(value.clone());
            }
        }
        let value = load().await?;
        *cached = Some((Instant::now(), value.clone()));
        Ok(value)
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.slots.lock().unwrap().len()
    }
}

/// An unfiltered totals read: window bounds via [`bucketed`], and whether
/// internal traffic counts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TotalsKey {
    pub since: i64,
    pub until: Option<i64>,
    pub include_internal: bool,
}

/// An Overview read: window bounds via [`bucketed`], the requested series
/// bucket width, and whether internal traffic counts.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct OverviewKey {
    pub since: i64,
    pub until: Option<i64>,
    pub bucket_ms: Option<i64>,
    pub include_internal: bool,
}

/// The cached usage reads, one per admin process.
pub struct UsageCache {
    pub totals: SingleFlight<TotalsKey, UsageTotals>,
    pub overview: SingleFlight<OverviewKey, Arc<OverviewUsage>>,
}

impl Default for UsageCache {
    fn default() -> Self {
        Self {
            totals: SingleFlight::new(USAGE_CACHE_TTL),
            overview: SingleFlight::new(USAGE_CACHE_TTL),
        }
    }
}

impl TotalsKey {
    /// The key of a totals read, or `None` when it is filtered to a tenant,
    /// key or model (those are not polled, so they are not cached).
    pub fn of(q: &crate::usage::UsageQuery, since: i64) -> Option<Self> {
        let unfiltered = q.tenant_id.is_none() && q.key_id.is_none() && q.model.is_none();
        unfiltered.then(|| Self {
            since: bucketed(since),
            until: q.until_ms.map(bucketed),
            include_internal: q.include_internal.unwrap_or(false),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn concurrent_callers_for_one_key_share_one_load() {
        let cache: Arc<SingleFlight<u8, u32>> =
            Arc::new(SingleFlight::new(Duration::from_secs(60)));
        let loads = Arc::new(AtomicUsize::new(0));
        let calls = (0..16).map(|_| {
            let (cache, loads) = (cache.clone(), loads.clone());
            tokio::spawn(async move {
                cache
                    .get_or_load(1, || async {
                        loads.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        Ok::<_, ()>(7)
                    })
                    .await
            })
        });
        for call in futures::future::join_all(calls).await {
            assert_eq!(call.unwrap(), Ok(7));
        }
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_failed_load_is_not_cached_and_an_aged_value_reloads() {
        let cache: SingleFlight<u8, u32> = SingleFlight::new(Duration::from_millis(30));
        assert_eq!(
            cache
                .get_or_load(1, || async { Err::<u32, _>("down") })
                .await,
            Err("down")
        );
        assert_eq!(
            cache.get_or_load(1, || async { Ok::<_, &str>(1) }).await,
            Ok(1)
        );
        assert_eq!(
            cache.get_or_load(1, || async { Ok::<_, &str>(2) }).await,
            Ok(1)
        );
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(
            cache.get_or_load(1, || async { Ok::<_, &str>(3) }).await,
            Ok(3)
        );
    }

    #[tokio::test]
    async fn aged_keys_are_forgotten() {
        let cache: SingleFlight<u8, u32> = SingleFlight::new(Duration::from_millis(10));
        for k in 0..5 {
            cache
                .get_or_load(k, || async { Ok::<_, ()>(0) })
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
        cache
            .get_or_load(9, || async { Ok::<_, ()>(0) })
            .await
            .unwrap();
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn polls_seconds_apart_share_a_key_and_filtered_reads_have_none() {
        let q = crate::usage::UsageQuery::default();
        let a = TotalsKey::of(&q, 1_700_000_001_000);
        let b = TotalsKey::of(&q, 1_700_000_004_000);
        assert_eq!(a, b);
        let filtered = crate::usage::UsageQuery {
            model: Some("m".into()),
            ..Default::default()
        };
        assert_eq!(TotalsKey::of(&filtered, 0), None);
    }
}
