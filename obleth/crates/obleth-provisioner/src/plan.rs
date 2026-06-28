use crate::domain::*;
use std::collections::HashMap;
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
pub fn plan(
    spec: &ManagedSpecView,
    replicas: &[ReplicaView],
    jobs: &HashMap<String, JobInfo>,
    health: &HashMap<Uuid, u16>,
    lost_retention_secs: i64,
) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut alive = 0i64;
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

        // Operator-requested restart: cancel this replica's job now (regardless
        // of target) so the resubmit-to-target below launches a fresh one. Don't
        // count it as alive. After cancel it becomes "draining" (skipped above)
        // until its job goes Gone and the row is GC'd, which clears the flag.
        if r.cancel_requested
            && matches!(
                jobs.get(&r.slurm_job_id).map(|j| j.state),
                Some(JobState::Pending | JobState::Running)
            )
        {
            actions.push(Action::Cancel {
                replica_id: r.id,
                job_id: r.slurm_job_id.clone(),
                endpoint_id: r.endpoint_id,
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
                // failed): the planner would otherwise never re-promote a
                // "healthy" row, leaving the model permanently unhealthy.
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
        let actions = plan(&spec(2), &[], &HashMap::new(), &HashMap::new(), 900);
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
        let actions = plan(&spec(2), &[r1, r2], &jobs, &HashMap::new(), 900);
        assert!(actions.is_empty(), "got {actions:?}");
    }

    #[test]
    fn starting_and_running_and_healthy_promotes_with_url() {
        let r = rv("starting", "j1", None, 30);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["gpu7"])]);
        let health = HashMap::from([(id, 8000u16)]);
        let actions = plan(&spec(1), &[r], &jobs, &health, 900);
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
        let actions = plan(&spec(1), &[r], &jobs, &health, 900);
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
        let actions = plan(&spec(1), &[r], &jobs, &health, 900);
        assert!(actions.is_empty(), "got {actions:?}");
    }

    #[test]
    fn starting_but_unhealthy_does_not_promote_and_does_not_oversubmit() {
        let r = rv("starting", "j1", None, 30);
        let jobs = HashMap::from([job("j1", JobState::Running, &["gpu7"])]);
        let health: HashMap<Uuid, u16> = HashMap::new();
        let actions = plan(&spec(1), &[r], &jobs, &health, 900);
        // still "alive" (counts toward target), so no submit, no promote.
        assert!(actions.is_empty(), "got {actions:?}");
    }

    #[test]
    fn preempted_job_marks_lost_and_resubmits() {
        let ep = Uuid::new_v4();
        let r = rv("healthy", "j1", Some(ep), 500);
        let id = r.id;
        // job gone from slurm entirely
        let actions = plan(&spec(1), &[r], &HashMap::new(), &HashMap::new(), 900);
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
        let actions = plan(&spec(1), &[healthy, pending], &jobs, &HashMap::new(), 900);
        assert_eq!(
            actions,
            vec![Action::Cancel {
                replica_id: pid,
                job_id: "j2".into(),
                endpoint_id: None
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
        let actions = plan(&spec(1), &[r], &jobs, &HashMap::new(), 900);
        // Cancelled (with its endpoint) regardless of target, and a fresh replica
        // submitted to refill — i.e. a restart.
        assert!(actions.contains(&Action::Cancel {
            replica_id: id,
            job_id: "j1".into(),
            endpoint_id: Some(ep)
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
        let actions = plan(&spec(1), &[h1, h2], &jobs, &HashMap::new(), 900);
        assert_eq!(
            actions,
            vec![Action::Cancel {
                replica_id: h1id,
                job_id: "j1".into(),
                endpoint_id: Some(ep)
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
        let actions = plan(&spec(1), &[r], &jobs, &health, 900);
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
        let actions = plan(&spec(1), &[r], &jobs, &health, 900);
        assert!(actions.is_empty(), "got {actions:?}");
    }

    #[test]
    fn old_lost_rows_are_gced() {
        let r = rv("lost", "j1", None, 5000);
        let id = r.id;
        let actions = plan(&spec(0), &[r], &HashMap::new(), &HashMap::new(), 900);
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
            900,
        );
        assert!(
            !actions.iter().any(|a| *a == Action::Submit),
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
            900,
        );
        assert!(actions.iter().any(|a| *a == Action::Submit));
    }

    #[test]
    fn zero_limit_means_no_cap() {
        // 100 lost replicas, limit = 0 → unlimited, still submits
        let lost: Vec<ReplicaView> = (0..100)
            .map(|i| rv("lost", &format!("j{i}"), None, 60))
            .collect();
        let actions = plan(&spec(2), &lost, &HashMap::new(), &HashMap::new(), 900);
        assert!(actions.iter().any(|a| *a == Action::Submit));
    }

    #[test]
    fn draining_replica_with_gone_job_is_deleted() {
        // Job already terminated (Gone) -> the stuck draining row must be deleted.
        let r = rv("draining", "j1", None, 120);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Gone, &[])]);
        let actions = plan(&spec(1), &[r], &jobs, &HashMap::new(), 900);
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
        let actions = plan(&spec(1), &[r], &HashMap::new(), &HashMap::new(), 900);
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
        let actions = plan(&spec(0), &[r], &jobs, &HashMap::new(), 900);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::Delete { replica_id } if *replica_id == id)),
            "draining replica with a live job must be left alone; got {actions:?}"
        );
    }

    #[test]
    fn draining_replica_with_pending_job_is_left_alone() {
        // Cancel sent but the job is still queued (Pending) -> wait, don't delete.
        let r = rv("draining", "j1", None, 10);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Pending, &[""])]);
        let actions = plan(&spec(0), &[r], &jobs, &HashMap::new(), 900);
        assert!(
            !actions
                .iter()
                .any(|a| matches!(a, Action::Delete { replica_id } if *replica_id == id)),
            "draining replica with a pending job must be left alone; got {actions:?}"
        );
    }
}
