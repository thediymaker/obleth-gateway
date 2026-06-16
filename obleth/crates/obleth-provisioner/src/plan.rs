use crate::domain::*;
use std::collections::HashMap;
use uuid::Uuid;

/// Pure reconcile. `jobs` is keyed by slurm_job_id; `health` is keyed by
/// replica id and only needs entries for `starting` replicas being probed.
pub fn plan(
    spec: &ManagedSpecView,
    replicas: &[ReplicaView],
    jobs: &HashMap<String, JobInfo>,
    health: &HashMap<Uuid, bool>,
    serving_port: i64,
    lost_retention_secs: i64,
) -> Vec<Action> {
    let mut actions = Vec::new();
    let mut alive = 0i64;
    // Replicas with a live job, eligible to be cancelled if we're over target.
    // (replica_id, job_id, rank, age) where lower rank = cancel first.
    let mut cancellable: Vec<(Uuid, String, u8, i64)> = Vec::new();

    for r in replicas {
        // GC dead rows past retention; they don't count as alive.
        if r.state == "lost" {
            if r.age_secs >= lost_retention_secs {
                actions.push(Action::Delete { replica_id: r.id });
            }
            continue;
        }
        if r.state == "draining" {
            // already on its way out; ignore (next tick its job goes Gone).
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
                cancellable.push((r.id, r.slurm_job_id.clone(), 0, r.age_secs)); // cancel pending first
            }
            Some(JobState::Running) => {
                alive += 1;
                if r.state == "starting" {
                    if health.get(&r.id).copied().unwrap_or(false) {
                        let node = jobs
                            .get(&r.slurm_job_id)
                            .and_then(|j| j.nodes.first().cloned())
                            .unwrap_or_default();
                        actions.push(Action::Promote {
                            replica_id: r.id,
                            api_base: format!("http://{node}:{serving_port}"),
                        });
                    }
                    cancellable.push((r.id, r.slurm_job_id.clone(), 1, r.age_secs));
                } else {
                    cancellable.push((r.id, r.slurm_job_id.clone(), 2, r.age_secs)); // healthy: cancel last
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
        cancellable.sort_by(|a, b| a.2.cmp(&b.2).then(b.3.cmp(&a.3)));
        for (id, job_id, _, _) in cancellable.into_iter().take((alive - target) as usize) {
            actions.push(Action::Cancel { replica_id: id, job_id });
        }
    }

    actions
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
        ReplicaView { id: Uuid::new_v4(), model_id: Uuid::new_v4(), slurm_job_id: job.into(), state: state.into(), endpoint_id: ep, age_secs: age }
    }
    fn job(id: &str, st: JobState, nodes: &[&str]) -> (String, JobInfo) {
        (id.into(), JobInfo { job_id: id.into(), state: st, nodes: nodes.iter().map(|s| s.to_string()).collect() })
    }
    fn spec(target: i64) -> ManagedSpecView { ManagedSpecView { target_replicas: target, max_job_failures: 0 } }
    fn spec_with_limit(target: i64, limit: i64) -> ManagedSpecView { ManagedSpecView { target_replicas: target, max_job_failures: limit } }

    #[test]
    fn empty_state_submits_target() {
        let actions = plan(&spec(2), &[], &HashMap::new(), &HashMap::new(), 8000, 900);
        assert_eq!(actions.iter().filter(|a| **a == Action::Submit).count(), 2);
    }

    #[test]
    fn healthy_at_target_does_nothing() {
        let r1 = rv("healthy", "j1", Some(Uuid::new_v4()), 100);
        let r2 = rv("healthy", "j2", Some(Uuid::new_v4()), 100);
        let jobs = HashMap::from([job("j1", JobState::Running, &["n1"]), job("j2", JobState::Running, &["n2"])]);
        let actions = plan(&spec(2), &[r1, r2], &jobs, &HashMap::new(), 8000, 900);
        assert!(actions.is_empty(), "got {actions:?}");
    }

    #[test]
    fn starting_and_running_and_healthy_promotes_with_url() {
        let r = rv("starting", "j1", None, 30);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["gpu7"])]);
        let health = HashMap::from([(id, true)]);
        let actions = plan(&spec(1), &[r], &jobs, &health, 8000, 900);
        assert_eq!(actions, vec![Action::Promote { replica_id: id, api_base: "http://gpu7:8000".into() }]);
    }

    #[test]
    fn starting_but_unhealthy_does_not_promote_and_does_not_oversubmit() {
        let r = rv("starting", "j1", None, 30);
        let id = r.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["gpu7"])]);
        let health = HashMap::from([(id, false)]);
        let actions = plan(&spec(1), &[r], &jobs, &health, 8000, 900);
        // still "alive" (counts toward target), so no submit, no promote.
        assert!(actions.is_empty(), "got {actions:?}");
    }

    #[test]
    fn preempted_job_marks_lost_and_resubmits() {
        let ep = Uuid::new_v4();
        let r = rv("healthy", "j1", Some(ep), 500);
        let id = r.id;
        // job gone from slurm entirely
        let actions = plan(&spec(1), &[r], &HashMap::new(), &HashMap::new(), 8000, 900);
        assert!(actions.contains(&Action::MarkLost { replica_id: id, endpoint_id: Some(ep) }));
        assert!(actions.contains(&Action::Submit));
    }

    #[test]
    fn excess_replicas_are_cancelled_pending_first() {
        let healthy = rv("healthy", "j1", Some(Uuid::new_v4()), 1000);
        let pending = rv("pending", "j2", None, 10);
        let pid = pending.id;
        let jobs = HashMap::from([job("j1", JobState::Running, &["n1"]), job("j2", JobState::Pending, &["",])]);
        let actions = plan(&spec(1), &[healthy, pending], &jobs, &HashMap::new(), 8000, 900);
        assert_eq!(actions, vec![Action::Cancel { replica_id: pid, job_id: "j2".into() }]);
    }

    #[test]
    fn old_lost_rows_are_gced() {
        let r = rv("lost", "j1", None, 5000);
        let id = r.id;
        let actions = plan(&spec(0), &[r], &HashMap::new(), &HashMap::new(), 8000, 900);
        assert!(actions.contains(&Action::Delete { replica_id: id }));
    }

    #[test]
    fn failure_limit_suppresses_submit_when_reached() {
        // 3 lost replicas, limit = 3 → no new Submit
        let lost: Vec<ReplicaView> = (0..3).map(|i| rv("lost", &format!("j{i}"), None, 60)).collect();
        let actions = plan(&spec_with_limit(2, 3), &lost, &HashMap::new(), &HashMap::new(), 8000, 900);
        assert!(!actions.iter().any(|a| *a == Action::Submit), "submit must be suppressed at limit");
    }

    #[test]
    fn failure_limit_allows_submit_below_threshold() {
        // 2 lost replicas, limit = 5 → still submits
        let lost: Vec<ReplicaView> = (0..2).map(|i| rv("lost", &format!("j{i}"), None, 60)).collect();
        let actions = plan(&spec_with_limit(2, 5), &lost, &HashMap::new(), &HashMap::new(), 8000, 900);
        assert!(actions.iter().any(|a| *a == Action::Submit));
    }

    #[test]
    fn zero_limit_means_no_cap() {
        // 100 lost replicas, limit = 0 → unlimited, still submits
        let lost: Vec<ReplicaView> = (0..100).map(|i| rv("lost", &format!("j{i}"), None, 60)).collect();
        let actions = plan(&spec(2), &lost, &HashMap::new(), &HashMap::new(), 8000, 900);
        assert!(actions.iter().any(|a| *a == Action::Submit));
    }
}
