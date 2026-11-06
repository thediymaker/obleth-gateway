//! Data-plane side of `auto` conversation stickiness: the store lookups, the
//! complaint question, and recording the turn once it has been answered. The
//! decisions themselves are pure and live in [`obleth_config::routing::sticky`].

use axum::body::Body;
use axum::http::{HeaderValue, Response};
use obleth_config::routing::sticky::{self, Decision, Observed, Reason, SessionRecord, TurnRecord};
use obleth_config::routing::{Candidate, Intent};
use uuid::Uuid;

use crate::state::AppState;

/// Response header naming the model `auto` served.
pub(crate) const AUTO_MODEL_HEADER: &str = "x-obleth-auto-model";
/// Response header saying why, when stickiness is on (see [`Reason`]).
pub(crate) const AUTO_REASON_HEADER: &str = "x-obleth-auto-reason";
/// Request header a client sends when it sees the model it was given failing:
/// `tool_loop` (repeating itself) or `complaint` (the person said it is
/// wrong). Either moves the conversation up a level, as the gateway's own
/// evidence would.
pub(crate) const AUTO_ESCALATE_HEADER: &str = "x-obleth-auto-escalate";

/// Apply a client's [`AUTO_ESCALATE_HEADER`] to what the request shows.
pub(crate) fn client_signal(observed: &mut Observed, value: Option<&str>) {
    match value.map(str::trim) {
        Some("tool_loop") => observed.tool_loop = true,
        Some("complaint") => observed.complaint = true,
        _ => {}
    }
}

/// Value stored under a retry key: the request was answered.
const ANSWERED: &str = "1";

/// What the store and the request say about the conversation so far.
pub(crate) struct Conversation {
    pub record: Option<TurnRecord>,
    /// The conversation's memory, when the client names it.
    pub session: Option<SessionRecord>,
    pub observed: Observed,
    retry_key: Option<String>,
    session_key: Option<String>,
    user: String,
}

/// Find the conversation's incumbent, and whether this exact request was
/// already answered. A conversation the client names (`session_id`) is found by
/// that name first; otherwise, or when it has no record yet, by the fingerprint
/// of its last answer. A store error reads as a new conversation.
pub(crate) async fn lookup(
    state: &AppState,
    tenant: &Uuid,
    json: &serde_json::Value,
    session_id: Option<&str>,
) -> Conversation {
    let session_key = session_id.map(sticky::session_key);
    // Within a named conversation, "the same request again" means in this
    // conversation: two chats that open the same way are not a regenerate.
    let retry_key = sticky::retry_key(json).map(|k| match &session_key {
        Some(s) => format!("{s}:{k}"),
        None => k,
    });
    let get = |key: Option<String>| async move {
        match key {
            Some(k) => state
                .redis
                .auto_turn_get(tenant, &k)
                .await
                .map_err(|e| tracing::debug!(error = %e, "auto turn lookup failed"))
                .ok()
                .flatten(),
            None => None,
        }
    };
    let (session, by_answer, seen) = tokio::join!(
        get(session_key.clone()),
        get(sticky::incumbent_key(json)),
        get(retry_key.clone()),
    );
    let session = session.and_then(|raw| serde_json::from_str::<SessionRecord>(&raw).ok());
    let record = session
        .as_ref()
        .and_then(|s| s.last.clone())
        .or_else(|| by_answer.and_then(|raw| serde_json::from_str::<TurnRecord>(&raw).ok()));
    Conversation {
        observed: sticky::observe(json, seen.is_some() && record.is_some()),
        record,
        session: session_key.as_ref().map(|_| session.unwrap_or_default()),
        retry_key,
        session_key,
        user: sticky::newest_user_text(json),
    }
}

