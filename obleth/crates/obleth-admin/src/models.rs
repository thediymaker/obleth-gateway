//! Models: request bodies, validation, handlers, families and status.

use super::*;

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateModel {
    pub model_name: String,
    pub description: Option<String>,
    /// Extra client-facing names this model answers to, so a name can be
    /// cleaned up (`glm-5-3-fp8` -> `glm-5-3`) without breaking pinned
    /// clients. Each must be unused by any other model's name or aliases.
    #[serde(default)]
    pub aliases: Option<Vec<String>>,
    /// Opt-in names for this same model with extra boons on, e.g.
    /// `glm-5-3-spec` with `speculation`. Each name must be unused by any
    /// model's name, alias or variant; at most 8.
    #[serde(default)]
    pub variants: Option<Vec<ModelVariantWrite>>,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    /// Extra headers sent on every request to this model's upstream, after the
    /// client's forwarded headers so these win (e.g. a routing hint an
    /// inference gateway reads, or a tenant or organization header a provider
    /// requires). Names are case-insensitive. Hop-by-hop headers,
    /// `authorization` (use `api_key`), `host`, `content-length`,
    /// `content-type`, and `accept-encoding` are refused. Values are
    /// write-only: responses list only `upstream_header_names`.
    #[serde(default)]
    #[schema(value_type = Option<std::collections::BTreeMap<String, String>>)]
    pub upstream_headers: Option<obleth_config::UpstreamHeadersWrite>,
    #[serde(default)]
    pub model_type: Option<String>,
    /// Serving format from the fixed `QUANTIZATIONS` vocabulary. Omitted means
    /// `unknown` — undeclared, not "full precision".
    #[serde(default)]
    pub quantization: Option<String>,
    pub input_cost_per_token: Option<f64>,
    pub output_cost_per_token: Option<f64>,
    #[serde(default)]
    pub cost_per_image: Option<f64>,
    #[serde(default)]
    pub cost_per_audio_second: Option<f64>,
    #[serde(default)]
    pub cost_per_character: Option<f64>,
    /// Flat USD price of one created job (`video` models).
    #[serde(default)]
    pub cost_per_video: Option<f64>,
    pub context_window: Option<i64>,
    pub admission_weight: Option<i64>,
    pub max_in_flight: Option<i64>,
    /// `static` (default), `tuned` or `discovered`. In `discovered` mode the
    /// pool size follows the live backend and `max_in_flight` is the fallback.
    #[serde(default)]
    pub capacity_mode: Option<String>,
    /// `discovered` mode: `endpoints` (default) or `kubernetes`.
    #[serde(default)]
    pub capacity_source: Option<String>,
    /// `kubernetes` source: namespace of the Service. Omitted looks it up in
    /// the gateway's `OBLETH_CAPACITY_DISCOVERY_NAMESPACES`, in order; the
    /// first namespace that has it wins.
    #[serde(default)]
    pub capacity_namespace: Option<String>,
    /// `kubernetes` source: name of the Service whose ready endpoints count
    /// the serving replicas. Omitted uses the gateway's
    /// `OBLETH_CAPACITY_DEFAULT_SERVICE` template.
    #[serde(default)]
    pub capacity_service: Option<String>,
    /// Requests one serving replica takes (e.g. the server's max concurrent
    /// sequences). Required in `discovered` mode on the `kubernetes` source,
    /// and on `endpoints` unless every endpoint sets its own `max_in_flight`.
    #[serde(default)]
    pub per_replica_max_in_flight: Option<i64>,
    /// Multiplier on the derived pool size (default 1.0).
    #[serde(default)]
    pub capacity_headroom: Option<f64>,
    pub supports_function_calling: Option<bool>,
    pub supports_system_messages: Option<bool>,
    pub supports_response_schema: Option<bool>,
    pub supports_tool_choice: Option<bool>,
    #[serde(default)]
    pub supports_vision: Option<bool>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub boons: Option<Vec<String>>,
    /// Registered MCP servers whose tools this model may use (gateway tool loop).
    #[serde(default)]
    pub tool_servers: Option<Vec<String>>,
    /// Energy accounting: concurrent sequences that saturate one node.
    /// 0 disables energy accounting for this model.
    #[serde(default)]
    pub energy_slots_per_node: Option<i64>,
    /// Per-model multiplier on the `auto` router's final score. `1.0` is
    /// neutral; the router clamps to `[0.1, 3.0]` before applying it.
    #[serde(default)]
    pub route_bias: Option<f64>,
    /// Whether the `auto` router may select this model. `false` keeps the model
    /// addressable by name but removes it from auto's candidate pool. Omitted
    /// means eligible.
    #[serde(default)]
    pub auto_eligible: Option<bool>,
    /// This model's own speculation drafter. Empty or omitted = fleet default.
    #[serde(default)]
    pub draft_model: Option<String>,
    /// Direct URL of a deployment of this model that scores its drafts
    /// (`prompt_logprobs`). Empty or omitted = the model cannot speculate.
    #[serde(default)]
    pub verify_api_base: Option<String>,
    /// Name that scoring backend serves, if not this model's `upstream_model`.
    #[serde(default)]
    pub verify_upstream_model: Option<String>,
    /// `false` creates the route switched off, so it serves nothing until it is
    /// turned on — for example by `POST /models/{id}/health/activate` once a
    /// health check passes. Omitted = on.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// `staged` creates the model served by name only: not listed on the
    /// discovery endpoints and never picked by `auto`, until
    /// `PUT /models/{id}/status` makes it `active`. Omitted = `active`; a new
    /// model cannot start deprecated or retired.
    #[serde(default)]
    pub status: Option<ModelStatus>,
    /// Top-level request body fields this model's backend cannot handle. A
    /// request carrying any of them (also inside `extra_body`) is refused
    /// with 400 before it is sent. Case-sensitive; at most 32; `model` cannot
    /// be one. Omitted = none.
    #[serde(default)]
    pub rejected_request_fields: Option<Vec<String>>,
    /// The model's own session affinity setting. Omitted = follows the
    /// gateway.
    #[serde(default)]
    pub session_affinity: Option<ModelSessionAffinity>,
}

