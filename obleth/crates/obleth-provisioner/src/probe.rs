use std::time::Duration;

/// `request` with the replica's API key as a bearer token, when it has one.
fn with_key(request: reqwest::RequestBuilder, api_key: Option<&str>) -> reqwest::RequestBuilder {
    match api_key {
        Some(key) => request.bearer_auth(key),
        None => request,
    }
}

/// True iff GET {api_base}{health_path} returns 2xx within the timeout.
/// Universal: no assumption beyond an HTTP health endpoint the operator named.
/// `api_key` is the replica's own key, sent in case the server guards its
/// health path too.
pub async fn is_healthy(
    http: &reqwest::Client,
    api_base: &str,
    health_path: &str,
    timeout_secs: u64,
    api_key: Option<&str>,
) -> bool {
    let url = format!("{}{}", api_base.trim_end_matches('/'), health_path);
    match with_key(http.get(&url), api_key)
        .timeout(Duration::from_secs(timeout_secs))
        .send()
        .await
    {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}

/// What one port in a replica's window answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortProbe {
    /// The health path answered 2xx, and the server lists the expected model
    /// on `/v1/models` (or does not list models at all).
    Serving,
    /// The health path answered, but the server lists other models: some
    /// other server holds this port.
    OtherModel(Vec<String>),
    /// No healthy answer.
    Down,
}

/// `http://host:port`, bracketing an IPv6 literal.
pub fn base_url(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("http://[{host}]:{port}")
    } else {
        format!("http://{host}:{port}")
    }
}

/// Probe one port: the health path first, then, when `expected` names the
/// model, `GET /v1/models` to make sure the server on the port is this
/// model's. A server that does not answer `/v1/models` with a model list
/// passes on its health path alone. Both requests carry the replica's
/// `api_key` when it has one, since a server started with a key answers
/// `/v1/models` only with it.
pub async fn probe_port(
    http: &reqwest::Client,
    host: &str,
    port: u16,
    health_path: &str,
    expected: &[String],
    timeout_secs: u64,
    api_key: Option<&str>,
) -> PortProbe {
    let base = base_url(host, port);
    if !is_healthy(http, &base, health_path, timeout_secs, api_key).await {
        return PortProbe::Down;
    }
    if expected.iter().all(|e| e.trim().is_empty()) {
        return PortProbe::Serving;
    }
    match served_models(http, &base, timeout_secs, api_key).await {
        Some(list) if !list.is_empty() && !list.iter().any(|m| names_match(m, expected)) => {
            PortProbe::OtherModel(list)
        }
        _ => PortProbe::Serving,
    }
}

/// The model ids a server lists on `GET /v1/models`, or `None` when it does
/// not answer with an OpenAI-style list.
async fn served_models(
    http: &reqwest::Client,
    base: &str,
    timeout_secs: u64,
    api_key: Option<&str>,
) -> Option<Vec<String>> {
    let resp = with_key(http.get(format!("{base}/v1/models")), api_key)
        .timeout(Duration::from_secs(timeout_secs))
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    model_ids(&v)
}

/// Whether the server on `host:port` turns away a request that carries no
/// API key: `Some(true)` when it answers 401 or 403, `Some(false)` when it
/// answers anything else, `None` when it did not answer. For a replica that
/// was started with a key, `Some(false)` means its job script never gave the
/// key to the server, which is then open to anything that can reach the port.
///
/// The request is an empty chat completion: every engine checks the key
/// before it reads the body, and a server without a key rejects the empty
/// body without running the model.
pub async fn refuses_without_key(
    http: &reqwest::Client,
    host: &str,
    port: u16,
    timeout_secs: u64,
) -> Option<bool> {
    let resp = http
        .post(format!("{}/v1/chat/completions", base_url(host, port)))
        .timeout(Duration::from_secs(timeout_secs))
        .json(&serde_json::json!({}))
        .send()
        .await
        .ok()?;
    Some(matches!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ))
}

/// The `data[].id` values of an OpenAI-style model list.
fn model_ids(v: &serde_json::Value) -> Option<Vec<String>> {
    Some(
        v.get("data")?
            .as_array()?
            .iter()
            .filter_map(|m| m.get("id").and_then(|x| x.as_str()))
            .map(str::to_string)
            .collect(),
    )
}

/// True when a served model id is one of the expected names. A `:latest`
/// tag is ignored on both sides (Ollama lists `name:latest` for `name`).
pub fn names_match(served: &str, expected: &[String]) -> bool {
    fn norm(s: &str) -> &str {
        let s = s.trim();
        s.strip_suffix(":latest").unwrap_or(s)
    }
    expected
        .iter()
        .filter(|e| !e.trim().is_empty())
        .any(|e| norm(e) == norm(served))
}

