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
//! died gets one strike against its fingerprint (the model plus its body,
//! less fields that do not change the answer), kept in Redis for
//! [`STRIKE_TTL`] so every gateway replica sees it.
//!
//! - No strike: the request goes through.
//! - One strike (a suspect): it goes through while the model has at least
//!   two ready servers, so a suspect never lands on the last server left
//!   standing. With one or none it is held with `503` and `Retry-After`
//!   until a second server is back. A model that usually runs one server
//!   lets it through once that server is back: there is nothing else to
//!   protect, and a second crash settles it.
//! - Two strikes: the request has been in flight at two engine deaths, which
//!   chance does not explain; it is refused with `422` until its strikes
//!   expire. A changed request has a new fingerprint and goes through.
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

/// How long a strike is remembered.
pub(crate) const STRIKE_TTL: Duration = Duration::from_secs(6 * 3600);
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

/// The fingerprint of a request to `model`: a hash of the model name and the
/// body without [`VOLATILE_FIELDS`].
pub(crate) fn fingerprint(model: &str, body: &Value) -> String {
    let mut h = Sha256::new();
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
                h.update(serde_json::to_vec(&map[k.as_str()]).unwrap_or_default());
                h.update([0u8]);
            }
        }
        other => h.update(serde_json::to_vec(other).unwrap_or_default()),
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
}

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
        }
    }

    #[cfg(test)]
    pub fn disabled() -> Self {
        CrashGuard {
            enabled: false,
            discovery: None,
            peaks: Arc::default(),
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    /// Ready servers now and the model's usual number, updating the usual
    /// number from this reading. Every guarded request reads it, so a busy
    /// model's usual number is known before its first suspect arrives.
    fn strength(&self, model: &str) -> (Option<usize>, Option<usize>) {
        let ready = self.discovery.as_ref().and_then(|d| {
            d.statuses()
                .into_iter()
                .find(|s| s.model_name == model)
                .and_then(|s| s.ready_replicas)
        });
        let Some(ready) = ready else {
            return (None, None);
        };
        let now = Instant::now();
        let mut peaks = self.peaks.lock().unwrap_or_else(|e| e.into_inner());
        let entry = peaks.entry(model.to_string()).or_insert((ready, now));
        (Some(ready), Some(update_peak(entry, ready, now)))
    }

    /// The verdict for a request to `model` with fingerprint `fp`.
    pub(crate) async fn check(&self, redis: &RedisStore, model: &str, fp: &str) -> Verdict {
        if !self.enabled || fp.is_empty() {
            return Verdict::Pass;
        }
        let (ready, usual) = self.strength(model);
        let strikes = match redis.crash_strikes(model, fp).await {
            Ok(n) => n,
            Err(e) => {
                tracing::warn!(error = %e, model, "crash guard: strike lookup failed; letting the request through");
                return Verdict::Pass;
            }
        };
        decide(strikes, ready, usual)
    }

    /// Record that a request with fingerprint `fp` was in flight when one of
    /// `model`'s engines died.
    pub(crate) async fn record_death(&self, redis: &RedisStore, model: &str, fp: &str) {
        if !self.enabled || fp.is_empty() {
            return;
        }
        match redis.crash_strike(model, fp, STRIKE_TTL.as_secs()).await {
            Ok(n) => tracing::warn!(
                model,
                fingerprint = fp,
                strikes = n,
                "crash guard: request was in flight when the model server's engine died"
            ),
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

    #[test]
    fn fingerprint_ignores_volatile_fields_and_key_order() {
        let a = json!({"messages": [{"role": "user", "content": "hi"}], "max_tokens": 5, "stream": true});
        let b = json!({"stream": false, "max_tokens": 5, "user": "x",
                       "messages": [{"role": "user", "content": "hi"}]});
        assert_eq!(fingerprint("m", &a), fingerprint("m", &b));
        let c = json!({"messages": [{"role": "user", "content": "hi!"}], "max_tokens": 5});
        assert_ne!(fingerprint("m", &a), fingerprint("m", &c));
        assert_ne!(fingerprint("m", &a), fingerprint("n", &a));
        assert_eq!(fingerprint("m", &a).len(), 32);
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
