//! Reasoning effort: one vocabulary for callers, translated per model.
//!
//! Callers ask for `none`, `low`, `medium` or `high`, whichever API they use
//! (`reasoning_effort` on chat, `reasoning.effort` on Responses, `thinking` or
//! `output_config.effort` on Messages). Model servers disagree about what
//! those words mean — one has only `low` and `high`, another turns thinking
//! off with a chat-template flag, a third rejects `high` outright — so each
//! model carries a map ([`ModelReasoning`]) from the four levels to the
//! fields its server understands.
//!
//! A model with no map passes requests through untouched, so nothing changes
//! for a deployment that never sets one.
//!
//! A [`ModelFamily`] goes one step further: one name for several models, with
//! the level choosing which one answers (an instruct model for `none`, its
//! thinking sibling for the rest).

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use utoipa::ToSchema;
use uuid::Uuid;

/// An effort level, as callers name it.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ToSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningLevel {
    None,
    Low,
    Medium,
    High,
}

impl ReasoningLevel {
    pub const ALL: [ReasoningLevel; 4] = [
        ReasoningLevel::None,
        ReasoningLevel::Low,
        ReasoningLevel::Medium,
        ReasoningLevel::High,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ReasoningLevel::None => "none",
            ReasoningLevel::Low => "low",
            ReasoningLevel::Medium => "medium",
            ReasoningLevel::High => "high",
        }
    }

    /// Read a caller's effort. Besides the four levels this takes the
    /// spellings other APIs use for the same idea, folded to the nearest
    /// level: `minimal` is low, `xhigh` and `max` are high, `off` and
    /// `disabled` are none. Case and surrounding space are ignored.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "none" | "off" | "disabled" => Some(ReasoningLevel::None),
            "minimal" | "low" => Some(ReasoningLevel::Low),
            "medium" => Some(ReasoningLevel::Medium),
            "high" | "xhigh" | "max" => Some(ReasoningLevel::High),
            _ => None,
        }
    }
}

impl std::fmt::Display for ReasoningLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How a model takes reasoning effort.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningMode {
    /// Requests reach the model as the caller sent them.
    #[default]
    Passthrough,
    /// The four levels are translated with the model's `levels`.
    Mapped,
    /// The model has no effort control: the caller's effort is removed, so a
    /// server that would reject it never sees it.
    Unsupported,
}

/// What one level becomes on the request to the model's server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningAction {
    /// Merge these fields into the request body. Objects merge key by key
    /// (so `chat_template_kwargs` keeps the caller's other keys) and the
    /// operator's value wins. `{}` sends nothing: the server's own default.
    #[schema(value_type = Object)]
    Send(Map<String, Value>),
    /// Serve the request as this other level, for a model that has nothing
    /// for this one (e.g. no way to turn thinking off). Must name a level
    /// that sends.
    Use(ReasoningLevel),
}

/// A model's reasoning-effort map. Stored as one JSON object on the model,
/// so the default (`{}`) is pass-through.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ModelReasoning {
    #[serde(default)]
    pub mode: ReasoningMode,
    /// The level served when the caller names none (`mapped` only). Unset
    /// sends nothing, which leaves the server's own default in charge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_level: Option<ReasoningLevel>,
    /// One action per level. `mapped` needs all four; the other modes keep
    /// whatever is here so switching back restores it.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[schema(value_type = Object)]
    pub levels: BTreeMap<ReasoningLevel, ReasoningAction>,
}

/// Request fields a level may not set: they belong to the caller or to the
/// gateway, and a map that set them would change far more than effort.
pub const RESERVED_REASONING_FIELDS: &[&str] = &[
    "model",
    "messages",
    "prompt",
    "input",
    "stream",
    "stream_options",
    "tools",
    "tool_choice",
    "n",
];

/// Largest serialized size of one level's fields.
pub const MAX_REASONING_SEND_BYTES: usize = 4096;

