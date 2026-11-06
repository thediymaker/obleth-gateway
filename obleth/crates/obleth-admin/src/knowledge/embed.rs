//! Calling a registered embedding model's upstream directly.
//!
//! Indexing and query embedding both go straight to the upstream rather than
//! back through our own data plane: a self-call would take a second fairshare
//! admission and write a billable usage row for a gateway-internal operation.

use std::time::Duration;

use anyhow::{anyhow, Result};
use obleth_config::ModelRoute;
use obleth_store::{Store, StoreError};
use serde_json::Value;

/// Where to send embedding requests for one collection's embedder.
#[derive(Debug, Clone)]
pub struct EmbedTarget {
    pub api_base: String,
    pub api_key: Option<String>,
    /// The embedding model's operator-configured upstream headers.
    pub headers: reqwest::header::HeaderMap,
    pub upstream_model: String,
}

/// Where embedding requests for `model` go right now, chosen the way the data
/// plane chooses (healthy endpoints first, endpoint key over model key), so a
/// Slurm-hosted embedder with a blank `api_base` is reachable. With no usable
/// endpoint this is the model's own `api_base`, which `embed_batch` refuses
/// when blank.
pub async fn resolve_target(
    store: &Store,
    model: &ModelRoute,
) -> std::result::Result<EmbedTarget, StoreError> {
    let endpoints = store.resolved_endpoints_for(model.id).await?;
    let route = crate::resolved_model_of(model, endpoints, Vec::new());
    let target =
        crate::build_targets(Some(&route), "", &route.endpoint_selection_mode, "").swap_remove(0);
    Ok(EmbedTarget {
        api_base: target.base,
        api_key: target.api_key,
        headers: target.headers,
        upstream_model: route.upstream_model,
    })
}

/// The upstream refused a batch because it is out of capacity, as TEI does for
/// any batch larger than `--max-concurrent-requests`. A plain 429 (a rate limit
/// or quota) is not this: a smaller batch would be refused just the same.
#[derive(Debug, thiserror::Error)]
#[error("embedding upstream returned {status}: {body}")]
struct UpstreamOverloaded {
    status: reqwest::StatusCode,
    body: String,
}

/// Scale a vector to unit length so cosine similarity is a plain dot product.
/// A zero vector is left alone — dividing by a zero norm yields NaN, which
/// would compare as false against everything and poison scoring silently.
pub fn normalize(v: &mut [f32]) {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in v.iter_mut() {
            *x /= norm;
        }
    }
}

/// Embed `inputs` in one upstream call. Vectors come back normalized and in
/// input order.
pub async fn embed_batch(
    client: &reqwest::Client,
    target: &EmbedTarget,
    inputs: &[String],
    timeout: Duration,
) -> Result<Vec<Vec<f32>>> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let url = join_embeddings_url(&target.api_base)?;
    let mut req = client
        .post(&url)
        .timeout(timeout)
        .headers(target.headers.clone())
        .json(&serde_json::json!({
            "model": target.upstream_model,
            "input": inputs,
        }));
    if let Some(key) = target.api_key.as_deref().filter(|k| !k.is_empty()) {
        req = req.bearer_auth(key);
    }
    let resp = req.send().await?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS
            && body.to_ascii_lowercase().contains("overloaded")
        {
            return Err(UpstreamOverloaded { status, body }.into());
        }
        return Err(anyhow!("embedding upstream returned {status}: {body}"));
    }
    let json: Value = resp.json().await?;
    let mut vectors = parse_embeddings(&json, inputs.len())?;
    for v in vectors.iter_mut() {
        normalize(v);
    }
    Ok(vectors)
}

/// Embed `inputs` in batches of at most `batch_size`, in input order. When the
/// upstream refuses a batch as overloaded the batch size is halved and stays
/// halved for the rest of `inputs`: the limit is the upstream's, usually a fixed
/// one, and no setting here can know it. Overload on a single input is an error.
pub async fn embed_in_batches(
    client: &reqwest::Client,
    target: &EmbedTarget,
    inputs: &[String],
    batch_size: usize,
    timeout: Duration,
) -> Result<Vec<Vec<f32>>> {
    let mut size = batch_size.max(1);
    let mut vectors = Vec::with_capacity(inputs.len());
    let mut rest = inputs;
    while !rest.is_empty() {
        let n = size.min(rest.len());
        match embed_batch(client, target, &rest[..n], timeout).await {
            Ok(batch) => {
                vectors.extend(batch);
                rest = &rest[n..];
            }
            Err(e) if n > 1 && e.is::<UpstreamOverloaded>() => {
                size = n / 2;
                tracing::info!(
                    batch = n,
                    retry = size,
                    "embedding upstream overloaded; splitting batch"
                );
            }
            Err(e) => return Err(e),
        }
    }
    Ok(vectors)
}

