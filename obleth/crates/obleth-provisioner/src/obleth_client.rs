use crate::config::ProvisionerConfig;
use crate::domain::ReplicaView;
use async_trait::async_trait;
use obleth_config::{ManagedBackoff, ManagedModelSpec, SlurmSettings};
use std::collections::HashMap;
use uuid::Uuid;

/// Provisioner-reported details of a replica (`PATCH /replicas/{id}`).
/// `None` leaves a field as it is; an empty `state_detail` or `log_path`
/// clears it.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
pub struct ReplicaDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_started_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_ends_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub log_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace_requested: Option<bool>,
}

/// A gateway model's name and the name its server is expected to serve.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelIdentity {
    pub name: String,
    /// `upstream_model`; empty when unknown.
    pub upstream: String,
}

impl ModelIdentity {
    /// The names a server for this model may list on `/v1/models`. Empty
    /// when the upstream name is unknown, so no identity check is made.
    pub fn served_names(&self) -> Vec<String> {
        if self.upstream.trim().is_empty() {
            Vec::new()
        } else {
            vec![self.upstream.clone(), self.name.clone()]
        }
    }

    /// The name used in messages about what the server should serve.
    pub fn expected(&self) -> &str {
        if self.upstream.trim().is_empty() {
            &self.name
        } else {
            &self.upstream
        }
    }
}

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
    /// Every model's name and upstream name by id, in one read.
    async fn model_identities(&self) -> anyhow::Result<HashMap<Uuid, ModelIdentity>> {
        Ok(HashMap::new())
    }
    /// One model's name and upstream name.
    async fn model_identity(&self, model_id: Uuid) -> anyhow::Result<ModelIdentity> {
        Ok(ModelIdentity {
            name: self.model_name(model_id).await?,
            upstream: String::new(),
        })
    }
    /// Record a new replica and where its job's output goes.
    async fn create_replica_with_log(
        &self,
        model_id: Uuid,
        slurm_job_id: &str,
        port_base: i64,
        _log_path: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        self.create_replica(model_id, slurm_job_id, port_base).await
    }
    /// Update a replica's Slurm times, plain-sentence detail, log path, port
    /// or replacement flag.
    async fn patch_replica_details(
        &self,
        _replica_id: Uuid,
        _details: &ReplicaDetails,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    /// Record a managed model's failure count and pause (backoff) state.
    async fn set_managed_backoff(
        &self,
        _model_id: Uuid,
        _backoff: &ManagedBackoff,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    /// A state change and the replica's details in one update.
    async fn patch_replica_with_details(
        &self,
        replica_id: Uuid,
        state: Option<&str>,
        nodes: Option<&str>,
        endpoint_id: Option<Uuid>,
        message: Option<&str>,
        details: &ReplicaDetails,
    ) -> anyhow::Result<()> {
        self.patch_replica(replica_id, state, nodes, endpoint_id, message)
            .await?;
        self.patch_replica_details(replica_id, details).await
    }
}

/// Map a JSON array of replica rows (`ModelReplica`) into `ReplicaView`s. Rows
/// missing/with an invalid `id` are skipped (logged), since they can't be acted
/// on.
fn replica_views_from_json(rows: &[serde_json::Value]) -> Vec<ReplicaView> {
    let now = chrono::Utc::now();
    let mut out = Vec::new();
    for r in rows {
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
        let time = |k: &str| -> Option<chrono::DateTime<chrono::Utc>> {
            r.get(k)
                .and_then(|x| x.as_str())
                .and_then(|s| s.parse().ok())
        };
        let text = |k: &str| -> Option<String> {
            r.get(k)
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
        };
        let created = time("created_at").unwrap_or(now);
        let updated = time("updated_at").unwrap_or(created);
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
            replace_requested: r
                .get("replace_requested")
                .and_then(|x| x.as_bool())
                .unwrap_or(false),
            updated_secs: (now - updated).num_seconds(),
            job_started_at: time("job_started_at"),
            job_ends_at: time("job_ends_at"),
            state_detail: text("state_detail"),
            port: r.get("port").and_then(|x| x.as_i64()),
            nodes: text("nodes"),
            log_path: text("log_path"),
        });
    }
    out
}

