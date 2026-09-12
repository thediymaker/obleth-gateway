//! The **image-generation** boon: lets a chat model produce images.
//!
//! A model granted this boon has a `generate_image` function tool merged into
//! its chat request. When the model calls it, the gateway executes the call
//! itself against a registered `image`-type model — no MCP server is involved,
//! exactly like the compression boon's `retrieve_original`.
//!
//! The tool's **return value** is a short text receipt, never the image. That
//! return value becomes a `role: "tool"` message that is re-dispatched to the
//! model on the next turn (`super::tool_loop::run`), so a data URL there would
//! turn one picture into megabytes of prompt tokens on every subsequent turn.
//! The image travels out of band in a `Vec<GeneratedImage>` accumulator and is
//! appended to the final assistant message as markdown.

// Scaffolding: these items are exercised by tests but have no production caller
// until the tool loop is wired up. Remove once the tool loop calls `inject` and
// other execution functions from this module (Task 6).
#![allow(dead_code)]

use std::time::Duration;

use obleth_config::{
    ImageGenerationBoonSettings, ResolvedKey, ResolvedModel, IMAGE_GENERATION_MAX_PER_REQUEST,
};
use serde_json::{json, Value};

use crate::state::AppState;

/// Name of the gateway-executed tool this boon injects.
pub(super) const GENERATE_IMAGE_TOOL: &str = "generate_image";

/// Synthetic "server" name registered in the tool map for `generate_image`, so
/// the loop recognizes the tool as gateway-owned (not a client tool to pass
/// through). Never used as a real MCP server; `execute_call` short-circuits the
/// call by name before any server lookup.
pub(super) const IMAGE_SYNTHETIC_SERVER: &str = "__image__";

/// Longest prompt excerpt echoed back to the model in a receipt.
const RECEIPT_PROMPT_MAX_CHARS: usize = 160;

/// One image produced during a request, held out of band until the final
/// assistant message is assembled.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct GeneratedImage {
    /// A data URL (`data:image/png;base64,…`) or an upstream URL.
    pub url: String,
    pub size: String,
    pub prompt: String,
}

/// The configured sizes with blanks dropped, falling back to the built-in
/// default list when an operator has cleared it.
pub(super) fn allowed_sizes(cfg: &ImageGenerationBoonSettings) -> Vec<String> {
    let sizes: Vec<String> = cfg
        .allowed_sizes
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if sizes.is_empty() {
        ImageGenerationBoonSettings::default().allowed_sizes
    } else {
        sizes
    }
}

/// The effective per-call image cap: at least 1, never above the hard ceiling.
pub(super) fn max_images(cfg: &ImageGenerationBoonSettings) -> u32 {
    cfg.max_images_per_request
        .clamp(1, IMAGE_GENERATION_MAX_PER_REQUEST)
}

/// The OpenAI function-tool definition for `generate_image`.
pub(super) fn tool_def(cfg: &ImageGenerationBoonSettings) -> Value {
    let sizes = allowed_sizes(cfg);
    json!({
        "type": "function",
        "function": {
            "name": GENERATE_IMAGE_TOOL,
            "description": cfg.tool_description,
            "parameters": {
                "type": "object",
                "properties": {
                    "prompt": {
                        "type": "string",
                        "description": "Detailed description of the image to create: \
                            subject, style, and composition.",
                    },
                    "size": {
                        "type": "string",
                        "enum": sizes,
                        "description": "Pixel dimensions of the image.",
                    },
                    "n": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": max_images(cfg),
                        "description": "How many images to generate.",
                    },
                },
                "required": ["prompt"]
            }
        }
    })
}

/// The short system nudge injected for plain chat clients that brought no tools
/// of their own.
fn nudge_text() -> &'static str {
    "You can create images. When the user asks for a picture, drawing, diagram, or \
     logo, call the `generate_image` tool with a detailed prompt instead of \
     explaining that you cannot draw. The image is attached to your reply \
     automatically."
}

