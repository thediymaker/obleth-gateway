use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::seedplan::{plan_model, ModelAction};

pub struct AdminClient {
    base: String,
    token: String,
    http: reqwest::Client,
}

/// Identifiers for everything obench created during a run, so they can be torn
/// down afterwards. Only resources obench *created* are tracked — pre-existing
/// tenants/models it merely updated are never deleted.
#[derive(Clone, Debug, Default)]
pub struct Teardown {
    pub key_ids: Vec<String>,
    pub model_ids: Vec<String>,
    pub tenant_ids: Vec<String>,
    /// The gateway's global in-flight ceiling before the run changed it, so
    /// teardown puts it back. A run that never touched the ceiling leaves this
    /// unset.
    pub restore_capacity: Option<u32>,
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
    /// Size of this model's own admission pool. `None` leaves the gateway's
    /// `default_model_max_in_flight` in force.
    pub max_in_flight: Option<u32>,
}

/// One tenant's row in a `/fairshare/live` response.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct TenantLive {
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub fairshare_group: String,
    #[serde(default)]
    pub weight: i64,
    #[serde(default)]
    pub max_in_flight: Option<u64>,
    #[serde(default)]
    pub in_flight: u64,
    #[serde(default)]
    pub queued: u64,
    #[serde(default)]
    pub served_tokens: f64,
    #[serde(default)]
    pub weight_share: f64,
}

/// One fairshare group's row in a `/fairshare/live` response.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct GroupLive {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub weight: i64,
    #[serde(default)]
    pub in_flight: u64,
    #[serde(default)]
    pub queued: u64,
    #[serde(default)]
    pub slot_cap: u64,
    #[serde(default)]
    pub borrowed: u64,
    #[serde(default)]
    pub served_tokens: f64,
    #[serde(default)]
    pub weight_share: f64,
}

/// One API key's row in a `/fairshare/live` response, inside a model pool.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct KeyLive {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub tenant_id: String,
    #[serde(default)]
    pub weight: i64,
    #[serde(default)]
    pub max_in_flight: Option<u64>,
    #[serde(default)]
    pub in_flight: u64,
    #[serde(default)]
    pub queued: u64,
    #[serde(default)]
    pub served_tokens: f64,
}

/// One model's admission pool in a `/fairshare/live` response.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct PoolLive {
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub cap: u64,
    #[serde(default)]
    pub in_flight: u64,
    #[serde(default)]
    pub queued: u64,
    #[serde(default)]
    pub tenants: Vec<TenantLive>,
    #[serde(default)]
    pub keys: Vec<KeyLive>,
}

/// Every field defaults so a gateway on a different version degrades to partial
/// data rather than failing the whole poll.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct FairshareLive {
    #[serde(default)]
    pub algorithm: String,
    #[serde(default)]
    pub max_in_flight: u64,
    #[serde(default)]
    pub global_in_flight: u64,
    #[serde(default)]
    pub global_queued: u64,
    #[serde(default)]
    pub groups: Vec<GroupLive>,
    #[serde(default)]
    pub tenants: Vec<TenantLive>,
    #[serde(default)]
    pub pools: Vec<PoolLive>,
}

