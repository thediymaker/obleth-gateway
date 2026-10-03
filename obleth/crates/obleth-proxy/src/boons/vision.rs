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
use uuid::Uuid;

use crate::state::AppState;

/// What the boon did to a request's images.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct VisionOutcome {
    /// Images swapped for the describer's text.
    pub described: u32,
    /// Images that couldn't be described and were swapped for a short note
    /// saying so.
    pub noted: u32,
}

/// Rewrite image content parts into text descriptions using the configured
/// describer model.
///
/// The boon only runs for a model that can't read images, so no image part is
/// ever forwarded: one that can't be described -- the describer is missing or
/// disabled, times out or errors, or the image is past `max_images` -- becomes
/// a note saying an image was there. The model can then tell the user, where a
/// raw image part would fail the whole request ("is not a multimodal model").
pub(super) async fn apply(
    state: &AppState,
    cfg: &VisionBoonSettings,
    key: &ResolvedKey,
    session_id: &str,
    request_id: Uuid,
    json: &mut Value,
) -> VisionOutcome {
    let Some(model_name) = cfg.fallback_model.as_deref() else {
        return VisionOutcome::default();
    };
    // Cheap pre-check: bail before resolving the describer if there is nothing
    // to describe.
    let images = image_parts(json);
    if images.is_empty() {
        return VisionOutcome::default();
    }
    let describer = match crate::proxy::resolve_model(state, model_name).await {
        Some(d) if d.enabled => d,
        found => {
            tracing::warn!(
                model = %model_name,
                registered = found.is_some(),
                "vision boon describer is not registered or is disabled; noting the images instead"
            );
            let notes = images
                .iter()
                .map(|(mi, pi, _)| ((*mi, *pi), Swap::Note(NOTE_NO_DESCRIBER.into())));
            return swap_parts(json, notes);
        }
    };

    let timeout = Duration::from_millis(cfg.timeout_ms.max(1));
    // The first `max_images` are described; the rest only get a note.
    let cap = cfg.max_images as usize;
    let (targets, past_cap) = images.split_at(images.len().min(cap));

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

    // Pass 3: swap every image for its description, or for a note saying why
    // there isn't one.
    let mut swaps = Vec::with_capacity(images.len());
    for ((mi, pi, _), outcome) in targets.iter().zip(outcomes) {
        match outcome {
            Ok(result) => {
                super::bill_helper_call(
                    state,
                    &describer,
                    key,
                    session_id,
                    request_id,
                    "vision_boon",
                    result.input_tokens,
                    result.output_tokens,
                );
                swaps.push(((*mi, *pi), Swap::Described(result.text)));
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    model = %describer.model_name,
                    "vision boon describe call failed; noting the image instead"
                );
                let why = if e.to_string().contains("timed out") {
                    NOTE_TIMED_OUT
                } else {
                    NOTE_FAILED
                };
                swaps.push(((*mi, *pi), Swap::Note(why.into())));
            }
        }
    }
    let note = format!("only the first {cap} images in a request are described");
    swaps.extend(
        past_cap
            .iter()
            .map(|(mi, pi, _)| ((*mi, *pi), Swap::Note(note.clone()))),
    );
    swap_parts(json, swaps)
}

const NOTE_NO_DESCRIBER: &str = "the image describer isn't available";
const NOTE_TIMED_OUT: &str = "the image describer didn't answer in time";
const NOTE_FAILED: &str = "the image describer returned an error";

/// What goes in place of one image part.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Swap {
    /// The describer's text.
    Described(String),
    /// Why there is no description, for a note in the image's place.
    Note(String),
}

/// Every `image_url` part with a url: (message index, part index, url).
fn image_parts(json: &Value) -> Vec<(usize, usize, String)> {
    let Some(messages) = json.get("messages").and_then(|m| m.as_array()) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for (mi, msg) in messages.iter().enumerate() {
        let Some(parts) = msg.get("content").and_then(|c| c.as_array()) else {
            continue;
        };
        for (pi, part) in parts.iter().enumerate() {
            if part.get("type").and_then(|t| t.as_str()) != Some("image_url") {
                continue;
            }
            if let Some(url) = part
                .get("image_url")
                .and_then(|u| u.get("url"))
                .and_then(|u| u.as_str())
            {
                found.push((mi, pi, url.to_string()));
            }
        }
    }
    found
}

/// Replace image parts in place, by (message, part) index, with text.
fn swap_parts(
    json: &mut Value,
    swaps: impl IntoIterator<Item = ((usize, usize), Swap)>,
) -> VisionOutcome {
    let mut outcome = VisionOutcome::default();
    for ((mi, pi), swap) in swaps {
        let Some(part) = json
            .get_mut("messages")
            .and_then(|m| m.as_array_mut())
            .and_then(|m| m.get_mut(mi))
            .and_then(|msg| msg.get_mut("content"))
            .and_then(|c| c.as_array_mut())
            .and_then(|p| p.get_mut(pi))
        else {
            continue;
        };
        let text = match swap {
            Swap::Described(text) => {
                outcome.described += 1;
                format!("[Image description: {}]", text.trim())
            }
            Swap::Note(why) => {
                outcome.noted += 1;
                format!("[An image was attached here, but it couldn't be described: {why}.]")
            }
        };
        *part = json!({ "type": "text", "text": text });
    }
    outcome
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
        assert_eq!(
            image_parts(&req_with_image()),
            vec![(1, 1, "data:image/png;base64,AAAA".to_string())]
        );
    }

    #[test]
    fn no_image_when_text_only() {
        let json = json!({
            "model": "minimax",
            "messages": [ { "role": "user", "content": "hello" } ]
        });
        assert!(image_parts(&json).is_empty());
    }

    #[test]
    fn no_image_when_no_messages() {
        assert!(image_parts(&json!({ "model": "minimax" })).is_empty());
    }

    fn no_image_parts_left(json: &Value) -> bool {
        image_parts(json).is_empty()
    }

    #[test]
    fn a_described_image_becomes_its_description() {
        let mut json = req_with_image();
        let out = swap_parts(
            &mut json,
            [((1, 1), Swap::Described(" a red error toast \n".into()))],
        );
        assert_eq!(
            out,
            VisionOutcome {
                described: 1,
                noted: 0
            }
        );
        assert_eq!(
            json["messages"][1]["content"][1]["text"],
            "[Image description: a red error toast]"
        );
        assert_eq!(
            json["messages"][1]["content"][0]["text"], "what is this?",
            "text parts are kept"
        );
        assert!(no_image_parts_left(&json));
    }

    #[test]
    fn an_image_that_cant_be_described_becomes_a_note_not_a_raw_image() {
        // The case that failed the request: the describer timed out and the raw
        // image went on to a model that rejects image parts.
        let mut json = req_with_image();
        let out = swap_parts(&mut json, [((1, 1), Swap::Note(NOTE_TIMED_OUT.into()))]);
        assert_eq!(
            out,
            VisionOutcome {
                described: 0,
                noted: 1
            }
        );
        let text = json["messages"][1]["content"][1]["text"].as_str().unwrap();
        assert!(text.contains("couldn't be described"), "{text}");
        assert!(text.contains("didn't answer in time"), "{text}");
        assert_eq!(json["messages"][1]["content"][1]["type"], "text");
        assert!(no_image_parts_left(&json));
    }

    #[test]
    fn a_swap_for_a_part_that_isnt_there_is_skipped() {
        let mut json = req_with_image();
        let out = swap_parts(&mut json, [((7, 0), Swap::Note("x".into()))]);
        assert_eq!(out, VisionOutcome::default());
        assert_eq!(image_parts(&json).len(), 1);
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
