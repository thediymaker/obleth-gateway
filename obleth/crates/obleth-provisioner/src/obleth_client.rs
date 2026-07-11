use crate::config::ProvisionerConfig;
use crate::domain::ReplicaView;
use async_trait::async_trait;
use obleth_config::{ManagedModelSpec, SlurmSettings};
use uuid::Uuid;

#[async_trait]
pub trait OblethClient: Send + Sync {
    async fn list_managed_models(&self) -> anyhow::Result<Vec<ManagedModelSpec>>;
    /// System-wide Slurm connection settings (incl. the decrypted JWT) from the
    /// Management API's provisioner-facing `resolved` route. `None` only when
    /// the gateway has never had Slurm configured.
    async fn get_slurm_settings(&self) -> anyhow::Result<Option<SlurmSettings>>;
    async fn model_name(&self, model_id: Uuid) -> anyhow::Result<String>;
    /// Every replica row across all models — used to reconcile orphans and drain
    /// models that have left the managed set.
    async fn list_all_replicas(&self) -> anyhow::Result<Vec<ReplicaView>>;
    async fn create_replica(
        &self,
        model_id: Uuid,
        slurm_job_id: &str,
        port_base: i64,
    ) -> anyhow::Result<Uuid>;
    async fn patch_replica(
        &self,
        replica_id: Uuid,
        state: Option<&str>,
        nodes: Option<&str>,
        endpoint_id: Option<Uuid>,
        message: Option<&str>,
    ) -> anyhow::Result<()>;
    async fn delete_replica(&self, replica_id: Uuid) -> anyhow::Result<()>;
    /// Register a live replica as a model endpoint; returns the endpoint id.
    async fn create_endpoint(
        &self,
        model_id: Uuid,
        name: &str,
        api_base: &str,
    ) -> anyhow::Result<Uuid>;
    /// List this model's endpoints, including the gateway's health verdict for
    /// each (see `EndpointView` — the real-inference signal self-heal folds in).
    async fn list_endpoints(
        &self,
        model_id: Uuid,
    ) -> anyhow::Result<Vec<crate::domain::EndpointView>>;
    async fn delete_endpoint(&self, model_id: Uuid, endpoint_id: Uuid) -> anyhow::Result<()>;
    /// Rewrite an endpoint's `api_base` (used to migrate a name-based endpoint to
    /// its resolved IP), preserving the endpoint's other fields.
    async fn update_endpoint_api_base(
        &self,
        model_id: Uuid,
        ep: &crate::domain::EndpointView,
        api_base: &str,
    ) -> anyhow::Result<()>;
    /// Record (or clear with `None`) the provisioner's last submit error for a
    /// model, so the dashboard can show it.
    async fn set_provision_error(&self, model_id: Uuid, error: Option<&str>) -> anyhow::Result<()>;
}

/// Map a JSON array of replica rows (`ModelReplica`) into `ReplicaView`s. Rows
/// missing/with an invalid `id` are skipped (logged), since they can't be acted
/// on. Shared by the per-model and all-models list calls.
fn replica_views_from_json(rows: &serde_json::Value) -> Vec<ReplicaView> {
    let now = chrono::Utc::now();
    let mut out = Vec::new();
    for r in rows.as_array().map(|a| a.as_slice()).unwrap_or_default() {
        let id: Uuid = match r
            .get("id")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
        {
            Some(id) => id,
            None => {
                tracing::warn!("replica row missing/invalid id; skipping");
                continue;
            }
        };
        let model_id: Uuid = match r
            .get("model_id")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
        {
            Some(m) => m,
            None => {
                tracing::warn!(replica_id = %id, "replica row missing/invalid model_id; skipping");
                continue;
            }
        };
        let created: chrono::DateTime<chrono::Utc> = r
            .get("created_at")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .unwrap_or(now);
        out.push(ReplicaView {
            id,
            model_id,
            slurm_job_id: r
                .get("slurm_job_id")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
            state: r
                .get("state")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string(),
            endpoint_id: r
                .get("endpoint_id")
                .and_then(|x| x.as_str())
                .and_then(|s| s.parse().ok()),
            age_secs: (now - created).num_seconds(),
            port_base: r.get("port_base").and_then(|x| x.as_i64()).unwrap_or(0),
            last_message: r
                .get("last_message")
                .and_then(|x| x.as_str())
                .map(str::to_string),
            cancel_requested: r
                .get("cancel_requested")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
        });
    }
    out
}

