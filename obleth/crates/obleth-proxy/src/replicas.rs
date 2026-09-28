//! Replica heartbeat: tells fairshare how many gateway replicas are live.
//!
//! Each process registers a heartbeat in Redis under a unique instance id and
//! refreshes it every `interval`; the reply carries the number of unexpired
//! heartbeats, which is handed to [`FairShare::set_replicas`]. With shared
//! slots on, more than one live replica switches admissions to cluster-wide
//! slots; otherwise (and in fallback) the count divides the limits. The same
//! script call reclaims the shared slots of any replica whose heartbeat has
//! expired. That is one small script call per interval, nothing on the
//! request path. A failed heartbeat keeps the last count (see
//! [`ReplicaTracker`]), so a Redis outage freezes the count instead of
//! resizing pools, and a replica that never got a count acts as if it were
//! alone.

use std::future::Future;
use std::time::Duration;

use obleth_fairshare::{FairShare, ReplicaTracker};
use obleth_redis::RedisStore;
use tokio::task::JoinHandle;

/// This process's heartbeat identity, which is also its shared-slot holder
/// id: the host name (the pod name under
/// Kubernetes) plus a random suffix, so a restarted pod that reuses its name
/// never refreshes the previous process's entry.
pub(crate) fn instance_id() -> String {
    let host = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| std::fs::read_to_string("/etc/hostname").ok())
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "obleth".into());
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    format!("{host}-{}", &suffix[..8])
}

/// A running heartbeat. [`ReplicaHeartbeat::stop`] ends it and removes this
/// replica from the count, so the survivors grow at their next heartbeat
/// rather than one TTL later.
pub(crate) struct ReplicaHeartbeat {
    redis: RedisStore,
    instance: String,
    task: JoinHandle<()>,
}

impl ReplicaHeartbeat {
    /// Heartbeat once before returning, so the scheduler is sized for the
    /// fleet before the first request, then keep heartbeating every
    /// `interval` in the background. Returns the heartbeat and the replica
    /// count the scheduler starts with.
    pub(crate) async fn start(
        redis: RedisStore,
        fairshare: FairShare,
        instance: String,
        interval: Duration,
        ttl: Duration,
    ) -> (Self, usize) {
        let mut tracker = ReplicaTracker::new();
        let beat = {
            let redis = redis.clone();
            let instance = instance.clone();
            move || {
                let redis = redis.clone();
                let instance = instance.clone();
                async move { redis.replica_heartbeat(&instance, ttl).await }
            }
        };
        if let Some(n) = tracker.observe(beat().await) {
            fairshare.set_replicas(n);
        }
        let replicas = tracker.count();
        tracing::info!(
            instance = %instance,
            replicas,
            known = tracker.known(),
            interval_secs = interval.as_secs(),
            ttl_secs = ttl.as_secs(),
            "gateway replica heartbeat started"
        );
        let task = tokio::spawn(run_heartbeats(beat, fairshare, tracker, interval));
        (
            ReplicaHeartbeat {
                redis,
                instance,
                task,
            },
            replicas,
        )
    }

    /// Stop heartbeating and deregister, freeing any shared slot still on
    /// the books. Best effort: if Redis is down the entry simply expires
    /// after its TTL and the next live replica reclaims its slots.
    pub(crate) async fn stop(self) {
        self.task.abort();
        let _ = self.task.await;
        if let Err(e) = self.redis.replica_deregister(&self.instance).await {
            tracing::warn!(error = %e, "replica deregister failed; the heartbeat expires on its own");
        }
    }
}

