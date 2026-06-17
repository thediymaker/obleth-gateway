use crate::domain::{JobInfo, JobState, JobSubmit};
use async_trait::async_trait;
use obleth_config::ManagedModelSpec;

#[async_trait]
pub trait SlurmClient: Send + Sync {
    /// Submit a job; return its Slurm job id.
    async fn submit(&self, job: &JobSubmit) -> anyhow::Result<String>;
    async fn cancel(&self, job_id: &str) -> anyhow::Result<()>;
    /// All jobs whose name starts with the gateway's prefix.
    async fn list_owned_jobs(&self, name_prefix: &str) -> anyhow::Result<Vec<JobInfo>>;
}

/// Build a version-agnostic JobSubmit from a model's spec. The job is named
/// `{prefix}{model_name}` so we can find our own jobs. The script runs the
/// operator's launch command inside their apptainer image — nothing about a
/// specific cluster is assumed.
pub fn job_submit_from_spec(
    spec: &ManagedModelSpec,
    model_name: &str,
    name_prefix: &str,
) -> JobSubmit {
    let preamble = spec.preamble.trim();
    let preamble_block = if preamble.is_empty() {
        String::new()
    } else {
        format!("{preamble}\n")
    };
    // `launch_command` (and `preamble`) are intentionally raw shell: operators
    // write real bash here (env var expansion, pipes, etc.) and already have
    // full script control via `preamble`, gated by the same admin token as
    // this endpoint. `image` has no such reason to contain shell syntax, so
    // quote it to stop a stray space/glob/metacharacter from corrupting the
    // apptainer invocation.
    let script = format!(
        "#!/bin/bash\nset -euo pipefail\n{preamble_block}apptainer exec --nv {image} {cmd}\n",
        image = shell_quote(&spec.image),
        cmd = spec.launch_command,
    );
    JobSubmit {
        name: format!("{name_prefix}{model_name}"),
        partition: spec.partition.clone(),
        gres: spec.gres.clone(),
        nodes: spec.nodes.max(1),
        time_limit: spec.time_limit.clone(),
        account: spec.account.clone(),
        qos: spec.qos.clone(),
        constraints: spec.constraints.clone(),
        exclude: spec.exclude.clone(),
        log_output_dir: spec.log_output_dir.clone(),
        script,
    }
}

/// POSIX single-quote a value for embedding in the generated bash script.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Map a Slurm job_state string (any version) onto our coarse JobState.
pub fn map_job_state(s: &str) -> JobState {
    match s.to_ascii_uppercase().as_str() {
        "PENDING" | "CONFIGURING" | "SUSPENDED" => JobState::Pending,
        "RUNNING" | "COMPLETING" => JobState::Running,
        _ => JobState::Gone, // COMPLETED/FAILED/CANCELLED/PREEMPTED/TIMEOUT/NODE_FAIL/...
    }
}

pub struct Slurmrestd {
    http: reqwest::Client,
    base: String,
    version: String,
    user: String,
    jwt: String,
}

impl Slurmrestd {
    /// Build a client from connection settings fetched at runtime (the slurm
    /// connection details now live in system-wide settings, not env vars).
    /// Takes a shared `reqwest::Client` (cheap to clone — internally `Arc`'d)
    /// rather than building its own connection pool per tick.
    pub fn new(http: reqwest::Client, url: &str, version: &str, user: &str, jwt: &str) -> Self {
        Self {
            http,
            base: url.trim_end_matches('/').to_string(),
            version: version.to_string(),
            user: user.to_string(),
            jwt: jwt.to_string(),
        }
    }
    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.header("X-SLURM-USER-NAME", &self.user)
            .header("X-SLURM-USER-TOKEN", &self.jwt)
    }
}

