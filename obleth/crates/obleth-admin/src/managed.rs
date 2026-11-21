//! Managed (Slurm-provisioned) model specs and the replica registry.

use super::*;

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct PutManagedModel {
    #[serde(default = "default_true")]
    enabled: bool,
    /// Empty uses the cluster's default partition.
    #[serde(default)]
    partition: String,
    #[serde(default)]
    gres: String,
    #[serde(default = "one")]
    nodes: i64,
    constraints: Option<String>,
    exclude: Option<String>,
    account: Option<String>,
    qos: Option<String>,
    time_limit: Option<String>,
    #[serde(default)]
    cpus_per_task: Option<i64>,
    #[serde(default)]
    mem: Option<String>,
    #[serde(default)]
    image: String,
    #[serde(default)]
    preamble: String,
    #[serde(default)]
    log_output_dir: String,
    #[serde(default)]
    launch_command: String,
    #[serde(default)]
    script_body: String,
    serving_port: i64,
    #[serde(default = "default_health_path")]
    health_path: String,
    #[serde(default = "two")]
    target_replicas: i64,
    #[serde(default = "one")]
    min_replicas: i64,
    #[serde(default)]
    max_job_failures: i64,
    #[serde(default)]
    launcher_spec: Option<serde_json::Value>,
    /// Minutes a running job's server may take to become healthy before the
    /// job is cancelled and counted as a failed launch. 0 never gives up.
    #[serde(default = "default_startup_timeout_mins")]
    startup_timeout_mins: i64,
    /// Minutes before a healthy replica's time limit at which a replacement
    /// is submitted. 0 turns the handoff off.
    #[serde(default = "default_handoff_mins")]
    handoff_mins: i64,
}

fn default_startup_timeout_mins() -> i64 {
    obleth_config::DEFAULT_STARTUP_TIMEOUT_MINS
}

fn default_handoff_mins() -> i64 {
    obleth_config::DEFAULT_HANDOFF_MINS
}

/// Ports the provisioner gives each replica by default (its window).
const DEFAULT_PORT_SPAN: i64 = 8;

/// Every problem with a managed spec, one plain sentence each. Empty when
/// the spec can be stored.
fn managed_spec_problems(b: &PutManagedModel) -> Vec<String> {
    use obleth_config::slurm as sv;
    let mut out = Vec::new();
    if !(1..=65535).contains(&b.serving_port) {
        out.push("The serving port must be between 1 and 65535.".to_string());
    } else {
        let last = b.serving_port + b.target_replicas.max(1) * DEFAULT_PORT_SPAN - 1;
        if last > 65535 {
            out.push(format!(
                "The serving port is too high: {} replicas need ports up to {last}, past \
                 65535. Use a lower port.",
                b.target_replicas.max(1)
            ));
        }
    }
    let health = b.health_path.trim();
    if !health.is_empty() && !health.starts_with('/') {
        out.push("The health path must start with /, for example /health.".to_string());
    }
    if b.target_replicas < 0 {
        out.push("The replica count cannot be negative. Use 0 to keep none running.".to_string());
    } else if b.min_replicas < 0 || b.min_replicas > b.target_replicas {
        out.push(format!(
            "The healthy-replica floor must be between 0 and the replica count ({}).",
            b.target_replicas
        ));
    }
    if b.max_job_failures < 0 {
        out.push("The failure limit cannot be negative. Use 0 for no limit.".to_string());
    }
    if b.startup_timeout_mins < 0 {
        out.push("The startup timeout cannot be negative. Use 0 to wait forever.".to_string());
    }
    if b.handoff_mins < 0 {
        out.push("The handoff time cannot be negative. Use 0 to turn it off.".to_string());
    }
    if b.nodes < 1 {
        out.push("A job needs at least one node.".to_string());
    }
    if b.cpus_per_task.is_some_and(|c| c < 0) {
        out.push(
            "CPUs per task cannot be negative. Leave it empty for the cluster default.".to_string(),
        );
    }
    if let Some(t) = b.time_limit.as_deref() {
        if let Err(e) = sv::parse_time_limit(t) {
            out.push(e);
        }
    }
    if let Some(m) = b.mem.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
        if sv::parse_mem_mb(m).is_none() {
            out.push(format!(
                "The memory request \"{m}\" is not a Slurm size. Use a number with a unit, for \
                 example 64G or 512000M."
            ));
        }
    }
    if let Err(e) = sv::validate_gres(&b.gres) {
        out.push(e);
    }
    if b.script_body.trim().is_empty() && b.launch_command.trim().is_empty() {
        out.push("Give the job a script or a launch command.".to_string());
    }
    out
}

