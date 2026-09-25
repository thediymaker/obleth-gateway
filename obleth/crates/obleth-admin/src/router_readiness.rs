//! Routing readiness: turn the known ways `auto` misroutes into named,
//! plain-language findings an operator sees BEFORE a user hits one.
//!
//! Every check here is a lint for a real incident shape: a topic the
//! classifier cannot express (it picks a wrong-but-offered tag instead and
//! scoring degrades to price), a cheap-per-token model that writes tens of
//! thousands of tokens per answer, price-identical twins whose tie breaks
//! alphabetically, and tiering knobs that silently do nothing. The endpoint
//! is read-only and cheap: one candidates build plus in-memory stats.

use axum::extract::State;
use axum::Json;
use serde::Serialize;
use utoipa::ToSchema;

use obleth_config::routing::{derive_levels, Candidate};
use obleth_config::{AutoRouterSettings, TierSource, MODEL_TAGS};

use crate::{AdminState, Result};

/// One readiness finding. `severity` is `"warn"` (will visibly misroute or
/// waste) or `"info"` (a default the operator should know is in effect).
#[derive(Debug, Serialize, ToSchema)]
pub struct ReadinessFinding {
    pub severity: &'static str,
    /// Stable machine code, e.g. `uncovered_tag`; the dashboard keys help
    /// text off it.
    pub code: &'static str,
    pub title: String,
    pub detail: String,
    /// Models the finding is about, when it is about specific models.
    pub models: Vec<String>,
}

/// The whole readiness report.
#[derive(Debug, Serialize, ToSchema)]
pub struct RouterReadiness {
    pub findings: Vec<ReadinessFinding>,
    /// Size of the effective `auto` pool the checks ran over.
    pub pool_size: usize,
    pub classifier_active: bool,
    pub difficulty_enabled: bool,
}

/// Tags the heuristic fallback can emit on its own. A hole in THESE is worse
/// than a hole elsewhere: even with the classifier off, requests will be
/// tagged with them, and an uncovered tag dilutes every candidate's tag score
/// equally — the ranking silently degrades to price and capacity.
const HEURISTIC_TAGS: [&str; 4] = ["coding", "math", "vision", "long-context"];

/// A model whose observed average answer runs past this many tokens is worth
/// a warning when it sits in the auto pool: per-token prices make it look
/// cheap while per-request it is the slowest thing in the fleet.
const LONG_OUTPUT_TOKENS: f64 = 4_000.0;

#[utoipa::path(
    get, path = "/api/v1/router/readiness", tag = "settings",
    responses((status = 200, body = RouterReadiness))
)]
pub(crate) async fn get_router_readiness(
    State(state): State<AdminState>,
) -> Result<Json<RouterReadiness>> {
    let settings = state
        .store
        .get_auto_router_settings()
        .await?
        .unwrap_or_default();
    let mut candidates = state.store.build_candidates(settings.tier_source).await?;
    derive_levels(&mut candidates, settings.tier_source);
    let stats = state.output_stats.snapshot();
    Ok(Json(build_readiness(&settings, &candidates, &stats)))
}

