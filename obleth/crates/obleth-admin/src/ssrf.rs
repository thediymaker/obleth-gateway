//! SSRF protection for admin-supplied upstream URLs (model `api_base`, MCP
//! `upstream_url`).
//!
//! obleth is built for self-hosted/local deployments where the upstreams you
//! register almost always live on the same private network (another node in the
//! cluster, a VM on the LAN, a model server on `localhost`). So by **default we
//! permit private/RFC1918, loopback, CGNAT and IPv6 unique-local targets** —
//! the addresses a local operator legitimately needs to reach.
//!
//! What we still block by default is the genuinely dangerous class with no
//! legitimate "local upstream" use: **link-local / cloud-metadata**
//! (`169.254.0.0/16`, incl. `169.254.169.254`, `fe80::/10`, AWS's
//! IPv6 IMDS endpoint `fd00:ec2::254`, and Alibaba Cloud's `100.100.100.200`,
//! which sits inside the otherwise-permitted CGNAT range), the
//! unspecified address, and broadcast/documentation ranges. Hostnames are
//! resolved, so a public name that maps to a blocked address is still rejected.
//!
//! Locked-down deployments that forward to untrusted upstreams can flip on the
//! strict policy with `OBLETH_BLOCK_PRIVATE_NETWORKS=1`, which rejects *all*
//! private/internal targets unless their exact range is listed in
//! `OBLETH_ALLOWED_PRIVATE_CIDRS` (comma-separated), e.g.
//! `OBLETH_ALLOWED_PRIVATE_CIDRS=10.0.0.0/8,192.168.0.0/16`.
//! Link-local/cloud-metadata addresses remain blocked even when allowlisted.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use ipnet::IpNet;

#[derive(Debug, thiserror::Error)]
pub enum SsrfError {
    #[error("invalid url: {0}")]
    InvalidUrl(String),
    #[error("url scheme must be http or https")]
    BadScheme,
    #[error("url has no host")]
    NoHost,
    #[error("could not resolve host '{0}'")]
    Unresolvable(String),
    #[error("host '{host}' resolves to blocked address {ip}. Private upstreams may be allowed with OBLETH_ALLOWED_PRIVATE_CIDRS; link-local/cloud-metadata addresses cannot be allowed")]
    Blocked { host: String, ip: IpAddr },
    #[error("url template placeholders are not allowed in the scheme or credentials")]
    TemplatedSchemeOrUserinfo,
}

/// Policy for which upstream targets are reachable.
///
/// By default, private/loopback/CGNAT/unique-local addresses are permitted
/// (obleth is a local-first tool). `allow` lists extra CIDRs to permit on top of
/// that, and is the *only* thing that opens internal ranges when `allow_private`
/// is turned off via `OBLETH_BLOCK_PRIVATE_NETWORKS`.
#[derive(Clone)]
pub struct SsrfPolicy {
    allow: Vec<IpNet>,
    allow_private: bool,
}

impl Default for SsrfPolicy {
    fn default() -> Self {
        Self {
            allow: Vec::new(),
            allow_private: true,
        }
    }
}

impl SsrfPolicy {
    /// Build the policy from the environment.
    ///
    /// `OBLETH_BLOCK_PRIVATE_NETWORKS` (truthy) switches to strict mode where
    /// private/internal targets are rejected unless explicitly listed in
    /// `OBLETH_ALLOWED_PRIVATE_CIDRS`.
    pub fn from_env() -> Self {
        Self {
            allow: parse_cidrs(&std::env::var("OBLETH_ALLOWED_PRIVATE_CIDRS").unwrap_or_default()),
            allow_private: !env_flag("OBLETH_BLOCK_PRIVATE_NETWORKS"),
        }
    }

    /// Parse a comma-separated list of CIDRs (extra allowed ranges). Invalid
    /// entries are ignored. Private targets remain allowed by default.
    pub fn parse(raw: &str) -> Self {
        Self {
            allow: parse_cidrs(raw),
            allow_private: true,
        }
    }

