//! Bounded in-memory history of scheduler samples, for the dashboard's
//! activity chart. It sits beside the scheduler, not inside it: a sampler
//! task asks the scheduler for a snapshot and stores what it gets, so the
//! scheduler task stays the sole owner of admission state.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::FairshareSnapshot;

/// How often the gateway samples the scheduler. The dashboard polls at the
/// same rate, so replayed and live points line up.
pub const FAIRSHARE_HISTORY_INTERVAL_MS: u64 = 2_000;

/// One group's occupancy inside one pool at sample time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupSample {
    pub name: String,
    pub in_flight: usize,
    pub queued: usize,
}

/// One model pool at sample time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PoolSample {
    pub model: String,
    pub cap: usize,
    pub in_flight: usize,
    pub queued: usize,
    pub groups: Vec<GroupSample>,
}

/// One sampled point of scheduler state across every pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FairshareSample {
    pub ts_ms: i64,
    pub global_in_flight: usize,
    pub global_queued: usize,
    pub pools: Vec<PoolSample>,
}

impl FairshareSample {
    pub fn from_snapshot(ts_ms: i64, snap: &FairshareSnapshot) -> Self {
        FairshareSample {
            ts_ms,
            global_in_flight: snap.global_in_flight,
            global_queued: snap.global_queued,
            pools: snap
                .pools
                .iter()
                .map(|p| PoolSample {
                    model: p.model.clone(),
                    cap: p.cap,
                    in_flight: p.in_flight,
                    queued: p.queued,
                    groups: p
                        .groups
                        .iter()
                        .map(|g| GroupSample {
                            name: g.name.clone(),
                            in_flight: g.in_flight,
                            queued: g.queued,
                        })
                        .collect(),
                })
                .collect(),
        }
    }
}

/// A sample projected to one scope: a single pool, or the aggregate across
/// pools. `groups` maps group name to in-flight slots.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryPoint {
    pub ts_ms: i64,
    pub in_flight: usize,
    pub queued: usize,
    pub groups: BTreeMap<String, usize>,
}

/// Bounded, time-ordered ring of samples. `max_len == 0` records nothing.
#[derive(Debug)]
pub struct FairshareHistory {
    samples: Mutex<VecDeque<FairshareSample>>,
    max_len: usize,
}

impl FairshareHistory {
    pub fn new(max_len: usize) -> Self {
        FairshareHistory {
            samples: Mutex::new(VecDeque::with_capacity(max_len.min(4096))),
            max_len,
        }
    }

