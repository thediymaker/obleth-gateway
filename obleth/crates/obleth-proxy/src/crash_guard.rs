//! Crash guard: one request must not take down every server of a model.
//!
//! Some requests stop a model server's engine outright (a shape its graph
//! compiler rejects, a field its runner mishandles). The server answers every
//! request it had in flight with vLLM's `EngineCore encountered an issue`
//! and restarts, which on large models takes from minutes to over an hour.
//! Clients retry what failed, the retry lands on the next replica and stops
//! that one too: on 2026-11-06 one agent turn and its retry, two seconds
//! apart, took both replicas of mimo-v2-6-flash down for 85 minutes.
//!
//! The gateway cannot tell which of the requests in flight killed the engine,
//! so it counts strikes. Every request that was being answered when an engine
//! died gets one strike against its fingerprint (the API key, the model and
//! the body less fields that do not change the answer), kept in Redis so
//! every gateway replica sees it. The key is part of it because a retry comes
//! from the caller that sent the request: another caller sending the same
//! prompt is not held back for it. Strikes count only while they come close
//! together: one more than [`STRIKE_WINDOW`] after the last starts the count
//! again, so a request that happens to be in flight at two unrelated deaths
//! (a large fleet loses a server now and then) is not refused for it. A
//! record is forgotten [`STRIKE_TTL`] after its last strike.
//!
//! Synthetic tenants (benchmarks, probes) collect no strikes: they repeat the
//! same bodies on a schedule, and a refused probe would read as an outage.
//! Neither does an engine-dead answer while one of the model's servers is
//! being shut down (capacity discovery sees it `terminating`): vLLM answers
//! requests still running when a planned drain runs out of time with the
//! same error.
//!
//! - No strike: the request goes through.
//! - One strike (a suspect): it goes through while the model has at least
//!   two ready servers, so a suspect never lands on the last server left
//!   standing. With one or none it is held with `503` and `Retry-After`
//!   until a second server is back. A model that usually runs one server
//!   lets it through once that server is back: there is nothing else to
//!   protect, and a second crash settles it.
//! - Two strikes: the request has been in flight at two engine deaths within
//!   [`STRIKE_WINDOW`], which chance does not explain; it is refused with
//!   `422` until its record expires. A changed request has a new
//!   fingerprint and goes through.
//!
//! A big fleet that loses one server still has others, so its suspects go
//! through: at worst a request that does kill engines takes a second server
//! before its second strike refuses it. "Usually runs one" is the most ready
//! servers this gateway replica has seen for the model in the last
//! [`PEAK_WINDOW`]. A model capacity discovery does not watch (no ready count)
//! lets suspects through. Redis errors let requests through (fail open).
//! `OBLETH_CRASH_GUARD=false` turns the guard off.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use obleth_admin::capacity_discovery::CapacityDiscovery;
use obleth_redis::RedisStore;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// How long a strike record is kept after its last strike.
pub(crate) const STRIKE_TTL: Duration = Duration::from_secs(6 * 3600);
/// How close together strikes must come to add up.
pub(crate) const STRIKE_WINDOW: Duration = Duration::from_secs(3600);
/// How long the most ready servers seen for a model counts as its strength.
pub(crate) const PEAK_WINDOW: Duration = Duration::from_secs(3 * 3600);
/// What a held suspect is told to wait before sending again.
pub(crate) const HOLD_RETRY_AFTER: Duration = Duration::from_secs(300);

/// Body fields that do not change what the model computes, left out of the
/// fingerprint so a retry that flips one of them is still the same request.
const VOLATILE_FIELDS: &[&str] = &["stream", "stream_options", "user", "metadata", "store"];

/// What the guard decides for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Verdict {
    Pass,
    /// A suspect while the model is short of servers.
    Hold {
        ready: usize,
        usual: usize,
    },
    /// In flight at two or more engine deaths.
    Refuse {
        strikes: u32,
    },
}

/// Whether a failure reason is a model server's engine dying under the
/// request (vLLM's `EngineDeadError`), as opposed to a refusal or a timeout.
pub(crate) fn is_engine_death(reason: &str) -> bool {
    reason.contains("EngineCore encountered an issue") || reason.contains("EngineDeadError")
}

