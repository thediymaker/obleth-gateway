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
    // `temperature` now feeds the softmax sampling in `select_model`.
    // `difficulty_enabled` is not yet consumed: the difficulty tier filter
    // lands in a later step of the auto-router-tuner plan, which is why it is
    // threaded through now instead of being added alongside its own consumer.
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
    /// Effective per-topic strength, one entry per domain (a model tag, or the
    /// synthetic [`GENERAL_DOMAIN`]). Computed by [`derive_levels`] at
    /// registry-refresh time; empty until the registry has run at least once.
    pub levels: Vec<(String, u8)>,
}

/// Synthetic domain covering every chat candidate, used when a request has no
/// intent tags. Deliberately NOT "general" — that is a real, operator-assignable
/// tag in `MODEL_TAGS`, and colliding with it would filter on the wrong thing.
pub const GENERAL_DOMAIN: &str = "*";

impl Candidate {
    /// Highest level this candidate holds across `domains`. Being strong at one
    /// of the request's intents is what qualifies it.
    ///
    /// Not yet called outside tests: the tier filter that consumes this lands
    /// in a later step of the auto-router-tuner plan, same as
    /// `RouterWeights::difficulty_enabled` above.
    #[allow(dead_code)]
    pub fn strength(&self, domains: &[String]) -> u8 {
        domains
            .iter()
            .filter_map(|d| self.levels.iter().find(|(ld, _)| ld == d).map(|(_, l)| *l))
            .max()
            .unwrap_or(0)
    }
}

/// Compute each candidate's effective per-topic strength. Runs at registry
/// refresh (every 15s), never on the request path: deriving cost quantiles
/// per request would be an O(n log n) sort in the hot path.
pub fn derive_levels(candidates: &mut [Candidate], source: obleth_config::TierSource) {
    use obleth_config::{TierSource, MAX_TIER_LEVEL};

    let mut domains: Vec<String> = vec![GENERAL_DOMAIN.to_string()];
    for c in candidates.iter() {
        for t in &c.model.tags {
            if !domains.contains(t) {
                domains.push(t.clone());
            }
        }
    }

    let cost = |c: &Candidate| c.model.input_cost_per_token + c.model.output_cost_per_token;
    let mut assigned: Vec<Vec<(String, u8)>> = vec![Vec::new(); candidates.len()];

    for domain in &domains {
        // Indices of candidates in this domain, cheapest first. Ties break by
        // name so the ladder is stable across refreshes.
        let mut members: Vec<usize> = (0..candidates.len())
            .filter(|&i| domain == GENERAL_DOMAIN || candidates[i].model.tags.contains(domain))
            .collect();
        if members.is_empty() {
            continue;
        }
        members.sort_by(|&a, &b| {
            cost(&candidates[a])
                .partial_cmp(&cost(&candidates[b]))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| {
                    candidates[a]
                        .model
                        .model_name
                        .cmp(&candidates[b].model.model_name)
                })
        });

        let n = members.len();
        for (rank, &i) in members.iter().enumerate() {
            let derived = if n >= MAX_TIER_LEVEL as usize {
                // Split into three bands by cost rank.
                (((rank * MAX_TIER_LEVEL as usize) / n) + 1).min(MAX_TIER_LEVEL as usize) as u8
            } else {
                // Fewer than three members: rank order, not three bands.
                (rank + 1) as u8
            };
            let declared = candidates[i]
                .model
                .declared_levels
                .iter()
                .find(|(d, _)| d == domain)
                .map(|(_, l)| *l);
            let level = match source {
                TierSource::Derived => derived,
                TierSource::Declared => declared.unwrap_or(1),
                TierSource::Hybrid => declared.unwrap_or(derived),
            };
            assigned[i].push((domain.clone(), level));
        }
    }

    for (i, levels) in assigned.into_iter().enumerate() {
        candidates[i].levels = levels;
    }
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
///
/// `uniform` is a draw in `[0,1)` used for softmax sampling when
/// `weights.temperature` is above zero; it is ignored (and the pick is exact
/// argmax) at the default temperature of `0.0`. Callers pass the draw in
/// rather than `select_model` generating it, so the function stays pure and
/// synchronous and can be exercised deterministically by tests and the
/// simulate endpoint.
#[allow(clippy::too_many_arguments)]
pub fn select_model(
    candidates: &[Candidate],
    features: &RequestFeatures,
    busyness: &HashMap<String, usize>,
    allowed_models: Option<&[String]>,
    desired_tags: &[String],
    grants: BoonGrants,
    weights: &RouterWeights,
    uniform: f64,
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

    let mut scored: Vec<(f64, &Candidate)> = Vec::with_capacity(eligible.len());
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
        // Operator thumb on the scale. Clamped so a typo cannot zero out or
        // explode a model's chances.
        let score = score * cand.model.route_bias.clamp(0.1, 3.0);
        scored.push((score, cand));
    }

    // Deterministic order first: descending score, ties broken by name ascending.
    // This ordering is also what makes sampling reproducible given a fixed draw.
    scored.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.1.model.model_name.cmp(&b.1.model.model_name))
    });

    if weights.temperature <= f64::EPSILON {
        return scored.first().map(|(_, c)| c.model.clone());
    }

    // Softmax over scores, shifted by the max for numerical stability.
    let max = scored[0].0;
    let exps: Vec<f64> = scored
        .iter()
        .map(|(s, _)| ((s - max) / weights.temperature).exp())
        .collect();
    let total: f64 = exps.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        return scored.first().map(|(_, c)| c.model.clone());
    }
    let target = uniform.clamp(0.0, 1.0) * total;
    let mut acc = 0.0;
    for (exp, (_, cand)) in exps.iter().zip(scored.iter()) {
        acc += exp;
        if acc >= target {
            return Some(cand.model.clone());
        }
    }
    scored.last().map(|(_, c)| c.model.clone())
}

