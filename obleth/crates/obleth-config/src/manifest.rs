//! The `obleth-models` manifest: a portable, hand-editable description of the
//! model registry.
//!
//! This is deliberately *not* the config backup ([`crate::types::ConfigBackup`]).
//! The backup is a whole-instance disaster-recovery artifact keyed by uuid and
//! carrying secrets as ciphertext. The manifest exists for the other job —
//! reviewing and bulk-editing model configuration by hand, or keeping it in a
//! repository — so it differs on every axis that job cares about:
//!
//! * **Keyed by `model_name`**, not `id`. Nobody hand-writes uuids, and
//!   `models.model_name` is `unique` in the schema, so it is a real key.
//! * **Sparse.** Every field except `model_name` is optional and an absent
//!   field means *leave it alone*. A three-line file that retags one model is
//!   valid and touches nothing else.
//! * **No secrets on the way out.** Export reports `has_api_key` rather than
//!   the key itself, so a manifest is safe to commit. Import still accepts a
//!   plaintext `api_key` when you want to set one, and leaves the stored key
//!   untouched when you don't. That means a manifest needs no encryption
//!   envelope and moves between instances with different
//!   `OBLETH_ENCRYPTION_KEY`s. Per-model upstream headers follow the same
//!   rule: export lists `upstream_header_names`, never the values, and import
//!   takes `upstream_headers` when you want to set them.
//!
//! # Coverage boundary
//!
//! The manifest covers exactly the operator-settable fields of
//! [`ModelRoute`] — see [`ModelConfig`]. It does **not** carry per-model health
//! *check* configuration or maintenance windows (managed from the health page,
//! and not part of `ModelRoute`), nor derived state (`id`, `capacity_tuned_at`,
//! timestamps, runtime health), nor the model's endpoints' health state. The
//! boundary is stated here, and in the dashboard card, rather than left for
//! someone to discover from a silently-missing field.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::types::{
    is_valid_capacity_mode, is_valid_endpoint_selection_mode, is_valid_model_type,
    is_valid_quantization, merge_upstream_headers, normalize_aliases, normalize_boons,
    normalize_tool_servers, parse_tag_level, upstream_header_names, ModelEndpoint, ModelRoute,
    UpstreamHeaders, UpstreamHeadersWrite, CAPACITY_MODES, DEFAULT_CAPACITY_MODE,
    DEFAULT_CAPACITY_SOURCE, DEFAULT_ENDPOINT_SELECTION_MODE, DEFAULT_MODEL_TYPE,
    DEFAULT_QUANTIZATION, DEFAULT_RETRY_BACKOFF_MS, ENDPOINT_SELECTION_MODES, MAX_MODEL_ALIASES,
    MODEL_TYPES, QUANTIZATIONS,
};

/// File-format discriminator for model manifests.
pub const MODEL_MANIFEST_FORMAT: &str = "obleth-models";
/// Current manifest schema version. Bump when the file shape changes.
pub const MODEL_MANIFEST_VERSION: u32 = 1;

/// Defaults applied to fields a manifest omits when *creating* a model. These
/// mirror the admin create form's defaults so a model born from a manifest is
/// indistinguishable from one created in the dashboard.
const DEFAULT_CONTEXT_WINDOW: i64 = 8192;
const DEFAULT_ADMISSION_WEIGHT: i64 = 100;
const DEFAULT_CACHE_TTL_SECS: i64 = 300;
const DEFAULT_ENDPOINT_PRIORITY: i64 = 100;
const DEFAULT_ENDPOINT_WEIGHT: i64 = 100;

// ---- the file ---------------------------------------------------------------

/// A model manifest document.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelManifest {
    /// Always [`MODEL_MANIFEST_FORMAT`].
    pub format: String,
    /// Manifest schema version ([`MODEL_MANIFEST_VERSION`]).
    pub version: u32,
    /// Informational; ignored on import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exported_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Gateway version that produced the file. Informational; ignored on import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway_version: Option<String>,
    pub models: Vec<ManifestModel>,
}

/// One model in a manifest. `model_name` identifies the row; every other field
/// is optional and absent means "leave unchanged" (or, for a model that does
/// not exist yet, "use the default").
///
/// Nullable columns (`max_in_flight`, `request_timeout_secs`,
/// `per_replica_max_in_flight`, an endpoint's `max_in_flight`) cannot be
/// *cleared* from a manifest — a JSON `null` reads the same as an absent field.
/// This matches the existing `PUT /api/v1/models/{id}` behaviour, where those
/// fields also fall back to the stored value; clear them from the dashboard.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ManifestModel {
    /// The key. Matched against `models.model_name`; created when absent.
    pub model_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    /// Set the upstream credential. Never written by export — see `has_api_key`.
    /// Absent leaves the stored key untouched; an empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Export-only: whether a credential is stored. Ignored on import, so an
    /// exported manifest can be re-imported unchanged without wiping keys.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_api_key: Option<bool>,
    /// Set the headers sent on every upstream request for this model. Never
    /// written by export — see `upstream_header_names`. The whole set is
    /// replaced when present: a `null` value keeps the stored value for that
    /// name, and a stored name left out is removed. Absent leaves them alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_headers: Option<UpstreamHeadersWrite>,
    /// Export-only: the names of the stored upstream headers. Ignored on
    /// import, like `has_api_key`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_header_names: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_type: Option<String>,
    /// Extra client-facing names for this model. The whole list is replaced
    /// when present, so removing an alias is spelled by omitting it from the
    /// list rather than by any delete syntax; absent leaves aliases alone.
    /// Blank and duplicate entries are dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    /// Serving format from the fixed `QUANTIZATIONS` vocabulary. An
    /// unrecognized value is rejected rather than silently defaulted, on the
    /// same reasoning as `model_type`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_cost_per_token: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_cost_per_token: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_image: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_audio_second: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_character: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_per_video: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_window: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_weight: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_in_flight: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_mode: Option<String>,
    /// `discovered` mode: `endpoints` or `kubernetes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_source: Option<String>,
    /// `kubernetes` source namespace; an empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_namespace: Option<String>,
    /// `kubernetes` source Service name; an empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_service: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_replica_max_in_flight: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_headroom: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_function_calling: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_system_messages: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_response_schema: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_tool_choice: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_vision: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_ttl_secs: Option<i64>,
    /// Routing tags from the fixed `MODEL_TAGS` vocabulary, each optionally
    /// carrying a `:1|2|3` strength suffix. Unknown tags are dropped with a
    /// warning rather than failing the import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    /// Gateway boons from the fixed `MODEL_BOONS` vocabulary. Unknown boons are
    /// dropped with a warning.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub boons: Option<Vec<String>>,
    /// MCP servers whose tools this model may use. Names that are not
    /// registered are kept but warned about — registering the server later
    /// makes the grant live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_servers: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_timeout_secs: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_backoff_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_selection_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug_diagnostics: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub energy_slots_per_node: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_bias: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_eligible: Option<bool>,
    /// This model's own drafter for speculation; empty clears it (fleet
    /// default applies). Absent leaves the stored value alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_model: Option<String>,
    /// Direct scoring URL for this model's drafts; empty clears it (the model
    /// stops speculating). Absent leaves the stored value alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_api_base: Option<String>,
    /// Name the scoring backend serves when it differs from `upstream_model`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_upstream_model: Option<String>,
    /// This model's endpoints, matched by `name` within the model. Absent
    /// leaves the model's endpoints alone entirely; endpoints present on the
    /// gateway but missing from the list are never deleted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoints: Option<Vec<ManifestEndpoint>>,
}

