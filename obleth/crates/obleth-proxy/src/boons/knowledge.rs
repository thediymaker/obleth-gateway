//! The knowledge boon: ground a chat request on admin-curated documents.
//!
//! Fail-open without exception. No embedder, a timeout, an empty index, a
//! dimension mismatch, or nothing above threshold all leave the body exactly
//! as it arrived.
//!
//! The boon runs in two phases inside `enrich_request`, not one block:
//! - **Phase 1** (before compression) captures the retrieval query from the
//!   untouched messages.
//! - **Phase 2** (after compression AND after guardrails) retrieves, packs,
//!   and injects.
//!
//! Phase 1 sits before compression because the compression boon's dedup/lossy
//! passes iterate every message regardless of role and gate purely on a
//! token-count floor: the trailing user turn the query is drawn from is an
//! eligible compression target, and extracting the query late risks embedding
//! a `[ref:HASH]` marker instead of the user's actual words.
//!
//! Phase 2 sits after compression for the mirror-image reason: the injected
//! block would itself be an eligible compression target (paraphrased by lossy
//! compression, or collapsed to a reference on a later turn). It also sits
//! after guardrails' input scan, not before: guardrails scans every message
//! including system content, so injecting first would let retrieved
//! institutional text (an email address in a policy doc, the literal phrase
//! "prompt injection" in an AI-use policy) trip a tenant's own scanner and
//! block a request the boon must never fail.

use obleth_config::{KnowledgeBoonSettings, ResolvedKey, ResolvedModel};
use serde_json::{json, Value};

use crate::knowledge::Hit;

/// Output tokens we refuse to crowd out when clamping the injection budget.
const OUTPUT_RESERVE_TOKENS: i64 = 512;

/// The model opted in, the boon is globally on, this is a chat request, and the
/// key is not an internal probe key.
pub fn eligible(
    route: &ResolvedModel,
    settings: &KnowledgeBoonSettings,
    key: &ResolvedKey,
    is_chat: bool,
) -> bool {
    is_chat
        && !key.internal
        && settings.active()
        && !route.knowledge_collections.is_empty()
        && route.boons.iter().any(|b| b == "knowledge")
}

/// Concatenate the last `turns` user messages, newest first. A short follow-up
/// on its own ("what about grad students?") retrieves nothing useful, so the
/// prior turn rides along.
pub fn extract_query(json: &Value, turns: u32) -> Option<String> {
    let messages = json.get("messages")?.as_array()?;
    let mut parts: Vec<String> = Vec::new();
    for msg in messages.iter().rev() {
        if parts.len() >= turns.max(1) as usize {
            break;
        }
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        if let Some(text) = content_text(msg.get("content")) {
            if !text.trim().is_empty() {
                parts.push(text);
            }
        }
    }
    if parts.is_empty() {
        return None;
    }
    let mut query = parts.join("\n");
    // Long queries embed poorly and cost latency. `parts` is newest-first, so
    // keeping the head (the first 2000 chars) and dropping the tail discards
    // the *older* turns first — the intent lives in the most recent turns,
    // which sit at the front of the joined string, not at the end of it.
    if query.chars().count() > 2000 {
        query = query.chars().take(2000).collect();
    }
    Some(query)
}

/// Plain string content, or the concatenated `text` parts of a multimodal array.
fn content_text(content: Option<&Value>) -> Option<String> {
    match content? {
        Value::String(s) => Some(s.clone()),
        Value::Array(parts) => {
            let text: Vec<&str> = parts
                .iter()
                .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect();
            if text.is_empty() {
                None
            } else {
                Some(text.join(" "))
            }
        }
        _ => None,
    }
}

/// Tokens available for injection: the configured ceiling, clamped so the
/// request plus injection plus an output reserve still fits the context window.
pub fn available_budget(max_context_tokens: u32, context_window: i64, estimated: u32) -> u32 {
    let room = context_window - estimated as i64 - OUTPUT_RESERVE_TOKENS;
    if room <= 0 {
        return 0;
    }
    (max_context_tokens as i64).min(room) as u32
}