/// Heartbeat every `interval` forever, resizing the scheduler whenever the
/// live count changes. The first tick fires one interval in, since
/// [`ReplicaHeartbeat::start`] has just heartbeat.
async fn run_heartbeats<F, Fut, E>(
    mut beat: F,
    fairshare: FairShare,
    mut tracker: ReplicaTracker,
    interval: Duration,
) where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<usize, E>>,
    E: std::fmt::Display,
{
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + interval, interval);
    // Catching up on missed ticks would only fire back-to-back heartbeats.
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tick.tick().await;
        if let Some(n) = tracker.observe(beat().await) {
            tracing::info!(
                replicas = n,
                "live gateway replica count changed; resizing fairshare"
            );
            fairshare.set_replicas(n);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use obleth_config::FairshareAlgorithm;
    use obleth_fairshare::StaticCapacity;

    async fn replicas(fs: &FairShare) -> usize {
        fs.snapshot().await.expect("snapshot").replicas
    }

    async fn wait_for_replicas(fs: &FairShare, n: usize) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while replicas(fs).await != n {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {n} replicas"));
    }

    #[test]
    fn instance_ids_are_unique_per_process_start() {
        let a = instance_id();
        let b = instance_id();
        assert_ne!(a, b);
        assert!(!a.is_empty() && !a.contains(char::is_whitespace), "{a}");
    }

    /// The loop follows the count Redis reports, keeps the last one while
    /// Redis is down, and follows again once it answers.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn the_loop_resizes_on_change_and_holds_through_an_outage() {
        let fs = FairShare::start(
            Arc::new(StaticCapacity::new(64)),
            FairshareAlgorithm::Weighted,
            8,
        );
        let script: Arc<Mutex<VecDeque<Result<usize, &'static str>>>> = Arc::new(Mutex::new(
            [Ok(3), Ok(3), Err("down"), Err("down"), Err("down"), Ok(2)].into(),
        ));
        // The scheduler's count as each heartbeat starts.
        let seen = Arc::new(Mutex::new(Vec::<usize>::new()));
        let beat = {
            let script = script.clone();
            let seen = seen.clone();
            let stats = fs.stats();
            move || {
                seen.lock()
                    .unwrap()
                    .push(stats.replicas.load(std::sync::atomic::Ordering::Relaxed));
                let next = script.lock().unwrap().pop_front().unwrap_or(Ok(2));
                async move { next }
            }
        };
        let task = tokio::spawn(run_heartbeats(
            beat,
            fs.clone(),
            ReplicaTracker::new(),
            Duration::from_millis(10),
        ));

        wait_for_replicas(&fs, 3).await;
        wait_for_replicas(&fs, 2).await;
        task.abort();
        let seen = seen.lock().unwrap().clone();
        let first_three = seen.iter().position(|n| *n == 3).expect("reached 3");
        let after = &seen[first_three..];
        assert!(
            after.iter().all(|n| *n == 3 || *n == 2),
            "the outage never drops the count back to 1: {seen:?}"
        );
        assert!(
            after.iter().filter(|n| **n == 3).count() >= 3,
            "3 holds through the failed heartbeats: {seen:?}"
        );
    }

    /// Redis down from the first heartbeat: the scheduler stays at 1, the
    /// per-process sizing of a single replica.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn redis_down_from_the_start_keeps_one_replica() {
        let fs = FairShare::start(
            Arc::new(StaticCapacity::new(64)),
            FairshareAlgorithm::Weighted,
            8,
        );
        let calls = Arc::new(Mutex::new(0usize));
        let beat = {
            let calls = calls.clone();
            move || {
                *calls.lock().unwrap() += 1;
                async { Err::<usize, _>("connection refused") }
            }
        };
        let task = tokio::spawn(run_heartbeats(
            beat,
            fs.clone(),
            ReplicaTracker::new(),
            Duration::from_millis(5),
        ));
        tokio::time::timeout(Duration::from_secs(5), async {
            while *calls.lock().unwrap() < 3 {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .expect("heartbeats keep running");
        assert_eq!(replicas(&fs).await, 1);
        task.abort();
    }

    /// Two replicas against the test Redis: each sees the other, and when
    /// one stops (cleanly here) the survivor grows back to the full size.
    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_replicas_split_the_pools_and_the_survivor_grows_back() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let redis = RedisStore::connect(&url).await.expect("connect");
        let start = || {
            FairShare::start(
                Arc::new(StaticCapacity::new(64)),
                FairshareAlgorithm::Weighted,
                8,
            )
        };
        let (fs_a, fs_b) = (start(), start());
        let interval = Duration::from_millis(50);
        let ttl = Duration::from_millis(400);
        // Shares the live key with any other gateway pointed at this Redis;
        // compare against the count before this test's replicas joined.
        let (a, _) =
            ReplicaHeartbeat::start(redis.clone(), fs_a.clone(), instance_id(), interval, ttl)
                .await;
        let base = replicas(&fs_a).await;
        let (b, n) =
            ReplicaHeartbeat::start(redis.clone(), fs_b.clone(), instance_id(), interval, ttl)
                .await;
        assert_eq!(n, base + 1);
        wait_for_replicas(&fs_a, base + 1).await;

        // b crashes: no deregister, its heartbeat just stops. a gets the
        // whole pool back once b's entry expires, about one TTL later.
        let crashed = std::time::Instant::now();
        b.task.abort();
        wait_for_replicas(&fs_a, base).await;
        assert!(
            crashed.elapsed() < ttl + interval * 4,
            "capacity came back after {:?}",
            crashed.elapsed()
        );
        a.stop().await;
    }
}
