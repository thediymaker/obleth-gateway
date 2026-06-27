//! Response-side machinery for the structured-output boon.
//!
//! When request enrichment arms a [`super::ResponsePlan`], the proxy forces a
//! non-streaming upstream call, buffers the complete `chat.completion` JSON,
//! and hands it to [`transform_completion`]. For clients that asked for
//! `stream: true`, the transformed completion is re-emitted as a short
//! synthesized SSE sequence by [`synthesize_sse`].

use std::time::Duration;

use obleth_config::{ResolvedKey, ResolvedModel, STRUCTURED_OUTPUT_MAX_REPAIR_ATTEMPTS};
use serde_json::{json, Value};

use super::{structured, ResponsePlan, StructuredPlan};
use crate::state::AppState;

/// Non-fatal outcome notes from a response transform, surfaced to the client
/// via the `x-obleth-boons-warning` header.
pub struct TransformResult {
    pub warning: Option<&'static str>,
}

/// Apply the structured-output boon to a buffered chat completion.
///
/// Fail-open throughout: anything unexpected leaves the body unchanged.
pub async fn transform_completion(
    state: &AppState,
    plan: &ResponsePlan,
    route: Option<&ResolvedModel>,
    key: &ResolvedKey,
    session_id: &str,
    body: &mut Value,
) -> TransformResult {
    // ---- structured-output boon ----
    let warning = match &plan.structured {
        Some(structured_plan) => {
            apply_structured(state, structured_plan, route, key, session_id, body).await
        }
        None => None,
    };
    TransformResult { warning }
}

/// Validate (and repair) the completion's JSON against the armed schema,
/// replacing the content with the canonical document on success. Returns the
/// warning label on final failure (the original content passes through).
pub(super) async fn apply_structured(
    state: &AppState,
    plan: &StructuredPlan,
    route: Option<&ResolvedModel>,
    key: &ResolvedKey,
    session_id: &str,
    body: &mut Value,
) -> Option<&'static str> {
    let content = body
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())?;
    match enforce_schema(state, plan, route, key, session_id, &content).await {
        Some(canonical) => {
            if let Some(message) = body
                .pointer_mut("/choices/0/message")
                .and_then(|m| m.as_object_mut())
            {
                message.insert("content".into(), Value::String(canonical));
            }
            None
        }
        None => Some("structured_output_validation_failed"),
    }
}

