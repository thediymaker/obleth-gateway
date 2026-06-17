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
pub async fn apply(
    action: &Action,
    model_id: uuid::Uuid,
    model_name: &str,
    submit_spec: Option<&ManagedModelSpec>,
    job_prefix: &str,
    slurm: &dyn SlurmClient,
    obleth: &dyn OblethClient,
) -> anyhow::Result<()> {
    match action {
        Action::Submit => {
            let spec = submit_spec.ok_or_else(|| {
                anyhow::anyhow!("Submit requires a managed spec but none was provided (model {model_id})")
            })?;
            let job = job_submit_from_spec(spec, model_name, job_prefix);
            let job_id = slurm.submit(&job).await?;
            obleth.create_replica(model_id, &job_id).await?;
        }
        Action::Promote { replica_id, api_base } => {
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
            obleth.patch_replica(*replica_id, Some("healthy"), Some(&node), Some(ep), Some("promoted")).await?;
        }
        Action::MarkLost { replica_id, endpoint_id } => {
            if let Some(ep) = endpoint_id {
                // best-effort: a missing endpoint is fine, but log real failures so
                // phantom endpoints don't accumulate unnoticed.
                if let Err(e) = obleth.delete_endpoint(model_id, *ep).await {
                    tracing::warn!(endpoint_id = %ep, error = %e, "failed to deregister endpoint on mark-lost");
                }
            }
            obleth.patch_replica(*replica_id, Some("lost"), None, None, Some("job gone")).await?;
        }
        Action::Cancel { replica_id, job_id } => {
            slurm.cancel(job_id).await?;
            obleth.patch_replica(*replica_id, Some("draining"), None, None, Some("scaled down")).await?;
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
    use crate::slurm::MockSlurm;
    use std::sync::{atomic::AtomicU64, Mutex};
    use uuid::Uuid;

    fn spec() -> ManagedModelSpec {
        ManagedModelSpec {
            model_id: Uuid::new_v4(),
            enabled: true,
            partition: "gpu-preempt".into(),
            gres: "gpu:h100:2".into(),
            nodes: 1,
            constraints: None, exclude: None, account: None, qos: None,
            time_limit: Some("12:00:00".into()),
            image: "vllm.sif".into(),
            preamble: String::new(),
            log_output_dir: String::new(),
            launch_command: "vllm serve nemotron --port 8000".into(),
            serving_port: 8000,
            health_path: "/health".into(),
            target_replicas: 2,
            max_job_failures: 0,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        }
    }

    fn mock_slurm() -> MockSlurm {
        MockSlurm { jobs: Mutex::new(vec![]), submitted: Mutex::new(vec![]), cancelled: Mutex::new(vec![]), next_id: AtomicU64::new(1) }
    }

    #[tokio::test]
    async fn submit_creates_job_then_replica() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let s = spec();
        apply(&Action::Submit, s.model_id, "nemotron", Some(&s), "obleth-", &slurm, &obleth).await.unwrap();
        assert_eq!(slurm.submitted.lock().unwrap().len(), 1);
        assert_eq!(obleth.created_replicas.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn submit_without_spec_errors() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let err = apply(&Action::Submit, Uuid::new_v4(), "nemotron", None, "obleth-", &slurm, &obleth).await;
        assert!(err.is_err(), "Submit must fail without a spec");
        assert_eq!(slurm.submitted.lock().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn promote_registers_endpoint_and_marks_healthy() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let s = spec();
        let rid = Uuid::new_v4();
        apply(&Action::Promote { replica_id: rid, api_base: "http://gpu7:8000".into() },
              s.model_id, "nemotron", Some(&s), "obleth-", &slurm, &obleth).await.unwrap();
        assert_eq!(obleth.created_endpoints.lock().unwrap().len(), 1);
        let patched = obleth.patched.lock().unwrap();
        assert!(patched.iter().any(|p| p.0 == rid && p.1.as_deref() == Some("healthy")));
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
        apply(&Action::MarkLost { replica_id: rid, endpoint_id: Some(ep) },
              Uuid::new_v4(), "nemotron", None, "obleth-", &slurm, &obleth).await.unwrap();
        assert!(obleth.deleted_endpoints.lock().unwrap().contains(&ep));
        let patched = obleth.patched.lock().unwrap();
        assert!(patched.iter().any(|p| p.0 == rid && p.1.as_deref() == Some("lost")));
    }

    #[tokio::test]
    async fn cancel_cancels_job_then_marks_draining() {
        let slurm = mock_slurm();
        let obleth = MockObleth::default();
        let rid = Uuid::new_v4();
        // No spec needed for drain actions.
        apply(&Action::Cancel { replica_id: rid, job_id: "j9".into() },
              Uuid::new_v4(), "nemotron", None, "obleth-", &slurm, &obleth).await.unwrap();
        assert!(slurm.cancelled.lock().unwrap().contains(&"j9".to_string()));
        let patched = obleth.patched.lock().unwrap();
        assert!(patched.iter().any(|p| p.0 == rid && p.1.as_deref() == Some("draining")));
    }
}
