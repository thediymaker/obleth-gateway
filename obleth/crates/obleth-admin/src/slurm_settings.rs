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
use chrono::{DateTime, Utc};
use obleth_config::slurm as slurm_values;
use obleth_config::{ClusterDefaults, NodeAlias, SlurmSettings};
use obleth_provisioner::slurm::{self as slurm_client, SlurmErrorKind, Slurmrestd};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{audit_actor, AdminError, AdminState, Result};

/// Masked view of the saved Slurm settings for the dashboard. The JWT and the
/// Hugging Face token are never returned; their presence and last 4 chars are
/// surfaced instead.
#[derive(Debug, Serialize, ToSchema)]
pub struct SlurmSettingsView {
    pub enabled: bool,
    pub slurmrestd_url: String,
    pub slurmrestd_api_version: String,
    pub slurm_user: String,
    pub jwt_set: bool,
    pub jwt_last4: Option<String>,
    /// Operator-supplied node hostname → IP overrides. Echoed back in full (no
    /// secret) so the dashboard can render and edit the list.
    pub node_aliases: Vec<NodeAlias>,
    /// Cluster-wide paths, setup lines and engine images for job scripts.
    pub cluster_defaults: ClusterDefaults,
    /// Whether a Hugging Face token is stored.
    pub hf_token_set: bool,
    pub hf_token_last4: Option<String>,
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
    /// When the saved token expires, read from its `exp` claim.
    pub jwt_expires_at: Option<DateTime<Utc>>,
    /// Seconds until the token expires (negative once expired).
    pub jwt_expires_in_secs: Option<i64>,
    /// The Slurm user the token was minted for (its `sun` claim).
    pub jwt_user: Option<String>,
    /// The API version in use: the configured one, or what `auto` resolved
    /// to when last checked. Null when not known yet.
    pub resolved_api_version: Option<String>,
    /// API versions the server offered when last checked.
    pub available_api_versions: Vec<String>,
    /// The Slurm release, when last checked.
    pub slurm_release: Option<String>,
    /// False when `OBLETH_ENCRYPTION_KEY` is unset and tokens are stored in
    /// plaintext.
    pub stored_encrypted: bool,
    /// Warnings from the provisioner's last tick, as plain sentences.
    pub provisioner_warnings: Vec<String>,
    /// Problems with the saved settings, as plain sentences with what to do.
    pub warnings: Vec<String>,
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
        let hf_token = s.hf_token.trim();
        SlurmSettingsView {
            enabled: s.enabled,
            slurmrestd_url: s.slurmrestd_url.clone(),
            slurmrestd_api_version: s.slurmrestd_api_version.clone(),
            slurm_user: s.slurm_user.clone(),
            jwt_set: !jwt.is_empty(),
            jwt_last4: last4(jwt),
            node_aliases: s.node_aliases.clone(),
            cluster_defaults: s.cluster_defaults.clone(),
            hf_token_set: !hf_token.is_empty(),
            hf_token_last4: last4(hf_token),
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
            jwt_expires_at: None,
            jwt_expires_in_secs: None,
            jwt_user: None,
            resolved_api_version: None,
            available_api_versions: Vec::new(),
            slurm_release: None,
            stored_encrypted: true,
            provisioner_warnings: Vec::new(),
            warnings: Vec::new(),
        }
        .with_token_and_discovery(s)
    }

    /// Fill the token claims and the last discovery for these settings.
    fn with_token_and_discovery(mut self, s: &SlurmSettings) -> Self {
        let jwt = jwt_health(&s.slurm_jwt);
        self.jwt_expires_at = jwt.expires_at;
        self.jwt_expires_in_secs = jwt.expires_in_secs;
        self.jwt_user = slurm_values::slurm_jwt_claims(&s.slurm_jwt).and_then(|c| c.sun);
        let found = recall_discovery(&s.slurmrestd_url, &s.slurm_user);
        let pinned = s.slurmrestd_api_version.trim();
        self.resolved_api_version = if slurm_values::is_auto_api_version(pinned) {
            found.as_ref().and_then(|d| d.resolved.clone())
        } else {
            match found.as_ref() {
                // A pin the server doesn't serve: the provisioner uses the
                // newest version it does serve.
                Some(d) if !d.available.is_empty() && !d.available.iter().any(|v| v == pinned) => {
                    slurm_values::pick_api_version(&d.available)
                }
                _ => Some(pinned.to_string()),
            }
        };
        if let Some(d) = found {
            self.available_api_versions = d.available;
            self.slurm_release = d.release;
        }
        self
    }
}

