//! Slurm helpers shared by the management API and the provisioner: parsing
//! the values an operator types (walltime, memory, GRES), slurmrestd API
//! version selection, reading a Slurm JWT's claims, and the backoff ladder a
//! failing deployment follows. Pure functions only; nothing here talks to a
//! cluster.

use base64::Engine;

/// The `slurmrestd_api_version` value that means "use the newest version the
/// server offers that this build supports". An empty string means the same.
pub const SLURM_API_VERSION_AUTO: &str = "auto";

/// slurmrestd data-parser versions this build can talk to, oldest first. The
/// job and node fields obleth reads and writes exist in each of them.
pub const SUPPORTED_SLURM_API_VERSIONS: &[&str] = &[
    "v0.0.40", "v0.0.41", "v0.0.42", "v0.0.43", "v0.0.44", "v0.0.45",
];

/// True when the configured version asks for automatic selection.
pub fn is_auto_api_version(v: &str) -> bool {
    let v = v.trim();
    v.is_empty() || v.eq_ignore_ascii_case(SLURM_API_VERSION_AUTO)
}

/// `v0.0.43` as `(0, 0, 43)`. `None` for anything that is not `v` followed by
/// three dot-separated numbers.
pub fn parse_api_version(v: &str) -> Option<(u32, u32, u32)> {
    let rest = v.trim().strip_prefix('v')?;
    let mut parts = rest.split('.');
    let a = parts.next()?.parse().ok()?;
    let b = parts.next()?.parse().ok()?;
    let c = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((a, b, c))
}

/// A value the settings may store: `auto` (or empty), or a version such as
/// `v0.0.43`. The version is interpolated into request paths, so nothing else
/// is accepted.
pub fn is_valid_api_version_setting(v: &str) -> bool {
    is_auto_api_version(v) || parse_api_version(v).is_some()
}

/// The API versions named in an OpenAPI document's path keys
/// (`/slurm/v0.0.43/ping`, `/slurmdb/v0.0.43/jobs`), oldest first, deduplicated.
pub fn api_versions_from_paths<'a>(paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for p in paths {
        let mut segs = p.split('/').filter(|s| !s.is_empty());
        let (Some(plugin), Some(ver)) = (segs.next(), segs.next()) else {
            continue;
        };
        if plugin != "slurm" && plugin != "slurmdb" {
            continue;
        }
        if parse_api_version(ver).is_some() && !out.iter().any(|v| v == ver) {
            out.push(ver.to_string());
        }
    }
    out.sort_by_key(|v| parse_api_version(v));
    out
}

/// The newest version in `available` that this build supports.
pub fn pick_api_version(available: &[String]) -> Option<String> {
    available
        .iter()
        .filter(|v| SUPPORTED_SLURM_API_VERSIONS.contains(&v.as_str()))
        .max_by_key(|v| parse_api_version(v))
        .cloned()
}

/// A job's walltime as slurmrestd takes it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlurmTimeLimit {
    /// Whole minutes.
    Minutes(i64),
    /// `UNLIMITED` / `INFINITE`: no limit beyond the partition's maximum.
    Unlimited,
}

