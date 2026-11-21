//! Boon settings.

use super::*;

/// View of the persisted model-"boons" settings, flattened per boon
/// (vision, structured output, tool loop).
#[derive(Debug, Serialize, ToSchema)]
pub struct BoonSettingsView {
    pub vision_enabled: bool,
    pub vision_fallback_model: Option<String>,
    pub vision_describe_prompt: String,
    pub vision_max_images: u32,
    pub vision_timeout_ms: u64,
    /// How long an image description is kept and reused, in seconds (0 = off).
    pub vision_cache_ttl_secs: u64,
    pub structured_output_enabled: bool,
    pub structured_output_fixer_model: Option<String>,
    pub structured_output_max_repair_attempts: u32,
    pub structured_output_timeout_ms: u64,
    pub tool_loop_enabled: bool,
    pub tool_loop_max_turns: u32,
    pub tool_loop_tool_timeout_ms: u64,
    /// Wall-clock budget for one request's whole tool loop, in seconds.
    pub tool_loop_deadline_secs: u64,
    pub tool_loop_nudge: String,
    pub compression_enabled: bool,
    pub compression_min_tokens: u32,
    pub compression_max_segments: u32,
    pub compression_original_ttl_secs: u64,
    pub compression_max_lossy_segments: u32,
    pub compression_code_compaction: bool,
    pub compression_dedup: bool,
    pub compression_compact_logs: bool,
    pub compression_allow_lossy: bool,
    pub compression_neural_keep_ratio: f32,
    pub image_generation_enabled: bool,
    pub image_generation_model: Option<String>,
    pub image_generation_tool_description: String,
    pub image_generation_allowed_sizes: Vec<String>,
    pub image_generation_max_images_per_request: u32,
    pub image_generation_timeout_ms: u64,
    pub speculation_enabled: bool,
    pub speculation_draft_model: Option<String>,
    pub speculation_classify_model: Option<String>,
    pub speculation_agree_min: f64,
    pub speculation_lp_min: f64,
    pub speculation_abort_agree: f64,
    pub speculation_abort_lp: f64,
    pub speculation_first_chunk_tokens: u32,
    pub speculation_chunk_tokens: u32,
    pub speculation_decide_by_tokens: u32,
    pub speculation_max_draft_tokens: u32,
    pub speculation_pace_ms: u64,
    pub speculation_timeout_ms: u64,
    /// `chat_template_kwargs` sent with draft calls, or null when unset.
    #[schema(value_type = Object)]
    pub speculation_draft_chat_template_kwargs: Option<serde_json::Value>,
    /// Per-category gates as `[{tag, speculate, agree_min, lp_min}]`.
    #[schema(value_type = Vec<Object>)]
    pub speculation_category_gates: serde_json::Value,
    pub speculation_unlisted_categories_speculate: bool,
    pub speculation_verify_url_template: String,
    /// Per-call bound on a verifier call, in milliseconds.
    pub speculation_verify_timeout_ms: u64,
    /// Largest verifier reply read, in bytes.
    pub speculation_verify_reply_max_bytes: usize,
    pub web_search_enabled: bool,
    pub web_search_tool: Option<String>,
    pub web_search_tool_description: String,
    pub web_search_max_results: u32,
    pub web_search_max_searches_per_request: u32,
    pub web_search_timeout_ms: u64,
}