/// Merge the tool definition into the request, and optionally add the system
/// nudge. Client-supplied tools are preserved: a client that brought its own
/// tools keeps them and gains this one.
pub(super) fn inject(
    cfg: &ImageGenerationBoonSettings,
    nudge: bool,
    supports_system_messages: bool,
    json_body: &mut Value,
) {
    let def = tool_def(cfg);
    if let Some(obj) = json_body.as_object_mut() {
        match obj.get_mut("tools").and_then(|v| v.as_array_mut()) {
            Some(existing) => existing.push(def),
            None => {
                obj.insert("tools".into(), Value::Array(vec![def]));
            }
        }
    }
    if nudge {
        super::structured::inject_prompt_section(
            json_body,
            nudge_text(),
            supports_system_messages,
        );
    }
}

/// `n` from the model's arguments, clamped to `1..=max_images(cfg)`. Schema
/// constraints are a hint, not a guarantee — a model that ignores the enum must
/// not be able to order eight images.
pub(super) fn clamp_n(cfg: &ImageGenerationBoonSettings, args: &Value) -> u32 {
    let requested = args.get("n").and_then(|v| v.as_u64()).unwrap_or(1);
    (requested.clamp(1, max_images(cfg) as u64)) as u32
}

/// `size` from the model's arguments when it is on the allowed list, otherwise
/// the first allowed size.
pub(super) fn clamp_size(cfg: &ImageGenerationBoonSettings, args: &Value) -> String {
    let sizes = allowed_sizes(cfg);
    let requested = args.get("size").and_then(|v| v.as_str()).unwrap_or("").trim();
    if sizes.iter().any(|s| s == requested) {
        requested.to_string()
    } else {
        sizes[0].clone()
    }
}

