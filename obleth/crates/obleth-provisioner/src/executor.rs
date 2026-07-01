use crate::domain::*;
use crate::obleth_client::OblethClient;
use crate::slurm::{job_submit_from_spec, SlurmClient};
use obleth_config::ManagedModelSpec;

/// Apply one action. Errors are returned so the loop can log + continue.
///
/// `submit_spec` is the full managed spec, required only for `Submit` (it carries
/// partition/image/launch details). Drain actions (`Cancel`/`MarkLost`/`Delete`)
/// and `Promote` only need `model_id`, so they still run for a model whose spec
/// has been disabled or deleted while replicas are being torn down.
///
/// `port_span` is the per-replica port window width (from config). `port_base` is
/// the disjoint window base reserved for this `Submit` by the caller — reserving
/// it in `main.rs` (rather than recomputing here) keeps multiple `Submit`s in the
/// same tick from all landing on the same base. Ignored for non-`Submit` actions.
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
) -> anyhow::Result<()> {
    match action {
        Action::Submit => {
            let spec = submit_spec.ok_or_else(|| {
                anyhow::anyhow!("Submit requires a managed spec (model {model_id})")
            })?;
            let job = job_submit_from_spec(spec, model_name, job_prefix, port_base, port_span);
            let job_id = match slurm.submit(&job).await {
                Ok(id) => id,
                Err(e) => {
                    // Slurm rejected the submit (e.g. bad account/partition/qos).
                    // Surface it in the dashboard, best-effort, then propagate.
                    let _ = obleth
                        .set_provision_error(model_id, Some(&e.to_string()))
                        .await;
                    return Err(e);
                }
            };
            // Record the replica that tracks this job. If recording fails, the job
            // is already running with nothing pointing at it — and we no longer
            // scan the cluster for orphans — so cancel it (compensating action).
            if let Err(e) = obleth.create_replica(model_id, &job_id, port_base).await {
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
        } => {
            tracing::info!(%replica_id, %api_base, model = model_name, "promoting replica to healthy");
            let name = format!("{job_prefix}{model_name}-{replica_id}");
            // Idempotent: reuse an existing endpoint with this deterministic name
            // (e.g. when a prior tick created it but failed to patch the replica),
            // otherwise create one.
            let existing = obleth.list_endpoints(model_id).await?;
            let ep = match existing.into_iter().find(|(_, n)| n == &name) {
                Some((id, _)) => id,
                None => obleth.create_endpoint(model_id, &name, api_base).await?,
            };
            let node = host_from_api_base(api_base);
            obleth
                .patch_replica(
                    *replica_id,
                    Some("healthy"),
                    Some(&node),
                    Some(ep),
                    Some("promoted"),
                )
                .await?;
        }
        Action::MarkLost {
            replica_id,
            endpoint_id,
        } => {
            if let Some(ep) = endpoint_id {
                // best-effort: a missing endpoint is fine, but log real failures so
                // phantom endpoints don't accumulate unnoticed.
                if let Err(e) = obleth.delete_endpoint(model_id, *ep).await {
                    tracing::warn!(endpoint_id = %ep, error = %e, "failed to deregister endpoint on mark-lost");
                }
            }
            obleth
                .patch_replica(*replica_id, Some("lost"), None, None, Some("job gone"))
                .await?;
        }
        Action::Cancel {
            replica_id,
            job_id,
            endpoint_id,
        } => {
            // Deregister the endpoint first so the proxy stops routing to this
            // backend immediately — otherwise it lingers in rotation (still
            // "healthy") until the job goes fully Gone a tick later, and requests
            // hit the dying backend (502). Best-effort: a missing endpoint is fine.
            if let Some(ep) = endpoint_id {
                if let Err(e) = obleth.delete_endpoint(model_id, *ep).await {
                    tracing::warn!(endpoint_id = %ep, error = %e, "failed to deregister endpoint on cancel");
                }
            }
            slurm.cancel(job_id).await?;
            obleth
                .patch_replica(
                    *replica_id,
                    Some("draining"),
                    None,
                    None,
                    Some("scaled down"),
                )
                .await?;
        }
        Action::Delete { replica_id } => {
            obleth.delete_replica(*replica_id).await?;
        }
    }
    Ok(())
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
        async fn submit(&self, job: &JobSubmit) -> anyhow::Result<String> {
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
            });
            Ok(job_id)
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
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
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
        )
        .await;
        assert!(err.is_err(), "Submit must fail without a spec");
        assert_eq!(slurm.submitted.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn promote_registers_endpoint_and_marks_healthy() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let s = spec();
        let rid = Uuid::new_v4();
        apply(
            &Action::Promote {
                replica_id: rid,
                api_base: "http://gpu7:8000".into(),
            },
            s.model_id,
            "nemotron",
            Some(&s),
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
        )
        .await
        .unwrap();
        assert_eq!(obleth.created_endpoints.lock().unwrap().len(), 1);
        let patched = obleth.patched.lock().unwrap();
        assert!(patched
            .iter()
            .any(|p| p.0 == rid && p.1.as_deref() == Some("healthy")));
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
            },
            Uuid::new_v4(),
            "nemotron",
            None,
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
        )
        .await
        .unwrap();
        assert!(obleth.deleted_endpoints.lock().unwrap().contains(&ep));
        let patched = obleth.patched.lock().unwrap();
        assert!(patched
            .iter()
            .any(|p| p.0 == rid && p.1.as_deref() == Some("lost")));
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
            },
            Uuid::new_v4(),
            "nemotron",
            None,
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
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
            },
            Uuid::new_v4(),
            "nemotron",
            None,
            "obleth-",
            8,
            8000,
            &slurm,
            &obleth,
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
        )
        .await
        .unwrap();
        let perr = obleth.provision_errors.lock().unwrap();
        assert!(
            perr.iter().any(|(id, e)| *id == s.model_id && e.is_none()),
            "clears on success"
        );
    }
}
