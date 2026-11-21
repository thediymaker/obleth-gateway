//! Billing for the work boons do on a request's behalf: each helper-model
//! call, generated image and search is a ledger row linked to the request it
//! served, and its spend lands in the tenant's and key's term-budget counters.

use obleth_config::{ResolvedKey, ResolvedModel, UsageRecord};
use serde_json::Value;
use uuid::Uuid;

use crate::proxy::UpstreamUsage;
use crate::state::AppState;

/// Label override for helper-call usage rows: synthetic tenants' helper calls
/// (vision, tool loop, guardrails, ...) are tagged `benchmark` just like their
/// main-path requests, so default usage/cost reads exclude them uniformly.
/// Everything else keeps the boon's own label.
fn helper_request_type<'a>(key: &ResolvedKey, label: &'a str) -> &'a str {
    if key.synthetic {
        obleth_config::BENCHMARK_REQUEST_TYPE
    } else {
        label
    }
}

/// Record a helper-model call against the tenant's ledger so the cost of the
/// boon is attributed and visible in the request log. `request_type` labels
/// the boon (e.g. `vision_boon`, `structured_output_boon`), unless `key` is a
/// synthetic tenant, in which case it is stamped `benchmark` instead (see
/// [`helper_request_type`]). `parent_request_id` is the client request the
/// call served; the row records it so the request log can link the two.
/// `usage.cached` is the part of the prompt the helper's upstream served from
/// its prefix cache.
pub(crate) fn bill_helper_call(
    state: &AppState,
    helper: &ResolvedModel,
    key: &ResolvedKey,
    session_id: &str,
    parent_request_id: Uuid,
    request_type: &str,
    usage: UpstreamUsage,
) {
    let UpstreamUsage {
        input: input_tokens,
        output: output_tokens,
        cached: cached_input_tokens,
    } = usage;
    let request_type = helper_request_type(key, request_type);
    let total_tokens = input_tokens.saturating_add(output_tokens);
    let cost_usd = (input_tokens as f64) * helper.input_cost_per_token
        + (output_tokens as f64) * helper.output_cost_per_token;

    state
        .metrics
        .record_request("boon", 200, input_tokens, output_tokens);
    // Internal probe keys are not billed; mirror `finalize`.
    if key.internal {
        return;
    }
    commit_helper_term_usage(state, key, total_tokens as i64, cost_usd);
    let mut record = helper_usage_record(
        helper,
        key,
        session_id,
        parent_request_id,
        request_type,
        input_tokens,
        output_tokens,
        cost_usd,
    );
    record.cached_input_tokens = cached_input_tokens;
    state.telemetry.record(record);
}

/// Helper-billing usage of a buffered completion: each count independently,
/// zero when absent, with the cached part of the prompt when it reports one.
pub(crate) fn reported_usage(completion: &Value) -> UpstreamUsage {
    let Some(usage) = completion.get("usage") else {
        return UpstreamUsage::default();
    };
    let read = |field: &str| usage.get(field).and_then(Value::as_u64).unwrap_or(0) as u32;
    UpstreamUsage::new(
        read("prompt_tokens"),
        read("completion_tokens"),
        crate::proxy::cached_prompt_tokens(usage) as u32,
    )
}

/// The ledger row for one helper call (see [`bill_helper_call`]). Split out so
/// the row's shape is unit-testable without telemetry.
#[allow(clippy::too_many_arguments)]
fn helper_usage_record(
    helper: &ResolvedModel,
    key: &ResolvedKey,
    session_id: &str,
    parent_request_id: Uuid,
    request_type: &str,
    input_tokens: u32,
    output_tokens: u32,
    cost_usd: f64,
) -> UsageRecord {
    let total_tokens = input_tokens.saturating_add(output_tokens);
    UsageRecord {
        request_id: Uuid::new_v4(),
        tenant_id: key.tenant_id,
        key_id: key.key_id,
        model: helper.model_name.clone(),
        admission: "boon".to_string(),
        weight: key.weight,
        input_tokens,
        output_tokens,
        cached_input_tokens: 0,
        estimated_tokens: total_tokens,
        queue_wait_ms: 0,
        ttft_ms: 0,
        total_ms: 0,
        status_code: 200,
        cache_status: "off".to_string(),
        cost_usd,
        // Helper calls record no duration (total_ms: 0), so slot-share energy
        // is zero here by construction; the main request's wall time covers
        // this hardware time at the main model's slot rate.
        energy_wh: 0.0,
        energy_cost_usd: 0.0,
        co2_g: 0.0,
        ts_ms: crate::tracer::now_ms(),
        session_id: session_id.to_string(),
        session_id_source: "none".to_string(),
        request_type: request_type.to_string(),
        device_id: String::new(),
        end_user: String::new(),
        parent_request_id,
        // A helper row is the helper model's own call; the variant the client
        // used is recorded on the request's row.
        model_variant: String::new(),
        reasoning_effort: String::new(),
        reasoning_effort_requested: String::new(),
        model_family: String::new(),
        key_name: key.key_name.clone(),
        tenant_name: key.tenant_name.clone(),
        streamed: false,
        error_class: String::new(),
        error_reason: String::new(),
        // A helper call is the gateway's own: it has no client.
        client_ip: String::new(),
        client_network: String::new(),
        client_country: String::new(),
        user_agent: String::new(),
    }
}