/// The fingerprint of a request from API key `key` to `model`: a hash of
/// the key, the model name and the body without [`VOLATILE_FIELDS`].
pub(crate) fn fingerprint(key: &uuid::Uuid, model: &str, body: &Value) -> String {
    /// Serialized JSON goes straight into the hash, not into a copy first.
    struct Hashing<'a>(&'a mut Sha256);
    impl std::io::Write for Hashing<'_> {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.update(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut h = Sha256::new();
    h.update(key.as_bytes());
    h.update(model.as_bytes());
    h.update([0u8]);
    match body {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map
                .keys()
                .filter(|k| !VOLATILE_FIELDS.contains(&k.as_str()))
                .collect();
            keys.sort();
            for k in keys {
                h.update(k.as_bytes());
                h.update([0u8]);
                let _ = serde_json::to_writer(Hashing(&mut h), &map[k.as_str()]);
                h.update([0u8]);
            }
        }
        other => {
            let _ = serde_json::to_writer(Hashing(&mut h), other);
        }
    }
    let digest = h.finalize();
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// The decision for a request with `strikes`, when the model has `ready`
/// servers now and usually `usual` (both `None` when unknown).
pub(crate) fn decide(strikes: u32, ready: Option<usize>, usual: Option<usize>) -> Verdict {
    match strikes {
        0 => Verdict::Pass,
        1 => match ready {
            None => Verdict::Pass,
            Some(ready) => {
                let usual = usual.unwrap_or(ready).max(ready);
                if ready >= 2 || (ready == 1 && usual <= 1) {
                    Verdict::Pass
                } else {
                    Verdict::Hold { ready, usual }
                }
            }
        },
        n => Verdict::Refuse { strikes: n },
    }
}

/// Fold a reading of `ready` servers into a model's `(peak, seen at)` and
/// return the peak: a new high (or the same) resets it, and an old peak that
/// has not been seen for [`PEAK_WINDOW`] gives way to the reading.
fn update_peak(entry: &mut (usize, Instant), ready: usize, now: Instant) -> usize {
    if ready >= entry.0 || now.duration_since(entry.1) > PEAK_WINDOW {
        *entry = (ready, now);
    }
    entry.0
}

/// The guard's state on one gateway replica. Cheap to clone.
#[derive(Clone)]
pub struct CrashGuard {
    enabled: bool,
    discovery: Option<CapacityDiscovery>,
    peaks: Arc<Mutex<HashMap<String, (usize, Instant)>>>,
    /// Models that may have live strike records, and until when. Only their
    /// requests' strikes are looked up in Redis: a model without one has
    /// none to find.
    recent: Arc<Mutex<HashMap<String, Instant>>>,
    /// Whether `recent` is complete: false until the first read of Redis,
    /// and again after a failed one, when every request's strikes are
    /// looked up.
    known: Arc<std::sync::atomic::AtomicBool>,
}

/// How often each replica reads which models have strike records, in case it
/// missed the message a strike sends (it was starting, or disconnected).
pub(crate) const RECENT_REFRESH: Duration = Duration::from_secs(60);

impl CrashGuard {
    pub fn new(discovery: Option<CapacityDiscovery>) -> Self {
        let enabled = std::env::var("OBLETH_CRASH_GUARD")
            .ok()
            .and_then(|v| obleth_config::config::parse_bool(&v))
            .unwrap_or(true);
        CrashGuard {
            enabled,
            discovery,
            peaks: Arc::default(),
            recent: Arc::default(),
            known: Arc::default(),
        }
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        CrashGuard {
            enabled: false,
            discovery: None,
            peaks: Arc::default(),
            recent: Arc::default(),
            known: Arc::default(),
        }
    }

    /// Note that `model` may have strike records for `left` from now.
    pub(crate) fn note_strike(&self, model: &str, left: Duration) {
        let until = Instant::now() + left;
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        match recent.get_mut(model) {
            Some(at) => *at = (*at).max(until),
            None => {
                recent.insert(model.to_string(), until);
            }
        }
    }

