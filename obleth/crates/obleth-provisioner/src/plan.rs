use crate::domain::*;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// Smallest free disjoint window base for a new replica: serving_port + i*span
/// for the smallest i whose base is not already taken by a live replica.
pub fn next_free_window_base(serving_port: i64, span: i64, live_port_bases: &[i64]) -> i64 {
    let span = span.max(1);
    let mut i = 0i64;
    loop {
        let base = serving_port + i * span;
        if !live_port_bases.contains(&base) {
            return base;
        }
        i += 1;
    }
}

/// Pure reconcile. `jobs` is keyed by slurm_job_id; `health` is keyed by
/// replica id and carries the discovered port for `starting`/`pending` replicas.
/// `restart` holds replica ids the self-heal loop wants restarted (healthy rows
/// whose job still reports RUNNING but which keep failing health probes).
pub fn plan(
    spec: &ManagedSpecView,
    replicas: &[ReplicaView],
    jobs: &HashMap<String, JobInfo>,
    health: &HashMap<Uuid, u16>,
    restart: &HashSet<Uuid>,
    lost_retention_secs: i64,
) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut alive = 0i64;
    // Self-heal restarts are capped at one per model per tick: if the
    // provisioner itself loses the network path to the nodes, every probe fails
    // at once, and an uncapped restart would mass-cancel a whole fleet of
    // actually-fine replicas. Rolling one at a time keeps capacity up and gives
    // the operator (and the held-reconcile alert) time to notice.
    let mut restart_budget = 1usize;
    // Replicas with a live job, eligible to be cancelled if we're over target.
    // (replica_id, job_id, endpoint_id, rank, age) where lower rank = cancel first.
    let mut cancellable: Vec<(Uuid, String, Option<Uuid>, u8, i64)> = Vec::new();

    for r in replicas {
        // GC dead rows past retention; they don't count as alive.
        if r.state == "lost" {
            if r.age_secs >= lost_retention_secs {
                actions.push(Action::Delete { replica_id: r.id });
            }
            continue;
        }
        if r.state == "draining" {
            // Cancelled on purpose. Once the job is actually gone, delete the row
            // directly. Not MarkLost -- "lost" counts toward the failure limit,
            // and a drain is not a failure. While the job is still pending/running
            // the cancel is in flight, so wait for it to terminate.
            if matches!(
                jobs.get(&r.slurm_job_id).map(|j| j.state),
                None | Some(JobState::Gone)
            ) {
                actions.push(Action::Delete { replica_id: r.id });
            }
            continue;
        }

        // Operator-requested restart (cancel_requested) or self-heal restart
        // (in the `restart` set): cancel this replica's job now (regardless of
        // target) so the resubmit-to-target below launches a fresh one. Don't
        // count it as alive. After cancel it becomes "draining" (skipped above)
        // until its job goes Gone and the row is GC'd, which clears the flag.
        // Operator restarts are never budget-limited; self-heal ones are.
        let self_heal = !r.cancel_requested && restart.contains(&r.id) && restart_budget > 0;
        if (r.cancel_requested || self_heal)
            && matches!(
                jobs.get(&r.slurm_job_id).map(|j| j.state),
                Some(JobState::Pending | JobState::Running)
            )
        {
            if self_heal {
                restart_budget -= 1;
            }
            actions.push(Action::Cancel {
                replica_id: r.id,
                job_id: r.slurm_job_id.clone(),
                endpoint_id: r.endpoint_id,
                reason: if r.cancel_requested {
                    CancelReason::OperatorRestart
                } else {
                    CancelReason::ProbeFailed
                },
            });
            continue;
        }

        match jobs.get(&r.slurm_job_id).map(|j| j.state) {
            None | Some(JobState::Gone) => {
                actions.push(Action::MarkLost {
                    replica_id: r.id,
                    endpoint_id: r.endpoint_id,
                });
                // not alive
            }
            Some(JobState::Pending) => {
                alive += 1;
                cancellable.push((r.id, r.slurm_job_id.clone(), r.endpoint_id, 0, r.age_secs));
                // cancel pending first
            }
            Some(JobState::Running) => {
                alive += 1;
                // Promote on a passing health check. This covers fresh replicas
                // ("pending" replicas haven't been probed yet — they were waiting
                // for the job to leave Slurm-PENDING — so treat them like
                // "starting") AND self-heals a replica stranded as "healthy" with
                // no endpoint linked (e.g. a prior promote whose endpoint write
                // failed, or one whose endpoint was removed out of band — see
                // `clear_dangling_endpoints`, which nulls that reference before
                // planning): the planner would otherwise never re-promote a
                // "healthy" row, leaving the model permanently short an endpoint.
                let needs_promote = r.state == "starting"
                    || r.state == "pending"
                    || (r.state == "healthy" && r.endpoint_id.is_none());
                if needs_promote {
                    if let Some(&port) = health.get(&r.id) {
                        let node = jobs
                            .get(&r.slurm_job_id)
                            .and_then(|j| j.nodes.first().cloned())
                            .unwrap_or_default();
                        actions.push(Action::Promote {
                            replica_id: r.id,
                            api_base: endpoint_api_base(&node, port as i64),
                        });
                    }
                    cancellable.push((r.id, r.slurm_job_id.clone(), r.endpoint_id, 1, r.age_secs));
                } else {
                    cancellable.push((r.id, r.slurm_job_id.clone(), r.endpoint_id, 2, r.age_secs));
                    // healthy: cancel last
                }
            }
        }
    }

    let target = spec.target_replicas.max(0);
    let lost_count = replicas.iter().filter(|r| r.state == "lost").count() as i64;
    let failure_limit_hit = spec.max_job_failures > 0 && lost_count >= spec.max_job_failures;
    if alive < target && !failure_limit_hit {
        for _ in 0..(target - alive) {
            actions.push(Action::Submit);
        }
    } else if alive > target {
        // cancel excess: pending first, then starting, then healthy; oldest first within a rank.
        cancellable.sort_by(|a, b| a.3.cmp(&b.3).then(b.4.cmp(&a.4)));
        for (id, job_id, endpoint_id, _, _) in
            cancellable.into_iter().take((alive - target) as usize)
        {
            actions.push(Action::Cancel {
                replica_id: id,
                job_id,
                endpoint_id,
                reason: CancelReason::ScaleDown,
            });
        }
    }

    actions
}