/// One uniform draw in `[0,1)`. Dependency-free splitmix64 seeded from the
/// clock, matching `weighted_order` in proxy.rs — obleth-proxy deliberately
/// carries no `rand` dependency.
pub fn splitmix_uniform() -> f64 {
    let mut z = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9e37_79b9_7f4a_7c15)
        .wrapping_add(0x9e37_79b9_7f4a_7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    ((z ^ (z >> 31)) as f64 / u64::MAX as f64).clamp(0.0, 0.999_999_999)
}

/// Where a request's routing intent came from. Surfaced in traces and the
/// tuner so an operator can tell a real classification from a fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IntentSource {
    Classifier,
    Heuristic,
    Header,
    Default,
}

/// A request's derived routing intent: which topics it touches and how hard it
/// looks. Difficulty is always in 1..=MAX_TIER_LEVEL.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Intent {
    pub tags: Vec<String>,
    pub difficulty: u8,
    pub source: IntentSource,
}

impl Default for Intent {
    fn default() -> Self {
        Intent {
            tags: Vec::new(),
            difficulty: 1,
            source: IntentSource::Default,
        }
    }
}

/// Map an `x-obleth-effort` header value to a difficulty level. Anything other
/// than `low`/`medium`/`high` (case-insensitive) falls through to the next
/// source in the precedence chain.
pub fn difficulty_from_header(raw: Option<&str>) -> Option<u8> {
    match raw?.trim().to_ascii_lowercase().as_str() {
        "low" => Some(1),
        "medium" => Some(2),
        "high" => Some(3),
        _ => None,
    }
}

