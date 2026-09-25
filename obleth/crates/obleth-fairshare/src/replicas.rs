//! Replica-aware pool sizing.
//!
//! Admission state is per process, so with N gateway replicas behind one
//! Service every replica would admit a model's whole `max_in_flight` and the
//! upstream would see N times the configured concurrency. Each replica instead
//! enforces its share of every cluster-wide limit: `ceil(configured / live)`,
//! never below 1. The live count comes from Redis heartbeats (see
//! `obleth-redis`); this module holds the arithmetic and the fallback rules
//! for when that count cannot be read.
//!
//! Rounding up means the fleet can admit slightly more than configured, by
//! fewer than `live` slots per limit (8 slots over 3 replicas is 3 + 3 + 3).
//! Rounding down would instead strand capacity, and a limit smaller than the
//! replica count would round to zero and admit nothing.

/// This replica's share of a cluster-wide limit of `total` across `replicas`
/// live replicas: `ceil(total / replicas)`, at least 1. A count of 0 is read
/// as 1, since the replica asking is always live.
pub fn replica_share(total: usize, replicas: usize) -> usize {
    total.div_ceil(replicas.max(1)).max(1)
}

/// Turns heartbeat results into the replica count to size pools by.
///
/// A successful heartbeat is authoritative (floored at 1: this replica just
/// registered itself). A failed one keeps the last count that was read, so a
/// Redis outage freezes the pool sizes rather than resizing them; before any
/// count has been read, that is 1, the per-process behaviour. The first
/// failure after a success is logged, and so is the recovery, so a long
/// outage does not log every interval.
#[derive(Debug)]
pub struct ReplicaTracker {
    count: usize,
    known: bool,
    failing: bool,
}

impl Default for ReplicaTracker {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplicaTracker {
    pub fn new() -> Self {
        ReplicaTracker {
            count: 1,
            known: false,
            failing: false,
        }
    }

    /// The count pools are currently sized by.
    pub fn count(&self) -> usize {
        self.count
    }

    /// Whether a heartbeat has ever succeeded.
    pub fn known(&self) -> bool {
        self.known
    }

    /// Record one heartbeat outcome. Returns the new count when it changed,
    /// so the caller only resizes the scheduler on a real change.
    pub fn observe<E: std::fmt::Display>(&mut self, result: Result<usize, E>) -> Option<usize> {
        match result {
            Ok(live) => {
                let live = live.max(1);
                if self.failing {
                    tracing::info!(replicas = live, "replica heartbeat recovered");
                }
                self.failing = false;
                self.known = true;
                if live == self.count {
                    return None;
                }
                self.count = live;
                Some(live)
            }
            Err(e) => {
                if !self.failing {
                    self.failing = true;
                    tracing::warn!(
                        error = %e,
                        replicas = self.count,
                        known = self.known,
                        "replica heartbeat failed; fairshare keeps sizing pools for the last \
                         known replica count until Redis answers again"
                    );
                }
                None
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_rounds_up() {
        assert_eq!(replica_share(8, 3), 3);
        assert_eq!(replica_share(9, 3), 3);
        assert_eq!(replica_share(10, 3), 4);
        assert_eq!(replica_share(32, 5), 7);
        assert_eq!(replica_share(4096, 3), 1366);
    }

    #[test]
    fn share_never_drops_below_one() {
        assert_eq!(replica_share(1, 4), 1);
        assert_eq!(replica_share(2, 8), 1);
        assert_eq!(replica_share(0, 3), 1);
    }

    #[test]
    fn zero_replicas_reads_as_one() {
        assert_eq!(replica_share(32, 0), 32);
    }

    /// For every count, each share is the smallest that covers the total, so
    /// the fleet over-admits by fewer than `n` slots and never under-admits.
    #[test]
    fn shares_cover_the_total_with_less_than_one_slot_per_replica_over() {
        for total in 1..=64usize {
            for n in 1..=16usize {
                let share = replica_share(total, n);
                let aggregate = share * n;
                assert!(aggregate >= total, "total {total} over {n}: {aggregate}");
                assert!(aggregate < total + n, "total {total} over {n}: {aggregate}");
                if share > 1 {
                    assert!((share - 1) * n < total, "share {share} is not minimal");
                }
            }
        }
    }

    #[test]
    fn one_replica_is_the_configured_value() {
        for total in [1usize, 7, 32, 4096] {
            assert_eq!(replica_share(total, 1), total);
        }
    }

    #[test]
    fn tracker_reports_only_changes() {
        let mut t = ReplicaTracker::new();
        assert_eq!(t.count(), 1);
        assert!(!t.known());
        assert_eq!(t.observe::<&str>(Ok(1)), None, "1 is the starting count");
        assert!(t.known());
        assert_eq!(t.observe::<&str>(Ok(3)), Some(3));
        assert_eq!(t.observe::<&str>(Ok(3)), None);
        assert_eq!(t.observe::<&str>(Ok(2)), Some(2));
        assert_eq!(t.count(), 2);
    }

    #[test]
    fn tracker_floors_a_successful_count_at_one() {
        let mut t = ReplicaTracker::new();
        t.observe::<&str>(Ok(4));
        assert_eq!(t.observe::<&str>(Ok(0)), Some(1));
    }

    #[test]
    fn redis_down_before_any_count_keeps_per_process_sizing() {
        let mut t = ReplicaTracker::new();
        assert_eq!(t.observe(Err("connection refused")), None);
        assert_eq!(t.observe(Err("connection refused")), None);
        assert_eq!(t.count(), 1);
        assert!(!t.known());
    }

    #[test]
    fn redis_down_keeps_the_last_known_count_until_it_answers() {
        let mut t = ReplicaTracker::new();
        t.observe::<&str>(Ok(3));
        for _ in 0..5 {
            assert_eq!(t.observe(Err("timed out")), None);
        }
        assert_eq!(t.count(), 3);
        // Recovery is authoritative, whatever happened meanwhile.
        assert_eq!(t.observe::<&str>(Ok(2)), Some(2));
        assert_eq!(t.observe::<&str>(Ok(2)), None);
    }
}
