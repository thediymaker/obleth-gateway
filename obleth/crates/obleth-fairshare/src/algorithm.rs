//! Group slot allocation helpers for hierarchical fairshare.

use std::collections::HashMap;
use std::hash::Hash;

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
