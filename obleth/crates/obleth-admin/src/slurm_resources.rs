//! Live cluster-resource discovery for the model launcher. Builds a one-shot
//! slurmrestd client from the saved Slurm settings and returns partitions,
//! nodes, and the caller's accounts/QoS. Empty (not an error) when Slurm is
//! unconfigured, so the launcher silently falls back to free-text entry.

use axum::extract::State;
use axum::Json;
use obleth_provisioner::domain::ClusterResources;
use obleth_provisioner::slurm::{SlurmClient, Slurmrestd};

use crate::{AdminState, Result};

// ClusterResources is defined in obleth-provisioner and does not derive
// utoipa::ToSchema; the wire JSON shape is what matters, so the utoipa
// annotation uses `body = Object` to keep the build green.
#[utoipa::path(get, path = "/api/v1/slurm/resources", tag = "slurm",
    responses((status = 200, description = "Cluster partitions, nodes, accounts, and QoS", body = Object)))]
pub async fn get_slurm_resources(
    State(state): State<AdminState>,
) -> Result<Json<ClusterResources>> {
    let s = state.store.get_slurm_settings().await?.unwrap_or_default();
    if !s.enabled || s.slurmrestd_url.trim().is_empty() {
        return Ok(Json(ClusterResources::default()));
    }
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|e| crate::AdminError::Internal(e.to_string()))?;
    let client = Slurmrestd::new(
        http,
        &s.slurmrestd_url,
        &s.slurmrestd_api_version,
        &s.slurm_user,
        &s.slurm_jwt,
    );
    // Discovery is best-effort: surface an empty set rather than a 5xx so the
    // launcher stays usable when the cluster read partially fails.
    let resources = client.discover_resources().await.unwrap_or_default();
    Ok(Json(resources))
}

#[cfg(test)]
mod tests {
    use obleth_config::SlurmSettings;
    #[test]
    fn disabled_settings_yield_empty_without_network() {
        let s = SlurmSettings::default();
        assert!(!s.enabled);
        // The handler's guard returns ClusterResources::default() for this case;
        // this asserts the precondition the guard checks.
        assert!(s.slurmrestd_url.trim().is_empty());
    }
}
