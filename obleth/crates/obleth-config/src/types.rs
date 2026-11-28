//! Shared domain types used across the data plane, store, cache and admin API.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::reasoning::{ModelFamily, ModelReasoning};

/// How global concurrency is divided among tenants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum FairshareAlgorithm {
    /// Per-tenant weighted fair queuing (`served / tenant_weight`).
    #[default]
    Weighted,
    /// Group-weighted capacity pools; weight-proportional split among tenants in each group.
    Hierarchical,
}

impl FairshareAlgorithm {
    pub fn parse(s: &str) -> Self {
        match s.trim().to_lowercase().as_str() {
            "hierarchical" | "group" | "groups" => Self::Hierarchical,
            _ => Self::Weighted,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Weighted => "weighted",
            Self::Hierarchical => "hierarchical",
        }
    }
}

/// Who may follow up a generation job (a video or a mesh: poll, download,
/// delete, list): `OBLETH_VIDEO_JOB_SCOPE`, named for the first job kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JobScope {
    /// Only the API key that created the job. With secret keys and with the
    /// per-(issuer, subject) identity keys of JWT callers alike, a key is one
    /// user, so a job is private to the user who made it.
    #[default]
    Key,
    /// Any key of the tenant that created the job.
    Tenant,
}

impl JobScope {
    /// The scope an `OBLETH_VIDEO_JOB_SCOPE` value names. Unset or blank is
    /// the default, [`JobScope::Key`]; any other value that is not `key`
    /// or `tenant` (case-insensitive) is `Err` with the value, so the caller
    /// can warn and fall back to the default rather than widen access on a
    /// typo.
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        match raw.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("key") => Ok(Self::Key),
            Some("tenant") => Ok(Self::Tenant),
            Some(_) => Err(raw.unwrap_or_default().to_string()),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Key => "key",
            Self::Tenant => "tenant",
        }
    }
}

/// Fairshare group — capacity is partitioned by group weight under the hierarchical algorithm.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FairshareGroup {
    pub name: String,
    pub weight: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// A tenant is the unit of fairshare. Every API key belongs to exactly one tenant.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct Tenant {
    pub id: Uuid,
    pub name: String,
    /// Fairshare group for hierarchical admission. Weights between groups; equal split within.
    pub fairshare_group: String,
    /// Fairshare weight (used by the weighted algorithm; token-budget priority hint). Must be >= 1.
    pub weight: i64,
    /// Sustained token budget refilled per minute (token-bucket rate).
    pub tokens_per_minute: i64,
    /// Optional per-tenant in-flight cap. `None` means only the global limit applies.
    pub max_in_flight: Option<i64>,
    /// Free-text operator note shown in dashboards.
    #[serde(default)]
    pub description: String,
    /// Optional grouping label (team, project, org unit, customer).
    #[serde(default)]
    pub organization: String,
    /// Contact for budget/expiry notifications.
    #[serde(default)]
    pub contact_email: String,
    /// Lifecycle: `active`, `suspended`, or `archived`. Only `active` admits traffic.
    #[serde(default = "default_tenant_status")]
    pub status: String,
    /// IANA timezone (e.g. `America/Phoenix`) the schedule below is evaluated in.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Optional activation start. Before this instant the tenant is not active.
    #[serde(default)]
    pub active_from: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional expiry cutoff. After this instant the tenant is not active.
    #[serde(default)]
    pub active_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional recurring weekly windows. Empty/`None` means any time of week.
    #[serde(default)]
    pub weekly_windows: Option<Vec<WeeklyWindow>>,
    /// Optional cumulative token budget for the current term. `None` = no cap.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Optional cumulative USD cost budget for the current term. `None` = no cap.
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    /// Term budget reset period: `lifetime`, `monthly`, or `term`. `None` = lifetime.
    #[serde(default)]
    pub budget_period: Option<String>,
    /// When the current term began. Changing it resets cumulative term usage.
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional per-tenant model allowlist. Empty/`None` = all models permitted.
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    /// Whether flight-recorder tracing is enabled for every key in this tenant
    /// (tenant-level OR key-level enables a request's spans).
    #[serde(default)]
    pub tracing_enabled: bool,
    /// Per-tenant guardrails policy. `None` means no guardrails are applied.
    #[serde(default)]
    pub guardrails_policy: Option<GuardrailsPolicy>,
    /// Per-tenant compression policy. `None` means follow the global default.
    #[serde(default)]
    pub compression_policy: Option<CompressionPolicy>,
    /// Synthetic tenants generate benchmark/test traffic. The proxy tags their
    /// requests `request_type = "benchmark"`, which default usage/cost reads
    /// exclude.
    #[serde(default)]
    pub synthetic: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// `request_type` label stamped on usage rows from synthetic tenants