/// Pure so it can be unit-tested against synthetic fleets. The only inputs
/// are the saved settings, the candidate set the router itself scores over,
/// and the observed completion-length averages.
pub fn build_readiness(
    settings: &AutoRouterSettings,
    candidates: &[Candidate],
    output_stats: &std::collections::HashMap<String, f64>,
) -> RouterReadiness {
    let mut findings: Vec<ReadinessFinding> = Vec::new();

    // The pool the router actually chooses from (health is a live signal the
    // hard filters handle; a temporarily-down model is not a config finding).
    let pool: Vec<&Candidate> = candidates
        .iter()
        .filter(|c| {
            c.model.enabled
                && c.model.auto_eligible
                && c.model.model_type == obleth_config::DEFAULT_MODEL_TYPE
        })
        .collect();

    let classifier_active = settings.classifier_active();

    // -- classifier state ---------------------------------------------------
    if !classifier_active {
        findings.push(ReadinessFinding {
            severity: "info",
            code: "classifier_off",
            title: "Auto routing is using keyword heuristics only".to_string(),
            detail: "No classifier model is configured, so request intent comes from a \
                     fixed keyword scan. Enabling a small, fast registered model as the \
                     classifier makes tagging much more reliable."
                .to_string(),
            models: Vec::new(),
        });
    } else if let Some(name) = settings.classifier_model.as_deref() {
        let registered = candidates
            .iter()
            .find(|c| c.model.model_name == name && c.model.enabled);
        if name == "auto" {
            findings.push(ReadinessFinding {
                severity: "warn",
                code: "classifier_is_auto",
                title: "The classifier model is set to \"auto\"".to_string(),
                detail: "\"auto\" cannot classify itself; the gateway falls back to \
                         keyword heuristics on every request. Point the classifier at a \
                         concrete registered model."
                    .to_string(),
                models: Vec::new(),
            });
        } else if registered.is_none() {
            findings.push(ReadinessFinding {
                severity: "warn",
                code: "classifier_missing",
                title: format!("Classifier model `{name}` is not registered (or disabled)"),
                detail: "Every auto request silently falls back to keyword heuristics \
                         until the classifier points at an enabled registered model."
                    .to_string(),
                models: vec![name.to_string()],
            });
        }
    }

    // -- uncovered tags -----------------------------------------------------
    // The classifier is only offered tags that pool models actually carry, so
    // an uncovered tag cannot be expressed at all: the classifier substitutes
    // the nearest tag it WAS offered, and scoring quietly degrades to price.
    for tag in MODEL_TAGS {
        let covered = pool.iter().any(|c| c.model.tags.iter().any(|t| t == tag));
        if covered {
            continue;
        }
        let heuristic = HEURISTIC_TAGS.contains(tag);
        findings.push(ReadinessFinding {
            severity: if heuristic { "warn" } else { "info" },
            code: "uncovered_tag",
            title: format!("No auto-eligible model carries the `{tag}` tag"),
            detail: format!(
                "Requests about {tag} cannot be matched by topic{}; they will route on \
                 price and spare capacity alone, which is how the cheapest wrong model \
                 wins. Tag the models that should catch {tag} traffic.",
                if heuristic {
                    " (and the heuristic fallback DOES emit this tag)"
                } else {
                    ""
                }
            ),
            models: Vec::new(),
        });
    }

    // -- long-output models in the pool --------------------------------------
    for c in &pool {
        if let Some(avg) = output_stats.get(&c.model.model_name) {
            if *avg >= LONG_OUTPUT_TOKENS {
                findings.push(ReadinessFinding {
                    severity: "warn",
                    code: "long_output_model",
                    title: format!(
                        "`{}` averages {:.0} output tokens per answer",
                        c.model.model_name, avg
                    ),
                    detail: "Its per-token price makes it look cheap while per request \
                             it is among the slowest and most expensive choices. The \
                             router's request-cost estimate accounts for this once it \
                             has observations, but consider whether an unbounded \
                             thinker belongs in the auto pool at all (auto-exclude \
                             keeps it callable by name)."
                        .to_string(),
                    models: vec![c.model.model_name.clone()],
                });
            }
        }
    }

    // -- price-identical twins ------------------------------------------------
    // Same unit prices, same tag set, same bias: their scores tie exactly on
    // every request and the name tie-break decides — fine when intended,
    // surprising when one twin is the better model.
    let mut seen: Vec<usize> = Vec::new();
    for (i, a) in pool.iter().enumerate() {
        if seen.contains(&i) {
            continue;
        }
        let mut group = vec![a.model.model_name.clone()];
        for (j, b) in pool.iter().enumerate().skip(i + 1) {
            let mut a_tags = a.model.tags.clone();
            let mut b_tags = b.model.tags.clone();
            a_tags.sort();
            b_tags.sort();
            if a.model.input_cost_per_token == b.model.input_cost_per_token
                && a.model.output_cost_per_token == b.model.output_cost_per_token
                && a.model.route_bias == b.model.route_bias
                && a_tags == b_tags
            {
                group.push(b.model.model_name.clone());
                seen.push(j);
            }
        }
        if group.len() > 1 {
            group.sort();
            findings.push(ReadinessFinding {
                severity: "info",
                code: "price_tie",
                title: format!("{} score identically on every request", group.join(", ")),
                detail: "Identical prices, tags and bias tie exactly, so the \
                         alphabetical name tie-break decides which one serves. If one \
                         of them is the better model, give it a slightly higher route \
                         bias (1.05 is enough)."
                    .to_string(),
                models: group,
            });
        }
    }

    // -- tiering ---------------------------------------------------------------
    if !settings.difficulty_enabled {
        findings.push(ReadinessFinding {
            severity: "info",
            code: "tiering_off",
            title: "Difficulty tiering is off".to_string(),
            detail: "Request difficulty is classified but unused: a hard request can \
                     land on the weakest model that matches its topic. Enabling \
                     difficulty tiering keeps weak models out of hard requests."
                .to_string(),
            models: Vec::new(),
        });
    } else if settings.tier_source == TierSource::Declared {
        let any_declared = pool.iter().any(|c| !c.model.declared_levels.is_empty());
        if !any_declared {
            findings.push(ReadinessFinding {
                severity: "warn",
                code: "tiering_inert",
                title: "Difficulty tiering is on but no model declares a level".to_string(),
                detail: "With tier source `declared` and no `tag:level` suffixes, every \
                         model reads as level 1 and the difficulty floor never filters \
                         anything. Declare levels, or switch tier source to `hybrid` to \
                         derive them from cost."
                    .to_string(),
                models: Vec::new(),
            });
        }
    }

    RouterReadiness {
        findings,
        pool_size: pool.len(),
        classifier_active,
        difficulty_enabled: settings.difficulty_enabled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obleth_config::ResolvedModel;

    fn model(name: &str) -> ResolvedModel {
        ResolvedModel {
            model_name: name.to_string(),
            aliases: Vec::new(),
            quantization: "unknown".into(),
            upstream_model: name.to_string(),
            api_base: "http://upstream".to_string(),
            api_key: None,
            upstream_headers: Default::default(),
            model_type: obleth_config::DEFAULT_MODEL_TYPE.to_string(),
            admission_weight: 100,
            max_in_flight: None,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 0,
            input_cost_per_token: 0.000_001,
            output_cost_per_token: 0.000_002,
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
            knowledge_collections: Vec::new(),
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: obleth_config::DEFAULT_RETRY_BACKOFF_MS,
            endpoint_selection_mode: obleth_config::DEFAULT_ENDPOINT_SELECTION_MODE.to_string(),
            debug_diagnostics: false,
            energy_slots_per_node: 0,
            route_bias: 1.0,
            auto_eligible: true,
            draft_model: String::new(),
            verify_api_base: String::new(),
            verify_upstream_model: String::new(),
            endpoints: Vec::new(),
        }
    }

    fn cand(m: ResolvedModel) -> Candidate {
        Candidate {
            model: m,
            healthy: true,
            levels: Vec::new(),
        }
    }

    fn codes(r: &RouterReadiness) -> Vec<&'static str> {
        r.findings.iter().map(|f| f.code).collect()
    }

    #[test]
    fn uncovered_heuristic_tag_is_a_warning() {
        let mut coder = model("coder");
        coder.tags = vec!["coding".to_string()];
        let cands = vec![cand(coder)];
        let r = build_readiness(&AutoRouterSettings::default(), &cands, &Default::default());
        let math = r
            .findings
            .iter()
            .find(|f| f.code == "uncovered_tag" && f.title.contains("`math`"))
            .expect("math must be reported uncovered");
        assert_eq!(math.severity, "warn");
        // creative is uncovered too, but the heuristic never emits it.
        let creative = r
            .findings
            .iter()
            .find(|f| f.code == "uncovered_tag" && f.title.contains("`creative`"))
            .unwrap();
        assert_eq!(creative.severity, "info");
    }

    #[test]
    fn long_output_pool_model_is_flagged() {
        let mut thinker = model("thinker");
        thinker.tags = vec!["reasoning".to_string()];
        let cands = vec![cand(thinker)];
        let mut stats = std::collections::HashMap::new();
        stats.insert("thinker".to_string(), 25_519.0);
        let r = build_readiness(&AutoRouterSettings::default(), &cands, &stats);
        assert!(codes(&r).contains(&"long_output_model"));
        // Auto-excluded models are not in the pool and must not be flagged.
        let mut excluded = model("thinker");
        excluded.auto_eligible = false;
        let r = build_readiness(&AutoRouterSettings::default(), &[cand(excluded)], &stats);
        assert!(!codes(&r).contains(&"long_output_model"));
    }

    #[test]
    fn price_identical_twins_are_reported_once() {
        let mut a = model("glm-5-2");
        let mut b = model("glm-5-3");
        for m in [&mut a, &mut b] {
            m.tags = vec!["coding".to_string()];
        }
        let r = build_readiness(
            &AutoRouterSettings::default(),
            &[cand(a), cand(b)],
            &Default::default(),
        );
        let ties: Vec<_> = r
            .findings
            .iter()
            .filter(|f| f.code == "price_tie")
            .collect();
        assert_eq!(ties.len(), 1);
        assert_eq!(
            ties[0].models,
            vec!["glm-5-2".to_string(), "glm-5-3".to_string()]
        );
        // A bias nudge dissolves the tie.
        let mut a = model("glm-5-2");
        let mut b = model("glm-5-3");
        for m in [&mut a, &mut b] {
            m.tags = vec!["coding".to_string()];
        }
        b.route_bias = 1.05;
        let r = build_readiness(
            &AutoRouterSettings::default(),
            &[cand(a), cand(b)],
            &Default::default(),
        );
        assert!(!codes(&r).contains(&"price_tie"));
    }

    #[test]
    fn tiering_findings_track_the_knobs() {
        let cands = vec![cand(model("m"))];
        let off = AutoRouterSettings::default();
        let r = build_readiness(&off, &cands, &Default::default());
        assert!(codes(&r).contains(&"tiering_off"));

        let declared_no_levels = AutoRouterSettings {
            difficulty_enabled: true,
            tier_source: TierSource::Declared,
            ..Default::default()
        };
        let r = build_readiness(&declared_no_levels, &cands, &Default::default());
        assert!(codes(&r).contains(&"tiering_inert"));

        // Hybrid derives levels from cost, so no inert warning.
        let hybrid = AutoRouterSettings {
            difficulty_enabled: true,
            tier_source: TierSource::Hybrid,
            ..Default::default()
        };
        let r = build_readiness(&hybrid, &cands, &Default::default());
        assert!(!codes(&r).contains(&"tiering_inert"));
    }

    #[test]
    fn classifier_state_findings() {
        let cands = vec![cand(model("m"))];
        let off = AutoRouterSettings::default();
        let r = build_readiness(&off, &cands, &Default::default());
        assert!(codes(&r).contains(&"classifier_off"));

        let missing = AutoRouterSettings {
            classifier_enabled: true,
            classifier_model: Some("ghost".to_string()),
            ..Default::default()
        };
        let r = build_readiness(&missing, &cands, &Default::default());
        assert!(codes(&r).contains(&"classifier_missing"));

        let is_auto = AutoRouterSettings {
            classifier_enabled: true,
            classifier_model: Some("auto".to_string()),
            ..Default::default()
        };
        let r = build_readiness(&is_auto, &cands, &Default::default());
        assert!(codes(&r).contains(&"classifier_is_auto"));
    }
}
