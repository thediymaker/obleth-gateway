use uuid::Uuid;

/// Coarse, version-agnostic view of a Slurm job's state. The slurm adapter maps
/// Slurm's many job_state strings onto these.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JobState {
    /// Queued/configuring, or put back in the queue (requeued, held); not
    /// serving.
    #[default]
    Pending,
    /// Allocated and running (vLLM may still be booting — health gates that).
    Running,
    /// Terminal/absent: completed, failed, cancelled, preempted, timed out, or
    /// not found in slurmrestd at all.
    Gone,
    /// A state this build does not recognise. Nothing should be torn down on
    /// it: the replica is left as it is until the state is one of the above.
    Unknown,
}

/// What slurmrestd tells us about one job we own.
#[derive(Debug, Clone, Default)]
pub struct JobInfo {
    pub job_id: String,
    pub state: JobState,
    /// Allocated node hostnames, with the hostlist expanded
    /// (`gpu[01-02]` is `gpu01`, `gpu02`).
    pub nodes: Vec<String>,
    /// The job's base Slurm state (e.g. "RUNNING", "PENDING", "FAILED").
    pub raw_state: String,
    /// slurmrestd state_reason (e.g. "Resources", "Priority"); `None`/"None" omitted.
    pub reason: Option<String>,
    /// Every state flag Slurm reported, e.g. `["PENDING", "REQUEUED"]`.
    pub state_flags: Vec<String>,
    /// The node running the batch script, where the server listens.
    pub batch_host: Option<String>,
    /// Job name and owner, when reported.
    pub name: Option<String>,
    pub user: Option<String>,
    /// The port the job wrote into its comment (`obleth-port=N`).
    pub reported_port: Option<i64>,
    /// How many times Slurm has requeued the job.
    pub restart_count: i64,
    /// When the job started running (its latest start after a requeue).
    pub start_time: Option<chrono::DateTime<chrono::Utc>>,
    /// While running: when it will reach its time limit.
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,
    /// Time limit in minutes; `None` when unlimited or not reported.
    pub time_limit_mins: Option<i64>,
    /// True when the job has no time limit.
    pub time_limit_unlimited: bool,
    /// When the job was submitted, when reported (accounting records).
    pub submit_time: Option<chrono::DateTime<chrono::Utc>>,
}

impl JobInfo {
    /// The host to probe and register: the batch host (where the batch
    /// script, and so the server, runs), else the first allocated node.
    pub fn probe_host(&self) -> Option<&str> {
        self.batch_host
            .as_deref()
            .or_else(|| self.nodes.first().map(String::as_str))
            .filter(|h| !h.is_empty())
    }

    /// True when Slurm put the job back in the queue (requeue or hold flag,
    /// or a restart count above zero while pending).
    pub fn is_requeued(&self) -> bool {
        self.state == JobState::Pending
            && (self.restart_count > 0
                || self
                    .state_flags
                    .iter()
                    .any(|f| f.starts_with("REQUEUE") || f == "SPECIAL_EXIT"))
    }
}

/// A version-agnostic job request, built from a ManagedModelSpec. The slurm
/// adapter renders this into the slurmrestd payload for the configured version.
#[derive(Debug, Clone)]
pub struct JobSubmit {
    pub name: String,
    pub partition: String,
    pub gres: String,
    pub nodes: i64,
    pub time_limit: Option<String>,
    pub account: Option<String>,
    pub qos: Option<String>,
    pub constraints: Option<String>,
    pub exclude: Option<String>,
    /// Slurm `--cpus-per-task`; `None` leaves it to the cluster default.
    pub cpus_per_task: Option<i64>,
    /// Memory per node in megabytes (slurm `--mem`); `None` leaves it default
    /// and `Some(0)` asks for all of each node's memory, as `--mem=0` does.
    /// The submit turns 0 into the partition's node size, because some
    /// clusters' submit plugins read 0 as unset and apply a small default.
    pub mem_mb: Option<i64>,
    /// Directory for stdout/stderr files; empty means Slurm default.
    pub log_output_dir: String,
    /// Shell script the job runs (the container command, image and <launch_command>).
    pub script: String,
}

