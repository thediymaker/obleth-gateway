use crate::domain::{ClusterResources, JobInfo, JobState, JobSubmit, NodeInfo, PartitionInfo};
use async_trait::async_trait;
use obleth_config::ManagedModelSpec;

#[async_trait]
pub trait SlurmClient: Send + Sync {
    /// Submit a job; return its Slurm job id.
    async fn submit(&self, job: &JobSubmit) -> anyhow::Result<String>;
    async fn cancel(&self, job_id: &str) -> anyhow::Result<()>;
    /// Current state of a single job by id. `Ok(None)` means the controller has
    /// no such job (finished and purged, or never existed) — the planner reads
    /// that as "gone". A transport/HTTP error is returned as `Err` so the caller
    /// can hold the tick rather than mistake an unreachable Slurm for dead jobs.
    /// We look our jobs up by id (recorded on replica rows at submit) rather than
    /// listing the whole controller, which on a busy cluster is huge.
    async fn get_job(&self, job_id: &str) -> anyhow::Result<Option<JobInfo>>;
    /// Best-effort read of cluster partitions, nodes, and the caller's
    /// accounts/QoS. Individual sub-reads that fail are logged and skipped so a
    /// partial cluster view still powers the launcher dropdowns.
    async fn discover_resources(&self) -> anyhow::Result<ClusterResources>;
}

/// Insert a bash block that binds the first free port in the replica's window
/// and exports `OBLETH_SERVING_PORT`. Inserted after the shebang line if one
/// is present, otherwise prepended.
fn inject_port_preamble(script: &str, port_base: i64, span: i64) -> String {
    let span = span.max(1);
    // Pure-bash: a C-style range loop (no `seq`) and a /dev/tcp probe in a
    // subshell (no `timeout`) so the block survives minimal images that ship
    // neither coreutils tool. A successful connect means the port is already
    // taken; the first one we *cannot* connect to is free. localhost connects
    // resolve immediately, so no timeout guard is needed.
    let block = format!(
        "# obleth: bind the first free port in this replica's window\n\
         for ((__p={base}; __p<={last}; __p++)); do\n\
         \x20 if ! (exec 3<>/dev/tcp/127.0.0.1/$__p) 2>/dev/null; then export OBLETH_SERVING_PORT=$__p; break; fi\n\
         done\n\
         export OBLETH_SERVING_PORT=${{OBLETH_SERVING_PORT:-{base}}}\n",
        base = port_base, last = port_base + span - 1,
    );
    // Insert after the first line if it is a shebang, else prepend.
    if let Some(nl) = script.find('\n') {
        if script.starts_with("#!") {
            return format!("{}\n{}{}", &script[..nl], block, &script[nl + 1..]);
        }
    }
    format!("{block}{script}")
}

/// Build a version-agnostic JobSubmit from a model's spec. The job is named
/// `{prefix}{model_name}` so we can find our own jobs. The script runs the
/// operator's launch command inside their apptainer image — or, when `image`
/// is blank, runs the launch command directly on the node (bare-metal, e.g. a
/// module-loaded native binary). Nothing about a specific cluster is assumed.
pub fn job_submit_from_spec(
    spec: &ManagedModelSpec,
    model_name: &str,
    name_prefix: &str,
    port_base: i64,
    span: i64,
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
    //
    // A blank `image` means "no container": run the launch command directly.
    // This supports HPC sites that serve a native, module-loaded binary (e.g.
    // a bare-metal `llama-server`) rather than an apptainer image.
    let image = spec.image.trim();
    let exec_line = if image.is_empty() {
        spec.launch_command.clone()
    } else {
        format!(
            "apptainer exec --nv {image} {cmd}",
            image = shell_quote(image),
            cmd = spec.launch_command,
        )
    };
    // A non-empty `script_body` is the rendered recipe output and wins: it is
    // submitted verbatim so the recipe author has full control of the job
    // script. Otherwise fall back to the legacy preamble/exec assembly.
    let assembled = if spec.script_body.trim().is_empty() {
        format!("#!/bin/bash\nset -euo pipefail\n{preamble_block}{exec_line}\n")
    } else {
        let body = &spec.script_body;
        if body.starts_with("#!") {
            // already a complete script
            if body.ends_with('\n') { body.clone() } else { format!("{body}\n") }
        } else {
            format!("#!/bin/bash\nset -euo pipefail\n{body}\n")
        }
    };
    let script = inject_port_preamble(&assembled, port_base, span);
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
        cpus_per_task: spec.cpus_per_task.filter(|&c| c > 0),
        mem_mb: spec.mem.as_deref().and_then(parse_mem_mb),
        log_output_dir: spec.log_output_dir.clone(),
        script,
    }
}

