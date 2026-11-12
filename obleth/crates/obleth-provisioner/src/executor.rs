use crate::domain::*;
use crate::obleth_client::{OblethClient, ReplicaDetails};
use crate::resolve::HostResolver;
use crate::slurm::{job_submit_from_spec, SlurmClient};
use obleth_config::ManagedModelSpec;

/// Apply one action.
///
/// `Ok(None)`: it applied. `Ok(Some(problem))`: its main effect applied but a
/// follow-up step failed (a cancel Slurm refused after the replica was taken
/// out of service, which the drain retries); the caller reports it. `Err`:
/// nothing that matters changed, and the next tick plans it again.
///
/// `submit_spec` is the full managed spec, required only for `Submit` (it carries
/// partition/image/launch details). Drain actions and `Promote` only need
/// `model_id`, so they still run for a model whose spec has been disabled or
/// deleted while replicas are being torn down.
///
/// `port_span` is the per-replica port window width (from config). `port_base` is
/// the disjoint window base reserved for this `Submit` by the caller — reserving
/// it in `main.rs` (rather than recomputing here) keeps multiple `Submit`s in the
/// same tick, and every model's replicas, on windows of their own. Ignored for
/// non-`Submit` actions.
#[allow(clippy::too_many_arguments)]
pub async fn apply(
    action: &Action,
    model_id: uuid::Uuid,
    model_name: &str,
    submit_spec: Option<&ManagedModelSpec>,
    job_prefix: &str,
    port_span: i64,
    port_base: i64,
    slurm: &dyn SlurmClient,
    obleth: &dyn OblethClient,
    resolver: &HostResolver,
) -> anyhow::Result<Option<String>> {
    match action {
        Action::Submit => {
            let spec = submit_spec.ok_or_else(|| {
                anyhow::anyhow!("Submit requires a managed spec (model {model_id})")
            })?;
            let job = job_submit_from_spec(spec, model_name, job_prefix, port_base, port_span);
            let job_id = match slurm.submit(&job).await {
                Ok(submitted) => {
                    for note in &submitted.notes {
                        tracing::info!(job_id = %submitted.job_id, model = model_name, %note,
                            "slurm accepted the job with a note");
                    }
                    submitted.job_id
                }
                Err(e) => {
                    // Slurm rejected the submit (e.g. bad account/partition/qos),
                    // or no answer came back. Surface it in the dashboard,
                    // best-effort, then propagate.
                    let _ = obleth
                        .set_provision_error(model_id, Some(&e.to_string()))
                        .await;
                    return Err(e);
                }
            };
            // Record the replica that tracks this job. If recording fails, the job
            // is already running with nothing pointing at it — and we never list
            // the cluster for orphans — so cancel it (compensating action).
            let log_path = crate::slurm::job_log_path(&job, &job_id);
            if let Err(e) = obleth
                .create_replica_with_log(model_id, &job_id, port_base, Some(&log_path))
                .await
            {
                match slurm.cancel(&job_id).await {
                    Ok(()) => tracing::warn!(%job_id, model = model_name,
                        "cancelled just-submitted job after replica record failed"),
                    Err(ce) => tracing::error!(%job_id, model = model_name, error = %ce,
                        "replica record failed AND orphan cancel failed; manual cleanup may be needed"),
                }
                return Err(e);
            }
            // Submitted and recorded: clear any prior provisioning error.
            let _ = obleth.set_provision_error(model_id, None).await;
        }
        Action::Promote {
            replica_id,
            api_base,
            port,
            job_started_at,
            job_ends_at,
        } => {
            // Register the endpoint by resolved IP so neither the gateway's
            // health check nor the data-plane proxy has to resolve the node name
            // per request — that per-request DNS is exactly what turns a flaky
            // resolver into intermittent 502s. The replica keeps the hostname for
            // display. Resolution failure falls back to the name (prior behaviour).
            let node = host_from_api_base(api_base);
            let ip_api_base = resolver.resolve_url_host(api_base).await;
            tracing::info!(%replica_id, %ip_api_base, node = %node, model = model_name, "promoting replica to healthy");
            let name = endpoint_name(job_prefix, model_name, *replica_id);
            // Idempotent: reuse an existing endpoint with this deterministic name
            // (e.g. when a prior tick created it but failed to patch the replica),
            // otherwise create one. If the existing one still points at the node
            // name (or a stale IP), migrate it to the resolved address in place.
            let existing = obleth.list_endpoints(model_id).await?;
            let ep = match existing.into_iter().find(|e| e.name == name) {
                Some(e) => {
                    if e.api_base != ip_api_base {
                        obleth
                            .update_endpoint_api_base(model_id, &e, &ip_api_base)
                            .await?;
                    }
                    e.id
                }
                None => {
                    obleth
                        .create_endpoint(model_id, &name, &ip_api_base)
                        .await?
                }
            };
            let details = ReplicaDetails {
                job_started_at: *job_started_at,
                job_ends_at: *job_ends_at,
                port: Some(*port),
                state_detail: Some(String::new()),
                ..Default::default()
            };
            obleth
                .patch_replica_with_details(
                    *replica_id,
                    Some("healthy"),
                    Some(&node),
                    Some(ep),
                    Some("promoted"),
                    &details,
                )
                .await?;
        }
        Action::Demote {
            replica_id,
            endpoint_id,
            detail,
        } => {
            // Out of rotation first. A failed delete leaves the replica as it
            // is, so the next tick tries again instead of leaving an endpoint
            // that points at a server that is gone.
            if let Some(ep) = endpoint_id {
                obleth.delete_endpoint(model_id, *ep).await?;
            }
            tracing::warn!(%replica_id, model = model_name, %detail, "replica stopped serving where it was registered");
            let details = ReplicaDetails {
                state_detail: Some(detail.clone()),
                ..Default::default()
            };
            obleth
                .patch_replica_with_details(
                    *replica_id,
                    Some("pending"),
                    None,
                    None,
                    Some("requeued or restarted by Slurm"),
                    &details,
                )
                .await?;
        }
        Action::MarkLost {
            replica_id,
            endpoint_id,
            raw_state,
            reason,
            failed_launch,
        } => {
            if let Some(ep) = endpoint_id {
                obleth.delete_endpoint(model_id, *ep).await?;
            }
            let message = lost_message(raw_state.as_deref(), reason.as_deref());
            let details = ReplicaDetails {
                state_detail: Some(lost_detail(
                    raw_state.as_deref(),
                    reason.as_deref(),
                    *failed_launch,
                )),
                ..Default::default()
            };
            obleth
                .patch_replica_with_details(
                    *replica_id,
                    Some("lost"),
                    None,
                    None,
                    Some(&message),
                    &details,
                )
                .await?;
        }
        Action::Cancel {
            replica_id,
            job_id,
            endpoint_id,
            reason,
            cancel_now,
        } => {
            // Deregister the endpoint first so the proxy stops routing to this
            // backend immediately. If that fails, nothing else happens this
            // tick: cancelling a job that still takes requests would 502 them.
            if let Some(ep) = endpoint_id {
                obleth.delete_endpoint(model_id, *ep).await?;
            }
            if *reason == CancelReason::ProbeFailed {
                tracing::warn!(%replica_id, %job_id, model = model_name,
                    "self-heal: cancelling zombie replica (job RUNNING, probes failing)");
            }
            // A cancel Slurm refuses is retried by the drain after its grace
            // period, so the replica is marked draining either way.
            let cancel_problem = if *cancel_now {
                slurm.cancel(job_id).await.err().map(|e| format!("{e:#}"))
            } else {
                None
            };
            let details = ReplicaDetails {
                state_detail: Some(reason.detail().to_string()),
                ..Default::default()
            };
            obleth
                .patch_replica_with_details(
                    *replica_id,
                    Some("draining"),
                    None,
                    None,
                    Some(reason.message()),
                    &details,
                )
                .await?;
            return Ok(cancel_problem);
        }
        Action::CancelJob { replica_id, job_id } => {
            slurm.cancel(job_id).await?;
            // Recording it restarts the grace clock, so a job that lingers
            // (COMPLETING) is asked again only after another grace period.
            obleth
                .patch_replica(*replica_id, None, None, None, Some("cancelled"))
                .await?;
        }
        Action::Delete { replica_id } => {
            obleth.delete_replica(*replica_id).await?;
        }
        Action::DeleteEndpoint { endpoint_id } => {
            tracing::info!(%endpoint_id, model = model_name, "removing an endpoint whose replica no longer serves");
            obleth.delete_endpoint(model_id, *endpoint_id).await?;
        }
        Action::Annotate {
            replica_id,
            detail,
            job_ends_at,
        } => {
            let details = ReplicaDetails {
                state_detail: detail.clone(),
                job_ends_at: *job_ends_at,
                ..Default::default()
            };
            obleth.patch_replica_details(*replica_id, &details).await?;
        }
    }
    Ok(None)
}

