//! Client network helpers: which address a request came from, which country
//! the edge says it came from, and whether a key's allowed networks admit it.
//!
//! Everything here is pure so it can be tested without a listener. The proxy
//! reads the trusted-proxy list and the country header name from the
//! environment once and applies these per request.

use std::net::IpAddr;

use ipnet::IpNet;

/// Most networks one key may list in `allowed_networks`.
pub const MAX_ALLOWED_NETWORKS: usize = 64;

/// Longest User-Agent kept on a usage or rejection row, in characters.
pub const USER_AGENT_MAX_CHARS: usize = 256;

/// Proxies trusted to set `X-Forwarded-For` when `OBLETH_TRUSTED_PROXIES` is
/// unset: the private and loopback ranges a gateway's own reverse proxies
/// sit in.
pub const DEFAULT_TRUSTED_PROXIES: &str =
    "10.0.0.0/8,172.16.0.0/12,192.168.0.0/16,127.0.0.0/8,::1/128,fc00::/7";

/// Header the edge names the client's country in, unless
/// `OBLETH_CLIENT_COUNTRY_HEADER` says otherwise.
pub const DEFAULT_COUNTRY_HEADER: &str = "cf-ipcountry";

/// An IPv4 address carried in IPv6 (`::ffff:192.0.2.1`) is that IPv4 address:
/// a dual-stack listener reports IPv4 peers this way, and a CIDR list written
/// in IPv4 must still match them.
pub fn canonical_ip(ip: IpAddr) -> IpAddr {
    ip.to_canonical()
}

/// Parse one CIDR or bare address. A bare address is the single-host network
/// (/32 or /128). Host bits are cleared, so `10.1.2.3/8` reads as
/// `10.0.0.0/8`.
pub fn parse_network(raw: &str) -> Option<IpNet> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let net = match raw.parse::<IpNet>() {
        Ok(net) => net,
        Err(_) => IpNet::from(raw.parse::<IpAddr>().ok()?),
    };
    // An IPv4-mapped IPv6 range is written as IPv4, so it matches the
    // canonical client addresses `ip_allowed` compares against.
    let net = match net {
        IpNet::V6(v6) => match v6.addr().to_ipv4_mapped() {
            Some(v4) if v6.prefix_len() >= 96 => {
                IpNet::new(IpAddr::V4(v4), v6.prefix_len() - 96).ok()?
            }
            _ => net,
        },
        IpNet::V4(_) => net,
    };
    Some(net.trunc())
}

/// Parse a comma-separated CIDR list (the `OBLETH_TRUSTED_PROXIES` shape).
/// Returns the networks and the entries that did not parse, so the caller
/// can warn about them.
pub fn parse_network_list(raw: &str) -> (Vec<IpNet>, Vec<String>) {
    let mut nets = Vec::new();
    let mut invalid = Vec::new();
    for entry in raw.split(',').map(str::trim).filter(|e| !e.is_empty()) {
        match parse_network(entry) {
            Some(net) => nets.push(net),
            None => invalid.push(entry.to_string()),
        }
    }
    (nets, invalid)
}

/// Validate and normalise a key's `allowed_networks` as an operator sent it:
/// blank entries are dropped, each entry becomes its canonical CIDR
/// (`203.0.113.7` becomes `203.0.113.7/32`, `10.1.2.3/8` becomes
/// `10.0.0.0/8`), and repeats are removed, keeping the first. The error
/// names the entry that is wrong in words an operator can act on.
pub fn normalize_allowed_networks(entries: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for entry in entries.iter().map(|e| e.trim()).filter(|e| !e.is_empty()) {
        let Some(net) = parse_network(entry) else {
            return Err(format!(
                "allowed_networks: \"{entry}\" is not an IP address or CIDR range \
                 (examples: 203.0.113.7, 203.0.113.0/24, 2001:db8::/48)"
            ));
        };
        let canonical = net.to_string();
        if !out.contains(&canonical) {
            out.push(canonical);
        }
    }
    if out.len() > MAX_ALLOWED_NETWORKS {
        return Err(format!(
            "allowed_networks: at most {MAX_ALLOWED_NETWORKS} networks per key ({} given)",
            out.len()
        ));
    }
    Ok(out)
}

