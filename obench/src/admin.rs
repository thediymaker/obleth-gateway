use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::seedplan::{plan_key, plan_model, ModelAction};

pub struct AdminClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

#[derive(Clone, Debug)]
pub struct ModelSpec {
    pub model_name: String,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: Option<String>,
    pub input_cost_per_token: f64,
    pub output_cost_per_token: f64,
    pub context_window: u32,
    pub admission_weight: u32,
}

#[derive(Debug, Deserialize)]
pub struct FairshareLive {
    #[serde(default)]
    pub global_in_flight: u64,
    #[serde(default)]
    pub global_queued: u64,
}

impl AdminClient {
    pub fn new(base: String, token: String) -> Self {
        Self { base, token, http: reqwest::Client::new() }
    }

    async fn req(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut rb = self
            .http
            .request(method.clone(), format!("{}/api/v1{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let res = rb.send().await.with_context(|| format!("{method} {path}"))?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            anyhow::bail!("{method} {path} -> {status}: {text}");
        }
        if text.is_empty() {
            return Ok(Value::Null);
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    pub async fn ensure_model(&self, spec: &ModelSpec) -> Result<()> {
        let models = self.req(reqwest::Method::GET, "/models", None).await?;
        let existing_id = models
            .as_array()
            .and_then(|arr| arr.iter().find(|m| m["model_name"] == spec.model_name.as_str()))
            .and_then(|m| m["id"].as_str().map(|s| s.to_string()));

        match plan_model(existing_id.as_deref()) {
            ModelAction::Create => {
                self.req(reqwest::Method::POST, "/models", Some(json!({
                    "model_name": spec.model_name,
                    "upstream_model": spec.upstream_model,
                    "api_base": spec.api_base,
                    "context_window": spec.context_window,
                    "admission_weight": spec.admission_weight,
                }))).await?;
            }
            ModelAction::Update(id) => {
                self.req(reqwest::Method::PUT, &format!("/models/{id}"), Some(json!({
                    "upstream_model": spec.upstream_model,
                    "api_base": spec.api_base,
                    "api_key": spec.api_key,
                    "input_cost_per_token": spec.input_cost_per_token,
                    "output_cost_per_token": spec.output_cost_per_token,
                    "context_window": spec.context_window,
                    "admission_weight": spec.admission_weight,
                    "supports_function_calling": false,
                    "supports_system_messages": true,
                    "supports_response_schema": false,
                    "supports_tool_choice": false,
                    "enabled": true,
                }))).await?;
            }
        }
        Ok(())
    }

    pub async fn ensure_group(&self, name: &str, weight: u32) -> Result<()> {
        let groups = self.req(reqwest::Method::GET, "/fairshare/groups", None).await?;
        let exists = groups.as_array().is_some_and(|a| a.iter().any(|g| g["name"] == name));
        if exists {
            self.req(reqwest::Method::PATCH, &format!("/fairshare/groups/{name}/weight"),
                     Some(json!({ "weight": weight }))).await?;
        } else {
            self.req(reqwest::Method::POST, "/fairshare/groups",
                     Some(json!({ "name": name, "weight": weight }))).await?;
        }
        Ok(())
    }

    pub async fn ensure_tenant(&self, name: &str, weight: u32, tokens_per_minute: u64, group: &str) -> Result<String> {
        let tenants = self.req(reqwest::Method::GET, "/tenants", None).await?;
        let existing = tenants.as_array()
            .and_then(|a| a.iter().find(|t| t["name"] == name))
            .cloned();
        if let Some(t) = existing {
            let id = t["id"].as_str().context("tenant id")?.to_string();
            self.req(reqwest::Method::PATCH, &format!("/tenants/{id}/weight"), Some(json!({ "weight": weight }))).await?;
            self.req(reqwest::Method::PUT, &format!("/tenants/{id}/quota"),
                     Some(json!({ "tokens_per_minute": tokens_per_minute, "max_in_flight": Value::Null }))).await?;
            self.req(reqwest::Method::PATCH, &format!("/tenants/{id}/group"),
                     Some(json!({ "fairshare_group": group }))).await?;
            Ok(id)
        } else {
            let created = self.req(reqwest::Method::POST, "/tenants", Some(json!({
                "name": name, "weight": weight, "tokens_per_minute": tokens_per_minute, "fairshare_group": group,
            }))).await?;
            Ok(created["id"].as_str().context("new tenant id")?.to_string())
        }
    }

    pub async fn ensure_key(&self, tenant_id: &str, key_name: &str) -> Result<Option<String>> {
        let keys = self.req(reqwest::Method::GET, "/keys", None).await?;
        let inv: Vec<(String, String, String)> = keys.as_array().map(|a| {
            a.iter().filter_map(|k| Some((
                k["id"].as_str()?.to_string(),
                k["tenant_id"].as_str()?.to_string(),
                k["name"].as_str()?.to_string(),
            ))).collect()
        }).unwrap_or_default();

        let plan = plan_key(&inv, tenant_id, key_name);
        for id in &plan.prune {
            let _ = self.req(reqwest::Method::DELETE, &format!("/keys/{id}"), None).await;
        }
        if plan.mint {
            let minted = self.req(reqwest::Method::POST, &format!("/tenants/{tenant_id}/keys"),
                                  Some(json!({ "name": key_name }))).await?;
            return Ok(Some(minted["secret"].as_str().context("minted secret")?.to_string()));
        }
        Ok(None) // reused — secret not retrievable from the API
    }

    pub async fn set_capacity(&self, max_in_flight: u32) -> Result<u32> {
        let body = self.req(reqwest::Method::PUT, "/capacity", Some(json!({ "max_in_flight": max_in_flight }))).await?;
        Ok(body["max_in_flight"].as_u64().unwrap_or(max_in_flight as u64) as u32)
    }

    pub async fn fairshare_live(&self) -> Result<FairshareLive> {
        let v = self.req(reqwest::Method::GET, "/fairshare/live", None).await?;
        Ok(serde_json::from_value(v).unwrap_or(FairshareLive { global_in_flight: 0, global_queued: 0 }))
    }
}
