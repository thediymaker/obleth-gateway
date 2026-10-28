//! Live load per model pool: how full each model is right now, as the
//! dashboard's Fairshare page reads it, without the groups, tenants and keys
//! behind the numbers. The data plane serves it to callers as `GET /v1/load`.
//!
//! The numbers come from where `GET /api/v1/fairshare/live` takes them: the
//! scheduler's pools merged across replicas in `shared` mode (else the
//! answering replica's own), the shared slot counts for requests in flight,
//! and each model's pool size from capacity discovery, its `max_in_flight` or
//! the gateway default. The health prober is left out, as it is there.

use std::collections::{HashMap, HashSet};

use obleth_config::ResolvedModel;
use obleth_fairshare::{replica_share, FairShare, ModelPoolFairshare, PoolKey, SlotMode};
use uuid::Uuid;

use crate::capacity_discovery::CapacityDiscovery;

/// A model to read the load of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadModel {
    /// The model's pool: its `model_name`.
    pub name: String,
    /// Its own pool size; `None` or 0 for the gateway default.
    pub max_in_flight: Option<usize>,
    /// Whether it has somewhere to send a request right now (see
    /// [`crate::has_servers`]).
    pub has_servers: bool,
}

impl LoadModel {
    /// A registered route, with `none_ready` the models discovery last saw
    /// with no ready replica ([`crate::models_without_ready_replicas`]).
    pub fn of(route: &ResolvedModel, none_ready: &HashSet<String>) -> Self {
        LoadModel {
            name: route.model_name.clone(),
            max_in_flight: route.max_in_flight,
            has_servers: route.enabled
                && crate::has_servers(
                    &route.api_base,
                    route.endpoints.iter().any(|e| e.enabled),
                    none_ready.contains(&route.model_name),
                ),
        }
    }
}

/// One model pool's load, in the [`LiveLoad`]'s scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolLoad {
    /// Requests the pool runs at once: its size cluster-wide in `shared`
    /// mode, the answering replica's share of it in the other modes. 0 when
    /// the model has nowhere to send a request.
    pub slots: usize,
    /// Requests running in the pool: across every replica in `shared` mode,
    /// the answering replica's otherwise.
    pub in_flight: usize,
    /// Requests waiting for a slot in the pool.
    pub waiting: usize,
}

/// Every model pool's load at one moment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LiveLoad {
    /// `cluster` when the counts cover every gateway replica, `replica` when
    /// they are the answering replica's own.
    pub scope: &'static str,
    /// What configured limits are divided by on the answering replica: 1
    /// unless they are split across replicas.
    pub divisor: usize,
    /// By model name.
    pub pools: HashMap<String, PoolLoad>,
    /// Each scheduler key's requests in flight, by pool, in the same scope.
    pub keys: HashMap<Uuid, HashMap<String, usize>>,
}

impl LiveLoad {
    /// Requests the scheduler key `key` has in flight in `pool`, or in
    /// every pool.
    pub fn key_in_flight(&self, key: Uuid, pool: Option<&str>) -> usize {
        let Some(pools) = self.keys.get(&key) else {
            return 0;
        };
        match pool {
            Some(pool) => pools.get(pool).copied().unwrap_or(0),
            None => pools.values().sum(),
        }
    }
}

/// The gateway settings a reading depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoadSettings {
    /// `OBLETH_FAIRSHARE_REPLICA_AWARE`.
    pub replica_aware: bool,
    /// Whether the replica count is known: a replica heartbeat runs
    /// (`OBLETH_FAIRSHARE_SHARED_SLOTS` or `OBLETH_FAIRSHARE_REPLICA_AWARE`
    /// on). Without it, a replica cannot tell it is the only one.
    pub replicas_known: bool,
    /// `OBLETH_DEFAULT_MODEL_MAX_IN_FLIGHT`.
    pub default_cap: usize,
}

