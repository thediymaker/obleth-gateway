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

use chrono::{DateTime, Utc};
use config::ProvisionerConfig;
use domain::{Action, JobInfo, JobState, ReplicaView};
use obleth_client::{HttpObleth, ModelIdentity, OblethClient, TickReport};
use obleth_config::{ManagedBackoff, ManagedModelSpec};
use obleth_provisioner::slurm::{SlurmClient, SlurmErrorKind, Slurmrestd};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use uuid::Uuid;

/// Outcome of one loop iteration, so the loop can log idle/active transitions
/// without spamming a line every tick.
enum Tick {
    /// A reconcile tick ran; carries warnings for the operator.
    Ran(Vec<String>),
    Idle(&'static str),
}

/// How long a resolved node address is cached before re-resolving. Node IPs are
/// stable for a job's lifetime; a few minutes catches the rare case of a node
/// rebooting onto a new address without hammering DNS.
const RESOLVE_TTL_SECS: u64 = 300;

/// How often Slurm's node addresses (NodeAddr) are read again while jobs run.
const NODE_ADDR_REFRESH: Duration = Duration::from_secs(600);

/// How long after a submit that got no answer the model's submits wait while
/// the accounting database is checked for the job. Slurm records a job there
/// within seconds of accepting it.
const UNCERTAIN_SUBMIT_SECS: i64 = 180;

/// What the reconcile loop remembers between ticks. In memory on purpose: the
/// provisioner is a singleton, and a restart only resets these streaks and
/// clocks (worst case: a remedy waits one more window).
#[derive(Default)]
struct Memory {
    /// Self-heal: net failed health probes per healthy replica.
    probe_failures: HashMap<Uuid, i64>,
    /// When each replica started waiting on something the planner times: a
    /// pending job Slurm will not start, or a serving replica awaiting its
    /// replacement.
    waiting_since: HashMap<Uuid, DateTime<Utc>>,
    /// Submits that got no answer, by model, until the job is found or the
    /// wait is over.
    uncertain: HashMap<Uuid, Vec<UncertainSubmit>>,
    /// When Slurm's node addresses were last read.
    node_addrs_read: Option<std::time::Instant>,
}

/// A submit whose answer was lost: Slurm may or may not have the job.
#[derive(Debug, Clone)]
struct UncertainSubmit {
    job_name: String,
    port_base: i64,
    /// `<working dir>/<job name>`: the job's output is `<this>-<id>.out`.
    log_prefix: String,
    at: DateTime<Utc>,
}

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
    let mut mem = Memory::default();
    // Long-lived so its success cache survives across ticks: once a node name is
    // resolved (or aliased) the provisioner stops touching DNS for it.
    let resolver = resolve::HostResolver::new(Duration::from_secs(RESOLVE_TTL_SECS));
    let (shutdown_tx, mut shutdown) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        wait_for_signal().await;
        let _ = shutdown_tx.send(true);
    });
    loop {
        match run_once(&cfg, &obleth, &http, &resolver, &mut mem).await {
            Ok(Tick::Ran(warnings)) => {
                obleth.set_last_tick(TickReport::ok().with_warnings(warnings));
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
        // The tick above is never interrupted: stopping mid-tick could leave
        // a submitted job unrecorded. A signal only cuts the idle wait short.
        if !sleep_unless_shutdown(Duration::from_secs(cfg.interval_secs), &mut shutdown).await {
            tracing::info!("shutdown signal received; exiting after the completed tick");
            return Ok(());
        }
    }
}

/// Sleep for `period`; returns `false` as soon as shutdown is requested
/// (including a request that arrived during the previous tick).
async fn sleep_unless_shutdown(
    period: Duration,
    shutdown: &mut tokio::sync::watch::Receiver<bool>,
) -> bool {
    tokio::select! {
        biased;
        _ = shutdown.wait_for(|stop| *stop) => false,
        _ = tokio::time::sleep(period) => true,
    }
}

async fn wait_for_signal() {
    let ctrl_c = async {
        if let Err(e) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %e, "ctrl-c handler unavailable");
            std::future::pending::<()>().await;
        }
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "SIGTERM handler unavailable");
                std::future::pending::<()>().await;
            }
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
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
    mem: &mut Memory,
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
    // `auto` (the default) resolves to the newest API version the server
    // offers that this build supports; the answer is cached for ten minutes.
    let mut slurm = Slurmrestd::connect(
        http.clone(),
        &settings.slurmrestd_url,
        &settings.slurmrestd_api_version,
        &settings.slurm_user,
        &settings.slurm_jwt,
    )
    .await?
    .with_hf_token(&settings.hf_token)
    .with_job_name_prefix(&cfg.job_name_prefix);
    // One authenticated read every tick, so a rejected token shows as an
    // error tick even when no replica would otherwise call Slurm.
    let token_warning =
        slurm::token_expiry_warning(&settings.slurm_jwt, chrono::Utc::now().timestamp());
    let mut auth = slurm.check_auth().await;
    // A pinned API version the server no longer serves (an old default kept
    // across a Slurm upgrade) falls back to the newest one it does serve, with
    // a warning, instead of failing every tick. A pin that works is kept.
    let mut version_warning = None;
    let pinned_not_served = auth
        .as_ref()
        .err()
        .and_then(slurm::slurm_api_error)
        .is_some_and(|se| {
            se.kind == slurm::SlurmErrorKind::NotServed
                && !obleth_config::slurm::is_auto_api_version(&settings.slurmrestd_api_version)
        });
    if pinned_not_served {
        let fallback = Slurmrestd::connect(
            http.clone(),
            &settings.slurmrestd_url,
            "auto",
            &settings.slurm_user,
            &settings.slurm_jwt,
        )
        .await;
        if let Ok(fallback) = fallback {
            version_warning = Some(format!(
                "slurmrestd doesn't serve the configured API version {}, so obleth uses {}, the \
                 newest one it serves. Set the API version to auto in the Slurm settings.",
                settings.slurmrestd_api_version.trim(),
                fallback.version()
            ));
            slurm = fallback
                .with_hf_token(&settings.hf_token)
                .with_job_name_prefix(&cfg.job_name_prefix);
            auth = slurm.check_auth().await;
        }
    }
    if let Err(e) = auth {
        // A rejected token that has also expired says so.
        let rejected = slurm::slurm_api_error(&e)
            .is_some_and(|se| se.kind == slurm::SlurmErrorKind::TokenRejected);
        match token_warning.filter(|_| rejected) {
            Some(w) => anyhow::bail!("{e} {w}"),
            None => return Err(e),
        }
    }
    let mut warnings: Vec<String> = version_warning.into_iter().chain(token_warning).collect();
    warnings.extend(tick(cfg, &slurm, obleth, http, resolver, mem).await?);
    Ok(Tick::Ran(warnings))
}