/// Ask the classifier whether the user's newest message says the last answer
/// was wrong. Only asked when there is an incumbent to judge and the user, not
/// a tool, spoke last; `false` whenever the classifier cannot answer.
pub(crate) async fn complaint(
    state: &AppState,
    settings: &obleth_config::AutoRouterSettings,
    conversation: Option<&Conversation>,
    json: &serde_json::Value,
    skip_classifier: bool,
) -> bool {
    let Some(conversation) = conversation else {
        return false;
    };
    if conversation.record.is_none()
        || skip_classifier
        || !settings.classifier_active()
        || !sticky::user_spoke_last(json)
    {
        return false;
    }
    let Some(name) = settings.classifier_model.as_deref() else {
        return false;
    };
    if name == crate::router::AUTO_MODEL_NAME {
        return false;
    }
    let Some(brain) = crate::proxy::resolve_model(state, name).await else {
        return false;
    };
    let message = conversation.user.trim();
    if message.is_empty() {
        return false;
    }
    state
        .classifier
        .complaint(&state.http, &brain, message)
        .await
        .is_some_and(|p| p >= sticky::COMPLAINT_THRESHOLD)
}

/// The candidates a sticky turn is routed over, and why.
pub(crate) struct Choice {
    /// `None` routes over every candidate.
    pub pool: Option<Vec<Candidate>>,
    pub reason: Reason,
    pub floor: u8,
    pub domains: Vec<String>,
    pub strikes: u8,
}

/// Narrow the candidates for this turn. `serves` says whether a pool would
/// produce a pick for this request (the router's hard filters), so the floor
/// can clamp down past models that cannot take it.
pub(crate) fn choose(
    candidates: &[Candidate],
    record: Option<&TurnRecord>,
    session: Option<&SessionRecord>,
    intent: &Intent,
    observed: &Observed,
    serves: impl Fn(&[Candidate]) -> bool,
) -> Choice {
    let Some(record) = record else {
        return Choice {
            pool: None,
            reason: Reason::Fresh,
            floor: 0,
            domains: sticky::domains_of(&intent.tags),
            strikes: 0,
        };
    };
    let plan = sticky::decide(record, intent, observed);
    let incumbent = candidates
        .iter()
        .find(|c| c.model.model_name == record.model);
    let choice = |pool, reason| Choice {
        pool,
        reason,
        floor: plan.floor,
        domains: plan.domains.clone(),
        strikes: plan.strikes,
    };

    // Back to a topic this conversation has been through: the model that
    // served it then takes it again (code, then docs, then code goes back to
    // the coding model).
    if plan.decision == Decision::Topic {
        let returning = session
            .and_then(|s| s.standing(&plan.domains))
            .and_then(|s| {
                candidates
                    .iter()
                    .find(|c| c.model.model_name == s.model)
                    .map(|c| (c, s.rung))
            });
        // It must still be strong enough for this turn; a harder one routes on.
        let returning = returning.filter(|(c, _)| c.strength(&plan.domains) >= intent.difficulty);
        if let Some((c, rung)) = returning {
            let only = std::slice::from_ref(c);
            if serves(only) {
                return Choice {
                    pool: Some(only.to_vec()),
                    reason: Reason::Returned,
                    floor: rung
                        .max(intent.difficulty)
                        .min(obleth_config::MAX_TIER_LEVEL),
                    domains: plan.domains.clone(),
                    strikes: 0,
                };
            }
        }
    }

    // The incumbent stays whenever the evidence allows it and it can take the
    // request: on a quiet turn, on a harder one it is strong enough for, and
    // on a new topic when no model that could take it is stronger there.
    let keepable = match plan.decision {
        Decision::Keep => true,
        Decision::Harder => incumbent.is_some_and(|c| c.strength(&plan.domains) >= plan.floor),
        Decision::Topic => incumbent.is_some_and(|c| {
            let held = c.strength(&plan.domains);
            held >= intent.difficulty.max(1)
                && !candidates
                    .iter()
                    .any(|o| o.strength(&plan.domains) > held && serves(std::slice::from_ref(o)))
        }),
        Decision::Escalate(_) => false,
    };
    if keepable {
        if let Some(c) = incumbent {
            let only = std::slice::from_ref(c);
            if serves(only) {
                return choice(Some(only.to_vec()), Reason::Kept);
            }
        }
    }
    let reason = match plan.decision {
        Decision::Keep => Reason::Forced,
        Decision::Escalate(signal) => Reason::Escalated(signal),
        Decision::Harder => Reason::Harder,
        Decision::Topic => Reason::Topic,
    };
    // An escalation moves away from the incumbent, and so does a new topic it
    // gave up to a stronger model: the router picks within the topic, by its
    // tags, from the others.
    let exclude = matches!(plan.decision, Decision::Escalate(_) | Decision::Topic)
        .then_some(record.model.as_str());
    for floor in (0..=plan.floor).rev() {
        let pool = sticky::floor_pool(candidates, &plan.domains, floor, exclude);
        if !pool.is_empty() && serves(&pool) {
            return choice(Some(pool), reason);
        }
    }
    // Nothing else can take it: route as usual, which may keep the incumbent.
    choice(None, reason)
}