/// (benchmark/test traffic). Mirrors the `health_probe` convention: default
/// usage reads exclude it; `include_internal=true` opts back in.
pub const BENCHMARK_REQUEST_TYPE: &str = "benchmark";
/// `request_type` of a request the batch runner sent for a batch item.
pub const BATCH_REQUEST_TYPE: &str = "batch";

pub fn default_tenant_status() -> String {
    "active".to_string()
}

/// Default tenant timezone used when deserializing legacy records.
pub fn default_timezone() -> String {
    "UTC".to_string()
}

/// A recurring weekly availability window. Times are minutes from local midnight
/// in the tenant's `timezone`; `day` is 0=Sunday .. 6=Saturday (matches
/// JavaScript `Date.getDay()`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ToSchema)]
pub struct WeeklyWindow {
    pub day: u8,
    pub start_min: u16,
    pub end_min: u16,
}

/// `api_keys.kind`: a minted secret, or an identity resolved from a verified JWT.
pub const API_KEY_KIND_SECRET: &str = "secret";
pub const API_KEY_KIND_IDENTITY: &str = "identity";

fn default_key_kind() -> String {
    API_KEY_KIND_SECRET.to_string()
}

/// Default fairshare weight for a key when unset (legacy records, defaults).
pub fn default_key_weight() -> i64 {
    100
}

/// An API key. The raw secret is never stored; only its hash + a display prefix.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKey {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    /// Operator note shown in dashboards; useful for ownership and intent.
    #[serde(default)]
    pub description: String,
    /// Display-only prefix, e.g. `obl_a1b2...` for dashboards.
    pub key_prefix: String,
    /// `secret` (minted, hashed secret) or `identity` (stands for a verified
    /// external identity; no secret exists).
    #[serde(default = "default_key_kind")]
    pub kind: String,
    /// Issuer URL of the identity this key stands for (identity keys only).
    #[serde(default)]
    pub identity_issuer: Option<String>,
    /// Subject claim of the identity this key stands for (identity keys only).
    /// Contract field: external account systems join it to their own user
    /// records. Do not rename.
    #[serde(default)]
    pub identity_subject: Option<String>,
    /// Extra claims captured at provision time (`identity_claims` in the
    /// issuer config), e.g. an immutable user id.
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub identity_claims: Option<serde_json::Value>,
    /// Optional cumulative token budget for this key's current term. `None` = no cap.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Optional cumulative USD cost budget for this key's current term. `None` = no cap.
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    /// Key budget reset period: `lifetime`, `monthly`, or `term`. `None` = lifetime.
    #[serde(default)]
    pub budget_period: Option<String>,
    /// When this key's current budget term began.
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Fairshare weight among this tenant's keys. Default 100.
    #[serde(default = "default_key_weight")]
    pub weight: i64,
    /// Per-model in-flight ceiling for this key. `None` = no cap.
    #[serde(default)]
    pub max_in_flight: Option<i64>,
    pub disabled: bool,
    pub tracing_enabled: bool,
    /// The key fronts many end users (a chatbot or other shared front end)
    /// and names each one per request (`x-obleth-end-user`, else the body's
    /// `user`). Each named end user then takes their own place in the fair
    /// queue, as if they held their own key: the key's weight and in-flight
    /// cap apply to each of them, while budgets stay on the key. Trusted:
    /// only set it for a caller that names its users honestly, since a caller
    /// could otherwise split itself into many users to jump the queue.
    #[serde(default)]
    pub end_user_fairshare: bool,
    /// Background priority: this key's requests run in each model's
    /// background fairshare pool, the one batch items use. Nothing new starts
    /// for the key while an interactive request for that model waits, and it
    /// never holds more than the background share of a model's slots. Its
    /// requests wait longer for a slot instead of failing when a model is
    /// busy. Operator-only, so off by default.
    #[serde(default)]
    pub background: bool,
    /// Networks the key may be used from, as canonical CIDRs (a single
    /// address is a /32 or /128). When this and `allowed_countries` are both
    /// empty the key works from any network; otherwise a request from
    /// outside both is refused with 403 `ip_not_allowed`.
    #[serde(default)]
    pub allowed_networks: Vec<String>,
    /// Countries the key may be used from, as upper-case ISO 3166-1 alpha-2
    /// codes. A request whose client country (set by the edge, and only for
    /// public client addresses) is listed is allowed even from outside
    /// `allowed_networks`. An unknown country never matches.
    #[serde(default)]
    pub allowed_countries: Vec<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Input to `Store::provision_identity_key` (obleth-store).
