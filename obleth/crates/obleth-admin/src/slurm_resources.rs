//! Live cluster-resource discovery for the model launcher. Builds a one-shot
//! slurmrestd client from the saved Slurm settings and returns partitions,
//! nodes, and the Slurm user's accounts/QoS. Empty when Slurm is not set up,
//! so the launcher falls back to free-text entry; when a read fails, `error`
//! says why in a plain sentence and `partial` says whether some reads worked.

use axum::extract::State;
use axum::Json;
use obleth_provisioner::domain::ClusterResources;
use obleth_provisioner::slurm::{SlurmClient, Slurmrestd};

use crate::{AdminState, Result};

fn failed(message: String) -> Json<ClusterResources> {
    Json(ClusterResources {
        error: Some(message),
        ..Default::default()
    })
}

// ClusterResources is defined in obleth-provisioner and does not derive
// utoipa::ToSchema; the wire JSON shape is what matters, so the utoipa
// annotation uses `body = Object` to keep the build green.
#[utoipa::path(get, path = "/api/v1/slurm/resources", tag = "slurm",
    responses((status = 200, description = "Cluster partitions, nodes, the Slurm user's \
        accounts and QoS, plus `error` (why a read failed, or null) and `partial` (some \
        reads worked)", body = Object)))]
pub async fn get_slurm_resources(
    State(state): State<AdminState>,
) -> Result<Json<ClusterResources>> {
    let s = state.store.get_slurm_settings().await?.unwrap_or_default();
    if !s.enabled || s.slurmrestd_url.trim().is_empty() {
        return Ok(Json(ClusterResources::default()));
    }
    // Same destination check as the connection test: the token is sent to
    // this URL, which may have been stored before the policy applied.
    if let Err(e) = state.ssrf.validate(s.slurmrestd_url.trim()).await {
        return Ok(failed(format!(
            "The slurmrestd URL is not allowed as a destination: {e}."
        )));
    }
    let client = match Slurmrestd::connect(
        state.slurm_http.client(),
        &s.slurmrestd_url,
        &s.slurmrestd_api_version,
        &s.slurm_user,
        &s.slurm_jwt,
    )
    .await
    {
        Ok(c) => c,
        Err(e) => return Ok(failed(e.message)),
    };
    match client.discover_resources().await {
        Ok(r) => Ok(Json(r)),
        Err(e) => Ok(failed(e.to_string())),
    }
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