/// Record a boon-generated image against the tenant's ledger. Image models are
/// priced per image, not per token, so this is a sibling of
/// [`bill_helper_call`] rather than a call into it: cost comes from
/// `cost_per_image` and both token counts are zero. Synthetic tenants are
/// relabelled `benchmark` and internal probe keys are unbilled, exactly as
/// there.
pub(crate) fn bill_image_generation(
    state: &AppState,
    image_model: &ResolvedModel,
    key: &ResolvedKey,
    session_id: &str,
    parent_request_id: Uuid,
    images: u32,
) {
    let request_type = helper_request_type(key, "image_generation_boon");
    let cost_usd = images as f64 * image_model.cost_per_image;

    state.metrics.record_request("boon", 200, 0, 0);
    if key.internal {
        return;
    }
    commit_helper_term_usage(state, key, 0, cost_usd);
    state.telemetry.record(helper_usage_record(
        image_model,
        key,
        session_id,
        parent_request_id,
        request_type,
        0,
        0,
        cost_usd,
    ));
}

/// Record one search the web-search boon ran for a chat request. A search
/// carries no tokens and, like `POST /v1/search`, no price; the row exists so
/// the request's searches show up under it (`parent_request_id`) and in the
/// search tool's own usage, failures included (`status_code`, `total_ms`).
/// Synthetic tenants are relabelled `benchmark` and internal probe keys are
/// unrecorded, exactly as for the other helpers.
pub(crate) fn bill_web_search(
    state: &AppState,
    search_tool: &ResolvedModel,
    key: &ResolvedKey,
    session_id: &str,
    parent_request_id: Uuid,
    status_code: u16,
    total_ms: u32,
) {
    let request_type = helper_request_type(key, "web_search_boon");

    state.metrics.record_request("boon", status_code, 0, 0);
    if key.internal {
        return;
    }
    let mut record = helper_usage_record(
        search_tool,
        key,
        session_id,
        parent_request_id,
        request_type,
        0,
        0,
        0.0,
    );
    record.status_code = status_code;
    record.total_ms = total_ms;
    state.telemetry.record(record);
}