#[derive(Debug, Clone)]
pub struct IdentityProvision<'a> {
    pub issuer: &'a str,
    pub subject: &'a str,
    pub tenant_name: &'a str,
    pub claims: serde_json::Value,
}

/// Result of provisioning: the hot-path handle and view, plus whether the
/// call created anything (so the caller can alert on tenant creation).
#[derive(Debug, Clone)]
pub struct ProvisionedIdentity {
    pub hash: String,
    pub resolved: ResolvedKey,
    pub tenant_created: bool,
    pub key_created: bool,
}

/// The compact, hot-path view resolved from an API key. This is what the data
/// plane caches in moka and reads from Redis; it carries everything the
/// fairshare admission step needs without a relational lookup.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResolvedKey {
    pub key_id: Uuid,
    /// The key's name, carried so each usage row can record it. Empty on an
    /// entry cached before the field existed, until the cache is rewarmed.
    #[serde(default)]
    pub key_name: String,
    pub tenant_id: Uuid,
    pub tenant_name: String,
    pub fairshare_group: String,
    pub group_weight: i64,
    pub weight: i64,
    pub tokens_per_minute: i64,
    pub max_in_flight: Option<i64>,
    pub disabled: bool,
    /// Tenant lifecycle status. Anything other than `active` blocks the request.
    #[serde(default = "default_tenant_status")]
    pub status: String,
    /// IANA timezone the scheduling fields below are evaluated in.
    #[serde(default = "default_timezone")]
    pub timezone: String,
    /// Optional activation start; requests before this instant are blocked.
    #[serde(default)]
    pub active_from: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional expiry cutoff; requests after this instant are blocked.
    #[serde(default)]
    pub active_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional recurring weekly windows. Empty/`None` means any time of week.
    #[serde(default)]
    pub weekly_windows: Option<Vec<WeeklyWindow>>,
    /// Optional cumulative token budget for the current term. `None` = no cap.
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    /// Optional cumulative USD cost budget for the current term. `None` = no cap.
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    /// Term budget reset period: `lifetime`, `monthly`, or `term`. `None` = lifetime.
    #[serde(default)]
    pub budget_period: Option<String>,
    /// When the current term began. Changing it resets cumulative term usage.
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Optional cumulative token budget for the key itself. `None` = no cap.
    #[serde(default)]
    pub key_budget_tokens: Option<i64>,
    /// Optional cumulative USD budget for the key itself. `None` = no cap.
    #[serde(default)]
    pub key_budget_cost_usd: Option<f64>,
    /// Key budget reset period: `lifetime`, `monthly`, or `term`. `None` = lifetime.
    #[serde(default)]
    pub key_budget_period: Option<String>,
    /// When the key's current budget term began.
    #[serde(default)]
    pub key_budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Fairshare weight among the tenant's keys (see `ApiKey::weight`).
    #[serde(default = "default_key_weight")]
    pub key_weight: i64,
    /// Per-model in-flight ceiling for the key. `None` = no cap.
    #[serde(default)]
    pub key_max_in_flight: Option<i64>,
    /// Each named end user of this key queues separately (see
    /// `ApiKey::end_user_fairshare`).
    #[serde(default)]
    pub end_user_fairshare: bool,
    /// The key's requests run in each model's background pool (see
    /// `ApiKey::background`). Defaults to off so entries cached before the
    /// field existed still parse.
    #[serde(default)]
    pub background: bool,
    /// Networks the key may be used from (see `ApiKey::allowed_networks`);
    /// empty means any. Cached entries live under a key namespace that only
    /// builds knowing this field write, so an entry written without it is
    /// never read as "any network".
    #[serde(default)]
    pub allowed_networks: Vec<String>,
    /// Countries the key may be used from (see `ApiKey::allowed_countries`).
    /// Cached under the same versioned namespace as `allowed_networks`.
    #[serde(default)]
    pub allowed_countries: Vec<String>,
    /// Optional per-tenant model allowlist. Empty/`None` = all models permitted.
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    /// Internal keys are allowed through the proxy path but are omitted from
    /// the normal usage ledger. This is used for gateway-owned health probes.
    #[serde(default)]
    pub internal: bool,
    /// Whether flight-recorder tracing is enabled for this key (key-level OR
    /// tenant-level flag). When true the proxy captures full request/response
    /// payloads for replay and debugging.
    #[serde(default)]
    pub tracing_enabled: bool,
    /// Per-tenant content policy, if configured. `None` means no guardrails.
    #[serde(default)]
    pub guardrails_policy: Option<GuardrailsPolicy>,
    /// Per-tenant compression policy, if configured. `None` means follow the
    /// global default for eligible models.
    #[serde(default)]
    pub compression_policy: Option<CompressionPolicy>,
    /// Mirrors `Tenant::synthetic`: requests from this key are tagged as
    /// benchmark traffic in the usage ledger.
    #[serde(default)]
    pub synthetic: bool,
}

