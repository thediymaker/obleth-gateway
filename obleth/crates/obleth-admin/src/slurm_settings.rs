//! System-wide Slurm provisioning settings.
//!
//! Holds the slurmrestd connection details the optional `obleth-provisioner`
//! plugin uses, persisted in Postgres (the `slurm` row in `app_settings`) so an
//! operator can configure Slurm from the dashboard instead of env vars. The JWT
//! is encrypted at rest by the store layer; it is **masked** on the public
//! `GET`/`PUT` routes and only returned in full on the provisioner-facing
//! `resolved` route (which the admin token already gates).

use std::time::{Duration, Instant};

use axum::extract::State;
use axum::Json;
use base64::Engine;
use chrono::{DateTime, Utc};
use obleth_config::SlurmSettings;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{AdminError, AdminState, Result};

/// Masked view of the saved Slurm settings for the dashboard. The JWT is never
/// returned; its presence and last 4 chars are surfaced instead.
#[derive(Debug, Serialize, ToSchema)]
pub struct SlurmSettingsView {
    pub enabled: bool,
    pub slurmrestd_url: String,
    pub slurmrestd_api_version: String,
    pub slurm_user: String,
    pub jwt_set: bool,
    pub jwt_last4: Option<String>,
    /// Seconds since the provisioner last polled the gateway, or null if it has
    /// not been seen since this gateway process started. The provisioner is a
    /// separate plugin process — when it isn't running, Slurm can be "enabled"
    /// here yet nothing ever launches, so the dashboard surfaces this.
    pub provisioner_last_seen_secs: Option<i64>,
    /// True when the provisioner has polled within the freshness window.
    pub provisioner_running: bool,
}

impl SlurmSettingsView {
    fn from_settings(s: &SlurmSettings) -> Self {
        let jwt = s.slurm_jwt.trim();
        let jwt_last4 = if jwt.len() >= 4 {
            Some(jwt[jwt.len() - 4..].to_string())
        } else {
            None
        };
        SlurmSettingsView {
            enabled: s.enabled,
            slurmrestd_url: s.slurmrestd_url.clone(),
            slurmrestd_api_version: s.slurmrestd_api_version.clone(),
            slurm_user: s.slurm_user.clone(),
            jwt_set: !jwt.is_empty(),
            jwt_last4,
            // Filled in by the GET handler, which has the heartbeat; the masked
            // view itself only knows the persisted settings.
            provisioner_last_seen_secs: None,
            provisioner_running: false,
        }
    }
}

/// How recently the provisioner must have polled to count as "running". The
/// provisioner polls every `OBLETH_PROVISIONER_INTERVAL_SECS` (default 15s);
/// this window allows several missed ticks before we report it as down.
const PROVISIONER_FRESH_SECS: i64 = 60;

/// TTL for the stored heartbeat. Longer than the freshness window so the
/// dashboard can still show "last polled 5m ago" for a recently-stopped
/// provisioner, while a truly-gone one eventually drops to "never".
const PROVISIONER_HEARTBEAT_TTL_SECS: u64 = 3600;

/// Derive the provisioner status from its last-seen heartbeat. Returns
/// `(seconds_since_last_seen, running)`. A non-positive `last_seen` epoch means
/// "never seen since startup" → `(None, false)`.
fn provisioner_status(last_seen: i64, now: i64, fresh_secs: i64) -> (Option<i64>, bool) {
    if last_seen <= 0 {
        return (None, false);
    }
    let secs = now - last_seen;
    (Some(secs), secs <= fresh_secs)
}

/// Update payload. `slurm_jwt` is write-only: omit/empty to keep the stored JWT,
/// or send a new value to replace it.
#[derive(Debug, Deserialize, ToSchema)]
pub struct UpdateSlurmSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub slurmrestd_url: String,
    #[serde(default)]
    pub slurmrestd_api_version: Option<String>,
    #[serde(default)]
    pub slurm_user: String,
    /// New JWT. Empty/omitted keeps the existing stored value.
    #[serde(default)]
    pub slurm_jwt: Option<String>,
}