/// Fixed token cost of `render_block`'s preamble and closing tag. No
/// tokenizer is available in this pure function, so this is an approximation
/// (roughly the true cost of the header sentence + `<knowledge>`/
/// `</knowledge>` tags) charged once against the budget up front, rather than
/// relying entirely on `OUTPUT_RESERVE_TOKENS` to absorb it.
const RENDER_PREAMBLE_TOKENS: u32 = 30;
/// Approximate token cost of each hit's own `[N] Title` label line and
/// surrounding newlines in the rendered block, charged per hit on top of its
/// `token_count`.
const RENDER_PER_HIT_OVERHEAD_TOKENS: u32 = 8;

/// Keep hits in score order while they fit the budget. Accounts for
/// `render_block`'s own overhead (preamble + per-hit label lines), not just
/// each hit's raw `token_count`, so the rendered block does not overrun the
/// budget it was packed against.
pub fn pack(hits: Vec<Hit>, budget: u32) -> Vec<Hit> {
    let mut remaining = budget as i64 - RENDER_PREAMBLE_TOKENS as i64;
    let mut out = Vec::new();
    for h in hits {
        let cost = h.token_count as i64 + RENDER_PER_HIT_OVERHEAD_TOKENS as i64;
        if remaining < cost {
            continue;
        }
        remaining -= cost;
        out.push(h);
    }
    out
}

/// Render the injected block. The hedge matters: a weak retrieval should not
/// drag the answer away from what the model already knows.
pub fn render_block(hits: &[Hit]) -> String {
    let mut s = String::from(
        "<knowledge>\nThe following institutional knowledge may be relevant. Use it \
         when it answers the question; ignore it when it does not.\n",
    );
    for (i, h) in hits.iter().enumerate() {
        s.push_str(&format!("\n[{}] {}\n{}\n", i + 1, h.title, h.text));
    }
    s.push_str("</knowledge>");
    s
}

/// Place the block. Many models honor only the first system message, so an
/// existing one is extended rather than joined by a second — that holds
/// whether its content is a plain string or an OpenAI-style array of parts,
/// and regardless of *where* in the message list that system message sits.
/// `[user, system, user]` is unusual but legal (some clients emit it), and
/// only checking `messages[0]` would miss it and fall through to inserting a
/// second system message ahead of it; treating an array as "no content"
/// would do the same. Either way the tenant's own system prompt gets
/// silently demoted to second place, and several chat templates reject two
/// system messages outright.
pub fn inject(json: &mut Value, block: &str, supports_system: bool) -> bool {
    let Some(messages) = json.get_mut("messages").and_then(Value::as_array_mut) else {
        return false;
    };
    if supports_system {
        if let Some(existing) = messages
            .iter_mut()
            .find(|m| m.get("role").and_then(Value::as_str) == Some("system"))
        {
            match existing.get_mut("content") {
                Some(Value::String(existing)) => {
                    existing.push_str("\n\n");
                    existing.push_str(block);
                    return true;
                }
                Some(Value::Array(parts)) => {
                    parts.push(json!({"type": "text", "text": block}));
                    return true;
                }
                _ => {}
            }
        }
        messages.insert(0, json!({"role": "system", "content": block}));
        return true;
    }
    // No system-message support: prepend to the LATEST user turn instead —
    // that is the turn the retrieval query was drawn from (see
    // `extract_query`), so the grounding must attach there. Scanning forward
    // for the first user turn with string content would find an *older* turn
    // whenever the latest one happens to carry array content, attaching the
    // block to the wrong message.
    for msg in messages.iter_mut().rev() {
        if msg.get("role").and_then(Value::as_str) != Some("user") {
            continue;
        }
        match msg.get_mut("content") {
            Some(Value::String(existing)) => {
                let merged = format!("{block}\n\n{existing}");
                *existing = merged;
                return true;
            }
            Some(Value::Array(parts)) => {
                parts.insert(0, json!({"type": "text", "text": block}));
                return true;
            }
            _ => return false,
        }
    }
    false
}