/// Build the embeddings URL for a model's configured base.
///
/// Rejects bases this client cannot use rather than emitting a URL that fails
/// with an opaque parse error. A blank base is a real state in this codebase —
/// a Slurm-provisioned model has no static upstream until a replica is
/// promoted — and the message surfaces in the document's `error` column.
fn join_embeddings_url(api_base: &str) -> Result<String> {
    let base = api_base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(anyhow!(
            "model has no upstream configured (no healthy endpoint, empty api_base); a Slurm-provisioned \
             model has none until a replica is promoted"
        ));
    }
    if base.contains('?') || base.contains('#') {
        return Err(anyhow!(
            "api_base must not contain a query string or fragment: {base}"
        ));
    }
    if base.ends_with("/embeddings") {
        Ok(base.to_string())
    } else {
        Ok(format!("{base}/embeddings"))
    }
}

/// Extract vectors, ordered by the documented `index` field rather than array
/// position, and reject anything that would misalign vectors with chunks.
fn parse_embeddings(json: &Value, expected: usize) -> Result<Vec<Vec<f32>>> {
    let data = json
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("embedding response has no `data` array"))?;
    if data.len() != expected {
        return Err(anyhow!(
            "embedding response returned {} vectors for {} inputs",
            data.len(),
            expected
        ));
    }
    let mut out = vec![Vec::new(); expected];
    let mut seen = vec![false; expected];
    for item in data {
        let idx = match item.get("index") {
            Some(v) => v
                .as_u64()
                .ok_or_else(|| anyhow!("embedding entry has a non-integer `index`: {v}"))?
                as usize,
            None => return Err(anyhow!("embedding entry is missing `index`")),
        };
        if idx >= expected {
            return Err(anyhow!("embedding response index {idx} out of range"));
        }
        if seen[idx] {
            return Err(anyhow!("embedding response repeats index {idx}"));
        }
        seen[idx] = true;
        let arr = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("embedding entry has no `embedding` array"))?;
        let mut v = Vec::with_capacity(arr.len());
        for (i, x) in arr.iter().enumerate() {
            // Reject rather than filter: dropping an element yields a shorter
            // vector, and if every vector in the batch drops the same count they
            // all stay equal-length and pass the ragged check while being
            // uniformly wrong.
            let f = x
                .as_f64()
                .ok_or_else(|| anyhow!("embedding element {i} is not a number: {x}"))?;
            if !f.is_finite() {
                return Err(anyhow!("embedding element {i} is not finite: {f}"));
            }
            v.push(f as f32);
        }
        out[idx] = v;
    }
    let dim = out[0].len();
    if dim == 0 || out.iter().any(|v| v.len() != dim) {
        return Err(anyhow!("embedding response has ragged or empty dimensions"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// A local upstream that refuses, with `refusal`, any batch larger than
    /// `limit`, and otherwise embeds "chunk N" as `[N, 1]`. Returns its target
    /// and the size of every batch it was sent.
    async fn fake_upstream(
        limit: usize,
        refusal: &'static str,
    ) -> (EmbedTarget, Arc<Mutex<Vec<usize>>>) {
        let sizes = Arc::new(Mutex::new(Vec::new()));
        let seen = sizes.clone();
        let app = axum::Router::new().route(
            "/v1/embeddings",
            axum::routing::post(move |axum::Json(body): axum::Json<Value>| {
                let seen = seen.clone();
                async move {
                    let inputs = body["input"].as_array().unwrap().clone();
                    seen.lock().unwrap().push(inputs.len());
                    if inputs.len() > limit {
                        return (
                            axum::http::StatusCode::TOO_MANY_REQUESTS,
                            refusal.to_string(),
                        );
                    }
                    let data: Vec<Value> = inputs
                        .iter()
                        .enumerate()
                        .map(|(index, text)| {
                            let n: f32 = text
                                .as_str()
                                .unwrap()
                                .trim_start_matches("chunk ")
                                .parse()
                                .unwrap();
                            serde_json::json!({"index": index, "embedding": [n, 1.0]})
                        })
                        .collect();
                    (
                        axum::http::StatusCode::OK,
                        serde_json::json!({"data": data}).to_string(),
                    )
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let target = EmbedTarget {
            api_base: format!("http://{addr}/v1"),
            api_key: None,
            headers: Default::default(),
            upstream_model: "bge-m3".into(),
        };
        (target, sizes)
    }

    fn numbered(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("chunk {i}")).collect()
    }

    const TEI_OVERLOADED: &str = r#"{"message":"Model is overloaded","type":"overloaded"}"#;

    #[tokio::test]
    async fn splits_batches_the_upstream_refuses_as_overloaded() {
        // TEI with --max-concurrent-requests 16 refuses every batch over 16,
        // however idle it is, so the default batch size of 32 never succeeds.
        let (target, sizes) = fake_upstream(16, TEI_OVERLOADED).await;
        let vectors = embed_in_batches(
            &reqwest::Client::new(),
            &target,
            &numbered(40),
            32,
            Duration::from_secs(5),
        )
        .await
        .expect("embed");

        // The refused batch is retried, and later batches stay at the size
        // that worked instead of being refused once each.
        assert_eq!(*sizes.lock().unwrap(), vec![32, 16, 16, 8]);
        // Vectors are matched back to chunks by position.
        assert_eq!(vectors.len(), 40);
        for (i, v) in vectors.iter().enumerate() {
            assert!((v[0] / v[1] - i as f32).abs() < 1e-3, "vector {i}: {v:?}");
        }
    }

    #[tokio::test]
    async fn a_plain_rate_limit_is_not_split() {
        // Retrying a rate limit in smaller pieces only hammers the upstream.
        let (target, sizes) = fake_upstream(0, "rate limit exceeded").await;
        let err = embed_in_batches(
            &reqwest::Client::new(),
            &target,
            &numbered(8),
            8,
            Duration::from_secs(5),
        )
        .await
        .expect_err("must fail");
        assert!(err.to_string().contains("429"), "{err}");
        assert_eq!(*sizes.lock().unwrap(), vec![8]);
    }

    #[tokio::test]
    async fn overload_on_a_single_input_is_an_error() {
        let (target, sizes) = fake_upstream(0, TEI_OVERLOADED).await;
        let err = embed_in_batches(
            &reqwest::Client::new(),
            &target,
            &numbered(4),
            4,
            Duration::from_secs(5),
        )
        .await
        .expect_err("must fail");
        assert!(err.to_string().contains("overloaded"), "{err}");
        assert_eq!(*sizes.lock().unwrap(), vec![4, 2, 1]);
    }

    #[test]
    fn normalize_makes_unit_length() {
        let mut v = vec![3.0f32, 4.0];
        normalize(&mut v);
        assert!((v[0] - 0.6).abs() < 1e-6);
        assert!((v[1] - 0.8).abs() < 1e-6);
    }

    #[test]
    fn normalize_leaves_zero_vector_alone() {
        // Dividing by a zero norm yields NaN, which would poison every later
        // cosine comparison silently.
        let mut v = vec![0.0f32, 0.0];
        normalize(&mut v);
        assert_eq!(v, vec![0.0, 0.0]);
    }

    #[test]
    fn parses_openai_embedding_response_in_index_order() {
        // The API documents `index`, and returning rows out of order would
        // silently attach each vector to the wrong chunk.
        let body = serde_json::json!({
            "data": [
                {"index": 1, "embedding": [0.0, 1.0]},
                {"index": 0, "embedding": [1.0, 0.0]}
            ]
        });
        let vectors = parse_embeddings(&body, 2).expect("parse");
        assert_eq!(vectors[0], vec![1.0, 0.0]);
        assert_eq!(vectors[1], vec![0.0, 1.0]);
    }

    #[test]
    fn rejects_response_with_wrong_count() {
        let body = serde_json::json!({"data": [{"index": 0, "embedding": [1.0]}]});
        assert!(parse_embeddings(&body, 2).is_err());
    }

    #[test]
    fn rejects_ragged_dimensions() {
        // Mixed dimensions cannot be scored against each other.
        let body = serde_json::json!({
            "data": [
                {"index": 0, "embedding": [1.0, 0.0]},
                {"index": 1, "embedding": [1.0]}
            ]
        });
        assert!(parse_embeddings(&body, 2).is_err());
    }

    #[test]
    fn rejects_non_numeric_embedding_element() {
        let body = serde_json::json!({
            "data": [{"index": 0, "embedding": [1.0, null]}]
        });
        assert!(parse_embeddings(&body, 1).is_err());
    }

    #[test]
    fn rejects_uniformly_truncated_vectors() {
        // The dangerous case: both vectors drop one bad element, so both end up
        // length 1 and would pass a ragged-dimension check while being wrong.
        let body = serde_json::json!({
            "data": [
                {"index": 0, "embedding": [1.0, "bad"]},
                {"index": 1, "embedding": [0.0, "bad"]}
            ]
        });
        assert!(
            parse_embeddings(&body, 2).is_err(),
            "uniform truncation must not pass the ragged check"
        );
    }

    #[test]
    fn rejects_missing_index() {
        let body = serde_json::json!({"data": [{"embedding": [1.0]}]});
        assert!(parse_embeddings(&body, 1).is_err());
    }

    #[test]
    fn rejects_duplicate_index() {
        let body = serde_json::json!({
            "data": [
                {"index": 0, "embedding": [1.0, 0.0]},
                {"index": 0, "embedding": [0.0, 1.0]}
            ]
        });
        assert!(parse_embeddings(&body, 2).is_err());
    }

    #[test]
    fn rejects_blank_api_base() {
        let err = join_embeddings_url("   ").expect_err("blank base must be rejected");
        assert!(err.to_string().contains("no upstream configured"));
    }

    #[test]
    fn rejects_api_base_with_query_string() {
        assert!(join_embeddings_url("http://x/v1?foo=bar").is_err());
    }

    #[test]
    fn joins_url_variants_without_doubling() {
        assert_eq!(
            join_embeddings_url("http://x/v1").unwrap(),
            "http://x/v1/embeddings"
        );
        assert_eq!(
            join_embeddings_url("http://x/v1/").unwrap(),
            "http://x/v1/embeddings"
        );
        assert_eq!(
            join_embeddings_url("http://x/v1/embeddings").unwrap(),
            "http://x/v1/embeddings"
        );
        assert_eq!(
            join_embeddings_url("http://x/v1/embeddings/").unwrap(),
            "http://x/v1/embeddings"
        );
    }
}