/// Outcome of the previous reconcile tick, reported to the gateway as headers
/// on the once-per-tick settings fetch (the same request that serves as the
/// liveness heartbeat). This is what lets the dashboard distinguish "process
/// alive and reconciling" from "process alive but every tick failing/holding" —
/// without it, a week of `slurm job lookup failed; holding tick` looks
/// identical to a healthy provisioner.
#[derive(Debug, Clone)]
pub struct TickReport {
    /// `ok` | `idle` | `error`.
    pub status: &'static str,
    /// Idle reason or error text; empty for `ok`.
    pub detail: String,
}

impl TickReport {
    pub fn ok() -> Self {
        Self {
            status: "ok",
            detail: String::new(),
        }
    }
    pub fn idle(reason: &str) -> Self {
        Self {
            status: "idle",
            detail: reason.to_string(),
        }
    }
    pub fn error(detail: &str) -> Self {
        Self {
            status: "error",
            detail: detail.to_string(),
        }
    }
}

/// Longest tick detail we put on the wire. Error chains can be long; the
/// dashboard only needs the head of the message.
const TICK_DETAIL_MAX: usize = 240;

/// Make an arbitrary string safe as an HTTP header value: collapse all
/// whitespace/control characters to single spaces, drop non-ASCII, and bound
/// the length. reqwest would otherwise reject the request outright on an
/// invalid header value — and a failed settings fetch reads as "provisioner
/// down", which is the exact confusion this reporting exists to remove.
fn header_safe(value: &str, max: usize) -> String {
    let cleaned: String = value
        .chars()
        .map(|c| if c.is_ascii_whitespace() { ' ' } else { c })
        .filter(|c| c.is_ascii() && !c.is_control())
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(max).collect()
}

pub struct HttpObleth {
    http: reqwest::Client,
    base: String,
    token: String,
    /// Previous tick's outcome, attached to the next settings fetch.
    last_tick: std::sync::Mutex<Option<TickReport>>,
}

impl HttpObleth {
    pub fn new(cfg: &ProvisionerConfig, http: reqwest::Client) -> Self {
        Self {
            http,
            base: format!("{}/api/v1", cfg.admin_base_url.trim_end_matches('/')),
            token: cfg.admin_token.clone(),
            last_tick: std::sync::Mutex::new(None),
        }
    }

    /// Record the outcome of the tick that just finished; the next
    /// `get_slurm_settings` call carries it to the gateway.
    pub fn set_last_tick(&self, report: TickReport) {
        if let Ok(mut t) = self.last_tick.lock() {
            *t = Some(report);
        }
    }
    fn req(&self, m: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        // Carry the provisioner's build identity on every call. The gateway reads
        // it off the once-per-tick `/settings/slurm/resolved` poll (its heartbeat)
        // so the dashboard can show which provisioner build is actually running —
        // independently of the gateway/control-plane images.
        let mut req = self
            .http
            .request(m, format!("{}{path}", self.base))
            .bearer_auth(&self.token)
            .header("X-Obleth-Audit-Actor", "system")
            .header("X-Obleth-Provisioner-Version", env!("CARGO_PKG_VERSION"));
        if let Some(sha) = option_env!("OBLETH_BUILD_SHA").filter(|s| !s.is_empty()) {
            req = req.header("X-Obleth-Provisioner-Sha", sha);
        }
        if let Some(built) = option_env!("OBLETH_BUILD_TIMESTAMP").filter(|s| !s.is_empty()) {
            req = req.header("X-Obleth-Provisioner-Built-At", built);
        }
        req
    }
}

