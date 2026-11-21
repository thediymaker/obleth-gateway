//! Group slot allocation helpers for hierarchical fairshare, and the share
//! split every view of a pool reports.

use std::collections::HashMap;
use std::hash::Hash;

use obleth_config::FairshareAlgorithm;
use uuid::Uuid;

/// A tenant as [`pool_shares`] sees it.
#[derive(Debug, Clone, Copy)]
pub struct ShareTenant<'a> {
    pub id: Uuid,
    pub group: &'a str,
    pub weight: i64,
    /// Has a request running or waiting in the pool.
    pub active: bool,
}

/// A key as [`pool_shares`] sees it.
#[derive(Debug, Clone, Copy)]
pub struct ShareKey {
    pub id: Uuid,
    pub tenant: Uuid,
    pub weight: i64,
    /// Has a request running or waiting in the pool.
    pub active: bool,
}

/// Each group's, tenant's and key's share of a pool, 0 to 1. Anything not
/// active has no share.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PoolShares {
    pub groups: HashMap<String, f64>,
    pub tenants: HashMap<Uuid, f64>,
    pub keys: HashMap<Uuid, f64>,
}

impl PoolShares {
    pub fn group(&self, name: &str) -> f64 {
        self.groups.get(name).copied().unwrap_or(0.0)
    }
    pub fn tenant(&self, id: &Uuid) -> f64 {
        self.tenants.get(id).copied().unwrap_or(0.0)
    }
    pub fn key(&self, id: &Uuid) -> f64 {
        self.keys.get(id).copied().unwrap_or(0.0)
    }
}

