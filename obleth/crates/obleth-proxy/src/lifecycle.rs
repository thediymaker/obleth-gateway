//! Model lifecycle at the request edge: what a caller gets from a model that
//! is on its way out ([`obleth_config::ModelLifecycle`]).
//!
//! - **Deprecated**: served as usual, and every response says so. The
//!   standard headers carry it — `Deprecation` (RFC 9745), `Sunset` (RFC
//!   8594) once a retirement date is set, and a `Link` to the replacement
//!   with `rel="successor-version"` — so a client library or proxy that knows
//!   them can warn without obleth-specific code, plus `x-obleth-model-*`
//!   headers a person can read at a glance.
//! - **Retired**: refused with `410 Gone` and an OpenAI-style error naming
//!   the replacement and the date. With `redirect` on, the replacement
//!   answers instead, and the response says the model was redirected.
//!
//! Decided per request from the cached route, so a status change takes
//! effect on the next registry refresh, and a deprecated model whose date has
//! passed is refused without anyone touching it.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header::HeaderName, HeaderValue, Response, StatusCode};
use chrono::{DateTime, Utc};
use obleth_config::{ModelLifecycle, ModelStatus, ResolvedModel};

use crate::state::AppState;

/// `active`, `deprecated` or `retired`: the status the request met.
pub(crate) const STATUS_HEADER: &str = "x-obleth-model-status";
/// The model callers should move to, when one is named.
pub(crate) const REPLACEMENT_HEADER: &str = "x-obleth-model-replacement";
/// On a redirected request: the retired model the caller named.
pub(crate) const REDIRECTED_FROM_HEADER: &str = "x-obleth-redirected-from";
/// The `error.code` of a refusal.
pub(crate) const RETIRED_CODE: &str = "model_retired";

/// What a request to a deprecated or retired model has to tell the caller.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Notice {
    /// The status in force when the request arrived.
    pub status: ModelStatus,
    /// The model the caller named (its canonical name).
    pub model: String,
    pub lifecycle: ModelLifecycle,
    /// The replacement answered in its place.
    pub redirected: bool,
}

/// What to do with a request, given the lifecycle of the model it resolved to.
pub(crate) enum Gate {
    /// Serve it; the notice, when there is one, goes on the response.
    Serve(Option<Notice>),
    /// Serve it with this route instead (a retired model with `redirect` on).
    Redirect(Arc<ResolvedModel>, Notice),
    /// Refuse it with this response.
    Refuse(Response<Body>),
}

/// Decide how to serve a request that resolved to `route` at `now`.
pub(crate) async fn gate(state: &AppState, route: &ResolvedModel, now: DateTime<Utc>) -> Gate {
    let lifecycle = &route.lifecycle;
    let status = lifecycle.status_at(now);
    let notice = |redirected| Notice {
        status,
        model: route.model_name.clone(),
        lifecycle: lifecycle.clone(),
        redirected,
    };
    match status {
        ModelStatus::Active => Gate::Serve(None),
        ModelStatus::Deprecated => Gate::Serve(Some(notice(false))),
        ModelStatus::Retired => {
            if lifecycle.redirect && !lifecycle.replacement.is_empty() {
                match crate::proxy::resolve_model(state, &lifecycle.replacement).await {
                    Some(next) if can_stand_in(route, &next, now) => {
                        return Gate::Redirect(next, notice(true));
                    }
                    _ => tracing::warn!(
                        model = %route.model_name,
                        replacement = %lifecycle.replacement,
                        "retired model's replacement is unavailable; refusing instead of redirecting"
                    ),
                }
            }
            Gate::Refuse(refusal(&notice(false)))
        }
    }
}

/// Whether `next` may answer for the retired `route`: enabled, of the same
/// type, and not itself retired (a redirect never chains).
fn can_stand_in(route: &ResolvedModel, next: &ResolvedModel, now: DateTime<Utc>) -> bool {
    next.enabled
        && next.model_type == route.model_type
        && next.model_name != route.model_name
        && next.lifecycle.status_at(now) != ModelStatus::Retired
}