/// The replica detail for a port held by another model's server.
pub fn other_model_detail(port: u16, served: &[String], expected: &str) -> String {
    format!(
        "The server on port {port} serves {}, not {expected}. Check the served model name.",
        served.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_ids_read_an_openai_list_only() {
        let v = serde_json::json!({"object": "list", "data": [{"id": "a"}, {"id": "b"}]});
        assert_eq!(model_ids(&v), Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(model_ids(&serde_json::json!({"status": "ok"})), None);
    }

    #[test]
    fn names_match_exactly_or_by_latest_tag() {
        let want = vec!["my-model".to_string(), "".to_string()];
        assert!(names_match("my-model", &want));
        assert!(names_match("my-model:latest", &want));
        assert!(!names_match("qwen3", &want));
        assert!(!names_match("", &want));
    }

    #[test]
    fn other_model_detail_names_both() {
        assert_eq!(
            other_model_detail(8001, &["qwen3".into()], "my-model"),
            "The server on port 8001 serves qwen3, not my-model. Check the served model name."
        );
    }

    #[test]
    fn base_url_brackets_ipv6() {
        assert_eq!(base_url("10.0.0.1", 8000), "http://10.0.0.1:8000");
        assert_eq!(base_url("::1", 8000), "http://[::1]:8000");
    }

    /// A one-route HTTP stub: answers every request on the health path with
    /// 200 and `/v1/models` with the given list.
    async fn stub(models: &'static str) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let body = if req.starts_with("GET /v1/models") {
                        models
                    } else {
                        "ok"
                    };
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn probe_port_requires_the_expected_model_when_listed() {
        let http = reqwest::Client::new();
        let want = vec!["my-model".to_string()];
        let port = stub(r#"{"data":[{"id":"qwen3"}]}"#).await;
        assert_eq!(
            probe_port(&http, "127.0.0.1", port, "/health", &want, 2, None).await,
            PortProbe::OtherModel(vec!["qwen3".into()])
        );
        let port = stub(r#"{"data":[{"id":"my-model"}]}"#).await;
        assert_eq!(
            probe_port(&http, "127.0.0.1", port, "/health", &want, 2, None).await,
            PortProbe::Serving
        );
        // No model list: the health path alone decides.
        let port = stub("not json").await;
        assert_eq!(
            probe_port(&http, "127.0.0.1", port, "/health", &want, 2, None).await,
            PortProbe::Serving
        );
    }

    /// A server started with `key`, the way the engines behave: the health
    /// path is open, and everything under `/v1` answers 401 without the key.
    /// It lists `qwen3`, and rejects an empty chat body with 400.
    async fn keyed_stub(key: Option<&'static str>) -> u16 {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                tokio::spawn(async move {
                    let mut buf = [0u8; 2048];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_lowercase();
                    let path = req.split_whitespace().nth(1).unwrap_or("").to_string();
                    let authorized = match key {
                        Some(k) => req.contains(&format!("authorization: bearer {k}")),
                        None => true,
                    };
                    let (status, body) = if !path.starts_with("/v1") {
                        ("200 OK", "ok")
                    } else if !authorized {
                        ("401 Unauthorized", r#"{"error":"Unauthorized"}"#)
                    } else if path == "/v1/models" {
                        ("200 OK", r#"{"data":[{"id":"qwen3"}]}"#)
                    } else {
                        ("400 Bad Request", r#"{"error":"messages is required"}"#)
                    };
                    let resp = format!(
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                });
            }
        });
        port
    }

    #[tokio::test]
    async fn the_model_list_is_read_with_the_replicas_key() {
        let http = reqwest::Client::new();
        let want = vec!["my-model".to_string()];
        let port = keyed_stub(Some("k1")).await;
        // With its key the list is read, and it names another model.
        assert_eq!(
            probe_port(&http, "127.0.0.1", port, "/health", &want, 2, Some("k1")).await,
            PortProbe::OtherModel(vec!["qwen3".into()])
        );
        // Without it the list cannot be read, and the health path decides.
        assert_eq!(
            probe_port(&http, "127.0.0.1", port, "/health", &want, 2, None).await,
            PortProbe::Serving
        );
    }

    #[tokio::test]
    async fn a_server_is_asked_whether_it_refuses_a_request_without_a_key() {
        let http = reqwest::Client::new();
        let keyed = keyed_stub(Some("k1")).await;
        assert_eq!(
            refuses_without_key(&http, "127.0.0.1", keyed, 2).await,
            Some(true)
        );
        // A server that never got the key answers the empty request itself.
        let open = keyed_stub(None).await;
        assert_eq!(
            refuses_without_key(&http, "127.0.0.1", open, 2).await,
            Some(false)
        );
        // No answer is not an answer.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let closed = listener.local_addr().unwrap().port();
        drop(listener);
        assert_eq!(
            refuses_without_key(&http, "127.0.0.1", closed, 2).await,
            None
        );
    }
}