/// Build the OpenAI-compatible `api_base` for a promoted replica's endpoint.
///
/// The rest of the gateway treats `api_base` as the OpenAI **root** (every
/// statically-registered model uses `https://host/v1`), and the model health
/// check appends `/models` to it expecting `…/v1/models`. Inference servers
/// (vLLM, Ollama, SGLang, LiteLLM) all serve their OpenAI surface under `/v1`,
/// so we register the node endpoint at that root. The data-plane URL builder
/// dedups a leading `/v1` in the request path, so this stays correct for proxy
/// traffic too. (The provisioner's *own* liveness probe uses the spec's
/// `health_path` against the bare node — that is a separate, native check.)
fn endpoint_api_base(node: &str, serving_port: i64) -> String {
    format!("http://{node}:{serving_port}/v1")
}

/// Null out a `healthy` replica's `endpoint_id` when it points at an endpoint
/// the gateway no longer has registered.
///
/// `model_replicas` has no foreign key to `model_endpoints` (it was dropped), so
/// when an endpoint is removed out of band — a manual "Disable"/delete in the
/// dashboard's Reliability tab, or a cancel/mark-lost whose endpoint delete
/// landed but whose replica patch didn't — the replica keeps a *dangling*
/// reference. The planner's re-promote path fires only on `endpoint_id.is_none()`,
/// so such a row would sit "healthy" forever, serving a phantom the model no
/// longer has (the classic "2 healthy replicas, 1 endpoint" split). Nulling the
/// dangling id here lets that path relink a fresh endpoint next.
///
/// `known_endpoints` MUST be a reliable snapshot from a successful fetch this
/// tick — never call this with an empty set derived from a failed lookup, or a
/// transient error would strip every replica's endpoint at once.
pub fn clear_dangling_endpoints(replicas: &mut [ReplicaView], known_endpoints: &HashSet<Uuid>) {
    for r in replicas.iter_mut() {
        if r.state == "healthy" && r.endpoint_id.is_some_and(|ep| !known_endpoints.contains(&ep)) {
            r.endpoint_id = None;
        }
    }
}

/// Advance the per-replica consecutive-probe-failure counters after one tick's
/// probes, for self-heal. Only `healthy` replicas whose job reports RUNNING
/// with a known node are judged: those are the rows the tick actually probed,
/// so a missing `health` entry means the probe failed. Everything else (booting
/// replicas, jobs without nodes, jobs the planner is already reconciling away)
/// is left untouched — probe failures there are expected, not evidence of a
/// zombie.
///
/// A miss **increments** the counter; a pass **decays** it by one (saturating at
/// zero, dropping the entry when it reaches zero) rather than resetting it to
/// zero outright. This is the crux of not killing a *flapping* replica: a
/// single-threaded server (e.g. llama.cpp) that is briefly busy will miss the
/// occasional probe and pass the next, oscillating healthy ↔ not-yet-healthy on
/// the tick beat. A hard reset let those flaps keep re-arming from zero so a
/// perfectly-fine replica eventually lost the coin flip and got cancelled. With
/// decay the counter tracks *sustained net* failure: a truly-dead server climbs
/// steadily toward the restart threshold, while a flapping-but-serving one hovers
/// near zero and never trips it.
pub fn update_probe_failures(
    replicas: &[ReplicaView],
    jobs: &HashMap<String, JobInfo>,
    health: &HashMap<Uuid, u16>,
    counts: &mut HashMap<Uuid, i64>,
) {
    for r in replicas {
        if r.state != "healthy" {
            continue;
        }
        let Some(j) = jobs.get(&r.slurm_job_id) else {
            continue;
        };
        if j.state != JobState::Running || j.nodes.is_empty() {
            continue;
        }
        if health.contains_key(&r.id) {
            // Passing probe: decay one step toward zero instead of resetting, so
            // an intermittent flap can't keep re-arming the counter from scratch.
            match counts.get_mut(&r.id) {
                Some(n) if *n > 1 => *n -= 1,
                Some(_) => {
                    counts.remove(&r.id);
                }
                None => {}
            }
        } else {
            *counts.entry(r.id).or_insert(0) += 1;
        }
    }
}

