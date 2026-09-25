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
//! - **image_generation** ([`image_gen`]): injects a gateway-executed
//!   `generate_image` tool so a chat model can produce images through a
//!   registered image model. The tool returns a text receipt; the image itself
//!   is attached to the final assistant message out of band.
//! - **speculation** ([`speculation`]): answers with a fast drafter model
//!   whenever the target model itself verifies the draft (one prompt_logprobs
//!   prefill scores every draft token); unverified drafts fall through to the
//!   target. Runs pre-dispatch in the proxy, not through [`ResponsePlan`].
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

pub(crate) mod code_prune;
pub(crate) mod compression;
#[cfg(test)]
mod compression_verify;
pub(crate) mod compressor;
pub(crate) mod drain;
pub(crate) mod embedded_json;
pub(crate) mod guardrails;
pub(crate) mod image_gen;
pub(crate) mod knowledge;
pub mod mcp_tools;
pub mod respond;
pub(crate) mod speculation;
pub(crate) mod structural_json;
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
use obleth_tokenizer::Tokenizer;
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
    let tenant_opted_in = key.compression_policy.as_ref().is_none_or(|p| p.enabled);
    is_chat
        && !key.internal
        && settings.compression.active()
        && tenant_opted_in
        && route.boons.iter().any(|b| b == "compression")
}

/// Base gate for the opt-in compression passes: chat + global active + boon
/// granted + not an internal key. No reversibility requirement (lossy/dedup are
/// deterministic and run on any model; retrieve_original is a bonus).
fn opt_in_compression_base(
    route: &obleth_config::ResolvedModel,
    settings: &obleth_config::BoonSettings,
    key: &obleth_config::ResolvedKey,
) -> bool {
    settings.compression.active() && route.boons.iter().any(|b| b == "compression") && !key.internal
}

/// Cross-turn dedup is eligible when the base holds and the `dedup` piece is on.
/// A per-tenant policy overrides the global `dedup` default; a tenant with no
/// policy inherits it.
fn dedup_eligible(
    route: &obleth_config::ResolvedModel,
    settings: &obleth_config::BoonSettings,
    key: &obleth_config::ResolvedKey,
) -> bool {
    opt_in_compression_base(route, settings, key)
        && match &key.compression_policy {
            Some(p) => p.dedup,
            None => settings.compression.dedup,
        }
}

/// Near-lossless log template-collapse is eligible when the base holds and the
/// `compact_logs` piece is on. A per-tenant policy overrides the global
/// `compact_logs` default; a tenant with no policy inherits it. Independent of
/// `allow_lossy`.
fn log_compaction_eligible(
    route: &obleth_config::ResolvedModel,
    settings: &obleth_config::BoonSettings,
    key: &obleth_config::ResolvedKey,
) -> bool {
    opt_in_compression_base(route, settings, key)
        && match &key.compression_policy {
            Some(p) => p.compact_logs,
            None => settings.compression.compact_logs,
        }
}

/// Lossy semantic compression is eligible when the base holds and the
/// `allow_lossy` piece is on. A per-tenant policy overrides the global
/// `allow_lossy` default; a tenant with no policy inherits it. `force` is the
/// per-request `x-obleth-boons: lossy` override — it turns the lossy pass on for
/// this request regardless of the toggle, but the base gate (boon granted, not
/// internal, globally active) still applies.
fn lossy_eligible(
    route: &obleth_config::ResolvedModel,
    settings: &obleth_config::BoonSettings,
    key: &obleth_config::ResolvedKey,
    force: bool,
) -> bool {
    opt_in_compression_base(route, settings, key)
        && (force
            || match &key.compression_policy {
                Some(p) => p.allow_lossy,
                None => settings.compression.allow_lossy,
            })
}

/// Per-tenant code-compaction toggle with the global setting as the no-policy
/// default: a tenant policy overrides the global `code_compaction` flag.
fn effective_code_compaction(
    settings: &obleth_config::BoonSettings,
    key: &obleth_config::ResolvedKey,
) -> bool {
    match &key.compression_policy {
        Some(policy) => policy.code_compaction,
        None => settings.compression.code_compaction,
    }
}

/// The image-generation boon is eligible when the model was granted it, the
/// boon is globally active (enabled with an image model configured), and the
/// model can actually call functions. Unlike the compression boon there is no
/// tenant opt-out: an operator granting the boon is the opt-in.
fn image_gen_eligible(
    route: &obleth_config::ResolvedModel,
    settings: &obleth_config::BoonSettings,
) -> bool {
    route.boons.iter().any(|b| b == "image_generation")
        && settings.image_generation.active()
        && route.supports_function_calling
}

/// The speculation boon is eligible when the model was granted it, the boon is
/// globally active (enabled with a drafter and a verifier configured), the key
/// is not an internal probe (probes must measure the target model itself), and
/// the drafter is not the target. Request-body eligibility (bypass params,
/// multimodal content) is decided separately by
/// [`speculation::eligible_messages`].
fn speculation_eligible(
    route: &obleth_config::ResolvedModel,
    settings: &obleth_config::BoonSettings,
    key: &obleth_config::ResolvedKey,
) -> bool {
    if key.internal
        || !settings.speculation.active()
        || !route.boons.iter().any(|b| b == "speculation")
    {
        return false;
    }
    // Both helpers resolve per target: its own drafter (fleet default as
    // fallback) and its own scoring endpoint. Missing either means the model
    // cannot speculate, decided here before anything is armed.
    if speculation::scoring_base(route, &settings.speculation.verify_url_template).is_empty() {
        return false;
    }
    let drafter = speculation::effective_draft_model(route, &settings.speculation);
    !drafter.is_empty() && drafter != route.model_name
}

/// Whether a response plan leaves room for the speculation boon.
///
/// `None` always does. A plan built only by the image-generation boon does
/// too: that boon arms the tool loop on every request to a model granted it,
/// so treating its presence as "something else owns the response" is what made
/// the two boons silently exclusive. Speculation abstains by falling through,
/// so the tool loop still answers whenever the cascade declines.
///
/// Nothing else yields. Structured output and output guardrails must see the
/// target's own answer, and a real MCP tool loop — more than the one synthetic
/// image entry, or a client passing its own tools through — is the model
/// actually being given tools to use.
fn plan_yields_to_speculation(plan: Option<&ResponsePlan>, image_tool_pending: bool) -> bool {
    let Some(plan) = plan else {
        return true;
    };
    if plan.structured.is_some() || plan.guardrails.is_some() || !image_tool_pending {
        return false;
    }
    plan.tool_loop
        .as_ref()
        .is_some_and(|t| t.tool_servers.len() == 1 && !t.passthrough_unmapped)
}

