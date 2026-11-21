//! The cluster-wide view of fairshare, for dashboards.
//!
//! With shared slots, pool sizes and caps hold across every replica, but each
//! replica keeps its own queue and decides on its own who goes next. One
//! replica's snapshot therefore shows the cluster-wide pool size next to only
//! its own share of the work: a tenant holding every slot of a pool across
//! three replicas reads as holding a third of it, and the requests queued on
//! the other two are not there at all.
//!
//! So that a dashboard can show the whole fleet, each replica periodically
//! publishes a compact [`ReplicaView`] of its scheduler (per pool: what each
//! group, tenant and key has running and waiting) through its
//! [`crate::SharedSlots`] backend, under its own replica id and with a short
//! expiry. [`crate::FairShare::cluster_snapshot`] reads the views of the other
//! live replicas, adds this replica's current one, and merges them with
//! [`merge_views`] into pools of the same shape as a replica's own snapshot.
//! Shares are recomputed from the merged active set with [`pool_shares`], the
//! split the scheduler itself reports, so a merged pool reads exactly as one
//! replica would if it held all of the fleet's work.
//!
//! A view lists only the pools with work, and in them only the tenants and
//! keys with a request running or waiting. It is bounded: at most
//! [`VIEW_MAX_TENANTS_PER_POOL`] tenants and [`VIEW_MAX_KEYS_PER_POOL`] keys
//! per pool, and [`VIEW_MAX_TENANTS`] and [`VIEW_MAX_KEYS`] over every pool,
//! the busiest first. Pool and group totals are always complete.

use std::collections::{BTreeMap, HashMap};

use obleth_config::FairshareAlgorithm;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
    group_slot_caps, pool_shares, EndUser, GroupFairshare, KeyFairshare, ModelPoolFairshare,
    PoolKey, ShareKey, ShareTenant, TenantFairshare,
};

/// The [`ReplicaView`] format this build writes. Readers skip a view they
/// cannot parse, so a mixed-version fleet degrades to fewer replicas
/// reporting rather than to a failed read.
pub const VIEW_VERSION: u32 = 1;
/// Most tenants one pool lists in a view.
pub const VIEW_MAX_TENANTS_PER_POOL: usize = 512;
/// Most keys one pool lists in a view.
pub const VIEW_MAX_KEYS_PER_POOL: usize = 128;
/// Most tenant entries in a view, over every pool.
pub const VIEW_MAX_TENANTS: usize = 2048;
/// Most key entries in a view, over every pool.
pub const VIEW_MAX_KEYS: usize = 512;

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// One replica's scheduler state, as it publishes it for the cluster view.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplicaView {
    #[serde(default)]
    pub version: u32,
    /// Pools with a request running or waiting on this replica.
    #[serde(default)]
    pub pools: Vec<ReplicaPoolView>,
}

/// One pool in a [`ReplicaView`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplicaPoolView {
    /// The pool's [`PoolKey::slot_id`].
    pub pool: String,
    /// The pool size as configured (cluster-wide).
    pub configured_cap: usize,
    pub in_flight: usize,
    /// The pool slots those requests hold: `in_flight`, or more in a
    /// background pool with long prompts. A replica that predates the field
    /// sends none, read as `in_flight`.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub slots: usize,
    pub queued: usize,
    /// Groups with a request running or waiting.
    #[serde(default)]
    pub groups: Vec<ReplicaGroupView>,
    /// Tenants with a request running or waiting, busiest first.
    #[serde(default)]
    pub tenants: Vec<ReplicaTenantView>,
    /// Keys with a request running or waiting, busiest first.
    #[serde(default)]
    pub keys: Vec<ReplicaKeyView>,
    /// Active tenants left out by the bounds.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_tenants: usize,
    /// Active keys left out by the bounds.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub omitted_keys: usize,
}

/// A group in a [`ReplicaPoolView`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplicaGroupView {
    pub name: String,
    pub weight: i64,
    pub in_flight: usize,
    pub queued: usize,
    /// Service the group's tenants received here, excluding the virtual time
    /// each was placed at (see [`GroupFairshare::served_tokens`]).
    #[serde(default)]
    pub served: f64,
}