/// The sentence a caller reads in a refusal.
pub(crate) fn retired_message(notice: &Notice) -> String {
    let l = &notice.lifecycle;
    let mut msg = match l.retired_on() {
        Some(at) => format!(
            "The model `{}` was retired on {}.",
            notice.model,
            at.format("%Y-%m-%d")
        ),
        None => format!("The model `{}` has been retired.", notice.model),
    };
    if !l.replacement.is_empty() {
        msg.push_str(&format!(" Use `{}` instead.", l.replacement));
    }
    if !l.note.is_empty() {
        msg.push(' ');
        msg.push_str(&l.note);
    }
    msg
}

/// `410 Gone` for a retired model, in the OpenAI error shape, with the same
/// lifecycle headers a deprecated model's responses carry.
pub(crate) fn refusal(notice: &Notice) -> Response<Body> {
    let l = &notice.lifecycle;
    let mut error = serde_json::json!({
        "message": retired_message(notice),
        "type": "invalid_request_error",
        "param": "model",
        "code": RETIRED_CODE,
    });
    if let Some(obj) = error.as_object_mut() {
        if !l.replacement.is_empty() {
            obj.insert("replacement".into(), l.replacement.clone().into());
        }
        if let Some(at) = l.retired_on() {
            obj.insert("retired_at".into(), at.to_rfc3339().into());
        }
    }
    let body = serde_json::json!({ "error": error });
    let mut resp = Response::builder()
        .status(StatusCode::GONE)
        .header(axum::http::header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap_or_else(|_| Response::new(Body::empty()));
    apply_headers(&mut resp, notice);
    resp
}

/// Put the lifecycle headers for `notice` on `resp`.
pub(crate) fn apply_headers(resp: &mut Response<Body>, notice: &Notice) {
    for (name, value) in headers_for(notice) {
        if let (Ok(name), Ok(value)) = (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(&value),
        ) {
            resp.headers_mut().insert(name, value);
        }
    }
}

/// The lifecycle headers for `notice`, as `(name, value)` pairs.
fn headers_for(notice: &Notice) -> Vec<(&'static str, String)> {
    let l = &notice.lifecycle;
    let mut out = vec![(STATUS_HEADER, notice.status.as_str().to_string())];
    // RFC 9745: the moment the resource was (or will be) deprecated, as a
    // structured-field date. A retired model was deprecated too.
    if let Some(at) = l.changed_at {
        out.push(("deprecation", format!("@{}", at.timestamp())));
    }
    // RFC 8594: when it stops (or stopped) answering, as an HTTP-date.
    if let Some(at) = l.retired_on() {
        out.push(("sunset", http_date(at)));
    }
    if !l.replacement.is_empty() {
        out.push((REPLACEMENT_HEADER, l.replacement.clone()));
        out.push((
            "link",
            format!("</v1/models/{}>; rel=\"successor-version\"", l.replacement),
        ));
    }
    if notice.redirected {
        out.push((REDIRECTED_FROM_HEADER, notice.model.clone()));
    }
    out
}

/// An IMF-fixdate (RFC 9110 §5.6.7), the form `Sunset` takes.
fn http_date(at: DateTime<Utc>) -> String {
    at.format("%a, %d %b %Y %H:%M:%S GMT").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn notice(status: ModelStatus, lifecycle: ModelLifecycle, redirected: bool) -> Notice {
        Notice {
            status,
            model: "glm-4-5v".into(),
            lifecycle,
            redirected,
        }
    }

    fn deprecated() -> ModelLifecycle {
        ModelLifecycle {
            status: ModelStatus::Deprecated,
            replacement: "gemma4-31b-it".into(),
            retire_at: Some(at("2026-10-19T00:00:00Z")),
            changed_at: Some(at("2026-10-05T16:00:00Z")),
            note: "Ask rc@asu.edu.".into(),
            redirect: false,
        }
    }

    #[test]
    fn a_deprecated_model_carries_the_standard_headers() {
        let h = headers_for(&notice(ModelStatus::Deprecated, deprecated(), false));
        let get = |n: &str| h.iter().find(|(k, _)| *k == n).map(|(_, v)| v.as_str());
        assert_eq!(get(STATUS_HEADER), Some("deprecated"));
        assert_eq!(
            get("deprecation"),
            Some(format!("@{}", at("2026-10-05T16:00:00Z").timestamp()).as_str())
        );
        assert_eq!(get("sunset"), Some("Mon, 19 Oct 2026 00:00:00 GMT"));
        assert_eq!(get(REPLACEMENT_HEADER), Some("gemma4-31b-it"));
        assert_eq!(
            get("link"),
            Some("</v1/models/gemma4-31b-it>; rel=\"successor-version\"")
        );
        assert_eq!(get(REDIRECTED_FROM_HEADER), None);
    }

    #[test]
    fn a_bare_deprecation_sends_only_what_it_knows() {
        let l = ModelLifecycle {
            status: ModelStatus::Deprecated,
            ..Default::default()
        };
        let h = headers_for(&notice(ModelStatus::Deprecated, l, false));
        assert_eq!(h, vec![(STATUS_HEADER, "deprecated".to_string())]);
    }

    #[test]
    fn a_redirect_names_the_model_the_caller_asked_for() {
        let mut l = deprecated();
        l.status = ModelStatus::Retired;
        l.redirect = true;
        let h = headers_for(&notice(ModelStatus::Retired, l, true));
        assert!(h.contains(&(STATUS_HEADER, "retired".to_string())));
        assert!(h.contains(&(REDIRECTED_FROM_HEADER, "glm-4-5v".to_string())));
    }

    #[test]
    fn the_refusal_says_when_what_instead_and_why() {
        let n = notice(ModelStatus::Retired, deprecated(), false);
        assert_eq!(
            retired_message(&n),
            "The model `glm-4-5v` was retired on 2026-10-19. Use `gemma4-31b-it` instead. Ask rc@asu.edu."
        );
        let bare = notice(
            ModelStatus::Retired,
            ModelLifecycle {
                status: ModelStatus::Retired,
                ..Default::default()
            },
            false,
        );
        assert_eq!(
            retired_message(&bare),
            "The model `glm-4-5v` has been retired."
        );
    }

    #[tokio::test]
    async fn the_refusal_is_a_410_in_the_openai_error_shape() {
        let resp = refusal(&notice(ModelStatus::Retired, deprecated(), false));
        assert_eq!(resp.status(), StatusCode::GONE);
        assert_eq!(resp.headers()[STATUS_HEADER], "retired");
        assert_eq!(resp.headers()["sunset"], "Mon, 19 Oct 2026 00:00:00 GMT");
        let bytes = axum::body::to_bytes(resp.into_body(), 4096).await.unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"]["code"], RETIRED_CODE);
        assert_eq!(body["error"]["param"], "model");
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["replacement"], "gemma4-31b-it");
        assert_eq!(body["error"]["retired_at"], "2026-10-19T00:00:00+00:00");
    }

    #[test]
    fn a_stand_in_must_be_live_of_the_same_type_and_not_retired() {
        let now = Utc::now();
        let mut old = crate::boons::test_support::endpoint_only_route("http://unused/v1");
        old.model_name = "glm-4-5v".into();
        let mut next = old.clone();
        next.model_name = "gemma4-31b-it".into();
        assert!(can_stand_in(&old, &next, now));

        let mut disabled = next.clone();
        disabled.enabled = false;
        assert!(!can_stand_in(&old, &disabled, now));

        let mut other_type = next.clone();
        other_type.model_type = "embedding".into();
        assert!(!can_stand_in(&old, &other_type, now));

        let mut retired = next.clone();
        retired.lifecycle.status = ModelStatus::Retired;
        assert!(!can_stand_in(&old, &retired, now));

        // A deprecated stand-in is fine: it still answers.
        let mut deprecated = next;
        deprecated.lifecycle.status = ModelStatus::Deprecated;
        assert!(can_stand_in(&old, &deprecated, now));
    }
}