    fn ip_allowed(&self, ip: IpAddr) -> bool {
        let ip = unmap(ip);
        // An overly broad allowlist must never reopen metadata endpoints.
        let metadata_or_link_local = match ip {
            IpAddr::V4(v4) => v4.is_link_local() || v4 == ALIBABA_METADATA_V4,
            IpAddr::V6(v6) => {
                (v6.segments()[0] & 0xffc0) == 0xfe80
                    || v6 == Ipv6Addr::new(0xfd00, 0xec2, 0, 0, 0, 0, 0, 0x254)
            }
        };
        if metadata_or_link_local {
            return false;
        }
        if self.allow.iter().any(|net| net.contains(&ip)) {
            return true;
        }
        if is_public(ip) {
            return true;
        }
        // Local-first default: permit the private/internal ranges an operator
        // legitimately reaches, but never the link-local/metadata class.
        self.allow_private && is_safe_private(ip)
    }

    /// Validate a user-supplied upstream URL: must be http/https, must have a
    /// host, and every resolved address must be public or explicitly allowed.
    ///
    /// Async because hostname resolution goes through `tokio::net::lookup_host`
    /// instead of blocking a runtime worker on the system resolver.
    pub async fn validate(&self, raw_url: &str) -> Result<(), SsrfError> {
        let url = reqwest::Url::parse(raw_url).map_err(|e| SsrfError::InvalidUrl(e.to_string()))?;
        match url.scheme() {
            "http" | "https" => {}
            _ => return Err(SsrfError::BadScheme),
        }
        let host = url.host_str().ok_or(SsrfError::NoHost)?.to_string();
        let port = url.port_or_known_default().unwrap_or(443);

        // If the host is already a literal IP, classify it directly. This avoids
        // relying on the platform resolver to normalize forms like the
        // IPv4-mapped IPv6 address `::ffff:127.0.0.1`, whose representation after
        // `to_socket_addrs()` differs across operating systems. `host_str()`
        // keeps the surrounding brackets on IPv6 literals, so strip them before
        // attempting to parse.
        let host_for_ip_parse = host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(&host);
        if let Ok(ip) = host_for_ip_parse.parse::<IpAddr>() {
            let ip = unmap(ip);
            if !self.ip_allowed(ip) {
                return Err(SsrfError::Blocked { host, ip });
            }
            return Ok(());
        }

        let addrs: Vec<IpAddr> = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|_| SsrfError::Unresolvable(host.clone()))?
            .map(|sa| unmap(sa.ip()))
            .collect();
        if addrs.is_empty() {
            return Err(SsrfError::Unresolvable(host));
        }
        for ip in addrs {
            if !self.ip_allowed(ip) {
                return Err(SsrfError::Blocked { host, ip });
            }
        }
        Ok(())
    }

    /// Validate a URL template whose `placeholders` (e.g. `{model}`) are filled
    /// per model at dispatch time (e.g. a per-model Kubernetes Service host).
    /// Placeholders are replaced with a dummy DNS label and the result is
    /// validated normally, except that a templated host cannot be resolved
    /// before its models exist, so only an unresolvable dummy host is
    /// tolerated. Placeholders in the scheme or credentials are rejected (see
    /// [`check_template_structure`]); the proxy re-checks each filled host.
    pub async fn validate_template(
        &self,
        template: &str,
        placeholders: &[&str],
    ) -> Result<(), SsrfError> {
        let url = check_template_structure(template, placeholders)?;
        match self.validate(url.as_str()).await {
            Err(SsrfError::Unresolvable(host)) if host.contains(TEMPLATE_DUMMY) => Ok(()),
            other => other,
        }
    }
}

/// Placeholders the speculation verifier fills per model in
/// `speculation.verify_url_template`.
pub const VERIFY_TEMPLATE_PLACEHOLDERS: &[&str] = &["{upstream}", "{model}"];

/// Stand-in substituted for placeholders when inspecting a template.
const TEMPLATE_DUMMY: &str = "obleth-template-placeholder";