/// A tenant in a [`ReplicaPoolView`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplicaTenantView {
    pub id: Uuid,
    pub group: String,
    pub weight: i64,
    /// The tenant's configured per-model cap, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<usize>,
    pub in_flight: usize,
    pub queued: usize,
    /// Service the tenant received here in this busy period, excluding the
    /// virtual time it was placed at.
    #[serde(default)]
    pub served: f64,
}

/// A key in a [`ReplicaPoolView`].
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ReplicaKeyView {
    pub id: Uuid,
    pub tenant: Uuid,
    pub weight: i64,
    /// The key's configured per-model cap, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap: Option<usize>,
    pub in_flight: usize,
    pub queued: usize,
    /// The key's service score here (see [`KeyFairshare::served_tokens`]).
    #[serde(default)]
    pub served: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_user: Option<EndUser>,
}

/// Another live replica's last published view.
#[derive(Debug, Clone, PartialEq)]
pub struct PeerView {
    /// The replica's id (its heartbeat identity).
    pub instance: String,
    pub view: ReplicaView,
    /// How long ago it was published, by the backend's clock.
    pub age_ms: u64,
}

/// What [`crate::SharedSlots::peer_views`] returns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PeerViews {
    /// Live replicas the backend knows of, the caller included when it is
    /// live itself.
    pub live: usize,
    /// The views of the other live replicas that have one. A live replica
    /// with no view yet (just started) or an unreadable one is left out.
    pub views: Vec<PeerView>,
}

/// Every live replica's scheduler state, merged (see the module docs).
#[derive(Debug, Clone)]
pub struct ClusterSnapshot {
    /// Pools with work on any replica, plus every pool this replica has,
    /// counted across the replicas in `replicas_reporting`. Each pool's
    /// `cap` is its configured size and `cluster_in_flight` is `None`.
    pub pools: Vec<ModelPoolFairshare>,
    /// Live replicas, this one included.
    pub replicas_live: usize,
    /// Replicas whose state the pools include: this one and every other live
    /// replica with a published view.
    pub replicas_reporting: usize,
    /// Age of the oldest view included, in ms; 0 with this replica alone.
    pub max_age_ms: u64,
}

/// Room left for tenant and key entries while a view is built.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ViewBudget {
    pub tenants: usize,
    pub keys: usize,
}

impl Default for ViewBudget {
    fn default() -> Self {
        ViewBudget {
            tenants: VIEW_MAX_TENANTS,
            keys: VIEW_MAX_KEYS,
        }
    }
}

/// Keep the busiest `limit` entries of `items` (by `load`, then id, so the
/// cut is stable), returning how many were dropped.
pub(crate) fn keep_busiest<T>(
    items: &mut Vec<T>,
    limit: usize,
    load: impl Fn(&T) -> (usize, Uuid),
) -> usize {
    items.sort_by(|a, b| {
        let (la, ia) = load(a);
        let (lb, ib) = load(b);
        lb.cmp(&la).then(ia.cmp(&ib))
    });
    let dropped = items.len().saturating_sub(limit);
    items.truncate(limit);
    dropped
}

#[derive(Default)]
struct GroupAcc {
    weight: Option<i64>,
    in_flight: usize,
    queued: usize,
    served: f64,
}

struct TenantAcc {
    group: String,
    weight: i64,
    cap: Option<usize>,
    in_flight: usize,
    queued: usize,
    served: f64,
}

struct KeyAcc {
    tenant: Uuid,
    weight: i64,
    cap: Option<usize>,
    in_flight: usize,
    queued: usize,
    served: f64,
    end_user: Option<EndUser>,
}

#[derive(Default)]
struct PoolAcc {
    configured_cap: Option<usize>,
    in_flight: usize,
    slots: usize,
    queued: usize,
    groups: BTreeMap<String, GroupAcc>,
    tenants: HashMap<Uuid, TenantAcc>,
    keys: HashMap<Uuid, KeyAcc>,
}