/// A routed turn waiting for its answer, to be stored once it is served.
pub(crate) struct Turn {
    tenant: Uuid,
    user: String,
    retry_key: Option<String>,
    /// The named conversation's key and memory, updated with this turn.
    session: Option<(String, SessionRecord)>,
    record: TurnRecord,
}

impl Turn {
    pub(crate) fn new(
        tenant: Uuid,
        conversation: Conversation,
        choice: &Choice,
        chosen: &Candidate,
    ) -> Self {
        let rung = chosen.strength(&choice.domains).max(choice.floor);
        Turn {
            tenant,
            user: conversation.user,
            retry_key: conversation.retry_key,
            session: conversation.session_key.zip(conversation.session),
            record: TurnRecord {
                model: chosen.model.model_name.clone(),
                domains: choice.domains.clone(),
                rung,
                strikes: choice.strikes,
                bad_output: false,
            },
        }
    }

    /// Store the turn under the answer the client received, and mark the
    /// request answered. Runs in the background; a store error only costs the
    /// next turn its incumbent.
    pub(crate) fn record(self, state: &AppState, completion: &serde_json::Value) {
        let Some(answer) = completion.pointer("/choices/0/message") else {
            return;
        };
        let key = sticky::turn_key(&self.user, answer);
        let mut record = self.record;
        record.bad_output = sticky::bad_output(completion);
        let Ok(value) = serde_json::to_string(&record) else {
            return;
        };
        let session = self.session.and_then(|(key, mut session)| {
            session.remember(&record);
            serde_json::to_string(&session).ok().map(|v| (key, v))
        });
        let redis = state.redis.clone();
        let tenant = self.tenant;
        let retry_key = self.retry_key;
        tokio::spawn(async move {
            let ttl = sticky::STICKY_TTL_SECS;
            if let Some((key, value)) = session {
                let ttl = sticky::SESSION_TTL_SECS;
                if let Err(e) = redis.auto_turn_put(&tenant, &key, &value, ttl).await {
                    tracing::debug!(error = %e, "auto session store failed");
                }
            }
            if let Err(e) = redis.auto_turn_put(&tenant, &key, &value, ttl).await {
                tracing::debug!(error = %e, "auto turn store failed");
            }
            if let Some(retry) = retry_key {
                let _ = redis.auto_turn_put(&tenant, &retry, ANSWERED, ttl).await;
            }
        });
    }
}

/// How an `auto` request was routed, for the response headers.
pub(crate) struct Note {
    pub model: String,
    /// `None` when stickiness is off.
    pub reason: Option<Reason>,
}

