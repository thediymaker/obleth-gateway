use std::time::Duration;

/// True iff GET {api_base}{health_path} returns 2xx within the timeout.
/// Universal: no assumption beyond an HTTP health endpoint the operator named.
pub async fn is_healthy(
    http: &reqwest::Client,
    api_base: &str,
    health_path: &str,
    timeout_secs: u64,
) -> bool {
    let url = format!("{}{}", api_base.trim_end_matches('/'), health_path);
    match http.get(&url).timeout(Duration::from_secs(timeout_secs)).send().await {
        Ok(r) => r.status().is_success(),
        Err(_) => false,
    }
}