/// Outcome of admission for a single request, recorded in telemetry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Admission {
    /// Admitted immediately (capacity available).
    Fast,
    /// Admitted after waiting in the weighted queue.
    Queued,
    /// Rejected (budget exhausted or fail-closed).
    Rejected,
}

impl Admission {
    pub fn as_str(self) -> &'static str {
        match self {
            Admission::Fast => "fast",
            Admission::Queued => "queued",
            Admission::Rejected => "rejected",
        }
    }
}

mod backup;
mod model;
mod settings;
mod usage;

pub use backup::*;
pub use model::*;
pub use settings::*;
pub use usage::*;

/// Optional Slurm provisioning spec for a model (1:1 with a model). Absent for
/// unmanaged models. Stored in `managed_models`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ManagedModelSpec {
    #[schema(value_type = String)]
    pub model_id: Uuid,
    pub enabled: bool,
    pub partition: String,
    pub gres: String,
    pub nodes: i64,
    pub constraints: Option<String>,
    pub exclude: Option<String>,
    pub account: Option<String>,
    pub qos: Option<String>,
    pub time_limit: Option<String>,
    /// Slurm `--cpus-per-task`. `None` leaves it to the cluster default.
    #[serde(default)]
    pub cpus_per_task: Option<i64>,
    /// Slurm `--mem` (e.g. `560G`). `None` leaves it to the cluster default.
    #[serde(default)]
    pub mem: Option<String>,
    pub image: String,
    /// Shell lines injected before the launch command in the batch script.
    /// Use this to load modules or set PATH on your cluster, e.g. `module load apptainer`.
    pub preamble: String,
    /// Directory for Slurm stdout/stderr files. When non-empty, jobs write
    /// `{dir}/{job_name}-%j.out` and `{dir}/{job_name}-%j.err`. Empty = Slurm default.
    pub log_output_dir: String,
    pub launch_command: String,
    /// Fully-rendered job script (recipe output). When non-empty it is submitted
    /// verbatim and takes precedence over the `image`/`preamble`/`launch_command`
    /// assembly. Empty preserves the legacy path.
    #[serde(default)]
    pub script_body: String,
    pub serving_port: i64,
    pub health_path: String,
    /// How many replicas the provisioner keeps running. 0 keeps none running
    /// (the spec stays, so raising it later starts them again).
    pub target_replicas: i64,
    /// Health floor: the model counts as healthy (rather than degraded) once
    /// this many replicas are healthy. It does not hold traffic back: each
    /// replica takes requests as soon as it is healthy. 0..=target_replicas.
    #[serde(default = "default_min_replicas")]
    pub min_replicas: i64,
    /// Failed launches in a row after which submits pause (0 = no limit). A
    /// failed launch is a job that ended without ever serving, or that hit
    /// `startup_timeout_mins`. A job that served and then hit its time limit,
    /// was preempted or lost its node is not a failure.
    pub max_job_failures: i64,
    /// Minutes a running job's server may take to become healthy before the
    /// job is cancelled and counted as a failed launch. 0 never gives up.
    #[serde(default = "default_startup_timeout_mins")]
    pub startup_timeout_mins: i64,
    /// When a healthy replica's job is within this many minutes of its time
    /// limit, a replacement is submitted, and the old one is drained once the
    /// replacement serves. 0 turns the handoff off.
    #[serde(default = "default_handoff_mins")]
    pub handoff_mins: i64,
    /// Start each replica with an API key of its own. The provisioner makes
    /// a random key for every job and passes it in the job's environment as
    /// `OBLETH_REPLICA_API_KEY` (and as `VLLM_API_KEY` and `LLAMA_API_KEY`,
    /// which vLLM and llama.cpp read on their own), then registers the
    /// replica's endpoint with that key, so the server answers the gateway
    /// and nothing else that can reach its port. Off by default; replicas
    /// already running keep the key, or the lack of one, they started with.
    #[serde(default)]
    pub replica_api_key: bool,
    /// Serialized launcher panel state (backend id + knob values + envelope) used to
    /// repopulate the dashboard edit view. Metadata only; does not affect provisioning.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub launcher_spec: Option<serde_json::Value>,
    /// Last provisioner submit failure, as a plain sentence (e.g. "Slurm
    /// rejected the job: Invalid account or account/partition combination
    /// specified."). `None` once a submit succeeds.
    #[serde(default)]
    pub last_provision_error: Option<String>,
    #[serde(default)]
    pub last_provision_error_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Why submits are paused, as a plain sentence; `None` when they are not.
    /// Set by the provisioner when `max_job_failures` is reached or Slurm
    /// rejects the job; cleared by `POST .../managed/retry`.
    #[serde(default)]
    pub paused_reason: Option<String>,
    /// No submits before this time. `None` when not paused.
    #[serde(default)]
    pub paused_until: Option<chrono::DateTime<chrono::Utc>>,
    /// Failed launches in a row since a launch last served.
    #[serde(default)]
    pub failed_launches: i64,
    /// Steps taken on the backoff ladder (5, 15, 30, 60 minutes, then hourly);
    /// 0 when not backing off.
    #[serde(default)]
    pub backoff_step: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}
