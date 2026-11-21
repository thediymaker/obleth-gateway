//! Model discovery: the `/v1/models` list and the `/v1/model/info` entries a
//! key may see, built from the registry and the configured families.

use super::*;

/// Serve `GET /v1/models` from the gateway's own registry: one entry per
/// enabled route, under its client-facing `model_name`, carrying the same
/// fields as the detail lookup.
///
/// The registry is the only honest source for this answer. The listing used to
/// be the union of every upstream's own `/v1/models`, which listed nothing for
/// a route whose backend does not serve a catalog (or was unreachable that
/// second), and advertised whatever else a backend happened to serve, under
/// its backend id, though a request naming a model the gateway has not
/// registered is refused. Provisioned models are registry rows too, so nothing
/// the union found is lost.
///
/// Tenants with a model allowlist see only the models they may call, as on
/// `/model/info`: listing a model the caller would be refused advertises a 403.
pub(super) fn models_list_response(state: &AppState, resolved: &ResolvedKey) -> Response<Body> {
    let candidates = state.model_registry.load();
    let allowed = allowed_models_for(resolved);
    let boons = state.boons.settings();
    let mut list = registry_models_list(&candidates, allowed, &boons);
    let families = family_entries(&state.families.all(), &candidates, allowed, &boons);
    if let Some(data) = list["data"].as_array_mut() {
        data.extend(families);
        data.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    }
    (StatusCode::OK, axum::Json(list)).into_response()
}

/// Discovery entries for model families: each family `allowed` lets the
/// caller see (by its own name, or because every member is allowed) whose
/// default member is listed, so a family never advertises a name that
/// answers 404 or 410 by default.
fn family_entries(
    families: &[std::sync::Arc<obleth_config::ModelFamily>],
    candidates: &[obleth_config::routing::Candidate],
    allowed: Option<&[String]>,
    boons: &obleth_config::BoonSettings,
) -> Vec<serde_json::Value> {
    listed_families(families, candidates, allowed, chrono::Utc::now())
        .into_iter()
        .map(|(family, member)| family_entry(family, Some(member), boons))
        .collect()
}

/// The families [`family_entries`] lists, each with its default member.
fn listed_families<'a>(
    families: &'a [std::sync::Arc<obleth_config::ModelFamily>],
    candidates: &'a [obleth_config::routing::Candidate],
    allowed: Option<&[String]>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<(&'a obleth_config::ModelFamily, &'a ResolvedModel)> {
    families
        .iter()
        .filter(|f| family_visible(f, allowed))
        .filter_map(|f| {
            let default_member = f.members.get(f.default_level);
            let member = candidates.iter().find(|c| {
                c.model.model_name == default_member
                    && c.model.enabled
                    && c.model.lifecycle.status_at(now).listed()
            })?;
            Some((f.as_ref(), &*member.model))
        })
        .collect()
}