/// One variant in a model write: a name that resolves to the model with extra
/// boons on.
#[derive(Debug, Clone, Deserialize, ToSchema)]
pub struct ModelVariantWrite {
    /// The name clients pass in `model`. Trimmed; case-sensitive.
    pub name: String,
    /// What a caller gets from this name, shown in the model listing.
    #[serde(default)]
    pub description: Option<String>,
    /// Boons from the fixed `MODEL_BOONS` vocabulary turned on in addition to
    /// the model's own. An unknown name is rejected.
    #[serde(default)]
    pub boons: Vec<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateModel {
    pub description: Option<String>,
    /// Replaces the alias list wholesale; omitted leaves it unchanged. An
    /// empty list removes every alias, which also evicts their resolver keys.
    #[serde(default)]
    pub aliases: Option<Vec<String>>,
    /// Replaces the variant list wholesale; omitted leaves it unchanged. An
    /// empty list removes every variant, which also evicts their resolver keys.
    #[serde(default)]
    pub variants: Option<Vec<ModelVariantWrite>>,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    /// Replaces the model's upstream headers; omitted leaves them unchanged
    /// and `{}` removes them all. A `null` value keeps the value stored under
    /// that name, so a client that only knows the names (values are
    /// write-only) can add or drop one header without re-sending the others.
    #[serde(default)]
    #[schema(value_type = Option<std::collections::BTreeMap<String, Option<String>>>)]
    pub upstream_headers: Option<obleth_config::UpstreamHeadersWrite>,
    #[serde(default)]
    pub model_type: Option<String>,
    /// Serving format from the fixed `QUANTIZATIONS` vocabulary; omitted
    /// leaves the current value unchanged.
    #[serde(default)]
    pub quantization: Option<String>,
    pub input_cost_per_token: Option<f64>,
    pub output_cost_per_token: Option<f64>,
    #[serde(default)]
    pub cost_per_image: Option<f64>,
    #[serde(default)]
    pub cost_per_audio_second: Option<f64>,
    #[serde(default)]
    pub cost_per_character: Option<f64>,
    /// Flat USD price of one created job (`video` models).
    #[serde(default)]
    pub cost_per_video: Option<f64>,
    pub context_window: Option<i64>,
    pub admission_weight: Option<i64>,
    /// The model-wide in-flight cap; `null` lifts it.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub max_in_flight: Option<Option<i64>>,
    /// `static`, `tuned` or `discovered`; omitted leaves it unchanged.
    #[serde(default)]
    pub capacity_mode: Option<String>,
    /// `endpoints` or `kubernetes`; omitted leaves it unchanged.
    #[serde(default)]
    pub capacity_source: Option<String>,
    /// Omitted leaves it unchanged; `null` or `""` clears it.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub capacity_namespace: Option<Option<String>>,
    /// Omitted leaves it unchanged; `null` or `""` clears it.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub capacity_service: Option<Option<String>>,
    /// Omitted leaves it unchanged; `null` clears it.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub per_replica_max_in_flight: Option<Option<i64>>,
    /// Omitted leaves it unchanged.
    #[serde(default)]
    pub capacity_headroom: Option<f64>,
    pub supports_function_calling: Option<bool>,
    pub supports_system_messages: Option<bool>,
    pub supports_response_schema: Option<bool>,
    pub supports_tool_choice: Option<bool>,
    #[serde(default)]
    pub supports_vision: Option<bool>,
    pub enabled: Option<bool>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub boons: Option<Vec<String>>,
    /// Registered MCP servers whose tools this model may use (gateway tool loop).
    #[serde(default)]
    pub tool_servers: Option<Vec<String>>,
    /// Energy accounting: concurrent sequences that saturate one node.
    /// 0 disables energy accounting for this model.
    #[serde(default)]
    pub energy_slots_per_node: Option<i64>,
    /// Per-model multiplier on the `auto` router's final score. `1.0` is
    /// neutral; the router clamps to `[0.1, 3.0]` before applying it.
    #[serde(default)]
    pub route_bias: Option<f64>,
    /// Whether the `auto` router may select this model. `false` keeps the model
    /// addressable by name but removes it from auto's candidate pool. Omitted
    /// leaves the current value unchanged.
    #[serde(default)]
    pub auto_eligible: Option<bool>,
    /// This model's own speculation drafter. Empty clears it (fleet default);
    /// omitted leaves the current value unchanged.
    #[serde(default)]
    pub draft_model: Option<String>,
    /// Direct scoring URL for this model's drafts. Empty clears it (the model
    /// stops speculating); omitted leaves the current value unchanged.
    #[serde(default)]
    pub verify_api_base: Option<String>,
    /// Name the scoring backend serves, if not `upstream_model`. Empty clears.
    #[serde(default)]
    pub verify_upstream_model: Option<String>,
    /// Replaces the request fields the model refuses; omitted leaves them
    /// unchanged and `[]` removes them all.
    #[serde(default)]
    pub rejected_request_fields: Option<Vec<String>>,
    /// Replaces the model's session affinity setting; omitted leaves it
    /// unchanged and `{}` makes the model follow the gateway again.
    #[serde(default)]
    pub session_affinity: Option<ModelSessionAffinity>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelCache {
    pub cache_enabled: bool,
    /// Response-cache lifetime in seconds (default 300). 0 disables the cache:
    /// responses are not written.
    pub cache_ttl_secs: Option<i64>,
}

/// Body of `PUT /api/v1/models/{id}/reliability`. An omitted field keeps
/// its stored value.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct SetModelReliability {
    /// Per-request upstream timeout in seconds. `null` defers to the global
    /// default.
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub request_timeout_secs: Option<Option<i64>>,
    #[serde(default)]
    pub max_retries: Option<i64>,
    #[serde(default)]
    pub retry_backoff_ms: Option<i64>,
    #[serde(default)]
    pub endpoint_selection_mode: Option<String>,
    #[serde(default)]
    pub debug_diagnostics: Option<bool>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelCapacity {
    pub max_in_flight: Option<i64>,
}

/// A model's lifecycle, replaced whole: an omitted field is cleared, and
/// `staged` or `active` clears them all.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelStatus {
    /// `staged` (served by name, but not listed and never picked by `auto`),
    /// `active`, `deprecated` (still served, with Deprecation/Sunset headers)
    /// or `retired` (refused with 410 Gone, or served by the replacement when
    /// `redirect` is on).
    pub status: ModelStatus,
    /// `model_name` of a registered model of the same type to move callers to.
    #[serde(default)]
    pub replacement: Option<String>,
    /// When a deprecated model stops being served (RFC 3339). Once it passes,
    /// the model is retired without another write.
    #[serde(default)]
    pub retire_at: Option<chrono::DateTime<chrono::Utc>>,
    /// A sentence for callers, repeated with the status (at most 500
    /// characters).
    #[serde(default)]
    pub note: Option<String>,
    /// Once retired, serve `replacement` instead of refusing. Needs a
    /// replacement.
    #[serde(default)]
    pub redirect: Option<bool>,
}

/// The request fields a model refuses, written whole.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelRejectedRequestFields {
    /// Top-level request body fields the model's backend cannot handle. A
    /// request carrying any of them (also inside `extra_body`) is refused
    /// with 400 before it is sent. Case-sensitive; at most 32; `model` cannot
    /// be one. `[]` refuses nothing.
    pub rejected_request_fields: Vec<String>,
}

/// Whether the discovery endpoints advertise a model.
#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelListing {
    /// `false` keeps the model out of `/v1/models` and `/model/info`; it is
    /// still served by name.
    pub listed: bool,
}

/// A model family, written whole.
#[derive(Debug, Deserialize, ToSchema)]
pub struct ModelFamilyWrite {
    /// The name callers pass in `model`. Must not be any model's name, alias
    /// or variant, or another family's name.
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    /// The level a request that names none is served at.
    pub default_level: ReasoningLevel,
    /// The chat model that answers at each level, by name or alias (stored
    /// as its `model_name`).
    pub members: FamilyMembers,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelCapacityMode {
    pub capacity_mode: String,
    /// The `discovered`-mode fields, with the same rules as on
    /// `PUT /api/v1/models/{id}`: omitted leaves a field unchanged.
    #[serde(default)]
    pub capacity_source: Option<String>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub capacity_namespace: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<String>)]
    pub capacity_service: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    #[schema(value_type = Option<i64>)]
    pub per_replica_max_in_flight: Option<Option<i64>>,
    #[serde(default)]
    pub capacity_headroom: Option<f64>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ApplyAutotuneCapacity {
    pub max_in_flight: i64,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct SetModelWeight {
    pub admission_weight: i64,
}

/// The speculation boon sends the model's upstream key to its scoring endpoint,
/// so a non-empty `verify_api_base` gets the same destination policy as
/// `api_base`.
async fn validate_verify_api_base(state: &AdminState, raw: Option<&str>) -> Result<()> {
    match raw.map(str::trim) {
        Some(url) if !url.is_empty() => state
            .ssrf
            .validate(url)
            .await
            .map_err(|e| AdminError::BadRequest(format!("verify_api_base: {e}"))),
        _ => Ok(()),
    }
}

/// Validate a declared serving format against the fixed vocabulary.
///
/// Unlike most enum-ish fields, this one is *rejected* rather than normalized
/// to the default on the write path: `normalize_quantization` silently folds an
/// unknown value to `unknown`, and an operator who types `fp-8` in the model
/// form deserves to be told, not to have the field quietly emptied. The
/// normalizer still runs in the store, for values arriving from a backend's own
/// spelling (`FP8`, `w4a16-awq`) where folding is the point.
fn validate_quantization(raw: &str) -> Result<String> {
    let q = raw.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Ok(obleth_config::DEFAULT_QUANTIZATION.to_string());
    }
    if !obleth_config::is_valid_quantization(&q) {
        return Err(AdminError::BadRequest(format!(
            "unknown quantization '{raw}' (expected one of: {})",
            obleth_config::QUANTIZATIONS.join(", ")
        )));
    }
    Ok(q)
}

