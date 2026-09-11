//! `auto` model selection.
//!
//! When a client sends `model: "auto"`, the gateway chooses a concrete
//! registered model instead of routing to a fixed upstream. Selection is a
//! three-stage process:
//!
//! 1. **Hard filters** remove models that cannot serve the request at all:
//!    disabled, unhealthy / in maintenance, too small a context window, or
//!    missing a required capability (tools / tool_choice / JSON schema). A
//!    per-tenant model allowlist, when present, is also enforced here.
//! 2. **Difficulty tier floor**, gated behind `difficulty_enabled`, drops
//!    candidates too weak for the request's difficulty. The floor clamps
//!    *down* to the best level actually available, so this stage can narrow
//!    the field but never empties it.
//! 3. **Scoring** ranks the survivors by spare capacity (prefer models that
//!    are not busy) and cost (prefer cheaper models), then picks the best with
//!    a deterministic tie-break on model name.
//!
//! All three stages live in one private function, [`evaluate`], and three thin
//! entry points sit over it: [`select_model`] takes the verdict and serves,
//! [`explain_selection`] narrates without serving (the simulate endpoint), and
//! [`route`] does both from a single evaluation — which is how a traced request
//! gets an explanation of the pick it actually served rather than of a second,
//! independently sampled one. The routing explanation therefore cannot describe
//! a decision the router would not make. All are pure and synchronous, so they
//! can be unit-tested without any of the data-plane wiring.

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
    // `temperature` feeds the softmax sampling in `select_model`.
    // `difficulty_enabled` gates the tier filter in `select_model`.
    pub temperature: f64,
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

/// Every hard-filter reason string, in the order the filters are applied. A
/// candidate is attributed to the *first* filter it fails, so this order is
/// part of the explanation's contract, not just presentation.
const REJECTION_REASONS: [&str; 9] = [
    "disabled",
    "unhealthy",
    "model_type",
    "context_window",
    "function_calling",
    "tool_choice",
    "response_schema",
    "tenant_allowlist",
    "below_tier_floor",
];

/// One scored survivor, holding a borrow of its candidate so [`select_model`]
/// can clone the winning [`ResolvedModel`] without re-running anything.
struct ScoredRef<'a> {
    cand: &'a Candidate,
    level: u8,
    spare: f64,
    cost_score: f64,
    tag_score: f64,
    bias: f64,
    score: f64,
}

/// Everything one routing decision produced: the ordered survivors, the index
/// of the pick, why each loser was dropped, and the tier-filter state.
struct Evaluation<'a> {
    /// Survivors in the router's own order: descending score, ties by name.
    ordered: Vec<ScoredRef<'a>>,
    /// Index into `ordered`. `None` only when no candidate cleared stage 1.
    chosen: Option<usize>,
    /// Narration only; empty under [`Narration::Off`].
    rejected: Vec<crate::route_explain::Rejection>,
    /// Drives the tier filter when tiering is on, and is reported either way.
    /// Empty only when tiering is off *and* nobody asked for narration.
    tier_domains: Vec<String>,
    tier_floor: u8,
    tier_floor_clamped: bool,
}

/// Whether an evaluation should build its explanation.
///
/// Narration — the rejection strings, the tier-domain list when tiering is off,
/// and each survivor's level — costs a `String` per rejected model on a path
/// that discards them. The serving hot path asks for [`Narration::Off`].
///
/// This is strictly additive output. Nothing computed only under
/// [`Narration::On`] is read by any stage that decides the outcome, so
/// `Evaluation::chosen` and `Evaluation::ordered` are identical either way;
/// `narration_cannot_move_the_verdict` pins that across the knob matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Narration {
    Off,
    On,
}

