//! `auto` model selection.
//!
//! When a client sends `model: "auto"`, the gateway chooses a concrete
//! registered model instead of routing to a fixed upstream. Selection is a
//! two-stage process:
//!
//! 1. **Hard filters** remove models that cannot serve the request at all:
//!    disabled, unhealthy / in maintenance, too small a context window, or
//!    missing a required capability (tools / tool_choice / JSON schema). A
//!    per-tenant model allowlist, when present, is also enforced here.
//! 2. **Scoring** ranks the survivors by spare capacity (prefer models that
//!    are not busy) and cost (prefer cheaper models), then picks the best with
//!    a deterministic tie-break on model name.
//!
//! The scoring function [`select_model`] is pure and synchronous so it can be
//! unit-tested without any of the data-plane wiring.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;
use obleth_config::ResolvedModel;

/// Reserved client-facing model name that triggers auto selection.
pub const AUTO_MODEL_NAME: &str = "auto";

/// Borrowed view of the scoring knobs, so `select_model` stays pure and can be
/// called with ad-hoc values by the simulate endpoint.
#[derive(Debug, Clone, Copy)]
pub struct RouterWeights {
    pub capacity: f64,
    pub cost: f64,
    pub tag: f64,
    pub soft_cap: f64,
    // Not yet consumed by `select_model`: softmax sampling and the difficulty
    // tier filter land in later steps of the auto-router-tuner plan, which is
    // why these are threaded through now instead of being added alongside
    // their own consumers.
    #[allow(dead_code)]
    pub temperature: f64,
    #[allow(dead_code)]
    pub difficulty_enabled: bool,
}

impl RouterWeights {
    pub fn from_settings(s: &obleth_config::AutoRouterSettings) -> Self {
        RouterWeights {
            capacity: s.capacity_weight,
            cost: s.cost_weight,
            tag: s.tag_weight,
            soft_cap: if s.default_soft_cap > 0 {
                s.default_soft_cap as f64
            } else {
                8.0
            },
            temperature: s.temperature.max(0.0),
            difficulty_enabled: s.difficulty_enabled,
        }
    }
}

impl Default for RouterWeights {
    fn default() -> Self {
        RouterWeights::from_settings(&obleth_config::AutoRouterSettings::default())
    }
}

/// A model the `auto` router may choose from, plus the liveness signal the
/// router needs that is not part of [`ResolvedModel`].
#[derive(Debug, Clone)]
pub struct Candidate {
    pub model: ResolvedModel,
    /// `false` when the model is reported down or is inside a maintenance
    /// window. Unhealthy candidates are filtered out before scoring.
    pub healthy: bool,
}

/// Lock-free, hot-swappable list of auto-routing candidates. Reads clone an
/// `Arc` to the current snapshot; refreshes atomically replace the whole list.
#[derive(Clone)]
pub struct ModelRegistry {
    inner: Arc<ArcSwap<Vec<Candidate>>>,
}

impl Default for ModelRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelRegistry {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ArcSwap::from_pointee(Vec::new())),
        }
    }

    /// Current candidate snapshot. Cheap; clones a single `Arc`.
    pub fn load(&self) -> Arc<Vec<Candidate>> {
        self.inner.load_full()
    }

    /// Atomically replace the candidate list (used on boot and on refresh).
    pub fn store(&self, candidates: Vec<Candidate>) {
        self.inner.store(Arc::new(candidates));
    }
}

/// Properties of a request that constrain which models can serve it.
#[derive(Debug, Clone, Default)]
pub struct RequestFeatures {
    /// Estimated prompt tokens.
    pub est_input_tokens: u64,
    /// Requested completion budget (`max_tokens`), 0 when unspecified.
    pub max_tokens: u64,
    pub needs_function_calling: bool,
    pub needs_tool_choice: bool,
    pub needs_response_schema: bool,
}

impl RequestFeatures {
    /// Derive capability requirements from an OpenAI-style request body. Token
    /// counts are supplied separately because they come from the tokenizer.
    pub fn from_request(json: &serde_json::Value, est_input_tokens: u64, max_tokens: u64) -> Self {
        let needs_function_calling = json
            .get("tools")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty())
            || json
                .get("functions")
                .and_then(|v| v.as_array())
                .is_some_and(|a| !a.is_empty());

        let needs_tool_choice = match json.get("tool_choice") {
            Some(serde_json::Value::String(s)) => s != "none" && s != "auto",
            Some(serde_json::Value::Object(_)) => true,
            _ => json
                .get("function_call")
                .is_some_and(|v| !v.is_null() && v.as_str() != Some("none")),
        };

