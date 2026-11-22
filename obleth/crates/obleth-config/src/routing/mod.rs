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
//!
//! This module lives in `obleth-config` rather than next to the data plane
//! because both the proxy (which serves `auto` requests) and the Management API
//! (which simulates them for the routing tuner) need the identical core, and the
//! dependency graph runs admin -> config/store, never admin -> proxy. The
//! runtime-only pieces — the hot-swappable `ModelRegistry` and the clock-seeded
//! uniform draw — stay in `obleth-proxy`, which re-exports everything here so no
//! data-plane call site has to name a different path.

mod reading;
pub mod route_explain;
pub mod sticky;

use std::collections::HashMap;

use crate::ResolvedModel;
use std::sync::Arc;

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
    /// With tiering on, serve a request from the lowest tier that meets its
    /// difficulty and has room, instead of from any tier at or above it.
    pub right_size: bool,
}

impl RouterWeights {
    pub fn from_settings(s: &crate::AutoRouterSettings) -> Self {
        RouterWeights {
            capacity: s.capacity_weight,
            cost: s.cost_weight,
            tag: s.tag_weight,
            soft_cap: if s.default_soft_cap > 0 {
                s.default_soft_cap as f64
            } else {
                crate::AutoRouterSettings::default().default_soft_cap as f64
            },
            temperature: s.temperature.max(0.0),
            difficulty_enabled: s.difficulty_enabled,
            right_size: s.difficulty_enabled && s.right_size_enabled,
        }
    }

    /// True when the pick is a softmax draw rather than the exact argmax, i.e.
    /// when the `uniform` a caller passes in is actually read.
    ///
    /// Exposed so a caller can skip *producing* a draw nobody will look at
    /// (`splitmix_uniform` is a clock read) without restating the threshold —
    /// [`pick_index`] is the single definition of it, and a copy in a call site
    /// would be exactly the drift this module's contract forbids.
    pub fn samples(&self) -> bool {
        self.temperature > f64::EPSILON
    }
}

impl Default for RouterWeights {
    fn default() -> Self {
        RouterWeights::from_settings(&crate::AutoRouterSettings::default())
    }
}