/// Read every pool's load for `routes` (each by its `model_name`). `None`
/// when the scheduler does not answer.
pub async fn live_load<'a>(
    fairshare: &FairShare,
    settings: &LoadSettings,
    discovery: &CapacityDiscovery,
    routes: impl IntoIterator<Item = &'a ResolvedModel>,
) -> Option<LiveLoad> {
    let snap = fairshare.snapshot().await?;
    let stats = fairshare.stats();
    let (mode, replicas, divisor) = crate::enforcement(&stats, settings.replica_aware);
    let scope = crate::live_scope(fairshare, mode).await;
    let source = scope
        .cluster
        .as_ref()
        .map_or(snap.pools.as_slice(), |c| c.pools.as_slice());
    let none_ready = crate::models_without_ready_replicas(discovery);
    let models: Vec<LoadModel> = routes
        .into_iter()
        .map(|r| LoadModel::of(r, &none_ready))
        .collect();
    let pool_keys: Vec<PoolKey> = models
        .iter()
        .map(|m| PoolKey::Model(m.name.clone()))
        .collect();
    let fresh = crate::cluster_in_flight(fairshare, &stats, mode, &pool_keys)
        .await
        .map(|(_, per)| per)
        .unwrap_or_else(|| vec![None; pool_keys.len()]);
    let sizing = Sizing {
        shared: mode == SlotMode::Shared,
        divisor,
        default_cap: settings.default_cap,
        discovered: &discovery.effective_caps(),
    };
    // The only live replica answers for the whole fleet.
    let whole = scope.cluster.is_some()
        || (settings.replicas_known && mode == SlotMode::Local && replicas == 1);
    Some(LiveLoad {
        scope: if whole { "cluster" } else { "replica" },
        divisor,
        pools: pool_loads(source, &snap.pools, &fresh, &sizing, &models),
        keys: key_in_flight(source),
    })
}

/// How the answering replica sizes pools right now.
pub struct Sizing<'a> {
    /// Limits are cluster-wide slots (`shared` mode).
    pub shared: bool,
    /// What configured sizes are divided by (see [`crate::enforcement`]).
    pub divisor: usize,
    /// `OBLETH_DEFAULT_MODEL_MAX_IN_FLIGHT`.
    pub default_cap: usize,
    /// The pool sizes capacity discovery set, by model name.
    pub discovered: &'a HashMap<String, usize>,
}

/// Each of `models`' load. `source` is the scheduler's pools in the view's
/// scope, `local` the answering replica's own, and `fresh[i]` the shared
/// slot count of `models[i]`'s pool read just now (`None` outside `shared`
/// mode or when it could not be read).
///
/// A pool the scheduler has is read as the Fairshare page reads it: in
/// `shared` mode its configured size and its cluster-wide count in flight,
/// otherwise the size the replica enforces and the replica's count. A model
/// with no pool yet (no traffic since its pool was last dropped) has its
/// configured size and nothing waiting.
pub fn pool_loads(
    source: &[ModelPoolFairshare],
    local: &[ModelPoolFairshare],
    fresh: &[Option<usize>],
    sizing: &Sizing<'_>,
    models: &[LoadModel],
) -> HashMap<String, PoolLoad> {
    models
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let fresh = fresh.get(i).copied().flatten();
            let pool = source.iter().find(|p| p.model == m.name);
            let load = match pool {
                Some(p) => {
                    let own = local.iter().find(|l| l.model == m.name);
                    let counts = crate::pool_counts(p, own, fresh);
                    PoolLoad {
                        slots: if sizing.shared && p.configured_cap > 0 {
                            p.configured_cap
                        } else {
                            p.cap
                        },
                        in_flight: if sizing.shared {
                            counts.cluster_in_flight.unwrap_or(counts.in_flight)
                        } else {
                            counts.in_flight
                        },
                        waiting: counts.queued,
                    }
                }
                None => PoolLoad {
                    slots: replica_share(
                        crate::pool_size(
                            &m.name,
                            m.max_in_flight,
                            sizing.default_cap,
                            sizing.discovered,
                        ),
                        sizing.divisor,
                    ),
                    in_flight: if sizing.shared { fresh.unwrap_or(0) } else { 0 },
                    waiting: 0,
                },
            };
            let slots = if m.has_servers { load.slots } else { 0 };
            (m.name.clone(), PoolLoad { slots, ..load })
        })
        .collect()
}