#[cfg(test)]
mod managed_spec_tests {
    use super::*;

    fn spec(extra: serde_json::Value) -> PutManagedModel {
        let mut v = serde_json::json!({ "serving_port": 8000, "launch_command": "serve" });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_minimal_spec_is_valid_and_takes_the_defaults() {
        let b = spec(serde_json::json!({}));
        assert!(
            managed_spec_problems(&b).is_empty(),
            "{:?}",
            managed_spec_problems(&b)
        );
        assert_eq!(b.partition, "", "empty partition uses the cluster default");
        assert_eq!(b.startup_timeout_mins, 60);
        assert_eq!(b.handoff_mins, 15);
        let zero = spec(serde_json::json!({ "target_replicas": 0, "min_replicas": 0 }));
        assert!(
            managed_spec_problems(&zero).is_empty(),
            "0 replicas is allowed"
        );
    }

    #[test]
    fn every_problem_is_named_in_a_plain_sentence() {
        let b = spec(serde_json::json!({
            "serving_port": 65530,
            "health_path": "health",
            "target_replicas": 2,
            "min_replicas": 3,
            "max_job_failures": -1,
            "time_limit": "2d",
            "mem": "lots",
            "gres": "gpu a100",
            "nodes": 0,
            "cpus_per_task": -2,
            "startup_timeout_mins": -1,
            "launch_command": "",
        }));
        let p = managed_spec_problems(&b);
        let all = p.join(" ");
        for needle in [
            "too high",
            "must start with /",
            "between 0 and the replica count (2)",
            "failure limit",
            "\"2d\" is not a Slurm walltime",
            "\"lots\" is not a Slurm size",
            "not a Slurm GRES",
            "at least one node",
            "CPUs per task",
            "startup timeout",
            "script or a launch command",
        ] {
            assert!(all.contains(needle), "missing {needle:?} in {all}");
        }
        assert!(p.iter().all(|m| m.ends_with('.')), "{p:?}");
        let neg = spec(serde_json::json!({ "target_replicas": -1 }));
        assert!(managed_spec_problems(&neg)[0].contains("cannot be negative"));
        for ok in ["UNLIMITED", "1-00:00:00", "90", ""] {
            let b = spec(serde_json::json!({ "time_limit": ok }));
            assert!(managed_spec_problems(&b).is_empty(), "{ok}");
        }
    }

    #[test]
    fn audit_hides_launch_environment_values_and_runtime_state() {
        let spec = obleth_config::ManagedModelSpec {
            launcher_spec: Some(serde_json::json!({ "env": { "HF_TOKEN": "s3cret" } })),
            paused_reason: Some("Paused.".into()),
            ..Default::default()
        };
        let a = managed_model_audit(&spec);
        assert_eq!(a["launcher_spec"]["env"]["HF_TOKEN"], "(hidden)");
        assert!(a.get("paused_reason").is_none());
        assert!(!a.to_string().contains("s3cret"));
    }
}

fn one() -> i64 {
    1
}

fn two() -> i64 {
    2
}

fn default_health_path() -> String {
    "/health".into()
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct CreateReplica {
    slurm_job_id: String,
    #[serde(default)]
    port_base: Option<i64>,
    /// Where the job's output goes.
    #[serde(default)]
    log_path: Option<String>,
}

#[derive(Debug, Deserialize, ToSchema)]
pub(crate) struct PatchReplica {
    state: Option<String>,
    message: Option<String>,
    nodes: Option<String>,
    #[schema(value_type = Option<String>)]
    endpoint_id: Option<Uuid>,
    /// When Slurm started the job the replica is promoted under.
    #[serde(default)]
    job_started_at: Option<chrono::DateTime<chrono::Utc>>,
    /// When the running job reaches its time limit.
    #[serde(default)]
    job_ends_at: Option<chrono::DateTime<chrono::Utc>>,
    /// Plain sentence for the dashboard; an empty string clears it.
    #[serde(default)]
    state_detail: Option<String>,
    /// Where the job's output goes; an empty string clears it.
    #[serde(default)]
    log_path: Option<String>,
    /// The port the server was found on.
    #[serde(default)]
    port: Option<i64>,
    /// Set or clear the surge-first replacement flag.
    #[serde(default)]
    replace_requested: Option<bool>,
    /// How the job ended, for the launch history, sent with a `lost` or
    /// `draining` state: Slurm's terminal state, `gone`, or
    /// `cancelled:<why>`. Without it the end is read from `message`.
    #[serde(default)]
    end_state: Option<String>,
    #[serde(default)]
    end_reason: Option<String>,
}

impl PatchReplica {
    fn details(&self) -> Option<obleth_store::ReplicaDetailsUpdate> {
        let d = obleth_store::ReplicaDetailsUpdate {
            job_started_at: self.job_started_at,
            job_ends_at: self.job_ends_at,
            state_detail: self.state_detail.clone(),
            log_path: self.log_path.clone(),
            port: self.port.filter(|p| (1..=65535).contains(p)),
            replace_requested: self.replace_requested,
        };
        (d.job_started_at.is_some()
            || d.job_ends_at.is_some()
            || d.state_detail.is_some()
            || d.log_path.is_some()
            || d.port.is_some()
            || d.replace_requested.is_some())
        .then_some(d)
    }
}

/// `DELETE /api/v1/replicas/{id}` options.
#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
pub(crate) struct DeleteReplicaQuery {
    /// Drop the row whatever its state (the provisioner's clean-up of a
    /// finished job). Without it a live replica is cancelled instead.
    #[serde(default)]
    force: bool,
}

#[utoipa::path(get, path = "/api/v1/managed", tag = "models",
    responses((status = 200, body = [ManagedModelSpec])))]