/// Jobs behind live replicas that Slurm no longer knows, when that is every
/// one of them and there are at least two. A whole fleet going missing in
/// one tick is far likelier a wrong cluster, URL or API version than real
/// deaths, so the tick checks the accounting database before it believes
/// it. Only replicas the planner would mark lost count: `lost`/`draining`
/// rows are expected to point at purged jobs, and a replica the operator
/// stopped is let go whatever Slurm says.
fn vanished_fleet(
    replicas: &[ReplicaView],
    jobs: &HashMap<String, JobInfo>,
) -> Option<Vec<String>> {
    let live: HashSet<&str> = replicas
        .iter()
        .filter(|r| {
            r.state != "lost"
                && r.state != "draining"
                && !r.cancel_requested
                && !r.slurm_job_id.is_empty()
        })
        .map(|r| r.slurm_job_id.as_str())
        .collect();
    let mut ids: Vec<String> = live.iter().map(|s| s.to_string()).collect();
    ids.sort();
    (live.len() >= 2 && live.iter().all(|id| !jobs.contains_key(*id))).then_some(ids)
}

/// Counts failed actions by kind, for the tick's warnings.
#[derive(Default)]
struct ActionFailures(Vec<(&'static str, usize, String)>);

impl ActionFailures {
    fn add(&mut self, kind: &'static str, error: String) {
        match self.0.iter_mut().find(|(k, ..)| *k == kind) {
            Some(entry) => entry.1 += 1,
            None => self.0.push((kind, 1, error)),
        }
    }

    /// One plain sentence per kind, e.g. "2 job cancels failed. The first:
    /// Slurm refused cancelling job 12: Access/permission denied."
    fn sentences(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|(kind, n, first)| {
                let first = first.trim().trim_end_matches('.');
                if *n == 1 {
                    format!("A {kind} failed: {first}.")
                } else {
                    format!("{n} {kind}s failed. The first: {first}.")
                }
            })
            .collect()
    }
}

/// A model's work for this tick, gathered before anything is changed so the
/// fleet guard can look at every probe at once.
struct ModelWork<'a> {
    spec: &'a ManagedModelSpec,
    identity: ModelIdentity,
    replicas: Vec<ReplicaView>,
    health: HashMap<Uuid, u16>,
    notes: HashMap<Uuid, String>,
    hold_submits: bool,
}

