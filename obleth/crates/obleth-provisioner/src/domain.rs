use uuid::Uuid;

/// Coarse, version-agnostic view of a Slurm job's state. The slurm adapter maps
/// Slurm's many job_state strings onto these three.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    /// Queued/configuring; not yet serving.
    Pending,
    /// Allocated and running (vLLM may still be booting — health gates that).
    Running,
    /// Terminal/absent: completed, failed, cancelled, preempted, timed out, or
    /// not found in slurmrestd at all.
    Gone,
}

/// What slurmrestd tells us about one job we own.
#[derive(Debug, Clone)]
pub struct JobInfo {
    pub job_id: String,
    pub state: JobState,
    /// Allocated node hostnames (first is used for the endpoint URL in v1).
    pub nodes: Vec<String>,
    /// Raw slurmrestd job_state string (e.g. "RUNNING", "PENDING", "FAILED").
    pub raw_state: String,
    /// slurmrestd state_reason (e.g. "Resources", "Priority"); `None`/"None" omitted.
    pub reason: Option<String>,
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
    /// Memory per node in megabytes (slurm `--mem`); `None` leaves it default.
    pub mem_mb: Option<i64>,
    /// Directory for stdout/stderr files; empty means Slurm default.
    pub log_output_dir: String,
    /// Shell script the job runs (apptainer exec ... <launch_command>).
    pub script: String,
}

/// Minimal projection of a `ModelReplica` (Plan 1) the planner reasons over.
#[derive(Debug, Clone)]
pub struct ReplicaView {
    pub id: Uuid,
    pub model_id: Uuid,
    pub slurm_job_id: String,
    pub state: String, // pending|starting|healthy|draining|lost
    pub endpoint_id: Option<Uuid>,
    pub age_secs: i64, // now - created_at, for GC + cancel ordering
    pub port_base: i64,
    /// Current replica message, so the tick can diff before re-patching.
    pub last_message: Option<String>,
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
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct PartitionInfo {
    pub name: String,
    pub nodes: Vec<String>,
    pub default_time: Option<String>,
    pub max_time: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct NodeInfo {
    pub name: String,
    pub partitions: Vec<String>,
    pub gres: String,
    pub cpus: Option<i64>,
    pub real_memory_mb: Option<i64>,
    pub features: Vec<String>,
}

/// Actions the executor applies. The planner emits these; it performs no I/O.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Submit one new job and record a replica row.
    Submit,
    /// Job is running + healthy: register endpoint + mark replica healthy.
    Promote { replica_id: Uuid, api_base: String },
    /// Job vanished/terminal: deregister endpoint (if any) + mark replica lost.
    MarkLost { replica_id: Uuid, endpoint_id: Option<Uuid> },
    /// Excess/restarted replica: deregister its endpoint, cancel its job, mark
    /// draining. `endpoint_id` is the linked endpoint to remove from rotation up
    /// front so the proxy stops routing to the dying backend immediately.
    Cancel { replica_id: Uuid, job_id: String, endpoint_id: Option<Uuid> },
    /// GC a long-dead `lost` replica row.
    Delete { replica_id: Uuid },
}