/// The one implementation of `auto` selection.
///
/// [`select_model`], [`explain_selection`] and [`route`] are all thin wrappers
/// over this function and share every filter, weight and tie-break by
/// construction, so the explanation cannot describe a decision the router would
/// not make. Do not reimplement any stage in a caller: a tuned threshold that
/// lives here changes all three, and a tuned threshold that lives in a caller is
/// a drift bug.
#[allow(clippy::too_many_arguments)]
fn evaluate<'a>(
    candidates: &'a [Candidate],
    features: &RequestFeatures,
    busyness: &HashMap<String, usize>,
    allowed_models: Option<&[String]>,
    desired_tags: &[String],
    grants: BoonGrants,
    weights: &RouterWeights,
    uniform: f64,
    difficulty: u8,
    narration: Narration,
) -> Evaluation<'a> {
    let narrating = narration == Narration::On;
    let required_context = features
        .est_input_tokens
        .saturating_add(features.max_tokens);

    // Capability via boon: the gateway emulates structured output for opted-in
    // models, so the hard filters must not exclude them. Function calling and
    // tool choice are native capabilities only — no boon emulates them.
    let schema_via_boon = |c: &Candidate| {
        grants.structured_active && c.model.boons.iter().any(|b| b == "structured_output")
    };

    // Which domains the tier filter reasons over. Needed for the verdict when
    // tiering is on, and reported when narrating; skipped entirely otherwise so
    // the serving path does not allocate a list nobody reads.
    let tier_domains: Vec<String> = if weights.difficulty_enabled || narrating {
        if desired_tags.is_empty() {
            vec![GENERAL_DOMAIN.to_string()]
        } else {
            desired_tags.to_vec()
        }
    } else {
        Vec::new()
    };

    // Rejections collapse by reason rather than one row per model, so the
    // payload stays small enough to ride a trace span. Under `Narration::Off`
    // nothing is recorded and no model name is ever cloned.
    let mut buckets: Vec<Vec<String>> = vec![Vec::new(); REJECTION_REASONS.len()];
    let mut reject = |reason: &str, name: &str| {
        if !narrating {
            return;
        }
        let slot = REJECTION_REASONS
            .iter()
            .position(|r| *r == reason)
            .expect("rejection reason must be declared in REJECTION_REASONS");
        buckets[slot].push(name.to_string());
    };
    let finish = |buckets: Vec<Vec<String>>| -> Vec<crate::route_explain::Rejection> {
        REJECTION_REASONS
            .iter()
            .zip(buckets)
            .filter(|(_, models)| !models.is_empty())
            .map(|(reason, models)| crate::route_explain::Rejection { reason, models })
            .collect()
    };

    // ---- stage 1: hard filters ----
    // First failing filter wins; the order below is the filter order.
    let hard_reject = |c: &Candidate| -> Option<&'static str> {
        if !c.model.enabled {
            return Some("disabled");
        }
        if !c.healthy {
            return Some("unhealthy");
        }
        // `auto` is a chat-completions convenience; only chat models are
        // eligible. Non-chat modalities (embedding, image, audio) are addressed
        // by name on their dedicated endpoints.
        if c.model.model_type != obleth_config::DEFAULT_MODEL_TYPE {
            return Some("model_type");
        }
        // A non-positive context window means "unknown" (misconfigured or
        // legacy row); don't exclude on a signal we don't trust.
        if c.model.context_window > 0 && (c.model.context_window as u64) < required_context {
            return Some("context_window");
        }
        if features.needs_function_calling && !c.model.supports_function_calling {
            return Some("function_calling");
        }
        if features.needs_tool_choice && !c.model.supports_tool_choice {
            return Some("tool_choice");
        }
        if features.needs_response_schema
            && !c.model.supports_response_schema
            && !schema_via_boon(c)
        {
            return Some("response_schema");
        }
        if let Some(allowed) = allowed_models {
            if !allowed.iter().any(|m| m == &c.model.model_name) {
                return Some("tenant_allowlist");
            }
        }
        None
    };

    let mut eligible: Vec<&Candidate> = Vec::with_capacity(candidates.len());
    for c in candidates {
        match hard_reject(c) {
            Some(reason) => reject(reason, &c.model.model_name),
            None => eligible.push(c),
        }
    }

    if eligible.is_empty() {
        return Evaluation {
            ordered: Vec::new(),
            chosen: None,
            rejected: finish(buckets),
            tier_domains,
            tier_floor: 0,
            tier_floor_clamped: false,
        };
    }

    // ---- stage 2: difficulty tier floor ----
    // Clamp-down: the floor is the lesser of what the request asked for and the
    // best level actually available, so this stage can narrow the field but can
    // never empty it — a hard request degrades within its topic instead of
    // failing or falling through to a weak generalist.
    let mut tier_floor = 0u8;
    let mut tier_floor_clamped = false;
    let eligible: Vec<&Candidate> = if weights.difficulty_enabled {
        let available = eligible
            .iter()
            .map(|c| c.strength(&tier_domains))
            .max()
            .unwrap_or(0);
        let requested = difficulty.clamp(1, obleth_config::MAX_TIER_LEVEL);
        tier_floor = requested.min(available);
        // True exactly when the request asked for more than any survivor holds,
        // i.e. the clamp actually moved the floor.
        tier_floor_clamped = requested > available;
        let (kept, dropped): (Vec<&Candidate>, Vec<&Candidate>) = eligible
            .iter()
            .copied()
            .partition(|c| c.strength(&tier_domains) >= tier_floor);
        // Defensive: a candidate set with no levels at all (stale snapshot mid
        // refresh) must not disappear.
        if kept.is_empty() {
            eligible
        } else {
            for c in dropped {
                reject("below_tier_floor", &c.model.model_name);
            }
            kept
        }
    } else {
        eligible
    };

    // ---- stage 3: scoring ----
    let costs: Vec<f64> = eligible
        .iter()
        .map(|c| c.model.input_cost_per_token + c.model.output_cost_per_token)
        .collect();
    let min_cost = costs.iter().copied().fold(f64::INFINITY, f64::min);
    let max_cost = costs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let cost_span = max_cost - min_cost;

    let mut scored: Vec<ScoredRef<'a>> = Vec::with_capacity(eligible.len());
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
        let (tag_score, score) = if desired_tags.is_empty() {
            (0.0, base)
        } else {
            let overlap = desired_tags
                .iter()
                .filter(|t| cand.model.tags.iter().any(|mt| mt == *t))
                .count();
            let tag_score = (overlap as f64 / desired_tags.len() as f64).min(1.0);
            (
                tag_score,
                weights.tag * tag_score + (1.0 - weights.tag) * base,
            )
        };
        // Operator thumb on the scale. Clamped so a typo cannot zero out or
        // explode a model's chances.
        let bias = cand.model.route_bias.clamp(0.1, 3.0);
        scored.push(ScoredRef {
            cand,
            // Narration only — no stage reads it. Skipped on the serving path.
            level: if narrating {
                cand.strength(&tier_domains)
            } else {
                0
            },
            spare,
            cost_score,
            tag_score,
            bias,
            score: score * bias,
        });
    }

    // Deterministic order first: descending score, ties broken by name ascending.
    // This ordering is also what makes sampling reproducible given a fixed draw.
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.cand.model.model_name.cmp(&b.cand.model.model_name))
    });

    let chosen = pick_index(&scored, weights, uniform);

    Evaluation {
        ordered: scored,
        chosen,
        rejected: finish(buckets),
        tier_domains,
        tier_floor,
        tier_floor_clamped,
    }
}