#[async_trait]
impl SlurmClient for Slurmrestd {
    async fn submit(&self, job: &JobSubmit) -> anyhow::Result<String> {
        // NOTE: verify this body against your slurmrestd version's schema
        // (`/openapi/v3`). Fields below are common to v0.0.39/40. Optional
        // fields are omitted when None so they don't override cluster defaults.
        let mut spec = serde_json::json!({
            "name": job.name,
            "partition": job.partition,
            "nodes": job.nodes.to_string(),
            "tasks": 1,
            "current_working_directory": "/tmp",
            // slurmrestd requires a non-empty environment. Use a broad PATH that
            // covers where apptainer/singularity typically live across clusters
            // (/usr/local/bin, /opt, sbin dirs) rather than a minimal one that
            // would fail if the launcher isn't under /usr/bin. Operators whose
            // cluster needs more can prepend it inside launch_command.
            "environment": {
                "PATH": "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/apptainer/bin:/opt/singularity/bin"
            },
        });
        let m = spec.as_object_mut().unwrap();
        if !job.gres.is_empty() { m.insert("tres_per_node".into(), serde_json::json!(format!("gres/{}", job.gres))); }
        if let Some(t) = &job.time_limit { m.insert("time_limit".into(), serde_json::json!(t)); }
        if let Some(a) = &job.account { m.insert("account".into(), serde_json::json!(a)); }
        if let Some(q) = &job.qos { m.insert("qos".into(), serde_json::json!(q)); }
        if let Some(c) = &job.constraints { m.insert("constraints".into(), serde_json::json!(c)); }
        if let Some(e) = &job.exclude { m.insert("excluded_nodes".into(), serde_json::json!(e)); }
        if !job.log_output_dir.is_empty() {
            let dir = job.log_output_dir.trim_end_matches('/');
            m.insert("standard_output".into(), serde_json::json!(format!("{dir}/{}-%j.out", job.name)));
            m.insert("standard_error".into(), serde_json::json!(format!("{dir}/{}-%j.err", job.name)));
        }

        let body = serde_json::json!({ "job": spec, "script": job.script });
        let url = format!("{}/slurm/{}/job/submit", self.base, self.version);
        let resp = self.auth(self.http.post(&url)).json(&body).send().await?;
        let status = resp.status();
        let v: serde_json::Value = resp.json().await?;
        if !status.is_success() {
            anyhow::bail!("slurmrestd submit failed ({status}): {v}");
        }
        let job_id = v.get("job_id")
            .and_then(|j| j.as_i64().map(|n| n.to_string()).or_else(|| j.as_str().map(String::from)))
            .ok_or_else(|| anyhow::anyhow!("no job_id in submit response: {v}"))?;
        Ok(job_id)
    }

    async fn cancel(&self, job_id: &str) -> anyhow::Result<()> {
        let url = format!("{}/slurm/{}/job/{}", self.base, self.version, job_id);
        let resp = self.auth(self.http.delete(&url)).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("slurmrestd cancel {job_id} failed: {}", resp.status());
        }
        Ok(())
    }

    async fn list_owned_jobs(&self, name_prefix: &str) -> anyhow::Result<Vec<JobInfo>> {
        let url = format!("{}/slurm/{}/jobs", self.base, self.version);
        let resp = self.auth(self.http.get(&url)).send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("slurmrestd jobs failed: {}", resp.status());
        }
        let v: serde_json::Value = resp.json().await?;
        let mut out = Vec::new();
        for j in v.get("jobs").and_then(|x| x.as_array()).cloned().unwrap_or_default() {
            let name = j.get("name").and_then(|x| x.as_str()).unwrap_or_default();
            if !name.starts_with(name_prefix) { continue; }
            let job_id = j.get("job_id")
                .and_then(|x| x.as_i64().map(|n| n.to_string()).or_else(|| x.as_str().map(String::from)))
                .unwrap_or_default();
            if job_id.is_empty() {
                tracing::warn!(job_name = name, "slurmrestd job missing job_id; skipping");
                continue;
            }
            // job_state may be a string or array of strings depending on version.
            let state_str = match j.get("job_state") {
                Some(serde_json::Value::String(s)) => s.clone(),
                Some(serde_json::Value::Array(a)) => a.first().and_then(|x| x.as_str()).unwrap_or("").to_string(),
                _ => String::new(),
            };
            let nodes = j.get("nodes").and_then(|x| x.as_str()).unwrap_or_default();
            out.push(JobInfo {
                job_id,
                state: map_job_state(&state_str),
                nodes: expand_nodelist(nodes),
            });
        }
        Ok(out)
    }
}