/// What the last connection test (or resource read) learned about a server.
#[derive(Debug, Clone, Default)]
struct Discovery {
    url: String,
    user: String,
    available: Vec<String>,
    resolved: Option<String>,
    release: Option<String>,
}

static DISCOVERY: std::sync::Mutex<Option<Discovery>> = std::sync::Mutex::new(None);

fn remember_discovery(d: Discovery) {
    if let Ok(mut slot) = DISCOVERY.lock() {
        *slot = Some(d);
    }
}

/// Merge what a resource read learned (version in use, release) into the
/// stored discovery for the same server.
pub(crate) fn remember_resolved(url: &str, user: &str, version: &str, release: Option<String>) {
    if let Ok(mut slot) = DISCOVERY.lock() {
        let mut d = slot
            .take()
            .filter(|d| d.url == url.trim() && d.user == user.trim())
            .unwrap_or_else(|| Discovery {
                url: url.trim().to_string(),
                user: user.trim().to_string(),
                ..Default::default()
            });
        d.resolved = Some(version.to_string());
        if release.is_some() {
            d.release = release;
        }
        *slot = Some(d);
    }
}

fn recall_discovery(url: &str, user: &str) -> Option<Discovery> {
    DISCOVERY
        .lock()
        .ok()?
        .clone()
        .filter(|d| d.url == url.trim() && d.user == user.trim())
}

/// How long a token may have left before saving it warns.
const SHORT_TOKEN_SECS: i64 = 7 * 86_400;

/// The command that mints a 30-day token, for messages.
const MINT_TOKEN: &str = "`scontrol token lifespan=2592000`";

/// Problems with the saved settings, as plain sentences with what to do.
/// Pure: the clock and the encryption state are passed in.
fn settings_warnings(s: &SlurmSettings, now: i64, stored_encrypted: bool) -> Vec<String> {
    let mut out = Vec::new();
    let jwt = s.slurm_jwt.trim();
    if jwt.is_empty() {
        if s.enabled {
            out.push(
                "Slurm is on but no token is saved, so slurmrestd will refuse requests unless it \
                 trusts this host. Paste the output of `scontrol token lifespan=2592000`."
                    .to_string(),
            );
        }
        return out;
    }
    let claims = slurm_values::slurm_jwt_claims(jwt);
    if let Some(exp) = claims.as_ref().and_then(|c| c.exp) {
        let left = exp - now;
        if left <= 0 {
            let at = DateTime::from_timestamp(exp, 0)
                .map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_default();
            out.push(format!(
                "This token expired at {at}. Mint a new one with {MINT_TOKEN} for 30 days."
            ));
        } else if left < SHORT_TOKEN_SECS {
            out.push(format!(
                "This token expires in {}. Mint one with {MINT_TOKEN} for 30 days.",
                slurm_client::plain_duration(left)
            ));
        }
    }
    if let Some(sun) = claims.as_ref().and_then(|c| c.sun.as_deref()) {
        let user = s.slurm_user.trim();
        if !user.is_empty() && sun != user {
            out.push(format!(
                "The token's user ({sun}) is not the Slurm user you entered ({user})."
            ));
        }
    }
    if !stored_encrypted {
        out.push(
            "OBLETH_ENCRYPTION_KEY is not set, so the Slurm token is stored without encryption."
                .to_string(),
        );
    }
    out
}