/// A fairshare weight from the admin API: at least 1. The store used to fold
/// 0 to 1 silently; an operator who sends 0 is told.
pub(crate) fn validate_weight(weight: i64) -> Result<i64> {
    if weight < 1 {
        return Err(AdminError::BadRequest("weight must be >= 1".into()));
    }
    Ok(weight)
}

/// A model type from the admin API: one of the fixed vocabulary, rejected
/// rather than folded to `chat`. The store's normaliser still runs for values
/// a backend reports in its own spelling.
fn validate_model_type(raw: &str) -> Result<String> {
    let t = raw.trim().to_ascii_lowercase();
    if !obleth_config::is_valid_model_type(&t) {
        return Err(AdminError::BadRequest(format!(
            "unknown model_type '{raw}' (expected one of: {})",
            obleth_config::MODEL_TYPES.join(", ")
        )));
    }
    Ok(t)
}

/// An endpoint selection mode from the admin API: one of the fixed
/// vocabulary, rejected rather than folded to the default.
fn validate_endpoint_selection_mode(raw: &str) -> Result<String> {
    let m = raw.trim().to_ascii_lowercase();
    if !obleth_config::is_valid_endpoint_selection_mode(&m) {
        return Err(AdminError::BadRequest(format!(
            "unknown endpoint_selection_mode '{raw}' (expected one of: {})",
            obleth_config::ENDPOINT_SELECTION_MODES.join(", ")
        )));
    }
    Ok(m)
}

/// A settings value that must lie in `range`, named by its field: a value
/// outside it is 400, never kept silently as the old value.
pub(crate) fn in_range<T: PartialOrd + Copy + std::fmt::Display>(
    field: &str,
    value: Option<T>,
    range: std::ops::RangeInclusive<T>,
    existing: T,
) -> Result<T> {
    match value {
        None => Ok(existing),
        Some(v) if range.contains(&v) => Ok(v),
        Some(v) => Err(AdminError::BadRequest(format!(
            "{field} must be between {} and {}, got {v}",
            range.start(),
            range.end()
        ))),
    }
}

/// A settings value that must be at least `min`, named by its field.
pub(crate) fn at_least<T: PartialOrd + Copy + std::fmt::Display>(
    field: &str,
    value: Option<T>,
    min: T,
    existing: T,
) -> Result<T> {
    match value {
        None => Ok(existing),
        Some(v) if v >= min => Ok(v),
        Some(v) => Err(AdminError::BadRequest(format!(
            "{field} must be at least {min}, got {v}"
        ))),
    }
}

/// Normalize an alias list and reject any name already claimed elsewhere.
///
/// A client sending `model: "x"` cannot tell whether `x` is a canonical name or
/// an alias, so `x` must identify exactly one route: an alias may not collide
/// with any model's `model_name`, with another model's alias, or with the
/// model's own name. `editing` is the row being written, whose own names are
/// not collisions with itself.
async fn validate_aliases(
    state: &AdminState,
    raw: &[String],
    editing: Option<Uuid>,
    model_name: &str,
) -> Result<Vec<String>> {
    let aliases = obleth_config::normalize_aliases(raw);
    for alias in &aliases {
        if alias == model_name {
            return Err(AdminError::BadRequest(format!(
                "alias '{alias}' is the model's own name"
            )));
        }
        if alias == obleth_config::routing::AUTO_MODEL_NAME {
            return Err(AdminError::BadRequest(format!(
                "alias '{alias}' is reserved for automatic model selection"
            )));
        }
        if let Some((owner_id, owner_name)) = state.store.model_name_owner(alias).await? {
            if Some(owner_id) != editing {
                return Err(AdminError::BadRequest(format!(
                    "alias '{alias}' is already taken by model '{owner_name}'"
                )));
            }
        }
        reject_family_name(state, alias, "alias").await?;
    }
    Ok(aliases)
}

/// The checks on a variant list that need no database: names present, unique
/// and not the model's own, boons known, the cap respected. Returns the list
/// in storage form. Stricter than [`obleth_config::normalize_variants`], which
/// drops what it cannot keep: an operator who typed a name or a boon gets told
/// why it was refused rather than finding it silently gone.
pub(crate) fn check_variants(
    raw: &[ModelVariantWrite],
    model_name: &str,
    aliases: &[String],
) -> Result<Vec<ModelVariant>> {
    if raw.len() > obleth_config::MAX_MODEL_VARIANTS {
        return Err(AdminError::BadRequest(format!(
            "a model can have at most {} variants",
            obleth_config::MAX_MODEL_VARIANTS
        )));
    }
    let mut out: Vec<ModelVariant> = Vec::with_capacity(raw.len());
    for v in raw {
        let name = v.name.trim();
        if name.is_empty() {
            return Err(AdminError::BadRequest("a variant needs a name".into()));
        }
        if name == model_name {
            return Err(AdminError::BadRequest(format!(
                "variant '{name}' is the model's own name"
            )));
        }
        if aliases.iter().any(|a| a == name) {
            return Err(AdminError::BadRequest(format!(
                "variant '{name}' is already one of the model's aliases"
            )));
        }
        if name == obleth_config::routing::AUTO_MODEL_NAME {
            return Err(AdminError::BadRequest(format!(
                "variant '{name}' is reserved for automatic model selection"
            )));
        }
        if out.iter().any(|o| o.name == name) {
            return Err(AdminError::BadRequest(format!(
                "variant '{name}' is listed twice"
            )));
        }
        if let Some(unknown) = v
            .boons
            .iter()
            .find(|b| !obleth_config::is_valid_boon(&b.trim().to_ascii_lowercase()))
        {
            return Err(AdminError::BadRequest(format!(
                "variant '{name}': unknown boon '{unknown}' (expected one of: {})",
                obleth_config::MODEL_BOONS.join(", ")
            )));
        }
        out.push(ModelVariant {
            name: name.to_string(),
            description: v
                .description
                .as_deref()
                .unwrap_or_default()
                .trim()
                .to_string(),
            boons: obleth_config::normalize_boons(&v.boons),
        });
    }
    Ok(out)
}

/// Validate a variant list and reject any name already claimed elsewhere.
///
/// A variant is a name a client can send, so it shares one namespace with
/// every model's `model_name`, aliases and variants (see
/// [`validate_aliases`]). `editing` is the row being written, whose own
/// names are not collisions with itself; `aliases` is the alias list this
/// same write leaves the model with.
async fn validate_variants(
    state: &AdminState,
    raw: &[ModelVariantWrite],
    editing: Option<Uuid>,
    model_name: &str,
    aliases: &[String],
) -> Result<Vec<ModelVariant>> {
    let variants = check_variants(raw, model_name, aliases)?;
    for v in &variants {
        if let Some((owner_id, owner_name)) = state.store.model_name_owner(&v.name).await? {
            if Some(owner_id) != editing {
                return Err(AdminError::BadRequest(format!(
                    "variant '{}' is already taken by model '{owner_name}'",
                    v.name
                )));
            }
        }
        reject_family_name(state, &v.name, "variant").await?;
    }
    Ok(variants)
}

