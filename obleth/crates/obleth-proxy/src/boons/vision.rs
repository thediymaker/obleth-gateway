//! The **vision** boon: image-to-text relay for models without native vision.
//!
//! When a model that does not natively accept images (`supports_vision ==
//! false`) receives a chat request containing `image_url` content parts, the
//! gateway relays each image to a designated vision model (the "describer"),
//! swaps the image part for the returned text description, and forwards the
//! rewritten request to the originally-requested model. The target model
//! therefore "sees" the image as text and can answer as if it had vision.

use std::time::Duration;

use obleth_config::{ResolvedKey, VisionBoonSettings};
use serde_json::{json, Value};

use crate::state::AppState;

/// Rewrite image content parts into text descriptions using the configured
/// describer model. Returns `true` when at least one image was described.
pub(super) async fn apply(
    state: &AppState,
    cfg: &VisionBoonSettings,
    key: &ResolvedKey,
    session_id: &str,
    json: &mut Value,
) -> bool {
    let Some(model_name) = cfg.fallback_model.as_deref() else {
        return false;
    };
    // Cheap pre-check: bail before resolving the describer if there is nothing
    // to describe.
    if !has_image(json) {
        return false;
    }
    let Some(describer) = crate::proxy::resolve_model(state, model_name).await else {
        tracing::warn!(
            model = %model_name,
            "vision boon describer is not registered; forwarding request unchanged"
        );
        return false;
    };
    if !describer.enabled {
        tracing::warn!(
            model = %model_name,
            "vision boon describer is disabled; forwarding request unchanged"
        );
        return false;
    }

    let timeout = Duration::from_millis(cfg.timeout_ms.max(1));

    // Pass 1: collect the image parts to describe (message/part indices plus
    // the url), bounded by `max_images`.
    let mut targets: Vec<(usize, usize, String)> = Vec::new();
    {
        let Some(messages) = json.get("messages").and_then(|m| m.as_array()) else {
            return false;
        };
        'collect: for (mi, msg) in messages.iter().enumerate() {
            let Some(parts) = msg.get("content").and_then(|c| c.as_array()) else {
                continue;
            };
            for (pi, part) in parts.iter().enumerate() {
                if targets.len() >= cfg.max_images as usize {
                    break 'collect;
                }
                if part.get("type").and_then(|t| t.as_str()) != Some("image_url") {
                    continue;
                }
                if let Some(url) = part
                    .get("image_url")
                    .and_then(|u| u.get("url"))
                    .and_then(|u| u.as_str())
                {
                    targets.push((mi, pi, url.to_string()));
                }
            }
        }
    }
    if targets.is_empty() {
        return false;
    }

    // Pass 2: describe all images concurrently (the set is already bounded by
    // `max_images`), so total added latency is one round trip, not one per image.
    let outcomes = futures_util::future::join_all(targets.iter().map(|(_, _, url)| {
        super::chat_call(
            state,
            &describer,
            describe_request(&describer.upstream_model, &cfg.describe_prompt, url),
            timeout,
        )
    }))
    .await;

    // Pass 3: swap successfully described images for their text descriptions.
    let mut described = 0u32;
    for ((mi, pi, _), outcome) in targets.into_iter().zip(outcomes) {
        match outcome {
            Ok(result) => {
                let part = json
                    .get_mut("messages")
                    .and_then(|m| m.as_array_mut())
                    .and_then(|m| m.get_mut(mi))
                    .and_then(|msg| msg.get_mut("content"))
                    .and_then(|c| c.as_array_mut())
                    .and_then(|p| p.get_mut(pi));
                if let Some(part) = part {
                    *part = json!({
                        "type": "text",
                        "text": format!("[Image description: {}]", result.text.trim()),
                    });
                    described += 1;
                    super::bill_helper_call(
                        state,
                        &describer,
                        key,
                        session_id,
                        "vision_boon",
                        result.input_tokens,
                        result.output_tokens,
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    model = %describer.model_name,
                    "vision boon describe call failed; leaving image unchanged"
                );
            }
        }
    }

    described > 0
}

/// The chat-completions body sent to the describer for a single image.
fn describe_request(upstream_model: &str, prompt: &str, image_url: &str) -> Value {
    json!({
        "model": upstream_model,
        "messages": [
            {
                "role": "user",
                "content": [
                    { "type": "text", "text": prompt },
                    { "type": "image_url", "image_url": { "url": image_url } },
                ],
            }
        ],
        "temperature": 0.2,
    })
}

/// True when any message carries an `image_url` content part.
fn has_image(json: &Value) -> bool {
    let Some(messages) = json.get("messages").and_then(|m| m.as_array()) else {
        return false;
    };
    messages.iter().any(|msg| {
        msg.get("content")
            .and_then(|c| c.as_array())
            .is_some_and(|parts| {
                parts
                    .iter()
                    .any(|p| p.get("type").and_then(|t| t.as_str()) == Some("image_url"))
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req_with_image() -> Value {
        json!({
            "model": "minimax",
            "messages": [
                { "role": "system", "content": "be helpful" },
                {
                    "role": "user",
                    "content": [
                        { "type": "text", "text": "what is this?" },
                        { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
                    ]
                }
            ]
        })
    }

    #[test]
    fn detects_image_parts() {
        assert!(has_image(&req_with_image()));
    }

    #[test]
    fn no_image_when_text_only() {
        let json = json!({
            "model": "minimax",
            "messages": [ { "role": "user", "content": "hello" } ]
        });
        assert!(!has_image(&json));
    }

    #[test]
    fn no_image_when_no_messages() {
        assert!(!has_image(&json!({ "model": "minimax" })));
    }

    #[test]
    fn describe_request_shape() {
        let body = describe_request("llava:13b", "describe it", "http://img/x.png");
        assert_eq!(body["model"], "llava:13b");
        assert_eq!(body["messages"][0]["content"][0]["text"], "describe it");
        assert_eq!(
            body["messages"][0]["content"][1]["image_url"]["url"],
            "http://img/x.png"
        );
    }
}
