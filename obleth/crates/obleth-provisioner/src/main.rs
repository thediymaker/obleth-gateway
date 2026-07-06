mod config;
mod executor;
mod obleth_client;
mod plan;
mod probe;
mod warmup;

// `domain` and `slurm` live in the crate's library half (lib.rs) so obleth-admin
// can call `discover_resources()`. Re-export them here rather than re-declaring
// the modules, so they compile once — re-declaring with `mod` would compile the
// discovery code into the binary too, where it is unused (dead-code warnings).
pub(crate) use obleth_provisioner::{domain, slurm};

use config::ProvisionerConfig;
use obleth_client::{HttpObleth, OblethClient, TickReport};
use obleth_provisioner::slurm::{SlurmClient, Slurmrestd};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

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
    // Self-heal state: consecutive failed health probes per healthy replica.
    // In-memory on purpose — the provisioner is a singleton, and a restart
    // merely resets the streaks (worst case: remediation is delayed by
    // `restart_after_failures` ticks). No schema for a transient counter.
    let mut probe_failures: HashMap<uuid::Uuid, i64> = HashMap::new();
    loop {
        match run_once(&cfg, &obleth, &http, &mut probe_failures).await {
            Ok(Tick::Ran) => {
                obleth.set_last_tick(TickReport::ok());
                if last_idle.take().is_some() {
                    tracing::info!("slurm active; reconciling managed models");
                }
            }
            Ok(Tick::Idle(reason)) => {
                obleth.set_last_tick(TickReport::idle(reason));
                if last_idle != Some(reason) {
                    tracing::info!(reason, "provisioner idle (no slurm work)");
                    last_idle = Some(reason);
                }
            }
            Err(e) => {
                // Reported to the gateway on the next settings fetch so the
                // dashboard can show "reconcile failing since X" instead of a
                // deceptively green heartbeat while every tick holds.
                obleth.set_last_tick(TickReport::error(&format!("{e:#}")));
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
    probe_failures: &mut HashMap<uuid::Uuid, i64>,
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
    tick(cfg, &slurm, obleth, http, probe_failures).await?;
    Ok(Tick::Ran)
}

async fn tick(
    cfg: &ProvisionerConfig,
    slurm: &dyn SlurmClient,
    obleth: &dyn OblethClient,
    http: &reqwest::Client,
    probe_failures: &mut HashMap<uuid::Uuid, i64>,
) -> anyhow::Result<()> {
    let specs = obleth.list_managed_models().await?; // obleth down -> bail (held). enabled only.
    let all_replicas = obleth.list_all_replicas().await?; // obleth down -> bail (held)

    // Drop self-heal streaks for replicas that no longer exist or are no longer
    // healthy (restarted, lost, draining) so the map can't grow unbounded.
    probe_failures.retain(|id, _| {
        all_replicas
            .iter()
            .any(|r| r.id == *id && r.state == "healthy")
    });

    // Look up Slurm state for just the jobs we track, by id — never the whole
    // controller (which on a busy cluster is huge and OOM-kills us). A clean
    // "not found" leaves a job out of the map, which the planner reads as gone;
    // a transport/HTTP error bails the whole tick so an unreachable Slurm is
    // never mistaken for a fleet of dead jobs (no destructive action while held).
    let mut jobs: HashMap<String, domain::JobInfo> = HashMap::new();
    for id in all_replicas.iter().map(|r| &r.slurm_job_id) {
        if id.is_empty() || jobs.contains_key(id) {
            continue;
        }
        match slurm.get_job(id).await {
            Ok(Some(info)) => {
                jobs.insert(id.clone(), info);
            }
            Ok(None) => {} // gone/purged -> absent from map -> planner reconciles it away
            Err(e) => return Err(e.context("slurm job lookup failed; holding tick")),
        }
    }

    // Annotate each replica with its live Slurm status so the dashboard shows why
    // a job is pending or what state it is in. Only non-terminal jobs (terminal
    // ones get the planner's MarkLost message) and only when the message changed,
    // so we don't write every tick.
    for r in &all_replicas {
        if let Some(job) = jobs.get(&r.slurm_job_id) {
            if matches!(
                job.state,
                domain::JobState::Pending | domain::JobState::Running
            ) {
                let msg = slurm::job_status_message(&job.raw_state, job.reason.as_deref());
                if r.last_message.as_deref() != Some(msg.as_str()) {
                    if let Err(e) = obleth
                        .patch_replica(r.id, None, None, None, Some(&msg))
                        .await
                    {
                        tracing::warn!(replica_id = %r.id, error = %e, "failed to annotate replica status");
                    }
                }
            }
        }
    }

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

        // Probe every replica with a running job. "pending" means the job was
        // submitted but we haven't seen it Running yet; once Slurm transitions
        // the job to Running, the replica is still "pending" (there is no
        // separate MarkStarting step), so both pre-promotion states are probed —
        // and healthy ones are re-probed for self-heal (see below).
        let mut health: HashMap<uuid::Uuid, u16> = HashMap::new();
        for r in &replicas {
            // Probe replicas awaiting promotion, stranded "healthy" rows with no
            // endpoint linked (a prior promote whose endpoint write failed, so
            // the planner can re-promote and re-link), AND every promoted
            // healthy replica — the last so self-heal can spot a zombie job
            // (Slurm still says RUNNING, but the inference server inside it is
            // dead) and restart it instead of leaving the model unhealthy
            // forever.
            if r.state == "starting" || r.state == "pending" || r.state == "healthy" {
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
                                // Probe the whole window concurrently rather than
                                // sequentially: a not-yet-up replica otherwise costs
                                // port_span * health_timeout of serial waits every
                                // tick (8 * 5s = 40s with the defaults), which can
                                // exceed the tick interval and stall the loop.
                                let mut set = tokio::task::JoinSet::new();
                                for p in r.port_base..(r.port_base + cfg.port_span) {
                                    // Skip ports outside the valid TCP range so a high
                                    // serving_port + window can't wrap on the u16 cast.
                                    if p <= 0 || p > u16::MAX as i64 {
                                        continue;
                                    }
                                    let http = http.clone();
                                    let node = node.clone();
                                    let health_path = spec.health_path.clone();
                                    let timeout = cfg.health_timeout_secs;
                                    set.spawn(async move {
                                        let api_base = format!("http://{node}:{p}");
                                        (
                                            p,
                                            probe::is_healthy(
                                                &http,
                                                &api_base,
                                                &health_path,
                                                timeout,
                                            )
                                            .await,
                                        )
                                    });
                                }
                                // The bound port is whichever responds healthy; pick
                                // the lowest for a stable, deterministic choice.
                                let mut found: Option<u16> = None;
                                while let Some(res) = set.join_next().await {
                                    if let Ok((p, true)) = res {
                                        let p = p as u16;
                                        found = Some(found.map_or(p, |cur| cur.min(p)));
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

        // Self-heal bookkeeping: judge each healthy replica's probe outcome,
        // pull the gateway's endpoint-health verdicts (its check is a real
        // 1-token inference — it catches zombies whose metadata GET still
        // answers), and collect restart candidates. Endpoint fetch is
        // best-effort: without it the GET-probe signal still works.
        plan::update_probe_failures(&replicas, &jobs, &health, probe_failures);
        let endpoints = match obleth.list_endpoints(spec.model_id).await {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(model_id = %spec.model_id, error = %e,
                    "endpoint health lookup failed; self-heal using probe signal only");
                Vec::new()
            }
        };
        let restart = plan::restart_candidates(
            &replicas,
            probe_failures,
            cfg.restart_after_failures,
            &endpoints,
        );

        let mut live_port_bases: Vec<i64> = replicas
            .iter()
            .filter(|r| r.state != "lost" && r.state != "draining")
            .map(|r| r.port_base)
            .collect();

        let view = plan::ManagedSpecView {
            target_replicas: spec.target_replicas,
            max_job_failures: spec.max_job_failures,
        };
        let actions = plan::plan(
            &view,
            &replicas,
            &jobs,
            &health,
            &restart,
            cfg.lost_retention_secs,
        );
        for action in &actions {
            // Reserve a distinct window per Submit *before* applying, so several
            // Submits in one tick don't all collapse onto the same port_base
            // (live_port_bases is recomputed per tick, not per action otherwise).
            let port_base = if matches!(action, domain::Action::Submit) {
                let b =
                    plan::next_free_window_base(spec.serving_port, cfg.port_span, &live_port_bases);
                live_port_bases.push(b);
                b
            } else {
                0
            };
            match executor::apply(
                action,
                spec.model_id,
                &model_name,
                Some(spec),
                &cfg.job_name_prefix,
                cfg.port_span,
                port_base,
                slurm,
                obleth,
            )
            .await
            {
                Err(e) => tracing::warn!(?action, error = %e, "action failed; continuing"),
                // A just-promoted replica is healthy by /health but may still be
                // cold for its first forward pass. Fire a throwaway warmup
                // inference, detached, so the slow cold first token is paid here
                // instead of by the first real user — and never stalls the tick.
                Ok(()) => {
                    if cfg.warmup_timeout_secs > 0 {
                        if let domain::Action::Promote { api_base, .. } = action {
                            let http = http.clone();
                            let api_base = api_base.clone();
                            let model_name = model_name.clone();
                            let budget = Duration::from_secs(cfg.warmup_timeout_secs);
                            tokio::spawn(async move {
                                match warmup::warm_up(&http, &api_base, budget).await {
                                    Ok(()) => tracing::info!(
                                        model = %model_name, %api_base,
                                        "warmup inference completed"
                                    ),
                                    Err(e) => tracing::warn!(
                                        model = %model_name, %api_base, error = %e,
                                        "warmup inference failed (best-effort)"
                                    ),
                                }
                            });
                        }
                    }
                }
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
        let view = plan::ManagedSpecView {
            target_replicas: 0,
            max_job_failures: 0,
        };
        let actions = plan::plan(
            &view,
            &replicas,
            &jobs,
            &HashMap::new(),
            &HashSet::new(),
            cfg.lost_retention_secs,
        );
        for action in &actions {
            // Drain reconciles toward target 0, so no Submit fires; port_base is unused.
            if let Err(e) = executor::apply(
                action,
                model_id,
                &model_name,
                None,
                &cfg.job_name_prefix,
                cfg.port_span,
                0,
                slurm,
                obleth,
            )
            .await
            {
                tracing::warn!(?action, error = %e, "drain action failed; continuing");
            }
        }
    }

    // Orphan jobs (submitted but never recorded) are prevented at the source:
    // the Submit executor cancels a job if recording its replica fails. So there
    // is no periodic cluster-wide scan here — we never list the whole controller.
    Ok(())
}
