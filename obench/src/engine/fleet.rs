#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum TrafficKind {
    ChatStream,
    ChatBuffered,
    Embed,
}

#[derive(Copy, Clone, Debug)]
pub struct TrafficType {
    pub model: &'static str,
    pub kind: TrafficKind,
    pub output_tokens: u32,
    pub weight: u32,
}

/// obench-managed fixture fleet. Names carry the latency-profile keyword the
/// GPU-free benchmark-backend keys off.
pub const FIXTURE_MODELS: &[&str] = &[
    "obench-turbo",
    "obench-base",
    "obench-code",
    "obench-large",
    "obench-embed",
];

pub const FIXTURE_GROUPS: &[(&str, u32)] = &[
    ("obench-chatbot", 500),
    ("obench-api", 50),
    ("obench-analytics", 100),
];

/// (tenant name, group, fairshare weight, traffic share)
pub const FIXTURE_TENANTS: &[(&str, &str, u32, u32)] = &[
    ("obench-chatbot", "obench-chatbot", 500, 35),
    ("obench-chatbot-2", "obench-chatbot", 500, 25),
    ("obench-api-batch", "obench-api", 50, 20),
    ("obench-analytics", "obench-analytics", 100, 15),
    ("obench-embeddings", "obench-api", 50, 5),
];

pub const FIXTURE_TRAFFIC: &[TrafficType] = &[
    TrafficType {
        model: "obench-turbo",
        kind: TrafficKind::ChatStream,
        output_tokens: 64,
        weight: 25,
    },
    TrafficType {
        model: "obench-base",
        kind: TrafficKind::ChatStream,
        output_tokens: 128,
        weight: 20,
    },
    TrafficType {
        model: "obench-base",
        kind: TrafficKind::ChatBuffered,
        output_tokens: 96,
        weight: 10,
    },
    TrafficType {
        model: "obench-large",
        kind: TrafficKind::ChatStream,
        output_tokens: 256,
        weight: 10,
    },
    TrafficType {
        model: "obench-code",
        kind: TrafficKind::ChatStream,
        output_tokens: 200,
        weight: 10,
    },
    TrafficType {
        model: "obench-embed",
        kind: TrafficKind::Embed,
        output_tokens: 0,
        weight: 25,
    },
];

/// Per-tenant request-dispatch weights.
///
/// The default fixture gives tenants unequal traffic shares, which is realistic
/// but makes a fairness measurement unreadable: a tenant offering less load than
/// its entitlement under-realizes for reasons that have nothing to do with
/// admission. `equal` flattens the shares so every tenant offers identical load
/// and the scheduler is the only thing deciding who gets served.
pub fn traffic_weights(shares: &[u32], equal: bool) -> Vec<u32> {
    shares
        .iter()
        .map(|s| if equal { 1 } else { (*s).max(1) })
        .collect()
}

/// Choose which tenant a given closed-loop worker should drive.
///
/// Sampling a tenant per request couples them: a worker blocked on a slow
/// tenant's queued request generates no load for anyone else, so whichever
/// tenant is most congested absorbs the worker pool and the rest go quiet.
/// That makes per-tenant saturation unreachable no matter how the traffic
/// shares are set. Pinning each worker to one tenant decouples the offered
/// loads, which a fairness measurement requires.
pub fn tenant_for_worker(worker: usize, shares: &[u32], pinned: bool, r: f64) -> usize {
    if shares.is_empty() {
        return 0;
    }
    if pinned {
        return worker % shares.len();
    }
    weighted_index(&traffic_weights(shares, false), r)
}

/// Pick an index proportional to weights. `r` in [0,1) is supplied by the
/// caller (e.g. rand) so this is deterministic and testable.
pub fn weighted_index(weights: &[u32], r: f64) -> usize {
    let total: u64 = weights.iter().map(|w| *w as u64).sum();
    if total == 0 {
        return 0;
    }
    let mut threshold = r * total as f64;
    for (i, w) in weights.iter().enumerate() {
        threshold -= *w as f64;
        if threshold < 0.0 {
            return i;
        }
    }
    weights.len() - 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_first_bucket_low_r() {
        assert_eq!(weighted_index(&[10, 10, 10], 0.0), 0);
    }

    #[test]
    fn picks_last_bucket_high_r() {
        assert_eq!(weighted_index(&[10, 10, 10], 0.999), 2);
    }

    #[test]
    fn respects_weight_boundaries() {
        // weights 1:3 -> r in [0,0.25) -> idx 0, [0.25,1) -> idx 1
        assert_eq!(weighted_index(&[1, 3], 0.20), 0);
        assert_eq!(weighted_index(&[1, 3], 0.30), 1);
    }

    #[test]
    fn zero_weights_safe() {
        assert_eq!(weighted_index(&[0, 0], 0.5), 0);
    }

    #[test]
    fn traffic_weights_preserve_configured_shares_by_default() {
        assert_eq!(
            traffic_weights(&[35, 25, 20, 15, 5], false),
            vec![35, 25, 20, 15, 5]
        );
    }

    #[test]
    fn equal_load_flattens_shares_so_every_tenant_offers_the_same() {
        assert_eq!(traffic_weights(&[35, 25, 20, 15, 5], true), vec![1; 5]);
    }

    #[test]
    fn a_zero_share_still_receives_traffic() {
        // weighted_index would otherwise never pick it, silently dropping a
        // tenant from the run.
        assert_eq!(traffic_weights(&[0, 10], false), vec![1, 10]);
    }

    #[test]
    fn no_tenants_yields_no_weights() {
        assert!(traffic_weights(&[], true).is_empty());
        assert!(traffic_weights(&[], false).is_empty());
    }

    #[test]
    fn pinned_workers_spread_round_robin_across_tenants() {
        let shares = [35, 25, 20, 15, 5];
        let got: Vec<usize> = (0..5)
            .map(|w| tenant_for_worker(w, &shares, true, 0.0))
            .collect();
        assert_eq!(got, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn pinned_workers_wrap_when_they_outnumber_tenants() {
        let shares = [1, 1, 1];
        let got: Vec<usize> = (0..7)
            .map(|w| tenant_for_worker(w, &shares, true, 0.9))
            .collect();
        // Every tenant gets at least two of the seven workers, and the random
        // draw is ignored entirely.
        assert_eq!(got, vec![0, 1, 2, 0, 1, 2, 0]);
    }

    #[test]
    fn unpinned_workers_still_sample_by_traffic_share() {
        // 1:3 split — r below 0.25 lands on the first tenant, above it the
        // second, regardless of which worker is asking.
        assert_eq!(tenant_for_worker(0, &[1, 3], false, 0.20), 0);
        assert_eq!(tenant_for_worker(0, &[1, 3], false, 0.30), 1);
        assert_eq!(tenant_for_worker(9, &[1, 3], false, 0.30), 1);
    }

    #[test]
    fn no_tenants_is_index_zero_rather_than_a_panic() {
        assert_eq!(tenant_for_worker(3, &[], true, 0.5), 0);
        assert_eq!(tenant_for_worker(3, &[], false, 0.5), 0);
    }

    #[test]
    fn fixture_catalog_is_nonempty() {
        assert_eq!(FIXTURE_MODELS.len(), 5);
        assert!(FIXTURE_TRAFFIC.iter().any(|t| t.kind == TrafficKind::Embed));
    }
}