/// Request fields that carry a caller's effort on a chat body. Removed
/// before a mapped level's fields go in, and for a model without effort
/// control.
pub const EFFORT_FIELDS: &[&str] = &["reasoning_effort", "reasoning"];

/// What to do to one request's body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReasoningPlan {
    /// Leave the request as it is.
    Untouched,
    /// The model has no effort control: remove the caller's effort fields.
    Strip { requested: Option<ReasoningLevel> },
    /// Remove the caller's effort fields and merge `send`.
    Apply {
        requested: Option<ReasoningLevel>,
        served: ReasoningLevel,
        send: Map<String, Value>,
    },
}

impl ReasoningPlan {
    /// Carry out the plan on a chat request body.
    pub fn apply_to(&self, body: &mut Value) {
        let Some(obj) = body.as_object_mut() else {
            return;
        };
        match self {
            ReasoningPlan::Untouched => {}
            ReasoningPlan::Strip { .. } => strip_effort(obj),
            ReasoningPlan::Apply { send, .. } => {
                strip_effort(obj);
                merge_into(obj, send);
            }
        }
    }

    /// The level the caller asked for, when the plan touched the request.
    pub fn requested(&self) -> Option<ReasoningLevel> {
        match self {
            ReasoningPlan::Untouched => None,
            ReasoningPlan::Strip { requested } | ReasoningPlan::Apply { requested, .. } => {
                *requested
            }
        }
    }
}

fn strip_effort(obj: &mut Map<String, Value>) {
    for field in EFFORT_FIELDS {
        obj.remove(*field);
    }
}

/// Merge `patch` into `target`: objects key by key, anything else replaced.
pub fn merge_into(target: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (k, v) in patch {
        match (target.get_mut(k), v) {
            (Some(Value::Object(existing)), Value::Object(inner)) => merge_into(existing, inner),
            _ => {
                target.insert(k.clone(), v.clone());
            }
        }
    }
}

impl ModelReasoning {
    /// Whether this map changes anything about a request.
    pub fn is_passthrough(&self) -> bool {
        self.mode == ReasoningMode::Passthrough
    }

    /// The plan for a request that asked for `requested` (`None`: the caller
    /// named no level).
    pub fn plan(&self, requested: Option<ReasoningLevel>) -> ReasoningPlan {
        match self.mode {
            ReasoningMode::Passthrough => ReasoningPlan::Untouched,
            ReasoningMode::Unsupported => ReasoningPlan::Strip { requested },
            ReasoningMode::Mapped => {
                let Some(level) = requested.or(self.default_level) else {
                    return ReasoningPlan::Untouched;
                };
                match self.resolve(level) {
                    Some((served, send)) => ReasoningPlan::Apply {
                        requested,
                        served,
                        send: send.clone(),
                    },
                    // A stored map the write path would have refused; serve
                    // as if the model had no control rather than guess.
                    None => ReasoningPlan::Strip { requested },
                }
            }
        }
    }

    /// The level a request at `level` is served at, and the fields it sends.
    fn resolve(&self, level: ReasoningLevel) -> Option<(ReasoningLevel, &Map<String, Value>)> {
        match self.levels.get(&level)? {
            ReasoningAction::Send(send) => Some((level, send)),
            ReasoningAction::Use(other) => match self.levels.get(other)? {
                ReasoningAction::Send(send) => Some((*other, send)),
                ReasoningAction::Use(_) => None,
            },
        }
    }

    /// For callers: the level each of the four is served at (`mapped`), so a
    /// client can see that `none` on a model without an off switch means
    /// `low`. `None` for the other modes.
    pub fn served_levels(&self) -> Option<BTreeMap<ReasoningLevel, ReasoningLevel>> {
        if self.mode != ReasoningMode::Mapped {
            return None;
        }
        Some(
            ReasoningLevel::ALL
                .iter()
                .filter_map(|l| self.resolve(*l).map(|(served, _)| (*l, served)))
                .collect(),
        )
    }

