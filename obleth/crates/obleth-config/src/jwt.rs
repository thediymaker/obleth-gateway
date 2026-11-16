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
    /// Settings applied only when JIT creates the tenant.
    #[serde(default)]
    pub tenant_defaults: Option<crate::TenantDefaults>,
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

/// A tenant template references a claim the token does not carry (or carries
/// as a non-string). Rendering fails rather than falling back to a name two
/// users could share: a personal tenant named from a missing claim would
/// merge distinct identities into one tenant.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TenantTemplateError {
    #[error(
        "tenant template references claim {0:?}, which is absent from the token or not a string"
    )]
    MissingClaim(String),
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
        validate_tenant_template(&i.tenant, &i.issuer)?;
        if let Some(p) = i
            .tenant_defaults
            .as_ref()
            .and_then(|d| d.budget_period.as_ref())
        {
            if !matches!(p.as_str(), "monthly" | "term" | "lifetime") {
                return Err(JwtConfigError::Field(format!(
                    "tenant_defaults.budget_period {p:?} must be monthly, term or lifetime for issuer {:?}",
                    i.issuer
                )));
            }
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

/// Placeholder names a tenant template may use: 1..=32 chars from
/// `[A-Za-z0-9._-]` — enough for `sub` and any reasonable claim name,
/// nothing that could collide with template syntax.
fn is_placeholder_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Walk a template's `{placeholder}` occurrences, calling `f` for each name in
/// order. A `{` without a closing `}` is not a placeholder: `f` is not called
/// and the scan stops there.
fn for_each_placeholder(template: &str, mut f: impl FnMut(&str)) {
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        let tail = &rest[open + 1..];
        let Some(close) = tail.find('}') else {
            return;
        };
        f(&tail[..close]);
        rest = &tail[close + 1..];
    }
}

/// Boot-time validation of a tenant template: every `{placeholder}` must have
/// a name from [`is_placeholder_name`] and may appear at most once. A
/// malformed placeholder would render into a tenant name at request time;
/// catch it at boot instead.
fn validate_tenant_template(tenant: &str, issuer: &str) -> Result<(), JwtConfigError> {
    let mut seen = HashSet::new();
    let mut rest = tenant;
    while let Some(open) = rest.find('{') {
        let tail = &rest[open + 1..];
        let Some(close) = tail.find('}') else {
            return Err(JwtConfigError::Field(format!(
                "tenant {tenant:?} has an unterminated placeholder for issuer {issuer:?}",
            )));
        };
        let name = &tail[..close];
        if !is_placeholder_name(name) {
            return Err(JwtConfigError::Field(format!(
                "tenant placeholder {{{name}}} in {tenant:?} must be 1..=32 chars from [A-Za-z0-9._-] for issuer {issuer:?}",
            )));
        }
        if !seen.insert(name.to_string()) {
            return Err(JwtConfigError::Field(format!(
                "tenant may contain placeholder {{{name}}} at most once for issuer {issuer:?}",
            )));
        }
        rest = &tail[close + 1..];
    }
    Ok(())
}

/// A template is personal when it names a per-identity tenant, i.e. it
/// carries any `{placeholder}` (`{sub}`, `{email}`, ...).
pub fn is_personal_template(template: &str) -> bool {
    let mut personal = false;
    for_each_placeholder(template, |_| personal = true);
    personal
}

/// Hash fallback for a placeholder value that cannot be used verbatim:
/// `h` + 32 hex chars of its SHA-256.
fn hash_part(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("h{}", &hex::encode(h.finalize())[..32])
}