/// Consecutive failed *gateway* checks of a replica's endpoint that count as a
/// restart signal. The gateway's check is a real 1-token inference at a slow
/// cadence (minutes), so two failures is already a sustained outage — and it
/// catches zombies the provisioner's own GET probe cannot (a server that
/// answers metadata instantly but hangs forever on inference).
const GATEWAY_UNHEALTHY_CHECKS: i64 = 2;

/// Ignore the gateway's endpoint verdict when it is older than this — e.g.
/// when scheduled model health checks are disabled, a stale `unhealthy` row
/// must not keep restarting a replica that recovered long ago.
const GATEWAY_CHECK_MAX_AGE_SECS: i64 = 3600;

/// Which healthy replicas self-heal should restart this tick. Two independent
/// signals, either suffices:
///
/// 1. the provisioner's own port-window probe has been failing for
///    `probe_threshold` net ticks (`probe_failures`, maintained by
///    `update_probe_failures`, which decays on a pass) — the server is not
///    answering at all, sustained rather than a transient flap;
/// 2. the gateway's health check of the replica's registered endpoint is
///    `unhealthy` with at least `GATEWAY_UNHEALTHY_CHECKS` consecutive
///    failures and a recent check — the server answers GETs but fails real
///    inference (zombie).
///
/// `probe_threshold <= 0` disables self-heal entirely (both signals). The
/// planner additionally caps restarts at one per model per tick.
pub fn restart_candidates(
    replicas: &[ReplicaView],
    probe_failures: &HashMap<Uuid, i64>,
    probe_threshold: i64,
    endpoints: &[EndpointView],
) -> HashSet<Uuid> {
    if probe_threshold <= 0 {
        return HashSet::new();
    }
    let unhealthy_eps: HashSet<Uuid> = endpoints
        .iter()
        .filter(|e| {
            e.health_status.as_deref() == Some("unhealthy")
                && e.consecutive_failures >= GATEWAY_UNHEALTHY_CHECKS
                && e.checked_secs_ago
                    .is_some_and(|s| s <= GATEWAY_CHECK_MAX_AGE_SECS)
        })
        .map(|e| e.id)
        .collect();
    replicas
        .iter()
        .filter(|r| {
            r.state == "healthy"
                && (probe_failures.get(&r.id).copied().unwrap_or(0) >= probe_threshold
                    || r.endpoint_id.is_some_and(|ep| unhealthy_eps.contains(&ep)))
        })
        .map(|r| r.id)
        .collect()
}

/// The slice of ManagedModelSpec the planner needs (keeps it decoupled from the
/// full DB struct in tests).
pub struct ManagedSpecView {
    pub target_replicas: i64,
    /// Stop submitting new jobs when this many lost replicas are visible (0 = no limit).
    pub max_job_failures: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rv(state: &str, job: &str, ep: Option<Uuid>, age: i64) -> ReplicaView {
        ReplicaView {
            id: Uuid::new_v4(),
            model_id: Uuid::new_v4(),
            slurm_job_id: job.into(),
            state: state.into(),
            endpoint_id: ep,
            age_secs: age,
            port_base: 0,
            last_message: None,
            cancel_requested: false,
        }
    }
    fn job(id: &str, st: JobState, nodes: &[&str]) -> (String, JobInfo) {
        (
            id.into(),
            JobInfo {
                job_id: id.into(),
                state: st,
                nodes: nodes.iter().map(|s| s.to_string()).collect(),
                raw_state: String::new(),
                reason: None,
            },
        )
    }
    fn spec(target: i64) -> ManagedSpecView {
        ManagedSpecView {
            target_replicas: target,
            max_job_failures: 0,
        }
    }
    fn spec_with_limit(target: i64, limit: i64) -> ManagedSpecView {
        ManagedSpecView {
            target_replicas: target,
            max_job_failures: limit,
        }
    }

