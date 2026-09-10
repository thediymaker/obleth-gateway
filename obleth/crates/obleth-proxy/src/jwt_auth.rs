//! JWT bearer authentication for the data plane.
//!
//! A credential that looks like a JWT is verified offline against a trusted
//! issuer's JWKS (cached per issuer, background-refreshed, stale-on-error), then
//! mapped to an *identity key*: a real `api_keys` row whose `key_hash` is
//! `jwt:sha256(issuer \0 subject)`. From there the request is indistinguishable
//! from one authenticated with a secret key, so every downstream policy stage
//! (tenant status, schedules, budgets, allowed models, guardrails, tracing)
//! applies unchanged. See `docs/superpowers/specs/2026-09-10-jwt-bearer-auth-design.md`.
//!
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use axum::body::Body;
use axum::http::{Response, StatusCode};
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use obleth_admin::AlertDispatcher;
use obleth_config::{sanitize_device_id, IdentityProvision, JwtIssuerConfig, ResolvedKey};
use obleth_store::Store;
use tokio::sync::Mutex;

use crate::metrics::Metrics;
use crate::state::AppState;

/// Background JWKS refresh period.
pub const JWKS_REFRESH_INTERVAL: Duration = Duration::from_secs(15 * 60);
/// Minimum gap between unknown-`kid` refetches for one issuer: a flood of
/// forged kids costs the IdP at most one fetch per minute.
pub const JWKS_REFETCH_MIN_GAP: Duration = Duration::from_secs(60);
/// Clock skew tolerated on `exp`/`nbf`/`iat`.
const LEEWAY_SECS: u64 = 60;
/// Upper bound on a credential we will even try to parse as a JWT.
const MAX_TOKEN_LEN: usize = 8 * 1024;

/// Cheap syntactic check: compact JWS is three base64url segments and every
/// JSON header starts with `{"` which encodes to `eyJ`. Secret keys are `sk_…`.
pub fn looks_like_jwt(credential: &str) -> bool {
    credential.starts_with("eyJ")
        && credential.len() <= MAX_TOKEN_LEN
        && credential.bytes().filter(|b| *b == b'.').count() == 2
}

/// What a verified token tells us about the caller.
#[derive(Debug, Clone)]
pub struct VerifiedIdentity {
    pub issuer_idx: usize,
    pub issuer: String,
    pub subject: String,
    /// Already sanitised; empty when absent or rejected.
    pub device_id: String,
    /// Object of the configured `identity_claims` present in the token.
    pub identity_claims: serde_json::Value,
}

struct Snapshot {
    /// kid -> decoding key. Keys without a kid are skipped.
    keys: HashMap<String, DecodingKey>,
}

struct IssuerState {
    cfg: JwtIssuerConfig,
    jwks_url: String,
    algorithms: Vec<Algorithm>,
    snapshot: ArcSwap<Snapshot>,
    /// Guards refetch: single-flight and rate-limited.
    refetch: Mutex<Option<Instant>>,
}

pub struct JwksVerifier {
    issuers: Vec<IssuerState>,
    http: reqwest::Client,
    metrics: Arc<Metrics>,
    alerts: AlertDispatcher,
}

impl JwksVerifier {
    pub fn new(
        issuers: Vec<JwtIssuerConfig>,
        http: reqwest::Client,
        metrics: Arc<Metrics>,
        alerts: AlertDispatcher,
    ) -> Arc<Self> {
        let issuers = issuers
            .into_iter()
            .enumerate()
            .map(|(idx, cfg)| {
                let jwks_url = cfg.jwks_url.clone().unwrap_or_else(|| {
                    format!("{}/.well-known/jwks.json", cfg.issuer.trim_end_matches('/'))
                });
                let algorithms = cfg
                    .algorithms
                    .iter()
                    .filter_map(|a| match parse_algorithm(a) {
                        Some(alg) => Some(alg),
                        None => {
                            tracing::warn!(
                                issuer_idx = idx,
                                algorithm = %a,
                                "dropping unrecognized algorithm from issuer config"
                            );
                            None
                        }
                    })
                    .collect();
                IssuerState {
                    cfg,
                    jwks_url,
                    algorithms,
                    snapshot: ArcSwap::from_pointee(Snapshot {
                        keys: HashMap::new(),
                    }),
                    refetch: Mutex::new(None),
                }
            })
            .collect();
        Arc::new(Self {
            issuers,
            http,
            metrics,
            alerts,
        })
    }

    pub fn issuer(&self, idx: usize) -> &JwtIssuerConfig {
        &self.issuers[idx].cfg
    }

