//! Inputs of the `discovered` capacity mode, shared by the Management API
//! (validated when a model is written) and the gateway's discovery loop
//! (defaults applied when the source is read).
//!
//! A discovered model's pool size is `ready serving replicas x per-replica
//! concurrency x headroom`, read from its capacity source: the model's own
//! enabled, healthy endpoints (`endpoints`), or the Ready pods a label
//! selector matches (`kubernetes`). Which pods serve is the selector's job:
//! a multi-node deployment whose worker pods serve nothing excludes them in
//! the selector (for KubeRay, `ray.io/node-type!=worker`).

use crate::{is_valid_capacity_source, DEFAULT_CAPACITY_SOURCE};

/// Largest per-replica concurrency accepted from an operator or read off a
/// container.
pub const MAX_PER_REPLICA_MAX_IN_FLIGHT: i64 = 100_000;

/// Largest `capacity_headroom` accepted.
pub const MAX_CAPACITY_HEADROOM: f64 = 10.0;

/// Longest label selector accepted.
const MAX_SELECTOR_LEN: usize = 1024;

/// Placeholders a selector template may use.
pub const SELECTOR_PLACEHOLDERS: &[&str] = &["{upstream_model}", "{model_name}"];

/// A model's `discovered`-mode inputs as stored. See the column comments in
/// migration 0029 for what an unset field falls back to.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveryFields {
    pub source: String,
    pub namespace: Option<String>,
    pub selector: Option<String>,
    pub per_replica_max_in_flight: Option<i64>,
    pub headroom: f64,
}

impl Default for DiscoveryFields {
    fn default() -> Self {
        DiscoveryFields {
            source: DEFAULT_CAPACITY_SOURCE.to_string(),
            namespace: None,
            selector: None,
            per_replica_max_in_flight: None,
            headroom: 1.0,
        }
    }
}

impl DiscoveryFields {
    /// The fields a stored model carries.
    pub fn of(m: &crate::ModelRoute) -> Self {
        DiscoveryFields {
            source: m.capacity_source.clone(),
            namespace: m.capacity_namespace.clone(),
            selector: m.capacity_selector.clone(),
            per_replica_max_in_flight: m.per_replica_max_in_flight,
            headroom: m.capacity_headroom,
        }
    }

    /// Trimmed and lowercased where the vocabulary is, with blank text read as
    /// unset and a blank source as the default.
    pub fn normalized(&self) -> Self {
        let source = self.source.trim().to_ascii_lowercase();
        DiscoveryFields {
            source: if source.is_empty() {
                DEFAULT_CAPACITY_SOURCE.to_string()
            } else {
                source
            },
            namespace: normalize_optional_text(self.namespace.as_deref()),
            selector: normalize_optional_text(self.selector.as_deref()),
            per_replica_max_in_flight: self.per_replica_max_in_flight,
            headroom: self.headroom,
        }
    }
}

/// The gateway settings a model write is checked against.
#[derive(Debug, Clone, Copy)]
pub struct DiscoveryPolicy<'a> {
    /// `OBLETH_CAPACITY_DISCOVERY_NAMESPACES`: the namespaces the
    /// `kubernetes` source may read. Empty means the source is unavailable.
    pub namespaces: &'a [String],
    /// `OBLETH_CAPACITY_DEFAULT_SELECTOR`: the template for models that set
    /// no selector. Empty means such a model is refused.
    pub default_selector: &'a str,
}

/// Trim an optional text field; blank reads as unset.
pub fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Fill a selector template's `{upstream_model}` and `{model_name}`
/// placeholders. Each substituted value must be a valid label value, and the
/// result must parse as a selector.
pub fn render_selector_template(
    template: &str,
    upstream_model: &str,
    model_name: &str,
) -> Result<String, String> {
    let mut out = template.trim().to_string();
    for (placeholder, field, value) in [
        ("{upstream_model}", "upstream_model", upstream_model.trim()),
        ("{model_name}", "model_name", model_name.trim()),
    ] {
        if out.contains(placeholder) {
            if !is_label_value(value) {
                return Err(format!(
                    "{field} `{value}` is not a valid label value, so the default selector \
                     `{template}` cannot match it; set capacity_selector"
                ));
            }
            out = out.replace(placeholder, value);
        }
    }
    validate_capacity_selector(&out).map_err(|e| {
        format!("OBLETH_CAPACITY_DEFAULT_SELECTOR renders an invalid selector: {e}")
    })?;
    Ok(out)
}