/// Each scheduler key's requests in flight, by pool. Keys with nothing in
/// flight are left out.
fn key_in_flight(source: &[ModelPoolFairshare]) -> HashMap<Uuid, HashMap<String, usize>> {
    let mut out: HashMap<Uuid, HashMap<String, usize>> = HashMap::new();
    for pool in source {
        for key in pool.keys.iter().filter(|k| k.in_flight > 0) {
            *out.entry(key.key_id)
                .or_default()
                .entry(pool.model.clone())
                .or_insert(0) += key.in_flight;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use obleth_fairshare::{KeyFairshare, ReplicaGroupView, ReplicaTenantView};

    use super::*;

    fn pool(model: &str, cap: usize, in_flight: usize, queued: usize) -> ModelPoolFairshare {
        ModelPoolFairshare {
            model: model.into(),
            cap,
            configured_cap: cap,
            in_flight,
            cluster_in_flight: None,
            queued,
            borrowed: 0,
            held: 0,
            groups: Vec::new(),
            tenants: Vec::new(),
            keys: Vec::new(),
        }
    }

    fn model(name: &str, max_in_flight: Option<usize>) -> LoadModel {
        LoadModel {
            name: name.into(),
            max_in_flight,
            has_servers: true,
        }
    }

    fn sizing(shared: bool, divisor: usize, discovered: &HashMap<String, usize>) -> Sizing<'_> {
        Sizing {
            shared,
            divisor,
            default_cap: 32,
            discovered,
        }
    }

    #[test]
    fn a_shared_pool_reads_its_configured_size_and_the_slot_count() {
        // The answering replica enforces its pool in full in shared mode; the
        // slot count read just now is every replica's.
        let source = [pool("big", 40, 12, 3)];
        let none = HashMap::new();
        let loads = pool_loads(
            &source,
            &source,
            &[Some(34)],
            &sizing(true, 1, &none),
            &[model("big", Some(40))],
        );
        assert_eq!(
            loads["big"],
            PoolLoad {
                slots: 40,
                in_flight: 34,
                waiting: 3
            }
        );

        // Without a fresh read, the scope's own count stands in.
        let loads = pool_loads(
            &source,
            &source,
            &[None],
            &sizing(true, 1, &none),
            &[model("big", Some(40))],
        );
        assert_eq!(loads["big"].in_flight, 12);
    }

    #[test]
    fn a_split_pool_reads_the_replicas_share_and_its_own_count() {
        let mut p = pool("big", 20, 7, 0);
        p.configured_cap = 40;
        let source = [p];
        let none = HashMap::new();
        let loads = pool_loads(
            &source,
            &source,
            &[None],
            &sizing(false, 2, &none),
            &[model("big", Some(40))],
        );
        assert_eq!(
            loads["big"],
            PoolLoad {
                slots: 20,
                in_flight: 7,
                waiting: 0
            }
        );
    }

    #[test]
    fn a_model_without_a_pool_has_its_configured_size() {
        let discovered = HashMap::from([("found".to_string(), 96)]);
        let models = [
            model("found", Some(8)),
            model("fixed", Some(8)),
            model("default", None),
            model("zero", Some(0)),
        ];
        let loads = pool_loads(&[], &[], &[], &sizing(false, 2, &discovered), &models);
        // Discovery wins over max_in_flight, and the split applies.
        assert_eq!(loads["found"].slots, 48);
        assert_eq!(loads["fixed"].slots, 4);
        assert_eq!(loads["default"].slots, 16);
        assert_eq!(loads["zero"].slots, 16);
        assert!(loads.values().all(|l| l.in_flight == 0 && l.waiting == 0));

        // In shared mode another replica's requests still count.
        let loads = pool_loads(
            &[],
            &[],
            &[Some(5)],
            &sizing(true, 1, &discovered),
            &[model("found", None)],
        );
        assert_eq!(
            loads["found"],
            PoolLoad {
                slots: 96,
                in_flight: 5,
                waiting: 0
            }
        );
    }

    #[test]
    fn a_model_without_servers_has_no_slots_but_keeps_its_counts() {
        let source = [pool("down", 32, 0, 4)];
        let mut down = model("down", None);
        down.has_servers = false;
        let none = HashMap::new();
        let loads = pool_loads(&source, &source, &[None], &sizing(false, 1, &none), &[down]);
        assert_eq!(
            loads["down"],
            PoolLoad {
                slots: 0,
                in_flight: 0,
                waiting: 4
            }
        );
    }

    #[test]
    fn the_health_prober_is_left_out_of_the_counts() {
        let mut p = pool("big", 10, 3, 1);
        p.groups.push(obleth_fairshare::GroupFairshare {
            name: crate::model_health::HEALTH_GROUP.into(),
            weight: 100,
            in_flight: 1,
            queued: 1,
            slot_cap: 10,
            borrowed: 0,
            served_tokens: 0.0,
            share_score: 0.0,
            weight_share: 0.0,
        });
        let source = [p];
        let none = HashMap::new();
        let loads = pool_loads(
            &source,
            &source,
            &[None],
            &sizing(false, 1, &none),
            &[model("big", None)],
        );
        assert_eq!((loads["big"].in_flight, loads["big"].waiting), (2, 0));
    }

    #[test]
    fn a_merged_pool_counts_every_replica() {
        // Two replicas' published views of one pool, merged as the cluster
        // view merges them: the waiting count is the fleet's.
        let (tenant, key) = (Uuid::new_v4(), Uuid::new_v4());
        let view = |in_flight: usize, queued: usize| obleth_fairshare::ReplicaView {
            version: obleth_fairshare::view::VIEW_VERSION,
            pools: vec![obleth_fairshare::ReplicaPoolView {
                pool: PoolKey::Model("big".into()).slot_id(),
                configured_cap: 8,
                in_flight,
                queued,
                groups: vec![ReplicaGroupView {
                    name: "default".into(),
                    weight: 100,
                    in_flight,
                    queued,
                    served: 0.0,
                }],
                tenants: vec![ReplicaTenantView {
                    id: tenant,
                    group: "default".into(),
                    weight: 100,
                    cap: None,
                    in_flight,
                    queued,
                    served: 0.0,
                }],
                keys: vec![obleth_fairshare::ReplicaKeyView {
                    id: key,
                    tenant,
                    weight: 100,
                    cap: None,
                    in_flight,
                    queued,
                    served: 0.0,
                    end_user: None,
                }],
                ..Default::default()
            }],
        };
        let views = [view(5, 2), view(3, 1)];
        let algorithm = obleth_config::FairshareAlgorithm::Hierarchical;
        let merged =
            obleth_fairshare::merge_views(algorithm, &views.iter().collect::<Vec<_>>(), 4096);
        let local = obleth_fairshare::merge_views(algorithm, &[&views[0]], 4096);
        let none = HashMap::new();
        let loads = pool_loads(
            &merged,
            &local,
            &[Some(8)],
            &sizing(true, 1, &none),
            &[model("big", None)],
        );
        assert_eq!(
            loads["big"],
            PoolLoad {
                slots: 8,
                in_flight: 8,
                waiting: 3
            }
        );
        let keys = key_in_flight(&merged);
        assert_eq!(keys[&key]["big"], 8);
    }

    #[test]
    fn a_keys_requests_add_up_across_pools() {
        let key = Uuid::new_v4();
        let other = Uuid::new_v4();
        let entry = |key_id: Uuid, in_flight: usize| KeyFairshare {
            key_id,
            tenant_id: Uuid::nil(),
            weight: 100,
            max_in_flight: None,
            in_flight,
            queued: 0,
            served_tokens: 0.0,
            share_score: 0.0,
            weight_share: 0.0,
            end_user: None,
        };
        let mut a = pool("a", 8, 3, 0);
        a.keys = vec![entry(key, 2), entry(other, 1)];
        let mut b = pool("b", 8, 1, 0);
        b.keys = vec![entry(key, 1), entry(other, 0)];
        let load = LiveLoad {
            scope: "cluster",
            divisor: 1,
            pools: HashMap::new(),
            keys: key_in_flight(&[a, b]),
        };
        assert_eq!(load.key_in_flight(key, None), 3);
        assert_eq!(load.key_in_flight(key, Some("a")), 2);
        assert_eq!(load.key_in_flight(key, Some("c")), 0);
        assert_eq!(load.key_in_flight(Uuid::new_v4(), None), 0);
        assert!(!load.keys[&other].contains_key("b"));
    }

    /// An enabled route served at a fixed base.
    fn route(name: &str, max_in_flight: Option<usize>) -> ResolvedModel {
        serde_json::from_value(serde_json::json!({
            "model_name": name,
            "upstream_model": name,
            "api_base": "http://backend:8000/v1",
            "api_key": null,
            "admission_weight": 100,
            "max_in_flight": max_in_flight,
            "enabled": true,
            "cache_enabled": false,
            "cache_ttl_secs": 0,
        }))
        .expect("route")
    }

    #[tokio::test]
    async fn a_reading_follows_the_schedulers_pools() {
        let fairshare = FairShare::start(
            std::sync::Arc::new(obleth_fairshare::StaticCapacity::new(64)),
            obleth_config::FairshareAlgorithm::default(),
            32,
        );
        let (tenant, mine, theirs) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let req = |key| {
            obleth_fairshare::AdmitRequest::new(tenant, "busy", 1)
                .key(key, 100)
                .model_cap(4)
        };
        let mut held = Vec::new();
        for key in [mine, mine, theirs, theirs] {
            held.push(fairshare.admit(req(key)).await.expect("admitted"));
        }
        // A fifth request waits for one of the four slots.
        let waiter = {
            let fairshare = fairshare.clone();
            let req = req(theirs);
            tokio::spawn(async move { fairshare.admit(req).await })
        };
        for _ in 0..200 {
            if fairshare
                .snapshot()
                .await
                .is_some_and(|s| s.global_queued == 1)
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }

        let busy = route("busy", Some(4));
        let quiet = route("quiet", Some(6));
        let mut down = route("down", None);
        down.api_base.clear();
        let settings = LoadSettings {
            replica_aware: true,
            replicas_known: true,
            default_cap: 32,
        };
        let discovery = CapacityDiscovery::new(Default::default());
        let load = live_load(&fairshare, &settings, &discovery, [&busy, &quiet, &down])
            .await
            .expect("a reading");
        // The only live replica counts every request there is.
        assert_eq!((load.scope, load.divisor), ("cluster", 1));
        assert_eq!(
            load.pools["busy"],
            PoolLoad {
                slots: 4,
                in_flight: 4,
                waiting: 1
            }
        );
        assert_eq!(
            load.pools["quiet"],
            PoolLoad {
                slots: 6,
                in_flight: 0,
                waiting: 0
            }
        );
        assert_eq!(load.pools["down"].slots, 0);
        assert_eq!(load.key_in_flight(mine, None), 2);
        assert_eq!(load.key_in_flight(theirs, Some("busy")), 2);
        assert_eq!(load.key_in_flight(mine, Some("quiet")), 0);

        // Without a heartbeat, a replica cannot tell it is the only one.
        let unknown = LoadSettings {
            replicas_known: false,
            ..settings
        };
        let load = live_load(&fairshare, &unknown, &discovery, [&busy])
            .await
            .expect("a reading");
        assert_eq!(load.scope, "replica");

        drop(held);
        let admitted = waiter.await.expect("joined");
        assert!(admitted.is_some(), "the waiter runs once a slot frees");
    }

    #[test]
    fn a_route_has_servers_through_its_base_or_an_enabled_endpoint() {
        let mut route = route("m", Some(12));
        let none = HashSet::new();
        assert_eq!(
            LoadModel::of(&route, &none),
            LoadModel {
                name: "m".into(),
                max_in_flight: Some(12),
                has_servers: true
            }
        );
        assert!(!LoadModel::of(&route, &HashSet::from(["m".to_string()])).has_servers);
        route.api_base = String::new();
        assert!(!LoadModel::of(&route, &none).has_servers);
        route.endpoints = vec![obleth_config::ResolvedEndpoint {
            id: "e".into(),
            api_base: "http://backend:8000/v1".into(),
            api_key: None,
            priority: 0,
            weight: 1,
            enabled: true,
            healthy: true,
            max_in_flight: None,
        }];
        assert!(LoadModel::of(&route, &none).has_servers);
        route.enabled = false;
        assert!(!LoadModel::of(&route, &none).has_servers);
    }
}
