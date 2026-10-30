//! `GET /v1/audio/voices?model=…`: the voices a text-to-speech model offers.
//!
//! OpenAI's speech API takes a `voice` but has no call that lists them, so a
//! client has nothing to build a voice picker from. Speech servers that
//! stream (vLLM-Omni, and OpenAI-compatible servers modelled on it) answer
//! `GET /v1/audio/voices` with `{"voices": [...], "uploaded_voices": [...]}`.
//! This endpoint forwards that call to the named model's server and passes
//! the answer through, the way `GET /v1/search/tools` lists search tools.
//!
//! The model is checked as a speech request would be: aliases resolve, a
//! retired model is refused or redirected, and the model must be an enabled
//! `audio_speech` model on the caller's allowlist. The call is a read of the
//! server's configuration, so it takes no fairshare slot, reserves no budget
//! and writes no ledger row. A server without the endpoint answers 404, which
//! reaches the client as is: it means "type a voice name".
//!
//! Speech itself (`POST /v1/audio/speech`) needs nothing here: the pipeline
//! streams any upstream body through as it arrives, so raw audio chunks and
//! `speech.audio.*` events reach the client while the rest is generated.

use std::collections::HashMap;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, HeaderMap, HeaderValue, Response, StatusCode};
use bytes::Bytes;
use obleth_config::{ResolvedKey, ResolvedModel};
use uuid::Uuid;

use crate::proxy::{
    self, build_targets, build_upstream_url, error_json, forward_headers, gate_resolved_key,
    resolve_model, Target,
};
use crate::state::AppState;

pub(crate) const VOICES_PATH: &str = "/v1/audio/voices";

/// The model type that serves `/v1/audio/speech`.
const SPEECH_MODEL_TYPE: &str = "audio_speech";

/// A voice list is a few names; a server that takes longer is not answering.
const VOICES_TIMEOUT: Duration = Duration::from_secs(10);

/// Largest voice listing passed through.
const VOICES_BODY_MAX: usize = 1 << 20;

/// `GET /v1/audio/voices`.
pub async fn voices(
    State(state): State<AppState>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
) -> Response<Body> {
    let request_id = Uuid::new_v4();
    let mut resp = handle(&state, query.get("model").map(String::as_str), &headers).await;
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        resp.headers_mut()
            .entry("x-obleth-request-id")
            .or_insert(value);
    }
    resp
}

async fn handle(state: &AppState, model: Option<&str>, headers: &HeaderMap) -> Response<Body> {
    let Some(secret) = proxy::bearer(headers) else {
        return error_json(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    let resolved = match crate::jwt_auth::authenticate_credential(state, &secret).await {
        Ok(cred) => cred.resolved,
        Err(resp) => return resp,
    };
    if let Err(resp) = gate_resolved_key(state, &resolved) {
        return resp;
    }
    let Some(name) = model.map(str::trim).filter(|n| !n.is_empty()) else {
        return error_json(
            StatusCode::BAD_REQUEST,
            "the `model` query parameter is required, e.g. /v1/audio/voices?model=my-tts",
        );
    };
    let Some(route) = resolve_model(state, name).await else {
        return error_json(
            StatusCode::NOT_FOUND,
            &format!("model '{name}' is not registered"),
        );
    };
    if let Err((status, msg)) = speech_model_usable(&route) {
        return error_json(status, &msg);
    }
    // A retired model is refused like it is for speech, or stood in for by
    // its replacement; the allowlist then applies to whichever answers.
    let route = match crate::lifecycle::gate(state, &route, chrono::Utc::now()).await {
        crate::lifecycle::Gate::Serve(_) => route,
        crate::lifecycle::Gate::Redirect(next, _) => next,
        crate::lifecycle::Gate::Refuse(resp, _) => return resp,
    };
    if !permitted(&resolved, &route, name) {
        return error_json(StatusCode::FORBIDDEN, "model not permitted for tenant");
    }
    let Some(target) = build_targets(Some(&route), &state.upstream_base, "failover", "")
        .into_iter()
        .next()
    else {
        return error_json(StatusCode::BAD_GATEWAY, "the model has no upstream");
    };
    fetch_voices(&state.http, &target, &route.upstream_model, headers).await
}

/// Whether `route` can answer a voices call at all.
fn speech_model_usable(route: &ResolvedModel) -> Result<(), (StatusCode, String)> {
    if route.model_type != SPEECH_MODEL_TYPE {
        return Err((
            StatusCode::BAD_REQUEST,
            format!(
                "'{}' is registered as '{}', not as a text-to-speech model",
                route.model_name, route.model_type
            ),
        ));
    }
    if !route.enabled {
        return Err((StatusCode::FORBIDDEN, "model is disabled".into()));
    }
    Ok(())
}

/// The tenant allowlist, which names a model by its own name or the name the
/// caller used (an alias or variant given on its own).
fn permitted(resolved: &ResolvedKey, route: &ResolvedModel, requested: &str) -> bool {
    resolved.internal
        || resolved.allowed_models.as_deref().is_none_or(|allowed| {
            allowed
                .iter()
                .any(|m| m == &route.model_name || m == requested)
        })
}

/// Ask the model's server for its voices and pass its answer through: status,
/// content type and body. The upstream model name rides along as `?model=`
/// for servers that host several; single-model servers ignore it.
async fn fetch_voices(
    http: &reqwest::Client,
    target: &Target,
    upstream_model: &str,
    client_headers: &HeaderMap,
) -> Response<Body> {
    let url = build_upstream_url(
        &target.base,
        VOICES_PATH,
        &format!("?model={}", encode_query_value(upstream_model)),
    );
    let mut headers = forward_headers(client_headers);
    headers.extend(target.headers.clone());
    if let Some(key) = &target.api_key {
        if let Ok(v) = HeaderValue::from_str(&format!("Bearer {key}")) {
            headers.insert(header::AUTHORIZATION, v);
        }
    }
    let call = async {
        let resp = http.get(&url).headers(headers).send().await?;
        let status = resp.status();
        let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();
        let body = read_capped(resp).await?;
        Ok::<_, reqwest::Error>((status, content_type, body))
    };
    let (status, content_type, body) = match tokio::time::timeout(VOICES_TIMEOUT, call).await {
        Ok(Ok(answer)) => answer,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, url = %url, "voices request failed");
            return error_json(StatusCode::BAD_GATEWAY, "upstream request failed");
        }
        Err(_) => return error_json(StatusCode::GATEWAY_TIMEOUT, "upstream request timed out"),
    };
    let mut builder = Response::builder().status(status.as_u16());
    if let Some(ct) = content_type {
        builder = builder.header(header::CONTENT_TYPE, ct);
    }
    builder
        .body(Body::from(body))
        .unwrap_or_else(|_| error_json(StatusCode::INTERNAL_SERVER_ERROR, "response build failed"))
}

