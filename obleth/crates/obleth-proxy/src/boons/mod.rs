//! Model **Boons**: gateway-side capabilities granted to models that lack them
//! natively.
//!
//! Current boons:
//! - **vision** ([`vision`]): relays `image_url` content parts to a designated
//!   describer model and swaps each image for its text description, so a
//!   text-only model can answer as if it had vision.
//! - **structured_output** ([`structured`]): enforces `response_format` JSON
//!   schemas. The schema is rendered into the prompt; the reply is validated at
//!   the gateway and repaired via a configurable fixer model when it fails.
//!
//! Vision rewrites only the request. Structured output additionally rewrites the
//! **response**: when it arms a [`ResponsePlan`], the proxy forces a
//! non-streaming upstream call, buffers the completion, applies the transform in
//! [`respond`], and (for streaming clients) re-emits the result as synthesized
//! SSE. The gateway tool loop also arms a [`ResponsePlan`]; streaming clients are
//! driven live by [`tool_stream`] instead.
//!
//! Boons are deliberately **fail-open**: any error (no helper configured,
//! upstream failure, timeout, unparseable reply) leaves the request/response
//! unchanged. A flaky helper must never block or fail a request the target
//! model might still handle on its own.
//!
//! The engine is hot-swappable: [`BoonEngine`] holds its [`BoonSettings`] behind
//! an [`ArcSwap`] that the periodic model-registry refresh task updates, exactly
//! like [`crate::classifier::Classifier`].

pub(crate) mod compression;
pub(crate) mod guardrails;
pub mod mcp_tools;
pub mod respond;
pub mod structured;
pub mod tool_loop;
pub mod tool_stream;
mod vision;

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use obleth_config::{
    BoonSettings, ResolvedKey, ResolvedModel, StructuredOutputBoonSettings, UsageRecord,
};
use serde_json::Value;
use uuid::Uuid;

use crate::state::AppState;

/// Phase A/B1 gating: the model was granted the boon AND it is globally active
/// AND this is a chat completion AND the key's tenant has not opted out. Internal
/// probe keys are exempt (mirrors guardrails). A `None` tenant policy follows the
/// global default (eligible).
fn compression_eligible(
    route: &obleth_config::ResolvedModel,
    settings: &obleth_config::BoonSettings,
    key: &obleth_config::ResolvedKey,
    is_chat: bool,
) -> bool {
    let tenant_opted_in = key
        .compression_policy
        .as_ref()
        .is_none_or(|p| p.enabled);
    is_chat
        && !key.internal
        && settings.compression.active()
        && tenant_opted_in
        && route.boons.iter().any(|b| b == "compression")
}

/// Request header that disables boon processing for a single request when set
/// to `off`. Also returned on responses listing the boons that were applied.
pub const BOONS_HEADER: &str = "x-obleth-boons";
/// Response header carrying a non-fatal boon warning (e.g. structured-output
/// validation failed and the original completion passed through).
pub const BOONS_WARNING_HEADER: &str = "x-obleth-boons-warning";

/// Hot-swappable boon configuration shared across the data plane.
#[derive(Clone)]
pub struct BoonEngine {
    settings: Arc<ArcSwap<BoonSettings>>,
}

/// What `enrich_request` did to the request, and whether the response must be
/// intercepted and transformed before it reaches the client.
#[derive(Default)]
pub struct EnrichOutcome {
    /// The body was rewritten and the caller must re-serialize it.
    pub rewritten: bool,
    /// Names of the boons that acted on this request (for the response header).
    pub applied: Vec<&'static str>,
    /// When set, the proxy must buffer the upstream response and run it through
    /// [`respond::transform_completion`] before replying to the client.
    pub response_plan: Option<ResponsePlan>,
    /// Set by the guardrails boon when a scanner rejects the request. The proxy
    /// must return `block.status` immediately when this is Some.
    pub blocked: Option<guardrails::GuardrailsBlock>,
}

