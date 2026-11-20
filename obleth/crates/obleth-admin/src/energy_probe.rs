//! Shared Prometheus instant-query helpers for energy accounting: used by the
//! proxy's power poller and the admin settings test-query route.

use std::collections::BTreeMap;

/// Extract the single scalar from a Prometheus instant-query vector response.
pub fn parse_prometheus_scalar(body: &serde_json::Value) -> Result<f64, String> {
    let value = body
        .pointer("/data/result/0/value/1")
        .ok_or_else(|| "query returned no data".to_string())?;
    let sample = value
        .as_str()
        .ok_or_else(|| "sample value is not a string".to_string())?;
    let parsed = sample
        .parse::<f64>()
        .map_err(|e| format!("sample value did not parse as f64: {e}"))?;
    // Prometheus emits NaN/±Inf for empty or divergent series; a stored NaN
    // reading would poison every request's frozen energy figures.
    if !parsed.is_finite() {
        return Err(format!("sample value {sample} is not a finite number"));
    }
    Ok(parsed)
}

/// One instant query against `{base}/api/v1/query`, read as a scalar.
pub async fn instant_query(http: &reqwest::Client, base: &str, expr: &str) -> Result<f64, String> {
    parse_prometheus_scalar(&query_json(http, base, expr).await?)
}

/// Watts per value of `label` from a Prometheus instant-query vector
/// response. Series without the label or with a non-finite sample are
/// skipped; series sharing a label value are summed, so an unaggregated
/// query still reads as one figure per model.
pub fn parse_prometheus_vector(
    body: &serde_json::Value,
    label: &str,
) -> Result<BTreeMap<String, f64>, String> {
    let result = body
        .pointer("/data/result")
        .and_then(|r| r.as_array())
        .ok_or_else(|| "response has no result vector".to_string())?;
    let mut watts = BTreeMap::new();
    for series in result {
        let Some(name) = series
            .pointer("/metric")
            .and_then(|m| m.get(label))
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
        else {
            continue;
        };
        let Some(sample) = series
            .pointer("/value/1")
            .and_then(|v| v.as_str())
            .and_then(|v| v.parse::<f64>().ok())
            .filter(|v| v.is_finite() && *v >= 0.0)
        else {
            continue;
        };
        *watts.entry(name.to_string()).or_insert(0.0) += sample;
    }
    if watts.is_empty() {
        return Err(if result.is_empty() {
            "query returned no data".to_string()
        } else {
            format!("no series carries a `{label}` label with a finite value")
        });
    }
    Ok(watts)
}

/// The power series a model is charged from: the first of its name, its
/// capacity Service and its upstream model that names a series.
pub fn match_power_series<'a, V>(
    series: &'a BTreeMap<String, V>,
    model_name: &str,
    capacity_service: Option<&str>,
    upstream_model: &str,
) -> Option<(&'a String, &'a V)> {
    [Some(model_name), capacity_service, Some(upstream_model)]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .find_map(|n| series.get_key_value(n))
}

async fn query_json(
    http: &reqwest::Client,
    base: &str,
    expr: &str,
) -> Result<serde_json::Value, String> {
    let url = format!("{}/api/v1/query", base.trim_end_matches('/'));
    let resp = http
        .get(&url)
        .query(&[("query", expr)])
        .timeout(crate::ssrf::ADMIN_OUTBOUND_TIMEOUT)
        .send()
        .await
        .map_err(|e| format!("prometheus request failed: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("prometheus returned {}", resp.status()));
    }
    resp.json()
        .await
        .map_err(|e| format!("prometheus response was not JSON: {e}"))
}

/// One instant query, read as watts per value of `label`.
pub async fn instant_vector(
    http: &reqwest::Client,
    base: &str,
    expr: &str,
    label: &str,
) -> Result<BTreeMap<String, f64>, String> {
    parse_prometheus_vector(&query_json(http, base, expr).await?, label)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vector(series: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "status": "success",
            "data": { "resultType": "vector", "result": series }
        })
    }

    #[test]
    fn parses_watts_per_label_value() {
        let body = vector(serde_json::json!([
            { "metric": { "model": "a" }, "value": [0.0, "1200.5"] },
            { "metric": { "model": "b" }, "value": [0.0, "300"] },
            { "metric": { "model": "a" }, "value": [0.0, "100"] },
            { "metric": { "other": "c" }, "value": [0.0, "50"] },
            { "metric": { "model": "d" }, "value": [0.0, "NaN"] },
            { "metric": { "model": "e" }, "value": [0.0, "-5"] }
        ]));
        let w = parse_prometheus_vector(&body, "model").unwrap();
        assert_eq!(
            w.len(),
            2,
            "unlabelled, NaN and negative series are skipped"
        );
        assert_eq!(w["a"], 1300.5, "series sharing a value are summed");
        assert_eq!(w["b"], 300.0);
    }

    #[test]
    fn vector_without_usable_series_is_an_error() {
        assert!(parse_prometheus_vector(&vector(serde_json::json!([])), "model").is_err());
        let unlabelled = vector(serde_json::json!([
            { "metric": { "node": "n1" }, "value": [0.0, "100"] }
        ]));
        let err = parse_prometheus_vector(&unlabelled, "model").unwrap_err();
        assert!(err.contains("`model`"), "{err}");
    }

    #[test]
    fn matches_name_then_service_then_upstream() {
        let series: BTreeMap<String, f64> = [("svc".to_string(), 1.0), ("up".to_string(), 2.0)]
            .into_iter()
            .collect();
        let hit =
            |name, svc, up| match_power_series(&series, name, svc, up).map(|(k, _)| k.clone());
        assert_eq!(
            hit("up", Some("svc"), "x"),
            Some("up".into()),
            "own name first"
        );
        assert_eq!(
            hit("m", Some("svc"), "up"),
            Some("svc".into()),
            "then the Service"
        );
        assert_eq!(
            hit("m", None, "up"),
            Some("up".into()),
            "then the upstream model"
        );
        assert_eq!(hit("m", Some(" "), "zzz"), None);
    }

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

    #[test]
    fn rejects_non_finite_samples() {
        for sample in ["NaN", "+Inf", "-Inf"] {
            let body = serde_json::json!({
                "status": "success",
                "data": { "resultType": "vector",
                    "result": [ { "metric": {}, "value": [1719849600.0, sample] } ] }
            });
            assert!(parse_prometheus_scalar(&body).is_err(), "{sample}");
        }
    }
}
