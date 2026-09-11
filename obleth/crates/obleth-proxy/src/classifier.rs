//! Tiny-model intent classifier for `auto` routing.
//!
//! When enabled and configured, an `auto` request is first shown to a small,
//! fast model (the "brain", e.g. a sub-1B model) whose only job is to map the
//! prompt to one or more routing tags from the fixed vocabulary. Those tags
//! bias [`crate::router::select_model`] toward the best-matched model.
//!
//! The classifier is deliberately defensive: it is hard-timeout bounded, caches
//! results, and returns an empty tag list (difficulty 1) on any error so an
//! `auto` request is never blocked or failed, and never routed more expensive,
//! because the brain is slow or down. Callers fall back to cheap heuristics
//! (see [`crate::router::heuristic_intent`]) in that case.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use moka::future::Cache;
use obleth_config::{AutoRouterSettings, ResolvedModel};

use crate::router::Intent;

/// Hot-swappable classifier configuration plus a short-lived result cache.
#[derive(Clone)]
pub struct Classifier {
    settings: Arc<ArcSwap<AutoRouterSettings>>,
    /// hash(prompt + available tags) -> classified intent.
    cache: Cache<u64, Intent>,
}

impl Classifier {
    pub fn new(initial: AutoRouterSettings) -> Self {
        Self {
            settings: Arc::new(ArcSwap::from_pointee(initial)),
            cache: Cache::builder()
                .max_capacity(10_000)
                .time_to_live(Duration::from_secs(300))
                .build(),
        }
    }

    /// Current settings snapshot (cheap `Arc` clone).
    pub fn settings(&self) -> Arc<AutoRouterSettings> {
        self.settings.load_full()
    }

    /// Replace the settings (called by the periodic refresh task).
    pub fn update(&self, settings: AutoRouterSettings) {
        self.settings.store(Arc::new(settings));
    }

    /// Classify `prompt` into a subset of `available_tags` (plus a difficulty)
    /// using `brain`.
    ///
    /// Returns `Intent::default()` (empty tags, difficulty 1) on timeout,
    /// transport error, or unparseable output; callers should treat empty tags
    /// as "no signal" and fall back to heuristics. Every failure path lowers
    /// difficulty rather than raising it.
    pub async fn classify(
        &self,
        http: &reqwest::Client,
        brain: &ResolvedModel,
        prompt: &str,
        available_tags: &[String],
    ) -> Intent {
        if available_tags.is_empty() {
            return Intent::default();
        }

        let key = cache_key(prompt, available_tags);
        if let Some(hit) = self.cache.get(&key).await {
            return hit;
        }

        let timeout = Duration::from_millis(self.settings().classifier_timeout_ms.max(1));
        let intent = match tokio::time::timeout(
            timeout,
            call_brain(http, brain, prompt, available_tags),
        )
        .await
        {
            Ok(Ok(intent)) => intent,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, model = %brain.model_name, "classifier call failed");
                Intent::default()
            }
            Err(_) => {
                tracing::debug!(model = %brain.model_name, "classifier call timed out");
                Intent::default()
            }
        };

        // Cache even empty/default results briefly to avoid hammering a flaky brain.
        self.cache.insert(key, intent.clone()).await;
        intent
    }
}

fn cache_key(prompt: &str, available_tags: &[String]) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    prompt.hash(&mut hasher);
    for t in available_tags {
        t.hash(&mut hasher);
    }
    hasher.finish()
}

/// Send the constrained classification request and parse the chosen tags and
/// difficulty.
async fn call_brain(
    http: &reqwest::Client,
    brain: &ResolvedModel,
    prompt: &str,
    available_tags: &[String],
) -> anyhow::Result<Intent> {
    let tag_list = available_tags.join(", ");
    let system = format!(
        "You are a routing classifier. Read the user's request and reply with ONLY \
         a JSON object, no prose. Choose 1 to 3 tags that best describe it from this \
         list: [{tag_list}]. Also rate how hard the request is: 1 = simple/factual, \
         2 = moderate, 3 = hard, needs careful reasoning. \
         Reply exactly like: {{\"tags\":[\"coding\"],\"difficulty\":2}}"
    );
    // Cap the prompt we forward so the classifier stays fast and cheap.
    let mut user = prompt.trim().to_string();
    if user.len() > 2_000 {
        user.truncate(2_000);
    }

    let request = serde_json::json!({
        "model": brain.upstream_model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "max_tokens": 48,
        "temperature": 0.0,
    });

    let url = build_chat_url(&brain.api_base);
    let mut req = http.post(url).json(&request);
    if let Some(key) = &brain.api_key {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("classifier upstream returned {}", resp.status());
    }
    let body: serde_json::Value = resp.json().await?;
    let content = body
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or_default();

    Ok(extract_intent(content, available_tags))
}