pub(crate) async fn list_managed_models(
    State(state): State<AdminState>,
) -> Result<Json<Vec<ManagedModelSpec>>> {
    Ok(Json(state.store.list_managed_models().await?))
}

#[utoipa::path(get, path = "/api/v1/models/{id}/managed", tag = "models",
    responses((status = 200, body = Option<ManagedModelSpec>)))]
pub(crate) async fn get_managed_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Option<ManagedModelSpec>>> {
    Ok(Json(state.store.get_managed_model(id).await?))
}

#[utoipa::path(put, path = "/api/v1/models/{id}/managed", tag = "models",
    request_body = PutManagedModel,
    responses((status = 200, body = ManagedModelSpec)))]
pub(crate) async fn put_managed_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<PutManagedModel>,
) -> Result<Json<ManagedModelSpec>> {
    // image is optional: an empty image means bare-metal (no apptainer
    // wrap), and an empty partition uses the cluster's default.
    let problems = managed_spec_problems(&body);
    if !problems.is_empty() {
        return Err(AdminError::BadRequest(problems.join(" ")));
    }
    let health_path = match body.health_path.trim() {
        "" => "/health".to_string(),
        p => p.to_string(),
    };
    let opt = |v: Option<String>| v.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    let spec = state
        .store
        .upsert_managed_model(obleth_store::UpsertManagedModel {
            model_id: id,
            enabled: body.enabled,
            partition: body.partition.trim().to_string(),
            gres: body.gres.trim().to_string(),
            nodes: body.nodes,
            constraints: opt(body.constraints),
            exclude: opt(body.exclude),
            account: opt(body.account),
            qos: opt(body.qos),
            time_limit: opt(body.time_limit),
            cpus_per_task: body.cpus_per_task.filter(|c| *c > 0),
            mem: opt(body.mem),
            image: body.image.trim().to_string(),
            // Stored with Unix line endings: bash reads a `\r` as part of
            // the line, and a browser textarea sends CRLF.
            preamble: body.preamble.replace("\r\n", "\n"),
            log_output_dir: body.log_output_dir,
            launch_command: body.launch_command.replace("\r\n", "\n"),
            script_body: body.script_body.replace("\r\n", "\n"),
            serving_port: body.serving_port,
            health_path,
            target_replicas: body.target_replicas,
            min_replicas: body.min_replicas,
            max_job_failures: body.max_job_failures,
            launcher_spec: body.launcher_spec,
            startup_timeout_mins: body.startup_timeout_mins,
            handoff_mins: body.handoff_mins,
        })
        .await?;
    state
        .audit(
            &headers,
            "put_managed_model",
            "managed_model",
            id,
            managed_model_audit(&spec),
        )
        .await?;
    Ok(Json(spec))
}

