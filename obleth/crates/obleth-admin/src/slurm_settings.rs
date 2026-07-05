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
use axum::http::HeaderMap;
use axum::Json;
use base64::Engine;
use chrono::{DateTime, Utc};
use obleth_config::SlurmSettings;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{audit_actor, AdminError, AdminState, Result};

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
    /// Build identity the provisioner last reported on its poll. The provisioner
    /// is deployed as its own image, so this can drift from the gateway version;
    /// surfacing it makes a stale provisioner deployment obvious. Null until it
    /// has reported (or for an older provisioner that doesn't send it).
    pub provisioner_version: Option<String>,
    pub provisioner_git_sha: Option<String>,
    pub provisioner_built_at: Option<String>,
    /// Outcome of the provisioner's last reconcile tick: `ok`, `idle`, or
    /// `error`. The heartbeat above only proves the *process* is alive — a
    /// provisioner can poll green for days while every tick fails against
    /// slurmrestd and holds all replica state frozen. Null until reported (or
    /// for an older provisioner that doesn't send it).
    pub provisioner_tick_status: Option<String>,
    /// Idle reason or error text for a non-`ok` tick status.
    pub provisioner_tick_detail: Option<String>,
    /// Seconds since the last *successful* reconcile tick, or null if none is
    /// known. This — not replica `updated_at` — is what "replica states may be
    /// stale" should key off.
    pub provisioner_last_ok_secs: Option<i64>,
    /// Seconds the current non-`ok` streak has lasted, or null when the last
    /// tick was `ok` / nothing has been reported.
    pub provisioner_held_secs: Option<i64>,
}

/// Build identity the provisioner reports via request headers, stored as JSON in
/// Redis alongside the heartbeat and echoed back on the settings view.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ProvisionerBuild {
    pub version: Option<String>,
    pub git_sha: Option<String>,
    pub built_at: Option<String>,
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
            provisioner_version: None,
            provisioner_git_sha: None,
            provisioner_built_at: None,
            provisioner_tick_status: None,
            provisioner_tick_detail: None,
            provisioner_last_ok_secs: None,
            provisioner_held_secs: None,
        }
    }
}

/// The provisioner's last reconcile-tick outcome, stored as JSON in Redis
/// alongside the heartbeat and echoed back on the settings view. `last_ok_at`
/// and `since` (start of the current non-`ok` streak) are maintained across
/// writes so the dashboard can say "reconcile failing since X" and the gateway
/// can alert once the streak passes `HELD_ALERT_SECS`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionerTick {
    /// `ok` | `idle` | `error` (as reported by the provisioner).
    pub status: String,
    /// Idle reason or error text; absent for `ok`.
    #[serde(default)]
    pub detail: Option<String>,
    /// Epoch seconds of the report this blob reflects.
    pub at: i64,
    /// Epoch seconds of the last `ok` tick, carried across non-`ok` writes.
    #[serde(default)]
    pub last_ok_at: Option<i64>,
    /// Epoch seconds when the current non-`ok` streak started; 0 when `ok`.
    #[serde(default)]
    pub since: i64,
}

/// What the tick-status merge decided about alerting.
#[derive(Debug, PartialEq, Eq)]
enum TickTransition {
    None,
    /// Reconciliation has been failing/idle past the alert threshold.
    Held,
    /// A held streak just ended with a successful tick.
    Recovered,
}

/// How long reconciliation may fail/hold before the gateway raises an alert.
/// Generous vs. the 15s tick so a slurmrestd blip or restart never pages;
/// a genuinely unreachable cluster still surfaces within minutes instead of
/// silently freezing replica state for days.
const HELD_ALERT_SECS: i64 = 600;

