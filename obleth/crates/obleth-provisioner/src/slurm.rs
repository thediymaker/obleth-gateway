use crate::domain::{
    AssociationInfo, ClusterResources, JobInfo, JobState, JobSubmit, NodeInfo, PartitionInfo,
};
use async_trait::async_trait;
use obleth_config::slurm as slurm_values;
use obleth_config::ManagedModelSpec;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// A job slurmrestd accepted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SubmittedJob {
    pub job_id: String,
    /// What Slurm said alongside an accepted job (`warnings[]`, the site's
    /// job_submit message), as plain sentences. Empty when it said nothing.
    pub notes: Vec<String>,
}

#[async_trait]
pub trait SlurmClient: Send + Sync {
    /// Submit a job. A failure is a [`SlurmApiError`] (see
    /// [`slurm_api_error`]); [`SlurmErrorKind::NoAnswer`] means the outcome
    /// is unknown and the job may exist.
    async fn submit(&self, job: &JobSubmit) -> anyhow::Result<SubmittedJob>;
    async fn cancel(&self, job_id: &str) -> anyhow::Result<()>;
    /// Current state of a single job by id. `Ok(None)` means the controller has
    /// no such job (finished and purged, or never existed), or the id now
    /// belongs to a job obleth did not submit; the planner reads that as
    /// "gone". It is returned only for an explicit job-not-found answer (by
    /// error number or text, whatever the HTTP status). A transport/HTTP
    /// error, an unrecognized 404, or an unparseable body is returned as `Err`
    /// so the caller can hold the tick rather than mistake an unreachable or
    /// mismatched Slurm for dead jobs.
    async fn get_job(&self, job_id: &str) -> anyhow::Result<Option<JobInfo>>;
    /// Best-effort read of cluster partitions, nodes, and the caller's
    /// accounts/QoS. A sub-read that fails is named in `error` (and `partial`
    /// is set when others worked), so the launcher can say why a list is empty.
    async fn discover_resources(&self) -> anyhow::Result<ClusterResources>;
    /// One cheap authenticated read, so a rejected token or an API version
    /// the server does not serve shows up even when nothing else calls Slurm.
    async fn check_auth(&self) -> anyhow::Result<()> {
        Ok(())
    }
    /// Live (pending or running) jobs of the configured user with exactly
    /// this name, from the accounting database. Used to find a job whose
    /// submit answer was lost. Empty when the cluster has no accounting
    /// database or it cannot be read.
    async fn find_live_jobs_by_name(&self, _name: &str) -> anyhow::Result<Vec<JobInfo>> {
        Ok(vec![])
    }
    /// Node name to address (Slurm's NodeAddr) for every node whose address
    /// differs from its name.
    async fn node_addresses(&self) -> anyhow::Result<HashMap<String, String>> {
        Ok(HashMap::new())
    }
    /// One job from the accounting database, which still has jobs the
    /// controller has purged. `Ok(None)` when it does not know the job (or the
    /// job is not obleth's); `Err` when there is no accounting database or it
    /// cannot be read.
    async fn accounting_job(&self, _job_id: &str) -> anyhow::Result<Option<JobInfo>> {
        anyhow::bail!("this Slurm client has no accounting database")
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// What kind of failure a slurmrestd call hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlurmErrorKind {
    /// HTTP 401, 403 or 511: the token or user was not accepted.
    TokenRejected,
    /// A 404 with no Slurm error in the body: this URL does not serve the
    /// route under the API version in use.
    NotServed,
    /// Slurm answered and refused the request (a 4xx, or a Slurm error
    /// number, with `errors[]`). Sending the same request again will not help.
    Rejected,
    /// A 5xx, a proxy in front, or a body slurmrestd would not send.
    Unavailable,
    /// No usable answer: connection refused, DNS, timeout, or a cut-off body.
    /// For a submit the outcome is unknown and the job may exist.
    NoAnswer,
}

/// A slurmrestd failure as a plain sentence, with its kind and HTTP status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlurmApiError {
    pub kind: SlurmErrorKind,
    pub status: Option<u16>,
    pub message: String,
}

impl std::fmt::Display for SlurmApiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SlurmApiError {}

impl SlurmApiError {
    fn new(kind: SlurmErrorKind, status: Option<u16>, message: String) -> Self {
        Self {
            kind,
            status,
            message,
        }
    }
    /// Slurm refused the request itself; retrying it unchanged will not help.
    pub fn is_rejection(&self) -> bool {
        self.kind == SlurmErrorKind::Rejected
    }
}

/// The [`SlurmApiError`] inside an error from this module, if it is one.
pub fn slurm_api_error(err: &anyhow::Error) -> Option<&SlurmApiError> {
    err.chain().find_map(|e| e.downcast_ref::<SlurmApiError>())
}

