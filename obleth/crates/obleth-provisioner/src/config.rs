//! Env-driven configuration.
//!
//! Only the **Management API endpoint/token** and operational **cadence** knobs
//! come from the environment. The Slurm connection details (slurmrestd URL,
//! version, user, JWT) and the master enable switch now live in system-wide
//! settings, configured from the dashboard and fetched from the Management API
//! each tick (see `obleth_client::get_slurm_settings`). Nothing cluster-specific
//! is compiled in.

#[derive(Debug, Clone)]
pub struct ProvisionerConfig {
    pub admin_base_url: String,
    pub admin_token: String,
    pub interval_secs: u64,
    pub health_timeout_secs: u64,
    pub lost_retention_secs: i64,
    /// Job-name prefix used to tag and later find this gateway's jobs.
    pub job_name_prefix: String,
}

impl ProvisionerConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        fn req(k: &str) -> anyhow::Result<String> {
            std::env::var(k).map_err(|_| anyhow::anyhow!("missing env {k}"))
        }
        fn opt(k: &str, default: &str) -> String {
            std::env::var(k).unwrap_or_else(|_| default.to_string())
        }
        Ok(Self {
            admin_base_url: opt("OBLETH_ADMIN_BASE_URL", "http://localhost:9180"),
            admin_token: req("OBLETH_ADMIN_TOKEN")?,
            interval_secs: opt("OBLETH_PROVISIONER_INTERVAL_SECS", "15").parse()?,
            health_timeout_secs: opt("OBLETH_PROVISIONER_HEALTH_TIMEOUT_SECS", "5").parse()?,
            lost_retention_secs: opt("OBLETH_PROVISIONER_LOST_RETENTION_SECS", "900").parse()?,
            job_name_prefix: opt("OBLETH_PROVISIONER_JOB_PREFIX", "obleth-"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_apply_and_required_are_enforced() {
        // Required var present, optionals absent -> defaults.
        std::env::set_var("OBLETH_ADMIN_TOKEN", "t");
        let c = ProvisionerConfig::from_env().expect("config");
        assert_eq!(c.interval_secs, 15);
        assert_eq!(c.health_timeout_secs, 5);
        assert_eq!(c.lost_retention_secs, 900);
        assert_eq!(c.job_name_prefix, "obleth-");
    }
}
