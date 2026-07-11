//! Node hostname → address resolution for the provisioner.
//!
//! Slurm allocations name compute nodes by short hostname (`scgh001`). When the
//! pods running obleth resolve those names unreliably — a common cluster setup
//! has the gateway/provisioner pods pointed at a corporate resolver behind a
//! long DNS search list, so a fraction of short-name lookups time out — that
//! flakiness lands on *every* health probe and, worse, on *every* proxied
//! request (the data plane resolves the endpoint host per call). One missed
//! lookup then reads as an instant `502 upstream request failed`.
//!
//! This resolver takes DNS off the hot path. It prefers operator-supplied
//! hostname→IP overrides (Slurm settings, editable in the dashboard), then an IP
//! literal as-is, then a cached prior success, and only then a retried system
//! lookup. The provisioner registers replica endpoints by the resolved **IP**,
//! so once a node is resolved (or aliased) neither the probe nor the proxy
//! touches DNS again for it.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// System-DNS attempts per `resolve` call before giving up. Turns a ~1-in-3
/// single-shot miss into a rare compound miss without stalling the tick.
const RESOLVE_ATTEMPTS: u32 = 3;
/// Pause between DNS attempts. Short on purpose — the whole point is to absorb a
/// transient miss, not to wait out a genuine outage.
const RESOLVE_BACKOFF: Duration = Duration::from_millis(150);

struct CacheEntry {
    ip: String,
    at: Instant,
}

/// Resolves node hostnames to addresses with operator overrides + a success
/// cache. Interior mutability so a single instance can be shared (`&self`)
/// across the per-tick probe loop and the executor's promotion path.
pub struct HostResolver {
    aliases: Mutex<HashMap<String, String>>,
    cache: Mutex<HashMap<String, CacheEntry>>,
    ttl: Duration,
}

impl HostResolver {
    pub fn new(ttl: Duration) -> Self {
        Self {
            aliases: Mutex::new(HashMap::new()),
            cache: Mutex::new(HashMap::new()),
            ttl,
        }
    }

    /// Replace the operator override map. Called once per tick from the current
    /// Slurm settings, so edits in the dashboard take effect without a restart.
    pub fn set_aliases(&self, aliases: HashMap<String, String>) {
        if let Ok(mut a) = self.aliases.lock() {
            *a = aliases;
        }
    }

    /// Resolve `host` to an address string. Order: operator alias → IP-literal
    /// passthrough → fresh cache → retried system DNS → stale cache. `None` only
    /// when the host is unknown, every lookup failed, and nothing was cached.
    pub async fn resolve(&self, host: &str) -> Option<String> {
        let host = host.trim();
        if host.is_empty() {
            return None;
        }
        // 1. Operator override wins outright — no DNS at all.
        if let Some(ip) = self.aliases.lock().ok().and_then(|a| a.get(host).cloned()) {
            return Some(ip);
        }
        // 2. Already an address literal: nothing to resolve.
        if host.parse::<IpAddr>().is_ok() {
            return Some(host.to_string());
        }
        // 3. Fresh cache hit.
        if let Some(ip) = self.cache.lock().ok().and_then(|c| {
            c.get(host)
                .filter(|e| e.at.elapsed() < self.ttl)
                .map(|e| e.ip.clone())
        }) {
            return Some(ip);
        }
        // 4. System DNS, a few tries — never holding a lock across the await.
        for attempt in 0..RESOLVE_ATTEMPTS {
            if let Some(ip) = lookup_first(host).await {
                if let Ok(mut c) = self.cache.lock() {
                    c.insert(
                        host.to_string(),
                        CacheEntry {
                            ip: ip.clone(),
                            at: Instant::now(),
                        },
                    );
                }
                return Some(ip);
            }
            if attempt + 1 < RESOLVE_ATTEMPTS {
                tokio::time::sleep(RESOLVE_BACKOFF).await;
            }
        }
        // 5. Stale cache: a possibly-old IP beats failing an otherwise-fine node.
        self.cache
            .lock()
            .ok()
            .and_then(|c| c.get(host).map(|e| e.ip.clone()))
    }

    /// Rewrite a URL's host to the resolved address, preserving scheme, port and
    /// path. Returns the input unchanged when the host is already an IP literal,
    /// resolution fails, or the URL can't be parsed — callers then proceed by
    /// name, exactly as before this resolver existed.
    pub async fn resolve_url_host(&self, url: &str) -> String {
        let Ok(mut parsed) = reqwest::Url::parse(url) else {
            return url.to_string();
        };
        let Some(host) = parsed.host_str().map(str::to_string) else {
            return url.to_string();
        };
        if host.parse::<IpAddr>().is_ok() {
            return url.to_string();
        }
        let Some(ip) = self.resolve(&host).await else {
            return url.to_string();
        };
        match ip.parse::<IpAddr>() {
            Ok(addr) if parsed.set_ip_host(addr).is_ok() => parsed.to_string(),
            _ => url.to_string(),
        }
    }
}

/// First address for `host` from the system resolver, preferring IPv4. Port 0 is
/// a placeholder — only the address is used.
async fn lookup_first(host: &str) -> Option<String> {
    let addrs: Vec<std::net::SocketAddr> = tokio::net::lookup_host((host, 0)).await.ok()?.collect();
    addrs
        .iter()
        .find(|a| a.is_ipv4())
        .or_else(|| addrs.first())
        .map(|a| a.ip().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolver() -> HostResolver {
        HostResolver::new(Duration::from_secs(300))
    }

    #[tokio::test]
    async fn alias_overrides_dns_entirely() {
        let r = resolver();
        r.set_aliases(HashMap::from([(
            "scgh001".to_string(),
            "10.0.0.7".to_string(),
        )]));
        assert_eq!(r.resolve("scgh001").await.as_deref(), Some("10.0.0.7"));
    }

    #[tokio::test]
    async fn ip_literal_passes_through() {
        let r = resolver();
        assert_eq!(r.resolve("10.1.2.3").await.as_deref(), Some("10.1.2.3"));
    }

    #[tokio::test]
    async fn resolve_url_host_swaps_hostname_for_alias_ip_keeping_port_and_path() {
        let r = resolver();
        r.set_aliases(HashMap::from([(
            "scgh002".to_string(),
            "10.0.0.9".to_string(),
        )]));
        assert_eq!(
            r.resolve_url_host("http://scgh002:8016/v1").await,
            "http://10.0.0.9:8016/v1"
        );
    }

    #[tokio::test]
    async fn resolve_url_host_leaves_ip_urls_untouched() {
        let r = resolver();
        // Already an IP → no DNS, returned verbatim (no trailing-slash churn).
        assert_eq!(
            r.resolve_url_host("http://10.0.0.9:8016/v1").await,
            "http://10.0.0.9:8016/v1"
        );
    }

    #[tokio::test]
    async fn empty_host_resolves_to_none() {
        let r = resolver();
        assert_eq!(r.resolve("   ").await, None);
    }
}
