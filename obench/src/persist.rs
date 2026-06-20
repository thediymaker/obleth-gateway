//! Remembered run configuration, written to `./.obench.json` in the working
//! directory so the next run pre-fills the last endpoint, models, and settings.
//!
//! **Tenant key secrets are never persisted**, and neither are the keys
//! themselves: a saved label without its secret is unusable on the next run, so
//! tenant keys are always re-entered fresh rather than restored.

use serde::{Deserialize, Serialize};

const PATH: &str = ".obench.json";

/// The last run's configuration, minus all secrets.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]
pub struct SavedSpec {
    /// "demo" or "live".
    #[serde(default)]
    pub target: String,
    /// Lower-cased profile name, e.g. "heavy".
    #[serde(default)]
    pub profile: String,
    #[serde(default)]
    pub input_tokens: u32,
    /// Parallel workers (closed-loop concurrency). 0 = use profile default.
    #[serde(default)]
    pub conc: u32,
    /// Max completion tokens per request. 0 = use profile default.
    #[serde(default)]
    pub output_tokens: u32,
    // live
    #[serde(default)]
    pub live_url: String,
    #[serde(default)]
    pub live_models: Vec<String>,
    // demo
    #[serde(default)]
    pub fixture_all: bool,
    #[serde(default)]
    pub fixture_model: String,
}

/// Load the saved spec, or a default if the file is missing/unreadable.
pub fn load() -> SavedSpec {
    std::fs::read_to_string(PATH)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

/// Best-effort save. Never fails the run if the file can't be written.
pub fn save(spec: &SavedSpec) {
    if let Ok(s) = serde_json::to_string_pretty(spec) {
        let _ = std::fs::write(PATH, s);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_without_secrets() {
        let spec = SavedSpec {
            target: "live".into(),
            profile: "heavy".into(),
            input_tokens: 256,
            conc: 128,
            output_tokens: 64,
            live_url: "https://gateway.example.com".into(),
            live_models: vec!["gpt-4o".into(), "llama-3".into()],
            fixture_all: false,
            fixture_model: String::new(),
        };
        let json = serde_json::to_string(&spec).unwrap();
        // The serialized form must never contain a "secret" field.
        assert!(!json.contains("secret"));
        let back: SavedSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(spec, back);
    }

    #[test]
    fn missing_fields_default() {
        let back: SavedSpec = serde_json::from_str("{}").unwrap();
        assert_eq!(back, SavedSpec::default());
    }
}
