//! Trusted JWT issuers for data-plane bearer authentication.
//!
//! Parsed once at boot from `OBLETH_JWT_ISSUERS` (a JSON array). Trust anchors
//! are deliberately env-only and fail loudly on any parse error: a silently
//! disabled issuer is worse than a refused boot.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

/// Signature algorithms a trusted issuer may use. Asymmetric only: a
/// symmetric (`HS*`) algorithm would make the JWKS itself a shared secret,
/// and `none` is not an algorithm.
pub const ALLOWED_JWT_ALGORITHMS: &[&str] = &[
    "RS256", "RS384", "RS512", "PS256", "PS384", "PS512", "ES256", "ES384", "EdDSA",
];

/// Maximum `device_id` length persisted on usage rows.
pub const DEVICE_ID_MAX_LEN: usize = 64;

/// One trusted issuer, as configured in `OBLETH_JWT_ISSUERS`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct JwtIssuerConfig {
    /// Exact-match against the token's `iss`.
    pub issuer: String,
    /// JWKS document URL. `None` = resolve `jwks_uri` from
    /// `<issuer>/.well-known/openid-configuration` at boot.
    #[serde(default)]
    pub jwks_url: Option<String>,
    /// Exact-match against `aud` (string or array member).
    pub audience: String,
    #[serde(default = "default_algorithms")]
    pub algorithms: Vec<String>,
    #[serde(default = "default_subject_claim")]
    pub subject_claim: String,
    /// Claim copied to `usage.device_id` (after sanitising).
    #[serde(default)]
    pub device_claim: Option<String>,
    /// Extra claims persisted into `api_keys.identity_claims` at provision time.
    #[serde(default)]
    pub identity_claims: Vec<String>,
    /// Name of the tenant identity keys are created under.
    pub tenant: String,
    /// When false, an unknown identity is a 401 rather than being provisioned.
    #[serde(default = "default_true")]
    pub jit_provision: bool,
}

fn default_algorithms() -> Vec<String> {
    vec!["RS256".to_string(), "ES256".to_string()]
}
fn default_subject_claim() -> String {
    "sub".to_string()
}
fn default_true() -> bool {
    true
}

#[derive(Debug, thiserror::Error)]
pub enum JwtConfigError {
    #[error("OBLETH_JWT_ISSUERS is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("OBLETH_JWT_ISSUERS: {0}")]
    Field(String),
    #[error("OBLETH_JWT_ISSUERS: algorithm {0:?} is not allowed (asymmetric only)")]
    Algorithm(String),
    #[error("OBLETH_JWT_ISSUERS: issuer {0:?} is listed more than once")]
    DuplicateIssuer(String),
}

/// Parse and validate the issuer list. Blank input means no issuers (feature off).
pub fn parse_jwt_issuers(raw: &str) -> Result<Vec<JwtIssuerConfig>, JwtConfigError> {
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    let issuers: Vec<JwtIssuerConfig> = serde_json::from_str(raw)?;
    let mut seen = HashSet::new();
    for i in &issuers {
        if i.issuer.trim().is_empty() {
            return Err(JwtConfigError::Field("issuer is required".into()));
        }
        if !is_http_url(&i.issuer) {
            return Err(JwtConfigError::Field(format!(
                "issuer {:?} must be an http(s) URL",
                i.issuer
            )));
        }
        if let Some(u) = &i.jwks_url {
            if !is_http_url(u) {
                return Err(JwtConfigError::Field(format!(
                    "jwks_url {u:?} must be an http(s) URL"
                )));
            }
        }
        if i.audience.trim().is_empty() {
            return Err(JwtConfigError::Field(format!(
                "audience is required for issuer {:?}",
                i.issuer
            )));
        }
        if i.tenant.trim().is_empty() {
            return Err(JwtConfigError::Field(format!(
                "tenant is required for issuer {:?}",
                i.issuer
            )));
        }
        if i.subject_claim.trim().is_empty() {
            return Err(JwtConfigError::Field(format!(
                "subject_claim must not be blank for issuer {:?}",
                i.issuer
            )));
        }
        if i.algorithms.is_empty() {
            return Err(JwtConfigError::Field(format!(
                "algorithms must not be empty for issuer {:?}",
                i.issuer
            )));
        }
        for alg in &i.algorithms {
            if !ALLOWED_JWT_ALGORITHMS.contains(&alg.as_str()) {
                return Err(JwtConfigError::Algorithm(alg.clone()));
            }
        }
        if !seen.insert(i.issuer.clone()) {
            return Err(JwtConfigError::DuplicateIssuer(i.issuer.clone()));
        }
    }
    Ok(issuers)
}

