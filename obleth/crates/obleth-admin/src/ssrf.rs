//! SSRF protection for admin-supplied upstream URLs (model `api_base`, MCP
//! `upstream_url`).
//!
//! Registered upstreams are forwarded to by the data plane, so an attacker who
//! can influence these URLs could otherwise reach internal-only services (cloud
//! metadata at `169.254.169.254`, `localhost` databases, private RFC1918
//! ranges). We block those targets by default and resolve hostnames so a public
//! name that maps to a private address is still rejected.
//!
//! Self-hosters frequently *do* need to reach an internal MCP server (another
//! cluster, a VM on the same network). They opt in by listing the exact CIDRs
//! they trust in `OBLETH_ALLOWED_PRIVATE_CIDRS` (comma-separated), e.g.
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
    #[error("host '{host}' resolves to disallowed address {ip}; add its range to OBLETH_ALLOWED_PRIVATE_CIDRS to permit an internal target")]
    Blocked { host: String, ip: IpAddr },
}

/// Allowlist of private/internal CIDRs that are explicitly permitted as upstream
/// targets. Empty by default (only public addresses allowed).
#[derive(Clone, Default)]
pub struct SsrfPolicy {
    allow: Vec<IpNet>,
}

impl SsrfPolicy {
    /// Build the policy from `OBLETH_ALLOWED_PRIVATE_CIDRS`.
    pub fn from_env() -> Self {
        Self::parse(&std::env::var("OBLETH_ALLOWED_PRIVATE_CIDRS").unwrap_or_default())
    }

    /// Parse a comma-separated list of CIDRs. Invalid entries are ignored.
    pub fn parse(raw: &str) -> Self {
        let allow = raw
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse::<IpNet>().ok())
            .collect();
        Self { allow }
    }

    fn ip_allowed(&self, ip: IpAddr) -> bool {
        let ip = unmap(ip);
        if self.allow.iter().any(|net| net.contains(&ip)) {
            return true;
        }
        is_public(ip)
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

        let addrs: Vec<IpAddr> = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|_| SsrfError::Unresolvable(host.clone()))?
            .map(|sa| sa.ip())
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

    #[test]
    fn blocks_metadata_endpoint_by_default() {
        let policy = SsrfPolicy::default();
        let err = policy.validate("http://169.254.169.254/latest/meta-data/");
        assert!(matches!(err, Err(SsrfError::Blocked { .. })));
    }

    #[test]
    fn blocks_loopback_and_private_by_default() {
        let policy = SsrfPolicy::default();
        assert!(matches!(
            policy.validate("http://127.0.0.1:5432"),
            Err(SsrfError::Blocked { .. })
        ));
        assert!(matches!(
            policy.validate("http://10.1.2.3:8080"),
            Err(SsrfError::Blocked { .. })
        ));
        assert!(matches!(
            policy.validate("http://192.168.1.10"),
            Err(SsrfError::Blocked { .. })
        ));
    }

    #[test]
    fn allows_private_range_when_listed() {
        let policy = SsrfPolicy::parse("10.0.0.0/8");
        assert!(policy.validate("http://10.1.2.3:8080/mcp").is_ok());
        // A different private range is still blocked.
        assert!(matches!(
            policy.validate("http://192.168.1.10"),
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
        let policy = SsrfPolicy::default();
        assert!(matches!(
            policy.validate("http://[::ffff:127.0.0.1]:80"),
            Err(SsrfError::Blocked { .. })
        ));
    }
}