/// What the audit log keeps of a managed model's spec: everything but the
/// shell it runs. The preamble, launch command and script body are free-form
/// and may carry credentials, and the audit log is searchable and never
/// pruned, so each is recorded as a line count (as the Slurm settings' setup is).
fn managed_model_audit(spec: &impl serde::Serialize) -> serde_json::Value {
    let mut detail = serde_json::to_value(spec).unwrap_or_default();
    if let Some(fields) = detail.as_object_mut() {
        for name in ["preamble", "launch_command", "script_body"] {
            if let Some(text) = fields.remove(name) {
                let lines = text.as_str().map_or(0, |t| t.lines().count());
                fields.insert(format!("{name}_lines"), lines.into());
            }
        }
        // The launcher panel state carries environment values and recipe
        // inputs typed at launch; keep names, hide values that may be secret.
        if let Some(ls) = fields.get_mut("launcher_spec") {
            *ls = obleth_config::slurm::redact_launcher_spec(ls);
        }
        // Runtime state, not something the operator set.
        for name in [
            "last_provision_error",
            "last_provision_error_at",
            "paused_reason",
            "paused_until",
            "failed_launches",
            "backoff_step",
        ] {
            fields.remove(name);
        }
    }
    detail
}

#[utoipa::path(delete, path = "/api/v1/models/{id}/managed", tag = "models",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 204), (status = 404, description = "The model has no launch spec")))]
pub(crate) async fn delete_managed_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    let previous = state
        .store
        .get_managed_model(id)
        .await?
        .ok_or(obleth_store::StoreError::NotFound)?;
    state.store.delete_managed_model(id).await?;
    state
        .audit(
            &headers,
            "delete_managed_model",
            "managed_model",
            id,
            managed_model_audit(&previous),
        )
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(serde::Deserialize, ToSchema)]
pub struct ProvisionErrorBody {
    #[serde(default)]
    pub error: Option<String>,
}

#[utoipa::path(patch, path = "/api/v1/models/{id}/managed/provision-error", tag = "models",
    request_body = ProvisionErrorBody,
    responses((status = 200)))]
