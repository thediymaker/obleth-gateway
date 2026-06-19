use std::collections::HashMap;
use std::fs;

use anyhow::{Context, Result};

use crate::admin::{AdminClient, ModelSpec};
use crate::cli::Scope;
use crate::config::LiveConfig;
use crate::engine::fleet;
use crate::report::out_dir;

#[derive(Clone, Debug)]
pub struct SeededTenant {
    pub name: String,
    pub group: String,
    pub traffic_share: u32,
    pub key: String,
}

#[derive(Clone, Debug)]
pub struct SeededRun {
    pub tenants: Vec<SeededTenant>,
    pub models: Vec<String>,
}

fn key_cache_path() -> std::path::PathBuf { out_dir().join("keys.json") }

fn load_key_cache() -> HashMap<String, String> {
    fs::read_to_string(key_cache_path())
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn save_key_cache(cache: &HashMap<String, String>) -> Result<()> {
    let s = serde_json::to_string_pretty(cache).context("serialize key cache")?;
    let path = key_cache_path();
    write_private(&path, s.as_bytes())
        .with_context(|| format!("write key cache to {}", path.display()))?;
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
        // Tighten the artifact dir to owner-only (defense in depth).
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))?;
    }
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    f.write_all(bytes)
}

#[cfg(not(unix))]
fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, bytes)
}

/// Resolve the secret for a tenant: a freshly minted secret wins; otherwise the
/// cached secret from a prior run; error if neither (the reused key's secret is
/// unknown and was never cached).
fn resolve_key(cache: &mut HashMap<String, String>, tenant: &str, minted: Option<String>) -> Result<String> {
    if let Some(secret) = minted {
        cache.insert(tenant.to_string(), secret.clone());
        return Ok(secret);
    }
    cache.get(tenant).cloned().with_context(|| {
        format!("reused key for tenant '{tenant}' has no cached secret in keys.json — delete that tenant's API key in the gateway and re-run to force a fresh mint")
    })
}

pub async fn seed_fixture(admin: &AdminClient, fixture_api_base: &str, scope: &Scope) -> Result<SeededRun> {
    let models: Vec<&str> = match scope {
        Scope::Single(name) => vec![name.as_str()],
        Scope::All => fleet::FIXTURE_MODELS.to_vec(),
    };
    for name in &models {
        admin.ensure_model(&ModelSpec {
            model_name: name.to_string(),
            upstream_model: name.to_string(),
            api_base: fixture_api_base.to_string(),
            api_key: None,
            input_cost_per_token: 0.0,
            output_cost_per_token: 0.0,
            context_window: 8192,
            admission_weight: 100,
        }).await?;
    }
    for (name, weight) in fleet::FIXTURE_GROUPS {
        admin.ensure_group(name, *weight).await?;
    }
    let mut cache = load_key_cache();
    let mut tenants = Vec::new();
    for (name, group, weight, share) in fleet::FIXTURE_TENANTS {
        let id = admin.ensure_tenant(name, *weight, 1_000_000, group).await?;
        let minted = admin.ensure_key(&id, "obench").await?;
        let key = resolve_key(&mut cache, name, minted)?;
        tenants.push(SeededTenant { name: name.to_string(), group: group.to_string(), traffic_share: *share, key });
    }
    save_key_cache(&cache)?;
    Ok(SeededRun { tenants, models: models.iter().map(|m| m.to_string()).collect() })
}

pub async fn seed_live(admin: &AdminClient, cfg: &LiveConfig, scope: &Scope) -> Result<SeededRun> {
    let models: Vec<&crate::config::LiveModel> = match scope {
        Scope::Single(name) => cfg.models.iter().filter(|m| &m.name == name).collect(),
        Scope::All => cfg.models.iter().collect(),
    };
    for m in &models {
        admin.ensure_model(&ModelSpec {
            model_name: m.name.clone(),
            upstream_model: m.upstream_model.clone(),
            api_base: m.api_base.clone(),
            api_key: Some(m.api_key.clone()),
            input_cost_per_token: m.input_cost_per_token,
            output_cost_per_token: m.output_cost_per_token,
            context_window: 8192,
            admission_weight: m.weight,
        }).await?;
    }
    let mut cache = load_key_cache();
    let mut tenants = Vec::new();
    for c in &cfg.clients {
        admin.ensure_group(&c.group, c.weight).await?;
        let id = admin.ensure_tenant(&c.name, c.weight, 1_000_000, &c.group).await?;
        let minted = admin.ensure_key(&id, "obench").await?;
        let key = resolve_key(&mut cache, &c.name, minted)?;
        tenants.push(SeededTenant { name: c.name.clone(), group: c.group.clone(), traffic_share: c.weight, key });
    }
    save_key_cache(&cache)?;
    Ok(SeededRun { tenants, models: models.iter().map(|m| m.name.clone()).collect() })
}