/// Response-side work armed by request enrichment. Captures everything the
/// transform needs that is no longer derivable once the request body has been
/// rewritten (original `stream` flag, tool names, the JSON schema).
pub struct ResponsePlan {
    pub structured: Option<StructuredPlan>,
    /// The gateway tool loop: granted MCP tools were injected and the gateway
    /// executes the model's tool calls itself.
    pub tool_loop: Option<tool_loop::ToolLoopPlan>,
    /// The client asked for `stream: true`; the transformed completion must be
    /// re-emitted as synthesized SSE.
    pub client_stream: bool,
    /// The client asked for `stream_options.include_usage`.
    pub include_usage: bool,
    /// Output-side guardrails plan (block/redact actions only; log_only is
    /// handled async in proxy.rs after the stream drains).
    pub guardrails: Option<GuardrailsOutputPlan>,
}

/// Output-side guardrails: run scanners on the buffered completion.
pub struct GuardrailsOutputPlan {
    pub policy: obleth_config::GuardrailsPolicy,
    pub settings: obleth_config::GuardrailsBoonSettings,
}

/// Structured-output-boon response work: validate (and repair) the completion
/// against the requested schema.
pub struct StructuredPlan {
    /// The JSON schema from `response_format.json_schema.schema`, or `None`
    /// for `json_object` (syntactic JSON check only).
    pub schema: Option<Value>,
    /// Settings snapshot taken at request time so a hot-reload mid-request
    /// cannot change repair behavior.
    pub settings: StructuredOutputBoonSettings,
}

impl BoonEngine {
    pub fn new(initial: BoonSettings) -> Self {
        Self {
            settings: Arc::new(ArcSwap::from_pointee(initial)),
        }
    }

    /// Current settings snapshot (cheap `Arc` clone).
    pub fn settings(&self) -> Arc<BoonSettings> {
        self.settings.load_full()
    }

    /// Replace the settings (called by the periodic refresh task).
    pub fn update(&self, settings: BoonSettings) {
        self.settings.store(Arc::new(settings));
    }

    /// Apply every applicable boon to `json` in place before the request is
    /// dispatched upstream, and report whether the response must be
    /// intercepted.
    ///
    /// `opt_out` is the per-request `x-obleth-boons: off` escape hatch;
    /// `is_chat` restricts the tools/structured boons to chat completions.
    #[allow(clippy::too_many_arguments)]
    pub async fn enrich_request(
        &self,
        state: &AppState,
        route: Option<&ResolvedModel>,
        key: &ResolvedKey,
        session_id: &str,
        opt_out: bool,
        is_chat: bool,
        json: &mut Value,
        mut tracer: Option<&mut crate::tracer::SpanRecorder>,
    ) -> EnrichOutcome {
        let mut outcome = EnrichOutcome::default();
        if opt_out {
            return outcome;
        }
        let settings = self.settings();
        let Some(route) = route else {
            return outcome;
        };

        // ---- vision boon ----
        // Only when the model opted into the vision boon, lacks native vision,
        // and a describer is configured. Models that natively accept images,
        // or that haven't enabled the boon, are left untouched.
        if route.boons.iter().any(|b| b == "vision")
            && !route.supports_vision
            && settings.vision.active()
        {
            let vision_start = crate::tracer::now_ms();
            let images_described =
                vision::apply(state, &settings.vision, key, session_id, json).await;
            if let Some(t) = tracer.as_deref_mut() {
                t.record_elapsed(
                    "boon:vision",
                    "proxy_request",
                    vision_start,
                    "ok",
                    serde_json::json!({
                        "images": images_described,
                        "describer": settings.vision.fallback_model.as_deref().unwrap_or(""),
                    }),
                );
            }
            if images_described > 0 {
                outcome.rewritten = true;
                outcome.applied.push("vision");
            }
        }

        // The tools/structured boons and the tool loop only make sense for
        // chat completions.
        if !is_chat {
            return outcome;
        }

        // ---- compression boon (lossless, Phase A) ----
        // `is_chat` is always true here (past the earlier `if !is_chat` guard);
        // the parameter exists so unit tests can exercise the ineligible path.
        if compression_eligible(route, &settings, key, is_chat) {
            let comp_start = crate::tracer::now_ms();
            let stats = compression::apply(&settings.compression, json);
            if stats.compressed > 0 {
                outcome.rewritten = true;
                outcome.applied.push("compression");
                state
                    .metrics
                    .record_compression_saved(stats.tokens_before.saturating_sub(stats.tokens_after));
            }
            if let Some(t) = tracer.as_deref_mut() {
                t.record_elapsed(
                    "boon:compression",
                    "proxy_request",
                    comp_start,
                    "ok",
                    serde_json::json!({
                        "scanned": stats.scanned,
                        "compressed": stats.compressed,
                        "tokens_before": stats.tokens_before,
                        "tokens_after": stats.tokens_after,
                    }),
                );
            }
        }

        // Capture the client's streaming intent before any rewrite; the proxy
        // forces `stream: false` upstream when a response plan is armed.
        let client_stream = json
            .get("stream")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let include_usage = json
            .pointer("/stream_options/include_usage")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let mut structured_plan: Option<StructuredPlan> = None;

        // ---- gateway tool loop: inject granted MCP-server tools ----
        // Only models with native function calling get the loop. Clients that
        // send their own `tools` (agentic clients like an IDE assistant) still
        // get the granted MCP tools *merged* into their set — a model that an
        // operator granted a tool always sees it. The gateway executes only its
        // own MCP tools and hands any client-owned tool call straight back to
        // the client.
        let client_sent_tools = json
            .get("tools")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty());
        let mut tool_loop_servers: Option<std::collections::HashMap<String, String>> = None;
        if settings.tool_loop.active() && !route.tool_servers.is_empty() {
            if route.supports_function_calling {
                // The nudge tells a plain chat client's model that it has tools
                // and when to call them. Agentic clients that brought their own
                // `tools` already steer tool use, so we merge the granted tools
                // in but leave their prompt alone.
                let nudge = (!client_sent_tools).then(|| settings.tool_loop.nudge.as_str());
                tool_loop_servers = tool_loop::inject(state, route, nudge, json).await;
                if tool_loop_servers.is_some() {
                    outcome.rewritten = true;
                    outcome.applied.push("tool_loop");
                }
            } else {
                // Granted tools but no native function calling: the loop is
                // skipped and the model silently gets no tools (it will often
                // claim it cannot search). Surface the misconfiguration loudly.
                tracing::warn!(
                    model = %route.model_name,
                    servers = ?route.tool_servers,
                    "model is granted MCP tool servers but is not flagged \
                     supports_function_calling; no tools will be injected. \
                     Enable function calling on this model to use the tool loop."
                );
            }
        }

