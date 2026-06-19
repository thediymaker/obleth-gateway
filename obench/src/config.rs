use serde::Deserialize;

use crate::cli::Scope;

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct LiveModel {
    pub name: String,
    pub upstream_model: String,
    pub api_base: String,
    pub api_key: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    #[serde(default)]
    pub input_cost_per_token: f64,
    #[serde(default)]
    pub output_cost_per_token: f64,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct LiveClient {
    pub name: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    pub group: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
pub struct LiveConfig {
    pub models: Vec<LiveModel>,
    pub clients: Vec<LiveClient>,
}

fn default_weight() -> u32 { 100 }

/// Replace every `${VAR}` with its looked-up value. A missing var is a hard
/// error — never a silent blank.
pub fn interpolate_env(
    s: &str,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after
            .find('}')
            .ok_or_else(|| format!("unterminated ${{ in config value: {s}"))?;
        let var = &after[..end];
        let val = lookup(var).ok_or_else(|| format!("environment variable {var} is not set"))?;
        out.push_str(&val);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

pub fn load_live_config(
    raw: &str,
    lookup: &impl Fn(&str) -> Option<String>,
) -> Result<LiveConfig, String> {
    let interpolated = interpolate_env(raw, lookup)?;
    serde_json::from_str(&interpolated).map_err(|e| format!("invalid live config: {e}"))
}

pub fn validate_live(cfg: &LiveConfig, scope: &Scope) -> Result<(), String> {
    match scope {
        Scope::All => {
            if cfg.models.len() < 2 || cfg.clients.len() < 2 {
                return Err(
                    "live + scope=all needs >=2 models and >=2 clients so load spreads \
                     across upstreams and distinct tenants"
                        .to_string(),
                );
            }
        }
        Scope::Single(name) => {
            if !cfg.models.iter().any(|m| &m.name == name) {
                return Err(format!("model {name} not found in live config"));
            }
            if cfg.clients.is_empty() {
                return Err("live config needs at least one client".to_string());
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup<'a>(map: &'a HashMap<&'a str, &'a str>) -> impl Fn(&str) -> Option<String> + 'a {
        move |k| map.get(k).map(|v| v.to_string())
    }

    #[test]
    fn interpolates_known_var() {
        let m = HashMap::from([("API_KEY_A", "secret123")]);
        let out = interpolate_env("Bearer ${API_KEY_A}", &lookup(&m)).unwrap();
        assert_eq!(out, "Bearer secret123");
    }

    #[test]
    fn missing_var_is_hard_error() {
        let m = HashMap::new();
        let err = interpolate_env("${NOPE}", &lookup(&m)).unwrap_err();
        assert!(err.contains("NOPE"));
    }

    #[test]
    fn loads_config_and_substitutes() {
        let raw = r#"{ "models": [
            {"name":"a","upstream_model":"a","api_base":"http://x","api_key":"${K}"}
          ], "clients": [ {"name":"c","group":"g"} ] }"#;
        let m = HashMap::from([("K", "kv")]);
        let cfg = load_live_config(raw, &lookup(&m)).unwrap();
        assert_eq!(cfg.models[0].api_key, "kv");
        assert_eq!(cfg.models[0].weight, 100); // default applied
    }

    #[test]
    fn scope_all_requires_two_of_each() {
        let cfg = LiveConfig {
            models: vec![LiveModel {
                name: "a".into(), upstream_model: "a".into(), api_base: "x".into(),
                api_key: "k".into(), weight: 100, input_cost_per_token: 0.0, output_cost_per_token: 0.0,
            }],
            clients: vec![LiveClient { name: "c".into(), weight: 100, group: "g".into() }],
        };
        assert!(validate_live(&cfg, &Scope::All).is_err());
    }

    #[test]
    fn scope_single_requires_named_model() {
        let cfg = LiveConfig { models: vec![], clients: vec![] };
        assert!(validate_live(&cfg, &Scope::Single("missing".into())).is_err());
    }
}
