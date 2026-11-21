//! Auto router settings and route simulation.

use super::*;

/// Convert a persisted `TierSource` to the lowercase string used on the wire.
fn tier_source_as_str(t: obleth_config::TierSource) -> &'static str {
    match t {
        obleth_config::TierSource::Hybrid => "hybrid",
        obleth_config::TierSource::Derived => "derived",
        obleth_config::TierSource::Declared => "declared",
    }
}

/// Parse a wire string into a `TierSource`. Returns `None` for anything
/// unrecognized so callers can fall back to the existing value instead of
/// erroring the request.
fn parse_tier_source(s: &str) -> Option<obleth_config::TierSource> {
    match s {
        "hybrid" => Some(obleth_config::TierSource::Hybrid),
        "derived" => Some(obleth_config::TierSource::Derived),
        "declared" => Some(obleth_config::TierSource::Declared),
        _ => None,
    }
}

/// View of the persisted `auto` router classifier and scoring settings.
#[derive(Debug, Serialize, ToSchema)]
pub struct AutoRouterSettingsView {
    pub classifier_enabled: bool,
    pub classifier_model: Option<String>,
    pub classifier_timeout_ms: u64,
    /// The fixed tag vocabulary, surfaced so the UI can render tag pickers.
    pub available_tags: Vec<String>,
    pub capacity_weight: f64,
    pub cost_weight: f64,
    pub tag_weight: f64,
    pub default_soft_cap: u32,
    pub temperature: f64,
    pub difficulty_enabled: bool,
    /// `"hybrid" | "derived" | "declared"`.
    pub tier_source: String,
    /// Keep a conversation on the model that answered its last turn until
    /// that model is not doing a good job.
    pub sticky_enabled: bool,
    /// Model or alias served for unknown model names on `/v1/messages`;
    /// `None` means such requests get `not_found_error`.
    pub messages_default_model: Option<String>,
    /// Model or alias that answers a `/v1/verdicts` request naming no model;
    /// `None` means `auto`.
    pub verdicts_default_model: Option<String>,
}