/// A model row's name and upstream name.
fn identity_from_json(m: &serde_json::Value) -> Option<ModelIdentity> {
    Some(ModelIdentity {
        name: m.get("model_name")?.as_str()?.to_string(),
        upstream: m
            .get("upstream_model")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string(),
    })
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
    /// Things an operator should know even when the tick worked, as plain
    /// sentences ("2 cancels failed: ...", "The Slurm token expires in 2 days.").
    pub warnings: Vec<String>,
}

impl TickReport {
    pub fn ok() -> Self {
        Self {
            status: "ok",
            detail: String::new(),
            warnings: Vec::new(),
        }
    }
    pub fn idle(reason: &str) -> Self {
        Self {
            status: "idle",
            detail: reason.to_string(),
            warnings: Vec::new(),
        }
    }
    pub fn error(detail: &str) -> Self {
        Self {
            status: "error",
            detail: detail.to_string(),
            warnings: Vec::new(),
        }
    }
    /// The same report carrying `warnings`.
    pub fn with_warnings(mut self, warnings: Vec<String>) -> Self {
        self.warnings = warnings;
        self
    }
}

/// Most warnings carried per tick, and the longest each may be.
const TICK_WARNINGS_MAX: usize = 8;
const TICK_WARNING_MAX_LEN: usize = 200;

/// The tick warnings as a JSON array that is safe as a header value.
fn warnings_header(warnings: &[String]) -> Option<String> {
    let list: Vec<String> = warnings
        .iter()
        .map(|w| header_safe(w, TICK_WARNING_MAX_LEN))
        .filter(|w| !w.is_empty())
        .take(TICK_WARNINGS_MAX)
        .collect();
    if list.is_empty() {
        return None;
    }
    serde_json::to_string(&list).ok()
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

    /// GET a list route strictly: a non-2xx status or a body that is not a JSON
    /// array is an `Err`. The admin error envelope (`{"error":"..."}`) is valid
    /// JSON, so reading it leniently turned a gateway 500 into an empty list —
    /// and the tick plans destructive actions (drain, resubmit) from these lists.
    async fn get_array(&self, path: &str) -> anyhow::Result<Vec<serde_json::Value>> {
        let v: serde_json::Value = self
            .req(reqwest::Method::GET, path)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        match v {
            serde_json::Value::Array(rows) => Ok(rows),
            other => {
                let body = other.to_string();
                anyhow::bail!(
                    "GET {path}: expected array, got {}",
                    String::from_utf8_lossy(&body.as_bytes()[..body.len().min(200)])
                )
            }
        }
    }
}