/// The selector a `kubernetes`-source model lists pods with: its own, else
/// the gateway's default template filled in for it.
pub fn effective_capacity_selector(
    selector: Option<&str>,
    default_template: &str,
    upstream_model: &str,
    model_name: &str,
) -> Result<String, String> {
    if let Some(own) = selector.map(str::trim).filter(|s| !s.is_empty()) {
        validate_capacity_selector(own)?;
        return Ok(own.to_string());
    }
    if default_template.trim().is_empty() {
        return Err(
            "no capacity_selector is set and OBLETH_CAPACITY_DEFAULT_SELECTOR is empty".into(),
        );
    }
    render_selector_template(default_template, upstream_model, model_name)
}

/// A Kubernetes namespace name: an RFC 1123 label (lowercase alphanumerics
/// and `-`, starting and ending alphanumeric, at most 63 characters).
pub fn validate_capacity_namespace(ns: &str) -> Result<(), String> {
    if is_dns_label(ns) {
        Ok(())
    } else {
        Err(format!(
            "capacity_namespace `{ns}` is not a valid Kubernetes namespace (lowercase letters, \
             digits and '-', at most 63 characters)"
        ))
    }
}

/// A per-replica concurrency, when given, must be 1..=[`MAX_PER_REPLICA_MAX_IN_FLIGHT`].
pub fn validate_per_replica_max_in_flight(value: Option<i64>) -> Result<(), String> {
    match value {
        Some(v) if !(1..=MAX_PER_REPLICA_MAX_IN_FLIGHT).contains(&v) => Err(format!(
            "per_replica_max_in_flight must be between 1 and {MAX_PER_REPLICA_MAX_IN_FLIGHT}"
        )),
        _ => Ok(()),
    }
}

/// The headroom multiplier must be finite, above 0 and at most
/// [`MAX_CAPACITY_HEADROOM`].
pub fn validate_capacity_headroom(value: f64) -> Result<(), String> {
    if value.is_finite() && value > 0.0 && value <= MAX_CAPACITY_HEADROOM {
        Ok(())
    } else {
        Err(format!(
            "capacity_headroom must be above 0 and at most {MAX_CAPACITY_HEADROOM} (got {value})"
        ))
    }
}

/// Check a label selector in the Kubernetes syntax: comma-separated
/// requirements, each `key`, `!key`, `key=value`, `key==value`,
/// `key!=value`, `key in (a,b)` or `key notin (a,b)`. The API server is the
/// final judge; this catches typos when the model is saved rather than on
/// the next discovery pass.
pub fn validate_capacity_selector(selector: &str) -> Result<(), String> {
    let selector = selector.trim();
    let fail = |why: &str| Err(format!("capacity_selector `{selector}`: {why}"));
    if selector.is_empty() {
        return fail("must not be empty (it would match every pod in the namespace)");
    }
    if selector.len() > MAX_SELECTOR_LEN {
        return fail("is too long");
    }
    if selector.chars().any(char::is_control) {
        return fail("contains control characters");
    }
    for requirement in split_requirements(selector)? {
        if let Err(why) = check_requirement(requirement.trim()) {
            return fail(&why);
        }
    }
    Ok(())
}