/// Per-request boon control header (comma-separated tokens), also echoed on
/// responses listing the boons that were applied. Recognized request tokens:
/// - `off`   — disable ALL boon processing for this request (wins over others).
/// - `lossy` — force the compression boon's lossy prose pass ON for this request
///   even when the tenant/global `allow_lossy` toggle is off. The model must
///   still have the `compression` boon granted. Intended for back-to-back A/B
///   testing (compare a request with and without lossy compression).
pub const BOONS_HEADER: &str = "x-obleth-boons";
/// Response header carrying a non-fatal boon warning (e.g. structured-output
/// validation failed and the original completion passed through).
pub const BOONS_WARNING_HEADER: &str = "x-obleth-boons-warning";
/// Response header summarizing what the compression boon did, for cheap
/// back-to-back comparison without reading traces:
/// `before=<tokens>;after=<tokens>;saved=<tokens>`. Emitted only when the
/// compression boon ran on the request.
pub const COMPRESSION_HEADER: &str = "x-obleth-compression";

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
    /// Combined compression token totals `(before, after)` across all passes,
    /// for the `x-obleth-compression` response header. `Some` only when the
    /// compression boon actually ran (saw tokens) on this request.
    pub compression_tokens: Option<(u32, u32)>,
    /// Armed by the speculation boon. Consumed by the proxy BEFORE upstream
    /// dispatch (unlike `response_plan`): a committed cascade returns the
    /// response itself, and an abstaining one falls through to the completely
    /// normal dispatch path. May be armed alongside a `response_plan` when
    /// that plan is only the image boon's tool loop (see
    /// [`plan_yields_to_speculation`]) — the plan must stay intact, because it
    /// answers the request whenever the cascade abstains.
    pub speculation: Option<speculation::SpeculationPlan>,
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