/// Parse a Slurm walltime in any form `sbatch --time` takes: `MM`, `MM:SS`,
/// `HH:MM:SS`, `D-HH`, `D-HH:MM`, `D-HH:MM:SS`, or `UNLIMITED`/`INFINITE`.
/// Seconds round up to the next whole minute. `Ok(None)` for an empty value
/// (the partition's default applies); `Err` holds a plain sentence.
pub fn parse_time_limit(raw: &str) -> Result<Option<SlurmTimeLimit>, String> {
    let s = raw.trim();
    if s.is_empty() {
        return Ok(None);
    }
    if s.eq_ignore_ascii_case("unlimited") || s.eq_ignore_ascii_case("infinite") {
        return Ok(Some(SlurmTimeLimit::Unlimited));
    }
    let bad = || {
        format!(
            "The time limit \"{s}\" is not a Slurm walltime. Use minutes, HH:MM:SS, \
             D-HH:MM:SS or UNLIMITED, for example 0-12:00:00."
        )
    };
    let (days, rest, has_days) = match s.split_once('-') {
        Some((d, rest)) => (
            d.trim().parse::<i64>().map_err(|_| bad())?,
            rest.trim(),
            true,
        ),
        None => (0, s, false),
    };
    let parts: Vec<i64> = rest
        .split(':')
        .map(|p| p.trim().parse::<i64>().ok())
        .collect::<Option<Vec<_>>>()
        .ok_or_else(bad)?;
    if days < 0 || parts.iter().any(|&n| n < 0) {
        return Err(bad());
    }
    // With a day part a single number is hours (`D-HH`); without one it is minutes.
    let (hours, minutes, seconds) = match (has_days, parts.as_slice()) {
        (true, [h]) => (*h, 0, 0),
        (true, [h, m]) => (*h, *m, 0),
        (true, [h, m, sec]) => (*h, *m, *sec),
        (false, [m]) => (0, *m, 0),
        (false, [m, sec]) => (0, *m, *sec),
        (false, [h, m, sec]) => (*h, *m, *sec),
        _ => return Err(bad()),
    };
    let total_secs = ((days * 24 + hours) * 60 + minutes) * 60 + seconds;
    if total_secs <= 0 {
        return Err(format!(
            "The time limit \"{s}\" is zero. Leave it empty to use the partition's default."
        ));
    }
    Ok(Some(SlurmTimeLimit::Minutes((total_secs + 59) / 60)))
}

/// Parse a Slurm memory size (`560G`, `512000M`, `2T`, `16000`, `64GB`, `64GiB`)
/// into whole megabytes. A bare number is megabytes, Slurm's default unit.
/// Zero is kept: like `--mem=0` it asks for all of each node's memory.
/// `None` for an empty or unreadable value.
pub fn parse_mem_mb(raw: &str) -> Option<i64> {
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }
    let upper = s.to_ascii_uppercase();
    // Optional trailing B or IB (GB, GIB, MB, ...); Slurm's units are binary.
    let core = upper
        .strip_suffix("IB")
        .or_else(|| upper.strip_suffix('B'))
        .unwrap_or(&upper);
    let (num_part, mult) = match core.chars().last() {
        Some('K') => (&core[..core.len() - 1], 1.0 / 1024.0),
        Some('M') => (&core[..core.len() - 1], 1.0),
        Some('G') => (&core[..core.len() - 1], 1024.0),
        Some('T') => (&core[..core.len() - 1], 1024.0 * 1024.0),
        Some(c) if c.is_ascii_digit() => (core, 1.0),
        _ => return None,
    };
    let n: f64 = num_part.trim().parse().ok()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    if n == 0.0 {
        return Some(0);
    }
    let mb = (n * mult).round() as i64;
    (mb > 0).then_some(mb)
}

/// One GRES request item without any `gres/` or `gres:` prefix.
fn gres_items(raw: &str) -> impl Iterator<Item = &str> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.strip_prefix("gres/")
                .or_else(|| s.strip_prefix("gres:"))
                .unwrap_or(s)
        })
}

/// Check a GRES request: comma-separated `name[:type]:count` items (a bare
/// `name` means one). `Err` holds a plain sentence.
pub fn validate_gres(raw: &str) -> Result<(), String> {
    for item in gres_items(raw) {
        let parts: Vec<&str> = item.split(':').collect();
        let ok_name = |s: &str| {
            !s.is_empty()
                && s.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
        };
        let ok_count = |s: &str| s.parse::<u64>().is_ok_and(|n| n > 0);
        let ok = match parts.as_slice() {
            [name] => ok_name(name),
            [name, last] => ok_name(name) && (ok_count(last) || ok_name(last)),
            [name, kind, count] => ok_name(name) && ok_name(kind) && ok_count(count),
            _ => false,
        };
        if !ok {
            return Err(format!(
                "The GPU request \"{item}\" is not a Slurm GRES. Write it as name:count or \
                 name:type:count, for example gpu:1 or gpu:a100:2, and separate several with commas."
            ));
        }
    }
    Ok(())
}

/// slurmrestd's `tres_per_node` for a GRES request: each comma-separated item
/// prefixed with `gres/`, as `sbatch --gres` does.
pub fn gres_to_tres_per_node(raw: &str) -> String {
    gres_items(raw)
        .map(|item| format!("gres/{item}"))
        .collect::<Vec<_>>()
        .join(",")
}

