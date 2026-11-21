//! Alert settings.

use super::*;

/// Read-only view of the saved alert settings. Secrets (webhook URL, SMTP
/// password) are never returned; presence is reported via boolean flags.
#[derive(Debug, Serialize, ToSchema)]
pub struct AlertSettingsView {
    pub slack_webhook_set: bool,
    pub min_interval_secs: u64,
    pub email: Option<EmailSettingsView>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct EmailSettingsView {
    pub smtp_host: String,
    pub smtp_port: u16,
    pub username: Option<String>,
    pub password_set: bool,
    pub from_address: String,
    pub recipients: Vec<String>,
    pub starttls: bool,
}

impl AlertSettingsView {
    fn from_settings(s: &AlertSettings) -> Self {
        AlertSettingsView {
            slack_webhook_set: s.slack_enabled(),
            min_interval_secs: s.min_interval_secs,
            email: s.email.as_ref().map(|e| EmailSettingsView {
                smtp_host: e.smtp_host.clone(),
                smtp_port: e.smtp_port,
                username: e.username.clone(),
                password_set: e.password.as_ref().is_some_and(|p| !p.is_empty()),
                from_address: e.from_address.clone(),
                recipients: e.recipients.clone(),
                starttls: e.starttls,
            }),
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateAlertSettings {
    /// New Slack webhook URL. Empty/omitted leaves the existing value untouched
    /// unless `clear_slack_webhook` is set.
    #[serde(default)]
    pub slack_webhook_url: Option<String>,
    #[serde(default)]
    pub clear_slack_webhook: bool,
    /// Cooldown between repeat alerts for the same key. Omitted keeps current.
    #[serde(default)]
    pub min_interval_secs: Option<u64>,
    /// Email delivery config. `null`/omitted disables email alerts.
    #[serde(default)]
    pub email: Option<UpdateEmailSettings>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateEmailSettings {
    pub smtp_host: String,
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    #[serde(default)]
    pub username: Option<String>,
    /// New SMTP password. Empty/omitted keeps the existing value unless
    /// `clear_smtp_password` is set.
    #[serde(default)]
    pub smtp_password: Option<String>,
    #[serde(default)]
    pub clear_smtp_password: bool,
    pub from_address: String,
    #[serde(default)]
    pub recipients: Vec<String>,
    #[serde(default = "default_true_bool")]
    pub starttls: bool,
}

fn default_smtp_port() -> u16 {
    587
}

fn default_true_bool() -> bool {
    true
}

#[utoipa::path(
    get, path = "/api/v1/settings/alerts", tag = "settings",
    responses((status = 200, body = AlertSettingsView))
)]
pub(crate) async fn get_alert_settings(
    State(state): State<AdminState>,
) -> Result<Json<AlertSettingsView>> {
    let settings = state.store.get_alert_settings().await?.unwrap_or_default();
    Ok(Json(AlertSettingsView::from_settings(&settings)))
}

#[utoipa::path(
    put, path = "/api/v1/settings/alerts", tag = "settings",
    request_body = UpdateAlertSettings,
    responses((status = 200, body = AlertSettingsView))
)]
pub(crate) async fn put_alert_settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
    Json(body): Json<UpdateAlertSettings>,
) -> Result<Json<AlertSettingsView>> {
    let existing = state.store.get_alert_settings().await?.unwrap_or_default();

    // Slack webhook: set / keep / clear.
    let slack_webhook_url = match body.slack_webhook_url.as_deref().map(str::trim) {
        Some(url) if !url.is_empty() => {
            // Alert dispatch POSTs to this URL from the gateway; hold it to the
            // same destination policy as registered upstreams.
            state.ssrf.validate(url).await?;
            Some(url.to_string())
        }
        _ if body.clear_slack_webhook => None,
        _ => existing.slack_webhook_url.clone(),
    };

    let min_interval_secs = body.min_interval_secs.unwrap_or(existing.min_interval_secs);

    // Email block: present => build (carrying over the password unless changed),
    // absent => disabled.
    let email = match body.email {
        None => None,
        Some(upd) => {
            if upd.smtp_host.trim().is_empty() {
                return Err(AdminError::BadRequest("smtp_host is required".into()));
            }
            if upd.from_address.trim().is_empty() {
                return Err(AdminError::BadRequest("from_address is required".into()));
            }
            let prev_password = existing.email.as_ref().and_then(|e| e.password.clone());
            let password = match upd.smtp_password.as_deref().map(str::trim) {
                Some(p) if !p.is_empty() => Some(p.to_string()),
                _ if upd.clear_smtp_password => None,
                _ => prev_password,
            };
            Some(EmailSettings {
                smtp_host: upd.smtp_host.trim().to_string(),
                smtp_port: upd.smtp_port,
                username: upd
                    .username
                    .map(|u| u.trim().to_string())
                    .filter(|u| !u.is_empty()),
                password,
                from_address: upd.from_address.trim().to_string(),
                recipients: upd
                    .recipients
                    .into_iter()
                    .map(|r| r.trim().to_string())
                    .filter(|r| !r.is_empty())
                    .collect(),
                starttls: upd.starttls,
            })
        }
    };

    let settings = AlertSettings {
        slack_webhook_url,
        email,
        min_interval_secs,
    };

    state.store.put_alert_settings(&settings).await?;
    state.alerts.update(settings.clone());
    publish_setting(&state, SavedSetting::Alerts).await;
    state
        .audit(
            &headers,
            "set_alert_settings",
            "settings",
            "alerts",
            serde_json::json!({
                "slack_enabled": settings.slack_enabled(),
                "email_enabled": settings.email_enabled(),
                "min_interval_secs": settings.min_interval_secs,
            }),
        )
        .await?;
    Ok(Json(AlertSettingsView::from_settings(&settings)))
}

#[derive(Debug, Serialize, ToSchema)]
pub struct TestAlertResult {
    pub results: Vec<ChannelResult>,
}

#[utoipa::path(
    post, path = "/api/v1/settings/alerts/test", tag = "settings",
    responses((status = 200, body = TestAlertResult))
)]
pub(crate) async fn test_alert_settings(
    State(state): State<AdminState>,
) -> Result<Json<TestAlertResult>> {
    if !state.alerts.enabled() {
        return Err(AdminError::BadRequest(
            "no alert channels are configured".into(),
        ));
    }
    let results = state.alerts.send_test().await;
    Ok(Json(TestAlertResult { results }))
}