/// Tenant input guardrails for one request, recorded into `outcome`. Returns
/// `true` when the request is blocked. Internal probe keys are exempt, and a
/// tenant without a policy is a no-op.
///
/// Output scanning (block/redact actions) is armed only for chat: it reads
/// chat-completion message content, the one response shape it understands.
/// `log_only` output scanning is handled in proxy.rs after the stream drains.
#[allow(clippy::too_many_arguments)]
async fn guard_input(
    state: &AppState,
    settings: &BoonSettings,
    key: &ResolvedKey,
    session_id: &str,
    is_chat: bool,
    json: &mut Value,
    tracer: Option<&mut crate::tracer::SpanRecorder>,
    outcome: &mut EnrichOutcome,
) -> bool {
    if key.internal {
        return false;
    }
    let Some(policy) = &key.guardrails_policy else {
        return false;
    };
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
    if let Some(block) = guard_outcome.blocked {
        outcome.blocked = Some(block);
        return true;
    }
    if guard_outcome.sanitized {
        outcome.rewritten = true;
        outcome.applied.push("guardrails_input");
    }
    if is_chat
        && !policy.output_scanners.is_empty()
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
    false
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

    /// Scan a chat-shaped body with the tenant's input guardrails outside the
    /// passthrough pipeline (the verdicts endpoint). `Err` carries the block
    /// to return; `Ok(true)` means redaction rewrote `json` in place. Output
    /// scanning is not armed: callers that use this read logprobs, not text.
    pub async fn scan_input(
        &self,
        state: &AppState,
        key: &ResolvedKey,
        session_id: &str,
        json: &mut Value,
        tracer: Option<&mut crate::tracer::SpanRecorder>,
    ) -> Result<bool, guardrails::GuardrailsBlock> {
        let Some(policy) = key.guardrails_policy.as_ref().filter(|_| !key.internal) else {
            return Ok(false);
        };
        let settings = self.settings();
        let out = guardrails::apply_input(
            state,
            &settings.guardrails,
            policy,
            key,
            session_id,
            json,
            tracer,
        )
        .await;
        match out.blocked {
            Some(block) => Err(block),
            None => Ok(out.sanitized),
        }
    }

    /// Apply every applicable boon to `json` in place before the request is
    /// dispatched upstream, and report whether the response must be
    /// intercepted.
    ///
    /// `opt_out` is the per-request `x-obleth-boons: off` escape hatch;
    /// `force_lossy` is the per-request `x-obleth-boons: lossy` override that
    /// turns the lossy compression pass on for this request; `is_chat` restricts
    /// the tools/structured boons to chat completions.
    #[allow(clippy::too_many_arguments)]
    pub async fn enrich_request(
        &self,
        state: &AppState,
        route: Option<&ResolvedModel>,
        key: &ResolvedKey,
        session_id: &str,
        opt_out: bool,
        force_lossy: bool,
        is_chat: bool,
        json: &mut Value,
        mut tracer: Option<&mut crate::tracer::SpanRecorder>,
    ) -> EnrichOutcome {
        let mut outcome = EnrichOutcome::default();
        let settings = self.settings();
        // A tenant's guardrails policy is its administrator's control, not a
        // boon the caller opted into: every early return below still scans the
        // input first, so `x-obleth-boons: off`, an unrouted request, or a
        // non-chat endpoint cannot skip it.
        if opt_out {
            guard_input(
                state,
                &settings,
                key,
                session_id,
                is_chat,
                json,
                tracer.as_deref_mut(),
                &mut outcome,
            )
            .await;
            return outcome;
        }

        // ---- replayed image attachments ----
        // Independent of every boon switch and of `route`: the base64 in an
        // assistant message was put there by this gateway, and a client that
        // replays it is sending back megabytes the model cannot read. Stripping
        // it is reverting our own insertion, so it runs for any model — but
        // still after `opt_out`, which means "do not touch my request".
        if is_chat && image_gen::strip_replayed_attachments(json) {
            outcome.rewritten = true;
        }

        let Some(route) = route else {
            guard_input(
                state,
                &settings,
                key,
                session_id,
                is_chat,
                json,
                tracer.as_deref_mut(),
                &mut outcome,
            )
            .await;
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
            guard_input(
                state,
                &settings,
                key,
                session_id,
                is_chat,
                json,
                tracer.as_deref_mut(),
                &mut outcome,
            )
            .await;
            return outcome;
        }

        // ---- knowledge boon, phase 1: capture the query ----
        // The query is read BEFORE any compression pass, because the trailing user
        // turn is itself an eligible compression target and a `[ref:HASH]` marker
        // makes a meaningless retrieval query. Injection happens in phase 2, after
        // compression, so the retrieved text reaches the model verbatim.
        let knowledge_eligible = knowledge::eligible(route, &settings.knowledge, key, is_chat);
        let knowledge_query = if knowledge_eligible {
            knowledge::extract_query(json, settings.knowledge.query_turns)
        } else {
            None
        };

        // ---- compression boon ----
        // All compression passes report under ONE `boon:compression` span: the
        // lossless structural/code pass here, then the reversible dedup + lossy
        // text passes after tool-loop injection. Stats accumulate across both and
        // the single span is recorded once, below.
        let comp_start = crate::tracer::now_ms();
        let mut lossless = compression::CompressionStats::default();
        let mut dedup = compression::DedupStats::default();
        let mut logs = compression::LogStats::default();
        let mut lossy = compression::LossyStats::default();

        // Lossless structural/code compaction: `is_chat` is always true here; the
        // parameter exists so unit tests can exercise the ineligible path.
        if compression_eligible(route, &settings, key, is_chat) {
            let code_compaction = effective_code_compaction(&settings, key);
            lossless = compression::apply(&settings.compression, code_compaction, json);
            if lossless.compressed > 0 {
                outcome.rewritten = true;
                outcome.applied.push("compression");
                state.metrics.record_compression_saved(
                    lossless.tokens_before.saturating_sub(lossless.tokens_after),
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

        // ---- image-generation boon ----
        // Injects a gateway-executed `generate_image` tool and arms the tool
        // loop by inserting its own synthetic-server entry. Deliberately
        // independent of `settings.tool_loop.active()`: granting a model the
        // ability to draw must not silently depend on the unrelated MCP tool
        // loop's global switch. `run()` executes whenever `plan.tool_loop` is
        // `Some`, so the entry alone is enough.
        let mut image_gen_cfg: Option<obleth_config::ImageGenerationBoonSettings> = None;
        if image_gen_eligible(route, &settings) {
            // `generate_image` may already be owned by the client's own tools
            // or by a granted MCP server (`tool_loop_servers`, just injected
            // above). Both the tool injection and the map insert below must be
            // skipped together in that case — deciding them from one shared
            // check keeps "who owns this name" consistent between the request
            // body and the tool loop's dispatch map. See `image_gen::collides`.
            if image_gen::collides(json, tool_loop_servers.as_ref()) {
                tracing::warn!(
                    tool = %image_gen::GENERATE_IMAGE_TOOL,
                    model = %route.model_name,
                    "generate_image collides with a client-supplied tool or a granted MCP \
                     tool; existing tool wins, image-generation boon inactive for this request"
                );
            } else {
                // Nudge only a plain chat client that got no other nudge: an
                // agentic client steers its own tool use, and the tool-loop nudge
                // already told the model it has tools. Read before the insert
                // below, which would otherwise make the map non-empty.
                let nudge = !client_sent_tools && tool_loop_servers.is_none();
                image_gen::inject(
                    &settings.image_generation,
                    nudge,
                    route.supports_system_messages,
                    json,
                );
                tool_loop_servers
                    .get_or_insert_with(std::collections::HashMap::new)
                    .insert(
                        image_gen::GENERATE_IMAGE_TOOL.to_string(),
                        image_gen::IMAGE_SYNTHETIC_SERVER.to_string(),
                    );
                image_gen_cfg = Some(settings.image_generation.clone());
                outcome.rewritten = true;
                outcome.applied.push("image_generation");
            }
        } else if route.boons.iter().any(|b| b == "image_generation")
            && settings.image_generation.active()
        {
            // Granted and configured, but the model cannot call functions.
            // Same failure shape as the tool loop's: the model silently gets no
            // tool and tells the user it cannot draw, with nothing in the logs
            // to explain why.
            tracing::warn!(
                model = %route.model_name,
                "model is granted the image_generation boon but is not flagged \
                 supports_function_calling; no generate_image tool will be injected. \
                 Enable function calling on this model to use the boon."
            );
        }

        // Reversible compression passes (dedup + lossy text), per-piece. Run
        // after tool-loop injection so retrieve_original can ride the same loop.
        if dedup_eligible(route, &settings, key)
            || log_compaction_eligible(route, &settings, key)
            || lossy_eligible(route, &settings, key, force_lossy)
        {
            if dedup_eligible(route, &settings, key) {
                dedup =
                    compression::apply_dedup(state, &settings.compression, key, session_id, json)
                        .await;
            }
            if log_compaction_eligible(route, &settings, key) {
                logs = compression::apply_log_compaction(
                    state,
                    &settings.compression,
                    key,
                    session_id,
                    json,
                )
                .await;
            }
            if lossy_eligible(route, &settings, key, force_lossy) {
                lossy =
                    compression::apply_lossy(state, &settings.compression, key, session_id, json)
                        .await;
            }
            let refs = dedup
                .refs_created
                .saturating_add(logs.refs_created)
                .saturating_add(lossy.refs_created);
            if refs > 0 {
                outcome.rewritten = true;
                if !outcome.applied.contains(&"compression") {
                    outcome.applied.push("compression");
                }
                // Bonus: when the model can call tools, let it recover originals.
                let reversible = route.supports_function_calling && settings.tool_loop.active();
                if reversible {
                    compression::inject_retrieve_original_tool(
                        json,
                        route.supports_system_messages,
                    );
                    tool_loop_servers
                        .get_or_insert_with(std::collections::HashMap::new)
                        .insert(
                            tool_loop::RETRIEVE_ORIGINAL_TOOL.to_string(),
                            tool_loop::COMPRESSION_SYNTHETIC_SERVER.to_string(),
                        );
                }
                state.metrics.record_compression_saved(
                    dedup
                        .tokens_before
                        .saturating_sub(dedup.tokens_after)
                        .saturating_add(logs.tokens_before.saturating_sub(logs.tokens_after))
                        .saturating_add(lossy.tokens_before.saturating_sub(lossy.tokens_after)),
                );
            }
        }

        // ---- combined compression token totals (all passes) ----
        let tokens_before = lossless
            .tokens_before
            .saturating_add(dedup.tokens_before)
            .saturating_add(logs.tokens_before)
            .saturating_add(lossy.tokens_before);
        let tokens_after = lossless
            .tokens_after
            .saturating_add(dedup.tokens_after)
            .saturating_add(logs.tokens_after)
            .saturating_add(lossy.tokens_after);
        // Surface totals for the `x-obleth-compression` response header whenever
        // the compression boon actually inspected tokens on this request.
        if tokens_before > 0 {
            outcome.compression_tokens = Some((tokens_before, tokens_after));
        }

        // ---- single compression trace span (all passes combined) ----
        if let Some(t) = tracer.as_deref_mut() {
            if lossless.scanned > 0 || tokens_before > 0 {
                t.record_elapsed(
                    "boon:compression",
                    "proxy_request",
                    comp_start,
                    "ok",
                    serde_json::json!({
                        "json_compacted": lossless.compressed,
                        "dedup_refs": dedup.refs_created,
                        "log_segments": logs.refs_created,
                        "lossy_segments": lossy.refs_created,
                        "tokens_before": tokens_before,
                        "tokens_after": tokens_after,
                        "tokens_saved": tokens_before.saturating_sub(tokens_after),
                    }),
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

        // ---- guardrails boon (input scanning) ----
        // Guardrails are enabled per-tenant by the presence of a policy; there
        // is no global master switch. Internal probe keys are always exempt.
        //
        // Runs BEFORE the knowledge boon's phase 2 on purpose: guardrails scans
        // every message including system content, so injecting the knowledge
        // block first would let retrieved institutional text (an email address
        // in a policy doc, the literal phrase "prompt injection" in an AI-use
        // policy) trip a tenant's own PII/injection scanner and block a request
        // the boon must never fail. `tracer` is reborrowed here (not moved) so
        // it is still available for the knowledge span below.
        if guard_input(
            state,
            &settings,
            key,
            session_id,
            is_chat,
            json,
            tracer.as_deref_mut(),
            &mut outcome,
        )
        .await
        {
            return outcome;
        }

        // ---- knowledge boon, phase 2: retrieve and inject ----
        // Runs after every compression pass so `available_budget` measures the
        // true post-compression prompt size, and so the injected block itself
        // is never handed to a compression pass on this same request. Also
        // runs after guardrails (see the comment above that block): a blocked
        // request already returned above, so knowledge only ever touches a
        // request that is going upstream, and its own injected text is never
        // subjected to the tenant's input scanners.
        if knowledge_eligible {
            let outcome_label = match knowledge_query.as_deref() {
                None => "no_query",
                Some(_) if route.context_window <= 0 => "no_window",
                Some(query) => {
                    let start = crate::tracer::now_ms();
                    let estimated = state.tokenizer.estimate_request(json).input_tokens;
                    let retrieval =
                        knowledge::apply(state, &settings.knowledge, route, estimated, query, json)
                            .await;
                    if retrieval.outcome == "hit" {
                        outcome.rewritten = true;
                        outcome.applied.push("knowledge");
                    }
                    // Last use of `tracer` in this function (nothing after phase
                    // 2 needs it), so it is moved rather than reborrowed.
                    if let Some(t) = tracer {
                        // Chunk ids and scores always; text only when explicitly
                        // enabled, because it costs ~20x as much span storage.
                        let chunks: Vec<Value> = retrieval
                            .hits
                            .iter()
                            .map(|h| {
                                let mut v =
                                    serde_json::json!({"id": h.id.to_string(), "score": h.score});
                                if settings.knowledge.debug_snapshot {
                                    v["text"] = serde_json::json!(h.text);
                                }
                                v
                            })
                            .collect();
                        t.record_elapsed(
                            "boon:knowledge",
                            "proxy_request",
                            start,
                            "ok",
                            serde_json::json!({
                                "chunks": chunks,
                                "count": retrieval.hits.len(),
                                "outcome": retrieval.outcome,
                            }),
                        );
                    }
                    retrieval.outcome
                }
            };
            state.metrics.record_knowledge_retrieval(outcome_label);
        }

        // The tool loop captures the fully enriched request body so follow-up
        // turns re-dispatch with identical sampling parameters; its own
        // dispatches are always non-streaming. Captured after the knowledge
        // boon's phase 2 so a follow-up tool turn re-dispatches WITH the
        // injected knowledge block, not without it.
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
                image_gen: image_gen_cfg.clone(),
                served_url: None,
            }
        });

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

        // ---- speculation boon ----
        // Armed LAST, over the fully enriched body. A buffered transform
        // (structured output, the MCP tool loop) or output guardrails owns the
        // response and must see the target model's own answer, and the boon's
        // stream would bypass output scanning — so those still rule it out.
        //
        // The image-generation boon is the exception, and it has to be: it
        // arms the tool loop on EVERY request to a model that was granted it,
        // whether or not a picture was wanted, which silently made the two
        // boons mutually exclusive. Granting a model image generation stopped
        // it speculating entirely, with nothing said anywhere.
        //
        // They compose because speculation runs pre-dispatch and abstains by
        // falling through: whichever claims the request wins, and an abstain
        // leaves the tool loop to answer exactly as it does today. The
        // decision is per request rather than per model — the classifier that
        // already gates speculation by category decides, and an unclassified
        // request abstains so the picture is never the thing that goes
        // missing. See `SpeculationPlan::image_tool_pending`.
        let image_tool_pending = image_gen_cfg.is_some();
        if plan_yields_to_speculation(outcome.response_plan.as_ref(), image_tool_pending)
            && speculation_eligible(route, &settings, key)
        {
            if let Some(messages) = speculation::eligible_messages(json) {
                outcome.applied.push("speculation");
                outcome.speculation = Some(speculation::SpeculationPlan {
                    request: json.clone(),
                    messages,
                    settings: settings.speculation.clone(),
                    client_stream,
                    include_usage,
                    image_tool_pending,
                });
            }
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

/// Largest helper reply body read into memory. A helper reply is one chat
/// completion (or one MCP JSON-RPC message); anything bigger is a misbehaving
/// upstream, and reading it unbounded would let it exhaust gateway memory.
pub(crate) const HELPER_BODY_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Where a helper call is sent: the same endpoint selection the proxy's own
/// dispatch uses ([`crate::proxy::build_targets`]), so a model that lives only
/// in `endpoints` (every Slurm-provisioned model has a blank `api_base`) is
/// reachable, unhealthy endpoints are skipped, and a per-endpoint key wins
/// over the model's.
///
/// `pinned` is a URL the model already answered on this request (the tool
/// loop's turn 0); the target whose base prefixes it is chosen so follow-up
/// turns land on the replica holding the conversation's prefix cache. Falls
/// back to the first target when nothing matches.
pub(crate) fn helper_target(
    model: &ResolvedModel,
    session_key: &str,
    pinned: Option<&str>,
) -> anyhow::Result<crate::proxy::Target> {
    let mut targets =
        crate::proxy::build_targets(Some(model), "", &model.endpoint_selection_mode, session_key);
    let pick = pinned
        .and_then(|url| {
            targets.iter().position(|t| {
                // Whole path segments only: `http://h:8000/v1` must not
                // claim a URL served by `http://h:8000/v10`.
                let base = t.base.trim_end_matches('/');
                !base.is_empty()
                    && url.strip_prefix(base).is_some_and(|rest| {
                        rest.is_empty() || rest.starts_with('/') || rest.starts_with('?')
                    })
            })
        })
        .unwrap_or(0);
    if pick >= targets.len() {
        anyhow::bail!("model `{}` has no upstream target", model.model_name);
    }
    let target = targets.swap_remove(pick);
    if target.base.trim().is_empty() {
        anyhow::bail!(
            "model `{}` has no usable upstream endpoint",
            model.model_name
        );
    }
    Ok(target)
}

/// Read a response body, refusing anything larger than `max` bytes. The
/// declared `content-length` is checked first so an honest oversized reply is
/// rejected without reading it; chunked bodies are cut off as they cross.
pub(crate) async fn read_body_capped(
    mut resp: reqwest::Response,
    max: usize,
) -> anyhow::Result<bytes::Bytes> {
    if resp.content_length().is_some_and(|n| n > max as u64) {
        anyhow::bail!("upstream body exceeds {max} bytes");
    }
    let mut buf: Vec<u8> = Vec::new();
    while let Some(chunk) = resp.chunk().await? {
        if buf.len().saturating_add(chunk.len()) > max {
            anyhow::bail!("upstream body exceeds {max} bytes");
        }
        buf.extend_from_slice(&chunk);
    }
    Ok(bytes::Bytes::from(buf))
}

/// POST one non-streaming chat completion to `target` and return the parsed
/// JSON body (size-capped). No timeout of its own; callers bound it.
async fn post_chat_completion(
    http: &reqwest::Client,
    target: &crate::proxy::Target,
    body: &Value,
) -> anyhow::Result<Value> {
    let mut req = http
        .post(build_chat_url(&target.base))
        .headers(target.headers.clone())
        .json(body);
    if let Some(api_key) = &target.api_key {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("upstream returned {}", resp.status());
    }
    let bytes = read_body_capped(resp, HELPER_BODY_MAX_BYTES).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Send a chat-completions request to a helper model (describer, fixer) and
/// return its reply text, bounded by `timeout`.
pub(crate) async fn chat_call(
    state: &AppState,
    helper: &ResolvedModel,
    body: Value,
    timeout: Duration,
) -> anyhow::Result<ChatCallResult> {
    chat_call_with(&state.http, helper, body, timeout).await
}

async fn chat_call_with(
    http: &reqwest::Client,
    helper: &ResolvedModel,
    body: Value,
    timeout: Duration,
) -> anyhow::Result<ChatCallResult> {
    let fut = async {
        let target = helper_target(helper, "", None)?;
        let body = post_chat_completion(http, &target, &body).await?;
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
    let target = helper_target(model, "", None)?;
    chat_call_completion_on(&state.http, &target, body, timeout).await
}

/// [`chat_call_completion`] against an already-resolved target, for callers
/// that must keep every call on one endpoint (the tool loop).
pub(crate) async fn chat_call_completion_on(
    http: &reqwest::Client,
    target: &crate::proxy::Target,
    body: Value,
    timeout: Duration,
) -> anyhow::Result<Value> {
    match tokio::time::timeout(timeout, post_chat_completion(http, target, &body)).await {
        Ok(result) => result,
        Err(_) => anyhow::bail!("chat call timed out after {timeout:?}"),
    }
}

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
/// [`helper_request_type`]).
pub(crate) fn bill_helper_call(
    state: &AppState,
    helper: &ResolvedModel,
    key: &ResolvedKey,
    session_id: &str,
    request_type: &str,
    input_tokens: u32,
    output_tokens: u32,
) {
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
        // Helper calls record no duration (total_ms: 0), so slot-share energy
        // is zero here by construction; the main request's wall time covers
        // this hardware time at the main model's slot rate.
        energy_wh: 0.0,
        energy_cost_usd: 0.0,
        co2_g: 0.0,
        ts_ms: now_ms(),
        session_id: session_id.to_string(),
        session_id_source: "none".to_string(),
        request_type: request_type.to_string(),
        device_id: String::new(),
    });
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
    images: u32,
) {
    let request_type = helper_request_type(key, "image_generation_boon");
    let cost_usd = images as f64 * image_model.cost_per_image;

    state.metrics.record_request("boon", 200, 0, 0);
    if key.internal {
        return;
    }
    commit_helper_term_usage(state, key, 0, cost_usd);
    state.telemetry.record(UsageRecord {
        request_id: Uuid::new_v4(),
        tenant_id: key.tenant_id,
        key_id: key.key_id,
        model: image_model.model_name.clone(),
        admission: "boon".to_string(),
        weight: key.weight,
        input_tokens: 0,
        output_tokens: 0,
        estimated_tokens: 0,
        queue_wait_ms: 0,
        ttft_ms: 0,
        total_ms: 0,
        status_code: 200,
        cache_status: "off".to_string(),
        cost_usd,
        // Same reasoning as `bill_helper_call`: no duration is recorded here,
        // so slot-share energy is zero by construction and the main request's
        // wall time covers this hardware time.
        energy_wh: 0.0,
        energy_cost_usd: 0.0,
        co2_g: 0.0,
        ts_ms: now_ms(),
        session_id: session_id.to_string(),
        session_id_source: "none".to_string(),
        request_type: request_type.to_string(),
        device_id: String::new(),
    });
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

/// Test fixtures other modules of the crate reuse.
#[cfg(test)]
pub(crate) mod test_support {
    /// A Slurm-provisioned model: blank `api_base`, one healthy endpoint
    /// carrying its own key (`endpoint-key`).
    pub(crate) fn endpoint_only_route(base: &str) -> obleth_config::ResolvedModel {
        super::tests::endpoint_only_route(base)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn test_route() -> obleth_config::ResolvedModel {
        obleth_config::ResolvedModel {
            model_name: "test".to_string(),
            aliases: Vec::new(),
            quantization: "unknown".into(),
            upstream_model: "test".to_string(),
            api_base: "http://localhost".to_string(),
            api_key: None,
            upstream_headers: Default::default(),
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
            declared_levels: vec![],
            boons: vec![],
            tool_servers: vec![],
            knowledge_collections: vec![],
            request_timeout_secs: None,
            max_retries: 0,
            retry_backoff_ms: 200,
            endpoint_selection_mode: "failover".to_string(),
            debug_diagnostics: false,
            energy_slots_per_node: 0,
            route_bias: 1.0,
            auto_eligible: true,
            draft_model: String::new(),
            verify_api_base: String::new(),
            verify_upstream_model: String::new(),
            endpoints: vec![],
        }
    }

    pub(super) fn test_key_with_policy(
        policy: Option<obleth_config::CompressionPolicy>,
    ) -> obleth_config::ResolvedKey {
        let mut k = obleth_config::ResolvedKey {
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
            key_weight: 100,
            key_max_in_flight: None,
            allowed_models: None,
            internal: false,
            tracing_enabled: false,
            guardrails_policy: None,
            compression_policy: None,
            synthetic: false,
        };
        k.compression_policy = policy;
        k
    }

    fn image_only_loop() -> tool_loop::ToolLoopPlan {
        let mut servers = std::collections::HashMap::new();
        servers.insert(
            image_gen::GENERATE_IMAGE_TOOL.to_string(),
            image_gen::IMAGE_SYNTHETIC_SERVER.to_string(),
        );
        tool_loop::ToolLoopPlan {
            tool_servers: servers,
            request: serde_json::json!({}),
            settings: obleth_config::ToolLoopSettings::default(),
            passthrough_unmapped: false,
            image_gen: None,
            served_url: None,
        }
    }

    fn plan_with(tool_loop: Option<tool_loop::ToolLoopPlan>) -> ResponsePlan {
        ResponsePlan {
            structured: None,
            tool_loop,
            client_stream: false,
            include_usage: false,
            guardrails: None,
        }
    }

    #[test]
    fn an_image_only_tool_loop_leaves_room_for_speculation() {
        // Granting image generation used to disable speculation outright,
        // because the boon arms the tool loop on every request.
        assert!(plan_yields_to_speculation(None, false));
        assert!(plan_yields_to_speculation(
            Some(&plan_with(Some(image_only_loop()))),
            true
        ));
    }

    #[test]
    fn a_real_tool_loop_or_buffered_transform_still_owns_the_response() {
        // Two servers: the model is genuinely being given tools.
        let mut two = image_only_loop();
        two.tool_servers
            .insert("search".to_string(), "mcp-search".to_string());
        assert!(!plan_yields_to_speculation(
            Some(&plan_with(Some(two))),
            true
        ));

        // The client passed its own tools through.
        let mut passthrough = image_only_loop();
        passthrough.passthrough_unmapped = true;
        assert!(!plan_yields_to_speculation(
            Some(&plan_with(Some(passthrough))),
            true
        ));

        // One server, but not the image boon's — a single granted MCP tool.
        assert!(!plan_yields_to_speculation(
            Some(&plan_with(Some(image_only_loop()))),
            false
        ));

        // A plan with no tool loop at all is structured output or guardrails.
        assert!(!plan_yields_to_speculation(Some(&plan_with(None)), true));
    }

    #[test]
    fn speculation_eligible_requires_grant_scoring_endpoint_and_distinct_drafter() {
        let mut route = test_route();
        let mut key = test_key_with_policy(None);
        let mut settings = obleth_config::BoonSettings::default();

        assert!(
            !speculation_eligible(&route, &settings, &key),
            "nothing configured"
        );
        settings.speculation.enabled = true;
        settings.speculation.draft_model = Some("drafter".into());
        assert!(
            !speculation_eligible(&route, &settings, &key),
            "boon not granted on the model"
        );
        route.boons = vec!["speculation".into()];
        assert!(
            !speculation_eligible(&route, &settings, &key),
            "the target has no scoring endpoint, so it cannot speculate"
        );
        route.verify_api_base = "http://scorer.test:8000/v1".into();
        assert!(speculation_eligible(&route, &settings, &key));

        key.internal = true;
        assert!(
            !speculation_eligible(&route, &settings, &key),
            "internal probes must measure the target model itself"
        );
        key.internal = false;

        // The model's own drafter beats the fleet default...
        route.draft_model = "special-drafter".into();
        assert!(speculation_eligible(&route, &settings, &key));
        // ...including when that choice is the (invalid) self-draft.
        route.draft_model = route.model_name.clone();
        assert!(
            !speculation_eligible(&route, &settings, &key),
            "a model must not draft for itself"
        );
        route.draft_model = String::new();
        settings.speculation.draft_model = Some(route.model_name.clone());
        assert!(
            !speculation_eligible(&route, &settings, &key),
            "the fleet-default drafter must not be the target either"
        );
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
                key_weight: 100,
                key_max_in_flight: None,
                allowed_models: None,
                internal: false,
                tracing_enabled: false,
                guardrails_policy: None,
                compression_policy: None,
                synthetic: false,
            }
        }

        let mut settings = BoonSettings {
            compression: CompressionBoonSettings {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };

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
        key.compression_policy = Some(CompressionPolicy {
            enabled: false,
            code_compaction: false,
            dedup: false,
            compact_logs: false,
            allow_lossy: false,
        });
        assert!(!compression_eligible(&route, &settings, &key, true));

        // Tenant policy enabled -> eligible again.
        key.compression_policy = Some(CompressionPolicy {
            enabled: true,
            code_compaction: false,
            dedup: false,
            compact_logs: false,
            allow_lossy: false,
        });
        assert!(compression_eligible(&route, &settings, &key, true));
    }

    #[test]
    fn dedup_and_lossy_gate_on_tenant_toggle_only() {
        use obleth_config::{BoonSettings, CompressionBoonSettings, CompressionPolicy};
        let settings = BoonSettings {
            compression: CompressionBoonSettings {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        // Note: NO summarizer, tool loop OFF, model NOT function-calling.
        let mut route = test_route();
        route.boons = vec!["compression".to_string()];
        route.supports_function_calling = false;

        let mut key = test_key_with_policy(Some(CompressionPolicy {
            enabled: true,
            code_compaction: false,
            dedup: true,
            compact_logs: false,
            allow_lossy: true,
        }));
        assert!(dedup_eligible(&route, &settings, &key));
        assert!(lossy_eligible(&route, &settings, &key, false));

        // Toggles off → ineligible.
        key.compression_policy = Some(CompressionPolicy {
            enabled: true,
            code_compaction: false,
            dedup: false,
            compact_logs: false,
            allow_lossy: false,
        });
        assert!(!dedup_eligible(&route, &settings, &key));
        assert!(!lossy_eligible(&route, &settings, &key, false));
        // ...but the per-request force override turns it on despite the toggle.
        assert!(lossy_eligible(&route, &settings, &key, true));

        // No policy → ineligible (conservative).
        key.compression_policy = None;
        assert!(!dedup_eligible(&route, &settings, &key));
        assert!(!lossy_eligible(&route, &settings, &key, false));
        // Force still respects the base gate: it flips only the allow_lossy
        // toggle, so with the boon granted + globally active it now applies.
        assert!(lossy_eligible(&route, &settings, &key, true));
    }

    #[test]
    fn force_lossy_still_requires_the_boon_granted() {
        use obleth_config::{BoonSettings, CompressionBoonSettings};
        let settings = BoonSettings {
            compression: CompressionBoonSettings {
                enabled: true,
                ..Default::default()
            },
            ..Default::default()
        };
        // Model was NOT granted the compression boon.
        let mut route = test_route();
        route.boons = vec![];
        let key = test_key_with_policy(None);
        // The force override cannot bypass the base gate (boon must be granted).
        assert!(!lossy_eligible(&route, &settings, &key, true));
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

    fn endpoint(id: &str, base: &str, key: Option<&str>, healthy: bool) -> ResolvedEndpoint {
        ResolvedEndpoint {
            id: id.to_string(),
            api_base: base.to_string(),
            api_key: key.map(str::to_string),
            priority: 0,
            weight: 1,
            enabled: true,
            healthy,
        }
    }

    /// A Slurm-provisioned model: blank `api_base`, reachable only through its
    /// endpoint list.
    pub(super) fn endpoint_only_route(base: &str) -> ResolvedModel {
        let mut route = test_route();
        route.api_base = String::new();
        route.api_key = Some("model-key".into());
        route.endpoints = vec![endpoint("e1", base, Some("endpoint-key"), true)];
        route
    }

    use obleth_config::ResolvedEndpoint;

    #[test]
    fn helper_target_uses_the_healthy_endpoint_of_an_endpoint_only_model() {
        let route = endpoint_only_route("http://replica-1:8000/v1");
        let target = helper_target(&route, "", None).expect("endpoint target");
        assert_eq!(target.base, "http://replica-1:8000/v1");
        assert_eq!(
            target.api_key.as_deref(),
            Some("endpoint-key"),
            "the endpoint's own key wins over the model's"
        );
    }

    #[test]
    fn helper_target_refuses_a_model_with_nowhere_to_go() {
        let mut route = endpoint_only_route("http://replica-1:8000/v1");
        route.endpoints[0].healthy = false;
        assert!(
            helper_target(&route, "", None).is_err(),
            "no healthy endpoint and a blank api_base must fail the helper, not POST to /chat/completions"
        );
        route.endpoints.clear();
        assert!(helper_target(&route, "", None).is_err());
    }

    #[test]
    fn helper_target_pins_the_endpoint_that_served_the_request() {
        let mut route = endpoint_only_route("http://replica-1:8000/v1");
        route
            .endpoints
            .push(endpoint("e2", "http://replica-2:8000/v1/", None, true));
        route.endpoints[1].priority = 5;
        // Failover order would pick replica-1 first.
        let pinned = helper_target(
            &route,
            "",
            Some("http://replica-2:8000/v1/chat/completions"),
        )
        .unwrap();
        assert_eq!(pinned.base, "http://replica-2:8000/v1/");
        assert_eq!(
            pinned.api_key.as_deref(),
            Some("model-key"),
            "an endpoint without its own key falls back to the model's"
        );
        // A pin matches whole path segments only.
        let mut prefixed = route.clone();
        prefixed.endpoints[1].api_base = "http://replica-1:8000/v10".into();
        prefixed.endpoints[1].priority = 0;
        prefixed.endpoints[0].priority = 5;
        let pin = helper_target(
            &prefixed,
            "",
            Some("http://replica-1:8000/v10/chat/completions"),
        )
        .unwrap();
        assert_eq!(pin.base, "http://replica-1:8000/v10");
        prefixed.endpoints[0].priority = 0;
        prefixed.endpoints[1].priority = 5;
        let pin = helper_target(
            &prefixed,
            "",
            Some("http://replica-1:8000/v10/chat/completions"),
        )
        .unwrap();
        assert_eq!(
            pin.base, "http://replica-1:8000/v10",
            "`/v1` must not claim a URL served by `/v10`"
        );
        // A pin that matches nothing falls back to normal selection.
        let fallback = helper_target(&route, "", Some("http://elsewhere/v1")).unwrap();
        assert_eq!(fallback.base, "http://replica-1:8000/v1");
    }

    /// Serve `reply` on `/v1/chat/completions`, recording each request's
    /// bearer token.
    pub(super) async fn chat_server(reply: Value) -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let seen_clone = seen.clone();
        let app = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(move |headers: axum::http::HeaderMap| {
                let seen = seen_clone.clone();
                let reply = reply.clone();
                async move {
                    let auth = headers
                        .get("authorization")
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    seen.lock().unwrap().push(auth);
                    axum::Json(reply)
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{addr}/v1"), seen)
    }

    #[tokio::test]
    async fn chat_call_reaches_an_endpoint_only_model() {
        let (base, seen) = chat_server(serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "described" } }],
            "usage": { "prompt_tokens": 7, "completion_tokens": 3 }
        }))
        .await;
        let route = endpoint_only_route(&base);
        let reply = chat_call_with(
            &reqwest::Client::new(),
            &route,
            serde_json::json!({ "messages": [] }),
            Duration::from_secs(5),
        )
        .await
        .expect("helper call through the endpoint");
        assert_eq!(reply.text, "described");
        assert_eq!((reply.input_tokens, reply.output_tokens), (7, 3));
        assert_eq!(seen.lock().unwrap().as_slice(), ["Bearer endpoint-key"]);
    }

    #[tokio::test]
    async fn chat_call_completion_on_rejects_an_oversized_reply() {
        let big = "x".repeat(HELPER_BODY_MAX_BYTES + 1);
        let (base, _) = chat_server(serde_json::json!({ "pad": big })).await;
        let target = helper_target(&endpoint_only_route(&base), "", None).unwrap();
        let err = chat_call_completion_on(
            &reqwest::Client::new(),
            &target,
            serde_json::json!({}),
            Duration::from_secs(10),
        )
        .await
        .expect_err("a reply over the cap is refused");
        assert!(err.to_string().contains("exceeds"), "{err}");
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
            let (tokens, cost) = redis.term_usage_read(&id, "l:0").await.unwrap();
            assert_eq!(tokens, 150);
            assert!((cost - 0.75).abs() < 1e-9, "cost {cost}");
        }
    }

    #[test]
    fn every_early_return_before_guardrails_still_scans_input() {
        // A tenant's guardrails policy must not be skippable by
        // `x-obleth-boons: off`, an unrouted request, or a non-chat path.
        // Every `return outcome;` in `enrich_request` above the in-flow
        // guardrails section must be preceded by its own `guard_input` call.
        let full_src = include_str!("mod.rs");
        let src = &full_src[..full_src
            .find("\nmod tests {")
            .expect("this file's own test module marker")];
        let start = src
            .find("pub async fn enrich_request(")
            .expect("enrich_request");
        let end = start
            + src[start..]
                .find("---- guardrails boon (input scanning) ----")
                .expect("guardrails section marker");
        let body = &src[start..end];
        let segments: Vec<&str> = body.split("return outcome;").collect();
        assert!(
            segments.len() >= 4,
            "expected the opt-out, no-route, and non-chat early returns"
        );
        for (i, seg) in segments[..segments.len() - 1].iter().enumerate() {
            assert!(
                seg.contains("guard_input("),
                "early return #{} in enrich_request skips the input guardrails",
                i + 1
            );
        }
    }

    /// The knowledge boon's two-phase split is an ordering property of
    /// `enrich_request` itself — no test over the pure functions in
    /// `boons::knowledge` can express "phase 1 runs before compression" or
    /// "phase 2 runs after guardrails", and there is no `AppState` test
    /// harness in this crate to exercise `enrich_request` end to end (only
    /// `main.rs` builds one, and it would drag in Redis, fairshare, and
    /// telemetry). So this asserts the invariant directly against this
    /// file's own source, blunt as that is: it fails loudly if someone
    /// reorders these blocks, which is the actual failure that must be
    /// caught. See Task 11 fix round 1, item 5, and item 1 for why phase 2
    /// must land after guardrails specifically (an injected chunk must never
    /// be scanned and blocked by the tenant's own input scanners).
    #[test]
    fn knowledge_boon_phases_stay_in_source_order() {
        let full_src = include_str!("mod.rs");
        // Search only the code above this test module. Otherwise this
        // test's own source contains the same anchor literals in the same
        // asserted order, so it would pass vacuously even if the boon's
        // wiring in `enrich_request` were deleted entirely — a reorder is
        // still caught, but a deletion is not, unless the test module
        // itself is excluded from the haystack.
        let src = &full_src[..full_src
            .find("\nmod tests {")
            .expect("this file's own test module marker")];
        let phase1 = src
            .find("knowledge boon, phase 1")
            .expect("phase 1 marker comment");
        let compression_apply = src
            .find("compression::apply(")
            .expect("lossless compression::apply call");
        let apply_lossy = src
            .find("compression::apply_lossy(")
            .expect("apply_lossy call");
        // The in-flow call, not the `guard_input` helper defined above
        // `enrich_request` (which would satisfy the ordering vacuously).
        let guardrails_marker = src
            .find("---- guardrails boon (input scanning) ----")
            .expect("guardrails section marker");
        let guardrails_apply = guardrails_marker
            + src[guardrails_marker..]
                .find("guard_input(")
                .expect("in-flow guard_input call");
        let phase2 = src
            .find("knowledge boon, phase 2")
            .expect("phase 2 marker comment");
        let tool_loop_plan = src
            .find("let tool_loop_plan = tool_loop_servers.map(")
            .expect("tool_loop_plan binding");

        assert!(
            phase1 < compression_apply,
            "the query must be captured before any compression pass touches the request"
        );
        assert!(
            apply_lossy < phase2,
            "phase 2 must run after every compression pass, or the injected \
             block itself becomes an eligible compression target"
        );
        assert!(
            guardrails_apply < phase2,
            "phase 2 must run after guardrails' input scan, or a retrieved \
             chunk can trip a tenant's own PII/injection scanner and block \
             a request the boon must never fail"
        );
        assert!(
            phase2 < tool_loop_plan,
            "the tool loop's captured request must include the injected \
             knowledge block, or every follow-up tool turn goes ungrounded"
        );
    }

    #[test]
    fn image_gen_eligible_requires_grant_active_and_function_calling() {
        use obleth_config::{BoonSettings, ImageGenerationBoonSettings};

        let mut settings = BoonSettings {
            image_generation: ImageGenerationBoonSettings {
                enabled: true,
                image_model: Some("sdxl".to_string()),
                ..Default::default()
            },
            ..Default::default()
        };
        let mut route = test_route();
        route.boons = vec!["image_generation".to_string()];
        route.supports_function_calling = true;

        assert!(image_gen_eligible(&route, &settings));

        // Not granted -> ineligible.
        route.boons.clear();
        assert!(!image_gen_eligible(&route, &settings));
        route.boons = vec!["image_generation".to_string()];

        // No native function calling -> ineligible (the tool can never be called).
        route.supports_function_calling = false;
        assert!(!image_gen_eligible(&route, &settings));
        route.supports_function_calling = true;

        // Global switch off -> ineligible.
        settings.image_generation.enabled = false;
        assert!(!image_gen_eligible(&route, &settings));
        settings.image_generation.enabled = true;

        // No image model configured -> ineligible.
        settings.image_generation.image_model = None;
        assert!(!image_gen_eligible(&route, &settings));
    }

    /// The gate must sit inside the `is_chat` region and after the tool-loop
    /// injection block, so a client-supplied `tools` array is already visible
    /// and the boon's synthetic entry lands in the same `tool_loop_servers`
    /// map the plan is built from. Asserted against this file's own source for
    /// the same reason `knowledge_boon_phases_stay_in_source_order` is: there
    /// is no `AppState` harness in this crate to drive `enrich_request`.
    #[test]
    fn image_gen_gate_runs_after_tool_loop_injection() {
        let full_src = include_str!("mod.rs");
        let src = &full_src[..full_src
            .find("\nmod tests {")
            .expect("this file's own test module marker")];
        let is_chat_guard = src.find("if !is_chat {").expect("the is_chat early return");
        let tool_loop_inject = src
            .find("tool_loop_servers = tool_loop::inject(")
            .expect("tool-loop injection call");
        // The full banner, not the bare phrase: `image_gen_eligible`'s own doc
        // comment sits far above the gate and would otherwise match first.
        let image_gate = src
            .find("---- image-generation boon ----")
            .expect("image-generation gate marker comment");
        let tool_loop_plan = src
            .find("let tool_loop_plan = tool_loop_servers.map(")
            .expect("tool_loop_plan binding");

        assert!(is_chat_guard < image_gate, "the boon is chat-only");
        assert!(
            tool_loop_inject < image_gate,
            "the gate must see the tool-loop injection's result so a model with both \
             MCP tools and this boon gets one merged tools array"
        );
        assert!(
            image_gate < tool_loop_plan,
            "the synthetic server entry must be in the map before the plan is built"
        );
    }
}