/// The trimmed `prompt` argument, or `None` when the model omitted it or sent
/// only whitespace.
pub(super) fn prompt_arg(args: &Value) -> Option<String> {
    args.get("prompt")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

/// Pull displayable image URLs out of an `/images/generations` response body.
/// Accepts both wire shapes: `b64_json` becomes a data URL; a backend that
/// returns `url` passes through. Mirrors `control-plane/lib/charo/images.ts`.
pub(super) fn image_urls(body: &Value) -> Vec<String> {
    let Some(data) = body.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in data {
        if let Some(b64) = item.get("b64_json").and_then(|v| v.as_str()) {
            if !b64.is_empty() {
                out.push(format!("data:image/png;base64,{b64}"));
                continue;
            }
        }
        if let Some(url) = item.get("url").and_then(|v| v.as_str()) {
            if !url.is_empty() {
                out.push(url.to_string());
            }
        }
    }
    out
}

/// Shorten a prompt for echoing back in a receipt.
fn excerpt(prompt: &str) -> String {
    let trimmed = prompt.trim();
    if trimmed.chars().count() <= RECEIPT_PROMPT_MAX_CHARS {
        return trimmed.to_string();
    }
    let mut s: String = trimmed.chars().take(RECEIPT_PROMPT_MAX_CHARS).collect();
    s.push('…');
    s
}

/// The text handed back to the model after a successful generation. Short by
/// design: this becomes a `role: "tool"` message in the model's context.
pub(super) fn receipt(count: usize, size: &str, prompt: &str) -> String {
    let plural = if count == 1 { "image" } else { "images" };
    format!(
        "Generated {count} {plural} ({size}) for prompt \"{}\". The {plural} \
         {} attached to your reply. Refer to {} in your answer, but do not \
         attempt to describe what it looks like.",
        excerpt(prompt),
        if count == 1 { "is" } else { "are" },
        if count == 1 { "it" } else { "them" },
    )
}

/// The text handed back to the model when the generation did not produce an
/// image. Reads as a fact the model can answer around, not as an error.
pub(super) fn failure_receipt(reason: &str) -> String {
    format!(
        "The image could not be generated ({reason}). No image is attached. Tell the \
         user the image generation failed; do not retry the tool."
    )
}

/// `{api_base}/images/generations`, normalised against a trailing slash.
pub(super) fn build_images_url(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    format!("{base}/images/generations")
}

/// Total attached image bytes allowed on one completion. Independent of
/// `proxy::BOON_BUFFER_MAX`, which caps the *upstream* buffer before this
/// transform runs — images are added afterwards, so both limits apply.
pub(super) const IMAGE_ATTACH_MAX_BYTES: usize = 3 * 1024 * 1024;

/// Markdown alt text for a generated image. The prompt is model-written, so a
/// stray `]` or newline in it would break the image link — strip both before
/// shortening.
fn alt_text(prompt: &str) -> String {
    let flattened: String = prompt
        .chars()
        .map(|c| match c {
            '[' | ']' | '\n' | '\r' => ' ',
            other => other,
        })
        .collect();
    excerpt(flattened.split_whitespace().collect::<Vec<_>>().join(" ").as_str())
}

/// The markdown block appended to the final assistant message, or `None` when
/// no image was generated. Over-budget images are dropped individually with a
/// visible note; the batch is never discarded wholesale.
pub(super) fn attachment(images: &[GeneratedImage]) -> Option<String> {
    if images.is_empty() {
        return None;
    }
    let mut used = 0usize;
    let mut dropped = 0usize;
    let mut lines: Vec<String> = Vec::new();
    for image in images {
        if used.saturating_add(image.url.len()) > IMAGE_ATTACH_MAX_BYTES {
            dropped += 1;
            continue;
        }
        used += image.url.len();
        lines.push(format!("![{}]({})", alt_text(&image.prompt), image.url));
    }
    if dropped > 0 {
        let plural = if dropped == 1 { "image was" } else { "images were" };
        lines.push(format!(
            "_{dropped} generated {plural} too large to include in this reply._"
        ));
    }
    if lines.is_empty() {
        return None;
    }
    Some(format!("\n\n{}", lines.join("\n\n")))
}

/// Append the generated images to `/choices/0/message/content`. Returns true
/// when the completion was modified. A completion without that pointer (an
/// error body, a malformed reply) is left byte-identical.
pub(super) fn attach_to_completion(images: &[GeneratedImage], body: &mut Value) -> bool {
    let Some(markdown) = attachment(images) else {
        return false;
    };
    let Some(message) = body
        .pointer_mut("/choices/0/message")
        .and_then(|m| m.as_object_mut())
    else {
        return false;
    };
    let existing = message
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    message.insert(
        "content".into(),
        Value::String(format!("{existing}{markdown}")),
    );
    true
}

/// One generation attempt, for the trace span. Recorded by the caller, which
/// owns the tracer (`execute` does not).
pub(super) struct ImageGenEvent {
    pub images: u32,
    pub size: String,
    pub model: String,
    pub upstream_ms: u32,
    pub ok: bool,
}

/// Everything the gateway-executed `generate_image` tool needs, threaded
/// through the tool loop for the life of one request.
pub(super) struct ImageCtx<'a> {
    /// Settings snapshot taken at request time, so a hot-reload mid-request
    /// cannot change behaviour.
    pub cfg: &'a ImageGenerationBoonSettings,
    pub key: &'a ResolvedKey,
    pub session_id: &'a str,
    /// Images produced so far — the out-of-band channel that keeps image bytes
    /// out of the model's context.
    pub images: Vec<GeneratedImage>,
    pub events: Vec<ImageGenEvent>,
}