/// Result of the "test connection" probe: JWT expiry + a slurmrestd ping.
#[derive(Debug, Serialize, ToSchema)]
pub struct SlurmHealthView {
    pub jwt: SlurmJwtHealth,
    pub ping: SlurmPingHealth,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SlurmJwtHealth {
    /// Whether a JWT is configured at all.
    pub set: bool,
    /// True when the JWT carries an `exp` claim that is in the past.
    pub expired: bool,
    /// Expiry time, if the JWT carries a readable `exp` claim.
    pub expires_at: Option<DateTime<Utc>>,
    /// Seconds until expiry (negative if already expired).
    pub expires_in_secs: Option<i64>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SlurmPingHealth {
    /// True when slurmrestd answered with a 2xx.
    pub ok: bool,
    /// HTTP status code, when a response was received.
    pub status_code: Option<u16>,
    /// Round-trip time of the ping, when a response was received.
    pub latency_ms: Option<u64>,
    /// Transport/usage error, when the ping could not be completed.
    pub error: Option<String>,
}

/// Decode a JWS-compact JWT's `exp` claim without verifying the signature (we
/// only have the cluster's token, not its signing secret — this is for an expiry
/// display, not authentication). Tolerates padded or unpadded base64url.
fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.trim().split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    claims.get("exp").and_then(|e| e.as_i64())
}

fn jwt_health(jwt: &str) -> SlurmJwtHealth {
    let jwt = jwt.trim();
    if jwt.is_empty() {
        return SlurmJwtHealth {
            set: false,
            expired: false,
            expires_at: None,
            expires_in_secs: None,
        };
    }
    match jwt_exp(jwt) {
        Some(exp) => {
            let now = Utc::now().timestamp();
            SlurmJwtHealth {
                set: true,
                expired: exp <= now,
                expires_at: DateTime::from_timestamp(exp, 0),
                expires_in_secs: Some(exp - now),
            }
        }
        None => SlurmJwtHealth {
            set: true,
            expired: false,
            expires_at: None,
            expires_in_secs: None,
        },
    }
}

/// Restrict the slurmrestd API version to a single URL path segment of
/// letters/digits/dots (e.g. `v0.0.40`) — it's interpolated unescaped into
/// the ping/job URLs, so this rules out `/`, `..`, and other path-altering
/// characters reaching slurmrestd requests.
fn is_valid_api_version(v: &str) -> bool {
    !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || c == '.')
}

async fn ping_slurm(s: &SlurmSettings) -> SlurmPingHealth {
    let url = format!(
        "{}/slurm/{}/ping",
        s.slurmrestd_url.trim_end_matches('/'),
        s.slurmrestd_api_version
    );
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            return SlurmPingHealth {
                ok: false,
                status_code: None,
                latency_ms: None,
                error: Some(e.to_string()),
            }
        }
    };
    let started = Instant::now();
    let res = client
        .get(&url)
        .header("X-SLURM-USER-NAME", &s.slurm_user)
        .header("X-SLURM-USER-TOKEN", &s.slurm_jwt)
        .send()
        .await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match res {
        Ok(r) => SlurmPingHealth {
            ok: r.status().is_success(),
            status_code: Some(r.status().as_u16()),
            latency_ms: Some(latency_ms),
            error: None,
        },
        Err(e) => SlurmPingHealth {
            ok: false,
            status_code: None,
            latency_ms: Some(latency_ms),
            error: Some(e.to_string()),
        },
    }
}

