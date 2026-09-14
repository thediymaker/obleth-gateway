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
//!   `OBLETH_ENCRYPTION_KEY`s.
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
    is_valid_capacity_mode, is_valid_endpoint_selection_mode, is_valid_model_type, normalize_boons,
    normalize_tag_levels, normalize_tool_servers, parse_tag_level, ModelEndpoint, ModelRoute,
    CAPACITY_MODES, DEFAULT_CAPACITY_MODE, DEFAULT_ENDPOINT_SELECTION_MODE, DEFAULT_MODEL_TYPE,
    DEFAULT_RETRY_BACKOFF_MS, ENDPOINT_SELECTION_MODES, MODEL_TYPES,
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
/// Nullable columns (`max_in_flight`, `request_timeout_secs`) cannot be
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_type: Option<String>,
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
    pub context_window: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_weight: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_in_flight: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capacity_mode: Option<String>,
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
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub model_type: String,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub cost_per_image: f64,
    pub cost_per_audio_second: f64,
    pub cost_per_character: f64,
    pub context_window: i64,
    pub admission_weight: i64,
    pub max_in_flight: Option<i64>,
    pub capacity_mode: String,
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
}

impl Default for ModelConfig {
    /// The shape of a model created from a manifest that sets nothing but a
    /// name. Mirrors the admin create form's defaults.
    fn default() -> Self {
        Self {
            description: String::new(),
            upstream_model: String::new(),
            api_base: String::new(),
            api_key: None,
            model_type: DEFAULT_MODEL_TYPE.to_string(),
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            context_window: DEFAULT_CONTEXT_WINDOW,
            admission_weight: DEFAULT_ADMISSION_WEIGHT,
            max_in_flight: None,
            capacity_mode: DEFAULT_CAPACITY_MODE.to_string(),
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
        }
    }
}

impl From<&ModelRoute> for ModelConfig {
    fn from(m: &ModelRoute) -> Self {
        Self {
            description: m.description.clone(),
            upstream_model: m.upstream_model.clone(),
            api_base: m.api_base.clone(),
            api_key: m.api_key.clone(),
            model_type: m.model_type.clone(),
            input_cost_per_token: m.input_cost_per_token,
            output_cost_per_token: m.output_cost_per_token,
            cost_per_image: m.cost_per_image,
            cost_per_audio_second: m.cost_per_audio_second,
            cost_per_character: m.cost_per_character,
            context_window: m.context_window,
            admission_weight: m.admission_weight,
            max_in_flight: m.max_in_flight,
            capacity_mode: m.capacity_mode.clone(),
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
        note(
            self.upstream_model != other.upstream_model,
            "upstream_model",
        );
        note(self.api_base != other.api_base, "api_base");
        note(self.api_key != other.api_key, "api_key");
        note(self.model_type != other.model_type, "model_type");
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

    if let Some(raw) = &entry.tags {
        // Store the suffixed form so a declared strength level survives, the
        // same shape `serialize_tag_levels` writes on every other path.
        next.tags = normalize_tag_levels(raw)
            .into_iter()
            .map(|(base, level)| {
                if level == 1 {
                    base
                } else {
                    format!("{base}:{level}")
                }
            })
            .collect();
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
/// by `has_api_key`, endpoints attached by the caller.
pub fn model_to_manifest_entry(m: &ModelRoute) -> ManifestModel {
    ManifestModel {
        model_name: m.model_name.clone(),
        description: Some(m.description.clone()),
        upstream_model: Some(m.upstream_model.clone()),
        api_base: Some(m.api_base.clone()),
        api_key: None,
        has_api_key: Some(m.api_key.as_deref().is_some_and(|k| !k.is_empty())),
        model_type: Some(m.model_type.clone()),
        input_cost_per_token: Some(m.input_cost_per_token),
        output_cost_per_token: Some(m.output_cost_per_token),
        cost_per_image: Some(m.cost_per_image),
        cost_per_audio_second: Some(m.cost_per_audio_second),
        cost_per_character: Some(m.cost_per_character),
        context_window: Some(m.context_window),
        admission_weight: Some(m.admission_weight),
        max_in_flight: m.max_in_flight,
        capacity_mode: Some(m.capacity_mode.clone()),
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
            description: "original".into(),
            upstream_model: "upstream/original".into(),
            api_base: "http://127.0.0.1:8000/v1".into(),
            api_key: Some("sk-original".into()),
            model_type: "chat".into(),
            input_cost_per_token: 1.0,
            output_cost_per_token: 2.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            context_window: 4096,
            admission_weight: 50,
            max_in_flight: Some(4),
            capacity_mode: "static".into(),
            capacity_tuned_at: None,
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
