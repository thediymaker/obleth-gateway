mod config;
mod executor;
mod obleth_client;
mod plan;
mod probe;
mod resolve;
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

/// How long a resolved node address is cached before re-resolving. Node IPs are
/// stable for a job's lifetime; a few minutes catches the rare case of a node
/// rebooting onto a new address without hammering DNS.
const RESOLVE_TTL_SECS: u64 = 300;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();
    let cfg = ProvisionerConfig::from_env()?;
    // slurmrestd and node probes carry the Slurm JWT; never let a redirect carry
    // it to an unvalidated destination.
    // Bounded by default so one hung admin/slurmrestd call can't stall the
    // singleton's loop forever. Probe and warmup set their own per-request
    // timeouts, which override these.
    let http = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(5))
        .build()?;
    let obleth = HttpObleth::new(&cfg, http.clone());
    tracing::info!(interval = cfg.interval_secs, "obleth-provisioner started");

    // Track the last idle reason so we only log on transitions.
    let mut last_idle: Option<&'static str> = None;
    // Self-heal state: consecutive failed health probes per healthy replica.
    // In-memory on purpose — the provisioner is a singleton, and a restart
    // merely resets the streaks (worst case: remediation is delayed by
    // `restart_after_failures` ticks). No schema for a transient counter.
    let mut probe_failures: HashMap<uuid::Uuid, i64> = HashMap::new();
    // Long-lived so its success cache survives across ticks: once a node name is
    // resolved (or aliased) the provisioner stops touching DNS for it.
    let resolver = resolve::HostResolver::new(Duration::from_secs(RESOLVE_TTL_SECS));
    loop {
        match run_once(&cfg, &obleth, &http, &resolver, &mut probe_failures).await {
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
    resolver: &resolve::HostResolver,
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
    // Refresh operator hostname→IP overrides from settings each tick, so edits in
    // the dashboard take effect without restarting the provisioner.
    resolver.set_aliases(settings.node_alias_map());
    let slurm = Slurmrestd::new(
        http.clone(),
        &settings.slurmrestd_url,
        &settings.slurmrestd_api_version,
        &settings.slurm_user,
        &settings.slurm_jwt,
    );
    tick(cfg, &slurm, obleth, http, resolver, probe_failures).await?;
    Ok(Tick::Ran)
}

/// Breaker for the job lookups: `Some(n)` when there are at least two distinct
/// jobs behind live replicas and *none* of them came back. A whole fleet going
/// missing in one tick is far likelier a wrong cluster/URL/API version than
/// real deaths, and each miss would otherwise become a MarkLost + resubmit.
/// Only replicas the planner would mark lost count: `lost`/`draining` rows are
/// expected to point at purged jobs (and must stay GC-able).
fn all_live_jobs_vanished(
    replicas: &[domain::ReplicaView],
    jobs: &HashMap<String, domain::JobInfo>,
) -> Option<usize> {
    let live: HashSet<&str> = replicas
        .iter()
        .filter(|r| r.state != "lost" && r.state != "draining" && !r.slurm_job_id.is_empty())
        .map(|r| r.slurm_job_id.as_str())
        .collect();
    (live.len() >= 2 && live.iter().all(|id| !jobs.contains_key(*id))).then_some(live.len())
}

async fn tick(
    cfg: &ProvisionerConfig,
    slurm: &dyn SlurmClient,
    obleth: &dyn OblethClient,
    http: &reqwest::Client,
    resolver: &resolve::HostResolver,
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
    if let Some(n) = all_live_jobs_vanished(&all_replicas, &jobs) {
        tracing::error!(
            jobs = n,
            "slurmrestd reports every tracked job as not found; holding tick \
             (check the slurmrestd URL/API version and cluster)"
        );
        anyhow::bail!("all {n} tracked slurm jobs reported not found; holding tick");
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
        let mut replicas = by_model.remove(&spec.model_id).unwrap_or_default();
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
                                // Resolve the node name to an address ONCE per tick
                                // (cached across ticks) and probe by that address,
                                // so a flaky resolver can't make a live replica
                                // flap unhealthy. Falls back to the name on a miss.
                                let probe_host =
                                    resolver.resolve(node).await.unwrap_or_else(|| node.clone());
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
                                    let host = probe_host.clone();
                                    let health_path = spec.health_path.clone();
                                    let timeout = cfg.health_timeout_secs;
                                    set.spawn(async move {
                                        let api_base = format!("http://{host}:{p}");
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
            Ok(e) => Some(e),
            Err(e) => {
                tracing::warn!(model_id = %spec.model_id, error = %e,
                    "endpoint health lookup failed; self-heal using probe signal only");
                None
            }
        };
        let endpoints_slice = endpoints.as_deref().unwrap_or(&[]);
        let restart = plan::restart_candidates(
            &replicas,
            probe_failures,
            cfg.restart_after_failures,
            endpoints_slice,
        );
        // If the endpoint list came back cleanly this tick, null out any healthy
        // replica whose endpoint_id is dangling — the endpoint was removed out of
        // band and there is no FK to null it for us — so the planner re-promotes
        // and relinks a fresh endpoint instead of leaving the model short one (the
        // "2 healthy replicas, 1 endpoint" split). Skipped when the fetch failed,
        // so a transient error can't strip the whole fleet's endpoints at once.
        if let Some(eps) = &endpoints {
            let live: HashSet<uuid::Uuid> = eps.iter().map(|ep| ep.id).collect();
            plan::clear_dangling_endpoints(&mut replicas, &live);
        }

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
                resolver,
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
                            // Warm up against the resolved IP too, so the slow cold
                            // first token isn't paid on a DNS miss.
                            let api_base = resolver.resolve_url_host(api_base).await;
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

        // Migrate any already-registered endpoint that still points at a node
        // *name* to its resolved IP, so existing healthy replicas stop depending
        // on per-request DNS without waiting to be re-promoted. resolve_url_host
        // returns the URL unchanged when the host is already an IP or can't be
        // resolved, so this only fires on a real name→IP change. Best-effort: a
        // failure here never holds the tick. Only runs on a good endpoint
        // snapshot (fetch succeeded above).
        if let Some(eps) = &endpoints {
            for r in &replicas {
                if r.state != "healthy" {
                    continue;
                }
                let Some(ep_id) = r.endpoint_id else { continue };
                let Some(ep) = eps.iter().find(|e| e.id == ep_id) else {
                    continue;
                };
                let desired = resolver.resolve_url_host(&ep.api_base).await;
                if desired != ep.api_base {
                    match obleth
                        .update_endpoint_api_base(spec.model_id, ep, &desired)
                        .await
                    {
                        Ok(()) => tracing::info!(
                            model_id = %spec.model_id, endpoint_id = %ep_id,
                            from = %ep.api_base, to = %desired,
                            "migrated endpoint to resolved IP"
                        ),
                        Err(e) => tracing::warn!(
                            model_id = %spec.model_id, endpoint_id = %ep_id, error = %e,
                            "failed to migrate endpoint to resolved IP"
                        ),
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
                resolver,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ClusterResources, JobInfo, JobState, JobSubmit, ReplicaView};
    use crate::obleth_client::MockObleth;
    use std::sync::atomic::Ordering;
    use std::sync::Mutex;

    /// Slurm fake that answers `get_job` from a fixed map (absent = `Ok(None)`)
    /// and records cancels/submits so a test can assert nothing destructive ran.
    #[derive(Default)]
    struct FakeSlurm {
        jobs: HashMap<String, JobInfo>,
        cancelled: Mutex<Vec<String>>,
        submitted: Mutex<Vec<String>>,
    }
    #[async_trait::async_trait]
    impl SlurmClient for FakeSlurm {
        async fn submit(&self, job: &JobSubmit) -> anyhow::Result<String> {
            self.submitted.lock().unwrap().push(job.name.clone());
            Ok("999".into())
        }
        async fn cancel(&self, job_id: &str) -> anyhow::Result<()> {
            self.cancelled.lock().unwrap().push(job_id.to_string());
            Ok(())
        }
        async fn get_job(&self, job_id: &str) -> anyhow::Result<Option<JobInfo>> {
            Ok(self.jobs.get(job_id).cloned())
        }
        async fn discover_resources(&self) -> anyhow::Result<ClusterResources> {
            Ok(ClusterResources::default())
        }
    }

    fn cfg() -> ProvisionerConfig {
        ProvisionerConfig {
            admin_base_url: "http://127.0.0.1:1".into(),
            admin_token: "t".into(),
            interval_secs: 15,
            health_timeout_secs: 1,
            warmup_timeout_secs: 0,
            lost_retention_secs: 900,
            restart_after_failures: 0,
            port_span: 8,
            job_name_prefix: "obleth-".into(),
        }
    }

    fn replica(job: &str, state: &str) -> ReplicaView {
        ReplicaView {
            id: uuid::Uuid::new_v4(),
            model_id: uuid::Uuid::new_v4(),
            slurm_job_id: job.into(),
            state: state.into(),
            endpoint_id: None,
            age_secs: 60,
            port_base: 8000,
            last_message: None,
            cancel_requested: false,
        }
    }

    fn pending_job(id: &str) -> JobInfo {
        JobInfo {
            job_id: id.into(),
            state: JobState::Pending,
            nodes: vec![],
            raw_state: "PENDING".into(),
            reason: None,
        }
    }

    async fn run_tick(slurm: &FakeSlurm, obleth: &MockObleth) -> anyhow::Result<()> {
        let resolver = resolve::HostResolver::new(Duration::from_secs(300));
        let mut probe_failures = HashMap::new();
        tick(
            &cfg(),
            slurm,
            obleth,
            &reqwest::Client::new(),
            &resolver,
            &mut probe_failures,
        )
        .await
    }

    fn nothing_destructive(slurm: &FakeSlurm, obleth: &MockObleth) {
        assert!(slurm.cancelled.lock().unwrap().is_empty(), "no scancel");
        assert!(slurm.submitted.lock().unwrap().is_empty(), "no submit");
        assert!(obleth.patched.lock().unwrap().is_empty(), "no state change");
        assert!(
            obleth.deleted_replicas.lock().unwrap().is_empty(),
            "no delete"
        );
    }

    #[tokio::test]
    async fn tick_bails_when_managed_models_read_fails() {
        let slurm = FakeSlurm::default();
        let obleth = MockObleth::default();
        *obleth.replicas.lock().unwrap() = vec![replica("1", "healthy")];
        obleth.fail_list_managed.store(true, Ordering::SeqCst);
        assert!(run_tick(&slurm, &obleth).await.is_err());
        nothing_destructive(&slurm, &obleth);
    }

    #[tokio::test]
    async fn tick_bails_when_replica_read_fails() {
        let slurm = FakeSlurm::default();
        let obleth = MockObleth::default();
        obleth.fail_list_replicas.store(true, Ordering::SeqCst);
        assert!(run_tick(&slurm, &obleth).await.is_err());
        nothing_destructive(&slurm, &obleth);
    }

    #[tokio::test]
    async fn tick_holds_when_every_tracked_job_vanishes() {
        // Two live replicas, Slurm answers "no such job" for both: far more
        // likely a wrong cluster/version than a fleet that died in one tick.
        let slurm = FakeSlurm::default();
        let obleth = MockObleth::default();
        *obleth.replicas.lock().unwrap() = vec![replica("1", "healthy"), replica("2", "pending")];
        let err = run_tick(&slurm, &obleth).await.unwrap_err();
        assert!(format!("{err:#}").contains("holding tick"), "{err:#}");
        nothing_destructive(&slurm, &obleth);
    }

    #[tokio::test]
    async fn tick_marks_lost_when_only_some_jobs_vanish() {
        let mut slurm = FakeSlurm::default();
        slurm.jobs.insert("2".into(), pending_job("2"));
        let obleth = MockObleth::default();
        let gone = replica("1", "healthy");
        let gone_id = gone.id;
        *obleth.replicas.lock().unwrap() = vec![gone, replica("2", "pending")];
        run_tick(&slurm, &obleth).await.unwrap();
        let patched = obleth.patched.lock().unwrap();
        assert!(
            patched
                .iter()
                .any(|(id, s)| *id == gone_id && s.as_deref() == Some("lost")),
            "the vanished job's replica is marked lost: {patched:?}"
        );
    }

    #[tokio::test]
    async fn breaker_ignores_rows_already_lost_or_draining() {
        // Purged jobs behind lost/draining rows are expected, not suspicious:
        // they must not keep the tick held (their GC would never run).
        let slurm = FakeSlurm::default();
        let obleth = MockObleth::default();
        let mut old_lost = replica("1", "lost");
        old_lost.age_secs = 10_000;
        let old_lost_id = old_lost.id;
        *obleth.replicas.lock().unwrap() = vec![old_lost, replica("2", "draining")];
        run_tick(&slurm, &obleth).await.unwrap();
        assert!(obleth
            .deleted_replicas
            .lock()
            .unwrap()
            .contains(&old_lost_id));
    }

    #[tokio::test]
    async fn single_vanished_job_is_still_marked_lost() {
        let slurm = FakeSlurm::default();
        let obleth = MockObleth::default();
        let r = replica("1", "healthy");
        let id = r.id;
        *obleth.replicas.lock().unwrap() = vec![r];
        run_tick(&slurm, &obleth).await.unwrap();
        assert!(obleth
            .patched
            .lock()
            .unwrap()
            .iter()
            .any(|(i, s)| *i == id && s.as_deref() == Some("lost")));
    }
}