impl Note {
    pub(crate) fn apply_headers(&self, resp: &mut Response<Body>) {
        if let Ok(v) = HeaderValue::from_str(&self.model) {
            resp.headers_mut().insert(AUTO_MODEL_HEADER, v);
        }
        if let Some(reason) = self.reason {
            resp.headers_mut().insert(
                AUTO_REASON_HEADER,
                HeaderValue::from_static(reason.as_str()),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obleth_config::routing::IntentSource;

    fn candidate(name: &str, level: u8) -> Candidate {
        let mut model = crate::boons::test_support::test_route();
        model.model_name = name.to_string();
        Candidate {
            model,
            healthy: true,
            levels: vec![("coding".into(), level)],
        }
    }

    fn record(model: &str, rung: u8) -> TurnRecord {
        TurnRecord {
            model: model.into(),
            domains: vec!["coding".into()],
            rung,
            strikes: 0,
            bad_output: false,
        }
    }

    fn coding(difficulty: u8) -> Intent {
        Intent {
            tags: vec!["coding".into()],
            difficulty,
            source: IntentSource::Classifier,
        }
    }

    fn names(choice: &Choice) -> Vec<&str> {
        choice
            .pool
            .as_ref()
            .map(|p| p.iter().map(|c| c.model.model_name.as_str()).collect())
            .unwrap_or_default()
    }

    #[test]
    fn a_quiet_turn_keeps_the_incumbent_even_when_others_are_free() {
        let all = vec![
            candidate("small", 1),
            candidate("mid", 2),
            candidate("big", 3),
        ];
        let choice = choose(
            &all,
            Some(&record("mid", 2)),
            None,
            &coding(1),
            &Observed::default(),
            |_| true,
        );
        assert_eq!(choice.reason, Reason::Kept);
        assert_eq!(names(&choice), ["mid"]);
    }

    #[test]
    fn a_complaint_moves_up_and_away_from_the_incumbent() {
        let all = vec![
            candidate("small", 1),
            candidate("mid", 2),
            candidate("big", 3),
        ];
        let observed = Observed {
            complaint: true,
            ..Default::default()
        };
        let choice = choose(
            &all,
            Some(&record("small", 1)),
            None,
            &coding(1),
            &observed,
            |_| true,
        );
        assert_eq!(choice.reason, Reason::Escalated(sticky::Signal::Complaint));
        assert_eq!(names(&choice), ["mid", "big"]);

        // At the top, the move is sideways to another top model.
        let tops = vec![
            candidate("big-a", 3),
            candidate("big-b", 3),
            candidate("mid", 2),
        ];
        let choice = choose(
            &tops,
            Some(&record("big-a", 3)),
            None,
            &coding(1),
            &observed,
            |_| true,
        );
        assert_eq!(names(&choice), ["big-b"]);
    }

    #[test]
    fn an_incumbent_that_cannot_serve_is_replaced_at_its_rung_or_above() {
        let all = vec![
            candidate("small", 1),
            candidate("mid", 2),
            candidate("big", 3),
        ];
        let serves = |pool: &[Candidate]| pool.iter().any(|c| c.model.model_name != "mid");
        let choice = choose(
            &all,
            Some(&record("mid", 2)),
            None,
            &coding(1),
            &Observed::default(),
            serves,
        );
        assert_eq!(choice.reason, Reason::Forced);
        assert_eq!(names(&choice), ["mid", "big"]);
    }

    #[test]
    fn the_floor_clamps_down_when_nothing_at_it_can_serve() {
        let all = vec![
            candidate("small", 1),
            candidate("mid", 2),
            candidate("big", 3),
        ];
        let observed = Observed {
            regenerate: true,
            ..Default::default()
        };
        // Only "small" can take this request (say, the context is too long
        // for the others).
        let serves = |pool: &[Candidate]| pool.iter().any(|c| c.model.model_name == "small");
        let choice = choose(
            &all,
            Some(&record("mid", 2)),
            None,
            &coding(1),
            &observed,
            serves,
        );
        assert_eq!(names(&choice), ["small", "big"]);
    }

    #[test]
    fn a_harder_turn_keeps_an_incumbent_strong_enough_for_it() {
        let all = vec![candidate("mid", 2), candidate("big", 3)];
        let strong = choose(
            &all,
            Some(&record("big", 1)),
            None,
            &coding(3),
            &Observed::default(),
            |_| true,
        );
        assert_eq!(strong.reason, Reason::Kept);
        let weak = choose(
            &all,
            Some(&record("mid", 2)),
            None,
            &coding(3),
            &Observed::default(),
            |_| true,
        );
        assert_eq!(weak.reason, Reason::Harder);
        assert_eq!(names(&weak), ["big"]);
    }

    #[test]
    fn a_new_topic_stays_with_an_incumbent_that_holds_it() {
        let mut writer = candidate("writer", 2);
        writer.levels = vec![("writing".into(), 2)];
        let mut both = candidate("both", 3);
        both.levels.push(("writing".into(), 2));
        let all = vec![candidate("coder", 3), both, writer];
        let writing = Intent {
            tags: vec!["writing".into()],
            difficulty: 1,
            source: IntentSource::Classifier,
        };
        let held = choose(
            &all,
            Some(&record("both", 3)),
            None,
            &writing,
            &Observed::default(),
            |_| true,
        );
        assert_eq!(held.reason, Reason::Kept);
        assert_eq!(held.domains, vec!["writing".to_string()]);
        let moved = choose(
            &all,
            Some(&record("coder", 3)),
            None,
            &writing,
            &Observed::default(),
            |_| true,
        );
        assert_eq!(moved.reason, Reason::Topic);
        assert_eq!(names(&moved), ["both", "writer"]);
    }

    #[test]
    fn the_new_record_never_drops_below_the_floor() {
        let small = candidate("small", 1);
        let conversation = Conversation {
            record: Some(record("big", 3)),
            session: None,
            observed: Observed::default(),
            retry_key: None,
            session_key: None,
            user: "now write it up".into(),
        };
        let choice = Choice {
            pool: None,
            reason: Reason::Topic,
            floor: 3,
            domains: vec!["coding".into()],
            strikes: 0,
        };
        let turn = Turn::new(Uuid::nil(), conversation, &choice, &small);
        assert_eq!(turn.record.rung, 3);
        assert_eq!(turn.record.model, "small");
    }

    fn leveled(name: &str, levels: &[(&str, u8)]) -> Candidate {
        let mut c = candidate(name, 0);
        c.levels = levels.iter().map(|(d, l)| (d.to_string(), *l)).collect();
        c
    }

    fn tagged(tag: &str, difficulty: u8) -> Intent {
        Intent {
            tags: vec![tag.into()],
            difficulty,
            source: IntentSource::Classifier,
        }
    }

    /// The fleet of the 11-06 chat that looped: a small coder that also chats,
    /// a big coder that also writes, and a writer.
    fn fleet() -> Vec<Candidate> {
        vec![
            leveled("north-mini", &[("*", 1), ("coding", 2)]),
            leveled("glm", &[("*", 2), ("coding", 3), ("writing", 2)]),
            leveled("hemmingway", &[("*", 2), ("writing", 3)]),
        ]
    }

    fn turn_record(model: &str, domain: &str, rung: u8) -> TurnRecord {
        TurnRecord {
            model: model.into(),
            domains: vec![domain.into()],
            rung,
            strikes: 0,
            bad_output: false,
        }
    }

    #[test]
    fn hello_then_build_moves_to_the_strongest_coder() {
        let all = fleet();
        let hello = turn_record("north-mini", "*", 1);
        let choice = choose(
            &all,
            Some(&hello),
            None,
            &tagged("coding", 2),
            &Observed::default(),
            |_| true,
        );
        // north-mini holds coding, but glm is stronger there.
        assert_eq!(choice.reason, Reason::Topic);
        assert!(!names(&choice).contains(&"north-mini"));
        assert!(names(&choice).contains(&"glm"));
    }

    #[test]
    fn go_ahead_after_a_coding_turn_stays_on_the_coder() {
        let all = fleet();
        let build = turn_record("glm", "coding", 3);
        for intent in [tagged("general", 1), tagged("coding", 1)] {
            let choice = choose(
                &all,
                Some(&build),
                None,
                &intent,
                &Observed::default(),
                |_| true,
            );
            assert_eq!(choice.reason, Reason::Kept);
            assert_eq!(names(&choice), ["glm"]);
        }
    }

    #[test]
    fn docs_go_to_the_writer_and_code_comes_back_to_the_coder() {
        let all = fleet();
        let mut session = SessionRecord::default();
        session.remember(&turn_record("north-mini", "*", 1));
        session.remember(&turn_record("glm", "coding", 3));

        let docs = choose(
            &all,
            session.last.as_ref(),
            Some(&session),
            &tagged("writing", 2),
            &Observed::default(),
            |_| true,
        );
        // glm holds writing at 2, but hemmingway is stronger there.
        assert_eq!(docs.reason, Reason::Topic);
        assert_eq!(names(&docs), ["hemmingway"]);
        session.remember(&turn_record("hemmingway", "writing", 3));

        let code = choose(
            &all,
            session.last.as_ref(),
            Some(&session),
            &tagged("coding", 1),
            &Observed::default(),
            |_| true,
        );
        assert_eq!(code.reason, Reason::Returned);
        assert_eq!(names(&code), ["glm"]);
        assert_eq!(code.floor, 3);
    }

    #[test]
    fn hello_tagged_general_does_not_hold_a_coding_turn() {
        // The classifier tags "Hello" general and a build request
        // coding + creative + general (11-06, obleth-dev).
        let all = fleet();
        let mut session = SessionRecord::default();
        session.remember(&turn_record("north-mini", "general", 1));
        let build = Intent {
            tags: vec!["coding".into(), "creative".into(), "general".into()],
            difficulty: 2,
            source: IntentSource::Classifier,
        };
        let choice = choose(
            &all,
            session.last.as_ref(),
            Some(&session),
            &build,
            &Observed::default(),
            |_| true,
        );
        assert_eq!(choice.reason, Reason::Topic);
        assert_eq!(names(&choice), ["glm"]);
    }

    #[test]
    fn a_coding_turn_tagged_creative_does_not_drop_the_floor_to_a_writer() {
        // The pool is everyone at the floor in any of the turn's topics, and
        // the router then scores them on the tags; the floor itself stays the
        // conversation's, not one above the incumbent.
        let mut all = fleet();
        all.push(leveled("writer", &[("creative", 3), ("writing", 3)]));
        let hello = turn_record("north-mini", "general", 1);
        let build = Intent {
            tags: vec!["coding".into(), "creative".into(), "general".into()],
            difficulty: 2,
            source: IntentSource::Classifier,
        };
        let choice = choose(
            &all,
            Some(&hello),
            None,
            &build,
            &Observed::default(),
            |_| true,
        );
        assert_eq!(choice.reason, Reason::Topic);
        assert_eq!(choice.floor, 2);
        assert!(names(&choice).contains(&"glm"));
        assert!(!names(&choice).contains(&"north-mini"));
    }

    #[test]
    fn a_returning_model_too_weak_for_a_harder_turn_is_passed_over() {
        let all = fleet();
        let mut session = SessionRecord::default();
        session.remember(&turn_record("north-mini", "coding", 2));
        session.remember(&turn_record("hemmingway", "writing", 3));
        let choice = choose(
            &all,
            session.last.as_ref(),
            Some(&session),
            &tagged("coding", 3),
            &Observed::default(),
            |_| true,
        );
        assert_eq!(choice.reason, Reason::Topic);
        assert_eq!(names(&choice), ["glm"]);
    }

    #[test]
    fn a_returning_model_that_cannot_serve_routes_within_the_topic() {
        let all = fleet();
        let mut session = SessionRecord::default();
        session.remember(&turn_record("glm", "coding", 3));
        session.remember(&turn_record("hemmingway", "writing", 3));
        let serves = |pool: &[Candidate]| pool.iter().any(|c| c.model.model_name != "glm");
        let code = choose(
            &all,
            session.last.as_ref(),
            Some(&session),
            &tagged("coding", 1),
            &Observed::default(),
            serves,
        );
        assert_eq!(code.reason, Reason::Topic);
        assert!(!names(&code).contains(&"hemmingway"));
    }

    #[test]
    fn an_equally_strong_incumbent_keeps_a_new_topic() {
        let all = vec![
            leveled("both", &[("coding", 3), ("writing", 3)]),
            leveled("writer", &[("writing", 3)]),
        ];
        let choice = choose(
            &all,
            Some(&turn_record("both", "coding", 3)),
            None,
            &tagged("writing", 1),
            &Observed::default(),
            |_| true,
        );
        assert_eq!(choice.reason, Reason::Kept);
        assert_eq!(names(&choice), ["both"]);
    }

    #[test]
    fn a_client_can_say_the_model_is_stuck() {
        let mut observed = Observed::default();
        client_signal(&mut observed, Some("tool_loop"));
        assert!(observed.tool_loop);
        let all = fleet();
        let choice = choose(
            &all,
            Some(&turn_record("north-mini", "coding", 2)),
            None,
            &tagged("coding", 1),
            &observed,
            |_| true,
        );
        assert_eq!(choice.reason, Reason::Escalated(sticky::Signal::ToolLoop));
        assert_eq!(names(&choice), ["glm"]);

        let mut observed = Observed::default();
        client_signal(&mut observed, Some("something else"));
        client_signal(&mut observed, None);
        assert_eq!(observed, Observed::default());
    }
}