/// The term-budget counters one helper call must be added to: `(counter id,
/// period key)` for the tenant and for the key, each present only when that
/// scope has a cumulative cap. Period keys come from the same functions the
/// proxy's settle path uses, so helper spend lands in the very counters
/// admission checks.
fn helper_term_commits(
    key: &ResolvedKey,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<(Uuid, String)> {
    let mut commits = Vec::with_capacity(2);
    if let Some(period) = crate::proxy::term_period_key(key, now) {
        commits.push((key.tenant_id, period));
    }
    if let Some(period) = crate::proxy::key_term_period_key(key, now) {
        commits.push((key.key_id, period));
    }
    commits
}

/// Add helper spend to the tenant and key term-budget counters. Without this a
/// tenant at its hard cap keeps spending through vision, repair, guardrails,
/// tool-loop turns, and image generation, because only the main request was
/// ever committed. Detached so the helper's caller never waits on Redis, and
/// fail-open: a Redis error is logged and the spend stays in the ledger only.
fn commit_helper_term_usage(state: &AppState, key: &ResolvedKey, tokens: i64, cost_usd: f64) {
    if tokens <= 0 && cost_usd <= 0.0 {
        return;
    }
    let commits = helper_term_commits(key, chrono::Utc::now());
    if commits.is_empty() {
        return;
    }
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let redis = state.redis.clone();
    runtime.spawn(async move {
        apply_term_commits(&redis, &commits, tokens, cost_usd).await;
    });
}

async fn apply_term_commits(
    redis: &obleth_redis::RedisStore,
    commits: &[(Uuid, String)],
    tokens: i64,
    cost_usd: f64,
) {
    for (id, period) in commits {
        if let Err(e) = redis.term_usage_add(id, period, tokens, cost_usd).await {
            tracing::warn!(error = %e, counter = %id, "helper term usage commit failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{test_key_with_policy, test_route};
    use super::*;

    #[test]
    fn a_helper_row_names_the_request_it_served() {
        let mut helper = test_route();
        helper.model_name = "drafter".into();
        let key = test_key_with_policy(None);
        let parent = Uuid::new_v4();
        let row = helper_usage_record(
            &helper,
            &key,
            "session-1",
            parent,
            "speculation_draft",
            700,
            112,
            0.0004,
        );
        assert_eq!(row.parent_request_id, parent);
        // Its own id, the helper model's name, and no variant: the variant the
        // client used is on the request's own row.
        assert_ne!(row.request_id, parent);
        assert_eq!(row.model, "drafter");
        assert_eq!(row.model_variant, "");
        assert_eq!(row.admission, "boon");
        assert_eq!(row.request_type, "speculation_draft");
        assert_eq!(row.estimated_tokens, 812);
        assert_eq!(row.session_id, "session-1");
    }

    #[test]
    fn every_helper_bill_records_its_parent_request() {
        // `bill_helper_call` and `bill_image_generation` need a live AppState;
        // pin that both build their row with the parent's id.
        let src = include_str!("billing.rs");
        for f in [
            "pub(crate) fn bill_helper_call(",
            "pub(crate) fn bill_image_generation(",
            "pub(crate) fn bill_web_search(",
        ] {
            let start = src.find(f).expect(f);
            let end = start + src[start..].find("\n}\n").expect("end of fn");
            let body: String = src[start..end].split_whitespace().collect();
            assert!(
                body.contains("helper_usage_record(")
                    && body.contains("parent_request_id,request_type"),
                "{f} must record the parent request"
            );
        }
    }

    #[test]
    fn helper_request_type_tags_synthetic_tenants_as_benchmark() {
        let mut key = test_key_with_policy(None);

        assert_eq!(helper_request_type(&key, "vision_boon"), "vision_boon");

        key.synthetic = true;
        assert_eq!(
            helper_request_type(&key, "vision_boon"),
            obleth_config::BENCHMARK_REQUEST_TYPE
        );
    }

    #[test]
    fn helper_spend_targets_both_capped_term_counters() {
        let now = chrono::Utc::now();
        let mut key = test_key_with_policy(None);
        key.tenant_id = Uuid::from_u128(1);
        key.key_id = Uuid::from_u128(2);
        assert!(
            helper_term_commits(&key, now).is_empty(),
            "no cumulative cap anywhere, nothing to commit"
        );

        key.budget_cost_usd = Some(10.0);
        key.key_budget_tokens = Some(1_000);
        key.key_budget_period = Some("monthly".into());
        let commits = helper_term_commits(&key, now);
        assert_eq!(
            commits,
            vec![
                (
                    key.tenant_id,
                    crate::proxy::term_period_key(&key, now).unwrap()
                ),
                (
                    key.key_id,
                    crate::proxy::key_term_period_key(&key, now).unwrap()
                ),
            ]
        );
    }

    /// Integration test; runs only when `OBLETH_TEST_REDIS_URL` is set.
    #[tokio::test]
    async fn helper_spend_lands_in_the_term_counters_admission_reads() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let redis = obleth_redis::RedisStore::connect(&url).await.unwrap();
        let tenant = Uuid::new_v4();
        let key_id = Uuid::new_v4();
        let commits = vec![(tenant, "l:0".to_string()), (key_id, "l:0".to_string())];
        apply_term_commits(&redis, &commits, 120, 0.5).await;
        apply_term_commits(&redis, &commits, 30, 0.25).await;
        for id in [tenant, key_id] {
            let (tokens, cost) = redis.term_usage_peek(&id, "l:0").await.unwrap();
            assert_eq!(tokens, 150);
            assert!((cost - 0.75).abs() < 1e-9, "cost {cost}");
        }
    }
}
