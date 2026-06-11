//! Model **Boons**: gateway-side capabilities granted to models that lack them
//! natively.
//!
//! The first boon is **vision**. When a model that does not natively accept
//! images (`supports_vision == false`) receives a chat request containing
//! `image_url` content parts, the gateway relays each image to a designated
//! vision model (the "describer"), swaps the image part for the returned text
//! description, and forwards the rewritten request to the originally-requested
//! model. The target model therefore "sees" the image as text and can answer
//! as if it had vision.
//!
//! Boons are deliberately **fail-open**: any error (no describer configured,
//! upstream failure, timeout, unparseable reply) leaves the affected image — and
//! the request as a whole — unchanged. A flaky describer must never block or
//! fail a request the target model might still handle on its own.
//!
//! The engine is hot-swappable: [`BoonEngine`] holds its [`BoonSettings`] behind
//! an [`ArcSwap`] that the periodic model-registry refresh task updates, exactly
//! like [`crate::classifier::Classifier`].

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use obleth_config::{BoonSettings, ResolvedKey, ResolvedModel, UsageRecord, VisionBoonSettings};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::state::AppState;

/// Hot-swappable boon configuration shared across the data plane.
#[derive(Clone)]
pub struct BoonEngine {
    settings: Arc<ArcSwap<BoonSettings>>,
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
    /// dispatched upstream.
    ///
    /// Returns `true` when the body was rewritten and the caller must
    /// re-serialize it before forwarding; `false` when nothing changed.
    pub async fn enrich_request(
        &self,
        state: &AppState,
        route: Option<&ResolvedModel>,
        key: &ResolvedKey,
        session_id: &str,
        json: &mut Value,
    ) -> bool {
        let settings = self.settings();
        let mut rewritten = false;

        // ---- vision boon ----
        // Only when the model opted into the vision boon, lacks native vision,
        // and a describer is configured. Models that natively accept images,
        // or that haven't enabled the boon, are left untouched.
        if let Some(route) = route {
            if route.boons.iter().any(|b| b == "vision")
                && !route.supports_vision
                && settings.vision.active()
                && apply_vision_boon(state, &settings.vision, key, session_id, json).await
            {
                rewritten = true;
            }
        }

        rewritten
    }
}

/// Rewrite image content parts into text descriptions using the configured
/// describer model. Returns `true` when at least one image was described.
async fn apply_vision_boon(
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
        tokio::time::timeout(
            timeout,
            describe_image(state, &describer, &cfg.describe_prompt, url),
        )
    }))
    .await;

    // Pass 3: swap successfully described images for their text descriptions.
    let mut described = 0u32;
    for ((mi, pi, _), outcome) in targets.into_iter().zip(outcomes) {
        match outcome {
            Ok(Ok(result)) => {
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
                    bill_describe(state, &describer, key, session_id, &result);
                }
            }
            Ok(Err(e)) => {
                tracing::warn!(
                    error = %e,
                    model = %describer.model_name,
                    "vision boon describe call failed; leaving image unchanged"
                );
            }
            Err(_) => {
                tracing::warn!(
                    model = %describer.model_name,
                    "vision boon describe call timed out; leaving image unchanged"
                );
            }
        }
    }

    described > 0
}

/// Outcome of one describe call: the description text plus token usage the
/// describer reported (zero when it did not return a `usage` object).
struct DescribeResult {
    text: String,
    input_tokens: u32,
    output_tokens: u32,
}

/// Send a single image to the describer model and return its text description.
async fn describe_image(
    state: &AppState,
    describer: &ResolvedModel,
    prompt: &str,
    image_url: &str,
) -> anyhow::Result<DescribeResult> {
    let request = json!({
        "model": describer.upstream_model,
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
    });

    let url = build_chat_url(&describer.api_base);
    let mut req = state.http.post(url).json(&request);
    if let Some(api_key) = &describer.api_key {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("describer upstream returned {}", resp.status());
    }
    let body: Value = resp.json().await?;
    let text = body
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if text.trim().is_empty() {
        anyhow::bail!("describer returned an empty description");
    }
    let input_tokens = body
        .pointer("/usage/prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;
    let output_tokens = body
        .pointer("/usage/completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32;

    Ok(DescribeResult {
        text,
        input_tokens,
        output_tokens,
    })
}

/// Record the describer call against the tenant's ledger so the cost of the
/// boon is attributed and visible in the request log.
fn bill_describe(
    state: &AppState,
    describer: &ResolvedModel,
    key: &ResolvedKey,
    session_id: &str,
    result: &DescribeResult,
) {
    let total_tokens = result.input_tokens.saturating_add(result.output_tokens);
    let cost_usd = (result.input_tokens as f64) * describer.input_cost_per_token
        + (result.output_tokens as f64) * describer.output_cost_per_token;

    state
        .metrics
        .record_request("boon", 200, result.input_tokens, result.output_tokens);
    // Internal probe keys are not billed; mirror `finalize`.
    if key.internal {
        return;
    }
    state.telemetry.record(UsageRecord {
        request_id: Uuid::new_v4(),
        tenant_id: key.tenant_id,
        key_id: key.key_id,
        model: describer.model_name.clone(),
        admission: "boon".to_string(),
        weight: key.weight,
        input_tokens: result.input_tokens,
        output_tokens: result.output_tokens,
        estimated_tokens: total_tokens,
        queue_wait_ms: 0,
        ttft_ms: 0,
        total_ms: 0,
        status_code: 200,
        cache_status: "off".to_string(),
        cost_usd,
        ts_ms: now_ms(),
        session_id: session_id.to_string(),
        request_type: "vision_boon".to_string(),
    });
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

fn build_chat_url(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    format!("{base}/chat/completions")
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