    fn may_have_strikes(&self, model: &str) -> bool {
        if !self.known.load(std::sync::atomic::Ordering::Relaxed) {
            return true;
        }
        let now = Instant::now();
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.retain(|_, until| *until > now);
        recent.contains_key(model)
    }

    /// Keep [`Self::recent`] in step with Redis every [`RECENT_REFRESH`],
    /// from the first moment on.
    pub(crate) fn spawn_refresh(&self, redis: RedisStore) {
        if !self.enabled {
            return;
        }
        let guard = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(RECENT_REFRESH);
            loop {
                tick.tick().await;
                match redis.crash_recent_models().await {
                    Ok(models) => {
                        for (model, left) in models {
                            guard.note_strike(&model, left);
                        }
                        guard
                            .known
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(e) => {
                        // Until a read works, look strikes up for every model.
                        tracing::warn!(error = %e, "crash guard: could not read which models have strikes");
                        guard
                            .known
                            .store(false, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            }
        });
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Ready servers now and the model's usual number, updating the usual
    /// number from this reading. Every guarded request reads it, so a busy
    /// model's usual number is known before its first suspect arrives.
    fn strength(&self, model: &str) -> (Option<usize>, Option<usize>) {
        let ready = self
            .discovery
            .as_ref()
            .and_then(|d| d.ready_replicas(model));
        let Some(ready) = ready else {
            return (None, None);
        };
        let now = Instant::now();
        let mut peaks = self.peaks.lock().unwrap_or_else(|e| e.into_inner());
        let peak = match peaks.get_mut(model) {
            Some(entry) => update_peak(entry, ready, now),
            None => {
                peaks.insert(model.to_string(), (ready, now));
                ready
            }
        };
        (Some(ready), Some(peak))
    }

    /// The verdict for a request to `model` with fingerprint `fp`.
    pub(crate) async fn check(&self, redis: &RedisStore, model: &str, fp: &str) -> Verdict {
        if !self.enabled || fp.is_empty() {
            return Verdict::Pass;
        }
        let (ready, usual) = self.strength(model);
        if !self.may_have_strikes(model) {
            return Verdict::Pass;
        }
        let strikes = match redis.crash_strikes(model, fp).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, model, "crash guard: strike lookup failed; letting the request through");
                return Verdict::Pass;
            }
        };
        decide(strikes, ready, usual)
    }

    /// Whether capacity discovery saw one of `model`'s servers being shut
    /// down (a rollout or scale-down) on its last pass.
    fn shutting_down(&self, model: &str) -> bool {
        self.discovery
            .as_ref()
            .and_then(|d| d.terminating_replicas(model))
            .is_some_and(|n| n > 0)
    }

    /// Record that a request with fingerprint `fp` was in flight when one of
    /// `model`'s engines died. Not while one of the model's servers is being
    /// shut down: vLLM ends a drain that runs out of time with the same
    /// engine-dead error, for requests still running then.
    pub(crate) async fn record_death(&self, redis: &RedisStore, model: &str, fp: &str) {
        if !self.enabled || fp.is_empty() {
            return;
        }
        if self.shutting_down(model) {
            tracing::info!(
                model,
                fingerprint = fp,
                "crash guard: engine-dead answer while a server is shutting down; not a strike"
            );
            return;
        }
        match redis
            .crash_strike(model, fp, STRIKE_WINDOW, STRIKE_TTL)
            .await
        {
            Ok(n) => {
                tracing::warn!(
                    model,
                    fingerprint = fp,
                    strikes = n,
                    "crash guard: request was in flight when the model server's engine died"
                );
                // This replica checks the model's requests at once; the
                // others when the message arrives.
                self.note_strike(model, STRIKE_TTL);
                let notice = obleth_redis::Invalidation::CrashStrike(model.to_string());
                if let Err(e) = redis.publish_invalidation(&notice).await {
                    tracing::warn!(error = %e, model, "crash guard: other replicas not told of the strike; they read it within a minute");
                }
            }
            Err(e) => tracing::warn!(error = %e, model, "crash guard: strike not recorded"),
        }
    }
}