/// Split on the commas that separate requirements, not those inside a set.
fn split_requirements(selector: &str) -> Result<Vec<&str>, String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (i, c) in selector.char_indices() {
        match c {
            '(' => depth += 1,
            ')' => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| format!("capacity_selector `{selector}`: unbalanced ')'"))?
            }
            ',' if depth == 0 => {
                out.push(&selector[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(format!("capacity_selector `{selector}`: unbalanced '('"));
    }
    out.push(&selector[start..]);
    Ok(out)
}

fn check_requirement(req: &str) -> Result<(), String> {
    if req.is_empty() {
        return Err("has an empty requirement".into());
    }
    if let Some(key) = req.strip_prefix('!') {
        return check_key(key.trim());
    }
    for op in ["!=", "==", "="] {
        if let Some((key, value)) = req.split_once(op) {
            check_key(key.trim())?;
            return check_value(value.trim());
        }
    }
    // Set-based: `key in (a,b)` / `key notin (a,b)`.
    if let Some(open) = req.find('(') {
        let head = req[..open].trim();
        let Some(body) = req[open + 1..].trim_end().strip_suffix(')') else {
            return Err(format!("`{req}` has no closing ')'"));
        };
        let mut words = head.split_whitespace();
        let (Some(key), Some(op), None) = (words.next(), words.next(), words.next()) else {
            return Err(format!(
                "`{req}` is not `key in (...)` or `key notin (...)`"
            ));
        };
        if op != "in" && op != "notin" {
            return Err(format!("`{req}` uses unknown operator `{op}`"));
        }
        check_key(key)?;
        let values: Vec<&str> = body.split(',').map(str::trim).collect();
        if values.iter().all(|v| v.is_empty()) {
            return Err(format!("`{req}` lists no values"));
        }
        return values.into_iter().try_for_each(check_value);
    }
    check_key(req)
}

/// A label key: an optional DNS-subdomain prefix and `/`, then a name of at
/// most 63 characters.
fn check_key(key: &str) -> Result<(), String> {
    let (prefix, name) = match key.rsplit_once('/') {
        Some((p, n)) => (Some(p), n),
        None => (None, key),
    };
    if let Some(p) = prefix {
        if p.is_empty() || p.len() > 253 || !p.split('.').all(is_dns_label) {
            return Err(format!("label key `{key}` has an invalid prefix"));
        }
    }
    if name.is_empty() || !is_label_value(name) {
        return Err(format!("label key `{key}` is not a valid label name"));
    }
    Ok(())
}

fn check_value(value: &str) -> Result<(), String> {
    if value.is_empty() || is_label_value(value) {
        Ok(())
    } else {
        Err(format!("`{value}` is not a valid label value"))
    }
}

/// True for a non-empty label value: at most 63 characters of alphanumerics,
/// `-`, `_` and `.`, starting and ending alphanumeric. An `upstream_model`
/// that is not one (say `org/model`) needs an explicit selector.
pub fn is_label_value(v: &str) -> bool {
    let bytes = v.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn is_dns_label(v: &str) -> bool {
    let bytes = v.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes[0].is_ascii_alphanumeric()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// Parse a comma-separated namespace list: trimmed, blanks dropped, duplicates
/// removed in order.
pub fn parse_namespace_list(raw: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for ns in raw.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !out.iter().any(|n| n == ns) {
            out.push(ns.to_string());
        }
    }
    out
}

/// Validate the capacity fields of a model write. The field syntax is always
/// checked. When the model is (or becomes) `discovered` with the
/// `kubernetes` source, it must also be readable by this gateway: an explicit
/// namespace inside `OBLETH_CAPACITY_DISCOVERY_NAMESPACES` (and that list set
/// at all), and a selector of its own or a default template that renders for
/// it. The `endpoints` source is checked against the endpoints themselves by
/// the discovery loop, since they change after the model is written.
pub fn validate_discovery_fields(
    model_name: &str,
    upstream_model: &str,
    fields: &DiscoveryFields,
    discovered: bool,
    policy: DiscoveryPolicy<'_>,
) -> Result<(), String> {
    let fields = fields.normalized();
    if !is_valid_capacity_source(&fields.source) {
        return Err(format!(
            "unknown capacity_source `{}` (expected one of: {})",
            fields.source,
            crate::CAPACITY_SOURCES.join(", ")
        ));
    }
    if let Some(ns) = &fields.namespace {
        validate_capacity_namespace(ns)?;
    }
    if let Some(sel) = &fields.selector {
        validate_capacity_selector(sel)?;
    }
    validate_per_replica_max_in_flight(fields.per_replica_max_in_flight)?;
    validate_capacity_headroom(fields.headroom)?;
    if !discovered || fields.source != "kubernetes" {
        return Ok(());
    }
    if policy.namespaces.is_empty() {
        return Err(
            "the kubernetes capacity source is not available on this gateway: \
                    OBLETH_CAPACITY_DISCOVERY_NAMESPACES is empty"
                .into(),
        );
    }
    if let Some(ns) = &fields.namespace {
        if !policy.namespaces.iter().any(|a| a == ns) {
            return Err(format!(
                "capacity_namespace `{ns}` is not in OBLETH_CAPACITY_DISCOVERY_NAMESPACES ({})",
                policy.namespaces.join(", ")
            ));
        }
    }
    effective_capacity_selector(
        fields.selector.as_deref(),
        policy.default_selector,
        upstream_model,
        model_name,
    )
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NS: &[&str] = &["llm", "image"];

    fn ns() -> Vec<String> {
        NS.iter().map(|s| s.to_string()).collect()
    }

    fn kube(namespace: Option<&str>, selector: Option<&str>) -> DiscoveryFields {
        DiscoveryFields {
            source: "kubernetes".into(),
            namespace: namespace.map(str::to_string),
            selector: selector.map(str::to_string),
            ..Default::default()
        }
    }

    #[test]
    fn a_models_own_selector_wins_over_the_template() {
        assert_eq!(
            effective_capacity_selector(Some(" app=vllm,tier=gpu "), "x={model_name}", "u", "m")
                .unwrap(),
            "app=vllm,tier=gpu"
        );
    }

    #[test]
    fn the_template_fills_both_placeholders() {
        let tpl =
            "serving.example/model={upstream_model},gateway.example/name={model_name},role!=worker";
        assert_eq!(
            effective_capacity_selector(None, tpl, "qwen3-32b", "qwen3").unwrap(),
            "serving.example/model=qwen3-32b,gateway.example/name=qwen3,role!=worker"
        );
        assert_eq!(
            effective_capacity_selector(Some("   "), "app={upstream_model}", "llama", "l").unwrap(),
            "app=llama"
        );
    }

    #[test]
    fn with_no_selector_and_no_template_there_is_nothing_to_list() {
        let err = effective_capacity_selector(None, "  ", "qwen3-32b", "qwen3").unwrap_err();
        assert!(err.contains("OBLETH_CAPACITY_DEFAULT_SELECTOR"), "{err}");
    }

    #[test]
    fn a_name_that_is_no_label_value_cannot_fill_the_template() {
        let err = effective_capacity_selector(None, "app={upstream_model}", "org/model", "m")
            .unwrap_err();
        assert!(err.contains("set capacity_selector"), "{err}");
        // A template that does not use the offending field is fine.
        assert_eq!(
            effective_capacity_selector(None, "app={model_name}", "org/model", "m").unwrap(),
            "app=m"
        );
    }

    #[test]
    fn selectors_in_kubernetes_syntax_are_accepted() {
        for ok in [
            "app.example.com/name=qwen3-32b",
            "app==vllm",
            "app!=sidecar",
            "app=vllm,tier in (gpu, hpu),!canary",
            "environment notin (dev)",
            "ray.io/node-type!=worker",
            "gpu",
            "app.example.com/name=",
        ] {
            assert!(validate_capacity_selector(ok).is_ok(), "{ok}");
        }
    }

    #[test]
    fn malformed_selectors_are_refused() {
        for bad in [
            "",
            "   ",
            "app=vllm,",
            "app in (a,b",
            "app in a,b)",
            "app is (a)",
            "app=has space",
            "-bad=x",
            "/name=x",
            "Bad_Prefix.example.com/name=x",
            "app=org/model",
            "app in ()",
        ] {
            assert!(validate_capacity_selector(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn namespaces_must_be_dns_labels() {
        assert!(validate_capacity_namespace("inference-llm").is_ok());
        assert!(validate_capacity_namespace("Models").is_err());
        assert!(validate_capacity_namespace("-models").is_err());
        assert!(validate_capacity_namespace("models_llm").is_err());
        assert!(validate_capacity_namespace(&"a".repeat(64)).is_err());
    }

    #[test]
    fn the_namespace_list_is_trimmed_and_deduplicated() {
        assert_eq!(
            parse_namespace_list(" llm, image ,, llm,audio "),
            vec!["llm", "image", "audio"]
        );
        assert!(parse_namespace_list(" , ").is_empty());
    }

    #[test]
    fn a_kubernetes_model_must_be_readable_by_this_gateway() {
        let namespaces = ns();
        let policy = DiscoveryPolicy {
            namespaces: &namespaces,
            default_selector: "app={upstream_model}",
        };
        let ok = |f: &DiscoveryFields| validate_discovery_fields("m", "m", f, true, policy);
        assert!(ok(&kube(Some("llm"), None)).is_ok());
        // No namespace searches the allowlist itself.
        assert!(ok(&kube(None, Some("app=x"))).is_ok());
        let err = ok(&kube(Some("kube-system"), None)).expect_err("outside the allowlist");
        assert!(
            err.contains("OBLETH_CAPACITY_DISCOVERY_NAMESPACES"),
            "{err}"
        );

        let none: Vec<String> = Vec::new();
        let off = DiscoveryPolicy {
            namespaces: &none,
            default_selector: "",
        };
        let err = validate_discovery_fields("m", "m", &kube(Some("llm"), Some("a=b")), true, off)
            .expect_err("source unavailable");
        assert!(err.contains("not available"), "{err}");
        // Not discovered yet: only the syntax is checked.
        assert!(validate_discovery_fields("m", "m", &kube(Some("llm"), None), false, off).is_ok());

        let no_template = DiscoveryPolicy {
            namespaces: &namespaces,
            default_selector: "",
        };
        let err = validate_discovery_fields("m", "m", &kube(Some("llm"), None), true, no_template)
            .expect_err("no selector at all");
        assert!(err.contains("capacity_selector"), "{err}");
    }

    #[test]
    fn the_endpoints_source_needs_no_gateway_settings() {
        let none: Vec<String> = Vec::new();
        let off = DiscoveryPolicy {
            namespaces: &none,
            default_selector: "",
        };
        let fields = DiscoveryFields {
            per_replica_max_in_flight: Some(8),
            ..Default::default()
        };
        assert!(validate_discovery_fields("m", "org/m", &fields, true, off).is_ok());
    }

    #[test]
    fn field_syntax_is_checked_whatever_the_mode() {
        let none: Vec<String> = Vec::new();
        let off = DiscoveryPolicy {
            namespaces: &none,
            default_selector: "",
        };
        let bad = |f: DiscoveryFields| validate_discovery_fields("m", "m", &f, false, off);
        assert!(bad(DiscoveryFields {
            source: "prometheus".into(),
            ..Default::default()
        })
        .unwrap_err()
        .contains("capacity_source"));
        assert!(bad(DiscoveryFields {
            per_replica_max_in_flight: Some(0),
            ..Default::default()
        })
        .is_err());
        assert!(bad(DiscoveryFields {
            headroom: 0.0,
            ..Default::default()
        })
        .is_err());
        assert!(bad(DiscoveryFields {
            headroom: f64::NAN,
            ..Default::default()
        })
        .is_err());
        assert!(bad(DiscoveryFields {
            selector: Some("a=b c".into()),
            ..Default::default()
        })
        .is_err());
        assert!(bad(DiscoveryFields {
            source: " Kubernetes ".into(),
            headroom: 1.25,
            ..Default::default()
        })
        .is_ok());
    }

    #[test]
    fn per_replica_concurrency_and_headroom_are_bounded() {
        assert!(validate_per_replica_max_in_flight(None).is_ok());
        assert!(validate_per_replica_max_in_flight(Some(1)).is_ok());
        assert!(validate_per_replica_max_in_flight(Some(-4)).is_err());
        assert!(
            validate_per_replica_max_in_flight(Some(MAX_PER_REPLICA_MAX_IN_FLIGHT + 1)).is_err()
        );
        assert!(validate_capacity_headroom(1.0).is_ok());
        assert!(validate_capacity_headroom(MAX_CAPACITY_HEADROOM).is_ok());
        assert!(validate_capacity_headroom(MAX_CAPACITY_HEADROOM + 0.1).is_err());
        assert!(validate_capacity_headroom(-1.0).is_err());
    }
}