        let needs_response_schema = json
            .get("response_format")
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            .is_some_and(|t| t == "json_schema" || t == "json_object");

        Self {
            est_input_tokens,
            max_tokens,
            needs_function_calling,
            needs_tool_choice,
            needs_response_schema,
        }
    }
}

/// Which boon-granted capabilities are currently active gateway-wide. A model
/// that carries the matching boon counts as capable in the hard filters below,
/// because the boon engine will emulate the capability at dispatch time.
#[derive(Debug, Clone, Copy, Default)]
pub struct BoonGrants {
    /// The structured-output boon is enabled in settings: models with the
    /// `structured_output` boon can serve `response_format` requests.
    pub structured_active: bool,
}

impl BoonGrants {
    /// Snapshot the grants from the live boon settings.
    pub fn from_settings(settings: &obleth_config::BoonSettings) -> Self {
        BoonGrants {
            structured_active: settings.structured_output.active(),
        }
    }
}

/// Pick the best concrete model for an `auto` request, or `None` when no
/// registered model can serve it.
///
/// `desired_tags` are the intent tags derived for this request (by the
/// classifier or heuristics). When non-empty, candidates whose tags overlap
/// the desired set are preferred; when empty, selection is pure capacity/cost.
pub fn select_model(
    candidates: &[Candidate],
    features: &RequestFeatures,
    busyness: &HashMap<String, usize>,
    allowed_models: Option<&[String]>,
    desired_tags: &[String],
    grants: BoonGrants,
    weights: &RouterWeights,
) -> Option<ResolvedModel> {
    let required_context = features
        .est_input_tokens
        .saturating_add(features.max_tokens);

    // Capability via boon: the gateway emulates structured output for opted-in
    // models, so the hard filters must not exclude them. Function calling and
    // tool choice are native capabilities only — no boon emulates them.
    let schema_via_boon = |c: &Candidate| {
        grants.structured_active && c.model.boons.iter().any(|b| b == "structured_output")
    };

    // ---- stage 1: hard filters ----
    let eligible: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| c.model.enabled && c.healthy)
        // `auto` is a chat-completions convenience; only chat models are
        // eligible. Non-chat modalities (embedding, image, audio) are addressed
        // by name on their dedicated endpoints.
        .filter(|c| c.model.model_type == obleth_config::DEFAULT_MODEL_TYPE)
        .filter(|c| {
            // A non-positive context window means "unknown" (misconfigured or
            // legacy row); don't exclude on a signal we don't trust.
            c.model.context_window <= 0 || c.model.context_window as u64 >= required_context
        })
        .filter(|c| !features.needs_function_calling || c.model.supports_function_calling)
        .filter(|c| !features.needs_tool_choice || c.model.supports_tool_choice)
        .filter(|c| {
            !features.needs_response_schema
                || c.model.supports_response_schema
                || schema_via_boon(c)
        })
        .filter(|c| match allowed_models {
            Some(allowed) => allowed.iter().any(|m| m == &c.model.model_name),
            None => true,
        })
        .collect();

    if eligible.is_empty() {
        return None;
    }

    // ---- stage 2: scoring ----
    let costs: Vec<f64> = eligible
        .iter()
        .map(|c| c.model.input_cost_per_token + c.model.output_cost_per_token)
        .collect();
    let min_cost = costs.iter().copied().fold(f64::INFINITY, f64::min);
    let max_cost = costs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let cost_span = max_cost - min_cost;

    let mut best: Option<(&Candidate, f64)> = None;
    for (cand, cost) in eligible.iter().zip(costs.iter()) {
        let in_flight = busyness.get(&cand.model.model_name).copied().unwrap_or(0) as f64;
        let cap = match cand.model.max_in_flight {
            Some(cap) if cap > 0 => cap as f64,
            _ => weights.soft_cap,
        };
        let spare = (1.0 - (in_flight / cap)).clamp(0.0, 1.0);

        // Cheaper is better; if all costs are equal everyone scores 1.0.
        let cost_score = if cost_span > f64::EPSILON {
            1.0 - (cost - min_cost) / cost_span
        } else {
            1.0
        };

        let base = weights.capacity * spare + weights.cost * cost_score;
        // Layer intent-tag matching on top of the capacity/cost base. With no
        // desired tags the score is just the base (neutral routing).
        let score = if desired_tags.is_empty() {
            base
        } else {
            let overlap = desired_tags
                .iter()
                .filter(|t| cand.model.tags.iter().any(|mt| mt == *t))
                .count();
            let tag_score = (overlap as f64 / desired_tags.len() as f64).min(1.0);
            weights.tag * tag_score + (1.0 - weights.tag) * base
        };
        let better = match best {
            None => true,
            Some((cur, cur_score)) => {
                // Higher score wins; break ties deterministically by name.
                score > cur_score + f64::EPSILON
                    || ((score - cur_score).abs() <= f64::EPSILON
                        && cand.model.model_name < cur.model.model_name)
            }
        };
        if better {
            best = Some((cand, score));
        }
    }

    best.map(|(cand, _)| cand.model.clone())
}