/// Fold one tick report into the previous stored state, deciding the alert
/// transition. Pure, for tests: all clock/Redis I/O stays in the handler.
fn merge_tick(
    prev: Option<&ProvisionerTick>,
    status: &str,
    detail: Option<String>,
    now: i64,
) -> (ProvisionerTick, TickTransition) {
    if status == "ok" {
        // A held streak (past threshold) that just ended is worth a recovery
        // note; a short blip that never alerted recovers silently.
        let was_held = prev
            .filter(|p| p.status != "ok" && p.since > 0)
            .map(|p| now - p.since >= HELD_ALERT_SECS)
            .unwrap_or(false);
        let tick = ProvisionerTick {
            status: "ok".into(),
            detail: None,
            at: now,
            last_ok_at: Some(now),
            since: 0,
        };
        let transition = if was_held {
            TickTransition::Recovered
        } else {
            TickTransition::None
        };
        return (tick, transition);
    }
    // Non-ok: keep the last-known-good marker and the streak start.
    let since = prev
        .filter(|p| p.status != "ok" && p.since > 0)
        .map(|p| p.since)
        .unwrap_or(now);
    let tick = ProvisionerTick {
        status: status.to_string(),
        detail,
        at: now,
        last_ok_at: prev.and_then(|p| p.last_ok_at),
        since,
    };
    let transition = if now - since >= HELD_ALERT_SECS {
        TickTransition::Held
    } else {
        TickTransition::None
    };
    (tick, transition)
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
    if let Some(build) = state
        .redis
        .get_provisioner_version()
        .await
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<ProvisionerBuild>(&json).ok())
    {
        view.provisioner_version = build.version;
        view.provisioner_git_sha = build.git_sha;
        view.provisioner_built_at = build.built_at;
    }
    if let Some(tick) = state
        .redis
        .get_provisioner_tick_status()
        .await
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str::<ProvisionerTick>(&json).ok())
    {
        let now = Utc::now().timestamp();
        view.provisioner_last_ok_secs = tick.last_ok_at.map(|t| (now - t).max(0));
        view.provisioner_held_secs =
            (tick.status != "ok" && tick.since > 0).then(|| (now - tick.since).max(0));
        view.provisioner_tick_detail = tick.detail;
        view.provisioner_tick_status = Some(tick.status);
    }
    Ok(Json(view))
}