async fn tick(
    cfg: &ProvisionerConfig,
    slurm: &dyn SlurmClient,
    obleth: &dyn OblethClient,
    http: &reqwest::Client,
    resolver: &resolve::HostResolver,
    mem: &mut Memory,
) -> anyhow::Result<Vec<String>> {
    let now = chrono::Utc::now();
    let mut warnings: Vec<String> = Vec::new();
    let mut failures = ActionFailures::default();
    let specs = obleth.list_managed_models().await?; // obleth down -> bail (held). enabled only.
    let all_replicas = obleth.list_all_replicas().await?; // obleth down -> bail (held)
                                                          // Every model's name in one read; a model missing from it is looked up
                                                          // on its own below.
    let identities = obleth.model_identities().await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "model list read failed; looking names up one by one");
        HashMap::new()
    });

    // Drop self-heal streaks and wait clocks of replicas that no longer exist
    // or are no longer healthy, so the maps can't grow unbounded.
    mem.probe_failures.retain(|id, _| {
        all_replicas
            .iter()
            .any(|r| r.id == *id && r.state == "healthy")
    });
    mem.waiting_since
        .retain(|id, _| all_replicas.iter().any(|r| r.id == *id));

    // Look up Slurm state for just the jobs we track, by id — never the whole
    // controller (which on a busy cluster is huge and OOM-kills us). A clean
    // "not found" leaves a job out of the map, which the planner reads as gone;
    // a transport/HTTP error bails the whole tick so an unreachable Slurm is
    // never mistaken for a fleet of dead jobs (no destructive action while held).
    let mut jobs: HashMap<String, JobInfo> = HashMap::new();
    // Lost rows only age out, so their jobs are not looked up: a lookup
    // error on a long-gone job must not hold every model's tick.
    for id in all_replicas
        .iter()
        .filter(|r| r.state != "lost")
        .map(|r| &r.slurm_job_id)
    {
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
    if let Some(ids) = vanished_fleet(&all_replicas, &jobs) {
        // The accounting database keeps jobs the controller has purged: a job
        // it shows as ended really ended. Anything it cannot confirm is left
        // as it is (and keeps its slot) until Slurm knows it again or the
        // operator stops that replica.
        let mut held = Vec::new();
        for id in &ids {
            match slurm.accounting_job(id).await {
                Ok(Some(info)) if info.state == JobState::Gone => {
                    jobs.insert(id.clone(), info);
                }
                _ => held.push(id.clone()),
            }
        }
        if !held.is_empty() {
            tracing::error!(
                jobs = ?held,
                "slurmrestd reports every tracked job as not found and accounting cannot \
                 confirm they ended; leaving those replicas as they are \
                 (check the slurmrestd URL/API version and cluster)"
            );
            for id in &held {
                jobs.insert(
                    id.clone(),
                    JobInfo {
                        job_id: id.clone(),
                        state: JobState::Unknown,
                        ..Default::default()
                    },
                );
            }
            warnings.push(format!(
                "Slurm no longer knows any of the {} jobs obleth tracks, and its accounting database \
                 could not confirm that they ended, so obleth leaves those replicas alone. If the jobs \
                 really ended, stop each replica on its deployment and obleth starts new ones.",
                held.len()
            ));
        }
    }

    // Slurm's own node addresses, read now and then while jobs run, so nodes
    // with cluster-only names are reached without per-node aliases.
    let any_running = jobs.values().any(|j| j.state == JobState::Running);
    if any_running
        && mem
            .node_addrs_read
            .is_none_or(|t| t.elapsed() >= NODE_ADDR_REFRESH)
    {
        mem.node_addrs_read = Some(std::time::Instant::now());
        match slurm.node_addresses().await {
            Ok(addrs) => resolver.set_node_addresses(addrs),
            Err(e) => {
                tracing::debug!(error = %e, "node address read failed; using aliases and DNS")
            }
        }
    }

    // Annotate each replica with its live Slurm status so the dashboard shows why
    // a job is pending or what state it is in. Only non-terminal jobs (terminal
    // ones get the planner's MarkLost message) and only when the message changed,
    // so we don't write every tick. Draining rows are left alone: their last
    // update marks when the drain started.
    for r in all_replicas.iter().filter(|r| r.state != "draining") {
        if let Some(job) = jobs.get(&r.slurm_job_id) {
            if matches!(job.state, JobState::Pending | JobState::Running) {
                let msg = slurm::job_status_message(&job.raw_state, job.reason.as_deref());
                if r.last_message.as_deref() != Some(msg.as_str()) {
                    // Once the job runs, record its allocation: the gateway
                    // stamps the launch's start time when nodes first arrive,
                    // which is what splits queue time from load time in the
                    // launch history.
                    let nodes = (job.state == JobState::Running && !job.nodes.is_empty())
                        .then(|| job.nodes.join(","));
                    if let Err(e) = obleth
                        .patch_replica(r.id, None, nodes.as_deref(), None, Some(&msg))
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
    let mut by_model: HashMap<Uuid, Vec<ReplicaView>> = HashMap::new();
    for r in all_replicas.iter().cloned() {
        by_model.entry(r.model_id).or_default().push(r);
    }
    // Port windows in use by every model's live replicas: a new window never
    // overlaps one of them, so two replicas never share a port.
    // A submit whose answer was lost may still start, so its window stays
    // taken until the job is found or given up on.
    let mut live_bases: Vec<i64> = all_replicas
        .iter()
        .filter(|r| r.state != "lost")
        .map(|r| r.port_base)
        .chain(mem.uncertain.values().flatten().map(|u| u.port_base))
        .collect();
    let mut tracked: HashSet<String> = all_replicas
        .iter()
        .filter(|r| r.state != "lost")
        .map(|r| r.slurm_job_id.clone())
        .collect();

    // 1. Gather each enabled model's state and probe its replicas.
    let mut work: Vec<ModelWork> = Vec::new();
    for spec in &specs {
        // Claim this model's replicas so the drain pass below ignores them, even
        // if we end up skipping this model for a transient reason.
        let mut replicas = by_model.remove(&spec.model_id).unwrap_or_default();
        let identity = match identities.get(&spec.model_id) {
            Some(i) => i.clone(),
            None => match obleth.model_identity(spec.model_id).await {
                Ok(i) => i,
                Err(e) => {
                    tracing::warn!(model_id = %spec.model_id, error = %e, "model lookup failed; skipping this tick");
                    continue;
                }
            },
        };
        if identity.name.is_empty() {
            tracing::warn!(model_id = %spec.model_id, "model has empty name; skipping this tick");
            continue;
        }
        let hold_submits = adopt_lost_submits(
            spec,
            &identity,
            slurm,
            obleth,
            &mut replicas,
            &mut tracked,
            &mut jobs,
            mem,
            now,
            &mut warnings,
        )
        .await;
        let (health, notes) =
            probe_model(cfg, http, resolver, spec, &identity, &replicas, &jobs).await;
        work.push(ModelWork {
            spec,
            identity,
            replicas,
            health,
            notes,
            hold_submits,
        });
    }

    // Fleet guard: when every probed healthy replica of every model failed at
    // once, the fault is almost certainly on the provisioner's own path to
    // the nodes. Hold every self-heal restart and leave the streaks alone.
    let probed_rows: Vec<ReplicaView> = work.iter().flat_map(|w| w.replicas.clone()).collect();
    let all_health: HashMap<Uuid, u16> = work
        .iter()
        .flat_map(|w| w.health.iter().map(|(k, v)| (*k, *v)))
        .collect();
    let path_down = plan::probe_path_down(&probed_rows, &jobs, &all_health);
    if let Some(n) = path_down {
        tracing::error!(
            replicas = n,
            "every health probe failed; holding self-heal restarts"
        );
        warnings.push(format!(
            "The provisioner could not reach any of the {n} running model servers, so it is not \
             restarting any of them. Check the network between the provisioner and the compute nodes."
        ));
    }

    // 2. Plan and apply each model.
    for w in &mut work {
        let spec = w.spec;
        if path_down.is_none() {
            plan::update_probe_failures(&w.replicas, &jobs, &w.health, &mut mem.probe_failures);
        }
        // The gateway's endpoint verdicts (its check is a real inference, so
        // it catches zombies whose metadata GET still answers). Best-effort:
        // without them the probe signal still works, and the phantom sweep
        // waits for a good snapshot.
        let endpoints = match obleth.list_endpoints(spec.model_id).await {
            Ok(e) => Some(e),
            Err(e) => {
                tracing::warn!(model_id = %spec.model_id, error = %e,
                    "endpoint lookup failed; self-heal using probe signal only");
                None
            }
        };
        let heal = if path_down.is_some() {
            plan::SelfHeal::default()
        } else {
            plan::self_heal_candidates(
                &w.replicas,
                &mem.probe_failures,
                cfg.restart_after_failures,
                endpoints.as_deref().unwrap_or(&[]),
            )
        };
        if let Some(eps) = &endpoints {
            let live: HashSet<Uuid> = eps.iter().map(|ep| ep.id).collect();
            plan::clear_dangling_endpoints(&mut w.replicas, &live);
        }
        let waited = waited_secs(&mut mem.waiting_since, &w.replicas, &jobs, &heal, now);
        let view = plan::ManagedSpecView {
            target_replicas: spec.target_replicas,
            startup_timeout_mins: spec.startup_timeout_mins,
            handoff_mins: spec.handoff_mins,
            paused_until: spec.paused_until,
            removed: false,
        };
        let prefix = format!("{}{}-", cfg.job_name_prefix, w.identity.name);
        let mut actions = plan::plan(&plan::PlanInput {
            spec: &view,
            replicas: &w.replicas,
            jobs: &jobs,
            health: &w.health,
            probe_notes: &w.notes,
            self_heal: &heal,
            waited_secs: &waited,
            endpoints: endpoints.as_deref().map(|e| (e, prefix.as_str())),
            now,
            lost_retention_secs: cfg.lost_retention_secs,
        });
        if w.hold_submits {
            actions.retain(|a| !matches!(a, Action::Submit));
        }

        let mut outcome = plan::LaunchOutcome::default();
        let mut stop_submits = false;
        for action in &actions {
            let is_submit = matches!(action, Action::Submit);
            if is_submit && stop_submits {
                continue;
            }
            // Reserve a distinct window per Submit *before* applying, so
            // several Submits in one tick (of any model) never share ports.
            let port_base = if is_submit {
                let b = plan::next_free_window_base(spec.serving_port, cfg.port_span, &live_bases);
                live_bases.push(b);
                b
            } else {
                0
            };
            match executor::apply(
                action,
                spec.model_id,
                &w.identity.name,
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
                Err(e) => {
                    tracing::warn!(?action, error = %e, "action failed; continuing");
                    failures.add(action.kind(), format!("{e:#}"));
                    if is_submit {
                        match slurm::slurm_api_error(&e).map(|se| se.kind) {
                            Some(SlurmErrorKind::Rejected) => {
                                outcome.rejected = Some(e.to_string());
                                stop_submits = true;
                            }
                            // No usable answer: Slurm may have the job. Hold
                            // this model's submits and look for it by name.
                            Some(SlurmErrorKind::NoAnswer | SlurmErrorKind::Unavailable) => {
                                let job = slurm::job_submit_from_spec(
                                    spec,
                                    &w.identity.name,
                                    &cfg.job_name_prefix,
                                    port_base,
                                    cfg.port_span,
                                );
                                mem.uncertain.entry(spec.model_id).or_default().push(
                                    UncertainSubmit {
                                        log_prefix: format!(
                                            "{}/{}",
                                            slurm::job_working_dir(&job),
                                            job.name
                                        ),
                                        job_name: job.name,
                                        port_base,
                                        at: now,
                                    },
                                );
                                stop_submits = true;
                            }
                            _ => {}
                        }
                    }
                }
                Ok(problem) => {
                    if let Some(p) = problem {
                        failures.add(action.kind(), p);
                    }
                    if plan::is_failed_launch(action, &w.replicas) {
                        outcome.failed += 1;
                        outcome.last_failure = plan::failure_phrase(action);
                    }
                    if let Action::Promote {
                        replica_id,
                        api_base,
                        ..
                    } = action
                    {
                        // A launch served (not a relink of a serving replica).
                        if w.replicas
                            .iter()
                            .any(|r| r.id == *replica_id && r.state != "healthy")
                        {
                            outcome.served = true;
                        }
                        spawn_warmup(cfg, http, resolver, api_base, &w.identity).await;
                    }
                }
            }
        }
        record_backoff(spec, &outcome, now, obleth, &mut failures).await;

        // Migrate any already-registered endpoint that still points at a node
        // *name* to its resolved IP, so existing healthy replicas stop depending
        // on per-request DNS without waiting to be re-promoted. resolve_url_host
        // returns the URL unchanged when the host is already an IP or can't be
        // resolved, so this only fires on a real name→IP change. Best-effort: a
        // failure here never holds the tick. Only runs on a good endpoint
        // snapshot (fetch succeeded above).
        if let Some(eps) = &endpoints {
            for r in &w.replicas {
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

    // 3. Drain models that still have replicas but are no longer in the enabled
    //    managed set (paused, deleted, or never-managed). Reconcile them toward
    //    target 0: drain and cancel live jobs, mark gone ones lost, GC old lost
    //    rows. No Submit/Promote fires at target 0, so the missing spec is fine.
    let drain_view = plan::ManagedSpecView {
        removed: true,
        ..Default::default()
    };
    for (model_id, replicas) in by_model {
        let model_name = match identities.get(&model_id) {
            Some(i) => i.name.clone(),
            None => obleth
                .model_identity(model_id)
                .await
                .map(|i| i.name)
                .unwrap_or_default(),
        };
        tracing::info!(%model_id, replicas = replicas.len(), "draining replicas for unmanaged model");
        let actions = plan::plan(&plan::PlanInput {
            spec: &drain_view,
            replicas: &replicas,
            jobs: &jobs,
            health: &HashMap::new(),
            probe_notes: &HashMap::new(),
            self_heal: &plan::SelfHeal::default(),
            waited_secs: &HashMap::new(),
            endpoints: None,
            now,
            lost_retention_secs: cfg.lost_retention_secs,
        });
        for action in &actions {
            // Drain reconciles toward target 0, so no Submit fires; port_base is unused.
            match executor::apply(
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
                Err(e) => {
                    tracing::warn!(?action, error = %e, "drain action failed; continuing");
                    failures.add(action.kind(), format!("{e:#}"));
                }
                Ok(Some(p)) => failures.add(action.kind(), p),
                Ok(None) => {}
            }
        }
    }

    // Orphan jobs (submitted but never recorded) are prevented at the source:
    // the Submit executor cancels a job if recording its replica fails, and a
    // submit whose answer was lost is looked up by name before the model
    // submits again. There is no cluster-wide scan: we never list the whole
    // controller.
    warnings.extend(failures.sentences());
    Ok(warnings)
}

/// Seconds each replica has been waiting on something the planner times out:
/// a pending job with a reason Slurm will not clear, or a serving replica
/// that is being replaced. Starts the clock on first sight and stops it when
/// the condition clears.
fn waited_secs(
    since: &mut HashMap<Uuid, DateTime<Utc>>,
    replicas: &[ReplicaView],
    jobs: &HashMap<String, JobInfo>,
    heal: &plan::SelfHeal,
    now: DateTime<Utc>,
) -> HashMap<Uuid, i64> {
    let mut out = HashMap::new();
    for r in replicas {
        let waiting = match r.state.as_str() {
            "healthy" => {
                r.replace_requested || heal.dead.contains(&r.id) || heal.zombie.contains(&r.id)
            }
            "pending" | "starting" => jobs
                .get(&r.slurm_job_id)
                .is_some_and(|j| plan::stuck_reason(j).is_some()),
            _ => false,
        };
        if waiting {
            let t = *since.entry(r.id).or_insert(now);
            out.insert(r.id, (now - t).num_seconds());
        } else {
            since.remove(&r.id);
        }
    }
    out
}

/// Submits of this model whose answer was lost: look the job up by name in
/// the accounting database and track it when found. Returns true while the
/// model's submits must wait (a lost answer that is still recent and not
/// found yet), so a lost answer never turns into a second job.
#[allow(clippy::too_many_arguments)]
async fn adopt_lost_submits(
    spec: &ManagedModelSpec,
    identity: &ModelIdentity,
    slurm: &dyn SlurmClient,
    obleth: &dyn OblethClient,
    replicas: &mut Vec<ReplicaView>,
    tracked: &mut HashSet<String>,
    jobs: &mut HashMap<String, JobInfo>,
    mem: &mut Memory,
    now: DateTime<Utc>,
    warnings: &mut Vec<String>,
) -> bool {
    let Some(lost) = mem.uncertain.remove(&spec.model_id) else {
        return false;
    };
    let mut keep = Vec::new();
    for u in lost {
        let recent = (now - u.at).num_seconds() < UNCERTAIN_SUBMIT_SECS;
        let found = match slurm.find_live_jobs_by_name(&u.job_name).await {
            Ok(found) => found,
            Err(e) => {
                tracing::warn!(model = %identity.name, error = %e, "accounting lookup failed");
                if recent {
                    keep.push(u);
                } else {
                    warnings.push(format!(
                        "A job submit for {} got no answer, and obleth could not check Slurm's \
                         accounting database, so a second job may be running. Look for jobs named \
                         {} with squeue and cancel any that the deployment does not list.",
                        identity.name, u.job_name
                    ));
                }
                continue;
            }
        };
        // A live job of this name that no replica tracks, submitted around
        // the time of the lost answer.
        let hit = found.into_iter().find(|j| {
            !tracked.contains(&j.job_id)
                && j.submit_time
                    .is_none_or(|t| (t - u.at).num_seconds().abs() <= UNCERTAIN_SUBMIT_SECS)
        });
        let Some(found_job) = hit else {
            if recent {
                keep.push(u);
            }
            continue;
        };
        let log_path = format!("{}-{}.out", u.log_prefix, found_job.job_id);
        match obleth
            .create_replica_with_log(
                spec.model_id,
                &found_job.job_id,
                u.port_base,
                Some(&log_path),
            )
            .await
        {
            Ok(id) => {
                tracing::warn!(model = %identity.name, job_id = %found_job.job_id,
                    "adopted a job whose submit answer was lost");
                tracked.insert(found_job.job_id.clone());
                let info = match slurm.get_job(&found_job.job_id).await {
                    Ok(Some(info)) => info,
                    _ => found_job.clone(),
                };
                jobs.insert(found_job.job_id.clone(), info);
                replicas.push(ReplicaView {
                    id,
                    model_id: spec.model_id,
                    slurm_job_id: found_job.job_id.clone(),
                    state: "pending".into(),
                    port_base: u.port_base,
                    log_path: Some(log_path),
                    ..Default::default()
                });
                warnings.push(format!(
                    "A job submit for {} got no answer, but Slurm had accepted it as job {}. \
                     obleth now tracks that job.",
                    identity.name, found_job.job_id
                ));
            }
            Err(e) => {
                tracing::warn!(model = %identity.name, error = %e, "could not record an adopted job");
                keep.push(u);
            }
        }
    }
    let hold = !keep.is_empty();
    if hold {
        warnings.push(format!(
            "A job submit for {} got no answer from slurmrestd. obleth is checking whether Slurm \
             started the job before it submits again.",
            identity.name
        ));
        mem.uncertain.insert(spec.model_id, keep);
    }
    hold
}

/// Probe every replica of a model whose job runs: the port the job reported
/// in its comment when it did, else every port of its window. A port passes
/// when the health path answers and the server lists this model (or lists
/// none). Returns the passing port per replica, and a note for a replica
/// whose port answers for another model.
async fn probe_model(
    cfg: &ProvisionerConfig,
    http: &reqwest::Client,
    resolver: &resolve::HostResolver,
    spec: &ManagedModelSpec,
    identity: &ModelIdentity,
    replicas: &[ReplicaView],
    jobs: &HashMap<String, JobInfo>,
) -> (HashMap<Uuid, u16>, HashMap<Uuid, String>) {
    let mut health = HashMap::new();
    let mut notes = HashMap::new();
    let expected = identity.served_names();
    for r in replicas {
        if !matches!(r.state.as_str(), "pending" | "starting" | "healthy") {
            continue;
        }
        let Some(job) = jobs
            .get(&r.slurm_job_id)
            .filter(|j| j.state == JobState::Running)
        else {
            continue;
        };
        // The batch host runs the server; for a multi-node job the node list
        // alone does not say which one.
        let Some(node) = job.probe_host() else {
            tracing::warn!(
                replica_id = %r.id,
                job_id = %r.slurm_job_id,
                "job is RUNNING but slurmrestd returned no nodes; \
                 cannot probe health — check slurmrestd response"
            );
            continue;
        };
        // Resolve the node name to an address once per tick (cached across
        // ticks) and probe by that address, so a flaky resolver can't make a
        // live replica flap unhealthy. Falls back to the name on a miss.
        let host = resolver
            .resolve(node)
            .await
            .unwrap_or_else(|| node.to_string());
        let ports: Vec<i64> = match job.reported_port {
            Some(p) => vec![p],
            None => (r.port_base..r.port_base + cfg.port_span).collect(),
        };
        // Probe the ports concurrently: a not-yet-up replica otherwise costs
        // port_span * health_timeout of serial waits every tick.
        let mut set = tokio::task::JoinSet::new();
        for p in ports
            .into_iter()
            .filter(|p| (1..=u16::MAX as i64).contains(p))
        {
            let http = http.clone();
            let host = host.clone();
            let health_path = spec.health_path.clone();
            let expected = expected.clone();
            let timeout = cfg.health_timeout_secs;
            set.spawn(async move {
                let p = p as u16;
                let res =
                    probe::probe_port(&http, &host, p, &health_path, &expected, timeout).await;
                (p, res)
            });
        }
        let mut serving: Option<u16> = None;
        let mut other: Option<(u16, Vec<String>)> = None;
        while let Some(res) = set.join_next().await {
            match res {
                Ok((p, probe::PortProbe::Serving)) => {
                    serving = Some(serving.map_or(p, |cur| cur.min(p)));
                }
                Ok((p, probe::PortProbe::OtherModel(list)))
                    if other.as_ref().is_none_or(|(cur, _)| p < *cur) =>
                {
                    other = Some((p, list));
                }
                _ => {}
            }
        }
        if let Some(p) = serving {
            tracing::info!(replica_id = %r.id, job_id = %r.slurm_job_id, port = p, "health probe: healthy");
            health.insert(r.id, p);
        } else if let Some((p, list)) = other {
            tracing::warn!(replica_id = %r.id, port = p, served = ?list,
                "health probe: another model answers on the port");
            notes.insert(
                r.id,
                probe::other_model_detail(p, &list, identity.expected()),
            );
        } else {
            tracing::info!(replica_id = %r.id, job_id = %r.slurm_job_id,
                port_base = r.port_base, reported_port = ?job.reported_port,
                "health probe: not yet healthy");
        }
    }
    (health, notes)
}

/// Record the model's failure count and pause state when this tick changed
/// them.
async fn record_backoff(
    spec: &ManagedModelSpec,
    outcome: &plan::LaunchOutcome,
    now: DateTime<Utc>,
    obleth: &dyn OblethClient,
    failures: &mut ActionFailures,
) {
    let current = ManagedBackoff {
        failed_launches: spec.failed_launches,
        backoff_step: spec.backoff_step,
        paused_reason: spec.paused_reason.clone(),
        paused_until: spec.paused_until,
    };
    let Some(next) = plan::next_backoff(&current, spec.max_job_failures, outcome, now) else {
        return;
    };
    if let Some(until) = next
        .paused_until
        .filter(|u| Some(*u) != current.paused_until)
    {
        tracing::warn!(model_id = %spec.model_id, %until, reason = ?next.paused_reason,
            "pausing submits");
    }
    if let Err(e) = obleth.set_managed_backoff(spec.model_id, &next).await {
        failures.add("pause update", format!("{e:#}"));
    }
}

/// A just-promoted replica is healthy by its health path but may still be
/// cold for its first forward pass. Fire a throwaway warmup inference,
/// detached, so the slow cold first token is paid here instead of by the
/// first real user — and never stalls the tick.
async fn spawn_warmup(
    cfg: &ProvisionerConfig,
    http: &reqwest::Client,
    resolver: &resolve::HostResolver,
    api_base: &str,
    identity: &ModelIdentity,
) {
    if cfg.warmup_timeout_secs == 0 {
        return;
    }
    let http = http.clone();
    // Warm up against the resolved IP too, so the slow cold first token isn't
    // paid on a DNS miss.
    let api_base = resolver.resolve_url_host(api_base).await;
    let model_name = identity.name.clone();
    let prefer = identity.served_names();
    let budget = Duration::from_secs(cfg.warmup_timeout_secs);
    tokio::spawn(async move {
        match warmup::warm_up_preferring(&http, &api_base, &prefer, budget).await {
            Ok(()) => tracing::info!(model = %model_name, %api_base, "warmup inference completed"),
            Err(e) => tracing::warn!(
                model = %model_name, %api_base, error = %e,
                "warmup inference failed (best-effort)"
            ),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ClusterResources, JobSubmit};
    use crate::obleth_client::MockObleth;
    use obleth_provisioner::slurm::SlurmApiError;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;

    #[tokio::test]
    async fn idle_wait_continues_without_a_signal() {
        let (_tx, mut rx) = tokio::sync::watch::channel(false);
        assert!(sleep_unless_shutdown(Duration::from_millis(10), &mut rx).await);
    }

    #[tokio::test]
    async fn signal_during_idle_wait_stops_the_loop() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(20)).await;
            let _ = tx.send(true);
        });
        let stopped = tokio::time::timeout(
            Duration::from_secs(5),
            sleep_unless_shutdown(Duration::from_secs(600), &mut rx),
        )
        .await
        .expect("wait cut short");
        assert!(!stopped);
    }

    #[tokio::test]
    async fn signal_during_a_tick_stops_after_it() {
        let (tx, mut rx) = tokio::sync::watch::channel(false);
        tx.send(true).unwrap();
        assert!(!sleep_unless_shutdown(Duration::from_secs(600), &mut rx).await);
    }

    /// Slurm fake that answers `get_job` from a fixed map (absent = `Ok(None)`)
    /// and records cancels/submits so a test can assert nothing destructive ran.
    #[derive(Default)]
    struct FakeSlurm {
        jobs: HashMap<String, JobInfo>,
        /// What the accounting database knows; absent = no accounting.
        accounting: Option<HashMap<String, JobInfo>>,
        /// Live jobs found by name.
        by_name: Mutex<Vec<JobInfo>>,
        cancelled: Mutex<Vec<String>>,
        submitted: Mutex<Vec<String>>,
        submit_gets_no_answer: AtomicBool,
        cancel_refused: AtomicBool,
    }
    #[async_trait::async_trait]
    impl SlurmClient for FakeSlurm {
        async fn submit(
            &self,
            job: &JobSubmit,
        ) -> anyhow::Result<obleth_provisioner::slurm::SubmittedJob> {
            self.submitted.lock().unwrap().push(job.name.clone());
            if self.submit_gets_no_answer.load(Ordering::SeqCst) {
                return Err(SlurmApiError {
                    kind: SlurmErrorKind::NoAnswer,
                    status: None,
                    message: "slurmrestd did not answer in time.".into(),
                }
                .into());
            }
            Ok(obleth_provisioner::slurm::SubmittedJob {
                job_id: "999".into(),
                notes: vec![],
            })
        }
        async fn cancel(&self, job_id: &str) -> anyhow::Result<()> {
            if self.cancel_refused.load(Ordering::SeqCst) {
                anyhow::bail!("Slurm refused cancelling job {job_id}: Access/permission denied");
            }
            self.cancelled.lock().unwrap().push(job_id.to_string());
            Ok(())
        }
        async fn get_job(&self, job_id: &str) -> anyhow::Result<Option<JobInfo>> {
            Ok(self.jobs.get(job_id).cloned())
        }
        async fn discover_resources(&self) -> anyhow::Result<ClusterResources> {
            Ok(ClusterResources::default())
        }
        async fn find_live_jobs_by_name(&self, _name: &str) -> anyhow::Result<Vec<JobInfo>> {
            Ok(self.by_name.lock().unwrap().clone())
        }
        async fn accounting_job(&self, job_id: &str) -> anyhow::Result<Option<JobInfo>> {
            match &self.accounting {
                Some(a) => Ok(a.get(job_id).cloned()),
                None => anyhow::bail!("no accounting database"),
            }
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
            age_secs: 60,
            updated_secs: 60,
            port_base: 8000,
            ..Default::default()
        }
    }

    fn pending_job(id: &str) -> JobInfo {
        JobInfo {
            job_id: id.into(),
            state: JobState::Pending,
            raw_state: "PENDING".into(),
            ..Default::default()
        }
    }

    fn managed(target: i64) -> ManagedModelSpec {
        ManagedModelSpec {
            model_id: uuid::Uuid::new_v4(),
            launch_command: "serve".into(),
            target_replicas: target,
            ..Default::default()
        }
    }

    async fn run_tick_with(
        slurm: &FakeSlurm,
        obleth: &MockObleth,
        mem: &mut Memory,
    ) -> anyhow::Result<Vec<String>> {
        let resolver = resolve::HostResolver::new(Duration::from_secs(300));
        tick(
            &cfg(),
            slurm,
            obleth,
            &reqwest::Client::new(),
            &resolver,
            mem,
        )
        .await
    }

    async fn run_tick(slurm: &FakeSlurm, obleth: &MockObleth) -> anyhow::Result<Vec<String>> {
        run_tick_with(slurm, obleth, &mut Memory::default()).await
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
    async fn a_vanished_fleet_is_held_with_a_warning_when_accounting_cannot_confirm() {
        // Two live replicas, Slurm answers "no such job" for both and there is
        // no accounting database: far more likely a wrong cluster/version
        // than a fleet that died in one tick.
        let slurm = FakeSlurm::default();
        let obleth = MockObleth::default();
        *obleth.replicas.lock().unwrap() = vec![replica("1", "healthy"), replica("2", "pending")];
        let warnings = run_tick(&slurm, &obleth).await.unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("no longer knows any of the 2 jobs")),
            "{warnings:?}"
        );
        nothing_destructive(&slurm, &obleth);
    }

    #[tokio::test]
    async fn a_vanished_fleet_that_accounting_shows_ended_is_marked_lost() {
        let ended = |id: &str| JobInfo {
            job_id: id.into(),
            state: JobState::Gone,
            raw_state: "TIMEOUT".into(),
            ..Default::default()
        };
        let slurm = FakeSlurm {
            accounting: Some(HashMap::from([
                ("1".to_string(), ended("1")),
                ("2".to_string(), ended("2")),
            ])),
            ..Default::default()
        };
        let obleth = MockObleth::default();
        *obleth.replicas.lock().unwrap() = vec![replica("1", "healthy"), replica("2", "pending")];
        let warnings = run_tick(&slurm, &obleth).await.unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        let lost = obleth
            .patched
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, s)| s.as_deref() == Some("lost"))
            .count();
        assert_eq!(lost, 2);
    }

    #[tokio::test]
    async fn a_stopped_replica_is_let_go_even_when_its_job_vanished() {
        let slurm = FakeSlurm::default();
        let obleth = MockObleth::default();
        let mut stopped = replica("1", "healthy");
        stopped.cancel_requested = true;
        let id = stopped.id;
        *obleth.replicas.lock().unwrap() = vec![stopped, replica("2", "pending")];
        let mut slurm = slurm;
        slurm.jobs.insert("2".into(), pending_job("2"));
        run_tick(&slurm, &obleth).await.unwrap();
        assert!(obleth
            .patched
            .lock()
            .unwrap()
            .iter()
            .any(|(i, s)| *i == id && s.as_deref() == Some("lost")));
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
        old_lost.updated_secs = 10_000;
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

    #[tokio::test]
    async fn a_submit_without_an_answer_holds_submits_and_adopts_the_job_slurm_has() {
        let slurm = FakeSlurm::default();
        slurm.submit_gets_no_answer.store(true, Ordering::SeqCst);
        let obleth = MockObleth::default();
        let spec = managed(1);
        let model_id = spec.model_id;
        *obleth.managed.lock().unwrap() = vec![spec];
        let mut mem = Memory::default();

        let warnings = run_tick_with(&slurm, &obleth, &mut mem).await.unwrap();
        assert_eq!(slurm.submitted.lock().unwrap().len(), 1);
        assert!(
            warnings
                .iter()
                .any(|w| w.starts_with("A job submit failed")),
            "{warnings:?}"
        );
        assert!(mem.uncertain.contains_key(&model_id));

        // Next tick: Slurm's accounting has not seen it yet, so nothing is
        // submitted again.
        slurm.submit_gets_no_answer.store(false, Ordering::SeqCst);
        let warnings = run_tick_with(&slurm, &obleth, &mut mem).await.unwrap();
        assert_eq!(slurm.submitted.lock().unwrap().len(), 1, "held");
        assert!(
            warnings.iter().any(|w| w.contains("got no answer")),
            "{warnings:?}"
        );

        // Then it shows up: the job is tracked instead of submitted twice.
        slurm.by_name.lock().unwrap().push(JobInfo {
            job_id: "4242".into(),
            state: JobState::Pending,
            ..Default::default()
        });
        run_tick_with(&slurm, &obleth, &mut mem).await.unwrap();
        assert_eq!(slurm.submitted.lock().unwrap().len(), 1, "no second job");
        let created = obleth.created_replicas.lock().unwrap();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].1, "4242");
        assert!(!mem.uncertain.contains_key(&model_id));
    }

    #[tokio::test]
    async fn refused_cancels_surface_as_tick_warnings() {
        let mut slurm = FakeSlurm::default();
        slurm.jobs.insert("1".into(), pending_job("1"));
        slurm.cancel_refused.store(true, Ordering::SeqCst);
        let obleth = MockObleth::default();
        // A pending replica of a model that left the managed set drains at once.
        *obleth.replicas.lock().unwrap() = vec![replica("1", "pending")];
        let warnings = run_tick(&slurm, &obleth).await.unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.starts_with("A job cancel failed: Slurm refused cancelling job 1")),
            "{warnings:?}"
        );
        // The replica is out of service and draining, so the cancel is retried.
        assert!(obleth
            .patched
            .lock()
            .unwrap()
            .iter()
            .any(|(_, s)| s.as_deref() == Some("draining")));
    }

    #[tokio::test]
    async fn the_provisioner_losing_every_node_holds_self_heal() {
        // Two healthy replicas whose servers nobody can reach (a closed port
        // on localhost): the fleet guard holds restarts and says so.
        let running = |id: &str| JobInfo {
            job_id: id.into(),
            state: JobState::Running,
            raw_state: "RUNNING".into(),
            nodes: vec!["127.0.0.1".into()],
            reported_port: Some(1),
            ..Default::default()
        };
        let mut slurm = FakeSlurm::default();
        slurm.jobs.insert("1".into(), running("1"));
        slurm.jobs.insert("2".into(), running("2"));
        let obleth = MockObleth::default();
        let spec = managed(2);
        let mut a = replica("1", "healthy");
        let mut b = replica("2", "healthy");
        a.model_id = spec.model_id;
        b.model_id = spec.model_id;
        *obleth.managed.lock().unwrap() = vec![spec];
        *obleth.replicas.lock().unwrap() = vec![a, b];
        let mut mem = Memory::default();
        let warnings = run_tick_with(&slurm, &obleth, &mut mem).await.unwrap();
        assert!(
            warnings
                .iter()
                .any(|w| w.contains("could not reach any of the 2 running model servers")),
            "{warnings:?}"
        );
        assert!(mem.probe_failures.is_empty(), "streaks do not advance");
        assert!(slurm.cancelled.lock().unwrap().is_empty());
    }

    #[test]
    fn action_failures_read_as_sentences() {
        let mut f = ActionFailures::default();
        f.add(
            "job cancel",
            "Slurm refused cancelling job 1: denied.".into(),
        );
        f.add(
            "job cancel",
            "Slurm refused cancelling job 2: denied.".into(),
        );
        f.add("replica update", "admin API timed out".into());
        assert_eq!(
            f.sentences(),
            vec![
                "2 job cancels failed. The first: Slurm refused cancelling job 1: denied.",
                "A replica update failed: admin API timed out.",
            ]
        );
    }

    #[test]
    fn wait_clocks_start_on_first_sight_and_stop_when_cleared() {
        let mut since = HashMap::new();
        let mut r = replica("1", "healthy");
        r.replace_requested = true;
        let t0 = chrono::Utc::now();
        let heal = plan::SelfHeal::default();
        let w = waited_secs(&mut since, &[r.clone()], &HashMap::new(), &heal, t0);
        assert_eq!(w.get(&r.id), Some(&0));
        let w = waited_secs(
            &mut since,
            &[r.clone()],
            &HashMap::new(),
            &heal,
            t0 + chrono::Duration::seconds(90),
        );
        assert_eq!(w.get(&r.id), Some(&90));
        r.replace_requested = false;
        waited_secs(&mut since, &[r.clone()], &HashMap::new(), &heal, t0);
        assert!(since.is_empty());
    }
}
