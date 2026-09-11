//! Calling a registered embedding model's upstream directly.
//!
//! Indexing and query embedding both go straight to the upstream rather than
//! back through our own data plane: a self-call would take a second fairshare
//! admission and write a billable usage row for a gateway-internal operation.

use std::time::Duration;

use anyhow::{anyhow, Result};
use serde_json::Value;

/// Where to send embedding requests for one collection's embedder.
#[derive(Debug, Clone)]
pub struct EmbedTarget {
    pub api_base: String,
    pub api_key: Option<String>,
    pub upstream_model: String,
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
    let url = join_embeddings_url(&target.api_base);
    let mut req = client.post(&url).timeout(timeout).json(&serde_json::json!({
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
        return Err(anyhow!("embedding upstream returned {status}: {body}"));
    }
    let json: Value = resp.json().await?;
    let mut vectors = parse_embeddings(&json, inputs.len())?;
    for v in vectors.iter_mut() {
        normalize(v);
    }
    Ok(vectors)
}

/// Mirrors the proxy's upstream-path handling: a base that already ends in
/// `/embeddings` is used as-is rather than having the suffix appended twice.
fn join_embeddings_url(api_base: &str) -> String {
    let base = api_base.trim_end_matches('/');
    if base.ends_with("/embeddings") {
        base.to_string()
    } else {
        format!("{base}/embeddings")
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
    for item in data {
        let idx = item.get("index").and_then(Value::as_u64).unwrap_or(0) as usize;
        if idx >= expected {
            return Err(anyhow!("embedding response index {idx} out of range"));
        }
        let v: Vec<f32> = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("embedding entry has no `embedding` array"))?
            .iter()
            .filter_map(|x| x.as_f64().map(|f| f as f32))
            .collect();
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
}