pub(crate) async fn set_provision_error(
    State(state): State<AdminState>,
    Path(id): Path<uuid::Uuid>,
    headers: HeaderMap,
    Json(body): Json<ProvisionErrorBody>,
) -> Result<Json<serde_json::Value>> {
    // Only a clear is an operator-visible change worth auditing: the
    // provisioner records an error on every failed submit and clears on every
    // successful one, so the audit row is written only when a clear actually
    // removed a recorded error.
    let cleared = match body.error {
        None => state
            .store
            .get_managed_model(id)
            .await?
            .and_then(|m| m.last_provision_error),
        Some(_) => None,
    };
    state
        .store
        .set_provision_error(id, body.error.as_deref())
        .await?;
    if let Some(previous) = cleared {
        state
            .audit(
                &headers,
                "clear_provision_error",
                "managed_model",
                id,
                serde_json::json!({ "cleared_error": previous }),
            )
            .await?;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// What `retry` and `roll` answer.
#[derive(Debug, serde::Serialize, ToSchema)]
pub(crate) struct ManagedActionResult {
    pub ok: bool,
    /// Replicas flagged for replacement (roll only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replicas: Option<u64>,
    /// What happens next, as a plain sentence.
    pub message: String,
}

/// "Try again": lift a pause set by the failure limit or a Slurm rejection,
/// and reset the backoff, so the provisioner's next pass submits.
#[utoipa::path(post, path = "/api/v1/models/{id}/managed/retry", tag = "slurm",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 200, body = ManagedActionResult), (status = 404)))]
pub(crate) async fn retry_managed_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<ManagedActionResult>> {
    let Some(before) = state.store.get_managed_model(id).await? else {
        return Err(AdminError::NotFound);
    };
    state.store.clear_managed_backoff(id).await?;
    state
        .audit(
            &headers,
            "retry_managed_model",
            "managed_model",
            id,
            serde_json::json!({
                "paused_reason": before.paused_reason,
                "failed_launches": before.failed_launches,
            }),
        )
        .await?;
    Ok(Json(ManagedActionResult {
        ok: true,
        replicas: None,
        message: "Submits are no longer paused. The provisioner starts jobs on its next pass."
            .into(),
    }))
}

/// Replace every live replica one at a time, each replacement started before
/// the old one is drained, so spec edits apply without an outage.
#[utoipa::path(post, path = "/api/v1/models/{id}/managed/roll", tag = "slurm",
    params(("id" = Uuid, Path, description = "Model id")),
    responses((status = 200, body = ManagedActionResult), (status = 404)))]
pub(crate) async fn roll_managed_model(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<ManagedActionResult>> {
    if state.store.get_managed_model(id).await?.is_none() {
        return Err(AdminError::NotFound);
    }
    let n = state.store.request_model_roll(id).await?;
    state
        .audit(
            &headers,
            "roll_managed_model",
            "managed_model",
            id,
            serde_json::json!({ "replicas": n }),
        )
        .await?;
    let message = if n == 0 {
        "No running replicas to replace. New jobs use the current settings.".to_string()
    } else {
        format!(
            "{n} replica{} will be replaced one at a time. Each keeps serving until its \
             replacement is healthy.",
            if n == 1 { "" } else { "s" }
        )
    };
    Ok(Json(ManagedActionResult {
        ok: true,
        replicas: Some(n),
        message,
    }))
}

/// The provisioner records a managed model's failure count and pause state.
#[utoipa::path(put, path = "/api/v1/models/{id}/managed/backoff", tag = "slurm",
    params(("id" = Uuid, Path, description = "Model id")),
    request_body = ManagedBackoff,
    responses((status = 200)))]