    /// Fetch and install an issuer's JWKS. Returns true on success. On failure
    /// the previous snapshot is retained and an alert is raised.
    pub async fn refresh_issuer(&self, idx: usize) -> bool {
        let st = &self.issuers[idx];
        let fetched = async {
            let resp = self
                .http
                .get(&st.jwks_url)
                .timeout(Duration::from_secs(10))
                .send()
                .await?
                .error_for_status()?;
            let set: JwkSet = resp.json().await?;
            Ok::<JwkSet, reqwest::Error>(set)
        }
        .await;
        match fetched {
            Ok(set) => {
                let mut keys = HashMap::new();
                for jwk in &set.keys {
                    let Some(kid) = jwk.common.key_id.clone() else {
                        tracing::warn!(issuer_idx = idx, "skipping jwk without a kid");
                        continue;
                    };
                    match DecodingKey::from_jwk(jwk) {
                        Ok(k) => {
                            keys.insert(kid, k);
                        }
                        Err(e) => {
                            tracing::warn!(issuer_idx = idx, kid, error = %e, "skipping unusable jwk")
                        }
                    }
                }
                if keys.is_empty() {
                    tracing::warn!(
                        issuer_idx = idx,
                        url = %st.jwks_url,
                        "jwks fetch returned no usable keys; keeping previous snapshot"
                    );
                    self.metrics.record_jwks_refresh(idx, "error");
                    self.alerts.issue(
                        format!("jwks_fetch_failed:{}", st.cfg.issuer),
                        "JWKS fetch failed",
                        format!(
                            "JWKS document contained no usable keys for issuer {}",
                            st.cfg.issuer
                        ),
                    );
                    return false;
                }
                st.snapshot.store(Arc::new(Snapshot { keys }));
                self.metrics.record_jwks_refresh(idx, "ok");
                true
            }
            Err(e) => {
                tracing::warn!(issuer_idx = idx, url = %st.jwks_url, error = %e, "jwks fetch failed; keeping previous snapshot");
                self.metrics.record_jwks_refresh(idx, "error");
                self.alerts.issue(
                    format!("jwks_fetch_failed:{}", st.cfg.issuer),
                    "JWKS fetch failed",
                    format!(
                        "Could not refresh signing keys for issuer {}: {e}",
                        st.cfg.issuer
                    ),
                );
                false
            }
        }
    }

