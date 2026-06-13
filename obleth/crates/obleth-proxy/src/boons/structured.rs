//! The **structured_output** boon: gateway-side JSON-schema enforcement for
//! models without native `response_format` support.
//!
//! Request side: the `response_format` field is stripped and replaced by an
//! injected system section instructing the model to reply with a single JSON
//! document (conforming to the schema, when one was given).
//!
//! Response side (driven by [`super::respond`]): the completion text is parsed
//! for its JSON document and validated against the schema. On failure the
//! completion is repaired via a configurable fixer model (or the request's own
//! model); on final failure the original completion passes through unchanged
//! (fail-open) with a warning header.

use serde_json::{json, Value};

/// Schemas larger than this are not validated (fail-open, prompt-only) so a
/// hostile client cannot feed the validator a pathological document.
const SCHEMA_MAX_BYTES: usize = 64 * 1024;

/// Apply the structured-output boon to a chat request in place.
///
/// Returns `None` when the request carries no `json_schema`/`json_object`
/// response format (nothing was changed). Returns `Some(schema)` when armed:
/// the schema to validate against, or `Some(None)` for a syntactic-only check
/// (`json_object`, missing schema, or an oversized schema).
pub(super) fn apply(json: &mut Value, supports_system: bool) -> Option<Option<Value>> {
    let format_type = json
        .pointer("/response_format/type")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if format_type != "json_schema" && format_type != "json_object" {
        return None;
    }

    let schema = json
        .pointer("/response_format/json_schema/schema")
        .cloned()
        .filter(|s| {
            let size = serde_json::to_string(s).map(|t| t.len()).unwrap_or(0);
            if size > SCHEMA_MAX_BYTES {
                tracing::warn!(
                    size,
                    "structured-output boon schema exceeds size cap; skipping validation"
                );
                return false;
            }
            true
        });

    let section = render_schema_prompt(schema.as_ref());
    inject_prompt_section(json, &section, supports_system);
    if let Some(obj) = json.as_object_mut() {
        obj.remove("response_format");
    }
    Some(schema)
}

/// Render the injected system section instructing the model to emit JSON.
pub(super) fn render_schema_prompt(schema: Option<&Value>) -> String {
    match schema {
        Some(schema) => format!(
            "# Response format\n\n\
             Respond with a single JSON document that conforms to the JSON Schema below. \
             Output only the JSON document — no prose, no markdown fences, nothing before \
             or after it.\n\n\
             JSON Schema: {}\n",
            serde_json::to_string(schema).unwrap_or_else(|_| "{}".into())
        ),
        None => "# Response format\n\n\
             Respond with a single valid JSON object. Output only the JSON — no prose, \
             no markdown fences, nothing before or after it.\n"
            .to_string(),
    }
}

/// Insert an instruction section into the conversation: as a new system
/// message after any leading system messages, or — for models that do not
/// support system messages — prepended to the first user message.
pub(super) fn inject_prompt_section(json: &mut Value, section: &str, supports_system: bool) {
    let Some(messages) = json.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    if supports_system {
        let insert_at = messages
            .iter()
            .take_while(|m| m.get("role").and_then(|r| r.as_str()) == Some("system"))
            .count();
        messages.insert(insert_at, json!({ "role": "system", "content": section }));
        return;
    }
    // No system-message support: fold the section into the first user message.
    for msg in messages.iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("user") {
            continue;
        }
        match msg.get_mut("content") {
            Some(Value::String(s)) => {
                *s = format!("{section}\n\n{s}");
            }
            Some(Value::Array(parts)) => {
                parts.insert(0, json!({ "type": "text", "text": section }));
            }
            _ => continue,
        }
        return;
    }
    // Degenerate request with no user message: add one carrying the section.
    messages.insert(0, json!({ "role": "user", "content": section }));
}

/// Extract the first JSON document from completion text: strips optional
/// markdown fences and tolerates prose before/after the document.
pub(super) fn extract_json(content: &str) -> Option<Value> {
    let mut text = content.trim();
    // Strip a leading ```json / ``` fence when present.
    if let Some(fence_start) = text.find("```") {
        let after = &text[fence_start + 3..];
        let body = after.strip_prefix("json").unwrap_or(after);
        if let Some(nl) = body.find('\n') {
            let inner = &body[nl + 1..];
            text = inner.split("```").next().unwrap_or(inner).trim();
        }
    }
    let start = text.find(['{', '['])?;
    // Parse the first complete JSON value, ignoring anything after it.
    serde_json::Deserializer::from_str(&text[start..])
        .into_iter::<Value>()
        .next()?
        .ok()
}

/// Validate `value` against `schema`. Returns the validator's error messages
/// on mismatch. A schema that fails to compile validates nothing (fail-open).
pub(super) fn validate(schema: &Value, value: &Value) -> Result<(), Vec<String>> {
    let validator = match jsonschema::validator_for(schema) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "structured-output schema failed to compile; skipping validation");
            return Ok(());
        }
    };
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{}: {}", e.instance_path, e))
        .take(8)
        .collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Build the chat-completions body for one repair attempt.