/// One endpoint under a model. `name` is the key within the model.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ManifestEndpoint {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base: Option<String>,
    /// Absent leaves the stored key untouched; an empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// Export-only, ignored on import. See [`ManifestModel::has_api_key`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub has_api_key: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// Requests this endpoint takes at once, for a `discovered` model using
    /// the `endpoints` source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_in_flight: Option<i64>,
}

// ---- the report -------------------------------------------------------------

/// What an import did, or — under `dry_run` — what it would have done.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct ModelImportReport {
    /// True when nothing was written.
    pub dry_run: bool,
    pub created: usize,
    pub updated: usize,
    pub unchanged: usize,
    /// One entry per model in the manifest, in file order.
    pub models: Vec<ModelImportEntry>,
}

/// The outcome for a single manifest entry.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct ModelImportEntry {
    pub model_name: String,
    /// `created`, `updated`, or `unchanged`.
    pub action: String,
    /// Names of the fields whose values this import changes. Empty when the
    /// action is `unchanged`; for `created`, lists the fields the manifest set
    /// explicitly (defaults are not reported as changes).
    #[serde(default)]
    pub changed_fields: Vec<String>,
    /// Non-fatal problems: dropped tags/boons, unregistered tool servers.
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// Import actions, as written into [`ModelImportEntry::action`].
pub const IMPORT_ACTION_CREATED: &str = "created";
pub const IMPORT_ACTION_UPDATED: &str = "updated";
pub const IMPORT_ACTION_UNCHANGED: &str = "unchanged";

// ---- the resolved target ----------------------------------------------------

/// Every operator-settable field of a `models` row, fully resolved — no
/// "absent" state left. Produced by [`resolve_model`] and written verbatim by
/// the store's importer, which keeps all the merge logic here where it can be
/// unit-tested without a database.
#[derive(Debug, Clone, PartialEq)]
pub struct ModelConfig {
    pub description: String,
    pub aliases: Vec<String>,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub upstream_headers: UpstreamHeaders,
    pub model_type: String,
    pub quantization: String,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cost_per_image: f64,
    pub cost_per_audio_second: f64,
    pub cost_per_character: f64,
    pub cost_per_video: f64,
    pub context_window: i64,
    pub admission_weight: i64,
    pub max_in_flight: Option<i64>,
    pub capacity_mode: String,
    pub capacity_source: String,
    pub capacity_namespace: Option<String>,
    pub capacity_service: Option<String>,
    pub per_replica_max_in_flight: Option<i64>,
    pub capacity_headroom: f64,
    pub supports_function_calling: bool,
    pub supports_system_messages: bool,
    pub supports_response_schema: bool,
    pub supports_tool_choice: bool,
    pub supports_vision: bool,
    pub enabled: bool,
    pub cache_enabled: bool,
    pub cache_ttl_secs: i64,
    pub tags: Vec<String>,
    pub boons: Vec<String>,
    pub tool_servers: Vec<String>,
    pub request_timeout_secs: Option<i64>,
    pub max_retries: i64,
    pub retry_backoff_ms: i64,
    pub endpoint_selection_mode: String,
    pub debug_diagnostics: bool,
    pub energy_slots_per_node: i64,
    pub route_bias: f64,
    pub auto_eligible: bool,
    pub draft_model: String,
    pub verify_api_base: String,
    pub verify_upstream_model: String,
}

impl Default for ModelConfig {
    /// The shape of a model created from a manifest that sets nothing but a
    /// name. Mirrors the admin create form's defaults.
    fn default() -> Self {
        Self {
            description: String::new(),
            aliases: Vec::new(),
            upstream_model: String::new(),
            api_base: String::new(),
            api_key: None,
            upstream_headers: UpstreamHeaders::new(),
            model_type: DEFAULT_MODEL_TYPE.to_string(),
            quantization: DEFAULT_QUANTIZATION.to_string(),
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            cost_per_video: 0.0,
            context_window: DEFAULT_CONTEXT_WINDOW,
            admission_weight: DEFAULT_ADMISSION_WEIGHT,
            max_in_flight: None,
            capacity_mode: DEFAULT_CAPACITY_MODE.to_string(),
            capacity_source: DEFAULT_CAPACITY_SOURCE.to_string(),
            capacity_namespace: None,
            capacity_service: None,
            per_replica_max_in_flight: None,
            capacity_headroom: 1.0,
            supports_function_calling: false,
            // Matches `create_model`: system messages are assumed supported.
            supports_system_messages: true,
            supports_response_schema: false,
            supports_tool_choice: false,
            supports_vision: false,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: DEFAULT_CACHE_TTL_SECS,
            tags: Vec::new(),
            boons: Vec::new(),
            tool_servers: Vec::new(),
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: DEFAULT_RETRY_BACKOFF_MS,
            endpoint_selection_mode: DEFAULT_ENDPOINT_SELECTION_MODE.to_string(),
            debug_diagnostics: false,
            energy_slots_per_node: 0,
            route_bias: 1.0,
            auto_eligible: true,
            draft_model: String::new(),
            verify_api_base: String::new(),
            verify_upstream_model: String::new(),
        }
    }
}

impl From<&ModelRoute> for ModelConfig {
    fn from(m: &ModelRoute) -> Self {
        Self {
            description: m.description.clone(),
            aliases: m.aliases.clone(),
            upstream_model: m.upstream_model.clone(),
            api_base: m.api_base.clone(),
            api_key: m.api_key.clone(),
            upstream_headers: m.upstream_headers.clone(),
            model_type: m.model_type.clone(),
            quantization: m.quantization.clone(),
            input_cost_per_token: m.input_cost_per_token,
            output_cost_per_token: m.output_cost_per_token,
            cost_per_image: m.cost_per_image,
            cost_per_audio_second: m.cost_per_audio_second,
            cost_per_character: m.cost_per_character,
            cost_per_video: m.cost_per_video,
            context_window: m.context_window,
            admission_weight: m.admission_weight,
            max_in_flight: m.max_in_flight,
            capacity_mode: m.capacity_mode.clone(),
            capacity_source: m.capacity_source.clone(),
            capacity_namespace: m.capacity_namespace.clone(),
            capacity_service: m.capacity_service.clone(),
            per_replica_max_in_flight: m.per_replica_max_in_flight,
            capacity_headroom: m.capacity_headroom,
            supports_function_calling: m.supports_function_calling,
            supports_system_messages: m.supports_system_messages,
            supports_response_schema: m.supports_response_schema,
            supports_tool_choice: m.supports_tool_choice,
            supports_vision: m.supports_vision,
            enabled: m.enabled,
            cache_enabled: m.cache_enabled,
            cache_ttl_secs: m.cache_ttl_secs,
            tags: m.tags.clone(),
            boons: m.boons.clone(),
            tool_servers: m.tool_servers.clone(),
            request_timeout_secs: m.request_timeout_secs,
            max_retries: m.max_retries,
            retry_backoff_ms: m.retry_backoff_ms,
            endpoint_selection_mode: m.endpoint_selection_mode.clone(),
            debug_diagnostics: m.debug_diagnostics,
            energy_slots_per_node: m.energy_slots_per_node,
            route_bias: m.route_bias,
            auto_eligible: m.auto_eligible,
            draft_model: m.draft_model.clone(),
            verify_api_base: m.verify_api_base.clone(),
            verify_upstream_model: m.verify_upstream_model.clone(),
        }
    }
}