/// Whether a key that lists `networks` may be used from `ip`. An empty list
/// allows any network. An unknown address is never inside a list: a key
/// limited to certain networks fails closed when the gateway cannot tell
/// where a request came from. Entries that do not parse (none should, as the
/// admin API normalises them) match nothing.
pub fn ip_allowed(networks: &[String], ip: Option<IpAddr>) -> bool {
    if networks.is_empty() {
        return true;
    }
    let Some(ip) = ip.map(canonical_ip) else {
        return false;
    };
    networks
        .iter()
        .filter_map(|n| parse_network(n))
        .any(|net| net.contains(&ip))
}

/// The network a client address is grouped under when watching where a key
/// is used: the IPv4 /24 or the IPv6 /48 around it, as CIDR text.
pub fn client_network(ip: IpAddr) -> String {
    let ip = canonical_ip(ip);
    let prefix = if ip.is_ipv4() { 24 } else { 48 };
    IpNet::new(ip, prefix)
        .map(|n| n.trunc().to_string())
        .unwrap_or_default()
}

/// Parse one `X-Forwarded-For` entry: a bare address, or one with a port
/// (`192.0.2.1:443`, `[2001:db8::1]:443`), as some proxies write it.
fn parse_forwarded_entry(entry: &str) -> Option<IpAddr> {
    let entry = entry.trim().trim_matches('"');
    if let Ok(ip) = entry.parse::<IpAddr>() {
        return Some(ip);
    }
    if let Ok(sock) = entry.parse::<std::net::SocketAddr>() {
        return Some(sock.ip());
    }
    // `[2001:db8::1]` without a port.
    entry
        .strip_prefix('[')
        .and_then(|e| e.strip_suffix(']'))
        .and_then(|e| e.parse::<IpAddr>().ok())
}

/// The address the nearest proxy put in `X-Forwarded-For`: the rightmost
/// entry across every header line, in order. A proxy appends the address it
/// saw, so that entry is the one it vouches for; anything to its left came
/// from the client and is not trusted. When that rightmost entry is not an
/// address, nothing further left is used either: `None` lets the caller fall
/// back to the peer.
pub fn rightmost_forwarded<'a>(values: impl IntoIterator<Item = &'a str>) -> Option<IpAddr> {
    let last = values
        .into_iter()
        .flat_map(|v| v.split(','))
        .map(str::trim)
        .filter(|e| !e.is_empty())
        .last()?;
    parse_forwarded_entry(last).map(canonical_ip)
}

/// The client address for a request: the rightmost `X-Forwarded-For` entry
/// when the peer is a trusted proxy and that entry is an address, else the
/// peer itself. `None` only when the peer is unknown (a request built in
/// process).
pub fn client_ip<'a>(
    peer: Option<IpAddr>,
    trusted: &[IpNet],
    forwarded_for: impl IntoIterator<Item = &'a str>,
) -> Option<IpAddr> {
    let peer = peer.map(canonical_ip);
    match peer {
        Some(p) if trusted.iter().any(|n| n.contains(&p)) => {
            rightmost_forwarded(forwarded_for).or(Some(p))
        }
        other => other,
    }
}

/// A country code as the edge sent it, cleaned up: two ASCII letters,
/// upper-cased. The placeholders some edges send for "unknown" (`XX`) and
/// for Tor (`T1`), and anything else, give `None`.
pub fn parse_country(raw: &str) -> Option<String> {
    let raw = raw.trim();
    if raw.len() != 2 || !raw.bytes().all(|b| b.is_ascii_alphabetic()) {
        return None;
    }
    let code = raw.to_ascii_uppercase();
    (code != "XX").then_some(code)
}

