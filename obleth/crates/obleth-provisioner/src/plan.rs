use crate::domain::*;
use chrono::{DateTime, Utc};
use obleth_config::ManagedBackoff;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// How long a replica that is being retired keeps its job after its endpoint
/// is removed, so requests already in flight can finish.
pub const DRAIN_GRACE_SECS: i64 = 60;

/// Highest TCP port a window may use.
const MAX_PORT: i64 = 65_535;

/// Lowest base the window search goes down to when every window above the
/// serving port is taken.
const MIN_FALLBACK_PORT: i64 = 1024;

/// Smallest window base `serving_port + i*span` whose window
/// `[base, base + span)` overlaps none of the live windows. `live_port_bases`
/// holds the bases of every live replica of every model (all windows are
/// `span` wide), so two replicas never share a port, whatever their models'
/// serving ports. When every window above the serving port is taken, the
/// search continues below it; only when nothing is free at all does it fall
/// back to the serving port.
pub fn next_free_window_base(serving_port: i64, span: i64, live_port_bases: &[i64]) -> i64 {
    let span = span.max(1);
    let free = |b: i64| {
        !live_port_bases
            .iter()
            .any(|&o| b < o + span && o < b + span)
    };
    let mut base = serving_port.max(1);
    while base + span - 1 <= MAX_PORT {
        if free(base) {
            return base;
        }
        base += span;
    }
    let mut base = serving_port - span;
    while base >= MIN_FALLBACK_PORT {
        if free(base) {
            return base;
        }
        base -= span;
    }
    serving_port
}

/// The slice of ManagedModelSpec the planner needs (keeps it decoupled from the
/// full DB struct in tests).
#[derive(Debug, Clone, Default)]
pub struct ManagedSpecView {
    pub target_replicas: i64,
    /// Minutes a running job's server may take to become healthy (0 = never
    /// give up). Also how long a job may sit pending for a reason Slurm will
    /// not clear by waiting.
    pub startup_timeout_mins: i64,
    /// Minutes before a job's time limit at which a replacement is started
    /// (0 = off).
    pub handoff_mins: i64,
    /// No submits before this time.
    pub paused_until: Option<DateTime<Utc>>,
    /// The model left the managed set (paused or removed): everything drains.
    pub removed: bool,
}

/// Self-heal verdicts for healthy replicas, from [`self_heal_candidates`].
#[derive(Debug, Clone, Default)]
pub struct SelfHeal {
    /// The server has stopped answering the provisioner's probes (sustained)
    /// and the gateway has no fresh healthy verdict for it.
    pub dead: HashSet<Uuid>,
    /// The server answers probes but the gateway's fresh inference check
    /// keeps failing (a zombie).
    pub zombie: HashSet<Uuid>,
}

/// Everything the planner reads for one model. Pure data: the planner does
/// no I/O.
pub struct PlanInput<'a> {
    pub spec: &'a ManagedSpecView,
    pub replicas: &'a [ReplicaView],
    /// Slurm's view of each tracked job, by job id. Absent = gone.
    pub jobs: &'a HashMap<String, JobInfo>,
    /// Passing probes (health path answered and the server lists the right
    /// model): replica id -> port.
    pub health: &'a HashMap<Uuid, u16>,
    /// Why a probe did not pass, when there is something useful to say (a
    /// different model answers on the port).
    pub probe_notes: &'a HashMap<Uuid, String>,
    pub self_heal: &'a SelfHeal,
    /// Seconds a replica has been waiting on something: a pending job with a
    /// reason Slurm will not clear, or a serving replica awaiting its
    /// replacement. Absent = 0.
    pub waited_secs: &'a HashMap<Uuid, i64>,
    /// This model's endpoints from a good snapshot this tick, and the name
    /// prefix the provisioner gives them (`{job_prefix}{model_name}-`).
    /// `None` skips the phantom-endpoint sweep.
    pub endpoints: Option<(&'a [EndpointView], &'a str)>,
    pub now: DateTime<Utc>,
    pub lost_retention_secs: i64,
}

/// Why a healthy replica is being replaced.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Retire {
    /// Its job is near its time limit; carries the end.
    Handoff(DateTime<Utc>),
    /// The operator asked for a replacement (restart or roll).
    Requested,
    /// Self-heal: it fails health checks and is the last serving replica, or
    /// the gateway's inference check fails.
    Unhealthy,
}

/// Plain sentence for a job Slurm put back in the queue.
pub const REQUEUED_DETAIL: &str = "Slurm put the job back in the queue (preempted or its node failed); it will start again when resources free up.";

/// Plain sentence for a job that restarted under the same id on a new start.
fn restarted_detail(job: &JobInfo) -> String {
    match job.probe_host() {
        Some(host) => format!(
            "Slurm restarted the job on {host}, so obleth stopped sending it requests until the server answers again."
        ),
        None => "Slurm restarted the job, so obleth stopped sending it requests until the server answers again.".to_string(),
    }
}

/// How a pending job's reason reads, when it is one Slurm will not clear by
/// waiting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stuck {
    /// Held by an admin, the user, or after a failed launch.
    Held,
    /// The request can never run where it was sent (partition or QOS limits,
    /// bad constraints, an account it may not use).
    NeverRuns,
    /// A per-user or per-account job limit that the deployment's own running
    /// jobs keep full.
    Limited,
}

const NEVER_RUNS_REASONS: &[&str] = &[
    "badconstraints",
    "partitionconfig",
    "partitiontimelimit",
    "partitionnodelimit",
    "dependencyneversatisfied",
    "invalidaccount",
    "invalidqos",
    "accountnotallowed",
    "qosnotallowed",
    "nodesdesignatedforotherjobs",
];

const LIMITED_REASONS: &[&str] = &[
    "qosmaxjobsperuserlimit",
    "qosmaxjobsperaccount",
    "assocmaxjobslimit",
    "qosmaxsubmitjobperuserlimit",
    "assocmaxsubmitjoblimit",
];

/// Classify a pending job's reason. `None` for ordinary waiting (Resources,
/// Priority, ...), which clears on its own.
pub fn stuck_reason(job: &JobInfo) -> Option<Stuck> {
    if job.state != JobState::Pending {
        return None;
    }
    if job
        .state_flags
        .iter()
        .any(|f| f == "REQUEUE_HOLD" || f == "RESV_DEL_HOLD")
    {
        return Some(Stuck::Held);
    }
    let reason = job.reason.as_deref()?.trim();
    let key: String = reason
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_ascii_lowercase();
    if key.contains("held") {
        Some(Stuck::Held)
    } else if NEVER_RUNS_REASONS.contains(&key.as_str())
        || key.ends_with("perjob")
        || key.ends_with("perjoblimit")
        || key.contains("maxwallduration")
    {
        Some(Stuck::NeverRuns)
    } else if LIMITED_REASONS.contains(&key.as_str()) {
        Some(Stuck::Limited)
    } else {
        None
    }
}

/// The dashboard sentence for a stuck pending job.
pub fn stuck_detail(kind: Stuck, reason: Option<&str>, startup_timeout_mins: i64) -> String {
    let why = reason
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .map(|r| format!(" ({r})"))
        .unwrap_or_default();
    let head = match kind {
        Stuck::Held => format!(
            "Slurm is holding this job{why}, so it will not start until the hold is released."
        ),
        Stuck::NeverRuns => format!(
            "Slurm says this job cannot start as asked{why}. Check the partition, account, time limit and resources on the deployment."
        ),
        Stuck::Limited => format!(
            "Slurm will not start this job while other jobs fill a job limit{why}. Lower the replica count or ask for a higher limit."
        ),
    };
    if startup_timeout_mins > 0 {
        format!(
            "{head} If it still cannot start after {startup_timeout_mins} minutes, obleth cancels it and counts a failed launch."
        )
    } else {
        head
    }
}

/// True when a job that ended without ever serving counts as a failed launch.
/// Preemption, a failed node, an outside cancel and a federation revoke are
/// not the launch's fault.
pub fn counts_as_failed_launch(raw_state: Option<&str>) -> bool {
    !matches!(
        raw_state.map(str::trim),
        Some("PREEMPTED" | "NODE_FAIL" | "CANCELLED" | "REVOKED" | "DEADLINE")
    )
}

/// When a healthy replica's job is close enough to its time limit that a
/// replacement should start now: the job's end. `None` when the handoff is
/// off, the job has no limit, or the limit is so short that a replacement
/// would itself be due at once.
fn handoff_due(job: &JobInfo, handoff_mins: i64, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    if handoff_mins <= 0 || job.time_limit_unlimited {
        return None;
    }
    let end = job.end_time?;
    let limit_mins = job
        .time_limit_mins
        .or_else(|| job.start_time.map(|s| (end - s).num_minutes()))?;
    if limit_mins <= 2 * handoff_mins {
        return None;
    }
    ((end - now).num_seconds() <= handoff_mins * 60).then_some(end)
}

