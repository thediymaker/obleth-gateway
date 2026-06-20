use anyhow::Result;

use crate::admin::{AdminClient, ModelSpec, Teardown};
use crate::cli::Scope;
use crate::config::LiveConfig;
use crate::engine::fleet;

#[derive(Clone, Debug)]
pub struct SeededTenant {
    pub name: String,
    pub traffic_share: u32,
    pub key: String,
}

#[derive(Clone, Debug)]
pub struct SeededRun {
    pub tenants: Vec<SeededTenant>,
    pub models: Vec<String>,
    /// IDs of everything obench created this run, for automatic teardown. API
    /// key secrets live only in `tenants[].key` (in memory) and are never
    /// written to disk.
    pub teardown: Teardown,
}

pub async fn seed_fixture(admin: &AdminClient, fixture_api_base: &str, scope: &Scope) -> Result<SeededRun> {
    let models: Vec<&str> = match scope {
        Scope::Single(name) => vec![name.as_str()],
        Scope::All => fleet::FIXTURE_MODELS.to_vec(),
    };
    let mut teardown = Teardown::default();
    for name in &models {
        let (id, created) = admin.ensure_model(&ModelSpec {
            model_name: name.to_string(),
            upstream_model: name.to_string(),
            api_base: fixture_api_base.to_string(),
            api_key: None,
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            context_window: 8192,
            admission_weight: 100,
        }).await?;
        if created {
            teardown.model_ids.push(id);
        }
    }
    for (name, weight) in fleet::FIXTURE_GROUPS {
        admin.ensure_group(name, *weight).await?;
    }
    let mut tenants = Vec::new();
    for (name, group, weight, share) in fleet::FIXTURE_TENANTS {
        // `tokens_per_minute = 0` means unlimited: the demo is a concurrency /
        // fairshare stress test, so we never want the per-minute token bucket to
        // shed traffic (that would mask the in-flight queueing we're measuring).
        let (id, created) = admin.ensure_tenant(name, *weight, 0, group).await?;
        if created {
            teardown.tenant_ids.push(id.clone());
        }
        let (key_id, secret) = admin.ensure_key(&id, "obench").await?;
        teardown.key_ids.push(key_id);
        tenants.push(SeededTenant { name: name.to_string(), traffic_share: *share, key: secret });
    }
    Ok(SeededRun { tenants, models: models.iter().map(|m| m.to_string()).collect(), teardown })
}

/// Build a `SeededRun` for a *remote* live gateway without any admin access.
///
/// `live` treats the remote obleth instance as a black box: obench does not
/// create models, tenants, or keys — the operator already has real tenant keys
/// on that gateway. Each supplied key becomes a `SeededTenant` (so the load
/// engine rotates across them by weight, driving fairshare contention), and the
/// selected model names become the fleet. Nothing is registered, so `teardown`
/// is empty.
pub fn live_run_from_config(cfg: &LiveConfig, scope: &Scope) -> Result<SeededRun> {
    let models: Vec<String> = match scope {
        Scope::Single(name) => vec![name.clone()],
        Scope::All => cfg.models.clone(),
    };
    let tenants: Vec<SeededTenant> = cfg
        .keys
        .iter()
        .enumerate()
        .map(|(i, k)| SeededTenant {
            name: if k.label.trim().is_empty() {
                format!("tenant-{}", i + 1)
            } else {
                k.label.clone()
            },
            traffic_share: k.weight.max(1),
            key: k.secret.clone(),
        })
        .collect();
    Ok(SeededRun { tenants, models, teardown: Teardown::default() })
}