fn build_chat_url(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    format!("{base}/chat/completions")
}

/// Leniently pull tags and a difficulty out of the model's reply. Accepts the
/// JSON object, a bare JSON array, or free text mentioning tag names. Anything
/// unparseable yields difficulty 1 — a confused brain must make routing
/// cheaper, never more expensive.
fn extract_intent(content: &str, available_tags: &[String]) -> Intent {
    let difficulty = serde_json::from_str::<serde_json::Value>(content.trim())
        .ok()
        .and_then(|v| v.get("difficulty").and_then(|d| d.as_u64()))
        .map(|d| (d as u8).clamp(1, obleth_config::MAX_TIER_LEVEL))
        .unwrap_or(1);

    // Tag extraction is unchanged: substring match restricted to the
    // achievable vocabulary, de-duplicated, capped at 3.
    let mut tags: Vec<String> = Vec::new();
    let lower = content.to_ascii_lowercase();
    for tag in available_tags {
        if lower.contains(&tag.to_ascii_lowercase()) && !tags.contains(tag) {
            tags.push(tag.clone());
        }
        if tags.len() >= 3 {
            break;
        }
    }

    Intent {
        tags,
        difficulty,
        source: crate::router::IntentSource::Classifier,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tags() -> Vec<String> {
        vec![
            "coding".to_string(),
            "math".to_string(),
            "vision".to_string(),
        ]
    }

    #[test]
    fn extract_from_json_array() {
        let got = extract_intent("[\"coding\"]", &tags());
        assert_eq!(got.tags, vec!["coding".to_string()]);
    }

    #[test]
    fn extract_from_free_text() {
        let got = extract_intent("This looks like a math and coding task.", &tags());
        assert!(got.tags.contains(&"coding".to_string()));
        assert!(got.tags.contains(&"math".to_string()));
    }

    #[test]
    fn extract_ignores_unknown_tags() {
        let got = extract_intent("[\"astrology\"]", &tags());
        assert!(got.tags.is_empty());
    }

    #[test]
    fn extract_parses_tags_and_difficulty() {
        let got = extract_intent(r#"{"tags":["coding"],"difficulty":3}"#, &tags());
        assert_eq!(got.tags, vec!["coding".to_string()]);
        assert_eq!(got.difficulty, 3);
    }

    #[test]
    fn extract_defaults_difficulty_to_one_when_absent() {
        let got = extract_intent(r#"["coding"]"#, &tags());
        assert_eq!(got.tags, vec!["coding".to_string()]);
        assert_eq!(
            got.difficulty, 1,
            "a brain that omits difficulty must route cheap, not expensive"
        );
    }

    #[test]
    fn extract_clamps_an_out_of_range_difficulty() {
        assert_eq!(
            extract_intent(r#"{"tags":["math"],"difficulty":99}"#, &tags()).difficulty,
            3
        );
        assert_eq!(
            extract_intent(r#"{"tags":["math"],"difficulty":0}"#, &tags()).difficulty,
            1
        );
    }

    #[test]
    fn extract_on_garbage_yields_no_tags_and_difficulty_one() {
        let got = extract_intent("I'm sorry, I can't help with that.", &tags());
        assert!(got.tags.is_empty());
        assert_eq!(got.difficulty, 1);
    }

    #[test]
    fn build_url_appends_chat_completions() {
        assert_eq!(
            build_chat_url("http://x/v1/"),
            "http://x/v1/chat/completions"
        );
    }
}