/// True when a promoted replica's job is no longer the run it was promoted
/// under: Slurm started it again (requeue, restart), or it now runs on
/// another node.
fn restarted_elsewhere(r: &ReplicaView, job: &JobInfo) -> bool {
    if let (Some(promoted), Some(started)) = (r.job_started_at, job.start_time) {
        if promoted.timestamp() != started.timestamp() {
            return true;
        }
    }
    match (r.nodes.as_deref().map(str::trim), job.probe_host()) {
        (Some(nodes), Some(host)) if !nodes.is_empty() => {
            !nodes.split(',').any(|n| n.trim() == host)
        }
        _ => false,
    }
}

/// True when a running job has had longer than the startup timeout to
/// become healthy.
fn startup_timed_out(job: &JobInfo, startup_timeout_mins: i64, now: DateTime<Utc>) -> bool {
    startup_timeout_mins > 0
        && job
            .start_time
            .is_some_and(|s| (now - s).num_seconds() >= startup_timeout_mins * 60)
}

/// Pure reconcile for one model. See [`PlanInput`] for what it reads.
///
/// Replicas whose job runs elsewhere than where they were promoted are
/// demoted at once; jobs near their time limit, and replicas the operator
/// asked to replace, get a successor first and are drained only once enough
/// replicas serve; a job that never becomes healthy or never starts is
/// cancelled after the startup timeout and counts as a failed launch.
pub fn plan(input: &PlanInput) -> Vec<Action> {
    let spec = input.spec;
    let now = input.now;
    let jobs = input.jobs;
    let mut actions = Vec::new();
    // Replicas that leave service this tick (their own action removes their
    // endpoint), and the endpoints those actions remove.
    let mut leaving: HashSet<Uuid> = HashSet::new();
    // Desired detail per replica, for replicas no other action annotates.
    let mut details: HashMap<Uuid, Option<String>> = HashMap::new();
    let mut ends_at: HashMap<Uuid, DateTime<Utc>> = HashMap::new();

    let mut queued: Vec<&ReplicaView> = Vec::new();
    let mut loading: Vec<&ReplicaView> = Vec::new();
    let mut promoting: HashSet<Uuid> = HashSet::new();
    let mut healthy: Vec<(&ReplicaView, &JobInfo)> = Vec::new();
    let mut unknown = 0i64;

    let cancel = |r: &ReplicaView, reason: CancelReason, cancel_now: bool| Action::Cancel {
        replica_id: r.id,
        job_id: r.slurm_job_id.clone(),
        endpoint_id: r.endpoint_id,
        reason,
        cancel_now,
    };

    for r in input.replicas {
        let job = jobs.get(&r.slurm_job_id);
        if r.state == "lost" {
            // Aged from when it was lost, not created: a launch that failed
            // after a long queue or load stays visible for the full retention.
            if r.updated_secs >= input.lost_retention_secs {
                actions.push(Action::Delete { replica_id: r.id });
            }
            continue;
        }
        if r.state == "draining" {
            // Cancelled on purpose. Once the job is gone, delete the row (not
            // MarkLost: a drain is not a failure). While it still runs, the
            // grace period lets in-flight requests finish, then the job is
            // cancelled (again, if an earlier cancel did not take).
            match job.map(|j| j.state) {
                None | Some(JobState::Gone) => actions.push(Action::Delete { replica_id: r.id }),
                Some(_) if r.updated_secs >= DRAIN_GRACE_SECS => actions.push(Action::CancelJob {
                    replica_id: r.id,
                    job_id: r.slurm_job_id.clone(),
                }),
                Some(_) => {}
            }
            continue;
        }
        let Some(job) = job.filter(|j| j.state != JobState::Gone) else {
            // Carry Slurm's terminal state when the job is still listed, so
            // the launch history records why it ended (TIMEOUT, OOM, ...).
            let raw_state = job
                .map(|j| j.raw_state.trim().to_string())
                .filter(|s| !s.is_empty());
            let failed_launch = !r.has_served()
                && !r.cancel_requested
                && counts_as_failed_launch(raw_state.as_deref());
            actions.push(Action::MarkLost {
                replica_id: r.id,
                endpoint_id: r.endpoint_id,
                raw_state,
                reason: job.and_then(|j| j.reason.clone()),
                failed_launch,
            });
            leaving.insert(r.id);
            continue;
        };
        if job.state == JobState::Unknown {
            // A state this build does not know: count it as alive and leave
            // it alone rather than tear it down on a guess.
            unknown += 1;
            continue;
        }
        if r.cancel_requested {
            // The operator stopped this replica: a serving one drains first.
            actions.push(cancel(
                r,
                CancelReason::OperatorRestart,
                r.state != "healthy",
            ));
            leaving.insert(r.id);
            continue;
        }
        match job.state {
            JobState::Pending => {
                if r.state == "healthy" {
                    // Requeued or suspended: it no longer serves where it was
                    // registered. Out of rotation now; promoted again when it
                    // serves.
                    actions.push(Action::Demote {
                        replica_id: r.id,
                        endpoint_id: r.endpoint_id,
                        detail: REQUEUED_DETAIL.to_string(),
                    });
                    leaving.insert(r.id);
                    queued.push(r);
                    continue;
                }
                if r.replace_requested {
                    // Not serving yet: replace it at once.
                    actions.push(cancel(r, CancelReason::OperatorRestart, true));
                    leaving.insert(r.id);
                    continue;
                }
                if let Some(kind) = stuck_reason(job) {
                    let waited = input.waited_secs.get(&r.id).copied().unwrap_or(0);
                    if spec.startup_timeout_mins > 0 && waited >= spec.startup_timeout_mins * 60 {
                        actions.push(cancel(r, CancelReason::Stuck, true));
                        leaving.insert(r.id);
                        continue;
                    }
                    details.insert(
                        r.id,
                        Some(stuck_detail(
                            kind,
                            job.reason.as_deref(),
                            spec.startup_timeout_mins,
                        )),
                    );
                } else if job.is_requeued() {
                    details.insert(r.id, Some(REQUEUED_DETAIL.to_string()));
                } else {
                    details.insert(r.id, None);
                }
                queued.push(r);
            }
            JobState::Running => {
                if job.end_time.is_some() && job.end_time != r.job_ends_at {
                    if let Some(end) = job.end_time {
                        ends_at.insert(r.id, end);
                    }
                }
                if r.state == "healthy" {
                    if restarted_elsewhere(r, job) {
                        actions.push(Action::Demote {
                            replica_id: r.id,
                            endpoint_id: r.endpoint_id,
                            detail: restarted_detail(job),
                        });
                        leaving.insert(r.id);
                        loading.push(r);
                        continue;
                    }
                    if r.endpoint_id.is_none() {
                        // Healthy with no endpoint linked (a promote whose
                        // endpoint write failed, or one removed out of band):
                        // promote again to relink it.
                        if let Some(&port) = input.health.get(&r.id) {
                            actions.push(promote(r, job, port));
                            promoting.insert(r.id);
                        }
                    }
                    healthy.push((r, job));
                    continue;
                }
                // Submitted, running, not serving yet.
                if r.replace_requested {
                    actions.push(cancel(r, CancelReason::OperatorRestart, true));
                    leaving.insert(r.id);
                    continue;
                }
                if let Some(&port) = input.health.get(&r.id) {
                    actions.push(promote(r, job, port));
                    promoting.insert(r.id);
                } else if startup_timed_out(job, spec.startup_timeout_mins, now) {
                    actions.push(cancel(r, CancelReason::StartupTimeout, true));
                    leaving.insert(r.id);
                    continue;
                } else {
                    details.insert(r.id, input.probe_notes.get(&r.id).cloned());
                }
                loading.push(r);
            }
            JobState::Gone | JobState::Unknown => unreachable!("handled above"),
        }
    }

    // Sort the healthy replicas into serving and retiring.
    let mut serving: Vec<&ReplicaView> = Vec::new();
    let mut retiring: Vec<(&ReplicaView, Retire)> = Vec::new();
    let mut dead: Vec<&ReplicaView> = Vec::new();
    let mut requested: Vec<&ReplicaView> = Vec::new();
    for &(r, job) in &healthy {
        if input.self_heal.dead.contains(&r.id) {
            dead.push(r);
        } else if input.self_heal.zombie.contains(&r.id) {
            retiring.push((r, Retire::Unhealthy));
        } else if let Some(end) = handoff_due(job, spec.handoff_mins, now) {
            retiring.push((r, Retire::Handoff(end)));
        } else if r.replace_requested {
            requested.push(r);
        } else {
            serving.push(r);
        }
    }
    // Operator replacements (restart, roll) go one at a time: the oldest is
    // replaced first and the others keep serving until it is done.
    requested.sort_by_key(|r| std::cmp::Reverse(r.age_secs));
    let mut requested = requested.into_iter();
    if let Some(r) = requested.next() {
        retiring.push((r, Retire::Requested));
    }
    serving.extend(requested);
    // A server that stopped answering is cancelled at once when another
    // replica serves (at most one per tick, so a probe-side fault cannot
    // empty a fleet); the last one is replaced surge-first instead.
    let mut self_heal_budget = 1usize;
    for r in dead {
        if !serving.is_empty() && self_heal_budget > 0 {
            self_heal_budget -= 1;
            actions.push(cancel(r, CancelReason::ProbeFailed, true));
            leaving.insert(r.id);
        } else if serving.is_empty() {
            retiring.push((r, Retire::Unhealthy));
        } else {
            serving.push(r);
        }
    }

    // Submit or scale down. Retiring replicas do not count: they are being
    // replaced, so their successors are submitted now.
    let target = spec.target_replicas.max(0);
    let alive = (queued.len() + loading.len() + serving.len()) as i64 + unknown;
    let paused = spec.paused_until.is_some_and(|t| t > now);
    let mut cancelled_healthy = 0usize;
    if alive < target && !paused {
        for _ in 0..(target - alive) {
            actions.push(Action::Submit);
        }
    } else if alive > target {
        // Queued jobs newest first (the oldest has the most queue priority),
        // then loading ones newest first (the oldest is closest to serving),
        // then serving ones oldest first (closest to their time limit). A job
        // in a state this build does not know is never picked.
        let mut order: Vec<(&ReplicaView, bool)> = Vec::new();
        let mut q = queued.clone();
        q.sort_by_key(|r| r.age_secs);
        order.extend(q.into_iter().map(|r| (r, true)));
        let mut l: Vec<&ReplicaView> = loading.clone();
        l.sort_by_key(|r| (promoting.contains(&r.id), r.age_secs));
        order.extend(l.into_iter().map(|r| (r, true)));
        let mut s = serving.clone();
        s.sort_by_key(|r| std::cmp::Reverse(r.age_secs));
        order.extend(s.into_iter().map(|r| (r, false)));
        for (r, not_serving) in order.into_iter().take((alive - target) as usize) {
            let reason = if spec.removed {
                CancelReason::Removed
            } else {
                CancelReason::ScaleDown
            };
            promoting.remove(&r.id);
            if !not_serving {
                cancelled_healthy += 1;
            }
            // The cancel replaces a promote or demote planned for it above
            // (it removes the endpoint itself).
            actions.retain(|a| match a {
                Action::Promote { replica_id, .. } | Action::Demote { replica_id, .. } => {
                    *replica_id != r.id
                }
                _ => true,
            });
            actions.push(cancel(r, reason, not_serving));
            leaving.insert(r.id);
            details.remove(&r.id);
        }
    }

    // Retire replicas once enough others serve: k of them can go while the
    // serving count stays at target. Handoffs go in order of their time
    // limits, others oldest first. A replacement the operator asked for (or
    // self-heal) also goes once its successor had the startup timeout to
    // come up.
    let healthy_after = (serving.len() + promoting.len()) as i64 - cancelled_healthy as i64;
    let hr = retiring.len() as i64;
    let mut k = hr.min((healthy_after + hr - target).max(0));
    retiring.sort_by_key(|(r, why)| match why {
        Retire::Handoff(end) => (0, end.timestamp(), 0),
        _ => (1, 0, -r.age_secs),
    });
    for (r, why) in retiring {
        let waited = input.waited_secs.get(&r.id).copied().unwrap_or(0);
        let gave_up = !matches!(why, Retire::Handoff(_))
            && spec.startup_timeout_mins > 0
            && waited >= spec.startup_timeout_mins * 60;
        if k > 0 || gave_up {
            k -= 1;
            let reason = if spec.removed {
                CancelReason::Removed
            } else {
                CancelReason::Replaced
            };
            actions.push(cancel(r, reason, false));
            leaving.insert(r.id);
            continue;
        }
        let detail = match why {
            Retire::Handoff(end) if paused => format!(
                "This job reaches its time limit at {} UTC. New jobs are paused, so no replacement is starting.",
                end.format("%H:%M")
            ),
            Retire::Handoff(end) => format!(
                "This job reaches its time limit at {} UTC. A replacement is starting, and this replica stops once the replacement serves.",
                end.format("%H:%M")
            ),
            Retire::Requested => {
                "A replacement is starting, and this replica stops once the replacement serves."
                    .to_string()
            }
            Retire::Unhealthy => "This replica is failing health checks. A replacement is starting, and this replica stops once the replacement serves.".to_string(),
        };
        details.insert(r.id, Some(detail));
    }
    for r in &serving {
        details.entry(r.id).or_insert(None);
    }

    // Plain-sentence details and Slurm end times, only where they changed.
    let mut annotated: Vec<&ReplicaView> = input
        .replicas
        .iter()
        .filter(|r| !leaving.contains(&r.id) && !promoting.contains(&r.id))
        .collect();
    annotated.sort_by_key(|r| r.id);
    for r in annotated {
        let detail = details.get(&r.id).and_then(|want| {
            let want = want.as_deref().unwrap_or("");
            (r.state_detail.as_deref().unwrap_or("") != want).then(|| want.to_string())
        });
        let job_ends_at = ends_at.get(&r.id).copied();
        if detail.is_some() || job_ends_at.is_some() {
            actions.push(Action::Annotate {
                replica_id: r.id,
                detail,
                job_ends_at,
            });
        }
    }

    // Endpoints this provisioner registered whose replica no longer serves
    // under them: an earlier delete that failed, or a duplicate.
    if let Some((endpoints, prefix)) = input.endpoints {
        let removed: HashSet<Uuid> = input
            .replicas
            .iter()
            .filter(|r| leaving.contains(&r.id))
            .filter_map(|r| r.endpoint_id)
            .collect();
        for ep in endpoints {
            let Some(rid) = ep
                .name
                .strip_prefix(prefix)
                .and_then(|rest| rest.parse::<Uuid>().ok())
            else {
                continue;
            };
            if removed.contains(&ep.id) {
                continue;
            }
            let keep = promoting.contains(&rid)
                || input.replicas.iter().any(|r| {
                    r.id == rid
                        && r.state == "healthy"
                        && !leaving.contains(&r.id)
                        && r.endpoint_id.is_none_or(|e| e == ep.id)
                });
            if !keep {
                actions.push(Action::DeleteEndpoint { endpoint_id: ep.id });
            }
        }
    }

    actions
}