/// The deterministic name of a replica's endpoint.
pub fn endpoint_name(job_prefix: &str, model_name: &str, replica_id: uuid::Uuid) -> String {
    format!("{job_prefix}{model_name}-{replica_id}")
}

/// The dashboard sentence for a replica whose job ended.
fn lost_detail(raw_state: Option<&str>, reason: Option<&str>, failed_launch: bool) -> String {
    let how = match (raw_state, reason) {
        (Some(s), Some(r)) => format!("{s}, {r}"),
        (Some(s), None) => s.to_string(),
        (None, _) => String::new(),
    };
    if failed_launch {
        return if how.is_empty() {
            "The job disappeared from Slurm before the server became healthy. The job's output usually says why.".to_string()
        } else {
            format!("The job ended ({how}) before the server became healthy. The job's output usually says why.")
        };
    }
    match raw_state {
        None => "The job is no longer in Slurm.".to_string(),
        Some("TIMEOUT") => "The job reached its time limit.".to_string(),
        Some("PREEMPTED") => "Slurm preempted the job.".to_string(),
        Some("NODE_FAIL") => "The job's node failed.".to_string(),
        Some("CANCELLED") => "The job was cancelled outside obleth.".to_string(),
        Some(_) => format!("The job ended ({how})."),
    }
}