impl FairshareLive {
    /// Project each model pool into a per-pool sample, resolving key rows'
    /// `tenant_id` back to the tenant name the accumulators key on.
    pub fn pool_samples(&self) -> Vec<crate::engine::fairshare::PoolSample> {
        self.pools
            .iter()
            .map(|p| {
                let names: std::collections::HashMap<&str, &str> = p
                    .tenants
                    .iter()
                    .map(|t| (t.tenant_id.as_str(), t.name.as_str()))
                    .collect();
                crate::engine::fairshare::PoolSample {
                    model: p.model.clone(),
                    cap: p.cap,
                    in_flight: p.in_flight,
                    queued: p.queued,
                    tenants: p
                        .tenants
                        .iter()
                        .map(|t| crate::engine::fairshare::TenantSample {
                            name: t.name.clone(),
                            group: t.fairshare_group.clone(),
                            weight: t.weight,
                            max_in_flight: t.max_in_flight,
                            in_flight: t.in_flight,
                            queued: t.queued,
                            served_tokens: t.served_tokens,
                            weight_share: t.weight_share,
                        })
                        .collect(),
                    keys: p
                        .keys
                        .iter()
                        .map(|k| crate::engine::fairshare::KeySample {
                            tenant: names
                                .get(k.tenant_id.as_str())
                                .map(|s| s.to_string())
                                .unwrap_or_else(|| k.tenant_id.clone()),
                            name: k.name.clone(),
                            weight: k.weight,
                            max_in_flight: k.max_in_flight,
                            in_flight: k.in_flight,
                            queued: k.queued,
                            served_tokens: k.served_tokens,
                        })
                        .collect(),
                }
            })
            .collect()
    }

    /// Project the tenant rows into accumulator samples.
    pub fn tenant_samples(&self) -> Vec<crate::engine::fairshare::TenantSample> {
        self.tenants
            .iter()
            .map(|t| crate::engine::fairshare::TenantSample {
                name: t.name.clone(),
                group: t.fairshare_group.clone(),
                weight: t.weight,
                max_in_flight: t.max_in_flight,
                in_flight: t.in_flight,
                queued: t.queued,
                served_tokens: t.served_tokens,
                weight_share: t.weight_share,
            })
            .collect()
    }

    /// Project the group rows into accumulator samples.
    pub fn group_samples(&self) -> Vec<crate::engine::fairshare::GroupSample> {
        self.groups
            .iter()
            .map(|g| crate::engine::fairshare::GroupSample {
                name: g.name.clone(),
                weight: g.weight,
                in_flight: g.in_flight,
                queued: g.queued,
                slot_cap: g.slot_cap,
                borrowed: g.borrowed,
                served_tokens: g.served_tokens,
                weight_share: g.weight_share,
            })
            .collect()
    }
}

impl AdminClient {
    pub fn new(base: String, token: String) -> Self {
        Self {
            base,
            token,
            http: reqwest::Client::new(),
        }
    }