/// Parse a URL template with its placeholders substituted and reject it if any
/// placeholder lands in the scheme or the userinfo (credentials) — positions
/// with no legitimate per-model use, where a value could redirect the request.
/// Host and path/query placeholders are allowed; the fill side constrains the
/// values it puts in each position.
pub fn check_template_structure(
    template: &str,
    placeholders: &[&str],
) -> Result<reqwest::Url, SsrfError> {
    let substituted = placeholders.iter().fold(template.to_string(), |acc, p| {
        acc.replace(p, TEMPLATE_DUMMY)
    });
    let url =
        reqwest::Url::parse(&substituted).map_err(|e| SsrfError::InvalidUrl(e.to_string()))?;
    let misplaced = [
        url.scheme(),
        url.username(),
        url.password().unwrap_or_default(),
    ]
    .iter()
    .any(|part| part.contains(TEMPLATE_DUMMY));
    if misplaced {
        return Err(SsrfError::TemplatedSchemeOrUserinfo);
    }
    Ok(url)
}

/// Registered upstreams must not redirect requests (or their bodies) to an
/// unvalidated destination. Configure the final endpoint URL instead.
pub fn upstream_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
}

/// Alibaba Cloud's instance-metadata endpoint. It lives inside CGNAT
/// (`100.64.0.0/10`), which the local-first default permits, so it is listed
/// explicitly with the always-blocked metadata class.
const ALIBABA_METADATA_V4: Ipv4Addr = Ipv4Addr::new(100, 100, 100, 200);

/// Collapse IPv6 forms that embed an IPv4 address to that IPv4 address so the
/// v4 classification rules apply and can't be bypassed: IPv4-mapped
/// (`::ffff:a.b.c.d`), IPv4-compatible (`::a.b.c.d`) and the NAT64 well-known
/// prefix (`64:ff9b::a.b.c.d`), all of which a dual-stack host or NAT64
/// gateway routes to the embedded v4 target.
fn unmap(ip: IpAddr) -> IpAddr {
    let IpAddr::V6(v6) = ip else {
        return ip;
    };
    if let Some(v4) = v6.to_ipv4_mapped() {
        return IpAddr::V4(v4);
    }
    let seg = v6.segments();
    let [a, b] = seg[6].to_be_bytes();
    let [c, d] = seg[7].to_be_bytes();
    let embedded = Ipv4Addr::new(a, b, c, d);
    let is_compat = seg[..6].iter().all(|s| *s == 0);
    let is_nat64 = seg[0] == 0x64 && seg[1] == 0xff9b && seg[2..6].iter().all(|s| *s == 0);
    // `::` and `::1` are the v6 unspecified/loopback addresses, not
    // v4-compatible forms; keep them v6 so they classify as themselves.
    if is_nat64 || (is_compat && u32::from(embedded) > 1) {
        return IpAddr::V4(embedded);
    }
    ip
}

/// Parse a comma-separated list of CIDRs, ignoring blank/invalid entries.
fn parse_cidrs(raw: &str) -> Vec<IpNet> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<IpNet>().ok())
        .collect()
}

/// Read a boolean-ish environment flag (`1`/`true`/`yes`/`on` => true).
fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Private/internal ranges that are safe to permit for a local-first deployment:
/// RFC1918, loopback, CGNAT and IPv6 unique-local. Deliberately excludes the
/// link-local/cloud-metadata range, the unspecified address, and
/// broadcast/documentation ranges, which stay blocked even in the default
/// (permissive) policy.
fn is_safe_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let [a, b, _, _] = v4.octets();
            let is_cgnat = a == 100 && (0x40..0x80).contains(&b);
            v4.is_private() || v4.is_loopback() || is_cgnat
        }
        IpAddr::V6(v6) => {
            let seg0 = v6.segments()[0];
            let is_unique_local = (seg0 & 0xfe00) == 0xfc00; // fc00::/7
            v6.is_loopback() || is_unique_local
        }
    }
}

