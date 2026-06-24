mod config; mod domain; mod plan; mod slurm; mod obleth_client; mod probe; mod executor;

use std::collections::{HashMap, HashSet};
use std::time::Duration;
use config::ProvisionerConfig;
use obleth_client::{HttpObleth, OblethClient};
use slurm::{SlurmClient, Slurmrestd};

/// Outcome of one loop iteration, so the loop can log idle/active transitions
/// without spamming a line every tick.
enum Tick {
    Ran,
    Idle(&'static str),
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cfg = ProvisionerConfig::from_env()?;
    let http = reqwest::Client::new();
    let obleth = HttpObleth::new(&cfg, http.clone());
    tracing::info!(interval = cfg.interval_secs, "obleth-provisioner started");

    // Track the last idle reason so we only log on transitions.
    let mut last_idle: Option<&'static str> = None;
    loop {
        match run_once(&cfg, &obleth, &http).await {
            Ok(Tick::Ran) => {
                if last_idle.take().is_some() {
                    tracing::info!("slurm active; reconciling managed models");
                }
            }
            Ok(Tick::Idle(reason)) => {
                if last_idle != Some(reason) {
                    tracing::info!(reason, "provisioner idle (no slurm work)");
                    last_idle = Some(reason);
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "tick failed; holding (no destructive action)");
            }
        }
        tokio::time::sleep(Duration::from_secs(cfg.interval_secs)).await;
    }
}

/// Fetch the system-wide Slurm settings and, when Slurm is enabled and
/// reachable, build a client from them and run one reconcile tick. Connection
/// details are read fresh each tick so settings changes take effect without a
/// restart.
async fn run_once(
    cfg: &ProvisionerConfig,
    obleth: &dyn OblethClient,
    http: &reqwest::Client,
) -> anyhow::Result<Tick> {
    let settings = match obleth.get_slurm_settings().await? {
        Some(s) => s,
        None => return Ok(Tick::Idle("slurm not configured")),
    };
    if !settings.enabled {
        return Ok(Tick::Idle("slurm disabled in settings"));
    }
    if settings.slurmrestd_url.trim().is_empty() {
        return Ok(Tick::Idle("slurm enabled but slurmrestd_url is empty"));
    }
    let slurm = Slurmrestd::new(
        http.clone(),
        &settings.slurmrestd_url,
        &settings.slurmrestd_api_version,
        &settings.slurm_user,
        &settings.slurm_jwt,
    );
    tick(cfg, &slurm, obleth, http).await?;
    Ok(Tick::Ran)
}

async fn tick(
    cfg: &ProvisionerConfig,
    slurm: &dyn SlurmClient,
    obleth: &dyn OblethClient,
    http: &reqwest::Client,
) -> anyhow::Result<()> {
    let specs = obleth.list_managed_models().await?;          // obleth down -> bail (held). enabled only.
    let owned = slurm.list_owned_jobs(&cfg.job_name_prefix).await?; // slurm down -> bail (held)
    let all_replicas = obleth.list_all_replicas().await?;
    let jobs: HashMap<String, domain::JobInfo> =
        owned.iter().map(|j| (j.job_id.clone(), j.clone())).collect();

    // Group every known replica by model so we can both reconcile the enabled
    // models and drain whatever rows belong to models that have left the set.
    let mut by_model: HashMap<uuid::Uuid, Vec<domain::ReplicaView>> = HashMap::new();
    for r in all_replicas.iter().cloned() {
        by_model.entry(r.model_id).or_default().push(r);
    }

    // 1. Reconcile each enabled managed model toward its target.
    for spec in &specs {
        // Claim this model's replicas so the drain pass below ignores them, even
        // if we end up skipping this model for a transient reason.
        let replicas = by_model.remove(&spec.model_id).unwrap_or_default();
        let model_name = match obleth.model_name(spec.model_id).await {
            Ok(n) if !n.is_empty() => n,
            Ok(_) => {
                tracing::warn!(model_id = %spec.model_id, "model has empty name; skipping this tick");
                continue;
            }
            Err(e) => {
                tracing::warn!(model_id = %spec.model_id, error = %e, "model_name lookup failed; skipping this tick");
                continue;
            }
        };

        // probe starting and pending replicas that have a running job.
        // "pending" means the job was submitted but we haven't seen it Running yet;
        // once Slurm transitions the job to Running, the replica is still "pending"
        // (there is no separate MarkStarting step), so we must probe both states.
        let mut health: HashMap<uuid::Uuid, u16> = HashMap::new();
        for r in &replicas {
            if r.state == "starting" || r.state == "pending" {
                if let Some(j) = jobs.get(&r.slurm_job_id) {
                    if j.state == domain::JobState::Running {
                        match j.nodes.first() {
                            None => {
                                tracing::warn!(
                                    replica_id = %r.id,
                                    job_id = %r.slurm_job_id,
                                    "job is RUNNING but slurmrestd returned no nodes; \
                                     cannot probe health — check slurmrestd response"
                                );
                            }
                            Some(node) => {
                                let mut found: Option<u16> = None;
                                for p in r.port_base..(r.port_base + cfg.port_span) {
                                    let api_base = format!("http://{node}:{p}");
                                    if probe::is_healthy(http, &api_base, &spec.health_path, cfg.health_timeout_secs).await {
                                        found = Some(p as u16);
                                        break;
                                    }
                                }
                                if let Some(p) = found {
                                    tracing::info!(
                                        replica_id = %r.id,
                                        job_id = %r.slurm_job_id,
                                        port = p,
                                        "health probe: healthy"
                                    );
                                    health.insert(r.id, p);
                                } else {
                                    tracing::info!(
                                        replica_id = %r.id,
                                        job_id = %r.slurm_job_id,
                                        port_base = r.port_base,
                                        port_span = cfg.port_span,
                                        "health probe: not yet healthy"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }

        let live_port_bases: Vec<i64> = replicas.iter()
            .filter(|r| r.state != "lost" && r.state != "draining")
            .map(|r| r.port_base)
            .collect();

        let view = plan::ManagedSpecView {
            target_replicas: spec.target_replicas,
            max_job_failures: spec.max_job_failures,
        };
        let actions = plan::plan(&view, &replicas, &jobs, &health, cfg.lost_retention_secs);
        for action in &actions {
            if let Err(e) = executor::apply(action, spec.model_id, &model_name, Some(spec), &cfg.job_name_prefix, cfg.port_span, &live_port_bases, slurm, obleth).await {
                tracing::warn!(?action, error = %e, "action failed; continuing");
            }
        }
    }

    // 2. Drain models that still have replicas but are no longer in the enabled
    //    managed set (disabled, deleted, or never-managed). Reconcile them toward
    //    target 0: cancel live jobs, mark gone ones lost, GC old lost rows. No
    //    Submit/Promote fires at target 0, so the missing spec is fine.
    for (model_id, replicas) in by_model {
        let model_name = obleth.model_name(model_id).await.unwrap_or_default();
        tracing::info!(%model_id, replicas = replicas.len(), "draining replicas for unmanaged model");
        let view = plan::ManagedSpecView { target_replicas: 0, max_job_failures: 0 };
        let actions = plan::plan(&view, &replicas, &jobs, &HashMap::new(), cfg.lost_retention_secs);
        for action in &actions {
            if let Err(e) = executor::apply(action, model_id, &model_name, None, &cfg.job_name_prefix, cfg.port_span, &[], slurm, obleth).await {
                tracing::warn!(?action, error = %e, "drain action failed; continuing");
            }
        }
    }

    // 3. Cancel orphan jobs: ours by name, but with no replica row tracking them.
    //    This happens when a prior tick submitted a job but failed to record the
    //    replica row, so the planner never sees the job and would otherwise leak
    //    it (a live GPU allocation) until its time limit.
    let known: HashSet<&str> = all_replicas.iter().map(|r| r.slurm_job_id.as_str()).collect();
    for j in &owned {
        if !known.contains(j.job_id.as_str()) {
            match slurm.cancel(&j.job_id).await {
                Ok(()) => tracing::warn!(job_id = %j.job_id, "cancelled orphan slurm job (no replica row)"),
                Err(e) => tracing::warn!(job_id = %j.job_id, error = %e, "failed to cancel orphan job; continuing"),
            }
        }
    }
    Ok(())
}