impl AutoRouterSettingsView {
    fn from_settings(s: &AutoRouterSettings) -> Self {
        AutoRouterSettingsView {
            classifier_enabled: s.classifier_enabled,
            classifier_model: s.classifier_model.clone(),
            classifier_timeout_ms: s.classifier_timeout_ms,
            available_tags: obleth_config::MODEL_TAGS
                .iter()
                .map(|t| t.to_string())
                .collect(),
            capacity_weight: s.capacity_weight,
            cost_weight: s.cost_weight,
            tag_weight: s.tag_weight,
            default_soft_cap: s.default_soft_cap,
            temperature: s.temperature,
            difficulty_enabled: s.difficulty_enabled,
            tier_source: tier_source_as_str(s.tier_source).to_string(),
            sticky_enabled: s.sticky_enabled,
            messages_default_model: s.messages_default_model.clone(),
            verdicts_default_model: s.verdicts_default_model.clone(),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema, Default)]
pub struct UpdateAutoRouterSettings {
    #[serde(default)]
    pub classifier_enabled: Option<bool>,
    /// `model_name` of the classifier brain. Empty string clears it.
    #[serde(default)]
    pub classifier_model: Option<String>,
    #[serde(default)]
    pub classifier_timeout_ms: Option<u64>,
    #[serde(default)]
    pub capacity_weight: Option<f64>,
    #[serde(default)]
    pub cost_weight: Option<f64>,
    #[serde(default)]
    pub tag_weight: Option<f64>,
    #[serde(default)]
    pub default_soft_cap: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub difficulty_enabled: Option<bool>,
    /// `"hybrid" | "derived" | "declared"`. Unrecognized or absent values
    /// leave the persisted `tier_source` untouched.
    #[serde(default)]
    pub tier_source: Option<String>,
    #[serde(default)]
    pub sticky_enabled: Option<bool>,
    /// Model or alias served for unknown model names on `/v1/messages`.
    /// Empty string clears it.
    #[serde(default)]
    pub messages_default_model: Option<String>,
    /// Model or alias that answers a `/v1/verdicts` request naming no model.
    /// Empty string clears it (back to `auto`).
    #[serde(default)]
    pub verdicts_default_model: Option<String>,
}

/// Merge an update over the persisted settings, clamping out-of-range values.
/// Weights are clamped to [0,1]; temperature to [0,2]; soft cap must be >= 1.
/// Fields absent from the update (or unrecognized, for `tier_source`) keep
/// their existing persisted value rather than resetting to a default.
fn merge_auto_router(
    existing: &AutoRouterSettings,
    body: &UpdateAutoRouterSettings,
) -> AutoRouterSettings {
    let unit = |v: Option<f64>, cur: f64| v.map(|x| x.clamp(0.0, 1.0)).unwrap_or(cur);
    let classifier_model = match body.classifier_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.classifier_model.clone(),
    };
    let messages_default_model = match body.messages_default_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.messages_default_model.clone(),
    };
    let verdicts_default_model = match body.verdicts_default_model.as_deref().map(str::trim) {
        Some("") => None,
        Some(m) => Some(m.to_string()),
        None => existing.verdicts_default_model.clone(),
    };
    AutoRouterSettings {
        classifier_enabled: body
            .classifier_enabled
            .unwrap_or(existing.classifier_enabled),
        classifier_model,
        classifier_timeout_ms: body
            .classifier_timeout_ms
            .filter(|ms| *ms > 0)
            .unwrap_or(existing.classifier_timeout_ms),
        capacity_weight: unit(body.capacity_weight, existing.capacity_weight),
        cost_weight: unit(body.cost_weight, existing.cost_weight),
        tag_weight: unit(body.tag_weight, existing.tag_weight),
        default_soft_cap: body
            .default_soft_cap
            .filter(|c| *c > 0)
            .unwrap_or(existing.default_soft_cap),
        temperature: body
            .temperature
            .map(|t| t.clamp(0.0, 2.0))
            .unwrap_or(existing.temperature),
        difficulty_enabled: body
            .difficulty_enabled
            .unwrap_or(existing.difficulty_enabled),
        tier_source: body
            .tier_source
            .as_deref()
            .and_then(parse_tier_source)
            .unwrap_or(existing.tier_source),
        sticky_enabled: body.sticky_enabled.unwrap_or(existing.sticky_enabled),
        messages_default_model,
        verdicts_default_model,
    }
}

#[utoipa::path(
    get, path = "/api/v1/settings/auto-router", tag = "settings",
    responses((status = 200, body = AutoRouterSettingsView))
)]
pub(crate) async fn get_auto_router_settings(
    State(state): State<AdminState>,
) -> Result<Json<AutoRouterSettingsView>> {
    let settings = state
        .store
        .get_auto_router_settings()
        .await?
        .unwrap_or_default();
    Ok(Json(AutoRouterSettingsView::from_settings(&settings)))
}

#[utoipa::path(
    put, path = "/api/v1/settings/auto-router", tag = "settings",
    request_body = UpdateAutoRouterSettings,
    responses((status = 200, body = AutoRouterSettingsView))
)]
pub(crate) async fn put_auto_router_settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<UpdateAutoRouterSettings>,
) -> Result<Json<AutoRouterSettingsView>> {
    let existing = state
        .store
        .get_auto_router_settings()
        .await?
        .unwrap_or_default();

    let settings = merge_auto_router(&existing, &body);

    state.store.put_auto_router_settings(&settings).await?;
    state
        .audit(
            &headers,
            "set_auto_router_settings",
            "settings",
            "auto_router",
            serde_json::json!({
                "classifier_enabled": settings.classifier_enabled,
                "classifier_model": settings.classifier_model,
                "classifier_timeout_ms": settings.classifier_timeout_ms,
                "capacity_weight": settings.capacity_weight,
                "cost_weight": settings.cost_weight,
                "tag_weight": settings.tag_weight,
                "default_soft_cap": settings.default_soft_cap,
                "temperature": settings.temperature,
                "difficulty_enabled": settings.difficulty_enabled,
                "tier_source": tier_source_as_str(settings.tier_source),
                "messages_default_model": settings.messages_default_model,
                "verdicts_default_model": settings.verdicts_default_model,
            }),
        )
        .await?;
    Ok(Json(AutoRouterSettingsView::from_settings(&settings)))
}