    #[test]
    fn next_free_window_base_empty_returns_serving_port() {
        assert_eq!(next_free_window_base(8000, 8, &[]), 8000);
    }

    #[test]
    fn next_free_window_base_skips_taken_slots() {
        // [8000] taken → next is 8008
        assert_eq!(next_free_window_base(8000, 8, &[8000]), 8008);
        // [8000, 8016] taken → 8008 is free (first gap)
        assert_eq!(next_free_window_base(8000, 8, &[8000, 8016]), 8008);
    }

    #[test]
    fn empty_state_submits_target() {
        let actions = plan(
            &spec(2),
            &[],
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            900,
        );
        assert_eq!(actions.iter().filter(|a| **a == Action::Submit).count(), 2);
    }

    #[test]
    fn healthy_at_target_does_nothing() {
        let r1 = rv("healthy", "j1", Some(Uuid::new_v4()), 100);
        let r2 = rv("healthy", "j2", Some(Uuid::new_v4()), 100);
        let jobs = HashMap::from([
            job("j1", JobState::Running, &["n1"]),
            job("j2", JobState::Running, &["n2"]),
        ]);
        let actions = plan(
            &spec(2),
            &[r1, r2],
            &jobs,
            &HashMap::new(),
            &HashSet::new(),
            900,
        );
        assert!(actions.is_empty(), "got {actions:?}");
    }