/// Index of the winner in an already-ordered candidate list: the argmax at
/// temperature 0, otherwise a softmax draw against `uniform`.
fn pick_index(scored: &[ScoredRef<'_>], weights: &RouterWeights, uniform: f64) -> Option<usize> {
    if scored.is_empty() {
        return None;
    }
    if weights.temperature <= f64::EPSILON {
        return Some(0);
    }

    // Softmax over scores, shifted by the max for numerical stability.
    let max = scored[0].score;
    let exps: Vec<f64> = scored
        .iter()
        .map(|s| ((s.score - max) / weights.temperature).exp())
        .collect();
    let total: f64 = exps.iter().sum();
    if !total.is_finite() || total <= 0.0 {
        return Some(0);
    }
    let target = uniform.clamp(0.0, 1.0) * total;
    let mut acc = 0.0;
    for (i, exp) in exps.iter().enumerate() {
        acc += exp;
        if acc >= target {
            return Some(i);
        }
    }
    Some(scored.len() - 1)
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
///
/// `difficulty` is the request's derived difficulty (1..=MAX_TIER_LEVEL). It
/// only has an effect when `weights.difficulty_enabled` is set; see stage 2 of
/// [`evaluate`] for the clamp-down floor it drives.
///
/// All of the logic lives in [`evaluate`], which [`explain_selection`] and
/// [`route`] also call — that shared core is what makes all three answers
/// identical by construction rather than by review.
///
/// Use this on the serving path when nothing will consume an explanation; it
/// asks [`evaluate`] for [`Narration::Off`] and so allocates nothing for one.
/// When the caller wants both the model and its explanation, call [`route`]
/// instead of calling this and [`explain_selection`] in turn — two calls would
/// mean two `uniform` draws and, above temperature 0, two different answers.
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
    difficulty: u8,
) -> Option<ResolvedModel> {
    let ev = evaluate(
        candidates,
        features,
        busyness,
        allowed_models,
        desired_tags,
        grants,
        weights,
        uniform,
        difficulty,
        Narration::Off,
    );
    ev.chosen.map(|i| ev.ordered[i].cand.model.clone())
}

/// Serve *and* explain one `auto` request from a single evaluation.
///
/// The pick and the explanation come out of the same [`Evaluation`], built from
/// one `uniform` draw, so `RouteExplain::chosen` names the model actually
/// returned — by identity, not by a test. A caller that instead called
/// [`select_model`] and [`explain_selection`] separately would have to pass a
/// draw to each; above temperature 0 those are two independent samples, and the
/// trace would describe a request the gateway did not serve. That is why this
/// function exists rather than a note telling callers to be careful.
///
/// See [`explain_selection`] for what `intent` supplies and why `classifier_ms`
/// is `0`.
// Not yet called outside tests: the `auto_route` span writer is the consumer,
// and it lands after this.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub(crate) fn route(
    candidates: &[Candidate],
    features: &RequestFeatures,
    busyness: &HashMap<String, usize>,
    allowed_models: Option<&[String]>,
    desired_tags: &[String],
    grants: BoonGrants,
    weights: &RouterWeights,
    uniform: f64,
    intent: &Intent,
) -> (Option<ResolvedModel>, crate::route_explain::RouteExplain) {
    let ev = evaluate(
        candidates,
        features,
        busyness,
        allowed_models,
        desired_tags,
        grants,
        weights,
        uniform,
        intent.difficulty,
        Narration::On,
    );
    let picked = ev.chosen.map(|i| ev.ordered[i].cand.model.clone());
    (picked, narrate(ev, desired_tags, weights, intent))
}