/// Merge replica views into one set of pools (see the module docs).
///
/// Counts and service add up across the views. Weights, groups, caps and
/// the pool size come from the first view that has them, so pass this
/// replica's own view first. Shares are recomputed over the merged active
/// set with [`pool_shares`]; group slot caps over the pool size, or less
/// when `ceiling` (the configured global ceiling) leaves less room than
/// that, as the scheduler computes them.
///
/// Each merged pool's `cap` is its configured size, the size every replica
/// enforces in shared mode, and `cluster_in_flight` is `None`. A tenant's
/// `served_tokens` (and so its `share_score`) is its service over the
/// views, excluding virtual-time placement; a key's is the sum of its
/// per-replica scores. Who goes next is still decided per replica.
pub fn merge_views(
    algorithm: FairshareAlgorithm,
    views: &[&ReplicaView],
    ceiling: usize,
) -> Vec<ModelPoolFairshare> {
    let mut order: Vec<String> = Vec::new();
    let mut pools: HashMap<String, PoolAcc> = HashMap::new();
    for view in views {
        for p in &view.pools {
            let acc = pools.entry(p.pool.clone()).or_insert_with(|| {
                order.push(p.pool.clone());
                PoolAcc::default()
            });
            acc.configured_cap.get_or_insert(p.configured_cap.max(1));
            acc.in_flight += p.in_flight;
            acc.slots += p.slots.max(p.in_flight);
            acc.queued += p.queued;
            for g in &p.groups {
                let e = acc.groups.entry(g.name.clone()).or_default();
                e.weight.get_or_insert(g.weight.max(1));
                e.in_flight += g.in_flight;
                e.queued += g.queued;
                e.served += g.served;
            }
            for t in &p.tenants {
                let e = acc.tenants.entry(t.id).or_insert_with(|| TenantAcc {
                    group: t.group.clone(),
                    weight: t.weight.max(1),
                    cap: None,
                    in_flight: 0,
                    queued: 0,
                    served: 0.0,
                });
                if e.cap.is_none() {
                    e.cap = t.cap;
                }
                e.in_flight += t.in_flight;
                e.queued += t.queued;
                e.served += t.served;
            }
            for k in &p.keys {
                let e = acc.keys.entry(k.id).or_insert_with(|| KeyAcc {
                    tenant: k.tenant,
                    weight: k.weight.max(1),
                    cap: None,
                    in_flight: 0,
                    queued: 0,
                    served: 0.0,
                    end_user: k.end_user.clone(),
                });
                if e.cap.is_none() {
                    e.cap = k.cap;
                }
                e.in_flight += k.in_flight;
                e.queued += k.queued;
                e.served += k.served;
            }
        }
    }

    let total_in_flight: usize = pools.values().map(|p| p.in_flight).sum();
    let headroom = ceiling.saturating_sub(total_in_flight);
    let mut out: Vec<ModelPoolFairshare> = order
        .into_iter()
        .filter_map(|id| {
            let acc = pools.remove(&id)?;
            Some(merge_pool(algorithm, &id, acc, headroom))
        })
        .collect();
    out.sort_by(|a, b| a.model.cmp(&b.model));
    out
}