impl ModelConfig {
    /// Field names where `self` and `other` differ, in declaration order.
    ///
    /// Floats are compared exactly on purpose: the question is "does this
    /// import write a different value than the one stored", and an exported
    /// value re-imported unchanged is bit-identical. An epsilon here would
    /// report a real (if tiny) price edit as `unchanged`.
    #[allow(clippy::float_cmp)]
    pub fn diff(&self, other: &Self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut note = |changed: bool, name: &str| {
            if changed {
                out.push(name.to_string());
            }
        };
        note(self.description != other.description, "description");
        note(self.aliases != other.aliases, "aliases");
        note(
            self.upstream_model != other.upstream_model,
            "upstream_model",
        );
        note(self.api_base != other.api_base, "api_base");
        note(self.api_key != other.api_key, "api_key");
        note(
            self.upstream_headers != other.upstream_headers,
            "upstream_headers",
        );
        note(self.model_type != other.model_type, "model_type");
        note(self.quantization != other.quantization, "quantization");
        note(
            self.input_cost_per_token != other.input_cost_per_token,
            "input_cost_per_token",
        );
        note(
            self.output_cost_per_token != other.output_cost_per_token,
            "output_cost_per_token",
        );
        note(
            self.cost_per_image != other.cost_per_image,
            "cost_per_image",
        );
        note(
            self.cost_per_audio_second != other.cost_per_audio_second,
            "cost_per_audio_second",
        );
        note(
            self.cost_per_character != other.cost_per_character,
            "cost_per_character",
        );
        note(
            self.cost_per_video != other.cost_per_video,
            "cost_per_video",
        );
        note(
            self.context_window != other.context_window,
            "context_window",
        );
        note(
            self.admission_weight != other.admission_weight,
            "admission_weight",
        );
        note(self.max_in_flight != other.max_in_flight, "max_in_flight");
        note(self.capacity_mode != other.capacity_mode, "capacity_mode");
        note(
            self.capacity_source != other.capacity_source,
            "capacity_source",
        );
        note(
            self.capacity_namespace != other.capacity_namespace,
            "capacity_namespace",
        );
        note(
            self.capacity_service != other.capacity_service,
            "capacity_service",
        );
        note(
            self.per_replica_max_in_flight != other.per_replica_max_in_flight,
            "per_replica_max_in_flight",
        );
        note(
            self.capacity_headroom != other.capacity_headroom,
            "capacity_headroom",
        );
        note(
            self.supports_function_calling != other.supports_function_calling,
            "supports_function_calling",
        );
        note(
            self.supports_system_messages != other.supports_system_messages,
            "supports_system_messages",
        );
        note(
            self.supports_response_schema != other.supports_response_schema,
            "supports_response_schema",
        );
        note(
            self.supports_tool_choice != other.supports_tool_choice,
            "supports_tool_choice",
        );
        note(
            self.supports_vision != other.supports_vision,
            "supports_vision",
        );
        note(self.enabled != other.enabled, "enabled");
        note(self.cache_enabled != other.cache_enabled, "cache_enabled");
        note(
            self.cache_ttl_secs != other.cache_ttl_secs,
            "cache_ttl_secs",
        );
        note(self.tags != other.tags, "tags");
        note(self.boons != other.boons, "boons");
        note(self.tool_servers != other.tool_servers, "tool_servers");
        note(
            self.request_timeout_secs != other.request_timeout_secs,
            "request_timeout_secs",
        );
        note(self.max_retries != other.max_retries, "max_retries");
        note(
            self.retry_backoff_ms != other.retry_backoff_ms,
            "retry_backoff_ms",
        );
        note(
            self.endpoint_selection_mode != other.endpoint_selection_mode,
            "endpoint_selection_mode",
        );
        note(
            self.debug_diagnostics != other.debug_diagnostics,
            "debug_diagnostics",
        );
        note(
            self.energy_slots_per_node != other.energy_slots_per_node,
            "energy_slots_per_node",
        );
        note(self.route_bias != other.route_bias, "route_bias");
        note(self.auto_eligible != other.auto_eligible, "auto_eligible");
        note(self.draft_model != other.draft_model, "draft_model");
        note(
            self.verify_api_base != other.verify_api_base,
            "verify_api_base",
        );
        note(
            self.verify_upstream_model != other.verify_upstream_model,
            "verify_upstream_model",
        );
        out
    }
}

// ---- resolution -------------------------------------------------------------

/// A manifest entry that could not be applied. Rejections are per-entry so the
/// caller can report every problem in one pass instead of making the operator
/// fix a 40-model file one error at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    pub model_name: String,
    pub message: String,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "model '{}': {}", self.model_name, self.message)
    }
}

/// A manifest entry merged onto the current state of the model.
#[derive(Debug, Clone)]
pub struct ResolvedManifestModel {
    pub model_name: String,
    /// The full target configuration to write.
    pub config: ModelConfig,
    /// Fields whose values differ from what is stored (empty for a no-op).
    pub changed_fields: Vec<String>,
    /// Non-fatal problems worth showing the operator.
    pub warnings: Vec<String>,
    /// True when no model with this name exists yet.
    pub is_new: bool,
}

impl ResolvedManifestModel {
    pub fn action(&self) -> &'static str {
        if self.is_new {
            IMPORT_ACTION_CREATED
        } else if self.changed_fields.is_empty() {
            IMPORT_ACTION_UNCHANGED
        } else {
            IMPORT_ACTION_UPDATED
        }
    }
}

