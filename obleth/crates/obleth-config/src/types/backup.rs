//! The config backup format.

use super::*;

// ---- config backup / restore ------------------------------------------------

/// File-format discriminator for config backups.
pub const BACKUP_FORMAT: &str = "obleth-config-backup";
/// Current backup schema version. Bump when the file shape changes.
pub const BACKUP_VERSION: u32 = 1;

/// A portable snapshot of all gateway configuration: everything needed to
/// recreate an instance except usage history (audit log, health-check history,
/// ClickHouse ledger). Provider secrets are carried exactly as stored — i.e.
/// AES-GCM ciphertext when `OBLETH_ENCRYPTION_KEY` is set — so restoring an
/// encrypted backup requires the same key.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ConfigBackup {
    /// Always [`BACKUP_FORMAT`].
    pub format: String,
    /// Backup schema version ([`BACKUP_VERSION`]).
    pub version: u32,
    pub exported_at: chrono::DateTime<chrono::Utc>,
    /// Gateway version that produced the backup (informational).
    pub gateway_version: String,
    pub encryption: BackupEncryption,
    pub data: BackupData,
}

/// Describes how secrets in the backup are protected, and carries a sentinel
/// that lets a restoring instance prove its encryption key matches before
/// touching the database.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct BackupEncryption {
    /// Whether the exporting instance had `OBLETH_ENCRYPTION_KEY` configured.
    pub cipher_enabled: bool,
    /// A known plaintext encrypted with the exporter's key. `None` when the
    /// cipher was disabled. Restore decrypts this first; a failure means the
    /// keys differ and the restore is rejected before any write.
    pub key_check: Option<String>,
    /// Whether the exporting instance hashed client keys with an
    /// `OBLETH_API_KEY_PEPPER`. A pepper mismatch can't be detected from the
    /// opaque hashes; this flag lets restore at least warn about it.
    #[serde(default)]
    pub api_key_pepper_set: bool,
}

/// The configuration rows themselves, in foreign-key order (groups before
/// tenants, tenants before keys, models before endpoints).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BackupData {
    #[serde(default)]
    pub fairshare_groups: Vec<FairshareGroupBackup>,
    #[serde(default)]
    pub tenants: Vec<TenantBackup>,
    #[serde(default)]
    pub api_keys: Vec<ApiKeyBackup>,
    #[serde(default)]
    pub models: Vec<ModelBackup>,
    #[serde(default)]
    pub model_endpoints: Vec<ModelEndpointBackup>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerBackup>,
    #[serde(default)]
    pub app_settings: Vec<AppSettingBackup>,
    /// Absent in backups taken before families existed.
    #[serde(default)]
    pub model_families: Vec<ModelFamily>,
    /// Slurm deployment specs (what to run and where). Absent in older backups.
    #[serde(default)]
    pub managed_models: Vec<ManagedModelBackup>,
    /// Recipes stored in the database. Absent in older backups.
    #[serde(default)]
    pub recipes: Vec<RecipeBackup>,
}

/// A Slurm deployment spec in a backup: what the operator set, without the
/// provisioner's runtime state (errors, pauses, backoff).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ManagedModelBackup {
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
    #[serde(default)]
    pub cpus_per_task: Option<i64>,
    #[serde(default)]
    pub mem: Option<String>,
    pub image: String,
    pub preamble: String,
    pub log_output_dir: String,
    pub launch_command: String,
    #[serde(default)]
    pub script_body: String,
    pub serving_port: i64,
    pub health_path: String,
    pub target_replicas: i64,
    #[serde(default = "default_min_replicas")]
    pub min_replicas: i64,
    pub max_job_failures: i64,
    #[serde(default = "default_startup_timeout_mins")]
    pub startup_timeout_mins: i64,
    #[serde(default = "default_handoff_mins")]
    pub handoff_mins: i64,
    #[serde(default)]
    pub replica_api_key: bool,
    #[serde(default)]
    #[schema(value_type = Option<Object>)]
    pub launcher_spec: Option<serde_json::Value>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<&ManagedModelSpec> for ManagedModelBackup {
    fn from(m: &ManagedModelSpec) -> Self {
        Self {
            model_id: m.model_id,
            enabled: m.enabled,
            partition: m.partition.clone(),
            gres: m.gres.clone(),
            nodes: m.nodes,
            constraints: m.constraints.clone(),
            exclude: m.exclude.clone(),
            account: m.account.clone(),
            qos: m.qos.clone(),
            time_limit: m.time_limit.clone(),
            cpus_per_task: m.cpus_per_task,
            mem: m.mem.clone(),
            image: m.image.clone(),
            preamble: m.preamble.clone(),
            log_output_dir: m.log_output_dir.clone(),
            launch_command: m.launch_command.clone(),
            script_body: m.script_body.clone(),
            serving_port: m.serving_port,
            health_path: m.health_path.clone(),
            target_replicas: m.target_replicas,
            min_replicas: m.min_replicas,
            max_job_failures: m.max_job_failures,
            startup_timeout_mins: m.startup_timeout_mins,
            handoff_mins: m.handoff_mins,
            replica_api_key: m.replica_api_key,
            launcher_spec: m.launcher_spec.clone(),
            created_at: m.created_at,
        }
    }
}