/// Tenant name for a JIT identity.
///
/// `{sub}` in `template` becomes the subject when it is 1..=64 ASCII
/// alphanumerics or `-`, else `h` + 32 hex chars of its SHA-256.
///
/// Any other `{claim}` placeholder becomes the value of that claim from
/// `claims` (a JSON object, e.g. `{"email": "a.b@example.com"}`) when the
/// value is a string of 1..=64 chars from `[A-Za-z0-9._@+-]` — the set an
/// email address needs and no more — else the same hash fallback. A
/// placeholder whose claim is absent from `claims` (or not a string) is an
/// error: every such token would land in the same tenant, so the call is
/// rejected instead of guessing a shared name.
pub fn render_identity_tenant(
    template: &str,
    subject: &str,
    claims: &serde_json::Value,
) -> Result<String, TenantTemplateError> {
    let subject_part = if (1..=64).contains(&subject.len())
        && subject
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        subject.to_string()
    } else {
        hash_part(subject)
    };
    let mut out = String::with_capacity(template.len() + subject_part.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let tail = &rest[open + 1..];
        let Some(close) = tail.find('}') else {
            // A `{` that is not a placeholder: keep it verbatim.
            out.push_str(tail);
            return Ok(out);
        };
        let name = &tail[..close];
        let value = if name == "sub" {
            subject_part.clone()
        } else {
            match claims.get(name) {
                Some(serde_json::Value::String(s)) if !s.trim().is_empty() => {
                    let s = s.trim();
                    if (1..=64).contains(&s.len())
                        && s.bytes().all(|b| {
                            b.is_ascii_alphanumeric()
                                || matches!(b, b'.' | b'@' | b'_' | b'+' | b'-')
                        })
                    {
                        s.to_string()
                    } else {
                        hash_part(s)
                    }
                }
                _ => return Err(TenantTemplateError::MissingClaim(name.to_string())),
            }
        };
        out.push_str(&value);
        rest = &tail[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
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

    #[test]
    fn renders_identity_tenant() {
        let empty = serde_json::json!({});
        assert_eq!(
            render_identity_tenant("u-{sub}", "3f2a-9c", &empty).unwrap(),
            "u-3f2a-9c"
        );
        assert_eq!(
            render_identity_tenant("open-webui", "x", &empty).unwrap(),
            "open-webui"
        );
        let h = render_identity_tenant("u-{sub}", "a@b", &empty).unwrap();
        assert!(h.starts_with("u-h") && h.len() == 2 + 1 + 32);
        assert_eq!(h, render_identity_tenant("u-{sub}", "a@b", &empty).unwrap());
        assert_ne!(h, render_identity_tenant("u-{sub}", "a_b", &empty).unwrap());
        assert!(render_identity_tenant("u-{sub}", &"x".repeat(65), &empty)
            .unwrap()
            .starts_with("u-h"));
    }

    #[test]
    fn renders_identity_tenant_from_claim() {
        let claims = serde_json::json!({"email": "test.user@example.org"});
        // A clean email renders verbatim, the TDDEV-5289 personal-tenant format.
        assert_eq!(
            render_identity_tenant("{email}", "u-02dd4a0b", &claims).unwrap(),
            "test.user@example.org"
        );
        // Email character set: dots, plus-signs, underscores, dashes.
        let claims = serde_json::json!({"email": "first.last+tag_1-2@sub.example.co"});
        assert_eq!(
            render_identity_tenant("t-{email}", "s", &claims).unwrap(),
            "t-first.last+tag_1-2@sub.example.co"
        );
        // `{sub}` and `{claim}` can combine.
        let claims = serde_json::json!({"email": "test.user@example.org"});
        assert_eq!(
            render_identity_tenant("{sub}/{email}", "abc", &claims).unwrap(),
            "abc/test.user@example.org"
        );
        // Out-of-set characters (space, unicode, path) fall back to the hash,
        // keeping one stable name per distinct value.
        let a =
            render_identity_tenant("{email}", "s", &serde_json::json!({"email": "x y@z"})).unwrap();
        let b =
            render_identity_tenant("{email}", "s", &serde_json::json!({"email": "x y@z"})).unwrap();
        let c =
            render_identity_tenant("{email}", "s", &serde_json::json!({"email": "x.z@z"})).unwrap();
        assert!(a.starts_with('h') && a.len() == 33);
        assert_eq!(a, b);
        assert_ne!(a, c);
        // A value longer than 64 chars is hashed, not truncated into a shared prefix.
        let long = render_identity_tenant(
            "{email}",
            "s",
            &serde_json::json!({"email": "a".repeat(65) + "@x.y"}),
        )
        .unwrap();
        assert!(long.starts_with('h') && long.len() == 33);
        // Whitespace-trimmed emails are usable.
        let claims = serde_json::json!({"email": " test.user@example.org "});
        assert_eq!(
            render_identity_tenant("{email}", "s", &claims).unwrap(),
            "test.user@example.org"
        );
    }

    #[test]
    fn renders_identity_tenant_missing_claim_is_an_error() {
        // Absent claim, non-string claim and empty claim all fail closed:
        // a fallback name would be shared by every such token.
        assert_eq!(
            render_identity_tenant("{email}", "s", &serde_json::json!({})).unwrap_err(),
            TenantTemplateError::MissingClaim("email".into())
        );
        assert_eq!(
            render_identity_tenant("{email}", "s", &serde_json::json!({"email": 42})).unwrap_err(),
            TenantTemplateError::MissingClaim("email".into())
        );
        assert_eq!(
            render_identity_tenant("{email}", "s", &serde_json::json!({"email": "  "}))
                .unwrap_err(),
            TenantTemplateError::MissingClaim("email".into())
        );
        // `{sub}` alone never fails, even with an empty claims object.
        assert!(render_identity_tenant("u-{sub}", "s", &serde_json::Value::Null).is_ok());
    }

    #[test]
    fn personal_template_detection() {
        assert!(is_personal_template("u-{sub}"));
        assert!(is_personal_template("{email}"));
        assert!(is_personal_template("team-{sub}-x-{email}"));
        assert!(!is_personal_template("open-webui"));
        assert!(!is_personal_template("cli-users"));
    }

    #[test]
    fn rejects_malformed_and_duplicate_placeholders() {
        let one = |tenant: &str| {
            parse_jwt_issuers(&format!(
                r#"[{{"issuer":"https://idp.example.com","audience":"a","tenant":{tenant}}}]"#
            ))
        };
        // Duplicate placeholder (covers the old `u-{sub}-{sub}` rejection).
        assert!(matches!(
            one(r#""u-{sub}-{sub}""#).unwrap_err(),
            JwtConfigError::Field(_)
        ));
        assert!(matches!(
            one(r#""{email}-{email}""#).unwrap_err(),
            JwtConfigError::Field(_)
        ));
        // Empty, too-long and bad-character placeholder names.
        assert!(matches!(
            one(r#""u-{}""#).unwrap_err(),
            JwtConfigError::Field(_)
        ));
        assert!(matches!(
            one(&format!(r#""u-{{{}}}""#, "a".repeat(33))).unwrap_err(),
            JwtConfigError::Field(_)
        ));
        assert!(matches!(
            one(r#""u-{bad name}""#).unwrap_err(),
            JwtConfigError::Field(_)
        ));
        // Valid placeholders (including literal text around them) pass.
        assert!(one(r#""u-{sub}""#).is_ok());
        assert!(one(r#""{email}""#).is_ok());
        assert!(one(r#""u-{sub}-{email}""#).is_ok());
    }

    #[test]
    fn tenant_defaults_parse_and_validate() {
        let ok = r#"[{"issuer":"https://idp.example.com","audience":"a","tenant":"u-{sub}","tenant_defaults":{"fairshare_group":"personal","budget_tokens":1000,"budget_period":"monthly","allowed_models":["m"]}}]"#;
        let i = &parse_jwt_issuers(ok).unwrap()[0];
        assert_eq!(
            i.tenant_defaults.as_ref().unwrap().budget_tokens,
            Some(1000)
        );
        assert!(parse_jwt_issuers(
            r#"[{"issuer":"https://idp.example.com","audience":"a","tenant":"u-{sub}-{sub}"}]"#
        )
        .is_err());
        assert!(parse_jwt_issuers(r#"[{"issuer":"https://idp.example.com","audience":"a","tenant":"u","tenant_defaults":{"budget_period":"weekly"}}]"#).is_err());
    }
}
