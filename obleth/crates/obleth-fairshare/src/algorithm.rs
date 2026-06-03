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
        let mut indices: Vec<usize> = (0..alloc.len()).collect();
        indices.sort_by(|&a, &b| alloc[b].1.cmp(&alloc[a].1));
        for idx in indices {
            if used <= max {
                break;
            }
            if alloc[idx].1 > 0 {
                alloc[idx].1 -= 1;
                used -= 1;
            }
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

    #[test]
    fn weighted_caps_guarantee_min_one_when_room() {
        // Even a tiny-weight tenant keeps a slot when the pool has room for all.
        let tenants = vec![(1u32, 1000), (2u32, 1)];
        let caps = weighted_caps(4, &tenants);
        assert_eq!(caps.get(&1).copied(), Some(3));
        assert_eq!(caps.get(&2).copied(), Some(1));
    }
}