    /// Check a map an operator wrote. A `mapped` map needs an action for
    /// every level, a `use` must point at a level that sends, and no level
    /// may set a field the caller or the gateway owns.
    pub fn validate(&self) -> Result<(), String> {
        for (level, action) in &self.levels {
            match action {
                ReasoningAction::Send(send) => {
                    if let Some(field) = RESERVED_REASONING_FIELDS
                        .iter()
                        .find(|f| send.contains_key(**f))
                    {
                        return Err(format!(
                            "level `{level}` may not set `{field}`: only effort-related fields can be sent"
                        ));
                    }
                    let size = serde_json::to_vec(send).map(|v| v.len()).unwrap_or(0);
                    if size > MAX_REASONING_SEND_BYTES {
                        return Err(format!(
                            "level `{level}` sends {size} bytes; the most is {MAX_REASONING_SEND_BYTES}"
                        ));
                    }
                }
                ReasoningAction::Use(other) => {
                    if other == level {
                        return Err(format!("level `{level}` cannot use itself"));
                    }
                    if self.mode == ReasoningMode::Mapped {
                        match self.levels.get(other) {
                            Some(ReasoningAction::Send(_)) => {}
                            Some(ReasoningAction::Use(_)) => {
                                return Err(format!(
                                    "level `{level}` uses `{other}`, which itself uses another level; point it at a level that sends"
                                ))
                            }
                            None => {
                                return Err(format!(
                                    "level `{level}` uses `{other}`, which has nothing set"
                                ))
                            }
                        }
                    }
                }
            }
        }
        if self.mode == ReasoningMode::Mapped {
            let missing: Vec<&str> = ReasoningLevel::ALL
                .iter()
                .filter(|l| !self.levels.contains_key(l))
                .map(|l| l.as_str())
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "a mapped model needs every level set; missing: {}",
                    missing.join(", ")
                ));
            }
        }
        Ok(())
    }
}

/// The effort a chat request asks for: `reasoning_effort`, or the
/// `reasoning: {"effort": ...}` object some clients send instead.
///
/// `Ok(None)` when it names none; `Err` with the raw value when it names one
/// obleth does not know.
pub fn chat_effort(body: &Value) -> Result<Option<ReasoningLevel>, String> {
    let raw = body
        .get("reasoning_effort")
        .or_else(|| body.pointer("/reasoning/effort"));
    parse_raw(raw)
}

/// The effort a Responses request asks for (`reasoning.effort`).
pub fn responses_effort(body: &Value) -> Result<Option<ReasoningLevel>, String> {
    parse_raw(body.pointer("/reasoning/effort"))
}

/// Thinking budgets below this read as `low` on the Messages API.
pub const MESSAGES_LOW_BUDGET: u64 = 4096;
/// Thinking budgets below this (and at least [`MESSAGES_LOW_BUDGET`]) read as
/// `medium`; anything larger is `high`.
pub const MESSAGES_MEDIUM_BUDGET: u64 = 16384;