/// One hypothetical `auto` request for the routing tuner.
///
/// Everything is optional: an empty body simulates a bare, untagged prompt
/// against the saved settings. The weight fields are *overrides* — unset ones
/// fall back to whatever is persisted, exactly as a partial settings write
/// would.
#[derive(Debug, Deserialize, ToSchema, Default)]
pub struct SimulateRouteRequest {
    /// Plain prompt. Ignored when `messages` is present.
    #[serde(default)]
    pub prompt: Option<String>,
    /// An OpenAI-style `messages` array, for replaying a real request shape
    /// (multi-turn, or multimodal parts the vision heuristic keys off). Must be
    /// a JSON array when present.
    #[serde(default)]
    pub messages: Option<serde_json::Value>,
    /// Requested completion budget. Counts toward the context-window filter
    /// exactly as a real request's `max_tokens` does; absent means the request
    /// does not pin one.
    #[serde(default)]
    pub max_tokens: Option<u64>,
    /// Apply this tenant's model allowlist.
    #[serde(default)]
    pub tenant_id: Option<String>,
    /// `low` | `medium` | `high`. Overrides the heuristic difficulty, and is
    /// reported as a `header` source because it is the same override the
    /// `x-obleth-effort` header performs on the data plane.
    #[serde(default)]
    pub effort: Option<String>,
    #[serde(default)]
    pub needs_function_calling: bool,
    /// The request pins a specific tool (`tool_choice` naming a function, or
    /// `function_call`), which only models with native tool-choice support can
    /// serve — no boon emulates it.
    #[serde(default)]
    pub needs_tool_choice: bool,
    #[serde(default)]
    pub needs_response_schema: bool,
    /// Bytes at the end of the system text the classifier may read, as the
    /// `x-obleth-classify-system` header sets on the data plane. Absent reads
    /// all of it.
    #[serde(default)]
    pub classify_system: Option<usize>,
    /// Weight overrides. Unset fields fall back to the saved settings.
    #[serde(default)]
    pub capacity_weight: Option<f64>,
    #[serde(default)]
    pub cost_weight: Option<f64>,
    #[serde(default)]
    pub tag_weight: Option<f64>,
    #[serde(default)]
    pub default_soft_cap: Option<u32>,
    #[serde(default)]
    pub temperature: Option<f64>,
    #[serde(default)]
    pub difficulty_enabled: Option<bool>,
    /// Pretend this many requests are in flight per model. **Absent means the
    /// live fleet load**, which is what the gateway itself scores against —
    /// this field is a what-if override, not the default.
    #[serde(default)]
    pub busyness: Option<std::collections::HashMap<String, usize>>,
    /// Pin the softmax draw in `[0,1)`. Absent means a fresh random draw, as on
    /// the request path. Pin it when comparing two simulations that differ only
    /// in their weights: at `temperature > 0` two independent draws make
    /// sampling noise look like an effect of the weight change. The draw that
    /// was used is echoed back as `uniform`.
    #[serde(default)]
    pub uniform: Option<f64>,
    /// Ask the LIVE intent classifier (the same brain, cache and timeout the
    /// data plane uses) instead of the keyword heuristic. Costs one small
    /// model call per user turn it reads: the newest, and up to two before it
    /// when the newer ones name no topic. Ignored — heuristics as before —
    /// when the classifier is disabled, unconfigured, or this admin runs
    /// without a data plane; the response's `tag_source` says which one
    /// actually ran.
    #[serde(default)]
    pub classify: bool,
}