    /// Periodic refresh of every issuer.
    pub fn spawn_refresh(self: &Arc<Self>) {
        let me = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(JWKS_REFRESH_INTERVAL);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            tick.tick().await; // first tick fires immediately; boot already fetched
            loop {
                tick.tick().await;
                for idx in 0..me.issuers.len() {
                    me.refresh_issuer(idx).await;
                }
            }
        });
    }

    /// Unknown-kid refetch: at most one per [`JWKS_REFETCH_MIN_GAP`] per issuer,
    /// and concurrent callers share one fetch. Returns whether a fetch ran.
    async fn maybe_refetch(&self, idx: usize) -> bool {
        let st = &self.issuers[idx];
        let mut last = st.refetch.lock().await;
        if let Some(t) = *last {
            if t.elapsed() < JWKS_REFETCH_MIN_GAP {
                return false;
            }
        }
        *last = Some(Instant::now());
        self.refresh_issuer(idx).await;
        true
    }

    /// Verify a compact JWT. `Err` carries the metric result label; the caller
    /// maps every error to the same coarse 401.
    pub async fn verify(&self, token: &str) -> Result<VerifiedIdentity, &'static str> {
        let outcome = self.verify_inner(token).await;
        self.metrics.record_jwt_verify(match &outcome {
            Ok(_) => "ok",
            Err(label) => label,
        });
        if let Err(label) = &outcome {
            tracing::debug!(result = label, "jwt rejected");
        }
        outcome
    }

    async fn verify_inner(&self, token: &str) -> Result<VerifiedIdentity, &'static str> {
        if token.len() > MAX_TOKEN_LEN {
            return Err("malformed");
        }
        let header = decode_header(token).map_err(|_| "malformed")?;
        // Unverified peek at `iss` to select the issuer before any crypto.
        let iss = unverified_issuer(token).ok_or("malformed")?;
        let idx = self
            .issuers
            .iter()
            .position(|st| st.cfg.issuer == iss)
            .ok_or("unknown_issuer")?;
        let st = &self.issuers[idx];
        if !st.algorithms.contains(&header.alg) {
            return Err("bad_alg");
        }
        let kid = header.kid.ok_or("unknown_kid")?;

        let key = match st.snapshot.load().keys.get(&kid) {
            Some(k) => k.clone(),
            None => {
                self.maybe_refetch(idx).await;
                st.snapshot
                    .load()
                    .keys
                    .get(&kid)
                    .cloned()
                    .ok_or("unknown_kid")?
            }
        };

        let mut validation = Validation::new(header.alg);
        validation.leeway = LEEWAY_SECS;
        validation.validate_nbf = true;
        validation.set_audience(&[st.cfg.audience.as_str()]);
        validation.set_issuer(&[st.cfg.issuer.as_str()]);
        // `required_spec_claims` defaults to just `{"exp"}`; without `aud`/`iss`
        // here, a token that simply omits `aud` (or sends a non-string/non-array
        // value, which fails to parse into `Audience`) sails through
        // `set_audience`'s match arm on `TryParse::NotPresent`/`FailedToParse`
        // and is accepted with no audience check at all.
        validation.set_required_spec_claims(&["exp", "aud", "iss"]);
        let data = decode::<serde_json::Map<String, serde_json::Value>>(token, &key, &validation)
            .map_err(|e| {
            use jsonwebtoken::errors::ErrorKind::*;
            match e.kind() {
                ExpiredSignature => "expired",
                ImmatureSignature => "not_yet_valid",
                InvalidAudience => "bad_audience",
                InvalidIssuer => "unknown_issuer",
                InvalidSignature => "bad_signature",
                InvalidAlgorithm | InvalidAlgorithmName => "bad_alg",
                MissingRequiredClaim(c) if c == "aud" => "bad_audience",
                MissingRequiredClaim(c) if c == "iss" => "unknown_issuer",
                _ => "malformed",
            }
        })?;
        let claims = data.claims;

        // `iat` must not be in the future beyond leeway (jsonwebtoken does not
        // check it). Use `as_f64` so a fractional `iat` (still valid JSON/JWT)
        // isn't silently skipped by an `as_i64` that only matches integers.
        if let Some(iat) = claims.get("iat").and_then(|v| v.as_f64()) {
            if iat.round() as i64 > chrono::Utc::now().timestamp() + LEEWAY_SECS as i64 {
                return Err("not_yet_valid");
            }
        }

        let subject = claims
            .get(&st.cfg.subject_claim)
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or("bad_subject")?
            .to_string();
        let device_id = sanitize_device_id(
            st.cfg
                .device_claim
                .as_deref()
                .and_then(|c| claims.get(c))
                .and_then(|v| v.as_str()),
        );
        let mut extra = serde_json::Map::new();
        for c in &st.cfg.identity_claims {
            if let Some(v) = claims.get(c) {
                extra.insert(c.clone(), v.clone());
            }
        }
        Ok(VerifiedIdentity {
            issuer_idx: idx,
            issuer: st.cfg.issuer.clone(),
            subject,
            device_id,
            identity_claims: if extra.is_empty() {
                serde_json::Value::Null
            } else {
                serde_json::Value::Object(extra)
            },
        })
    }
}

fn parse_algorithm(name: &str) -> Option<Algorithm> {
    name.parse::<Algorithm>().ok()
}

/// Read `iss` from the payload segment without verifying anything. Used only
/// to pick which issuer's keys to verify against; the verified claims are
/// re-checked against that issuer afterwards.
fn unverified_issuer(token: &str) -> Option<String> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let v: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    v.get("iss")?.as_str().map(str::to_string)
}

pub struct Authenticated {
    pub resolved: Arc<ResolvedKey>,
    pub device_id: String,
}

pub enum AuthFailure {
    /// Any verification failure, an unknown identity with `jit_provision:
    /// false`, or a cache error. Disabled keys are rejected by the handler
    /// after resolution, exactly as for secret keys.
    Unauthorized,
    /// Verified identity, first sight, and Postgres is unavailable: 503.
    ProvisioningUnavailable,
}

/// A resolved bearer credential, shared by the data-plane proxy and the MCP
/// gateway so both authenticate identically. `auth_kind` is `"identity"` or
/// `"key"`, matching the tracer label `proxy_request` has always recorded.
pub(crate) struct Credential {
    pub resolved: Arc<ResolvedKey>,
    pub device_id: String,
    pub auth_kind: &'static str,
}

/// Which resolution path a bearer credential takes. Extracted as a pure
/// function so the routing decision is unit-testable without constructing an
/// `AppState` (which needs live Redis/Postgres connections in this crate's
/// tests).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Route {
    Jwt,
    Key,
}

pub(crate) fn credential_route(jwt_enabled: bool, secret: &str) -> Route {
    if jwt_enabled && looks_like_jwt(secret) {
        Route::Jwt
    } else {
        Route::Key
    }
}