/// Explain the decision [`select_model`] would make for these inputs, without
/// making it. This is the simulate endpoint's entry point: it answers "what
/// would happen" for hypothetical weights, so it explains without serving.
///
/// Every function here delegates to [`evaluate`], so the verdict described is
/// the verdict taken — the explanation records the losers instead of discarding
/// them, and adds nothing that could disagree.
///
/// `intent` supplies the difficulty *and* its provenance: `intent.source` is
/// reported as both `difficulty_source` and `tag_source`, because a request's
/// tags and difficulty are always derived together by the same step.
///
/// `classifier_ms` is always `0` here. Only the data-plane call site times the
/// classifier, so the proxy overwrites the field before recording the span;
/// a `0` from the simulate endpoint is correct, not a missing measurement.
// Not yet called outside tests: the admin simulate endpoint is the consumer,
// and it lands after this.
#[allow(dead_code)]
#[allow(clippy::too_many_arguments)]
pub fn explain_selection(
    candidates: &[Candidate],
    features: &RequestFeatures,
    busyness: &HashMap<String, usize>,
    allowed_models: Option<&[String]>,
    desired_tags: &[String],
    grants: BoonGrants,
    weights: &RouterWeights,
    uniform: f64,
    intent: &Intent,
) -> crate::route_explain::RouteExplain {
    let ev = evaluate(
        candidates,
        features,
        busyness,
        allowed_models,
        desired_tags,
        grants,
        weights,
        uniform,
        intent.difficulty,
        Narration::On,
    );
    narrate(ev, desired_tags, weights, intent)
}