#[utoipa::path(
    get, path = "/api/v1/settings/slurm", tag = "settings",
    responses((status = 200, body = SlurmSettingsView))
)]
pub async fn get_slurm_settings(
    State(state): State<AdminState>,
) -> Result<Json<SlurmSettingsView>> {
    let settings = state.store.get_slurm_settings().await?.unwrap_or_default();
    let mut view = SlurmSettingsView::from_settings(&settings);
    // Best-effort: a Redis hiccup shouldn't fail the settings page, just leave
    // the status unknown (rendered as "never"/"not detected").
    let last_seen = state
        .redis
        .get_provisioner_heartbeat()
        .await
        .ok()
        .flatten()
        .unwrap_or(0);
    let (secs, running) =
        provisioner_status(last_seen, Utc::now().timestamp(), PROVISIONER_FRESH_SECS);
    view.provisioner_last_seen_secs = secs;
    view.provisioner_running = running;
    Ok(Json(view))
}

#[utoipa::path(
    put, path = "/api/v1/settings/slurm", tag = "settings",
    request_body = UpdateSlurmSettings,
    responses((status = 200, body = SlurmSettingsView))
)]
pub async fn put_slurm_settings(
    State(state): State<AdminState>,
    Json(body): Json<UpdateSlurmSettings>,
) -> Result<Json<SlurmSettingsView>> {
    let existing = state.store.get_slurm_settings().await?.unwrap_or_default();

    let slurmrestd_url = body.slurmrestd_url.trim().to_string();
    let slurm_user = body.slurm_user.trim().to_string();
    if body.enabled {
        if slurmrestd_url.is_empty() {
            return Err(AdminError::BadRequest(
                "slurmrestd_url is required when Slurm is enabled".into(),
            ));
        }
        if slurm_user.is_empty() {
            return Err(AdminError::BadRequest(
                "slurm_user is required when Slurm is enabled".into(),
            ));
        }
    }

    // JWT: replace when a non-empty value is supplied, otherwise keep existing.
    let slurm_jwt = match body.slurm_jwt.as_deref().map(str::trim) {
        Some(j) if !j.is_empty() => j.to_string(),
        _ => existing.slurm_jwt.clone(),
    };

    let api_version = body
        .slurmrestd_api_version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_string())
        .unwrap_or_else(|| {
            if existing.slurmrestd_api_version.is_empty() {
                "v0.0.40".to_string()
            } else {
                existing.slurmrestd_api_version.clone()
            }
        });
    if !is_valid_api_version(&api_version) {
        return Err(AdminError::BadRequest(
            "slurmrestd_api_version must look like 'v0.0.40' (letters, digits, dots only)".into(),
        ));
    }

    let settings = SlurmSettings {
        enabled: body.enabled,
        slurmrestd_url,
        slurmrestd_api_version: api_version,
        slurm_user,
        slurm_jwt,
    };

    state.store.put_slurm_settings(&settings).await?;
    state
        .store
        .record_audit(
            "admin",
            "set_slurm_settings",
            "settings",
            "slurm",
            serde_json::json!({
                "enabled": settings.enabled,
                "slurmrestd_url": settings.slurmrestd_url,
                "slurmrestd_api_version": settings.slurmrestd_api_version,
                "slurm_user": settings.slurm_user,
                "jwt_set": !settings.slurm_jwt.is_empty(),
            }),
        )
        .await?;
    Ok(Json(SlurmSettingsView::from_settings(&settings)))
}

#[utoipa::path(
    post, path = "/api/v1/settings/slurm/test", tag = "settings",
    responses((status = 200, body = SlurmHealthView))
)]
pub async fn test_slurm_settings(State(state): State<AdminState>) -> Result<Json<SlurmHealthView>> {
    let settings = state.store.get_slurm_settings().await?.unwrap_or_default();
    if settings.slurmrestd_url.trim().is_empty() {
        return Err(AdminError::BadRequest(
            "slurmrestd_url is not configured".into(),
        ));
    }
    let jwt = jwt_health(&settings.slurm_jwt);
    let ping = ping_slurm(&settings).await;
    Ok(Json(SlurmHealthView { jwt, ping }))
}