/// Cheap, dependency-free intent tags derived from the request body. Used as a
/// fallback when the classifier is disabled, unconfigured, or unavailable so
/// `auto` routing still gets a useful tag signal without an extra model call.
///
/// Only emits tags from the fixed vocabulary. Returns an empty list when no
/// signal is detected, in which case [`select_model`] routes on capacity/cost.
pub fn heuristic_tags(json: &serde_json::Value, est_input_tokens: u64) -> Vec<String> {
    let mut text = String::new();
    let mut has_image = false;
    if let Some(messages) = json.get("messages").and_then(|m| m.as_array()) {
        for msg in messages {
            match msg.get("content") {
                Some(serde_json::Value::String(s)) => {
                    text.push_str(s);
                    text.push('\n');
                }
                Some(serde_json::Value::Array(parts)) => {
                    for part in parts {
                        match part.get("type").and_then(|t| t.as_str()) {
                            Some("image_url") => has_image = true,
                            _ => {
                                if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                                    text.push_str(t);
                                    text.push('\n');
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let lower = text.to_ascii_lowercase();

    let mut tags: Vec<String> = Vec::new();
    let mut push = |t: &str| {
        let t = t.to_string();
        if !tags.contains(&t) {
            tags.push(t);
        }
    };

    if has_image {
        push("vision");
    }

    let code_signal = text.contains("```")
        || [
            "function ",
            "def ",
            "class ",
            "import ",
            "select ",
            "public ",
            "const ",
            "=> ",
        ]
        .iter()
        .any(|k| lower.contains(k))
        || [
            "code",
            "compile",
            "stack trace",
            "bug",
            "python",
            "javascript",
            "typescript",
            "rust",
            "java",
            "sql",
            "regex",
        ]
        .iter()
        .any(|k| lower.contains(k));
    if code_signal {
        push("coding");
    }

    let math_signal = [
        "solve",
        "equation",
        "integral",
        "derivative",
        "theorem",
        "calculate",
        "probability",
        "algebra",
        "calculus",
        "matrix",
    ]
    .iter()
    .any(|k| lower.contains(k))
        || (lower.contains('=')
            && lower
                .chars()
                .any(|c| matches!(c, '+' | '-' | '*' | '/' | '^' | '∫' | '√' | '∑')));
    if math_signal {
        push("math");
    }

    if est_input_tokens > 32_000 {
        push("long-context");
    }

    tags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(name: &str) -> ResolvedModel {
        ResolvedModel {
            model_name: name.to_string(),
            upstream_model: name.to_string(),
            api_base: "http://upstream".to_string(),
            api_key: None,
            model_type: obleth_config::DEFAULT_MODEL_TYPE.to_string(),
            admission_weight: 100,
            max_in_flight: None,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 0,
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            context_window: 128_000,
            supports_function_calling: true,
            supports_system_messages: true,
            supports_response_schema: true,
            supports_tool_choice: true,
            supports_vision: false,
            tags: Vec::new(),
            boons: Vec::new(),
            tool_servers: Vec::new(),
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: obleth_config::DEFAULT_RETRY_BACKOFF_MS,
            endpoint_selection_mode: obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string(),
            debug_diagnostics: false,
            energy_slots_per_node: 0,
            endpoints: Vec::new(),
        }
    }

    fn healthy(m: ResolvedModel) -> Candidate {
        Candidate {
            model: m,
            healthy: true,
        }
    }

    #[test]
    fn empty_registry_returns_none() {
        let chosen = select_model(
            &[],
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
        );
        assert!(chosen.is_none());
    }

    #[test]
    fn context_overflow_is_filtered_out() {
        let mut small = model("small");
        small.context_window = 4_000;
        let mut large = model("large");
        large.context_window = 200_000;
        let candidates = vec![healthy(small), healthy(large)];
        let features = RequestFeatures {
            est_input_tokens: 100_000,
            max_tokens: 2_000,
            ..Default::default()
        };
        let chosen = select_model(
            &candidates,
            &features,
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
        )
        .unwrap();
        assert_eq!(chosen.model_name, "large");
    }

    #[test]
    fn no_candidate_fits_context_returns_none() {
        let mut small = model("small");
        small.context_window = 4_000;
        let candidates = vec![healthy(small)];
        let features = RequestFeatures {
            est_input_tokens: 100_000,
            max_tokens: 2_000,
            ..Default::default()
        };
        assert!(select_model(
            &candidates,
            &features,
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default()
        )
        .is_none());
    }

    #[test]
    fn capability_requirement_filters_models() {
        let mut plain = model("plain");
        plain.supports_function_calling = false;
        plain.supports_tool_choice = false;
        let tools = model("tools");
        let candidates = vec![healthy(plain), healthy(tools)];
        let features = RequestFeatures {
            needs_function_calling: true,
            ..Default::default()
        };
        let chosen = select_model(
            &candidates,
            &features,
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
        )
        .unwrap();
        assert_eq!(chosen.model_name, "tools");
    }

    #[test]
    fn unhealthy_candidates_are_skipped() {
        let mut down = Candidate {
            model: model("down"),
            healthy: false,
        };
        down.model.input_cost_per_token = 0.0; // would otherwise be cheapest
        let up = healthy(model("up"));
        let candidates = vec![down, up];
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
        )
        .unwrap();
        assert_eq!(chosen.model_name, "up");
    }

    #[test]
    fn cheaper_model_wins_when_capacity_equal() {
        let mut cheap = model("cheap");
        cheap.input_cost_per_token = 0.000_001;
        cheap.output_cost_per_token = 0.000_002;
        let mut pricey = model("pricey");
        pricey.input_cost_per_token = 0.000_010;
        pricey.output_cost_per_token = 0.000_020;
        let candidates = vec![healthy(pricey), healthy(cheap)];
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
        )
        .unwrap();
        assert_eq!(chosen.model_name, "cheap");
    }

    #[test]
    fn busy_model_is_avoided() {
        let mut a = model("a");
        a.max_in_flight = Some(4);
        let mut b = model("b");
        b.max_in_flight = Some(4);
        let candidates = vec![healthy(a), healthy(b)];
        let mut busyness = HashMap::new();
        busyness.insert("a".to_string(), 4); // fully saturated
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &busyness,
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
        )
        .unwrap();
        assert_eq!(chosen.model_name, "b");
    }

    #[test]
    fn allowlist_restricts_candidates() {
        let candidates = vec![healthy(model("a")), healthy(model("b"))];
        let allowed = vec!["b".to_string()];
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            Some(&allowed),
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
        )
        .unwrap();
        assert_eq!(chosen.model_name, "b");
    }

    #[test]
    fn features_detect_tools_and_schema() {
        let body = serde_json::json!({
            "model": "auto",
            "tools": [{"type": "function", "function": {"name": "x"}}],
            "tool_choice": {"type": "function", "function": {"name": "x"}},
            "response_format": {"type": "json_schema", "json_schema": {"name": "y"}}
        });
        let f = RequestFeatures::from_request(&body, 10, 100);
        assert!(f.needs_function_calling);
        assert!(f.needs_tool_choice);
        assert!(f.needs_response_schema);
    }

    #[test]
    fn features_ignore_auto_tool_choice() {
        let body = serde_json::json!({ "model": "auto", "tool_choice": "auto" });
        let f = RequestFeatures::from_request(&body, 10, 0);
        assert!(!f.needs_tool_choice);
    }

    #[test]
    fn desired_tags_prefer_matching_model() {
        // Two equal-capacity, equal-cost models; only the tag match differs.
        let mut coder = model("coder");
        coder.tags = vec!["coding".to_string(), "fast".to_string()];
        let mut writer = model("writer");
        writer.tags = vec!["creative".to_string()];
        let candidates = vec![healthy(writer), healthy(coder)];
        let desired = vec!["coding".to_string()];
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &desired,
            BoonGrants::default(),
            &RouterWeights::default(),
        )
        .unwrap();
        assert_eq!(chosen.model_name, "coder");
    }

    #[test]
    fn empty_desired_tags_falls_back_to_cost() {
        let mut cheap = model("cheap");
        cheap.input_cost_per_token = 0.000_001;
        cheap.tags = vec!["creative".to_string()];
        let mut pricey = model("pricey");
        pricey.input_cost_per_token = 0.000_100;
        pricey.tags = vec!["coding".to_string()];
        let candidates = vec![healthy(pricey), healthy(cheap)];
        // No desired tags -> tag list is ignored, cheapest wins.
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
        )
        .unwrap();
        assert_eq!(chosen.model_name, "cheap");
    }

    #[test]
    fn heuristic_tags_detect_code_and_vision() {
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": [
                    {"type": "text", "text": "Fix this python function please ```def f(): pass```"},
                    {"type": "image_url", "image_url": {"url": "http://x/y.png"}}
                ]}
            ]
        });
        let tags = heuristic_tags(&body, 10);
        assert!(tags.contains(&"coding".to_string()));
        assert!(tags.contains(&"vision".to_string()));
    }

    #[test]
    fn heuristic_tags_long_context() {
        let body = serde_json::json!({ "messages": [{"role": "user", "content": "hello"}] });
        let tags = heuristic_tags(&body, 40_000);
        assert!(tags.contains(&"long-context".to_string()));
    }

    #[test]
    fn function_calling_requires_native_support() {
        // No boon emulates function calling / tool choice: a model lacking the
        // native capability is filtered out regardless of any boons it carries.
        let mut plain = model("plain");
        plain.supports_function_calling = false;
        plain.supports_tool_choice = false;
        plain.boons = vec!["structured_output".to_string()];
        let candidates = vec![healthy(plain)];
        let features = RequestFeatures {
            needs_function_calling: true,
            needs_tool_choice: true,
            ..Default::default()
        };
        let grants = BoonGrants {
            structured_active: true,
        };
        assert!(select_model(
            &candidates,
            &features,
            &HashMap::new(),
            None,
            &[],
            grants,
            &RouterWeights::default()
        )
        .is_none());
    }

    #[test]
    fn structured_boon_satisfies_schema_filter() {
        let mut emulated = model("emulated");
        emulated.supports_response_schema = false;
        emulated.boons = vec!["structured_output".to_string()];
        let candidates = vec![healthy(emulated)];
        let features = RequestFeatures {
            needs_response_schema: true,
            ..Default::default()
        };
        assert!(select_model(
            &candidates,
            &features,
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default()
        )
        .is_none());
        let grants = BoonGrants {
            structured_active: true,
        };
        let chosen = select_model(
            &candidates,
            &features,
            &HashMap::new(),
            None,
            &[],
            grants,
            &RouterWeights::default(),
        )
        .unwrap();
        assert_eq!(chosen.model_name, "emulated");
    }

    #[test]
    fn grant_without_model_boon_does_not_grant() {
        let mut plain = model("plain");
        plain.supports_response_schema = false;
        plain.boons = Vec::new();
        let candidates = vec![healthy(plain)];
        let features = RequestFeatures {
            needs_response_schema: true,
            ..Default::default()
        };
        let grants = BoonGrants {
            structured_active: true,
        };
        assert!(select_model(
            &candidates,
            &features,
            &HashMap::new(),
            None,
            &[],
            grants,
            &RouterWeights::default()
        )
        .is_none());
    }

    #[test]
    fn raising_cost_weight_flips_the_pick() {
        // `cheap` is pricier-but-idle vs `pricey` which is cheap-but-saturated.
        let mut idle_expensive = model("idle-expensive");
        idle_expensive.input_cost_per_token = 0.000_100;
        idle_expensive.max_in_flight = Some(4);
        let mut busy_cheap = model("busy-cheap");
        busy_cheap.input_cost_per_token = 0.000_001;
        busy_cheap.max_in_flight = Some(4);
        let candidates = vec![healthy(idle_expensive), healthy(busy_cheap)];
        let mut busyness = HashMap::new();
        busyness.insert("busy-cheap".to_string(), 4);

        let capacity_first = RouterWeights {
            capacity: 1.0,
            cost: 0.0,
            ..Default::default()
        };
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &busyness,
            None,
            &[],
            BoonGrants::default(),
            &capacity_first,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "idle-expensive");

        let cost_first = RouterWeights {
            capacity: 0.0,
            cost: 1.0,
            ..Default::default()
        };
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &busyness,
            None,
            &[],
            BoonGrants::default(),
            &cost_first,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "busy-cheap");
    }
}