pub(super) fn repair_request_body(
    upstream_model: &str,
    schema: Option<&Value>,
    invalid_output: &str,
    errors: &[String],
) -> Value {
    let schema_text = schema
        .map(|s| serde_json::to_string(s).unwrap_or_else(|_| "{}".into()))
        .unwrap_or_else(|| "(none — any valid JSON object)".into());
    json!({
        "model": upstream_model,
        "messages": [
            {
                "role": "system",
                "content": "You repair JSON to conform to a JSON Schema. Output only the \
                            corrected JSON document — no prose, no markdown fences.",
            },
            {
                "role": "user",
                "content": format!(
                    "JSON Schema:\n{schema_text}\n\nInvalid output:\n{invalid_output}\n\n\
                     Validation errors:\n- {}",
                    errors.join("\n- ")
                ),
            }
        ],
        "temperature": 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema() -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string" },
                "age": { "type": "integer" }
            },
            "required": ["name", "age"],
            "additionalProperties": false
        })
    }

    // ---- request apply ----

    #[test]
    fn apply_arms_for_json_schema() {
        let mut req = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "extract: Bob, 42" }],
            "response_format": {
                "type": "json_schema",
                "json_schema": { "name": "person", "schema": schema() }
            }
        });
        let plan = apply(&mut req, true).expect("armed");
        assert_eq!(plan, Some(schema()));
        assert!(req.get("response_format").is_none());
        assert_eq!(req["messages"][0]["role"], "system");
        assert!(req["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("JSON Schema"));
    }

    #[test]
    fn apply_arms_for_json_object_without_schema() {
        let mut req = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "x" }],
            "response_format": { "type": "json_object" }
        });
        let plan = apply(&mut req, true).expect("armed");
        assert!(plan.is_none());
        assert!(req.get("response_format").is_none());
    }

    #[test]
    fn apply_ignores_text_format() {
        let mut req = json!({
            "model": "m",
            "messages": [],
            "response_format": { "type": "text" }
        });
        assert!(apply(&mut req, true).is_none());
        assert!(req.get("response_format").is_some());
    }

    #[test]
    fn oversized_schema_falls_back_to_syntactic() {
        let big = json!({ "type": "object", "description": "x".repeat(SCHEMA_MAX_BYTES) });
        let mut req = json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "x" }],
            "response_format": { "type": "json_schema", "json_schema": { "schema": big } }
        });
        let plan = apply(&mut req, true).expect("armed");
        assert!(plan.is_none());
    }

    // ---- prompt injection ----

    #[test]
    fn injects_after_existing_system_messages() {
        let mut req = json!({
            "messages": [
                { "role": "system", "content": "be helpful" },
                { "role": "user", "content": "hi" }
            ]
        });
        inject_prompt_section(&mut req, "SECTION", true);
        assert_eq!(req["messages"][0]["content"], "be helpful");
        assert_eq!(req["messages"][1]["content"], "SECTION");
        assert_eq!(req["messages"][2]["role"], "user");
    }

    #[test]
    fn folds_into_user_message_without_system_support() {
        let mut req = json!({
            "messages": [{ "role": "user", "content": "hi" }]
        });
        inject_prompt_section(&mut req, "SECTION", false);
        let content = req["messages"][0]["content"].as_str().unwrap();
        assert!(content.starts_with("SECTION"));
        assert!(content.ends_with("hi"));
    }

    #[test]
    fn folds_into_user_message_with_part_array() {
        let mut req = json!({
            "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hi" }] }]
        });
        inject_prompt_section(&mut req, "SECTION", false);
        assert_eq!(req["messages"][0]["content"][0]["text"], "SECTION");
        assert_eq!(req["messages"][0]["content"][1]["text"], "hi");
    }

    // ---- extraction ----

    #[test]
    fn extracts_bare_json() {
        let v = extract_json("{\"name\": \"Bob\", \"age\": 42}").unwrap();
        assert_eq!(v["name"], "Bob");
    }

    #[test]
    fn extracts_fenced_json() {
        let v = extract_json("```json\n{\"name\": \"Bob\", \"age\": 42}\n```").unwrap();
        assert_eq!(v["age"], 42);
    }

    #[test]
    fn extracts_json_wrapped_in_prose() {
        let v = extract_json("Sure! Here you go: {\"name\": \"Bob\", \"age\": 42} Hope it helps.")
            .unwrap();
        assert_eq!(v["name"], "Bob");
    }

    #[test]
    fn extracts_array_document() {
        let v = extract_json("[1, 2, 3]").unwrap();
        assert_eq!(v, json!([1, 2, 3]));
    }

    #[test]
    fn truncated_json_returns_none() {
        assert!(extract_json("{\"name\": \"Bob\", \"age\":").is_none());
        assert!(extract_json("no json here at all").is_none());
    }

    // ---- validation ----

    #[test]
    fn validate_passes_conforming_value() {
        let value = json!({ "name": "Bob", "age": 42 });
        assert!(validate(&schema(), &value).is_ok());
    }

    #[test]
    fn validate_reports_errors() {
        let value = json!({ "name": "Bob", "age": "forty-two", "extra": true });
        let errors = validate(&schema(), &value).unwrap_err();
        assert!(!errors.is_empty());
    }

    #[test]
    fn uncompilable_schema_fails_open() {
        let bad = json!({ "type": "not-a-real-type" });
        assert!(validate(&bad, &json!({})).is_ok());
    }

    // ---- repair body ----

    #[test]
    fn repair_body_carries_schema_output_and_errors() {
        let body = repair_request_body(
            "fixer-1b",
            Some(&schema()),
            "{\"name\": \"Bob\"}",
            &["missing required property 'age'".to_string()],
        );
        assert_eq!(body["model"], "fixer-1b");
        assert_eq!(body["temperature"], 0);
        let user = body["messages"][1]["content"].as_str().unwrap();
        assert!(user.contains("\"required\":[\"name\",\"age\"]"));
        assert!(user.contains("{\"name\": \"Bob\"}"));
        assert!(user.contains("missing required property"));
    }
}