/// Parse a Slurm-style memory string (e.g. `560G`, `512000M`, `2T`, `16000`)
/// into an integer number of **megabytes** for slurmrestd's `memory_per_node`.
/// A bare number is treated as megabytes (Slurm's default unit). Returns `None`
/// for empty/unparseable input so the field is omitted entirely.
pub fn parse_mem_mb(raw: &str) -> Option<i64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let upper = s.to_ascii_uppercase();
    // strip an optional trailing 'B' (GB/MB/TB) then read the unit letter
    let core = upper.strip_suffix('B').unwrap_or(&upper);
    let (num_part, mult) = match core.chars().last() {
        Some('K') => (&core[..core.len() - 1], 1.0 / 1024.0),
        Some('M') => (&core[..core.len() - 1], 1.0),
        Some('G') => (&core[..core.len() - 1], 1024.0),
        Some('T') => (&core[..core.len() - 1], 1024.0 * 1024.0),
        Some(c) if c.is_ascii_digit() => (core.as_ref(), 1.0),
        _ => return None,
    };
    let n: f64 = num_part.trim().parse().ok()?;
    if n <= 0.0 {
        return None;
    }
    let mb = (n * mult).round() as i64;
    if mb > 0 { Some(mb) } else { None }
}

/// POSIX single-quote a value for embedding in the generated bash script.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Minimal projection of a slurmrestd job record: only the fields the reconcile
/// loop reads. Deserializing into this (rather than `serde_json::Value`) makes
/// serde *skip* every other field instead of allocating a dynamic tree for it,
/// keeping memory proportional to "fields we use" rather than "every field of
/// the record".
#[derive(serde::Deserialize)]
struct JobsResponse {
    #[serde(default)]
    jobs: Vec<JobRecord>,
}

#[derive(serde::Deserialize)]
struct JobRecord {
    /// int or string depending on slurmrestd version — kept as a small scalar
    /// Value and normalized below. (Per-field Value for a scalar is cheap; the
    /// memory blowup came from materializing *whole records*.)
    #[serde(default)]
    job_id: serde_json::Value,
    /// string or array-of-strings depending on version.
    #[serde(default)]
    job_state: serde_json::Value,
    #[serde(default)]
    nodes: String,
    #[serde(default)]
    state_reason: serde_json::Value,
}

fn job_id_to_string(v: &serde_json::Value) -> String {
    v.as_i64()
        .map(|n| n.to_string())
        .or_else(|| v.as_str().map(String::from))
        .unwrap_or_default()
}

fn job_state_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a.first().and_then(|x| x.as_str()).unwrap_or("").to_string(),
        _ => String::new(),
    }
}

/// Project a slurmrestd single-job response body into our `JobInfo`. The
/// `GET /job/{id}` route returns `{ "jobs": [ {record} ] }`; we take the first
/// record. `None` when the array is empty (some versions answer an unknown id
/// with 200 + empty jobs rather than 404) or the body isn't the expected shape.
/// Pure (no HTTP) so the version-shape handling is unit-testable, and typed so
/// the many fields we don't read are skipped rather than allocated.
fn parse_job(body: &[u8]) -> Option<JobInfo> {
    let parsed: JobsResponse = serde_json::from_slice(body).ok()?;
    let j = parsed.jobs.into_iter().next()?;
    let job_id = job_id_to_string(&j.job_id);
    if job_id.is_empty() {
        return None;
    }
    let raw_state = job_state_to_string(&j.job_state);
    let reason = j
        .state_reason
        .as_str()
        .map(str::to_string)
        .filter(|r| !r.trim().is_empty() && !r.eq_ignore_ascii_case("none"));
    Some(JobInfo {
        job_id,
        state: map_job_state(&raw_state),
        nodes: expand_nodelist(&j.nodes),
        raw_state,
        reason,
    })
}