/// One entry of slurmrestd's `errors` / `warnings` arrays.
#[derive(serde::Deserialize, Default)]
struct SlurmMessage {
    #[serde(default)]
    error_number: Option<i64>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    description: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct Envelope {
    #[serde(default)]
    errors: Vec<SlurmMessage>,
    #[serde(default)]
    warnings: Vec<SlurmMessage>,
}

/// Slurm's `ESLURM_INVALID_JOB_ID` ("Invalid job id specified"): what
/// slurmrestd reports for a job the controller has purged or never had.
const ESLURM_INVALID_JOB_ID: i64 = 2017;

impl SlurmMessage {
    fn is_job_not_found(&self) -> bool {
        self.error_number == Some(ESLURM_INVALID_JOB_ID)
            || [&self.error, &self.description].iter().any(|t| {
                t.as_deref()
                    .is_some_and(|e| e.to_ascii_lowercase().contains("invalid job id"))
            })
    }
    /// Slurm's own error codes (ESLURM_*) sit in 2000..=3999; they describe
    /// the request, not the transport.
    fn is_slurm_refusal(&self) -> bool {
        self.error_number
            .is_some_and(|n| (2000..=3999).contains(&n))
    }
    /// The most specific text: Slurm's error string, else the description.
    fn text(&self) -> Option<String> {
        let clean = |s: &Option<String>| {
            s.as_deref()
                .map(|t| t.trim().trim_end_matches('.').to_string())
                .filter(|t| !t.is_empty())
        };
        clean(&self.error).or_else(|| clean(&self.description))
    }
}

/// Descriptions slurmrestd puts on every failed submit; they add nothing.
const GENERIC_DESCRIPTIONS: &[&str] = &["batch job submission failed", "job submission failed"];

fn envelope(body: &[u8]) -> Envelope {
    serde_json::from_slice(body).unwrap_or_default()
}

/// The first `max` characters of a body that is not slurmrestd JSON.
fn excerpt(body: &[u8], max: usize) -> String {
    let s = String::from_utf8_lossy(body);
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if s.chars().count() > max {
        format!("{}...", s.chars().take(max).collect::<String>())
    } else {
        s
    }
}

/// `errors[]` as one sentence fragment, or a body excerpt when there are none.
fn error_detail(env: &Envelope, body: &[u8]) -> String {
    let texts: Vec<String> = env.errors.iter().filter_map(SlurmMessage::text).collect();
    if texts.is_empty() {
        let e = excerpt(body, 200);
        if e.is_empty() {
            "no details were given".to_string()
        } else {
            e
        }
    } else {
        texts.join("; ")
    }
}

/// Turn a non-success answer into a plain sentence. `what` names the call
/// ("reading job 123", "submitting the job"); `version` is the API version in use.
pub fn error_from_response(status: u16, body: &[u8], what: &str, version: &str) -> SlurmApiError {
    let env = envelope(body);
    let refused = env.errors.iter().any(SlurmMessage::is_slurm_refusal);
    match status {
        401 | 403 | 511 => SlurmApiError::new(
            SlurmErrorKind::TokenRejected,
            Some(status),
            format!(
                "slurmrestd rejected the Slurm token while {what} (HTTP {status}). The token \
                 may have expired or belong to another user. Mint a new one with `scontrol \
                 token` and save it in the Slurm settings."
            ),
        ),
        404 if env.errors.is_empty() => SlurmApiError::new(
            SlurmErrorKind::NotServed,
            Some(status),
            format!(
                "slurmrestd does not serve API version {version} at this URL (HTTP 404 while \
                 {what}). Set the API version to auto, or to one this slurmrestd serves."
            ),
        ),
        408 | 429 => SlurmApiError::new(
            SlurmErrorKind::Unavailable,
            Some(status),
            format!(
                "slurmrestd was busy while {what} (HTTP {status}): {}.",
                error_detail(&env, body)
            ),
        ),
        400..=499 => SlurmApiError::new(
            SlurmErrorKind::Rejected,
            Some(status),
            format!("Slurm refused {what}: {}.", error_detail(&env, body)),
        ),
        _ if refused => SlurmApiError::new(
            SlurmErrorKind::Rejected,
            Some(status),
            format!("Slurm refused {what}: {}.", error_detail(&env, body)),
        ),
        _ => SlurmApiError::new(
            SlurmErrorKind::Unavailable,
            Some(status),
            format!(
                "slurmrestd failed while {what} (HTTP {status}): {}.",
                error_detail(&env, body)
            ),
        ),
    }
}

/// A submit refusal in the form the dashboard shows: "Slurm rejected the job:
/// <reason>. (job_submit said: <message>)".
fn submit_error_from_response(status: u16, body: &[u8], version: &str) -> SlurmApiError {
    let mut e = error_from_response(status, body, "submitting the job", version);
    if e.kind != SlurmErrorKind::Rejected {
        return e;
    }
    let env = envelope(body);
    let reason = error_detail(&env, body);
    let mut said: Vec<String> = env
        .errors
        .iter()
        .filter_map(|m| m.description.as_deref().map(str::trim))
        .filter(|d| {
            !d.is_empty()
                && !GENERIC_DESCRIPTIONS.contains(&d.to_ascii_lowercase().trim_end_matches('.'))
                && !reason.contains(d.trim_end_matches('.'))
        })
        .map(|d| d.trim_end_matches('.').to_string())
        .collect();
    if let Some(m) = job_submit_user_msg(body) {
        if !said.contains(&m) && !reason.contains(&m) {
            said.push(m);
        }
    }
    e.message = if said.is_empty() {
        format!("Slurm rejected the job: {reason}.")
    } else {
        format!(
            "Slurm rejected the job: {reason}. (job_submit said: {}.)",
            said.join("; ")
        )
    };
    e
}

/// The site job_submit plugin's message, at the top level or under `result`.
fn job_submit_user_msg(body: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(body).ok()?;
    let found = [
        v.get("job_submit_user_msg"),
        v.get("result").and_then(|r| r.get("job_submit_user_msg")),
    ]
    .into_iter()
    .flatten()
    .filter_map(|m| m.as_str())
    .map(|m| m.trim().trim_end_matches('.').to_string())
    .find(|m| !m.is_empty());
    found
}

/// The deepest cause of a transport error, which names what actually failed
/// (connection refused, DNS, certificate).
fn root_cause(e: &(dyn std::error::Error + 'static)) -> String {
    let mut cur = e;
    while let Some(next) = cur.source() {
        cur = next;
    }
    cur.to_string()
}

fn no_answer(base: &str, what: &str, e: &reqwest::Error) -> SlurmApiError {
    let message = if e.is_timeout() {
        format!("slurmrestd at {base} did not answer in time while {what}.")
    } else {
        format!(
            "Could not reach slurmrestd at {base} while {what}: {}.",
            root_cause(e).trim_end_matches('.')
        )
    };
    SlurmApiError::new(SlurmErrorKind::NoAnswer, None, message)
}

// ---------------------------------------------------------------------------
// API version discovery
// ---------------------------------------------------------------------------

/// The API versions a slurmrestd serves and the one obleth uses with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApiVersions {
    /// Every version the server lists, oldest first.
    pub available: Vec<String>,
    /// The newest of them this build supports, if any.
    pub resolved: Option<String>,
}

#[derive(serde::Deserialize)]
struct OpenApiPaths {
    #[serde(default)]
    paths: HashMap<String, serde::de::IgnoredAny>,
}

/// Read which API versions `base` serves: the path keys of `GET /openapi/v3`
/// (or `/openapi`), else a `/ping` under each supported version, newest first.
pub async fn discover_api_versions(
    http: &reqwest::Client,
    base: &str,
    user: &str,
    jwt: &str,
) -> Result<ApiVersions, SlurmApiError> {
    let base = base.trim_end_matches('/');
    let auth = |rb: reqwest::RequestBuilder| {
        rb.header("X-SLURM-USER-NAME", user)
            .header("X-SLURM-USER-TOKEN", jwt)
    };
    let what = "reading which API versions it serves";
    let mut last_err = None;
    for path in ["openapi/v3", "openapi"] {
        let resp = auth(http.get(format!("{base}/{path}")))
            .send()
            .await
            .map_err(|e| no_answer(base, what, &e))?;
        let status = resp.status().as_u16();
        let body = resp.bytes().await.map_err(|e| no_answer(base, what, &e))?;
        if (200..300).contains(&status) {
            if let Ok(doc) = serde_json::from_slice::<OpenApiPaths>(&body) {
                let available =
                    slurm_values::api_versions_from_paths(doc.paths.keys().map(String::as_str));
                if !available.is_empty() {
                    let resolved = slurm_values::pick_api_version(&available);
                    return Ok(ApiVersions {
                        available,
                        resolved,
                    });
                }
            }
            continue;
        }
        let e = error_from_response(status, &body, what, "");
        if e.kind == SlurmErrorKind::TokenRejected {
            return Err(e);
        }
        last_err = Some(e);
    }
    // No OpenAPI document: ask each supported version's ping, newest first.
    let mut available = Vec::new();
    for v in slurm_values::SUPPORTED_SLURM_API_VERSIONS.iter().rev() {
        let resp = auth(http.get(format!("{base}/slurm/{v}/ping")))
            .send()
            .await
            .map_err(|e| no_answer(base, what, &e))?;
        let status = resp.status().as_u16();
        if matches!(status, 401 | 403 | 511) {
            return Err(error_from_response(status, &[], what, v));
        }
        if status != 404 {
            available.push(v.to_string());
        }
    }
    if available.is_empty() {
        return Err(last_err.unwrap_or_else(|| {
            SlurmApiError::new(
                SlurmErrorKind::NotServed,
                Some(404),
                format!("slurmrestd at {base} serves none of the API versions obleth supports."),
            )
        }));
    }
    available.sort_by_key(|v| slurm_values::parse_api_version(v));
    let resolved = slurm_values::pick_api_version(&available);
    Ok(ApiVersions {
        available,
        resolved,
    })
}

/// The sentence for a server that serves no version this build supports.
pub fn no_supported_version_message(available: &[String]) -> String {
    let supported = slurm_values::SUPPORTED_SLURM_API_VERSIONS;
    format!(
        "This slurmrestd serves API versions {}, and obleth supports {} to {}. Update obleth \
         or ask the cluster administrator to enable a supported version.",
        if available.is_empty() {
            "none that could be read".to_string()
        } else {
            available.join(", ")
        },
        supported[0],
        supported[supported.len() - 1],
    )
}

const API_VERSION_CACHE_TTL: Duration = Duration::from_secs(600);

type VersionCache = Mutex<HashMap<(String, String), (Instant, String)>>;

fn version_cache() -> &'static VersionCache {
    static CACHE: OnceLock<VersionCache> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// The API version to use: the configured one, or for `auto` (or empty) the
/// newest supported version the server offers, cached per URL and user for
/// ten minutes.
pub async fn resolve_api_version(
    http: &reqwest::Client,
    base: &str,
    setting: &str,
    user: &str,
    jwt: &str,
) -> Result<String, SlurmApiError> {
    if !slurm_values::is_auto_api_version(setting) {
        return Ok(setting.trim().to_string());
    }
    let key = (base.trim_end_matches('/').to_string(), user.to_string());
    if let Ok(cache) = version_cache().lock() {
        if let Some((at, v)) = cache.get(&key) {
            if at.elapsed() < API_VERSION_CACHE_TTL {
                return Ok(v.clone());
            }
        }
    }
    let found = discover_api_versions(http, base, user, jwt).await?;
    let Some(v) = found.resolved else {
        return Err(SlurmApiError::new(
            SlurmErrorKind::NotServed,
            None,
            no_supported_version_message(&found.available),
        ));
    };
    if let Ok(mut cache) = version_cache().lock() {
        cache.insert(key, (Instant::now(), v.clone()));
    }
    Ok(v)
}

/// Forget cached resolutions, so the next call reads the server again.
pub fn forget_resolved_api_versions() {
    if let Ok(mut cache) = version_cache().lock() {
        cache.clear();
    }
}

/// How far ahead a token's expiry is warned about.
pub const TOKEN_WARN_SECS: i64 = 3 * 86_400;

/// "2 days", "5 hours", "40 minutes".
pub fn plain_duration(secs: i64) -> String {
    let secs = secs.max(0);
    let (n, unit) = if secs >= 2 * 86_400 {
        (secs / 86_400, "day")
    } else if secs >= 2 * 3_600 {
        (secs / 3_600, "hour")
    } else {
        ((secs / 60).max(1), "minute")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

/// A warning about the Slurm token's expiry, when it expires within
/// [`TOKEN_WARN_SECS`] or has expired. `None` for a token without a readable
/// expiry or one with time to spare.
pub fn token_expiry_warning(jwt: &str, now_unix: i64) -> Option<String> {
    let exp = slurm_values::slurm_jwt_claims(jwt)?.exp?;
    let left = exp - now_unix;
    if left <= 0 {
        let at = chrono::DateTime::from_timestamp(exp, 0)?;
        Some(format!(
            "The Slurm token expired at {} UTC. Mint a new one with `scontrol token \
             lifespan=2592000` and save it in the Slurm settings.",
            at.format("%Y-%m-%d %H:%M")
        ))
    } else if left <= TOKEN_WARN_SECS {
        Some(format!(
            "The Slurm token expires in {}. Mint a new one with `scontrol token \
             lifespan=2592000` and save it in the Slurm settings.",
            plain_duration(left)
        ))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Job script
// ---------------------------------------------------------------------------

/// What the job writes into its comment to tell obleth which port it bound.
pub const PORT_COMMENT_KEY: &str = "obleth-port=";

/// Insert a bash block that binds the first free port in the replica's window,
/// exports `OBLETH_SERVING_PORT`, and records the port in the job's comment
/// (a job's owner may update it) so the provisioner probes exactly that port.
/// Inserted after the shebang line if one is present, otherwise prepended.
fn inject_port_preamble(script: &str, port_base: i64, span: i64) -> String {
    let span = span.max(1);
    // Pure bash: a C-style range loop (no `seq`) and a /dev/tcp probe in a
    // subshell (no `timeout`) so the block survives minimal images that ship
    // neither coreutils tool. A successful connect means the port is already
    // taken; the first one that cannot be connected to is free. localhost
    // connects resolve immediately, so no timeout guard is needed.
    let block = format!(
        "# obleth: bind the first free port in this replica's window\n\
         for ((__p={base}; __p<={last}; __p++)); do\n\
         \x20 if ! (exec 3<>/dev/tcp/127.0.0.1/$__p) 2>/dev/null; then export OBLETH_SERVING_PORT=$__p; break; fi\n\
         done\n\
         export OBLETH_SERVING_PORT=${{OBLETH_SERVING_PORT:-{base}}}\n\
         scontrol update JobId=\"${{SLURM_JOB_ID:-}}\" Comment=\"{key}$OBLETH_SERVING_PORT\" >/dev/null 2>&1 || true\n",
        base = port_base,
        last = port_base + span - 1,
        key = PORT_COMMENT_KEY,
    );
    if script.starts_with("#!") {
        if let Some(nl) = script.find('\n') {
            return format!("{}\n{}{}", &script[..nl], block, &script[nl + 1..]);
        }
    }
    format!("{block}{script}")
}

/// The shebang a script gets when it has none: a login shell, so the
/// cluster's module system and profile are set up as in an interactive job.
const DEFAULT_SHEBANG: &str = "#!/bin/bash -l";

/// Build a version-agnostic JobSubmit from a model's spec. The job is named
/// `{prefix}{model_name}` (made safe for file names). The script runs the
/// operator's launch command inside their image with the cluster's container
/// command (`$OBLETH_CONTAINER_RUN`, `apptainer exec --nv` when unset, as the
/// shipped recipes do), or, when `image` is blank, directly on the node.
/// Nothing about a specific cluster is assumed.
pub fn job_submit_from_spec(
    spec: &ManagedModelSpec,
    model_name: &str,
    name_prefix: &str,
    port_base: i64,
    span: i64,
) -> JobSubmit {
    let preamble = unix_line_endings(&spec.preamble);
    let launch_command = unix_line_endings(&spec.launch_command);
    let script_body = unix_line_endings(&spec.script_body);
    let preamble = preamble.trim();
    let preamble_block = if preamble.is_empty() {
        String::new()
    } else {
        format!("{preamble}\n")
    };
    // `launch_command` and `preamble` are raw shell on purpose: operators
    // write real bash here and already control the script through `preamble`,
    // behind the same admin token as this endpoint. `image` has no reason to
    // contain shell syntax, so it is quoted.
    //
    // A blank `image` means "no container": run the launch command directly
    // (a native, module-loaded binary, for example).
    let image = spec.image.trim();
    let exec_line = if image.is_empty() {
        launch_command
    } else {
        // Unquoted on purpose: the container command is several words.
        format!(
            "${{OBLETH_CONTAINER_RUN:-apptainer exec --nv}} {image} {cmd}",
            image = shell_quote(image),
            cmd = launch_command,
        )
    };
    // A non-empty `script_body` is the rendered recipe and wins: it is
    // submitted as written (after the port block). Leading blank lines and a
    // byte-order mark are dropped so a shebang below them is still found.
    let body = script_body
        .trim_start_matches('\u{feff}')
        .trim_start_matches(|c: char| c.is_whitespace());
    let assembled = if body.trim().is_empty() {
        format!("{DEFAULT_SHEBANG}\nset -euo pipefail\n{preamble_block}{exec_line}\n")
    } else if body.starts_with("#!") {
        if body.ends_with('\n') {
            body.to_string()
        } else {
            format!("{body}\n")
        }
    } else {
        format!("{DEFAULT_SHEBANG}\nset -euo pipefail\n{body}\n")
    };
    let script = inject_port_preamble(&assembled, port_base, span);
    JobSubmit {
        name: slurm_values::slurm_job_name(name_prefix, model_name),
        partition: spec.partition.trim().to_string(),
        gres: spec.gres.trim().to_string(),
        nodes: spec.nodes.max(1),
        time_limit: spec.time_limit.clone(),
        account: non_empty(&spec.account),
        qos: non_empty(&spec.qos),
        constraints: non_empty(&spec.constraints),
        exclude: non_empty(&spec.exclude),
        cpus_per_task: spec.cpus_per_task.filter(|&c| c > 0),
        mem_mb: spec.mem.as_deref().and_then(parse_mem_mb),
        log_output_dir: spec.log_output_dir.trim().to_string(),
        script,
    }
}

fn non_empty(v: &Option<String>) -> Option<String> {
    v.as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

/// The folder the job runs in: its log folder when one is set, else `/tmp`.
pub fn job_working_dir(job: &JobSubmit) -> String {
    let dir = job.log_output_dir.trim().trim_end_matches('/');
    if dir.is_empty() {
        "/tmp".to_string()
    } else {
        dir.to_string()
    }
}

/// Where a submitted job's output goes: `<log folder or /tmp>/<job-name>-<id>.out`.
pub fn job_log_path(job: &JobSubmit, job_id: &str) -> String {
    format!("{}/{}-{job_id}.out", job_working_dir(job), job.name)
}

/// A script with Windows line endings (CRLF) as Unix ones (LF). A browser
/// sends a textarea's text with CRLF, and bash reads the `\r` as part of each
/// line: `#!/bin/bash -l\r` passes the option `-\r`, so the job exits with
/// "invalid option" before its first command runs.
fn unix_line_endings(s: &str) -> String {
    s.replace("\r\n", "\n")
}

/// Parse a Slurm memory size into megabytes; see
/// [`obleth_config::slurm::parse_mem_mb`].
pub fn parse_mem_mb(raw: &str) -> Option<i64> {
    slurm_values::parse_mem_mb(raw)
}

/// All the memory a job can have on one node of `partition`, in megabytes:
/// the smallest node's memory less what Slurm keeps back for the node itself.
/// The smallest node, so the job still fits wherever Slurm places it. `None`
/// when the partition is unnamed or none of its nodes report their memory.
pub fn whole_node_mem_mb(nodes: &[NodeInfo], partition: &str) -> Option<i64> {
    if partition.is_empty() {
        return None;
    }
    nodes
        .iter()
        .filter(|n| n.partitions.iter().any(|p| p == partition))
        .filter_map(|n| {
            let real = n.real_memory_mb?;
            Some(real - n.specialized_memory_mb.unwrap_or(0))
        })
        .filter(|mb| *mb > 0)
        .min()
}

/// POSIX single-quote a value for embedding in the generated bash script.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

// ---------------------------------------------------------------------------
// Version-specific JSON: job records, submit body, ping, discovery
// ---------------------------------------------------------------------------
//
// Everything below that reads or writes slurmrestd JSON was checked against
// the job_desc_msg / job_info / ping / node schemas of v0.0.40 to v0.0.45.
// Numbers that may be unset arrive as `{set, infinite, number}` objects from
// v0.0.40 on and as plain integers before; both are read.

/// `jobs` is required: a 2xx body without it (an error envelope, a proxy page)
/// is a failed read, not an empty answer.
#[derive(serde::Deserialize)]
struct JobsResponse {
    jobs: Vec<JobRecord>,
    #[serde(default)]
    errors: Vec<SlurmMessage>,
}

/// The fields of a slurmrestd job record the reconcile loop reads. Typed so
/// serde skips every other field instead of allocating it.
#[derive(serde::Deserialize, Default)]
struct JobRecord {
    /// int or string depending on version.
    #[serde(default)]
    job_id: serde_json::Value,
    /// string or array of strings depending on version.
    #[serde(default)]
    job_state: serde_json::Value,
    #[serde(default)]
    nodes: Option<String>,
    #[serde(default)]
    batch_host: Option<String>,
    #[serde(default)]
    state_reason: serde_json::Value,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    user_name: Option<String>,
    /// string, or an object of comment kinds in some versions.
    #[serde(default)]
    comment: serde_json::Value,
    #[serde(default)]
    restart_cnt: serde_json::Value,
    #[serde(default)]
    start_time: serde_json::Value,
    #[serde(default)]
    end_time: serde_json::Value,
    #[serde(default)]
    time_limit: serde_json::Value,
}

/// A number that may be a plain integer or a `{set, infinite, number}` object.
/// `Some(None)` for an infinite value; `None` when unset or absent.
fn no_val_number(v: &serde_json::Value) -> Option<Option<i64>> {
    if let Some(n) = v.as_i64() {
        return Some(Some(n));
    }
    let obj = v.as_object()?;
    if obj.get("infinite").and_then(|x| x.as_bool()) == Some(true) {
        return Some(None);
    }
    if obj.get("set").and_then(|x| x.as_bool()) == Some(false) {
        return None;
    }
    obj.get("number").and_then(|x| x.as_i64()).map(Some)
}

fn unix_time(v: &serde_json::Value) -> Option<chrono::DateTime<chrono::Utc>> {
    let secs = no_val_number(v)??;
    if secs <= 0 {
        return None;
    }
    chrono::DateTime::from_timestamp(secs, 0)
}

fn job_id_to_string(v: &serde_json::Value) -> String {
    v.as_i64()
        .map(|n| n.to_string())
        .or_else(|| v.as_str().map(String::from))
        .unwrap_or_default()
}

/// Every state flag: `"RUNNING"` or `["PENDING", "REQUEUED"]`, upper case.
fn job_state_flags(v: &serde_json::Value) -> Vec<String> {
    match v {
        serde_json::Value::String(s) => vec![s.to_ascii_uppercase()],
        serde_json::Value::Array(a) => a
            .iter()
            .filter_map(|x| x.as_str())
            .map(str::to_ascii_uppercase)
            .collect(),
        _ => vec![],
    }
}

/// The port the job reported in its comment (`obleth-port=8001`).
pub fn port_from_comment(comment: &str) -> Option<i64> {
    let at = comment.find(PORT_COMMENT_KEY)? + PORT_COMMENT_KEY.len();
    let digits: String = comment[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    digits.parse().ok().filter(|p| (1..=65535).contains(p))
}

fn comment_text(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::String(s) => Some(s.clone()),
        serde_json::Value::Object(o) => o.get("job").and_then(|x| x.as_str()).map(String::from),
        _ => None,
    }
}

/// Project a slurmrestd single-job response body into a `JobInfo`. The
/// `GET /job/{id}` route returns `{ "jobs": [ {record} ] }`; the first record
/// is taken. `Ok(None)` when the array is empty (some versions answer an
/// unknown id with 200 + empty jobs rather than 404) unless it carries an
/// error other than job-not-found; a body that isn't the expected shape is `Err`.
fn parse_job(body: &[u8]) -> anyhow::Result<Option<JobInfo>> {
    let parsed: JobsResponse = serde_json::from_slice(body).map_err(|e| {
        SlurmApiError::new(
            SlurmErrorKind::Unavailable,
            None,
            format!("slurmrestd sent a job record obleth could not read: {e}."),
        )
    })?;
    let Some(j) = parsed.jobs.into_iter().next() else {
        if let Some(e) = parsed.errors.iter().find(|e| !e.is_job_not_found()) {
            return Err(SlurmApiError::new(
                SlurmErrorKind::Unavailable,
                None,
                format!(
                    "slurmrestd returned no job and an error: {}.",
                    e.text().unwrap_or_else(|| "no details".into())
                ),
            )
            .into());
        }
        return Ok(None);
    };
    let info = job_info_from_record(j);
    if info.job_id.is_empty() {
        anyhow::bail!(SlurmApiError::new(
            SlurmErrorKind::Unavailable,
            None,
            "slurmrestd sent a job record without a job id.".into(),
        ));
    }
    Ok(Some(info))
}

fn job_info_from_record(j: JobRecord) -> JobInfo {
    let flags = job_state_flags(&j.job_state);
    let state = map_job_state_flags(&flags);
    let raw_state = base_state(&flags).unwrap_or_default();
    let reason = j
        .state_reason
        .as_str()
        .map(str::to_string)
        .filter(|r| !r.trim().is_empty() && !r.eq_ignore_ascii_case("none"));
    let time_limit = no_val_number(&j.time_limit);
    JobInfo {
        job_id: job_id_to_string(&j.job_id),
        state,
        nodes: expand_nodelist(j.nodes.as_deref().unwrap_or_default()),
        raw_state,
        reason,
        state_flags: flags,
        batch_host: j.batch_host.filter(|h| !h.trim().is_empty()),
        name: j.name.filter(|n| !n.is_empty()),
        user: j.user_name.filter(|n| !n.is_empty()),
        reported_port: comment_text(&j.comment)
            .as_deref()
            .and_then(port_from_comment),
        restart_count: no_val_number(&j.restart_cnt).flatten().unwrap_or(0),
        start_time: unix_time(&j.start_time),
        end_time: unix_time(&j.end_time),
        time_limit_mins: time_limit.flatten().filter(|m| *m > 0),
        time_limit_unlimited: matches!(time_limit, Some(None)),
        submit_time: None,
    }
}

/// Interpret a `GET /job/{id}` response. `Ok(None)` ("gone") only for an
/// explicit job-not-found answer, by Slurm error number or text, whatever
/// the HTTP status; every other failure is `Err` so the tick holds.
fn job_lookup_from_response(
    status: u16,
    body: &[u8],
    job_id: &str,
    version: &str,
) -> anyhow::Result<Option<JobInfo>> {
    if envelope(body)
        .errors
        .iter()
        .any(SlurmMessage::is_job_not_found)
    {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(
            error_from_response(status, body, &format!("reading job {job_id}"), version).into(),
        );
    }
    parse_job(body)
}

/// Slurm base states (the first that is one of these names the job).
const PENDING_STATES: &[&str] = &[
    "PENDING",
    "CONFIGURING",
    "SUSPENDED",
    "REQUEUED",
    "REQUEUE_HOLD",
    "REQUEUE_FED",
    "SPECIAL_EXIT",
    "RESV_DEL_HOLD",
    "RESIZING_PENDING",
];
const RUNNING_STATES: &[&str] = &[
    "RUNNING",
    "COMPLETING",
    "SIGNALING",
    "RESIZING",
    "STOPPED",
    "STAGE_OUT",
    "POWER_UP_NODE",
    "UPDATE_DB",
];
const ENDED_STATES: &[&str] = &[
    "COMPLETED",
    "FAILED",
    "CANCELLED",
    "TIMEOUT",
    "NODE_FAIL",
    "PREEMPTED",
    "BOOT_FAIL",
    "DEADLINE",
    "OUT_OF_MEMORY",
    "REVOKED",
    "LAUNCH_FAILED",
];

fn base_state(flags: &[String]) -> Option<String> {
    let known = |f: &String| {
        ENDED_STATES.contains(&f.as_str())
            || PENDING_STATES.contains(&f.as_str())
            || RUNNING_STATES.contains(&f.as_str())
    };
    flags
        .iter()
        .find(|f| known(f))
        .or_else(|| flags.first())
        .cloned()
}

/// Map a job's state flags (any version) onto the coarse [`JobState`]. A
/// terminal base state wins; a requeue or hold flag on a job that is not
/// ended means it waits in the queue again. A state this build does not know
/// is [`JobState::Unknown`] rather than "gone", so nothing is torn down on a
/// guess.
pub fn map_job_state_flags(flags: &[String]) -> JobState {
    let has = |set: &[&str]| flags.iter().any(|f| set.contains(&f.as_str()));
    if flags.is_empty() {
        JobState::Unknown
    } else if has(ENDED_STATES) {
        JobState::Gone
    } else if has(PENDING_STATES) {
        JobState::Pending
    } else if has(RUNNING_STATES) {
        JobState::Running
    } else {
        JobState::Unknown
    }
}

/// Map a single Slurm job_state string onto the coarse [`JobState`].
pub fn map_job_state(s: &str) -> JobState {
    map_job_state_flags(&[s.to_ascii_uppercase()])
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

/// A `{set, infinite, number}` value as job_desc_msg takes it (v0.0.40 on).
fn no_val(number: i64) -> serde_json::Value {
    serde_json::json!({ "set": true, "infinite": false, "number": number })
}

/// Render the slurmrestd `job/submit` request body for `job`. Pure (no HTTP)
/// so the payload shape is unit-testable.
///
/// A non-empty `hf_token` is passed to the job as `HF_TOKEN` and
/// `HUGGING_FACE_HUB_TOKEN` in the job `environment`, never in the script
/// body, which Slurm keeps on disk and shows in `scontrol write batch_script`.
///
/// Every field is a job_desc_msg field in v0.0.40 through v0.0.45. The script
/// goes in `job.script` (the top-level `script` is deprecated). Optional
/// fields are omitted when unset so they do not override cluster defaults.
pub fn submit_body(job: &JobSubmit, hf_token: &str) -> serde_json::Value {
    // slurmrestd requires a non-empty environment, given as a list of
    // `NAME=value` strings. The PATH covers where container runtimes usually
    // live; launch commands can extend it.
    let mut environment = vec![
        "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/opt/apptainer/bin:/opt/singularity/bin".to_string(),
    ];
    let hf_token = hf_token.trim();
    if !hf_token.is_empty() {
        environment.push(format!("HF_TOKEN={hf_token}"));
        environment.push(format!("HUGGING_FACE_HUB_TOKEN={hf_token}"));
    }
    let cwd = job_working_dir(job);
    let mut spec = serde_json::json!({
        "name": job.name,
        "nodes": job.nodes.to_string(),
        "tasks": 1,
        "current_working_directory": cwd,
        "environment": environment,
        // Not requeueable: after a preemption or node failure the job ends
        // (PREEMPTED / NODE_FAIL) and the provisioner starts a fresh one,
        // instead of the job silently waiting in the queue under its old id.
        "requeue": false,
        "standard_output": format!("{cwd}/{}-%j.out", job.name),
        "script": job.script,
    });
    let m = spec.as_object_mut().expect("object literal");
    if !job.partition.is_empty() {
        m.insert("partition".into(), serde_json::json!(job.partition));
    }
    // Multi-node jobs (vLLM over Ray, for example) start one worker per node
    // with `srun` inside the batch script, which needs one task per node.
    if job.nodes > 1 {
        m.insert("tasks".into(), serde_json::json!(job.nodes));
        m.insert("tasks_per_node".into(), serde_json::json!(1));
    }
    if !job.gres.is_empty() {
        m.insert(
            "tres_per_node".into(),
            serde_json::json!(slurm_values::gres_to_tres_per_node(&job.gres)),
        );
    }
    if let Some(t) = &job.time_limit {
        // slurmrestd takes minutes (or infinite), not a walltime string.
        match slurm_values::parse_time_limit(t) {
            Ok(Some(slurm_values::SlurmTimeLimit::Minutes(mins))) => {
                m.insert("time_limit".into(), no_val(mins));
            }
            Ok(Some(slurm_values::SlurmTimeLimit::Unlimited)) => {
                m.insert(
                    "time_limit".into(),
                    serde_json::json!({ "set": true, "infinite": true, "number": 0 }),
                );
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(time_limit = %t, error = %e,
                "time limit not understood; the partition default applies"),
        }
    }
    if let Some(a) = &job.account {
        m.insert("account".into(), serde_json::json!(a));
    }
    if let Some(q) = &job.qos {
        m.insert("qos".into(), serde_json::json!(q));
    }
    if let Some(c) = &job.constraints {
        m.insert("constraints".into(), serde_json::json!(c));
    }
    if let Some(e) = &job.exclude {
        m.insert("excluded_nodes".into(), serde_json::json!(e));
    }
    if let Some(c) = job.cpus_per_task.filter(|c| *c > 0) {
        m.insert("cpus_per_task".into(), serde_json::json!(c));
    }
    // Megabytes. `submit` has already turned 0 into the node's size where it
    // could; a 0 still here asks Slurm for all of each node's memory.
    if let Some(mb) = job.mem_mb.filter(|mb| *mb >= 0) {
        m.insert("memory_per_node".into(), no_val(mb));
    }
    if !job.log_output_dir.is_empty() {
        m.insert(
            "standard_error".into(),
            serde_json::json!(format!("{cwd}/{}-%j.err", job.name)),
        );
    }
    serde_json::json!({ "job": spec })
}

/// Read a submit answer: the job id, and Slurm's warnings and job_submit
/// message as notes.
fn submitted_from_response(body: &[u8]) -> Result<SubmittedJob, SlurmApiError> {
    let v: serde_json::Value = serde_json::from_slice(body).map_err(|_| {
        SlurmApiError::new(
            SlurmErrorKind::NoAnswer,
            None,
            format!(
                "slurmrestd accepted the submit but its answer could not be read ({}), so \
                 the job may exist without obleth knowing its id.",
                excerpt(body, 120)
            ),
        )
    })?;
    let id = [
        v.get("job_id"),
        v.get("result").and_then(|r| r.get("job_id")),
    ]
    .into_iter()
    .flatten()
    .find_map(|j| {
        j.as_i64()
            .filter(|n| *n > 0)
            .map(|n| n.to_string())
            .or_else(|| j.as_str().filter(|s| !s.is_empty()).map(String::from))
    })
    .ok_or_else(|| {
        SlurmApiError::new(
            SlurmErrorKind::NoAnswer,
            None,
            "slurmrestd answered the submit without a job id, so the job may exist without \
                 obleth knowing its id."
                .into(),
        )
    })?;
    let env = envelope(body);
    let mut notes: Vec<String> = env
        .warnings
        .iter()
        .filter_map(|w| w.text().map(|t| format!("Slurm warned: {t}.")))
        .collect();
    if let Some(m) = job_submit_user_msg(body) {
        notes.push(format!("job_submit said: {m}."));
    }
    Ok(SubmittedJob { job_id: id, notes })
}

/// What a `/ping` answer says about the controller.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PingInfo {
    /// `"UP"` when a controller answered, `"DOWN"` when none did, `None`
    /// when the answer did not say.
    pub slurmctld: Option<String>,
    /// The Slurm release, e.g. `26.05.5`.
    pub slurm_release: Option<String>,
}

/// Read a `/ping` body. v0.0.40 to v0.0.44 report `pinged: "UP"`; v0.0.45
/// drops `pinged` and keeps `responding`.
pub fn parse_ping(body: &[u8]) -> PingInfo {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(body) else {
        return PingInfo::default();
    };
    let pings = v.get("pings").and_then(|p| p.as_array());
    let slurmctld = pings.filter(|p| !p.is_empty()).map(|pings| {
        let up = pings.iter().any(|p| {
            p.get("responding").and_then(|r| r.as_bool()) == Some(true)
                || p.get("pinged")
                    .and_then(|r| r.as_str())
                    .is_some_and(|s| s.eq_ignore_ascii_case("UP"))
        });
        if up { "UP" } else { "DOWN" }.to_string()
    });
    PingInfo {
        slurmctld,
        slurm_release: slurm_release(&v),
    }
}

/// The Slurm release from any answer's `meta`.
pub fn slurm_release(v: &serde_json::Value) -> Option<String> {
    let s = v
        .get("meta")
        .and_then(|m| m.get("slurm").or_else(|| m.get("Slurm")))?;
    if let Some(r) = s
        .get("release")
        .and_then(|r| r.as_str())
        .filter(|r| !r.is_empty())
    {
        return Some(r.to_string());
    }
    let ver = s.get("version")?;
    let part = |k: &str| {
        ver.get(k).and_then(|x| {
            x.as_i64()
                .map(|n| n.to_string())
                .or_else(|| x.as_str().map(String::from))
        })
    };
    Some(format!(
        "{}.{}.{}",
        part("major")?,
        part("minor")?,
        part("micro")?
    ))
}

/// One slurmdbd job record as a [`JobInfo`]: its id, state, reason, nodes,
/// name, owner and submit, start and end times.
fn db_job_info(j: &serde_json::Value) -> Option<JobInfo> {
    let state = j.get("state");
    let flags = job_state_flags(state.and_then(|s| s.get("current"))?);
    let time = |k: &str| j.get("time").and_then(|t| t.get(k)).and_then(unix_time);
    let info = JobInfo {
        job_id: job_id_to_string(j.get("job_id")?),
        state: map_job_state_flags(&flags),
        raw_state: base_state(&flags).unwrap_or_default(),
        reason: state
            .and_then(|s| s.get("reason"))
            .and_then(|r| r.as_str())
            .map(str::to_string)
            .filter(|r| !r.trim().is_empty() && !r.eq_ignore_ascii_case("none")),
        nodes: expand_nodelist(j.get("nodes").and_then(|n| n.as_str()).unwrap_or("")),
        name: j.get("name").and_then(|n| n.as_str()).map(String::from),
        user: j.get("user").and_then(|u| u.as_str()).map(String::from),
        state_flags: flags,
        submit_time: time("submission"),
        start_time: time("start"),
        end_time: time("end"),
        ..Default::default()
    };
    (!info.job_id.is_empty()).then_some(info)
}

/// The live (pending or running) jobs in a slurmdbd job list with exactly
/// this name, owned by `user` when one is configured.
fn parse_db_jobs(v: &serde_json::Value, user: &str, name: &str) -> Vec<JobInfo> {
    v.get("jobs")
        .and_then(|j| j.as_array())
        .into_iter()
        .flatten()
        .filter_map(db_job_info)
        .filter(|j| j.name.as_deref() == Some(name))
        .filter(|j| user.is_empty() || j.user.as_deref().is_none_or(|u| u == user))
        .filter(|j| matches!(j.state, JobState::Pending | JobState::Running))
        .collect()
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

pub struct Slurmrestd {
    http: reqwest::Client,
    base: String,
    version: String,
    user: String,
    jwt: String,
    /// Hugging Face token passed to submitted jobs via their environment.
    /// Empty = none. (No `Debug` derive on this struct: it holds secrets.)
    hf_token: String,
    /// Jobs whose name does not start with this are not obleth's. Empty
    /// skips the check.
    job_name_prefix: String,
}

impl Slurmrestd {
    /// Build a client for a known API version (`v0.0.43`). Use [`connect`]
    /// when the setting may be `auto`. Takes a shared `reqwest::Client`
    /// (cheap to clone) rather than building a connection pool per tick.
    ///
    /// [`connect`]: Slurmrestd::connect
    pub fn new(http: reqwest::Client, url: &str, version: &str, user: &str, jwt: &str) -> Self {
        Self {
            http,
            base: url.trim().trim_end_matches('/').to_string(),
            version: version.trim().to_string(),
            user: user.trim().to_string(),
            jwt: jwt.trim().to_string(),
            hf_token: String::new(),
            job_name_prefix: String::new(),
        }
    }

    /// Build a client, resolving an `auto` (or empty) API version against the
    /// server (cached per URL and user for ten minutes).
    pub async fn connect(
        http: reqwest::Client,
        url: &str,
        version_setting: &str,
        user: &str,
        jwt: &str,
    ) -> Result<Self, SlurmApiError> {
        let version =
            resolve_api_version(&http, url.trim(), version_setting, user.trim(), jwt.trim())
                .await?;
        Ok(Self::new(http, url, &version, user, jwt))
    }

    /// The API version this client uses.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Pass a Hugging Face token to every job this client submits (as
    /// `HF_TOKEN` / `HUGGING_FACE_HUB_TOKEN` in the job environment). Empty
    /// leaves the environment without it.
    pub fn with_hf_token(mut self, token: &str) -> Self {
        self.hf_token = token.trim().to_string();
        self
    }

    /// Treat a job whose name does not start with `prefix` as not obleth's
    /// (a reused job id): `get_job` reports it as gone and nothing cancels it.
    pub fn with_job_name_prefix(mut self, prefix: &str) -> Self {
        self.job_name_prefix = prefix.to_string();
        self
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.header("X-SLURM-USER-NAME", &self.user)
            .header("X-SLURM-USER-TOKEN", &self.jwt)
    }

    fn url(&self, plugin: &str, path: &str) -> String {
        format!("{}/{plugin}/{}/{path}", self.base, self.version)
    }

    /// Send a request and read the whole body before looking at it, so a
    /// failure is reported from the status and Slurm's `errors[]` rather than
    /// as a JSON decoding error.
    async fn send(
        &self,
        rb: reqwest::RequestBuilder,
        what: &str,
    ) -> Result<(u16, Vec<u8>), SlurmApiError> {
        let resp = self
            .auth(rb)
            .send()
            .await
            .map_err(|e| no_answer(&self.base, what, &e))?;
        let status = resp.status().as_u16();
        let body = resp
            .bytes()
            .await
            .map_err(|e| no_answer(&self.base, what, &e))?;
        Ok((status, body.to_vec()))
    }

    /// GET a JSON document, failing with a plain sentence.
    async fn get_json(&self, url: String, what: &str) -> Result<serde_json::Value, SlurmApiError> {
        let (status, body) = self.send(self.http.get(&url), what).await?;
        if !(200..300).contains(&status) {
            return Err(error_from_response(status, &body, what, &self.version));
        }
        serde_json::from_slice(&body).map_err(|_| {
            SlurmApiError::new(
                SlurmErrorKind::Unavailable,
                Some(status),
                format!(
                    "slurmrestd answered {what} with something that is not JSON: {}.",
                    excerpt(&body, 120)
                ),
            )
        })
    }

    /// `GET /ping`: whether a controller answers, and the Slurm release.
    /// slurmrestd may answer a ping without checking the token; use
    /// [`check_token`](Self::check_token) for that.
    pub async fn ping(&self) -> Result<PingInfo, SlurmApiError> {
        let what = "pinging the controller";
        let (status, body) = self
            .send(self.http.get(self.url("slurm", "ping")), what)
            .await?;
        if !(200..300).contains(&status) {
            return Err(error_from_response(status, &body, what, &self.version));
        }
        Ok(parse_ping(&body))
    }

    /// An authenticated read (the partition list) that fails when the token
    /// is not accepted. Returns the number of partitions.
    pub async fn check_token(&self) -> Result<usize, SlurmApiError> {
        let v = self
            .get_json(self.url("slurm", "partitions"), "reading partitions")
            .await?;
        Ok(parse_partitions(&v).len())
    }

    async fn read_nodes(&self) -> Result<Vec<NodeInfo>, SlurmApiError> {
        let v = self
            .get_json(self.url("slurm", "nodes"), "reading nodes")
            .await?;
        Ok(parse_nodes(&v))
    }

    /// `--mem=0` means all of the node's memory, but some sites' submit
    /// plugins read 0 as unset and give a small default instead. Ask for the
    /// node's size outright. If it can't be found, 0 is sent as before.
    async fn with_whole_node_mem(&self, job: &JobSubmit) -> JobSubmit {
        let mut job = job.clone();
        if job.mem_mb != Some(0) {
            return job;
        }
        match self.read_nodes().await {
            Ok(nodes) => match whole_node_mem_mb(&nodes, &job.partition) {
                Some(mb) => {
                    tracing::info!(job = %job.name, partition = %job.partition, mem_mb = mb,
                        "asking for all of the node's memory by size");
                    job.mem_mb = Some(mb);
                }
                None => tracing::warn!(job = %job.name, partition = %job.partition,
                    "no node memory known for the partition; sending --mem=0"),
            },
            Err(e) => tracing::warn!(job = %job.name, error = %e,
                "could not read node memory; sending --mem=0"),
        }
        job
    }

    /// Whether a job record belongs to obleth's user and naming. Fields the
    /// record leaves out are not held against it.
    fn is_ours(&self, job: &JobInfo) -> bool {
        let user_ok = self.user.is_empty() || job.user.as_deref().is_none_or(|u| u == self.user);
        let name_ok = self.job_name_prefix.is_empty()
            || job
                .name
                .as_deref()
                .is_none_or(|n| n.starts_with(&self.job_name_prefix));
        user_ok && name_ok
    }
}

#[async_trait]
impl SlurmClient for Slurmrestd {
    async fn submit(&self, job: &JobSubmit) -> anyhow::Result<SubmittedJob> {
        let job = &self.with_whole_node_mem(job).await;
        let body = submit_body(job, &self.hf_token);
        let (status, resp) = self
            .send(
                self.http.post(self.url("slurm", "job/submit")).json(&body),
                "submitting the job",
            )
            .await?;
        if !(200..300).contains(&status) {
            return Err(submit_error_from_response(status, &resp, &self.version).into());
        }
        Ok(submitted_from_response(&resp)?)
    }

    async fn cancel(&self, job_id: &str) -> anyhow::Result<()> {
        let what = format!("cancelling job {job_id}");
        let (status, body) = self
            .send(
                self.http
                    .delete(self.url("slurm", &format!("job/{job_id}"))),
                &what,
            )
            .await?;
        if (200..300).contains(&status) {
            return Ok(());
        }
        // Already gone counts as cancelled.
        if envelope(&body)
            .errors
            .iter()
            .any(SlurmMessage::is_job_not_found)
        {
            return Ok(());
        }
        Err(error_from_response(status, &body, &what, &self.version).into())
    }

    async fn get_job(&self, job_id: &str) -> anyhow::Result<Option<JobInfo>> {
        let what = format!("reading job {job_id}");
        let (status, body) = self
            .send(
                self.http.get(self.url("slurm", &format!("job/{job_id}"))),
                &what,
            )
            .await?;
        let job = job_lookup_from_response(status, &body, job_id, &self.version)?;
        Ok(job.filter(|j| {
            let ours = self.is_ours(j);
            if !ours {
                tracing::warn!(job_id, name = ?j.name, user = ?j.user,
                    "job id now belongs to a job obleth did not submit; treating it as gone");
            }
            ours
        }))
    }

    async fn discover_resources(&self) -> anyhow::Result<ClusterResources> {
        let mut out = ClusterResources::default();
        let mut failed: Vec<String> = Vec::new();
        let mut worked = 0;
        match self
            .get_json(self.url("slurm", "partitions"), "reading partitions")
            .await
        {
            Ok(v) => {
                worked += 1;
                if out.slurm_release.is_none() {
                    out.slurm_release = slurm_release(&v);
                }
                out.partitions = parse_partitions(&v);
            }
            Err(e) => failed.push(e.message),
        }
        match self
            .get_json(self.url("slurm", "nodes"), "reading nodes")
            .await
        {
            Ok(v) => {
                worked += 1;
                out.nodes = parse_nodes(&v);
            }
            Err(e) => failed.push(e.message),
        }
        let assoc_url = self.url("slurmdb", "associations");
        let rb = if self.user.is_empty() {
            self.http.get(&assoc_url)
        } else {
            self.http
                .get(&assoc_url)
                .query(&[("user", self.user.as_str())])
        };
        let what = "reading your accounts from the accounting database";
        match self.send(rb, what).await {
            Ok((status, body)) if (200..300).contains(&status) => {
                match serde_json::from_slice::<serde_json::Value>(&body) {
                    Ok(v) => {
                        worked += 1;
                        let v = filter_associations(v, &self.user);
                        let (a, q) = parse_associations(&v);
                        out.accounts = a;
                        out.qos = q;
                        out.associations = parse_association_list(&v);
                    }
                    Err(_) => failed.push(format!(
                        "slurmrestd answered {what} with something that is not JSON."
                    )),
                }
            }
            Ok((status, body)) => {
                failed.push(error_from_response(status, &body, what, &self.version).message)
            }
            Err(e) => failed.push(e.message),
        }
        if !failed.is_empty() {
            for f in &failed {
                tracing::warn!(error = %f, "slurm resource read failed");
            }
            out.partial = worked > 0;
            out.error = Some(failed.join(" "));
        }
        Ok(out)
    }

    async fn check_auth(&self) -> anyhow::Result<()> {
        self.check_token().await?;
        Ok(())
    }

    async fn find_live_jobs_by_name(&self, name: &str) -> anyhow::Result<Vec<JobInfo>> {
        let mut q: Vec<(&str, &str)> = vec![("job_name", name)];
        if !self.user.is_empty() {
            q.push(("users", self.user.as_str()));
        }
        let rb = self.http.get(self.url("slurmdb", "jobs")).query(&q);
        let what = "looking up jobs by name in the accounting database";
        let (status, body) = self.send(rb, what).await?;
        if !(200..300).contains(&status) {
            return Err(error_from_response(status, &body, what, &self.version).into());
        }
        let v: serde_json::Value = serde_json::from_slice(&body).unwrap_or_default();
        Ok(parse_db_jobs(&v, &self.user, name))
    }

    async fn accounting_job(&self, job_id: &str) -> anyhow::Result<Option<JobInfo>> {
        let what = format!("looking up job {job_id} in the accounting database");
        let (status, body) = self
            .send(
                self.http.get(self.url("slurmdb", &format!("job/{job_id}"))),
                &what,
            )
            .await?;
        if envelope(&body)
            .errors
            .iter()
            .any(SlurmMessage::is_job_not_found)
        {
            return Ok(None);
        }
        if !(200..300).contains(&status) {
            return Err(error_from_response(status, &body, &what, &self.version).into());
        }
        let v: serde_json::Value = serde_json::from_slice(&body).map_err(|_| {
            SlurmApiError::new(
                SlurmErrorKind::Unavailable,
                Some(status),
                format!("slurmrestd answered {what} with something that is not JSON."),
            )
        })?;
        Ok(v.get("jobs")
            .and_then(|a| a.as_array())
            .into_iter()
            .flatten()
            .filter_map(db_job_info)
            .find(|j| j.job_id == job_id)
            .filter(|j| self.is_ours(j)))
    }

    async fn node_addresses(&self) -> anyhow::Result<HashMap<String, String>> {
        Ok(self
            .read_nodes()
            .await?
            .into_iter()
            .filter_map(|n| n.address.map(|a| (n.name, a)))
            .collect())
    }
}

/// Keep only `user`'s associations (a server that ignores the `user` filter
/// would otherwise list every user's accounts).
fn filter_associations(mut v: serde_json::Value, user: &str) -> serde_json::Value {
    if user.is_empty() {
        return v;
    }
    if let Some(arr) = v.get_mut("associations").and_then(|a| a.as_array_mut()) {
        arr.retain(|a| {
            a.get("user")
                .and_then(|u| u.as_str())
                .is_none_or(|u| u == user)
        });
    }
    v
}

/// Expand a Slurm hostlist: `gpu[01-04,07]`, `a-[1-2]b`, `n[1-2]-ib[3-4]`,
/// `x1,y[2-3]`. Zero padding follows the range's first number. A malformed
/// group is kept as written.
pub fn expand_nodelist(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for item in split_top_level(s.trim()) {
        if !item.is_empty() {
            out.extend(expand_host(&item));
        }
    }
    out
}

/// Split on commas outside brackets.
fn split_top_level(s: &str) -> Vec<String> {
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth -= 1,
            ',' if depth == 0 => {
                parts.push(std::mem::take(&mut cur).trim().to_string());
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    parts.push(cur.trim().to_string());
    parts
}

/// Cap on hosts from one expression, so a typo cannot allocate millions.
const MAX_EXPANDED_HOSTS: usize = 65_536;

fn expand_host(item: &str) -> Vec<String> {
    let Some(open) = item.find('[') else {
        return vec![item.to_string()];
    };
    let Some(close_rel) = item[open..].find(']') else {
        return vec![item.to_string()];
    };
    let close = open + close_rel;
    let prefix = &item[..open];
    let inner = &item[open + 1..close];
    let rest = expand_host(&item[close + 1..]);
    let mut out = Vec::new();
    for part in inner.split(',') {
        let part = part.trim();
        let (lo, hi) = part.split_once('-').unwrap_or((part, part));
        let (Ok(a), Ok(b)) = (lo.parse::<u64>(), hi.parse::<u64>()) else {
            return vec![item.to_string()];
        };
        if b < a {
            return vec![item.to_string()];
        }
        let width = lo.len();
        for n in a..=b {
            for suffix in &rest {
                if out.len() >= MAX_EXPANDED_HOSTS {
                    return out;
                }
                out.push(format!("{prefix}{n:0width$}{suffix}"));
            }
        }
    }
    out
}

/// Read a slurmrestd time value that may be `{"number":N}` (minutes), a bare
/// number, or a string. Returns a display string of minutes, `UNLIMITED`, or None.
fn time_minutes(v: &serde_json::Value) -> Option<String> {
    match no_val_number(v) {
        Some(Some(n)) => Some(n.to_string()),
        Some(None) => Some("UNLIMITED".to_string()),
        None => v.as_str().filter(|s| !s.is_empty()).map(|s| s.to_string()),
    }
}

/// A slurmrestd field that may be a CSV string or an array of strings.
fn str_list(v: Option<&serde_json::Value>) -> Vec<String> {
    match v {
        Some(serde_json::Value::String(s)) => s
            .split(',')
            .map(|x| x.trim())
            .filter(|x| !x.is_empty())
            .map(String::from)
            .collect(),
        Some(serde_json::Value::Array(a)) => a
            .iter()
            .filter_map(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .collect(),
        _ => vec![],
    }
}

pub fn parse_partitions(v: &serde_json::Value) -> Vec<PartitionInfo> {
    v.get("partitions")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    let name = p.get("name").and_then(|x| x.as_str())?.to_string();
                    let nodes = p
                        .get("nodes")
                        .and_then(|n| n.get("configured").cloned().or_else(|| Some(n.clone())))
                        .map(|n| str_list(Some(&n)))
                        .unwrap_or_default();
                    Some(PartitionInfo {
                        name,
                        nodes,
                        default_time: p
                            .get("defaults")
                            .and_then(|d| d.get("time"))
                            .and_then(time_minutes),
                        max_time: p
                            .get("maximums")
                            .and_then(|d| d.get("time"))
                            .and_then(time_minutes),
                        // slurmrestd reports AllowAccounts as "ALL" when unset.
                        allowed_accounts: str_list(
                            p.get("accounts").and_then(|a| a.get("allowed")),
                        )
                        .into_iter()
                        .filter(|a| !a.eq_ignore_ascii_case("ALL"))
                        .collect(),
                        denied_accounts: str_list(p.get("accounts").and_then(|a| a.get("deny"))),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn parse_nodes(v: &serde_json::Value) -> Vec<NodeInfo> {
    v.get("nodes")
        .and_then(|x| x.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|n| {
                    let name = n.get("name").and_then(|x| x.as_str())?.to_string();
                    let number = |key: &str| n.get(key).and_then(|m| no_val_number(m).flatten());
                    let real_memory_mb = number("real_memory");
                    let address = n
                        .get("address")
                        .and_then(|x| x.as_str())
                        .map(str::trim)
                        .filter(|a| !a.is_empty() && *a != name)
                        .map(String::from);
                    Some(NodeInfo {
                        specialized_memory_mb: number("specialized_memory").filter(|m| *m > 0),
                        partitions: str_list(n.get("partitions")),
                        gres: n
                            .get("gres")
                            .and_then(|x| x.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        cpus: number("cpus"),
                        real_memory_mb,
                        features: str_list(n.get("features")),
                        state: str_list(n.get("state"))
                            .into_iter()
                            .map(|s| s.to_ascii_uppercase())
                            .collect(),
                        alloc_cpus: number("alloc_cpus"),
                        alloc_memory_mb: number("alloc_memory"),
                        address,
                        name,
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Each association's account, partition (None = all) and QoS, deduplicated.
pub fn parse_association_list(v: &serde_json::Value) -> Vec<AssociationInfo> {
    let mut out: Vec<AssociationInfo> = Vec::new();
    for a in v
        .get("associations")
        .and_then(|x| x.as_array())
        .into_iter()
        .flatten()
    {
        let Some(account) = a
            .get("account")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let partition = a
            .get("partition")
            .and_then(|x| x.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from);
        let info = AssociationInfo {
            account: account.to_string(),
            partition,
            qos: str_list(a.get("qos")),
        };
        if !out.contains(&info) {
            out.push(info);
        }
    }
    out
}

pub fn parse_associations(v: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let mut accounts = std::collections::BTreeSet::new();
    let mut qos = std::collections::BTreeSet::new();
    if let Some(arr) = v.get("associations").and_then(|x| x.as_array()) {
        for a in arr {
            if let Some(acc) = a.get("account").and_then(|x| x.as_str()) {
                if !acc.is_empty() {
                    accounts.insert(acc.to_string());
                }
            }
            for q in str_list(a.get("qos")) {
                qos.insert(q);
            }
        }
    }
    (accounts.into_iter().collect(), qos.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn time_limit_to_minutes(raw: &str) -> Option<i64> {
        match slurm_values::parse_time_limit(raw) {
            Ok(Some(slurm_values::SlurmTimeLimit::Minutes(m))) => Some(m),
            _ => None,
        }
    }

    fn spec() -> ManagedModelSpec {
        ManagedModelSpec {
            model_id: uuid::Uuid::new_v4(),
            enabled: true,
            partition: "gpu-preempt".into(),
            gres: "gpu:h100:2".into(),
            nodes: 1,
            constraints: None,
            exclude: None,
            account: None,
            qos: None,
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
            ..Default::default()
        }
    }

    #[test]
    fn submit_body_carries_hf_token_in_environment_only_when_set() {
        let job = job_submit_from_spec(&spec(), "nemotron", "obleth-", 8000, 8);
        let with = submit_body(&job, " hf_secret ");
        let env: Vec<&str> = with["job"]["environment"]
            .as_array()
            .expect("environment is a list of NAME=value strings")
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(env.contains(&"HF_TOKEN=hf_secret"));
        assert!(env.contains(&"HUGGING_FACE_HUB_TOKEN=hf_secret"));
        assert!(
            env.iter().any(|e| e.starts_with("PATH=")),
            "PATH is still set"
        );
        assert!(
            !with["job"]["script"]
                .as_str()
                .unwrap()
                .contains("hf_secret"),
            "token must not be written into the script"
        );

        let without = submit_body(&job, "");
        let env: Vec<&str> = without["job"]["environment"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        assert!(!env
            .iter()
            .any(|e| e.starts_with("HF_TOKEN=") || e.starts_with("HUGGING_FACE_HUB_TOKEN=")));
        assert!(env.iter().any(|e| e.starts_with("PATH=")));
    }

    #[test]
    fn submit_body_asks_for_one_task_per_node_on_multi_node_jobs() {
        let single = job_submit_from_spec(&spec(), "m", "obleth-", 8000, 8);
        let body = submit_body(&single, "");
        assert_eq!(body["job"]["nodes"], "1");
        assert_eq!(body["job"]["tasks"], 1);
        assert!(body["job"].get("tasks_per_node").is_none());

        let mut s = spec();
        s.nodes = 4;
        let multi = job_submit_from_spec(&s, "m", "obleth-", 8000, 8);
        let body = submit_body(&multi, "");
        assert_eq!(body["job"]["nodes"], "4");
        assert_eq!(body["job"]["tasks"], 4);
        assert_eq!(body["job"]["tasks_per_node"], 1);
    }

    #[test]
    fn builder_is_generic_and_tags_name() {
        let j = job_submit_from_spec(&spec(), "nemotron", "obleth-", 8000, 8);
        assert_eq!(j.name, "obleth-nemotron");
        assert_eq!(j.partition, "gpu-preempt");
        assert!(j.script.contains(
            "${OBLETH_CONTAINER_RUN:-apptainer exec --nv} 'vllm.sif' vllm serve nemotron"
        ));
    }

    #[test]
    fn time_limit_to_minutes_covers_every_slurm_form() {
        // minutes / MM:SS / HH:MM:SS
        assert_eq!(time_limit_to_minutes("120"), Some(120));
        assert_eq!(time_limit_to_minutes("30:00"), Some(30));
        assert_eq!(time_limit_to_minutes("4:00:00"), Some(240));
        // days: D-HH / D-HH:MM / D-HH:MM:SS (the dashboard's default form)
        assert_eq!(time_limit_to_minutes("0-04:00:00"), Some(240));
        assert_eq!(time_limit_to_minutes("1-00"), Some(24 * 60));
        assert_eq!(
            time_limit_to_minutes("2-12:30"),
            Some((2 * 24 + 12) * 60 + 30)
        );
        // seconds round up to the next whole minute (never under-allocate)
        assert_eq!(time_limit_to_minutes("0:30"), Some(1));
        assert_eq!(time_limit_to_minutes("1:01"), Some(2));
    }

    #[test]
    fn time_limit_to_minutes_rejects_garbage_and_zero() {
        for bad in ["", "  ", "4h", "abc", "1-", "1:2:3:4", "-5"] {
            assert_eq!(time_limit_to_minutes(bad), None, "should reject {bad:?}");
        }
        assert_eq!(time_limit_to_minutes("0"), None);
        assert_eq!(time_limit_to_minutes("0:00"), None);
    }

    #[test]
    fn preamble_injected_before_apptainer_when_set() {
        let mut s = spec();
        s.preamble = "module load apptainer/1.3.4".into();
        let j = job_submit_from_spec(&s, "nemotron", "obleth-", 8000, 8);
        let lines: Vec<&str> = j.script.lines().collect();
        let apptainer_idx = lines
            .iter()
            .position(|l| l.contains("apptainer exec"))
            .unwrap();
        let module_idx = lines
            .iter()
            .position(|l| l.contains("module load apptainer"))
            .unwrap();
        assert!(
            module_idx < apptainer_idx,
            "preamble must come before apptainer exec"
        );
    }

    #[test]
    fn empty_preamble_produces_no_extra_line() {
        let j = job_submit_from_spec(&spec(), "nemotron", "obleth-", 8000, 8);
        assert!(
            !j.script.contains("module load"),
            "no preamble lines when preamble is empty"
        );
        assert!(j.script.contains(
            "${OBLETH_CONTAINER_RUN:-apptainer exec --nv} 'vllm.sif' vllm serve nemotron"
        ));
    }

    #[test]
    fn image_with_shell_metacharacters_is_quoted() {
        let mut s = spec();
        s.image = "weird'$(rm -rf /); image.sif".into();
        let j = job_submit_from_spec(&s, "nemotron", "obleth-", 8000, 8);
        assert!(j.script.contains(
            "${OBLETH_CONTAINER_RUN:-apptainer exec --nv} 'weird'\\''$(rm -rf /); image.sif'"
        ));
    }

    #[test]
    fn blank_image_runs_launch_command_bare_metal() {
        let mut s = spec();
        s.image = "  ".into();
        s.preamble = "module load cuda".into();
        s.launch_command = "llama-server -m model.gguf --port 8000".into();
        let j = job_submit_from_spec(&s, "glm", "obleth-", 8000, 8);
        assert!(
            !j.script.contains("apptainer"),
            "no container wrap when image is blank"
        );
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
        assert_eq!(parse_mem_mb("-4G"), None);
        // `--mem=0` is all of the node's memory, not "unset".
        assert_eq!(parse_mem_mb("0"), Some(0));
        assert_eq!(parse_mem_mb("0G"), Some(0));
    }

    #[test]
    fn mem_zero_asks_slurm_for_the_whole_node() {
        let mut s = spec();
        s.mem = Some("0".into());
        let j = job_submit_from_spec(&s, "m", "obleth-", 8000, 1);
        assert_eq!(submit_body(&j, "")["job"]["memory_per_node"]["number"], 0);
        s.mem = None;
        let j = job_submit_from_spec(&s, "m", "obleth-", 8000, 1);
        assert!(submit_body(&j, "")["job"].get("memory_per_node").is_none());
    }

    fn node(name: &str, partition: &str, real: Option<i64>, spec: Option<i64>) -> NodeInfo {
        NodeInfo {
            name: name.into(),
            partitions: vec![partition.into()],
            gres: String::new(),
            cpus: Some(72),
            real_memory_mb: real,
            specialized_memory_mb: spec,
            features: vec![],
            state: vec!["IDLE".into()],
            alloc_cpus: None,
            alloc_memory_mb: None,
            address: None,
        }
    }

    #[test]
    fn whole_node_mem_is_the_smallest_node_less_what_slurm_keeps() {
        let nodes = vec![
            node("gh1", "arm", Some(587553), Some(3500)),
            node("gh2", "arm", Some(587289), Some(3500)),
            node("gh3", "arm", None, None),
            node("c1", "general", Some(515516), None),
        ];
        assert_eq!(whole_node_mem_mb(&nodes, "arm"), Some(583789));
        assert_eq!(whole_node_mem_mb(&nodes, "general"), Some(515516));
        assert_eq!(whole_node_mem_mb(&nodes, "gpu"), None);
        assert_eq!(whole_node_mem_mb(&nodes, ""), None);
    }

    #[test]
    fn parse_nodes_reads_specialized_memory() {
        let v = serde_json::json!({"nodes": [
            {"name": "gh1", "partitions": ["arm"], "real_memory": 587289, "specialized_memory": 3500},
            {"name": "c1", "partitions": ["general"], "real_memory": 515516, "specialized_memory": 0},
        ]});
        let nodes = parse_nodes(&v);
        assert_eq!(nodes[0].specialized_memory_mb, Some(3500));
        assert_eq!(nodes[1].specialized_memory_mb, None);
    }

    #[test]
    fn windows_line_endings_are_submitted_as_unix_ones() {
        let mut s = spec();
        s.script_body = "#!/bin/bash -l\r\nset -euo pipefail\r\nvllm serve m --port \"$OBLETH_SERVING_PORT\"\r\n".into();
        let j = job_submit_from_spec(&s, "m", "obleth-", 8000, 1);
        assert!(!j.script.contains('\r'), "no carriage returns reach bash");
        assert!(j.script.starts_with("#!/bin/bash -l\n# obleth: bind"));
        assert!(j.script.contains("\nset -euo pipefail\n"));
        // The legacy assembly gets the same treatment.
        let mut s = spec();
        s.preamble = "module load cuda\r\nmodule load apptainer".into();
        s.launch_command = "llama-server --port 8000\r\n".into();
        let j = job_submit_from_spec(&s, "m", "obleth-", 8000, 1);
        assert!(!j.script.contains('\r'));
    }

    #[test]
    fn script_body_with_shebang_gets_preamble_injected() {
        let mut s = spec();
        s.script_body = "#!/bin/bash\nset -e\nllama-server --port 8000\n".into();
        let j = job_submit_from_spec(&s, "glm", "obleth-", 8000, 8);
        // Port preamble is injected after the shebang; script_body wins over legacy assembly.
        assert!(
            j.script.starts_with("#!/bin/bash\n"),
            "shebang retained as first line"
        );
        assert!(
            j.script.contains("OBLETH_SERVING_PORT"),
            "port preamble injected"
        );
        assert!(j.script.contains("llama-server --port 8000"));
        assert!(
            !j.script.contains("apptainer"),
            "script_body overrides legacy assembly"
        );
    }

    #[test]
    fn script_body_without_shebang_gets_wrapped_and_preamble_injected() {
        let mut s = spec();
        s.script_body = "llama-server --port 8000".into();
        let j = job_submit_from_spec(&s, "glm", "obleth-", 8000, 8);
        assert!(j.script.starts_with("#!/bin/bash -l\n"), "shebang added");
        assert!(
            j.script.contains("OBLETH_SERVING_PORT"),
            "port preamble injected"
        );
        assert!(j.script.contains("llama-server --port 8000"));
    }

    #[test]
    fn port_preamble_uses_correct_window() {
        let j = job_submit_from_spec(&spec(), "nemotron", "obleth-", 8016, 8);
        assert!(
            j.script.contains("__p=8016; __p<=8023"),
            "window base and last computed correctly"
        );
        assert!(
            j.script.contains("OBLETH_SERVING_PORT:-8016"),
            "fallback to window base"
        );
    }

    #[test]
    fn job_status_message_formats_state_and_reason() {
        assert_eq!(
            job_status_message("PENDING", Some("Resources")),
            "PENDING — Resources"
        );
        assert_eq!(job_status_message("RUNNING", None), "RUNNING");
        assert_eq!(job_status_message("RUNNING", Some("None")), "RUNNING");
        assert_eq!(job_status_message("PENDING", Some("  ")), "PENDING");
    }

    #[test]
    fn parse_job_tolerates_int_id_and_array_state() {
        let body = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": 123, "job_state": ["PENDING"], "nodes": "", "state_reason": "Resources" },
        ] }))
        .unwrap();
        let j = parse_job(&body).unwrap().expect("job");
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
        ] }))
        .unwrap();
        let j = parse_job(&body).unwrap().expect("job");
        assert_eq!(j.job_id, "456");
        assert_eq!(j.state, JobState::Pending);
    }

    #[test]
    fn parse_job_returns_none_only_for_empty_jobs() {
        // 200-with-empty-jobs is how some versions report an unknown id.
        let empty = serde_json::to_vec(&serde_json::json!({ "jobs": [] })).unwrap();
        assert!(parse_job(&empty).unwrap().is_none());
        // A record we can't identify is a malformed answer, not "gone".
        let no_id =
            serde_json::to_vec(&serde_json::json!({ "jobs": [ { "job_state": "RUNNING" } ] }))
                .unwrap();
        assert!(parse_job(&no_id).is_err());
    }

    #[test]
    fn parse_job_errors_on_unparseable_2xx_body() {
        assert!(parse_job(b"<html>proxy error</html>").is_err());
        // No `jobs` key at all (e.g. an error envelope) is not "gone".
        assert!(parse_job(br#"{"error":"x"}"#).is_err());
        // An empty jobs array carrying a non-not-found error is not "gone" either.
        let auth = serde_json::to_vec(&serde_json::json!({
            "jobs": [],
            "errors": [ { "error_number": 1007, "error": "Protocol authentication error" } ],
        }))
        .unwrap();
        assert!(parse_job(&auth).is_err());
    }

    #[test]
    fn job_lookup_404_is_gone_only_for_job_not_found_body() {
        let not_found = serde_json::to_vec(&serde_json::json!({
            "jobs": [],
            "errors": [ {
                "description": "Unable to query JobId=123",
                "error_number": 2017,
                "error": "Invalid job id specified",
                "source": "_handle_job_get",
            } ],
        }))
        .unwrap();
        assert!(job_lookup_from_response(404, &not_found, "1", "v0.0.43")
            .unwrap()
            .is_none());
        // Matched by message too, for versions that omit error_number.
        let by_text = br#"{"errors":[{"error":"Invalid job id specified"}]}"#;
        assert!(job_lookup_from_response(404, by_text, "1", "v0.0.43")
            .unwrap()
            .is_none());
        // A route/version-mismatch 404 must hold the tick, not read as "gone".
        for body in [&b""[..], b"Not Found", br#"{"errors":[]}"#] {
            assert!(
                job_lookup_from_response(404, body, "1", "v0.0.43").is_err(),
                "404 body {:?} must be Err",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn job_lookup_non_404_errors_and_2xx_parses() {
        // Job-not-found is read from the body whatever the status.
        assert!(job_lookup_from_response(
            500,
            br#"{"errors":[{"error_number":2017}]}"#,
            "1",
            "v0.0.43"
        )
        .unwrap()
        .is_none());
        let e =
            job_lookup_from_response(500, b"<html>proxy error</html>", "1", "v0.0.43").unwrap_err();
        let e = slurm_api_error(&e).expect("typed");
        assert_eq!(e.kind, SlurmErrorKind::Unavailable);
        assert!(e.message.contains("proxy error"), "{}", e.message);
        let ok = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": 7, "job_state": "RUNNING", "nodes": "node001" },
        ] }))
        .unwrap();
        let j = job_lookup_from_response(200, &ok, "1", "v0.0.43")
            .unwrap()
            .expect("job");
        assert_eq!(j.job_id, "7");
        assert!(job_lookup_from_response(200, b"not json", "1", "v0.0.43").is_err());
    }

    #[test]
    fn parse_job_collapses_none_reason_at_parse_time() {
        // Literal "None" and whitespace-only reasons collapse to None
        let none_literal = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": 1, "job_state": "PENDING", "state_reason": "None" },
        ] }))
        .unwrap();
        let j = parse_job(&none_literal).unwrap().expect("job");
        assert_eq!(
            j.reason, None,
            "literal 'None' reason should collapse to None"
        );

        let none_mixed_case = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": 2, "job_state": "PENDING", "state_reason": "nOnE" },
        ] }))
        .unwrap();
        let j = parse_job(&none_mixed_case).unwrap().expect("job");
        assert_eq!(j.reason, None, "case-insensitive 'None' should collapse");

        let whitespace_only = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": 3, "job_state": "PENDING", "state_reason": "   " },
        ] }))
        .unwrap();
        let j = parse_job(&whitespace_only).unwrap().expect("job");
        assert_eq!(j.reason, None, "whitespace-only reason should collapse");

        let valid_reason = serde_json::to_vec(&serde_json::json!({ "jobs": [
            { "job_id": 4, "job_state": "PENDING", "state_reason": "Resources" },
        ] }))
        .unwrap();
        let j = parse_job(&valid_reason).unwrap().expect("job");
        assert_eq!(
            j.reason,
            Some("Resources".to_string()),
            "valid reasons preserved"
        );
    }

    #[test]
    fn job_state_mapping_is_version_tolerant() {
        assert_eq!(map_job_state("RUNNING"), JobState::Running);
        assert_eq!(map_job_state("Pending"), JobState::Pending);
        assert_eq!(map_job_state("PREEMPTED"), JobState::Gone);
        assert_eq!(map_job_state("anything-else"), JobState::Unknown);
        let f =
            |v: &[&str]| map_job_state_flags(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(f(&["PENDING", "REQUEUED"]), JobState::Pending);
        assert_eq!(f(&["REQUEUE_HOLD"]), JobState::Pending);
        assert_eq!(f(&["RUNNING", "SIGNALING"]), JobState::Running);
        assert_eq!(f(&["STOPPED"]), JobState::Running);
        assert_eq!(f(&["CANCELLED", "COMPLETING"]), JobState::Gone);
        assert_eq!(f(&["TIMEOUT"]), JobState::Gone);
        assert_eq!(f(&[]), JobState::Unknown);
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
        assert!(n[0].state.is_empty());
        assert_eq!(n[0].alloc_cpus, None);
    }

    #[test]
    fn parse_nodes_reads_state_and_allocation() {
        let v = serde_json::json!({"nodes":[
            {"name":"gpu02","partitions":["arm"],"gres":"gpu:gh200:1","cpus":72,
             "real_memory":577536,"state":["MIXED","DRAIN"],"alloc_cpus":36,
             "alloc_memory":{"set":true,"number":262144}},
            {"name":"gpu03","state":"idle"}
        ]});
        let n = parse_nodes(&v);
        assert_eq!(n[0].state, vec!["MIXED", "DRAIN"]);
        assert_eq!(n[0].alloc_cpus, Some(36));
        assert_eq!(n[0].alloc_memory_mb, Some(262144));
        assert_eq!(n[1].state, vec!["IDLE"]);
    }

    #[test]
    fn parse_association_list_keeps_partitions() {
        let v = serde_json::json!({"associations":[
            {"account":"grp_a","partition":"gh200","qos":["normal"]},
            {"account":"grp_a","partition":"gh200","qos":["normal"]},
            {"account":"grp_b","partition":"","qos":"public"}
        ]});
        assert_eq!(
            parse_association_list(&v),
            vec![
                AssociationInfo {
                    account: "grp_a".into(),
                    partition: Some("gh200".into()),
                    qos: vec!["normal".into()]
                },
                AssociationInfo {
                    account: "grp_b".into(),
                    partition: None,
                    qos: vec!["public".into()]
                },
            ]
        );
    }

    #[test]
    fn parse_partitions_reads_allowed_and_denied_accounts() {
        let v = serde_json::json!({"partitions":[
            {"name":"gh200","accounts":{"allowed":"grp_a,grp_b","deny":""}},
            {"name":"general","accounts":{"allowed":"ALL","deny":"grp_x"}}
        ]});
        let p = parse_partitions(&v);
        assert_eq!(p[0].allowed_accounts, vec!["grp_a", "grp_b"]);
        assert!(p[1].allowed_accounts.is_empty());
        assert_eq!(p[1].denied_accounts, vec!["grp_x"]);
    }

    #[test]
    fn parse_associations_dedupes_accounts_and_qos() {
        let v = serde_json::json!({"associations":[
            {"account":"ml-team","qos":["normal","high"]},
            {"account":"ml-team","qos":["normal"]}
        ]});
        let (accounts, qos) = parse_associations(&v);
        assert_eq!(accounts, vec!["ml-team"]);
        assert_eq!(qos, vec!["high", "normal"]); // sorted+deduped
    }

    #[test]
    fn parsers_tolerate_missing_keys() {
        let empty = serde_json::json!({});
        assert!(parse_partitions(&empty).is_empty());
        assert!(parse_nodes(&empty).is_empty());
        assert_eq!(parse_associations(&empty), (vec![], vec![]));
    }

    #[test]
    fn hostlists_expand_ranges_padding_lists_and_suffixes() {
        assert_eq!(expand_nodelist(""), Vec::<String>::new());
        assert_eq!(
            expand_nodelist("gpu[01-04,07]"),
            ["gpu01", "gpu02", "gpu03", "gpu04", "gpu07"]
        );
        assert_eq!(expand_nodelist("a-[1-2]b"), ["a-1b", "a-2b"]);
        assert_eq!(expand_nodelist("a[1-2],b3"), ["a1", "a2", "b3"]);
        assert_eq!(
            expand_nodelist("n[1-2]-ib[3-4]"),
            ["n1-ib3", "n1-ib4", "n2-ib3", "n2-ib4"]
        );
        assert_eq!(expand_nodelist("x[9-11]"), ["x9", "x10", "x11"]);
        assert_eq!(expand_nodelist("bad[a-b]"), ["bad[a-b]"]);
        assert_eq!(expand_nodelist("open[1-2"), ["open[1-2"]);
    }

    #[test]
    fn job_records_carry_batch_host_comment_port_and_times() {
        let body = serde_json::to_vec(&serde_json::json!({ "jobs": [{
            "job_id": 42,
            "name": "obleth-m",
            "user_name": "svc",
            "job_state": ["RUNNING"],
            "nodes": "gpu[01-02]",
            "batch_host": "gpu01",
            "comment": "obleth-port=8003",
            "restart_cnt": 1,
            "start_time": { "set": true, "infinite": false, "number": 1_700_000_000 },
            "end_time": { "set": true, "infinite": false, "number": 1_700_003_600 },
            "time_limit": { "set": true, "infinite": false, "number": 60 },
            "state_reason": "None",
        }]}))
        .unwrap();
        let j = parse_job(&body).unwrap().unwrap();
        assert_eq!(j.state, JobState::Running);
        assert_eq!(j.nodes, ["gpu01", "gpu02"]);
        assert_eq!(j.probe_host(), Some("gpu01"));
        assert_eq!(j.reported_port, Some(8003));
        assert_eq!(j.restart_count, 1);
        assert_eq!(j.start_time.unwrap().timestamp(), 1_700_000_000);
        assert_eq!(j.end_time.unwrap().timestamp(), 1_700_003_600);
        assert_eq!(j.time_limit_mins, Some(60));
        assert!(!j.time_limit_unlimited);
        assert_eq!(j.name.as_deref(), Some("obleth-m"));
        assert_eq!(j.user.as_deref(), Some("svc"));

        let body = serde_json::to_vec(&serde_json::json!({ "jobs": [{
            "job_id": 43,
            "job_state": ["PENDING", "REQUEUED"],
            "nodes": "",
            "restart_cnt": 2,
            "start_time": { "set": true, "infinite": false, "number": 0 },
            "time_limit": { "set": true, "infinite": true, "number": 0 },
            "comment": "",
        }]}))
        .unwrap();
        let j = parse_job(&body).unwrap().unwrap();
        assert_eq!(j.state, JobState::Pending);
        assert!(j.is_requeued());
        assert_eq!(j.raw_state, "PENDING");
        assert_eq!(j.start_time, None);
        assert!(j.time_limit_unlimited);
        assert_eq!(j.time_limit_mins, None);
        assert_eq!(j.reported_port, None);
        assert_eq!(j.probe_host(), None);
    }

    #[test]
    fn ports_are_read_from_the_comment() {
        assert_eq!(port_from_comment("obleth-port=8000"), Some(8000));
        assert_eq!(port_from_comment("x; obleth-port=12001 y"), Some(12001));
        assert_eq!(port_from_comment("obleth-port="), None);
        assert_eq!(port_from_comment("obleth-port=70000"), None);
        assert_eq!(port_from_comment("other"), None);
    }

    #[test]
    fn errors_are_plain_sentences_by_status() {
        let e = error_from_response(401, b"Authentication failure", "reading job 1", "v0.0.43");
        assert_eq!(e.kind, SlurmErrorKind::TokenRejected);
        assert!(
            e.message.contains("rejected the Slurm token"),
            "{}",
            e.message
        );
        let e = error_from_response(511, b"", "reading job 1", "v0.0.43");
        assert_eq!(e.kind, SlurmErrorKind::TokenRejected);
        let e = error_from_response(404, b"Not Found", "submitting the job", "v0.0.40");
        assert_eq!(e.kind, SlurmErrorKind::NotServed);
        assert!(e.message.contains("v0.0.40"), "{}", e.message);
        let e = error_from_response(502, b"<html>Bad Gateway</html>", "reading nodes", "v0.0.43");
        assert_eq!(e.kind, SlurmErrorKind::Unavailable);
        assert!(!e.message.contains("decoding"), "{}", e.message);
    }

    #[test]
    fn submit_rejections_name_slurms_reason_not_the_raw_body() {
        let body = serde_json::to_vec(&serde_json::json!({
            "errors": [{
                "description": "Batch job submission failed",
                "error": "Requested node configuration is not available",
                "error_number": 2014,
                "source": "slurm_submit_batch_job()",
            }],
            "meta": { "plugin": { "type": "openapi/slurmctld" } },
        }))
        .unwrap();
        let e = submit_error_from_response(422, &body, "v0.0.43");
        assert!(e.is_rejection());
        assert_eq!(
            e.message,
            "Slurm rejected the job: Requested node configuration is not available."
        );
        // Older versions answer a refusal with 500; the Slurm error number says
        // it is still a refusal.
        let e = submit_error_from_response(500, &body, "v0.0.40");
        assert!(e.is_rejection());
        let body = serde_json::to_vec(&serde_json::json!({
            "errors": [{
                "description": "You must name an account with --account",
                "error": "Invalid account or account/partition combination specified",
                "error_number": 2045,
            }],
        }))
        .unwrap();
        let e = submit_error_from_response(422, &body, "v0.0.43");
        assert_eq!(
            e.message,
            "Slurm rejected the job: Invalid account or account/partition combination \
             specified. (job_submit said: You must name an account with --account.)"
        );
        let e = submit_error_from_response(503, b"upstream down", "v0.0.43");
        assert!(!e.is_rejection());
    }

    #[test]
    fn accepted_submits_keep_warnings_as_notes() {
        let body = br#"{"job_id": 77, "step_id": "batch", "job_submit_user_msg": "Routed to gpu",
            "warnings": [{"description": "Expected OpenAPI type=array", "source": "x"}], "errors": []}"#;
        let s = submitted_from_response(body).unwrap();
        assert_eq!(s.job_id, "77");
        assert_eq!(
            s.notes,
            [
                "Slurm warned: Expected OpenAPI type=array.",
                "job_submit said: Routed to gpu."
            ]
        );
        let s = submitted_from_response(br#"{"result": {"job_id": "78"}}"#).unwrap();
        assert_eq!(s.job_id, "78");
        let e = submitted_from_response(b"cut off").unwrap_err();
        assert_eq!(e.kind, SlurmErrorKind::NoAnswer);
    }

    #[test]
    fn ping_reads_controller_state_across_versions() {
        let v44 = br#"{"pings":[{"hostname":"ctl","pinged":"UP","responding":true}],
            "meta":{"slurm":{"release":"25.05.1"}}}"#;
        assert_eq!(
            parse_ping(v44),
            PingInfo {
                slurmctld: Some("UP".into()),
                slurm_release: Some("25.05.1".into())
            }
        );
        let v45 = br#"{"pings":[{"hostname":"ctl","responding":false}],
            "meta":{"slurm":{"version":{"major":"26","minor":"05","micro":"5"}}}}"#;
        let p = parse_ping(v45);
        assert_eq!(p.slurmctld.as_deref(), Some("DOWN"));
        assert_eq!(p.slurm_release.as_deref(), Some("26.05.5"));
        assert_eq!(parse_ping(b"nope"), PingInfo::default());
    }

    #[test]
    fn submit_body_follows_the_job_desc_schema() {
        let mut s = spec();
        s.partition = String::new();
        s.gres = "gpu:a100:1, shard:2".into();
        s.time_limit = Some("UNLIMITED".into());
        let j = job_submit_from_spec(&s, "org/Model 7B", "obleth-", 8000, 4);
        assert_eq!(j.name, "obleth-org-Model-7B");
        let b = submit_body(&j, "");
        assert!(b.get("script").is_none(), "script goes in job.script");
        let job = &b["job"];
        assert!(job["script"].as_str().unwrap().starts_with("#!"));
        assert!(
            job.get("partition").is_none(),
            "empty partition uses the cluster default"
        );
        assert_eq!(job["requeue"], false);
        assert_eq!(job["tres_per_node"], "gres/gpu:a100:1,gres/shard:2");
        assert_eq!(job["time_limit"]["infinite"], true);
        assert_eq!(job["current_working_directory"], "/tmp");
        assert_eq!(job["standard_output"], "/tmp/obleth-org-Model-7B-%j.out");
        assert!(job.get("standard_error").is_none());
        assert_eq!(job_log_path(&j, "12"), "/tmp/obleth-org-Model-7B-12.out");

        s.log_output_dir = "/shared/logs/".into();
        s.time_limit = Some("1-00:00:00".into());
        let j = job_submit_from_spec(&s, "m", "obleth-", 8000, 4);
        let job = &submit_body(&j, "")["job"];
        assert_eq!(job["current_working_directory"], "/shared/logs");
        assert_eq!(job["standard_output"], "/shared/logs/obleth-m-%j.out");
        assert_eq!(job["standard_error"], "/shared/logs/obleth-m-%j.err");
        assert_eq!(job["time_limit"]["number"], 1440);
        assert_eq!(job_log_path(&j, "9"), "/shared/logs/obleth-m-9.out");
    }

    #[test]
    fn port_block_reports_the_port_in_the_job_comment() {
        let j = job_submit_from_spec(&spec(), "m", "obleth-", 8000, 4);
        assert!(j.script.contains(
            "scontrol update JobId=\"${SLURM_JOB_ID:-}\" Comment=\"obleth-port=$OBLETH_SERVING_PORT\" >/dev/null 2>&1 || true"
        ));
    }

    #[test]
    fn shebang_after_blank_lines_or_a_bom_is_kept() {
        let mut s = spec();
        s.script_body = "\u{feff}\n\n#!/bin/zsh\necho hi\n".into();
        let j = job_submit_from_spec(&s, "m", "obleth-", 8000, 1);
        assert!(j.script.starts_with("#!/bin/zsh\n"), "{}", j.script);
        assert!(!j.script.contains("#!/bin/bash"));
    }

    #[test]
    fn nodes_keep_an_address_that_differs_from_the_name() {
        let v = serde_json::json!({ "nodes": [
            { "name": "gpu01", "address": "10.0.0.1", "cpus": 8 },
            { "name": "gpu02", "address": "gpu02" },
        ]});
        let n = parse_nodes(&v);
        assert_eq!(n[0].address.as_deref(), Some("10.0.0.1"));
        assert_eq!(n[0].cpus, Some(8));
        assert_eq!(n[1].address, None);
    }

    #[test]
    fn associations_are_limited_to_the_slurm_user() {
        let v = serde_json::json!({ "associations": [
            { "account": "mine", "user": "svc", "qos": ["normal"] },
            { "account": "theirs", "user": "bob", "qos": ["high"] },
            { "account": "no-user" },
        ]});
        let (accounts, qos) = parse_associations(&filter_associations(v, "svc"));
        assert_eq!(accounts, ["mine", "no-user"]);
        assert_eq!(qos, ["normal"]);
    }

    #[test]
    fn accounting_jobs_are_matched_by_name_user_and_live_state() {
        let v = serde_json::json!({ "jobs": [
            { "job_id": 1, "name": "obleth-m", "user": "svc", "state": { "current": ["RUNNING"] }, "nodes": "n1" },
            { "job_id": 2, "name": "obleth-m", "user": "svc", "state": { "current": ["COMPLETED"] } },
            { "job_id": 3, "name": "obleth-m", "user": "bob", "state": { "current": ["PENDING"] } },
            { "job_id": 4, "name": "other", "user": "svc", "state": { "current": ["PENDING"] } },
        ]});
        let found = parse_db_jobs(&v, "svc", "obleth-m");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].job_id, "1");
        assert_eq!(found[0].nodes, ["n1"]);
    }

    #[test]
    fn accounting_records_carry_end_state_reason_and_times() {
        let j = serde_json::json!({
            "job_id": 7, "name": "obleth-m", "user": "svc",
            "state": { "current": ["TIMEOUT"], "reason": "None" },
            "time": { "submission": 1_700_000_000, "start": 1_700_000_060, "end": 1_700_003_660 },
        });
        let info = db_job_info(&j).unwrap();
        assert_eq!(info.state, JobState::Gone);
        assert_eq!(info.raw_state, "TIMEOUT");
        assert_eq!(info.reason, None);
        assert_eq!(info.submit_time.map(|t| t.timestamp()), Some(1_700_000_000));
        assert_eq!(info.start_time.map(|t| t.timestamp()), Some(1_700_000_060));
    }

    #[test]
    fn jobs_under_another_user_or_name_are_not_ours() {
        let c = Slurmrestd::new(reqwest::Client::new(), "http://x", "v0.0.43", "svc", "t")
            .with_job_name_prefix("obleth-");
        let mut j = JobInfo {
            name: Some("obleth-m".into()),
            user: Some("svc".into()),
            ..Default::default()
        };
        assert!(c.is_ours(&j));
        j.user = Some("bob".into());
        assert!(!c.is_ours(&j));
        j.user = None;
        j.name = Some("someone-else".into());
        assert!(!c.is_ours(&j));
        j.name = None;
        assert!(c.is_ours(&j));
    }

    #[test]
    fn token_expiry_is_warned_about_three_days_ahead() {
        let enc =
            |p: &str| base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, p);
        let tok = |exp: i64| format!("{}.{}.s", enc("{}"), enc(&format!(r#"{{"exp":{exp}}}"#)));
        let now = 1_700_000_000;
        assert_eq!(token_expiry_warning(&tok(now + 10 * 86_400), now), None);
        let w = token_expiry_warning(&tok(now + 2 * 86_400), now).unwrap();
        assert!(w.contains("expires in 2 days"), "{w}");
        let w = token_expiry_warning(&tok(now + 1800), now).unwrap();
        assert!(w.contains("expires in 30 minutes"), "{w}");
        let w = token_expiry_warning(&tok(now - 60), now).unwrap();
        assert!(w.contains("expired at 2023-11-14 22:12 UTC"), "{w}");
        assert_eq!(token_expiry_warning("opaque", now), None);
        assert_eq!(plain_duration(3600), "60 minutes");
        assert_eq!(plain_duration(86_400), "24 hours");
    }
}
