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
//! (`169.254.0.0/16`, incl. `169.254.169.254`, and `fe80::/10`), the
//! unspecified address, and broadcast/documentation ranges. Hostnames are
//! resolved, so a public name that maps to a blocked address is still rejected.
//!
//! Locked-down deployments that forward to untrusted upstreams can flip on the
//! strict policy with `OBLETH_BLOCK_PRIVATE_NETWORKS=1`, which rejects *all*
//! private/internal targets unless their exact range is listed in
//! `OBLETH_ALLOWED_PRIVATE_CIDRS` (comma-separated), e.g.
//! `OBLETH_ALLOWED_PRIVATE_CIDRS=10.0.0.0/8,192.168.0.0/16`.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, ToSocketAddrs};

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
    #[error("host '{host}' resolves to {ip}, a blocked link-local/cloud-metadata or otherwise unsafe address. If this is a trusted internal upstream, add its range to OBLETH_ALLOWED_PRIVATE_CIDRS")]
    Blocked { host: String, ip: IpAddr },
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
    pub fn validate(&self, raw_url: &str) -> Result<(), SsrfError> {
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

        let addrs: Vec<IpAddr> = (host.as_str(), port)
            .to_socket_addrs()
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
}

/// Collapse IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) to their IPv4 form so
/// the v4 classification rules apply and can't be bypassed.
fn unmap(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
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

    #[test]
    fn blocks_metadata_endpoint_by_default() {
        // Cloud metadata (link-local) is dangerous and stays blocked even in the
        // default permissive policy.
        let policy = SsrfPolicy::default();
        let err = policy.validate("http://169.254.169.254/latest/meta-data/");
        assert!(matches!(err, Err(SsrfError::Blocked { .. })));
    }

    #[test]
    fn allows_loopback_and_private_by_default() {
        // Local-first default: the addresses an operator legitimately reaches.
        let policy = SsrfPolicy::default();
        assert!(policy.validate("http://127.0.0.1:5432").is_ok());
        assert!(policy.validate("http://10.1.2.3:8080").is_ok());
        assert!(policy.validate("http://192.168.1.10").is_ok());
        assert!(policy.validate("http://172.16.5.5:11434/v1").is_ok());
    }

    #[test]
    fn strict_mode_blocks_private_unless_listed() {
        let policy = strict();
        assert!(matches!(
            policy.validate("http://127.0.0.1:5432"),
            Err(SsrfError::Blocked { .. })
        ));
        assert!(matches!(
            policy.validate("http://192.168.1.10"),
            Err(SsrfError::Blocked { .. })
        ));

        let listed = SsrfPolicy {
            allow: parse_cidrs("10.0.0.0/8"),
            allow_private: false,
        };
        assert!(listed.validate("http://10.1.2.3:8080/mcp").is_ok());
        // A range outside the explicit list is still blocked in strict mode.
        assert!(matches!(
            listed.validate("http://192.168.1.10"),
            Err(SsrfError::Blocked { .. })
        ));
    }

    #[test]
    fn allows_public_address() {
        let policy = SsrfPolicy::default();
        assert!(policy.validate("https://1.1.1.1").is_ok());
    }

    #[test]
    fn rejects_non_http_scheme() {
        let policy = SsrfPolicy::default();
        assert!(matches!(
            policy.validate("file:///etc/passwd"),
            Err(SsrfError::BadScheme)
        ));
    }

    #[test]
    fn ipv4_mapped_ipv6_cannot_bypass() {
        // In strict mode a mapped loopback must classify as loopback and block.
        let policy = strict();
        assert!(matches!(
            policy.validate("http://[::ffff:127.0.0.1]:80"),
            Err(SsrfError::Blocked { .. })
        ));
    }

    #[test]
    fn ipv6_loopback_is_blocked_in_strict_mode() {
        let policy = strict();
        assert!(matches!(
            policy.validate("http://[::1]:80"),
            Err(SsrfError::Blocked { .. })
        ));
    }

    #[test]
    fn ipv4_mapped_private_cannot_bypass() {
        let policy = strict();
        assert!(matches!(
            policy.validate("http://[::ffff:10.1.2.3]:80"),
            Err(SsrfError::Blocked { .. })
        ));
    }

    #[test]
    fn public_ipv6_literal_is_allowed() {
        let policy = SsrfPolicy::default();
        assert!(policy.validate("http://[2606:4700:4700::1111]:80").is_ok());
    }
}