/// Minimal projection of a `ModelReplica` the planner reasons over.
#[derive(Debug, Clone, Default)]
pub struct ReplicaView {
    pub id: Uuid,
    pub model_id: Uuid,
    pub slurm_job_id: String,
    pub state: String, // pending|starting|healthy|draining|lost
    pub endpoint_id: Option<Uuid>,
    pub age_secs: i64, // now - created_at, for cancel ordering
    /// now - updated_at. For a `lost` row this is how long ago it was lost
    /// (nothing patches a lost row), and for a `draining` row how long it has
    /// been draining (the status annotation skips draining rows).
    pub updated_secs: i64,
    pub port_base: i64,
    /// Current replica message, so the tick can diff before re-patching.
    pub last_message: Option<String>,
    /// The operator asked to stop this replica: drain and cancel it, and let
    /// the replica count bring in a new one.
    pub cancel_requested: bool,
    /// The operator asked to replace this replica without an outage: start a
    /// successor first and drain this one once the successor serves.
    pub replace_requested: bool,
    /// The Slurm start time the replica was promoted under (set at
    /// promotion, so `Some` also means the replica has served).
    pub job_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the running job reaches its time limit, as last recorded.
    pub job_ends_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The plain-sentence detail currently shown for the replica.
    pub state_detail: Option<String>,
    /// The port the server was found on (set at promotion).
    pub port: Option<i64>,
    /// Nodes recorded on the row (comma-separated): the allocation once the
    /// job ran, then the promoted host.
    pub nodes: Option<String>,
    /// Where the job's output goes, when known.
    pub log_path: Option<String>,
}

impl ReplicaView {
    /// True once the replica has been promoted at least once: it served.
    pub fn has_served(&self) -> bool {
        self.state == "healthy" || self.job_started_at.is_some() || self.port.is_some()
    }
}

/// One of a model's registered endpoints, as the gateway sees it — including
/// the gateway's own health verdict. The gateway's endpoint check is a real
/// 1-token inference, so it catches "zombie" servers that still answer
/// metadata GETs (which the provisioner's cheap health_path probe cannot):
/// an Ollama that lists its models instantly but hangs forever on a
/// completion passes the GET probe and fails the gateway check.
#[derive(Debug, Clone)]
pub struct EndpointView {
    pub id: Uuid,
    pub name: String,
    /// Registered upstream base URL (e.g. `http://10.0.0.7:8016/v1`). Used to
    /// detect when a name-based endpoint should be rewritten to its resolved IP.
    pub api_base: String,
    /// Priority / weight / enabled, carried so a convergence rewrite of
    /// `api_base` can PUT them back unchanged instead of clobbering operator edits.
    pub priority: i64,
    pub weight: i64,
    pub enabled: bool,
    /// Gateway-recorded health status ("healthy" | "unhealthy" | "degraded" |
    /// …); `None` when the gateway has never checked this endpoint.
    pub health_status: Option<String>,
    /// Consecutive failed gateway checks (reset on a passing one).
    pub consecutive_failures: i64,
    /// Seconds since the gateway last checked this endpoint; `None` if never.
    pub checked_secs_ago: Option<i64>,
}