    pub fn max_len(&self) -> usize {
        self.max_len
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, VecDeque<FairshareSample>> {
        self.samples.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Append a sample, dropping the oldest once the ring is full.
    pub fn push(&self, sample: FairshareSample) {
        if self.max_len == 0 {
            return;
        }
        let mut samples = self.lock();
        while samples.len() >= self.max_len {
            samples.pop_front();
        }
        samples.push_back(sample);
    }

    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.lock().is_empty()
    }

    pub fn oldest_ts_ms(&self) -> Option<i64> {
        self.lock().front().map(|s| s.ts_ms)
    }

    pub fn newest_ts_ms(&self) -> Option<i64> {
        self.lock().back().map(|s| s.ts_ms)
    }

    /// Samples at or after `since_ms`, projected to `model` or to the
    /// aggregate when `model` is `None`. `exclude_group`, when set, hides that
    /// group's traffic the same way `/fairshare/live` hides prober traffic:
    /// dropped from `groups` and subtracted from `in_flight`/`queued`.
    /// Ascending by time.
    pub fn points(
        &self,
        since_ms: i64,
        model: Option<&str>,
        exclude_group: Option<&str>,
    ) -> Vec<HistoryPoint> {
        self.lock()
            .iter()
            .filter(|s| s.ts_ms >= since_ms)
            .map(|s| project(s, model, exclude_group))
            .collect()
    }
}

fn project(
    sample: &FairshareSample,
    model: Option<&str>,
    exclude_group: Option<&str>,
) -> HistoryPoint {
    match model {
        Some(name) => match sample.pools.iter().find(|p| p.model == name) {
            Some(p) => {
                let mut in_flight = p.in_flight;
                let mut queued = p.queued;
                let mut groups: BTreeMap<String, usize> = BTreeMap::new();
                for g in &p.groups {
                    if exclude_group == Some(g.name.as_str()) {
                        in_flight = in_flight.saturating_sub(g.in_flight);
                        queued = queued.saturating_sub(g.queued);
                        continue;
                    }
                    groups.insert(g.name.clone(), g.in_flight);
                }
                HistoryPoint {
                    ts_ms: sample.ts_ms,
                    in_flight,
                    queued,
                    groups,
                }
            }
            None => HistoryPoint {
                ts_ms: sample.ts_ms,
                in_flight: 0,
                queued: 0,
                groups: BTreeMap::new(),
            },
        },
        None => {
            let mut groups: BTreeMap<String, usize> = BTreeMap::new();
            let mut excluded_in_flight = 0usize;
            let mut excluded_queued = 0usize;
            for p in &sample.pools {
                for g in &p.groups {
                    if exclude_group == Some(g.name.as_str()) {
                        excluded_in_flight += g.in_flight;
                        excluded_queued += g.queued;
                        continue;
                    }
                    if g.in_flight > 0 || g.queued > 0 {
                        *groups.entry(g.name.clone()).or_insert(0) += g.in_flight;
                    }
                }
            }
            HistoryPoint {
                ts_ms: sample.ts_ms,
                in_flight: sample.global_in_flight.saturating_sub(excluded_in_flight),
                queued: sample.global_queued.saturating_sub(excluded_queued),
                groups,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FairshareSnapshot, GroupFairshare, ModelPoolFairshare};

    fn group(name: &str, in_flight: usize, queued: usize) -> GroupFairshare {
        GroupFairshare {
            name: name.into(),
            weight: 100,
            in_flight,
            queued,
            slot_cap: 0,
            borrowed: 0,
            served_tokens: 0.0,
            share_score: 0.0,
            weight_share: 0.0,
        }
    }

    fn pool(
        model: &str,
        cap: usize,
        in_flight: usize,
        queued: usize,
        groups: Vec<GroupFairshare>,
    ) -> ModelPoolFairshare {
        ModelPoolFairshare {
            model: model.into(),
            cap,
            in_flight,
            queued,
            borrowed: 0,
            groups,
            tenants: Vec::new(),
            keys: Vec::new(),
        }
    }

    fn snapshot(pools: Vec<ModelPoolFairshare>) -> FairshareSnapshot {
        FairshareSnapshot {
            algorithm: "hierarchical".into(),
            max_in_flight: 64,
            default_model_max_in_flight: 32,
            global_in_flight: pools.iter().map(|p| p.in_flight).sum(),
            global_queued: pools.iter().map(|p| p.queued).sum(),
            global_borrowed: 0,
            pools,
            model_in_flight: Default::default(),
            model_queued: Default::default(),
        }
    }

    fn sample(ts_ms: i64) -> FairshareSample {
        FairshareSample::from_snapshot(
            ts_ms,
            &snapshot(vec![
                pool(
                    "m",
                    8,
                    3,
                    1,
                    vec![group("research", 2, 1), group("teaching", 1, 0)],
                ),
                pool("n", 8, 5, 4, vec![group("research", 5, 4)]),
            ]),
        )
    }

    #[test]
    fn from_snapshot_maps_every_field() {
        let s = sample(1_000);
        assert_eq!(s.ts_ms, 1_000);
        assert_eq!(s.global_in_flight, 8);
        assert_eq!(s.global_queued, 5);
        assert_eq!(s.pools.len(), 2);
        let m = &s.pools[0];
        assert_eq!(
            (m.model.as_str(), m.cap, m.in_flight, m.queued),
            ("m", 8, 3, 1)
        );
        assert_eq!(
            m.groups[0],
            GroupSample {
                name: "research".into(),
                in_flight: 2,
                queued: 1
            }
        );
    }

    #[test]
    fn push_keeps_only_the_newest_max_len() {
        let h = FairshareHistory::new(3);
        for ts in [1, 2, 3, 4, 5] {
            h.push(sample(ts));
        }
        assert_eq!(h.len(), 3);
        assert_eq!(h.oldest_ts_ms(), Some(3));
        assert_eq!(h.newest_ts_ms(), Some(5));
    }

    #[test]
    fn zero_max_len_records_nothing() {
        let h = FairshareHistory::new(0);
        h.push(sample(1));
        assert!(h.is_empty());
        assert_eq!(h.oldest_ts_ms(), None);
        assert!(h.points(0, None, None).is_empty());
    }

    #[test]
    fn points_filter_by_since_and_stay_ascending() {
        let h = FairshareHistory::new(10);
        for ts in [1_000, 3_000, 5_000] {
            h.push(sample(ts));
        }
        let ts: Vec<i64> = h
            .points(3_000, None, None)
            .iter()
            .map(|p| p.ts_ms)
            .collect();
        assert_eq!(ts, vec![3_000, 5_000]);
    }

    #[test]
    fn aggregate_sums_groups_across_pools() {
        let h = FairshareHistory::new(10);
        h.push(sample(1));
        let p = &h.points(0, None, None)[0];
        assert_eq!((p.in_flight, p.queued), (8, 5));
        assert_eq!(p.groups.get("research"), Some(&7));
        assert_eq!(p.groups.get("teaching"), Some(&1));
    }

    #[test]
    fn model_scope_returns_that_pool_or_zeros() {
        let h = FairshareHistory::new(10);
        h.push(sample(1));
        let m = &h.points(0, Some("m"), None)[0];
        assert_eq!((m.in_flight, m.queued), (3, 1));
        assert_eq!(m.groups.get("research"), Some(&2));
        assert_eq!(m.groups.get("teaching"), Some(&1));
        let missing = &h.points(0, Some("absent"), None)[0];
        assert_eq!(
            (missing.ts_ms, missing.in_flight, missing.queued),
            (1, 0, 0)
        );
        assert!(missing.groups.is_empty());
    }

    #[test]
    fn excluded_group_is_hidden_from_scope_and_aggregate() {
        let h = FairshareHistory::new(10);
        h.push(FairshareSample::from_snapshot(
            1,
            &snapshot(vec![pool("m", 8, 2, 1, vec![group("model-health", 2, 1)])]),
        ));
        let agg = &h.points(0, None, Some("model-health"))[0];
        assert_eq!((agg.in_flight, agg.queued), (0, 0));
        assert!(agg.groups.is_empty(), "aggregate groups: {:?}", agg.groups);

        let scoped = &h.points(0, Some("m"), Some("model-health"))[0];
        assert_eq!((scoped.in_flight, scoped.queued), (0, 0));
        assert!(
            scoped.groups.is_empty(),
            "scoped groups: {:?}",
            scoped.groups
        );
    }
}