/// Resolve the bearer credential exactly as `proxy_request` does: JWT path when
/// the feature is on and the credential looks like a JWT, else the secret-key
/// path (`hash_api_key` -> `resolve_key`, byte-for-byte unchanged). Every
/// failure returns the same response the caller would have built inline.
pub(crate) async fn authenticate_credential(
    state: &AppState,
    secret: &str,
) -> Result<Credential, Response<Body>> {
    match credential_route(state.jwt.is_some(), secret) {
        Route::Jwt => {
            let jwt = state
                .jwt
                .as_ref()
                .expect("Route::Jwt is only returned when state.jwt is Some");
            match jwt.authenticate(state, secret).await {
                Ok(a) => Ok(Credential {
                    resolved: a.resolved,
                    device_id: a.device_id,
                    auth_kind: "identity",
                }),
                Err(AuthFailure::Unauthorized) => Err(crate::proxy::error_json(
                    StatusCode::UNAUTHORIZED,
                    "invalid token",
                )),
                Err(AuthFailure::ProvisioningUnavailable) => Err(crate::proxy::error_json(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "identity provisioning unavailable",
                )),
            }
        }
        Route::Key => {
            let hash = obleth_config::hash_api_key(secret);
            match crate::proxy::resolve_key(state, &hash).await {
                Some(r) => Ok(Credential {
                    resolved: r,
                    device_id: String::new(),
                    auth_kind: "key",
                }),
                None => Err(crate::proxy::error_json(
                    StatusCode::UNAUTHORIZED,
                    "invalid api key",
                )),
            }
        }
    }
}

/// Map of handle -> single-flight gate. Wrapped in a `std::sync::Mutex`
/// (never held across an `.await`) rather than `tokio::sync::Mutex`, since
/// every access is a quick get/insert/remove.
type InflightMap = Arc<std::sync::Mutex<HashMap<String, Arc<Mutex<()>>>>>;

/// RAII cleanup for a single-flight entry: removes `handle` from the map on
/// drop, including when the owning future is cancelled mid-flight (e.g. a
/// client disconnect while `authenticate` is awaiting Postgres), so the map
/// can never accumulate a stale entry for a request that never finished.
struct InflightEntry {
    map: InflightMap,
    handle: String,
}

impl Drop for InflightEntry {
    fn drop(&mut self) {
        self.map.lock().unwrap().remove(&self.handle);
    }
}