fn default_min_replicas() -> i64 {
    1
}
/// Default for [`ManagedModelSpec::startup_timeout_mins`].
pub const DEFAULT_STARTUP_TIMEOUT_MINS: i64 = 60;
/// Default for [`ManagedModelSpec::handoff_mins`].
pub const DEFAULT_HANDOFF_MINS: i64 = 15;
fn default_startup_timeout_mins() -> i64 {
    DEFAULT_STARTUP_TIMEOUT_MINS
}
fn default_handoff_mins() -> i64 {
    DEFAULT_HANDOFF_MINS
}

impl Default for ManagedModelSpec {
    fn default() -> Self {
        Self {
            model_id: Uuid::nil(),
            enabled: true,
            partition: String::new(),
            gres: String::new(),
            nodes: 1,
            constraints: None,
            exclude: None,
            account: None,
            qos: None,
            time_limit: None,
            cpus_per_task: None,
            mem: None,
            image: String::new(),
            preamble: String::new(),
            log_output_dir: String::new(),
            launch_command: String::new(),
            script_body: String::new(),
            serving_port: 8000,
            health_path: "/health".into(),
            target_replicas: 1,
            min_replicas: 1,
            max_job_failures: 0,
            startup_timeout_mins: DEFAULT_STARTUP_TIMEOUT_MINS,
            handoff_mins: DEFAULT_HANDOFF_MINS,
            replica_api_key: false,
            launcher_spec: None,
            last_provision_error: None,
            last_provision_error_at: None,
            paused_reason: None,
            paused_until: None,
            failed_launches: 0,
            backoff_step: 0,
            created_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
            updated_at: chrono::DateTime::<chrono::Utc>::UNIX_EPOCH,
        }
    }
}