/// Validate a capacity mode against the fixed vocabulary, returning its
/// canonical form. Rejected rather than normalized: a typo must not quietly
/// turn a model back to `static`.
fn validate_capacity_mode(raw: &str) -> Result<String> {
    let m = raw.trim().to_ascii_lowercase();
    if !obleth_config::is_valid_capacity_mode(&m) {
        return Err(AdminError::BadRequest(format!(
            "invalid capacity_mode `{raw}` (expected one of: {})",
            obleth_config::CAPACITY_MODES.join(", ")
        )));
    }
    Ok(m)
}

/// The stored discovery fields with a patch applied: an omitted field keeps
/// its value, a `null` (or blank text) clears it.
fn patch_discovery_fields(
    existing: &ModelRoute,
    source: Option<&str>,
    namespace: Option<&Option<String>>,
    service: Option<&Option<String>>,
    per_replica_max_in_flight: Option<Option<i64>>,
    headroom: Option<f64>,
) -> obleth_config::capacity::DiscoveryFields {
    let current = obleth_config::capacity::DiscoveryFields::of(existing);
    obleth_config::capacity::DiscoveryFields {
        source: source.map(str::to_string).unwrap_or(current.source),
        namespace: namespace.cloned().unwrap_or(current.namespace),
        service: service.cloned().unwrap_or(current.service),
        per_replica_max_in_flight: per_replica_max_in_flight
            .unwrap_or(current.per_replica_max_in_flight),
        headroom: headroom.unwrap_or(current.headroom),
    }
    .normalized()
}

/// Check a model's capacity fields, and for a `discovered` model on the
/// `kubernetes` source that this gateway can read it (see
/// [`obleth_config::capacity::validate_discovery_fields`]).
///
/// `endpoint_max_in_flight` is each of the model's endpoints' own
/// `max_in_flight` (see [`endpoint_concurrency`]), which decides whether an
/// `endpoints`-source model needs its own per-replica value.
pub(crate) fn validate_capacity_fields(
    state: &AdminState,
    capacity_mode: &str,
    model_name: &str,
    upstream_model: &str,
    fields: &obleth_config::capacity::DiscoveryFields,
    endpoint_max_in_flight: &[Option<i64>],
) -> Result<()> {
    obleth_config::capacity::validate_discovery_fields(
        model_name,
        upstream_model,
        fields,
        capacity_mode == obleth_config::DISCOVERED_CAPACITY_MODE,
        state.capacity_discovery.policy(),
        endpoint_max_in_flight,
    )
    .map_err(AdminError::BadRequest)
}

/// The own `max_in_flight` of each of a model's endpoints.
async fn endpoint_concurrency(state: &AdminState, model_id: Uuid) -> Result<Vec<Option<i64>>> {
    Ok(state
        .store
        .list_model_endpoints(model_id)
        .await?
        .into_iter()
        .map(|e| e.max_in_flight)
        .collect())
}

/// Refuse an endpoint write that would leave a `discovered` model on the
/// `endpoints` source with an endpoint it cannot count: one with no
/// `max_in_flight` while the model has no `per_replica_max_in_flight`.
/// `endpoint_id` is the endpoint being written (`None` for a new one) and
/// `max_in_flight` its value after the write.
pub(crate) async fn validate_endpoint_write(
    state: &AdminState,
    model: &ModelRoute,
    endpoint_id: Option<Uuid>,
    max_in_flight: Option<i64>,
) -> Result<()> {
    if model.capacity_mode != obleth_config::DISCOVERED_CAPACITY_MODE {
        return Ok(());
    }
    let mut values: Vec<Option<i64>> = state
        .store
        .list_model_endpoints(model.id)
        .await?
        .into_iter()
        .filter(|e| Some(e.id) != endpoint_id)
        .map(|e| e.max_in_flight)
        .collect();
    values.push(max_in_flight);
    validate_capacity_fields(
        state,
        &model.capacity_mode,
        &model.model_name,
        &model.upstream_model,
        &obleth_config::capacity::DiscoveryFields::of(model),
        &values,
    )
    .map_err(|e| match e {
        AdminError::BadRequest(msg) => {
            AdminError::BadRequest(format!("this endpoint needs its own max_in_flight: {msg}"))
        }
        other => other,
    })
}

#[utoipa::path(
    post, path = "/api/v1/models", tag = "models",
    request_body = CreateModel,
    responses((status = 200, body = ModelRouteView))
)]
pub(crate) async fn create_model(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<CreateModel>,
) -> Result<Json<ModelRouteView>> {
    let initial_status = initial_model_status(body.status)?;
    // A blank api_base is allowed: Slurm-provisioned models have no static
    // upstream until a replica is promoted into the endpoint rotation. Only
    // validate a non-empty URL.
    if !body.api_base.trim().is_empty() {
        state.ssrf.validate(&body.api_base).await?;
    }
    validate_verify_api_base(&state, body.verify_api_base.as_deref()).await?;
    let quantization = validate_quantization(body.quantization.as_deref().unwrap_or_default())?;
    // A new name must not already be another model's alias or variant, or it
    // would mean two routes. A duplicate `model_name` is left to the store's
    // unique constraint, which reports it as the conflict it always has.
    if let Some((_, owner)) = state.store.model_name_owner(&body.model_name).await? {
        if owner != body.model_name.trim() {
            return Err(AdminError::BadRequest(format!(
                "'{}' is already an alias or variant of model '{owner}'",
                body.model_name.trim()
            )));
        }
    }
    reject_family_name(&state, &body.model_name, "model name").await?;
    let rejected_request_fields = body
        .rejected_request_fields
        .as_deref()
        .map(obleth_config::validate_rejected_request_fields)
        .transpose()
        .map_err(AdminError::BadRequest)?
        .unwrap_or_default();
    let session_affinity = body.session_affinity.unwrap_or_default();
    session_affinity
        .validate()
        .map_err(AdminError::BadRequest)?;
    let aliases = validate_aliases(
        &state,
        body.aliases.as_deref().unwrap_or_default(),
        None,
        body.model_name.trim(),
    )
    .await?;
    let variants = validate_variants(
        &state,
        body.variants.as_deref().unwrap_or_default(),
        None,
        body.model_name.trim(),
        &aliases,
    )
    .await?;
    let upstream_headers = match &body.upstream_headers {
        Some(write) => obleth_config::merge_upstream_headers(&Default::default(), write)
            .map_err(AdminError::BadRequest)?,
        None => Default::default(),
    };
    let capacity_mode = validate_capacity_mode(
        body.capacity_mode
            .as_deref()
            .unwrap_or(obleth_config::DEFAULT_CAPACITY_MODE),
    )?;
    let discovery = obleth_config::capacity::DiscoveryFields {
        source: body
            .capacity_source
            .clone()
            .unwrap_or_else(|| obleth_config::DEFAULT_CAPACITY_SOURCE.to_string()),
        namespace: body.capacity_namespace.clone(),
        service: body.capacity_service.clone(),
        per_replica_max_in_flight: body.per_replica_max_in_flight,
        headroom: body.capacity_headroom.unwrap_or(1.0),
    }
    .normalized();
    // A new model has no endpoint rows yet.
    validate_capacity_fields(
        &state,
        &capacity_mode,
        body.model_name.trim(),
        &body.upstream_model,
        &discovery,
        &[],
    )?;
    let model_type = validate_model_type(body.model_type.as_deref().unwrap_or("chat"))?;
    let lifecycle = ModelLifecycle {
        status: initial_status,
        changed_at: (initial_status == ModelStatus::Staged).then(chrono::Utc::now),
        ..Default::default()
    };
    // One insert: switched off, staged, restricted and scheduled for health
    // checks as asked before the first publish below, so the data plane
    // never sees the route any other way.
    let model = state
        .store
        .create_model(&obleth_store::NewModel {
            model_name: body.model_name.clone(),
            fields: obleth_store::ModelFields {
                description: body.description.clone().unwrap_or_default(),
                upstream_model: body.upstream_model.clone(),
                api_base: body.api_base.clone(),
                api_key: body.api_key.clone(),
                model_type,
                input_cost_per_token: body.input_cost_per_token.unwrap_or(0.0),
                output_cost_per_token: body.output_cost_per_token.unwrap_or(0.0),
                cost_per_image: body.cost_per_image.unwrap_or(0.0),
                cost_per_audio_second: body.cost_per_audio_second.unwrap_or(0.0),
                cost_per_character: body.cost_per_character.unwrap_or(0.0),
                cost_per_video: body.cost_per_video.unwrap_or(0.0),
                context_window: body.context_window.unwrap_or(8192),
                admission_weight: body.admission_weight.unwrap_or(100),
                max_in_flight: body.max_in_flight,
                supports_function_calling: body.supports_function_calling.unwrap_or(false),
                supports_system_messages: body.supports_system_messages.unwrap_or(true),
                supports_response_schema: body.supports_response_schema.unwrap_or(false),
                supports_tool_choice: body.supports_tool_choice.unwrap_or(false),
                supports_vision: body.supports_vision.unwrap_or(false),
                tags: body.tags.clone().unwrap_or_default(),
                boons: body.boons.clone().unwrap_or_default(),
                tool_servers: body.tool_servers.clone().unwrap_or_default(),
                energy_slots_per_node: body.energy_slots_per_node.unwrap_or(0),
                route_bias: body.route_bias.unwrap_or(1.0),
                auto_eligible: body.auto_eligible.unwrap_or(true),
                draft_model: body.draft_model.clone().unwrap_or_default(),
                verify_api_base: body.verify_api_base.clone().unwrap_or_default(),
                verify_upstream_model: body.verify_upstream_model.clone().unwrap_or_default(),
                aliases,
                quantization,
                upstream_headers,
                capacity_mode,
                discovery,
                variants,
            },
            enabled: body.enabled != Some(false),
            lifecycle,
            rejected_request_fields,
            session_affinity,
            health_check_interval_secs: state.health.default_interval_secs,
        })
        .await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "create_model",
            "model",
            model.id,
            serde_json::to_value(ModelRouteView::from(model.clone())).unwrap_or_default(),
        )
        .await?;
    Ok(Json(model.into()))
}