#[utoipa::path(
    put, path = "/api/v1/settings/slurm", tag = "settings",
    request_body = UpdateSlurmSettings,
    responses((status = 200, body = SlurmSettingsView))
)]
pub async fn put_slurm_settings(
    State(state): State<AdminState>,
    headers: HeaderMap,
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
            &audit_actor(&headers),
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
    headers: HeaderMap,
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
    // The provisioner reports its build identity via headers; persist it next to
    // the heartbeat (same TTL) so the Slurm tab can show which provisioner build
    // is running. Only stored when a version header is present.
    let header_str = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .filter(|s| !s.is_empty())
    };
    if let Some(version) = header_str("x-obleth-provisioner-version") {
        let build = ProvisionerBuild {
            version: Some(version),
            git_sha: header_str("x-obleth-provisioner-sha"),
            built_at: header_str("x-obleth-provisioner-built-at"),
        };
        if let Ok(json) = serde_json::to_string(&build) {
            if let Err(e) = state
                .redis
                .set_provisioner_version(&json, PROVISIONER_HEARTBEAT_TTL_SECS)
                .await
            {
                tracing::warn!(error = %e, "failed to record provisioner version");
            }
        }
    }
    let settings = state.store.get_slurm_settings().await?.unwrap_or_default();
    // The provisioner also reports its previous tick's outcome. Merge it with
    // the stored streak state (best-effort, like the heartbeat) and raise an
    // alert when reconciliation has been failing long enough that replica
    // state on the dashboard is effectively frozen. Gated on `enabled`: a
    // deliberately disabled Slurm idles the provisioner forever and must not
    // page anyone.
    if let Some(status) = header_str("x-obleth-provisioner-tick-status") {
        let detail = header_str("x-obleth-provisioner-tick-detail");
        let prev = state
            .redis
            .get_provisioner_tick_status()
            .await
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str::<ProvisionerTick>(&json).ok());
        let (tick, transition) = merge_tick(prev.as_ref(), &status, detail, Utc::now().timestamp());
        if settings.enabled {
            match transition {
                TickTransition::Held => state.alerts.issue(
                    "slurm_reconcile_held",
                    "Slurm reconciliation is failing",
                    format!(
                        "The provisioner is running but has not completed a reconcile tick for {} minute(s) (status `{}`): {}. \
                         Replica states shown on model pages are frozen at their last reconciled values until this clears.",
                        (Utc::now().timestamp() - tick.since).max(0) / 60,
                        tick.status,
                        tick.detail.as_deref().unwrap_or("no detail reported"),
                    ),
                ),
                TickTransition::Recovered => state.alerts.issue(
                    "slurm_reconcile_recovered",
                    "Slurm reconciliation recovered",
                    "The provisioner completed a reconcile tick after a held period; replica states are live again.".to_string(),
                ),
                TickTransition::None => {}
            }
        }
        if let Ok(json) = serde_json::to_string(&tick) {
            if let Err(e) = state
                .redis
                .set_provisioner_tick_status(&json, PROVISIONER_HEARTBEAT_TTL_SECS)
                .await
            {
                tracing::warn!(error = %e, "failed to record provisioner tick status");
            }
        }
    }
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

    // --- merge_tick (reconcile-outcome streak tracking) ---

    #[test]
    fn first_ok_tick_sets_last_ok_and_no_alert() {
        let (tick, tr) = merge_tick(None, "ok", None, 1_000);
        assert_eq!(tick.status, "ok");
        assert_eq!(tick.last_ok_at, Some(1_000));
        assert_eq!(tick.since, 0);
        assert_eq!(tr, TickTransition::None);
    }

    #[test]
    fn error_streak_preserves_last_ok_and_start_and_alerts_past_threshold() {
        let (ok_tick, _) = merge_tick(None, "ok", None, 1_000);
        // First error: streak starts now, below threshold -> no alert yet.
        let (e1, tr1) = merge_tick(Some(&ok_tick), "error", Some("boom".into()), 1_015);
        assert_eq!(e1.since, 1_015);
        assert_eq!(e1.last_ok_at, Some(1_000));
        assert_eq!(tr1, TickTransition::None);
        // Still failing 15s later: streak start must NOT move.
        let (e2, tr2) = merge_tick(Some(&e1), "error", Some("boom".into()), 1_030);
        assert_eq!(e2.since, 1_015);
        assert_eq!(tr2, TickTransition::None);
        // Past the threshold: held.
        let (e3, tr3) = merge_tick(Some(&e2), "error", Some("boom".into()), 1_015 + HELD_ALERT_SECS);
        assert_eq!(e3.since, 1_015);
        assert_eq!(e3.last_ok_at, Some(1_000));
        assert_eq!(tr3, TickTransition::Held);
    }

    #[test]
    fn ok_after_held_streak_is_recovered_ok_after_blip_is_silent() {
        let (ok_tick, _) = merge_tick(None, "ok", None, 1_000);
        let (err, _) = merge_tick(Some(&ok_tick), "error", Some("boom".into()), 1_015);

        // Short blip: recovers silently.
        let (back, tr) = merge_tick(Some(&err), "ok", None, 1_045);
        assert_eq!(tr, TickTransition::None);
        assert_eq!(back.last_ok_at, Some(1_045));

        // Long outage: recovery is announced.
        let (_, tr) = merge_tick(Some(&err), "ok", None, 1_015 + HELD_ALERT_SECS + 5);
        assert_eq!(tr, TickTransition::Recovered);
    }

    #[test]
    fn error_with_no_prior_state_starts_streak_now() {
        // Redis TTL expired / first report ever is already an error: the streak
        // starts at this observation, and there is no last-known-good.
        let (tick, tr) = merge_tick(None, "error", Some("boom".into()), 5_000);
        assert_eq!(tick.since, 5_000);
        assert_eq!(tick.last_ok_at, None);
        assert_eq!(tr, TickTransition::None);
    }

    #[test]
    fn idle_streak_is_tracked_like_error() {
        // `idle` also freezes replica state (nothing reconciles); the streak and
        // held detection apply the same way. The enabled-gate in the handler is
        // what keeps a deliberately disabled Slurm from alerting.
        let (i1, _) = merge_tick(None, "idle", Some("slurm disabled in settings".into()), 1_000);
        let (_, tr) = merge_tick(Some(&i1), "idle", Some("slurm disabled in settings".into()), 1_000 + HELD_ALERT_SECS);
        assert_eq!(tr, TickTransition::Held);
    }
}