/// A managed model's failure and pause state, as the provisioner sets it
/// (`PUT /api/v1/models/{id}/managed/backoff`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ManagedBackoff {
    /// Failed launches in a row.
    #[serde(default)]
    pub failed_launches: i64,
    /// Steps taken on the backoff ladder; 0 when not backing off.
    #[serde(default)]
    pub backoff_step: i64,
    /// Why submits are paused, as a plain sentence; `None` lifts a pause.
    #[serde(default)]
    pub paused_reason: Option<String>,
    /// No submits before this time.
    #[serde(default)]
    pub paused_until: Option<chrono::DateTime<chrono::Utc>>,
}

/// One known Slurm-backed replica of a managed model. Stored in `model_replicas`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelReplica {
    #[schema(value_type = String)]
    pub id: Uuid,
    #[schema(value_type = String)]
    pub model_id: Uuid,
    pub slurm_job_id: String,
    pub nodes: Option<String>,
    #[schema(value_type = Option<String>)]
    pub endpoint_id: Option<Uuid>,
    /// One of: pending, starting, healthy, draining, lost.
    pub state: String,
    pub last_message: Option<String>,
    #[serde(default)]
    pub port_base: Option<i64>,
    /// Cancel this replica's job now; the target count then brings in a new
    /// one. Set by `DELETE /api/v1/replicas/{id}` on a live replica.
    #[serde(default)]
    pub cancel_requested: bool,
    /// Replace this replica without an outage: the provisioner submits a
    /// successor and drains this one once the successor is healthy (or after
    /// the startup timeout). Set by restart and roll.
    #[serde(default)]
    pub replace_requested: bool,
    /// When Slurm started the job the replica was promoted under.
    #[serde(default)]
    pub job_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the running job will reach its time limit, from Slurm.
    #[serde(default)]
    pub job_ends_at: Option<chrono::DateTime<chrono::Utc>>,
    /// What is happening with the replica, as a plain sentence for the
    /// dashboard (e.g. "Slurm put the job back in the queue ...").
    #[serde(default)]
    pub state_detail: Option<String>,
    /// Where this job's output goes, or `None` when unknown.
    #[serde(default)]
    pub log_path: Option<String>,
    /// The port the server was found on.
    #[serde(default)]
    pub port: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// One past or current launch of a Slurm-backed replica, from the durable
/// launch history (`deployment_launches`). Outlives the replica row, which is
/// garbage-collected. `id` is the replica id.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct DeploymentLaunch {
    #[schema(value_type = String)]
    pub id: Uuid,
    #[schema(value_type = String)]
    pub model_id: Uuid,
    pub model_name: String,
    /// `launcher_spec.recipe_id` at submit time, when launched from a recipe.
    pub recipe_id: Option<String>,
    pub slurm_job_id: String,
    // Managed spec as submitted.
    pub partition: Option<String>,
    pub account: Option<String>,
    pub qos: Option<String>,
    pub time_limit: Option<String>,
    pub gres: Option<String>,
    pub nodes_requested: Option<i64>,
    pub cpus_per_task: Option<i64>,
    pub mem: Option<String>,
    pub exclude: Option<String>,
    pub constraints: Option<String>,
    #[schema(value_type = Option<Object>)]
    pub launcher_spec: Option<serde_json::Value>,
    /// Allocated node hostnames (comma-separated), once the job ran.
    pub nodes: Option<String>,
    pub submitted_at: chrono::DateTime<chrono::Utc>,
    pub started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub healthy_at: Option<chrono::DateTime<chrono::Utc>>,
    pub ended_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Slurm's terminal state (`TIMEOUT`, `OUT_OF_MEMORY`, `NODE_FAIL`,
    /// `FAILED`, ...), `gone` when the job vanished from slurmrestd,
    /// `cancelled:scale-down|restart|probe-failed|startup-timeout|stuck|replaced|removed`
    /// when obleth cancelled it (`cancelled` alone from an older provisioner),
    /// or `deleted` when the replica row was removed while still open.
    pub end_state: Option<String>,
    pub end_reason: Option<String>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    /// Seconds from submit to start (time in the queue).
    pub queued_secs: Option<i64>,
    /// Seconds from start to healthy (weights load and warm-up).
    pub load_secs: Option<i64>,
    /// Seconds from start to end, or to now while still running.
    pub ran_secs: Option<i64>,
}