#[utoipa::path(
    get, path = "/api/v1/models", tag = "models",
    responses((status = 200, body = [ModelRouteView]))
)]
pub(crate) async fn list_models(
    State(state): State<AdminState>,
) -> Result<Json<Vec<ModelRouteView>>> {
    Ok(Json(views(state.store.list_models().await?)))
}

#[utoipa::path(
    get, path = "/api/v1/models/{id}", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 200, body = ModelRouteView), (status = 404))
)]
pub(crate) async fn get_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<ModelRouteView>> {
    Ok(Json(state.store.get_model(id).await?.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = UpdateModel,
    responses((status = 200, body = ModelRouteView))
)]
pub(crate) async fn update_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<UpdateModel>,
) -> Result<Json<ModelRouteView>> {
    // Blank api_base allowed for provisioned-only (Slurm) models — see create_model.
    if !body.api_base.trim().is_empty() {
        state.ssrf.validate(&body.api_base).await?;
    }
    validate_verify_api_base(&state, body.verify_api_base.as_deref()).await?;
    let existing = state.store.get_model(id).await?;
    let rejected_request_fields = body
        .rejected_request_fields
        .as_deref()
        .map(obleth_config::validate_rejected_request_fields)
        .transpose()
        .map_err(AdminError::BadRequest)?;
    if let Some(affinity) = &body.session_affinity {
        affinity.validate().map_err(AdminError::BadRequest)?;
    }
    let api_key = body.api_key.as_deref().or(existing.api_key.as_deref());
    let quantization = match body.quantization.as_deref() {
        Some(q) => validate_quantization(q)?,
        None => existing.quantization.clone(),
    };
    let aliases = match body.aliases.as_deref() {
        Some(list) => validate_aliases(&state, list, Some(id), &existing.model_name).await?,
        None => existing.aliases.clone(),
    };
    let variants = match body.variants.as_deref() {
        Some(list) => {
            validate_variants(&state, list, Some(id), &existing.model_name, &aliases).await?
        }
        // Kept as stored, but a new alias may not take a kept variant's name.
        None => {
            if let Some(v) = existing.variants.iter().find(|v| aliases.contains(&v.name)) {
                return Err(AdminError::BadRequest(format!(
                    "alias '{}' is already one of the model's variants",
                    v.name
                )));
            }
            existing.variants.clone()
        }
    };
    let upstream_headers = match &body.upstream_headers {
        Some(write) => obleth_config::merge_upstream_headers(&existing.upstream_headers, write)
            .map_err(AdminError::BadRequest)?,
        None => existing.upstream_headers.clone(),
    };
    let capacity_mode = match body.capacity_mode.as_deref() {
        Some(m) => validate_capacity_mode(m)?,
        None => existing.capacity_mode.clone(),
    };
    let discovery = patch_discovery_fields(
        &existing,
        body.capacity_source.as_deref(),
        body.capacity_namespace.as_ref(),
        body.capacity_service.as_ref(),
        body.per_replica_max_in_flight,
        body.capacity_headroom,
    );
    validate_capacity_fields(
        &state,
        &capacity_mode,
        &existing.model_name,
        &body.upstream_model,
        &discovery,
        &endpoint_concurrency(&state, existing.id).await?,
    )?;
    let model_type =
        validate_model_type(body.model_type.as_deref().unwrap_or(&existing.model_type))?;
    let model = state
        .store
        .update_model(
            id,
            &obleth_store::ModelUpdate {
                fields: obleth_store::ModelFields {
                    description: body
                        .description
                        .clone()
                        .unwrap_or_else(|| existing.description.clone()),
                    upstream_model: body.upstream_model.clone(),
                    api_base: body.api_base.clone(),
                    api_key: api_key.map(str::to_string),
                    model_type,
                    input_cost_per_token: body
                        .input_cost_per_token
                        .unwrap_or(existing.input_cost_per_token),
                    output_cost_per_token: body
                        .output_cost_per_token
                        .unwrap_or(existing.output_cost_per_token),
                    cost_per_image: body.cost_per_image.unwrap_or(existing.cost_per_image),
                    cost_per_audio_second: body
                        .cost_per_audio_second
                        .unwrap_or(existing.cost_per_audio_second),
                    cost_per_character: body
                        .cost_per_character
                        .unwrap_or(existing.cost_per_character),
                    cost_per_video: body.cost_per_video.unwrap_or(existing.cost_per_video),
                    context_window: body.context_window.unwrap_or(existing.context_window),
                    admission_weight: body.admission_weight.unwrap_or(existing.admission_weight),
                    max_in_flight: body.max_in_flight.unwrap_or(existing.max_in_flight),
                    supports_function_calling: body
                        .supports_function_calling
                        .unwrap_or(existing.supports_function_calling),
                    supports_system_messages: body
                        .supports_system_messages
                        .unwrap_or(existing.supports_system_messages),
                    supports_response_schema: body
                        .supports_response_schema
                        .unwrap_or(existing.supports_response_schema),
                    supports_tool_choice: body
                        .supports_tool_choice
                        .unwrap_or(existing.supports_tool_choice),
                    supports_vision: body.supports_vision.unwrap_or(existing.supports_vision),
                    tags: body.tags.clone().unwrap_or_else(|| existing.tags.clone()),
                    boons: body.boons.clone().unwrap_or_else(|| existing.boons.clone()),
                    tool_servers: body
                        .tool_servers
                        .clone()
                        .unwrap_or_else(|| existing.tool_servers.clone()),
                    energy_slots_per_node: body
                        .energy_slots_per_node
                        .unwrap_or(existing.energy_slots_per_node),
                    route_bias: body.route_bias.unwrap_or(existing.route_bias),
                    auto_eligible: body.auto_eligible.unwrap_or(existing.auto_eligible),
                    draft_model: body
                        .draft_model
                        .clone()
                        .unwrap_or_else(|| existing.draft_model.clone()),
                    verify_api_base: body
                        .verify_api_base
                        .clone()
                        .unwrap_or_else(|| existing.verify_api_base.clone()),
                    verify_upstream_model: body
                        .verify_upstream_model
                        .clone()
                        .unwrap_or_else(|| existing.verify_upstream_model.clone()),
                    aliases,
                    quantization,
                    upstream_headers,
                    capacity_mode,
                    discovery,
                    variants,
                },
                enabled: body.enabled.unwrap_or(existing.enabled),
                rejected_request_fields,
                session_affinity: body.session_affinity,
            },
        )
        .await?;
    if model_health::probe_config_changed(&existing, &model) {
        // The old failure streak / alert state describe a configuration that
        // no longer exists; reset so the scheduler re-verifies immediately.
        state.store.reset_model_health(id).await?;
    }
    sync_model_from(&state, &model, Some(&existing)).await?;
    advice::invalidate();
    let view = ModelRouteView::from(model);
    let changes = changed_fields(&ModelRouteView::from(existing), &view);
    state
        .audit(
            &headers,
            "update_model",
            "model",
            id,
            serde_json::json!({ "model_name": view.model_name, "changes": changes }),
        )
        .await?;
    Ok(Json(view))
}