/// Merge one manifest entry onto the model as it exists today (`None` to
/// create), validating as we go.
///
/// Validation splits two ways on purpose:
///
/// * **Rejected** (returns `Err`): a value from a closed vocabulary that we do
///   not recognise — `model_type`, `capacity_mode`, `endpoint_selection_mode`.
///   The normalizers these fields use elsewhere silently fall back to the
///   default, which would quietly turn a typo'd `embeddings` model into a
///   `chat` model. In a hand-edited file that is worse than failing.
/// * **Warned** (kept in `warnings`): tags and boons outside their vocabulary
///   are dropped, matching how every other write path treats them, and
///   unregistered tool servers are kept as-is. These weaken routing at worst,
///   and partial validity across a list is normal.
pub fn resolve_model(
    entry: &ManifestModel,
    existing: Option<&ModelRoute>,
    registered_tool_servers: &[String],
) -> Result<ResolvedManifestModel, ManifestError> {
    let name = entry.model_name.trim();
    if name.is_empty() {
        return Err(ManifestError {
            model_name: entry.model_name.clone(),
            message: "model_name must not be empty".to_string(),
        });
    }
    let reject = |message: String| ManifestError {
        model_name: name.to_string(),
        message,
    };

    let current: ModelConfig = existing.map(ModelConfig::from).unwrap_or_default();
    let mut next = current.clone();
    let mut warnings: Vec<String> = Vec::new();

    if let Some(v) = &entry.description {
        next.description = v.clone();
    }
    if let Some(v) = &entry.upstream_model {
        next.upstream_model = v.clone();
    }
    if let Some(v) = &entry.api_base {
        next.api_base = v.clone();
    }
    // An explicit empty string clears the credential; absent leaves it alone.
    if let Some(v) = &entry.api_key {
        next.api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }
    if let Some(write) = &entry.upstream_headers {
        next.upstream_headers =
            merge_upstream_headers(&current.upstream_headers, write).map_err(&reject)?;
    }
    if let Some(v) = &entry.model_type {
        let t = v.trim().to_ascii_lowercase();
        if !is_valid_model_type(&t) {
            return Err(reject(format!(
                "unknown model_type '{v}' (expected one of: {})",
                MODEL_TYPES.join(", ")
            )));
        }
        next.model_type = t;
    }
    if let Some(v) = &entry.quantization {
        let q = v.trim().to_ascii_lowercase();
        if !is_valid_quantization(&q) {
            return Err(reject(format!(
                "unknown quantization '{v}' (expected one of: {})",
                QUANTIZATIONS.join(", ")
            )));
        }
        next.quantization = q;
    }
    if let Some(v) = &entry.capacity_mode {
        let m = v.trim().to_ascii_lowercase();
        if !is_valid_capacity_mode(&m) {
            return Err(reject(format!(
                "unknown capacity_mode '{v}' (expected one of: {})",
                CAPACITY_MODES.join(", ")
            )));
        }
        next.capacity_mode = m;
    }
    if let Some(v) = &entry.capacity_source {
        next.capacity_source = v.trim().to_ascii_lowercase();
    }
    if let Some(v) = &entry.capacity_namespace {
        next.capacity_namespace = crate::capacity::normalize_optional_text(Some(v));
    }
    if let Some(v) = &entry.capacity_service {
        next.capacity_service = crate::capacity::normalize_optional_text(Some(v));
    }
    if let Some(v) = entry.per_replica_max_in_flight {
        next.per_replica_max_in_flight = Some(v);
    }
    if let Some(v) = entry.capacity_headroom {
        next.capacity_headroom = v;
    }
    // Syntax only: whether this gateway can read a `kubernetes` source (its
    // namespace allowlist, its default Service) and whether a per-replica
    // value is there when the mode needs one (which depends on the model's
    // endpoints) are the importer's checks.
    crate::capacity::validate_discovery_fields(
        name,
        &next.upstream_model,
        &crate::capacity::DiscoveryFields {
            source: next.capacity_source.clone(),
            namespace: next.capacity_namespace.clone(),
            service: next.capacity_service.clone(),
            per_replica_max_in_flight: next.per_replica_max_in_flight,
            headroom: next.capacity_headroom,
        },
        false,
        crate::capacity::DiscoveryPolicy {
            namespaces: &[],
            default_service: "",
        },
        &[],
    )
    .map_err(reject)?;
    if let Some(v) = &entry.endpoint_selection_mode {
        let m = v.trim().to_ascii_lowercase();
        if !is_valid_endpoint_selection_mode(&m) {
            return Err(reject(format!(
                "unknown endpoint_selection_mode '{v}' (expected one of: {})",
                ENDPOINT_SELECTION_MODES.join(", ")
            )));
        }
        next.endpoint_selection_mode = m;
    }

    if let Some(v) = entry.input_cost_per_token {
        next.input_cost_per_token = reject_negative(v, "input_cost_per_token").map_err(reject)?;
    }
    if let Some(v) = entry.output_cost_per_token {
        next.output_cost_per_token = reject_negative(v, "output_cost_per_token").map_err(reject)?;
    }
    if let Some(v) = entry.cost_per_image {
        next.cost_per_image = reject_negative(v, "cost_per_image").map_err(reject)?;
    }
    if let Some(v) = entry.cost_per_audio_second {
        next.cost_per_audio_second = reject_negative(v, "cost_per_audio_second").map_err(reject)?;
    }
    if let Some(v) = entry.cost_per_character {
        next.cost_per_character = reject_negative(v, "cost_per_character").map_err(reject)?;
    }
    if let Some(v) = entry.cost_per_video {
        next.cost_per_video = reject_negative(v, "cost_per_video").map_err(reject)?;
    }
    if let Some(v) = entry.route_bias {
        if !v.is_finite() || v < 0.0 {
            return Err(reject(format!(
                "route_bias must be a finite number >= 0 (got {v})"
            )));
        }
        next.route_bias = v;
    }

    if let Some(v) = entry.context_window {
        next.context_window = v.max(0);
    }
    if let Some(v) = entry.admission_weight {
        next.admission_weight = v.max(1);
    }
    if let Some(v) = entry.max_in_flight {
        next.max_in_flight = Some(v.max(1));
    }
    if let Some(v) = entry.cache_ttl_secs {
        next.cache_ttl_secs = v.max(0);
    }
    if let Some(v) = entry.request_timeout_secs {
        next.request_timeout_secs = Some(v.max(1));
    }
    if let Some(v) = entry.max_retries {
        next.max_retries = v.max(0);
    }
    if let Some(v) = entry.retry_backoff_ms {
        next.retry_backoff_ms = v.max(0);
    }
    if let Some(v) = entry.energy_slots_per_node {
        next.energy_slots_per_node = v.max(0);
    }

    if let Some(v) = entry.supports_function_calling {
        next.supports_function_calling = v;
    }
    if let Some(v) = entry.supports_system_messages {
        next.supports_system_messages = v;
    }
    if let Some(v) = entry.supports_response_schema {
        next.supports_response_schema = v;
    }
    if let Some(v) = entry.supports_tool_choice {
        next.supports_tool_choice = v;
    }
    if let Some(v) = entry.supports_vision {
        next.supports_vision = v;
    }
    if let Some(v) = entry.enabled {
        next.enabled = v;
    }
    if let Some(v) = entry.cache_enabled {
        next.cache_enabled = v;
    }
    if let Some(v) = entry.debug_diagnostics {
        next.debug_diagnostics = v;
    }
    if let Some(v) = entry.auto_eligible {
        next.auto_eligible = v;
    }
    if let Some(v) = &entry.draft_model {
        next.draft_model = v.trim().to_string();
    }
    if let Some(v) = &entry.verify_api_base {
        next.verify_api_base = v.trim().to_string();
    }
    if let Some(v) = &entry.verify_upstream_model {
        next.verify_upstream_model = v.trim().to_string();
    }

    if let Some(raw) = &entry.aliases {
        next.aliases = normalize_aliases(raw);
        // An alias colliding with a real `model_name` is caught by the
        // importer, which is the only layer that can see the other rows. Here
        // we can only report what normalization threw away.
        let dropped = raw.len() - next.aliases.len();
        if dropped > 0 {
            warnings.push(format!(
                "dropped {dropped} blank, duplicate, or over-cap alias(es) (at most {MAX_MODEL_ALIASES} are kept)"
            ));
        }
    }

    if let Some(raw) = &entry.tags {
        // Store the suffixed form so a declared strength level survives, the
        // same shape `serialize_tag_levels` writes on every other path. An
        // explicit `:1` stays explicit: bare = derive from cost rank under
        // hybrid tiering, `tag:1` = pinned to the bottom tier.
        next.tags = crate::canonical_tags(raw);
        let dropped: Vec<&str> = raw
            .iter()
            .filter(|t| parse_tag_level(t).is_none())
            .map(|t| t.as_str())
            .collect();
        if !dropped.is_empty() {
            warnings.push(format!(
                "dropped unknown tag(s): {} — these do not exist in the routing vocabulary and would never match",
                dropped.join(", ")
            ));
        }
    }
    if let Some(raw) = &entry.boons {
        next.boons = normalize_boons(raw);
        let dropped: Vec<&str> = raw
            .iter()
            .filter(|b| !next.boons.contains(&b.trim().to_ascii_lowercase()))
            .map(|b| b.as_str())
            .collect();
        if !dropped.is_empty() {
            warnings.push(format!("dropped unknown boon(s): {}", dropped.join(", ")));
        }
    }
    if let Some(raw) = &entry.tool_servers {
        next.tool_servers = normalize_tool_servers(raw);
        let unknown: Vec<&str> = next
            .tool_servers
            .iter()
            .filter(|s| !registered_tool_servers.iter().any(|r| r == *s))
            .map(|s| s.as_str())
            .collect();
        if !unknown.is_empty() {
            warnings.push(format!(
                "tool server(s) not registered on this gateway: {} — the grant stays inert until they are added",
                unknown.join(", ")
            ));
        }
    }

    let changed_fields = current.diff(&next);
    Ok(ResolvedManifestModel {
        model_name: name.to_string(),
        config: next,
        changed_fields,
        warnings,
        is_new: existing.is_none(),
    })
}