/// A Slurm job name for `model_name` behind `prefix`. The name also forms the
/// job's output file name, so anything other than letters, digits, `.`, `_`
/// and `-` becomes `-`.
pub fn slurm_job_name(prefix: &str, model_name: &str) -> String {
    let clean: String = model_name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("{prefix}{clean}")
}

/// The claims of a Slurm JWT that obleth reads. Read without checking the
/// signature: obleth holds the token, not the cluster's signing key, so this
/// is for display and warnings, never for authentication.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SlurmJwtClaims {
    /// Expiry, unix seconds.
    pub exp: Option<i64>,
    /// Issue time, unix seconds.
    pub iat: Option<i64>,
    /// The Slurm user the token was minted for.
    pub sun: Option<String>,
}

/// Decode a JWS-compact token's payload. Tolerates padded or unpadded
/// base64url. `None` when the token is not a readable JWT.
pub fn slurm_jwt_claims(token: &str) -> Option<SlurmJwtClaims> {
    let payload = token.trim().split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    let claims: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let num = |k: &str| {
        claims
            .get(k)
            .and_then(|v| v.as_i64().or_else(|| v.as_f64().map(|f| f as i64)))
    };
    Some(SlurmJwtClaims {
        exp: num("exp"),
        iat: num("iat"),
        sun: claims
            .get("sun")
            .and_then(|v| v.as_str())
            .map(str::to_string)
            .filter(|s| !s.is_empty()),
    })
}

/// A token as pasted, cleaned of what `scontrol token` prints around it:
/// `SLURM_JWT=...`, `export SLURM_JWT=...`, surrounding quotes and whitespace.
pub fn clean_pasted_jwt(raw: &str) -> String {
    let mut s = raw.trim();
    if let Some(rest) = s.strip_prefix("export ") {
        s = rest.trim_start();
    }
    if let Some((name, value)) = s.split_once('=') {
        if name.trim().eq_ignore_ascii_case("SLURM_JWT") {
            s = value.trim();
        }
    }
    s.trim_matches(|c| c == '"' || c == '\'').trim().to_string()
}

/// The pause after the `step`-th failure in a row (1-based): 5, 15, 30 and 60
/// minutes, then hourly. Step 0 or below means no pause.
pub fn backoff_minutes(step: i64) -> i64 {
    match step {
        i64::MIN..=0 => 0,
        1 => 5,
        2 => 15,
        3 => 30,
        _ => 60,
    }
}

/// What the replacement text for a hidden value reads.
pub const HIDDEN_VALUE: &str = "(hidden)";

fn looks_secret(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "key",
        "token",
        "secret",
        "password",
        "passwd",
        "credential",
        "auth",
    ]
    .iter()
    .any(|w| n.contains(w))
}

/// Hide the values of command-line options whose names look like secrets
/// (`--api-key X`, `--hf-token=X`), keeping everything else.
fn hide_secret_flags(text: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut hide_next = false;
    for word in text.split_whitespace() {
        if hide_next {
            out.push(HIDDEN_VALUE.to_string());
            hide_next = false;
            continue;
        }
        if word.starts_with('-') {
            let flag = word.trim_start_matches('-');
            if let Some((name, _)) = flag.split_once('=') {
                if looks_secret(name) {
                    let dashes = &word[..word.len() - flag.len()];
                    out.push(format!("{dashes}{name}={HIDDEN_VALUE}"));
                    continue;
                }
            } else if looks_secret(flag) {
                hide_next = true;
            }
        }
        out.push(word.to_string());
    }
    out.join(" ")
}