/// A database-stored recipe in a backup.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RecipeBackup {
    #[schema(value_type = String)]
    pub id: Uuid,
    pub name: String,
    pub body: String,
    #[serde(default)]
    pub author: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct FairshareGroupBackup {
    pub name: String,
    pub weight: i64,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// [`Tenant`] minus server-managed `updated_at`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct TenantBackup {
    pub id: Uuid,
    pub name: String,
    pub fairshare_group: String,
    pub weight: i64,
    pub tokens_per_minute: i64,
    pub max_in_flight: Option<i64>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub organization: String,
    #[serde(default)]
    pub contact_email: String,
    #[serde(default = "default_tenant_status")]
    pub status: String,
    #[serde(default = "default_timezone")]
    pub timezone: String,
    #[serde(default)]
    pub active_from: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub active_until: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub weekly_windows: Option<Vec<WeeklyWindow>>,
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    #[serde(default)]
    pub budget_period: Option<String>,
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub allowed_models: Option<Vec<String>>,
    /// Absent in backups taken before 2.12; a restore of one leaves the
    /// tenant unguarded and untraced, as it was then.
    #[serde(default)]
    pub guardrails_policy: Option<GuardrailsPolicy>,
    #[serde(default)]
    pub compression_policy: Option<CompressionPolicy>,
    #[serde(default)]
    pub tracing_enabled: bool,
    #[serde(default)]
    pub synthetic: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// [`ApiKey`] plus the stored `key_hash`, so client keys issued before the
/// backup keep authenticating after a restore. The hash is a one-way SHA-256
/// digest — it cannot be reversed into a usable secret.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ApiKeyBackup {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub key_prefix: String,
    pub key_hash: String,
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
    #[serde(default)]
    pub budget_tokens: Option<i64>,
    #[serde(default)]
    pub budget_cost_usd: Option<f64>,
    #[serde(default)]
    pub budget_period: Option<String>,
    #[serde(default)]
    pub budget_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Fairshare weight among this tenant's keys. Default 100.
    #[serde(default = "default_key_weight")]
    pub weight: i64,
    /// Per-model in-flight ceiling for this key. `None` = no cap.
    #[serde(default)]
    pub max_in_flight: Option<i64>,
    /// See `ApiKey::end_user_fairshare`. Defaults to off for backups taken
    /// before the column existed.
    #[serde(default)]
    pub end_user_fairshare: bool,
    /// See `ApiKey::background`. Defaults to off for backups taken before
    /// the column existed.
    #[serde(default)]
    pub background: bool,
    /// See `ApiKey::allowed_networks`. Defaults to any network for backups
    /// taken before the column existed.
    #[serde(default)]
    pub allowed_networks: Vec<String>,
    /// See `ApiKey::allowed_countries`. Defaults to none for backups taken
    /// before the column existed.
    #[serde(default)]
    pub allowed_countries: Vec<String>,
    pub disabled: bool,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// [`ModelRoute`] configuration columns plus health *configuration* (intervals,
/// thresholds, maintenance window). Runtime health state (status, failure
/// counters, last-check telemetry) is deliberately absent: a restored model is
/// re-probed from scratch. `api_key` carries the stored value verbatim —
/// ciphertext on encrypted instances.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelBackup {
    pub id: Uuid,
    pub model_name: String,
    /// Extra names this route answers to. Defaulted so backups taken before
    /// aliases existed restore as a model with none.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// Opt-in names with extra boons on. Defaulted so backups taken before
    /// variants existed restore as a model with none.
    #[serde(default)]
    pub variants: Vec<ModelVariant>,
    /// `#[serde(default)]`: a backup taken before lifecycles existed restores
    /// every model as active.
    #[serde(default)]
    pub lifecycle: ModelLifecycle,
    /// `#[serde(default)]`: a backup taken before effort maps existed
    /// restores every model as pass-through.
    #[serde(default)]
    pub reasoning: ModelReasoning,
    /// `#[serde(default)]`: a backup taken before the flag existed restores
    /// every model as listed.
    #[serde(default = "default_true")]
    pub listed: bool,
    /// `#[serde(default)]`: a backup taken before the list existed restores
    /// every model as refusing nothing.
    #[serde(default)]
    pub rejected_request_fields: Vec<String>,
    /// `#[serde(default)]`: a backup taken before the setting existed
    /// restores every model as following the gateway.
    #[serde(default)]
    pub session_affinity: ModelSessionAffinity,
    #[serde(default)]
    pub description: String,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    /// Upstream header values as stored (ciphertext on encrypted instances),
    /// like `api_key`. Absent in backups taken before the column existed.
    #[serde(default)]
    #[schema(value_type = std::collections::BTreeMap<String, String>)]
    pub upstream_headers: UpstreamHeaders,
    #[serde(default = "default_model_type")]
    pub model_type: String,
    /// Declared serving format. Defaulted so backups taken before the column
    /// existed restore as `unknown` rather than failing to parse.
    #[serde(default = "default_quantization")]
    pub quantization: String,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    #[serde(default)]
    pub cost_per_image: f64,
    #[serde(default)]
    pub cost_per_audio_second: f64,
    #[serde(default)]
    pub cost_per_character: f64,
    /// Absent in backups taken before the column existed.
    #[serde(default)]
    pub cost_per_video: f64,
    pub context_window: i64,
    pub admission_weight: i64,
    pub max_in_flight: Option<i64>,
    #[serde(default = "default_capacity_mode")]
    pub capacity_mode: String,
    #[serde(default)]
    pub capacity_tuned_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `discovered` mode inputs. Absent in backups taken before the columns
    /// existed.
    #[serde(default = "default_capacity_source")]
    pub capacity_source: String,
    #[serde(default)]
    pub capacity_namespace: Option<String>,
    #[serde(default)]
    pub capacity_service: Option<String>,
    #[serde(default)]
    pub per_replica_max_in_flight: Option<i64>,
    #[serde(default = "default_capacity_headroom")]
    pub capacity_headroom: f64,
    pub supports_function_calling: bool,
    pub supports_system_messages: bool,
    pub supports_response_schema: bool,
    pub supports_tool_choice: bool,
    #[serde(default)]
    pub supports_vision: bool,
    pub enabled: bool,
    pub cache_enabled: bool,
    pub cache_ttl_secs: i64,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub boons: Vec<String>,
    #[serde(default)]
    pub tool_servers: Vec<String>,
    /// Operator thumb on the scale for `auto` routing. Defaulted rather than
    /// required so backups taken before the column existed still restore.
    #[serde(default = "default_route_bias")]
    pub route_bias: f64,
    /// Whether `auto` may select this model. Defaulted to eligible so backups
    /// taken before the column existed restore without excluding anything.
    #[serde(default = "default_auto_eligible")]
    pub auto_eligible: bool,
    /// Which small model writes this model's speculation drafts. Empty falls
    /// back to the fleet default in the boon settings.
    #[serde(default)]
    pub draft_model: String,
    /// Direct (non-gateway) URL of a deployment of THIS model whose backend
    /// supports `prompt_logprobs`; it scores every draft token. Empty means
    /// this model cannot speculate.
    #[serde(default)]
    pub verify_api_base: String,
    /// The model name that scoring backend serves, when it differs from this
    /// model's own `upstream_model` (a canary serving its own name). Empty =
    /// same as `upstream_model`.
    #[serde(default)]
    pub verify_upstream_model: String,
    #[serde(default)]
    pub request_timeout_secs: Option<i64>,
    #[serde(default)]
    pub max_retries: i64,
    #[serde(default = "default_retry_backoff_ms")]
    pub retry_backoff_ms: i64,
    #[serde(default = "default_endpoint_selection_mode")]
    pub endpoint_selection_mode: String,
    /// Opt-in upstream diagnostics. `#[serde(default)]` keeps backups written
    /// before this field was carried here readable; they restore to the same
    /// column default such a backup already implied.
    #[serde(default)]
    pub debug_diagnostics: bool,
    /// Declared saturation for energy accounting. Same compatibility note as
    /// `debug_diagnostics`.
    #[serde(default)]
    pub energy_slots_per_node: i64,
    pub health_checks_enabled: bool,
    pub health_alerts_enabled: bool,
    pub health_check_interval_secs: i64,
    pub health_failure_threshold: i64,
    #[serde(default)]
    pub health_maintenance_until: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub health_maintenance_note: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// [`ModelEndpoint`] configuration columns, without runtime health state.
/// `api_key` carries the stored value verbatim.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelEndpointBackup {
    pub id: Uuid,
    #[schema(value_type = String)]
    pub model_id: Uuid,
    pub name: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub priority: i64,
    pub weight: i64,
    pub enabled: bool,
    /// Absent in backups taken before the column existed.
    #[serde(default)]
    pub max_in_flight: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// [`McpServer`] with `auth_header` carried verbatim as stored.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct McpServerBackup {
    pub id: Uuid,
    pub name: String,
    pub upstream_url: String,
    pub auth_header: Option<String>,
    pub enabled: bool,
    /// Absent in a backup made before the field existed: restored as no tenants.
    #[serde(default)]
    pub allowed_tenants: Vec<Uuid>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

/// One `app_settings` row, value carried verbatim. Exported generically so
/// settings keys added after this format shipped are still covered.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct AppSettingBackup {
    pub key: String,
    #[schema(value_type = Object)]
    pub value: serde_json::Value,
}