/// Live, version-agnostic snapshot of what the configured Slurm cluster offers.
/// Powers the launcher's resource dropdowns. Every field is best-effort: a
/// version skew or permission gap yields empties, never an error.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct ClusterResources {
    pub partitions: Vec<PartitionInfo>,
    pub nodes: Vec<NodeInfo>,
    pub accounts: Vec<String>,
    pub qos: Vec<String>,
    /// The caller's associations, one per account (and partition, when the
    /// association names one), so the launcher can tell which accounts a
    /// partition takes before Slurm refuses the job.
    #[serde(default)]
    pub associations: Vec<AssociationInfo>,
    /// Why some or all of the above could not be read, as plain sentences;
    /// `None` when every read worked.
    #[serde(default)]
    pub error: Option<String>,
    /// True when some reads worked and others failed.
    #[serde(default)]
    pub partial: bool,
    /// The Slurm release, when an answer carried it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slurm_release: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct AssociationInfo {
    pub account: String,
    /// None when the association covers every partition.
    pub partition: Option<String>,
    pub qos: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PartitionInfo {
    pub name: String,
    pub nodes: Vec<String>,
    pub default_time: Option<String>,
    pub max_time: Option<String>,
    /// The partition's AllowAccounts; empty means any account.
    #[serde(default)]
    pub allowed_accounts: Vec<String>,
    /// The partition's DenyAccounts.
    #[serde(default)]
    pub denied_accounts: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NodeInfo {
    pub name: String,
    pub partitions: Vec<String>,
    pub gres: String,
    pub cpus: Option<i64>,
    pub real_memory_mb: Option<i64>,
    /// Memory Slurm keeps back for the node's own daemons (MemSpecLimit), so
    /// a job can ask for at most `real_memory_mb` minus this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub specialized_memory_mb: Option<i64>,
    pub features: Vec<String>,
    /// Slurm's state flags, e.g. `["IDLE"]` or `["MIXED", "DRAIN"]`. Empty
    /// when slurmrestd does not report them.
    pub state: Vec<String>,
    /// CPUs and memory already allocated to running jobs, when reported.
    pub alloc_cpus: Option<i64>,
    pub alloc_memory_mb: Option<i64>,
    /// Slurm's NodeAddr when it differs from the node name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub address: Option<String>,
}

/// Actions the executor applies. The planner emits these; it performs no I/O.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Submit one new job and record a replica row.
    Submit,
    /// Job is running + healthy: register endpoint + mark replica healthy,
    /// recording the Slurm times and the port it was promoted under.
    Promote {
        replica_id: Uuid,
        api_base: String,
        port: i64,
        job_started_at: Option<chrono::DateTime<chrono::Utc>>,
        job_ends_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    /// A promoted replica stopped serving where it was registered (Slurm
    /// requeued, suspended or restarted the job): remove its endpoint at once
    /// and put it back to `pending` until it serves again.
    Demote {
        replica_id: Uuid,
        endpoint_id: Option<Uuid>,
        detail: String,
    },
    /// Job vanished/terminal: deregister endpoint (if any) + mark replica lost.
    /// `raw_state`/`reason` are Slurm's terminal state and reason when the job
    /// is still visible in slurmrestd (e.g. `TIMEOUT`, `OUT_OF_MEMORY`); both
    /// `None` when the job is gone from the listing. See [`lost_message`].
    /// `failed_launch` is true when the job ended without ever serving and
    /// counts toward the model's failure limit.
    MarkLost {
        replica_id: Uuid,
        endpoint_id: Option<Uuid>,
        raw_state: Option<String>,
        reason: Option<String>,
        failed_launch: bool,
    },
    /// Take a replica out of service: deregister its endpoint first so the
    /// proxy stops routing to it, then mark it draining. With `cancel_now`
    /// the job is cancelled in the same step; otherwise it keeps running for
    /// the drain grace period (in-flight requests finish) and a later
    /// [`Action::CancelJob`] cancels it.
    Cancel {
        replica_id: Uuid,
        job_id: String,
        endpoint_id: Option<Uuid>,
        reason: CancelReason,
        cancel_now: bool,
    },
    /// A draining replica's grace period is over: cancel its job.
    CancelJob { replica_id: Uuid, job_id: String },
    /// GC a long-dead `lost` (or finished `draining`) replica row.
    Delete { replica_id: Uuid },
    /// Remove an endpoint this provisioner registered whose replica no longer
    /// serves (a delete that failed earlier, or a duplicate).
    DeleteEndpoint { endpoint_id: Uuid },
    /// Record what is happening with a replica. `detail` `Some("")` clears the
    /// plain-sentence detail; `None` leaves it. `job_ends_at` records a new
    /// time-limit end from Slurm.
    Annotate {
        replica_id: Uuid,
        detail: Option<String>,
        job_ends_at: Option<chrono::DateTime<chrono::Utc>>,
    },
}

impl Action {
    /// Short name for counting failures in tick warnings.
    pub fn kind(&self) -> &'static str {
        match self {
            Action::Submit => "job submit",
            Action::Promote { .. } => "replica promotion",
            Action::Demote { .. } => "replica demotion",
            Action::MarkLost { .. } => "replica update",
            Action::Cancel { .. } | Action::CancelJob { .. } => "job cancel",
            Action::Delete { .. } => "replica clean-up",
            Action::DeleteEndpoint { .. } => "endpoint removal",
            Action::Annotate { .. } => "replica update",
        }
    }
}