/// The top-level fields that differ between two serialized views, as
/// `{field: {"before": .., "after": ..}}`, leaving out the row's timestamps.
/// Audit details take it from the API's own view, so a secret the view hides
/// (an upstream key, header values) never reaches the log.
fn changed_fields<T: Serialize>(
    before: &T,
    after: &T,
) -> serde_json::Map<String, serde_json::Value> {
    let (Ok(serde_json::Value::Object(before)), Ok(serde_json::Value::Object(after))) =
        (serde_json::to_value(before), serde_json::to_value(after))
    else {
        return serde_json::Map::new();
    };
    after
        .into_iter()
        .filter(|(field, _)| !matches!(field.as_str(), "created_at" | "updated_at"))
        .filter_map(|(field, after)| {
            let before = before.get(&field).cloned().unwrap_or_default();
            (before != after).then(|| {
                (
                    field,
                    serde_json::json!({ "before": before, "after": after }),
                )
            })
        })
        .collect()
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/capacity", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelCapacity,
    responses((status = 200, body = ModelRouteView))
)]
pub(crate) async fn set_model_capacity(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelCapacity>,
) -> Result<Json<ModelRouteView>> {
    let model = state
        .store
        .update_model_capacity(id, body.max_in_flight)
        .await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "set_model_capacity",
            "model",
            id,
            serde_json::json!({ "max_in_flight": model.max_in_flight }),
        )
        .await?;
    Ok(Json(model.into()))
}

/// The status a new model starts in: `active` unless the request asks for
/// `staged`. Deprecating or retiring something nobody has called yet means
/// nothing, so those are refused rather than stored.
pub(crate) fn initial_model_status(requested: Option<ModelStatus>) -> Result<ModelStatus> {
    use ModelStatus;
    match requested.unwrap_or_default() {
        status @ (ModelStatus::Staged | ModelStatus::Active) => Ok(status),
        other => Err(AdminError::BadRequest(format!(
            "a new model starts `active` or `staged`, not `{}`",
            other.as_str()
        ))),
    }
}

/// The lifecycle `body` asks for on `existing`, validated against the model
/// it names as the replacement (looked up by the caller; `None` when the body
/// names none). `changed_at` moves only when the status does.
pub(crate) fn lifecycle_from_request(
    existing: &ModelRoute,
    body: SetModelStatus,
    replacement: Option<&ModelRoute>,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<ModelLifecycle> {
    use obleth_config::{ModelLifecycle, ModelStatus};
    let changed_at = if body.status == existing.lifecycle.status {
        existing.lifecycle.changed_at.or(Some(now))
    } else {
        Some(now)
    };
    if matches!(body.status, ModelStatus::Staged | ModelStatus::Active) {
        return Ok(ModelLifecycle {
            status: body.status,
            changed_at,
            ..Default::default()
        });
    }
    let note = body
        .note
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    let replacement_name = body
        .replacement
        .as_deref()
        .map(str::trim)
        .unwrap_or("")
        .to_string();
    if !replacement_name.is_empty() {
        let Some(r) = replacement else {
            return Err(AdminError::BadRequest(format!(
                "replacement `{replacement_name}` is not a registered model"
            )));
        };
        if r.id == existing.id {
            return Err(AdminError::BadRequest(
                "a model cannot be its own replacement".to_string(),
            ));
        }
        if r.model_type != existing.model_type {
            return Err(AdminError::BadRequest(format!(
                "replacement `{}` is a `{}` model; it must be `{}` like `{}`",
                r.model_name, r.model_type, existing.model_type, existing.model_name
            )));
        }
        match r.lifecycle.status_at(now) {
            ModelStatus::Retired => {
                return Err(AdminError::BadRequest(format!(
                    "replacement `{}` is itself retired",
                    r.model_name
                )));
            }
            ModelStatus::Staged => {
                return Err(AdminError::BadRequest(format!(
                    "replacement `{}` is staged, so callers cannot find it; make it active first",
                    r.model_name
                )));
            }
            ModelStatus::Active | ModelStatus::Deprecated => {}
        }
    }
    let lifecycle = ModelLifecycle {
        status: body.status,
        // The canonical name, even when the body used an alias.
        replacement: replacement
            .map(|r| r.model_name.clone())
            .unwrap_or_default(),
        retire_at: body.retire_at,
        changed_at,
        note,
        redirect: body.redirect.unwrap_or(false),
    };
    lifecycle.validate().map_err(AdminError::BadRequest)?;
    Ok(lifecycle)
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/status", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelStatus,
    responses((status = 200, body = ModelRouteView))
)]
pub(crate) async fn set_model_status(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelStatus>,
) -> Result<Json<ModelRouteView>> {
    let existing = state.store.get_model(id).await?;
    let replacement = match body.replacement.as_deref().map(str::trim).filter(|r| {
        !r.is_empty() && matches!(body.status, ModelStatus::Deprecated | ModelStatus::Retired)
    }) {
        Some(name) => match state.store.get_model_by_name(name).await {
            Ok(m) => Some(m),
            Err(obleth_store::StoreError::NotFound) => None,
            Err(e) => return Err(e.into()),
        },
        None => None,
    };
    let lifecycle =
        lifecycle_from_request(&existing, body, replacement.as_ref(), chrono::Utc::now())?;
    let model = state.store.set_model_lifecycle(id, &lifecycle).await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "set_model_status",
            "model",
            id,
            serde_json::json!({
                "model_name": model.model_name,
                "status": lifecycle.status,
                "replacement": lifecycle.replacement,
                "retire_at": lifecycle.retire_at,
                "redirect": lifecycle.redirect,
            }),
        )
        .await?;
    Ok(Json(model.into()))
}