/// Per-entity insert/update tallies from a restore.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema)]
pub struct RestoreCounts {
    pub inserted: u64,
    pub updated: u64,
}

/// Outcome of a successful restore: what was inserted vs updated, plus any
/// non-fatal warnings (e.g. a possible API-key pepper mismatch).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct RestoreReport {
    pub fairshare_groups: RestoreCounts,
    pub tenants: RestoreCounts,
    pub api_keys: RestoreCounts,
    pub models: RestoreCounts,
    pub model_endpoints: RestoreCounts,
    pub mcp_servers: RestoreCounts,
    pub app_settings: RestoreCounts,
    #[serde(default)]
    pub model_families: RestoreCounts,
    #[serde(default)]
    pub managed_models: RestoreCounts,
    #[serde(default)]
    pub recipes: RestoreCounts,
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[cfg(test)]
mod tests {
    /// A key's network list survives the trip through a backup file, and a
    /// backup taken before the field existed restores as "any network".
    #[test]
    fn backups_carry_allowed_networks() {
        let raw = serde_json::json!({
            "id": uuid::Uuid::nil(), "tenant_id": uuid::Uuid::nil(), "name": "k",
            "key_prefix": "obl_0123456789abcd", "key_hash": "h", "disabled": false,
            "created_at": "2026-01-01T00:00:00Z"
        });
        let old: super::ApiKeyBackup = serde_json::from_value(raw.clone()).unwrap();
        assert!(old.allowed_networks.is_empty());
        assert!(old.allowed_countries.is_empty());
        let mut with = raw;
        with["allowed_networks"] = serde_json::json!(["203.0.113.0/24", "2001:db8::/48"]);
        with["allowed_countries"] = serde_json::json!(["US", "CA"]);
        let backup: super::ApiKeyBackup = serde_json::from_value(with).unwrap();
        let again: super::ApiKeyBackup =
            serde_json::from_str(&serde_json::to_string(&backup).unwrap()).unwrap();
        assert_eq!(again.allowed_networks, ["203.0.113.0/24", "2001:db8::/48"]);
        assert_eq!(again.allowed_countries, ["US", "CA"]);
    }
}