/// Run the whole `auto` routing pipeline against the live fleet and return the
/// decision it *would* make, without dispatching anything.
///
/// Fidelity is the entire point, so this shares code with the data plane rather
/// than reimplementing it: candidates come from [`Store::build_candidates`] (the
/// same call the proxy's boot warm-up and 15s registry refresh make), busyness
/// defaults to the same `fairshare.model_load()` the request path scores
/// against, tokens are counted with the same `obleth-tokenizer` estimator that
/// feeds the real context-window filter, overrides are merged through the same
/// [`merge_auto_router`] that clamps a real settings write, and the verdict comes
/// from `explain_selection`, which is the same `evaluate` the proxy serves from.
/// A simulator that diverged on any of these would confidently show an operator
/// a decision the gateway would never make.
///
/// Every request field is an *override* of a live input, never a replacement
/// for one: omit `busyness` and the real fleet load is used, omit `uniform` and
/// a fresh draw is taken. The draw that was used is always echoed back, so two
/// simulations can be diffed without mistaking sampling noise for a weight
/// effect.
///
/// Intent comes from `heuristic_intent` plus the optional `effort` override
/// unless the body sets `classify: true`. Then the live classifier reads the
/// same per-turn prompts the data plane builds ([`classifier_prompts`]), walks
/// them the same way ([`classify_turns`]), and is offered the same tags, the
/// candidates' tags under the tenant's allowlist. `tag_source` and
/// `difficulty_source` say which one ran, so the dashboard never implies a
/// classification that did not happen.
///
/// Read-only: no audit entry is recorded, and nothing is written.
#[utoipa::path(
    post, path = "/api/v1/router/simulate", tag = "settings",
    request_body = SimulateRouteRequest,
    responses((status = 200, body = RouteExplain))
)]
pub(crate) async fn simulate_route(
    State(state): State<AdminState>,
    Json(body): Json<SimulateRouteRequest>,
) -> Result<Json<RouteExplain>> {
    // Saved settings with the request's overrides merged over them, clamped by
    // the same merge a `PUT /settings/auto-router` would use.
    let saved = state
        .store
        .get_auto_router_settings()
        .await?
        .unwrap_or_default();
    let settings = merge_auto_router(
        &saved,
        &UpdateAutoRouterSettings {
            capacity_weight: body.capacity_weight,
            cost_weight: body.cost_weight,
            tag_weight: body.tag_weight,
            default_soft_cap: body.default_soft_cap,
            temperature: body.temperature,
            difficulty_enabled: body.difficulty_enabled,
            ..Default::default()
        },
    );
    let weights = RouterWeights::from_settings(&settings);

    // Same candidate assembly, same tier derivation, as the data plane's
    // registry refresh — including health and maintenance state.
    let candidates = state.store.build_candidates(settings.tier_source).await?;

    // Tenant allowlist, when the caller wants to see a specific tenant's view.
    // An empty list is "no allowlist", matching the proxy's treatment.
    let allowed: Option<Vec<String>> = match body.tenant_id.as_deref().map(str::trim) {
        Some(id) if !id.is_empty() => {
            let uuid = Uuid::parse_str(id)
                .map_err(|_| AdminError::BadRequest("tenant_id is not a uuid".to_string()))?;
            state
                .store
                .get_tenant(uuid)
                .await?
                .allowed_models
                .filter(|m| !m.is_empty())
        }
        _ => None,
    };

    let grants =
        BoonGrants::from_settings(&state.store.get_boon_settings().await?.unwrap_or_default());

    // Rebuild an OpenAI-style body so token counting and feature detection read
    // exactly what the data plane reads. A `messages` that is present but not an
    // array is the caller's mistake: silently simulating a blank prompt instead
    // would answer a question they did not ask, which is the worst thing to hand
    // someone debugging their routing.
    let messages = match body.messages {
        Some(serde_json::Value::Array(m)) => serde_json::Value::Array(m),
        Some(_) => {
            return Err(AdminError::BadRequest(
                "messages must be an array of chat messages".to_string(),
            ))
        }
        None => serde_json::json!([{
            "role": "user",
            "content": body.prompt.unwrap_or_default(),
        }]),
    };
    let max_tokens = body.max_tokens.unwrap_or(0);
    let mut json = serde_json::json!({ "model": "auto", "messages": messages });
    if max_tokens > 0 {
        json["max_tokens"] = serde_json::json!(max_tokens);
    }
    let est = obleth_tokenizer::HeuristicTokenizer::new().estimate_request(&json);
    let est_input_tokens = est.input_tokens as u64;
    let mut features = RequestFeatures::from_request(&json, est_input_tokens, max_tokens);
    // The three capability requirements the synthesized body cannot express on
    // its own, OR-ed on so an explicit flag can only ever tighten the filters.
    features.needs_function_calling |= body.needs_function_calling;
    features.needs_tool_choice |= body.needs_tool_choice;
    features.needs_response_schema |= body.needs_response_schema;

    let mut intent = None;
    if body.classify {
        if let Some(classify) = state.classify.as_ref() {
            let settings = state
                .store
                .get_auto_router_settings()
                .await?
                .unwrap_or_default();
            if settings.classifier_active() {
                // Same menu the data plane offers: only tags a candidate the
                // tenant may use actually carries.
                let mut menu: Vec<String> = Vec::new();
                for c in &candidates {
                    if let Some(allowed) = allowed.as_deref() {
                        if !allowed.iter().any(|m| m == &c.model.model_name) {
                            continue;
                        }
                    }
                    for t in &c.model.tags {
                        if !menu.contains(t) {
                            menu.push(t.clone());
                        }
                    }
                }
                if !menu.is_empty() {
                    // `None` is the classifier's failure shape; heuristics take
                    // over below, exactly as on the data plane.
                    intent =
                        classify_turns(classifier_prompts(&json, body.classify_system), |prompt| {
                            classify(prompt, menu.clone())
                        })
                        .await;
                }
            }
        }
    }
    let mut intent = intent.unwrap_or_else(|| heuristic_intent(&json, est_input_tokens));
    if let Some(difficulty) = difficulty_from_header(body.effort.as_deref()) {
        intent.difficulty = difficulty;
        intent.source = IntentSource::Header;
    }

    // Spare capacity is one of the three terms an operator comes here to tune,
    // so the default must be the live fleet load the gateway itself scores
    // against — simulating a perfectly idle fleet would make `capacity_weight`
    // look inert. An explicit `busyness` is a what-if override.
    let busyness = body
        .busyness
        .unwrap_or_else(|| state.fairshare.model_load());
    // At the default temperature of 0 the draw is ignored and the pick is the
    // exact argmax. Above it production samples, so an unpinned simulation draws
    // too. A caller comparing two simulations must pin the same draw across
    // both, or the sampling difference reads as a weight effect; the draw used
    // is echoed back in the response either way.
    let uniform = match body.uniform {
        Some(u) if u.is_finite() => u.clamp(0.0, 0.999_999_999),
        Some(_) => {
            return Err(AdminError::BadRequest(
                "uniform must be a finite number in [0,1)".to_string(),
            ))
        }
        None => rand::thread_rng().gen_range(0.0..1.0),
    };

    Ok(Json(explain_selection(
        &candidates,
        &features,
        &busyness,
        &state.output_stats.snapshot(),
        allowed.as_deref(),
        &intent.tags,
        grants,
        &weights,
        uniform,
        &intent,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn update_auto_router_clamps_weights_and_preserves_unset() {
        use obleth_config::AutoRouterSettings;
        let existing = AutoRouterSettings {
            capacity_weight: 0.9,
            temperature: 0.3,
            ..Default::default()
        };
        let body = UpdateAutoRouterSettings {
            cost_weight: Some(0.7),
            temperature: Some(-5.0), // out of range, must clamp to 0.0
            ..Default::default()
        };
        let merged = merge_auto_router(&existing, &body);
        assert_eq!(merged.cost_weight, 0.7);
        assert_eq!(
            merged.capacity_weight, 0.9,
            "unset fields keep their existing value"
        );
        assert_eq!(
            merged.temperature, 0.0,
            "negative temperature clamps to argmax"
        );
    }

    #[test]
    fn update_auto_router_rejects_zero_soft_cap() {
        use obleth_config::AutoRouterSettings;
        let existing = AutoRouterSettings::default();
        let body = UpdateAutoRouterSettings {
            default_soft_cap: Some(0),
            ..Default::default()
        };
        assert_eq!(merge_auto_router(&existing, &body).default_soft_cap, 8);
    }

    #[test]
    fn update_auto_router_preserves_tier_source_on_unrecognized_string() {
        use obleth_config::{AutoRouterSettings, TierSource};
        let existing = AutoRouterSettings {
            tier_source: TierSource::Declared,
            ..Default::default()
        };
        let body = UpdateAutoRouterSettings {
            tier_source: Some("bogus".to_string()),
            ..Default::default()
        };
        assert_eq!(
            merge_auto_router(&existing, &body).tier_source,
            TierSource::Declared
        );

        let body_absent = UpdateAutoRouterSettings::default();
        assert_eq!(
            merge_auto_router(&existing, &body_absent).tier_source,
            TierSource::Declared
        );

        let body_valid = UpdateAutoRouterSettings {
            tier_source: Some("derived".to_string()),
            ..Default::default()
        };
        assert_eq!(
            merge_auto_router(&existing, &body_valid).tier_source,
            TierSource::Derived
        );
    }

    #[test]
    fn auto_router_view_round_trips_scoring_fields() {
        use obleth_config::{AutoRouterSettings, TierSource};
        let s = AutoRouterSettings {
            capacity_weight: 0.7,
            cost_weight: 0.3,
            tag_weight: 0.4,
            default_soft_cap: 12,
            temperature: 0.5,
            difficulty_enabled: true,
            tier_source: TierSource::Declared,
            ..Default::default()
        };
        let view = AutoRouterSettingsView::from_settings(&s);
        assert_eq!(view.capacity_weight, 0.7);
        assert_eq!(view.cost_weight, 0.3);
        assert_eq!(view.tag_weight, 0.4);
        assert_eq!(view.default_soft_cap, 12);
        assert_eq!(view.temperature, 0.5);
        assert!(view.difficulty_enabled);
        assert_eq!(view.tier_source, "declared");
    }

    #[test]
    fn merge_auto_router_sets_and_clears_messages_default_model() {
        use obleth_config::AutoRouterSettings;
        let existing = AutoRouterSettings::default();
        let set = merge_auto_router(
            &existing,
            &UpdateAutoRouterSettings {
                messages_default_model: Some("local-llama".into()),
                ..Default::default()
            },
        );
        assert_eq!(set.messages_default_model.as_deref(), Some("local-llama"));
        let cleared = merge_auto_router(
            &set,
            &UpdateAutoRouterSettings {
                messages_default_model: Some("".into()),
                ..Default::default()
            },
        );
        assert_eq!(cleared.messages_default_model, None);
        let kept = merge_auto_router(&set, &UpdateAutoRouterSettings::default());
        assert_eq!(kept.messages_default_model.as_deref(), Some("local-llama"));
    }

    #[test]
    fn merge_auto_router_sets_and_clears_verdicts_default_model() {
        use obleth_config::AutoRouterSettings;
        let existing = AutoRouterSettings::default();
        let set = merge_auto_router(
            &existing,
            &UpdateAutoRouterSettings {
                verdicts_default_model: Some(" judge-7b ".into()),
                ..Default::default()
            },
        );
        assert_eq!(set.verdicts_default_model.as_deref(), Some("judge-7b"));
        let kept = merge_auto_router(
            &set,
            &UpdateAutoRouterSettings {
                messages_default_model: Some("local-llama".into()),
                ..Default::default()
            },
        );
        assert_eq!(kept.verdicts_default_model.as_deref(), Some("judge-7b"));
        let cleared = merge_auto_router(
            &kept,
            &UpdateAutoRouterSettings {
                verdicts_default_model: Some("".into()),
                ..Default::default()
            },
        );
        assert_eq!(cleared.verdicts_default_model, None);
    }
}