/// Check a reasoning map for `model`: well formed, and only on a chat
/// model unless it is pass-through.
pub(crate) fn check_reasoning(model: &ModelRoute, reasoning: &ModelReasoning) -> Result<()> {
    reasoning.validate().map_err(AdminError::BadRequest)?;
    if !reasoning.is_passthrough() && model.model_type != "chat" {
        return Err(AdminError::BadRequest(format!(
            "`{}` is a `{}` model; only chat models take reasoning effort",
            model.model_name, model.model_type
        )));
    }
    Ok(())
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/reasoning", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = ModelReasoning,
    responses((status = 200, body = ModelRouteView))
)]
pub(crate) async fn set_model_reasoning(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ModelReasoning>,
) -> Result<Json<ModelRouteView>> {
    let existing = state.store.get_model(id).await?;
    check_reasoning(&existing, &body)?;
    let model = state.store.set_model_reasoning(id, &body).await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "set_model_reasoning",
            "model",
            id,
            serde_json::json!({
                "model_name": model.model_name,
                "reasoning": body,
            }),
        )
        .await?;
    Ok(Json(model.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/listing", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelListing,
    responses((status = 200, body = ModelRouteView))
)]
pub(crate) async fn set_model_listing(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelListing>,
) -> Result<Json<ModelRouteView>> {
    let model = state.store.set_model_listed(id, body.listed).await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "set_model_listing",
            "model",
            id,
            serde_json::json!({
                "model_name": model.model_name,
                "listed": body.listed,
            }),
        )
        .await?;
    Ok(Json(model.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/rejected-request-fields", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelRejectedRequestFields,
    responses((status = 200, body = ModelRouteView), (status = 400), (status = 404))
)]
pub(crate) async fn set_model_rejected_request_fields(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelRejectedRequestFields>,
) -> Result<Json<ModelRouteView>> {
    let fields = obleth_config::validate_rejected_request_fields(&body.rejected_request_fields)
        .map_err(AdminError::BadRequest)?;
    let model = state
        .store
        .set_model_rejected_request_fields(id, &fields)
        .await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "set_model_rejected_request_fields",
            "model",
            id,
            serde_json::json!({
                "model_name": model.model_name,
                "rejected_request_fields": model.rejected_request_fields,
            }),
        )
        .await?;
    Ok(Json(model.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/session-affinity", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = ModelSessionAffinity,
    responses((status = 200, body = ModelRouteView), (status = 400), (status = 404))
)]
pub(crate) async fn set_model_session_affinity(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ModelSessionAffinity>,
) -> Result<Json<ModelRouteView>> {
    body.validate().map_err(AdminError::BadRequest)?;
    let model = state.store.set_model_session_affinity(id, &body).await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "set_model_session_affinity",
            "model",
            id,
            serde_json::json!({
                "model_name": model.model_name,
                "session_affinity": body,
            }),
        )
        .await?;
    Ok(Json(model.into()))
}

/// Refuse `name` for a model, alias or variant when a family already uses
/// it: a client sending `model: "x"` must reach one thing.
async fn reject_family_name(state: &AdminState, name: &str, what: &str) -> Result<()> {
    if let Some((_, family)) = state.store.family_name_owner(name).await? {
        return Err(AdminError::BadRequest(format!(
            "{what} '{}' is already the name of model family '{family}'",
            name.trim()
        )));
    }
    Ok(())
}

/// Validate a family write and return it in storage form: the name trimmed
/// and unclaimed, every member a registered chat model named canonically.
async fn check_family(
    state: &AdminState,
    body: ModelFamilyWrite,
    editing: Option<Uuid>,
) -> Result<(String, String, ReasoningLevel, FamilyMembers)> {
    let name = body.name.trim().to_string();
    if name.is_empty() {
        return Err(AdminError::BadRequest("name must not be empty".into()));
    }
    if name.chars().count() > obleth_config::reasoning::MAX_FAMILY_NAME_CHARS
        || name.chars().any(char::is_whitespace)
    {
        return Err(AdminError::BadRequest(format!(
            "name must be at most {} characters with no spaces",
            obleth_config::reasoning::MAX_FAMILY_NAME_CHARS
        )));
    }
    if name == obleth_config::routing::AUTO_MODEL_NAME {
        return Err(AdminError::BadRequest(format!(
            "'{name}' is reserved for automatic model selection"
        )));
    }
    if let Some((_, owner)) = state.store.model_name_owner(&name).await? {
        return Err(AdminError::BadRequest(format!(
            "'{name}' is already a name of model '{owner}'"
        )));
    }
    if let Some((owner_id, _)) = state.store.family_name_owner(&name).await? {
        if Some(owner_id) != editing {
            return Err(AdminError::BadRequest(format!(
                "a model family named '{name}' already exists"
            )));
        }
    }
    let description = body.description.unwrap_or_default().trim().to_string();
    if description.chars().count() > obleth_config::reasoning::MAX_FAMILY_DESCRIPTION_CHARS {
        return Err(AdminError::BadRequest(format!(
            "description is longer than {} characters",
            obleth_config::reasoning::MAX_FAMILY_DESCRIPTION_CHARS
        )));
    }
    let mut members = FamilyMembers::default();
    for level in ReasoningLevel::ALL {
        let raw = body.members.get(level).trim();
        if raw.is_empty() {
            return Err(AdminError::BadRequest(format!(
                "choose the model that answers at `{level}`"
            )));
        }
        let Some((id, _)) = state.store.model_name_owner(raw).await? else {
            return Err(AdminError::BadRequest(format!(
                "`{level}`: '{raw}' is not a registered model"
            )));
        };
        let model = state.store.get_model(id).await?;
        if model.model_type != "chat" {
            return Err(AdminError::BadRequest(format!(
                "`{level}`: '{}' is a `{}` model; a family's members must be chat models",
                model.model_name, model.model_type
            )));
        }
        *members.get_mut(level) = model.model_name;
    }
    Ok((name, description, body.default_level, members))
}

#[utoipa::path(
    get, path = "/api/v1/model-families", tag = "models",
    responses((status = 200, body = [ModelFamily]))
)]
pub(crate) async fn list_model_families(
    State(state): State<AdminState>,
) -> Result<Json<Vec<ModelFamily>>> {
    Ok(Json(state.store.list_model_families().await?))
}

#[utoipa::path(
    post, path = "/api/v1/model-families", tag = "models",
    request_body = ModelFamilyWrite,
    responses((status = 200, body = ModelFamily))
)]
pub(crate) async fn create_model_family(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<ModelFamilyWrite>,
) -> Result<Json<ModelFamily>> {
    let (name, description, default_level, members) = check_family(&state, body, None).await?;
    let family = state
        .store
        .create_model_family(&name, &description, default_level, &members)
        .await?;
    state
        .audit(
            &headers,
            "create_model_family",
            "model_family",
            family.id,
            serde_json::json!({
                "name": family.name,
                "default_level": family.default_level,
                "members": family.members,
            }),
        )
        .await?;
    Ok(Json(family))
}

#[utoipa::path(
    put, path = "/api/v1/model-families/{id}", tag = "models",
    params(("id" = Uuid, Path, description = "Family id")),
    request_body = ModelFamilyWrite,
    responses((status = 200, body = ModelFamily))
)]
pub(crate) async fn update_model_family(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ModelFamilyWrite>,
) -> Result<Json<ModelFamily>> {
    state.store.get_model_family(id).await?;
    let (name, description, default_level, members) = check_family(&state, body, Some(id)).await?;
    let family = state
        .store
        .update_model_family(id, &name, &description, default_level, &members)
        .await?;
    state
        .audit(
            &headers,
            "update_model_family",
            "model_family",
            id,
            serde_json::json!({
                "name": family.name,
                "default_level": family.default_level,
                "members": family.members,
            }),
        )
        .await?;
    Ok(Json(family))
}