#[async_trait]
impl OblethClient for HttpObleth {
    async fn list_managed_models(&self) -> anyhow::Result<Vec<ManagedModelSpec>> {
        // /managed is per-model; the provisioner needs them all. List models,
        // then GET each /managed. (A bulk /managed route exists and is a fine
        // future optimization; v1 keeps the surface minimal.)
        // One read of every spec. An error status fails the whole list (it
        // must not read as "nothing is managed", which would drain replicas).
        let rows = self.get_array("/managed").await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let spec: ManagedModelSpec = serde_json::from_value(row)
                .map_err(|e| anyhow::anyhow!("GET /managed: unreadable spec: {e}"))?;
            if spec.enabled {
                out.push(spec);
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
            if let Some(w) = warnings_header(&t.warnings) {
                req = req.header("X-Obleth-Provisioner-Tick-Warnings", w);
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
            .error_for_status()?
            .json()
            .await?;
        Ok(m.get("model_name")
            .and_then(|x| x.as_str())
            .unwrap_or_default()
            .to_string())
    }

    async fn list_all_replicas(&self) -> anyhow::Result<Vec<ReplicaView>> {
        let rows = self.get_array("/replicas").await?;
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
        // `force` drops the row outright. Without it the gateway treats a
        // DELETE of a live replica as "cancel it", which is the operator's
        // action, not the provisioner's clean-up of a finished row.
        self.req(
            reqwest::Method::DELETE,
            &format!("/replicas/{replica_id}?force=true"),
        )
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
        let rows = self
            .get_array(&format!("/models/{model_id}/endpoints"))
            .await?;
        let now = chrono::Utc::now();
        let mut out = Vec::new();
        for e in rows {
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
        let resp = self
            .req(
                reqwest::Method::DELETE,
                &format!("/models/{model_id}/endpoints/{endpoint_id}"),
            )
            .send()
            .await?;
        // Already gone is what a delete wants.
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        resp.error_for_status()?;
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

    async fn model_identities(&self) -> anyhow::Result<HashMap<Uuid, ModelIdentity>> {
        let rows = self.get_array("/models").await?;
        Ok(rows
            .iter()
            .filter_map(|m| Some((m.get("id")?.as_str()?.parse().ok()?, identity_from_json(m)?)))
            .collect())
    }

    async fn model_identity(&self, model_id: Uuid) -> anyhow::Result<ModelIdentity> {
        let m: serde_json::Value = self
            .req(reqwest::Method::GET, &format!("/models/{model_id}"))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(identity_from_json(&m).unwrap_or_default())
    }

    async fn patch_replica_with_details(
        &self,
        replica_id: Uuid,
        state: Option<&str>,
        nodes: Option<&str>,
        endpoint_id: Option<Uuid>,
        message: Option<&str>,
        details: &ReplicaDetails,
    ) -> anyhow::Result<()> {
        let mut body = match serde_json::to_value(details)? {
            serde_json::Value::Object(m) => m,
            _ => serde_json::Map::new(),
        };
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
        if body.is_empty() {
            return Ok(());
        }
        self.req(reqwest::Method::PATCH, &format!("/replicas/{replica_id}"))
            .json(&serde_json::Value::Object(body))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn create_replica_with_log(
        &self,
        model_id: Uuid,
        slurm_job_id: &str,
        port_base: i64,
        log_path: Option<&str>,
    ) -> anyhow::Result<Uuid> {
        let v: serde_json::Value = self
            .req(
                reqwest::Method::POST,
                &format!("/models/{model_id}/replicas"),
            )
            .json(&serde_json::json!({
                "slurm_job_id": slurm_job_id,
                "port_base": port_base,
                "log_path": log_path,
            }))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        v.get("id")
            .and_then(|x| x.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| anyhow::anyhow!("create_replica: response missing a valid id: {v}"))
    }

    async fn patch_replica_details(
        &self,
        replica_id: Uuid,
        details: &ReplicaDetails,
    ) -> anyhow::Result<()> {
        if *details == ReplicaDetails::default() {
            return Ok(());
        }
        self.req(reqwest::Method::PATCH, &format!("/replicas/{replica_id}"))
            .json(details)
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn set_managed_backoff(
        &self,
        model_id: Uuid,
        backoff: &ManagedBackoff,
    ) -> anyhow::Result<()> {
        self.req(
            reqwest::Method::PUT,
            &format!("/models/{model_id}/managed/backoff"),
        )
        .json(backoff)
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

    /// A throwaway HTTP/1.1 server that answers every request with the same
    /// status and JSON body (`Connection: close`, same shape as warmup.rs's).
    async fn admin_stub(status: u16, body: &'static str) -> HttpObleth {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = sock.read(&mut buf).await;
                    let resp = format!(
                        "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = sock.write_all(resp.as_bytes()).await;
                    let _ = sock.flush().await;
                });
            }
        });
        let cfg = ProvisionerConfig {
            admin_base_url: format!("http://{addr}"),
            admin_token: "t".into(),
            interval_secs: 15,
            health_timeout_secs: 5,
            warmup_timeout_secs: 0,
            lost_retention_secs: 900,
            restart_after_failures: 0,
            port_span: 8,
            job_name_prefix: "obleth-".into(),
        };
        HttpObleth::new(&cfg, reqwest::Client::new())
    }

    // The three reads the tick plans from must never turn an admin-API failure
    // into an empty list: an empty managed set drains the whole fleet, and an
    // empty replica list double-submits every model.

    #[tokio::test]
    async fn list_managed_models_errors_on_500_and_error_envelope() {
        let c = admin_stub(500, r#"{"error":"db down"}"#).await;
        assert!(c.list_managed_models().await.is_err(), "500 must be Err");
        let c = admin_stub(500, "[]").await;
        assert!(
            c.list_managed_models().await.is_err(),
            "500 with an array body must be Err"
        );
        let c = admin_stub(200, r#"{"error":"x"}"#).await;
        assert!(
            c.list_managed_models().await.is_err(),
            "non-array must be Err"
        );
        let c = admin_stub(200, "[]").await;
        assert!(c.list_managed_models().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn managed_specs_come_from_one_list_and_disabled_ones_are_skipped() {
        let c = admin_stub(
            200,
            r#"[{"model_id":"00000000-0000-0000-0000-000000000001","enabled":true,"partition":"","gres":"","nodes":1,"image":"","preamble":"","log_output_dir":"","launch_command":"x","serving_port":8000,"health_path":"/health","target_replicas":1,"max_job_failures":0,"paused_until":"2030-01-01T00:00:00Z","paused_reason":"Paused.","created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"},
                {"model_id":"00000000-0000-0000-0000-000000000002","enabled":false,"partition":"","gres":"","nodes":1,"image":"","preamble":"","log_output_dir":"","launch_command":"x","serving_port":8000,"health_path":"/health","target_replicas":1,"max_job_failures":0,"created_at":"2026-01-01T00:00:00Z","updated_at":"2026-01-01T00:00:00Z"}]"#,
        )
        .await;
        let specs = c.list_managed_models().await.unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].paused_reason.as_deref(), Some("Paused."));
        assert!(specs[0].paused_until.is_some());
        // Fields an older gateway does not send take their defaults.
        assert_eq!(specs[0].startup_timeout_mins, 60);
        assert_eq!(specs[0].handoff_mins, 15);
    }

    #[tokio::test]
    async fn model_names_are_read_in_one_list() {
        let c = admin_stub(
            200,
            r#"[{"id":"00000000-0000-0000-0000-000000000001","model_name":"a","upstream_model":"org/a"},{"id":"bad","model_name":"b"}]"#,
        )
        .await;
        let names = c.model_identities().await.unwrap();
        assert_eq!(names.len(), 1);
        let a = names.values().next().unwrap();
        assert_eq!((a.name.as_str(), a.upstream.as_str()), ("a", "org/a"));
        assert_eq!(a.served_names(), vec!["org/a".to_string(), "a".to_string()]);
        let c = admin_stub(500, "[]").await;
        assert!(c.model_identities().await.is_err());
        let c = admin_stub(404, r#"{"error":"not found"}"#).await;
        assert!(c.model_identity(Uuid::nil()).await.is_err());
    }

    #[tokio::test]
    async fn deleting_an_endpoint_that_is_already_gone_succeeds() {
        let c = admin_stub(404, r#"{"error":"not found"}"#).await;
        assert!(c.delete_endpoint(Uuid::nil(), Uuid::nil()).await.is_ok());
        let c = admin_stub(500, r#"{"error":"db down"}"#).await;
        assert!(c.delete_endpoint(Uuid::nil(), Uuid::nil()).await.is_err());
    }

    #[test]
    fn tick_warnings_travel_as_a_bounded_json_header() {
        assert_eq!(warnings_header(&[]), None);
        let w = warnings_header(&["The Slurm token\nexpires in 2 days.".into(), String::new()])
            .unwrap();
        assert_eq!(w, r#"["The Slurm token expires in 2 days."]"#);
        let many: Vec<String> = (0..20).map(|i| format!("w{i}")).collect();
        let parsed: Vec<String> = serde_json::from_str(&warnings_header(&many).unwrap()).unwrap();
        assert_eq!(parsed.len(), TICK_WARNINGS_MAX);
    }

    #[tokio::test]
    async fn list_all_replicas_errors_on_500_and_error_envelope() {
        let c = admin_stub(500, r#"{"error":"db down"}"#).await;
        assert!(c.list_all_replicas().await.is_err(), "500 must be Err");
        let c = admin_stub(500, "[]").await;
        assert!(
            c.list_all_replicas().await.is_err(),
            "500 with an array body must be Err"
        );
        let c = admin_stub(200, r#"{"error":"x"}"#).await;
        assert!(
            c.list_all_replicas().await.is_err(),
            "non-array must be Err"
        );
        let c = admin_stub(200, "[]").await;
        assert!(c.list_all_replicas().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn list_endpoints_errors_on_500_and_error_envelope() {
        let m = Uuid::new_v4();
        let c = admin_stub(500, r#"{"error":"db down"}"#).await;
        assert!(c.list_endpoints(m).await.is_err(), "500 must be Err");
        let c = admin_stub(500, "[]").await;
        assert!(
            c.list_endpoints(m).await.is_err(),
            "500 with an array body must be Err"
        );
        let c = admin_stub(200, r#"{"error":"x"}"#).await;
        assert!(c.list_endpoints(m).await.is_err(), "non-array must be Err");
        let c = admin_stub(200, "[]").await;
        assert!(c.list_endpoints(m).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn non_array_error_body_is_truncated() {
        let long = format!(r#"{{"error":"{}"}}"#, "x".repeat(5_000));
        let c = admin_stub(200, Box::leak(long.into_boxed_str())).await;
        let err = c.list_managed_models().await.unwrap_err().to_string();
        assert!(err.contains("expected array"), "{err}");
        assert!(
            err.len() < 300,
            "error body not truncated: {} bytes",
            err.len()
        );
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
    /// Every replica patch in full: (replica_id, state, nodes, message).
    #[allow(clippy::type_complexity)]
    pub patches: std::sync::Mutex<Vec<(Uuid, Option<String>, Option<String>, Option<String>)>>,
    pub provision_errors: std::sync::Mutex<Vec<(Uuid, Option<String>)>>,
    /// Every details patch: (replica_id, details).
    pub details: std::sync::Mutex<Vec<(Uuid, ReplicaDetails)>>,
    /// Every backoff write: (model_id, state).
    pub backoffs: std::sync::Mutex<Vec<(Uuid, ManagedBackoff)>>,
    /// Endpoints `list_endpoints` reports for every model.
    pub endpoints: std::sync::Mutex<Vec<crate::domain::EndpointView>>,
    /// When set, `delete_endpoint` fails.
    pub fail_delete_endpoint: std::sync::atomic::AtomicBool,
    /// When set, `create_replica` returns an error — drives the compensating
    /// "cancel the orphan job" path in the Submit executor.
    pub fail_create_replica: std::sync::atomic::AtomicBool,
    /// When set, the tick's planning reads fail — drives the hold-the-tick path.
    pub fail_list_managed: std::sync::atomic::AtomicBool,
    pub fail_list_replicas: std::sync::atomic::AtomicBool,
}

#[cfg(test)]
#[async_trait]
impl OblethClient for MockObleth {
    async fn list_managed_models(&self) -> anyhow::Result<Vec<ManagedModelSpec>> {
        if self
            .fail_list_managed
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            anyhow::bail!("simulated list_managed_models failure");
        }
        Ok(self.managed.lock().unwrap().clone())
    }
    async fn get_slurm_settings(&self) -> anyhow::Result<Option<SlurmSettings>> {
        Ok(self.slurm.lock().unwrap().clone())
    }
    async fn model_name(&self, _model_id: Uuid) -> anyhow::Result<String> {
        Ok("test-model".to_string())
    }
    async fn list_all_replicas(&self) -> anyhow::Result<Vec<ReplicaView>> {
        if self
            .fail_list_replicas
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            anyhow::bail!("simulated list_all_replicas failure");
        }
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
        nodes: Option<&str>,
        _endpoint_id: Option<Uuid>,
        message: Option<&str>,
    ) -> anyhow::Result<()> {
        self.patched
            .lock()
            .unwrap()
            .push((replica_id, state.map(|s| s.to_string())));
        self.patches.lock().unwrap().push((
            replica_id,
            state.map(str::to_string),
            nodes.map(str::to_string),
            message.map(str::to_string),
        ));
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
        // Canned endpoints (none by default): each test promote then creates
        // exactly one endpoint.
        Ok(self.endpoints.lock().unwrap().clone())
    }
    async fn delete_endpoint(&self, _model_id: Uuid, endpoint_id: Uuid) -> anyhow::Result<()> {
        if self
            .fail_delete_endpoint
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            anyhow::bail!("simulated delete_endpoint failure");
        }
        self.deleted_endpoints.lock().unwrap().push(endpoint_id);
        Ok(())
    }
    async fn patch_replica_details(
        &self,
        replica_id: Uuid,
        details: &ReplicaDetails,
    ) -> anyhow::Result<()> {
        self.details
            .lock()
            .unwrap()
            .push((replica_id, details.clone()));
        Ok(())
    }
    async fn set_managed_backoff(
        &self,
        model_id: Uuid,
        backoff: &ManagedBackoff,
    ) -> anyhow::Result<()> {
        self.backoffs
            .lock()
            .unwrap()
            .push((model_id, backoff.clone()));
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