/// A User-Agent cut to [`USER_AGENT_MAX_CHARS`], with control characters
/// dropped so a row never carries a newline or escape sequence into a log.
pub fn clean_user_agent(raw: &str) -> String {
    raw.trim()
        .chars()
        .filter(|c| !c.is_control())
        .take(USER_AGENT_MAX_CHARS)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn default_trusted() -> Vec<IpNet> {
        let (nets, invalid) = parse_network_list(DEFAULT_TRUSTED_PROXIES);
        assert!(invalid.is_empty());
        nets
    }

    #[test]
    fn networks_normalise_to_canonical_cidrs() {
        let got = normalize_allowed_networks(&[
            " 203.0.113.7 ".into(),
            "10.1.2.3/8".into(),
            "".into(),
            "2001:DB8::1".into(),
            "2001:db8:abcd:12::/48".into(),
            "10.0.0.0/8".into(),
            "::ffff:192.0.2.0/120".into(),
        ])
        .unwrap();
        assert_eq!(
            got,
            vec![
                "203.0.113.7/32",
                "10.0.0.0/8",
                "2001:db8::1/128",
                "2001:db8:abcd::/48",
                "192.0.2.0/24",
            ]
        );
        assert_eq!(
            normalize_allowed_networks(&[]).unwrap(),
            Vec::<String>::new()
        );
    }

    #[test]
    fn bad_networks_are_refused_with_the_entry_named() {
        for bad in [
            "10.0.0.0/33",
            "not-an-ip",
            "10.0.0",
            "2001:db8::/129",
            "1.2.3.4/",
        ] {
            let err = normalize_allowed_networks(&[bad.into()]).unwrap_err();
            assert!(err.contains(bad), "{err}");
            assert!(err.contains("not an IP address or CIDR range"), "{err}");
        }
        let many: Vec<String> = (0..65).map(|i| format!("10.0.{i}.0/24")).collect();
        let err = normalize_allowed_networks(&many).unwrap_err();
        assert!(err.contains("at most 64"), "{err}");
        // Repeats collapse before the count.
        let repeated: Vec<String> = (0..100).map(|_| "10.0.0.1".to_string()).collect();
        assert_eq!(normalize_allowed_networks(&repeated).unwrap().len(), 1);
    }

    #[test]
    fn allowlist_matches_v4_v6_and_mapped_addresses() {
        let nets = normalize_allowed_networks(&[
            "203.0.113.0/24".into(),
            "2001:db8::/32".into(),
            "198.51.100.9".into(),
        ])
        .unwrap();
        assert!(ip_allowed(&nets, Some(ip("203.0.113.200"))));
        assert!(ip_allowed(&nets, Some(ip("::ffff:203.0.113.5"))));
        assert!(ip_allowed(&nets, Some(ip("2001:db8:ffff::1"))));
        assert!(ip_allowed(&nets, Some(ip("198.51.100.9"))));
        assert!(!ip_allowed(&nets, Some(ip("198.51.100.10"))));
        assert!(!ip_allowed(&nets, Some(ip("203.0.114.1"))));
        assert!(!ip_allowed(&nets, Some(ip("2001:db9::1"))));
        // Unknown origin fails closed for a limited key...
        assert!(!ip_allowed(&nets, None));
        // ...and an unlimited key allows anything, unknown included.
        assert!(ip_allowed(&[], None));
        assert!(ip_allowed(&[], Some(ip("192.0.2.1"))));
        // An entry that somehow does not parse matches nothing.
        assert!(!ip_allowed(&["junk".into()], Some(ip("192.0.2.1"))));
    }

    #[test]
    fn rightmost_forwarded_entry_wins_from_a_trusted_proxy() {
        let trusted = default_trusted();
        let peer = Some(ip("10.42.0.7"));
        // A proxy that sets the header to exactly the client.
        assert_eq!(
            client_ip(peer, &trusted, ["203.0.113.9"]),
            Some(ip("203.0.113.9"))
        );
        // A proxy that appends: the client-supplied entries on the left are
        // ignored.
        assert_eq!(
            client_ip(peer, &trusted, ["1.1.1.1, 8.8.8.8 ,203.0.113.9"]),
            Some(ip("203.0.113.9"))
        );
        // Several header lines read as one list, in order.
        assert_eq!(
            client_ip(peer, &trusted, ["1.1.1.1", "198.51.100.4"]),
            Some(ip("198.51.100.4"))
        );
        // Ports, brackets and IPv6.
        assert_eq!(
            client_ip(peer, &trusted, ["203.0.113.9:51234"]),
            Some(ip("203.0.113.9"))
        );
        assert_eq!(
            client_ip(peer, &trusted, ["[2001:db8::5]:443"]),
            Some(ip("2001:db8::5"))
        );
        assert_eq!(
            client_ip(peer, &trusted, ["2001:db8::5"]),
            Some(ip("2001:db8::5"))
        );
        assert_eq!(
            client_ip(peer, &trusted, ["::ffff:203.0.113.9"]),
            Some(ip("203.0.113.9"))
        );
    }

    #[test]
    fn junk_or_missing_forwarded_for_falls_back_to_the_peer() {
        let trusted = default_trusted();
        let peer = Some(ip("10.42.0.7"));
        assert_eq!(client_ip(peer, &trusted, []), peer);
        assert_eq!(client_ip(peer, &trusted, [""]), peer);
        assert_eq!(client_ip(peer, &trusted, [" , "]), peer);
        // A junk rightmost entry does not hand the decision to the
        // client-supplied entries to its left.
        assert_eq!(client_ip(peer, &trusted, ["203.0.113.9, unknown"]), peer);
        assert_eq!(client_ip(peer, &trusted, ["203.0.113.9, 999.1.1.1"]), peer);
    }

    #[test]
    fn forwarded_for_is_ignored_from_an_untrusted_peer() {
        let trusted = default_trusted();
        let peer = Some(ip("198.51.100.77"));
        assert_eq!(client_ip(peer, &trusted, ["203.0.113.9"]), peer);
        // An IPv6 peer outside fc00::/7.
        let peer6 = Some(ip("2001:db8::77"));
        assert_eq!(client_ip(peer6, &trusted, ["203.0.113.9"]), peer6);
        // A mapped private peer is the private IPv4 address, so trusted.
        assert_eq!(
            client_ip(Some(ip("::ffff:10.0.0.1")), &trusted, ["203.0.113.9"]),
            Some(ip("203.0.113.9"))
        );
        // Trusting nobody.
        assert_eq!(
            client_ip(Some(ip("10.0.0.1")), &[], ["203.0.113.9"]),
            Some(ip("10.0.0.1"))
        );
        // No peer at all (an in-process request).
        assert_eq!(client_ip(None, &trusted, ["203.0.113.9"]), None);
    }

    #[test]
    fn trusted_proxy_lists_report_bad_entries() {
        let (nets, invalid) = parse_network_list(" 10.0.0.0/8 , nope,, 192.0.2.1 ");
        assert_eq!(nets.len(), 2);
        assert_eq!(invalid, vec!["nope".to_string()]);
        assert_eq!(default_trusted().len(), 6);
    }

    #[test]
    fn country_codes_are_two_letters_upper_cased() {
        assert_eq!(parse_country("us").as_deref(), Some("US"));
        assert_eq!(parse_country(" De ").as_deref(), Some("DE"));
        assert_eq!(parse_country("XX"), None);
        assert_eq!(parse_country("xx"), None);
        assert_eq!(parse_country("T1"), None);
        assert_eq!(parse_country("USA"), None);
        assert_eq!(parse_country(""), None);
        assert_eq!(parse_country("1A"), None);
        assert_eq!(parse_country("é"), None);
    }

    #[test]
    fn networks_group_v4_by_24_and_v6_by_48() {
        assert_eq!(client_network(ip("203.0.113.77")), "203.0.113.0/24");
        assert_eq!(client_network(ip("::ffff:203.0.113.77")), "203.0.113.0/24");
        assert_eq!(
            client_network(ip("2001:db8:abcd:12:1::9")),
            "2001:db8:abcd::/48"
        );
    }

    #[test]
    fn user_agents_are_cut_and_cleaned() {
        assert_eq!(clean_user_agent("  curl/8.0 "), "curl/8.0");
        assert_eq!(clean_user_agent("a\nb\x1bc"), "abc");
        let long = "x".repeat(1000);
        assert_eq!(
            clean_user_agent(&long).chars().count(),
            USER_AGENT_MAX_CHARS
        );
    }
}