/// A copy of a deployment's `launcher_spec` safe to keep in records that
/// outlive the deployment (audit log, launch history): environment values are
/// hidden (names kept), and so are input values whose name looks like a
/// secret or that pass a secret-looking command-line option.
pub fn redact_launcher_spec(spec: &serde_json::Value) -> serde_json::Value {
    let mut out = spec.clone();
    let Some(obj) = out.as_object_mut() else {
        return out;
    };
    if let Some(env) = obj.get_mut("env").and_then(|e| e.as_object_mut()) {
        for v in env.values_mut() {
            *v = serde_json::Value::String(HIDDEN_VALUE.into());
        }
    }
    for key in ["inputs", "variables"] {
        if let Some(inputs) = obj.get_mut(key).and_then(|e| e.as_object_mut()) {
            for (name, v) in inputs.iter_mut() {
                if looks_secret(name) {
                    *v = serde_json::Value::String(HIDDEN_VALUE.into());
                } else if let Some(text) = v.as_str() {
                    let hidden = hide_secret_flags(text);
                    if hidden != text.split_whitespace().collect::<Vec<_>>().join(" ") {
                        *v = serde_json::Value::String(hidden);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_versions_are_read_from_openapi_paths_and_the_newest_supported_wins() {
        let paths = [
            "/slurm/v0.0.43/ping",
            "/slurm/v0.0.42/jobs",
            "/slurmdb/v0.0.44/jobs",
            "/slurm/v0.0.46/ping",
            "/openapi/v3",
            "/slurm/v0.0.43/job/{job_id}",
        ];
        let v = api_versions_from_paths(paths);
        assert_eq!(v, ["v0.0.42", "v0.0.43", "v0.0.44", "v0.0.46"]);
        assert_eq!(pick_api_version(&v).as_deref(), Some("v0.0.44"));
        assert_eq!(pick_api_version(&["v0.0.39".into()]), None);
    }

    #[test]
    fn api_version_settings_accept_auto_or_a_version_only() {
        assert!(is_valid_api_version_setting("auto"));
        assert!(is_valid_api_version_setting(""));
        assert!(is_valid_api_version_setting("v0.0.43"));
        assert!(!is_valid_api_version_setting("0.0.43"));
        assert!(!is_valid_api_version_setting("latest"));
        assert!(!is_valid_api_version_setting("v0.0.43/jobs"));
        assert!(!is_valid_api_version_setting("../admin"));
        assert_eq!(parse_api_version("v0.0.45"), Some((0, 0, 45)));
        assert!(parse_api_version("v0.0.4.5").is_none());
    }

    #[test]
    fn time_limits_cover_every_slurm_form() {
        let m = |s: &str| parse_time_limit(s).unwrap();
        assert_eq!(m(""), None);
        assert_eq!(m("120"), Some(SlurmTimeLimit::Minutes(120)));
        assert_eq!(m("30:00"), Some(SlurmTimeLimit::Minutes(30)));
        assert_eq!(m("4:00:00"), Some(SlurmTimeLimit::Minutes(240)));
        assert_eq!(m("0-04:00:00"), Some(SlurmTimeLimit::Minutes(240)));
        assert_eq!(m("1-00"), Some(SlurmTimeLimit::Minutes(1440)));
        assert_eq!(
            m("2-12:30"),
            Some(SlurmTimeLimit::Minutes((2 * 24 + 12) * 60 + 30))
        );
        assert_eq!(m("0:30"), Some(SlurmTimeLimit::Minutes(1)));
        assert_eq!(m("UNLIMITED"), Some(SlurmTimeLimit::Unlimited));
        assert_eq!(m("infinite"), Some(SlurmTimeLimit::Unlimited));
        for bad in ["4h", "2d", "abc", "1-", "1:2:3:4", "-5", "0", "0:00"] {
            let e = parse_time_limit(bad).unwrap_err();
            assert!(e.contains(bad) || e.contains("zero"), "{bad}: {e}");
        }
    }

    #[test]
    fn memory_sizes_parse_in_megabytes() {
        assert_eq!(parse_mem_mb("560G"), Some(560 * 1024));
        assert_eq!(parse_mem_mb("560GB"), Some(560 * 1024));
        assert_eq!(parse_mem_mb("64GiB"), Some(64 * 1024));
        assert_eq!(parse_mem_mb("64 G"), Some(64 * 1024));
        assert_eq!(parse_mem_mb("512000M"), Some(512000));
        assert_eq!(parse_mem_mb("512000"), Some(512000));
        assert_eq!(parse_mem_mb("2T"), Some(2 * 1024 * 1024));
        assert_eq!(parse_mem_mb("0"), Some(0));
        for bad in ["", "lots", "-4G", "G"] {
            assert_eq!(parse_mem_mb(bad), None, "{bad}");
        }
    }

    #[test]
    fn gres_items_are_checked_and_each_gets_the_tres_prefix() {
        assert!(validate_gres("").is_ok());
        assert!(validate_gres("gpu:1").is_ok());
        assert!(validate_gres("gpu:a100:2,shard:2").is_ok());
        assert!(validate_gres("gres/gpu:1").is_ok());
        assert!(validate_gres("gpu").is_ok());
        assert!(validate_gres("gpu:a100:0").is_err());
        assert!(validate_gres("gpu:a100:x").is_err());
        assert!(validate_gres("gpu a100").is_err());
        assert_eq!(gres_to_tres_per_node("gpu:1"), "gres/gpu:1");
        assert_eq!(
            gres_to_tres_per_node("gpu:a100:1, shard:2"),
            "gres/gpu:a100:1,gres/shard:2"
        );
        assert_eq!(gres_to_tres_per_node("gres/gpu:1"), "gres/gpu:1");
        assert_eq!(gres_to_tres_per_node("gres:gpu:1"), "gres/gpu:1");
    }

    #[test]
    fn job_names_are_safe_in_file_names() {
        assert_eq!(
            slurm_job_name("obleth-", "org/Model 7B"),
            "obleth-org-Model-7B"
        );
        assert_eq!(slurm_job_name("obleth-", "a.b_c-d"), "obleth-a.b_c-d");
    }

    fn token(payload: &str) -> String {
        let enc = |s: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(s);
        format!("{}.{}.sig", enc(r#"{"alg":"HS256"}"#), enc(payload))
    }

    #[test]
    fn jwt_claims_are_read_without_verification() {
        let c = slurm_jwt_claims(&token(
            r#"{"exp":1700000000,"iat":1699998200,"sun":"alice"}"#,
        ))
        .unwrap();
        assert_eq!(c.exp, Some(1_700_000_000));
        assert_eq!(c.iat, Some(1_699_998_200));
        assert_eq!(c.sun.as_deref(), Some("alice"));
        assert!(slurm_jwt_claims("not-a-jwt").is_none());
    }

    #[test]
    fn pasted_tokens_lose_the_shell_wrapping() {
        assert_eq!(clean_pasted_jwt("SLURM_JWT=abc.def.ghi"), "abc.def.ghi");
        assert_eq!(
            clean_pasted_jwt("export SLURM_JWT=abc.def.ghi\n"),
            "abc.def.ghi"
        );
        assert_eq!(clean_pasted_jwt("  'abc.def.ghi' "), "abc.def.ghi");
        assert_eq!(clean_pasted_jwt("SLURM_JWT=\"abc.def\""), "abc.def");
        assert_eq!(clean_pasted_jwt("abc.def.ghi"), "abc.def.ghi");
    }

    #[test]
    fn backoff_follows_the_ladder() {
        let steps: Vec<i64> = (0..7).map(backoff_minutes).collect();
        assert_eq!(steps, [0, 5, 15, 30, 60, 60, 60]);
    }

    #[test]
    fn launcher_spec_keeps_names_and_hides_secret_values() {
        let spec = serde_json::json!({
            "recipe_id": "vllm",
            "env": { "WANDB_API_KEY": "abc", "VLLM_LOGGING_LEVEL": "INFO" },
            "inputs": {
                "model": "org/model",
                "extra_flags": "--max-model-len 8192 --api-key s3cret --hf-token=t0k",
                "hf_token": "t0k",
            },
        });
        let r = redact_launcher_spec(&spec);
        assert_eq!(r["recipe_id"], "vllm");
        assert_eq!(r["env"]["WANDB_API_KEY"], HIDDEN_VALUE);
        assert_eq!(r["env"]["VLLM_LOGGING_LEVEL"], HIDDEN_VALUE);
        assert_eq!(r["inputs"]["model"], "org/model");
        assert_eq!(r["inputs"]["hf_token"], HIDDEN_VALUE);
        let flags = r["inputs"]["extra_flags"].as_str().unwrap();
        assert!(flags.contains("--max-model-len 8192"), "{flags}");
        assert!(
            !flags.contains("s3cret") && !flags.contains("t0k"),
            "{flags}"
        );
        assert!(flags.contains("--hf-token=(hidden)"), "{flags}");
        assert_eq!(
            redact_launcher_spec(&serde_json::json!(null)),
            serde_json::json!(null)
        );
    }
}