fn is_http_url(s: &str) -> bool {
    s.starts_with("https://") || s.starts_with("http://")
}

/// Read `OBLETH_JWT_ISSUERS`. A trust anchor that fails to parse must not
/// silently disable itself, so any error aborts boot.
pub fn jwt_issuers_from_env() -> Vec<JwtIssuerConfig> {
    let raw = std::env::var("OBLETH_JWT_ISSUERS").unwrap_or_default();
    match parse_jwt_issuers(&raw) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("fatal: {e}");
            std::process::exit(2);
        }
    }
}

/// Bound the token's device claim before it reaches the usage ledger: at most
/// [`DEVICE_ID_MAX_LEN`] characters from `[A-Za-z0-9_-]`, otherwise empty.
pub fn sanitize_device_id(raw: Option<&str>) -> String {
    match raw {
        Some(s)
            if !s.is_empty()
                && s.len() <= DEVICE_ID_MAX_LEN
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') =>
        {
            s.to_string()
        }
        _ => String::new(),
    }
}

/// Lookup handle for an identity key: `jwt:` + SHA-256 over `issuer \0 subject`.
/// Stored in `api_keys.key_hash` and used as the Redis/moka key exactly like a
/// secret key's hash, so the hot path needs no special case after resolution.
pub fn identity_key_hash(issuer: &str, subject: &str) -> String {
    let mut h = Sha256::new();
    h.update(issuer.as_bytes());
    h.update([0u8]);
    h.update(subject.as_bytes());
    format!("jwt:{}", hex::encode(h.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: &str = r#"[{
        "issuer": "https://idp.example.com",
        "jwks_url": "https://idp.example.com/.well-known/jwks.json",
        "audience": "obleth",
        "algorithms": ["ES256"],
        "subject_claim": "sub",
        "device_claim": "device_id",
        "identity_claims": ["uid"],
        "tenant": "cli-users",
        "jit_provision": true
    }]"#;

    #[test]
    fn parses_full_entry() {
        let v = parse_jwt_issuers(FULL).unwrap();
        assert_eq!(v.len(), 1);
        let i = &v[0];
        assert_eq!(i.issuer, "https://idp.example.com");
        assert_eq!(
            i.jwks_url.as_deref(),
            Some("https://idp.example.com/.well-known/jwks.json")
        );
        assert_eq!(i.audience, "obleth");
        assert_eq!(i.algorithms, vec!["ES256".to_string()]);
        assert_eq!(i.subject_claim, "sub");
        assert_eq!(i.device_claim.as_deref(), Some("device_id"));
        assert_eq!(i.identity_claims, vec!["uid".to_string()]);
        assert_eq!(i.tenant, "cli-users");
        assert!(i.jit_provision);
    }

    #[test]
    fn applies_defaults() {
        let v = parse_jwt_issuers(
            r#"[{"issuer":"https://idp.example.com","audience":"obleth","tenant":"cli-users"}]"#,
        )
        .unwrap();
        let i = &v[0];
        assert!(i.jwks_url.is_none());
        assert_eq!(i.algorithms, vec!["RS256".to_string(), "ES256".to_string()]);
        assert_eq!(i.subject_claim, "sub");
        assert!(i.device_claim.is_none());
        assert!(i.identity_claims.is_empty());
        assert!(i.jit_provision);
    }

    #[test]
    fn empty_or_blank_is_no_issuers() {
        assert!(parse_jwt_issuers("").unwrap().is_empty());
        assert!(parse_jwt_issuers("   ").unwrap().is_empty());
        assert!(parse_jwt_issuers("[]").unwrap().is_empty());
    }

    #[test]
    fn rejects_symmetric_and_none_algorithms() {
        for alg in ["HS256", "HS384", "HS512", "none"] {
            let raw = format!(
                r#"[{{"issuer":"https://idp.example.com","audience":"obleth","tenant":"t","algorithms":["{alg}"]}}]"#
            );
            let err = parse_jwt_issuers(&raw).unwrap_err();
            assert!(matches!(err, JwtConfigError::Algorithm(_)), "{alg}: {err}");
        }
    }

    #[test]
    fn rejects_duplicate_issuer() {
        let raw = r#"[
          {"issuer":"https://idp.example.com","audience":"a","tenant":"t"},
          {"issuer":"https://idp.example.com","audience":"b","tenant":"t"}
        ]"#;
        assert!(matches!(
            parse_jwt_issuers(raw).unwrap_err(),
            JwtConfigError::DuplicateIssuer(_)
        ));
    }

    #[test]
    fn rejects_blank_required_fields_and_non_http_urls() {
        assert!(matches!(
            parse_jwt_issuers(r#"[{"issuer":"","audience":"a","tenant":"t"}]"#).unwrap_err(),
            JwtConfigError::Field(_)
        ));
        assert!(matches!(
            parse_jwt_issuers(
                r#"[{"issuer":"https://idp.example.com","audience":"","tenant":"t"}]"#
            )
            .unwrap_err(),
            JwtConfigError::Field(_)
        ));
        assert!(matches!(
            parse_jwt_issuers(
                r#"[{"issuer":"https://idp.example.com","audience":"a","tenant":" "}]"#
            )
            .unwrap_err(),
            JwtConfigError::Field(_)
        ));
        assert!(matches!(
            parse_jwt_issuers(
                r#"[{"issuer":"ftp://idp.example.com","audience":"a","tenant":"t"}]"#
            )
            .unwrap_err(),
            JwtConfigError::Field(_)
        ));
        assert!(matches!(
            parse_jwt_issuers(r#"[{"issuer":"https://idp.example.com","jwks_url":"file:///etc/passwd","audience":"a","tenant":"t"}]"#).unwrap_err(),
            JwtConfigError::Field(_)
        ));
    }

    #[test]
    fn rejects_malformed_json() {
        assert!(matches!(
            parse_jwt_issuers("{not json").unwrap_err(),
            JwtConfigError::Json(_)
        ));
    }

    #[test]
    fn device_id_sanitising() {
        assert_eq!(sanitize_device_id(Some("cm1abc_DEF-9")), "cm1abc_DEF-9");
        assert_eq!(sanitize_device_id(None), "");
        assert_eq!(sanitize_device_id(Some("")), "");
        assert_eq!(sanitize_device_id(Some("has space")), "");
        assert_eq!(sanitize_device_id(Some("semi;colon")), "");
        assert_eq!(sanitize_device_id(Some(&"a".repeat(64))), "a".repeat(64));
        assert_eq!(sanitize_device_id(Some(&"a".repeat(65))), "");
    }

    #[test]
    fn identity_hash_is_prefixed_stable_and_issuer_scoped() {
        let a = identity_key_hash("https://idp.example.com", "alice");
        assert!(a.starts_with("jwt:"));
        assert_eq!(a.len(), 4 + 64);
        assert_eq!(a, identity_key_hash("https://idp.example.com", "alice"));
        assert_ne!(a, identity_key_hash("https://other.example.com", "alice"));
        assert_ne!(a, identity_key_hash("https://idp.example.com", "bob"));
        // The NUL separator prevents (iss="a", sub="bc") colliding with (iss="ab", sub="c").
        assert_ne!(identity_key_hash("a", "bc"), identity_key_hash("ab", "c"));
    }
}