/// The effort a Messages request asks for. `output_config.effort` wins; else
/// `thinking`: `disabled` is none, an `enabled` budget is bucketed by size,
/// and `adaptive` (or no thinking field) names no level.
pub fn messages_effort(body: &Value) -> Result<Option<ReasoningLevel>, String> {
    if let Some(level) = parse_raw(body.pointer("/output_config/effort"))? {
        return Ok(Some(level));
    }
    let Some(thinking) = body.get("thinking") else {
        return Ok(None);
    };
    match thinking.get("type").and_then(Value::as_str) {
        Some("disabled") => Ok(Some(ReasoningLevel::None)),
        Some("enabled") => {
            let budget = thinking
                .get("budget_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(MESSAGES_MEDIUM_BUDGET);
            Ok(Some(if budget < MESSAGES_LOW_BUDGET {
                ReasoningLevel::Low
            } else if budget < MESSAGES_MEDIUM_BUDGET {
                ReasoningLevel::Medium
            } else {
                ReasoningLevel::High
            }))
        }
        _ => Ok(None),
    }
}

fn parse_raw(raw: Option<&Value>) -> Result<Option<ReasoningLevel>, String> {
    match raw {
        None | Some(Value::Null) => Ok(None),
        // What was asked for goes into the request log as written, so only its
        // head is kept: the value is a client's and can be megabytes.
        Some(Value::String(s)) => ReasoningLevel::parse(s)
            .map(Some)
            .ok_or_else(|| s.chars().take(64).collect()),
        Some(other) => Err(other.to_string().chars().take(64).collect()),
    }
}

/// Longest family name and description.
pub const MAX_FAMILY_NAME_CHARS: usize = 128;
pub const MAX_FAMILY_DESCRIPTION_CHARS: usize = 500;

/// Which model answers a family name at each level, by `model_name`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FamilyMembers {
    pub none: String,
    pub low: String,
    pub medium: String,
    pub high: String,
}

impl FamilyMembers {
    pub fn get(&self, level: ReasoningLevel) -> &str {
        match level {
            ReasoningLevel::None => &self.none,
            ReasoningLevel::Low => &self.low,
            ReasoningLevel::Medium => &self.medium,
            ReasoningLevel::High => &self.high,
        }
    }

    pub fn get_mut(&mut self, level: ReasoningLevel) -> &mut String {
        match level {
            ReasoningLevel::None => &mut self.none,
            ReasoningLevel::Low => &mut self.low,
            ReasoningLevel::Medium => &mut self.medium,
            ReasoningLevel::High => &mut self.high,
        }
    }

    /// Each distinct member once, in level order.
    pub fn distinct(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for level in ReasoningLevel::ALL {
            let name = self.get(level);
            if !out.contains(&name) {
                out.push(name);
            }
        }
        out
    }
}