/// An endpoint's resolved target state. Endpoint rows carry far less config
/// than models, so this is the whole settable surface.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointConfig {
    pub name: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub priority: i64,
    pub weight: i64,
    pub enabled: bool,
    pub max_in_flight: Option<i64>,
}

impl Default for EndpointConfig {
    /// Mirrors the admin create-endpoint form's defaults.
    fn default() -> Self {
        Self {
            name: String::new(),
            api_base: String::new(),
            api_key: None,
            priority: DEFAULT_ENDPOINT_PRIORITY,
            weight: DEFAULT_ENDPOINT_WEIGHT,
            enabled: true,
            max_in_flight: None,
        }
    }
}

impl From<&ModelEndpoint> for EndpointConfig {
    fn from(e: &ModelEndpoint) -> Self {
        Self {
            name: e.name.clone(),
            api_base: e.api_base.clone(),
            api_key: e.api_key.clone(),
            priority: e.priority,
            weight: e.weight,
            enabled: e.enabled,
            max_in_flight: e.max_in_flight,
        }
    }
}

/// An endpoint entry merged onto the endpoint as it exists today.
#[derive(Debug, Clone)]
pub struct ResolvedManifestEndpoint {
    pub config: EndpointConfig,
    pub changed_fields: Vec<String>,
    pub is_new: bool,
}

/// Merge one endpoint entry onto the endpoint as it exists today (`None` to
/// create). Same sparse rules as [`resolve_model`]: absent means unchanged, and
/// an empty `api_key` string clears the stored credential.
pub fn resolve_endpoint(
    entry: &ManifestEndpoint,
    existing: Option<&ModelEndpoint>,
    model_name: &str,
) -> Result<ResolvedManifestEndpoint, ManifestError> {
    let name = entry.name.trim();
    if name.is_empty() {
        return Err(ManifestError {
            model_name: model_name.to_string(),
            message: "endpoint name must not be empty".to_string(),
        });
    }

    let current: EndpointConfig = existing.map(EndpointConfig::from).unwrap_or_default();
    let mut next = current.clone();
    next.name = name.to_string();

    if let Some(v) = &entry.api_base {
        next.api_base = v.clone();
    }
    if let Some(v) = &entry.api_key {
        next.api_key = if v.is_empty() { None } else { Some(v.clone()) };
    }
    // Clamp to what the table's CHECK constraints accept, so a hand-typed
    // `0` is corrected rather than turning into a 500 mid-import:
    // `model_endpoints_priority_check` is `>= 0`, `..._weight_check` is `>= 1`.
    if let Some(v) = entry.priority {
        next.priority = v.max(0);
    }
    if let Some(v) = entry.weight {
        next.weight = v.max(1);
    }
    if let Some(v) = entry.enabled {
        next.enabled = v;
    }
    if let Some(v) = entry.max_in_flight {
        crate::capacity::validate_per_replica_max_in_flight(Some(v)).map_err(|_| {
            ManifestError {
                model_name: model_name.to_string(),
                message: format!(
                    "endpoint '{name}': max_in_flight must be between 1 and {}",
                    crate::capacity::MAX_PER_REPLICA_MAX_IN_FLIGHT
                ),
            }
        })?;
        next.max_in_flight = Some(v);
    }

    // An endpoint is dispatched to, so it must have somewhere to dispatch. The
    // model row tolerates a blank `api_base` (Slurm-provisioned models get one
    // at promotion time); an endpoint row never does.
    if next.api_base.trim().is_empty() {
        return Err(ManifestError {
            model_name: model_name.to_string(),
            message: format!("endpoint '{name}' needs an api_base"),
        });
    }

    let mut changed_fields: Vec<String> = Vec::new();
    if current.api_base != next.api_base {
        changed_fields.push(format!("endpoints.{name}.api_base"));
    }
    if current.api_key != next.api_key {
        changed_fields.push(format!("endpoints.{name}.api_key"));
    }
    if current.priority != next.priority {
        changed_fields.push(format!("endpoints.{name}.priority"));
    }
    if current.weight != next.weight {
        changed_fields.push(format!("endpoints.{name}.weight"));
    }
    if current.enabled != next.enabled {
        changed_fields.push(format!("endpoints.{name}.enabled"));
    }
    if current.max_in_flight != next.max_in_flight {
        changed_fields.push(format!("endpoints.{name}.max_in_flight"));
    }

    Ok(ResolvedManifestEndpoint {
        config: next,
        changed_fields,
        is_new: existing.is_none(),
    })
}

/// Render an endpoint as a manifest entry, with the credential replaced by a
/// presence flag.
pub fn endpoint_to_manifest_entry(e: &ModelEndpoint) -> ManifestEndpoint {
    ManifestEndpoint {
        name: e.name.clone(),
        api_base: Some(e.api_base.clone()),
        api_key: None,
        has_api_key: Some(e.api_key.as_deref().is_some_and(|k| !k.is_empty())),
        priority: Some(e.priority),
        weight: Some(e.weight),
        enabled: Some(e.enabled),
        max_in_flight: e.max_in_flight,
    }
}

fn reject_negative(v: f64, field: &str) -> Result<f64, String> {
    if !v.is_finite() || v < 0.0 {
        Err(format!("{field} must be a finite number >= 0 (got {v})"))
    } else {
        Ok(v)
    }
}