/// A model the `auto` router may choose from, plus the liveness signal the
/// router needs that is not part of [`ResolvedModel`].
#[derive(Debug, Clone)]
pub struct Candidate {
    /// Shared with every request routed between registry refreshes: the
    /// router hands out this `Arc`, never a copy of the model.
    pub model: Arc<ResolvedModel>,
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
///
/// Only chat candidates take part. `auto` is a chat-completions convenience and
/// [`evaluate`]'s `model_type` hard filter drops every other modality anyway, so
/// letting embedding, image or audio rows occupy cost ranks here would only
/// distort the ladder the chat models are ranked on — on a fleet with cheap
/// embedding models, every real chat model would be pushed into the top bands.
/// Non-chat candidates keep an empty level list.
pub fn derive_levels(candidates: &mut [Candidate], source: crate::TierSource) {
    use crate::{TierSource, MAX_TIER_LEVEL};

    let eligible = |c: &Candidate| c.model.model_type == crate::DEFAULT_MODEL_TYPE;

    let mut domains: Vec<String> = vec![GENERAL_DOMAIN.to_string()];
    for c in candidates.iter().filter(|c| eligible(c)) {
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
            .filter(|&i| eligible(&candidates[i]))
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
            // A request with no topic is ranked on a model's overall strength:
            // the best level it declares anywhere. Without this every model
            // held the same level on the general domain, and the difficulty
            // of a request that named no topic changed nothing.
            let declared = if domain == GENERAL_DOMAIN {
                candidates[i]
                    .model
                    .declared_levels
                    .iter()
                    .map(|(_, l)| *l)
                    .max()
            } else {
                candidates[i]
                    .model
                    .declared_levels
                    .iter()
                    .find(|(d, _)| d == domain)
                    .map(|(_, l)| *l)
            };
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
    /// The request carries image parts. A model that cannot see them would
    /// silently answer from the text alone, so this is a hard requirement,
    /// not a preference the tag score can trade away.
    pub needs_vision: bool,
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

        // Same message-part scan shape as `heuristic_intent`, but as a hard
        // capability: any image part makes vision a requirement.
        let needs_vision = json
            .get("messages")
            .and_then(|m| m.as_array())
            .is_some_and(|messages| messages.iter().any(has_image_part));

        Self {
            est_input_tokens,
            max_tokens,
            needs_function_calling,
            needs_tool_choice,
            needs_response_schema,
            needs_vision,
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
    /// The vision boon is enabled and points at a describer: models with the
    /// `vision` boon can serve image requests (the gateway relays images to
    /// the describer at dispatch time).
    pub vision_active: bool,
}

impl BoonGrants {
    /// Snapshot the grants from the live boon settings.
    pub fn from_settings(settings: &crate::BoonSettings) -> Self {
        BoonGrants {
            structured_active: settings.structured_output.active(),
            vision_active: settings.vision.active(),
        }
    }
}

/// Every hard-filter reason string, in the order the filters are applied. A
/// candidate is attributed to the *first* filter it fails, so this order is
/// part of the explanation's contract, not just presentation.
const REJECTION_REASONS: [&str; 15] = [
    "disabled",
    "staged",
    "retiring",
    "auto_excluded",
    "unhealthy",
    "model_type",
    "context_window",
    "function_calling",
    "tool_choice",
    "response_schema",
    "vision",
    "tenant_allowlist",
    "below_tier_floor",
    "tier_full",
    "above_needed_tier",
];

/// Expected completion length, in tokens, for a model the gateway has not yet
/// observed answering. Used by the per-request cost estimate in [`evaluate`]:
/// with no history every model gets this same figure, so ranking degrades to
/// the old unit-price comparison instead of inventing a difference.
pub const DEFAULT_EXPECTED_OUTPUT_TOKENS: f64 = 500.0;

/// Fewest answers in the window before a model's windowed mean is trusted
/// over the live average. Below it, a handful of long agent replies would
/// decide the figure.
pub const WINDOWED_OUTPUT_MIN_ANSWERS: u64 = 200;

/// Per-model expected completion length, shared between the data plane (which
/// records every settled request) and anything that scores candidates (the
/// router, the admin simulate endpoint, routing readiness): same process, same
/// numbers, so the playground cannot drift from serving.
///
/// Two sources, the first that has an answer wins:
/// - **windowed**: the mean over a recent window of the shared request ledger,
///   refreshed periodically and identical on every gateway replica. Only set
///   for models with at least [`WINDOWED_OUTPUT_MIN_ANSWERS`] answers.
/// - **live**: an in-memory EWMA of this process's own requests, for models
///   the ledger has too little on (new models, a cold ledger). With alpha 0.2
///   it mostly reflects the last few answers, which is why it is only the
///   fallback.
#[derive(Clone, Default)]
pub struct OutputStats {
    live: std::sync::Arc<std::sync::RwLock<HashMap<String, f64>>>,
    windowed: std::sync::Arc<std::sync::RwLock<HashMap<String, WindowedOutput>>>,
}

/// One model's completion lengths over the ledger window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WindowedOutput {
    pub mean: f64,
    pub answers: u64,
}

impl OutputStats {
    const ALPHA: f64 = 0.2;

    /// Record one settled request's completion length for `model`.
    pub fn observe(&self, model: &str, output_tokens: u64) {
        let mut map = match self.live.write() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        let sample = output_tokens as f64;
        match map.get_mut(model) {
            Some(avg) => *avg = *avg + Self::ALPHA * (sample - *avg),
            None => {
                map.insert(model.to_string(), sample);
            }
        }
    }

    /// Replace the windowed figures with a fresh read of the ledger. Models
    /// under [`WINDOWED_OUTPUT_MIN_ANSWERS`] are dropped here, so they fall
    /// back to the live average.
    pub fn set_windowed(&self, figures: HashMap<String, WindowedOutput>) {
        let kept = figures
            .into_iter()
            .filter(|(_, w)| w.answers >= WINDOWED_OUTPUT_MIN_ANSWERS && w.mean.is_finite())
            .collect();
        match self.windowed.write() {
            Ok(mut g) => *g = kept,
            Err(poisoned) => *poisoned.into_inner() = kept,
        }
    }

    /// The windowed figures alone: models the ledger has enough answers on.
    pub fn windowed(&self) -> HashMap<String, WindowedOutput> {
        match self.windowed.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    /// Snapshot for one scoring pass: the windowed mean where there is one,
    /// else the live average. Auto requests only, so the clone is off the hot
    /// path for every request that names its model.
    pub fn snapshot(&self) -> HashMap<String, f64> {
        let mut out = match self.live.read() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        for (model, w) in self.windowed() {
            out.insert(model, w.mean);
        }
        out
    }
}

/// One scored survivor, holding a borrow of its candidate so [`select_model`]
/// can hand out the winning model's `Arc` without re-running anything.
struct ScoredRef<'a> {
    cand: &'a Candidate,
    level: u8,
    spare: f64,
    cost_score: f64,
    /// Estimated dollars for THIS request on this model: unit prices times
    /// the request's prompt estimate and the model's observed completion
    /// length. What `cost_score` normalizes over.
    est_cost: f64,
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
    rejected: Vec<route_explain::Rejection>,
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
    expected_output: &HashMap<String, f64>,
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
    let schema_via_boon =
        |c: &Candidate| grants.structured_active && c.model.has_boon(crate::Boon::StructuredOutput);
    // Likewise vision: the vision boon relays images to a describer, so a
    // text-only model that carries it can serve an image request.
    let vision_via_boon =
        |c: &Candidate| grants.vision_active && c.model.has_boon(crate::Boon::Vision);

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
    let finish = |buckets: Vec<Vec<String>>| -> Vec<route_explain::Rejection> {
        REJECTION_REASONS
            .iter()
            .zip(buckets)
            .filter(|(_, models)| !models.is_empty())
            .map(|(reason, models)| route_explain::Rejection { reason, models })
            .collect()
    };

    // ---- stage 1: hard filters ----
    // First failing filter wins; the order below is the filter order.
    let hard_reject = |c: &Candidate| -> Option<&'static str> {
        if !c.model.enabled {
            return Some("disabled");
        }
        // A staged model is served by name for testing, but `auto` must not
        // send traffic to it before it goes live. A deprecated model is still
        // served by name, but `auto` must not start sending traffic to
        // something on its way out; a retired one is not served at all.
        match c.model.lifecycle.status_at(chrono::Utc::now()) {
            crate::ModelStatus::Active => {}
            crate::ModelStatus::Staged => return Some("staged"),
            crate::ModelStatus::Deprecated | crate::ModelStatus::Retired => {
                return Some("retiring")
            }
        }
        // Operator policy, checked ahead of health so the explanation reads as
        // a deliberate exclusion rather than a transient outage. The model stays
        // addressable by name; only `auto` skips it.
        if !c.model.auto_eligible {
            return Some("auto_excluded");
        }
        if !c.healthy {
            return Some("unhealthy");
        }
        // `auto` is a chat-completions convenience; only chat models are
        // eligible. Non-chat modalities (embedding, image, audio) are addressed
        // by name on their dedicated endpoints.
        if c.model.model_type != crate::DEFAULT_MODEL_TYPE {
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
        // A model that cannot see the request's images would answer from the
        // text alone and look successful while ignoring half the prompt —
        // silent enough that no tag weight is allowed to trade it away.
        if features.needs_vision && !c.model.supports_vision && !vision_via_boon(c) {
            return Some("vision");
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
        let requested = difficulty.clamp(1, crate::MAX_TIER_LEVEL);
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

    // ---- stage 2b: the smallest sufficient tier ----
    // The floor says what a request needs. A model above it costs more and
    // gives the request nothing it asked for, so with `right_size` the request
    // is served from the lowest tier at or above the floor that has a model
    // with room. A tier whose models are all at their caps gives way to the
    // next one up: load spills upward, never down. When no tier has room,
    // every survivor competes as before.
    let eligible: Vec<&Candidate> = if weights.right_size && weights.difficulty_enabled {
        let has_room = |c: &Candidate| {
            let in_flight = busyness.get(&c.model.model_name).copied().unwrap_or(0) as f64;
            let cap = match c.model.max_in_flight {
                Some(cap) if cap > 0 => cap as f64,
                _ => weights.soft_cap,
            };
            in_flight < cap
        };
        let mut tiers: Vec<u8> = eligible.iter().map(|c| c.strength(&tier_domains)).collect();
        tiers.sort_unstable();
        tiers.dedup();
        let target = tiers.into_iter().find(|tier| {
            eligible
                .iter()
                .any(|c| c.strength(&tier_domains) == *tier && has_room(c))
        });
        match target {
            Some(target) => {
                let mut kept = Vec::with_capacity(eligible.len());
                for c in eligible {
                    let strength = c.strength(&tier_domains);
                    if strength == target {
                        kept.push(c);
                    } else if strength < target {
                        reject("tier_full", &c.model.model_name);
                    } else {
                        reject("above_needed_tier", &c.model.model_name);
                    }
                }
                kept
            }
            None => eligible,
        }
    } else {
        eligible
    };

    // ---- stage 3: scoring ----
    // Cost is estimated PER REQUEST, not per token: unit prices weighted by
    // the request's prompt estimate and each model's observed completion
    // length. Unit prices alone made an unbounded thinker look "cheapest"
    // while it wrote 50x the tokens of every other candidate; the observed
    // average is what turns that into the cost it actually is. Models with no
    // history all get the same default, which degrades exactly to the old
    // unit-price ranking rather than inventing a difference.
    let est_in = features.est_input_tokens as f64;
    let costs: Vec<f64> = eligible
        .iter()
        .map(|c| {
            let expected_out = expected_output
                .get(&c.model.model_name)
                .copied()
                .unwrap_or(DEFAULT_EXPECTED_OUTPUT_TOKENS);
            c.model.input_cost_per_token * est_in + c.model.output_cost_per_token * expected_out
        })
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
            est_cost: *cost,
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
    if !weights.samples() {
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
    expected_output: &HashMap<String, f64>,
    allowed_models: Option<&[String]>,
    desired_tags: &[String],
    grants: BoonGrants,
    weights: &RouterWeights,
    uniform: f64,
    difficulty: u8,
) -> Option<Arc<ResolvedModel>> {
    select_model_among(
        candidates,
        features,
        busyness,
        expected_output,
        allowed_models,
        desired_tags,
        grants,
        weights,
        uniform,
        difficulty,
    )
    .0
}

/// A model that was scored for an `auto` request and not chosen.
#[derive(Clone, Debug, PartialEq)]
pub struct RunnerUp {
    pub model: String,
    /// How far its score was from the chosen model's, never negative: `0.0` is
    /// a tie, and a small figure says the pick could as well have gone here.
    pub gap: f64,
}

/// How many runners-up an `auto` pick names.
pub const RUNNERS_UP: usize = 2;

/// What an `auto` pick was made among.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Among {
    /// The models that were scored: those that passed the hard filters and the
    /// difficulty floor.
    pub count: usize,
    /// The best-scored models that were not chosen, at most [`RUNNERS_UP`], in
    /// the router's order.
    pub next: Vec<RunnerUp>,
}

impl Among {
    /// From the scored models in the router's order, as (name, score), and the
    /// index of the one chosen.
    pub fn of<'a>(
        scored: impl ExactSizeIterator<Item = (&'a str, f64)>,
        chosen: Option<usize>,
    ) -> Self {
        let count = scored.len();
        let Some(chosen) = chosen else {
            return Self {
                count,
                next: Vec::new(),
            };
        };
        let mut picked = f64::NAN;
        let mut others = Vec::with_capacity(RUNNERS_UP);
        for (i, (model, score)) in scored.enumerate() {
            if i == chosen {
                picked = score;
            } else if others.len() < RUNNERS_UP {
                others.push((model, score));
            } else if i > chosen {
                break;
            }
        }
        let next = others
            .into_iter()
            .map(|(model, score)| RunnerUp {
                model: model.to_owned(),
                gap: if picked.is_finite() && score.is_finite() {
                    (picked - score).abs()
                } else {
                    0.0
                },
            })
            .collect();
        Self { count, next }
    }
}

impl route_explain::RouteExplain {
    /// What this pick was made among, the same as [`select_model_among`] gives
    /// for the same evaluation.
    pub fn among(&self) -> Among {
        Among::of(
            self.scored.iter().map(|s| (s.model.as_str(), s.score)),
            self.scored.iter().position(|s| s.chosen),
        )
    }
}

/// [`select_model`], and what the pick was made among: how many models passed
/// the hard filters and the difficulty floor and so were scored, which is the
/// count `x-obleth-auto-why` reports (not the size of the registry), and the
/// best of them that were not chosen, which `x-obleth-auto-next` names.
#[allow(clippy::too_many_arguments)]
pub fn select_model_among(
    candidates: &[Candidate],
    features: &RequestFeatures,
    busyness: &HashMap<String, usize>,
    expected_output: &HashMap<String, f64>,
    allowed_models: Option<&[String]>,
    desired_tags: &[String],
    grants: BoonGrants,
    weights: &RouterWeights,
    uniform: f64,
    difficulty: u8,
) -> (Option<Arc<ResolvedModel>>, Among) {
    let ev = evaluate(
        candidates,
        features,
        busyness,
        expected_output,
        allowed_models,
        desired_tags,
        grants,
        weights,
        uniform,
        difficulty,
        Narration::Off,
    );
    let among = Among::of(
        ev.ordered
            .iter()
            .map(|s| (s.cand.model.model_name.as_str(), s.score)),
        ev.chosen,
    );
    (ev.chosen.map(|i| ev.ordered[i].cand.model.clone()), among)
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
///
/// Called from the data plane's `auto` branch only when a request is traced;
/// the untraced path calls [`select_model`] instead so narration is never
/// built for a span nobody records.
#[allow(clippy::too_many_arguments)]
pub fn route(
    candidates: &[Candidate],
    features: &RequestFeatures,
    busyness: &HashMap<String, usize>,
    expected_output: &HashMap<String, f64>,
    allowed_models: Option<&[String]>,
    desired_tags: &[String],
    grants: BoonGrants,
    weights: &RouterWeights,
    uniform: f64,
    intent: &Intent,
) -> (Option<Arc<ResolvedModel>>, route_explain::RouteExplain) {
    let ev = evaluate(
        candidates,
        features,
        busyness,
        expected_output,
        allowed_models,
        desired_tags,
        grants,
        weights,
        uniform,
        intent.difficulty,
        Narration::On,
    );
    let picked = ev.chosen.map(|i| ev.ordered[i].cand.model.clone());
    (picked, narrate(ev, desired_tags, weights, intent, uniform))
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
#[allow(clippy::too_many_arguments)]
pub fn explain_selection(
    candidates: &[Candidate],
    features: &RequestFeatures,
    busyness: &HashMap<String, usize>,
    expected_output: &HashMap<String, f64>,
    allowed_models: Option<&[String]>,
    desired_tags: &[String],
    grants: BoonGrants,
    weights: &RouterWeights,
    uniform: f64,
    intent: &Intent,
) -> route_explain::RouteExplain {
    let ev = evaluate(
        candidates,
        features,
        busyness,
        expected_output,
        allowed_models,
        desired_tags,
        grants,
        weights,
        uniform,
        intent.difficulty,
        Narration::On,
    );
    narrate(ev, desired_tags, weights, intent, uniform)
}

/// Render a finished [`Evaluation`] as the wire shape. Pure presentation: it
/// decides nothing, and is the single place both explaining entry points build
/// their output so they cannot describe the same evaluation differently.
fn narrate(
    ev: Evaluation<'_>,
    desired_tags: &[String],
    weights: &RouterWeights,
    intent: &Intent,
    uniform: f64,
) -> route_explain::RouteExplain {
    route_explain::RouteExplain {
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
        weights: route_explain::WeightsView {
            capacity: weights.capacity,
            cost: weights.cost,
            tag: weights.tag,
            soft_cap: weights.soft_cap,
            difficulty_enabled: weights.difficulty_enabled,
            right_size: weights.right_size,
        },
        temperature: weights.temperature,
        // Echoed verbatim, including at temperature 0 where it was ignored, so
        // a caller can always replay this exact decision.
        uniform,
        // Index-based: the draw moved the pick off the head of the list. See
        // the field's own docs for why that is not quite "beat the argmax".
        sampled: ev.chosen.is_some_and(|i| i != 0),
        scored: ev
            .ordered
            .iter()
            .enumerate()
            .map(|(i, s)| route_explain::ScoredCandidate {
                model: s.cand.model.model_name.clone(),
                level: s.level,
                spare: s.spare,
                cost_score: s.cost_score,
                est_cost: s.est_cost,
                tag_score: s.tag_score,
                bias: s.bias,
                score: s.score,
                chosen: ev.chosen == Some(i),
            })
            .collect(),
        rejected: ev.rejected,
    }
}

/// Where a request's routing intent came from. Surfaced in traces and the
/// tuner so an operator can tell a real classification from a fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
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
///
/// It reads what the classifier reads: the newest user turn with text (back
/// to [`CLASSIFIER_LOOKBACK_TURNS`] when the newest is empty), at most
/// [`HEURISTIC_MAX_BYTES`] of it. System prompts, assistant replies and tool
/// output are not read, so a long system prompt that mentions code or a tool
/// result with a stack trace does not tag every turn. Images count from any
/// user turn: a request carrying one needs a model that can see it.
pub fn heuristic_intent(json: &serde_json::Value, est_input_tokens: u64) -> Intent {
    let user_turns: Vec<&serde_json::Value> = messages(json)
        .iter()
        .filter(|m| role(m) == "user")
        .collect();
    let has_image = user_turns.iter().any(|m| has_image_part(m));
    // What the person said, oldest first, as far back as the classifier reads.
    let mut said: Vec<String> = user_turns
        .iter()
        .rev()
        .take(CLASSIFIER_LOOKBACK_TURNS)
        .map(|m| message_text(m.get("content")))
        .filter(|t| !t.trim().is_empty())
        .collect();
    said.reverse();
    let text = said.last().cloned().unwrap_or_default();
    let text = head_bytes(&text, HEURISTIC_MAX_BYTES);

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
    let mut topics = heuristic_topics(text);
    // A short follow-up ("continue", "that still fails") is about what the
    // request before it was about.
    if topics.is_empty() && said.len() > 1 {
        let lower = text.to_ascii_lowercase();
        if reading::follows_on(lower.trim()) && !reading::is_pleasantry(lower.trim()) {
            topics = heuristic_topics(head_bytes(&said[said.len() - 2], HEURISTIC_MAX_BYTES));
        }
    }
    for topic in topics {
        push(topic);
    }

    if est_input_tokens > HEURISTIC_LONG_CONTEXT_TOKENS {
        push("long-context");
    }

    // Difficulty from what the words show: see `reading`. The middle is the
    // default, so a request that shows nothing is neither sent to the weakest
    // models nor to the strongest. Only the person's own turns are read: the
    // system text and tool definitions a client sends with every message say
    // nothing about how hard this one is.
    let difficulty = reading::difficulty(&said);

    Intent {
        tags,
        difficulty,
        source: IntentSource::Heuristic,
    }
}

/// The topics one message is about, from the fixed vocabulary.
fn heuristic_topics(text: &str) -> Vec<&'static str> {
    let lower = text.to_ascii_lowercase();
    let mut topics = Vec::new();

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
            "c++",
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
            "script",
            "query",
            "dockerfile",
            "docker",
            "bash",
            "awk",
            "pytest",
            "unit tests",
            "git",
            "vim",
            "chmod",
            "syntax",
            "api",
            "schema",
            "parser",
            "mutex",
            "deadlock",
            "deadlocks",
            "race condition",
            "lock-free",
            "refactor",
            "postgres",
            "postgresql",
            "mysql",
            "golang",
            "go service",
            "tokio",
            "panicked",
            "traceback",
            "pandas",
            "numpy",
            "dataframe",
            "df",
            "flask",
            "django",
            "react",
            "npm",
            "pip",
            "conda",
            "json",
            "yaml",
            "endpoint",
            "lambda",
            "loop",
            "typeerror",
            "exception",
            "segfault",
            "compiler",
            "makefile",
            "cmake",
            "kubectl",
            "linux",
            "command",
            "terminal",
            "shell",
            "ssh",
            "tar",
            "grep",
            "sed",
            "cron",
            "etl",
            "openmp",
        ]
        .iter()
        .any(|k| has_word(&lower, k))
        // Text that is itself code or a command line: a call, an option, a
        // source file.
        || ["`", "()", "):", " == ", "::", "->"]
            .iter()
            .any(|k| lower.contains(k))
        || lower
            .split_whitespace()
            .any(|w| w.len() > 3 && w.starts_with("--") && w.as_bytes()[2].is_ascii_alphabetic())
        || [".py", ".js", ".ts", ".rs", ".go", ".cpp", ".sh", ".sql"].iter().any(|ext| {
            lower.match_indices(ext).any(|(at, _)| {
                lower[at + ext.len()..]
                    .chars()
                    .next()
                    .is_none_or(|c| !c.is_alphanumeric())
            })
        });
    if code_signal {
        topics.push("coding");
    }

    // Prose to write or edit. Phrases rather than single words, and never
    // alongside a code signal: "write" and "email" alone match "write a SQL
    // query" and "validate an email address" just as well.
    let writing_signal = !code_signal
        && ([
            "short story",
            "poem",
            "sonnet",
            "essay",
            "cover letter",
            "personal statement",
            "an email",
            "email to",
            "a letter",
            "blog post",
            "newsletter",
            "press release",
            "linkedin post",
            "rewrite",
            "rephrase",
            "reword",
            "proofread",
            "tighten this",
            "polish this",
            "edit this",
            "fix the grammar",
            "more diplomatic",
            "more professional",
            "draft a",
            "polite reply",
            "translate",
            "grammar",
            "less rude",
            "more polite",
            "more formal",
            "friendlier",
            " tone",
            "shorten",
            "tighten",
        ]
        .iter()
        .any(|k| lower.contains(k))
            // A piece of prose asked for by its kind: "write a short
            // thank-you note", "need a linkedin post".
            || (["write", "draft", "compose", "need a", "need an", "give me a"]
                .iter()
                .any(|k| lower.contains(k))
                && [
                    "note",
                    "letter",
                    "email",
                    "message",
                    "description",
                    "post",
                    "bio",
                    "speech",
                    "toast",
                    "abstract",
                    "paragraph",
                    "caption",
                    "announcement",
                    "blurb",
                    "apology",
                    "invitation",
                    "story",
                    "article",
                    "slogan",
                ]
                .iter()
                .any(|k| has_word(&lower, k))));
    if writing_signal {
        topics.push("writing");
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
        "prove",
        "proof",
        "determinant",
        "irrational",
        "converges",
        "closed form",
    ]
    .iter()
    .any(|k| has_word(&lower, k))
        || (lower.contains('=')
            && lower
                .chars()
                .any(|c| matches!(c, '+' | '-' | '*' | '/' | '^' | '∫' | '√' | '∑')))
        || [
            "calculation",
            "critical points",
            "standard deviation",
            "variance",
            "p-value",
            "confidence interval",
            "eigenvalue",
            "polynomial",
            "logarithm",
        ]
        .iter()
        .any(|k| lower.contains(k))
        // A quantity asked for from figures given: a word problem.
        || (reading::figures(&lower) >= 2
            && [
                "how high",
                "how far",
                "how fast",
                "how likely",
                "what's the chance",
                "what is the chance",
                "what are the chances",
                "what fraction",
                "what percent",
            ]
            .iter()
            .any(|k| lower.contains(k)));
    // Beside code, only a proof is mathematics: "solve" and "matrix" are
    // everyday words there.
    if math_signal && !(code_signal && !lower.contains("prove that") && !lower.contains("proof")) {
        topics.push("math");
    }
    topics
}

/// Most bytes of the newest user turn the heuristic reads.
pub const HEURISTIC_MAX_BYTES: usize = 8_000;
/// Estimated tokens of the whole request from which the heuristic tags it
/// `long-context`.
pub const HEURISTIC_LONG_CONTEXT_TOKENS: u64 = 32_000;

/// True when `word` occurs in `text` as a whole word (a letter or digit on
/// neither side), so "java" matches "a java class" and not "javascript", and
/// "bug" matches "a bug" and not "debug".
fn has_word(text: &str, word: &str) -> bool {
    let boundary = |c: Option<char>| c.is_none_or(|c| !c.is_alphanumeric());
    text.match_indices(word).any(|(at, _)| {
        boundary(text[..at].chars().next_back()) && boundary(text[at + word.len()..].chars().next())
    })
}

/// The request's messages, or none.
pub(crate) fn messages(json: &serde_json::Value) -> &[serde_json::Value] {
    json.get("messages")
        .and_then(|m| m.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

/// A message's role, or "" when it has none.
pub(crate) fn role(m: &serde_json::Value) -> &str {
    m.get("role").and_then(|r| r.as_str()).unwrap_or("")
}

/// A message's text: a string content as is, a part list's text parts joined
/// by newlines, anything else empty.
pub(crate) fn message_text(content: Option<&serde_json::Value>) -> String {
    classifier_message_text(content)
}

/// True for a content part that carries an image, in either API's spelling.
pub(crate) fn is_image_part(part: &serde_json::Value) -> bool {
    matches!(
        part.get("type").and_then(|t| t.as_str()),
        Some("image_url") | Some("input_image")
    )
}

/// True when a message's content has an image part.
pub(crate) fn has_image_part(message: &serde_json::Value) -> bool {
    message
        .get("content")
        .and_then(|c| c.as_array())
        .is_some_and(|parts| parts.iter().any(is_image_part))
}

/// Request header naming how many bytes at the end of the system text the
/// classifier may read. See [`classifier_prompts`].
pub const CLASSIFY_SYSTEM_HEADER: &str = "x-obleth-classify-system";

/// Parse [`CLASSIFY_SYSTEM_HEADER`]: a byte count. Anything else is ignored,
/// so the whole system text is read as before.
pub fn classify_system_from_header(raw: Option<&str>) -> Option<usize> {
    raw?.trim().parse().ok()
}

/// Most bytes of request text one classifier call reads.
pub const CLASSIFIER_PROMPT_MAX_BYTES: usize = 2_000;

/// How many user turns, newest first, the classifier may read for one
/// request. See [`classify_turns`].
pub const CLASSIFIER_LOOKBACK_TURNS: usize = 3;

/// The tag every chat request touches. A classification that names nothing
/// more specific says nothing about where the request should go.
const GENERAL_TAG: &str = "general";

/// The classifier prompts for a chat body: one per user turn, newest first,
/// at most [`CLASSIFIER_LOOKBACK_TURNS`].
///
/// Each prompt is the leading system text plus a single user message, so a
/// conversation that opens with "hello" and moves on to code is classified as
/// code. Earlier turns are never mixed into the newest one: given both, the
/// classifier follows the earlier topic, and a request for a script that comes
/// after a request for a poem would route as `writing`. Tool results are not
/// user turns, so an agent's tool loop keeps the classification of the
/// request that started it.
///
/// The user message has first claim on [`CLASSIFIER_PROMPT_MAX_BYTES`] and the
/// system text gets what is left, so a long system prompt cannot push the
/// request itself out of view.
///
/// `system_tail` is the client's [`CLASSIFY_SYSTEM_HEADER`]: only that many
/// bytes at the end of the system text are read. A client whose system text is
/// mostly its own instructions (tools, display formats) sends it, so those
/// instructions do not decide the topic; `Some(0)` reads no system text.
pub fn classifier_prompts(json: &serde_json::Value, system_tail: Option<usize>) -> Vec<String> {
    let messages = messages(json);
    if messages.is_empty() {
        return Vec::new();
    }

    let mut system: Vec<String> = Vec::new();
    for msg in messages {
        match role(msg) {
            "user" => break,
            "system" => {
                let text = classifier_message_text(msg.get("content"));
                if !text.trim().is_empty() {
                    system.push(text);
                }
            }
            _ => {}
        }
    }
    let system = system.join("\n");
    let system = system.trim();
    let system = match system_tail {
        Some(n) => tail_bytes(system, n).trim(),
        None => system,
    };

    messages
        .iter()
        .rev()
        .filter(|msg| role(msg) == "user")
        .map(|msg| classifier_message_text(msg.get("content")))
        .filter(|text| !text.trim().is_empty())
        .take(CLASSIFIER_LOOKBACK_TURNS)
        .map(|text| {
            let user = head_bytes(text.trim(), CLASSIFIER_PROMPT_MAX_BYTES);
            let room = CLASSIFIER_PROMPT_MAX_BYTES.saturating_sub(user.len() + 1);
            let system = head_bytes(system, room).trim_end();
            if system.is_empty() {
                user.to_string()
            } else {
                format!("{system}\n{user}")
            }
        })
        .collect()
}

/// Classify an `auto` request turn by turn, newest first, and return the first
/// answer that names a topic.
///
/// A short follow-up such as "make it shorter" or "thanks" has no topic of its
/// own and classifies as `general`, which on its own would move a coding
/// conversation to a general model halfway through. So when the newest turn
/// names nothing more specific than `general`, the turn before it decides,
/// back to [`CLASSIFIER_LOOKBACK_TURNS`]. Those turns were classified when they
/// were sent, so while the conversation is active this is usually a cache hit.
/// If no turn names a topic, the newest turn's answer stands.
///
/// No tags at all is the classifier's failure shape. It ends the walk with
/// `None` so the caller falls back to heuristics: a slow or broken classifier
/// costs one timeout, never several.
pub async fn classify_turns<F, Fut>(prompts: Vec<String>, mut classify: F) -> Option<Intent>
where
    F: FnMut(String) -> Fut,
    Fut: std::future::Future<Output = Intent>,
{
    let mut newest: Option<Intent> = None;
    for prompt in prompts {
        let intent = classify(prompt).await;
        if intent.tags.is_empty() {
            break;
        }
        if intent.tags.iter().any(|t| t != GENERAL_TAG) {
            return Some(intent);
        }
        newest.get_or_insert(intent);
    }
    newest
}

/// A message's text: a string, or its text parts joined.
fn classifier_message_text(content: Option<&serde_json::Value>) -> String {
    match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// At most `max` bytes from the start of `s`, cut on a char boundary.
/// The last `max` bytes of `s`, cut forward to a character boundary.
fn tail_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

fn head_bytes(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn model(name: &str) -> ResolvedModel {
        ResolvedModel {
            model_name: name.to_string(),
            upstream_model: name.to_string(),
            api_base: "http://upstream".to_string(),
            context_window: 128_000,
            supports_function_calling: true,
            supports_response_schema: true,
            supports_tool_choice: true,
            ..Default::default()
        }
    }

    pub(super) fn healthy(m: ResolvedModel) -> Candidate {
        Candidate {
            model: Arc::new(m),
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
    fn variants_are_never_auto_candidates() {
        let mut m = model("glm-5-3");
        m.boons = vec!["vision".into()];
        m.variants = vec![crate::types::ModelVariant {
            name: "glm-5-3-spec".into(),
            description: String::new(),
            boons: vec!["speculation".into()],
        }];
        let candidates = vec![healthy(m)];
        let pick = |allowed: Option<&[String]>| {
            select_model(
                &candidates,
                &RequestFeatures::default(),
                &HashMap::new(),
                &HashMap::new(),
                allowed,
                &[],
                BoonGrants::default(),
                &RouterWeights::default(),
                0.0,
                1,
            )
        };
        // `auto` picks the model itself, with only its own boons.
        let chosen = pick(None).unwrap();
        assert_eq!(chosen.model_name, "glm-5-3");
        assert_eq!(chosen.boons, vec!["vision".to_string()]);
        // A tenant allowed only the variant has nothing for `auto` to pick:
        // a variant is a name for a model, not a candidate of its own.
        let only_variant = vec!["glm-5-3-spec".to_string()];
        assert!(pick(Some(&only_variant)).is_none());
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
    fn auto_ineligible_model_is_never_selected() {
        let mut a = model("alpha");
        // "alpha" would win the name tie-break, and carries a bias high enough
        // that no scoring weight could demote it — only the hard filter keeps
        // it out.
        a.route_bias = 3.0;
        a.auto_eligible = false;
        let candidates = vec![healthy(a), healthy(model("beta"))];
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            1,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "beta");
    }

    #[test]
    fn excluding_every_model_leaves_auto_with_no_pick() {
        let mut a = model("alpha");
        let mut b = model("beta");
        a.auto_eligible = false;
        b.auto_eligible = false;
        let candidates = vec![healthy(a), healthy(b)];
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            1,
        );
        // Excluding the whole fleet is operator error, but it must surface as
        // "auto has nothing to pick" rather than falling back to an excluded
        // model.
        assert!(chosen.is_none());
    }

    #[test]
    fn exclusion_is_reported_as_operator_policy_not_a_health_problem() {
        let mut a = model("alpha");
        a.auto_eligible = false;
        // Unhealthy *as well*, to pin the filter order: operator intent is the
        // reported reason even when a transient condition also applies.
        let mut cand = healthy(a);
        cand.healthy = false;
        let explain = explain_selection(
            &[cand, healthy(model("beta"))],
            &RequestFeatures::default(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            &Intent::default(),
        );
        let reason = explain
            .rejected
            .iter()
            .find(|r| r.models.iter().any(|m| m == "alpha"))
            .map(|r| r.reason)
            .expect("alpha must appear in the rejection list");
        assert_eq!(reason, "auto_excluded");
    }

    #[test]
    fn auto_never_picks_a_deprecated_or_retired_model() {
        for status in [crate::ModelStatus::Deprecated, crate::ModelStatus::Retired] {
            let mut old = model("old");
            old.lifecycle.status = status;
            let explain = explain_selection(
                &[healthy(old), healthy(model("new"))],
                &RequestFeatures::default(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &[],
                BoonGrants::default(),
                &RouterWeights::default(),
                0.0,
                &Intent::default(),
            );
            let reason = explain
                .rejected
                .iter()
                .find(|r| r.models.iter().any(|m| m == "old"))
                .map(|r| r.reason);
            assert_eq!(reason, Some("retiring"), "{status:?}");
        }
    }

    #[test]
    fn auto_never_picks_a_staged_model() {
        let mut new = model("new");
        new.lifecycle.status = crate::ModelStatus::Staged;
        let explain = explain_selection(
            &[healthy(new), healthy(model("live"))],
            &RequestFeatures::default(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            &Intent::default(),
        );
        let reason = explain
            .rejected
            .iter()
            .find(|r| r.models.iter().any(|m| m == "new"))
            .map(|r| r.reason);
        assert_eq!(reason, Some("staged"));
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

    /// Between registry refreshes every request gets the candidate's own
    /// `Arc`, not a copy of the model.
    #[test]
    fn the_pick_is_the_candidates_own_arc() {
        let candidates = vec![healthy(model("solo"))];
        let pick = || {
            select_model(
                &candidates,
                &RequestFeatures::default(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &[],
                BoonGrants::default(),
                &RouterWeights::default(),
                0.0,
                0,
            )
            .expect("picked")
        };
        let (a, b) = (pick(), pick());
        assert!(Arc::ptr_eq(&a, &candidates[0].model));
        assert!(Arc::ptr_eq(&a, &b));
    }

    #[test]
    fn unhealthy_candidates_are_skipped() {
        let mut down = Candidate {
            model: Arc::new(model("down")),
            healthy: false,
            levels: Vec::new(),
        };
        Arc::make_mut(&mut down.model).input_cost_per_token = 0.0; // would otherwise be cheapest
        let up = healthy(model("up"));
        let candidates = vec![down, up];
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
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
            &HashMap::new(),
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
    fn heuristic_intent_detects_code_and_vision() {
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

    /// The heuristic reads what the classifier reads: the newest user turn.
    /// A system prompt or a tool result that mentions code does not make a
    /// plain question a coding request; a user turn asking for code still does.
    #[test]
    fn heuristic_intent_reads_only_the_newest_user_turn() {
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "You are a coding assistant. Fix every bug; python, rust and sql are fair game. ```never mind```"},
                {"role": "user", "content": "Find the bug in this python function: ```def f(): pass```"},
                {"role": "assistant", "content": "", "tool_calls": [{"id": "c1", "type": "function", "function": {"name": "run", "arguments": "{}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "thread 'main' panicked at src/x.rs:12\nstack backtrace:\n 0: foo"},
                {"role": "user", "content": "What is the capital of France?"}
            ]
        });
        let intent = heuristic_intent(&body, 10);
        assert!(intent.tags.is_empty(), "{:?}", intent.tags);
        assert_eq!(intent.difficulty, 1);
        // The newest turn asks for code.
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "Be brief."},
                {"role": "user", "content": "What is the capital of France?"},
                {"role": "assistant", "content": "Paris."},
                {"role": "user", "content": "Now write a python function that reverses a string."}
            ]
        });
        assert!(heuristic_intent(&body, 10)
            .tags
            .contains(&"coding".to_string()));
        // An empty newest turn falls back to the one before it.
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": "Write a regex that validates an email address."},
                {"role": "user", "content": [{"type": "image_url", "image_url": {"url": "http://x/y.png"}}]}
            ]
        });
        let tags = heuristic_intent(&body, 10).tags;
        assert!(
            tags.contains(&"coding".to_string()) && tags.contains(&"vision".to_string()),
            "{tags:?}"
        );
    }

    #[test]
    fn heuristic_keywords_match_whole_words() {
        for (prompt, coding) in [
            ("I need to debug my feelings about javascripting", false),
            ("A java class that reads a file", true),
            ("How do I compile this?", true),
            ("Explain the rustic charm of this village", false),
        ] {
            let body = serde_json::json!({ "messages": [{"role": "user", "content": prompt}] });
            assert_eq!(
                heuristic_intent(&body, 10)
                    .tags
                    .contains(&"coding".to_string()),
                coding,
                "{prompt}"
            );
        }
    }

    #[test]
    fn heuristic_intent_tags_long_context() {
        let body = serde_json::json!({ "messages": [{"role": "user", "content": "hello"}] });
        let tags = heuristic_intent(&body, 40_000).tags;
        assert!(tags.contains(&"long-context".to_string()));
    }

    #[test]
    fn heuristic_intent_tags_prose_to_write_or_edit() {
        for prompt in [
            "Draft an email to my thesis committee thanking them.",
            "Can you proofread my cover letter?",
            "Rewrite this paragraph so it reads more naturally.",
            "Make this message more diplomatic: you missed the deadline again.",
        ] {
            let body = serde_json::json!({ "messages": [{"role": "user", "content": prompt}] });
            let tags = heuristic_intent(&body, 10).tags;
            assert_eq!(tags, vec!["writing".to_string()], "{prompt}");
        }
    }

    #[test]
    fn heuristic_intent_does_not_call_code_writing() {
        for prompt in [
            "Write a SQL query that counts orders per customer.",
            "Rewrite this python function to use a generator.",
            "Write a regex that validates an email address.",
        ] {
            let body = serde_json::json!({ "messages": [{"role": "user", "content": prompt}] });
            let tags = heuristic_intent(&body, 10).tags;
            assert!(tags.contains(&"coding".to_string()), "{prompt}");
            assert!(!tags.contains(&"writing".to_string()), "{prompt}");
        }
        let body = serde_json::json!({ "messages": [{"role": "user", "content": "What is the capital of France?"}] });
        assert!(heuristic_intent(&body, 10).tags.is_empty());
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
    fn heuristic_difficulty_is_moderate_for_a_long_prompt_that_shows_nothing_else() {
        let body =
            serde_json::json!({ "messages": [{"role":"user","content":"word ".repeat(1_700)}] });
        assert_eq!(heuristic_intent(&body, 2_200).difficulty, 2);
    }

    #[test]
    fn a_greeting_with_a_long_system_prompt_and_tools_is_still_simple() {
        // What a chat client sends with every message: instructions and tool
        // definitions, several thousand tokens of them, around one short turn.
        let body = serde_json::json!({
            "messages": [
                {"role":"system","content":"You have tools. ".repeat(1_200)},
                {"role":"user","content":"Helloo"},
            ],
            "tools": [{"type":"function","function":{"name":"run","description":"d".repeat(4_000)}}],
        });
        let intent = heuristic_intent(&body, 5_700);
        assert_eq!(intent.difficulty, 1);
        assert!(intent.tags.is_empty());
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
            vision_active: false,
        };
        assert!(select_model(
            &candidates,
            &features,
            &HashMap::new(),
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
            vision_active: false,
        };
        let chosen = select_model(
            &candidates,
            &features,
            &HashMap::new(),
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
    fn image_request_filters_out_text_only_models() {
        let mut blind = model("blind");
        blind.supports_vision = false;
        blind.input_cost_per_token = 0.0; // would otherwise win on cost
        let mut sighted = model("sighted");
        sighted.supports_vision = true;
        sighted.input_cost_per_token = 0.000_100;
        sighted.output_cost_per_token = 0.000_100;
        let candidates = vec![healthy(blind), healthy(sighted)];
        let features = RequestFeatures {
            needs_vision: true,
            ..Default::default()
        };
        let chosen = select_model(
            &candidates,
            &features,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            1,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "sighted");
    }

    #[test]
    fn vision_boon_satisfies_the_vision_filter() {
        let mut relayed = model("relayed");
        relayed.supports_vision = false;
        relayed.boons = vec!["vision".to_string()];
        let candidates = vec![healthy(relayed)];
        let features = RequestFeatures {
            needs_vision: true,
            ..Default::default()
        };
        // Boon carried but not active gateway-wide: still filtered.
        assert!(select_model(
            &candidates,
            &features,
            &HashMap::new(),
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
            structured_active: false,
            vision_active: true,
        };
        let chosen = select_model(
            &candidates,
            &features,
            &HashMap::new(),
            &HashMap::new(),
            None,
            &[],
            grants,
            &RouterWeights::default(),
            0.0,
            1,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "relayed");
    }

    #[test]
    fn features_detect_image_parts() {
        let body = serde_json::json!({
            "messages": [{"role":"user","content":[
                {"type":"text","text":"what is in this picture?"},
                {"type":"image_url","image_url":{"url":"http://example.invalid/x.png"}}
            ]}]
        });
        assert!(RequestFeatures::from_request(&body, 10, 0).needs_vision);
        let text_only = serde_json::json!({
            "messages": [{"role":"user","content":"hello"}]
        });
        assert!(!RequestFeatures::from_request(&text_only, 10, 0).needs_vision);
    }

    #[test]
    fn observed_output_length_beats_unit_price() {
        // The thinker is cheapest per token but writes 25k tokens per answer;
        // the coder costs more per token and writes 500. Per REQUEST the coder
        // is cheaper, and that is the cost the router must rank on.
        let mut thinker = model("thinker");
        thinker.input_cost_per_token = 0.000_000_05;
        thinker.output_cost_per_token = 0.000_000_1;
        let mut coder = model("coder");
        coder.input_cost_per_token = 0.000_000_07;
        coder.output_cost_per_token = 0.000_000_27;
        let candidates = vec![healthy(thinker), healthy(coder)];

        // No history: unit prices decide, thinker looks cheapest.
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            1,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "thinker");

        // With observed completion lengths the ranking flips.
        let stats = OutputStats::default();
        stats.observe("thinker", 25_000);
        stats.observe("coder", 500);
        let chosen = select_model(
            &candidates,
            &RequestFeatures::default(),
            &HashMap::new(),
            &stats.snapshot(),
            None,
            &[],
            BoonGrants::default(),
            &RouterWeights::default(),
            0.0,
            1,
        )
        .unwrap();
        assert_eq!(chosen.model_name, "coder");
    }

    #[test]
    fn output_stats_ewma_converges_and_defaults_apply() {
        let stats = OutputStats::default();
        stats.observe("m", 1_000);
        assert_eq!(stats.snapshot().get("m").copied(), Some(1_000.0));
        for _ in 0..50 {
            stats.observe("m", 100);
        }
        let avg = stats.snapshot()["m"];
        assert!(
            avg < 150.0,
            "EWMA must converge toward recent behavior, got {avg}"
        );
        assert!(!stats.snapshot().contains_key("unseen"));
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
            vision_active: false,
        };
        assert!(select_model(
            &candidates,
            &features,
            &HashMap::new(),
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
            &HashMap::new(),
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
            &HashMap::new(),
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
        derive_levels(&mut cands, crate::TierSource::Derived);
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
        derive_levels(&mut cands, crate::TierSource::Derived);
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
        derive_levels(&mut cands, crate::TierSource::Hybrid);
        assert_eq!(level_of(&cands[0], "coding"), 3, "declared wins");
        assert_eq!(level_of(&cands[0], "math"), 1, "other tags stay derived");
    }

    #[test]
    fn every_candidate_has_a_level_in_the_general_domain() {
        let mut untagged = model("untagged");
        untagged.tags = Vec::new();
        let mut cands = vec![healthy(untagged)];
        derive_levels(&mut cands, crate::TierSource::Hybrid);
        assert!(
            level_of(&cands[0], GENERAL_DOMAIN) >= 1,
            "the synthetic domain must cover every candidate so the filter cannot empty the set"
        );
    }

    #[test]
    fn non_chat_models_do_not_occupy_a_band() {
        // A realistic mixed fleet: three chat models and three embedding models
        // that are an order of magnitude cheaper. Ranking all six together would
        // put the embeddings in the cheap ranks and push every chat model into
        // band 2 or 3, collapsing the ladder.
        let mut cands = Vec::new();
        for (name, cost) in [
            ("chat-cheap", 0.000_001),
            ("chat-mid", 0.000_010),
            ("chat-dear", 0.000_100),
        ] {
            let mut m = model(name);
            m.input_cost_per_token = cost;
            m.tags = vec!["coding".to_string()];
            cands.push(healthy(m));
        }
        for (name, cost) in [
            ("embed-a", 0.000_000_001),
            ("embed-b", 0.000_000_002),
            ("embed-c", 0.000_000_003),
        ] {
            let mut m = model(name);
            m.model_type = "embedding".to_string();
            m.input_cost_per_token = cost;
            m.tags = vec!["coding".to_string()];
            cands.push(healthy(m));
        }

        derive_levels(&mut cands, crate::TierSource::Derived);

        for domain in [GENERAL_DOMAIN, "coding"] {
            assert_eq!(
                (
                    level_of(&cands[0], domain),
                    level_of(&cands[1], domain),
                    level_of(&cands[2], domain)
                ),
                (1, 2, 3),
                "chat models must be ranked 1..3 among themselves in {domain}"
            );
        }
        for c in &cands[3..] {
            assert!(
                c.levels.is_empty(),
                "{} is not a chat model and must hold no level",
                c.model.model_name
            );
        }
    }

    #[test]
    fn general_domain_sentinel_does_not_collide_with_the_real_general_tag() {
        assert_ne!(GENERAL_DOMAIN, "general");
        assert!(!crate::is_valid_tag(GENERAL_DOMAIN));
    }

    #[test]
    fn strength_is_the_max_across_desired_domains() {
        let mut m = model("specialist");
        m.tags = vec!["coding".to_string(), "math".to_string()];
        m.declared_levels = vec![("coding".to_string(), 3), ("math".to_string(), 1)];
        let mut cands = vec![healthy(m)];
        derive_levels(&mut cands, crate::TierSource::Declared);
        let domains = vec!["coding".to_string(), "math".to_string()];
        assert_eq!(cands[0].strength(&domains), 3);
    }

    fn tiered(name: &str, cost: f64, levels: &[(&str, u8)]) -> Candidate {
        let mut m = model(name);
        m.input_cost_per_token = cost;
        // Cost is estimated per request (prompt estimate x input price +
        // expected output x output price); with the fixtures' zero-token
        // prompt an input-only price would make every model free. Price the
        // output too so "cheap" stays cheap under the request-cost estimate.
        m.output_cost_per_token = cost;
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

    fn right_sizing() -> RouterWeights {
        RouterWeights {
            difficulty_enabled: true,
            right_size: true,
            ..Default::default()
        }
    }

    /// Three coding models, one a tier, where the strongest is also the
    /// cheapest: on cost alone it would win every request.
    fn ladder() -> Vec<Candidate> {
        vec![
            tiered("weak", 0.000_100, &[("coding", 1)]),
            tiered("middling", 0.000_010, &[("coding", 2)]),
            tiered("strong", 0.000_001, &[("coding", 3)]),
        ]
    }

    fn right_sized(busy: &[(&str, usize)], difficulty: u8) -> route_explain::RouteExplain {
        let busyness: HashMap<String, usize> =
            busy.iter().map(|(m, n)| (m.to_string(), *n)).collect();
        explain_selection(
            &ladder(),
            &RequestFeatures::default(),
            &busyness,
            &HashMap::new(),
            None,
            &["coding".to_string()],
            BoonGrants::default(),
            &right_sizing(),
            0.0,
            &intent_at(difficulty),
        )
    }

    fn rejected_as<'a>(explain: &'a route_explain::RouteExplain, reason: &str) -> Vec<&'a str> {
        explain
            .rejected
            .iter()
            .filter(|r| r.reason == reason)
            .flat_map(|r| r.models.iter().map(String::as_str))
            .collect()
    }

    #[test]
    fn without_right_sizing_an_easy_request_can_land_on_the_strongest_model() {
        let desired = vec!["coding".to_string()];
        let chosen = select_model(
            &ladder(),
            &RequestFeatures::default(),
            &HashMap::new(),
            &HashMap::new(),
            None,
            &desired,
            BoonGrants::default(),
            &tiering_on(),
            0.0,
            1,
        )
        .unwrap();
        assert_eq!(
            chosen.model_name, "strong",
            "the floor alone keeps every tier above it in the running"
        );
    }

    #[test]
    fn right_sizing_serves_each_difficulty_from_its_own_tier() {
        for (difficulty, model) in [(1, "weak"), (2, "middling"), (3, "strong")] {
            let explain = right_sized(&[], difficulty);
            assert_self_consistent(&explain);
            assert_eq!(
                explain.chosen.as_deref(),
                Some(model),
                "difficulty {difficulty}"
            );
        }
        let easy = right_sized(&[], 1);
        assert_eq!(
            rejected_as(&easy, "above_needed_tier"),
            vec!["middling", "strong"]
        );
        assert!(rejected_as(&easy, "tier_full").is_empty());
    }

    #[test]
    fn a_full_tier_spills_to_the_next_one_up() {
        // The soft cap is 8 requests in flight for a model with no cap of its own.
        let explain = right_sized(&[("weak", 8)], 1);
        assert_self_consistent(&explain);
        assert_eq!(explain.chosen.as_deref(), Some("middling"));
        assert_eq!(rejected_as(&explain, "tier_full"), vec!["weak"]);
        assert_eq!(rejected_as(&explain, "above_needed_tier"), vec!["strong"]);
    }

    #[test]
    fn right_sizing_never_spills_below_the_floor() {
        let explain = right_sized(&[("strong", 8)], 3);
        assert_self_consistent(&explain);
        assert_eq!(
            explain.chosen.as_deref(),
            Some("strong"),
            "a hard request waits on the strong model, it does not drop a tier"
        );
        assert_eq!(
            rejected_as(&explain, "below_tier_floor"),
            vec!["weak", "middling"]
        );
    }

    #[test]
    fn when_every_tier_is_full_all_of_them_compete() {
        let explain = right_sized(&[("weak", 8), ("middling", 8), ("strong", 8)], 1);
        assert_self_consistent(&explain);
        assert!(explain.chosen.is_some());
        assert!(rejected_as(&explain, "tier_full").is_empty());
        assert!(rejected_as(&explain, "above_needed_tier").is_empty());
    }

    #[test]
    fn right_sizing_needs_the_difficulty_floor_to_be_on() {
        let settings = crate::AutoRouterSettings {
            right_size_enabled: true,
            ..Default::default()
        };
        assert!(!RouterWeights::from_settings(&settings).right_size);
        let settings = crate::AutoRouterSettings {
            right_size_enabled: true,
            difficulty_enabled: true,
            ..Default::default()
        };
        assert!(RouterWeights::from_settings(&settings).right_size);
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
        Arc::make_mut(&mut specialist.model).tags = vec!["coding".to_string(), "math".to_string()];
        Arc::make_mut(&mut shallow.model).tags = vec!["coding".to_string(), "math".to_string()];
        let cands = vec![specialist, shallow];
        let desired = vec!["coding".to_string(), "math".to_string()];
        let chosen = select_model(
            &cands,
            &RequestFeatures::default(),
            &HashMap::new(),
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
    fn assert_self_consistent(ex: &route_explain::RouteExplain) {
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
        Arc::make_mut(&mut dear.model).route_bias = 1.4;
        let mut plain = healthy(model("plain"));
        Arc::make_mut(&mut plain.model).input_cost_per_token = 0.000_050;
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
            // The count auto-why reports is the scored rows, not the registry.
            let (_, among) = select_model_among(
                &cands,
                &RequestFeatures::default(),
                &HashMap::new(),
                &HashMap::new(),
                k.allowed.as_deref(),
                &k.tags,
                BoonGrants::default(),
                &k.weights,
                k.uniform,
                k.difficulty,
            );
            assert_eq!(among.count, ex.scored.len(), "[{label}]");
            // The runners-up are the best of the rest, the same either way.
            assert_eq!(among, ex.among(), "[{label}]");
            assert_eq!(
                among.next.len(),
                ex.scored.len().saturating_sub(1).min(RUNNERS_UP),
                "[{label}]"
            );
            for next in &among.next {
                assert_ne!(Some(&next.model), ex.chosen.as_ref(), "[{label}]");
                assert!(next.gap >= 0.0, "[{label}]");
            }
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
    fn the_runners_up_are_the_best_of_the_rest_with_their_distance() {
        let scored = [("a", 0.9), ("b", 0.85), ("c", 0.5), ("d", 0.1)];
        let among = |chosen| Among::of(scored.iter().copied(), chosen);
        let names = |a: &Among| a.next.iter().map(|n| n.model.clone()).collect::<Vec<_>>();
        // The head of the list was chosen: the next two follow it.
        let head = among(Some(0));
        assert_eq!(head.count, 4);
        assert_eq!(names(&head), ["b", "c"]);
        assert!((head.next[0].gap - 0.05).abs() < 1e-9);
        assert!((head.next[1].gap - 0.4).abs() < 1e-9);
        // The draw moved the pick down the list: the ones it passed over are
        // still the best of the rest, and the distance is never negative.
        let sampled = among(Some(2));
        assert_eq!(names(&sampled), ["a", "b"]);
        assert!((sampled.next[0].gap - 0.4).abs() < 1e-9);
        let last = among(Some(3));
        assert_eq!(names(&last), ["a", "b"]);
        // One model, or none chosen: nothing to name.
        assert!(Among::of([("a", 0.9)].into_iter(), Some(0)).next.is_empty());
        assert_eq!(
            among(None),
            Among {
                count: 4,
                next: Vec::new()
            }
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
                &HashMap::new(),
                k.allowed.as_deref(),
                &k.tags,
                BoonGrants::default(),
                &k.weights,
                k.uniform,
                k.difficulty,
            );
            assert_eq!(
                picked.map(|m| m.model_name.clone()),
                alone.map(|m| m.model_name.clone()),
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

    /// The explanation echoes the draw it was made with, so a consumer diffing
    /// two decisions can tell a weight effect from sampling noise — and can
    /// replay either one exactly.
    #[test]
    fn explain_echoes_the_draw_it_used() {
        let cands = varied_candidates();
        let w = RouterWeights {
            temperature: 1.0,
            ..Default::default()
        };
        for u in [0.0, 0.25, 0.75, 0.999] {
            let ex = explain_selection(
                &cands,
                &RequestFeatures::default(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &[],
                BoonGrants::default(),
                &w,
                u,
                &Intent::default(),
            );
            assert_eq!(ex.uniform, u, "the reported draw must be the one used");
            // Replaying the echoed draw reproduces the same pick.
            let replay = explain_selection(
                &cands,
                &RequestFeatures::default(),
                &HashMap::new(),
                &HashMap::new(),
                None,
                &[],
                BoonGrants::default(),
                &w,
                ex.uniform,
                &Intent::default(),
            );
            assert_eq!(replay.chosen, ex.chosen);
        }
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
                model: Arc::new(broken),
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

    #[test]
    fn classifier_prompts_start_from_the_newest_user_turn() {
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "Be brief."},
                {"role": "user", "content": "hello"},
                {"role": "assistant", "content": "Hi! How can I help?"},
                {"role": "user", "content": [{"type": "text", "text": "Write a Rust function"}]}
            ]
        });
        assert_eq!(
            classifier_prompts(&body, None),
            vec!["Be brief.\nWrite a Rust function", "Be brief.\nhello"]
        );
    }

    #[test]
    fn classifier_prompts_read_only_the_system_tail_the_client_names() {
        let body = serde_json::json!({"messages": [
            {"role": "system", "content": "Use the run_command tool to build code.\n\nYou help with poems."},
            {"role": "user", "content": "brainstorm names for a bakery"}
        ]});
        assert_eq!(
            classifier_prompts(&body, Some("You help with poems.".len())),
            vec!["You help with poems.\nbrainstorm names for a bakery"]
        );
        assert_eq!(
            classifier_prompts(&body, Some(0)),
            vec!["brainstorm names for a bakery"]
        );
        // A tail longer than the system text reads all of it.
        assert_eq!(
            classifier_prompts(&body, Some(10_000)),
            classifier_prompts(&body, None)
        );
        // A cut inside a multi-byte character moves forward to its end.
        let body = serde_json::json!({"messages": [
            {"role": "system", "content": "tools ééé"},
            {"role": "user", "content": "hi"}
        ]});
        assert_eq!(classifier_prompts(&body, Some(5)), vec!["éé\nhi"]);
    }

    #[test]
    fn classify_system_header_is_a_byte_count() {
        assert_eq!(classify_system_from_header(Some(" 42 ")), Some(42));
        assert_eq!(classify_system_from_header(Some("0")), Some(0));
        assert_eq!(classify_system_from_header(Some("all")), None);
        assert_eq!(classify_system_from_header(None), None);
    }

    #[test]
    fn classifier_prompts_skip_tool_results_and_turns_without_text() {
        let body = serde_json::json!({
            "messages": [
                {"role": "user", "content": "fix the failing build"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "c1", "type": "function", "function": {"name": "sh", "arguments": "{}"}}
                ]},
                {"role": "tool", "tool_call_id": "c1", "content": "error[E0308]: mismatched types"},
                {"role": "user", "content": [
                    {"type": "image_url", "image_url": {"url": "http://x/y.png"}}
                ]}
            ]
        });
        assert_eq!(
            classifier_prompts(&body, None),
            vec!["fix the failing build"]
        );
    }

    #[test]
    fn classifier_prompts_look_back_a_bounded_number_of_turns() {
        let messages: Vec<serde_json::Value> = (0..CLASSIFIER_LOOKBACK_TURNS + 2)
            .map(|i| serde_json::json!({"role": "user", "content": format!("turn {i}")}))
            .collect();
        let prompts = classifier_prompts(&serde_json::json!({ "messages": messages }), None);
        assert_eq!(prompts.len(), CLASSIFIER_LOOKBACK_TURNS);
        assert_eq!(
            prompts[0],
            format!("turn {}", CLASSIFIER_LOOKBACK_TURNS + 1)
        );
    }

    #[test]
    fn a_long_system_prompt_gives_way_to_the_request() {
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "You are a course assistant. ".repeat(200)},
                {"role": "user", "content": "Write a Rust function that parses JSON"}
            ]
        });
        let prompts = classifier_prompts(&body, None);
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].starts_with("You are a course assistant."));
        assert!(prompts[0].ends_with("\nWrite a Rust function that parses JSON"));
        assert!(prompts[0].len() <= CLASSIFIER_PROMPT_MAX_BYTES);
    }

    #[test]
    fn a_long_request_fills_the_budget_on_a_char_boundary() {
        let body = serde_json::json!({
            "messages": [
                {"role": "system", "content": "Be brief."},
                {"role": "user", "content": "é".repeat(CLASSIFIER_PROMPT_MAX_BYTES)}
            ]
        });
        let prompts = classifier_prompts(&body, None);
        assert!(prompts[0].len() <= CLASSIFIER_PROMPT_MAX_BYTES);
        assert!(
            prompts[0].starts_with('é'),
            "no room left for the system text"
        );
    }

    fn classified(tags: &[&str], difficulty: u8) -> Intent {
        Intent {
            tags: tags.iter().map(|t| t.to_string()).collect(),
            difficulty,
            source: IntentSource::Classifier,
        }
    }

    /// Run `classify_turns` over canned answers, one per turn, newest first.
    /// Returns its result and how many turns it asked about.
    fn walk(answers: &[Intent]) -> (Option<Intent>, usize) {
        let prompts: Vec<String> = (0..answers.len()).map(|i| i.to_string()).collect();
        let mut asked = 0;
        let out = {
            let fut = classify_turns(prompts, |p| {
                asked += 1;
                std::future::ready(answers[p.parse::<usize>().unwrap()].clone())
            });
            let mut fut = std::pin::pin!(fut);
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            let std::task::Poll::Ready(out) = std::future::Future::poll(fut.as_mut(), &mut cx)
            else {
                panic!("ready answers cannot leave the walk pending");
            };
            out
        };
        (out, asked)
    }

    #[test]
    fn a_topic_on_the_newest_turn_decides_alone() {
        let (out, asked) = walk(&[
            classified(&["coding", "general"], 2),
            classified(&["writing"], 1),
        ]);
        assert_eq!(out.unwrap().tags, vec!["coding", "general"]);
        assert_eq!(asked, 1);
    }

    #[test]
    fn a_follow_up_without_a_topic_keeps_the_conversation_s_topic() {
        let (out, asked) = walk(&[classified(&["general"], 1), classified(&["coding"], 2)]);
        let out = out.unwrap();
        assert_eq!(out.tags, vec!["coding"]);
        assert_eq!(out.difficulty, 2);
        assert_eq!(asked, 2);
    }

    #[test]
    fn with_no_topic_anywhere_the_newest_answer_stands() {
        let (out, asked) = walk(&[
            classified(&["general"], 1),
            classified(&["general"], 2),
            classified(&["general"], 3),
        ]);
        assert_eq!(out.unwrap().difficulty, 1);
        assert_eq!(asked, 3);
    }

    #[test]
    fn a_failed_classification_ends_the_walk() {
        // Newest turn failed: heuristics take over, older turns are not asked.
        let (out, asked) = walk(&[classified(&[], 1), classified(&["coding"], 2)]);
        assert!(out.is_none());
        assert_eq!(asked, 1);

        // An older turn failed: the newest answer stands.
        let (out, asked) = walk(&[
            classified(&["general"], 1),
            classified(&[], 1),
            classified(&["coding"], 2),
        ]);
        assert_eq!(out.unwrap().tags, vec!["general"]);
        assert_eq!(asked, 2);
    }
}
