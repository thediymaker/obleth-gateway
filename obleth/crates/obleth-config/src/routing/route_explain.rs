//! Serializable account of one `auto` routing decision.
//!
//! The same shape is returned by the admin simulate endpoint and written into
//! the `auto_route` span, so the dashboard renders a hypothetical and a real
//! decision with one component, and a real decision can be replayed through the
//! simulator unchanged.

use serde::Serialize;
use utoipa::ToSchema;

/// One candidate that survived every hard filter, with the score components
/// that produced its rank. Emitted in the router's own descending-score order.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct ScoredCandidate {
    pub model: String,
    /// Strength on the request's tier domains (see [`RouteExplain::tier_domains`]).
    pub level: u8,
    pub spare: f64,
    pub cost_score: f64,
    /// Fraction of the request's intent tags this model carries. Always `0.0`
    /// when the request had no tags, matching the router's neutral path where
    /// the tag term is skipped entirely rather than scored as a miss.
    pub tag_score: f64,
    pub bias: f64,
    pub score: f64,
    pub chosen: bool,
}

/// Candidates dropped by one hard filter, collapsed under that filter's name.
/// Collapsing keeps the payload small enough to ride the span without a
/// per-model row for the boring majority.
#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct Rejection {
    pub reason: &'static str,
    pub models: Vec<String>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct RouteExplain {
    pub chosen: Option<String>,
    pub difficulty: u8,
    pub difficulty_source: super::IntentSource,
    pub tags: Vec<String>,
    pub tag_source: super::IntentSource,
    /// Milliseconds spent in the intent classifier. Only the proxy call site
    /// knows the real timing, so `explain_selection` always emits `0` and the
    /// data plane overwrites it before the span is recorded. A `0` here from
    /// the simulate endpoint is expected, not a bug.
    pub classifier_ms: u32,
    /// Domains the tier filter reasoned over: the request's intent tags, or the
    /// single synthetic `*` domain when it had none. Empty only when tiering was
    /// off and nothing needed it.
    pub tier_domains: Vec<String>,
    /// Minimum strength a candidate needed on `tier_domains` to survive stage 2.
    /// Always `0` when `weights.difficulty_enabled` is false — that is "tiering
    /// was off", not "every model qualified at level 0".
    pub tier_floor: u8,
    /// True when the request asked for a level no surviving candidate holds, so
    /// the floor clamped down to the best available. Always `false` when
    /// `weights.difficulty_enabled` is false.
    pub tier_floor_clamped: bool,
    pub weights: WeightsView,
    pub temperature: f64,
    /// True when the softmax draw landed on a row other than the first.
    ///
    /// Rows are ordered by descending score, ties broken by name, and this
    /// field is index-based: when the leading scores tie, a draw that moves the
    /// pick off the head of the list reports `true` even though the winner is
    /// also an argmax. Read it as "sampling changed which row was taken", not
    /// as "a lower-scoring model won".
    pub sampled: bool,
    pub scored: Vec<ScoredCandidate>,
    pub rejected: Vec<Rejection>,
}

#[derive(Debug, Clone, Serialize, ToSchema)]
pub struct WeightsView {
    pub capacity: f64,
    pub cost: f64,
    pub tag: f64,
    pub soft_cap: f64,
    pub difficulty_enabled: bool,
}