/// The replica message recorded with a `lost` state: `job ended: <STATE>`,
/// plus ` (<reason>)` when Slurm gave one, while the job is still visible in
/// slurmrestd; `job gone` once it is not. The store parses this into the
/// launch history's `end_state`/`end_reason`, so keep the two in step.
pub fn lost_message(raw_state: Option<&str>, reason: Option<&str>) -> String {
    match raw_state.map(str::trim).filter(|s| !s.is_empty()) {
        None => "job gone".to_string(),
        Some(state) => match reason.map(str::trim).filter(|r| !r.is_empty()) {
            Some(r) => format!("job ended: {state} ({r})"),
            None => format!("job ended: {state}"),
        },
    }
}

/// Why a replica's job is being cancelled — recorded on the replica row so the
/// dashboard's draining pill says what actually happened, not just "scaled down".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelReason {
    /// Over target: normal scale-down.
    ScaleDown,
    /// The operator stopped this replica (`cancel_requested` on the row), or
    /// asked to replace one that was not serving yet.
    OperatorRestart,
    /// Self-heal: the job still reports RUNNING but the replica failed
    /// consecutive health probes (zombie job) — cancel so the resubmit-to-target
    /// replaces it with a fresh one.
    ProbeFailed,
    /// The server did not become healthy within the startup timeout.
    StartupTimeout,
    /// The job sat pending with a reason Slurm will not clear by waiting
    /// (held, or a request the partition or QOS can never run).
    Stuck,
    /// A successor serves, so this replica (near its time limit, or flagged
    /// for replacement) is retired.
    Replaced,
    /// The deployment was paused or removed.
    Removed,
}

impl CancelReason {
    /// The replica's message when it starts draining.
    pub fn message(self) -> &'static str {
        match self {
            CancelReason::ScaleDown => "scaled down",
            CancelReason::OperatorRestart => "restart requested",
            CancelReason::ProbeFailed => "restarting: failed health probes while job running",
            CancelReason::StartupTimeout => "cancelled: server did not become healthy in time",
            CancelReason::Stuck => "cancelled: job could not start",
            CancelReason::Replaced => "replaced: a successor is serving",
            CancelReason::Removed => "stopping: deployment paused or removed",
        }
    }

    /// A plain sentence for the dashboard while the replica drains.
    pub fn detail(self) -> &'static str {
        match self {
            CancelReason::ScaleDown => {
                "The replica count went down, so obleth stops this replica after its requests finish."
            }
            CancelReason::OperatorRestart => "Stopping this replica as requested.",
            CancelReason::ProbeFailed => {
                "The server stopped answering health checks while its job kept running, so obleth cancelled the job. A new replica replaces it."
            }
            CancelReason::StartupTimeout => {
                "The server did not become healthy within the startup timeout, so obleth cancelled the job. Check the job's output for the cause."
            }
            CancelReason::Stuck => {
                "Slurm could not start this job as asked, so obleth cancelled it. Check the partition, account, time limit and resources on the deployment."
            }
            CancelReason::Replaced => {
                "A replacement is serving, so obleth stops this replica after its requests finish."
            }
            CancelReason::Removed => {
                "The deployment was paused or removed, so obleth stops this replica after its requests finish."
            }
        }
    }

    /// True when the cancelled job counts as a failed launch.
    pub fn is_failed_launch(self) -> bool {
        matches!(self, CancelReason::StartupTimeout | CancelReason::Stuck)
    }
}