/// Get-or-create the single-flight gate for `handle`. Concurrent callers for
/// the same handle observe `Arc::ptr_eq` gates.
fn inflight_gate(map: &InflightMap, handle: &str) -> Arc<Mutex<()>> {
    map.lock()
        .unwrap()
        .entry(handle.to_string())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

pub struct JwtAuth {
    verifier: Arc<JwksVerifier>,
    store: Store,
    /// Per-handle single-flight guards for first-sight provisioning.
    inflight: InflightMap,
}

impl JwtAuth {
    pub fn new(verifier: Arc<JwksVerifier>, store: Store) -> Arc<Self> {
        Arc::new(Self {
            verifier,
            store,
            inflight: Arc::new(std::sync::Mutex::new(HashMap::new())),
        })
    }

    /// Verify the token and resolve it to a key exactly as a secret would be
    /// resolved. On a miss, provision the identity key (once, single-flight)
    /// and warm Redis + the local cache.
    pub async fn authenticate(
        &self,
        state: &AppState,
        token: &str,
    ) -> Result<Authenticated, AuthFailure> {
        let identity = self
            .verifier
            .verify(token)
            .await
            .map_err(|_| AuthFailure::Unauthorized)?;
        let handle = obleth_config::identity_key_hash(&identity.issuer, &identity.subject);

        match crate::proxy::try_resolve_key(state, &handle).await {
            Ok(Some(resolved)) => {
                return Ok(Authenticated {
                    resolved,
                    device_id: identity.device_id,
                });
            }
            Ok(None) => {}
            Err(()) => {
                // The shared helper already alerted `redis_key_lookup_failed`;
                // fail closed instead of treating a cache error as first
                // sight, which would otherwise hit Postgres on every request
                // for an already-provisioned identity during a Redis outage.
                tracing::debug!("jwt path failed closed on a cache error");
                return Err(AuthFailure::Unauthorized);
            }
        }

        let cfg = self.verifier.issuer(identity.issuer_idx);
        if !cfg.jit_provision {
            state.metrics.record_jwt_verify("not_provisioned");
            return Err(AuthFailure::Unauthorized);
        }

        // Single-flight per handle: concurrent first requests from one user
        // perform one Postgres write; the rest wait and re-read the cache.
        //
        // The cleanup guard is constructed *before* the await on the gate,
        // not after: if this future is cancelled while still waiting for the
        // gate (e.g. a client disconnect during a long queue behind another
        // in-flight provision), the guard must already exist so its `Drop`
        // fires and removes the map entry -- otherwise a future built only
        // after a successful `.await` would simply never run, leaking the
        // entry for the lifetime of the process. Locals drop in reverse
        // declaration order, so binding through `entry` and only renaming to
        // `_entry` after the lock is held keeps the normal-path drop order
        // unchanged: `_entry` (declared last) still drops before `_held`,
        // removing the map entry before the gate is released.
        let gate = inflight_gate(&self.inflight, &handle);
        let entry = InflightEntry {
            map: self.inflight.clone(),
            handle: handle.clone(),
        };
        let _held = gate.lock().await;
        let _entry = entry;
        async {
            match crate::proxy::try_resolve_key(state, &handle).await {
                Ok(Some(resolved)) => {
                    return Ok(Authenticated {
                        resolved,
                        device_id: identity.device_id.clone(),
                    });
                }
                Ok(None) => {}
                Err(()) => {
                    tracing::debug!("jwt path failed closed on a cache error");
                    return Err(AuthFailure::Unauthorized);
                }
            }
            let provisioned = self
                .store
                .provision_identity_key(IdentityProvision {
                    issuer: &identity.issuer,
                    subject: &identity.subject,
                    tenant_name: &cfg.tenant,
                    claims: identity.identity_claims.clone(),
                })
                .await
                .map_err(|e| {
                    tracing::warn!(error = %e, "identity provisioning failed");
                    state.metrics.record_jwt_verify("provision_failed");
                    state.alerts.issue(
                        format!("identity_provision_failed:{}", cfg.issuer),
                        "Identity provisioning failed",
                        format!(
                            "Could not create the identity key for a verified token from {}: {e}",
                            cfg.issuer
                        ),
                    );
                    AuthFailure::ProvisioningUnavailable
                })?;
            if provisioned.tenant_created {
                // In a deployment whose account system already manages the
                // tenant, creation almost always means a config typo silently
                // forked CLI traffic onto fresh default policy. Loud on purpose.
                tracing::warn!(
                    tenant = %cfg.tenant,
                    issuer = %cfg.issuer,
                    "identity tenant did not exist and was created"
                );
                state.alerts.issue(
                    format!("identity_tenant_created:{}", cfg.issuer),
                    "Identity tenant created",
                    format!(
                        "Tenant `{}` for issuer {} did not exist and was created with default policy. Verify the `tenant` value in OBLETH_JWT_ISSUERS.",
                        cfg.tenant, cfg.issuer
                    ),
                );
            }
            if let Err(e) = state
                .redis
                .put_resolved_key(&provisioned.hash, &provisioned.resolved)
                .await
            {
                // The shared `redis_key_lookup_failed` alert path covers
                // Redis outages; a single warm-on-write miss here is not
                // itself alert-worthy.
                tracing::debug!(error = %e, "failed to warm identity key into redis");
            }
            let resolved = Arc::new(provisioned.resolved);
            state
                .key_cache
                .insert(provisioned.hash.clone(), resolved.clone())
                .await;
            Ok(Authenticated {
                resolved,
                device_id: identity.device_id.clone(),
            })
        }
        .await
    }
}

#[cfg(test)]
mod inflight_tests {
    use super::{inflight_gate, InflightEntry, InflightMap};
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Covers the single-flight bookkeeping without needing a `Store` (which
    /// would require a live Postgres connection): two callers for the same
    /// handle share one gate, and the RAII guard removes the map entry on
    /// drop -- including an "early" drop that stands in for a request future
    /// being cancelled mid-flight (e.g. a client disconnect while awaiting
    /// Postgres), which is exactly the leak this guard exists to prevent.
    #[test]
    fn inflight_gate_shares_arc_and_raii_guard_cleans_up_on_drop() {
        let map: InflightMap = Arc::new(std::sync::Mutex::new(HashMap::new()));

        let gate_a = inflight_gate(&map, "h1");
        let entry_a = InflightEntry {
            map: map.clone(),
            handle: "h1".to_string(),
        };
        assert_eq!(map.lock().unwrap().len(), 1);

        // A second caller for the same handle observes the same gate.
        let gate_b = inflight_gate(&map, "h1");
        let entry_b = InflightEntry {
            map: map.clone(),
            handle: "h1".to_string(),
        };
        assert!(Arc::ptr_eq(&gate_a, &gate_b));
        assert_eq!(map.lock().unwrap().len(), 1);

        // Dropping an entry early -- simulating a cancelled request -- still
        // empties the map for this handle; no leaked entry survives.
        drop(entry_a);
        assert!(map.lock().unwrap().is_empty());

        drop(entry_b);
        assert!(map.lock().unwrap().is_empty());

        drop(gate_a);
        drop(gate_b);
    }

    /// `authenticate` constructs the cleanup guard *before* awaiting the gate
    /// (see the comment above that call site), specifically so a request
    /// cancelled while still waiting for the gate -- before the gate's own
    /// `.await` ever resolves -- still cleans up. This reproduces that
    /// ordering directly: the guard exists with no lock ever taken, and
    /// dropping it (standing in for the future being cancelled mid-wait)
    /// still empties the map.
    #[test]
    fn entry_created_before_locking_is_removed_on_drop() {
        let map: InflightMap = Arc::new(std::sync::Mutex::new(HashMap::new()));
        let _gate = inflight_gate(&map, "h1");
        let entry = InflightEntry {
            map: map.clone(),
            handle: "h1".to_string(),
        };
        assert_eq!(map.lock().unwrap().len(), 1);

        drop(entry);
        assert!(map.lock().unwrap().is_empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::response::IntoResponse;
    use base64::Engine;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use ring::signature::{EcdsaKeyPair, KeyPair, ECDSA_P256_SHA256_FIXED_SIGNING};
    use std::sync::Mutex;

    /// An ES256 keypair plus the JWK the JWKS server publishes for it.
    struct TestKey {
        pkcs8: Vec<u8>,
        jwk: serde_json::Value,
    }

    fn gen_key(kid: &str) -> TestKey {
        let rng = ring::rand::SystemRandom::new();
        let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
        let pair =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, doc.as_ref(), &rng).unwrap();
        let point = pair.public_key().as_ref(); // 0x04 || X(32) || Y(32)
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        TestKey {
            pkcs8: doc.as_ref().to_vec(),
            jwk: serde_json::json!({
                "kty": "EC", "crv": "P-256", "alg": "ES256", "use": "sig", "kid": kid,
                "x": b64.encode(&point[1..33]),
                "y": b64.encode(&point[33..65]),
            }),
        }
    }

    /// In-process JWKS server whose body (or failure mode) can be swapped.
    struct JwksServer {
        url: String,
        body: Arc<Mutex<Result<serde_json::Value, ()>>>,
        hits: Arc<std::sync::atomic::AtomicUsize>,
    }

    #[allow(clippy::type_complexity)]
    async fn serve_jwks(keys: Vec<serde_json::Value>) -> JwksServer {
        use axum::{extract::State, http::StatusCode, routing::get, Router};
        let body = Arc::new(Mutex::new(Ok(serde_json::json!({ "keys": keys }))));
        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let st = (body.clone(), hits.clone());
        let app = Router::new()
            .route(
                "/jwks",
                get(
                    |State((body, hits)): State<(
                        Arc<Mutex<Result<serde_json::Value, ()>>>,
                        Arc<std::sync::atomic::AtomicUsize>,
                    )>| async move {
                        hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        match body.lock().unwrap().clone() {
                            Ok(v) => (StatusCode::OK, axum::Json(v)).into_response(),
                            Err(()) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                        }
                    },
                ),
            )
            .with_state(st);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        JwksServer {
            url: format!("http://{addr}/jwks"),
            body,
            hits,
        }
    }

    fn issuer_cfg(jwks_url: &str) -> JwtIssuerConfig {
        JwtIssuerConfig {
            issuer: "https://idp.example.com".into(),
            jwks_url: Some(jwks_url.into()),
            audience: "obleth".into(),
            algorithms: vec!["ES256".into()],
            subject_claim: "sub".into(),
            device_claim: Some("device_id".into()),
            identity_claims: vec!["uid".into()],
            tenant: "cli-users".into(),
            jit_provision: true,
        }
    }

    fn now() -> i64 {
        chrono::Utc::now().timestamp()
    }

    fn sign(key: &TestKey, kid: &str, claims: serde_json::Value) -> String {
        let mut h = Header::new(jsonwebtoken::Algorithm::ES256);
        h.kid = Some(kid.into());
        encode(&h, &claims, &EncodingKey::from_ec_der(&key.pkcs8)).unwrap()
    }

    fn good_claims() -> serde_json::Value {
        serde_json::json!({
            "iss": "https://idp.example.com", "aud": "obleth", "sub": "alice",
            "exp": now() + 600, "iat": now(), "nbf": now() - 5,
            "device_id": "dev-1", "uid": "u-1", "other": "ignored"
        })
    }

    async fn verifier(server: &JwksServer) -> Arc<JwksVerifier> {
        let v = JwksVerifier::new(
            vec![issuer_cfg(&server.url)],
            reqwest::Client::new(),
            Arc::new(Metrics::new()),
            obleth_admin::AlertDispatcher::new(
                reqwest::Client::new(),
                obleth_config::AlertSettings::default(),
            ),
        );
        assert!(v.refresh_issuer(0).await);
        v
    }

    #[test]
    fn detection_requires_eyj_prefix_and_two_dots() {
        assert!(looks_like_jwt("eyJhbGciOiJFUzI1NiJ9.eyJzdWIiOiJhIn0.sig"));
        assert!(!looks_like_jwt(
            "sk_0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
        assert!(!looks_like_jwt("eyJ.onlyonedot"));
        assert!(!looks_like_jwt("eyJ.three.dots.here"));
        assert!(!looks_like_jwt(""));
    }

    /// Routing decision `authenticate_credential` is built on. Covers both
    /// the MCP handler's request (JWT off, so an `eyJ…` credential must still
    /// fall through to the secret-key path and fail as `invalid api key`
    /// rather than being treated as a token) and the ordinary key path.
    #[test]
    fn credential_route_selects_jwt_only_when_enabled_and_shaped_like_one() {
        let jwt_like = "eyJhbGciOiJFUzI1NiJ9.eyJzdWIiOiJhIn0.sig";
        let sk_like = "sk_0123456789abcdef0123456789abcdef0123456789abcdef";

        assert_eq!(credential_route(true, jwt_like), Route::Jwt);
        assert_eq!(credential_route(true, sk_like), Route::Key);
        // With the feature off (`state.jwt = None`), even a JWT-shaped
        // credential takes the key path and is hashed/looked up like any
        // other key -- exactly the `invalid api key` behaviour required when
        // `OBLETH_JWT_ISSUERS` is unset.
        assert_eq!(credential_route(false, jwt_like), Route::Key);
        assert_eq!(credential_route(false, sk_like), Route::Key);
    }

    #[tokio::test]
    async fn valid_token_yields_identity_and_sanitised_device() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        let v = verifier(&server).await;
        let id = v.verify(&sign(&key, "k1", good_claims())).await.unwrap();
        assert_eq!(id.issuer_idx, 0);
        assert_eq!(id.issuer, "https://idp.example.com");
        assert_eq!(id.subject, "alice");
        assert_eq!(id.device_id, "dev-1");
        assert_eq!(id.identity_claims, serde_json::json!({ "uid": "u-1" }));
    }

    #[tokio::test]
    async fn device_claim_is_sanitised() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        let v = verifier(&server).await;
        let mut c = good_claims();
        c["device_id"] = serde_json::Value::String("bad id;".into());
        assert_eq!(v.verify(&sign(&key, "k1", c)).await.unwrap().device_id, "");
    }

    #[tokio::test]
    async fn rejects_each_failure_class() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        let v = verifier(&server).await;

        let mut c = good_claims();
        c["exp"] = serde_json::json!(now() - 120);
        assert_eq!(v.verify(&sign(&key, "k1", c)).await.unwrap_err(), "expired");

        let mut c = good_claims();
        c["nbf"] = serde_json::json!(now() + 600);
        assert_eq!(
            v.verify(&sign(&key, "k1", c)).await.unwrap_err(),
            "not_yet_valid"
        );

        let mut c = good_claims();
        c["iat"] = serde_json::json!(now() + 600);
        assert_eq!(
            v.verify(&sign(&key, "k1", c)).await.unwrap_err(),
            "not_yet_valid"
        );

        let mut c = good_claims();
        c["aud"] = serde_json::json!("someone-else");
        assert_eq!(
            v.verify(&sign(&key, "k1", c)).await.unwrap_err(),
            "bad_audience"
        );

        let mut c = good_claims();
        c["iss"] = serde_json::json!("https://other.example.com");
        assert_eq!(
            v.verify(&sign(&key, "k1", c)).await.unwrap_err(),
            "unknown_issuer"
        );

        let mut c = good_claims();
        c["sub"] = serde_json::json!("");
        assert_eq!(
            v.verify(&sign(&key, "k1", c)).await.unwrap_err(),
            "bad_subject"
        );

        let mut c = good_claims();
        c.as_object_mut().unwrap().remove("sub");
        assert_eq!(
            v.verify(&sign(&key, "k1", c)).await.unwrap_err(),
            "bad_subject"
        );

        // Signed by a different key under the same kid.
        let other = gen_key("k1");
        assert_eq!(
            v.verify(&sign(&other, "k1", good_claims()))
                .await
                .unwrap_err(),
            "bad_signature"
        );

        // Symmetric algorithm is refused before any key lookup.
        let hs = encode(
            &Header::new(jsonwebtoken::Algorithm::HS256),
            &good_claims(),
            &EncodingKey::from_secret(b"secret"),
        )
        .unwrap();
        assert_eq!(v.verify(&hs).await.unwrap_err(), "bad_alg");

        // Garbage.
        assert_eq!(v.verify("eyJ.x.y").await.unwrap_err(), "malformed");

        // 60s leeway: a token expired 30s ago still verifies.
        let mut c = good_claims();
        c["exp"] = serde_json::json!(now() - 30);
        assert!(v.verify(&sign(&key, "k1", c)).await.is_ok());
    }

    #[tokio::test]
    async fn missing_audience_is_rejected() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        let v = verifier(&server).await;
        let mut c = good_claims();
        c.as_object_mut().unwrap().remove("aud");
        assert_eq!(
            v.verify(&sign(&key, "k1", c)).await.unwrap_err(),
            "bad_audience"
        );
    }

    #[tokio::test]
    async fn non_string_audience_is_rejected() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        let v = verifier(&server).await;
        let mut c = good_claims();
        c["aud"] = serde_json::json!(42);
        assert_eq!(
            v.verify(&sign(&key, "k1", c)).await.unwrap_err(),
            "bad_audience"
        );
    }

    #[tokio::test]
    async fn missing_issuer_claim_is_rejected() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        let v = verifier(&server).await;
        let mut c = good_claims();
        c.as_object_mut().unwrap().remove("iss");
        // The unverified `iss` peek (used to pick the issuer before any crypto
        // runs) fails to find the claim, so this is rejected as `malformed`
        // before jsonwebtoken's own required-claims check ever runs. Still a
        // hard rejection either way.
        assert_eq!(
            v.verify(&sign(&key, "k1", c)).await.unwrap_err(),
            "malformed"
        );
    }

    #[tokio::test]
    async fn missing_exp_is_rejected() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        let v = verifier(&server).await;
        let mut c = good_claims();
        c.as_object_mut().unwrap().remove("exp");
        assert_eq!(
            v.verify(&sign(&key, "k1", c)).await.unwrap_err(),
            "malformed"
        );
    }

    #[tokio::test]
    async fn oversized_token_is_malformed() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        let v = verifier(&server).await;
        let huge = format!("eyJ{}", "a".repeat(9000));
        assert_eq!(v.verify(&huge).await.unwrap_err(), "malformed");
    }

    #[tokio::test]
    async fn fractional_future_iat_is_not_yet_valid() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        let v = verifier(&server).await;
        let mut c = good_claims();
        c["iat"] = serde_json::json!((now() + 600) as f64 + 0.5);
        assert_eq!(
            v.verify(&sign(&key, "k1", c)).await.unwrap_err(),
            "not_yet_valid"
        );
    }

    #[tokio::test]
    async fn empty_jwks_response_keeps_previous_snapshot() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        let v = verifier(&server).await;
        *server.body.lock().unwrap() = Ok(serde_json::json!({ "keys": [] }));
        assert!(!v.refresh_issuer(0).await);
        assert!(v.verify(&sign(&key, "k1", good_claims())).await.is_ok());
    }

    #[tokio::test]
    async fn unknown_kid_refetches_once_per_gap() {
        let k1 = gen_key("k1");
        let server = serve_jwks(vec![k1.jwk.clone()]).await;
        let v = verifier(&server).await;
        let hits_after_boot = server.hits.load(std::sync::atomic::Ordering::SeqCst);

        // Rotate: publish k2, sign with k2. First sight of kid=k2 triggers a refetch.
        let k2 = gen_key("k2");
        *server.body.lock().unwrap() = Ok(serde_json::json!({ "keys": [k1.jwk, k2.jwk] }));
        assert!(v.verify(&sign(&k2, "k2", good_claims())).await.is_ok());
        assert_eq!(
            server.hits.load(std::sync::atomic::Ordering::SeqCst),
            hits_after_boot + 1
        );

        // A second unknown kid inside the gap does NOT fetch again.
        let k3 = gen_key("k3");
        assert_eq!(
            v.verify(&sign(&k3, "k3", good_claims())).await.unwrap_err(),
            "unknown_kid"
        );
        assert_eq!(
            server.hits.load(std::sync::atomic::Ordering::SeqCst),
            hits_after_boot + 1
        );
    }

    #[tokio::test]
    async fn stale_snapshot_survives_fetch_error() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        let v = verifier(&server).await;
        *server.body.lock().unwrap() = Err(());
        assert!(!v.refresh_issuer(0).await);
        assert!(v.verify(&sign(&key, "k1", good_claims())).await.is_ok());
    }

    #[tokio::test]
    async fn cold_start_with_unreachable_jwks_is_unknown_kid() {
        let key = gen_key("k1");
        let server = serve_jwks(vec![key.jwk.clone()]).await;
        *server.body.lock().unwrap() = Err(());
        let v = JwksVerifier::new(
            vec![issuer_cfg(&server.url)],
            reqwest::Client::new(),
            Arc::new(Metrics::new()),
            obleth_admin::AlertDispatcher::new(
                reqwest::Client::new(),
                obleth_config::AlertSettings::default(),
            ),
        );
        assert!(!v.refresh_issuer(0).await);
        assert_eq!(
            v.verify(&sign(&key, "k1", good_claims()))
                .await
                .unwrap_err(),
            "unknown_kid"
        );
    }
}