/// The answer for a request the guard does not send: the OpenAI error shape,
/// with `Retry-After` on a hold.
pub(crate) fn guard_response(
    status: axum::http::StatusCode,
    msg: &str,
) -> axum::response::Response<axum::body::Body> {
    use axum::response::IntoResponse;
    let body = serde_json::json!({ "error": {
        "message": msg,
        "type": if status.is_server_error() { "server_error" } else { "invalid_request_error" },
        "code": "crash_guard",
    } });
    let mut resp = (status, axum::Json(body)).into_response();
    if status == axum::http::StatusCode::SERVICE_UNAVAILABLE {
        resp.headers_mut().insert(
            axum::http::header::RETRY_AFTER,
            axum::http::HeaderValue::from(HOLD_RETRY_AFTER.as_secs()),
        );
    }
    resp
}

/// The message a held suspect gets.
pub(crate) fn hold_message(model: &str, ready: usize, usual: usize) -> String {
    format!(
        "This exact request was being answered when a server for `{model}` crashed, and the model \
         is down to {ready} of its {usual} servers. So that it cannot take down the last one, the \
         gateway holds this request back until a second server is ready again (usually minutes; \
         large models can take over an hour). Send it again later, or change it."
    )
}

/// The message a refused request gets.
pub(crate) fn refuse_message(model: &str, strikes: u32) -> String {
    format!(
        "This exact request was being answered each time a server for `{model}` crashed \
         ({strikes} times), so the gateway will not send it again for a few hours. A changed \
         request (different messages or settings) is sent as usual."
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn engine_death_text_is_recognised() {
        assert!(is_engine_death(
            "EngineCore encountered an issue. See stack trace (above) for the root cause."
        ));
        assert!(is_engine_death(
            "vllm.v1.engine.exceptions.EngineDeadError: boom"
        ));
        assert!(!is_engine_death(
            "no answer from the model server within 3600 s"
        ));
        assert!(!is_engine_death(
            "could not reach the model server: Connection refused (os error 111)"
        ));
    }

    /// Hashed without copying, a fingerprint is the one recorded before, so
    /// strikes already in Redis still match.
    #[test]
    fn fingerprints_are_unchanged() {
        let key = uuid::Uuid::from_u128(7);
        let body = json!({
            "model": "m", "stream": true,
            "messages": [{"role": "user", "content": "hi \u{e9}"}],
            "temperature": 0.25,
        });
        let mut h = Sha256::new();
        h.update(key.as_bytes());
        h.update(b"m");
        h.update([0u8]);
        for k in ["messages", "model", "temperature"] {
            h.update(k.as_bytes());
            h.update([0u8]);
            h.update(serde_json::to_vec(&body[k]).unwrap());
            h.update([0u8]);
        }
        let old: String = h.finalize()[..16]
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(fingerprint(&key, "m", &body), old);
    }

    /// A model with no strike record on any replica costs no Redis lookup;
    /// once a strike is noted (here or by message), its requests are checked
    /// again. Integration test; runs only with `OBLETH_TEST_REDIS_URL`.
    #[tokio::test]
    async fn only_models_with_strikes_are_looked_up() {
        let Ok(url) = std::env::var("OBLETH_TEST_REDIS_URL") else {
            eprintln!("skipping: set OBLETH_TEST_REDIS_URL to run");
            return;
        };
        let redis = RedisStore::connect(&url).await.expect("connect");
        let model = format!("crash-test-{}", uuid::Uuid::new_v4().simple());
        let guard = CrashGuard {
            enabled: true,
            ..CrashGuard::disabled()
        };
        guard
            .known
            .store(true, std::sync::atomic::Ordering::Relaxed);
        for _ in 0..2 {
            redis
                .crash_strike(&model, "fp", STRIKE_WINDOW, Duration::from_secs(60))
                .await
                .unwrap();
        }
        // Struck in Redis, but this replica has not heard: no lookup.
        assert_eq!(guard.check(&redis, &model, "fp").await, Verdict::Pass);
        // The refresh finds it.
        let recent = redis.crash_recent_models().await.unwrap();
        let (_, left) = recent.iter().find(|(m, _)| *m == model).expect("listed");
        guard.note_strike(&model, *left);
        assert_eq!(
            guard.check(&redis, &model, "fp").await,
            Verdict::Refuse { strikes: 2 }
        );
        // Not knowing which models have strikes means looking every one up.
        let other = format!("crash-test-{}", uuid::Uuid::new_v4().simple());
        redis
            .crash_strike(&other, "fp", STRIKE_WINDOW, Duration::from_secs(60))
            .await
            .unwrap();
        redis
            .crash_strike(&other, "fp", STRIKE_WINDOW, Duration::from_secs(60))
            .await
            .unwrap();
        guard
            .known
            .store(false, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            guard.check(&redis, &other, "fp").await,
            Verdict::Refuse { strikes: 2 }
        );
    }

    #[test]
    fn fingerprint_ignores_volatile_fields_and_key_order() {
        let a = json!({"messages": [{"role": "user", "content": "hi"}], "max_tokens": 5, "stream": true});
        let b = json!({"stream": false, "max_tokens": 5, "user": "x",
                       "messages": [{"role": "user", "content": "hi"}]});
        let k = uuid::Uuid::new_v4();
        assert_eq!(fingerprint(&k, "m", &a), fingerprint(&k, "m", &b));
        let c = json!({"messages": [{"role": "user", "content": "hi!"}], "max_tokens": 5});
        assert_ne!(fingerprint(&k, "m", &a), fingerprint(&k, "m", &c));
        assert_ne!(fingerprint(&k, "m", &a), fingerprint(&k, "n", &a));
        assert_eq!(fingerprint(&k, "m", &a).len(), 32);
        // Another caller sending the same body is another request.
        assert_ne!(
            fingerprint(&k, "m", &a),
            fingerprint(&uuid::Uuid::new_v4(), "m", &a)
        );
    }

    #[test]
    fn no_strike_always_passes() {
        assert_eq!(decide(0, Some(0), Some(4)), Verdict::Pass);
        assert_eq!(decide(0, None, None), Verdict::Pass);
    }

    #[test]
    fn suspect_is_held_while_the_model_is_short_of_servers() {
        // mimo-v2-6-flash on 11-06: 2 servers, one crashed.
        assert_eq!(
            decide(1, Some(1), Some(2)),
            Verdict::Hold { ready: 1, usual: 2 }
        );
        // Both down.
        assert_eq!(
            decide(1, Some(0), Some(2)),
            Verdict::Hold { ready: 0, usual: 2 }
        );
        // Back to two: one more server may go, one stays.
        assert_eq!(decide(1, Some(2), Some(2)), Verdict::Pass);
        // A big fleet that lost one of twenty keeps serving its suspects.
        assert_eq!(decide(1, Some(19), Some(20)), Verdict::Pass);
        assert_eq!(decide(1, Some(2), Some(3)), Verdict::Pass);
    }

    #[test]
    fn single_server_model_lets_a_suspect_through_once_it_is_back() {
        assert_eq!(decide(1, Some(1), Some(1)), Verdict::Pass);
        assert_eq!(
            decide(1, Some(0), Some(1)),
            Verdict::Hold { ready: 0, usual: 1 }
        );
    }

    #[test]
    fn unknown_strength_lets_a_suspect_through() {
        assert_eq!(decide(1, None, None), Verdict::Pass);
    }

    #[test]
    fn two_strikes_refuse() {
        assert_eq!(decide(2, Some(4), Some(4)), Verdict::Refuse { strikes: 2 });
        assert_eq!(decide(3, None, None), Verdict::Refuse { strikes: 3 });
    }

    #[test]
    fn usual_strength_follows_the_peak_and_forgets_it_after_the_window() {
        let t0 = Instant::now();
        let mut e = (2, t0);
        assert_eq!(update_peak(&mut e, 1, t0 + Duration::from_secs(60)), 2);
        assert_eq!(update_peak(&mut e, 3, t0 + Duration::from_secs(120)), 3);
        let later = t0 + Duration::from_secs(120) + PEAK_WINDOW + Duration::from_secs(1);
        assert_eq!(update_peak(&mut e, 1, later), 1);
        let g = CrashGuard::disabled();
        assert_eq!(g.strength("m"), (None, None));
    }
}
