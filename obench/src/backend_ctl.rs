//! Client for benchmark-backend's control surface: fault injection
//! (`POST /control`), ground-truth counters (`GET /stats`), reachability.

use anyhow::{Context, Result};

pub struct BackendControl {
    base: String,
    http: reqwest::Client,
}

impl BackendControl {
    pub fn new(base: String) -> Self {
        Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    pub async fn set_fault(&self, model: &str, mode: &str) -> Result<()> {
        let url = format!("{}/control", self.base);
        let res = self
            .http
            .post(&url)
            .json(&serde_json::json!({ "model": model, "mode": mode }))
            .send()
            .await
            .with_context(|| format!("POST {url}"))?;
        anyhow::ensure!(res.status().is_success(), "POST {url} -> {}", res.status());
        Ok(())
    }

    #[allow(dead_code)] // consumed in task 10
    pub async fn stats(&self) -> Result<serde_json::Value> {
        let url = format!("{}/stats", self.base);
        Ok(self.http.get(&url).send().await?.json().await?)
    }

    pub async fn healthy(&self) -> bool {
        let url = format!("{}/health", self.base);
        matches!(self.http.get(&url).send().await, Ok(r) if r.status().is_success())
    }
}
