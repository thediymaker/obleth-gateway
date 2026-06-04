//! Tiny-model intent classifier for `auto` routing.
//!
//! When enabled and configured, an `auto` request is first shown to a small,
//! fast model (the "brain", e.g. a sub-1B model) whose only job is to map the
//! prompt to one or more routing tags from the fixed vocabulary. Those tags
//! bias [`crate::router::select_model`] toward the best-matched model.
//!
//! The classifier is deliberately defensive: it is hard-timeout bounded, caches
//! results, and returns an empty tag list on any error so an `auto` request is
//! never blocked or failed because the brain is slow or down. Callers fall back
//! to cheap heuristics (see [`crate::router::heuristic_tags`]) in that case.

use std::hash::{Hash, Hasher};
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use moka::future::Cache;
use obleth_config::{AutoRouterSettings, ResolvedModel};

/// Hot-swappable classifier configuration plus a short-lived result cache.
#[derive(Clone)]
pub struct Classifier {
    settings: Arc<ArcSwap<AutoRouterSettings>>,
    /// hash(prompt + available tags) -> classified tags.
    cache: Cache<u64, Vec<String>>,
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

    /// Classify `prompt` into a subset of `available_tags` using `brain`.
    ///
    /// Returns an empty vec on timeout, transport error, or unparseable output;
    /// callers should treat empty as "no signal" and fall back to heuristics.
    pub async fn classify(
        &self,
        http: &reqwest::Client,
        brain: &ResolvedModel,
        prompt: &str,
        available_tags: &[String],
    ) -> Vec<String> {
        if available_tags.is_empty() {
            return Vec::new();
        }

        let key = cache_key(prompt, available_tags);
        if let Some(hit) = self.cache.get(&key).await {
            return hit;
        }

        let timeout = Duration::from_millis(self.settings().classifier_timeout_ms.max(1));
        let tags = match tokio::time::timeout(
            timeout,
            call_brain(http, brain, prompt, available_tags),
        )
        .await
        {
            Ok(Ok(tags)) => tags,
            Ok(Err(e)) => {
                tracing::debug!(error = %e, model = %brain.model_name, "classifier call failed");
                Vec::new()
            }
            Err(_) => {
                tracing::debug!(model = %brain.model_name, "classifier call timed out");
                Vec::new()
            }
        };

        // Cache even empty results briefly to avoid hammering a flaky brain.
        self.cache.insert(key, tags.clone()).await;
        tags
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

/// Send the constrained classification request and parse the chosen tags.
async fn call_brain(
    http: &reqwest::Client,
    brain: &ResolvedModel,
    prompt: &str,
    available_tags: &[String],
) -> anyhow::Result<Vec<String>> {
    let tag_list = available_tags.join(", ");
    let system = format!(
        "You are a routing classifier. Read the user's request and choose 1 to 3 \
         tags that best describe it, ONLY from this list: [{tag_list}]. Reply with \
         a JSON array of tag strings and nothing else, e.g. [\"coding\"]."
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
        "max_tokens": 32,
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

    Ok(extract_tags(content, available_tags))
}

fn build_chat_url(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    format!("{base}/chat/completions")
}

/// Leniently pull valid tags out of the model's reply. Accepts a JSON array or
/// any free text that mentions valid tag names; restricts to `available_tags`,
/// de-duplicates, and caps the result at 3 tags.
fn extract_tags(content: &str, available_tags: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let lower = content.to_ascii_lowercase();
    for tag in available_tags {
        if lower.contains(&tag.to_ascii_lowercase()) && !out.contains(tag) {
            out.push(tag.clone());
        }
        if out.len() >= 3 {
            break;
        }
    }
    out
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
        let got = extract_tags("[\"coding\"]", &tags());
        assert_eq!(got, vec!["coding".to_string()]);
    }

    #[test]
    fn extract_from_free_text() {
        let got = extract_tags("This looks like a math and coding task.", &tags());
        assert!(got.contains(&"coding".to_string()));
        assert!(got.contains(&"math".to_string()));
    }

    #[test]
    fn extract_ignores_unknown_tags() {
        let got = extract_tags("[\"astrology\"]", &tags());
        assert!(got.is_empty());
    }

    #[test]
    fn build_url_appends_chat_completions() {
        assert_eq!(
            build_chat_url("http://x/v1/"),
            "http://x/v1/chat/completions"
        );
    }
}