#[async_trait]
impl OblethClient for HttpObleth {
    async fn list_managed_models(&self) -> anyhow::Result<Vec<ManagedModelSpec>> {
        // /managed is per-model; the provisioner needs them all. List models,
        // then GET each /managed. (A bulk /managed route exists and is a fine
        // future optimization; v1 keeps the surface minimal.)
        let models: serde_json::Value = self
            .req(reqwest::Method::GET, "/models")
            .send()
            .await?
            .json()
            .await?;
        let mut out = Vec::new();
        for m in models.as_array().cloned().unwrap_or_default() {
            let id = m.get("id").and_then(|x| x.as_str()).unwrap_or_default();
            if id.is_empty() {
                continue;
            }
            let spec: Option<ManagedModelSpec> = self
                .req(reqwest::Method::GET, &format!("/models/{id}/managed"))
                .send()
                .await?
                .json()
                .await?;
            if let Some(s) = spec {
                if s.enabled {
                    out.push(s);
                }
            }
        }
        Ok(out)
    }

    async fn get_slurm_settings(&self) -> anyhow::Result<Option<SlurmSettings>> {
        // Report the previous tick's outcome alongside this fetch (the same
        // request the gateway treats as the heartbeat), so the dashboard can
        // show "reconcile failing since X" rather than a green process light.
        let mut req = self.req(reqwest::Method::GET, "/settings/slurm/resolved");
        let tick = self.last_tick.lock().ok().and_then(|t| t.clone());
        if let Some(t) = tick {
            req = req.header("X-Obleth-Provisioner-Tick-Status", t.status);
            let detail = header_safe(&t.detail, TICK_DETAIL_MAX);
            if !detail.is_empty() {
                req = req.header("X-Obleth-Provisioner-Tick-Detail", detail);
            }
        }
        let s: SlurmSettings = req.send().await?.error_for_status()?.json().await?;
        Ok(Some(s))
    }

    async fn model_name(&self, model_id: Uuid) -> anyhow::Result<String> {
        let m: serde_json::Value = self
            .req(reqwest::Method::GET, &format!("/models/{model_id}"))
            .send()
            .await?
            .json()
            .await?;
        Ok(m.get("model_name")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string())
    }

    async fn list_all_replicas(&self) -> anyhow::Result<Vec<ReplicaView>> {
        let rows: serde_json::Value = self
            .req(reqwest::Method::GET, "/replicas")
            .send()
            .await?
            .json()
            .await?;
        Ok(replica_views_from_json(&rows))
    }