/// Minimal Slurm nodelist expansion. v1 supports the common comma-separated and
/// single-host cases; bracket ranges (`n[1-3]`) fall back to the raw string as a
/// single entry (the health probe still works for single-node jobs, which is the
/// v1 target). Documented limitation; revisit for multi-node fan-out.
pub fn expand_nodelist(s: &str) -> Vec<String> {
    if s.is_empty() { return vec![]; }
    if s.contains('[') { return vec![s.to_string()]; }
    s.split(',').filter(|x| !x.is_empty()).map(|x| x.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ManagedModelSpec {
        ManagedModelSpec {
            model_id: uuid::Uuid::new_v4(),
            enabled: true,
            partition: "gpu-preempt".into(),
            gres: "gpu:h100:2".into(),
            nodes: 1,
            constraints: None, exclude: None, account: None, qos: None,
            time_limit: Some("12:00:00".into()),
            image: "vllm.sif".into(),
            preamble: String::new(),
            log_output_dir: String::new(),
            launch_command: "vllm serve nemotron --port 8000".into(),
            serving_port: 8000,
            health_path: "/health".into(),
            target_replicas: 2,
            max_job_failures: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn builder_is_generic_and_tags_name() {
        let j = job_submit_from_spec(&spec(), "nemotron", "obleth-");
        assert_eq!(j.name, "obleth-nemotron");
        assert_eq!(j.partition, "gpu-preempt");
        assert!(j.script.contains("apptainer exec --nv 'vllm.sif' vllm serve nemotron"));
    }

    #[test]
    fn preamble_injected_before_apptainer_when_set() {
        let mut s = spec();
        s.preamble = "module load apptainer/1.3.4".into();
        let j = job_submit_from_spec(&s, "nemotron", "obleth-");
        let lines: Vec<&str> = j.script.lines().collect();
        let apptainer_idx = lines.iter().position(|l| l.contains("apptainer exec")).unwrap();
        let module_idx = lines.iter().position(|l| l.contains("module load")).unwrap();
        assert!(module_idx < apptainer_idx, "preamble must come before apptainer exec");
    }

    #[test]
    fn empty_preamble_produces_no_extra_line() {
        let j = job_submit_from_spec(&spec(), "nemotron", "obleth-");
        assert!(!j.script.contains("module"), "no preamble lines when preamble is empty");
        assert!(j.script.contains("apptainer exec --nv 'vllm.sif' vllm serve nemotron"));
    }

    #[test]
    fn image_with_shell_metacharacters_is_quoted() {
        let mut s = spec();
        s.image = "weird'$(rm -rf /); image.sif".into();
        let j = job_submit_from_spec(&s, "nemotron", "obleth-");
        assert!(j.script.contains("apptainer exec --nv 'weird'\\''$(rm -rf /); image.sif'"));
    }

    #[test]
    fn job_state_mapping_is_version_tolerant() {
        assert_eq!(map_job_state("RUNNING"), JobState::Running);
        assert_eq!(map_job_state("Pending"), JobState::Pending);
        assert_eq!(map_job_state("PREEMPTED"), JobState::Gone);
        assert_eq!(map_job_state("anything-else"), JobState::Gone);
    }

    #[test]
    fn nodelist_parsing() {
        assert_eq!(expand_nodelist("gpu7"), vec!["gpu7"]);
        assert_eq!(expand_nodelist("a,b"), vec!["a", "b"]);
        assert!(expand_nodelist("").is_empty());
    }
}

/// In-memory fake for executor/loop tests — no network.
#[cfg(test)]
pub struct MockSlurm {
    pub jobs: std::sync::Mutex<Vec<JobInfo>>,
    pub submitted: std::sync::Mutex<Vec<JobSubmit>>,
    pub cancelled: std::sync::Mutex<Vec<String>>,
    pub next_id: std::sync::atomic::AtomicU64,
}
#[cfg(test)]
#[async_trait]
impl SlurmClient for MockSlurm {
    async fn submit(&self, job: &JobSubmit) -> anyhow::Result<String> {
        let id = self.next_id.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.submitted.lock().unwrap().push(job.clone());
        let job_id = format!("job-{id}");
        self.jobs.lock().unwrap().push(JobInfo { job_id: job_id.clone(), state: JobState::Pending, nodes: vec![] });
        Ok(job_id)
    }
    async fn cancel(&self, job_id: &str) -> anyhow::Result<()> {
        self.cancelled.lock().unwrap().push(job_id.to_string());
        Ok(())
    }
    async fn list_owned_jobs(&self, _prefix: &str) -> anyhow::Result<Vec<JobInfo>> {
        Ok(self.jobs.lock().unwrap().clone())
    }
}
