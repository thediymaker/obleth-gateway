use anyhow::Result;

use crate::admin::{AdminClient, ModelSpec, Teardown};
use crate::cli::Scope;
use crate::config::LiveConfig;
use crate::engine::fleet;

/// One API key minted under a seeded tenant. The secret lives only in memory
/// for the duration of the run.
#[derive(Clone, Debug)]
pub struct SeededKey {
    pub name: String,
    pub secret: String,
}

#[derive(Clone, Debug)]
pub struct SeededTenant {
    pub name: String,
    pub traffic_share: u32,
    pub keys: Vec<SeededKey>,
}

impl SeededTenant {
    /// The secret for consumers that drive a single key per tenant.
    pub fn first_key(&self) -> Result<&str> {
        self.keys.first().map(|k| k.secret.as_str()).ok_or_else(|| {
            anyhow::anyhow!("tenant {} has no API key to drive load with", self.name)
        })
    }
}

/// Which fixture fleet to seed. `Fairshare` is the wider many-pool, many-key
/// fleet the `fairshare` profile needs; everything else uses `Standard`.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum FleetChoice {
    Standard,
    Fairshare,
}

#[derive(Clone, Debug)]
pub struct SeededRun {
    pub tenants: Vec<SeededTenant>,
    pub models: Vec<String>,
    /// IDs of everything obench created this run, for automatic teardown. API
    /// key secrets live only in `tenants[].keys[].secret` (in memory) and are
    /// never written to disk.
    pub teardown: Teardown,
}

/// Normalize an api_base to end in exactly one `/v1`, as the gateway's active
/// health-probe tier assumes (`build_probe_request` in obleth-admin does
/// `{api_base}/chat/completions`, i.e. it expects the base to already include
/// `/v1`). obench's fixture backend is seeded with a bare `http://host:port`
/// base, so without this the active probe 404s against every obench-seeded
/// model. Safe for the request-serving data path too: the proxy's
/// `build_upstream_url` dedupes a doubled `/v1/v1/...` (see
/// obleth-proxy/src/proxy.rs `build_upstream_url` tests), so appending `/v1`
/// here does not change the URL requests actually get routed to.
fn normalize_api_base_v1(base: &str) -> String {
    let trimmed = base.trim_end_matches('/');
    let trimmed = trimmed.strip_suffix("/v1").unwrap_or(trimmed);
    format!("{trimmed}/v1")
}

/// Seed the GPU-free fixture fleet: one model per admission pool, the fairshare
/// groups, the tenants, and every tenant's keys.
///
/// `model_cap` is the per-model pool size pushed onto each seeded model; `None`
/// leaves the gateway default in force.
pub async fn seed_fixture(
    admin: &AdminClient,
    fixture_api_base: &str,
    scope: &Scope,
    fleet_choice: FleetChoice,
    model_cap: Option<u32>,
) -> Result<SeededRun> {
    let fleet_models: &[&str] = match fleet_choice {
        FleetChoice::Standard => fleet::FIXTURE_MODELS,
        FleetChoice::Fairshare => fleet::FAIRSHARE_MODELS,
    };
    let fleet_tenants: &[(&str, &str, u32, u32)] = match fleet_choice {
        FleetChoice::Standard => fleet::FIXTURE_TENANTS,
        FleetChoice::Fairshare => fleet::FAIRSHARE_TENANTS,
    };
    let models: Vec<&str> = match scope {
        Scope::Single(name) => vec![name.as_str()],
        Scope::All => fleet_models.to_vec(),
    };
    let api_base = normalize_api_base_v1(fixture_api_base);
    let mut teardown = Teardown::default();
    for name in &models {
        let (id, created) = admin
            .ensure_model(&ModelSpec {
                model_name: name.to_string(),
                upstream_model: name.to_string(),
                api_base: api_base.clone(),
                api_key: None,
                input_cost_per_token: 0.0,
                output_cost_per_token: 0.0,
                context_window: 8192,
                admission_weight: 100,
                max_in_flight: model_cap,
            })
            .await?;
        if created {
            teardown.model_ids.push(id);
        }
    }
    for (name, weight) in fleet::FIXTURE_GROUPS {
        admin.ensure_group(name, *weight).await?;
    }
    let mut tenants = Vec::new();
    for (i, (name, group, weight, share)) in fleet_tenants.iter().enumerate() {
        // The first fairshare tenant carries a per-model tenant ceiling so the
        // run exercises tenant caps as well as weights.
        let tenant_cap = (fleet_choice == FleetChoice::Fairshare && i == 0).then_some(4);
        // `tokens_per_minute = 0` means unlimited: the demo is a concurrency /
        // fairshare stress test, so we never want the per-minute token bucket to
        // shed traffic (that would mask the in-flight queueing we're measuring).
        let (id, created) = admin
            .ensure_tenant(name, *weight, 0, tenant_cap, group, true)
            .await?;
        if created {
            teardown.tenant_ids.push(id.clone());
        }
        // 3 to 6 keys per fairshare tenant, so the per-key split is measured
        // across differently sized key sets.
        let key_specs: &[(&str, u32, Option<u32>)] = match fleet_choice {
            FleetChoice::Standard => fleet::FIXTURE_KEYS,
            FleetChoice::Fairshare => {
                let n = (3 + (i % 4)).min(fleet::FAIRSHARE_KEYS.len());
                &fleet::FAIRSHARE_KEYS[..n]
            }
        };
        let mut keys = Vec::new();
        for (key_name, key_weight, key_cap) in key_specs {
            let (key_id, secret) = admin
                .ensure_key(&id, key_name, *key_weight, *key_cap)
                .await?;
            teardown.key_ids.push(key_id);
            keys.push(SeededKey {
                name: key_name.to_string(),
                secret,
            });
        }
        tenants.push(SeededTenant {
            name: name.to_string(),
            traffic_share: *share,
            keys,
        });
    }
    Ok(SeededRun {
        tenants,
        models: models.iter().map(|m| m.to_string()).collect(),
        teardown,
    })
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
            keys: vec![SeededKey {
                name: "live".to_string(),
                secret: k.secret.clone(),
            }],
        })
        .collect();
    Ok(SeededRun {
        tenants,
        models,
        teardown: Teardown::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::normalize_api_base_v1;

    #[test]
    fn appends_v1_when_missing() {
        assert_eq!(
            normalize_api_base_v1("http://benchmark-backend:8081"),
            "http://benchmark-backend:8081/v1"
        );
    }

    #[test]
    fn leaves_single_v1_suffix_alone() {
        assert_eq!(
            normalize_api_base_v1("http://benchmark-backend:8081/v1"),
            "http://benchmark-backend:8081/v1"
        );
    }

    #[test]
    fn strips_trailing_slash_before_checking_v1() {
        assert_eq!(
            normalize_api_base_v1("http://benchmark-backend:8081/v1/"),
            "http://benchmark-backend:8081/v1"
        );
        assert_eq!(
            normalize_api_base_v1("http://benchmark-backend:8081/"),
            "http://benchmark-backend:8081/v1"
        );
    }

    #[test]
    fn does_not_touch_v1_in_the_middle_of_the_base() {
        // Only a trailing `/v1` counts as "already normalized" — a base whose
        // path happens to contain "v1" elsewhere must still get one appended.
        assert_eq!(
            normalize_api_base_v1("http://v1proxy.example.com:8081"),
            "http://v1proxy.example.com:8081/v1"
        );
    }
}