async fn read_capped(resp: reqwest::Response) -> Result<Bytes, reqwest::Error> {
    use futures_util::StreamExt;
    let mut stream = resp.bytes_stream();
    let mut buf = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        let room = VOICES_BODY_MAX.saturating_sub(buf.len());
        buf.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if chunk.len() > room {
            break;
        }
    }
    Ok(Bytes::from(buf))
}

/// Percent-encode a query value, leaving only RFC 3986 unreserved characters.
fn encode_query_value(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use axum::extract::Query;
    use axum::routing::get;
    use axum::Router;

    use super::*;

    fn speech_route() -> ResolvedModel {
        let mut route = crate::boons::test_support::test_route();
        route.model_name = "reader".into();
        route.upstream_model = "org/reader 3b".into();
        route.model_type = SPEECH_MODEL_TYPE.into();
        route.enabled = true;
        route
    }

    #[test]
    fn only_enabled_speech_models_list_voices() {
        assert!(speech_model_usable(&speech_route()).is_ok());

        let mut chat = speech_route();
        chat.model_type = "chat".into();
        let (status, msg) = speech_model_usable(&chat).unwrap_err();
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(msg.contains("not as a text-to-speech model"), "{msg}");

        let mut off = speech_route();
        off.enabled = false;
        assert_eq!(
            speech_model_usable(&off).unwrap_err().0,
            StatusCode::FORBIDDEN
        );
    }

    #[test]
    fn the_allowlist_takes_the_model_or_the_name_called() {
        let route = speech_route();
        let mut key = crate::boons::test_support::test_key();
        key.internal = false;
        key.allowed_models = None;
        assert!(permitted(&key, &route, "reader"));

        key.allowed_models = Some(vec!["reader".into()]);
        assert!(permitted(&key, &route, "reader-alias"));
        key.allowed_models = Some(vec!["reader-alias".into()]);
        assert!(permitted(&key, &route, "reader-alias"));
        key.allowed_models = Some(vec!["other".into()]);
        assert!(!permitted(&key, &route, "reader"));

        key.internal = true;
        assert!(permitted(&key, &route, "reader"));
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode_query_value("org/reader 3b"), "org%2Freader%203b");
        assert_eq!(encode_query_value("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode_query_value("é&="), "%C3%A9%26%3D");
    }

    type Seen = Vec<(HashMap<String, String>, Option<String>)>;

    async fn spawn_server(app: Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{addr}/v1")
    }

    fn target(base: String) -> Target {
        Target {
            base,
            api_key: Some("up-key".into()),
            headers: HeaderMap::new(),
        }
    }

    #[tokio::test]
    async fn the_servers_voices_pass_through_with_its_key_and_model() {
        let seen: Arc<Mutex<Seen>> = Arc::default();
        let log = seen.clone();
        let app = Router::new().route(
            VOICES_PATH,
            get(
                move |Query(q): Query<HashMap<String, String>>, h: HeaderMap| {
                    let log = log.clone();
                    async move {
                        let auth = h
                            .get(header::AUTHORIZATION)
                            .and_then(|v| v.to_str().ok())
                            .map(str::to_string);
                        log.lock().unwrap().push((q, auth));
                        axum::Json(
                            serde_json::json!({ "voices": ["tara", "leo"], "uploaded_voices": [] }),
                        )
                    }
                },
            ),
        );
        let base = spawn_server(app).await;
        let mut client = HeaderMap::new();
        client.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer client-secret"),
        );

        let resp = fetch_voices(
            &reqwest::Client::new(),
            &target(base),
            "org/reader 3b",
            &client,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.headers()[header::CONTENT_TYPE], "application/json");
        let body = axum::body::to_bytes(resp.into_body(), 1 << 20)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["voices"], serde_json::json!(["tara", "leo"]));

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(
            seen[0].0.get("model").map(String::as_str),
            Some("org/reader 3b")
        );
        // The model's key replaces the caller's.
        assert_eq!(seen[0].1.as_deref(), Some("Bearer up-key"));
    }

    #[tokio::test]
    async fn a_server_without_the_endpoint_answers_404_through() {
        let base = spawn_server(Router::new()).await;
        let resp = fetch_voices(
            &reqwest::Client::new(),
            &target(base),
            "m",
            &HeaderMap::new(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn an_unreachable_server_is_a_bad_gateway() {
        let resp = fetch_voices(
            &reqwest::Client::new(),
            &target("http://127.0.0.1:1/v1".into()),
            "m",
            &HeaderMap::new(),
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}