    async fn req(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value> {
        let mut rb = self
            .http
            .request(method.clone(), format!("{}/api/v1{path}", self.base))
            .bearer_auth(&self.token);
        if let Some(b) = body {
            rb = rb.json(&b);
        }
        let res = rb
            .send()
            .await
            .with_context(|| format!("{method} {path}"))?;
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

    pub async fn ensure_model(&self, spec: &ModelSpec) -> Result<(String, bool)> {
        let models = self.req(reqwest::Method::GET, "/models", None).await?;
        let existing_id = models
            .as_array()
            .and_then(|arr| {
                arr.iter()
                    .find(|m| m["model_name"] == spec.model_name.as_str())
            })
            .and_then(|m| m["id"].as_str().map(|s| s.to_string()));

        match plan_model(existing_id.as_deref()) {
            ModelAction::Create => {
                let created = self
                    .req(
                        reqwest::Method::POST,
                        "/models",
                        Some(json!({
                            "model_name": spec.model_name,
                            "upstream_model": spec.upstream_model,
                            "api_base": spec.api_base,
                            "context_window": spec.context_window,
                            "admission_weight": spec.admission_weight,
                            "max_in_flight": spec.max_in_flight,
                        })),
                    )
                    .await?;
                // Some deployments return the created object; fall back to a
                // follow-up lookup if the POST response omits the id.
                let id = match created["id"].as_str() {
                    Some(id) => id.to_string(),
                    None => {
                        let models = self.req(reqwest::Method::GET, "/models", None).await?;
                        models
                            .as_array()
                            .and_then(|arr| {
                                arr.iter()
                                    .find(|m| m["model_name"] == spec.model_name.as_str())
                            })
                            .and_then(|m| m["id"].as_str().map(|s| s.to_string()))
                            .context("created model id")?
                    }
                };
                // Live models carry the upstream api_key; push it in via update.
                if spec.api_key.is_some() {
                    self.req(
                        reqwest::Method::PUT,
                        &format!("/models/{id}"),
                        Some(json!({
                            "upstream_model": spec.upstream_model,
                            "api_base": spec.api_base,
                            "api_key": spec.api_key,
                            "input_cost_per_token": spec.input_cost_per_token,
                            "output_cost_per_token": spec.output_cost_per_token,
                            "context_window": spec.context_window,
                            "admission_weight": spec.admission_weight,
                            "max_in_flight": spec.max_in_flight,
                            "enabled": true,
                        })),
                    )
                    .await?;
                }
                Ok((id, true))
            }
            ModelAction::Update(id) => {
                self.req(
                    reqwest::Method::PUT,
                    &format!("/models/{id}"),
                    Some(json!({
                        "upstream_model": spec.upstream_model,
                        "api_base": spec.api_base,
                        "api_key": spec.api_key,
                        "input_cost_per_token": spec.input_cost_per_token,
                        "output_cost_per_token": spec.output_cost_per_token,
                        "context_window": spec.context_window,
                        "admission_weight": spec.admission_weight,
                        "max_in_flight": spec.max_in_flight,
                        "supports_function_calling": false,
                        "supports_system_messages": true,
                        "supports_response_schema": false,
                        "supports_tool_choice": false,
                        "enabled": true,
                    })),
                )
                .await?;
                Ok((id, false))
            }
        }
    }

    pub async fn ensure_group(&self, name: &str, weight: u32) -> Result<()> {
        let groups = self
            .req(reqwest::Method::GET, "/fairshare/groups", None)
            .await?;
        let exists = groups
            .as_array()
            .is_some_and(|a| a.iter().any(|g| g["name"] == name));
        if exists {
            self.req(
                reqwest::Method::PATCH,
                &format!("/fairshare/groups/{name}/weight"),
                Some(json!({ "weight": weight })),
            )
            .await?;
        } else {
            self.req(
                reqwest::Method::POST,
                "/fairshare/groups",
                Some(json!({ "name": name, "weight": weight })),
            )
            .await?;
        }
        Ok(())
    }

    pub async fn ensure_tenant(
        &self,
        name: &str,
        weight: u32,
        tokens_per_minute: u64,
        max_in_flight: Option<u32>,
        group: &str,
        synthetic: bool,
    ) -> Result<(String, bool)> {
        let tenants = self.req(reqwest::Method::GET, "/tenants", None).await?;
        let existing = tenants
            .as_array()
            .and_then(|a| a.iter().find(|t| t["name"] == name))
            .cloned();
        if let Some(t) = existing {
            let id = t["id"].as_str().context("tenant id")?.to_string();
            self.req(
                reqwest::Method::PATCH,
                &format!("/tenants/{id}/weight"),
                Some(json!({ "weight": weight })),
            )
            .await?;
            self.req(
                reqwest::Method::PUT,
                &format!("/tenants/{id}/quota"),
                Some(
                    json!({ "tokens_per_minute": tokens_per_minute, "max_in_flight": max_in_flight }),
                ),
            )
            .await?;
            self.req(
                reqwest::Method::PATCH,
                &format!("/tenants/{id}/group"),
                Some(json!({ "fairshare_group": group })),
            )
            .await?;
            if synthetic {
                // Older gateways lack this route; tagging degrades gracefully.
                let _ = self
                    .req(
                        reqwest::Method::PUT,
                        &format!("/tenants/{id}/synthetic"),
                        Some(json!({ "synthetic": true })),
                    )
                    .await;
            }
            Ok((id, false))
        } else {
            let created = self.req(reqwest::Method::POST, "/tenants", Some(json!({
                "name": name, "weight": weight, "tokens_per_minute": tokens_per_minute, "fairshare_group": group,
                "max_in_flight": max_in_flight, "synthetic": synthetic,
            }))).await?;
            Ok((
                created["id"].as_str().context("new tenant id")?.to_string(),
                true,
            ))
        }
    }

    /// Mint a fresh API key for a tenant, returning `(key_id, secret)`. Any
    /// pre-existing obench keys with the same name are pruned first so the
    /// secret is always retrievable and no stale test keys accumulate. obench
    /// never reuses (and never persists) a key — the secret lives only in memory
    /// for the duration of the run and the key is deleted during teardown.
    pub async fn ensure_key(
        &self,
        tenant_id: &str,
        key_name: &str,
        weight: u32,
        max_in_flight: Option<u32>,
    ) -> Result<(String, String)> {
        let keys = self.req(reqwest::Method::GET, "/keys", None).await?;
        let inv: Vec<(String, String, String)> = keys
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|k| {
                        Some((
                            k["id"].as_str()?.to_string(),
                            k["tenant_id"].as_str()?.to_string(),
                            k["name"].as_str()?.to_string(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();

        // Prune every same-named key for this tenant, then always mint fresh.
        for (id, tid, name) in &inv {
            if tid == tenant_id && name == key_name {
                let _ = self
                    .req(reqwest::Method::DELETE, &format!("/keys/{id}"), None)
                    .await;
            }
        }
        let minted = self
            .req(
                reqwest::Method::POST,
                &format!("/tenants/{tenant_id}/keys"),
                Some(json!({ "name": key_name, "weight": weight, "max_in_flight": max_in_flight })),
            )
            .await?;
        // Response shape: { "key": { "id": ..., ... }, "secret": "..." }.
        let id = minted["key"]["id"]
            .as_str()
            .context("minted key id")?
            .to_string();
        let secret = minted["secret"]
            .as_str()
            .context("minted secret")?
            .to_string();
        Ok((id, secret))
    }

    /// Best-effort teardown of everything obench created this run. Deletes keys
    /// first (the secret material), then live models (which embed the upstream
    /// api_key), then the synthetic tenants. Never errors: cleanup runs even on
    /// a failed/interrupted run, and a missing resource is fine.
    pub async fn teardown(&self, td: &Teardown) {
        for id in &td.key_ids {
            let _ = self
                .req(reqwest::Method::DELETE, &format!("/keys/{id}"), None)
                .await;
        }
        for id in &td.model_ids {
            let _ = self
                .req(reqwest::Method::DELETE, &format!("/models/{id}"), None)
                .await;
        }
        for id in &td.tenant_ids {
            let _ = self
                .req(reqwest::Method::DELETE, &format!("/tenants/{id}"), None)
                .await;
        }
        if let Some(cap) = td.restore_capacity {
            if let Err(e) = self.set_capacity(cap).await {
                eprintln!(
                    "warning: failed to restore the gateway in-flight ceiling to {cap} after the run: {e} — set it back via PUT /api/v1/capacity"
                );
            }
        }
    }

    /// Raise the gateway's global in-flight ceiling for a run and remember
    /// the previous value in `td` so `teardown` restores it.
    pub async fn raise_capacity_for_run(
        &self,
        td: &mut Teardown,
        max_in_flight: u32,
    ) -> Result<u32> {
        if td.restore_capacity.is_none() {
            td.restore_capacity = Some(self.get_capacity().await?);
        }
        self.set_capacity(max_in_flight).await
    }

    pub async fn set_capacity(&self, max_in_flight: u32) -> Result<u32> {
        let body = self
            .req(
                reqwest::Method::PUT,
                "/capacity",
                Some(json!({ "max_in_flight": max_in_flight })),
            )
            .await?;
        Ok(body["max_in_flight"]
            .as_u64()
            .unwrap_or(max_in_flight as u64) as u32)
    }

    pub async fn fairshare_live(&self) -> Result<FairshareLive> {
        let v = self
            .req(reqwest::Method::GET, "/fairshare/live", None)
            .await?;
        Ok(serde_json::from_value(v).unwrap_or_default())
    }

    pub async fn list_model_names(&self) -> anyhow::Result<Vec<String>> {
        let v = self.req(reqwest::Method::GET, "/models", None).await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|m| m["model_name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn list_tenant_names(&self) -> anyhow::Result<Vec<String>> {
        let v = self.req(reqwest::Method::GET, "/tenants", None).await?;
        Ok(v.as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|t| t["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub async fn get_capacity(&self) -> anyhow::Result<u32> {
        let v = self.req(reqwest::Method::GET, "/capacity", None).await?;
        Ok(v["max_in_flight"].as_u64().unwrap_or(0) as u32)
    }

    /// Fetch the current model-boons settings view (flattened per boon). Used to
    /// snapshot compression settings before a benchmark mutates them.
    pub async fn get_boons(&self) -> anyhow::Result<Value> {
        self.req(reqwest::Method::GET, "/settings/boons", None)
            .await
    }

    /// Update model-boons settings. The server merges partial bodies, so `patch`
    /// need only carry the fields being changed.
    pub async fn set_boons(&self, patch: Value) -> anyhow::Result<()> {
        self.req(reqwest::Method::PUT, "/settings/boons", Some(patch))
            .await?;
        Ok(())
    }

    pub async fn find_model_id(&self, model_name: &str) -> Result<Option<String>> {
        let v = self.req(reqwest::Method::GET, "/models", None).await?;
        Ok(v.as_array().and_then(|a| {
            a.iter()
                .find(|m| m["model_name"].as_str() == Some(model_name))
                .and_then(|m| m["id"].as_str().map(String::from))
        }))
    }

    pub async fn model_health(&self) -> Result<Value> {
        self.req(reqwest::Method::GET, "/models/health", None).await
    }

    pub async fn set_model_health_config(
        &self,
        id: &str,
        checks_enabled: bool,
        alerts_enabled: bool,
        interval_secs: i64,
        failure_threshold: i64,
    ) -> Result<()> {
        self.req(
            reqwest::Method::PUT,
            &format!("/models/{id}/health/config"),
            Some(json!({
                "checks_enabled": checks_enabled,
                "alerts_enabled": alerts_enabled,
                "check_interval_secs": interval_secs,
                "failure_threshold": failure_threshold,
                "maintenance_until": Value::Null,
                "maintenance_note": Value::Null,
            })),
        )
        .await?;
        Ok(())
    }

    pub async fn get_version(&self) -> Result<String> {
        let v = self.req(reqwest::Method::GET, "/version", None).await?;
        Ok(v["version"].as_str().unwrap_or("unknown").to_string())
    }
}

/// Query an OpenAI-compatible upstream for its model catalog.
///
/// Used by the live wizard: the user supplies a base URL + key, and obench
/// lists `GET {base}/models` so they can pick which models to benchmark instead
/// of hand-writing a config file. Accepts either the standard
/// `{ "data": [ { "id": ... } ] }` envelope or a bare array.
pub async fn fetch_upstream_models(base: &str, key: &str) -> Result<Vec<String>> {
    let base = base.trim_end_matches('/');
    let url = format!("{base}/models");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let res = client
        .get(&url)
        .bearer_auth(key)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?;
    let status = res.status();
    let text = res.text().await.unwrap_or_default();
    if !status.is_success() {
        anyhow::bail!("GET {url} -> {status}: {text}");
    }
    let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
    let mut out = Vec::new();
    let items = v["data"].as_array().or_else(|| v.as_array());
    if let Some(arr) = items {
        for m in arr {
            if let Some(id) = m["id"].as_str() {
                out.push(id.to_string());
            } else if let Some(s) = m.as_str() {
                out.push(s.to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    if out.is_empty() {
        anyhow::bail!("{url} returned no models — check the base URL and key");
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped exactly like `get_fairshare_live` in obleth-admin.
    const LIVE_PAYLOAD: &str = r#"{
        "algorithm": "hierarchical",
        "max_in_flight": 8,
        "global_in_flight": 8,
        "global_queued": 12,
        "groups": [
            {"name":"prod","weight":500,"in_flight":7,"queued":4,"slot_cap":7,
             "served_tokens":91000.0,"share_score":182.0,"weight_share":0.909,
             "expected_slots":7.27},
            {"name":"dev","weight":50,"in_flight":1,"queued":8,"slot_cap":1,
             "served_tokens":9000.0,"share_score":180.0,"weight_share":0.0909,
             "expected_slots":0.73}
        ],
        "tenants": [
            {"tenant_id":"11111111-1111-1111-1111-111111111111","name":"chatbot",
             "fairshare_group":"prod","weight":500,"in_flight":7,"queued":4,
             "served_tokens":91000.0,"share_score":182.0,"weight_share":0.909,
             "expected_slots":7.27},
            {"tenant_id":"22222222-2222-2222-2222-222222222222","name":"api-batch",
             "fairshare_group":"dev","weight":50,"in_flight":1,"queued":8,
             "served_tokens":9000.0,"share_score":180.0,"weight_share":0.0909,
             "expected_slots":0.73}
        ],
        "model_in_flight": {"obench-base": 8},
        "model_queued": {"obench-base": 12}
    }"#;

    #[test]
    fn live_payload_keeps_per_tenant_rows() {
        let live: FairshareLive = serde_json::from_str(LIVE_PAYLOAD).unwrap();
        assert_eq!(live.algorithm, "hierarchical");
        assert_eq!(live.max_in_flight, 8);
        assert_eq!(live.tenants.len(), 2);
        assert_eq!(live.groups.len(), 2);
    }

    #[test]
    fn tenant_samples_project_every_field_the_accumulator_needs() {
        let live: FairshareLive = serde_json::from_str(LIVE_PAYLOAD).unwrap();
        let samples = live.tenant_samples();
        assert_eq!(samples.len(), 2);
        let chatbot = samples.iter().find(|s| s.name == "chatbot").unwrap();
        assert_eq!(chatbot.group, "prod");
        assert_eq!(chatbot.weight, 500);
        assert_eq!(chatbot.in_flight, 7);
        assert_eq!(chatbot.queued, 4);
        assert!((chatbot.served_tokens - 91000.0).abs() < 1e-9);
        assert!((chatbot.weight_share - 0.909).abs() < 1e-9);
    }

    #[test]
    fn group_samples_carry_the_slot_cap_tenants_cannot_supply() {
        let live: FairshareLive = serde_json::from_str(LIVE_PAYLOAD).unwrap();
        let groups = live.group_samples();
        assert_eq!(groups.len(), 2);
        let prod = groups.iter().find(|g| g.name == "prod").unwrap();
        assert_eq!(prod.slot_cap, 7);
        assert_eq!(prod.weight, 500);
        assert_eq!(prod.in_flight, 7);
        let dev = groups.iter().find(|g| g.name == "dev").unwrap();
        assert_eq!(dev.slot_cap, 1);
        assert_eq!(dev.queued, 8);
    }

    #[test]
    fn unknown_and_missing_fields_degrade_to_defaults() {
        // A gateway that predates the per-tenant rows, plus a field obench
        // does not know about: neither may fail the poll.
        let live: FairshareLive =
            serde_json::from_str(r#"{"global_in_flight":3,"future_field":true}"#).unwrap();
        assert_eq!(live.global_in_flight, 3);
        assert_eq!(live.max_in_flight, 0);
        assert!(live.tenants.is_empty());
        assert!(live.tenant_samples().is_empty());
    }
}