impl BoonSettingsView {
    fn from_settings(s: &BoonSettings) -> Self {
        BoonSettingsView {
            vision_enabled: s.vision.enabled,
            vision_fallback_model: s.vision.fallback_model.clone(),
            vision_describe_prompt: s.vision.prompt().to_string(),
            vision_max_images: s.vision.max_images,
            vision_timeout_ms: s.vision.timeout_ms,
            vision_cache_ttl_secs: s.vision.cache_ttl_secs,
            structured_output_enabled: s.structured_output.enabled,
            structured_output_fixer_model: s.structured_output.fixer_model.clone(),
            structured_output_max_repair_attempts: s.structured_output.max_repair_attempts,
            structured_output_timeout_ms: s.structured_output.timeout_ms,
            tool_loop_enabled: s.tool_loop.enabled,
            tool_loop_max_turns: s.tool_loop.max_turns,
            tool_loop_tool_timeout_ms: s.tool_loop.tool_timeout_ms,
            tool_loop_deadline_secs: s.tool_loop.deadline_secs,
            tool_loop_nudge: s.tool_loop.nudge.clone(),
            compression_enabled: s.compression.enabled,
            compression_min_tokens: s.compression.min_tokens,
            compression_max_segments: s.compression.max_segments,
            compression_original_ttl_secs: s.compression.original_ttl_secs,
            compression_max_lossy_segments: s.compression.max_lossy_segments,
            compression_code_compaction: s.compression.code_compaction,
            compression_dedup: s.compression.dedup,
            compression_compact_logs: s.compression.compact_logs,
            compression_allow_lossy: s.compression.allow_lossy,
            compression_neural_keep_ratio: s.compression.neural_keep_ratio,
            image_generation_enabled: s.image_generation.enabled,
            image_generation_model: s.image_generation.image_model.clone(),
            image_generation_tool_description: s.image_generation.tool_description.clone(),
            image_generation_allowed_sizes: s.image_generation.allowed_sizes.clone(),
            image_generation_max_images_per_request: s.image_generation.max_images_per_request,
            image_generation_timeout_ms: s.image_generation.timeout_ms,
            speculation_enabled: s.speculation.enabled,
            speculation_draft_model: s.speculation.draft_model.clone(),
            speculation_classify_model: s.speculation.classify_model.clone(),
            speculation_agree_min: s.speculation.agree_min,
            speculation_lp_min: s.speculation.lp_min,
            speculation_abort_agree: s.speculation.abort_agree,
            speculation_abort_lp: s.speculation.abort_lp,
            speculation_first_chunk_tokens: s.speculation.first_chunk_tokens,
            speculation_chunk_tokens: s.speculation.chunk_tokens,
            speculation_decide_by_tokens: s.speculation.decide_by_tokens,
            speculation_max_draft_tokens: s.speculation.max_draft_tokens,
            speculation_pace_ms: s.speculation.pace_ms,
            speculation_timeout_ms: s.speculation.timeout_ms,
            speculation_draft_chat_template_kwargs: s
                .speculation
                .draft_chat_template_kwargs
                .clone(),
            speculation_category_gates: serde_json::to_value(&s.speculation.category_gates)
                .unwrap_or_else(|_| serde_json::Value::Array(Vec::new())),
            speculation_unlisted_categories_speculate: s.speculation.unlisted_categories_speculate,
            speculation_verify_url_template: s.speculation.verify_url_template.clone(),
            speculation_verify_timeout_ms: s.speculation.verify_timeout_ms,
            speculation_verify_reply_max_bytes: s.speculation.verify_reply_max_bytes,
            web_search_enabled: s.web_search.enabled,
            web_search_tool: s.web_search.search_tool.clone(),
            web_search_tool_description: s.web_search.tool_description.clone(),
            web_search_max_results: s.web_search.max_results,
            web_search_max_searches_per_request: s.web_search.max_searches_per_request,
            web_search_timeout_ms: s.web_search.timeout_ms,
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateBoonSettings {
    #[serde(default)]
    pub vision_enabled: Option<bool>,
    /// `model_name` of the vision describer model. Empty string clears it.
    #[serde(default)]
    pub vision_fallback_model: Option<String>,
    #[serde(default)]
    pub vision_describe_prompt: Option<String>,
    #[serde(default)]
    pub vision_max_images: Option<u32>,
    #[serde(default)]
    pub vision_timeout_ms: Option<u64>,
    /// How long an image description is kept and reused, in seconds. 0 turns
    /// the reuse off.
    #[serde(default)]
    pub vision_cache_ttl_secs: Option<u64>,
    #[serde(default)]
    pub structured_output_enabled: Option<bool>,
    /// `model_name` of the JSON-repair fixer model. Empty string clears it
    /// (repairs then re-prompt the request's own model).
    #[serde(default)]
    pub structured_output_fixer_model: Option<String>,
    #[serde(default)]
    pub structured_output_max_repair_attempts: Option<u32>,
    #[serde(default)]
    pub structured_output_timeout_ms: Option<u64>,
    #[serde(default)]
    pub tool_loop_enabled: Option<bool>,
    #[serde(default)]
    pub tool_loop_max_turns: Option<u32>,
    #[serde(default)]
    pub tool_loop_tool_timeout_ms: Option<u64>,
    /// Wall-clock budget for one request's whole tool loop, in seconds.
    /// Clamped to `1..=3600` (`TOOL_LOOP_MAX_DEADLINE_SECS`); omit (or send 0)
    /// to leave unchanged.
    #[serde(default)]
    pub tool_loop_deadline_secs: Option<u64>,
    /// System nudge injected with granted tools. Empty string resets it to the
    /// built-in default; omit the field to leave it unchanged.
    #[serde(default)]
    pub tool_loop_nudge: Option<String>,
    /// Enable or disable the compression boon globally. Omit to leave unchanged.
    #[serde(default)]
    pub compression_enabled: Option<bool>,
    /// Minimum heuristic token count for a segment to be considered for compaction.
    /// A value of `0` is a no-op and leaves the existing setting unchanged.
    #[serde(default)]
    pub compression_min_tokens: Option<u32>,
    /// Maximum number of segments that may be compacted per request.
    /// A value of `0` is a no-op and leaves the existing setting unchanged.
    #[serde(default)]
    pub compression_max_segments: Option<u32>,
    /// Redis TTL for stashed originals (secs). Omit/zero leaves unchanged.
    #[serde(default)]
    pub compression_original_ttl_secs: Option<u64>,
    /// Lossy segment cap. Omit/zero leaves unchanged.
    #[serde(default)]
    pub compression_max_lossy_segments: Option<u32>,
    /// Toggle conservative code compaction. Omit to leave unchanged.
    #[serde(default)]
    pub compression_code_compaction: Option<bool>,
    /// Global default for cross-turn dedup (tenant policy overrides). Omit to leave unchanged.
    #[serde(default)]
    pub compression_dedup: Option<bool>,
    /// Global default for log template-collapse (tenant policy overrides). Omit to leave unchanged.
    #[serde(default)]
    pub compression_compact_logs: Option<bool>,
    /// Global default for lossy text compaction (tenant policy overrides). Omit to leave unchanged.
    #[serde(default)]
    pub compression_allow_lossy: Option<bool>,
    /// Fraction of sentences the neural (compressor) prose pass keeps. Must be in
    /// `(0.0, 1.0]`; values outside that range (or omitted) leave it unchanged.
    #[serde(default)]
    pub compression_neural_keep_ratio: Option<f32>,
    /// Enable or disable the image-generation boon globally. Omit to leave unchanged.
    #[serde(default)]
    pub image_generation_enabled: Option<bool>,
    /// `model_name` of the registered `image`-type model that serves
    /// generations. Empty string clears it (which deactivates the boon).
    #[serde(default)]
    pub image_generation_model: Option<String>,
    /// Tool description the model reads. Empty string resets it to the built-in
    /// default; omit the field to leave it unchanged.
    #[serde(default)]
    pub image_generation_tool_description: Option<String>,
    /// Sizes offered in the tool schema. An empty or all-blank list resets it to
    /// the built-in default.
    #[serde(default)]
    pub image_generation_allowed_sizes: Option<Vec<String>>,
    /// Images per tool call, clamped to `IMAGE_GENERATION_MAX_PER_REQUEST`.
    /// A value of `0` is a no-op and leaves the existing setting unchanged.
    #[serde(default)]
    pub image_generation_max_images_per_request: Option<u32>,
    /// Generation timeout (ms). Omit/zero leaves unchanged.
    #[serde(default)]
    pub image_generation_timeout_ms: Option<u64>,
    /// Enable or disable the speculation boon globally. Omit to leave unchanged.
    #[serde(default)]
    pub speculation_enabled: Option<bool>,
    /// `model_name` of the registered chat model that drafts. Empty string
    /// clears it (which deactivates the boon).
    #[serde(default)]
    pub speculation_draft_model: Option<String>,
    /// `model_name` of the tiny classify model for per-category gates. Empty
    /// string clears it (the default gate then applies to everything).
    #[serde(default)]
    pub speculation_classify_model: Option<String>,
    /// Ship floor on rank-1 agreement, in `[0,1]`. Omit to leave unchanged.
    #[serde(default)]
    pub speculation_agree_min: Option<f64>,
    /// Ship floor on mean logprob (≤ 0). Omit to leave unchanged.
    #[serde(default)]
    pub speculation_lp_min: Option<f64>,
    /// Abort floor on agreement, in `[0,1]`. Omit to leave unchanged.
    #[serde(default)]
    pub speculation_abort_agree: Option<f64>,
    /// Abort floor on mean logprob (≤ 0). Omit to leave unchanged.
    #[serde(default)]
    pub speculation_abort_lp: Option<f64>,
    /// Draft tokens before the first verification. Omit/zero leaves unchanged.
    #[serde(default)]
    pub speculation_first_chunk_tokens: Option<u32>,
    /// Draft tokens between verifications. Omit/zero leaves unchanged.
    #[serde(default)]
    pub speculation_chunk_tokens: Option<u32>,
    /// Defer patience in draft tokens. Omit/zero leaves unchanged.
    #[serde(default)]
    pub speculation_decide_by_tokens: Option<u32>,
    /// Drafter max_tokens cap. Omit/zero leaves unchanged.
    #[serde(default)]
    pub speculation_max_draft_tokens: Option<u32>,
    /// Milliseconds between released deltas (0 = burst). Omit leaves unchanged.
    #[serde(default)]
    pub speculation_pace_ms: Option<u64>,
    /// Pre-release wall-clock budget (ms). Omit/zero leaves unchanged.
    #[serde(default)]
    pub speculation_timeout_ms: Option<u64>,
    /// `chat_template_kwargs` for draft calls. An empty object `{}` clears it;
    /// omit (or null) to leave unchanged.
    #[serde(default)]
    #[schema(value_type = Object)]
    pub speculation_draft_chat_template_kwargs: Option<serde_json::Value>,
    /// Replaces the per-category gate list. Entries are
    /// `{tag, speculate?, agree_min?, lp_min?}`; an empty list clears all
    /// per-category gates. Omit to leave unchanged.
    #[serde(default)]
    #[schema(value_type = Vec<Object>)]
    pub speculation_category_gates: Option<serde_json::Value>,
    /// Whether unlisted categories use the default gate (true) or abstain
    /// (false). Omit to leave unchanged.
    #[serde(default)]
    pub speculation_unlisted_categories_speculate: Option<bool>,
    /// Fleet rule for locating scoring endpoints ({upstream}/{model}
    /// placeholders). Empty string clears; omitted keeps the current value.
    #[serde(default)]
    pub speculation_verify_url_template: Option<String>,
    /// Per-call bound on a verifier call (tokenize, score), in milliseconds;
    /// after the commit it is what ends a stalled scorer. Omit to leave unchanged.
    #[serde(default)]
    pub speculation_verify_timeout_ms: Option<u64>,
    /// Largest verifier reply read, in bytes. Omit to leave unchanged.
    #[serde(default)]
    pub speculation_verify_reply_max_bytes: Option<usize>,
    /// Enable or disable the web-search boon globally. Omit to leave unchanged.
    #[serde(default)]
    pub web_search_enabled: Option<bool>,
    /// `model_name` of the registered `search`-type route that runs searches.
    /// Empty string clears it (which deactivates the boon).
    #[serde(default)]
    pub web_search_tool: Option<String>,
    /// Tool description the model reads. Empty string resets it to the built-in
    /// default; omit the field to leave it unchanged.
    #[serde(default)]
    pub web_search_tool_description: Option<String>,
    /// Results handed to the model per search, clamped to
    /// `WEB_SEARCH_MAX_RESULTS`. Omit/zero leaves unchanged.
    #[serde(default)]
    pub web_search_max_results: Option<u32>,
    /// Searches one request may run, clamped to `WEB_SEARCH_MAX_PER_REQUEST`.
    /// Omit/zero leaves unchanged.
    #[serde(default)]
    pub web_search_max_searches_per_request: Option<u32>,
    /// Timeout for one search (ms). Omit/zero leaves unchanged.
    #[serde(default)]
    pub web_search_timeout_ms: Option<u64>,
}

#[utoipa::path(
    get, path = "/api/v1/settings/boons", tag = "settings",
    responses((status = 200, body = BoonSettingsView))
)]
pub(crate) async fn get_boon_settings(
    State(state): State<AdminState>,
) -> Result<Json<BoonSettingsView>> {
    let settings = state.store.get_boon_settings().await?.unwrap_or_default();
    Ok(Json(BoonSettingsView::from_settings(&settings)))
}

/// Trim, drop blanks, de-duplicate order-stably, and fall back to the built-in
/// list when nothing usable remains — an empty `enum` in the tool schema would
/// make the `size` argument unsatisfiable.
fn normalize_image_sizes(sizes: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for size in sizes {
        let s = size.trim().to_string();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    }
    if out.is_empty() {
        obleth_config::ImageGenerationBoonSettings::default().allowed_sizes
    } else {
        out
    }
}

/// Images per call, bounded by the hard ceiling.
fn clamp_image_count(n: u32) -> u32 {
    n.clamp(1, obleth_config::IMAGE_GENERATION_MAX_PER_REQUEST)
}

/// Apply a `tool_loop_deadline_secs` update: `None`/0 keeps `existing`, and any
/// other value is clamped to `1..=TOOL_LOOP_MAX_DEADLINE_SECS`. Unbounded, a huge
/// value overflows the loop's deadline arithmetic on the request path.
fn merge_tool_loop_deadline(update: Option<u64>, existing: u64) -> u64 {
    update
        .filter(|s| *s > 0)
        .map(|s| s.min(TOOL_LOOP_MAX_DEADLINE_SECS))
        .unwrap_or(existing)
}

#[utoipa::path(
    put, path = "/api/v1/settings/boons", tag = "settings",
    request_body = UpdateBoonSettings,
    responses((status = 200, body = BoonSettingsView))
)]
pub(crate) async fn put_boon_settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<UpdateBoonSettings>,
) -> Result<Json<BoonSettingsView>> {
    let existing = state.store.get_boon_settings().await?.unwrap_or_default();
    let verify_template_supplied = body.speculation_verify_url_template.is_some();

    let fallback_model = match body.vision_fallback_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.vision.fallback_model.clone(),
    };

    let describe_prompt = match body.vision_describe_prompt.as_deref().map(str::trim) {
        Some("") | None => existing.vision.describe_prompt.clone(),
        Some(p) => p.to_string(),
    };

    let fixer_model = match body.structured_output_fixer_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.structured_output.fixer_model.clone(),
    };

    let image_model = match body.image_generation_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.image_generation.image_model.clone(),
    };
    // Reject a misconfiguration here rather than leaving it to surface as a
    // warn line at request time: the boon fails open, so a bad model name is
    // otherwise invisible until someone notices no images are ever produced.
    //
    // Only validate when the caller actually supplied the field: `image_model`
    // above falls back to the *existing* persisted value when it is omitted,
    // and re-validating that carried-forward value here would mean a PUT that
    // only touches an unrelated setting (e.g. toggling vision) starts failing
    // the moment the configured image model is deleted or renamed elsewhere.
    if body.image_generation_model.is_some() {
        if let Some(name) = image_model.as_deref() {
            match state.store.get_model_by_name(name).await {
                Ok(model) if model.model_type == "image" => {}
                Ok(model) => {
                    return Err(AdminError::BadRequest(format!(
                        "image_generation_model `{name}` has model_type `{}`; it must be `image`",
                        model.model_type
                    )))
                }
                // A genuinely missing model is the caller's mistake.
                Err(obleth_store::StoreError::NotFound) => {
                    return Err(AdminError::BadRequest(format!(
                        "image_generation_model `{name}` is not a registered model"
                    )))
                }
                // Any other store error (a transient DB failure, say) is not a
                // bad model name and must not be reported as one — that would
                // invite an operator to "fix" a perfectly good setting.
                Err(e) => return Err(e.into()),
            }
        }
    }

    let image_tool_description = match body
        .image_generation_tool_description
        .as_deref()
        .map(str::trim)
    {
        Some("") => obleth_config::DEFAULT_IMAGE_TOOL_DESCRIPTION.to_string(),
        Some(d) => d.to_string(),
        None => existing.image_generation.tool_description.clone(),
    };

    // Web-search route: same merge-and-validate shape as
    // `image_generation_model` above, but it must be a `search` route.
    let search_tool = match body.web_search_tool.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.web_search.search_tool.clone(),
    };
    if body.web_search_tool.is_some() {
        if let Some(name) = search_tool.as_deref() {
            match state.store.get_model_by_name(name).await {
                Ok(model) if model.model_type == obleth_config::SEARCH_MODEL_TYPE => {}
                Ok(model) => {
                    return Err(AdminError::BadRequest(format!(
                        "web_search_tool `{name}` has model_type `{}`; it must be `{}`",
                        model.model_type,
                        obleth_config::SEARCH_MODEL_TYPE
                    )))
                }
                Err(obleth_store::StoreError::NotFound) => {
                    return Err(AdminError::BadRequest(format!(
                        "web_search_tool `{name}` is not a registered model"
                    )))
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    let search_tool_description = match body.web_search_tool_description.as_deref().map(str::trim) {
        Some("") => obleth_config::DEFAULT_WEB_SEARCH_TOOL_DESCRIPTION.to_string(),
        Some(d) => d.to_string(),
        None => existing.web_search.tool_description.clone(),
    };

    // Speculation helper models: same merge-and-validate shape as
    // `image_generation_model` above — validate only fields the caller
    // actually supplied, and require registered chat models.
    let spec_draft_model = match body.speculation_draft_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.speculation.draft_model.clone(),
    };
    let spec_classify_model = match body.speculation_classify_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.speculation.classify_model.clone(),
    };
    for (field, supplied, value) in [
        (
            "speculation_draft_model",
            body.speculation_draft_model.is_some(),
            spec_draft_model.as_deref(),
        ),
        (
            "speculation_classify_model",
            body.speculation_classify_model.is_some(),
            spec_classify_model.as_deref(),
        ),
    ] {
        if !supplied {
            continue;
        }
        if let Some(name) = value {
            match state.store.get_model_by_name(name).await {
                Ok(model) if model.model_type == "chat" => {}
                Ok(model) => {
                    return Err(AdminError::BadRequest(format!(
                        "{field} `{name}` has model_type `{}`; it must be `chat`",
                        model.model_type
                    )))
                }
                Err(obleth_store::StoreError::NotFound) => {
                    return Err(AdminError::BadRequest(format!(
                        "{field} `{name}` is not a registered model"
                    )))
                }
                Err(e) => return Err(e.into()),
            }
        }
    }
    let spec_category_gates = match body.speculation_category_gates {
        Some(v) => serde_json::from_value::<Vec<obleth_config::SpeculationCategoryGate>>(v)
            .map_err(|e| {
                AdminError::BadRequest(format!(
                    "speculation_category_gates must be a list of {{tag, speculate?, agree_min?, lp_min?}}: {e}"
                ))
            })?,
        None => existing.speculation.category_gates.clone(),
    };
    let spec_draft_kwargs = match body.speculation_draft_chat_template_kwargs {
        // An empty object clears; any other object replaces; omit keeps.
        Some(v) if v.as_object().is_some_and(|o| o.is_empty()) => None,
        Some(v) if v.is_object() => Some(v),
        Some(v) if v.is_null() => existing.speculation.draft_chat_template_kwargs.clone(),
        Some(_) => {
            return Err(AdminError::BadRequest(
                "speculation_draft_chat_template_kwargs must be a JSON object".to_string(),
            ))
        }
        None => existing.speculation.draft_chat_template_kwargs.clone(),
    };

    let settings = BoonSettings {
        vision: VisionBoonSettings {
            enabled: body.vision_enabled.unwrap_or(existing.vision.enabled),
            fallback_model,
            describe_prompt,
            max_images: body
                .vision_max_images
                .filter(|n| *n > 0)
                .unwrap_or(existing.vision.max_images),
            timeout_ms: body
                .vision_timeout_ms
                .filter(|ms| *ms > 0)
                .unwrap_or(existing.vision.timeout_ms),
            cache_ttl_secs: body
                .vision_cache_ttl_secs
                .unwrap_or(existing.vision.cache_ttl_secs),
        },
        structured_output: StructuredOutputBoonSettings {
            enabled: body
                .structured_output_enabled
                .unwrap_or(existing.structured_output.enabled),
            fixer_model,
            max_repair_attempts: body
                .structured_output_max_repair_attempts
                .map(|n| n.min(STRUCTURED_OUTPUT_MAX_REPAIR_ATTEMPTS))
                .unwrap_or(existing.structured_output.max_repair_attempts),
            timeout_ms: body
                .structured_output_timeout_ms
                .filter(|ms| *ms > 0)
                .unwrap_or(existing.structured_output.timeout_ms),
        },
        tool_loop: ToolLoopSettings {
            enabled: body.tool_loop_enabled.unwrap_or(existing.tool_loop.enabled),
            max_turns: body
                .tool_loop_max_turns
                .filter(|n| *n > 0)
                .map(|n| n.min(TOOL_LOOP_MAX_TURNS))
                .unwrap_or(existing.tool_loop.max_turns),
            tool_timeout_ms: body
                .tool_loop_tool_timeout_ms
                .filter(|ms| *ms > 0)
                .unwrap_or(existing.tool_loop.tool_timeout_ms),
            nudge: match body.tool_loop_nudge.as_deref().map(str::trim) {
                Some("") => obleth_config::default_tool_loop_nudge(),
                Some(n) => n.to_string(),
                None => existing.tool_loop.nudge.clone(),
            },
            deadline_secs: merge_tool_loop_deadline(
                body.tool_loop_deadline_secs,
                existing.tool_loop.deadline_secs,
            ),
        },
        guardrails: existing.guardrails.clone(),
        compression: obleth_config::CompressionBoonSettings {
            enabled: body
                .compression_enabled
                .unwrap_or(existing.compression.enabled),
            min_tokens: body
                .compression_min_tokens
                .filter(|n| *n > 0)
                .unwrap_or(existing.compression.min_tokens),
            max_segments: body
                .compression_max_segments
                .filter(|n| *n > 0)
                .unwrap_or(existing.compression.max_segments),
            original_ttl_secs: body
                .compression_original_ttl_secs
                .filter(|n| *n > 0)
                .unwrap_or(existing.compression.original_ttl_secs),
            max_lossy_segments: body
                .compression_max_lossy_segments
                .filter(|n| *n > 0)
                .unwrap_or(existing.compression.max_lossy_segments),
            code_compaction: body
                .compression_code_compaction
                .unwrap_or(existing.compression.code_compaction),
            dedup: body.compression_dedup.unwrap_or(existing.compression.dedup),
            compact_logs: body
                .compression_compact_logs
                .unwrap_or(existing.compression.compact_logs),
            allow_lossy: body
                .compression_allow_lossy
                .unwrap_or(existing.compression.allow_lossy),
            neural_keep_ratio: body
                .compression_neural_keep_ratio
                .filter(|r| *r > 0.0 && *r <= 1.0)
                .unwrap_or(existing.compression.neural_keep_ratio),
        },
        // No admin-API fields for the knowledge boon yet (Task 7+); carry the
        // persisted value through unchanged, same as `guardrails` above.
        knowledge: existing.knowledge.clone(),
        image_generation: obleth_config::ImageGenerationBoonSettings {
            enabled: body
                .image_generation_enabled
                .unwrap_or(existing.image_generation.enabled),
            image_model,
            tool_description: image_tool_description,
            allowed_sizes: match body.image_generation_allowed_sizes {
                Some(sizes) => normalize_image_sizes(sizes),
                None => existing.image_generation.allowed_sizes.clone(),
            },
            max_images_per_request: body
                .image_generation_max_images_per_request
                .filter(|n| *n > 0)
                .map(clamp_image_count)
                .unwrap_or(existing.image_generation.max_images_per_request),
            timeout_ms: body
                .image_generation_timeout_ms
                .filter(|ms| *ms > 0)
                .unwrap_or(existing.image_generation.timeout_ms),
        },
        speculation: obleth_config::SpeculationBoonSettings {
            enabled: body
                .speculation_enabled
                .unwrap_or(existing.speculation.enabled),
            draft_model: spec_draft_model,
            classify_model: spec_classify_model,
            agree_min: in_range(
                "speculation_agree_min",
                body.speculation_agree_min,
                0.0..=1.0,
                existing.speculation.agree_min,
            )?,
            lp_min: in_range(
                "speculation_lp_min",
                body.speculation_lp_min,
                f64::NEG_INFINITY..=0.0,
                existing.speculation.lp_min,
            )?,
            abort_agree: in_range(
                "speculation_abort_agree",
                body.speculation_abort_agree,
                0.0..=1.0,
                existing.speculation.abort_agree,
            )?,
            abort_lp: in_range(
                "speculation_abort_lp",
                body.speculation_abort_lp,
                f64::NEG_INFINITY..=0.0,
                existing.speculation.abort_lp,
            )?,
            first_chunk_tokens: at_least(
                "speculation_first_chunk_tokens",
                body.speculation_first_chunk_tokens,
                1,
                existing.speculation.first_chunk_tokens,
            )?,
            chunk_tokens: at_least(
                "speculation_chunk_tokens",
                body.speculation_chunk_tokens,
                1,
                existing.speculation.chunk_tokens,
            )?,
            decide_by_tokens: at_least(
                "speculation_decide_by_tokens",
                body.speculation_decide_by_tokens,
                1,
                existing.speculation.decide_by_tokens,
            )?,
            max_draft_tokens: at_least(
                "speculation_max_draft_tokens",
                body.speculation_max_draft_tokens,
                1,
                existing.speculation.max_draft_tokens,
            )?,
            pace_ms: body
                .speculation_pace_ms
                .unwrap_or(existing.speculation.pace_ms),
            draft_chat_template_kwargs: spec_draft_kwargs,
            timeout_ms: at_least(
                "speculation_timeout_ms",
                body.speculation_timeout_ms,
                1,
                existing.speculation.timeout_ms,
            )?,
            category_gates: spec_category_gates,
            unlisted_categories_speculate: body
                .speculation_unlisted_categories_speculate
                .unwrap_or(existing.speculation.unlisted_categories_speculate),
            verify_url_template: body
                .speculation_verify_url_template
                .map(|t| t.trim().to_string())
                .unwrap_or_else(|| existing.speculation.verify_url_template.clone()),
            verify_timeout_ms: at_least(
                "speculation_verify_timeout_ms",
                body.speculation_verify_timeout_ms,
                1,
                existing.speculation.verify_timeout_ms,
            )?,
            verify_reply_max_bytes: at_least(
                "speculation_verify_reply_max_bytes",
                body.speculation_verify_reply_max_bytes,
                1,
                existing.speculation.verify_reply_max_bytes,
            )?,
        },
        web_search: obleth_config::WebSearchBoonSettings {
            enabled: body
                .web_search_enabled
                .unwrap_or(existing.web_search.enabled),
            search_tool,
            tool_description: search_tool_description,
            max_results: body
                .web_search_max_results
                .filter(|n| *n > 0)
                .map(|n| n.min(obleth_config::WEB_SEARCH_MAX_RESULTS))
                .unwrap_or(existing.web_search.max_results),
            max_searches_per_request: body
                .web_search_max_searches_per_request
                .filter(|n| *n > 0)
                .map(|n| n.min(obleth_config::WEB_SEARCH_MAX_PER_REQUEST))
                .unwrap_or(existing.web_search.max_searches_per_request),
            timeout_ms: body
                .web_search_timeout_ms
                .filter(|ms| *ms > 0)
                .unwrap_or(existing.web_search.timeout_ms),
        },
    };

    // The speculation verifier sends each model's upstream key to this URL, so
    // hold it to the upstream destination policy (placeholders are refused in
    // the scheme/credentials; a filled host is re-checked by the proxy). Only
    // a newly supplied value is checked, so an unrelated save isn't blocked by
    // DNS for a stored one.
    let template = settings.speculation.verify_url_template.as_str();
    if verify_template_supplied && !template.is_empty() {
        state
            .ssrf
            .validate_template(template, ssrf::VERIFY_TEMPLATE_PLACEHOLDERS)
            .await
            .map_err(|e| AdminError::BadRequest(format!("speculation_verify_url_template: {e}")))?;
    }
    state.store.put_boon_settings(&settings).await?;
    state.audit(&headers, "set_boon_settings", "settings", "boons", serde_json::json!({
                "vision_enabled": settings.vision.enabled,
                "vision_fallback_model": settings.vision.fallback_model,
                "vision_max_images": settings.vision.max_images,
                "vision_timeout_ms": settings.vision.timeout_ms,
                "vision_cache_ttl_secs": settings.vision.cache_ttl_secs,
                "structured_output_enabled": settings.structured_output.enabled,
                "structured_output_fixer_model": settings.structured_output.fixer_model,
                "structured_output_max_repair_attempts": settings.structured_output.max_repair_attempts,
                "structured_output_timeout_ms": settings.structured_output.timeout_ms,
                "tool_loop_enabled": settings.tool_loop.enabled,
                "tool_loop_max_turns": settings.tool_loop.max_turns,
                "tool_loop_tool_timeout_ms": settings.tool_loop.tool_timeout_ms,
                "tool_loop_deadline_secs": settings.tool_loop.deadline_secs,
                "tool_loop_nudge_len": settings.tool_loop.nudge.len(),
                "image_generation_enabled": settings.image_generation.enabled,
                "image_generation_model": settings.image_generation.image_model,
                "image_generation_allowed_sizes": settings.image_generation.allowed_sizes,
                "image_generation_max_images_per_request": settings.image_generation.max_images_per_request,
                "image_generation_timeout_ms": settings.image_generation.timeout_ms,
                "speculation_enabled": settings.speculation.enabled,
                "speculation_draft_model": settings.speculation.draft_model,
                "speculation_classify_model": settings.speculation.classify_model,
                "speculation_agree_min": settings.speculation.agree_min,
                "speculation_lp_min": settings.speculation.lp_min,
                "speculation_category_gates": settings.speculation.category_gates.len(),
                "speculation_unlisted_categories_speculate": settings.speculation.unlisted_categories_speculate,
                "speculation_verify_url_template": settings.speculation.verify_url_template,
                "web_search_enabled": settings.web_search.enabled,
                "web_search_tool": settings.web_search.search_tool,
                "web_search_max_results": settings.web_search.max_results,
                "web_search_max_searches_per_request": settings.web_search.max_searches_per_request,
                "web_search_timeout_ms": settings.web_search.timeout_ms,
            }))
        .await?;
    Ok(Json(BoonSettingsView::from_settings(&settings)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boon_view_round_trips_compression() {
        use obleth_config::{BoonSettings, CompressionBoonSettings};
        let s = BoonSettings {
            compression: CompressionBoonSettings {
                enabled: true,
                min_tokens: 256,
                max_segments: 8,
                ..Default::default()
            },
            ..Default::default()
        };
        let view = BoonSettingsView::from_settings(&s);
        assert!(view.compression_enabled);
        assert_eq!(view.compression_min_tokens, 256);
        assert_eq!(view.compression_max_segments, 8);
        // Defaults surface the neural keep ratio so operators can read/tune it.
        assert_eq!(view.compression_neural_keep_ratio, 0.5);
    }

    #[test]
    fn boon_view_round_trips_neural_keep_ratio() {
        use obleth_config::BoonSettings;
        let mut s = BoonSettings::default();
        s.compression.neural_keep_ratio = 0.3;
        let view = BoonSettingsView::from_settings(&s);
        assert_eq!(view.compression_neural_keep_ratio, 0.3);
    }

    #[test]
    fn boon_view_round_trips_lossy_compression() {
        use obleth_config::BoonSettings;
        let mut s = BoonSettings::default();
        s.compression.enabled = true;
        s.compression.original_ttl_secs = 999;
        s.compression.max_lossy_segments = 7;
        let view = BoonSettingsView::from_settings(&s);
        assert_eq!(view.compression_original_ttl_secs, 999);
        assert_eq!(view.compression_max_lossy_segments, 7);
    }

    #[test]
    fn boon_view_round_trips_code_compaction() {
        use obleth_config::{BoonSettings, CompressionBoonSettings};
        let s = BoonSettings {
            compression: CompressionBoonSettings {
                enabled: true,
                code_compaction: true,
                ..Default::default()
            },
            ..Default::default()
        };
        let view = BoonSettingsView::from_settings(&s);
        assert!(view.compression_code_compaction);
    }

    #[test]
    fn boon_view_update_clamps_the_tool_loop_deadline() {
        assert_eq!(merge_tool_loop_deadline(None, 300), 300);
        assert_eq!(
            merge_tool_loop_deadline(Some(0), 300),
            300,
            "0 means unchanged"
        );
        assert_eq!(merge_tool_loop_deadline(Some(120), 300), 120);
        assert_eq!(
            merge_tool_loop_deadline(Some(u64::MAX), 300),
            TOOL_LOOP_MAX_DEADLINE_SECS
        );
        assert_eq!(
            merge_tool_loop_deadline(Some(TOOL_LOOP_MAX_DEADLINE_SECS + 1), 300),
            TOOL_LOOP_MAX_DEADLINE_SECS
        );
    }

    #[test]
    fn boon_view_exposes_the_tool_loop_deadline() {
        let mut s = BoonSettings::default();
        assert_eq!(
            BoonSettingsView::from_settings(&s).tool_loop_deadline_secs,
            300
        );
        s.tool_loop.deadline_secs = 90;
        assert_eq!(
            BoonSettingsView::from_settings(&s).tool_loop_deadline_secs,
            90
        );
        let update: UpdateBoonSettings =
            serde_json::from_value(serde_json::json!({ "tool_loop_deadline_secs": 120 })).unwrap();
        assert_eq!(update.tool_loop_deadline_secs, Some(120));
    }

    #[test]
    fn boon_view_exposes_image_generation_settings() {
        let s = BoonSettings {
            image_generation: obleth_config::ImageGenerationBoonSettings {
                enabled: true,
                image_model: Some("sdxl".to_string()),
                allowed_sizes: vec!["768x768".to_string()],
                max_images_per_request: 3,
                timeout_ms: 90_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let view = BoonSettingsView::from_settings(&s);
        assert!(view.image_generation_enabled);
        assert_eq!(view.image_generation_model.as_deref(), Some("sdxl"));
        assert_eq!(
            view.image_generation_allowed_sizes,
            vec!["768x768".to_string()]
        );
        assert_eq!(view.image_generation_max_images_per_request, 3);
        assert_eq!(view.image_generation_timeout_ms, 90_000);
        assert!(!view.image_generation_tool_description.is_empty());
    }

    #[test]
    fn boon_view_exposes_web_search_settings() {
        let s = BoonSettings {
            web_search: obleth_config::WebSearchBoonSettings {
                enabled: true,
                search_tool: Some("web".to_string()),
                max_results: 7,
                max_searches_per_request: 2,
                timeout_ms: 9_000,
                ..Default::default()
            },
            ..Default::default()
        };
        let view = BoonSettingsView::from_settings(&s);
        assert!(view.web_search_enabled);
        assert_eq!(view.web_search_tool.as_deref(), Some("web"));
        assert_eq!(view.web_search_max_results, 7);
        assert_eq!(view.web_search_max_searches_per_request, 2);
        assert_eq!(view.web_search_timeout_ms, 9_000);
        assert_eq!(
            view.web_search_tool_description,
            obleth_config::DEFAULT_WEB_SEARCH_TOOL_DESCRIPTION
        );
        let update: UpdateBoonSettings = serde_json::from_value(serde_json::json!({
            "web_search_enabled": true,
            "web_search_tool": "web",
        }))
        .unwrap();
        assert_eq!(update.web_search_enabled, Some(true));
        assert_eq!(update.web_search_tool.as_deref(), Some("web"));
        assert_eq!(update.web_search_max_results, None);
    }

    #[test]
    fn image_generation_sizes_are_normalised() {
        // Blank and duplicate entries are dropped; an all-blank list falls back
        // to the default rather than leaving the tool schema with an empty enum.
        assert_eq!(
            normalize_image_sizes(vec![" 512x512 ".into(), "".into(), "512x512".into()]),
            vec!["512x512".to_string()]
        );
        assert_eq!(
            normalize_image_sizes(vec!["   ".into()]),
            obleth_config::ImageGenerationBoonSettings::default().allowed_sizes
        );
    }

    #[test]
    fn image_generation_count_is_clamped_to_the_ceiling() {
        assert_eq!(
            clamp_image_count(99),
            obleth_config::IMAGE_GENERATION_MAX_PER_REQUEST
        );
        assert_eq!(clamp_image_count(1), 1);
    }
}