/// Map a Slurm job_state string (any version) onto our coarse JobState.
pub fn map_job_state(s: &str) -> JobState {
    match s.to_ascii_uppercase().as_str() {
        "PENDING" | "CONFIGURING" | "SUSPENDED" => JobState::Pending,
        "RUNNING" | "COMPLETING" => JobState::Running,
        _ => JobState::Gone, // COMPLETED/FAILED/CANCELLED/PREEMPTED/TIMEOUT/NODE_FAIL/...
    }
}

/// Human status line for a replica's Slurm job, e.g. "PENDING — Resources" or
/// "RUNNING". A missing or "None" reason collapses to just the state.
pub fn job_status_message(raw_state: &str, reason: Option<&str>) -> String {
    match reason {
        Some(r) if !r.trim().is_empty() && !r.eq_ignore_ascii_case("none") => {
            format!("{raw_state} — {r}")
        }
        _ => raw_state.to_string(),
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
        if let Some(c) = job.cpus_per_task { if c > 0 { m.insert("cpus_per_task".into(), serde_json::json!(c)); } }
        // slurmrestd expects memory_per_node in megabytes (integer).
        if let Some(mb) = job.mem_mb { if mb > 0 { m.insert("memory_per_node".into(), serde_json::json!(mb)); } }
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

    async fn get_job(&self, job_id: &str) -> anyhow::Result<Option<JobInfo>> {
        let url = format!("{}/slurm/{}/job/{}", self.base, self.version, job_id);
        let resp = self.auth(self.http.get(&url)).send().await?;
        // A finished+purged (or unknown) job is reported as 404 by some versions
        // and as 200 with an empty `jobs` array by others — both mean "gone".
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            anyhow::bail!("slurmrestd job {job_id} failed: {}", resp.status());
        }
        // Typed projection over the raw body (not `.json::<Value>()`): the
        // single-job response still carries a full record we mostly ignore.
        let body = resp.bytes().await?;
        Ok(parse_job(&body))
    }

    async fn discover_resources(&self) -> anyhow::Result<ClusterResources> {
        let get = |path: String| {
            let url = format!("{}/{}", self.base, path);
            self.auth(self.http.get(&url)).send()
        };
        let mut out = ClusterResources::default();
        match get(format!("slurm/{}/partitions", self.version)).await {
            Ok(r) if r.status().is_success() => {
                match r.json::<serde_json::Value>().await {
                    Ok(v) => out.partitions = parse_partitions(&v),
                    Err(e) => tracing::warn!(error=%e, "slurm partitions body not JSON"),
                }
            }
            Ok(r) => tracing::warn!(status=%r.status(), "slurm partitions read failed"),
            Err(e) => tracing::warn!(error=%e, "slurm partitions read errored"),
        }
        match get(format!("slurm/{}/nodes", self.version)).await {
            Ok(r) if r.status().is_success() => {
                match r.json::<serde_json::Value>().await {
                    Ok(v) => out.nodes = parse_nodes(&v),
                    Err(e) => tracing::warn!(error=%e, "slurm nodes body not JSON"),
                }
            }
            Ok(r) => tracing::warn!(status=%r.status(), "slurm nodes read failed"),
            Err(e) => tracing::warn!(error=%e, "slurm nodes read errored"),
        }
        match get(format!("slurmdb/{}/associations", self.version)).await {
            Ok(r) if r.status().is_success() => {
                match r.json::<serde_json::Value>().await {
                    Ok(v) => {
                        let (a, q) = parse_associations(&v);
                        out.accounts = a; out.qos = q;
                    }
                    Err(e) => tracing::warn!(error=%e, "slurm associations body not JSON"),
                }
            }
            Ok(r) => tracing::warn!(status=%r.status(), "slurm associations read failed"),
            Err(e) => tracing::warn!(error=%e, "slurm associations read errored"),
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

/// Read a slurmrestd time value that may be `{"number":N}` (minutes), a bare
/// number, or a string. Returns a display string of minutes, or None.
fn time_minutes(v: &serde_json::Value) -> Option<String> {
    if let Some(n) = v.get("number").and_then(|x| x.as_i64()) { return Some(n.to_string()); }
    if let Some(n) = v.as_i64() { return Some(n.to_string()); }
    v.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string())
}

/// A slurmrestd field that may be a CSV string or an array of strings.
fn str_list(v: Option<&serde_json::Value>) -> Vec<String> {
    match v {
        Some(serde_json::Value::String(s)) =>
            s.split(',').map(|x| x.trim()).filter(|x| !x.is_empty()).map(String::from).collect(),
        Some(serde_json::Value::Array(a)) =>
            a.iter().filter_map(|x| x.as_str()).filter(|s| !s.is_empty()).map(String::from).collect(),
        _ => vec![],
    }
}

pub fn parse_partitions(v: &serde_json::Value) -> Vec<PartitionInfo> {
    v.get("partitions").and_then(|x| x.as_array()).map(|arr| arr.iter().filter_map(|p| {
        let name = p.get("name").and_then(|x| x.as_str())?.to_string();
        let nodes = p.get("nodes")
            .and_then(|n| n.get("configured").map(|c| c.clone()).or_else(|| Some(n.clone())))
            .map(|n| str_list(Some(&n))).unwrap_or_default();
        Some(PartitionInfo {
            name,
            nodes,
            default_time: p.get("defaults").and_then(|d| d.get("time")).and_then(time_minutes),
            max_time: p.get("maximums").and_then(|d| d.get("time")).and_then(time_minutes),
        })
    }).collect()).unwrap_or_default()
}

pub fn parse_nodes(v: &serde_json::Value) -> Vec<NodeInfo> {
    v.get("nodes").and_then(|x| x.as_array()).map(|arr| arr.iter().filter_map(|n| {
        let name = n.get("name").and_then(|x| x.as_str())?.to_string();
        let real_memory_mb = n.get("real_memory")
            .and_then(|m| m.as_i64().or_else(|| m.get("number").and_then(|x| x.as_i64())));
        Some(NodeInfo {
            name,
            partitions: str_list(n.get("partitions")),
            gres: n.get("gres").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
            cpus: n.get("cpus").and_then(|m| m.as_i64().or_else(|| m.get("number").and_then(|x| x.as_i64()))),
            real_memory_mb,
            features: str_list(n.get("features")),
        })
    }).collect()).unwrap_or_default()
}

pub fn parse_associations(v: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let mut accounts = std::collections::BTreeSet::new();
    let mut qos = std::collections::BTreeSet::new();
    if let Some(arr) = v.get("associations").and_then(|x| x.as_array()) {
        for a in arr {
            if let Some(acc) = a.get("account").and_then(|x| x.as_str()) {
                if !acc.is_empty() { accounts.insert(acc.to_string()); }
            }
            for q in str_list(a.get("qos")) { qos.insert(q); }
        }
    }
    (accounts.into_iter().collect(), qos.into_iter().collect())
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
            cpus_per_task: None,
            mem: None,
            image: "vllm.sif".into(),
            preamble: String::new(),
            log_output_dir: String::new(),
            launch_command: "vllm serve nemotron --port 8000".into(),
            script_body: String::new(),
            serving_port: 8000,
            health_path: "/health".into(),
            target_replicas: 2,
            min_replicas: 1,
            max_job_failures: 0,
            launcher_spec: None,
            last_provision_error: None,
            last_provision_error_at: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn builder_is_generic_and_tags_name() {
        let j = job_submit_from_spec(&spec(), "nemotron", "obleth-", 8000, 8);
        assert_eq!(j.name, "obleth-nemotron");
        assert_eq!(j.partition, "gpu-preempt");
        assert!(j.script.contains("apptainer exec --nv 'vllm.sif' vllm serve nemotron"));
    }

    #[test]
    fn preamble_injected_before_apptainer_when_set() {
        let mut s = spec();
        s.preamble = "module load apptainer/1.3.4".into();
        let j = job_submit_from_spec(&s, "nemotron", "obleth-", 8000, 8);
        let lines: Vec<&str> = j.script.lines().collect();
        let apptainer_idx = lines.iter().position(|l| l.contains("apptainer exec")).unwrap();
        let module_idx = lines.iter().position(|l| l.contains("module load apptainer")).unwrap();
        assert!(module_idx < apptainer_idx, "preamble must come before apptainer exec");
    }

    #[test]
    fn empty_preamble_produces_no_extra_line() {
        let j = job_submit_from_spec(&spec(), "nemotron", "obleth-", 8000, 8);
        assert!(!j.script.contains("module load"), "no preamble lines when preamble is empty");
        assert!(j.script.contains("apptainer exec --nv 'vllm.sif' vllm serve nemotron"));
    }

    #[test]
    fn image_with_shell_metacharacters_is_quoted() {
        let mut s = spec();
        s.image = "weird'$(rm -rf /); image.sif".into();
        let j = job_submit_from_spec(&s, "nemotron", "obleth-", 8000, 8);
        assert!(j.script.contains("apptainer exec --nv 'weird'\\''$(rm -rf /); image.sif'"));
    }

    #[test]
    fn blank_image_runs_launch_command_bare_metal() {
        let mut s = spec();
        s.image = "  ".into();
        s.preamble = "module load cuda".into();
        s.launch_command = "llama-server -m model.gguf --port 8000".into();
        let j = job_submit_from_spec(&s, "glm", "obleth-", 8000, 8);
        assert!(!j.script.contains("apptainer"), "no container wrap when image is blank");
        assert!(j.script.contains("module load cuda"));
        assert!(j.script.contains("llama-server -m model.gguf --port 8000"));
    }

    #[test]
    fn cpus_and_mem_flow_into_job_submit() {
        let mut s = spec();
        s.cpus_per_task = Some(72);
        s.mem = Some("560G".into());
        let j = job_submit_from_spec(&s, "glm", "obleth-", 8000, 8);
        assert_eq!(j.cpus_per_task, Some(72));
        assert_eq!(j.mem_mb, Some(560 * 1024));
    }

    #[test]
    fn zero_cpus_is_treated_as_unset() {
        let mut s = spec();
        s.cpus_per_task = Some(0);
        let j = job_submit_from_spec(&s, "glm", "obleth-", 8000, 8);
        assert_eq!(j.cpus_per_task, None);
    }

    #[test]
    fn parse_mem_units() {
        assert_eq!(parse_mem_mb("560G"), Some(560 * 1024));
        assert_eq!(parse_mem_mb("560GB"), Some(560 * 1024));
        assert_eq!(parse_mem_mb("512000M"), Some(512000));
        assert_eq!(parse_mem_mb("512000"), Some(512000));
        assert_eq!(parse_mem_mb("2T"), Some(2 * 1024 * 1024));
        assert_eq!(parse_mem_mb(""), None);
        assert_eq!(parse_mem_mb("   "), None);
        assert_eq!(parse_mem_mb("lots"), None);
        assert_eq!(parse_mem_mb("0G"), None);
    }

    #[test]
    fn script_body_with_shebang_gets_preamble_injected() {
        let mut s = spec();
        s.script_body = "#!/bin/bash\nset -e\nllama-server --port 8000\n".into();
        let j = job_submit_from_spec(&s, "glm", "obleth-", 8000, 8);
        // Port preamble is injected after the shebang; script_body wins over legacy assembly.
        assert!(j.script.starts_with("#!/bin/bash\n"), "shebang retained as first line");
        assert!(j.script.contains("OBLETH_SERVING_PORT"), "port preamble injected");
        assert!(j.script.contains("llama-server --port 8000"));
        assert!(!j.script.contains("apptainer"), "script_body overrides legacy assembly");
    }

    #[test]
    fn script_body_without_shebang_gets_wrapped_and_preamble_injected() {
        let mut s = spec();
        s.script_body = "llama-server --port 8000".into();
        let j = job_submit_from_spec(&s, "glm", "obleth-", 8000, 8);
        assert!(j.script.starts_with("#!/bin/bash\n"), "shebang added");
        assert!(j.script.contains("OBLETH_SERVING_PORT"), "port preamble injected");
        assert!(j.script.contains("llama-server --port 8000"));
    }

    #[test]
    fn port_preamble_uses_correct_window() {
        let j = job_submit_from_spec(&spec(), "nemotron", "obleth-", 8016, 8);
        assert!(j.script.contains("__p=8016; __p<=8023"), "window base and last computed correctly");
        assert!(j.script.contains("OBLETH_SERVING_PORT:-8016"), "fallback to window base");
    }

    #[test]
    fn job_status_message_formats_state_and_reason() {
        assert_eq!(job_status_message("PENDING", Some("Resources")), "PENDING — Resources");
        assert_eq!(job_status_message("RUNNING", None), "RUNNING");
        assert_eq!(job_status_message("RUNNING", Some("None")), "RUNNING");
        assert_eq!(job_status_message("PENDING", Some("  ")), "PENDING");
    }

    #[test]
    fn parse_job_tolerates_int_id_and_array_state() {
        let body = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": 123, "job_state": ["PENDING"], "nodes": "", "state_reason": "Resources" },
        ] })).unwrap();
        let j = parse_job(&body).expect("job");
        assert_eq!(j.job_id, "123");
        assert_eq!(j.raw_state, "PENDING");
        assert_eq!(j.state, JobState::Pending);
        assert_eq!(j.nodes, Vec::<String>::new());
        assert_eq!(j.reason.as_deref(), Some("Resources"));
    }

    #[test]
    fn parse_job_tolerates_string_id_and_string_state() {
        let body = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": "456", "job_state": "PENDING", "nodes": "gpu03" },
        ] })).unwrap();
        let j = parse_job(&body).expect("job");
        assert_eq!(j.job_id, "456");
        assert_eq!(j.state, JobState::Pending);
    }

    #[test]
    fn parse_job_returns_none_for_empty_or_unknown() {
        // 200-with-empty-jobs is how some versions report an unknown id.
        let empty = serde_json::to_vec(&serde_json::json!({ "jobs": [] })).unwrap();
        assert!(parse_job(&empty).is_none());
        let no_id = serde_json::to_vec(&serde_json::json!({ "jobs": [ { "job_state": "RUNNING" } ] })).unwrap();
        assert!(parse_job(&no_id).is_none());
    }

    #[test]
    fn parse_job_collapses_none_reason_at_parse_time() {
        // Literal "None" and whitespace-only reasons collapse to None
        let none_literal = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": 1, "job_state": "PENDING", "state_reason": "None" },
        ] })).unwrap();
        let j = parse_job(&none_literal).expect("job");
        assert_eq!(j.reason, None, "literal 'None' reason should collapse to None");

        let none_mixed_case = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": 2, "job_state": "PENDING", "state_reason": "nOnE" },
        ] })).unwrap();
        let j = parse_job(&none_mixed_case).expect("job");
        assert_eq!(j.reason, None, "case-insensitive 'None' should collapse");

        let whitespace_only = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": 3, "job_state": "PENDING", "state_reason": "   " },
        ] })).unwrap();
        let j = parse_job(&whitespace_only).expect("job");
        assert_eq!(j.reason, None, "whitespace-only reason should collapse");

        let valid_reason = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": 4, "job_state": "PENDING", "state_reason": "Resources" },
        ] })).unwrap();
        let j = parse_job(&valid_reason).expect("job");
        assert_eq!(j.reason, Some("Resources".to_string()), "valid reasons preserved");
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

    #[test]
    fn parse_partitions_reads_name_and_nodes() {
        let v = serde_json::json!({"partitions":[
            {"name":"arm","nodes":{"total":1,"configured":"gpu[01-02]"},
             "maximums":{"time":{"number":240}}, "defaults":{"time":{"number":60}}}
        ]});
        let p = parse_partitions(&v);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].name, "arm");
        assert!(p[0].nodes.contains(&"gpu[01-02]".to_string()) || !p[0].nodes.is_empty());
    }

    #[test]
    fn parse_nodes_reads_gres_cpu_mem_features() {
        let v = serde_json::json!({"nodes":[
            {"name":"gpu01","partitions":["arm"],"gres":"gpu:gh200:1",
             "cpus":72,"real_memory":577536,"features":"gh200,arm"}
        ]});
        let n = parse_nodes(&v);
        assert_eq!(n[0].name, "gpu01");
        assert_eq!(n[0].gres, "gpu:gh200:1");
        assert_eq!(n[0].cpus, Some(72));
        assert_eq!(n[0].real_memory_mb, Some(577536));
        assert!(n[0].features.contains(&"gh200".to_string()));
    }

    #[test]
    fn parse_associations_dedupes_accounts_and_qos() {
        let v = serde_json::json!({"associations":[
            {"account":"ml-team","qos":["normal","high"]},
            {"account":"ml-team","qos":["normal"]}
        ]});
        let (accounts, qos) = parse_associations(&v);
        assert_eq!(accounts, vec!["ml-team"]);
        assert_eq!(qos, vec!["high","normal"]); // sorted+deduped
    }

    #[test]
    fn parsers_tolerate_missing_keys() {
        let empty = serde_json::json!({});
        assert!(parse_partitions(&empty).is_empty());
        assert!(parse_nodes(&empty).is_empty());
        assert_eq!(parse_associations(&empty), (vec![], vec![]));
    }
}