pub(crate) async fn set_managed_backoff(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<ManagedBackoff>,
) -> Result<Json<serde_json::Value>> {
    let before = state.store.get_managed_model(id).await?;
    state.store.set_managed_backoff(id, &body).await?;
    // Only a new pause is worth an audit row; the counters move every launch.
    let newly_paused = body.paused_reason.is_some()
        && before
            .as_ref()
            .is_some_and(|m| m.paused_reason != body.paused_reason);
    if newly_paused {
        state
            .audit(
                &headers,
                "pause_managed_model",
                "managed_model",
                id,
                serde_json::json!({
                    "paused_reason": body.paused_reason,
                    "paused_until": body.paused_until,
                    "failed_launches": body.failed_launches,
                }),
            )
            .await?;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[utoipa::path(get, path = "/api/v1/models/{id}/replicas", tag = "models",
    responses((status = 200, body = [ModelReplica])))]
pub(crate) async fn list_replicas(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<ModelReplica>>> {
    Ok(Json(state.store.list_replicas(id).await?))
}

#[utoipa::path(get, path = "/api/v1/replicas", tag = "models",
    responses((status = 200, body = [ModelReplica])))]
pub(crate) async fn list_all_replicas(
    State(state): State<AdminState>,
) -> Result<Json<Vec<ModelReplica>>> {
    Ok(Json(state.store.all_replicas().await?))
}

#[utoipa::path(post, path = "/api/v1/models/{id}/replicas", tag = "models",
    request_body = CreateReplica,
    responses((status = 200, body = ModelReplica)))]
pub(crate) async fn create_replica(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<CreateReplica>,
) -> Result<Json<ModelReplica>> {
    let r = state
        .store
        .create_replica_with_log(
            id,
            &body.slurm_job_id,
            body.port_base,
            body.log_path.as_deref(),
        )
        .await?;
    state
        .audit(
            &headers,
            "create_replica",
            "model_replica",
            r.id,
            serde_json::to_value(&r).unwrap_or_default(),
        )
        .await?;
    Ok(Json(r))
}

#[utoipa::path(patch, path = "/api/v1/replicas/{id}", tag = "models",
    request_body = PatchReplica,
    responses((status = 200, body = ModelReplica)))]
pub(crate) async fn patch_replica(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<PatchReplica>,
) -> Result<Json<ModelReplica>> {
    // Validate up front so an invalid state can't partially apply runtime changes.
    if let Some(state_val) = body.state.as_deref() {
        if !obleth_config::REPLICA_STATES.contains(&state_val) {
            return Err(AdminError::BadRequest("invalid replica state".into()));
        }
    }
    // Apply runtime (nodes/endpoint) BEFORE flipping state. If linking the
    // endpoint fails (e.g. an invalid endpoint_id), we must not have already
    // marked the replica healthy — that would strand it as "healthy" with no
    // endpoint, which the planner never re-promotes.
    if body.nodes.is_some() || body.endpoint_id.is_some() {
        state
            .store
            .set_replica_runtime(id, body.nodes.as_deref(), body.endpoint_id)
            .await?;
    }
    if let Some(d) = body.details() {
        state.store.set_replica_details(id, &d).await?;
    }
    if let Some(state_val) = body.state.as_deref() {
        state
            .store
            .update_replica_state(
                id,
                state_val,
                body.message.as_deref(),
                obleth_store::LaunchEnd {
                    state: body.end_state.as_deref(),
                    reason: body.end_reason.as_deref(),
                },
            )
            .await?;
    } else if let Some(msg) = body.message.as_deref() {
        state.store.set_replica_message(id, msg).await?;
    }
    let current = state
        .store
        .get_replica(id)
        .await?
        .ok_or(AdminError::NotFound)?;
    state
        .audit(
            &headers,
            "patch_replica",
            "model_replica",
            id,
            serde_json::to_value(&current).unwrap_or_default(),
        )
        .await?;
    Ok(Json(current))
}

#[utoipa::path(post, path = "/api/v1/replicas/{id}/restart", tag = "models",
    responses((status = 200)))]