        // ---- structured-output boon ----
        if route.boons.iter().any(|b| b == "structured_output")
            && !route.supports_response_schema
            && settings.structured_output.active()
        {
            let structured_start = crate::tracer::now_ms();
            if let Some(plan) = structured::apply(json, route.supports_system_messages) {
                if let Some(t) = tracer.as_deref_mut() {
                    t.record_elapsed(
                        "boon:structured_repair",
                        "proxy_request",
                        structured_start,
                        "ok",
                        serde_json::json!({}),
                    );
                }
                outcome.rewritten = true;
                outcome.applied.push("structured_output");
                structured_plan = Some(StructuredPlan {
                    schema: plan,
                    settings: settings.structured_output.clone(),
                });
            }
        }

        // The tool loop captures the fully enriched request body so follow-up
        // turns re-dispatch with identical sampling parameters; its own
        // dispatches are always non-streaming.
        let tool_loop_plan = tool_loop_servers.map(|servers| {
            let mut request = json.clone();
            if let Some(obj) = request.as_object_mut() {
                obj.insert("stream".into(), Value::Bool(false));
                obj.remove("stream_options");
            }
            tool_loop::ToolLoopPlan {
                tool_servers: servers,
                request,
                settings: settings.tool_loop.clone(),
                passthrough_unmapped: client_sent_tools,
            }
        });