/// Extract, validate, and (when necessary) repair the completion's JSON
/// document. Returns the canonical compact JSON string on success, `None`
/// when every attempt failed (the caller passes the original through).
async fn enforce_schema(
    state: &AppState,
    plan: &super::StructuredPlan,
    route: Option<&ResolvedModel>,
    key: &ResolvedKey,
    session_id: &str,
    content: &str,
) -> Option<String> {
    let mut errors = match check(plan.schema.as_ref(), content) {
        Ok(canonical) => return Some(canonical),
        Err(errors) => errors,
    };

    // Repair loop: the configured fixer model, or the request's own model.
    let fixer = match plan
        .settings
        .fixer_model
        .as_deref()
        .filter(|m| !m.trim().is_empty())
    {
        Some(name) => match crate::proxy::resolve_model(state, name).await {
            Some(m) if m.enabled => Some(m),
            _ => {
                tracing::warn!(
                    model = %name,
                    "structured-output fixer model unavailable; repairing with the request's own model"
                );
                None
            }
        },
        None => None,
    };
    let helper = match (&fixer, route) {
        (Some(f), _) => f.as_ref().clone(),
        (None, Some(r)) => r.clone(),
        (None, None) => return None,
    };

    let attempts = plan
        .settings
        .max_repair_attempts
        .min(STRUCTURED_OUTPUT_MAX_REPAIR_ATTEMPTS);
    let timeout = Duration::from_millis(plan.settings.timeout_ms.max(1));
    let mut current = content.to_string();
    for attempt in 0..attempts {
        let request = structured::repair_request_body(
            &helper.upstream_model,
            plan.schema.as_ref(),
            &current,
            &errors,
        );
        match super::chat_call(state, &helper, request, timeout).await {
            Ok(reply) => {
                super::bill_helper_call(
                    state,
                    &helper,
                    key,
                    session_id,
                    "structured_output_boon",
                    reply.input_tokens,
                    reply.output_tokens,
                );
                match check(plan.schema.as_ref(), &reply.text) {
                    Ok(canonical) => return Some(canonical),
                    Err(next_errors) => {
                        current = reply.text;
                        errors = next_errors;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    attempt,
                    model = %helper.model_name,
                    "structured-output repair call failed"
                );
            }
        }
    }
    None
}

/// Extract the JSON document from `content` and validate it. Returns the
/// canonical compact serialization on success, the error list on failure.
fn check(schema: Option<&Value>, content: &str) -> Result<String, Vec<String>> {
    let Some(value) = structured::extract_json(content) else {
        return Err(vec!["no JSON document found in the output".to_string()]);
    };
    if let Some(schema) = schema {
        structured::validate(schema, &value)?;
    }
    serde_json::to_string(&value).map_err(|e| vec![e.to_string()])
}

/// Re-emit a complete chat completion as the SSE chunk sequence a streaming
/// client expects: role delta, content delta, tool-call delta, finish chunk,
/// optional usage chunk, `[DONE]`.
pub fn synthesize_sse(completion: &Value, include_usage: bool) -> String {
    let id = completion
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("chatcmpl-obleth-boon");
    let created = completion
        .get("created")
        .and_then(|v| v.as_u64())
        .unwrap_or_else(|| (super::now_ms() / 1000) as u64);
    let model = completion
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let chunk = |delta: Value, finish: Value| -> String {
        let body = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }],
        });
        format!("data: {body}\n\n")
    };

    let message = completion.pointer("/choices/0/message");
    let content = message
        .and_then(|m| m.get("content"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    // Preserve a reasoning/thinking trace when the upstream returned one in a
    // dedicated field (`reasoning_content` on most OpenAI-compatible servers,
    // `reasoning` on some). The buffered tool-loop path re-synthesizes the SSE
    // stream, so without this the thinking — including whatever the model
    // concluded from the tool results — would be dropped, unlike the plain
    // streaming passthrough. The original field name is mirrored back so the
    // client renders it exactly as it would natively.
    let reasoning = message.and_then(|m| {
        ["reasoning_content", "reasoning"]
            .into_iter()
            .find_map(|field| {
                m.get(field)
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
                    .map(|s| (field, s.to_string()))
            })
    });
    let tool_calls = message
        .and_then(|m| m.get("tool_calls"))
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    let finish_reason = completion
        .pointer("/choices/0/finish_reason")
        .cloned()
        .filter(|v| !v.is_null())
        .unwrap_or_else(|| Value::String("stop".into()));

    let mut out = String::new();
    out.push_str(&chunk(
        json!({ "role": "assistant", "content": "" }),
        Value::Null,
    ));
    // Thinking streams before the answer, matching native reasoning models.
    if let Some((field, text)) = &reasoning {
        let mut delta = serde_json::Map::new();
        delta.insert((*field).to_string(), Value::String(text.clone()));
        out.push_str(&chunk(Value::Object(delta), Value::Null));
    }
    if !content.is_empty() {
        out.push_str(&chunk(json!({ "content": content }), Value::Null));
    }
    if !tool_calls.is_empty() {
        let deltas: Vec<Value> = tool_calls
            .iter()
            .enumerate()
            .map(|(i, call)| {
                json!({
                    "index": i,
                    "id": call.get("id").cloned().unwrap_or(Value::Null),
                    "type": "function",
                    "function": call.get("function").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        out.push_str(&chunk(json!({ "tool_calls": deltas }), Value::Null));
    }
    out.push_str(&chunk(json!({}), finish_reason));
    if include_usage {
        let usage = completion.get("usage").cloned().unwrap_or(Value::Null);
        let body = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [],
            "usage": usage,
        });
        out.push_str(&format!("data: {body}\n\n"));
    }
    out.push_str("data: [DONE]\n\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion(message: Value, finish: &str) -> Value {
        json!({
            "id": "chatcmpl-42",
            "object": "chat.completion",
            "created": 1700000000,
            "model": "minimax",
            "choices": [{ "index": 0, "message": message, "finish_reason": finish }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15 }
        })
    }

    #[test]
    fn sse_for_plain_content() {
        let body = completion(json!({ "role": "assistant", "content": "Hello!" }), "stop");
        let sse = synthesize_sse(&body, false);
        let events: Vec<&str> = sse.split("\n\n").filter(|s| !s.is_empty()).collect();
        // role chunk, content chunk, finish chunk, [DONE]
        assert_eq!(events.len(), 4);
        assert!(events[0].contains("\"role\":\"assistant\""));
        assert!(events[1].contains("\"content\":\"Hello!\""));
        assert!(events[2].contains("\"finish_reason\":\"stop\""));
        assert_eq!(events[3], "data: [DONE]");
        // Every data chunk carries the completion id and chunk object type.
        for e in &events[..3] {
            assert!(e.starts_with("data: "));
            assert!(e.contains("chatcmpl-42"));
            assert!(e.contains("chat.completion.chunk"));
        }
    }

    #[test]
    fn sse_for_tool_calls_with_usage() {
        let body = completion(
            json!({
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "get_weather", "arguments": "{\"city\":\"Paris\"}" }
                }]
            }),
            "tool_calls",
        );
        let sse = synthesize_sse(&body, true);
        let events: Vec<&str> = sse.split("\n\n").filter(|s| !s.is_empty()).collect();
        // role chunk, tool_calls chunk, finish chunk, usage chunk, [DONE]
        assert_eq!(events.len(), 5);
        assert!(events[1].contains("\"tool_calls\""));
        assert!(events[1].contains("\"index\":0"));
        assert!(events[1].contains("get_weather"));
        assert!(events[2].contains("\"finish_reason\":\"tool_calls\""));
        assert!(events[3].contains("\"prompt_tokens\":10"));
        assert!(events[3].contains("\"choices\":[]"));
        assert_eq!(events[4], "data: [DONE]");
    }

    #[test]
    fn sse_preserves_reasoning_before_content() {
        let body = completion(
            json!({
                "role": "assistant",
                "reasoning_content": "Let me search, then answer.",
                "content": "Paris is sunny."
            }),
            "stop",
        );
        let sse = synthesize_sse(&body, false);
        let events: Vec<&str> = sse.split("\n\n").filter(|s| !s.is_empty()).collect();
        // role chunk, reasoning chunk, content chunk, finish chunk, [DONE]
        assert_eq!(events.len(), 5);
        assert!(events[1].contains("\"reasoning_content\":\"Let me search, then answer.\""));
        assert!(events[2].contains("\"content\":\"Paris is sunny.\""));
        // Thinking streams before the answer.
        let r_idx = sse.find("reasoning_content").unwrap();
        let c_idx = sse.find("Paris is sunny").unwrap();
        assert!(r_idx < c_idx);
    }

    #[test]
    fn sse_defaults_for_sparse_completion() {
        let sse = synthesize_sse(&json!({}), false);
        assert!(sse.contains("chatcmpl-obleth-boon"));
        assert!(sse.contains("\"finish_reason\":\"stop\""));
        assert!(sse.ends_with("data: [DONE]\n\n"));
    }

    #[test]
    fn check_returns_canonical_json() {
        let schema = json!({ "type": "object", "required": ["a"] });
        let out = check(Some(&schema), "here: {\"a\": 1} done").unwrap();
        assert_eq!(out, "{\"a\":1}");
    }

    #[test]
    fn check_collects_validation_errors() {
        let schema = json!({ "type": "object", "required": ["a"] });
        let errors = check(Some(&schema), "{\"b\": 2}").unwrap_err();
        assert!(!errors.is_empty());
    }

    #[test]
    fn check_without_schema_is_syntactic_only() {
        assert!(check(None, "{\"anything\": true}").is_ok());
        assert!(check(None, "not json").is_err());
    }
}