/// Cheap, dependency-free intent derived from the request body: which topics
/// it touches, and how hard it looks. Used as a fallback when the classifier
/// is disabled, unconfigured, or unavailable so `auto` routing still gets a
/// useful signal without an extra model call.
///
/// Only emits tags from the fixed vocabulary. Difficulty starts at 1 (a quiet
/// prompt always routes cheap) and only ever rises from signals found during
/// the same scan used for tags — no second pass over the message content.
pub fn heuristic_intent(json: &serde_json::Value, est_input_tokens: u64) -> Intent {
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

    // Difficulty from the same scan. Each signal is a cheap proxy for effort;
    // absence of signal means 1, so a quiet prompt always routes cheap.
    let mut difficulty: u8 = 1;
    let mut bump = |to: u8| {
        if to > difficulty {
            difficulty = to;
        }
    };
    if lower.contains("stack backtrace")
        || lower.contains("traceback (most recent call last)")
        || lower.contains("panicked at")
        || lower.contains("segmentation fault")
    {
        bump(3);
    }
    if [
        "why does",
        "race condition",
        "deadlock",
        "optimize",
        "refactor",
        "prove",
        "derive",
        "explain why",
        "trade-off",
        "root cause",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        bump(2);
    }
    if est_input_tokens > 32_000 {
        bump(2);
    }
    if text.len() > 8_000 {
        bump(2);
    }

    Intent {
        tags,
        difficulty,
        source: IntentSource::Heuristic,
    }
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
            declared_levels: Vec::new(),
            boons: Vec::new(),
            tool_servers: Vec::new(),
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: obleth_config::DEFAULT_RETRY_BACKOFF_MS,
            endpoint_selection_mode: obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string(),
            debug_diagnostics: false,
            energy_slots_per_node: 0,
            route_bias: 1.0,
            endpoints: Vec::new(),
        }
    }

    fn healthy(m: ResolvedModel) -> Candidate {
        Candidate {
            model: m,
            healthy: true,
            levels: Vec::new(),
        }
    }

    fn level_of(c: &Candidate, domain: &str) -> u8 {
        c.levels
            .iter()
            .find(|(d, _)| d == domain)
            .map(|(_, l)| *l)
            .unwrap_or(0)
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
            0.0,
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
            0.0,
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
            &RouterWeights::default(),
            0.0
        )
        .is_none());
    }

    #[test]
    fn route_bias_can_flip_an_otherwise_equal_pick() {
        let mut a = model("alpha");
        let mut b = model("beta");
        a.route_bias = 1.0;
        b.route_bias = 2.0;
        let candidates = vec![healthy(a), healthy(b)];
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
        )
        .unwrap();
        // Without bias, the name tie-break would pick "alpha".
        assert_eq!(chosen.model_name, "beta");
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
            0.0,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "tools");
    }

    #[test]
    fn unhealthy_candidates_are_skipped() {
        let mut down = Candidate {
            model: model("down"),
            healthy: false,
            levels: Vec::new(),
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
            0.0,
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
            0.0,
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
            0.0,
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
            0.0,
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
            0.0,
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
            0.0,
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
        let tags = heuristic_intent(&body, 10).tags;
        assert!(tags.contains(&"coding".to_string()));
        assert!(tags.contains(&"vision".to_string()));
    }

    #[test]
    fn heuristic_tags_long_context() {
        let body = serde_json::json!({ "messages": [{"role": "user", "content": "hello"}] });
        let tags = heuristic_intent(&body, 40_000).tags;
        assert!(tags.contains(&"long-context".to_string()));
    }

    #[test]
    fn header_maps_effort_words_to_levels() {
        assert_eq!(difficulty_from_header(Some("low")), Some(1));
        assert_eq!(difficulty_from_header(Some("medium")), Some(2));
        assert_eq!(difficulty_from_header(Some("HIGH")), Some(3));
        assert_eq!(difficulty_from_header(Some("banana")), None);
        assert_eq!(difficulty_from_header(None), None);
    }

    #[test]
    fn heuristic_difficulty_defaults_to_one() {
        let body = serde_json::json!({ "messages": [{"role":"user","content":"hi"}] });
        assert_eq!(heuristic_intent(&body, 10).difficulty, 1);
    }

    #[test]
    fn heuristic_difficulty_rises_on_a_stack_trace() {
        let body = serde_json::json!({ "messages": [{"role":"user","content":
            "thread 'main' panicked at src/x.rs:12\n  stack backtrace:\n   0: foo\n   1: bar"}] });
        assert!(heuristic_intent(&body, 10).difficulty >= 2);
    }

    #[test]
    fn heuristic_difficulty_rises_on_long_context() {
        let body = serde_json::json!({ "messages": [{"role":"user","content":"summarize"}] });
        assert!(heuristic_intent(&body, 60_000).difficulty >= 2);
    }

    #[test]
    fn heuristic_intent_still_yields_the_same_tags_as_before() {
        let body = serde_json::json!({
            "messages": [{"role":"user","content":[
                {"type":"text","text":"Fix this python function please ```def f(): pass```"},
                {"type":"image_url","image_url":{"url":"http://example.invalid/y.png"}}
            ]}]
        });
        let intent = heuristic_intent(&body, 10);
        assert!(intent.tags.contains(&"coding".to_string()));
        assert!(intent.tags.contains(&"vision".to_string()));
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
            &RouterWeights::default(),
            0.0
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
            &RouterWeights::default(),
            0.0
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
            0.0,
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
            &RouterWeights::default(),
            0.0
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
            0.0,
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
            0.0,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "busy-cheap");
    }

    #[test]
    fn temperature_zero_is_exact_argmax() {
        let mut cheap = model("cheap");
        cheap.input_cost_per_token = 0.000_001;
        let mut pricey = model("pricey");
        pricey.input_cost_per_token = 0.000_100;
        let candidates = vec![healthy(pricey), healthy(cheap)];
        let w = RouterWeights {
            temperature: 0.0,
            ..Default::default()
        };
        // Every uniform draw must produce the same answer when temperature is 0.
        for u in [0.0, 0.25, 0.5, 0.75, 0.999] {
            let chosen = select_model(
                &candidates,
                &RequestFeatures::default(),
                &HashMap::new(),
                None,
                &[],
                BoonGrants::default(),
                &w,
                u,
            )
            .unwrap();
            assert_eq!(
                chosen.model_name, "cheap",
                "uniform {u} changed a temperature-0 pick"
            );
        }
    }

    #[test]
    fn temperature_makes_the_outcome_depend_on_the_draw() {
        let mut cheap = model("cheap");
        cheap.input_cost_per_token = 0.000_001;
        let mut pricey = model("pricey");
        pricey.input_cost_per_token = 0.000_100;
        let candidates = vec![healthy(pricey), healthy(cheap)];
        let w = RouterWeights {
            temperature: 1.0,
            ..Default::default()
        };
        // Candidates are ordered by descending score; a draw past the leader's
        // probability mass must land on the runner-up.
        let low = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &w,
            0.0,
        )
        .unwrap();
        let high = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &w,
            0.999,
        )
        .unwrap();
        assert_ne!(
            low.model_name, high.model_name,
            "temperature 1.0 must make the outcome depend on the draw"
        );
    }

    #[test]
    fn temperature_still_returns_some_for_a_single_candidate() {
        let candidates = vec![healthy(model("only"))];
        let w = RouterWeights {
            temperature: 1.5,
            ..Default::default()
        };
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &w,
            0.5,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "only");
    }

    #[test]
    fn cost_rank_splits_three_models_into_three_bands() {
        let mut cheap = model("cheap");
        cheap.input_cost_per_token = 0.000_001;
        let mut mid = model("mid");
        mid.input_cost_per_token = 0.000_010;
        let mut dear = model("dear");
        dear.input_cost_per_token = 0.000_100;
        for m in [&mut cheap, &mut mid, &mut dear] {
            m.tags = vec!["coding".to_string()];
        }
        let mut cands = vec![healthy(cheap), healthy(mid), healthy(dear)];
        derive_levels(&mut cands, obleth_config::TierSource::Derived);
        assert_eq!(level_of(&cands[0], "coding"), 1);
        assert_eq!(level_of(&cands[1], "coding"), 2);
        assert_eq!(level_of(&cands[2], "coding"), 3);
    }

    #[test]
    fn two_models_get_rank_order_not_three_bands() {
        let mut cheap = model("cheap");
        cheap.input_cost_per_token = 0.000_001;
        let mut dear = model("dear");
        dear.input_cost_per_token = 0.000_100;
        for m in [&mut cheap, &mut dear] {
            m.tags = vec!["math".to_string()];
        }
        let mut cands = vec![healthy(cheap), healthy(dear)];
        derive_levels(&mut cands, obleth_config::TierSource::Derived);
        assert_eq!(level_of(&cands[0], "math"), 1);
        assert_eq!(
            level_of(&cands[1], "math"),
            2,
            "two models rank 1..2, not 1 and 3"
        );
    }

    #[test]
    fn declared_level_overrides_derived_for_that_pair_only() {
        let mut small = model("small");
        small.input_cost_per_token = 0.000_001;
        small.tags = vec!["coding".to_string(), "math".to_string()];
        small.declared_levels = vec![("coding".to_string(), 3)];
        let mut big = model("big");
        big.input_cost_per_token = 0.000_100;
        big.tags = vec!["coding".to_string(), "math".to_string()];
        let mut cands = vec![healthy(small), healthy(big)];
        derive_levels(&mut cands, obleth_config::TierSource::Hybrid);
        assert_eq!(level_of(&cands[0], "coding"), 3, "declared wins");
        assert_eq!(level_of(&cands[0], "math"), 1, "other tags stay derived");
    }

    #[test]
    fn every_candidate_has_a_level_in_the_general_domain() {
        let mut untagged = model("untagged");
        untagged.tags = Vec::new();
        let mut cands = vec![healthy(untagged)];
        derive_levels(&mut cands, obleth_config::TierSource::Hybrid);
        assert!(
            level_of(&cands[0], GENERAL_DOMAIN) >= 1,
            "the synthetic domain must cover every candidate so the filter cannot empty the set"
        );
    }

    #[test]
    fn general_domain_sentinel_does_not_collide_with_the_real_general_tag() {
        assert_ne!(GENERAL_DOMAIN, "general");
        assert!(!obleth_config::is_valid_tag(GENERAL_DOMAIN));
    }

    #[test]
    fn strength_is_the_max_across_desired_domains() {
        let mut m = model("specialist");
        m.tags = vec!["coding".to_string(), "math".to_string()];
        m.declared_levels = vec![("coding".to_string(), 3), ("math".to_string(), 1)];
        let mut cands = vec![healthy(m)];
        derive_levels(&mut cands, obleth_config::TierSource::Declared);
        let domains = vec!["coding".to_string(), "math".to_string()];
        assert_eq!(cands[0].strength(&domains), 3);
    }
}