        // ---- guardrails boon (input scanning) ----
        // Guardrails are enabled per-tenant by the presence of a policy; there
        // is no global master switch. Internal probe keys are always exempt.
        if !key.internal {
            if let Some(policy) = &key.guardrails_policy {
                let guard_outcome = guardrails::apply_input(
                    state,
                    &settings.guardrails,
                    policy,
                    key,
                    session_id,
                    json,
                    tracer,
                )
                .await;
                if let Some(reason) = guard_outcome.blocked {
                    outcome.blocked = Some(reason);
                    return outcome;
                }
                if guard_outcome.sanitized {
                    outcome.rewritten = true;
                    outcome.applied.push("guardrails_input");
                }
                // Arm output plan for block/redact actions.
                // log_only output scanning is handled async in proxy.rs after the stream drains.
                if !policy.output_scanners.is_empty()
                    && !matches!(policy.action, obleth_config::GuardrailsAction::LogOnly)
                {
                    let plan = outcome.response_plan.get_or_insert_with(|| ResponsePlan {
                        structured: None,
                        tool_loop: None,
                        client_stream: json
                            .get("stream")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        include_usage: json
                            .pointer("/stream_options/include_usage")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        guardrails: None,
                    });
                    plan.guardrails = Some(GuardrailsOutputPlan {
                        policy: policy.clone(),
                        settings: settings.guardrails.clone(),
                    });
                }
            }
        }

        if structured_plan.is_some() || tool_loop_plan.is_some() {
            let existing_guardrails = outcome
                .response_plan
                .as_mut()
                .and_then(|p| p.guardrails.take());
            outcome.response_plan = Some(ResponsePlan {
                structured: structured_plan,
                tool_loop: tool_loop_plan,
                client_stream,
                include_usage,
                guardrails: existing_guardrails,
            });
        }
        outcome
    }
}

/// Result of one helper-model chat call: the reply text plus the token usage
/// the helper reported (zero when it did not return a `usage` object).
pub(crate) struct ChatCallResult {
    pub text: String,
    pub input_tokens: u32,
    pub output_tokens: u32,
}

/// Send a chat-completions request to a helper model (describer, fixer) and
/// return its reply text, bounded by `timeout`.
pub(crate) async fn chat_call(
    state: &AppState,
    helper: &ResolvedModel,
    body: Value,
    timeout: Duration,
) -> anyhow::Result<ChatCallResult> {
    let fut = async {
        let url = build_chat_url(&helper.api_base);
        let mut req = state.http.post(url).json(&body);
        if let Some(api_key) = &helper.api_key {
            req = req.bearer_auth(api_key);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("helper upstream returned {}", resp.status());
        }
        let body: Value = resp.json().await?;
        let text = body
            .pointer("/choices/0/message/content")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        if text.trim().is_empty() {
            anyhow::bail!("helper returned an empty reply");
        }
        let input_tokens = body
            .pointer("/usage/prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let output_tokens = body
            .pointer("/usage/completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        Ok(ChatCallResult {
            text,
            input_tokens,
            output_tokens,
        })
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!("helper call timed out after {timeout:?}"),
    }
}

/// Send a chat-completions request to a model and return the **complete**
/// completion JSON, bounded by `timeout`. Used by the gateway tool loop, which
/// needs the full message (including `tool_calls`) and usage, not just text.
pub(crate) async fn chat_call_completion(
    state: &AppState,
    model: &ResolvedModel,
    body: Value,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let fut = async {
        let url = build_chat_url(&model.api_base);
        let mut req = state.http.post(url).json(&body);
        if let Some(api_key) = &model.api_key {
            req = req.bearer_auth(api_key);
        }
        let resp = req.send().await?;
        if !resp.status().is_success() {
            anyhow::bail!("upstream returned {}", resp.status());
        }
        Ok(resp.json::<Value>().await?)
    };
    match tokio::time::timeout(timeout, fut).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!("chat call timed out after {timeout:?}"),
    }
}

/// Record a helper-model call against the tenant's ledger so the cost of the
/// boon is attributed and visible in the request log. `request_type` labels
/// the boon (e.g. `vision_boon`, `structured_output_boon`).
pub(crate) fn bill_helper_call(
    state: &AppState,
    helper: &ResolvedModel,
    key: &ResolvedKey,
    session_id: &str,
    request_type: &str,
    input_tokens: u32,
    output_tokens: u32,
) {
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
    state.telemetry.record(UsageRecord {
        request_id: Uuid::new_v4(),
        tenant_id: key.tenant_id,
        key_id: key.key_id,
        model: helper.model_name.clone(),
        admission: "boon".to_string(),
        weight: key.weight,
        input_tokens,
        output_tokens,
        estimated_tokens: total_tokens,
        queue_wait_ms: 0,
        ttft_ms: 0,
        total_ms: 0,
        status_code: 200,
        cache_status: "off".to_string(),
        cost_usd,
        ts_ms: now_ms(),
        session_id: session_id.to_string(),
        session_id_source: "none".to_string(),
        request_type: request_type.to_string(),
    });
}

