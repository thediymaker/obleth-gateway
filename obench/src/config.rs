use serde::{Deserialize, Serialize};

use crate::cli::Scope;

// ── Live = remote obleth, black-box client ──────────────────────────────────
//
// `live` points obench at a *remote obleth gateway* (e.g. https://gateway.example.com).
// obench is a pure client: it never seeds, never uses an admin token, and never
// tears anything down. The operator supplies the proxy URL, the model names to
// drive, and one or more real tenant API keys. Multiple keys = multiple tenants,
// which is what produces fairshare contention on the remote gateway.

/// A real tenant API key for the remote gateway. `weight` shapes how much of the
/// load this tenant generates (so you can drive uneven fairshare contention).
/// `secret` may contain `${VAR}` placeholders in headless config files.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LiveKey {
    #[serde(default)]
    pub label: String,
    #[serde(default = "default_weight")]
    pub weight: u32,
    pub secret: String,
}

/// A headless live run: which remote proxy, which models, and which tenant keys.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
pub struct LiveConfig {
    /// Base URL of the remote obleth proxy (OpenAI-compatible), e.g.
    /// `https://gateway.example.com` or `https://gateway.example.com/v1`.
    pub proxy_url: String,
    /// Model names to drive (must exist on the remote gateway).
    pub models: Vec<String>,
    /// Real tenant API keys minted on the remote gateway.
    pub keys: Vec<LiveKey>,
}

fn default_weight() -> u32 {
    100
}

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

/// Build a `LiveConfig` from the interactive TUI selection: the remote proxy URL,
/// the tenant keys the user entered (each with its own weight), and the model
/// names they picked. Secrets are stored literally here — the user typed the real
/// keys into the TUI, so there is no `${VAR}` expansion.
pub fn live_config_from_selection(
    proxy_url: &str,
    keys: &[LiveKey],
    model_names: &[String],
) -> LiveConfig {
    LiveConfig {
        proxy_url: proxy_url.to_string(),
        models: model_names.to_vec(),
        keys: keys.to_vec(),
    }
}

pub fn validate_live(cfg: &LiveConfig, scope: &Scope) -> Result<(), String> {
    if cfg.proxy_url.trim().is_empty() {
        return Err("live config needs a proxy_url".to_string());
    }
    if cfg.keys.is_empty() {
        return Err("live config needs at least one tenant key".to_string());
    }
    if cfg.keys.iter().any(|k| k.secret.trim().is_empty()) {
        return Err("every tenant key needs a non-empty secret".to_string());
    }
    match scope {
        Scope::All => {
            if cfg.models.is_empty() {
                return Err("live + scope=all needs at least one model to drive".to_string());
            }
        }
        Scope::Single(name) => {
            if !cfg.models.iter().any(|m| m == name) {
                return Err(format!("model {name} not found in live config"));
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
        let raw = r#"{ "proxy_url": "https://gateway.example.com",
            "models": ["gpt-4o", "llama-3"],
            "keys": [ {"label":"a","secret":"${K}"} ] }"#;
        let m = HashMap::from([("K", "kv")]);
        let cfg = load_live_config(raw, &lookup(&m)).unwrap();
        assert_eq!(cfg.keys[0].secret, "kv");
        assert_eq!(cfg.keys[0].weight, 100); // default applied
    }

    #[test]
    fn scope_all_requires_a_model_and_key() {
        let empty = LiveConfig {
            proxy_url: "https://x".into(),
            models: vec![],
            keys: vec![],
        };
        assert!(validate_live(&empty, &Scope::All).is_err());

        let ok = LiveConfig {
            proxy_url: "https://x".into(),
            models: vec!["gpt-4o".into()],
            keys: vec![LiveKey {
                label: "a".into(),
                weight: 100,
                secret: "k".into(),
            }],
        };
        assert!(validate_live(&ok, &Scope::All).is_ok());
    }

    #[test]
    fn scope_single_requires_named_model() {
        let cfg = LiveConfig {
            proxy_url: "https://x".into(),
            models: vec![],
            keys: vec![LiveKey {
                label: "a".into(),
                weight: 100,
                secret: "k".into(),
            }],
        };
        assert!(validate_live(&cfg, &Scope::Single("missing".into())).is_err());
    }
}
