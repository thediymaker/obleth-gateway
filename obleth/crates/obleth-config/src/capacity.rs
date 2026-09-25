//! Inputs of the `discovered` capacity mode, shared by the Management API
//! (validated when a model is written) and the gateway's discovery loop
//! (defaults applied when the source is read).
//!
//! A discovered model's pool size is `ready serving replicas x per-replica
//! concurrency x headroom`. The replicas are counted by its capacity source:
//! the model's own enabled, healthy endpoints (`endpoints`), or the ready
//! endpoints of a Kubernetes Service (`kubernetes`). The per-replica
//! concurrency is always the operator's: the model's
//! `per_replica_max_in_flight`, or for the `endpoints` source an endpoint's
//! own `max_in_flight`. Which pods a Service counts is the Service's own
//! selector: a multi-node deployment where only some pods take requests points
//! the Service at those pods.

use crate::{is_valid_capacity_source, DEFAULT_CAPACITY_SOURCE};

/// Largest per-replica concurrency accepted from an operator.
pub const MAX_PER_REPLICA_MAX_IN_FLIGHT: i64 = 100_000;

/// Largest `capacity_headroom` accepted.
pub const MAX_CAPACITY_HEADROOM: f64 = 10.0;

/// Placeholders a Service name template may use.
pub const SERVICE_PLACEHOLDERS: &[&str] = &["{upstream_model}", "{model_name}"];

/// A model's `discovered`-mode inputs as stored. See the column comments in
/// migration 0029 for what an unset field falls back to.
#[derive(Debug, Clone, PartialEq)]
pub struct DiscoveryFields {
    pub source: String,
    pub namespace: Option<String>,
    pub service: Option<String>,
    pub per_replica_max_in_flight: Option<i64>,
    pub headroom: f64,
}

impl Default for DiscoveryFields {
    fn default() -> Self {
        DiscoveryFields {
            source: DEFAULT_CAPACITY_SOURCE.to_string(),
            namespace: None,
            service: None,
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
            service: m.capacity_service.clone(),
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
            service: normalize_optional_text(self.service.as_deref()),
            per_replica_max_in_flight: self.per_replica_max_in_flight,
            headroom: self.headroom,
        }
    }
}

/// The gateway settings a model write is checked against.
#[derive(Debug, Clone, Copy)]
pub struct DiscoveryPolicy<'a> {
    /// `OBLETH_CAPACITY_DISCOVERY_NAMESPACES`: the namespaces the
    /// `kubernetes` source may read, in the order a model that names none is
    /// looked up. Empty means the source is unavailable.
    pub namespaces: &'a [String],
    /// `OBLETH_CAPACITY_DEFAULT_SERVICE`: the Service name template for
    /// models that set no `capacity_service`. Empty means such a model is
    /// refused.
    pub default_service: &'a str,
}

/// Trim an optional text field; blank reads as unset.
pub fn normalize_optional_text(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string)
}

/// Fill a Service name template's `{upstream_model}` and `{model_name}`
/// placeholders. Nothing is rewritten: the result must already be a valid
/// Service name, or the model needs its own `capacity_service`.
pub fn render_service_template(
    template: &str,
    upstream_model: &str,
    model_name: &str,
) -> Result<String, String> {
    let template = template.trim();
    let mut out = template.to_string();
    for (placeholder, value) in [
        ("{upstream_model}", upstream_model.trim()),
        ("{model_name}", model_name.trim()),
    ] {
        out = out.replace(placeholder, value);
    }
    if is_service_name(&out) {
        Ok(out)
    } else {
        Err(format!(
            "OBLETH_CAPACITY_DEFAULT_SERVICE `{template}` gives `{out}` for this model, which is \
             not a valid Service name (lowercase letters, digits and '-', starting with a letter, \
             at most 63 characters); set capacity_service"
        ))
    }
}

/// The Service a `kubernetes`-source model is counted by: its own, else the
/// gateway's default template filled in for it.
pub fn effective_capacity_service(
    service: Option<&str>,
    default_template: &str,
    upstream_model: &str,
    model_name: &str,
) -> Result<String, String> {
    if let Some(own) = service.map(str::trim).filter(|s| !s.is_empty()) {
        validate_capacity_service(own)?;
        return Ok(own.to_string());
    }
    if default_template.trim().is_empty() {
        return Err(
            "no capacity_service is set and OBLETH_CAPACITY_DEFAULT_SERVICE is empty: name the \
             Service whose ready endpoints count this model's replicas"
                .into(),
        );
    }
    render_service_template(default_template, upstream_model, model_name)
}

