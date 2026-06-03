//! Optional Slack incoming-webhook alerts for operational failures.
//!
//! Alert delivery must never block the data plane. Calls update a small
//! in-memory cooldown map, then hand the HTTP POST to a background task.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use obleth_config::SlackAlertConfig;
use reqwest::Client;

#[derive(Clone)]
pub struct SlackAlerts {
    inner: Option<Arc<Inner>>,
}

struct Inner {
    webhook_url: String,
    http: Client,
    min_interval: Duration,
    last_sent: Mutex<HashMap<String, Instant>>,
}

impl SlackAlerts {
    pub fn from_config(config: &SlackAlertConfig, http: Client) -> Self {
        let Some(webhook_url) = config.webhook_url.as_ref() else {
            return Self { inner: None };
        };
        Self {
            inner: Some(Arc::new(Inner {
                webhook_url: webhook_url.clone(),
                http,
                min_interval: config.min_interval,
                last_sent: Mutex::new(HashMap::new()),
            })),
        }
    }

    pub fn enabled(&self) -> bool {
        self.inner.is_some()
    }

    pub fn issue(
        &self,
        key: impl Into<String>,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) {
        let Some(inner) = self.inner.as_ref().cloned() else {
            return;
        };
        let key = key.into();
        if !inner.should_send(&key) {
            return;
        }

        let title = title.into();
        let detail = detail.into();
        tokio::spawn(async move {
            inner.post(&key, &title, &detail).await;
        });
    }
}

impl obleth_admin::AlertSink for SlackAlerts {
    fn issue(&self, key: String, title: String, detail: String) {
        SlackAlerts::issue(self, key, title, detail);
    }
}

impl Inner {
    fn should_send(&self, key: &str) -> bool {
        let Ok(mut last_sent) = self.last_sent.lock() else {
            return true;
        };
        let now = Instant::now();
        if let Some(last) = last_sent.get(key) {
            if now.duration_since(*last) < self.min_interval {
                return false;
            }
        }
        last_sent.insert(key.to_string(), now);
        true
    }

    async fn post(&self, key: &str, title: &str, detail: &str) {
        let text = format!(
            "*obleth alert*: {title}\n*Issue*: `{key}`\n{detail}",
            title = title,
            key = key,
            detail = detail,
        );
        let response = self
            .http
            .post(&self.webhook_url)
            .json(&serde_json::json!({ "text": text }))
            .send()
            .await;

        match response {
            Ok(res) if res.status().is_success() => {}
            Ok(res) => {
                let status = res.status();
                let body = res.text().await.unwrap_or_default();
                tracing::warn!(%status, body = %body, "slack alert delivery failed");
            }
            Err(error) => {
                tracing::warn!(%error, "slack alert delivery failed");
            }
        }
    }
}