/// Render a model as a manifest entry: every field populated, secrets replaced
/// by `has_api_key` and `upstream_header_names`, endpoints attached by the
/// caller.
pub fn model_to_manifest_entry(m: &ModelRoute) -> ManifestModel {
    ManifestModel {
        model_name: m.model_name.clone(),
        description: Some(m.description.clone()),
        aliases: Some(m.aliases.clone()),
        upstream_model: Some(m.upstream_model.clone()),
        api_base: Some(m.api_base.clone()),
        api_key: None,
        has_api_key: Some(m.api_key.as_deref().is_some_and(|k| !k.is_empty())),
        upstream_headers: None,
        upstream_header_names: Some(upstream_header_names(&m.upstream_headers)),
        model_type: Some(m.model_type.clone()),
        quantization: Some(m.quantization.clone()),
        input_cost_per_token: Some(m.input_cost_per_token),
        output_cost_per_token: Some(m.output_cost_per_token),
        cost_per_image: Some(m.cost_per_image),
        cost_per_audio_second: Some(m.cost_per_audio_second),
        cost_per_character: Some(m.cost_per_character),
        cost_per_video: Some(m.cost_per_video),
        context_window: Some(m.context_window),
        admission_weight: Some(m.admission_weight),
        max_in_flight: m.max_in_flight,
        capacity_mode: Some(m.capacity_mode.clone()),
        capacity_source: Some(m.capacity_source.clone()),
        capacity_namespace: m.capacity_namespace.clone(),
        capacity_service: m.capacity_service.clone(),
        per_replica_max_in_flight: m.per_replica_max_in_flight,
        capacity_headroom: Some(m.capacity_headroom),
        supports_function_calling: Some(m.supports_function_calling),
        supports_system_messages: Some(m.supports_system_messages),
        supports_response_schema: Some(m.supports_response_schema),
        supports_tool_choice: Some(m.supports_tool_choice),
        supports_vision: Some(m.supports_vision),
        enabled: Some(m.enabled),
        cache_enabled: Some(m.cache_enabled),
        cache_ttl_secs: Some(m.cache_ttl_secs),
        tags: Some(m.tags.clone()),
        boons: Some(m.boons.clone()),
        tool_servers: Some(m.tool_servers.clone()),
        request_timeout_secs: m.request_timeout_secs,
        max_retries: Some(m.max_retries),
        retry_backoff_ms: Some(m.retry_backoff_ms),
        endpoint_selection_mode: Some(m.endpoint_selection_mode.clone()),
        debug_diagnostics: Some(m.debug_diagnostics),
        energy_slots_per_node: Some(m.energy_slots_per_node),
        route_bias: Some(m.route_bias),
        auto_eligible: Some(m.auto_eligible),
        draft_model: Some(m.draft_model.clone()),
        verify_api_base: Some(m.verify_api_base.clone()),
        verify_upstream_model: Some(m.verify_upstream_model.clone()),
        endpoints: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn route(name: &str) -> ModelRoute {
        let now = chrono::Utc::now();
        ModelRoute {
            id: uuid::Uuid::new_v4(),
            model_name: name.to_string(),
            aliases: Vec::new(),
            description: "original".into(),
            upstream_model: "upstream/original".into(),
            api_base: "http://127.0.0.1:8000/v1".into(),
            api_key: Some("sk-original".into()),
            upstream_headers: Default::default(),
            model_type: "chat".into(),
            quantization: "unknown".into(),
            input_cost_per_token: 1.0,
            output_cost_per_token: 2.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            cost_per_video: 0.0,
            context_window: 4096,
            admission_weight: 50,
            max_in_flight: Some(4),
            capacity_mode: "static".into(),
            capacity_tuned_at: None,
            capacity_source: "endpoints".into(),
            capacity_namespace: None,
            capacity_service: None,
            per_replica_max_in_flight: None,
            capacity_headroom: 1.0,
            supports_function_calling: true,
            supports_system_messages: true,
            supports_response_schema: false,
            supports_tool_choice: false,
            supports_vision: false,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 300,
            tags: vec!["coding".into()],
            boons: vec![],
            tool_servers: vec![],
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: DEFAULT_RETRY_BACKOFF_MS,
            endpoint_selection_mode: "failover".into(),
            debug_diagnostics: false,
            energy_slots_per_node: 8,
            route_bias: 1.0,
            auto_eligible: true,
            draft_model: String::new(),
            verify_api_base: String::new(),
            verify_upstream_model: String::new(),
            created_at: now,
            updated_at: now,
        }
    }

    fn entry(name: &str) -> ManifestModel {
        ManifestModel {
            model_name: name.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn omitted_fields_are_left_untouched() {
        let existing = route("m");
        let mut e = entry("m");
        e.tags = Some(vec!["general".into()]);

        let r = resolve_model(&e, Some(&existing), &[]).unwrap();

        assert_eq!(r.changed_fields, vec!["tags"]);
        assert_eq!(r.action(), IMPORT_ACTION_UPDATED);
        // Everything the manifest did not mention survives verbatim.
        assert_eq!(r.config.description, "original");
        assert_eq!(r.config.input_cost_per_token, 1.0);
        assert_eq!(r.config.max_in_flight, Some(4));
        assert_eq!(r.config.energy_slots_per_node, 8);
        assert_eq!(r.config.api_key.as_deref(), Some("sk-original"));
    }

    #[test]
    fn an_entry_that_changes_nothing_reports_unchanged() {
        let existing = route("m");
        let mut e = entry("m");
        e.description = Some("original".into());
        e.tags = Some(vec!["coding".into()]);

        let r = resolve_model(&e, Some(&existing), &[]).unwrap();

        assert!(r.changed_fields.is_empty(), "{:?}", r.changed_fields);
        assert_eq!(r.action(), IMPORT_ACTION_UNCHANGED);
    }

    #[test]
    fn a_full_export_round_trips_to_unchanged() {
        let existing = route("m");
        let mut e = model_to_manifest_entry(&existing);
        // Export never emits the key; re-importing must not clear it.
        assert!(e.api_key.is_none());
        assert_eq!(e.has_api_key, Some(true));
        e.endpoints = None;

        let r = resolve_model(&e, Some(&existing), &[]).unwrap();

        assert!(
            r.changed_fields.is_empty(),
            "export->import should be a no-op, changed: {:?}",
            r.changed_fields
        );
        assert_eq!(r.config.api_key.as_deref(), Some("sk-original"));
    }

    #[test]
    fn a_missing_model_is_created_with_the_create_form_defaults() {
        let mut e = entry("brand-new");
        e.upstream_model = Some("vendor/model".into());

        let r = resolve_model(&e, None, &[]).unwrap();

        assert!(r.is_new);
        assert_eq!(r.action(), IMPORT_ACTION_CREATED);
        assert_eq!(r.config.context_window, 8192);
        assert_eq!(r.config.admission_weight, 100);
        assert!(r.config.supports_system_messages);
        assert!(r.config.enabled);
        assert_eq!(r.config.route_bias, 1.0);
    }

    #[test]
    fn unknown_tags_are_dropped_and_warned_about_not_stored() {
        let existing = route("m");
        let mut e = entry("m");
        // "code" is a plausible typo for the real tag "coding".
        e.tags = Some(vec!["code".into(), "general".into()]);

        let r = resolve_model(&e, Some(&existing), &[]).unwrap();

        assert_eq!(r.config.tags, vec!["general".to_string()]);
        assert_eq!(r.warnings.len(), 1, "{:?}", r.warnings);
        assert!(r.warnings[0].contains("code"), "{:?}", r.warnings);
    }

    #[test]
    fn tag_strength_suffixes_survive_a_round_trip() {
        let existing = route("m");
        let mut e = entry("m");
        e.tags = Some(vec!["coding:3".into(), "general".into()]);

        let r = resolve_model(&e, Some(&existing), &[]).unwrap();

        assert_eq!(
            r.config.tags,
            vec!["coding:3".to_string(), "general".to_string()]
        );
        assert!(r.warnings.is_empty(), "{:?}", r.warnings);
    }

    #[test]
    fn unknown_boons_are_dropped_and_warned_about() {
        let existing = route("m");
        let mut e = entry("m");
        e.boons = Some(vec!["vision".into(), "teleportation".into()]);

        let r = resolve_model(&e, Some(&existing), &[]).unwrap();

        assert_eq!(r.config.boons, vec!["vision".to_string()]);
        assert!(
            r.warnings.iter().any(|w| w.contains("teleportation")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn unregistered_tool_servers_are_kept_but_warned_about() {
        let existing = route("m");
        let mut e = entry("m");
        e.tool_servers = Some(vec!["files".into(), "ghost".into()]);

        let r = resolve_model(&e, Some(&existing), &["files".to_string()]).unwrap();

        assert_eq!(r.config.tool_servers, vec!["files", "ghost"]);
        assert!(
            r.warnings.iter().any(|w| w.contains("ghost")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn an_unknown_model_type_is_rejected_rather_than_silently_defaulting() {
        let existing = route("m");
        let mut e = entry("m");
        e.model_type = Some("embeddings".into()); // real value is "embedding"

        let err = resolve_model(&e, Some(&existing), &[]).unwrap_err();

        assert_eq!(err.model_name, "m");
        assert!(err.message.contains("embeddings"), "{}", err.message);
        assert!(err.message.contains("embedding"), "{}", err.message);
    }

    #[test]
    fn an_unknown_quantization_is_rejected_rather_than_silently_defaulting() {
        let existing = route("m");
        let mut e = entry("m");
        e.quantization = Some("fp-8".into()); // real value is "fp8"

        let err = resolve_model(&e, Some(&existing), &[]).unwrap_err();

        assert_eq!(err.model_name, "m");
        assert!(err.message.contains("fp-8"), "{}", err.message);
        // The vocabulary is listed, so the fix is in the error.
        assert!(err.message.contains("mxfp4"), "{}", err.message);
    }

    #[test]
    fn aliases_replace_the_whole_list_and_report_what_was_dropped() {
        let mut existing = route("m");
        existing.aliases = vec!["m-fp8".into(), "m-old".into()];
        let mut e = entry("m");
        // Omitting a stored alias is how one is removed; blanks and duplicates
        // are dropped with a warning rather than failing the import.
        e.aliases = Some(vec!["m-fp8".into(), "".into(), "m-fp8".into()]);

        let r = resolve_model(&e, Some(&existing), &[]).unwrap();

        assert_eq!(r.config.aliases, vec!["m-fp8".to_string()]);
        assert!(r.changed_fields.contains(&"aliases".to_string()));
        assert!(
            r.warnings.iter().any(|w| w.contains("dropped 2")),
            "{:?}",
            r.warnings
        );
    }

    #[test]
    fn an_absent_alias_list_leaves_the_stored_aliases_alone() {
        let mut existing = route("m");
        existing.aliases = vec!["m-fp8".into()];
        existing.quantization = "fp8".into();
        let e = entry("m");

        let r = resolve_model(&e, Some(&existing), &[]).unwrap();

        assert_eq!(r.config.aliases, vec!["m-fp8".to_string()]);
        assert_eq!(r.config.quantization, "fp8");
        assert!(!r.changed_fields.contains(&"aliases".to_string()));
        assert!(!r.changed_fields.contains(&"quantization".to_string()));
    }

    #[test]
    fn unknown_capacity_and_selection_modes_are_rejected() {
        let existing = route("m");
        let mut e = entry("m");
        e.capacity_mode = Some("automatic".into());
        assert!(resolve_model(&e, Some(&existing), &[]).is_err());

        let mut e = entry("m");
        e.endpoint_selection_mode = Some("round_robin".into());
        assert!(resolve_model(&e, Some(&existing), &[]).is_err());
    }

    #[test]
    fn negative_prices_are_rejected_with_the_field_named() {
        let existing = route("m");
        let mut e = entry("m");
        e.input_cost_per_token = Some(-1.0);

        let err = resolve_model(&e, Some(&existing), &[]).unwrap_err();

        assert!(
            err.message.contains("input_cost_per_token"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_video_model_imports_with_its_flat_price() {
        let mut e = entry("video-model");
        e.model_type = Some("video".into());
        e.cost_per_video = Some(0.5);

        let r = resolve_model(&e, None, &[]).unwrap();

        assert_eq!(r.config.model_type, "video");
        assert_eq!(r.config.cost_per_video, 0.5);

        let existing = route("video-model");
        let r = resolve_model(&e, Some(&existing), &[]).unwrap();
        assert!(r.changed_fields.contains(&"cost_per_video".to_string()));

        let mut e = entry("video-model");
        e.cost_per_video = Some(-0.5);
        let err = resolve_model(&e, Some(&existing), &[]).unwrap_err();
        assert!(err.message.contains("cost_per_video"), "{}", err.message);
    }

    #[test]
    fn a_discovered_model_carries_its_capacity_source() {
        let mut e = entry("m");
        e.capacity_mode = Some("discovered".into());
        e.capacity_source = Some(" Kubernetes ".into());
        e.capacity_namespace = Some(" inference ".into());
        e.capacity_service = Some("m-serve".into());
        e.per_replica_max_in_flight = Some(8);
        e.capacity_headroom = Some(1.25);

        let r = resolve_model(&e, None, &[]).unwrap();
        assert_eq!(r.config.capacity_mode, "discovered");
        assert_eq!(r.config.capacity_source, "kubernetes");
        assert_eq!(r.config.capacity_namespace.as_deref(), Some("inference"));
        assert_eq!(r.config.capacity_service.as_deref(), Some("m-serve"));
        assert_eq!(r.config.per_replica_max_in_flight, Some(8));
        assert_eq!(r.config.capacity_headroom, 1.25);

        let existing = route("m");
        let r = resolve_model(&e, Some(&existing), &[]).unwrap();
        for field in [
            "capacity_mode",
            "capacity_source",
            "capacity_namespace",
            "capacity_service",
            "per_replica_max_in_flight",
            "capacity_headroom",
        ] {
            assert!(r.changed_fields.contains(&field.to_string()), "{field}");
        }

        // An empty string clears a text field.
        let mut stored = route("m");
        stored.capacity_namespace = Some("inference".into());
        let mut clear = entry("m");
        clear.capacity_namespace = Some(String::new());
        let r = resolve_model(&clear, Some(&stored), &[]).unwrap();
        assert_eq!(r.config.capacity_namespace, None);

        // Export and re-import is a no-op.
        let mut stored = route("m");
        stored.capacity_mode = "discovered".into();
        stored.capacity_source = "kubernetes".into();
        stored.capacity_service = Some("m".into());
        stored.per_replica_max_in_flight = Some(4);
        stored.capacity_headroom = 1.5;
        let exported = model_to_manifest_entry(&stored);
        let r = resolve_model(&exported, Some(&stored), &[]).unwrap();
        assert!(r.changed_fields.is_empty(), "{:?}", r.changed_fields);
    }

    #[test]
    fn bad_capacity_fields_are_refused_with_the_field_named() {
        for (field, apply) in [
            (
                "capacity_source",
                (|e: &mut ManifestModel| e.capacity_source = Some("prometheus".into()))
                    as fn(&mut ManifestModel),
            ),
            ("capacity_namespace", |e| {
                e.capacity_namespace = Some("Not_A_Namespace".into())
            }),
            ("capacity_service", |e| {
                e.capacity_service = Some("app=m".into())
            }),
            ("per_replica_max_in_flight", |e| {
                e.per_replica_max_in_flight = Some(0)
            }),
            ("capacity_headroom", |e| e.capacity_headroom = Some(0.0)),
        ] {
            let mut e = entry("m");
            apply(&mut e);
            let err = resolve_model(&e, None, &[]).expect_err(field);
            assert!(err.message.contains(field), "{field}: {}", err.message);
        }
    }

    #[test]
    fn an_endpoint_carries_its_own_concurrency() {
        let mut e = ManifestEndpoint {
            name: "a".into(),
            api_base: Some("http://a/v1".into()),
            max_in_flight: Some(16),
            ..Default::default()
        };
        let r = resolve_endpoint(&e, None, "m").unwrap();
        assert_eq!(r.config.max_in_flight, Some(16));
        e.max_in_flight = Some(0);
        let err = resolve_endpoint(&e, None, "m").unwrap_err();
        assert!(err.message.contains("max_in_flight"), "{}", err.message);
    }

    #[test]
    fn an_empty_model_name_is_rejected() {
        let e = entry("   ");
        assert!(resolve_model(&e, None, &[]).is_err());
    }

    #[test]
    fn an_empty_api_key_string_clears_the_stored_credential() {
        let existing = route("m");
        let mut e = entry("m");
        e.api_key = Some(String::new());

        let r = resolve_model(&e, Some(&existing), &[]).unwrap();

        assert_eq!(r.config.api_key, None);
        assert_eq!(r.changed_fields, vec!["api_key"]);
    }

    #[test]
    fn has_api_key_is_ignored_on_import() {
        let existing = route("m");
        let mut e = entry("m");
        e.has_api_key = Some(false); // lying about it must not clear the key

        let r = resolve_model(&e, Some(&existing), &[]).unwrap();

        assert_eq!(r.config.api_key.as_deref(), Some("sk-original"));
        assert!(r.changed_fields.is_empty());
    }

    #[test]
    fn upstream_headers_export_as_names_and_import_as_a_write() {
        let mut existing = route("m");
        existing.upstream_headers = [
            ("x-routing-hint".to_string(), "sticky".to_string()),
            ("x-upstream-token".to_string(), "secret".to_string()),
        ]
        .into();

        let exported = model_to_manifest_entry(&existing);
        assert!(exported.upstream_headers.is_none(), "values never leave");
        assert_eq!(
            exported.upstream_header_names,
            Some(vec!["x-routing-hint".into(), "x-upstream-token".into()])
        );
        // Re-importing the export changes nothing: the names are export-only.
        let r = resolve_model(&exported, Some(&existing), &[]).unwrap();
        assert!(!r.changed_fields.contains(&"upstream_headers".to_string()));
        assert_eq!(r.config.upstream_headers, existing.upstream_headers);

        // A write keeps a `null` value, sets a string, and drops what is left out.
        let mut e = entry("m");
        e.upstream_headers = Some(
            [
                ("X-Upstream-Token".to_string(), None),
                ("x-routing-hint".to_string(), Some("least-busy".into())),
            ]
            .into(),
        );
        let r = resolve_model(&e, Some(&existing), &[]).unwrap();
        assert_eq!(r.changed_fields, vec!["upstream_headers"]);
        assert_eq!(r.config.upstream_headers["x-upstream-token"], "secret");
        assert_eq!(r.config.upstream_headers["x-routing-hint"], "least-busy");

        let mut e = entry("m");
        e.upstream_headers = Some([("Authorization".to_string(), Some("Bearer x".into()))].into());
        let err = resolve_model(&e, Some(&existing), &[]).unwrap_err();
        assert!(err.message.contains("authorization"), "{}", err.message);
    }

    /// Every numeric field the `models` table constrains must be clamped into
    /// range here, or the import fails at the database with a 500 instead of
    /// quietly correcting an obvious typo.
    #[test]
    fn out_of_range_numbers_are_clamped_to_what_the_check_constraints_accept() {
        let existing = route("m");
        let mut e = entry("m");
        e.admission_weight = Some(0); // CHECK >= 1
        e.context_window = Some(-5); // CHECK >= 0
        e.max_in_flight = Some(0); // CHECK null or >= 1
        e.cache_ttl_secs = Some(-1); // CHECK >= 0
        e.max_retries = Some(-3); // CHECK >= 0
        e.retry_backoff_ms = Some(-1); // CHECK >= 0
        e.request_timeout_secs = Some(0); // CHECK null or >= 1
        e.energy_slots_per_node = Some(-2);

        let r = resolve_model(&e, Some(&existing), &[]).unwrap();

        assert_eq!(r.config.admission_weight, 1);
        assert_eq!(r.config.context_window, 0);
        assert_eq!(r.config.max_in_flight, Some(1));
        assert_eq!(r.config.cache_ttl_secs, 0);
        assert_eq!(r.config.max_retries, 0);
        assert_eq!(r.config.retry_backoff_ms, 0);
        assert_eq!(r.config.request_timeout_secs, Some(1));
        assert_eq!(r.config.energy_slots_per_node, 0);
    }

    /// `model_endpoints` constrains `priority >= 0` and `weight >= 1`.
    #[test]
    fn endpoint_priority_and_weight_are_clamped_into_range() {
        let e = ManifestEndpoint {
            name: "primary".into(),
            api_base: Some("http://127.0.0.1:9000/v1".into()),
            priority: Some(-10),
            weight: Some(0),
            ..Default::default()
        };

        let r = resolve_endpoint(&e, None, "m").unwrap();

        assert_eq!(r.config.priority, 0);
        assert_eq!(r.config.weight, 1);
    }

    #[test]
    fn an_endpoint_without_an_api_base_is_rejected() {
        let e = ManifestEndpoint {
            name: "primary".into(),
            ..Default::default()
        };
        let err = resolve_endpoint(&e, None, "m").unwrap_err();
        assert!(err.message.contains("api_base"), "{}", err.message);
    }

    #[test]
    fn a_sparse_manifest_deserializes_with_everything_else_absent() {
        let json = r#"{
            "format": "obleth-models",
            "version": 1,
            "models": [{ "model_name": "llama", "tags": ["coding"] }]
        }"#;
        let m: ModelManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.models.len(), 1);
        assert_eq!(m.models[0].tags, Some(vec!["coding".to_string()]));
        assert!(m.models[0].description.is_none());
        assert!(m.models[0].input_cost_per_token.is_none());
    }
}