/// Execute one `generate_image` call: clamp the arguments, POST to the
/// configured image model, push any images into the accumulator, bill them, and
/// return the short text receipt the model will read.
///
/// Fail-open throughout: every failure returns a receipt describing the failure
/// and leaves the accumulator untouched, so the model answers in prose.
pub(super) async fn execute(state: &AppState, ctx: &mut ImageCtx<'_>, args: &Value) -> String {
    let Some(prompt) = prompt_arg(args) else {
        return failure_receipt("no prompt was supplied");
    };
    let Some(model_name) = ctx.cfg.image_model.as_deref().map(str::trim) else {
        return failure_receipt("no image model is configured");
    };
    let Some(image_model) = crate::proxy::resolve_model(state, model_name).await else {
        tracing::warn!(
            model = %model_name,
            "image-generation boon target is not registered; no image produced"
        );
        return failure_receipt("the image model is not available");
    };
    if !image_model.enabled {
        tracing::warn!(
            model = %model_name,
            "image-generation boon target is disabled; no image produced"
        );
        return failure_receipt("the image model is not available");
    }

    let n = clamp_n(ctx.cfg, args);
    let size = clamp_size(ctx.cfg, args);
    // Deliberately the boon's own timeout, not the tool loop's `tool_timeout_ms`
    // (default 30s): image generation routinely takes longer than an MCP call.
    let timeout = Duration::from_millis(ctx.cfg.timeout_ms.max(1));
    let body = json!({
        "model": image_model.upstream_model,
        "prompt": prompt,
        "n": n,
        "size": size,
        "response_format": "b64_json",
    });

    let started = crate::tracer::now_ms();
    let outcome = generate(state, &image_model, body, timeout).await;
    let upstream_ms = (crate::tracer::now_ms() - started) as u32;

    match outcome {
        Ok(response) => {
            let urls = image_urls(&response);
            if urls.is_empty() {
                tracing::warn!(
                    model = %model_name,
                    "image-generation boon response carried no image data"
                );
                ctx.events.push(ImageGenEvent {
                    images: 0,
                    size,
                    model: image_model.model_name.clone(),
                    upstream_ms,
                    ok: false,
                });
                return failure_receipt("the image model returned no image");
            }
            let count = urls.len();
            for url in urls {
                ctx.images.push(GeneratedImage {
                    url,
                    size: size.clone(),
                    prompt: prompt.clone(),
                });
            }
            super::bill_image_generation(
                state,
                &image_model,
                ctx.key,
                ctx.session_id,
                count as u32,
            );
            ctx.events.push(ImageGenEvent {
                images: count as u32,
                size: size.clone(),
                model: image_model.model_name.clone(),
                upstream_ms,
                ok: true,
            });
            receipt(count, &size, &prompt)
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                model = %model_name,
                "image-generation boon call failed; answering without an image"
            );
            ctx.events.push(ImageGenEvent {
                images: 0,
                size,
                model: image_model.model_name.clone(),
                upstream_ms,
                ok: false,
            });
            failure_receipt(&e.to_string())
        }
    }
}