#[utoipa::path(
    get, path = "/api/v1/settings/slurm/resolved", tag = "settings",
    responses((status = 200, body = SlurmSettings))
)]
pub async fn get_slurm_settings_resolved(
    State(state): State<AdminState>,
) -> Result<Json<SlurmSettings>> {
    // Provisioner-facing: returns the decrypted JWT in full. Still behind the
    // admin token, which already grants full read/write — so this exposes nothing
    // a holder couldn't otherwise obtain; it just keeps the UI GET secret-free.
    //
    // This route is hit only by the provisioner, once per reconcile tick, so we
    // treat each call as a heartbeat: record the time so the dashboard can show
    // whether the provisioner process is actually running. Best-effort — a Redis
    // error must not stop the provisioner from fetching its settings.
    if let Err(e) = state
        .redis
        .set_provisioner_heartbeat(Utc::now().timestamp(), PROVISIONER_HEARTBEAT_TTL_SECS)
        .await
    {
        tracing::warn!(error = %e, "failed to record provisioner heartbeat");
    }
    let settings = state.store.get_slurm_settings().await?.unwrap_or_default();
    Ok(Json(settings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;

    fn make_jwt(exp: i64) -> String {
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(br#"{"alg":"HS256","typ":"JWT"}"#);
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(format!(r#"{{"exp":{exp},"sun":"obleth"}}"#).as_bytes());
        format!("{header}.{payload}.signature")
    }

    #[test]
    fn parses_exp_claim() {
        let token = make_jwt(1_700_000_000);
        assert_eq!(jwt_exp(&token), Some(1_700_000_000));
    }

    #[test]
    fn missing_or_garbage_token_has_no_exp() {
        assert_eq!(jwt_exp("not-a-jwt"), None);
        assert_eq!(jwt_exp(""), None);
    }

    #[test]
    fn empty_jwt_health_is_unset() {
        let h = jwt_health("");
        assert!(!h.set);
        assert!(!h.expired);
        assert!(h.expires_at.is_none());
    }

    #[test]
    fn expired_jwt_is_flagged() {
        let past = Utc::now().timestamp() - 3600;
        let h = jwt_health(&make_jwt(past));
        assert!(h.set);
        assert!(h.expired);
        assert!(h.expires_in_secs.unwrap() < 0);
    }

    #[test]
    fn api_version_rejects_path_traversal_and_separators() {
        assert!(is_valid_api_version("v0.0.40"));
        assert!(!is_valid_api_version("../admin"));
        assert!(!is_valid_api_version("v0.0.40/jobs"));
        assert!(!is_valid_api_version(""));
        assert!(!is_valid_api_version("v0 0.40"));
    }

    #[test]
    fn valid_jwt_not_expired() {
        let future = Utc::now().timestamp() + 3600;
        let h = jwt_health(&make_jwt(future));
        assert!(h.set);
        assert!(!h.expired);
        assert!(h.expires_in_secs.unwrap() > 0);
    }

    #[test]
    fn provisioner_never_seen_is_not_running() {
        let (secs, running) = provisioner_status(0, 1_000, 60);
        assert_eq!(secs, None);
        assert!(!running);
    }

    #[test]
    fn provisioner_seen_recently_is_running() {
        // last seen 10s ago, 60s window -> running
        let (secs, running) = provisioner_status(990, 1_000, 60);
        assert_eq!(secs, Some(10));
        assert!(running);
    }

    #[test]
    fn provisioner_stale_is_not_running() {
        // last seen 120s ago, 60s window -> seen but not running
        let (secs, running) = provisioner_status(880, 1_000, 60);
        assert_eq!(secs, Some(120));
        assert!(!running);
    }

    #[test]
    fn provisioner_exactly_at_window_is_running() {
        let (secs, running) = provisioner_status(940, 1_000, 60);
        assert_eq!(secs, Some(60));
        assert!(running);
    }
}
