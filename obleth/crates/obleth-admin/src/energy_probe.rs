//! Shared Prometheus instant-query helpers for energy accounting: used by the
//! proxy's power poller and the admin settings test-query route.

/// Extract the single scalar from a Prometheus instant-query vector response.
pub fn parse_prometheus_scalar(body: &serde_json::Value) -> Result<f64, String> {
    let value = body
        .pointer("/data/result/0/value/1")
        .ok_or_else(|| "query returned no data".to_string())?;
    value
        .as_str()
        .ok_or_else(|| "sample value is not a string".to_string())?
        .parse::<f64>()
        .map_err(|e| format!("sample value did not parse as f64: {e}"))
}

/// One instant query against `{base}/api/v1/query`.
pub async fn instant_query(http: &reqwest::Client, base: &str, expr: &str) -> Result<f64, String> {
    let url = format!("{}/api/v1/query", base.trim_end_matches('/'));
    let resp = http
        .get(&url)
        .query(&[("query", expr)])
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| format!("prometheus request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("prometheus returned {}", resp.status()));
    }
    let body: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| format!("prometheus response was not JSON: {e}"))?;
    parse_prometheus_scalar(&body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_prometheus_instant_vector() {
        let body: serde_json::Value = serde_json::json!({
            "status": "success",
            "data": { "resultType": "vector",
                "result": [ { "metric": {}, "value": [1719849600.0, "409000"] } ] }
        });
        assert_eq!(parse_prometheus_scalar(&body).unwrap(), 409000.0);
        let empty: serde_json::Value = serde_json::json!({
            "status": "success", "data": { "resultType": "vector", "result": [] }
        });
        assert!(parse_prometheus_scalar(&empty).is_err());
    }
}