pub(crate) async fn restart_replica(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let replica = state
        .store
        .get_replica(id)
        .await?
        .ok_or(AdminError::NotFound)?;
    if matches!(replica.state.as_str(), "lost" | "draining") || replica.cancel_requested {
        return Err(AdminError::Conflict(
            "This replica is already stopping or has ended, so there is nothing to restart. \
             The replica count brings in a new one."
                .into(),
        ));
    }
    // Surge-first: the provisioner starts a successor and drains this one
    // only once the successor serves (or after the startup timeout).
    state.store.request_replica_replace(id).await?;
    state
        .audit(
            &headers,
            "restart_replica",
            "model_replica",
            id,
            serde_json::json!({ "replace": true }),
        )
        .await?;
    Ok(Json(serde_json::json!({
        "ok": true,
        "message": "A replacement job starts now. This replica keeps serving until the \
                    replacement is healthy, then it is drained and cancelled.",
    })))
}

#[utoipa::path(delete, path = "/api/v1/replicas/{id}", tag = "models",
    params(DeleteReplicaQuery),
    responses(
        (status = 200, description = "The row was deleted (a lost replica, or `force`)"),
        (status = 202, description = "The replica's job is being cancelled; the row goes once it has ended"),
        (status = 404)))]
pub(crate) async fn delete_replica(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    Query(q): Query<DeleteReplicaQuery>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    if !q.force {
        let replica = state
            .store
            .get_replica(id)
            .await?
            .ok_or(AdminError::NotFound)?;
        if replica.state != "lost" {
            // A live replica: cancel its job (the provisioner removes the
            // endpoint first) and let the clean-up drop the row once it ends.
            state.store.request_replica_cancel(id).await?;
            state
                .audit(
                    &headers,
                    "cancel_replica",
                    "model_replica",
                    id,
                    serde_json::json!({ "state": replica.state }),
                )
                .await?;
            return Ok((
                StatusCode::ACCEPTED,
                Json(serde_json::json!({
                    "deleted": false,
                    "cancelling": true,
                    "message": "The replica's job is being cancelled. It is removed once the \
                                job has ended.",
                })),
            ));
        }
    }
    state.store.delete_replica(id).await?;
    state
        .audit(
            &headers,
            "delete_replica",
            "model_replica",
            id,
            serde_json::json!({ "force": q.force }),
        )
        .await?;
    Ok((StatusCode::OK, Json(serde_json::json!({"deleted": true}))))
}

#[utoipa::path(post, path = "/api/v1/models/{id}/replicas/clear-lost", tag = "models",
    responses((status = 200, body = serde_json::Value)))]
pub async fn clear_lost_replicas(
    State(state): State<AdminState>,
    Path(id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<serde_json::Value>> {
    let n = state.store.delete_lost_replicas(id).await?;
    state
        .audit(
            &headers,
            "clear_lost_replicas",
            "model",
            id,
            serde_json::json!({ "deleted": n }),
        )
        .await?;
    Ok(Json(serde_json::json!({ "deleted": n })))
}

#[derive(Debug, Deserialize, utoipa::IntoParams, ToSchema)]
pub struct DeploymentLaunchesQuery {
    /// Only launches of this model.
    #[param(value_type = Option<String>)]
    #[schema(value_type = Option<String>)]
    pub model_id: Option<Uuid>,
    /// Only launches from this recipe (`launcher_spec.recipe_id`).
    pub recipe_id: Option<String>,
    /// Rows to return, newest first. Default 50, at most 500.
    pub limit: Option<i64>,
}

/// Launch history of Slurm-backed replicas, newest first. Rows outlive the
/// replica rows they describe; `queued_secs`, `load_secs` and `ran_secs` are
/// computed (ran counts to now while the launch is still going).
#[utoipa::path(get, path = "/api/v1/deployments/launches", tag = "models",
    params(DeploymentLaunchesQuery),
    responses((status = 200, body = [DeploymentLaunch])))]
pub(crate) async fn list_deployment_launches(
    State(state): State<AdminState>,
    Query(q): Query<DeploymentLaunchesQuery>,
) -> Result<Json<Vec<DeploymentLaunch>>> {
    let recipe_id = q
        .recipe_id
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty());
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    Ok(Json(
        state
            .store
            .list_deployment_launches(q.model_id, recipe_id, limit)
            .await?,
    ))
}