#[utoipa::path(
    delete, path = "/api/v1/model-families/{id}", tag = "models",
    params(("id" = Uuid, Path, description = "Family id")),
    responses((status = 204))
)]
pub(crate) async fn delete_model_family(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let family = state.store.get_model_family(id).await?;
    state.store.delete_model_family(id).await?;
    state
        .audit(
            &headers,
            "delete_model_family",
            "model_family",
            id,
            serde_json::json!({ "name": family.name }),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[utoipa::path(
    put,
    path = "/api/v1/models/{id}/capacity-mode",
    tag = "models",
    request_body = SetModelCapacityMode,
    responses((status = 200, body = ModelRouteView))
)]
pub(crate) async fn set_model_capacity_mode(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelCapacityMode>,
) -> Result<Json<ModelRouteView>> {
    let capacity_mode = validate_capacity_mode(&body.capacity_mode)?;
    let existing = state.store.get_model(id).await?;
    let discovery = patch_discovery_fields(
        &existing,
        body.capacity_source.as_deref(),
        body.capacity_namespace.as_ref(),
        body.capacity_service.as_ref(),
        body.per_replica_max_in_flight,
        body.capacity_headroom,
    );
    validate_capacity_fields(
        &state,
        &capacity_mode,
        &existing.model_name,
        &existing.upstream_model,
        &discovery,
        &endpoint_concurrency(&state, existing.id).await?,
    )?;
    let model = state
        .store
        .update_model_capacity_mode(id, &capacity_mode, Some(&discovery))
        .await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "set_model_capacity_mode",
            "model",
            id,
            serde_json::json!({
                "capacity_mode": model.capacity_mode,
                "capacity_source": model.capacity_source,
                "capacity_namespace": model.capacity_namespace,
                "capacity_service": model.capacity_service,
                "per_replica_max_in_flight": model.per_replica_max_in_flight,
                "capacity_headroom": model.capacity_headroom,
            }),
        )
        .await?;
    Ok(Json(model.into()))
}

/// Run an auto-tune ramp probe against the model's upstream and return a
/// recommendation. Recommend-only: this writes no config. The probe drives
/// real load directly at the upstream, so it costs upstream tokens.
#[utoipa::path(
    post,
    path = "/api/v1/models/{id}/autotune",
    tag = "models",
    request_body = AutotuneRequest,
    responses((status = 200, body = AutotuneReport))
)]
pub(crate) async fn autotune_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<AutotuneRequest>,
) -> Result<Json<AutotuneReport>> {
    let model = state.store.get_model(id).await?;
    let report = autotune::run_probe(&state.health.http, &model, &body)
        .await
        .map_err(|e| AdminError::BadRequest(e.to_string()))?;
    state
        .audit(
            &headers,
            "autotune_model",
            "model",
            id,
            serde_json::json!({
                "recommended_max_in_flight": report.recommended_max_in_flight,
                "knee_reason": report.knee_reason,
                "baseline_p99_ms": report.baseline_p99_ms,
                "latency_ceiling_ms": report.latency_ceiling_ms,
                "latency_headroom": report.latency_headroom,
                "workload": report.workload,
                "max_concurrency": report.max_concurrency,
            }),
        )
        .await?;
    Ok(Json(report))
}

/// Apply an auto-tune recommendation: set `max_in_flight`, flip the model to
/// `tuned` mode, and stamp the tuned timestamp.
#[utoipa::path(
    post,
    path = "/api/v1/models/{id}/autotune/apply",
    tag = "models",
    request_body = ApplyAutotuneCapacity,
    responses((status = 200, body = ModelRouteView))
)]
pub(crate) async fn apply_autotune_capacity(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ApplyAutotuneCapacity>,
) -> Result<Json<ModelRouteView>> {
    if body.max_in_flight < 1 {
        return Err(AdminError::BadRequest(
            "max_in_flight must be >= 1".to_string(),
        ));
    }
    let model = state
        .store
        .apply_tuned_model_capacity(id, body.max_in_flight)
        .await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "apply_autotune_capacity",
            "model",
            id,
            serde_json::json!({
                "max_in_flight": model.max_in_flight,
                "capacity_mode": model.capacity_mode,
            }),
        )
        .await?;
    Ok(Json(model.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/weight", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelWeight,
    responses((status = 200, body = ModelRouteView))
)]
pub(crate) async fn set_model_weight(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelWeight>,
) -> Result<Json<ModelRouteView>> {
    let model = state
        .store
        .update_model_admission_weight(id, body.admission_weight)
        .await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "set_model_weight",
            "model",
            id,
            serde_json::json!({ "admission_weight": model.admission_weight }),
        )
        .await?;
    Ok(Json(model.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/cache", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelCache,
    responses((status = 200, body = ModelRouteView))
)]
pub(crate) async fn set_model_cache(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelCache>,
) -> Result<Json<ModelRouteView>> {
    let model = state
        .store
        .update_model_cache(id, body.cache_enabled, body.cache_ttl_secs.unwrap_or(300))
        .await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "set_model_cache",
            "model",
            id,
            serde_json::json!({
                "cache_enabled": model.cache_enabled,
                "cache_ttl_secs": model.cache_ttl_secs
            }),
        )
        .await?;
    Ok(Json(model.into()))
}

#[utoipa::path(
    put, path = "/api/v1/models/{id}/reliability", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = SetModelReliability,
    responses((status = 200, body = ModelRouteView))
)]
pub(crate) async fn set_model_reliability(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<SetModelReliability>,
) -> Result<Json<ModelRouteView>> {
    let existing = state.store.get_model(id).await?;
    let selection_mode = match body.endpoint_selection_mode.as_deref() {
        Some(mode) => validate_endpoint_selection_mode(mode)?,
        None => existing.endpoint_selection_mode.clone(),
    };
    let model = state
        .store
        .update_model_reliability(
            id,
            body.request_timeout_secs
                .unwrap_or(existing.request_timeout_secs),
            body.max_retries.unwrap_or(existing.max_retries),
            body.retry_backoff_ms.unwrap_or(existing.retry_backoff_ms),
            &selection_mode,
            body.debug_diagnostics.unwrap_or(existing.debug_diagnostics),
        )
        .await?;
    sync_model(&state, &model).await?;
    state
        .audit(
            &headers,
            "set_model_reliability",
            "model",
            id,
            serde_json::json!({
                "request_timeout_secs": model.request_timeout_secs,
                "max_retries": model.max_retries,
                "retry_backoff_ms": model.retry_backoff_ms,
                "endpoint_selection_mode": model.endpoint_selection_mode,
            }),
        )
        .await?;
    Ok(Json(model.into()))
}

#[utoipa::path(
    delete, path = "/api/v1/models/{id}", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 204), (status = 404))
)]
pub(crate) async fn delete_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let model = state.store.get_model(id).await?;
    // A family names its members; deleting one would leave a level of that
    // family answering 404.
    let families = state.store.families_using(&model.model_name).await?;
    if !families.is_empty() {
        return Err(AdminError::BadRequest(format!(
            "'{}' answers for model family {}; change or delete the family first",
            model.model_name,
            families
                .iter()
                .map(|f| format!("'{f}'"))
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    state.store.delete_model(id).await?;
    state.live_catalog.invalidate().await;
    // Aliases and variants are resolver keys of their own, so a delete has to
    // clear all of them or the model stays reachable under its old names.
    let mut evict_err = None;
    for name in model.addressable_names() {
        let evicted = async {
            state.redis.delete_resolved_model(name).await?;
            state
                .redis
                .publish_invalidation(&Invalidation::Model(name.to_string()))
                .await
        }
        .await;
        if let Err(e) = evicted {
            evict_err.get_or_insert(e);
        }
    }
    // Audited before an eviction failure is reported: the row is gone.
    state
        .audit(
            &headers,
            "delete_model",
            "model",
            id,
            serde_json::json!({
                "model_name": model.model_name,
                "cache_evicted": evict_err.is_none(),
            }),
        )
        .await?;
    if let Some(e) = evict_err {
        return Err(eviction_failed("the model", e));
    }
    Ok(StatusCode::NO_CONTENT)
}