pub(crate) fn build_chat_url(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    format!("{base}/chat/completions")
}

pub(crate) fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_route() -> obleth_config::ResolvedModel {
        obleth_config::ResolvedModel {
            model_name: "test".to_string(),
            upstream_model: "test".to_string(),
            api_base: "http://localhost".to_string(),
            api_key: None,
            model_type: "chat".to_string(),
            admission_weight: 1,
            max_in_flight: None,
            enabled: true,
            cache_enabled: false,
            cache_ttl_secs: 0,
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            cost_per_image: 0.0,
            cost_per_audio_second: 0.0,
            cost_per_character: 0.0,
            context_window: 0,
            supports_function_calling: false,
            supports_system_messages: false,
            supports_response_schema: false,
            supports_tool_choice: false,
            supports_vision: false,
            tags: vec![],
            boons: vec![],
            tool_servers: vec![],
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: 200,
            endpoint_selection_mode: "failover".to_string(),
            debug_diagnostics: false,
            endpoints: vec![],
        }
    }

    #[test]
    fn compression_eligible_requires_grant_active_chat_and_tenant_optin() {
        use obleth_config::{BoonSettings, CompressionBoonSettings, CompressionPolicy};

        fn test_key() -> obleth_config::ResolvedKey {
            // Build via the redis crate's test-shaped literal is not accessible here;
            // construct the minimal key inline.
            obleth_config::ResolvedKey {
                key_id: uuid::Uuid::nil(),
                tenant_id: uuid::Uuid::nil(),
                tenant_name: "t".into(),
                fairshare_group: "default".into(),
                group_weight: 100,
                weight: 1,
                tokens_per_minute: 0,
                max_in_flight: None,
                disabled: false,
                status: "active".into(),
                timezone: "UTC".into(),
                active_from: None,
                active_until: None,
                weekly_windows: None,
                budget_tokens: None,
                budget_cost_usd: None,
                budget_period: None,
                budget_started_at: None,
                key_budget_tokens: None,
                key_budget_cost_usd: None,
                key_budget_period: None,
                key_budget_started_at: None,
                allowed_models: None,
                internal: false,
                tracing_enabled: false,
                guardrails_policy: None,
                compression_policy: None,
            }
        }

        let mut settings = BoonSettings::default();
        settings.compression = CompressionBoonSettings { enabled: true, ..Default::default() };

        let mut route = test_route();
        route.boons = vec!["compression".to_string()];
        let mut key = test_key();

        // Granted + active + chat + no tenant policy -> eligible.
        assert!(compression_eligible(&route, &settings, &key, true));

        // Not chat -> ineligible.
        assert!(!compression_eligible(&route, &settings, &key, false));

        // No grant -> ineligible.
        route.boons.clear();
        assert!(!compression_eligible(&route, &settings, &key, true));
        route.boons = vec!["compression".to_string()];

        // Global master switch off -> ineligible.
        settings.compression.enabled = false;
        assert!(!compression_eligible(&route, &settings, &key, true));
        settings.compression.enabled = true;

        // Internal probe keys are exempt.
        key.internal = true;
        assert!(!compression_eligible(&route, &settings, &key, true));
        key.internal = false;

        // Tenant opt-out policy (enabled = false) -> ineligible.
        key.compression_policy = Some(CompressionPolicy { enabled: false, allow_lossy: false });
        assert!(!compression_eligible(&route, &settings, &key, true));

        // Tenant policy enabled -> eligible again.
        key.compression_policy = Some(CompressionPolicy { enabled: true, allow_lossy: false });
        assert!(compression_eligible(&route, &settings, &key, true));
    }

    #[test]
    fn chat_url_is_normalised() {
        assert_eq!(
            build_chat_url("http://host:8080/v1/"),
            "http://host:8080/v1/chat/completions"
        );
        assert_eq!(
            build_chat_url("http://host:8080/v1"),
            "http://host:8080/v1/chat/completions"
        );
    }
}