fn merge_pool(
    algorithm: FairshareAlgorithm,
    id: &str,
    acc: PoolAcc,
    headroom: usize,
) -> ModelPoolFairshare {
    let model = match PoolKey::from_slot_id(id) {
        Some(key) => key.display_name().to_string(),
        None => id.to_string(),
    };
    let cap = acc.configured_cap.unwrap_or(1).max(1);
    let mut group_weights: HashMap<String, i64> = acc
        .groups
        .iter()
        .map(|(name, g)| (name.clone(), g.weight.unwrap_or(100).max(1)))
        .collect();
    for t in acc.tenants.values() {
        group_weights.entry(t.group.clone()).or_insert(100);
    }
    let share_tenants: Vec<ShareTenant<'_>> = acc
        .tenants
        .iter()
        .map(|(id, t)| ShareTenant {
            id: *id,
            group: &t.group,
            weight: t.weight,
            active: t.in_flight > 0 || t.queued > 0,
        })
        .collect();
    let share_keys: Vec<ShareKey> = acc
        .keys
        .iter()
        .map(|(id, k)| ShareKey {
            id: *id,
            tenant: k.tenant,
            weight: k.weight,
            active: k.in_flight > 0 || k.queued > 0,
        })
        .collect();
    let shares = pool_shares(algorithm, &group_weights, &share_tenants, &share_keys);

    // The same room the scheduler splits between groups: the pool, or less
    // when the global ceiling leaves less than that.
    let effective = cap.min(acc.slots.saturating_add(headroom)).max(1);
    let active_groups: Vec<(String, i64)> = shares
        .groups
        .keys()
        .map(|name| {
            let weight = group_weights.get(name).copied().unwrap_or(100);
            (name.clone(), weight)
        })
        .collect();
    let group_caps = group_slot_caps(effective, &active_groups);

    let mut groups: Vec<GroupFairshare> = acc
        .groups
        .iter()
        .map(|(name, g)| {
            let weight = g.weight.unwrap_or(100).max(1);
            let slot_cap = group_caps.get(name).copied().unwrap_or(0);
            GroupFairshare {
                name: name.clone(),
                weight,
                in_flight: g.in_flight,
                queued: g.queued,
                slot_cap,
                borrowed: g.in_flight.saturating_sub(slot_cap),
                served_tokens: g.served,
                share_score: g.served / weight as f64,
                weight_share: shares.group(name),
            }
        })
        .collect();
    groups.sort_by(|a, b| a.name.cmp(&b.name));

    let by_score = |a: f64, b: f64| a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal);
    let mut tenants: Vec<TenantFairshare> = acc
        .tenants
        .iter()
        .map(|(id, t)| TenantFairshare {
            tenant_id: *id,
            fairshare_group: t.group.clone(),
            weight: t.weight,
            max_in_flight: t.cap,
            in_flight: t.in_flight,
            queued: t.queued,
            served_tokens: t.served,
            share_score: t.served / t.weight as f64,
            weight_share: shares.tenant(id),
        })
        .collect();
    tenants
        .sort_by(|a, b| by_score(a.share_score, b.share_score).then(a.tenant_id.cmp(&b.tenant_id)));
    let mut keys: Vec<KeyFairshare> = acc
        .keys
        .iter()
        .map(|(id, k)| KeyFairshare {
            key_id: *id,
            tenant_id: k.tenant,
            weight: k.weight,
            max_in_flight: k.cap,
            in_flight: k.in_flight,
            queued: k.queued,
            served_tokens: k.served,
            share_score: k.served / k.weight as f64,
            weight_share: shares.key(id),
            end_user: k.end_user.clone(),
        })
        .collect();
    keys.sort_by(|a, b| by_score(a.share_score, b.share_score).then(a.key_id.cmp(&b.key_id)));

    ModelPoolFairshare {
        model,
        cap,
        configured_cap: cap,
        in_flight: acc.in_flight,
        slots: acc.slots,
        cluster_in_flight: None,
        queued: acc.queued,
        borrowed: groups.iter().map(|g| g.borrowed).sum(),
        held: 0,
        groups,
        tenants,
        keys,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tenant(id: Uuid, group: &str, in_flight: usize, queued: usize) -> ReplicaTenantView {
        ReplicaTenantView {
            id,
            group: group.into(),
            weight: 100,
            cap: None,
            in_flight,
            queued,
            served: in_flight as f64,
        }
    }

    fn key(id: Uuid, tenant: Uuid, in_flight: usize, queued: usize) -> ReplicaKeyView {
        ReplicaKeyView {
            id,
            tenant,
            weight: 100,
            cap: None,
            in_flight,
            queued,
            served: 0.0,
            end_user: None,
        }
    }

    fn group(name: &str, in_flight: usize, queued: usize) -> ReplicaGroupView {
        ReplicaGroupView {
            name: name.into(),
            weight: 100,
            in_flight,
            queued,
            served: in_flight as f64,
        }
    }

    /// One replica's view of a pool held by a single tenant on one key.
    fn single(
        pool: &str,
        cap: usize,
        t: Uuid,
        k: Uuid,
        in_flight: usize,
        queued: usize,
    ) -> ReplicaView {
        ReplicaView {
            version: VIEW_VERSION,
            pools: vec![ReplicaPoolView {
                pool: PoolKey::Model(pool.into()).slot_id(),
                configured_cap: cap,
                in_flight,
                queued,
                groups: vec![group("default", in_flight, queued)],
                tenants: vec![tenant(t, "default", in_flight, queued)],
                keys: vec![key(k, t, in_flight, queued)],
                ..Default::default()
            }],
        }
    }

    /// A tenant alone in a full 256-slot pool, spread over three replicas:
    /// merged, it holds every slot against a fair share of every slot, and
    /// every queued request is counted.
    #[test]
    fn a_single_tenant_full_pool_merges_to_the_whole_pool() {
        let (t, k) = (Uuid::new_v4(), Uuid::new_v4());
        let views = [
            single("m", 256, t, k, 42, 350),
            single("m", 256, t, k, 96, 209),
            single("m", 256, t, k, 118, 208),
        ];
        let refs: Vec<&ReplicaView> = views.iter().collect();
        let pools = merge_views(FairshareAlgorithm::Hierarchical, &refs, 4096);
        assert_eq!(pools.len(), 1);
        let p = &pools[0];
        assert_eq!(p.model, "m");
        assert_eq!((p.cap, p.configured_cap), (256, 256));
        assert_eq!((p.in_flight, p.queued), (256, 767));
        assert_eq!(p.cluster_in_flight, None);
        let tenant = &p.tenants[0];
        assert_eq!((tenant.in_flight, tenant.queued), (256, 767));
        assert!((tenant.weight_share - 1.0).abs() < 1e-12);
        let expected = tenant.weight_share * p.cap as f64;
        assert!(
            tenant.in_flight as f64 >= expected.floor(),
            "the tenant holds its whole fair share"
        );
        let g = &p.groups[0];
        assert_eq!(
            (g.in_flight, g.queued, g.slot_cap, g.borrowed),
            (256, 767, 256, 0)
        );
        assert!((g.weight_share - 1.0).abs() < 1e-12);
        let key = &p.keys[0];
        assert_eq!((key.in_flight, key.queued), (256, 767));
        assert!((key.weight_share - 1.0).abs() < 1e-12);
    }

    /// Shares are recomputed over the merged active set: a tenant active
    /// only on another replica still takes its share here.
    #[test]
    fn shares_follow_the_cluster_wide_active_set() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let (ka, kb) = (Uuid::new_v4(), Uuid::new_v4());
        let here = single("m", 10, a, ka, 10, 4);
        let there = single("m", 10, b, kb, 0, 3);
        let pools = merge_views(FairshareAlgorithm::Weighted, &[&here, &there], 4096);
        let p = &pools[0];
        assert_eq!((p.in_flight, p.queued), (10, 7));
        for t in &p.tenants {
            assert!((t.weight_share - 0.5).abs() < 1e-12, "{t:?}");
        }
        let b_row = p.tenants.iter().find(|t| t.tenant_id == b).unwrap();
        assert_eq!((b_row.in_flight, b_row.queued), (0, 3));
        // `a` holds 10 against a fair share of 5; `b` waits with none.
        let a_row = p.tenants.iter().find(|t| t.tenant_id == a).unwrap();
        assert_eq!(a_row.in_flight, 10);
    }

    /// Pools only another replica has are part of the merged view, sized
    /// from that replica's view.
    #[test]
    fn pools_from_every_replica_are_listed() {
        let (t, k) = (Uuid::new_v4(), Uuid::new_v4());
        let here = single("a", 8, t, k, 1, 0);
        let there = single("b", 16, t, k, 2, 1);
        let pools = merge_views(FairshareAlgorithm::Hierarchical, &[&here, &there], 4096);
        let names: Vec<(&str, usize)> = pools.iter().map(|p| (p.model.as_str(), p.cap)).collect();
        assert_eq!(names, vec![("a", 8), ("b", 16)]);
    }

    /// Hierarchical: a group's share splits by group weight first, then by
    /// tenant weight inside the group, over the merged active tenants.
    #[test]
    fn hierarchical_shares_split_by_group_then_tenant() {
        let (r1, r2, s) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let pool = PoolKey::Model("m".into()).slot_id();
        let view = |tenants: Vec<ReplicaTenantView>, groups: Vec<ReplicaGroupView>| ReplicaView {
            version: VIEW_VERSION,
            pools: vec![ReplicaPoolView {
                pool: pool.clone(),
                configured_cap: 12,
                in_flight: tenants.iter().map(|t| t.in_flight).sum(),
                queued: tenants.iter().map(|t| t.queued).sum(),
                groups,
                tenants,
                ..Default::default()
            }],
        };
        let mut research = group("research", 6, 0);
        research.weight = 200;
        let here = view(
            vec![tenant(r1, "research", 6, 0), tenant(s, "teaching", 2, 1)],
            vec![research.clone(), group("teaching", 2, 1)],
        );
        let there = view(
            vec![tenant(r2, "research", 4, 2)],
            vec![group("research", 4, 2)],
        );
        let pools = merge_views(FairshareAlgorithm::Hierarchical, &[&here, &there], 4096);
        let p = &pools[0];
        let share = |id: Uuid| {
            p.tenants
                .iter()
                .find(|t| t.tenant_id == id)
                .unwrap()
                .weight_share
        };
        // research weighs 200 (from the first view that names it), teaching 100.
        assert!((share(r1) - 1.0 / 3.0).abs() < 1e-12);
        assert!((share(r2) - 1.0 / 3.0).abs() < 1e-12);
        assert!((share(s) - 1.0 / 3.0).abs() < 1e-12);
        let g = |name: &str| p.groups.iter().find(|g| g.name == name).unwrap().clone();
        assert_eq!((g("research").in_flight, g("research").queued), (10, 2));
        assert_eq!(g("research").slot_cap, 8);
        assert_eq!(g("research").borrowed, 2);
        assert_eq!(g("teaching").slot_cap, 4);
    }

    /// The global ceiling bounds the room groups split, as on a replica.
    #[test]
    fn group_caps_follow_the_ceiling_when_it_binds() {
        let (t, k) = (Uuid::new_v4(), Uuid::new_v4());
        let here = single("m", 64, t, k, 6, 3);
        let pools = merge_views(FairshareAlgorithm::Hierarchical, &[&here], 8);
        assert_eq!(
            pools[0].groups[0].slot_cap, 8,
            "6 running + 2 left under the ceiling"
        );
    }

    #[test]
    fn keep_busiest_drops_the_quietest_and_counts_them() {
        let ids: Vec<Uuid> = (0..5).map(|_| Uuid::new_v4()).collect();
        let mut items: Vec<(Uuid, usize)> = ids.iter().copied().zip([3, 9, 1, 7, 5]).collect();
        let dropped = keep_busiest(&mut items, 3, |(id, n)| (*n, *id));
        assert_eq!(dropped, 2);
        assert_eq!(
            items.iter().map(|(_, n)| *n).collect::<Vec<_>>(),
            vec![9, 7, 5]
        );
    }

    /// The wire format round-trips and leaves out what is empty.
    #[test]
    fn views_round_trip_through_serde() {
        let (t, k) = (Uuid::new_v4(), Uuid::new_v4());
        let mut view = single("m", 8, t, k, 2, 1);
        view.pools[0].keys[0].end_user = Some(EndUser {
            parent_key: k,
            name: "someone".into(),
        });
        let json = serde_json::to_string(&view).unwrap();
        assert!(!json.contains("omitted"), "{json}");
        let back: ReplicaView = serde_json::from_str(&json).unwrap();
        assert_eq!(back, view);
    }
}