/// Extract the host from an endpoint's `api_base` URL for display/recording
/// on the replica row. Uses a real URL parser (not naive `split(':')`) so
/// IPv6 hosts like `http://[::1]:8000` resolve correctly instead of yielding
/// a truncated/garbage fragment.
fn host_from_api_base(api_base: &str) -> String {
    reqwest::Url::parse(api_base)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::obleth_client::MockObleth;
    use std::sync::{atomic::AtomicU64, Mutex};
    use uuid::Uuid;

    /// In-memory fake for executor/loop tests — no network.
    struct MockSlurm {
        jobs: Mutex<Vec<JobInfo>>,
        submitted: Mutex<Vec<JobSubmit>>,
        cancelled: Mutex<Vec<String>>,
        next_id: AtomicU64,
        fail_submit: std::sync::atomic::AtomicBool,
    }
    #[async_trait::async_trait]
    impl SlurmClient for MockSlurm {
        async fn submit(&self, job: &JobSubmit) -> anyhow::Result<crate::slurm::SubmittedJob> {
            if self.fail_submit.load(std::sync::atomic::Ordering::SeqCst) {
                anyhow::bail!("simulated submit rejection (error 2045)");
            }
            let id = self
                .next_id
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            self.submitted.lock().unwrap().push(job.clone());
            let job_id = format!("job-{id}");
            self.jobs.lock().unwrap().push(JobInfo {
                job_id: job_id.clone(),
                state: JobState::Pending,
                nodes: vec![],
                raw_state: "PENDING".into(),
                reason: None,
                ..Default::default()
            });
            Ok(crate::slurm::SubmittedJob {
                job_id,
                notes: vec![],
            })
        }
        async fn cancel(&self, job_id: &str) -> anyhow::Result<()> {
            self.cancelled.lock().unwrap().push(job_id.to_string());
            Ok(())
        }
        async fn get_job(&self, job_id: &str) -> anyhow::Result<Option<JobInfo>> {
            Ok(self
                .jobs
                .lock()
                .unwrap()
                .iter()
                .find(|j| j.job_id == job_id)
                .cloned())
        }
        async fn discover_resources(&self) -> anyhow::Result<ClusterResources> {
            Ok(ClusterResources::default())
        }
    }

    fn spec() -> ManagedModelSpec {
        ManagedModelSpec {
            model_id: Uuid::new_v4(),
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
            launch_command: "vllm serve nemotron --port 8000".into(),
            script_body: String::new(),
            serving_port: 8000,
            health_path: "/health".into(),
            target_replicas: 2,
            min_replicas: 1,
            max_job_failures: 0,
            launcher_spec: None,
            last_provision_error: None,
            last_provision_error_at: None,
            ..Default::default()
        }
    }

    fn mock_slurm() -> MockSlurm {
        MockSlurm {
            jobs: Mutex::new(vec![]),
            submitted: Mutex::new(vec![]),
            cancelled: Mutex::new(vec![]),
            next_id: AtomicU64::new(1),
            fail_submit: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// A resolver with no overrides — non-promote actions never call it, and a
    /// bare hostname won't resolve in CI, so promotion falls back to the name.
    fn resolver() -> HostResolver {
        HostResolver::new(std::time::Duration::from_secs(300))
    }

    #[tokio::test]
    async fn submit_creates_job_then_replica() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let s = spec();
        apply(
            &Action::Submit,
            s.model_id,
            "nemotron",
            Some(&s),
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
            &resolver(),
        )
        .await
        .unwrap();
        assert_eq!(slurm.submitted.lock().unwrap().len(), 1);
        assert_eq!(obleth.created_replicas.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn submit_cancels_job_when_replica_record_fails() {
        // The job submits, but recording its replica fails — the executor must
        // cancel the just-submitted job so it doesn't leak, and surface the error.
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        obleth
            .fail_create_replica
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let s = spec();
        let err = apply(
            &Action::Submit,
            s.model_id,
            "nemotron",
            Some(&s),
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
            &resolver(),
        )
        .await;
        assert!(err.is_err(), "Submit must propagate the record failure");
        let submitted = slurm.submitted.lock().unwrap();
        let cancelled = slurm.cancelled.lock().unwrap();
        assert_eq!(submitted.len(), 1, "the job was submitted");
        assert_eq!(cancelled.len(), 1, "the orphan job was cancelled");
        assert_eq!(obleth.created_replicas.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn submit_without_spec_errors() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let err = apply(
            &Action::Submit,
            Uuid::new_v4(),
            "nemotron",
            None,
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
            &resolver(),
        )
        .await;
        assert!(err.is_err(), "Submit must fail without a spec");
        assert_eq!(slurm.submitted.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn promote_registers_endpoint_by_resolved_ip_and_marks_healthy() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let s = spec();
        let rid = Uuid::new_v4();
        // Alias the node so promotion registers the endpoint by IP, not by name —
        // the whole point of the resolver: keep DNS off the proxy's hot path.
        let resolver = HostResolver::new(std::time::Duration::from_secs(300));
        resolver.set_aliases(std::collections::HashMap::from([(
            "gpu7".to_string(),
            "10.9.9.9".to_string(),
        )]));
        apply(
            &Action::Promote {
                replica_id: rid,
                api_base: "http://gpu7:8000/v1".into(),
                port: 8000,
                job_started_at: chrono::DateTime::from_timestamp(1_700_000_000, 0),
                job_ends_at: None,
            },
            s.model_id,
            "nemotron",
            Some(&s),
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
            &resolver,
        )
        .await
        .unwrap();
        let eps = obleth.created_endpoints.lock().unwrap();
        assert_eq!(eps.len(), 1);
        assert_eq!(
            eps[0].2, "http://10.9.9.9:8000/v1",
            "endpoint registered by resolved IP, not hostname"
        );
        let patched = obleth.patched.lock().unwrap();
        assert!(patched
            .iter()
            .any(|p| p.0 == rid && p.1.as_deref() == Some("healthy")));
        // The run it was promoted under and the port are recorded, and any
        // earlier detail is cleared.
        let details = obleth.details.lock().unwrap();
        let (_, d) = details.iter().find(|(id, _)| *id == rid).unwrap();
        assert_eq!(d.port, Some(8000));
        assert_eq!(d.job_started_at.map(|t| t.timestamp()), Some(1_700_000_000));
        assert_eq!(d.state_detail.as_deref(), Some(""));
    }

    #[test]
    fn host_from_api_base_handles_ipv4_and_ipv6_and_hostnames() {
        assert_eq!(host_from_api_base("http://gpu7:8000"), "gpu7");
        assert_eq!(host_from_api_base("http://10.0.0.5:8000"), "10.0.0.5");
        assert_eq!(host_from_api_base("http://[::1]:8000"), "[::1]");
        assert_eq!(host_from_api_base("not a url"), "");
    }

    #[tokio::test]
    async fn mark_lost_deletes_endpoint_then_patches_lost() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let rid = Uuid::new_v4();
        let ep = Uuid::new_v4();
        // No spec needed for drain actions (drives the disabled/deleted path).
        apply(
            &Action::MarkLost {
                replica_id: rid,
                endpoint_id: Some(ep),
                raw_state: None,
                reason: None,
                failed_launch: true,
            },
            Uuid::new_v4(),
            "nemotron",
            None,
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
            &resolver(),
        )
        .await
        .unwrap();
        assert!(obleth.deleted_endpoints.lock().unwrap().contains(&ep));
        let patched = obleth.patched.lock().unwrap();
        assert!(patched
            .iter()
            .any(|p| p.0 == rid && p.1.as_deref() == Some("lost")));
        let patches = obleth.patches.lock().unwrap();
        assert_eq!(patches.last().unwrap().3.as_deref(), Some("job gone"));
    }

    #[tokio::test]
    async fn mark_lost_records_slurm_terminal_state_in_message() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let rid = Uuid::new_v4();
        apply(
            &Action::MarkLost {
                replica_id: rid,
                endpoint_id: None,
                raw_state: Some("OUT_OF_MEMORY".into()),
                reason: Some("OutOfMemory".into()),
                failed_launch: true,
            },
            Uuid::new_v4(),
            "nemotron",
            None,
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
            &resolver(),
        )
        .await
        .unwrap();
        let patches = obleth.patches.lock().unwrap();
        let p = patches.last().unwrap();
        assert_eq!(p.1.as_deref(), Some("lost"));
        assert_eq!(
            p.3.as_deref(),
            Some("job ended: OUT_OF_MEMORY (OutOfMemory)")
        );
    }

    #[tokio::test]
    async fn cancel_deregisters_endpoint_then_cancels_job_and_drains() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let rid = Uuid::new_v4();
        let ep = Uuid::new_v4();
        // No spec needed for drain actions.
        apply(
            &Action::Cancel {
                replica_id: rid,
                job_id: "j9".into(),
                endpoint_id: Some(ep),
                reason: CancelReason::ScaleDown,
                cancel_now: true,
            },
            Uuid::new_v4(),
            "nemotron",
            None,
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
            &resolver(),
        )
        .await
        .unwrap();
        // Endpoint removed from rotation BEFORE the job is cancelled (502 guard).
        assert!(obleth.deleted_endpoints.lock().unwrap().contains(&ep));
        assert!(slurm.cancelled.lock().unwrap().contains(&"j9".to_string()));
        let patched = obleth.patched.lock().unwrap();
        assert!(patched
            .iter()
            .any(|p| p.0 == rid && p.1.as_deref() == Some("draining")));
    }

    #[tokio::test]
    async fn cancel_without_endpoint_still_cancels_and_drains() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let rid = Uuid::new_v4();
        apply(
            &Action::Cancel {
                replica_id: rid,
                job_id: "j9".into(),
                endpoint_id: None,
                reason: CancelReason::ScaleDown,
                cancel_now: true,
            },
            Uuid::new_v4(),
            "nemotron",
            None,
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
            &resolver(),
        )
        .await
        .unwrap();
        assert!(obleth.deleted_endpoints.lock().unwrap().is_empty());
        assert!(slurm.cancelled.lock().unwrap().contains(&"j9".to_string()));
    }

    #[tokio::test]
    async fn submit_records_provision_error_on_rejection() {
        let slurm = mock_slurm();
        slurm
            .fail_submit
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let obleth = MockObleth::default();
        let s = spec();
        let err = apply(
            &Action::Submit,
            s.model_id,
            "nemotron",
            Some(&s),
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
            &resolver(),
        )
        .await;
        assert!(err.is_err(), "submit rejection propagates");
        let perr = obleth.provision_errors.lock().unwrap();
        assert_eq!(perr.len(), 1);
        assert_eq!(perr[0].0, s.model_id);
        assert!(perr[0].1.as_deref().unwrap().contains("2045"));
        assert_eq!(obleth.created_replicas.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn submit_clears_provision_error_on_success() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let s = spec();
        apply(
            &Action::Submit,
            s.model_id,
            "nemotron",
            Some(&s),
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
            &resolver(),
        )
        .await
        .unwrap();
        let perr = obleth.provision_errors.lock().unwrap();
        assert!(
            perr.iter().any(|(id, e)| *id == s.model_id && e.is_none()),
            "clears on success"
        );
    }

    async fn run(
        action: &Action,
        slurm: &MockSlurm,
        obleth: &MockObleth,
    ) -> anyhow::Result<Option<String>> {
        apply(
            action,
            Uuid::new_v4(),
            "nemotron",
            None,
            "obleth-",
            8,
            8000,
            slurm,
            obleth,
            &resolver(),
        )
        .await
    }

    #[tokio::test]
    async fn demote_removes_the_endpoint_and_puts_the_replica_back_to_pending() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let (rid, ep) = (Uuid::new_v4(), Uuid::new_v4());
        let action = Action::Demote {
            replica_id: rid,
            endpoint_id: Some(ep),
            detail: crate::plan::REQUEUED_DETAIL.into(),
        };
        run(&action, &slurm, &obleth).await.unwrap();
        assert_eq!(*obleth.deleted_endpoints.lock().unwrap(), vec![ep]);
        assert!(obleth
            .patched
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.0 == rid && p.1.as_deref() == Some("pending")));
        let details = obleth.details.lock().unwrap();
        assert_eq!(
            details[0].1.state_detail.as_deref(),
            Some(crate::plan::REQUEUED_DETAIL)
        );
    }

    #[tokio::test]
    async fn a_failed_endpoint_delete_changes_nothing_so_the_next_tick_retries() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        obleth
            .fail_delete_endpoint
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let ep = Some(Uuid::new_v4());
        let actions = [
            Action::Demote {
                replica_id: Uuid::new_v4(),
                endpoint_id: ep,
                detail: "x".into(),
            },
            Action::MarkLost {
                replica_id: Uuid::new_v4(),
                endpoint_id: ep,
                raw_state: None,
                reason: None,
                failed_launch: false,
            },
            Action::Cancel {
                replica_id: Uuid::new_v4(),
                job_id: "j1".into(),
                endpoint_id: ep,
                reason: CancelReason::ScaleDown,
                cancel_now: true,
            },
        ];
        for a in &actions {
            assert!(run(a, &slurm, &obleth).await.is_err(), "{a:?}");
        }
        assert!(obleth.patched.lock().unwrap().is_empty());
        assert!(
            slurm.cancelled.lock().unwrap().is_empty(),
            "no cancel while it takes requests"
        );
    }

    #[tokio::test]
    async fn a_graceful_drain_cancels_the_job_only_after_the_grace() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let rid = Uuid::new_v4();
        let drain = Action::Cancel {
            replica_id: rid,
            job_id: "j7".into(),
            endpoint_id: Some(Uuid::new_v4()),
            reason: CancelReason::Replaced,
            cancel_now: false,
        };
        run(&drain, &slurm, &obleth).await.unwrap();
        assert!(slurm.cancelled.lock().unwrap().is_empty());
        assert!(obleth
            .patched
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.0 == rid && p.1.as_deref() == Some("draining")));
        let later = Action::CancelJob {
            replica_id: rid,
            job_id: "j7".into(),
        };
        run(&later, &slurm, &obleth).await.unwrap();
        assert_eq!(*slurm.cancelled.lock().unwrap(), vec!["j7".to_string()]);
    }

    #[test]
    fn lost_details_say_whether_the_launch_failed() {
        assert!(
            lost_detail(Some("FAILED"), Some("NonZeroExitCode"), true).starts_with(
                "The job ended (FAILED, NonZeroExitCode) before the server became healthy."
            )
        );
        assert_eq!(
            lost_detail(Some("TIMEOUT"), None, false),
            "The job reached its time limit."
        );
        assert_eq!(
            lost_detail(None, None, false),
            "The job is no longer in Slurm."
        );
    }
}
