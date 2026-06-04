//! Runtime-reloadable alert dispatch (Slack webhook + SMTP email).
//!
//! A single `AlertDispatcher` is shared in-process by both the data plane
//! (proxy) and the management API (admin). Settings live behind an `ArcSwap`,
//! so a control-plane edit takes effect immediately without a restart. Alert
//! delivery never blocks callers: `issue` checks a cooldown and hands the work
//! to a background task.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use lettre::message::Mailbox;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use obleth_config::{AlertSettings, EmailSettings};
use reqwest::Client;

use crate::model_health::AlertSink;

/// Outcome of delivering a test alert to one channel.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
pub struct ChannelResult {
    /// `slack` or `email`.
    pub channel: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Clone)]
pub struct AlertDispatcher {
    inner: Arc<Inner>,
}

struct Inner {
    http: Client,
    settings: ArcSwap<AlertSettings>,
    last_sent: Mutex<HashMap<String, Instant>>,
}

impl AlertDispatcher {
    pub fn new(http: Client, initial: AlertSettings) -> Self {
        Self {
            inner: Arc::new(Inner {
                http,
                settings: ArcSwap::from_pointee(initial),
                last_sent: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Snapshot the current settings.
    pub fn current(&self) -> Arc<AlertSettings> {
        self.inner.settings.load_full()
    }

    /// Atomically replace the active settings (applies to in-flight alerts).
    pub fn update(&self, settings: AlertSettings) {
        self.inner.settings.store(Arc::new(settings));
    }

    /// True when at least one delivery channel is currently configured.
    pub fn enabled(&self) -> bool {
        self.inner.settings.load().any_channel_enabled()
    }

    /// Fire an alert without blocking. Deduplicates by `key` within the
    /// configured cooldown window and dispatches to every enabled channel.
    pub fn issue(
        &self,
        key: impl Into<String>,
        title: impl Into<String>,
        detail: impl Into<String>,
    ) {
        let settings = self.inner.settings.load_full();
        if !settings.any_channel_enabled() {
            return;
        }
        let key = key.into();
        if !self.should_send(&key, settings.min_interval_secs) {
            return;
        }
        let title = title.into();
        let detail = detail.into();
        let http = self.inner.http.clone();
        tokio::spawn(async move {
            deliver_all(&http, &settings, &key, &title, &detail).await;
        });
    }

    /// Send a test alert through every currently-configured channel and report
    /// the per-channel result (used by the control plane "send test" button).
    pub async fn send_test(&self) -> Vec<ChannelResult> {
        let settings = self.inner.settings.load_full();
        let key = "test_alert";
        let title = "obleth test alert";
        let detail = "This is a test alert triggered from the control plane settings page.";
        let mut results = Vec::new();

        if settings.slack_enabled() {
            let url = settings.slack_webhook_url.clone().unwrap_or_default();
            let res = post_slack(&self.inner.http, &url, key, title, detail).await;
            results.push(ChannelResult {
                channel: "slack".into(),
                ok: res.is_ok(),
                detail: res.err().unwrap_or_else(|| "delivered".into()),
            });
        }
        if settings.email_enabled() {
            if let Some(email) = settings.email.as_ref() {
                let res = send_email(email, title, detail).await;
                results.push(ChannelResult {
                    channel: "email".into(),
                    ok: res.is_ok(),
                    detail: res.err().unwrap_or_else(|| {
                        format!("delivered to {} recipient(s)", email.recipients.len())
                    }),
                });
            }
        }
        results
    }

    fn should_send(&self, key: &str, min_interval_secs: u64) -> bool {
        let Ok(mut last_sent) = self.inner.last_sent.lock() else {
            return true;
        };
        let now = Instant::now();
        let min_interval = Duration::from_secs(min_interval_secs);
        if let Some(last) = last_sent.get(key) {
            if now.duration_since(*last) < min_interval {
                return false;
            }
        }
        last_sent.insert(key.to_string(), now);
        true
    }
}

impl AlertSink for AlertDispatcher {
    fn issue(&self, key: String, title: String, detail: String) {
        AlertDispatcher::issue(self, key, title, detail);
    }
}

async fn deliver_all(
    http: &Client,
    settings: &AlertSettings,
    key: &str,
    title: &str,
    detail: &str,
) {
    if settings.slack_enabled() {
        if let Some(url) = settings.slack_webhook_url.as_ref() {
            if let Err(e) = post_slack(http, url, key, title, detail).await {
                tracing::warn!(error = %e, "slack alert delivery failed");
            }
        }
    }
    if settings.email_enabled() {
        if let Some(email) = settings.email.as_ref() {
            if let Err(e) = send_email(email, title, &format!("Issue: {key}\n\n{detail}")).await {
                tracing::warn!(error = %e, "email alert delivery failed");
            }
        }
    }
}

async fn post_slack(
    http: &Client,
    webhook_url: &str,
    key: &str,
    title: &str,
    detail: &str,
) -> Result<(), String> {
    let text = format!("*obleth alert*: {title}\n*Issue*: `{key}`\n{detail}");
    let response = http
        .post(webhook_url)
        .json(&serde_json::json!({ "text": text }))
        .send()
        .await
        .map_err(|e| e.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        Err(format!("slack returned {status}: {body}"))
    }
}

async fn send_email(email: &EmailSettings, subject: &str, body: &str) -> Result<(), String> {
    let from: Mailbox = email
        .from_address
        .parse()
        .map_err(|e| format!("invalid from address: {e}"))?;

    let mut builder = Message::builder().from(from).subject(subject);
    for rcpt in &email.recipients {
        let mbox: Mailbox = rcpt
            .parse()
            .map_err(|e| format!("invalid recipient `{rcpt}`: {e}"))?;
        builder = builder.to(mbox);
    }
    let message = builder
        .body(body.to_string())
        .map_err(|e| format!("failed to build email: {e}"))?;

    let mut transport = if email.starttls {
        AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&email.smtp_host)
            .map_err(|e| format!("smtp setup failed: {e}"))?
    } else {
        AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&email.smtp_host)
    }
    .port(email.smtp_port);

    if let (Some(user), Some(pass)) = (email.username.as_ref(), email.password.as_ref()) {
        if !user.is_empty() {
            transport = transport.credentials(Credentials::new(user.clone(), pass.clone()));
        }
    }

    transport
        .build()
        .send(message)
        .await
        .map_err(|e| format!("smtp send failed: {e}"))?;
    Ok(())
}
