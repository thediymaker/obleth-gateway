//! Managed (Slurm-provisioned) models, recipes, replicas and launches.

use super::*;

pub struct Recipe {
    pub id: Uuid,
    pub name: String,
    pub body: String,
    pub author: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

/// What a recipe is made of or changed to; `create_recipe` mints the id.
pub struct NewRecipe {
    pub name: String,
    pub body: String,
    pub author: String,
}

/// Input for `upsert_managed_model` (no timestamps; the DB sets them).
#[derive(Debug, Clone)]
pub struct UpsertManagedModel {
    pub model_id: Uuid,
    pub enabled: bool,
    pub partition: String,
    pub gres: String,
    pub nodes: i64,
    pub constraints: Option<String>,
    pub exclude: Option<String>,
    pub account: Option<String>,
    pub qos: Option<String>,
    pub time_limit: Option<String>,
    pub cpus_per_task: Option<i64>,
    pub mem: Option<String>,
    pub image: String,
    pub preamble: String,
    pub log_output_dir: String,
    pub launch_command: String,
    pub script_body: String,
    pub serving_port: i64,
    pub health_path: String,
    pub target_replicas: i64,
    pub min_replicas: i64,
    pub max_job_failures: i64,
    pub launcher_spec: Option<serde_json::Value>,
    /// Minutes a running job may take to serve; 0 never gives up.
    pub startup_timeout_mins: i64,
    /// Minutes before the time limit a replacement starts; 0 is off.
    pub handoff_mins: i64,
    /// Start each replica with its own API key.
    pub replica_api_key: bool,
}

/// Provisioner-reported replica details. `None` keeps the stored value.
#[derive(Debug, Clone, Default)]
pub struct ReplicaDetailsUpdate {
    pub job_started_at: Option<chrono::DateTime<chrono::Utc>>,
    pub job_ends_at: Option<chrono::DateTime<chrono::Utc>>,
    /// An empty string clears it.
    pub state_detail: Option<String>,
    /// An empty string clears it.
    pub log_path: Option<String>,
    pub port: Option<i64>,
    pub replace_requested: Option<bool>,
}

pub use obleth_config::ManagedBackoff;

fn recipe_from_row(row: &PgRow) -> Result<Recipe> {
    Ok(Recipe {
        id: row.try_get("id")?,
        name: row.try_get("name")?,
        body: row.try_get("body")?,
        author: row.try_get("author")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn managed_model_from_row(row: &PgRow) -> Result<ManagedModelSpec> {
    let launcher_spec: Option<sqlx::types::Json<serde_json::Value>> =
        row.try_get("launcher_spec")?;
    Ok(ManagedModelSpec {
        model_id: row.try_get("model_id")?,
        enabled: row.try_get("enabled")?,
        partition: row.try_get("partition")?,
        gres: row.try_get("gres")?,
        nodes: row.try_get("nodes")?,
        constraints: row.try_get("constraints")?,
        exclude: row.try_get("exclude")?,
        account: row.try_get("account")?,
        qos: row.try_get("qos")?,
        time_limit: row.try_get("time_limit")?,
        cpus_per_task: row.try_get("cpus_per_task")?,
        mem: row.try_get("mem")?,
        image: row.try_get("image")?,
        preamble: row.try_get("preamble")?,
        log_output_dir: row.try_get("log_output_dir")?,
        launch_command: row.try_get("launch_command")?,
        script_body: row.try_get("script_body")?,
        serving_port: row.try_get("serving_port")?,
        health_path: row.try_get("health_path")?,
        target_replicas: row.try_get("target_replicas")?,
        min_replicas: row.try_get("min_replicas")?,
        max_job_failures: row.try_get("max_job_failures")?,
        launcher_spec: launcher_spec.map(|j| j.0),
        last_provision_error: row.try_get("last_provision_error")?,
        last_provision_error_at: row.try_get("last_provision_error_at")?,
        startup_timeout_mins: row.try_get("startup_timeout_mins")?,
        handoff_mins: row.try_get("handoff_mins")?,
        replica_api_key: row.try_get("replica_api_key")?,
        paused_reason: row.try_get("paused_reason")?,
        paused_until: row.try_get("paused_until")?,
        failed_launches: row.try_get("failed_launches")?,
        backoff_step: row.try_get("backoff_step")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn replica_from_row(row: &PgRow) -> Result<ModelReplica> {
    Ok(ModelReplica {
        id: row.try_get("id")?,
        model_id: row.try_get("model_id")?,
        slurm_job_id: row.try_get("slurm_job_id")?,
        nodes: row.try_get("nodes")?,
        endpoint_id: row.try_get("endpoint_id")?,
        state: row.try_get("state")?,
        last_message: row.try_get("last_message")?,
        port_base: row.try_get("port_base")?,
        cancel_requested: row.try_get("cancel_requested")?,
        replace_requested: row.try_get("replace_requested")?,
        job_started_at: row.try_get("job_started_at")?,
        job_ends_at: row.try_get("job_ends_at")?,
        state_detail: row.try_get("state_detail")?,
        log_path: row.try_get("log_path")?,
        port: row.try_get("port")?,
        created_at: row.try_get("created_at")?,
        updated_at: row.try_get("updated_at")?,
    })
}

fn launch_from_row(row: &PgRow) -> Result<DeploymentLaunch> {
    let launcher_spec: Option<sqlx::types::Json<serde_json::Value>> =
        row.try_get("launcher_spec")?;
    Ok(DeploymentLaunch {
        id: row.try_get("id")?,
        model_id: row.try_get("model_id")?,
        model_name: row.try_get("model_name")?,
        recipe_id: row.try_get("recipe_id")?,
        slurm_job_id: row.try_get("slurm_job_id")?,
        partition: row.try_get("partition")?,
        account: row.try_get("account")?,
        qos: row.try_get("qos")?,
        time_limit: row.try_get("time_limit")?,
        gres: row.try_get("gres")?,
        nodes_requested: row.try_get("nodes_requested")?,
        cpus_per_task: row.try_get("cpus_per_task")?,
        mem: row.try_get("mem")?,
        exclude: row.try_get("exclude")?,
        constraints: row.try_get("constraints")?,
        launcher_spec: launcher_spec.map(|j| j.0),
        nodes: row.try_get("nodes")?,
        submitted_at: row.try_get("submitted_at")?,
        started_at: row.try_get("started_at")?,
        healthy_at: row.try_get("healthy_at")?,
        ended_at: row.try_get("ended_at")?,
        end_state: row.try_get("end_state")?,
        end_reason: row.try_get("end_reason")?,
        updated_at: row.try_get("updated_at")?,
        queued_secs: row.try_get("queued_secs")?,
        load_secs: row.try_get("load_secs")?,
        ran_secs: row.try_get("ran_secs")?,
    })
}

/// What a replica state change means for its launch-history row.
#[derive(Debug, Default, PartialEq, Eq)]
struct LaunchTransition {
    started: bool,
    healthy: bool,
    ended: bool,
    end_state: Option<String>,
    end_reason: Option<String>,
}

/// How a replica's job ended, as the provisioner reports it alongside a
/// `lost` or `draining` state: Slurm's terminal state, `gone`, or
/// `cancelled:<why>`, and the reason.
#[derive(Debug, Clone, Copy, Default)]
pub struct LaunchEnd<'a> {
    pub state: Option<&'a str>,
    pub reason: Option<&'a str>,
}

/// Map a replica state change onto the launch row. A provisioner that names
/// how the job ended (`end`) is taken at its word; without it (an older
/// provisioner) the end is read from the message: `job ended: STATE
/// (reason)` or `job gone` for `lost`, the cancel reason for `draining`.
fn launch_transition(state: &str, message: Option<&str>, end: LaunchEnd<'_>) -> LaunchTransition {
    let mut t = launch_transition_from_message(state, message);
    if let (true, Some(end_state)) = (t.ended, end.state.map(str::trim).filter(|s| !s.is_empty())) {
        t.end_state = Some(end_state.to_string());
        t.end_reason = end
            .reason
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(str::to_string);
    }
    t
}

fn launch_transition_from_message(state: &str, message: Option<&str>) -> LaunchTransition {
    let msg = message.map(str::trim).filter(|m| !m.is_empty());
    match state {
        "starting" => LaunchTransition {
            started: true,
            ..Default::default()
        },
        "healthy" => LaunchTransition {
            started: true,
            healthy: true,
            ..Default::default()
        },
        "lost" => {
            let (end_state, end_reason) = match msg {
                Some(m) if m.starts_with("job ended:") => {
                    let rest = m["job ended:".len()..].trim();
                    match rest.split_once(" (") {
                        Some((s, r)) => (
                            s.trim().to_string(),
                            Some(r.trim_end_matches(')').trim().to_string())
                                .filter(|r| !r.is_empty()),
                        ),
                        None => (rest.to_string(), None),
                    }
                }
                Some("job gone") => ("gone".to_string(), None),
                other => ("lost".to_string(), other.map(str::to_string)),
            };
            let end_state = if end_state.is_empty() {
                "gone".to_string()
            } else {
                end_state
            };
            LaunchTransition {
                ended: true,
                end_state: Some(end_state),
                end_reason,
                ..Default::default()
            }
        }
        "draining" => {
            let end_state = match msg {
                Some("scaled down") => "cancelled:scale-down",
                Some("restart requested") => "cancelled:restart",
                Some(m) if m.starts_with("restarting: failed health probes") => {
                    "cancelled:probe-failed"
                }
                Some("cancelled: server did not become healthy in time") => {
                    "cancelled:startup-timeout"
                }
                Some("cancelled: job could not start") => "cancelled:stuck",
                Some("replaced: a successor is serving") => "cancelled:replaced",
                Some("stopping: deployment paused or removed") => "cancelled:removed",
                _ => "cancelled",
            };
            LaunchTransition {
                ended: true,
                end_state: Some(end_state.to_string()),
                end_reason: msg.map(str::to_string),
                ..Default::default()
            }
        }
        _ => LaunchTransition::default(),
    }
}

impl Store {
    pub async fn get_managed_model(&self, model_id: Uuid) -> Result<Option<ManagedModelSpec>> {
        let row = sqlx::query(
            "select model_id, enabled, partition, gres, nodes, constraints, exclude, account, \
             qos, time_limit, cpus_per_task, mem, image, preamble, log_output_dir, launch_command, \
             script_body, serving_port, \
             health_path, target_replicas, min_replicas, max_job_failures, launcher_spec, \
             last_provision_error, last_provision_error_at, startup_timeout_mins, handoff_mins, \
             replica_api_key, paused_reason, paused_until, failed_launches, backoff_step, created_at, updated_at \
             from managed_models where model_id = $1",
        )
        .bind(model_id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(managed_model_from_row).transpose()
    }

    /// Just the health floor for a managed model. Used on the health-check hot
    /// path so it doesn't fetch + deserialize the full spec (script_body,
    /// launcher_spec JSON, ...) only to read one scalar.
    pub async fn get_managed_min_replicas(&self, model_id: Uuid) -> Result<Option<i64>> {
        let v = sqlx::query_scalar::<_, i64>(
            "select min_replicas from managed_models where model_id = $1",
        )
        .bind(model_id)
        .fetch_optional(&self.pool)
        .await?;
        Ok(v)
    }

    pub async fn list_managed_models(&self) -> Result<Vec<ManagedModelSpec>> {
        let rows = sqlx::query(
            "select model_id, enabled, partition, gres, nodes, constraints, exclude, account, \
             qos, time_limit, cpus_per_task, mem, image, preamble, log_output_dir, launch_command, \
             script_body, serving_port, \
             health_path, target_replicas, min_replicas, max_job_failures, launcher_spec, \
             last_provision_error, last_provision_error_at, startup_timeout_mins, handoff_mins, \
             replica_api_key, paused_reason, paused_until, failed_launches, backoff_step, created_at, updated_at \
             from managed_models order by model_id",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(managed_model_from_row).collect()
    }

    pub async fn upsert_managed_model(&self, m: UpsertManagedModel) -> Result<ManagedModelSpec> {
        let row = sqlx::query(
            "insert into managed_models
                (model_id, enabled, partition, gres, nodes, constraints, exclude,
                 account, qos, time_limit, cpus_per_task, mem, image, preamble, log_output_dir,
                 launch_command, script_body,
                 serving_port, health_path, target_replicas, min_replicas, max_job_failures, launcher_spec,
                 startup_timeout_mins, handoff_mins, replica_api_key)
             values ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,$23,$24,$25,$26)
             on conflict (model_id) do update set
                enabled = excluded.enabled, partition = excluded.partition,
                gres = excluded.gres, nodes = excluded.nodes,
                constraints = excluded.constraints, exclude = excluded.exclude,
                account = excluded.account, qos = excluded.qos,
                time_limit = excluded.time_limit, cpus_per_task = excluded.cpus_per_task,
                mem = excluded.mem, image = excluded.image,
                preamble = excluded.preamble, log_output_dir = excluded.log_output_dir,
                launch_command = excluded.launch_command, script_body = excluded.script_body,
                serving_port = excluded.serving_port, health_path = excluded.health_path,
                target_replicas = excluded.target_replicas,
                min_replicas = excluded.min_replicas,
                max_job_failures = excluded.max_job_failures,
                launcher_spec = excluded.launcher_spec,
                startup_timeout_mins = excluded.startup_timeout_mins,
                handoff_mins = excluded.handoff_mins,
                replica_api_key = excluded.replica_api_key, updated_at = now()
             returning model_id, enabled, partition, gres, nodes, constraints, exclude, account, \
             qos, time_limit, cpus_per_task, mem, image, preamble, log_output_dir, launch_command, \
             script_body, serving_port, \
             health_path, target_replicas, min_replicas, max_job_failures, launcher_spec, \
             last_provision_error, last_provision_error_at, startup_timeout_mins, handoff_mins, \
             replica_api_key, paused_reason, paused_until, failed_launches, backoff_step, created_at, updated_at",
        )
        .bind(m.model_id)
        .bind(m.enabled)
        .bind(&m.partition)
        .bind(&m.gres)
        .bind(m.nodes.max(1))
        .bind(&m.constraints)
        .bind(&m.exclude)
        .bind(&m.account)
        .bind(&m.qos)
        .bind(&m.time_limit)
        .bind(m.cpus_per_task)
        .bind(&m.mem)
        .bind(&m.image)
        .bind(&m.preamble)
        .bind(&m.log_output_dir)
        .bind(&m.launch_command)
        .bind(&m.script_body)
        .bind(m.serving_port)
        .bind(&m.health_path)
        // 0 keeps the spec but runs no replicas. The API rejects values out
        // of range with a message; these bounds only guard other callers.
        .bind(m.target_replicas.max(0))
        // The health floor cannot exceed the count the reconciler submits toward;
        // otherwise the model could never reach `min_replicas` healthy.
        .bind(m.min_replicas.clamp(0, m.target_replicas.max(0)))
        .bind(m.max_job_failures.max(0))
        .bind(m.launcher_spec.as_ref().map(sqlx::types::Json))
        .bind(m.startup_timeout_mins.max(0))
        .bind(m.handoff_mins.max(0))
        .bind(m.replica_api_key)
        .fetch_one(&self.pool)
        .await?;
        managed_model_from_row(&row)
    }

    /// Stop managing a model; `NotFound` when it has no launch spec.
    pub async fn delete_managed_model(&self, model_id: Uuid) -> Result<()> {
        let deleted = sqlx::query("delete from managed_models where model_id = $1")
            .bind(model_id)
            .execute(&self.pool)
            .await?
            .rows_affected();
        if deleted == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    /// Record (or clear) the provisioner's last submit error for a model. Sets
    /// the timestamp to now() when storing an error; both columns to NULL when
    /// clearing. No-op if the model has no managed_models row.
    pub async fn set_provision_error(&self, model_id: Uuid, error: Option<&str>) -> Result<()> {
        sqlx::query(
            "update managed_models \
             set last_provision_error = $2, \
                 last_provision_error_at = case when $2 is null then null else now() end, \
                 updated_at = now() \
             where model_id = $1",
        )
        .bind(model_id)
        .bind(error)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    // ---- recipes ---------------------------------------------------------

    pub async fn list_recipes(&self) -> Result<Vec<Recipe>> {
        let rows = sqlx::query(
            "select id, name, body, author, created_at, updated_at
             from recipes order by updated_at desc",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(recipe_from_row).collect()
    }

    pub async fn get_recipe(&self, id: Uuid) -> Result<Option<Recipe>> {
        let row = sqlx::query(
            "select id, name, body, author, created_at, updated_at
             from recipes where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(recipe_from_row).transpose()
    }

    pub async fn create_recipe(&self, r: NewRecipe) -> Result<Recipe> {
        let row = sqlx::query(
            "insert into recipes (id, name, body, author)
             values ($1,$2,$3,$4)
             returning id, name, body, author, created_at, updated_at",
        )
        .bind(Uuid::new_v4())
        .bind(&r.name)
        .bind(&r.body)
        .bind(&r.author)
        .fetch_one(&self.pool)
        .await?;
        recipe_from_row(&row)
    }

    /// `NotFound` for an id that is not there: an update never creates.
    pub async fn update_recipe(&self, id: Uuid, r: NewRecipe) -> Result<Recipe> {
        let row = sqlx::query(
            "update recipes set name=$2, body=$3, author=$4, updated_at=now()
             where id = $1
             returning id, name, body, author, created_at, updated_at",
        )
        .bind(id)
        .bind(&r.name)
        .bind(&r.body)
        .bind(&r.author)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        recipe_from_row(&row)
    }

    /// `NotFound` when there was nothing to delete.
    pub async fn delete_recipe(&self, id: Uuid) -> Result<()> {
        let done = sqlx::query("delete from recipes where id = $1")
            .bind(id)
            .execute(&self.pool)
            .await?;
        if done.rows_affected() == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }

    pub async fn create_replica(
        &self,
        model_id: Uuid,
        slurm_job_id: &str,
        port_base: Option<i64>,
    ) -> Result<ModelReplica> {
        self.create_replica_with_log(model_id, slurm_job_id, port_base, None, None)
            .await
    }

    /// [`create_replica`](Self::create_replica), also recording where the
    /// job's output goes and the API key its server was started with. The
    /// key is stored encrypted and read back only by
    /// [`replica_api_keys`](Self::replica_api_keys).
    pub async fn create_replica_with_log(
        &self,
        model_id: Uuid,
        slurm_job_id: &str,
        port_base: Option<i64>,
        log_path: Option<&str>,
        api_key: Option<&str>,
    ) -> Result<ModelReplica> {
        let api_key = cipher().encrypt_opt(api_key.map(str::trim).filter(|k| !k.is_empty()));
        // The launch row keeps the launcher panel state with environment
        // values and secret-looking inputs hidden: launch history outlives
        // the deployment.
        let launcher_spec: Option<serde_json::Value> =
            sqlx::query_scalar::<_, Option<sqlx::types::Json<serde_json::Value>>>(
                "select launcher_spec from managed_models where model_id = $1",
            )
            .bind(model_id)
            .fetch_optional(&self.pool)
            .await?
            .flatten()
            .map(|j| obleth_config::slurm::redact_launcher_spec(&j.0));
        let log_path = log_path.map(str::trim).filter(|p| !p.is_empty());
        // Idempotent: a retried insert for the same (model_id, slurm_job_id)
        // returns the existing row instead of erroring on the unique index.
        //
        // The same statement opens the replica's launch-history row (same id),
        // snapshotting the managed spec as submitted. A model without a
        // managed spec still gets a row, with the spec columns null. On a
        // retried insert the launch row already exists and is left alone.
        let row = sqlx::query(
            "with r as (
                insert into model_replicas (id, model_id, slurm_job_id, port_base, log_path, api_key)
                values ($1, $2, $3, $4, $6, $7)
                on conflict (model_id, slurm_job_id) do update set
                    log_path = coalesce(model_replicas.log_path, excluded.log_path),
                    api_key = coalesce(model_replicas.api_key, excluded.api_key),
                    updated_at = now()
                returning id, model_id, slurm_job_id, nodes, endpoint_id, state,
                          last_message, port_base, cancel_requested, replace_requested, job_started_at,
                    job_ends_at, state_detail, log_path, port, created_at, updated_at
             ), launch as (
                insert into deployment_launches
                    (id, model_id, model_name, recipe_id, slurm_job_id, partition, account,
                     qos, time_limit, gres, nodes_requested, cpus_per_task, mem, exclude,
                     constraints, launcher_spec, submitted_at)
                select r.id, r.model_id, m.model_name, mm.launcher_spec->>'recipe_id',
                       r.slurm_job_id, mm.partition, mm.account, mm.qos, mm.time_limit,
                       mm.gres, mm.nodes, mm.cpus_per_task, mm.mem, mm.exclude,
                       mm.constraints, $5::jsonb, r.created_at
                from r
                join models m on m.id = r.model_id
                left join managed_models mm on mm.model_id = r.model_id
                on conflict (id) do nothing
             )
             select * from r",
        )
        .bind(Uuid::new_v4())
        .bind(model_id)
        .bind(slurm_job_id)
        .bind(port_base)
        .bind(launcher_spec.map(sqlx::types::Json))
        .bind(log_path)
        .bind(api_key)
        .fetch_one(&self.pool)
        .await?;
        replica_from_row(&row)
    }

    /// The API key of every replica that was started with one, by replica
    /// id. For the provisioner, which probes and registers the replicas;
    /// replica rows themselves never carry the key.
    pub async fn replica_api_keys(&self) -> Result<HashMap<Uuid, String>> {
        let rows = sqlx::query("select id, api_key from model_replicas where api_key is not null")
            .fetch_all(&self.pool)
            .await?;
        let mut out = HashMap::with_capacity(rows.len());
        for row in &rows {
            if let Some(key) = cipher().decrypt_opt(row.try_get("api_key")?)? {
                out.insert(row.try_get("id")?, key);
            }
        }
        Ok(out)
    }

    /// Flag a replica for surge-first replacement: the provisioner submits a
    /// successor and drains this one once the successor serves. `false` when
    /// no replica with that id exists.
    pub async fn request_replica_replace(&self, id: Uuid) -> Result<bool> {
        let res = sqlx::query(
            "update model_replicas set replace_requested = true, updated_at = now() where id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Flag every live replica (not draining, lost, or already being
    /// replaced) of a model for surge-first replacement. Returns how many.
    pub async fn request_model_roll(&self, model_id: Uuid) -> Result<u64> {
        let res = sqlx::query(
            "update model_replicas set replace_requested = true, updated_at = now()
             where model_id = $1 and state in ('pending','starting','healthy')
               and not replace_requested and not cancel_requested",
        )
        .bind(model_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected())
    }

    /// Update the provisioner-reported details of a replica. `None` keeps a
    /// field; an empty `state_detail` or `log_path` clears it.
    pub async fn set_replica_details(
        &self,
        id: Uuid,
        d: &ReplicaDetailsUpdate,
    ) -> Result<ModelReplica> {
        let row = sqlx::query(
            "update model_replicas set
                 job_started_at = coalesce($2, job_started_at),
                 job_ends_at = coalesce($3, job_ends_at),
                 state_detail = case when $4::text is null then state_detail
                                     else nullif($4, '') end,
                 log_path = case when $5::text is null then log_path
                                 else nullif($5, '') end,
                 port = coalesce($6, port),
                 replace_requested = coalesce($7, replace_requested),
                 updated_at = now()
             where id = $1
             returning id, model_id, slurm_job_id, nodes, endpoint_id, state,
                       last_message, port_base, cancel_requested, replace_requested,
                       job_started_at, job_ends_at, state_detail, log_path, port,
                       created_at, updated_at",
        )
        .bind(id)
        .bind(d.job_started_at)
        .bind(d.job_ends_at)
        .bind(d.state_detail.as_deref())
        .bind(d.log_path.as_deref())
        .bind(d.port)
        .bind(d.replace_requested)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        replica_from_row(&row)
    }

    /// Record the provisioner's failure and pause state for a managed model.
    /// `paused_until = None` with `paused_reason = None` lifts a pause.
    /// No-op if the model has no managed_models row.
    pub async fn set_managed_backoff(&self, model_id: Uuid, b: &ManagedBackoff) -> Result<()> {
        sqlx::query(
            "update managed_models set
                 failed_launches = $2, backoff_step = $3,
                 paused_reason = $4, paused_until = $5, updated_at = now()
             where model_id = $1",
        )
        .bind(model_id)
        .bind(b.failed_launches.max(0))
        .bind(b.backoff_step.max(0))
        .bind(b.paused_reason.as_deref().filter(|r| !r.trim().is_empty()))
        .bind(b.paused_until)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// "Try again": lift a pause and reset the failure count and backoff so
    /// the next pass submits. `false` when the model has no managed spec.
    pub async fn clear_managed_backoff(&self, model_id: Uuid) -> Result<bool> {
        let res = sqlx::query(
            "update managed_models set
                 failed_launches = 0, backoff_step = 0, paused_reason = null,
                 paused_until = null, updated_at = now()
             where model_id = $1",
        )
        .bind(model_id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    pub async fn list_replicas(&self, model_id: Uuid) -> Result<Vec<ModelReplica>> {
        let rows = sqlx::query(
            "select id, model_id, slurm_job_id, nodes, endpoint_id, state,
                    last_message, port_base, cancel_requested, replace_requested, job_started_at,
                    job_ends_at, state_detail, log_path, port, created_at, updated_at
             from model_replicas where model_id = $1 order by created_at",
        )
        .bind(model_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(replica_from_row).collect()
    }

    pub async fn all_replicas(&self) -> Result<Vec<ModelReplica>> {
        let rows = sqlx::query(
            "select id, model_id, slurm_job_id, nodes, endpoint_id, state,
                    last_message, port_base, cancel_requested, replace_requested, job_started_at,
                    job_ends_at, state_detail, log_path, port, created_at, updated_at
             from model_replicas order by model_id, created_at",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(replica_from_row).collect()
    }

    pub async fn update_replica_state(
        &self,
        id: Uuid,
        state: &str,
        message: Option<&str>,
        end: LaunchEnd<'_>,
    ) -> Result<ModelReplica> {
        // The launch-history row follows in the same statement: timestamps are
        // stamped once (coalesce) and the first end state recorded wins, so a
        // replica that drains and is later seen gone keeps its cancel reason.
        let t = launch_transition(state, message, end);
        let row = sqlx::query(
            "with r as (
                update model_replicas set state = $2, last_message = $3, updated_at = now()
                where id = $1
                returning id, model_id, slurm_job_id, nodes, endpoint_id, state,
                          last_message, port_base, cancel_requested, replace_requested, job_started_at,
                    job_ends_at, state_detail, log_path, port, created_at, updated_at
             ), launch as (
                update deployment_launches l set
                    started_at = case when $4 then coalesce(l.started_at, now())
                                      else l.started_at end,
                    healthy_at = case when $5 then coalesce(l.healthy_at, now())
                                      else l.healthy_at end,
                    ended_at = case when $6 then coalesce(l.ended_at, now())
                                    else l.ended_at end,
                    end_state = coalesce(l.end_state, $7),
                    end_reason = case when l.end_state is null and $7 is not null
                                      then $8 else l.end_reason end,
                    updated_at = now()
                from r where l.id = r.id
             )
             select * from r",
        )
        .bind(id)
        .bind(state)
        .bind(message)
        .bind(t.started)
        .bind(t.healthy)
        .bind(t.ended)
        .bind(t.end_state)
        .bind(t.end_reason)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        replica_from_row(&row)
    }

    /// Set the allocated nodes and/or linked endpoint for a replica.
    ///
    /// A `None` argument keeps the current column value instead of overwriting
    /// it with NULL (COALESCE semantics). Pass `Some(value)` to update a field,
    /// `None` to leave it unchanged.
    pub async fn set_replica_runtime(
        &self,
        id: Uuid,
        nodes: Option<&str>,
        endpoint_id: Option<Uuid>,
    ) -> Result<ModelReplica> {
        // Nodes arriving means the job was allocated and started: the launch
        // row keeps the first allocation reported and stamps its start once.
        let row = sqlx::query(
            "with r as (
                update model_replicas set nodes = coalesce($2, nodes),
                       endpoint_id = coalesce($3, endpoint_id), updated_at = now()
                where id = $1
                returning id, model_id, slurm_job_id, nodes, endpoint_id, state,
                          last_message, port_base, cancel_requested, replace_requested, job_started_at,
                    job_ends_at, state_detail, log_path, port, created_at, updated_at
             ), launch as (
                update deployment_launches l set
                    nodes = coalesce(l.nodes, $2),
                    started_at = coalesce(l.started_at, now()),
                    updated_at = now()
                from r where l.id = r.id and $2::text is not null
             )
             select * from r",
        )
        .bind(id)
        .bind(nodes)
        .bind(endpoint_id)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        replica_from_row(&row)
    }

    /// Update just a replica's last_message (used by the provisioner's live-status
    /// annotation, which carries no state/runtime change). Returns the updated row.
    pub async fn set_replica_message(&self, id: Uuid, message: &str) -> Result<ModelReplica> {
        let row = sqlx::query(
            "update model_replicas set last_message = $2, updated_at = now()
             where id = $1
             returning id, model_id, slurm_job_id, nodes, endpoint_id, state,
                       last_message, port_base, cancel_requested, replace_requested, job_started_at,
                    job_ends_at, state_detail, log_path, port, created_at, updated_at",
        )
        .bind(id)
        .bind(message)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        replica_from_row(&row)
    }

    /// Flag a replica for restart: the provisioner cancels its Slurm job on the
    /// next tick and resubmit-to-target launches a fresh one. Returns `false`
    /// when no replica with that id exists, so the caller can surface a 404.
    pub async fn request_replica_cancel(&self, id: Uuid) -> Result<bool> {
        let res = sqlx::query(
            "update model_replicas set cancel_requested = true, updated_at = now() where id = $1",
        )
        .bind(id)
        .execute(&self.pool)
        .await?;
        Ok(res.rows_affected() > 0)
    }

    /// Fetch a single replica by id. Returns `None` if no row exists.
    pub async fn get_replica(&self, id: Uuid) -> Result<Option<ModelReplica>> {
        let row = sqlx::query(
            "select id, model_id, slurm_job_id, nodes, endpoint_id, state,
                    last_message, port_base, cancel_requested, replace_requested, job_started_at,
                    job_ends_at, state_detail, log_path, port, created_at, updated_at
             from model_replicas where id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await?;
        row.as_ref().map(replica_from_row).transpose()
    }

    /// Delete a replica row. Its launch-history row stays; if the launch was
    /// still open (the row was removed before it drained or was lost) it is
    /// closed now with end state `deleted`.
    /// `NotFound` when there was no such replica; its launch record, if any,
    /// is closed as deleted.
    pub async fn delete_replica(&self, id: Uuid) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("delete from model_replicas where id = $1 returning id")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or(StoreError::NotFound)?;
        sqlx::query(
            "update deployment_launches set
                 ended_at = coalesce(ended_at, now()),
                 end_state = coalesce(end_state, 'deleted'),
                 updated_at = now()
             where id = $1 and (ended_at is null or end_state is null)",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Launch history, newest first, optionally narrowed to one model and/or
    /// recipe. `limit` is clamped to 1..=500.
    pub async fn list_deployment_launches(
        &self,
        model_id: Option<Uuid>,
        recipe_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<DeploymentLaunch>> {
        let rows = sqlx::query(
            "select id, model_id, model_name, recipe_id, slurm_job_id, partition, account,
                    qos, time_limit, gres, nodes_requested, cpus_per_task, mem, exclude,
                    constraints, launcher_spec, nodes, submitted_at, started_at, healthy_at,
                    ended_at, end_state, end_reason, updated_at,
                    extract(epoch from (started_at - submitted_at))::bigint as queued_secs,
                    extract(epoch from (healthy_at - started_at))::bigint as load_secs,
                    extract(epoch from (coalesce(ended_at, now()) - started_at))::bigint
                        as ran_secs
             from deployment_launches
             where ($1::uuid is null or model_id = $1)
               and ($2::text is null or recipe_id = $2)
             order by submitted_at desc, id
             limit $3",
        )
        .bind(model_id)
        .bind(recipe_id)
        .bind(limit.clamp(1, 500))
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(launch_from_row).collect()
    }

    pub async fn delete_lost_replicas(&self, model_id: Uuid) -> Result<u64> {
        let r = sqlx::query("delete from model_replicas where model_id = $1 and state = 'lost'")
            .bind(model_id)
            .execute(&self.pool)
            .await?;
        Ok(r.rows_affected())
    }

    /// Record the outcome of a per-endpoint health check. Mirrors
    /// `record_model_health_check`: `healthy`/`disabled` reset the failure
    /// counter; `degraded`/`skipped`/`unknown` are transient (left untouched);
    /// anything else increments `consecutive_failures`.
    pub async fn record_endpoint_health(
        &self,
        id: Uuid,
        status: &str,
        latency_ms: Option<i64>,
        http_status: Option<i64>,
        message: Option<&str>,
    ) -> Result<ModelEndpoint> {
        let failure_delta: i64 = match status {
            "healthy" | "disabled" => 0,
            "degraded" | "skipped" | "unknown" => -1, // sentinel: leave counter as-is
            _ => 1,
        };
        // `skipped` is "we didn't really probe" -> never touch the stored status.
        // `degraded` is "reachable, but couldn't confirm the model id" -> it's a
        // real liveness signal, and the router treats degraded as routable. So a
        // degraded probe must lift a stale `unhealthy` (otherwise a steady-state
        // degraded endpoint — e.g. an id-format mismatch in /v1/models — can never
        // recover after one transient unhealthy read, and stays out of rotation
        // until a manual check). It still must not downgrade a confirmed `healthy`.
        let row = sqlx::query(&format!(
            "update model_endpoints set
                health_status = case
                    when $2 = 'skipped' then health_status
                    when $2 = 'degraded' then
                        case when health_status = 'unhealthy' then 'degraded' else health_status end
                    else $2 end,
                consecutive_failures = case
                    when $3 = 0 then 0
                    when $3 < 0 then consecutive_failures
                    else consecutive_failures + 1 end,
                last_checked_at = now(),
                last_latency_ms = $4,
                last_http_status = $5,
                last_message = $6,
                updated_at = now()
             where id = $1
             returning {ENDPOINT_COLUMNS}"
        ))
        .bind(id)
        .bind(status)
        .bind(failure_delta)
        .bind(latency_ms)
        .bind(http_status)
        .bind(message)
        .fetch_optional(&self.pool)
        .await?
        .ok_or(StoreError::NotFound)?;
        endpoint_from_row(&row)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{default_test_fields, serial, FixtureGuard};

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// A missing recipe is `NotFound` on update and delete, and `None` on get;
    /// an update never creates.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn a_missing_recipe_is_not_found_and_never_created_by_an_update() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let recipe = |name: &str| NewRecipe {
            name: name.into(),
            body: "spec".into(),
            author: "ana".into(),
        };
        let missing = Uuid::new_v4();
        assert!(matches!(
            store.update_recipe(missing, recipe("x")).await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store.delete_recipe(missing).await,
            Err(StoreError::NotFound)
        ));
        assert!(store.get_recipe(missing).await.expect("get").is_none());
        let made = store.create_recipe(recipe("first")).await.expect("create");
        let changed = store
            .update_recipe(made.id, recipe("second"))
            .await
            .expect("update");
        assert_eq!((changed.id, changed.name.as_str()), (made.id, "second"));
        store.delete_recipe(made.id).await.expect("delete");
        assert!(matches!(
            store.delete_recipe(made.id).await,
            Err(StoreError::NotFound)
        ));
        assert!(matches!(
            store.delete_replica(Uuid::new_v4()).await,
            Err(StoreError::NotFound)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn managed_model_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        // a model is required (FK). Reuse create_model with minimal args.
        let model_name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(&model_name, default_test_fields()))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        assert!(store
            .get_managed_model(model.id)
            .await
            .expect("get")
            .is_none());

        let spec = store
            .upsert_managed_model(UpsertManagedModel {
                model_id: model.id,
                enabled: true,
                partition: "gpu-preempt".into(),
                gres: "gpu:h100:2".into(),
                nodes: 1,
                constraints: None,
                exclude: None,
                account: None,
                qos: None,
                time_limit: Some("12:00:00".into()),
                cpus_per_task: None,
                mem: None,
                image: "vllm.sif".into(),
                preamble: String::new(),
                log_output_dir: String::new(),
                launch_command: "vllm serve nemotron".into(),
                script_body: String::new(),
                serving_port: 8000,
                health_path: "/health".into(),
                target_replicas: 2,
                min_replicas: 1,
                max_job_failures: 0,
                launcher_spec: Some(serde_json::json!({"backendId":"llamacpp"})),
                startup_timeout_mins: 60,
                handoff_mins: 15,
                replica_api_key: false,
            })
            .await
            .expect("upsert");
        assert_eq!(spec.target_replicas, 2);
        assert_eq!(spec.min_replicas, 1);
        assert_eq!(spec.partition, "gpu-preempt");
        assert_eq!(
            spec.launcher_spec,
            Some(serde_json::json!({"backendId":"llamacpp"}))
        );

        // Assert membership, not a global count — other tests share this DB and
        // may have their own managed models, so an exact count is flaky in CI.
        let listed = store.list_managed_models().await.expect("list");
        assert!(listed.iter().any(|m| m.model_id == model.id));

        store.delete_managed_model(model.id).await.expect("delete");
        assert!(store
            .get_managed_model(model.id)
            .await
            .expect("get")
            .is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn managed_model_provision_error_set_and_clear() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(&model_name, default_test_fields()))
            .await
            .expect("model");
        fixtures.track_model(model.id);
        store
            .upsert_managed_model(UpsertManagedModel {
                model_id: model.id,
                enabled: true,
                partition: "gpu-preempt".into(),
                gres: "gpu:h100:1".into(),
                nodes: 1,
                constraints: None,
                exclude: None,
                account: None,
                qos: None,
                time_limit: None,
                cpus_per_task: None,
                mem: None,
                image: "vllm.sif".into(),
                preamble: String::new(),
                log_output_dir: String::new(),
                launch_command: "vllm serve perr-model".into(),
                script_body: String::new(),
                serving_port: 8000,
                health_path: "/health".into(),
                target_replicas: 1,
                min_replicas: 1,
                max_job_failures: 0,
                launcher_spec: None,
                startup_timeout_mins: 60,
                handoff_mins: 15,
                replica_api_key: false,
            })
            .await
            .expect("managed");

        store
            .set_provision_error(model.id, Some("error 2045"))
            .await
            .expect("set");
        let m = store
            .get_managed_model(model.id)
            .await
            .expect("get")
            .expect("some");
        assert_eq!(m.last_provision_error.as_deref(), Some("error 2045"));
        assert!(m.last_provision_error_at.is_some());

        store
            .set_provision_error(model.id, None)
            .await
            .expect("clear");
        let m = store
            .get_managed_model(model.id)
            .await
            .expect("get")
            .expect("some");
        assert_eq!(m.last_provision_error, None);
        assert!(m.last_provision_error_at.is_none());

        // Clean up so the shared test DB doesn't accumulate managed models.
        store.delete_managed_model(model.id).await.expect("delete");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn managed_backoff_replace_and_details() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(&model_name, default_test_fields()))
            .await
            .expect("model");
        fixtures.track_model(model.id);
        store
            .upsert_managed_model(UpsertManagedModel {
                model_id: model.id,
                enabled: true,
                partition: "gpu-preempt".into(),
                gres: "gpu:h100:1".into(),
                nodes: 1,
                constraints: None,
                exclude: None,
                account: None,
                qos: None,
                time_limit: None,
                cpus_per_task: None,
                mem: None,
                image: "vllm.sif".into(),
                preamble: String::new(),
                log_output_dir: String::new(),
                launch_command: "vllm serve perr-model".into(),
                script_body: String::new(),
                serving_port: 8000,
                health_path: "/health".into(),
                target_replicas: 1,
                min_replicas: 1,
                max_job_failures: 0,
                launcher_spec: None,
                startup_timeout_mins: 60,
                handoff_mins: 15,
                replica_api_key: false,
            })
            .await
            .expect("managed");

        // Zero replicas is a valid target; the floor follows it down.
        let mut zero = UpsertManagedModel {
            model_id: model.id,
            enabled: true,
            partition: String::new(),
            gres: String::new(),
            nodes: 1,
            constraints: None,
            exclude: None,
            account: None,
            qos: None,
            time_limit: None,
            cpus_per_task: None,
            mem: None,
            image: String::new(),
            preamble: String::new(),
            log_output_dir: String::new(),
            launch_command: "serve".into(),
            script_body: String::new(),
            serving_port: 8000,
            health_path: "/health".into(),
            target_replicas: 0,
            min_replicas: 1,
            max_job_failures: 2,
            launcher_spec: Some(serde_json::json!({ "env": { "HF_TOKEN": "s3cret" } })),
            startup_timeout_mins: 30,
            handoff_mins: 0,
            replica_api_key: false,
        };
        let m = store
            .upsert_managed_model(zero.clone())
            .await
            .expect("zero");
        assert_eq!((m.target_replicas, m.min_replicas), (0, 0));
        assert_eq!((m.startup_timeout_mins, m.handoff_mins), (30, 0));
        zero.target_replicas = 1;
        store.upsert_managed_model(zero).await.expect("one");

        let until = chrono::Utc::now() + chrono::Duration::minutes(5);
        store
            .set_managed_backoff(
                model.id,
                &ManagedBackoff {
                    failed_launches: 2,
                    backoff_step: 1,
                    paused_reason: Some("Two launches in a row ended before serving.".into()),
                    paused_until: Some(until),
                },
            )
            .await
            .expect("pause");
        let m = store
            .get_managed_model(model.id)
            .await
            .expect("get")
            .expect("some");
        assert_eq!((m.failed_launches, m.backoff_step), (2, 1));
        assert!(m.paused_reason.is_some() && m.paused_until.is_some());
        assert!(store.clear_managed_backoff(model.id).await.expect("retry"));
        let m = store
            .get_managed_model(model.id)
            .await
            .expect("get")
            .expect("some");
        assert_eq!((m.failed_launches, m.backoff_step), (0, 0));
        assert!(m.paused_reason.is_none() && m.paused_until.is_none());
        assert!(!store
            .clear_managed_backoff(Uuid::new_v4())
            .await
            .expect("none"));

        let r = store
            .create_replica_with_log(
                model.id,
                "j-backoff",
                Some(8000),
                Some("/tmp/x-1.out"),
                None,
            )
            .await
            .expect("replica");
        assert_eq!(r.log_path.as_deref(), Some("/tmp/x-1.out"));
        assert!(!r.replace_requested);
        // A replica started with its own API key: the key is read back only
        // by `replica_api_keys`, and a retried insert keeps it.
        let keyed = store
            .create_replica_with_log(model.id, "j-keyed", Some(8010), None, Some(" k-one "))
            .await
            .expect("keyed replica");
        let again = store
            .create_replica_with_log(model.id, "j-keyed", Some(8010), None, None)
            .await
            .expect("retried insert");
        assert_eq!(again.id, keyed.id);
        let keys = store.replica_api_keys().await.expect("keys");
        assert_eq!(keys.get(&keyed.id).map(String::as_str), Some("k-one"));
        assert!(!keys.contains_key(&r.id), "no key unless started with one");
        assert!(!serde_json::to_string(&keyed).unwrap().contains("k-one"));
        store.delete_replica(keyed.id).await.expect("delete keyed");
        // The launch row hides environment values.
        let spec: serde_json::Value =
            sqlx::query_scalar::<_, sqlx::types::Json<serde_json::Value>>(
                "select launcher_spec from deployment_launches where id = $1",
            )
            .bind(r.id)
            .fetch_one(&store.pool)
            .await
            .expect("launch")
            .0;
        assert_eq!(spec["env"]["HF_TOKEN"], "(hidden)");

        let started = chrono::Utc::now();
        let r2 = store
            .set_replica_details(
                r.id,
                &ReplicaDetailsUpdate {
                    job_started_at: Some(started),
                    state_detail: Some("Waiting in the queue.".into()),
                    port: Some(8001),
                    ..Default::default()
                },
            )
            .await
            .expect("details");
        assert_eq!(r2.port, Some(8001));
        assert_eq!(r2.state_detail.as_deref(), Some("Waiting in the queue."));
        assert_eq!(
            r2.log_path.as_deref(),
            Some("/tmp/x-1.out"),
            "None keeps a field"
        );
        let r3 = store
            .set_replica_details(
                r.id,
                &ReplicaDetailsUpdate {
                    state_detail: Some(String::new()),
                    ..Default::default()
                },
            )
            .await
            .expect("clear detail");
        assert_eq!(r3.state_detail, None);
        assert_eq!(r3.port, Some(8001));

        assert!(store.request_replica_replace(r.id).await.expect("replace"));
        assert!(
            store
                .get_replica(r.id)
                .await
                .expect("get")
                .expect("row")
                .replace_requested
        );
        assert!(!store
            .request_replica_replace(Uuid::new_v4())
            .await
            .expect("none"));
        let other = store
            .create_replica(model.id, "j-backoff-2", Some(8008))
            .await
            .expect("replica 2");
        assert_eq!(store.request_model_roll(model.id).await.expect("roll"), 1);
        assert!(
            store
                .get_replica(other.id)
                .await
                .expect("get")
                .expect("row")
                .replace_requested
        );

        store.delete_replica(r.id).await.expect("delete r");
        store.delete_replica(other.id).await.expect("delete other");
        // Clean up so the shared test DB doesn't accumulate managed models.
        store.delete_managed_model(model.id).await.expect("delete");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn model_replica_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(&model_name, default_test_fields()))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        let r = store
            .create_replica(model.id, "job-123", None)
            .await
            .expect("create replica");
        fixtures.track_replica(r.id);
        assert_eq!(r.state, "pending");
        assert_eq!(r.slurm_job_id, "job-123");

        // Idempotent: re-creating the same (model_id, slurm_job_id) returns the
        // same row rather than spawning a duplicate.
        let again = store
            .create_replica(model.id, "job-123", None)
            .await
            .expect("create replica again");
        assert_eq!(again.id, r.id, "duplicate create must return the same row");
        assert_eq!(
            store.list_replicas(model.id).await.expect("list").len(),
            1,
            "no duplicate replica row"
        );

        let promoted = store
            .update_replica_state(r.id, "starting", Some("node alloc"), LaunchEnd::default())
            .await
            .expect("update state");
        assert_eq!(promoted.state, "starting");

        store
            .set_replica_runtime(r.id, Some("gpu-node-7"), None)
            .await
            .expect("set runtime");

        let mine = store.list_replicas(model.id).await.expect("list");
        assert_eq!(mine.len(), 1);
        assert_eq!(mine[0].nodes.as_deref(), Some("gpu-node-7"));

        let all = store.all_replicas().await.expect("all");
        assert!(all.iter().any(|x| x.id == r.id));

        // keep-semantics: patching only endpoint must NOT wipe nodes (and vice versa)
        store
            .set_replica_runtime(r.id, None, None)
            .await
            .expect("noop runtime");
        let after = store.list_replicas(model.id).await.expect("list2");
        assert_eq!(
            after[0].nodes.as_deref(),
            Some("gpu-node-7"),
            "nodes must be preserved"
        );

        // get_replica returns the row
        let got = store
            .get_replica(r.id)
            .await
            .expect("get_replica")
            .expect("present");
        assert_eq!(got.id, r.id);

        store.delete_replica(r.id).await.expect("delete");
        assert!(store
            .list_replicas(model.id)
            .await
            .expect("list")
            .is_empty());
    }

    #[tokio::test]
    async fn recipe_roundtrip() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");

        let saved = store
            .create_recipe(NewRecipe {
                name: "GLM".into(),
                body: "---\nname: GLM\n---\nllama-server".into(),
                author: "you".into(),
            })
            .await
            .expect("insert");
        assert!(store
            .list_recipes()
            .await
            .expect("list")
            .iter()
            .any(|r| r.id == saved.id));

        let updated = store
            .update_recipe(
                saved.id,
                NewRecipe {
                    name: "GLM v2".into(),
                    body: saved.body.clone(),
                    author: "you".into(),
                },
            )
            .await
            .expect("update");
        assert_eq!(updated.name, "GLM v2");

        store.delete_recipe(saved.id).await.expect("delete");
        assert!(!store
            .list_recipes()
            .await
            .expect("list2")
            .iter()
            .any(|r| r.id == saved.id));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn delete_lost_replicas_test() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(&model_name, default_test_fields()))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        // Insert two replicas.
        let r1 = store
            .create_replica(model.id, "job-loss-1", None)
            .await
            .expect("create replica 1");
        fixtures.track_replica(r1.id);
        let r2 = store
            .create_replica(model.id, "job-loss-2", None)
            .await
            .expect("create replica 2");
        fixtures.track_replica(r2.id);

        // Mark one as lost.
        store
            .update_replica_state(r1.id, "lost", Some("node gone"), LaunchEnd::default())
            .await
            .expect("mark lost");

        // delete_lost_replicas must remove only the lost one.
        let deleted = store
            .delete_lost_replicas(model.id)
            .await
            .expect("delete_lost_replicas");
        assert_eq!(deleted, 1, "exactly one lost replica deleted");

        let remaining = store.list_replicas(model.id).await.expect("list");
        assert_eq!(remaining.len(), 1, "one replica remains");
        assert_eq!(remaining[0].id, r2.id, "the non-lost replica survives");

        // Clean up.
        store.delete_replica(r2.id).await.expect("delete r2");
    }

    #[test]
    fn launch_transition_maps_states_and_messages() {
        assert_eq!(
            launch_transition("pending", None, LaunchEnd::default()),
            LaunchTransition::default()
        );
        let s = launch_transition("starting", Some("node alloc"), LaunchEnd::default());
        assert!(s.started && !s.healthy && !s.ended);
        let h = launch_transition("healthy", Some("promoted"), LaunchEnd::default());
        assert!(h.started && h.healthy && !h.ended && h.end_state.is_none());

        let lost = |m: Option<&str>| {
            let t = launch_transition("lost", m, LaunchEnd::default());
            assert!(t.ended);
            (t.end_state, t.end_reason)
        };
        assert_eq!(
            lost(Some("job ended: TIMEOUT (TimeLimit)")),
            (Some("TIMEOUT".into()), Some("TimeLimit".into()))
        );
        assert_eq!(
            lost(Some("job ended: NODE_FAIL")),
            (Some("NODE_FAIL".into()), None)
        );
        assert_eq!(lost(Some("job gone")), (Some("gone".into()), None));
        assert_eq!(
            lost(Some("node gone")),
            (Some("lost".into()), Some("node gone".into()))
        );
        assert_eq!(lost(None), (Some("lost".into()), None));

        let drained =
            |m: &str| launch_transition("draining", Some(m), LaunchEnd::default()).end_state;
        assert_eq!(
            drained("scaled down").as_deref(),
            Some("cancelled:scale-down")
        );
        assert_eq!(
            drained("restart requested").as_deref(),
            Some("cancelled:restart")
        );
        assert_eq!(
            drained("restarting: failed health probes while job running").as_deref(),
            Some("cancelled:probe-failed")
        );
        assert_eq!(drained("by hand").as_deref(), Some("cancelled"));
        assert_eq!(
            drained("cancelled: server did not become healthy in time").as_deref(),
            Some("cancelled:startup-timeout")
        );
        assert_eq!(
            drained("cancelled: job could not start").as_deref(),
            Some("cancelled:stuck")
        );

        // A named end wins over the message, for lost and draining alike, and
        // means nothing for a state that does not end the launch.
        let named = |state: &str, m: &str, end: &str, reason: Option<&str>| {
            let t = launch_transition(
                state,
                Some(m),
                LaunchEnd {
                    state: Some(end),
                    reason,
                },
            );
            (t.end_state, t.end_reason)
        };
        assert_eq!(
            named(
                "lost",
                "job ended: TIMEOUT (Time (limit))",
                "TIMEOUT",
                Some("Time (limit)")
            ),
            (Some("TIMEOUT".into()), Some("Time (limit)".into()))
        );
        assert_eq!(
            named("draining", "reworded", "cancelled:stuck", Some("reworded")),
            (Some("cancelled:stuck".into()), Some("reworded".into()))
        );
        assert_eq!(named("healthy", "promoted", "x", None), (None, None));
        assert_eq!(
            named("lost", "job gone", " ", None),
            (Some("gone".into()), None),
            "a blank end falls back to the message"
        );
        assert!(launch_transition("draining", Some("scaled down"), LaunchEnd::default()).ended);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn deployment_launch_history_test() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(&model_name, default_test_fields()))
            .await
            .expect("create model");
        fixtures.track_model(model.id);
        let recipe_id = format!("recipe-{}", Uuid::new_v4());
        store
            .upsert_managed_model(UpsertManagedModel {
                model_id: model.id,
                enabled: true,
                partition: "gpu".into(),
                gres: "gpu:4".into(),
                nodes: 2,
                constraints: Some("fast".into()),
                exclude: Some("node9".into()),
                account: Some("acct".into()),
                qos: Some("normal".into()),
                time_limit: Some("12:00:00".into()),
                cpus_per_task: Some(16),
                mem: Some("500G".into()),
                image: "vllm.sif".into(),
                preamble: String::new(),
                log_output_dir: String::new(),
                launch_command: "vllm serve x".into(),
                script_body: String::new(),
                serving_port: 8000,
                health_path: "/health".into(),
                target_replicas: 1,
                min_replicas: 1,
                max_job_failures: 0,
                launcher_spec: Some(serde_json::json!({"source":"recipe","recipe_id":recipe_id})),
                startup_timeout_mins: 60,
                handoff_mins: 15,
                replica_api_key: false,
            })
            .await
            .expect("upsert managed");

        // Submit: the launch row snapshots the spec.
        let r1 = store
            .create_replica(model.id, "job-launch-1", Some(8000))
            .await
            .expect("create r1");
        fixtures.track_replica(r1.id);
        // A retried create is idempotent for the launch row too.
        store
            .create_replica(model.id, "job-launch-1", Some(8000))
            .await
            .expect("create r1 again");
        let launches = store
            .list_deployment_launches(Some(model.id), None, 50)
            .await
            .expect("list");
        assert_eq!(launches.len(), 1);
        let l = &launches[0];
        assert_eq!(l.id, r1.id);
        assert_eq!(l.model_name, model_name);
        assert_eq!(l.recipe_id.as_deref(), Some(recipe_id.as_str()));
        assert_eq!(l.slurm_job_id, "job-launch-1");
        assert_eq!(l.partition.as_deref(), Some("gpu"));
        assert_eq!(l.account.as_deref(), Some("acct"));
        assert_eq!(l.qos.as_deref(), Some("normal"));
        assert_eq!(l.gres.as_deref(), Some("gpu:4"));
        assert_eq!(l.nodes_requested, Some(2));
        assert_eq!(l.cpus_per_task, Some(16));
        assert_eq!(l.mem.as_deref(), Some("500G"));
        assert_eq!(l.exclude.as_deref(), Some("node9"));
        assert_eq!(l.constraints.as_deref(), Some("fast"));
        assert!(l.started_at.is_none() && l.queued_secs.is_none() && l.ran_secs.is_none());

        // Nodes arrive (job started), then healthy, then Slurm ends it.
        store
            .set_replica_runtime(r1.id, Some("node1,node2"), None)
            .await
            .expect("runtime");
        store
            .set_replica_runtime(r1.id, Some("node1"), None)
            .await
            .expect("runtime again");
        store
            .update_replica_state(r1.id, "healthy", Some("promoted"), LaunchEnd::default())
            .await
            .expect("healthy");
        store
            .update_replica_state(
                r1.id,
                "lost",
                Some("job ended: TIMEOUT (TimeLimit)"),
                LaunchEnd::default(),
            )
            .await
            .expect("lost");
        // Later messages must not overwrite the first recorded end.
        store
            .update_replica_state(r1.id, "lost", Some("job gone"), LaunchEnd::default())
            .await
            .expect("lost again");

        // A second launch, cancelled for a startup timeout, then GC'd. The
        // end comes as fields; the message is not read for it.
        let r2 = store
            .create_replica(model.id, "job-launch-2", Some(8008))
            .await
            .expect("create r2");
        fixtures.track_replica(r2.id);
        store
            .update_replica_state(
                r2.id,
                "draining",
                Some("worded any way at all"),
                LaunchEnd {
                    state: Some("cancelled:startup-timeout"),
                    reason: Some("not healthy within 60 minutes"),
                },
            )
            .await
            .expect("drain");
        store.delete_replica(r2.id).await.expect("delete r2");
        // And the lost one is cleared.
        store
            .delete_lost_replicas(model.id)
            .await
            .expect("clear lost");

        // Both launches survive their replica rows, newest first.
        let launches = store
            .list_deployment_launches(Some(model.id), None, 50)
            .await
            .expect("list");
        assert_eq!(launches.len(), 2);
        let (second, first) = (&launches[0], &launches[1]);
        assert_eq!(second.id, r2.id);
        assert_eq!(
            second.end_state.as_deref(),
            Some("cancelled:startup-timeout")
        );
        assert_eq!(
            second.end_reason.as_deref(),
            Some("not healthy within 60 minutes")
        );
        assert!(second.ended_at.is_some());
        assert!(second.started_at.is_none() && second.ran_secs.is_none());

        assert_eq!(first.id, r1.id);
        assert_eq!(first.nodes.as_deref(), Some("node1,node2"));
        assert!(first.started_at.is_some() && first.healthy_at.is_some());
        assert!(first.ended_at.is_some());
        assert_eq!(first.end_state.as_deref(), Some("TIMEOUT"));
        assert_eq!(first.end_reason.as_deref(), Some("TimeLimit"));
        assert!(first.queued_secs.is_some_and(|s| s >= 0));
        assert!(first.load_secs.is_some_and(|s| s >= 0));
        assert!(first.ran_secs.is_some_and(|s| s >= 0));

        // Filters: by recipe, limit, and a recipe nobody launched.
        let by_recipe = store
            .list_deployment_launches(None, Some(&recipe_id), 50)
            .await
            .expect("by recipe");
        assert_eq!(by_recipe.len(), 2);
        let limited = store
            .list_deployment_launches(Some(model.id), None, 1)
            .await
            .expect("limited");
        assert_eq!(limited.len(), 1);
        assert_eq!(limited[0].id, r2.id);
        assert!(store
            .list_deployment_launches(None, Some("no-such-recipe-xyz"), 50)
            .await
            .expect("none")
            .is_empty());

        store
            .delete_managed_model(model.id)
            .await
            .expect("delete managed");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn set_replica_message_test() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        let model_name = format!("m-{}", Uuid::new_v4());
        let model = store
            .create_model(&NewModel::new(&model_name, default_test_fields()))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        let replica = store
            .create_replica(model.id, "job-msg-1", None)
            .await
            .expect("create replica");
        fixtures.track_replica(replica.id);

        // A message-only update must persist without touching state.
        let updated = store
            .set_replica_message(replica.id, "provisioning: waiting for allocation")
            .await
            .expect("set_replica_message");
        assert_eq!(
            updated.last_message.as_deref(),
            Some("provisioning: waiting for allocation"),
            "last_message must be persisted"
        );
        assert_eq!(updated.state, replica.state, "state must be unchanged");

        // Clean up.
        store
            .delete_replica(replica.id)
            .await
            .expect("delete replica");
    }

    /// Integration test; runs only when `OBLETH_TEST_DATABASE_URL` is set.
    /// Deleting a model must NOT delete its replica rows — the provisioner needs
    /// them to drain the Slurm jobs they represent.
    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn deleting_model_leaves_replica_rows() {
        let Some(url) = crate::test_support::test_db_url() else {
            eprintln!("skipping: set OBLETH_TEST_DATABASE_URL to run");
            return;
        };
        let _g = serial().lock().await;
        let store = Store::connect(&url).await.expect("connect");
        store.migrate().await.expect("migrate");
        let mut fixtures = FixtureGuard::new(&store);

        // Same positional create_model call as `tenant_key_audit_roundtrip`.
        let model = store
            .create_model(&NewModel::new(
                format!("m-{}", Uuid::new_v4()),
                ModelFields {
                    description: "replica-survival test model".into(),
                    upstream_model: "upstream-model".into(),
                    api_base: "http://127.0.0.1:8081".into(),
                    model_type: "chat".into(),
                    context_window: 8192,
                    admission_weight: 100,
                    supports_system_messages: true,
                    route_bias: 1.0,
                    ..default_test_fields()
                },
            ))
            .await
            .expect("create model");
        fixtures.track_model(model.id);

        let replica = store
            .create_replica(model.id, "slurm-job-12345", Some(8000))
            .await
            .expect("create replica");
        fixtures.track_replica(replica.id);

        // Hard-delete the model. Before the cascade was dropped this also deleted
        // the replica row; now the row must survive.
        store.delete_model(model.id).await.expect("delete model");

        // The model is gone...
        assert!(
            matches!(store.get_model(model.id).await, Err(StoreError::NotFound)),
            "model row should be deleted"
        );
        // ...but its replica row survives for the provisioner to drain.
        let replicas = store.list_replicas(model.id).await.expect("list replicas");
        assert!(
            replicas.iter().any(|r| r.id == replica.id),
            "replica row must outlive the model delete"
        );

        // Clean up the now-orphaned replica row, matching the teardown discipline
        // of the other replica integration tests.
        store.delete_replica(replica.id).await.ok();
    }
}