/// What share of a pool each active group, tenant and key is entitled to
/// while every one of them is busy, split the way the scheduler splits slots:
///
/// - groups (hierarchical): a group's weight over the weights of the groups
///   with an active tenant;
/// - tenants: hierarchical, the group's share split across the group's
///   active tenants by tenant weight; weighted, the tenant's weight over the
///   weights of every active tenant;
/// - keys: the tenant's share split across its active keys by key weight.
///
/// A replica's own snapshot and the cluster-wide view both use this, so the
/// two can only differ in which requests they count. Weights below 1 count
/// as 1; a group missing from `group_weights` weighs 100, the default.
pub fn pool_shares(
    algorithm: FairshareAlgorithm,
    group_weights: &HashMap<String, i64>,
    tenants: &[ShareTenant<'_>],
    keys: &[ShareKey],
) -> PoolShares {
    let mut shares = PoolShares::default();
    let group_weight = |g: &str| {
        group_weights
            .get(g)
            .copied()
            .unwrap_or(crate::DEFAULT_GROUP_WEIGHT)
            .max(1)
    };

    let mut active_groups: HashMap<&str, i64> = HashMap::new();
    let mut active_in_group: HashMap<&str, i64> = HashMap::new();
    let mut active_total: i64 = 0;
    for t in tenants.iter().filter(|t| t.active) {
        active_groups
            .entry(t.group)
            .or_insert_with(|| group_weight(t.group));
        *active_in_group.entry(t.group).or_insert(0) += t.weight.max(1);
        active_total += t.weight.max(1);
    }
    let group_total: i64 = active_groups.values().sum();
    for (name, w) in &active_groups {
        shares
            .groups
            .insert((*name).to_string(), *w as f64 / group_total as f64);
    }

    for t in tenants.iter().filter(|t| t.active) {
        let w = t.weight.max(1) as f64;
        let share = match algorithm {
            FairshareAlgorithm::Weighted => w / active_total as f64,
            FairshareAlgorithm::Hierarchical => {
                let in_group = active_in_group.get(t.group).copied().unwrap_or(1).max(1);
                shares.group(t.group) * w / in_group as f64
            }
        };
        shares.tenants.insert(t.id, share);
    }

    let mut active_key_weight: HashMap<Uuid, i64> = HashMap::new();
    for k in keys.iter().filter(|k| k.active) {
        *active_key_weight.entry(k.tenant).or_insert(0) += k.weight.max(1);
    }
    for k in keys.iter().filter(|k| k.active) {
        let total = active_key_weight
            .get(&k.tenant)
            .copied()
            .unwrap_or(1)
            .max(1);
        let share = shares.tenant(&k.tenant) * k.weight.max(1) as f64 / total as f64;
        shares.keys.insert(k.id, share);
    }
    shares
}

/// Integer slot caps per group that sum to at most `max`.
pub fn group_slot_caps(max: usize, groups: &[(String, i64)]) -> HashMap<String, usize> {
    weighted_caps(max, groups)
}

/// Split `max` integer slots across weighted items using largest-remainder
/// apportionment. Each active item is guaranteed at least one slot when total
/// capacity allows (`max >= items.len()`). Used both to divide global capacity
/// across groups and to divide a group's pool across its tenants by weight.
pub fn weighted_caps<K>(max: usize, items: &[(K, i64)]) -> HashMap<K, usize>
where
    K: Clone + Eq + Hash,
{
    let mut caps = HashMap::new();
    if max == 0 || items.is_empty() {
        return caps;
    }

    let total_weight: i64 = items.iter().map(|(_, w)| (*w).max(1)).sum();
    if total_weight <= 0 {
        return caps;
    }

    let n = items.len();
    let mut alloc: Vec<(K, usize, f64)> = items
        .iter()
        .map(|(key, weight)| {
            let exact = max as f64 * ((*weight).max(1) as f64) / (total_weight as f64);
            (key.clone(), exact.floor() as usize, exact - exact.floor())
        })
        .collect();

    // When capacity allows, guarantee each active item at least one slot.
    if max >= n {
        for (_, cap, _) in &mut alloc {
            if *cap == 0 {
                *cap = 1;
            }
        }
    }

    let mut used: usize = alloc.iter().map(|(_, c, _)| c).sum();
    if used > max {
        // Pay for the min-one raises from items above one, largest first, so
        // no item is pushed back to zero. Overflow only happens after raises
        // (`max >= n`), so items above one always exist while `used > max`.
        while used > max {
            let Some(idx) = (0..alloc.len())
                .filter(|&i| alloc[i].1 > 1)
                .max_by(|&a, &b| {
                    alloc[a].1.cmp(&alloc[b].1).then(
                        alloc[b]
                            .2
                            .partial_cmp(&alloc[a].2)
                            .unwrap_or(std::cmp::Ordering::Equal),
                    )
                })
            else {
                break;
            };
            alloc[idx].1 -= 1;
            used -= 1;
        }
    } else if used < max {
        let mut remaining = max - used;
        let mut indices: Vec<usize> = (0..alloc.len()).collect();
        indices.sort_by(|&a, &b| {
            alloc[b]
                .2
                .partial_cmp(&alloc[a].2)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        for idx in indices {
            if remaining == 0 {
                break;
            }
            alloc[idx].1 += 1;
            remaining -= 1;
        }
    }

    for (key, cap, _) in alloc {
        caps.insert(key, cap);
    }
    caps
}

#[cfg(test)]
mod tests {
    use super::*;

    fn weights(groups: &[(&str, i64)]) -> HashMap<String, i64> {
        groups.iter().map(|(n, w)| (n.to_string(), *w)).collect()
    }

    /// Hierarchical: groups split by weight, then tenants inside a group;
    /// an idle tenant takes nothing and dilutes nobody.
    #[test]
    fn pool_shares_split_groups_then_tenants_over_the_active_set() {
        let (a, b, c, idle) = (
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
        );
        let tenants = [
            ShareTenant {
                id: a,
                group: "research",
                weight: 300,
                active: true,
            },
            ShareTenant {
                id: b,
                group: "research",
                weight: 100,
                active: true,
            },
            ShareTenant {
                id: c,
                group: "teaching",
                weight: 100,
                active: true,
            },
            ShareTenant {
                id: idle,
                group: "research",
                weight: 1000,
                active: false,
            },
        ];
        let shares = pool_shares(
            FairshareAlgorithm::Hierarchical,
            &weights(&[("research", 300), ("teaching", 100)]),
            &tenants,
            &[],
        );
        assert!((shares.group("research") - 0.75).abs() < 1e-12);
        assert!((shares.group("teaching") - 0.25).abs() < 1e-12);
        assert!((shares.tenant(&a) - 0.75 * 0.75).abs() < 1e-12);
        assert!((shares.tenant(&b) - 0.75 * 0.25).abs() < 1e-12);
        assert!((shares.tenant(&c) - 0.25).abs() < 1e-12);
        assert_eq!(shares.tenant(&idle), 0.0);
        let total: f64 = shares.tenants.values().sum();
        assert!((total - 1.0).abs() < 1e-12, "active shares add to the pool");
    }

    #[test]
    fn pool_shares_weighted_ignores_groups_for_tenants() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let tenants = [
            ShareTenant {
                id: a,
                group: "x",
                weight: 300,
                active: true,
            },
            ShareTenant {
                id: b,
                group: "y",
                weight: 100,
                active: true,
            },
        ];
        let shares = pool_shares(FairshareAlgorithm::Weighted, &weights(&[]), &tenants, &[]);
        assert!((shares.tenant(&a) - 0.75).abs() < 1e-12);
        assert!((shares.tenant(&b) - 0.25).abs() < 1e-12);
        // Missing group weights count as the default 100.
        assert!((shares.group("x") - 0.5).abs() < 1e-12);
    }

    #[test]
    fn pool_shares_split_a_tenant_across_its_active_keys() {
        let t = Uuid::new_v4();
        let (k1, k2, k3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let tenants = [ShareTenant {
            id: t,
            group: "default",
            weight: 100,
            active: true,
        }];
        let keys = [
            ShareKey {
                id: k1,
                tenant: t,
                weight: 100,
                active: true,
            },
            ShareKey {
                id: k2,
                tenant: t,
                weight: 300,
                active: true,
            },
            ShareKey {
                id: k3,
                tenant: t,
                weight: 500,
                active: false,
            },
        ];
        let shares = pool_shares(
            FairshareAlgorithm::Hierarchical,
            &weights(&[]),
            &tenants,
            &keys,
        );
        assert!((shares.key(&k1) - 0.25).abs() < 1e-12);
        assert!((shares.key(&k2) - 0.75).abs() < 1e-12);
        assert_eq!(shares.key(&k3), 0.0);
    }

    #[test]
    fn pool_shares_of_an_idle_pool_are_empty() {
        let t = Uuid::new_v4();
        let tenants = [ShareTenant {
            id: t,
            group: "default",
            weight: 100,
            active: false,
        }];
        let shares = pool_shares(
            FairshareAlgorithm::Hierarchical,
            &weights(&[]),
            &tenants,
            &[],
        );
        assert_eq!(shares, PoolShares::default());
    }

    /// The obench fixture's group weights across the capacity sweep. Largest
    /// remainder must allocate every slot at each level: an unallocated
    /// remainder would leave capacity permanently idle under contention.
    #[test]
    fn fixture_group_caps_allocate_every_slot_across_the_sweep() {
        let groups = vec![
            ("chatbot".to_string(), 500i64),
            ("api".to_string(), 50),
            ("analytics".to_string(), 100),
        ];
        for max in [8usize, 16, 32, 64] {
            let caps = group_slot_caps(max, &groups);
            let total: usize = caps.values().sum();
            println!(
                "SWEEP max={max:>2} chatbot={} api={} analytics={} total={total}",
                caps["chatbot"], caps["api"], caps["analytics"]
            );
            assert_eq!(total, max, "max={max}: caps must allocate every slot");
        }
    }

    #[test]
    fn caps_split_500_50_with_min_one() {
        let groups = vec![("chatbot".into(), 500), ("api".into(), 50)];
        let caps = group_slot_caps(8, &groups);
        assert_eq!(caps.get("chatbot").copied(), Some(7));
        assert_eq!(caps.get("api").copied(), Some(1));
    }

    #[test]
    fn caps_equal_groups_split_evenly() {
        let groups = vec![("a".into(), 100), ("b".into(), 100)];
        let caps = group_slot_caps(8, &groups);
        assert_eq!(caps.get("a").copied(), Some(4));
        assert_eq!(caps.get("b").copied(), Some(4));
    }

    #[test]
    fn weighted_caps_split_group_pool_by_tenant_weight() {
        // A 10-slot group pool shared by two tenants weighted 3:1 should give
        // the boosted tenant roughly three times the slots.
        let tenants = vec![(1u32, 300), (2u32, 100)];
        let caps = weighted_caps(10, &tenants);
        assert_eq!(caps.get(&1).copied(), Some(8));
        assert_eq!(caps.get(&2).copied(), Some(2));
    }

    /// Characterises what happens when a group is apportioned fewer slots than
    /// it has active tenants: the min-one guarantee is conditional on
    /// `max >= items.len()`, so below that some tenants are capped at zero.
    #[test]
    fn caps_below_tenant_count_leave_some_tenants_at_zero() {
        for (cap, n) in [(1usize, 2usize), (1, 3), (1, 5), (2, 3), (2, 5)] {
            let items: Vec<(usize, i64)> = (0..n).map(|i| (i, 100)).collect();
            let caps = weighted_caps(cap, &items);
            let mut got: Vec<usize> = (0..n).map(|i| caps.get(&i).copied().unwrap_or(0)).collect();
            got.sort();
            let zeros = got.iter().filter(|c| **c == 0).count();
            assert_eq!(
                zeros,
                n - cap,
                "cap={cap} n={n}: expected {} tenants capped at zero, got {got:?}",
                n - cap
            );
            assert_eq!(got.iter().sum::<usize>(), cap, "caps must sum to the pool");
        }
    }

    /// Paying for the min-one raises must not take a slot back from an item
    /// that was just raised to one.
    #[test]
    fn min_one_raise_is_never_undone_by_the_trim() {
        let tenants = vec![(1u32, 1000), (2u32, 1), (3u32, 1), (4u32, 1)];
        let caps = weighted_caps(4, &tenants);
        for t in 1..=4u32 {
            assert_eq!(caps.get(&t).copied(), Some(1), "tenant {t}: {caps:?}");
        }
    }

    #[test]
    fn every_item_keeps_a_slot_whenever_max_covers_them() {
        let weight_sets: [&[i64]; 5] = [
            &[1000, 1, 1, 1],
            &[1000, 1000, 1, 1, 1],
            &[5000, 10, 1],
            &[1, 1, 1, 1, 1, 1],
            &[10_000, 1, 1, 1, 1, 1, 1, 1],
        ];
        for weights in weight_sets {
            let items: Vec<(usize, i64)> = weights.iter().copied().enumerate().collect();
            for max in weights.len()..weights.len() + 6 {
                let caps = weighted_caps(max, &items);
                let got: Vec<usize> = (0..items.len()).map(|i| caps[&i]).collect();
                assert!(
                    got.iter().all(|c| *c >= 1),
                    "weights={weights:?} max={max}: {got:?}"
                );
                assert_eq!(
                    got.iter().sum::<usize>(),
                    max,
                    "weights={weights:?} max={max}"
                );
            }
        }
    }

    #[test]
    fn weighted_caps_guarantee_min_one_when_room() {
        // Even a tiny-weight tenant keeps a slot when the pool has room for all.
        let tenants = vec![(1u32, 1000), (2u32, 1)];
        let caps = weighted_caps(4, &tenants);
        assert_eq!(caps.get(&1).copied(), Some(3));
        assert_eq!(caps.get(&2).copied(), Some(1));
    }
}