/// Valid `ModelReplica::state` values, in lifecycle order. The provisioner
/// never writes `starting` (it records a start by reporting nodes); the
/// value stays valid for API clients and older rows.
pub const REPLICA_STATES: [&str; 5] = ["pending", "starting", "healthy", "draining", "lost"];

/// A registered MCP (Model Context Protocol) server. obleth fronts it with the
/// same identity, audit, and reliability layer it applies to LLM traffic:
/// clients reach many MCP servers through one authenticated obleth endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct McpServer {
    pub id: Uuid,
    /// Path segment clients use: `/mcp/{name}`.
    pub name: String,
    /// Upstream MCP base URL (streamable-HTTP / SSE endpoint).
    pub upstream_url: String,
    /// Full Authorization header value forwarded upstream (e.g. `Bearer …`).
    /// Stored encrypted-at-rest in production.
    pub auth_header: Option<String>,
    pub enabled: bool,
    /// The tenants whose keys may call the server through `/mcp/{name}`. Empty
    /// means none: a server is closed to direct calls until tenants are named.
    #[serde(default)]
    pub allowed_tenants: Vec<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// Hot-path view of an MCP server for the data plane cache.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedMcpServer {
    pub name: String,
    pub upstream_url: String,
    pub auth_header: Option<String>,
    pub enabled: bool,
    /// See [`McpServer::allowed_tenants`]. An entry cached before the field
    /// existed reads as empty, so it admits no one until it is published again.
    #[serde(default)]
    pub allowed_tenants: Vec<Uuid>,
}

/// A response stored in the exact-match cache. Bodies are UTF-8 (JSON or SSE
/// text), so they round-trip losslessly as a `String`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CachedResponse {
    pub status: u16,
    pub content_type: String,
    pub body: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The cached hot-path view keeps the list through the JSON Redis holds.
    /// (An entry written by a build without the field can only exist in the
    /// older key namespace, which this build never reads; see obleth-redis.)
    #[test]
    fn resolved_keys_cache_allowed_networks() {
        let raw = serde_json::json!({
            "key_id": uuid::Uuid::nil(), "tenant_id": uuid::Uuid::nil(),
            "tenant_name": "t", "fairshare_group": "default", "group_weight": 100,
            "weight": 100, "tokens_per_minute": 1000, "max_in_flight": null,
            "disabled": false, "allowed_networks": ["192.0.2.0/24"],
            "allowed_countries": ["US"]
        });
        let key: super::ResolvedKey = serde_json::from_value(raw).unwrap();
        assert_eq!(key.allowed_networks, ["192.0.2.0/24"]);
        assert_eq!(key.allowed_countries, ["US"]);
        let again: super::ResolvedKey =
            serde_json::from_str(&serde_json::to_string(&key).unwrap()).unwrap();
        assert_eq!(again, key);
    }

    #[test]
    fn resolved_key_carries_guardrails_policy() {
        let policy = GuardrailsPolicy {
            action: GuardrailsAction::Block,
            input_scanners: vec!["prompt_injection".into()],
            output_scanners: vec![],
            guard_model: None,
            ban_keywords: vec![],
            fail_open: true,
        };
        let json = serde_json::to_string(&policy).unwrap();
        let back: GuardrailsPolicy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy, back);
    }
}