    #[test]
    fn starting_and_running_and_healthy_promotes_with_url() {
        let r = rv("starting", "j1", None, 30);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["gpu7"])]);
        let health = HashMap::from([(id, 8000u16)]);
        let actions = plan(&spec(1), &[r], &jobs, &health, &HashSet::new(), 900);
        assert_eq!(
            actions,
            vec![Action::Promote {
                replica_id: id,
                api_base: "http://gpu7:8000/v1".into()
            }]
        );
    }

    #[test]
    fn healthy_without_endpoint_is_repromoted() {
        // Stranded "healthy" with no endpoint linked (a prior promote whose
        // endpoint write failed) must be re-promoted so it relinks an endpoint.
        let r = rv("healthy", "j1", None, 300);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["gpu7"])]);
        let health = HashMap::from([(id, 8000u16)]);
        let actions = plan(&spec(1), &[r], &jobs, &health, &HashSet::new(), 900);
        assert_eq!(
            actions,
            vec![Action::Promote {
                replica_id: id,
                api_base: "http://gpu7:8000/v1".into()
            }]
        );
    }

    #[test]
    fn healthy_with_endpoint_is_not_repromoted() {
        // A properly-linked healthy replica must NOT be re-promoted, even if a
        // stale health entry exists for it.
        let r = rv("healthy", "j1", Some(Uuid::new_v4()), 300);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["gpu7"])]);
        let health = HashMap::from([(id, 8000u16)]);
        let actions = plan(&spec(1), &[r], &jobs, &health, &HashSet::new(), 900);
        assert!(actions.is_empty(), "got {actions:?}");
    }

    #[test]
    fn clear_dangling_endpoints_nulls_only_missing_healthy_refs() {
        let live_ep = Uuid::new_v4();
        let dead_ep = Uuid::new_v4();
        let mut replicas = vec![
            rv("healthy", "j1", Some(live_ep), 100), // still registered -> kept
            rv("healthy", "j2", Some(dead_ep), 100), // removed out of band -> nulled
            rv("healthy", "j3", None, 100),          // already none -> unchanged
            rv("starting", "j4", Some(dead_ep), 100), // not healthy -> left alone
        ];
        clear_dangling_endpoints(&mut replicas, &HashSet::from([live_ep]));
        assert_eq!(replicas[0].endpoint_id, Some(live_ep));
        assert_eq!(replicas[1].endpoint_id, None);
        assert_eq!(replicas[2].endpoint_id, None);
        assert_eq!(replicas[3].endpoint_id, Some(dead_ep));
    }

    #[test]
    fn dangling_endpoint_is_repromoted_after_clear() {
        // End-to-end for the "2 healthy replicas, 1 endpoint" split: a healthy
        // replica whose endpoint was removed out of band is nulled by
        // clear_dangling_endpoints, then re-promoted by the planner so it relinks
        // a fresh endpoint instead of serving a phantom forever.
        let dead_ep = Uuid::new_v4();
        let mut replicas = vec![rv("healthy", "j1", Some(dead_ep), 300)];
        let id = replicas[0].id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["gpu7"])]);
        let health = HashMap::from([(id, 8000u16)]);
        // Gateway no longer has that endpoint registered.
        clear_dangling_endpoints(&mut replicas, &HashSet::new());
        let actions = plan(&spec(1), &replicas, &jobs, &health, &HashSet::new(), 900);
        assert_eq!(
            actions,
            vec![Action::Promote {
                replica_id: id,
                api_base: "http://gpu7:8000/v1".into()
            }]
        );
    }

    #[test]
    fn starting_but_unhealthy_does_not_promote_and_does_not_oversubmit() {
        let r = rv("starting", "j1", None, 30);
        let jobs = HashMap::from([job("j1", JobState::Running, &["gpu7"])]);
        let health: HashMap<Uuid, u16> = HashMap::new();
        let actions = plan(&spec(1), &[r], &jobs, &health, &HashSet::new(), 900);
        // still "alive" (counts toward target), so no submit, no promote.
        assert!(actions.is_empty(), "got {actions:?}");
    }

    #[test]
    fn preempted_job_marks_lost_and_resubmits() {
        let ep = Uuid::new_v4();
        let r = rv("healthy", "j1", Some(ep), 500);
        let id = r.id;
        // job gone from slurm entirely
        let actions = plan(
            &spec(1),
            &[r],
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            900,
        );
        assert!(actions.contains(&Action::MarkLost {
            replica_id: id,
            endpoint_id: Some(ep)
        }));
        assert!(actions.contains(&Action::Submit));
    }

    #[test]
    fn excess_replicas_are_cancelled_pending_first() {
        let healthy = rv("healthy", "j1", Some(Uuid::new_v4()), 1000);
        let pending = rv("pending", "j2", None, 10);
        let pid = pending.id;
        let jobs = HashMap::from([
            job("j1", JobState::Running, &["n1"]),
            job("j2", JobState::Pending, &[""]),
        ]);
        let actions = plan(
            &spec(1),
            &[healthy, pending],
            &jobs,
            &HashMap::new(),
            &HashSet::new(),
            900,
        );
        assert_eq!(
            actions,
            vec![Action::Cancel {
                replica_id: pid,
                job_id: "j2".into(),
                endpoint_id: None,
                reason: CancelReason::ScaleDown
            }]
        );
    }

    #[test]
    fn cancel_requested_replica_is_cancelled_and_replaced() {
        let ep = Uuid::new_v4();
        let mut r = rv("healthy", "j1", Some(ep), 100);
        r.cancel_requested = true;
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["n1"])]);
        let actions = plan(&spec(1), &[r], &jobs, &HashMap::new(), &HashSet::new(), 900);
        // Cancelled (with its endpoint) regardless of target, and a fresh replica
        // submitted to refill — i.e. a restart.
        assert!(actions.contains(&Action::Cancel {
            replica_id: id,
            job_id: "j1".into(),
            endpoint_id: Some(ep),
            reason: CancelReason::OperatorRestart
        }));
        assert!(actions.contains(&Action::Submit));
    }

    #[test]
    fn cancel_carries_the_replicas_endpoint_id() {
        let ep = Uuid::new_v4();
        let h1 = rv("healthy", "j1", Some(ep), 1000); // oldest -> cancelled first within rank
        let h1id = h1.id;
        let h2 = rv("healthy", "j2", Some(Uuid::new_v4()), 500);
        let jobs = HashMap::from([
            job("j1", JobState::Running, &["n1"]),
            job("j2", JobState::Running, &["n2"]),
        ]);
        let actions = plan(
            &spec(1),
            &[h1, h2],
            &jobs,
            &HashMap::new(),
            &HashSet::new(),
            900,
        );
        assert_eq!(
            actions,
            vec![Action::Cancel {
                replica_id: h1id,
                job_id: "j1".into(),
                endpoint_id: Some(ep),
                reason: CancelReason::ScaleDown
            }]
        );
    }

    #[test]
    fn pending_replica_with_running_job_is_promoted_when_healthy() {
        // Replicas are created in "pending" state. Once the Slurm job transitions
        // to Running they should be promoted directly (no separate MarkStarting
        // step), just like a "starting" replica would be.
        let r = rv("pending", "j1", None, 30);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["gpu7"])]);
        let health = HashMap::from([(id, 8000u16)]);
        let actions = plan(&spec(1), &[r], &jobs, &health, &HashSet::new(), 900);
        assert_eq!(
            actions,
            vec![Action::Promote {
                replica_id: id,
                api_base: "http://gpu7:8000/v1".into()
            }]
        );
    }

    #[test]
    fn pending_replica_with_running_job_and_unhealthy_does_not_promote() {
        let r = rv("pending", "j1", None, 30);
        let jobs = HashMap::from([job("j1", JobState::Running, &["gpu7"])]);
        let health: HashMap<Uuid, u16> = HashMap::new();
        let actions = plan(&spec(1), &[r], &jobs, &health, &HashSet::new(), 900);
        assert!(actions.is_empty(), "got {actions:?}");
    }

    #[test]
    fn old_lost_rows_are_gced() {
        let r = rv("lost", "j1", None, 5000);
        let id = r.id;
        let actions = plan(
            &spec(0),
            &[r],
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            900,
        );
        assert!(actions.contains(&Action::Delete { replica_id: id }));
    }

    #[test]
    fn failure_limit_suppresses_submit_when_reached() {
        // 3 lost replicas, limit = 3 → no new Submit
        let lost: Vec<ReplicaView> = (0..3)
            .map(|i| rv("lost", &format!("j{i}"), None, 60))
            .collect();
        let actions = plan(
            &spec_with_limit(2, 3),
            &lost,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            900,
        );
        assert!(
            !actions.contains(&Action::Submit),
            "submit must be suppressed at limit"
        );
    }

    #[test]
    fn failure_limit_allows_submit_below_threshold() {
        // 2 lost replicas, limit = 5 → still submits
        let lost: Vec<ReplicaView> = (0..2)
            .map(|i| rv("lost", &format!("j{i}"), None, 60))
            .collect();
        let actions = plan(
            &spec_with_limit(2, 5),
            &lost,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            900,
        );
        assert!(actions.contains(&Action::Submit));
    }

    #[test]
    fn zero_limit_means_no_cap() {
        // 100 lost replicas, limit = 0 → unlimited, still submits
        let lost: Vec<ReplicaView> = (0..100)
            .map(|i| rv("lost", &format!("j{i}"), None, 60))
            .collect();
        let actions = plan(
            &spec(2),
            &lost,
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            900,
        );
        assert!(actions.contains(&Action::Submit));
    }

    #[test]
    fn draining_replica_with_gone_job_is_deleted() {
        // Job already terminated (Gone) -> the stuck draining row must be deleted.
        let r = rv("draining", "j1", None, 120);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Gone, &[])]);
        let actions = plan(&spec(1), &[r], &jobs, &HashMap::new(), &HashSet::new(), 900);
        assert!(
            actions.contains(&Action::Delete { replica_id: id }),
            "got {actions:?}"
        );
    }

    #[test]
    fn draining_replica_with_absent_job_is_deleted() {
        // Job purged from Slurm entirely (absent from the map) -> delete the row.
        let r = rv("draining", "j1", None, 120);
        let id = r.id;
        let actions = plan(
            &spec(1),
            &[r],
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            900,
        );
        assert!(
            actions.contains(&Action::Delete { replica_id: id }),
            "got {actions:?}"
        );
    }

    #[test]
    fn draining_replica_with_running_job_is_left_alone() {
        // Cancel still in flight (job not yet gone) -> no action for this replica.
        let r = rv("draining", "j1", None, 10);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["n1"])]);
        // spec target 0 so the only possible action would concern this replica.
        let actions = plan(&spec(0), &[r], &jobs, &HashMap::new(), &HashSet::new(), 900);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::Delete { replica_id } if *replica_id == id)),
            "draining replica with a live job must be left alone; got {actions:?}"
        );
    }

    #[test]
    fn zombie_replica_in_restart_set_is_cancelled_and_replaced() {
        // Slurm says RUNNING but the replica keeps failing probes (self-heal
        // decided it's a zombie): cancel it — with its endpoint, so it leaves
        // rotation immediately — and submit a fresh replacement this tick.
        let ep = Uuid::new_v4();
        let r = rv("healthy", "j1", Some(ep), 600);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["n1"])]);
        let restart = HashSet::from([id]);
        let actions = plan(&spec(1), &[r], &jobs, &HashMap::new(), &restart, 900);
        assert!(actions.contains(&Action::Cancel {
            replica_id: id,
            job_id: "j1".into(),
            endpoint_id: Some(ep),
            reason: CancelReason::ProbeFailed
        }));
        assert!(actions.contains(&Action::Submit));
    }

    #[test]
    fn self_heal_restarts_are_capped_at_one_per_tick() {
        // Two zombies at once (e.g. the provisioner lost its network path to the
        // nodes): only ONE may be restarted per tick, so a false positive rolls
        // the fleet gradually instead of mass-cancelling it.
        let r1 = rv("healthy", "j1", Some(Uuid::new_v4()), 600);
        let r2 = rv("healthy", "j2", Some(Uuid::new_v4()), 500);
        let restart = HashSet::from([r1.id, r2.id]);
        let jobs = HashMap::from([
            job("j1", JobState::Running, &["n1"]),
            job("j2", JobState::Running, &["n2"]),
        ]);
        let actions = plan(&spec(2), &[r1, r2], &jobs, &HashMap::new(), &restart, 900);
        let cancels = actions
            .iter()
            .filter(|a| matches!(a, Action::Cancel { .. }))
            .count();
        let submits = actions.iter().filter(|a| **a == Action::Submit).count();
        assert_eq!(
            cancels, 1,
            "exactly one self-heal restart per tick: {actions:?}"
        );
        assert_eq!(
            submits, 1,
            "one replacement for the one cancel: {actions:?}"
        );
    }

    #[test]
    fn restart_set_is_ignored_when_job_already_gone() {
        // The job died between the probe and the plan: MarkLost wins (normal
        // lost/resubmit path), no Cancel is sent for a job that no longer exists.
        let ep = Uuid::new_v4();
        let r = rv("healthy", "j1", Some(ep), 600);
        let id = r.id;
        let restart = HashSet::from([id]);
        let actions = plan(
            &spec(1),
            &[r],
            &HashMap::new(),
            &HashMap::new(),
            &restart,
            900,
        );
        assert!(actions.contains(&Action::MarkLost {
            replica_id: id,
            endpoint_id: Some(ep)
        }));
        assert!(
            !actions.iter().any(|a| matches!(a, Action::Cancel { .. })),
            "no cancel for a gone job: {actions:?}"
        );
    }

    #[test]
    fn operator_restart_is_not_consumed_by_the_self_heal_budget() {
        // An operator cancel_requested and a self-heal restart in the same tick:
        // both fire — the budget only limits self-heal cancels.
        let mut r1 = rv("healthy", "j1", Some(Uuid::new_v4()), 600);
        r1.cancel_requested = true;
        let r2 = rv("healthy", "j2", Some(Uuid::new_v4()), 500);
        let restart = HashSet::from([r1.id, r2.id]);
        let jobs = HashMap::from([
            job("j1", JobState::Running, &["n1"]),
            job("j2", JobState::Running, &["n2"]),
        ]);
        let actions = plan(&spec(2), &[r1, r2], &jobs, &HashMap::new(), &restart, 900);
        let cancels = actions
            .iter()
            .filter(|a| matches!(a, Action::Cancel { .. }))
            .count();
        assert_eq!(cancels, 2, "operator + one self-heal: {actions:?}");
    }

    // --- restart_candidates (self-heal restart signals) ---

    fn ep(id: Uuid, status: &str, failures: i64, checked_secs_ago: Option<i64>) -> EndpointView {
        EndpointView {
            id,
            name: "ep".into(),
            api_base: "http://node:8000/v1".into(),
            priority: 100,
            weight: 100,
            enabled: true,
            health_status: Some(status.into()),
            consecutive_failures: failures,
            checked_secs_ago,
        }
    }

    #[test]
    fn probe_failures_past_threshold_are_candidates() {
        let r = rv("healthy", "j1", Some(Uuid::new_v4()), 600);
        let counts = HashMap::from([(r.id, 3i64)]);
        let set = restart_candidates(std::slice::from_ref(&r), &counts, 3, &[]);
        assert!(set.contains(&r.id));
        // below threshold -> not a candidate
        let counts = HashMap::from([(r.id, 2i64)]);
        assert!(restart_candidates(&[r], &counts, 3, &[]).is_empty());
    }

    #[test]
    fn gateway_unhealthy_endpoint_is_a_candidate_even_when_get_probe_passes() {
        // The zombie case from production: Ollama answers /v1/models instantly
        // (provisioner GET probe passes, counter stays 0) but hangs on real
        // inference — the gateway's endpoint check says unhealthy. Restart it.
        let ep_id = Uuid::new_v4();
        let r = rv("healthy", "j1", Some(ep_id), 600);
        let eps = [ep(ep_id, "unhealthy", 528, Some(60))];
        let set = restart_candidates(std::slice::from_ref(&r), &HashMap::new(), 3, &eps);
        assert!(set.contains(&r.id));
    }

    #[test]
    fn gateway_signal_requires_failures_and_recency() {
        let ep_id = Uuid::new_v4();
        let r = rv("healthy", "j1", Some(ep_id), 600);
        // only one failed check -> not yet
        let eps = [ep(ep_id, "unhealthy", 1, Some(60))];
        assert!(restart_candidates(std::slice::from_ref(&r), &HashMap::new(), 3, &eps).is_empty());
        // stale verdict (checks disabled long ago) -> ignored
        let eps = [ep(ep_id, "unhealthy", 528, Some(90_000))];
        assert!(restart_candidates(std::slice::from_ref(&r), &HashMap::new(), 3, &eps).is_empty());
        // never checked -> ignored
        let mut never = ep(ep_id, "unhealthy", 528, None);
        never.checked_secs_ago = None;
        assert!(
            restart_candidates(std::slice::from_ref(&r), &HashMap::new(), 3, &[never]).is_empty()
        );
        // healthy endpoint -> ignored
        let eps = [ep(ep_id, "healthy", 0, Some(60))];
        assert!(restart_candidates(&[r], &HashMap::new(), 3, &eps).is_empty());
    }

    #[test]
    fn zero_threshold_disables_both_signals() {
        let ep_id = Uuid::new_v4();
        let r = rv("healthy", "j1", Some(ep_id), 600);
        let counts = HashMap::from([(r.id, 99i64)]);
        let eps = [ep(ep_id, "unhealthy", 528, Some(60))];
        assert!(restart_candidates(&[r], &counts, 0, &eps).is_empty());
    }

    #[test]
    fn non_healthy_replicas_are_never_candidates() {
        // A starting replica's endpoint doesn't exist yet; a draining one is
        // already being reconciled away. Only promoted healthy rows restart.
        let ep_id = Uuid::new_v4();
        let mut r = rv("draining", "j1", Some(ep_id), 600);
        let eps = [ep(ep_id, "unhealthy", 528, Some(60))];
        assert!(restart_candidates(&[r.clone()], &HashMap::new(), 3, &eps).is_empty());
        r.state = "starting".into();
        assert!(restart_candidates(&[r], &HashMap::new(), 3, &eps).is_empty());
    }

    // --- update_probe_failures (self-heal counters) ---

    #[test]
    fn probe_failure_counter_increments_on_miss_and_decays_on_pass() {
        let r = rv("healthy", "j1", Some(Uuid::new_v4()), 600);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["n1"])]);
        let mut counts = HashMap::new();

        // probed, not in health map -> failed -> increments
        update_probe_failures(
            std::slice::from_ref(&r),
            &jobs,
            &HashMap::new(),
            &mut counts,
        );
        update_probe_failures(
            std::slice::from_ref(&r),
            &jobs,
            &HashMap::new(),
            &mut counts,
        );
        assert_eq!(counts.get(&id), Some(&2));

        // passing probe decays by one (not a full reset), so an intermittent
        // flap can't keep re-arming the counter from zero.
        let health = HashMap::from([(id, 8000u16)]);
        update_probe_failures(std::slice::from_ref(&r), &jobs, &health, &mut counts);
        assert_eq!(counts.get(&id), Some(&1));

        // decaying past one drops the entry entirely.
        update_probe_failures(&[r], &jobs, &health, &mut counts);
        assert_eq!(counts.get(&id), None);
    }

    #[test]
    fn flapping_replica_never_reaches_restart_threshold() {
        // A single-threaded server that alternates miss/pass on the tick beat
        // must not accumulate toward a restart: increments and decays cancel out.
        let r = rv("healthy", "j1", Some(Uuid::new_v4()), 600);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["n1"])]);
        let miss: HashMap<Uuid, u16> = HashMap::new();
        let pass = HashMap::from([(id, 8000u16)]);
        let mut counts = HashMap::new();
        for i in 0..40 {
            let health = if i % 2 == 0 { &miss } else { &pass };
            update_probe_failures(std::slice::from_ref(&r), &jobs, health, &mut counts);
            assert!(
                counts.get(&id).copied().unwrap_or(0) <= 1,
                "flap must stay near zero, got {counts:?} at i={i}"
            );
        }
    }

    #[test]
    fn sustained_failure_climbs_to_threshold() {
        // A genuinely dead server misses every probe -> the counter climbs one
        // per tick and crosses the default self-heal threshold.
        let r = rv("healthy", "j1", Some(Uuid::new_v4()), 600);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["n1"])]);
        let mut counts = HashMap::new();
        for _ in 0..20 {
            update_probe_failures(
                std::slice::from_ref(&r),
                &jobs,
                &HashMap::new(),
                &mut counts,
            );
        }
        assert_eq!(counts.get(&id), Some(&20));
    }

    #[test]
    fn probe_failure_counter_ignores_unjudgeable_replicas() {
        // Booting (starting), job not RUNNING, or no nodes reported: a probe
        // miss there is expected and must not accrue toward a restart.
        let starting = rv("starting", "j1", None, 30);
        let pending_job = rv("healthy", "j2", Some(Uuid::new_v4()), 600);
        let no_nodes = rv("healthy", "j3", Some(Uuid::new_v4()), 600);
        let gone = rv("healthy", "j4", Some(Uuid::new_v4()), 600);
        let jobs = HashMap::from([
            job("j1", JobState::Running, &["n1"]),
            job("j2", JobState::Pending, &[""]),
            job("j3", JobState::Running, &[]),
        ]);
        let mut counts = HashMap::new();
        update_probe_failures(
            &[starting, pending_job, no_nodes, gone],
            &jobs,
            &HashMap::new(),
            &mut counts,
        );
        assert!(counts.is_empty(), "got {counts:?}");
    }

    #[test]
    fn draining_replica_with_pending_job_is_left_alone() {
        // Cancel sent but the job is still queued (Pending) -> wait, don't delete.
        let r = rv("draining", "j1", None, 10);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Pending, &[""])]);
        let actions = plan(&spec(0), &[r], &jobs, &HashMap::new(), &HashSet::new(), 900);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::Delete { replica_id } if *replica_id == id)),
            "draining replica with a pending job must be left alone; got {actions:?}"
        );
    }
}