/// Render a finished [`Evaluation`] as the wire shape. Pure presentation: it
/// decides nothing, and is the single place both explaining entry points build
/// their output so they cannot describe the same evaluation differently.
fn narrate(
    ev: Evaluation<'_>,
    desired_tags: &[String],
    weights: &RouterWeights,
    intent: &Intent,
) -> crate::route_explain::RouteExplain {
    crate::route_explain::RouteExplain {
        chosen: ev
            .chosen
            .map(|i| ev.ordered[i].cand.model.model_name.clone()),
        difficulty: intent.difficulty,
        difficulty_source: intent.source,
        // The tags that actually drove scoring, which the simulate endpoint may
        // override independently of the intent that produced them.
        tags: desired_tags.to_vec(),
        tag_source: intent.source,
        classifier_ms: 0,
        tier_domains: ev.tier_domains,
        tier_floor: ev.tier_floor,
        tier_floor_clamped: ev.tier_floor_clamped,
        weights: crate::route_explain::WeightsView {
            capacity: weights.capacity,
            cost: weights.cost,
            tag: weights.tag,
            soft_cap: weights.soft_cap,
            difficulty_enabled: weights.difficulty_enabled,
        },
        temperature: weights.temperature,
        // Index-based: the draw moved the pick off the head of the list. See
        // the field's own docs for why that is not quite "beat the argmax".
        sampled: ev.chosen.is_some_and(|i| i != 0),
        scored: ev
            .ordered
            .iter()
            .enumerate()
            .map(|(i, s)| crate::route_explain::ScoredCandidate {
                model: s.cand.model.model_name.clone(),
                level: s.level,
                spare: s.spare,
                cost_score: s.cost_score,
                tag_score: s.tag_score,
                bias: s.bias,
                score: s.score,
                chosen: ev.chosen == Some(i),
            })
            .collect(),
        rejected: ev.rejected,
    }
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
            1,
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
            1,
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
            0.0,
            1
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
            1,
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
            1,
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
            1,
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
            1,
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
            1,
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
            1,
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
            1,
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
            1,
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
            0.0,
            1
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
            0.0,
            1
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
            1,
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
            0.0,
            1
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
            1,
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
            1,
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
                1,
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
            1,
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
            1,
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
            1,
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

    fn tiered(name: &str, cost: f64, levels: &[(&str, u8)]) -> Candidate {
        let mut m = model(name);
        m.input_cost_per_token = cost;
        m.tags = levels.iter().map(|(d, _)| d.to_string()).collect();
        let mut c = healthy(m);
        c.levels = levels.iter().map(|(d, l)| (d.to_string(), *l)).collect();
        c
    }

    fn tiering_on() -> RouterWeights {
        RouterWeights {
            difficulty_enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn hard_request_skips_the_weak_model() {
        let cands = vec![
            tiered("weak", 0.000_001, &[("coding", 1)]),
            tiered("strong", 0.000_100, &[("coding", 3)]),
        ];
        let desired = vec!["coding".to_string()];
        let chosen = select_model(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &desired,
            BoonGrants::default(),
            &tiering_on(),
            0.0,
            3,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "strong");
    }

    #[test]
    fn easy_request_still_takes_the_cheap_model() {
        let cands = vec![
            tiered("weak", 0.000_001, &[("coding", 1)]),
            tiered("strong", 0.000_100, &[("coding", 3)]),
        ];
        let desired = vec!["coding".to_string()];
        let chosen = select_model(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &desired,
            BoonGrants::default(),
            &tiering_on(),
            0.0,
            1,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "weak");
    }

    #[test]
    fn floor_clamps_down_when_the_top_tier_is_absent() {
        // The only coding:3 model is gone. A difficulty-3 request must fall to the
        // best coding model still present, NOT to a weak generalist, and must not
        // return None.
        let cands = vec![
            tiered("weak", 0.000_001, &[("coding", 1)]),
            tiered("middling", 0.000_010, &[("coding", 2)]),
        ];
        let desired = vec!["coding".to_string()];
        let chosen = select_model(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &desired,
            BoonGrants::default(),
            &tiering_on(),
            0.0,
            3,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "middling");
    }

    #[test]
    fn tier_filter_never_empties_the_candidate_set() {
        let cands = vec![tiered("only", 0.000_001, &[("coding", 1)])];
        let desired = vec!["coding".to_string()];
        for difficulty in 1..=3 {
            assert!(
                select_model(
                    &cands,
                    &RequestFeatures::default(),
                    &HashMap::new(),
                    None,
                    &desired,
                    BoonGrants::default(),
                    &tiering_on(),
                    0.0,
                    difficulty,
                )
                .is_some(),
                "difficulty {difficulty} turned a servable request into no-model"
            );
        }
    }

    #[test]
    fn untagged_request_tiers_through_the_general_domain() {
        let mut weak = tiered("weak", 0.000_001, &[]);
        weak.levels = vec![(GENERAL_DOMAIN.to_string(), 1)];
        let mut strong = tiered("strong", 0.000_100, &[]);
        strong.levels = vec![(GENERAL_DOMAIN.to_string(), 3)];
        let cands = vec![weak, strong];
        let chosen = select_model(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &tiering_on(),
            0.0,
            3,
        )
        .unwrap();
        assert_eq!(
            chosen.model_name, "strong",
            "a hard question with no topic tag must still reach a strong model"
        );
    }

    #[test]
    fn tiering_disabled_ignores_difficulty_entirely() {
        let cands = vec![
            tiered("weak", 0.000_001, &[("coding", 1)]),
            tiered("strong", 0.000_100, &[("coding", 3)]),
        ];
        let desired = vec!["coding".to_string()];
        let off = RouterWeights {
            difficulty_enabled: false,
            ..Default::default()
        };
        let chosen = select_model(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &desired,
            BoonGrants::default(),
            &off,
            0.0,
            3,
        )
        .unwrap();
        assert_eq!(
            chosen.model_name, "weak",
            "with tiering off, cost still wins"
        );
    }

    #[test]
    fn empty_levels_make_the_tier_filter_a_no_op() {
        // Registry mid-refresh: `levels` hasn't been populated on either
        // candidate yet. `kept == eligible` here by construction (every
        // strength is 0, so the floor collapses to 0 and nothing is excluded)
        // — this pins that no-op behavior, it does not exercise the
        // `if kept.is_empty()` defensive fallback (that branch is otherwise
        // unreachable; see the report for why it is kept anyway).
        let cands = vec![healthy(model("a")), healthy(model("b"))];
        let desired = vec!["coding".to_string()];
        assert!(select_model(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &desired,
            BoonGrants::default(),
            &tiering_on(),
            0.0,
            3,
        )
        .is_some());
    }

    #[test]
    fn absent_domain_makes_the_tier_filter_a_no_op() {
        // The request wants `math`, but every candidate is tagged only
        // `coding`. `kept == eligible` here by construction: every candidate's
        // strength on the desired domain is 0, so the floor collapses to 0 and
        // the filter excludes nothing — it does not exercise the
        // `if kept.is_empty()` defensive fallback.
        let cands = vec![
            tiered("weak", 0.000_001, &[("coding", 1)]),
            tiered("strong", 0.000_100, &[("coding", 3)]),
        ];
        let desired = vec!["math".to_string()];
        assert!(select_model(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &desired,
            BoonGrants::default(),
            &tiering_on(),
            0.0,
            3,
        )
        .is_some());
    }

    #[test]
    fn multi_domain_candidate_survives_on_its_strongest_matching_tag() {
        // Strong at coding, weak at math, competing against a cheaper model
        // that only holds a coding level. Both are tagged identically
        // (`coding` + `math`) so stage 3's tag-overlap score ties between them
        // — only the tier filter (stage 2) can decide the outcome here, not
        // scoring. The floor is set by the specialist's max across desired
        // domains (3); the competitor's strength is 1 regardless of max-vs-min
        // (it only has one matching level), so it cannot clear a floor of 3
        // and must be filtered out before scoring runs.
        //
        // If `strength` computed a min across domains instead of a max, the
        // specialist's own strength would drop to 1 too, both candidates
        // would clear the collapsed floor, tags would still tie in stage 3,
        // and the cheaper competitor would then win on cost — this test fails
        // in that case (verified by temporarily mutating `strength`'s `.max()`
        // to `.min()`: with the tags left coupled to levels the test still
        // passed, because tag overlap alone decided it; decoupling tags from
        // levels, as done here, was required to make the mutation flip the
        // outcome).
        let mut specialist = tiered("specialist", 0.000_050, &[("coding", 3), ("math", 1)]);
        let mut shallow = tiered("shallow", 0.000_001, &[("coding", 1)]);
        specialist.model.tags = vec!["coding".to_string(), "math".to_string()];
        shallow.model.tags = vec!["coding".to_string(), "math".to_string()];
        let cands = vec![specialist, shallow];
        let desired = vec!["coding".to_string(), "math".to_string()];
        let chosen = select_model(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &desired,
            BoonGrants::default(),
            &tiering_on(),
            0.0,
            3,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "specialist");
    }

    fn intent_at(difficulty: u8) -> Intent {
        Intent {
            difficulty,
            ..Default::default()
        }
    }

    /// An explanation must agree with itself: exactly one scored row is marked
    /// `chosen`, and it names the model in `chosen`. `explain_agrees_with_
    /// select_model` only checks the top-level field, so an off-by-one in the
    /// per-row marking would otherwise slip through.
    fn assert_self_consistent(ex: &crate::route_explain::RouteExplain) {
        assert_eq!(
            ex.scored.iter().find(|s| s.chosen).map(|s| &s.model),
            ex.chosen.as_ref(),
            "the marked row and the `chosen` field must name the same model"
        );
        assert_eq!(
            ex.scored.iter().filter(|s| s.chosen).count(),
            usize::from(ex.chosen.is_some()),
            "exactly one row may be marked chosen, and none when nothing was picked"
        );
    }

    /// A candidate set with enough spread — costs, tags, tier levels and a bias
    /// — that every knob in the agreement matrix can actually change the pick.
    fn varied_candidates() -> Vec<Candidate> {
        let mut cheap = tiered("cheap", 0.000_001, &[("coding", 1)]);
        cheap.levels.push((GENERAL_DOMAIN.to_string(), 1));
        let mut mid = tiered("mid", 0.000_010, &[("coding", 2), ("math", 2)]);
        mid.levels.push((GENERAL_DOMAIN.to_string(), 2));
        let mut dear = tiered("dear", 0.000_100, &[("coding", 3), ("math", 3)]);
        dear.levels.push((GENERAL_DOMAIN.to_string(), 3));
        dear.model.route_bias = 1.4;
        let mut plain = healthy(model("plain"));
        plain.model.input_cost_per_token = 0.000_050;
        plain.levels = vec![(GENERAL_DOMAIN.to_string(), 2)];
        vec![cheap, mid, dear, plain]
    }

    /// One cell of the agreement matrix: every input that can move a decision.
    struct Knobs {
        weights: RouterWeights,
        uniform: f64,
        tags: Vec<String>,
        difficulty: u8,
        allowed: Option<Vec<String>>,
    }

    impl Knobs {
        /// Names the cell, so a failure says which combination broke.
        fn label(&self) -> String {
            format!(
                "temp={} uniform={} tags={:?} difficulty={} allowed={:?} tiering={}",
                self.weights.temperature,
                self.uniform,
                self.tags,
                self.difficulty,
                self.allowed,
                self.weights.difficulty_enabled,
            )
        }
    }

    /// Every combination of the knobs that can move a decision: 216 cells.
    fn knob_matrix() -> Vec<Knobs> {
        let mut out = Vec::new();
        for temperature in [0.0, 0.5, 2.0] {
            for uniform in [0.0, 0.4, 0.999] {
                for tags in [vec![], vec!["coding".to_string()]] {
                    for difficulty in 1..=3u8 {
                        for allowed in [None, Some(vec!["cheap".to_string(), "dear".to_string()])] {
                            for difficulty_enabled in [false, true] {
                                out.push(Knobs {
                                    weights: RouterWeights {
                                        temperature,
                                        difficulty_enabled,
                                        ..Default::default()
                                    },
                                    uniform,
                                    tags: tags.clone(),
                                    difficulty,
                                    allowed: allowed.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }
        out
    }

    #[test]
    fn explain_collapses_rejections_by_reason() {
        let mut small = model("small");
        small.context_window = 4_000;
        let mut tiny = model("tiny");
        tiny.context_window = 2_000;
        let ok = model("ok");
        let cands = vec![healthy(small), healthy(tiny), healthy(ok)];
        let features = RequestFeatures {
            est_input_tokens: 100_000,
            max_tokens: 2_000,
            ..Default::default()
        };
        let ex = explain_selection(
            &cands,
            &features,
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            &Intent::default(),
        );
        assert_self_consistent(&ex);
        assert_eq!(ex.chosen.as_deref(), Some("ok"));
        assert_eq!(ex.scored.len(), 1);
        let ctx = ex
            .rejected
            .iter()
            .find(|r| r.reason == "context_window")
            .unwrap();
        assert_eq!(
            ctx.models.len(),
            2,
            "both too-small models collapse under one reason"
        );
    }

    #[test]
    fn explain_agrees_with_select_model() {
        let cands = varied_candidates();
        let mut outcomes: Vec<Option<String>> = Vec::new();
        for k in knob_matrix() {
            let label = k.label();
            let picked = select_model(
                &cands,
                &RequestFeatures::default(),
                &HashMap::new(),
                k.allowed.as_deref(),
                &k.tags,
                BoonGrants::default(),
                &k.weights,
                k.uniform,
                k.difficulty,
            );
            let ex = explain_selection(
                &cands,
                &RequestFeatures::default(),
                &HashMap::new(),
                k.allowed.as_deref(),
                &k.tags,
                BoonGrants::default(),
                &k.weights,
                k.uniform,
                &intent_at(k.difficulty),
            );
            assert_eq!(
                ex.chosen,
                picked.as_ref().map(|m| m.model_name.clone()),
                "the explanation must never disagree with the real selection [{label}]"
            );
            assert_self_consistent(&ex);
            outcomes.push(ex.chosen);
        }
        outcomes.sort();
        outcomes.dedup();
        assert!(
            outcomes.len() >= 3,
            "the matrix must actually move the pick around, else it proves nothing; got {outcomes:?}"
        );
    }

    #[test]
    fn route_serves_and_explains_from_one_draw() {
        let cands = varied_candidates();
        for k in knob_matrix() {
            let (picked, ex) = route(
                &cands,
                &RequestFeatures::default(),
                &HashMap::new(),
                k.allowed.as_deref(),
                &k.tags,
                BoonGrants::default(),
                &k.weights,
                k.uniform,
                &intent_at(k.difficulty),
            );
            assert_eq!(
                ex.chosen,
                picked.as_ref().map(|m| m.model_name.clone()),
                "the served model and its explanation come from one evaluation"
            );
            assert_self_consistent(&ex);
            // And the same inputs through the serving-only path agree too.
            let alone = select_model(
                &cands,
                &RequestFeatures::default(),
                &HashMap::new(),
                k.allowed.as_deref(),
                &k.tags,
                BoonGrants::default(),
                &k.weights,
                k.uniform,
                k.difficulty,
            );
            assert_eq!(
                picked.map(|m| m.model_name),
                alone.map(|m| m.model_name),
                "route must serve exactly what select_model would"
            );
        }
    }

    #[test]
    fn narration_cannot_move_the_verdict() {
        let cands = varied_candidates();
        for k in knob_matrix() {
            let quiet = evaluate(
                &cands,
                &RequestFeatures::default(),
                &HashMap::new(),
                k.allowed.as_deref(),
                &k.tags,
                BoonGrants::default(),
                &k.weights,
                k.uniform,
                k.difficulty,
                Narration::Off,
            );
            let loud = evaluate(
                &cands,
                &RequestFeatures::default(),
                &HashMap::new(),
                k.allowed.as_deref(),
                &k.tags,
                BoonGrants::default(),
                &k.weights,
                k.uniform,
                k.difficulty,
                Narration::On,
            );
            let names = |ev: &Evaluation<'_>| -> Vec<String> {
                ev.ordered
                    .iter()
                    .map(|s| s.cand.model.model_name.clone())
                    .collect()
            };
            assert_eq!(
                quiet.chosen, loud.chosen,
                "narration moved the chosen index"
            );
            assert_eq!(names(&quiet), names(&loud), "narration reordered the field");
            assert_eq!(quiet.tier_floor, loud.tier_floor);
            assert_eq!(quiet.tier_floor_clamped, loud.tier_floor_clamped);
            let scores =
                |ev: &Evaluation<'_>| -> Vec<f64> { ev.ordered.iter().map(|s| s.score).collect() };
            assert_eq!(
                scores(&quiet),
                scores(&loud),
                "narration changed a score; it must be output-only"
            );
            // ...and the quiet path really did skip the narration allocations.
            assert!(
                quiet.rejected.is_empty(),
                "the serving path must not build rejection strings it discards"
            );
            if !k.weights.difficulty_enabled {
                assert!(
                    quiet.tier_domains.is_empty(),
                    "with tiering off the serving path must not allocate tier domains"
                );
            }
        }
    }

    #[test]
    fn explain_marks_the_chosen_row_and_orders_by_score() {
        let mut cheap = model("cheap");
        cheap.input_cost_per_token = 0.000_001;
        let mut pricey = model("pricey");
        pricey.input_cost_per_token = 0.000_100;
        let cands = vec![healthy(pricey), healthy(cheap)];
        let ex = explain_selection(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            &Intent::default(),
        );
        assert_self_consistent(&ex);
        assert_eq!(ex.scored[0].model, "cheap");
        assert!(ex.scored[0].chosen);
        assert!(!ex.scored[1].chosen);
        assert!(
            ex.scored[0].score >= ex.scored[1].score,
            "rows must arrive in the router's own descending-score order"
        );
        assert!(!ex.sampled, "temperature 0 is never a sampled pick");
    }

    #[test]
    fn explain_flags_the_clamp_only_when_it_fires() {
        let cands = vec![
            tiered("weak", 0.000_001, &[("coding", 1)]),
            tiered("middling", 0.000_010, &[("coding", 2)]),
        ];
        let desired = vec!["coding".to_string()];
        let reachable = explain_selection(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &desired,
            BoonGrants::default(),
            &tiering_on(),
            0.0,
            &intent_at(2),
        );
        assert_self_consistent(&reachable);
        assert!(!reachable.tier_floor_clamped);
        assert_eq!(reachable.tier_floor, 2);
        assert_eq!(reachable.tier_domains, desired);
        let below = reachable
            .rejected
            .iter()
            .find(|r| r.reason == "below_tier_floor")
            .expect("the weak model is dropped by the tier filter");
        assert_eq!(below.models, vec!["weak".to_string()]);

        // Nothing holds coding:3, so the floor clamps down to 2.
        let clamped = explain_selection(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &desired,
            BoonGrants::default(),
            &tiering_on(),
            0.0,
            &intent_at(3),
        );
        assert_self_consistent(&clamped);
        assert!(
            clamped.tier_floor_clamped,
            "asking for a level nothing holds must report the clamp"
        );
        assert_eq!(clamped.tier_floor, 2);
        assert_eq!(clamped.chosen.as_deref(), Some("middling"));
    }

    #[test]
    fn explain_reports_a_sampled_pick() {
        let mut cheap = model("cheap");
        cheap.input_cost_per_token = 0.000_001;
        let mut pricey = model("pricey");
        pricey.input_cost_per_token = 0.000_100;
        let cands = vec![healthy(pricey), healthy(cheap)];
        let w = RouterWeights {
            temperature: 1.0,
            ..Default::default()
        };
        let explain_with = |uniform: f64| {
            explain_selection(
                &cands,
                &RequestFeatures::default(),
                &HashMap::new(),
                None,
                &[],
                BoonGrants::default(),
                &w,
                uniform,
                &Intent::default(),
            )
        };
        let argmax = explain_with(0.0);
        assert_self_consistent(&argmax);
        assert_eq!(argmax.chosen.as_deref(), Some("cheap"));
        assert!(
            !argmax.sampled,
            "landing on the argmax is not a sampled pick"
        );
        let drawn = explain_with(0.999);
        assert_self_consistent(&drawn);
        assert_eq!(drawn.chosen.as_deref(), Some("pricey"));
        assert!(drawn.sampled);
        assert!(drawn.scored[1].chosen, "the marked row follows the draw");
    }

    #[test]
    fn explain_reports_no_model_without_inventing_one() {
        let mut small = model("small");
        small.context_window = 4_000;
        let cands = vec![healthy(small)];
        let features = RequestFeatures {
            est_input_tokens: 100_000,
            max_tokens: 2_000,
            ..Default::default()
        };
        let ex = explain_selection(
            &cands,
            &features,
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            &Intent::default(),
        );
        assert_self_consistent(&ex);
        assert!(ex.chosen.is_none());
        assert!(ex.scored.is_empty());
        assert_eq!(ex.rejected.len(), 1);
        assert_eq!(ex.rejected[0].reason, "context_window");
    }

    #[test]
    fn explain_attributes_each_model_to_its_first_failing_filter() {
        // Disabled *and* unhealthy *and* too small: only the first filter in
        // the chain may claim it, or the collapsed counts double-count.
        let mut broken = model("broken");
        broken.enabled = false;
        broken.context_window = 10;
        let cands = vec![
            Candidate {
                model: broken,
                healthy: false,
                levels: Vec::new(),
            },
            healthy(model("ok")),
        ];
        let features = RequestFeatures {
            est_input_tokens: 50_000,
            ..Default::default()
        };
        let ex = explain_selection(
            &cands,
            &features,
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            &Intent::default(),
        );
        assert_self_consistent(&ex);
        assert_eq!(ex.rejected.len(), 1);
        assert_eq!(ex.rejected[0].reason, "disabled");
        assert_eq!(ex.rejected[0].models, vec!["broken".to_string()]);
    }

    #[test]
    fn explain_carries_intent_provenance_and_leaves_timing_to_the_caller() {
        let cands = vec![healthy(model("only"))];
        let intent = Intent {
            tags: vec!["coding".to_string()],
            difficulty: 2,
            source: IntentSource::Classifier,
        };
        let ex = explain_selection(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
            None,
            &intent.tags,
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            &intent,
        );
        assert_self_consistent(&ex);
        assert_eq!(ex.difficulty, 2);
        assert_eq!(ex.difficulty_source, IntentSource::Classifier);
        assert_eq!(ex.tag_source, IntentSource::Classifier);
        assert_eq!(ex.tags, vec!["coding".to_string()]);
        assert_eq!(ex.classifier_ms, 0, "only the proxy knows the real timing");
        assert!(serde_json::to_string(&ex).is_ok());
    }
}