/// A Kubernetes Service name: an RFC 1035 label (lowercase alphanumerics and
/// `-`, starting with a letter, ending alphanumeric, at most 63 characters).
pub fn validate_capacity_service(name: &str) -> Result<(), String> {
    if is_service_name(name) {
        Ok(())
    } else {
        Err(format!(
            "capacity_service `{name}` is not a valid Kubernetes Service name (lowercase \
             letters, digits and '-', starting with a letter, at most 63 characters)"
        ))
    }
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

/// True for an RFC 1035 label, the syntax of a Service name.
pub fn is_service_name(v: &str) -> bool {
    let bytes = v.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes[0].is_ascii_lowercase()
        && bytes[bytes.len() - 1].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
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

/// The per-replica hint shown in every "required" error.
const PER_REPLICA_HINT: &str = "the requests one backend replica serves at once, e.g. your \
                                server's max concurrent sequences, such as vLLM --max-num-seqs";

/// Validate the capacity fields of a model write. The field syntax is always
/// checked. When the model is (or becomes) `discovered`:
///
/// - `kubernetes` needs `per_replica_max_in_flight`, and must be readable by
///   this gateway: `OBLETH_CAPACITY_DISCOVERY_NAMESPACES` set, an explicit
///   namespace inside it, and a Service of its own or a default template that
///   renders for it.
/// - `endpoints` needs `per_replica_max_in_flight` unless every one of the
///   model's endpoints sets its own `max_in_flight`. `endpoint_max_in_flight`
///   holds those values, one per endpoint row (empty for a model with none,
///   which is counted by its `api_base` and so needs the model's value).
pub fn validate_discovery_fields(
    model_name: &str,
    upstream_model: &str,
    fields: &DiscoveryFields,
    discovered: bool,
    policy: DiscoveryPolicy<'_>,
    endpoint_max_in_flight: &[Option<i64>],
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
    if let Some(svc) = &fields.service {
        validate_capacity_service(svc)?;
    }
    validate_per_replica_max_in_flight(fields.per_replica_max_in_flight)?;
    validate_capacity_headroom(fields.headroom)?;
    if !discovered {
        return Ok(());
    }
    if fields.source != "kubernetes" {
        let every_endpoint_has_its_own = !endpoint_max_in_flight.is_empty()
            && endpoint_max_in_flight.iter().all(Option::is_some);
        if fields.per_replica_max_in_flight.is_none() && !every_endpoint_has_its_own {
            return Err(format!(
                "per_replica_max_in_flight is required for a discovered model on the endpoints \
                 source unless every endpoint sets its own max_in_flight: set it to \
                 {PER_REPLICA_HINT}"
            ));
        }
        return Ok(());
    }
    if fields.per_replica_max_in_flight.is_none() {
        return Err(format!(
            "per_replica_max_in_flight is required for a discovered model on the kubernetes \
             source: set it to {PER_REPLICA_HINT}"
        ));
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
    effective_capacity_service(
        fields.service.as_deref(),
        policy.default_service,
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

    fn kube(namespace: Option<&str>, service: Option<&str>) -> DiscoveryFields {
        DiscoveryFields {
            source: "kubernetes".into(),
            namespace: namespace.map(str::to_string),
            service: service.map(str::to_string),
            per_replica_max_in_flight: Some(8),
            ..Default::default()
        }
    }

    #[test]
    fn a_models_own_service_wins_over_the_template() {
        assert_eq!(
            effective_capacity_service(Some(" my-model "), "{model_name}", "u", "m").unwrap(),
            "my-model"
        );
    }

    #[test]
    fn the_template_fills_both_placeholders() {
        assert_eq!(
            effective_capacity_service(None, "{upstream_model}", "qwen3-32b", "qwen3").unwrap(),
            "qwen3-32b"
        );
        assert_eq!(
            effective_capacity_service(Some("   "), "{model_name}-serve", "u", "chat").unwrap(),
            "chat-serve"
        );
        assert_eq!(
            effective_capacity_service(None, "{model_name}-{upstream_model}", "v2", "chat")
                .unwrap(),
            "chat-v2"
        );
    }

    #[test]
    fn with_no_service_and_no_template_there_is_nothing_to_count() {
        let err = effective_capacity_service(None, "  ", "qwen3-32b", "qwen3").unwrap_err();
        assert!(err.contains("OBLETH_CAPACITY_DEFAULT_SERVICE"), "{err}");
        assert!(err.contains("capacity_service"), "{err}");
    }

    #[test]
    fn a_name_that_makes_no_service_name_cannot_fill_the_template() {
        for upstream in ["org/model", "Qwen3-32B", "model.v2", "9lives"] {
            let err =
                effective_capacity_service(None, "{upstream_model}", upstream, "m").unwrap_err();
            assert!(err.contains("set capacity_service"), "{upstream}: {err}");
        }
        // A template that does not use the offending field is fine.
        assert_eq!(
            effective_capacity_service(None, "{model_name}", "org/model", "m").unwrap(),
            "m"
        );
    }

    #[test]
    fn service_names_follow_rfc_1035() {
        for ok in ["m", "my-model", "qwen3-32b", &"a".repeat(63)] {
            assert!(validate_capacity_service(ok).is_ok(), "{ok}");
        }
        for bad in [
            "",
            "My-Model",
            "1model",
            "-model",
            "model-",
            "my_model",
            "my.model",
            "app=vllm",
            "model/x",
            &"a".repeat(64),
        ] {
            assert!(validate_capacity_service(bad).is_err(), "{bad:?}");
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
            default_service: "{upstream_model}",
        };
        let ok = |f: &DiscoveryFields| validate_discovery_fields("m", "m", f, true, policy, &[]);
        assert!(ok(&kube(Some("llm"), None)).is_ok());
        // No namespace looks the Service up in the allowlist itself.
        assert!(ok(&kube(None, Some("x"))).is_ok());
        let err = ok(&kube(Some("kube-system"), None)).expect_err("outside the allowlist");
        assert!(
            err.contains("OBLETH_CAPACITY_DISCOVERY_NAMESPACES"),
            "{err}"
        );

        let none: Vec<String> = Vec::new();
        let off = DiscoveryPolicy {
            namespaces: &none,
            default_service: "",
        };
        let err =
            validate_discovery_fields("m", "m", &kube(Some("llm"), Some("b")), true, off, &[])
                .expect_err("source unavailable");
        assert!(err.contains("not available"), "{err}");
        // Not discovered yet: only the syntax is checked.
        assert!(
            validate_discovery_fields("m", "m", &kube(Some("llm"), None), false, off, &[]).is_ok()
        );

        let no_template = DiscoveryPolicy {
            namespaces: &namespaces,
            default_service: "",
        };
        let err =
            validate_discovery_fields("m", "m", &kube(Some("llm"), None), true, no_template, &[])
                .expect_err("no Service at all");
        assert!(err.contains("capacity_service"), "{err}");
    }

    #[test]
    fn a_kubernetes_model_needs_its_per_replica_value() {
        let namespaces = ns();
        let policy = DiscoveryPolicy {
            namespaces: &namespaces,
            default_service: "{upstream_model}",
        };
        let mut f = kube(Some("llm"), None);
        f.per_replica_max_in_flight = None;
        let err = validate_discovery_fields("m", "m", &f, true, policy, &[Some(8)])
            .expect_err("required");
        assert!(
            err.contains("per_replica_max_in_flight is required"),
            "{err}"
        );
        assert!(err.contains("--max-num-seqs"), "{err}");
        // Not in the discovered mode: not required.
        assert!(validate_discovery_fields("m", "m", &f, false, policy, &[]).is_ok());
    }

    #[test]
    fn the_endpoints_source_needs_a_value_for_every_endpoint() {
        let none: Vec<String> = Vec::new();
        let off = DiscoveryPolicy {
            namespaces: &none,
            default_service: "",
        };
        let with_model_value = DiscoveryFields {
            per_replica_max_in_flight: Some(8),
            ..Default::default()
        };
        let without = DiscoveryFields::default();
        let check = |f: &DiscoveryFields, eps: &[Option<i64>]| {
            validate_discovery_fields("m", "org/m", f, true, off, eps)
        };
        // The model's value covers everything, with or without endpoint rows.
        assert!(check(&with_model_value, &[]).is_ok());
        assert!(check(&with_model_value, &[None, Some(4)]).is_ok());
        // Without it, every endpoint needs its own.
        assert!(check(&without, &[Some(4), Some(16)]).is_ok());
        let err = check(&without, &[Some(4), None]).expect_err("one endpoint has none");
        assert!(
            err.contains("every endpoint sets its own max_in_flight"),
            "{err}"
        );
        // No endpoint rows: the api_base is counted at the model's value.
        assert!(check(&without, &[]).is_err());
    }

    #[test]
    fn field_syntax_is_checked_whatever_the_mode() {
        let none: Vec<String> = Vec::new();
        let off = DiscoveryPolicy {
            namespaces: &none,
            default_service: "",
        };
        let bad = |f: DiscoveryFields| validate_discovery_fields("m", "m", &f, false, off, &[]);
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
            service: Some("app=x".into()),
            ..Default::default()
        })
        .unwrap_err()
        .contains("capacity_service"));
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
