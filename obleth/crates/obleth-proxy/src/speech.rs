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
use obleth_config::ResolvedModel;
use uuid::Uuid;

use crate::proxy::{self, build_upstream_url, error_json, resolve_model, Target};
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
    client: crate::client::ClientInfo,
    headers: HeaderMap,
) -> Response<Body> {
    let request_id = Uuid::new_v4();
    let resp = handle(
        &state,
        query.get("model").map(String::as_str),
        &headers,
        &client,
    )
    .await;
    proxy::with_request_id(resp, request_id)
}

async fn handle(
    state: &AppState,
    model: Option<&str>,
    headers: &HeaderMap,
    client: &crate::client::ClientInfo,
) -> Response<Body> {
    let resolved = match proxy::authenticate_and_gate(state, headers, client).await {
        Ok(cred) => cred.resolved,
        Err(resp) => return resp,
    };
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
    if !crate::proxy::model_permitted(
        &resolved,
        &route,
        crate::proxy::variant_called(&route, name),
    ) {
        return error_json(StatusCode::FORBIDDEN, "model not permitted for tenant");
    }
    let target = match crate::boons::helper_target(&route, "", None) {
        Ok(target) => target,
        Err(_) => return error_json(StatusCode::BAD_GATEWAY, "the model has no upstream"),
    };
    fetch_voices(
        &state.http,
        &target,
        &route.upstream_model,
        headers,
        proxy::request_timeout(state, &route).min(VOICES_TIMEOUT),
    )
    .await
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

/// Ask the model's server for its voices and pass its answer through: status,
/// content type and body. The upstream model name rides along as `?model=`
/// for servers that host several; single-model servers ignore it.
async fn fetch_voices(
    http: &reqwest::Client,
    target: &Target,
    upstream_model: &str,
    client_headers: &HeaderMap,
    timeout: Duration,
) -> Response<Body> {
    let url = build_upstream_url(
        &target.base,
        VOICES_PATH,
        &format!("?model={}", proxy::encode_url_component(upstream_model)),
    );
    let headers = proxy::upstream_headers(client_headers, target);
    let call = async {
        let resp = http.get(&url).headers(headers).send().await?;
        let status = resp.status();
        let content_type = resp.headers().get(header::CONTENT_TYPE).cloned();
        // Over the cap is an error, never a listing cut short.
        let body = crate::boons::read_body_capped(resp, VOICES_BODY_MAX).await?;
        Ok::<_, anyhow::Error>((status, content_type, body))
    };
    let (status, content_type, body) = match tokio::time::timeout(timeout, call).await {
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

    /// The voices call applies the same allowlist rule as the pipeline (see
    /// `proxy::model_permitted`): the model's name covers its aliases and
    /// variants, a variant's own name covers that variant, an alias grants
    /// nothing.
    #[test]
    fn the_allowlist_rule_is_the_pipelines() {
        use crate::proxy::{model_permitted, variant_called};
        let mut route = speech_route();
        route.aliases = vec!["reader-alias".into()];
        route.variants = vec![obleth_config::ModelVariant {
            name: "reader-fast".into(),
            description: String::new(),
            boons: vec![],
        }];
        let permitted = |key: &obleth_config::ResolvedKey, requested: &str| {
            model_permitted(key, &route, variant_called(&route, requested))
        };
        let mut key = crate::boons::test_support::test_key();
        key.internal = false;
        key.allowed_models = None;
        assert!(permitted(&key, "reader"));
        key.allowed_models = Some(vec!["reader".into()]);
        assert!(permitted(&key, "reader-alias") && permitted(&key, "reader-fast"));
        key.allowed_models = Some(vec!["reader-fast".into()]);
        assert!(permitted(&key, "reader-fast") && !permitted(&key, "reader"));
        key.allowed_models = Some(vec!["reader-alias".into()]);
        assert!(
            !permitted(&key, "reader-alias"),
            "an alias on the list grants nothing"
        );
        key.allowed_models = Some(vec!["other".into()]);
        assert!(!permitted(&key, "reader"));
        key.internal = true;
        assert!(permitted(&key, "reader"));
    }

    #[test]
    fn query_values_are_percent_encoded() {
        use crate::proxy::encode_url_component as encode_query_value;
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
            VOICES_TIMEOUT,
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
            VOICES_TIMEOUT,
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
            VOICES_TIMEOUT,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }

    /// A listing over the cap is refused whole, never passed through cut short
    /// (a truncated JSON array would look like fewer voices).
    #[tokio::test]
    async fn an_oversize_voice_listing_is_an_error_not_a_shorter_list() {
        let app = Router::new().route(
            VOICES_PATH,
            axum::routing::get(|| async { "x".repeat(VOICES_BODY_MAX + 1) }),
        );
        let base = spawn_server(app).await;
        let resp = fetch_voices(
            &reqwest::Client::new(),
            &target(base),
            "m",
            &HeaderMap::new(),
            VOICES_TIMEOUT,
        )
        .await;
        assert_eq!(resp.status(), StatusCode::BAD_GATEWAY);
    }
}