    async fn create_replica(
        &self,
        model_id: Uuid,
        slurm_job_id: &str,
        port_base: i64,
    ) -> anyhow::Result<Uuid> {
        let v: serde_json::Value = self
            .req(
                reqwest::Method::POST,
                &format!("/models/{model_id}/replicas"),
            )
            .json(&serde_json::json!({ "slurm_job_id": slurm_job_id, "port_base": port_base }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        // A successful POST must return a parseable id. Returning a nil UUID on a
        // missing/unparseable id (the old `unwrap_or_default`) silently strands
        // the caller with an invalid reference, so surface it as an error instead.
        v.get("id")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("create_replica: response missing a valid id: {v}"))
    }

    async fn patch_replica(
        &self,
        replica_id: Uuid,
        state: Option<&str>,
        nodes: Option<&str>,
        endpoint_id: Option<Uuid>,
        message: Option<&str>,
    ) -> anyhow::Result<()> {
        let mut body = serde_json::Map::new();
        if let Some(s) = state {
            body.insert("state".into(), serde_json::json!(s));
        }
        if let Some(n) = nodes {
            body.insert("nodes".into(), serde_json::json!(n));
        }
        if let Some(e) = endpoint_id {
            body.insert("endpoint_id".into(), serde_json::json!(e.to_string()));
        }
        if let Some(m) = message {
            body.insert("message".into(), serde_json::json!(m));
        }
        self.req(reqwest::Method::PATCH, &format!("/replicas/{replica_id}"))
            .json(&serde_json::Value::Object(body))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn delete_replica(&self, replica_id: Uuid) -> anyhow::Result<()> {
        self.req(reqwest::Method::DELETE, &format!("/replicas/{replica_id}"))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn create_endpoint(
        &self,
        model_id: Uuid,
        name: &str,
        api_base: &str,
    ) -> anyhow::Result<Uuid> {
        let v: serde_json::Value = self
            .req(
                reqwest::Method::POST,
                &format!("/models/{model_id}/endpoints"),
            )
            .json(&serde_json::json!({ "name": name, "api_base": api_base, "enabled": true }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        // Must return a parseable id; a nil fallback would be written onto the
        // replica as endpoint_id and fail the subsequent patch with an FK error,
        // stranding the replica as a phantom "healthy" with no endpoint.
        v.get("id")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("create_endpoint: response missing a valid id: {v}"))
    }

    async fn list_endpoints(
        &self,
        model_id: Uuid,
    ) -> anyhow::Result<Vec<crate::domain::EndpointView>> {
        let rows: serde_json::Value = self
            .req(
                reqwest::Method::GET,
                &format!("/models/{model_id}/endpoints"),
            )
            .send()
            .await?
            .json()
            .await?;
        let now = chrono::Utc::now();
        let mut out = Vec::new();
        for e in rows.as_array().cloned().unwrap_or_default() {
            let id = e
                .get("id")
                .and_then(|x| x.as_str())
                .and_then(|s| s.parse().ok());
            let name = e
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            if let Some(id) = id {
                out.push(crate::domain::EndpointView {
                    id,
                    name,
                    api_base: e
                        .get("api_base")
                        .and_then(|x| x.as_str())
                        .unwrap_or_default()
                        .to_string(),
                    priority: e.get("priority").and_then(|x| x.as_i64()).unwrap_or(100),
                    weight: e.get("weight").and_then(|x| x.as_i64()).unwrap_or(100),
                    enabled: e.get("enabled").and_then(|x| x.as_bool()).unwrap_or(true),
                    health_status: e
                        .get("health_status")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    consecutive_failures: e
                        .get("consecutive_failures")
                        .and_then(|x| x.as_i64())
                        .unwrap_or(0),
                    checked_secs_ago: e
                        .get("last_checked_at")
                        .and_then(|x| x.as_str())
                        .and_then(|s| s.parse::<chrono::DateTime<chrono::Utc>>().ok())
                        .map(|t| (now - t).num_seconds()),
                });
            }
        }
        Ok(out)
    }

    async fn delete_endpoint(&self, model_id: Uuid, endpoint_id: Uuid) -> anyhow::Result<()> {
        self.req(
            reqwest::Method::DELETE,
            &format!("/models/{model_id}/endpoints/{endpoint_id}"),
        )
        .send()
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn update_endpoint_api_base(
        &self,
        model_id: Uuid,
        ep: &crate::domain::EndpointView,
        api_base: &str,
    ) -> anyhow::Result<()> {
        // Full-object PUT: re-send the endpoint's own name/priority/weight/enabled
        // so only api_base changes. api_key is omitted, which the admin handler
        // reads as "keep the stored secret".
        self.req(
            reqwest::Method::PUT,
            &format!("/models/{model_id}/endpoints/{}", ep.id),
        )
        .json(&serde_json::json!({
            "name": ep.name,
            "api_base": api_base,
            "priority": ep.priority,
            "weight": ep.weight,
            "enabled": ep.enabled,
        }))
        .send()
        .await?
        .error_for_status()?;
        Ok(())
    }

    async fn set_provision_error(&self, model_id: Uuid, error: Option<&str>) -> anyhow::Result<()> {
        self.req(
            reqwest::Method::PATCH,
            &format!("/models/{model_id}/managed/provision-error"),
        )
        .json(&serde_json::json!({ "error": error }))
        .send()
        .await?
        .error_for_status()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_safe_collapses_whitespace_and_bounds_length() {
        // Multi-line error chains must flatten to one bounded ASCII line.
        let raw = "slurm job lookup failed; holding tick:\n\n  error sending request for url (http://slurm:6820/…)\ttimed out";
        let safe = header_safe(raw, 60);
        assert!(safe.is_ascii());
        assert!(!safe.contains('\n') && !safe.contains('\t'));
        assert!(safe.len() <= 60, "got {} chars: {safe}", safe.len());
        assert!(safe.starts_with("slurm job lookup failed; holding tick:"));
    }

    #[test]
    fn header_safe_drops_non_ascii_and_controls() {
        assert_eq!(header_safe("ok\u{7}\u{200b}→ done", 100), "ok done");
        assert_eq!(header_safe("", 100), "");
    }
}

/// In-memory fake for executor/loop tests — no network. Records every mutating
/// call so tests can assert on them, and serves canned `managed`/`replicas`.
#[cfg(test)]
#[derive(Default)]
pub struct MockObleth {
    pub managed: std::sync::Mutex<Vec<ManagedModelSpec>>,
    pub slurm: std::sync::Mutex<Option<SlurmSettings>>,
    pub replicas: std::sync::Mutex<Vec<ReplicaView>>,
    pub created_replicas: std::sync::Mutex<Vec<(Uuid, String, i64)>>, // (model_id, slurm_job_id, port_base)
    pub created_endpoints: std::sync::Mutex<Vec<(Uuid, String, String)>>, // (model_id, name, api_base)
    pub deleted_endpoints: std::sync::Mutex<Vec<Uuid>>,
    pub updated_endpoints: std::sync::Mutex<Vec<(Uuid, String)>>, // (endpoint_id, new api_base)
    pub deleted_replicas: std::sync::Mutex<Vec<Uuid>>,
    pub patched: std::sync::Mutex<Vec<(Uuid, Option<String>)>>, // (replica_id, state)
    pub provision_errors: std::sync::Mutex<Vec<(Uuid, Option<String>)>>,
    /// When set, `create_replica` returns an error — drives the compensating
    /// "cancel the orphan job" path in the Submit executor.
    pub fail_create_replica: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
#[async_trait]
impl OblethClient for MockObleth {
    async fn list_managed_models(&self) -> anyhow::Result<Vec<ManagedModelSpec>> {
        Ok(self.managed.lock().unwrap().clone())
    }
    async fn get_slurm_settings(&self) -> anyhow::Result<Option<SlurmSettings>> {
        Ok(self.slurm.lock().unwrap().clone())
    }
    async fn model_name(&self, _model_id: Uuid) -> anyhow::Result<String> {
        Ok("test-model".to_string())
    }
    async fn list_all_replicas(&self) -> anyhow::Result<Vec<ReplicaView>> {
        Ok(self.replicas.lock().unwrap().clone())
    }
    async fn create_replica(
        &self,
        model_id: Uuid,
        slurm_job_id: &str,
        port_base: i64,
    ) -> anyhow::Result<Uuid> {
        if self
            .fail_create_replica
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            anyhow::bail!("simulated create_replica failure");
        }
        self.created_replicas
            .lock()
            .unwrap()
            .push((model_id, slurm_job_id.to_string(), port_base));
        Ok(Uuid::new_v4())
    }
    async fn patch_replica(
        &self,
        replica_id: Uuid,
        state: Option<&str>,
        _nodes: Option<&str>,
        _endpoint_id: Option<Uuid>,
        _message: Option<&str>,
    ) -> anyhow::Result<()> {
        self.patched
            .lock()
            .unwrap()
            .push((replica_id, state.map(|s| s.to_string())));
        Ok(())
    }
    async fn delete_replica(&self, replica_id: Uuid) -> anyhow::Result<()> {
        self.deleted_replicas.lock().unwrap().push(replica_id);
        Ok(())
    }
    async fn create_endpoint(
        &self,
        model_id: Uuid,
        name: &str,
        api_base: &str,
    ) -> anyhow::Result<Uuid> {
        self.created_endpoints.lock().unwrap().push((
            model_id,
            name.to_string(),
            api_base.to_string(),
        ));
        Ok(Uuid::new_v4())
    }
    async fn list_endpoints(
        &self,
        _model_id: Uuid,
    ) -> anyhow::Result<Vec<crate::domain::EndpointView>> {
        // MockObleth doesn't track the returned endpoint ids by name, so report
        // none exist — each test promote creates exactly one endpoint.
        Ok(Vec::new())
    }
    async fn delete_endpoint(&self, _model_id: Uuid, endpoint_id: Uuid) -> anyhow::Result<()> {
        self.deleted_endpoints.lock().unwrap().push(endpoint_id);
        Ok(())
    }
    async fn update_endpoint_api_base(
        &self,
        _model_id: Uuid,
        ep: &crate::domain::EndpointView,
        api_base: &str,
    ) -> anyhow::Result<()> {
        self.updated_endpoints
            .lock()
            .unwrap()
            .push((ep.id, api_base.to_string()));
        Ok(())
    }
    async fn set_provision_error(&self, model_id: Uuid, error: Option<&str>) -> anyhow::Result<()> {
        self.provision_errors
            .lock()
            .unwrap()
            .push((model_id, error.map(str::to_string)));
        Ok(())
    }
}