/// Last four characters of a secret, or `None` when it is shorter than that.
/// By chars, not bytes: a byte slice panics on a multibyte suffix.
fn last4(secret: &str) -> Option<String> {
    (secret.chars().count() >= 4).then(|| {
        let mut tail: Vec<char> = secret.chars().rev().take(4).collect();
        tail.reverse();
        tail.into_iter().collect::<String>()
    })
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
    /// Warnings the provisioner reported with the tick.
    #[serde(default)]
    pub warnings: Vec<String>,
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
            warnings: Vec::new(),
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
        warnings: Vec::new(),
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

/// Update payload. Every field is optional: an omitted field keeps its stored
/// value, and an explicit empty string clears a text field. `slurm_jwt` is
/// write-only: omit it or send an empty string to keep the stored token, or
/// send a new one (as `scontrol token` prints it, with or without
/// `SLURM_JWT=` / `export`) to replace it. `hf_token` is write-only too, but
/// can be cleared: omit/null keeps it, `""` clears it, any other value
/// replaces it. `node_aliases` and `cluster_defaults` replace the stored
/// values when present.
#[derive(Debug, Default, Deserialize, ToSchema)]
pub struct UpdateSlurmSettings {
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub slurmrestd_url: Option<String>,
    /// `auto` (or empty) uses the newest version the server offers; or a
    /// version such as `v0.0.43`.
    #[serde(default)]
    pub slurmrestd_api_version: Option<String>,
    #[serde(default)]
    pub slurm_user: Option<String>,
    /// New JWT. Empty/omitted keeps the existing stored value.
    #[serde(default)]
    pub slurm_jwt: Option<String>,
    /// Full replacement set of node hostname → IP overrides. Blank rows are
    /// dropped; a non-blank host with a non-IP address is rejected.
    #[serde(default)]
    pub node_aliases: Option<Vec<NodeAlias>>,
    /// Full replacement of the cluster defaults. Omitted/null keeps the stored
    /// block.
    #[serde(default)]
    pub cluster_defaults: Option<ClusterDefaults>,
    /// New Hugging Face token. Omitted/null keeps the stored one; an empty
    /// string clears it.
    #[serde(default)]
    pub hf_token: Option<String>,
}

/// Apply the `hf_token` PUT semantics: omitted/null keeps the stored token,
/// empty (or whitespace-only) clears it, anything else replaces it.
fn resolve_hf_token(update: Option<&str>, existing: &str) -> String {
    match update.map(str::trim) {
        None => existing.to_string(),
        Some(t) => t.to_string(),
    }
}

/// Tidy a submitted cluster-defaults block: trim the single-line fields and
/// the image map, dropping rows whose engine name or image is blank. `setup`
/// is multi-line shell and kept as written, minus trailing whitespace.
fn normalize_cluster_defaults(d: &ClusterDefaults) -> ClusterDefaults {
    ClusterDefaults {
        cache_dir: d.cache_dir.trim().to_string(),
        images_dir: d.images_dir.trim().to_string(),
        log_dir: d.log_dir.trim().to_string(),
        setup: d.setup.trim_end().to_string(),
        images: d
            .images
            .iter()
            .filter_map(|(k, v)| {
                let (k, v) = (k.trim(), v.trim());
                (!k.is_empty() && !v.is_empty()).then(|| (k.to_string(), v.to_string()))
            })
            .collect(),
    }
}

/// Result of the "test connection" probe.
#[derive(Debug, Serialize, ToSchema)]
pub struct SlurmHealthView {
    pub jwt: SlurmJwtHealth,
    pub ping: SlurmPingHealth,
    pub api: SlurmApiHealth,
    pub auth: SlurmAuthHealth,
    /// `UP` or `DOWN` from the ping; null when the ping did not say.
    pub slurmctld: Option<String>,
    pub slurm_release: Option<String>,
    /// The Slurm user the token was minted for (its `sun` claim).
    pub jwt_user: Option<String>,
    /// Every problem found, as a plain sentence with what to do. Empty when
    /// the connection works.
    pub problems: Vec<String>,
}

/// Which API version is in use and whether the server serves it.
#[derive(Debug, Serialize, ToSchema)]
pub struct SlurmApiHealth {
    /// The saved setting (`auto` or a version).
    pub configured: String,
    /// The version used for the checks below.
    pub resolved: Option<String>,
    /// Every version the server offers, oldest first.
    pub available: Vec<String>,
    pub ok: bool,
    pub error: Option<String>,
}

/// Whether an authenticated read (the partition list) worked.
#[derive(Debug, Serialize, ToSchema)]
pub struct SlurmAuthHealth {
    pub ok: bool,
    pub error: Option<String>,
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
    /// True when slurmrestd answered, the token was accepted and the
    /// controller is up.
    pub ok: bool,
    /// HTTP status code, when a response was received.
    pub status_code: Option<u16>,
    /// Round-trip time of the ping, when a response was received.
    pub latency_ms: Option<u64>,
    /// Transport/usage error, when the ping could not be completed.
    pub error: Option<String>,
}

/// A JWT's `exp` claim, read without verifying the signature (for display
/// and warnings, not authentication).
fn jwt_exp(token: &str) -> Option<i64> {
    slurm_values::slurm_jwt_claims(token)?.exp
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

/// `auto` (or empty) or a version such as `v0.0.43`. The version is
/// interpolated into request paths, so nothing else is accepted.
fn is_valid_api_version(v: &str) -> bool {
    slurm_values::is_valid_api_version_setting(v)
}

/// The test's verdict on the API version, from what the server offers.
fn api_health(
    configured: &str,
    discovered: &std::result::Result<Vec<String>, String>,
) -> SlurmApiHealth {
    let auto = slurm_values::is_auto_api_version(configured);
    let available = discovered.as_ref().cloned().unwrap_or_default();
    let configured = if auto {
        slurm_values::SLURM_API_VERSION_AUTO.to_string()
    } else {
        configured.trim().to_string()
    };
    let (resolved, error) = if auto {
        match discovered {
            Err(e) => (None, Some(e.clone())),
            Ok(av) => match slurm_values::pick_api_version(av) {
                Some(v) => (Some(v), None),
                None => (None, Some(slurm_client::no_supported_version_message(av))),
            },
        }
    } else if !available.is_empty() && !available.contains(&configured) {
        // The provisioner falls back to the newest supported version the
        // server serves, so the test checks that one too and names it.
        let fallback = slurm_values::pick_api_version(&available);
        let uses = fallback
            .as_ref()
            .map(|v| format!(" obleth uses {v} until you set the API version to auto or to {v}."))
            .unwrap_or_else(|| " Set the API version to auto.".to_string());
        (
            fallback,
            Some(format!(
                "This slurmrestd does not serve API version {configured}; it serves {}.{uses}",
                available.join(", ")
            )),
        )
    } else {
        (Some(configured.clone()), None)
    };
    SlurmApiHealth {
        ok: resolved.is_some() && error.is_none(),
        configured,
        resolved,
        available,
        error,
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
        view.provisioner_warnings = tick.warnings;
    }
    view.stored_encrypted = state.store.cipher_enabled();
    view.warnings = settings_warnings(&settings, Utc::now().timestamp(), view.stored_encrypted);
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

    // Partial update: an omitted field keeps its stored value.
    let enabled = body.enabled.unwrap_or(existing.enabled);
    let slurmrestd_url = body
        .slurmrestd_url
        .as_deref()
        .map_or_else(|| existing.slurmrestd_url.clone(), |u| u.trim().to_string());
    let slurm_user = body
        .slurm_user
        .as_deref()
        .map_or_else(|| existing.slurm_user.clone(), |u| u.trim().to_string());
    if enabled {
        if slurmrestd_url.is_empty() {
            return Err(AdminError::BadRequest(
                "Enter the slurmrestd URL before turning Slurm on.".into(),
            ));
        }
        if slurm_user.is_empty() {
            return Err(AdminError::BadRequest(
                "Enter the Slurm user before turning Slurm on.".into(),
            ));
        }
        // The provisioner and status probes send the JWT to this URL; hold it
        // to the same destination policy as registered upstreams. The test
        // route re-checks before every ping, so a disabled draft is safe.
        state.ssrf.validate(&slurmrestd_url).await?;
    }

    // JWT: replace when a non-empty value is supplied, otherwise keep existing.
    // A token pasted as `scontrol token` prints it loses the `SLURM_JWT=`.
    let slurm_jwt = match body
        .slurm_jwt
        .as_deref()
        .map(slurm_values::clean_pasted_jwt)
    {
        Some(j) if !j.is_empty() => j,
        _ => existing.slurm_jwt.clone(),
    };

    let api_version = match body.slurmrestd_api_version.as_deref().map(str::trim) {
        Some(v) if !v.is_empty() => v.to_string(),
        Some(_) => slurm_values::SLURM_API_VERSION_AUTO.to_string(),
        None if existing.slurmrestd_api_version.trim().is_empty() => {
            slurm_values::SLURM_API_VERSION_AUTO.to_string()
        }
        None => existing.slurmrestd_api_version.clone(),
    };
    if !is_valid_api_version(&api_version) {
        return Err(AdminError::BadRequest(
            "The API version must be auto or look like v0.0.43.".into(),
        ));
    }

    // Node aliases: drop fully-blank rows the UI may submit; a host with no IP
    // (or vice versa) is a mistake worth surfacing, and the address must be a
    // real IP literal: a hostname here would push the flaky DNS lookup back
    // onto the proxy, defeating the point of the override.
    let node_aliases = match &body.node_aliases {
        None => existing.node_aliases.clone(),
        Some(list) => {
            let mut out = Vec::with_capacity(list.len());
            for a in list {
                let host = a.host.trim();
                let ip = a.ip.trim();
                if host.is_empty() && ip.is_empty() {
                    continue;
                }
                if host.is_empty() {
                    return Err(AdminError::BadRequest(
                        "A node alias has an address but no node name.".into(),
                    ));
                }
                if ip.parse::<std::net::IpAddr>().is_err() {
                    return Err(AdminError::BadRequest(format!(
                        "The address for node {host} must be an IP address, not \"{ip}\"."
                    )));
                }
                out.push(NodeAlias {
                    host: host.to_string(),
                    ip: ip.to_string(),
                });
            }
            out
        }
    };

    let cluster_defaults = match &body.cluster_defaults {
        Some(d) => normalize_cluster_defaults(d),
        None => existing.cluster_defaults.clone(),
    };
    let hf_token = resolve_hf_token(body.hf_token.as_deref(), &existing.hf_token);

    let settings = SlurmSettings {
        enabled,
        slurmrestd_url,
        slurmrestd_api_version: api_version,
        slurm_user,
        slurm_jwt,
        node_aliases,
        cluster_defaults,
        hf_token,
    };
    if settings.slurmrestd_url != existing.slurmrestd_url
        || settings.slurm_user != existing.slurm_user
        || settings.slurm_jwt != existing.slurm_jwt
        || settings.slurmrestd_api_version != existing.slurmrestd_api_version
    {
        slurm_client::forget_resolved_api_versions();
    }

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
                "node_aliases": settings.node_aliases.len(),
                // Paths and image names only: `setup` is free-form shell that
                // may carry credentials, so only its size is recorded.
                "cluster_defaults": {
                    "cache_dir": settings.cluster_defaults.cache_dir,
                    "images_dir": settings.cluster_defaults.images_dir,
                    "log_dir": settings.cluster_defaults.log_dir,
                    "setup_lines": settings.cluster_defaults.setup.lines().count(),
                    "images": settings.cluster_defaults.images,
                },
                "hf_token_set": !settings.hf_token.is_empty(),
            }),
        )
        .await?;
    // Turning Slurm off stops the provisioner from managing jobs that are
    // still running; say who did it.
    if existing.enabled && !settings.enabled {
        let live = state
            .store
            .all_replicas()
            .await
            .map(|rs| rs.iter().filter(|r| r.state != "lost").count())
            .unwrap_or(0);
        if live > 0 {
            state.alerts.issue(
                "slurm_turned_off",
                "Slurm was turned off with replicas running",
                format!(
                    "{} turned Slurm off while {live} replica{} were running. The provisioner \
                     stops managing them: their jobs keep running until they end or are \
                     cancelled on the cluster. Turn Slurm back on to manage them again.",
                    audit_actor(&headers),
                    if live == 1 { "" } else { "s" }
                ),
            );
        }
    }
    let mut view = SlurmSettingsView::from_settings(&settings);
    view.stored_encrypted = state.store.cipher_enabled();
    view.warnings = settings_warnings(&settings, Utc::now().timestamp(), view.stored_encrypted);
    Ok(Json(view))
}