/// One name for several models, with the effort level choosing which one
/// answers: e.g. `olmo3-7b` served by an instruct model at `none` and by its
/// thinking sibling at the other levels. A caller who names no level gets
/// `default_level`.
///
/// The member serves the request as if the caller had named it — its
/// admission pool, prices, allowlist entry and lifecycle — and the level is
/// passed on, so a member with its own effort map applies it. The usage
/// ledger records the family name as the name the caller used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ModelFamily {
    pub id: Uuid,
    /// The name callers pass in `model`. Shares one namespace with model
    /// names, aliases and variants.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The level a request that names none is served at.
    pub default_level: ReasoningLevel,
    pub members: FamilyMembers,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl ModelFamily {
    /// The member that answers a request at `requested` (or the default).
    pub fn member_for(&self, requested: Option<ReasoningLevel>) -> (ReasoningLevel, &str) {
        let level = requested.unwrap_or(self.default_level);
        (level, self.members.get(level))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn send(v: Value) -> ReasoningAction {
        ReasoningAction::Send(v.as_object().unwrap().clone())
    }

    fn glm_like() -> ModelReasoning {
        ModelReasoning {
            mode: ReasoningMode::Mapped,
            default_level: None,
            levels: BTreeMap::from([
                (
                    ReasoningLevel::None,
                    ReasoningAction::Use(ReasoningLevel::Low),
                ),
                (
                    ReasoningLevel::Low,
                    send(json!({"reasoning_effort": "low"})),
                ),
                (ReasoningLevel::Medium, send(json!({}))),
                (
                    ReasoningLevel::High,
                    send(json!({"reasoning_effort": "max"})),
                ),
            ]),
        }
    }

    #[test]
    fn levels_parse_with_the_other_apis_spellings() {
        assert_eq!(ReasoningLevel::parse(" High "), Some(ReasoningLevel::High));
        assert_eq!(ReasoningLevel::parse("minimal"), Some(ReasoningLevel::Low));
        assert_eq!(ReasoningLevel::parse("xhigh"), Some(ReasoningLevel::High));
        assert_eq!(ReasoningLevel::parse("max"), Some(ReasoningLevel::High));
        assert_eq!(ReasoningLevel::parse("off"), Some(ReasoningLevel::None));
        assert_eq!(ReasoningLevel::parse("turbo"), None);
    }

    #[test]
    fn the_default_map_is_passthrough_and_stores_as_an_empty_object() {
        let r = ModelReasoning::default();
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            json!({"mode": "passthrough"})
        );
        let read: ModelReasoning = serde_json::from_value(json!({})).unwrap();
        assert_eq!(read, r);
        assert_eq!(r.plan(Some(ReasoningLevel::High)), ReasoningPlan::Untouched);
    }

    #[test]
    fn actions_serialize_as_send_or_use() {
        let r = glm_like();
        let v = serde_json::to_value(&r).unwrap();
        assert_eq!(v["levels"]["none"], json!({"use": "low"}));
        assert_eq!(
            v["levels"]["high"],
            json!({"send": {"reasoning_effort": "max"}})
        );
        let back: ModelReasoning = serde_json::from_value(v).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn a_used_level_serves_as_its_target_and_reports_both() {
        let plan = glm_like().plan(Some(ReasoningLevel::None));
        let ReasoningPlan::Apply {
            requested,
            served,
            send,
        } = plan
        else {
            panic!("expected Apply");
        };
        assert_eq!(requested, Some(ReasoningLevel::None));
        assert_eq!(served, ReasoningLevel::Low);
        assert_eq!(Value::Object(send), json!({"reasoning_effort": "low"}));
    }

    #[test]
    fn applying_replaces_the_callers_effort_and_merges_template_kwargs() {
        let mut r = glm_like();
        r.levels.insert(
            ReasoningLevel::None,
            send(json!({"chat_template_kwargs": {"enable_thinking": false}})),
        );
        let mut body = json!({
            "model": "m",
            "reasoning_effort": "none",
            "chat_template_kwargs": {"tools_style": "x", "enable_thinking": true},
        });
        r.plan(Some(ReasoningLevel::None)).apply_to(&mut body);
        assert_eq!(
            body,
            json!({
                "model": "m",
                "chat_template_kwargs": {"tools_style": "x", "enable_thinking": false},
            })
        );
    }

    #[test]
    fn a_mapped_model_with_no_default_leaves_a_silent_request_alone() {
        let mut body = json!({"model": "m"});
        let plan = glm_like().plan(None);
        assert_eq!(plan, ReasoningPlan::Untouched);
        plan.apply_to(&mut body);
        assert_eq!(body, json!({"model": "m"}));
    }

    #[test]
    fn a_default_level_applies_when_the_caller_names_none() {
        let mut r = glm_like();
        r.default_level = Some(ReasoningLevel::High);
        let mut body = json!({"model": "m"});
        let plan = r.plan(None);
        assert_eq!(plan.requested(), None);
        plan.apply_to(&mut body);
        assert_eq!(body["reasoning_effort"], "max");
    }

    #[test]
    fn unsupported_strips_every_effort_field() {
        let r = ModelReasoning {
            mode: ReasoningMode::Unsupported,
            ..Default::default()
        };
        let mut body =
            json!({"model": "m", "reasoning_effort": "low", "reasoning": {"effort": "low"}});
        r.plan(Some(ReasoningLevel::Low)).apply_to(&mut body);
        assert_eq!(body, json!({"model": "m"}));
    }

    #[test]
    fn served_levels_show_fallbacks() {
        let served = glm_like().served_levels().unwrap();
        assert_eq!(served[&ReasoningLevel::None], ReasoningLevel::Low);
        assert_eq!(served[&ReasoningLevel::High], ReasoningLevel::High);
        assert!(ModelReasoning::default().served_levels().is_none());
    }

    #[test]
    fn validation_catches_holes_chains_and_reserved_fields() {
        assert!(glm_like().validate().is_ok());

        let mut missing = glm_like();
        missing.levels.remove(&ReasoningLevel::Medium);
        assert!(missing.validate().unwrap_err().contains("medium"));

        let mut chain = glm_like();
        chain.levels.insert(
            ReasoningLevel::Medium,
            ReasoningAction::Use(ReasoningLevel::None),
        );
        assert!(chain.validate().unwrap_err().contains("itself uses"));

        let mut own = glm_like();
        own.levels.insert(
            ReasoningLevel::Low,
            ReasoningAction::Use(ReasoningLevel::Low),
        );
        assert!(own.validate().is_err());

        let mut reserved = glm_like();
        reserved
            .levels
            .insert(ReasoningLevel::High, send(json!({"model": "other"})));
        assert!(reserved.validate().unwrap_err().contains("`model`"));

        // Other modes keep an incomplete table without complaint.
        let mut kept = missing.clone();
        kept.mode = ReasoningMode::Passthrough;
        assert!(kept.validate().is_ok());
    }

    #[test]
    fn effort_is_read_from_every_api_shape() {
        assert_eq!(
            chat_effort(&json!({"reasoning_effort": "low"})),
            Ok(Some(ReasoningLevel::Low))
        );
        assert_eq!(
            chat_effort(&json!({"reasoning": {"effort": "max"}})),
            Ok(Some(ReasoningLevel::High))
        );
        assert_eq!(chat_effort(&json!({})), Ok(None));
        assert_eq!(
            chat_effort(&json!({"reasoning_effort": "turbo"})),
            Err("turbo".to_string())
        );
        assert_eq!(
            responses_effort(&json!({"reasoning": {"effort": "medium"}})),
            Ok(Some(ReasoningLevel::Medium))
        );
        assert_eq!(
            messages_effort(&json!({"thinking": {"type": "disabled"}})),
            Ok(Some(ReasoningLevel::None))
        );
        assert_eq!(
            messages_effort(&json!({"thinking": {"type": "enabled", "budget_tokens": 1024}})),
            Ok(Some(ReasoningLevel::Low))
        );
        assert_eq!(
            messages_effort(&json!({"thinking": {"type": "enabled", "budget_tokens": 8000}})),
            Ok(Some(ReasoningLevel::Medium))
        );
        assert_eq!(
            messages_effort(&json!({"thinking": {"type": "enabled", "budget_tokens": 32000}})),
            Ok(Some(ReasoningLevel::High))
        );
        assert_eq!(
            messages_effort(&json!({
                "thinking": {"type": "enabled", "budget_tokens": 1024},
                "output_config": {"effort": "high"},
            })),
            Ok(Some(ReasoningLevel::High)),
            "an explicit effort wins over the budget"
        );
        assert_eq!(
            messages_effort(&json!({"thinking": {"type": "adaptive"}})),
            Ok(None)
        );
    }

    #[test]
    fn a_family_serves_the_default_when_no_level_is_named() {
        let f = ModelFamily {
            id: Uuid::nil(),
            name: "olmo3-7b".into(),
            description: String::new(),
            default_level: ReasoningLevel::None,
            members: FamilyMembers {
                none: "olmo3-7b-instruct".into(),
                low: "olmo3-7b-think".into(),
                medium: "olmo3-7b-think".into(),
                high: "olmo3-7b-think".into(),
            },
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        assert_eq!(
            f.member_for(None),
            (ReasoningLevel::None, "olmo3-7b-instruct")
        );
        assert_eq!(
            f.member_for(Some(ReasoningLevel::High)),
            (ReasoningLevel::High, "olmo3-7b-think")
        );
        assert_eq!(
            f.members.distinct(),
            vec!["olmo3-7b-instruct", "olmo3-7b-think"]
        );
    }
}
