//! Per-pod memory of key hashes Redis said it does not know.
//!
//! A made-up API key that passes the shape check (any older `sk_` key, or a
//! checksummed one: the checksum is public CRC-32, so it only catches typos)
//! would otherwise cost a Redis round trip per request, as would a deleted
//! key left in a busy client. Remembering the miss for a minute keeps a
//! flood of the same key on this pod. Keys that fail their checksum never
//! get this far, identity handles are never
//! remembered (a miss there means "provision"), and a Redis error is never
//! remembered (it is not an answer).
//!
//! A key created, restored or re-synced is forgotten here on every pod: the
//! admin API publishes the key's hash on the invalidation channel (and, on
//! the pod that made the change, sends it straight to this process), and
//! both paths drop the hash from this cache as well as from the key cache.
//! A lookup that read Redis just before the key was pushed and records its
//! miss just after can still refuse the new key for up to [`KEY_MISS_TTL`].

use moka::future::Cache;
use std::future::Future;
use std::time::Duration;

/// How long a miss is remembered.
pub const KEY_MISS_TTL: Duration = Duration::from_secs(60);

/// Most misses remembered at once; the least useful are dropped past this.
pub const KEY_MISS_MAX_ENTRIES: u64 = 100_000;

#[derive(Clone)]
pub struct KeyMissCache(Cache<String, ()>);

impl Default for KeyMissCache {
    fn default() -> Self {
        Self::new(KEY_MISS_TTL, KEY_MISS_MAX_ENTRIES)
    }
}

impl KeyMissCache {
    pub fn new(ttl: Duration, max_entries: u64) -> Self {
        Self(
            Cache::builder()
                .time_to_live(ttl)
                .max_capacity(max_entries)
                .build(),
        )
    }

    /// Look `hash` up with `fetch`, unless a recent lookup already found
    /// nothing. With `remember_miss`, an `Ok(None)` answer is remembered and
    /// a remembered miss answers `Ok(None)` without calling `fetch`. Errors
    /// pass through and are never remembered.
    pub async fn lookup<T, E, F, Fut>(
        &self,
        hash: &str,
        remember_miss: bool,
        fetch: F,
    ) -> Result<Option<T>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Option<T>, E>>,
    {
        if remember_miss && self.0.contains_key(hash) {
            return Ok(None);
        }
        let found = fetch().await;
        if remember_miss && matches!(found, Ok(None)) {
            self.0.insert(hash.to_string(), ()).await;
        }
        found
    }

    /// Forget a remembered miss, because a key with this hash now exists.
    pub async fn forget(&self, hash: &str) {
        self.0.invalidate(hash).await;
    }

    /// Forget every remembered miss.
    pub fn clear(&self) {
        self.0.invalidate_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn lookup(
        cache: &KeyMissCache,
        calls: &AtomicUsize,
        remember: bool,
        answer: Result<Option<u8>, ()>,
    ) -> Result<Option<u8>, ()> {
        cache
            .lookup("h", remember, || async {
                calls.fetch_add(1, Ordering::SeqCst);
                answer
            })
            .await
    }

    #[test]
    fn misses_are_kept_a_minute_and_at_most_100k() {
        let cache = KeyMissCache::default();
        let policy = cache.0.policy();
        assert_eq!(policy.time_to_live(), Some(Duration::from_secs(60)));
        assert_eq!(policy.max_capacity(), Some(100_000));
    }

    #[tokio::test]
    async fn a_miss_is_remembered_and_a_hit_skips_the_lookup() {
        let (cache, calls) = (KeyMissCache::default(), AtomicUsize::new(0));
        assert_eq!(lookup(&cache, &calls, true, Ok(None)).await, Ok(None));
        // Remembered: the lookup is not made again, whatever it would say.
        assert_eq!(lookup(&cache, &calls, true, Ok(Some(1))).await, Ok(None));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        // A key with this hash appeared: the next lookup asks again.
        cache.forget("h").await;
        assert_eq!(lookup(&cache, &calls, true, Ok(Some(1))).await, Ok(Some(1)));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // A found key is not a miss, so it is looked up every time.
        assert_eq!(lookup(&cache, &calls, true, Ok(Some(1))).await, Ok(Some(1)));
        assert_eq!(calls.load(Ordering::SeqCst), 3);

        lookup(&cache, &calls, true, Ok(None)).await.unwrap();
        // moka clears what was written before the clear, by its clock: let
        // that clock move past the insert.
        tokio::time::sleep(Duration::from_millis(5)).await;
        cache.clear();
        assert_eq!(lookup(&cache, &calls, true, Ok(Some(2))).await, Ok(Some(2)));
    }

    #[tokio::test]
    async fn an_error_is_never_remembered() {
        let (cache, calls) = (KeyMissCache::default(), AtomicUsize::new(0));
        assert_eq!(lookup(&cache, &calls, true, Err(())).await, Err(()));
        assert_eq!(lookup(&cache, &calls, true, Err(())).await, Err(()));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn without_remember_misses_always_look_up() {
        let (cache, calls) = (KeyMissCache::default(), AtomicUsize::new(0));
        lookup(&cache, &calls, false, Ok(None)).await.unwrap();
        lookup(&cache, &calls, false, Ok(None)).await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // And a miss remembered by another caller does not answer for it.
        lookup(&cache, &calls, true, Ok(None)).await.unwrap();
        assert_eq!(
            lookup(&cache, &calls, false, Ok(Some(3))).await,
            Ok(Some(3))
        );
    }

    #[tokio::test]
    async fn misses_expire() {
        let cache = KeyMissCache::new(Duration::from_millis(50), 10);
        let calls = AtomicUsize::new(0);
        lookup(&cache, &calls, true, Ok(None)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(lookup(&cache, &calls, true, Ok(Some(4))).await, Ok(Some(4)));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