#[utoipa::path(
    post, path = "/api/v1/settings/slurm/test", tag = "settings",
    responses((status = 200, body = SlurmHealthView))
)]
pub async fn test_slurm_settings(State(state): State<AdminState>) -> Result<Json<SlurmHealthView>> {
    let settings = state.store.get_slurm_settings().await?.unwrap_or_default();
    let url = settings.slurmrestd_url.trim().to_string();
    if url.is_empty() {
        return Err(AdminError::BadRequest(
            "Enter the slurmrestd URL first.".into(),
        ));
    }
    // Re-checked here: a URL stored before the policy applied, or restored from
    // a backup, must not receive the JWT.
    state.ssrf.validate(&url).await?;
    let jwt = jwt_health(&settings.slurm_jwt);
    let jwt_user = slurm_values::slurm_jwt_claims(&settings.slurm_jwt).and_then(|c| c.sun);
    let http = crate::ssrf::upstream_client_builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|e| AdminError::Internal(e.to_string()))?;
    let user = settings.slurm_user.trim();
    let token = settings.slurm_jwt.trim();

    let mut problems: Vec<String> = Vec::new();
    if !jwt.set {
        problems.push(
            "No Slurm token is saved. Paste the output of `scontrol token lifespan=2592000` into the \
             token field."
                .to_string(),
        );
    } else if jwt.expired {
        problems.push(format!(
            "The token has expired. Mint a new one with {MINT_TOKEN} for 30 days."
        ));
    }
    if let (Some(sun), false) = (jwt_user.as_deref(), user.is_empty()) {
        if sun != user {
            problems.push(format!(
                "The token's user ({sun}) is not the Slurm user you entered ({user})."
            ));
        }
    }

    let discovered = slurm_client::discover_api_versions(&http, &url, user, token).await;
    let mut auth_error: Option<String> = None;
    let discovered = discovered.map(|d| d.available).map_err(|e| {
        if e.kind == SlurmErrorKind::TokenRejected {
            auth_error = Some(e.message.clone());
        }
        e.message
    });
    let api = api_health(&settings.slurmrestd_api_version, &discovered);
    if let Some(e) = api.error.as_ref().filter(|_| auth_error.is_none()) {
        problems.push(e.clone());
    }

    let mut ping = SlurmPingHealth {
        ok: false,
        status_code: None,
        latency_ms: None,
        error: None,
    };
    let mut slurmctld = None;
    let mut slurm_release = None;
    if let Some(version) = api.resolved.as_deref() {
        let client = Slurmrestd::new(http.clone(), &url, version, user, token);
        let started = Instant::now();
        match client.ping().await {
            Ok(p) => {
                ping.status_code = Some(200);
                slurmctld = p.slurmctld;
                slurm_release = p.slurm_release;
            }
            Err(e) => {
                ping.status_code = e.status;
                ping.error = Some(e.message);
            }
        }
        ping.latency_ms = Some(started.elapsed().as_millis() as u64);
        if auth_error.is_none() {
            if let Err(e) = client.check_token().await {
                auth_error = Some(e.message);
            }
        }
    }
    if let Some(e) = &ping.error {
        if !problems.contains(e) && auth_error.as_deref() != Some(e) {
            problems.push(e.clone());
        }
    }
    if let Some(e) = &auth_error {
        problems.push(e.clone());
    }
    if slurmctld.as_deref() == Some("DOWN") {
        problems.push(
            "slurmrestd answered, but the Slurm controller (slurmctld) is not responding, so \
             jobs cannot be submitted. Check slurmctld on the cluster."
                .to_string(),
        );
    }
    let auth = SlurmAuthHealth {
        ok: api.resolved.is_some() && auth_error.is_none(),
        error: auth_error,
    };
    ping.ok = ping.status_code.is_some_and(|c| (200..300).contains(&c))
        && auth.ok
        && slurmctld.as_deref() != Some("DOWN");

    remember_discovery(Discovery {
        url: url.clone(),
        user: user.to_string(),
        available: api.available.clone(),
        resolved: api.resolved.clone(),
        release: slurm_release.clone(),
    });
    Ok(Json(SlurmHealthView {
        jwt,
        ping,
        api,
        auth,
        slurmctld,
        slurm_release,
        jwt_user,
        problems,
    }))
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
        let (mut tick, transition) =
            merge_tick(prev.as_ref(), &status, detail, Utc::now().timestamp());
        tick.warnings = header_str("x-obleth-provisioner-tick-warnings")
            .and_then(|w| serde_json::from_str::<Vec<String>>(&w).ok())
            .unwrap_or_default();
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
    fn jwt_last4_counts_chars_not_bytes() {
        let view = |jwt: &str| {
            SlurmSettingsView::from_settings(&SlurmSettings {
                slurm_jwt: jwt.into(),
                ..Default::default()
            })
            .jwt_last4
        };
        // Four trailing multibyte chars: a byte slice would split one and panic.
        assert_eq!(view("header.payload.sig-éèêë").as_deref(), Some("éèêë"));
        assert_eq!(view("abcdef").as_deref(), Some("cdef"));
        assert_eq!(view("aé").as_deref(), None);
    }

    #[test]
    fn hf_token_is_masked_in_the_view() {
        let v = SlurmSettingsView::from_settings(&SlurmSettings {
            hf_token: "hf_abcdefWXYZ".into(),
            ..Default::default()
        });
        assert!(v.hf_token_set);
        assert_eq!(v.hf_token_last4.as_deref(), Some("WXYZ"));
        let json = serde_json::to_string(&v).unwrap();
        assert!(!json.contains("hf_abcdef"), "full token leaked: {json}");
        let unset = SlurmSettingsView::from_settings(&SlurmSettings::default());
        assert!(!unset.hf_token_set);
        assert_eq!(unset.hf_token_last4, None);
    }

    #[test]
    fn hf_token_put_keeps_on_omit_clears_on_empty_replaces_otherwise() {
        assert_eq!(resolve_hf_token(None, "hf_old"), "hf_old");
        assert_eq!(resolve_hf_token(Some(""), "hf_old"), "");
        assert_eq!(resolve_hf_token(Some("  "), "hf_old"), "");
        assert_eq!(resolve_hf_token(Some(" hf_new "), "hf_old"), "hf_new");
        // null deserializes to None, same as omitted.
        let body: UpdateSlurmSettings = serde_json::from_str(r#"{"hf_token":null}"#).unwrap();
        assert!(body.hf_token.is_none());
        assert!(body.cluster_defaults.is_none());
    }

    #[test]
    fn cluster_defaults_are_trimmed_and_blank_images_dropped() {
        let d = normalize_cluster_defaults(&ClusterDefaults {
            cache_dir: " /scratch/hf ".into(),
            images_dir: "/images".into(),
            log_dir: "".into(),
            setup: "module load apptainer\nexport X=1\n\n".into(),
            images: [
                (" vllm ".to_string(), " vllm.sif ".to_string()),
                ("sglang".to_string(), "  ".to_string()),
                ("".to_string(), "x.sif".to_string()),
            ]
            .into(),
        });
        assert_eq!(d.cache_dir, "/scratch/hf");
        assert_eq!(d.setup, "module load apptainer\nexport X=1");
        assert_eq!(d.images.len(), 1);
        assert_eq!(d.images.get("vllm").map(String::as_str), Some("vllm.sif"));
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
        assert!(is_valid_api_version("auto"));
        assert!(is_valid_api_version(""));
        assert!(!is_valid_api_version("../admin"));
        assert!(!is_valid_api_version("v0.0.40/jobs"));
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
        let (e3, tr3) = merge_tick(
            Some(&e2),
            "error",
            Some("boom".into()),
            1_015 + HELD_ALERT_SECS,
        );
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
        let (i1, _) = merge_tick(
            None,
            "idle",
            Some("slurm disabled in settings".into()),
            1_000,
        );
        let (_, tr) = merge_tick(
            Some(&i1),
            "idle",
            Some("slurm disabled in settings".into()),
            1_000 + HELD_ALERT_SECS,
        );
        assert_eq!(tr, TickTransition::Held);
    }

    #[test]
    fn settings_warn_about_short_or_expired_tokens_user_mismatch_and_plaintext() {
        let now = 1_700_000_000;
        let tok = |exp: i64, sun: &str| {
            let enc = |b: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(b);
            format!(
                "{}.{}.s",
                enc("{}"),
                enc(&format!(r#"{{"exp":{exp},"sun":"{sun}"}}"#))
            )
        };
        let s = |jwt: String, user: &str| SlurmSettings {
            enabled: true,
            slurm_user: user.into(),
            slurm_jwt: jwt,
            ..Default::default()
        };
        assert!(settings_warnings(&s(tok(now + 30 * 86_400, "svc"), "svc"), now, true).is_empty());
        let w = settings_warnings(&s(tok(now + 1800, "alice"), "bob"), now, false);
        assert_eq!(w.len(), 3, "{w:?}");
        assert!(
            w[0].starts_with("This token expires in 30 minutes."),
            "{}",
            w[0]
        );
        assert!(w[0].contains("scontrol token lifespan=2592000"));
        assert_eq!(
            w[1],
            "The token's user (alice) is not the Slurm user you entered (bob)."
        );
        assert!(w[2].contains("OBLETH_ENCRYPTION_KEY"));
        let w = settings_warnings(&s(tok(now - 60, "svc"), "svc"), now, true);
        assert!(
            w[0].starts_with("This token expired at 2023-11-14 22:12 UTC."),
            "{w:?}"
        );
        let w = settings_warnings(&s(String::new(), "svc"), now, true);
        assert!(w[0].contains("no token is saved"));
    }

    #[test]
    fn api_health_explains_versions_the_server_does_not_serve() {
        let served = Ok(vec![
            "v0.0.42".to_string(),
            "v0.0.43".into(),
            "v0.0.44".into(),
            "v0.0.45".into(),
        ]);
        let a = api_health("auto", &served);
        assert!(a.ok);
        assert_eq!(a.resolved.as_deref(), Some("v0.0.45"));
        let a = api_health("", &served);
        assert_eq!(a.configured, "auto");
        let a = api_health("v0.0.40", &served);
        assert!(!a.ok);
        let e = a.error.unwrap();
        assert!(e.contains("does not serve API version v0.0.40"), "{e}");
        assert!(e.contains("v0.0.42, v0.0.43, v0.0.44, v0.0.45"), "{e}");
        assert!(
            e.contains("obleth uses v0.0.45 until you set the API version to auto or to v0.0.45"),
            "{e}"
        );
        // The test goes on with the version the provisioner falls back to.
        assert_eq!(a.resolved.as_deref(), Some("v0.0.45"));
        let a = api_health("auto", &Ok(vec!["v0.0.39".into()]));
        assert!(
            !a.ok
                && a.error
                    .unwrap()
                    .contains("obleth supports v0.0.40 to v0.0.45")
        );
        let a = api_health("auto", &Err("Could not reach slurmrestd.".into()));
        assert_eq!(a.error.as_deref(), Some("Could not reach slurmrestd."));
        // An explicit version is used as is when discovery could not list any.
        let a = api_health("v0.0.43", &Err("no openapi".into()));
        assert!(a.ok);
        assert_eq!(a.resolved.as_deref(), Some("v0.0.43"));
    }

    #[test]
    fn partial_update_payload_keeps_omitted_fields() {
        let u: UpdateSlurmSettings =
            serde_json::from_str(r#"{"slurm_jwt":"SLURM_JWT=abc"}"#).unwrap();
        assert!(u.enabled.is_none() && u.slurmrestd_url.is_none() && u.slurm_user.is_none());
        assert!(u.node_aliases.is_none());
        assert_eq!(
            obleth_config::slurm::clean_pasted_jwt(u.slurm_jwt.as_deref().unwrap()),
            "abc"
        );
    }
}