/// POST one generation request to the image model, bounded by `timeout`.
async fn generate(
    state: &AppState,
    model: &ResolvedModel,
    body: Value,
    timeout: Duration,
) -> anyhow::Result<Value> {
    let fut = async {
        let url = build_images_url(&model.api_base);
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
        Err(_) => anyhow::bail!("image generation timed out after {timeout:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use obleth_config::ImageGenerationBoonSettings;

    fn cfg() -> ImageGenerationBoonSettings {
        ImageGenerationBoonSettings {
            enabled: true,
            image_model: Some("sdxl".to_string()),
            max_images_per_request: 2,
            ..Default::default()
        }
    }

    #[test]
    fn tool_def_carries_the_configured_sizes_and_cap() {
        let def = tool_def(&cfg());
        assert_eq!(def["type"], "function");
        assert_eq!(def["function"]["name"], GENERATE_IMAGE_TOOL);
        let params = &def["function"]["parameters"];
        assert_eq!(params["properties"]["prompt"]["type"], "string");
        assert_eq!(params["required"][0], "prompt");
        assert_eq!(
            params["properties"]["size"]["enum"],
            serde_json::json!(["512x512", "1024x1024"])
        );
        assert_eq!(params["properties"]["n"]["minimum"], 1);
        assert_eq!(params["properties"]["n"]["maximum"], 2);
    }

    #[test]
    fn tool_def_falls_back_when_sizes_are_blank() {
        let mut c = cfg();
        c.allowed_sizes = vec!["  ".to_string()];
        let def = tool_def(&c);
        assert_eq!(
            def["function"]["parameters"]["properties"]["size"]["enum"],
            serde_json::json!(["512x512", "1024x1024"])
        );
    }

    #[test]
    fn inject_merges_into_client_tools() {
        let mut json = serde_json::json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "draw a cat" }],
            "tools": [{ "type": "function", "function": { "name": "client_tool" } }],
        });
        inject(&cfg(), false, true, &mut json);
        let tools = json["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["function"]["name"], "client_tool");
        assert_eq!(tools[1]["function"]["name"], GENERATE_IMAGE_TOOL);
    }

    #[test]
    fn inject_creates_the_tools_array_and_nudges() {
        let mut json = serde_json::json!({
            "model": "m",
            "messages": [{ "role": "user", "content": "draw a cat" }],
        });
        inject(&cfg(), true, true, &mut json);
        assert_eq!(json["tools"].as_array().unwrap().len(), 1);
        let system = json["messages"][0]["content"].as_str().unwrap_or_default();
        assert!(
            system.to_lowercase().contains("generate_image"),
            "the nudge must name the tool, got: {system}"
        );
    }

    #[test]
    fn clamping_survives_a_model_that_ignores_the_schema() {
        let c = cfg();
        // n over the cap, a size that is not on the list.
        let args = serde_json::json!({ "prompt": "x", "n": 8, "size": "4096x4096" });
        assert_eq!(clamp_n(&c, &args), 2);
        assert_eq!(clamp_size(&c, &args), "512x512");
        // n below 1, and a missing size.
        let args = serde_json::json!({ "prompt": "x", "n": 0 });
        assert_eq!(clamp_n(&c, &args), 1);
        assert_eq!(clamp_size(&c, &args), "512x512");
        // an allowed size passes through.
        let args = serde_json::json!({ "size": "1024x1024" });
        assert_eq!(clamp_size(&c, &args), "1024x1024");
    }

    #[test]
    fn max_images_is_capped_by_the_hard_ceiling() {
        let mut c = cfg();
        c.max_images_per_request = 99;
        assert_eq!(max_images(&c), obleth_config::IMAGE_GENERATION_MAX_PER_REQUEST);
        c.max_images_per_request = 0;
        assert_eq!(max_images(&c), 1);
    }

    #[test]
    fn prompt_arg_rejects_blanks() {
        assert_eq!(
            prompt_arg(&serde_json::json!({ "prompt": " a cat " })),
            Some("a cat".to_string())
        );
        assert_eq!(prompt_arg(&serde_json::json!({ "prompt": "   " })), None);
        assert_eq!(prompt_arg(&serde_json::json!({})), None);
    }

    #[test]
    fn image_urls_handles_both_wire_shapes() {
        let body = serde_json::json!({
            "data": [
                { "b64_json": "AAAA" },
                { "url": "http://img/x.png" },
                { "revised_prompt": "neither" },
                "not an object"
            ]
        });
        assert_eq!(
            image_urls(&body),
            vec![
                "data:image/png;base64,AAAA".to_string(),
                "http://img/x.png".to_string()
            ]
        );
    }

    #[test]
    fn image_urls_empty_for_a_response_with_no_data() {
        assert!(image_urls(&serde_json::json!({ "error": "nope" })).is_empty());
        assert!(image_urls(&serde_json::json!({ "data": "wrong type" })).is_empty());
    }

    #[test]
    fn receipt_never_carries_image_bytes() {
        let r = receipt(2, "1024x1024", "a cat wearing a hat");
        assert!(r.contains('2'));
        assert!(r.contains("1024x1024"));
        assert!(r.contains("a cat wearing a hat"));
        assert!(!r.contains("base64"), "the receipt must not carry image data");
        assert!(r.to_lowercase().contains("attached"));
    }

    #[test]
    fn receipt_truncates_a_long_prompt() {
        let long = "a ".repeat(500);
        let r = receipt(1, "512x512", &long);
        assert!(r.len() < 500, "receipt must stay short, got {} chars", r.len());
    }

    #[test]
    fn failure_receipt_reads_as_a_failure_not_an_error_to_recover_from() {
        let r = failure_receipt("upstream returned 503");
        assert!(r.to_lowercase().contains("could not"));
        assert!(r.contains("upstream returned 503"));
    }

    #[test]
    fn images_url_is_normalised() {
        assert_eq!(
            build_images_url("http://host:8080/v1/"),
            "http://host:8080/v1/images/generations"
        );
        assert_eq!(
            build_images_url("http://host:8080/v1"),
            "http://host:8080/v1/images/generations"
        );
    }

    fn img(url: &str) -> GeneratedImage {
        GeneratedImage {
            url: url.to_string(),
            size: "512x512".to_string(),
            prompt: "a cat".to_string(),
        }
    }

    #[test]
    fn attachment_is_none_when_nothing_was_generated() {
        assert!(attachment(&[]).is_none());
    }

    #[test]
    fn attachment_renders_one_markdown_image_per_line() {
        let md = attachment(&[img("data:image/png;base64,AAAA"), img("http://i/x.png")])
            .expect("two images produce an attachment");
        assert!(md.contains("![a cat](data:image/png;base64,AAAA)"));
        assert!(md.contains("![a cat](http://i/x.png)"));
        assert_eq!(md.matches("![").count(), 2);
    }

    #[test]
    fn alt_text_cannot_break_the_markdown_link() {
        let mut image = img("http://i/x.png");
        image.prompt = "a [cat]\nwearing a hat".to_string();
        let md = attachment(std::slice::from_ref(&image)).unwrap();
        assert_eq!(md.trim(), "![a cat wearing a hat](http://i/x.png)");
        assert_eq!(md.matches('[').count(), 1);
        assert_eq!(md.matches(']').count(), 1);
    }

    #[test]
    fn attachment_drops_the_overflow_not_the_whole_batch() {
        let big = "x".repeat(IMAGE_ATTACH_MAX_BYTES);
        let md = attachment(&[img("data:image/png;base64,AAAA"), img(&big)])
            .expect("the first image still fits");
        assert!(md.contains("AAAA"));
        assert!(!md.contains(&big));
        assert!(
            md.to_lowercase().contains("too large"),
            "the dropped image must be acknowledged, got: {md}"
        );
    }

    #[test]
    fn attach_to_completion_leaves_content_alone_when_empty() {
        let mut body = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "here you go" } }]
        });
        let before = body.clone();
        assert!(!attach_to_completion(&[], &mut body));
        assert_eq!(body, before);
    }

    #[test]
    fn attach_to_completion_appends_after_the_prose() {
        let mut body = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": "here you go" } }]
        });
        assert!(attach_to_completion(&[img("http://i/x.png")], &mut body));
        let content = body["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(content.starts_with("here you go"));
        assert!(content.contains("![a cat](http://i/x.png)"));
    }

    #[test]
    fn attach_to_completion_handles_a_null_content_message() {
        // A model that answered with only a tool call leaves `content: null`.
        let mut body = serde_json::json!({
            "choices": [{ "message": { "role": "assistant", "content": null } }]
        });
        assert!(attach_to_completion(&[img("http://i/x.png")], &mut body));
        let content = body["choices"][0]["message"]["content"].as_str().unwrap();
        assert!(content.contains("![a cat](http://i/x.png)"));
    }

    #[test]
    fn attach_to_completion_is_a_noop_on_a_malformed_completion() {
        let mut body = serde_json::json!({ "error": "upstream exploded" });
        let before = body.clone();
        assert!(!attach_to_completion(&[img("http://i/x.png")], &mut body));
        assert_eq!(body, before);
    }
}