/// Is this address safe to reach as an arbitrary upstream (i.e. not internal)?
fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, _, _] = ip.octets();
    // Carrier-grade NAT 100.64.0.0/10.
    let is_cgnat = a == 100 && (0x40..0x80).contains(&b);
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local() // covers 169.254.0.0/16 (cloud metadata)
        || ip.is_broadcast()
        || ip.is_documentation()
        || ip.is_unspecified()
        || a == 0 // "this network" 0.0.0.0/8
        || is_cgnat)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    let seg0 = ip.segments()[0];
    let is_unique_local = (seg0 & 0xfe00) == 0xfc00; // fc00::/7
    let is_link_local = (seg0 & 0xffc0) == 0xfe80; // fe80::/10
    !(ip.is_loopback() || ip.is_unspecified() || is_unique_local || is_link_local)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Strict (locked-down) policy: private/internal targets rejected unless
    /// listed. Mirrors `OBLETH_BLOCK_PRIVATE_NETWORKS=1`.
    fn strict() -> SsrfPolicy {
        SsrfPolicy {
            allow: Vec::new(),
            allow_private: false,
        }
    }

    #[tokio::test]
    async fn blocks_metadata_endpoint_by_default() {
        // Cloud metadata (link-local) is dangerous and stays blocked even in the
        // default permissive policy.
        let policy = SsrfPolicy::default();
        let err = policy
            .validate("http://169.254.169.254/latest/meta-data/")
            .await;
        assert!(matches!(err, Err(SsrfError::Blocked { .. })));
        assert!(matches!(
            policy
                .validate("http://[fd00:ec2::254]/latest/meta-data/")
                .await,
            Err(SsrfError::Blocked { .. })
        ));
    }

    #[tokio::test]
    async fn allowlists_cannot_reopen_metadata_addresses() {
        for allow_private in [true, false] {
            let policy = SsrfPolicy {
                allow: parse_cidrs("0.0.0.0/0,::/0,169.254.0.0/16,fe80::/10"),
                allow_private,
            };
            for url in [
                "http://169.254.169.254/latest/meta-data/",
                "http://169.254.170.2/",
                "http://[fe80::1]/",
                "http://[fd00:ec2::254]/latest/meta-data/",
                "http://[::ffff:169.254.169.254]/",
            ] {
                assert!(
                    matches!(policy.validate(url).await, Err(SsrfError::Blocked { .. })),
                    "{url}"
                );
            }
            assert!(policy.validate("http://10.1.2.3:8080").await.is_ok());
        }
    }

    #[tokio::test]
    async fn upstream_client_does_not_follow_redirects() {
        use axum::{routing::get, Router};
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        };

        let hits = Arc::new(AtomicUsize::new(0));
        let target_hits = hits.clone();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = Router::new()
            .route(
                "/redirect",
                get(|| async { axum::response::Redirect::temporary("/target") }),
            )
            .route(
                "/target",
                get(move || async move {
                    target_hits.fetch_add(1, Ordering::SeqCst);
                    "unexpected target"
                }),
            );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let response = upstream_client_builder()
            .no_proxy()
            .build()
            .unwrap()
            .get(format!("http://{addr}/redirect"))
            .send()
            .await
            .unwrap();
        server.abort();
        assert_eq!(response.status(), reqwest::StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn allows_loopback_and_private_by_default() {
        // Local-first default: the addresses an operator legitimately reaches.
        let policy = SsrfPolicy::default();
        assert!(policy.validate("http://127.0.0.1:5432").await.is_ok());
        assert!(policy.validate("http://10.1.2.3:8080").await.is_ok());
        assert!(policy.validate("http://192.168.1.10").await.is_ok());
        assert!(policy.validate("http://172.16.5.5:11434/v1").await.is_ok());
    }

    #[tokio::test]
    async fn strict_mode_blocks_private_unless_listed() {
        let policy = strict();
        assert!(matches!(
            policy.validate("http://127.0.0.1:5432").await,
            Err(SsrfError::Blocked { .. })
        ));
        assert!(matches!(
            policy.validate("http://192.168.1.10").await,
            Err(SsrfError::Blocked { .. })
        ));

        let listed = SsrfPolicy {
            allow: parse_cidrs("10.0.0.0/8"),
            allow_private: false,
        };
        assert!(listed.validate("http://10.1.2.3:8080/mcp").await.is_ok());
        // A range outside the explicit list is still blocked in strict mode.
        assert!(matches!(
            listed.validate("http://192.168.1.10").await,
            Err(SsrfError::Blocked { .. })
        ));
    }

    #[tokio::test]
    async fn allows_public_address() {
        let policy = SsrfPolicy::default();
        assert!(policy.validate("https://1.1.1.1").await.is_ok());
    }

    #[tokio::test]
    async fn rejects_non_http_scheme() {
        let policy = SsrfPolicy::default();
        assert!(matches!(
            policy.validate("file:///etc/passwd").await,
            Err(SsrfError::BadScheme)
        ));
    }

    #[tokio::test]
    async fn ipv4_mapped_ipv6_cannot_bypass() {
        // In strict mode a mapped loopback must classify as loopback and block.
        let policy = strict();
        assert!(matches!(
            policy.validate("http://[::ffff:127.0.0.1]:80").await,
            Err(SsrfError::Blocked { .. })
        ));
    }

    #[tokio::test]
    async fn ipv6_loopback_is_blocked_in_strict_mode() {
        let policy = strict();
        assert!(matches!(
            policy.validate("http://[::1]:80").await,
            Err(SsrfError::Blocked { .. })
        ));
    }

    #[tokio::test]
    async fn ipv4_mapped_private_cannot_bypass() {
        let policy = strict();
        assert!(matches!(
            policy.validate("http://[::ffff:10.1.2.3]:80").await,
            Err(SsrfError::Blocked { .. })
        ));
    }

    #[tokio::test]
    async fn public_ipv6_literal_is_allowed() {
        let policy = SsrfPolicy::default();
        assert!(policy
            .validate("http://[2606:4700:4700::1111]:80")
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn embedded_ipv4_metadata_forms_are_blocked() {
        // IPv4-compatible and NAT64 spellings of 169.254.169.254, plus
        // Alibaba's CGNAT-range metadata address, in both policies.
        for policy in [SsrfPolicy::default(), strict()] {
            for url in [
                "http://[::a9fe:a9fe]/",
                "http://[64:ff9b::a9fe:a9fe]/",
                "http://100.100.100.200/latest/meta-data/",
            ] {
                assert!(
                    matches!(policy.validate(url).await, Err(SsrfError::Blocked { .. })),
                    "{url}"
                );
            }
        }
        // The rest of CGNAT stays reachable under the local-first default.
        assert!(SsrfPolicy::default()
            .validate("http://100.100.100.201/")
            .await
            .is_ok());
    }

    #[test]
    fn unmap_keeps_v6_loopback_and_unspecified() {
        assert_eq!(
            unmap("::1".parse().unwrap()),
            "::1".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            unmap("::".parse().unwrap()),
            "::".parse::<IpAddr>().unwrap()
        );
        assert_eq!(
            unmap("64:ff9b::a00:1".parse().unwrap()),
            "10.0.0.1".parse::<IpAddr>().unwrap()
        );
    }

    #[tokio::test]
    async fn templates_allow_host_and_path_placeholders_but_not_scheme_or_userinfo() {
        let policy = SsrfPolicy::default();
        let ph = VERIFY_TEMPLATE_PLACEHOLDERS;
        for template in [
            "http://10.0.0.5:8000/{upstream}/v1?m={model}",
            // Per-model Kubernetes Service: the host can't resolve until the
            // model exists, so the dummy host's resolution failure is tolerated.
            "http://{upstream}.serving.svc.cluster.local:8000/v1",
            "http://{upstream}/v1",
        ] {
            assert!(
                policy.validate_template(template, ph).await.is_ok(),
                "{template}"
            );
        }
        for template in [
            "http://{model}@10.0.0.5/v1",
            "http://user:{model}@10.0.0.5/v1",
            "{upstream}://10.0.0.5/v1",
        ] {
            assert!(
                matches!(
                    policy.validate_template(template, ph).await,
                    Err(SsrfError::TemplatedSchemeOrUserinfo)
                ),
                "{template}"
            );
        }
        // A templated port can't even parse, so it is rejected too.
        assert!(policy
            .validate_template("http://10.0.0.5:{model}/v1", ph)
            .await
            .is_err());
        assert!(matches!(
            policy
                .validate_template("http://169.254.169.254/{model}/v1", ph)
                .await,
            Err(SsrfError::Blocked { .. })
        ));
        assert!(matches!(
            policy.validate_template("file:///{model}", ph).await,
            Err(SsrfError::BadScheme)
        ));
    }
}