fn promote(r: &ReplicaView, job: &JobInfo, port: u16) -> Action {
    let node = job.probe_host().unwrap_or_default();
    Action::Promote {
        replica_id: r.id,
        api_base: endpoint_api_base(node, port as i64),
        port: port as i64,
        job_started_at: job.start_time,
        job_ends_at: job.end_time,
    }
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

/// Null out a replica's `endpoint_id` when it points at an endpoint the
/// gateway no longer has registered.
///
/// `model_replicas` has no foreign key to `model_endpoints`, so when an
/// endpoint is removed out of band (a manual delete in the dashboard, or a
/// demote/cancel whose endpoint delete landed) the replica keeps a dangling
/// reference. The planner's re-promote path fires only on
/// `endpoint_id.is_none()`, so such a healthy row would never relink, and a
/// later cancel would try to delete an endpoint that is long gone. Nulling
/// the dangling id here (in this tick's view) fixes both.
///
/// `known_endpoints` MUST be a reliable snapshot from a successful fetch this
/// tick — never call this with an empty set derived from a failed lookup, or a
/// transient error would strip every replica's endpoint at once.
pub fn clear_dangling_endpoints(replicas: &mut [ReplicaView], known_endpoints: &HashSet<Uuid>) {
    for r in replicas.iter_mut() {
        if r.endpoint_id
            .is_some_and(|ep| !known_endpoints.contains(&ep))
        {
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
        if probed_healthy_job(r, jobs).is_none() {
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

/// The job of a healthy replica the tick probes: running, with a host.
fn probed_healthy_job<'a>(
    r: &ReplicaView,
    jobs: &'a HashMap<String, JobInfo>,
) -> Option<&'a JobInfo> {
    if r.state != "healthy" {
        return None;
    }
    jobs.get(&r.slurm_job_id)
        .filter(|j| j.state == JobState::Running && j.probe_host().is_some())
}

/// Fleet guard for self-heal: true when at least two healthy replicas were
/// probed this tick (across every model) and every one of them failed. A
/// whole fleet going dark in one tick is far likelier a fault on the
/// provisioner's own path to the nodes (network policy, route, MTU) than
/// real deaths, so the probe counters must not advance and nothing is
/// restarted on the probe signal.
pub fn probe_path_down(
    replicas: &[ReplicaView],
    jobs: &HashMap<String, JobInfo>,
    health: &HashMap<Uuid, u16>,
) -> Option<usize> {
    let probed: Vec<Uuid> = replicas
        .iter()
        .filter(|r| probed_healthy_job(r, jobs).is_some())
        .map(|r| r.id)
        .collect();
    (probed.len() >= 2 && probed.iter().all(|id| !health.contains_key(id))).then_some(probed.len())
}

/// Consecutive failed *gateway* checks of a replica's endpoint that count as a
/// restart signal. The gateway's check is a real 1-token inference at a slow
/// cadence (minutes), so two failures is already a sustained outage — and it
/// catches zombies the provisioner's own GET probe cannot (a server that
/// answers metadata instantly but hangs forever on inference).
const GATEWAY_UNHEALTHY_CHECKS: i64 = 2;

/// The gateway's endpoint verdict is used only when it is at most this old.
/// Its checks run every few minutes by default; an older verdict (checks
/// turned off, or the gateway behind) says nothing about the server now.
pub const GATEWAY_FRESH_SECS: i64 = 1800;

/// Which healthy replicas self-heal should act on this tick:
///
/// - `dead`: the provisioner's own probe has failed for `probe_threshold` net
///   ticks (`probe_failures`, maintained by `update_probe_failures`, which
///   decays on a pass) and the gateway has no fresh healthy verdict for the
///   replica's endpoint (a fresh healthy one means the gateway still reaches
///   and serves it, so the fault is on the provisioner's side);
/// - `zombie`: the gateway's fresh check of the replica's endpoint is
///   `unhealthy` with at least `GATEWAY_UNHEALTHY_CHECKS` consecutive
///   failures: the server answers GETs but fails real inference.
///
/// `probe_threshold <= 0` disables self-heal entirely (both signals).
pub fn self_heal_candidates(
    replicas: &[ReplicaView],
    probe_failures: &HashMap<Uuid, i64>,
    probe_threshold: i64,
    endpoints: &[EndpointView],
) -> SelfHeal {
    let mut out = SelfHeal::default();
    if probe_threshold <= 0 {
        return out;
    }
    for r in replicas.iter().filter(|r| r.state == "healthy") {
        let fresh = r
            .endpoint_id
            .and_then(|id| endpoints.iter().find(|e| e.id == id))
            .filter(|e| e.checked_secs_ago.is_some_and(|s| s <= GATEWAY_FRESH_SECS));
        let gateway_healthy = fresh.is_some_and(|e| e.health_status.as_deref() == Some("healthy"));
        let gateway_unhealthy = fresh.is_some_and(|e| {
            e.health_status.as_deref() == Some("unhealthy")
                && e.consecutive_failures >= GATEWAY_UNHEALTHY_CHECKS
        });
        let probe_dead = probe_failures.get(&r.id).copied().unwrap_or(0) >= probe_threshold;
        if probe_dead && !gateway_healthy {
            out.dead.insert(r.id);
        } else if gateway_unhealthy {
            out.zombie.insert(r.id);
        }
    }
    out
}

/// What one tick's applied actions did to a model's launch record.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct LaunchOutcome {
    /// Launches that failed this tick (ended without serving, startup
    /// timeout, or stuck in the queue).
    pub failed: i64,
    /// How the last of them ended, as a short phrase.
    pub last_failure: Option<String>,
    /// A replica was promoted (a launch served).
    pub served: bool,
    /// Slurm refused a submit outright; carries its plain sentence.
    pub rejected: Option<String>,
}

/// Minutes to wait after the n-th consecutive backoff: 5, 15, 30, 60, then
/// hourly.
pub fn backoff_minutes(step: i64) -> i64 {
    match step {
        i64::MIN..=1 => 5,
        2 => 15,
        3 => 30,
        _ => 60,
    }
}

/// The model's new failure and pause state after a tick, or `None` when it
/// does not change. A launch that served resets the count and lifts any
/// pause. Failed launches add up; reaching `max_job_failures` (when it is
/// above 0) pauses submits on the backoff ladder. A rejected submit backs off
/// on the same ladder whatever the limit.
pub fn next_backoff(
    current: &ManagedBackoff,
    max_job_failures: i64,
    outcome: &LaunchOutcome,
    now: DateTime<Utc>,
) -> Option<ManagedBackoff> {
    let mut next = current.clone();
    if outcome.served {
        next = ManagedBackoff::default();
    }
    if outcome.failed > 0 {
        next.failed_launches += outcome.failed;
        if max_job_failures > 0 && next.failed_launches >= max_job_failures {
            next.backoff_step += 1;
            next.paused_until =
                Some(now + chrono::Duration::minutes(backoff_minutes(next.backoff_step)));
            let n = next.failed_launches;
            let launches = if n == 1 {
                "1 launch".to_string()
            } else {
                format!("{n} launches in a row")
            };
            next.paused_reason = Some(match &outcome.last_failure {
                Some(how) => format!(
                    "{launches} ended before the server became healthy; the last one {how}."
                ),
                None => format!("{launches} ended before the server became healthy."),
            });
        }
    }
    if let Some(msg) = &outcome.rejected {
        next.backoff_step += 1;
        next.paused_until =
            Some(now + chrono::Duration::minutes(backoff_minutes(next.backoff_step)));
        next.paused_reason = Some(msg.clone());
    }
    (next != *current).then_some(next)
}

/// A short phrase for how a failed launch ended, for the pause reason.
pub fn failure_phrase(action: &Action) -> Option<String> {
    match action {
        Action::MarkLost {
            raw_state, reason, ..
        } => Some(match (raw_state.as_deref(), reason.as_deref()) {
            (Some(s), Some(r)) => format!("ended {s} ({r})"),
            (Some(s), None) => format!("ended {s}"),
            _ => "disappeared from Slurm".to_string(),
        }),
        Action::Cancel {
            reason: CancelReason::StartupTimeout,
            ..
        } => Some("did not become healthy within the startup timeout".to_string()),
        Action::Cancel {
            reason: CancelReason::Stuck,
            ..
        } => Some("could not start in the Slurm queue".to_string()),
        _ => None,
    }
}

/// True when applying `action` records a failed launch. A replica that has
/// served once (and was then requeued) is not a failed launch.
pub fn is_failed_launch(action: &Action, replicas: &[ReplicaView]) -> bool {
    match action {
        Action::MarkLost { failed_launch, .. } => *failed_launch,
        Action::Cancel {
            replica_id, reason, ..
        } => {
            reason.is_failed_launch()
                && !replicas
                    .iter()
                    .any(|r| r.id == *replica_id && r.has_served())
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 1, 10, 12, 0, 0).unwrap()
    }

    fn mins_ago(m: i64) -> DateTime<Utc> {
        now() - chrono::Duration::minutes(m)
    }

    fn rv(state: &str, job: &str, ep: Option<Uuid>, age: i64) -> ReplicaView {
        ReplicaView {
            id: Uuid::new_v4(),
            slurm_job_id: job.into(),
            state: state.into(),
            endpoint_id: ep,
            age_secs: age,
            updated_secs: age,
            port_base: 8000,
            ..Default::default()
        }
    }

    /// A replica that was promoted on `node` under a job that started at
    /// `started`.
    fn served(job: &str, ep: Option<Uuid>, node: &str, started: DateTime<Utc>) -> ReplicaView {
        ReplicaView {
            nodes: Some(node.into()),
            job_started_at: Some(started),
            port: Some(8000),
            ..rv("healthy", job, ep, 600)
        }
    }

    fn job(id: &str, st: JobState, nodes: &[&str]) -> (String, JobInfo) {
        (
            id.to_string(),
            JobInfo {
                job_id: id.into(),
                state: st,
                nodes: nodes.iter().map(|s| s.to_string()).collect(),
                raw_state: match st {
                    JobState::Pending => "PENDING",
                    JobState::Running => "RUNNING",
                    JobState::Gone => "COMPLETED",
                    JobState::Unknown => "WEIRD",
                }
                .into(),
                ..Default::default()
            },
        )
    }

    fn running(id: &str, node: &str, started: DateTime<Utc>) -> (String, JobInfo) {
        let (k, mut j) = job(id, JobState::Running, &[node]);
        j.start_time = Some(started);
        (k, j)
    }

    fn ended(id: &str, raw: &str) -> (String, JobInfo) {
        let (k, mut j) = job(id, JobState::Gone, &[]);
        j.raw_state = raw.into();
        (k, j)
    }

    fn spec(target: i64) -> ManagedSpecView {
        ManagedSpecView {
            target_replicas: target,
            startup_timeout_mins: 60,
            handoff_mins: 15,
            ..Default::default()
        }
    }

    /// One planner run with everything optional defaulted.
    #[derive(Default)]
    struct Case {
        spec: ManagedSpecView,
        replicas: Vec<ReplicaView>,
        jobs: HashMap<String, JobInfo>,
        health: HashMap<Uuid, u16>,
        notes: HashMap<Uuid, String>,
        heal: SelfHeal,
        waited: HashMap<Uuid, i64>,
        endpoints: Option<(Vec<EndpointView>, String)>,
    }

    impl Case {
        fn new(spec: ManagedSpecView) -> Self {
            Case {
                spec,
                ..Default::default()
            }
        }
        fn plan(&self) -> Vec<Action> {
            plan(&PlanInput {
                spec: &self.spec,
                replicas: &self.replicas,
                jobs: &self.jobs,
                health: &self.health,
                probe_notes: &self.notes,
                self_heal: &self.heal,
                waited_secs: &self.waited,
                endpoints: self
                    .endpoints
                    .as_ref()
                    .map(|(e, p)| (e.as_slice(), p.as_str())),
                now: now(),
                lost_retention_secs: 900,
            })
        }
    }

    fn count(actions: &[Action], f: impl Fn(&Action) -> bool) -> usize {
        actions.iter().filter(|a| f(a)).count()
    }

    fn submits(actions: &[Action]) -> usize {
        count(actions, |a| matches!(a, Action::Submit))
    }

    fn cancel_of(actions: &[Action], id: Uuid) -> Option<(CancelReason, bool)> {
        actions.iter().find_map(|a| match a {
            Action::Cancel {
                replica_id,
                reason,
                cancel_now,
                ..
            } if *replica_id == id => Some((*reason, *cancel_now)),
            _ => None,
        })
    }

    fn detail_of(actions: &[Action], id: Uuid) -> Option<String> {
        actions.iter().find_map(|a| match a {
            Action::Annotate {
                replica_id,
                detail: Some(d),
                ..
            } if *replica_id == id => Some(d.clone()),
            _ => None,
        })
    }

    // ---- port windows ----

    #[test]
    fn next_free_window_base_empty_returns_serving_port() {
        assert_eq!(next_free_window_base(8000, 8, &[]), 8000);
    }

    #[test]
    fn next_free_window_base_skips_taken_slots() {
        assert_eq!(next_free_window_base(8000, 8, &[8000]), 8008);
        assert_eq!(next_free_window_base(8000, 8, &[8000, 8008]), 8016);
        assert_eq!(next_free_window_base(8000, 8, &[8008]), 8000);
    }

    #[test]
    fn windows_never_overlap_across_models_with_different_serving_ports() {
        // Model A holds 8000-8007; model B serves on 8001. B's window must
        // not share a port with A's.
        let b = next_free_window_base(8001, 8, &[8000]);
        assert!(b >= 8008, "B got {b}, overlapping A's 8000-8007");
        // And a third model on 8004 clears both.
        let c = next_free_window_base(8004, 8, &[8000, b]);
        assert!(c + 8 <= 8000 || c >= 8008, "{c}");
        assert!(c + 8 <= b || c >= b + 8, "{c} overlaps {b}");
    }

    #[test]
    fn window_search_goes_below_the_serving_port_when_the_top_is_full() {
        assert_eq!(next_free_window_base(65_528, 8, &[65_528]), 65_520);
    }

    // ---- basics ----

    #[test]
    fn empty_state_submits_target() {
        assert_eq!(submits(&Case::new(spec(2)).plan()), 2);
    }

    #[test]
    fn healthy_at_target_does_nothing() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![served("1", Some(Uuid::new_v4()), "n1", mins_ago(30))];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(30))]);
        c.health.insert(c.replicas[0].id, 8000);
        assert!(c.plan().is_empty(), "{:?}", c.plan());
    }

    #[test]
    fn running_replica_is_promoted_on_the_port_that_passed() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![rv("pending", "1", None, 60)];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(2))]);
        c.health.insert(c.replicas[0].id, 8003);
        let a = c.plan();
        assert_eq!(
            a,
            vec![Action::Promote {
                replica_id: c.replicas[0].id,
                api_base: "http://n1:8003/v1".into(),
                port: 8003,
                job_started_at: Some(mins_ago(2)),
                job_ends_at: None,
            }]
        );
    }

    #[test]
    fn loading_replica_is_not_promoted_and_not_oversubmitted() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![rv("pending", "1", None, 60)];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(2))]);
        assert!(c.plan().is_empty());
    }

    #[test]
    fn a_wrong_model_on_the_port_is_shown_on_the_replica() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![rv("pending", "1", None, 60)];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(2))]);
        c.notes.insert(
            c.replicas[0].id,
            "The server on port 8001 serves qwen3, not my-model.".into(),
        );
        let a = c.plan();
        assert_eq!(submits(&a), 0);
        assert!(detail_of(&a, c.replicas[0].id).unwrap().contains("qwen3"));
    }

    #[test]
    fn healthy_without_endpoint_is_repromoted() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![served("1", None, "n1", mins_ago(30))];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(30))]);
        c.health.insert(c.replicas[0].id, 8000);
        assert!(matches!(c.plan().as_slice(), [Action::Promote { .. }]));
    }

    #[test]
    fn clear_dangling_endpoints_nulls_missing_refs_in_any_state() {
        let live = Uuid::new_v4();
        let mut rs = vec![
            rv("healthy", "1", Some(live), 1),
            rv("healthy", "2", Some(Uuid::new_v4()), 1),
            rv("pending", "3", Some(Uuid::new_v4()), 1),
        ];
        clear_dangling_endpoints(&mut rs, &HashSet::from([live]));
        assert_eq!(rs[0].endpoint_id, Some(live));
        assert_eq!(rs[1].endpoint_id, None);
        assert_eq!(rs[2].endpoint_id, None);
    }

    #[test]
    fn an_unknown_job_state_is_left_alone_and_counts_as_alive() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![served("1", Some(Uuid::new_v4()), "n1", mins_ago(5))];
        c.jobs = HashMap::from([job("1", JobState::Unknown, &["n1"])]);
        assert!(c.plan().is_empty());
    }

    // ---- requeue, suspend, restart ----

    #[test]
    fn a_requeued_job_is_demoted_at_once_and_still_counts() {
        let ep = Uuid::new_v4();
        let mut c = Case::new(spec(1));
        c.replicas = vec![served("1", Some(ep), "n1", mins_ago(30))];
        let (k, mut j) = job("1", JobState::Pending, &[]);
        j.state_flags = vec!["PENDING".into(), "REQUEUED".into()];
        j.restart_count = 1;
        c.jobs = HashMap::from([(k, j)]);
        let a = c.plan();
        assert_eq!(
            a,
            vec![Action::Demote {
                replica_id: c.replicas[0].id,
                endpoint_id: Some(ep),
                detail: REQUEUED_DETAIL.into(),
            }]
        );
    }

    #[test]
    fn a_job_restarted_on_another_node_is_demoted() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![served("1", Some(Uuid::new_v4()), "n1", mins_ago(30))];
        c.jobs = HashMap::from([running("1", "n2", mins_ago(1))]);
        c.health.insert(c.replicas[0].id, 8000);
        let a = c.plan();
        assert_eq!(a.len(), 1, "{a:?}");
        match &a[0] {
            Action::Demote { detail, .. } => assert!(detail.contains("n2"), "{detail}"),
            other => panic!("{other:?}"),
        }
        assert_eq!(submits(&a), 0);
    }

    #[test]
    fn a_new_start_time_on_the_same_node_is_demoted_too() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![served("1", Some(Uuid::new_v4()), "n1", mins_ago(30))];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(1))]);
        assert!(matches!(c.plan().as_slice(), [Action::Demote { .. }]));
    }

    #[test]
    fn a_demoted_replica_is_promoted_again_when_it_serves() {
        let mut c = Case::new(spec(1));
        let mut r = served("1", None, "n1", mins_ago(30));
        r.state = "pending".into();
        r.state_detail = Some(REQUEUED_DETAIL.into());
        c.replicas = vec![r];
        c.jobs = HashMap::from([running("1", "n2", mins_ago(1))]);
        c.health.insert(c.replicas[0].id, 8000);
        match c.plan().as_slice() {
            [Action::Promote {
                api_base,
                job_started_at,
                ..
            }] => {
                assert_eq!(api_base, "http://n2:8000/v1");
                assert_eq!(*job_started_at, Some(mins_ago(1)));
            }
            other => panic!("{other:?}"),
        }
    }

    // ---- failure accounting ----

    #[test]
    fn a_launch_that_never_served_counts_as_failed() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![rv("pending", "1", None, 600)];
        c.jobs = HashMap::from([ended("1", "FAILED")]);
        let a = c.plan();
        assert!(a.contains(&Action::MarkLost {
            replica_id: c.replicas[0].id,
            endpoint_id: None,
            raw_state: Some("FAILED".into()),
            reason: None,
            failed_launch: true,
        }));
        assert_eq!(submits(&a), 1);
        assert!(is_failed_launch(&a[0], &c.replicas));
    }

    #[test]
    fn walltime_and_preemption_after_serving_are_not_failures() {
        for raw in ["TIMEOUT", "PREEMPTED", "NODE_FAIL", "FAILED"] {
            let mut c = Case::new(spec(1));
            c.replicas = vec![served("1", Some(Uuid::new_v4()), "n1", mins_ago(300))];
            c.jobs = HashMap::from([ended("1", raw)]);
            let a = c.plan();
            assert!(
                a.iter().any(|x| matches!(
                    x,
                    Action::MarkLost {
                        failed_launch: false,
                        ..
                    }
                )),
                "{raw}: {a:?}"
            );
        }
    }

    #[test]
    fn preemption_and_outside_cancels_before_serving_are_not_failures() {
        for raw in ["PREEMPTED", "NODE_FAIL", "CANCELLED"] {
            let mut c = Case::new(spec(1));
            c.replicas = vec![rv("pending", "1", None, 60)];
            c.jobs = HashMap::from([ended("1", raw)]);
            assert!(!is_failed_launch(&c.plan()[0], &c.replicas), "{raw}");
        }
        // A job gone without a trace while it never served does count.
        let mut c = Case::new(spec(1));
        c.replicas = vec![rv("starting", "1", None, 60)];
        assert!(is_failed_launch(&c.plan()[0], &c.replicas));
    }

    #[test]
    fn a_stopped_replica_ending_is_not_a_failure() {
        let mut c = Case::new(spec(1));
        let mut r = rv("pending", "1", None, 60);
        r.cancel_requested = true;
        c.replicas = vec![r];
        c.jobs = HashMap::from([ended("1", "CANCELLED")]);
        assert!(!is_failed_launch(&c.plan()[0], &c.replicas));
    }

    #[test]
    fn lost_rows_age_from_when_they_were_lost() {
        let mut c = Case::new(spec(0));
        let mut fresh = rv("lost", "1", None, 10_000);
        fresh.updated_secs = 30;
        let mut old = rv("lost", "2", None, 10_000);
        old.updated_secs = 901;
        c.replicas = vec![fresh, old];
        assert_eq!(
            c.plan(),
            vec![Action::Delete {
                replica_id: c.replicas[1].id
            }]
        );
    }

    #[test]
    fn paused_models_do_not_submit_until_the_pause_ends() {
        let mut s = spec(2);
        s.paused_until = Some(now() + chrono::Duration::minutes(5));
        assert_eq!(submits(&Case::new(s.clone()).plan()), 0);
        s.paused_until = Some(now() - chrono::Duration::seconds(1));
        assert_eq!(submits(&Case::new(s).plan()), 2);
    }

    #[test]
    fn backoff_ladder_is_5_15_30_60_then_hourly() {
        let steps: Vec<i64> = (1..=6).map(backoff_minutes).collect();
        assert_eq!(steps, vec![5, 15, 30, 60, 60, 60]);
    }

    #[test]
    fn failures_pause_at_the_limit_and_climb_the_ladder() {
        let fail = |how: &str| LaunchOutcome {
            failed: 1,
            last_failure: Some(how.into()),
            ..Default::default()
        };
        let b0 = ManagedBackoff::default();
        let b1 = next_backoff(&b0, 2, &fail("ended FAILED"), now()).unwrap();
        assert_eq!((b1.failed_launches, b1.backoff_step), (1, 0));
        assert!(b1.paused_until.is_none());
        let b2 = next_backoff(&b1, 2, &fail("ended FAILED (OutOfMemory)"), now()).unwrap();
        assert_eq!(b2.backoff_step, 1);
        assert_eq!(b2.paused_until, Some(now() + chrono::Duration::minutes(5)));
        let reason = b2.paused_reason.clone().unwrap();
        assert!(reason.starts_with("2 launches in a row"), "{reason}");
        assert!(reason.contains("OutOfMemory"), "{reason}");
        let b3 = next_backoff(&b2, 2, &fail("ended FAILED"), now()).unwrap();
        assert_eq!(b3.backoff_step, 2);
        assert_eq!(b3.paused_until, Some(now() + chrono::Duration::minutes(15)));
        // A launch that serves resets everything.
        let ok = LaunchOutcome {
            served: true,
            ..Default::default()
        };
        assert_eq!(
            next_backoff(&b3, 2, &ok, now()),
            Some(ManagedBackoff::default())
        );
        // Nothing happened: no write.
        assert_eq!(next_backoff(&b0, 2, &LaunchOutcome::default(), now()), None);
        // No limit: failures are counted but never pause.
        let b = next_backoff(&b0, 0, &fail("ended FAILED"), now()).unwrap();
        assert!(b.paused_until.is_none() && b.failed_launches == 1);
    }

    #[test]
    fn a_rejected_submit_backs_off_whatever_the_limit() {
        let out = LaunchOutcome {
            rejected: Some("Slurm rejected the job: Invalid account.".into()),
            ..Default::default()
        };
        let b = next_backoff(&ManagedBackoff::default(), 0, &out, now()).unwrap();
        assert_eq!(b.backoff_step, 1);
        assert_eq!(b.paused_until, Some(now() + chrono::Duration::minutes(5)));
        assert_eq!(
            b.paused_reason.as_deref(),
            Some("Slurm rejected the job: Invalid account.")
        );
        let b2 = next_backoff(&b, 0, &out, now()).unwrap();
        assert_eq!(b2.paused_until, Some(now() + chrono::Duration::minutes(15)));
    }

    // ---- startup timeout and stuck jobs ----

    #[test]
    fn a_server_that_never_becomes_healthy_is_cancelled_after_the_startup_timeout() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![rv("pending", "1", None, 4000)];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(59))]);
        assert!(c.plan().is_empty());
        c.jobs = HashMap::from([running("1", "n1", mins_ago(61))]);
        let a = c.plan();
        assert_eq!(
            cancel_of(&a, c.replicas[0].id),
            Some((CancelReason::StartupTimeout, true))
        );
        assert_eq!(submits(&a), 1, "a replacement is submitted");
        assert!(is_failed_launch(&a[0], &c.replicas));
        // 0 never gives up.
        c.spec.startup_timeout_mins = 0;
        assert!(c.plan().is_empty());
    }

    #[test]
    fn a_requeued_replica_that_served_is_not_a_failed_launch_on_timeout() {
        let mut c = Case::new(spec(1));
        let mut r = served("1", None, "n1", mins_ago(500));
        r.state = "pending".into();
        c.replicas = vec![r];
        c.jobs = HashMap::from([running("1", "n2", mins_ago(61))]);
        let a = c.plan();
        assert_eq!(
            cancel_of(&a, c.replicas[0].id),
            Some((CancelReason::StartupTimeout, true))
        );
        assert!(!is_failed_launch(&a[0], &c.replicas));
    }

    #[test]
    fn a_held_job_is_flagged_then_cancelled_after_the_startup_timeout() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![rv("pending", "1", None, 60)];
        let (k, mut j) = job("1", JobState::Pending, &[]);
        j.reason = Some("JobHeldAdmin".into());
        c.jobs = HashMap::from([(k, j)]);
        let a = c.plan();
        assert_eq!(submits(&a), 0);
        let d = detail_of(&a, c.replicas[0].id).unwrap();
        assert!(d.contains("holding") && d.contains("JobHeldAdmin"), "{d}");
        c.waited.insert(c.replicas[0].id, 3600);
        let a = c.plan();
        assert_eq!(
            cancel_of(&a, c.replicas[0].id),
            Some((CancelReason::Stuck, true))
        );
        assert_eq!(submits(&a), 1);
    }

    #[test]
    fn stuck_reasons_are_classified() {
        let with = |reason: &str| {
            let (_, mut j) = job("1", JobState::Pending, &[]);
            j.reason = Some(reason.into());
            stuck_reason(&j)
        };
        assert_eq!(with("JobHeldUser"), Some(Stuck::Held));
        assert_eq!(with("launch failed requeued held"), Some(Stuck::Held));
        assert_eq!(with("BadConstraints"), Some(Stuck::NeverRuns));
        assert_eq!(with("PartitionConfig"), Some(Stuck::NeverRuns));
        assert_eq!(
            with("QOSMaxWallDurationPerJobLimit"),
            Some(Stuck::NeverRuns)
        );
        assert_eq!(with("QOSMaxGRESPerJob"), Some(Stuck::NeverRuns));
        assert_eq!(with("QOSMaxJobsPerUserLimit"), Some(Stuck::Limited));
        assert_eq!(with("Resources"), None);
        assert_eq!(with("Priority"), None);
    }

    // ---- handoff and replacement ----

    fn near_limit(id: &str, node: &str, ends_in_mins: i64) -> (String, JobInfo) {
        let (k, mut j) = running(id, node, mins_ago(240 - ends_in_mins));
        j.time_limit_mins = Some(240);
        j.end_time = Some(now() + chrono::Duration::minutes(ends_in_mins));
        (k, j)
    }

    #[test]
    fn a_job_near_its_time_limit_gets_a_successor_first() {
        let mut c = Case::new(spec(1));
        let mut old = served("1", Some(Uuid::new_v4()), "n1", mins_ago(230));
        old.job_ends_at = Some(now() + chrono::Duration::minutes(10));
        c.replicas = vec![old];
        c.jobs = HashMap::from([near_limit("1", "n1", 10)]);
        c.health.insert(c.replicas[0].id, 8000);
        let a = c.plan();
        assert_eq!(submits(&a), 1);
        assert!(cancel_of(&a, c.replicas[0].id).is_none(), "kept serving");
        let d = detail_of(&a, c.replicas[0].id).unwrap();
        assert!(d.contains("12:10 UTC"), "{d}");
    }

    #[test]
    fn the_old_replica_drains_once_its_successor_serves() {
        let mut c = Case::new(spec(1));
        let old = served("1", Some(Uuid::new_v4()), "n1", mins_ago(235));
        let new = served("2", Some(Uuid::new_v4()), "n2", mins_ago(5));
        c.replicas = vec![old, new];
        c.jobs = HashMap::from([near_limit("1", "n1", 5), running("2", "n2", mins_ago(5))]);
        let a = c.plan();
        assert_eq!(submits(&a), 0);
        assert_eq!(
            cancel_of(&a, c.replicas[0].id),
            Some((CancelReason::Replaced, false))
        );
        assert!(cancel_of(&a, c.replicas[1].id).is_none());
    }

    #[test]
    fn the_old_replica_drains_on_the_tick_its_successor_is_promoted() {
        let mut c = Case::new(spec(1));
        let old = served("1", Some(Uuid::new_v4()), "n1", mins_ago(235));
        let new = rv("pending", "2", None, 600);
        c.replicas = vec![old, new];
        c.jobs = HashMap::from([near_limit("1", "n1", 5), running("2", "n2", mins_ago(5))]);
        c.health.insert(c.replicas[1].id, 8008);
        let a = c.plan();
        let promote = a
            .iter()
            .position(|x| matches!(x, Action::Promote { .. }))
            .unwrap();
        let drain = a
            .iter()
            .position(|x| matches!(x, Action::Cancel { .. }))
            .unwrap();
        assert!(promote < drain, "the successor is registered first: {a:?}");
        assert_eq!(submits(&a), 0);
    }

    #[test]
    fn no_handoff_when_the_limit_is_too_short_or_off() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![served("1", Some(Uuid::new_v4()), "n1", mins_ago(15))];
        let (k, mut j) = running("1", "n1", mins_ago(15));
        j.time_limit_mins = Some(20);
        j.end_time = Some(now() + chrono::Duration::minutes(5));
        c.replicas[0].job_ends_at = j.end_time;
        c.jobs = HashMap::from([(k, j)]);
        assert!(c.plan().is_empty());
        c.jobs = HashMap::from([near_limit("1", "n1", 5)]);
        c.replicas[0].job_ends_at = Some(now() + chrono::Duration::minutes(5));
        c.replicas[0].job_started_at = Some(mins_ago(235));
        c.spec.handoff_mins = 0;
        assert!(c.plan().is_empty(), "{:?}", c.plan());
    }

    #[test]
    fn a_roll_replaces_one_replica_at_a_time() {
        let mut c = Case::new(spec(2));
        let mut a1 = served("1", Some(Uuid::new_v4()), "n1", mins_ago(60));
        a1.replace_requested = true;
        a1.age_secs = 7200;
        let mut a2 = served("2", Some(Uuid::new_v4()), "n2", mins_ago(30));
        a2.replace_requested = true;
        c.replicas = vec![a1, a2];
        c.jobs = HashMap::from([
            running("1", "n1", mins_ago(60)),
            running("2", "n2", mins_ago(30)),
        ]);
        let a = c.plan();
        assert_eq!(submits(&a), 1, "{a:?}");
        assert!(
            detail_of(&a, c.replicas[0].id).is_some(),
            "the oldest goes first"
        );
        assert!(detail_of(&a, c.replicas[1].id).is_none());
    }

    #[test]
    fn a_replacement_that_never_serves_still_restarts_after_the_timeout() {
        let mut c = Case::new(spec(1));
        let mut old = served("1", Some(Uuid::new_v4()), "n1", mins_ago(120));
        old.replace_requested = true;
        c.replicas = vec![old, rv("pending", "2", None, 60)];
        let (k, j) = job("2", JobState::Pending, &[]);
        c.jobs = HashMap::from([running("1", "n1", mins_ago(120)), (k, j)]);
        assert!(cancel_of(&c.plan(), c.replicas[0].id).is_none());
        c.waited.insert(c.replicas[0].id, 3600);
        assert_eq!(
            cancel_of(&c.plan(), c.replicas[0].id),
            Some((CancelReason::Replaced, false))
        );
    }

    #[test]
    fn replacing_a_replica_that_does_not_serve_yet_cancels_it_at_once() {
        let mut c = Case::new(spec(1));
        let mut r = rv("pending", "1", None, 60);
        r.replace_requested = true;
        c.replicas = vec![r];
        c.jobs = HashMap::from([job("1", JobState::Pending, &[])]);
        let a = c.plan();
        assert_eq!(
            cancel_of(&a, c.replicas[0].id),
            Some((CancelReason::OperatorRestart, true))
        );
        assert_eq!(submits(&a), 1);
    }

    #[test]
    fn stopping_a_serving_replica_drains_it_first() {
        let mut c = Case::new(spec(1));
        let mut r = served("1", Some(Uuid::new_v4()), "n1", mins_ago(30));
        r.cancel_requested = true;
        c.replicas = vec![r];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(30))]);
        let a = c.plan();
        assert_eq!(
            cancel_of(&a, c.replicas[0].id),
            Some((CancelReason::OperatorRestart, false))
        );
        assert_eq!(submits(&a), 1);
    }

    // ---- scale-down and drain ----

    #[test]
    fn scale_down_keeps_the_oldest_queued_job_and_drains_serving_gracefully() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![
            rv("pending", "old", None, 21_600),
            rv("pending", "mid", None, 60),
            rv("pending", "new", None, 30),
        ];
        c.jobs = HashMap::from([
            job("old", JobState::Pending, &[]),
            job("mid", JobState::Pending, &[]),
            job("new", JobState::Pending, &[]),
        ]);
        let a = c.plan();
        assert!(cancel_of(&a, c.replicas[0].id).is_none(), "{a:?}");
        assert_eq!(
            cancel_of(&a, c.replicas[1].id),
            Some((CancelReason::ScaleDown, true))
        );
        assert_eq!(
            cancel_of(&a, c.replicas[2].id),
            Some((CancelReason::ScaleDown, true))
        );

        // Serving replicas go last, oldest first, with a graceful drain.
        let mut c = Case::new(spec(1));
        let mut older = served("a", Some(Uuid::new_v4()), "n1", mins_ago(300));
        older.age_secs = 18_000;
        let newer = served("b", Some(Uuid::new_v4()), "n2", mins_ago(10));
        c.replicas = vec![older, newer, rv("starting", "c", None, 120)];
        c.jobs = HashMap::from([
            running("a", "n1", mins_ago(300)),
            running("b", "n2", mins_ago(10)),
            running("c", "n3", mins_ago(1)),
        ]);
        let a = c.plan();
        assert_eq!(
            cancel_of(&a, c.replicas[2].id),
            Some((CancelReason::ScaleDown, true))
        );
        assert_eq!(
            cancel_of(&a, c.replicas[0].id),
            Some((CancelReason::ScaleDown, false))
        );
        assert!(cancel_of(&a, c.replicas[1].id).is_none());
    }

    #[test]
    fn a_requeued_replica_picked_for_scale_down_is_cancelled_not_demoted() {
        let ep = Uuid::new_v4();
        let mut c = Case::new(spec(0));
        c.replicas = vec![served("1", Some(ep), "n1", mins_ago(30))];
        c.jobs = HashMap::from([job("1", JobState::Pending, &[])]);
        assert_eq!(
            c.plan(),
            vec![Action::Cancel {
                replica_id: c.replicas[0].id,
                job_id: "1".into(),
                endpoint_id: Some(ep),
                reason: CancelReason::ScaleDown,
                cancel_now: true,
            }]
        );
    }

    #[test]
    fn a_removed_model_drains_everything() {
        let mut s = spec(0);
        s.removed = true;
        let mut c = Case::new(s);
        c.replicas = vec![served("1", Some(Uuid::new_v4()), "n1", mins_ago(30))];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(30))]);
        assert_eq!(
            cancel_of(&c.plan(), c.replicas[0].id),
            Some((CancelReason::Removed, false))
        );
    }

    #[test]
    fn draining_replicas_are_cancelled_after_the_grace_and_deleted_when_gone() {
        let mut c = Case::new(spec(0));
        let mut fresh = rv("draining", "1", None, 600);
        fresh.updated_secs = 10;
        let mut due = rv("draining", "2", None, 600);
        due.updated_secs = DRAIN_GRACE_SECS;
        c.replicas = vec![fresh, due, rv("draining", "3", None, 600)];
        c.jobs = HashMap::from([
            running("1", "n1", mins_ago(30)),
            running("2", "n2", mins_ago(30)),
            ended("3", "CANCELLED"),
        ]);
        let a = c.plan();
        assert_eq!(
            a,
            vec![
                Action::CancelJob {
                    replica_id: c.replicas[1].id,
                    job_id: "2".into()
                },
                Action::Delete {
                    replica_id: c.replicas[2].id
                },
            ]
        );
    }

    // ---- self-heal ----

    fn ep(id: Uuid, status: &str, failures: i64, checked_secs_ago: Option<i64>) -> EndpointView {
        EndpointView {
            id,
            name: String::new(),
            api_base: String::new(),
            priority: 100,
            weight: 100,
            enabled: true,
            health_status: Some(status.into()),
            consecutive_failures: failures,
            checked_secs_ago,
        }
    }

    #[test]
    fn probe_path_down_needs_two_probed_replicas_all_failing() {
        let a = rv("healthy", "1", None, 1);
        let b = rv("healthy", "2", None, 1);
        let jobs = HashMap::from([
            running("1", "n1", mins_ago(5)),
            running("2", "n2", mins_ago(5)),
        ]);
        let rs = vec![a.clone(), b.clone()];
        assert_eq!(probe_path_down(&rs, &jobs, &HashMap::new()), Some(2));
        assert_eq!(
            probe_path_down(&rs, &jobs, &HashMap::from([(a.id, 8000)])),
            None
        );
        assert_eq!(probe_path_down(&rs[..1], &jobs, &HashMap::new()), None);
    }

    #[test]
    fn self_heal_uses_only_fresh_gateway_verdicts() {
        let (e1, e2, e3) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
        let dead_vetoed = rv("healthy", "1", Some(e1), 1);
        let dead = rv("healthy", "2", Some(e2), 1);
        let zombie = rv("healthy", "3", Some(e3), 1);
        let stale = rv("healthy", "4", Some(Uuid::new_v4()), 1);
        let failures = HashMap::from([(dead_vetoed.id, 20), (dead.id, 20)]);
        let eps = vec![
            ep(e1, "healthy", 0, Some(60)),
            ep(e2, "healthy", 0, Some(GATEWAY_FRESH_SECS + 1)),
            ep(e3, "unhealthy", 2, Some(60)),
            ep(
                stale.endpoint_id.unwrap(),
                "unhealthy",
                5,
                Some(GATEWAY_FRESH_SECS + 1),
            ),
        ];
        let rs = vec![
            dead_vetoed.clone(),
            dead.clone(),
            zombie.clone(),
            stale.clone(),
        ];
        let h = self_heal_candidates(&rs, &failures, 20, &eps);
        assert_eq!(h.dead, HashSet::from([dead.id]));
        assert_eq!(h.zombie, HashSet::from([zombie.id]));
        let off = self_heal_candidates(&rs, &failures, 0, &eps);
        assert!(off.dead.is_empty() && off.zombie.is_empty());
    }

    #[test]
    fn a_dead_server_is_cancelled_when_another_serves_but_the_last_one_is_replaced_first() {
        let mut c = Case::new(spec(2));
        let a = served("1", Some(Uuid::new_v4()), "n1", mins_ago(30));
        let b = served("2", Some(Uuid::new_v4()), "n2", mins_ago(30));
        c.replicas = vec![a.clone(), b];
        c.jobs = HashMap::from([
            running("1", "n1", mins_ago(30)),
            running("2", "n2", mins_ago(30)),
        ]);
        c.heal.dead.insert(a.id);
        let acts = c.plan();
        assert_eq!(
            cancel_of(&acts, a.id),
            Some((CancelReason::ProbeFailed, true))
        );
        assert_eq!(submits(&acts), 1);

        let mut c = Case::new(spec(1));
        c.replicas = vec![a.clone()];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(30))]);
        c.heal.dead.insert(a.id);
        let acts = c.plan();
        assert!(
            cancel_of(&acts, a.id).is_none(),
            "the last replica keeps its slot"
        );
        assert_eq!(submits(&acts), 1, "a successor starts first");
    }

    #[test]
    fn self_heal_cancels_at_most_one_per_tick() {
        let mut c = Case::new(spec(3));
        let rs: Vec<ReplicaView> = (1..=3)
            .map(|i| {
                served(
                    &i.to_string(),
                    Some(Uuid::new_v4()),
                    &format!("n{i}"),
                    mins_ago(30),
                )
            })
            .collect();
        c.jobs = (1..=3)
            .map(|i| running(&i.to_string(), &format!("n{i}"), mins_ago(30)))
            .collect();
        c.heal.dead = HashSet::from([rs[0].id, rs[1].id]);
        c.replicas = rs;
        let a = c.plan();
        assert_eq!(
            count(&a, |x| matches!(x, Action::Cancel { .. })),
            1,
            "{a:?}"
        );
    }

    #[test]
    fn a_zombie_is_replaced_surge_first() {
        let mut c = Case::new(spec(1));
        c.replicas = vec![served("1", Some(Uuid::new_v4()), "n1", mins_ago(30))];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(30))]);
        c.heal.zombie.insert(c.replicas[0].id);
        let a = c.plan();
        assert_eq!(submits(&a), 1);
        assert!(cancel_of(&a, c.replicas[0].id).is_none());
    }

    #[test]
    fn probe_failure_counter_increments_on_miss_and_decays_on_pass() {
        let r = rv("healthy", "1", None, 1);
        let jobs = HashMap::from([running("1", "n1", mins_ago(5))]);
        let mut counts = HashMap::new();
        for _ in 0..3 {
            update_probe_failures(
                std::slice::from_ref(&r),
                &jobs,
                &HashMap::new(),
                &mut counts,
            );
        }
        assert_eq!(counts.get(&r.id), Some(&3));
        let pass = HashMap::from([(r.id, 8000u16)]);
        update_probe_failures(std::slice::from_ref(&r), &jobs, &pass, &mut counts);
        assert_eq!(counts.get(&r.id), Some(&2));
        update_probe_failures(std::slice::from_ref(&r), &jobs, &pass, &mut counts);
        update_probe_failures(std::slice::from_ref(&r), &jobs, &pass, &mut counts);
        assert!(!counts.contains_key(&r.id));
    }

    #[test]
    fn flapping_replica_never_reaches_restart_threshold() {
        let r = rv("healthy", "1", None, 1);
        let jobs = HashMap::from([running("1", "n1", mins_ago(5))]);
        let pass = HashMap::from([(r.id, 8000u16)]);
        let mut counts = HashMap::new();
        for i in 0..100 {
            let h = if i % 2 == 0 {
                HashMap::new()
            } else {
                pass.clone()
            };
            update_probe_failures(std::slice::from_ref(&r), &jobs, &h, &mut counts);
            assert!(counts.get(&r.id).copied().unwrap_or(0) <= 1);
        }
    }

    #[test]
    fn probe_failure_counter_ignores_unjudgeable_replicas() {
        let loading = rv("starting", "1", None, 1);
        let queued = rv("healthy", "2", None, 1);
        let jobs = HashMap::from([
            running("1", "n1", mins_ago(5)),
            job("2", JobState::Pending, &[]),
        ]);
        let mut counts = HashMap::new();
        update_probe_failures(&[loading, queued], &jobs, &HashMap::new(), &mut counts);
        assert!(counts.is_empty());
    }

    // ---- phantom endpoints ----

    #[test]
    fn endpoints_left_behind_by_failed_deletes_are_removed() {
        let prefix = "obleth-m-".to_string();
        let linked = Uuid::new_v4();
        let mut c = Case::new(spec(1));
        let live = served("1", Some(linked), "n1", mins_ago(30));
        let gone_replica = Uuid::new_v4();
        let leftover = Uuid::new_v4();
        let duplicate = Uuid::new_v4();
        let operator = Uuid::new_v4();
        let named = |id: Uuid, rid: Uuid| EndpointView {
            name: format!("{prefix}{rid}"),
            ..ep(id, "healthy", 0, None)
        };
        c.endpoints = Some((
            vec![
                named(linked, live.id),
                named(duplicate, live.id),
                named(leftover, gone_replica),
                EndpointView {
                    name: "my-own-endpoint".into(),
                    ..ep(operator, "healthy", 0, None)
                },
            ],
            prefix,
        ));
        c.replicas = vec![live];
        c.jobs = HashMap::from([running("1", "n1", mins_ago(30))]);
        let a = c.plan();
        let mut deleted: Vec<Uuid> = a
            .iter()
            .filter_map(|x| match x {
                Action::DeleteEndpoint { endpoint_id } => Some(*endpoint_id),
                _ => None,
            })
            .collect();
        deleted.sort();
        let mut want = vec![duplicate, leftover];
        want.sort();
        assert_eq!(deleted, want);
    }
}