/// What one retrieval attempt did, so the metric can distinguish "the corpus
/// had nothing" from "the embedder was unreachable".
pub struct Retrieval {
    pub outcome: &'static str,
    pub hits: Vec<Hit>,
}

/// Retrieve and inject, given a query already extracted (in phase 1, before
/// any compression pass touched the request). Embeds the query once per
/// distinct embedder among the attached collections, scores every collection,
/// merges, packs against the budget, and injects. Always fails open: any
/// failure leaves `json` untouched and reports an outcome the caller records.
pub async fn apply(
    state: &crate::state::AppState,
    settings: &KnowledgeBoonSettings,
    route: &ResolvedModel,
    estimated_tokens: u32,
    query: &str,
    json: &mut Value,
) -> Retrieval {
    let slabs = state.knowledge.snapshot();

    // One embedding *attempt* per distinct embedder, not per collection —
    // `None` is cached just like `Some`, so a dead embedder shared by several
    // collections pays `embed_timeout_ms` once per request, not once per
    // collection.
    let mut vectors: std::collections::HashMap<String, Option<Vec<f32>>> = Default::default();
    let mut all: Vec<Hit> = Vec::new();
    // Did at least one attached collection have data, and did every embed
    // attempt for it fail? That is the "error" outcome (an outage), distinct
    // from "ran fine but nothing scored" (a "miss").
    let mut any_nonempty_slab = false;
    let mut any_embed_succeeded = false;
    for id in &route.knowledge_collections {
        let Some(slab) = slabs.get(id) else { continue };
        if slab.chunks.is_empty() || slab.embedding_model.is_empty() {
            continue;
        }
        any_nonempty_slab = true;
        let vector = if let Some(cached) = vectors.get(&slab.embedding_model) {
            match cached {
                Some(v) => v.clone(),
                None => continue, // this embedder already failed this request
            }
        } else {
            match crate::knowledge::embed_query(state, &slab.embedding_model, query, settings).await
            {
                Some(v) => {
                    vectors.insert(slab.embedding_model.clone(), Some(v.clone()));
                    v
                }
                None => {
                    vectors.insert(slab.embedding_model.clone(), None);
                    continue;
                }
            }
        };
        any_embed_succeeded = true;
        all.extend(slab.retrieve(&vector, settings.top_k as usize, settings.min_score));
    }

    if any_nonempty_slab && !any_embed_succeeded {
        return Retrieval {
            outcome: "error",
            hits: Vec::new(),
        };
    }
    if all.is_empty() {
        return Retrieval {
            outcome: "miss",
            hits: Vec::new(),
        };
    }
    all.sort_by(|a, b| b.score.total_cmp(&a.score));
    all.truncate(settings.top_k as usize);

    let budget = available_budget(
        settings.max_context_tokens,
        route.context_window,
        estimated_tokens,
    );
    let packed = pack(all, budget);
    if packed.is_empty() {
        return Retrieval {
            outcome: "miss",
            hits: Vec::new(),
        };
    }
    let block = render_block(&packed);
    if inject(json, &block, route.supports_system_messages) {
        Retrieval {
            outcome: "hit",
            hits: packed,
        }
    } else {
        Retrieval {
            outcome: "miss",
            hits: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn hit(title: &str, text: &str, tokens: u32, score: f32) -> Hit {
        Hit {
            id: Uuid::new_v4(),
            title: title.into(),
            text: text.into(),
            token_count: tokens,
            score,
        }
    }

    // `ResolvedKey` (29 fields) and `ResolvedModel` (36 fields) derive
    // neither `Default` nor anything an abbreviated struct literal could
    // lean on. Rather than duplicate two more field-by-field constructors,
    // this reuses `boons::tests::test_route` / `test_key_with_policy`
    // (made `pub(super)` for exactly this) and layers the fields each
    // eligibility test actually cares about on top.
    //
    // `context_window` and `supports_system_messages` are set explicitly
    // here rather than left at `test_route()`'s defaults (`0` / `false`):
    // since Task 11's fix round, a non-positive `context_window` makes
    // `apply` short-circuit to the `"no_window"` outcome before retrieval
    // ever runs. These `eligible()` tests do not call `apply`, so that
    // short-circuit is not itself in play here — but leaving the fields at
    // their zero-value defaults would make the intent invisible in the test
    // body, so they are pinned to values appropriate to an otherwise
    // eligible route.
    fn route(boons: &[&str], collections: usize) -> ResolvedModel {
        let mut r = crate::boons::tests::test_route();
        r.boons = boons.iter().map(|s| s.to_string()).collect();
        r.knowledge_collections = (0..collections).map(|_| Uuid::new_v4()).collect();
        r.context_window = 8192;
        r.supports_system_messages = true;
        r
    }

    fn key(internal: bool) -> ResolvedKey {
        let mut k = crate::boons::tests::test_key_with_policy(None);
        k.internal = internal;
        k
    }

    fn on() -> KnowledgeBoonSettings {
        KnowledgeBoonSettings {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn extracts_the_last_user_message() {
        let body = json!({"messages": [
            {"role": "system", "content": "be helpful"},
            {"role": "user", "content": "what is the refund policy?"}
        ]});
        assert_eq!(
            extract_query(&body, 1).as_deref(),
            Some("what is the refund policy?")
        );
    }

    #[test]
    fn extracts_two_turns_so_short_followups_retrieve() {
        // "what about grad students?" alone retrieves nothing useful.
        let body = json!({"messages": [
            {"role": "user", "content": "what is the refund policy?"},
            {"role": "assistant", "content": "Refunds are..."},
            {"role": "user", "content": "what about grad students?"}
        ]});
        let q = extract_query(&body, 2).expect("query");
        assert!(q.contains("grad students"));
        assert!(q.contains("refund policy"));
    }

    #[test]
    fn extracts_text_parts_from_multimodal_content() {
        let body = json!({"messages": [{"role": "user", "content": [
            {"type": "text", "text": "what is the policy?"},
            {"type": "image_url", "image_url": {"url": "http://x/y.png"}}
        ]}]});
        assert_eq!(
            extract_query(&body, 1).as_deref(),
            Some("what is the policy?")
        );
    }

    #[test]
    fn no_user_message_yields_no_query() {
        let body = json!({"messages": [{"role": "system", "content": "hi"}]});
        assert!(extract_query(&body, 2).is_none());
    }

    #[test]
    fn pack_stops_at_the_budget() {
        let hits = vec![
            hit("A", "a", 100, 0.9),
            hit("B", "b", 100, 0.8),
            hit("C", "c", 100, 0.7),
        ];
        let packed = pack(hits, 250);
        assert_eq!(packed.len(), 2, "third chunk does not fit");
        assert_eq!(packed[0].title, "A");
    }

    #[test]
    fn pack_with_zero_budget_keeps_nothing() {
        assert!(pack(vec![hit("A", "a", 10, 0.9)], 0).is_empty());
    }

    #[test]
    fn render_block_labels_each_chunk() {
        let block = render_block(&[hit("Refund Policy", "no refunds", 5, 0.9)]);
        assert!(block.contains("<knowledge>"));
        assert!(block.contains("[1] Refund Policy"));
        assert!(block.contains("no refunds"));
        // The hedge keeps a weak retrieval from dragging the answer off course.
        assert!(block.contains("ignore it when it does not"));
    }

    #[test]
    fn inject_appends_to_an_existing_system_message() {
        let mut body = json!({"messages": [
            {"role": "system", "content": "be helpful"},
            {"role": "user", "content": "q"}
        ]});
        assert!(inject(&mut body, "<knowledge>x</knowledge>", true));
        let sys = body["messages"][0]["content"].as_str().expect("system");
        assert!(sys.starts_with("be helpful"));
        assert!(sys.contains("<knowledge>x</knowledge>"));
        assert_eq!(body["messages"].as_array().expect("arr").len(), 2);
    }

    #[test]
    fn inject_appends_a_text_part_to_an_array_content_system_message() {
        // An array-content system message must not be treated as "no
        // content" and get a second system message inserted ahead of it —
        // that would silently demote the tenant's own system prompt and
        // several chat templates reject two system messages outright.
        let mut body = json!({"messages": [
            {"role": "system", "content": [{"type": "text", "text": "be helpful"}]},
            {"role": "user", "content": "q"}
        ]});
        assert!(inject(&mut body, "BLOCK", true));
        assert_eq!(
            body["messages"].as_array().expect("arr").len(),
            2,
            "still one system message, not two"
        );
        let parts = body["messages"][0]["content"]
            .as_array()
            .expect("array content");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["text"], "be helpful");
        assert_eq!(parts[1]["text"], "BLOCK");
    }

    #[test]
    fn inject_targets_the_latest_user_turn_even_with_array_content_when_system_is_unsupported() {
        // The retrieval query is drawn from the LATEST user turn, so the
        // block must attach there — not to an older turn that merely happens
        // to have string content while the latest turn has array content.
        let mut body = json!({"messages": [
            {"role": "user", "content": "older turn"},
            {"role": "assistant", "content": "..."},
            {"role": "user", "content": [{"type": "text", "text": "latest turn"}]}
        ]});
        assert!(inject(&mut body, "BLOCK", false));
        let parts = body["messages"][2]["content"]
            .as_array()
            .expect("array content");
        assert_eq!(parts[0]["text"], "BLOCK");
        assert_eq!(parts[1]["text"], "latest turn");
        assert_eq!(
            body["messages"][0]["content"], "older turn",
            "the older turn must be untouched"
        );
    }

    #[test]
    fn inject_creates_a_system_message_when_absent() {
        let mut body = json!({"messages": [{"role": "user", "content": "q"}]});
        assert!(inject(&mut body, "BLOCK", true));
        assert_eq!(body["messages"][0]["role"], "system");
        assert_eq!(body["messages"][1]["role"], "user");
    }

    #[test]
    fn inject_prepends_to_the_user_turn_when_system_is_unsupported() {
        let mut body = json!({"messages": [{"role": "user", "content": "q"}]});
        assert!(inject(&mut body, "BLOCK", false));
        assert_eq!(body["messages"].as_array().expect("arr").len(), 1);
        let user = body["messages"][0]["content"].as_str().expect("user");
        assert!(user.starts_with("BLOCK"));
        assert!(user.ends_with('q'));
    }

    #[test]
    fn budget_clamps_against_the_context_window() {
        // Injecting into a nearly-full context would turn a working request
        // into an upstream 400 — the boon causing the failure it must not cause.
        assert_eq!(available_budget(1500, 8192, 8000), 0);
        assert_eq!(available_budget(1500, 8192, 1000), 1500);
        // Room for 2192 after the reserve, so the configured ceiling applies.
        assert!(available_budget(4000, 8192, 5000) < 4000);
    }

    // ---- eligibility gate ----

    #[test]
    fn ineligible_without_the_boon_grant() {
        assert!(!eligible(&route(&[], 1), &on(), &key(false), true));
    }

    #[test]
    fn ineligible_without_attached_collections() {
        assert!(!eligible(
            &route(&["knowledge"], 0),
            &on(),
            &key(false),
            true
        ));
    }

    #[test]
    fn ineligible_when_globally_disabled() {
        let off = KnowledgeBoonSettings::default();
        assert!(!eligible(
            &route(&["knowledge"], 1),
            &off,
            &key(false),
            true
        ));
    }

    #[test]
    fn ineligible_for_internal_probe_keys() {
        assert!(!eligible(
            &route(&["knowledge"], 1),
            &on(),
            &key(true),
            true
        ));
    }

    #[test]
    fn ineligible_for_non_chat_requests() {
        assert!(!eligible(
            &route(&["knowledge"], 1),
            &on(),
            &key(false),
            false
        ));
    }

    #[test]
    fn eligible_when_everything_lines_up() {
        assert!(eligible(
            &route(&["knowledge"], 1),
            &on(),
            &key(false),
            true
        ));
    }

    // ---- fail-open: retrieval and injection must leave the body untouched
    // when nothing clears the bar ----

    #[test]
    fn injection_is_skipped_when_nothing_clears_the_threshold() {
        // pack() of an empty hit list must leave the body untouched. This
        // mirrors `apply`'s own guard: it returns the "miss" outcome (and
        // never calls inject) whenever `pack` comes back empty.
        let mut body = json!({"messages": [{"role": "user", "content": "q"}]});
        let before = body.clone();
        let packed = pack(Vec::new(), 1500);
        assert!(packed.is_empty());
        if !packed.is_empty() {
            inject(&mut body, &render_block(&packed), true);
        }
        assert_eq!(body, before, "body must be byte-identical");
    }

    #[test]
    fn injection_is_skipped_when_the_context_window_is_full() {
        let mut body = json!({"messages": [{"role": "user", "content": "q"}]});
        let before = body.clone();
        // 8000 estimated tokens against an 8192-token window leaves no room
        // once the output reserve is charged — available_budget clamps to 0.
        let budget = available_budget(1500, 8192, 8000);
        let packed = pack(vec![hit("A", "a", 100, 0.9)], budget);
        assert!(packed.is_empty(), "no room left in the context window");
        if !packed.is_empty() {
            inject(&mut body, &render_block(&packed), true);
        }
        assert_eq!(body, before);
    }

    #[test]
    fn inject_leaves_a_body_without_messages_untouched() {
        let mut body = json!({"prompt": "legacy completion"});
        let before = body.clone();
        assert!(!inject(&mut body, "BLOCK", true));
        assert_eq!(body, before);
    }

    #[test]
    fn retrieval_across_embedding_spaces_yields_nothing() {
        // A slab built with a 768-dim embedder must not match a 2-dim query:
        // `score_against` skips any chunk vector whose length differs from
        // the query's, so retrieval yields nothing rather than garbage
        // scores across incompatible embedding spaces.
        let slab = crate::knowledge::CollectionSlab {
            embedding_model: "embed-a".into(),
            dim: 768,
            version: 1,
            chunks: vec![crate::knowledge::SlabChunk {
                id: Uuid::new_v4(),
                title: "Doc".into(),
                text: "chunk".into(),
                token_count: 10,
                embedding: vec![1.0; 768],
            }],
        };
        assert!(slab.retrieve(&[1.0, 0.0], 5, -1.0).is_empty());
    }

    // ---- fold-in fix: a system message that is not messages[0] ----

    #[test]
    fn inject_finds_a_system_message_that_is_not_first() {
        // [user, system, user] is unusual but legal, and some clients emit
        // it. Only checking messages[0] would miss this system message and
        // fall through to inserting a SECOND one ahead of it, demoting the
        // tenant's own system prompt and risking a 400 from chat templates
        // that reject two system messages.
        let mut body = json!({"messages": [
            {"role": "user", "content": "first"},
            {"role": "system", "content": "be helpful"},
            {"role": "user", "content": "second"}
        ]});
        assert!(inject(&mut body, "BLOCK", true));
        let messages = body["messages"].as_array().expect("arr");
        assert_eq!(messages.len(), 3, "still exactly one system message");
        assert_eq!(messages[0]["role"], "user");
        assert_eq!(messages[1]["role"], "system");
        let sys = messages[1]["content"].as_str().expect("system content");
        assert!(sys.starts_with("be helpful"));
        assert!(sys.contains("BLOCK"));
        assert_eq!(messages[2]["role"], "user");
    }
}