/// Every id `GET /v1/models` lists for `resolved`, sorted, with the model
/// pool that serves it: a variant's model's, and a family's default
/// member's. `GET /v1/load` reports these, so it shows exactly the models
/// the caller is offered.
pub(crate) fn listed_pools(
    registry: &crate::router::ModelRegistry,
    families: &crate::reasoning::FamilyRegistry,
    resolved: &ResolvedKey,
) -> Vec<(String, String)> {
    let candidates = registry.load();
    let families = families.all();
    let allowed = allowed_models_for(resolved);
    let now = chrono::Utc::now();
    let mut out: Vec<(String, String)> = listed_routes(&candidates, allowed, now)
        .into_iter()
        .map(|(model, variant)| {
            let id = variant.map_or(&model.model_name, |v| &v.name);
            (id.clone(), model.model_name.clone())
        })
        .chain(
            listed_families(&families, &candidates, allowed, now)
                .into_iter()
                .map(|(family, member)| (family.name.clone(), member.model_name.clone())),
        )
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

/// Whether a caller limited to `allowed` may see a family: by its own name,
/// or because every member is allowed.
fn family_visible(family: &obleth_config::ModelFamily, allowed: Option<&[String]>) -> bool {
    model_visible(allowed, &family.name)
        || family
            .members
            .distinct()
            .iter()
            .all(|m| model_visible(allowed, m))
}

/// One family's discovery entry: which model answers at each level, and the
/// level a request that names none gets. The catalog facts (context window,
/// prices, capabilities, boons) are the default member's, which serves a
/// request that names no level; `description` falls back to it too.
fn family_entry(
    family: &obleth_config::ModelFamily,
    default_member: Option<&ResolvedModel>,
    boons: &obleth_config::BoonSettings,
) -> serde_json::Value {
    let mut entry = serde_json::json!({
        "id": family.name,
        "object": "model",
        "owned_by": "obleth",
        "model_type": obleth_config::DEFAULT_MODEL_TYPE,
        "mode": obleth_config::DEFAULT_MODEL_TYPE,
        "description": family.description,
        "family": {
            "default_level": family.default_level,
            "members": family.members,
        },
    });
    if let (Some(member), Some(obj)) = (default_member, entry.as_object_mut()) {
        if family.description.is_empty() {
            obj.remove("description");
        }
        ModelDetails::of(member, &member.boons, boons).extend(obj);
    }
    entry
}

/// The `reasoning` a discovery entry carries: nothing for a model that
/// passes effort through; `unsupported`; or, for a mapped model, the level
/// each of the four is served at and the level a request that names none
/// gets.
fn reasoning_fields(reasoning: &obleth_config::ModelReasoning) -> Option<serde_json::Value> {
    match reasoning.mode {
        obleth_config::ReasoningMode::Passthrough => None,
        obleth_config::ReasoningMode::Unsupported => {
            Some(serde_json::json!({ "mode": "unsupported" }))
        }
        obleth_config::ReasoningMode::Mapped => {
            let mut out = serde_json::json!({
                "mode": "mapped",
                "levels": reasoning.served_levels().unwrap_or_default(),
            });
            if let Some(level) = reasoning.default_level {
                out["default_level"] = serde_json::json!(level);
            }
            Some(out)
        }
    }
}

/// The OpenAI `{object:"list", data:[…]}` listing for a registry snapshot,
/// sorted by id and limited to `allowed` when set. Pure, so the shape is
/// unit-testable without a registry.
///
/// Each variant is an entry of its own, after its model: unlike an alias it
/// changes what the caller gets, so it is meant to be found. Variants are
/// never auto-router candidates; they only appear here because their model
/// is one.
///
/// A retired model is left out (with its variants), since calling it is
/// refused; a deprecated one is listed, marked. Asking for a retired model by
/// id still answers, so a caller can find out what replaced it. A staged model
/// is left out too, and so is its id: it is served by name, for whoever was
/// told it, and is not advertised until it goes live.
fn registry_models_list(
    candidates: &[obleth_config::routing::Candidate],
    allowed: Option<&[String]>,
    boons: &obleth_config::BoonSettings,
) -> serde_json::Value {
    let mut data: Vec<serde_json::Value> = listed_routes(candidates, allowed, chrono::Utc::now())
        .into_iter()
        .map(|(model, variant)| match variant {
            Some(v) => ModelFacts::of_variant(model, v, boons),
            None => ModelFacts::of(model, boons),
        })
        .map(|f| model_entry(&f))
        .collect();
    data.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    data.dedup_by(|a, b| a["id"] == b["id"]);
    serde_json::json!({ "object": "list", "data": data })
}

/// The routes [`registry_models_list`] lists, each once for itself (`None`)
/// and once per variant, in registry order: enabled, listed and of a listed
/// type, active or deprecated at `now`, and visible under `allowed` by the
/// route's name or, for a variant, its own (as [`ModelFacts::visible`]).
fn listed_routes<'a>(
    candidates: &'a [obleth_config::routing::Candidate],
    allowed: Option<&[String]>,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<(&'a ResolvedModel, Option<&'a obleth_config::ModelVariant>)> {
    candidates
        .iter()
        .map(|c| &*c.model)
        // An unlisted model (and its variants) is served by name only.
        .filter(|m| m.enabled && m.listed && is_listed_model_type(&m.model_type))
        .filter(|m| m.lifecycle.status_at(now).listed())
        .flat_map(|m| {
            std::iter::once((m, None)).chain(m.variants.iter().map(move |v| (m, Some(v))))
        })
        .filter(|(m, v)| {
            model_visible(allowed, &m.model_name)
                || v.is_some_and(|v| model_visible(allowed, &v.name))
        })
        .collect()
}

/// One discovery entry (`/v1/models` and `/v1/models/{id}`). A variant's
/// entry also names the model it is served by and says what it is for; its
/// boons are the ones a request through it runs with.
fn model_entry(facts: &ModelFacts) -> serde_json::Value {
    let mut entry = serde_json::json!({
        "id": facts.model_name,
        "object": "model",
        "owned_by": "obleth",
        "model_type": facts.model_type,
        "mode": mode_for_model_type(&facts.model_type),
        "quantization": facts.quantization,
        "tags": facts.tags,
        "aliases": facts.aliases,
    });
    if let (Some(v), Some(obj)) = (&facts.variant, entry.as_object_mut()) {
        obj.insert("variant_of".into(), v.parent.clone().into());
        obj.insert("description".into(), v.description.clone().into());
    }
    if let (Some(r), Some(obj)) = (reasoning_fields(&facts.reasoning), entry.as_object_mut()) {
        obj.insert("reasoning".into(), r);
    }
    if let Some(obj) = entry.as_object_mut() {
        facts.details.extend(obj);
    }
    // Only a model on its way out says anything about its lifecycle, so an
    // active model's entry is unchanged.
    if facts.status != obleth_config::ModelStatus::Active {
        if let Some(obj) = entry.as_object_mut() {
            obj.extend(lifecycle_fields(facts.status, &facts.lifecycle));
        }
    }
    entry
}

/// The lifecycle fields a discovery entry carries for a deprecated or retired
/// model: `status`, plus `replacement`, `retire_at` and `note` when set.
fn lifecycle_fields(
    status: obleth_config::ModelStatus,
    lifecycle: &obleth_config::ModelLifecycle,
) -> serde_json::Map<String, serde_json::Value> {
    let mut out = serde_json::Map::new();
    out.insert("status".into(), status.as_str().into());
    if !lifecycle.replacement.is_empty() {
        out.insert("replacement".into(), lifecycle.replacement.clone().into());
    }
    if let Some(at) = lifecycle.retired_on() {
        out.insert("retire_at".into(), at.to_rfc3339().into());
    }
    if !lifecycle.note.is_empty() {
        out.insert("note".into(), lifecycle.note.clone().into());
    }
    out
}

/// What the gateway knows about one registered route, or one variant of it,
/// as the discovery endpoints report it.
#[derive(Debug, Clone, PartialEq)]
struct ModelFacts {
    /// The one name the gateway advertises for this entry: the route's
    /// `model_name`, or the variant's own name.
    model_name: String,
    model_type: String,
    quantization: String,
    aliases: Vec<String>,
    tags: Vec<String>,
    /// Set on a variant's entry.
    variant: Option<VariantFacts>,
    /// The lifecycle in force now (a variant's is its model's), and the
    /// status it gives.
    lifecycle: obleth_config::ModelLifecycle,
    status: obleth_config::ModelStatus,
    /// How the model takes reasoning effort (a variant's is its model's).
    reasoning: obleth_config::ModelReasoning,
    /// What a catalog shows about the model (a variant's is its model's).
    details: ModelDetails,
}

/// The facts a model catalog needs to describe a model without a second
/// call: what it is for, how much it reads, what it costs, what it can do.
/// Flattened into every discovery entry under the conventional `/model/info`
/// key names.
#[derive(Debug, Clone, PartialEq)]
struct ModelDetails {
    description: String,
    context_window: i64,
    input_cost_per_token: f64,
    output_cost_per_token: f64,
    supports_function_calling: bool,
    supports_tool_choice: bool,
    supports_response_schema: bool,
    supports_system_messages: bool,
    supports_vision: bool,
    /// The boons a request to this entry runs with (a variant's are its
    /// model's, then its own).
    boons: Vec<String>,
    /// What each of `boons` does on this model; see [`boon_details`].
    boon_details: serde_json::Map<String, serde_json::Value>,
}

impl ModelDetails {
    fn of(model: &ResolvedModel, boons: &[String], settings: &obleth_config::BoonSettings) -> Self {
        ModelDetails {
            description: model.description.clone(),
            context_window: model.context_window,
            input_cost_per_token: model.input_cost_per_token,
            output_cost_per_token: model.output_cost_per_token,
            supports_function_calling: model.supports_function_calling,
            supports_tool_choice: model.supports_tool_choice,
            supports_response_schema: model.supports_response_schema,
            supports_system_messages: model.supports_system_messages,
            supports_vision: model.supports_vision,
            boons: boons.to_vec(),
            boon_details: boon_details(model, boons, settings),
        }
    }

    /// Add these facts to a discovery entry. `description` is left to an
    /// entry that already set its own (a variant's, a family's).
    fn extend(&self, obj: &mut serde_json::Map<String, serde_json::Value>) {
        obj.entry("description")
            .or_insert_with(|| self.description.clone().into());
        obj.insert("context_window".into(), self.context_window.into());
        obj.insert("max_input_tokens".into(), self.context_window.into());
        obj.insert(
            "input_cost_per_token".into(),
            self.input_cost_per_token.into(),
        );
        obj.insert(
            "output_cost_per_token".into(),
            self.output_cost_per_token.into(),
        );
        obj.insert(
            "supports_function_calling".into(),
            self.supports_function_calling.into(),
        );
        obj.insert(
            "supports_tool_choice".into(),
            self.supports_tool_choice.into(),
        );
        obj.insert(
            "supports_response_schema".into(),
            self.supports_response_schema.into(),
        );
        obj.insert(
            "supports_system_messages".into(),
            self.supports_system_messages.into(),
        );
        obj.insert("supports_vision".into(), self.supports_vision.into());
        obj.insert("boons".into(), self.boons.clone().into());
        obj.insert(
            "boon_details".into(),
            serde_json::Value::Object(self.boon_details.clone()),
        );
    }
}

/// What each granted boon does on `model` as the gateway is configured now,
/// keyed by boon name. The `supports_*` flags are the model's own abilities;
/// this says which of them the gateway lends it, and which model does the
/// work. Without it, an answer about an image from a text-only model with the
/// vision boon reads as the model's own vision, and cannot be reproduced
/// against that model anywhere else.
///
/// Each value has:
/// - `active`: whether the boon runs on this model's requests. A grant can
///   sit idle: the model has the ability itself, the boon is off gateway-wide
///   or has no helper set, or a tool boon's model cannot call functions.
/// - `native`: for the boons that stand in for an ability a model may have
///   itself (`vision`, `structured_output`), whether this model has it.
/// - `helper_model`: when active, the registered model (or search route)
///   that does the boon's work. Absent when no other model is involved.
/// - `inactive_reason`: when not active, why.
///
/// Gates that depend on the caller or the request (a tenant's compression
/// policy, a request with no image in it) are not reflected: `active` means
/// the boon runs for a request it applies to.
fn boon_details(
    model: &ResolvedModel,
    boons: &[String],
    settings: &obleth_config::BoonSettings,
) -> serde_json::Map<String, serde_json::Value> {
    boons
        .iter()
        .map(|boon| (boon.clone(), boon_detail(model, boon, settings)))
        .collect()
}

/// One entry of [`boon_details`]. Each arm mirrors the gate the boon itself
/// applies in [`crate::boons`], minus the per-request parts.
fn boon_detail(
    model: &ResolvedModel,
    boon: &str,
    settings: &obleth_config::BoonSettings,
) -> serde_json::Value {
    fn named(name: Option<&str>) -> Option<String> {
        name.map(str::trim)
            .filter(|n| !n.is_empty())
            .map(str::to_string)
    }
    // A tool boon needs the model to call its tool.
    let tool_gate = |active: bool| {
        if !active {
            Some("off")
        } else if !model.supports_function_calling {
            Some("needs_function_calling")
        } else {
            None
        }
    };
    let (native, helper, inactive_reason) = match boon {
        "vision" => (
            Some(model.supports_vision),
            named(settings.vision.fallback_model.as_deref()),
            if model.supports_vision {
                Some("native")
            } else if !settings.vision.active() {
                Some("off")
            } else {
                None
            },
        ),
        // With no fixer, an invalid reply goes back to the model itself.
        "structured_output" => (
            Some(model.supports_response_schema),
            named(settings.structured_output.fixer_model.as_deref()),
            if model.supports_response_schema {
                Some("native")
            } else if !settings.structured_output.active() {
                Some("off")
            } else {
                None
            },
        ),
        "image_generation" => (
            None,
            named(settings.image_generation.image_model.as_deref()),
            tool_gate(settings.image_generation.active()),
        ),
        "web_search" => (
            None,
            named(settings.web_search.search_tool.as_deref()),
            tool_gate(settings.web_search.active()),
        ),
        "speculation" => {
            let s = &settings.speculation;
            let drafter = crate::boons::speculation::effective_draft_model(model, s);
            let reason = if !s.active() {
                Some("off")
            } else if drafter.is_empty() || drafter == model.model_name {
                Some("no_drafter")
            } else if crate::boons::speculation::scoring_base(model, &s.verify_url_template)
                .is_empty()
            {
                Some("no_scoring_endpoint")
            } else {
                None
            };
            (None, named(Some(&drafter)), reason)
        }
        "compression" => (
            None,
            None,
            (!settings.compression.active()).then_some("off"),
        ),
        "knowledge" => (
            None,
            None,
            if !settings.knowledge.active() {
                Some("off")
            } else if model.knowledge_collections.is_empty() {
                Some("no_collections")
            } else {
                None
            },
        ),
        // Stored boons are normalized to the known set; a name this gateway
        // does not know never runs.
        _ => (None, None, Some("unknown")),
    };
    let mut out = serde_json::Map::new();
    out.insert("active".into(), inactive_reason.is_none().into());
    if let Some(native) = native {
        out.insert("native".into(), native.into());
    }
    match (inactive_reason, helper) {
        (Some(reason), _) => {
            out.insert("inactive_reason".into(), reason.into());
        }
        (None, Some(helper)) => {
            out.insert("helper_model".into(), helper.into());
        }
        (None, None) => {}
    }
    serde_json::Value::Object(out)
}

/// The variant-only part of a discovery entry.
#[derive(Debug, Clone, PartialEq)]
struct VariantFacts {
    /// The model the variant is served by.
    parent: String,
    description: String,
}

impl ModelFacts {
    fn of(model: &ResolvedModel, boons: &obleth_config::BoonSettings) -> Self {
        ModelFacts {
            model_name: model.model_name.clone(),
            model_type: model.model_type.clone(),
            quantization: model.quantization.clone(),
            aliases: model.aliases.clone(),
            tags: model.tags.clone(),
            variant: None,
            lifecycle: model.lifecycle.clone(),
            reasoning: model.reasoning.clone(),
            details: ModelDetails::of(model, &model.boons, boons),
            status: model.lifecycle.status_at(chrono::Utc::now()),
        }
    }

    /// A variant's entry: its model's facts, with the boons a request through
    /// the variant runs with (the model's own, then the variant's).
    fn of_variant(
        model: &ResolvedModel,
        variant: &obleth_config::ModelVariant,
        boons: &obleth_config::BoonSettings,
    ) -> Self {
        let granted = obleth_config::union_boons(&model.boons, &variant.boons);
        ModelFacts {
            model_name: variant.name.clone(),
            aliases: Vec::new(),
            variant: Some(VariantFacts {
                parent: model.model_name.clone(),
                description: variant.description.clone(),
            }),
            details: ModelDetails::of(model, &granted, boons),
            ..ModelFacts::of(model, boons)
        }
    }

    /// Whether a caller limited to `allowed` may see this entry: by the
    /// route's name, or for a variant by its parent's name or its own (the
    /// same rule the request path's allowlist applies).
    fn visible(&self, allowed: Option<&[String]>) -> bool {
        model_visible(allowed, &self.model_name)
            || self
                .variant
                .as_ref()
                .is_some_and(|v| model_visible(allowed, &v.parent))
    }

    /// Whether `GET /v1/models/{id}` answers for this entry: when the caller
    /// may see it and it is not staged. A retired model still answers, so a
    /// caller can find out what replaced it.
    fn answers_by_id(&self, allowed: Option<&[String]>) -> bool {
        self.visible(allowed) && self.status != obleth_config::ModelStatus::Staged
    }
}

/// Index every name a registered route can be recognized by — its
/// `model_name`, its aliases, and the `upstream_model` its backend reports —
/// onto the facts the gateway advertises for it, and each of its variants
/// onto that variant's own entry.
///
/// The `upstream_model` key lets a detail lookup by a backend's own id (a
/// client that learned `glm-5-3-mxfp4` from the backend) answer with the
/// registered `glm-5-3`. Insertion order sets precedence deliberately —
/// `model_name` is written last and wins, because it is the name request
/// resolution matches on.
fn model_facts_index(
    candidates: &[obleth_config::routing::Candidate],
    boons: &obleth_config::BoonSettings,
) -> std::collections::HashMap<String, std::sync::Arc<ModelFacts>> {
    let mut by_name: std::collections::HashMap<String, std::sync::Arc<ModelFacts>> =
        std::collections::HashMap::new();
    let candidates: Vec<&obleth_config::routing::Candidate> = candidates
        .iter()
        .filter(|c| is_listed_model_type(&c.model.model_type))
        .collect();
    let facts: Vec<std::sync::Arc<ModelFacts>> = candidates
        .iter()
        .map(|c| std::sync::Arc::new(ModelFacts::of(&c.model, boons)))
        .collect();
    for (c, f) in candidates.iter().zip(facts.iter()) {
        if !c.model.upstream_model.is_empty() {
            by_name
                .entry(c.model.upstream_model.clone())
                .or_insert_with(|| f.clone());
        }
    }
    for (c, f) in candidates.iter().zip(facts.iter()) {
        for alias in &c.model.aliases {
            by_name.entry(alias.clone()).or_insert_with(|| f.clone());
        }
    }
    // A variant answers with its own entry, not its parent's.
    for c in &candidates {
        for v in &c.model.variants {
            by_name
                .entry(v.name.clone())
                .or_insert_with(|| std::sync::Arc::new(ModelFacts::of_variant(&c.model, v, boons)));
        }
    }
    for f in facts {
        by_name.insert(f.model_name.clone(), f);
    }
    by_name
}

/// Whether routes of `model_type` appear on the model discovery endpoints.
/// Search tools do not: they are not models a chat client can call, and are
/// listed on `GET /v1/search/tools` instead.
fn is_listed_model_type(model_type: &str) -> bool {
    model_type != obleth_config::SEARCH_MODEL_TYPE
}

/// The LiteLLM-convention spelling of a modality, which many OpenAI-compatible
/// clients read instead of obleth's own `model_type`. Identical to the obleth
/// vocabulary except that `image` and `video` are spelled `image_generation`
/// and `video_generation`.
pub(super) fn mode_for_model_type(model_type: &str) -> &str {
    match model_type {
        "image" => "image_generation",
        "video" => "video_generation",
        other => other,
    }
}

/// One OpenAI model object for a name the gateway has a route for — canonical
/// or alias — identical to that route's entry in the listing. `None` for a
/// name no route claims, which lets the caller fall through to the upstream
/// passthrough.
///
/// `owned_by` is `obleth`: the answer is the gateway's own. `created` is
/// omitted rather than fabricated — the hot-path view of a route does not
/// carry a registration timestamp.
///
/// `Some(Err(()))` when a route claims the name but it is outside `allowed`,
/// or staged: the caller answers that like an unknown model, without
/// forwarding the id, so the detail lookup reveals no more than the filtered
/// listing does.
pub(super) fn registered_model_entry(
    state: &AppState,
    id: &str,
    allowed: Option<&[String]>,
) -> Option<Result<serde_json::Value, ()>> {
    if id.is_empty() {
        return None;
    }
    let candidates = state.model_registry.load();
    let boons = state.boons.settings();
    let Some(facts) = model_facts_index(&candidates, &boons).get(id).cloned() else {
        let family = state.families.get(id)?;
        if !family_visible(&family, allowed) {
            return Some(Err(()));
        }
        let member = candidates
            .iter()
            .find(|c| c.model.model_name == family.members.get(family.default_level))
            .map(|c| &*c.model);
        return Some(Ok(family_entry(&family, member, &boons)));
    };
    if !facts.answers_by_id(allowed) {
        return Some(Err(()));
    }
    Some(Ok(model_entry(&facts)))
}

/// Serve `GET /model/info` from the gateway's own registry.
///
/// Where `/v1/models` answers "which names can I call" in the OpenAI shape,
/// this answers "what has this gateway been told about each model" — every
/// registered, enabled route with its health, and the configured facts a
/// client cannot infer from a name: the serving format, the routing tags, the context
/// window, the per-token prices, the capability flags, the aliases that still
/// resolve here.
///
/// The envelope keeps the *shape* of LiteLLM's `/model/info`, so a client
/// written against that proxy needs no new parsing: `{"data": [{model_name,
/// obleth_params, model_info}]}`, with the conventional `model_info` keys
/// (`mode`, `max_input_tokens`, `supports_*`, `*_cost_per_token`) in place.
///
/// The per-route object is `obleth_params`, not `litellm_params`: the shape is
/// borrowed, the contents are this gateway's, and they have diverged — the
/// serving format, the routing tags and the aliases have no LiteLLM
/// equivalent. Naming it after another proxy implied a compatibility that
/// stopped being true.
///
/// Deliberately absent from `obleth_params`: `api_base` and `api_key`. This
/// is a tenant-facing endpoint, and a backend's internal URL is not a client's
/// business — the upstream *name* is reported, the route to it is not.
///
/// Tenants with a model allowlist see only the models they may call. Listing a
/// model a caller would be refused would be advertising a 403.
pub(super) fn model_info_response(state: &AppState, resolved: &ResolvedKey) -> Response<Body> {
    let candidates = state.model_registry.load();
    let boons = state.boons.settings();
    let data = registry_model_info(&candidates, allowed_models_for(resolved), &boons);
    (
        StatusCode::OK,
        axum::Json(serde_json::json!({ "data": data })),
    )
        .into_response()
}

/// The `/model/info` entries for a registry snapshot: every enabled, listed
/// route `allowed` lets the caller see. Pure, like [`registry_models_list`].
fn registry_model_info(
    candidates: &[obleth_config::routing::Candidate],
    allowed: Option<&[String]>,
    boons: &obleth_config::BoonSettings,
) -> Vec<serde_json::Value> {
    let now = chrono::Utc::now();
    candidates
        .iter()
        .filter(|c| c.model.enabled && c.model.listed && is_listed_model_type(&c.model.model_type))
        .filter(|c| model_visible(allowed, &c.model.model_name))
        .filter(|c| c.model.lifecycle.status_at(now).listed())
        .map(|c| model_info_entry(&c.model, c.healthy, boons))
        .collect()
}

/// One `/model/info` entry. Split out so the payload shape is unit-testable
/// without a registry or a request.
pub(super) fn model_info_entry(
    model: &ResolvedModel,
    healthy: bool,
    boons: &obleth_config::BoonSettings,
) -> serde_json::Value {
    let status = model.lifecycle.status_at(chrono::Utc::now());
    let mut entry = serde_json::json!({
        "model_name": model.model_name,
        "obleth_params": {
            // The name the backend serves, which is where a quantization
            // suffix legitimately lives — the client-facing name above stays
            // clean regardless of how this deployment is built.
            "model": model.upstream_model,
        },
        "model_info": {
            // Conventional keys kept under their usual names, so a client
            // written against the LiteLLM shape reads them as-is.
            "mode": mode_for_model_type(&model.model_type),
            "max_input_tokens": model.context_window,
            "input_cost_per_token": model.input_cost_per_token,
            "output_cost_per_token": model.output_cost_per_token,
            "supports_function_calling": model.supports_function_calling,
            "supports_system_messages": model.supports_system_messages,
            "supports_response_schema": model.supports_response_schema,
            "supports_vision": model.supports_vision,
            // obleth's own additions.
            "model_type": model.model_type,
            "quantization": model.quantization,
            "aliases": model.aliases,
            // Names that serve this model with more boons on.
            "variants": model.variants,
            "tags": model.tags,
            "boons": model.boons,
            // What each boon does here, and which model does it.
            "boon_details": boon_details(model, &model.boons, boons),
            "supports_tool_choice": model.supports_tool_choice,
            "context_window": model.context_window,
            "cost_per_image": model.cost_per_image,
            "cost_per_audio_second": model.cost_per_audio_second,
            "cost_per_character": model.cost_per_character,
            "cost_per_video": model.cost_per_video,
            // Health as the gateway last observed it. False means the model is
            // registered and addressable but currently failing its probe or
            // held in a maintenance window.
            "healthy": healthy,
            "status": status.as_str(),
        },
    });
    if let Some(r) = reasoning_fields(&model.reasoning) {
        entry["model_info"]["reasoning"] = r;
    }
    if status != obleth_config::ModelStatus::Active {
        if let Some(info) = entry["model_info"].as_object_mut() {
            let mut fields = lifecycle_fields(status, &model.lifecycle);
            fields.remove("status");
            info.extend(fields);
        }
    }
    entry
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::proxy::tests::{candidate, model_with, with_variants};

    /// Gateway settings with every boon on and each helper pointed at a model.
    fn boons_configured() -> obleth_config::BoonSettings {
        let mut s = obleth_config::BoonSettings::default();
        s.vision.enabled = true;
        s.vision.fallback_model = Some("describer".into());
        s.structured_output.enabled = true;
        s.image_generation.enabled = true;
        s.image_generation.image_model = Some("painter".into());
        s.web_search.enabled = true;
        s.web_search.search_tool = Some("web".into());
        s.speculation.enabled = true;
        s.speculation.draft_model = Some("drafter".into());
        s.compression.enabled = true;
        s.knowledge.enabled = true;
        s
    }

    #[test]
    fn models_listing_is_the_registry_under_client_facing_names() {
        use super::registry_models_list;
        let mut disabled = candidate("retired", "retired", "chat", "none", &[], &[]);
        Arc::make_mut(&mut disabled.model).enabled = false;
        let candidates = vec![
            // A backend that knows itself only by its quantized name, with an
            // old spelling kept as an alias. Whether or not that backend
            // serves a catalog, the route is listed once, as `glm-5-3`.
            candidate(
                "glm-5-3",
                "glm-5-3-mxfp4",
                "chat",
                "mxfp4",
                &["glm-5-3-fp8"],
                &["coding"],
            ),
            candidate(
                "image-model",
                "vendor/image-model",
                "image",
                "bf16",
                &[],
                &["creative"],
            ),
            disabled,
        ];
        let list = registry_models_list(&candidates, None, &Default::default());
        assert_eq!(list["object"], "list");
        let data = list["data"].as_array().unwrap();
        let ids: Vec<&str> = data.iter().map(|m| m["id"].as_str().unwrap()).collect();
        // Sorted, enabled routes only, and never a backend id.
        assert_eq!(ids, vec!["glm-5-3", "image-model"]);

        let glm = &data[0];
        assert_eq!(glm["object"], "model");
        assert_eq!(glm["owned_by"], "obleth");
        assert_eq!(glm["quantization"], "mxfp4");
        assert_eq!(glm["tags"], serde_json::json!(["coding"]));
        // The old name is advertised as an alias, so a client that had pinned
        // it can see where it went instead of finding it simply gone.
        assert_eq!(glm["aliases"], serde_json::json!(["glm-5-3-fp8"]));
        // Both the obleth vocabulary and the LiteLLM-convention alias.
        assert_eq!(data[1]["model_type"], "image");
        assert_eq!(data[1]["mode"], "image_generation");
        assert_eq!(glm["mode"], "chat");
        assert!(!list.to_string().contains("glm-5-3-mxfp4"));
    }

    #[test]
    fn search_tools_are_not_listed_as_models() {
        use super::{model_facts_index, registry_models_list};
        let candidates = vec![
            candidate("glm-5-3", "glm-5-3", "chat", "none", &[], &[]),
            candidate("web-search", "searxng", "search", "unknown", &[], &[]),
        ];
        let ids: Vec<String> = registry_models_list(&candidates, None, &Default::default())["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(ids, vec!["glm-5-3"]);
        // Nor found by a detail lookup, under its name or its upstream's.
        let index = model_facts_index(&candidates, &Default::default());
        assert!(!index.contains_key("web-search"));
        assert!(!index.contains_key("searxng"));
    }

    #[test]
    fn a_listing_entry_and_the_detail_lookup_agree() {
        use super::{model_entry, model_facts_index, registry_models_list};
        let candidates = vec![candidate(
            "glm-5-3",
            "glm-5-3-mxfp4",
            "chat",
            "mxfp4",
            &["glm-5-3-fp8"],
            &[],
        )];
        let listed =
            registry_models_list(&candidates, None, &Default::default())["data"][0].clone();
        let detail =
            model_entry(&model_facts_index(&candidates, &Default::default())["glm-5-3-fp8"]);
        assert_eq!(listed, detail);
    }

    #[test]
    fn the_models_listing_shows_a_tenant_only_its_allowed_models() {
        use super::{model_visible, registry_models_list};
        let candidates = vec![
            candidate(
                "glm-5-3",
                "glm-5-3-mxfp4",
                "chat",
                "mxfp4",
                &["glm-5-3-fp8"],
                &[],
            ),
            candidate(
                "image-model",
                "vendor/image-model",
                "image",
                "bf16",
                &[],
                &[],
            ),
        ];
        let allowed = vec!["glm-5-3".to_string()];
        let list = registry_models_list(&candidates, Some(&allowed), &Default::default());
        let ids: Vec<&str> = list["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["glm-5-3"]);
        // No allowlist, or an internal caller, sees every enabled route.
        assert_eq!(
            registry_models_list(&candidates, None, &Default::default())["data"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        // Visibility is decided on the canonical name an alias resolves to.
        assert!(model_visible(Some(&allowed), "glm-5-3"));
        assert!(!model_visible(Some(&allowed), "image-model"));
        assert!(!model_visible(Some(&[]), "glm-5-3"));
    }

    #[test]
    fn the_models_listing_makes_no_upstream_calls() {
        // Served from the registry snapshot: no per-upstream fan-out, so an
        // upstream without a catalog (or one that is down) cannot shrink it.
        let src = include_str!("discovery.rs");
        let src = &src[..src.find("\nmod tests {").expect("the test module")];
        let start = src.find("fn models_list_response(").unwrap();
        let end = start + src[start..].find("\n}\n").unwrap();
        let body = &src[start..end];
        assert!(!body.contains("http"), "{body}");
        assert!(!body.contains(".await"), "{body}");
    }

    #[test]
    fn model_facts_index_prefers_the_client_facing_name() {
        use super::model_facts_index;
        // A pathological-but-legal fleet: one model's `upstream_model` is
        // another model's client-facing `model_name`. Resolution matches on
        // `model_name`, so the index has to agree with it.
        let candidates = vec![
            candidate("shared-name", "shared-name-fp8", "chat", "fp8", &[], &[]),
            candidate("other", "shared-name", "chat", "none", &[], &[]),
        ];
        let index = model_facts_index(&candidates, &Default::default());
        assert_eq!(index["shared-name"].model_name, "shared-name");
        assert_eq!(index["shared-name-fp8"].model_name, "shared-name");
        assert_eq!(index["other"].model_name, "other");
    }

    #[test]
    fn retired_models_leave_the_listing_and_deprecated_ones_are_marked() {
        use super::{model_entry, model_facts_index, model_info_entry, registry_models_list};
        let now = chrono::Utc::now();
        let mut old = candidate("glm-4-5v", "glm-4.5v", "chat", "fp8", &[], &[]);
        Arc::make_mut(&mut old.model).lifecycle = obleth_config::ModelLifecycle {
            status: obleth_config::ModelStatus::Deprecated,
            replacement: "gemma4-31b-it".into(),
            retire_at: Some(now + chrono::Duration::days(14)),
            ..Default::default()
        };
        let mut gone = candidate("llama2-70b", "llama2", "chat", "fp16", &[], &[]);
        Arc::make_mut(&mut gone.model).lifecycle.status = obleth_config::ModelStatus::Retired;
        Arc::make_mut(&mut gone.model).variants = vec![obleth_config::ModelVariant {
            name: "llama2-70b-spec".into(),
            ..Default::default()
        }];
        let mut overdue = candidate("old-overdue", "x", "chat", "fp8", &[], &[]);
        Arc::make_mut(&mut overdue.model).lifecycle = obleth_config::ModelLifecycle {
            status: obleth_config::ModelStatus::Deprecated,
            retire_at: Some(now - chrono::Duration::hours(1)),
            ..Default::default()
        };
        let active = candidate("gemma4-31b-it", "gemma", "chat", "bf16", &[], &[]);
        let candidates = vec![old, gone, overdue, active];

        let list = registry_models_list(&candidates, None, &Default::default());
        let data = list["data"].as_array().unwrap();
        let ids: Vec<&str> = data.iter().map(|m| m["id"].as_str().unwrap()).collect();
        // Retired (by status or by date) is gone, variants included.
        assert_eq!(ids, vec!["gemma4-31b-it", "glm-4-5v"]);
        let dep = &data[1];
        assert_eq!(dep["status"], "deprecated");
        assert_eq!(dep["replacement"], "gemma4-31b-it");
        assert!(dep["retire_at"].is_string());
        // An active model's entry is unchanged.
        assert!(data[0].get("status").is_none());

        // Asking for a retired model by id still says what became of it.
        let detail =
            model_entry(&model_facts_index(&candidates, &Default::default())["llama2-70b"]);
        assert_eq!(detail["status"], "retired");

        let info = model_info_entry(&candidates[0].model, true, &Default::default());
        assert_eq!(info["model_info"]["status"], "deprecated");
        assert_eq!(info["model_info"]["replacement"], "gemma4-31b-it");
        let info = model_info_entry(&candidates[3].model, true, &Default::default());
        assert_eq!(info["model_info"]["status"], "active");
        assert!(info["model_info"].get("replacement").is_none());
    }

    #[test]
    fn the_load_listing_names_what_the_models_listing_lists() {
        use super::{family_entries, listed_pools, registry_models_list};
        let mut glm = candidate("glm", "glm-fp8", "chat", "fp8", &["glm-old"], &[]);
        Arc::make_mut(&mut glm.model).variants = vec![obleth_config::ModelVariant {
            name: "glm-search".into(),
            ..Default::default()
        }];
        let mut staged = candidate("staged", "staged", "chat", "fp8", &[], &[]);
        Arc::make_mut(&mut staged.model).lifecycle.status = obleth_config::ModelStatus::Staged;
        let mut retired = candidate("retired", "retired", "chat", "fp8", &[], &[]);
        Arc::make_mut(&mut retired.model).lifecycle.status = obleth_config::ModelStatus::Retired;
        let mut unlisted = candidate("unlisted", "unlisted", "chat", "fp8", &[], &[]);
        Arc::make_mut(&mut unlisted.model).listed = false;
        let search = candidate(
            "web",
            "searxng",
            obleth_config::SEARCH_MODEL_TYPE,
            "none",
            &[],
            &[],
        );
        let other = candidate("other", "other", "embedding", "none", &[], &[]);
        let registry = crate::router::ModelRegistry::new();
        registry.store(vec![glm, staged, retired, unlisted, search, other]);
        let families = crate::reasoning::FamilyRegistry::new();
        families.store(vec![obleth_config::ModelFamily {
            id: uuid::Uuid::nil(),
            name: "fam".into(),
            description: String::new(),
            default_level: obleth_config::ReasoningLevel::None,
            members: obleth_config::FamilyMembers {
                none: "unlisted".into(),
                low: "glm".into(),
                medium: "glm".into(),
                high: "glm".into(),
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }]);

        let allowlists = [
            None,
            Some(vec!["glm-search".to_string(), "other".to_string()]),
            Some(vec!["glm".to_string(), "fam".to_string()]),
        ];
        for allowed in allowlists {
            let mut key = crate::boons::test_support::test_key();
            key.allowed_models = allowed.clone();
            let candidates = registry.load();
            let mut ids: Vec<String> =
                registry_models_list(&candidates, allowed.as_deref(), &Default::default())["data"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .chain(
                        family_entries(
                            &families.all(),
                            &candidates,
                            allowed.as_deref(),
                            &Default::default(),
                        )
                        .iter(),
                    )
                    .map(|e| e["id"].as_str().unwrap().to_string())
                    .collect();
            ids.sort();
            let pools = listed_pools(&registry, &families, &key);
            let listed: Vec<String> = pools.iter().map(|(id, _)| id.clone()).collect();
            assert_eq!(listed, ids, "{allowed:?}");
        }

        // A variant counts in its model's pool, a family in its default
        // member's.
        let pools = listed_pools(
            &registry,
            &families,
            &crate::boons::test_support::test_key(),
        );
        assert_eq!(
            pools,
            [
                ("fam", "unlisted"),
                ("glm", "glm"),
                ("glm-search", "glm"),
                ("other", "other"),
            ]
            .map(|(id, pool)| (id.to_string(), pool.to_string()))
        );
    }

    #[test]
    fn staged_models_are_served_by_name_but_never_advertised() {
        use super::{model_facts_index, registry_model_info, registry_models_list};
        let mut staged = candidate("qwen4-72b", "qwen4", "chat", "fp8", &["qwen4"], &[]);
        Arc::make_mut(&mut staged.model).lifecycle.status = obleth_config::ModelStatus::Staged;
        Arc::make_mut(&mut staged.model).variants = vec![obleth_config::ModelVariant {
            name: "qwen4-72b-spec".into(),
            ..Default::default()
        }];
        let live = candidate("gemma4-31b-it", "gemma", "chat", "bf16", &[], &[]);
        let candidates = vec![staged, live];

        // Not in the listing, variants included, whatever the allowlist says.
        for allowed in [None, Some(&["qwen4-72b".to_string()][..])] {
            let list = registry_models_list(&candidates, allowed, &Default::default());
            let ids: Vec<&str> = list["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["id"].as_str().unwrap())
                .collect();
            assert!(!ids.iter().any(|id| id.starts_with("qwen4")), "{ids:?}");
        }
        // Not on /model/info.
        let info = registry_model_info(&candidates, None, &Default::default());
        let names: Vec<&str> = info
            .iter()
            .map(|m| m["model_name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["gemma4-31b-it"]);
        // A lookup by any of its names answers like an unknown model.
        let index = model_facts_index(&candidates, &Default::default());
        for id in ["qwen4-72b", "qwen4", "qwen4-72b-spec"] {
            assert!(!index[id].answers_by_id(None), "{id}");
        }
        assert!(index["gemma4-31b-it"].answers_by_id(None));
    }

    #[test]
    fn unlisted_models_are_left_out_of_the_listings_but_answer_by_id() {
        use super::{family_entries, model_facts_index, registry_model_info, registry_models_list};
        let mut instruct = candidate(
            "acme-instruct",
            "acme-i",
            "chat",
            "fp8",
            &["acme-instruct-2507"],
            &[],
        );
        Arc::make_mut(&mut instruct.model).listed = false;
        let mut think = candidate("acme-think", "acme-t", "chat", "fp8", &[], &[]);
        Arc::make_mut(&mut think.model).listed = false;
        let other = candidate("gemma4-31b-it", "gemma", "chat", "bf16", &[], &[]);
        let candidates = vec![instruct, think, other];

        let list = registry_models_list(&candidates, None, &Default::default());
        let ids: Vec<&str> = list["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, vec!["gemma4-31b-it"]);
        let info = registry_model_info(&candidates, None, &Default::default());
        assert_eq!(info.len(), 1);
        // Still a model the caller can ask about, by any of its names.
        let index = model_facts_index(&candidates, &Default::default());
        for id in ["acme-instruct", "acme-instruct-2507", "acme-think"] {
            assert!(index[id].answers_by_id(None), "{id}");
        }

        // A family standing for unlisted members is listed itself.
        let family = std::sync::Arc::new(obleth_config::ModelFamily {
            id: uuid::Uuid::nil(),
            name: "acme".into(),
            description: String::new(),
            default_level: obleth_config::ReasoningLevel::None,
            members: obleth_config::FamilyMembers {
                none: "acme-instruct".into(),
                low: "acme-think".into(),
                medium: "acme-think".into(),
                high: "acme-think".into(),
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        });
        let entries = family_entries(&[family], &candidates, None, &Default::default());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0]["id"], "acme");
        assert_eq!(entries[0]["family"]["members"]["high"], "acme-think");
        // A catalog can describe the family from its entry alone: the facts
        // are the default member's.
        assert_eq!(
            entries[0]["context_window"],
            candidates[0].model.context_window
        );
        assert_eq!(
            entries[0]["supports_function_calling"],
            candidates[0].model.supports_function_calling
        );
        // Model entries carry them too.
        let listed = &list["data"][0];
        assert_eq!(listed["context_window"], candidates[2].model.context_window);
        assert!(listed.get("input_cost_per_token").is_some());
        assert!(listed.get("description").is_some());
    }

    #[test]
    fn model_info_entry_uses_obleth_params_and_keeps_the_conventional_keys() {
        use super::model_info_entry;
        let mut model = model_with(Vec::new());
        model.model_name = "glm-5-3".into();
        model.upstream_model = "glm-5-3-mxfp4".into();
        model.api_base = "http://glm-5-3.internal.svc:8000/v1".into();
        model.api_key = Some("sk-secret".into());
        model.quantization = "mxfp4".into();
        model.aliases = vec!["glm-5-3-mxfp4".into()];
        model.tags = vec!["coding".into()];
        model.context_window = 200_000;

        let entry = model_info_entry(&model, false, &Default::default());
        assert_eq!(entry["model_name"], "glm-5-3");
        // The per-route object is ours; the upstream name lives in it.
        assert_eq!(entry["obleth_params"]["model"], "glm-5-3-mxfp4");
        // The old LiteLLM-branded key is gone, not duplicated.
        assert!(entry.get("litellm_params").is_none());
        assert_eq!(entry["model_info"]["mode"], "chat");
        assert_eq!(entry["model_info"]["max_input_tokens"], 200_000);
        // obleth's additions, including the format that used to live in the name.
        assert_eq!(entry["model_info"]["quantization"], "mxfp4");
        assert_eq!(
            entry["model_info"]["aliases"],
            serde_json::json!(["glm-5-3-mxfp4"])
        );
        assert_eq!(entry["model_info"]["tags"], serde_json::json!(["coding"]));
        // Registered but failing its probe: addressable, and honestly labelled.
        assert_eq!(entry["model_info"]["healthy"], false);
        // A tenant-facing endpoint never reports how to reach the backend.
        let rendered = entry.to_string();
        assert!(!rendered.contains("sk-secret"), "{rendered}");
        assert!(!rendered.contains("internal.svc"), "{rendered}");
    }

    #[test]
    fn model_detail_answers_for_the_clean_name_and_its_aliases() {
        use super::model_facts_index;
        // The lookup `registered_model_entry` performs, without an AppState:
        // the detail probe has to accept the clean name the listing publishes
        // and the aliases that still resolve, or a client following the
        // listing would ask for an id the gateway itself advertised and 404.
        let candidates = vec![candidate(
            "glm-5-3",
            "glm-5-3-mxfp4",
            "chat",
            "mxfp4",
            &["glm-5-3-fp8"],
            &[],
        )];
        let index = model_facts_index(&candidates, &Default::default());
        for id in ["glm-5-3", "glm-5-3-fp8", "glm-5-3-mxfp4"] {
            assert_eq!(
                index.get(id).map(|f| f.model_name.as_str()),
                Some("glm-5-3"),
                "{id}"
            );
        }
        // An unclaimed id still has no entry, so the request falls through to
        // the upstream passthrough rather than being answered with a guess.
        assert!(!index.contains_key("wildcard-passthrough"));
    }

    #[test]
    fn models_listing_lists_each_variant_as_its_own_entry() {
        use super::{model_entry, model_facts_index, registry_models_list};
        let candidates = vec![with_variants(candidate(
            "glm-5-3",
            "glm-5-3-mxfp4",
            "chat",
            "mxfp4",
            &["glm-5-3-fp8"],
            &["coding"],
        ))];
        let list = registry_models_list(&candidates, None, &Default::default());
        let data = list["data"].as_array().unwrap();
        let ids: Vec<&str> = data.iter().map(|m| m["id"].as_str().unwrap()).collect();
        // Aliases stay out of the listing; a variant is listed by its name.
        assert_eq!(ids, vec!["glm-5-3", "glm-5-3-spec"]);
        let spec = &data[1];
        assert_eq!(spec["variant_of"], "glm-5-3");
        assert_eq!(
            spec["description"],
            "Drafted by a small model, verified by this one"
        );
        assert_eq!(spec["boons"], serde_json::json!(["vision", "speculation"]));
        assert_eq!(spec["mode"], "chat");
        assert_eq!(spec["quantization"], "mxfp4");
        assert_eq!(spec["aliases"], serde_json::json!([]));
        // The parent's own entry is unchanged by having variants.
        assert!(data[0].get("variant_of").is_none());
        // A detail lookup by the variant's name answers with its entry.
        let detail =
            model_entry(&model_facts_index(&candidates, &Default::default())["glm-5-3-spec"]);
        assert_eq!(&detail, spec);
    }

    #[test]
    fn a_variant_is_listed_to_tenants_allowed_its_model_or_its_name() {
        use super::registry_models_list;
        let candidates = vec![with_variants(candidate(
            "glm-5-3",
            "glm-5-3",
            "chat",
            "none",
            &[],
            &[],
        ))];
        let ids = |allowed: &[&str]| -> Vec<String> {
            let allowed: Vec<String> = allowed.iter().map(|a| a.to_string()).collect();
            registry_models_list(&candidates, Some(&allowed), &Default::default())["data"]
                .as_array()
                .unwrap()
                .iter()
                .map(|m| m["id"].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids(&["glm-5-3"]), vec!["glm-5-3", "glm-5-3-spec"]);
        assert_eq!(ids(&["glm-5-3-spec"]), vec!["glm-5-3-spec"]);
        assert!(ids(&["other"]).is_empty());
    }

    #[test]
    fn a_lent_ability_names_the_model_that_lends_it() {
        // A text-only model granted vision. Its entry has to say that the
        // model cannot see, that the gateway lends it sight, and which model
        // does the looking: an answer about an image is that model's
        // description read back, which the model alone does not reproduce.
        use super::{model_entry, model_facts_index, registry_model_info};
        let mut c = candidate("text-only", "text-only", "chat", "none", &[], &[]);
        Arc::make_mut(&mut c.model).boons = vec!["vision".into()];
        let candidates = vec![c];
        let s = boons_configured();
        let detail = model_entry(&model_facts_index(&candidates, &s)["text-only"]);
        assert_eq!(detail["supports_vision"], false);
        assert_eq!(detail["boons"], serde_json::json!(["vision"]));
        assert_eq!(
            detail["boon_details"],
            serde_json::json!({
                "vision": {"active": true, "native": false, "helper_model": "describer"}
            })
        );
        // `/model/info` says the same.
        let info = &registry_model_info(&candidates, None, &s)[0]["model_info"];
        assert_eq!(info["boon_details"], detail["boon_details"]);
    }

    #[test]
    fn a_model_with_no_boons_says_so() {
        use super::{model_entry, model_facts_index, registry_model_info};
        let candidates = vec![candidate("plain", "plain", "chat", "none", &[], &[])];
        let s = boons_configured();
        let detail = model_entry(&model_facts_index(&candidates, &s)["plain"]);
        assert_eq!(detail["boons"], serde_json::json!([]));
        assert_eq!(detail["boon_details"], serde_json::json!({}));
        let info = &registry_model_info(&candidates, None, &s)[0]["model_info"];
        assert_eq!(info["boon_details"], serde_json::json!({}));
    }

    #[test]
    fn an_idle_boon_grant_says_why_and_names_no_helper() {
        use super::boon_details;
        use serde_json::json;
        let s = boons_configured();
        let mut model = Arc::unwrap_or_clone(candidate("m", "m", "chat", "none", &[], &[]).model);
        model.boons = [
            "vision",
            "structured_output",
            "image_generation",
            "web_search",
            "speculation",
            "knowledge",
            "compression",
        ]
        .map(String::from)
        .to_vec();
        model.supports_vision = true;
        model.supports_response_schema = true;
        model.supports_function_calling = false;
        let d = boon_details(&model, &model.boons, &s);
        // The model has the ability itself, so the boon steps aside.
        assert_eq!(
            d["vision"],
            json!({"active": false, "native": true, "inactive_reason": "native"})
        );
        assert_eq!(
            d["structured_output"],
            json!({"active": false, "native": true, "inactive_reason": "native"})
        );
        // A tool boon needs a model that can call its tool.
        assert_eq!(
            d["image_generation"],
            json!({"active": false, "inactive_reason": "needs_function_calling"})
        );
        assert_eq!(
            d["web_search"],
            json!({"active": false, "inactive_reason": "needs_function_calling"})
        );
        // A drafter is set, but nothing can score its drafts.
        assert_eq!(
            d["speculation"],
            json!({"active": false, "inactive_reason": "no_scoring_endpoint"})
        );
        assert_eq!(
            d["knowledge"],
            json!({"active": false, "inactive_reason": "no_collections"})
        );
        // Compression has no helper; the gateway does it alone.
        assert_eq!(d["compression"], json!({"active": true}));

        // Switched off gateway-wide, every grant is idle.
        let d = boon_details(&model, &model.boons, &Default::default());
        for (boon, detail) in &d {
            if detail.get("native") == Some(&json!(true)) {
                continue;
            }
            assert_eq!(detail["inactive_reason"], "off", "{boon}");
        }
    }

    #[test]
    fn an_active_boon_names_its_helper() {
        use super::boon_details;
        use serde_json::json;
        let s = boons_configured();
        let mut model = Arc::unwrap_or_clone(candidate("m", "m", "chat", "none", &[], &[]).model);
        model.boons = [
            "structured_output",
            "image_generation",
            "web_search",
            "speculation",
        ]
        .map(String::from)
        .to_vec();
        model.supports_response_schema = false;
        model.supports_function_calling = true;
        model.verify_api_base = "http://scorer:8000/v1".into();
        model.draft_model = "own-drafter".into();
        let d = boon_details(&model, &model.boons, &s);
        // No fixer is set: an invalid reply goes back to the model itself.
        assert_eq!(
            d["structured_output"],
            json!({"active": true, "native": false})
        );
        assert_eq!(
            d["image_generation"],
            json!({"active": true, "helper_model": "painter"})
        );
        assert_eq!(
            d["web_search"],
            json!({"active": true, "helper_model": "web"})
        );
        // The model's own drafter wins over the gateway default.
        assert_eq!(
            d["speculation"],
            json!({"active": true, "helper_model": "own-drafter"})
        );

        let mut s = s;
        s.structured_output.fixer_model = Some("fixer".into());
        let d = boon_details(&model, &model.boons, &s);
        assert_eq!(
            d["structured_output"],
            json!({"active": true, "native": false, "helper_model": "fixer"})
        );
    }

    #[test]
    fn a_variant_details_the_boons_it_adds() {
        use super::{model_entry, model_facts_index};
        let mut c = with_variants(candidate("glm-5-3", "glm-5-3", "chat", "none", &[], &[]));
        Arc::make_mut(&mut c.model).verify_api_base = "http://scorer:8000/v1".into();
        let candidates = vec![c];
        let s = boons_configured();
        let index = model_facts_index(&candidates, &s);
        let parent = model_entry(&index["glm-5-3"]);
        let spec = model_entry(&index["glm-5-3-spec"]);
        assert_eq!(parent["boons"], serde_json::json!(["vision"]));
        assert!(parent["boon_details"].get("speculation").is_none());
        assert_eq!(spec["boons"], serde_json::json!(["vision", "speculation"]));
        assert_eq!(
            spec["boon_details"]["speculation"],
            serde_json::json!({"active": true, "helper_model": "drafter"})
        );
        assert_eq!(
            spec["boon_details"]["vision"],
            parent["boon_details"]["vision"]
        );
    }
}
